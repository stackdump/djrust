//! The FX unit: hands-on, continuous control of a target's master chain and, on a net deck, of
//! its room and echo sends (`Audio/FxUnit.swift`, the part with no AVFoundation).
//!
//! A target is deck "A", deck "B" or the loop channel "L"; each has its own settings. The poke
//! is the value (`DeckAction.fx`): a setting is configuration, so a replay that sets the same
//! values renders the same audio. The boundary is immediate: a move is smoothed on the audio
//! thread over ~30 ms, a deliberate exception to bar quantise for continuous controls. Reset
//! and kill can wait for the next beat; a knob moved while one waits makes it land at once.
//!
//! How it combines with the master macros ([`super::automation`], beats' sliders): the macros
//! keep running on their own sliders and the chain hears, per key, the more processed of the
//! two: HP the higher cutoff, LP the lower, phaser mix / crush / drive the larger; pitch adds
//! (+-24); phaser depth and rate are the macro's while it moves them, else the unit's. A unit at
//! neutral leaves every macro exactly as it was.
//!
//! Not here: `DeckAction.fx` / `fxReset` / `fxKill` (`FxUnitSettings.actions`), which belong to
//! the session codec, and the Room / Echo send *application*, which lives in the runner.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libm::{exp, log10};

use super::automation::FxAutomation;
use super::master_chain::MasterParams;
use super::master_macro::MasterFxKey;
use crate::json::{JsonObject, JsonValue};

/// One control of the FX unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FxParam {
    Filter,
    Hp,
    Lp,
    PhaserMix,
    PhaserDepth,
    PhaserRate,
    Crush,
    Drive,
    Pitch,
    Room,
    Echo,
    Feedback,
}

impl FxParam {
    pub const ALL: [FxParam; 12] = [
        FxParam::Filter,
        FxParam::Hp,
        FxParam::Lp,
        FxParam::PhaserMix,
        FxParam::PhaserDepth,
        FxParam::PhaserRate,
        FxParam::Crush,
        FxParam::Drive,
        FxParam::Pitch,
        FxParam::Room,
        FxParam::Echo,
        FxParam::Feedback,
    ];
    pub const COUNT: usize = 12;

    pub fn id(self) -> &'static str {
        match self {
            FxParam::Filter => "filter",
            FxParam::Hp => "hp",
            FxParam::Lp => "lp",
            FxParam::PhaserMix => "phaser-mix",
            FxParam::PhaserDepth => "phaser-depth",
            FxParam::PhaserRate => "phaser-rate",
            FxParam::Crush => "crush",
            FxParam::Drive => "drive",
            FxParam::Pitch => "pitch",
            FxParam::Room => "room",
            FxParam::Echo => "echo",
            FxParam::Feedback => "feedback",
        }
    }

    pub fn from_id(id: &str) -> Option<FxParam> {
        Self::ALL.iter().copied().find(|p| p.id() == id)
    }

    /// Its place in `FxUnitSettings.values` (and the audio thread's arrays).
    pub fn index(self) -> usize {
        self as usize
    }

    /// Its travel: -1...1 for the bipolar filter, -12...12 semitones for pitch, else 0...1.
    pub fn range(self) -> (f64, f64) {
        match self {
            FxParam::Filter => (-1.0, 1.0),
            FxParam::Pitch => (-12.0, 12.0),
            _ => (0.0, 1.0),
        }
    }

    /// Where it does nothing: LP open, the phaser's depth at beats' default (3 octaves), the
    /// rest 0.
    pub fn neutral(self) -> f64 {
        match self {
            FxParam::Lp => 1.0,
            FxParam::PhaserDepth => 0.5,
            _ => 0.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FxParam::Filter => "Filter",
            FxParam::Hp => "HP",
            FxParam::Lp => "LP",
            FxParam::PhaserMix => "Phaser",
            FxParam::PhaserDepth => "Depth",
            FxParam::PhaserRate => "Rate",
            FxParam::Crush => "Crush",
            FxParam::Drive => "Drive",
            FxParam::Pitch => "Pitch",
            FxParam::Room => "Room",
            FxParam::Echo => "Echo",
            FxParam::Feedback => "Feedback",
        }
    }

    /// A send on a net deck's runner (its room and echo), not the master chain.
    pub fn is_send(self) -> bool {
        matches!(self, FxParam::Room | FxParam::Echo | FxParam::Feedback)
    }

    /// The value, inside its travel (NaN: neutral).
    pub fn clamp(self, v: f64) -> f64 {
        if !v.is_finite() {
            return self.neutral();
        }
        let (lo, hi) = self.range();
        lo.max(hi.min(v))
    }

    /// Close enough to call it there (the audio thread snaps onto the target from this near).
    fn epsilon(self) -> f64 {
        let (lo, hi) = self.range();
        (hi - lo) * 1e-5
    }

    /// What it does at `v`, for a label: "LP 1.2 kHz", "HP 310 Hz", "8 bit", "+3.0 st".
    pub fn readout(self, v: f64) -> String {
        fn hz(f: f64) -> String {
            if f >= 1000.0 {
                format!("{:.1} kHz", f / 1000.0)
            } else {
                format!("{:.0} Hz", f)
            }
        }
        fn pct(x: f64) -> String {
            format!("{:.0}%", x * 100.0)
        }
        let v = self.clamp(v);
        match self {
            FxParam::Filter => {
                if v < 0.0 {
                    return format!(
                        "LP {}",
                        hz(100.0f64.max(MasterParams::lp_freq(100.0 * (1.0 + v))))
                    );
                }
                if v > 0.0 {
                    return format!("HP {}", hz(5000.0f64.min(MasterParams::hp_freq(100.0 * v))));
                }
                "off".into()
            }
            FxParam::Hp => {
                if v == 0.0 {
                    "off".into()
                } else {
                    hz(5000.0f64.min(MasterParams::hp_freq(100.0 * v)))
                }
            }
            FxParam::Lp => {
                if v == 1.0 {
                    "off".into()
                } else {
                    hz(100.0f64.max(MasterParams::lp_freq(100.0 * v)))
                }
            }
            FxParam::PhaserMix | FxParam::Drive | FxParam::Feedback => {
                if v == 0.0 {
                    "off".into()
                } else {
                    pct(v)
                }
            }
            FxParam::PhaserDepth => format!("{:.1} oct", 0.5f64.max(v * 6.0)),
            FxParam::PhaserRate => {
                format!("{:.1} Hz", if v == 0.0 { 0.1 } else { 0.1 + v * 9.9 })
            }
            FxParam::Crush => {
                if v == 0.0 {
                    "off".into()
                } else {
                    format!("{:.1} bit", 1.0f64.max(16.0 - v * 15.0))
                }
            }
            FxParam::Pitch => {
                if v == 0.0 {
                    "0 st".into()
                } else {
                    format!("{:+.1} st", v)
                }
            }
            FxParam::Room | FxParam::Echo => {
                if v == 0.0 {
                    "off".into()
                } else {
                    format!("+{:.1} dB", 20.0 * log10(1.0 + 3.0 * v))
                }
            }
        }
    }

    /// The chain's controls, in panel order.
    pub const CHAIN: [FxParam; 9] = [
        FxParam::Filter,
        FxParam::Hp,
        FxParam::Lp,
        FxParam::PhaserMix,
        FxParam::PhaserDepth,
        FxParam::PhaserRate,
        FxParam::Crush,
        FxParam::Drive,
        FxParam::Pitch,
    ];
    /// The net decks' sends.
    pub const SENDS: [FxParam; 3] = [FxParam::Room, FxParam::Echo, FxParam::Feedback];

    /// The most the echo's feedback goes to (feedback 1): repeats that ring for seconds.
    pub const MAX_FEEDBACK: f64 = 0.92;
    /// Room / echo 1 sends 4x the genre's own (+12 dB).
    pub const MAX_SEND_GAIN: f64 = 4.0;
}

/// One target's FX unit: a value per control, and the kill (bypass the unit, keep the knobs).
#[derive(Clone, Debug, PartialEq)]
pub struct FxUnitSettings {
    values: [f64; FxParam::COUNT],
    /// The unit is bypassed (the chain hears its neutral), its values kept for when it is let go.
    pub killed: bool,
}

impl Default for FxUnitSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl FxUnitSettings {
    pub fn new() -> FxUnitSettings {
        let mut values = [0.0; FxParam::COUNT];
        for p in FxParam::ALL {
            values[p.index()] = p.neutral();
        }
        FxUnitSettings {
            values,
            killed: false,
        }
    }

    pub fn get(&self, p: FxParam) -> f64 {
        self.values[p.index()]
    }

    pub fn set(&mut self, p: FxParam, v: f64) {
        self.values[p.index()] = p.clamp(v);
    }

    /// Every control at its neutral (killed or not).
    pub fn is_neutral(&self) -> bool {
        FxParam::ALL.iter().all(|&p| self.get(p) == p.neutral())
    }

    /// Is the unit changing the sound (not neutral, not killed)?
    pub fn is_active(&self) -> bool {
        !self.killed && !self.is_neutral()
    }

    /// The controls away from neutral, in panel order.
    pub fn moved(&self) -> Vec<FxParam> {
        FxParam::ALL
            .iter()
            .copied()
            .filter(|&p| self.get(p) != p.neutral())
            .collect()
    }

    /// The controls that differ, and only them; `killed` only when on.
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for p in self.moved() {
            o.set(p.id(), JsonValue::Number(self.get(p)));
        }
        if self.killed {
            o.set("killed", JsonValue::Bool(true));
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: Option<&JsonValue>) -> FxUnitSettings {
        let mut s = FxUnitSettings::new();
        for p in FxParam::ALL {
            if let Some(x) = v.and_then(|v| v.get(p.id())).and_then(JsonValue::as_f64) {
                s.set(p, x);
            }
        }
        s.killed = v
            .and_then(|v| v.get("killed"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        s
    }
}

/// Main -> audio: what the FX unit's owner takes from its command ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FxUnitCommand {
    /// A control (`FxParam::index`) to `value`, smoothed; `snap`: at once (a restore, a mode
    /// switch).
    Set { param: u8, value: f64, snap: bool },
    /// Every control back to neutral and the kill off, at the block holding owner frame
    /// `frame` (0, or a frame already past: the next block).
    Reset { frame: i64 },
    /// The kill on or off, at the block holding `frame`.
    Kill { on: bool, frame: i64 },
}

/// The FX unit on the audio thread: the controls' targets, their smoothed values, and the chain
/// keys it has asked for. Its owner calls `take` for each command, `begin_block` at the start of
/// every block, and `automated` for every slider dispatch of the macros.
pub struct FxUnitLayer {
    pub sample_rate: f64,
    target: [f64; FxParam::COUNT],
    current: [f64; FxParam::COUNT],
    neutral: [f64; FxParam::COUNT],
    epsilon: [f64; FxParam::COUNT],
    killed: bool,
    /// The macros' last slider value per key (`FxAutomation`'s, as dispatched).
    slider: [f64; FxUnitLayer::KEY_COUNT],
    /// What the chain was last asked for, per key.
    applied: [f64; FxUnitLayer::KEY_COUNT],
    rest_key: [f64; FxUnitLayer::KEY_COUNT],
    // one quantised move waiting for its frame: 0 none, 1 reset, 2 kill on, 3 kill off
    pending_kind: u8,
    pending_frame: i64,
    /// Is every control where it is going (no smoothing in flight)?
    settled: bool,
}

impl FxUnitLayer {
    pub const SMOOTH_SECONDS: f64 = 0.03;
    pub const KEY_COUNT: usize = 8;

    pub fn new(sample_rate: f64) -> FxUnitLayer {
        let mut n = [0.0; FxParam::COUNT];
        let mut e = [0.0; FxParam::COUNT];
        for p in FxParam::ALL {
            n[p.index()] = p.neutral();
            e[p.index()] = p.epsilon();
        }
        let mut rest = [0.0; Self::KEY_COUNT];
        for k in MasterFxKey::ALL {
            rest[k.index()] = k.default_value() as f64;
        }
        FxUnitLayer {
            sample_rate,
            target: n,
            current: n,
            neutral: n,
            epsilon: e,
            killed: false,
            slider: rest,
            applied: rest,
            rest_key: rest,
            pending_kind: 0,
            pending_frame: 0,
            settled: true,
        }
    }

    pub fn settled(&self) -> bool {
        self.settled
    }

    // MARK: commands

    pub fn take(&mut self, c: FxUnitCommand) {
        match c {
            FxUnitCommand::Set { param, value, snap } => {
                let k = param as usize;
                if k >= FxParam::COUNT {
                    return;
                }
                if self.pending_kind != 0 {
                    self.land_pending(); // the hand wins: what waited lands now
                }
                self.target[k] = value;
                if snap {
                    self.current[k] = if self.killed { self.neutral[k] } else { value };
                }
            }
            FxUnitCommand::Reset { frame } => {
                if self.pending_kind != 0 {
                    self.land_pending();
                }
                if frame <= 0 {
                    self.reset();
                } else {
                    self.pending_kind = 1;
                    self.pending_frame = frame;
                }
            }
            FxUnitCommand::Kill { on, frame } => {
                if self.pending_kind != 0 {
                    self.land_pending();
                }
                if frame <= 0 {
                    self.killed = on;
                } else {
                    self.pending_kind = if on { 2 } else { 3 };
                    self.pending_frame = frame;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.target = self.neutral;
        self.killed = false;
    }

    fn land_pending(&mut self) {
        match self.pending_kind {
            1 => self.reset(),
            2 => self.killed = true,
            3 => self.killed = false,
            _ => {}
        }
        self.pending_kind = 0;
    }

    // MARK: the block

    /// The block starting at owner frame `start_frame`: land a quantised move due in it,
    /// smooth the controls one step, and `emit` each chain key whose combined value moved (the
    /// owner applies it at the block's first sample as a ramp over the block).
    pub fn begin_block(
        &mut self,
        frames: i64,
        start_frame: i64,
        automation: &FxAutomation,
        emit: &mut dyn FnMut(MasterFxKey, f64),
    ) {
        if self.pending_kind != 0 && self.pending_frame < start_frame + frames {
            self.land_pending();
        }
        let a = 1.0 - exp(-(frames as f64) / (Self::SMOOTH_SECONDS * self.sample_rate));
        let mut still = true;
        for i in 0..FxParam::COUNT {
            let t = if self.killed {
                self.neutral[i]
            } else {
                self.target[i]
            };
            let mut c = self.current[i];
            if c == t {
                continue;
            }
            c += (t - c) * a;
            if (t - c).abs() <= self.epsilon[i] {
                c = t;
            } else {
                still = false;
            }
            self.current[i] = c;
        }
        self.settled = still;
        for k in 0..Self::KEY_COUNT {
            let key = MasterFxKey::from_index(k).unwrap();
            let v = self.combined(key, automation.animating_key(k));
            if v != self.applied[k] {
                self.applied[k] = v;
                emit(key, v);
            }
        }
    }

    /// A macro's slider dispatch: what the chain should hear for it (the macro on the unit), with
    /// the automation as it is now.
    pub fn automated(&mut self, key: MasterFxKey, value: i32, automation: &FxAutomation) -> f64 {
        self.automated_at(key, value, automation.animating_key(key.index()))
    }

    /// [`automated`](Self::automated) for a dispatch whose animation state was taken at its own
    /// instant (`FxAutomationEvent::Fx::animating`). Swift's handler runs inline, inside
    /// `fx.advance`, so a slider dispatched earlier in a block sees the animation still running
    /// even when it ends later in the same block; a host that collects the events and handles
    /// them after the block passes this, not the automation's state at the block's end.
    pub fn automated_at(&mut self, key: MasterFxKey, value: i32, animating: bool) -> f64 {
        let k = key.index();
        self.slider[k] = value as f64;
        let v = self.combined(key, animating);
        self.applied[k] = v;
        v
    }

    /// The unit's own position for a chain key, in the slider's units.
    fn unit_value(&self, key: MasterFxKey) -> f64 {
        let f = self.current[FxParam::Filter.index()];
        let c = |p: FxParam| self.current[p.index()];
        match key {
            MasterFxKey::HpFreq => 100.0 * c(FxParam::Hp).max(f.max(0.0)),
            MasterFxKey::LpFreq => 100.0 * c(FxParam::Lp).min(1.0 + f.min(0.0)),
            MasterFxKey::PhaserWet => 100.0 * c(FxParam::PhaserMix),
            MasterFxKey::PhaserDepth => 100.0 * c(FxParam::PhaserDepth),
            MasterFxKey::PhaserFreq => 100.0 * c(FxParam::PhaserRate),
            MasterFxKey::CrushBits => 100.0 * c(FxParam::Crush),
            MasterFxKey::Distortion => 100.0 * c(FxParam::Drive),
            MasterFxKey::MasterPitch => c(FxParam::Pitch),
        }
    }

    /// The combination rule (see the top of the file). With the unit at neutral it is the
    /// slider.
    fn combined(&self, key: MasterFxKey, animating: bool) -> f64 {
        let u = self.unit_value(key);
        let s = self.slider[key.index()];
        match key {
            MasterFxKey::HpFreq
            | MasterFxKey::PhaserWet
            | MasterFxKey::CrushBits
            | MasterFxKey::Distortion => u.max(s),
            MasterFxKey::LpFreq => u.min(s),
            MasterFxKey::MasterPitch => (-24.0f64).max(24.0f64.min(u + s)),
            MasterFxKey::PhaserDepth | MasterFxKey::PhaserFreq => {
                if animating || s != self.rest_key[key.index()] {
                    s
                } else {
                    u
                }
            }
        }
    }

    // MARK: sends (net decks)

    /// The room's send gain: 1 = the genre's own, up to [`FxParam::MAX_SEND_GAIN`].
    pub fn room_gain(&self) -> f64 {
        1.0 + (FxParam::MAX_SEND_GAIN - 1.0) * self.current[FxParam::Room.index()]
    }
    pub fn echo_gain(&self) -> f64 {
        1.0 + (FxParam::MAX_SEND_GAIN - 1.0) * self.current[FxParam::Echo.index()]
    }
    /// 0 = the genre's feedback, 1 = [`FxParam::MAX_FEEDBACK`].
    pub fn feedback_boost(&self) -> f64 {
        self.current[FxParam::Feedback.index()]
    }

    /// The smoothed value of a control (tests).
    pub fn value(&self, p: FxParam) -> f64 {
        self.current[p.index()]
    }
}

/// A dragged knob, recorded at a bounded rate: the first move goes at once, then at most one
/// every `interval` per key, the latest value winning; whatever waits goes out when its time
/// comes ([`due`](Self::due), from a poll) or when the drag ends ([`finish`](Self::finish)), so
/// the last value is always the one that lands. Main thread.
#[derive(Clone, Debug)]
pub struct FxUnitCoalescer {
    pub interval: f64,
    sent: BTreeMap<String, f64>,
    waiting: BTreeMap<String, f64>,
}

impl Default for FxUnitCoalescer {
    fn default() -> Self {
        Self::new(Self::INTERVAL)
    }
}

impl FxUnitCoalescer {
    /// <= 30 Hz.
    pub const INTERVAL: f64 = 1.0 / 30.0;

    pub fn new(interval: f64) -> FxUnitCoalescer {
        FxUnitCoalescer {
            interval,
            sent: BTreeMap::new(),
            waiting: BTreeMap::new(),
        }
    }

    /// A move of `key` to `value` at time `now` (seconds): true to send it now, false if it
    /// waits.
    pub fn offer(&mut self, key: &str, value: f64, now: f64) -> bool {
        if let Some(&last) = self.sent.get(key) {
            if now - last < self.interval {
                self.waiting.insert(key.into(), value);
                return false;
            }
        }
        self.sent.insert(key.into(), now);
        self.waiting.remove(key);
        true
    }

    /// The waiting moves whose time has come, to send now (sorted by key).
    pub fn due(&mut self, now: f64) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        let keys: Vec<String> = self.waiting.keys().cloned().collect();
        for k in keys {
            let last = self.sent.get(&k).copied().unwrap_or(-1e9);
            if now - last >= self.interval {
                let v = self.waiting.remove(&k).unwrap();
                self.sent.insert(k.clone(), now);
                out.push((k, v));
            }
        }
        out
    }

    /// The drag on `key` ended: its waiting value, if any, to send now.
    pub fn finish(&mut self, key: &str) -> Option<f64> {
        self.sent.remove(key);
        self.waiting.remove(key)
    }

    pub fn has_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }
}
