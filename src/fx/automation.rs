//! beats' macro animation for the master chain, ported as a state machine that runs on the
//! deck's transport clock (`FxAutomation.swift`; lib/macros/effects.js `fxSweep`, `fxHold`,
//! `runCompound`, `clearLocalMacroState`; lib/macros/runtime.js `executeMacro`, `feelSnap`,
//! `feelSweep`, `feelGenreReset`, `restoreFeelState`, `cancelFeelAnim`; lib/feel/axes.js
//! `applyFeelGrid`; lib/ui/controllers.js `applyFeel`, `setFxByKey`).
//!
//! Timing is the one beats uses when it renders offline (`sched.js audioAnimLoop` with an
//! OfflineAudioContext): every animation dispatches its slider on a fixed audio-clock grid from
//! the moment it was fired, 120 ms for the FX sweeps and holds, 200 ms for the feel sweeps and
//! the reset, plus once at its end. All of it is in transport seconds: it stops while the deck
//! is paused, as Tone.Transport does. Events at the same time run in the order they were
//! scheduled.
//!
//! A dispatch goes through the slider (`slider.value = Math.round(v)`, clamped to its range)
//! and then to the engine. beats coalesces the engine calls (one flush 33 ms later, latest
//! value per slider); here every dispatch of one instant is flushed together at that instant,
//! in the order the sliders were first touched in it.
//!
//! Only the sliders that reach wave audio are kept (see [`super::master_macro`]). Feel tempo
//! writes go out as [`FxAutomationEvent::Tempo`] (the owner applies them with
//! `WaveRunner.setTempo`, as the tempo macros do).
//!
//! Real-time: everything is preallocated in [`FxAutomation::new`]; `fire`, `set_slider`,
//! `panic`, `advance` and the callbacks they make allocate nothing. Checked against beats' real
//! code event for event by `tests/fx_macros.rs` (`macro-fx.json`).

use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, sin};

use super::master_macro::{Kind, MasterFxKey, MasterMacro};
use crate::json::js_round;

/// What the automation tells its owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FxAutomationEvent {
    /// `applyFx(key, value)` at transport time `time`. `animating` is whether a sweep or hold
    /// was running on the key at that instant (`isAnimating(key)` as the owner's handler reads
    /// it inline in Swift): the FX unit's combination rule needs it at the instant, not at the
    /// end of the block the automation ran through.
    Fx {
        time: f64,
        key: MasterFxKey,
        value: i32,
        animating: bool,
    },
    /// `setTempo(bpm)` (already clamped to 20...300) at transport time `time`.
    Tempo { time: f64, bpm: f64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FeelKind {
    None,
    Snap,
    Sweep,
    Reset,
}

pub struct FxAutomation {
    // MARK: state that beats keeps on the element
    /// The sliders (`parseInt(slider.value)`).
    sliders: Vec<i32>,
    /// `el._tempo`. The owner keeps it in step with the runner before each block.
    pub tempo: f64,
    /// The current genre's BPM (`el._genreData[genre].bpm`): Feel's tempo axis and Reset's target.
    pub genre_bpm: f64,
    /// `el._feelState.puck`.
    puck_x: f64,
    puck_y: f64,
    /// Transport time of the instant being run (the time of the last `advance`).
    now: f64,
    /// `schedAudio` order: events at equal times run in this order.
    seq: i64,

    // the pending flush (build.js `_fxPending`)
    pending_value: Vec<i32>,
    pending_set: Vec<bool>,
    pending_order: Vec<usize>,
    pending_count: usize,

    // MARK: animations (one token per slider: `el._fxAnim[fxKey]`)
    anim_active: Vec<bool>,
    anim_hold: Vec<bool>,
    anim_macro: Vec<u8>,
    anim_start: Vec<f64>,
    anim_to: Vec<f64>,
    anim_t0: Vec<f64>,
    anim_dur: Vec<f64>,
    anim_ramp_down: Vec<f64>,
    anim_sustain: Vec<f64>,
    anim_tail: Vec<f64>,
    anim_grid: Vec<usize>, // dispatches before the final one
    anim_next: Vec<usize>, // index of the next event (anim_grid = the final one)
    anim_seq: Vec<i64>,    // seq of event 0

    // MARK: the Feel animation (`el._feelAnim`): one at a time
    feel_kind: FeelKind,
    feel_macro: u8,
    feel_t0: f64,
    feel_dur: f64,
    feel_grid: usize,
    feel_next: usize,
    feel_seq: i64,
    /// The pending restore (snap: at the end; sweep: 60 ms after it), or none.
    feel_restore_at: f64,
    feel_restore_seq: i64,
    // sweep
    feel_from_x: f64,
    feel_from_y: f64,
    feel_to_x: f64,
    feel_to_y: f64,
    // snapshot (snap / sweep): `snapshotFeelState`
    snap_puck_x: f64,
    snap_puck_y: f64,
    snap_tempo: f64,
    snap_fx: Vec<i32>,
    // reset: `startFx`, `startBpm`, `targetBpm`
    reset_fx: Vec<i32>,
    reset_start_bpm: f64,
    reset_target_bpm: f64,
    reset_bpm_changes: bool,

    // MARK: compound sub-fires (`el._compoundTimers`)
    compound_at: Vec<f64>,
    compound_seq: Vec<i64>,
    compound_sub_ms: Vec<f64>,
    compound_macro: Vec<u8>,
    compound_count: usize,

    // MARK: the catalog, flattened (no array is built on the audio thread)
    op_count: Vec<usize>,
    op_key: Vec<usize>,
    op_to: Vec<f64>,
    hold_tail: Vec<f64>,
    kind_code: Vec<u8>, // 0 sweep, 1 hold, 2 compound, 3 snap, 4 feel sweep, 5 reset
    target_x: Vec<f64>,
    target_y: Vec<f64>,
    default_bars: Vec<i32>,
    // MARK: latched (permanent) macros: what each put back on release
    latch_on: Vec<bool>,
    latch_prior: Vec<i32>,
    latch_puck: Vec<(f64, f64)>,
    latch_bpm: Vec<f64>,
}

impl FxAutomation {
    /// audioAnimLoop's dispatch grids (ms).
    pub const FX_STEP_MS: f64 = 120.0;
    pub const FEEL_STEP_MS: f64 = 200.0;
    /// The Feel snapshot's FX sliders (runtime.js `FEEL_FX_KEYS`) that reach wave audio.
    pub const FEEL_KEYS: [MasterFxKey; 3] = [
        MasterFxKey::Distortion,
        MasterFxKey::CrushBits,
        MasterFxKey::LpFreq,
    ];
    pub const KEY_COUNT: usize = 8;
    pub const MAX_OPS: usize = 3;
    pub const COMPOUND_CAPACITY: usize = 16;

    pub fn new(tempo: f64, genre_bpm: f64) -> FxAutomation {
        let n = Self::KEY_COUNT;
        let c = Self::COMPOUND_CAPACITY;
        let m = MasterMacro::ALL.len();
        let mut a = FxAutomation {
            sliders: MasterFxKey::ALL.iter().map(|k| k.default_value()).collect(),
            tempo,
            genre_bpm,
            puck_x: 0.5,
            puck_y: 0.5,
            now: 0.0,
            seq: 0,
            pending_value: vec![0; n],
            pending_set: vec![false; n],
            pending_order: vec![0; n],
            pending_count: 0,
            anim_active: vec![false; n],
            anim_hold: vec![false; n],
            anim_macro: vec![0; n],
            anim_start: vec![0.0; n],
            anim_to: vec![0.0; n],
            anim_t0: vec![0.0; n],
            anim_dur: vec![0.0; n],
            anim_ramp_down: vec![0.0; n],
            anim_sustain: vec![0.0; n],
            anim_tail: vec![0.0; n],
            anim_grid: vec![0; n],
            anim_next: vec![0; n],
            anim_seq: vec![0; n],
            feel_kind: FeelKind::None,
            feel_macro: 0,
            feel_t0: 0.0,
            feel_dur: 0.0,
            feel_grid: 0,
            feel_next: 0,
            feel_seq: 0,
            feel_restore_at: f64::INFINITY,
            feel_restore_seq: 0,
            feel_from_x: 0.0,
            feel_from_y: 0.0,
            feel_to_x: 0.0,
            feel_to_y: 0.0,
            snap_puck_x: 0.5,
            snap_puck_y: 0.5,
            snap_tempo: 120.0,
            snap_fx: vec![0; n],
            reset_fx: vec![0; n],
            reset_start_bpm: 120.0,
            reset_target_bpm: 120.0,
            reset_bpm_changes: false,
            compound_at: vec![0.0; c],
            compound_seq: vec![0; c],
            compound_sub_ms: vec![0.0; c],
            compound_macro: vec![0; c],
            compound_count: 0,
            op_count: vec![0; m],
            op_key: vec![0; m * Self::MAX_OPS],
            op_to: vec![0.0; m * Self::MAX_OPS],
            hold_tail: vec![0.6; m],
            kind_code: vec![0; m],
            target_x: vec![0.0; m],
            target_y: vec![0.0; m],
            default_bars: vec![1; m],
            latch_on: vec![false; m],
            latch_prior: vec![0; m * Self::MAX_OPS],
            latch_puck: vec![(0.5, 0.5); m],
            latch_bpm: vec![0.0; m],
        };
        for mac in MasterMacro::ALL {
            let i = mac.raw() as usize;
            a.default_bars[i] = mac.default_duration();
            match mac.kind() {
                Kind::Sweep(ops) => {
                    a.kind_code[i] = 0;
                    a.put_ops(i, ops);
                }
                Kind::Hold(ops, tail) => {
                    a.kind_code[i] = 1;
                    a.put_ops(i, ops);
                    a.hold_tail[i] = tail;
                }
                Kind::Compound => a.kind_code[i] = 2,
                Kind::FeelSnap { x, y } => {
                    a.kind_code[i] = 3;
                    a.target_x[i] = x;
                    a.target_y[i] = y;
                }
                Kind::FeelSweep { x, y } => {
                    a.kind_code[i] = 4;
                    a.target_x[i] = x;
                    a.target_y[i] = y;
                }
                Kind::GenreReset => a.kind_code[i] = 5,
            }
        }
        a
    }

    fn put_ops(&mut self, i: usize, ops: &[super::master_macro::Op]) {
        assert!(ops.len() <= Self::MAX_OPS);
        self.op_count[i] = ops.len();
        for (j, op) in ops.iter().enumerate() {
            self.op_key[i * Self::MAX_OPS + j] = op.key.index();
            self.op_to[i * Self::MAX_OPS + j] = op.to;
        }
    }

    // MARK: queries

    /// Bits ([`MasterMacro::bit`]) of the macros with an animation, a restore or a sub-fire
    /// pending.
    pub fn active_mask(&self) -> u32 {
        let mut m = 0u32;
        for k in 0..Self::KEY_COUNT {
            if self.anim_active[k] {
                m |= 1 << self.anim_macro[k] as u32;
            }
        }
        if self.feel_kind != FeelKind::None {
            m |= 1 << self.feel_macro as u32;
        }
        for i in 0..self.compound_count {
            m |= 1 << self.compound_macro[i] as u32;
        }
        m
    }

    /// Anything scheduled (an animation, a feel restore, a sub-fire)?
    pub fn is_busy(&self) -> bool {
        self.active_mask() != 0
    }

    pub fn slider(&self, k: MasterFxKey) -> i32 {
        self.sliders[k.index()]
    }

    /// Is slider `k` (`MasterFxKey::index`) under a sweep or hold right now?
    pub fn animating_key(&self, k: usize) -> bool {
        self.anim_active[k]
    }

    pub fn puck(&self) -> (f64, f64) {
        (self.puck_x, self.puck_y)
    }

    pub fn now(&self) -> f64 {
        self.now
    }

    // MARK: the user's steps (at `now`; flushed by the next `advance`)

    /// Move the clock to `t` without running anything (the start of a block). Events before
    /// `t` must have been run by an `advance` already.
    pub fn set_now(&mut self, t: f64) {
        self.now = self.now.max(t);
    }

    /// Where the Feel puck is (beats keeps it in localStorage; it starts at the centre).
    pub fn set_puck(&mut self, x: f64, y: f64) {
        self.puck_x = x;
        self.puck_y = y;
    }

    /// A hand on a slider (`setFxByKey`).
    pub fn set_slider(&mut self, key: MasterFxKey, value: f64) {
        self.set_fx(key.index(), value);
    }

    /// `executeMacro(el, id, { duration })` at `now`; `duration` in bars, 0 = permanent, `None`
    /// = the macro's default. Returns the macro's run time in ticks (`markMacroRunning`).
    pub fn fire(
        &mut self,
        mac: MasterMacro,
        duration: Option<i32>,
        emit: &mut dyn FnMut(FxAutomationEvent),
    ) -> i64 {
        let i = mac.raw() as usize;
        let d = duration.unwrap_or(self.default_bars[i]);
        let permanent = d == 0;
        let effective = if permanent { self.default_bars[i] } else { d };
        let duration_ticks = effective as i64 * 16;
        let ms_per_tick = self.ms_per_bar() / 16.0;
        let duration_ms = duration_ticks as f64 * ms_per_tick;
        match self.kind_code[i] {
            0 | 1 => {
                for j in 0..self.op_count[i] {
                    let k = self.op_key[i * Self::MAX_OPS + j];
                    let to = self.op_to[i * Self::MAX_OPS + j];
                    if permanent {
                        if !self.latch_on[i] {
                            self.latch_prior[i * Self::MAX_OPS + j] = self.sliders[k];
                        }
                        self.set_fx(k, to);
                    } else if self.kind_code[i] == 0 {
                        self.fx_sweep(k, to, duration_ms, mac.raw());
                    } else {
                        let tail = self.hold_tail[i];
                        self.fx_hold(k, to, duration_ms, tail, mac.raw());
                    }
                }
            }
            2 => {
                // vinyl-brake: steps [{tape-stop, 0 ms}, {octave-down, 60 ms}]. runCompound
                // fires only `mute` and `fx-sweep` / `fx-hold` sub-macros: the tape-stop step
                // (a tempo-sweep) does nothing in beats, so here neither. Octave-down runs for
                // ITS default duration (1 bar) and with fxHold's default tail (0.6). The parity
                // path only: on a project prepared with `WaveOptions.turntableBrake` the deck
                // brakes instead ([`super::brake`]).
                self.seq += 1; // the tape-stop step's timer
                self.add_compound(
                    self.now + 60.0 / 1000.0,
                    (1 * 16) as f64 * ms_per_tick,
                    mac.raw(),
                );
            }
            3 => {
                if permanent {
                    if !self.latch_on[i] {
                        self.latch_puck[i] = (self.puck_x, self.puck_y);
                        self.latch_bpm[i] = self.tempo;
                        self.latch_prior[i * Self::MAX_OPS] = self.sliders[MasterFxKey::LpFreq.index()];
                    }
                    self.cancel_feel_anim(emit);
                    let (x, y) = (clamp01(self.target_x[i]), clamp01(self.target_y[i]));
                    self.apply_feel(x, y, emit);
                } else {
                    self.feel_snap(i, duration_ms, emit);
                }
            }
            4 => self.feel_sweep(i, duration_ms, emit),
            _ => self.feel_genre_reset(mac.raw(), duration_ms, emit),
        }
        if permanent && matches!(self.kind_code[i], 0 | 1 | 3) {
            self.latch_on[i] = true;
        }
        // runtime.js: permanent macros budget the pulse (`min(3 bars, 4 s)`), others the duration.
        if permanent {
            let pulse_ms = (3.0 * self.ms_per_bar()).min(4000.0);
            return 1i64.max(libm::ceil(pulse_ms / ms_per_tick) as i64);
        }
        duration_ticks
    }

    /// Is `mac` held on by a permanent fire (a latch)?
    pub fn latched(&self, mac: MasterMacro) -> bool {
        self.latch_on[mac.raw() as usize]
    }

    /// Let go of a latched macro: its sliders (or the Feel puck and tempo) go back to what they
    /// were when it was latched. Nothing when it is not latched.
    pub fn release(&mut self, mac: MasterMacro, emit: &mut dyn FnMut(FxAutomationEvent)) {
        let i = mac.raw() as usize;
        if !self.latch_on[i] {
            return;
        }
        self.latch_on[i] = false;
        match self.kind_code[i] {
            0 | 1 => {
                for j in 0..self.op_count[i] {
                    let k = self.op_key[i * Self::MAX_OPS + j];
                    let v = self.latch_prior[i * Self::MAX_OPS + j] as f64;
                    self.set_fx(k, v);
                }
            }
            3 => {
                let (x, y) = self.latch_puck[i];
                self.apply_feel(x, y, emit);
                let bpm = self.latch_bpm[i];
                if bpm > 0.0 {
                    self.set_tempo(bpm, emit);
                }
                let lp = self.latch_prior[i * Self::MAX_OPS] as f64;
                self.set_fx(MasterFxKey::LpFreq.index(), lp);
            }
            _ => {}
        }
        self.flush_pending(emit);
    }

    /// `clearLocalMacroState` (panic, transport stop): drop the sub-fires, cancel the Feel
    /// animation and run its restore, cancel every slider animation and put its slider back
    /// where it started. (The tempo macros' restore is the runner's.)
    pub fn panic(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.compound_count = 0;
        self.cancel_feel_anim(emit);
        for k in 0..Self::KEY_COUNT {
            if self.anim_active[k] {
                self.anim_active[k] = false;
                let s = self.anim_start[k];
                self.set_fx(k, s);
            }
        }
    }

    // MARK: the clock

    /// Run every event before `end` (transport seconds), one instant at a time: the events of
    /// an instant in schedule order, then the flush. Whatever the steps since the last call
    /// dispatched is flushed first, at `now`, with the events due at `now`.
    pub fn advance(&mut self, end: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        if self.pending_count > 0 {
            let now = self.now;
            self.run_instant(now, emit);
        }
        loop {
            let t = self.next_time();
            if !(t < end) {
                break;
            }
            self.now = t;
            self.run_instant(t, emit);
        }
    }

    /// Flush the steps' dispatches at `now` without running any event (a paused transport).
    pub fn flush(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.flush_pending(emit);
    }

    fn run_instant(&mut self, t: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        loop {
            // the earliest (time, seq) that is due at exactly t
            let mut best = usize::MAX;
            let mut best_seq = i64::MAX;
            let mut source = 0; // 1 anim, 2 feel grid, 3 feel restore, 4 compound
            for k in 0..Self::KEY_COUNT {
                if self.anim_active[k]
                    && self.anim_time(k) == t
                    && self.anim_seq[k] + (self.anim_next[k] as i64) < best_seq
                {
                    best_seq = self.anim_seq[k] + self.anim_next[k] as i64;
                    best = k;
                    source = 1;
                }
            }
            if (self.feel_kind == FeelKind::Sweep || self.feel_kind == FeelKind::Reset)
                && self.feel_next <= self.feel_grid
                && self.feel_time() == t
                && self.feel_seq + (self.feel_next as i64) < best_seq
            {
                best_seq = self.feel_seq + self.feel_next as i64;
                source = 2;
            }
            if self.feel_restore_at == t && self.feel_restore_seq < best_seq {
                best_seq = self.feel_restore_seq;
                source = 3;
            }
            for i in 0..self.compound_count {
                if self.compound_at[i] == t && self.compound_seq[i] < best_seq {
                    best_seq = self.compound_seq[i];
                    best = i;
                    source = 4;
                }
            }
            match source {
                1 => self.run_anim(best),
                2 => self.run_feel_step(emit),
                3 => self.run_feel_restore(emit),
                4 => self.run_compound(best),
                _ => {
                    self.flush_pending(emit);
                    return;
                }
            }
        }
    }

    fn next_time(&self) -> f64 {
        let mut t = f64::INFINITY;
        for k in 0..Self::KEY_COUNT {
            if self.anim_active[k] {
                t = t.min(self.anim_time(k));
            }
        }
        if (self.feel_kind == FeelKind::Sweep || self.feel_kind == FeelKind::Reset)
            && self.feel_next <= self.feel_grid
        {
            t = t.min(self.feel_time());
        }
        t = t.min(self.feel_restore_at);
        for i in 0..self.compound_count {
            t = t.min(self.compound_at[i]);
        }
        t
    }

    fn flush_pending(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        for j in 0..self.pending_count {
            let k = self.pending_order[j];
            self.pending_set[k] = false;
            emit(FxAutomationEvent::Fx {
                time: self.now,
                key: MasterFxKey::from_index(k).unwrap(),
                value: self.pending_value[k],
                animating: self.anim_active[k],
            });
        }
        self.pending_count = 0;
    }

    // MARK: sliders

    /// `setFxValue(slider, v)`: the slider takes `Math.round(v)` (clamped), and its `input`
    /// event queues the slider's value for the flush.
    fn set_fx(&mut self, k: usize, v: f64) {
        let key = MasterFxKey::from_index(k).unwrap();
        let s = key.sanitize(v);
        self.sliders[k] = s;
        if !self.pending_set[k] {
            self.pending_set[k] = true;
            self.pending_order[self.pending_count] = k;
            self.pending_count += 1;
        }
        self.pending_value[k] = s;
    }

    /// `setTempo`: `el._tempo = clamp(20, 300, bpm)` and tell the engine.
    fn set_tempo(&mut self, bpm: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        if !bpm.is_finite() {
            return;
        }
        self.tempo = 20.0f64.max(300.0f64.min(bpm));
        emit(FxAutomationEvent::Tempo {
            time: self.now,
            bpm: self.tempo,
        });
    }

    /// petri-note.js `_msPerBar`.
    fn ms_per_bar(&self) -> f64 {
        (60000.0 / ((if self.tempo != 0.0 { self.tempo } else { 120.0 }) * 4.0)) * 16.0
    }

    // MARK: fxSweep / fxHold (effects.js)

    /// Grid points `0, 120, ...` below `duration_ms` (audioAnimLoop's loop).
    fn grid_count(duration_ms: f64, step: f64) -> usize {
        let mut n = 0;
        let mut e = 0.0;
        while e < duration_ms {
            n += 1;
            e += step;
        }
        n
    }

    fn start_anim(&mut self, k: usize, hold: bool, to: f64, duration_ms: f64, mac: u8) {
        // a re-fire keeps the first fire's start, so a stacked macro never strands its slider
        let start = if self.anim_active[k] {
            self.anim_start[k]
        } else {
            self.sliders[k] as f64
        };
        self.anim_active[k] = true;
        self.anim_hold[k] = hold;
        self.anim_macro[k] = mac;
        self.anim_start[k] = start;
        self.anim_to[k] = to;
        self.anim_t0[k] = self.now;
        self.anim_dur[k] = duration_ms;
        self.anim_next[k] = 0;
        self.anim_grid[k] = Self::grid_count(duration_ms, Self::FX_STEP_MS);
        self.anim_seq[k] = self.seq;
        self.seq += self.anim_grid[k] as i64 + 1;
    }

    fn fx_sweep(&mut self, k: usize, to: f64, duration_ms: f64, mac: u8) {
        self.start_anim(k, false, to, duration_ms, mac);
        self.anim_ramp_down[k] = duration_ms * 0.8;
    }

    fn fx_hold(&mut self, k: usize, to: f64, duration_ms: f64, tail_frac: f64, mac: u8) {
        self.start_anim(k, true, to, duration_ms, mac);
        let tail = 50.0f64.max(duration_ms * tail_frac);
        self.anim_tail[k] = tail;
        self.anim_sustain[k] = 0.0f64.max(duration_ms - tail);
        self.set_fx(k, to);
    }

    /// Time of slider `k`'s next dispatch: `now + elapsed / 1000` from its fire.
    fn anim_time(&self, k: usize) -> f64 {
        let i = self.anim_next[k];
        let elapsed = if i < self.anim_grid[k] {
            i as f64 * Self::FX_STEP_MS
        } else {
            self.anim_dur[k]
        };
        self.anim_t0[k] + elapsed / 1000.0
    }

    /// One `applyAt(elapsed)` of slider `k`'s animation.
    fn run_anim(&mut self, k: usize) {
        let i = self.anim_next[k];
        let elapsed = if i < self.anim_grid[k] {
            i as f64 * Self::FX_STEP_MS
        } else {
            self.anim_dur[k]
        };
        self.anim_next[k] = i + 1;
        let start = self.anim_start[k];
        let to = self.anim_to[k];
        let dur = self.anim_dur[k];
        if self.anim_hold[k] {
            if elapsed < self.anim_sustain[k] {
                if i >= self.anim_grid[k] {
                    self.anim_active[k] = false;
                }
                return;
            }
            if elapsed >= dur {
                self.set_fx(k, start);
                self.anim_active[k] = false;
                return;
            }
            let t = (elapsed - self.anim_sustain[k]) / self.anim_tail[k];
            self.set_fx(k, to + (start - to) * t);
        } else {
            let done = elapsed >= dur;
            let ramp_down = self.anim_ramp_down[k];
            let v = if elapsed < ramp_down {
                start + (to - start) * (elapsed / ramp_down)
            } else if elapsed < dur {
                to + (start - to) * ((elapsed - ramp_down) / (dur - ramp_down))
            } else {
                start
            };
            self.set_fx(k, v);
            if done {
                self.anim_active[k] = false;
            }
        }
        if i >= self.anim_grid[k] {
            self.anim_active[k] = false;
        }
    }

    // MARK: compound (vinyl-brake)

    fn add_compound(&mut self, at: f64, sub_ms: f64, mac: u8) {
        let s = self.seq;
        self.seq += 1;
        if self.compound_count >= Self::COMPOUND_CAPACITY {
            return;
        }
        let c = self.compound_count;
        self.compound_at[c] = at;
        self.compound_seq[c] = s;
        self.compound_sub_ms[c] = sub_ms;
        self.compound_macro[c] = mac;
        self.compound_count += 1;
    }

    fn run_compound(&mut self, i: usize) {
        let sub_ms = self.compound_sub_ms[i];
        let mac = self.compound_macro[i];
        self.compound_count -= 1;
        for j in i..self.compound_count {
            self.compound_at[j] = self.compound_at[j + 1];
            self.compound_seq[j] = self.compound_seq[j + 1];
            self.compound_sub_ms[j] = self.compound_sub_ms[j + 1];
            self.compound_macro[j] = self.compound_macro[j + 1];
        }
        self.fx_hold(MasterFxKey::MasterPitch.index(), -12.0, sub_ms, 0.6, mac);
    }

    // MARK: Feel (runtime.js, feel/axes.js)

    /// `applyFeel(el, puck)` -> `applyFeelGrid`: tempo = round(genre BPM x the blended
    /// multiplier) clamped to 40...300; LP slider = round(the blended LP).
    fn apply_feel(&mut self, x0: f64, y0: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        let x = 0.0f64.max(1.0f64.min(if x0.is_finite() { x0 } else { 0.5 }));
        let y = 0.0f64.max(1.0f64.min(if y0.is_finite() { y0 } else { 0.5 }));
        self.puck_x = x;
        self.puck_y = y;
        let w0 = (1.0 - x) * (1.0 - y);
        let w1 = x * (1.0 - y);
        let w2 = (1.0 - x) * y;
        let w3 = x * y;
        let bpm_mult = 0.6 * w0 + 0.6 * w1 + 1.4 * w2 + 1.4 * w3;
        let lp = 50.0 * w0 + 100.0 * w1 + 50.0 * w2 + 100.0 * w3;
        let base = if self.genre_bpm != 0.0 {
            self.genre_bpm
        } else if self.tempo != 0.0 {
            self.tempo
        } else {
            120.0
        };
        let bpm = js_round(base * bpm_mult);
        self.set_tempo(40.0f64.max(300.0f64.min(bpm)), emit);
        self.set_fx(MasterFxKey::LpFreq.index(), js_round(lp));
    }

    fn snapshot(&mut self) {
        self.snap_puck_x = self.puck_x;
        self.snap_puck_y = self.puck_y;
        self.snap_tempo = if self.tempo != 0.0 { self.tempo } else { 120.0 };
        for k in Self::FEEL_KEYS {
            self.snap_fx[k.index()] = self.sliders[k.index()];
        }
    }

    /// `restoreFeelState` (its reverb flush is dropped: no reverb here).
    fn restore_snapshot(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.apply_feel(self.snap_puck_x, self.snap_puck_y, emit);
        self.set_tempo(self.snap_tempo, emit);
        for k in Self::FEEL_KEYS {
            self.set_fx(k.index(), self.snap_fx[k.index()] as f64);
        }
    }

    /// `cancelFeelAnim`: stop the running Feel animation and land on what it would have
    /// landed on.
    fn cancel_feel_anim(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        let kind = self.feel_kind;
        if kind == FeelKind::None {
            return;
        }
        self.feel_kind = FeelKind::None;
        self.feel_restore_at = f64::INFINITY;
        if kind == FeelKind::Reset {
            self.finish_reset(emit);
        } else {
            self.restore_snapshot(emit);
        }
    }

    fn feel_snap(&mut self, i: usize, duration_ms: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.cancel_feel_anim(emit);
        self.snapshot();
        self.feel_kind = FeelKind::Snap;
        self.feel_macro = i as u8;
        let (x, y) = (clamp01(self.target_x[i]), clamp01(self.target_y[i]));
        self.apply_feel(x, y, emit);
        self.feel_restore_at = self.now + duration_ms / 1000.0;
        self.feel_restore_seq = self.seq;
        self.seq += 1;
    }

    fn feel_sweep(&mut self, i: usize, duration_ms: f64, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.cancel_feel_anim(emit);
        self.snapshot();
        self.feel_from_x = self.snap_puck_x;
        self.feel_from_y = self.snap_puck_y;
        self.feel_to_x = clamp01(self.target_x[i]);
        self.feel_to_y = clamp01(self.target_y[i]);
        self.feel_kind = FeelKind::Sweep;
        self.feel_macro = i as u8;
        self.start_feel_grid(duration_ms);
    }

    fn start_feel_grid(&mut self, duration_ms: f64) {
        self.feel_t0 = self.now;
        self.feel_dur = duration_ms;
        self.feel_next = 0;
        self.feel_grid = Self::grid_count(duration_ms, Self::FEEL_STEP_MS);
        self.feel_seq = self.seq;
        self.seq += self.feel_grid as i64 + 1;
    }

    fn feel_time(&self) -> f64 {
        let elapsed = if self.feel_next < self.feel_grid {
            self.feel_next as f64 * Self::FEEL_STEP_MS
        } else {
            self.feel_dur
        };
        self.feel_t0 + elapsed / 1000.0
    }

    fn run_feel_step(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        let i = self.feel_next;
        let elapsed = if i < self.feel_grid {
            i as f64 * Self::FEEL_STEP_MS
        } else {
            self.feel_dur
        };
        self.feel_next = i + 1;
        let t = elapsed / self.feel_dur;
        if self.feel_kind == FeelKind::Sweep {
            if t >= 1.0 {
                let (x, y) = (self.feel_to_x, self.feel_to_y);
                self.apply_feel(x, y, emit);
                self.feel_restore_at = self.now + 60.0 / 1000.0;
                self.feel_restore_seq = self.seq;
                self.seq += 1;
                return;
            }
            let e = 0.5 - 0.5 * cos(t * core::f64::consts::PI);
            let x = self.feel_from_x + (self.feel_to_x - self.feel_from_x) * e;
            let y = self.feel_from_y + (self.feel_to_y - self.feel_from_y) * e;
            self.apply_feel(x, y, emit);
        } else {
            if t >= 1.0 {
                self.feel_kind = FeelKind::None;
                self.finish_reset(emit);
                return;
            }
            let e = sin(t * core::f64::consts::PI / 2.0);
            self.lerp_reset(e);
            if self.reset_bpm_changes {
                let b = js_round(
                    self.reset_start_bpm + (self.reset_target_bpm - self.reset_start_bpm) * e,
                );
                self.set_tempo(b, emit);
            }
        }
    }

    fn run_feel_restore(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.feel_restore_at = f64::INFINITY;
        self.feel_kind = FeelKind::None;
        self.restore_snapshot(emit);
    }

    fn lerp_reset(&mut self, e: f64) {
        for key in MasterFxKey::ALL {
            let k = key.index();
            let start = self.reset_fx[k];
            let def = key.default_value();
            if start == def {
                continue;
            }
            self.set_fx(k, js_round(start as f64 + (def - start) as f64 * e));
        }
    }

    fn finish_reset(&mut self, emit: &mut dyn FnMut(FxAutomationEvent)) {
        self.lerp_reset(1.0);
        if self.reset_bpm_changes {
            self.set_tempo(js_round(self.reset_target_bpm), emit);
        }
    }

    fn feel_genre_reset(
        &mut self,
        mac: u8,
        duration_ms: f64,
        emit: &mut dyn FnMut(FxAutomationEvent),
    ) {
        self.cancel_feel_anim(emit);
        for k in 0..Self::KEY_COUNT {
            self.reset_fx[k] = self.sliders[k];
        }
        self.reset_start_bpm = if self.tempo != 0.0 { self.tempo } else { 120.0 };
        self.reset_target_bpm = if self.genre_bpm != 0.0 {
            self.genre_bpm
        } else {
            120.0
        };
        self.reset_bpm_changes = (self.reset_start_bpm - self.reset_target_bpm).abs() > 1.0;
        self.feel_kind = FeelKind::Reset;
        self.feel_macro = mac;
        self.start_feel_grid(duration_ms);
    }
}

fn clamp01(v: f64) -> f64 {
    0.0f64.max(1.0f64.min(v))
}

#[cfg(test)]
mod latch_tests {
    use super::*;

    #[test]
    fn a_permanent_macro_holds_and_release_puts_the_sliders_back() {
        let mut a = FxAutomation::new(120.0, 120.0);
        let mut ev = |_e: FxAutomationEvent| {};
        let before = a.slider(MasterFxKey::CrushBits);
        a.fire(MasterMacro::BuildCrush, Some(0), &mut ev);
        assert!(a.latched(MasterMacro::BuildCrush));
        assert_ne!(a.slider(MasterFxKey::CrushBits), before, "it jumped to its value and stays");
        a.release(MasterMacro::BuildCrush, &mut ev);
        assert!(!a.latched(MasterMacro::BuildCrush));
        assert_eq!(a.slider(MasterFxKey::CrushBits), before);
        assert_eq!(a.slider(MasterFxKey::Distortion), MasterFxKey::Distortion.default_value());
    }

    #[test]
    fn a_latched_feel_snap_gives_back_the_puck_when_released() {
        let mut a = FxAutomation::new(120.0, 120.0);
        let mut ev = |_e: FxAutomationEvent| {};
        let (x, y) = a.puck();
        a.fire(MasterMacro::FeelDrive, Some(0), &mut ev);
        assert!(a.latched(MasterMacro::FeelDrive));
        a.release(MasterMacro::FeelDrive, &mut ev);
        assert!(!a.latched(MasterMacro::FeelDrive));
        assert_eq!(a.puck(), (x, y));
    }
}
