//! Sets: a named, ordered run of saved compositions laid out on an energy curve. The pure
//! half of `Session/SetStore.swift`: the models, `SetPlanner` and `SetAuto`. The files are
//! `crate::host::stores`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::json::{JsonObject, JsonValue};

/// `SetPlanSlot`.
#[derive(Clone, Debug, PartialEq)]
pub struct SetPlanSlot {
    pub label: String,
    /// 0…1.
    pub energy: f64,
    pub bpm: Option<f64>,
    /// "A minor", "F# major".
    pub key: Option<String>,
    /// Wanted length, a hint for composing.
    pub bars: Option<i64>,
}

impl SetPlanSlot {
    pub fn new(label: &str, energy: f64) -> Self {
        SetPlanSlot {
            label: label.to_string(),
            energy: energy.min(1.0).max(0.0),
            bpm: None,
            key: None,
            bars: None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("label", JsonValue::str(self.label.as_str()));
        o.set("energy", JsonValue::Number(self.energy));
        if let Some(b) = self.bpm {
            o.set("bpm", JsonValue::Number(b));
        }
        if let Some(k) = &self.key {
            o.set("key", JsonValue::str(k.as_str()));
        }
        if let Some(b) = self.bars {
            o.set("bars", JsonValue::int(b));
        }
        JsonValue::Object(o)
    }

    /// Codable decode: `label` and `energy` required, no clamping.
    pub fn from_json(v: &JsonValue) -> Option<Self> {
        Some(SetPlanSlot {
            label: v.get("label")?.as_str()?.to_string(),
            energy: v.get("energy")?.as_f64()?,
            bpm: v.get("bpm").and_then(JsonValue::as_f64),
            key: v.get("key").and_then(JsonValue::as_str).map(String::from),
            bars: v.get("bars").and_then(JsonValue::as_i64),
        })
    }
}

/// `SetItem`.
#[derive(Clone, Debug, PartialEq)]
pub struct SetItem {
    /// A `CompositionStore` entry id.
    pub composition: String,
    /// Index into the plan.
    pub slot: Option<i64>,
    /// Start mixing out here.
    pub mix_out_bar: Option<i64>,
    /// Crossfade length into the next item.
    pub fade_bars: Option<i64>,
}

impl SetItem {
    pub fn new(composition: &str) -> Self {
        SetItem {
            composition: composition.to_string(),
            slot: None,
            mix_out_bar: None,
            fade_bars: None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("composition", JsonValue::str(self.composition.as_str()));
        if let Some(s) = self.slot {
            o.set("slot", JsonValue::int(s));
        }
        if let Some(s) = self.mix_out_bar {
            o.set("mixOutBar", JsonValue::int(s));
        }
        if let Some(s) = self.fade_bars {
            o.set("fadeBars", JsonValue::int(s));
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Option<Self> {
        Some(SetItem {
            composition: v.get("composition")?.as_str()?.to_string(),
            slot: v.get("slot").and_then(JsonValue::as_i64),
            mix_out_bar: v.get("mixOutBar").and_then(JsonValue::as_i64),
            fade_bars: v.get("fadeBars").and_then(JsonValue::as_i64),
        })
    }
}

/// `DJSet`.
#[derive(Clone, Debug, PartialEq)]
pub struct DjSet {
    pub v: i64,
    pub name: String,
    pub created: String,
    pub plan: Vec<SetPlanSlot>,
    pub items: Vec<SetItem>,
    pub cursor: i64,
    /// Default crossfade length in bars (an item's `fadeBars` wins).
    pub fade_bars: i64,
}

impl DjSet {
    pub fn new(name: &str) -> Self {
        DjSet {
            v: 1,
            name: name.to_string(),
            created: String::new(),
            plan: Vec::new(),
            items: Vec::new(),
            cursor: 0,
            fade_bars: 8,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("v", JsonValue::int(self.v));
        o.set("name", JsonValue::str(self.name.as_str()));
        o.set("created", JsonValue::str(self.created.as_str()));
        o.set(
            "plan",
            JsonValue::Array(self.plan.iter().map(SetPlanSlot::to_json).collect()),
        );
        o.set(
            "items",
            JsonValue::Array(self.items.iter().map(SetItem::to_json).collect()),
        );
        o.set("cursor", JsonValue::int(self.cursor));
        o.set("fadeBars", JsonValue::int(self.fade_bars));
        JsonValue::Object(o)
    }

    /// Codable decode: every non-optional key must be present or the set is unreadable.
    pub fn from_json(v: &JsonValue) -> Option<Self> {
        let plan = v
            .get("plan")?
            .as_array()?
            .iter()
            .map(SetPlanSlot::from_json)
            .collect::<Option<Vec<_>>>()?;
        let items = v
            .get("items")?
            .as_array()?
            .iter()
            .map(SetItem::from_json)
            .collect::<Option<Vec<_>>>()?;
        Some(DjSet {
            v: v.get("v")?.as_i64()?,
            name: v.get("name")?.as_str()?.to_string(),
            created: v.get("created")?.as_str()?.to_string(),
            plan,
            items,
            cursor: v.get("cursor")?.as_i64()?,
            fade_bars: v.get("fadeBars")?.as_i64()?,
        })
    }
}

/// `CompositionStore.slug`: ASCII letters and digits, runs of anything else become one `-`,
/// at most 40 characters, `untitled` when empty.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.to_lowercase().chars() {
        if ch.is_alphanumeric() && ch.is_ascii() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    let t = if t.is_empty() { "untitled" } else { t };
    t.chars().take(40).collect()
}

/// `CompositionInfo`.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionInfo {
    pub id: String,
    pub name: String,
    pub energy: Option<f64>,
    pub key: Option<String>,
    pub tempo: Option<f64>,
    pub bars: Option<i64>,
}

impl CompositionInfo {
    pub fn new(id: &str, name: &str) -> Self {
        CompositionInfo {
            id: id.to_string(),
            name: name.to_string(),
            energy: None,
            key: None,
            tempo: None,
            bars: None,
        }
    }
}

/// `SetPlanner.Gap`: a slot nothing saved fits.
#[derive(Clone, Debug, PartialEq)]
pub struct Gap {
    pub slot: usize,
    pub label: String,
    pub energy: f64,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub bars: Option<i64>,
    /// The closest thing saved, and why it was refused (None: nothing saved at all).
    pub closest: Option<String>,
}

/// `SetPlanner.Result`.
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceResult {
    pub items: Vec<SetItem>,
    pub gaps: Vec<Gap>,
    /// Neighbouring tracks whose tempos can't be matched on the fader: `(from, to, ratio)`.
    pub tempo_jumps: Vec<(usize, usize, f64)>,
}

/// `SetPlanner`.
pub struct SetPlanner;

impl SetPlanner {
    /// How far a composition is from a slot: 0 = exact.
    pub fn cost(slot: &SetPlanSlot, c: &CompositionInfo) -> f64 {
        let mut cost = 2.0 * (slot.energy - c.energy.unwrap_or(0.5)).abs();
        if let (Some(want), Some(have)) = (slot.bpm, c.tempo) {
            if want > 0.0 && have > 0.0 {
                cost += 4.0 * Self::tempo_gap(have, want);
            }
        }
        if let (Some(a), Some(b)) = (&slot.key, &c.key) {
            if let Some(d) = Self::key_distance(a, b) {
                cost += 0.12 * d as f64;
            }
        }
        cost
    }

    /// Relative tempo gap after the best of 1×, ½×, 2× (0 = same).
    pub fn tempo_gap(a: f64, b: f64) -> f64 {
        [1.0, 0.5, 2.0]
            .iter()
            .map(|m| (a * m / b - 1.0).abs())
            .fold(f64::INFINITY, f64::min)
    }

    /// Steps apart on the circle of fifths, relative keys counting as the same.
    pub fn key_distance(a: &str, b: &str) -> Option<i64> {
        let x = Self::fifths(a)?;
        let y = Self::fifths(b)?;
        let d = (x - y).abs() % 12;
        Some(core::cmp::min(d, 12 - d))
    }

    /// A key's position on the circle of fifths as its relative major (C = 0, G = 1, F = −1…).
    pub fn fifths(key: &str) -> Option<i64> {
        let s = key.trim_matches(|c: char| c == ' ' || c == '\t');
        let first = s.chars().next()?;
        if !"ABCDEFGabcdefg".contains(first) {
            return None;
        }
        let base = match first.to_ascii_uppercase() {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            _ => 11,
        };
        let mut pc: i64 = base;
        let rest = &s[first.len_utf8()..];
        match rest.chars().next() {
            Some('#') | Some('♯') => pc += 1,
            Some('b') | Some('♭') => pc -= 1,
            _ => {}
        }
        let lower = s.to_lowercase();
        let minor = lower.contains("min") || (rest.contains('m') && !lower.contains("maj"));
        if minor {
            pc += 3;
        }
        pc = ((pc % 12) + 12) % 12;
        Some((pc * 7) % 12)
    }

    /// Fill the plan, slot by slot, from `pool` (each composition once): `kept` items keep
    /// their mix overrides. Slots whose best candidate costs more than `max_cost` are gaps.
    pub fn sequence(
        plan: &[SetPlanSlot],
        pool: &[CompositionInfo],
        kept: &[SetItem],
        max_cost: f64,
    ) -> SequenceResult {
        let mut free: Vec<CompositionInfo> = pool.to_vec();
        let mut items = Vec::new();
        let mut gaps = Vec::new();
        let mut chosen: Vec<CompositionInfo> = Vec::new();
        for (i, slot) in plan.iter().enumerate() {
            let mut ranked: Vec<(CompositionInfo, f64)> = free
                .iter()
                .map(|c| (c.clone(), Self::cost(slot, c)))
                .collect();
            ranked.sort_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.0.id.cmp(&b.0.id))
            });
            match ranked.first() {
                Some((best, c)) if *c <= max_cost => {
                    let best = best.clone();
                    free.retain(|f| f.id != best.id);
                    let mut item = kept
                        .iter()
                        .find(|k| k.composition == best.id)
                        .cloned()
                        .unwrap_or_else(|| SetItem::new(&best.id));
                    item.slot = Some(i as i64);
                    items.push(item);
                    chosen.push(best);
                }
                _ => gaps.push(Gap {
                    slot: i,
                    label: slot.label.clone(),
                    energy: slot.energy,
                    bpm: slot.bpm,
                    key: slot.key.clone(),
                    bars: slot.bars,
                    closest: ranked
                        .first()
                        .map(|(c, cost)| format!("{} (cost {:.2})", c.name, cost)),
                }),
            }
        }
        let mut jumps = Vec::new();
        for i in 1..core::cmp::max(1, chosen.len()) {
            if let (Some(a), Some(b)) = (chosen[i - 1].tempo, chosen[i].tempo) {
                if Self::tempo_gap(a, b) > 0.08 {
                    jumps.push((i - 1, i, b / a));
                }
            }
        }
        SequenceResult {
            items,
            gaps,
            tempo_jumps: jumps,
        }
    }

    /// `maxCost` default.
    pub const DEFAULT_MAX_COST: f64 = 0.6;

    /// A ready-made energy curve of `count` slots: `arc`, `rise` or `plateau`. `bpm` maps the
    /// curve's low and high energy onto a tempo range.
    pub fn curve(shape: &str, count: usize, bpm: Option<(f64, f64)>) -> Vec<SetPlanSlot> {
        let n = count.clamp(1, 32);
        let energies: Vec<f64> = (0..n)
            .map(|i| {
                let t = if n == 1 {
                    0.5
                } else {
                    i as f64 / (n - 1) as f64
                };
                match shape {
                    "rise" => 0.3 + 0.7 * t,
                    "plateau" => {
                        if t < 0.25 {
                            0.3 + 0.6 * (t / 0.25)
                        } else if t > 0.85 {
                            0.55
                        } else {
                            0.9
                        }
                    }
                    _ => 0.35 + 0.6 * libm::sin(core::f64::consts::PI * t),
                }
            })
            .collect();
        let lo = energies.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = energies.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        energies
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                let label = if e < 0.4 {
                    if i < n / 2 {
                        "warm-up"
                    } else {
                        "cool-down"
                    }
                } else if e < 0.65 {
                    "build"
                } else if e < 0.85 {
                    "lift"
                } else {
                    "peak"
                };
                let tempo = bpm
                    .map(|(l, h)| l + (h - l) * if hi > lo { (e - lo) / (hi - lo) } else { 0.5 });
                let mut slot = SetPlanSlot::new(label, libm::round(e * 100.0) / 100.0);
                slot.bpm = tempo.map(libm::round);
                slot
            })
            .collect()
    }
}

/// `SetAuto`: when a running set hands off on its own.
pub struct SetAuto;

impl SetAuto {
    /// The bar (1-based) the fade starts at: the item's `mixOutBar`, else `fade_bars` before
    /// the track's last bar. None: the track's length isn't known and nothing was set.
    pub fn start_bar(bars: Option<i64>, item: &SetItem, fade_bars: i64) -> Option<i64> {
        if let Some(b) = item.mix_out_bar {
            return Some(core::cmp::max(1, b));
        }
        let bars = bars?;
        Some(core::cmp::max(1, bars - fade_bars + 1))
    }

    /// Begin the hand-off at `bar`: `lead_bars` early.
    pub fn due(bar: i64, start_bar: i64, lead_bars: i64) -> bool {
        bar >= start_bar - core::cmp::max(0, lead_bars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(
        id: &str,
        energy: Option<f64>,
        tempo: Option<f64>,
        key: Option<&str>,
    ) -> CompositionInfo {
        CompositionInfo {
            id: id.into(),
            name: id.into(),
            energy,
            key: key.map(Into::into),
            tempo,
            bars: Some(36),
        }
    }

    // SetPlannerTests.keysMeasureStepsOnTheCircleOfFifthsWithRelativesAsOne
    #[test]
    fn keys_measure_steps_on_the_circle_of_fifths_with_relatives_as_one() {
        let d = SetPlanner::key_distance;
        assert_eq!(d("A minor", "C major"), Some(0));
        assert_eq!(d("A minor", "E minor"), Some(1));
        assert_eq!(d("A minor", "D minor"), Some(1));
        assert_eq!(d("F# minor", "A major"), Some(0));
        assert_eq!(d("A minor", "Eb minor"), Some(6));
        assert_eq!(d("Am", "Em"), Some(1));
        assert_eq!(d("nonsense", "A minor"), None);
    }

    // SetPlannerTests.theBestFitFillsEachSlotInOrderAndEachTrackPlaysOnce
    #[test]
    fn the_best_fit_fills_each_slot_in_order_and_each_track_plays_once() {
        let mut a = SetPlanSlot::new("warm-up", 0.3);
        a.bpm = Some(124.0);
        let mut b = SetPlanSlot::new("peak", 0.95);
        b.bpm = Some(150.0);
        let mut c = SetPlanSlot::new("cool-down", 0.4);
        c.bpm = Some(124.0);
        let pool = [
            comp("hard", Some(0.95), Some(150.0), None),
            comp("chill", Some(0.3), Some(124.0), None),
            comp("mellow", Some(0.35), Some(125.0), None),
            comp("mid", Some(0.6), Some(130.0), None),
        ];
        let r = SetPlanner::sequence(&[a, b, c], &pool, &[], 0.6);
        let ids: Vec<&str> = r.items.iter().map(|i| i.composition.as_str()).collect();
        assert_eq!(ids, ["chill", "hard", "mellow"]);
        assert_eq!(
            r.items.iter().map(|i| i.slot).collect::<Vec<_>>(),
            [Some(0), Some(1), Some(2)]
        );
        assert!(r.gaps.is_empty());
        assert_eq!(
            r.tempo_jumps.iter().map(|j| (j.0, j.1)).collect::<Vec<_>>(),
            [(0, 1), (1, 2)]
        );
    }

    // SetPlannerTests.aSlotNothingFitsIsAGapWithTheBriefToCompose
    #[test]
    fn a_slot_nothing_fits_is_a_gap_with_the_brief_to_compose() {
        let mut slot = SetPlanSlot::new("peak", 1.0);
        slot.bpm = Some(150.0);
        slot.key = Some("A minor".into());
        slot.bars = Some(36);
        let r = SetPlanner::sequence(
            core::slice::from_ref(&slot),
            &[comp("lofi", Some(0.2), Some(80.0), None)],
            &[],
            0.6,
        );
        assert!(r.items.is_empty() && r.gaps.len() == 1);
        assert_eq!(r.gaps[0].label, "peak");
        assert_eq!(r.gaps[0].bpm, Some(150.0));
        assert_eq!(r.gaps[0].key.as_deref(), Some("A minor"));
        assert!(r.gaps[0].closest.as_deref().unwrap().starts_with("lofi"));
        let none = SetPlanner::sequence(core::slice::from_ref(&slot), &[], &[], 0.6);
        assert_eq!(none.gaps[0].closest, None);
    }

    // SetPlannerTests.halfAndDoubleTimeCountAsMatchableAndKeyBreaksTies
    #[test]
    fn half_and_double_time_count_as_matchable_and_key_breaks_ties() {
        let mut slot = SetPlanSlot::new("x", 0.5);
        slot.bpm = Some(140.0);
        slot.key = Some("A minor".into());
        assert!(SetPlanner::cost(&slot, &comp("half", Some(0.5), Some(70.0), None)) < 0.01);
        assert!(
            SetPlanner::cost(
                &slot,
                &comp("near", Some(0.5), Some(140.0), Some("E minor"))
            ) < SetPlanner::cost(
                &slot,
                &comp("far", Some(0.5), Some(140.0), Some("Eb minor"))
            )
        );
        assert_eq!(
            SetPlanner::cost(&slot, &comp("unknown", None, None, None)),
            0.0
        );
    }

    // SetPlannerTests.keptItemsKeepTheirMixOverrides
    #[test]
    fn kept_items_keep_their_mix_overrides() {
        let plan = [SetPlanSlot::new("a", 0.5)];
        let mut kept = SetItem::new("t");
        kept.mix_out_bar = Some(28);
        kept.fade_bars = Some(4);
        let r = SetPlanner::sequence(
            &plan,
            &[comp("t", Some(0.5), Some(120.0), None)],
            &[kept],
            0.6,
        );
        let mut want = SetItem::new("t");
        want.slot = Some(0);
        want.mix_out_bar = Some(28);
        want.fade_bars = Some(4);
        assert_eq!(r.items, [want]);
    }

    // SetPlannerTests.curvesShapeTheEnergyAndMapTempo
    #[test]
    fn curves_shape_the_energy_and_map_tempo() {
        let arc = SetPlanner::curve("arc", 5, Some((124.0, 150.0)));
        assert_eq!(arc.len(), 5);
        assert!(arc[2].energy > arc[0].energy && arc[2].energy > arc[4].energy);
        assert!(
            (arc[2].label == "peak" && arc[0].label == "warm-up" && arc[4].label == "warm-up")
                || arc[4].label == "cool-down"
        );
        assert!(arc[2].bpm == Some(150.0) && arc[0].bpm == Some(124.0));
        let rise = SetPlanner::curve("rise", 4, None);
        let e: Vec<f64> = rise.iter().map(|s| s.energy).collect();
        let mut sorted = e.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(e, sorted);
        assert_eq!(rise.last().unwrap().energy, 1.0);
        assert_eq!(rise[0].bpm, None);
        assert_eq!(SetPlanner::curve("arc", 1, None).len(), 1);
    }

    // SetPlannerTests.autoStartsTheFadeSoItEndsWithTheTrackAndBeginsALeadEarly
    #[test]
    fn auto_starts_the_fade_so_it_ends_with_the_track_and_begins_a_lead_early() {
        let plain = SetItem::new("t");
        assert_eq!(SetAuto::start_bar(Some(52), &plain, 8), Some(45));
        let mut m = SetItem::new("t");
        m.mix_out_bar = Some(30);
        assert_eq!(SetAuto::start_bar(Some(52), &m, 8), Some(30));
        assert_eq!(SetAuto::start_bar(None, &plain, 8), None);
        m.mix_out_bar = Some(20);
        assert_eq!(SetAuto::start_bar(None, &m, 8), Some(20));
        assert_eq!(SetAuto::start_bar(Some(4), &plain, 8), Some(1));
        assert!(!SetAuto::due(43, 45, 1));
        assert!(SetAuto::due(44, 45, 1) && SetAuto::due(50, 45, 1));
        assert!(SetAuto::due(45, 45, 0) && !SetAuto::due(44, 45, 0));
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slug("Daft Punk-style (original)"),
            "daft-punk-style-original"
        );
        assert_eq!(slug(""), "untitled");
        assert_eq!(slug("!!!"), "untitled");
        assert_eq!(slug(&"a".repeat(60)).len(), 40);
    }

    #[test]
    fn a_set_round_trips_its_json() {
        let mut set = DjSet::new("Friday Night");
        set.created = "2026-10-21T00:00:00Z".into();
        set.fade_bars = 6;
        let mut slot = SetPlanSlot::new("peak", 0.9);
        slot.bpm = Some(150.0);
        slot.key = Some("A minor".into());
        slot.bars = Some(36);
        set.plan = alloc::vec![slot];
        let mut it = SetItem::new("abc");
        it.slot = Some(0);
        it.mix_out_bar = Some(28);
        set.items = alloc::vec![it];
        let back = DjSet::from_json(&set.to_json()).unwrap();
        assert_eq!(back, set);
        assert!(DjSet::from_json(&JsonValue::parse("{\"name\":\"x\"}").unwrap()).is_none());
    }
}
