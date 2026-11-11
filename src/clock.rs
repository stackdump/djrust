//! The MIDI clock master (docs/HUB.md, M2), as a pure event source: 24-PPQN clock, Start / Stop,
//! and a 16-step note pattern for gear that ignores clock (the PD705), each output shifted earlier
//! by its own latency offset. A host thread asks [`ClockEngine::next_time`], waits until then and
//! sends what [`ClockEngine::pop_due`] returns. Times are seconds on the host's monotonic clock.
//!
//! Ticks sit on absolute deadlines (`base_time + (k - base_tick) * period`): a tempo change
//! re-anchors at the next tick, so no error accumulates, and a thread that wakes late sends at
//! most one overdue tick and skips the rest instead of bursting.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Clock ticks per quarter note.
pub const PPQN: u64 = 24;
/// Ticks per sixteenth (one pattern step).
pub const TICKS_PER_STEP: u64 = PPQN / 4;
/// Steps in a pattern.
pub const STEPS: usize = 16;
/// Later than this, overdue ticks are skipped rather than sent (a stall, a laptop lid).
pub const MAX_LATE_S: f64 = 0.050;
/// The longest an [`ClockEngine::align`] call moves the clock, in ticks (a gentle phase lock).
pub const MAX_ALIGN_TICKS: f64 = 0.25;
/// The phase lock's time constant, seconds: it takes back about this fraction of the error per
/// second ([`phase_correction`]).
pub const LOCK_TAU_S: f64 = 0.5;
/// The fastest the phase lock moves the clock, ticks per second: 2 ticks/s is ~4.6% of the
/// tick rate at 108 BPM, a lean, not the ±35% swing a per-frame step of
/// [`MAX_ALIGN_TICKS`] at 60 frames/s gives.
pub const MAX_SLEW_TICKS_PER_S: f64 = 2.0;
/// Errors under this many ticks are left alone.
pub const LOCK_DEADBAND_TICKS: f64 = 0.02;

/// The phase lock's correction for one host frame of `dt` seconds, in ticks for
/// [`ClockEngine::align`] (positive: later). `err_beats` is the master deck's beat minus the
/// clock's (both from Start's bar line). A master deck that is not playing has a frozen beat,
/// so its "error" only grows with the clock's own progress: nothing is corrected then.
/// Otherwise the error is taken modulo the bar (the nearest bar line wins), and corrected at
/// most [`MAX_SLEW_TICKS_PER_S`], independent of the frame rate.
pub fn phase_correction(err_beats: f64, dt: f64, master_playing: bool) -> f64 {
    if !master_playing || !(dt > 0.0) || !err_beats.is_finite() {
        return 0.0;
    }
    let err = err_beats - libm::round(err_beats / 4.0) * 4.0;
    let want = -err * PPQN as f64 * (dt / LOCK_TAU_S).min(1.0);
    let cap = (MAX_SLEW_TICKS_PER_S * dt).min(MAX_ALIGN_TICKS);
    let ticks = want.clamp(-cap, cap);
    if (err * PPQN as f64).abs() < LOCK_DEADBAND_TICKS {
        0.0
    } else {
        ticks
    }
}

/// The phase lock's new origin when it moves to another deck while the clock runs (an auto
/// transport re-lock): the deck's beat that the clock's beat 0 stands for, chosen so the clock
/// goes on as it is and only the part of a beat between the two is corrected. Whole beats are
/// kept, so the gear does not slew bars to catch up with a deck that is beats off.
pub fn rebase_origin(deck_beat: f64, clock_beat: f64) -> f64 {
    libm::round(deck_beat - clock_beat)
}

/// What an output gets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutputRole {
    /// Clock and Start / Stop (the TD-3).
    Clock,
    /// The pattern's notes on this channel (0-based), played on the clock (the PD705, ch 10 =
    /// 9). No clock bytes.
    Steps { channel: u8 },
}

/// One output's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputConfig {
    /// The MIDI port's name (the host opens it).
    pub port: String,
    pub role: OutputRole,
    /// Send this much earlier, seconds (the device's latency to the speakers).
    pub offset: f64,
    pub enabled: bool,
}

/// A step pattern: lanes of velocities (0 = rest), 16 steps to a bar. A lane may be longer than
/// a bar (a PD705 kit's pattern, `gear::pd705_pattern`), and each lane cycles on its own length.
#[derive(Clone, Debug, PartialEq)]
pub struct StepPattern {
    pub lanes: Vec<Lane>,
    /// How long a hit's note is held, in ticks (6 = a sixteenth).
    pub gate_ticks: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub note: u8,
    pub name: &'static str,
    /// One velocity per sixteenth, 0 = rest; the lane cycles on its own length (16 for the
    /// default groove). An empty lane plays nothing.
    pub steps: Vec<u8>,
}

impl StepPattern {
    /// A house groove on the PD705's factory notes: kick on the beats, snare on 2 and 4, hats
    /// on the off-beats with the eighths softer.
    pub fn pd705_default() -> StepPattern {
        let mut kick = [0u8; STEPS];
        let mut snare = [0u8; STEPS];
        let mut hat = [0u8; STEPS];
        for s in 0..STEPS {
            if s % 4 == 0 {
                kick[s] = 110;
            }
            if s % 8 == 4 {
                snare[s] = 105;
            }
            if s % 2 == 0 {
                hat[s] = if s % 4 == 2 { 100 } else { 60 };
            }
        }
        StepPattern {
            lanes: vec![
                Lane {
                    note: 36,
                    name: "kick",
                    steps: kick.to_vec(),
                },
                Lane {
                    note: 38,
                    name: "snare",
                    steps: snare.to_vec(),
                },
                Lane {
                    note: 42,
                    name: "hat",
                    steps: hat.to_vec(),
                },
                Lane {
                    note: 49,
                    name: "crash",
                    steps: vec![0; STEPS],
                },
            ],
            gate_ticks: 3,
        }
    }

    /// This pattern without the lanes whose note `skip` says to hold back (a PD705 kit's loop
    /// pads, `gear::pd705::Kit::holds_back`).
    pub fn without(&self, skip: impl Fn(u8) -> bool) -> StepPattern {
        StepPattern {
            lanes: self
                .lanes
                .iter()
                .filter(|l| !skip(l.note))
                .cloned()
                .collect(),
            gate_ticks: self.gate_ticks,
        }
    }
}

/// One message due on one output.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockEvent {
    pub time: f64,
    pub output: usize,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct OutState {
    cfg: OutputConfig,
    /// The next tick this output has not handled.
    next_tick: u64,
    /// Note-offs waiting: (time, bytes).
    offs: Vec<(f64, [u8; 3])>,
    /// Immediate messages (Stop), sent at the next pop.
    now: Vec<Vec<u8>>,
}

/// The clock master's state.
#[derive(Clone, Debug)]
pub struct ClockEngine {
    outputs: Vec<OutState>,
    pattern: StepPattern,
    base_tick: u64,
    base_time: f64,
    /// Seconds per tick.
    period: f64,
    bpm: f64,
    /// The tick Start lands on, once started.
    start_tick: Option<u64>,
    running: bool,
    /// Send clock while stopped too (receivers show the tempo and lock before Start).
    pub clock_always: bool,
    /// Ticks that carried something (a clock byte, a step's notes) and were not sent because
    /// the host woke too late. Ticks with nothing on them are passed over, not counted: a steps
    /// output wakes the host only for its notes, so it always passes over the ticks between.
    pub skipped: u64,
}

fn period_of(bpm: f64) -> f64 {
    60.0 / (bpm.clamp(20.0, 400.0) * PPQN as f64)
}

impl ClockEngine {
    /// A clock at `bpm` whose tick 0 is at `now`.
    pub fn new(bpm: f64, now: f64) -> ClockEngine {
        ClockEngine {
            outputs: Vec::new(),
            pattern: StepPattern::pd705_default(),
            base_tick: 0,
            base_time: now,
            period: period_of(bpm),
            bpm: bpm.clamp(20.0, 400.0),
            start_tick: None,
            running: false,
            clock_always: true,
            skipped: 0,
        }
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }
    pub fn is_running(&self) -> bool {
        self.running
    }
    /// The tick Start landed on (None before the first Start).
    pub fn start_tick(&self) -> Option<u64> {
        self.start_tick
    }
    pub fn pattern(&self) -> &StepPattern {
        &self.pattern
    }
    pub fn set_pattern(&mut self, p: StepPattern) {
        self.pattern = p;
    }

    /// When tick `k` falls (before any output's offset).
    pub fn time_of(&self, k: u64) -> f64 {
        self.base_time + (k as f64 - self.base_tick as f64) * self.period
    }

    /// The tick position at `t`, fractional.
    pub fn tick_at(&self, t: f64) -> f64 {
        self.base_tick as f64 + (t - self.base_time) / self.period
    }

    /// Beats since Start at time `t` (None while stopped).
    pub fn beat_at(&self, t: f64) -> Option<f64> {
        let s = self.start_tick?;
        self.running
            .then(|| (self.tick_at(t) - s as f64) / PPQN as f64)
    }

    /// The first tick at or after `t`.
    fn tick_after(&self, t: f64) -> u64 {
        libm::ceil(self.tick_at(t) - 1e-9).max(0.0) as u64
    }

    /// Replace the outputs. An output keeps its place in the tick stream if its port stays.
    pub fn set_outputs(&mut self, cfgs: Vec<OutputConfig>, now: f64) {
        let next = self.tick_after(now);
        let mut out = Vec::new();
        for c in cfgs {
            let old = self.outputs.iter().position(|o| o.cfg.port == c.port);
            match old {
                Some(i) => {
                    let mut o = self.outputs[i].clone();
                    o.cfg = c;
                    out.push(o);
                }
                None => out.push(OutState {
                    // the first tick whose shifted time is still ahead
                    next_tick: self.tick_after(now + c.offset).max(next),
                    cfg: c,
                    offs: Vec::new(),
                    now: Vec::new(),
                }),
            }
        }
        self.outputs = out;
    }

    pub fn outputs(&self) -> impl Iterator<Item = &OutputConfig> {
        self.outputs.iter().map(|o| &o.cfg)
    }

    /// A new tempo from the next tick on (ticks already due keep their times).
    pub fn set_bpm(&mut self, bpm: f64, now: f64) {
        let bpm = bpm.clamp(20.0, 400.0);
        if (bpm - self.bpm).abs() < 1e-9 {
            return;
        }
        let k = self.tick_after(now);
        self.base_time = self.time_of(k);
        self.base_tick = k;
        self.period = period_of(bpm);
        self.bpm = bpm;
    }

    /// Start: Start lands on the first tick at or after `at` (its clock byte follows Start, as
    /// MIDI wants). Pattern steps count from there. Returns that tick's time.
    pub fn start(&mut self, at: f64) -> f64 {
        let k = self.tick_after(at);
        self.start_tick = Some(k);
        self.running = true;
        self.time_of(k)
    }

    /// Stop now: Stop to every clock output, every held note let go.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        for o in &mut self.outputs {
            if !o.cfg.enabled {
                continue;
            }
            let offs: Vec<[u8; 3]> = o.offs.drain(..).map(|(_, b)| b).collect();
            for b in offs {
                o.now.push(b.to_vec());
            }
            if o.cfg.role == OutputRole::Clock {
                o.now.push(vec![0xFC]);
            }
        }
    }

    /// Move the clock by `ticks` (positive: later), at most [`MAX_ALIGN_TICKS`] per call. A
    /// host calls it every frame with the phase error to the master deck: a gentle phase lock.
    pub fn align(&mut self, ticks: f64) {
        let t = ticks.clamp(-MAX_ALIGN_TICKS, MAX_ALIGN_TICKS);
        self.base_time += t * self.period;
    }

    /// What tick `k` carries for output `o`.
    fn tick_messages(&self, o: &OutState, k: u64) -> Vec<Vec<u8>> {
        let mut m = Vec::new();
        let started = self.running && self.start_tick.is_some_and(|s| k >= s);
        match o.cfg.role {
            OutputRole::Clock => {
                if self.running && self.start_tick == Some(k) {
                    m.push(vec![0xFA]);
                }
                let pre_roll = self.start_tick.is_some_and(|s| k + PPQN >= s);
                if self.clock_always || started || (self.running && pre_roll) {
                    m.push(vec![0xF8]);
                }
            }
            OutputRole::Steps { channel } => {
                if let (true, Some(s)) = (started, self.start_tick) {
                    let d = k - s;
                    if d % TICKS_PER_STEP == 0 {
                        let n = d / TICKS_PER_STEP;
                        for lane in &self.pattern.lanes {
                            if lane.steps.is_empty() {
                                continue;
                            }
                            let v = lane.steps[(n % lane.steps.len() as u64) as usize];
                            if v > 0 {
                                m.push(vec![0x90 | (channel & 0x0F), lane.note & 0x7F, v.min(127)]);
                            }
                        }
                    }
                }
            }
        }
        m
    }

    /// When the next message is due on any output (None: nothing will ever be, e.g. no outputs).
    pub fn next_time(&self) -> Option<f64> {
        let mut best: Option<f64> = None;
        let mut take = |t: f64| best = Some(best.map_or(t, |b: f64| b.min(t)));
        for o in self.outputs.iter().filter(|o| o.cfg.enabled) {
            if !o.now.is_empty() {
                take(f64::NEG_INFINITY);
            }
            for (t, _) in &o.offs {
                take(*t);
            }
            // the next tick that carries something for this output
            let carries = |k: u64| !self.tick_messages(o, k).is_empty();
            let mut k = o.next_tick;
            let limit = k + PPQN * 8;
            while k < limit && !carries(k) {
                k += 1;
            }
            if k < limit {
                take(self.time_of(k) - o.cfg.offset);
            } else if o.cfg.role == OutputRole::Clock {
                // nothing for 8 beats (stopped, no clock_always): wake to recheck
                take(self.time_of(o.next_tick) - o.cfg.offset);
            }
        }
        best
    }

    /// Every message due at or before `now`, in time order (ties: output order, then Start before
    /// clock before notes). Overdue ticks later than [`MAX_LATE_S`] are skipped.
    pub fn pop_due(&mut self, now: f64) -> Vec<ClockEvent> {
        let mut ev = Vec::new();
        let period = self.period;
        for i in 0..self.outputs.len() {
            if !self.outputs[i].cfg.enabled {
                // keep a disabled output's place, so enabling it does not burst
                let off = self.outputs[i].cfg.offset;
                let k = self.tick_after(now + off);
                let o = &mut self.outputs[i];
                o.next_tick = o.next_tick.max(k);
                o.now.clear();
                continue;
            }
            for b in core::mem::take(&mut self.outputs[i].now) {
                ev.push(ClockEvent {
                    time: now,
                    output: i,
                    bytes: b,
                });
            }
            let off = self.outputs[i].cfg.offset;
            // late beyond the bound: jump to the newest overdue tick
            let due_tick = libm::floor(self.tick_at(now + off) + 1e-9).max(0.0) as u64;
            if self.time_of(self.outputs[i].next_tick) - off < now - MAX_LATE_S {
                let from = self.outputs[i].next_tick;
                let gap = due_tick.saturating_sub(from);
                // count what was really lost (a long stall: every tick, without looking)
                let lost = if gap > PPQN * 64 {
                    gap
                } else {
                    (from..due_tick)
                        .filter(|k| !self.tick_messages(&self.outputs[i], *k).is_empty())
                        .count() as u64
                };
                self.skipped += lost;
                self.outputs[i].next_tick = due_tick;
            }
            while self.time_of(self.outputs[i].next_tick) - off <= now + 1e-9 {
                let k = self.outputs[i].next_tick;
                let t = self.time_of(k) - off;
                let msgs = self.tick_messages(&self.outputs[i], k);
                let gate = self.pattern.gate_ticks.max(1) as f64 * period;
                for b in msgs {
                    if b[0] & 0xF0 == 0x90 {
                        self.outputs[i]
                            .offs
                            .push((t + gate, [b[0] & 0x0F | 0x80, b[1], 0]));
                    }
                    ev.push(ClockEvent {
                        time: t,
                        output: i,
                        bytes: b,
                    });
                }
                self.outputs[i].next_tick = k + 1;
            }
            let o = &mut self.outputs[i];
            let mut k = 0;
            while k < o.offs.len() {
                if o.offs[k].0 <= now + 1e-9 {
                    let (t, b) = o.offs.remove(k);
                    ev.push(ClockEvent {
                        time: t,
                        output: i,
                        bytes: b.to_vec(),
                    });
                } else {
                    k += 1;
                }
            }
        }
        ev.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.output.cmp(&b.output))
        });
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(port: &str, role: OutputRole, offset: f64) -> OutputConfig {
        OutputConfig {
            port: String::from(port),
            role,
            offset,
            enabled: true,
        }
    }

    /// Run the engine as a perfect host would, from `from` to `to`.
    fn run(c: &mut ClockEngine, to: f64) -> Vec<ClockEvent> {
        let mut all = Vec::new();
        while let Some(t) = c.next_time() {
            if t > to {
                break;
            }
            all.extend(c.pop_due(t.max(f64::MIN)));
        }
        all
    }

    #[test]
    fn rebase_keeps_whole_beats_and_corrects_the_rest() {
        // the new deck is 37.3 beats in, the clock 12.0: origin 25, error 0.3 beat
        let o = rebase_origin(37.3, 12.0);
        assert_eq!(o, 25.0);
        let err = (37.3 - o) - 12.0;
        assert!((err - 0.3).abs() < 1e-9);
        // a deck a little behind: the error is negative, never most of a beat
        let o = rebase_origin(9.8, 4.0);
        assert!(((9.8 - o) - 4.0 + 0.2).abs() < 1e-9);
        // the correction it leads to is a lean, not a jump
        let ticks = phase_correction((9.8 - o) - 4.0, 1.0 / 60.0, true);
        assert!(ticks.abs() <= MAX_SLEW_TICKS_PER_S / 60.0 + 1e-12);
    }

    #[test]
    fn ticks_at_24_ppqn_on_absolute_deadlines() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(vec![out("TD-3", OutputRole::Clock, 0.0)], 0.0);
        let ev = run(&mut c, 1.0);
        assert_eq!(ev.len(), 49, "ticks 0..=48 in one second at 120 BPM");
        for (k, e) in ev.iter().enumerate() {
            assert_eq!(e.bytes, vec![0xF8]);
            assert!((e.time - k as f64 / 48.0).abs() < 1e-12);
        }
    }

    #[test]
    fn start_lands_before_its_tick_and_stop_sends_stop() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(vec![out("TD-3", OutputRole::Clock, 0.0)], 0.0);
        run(&mut c, 0.1);
        let t = c.start(0.5);
        assert!((t - 0.5).abs() < 1e-12);
        let ev = run(&mut c, 0.5);
        let n = ev.len();
        assert_eq!(ev[n - 2].bytes, vec![0xFA]);
        assert_eq!(ev[n - 1].bytes, vec![0xF8]);
        assert_eq!(ev[n - 2].time, ev[n - 1].time);
        assert_eq!(c.beat_at(1.5), Some(2.0));
        run(&mut c, 1.5);
        c.stop();
        let ev = c.pop_due(1.51);
        assert_eq!(ev[0].bytes, vec![0xFC]);
        assert_eq!(c.beat_at(1.6), None);
    }

    #[test]
    fn a_kits_loop_pads_drop_out_of_the_pattern() {
        let k = crate::gear::pd705::Kit::preset("acid-house").unwrap();
        let p = StepPattern::pd705_default().without(|n| k.holds_back(n));
        let notes: Vec<u8> = p.lanes.iter().map(|l| l.note).collect();
        assert_eq!(notes, [36, 38, 42]);
        assert_eq!(p.gate_ticks, StepPattern::pd705_default().gate_ticks);
        let f = crate::gear::pd705::Kit::preset("factory").unwrap();
        assert_eq!(
            StepPattern::pd705_default().without(|n| f.holds_back(n)),
            StepPattern::pd705_default()
        );
    }

    #[test]
    fn steps_play_the_pattern_on_sixteenths_and_let_go() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(
            vec![out("DrumPad", OutputRole::Steps { channel: 9 }, 0.0)],
            0.0,
        );
        assert!(run(&mut c, 1.0).is_empty(), "nothing until Start");
        c.start(1.0);
        let ev = run(&mut c, 3.0 - 1e-6); // one bar at 120 BPM
        let ons: Vec<_> = ev.iter().filter(|e| e.bytes[0] == 0x99).collect();
        let offs = ev.iter().filter(|e| e.bytes[0] == 0x89).count();
        // the default groove: 4 kicks, 2 snares, 8 hats in a bar
        assert_eq!(ons.len(), 14);
        assert_eq!(offs, 14);
        let kicks: Vec<f64> = ons
            .iter()
            .filter(|e| e.bytes[1] == 36)
            .map(|e| e.time)
            .collect();
        assert_eq!(kicks, [1.0, 1.5, 2.0, 2.5]);
        let snare: Vec<f64> = ons
            .iter()
            .filter(|e| e.bytes[1] == 38)
            .map(|e| e.time)
            .collect();
        assert_eq!(snare, [1.5, 2.5]);
        // a hit is held for gate_ticks (3 ticks = 1/32 bar at 120 = 31.25 ms)
        let first_off = ev.iter().find(|e| e.bytes == [0x89, 36, 0]).unwrap();
        assert!((first_off.time - (1.0 + 3.0 / 48.0)).abs() < 1e-12);
        // stop lets every held note go at once
        c.start(4.0);
        run(&mut c, 4.0);
        c.stop();
        let ev = c.pop_due(4.001);
        assert!(ev.iter().all(|e| e.bytes[0] == 0x89) && !ev.is_empty());
    }

    #[test]
    fn offsets_send_earlier() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(
            vec![
                out("TD-3", OutputRole::Clock, 0.010),
                out("DrumPad", OutputRole::Steps { channel: 9 }, 0.004),
            ],
            0.0,
        );
        c.start(1.0);
        let ev = run(&mut c, 1.0);
        let fa = ev.iter().find(|e| e.bytes == [0xFA]).unwrap();
        assert!((fa.time - 0.990).abs() < 1e-12);
        let kick = ev.iter().find(|e| e.bytes[0] == 0x99).unwrap();
        assert!((kick.time - 0.996).abs() < 1e-12);
    }

    #[test]
    fn tempo_changes_keep_the_ticks_continuous() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(vec![out("TD-3", OutputRole::Clock, 0.0)], 0.0);
        run(&mut c, 0.5);
        c.set_bpm(150.0, 0.5);
        let ev = run(&mut c, 1.0);
        // ticks up to 0.5 s stayed at 120 (tick 24 fell at 0.5); from there every 1/60 s
        let times: Vec<f64> = ev.iter().map(|e| e.time).collect();
        assert!((times[0] - (0.5 + 1.0 / 60.0)).abs() < 1e-12, "{times:?}");
        for w in times.windows(2) {
            assert!((w[1] - w[0] - 1.0 / 60.0).abs() < 1e-12);
        }
    }

    #[test]
    fn a_late_host_skips_instead_of_bursting() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.set_outputs(vec![out("TD-3", OutputRole::Clock, 0.0)], 0.0);
        run(&mut c, 0.1);
        // the host stalls for a second
        let ev = c.pop_due(1.1);
        assert_eq!(ev.len(), 1, "one tick, not 48");
        assert!(c.skipped > 40);
        // and the next one is on the grid
        let t = c.next_time().unwrap();
        assert!((t * 48.0 - libm::round(t * 48.0)).abs() < 1e-9);
    }

    #[test]
    fn clock_only_while_running_has_a_beat_of_pre_roll() {
        let mut c = ClockEngine::new(120.0, 0.0);
        c.clock_always = false;
        c.set_outputs(vec![out("TD-3", OutputRole::Clock, 0.0)], 0.0);
        assert!(c.pop_due(0.3).is_empty());
        c.start(1.0);
        let ev = run(&mut c, 1.0);
        let first = ev.first().unwrap();
        assert!((first.time - 0.5).abs() < 1e-12, "a beat before Start");
        assert_eq!(ev.iter().filter(|e| e.bytes == [0xF8]).count(), 25);
    }

    #[test]
    fn align_moves_the_clock_gently() {
        let mut c = ClockEngine::new(120.0, 0.0);
        let before = c.time_of(100);
        c.align(10.0);
        assert!((c.time_of(100) - before - MAX_ALIGN_TICKS / 48.0).abs() < 1e-12);
    }

    #[test]
    fn a_disabled_output_keeps_its_place() {
        let mut c = ClockEngine::new(120.0, 0.0);
        let mut o = out("TD-3", OutputRole::Clock, 0.0);
        o.enabled = false;
        c.set_outputs(vec![o.clone()], 0.0);
        assert!(c.pop_due(2.0).is_empty());
        o.enabled = true;
        c.set_outputs(vec![o], 2.0);
        assert_eq!(c.pop_due(2.0).len(), 1, "no burst when enabled again");
    }

    /// The host as djapp runs it: the clock thread sends what is due, and every UI frame
    /// (60 Hz) the phase lock reads the master deck. `deck_beat(t)` is the deck's beat, `playing`
    /// whether it moves; `old_lock` uses the per-frame rule the app had (0.15 of the error,
    /// up to MAX_ALIGN_TICKS every frame, whether or not the deck played). Returns the
    /// note-ons, with their times.
    fn host(
        bpm: f64,
        seconds: f64,
        playing: bool,
        deck_beat: impl Fn(f64) -> f64,
        old_lock: bool,
    ) -> (ClockEngine, Vec<ClockEvent>, f64) {
        let mut c = ClockEngine::new(bpm, 0.0);
        c.set_outputs(
            vec![out("DrumPad", OutputRole::Steps { channel: 9 }, 0.0)],
            0.0,
        );
        let t0 = c.start(0.1);
        let dt = 1.0 / 60.0;
        let mut ev = Vec::new();
        let mut t = 0.0;
        while t < seconds {
            t += dt;
            ev.extend(run(&mut c, t));
            if let Some(cb) = c.beat_at(t) {
                let err = deck_beat(t) - cb;
                let ticks = if old_lock {
                    let mut e = err;
                    e -= libm::round(e / 4.0) * 4.0;
                    -e * PPQN as f64 * 0.15
                } else {
                    phase_correction(err, dt, playing)
                };
                // the old rule's own threshold; the new one has its deadband inside
                if !old_lock || ticks.abs() > 0.02 {
                    c.align(ticks);
                }
            }
        }
        let ons = ev.into_iter().filter(|e| e.bytes[0] == 0x99).collect();
        (c, ons, t0)
    }

    fn kick_gaps(ons: &[ClockEvent]) -> Vec<f64> {
        let kicks: Vec<f64> = ons
            .iter()
            .filter(|e| e.bytes[1] == 36)
            .map(|e| e.time)
            .collect();
        kicks.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// A field report: PD705 steps at 108 BPM, master locked to deck A, deck A stopped: a
    /// rush-drag-rush-drag feel and `skipped` in the thousands. The stopped deck's frozen beat
    /// read as a growing phase error that the per-frame lock chased at ±15 ticks/s, flipping
    /// direction each time the error wrapped round the bar.
    #[test]
    fn pd705_steps_at_108_stay_on_the_grid_with_the_master_deck_stopped() {
        let beat = 60.0 / 108.0;
        // the old lock: the kicks wander far off the beat, both ways
        let (_, ons, _) = host(108.0, 20.0, false, |_| 0.0, true);
        let gaps = kick_gaps(&ons);
        let (lo, hi) = gaps
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), g| (a.min(*g), b.max(*g)));
        assert!(
            lo < beat * 0.85 && hi > beat * 1.15,
            "the old lock should rush and drag: {lo:.3}..{hi:.3} s vs {beat:.3}"
        );
        // the new one leaves a clock alone while the master deck is stopped
        let (c, ons, t0) = host(108.0, 20.0, false, |_| 0.0, false);
        let gaps = kick_gaps(&ons);
        assert!(gaps.len() > 30);
        for g in &gaps {
            assert!((g - beat).abs() < 1e-9, "kick gap {g} vs {beat}");
        }
        // every sixteenth with a hit lands on the 16th grid from Start
        let step = beat / 4.0;
        for e in &ons {
            let k = (e.time - t0) / step;
            assert!((k - libm::round(k)).abs() < 1e-6, "note at step {k}");
        }
        assert_eq!(c.skipped, 0, "nothing that carried a note was skipped");
    }

    /// A steps output wakes the host only for its notes: the empty ticks between are passed
    /// over, which is not skipping (it counted ~11 per hat before, `skipped=2845` in minutes).
    #[test]
    fn passing_over_empty_ticks_is_not_skipping_but_a_stall_is() {
        let mut c = ClockEngine::new(108.0, 0.0);
        c.set_outputs(
            vec![out("DrumPad", OutputRole::Steps { channel: 9 }, 0.0)],
            0.0,
        );
        c.start(0.0);
        run(&mut c, 10.0);
        assert_eq!(c.skipped, 0);
        // a real stall: a bar of nothing, then a wake far too late
        let bar = 4.0 * 60.0 / 108.0;
        let ev = c.pop_due(10.0 + bar);
        // only the newest overdue tick plays; the bar's hits in between are lost and counted
        assert!(ev.iter().filter(|e| e.bytes[0] == 0x99).count() <= 3);
        // 8 hat steps in the bar: the last plays, 7 are lost
        assert_eq!(c.skipped, 7);
    }

    #[test]
    fn the_lock_pulls_a_playing_deck_in_gently_and_whatever_the_frame_rate() {
        // the deck runs a quarter beat ahead of the clock
        let deck = |t: f64| (t - 0.1) * 108.0 / 60.0 + 0.25;
        let (c, ons, _) = host(108.0, 12.0, true, deck, false);
        let err = deck(12.0) - c.beat_at(12.0).unwrap();
        assert!(err.abs() < 0.01, "still {err} beats off");
        // never faster than the slew: a kick gap moves under 5% at 108
        let beat = 60.0 / 108.0;
        for g in kick_gaps(&ons) {
            assert!((g - beat).abs() / beat < 0.05, "gap {g}");
        }
        for dt in [1.0 / 30.0, 1.0 / 60.0, 1.0 / 144.0] {
            let k = phase_correction(-0.5, dt, true);
            assert!(k > 0.0 && k <= MAX_SLEW_TICKS_PER_S * dt + 1e-12, "{dt}: {k}");
        }
        assert_eq!(phase_correction(-0.5, 1.0 / 60.0, false), 0.0);
        assert_eq!(phase_correction(0.0004, 1.0 / 60.0, true), 0.0, "deadband");
        // the nearest bar line wins: 3.9 beats ahead is 0.1 behind
        assert!(phase_correction(3.9, 1.0 / 60.0, true) > 0.0);
    }
}
