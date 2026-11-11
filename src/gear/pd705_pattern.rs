//! A kit's own step pattern for the PD705 (docs/HUB.md, M8 "Kit step patterns").
//!
//! The PD705 ignores MIDI clock, so the hub plays its pads itself, on the master clock
//! ([`crate::clock::ClockEngine`], role `Steps`). Until now that was one fixed 16-step house
//! groove on pads 7 / 8 / 9. A [`KitPattern`] belongs to a kit instead: lanes are pad inputs
//! (1-15, [`super::pd705::input_of`]), 1 to [`MAX_BARS`] bars of sixteenths, a velocity in each
//! cell (0 = rest), edited like the TD-3's step grid. It becomes a [`StepPattern`] for the clock
//! with [`KitPattern::to_step_pattern`], leaving out the notes of a kit's loop pads (a note-on
//! there would start another long sample under the drums).
//!
//! Pure: no files. The kit store keeps it as JSON ([`KitPattern::to_json`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::pd705::{input_name, note_of, INPUTS};
use super::PD705_NOTES;
use crate::clock::{Lane, StepPattern};
use crate::json::{JsonObject, JsonValue};
use crate::rng::Mulberry32;

/// Sixteenths in a bar.
pub const BAR: usize = 16;
/// The longest pattern, in bars.
pub const MAX_BARS: u8 = 4;
/// The velocities a click on a cell steps through: off, soft, medium, hard, off.
pub const LEVELS: [u8; 3] = [60, 100, 127];
/// The note-off after this many clock ticks (6 = a sixteenth): the default groove's.
pub const GATE_TICKS: u64 = 3;

/// One pad's row: a velocity per sixteenth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatLane {
    /// The pad input, 1-15 (`Pad1`-`Pad9`, `Trig1`-`Trig4`, `Foot1`-`Foot2`).
    pub input: u8,
    /// `bars * 16` velocities, 0 = rest.
    pub steps: Vec<u8>,
}

/// The pattern a kit plays on the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KitPattern {
    /// 1 to [`MAX_BARS`].
    pub bars: u8,
    /// Sorted by input, no repeats.
    pub lanes: Vec<PatLane>,
    /// How long a hit's note is held, in clock ticks.
    pub gate_ticks: u64,
}

impl Default for KitPattern {
    fn default() -> Self {
        KitPattern::empty()
    }
}

impl KitPattern {
    /// One bar, no lanes.
    pub fn empty() -> KitPattern {
        KitPattern {
            bars: 1,
            lanes: Vec::new(),
            gate_ticks: GATE_TICKS,
        }
    }

    /// Steps in a lane.
    pub fn len(&self) -> usize {
        self.bars as usize * BAR
    }

    pub fn is_empty(&self) -> bool {
        self.hits() == 0
    }

    /// Cells with a hit, over all lanes.
    pub fn hits(&self) -> usize {
        self.lanes
            .iter()
            .map(|l| l.steps.iter().filter(|v| **v > 0).count())
            .sum()
    }

    pub fn lane(&self, input: u8) -> Option<&PatLane> {
        self.lanes.iter().find(|l| l.input == input)
    }

    /// The lane for `input`, added (empty, in order) if there is none. None for an input off
    /// the pad (not 1-15).
    pub fn lane_mut(&mut self, input: u8) -> Option<&mut PatLane> {
        if !(1..=INPUTS).contains(&input) {
            return None;
        }
        let at = match self.lanes.iter().position(|l| l.input >= input) {
            Some(i) if self.lanes[i].input == input => i,
            Some(i) => {
                self.lanes.insert(i, self.blank(input));
                i
            }
            None => {
                self.lanes.push(self.blank(input));
                self.lanes.len() - 1
            }
        };
        Some(&mut self.lanes[at])
    }

    fn blank(&self, input: u8) -> PatLane {
        PatLane {
            input,
            steps: alloc::vec![0; self.len()],
        }
    }

    /// Set one cell. `velocity` 0 clears it, anything else is 1-127. Errors name what is wrong.
    pub fn set_step(&mut self, input: u8, step: usize, velocity: u8) -> Result<(), String> {
        if step >= self.len() {
            return Err(format!(
                "step {step}: the pattern has {} (0-{})",
                self.len(),
                self.len() - 1
            ));
        }
        if velocity > 127 {
            return Err(format!("velocity {velocity}: 0-127"));
        }
        let l = self.lane_mut(input).ok_or_else(|| {
            format!("pad {input}: inputs are 1-9 (pads), 10-13 (trig 1-4), 14-15 (foot 1-2)")
        })?;
        l.steps[step] = velocity;
        Ok(())
    }

    /// A click on a cell: off → soft → medium → hard → off. Returns the new velocity.
    pub fn cycle_step(&mut self, input: u8, step: usize) -> Result<u8, String> {
        let now = self
            .lane(input)
            .and_then(|l| l.steps.get(step).copied())
            .unwrap_or(0);
        // the first level above the velocity there now: a velocity between levels counts as the
        // level above it; past the last one, off
        let next = if now == 0 {
            LEVELS[0]
        } else {
            LEVELS.iter().copied().find(|l| *l > now).unwrap_or(0)
        };
        self.set_step(input, step, next)?;
        Ok(next)
    }

    /// Change the length to `bars` (1 to [`MAX_BARS`]). Growing repeats what is there, so a
    /// 1-bar groove becomes a 2-bar one to vary; shrinking cuts from the end.
    pub fn set_bars(&mut self, bars: u8) -> Result<(), String> {
        if !(1..=MAX_BARS).contains(&bars) {
            return Err(format!("bars {bars}: 1-{MAX_BARS}"));
        }
        let want = bars as usize * BAR;
        for l in &mut self.lanes {
            let have = l.steps.len().max(1);
            if want > l.steps.len() {
                let src = l.steps.clone();
                while l.steps.len() < want {
                    let i = l.steps.len() % have;
                    l.steps.push(src.get(i).copied().unwrap_or(0));
                }
            } else {
                l.steps.truncate(want);
            }
        }
        self.bars = bars;
        Ok(())
    }

    /// Empty every lane, keep the length.
    pub fn clear(&mut self) {
        self.lanes.clear();
    }

    /// Drop a lane. Whether there was one.
    pub fn remove_lane(&mut self, input: u8) -> bool {
        let n = self.lanes.len();
        self.lanes.retain(|l| l.input != input);
        self.lanes.len() != n
    }

    /// Lanes that have no hit left are dropped (a tidy file and a quiet clock).
    pub fn prune(&mut self) {
        self.lanes.retain(|l| l.steps.iter().any(|v| *v > 0));
    }

    /// The clock's pattern: each lane on the factory note of its pad, minus the notes `skip`
    /// holds back (a kit's loop pads).
    pub fn to_step_pattern(&self, skip: impl Fn(u8) -> bool) -> StepPattern {
        let mut lanes = Vec::new();
        for l in &self.lanes {
            let Some(note) = note_of(l.input) else {
                continue;
            };
            if skip(note) {
                continue;
            }
            let name = PD705_NOTES
                .iter()
                .find(|(n, _)| *n == note)
                .map_or("pad", |(_, name)| *name);
            lanes.push(Lane {
                note,
                name,
                steps: l.steps.clone(),
            });
        }
        StepPattern {
            lanes,
            gate_ticks: self.gate_ticks,
        }
    }

    /// `2 bars, 3 lanes, 18 hits` (for a line in a window or a log).
    pub fn line(&self) -> String {
        format!(
            "{} bar{}, {} lane{}, {} hit{}",
            self.bars,
            if self.bars == 1 { "" } else { "s" },
            self.lanes.len(),
            if self.lanes.len() == 1 { "" } else { "s" },
            self.hits(),
            if self.hits() == 1 { "" } else { "s" },
        )
    }

    /// One lane as a row of characters for a terminal: `x` hard, `o` medium, `.` soft, `-` rest.
    pub fn row(&self, input: u8) -> String {
        let Some(l) = self.lane(input) else {
            return "-".repeat(self.len());
        };
        l.steps
            .iter()
            .map(|v| match *v {
                0 => '-',
                1..=79 => '.',
                80..=104 => 'o',
                _ => 'x',
            })
            .collect()
    }

    // MARK: presets

    /// A house groove (kick on the beats, snare on 2 and 4, hats on the off-beats) on the pads
    /// of `pads` that exist: 7 kick, 8 snare, 9 hat. The default groove's velocities.
    pub fn house(pads: &[u8]) -> KitPattern {
        let mut p = KitPattern::empty();
        for s in 0..BAR {
            if s % 4 == 0 {
                put(&mut p, pads, 7, s, 110);
            }
            if s % 8 == 4 {
                put(&mut p, pads, 8, s, 105);
            }
            if s % 2 == 0 {
                put(&mut p, pads, 9, s, if s % 4 == 2 { 100 } else { 60 });
            }
        }
        p
    }

    /// A broken beat: kick on 1 and the "and" of 3, snare on 2 and 4 with a ghost note before
    /// each, hats on the eighths.
    pub fn breaks(pads: &[u8]) -> KitPattern {
        let mut p = KitPattern::empty();
        for s in [0, 10] {
            put(&mut p, pads, 7, s, 112);
        }
        put(&mut p, pads, 7, 7, 70);
        for s in [4, 12] {
            put(&mut p, pads, 8, s, 108);
        }
        for s in [7, 15] {
            put(&mut p, pads, 8, s, 45);
        }
        for s in (0..BAR).step_by(2) {
            put(&mut p, pads, 9, s, if s % 4 == 0 { 90 } else { 65 });
        }
        p
    }

    /// Half time: kick on 1, snare on 3, sixteenth hats at two strengths.
    pub fn halftime(pads: &[u8]) -> KitPattern {
        let mut p = KitPattern::empty();
        put(&mut p, pads, 7, 0, 112);
        put(&mut p, pads, 7, 10, 90);
        put(&mut p, pads, 8, 8, 108);
        for s in 0..BAR {
            put(&mut p, pads, 9, s, if s % 2 == 0 { 70 } else { 40 });
        }
        p
    }

    /// A pattern drawn from `seed` on `pads`: a steady kick, a snare on the backbeat, and
    /// whatever else the kit has thrown in at a density by role. The same seed and pads give
    /// the same pattern.
    pub fn from_seed(seed: f64, pads: &[u8]) -> KitPattern {
        let mut r = Mulberry32::new(seed);
        let mut p = KitPattern::empty();
        // the spine
        for s in (0..BAR).step_by(4) {
            put(&mut p, pads, 7, s, 105 + r.next_int(15) as u8);
        }
        for s in [4, 12] {
            put(&mut p, pads, 8, s, 100 + r.next_int(20) as u8);
        }
        for &pad in pads {
            if !(1..=INPUTS).contains(&pad) || matches!(pad, 7 | 8) {
                continue;
            }
            // chance of a hit on a step: hats busy, cymbals rare, the rest sparse
            let (grid, chance) = match pad {
                9 | 14 => (2, 0.85),
                1..=3 => (4, 0.12),
                4..=6 => (2, 0.15),
                _ => (2, 0.2),
            };
            for s in (0..BAR).step_by(grid) {
                if r.next() < chance {
                    let v = 55 + r.next_int(60) as u8;
                    put(&mut p, pads, pad, s, v);
                }
            }
        }
        p
    }

    /// A short name for a preset, or None.
    pub fn preset(name: &str, pads: &[u8], seed: f64) -> Option<KitPattern> {
        Some(match name {
            "house" => KitPattern::house(pads),
            "breaks" => KitPattern::breaks(pads),
            "halftime" | "half-time" => KitPattern::halftime(pads),
            "random" => KitPattern::from_seed(seed, pads),
            "empty" | "clear" => KitPattern::empty(),
            _ => return None,
        })
    }

    /// The preset names, in the order a window offers them.
    pub const PRESETS: [&'static str; 5] = ["house", "breaks", "halftime", "random", "empty"];

    // MARK: JSON

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("bars", JsonValue::int(self.bars as i64));
        o.set("gate", JsonValue::int(self.gate_ticks as i64));
        o.set(
            "lanes",
            JsonValue::Array(
                self.lanes
                    .iter()
                    .map(|l| {
                        JsonValue::Object(JsonObject::from_pairs([
                            ("pad", JsonValue::int(l.input as i64)),
                            (
                                "steps",
                                JsonValue::Array(
                                    l.steps.iter().map(|v| JsonValue::int(*v as i64)).collect(),
                                ),
                            ),
                        ]))
                    })
                    .collect(),
            ),
        );
        JsonValue::Object(o)
    }

    /// None unless it is a pattern document; lanes off the pad or of the wrong length are an
    /// error rather than silently dropped.
    pub fn from_json(v: &JsonValue) -> Result<KitPattern, String> {
        let bars = v.get("bars").and_then(JsonValue::as_i64).unwrap_or(1);
        if !(1..=MAX_BARS as i64).contains(&bars) {
            return Err(format!("bars {bars}: 1-{MAX_BARS}"));
        }
        let mut p = KitPattern::empty();
        p.bars = bars as u8;
        p.gate_ticks = v
            .get("gate")
            .and_then(JsonValue::as_i64)
            .map_or(GATE_TICKS, |g| g.clamp(1, 24) as u64);
        for l in v
            .get("lanes")
            .and_then(JsonValue::as_array)
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let pad = l
                .get("pad")
                .and_then(JsonValue::as_i64)
                .ok_or("a lane needs \"pad\"")?;
            let steps = l.get("steps").and_then(JsonValue::ints).ok_or_else(|| {
                format!("lane {pad}: needs \"steps\", a list of velocities")
            })?;
            if steps.len() != p.len() {
                return Err(format!(
                    "lane {pad}: {} steps, the pattern has {}",
                    steps.len(),
                    p.len()
                ));
            }
            if steps.iter().any(|v| !(0..=127).contains(v)) {
                return Err(format!("lane {pad}: velocities are 0-127"));
            }
            if !(1..=INPUTS as i64).contains(&pad) {
                return Err(format!("lane {pad}: pads are 1-{INPUTS}"));
            }
            let lane = p.lane_mut(pad as u8).expect("checked");
            lane.steps = steps.iter().map(|v| *v as u8).collect();
        }
        Ok(p)
    }

    /// `Pad7 kick` style label for a lane's row: the pad's name, then what the kit put on it.
    pub fn lane_label(input: u8, sample: Option<&str>) -> String {
        match sample {
            Some(s) if !s.is_empty() => format!("{} {s}", input_name(input)),
            _ => input_name(input),
        }
    }
}

/// Put a hit on `pad` only if the kit has it.
fn put(p: &mut KitPattern, pads: &[u8], pad: u8, step: usize, v: u8) {
    if pads.contains(&pad) {
        let _ = p.set_step(pad, step, v.clamp(1, 127));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [u8; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 9];

    #[test]
    fn house_matches_the_default_groove_on_the_pads_it_has() {
        let p = KitPattern::house(&[7, 8, 9]);
        let sp = p.to_step_pattern(|_| false);
        let def = StepPattern::pd705_default();
        for want in def.lanes.iter().filter(|l| l.note != 49) {
            let got = sp.lanes.iter().find(|l| l.note == want.note).unwrap();
            assert_eq!(got.steps, want.steps, "note {}", want.note);
        }
        assert_eq!(sp.gate_ticks, def.gate_ticks);
        // a kit without pad 9 has no hat lane
        let p = KitPattern::house(&[7, 8]);
        assert!(p.lane(9).is_none());
        assert_eq!(p.lanes.len(), 2);
    }

    #[test]
    fn clicking_a_cell_steps_through_the_levels() {
        let mut p = KitPattern::empty();
        let seen: Vec<u8> = (0..5).map(|_| p.cycle_step(7, 3).unwrap()).collect();
        assert_eq!(seen, [60, 100, 127, 0, 60]);
        // a velocity between levels counts as the level above it
        p.set_step(7, 3, 90).unwrap();
        assert_eq!(p.cycle_step(7, 3).unwrap(), 100);
        assert!(p.cycle_step(0, 0).is_err());
        assert!(p.cycle_step(16, 0).is_err());
        assert!(p.set_step(7, 16, 100).unwrap_err().contains("0-15"));
        assert!(p.set_step(7, 0, 128).is_err());
    }

    #[test]
    fn lanes_stay_sorted_and_unique() {
        let mut p = KitPattern::empty();
        for pad in [9, 2, 7, 2, 15, 1] {
            p.set_step(pad, 0, 100).unwrap();
        }
        let pads: Vec<u8> = p.lanes.iter().map(|l| l.input).collect();
        assert_eq!(pads, [1, 2, 7, 9, 15]);
        assert!(p.remove_lane(7));
        assert!(!p.remove_lane(7));
    }

    #[test]
    fn growing_repeats_and_shrinking_cuts() {
        let mut p = KitPattern::house(&ALL);
        let one = p.lane(7).unwrap().steps.clone();
        p.set_bars(2).unwrap();
        assert_eq!(p.len(), 32);
        let two = &p.lane(7).unwrap().steps;
        assert_eq!(&two[..16], &one[..]);
        assert_eq!(&two[16..], &one[..]);
        // vary the second bar, then cut back: the first bar is untouched
        p.set_step(7, 17, 127).unwrap();
        p.set_bars(1).unwrap();
        assert_eq!(p.lane(7).unwrap().steps, one);
        assert!(p.set_bars(0).is_err());
        assert!(p.set_bars(5).is_err());
        // a lane added after growing has the new length
        p.set_bars(4).unwrap();
        p.set_step(3, 63, 80).unwrap();
        assert_eq!(p.lane(3).unwrap().steps.len(), 64);
    }

    #[test]
    fn a_loop_pads_notes_never_reach_the_clock() {
        let p = KitPattern::from_seed(3.0, &ALL);
        let all = p.to_step_pattern(|_| false);
        // Pads 1-3 hold loops in the Acid House kit: notes 49 57 51
        let held = p.to_step_pattern(|n| matches!(n, 49 | 57 | 51));
        assert!(held.lanes.iter().all(|l| !matches!(l.note, 49 | 57 | 51)));
        assert!(held.lanes.len() <= all.lanes.len());
        assert!(held.lanes.iter().all(|l| l.steps.len() == 16));
    }

    #[test]
    fn a_seed_gives_the_same_pattern_and_another_a_different_one() {
        let a = KitPattern::from_seed(1.0, &ALL);
        assert_eq!(a, KitPattern::from_seed(1.0, &ALL));
        assert_ne!(a, KitPattern::from_seed(2.0, &ALL));
        assert!(a.hits() > 8);
        // only the pads the kit has
        let b = KitPattern::from_seed(1.0, &[7, 8]);
        assert!(b.lanes.iter().all(|l| l.input == 7 || l.input == 8));
        assert!(KitPattern::from_seed(1.0, &[]).is_empty());
    }

    #[test]
    fn presets_by_name() {
        for n in KitPattern::PRESETS {
            assert!(KitPattern::preset(n, &ALL, 1.0).is_some(), "{n}");
        }
        assert!(KitPattern::preset("nope", &ALL, 1.0).is_none());
        assert!(KitPattern::preset("empty", &ALL, 1.0).unwrap().is_empty());
        assert_eq!(KitPattern::breaks(&ALL).hits(), 15);
        assert_eq!(KitPattern::halftime(&ALL).hits(), 19);
    }

    #[test]
    fn json_round_trips_and_refuses_bad_documents() {
        let mut p = KitPattern::breaks(&ALL);
        p.set_bars(2).unwrap();
        p.set_step(4, 20, 99).unwrap();
        let text = p.to_json().canonical_js();
        let back = KitPattern::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
        assert_eq!(back, p);
        let bad = |t: &str| KitPattern::from_json(&JsonValue::parse(t).unwrap());
        assert!(bad(r#"{"bars":9}"#).is_err());
        assert!(bad(r#"{"bars":1,"lanes":[{"pad":7,"steps":[1,2]}]}"#)
            .unwrap_err()
            .contains("2 steps"));
        assert!(bad(r#"{"bars":1,"lanes":[{"pad":99,"steps":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}]}"#).is_err());
        assert!(bad(r#"{"bars":1,"lanes":[{"pad":7,"steps":[200,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}]}"#).is_err());
        assert_eq!(bad("{}").unwrap(), KitPattern::empty());
    }

    #[test]
    fn rows_and_lines() {
        let p = KitPattern::house(&[7, 8, 9]);
        assert_eq!(p.row(7), "x---x---x---x---");
        assert_eq!(p.row(1), "----------------");
        assert_eq!(p.line(), "1 bar, 3 lanes, 14 hits");
        assert_eq!(KitPattern::lane_label(7, Some("Kick")), "Pad7 Kick");
        assert_eq!(KitPattern::lane_label(14, None), "Foot1");
    }

    #[test]
    fn a_two_bar_pattern_plays_two_bars_on_the_clock() {
        use crate::clock::{ClockEngine, OutputConfig, OutputRole};
        let mut p = KitPattern::empty();
        p.set_bars(2).unwrap();
        // a kick on the first step of each bar, a snare only in bar two
        p.set_step(7, 0, 100).unwrap();
        p.set_step(7, 16, 100).unwrap();
        p.set_step(8, 20, 100).unwrap();
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_pattern(p.to_step_pattern(|_| false));
        c.set_outputs(
            alloc::vec![OutputConfig {
                port: String::from("DrumPad"),
                role: OutputRole::Steps { channel: 9 },
                offset: 0.0,
                enabled: true,
            }],
            0.0,
        );
        c.start(1.0);
        let mut ons: Vec<(f64, u8)> = Vec::new();
        let mut t = 0.0;
        while t < 1.0 + 4.0 + 1e-6 {
            t += 0.001;
            for e in c.pop_due(t) {
                if e.bytes[0] == 0x99 {
                    ons.push((e.time, e.bytes[1]));
                }
            }
        }
        // 120 BPM: a bar is 2 s. Kicks at +0 and +2 s, the snare 20 sixteenths in (+2.5 s),
        // and the pattern wraps at +4 s
        let kicks: Vec<f64> = ons.iter().filter(|o| o.1 == 36).map(|o| o.0).collect();
        let snares: Vec<f64> = ons.iter().filter(|o| o.1 == 38).map(|o| o.0).collect();
        assert_eq!(kicks.len(), 3, "{ons:?}");
        for (k, want) in kicks.iter().zip([1.0, 3.0, 5.0]) {
            assert!((k - want).abs() < 0.002, "{kicks:?}");
        }
        assert_eq!(snares.len(), 1);
        assert!((snares[0] - 3.5).abs() < 0.002, "{snares:?}");
    }
}
