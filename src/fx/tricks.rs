//! Performance tricks (beyond Swift; docs/TRICKS.md): the moves an acid-house set throws,
//! each a small insert on a deck's strip, fired from a pad (momentary or latched), the socket
//! or MCP. Pure DSP here; `crate::host::deck::tricks` puts them on the strips and times them.
//!
//! Every trick takes stereo blocks in place and is real-time safe once built (buffers sized
//! for the slowest tempo up front). Timing comes from the host: frames per beat, and the frame
//! a quantised fire lands on.

use alloc::vec;
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};

/// The slowest tempo the buffers are sized for.
pub const MIN_BPM: f64 = 60.0;

/// A trick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrickKind {
    /// Beat repeat: a slice of 1/4 .. 1/32 note looped, rising in pitch.
    Stutter,
    /// Sidechain pump: the deck ducks on each kick of a source deck (or on its own grid).
    Pump,
    /// Dub delay throw: the deck sent into a tempo-synced ping-pong delay while held; the
    /// echoes ring on after.
    Delay,
    /// Reverb wash-out: the deck thrown into a big room while its dry signal fades away.
    Wash,
    /// Reverb freeze: the deck thrown into the room, which then holds near-forever.
    VerbFreeze,
    /// Tape stop: the deck slows to a halt.
    TapeStop,
    /// Spin-back: the deck whipped backwards to a halt.
    SpinBack,
    /// Half time: the deck runs at half its tempo (a net deck's notes keep their pitch).
    HalfTime,
    /// Double time: the deck runs at twice its tempo.
    DoubleTime,
    /// The one-knob resonant filter: ridden by a knob, or on its own to a target.
    Filter,
    /// The TD-3's notes accented (velocity), over MIDI.
    Td3Accent,
    /// The TD-3's notes slid (legato), over MIDI.
    Td3Slide,
    /// The TD-3's cutoff ridden by a CC, for gear that has one.
    Td3Cutoff,
    /// A noise riser for transitions, over a number of bars.
    Riser,
}

impl TrickKind {
    pub const ALL: [TrickKind; 14] = [
        TrickKind::Stutter,
        TrickKind::Pump,
        TrickKind::Delay,
        TrickKind::Wash,
        TrickKind::VerbFreeze,
        TrickKind::TapeStop,
        TrickKind::SpinBack,
        TrickKind::HalfTime,
        TrickKind::DoubleTime,
        TrickKind::Filter,
        TrickKind::Td3Accent,
        TrickKind::Td3Slide,
        TrickKind::Td3Cutoff,
        TrickKind::Riser,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TrickKind::Stutter => "stutter",
            TrickKind::Pump => "pump",
            TrickKind::Delay => "delay",
            TrickKind::Wash => "wash",
            TrickKind::VerbFreeze => "verb_freeze",
            TrickKind::TapeStop => "tape_stop",
            TrickKind::SpinBack => "spin_back",
            TrickKind::HalfTime => "half_time",
            TrickKind::DoubleTime => "double_time",
            TrickKind::Filter => "filter",
            TrickKind::Td3Accent => "td3_accent",
            TrickKind::Td3Slide => "td3_slide",
            TrickKind::Td3Cutoff => "td3_cutoff",
            TrickKind::Riser => "riser",
        }
    }

    pub fn parse(s: &str) -> Option<TrickKind> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace(['-', ' '], "_")
                .as_str()
            {
                "stutter" | "beat_repeat" | "repeat" => TrickKind::Stutter,
                "pump" | "sidechain" | "duck" => TrickKind::Pump,
                "delay" | "dub_delay" | "echo" | "throw" => TrickKind::Delay,
                "wash" | "wash_out" | "washout" | "reverb" => TrickKind::Wash,
                "tape_stop" | "tapestop" | "stop" | "brake" => TrickKind::TapeStop,
                "spin_back" | "spinback" | "rewind" | "backspin" => TrickKind::SpinBack,
                "half_time" | "halftime" | "half" => TrickKind::HalfTime,
                "double_time" | "doubletime" | "double" => TrickKind::DoubleTime,
                "filter" | "filter_ride" | "ride" | "resonant_filter" => TrickKind::Filter,
                "td3_accent" | "accent" => TrickKind::Td3Accent,
                "td3_slide" | "slide" => TrickKind::Td3Slide,
                "td3_cutoff" | "cutoff" => TrickKind::Td3Cutoff,
                "riser" | "noise_riser" | "uplifter" | "sweep" => TrickKind::Riser,
                "verb_freeze" | "verbfreeze" | "reverb_freeze" | "space_freeze" => {
                    TrickKind::VerbFreeze
                }
                _ => return None,
            },
        )
    }

    /// The grid a fire lands on by default, in beats (0: at once).
    pub fn default_quantum(self) -> f64 {
        match self {
            TrickKind::Stutter => 0.25,
            TrickKind::Pump | TrickKind::HalfTime | TrickKind::DoubleTime => 1.0,
            // a ride starts on the bar
            TrickKind::Filter => 4.0,
            // the TD-3's notes change from the next one; a cutoff ride from the bar
            TrickKind::Td3Accent | TrickKind::Td3Slide => 0.0,
            TrickKind::Td3Cutoff => 4.0,
            // from the bar, so it ends on one
            TrickKind::Riser => 4.0,
            // a throw catches what is playing now
            TrickKind::Delay
            | TrickKind::Wash
            | TrickKind::VerbFreeze
            | TrickKind::TapeStop
            | TrickKind::SpinBack => 0.0,
        }
    }

    /// The tempo it puts a deck at (the time tricks), else None.
    pub fn time_scale(self) -> Option<f64> {
        match self {
            TrickKind::HalfTime => Some(0.5),
            TrickKind::DoubleTime => Some(2.0),
            _ => None,
        }
    }

    /// A trick on the gear (MIDI) rather than on a deck's audio.
    pub fn is_gear(self) -> bool {
        matches!(
            self,
            TrickKind::Td3Accent | TrickKind::Td3Slide | TrickKind::Td3Cutoff
        )
    }

    /// What a fire uses when it says nothing.
    pub fn default_args(self) -> TrickArgs {
        let d = TrickArgs::default();
        match self {
            TrickKind::Stutter
            | TrickKind::HalfTime
            | TrickKind::DoubleTime
            | TrickKind::Td3Accent
            | TrickKind::Td3Slide => d,
            // four bars of rising noise, to half level
            TrickKind::Riser => TrickArgs {
                amount: 0.5,
                bars: 4.0,
                ..d
            },
            // a cutoff ride up to open over two bars
            TrickKind::Td3Cutoff => TrickArgs {
                amount: 1.0,
                bars: 2.0,
                ..d
            },
            // a ride down to a low-pass, singing, over a bar
            TrickKind::Filter => TrickArgs {
                amount: -0.8,
                bars: 1.0,
                res: 0.6,
                ..d
            },
            TrickKind::Pump => TrickArgs {
                div: 8.0,
                amount: 0.8,
                ..d
            },
            TrickKind::Delay => TrickArgs {
                beats: 0.75,
                amount: 0.8,
                feedback: 0.6,
                ..d
            },
            // wash: the dry fades out over a beat, the room at full; freeze: half a beat of the
            // deck caught, the room level 0.8
            TrickKind::Wash => TrickArgs {
                beats: 1.0,
                amount: 1.0,
                ..d
            },
            TrickKind::VerbFreeze => TrickArgs {
                beats: 0.5,
                amount: 0.8,
                ..d
            },
            // a beat to stop; half a beat to spin back at up to 3x (4 x 0.75)
            TrickKind::TapeStop => TrickArgs { beats: 1.0, ..d },
            TrickKind::SpinBack => TrickArgs {
                beats: 0.5,
                amount: 0.75,
                ..d
            },
        }
    }
}

/// What a sidechain listens to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrickSource {
    /// The other deck's kick if it plays, else the grid (resolved when fired).
    #[default]
    Auto,
    /// Every beat of the deck's own grid: four on the floor.
    Grid,
    /// The kick on deck `n` (the mixer's order).
    Deck(u8),
}

/// What a fire says, beyond on / off: each trick reads the fields it knows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrickArgs {
    /// Note division (4 = quarter .. 32), for the stutter.
    pub div: f64,
    /// Semitones the pitch rises per repeat (stutter).
    pub rise: f64,
    /// 0..1, the trick's main amount (depth, send, level).
    pub amount: f64,
    /// Length in bars (riser, tape stop in beats ×4 ...), when a trick has one.
    pub bars: f64,
    /// What a sidechain listens to (pump).
    pub source: TrickSource,
    /// A time in beats (the delay's: 0.75 is a dotted eighth).
    pub beats: f64,
    /// Feedback, 0..0.95 (delay).
    pub feedback: f64,
    /// Resonance, 0..1 (filter).
    pub res: f64,
}

impl Default for TrickArgs {
    fn default() -> Self {
        TrickArgs {
            div: 16.0,
            rise: 0.0,
            amount: 1.0,
            bars: 1.0,
            source: TrickSource::Auto,
            beats: 0.75,
            feedback: 0.6,
            res: 0.6,
        }
    }
}

impl TrickArgs {
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("div", JsonValue::from(self.div));
        o.set("rise", JsonValue::from(self.rise));
        o.set("amount", JsonValue::from(self.amount));
        o.set("bars", JsonValue::from(self.bars));
        o.set("beats", JsonValue::from(self.beats));
        o.set("feedback", JsonValue::from(self.feedback));
        o.set("res", JsonValue::from(self.res));
        o.set(
            "source",
            match self.source {
                TrickSource::Auto => "auto".into(),
                TrickSource::Grid => "grid".into(),
                TrickSource::Deck(n) => JsonValue::int(n as i64),
            },
        );
        JsonValue::Object(o)
    }
}

/// A note division snapped to the ones a stutter offers: 4, 8, 16, 32 (and triplets 6, 12, 24).
pub fn snap_div(d: f64) -> f64 {
    const DIVS: [f64; 7] = [4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0];
    let d = if d.is_finite() { d } else { 16.0 };
    let mut best = DIVS[0];
    for x in DIVS {
        if libm::fabs(x - d) < libm::fabs(best - d) {
            best = x;
        }
    }
    best
}

/// Frames of a 1/`div` note at `fpb` frames per beat (a beat is a quarter note).
pub fn div_frames(div: f64, fpb: f64) -> f64 {
    fpb * 4.0 / div
}

// MARK: stutter

/// Beat repeat. On a fire it records one slice of 1/`div` note from the live signal (which plays
/// through while it records), then loops it, each repeat `rise` semitones higher (and so
/// shorter), with a 2 ms edge on each repeat so the loop does not click. Off: back to live
/// over 5 ms.
pub struct Stutter {
    sr: f64,
    l: Vec<f32>,
    r: Vec<f32>,
    /// Frames of the slice.
    len: usize,
    /// Frames recorded so far (< len: recording).
    rec: usize,
    pos: f64,
    rate: f64,
    rise: f64,
    on: bool,
    weight: f32,
    step: f32,
    edge: f64,
    /// Repeats played (the rate rises per repeat).
    pub repeats: u32,
}

impl Stutter {
    /// Holds up to a quarter note at [`MIN_BPM`].
    pub fn new(sr: f64) -> Stutter {
        let cap = (60.0 / MIN_BPM * sr) as usize + 16;
        Stutter {
            sr,
            l: vec![0.0; cap],
            r: vec![0.0; cap],
            len: 0,
            rec: 0,
            pos: 0.0,
            rate: 1.0,
            rise: 0.0,
            on: false,
            weight: 0.0,
            step: (1.0 / (0.005 * sr)).min(1.0) as f32,
            edge: 0.002 * sr,
            repeats: 0,
        }
    }

    /// Start: a slice of 1/`div` note at `fpb`, rising `rise` semitones a repeat.
    pub fn start(&mut self, div: f64, fpb: f64, rise: f64) {
        let len = div_frames(snap_div(div), fpb) as usize;
        self.len = len.clamp(32, self.l.len());
        self.rec = 0;
        self.pos = 0.0;
        self.rate = 1.0;
        self.rise = rise.clamp(-12.0, 12.0);
        self.repeats = 0;
        self.on = true;
    }

    pub fn stop(&mut self) {
        self.on = false;
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Sounding at all (on, or fading back to live).
    pub fn is_active(&self) -> bool {
        self.on || self.weight > 0.0
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            return;
        }
        let len = self.len;
        for i in 0..l.len().min(r.len()) {
            let (dl, dr) = (l[i], r[i]);
            if self.on && self.rec < len {
                // the first pass records and plays live
                self.l[self.rec] = dl;
                self.r[self.rec] = dr;
                self.rec += 1;
                if self.weight < 1.0 {
                    self.weight = 1.0;
                }
                continue;
            }
            let want = if self.on { 1.0 } else { 0.0 };
            if self.weight < want {
                self.weight = (self.weight + self.step).min(want);
            } else if self.weight > want {
                self.weight = (self.weight - self.step).max(want);
            }
            if self.rec < len {
                // stopped while still recording: nothing to repeat
                continue;
            }
            let k = libm::floor(self.pos) as usize;
            let f = (self.pos - k as f64) as f32;
            let k1 = (k + 1).min(len - 1);
            let sl = self.l[k] + (self.l[k1] - self.l[k]) * f;
            let sr = self.r[k] + (self.r[k1] - self.r[k]) * f;
            // a short edge at each end of the repeat
            let from_end = (len as f64 - self.pos) / self.rate;
            let e = (self.pos / self.rate).min(from_end).min(self.edge) / self.edge;
            let g = e as f32;
            let w = self.weight;
            l[i] = dl * (1.0 - w) + sl * g * w;
            r[i] = dr * (1.0 - w) + sr * g * w;
            self.pos += self.rate;
            if self.pos >= len as f64 {
                self.pos -= len as f64;
                self.repeats += 1;
                if self.rise != 0.0 {
                    // up to two octaves, then it holds
                    self.rate = (self.rate * libm::pow(2.0, self.rise / 12.0)).clamp(0.25, 4.0);
                }
            }
        }
    }

    /// The sample rate it was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }
}

// MARK: pump

/// One-pole low-pass coefficient for `hz`.
fn lp_coef(hz: f64, sr: f64) -> f32 {
    (1.0 - libm::exp(-2.0 * core::f64::consts::PI * hz / sr)) as f32
}

/// Sidechain pump. A kick detector (two one-pole low-passes at 120 Hz, a fast envelope against a
/// slow one, a refractory quarter of a beat) on a source signal, or the deck's own grid, triggers
/// a duck: down by `depth` in 2 ms, back over a 1/`div` note on a squared curve, the pump of a
/// compressor keyed from the kick.
pub struct Pump {
    sr: f64,
    on: bool,
    depth: f32,
    release: f64,
    attack: f64,
    /// Frames since the last trigger (f64::MAX: none yet).
    since: f64,
    lp1: f32,
    lp2: f32,
    fast: f32,
    slow: f32,
    c_lp: f32,
    c_fast_rel: f32,
    c_slow: f32,
    refractory: f64,
    /// Grid mode: frames to the next beat, and frames per beat.
    grid: Option<(f64, f64)>,
    /// The gain applied at the last frame (for a view, and so off is smooth).
    gain: f32,
    /// Kicks heard (tests, views).
    pub triggers: u32,
}

impl Pump {
    pub fn new(sr: f64) -> Pump {
        Pump {
            sr,
            on: false,
            depth: 0.8,
            release: 0.25 * sr,
            attack: 0.002 * sr,
            since: f64::MAX,
            lp1: 0.0,
            lp2: 0.0,
            fast: 0.0,
            slow: 0.0,
            c_lp: lp_coef(120.0, sr),
            c_fast_rel: lp_coef(1.0 / 0.03, sr),
            c_slow: lp_coef(1.0 / 0.3, sr),
            refractory: 0.1 * sr,
            grid: None,
            gain: 1.0,
            triggers: 0,
        }
    }

    /// Start: duck by `depth` (0..1), released over a 1/`div` note at `fpb`. `grid`: duck on
    /// every beat from now (the host starts it on a beat) instead of listening.
    pub fn start(&mut self, depth: f64, div: f64, fpb: f64, grid: bool) {
        self.on = true;
        self.depth = depth.clamp(0.0, 1.0) as f32;
        self.release = div_frames(snap_div(div), fpb).max(0.01 * self.sr);
        self.refractory = 0.25 * fpb;
        self.grid = grid.then_some((0.0, fpb));
        self.since = f64::MAX;
    }

    /// The tempo moved (grid mode keeps its phase).
    pub fn set_fpb(&mut self, fpb: f64, div: f64) {
        if let Some((next, _)) = self.grid {
            self.grid = Some((next, fpb));
        }
        self.release = div_frames(snap_div(div), fpb).max(0.01 * self.sr);
        self.refractory = 0.25 * fpb;
    }

    pub fn stop(&mut self) {
        self.on = false;
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn is_active(&self) -> bool {
        self.on || self.gain < 1.0
    }

    /// Listens to the grid rather than a source.
    pub fn on_grid(&self) -> bool {
        self.grid.is_some()
    }

    /// The gain now (1: not ducked).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    fn duck_at(&self) -> f32 {
        if !self.on || self.since == f64::MAX {
            return 0.0;
        }
        let t = self.since;
        if t < self.attack {
            return self.depth * (t / self.attack) as f32;
        }
        let r = (t - self.attack) / self.release;
        if r >= 1.0 {
            return 0.0;
        }
        self.depth * ((1.0 - r) * (1.0 - r)) as f32
    }

    /// Duck `l` / `r`; `side` is the source's mono signal (None in grid mode or with no source).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], side: Option<&[f32]>) {
        if !self.is_active() {
            return;
        }
        let n = l.len().min(r.len());
        for i in 0..n {
            if self.on {
                let trig = match (&mut self.grid, side) {
                    (Some((next, fpb)), _) => {
                        let t = *next <= 0.0;
                        if t {
                            *next += *fpb;
                        }
                        *next -= 1.0;
                        t
                    }
                    (None, Some(s)) => {
                        let x = s.get(i).copied().unwrap_or(0.0);
                        self.lp1 += self.c_lp * (x - self.lp1);
                        self.lp2 += self.c_lp * (self.lp1 - self.lp2);
                        let a = self.lp2.abs();
                        // fast: instant attack, 30 ms release; slow: 300 ms both ways
                        self.fast = if a > self.fast {
                            a
                        } else {
                            self.fast + self.c_fast_rel * (a - self.fast)
                        };
                        self.slow += self.c_slow * (a - self.slow);
                        let rising = self.fast > 1.6 * self.slow + 0.02;
                        rising && self.since >= self.refractory
                    }
                    (None, None) => false,
                };
                if trig {
                    self.since = 0.0;
                    self.triggers += 1;
                } else if self.since != f64::MAX {
                    self.since += 1.0;
                }
            }
            let g = 1.0 - self.duck_at();
            // off: glide back up rather than jump
            self.gain = if self.on {
                g
            } else {
                (self.gain + 0.0005).min(1.0)
            };
            l[i] *= self.gain;
            r[i] *= self.gain;
        }
    }
}

// MARK: dub delay

/// The longest delay, in beats at [`MIN_BPM`].
pub const MAX_DELAY_BEATS: f64 = 2.0;

/// A dub delay throw: a ping-pong delay of `beats` (tempo-synced), the deck sent in by `send`
/// while thrown; the repeats run through a dub tone in the loop (high-pass ~150 Hz, low-pass
/// ~2.8 kHz, soft saturation), so they darken and grit as they fade. Released, the send closes
/// over 10 ms and the echoes ring on. The wet signal is added to the deck.
pub struct DubDelay {
    sr: f64,
    l: Vec<f32>,
    r: Vec<f32>,
    w: usize,
    /// Delay in frames (fractional), chasing `target` so a tempo change glides.
    delay: f64,
    target: f64,
    send: f32,
    send_want: f32,
    send_step: f32,
    feedback: f32,
    hp_l: f32,
    hp_r: f32,
    lp_l: f32,
    lp_r: f32,
    c_hp: f32,
    c_lp: f32,
    /// Frames since anything audible went into the loop: echoes are still on their way
    /// until a whole delay has passed with nothing written.
    idle: f64,
}

impl DubDelay {
    pub fn new(sr: f64) -> DubDelay {
        let cap = (MAX_DELAY_BEATS * 60.0 / MIN_BPM * sr) as usize + 8;
        DubDelay {
            sr,
            l: vec![0.0; cap],
            r: vec![0.0; cap],
            w: 0,
            delay: 0.375 * sr,
            target: 0.375 * sr,
            send: 0.0,
            send_want: 0.0,
            send_step: (1.0 / (0.010 * sr)).min(1.0) as f32,
            feedback: 0.6,
            hp_l: 0.0,
            hp_r: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
            c_hp: lp_coef(150.0, sr),
            c_lp: lp_coef(2_800.0, sr),
            idle: f64::MAX,
        }
    }

    /// Throw: send at `amount`, echoes every `beats` at `fpb`, `feedback` 0..0.95.
    pub fn throw(&mut self, amount: f64, beats: f64, fpb: f64, feedback: f64) {
        self.set_time(beats, fpb);
        if !self.ringing() {
            // a fresh throw starts at the right time, not gliding from the last one
            self.delay = self.target;
        }
        self.send_want = amount.clamp(0.0, 1.0) as f32;
        self.feedback = feedback.clamp(0.0, 0.95) as f32;
    }

    /// Release: the send closes, the echoes ring on.
    pub fn release(&mut self) {
        self.send_want = 0.0;
    }

    /// The tempo moved, or the time did.
    pub fn set_time(&mut self, beats: f64, fpb: f64) {
        let max = (self.l.len() - 4) as f64;
        self.target = (beats.clamp(1.0 / 16.0, MAX_DELAY_BEATS) * fpb).clamp(16.0, max);
    }

    pub fn is_thrown(&self) -> bool {
        self.send_want > 0.0
    }

    /// Thrown, or still ringing.
    pub fn is_active(&self) -> bool {
        self.send_want > 0.0 || self.send > 0.0 || self.ringing()
    }

    /// Echoes still sounding, or still in the loop on their way out.
    pub fn ringing(&self) -> bool {
        self.idle <= self.delay.max(self.target) + 64.0
    }

    #[inline]
    fn read(buf: &[f32], w: usize, d: f64) -> f32 {
        let len = buf.len();
        let p = w as f64 + len as f64 - d;
        let i = libm::floor(p) as usize;
        let f = (p - libm::floor(p)) as f32;
        let a = buf[i % len];
        let b = buf[(i + 1) % len];
        a + (b - a) * f
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            return;
        }
        let len = self.l.len();
        for i in 0..l.len().min(r.len()) {
            if self.send < self.send_want {
                self.send = (self.send + self.send_step).min(self.send_want);
            } else if self.send > self.send_want {
                self.send = (self.send - self.send_step).max(self.send_want);
            }
            self.delay += (self.target - self.delay) * 0.0005;
            let dl = Self::read(&self.l, self.w, self.delay);
            let dr = Self::read(&self.r, self.w, self.delay);
            // the dub tone in the loop
            self.hp_l += self.c_hp * (dl - self.hp_l);
            self.hp_r += self.c_hp * (dr - self.hp_r);
            let (hl, hr) = (dl - self.hp_l, dr - self.hp_r);
            self.lp_l += self.c_lp * (hl - self.lp_l);
            self.lp_r += self.c_lp * (hr - self.lp_r);
            let fb = self.feedback;
            // ping-pong: the mono send enters on the left, each side feeds the other
            let input = 0.5 * (l[i] + r[i]) * self.send;
            let nl = libm::tanhf(input + fb * self.lp_r);
            let nr = libm::tanhf(fb * self.lp_l);
            self.l[self.w] = nl;
            self.r[self.w] = nr;
            self.w = (self.w + 1) % len;
            if nl.abs() > 1e-6 || nr.abs() > 1e-6 {
                self.idle = 0.0;
            } else if self.idle != f64::MAX {
                self.idle += 1.0;
            }
            l[i] += dl;
            r[i] += dr;
        }
        if !self.is_active() {
            // rung out: the loop holds only silence now
            self.idle = f64::MAX;
        }
    }

    /// The sample rate it was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }
}

// MARK: the reverb stand-in and its throws

/// The strip reverb's stand-in (docs/TRICKS.md, PORTING.md open decision 1): Swift's deck reverb
/// is Apple's AUReverb2, which no Rust code can match; this is [`FdnReverb`]
/// (`musicality::space`, the runner's room) with the knobs decision 1 proposes: `size` → decay
/// 0.25 × 32^size s, `tone` → the loop's low-pass 1 kHz .. 16 kHz.
///
/// [`FdnReverb`]: crate::musicality::space::FdnReverb
pub fn stand_in_spec(size: f64, tone: f64) -> crate::musicality::space::SpaceSpec {
    let size = size.clamp(0.0, 1.0);
    let tone = tone.clamp(0.0, 1.0);
    crate::musicality::space::SpaceSpec::new(
        0.25 * libm::pow(32.0, size),
        1.0,
        1_000.0 * libm::pow(16.0, tone),
    )
}

/// The room the throws use: big and bright enough to wash in.
pub const THROW_SIZE: f64 = 0.85;
pub const THROW_TONE: f64 = 0.6;
/// The decay a freeze holds, seconds.
pub const FREEZE_SECONDS: f64 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VerbMode {
    Off,
    Wash,
    Freeze,
}

/// The reverb throws on one deck, on one stand-in room:
/// - **wash**: the deck's send opens to full in 20 ms and its dry signal fades out over `beats`,
///   leaving the room (at `amount`); off brings the dry back in 20 ms, closes the send, and the
///   room decays at its own time.
/// - **freeze**: the send opens for `beats` (catching what plays), then closes while the room's
///   decay is held at [`FREEZE_SECONDS`]: a pad of the moment under the deck, at `amount`; off
///   lets the room decay again.
pub struct VerbThrow {
    sr: f64,
    fdn: crate::musicality::space::FdnReverb,
    mode: VerbMode,
    dry: f32,
    dry_want: f32,
    dry_step: f32,
    wet: f32,
    wet_want: f32,
    /// Frames the freeze still listens for.
    catch_left: usize,
    tail: f32,
    normal_seconds: f64,
    normal_damping: f64,
}

impl VerbThrow {
    pub fn new(sr: f64) -> VerbThrow {
        let mut fdn = crate::musicality::space::FdnReverb::new(sr);
        let spec = stand_in_spec(THROW_SIZE, THROW_TONE);
        fdn.tune(Some(&spec));
        fdn.set_send(0.0, 1);
        VerbThrow {
            sr,
            fdn,
            mode: VerbMode::Off,
            dry: 1.0,
            dry_want: 1.0,
            dry_step: 1.0,
            wet: 0.0,
            wet_want: 0.0,
            catch_left: 0,
            tail: 0.0,
            normal_seconds: spec.seconds,
            normal_damping: spec.damping,
        }
    }

    fn ramp(&self) -> usize {
        (0.020 * self.sr) as usize
    }

    /// Wash out: the dry fades over `beats` at `fpb`, the room at `amount`.
    pub fn wash(&mut self, amount: f64, beats: f64, fpb: f64) {
        self.mode = VerbMode::Wash;
        self.fdn.hold(self.normal_seconds);
        self.fdn.set_damping(Some(self.normal_damping));
        self.fdn.set_send(1.0, self.ramp());
        self.wet_want = amount.clamp(0.0, 1.0) as f32;
        self.dry_want = 0.0;
        let frames = (beats.max(0.0) * fpb).max(self.ramp() as f64);
        self.dry_step = (1.0 / frames) as f32;
    }

    /// Freeze: catch `beats` at `fpb`, then hold the room at `amount`.
    pub fn freeze(&mut self, amount: f64, beats: f64, fpb: f64) {
        self.mode = VerbMode::Freeze;
        self.fdn.hold(self.normal_seconds);
        self.fdn.set_damping(Some(self.normal_damping));
        self.fdn.set_send(1.0, self.ramp());
        self.catch_left = (beats.clamp(0.0625, 4.0) * fpb) as usize;
        self.wet_want = amount.clamp(0.0, 1.0) as f32;
        self.dry_want = 1.0;
        self.dry_step = (1.0 / self.ramp() as f64) as f32;
    }

    /// Let go of either: the dry comes back, the send closes, the room decays.
    pub fn release(&mut self) {
        self.mode = VerbMode::Off;
        self.fdn.hold(self.normal_seconds);
        self.fdn.set_damping(Some(self.normal_damping));
        self.fdn.set_send(0.0, self.ramp());
        self.dry_want = 1.0;
        self.dry_step = (1.0 / self.ramp() as f64) as f32;
        self.catch_left = 0;
    }

    pub fn is_washing(&self) -> bool {
        self.mode == VerbMode::Wash
    }

    pub fn is_frozen(&self) -> bool {
        self.mode == VerbMode::Freeze
    }

    /// Thrown, or the room still sounding.
    pub fn is_active(&self) -> bool {
        self.mode != VerbMode::Off || self.tail > 1e-5 || self.dry < 1.0
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            return;
        }
        let wet_step = (1.0 / self.ramp() as f64) as f32;
        let mut peak = 0f32;
        for i in 0..l.len().min(r.len()) {
            if self.catch_left > 0 {
                self.catch_left -= 1;
                if self.catch_left == 0 {
                    // caught: close the door and hold the room
                    self.fdn.set_send(0.0, self.ramp());
                    self.fdn.hold(FREEZE_SECONDS);
                    self.fdn.set_damping(None);
                }
            }
            if self.dry < self.dry_want {
                self.dry = (self.dry + self.dry_step).min(self.dry_want);
            } else if self.dry > self.dry_want {
                self.dry = (self.dry - self.dry_step).max(self.dry_want);
            }
            // the return stays up while the room rings out after a release
            let ww = if self.mode == VerbMode::Off {
                self.wet
            } else {
                self.wet_want
            };
            if self.wet < ww {
                self.wet = (self.wet + wet_step).min(ww);
            } else if self.wet > ww {
                self.wet = (self.wet - wet_step).max(ww);
            }
            let x = 0.5 * (l[i] + r[i]);
            let (wl, wr) = self.fdn.process(x as f64);
            let (wl, wr) = (wl as f32 * self.wet, wr as f32 * self.wet);
            peak = peak.max(wl.abs()).max(wr.abs());
            l[i] = l[i] * self.dry + wl;
            r[i] = r[i] * self.dry + wr;
        }
        self.tail = peak;
        if self.mode == VerbMode::Off && peak < 1e-5 && self.dry >= 1.0 {
            self.tail = 0.0;
            self.fdn.reset();
            self.wet = 0.0;
        }
    }

    /// The sample rate it was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }
}

// MARK: tape stop and spin-back

/// Seconds of the deck the tape head can reach back into.
pub const TAPE_SECONDS: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TapeMode {
    Live,
    Moving,
    Halted,
}

/// A tape head over the deck's last [`TAPE_SECONDS`]: the live signal is always written; a
/// **stop** reads it at a speed falling from 1 to 0 over `beats` (the pitch and tempo fall
/// together, as a tape machine's motor dies); a **spin-back** reads it backwards from -4 ×
/// `amount` easing to 0 over `beats`, the DJ's hand dragging the record back. Either holds
/// silence once halted (the level follows the speed down over its last tenth, so there is no
/// DC step) until released; then the live signal is back over 10 ms.
pub struct TapeFx {
    sr: f64,
    l: Vec<f32>,
    r: Vec<f32>,
    /// Frames written.
    w: u64,
    /// The read head, an absolute frame position.
    pos: f64,
    rate: f64,
    from: f64,
    /// Frames the move takes, and how far into it.
    len: f64,
    t: f64,
    mode: TapeMode,
    /// The head's share of the output, chasing 1 while moving or halted, 0 when live.
    weight: f32,
    step: f32,
    spinning: bool,
}

impl TapeFx {
    pub fn new(sr: f64) -> TapeFx {
        let cap = (TAPE_SECONDS * sr) as usize + 8;
        TapeFx {
            sr,
            l: vec![0.0; cap],
            r: vec![0.0; cap],
            w: 0,
            pos: 0.0,
            rate: 1.0,
            from: 1.0,
            len: 1.0,
            t: 0.0,
            mode: TapeMode::Live,
            weight: 0.0,
            step: (1.0 / (0.010 * sr)).min(1.0) as f32,
            spinning: false,
        }
    }

    fn begin(&mut self, from: f64, beats: f64, fpb: f64, spinning: bool) {
        // the head starts where the live signal is
        self.pos = self.w as f64 - 1.0;
        self.from = from;
        self.rate = from;
        self.len = (beats.clamp(0.0625, 8.0) * fpb).max(64.0);
        self.t = 0.0;
        self.mode = TapeMode::Moving;
        self.spinning = spinning;
        // a stop starts seamlessly; a spin-back cuts in (the hand grabs the record)
        self.weight = if spinning { self.weight.max(0.0) } else { 1.0 };
    }

    /// Stop the tape over `beats` at `fpb`.
    pub fn stop(&mut self, beats: f64, fpb: f64) {
        self.begin(1.0, beats, fpb, false);
    }

    /// Spin back at up to 4 × `amount` over `beats` at `fpb`.
    pub fn spin_back(&mut self, amount: f64, beats: f64, fpb: f64) {
        self.begin(-4.0 * amount.clamp(0.1, 1.0), beats, fpb, true);
    }

    /// Back to the live signal.
    pub fn release(&mut self) {
        self.mode = TapeMode::Live;
    }

    pub fn is_stopping(&self) -> bool {
        self.mode != TapeMode::Live && !self.spinning
    }

    pub fn is_spinning(&self) -> bool {
        self.mode != TapeMode::Live && self.spinning
    }

    pub fn is_halted(&self) -> bool {
        self.mode == TapeMode::Halted
    }

    #[inline]
    fn at(buf: &[f32], p: f64) -> f32 {
        let len = buf.len() as u64;
        let i = libm::floor(p).max(0.0) as u64;
        let f = (p - libm::floor(p)) as f32;
        let a = buf[(i % len) as usize];
        let b = buf[((i + 1) % len) as usize];
        a + (b - a) * f
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let cap = self.l.len() as u64;
        for i in 0..l.len().min(r.len()) {
            let k = (self.w % cap) as usize;
            self.l[k] = l[i];
            self.r[k] = r[i];
            self.w += 1;
            let want = if self.mode == TapeMode::Live {
                0.0
            } else {
                1.0
            };
            if self.weight < want {
                self.weight = (self.weight + self.step).min(want);
            } else if self.weight > want {
                self.weight = (self.weight - self.step).max(want);
            }
            if self.weight <= 0.0 {
                continue;
            }
            let (hl, hr) = match self.mode {
                TapeMode::Halted => (0.0, 0.0),
                _ => {
                    // the oldest frame still held, and never past the newest
                    let lo = (self.w.saturating_sub(cap - 4)) as f64;
                    self.pos = self.pos.clamp(lo, self.w as f64 - 1.0);
                    let g = (libm::fabs(self.rate) / libm::fabs(self.from) * 10.0).min(1.0) as f32;
                    let s = (
                        Self::at(&self.l, self.pos) * g,
                        Self::at(&self.r, self.pos) * g,
                    );
                    if self.mode == TapeMode::Moving {
                        self.pos += self.rate;
                        self.t += 1.0;
                        let x = (self.t / self.len).min(1.0);
                        // a stop slows evenly; a spin-back eases out
                        self.rate = if self.spinning {
                            self.from * (1.0 - x) * (1.0 - x)
                        } else {
                            self.from * (1.0 - x)
                        };
                        if x >= 1.0 {
                            self.mode = TapeMode::Halted;
                        }
                    }
                    s
                }
            };
            let w = self.weight;
            l[i] = l[i] * (1.0 - w) + hl * w;
            r[i] = r[i] * (1.0 - w) + hr * w;
        }
    }

    /// The sample rate it was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }
}

// MARK: the one-knob resonant filter

/// The knob's dead zone around centre: the filter is out of the way there.
pub const RIDE_DEAD: f64 = 0.02;

/// Where the one knob puts the filter: (high-pass?, cutoff Hz) for a position -1..1, None in
/// the dead zone. Left: a low-pass from 20 kHz down to 60 Hz; right: a high-pass from 20 Hz up
/// to 10 kHz; both exponential in the knob's travel, as a DJ mixer's filter knob.
pub fn ride_cutoff(pos: f64) -> Option<(bool, f64)> {
    let p = if pos.is_finite() {
        pos.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    if libm::fabs(p) <= RIDE_DEAD {
        return None;
    }
    let x = (libm::fabs(p) - RIDE_DEAD) / (1.0 - RIDE_DEAD);
    Some(if p < 0.0 {
        (false, 20_000.0 * libm::pow(60.0 / 20_000.0, x))
    } else {
        (true, 20.0 * libm::pow(10_000.0 / 20.0, x))
    })
}

/// The Q a resonance of `res` (0..1) gives: 0.7 (none) .. 12 (singing).
pub fn ride_q(res: f64) -> f64 {
    0.7 + res.clamp(0.0, 1.0) * 11.3
}

/// The one-knob resonant filter: a stereo state-variable filter (Simper's trapezoidal SVF, stable
/// at any cutoff and Q) whose knob, -1..1, is a low-pass to the left of centre and a high-pass to
/// the right, out of the way in the middle. The knob glides (5 ms) so a jump does not click; a
/// **ride** moves it on its own to a target over a number of frames, and a hand on the knob takes
/// over from a ride at once.
pub struct FilterRide {
    sr: f64,
    pos: f64,
    target: f64,
    /// Per-frame step of a ride under way (0: the knob is the hand's).
    ride_step: f64,
    q: f64,
    ic1: [f32; 2],
    ic2: [f32; 2],
    /// The coefficients, and the knob they were made for.
    coef: (f32, f32, f32, f32),
    made_for: f64,
    hp: bool,
}

impl FilterRide {
    pub fn new(sr: f64) -> FilterRide {
        FilterRide {
            sr,
            pos: 0.0,
            target: 0.0,
            ride_step: 0.0,
            q: ride_q(0.6),
            ic1: [0.0; 2],
            ic2: [0.0; 2],
            coef: (0.0, 0.0, 0.0, 0.0),
            made_for: f64::NAN,
            hp: false,
        }
    }

    /// A hand on the knob: `pos` -1..1 (a ride under way stops).
    pub fn set(&mut self, pos: f64) {
        self.target = if pos.is_finite() {
            pos.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.ride_step = 0.0;
    }

    /// Ride the knob to `to` over `frames`.
    pub fn ride(&mut self, to: f64, frames: f64) {
        self.target = to.clamp(-1.0, 1.0);
        self.ride_step = libm::fabs(self.target - self.pos) / frames.max(1.0);
    }

    pub fn set_resonance(&mut self, res: f64) {
        self.q = ride_q(res);
        self.made_for = f64::NAN;
    }

    /// The knob now.
    pub fn position(&self) -> f64 {
        self.pos
    }

    /// The filter is in the signal (off centre, or getting there).
    pub fn is_active(&self) -> bool {
        libm::fabs(self.pos) > RIDE_DEAD || libm::fabs(self.target) > RIDE_DEAD
    }

    fn make(&mut self) {
        let Some((hp, fc)) = ride_cutoff(self.pos) else {
            return;
        };
        let g = libm::tan(core::f64::consts::PI * (fc / self.sr).min(0.49));
        let k = 1.0 / self.q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        self.coef = (a1 as f32, a2 as f32, a3 as f32, k as f32);
        self.hp = hp;
        self.made_for = self.pos;
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            self.ic1 = [0.0; 2];
            self.ic2 = [0.0; 2];
            return;
        }
        // a glide of 5 ms for a hand, the ride's own pace for a ride
        let glide = 1.0 / (0.005 * self.sr);
        for i in 0..l.len().min(r.len()) {
            let d = self.target - self.pos;
            if d != 0.0 {
                let step = if self.ride_step > 0.0 {
                    self.ride_step
                } else {
                    libm::fabs(d) * glide.min(1.0)
                };
                self.pos += d.signum() * step.min(libm::fabs(d));
                if libm::fabs(self.target - self.pos) < 1e-9 {
                    self.pos = self.target;
                    self.ride_step = 0.0;
                }
            }
            if self.pos != self.made_for {
                // the dead zone: crossing centre resets the filter so it comes in clean
                if ride_cutoff(self.pos).is_none() {
                    self.ic1 = [0.0; 2];
                    self.ic2 = [0.0; 2];
                    self.made_for = self.pos;
                    continue;
                }
                self.make();
            }
            let (a1, a2, a3, k) = self.coef;
            for (c, x) in [(0usize, &mut l[i]), (1usize, &mut r[i])] {
                let v0 = *x;
                let v3 = v0 - self.ic2[c];
                let v1 = a1 * self.ic1[c] + a2 * v3;
                let v2 = self.ic2[c] + a2 * self.ic1[c] + a3 * v3;
                self.ic1[c] = 2.0 * v1 - self.ic1[c];
                self.ic2[c] = 2.0 * v2 - self.ic2[c];
                *x = if self.hp { v0 - k * v1 - v2 } else { v2 };
            }
        }
    }
}

// MARK: noise riser

/// Where a riser's band-pass starts and ends, Hz.
pub const RISER_FROM_HZ: f64 = 300.0;
pub const RISER_TO_HZ: f64 = 12_000.0;

/// A noise riser for transitions: white noise (two generators, so it is wide) through a resonant
/// band-pass sweeping from [`RISER_FROM_HZ`] to [`RISER_TO_HZ`] over its length, exponentially,
/// its level rising on a curve to `amount`; it is added to the deck. At its end (a bar line when
/// it starts on one) it cuts; released early, it fades in 5 ms.
pub struct Riser {
    sr: f64,
    on: bool,
    t: f64,
    len: f64,
    amount: f32,
    gain: f32,
    step: f32,
    rng: [u32; 2],
    ic1: [f32; 2],
    ic2: [f32; 2],
    /// Frames since the coefficients were made (they are remade every 32).
    since: u32,
    coef: (f32, f32, f32),
}

impl Riser {
    pub fn new(sr: f64) -> Riser {
        Riser {
            sr,
            on: false,
            t: 0.0,
            len: 1.0,
            amount: 0.5,
            gain: 0.0,
            step: (1.0 / (0.005 * sr)).min(1.0) as f32,
            rng: [0x1234_5678, 0x9abc_def1],
            ic1: [0.0; 2],
            ic2: [0.0; 2],
            since: u32::MAX,
            coef: (0.0, 0.0, 0.0),
        }
    }

    /// Rise over `frames`, to `amount` (0..1).
    pub fn start(&mut self, amount: f64, frames: f64) {
        self.on = true;
        self.t = 0.0;
        self.len = frames.max(64.0);
        self.amount = amount.clamp(0.0, 1.0) as f32;
        self.since = u32::MAX;
    }

    /// Cut it (a 5 ms fade).
    pub fn stop(&mut self) {
        self.on = false;
    }

    pub fn is_rising(&self) -> bool {
        self.on
    }

    pub fn is_active(&self) -> bool {
        self.on || self.gain > 0.0
    }

    /// How far through it is, 0..1.
    pub fn progress(&self) -> f64 {
        (self.t / self.len).min(1.0)
    }

    fn make(&mut self) {
        let x = self.progress();
        let fc = RISER_FROM_HZ * libm::pow(RISER_TO_HZ / RISER_FROM_HZ, x);
        let g = libm::tan(core::f64::consts::PI * (fc / self.sr).min(0.45));
        // resonance grows as it rises
        let k = 1.0 / (1.5 + 4.0 * x);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        self.coef = (a1 as f32, a2 as f32, a3 as f32);
        self.since = 0;
    }

    #[inline]
    fn noise(&mut self, c: usize) -> f32 {
        let mut x = self.rng[c];
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng[c] = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if !self.is_active() {
            return;
        }
        for i in 0..l.len().min(r.len()) {
            if self.on && self.t >= self.len {
                // the end: a cut
                self.on = false;
                self.gain = 0.0;
            }
            if !self.on {
                self.gain = (self.gain - self.step).max(0.0);
                if self.gain <= 0.0 {
                    self.ic1 = [0.0; 2];
                    self.ic2 = [0.0; 2];
                    return;
                }
            } else {
                let x = self.progress() as f32;
                self.gain = self.amount * x * x;
                self.t += 1.0;
            }
            if self.since >= 32 {
                self.make();
            }
            self.since += 1;
            let (a1, a2, a3) = self.coef;
            let mut y = [0f32; 2];
            for (c, out) in y.iter_mut().enumerate() {
                let v0 = self.noise(c);
                let v3 = v0 - self.ic2[c];
                let v1 = a1 * self.ic1[c] + a2 * v3;
                let v2 = self.ic2[c] + a2 * self.ic1[c] + a3 * v3;
                self.ic1[c] = 2.0 * v1 - self.ic1[c];
                self.ic2[c] = 2.0 * v2 - self.ic2[c];
                // the band-pass output, made up for its narrowing
                *out = v1 * 1.5;
            }
            l[i] += y[0] * self.gain;
            r[i] += y[1] * self.gain;
        }
    }

    /// The sample rate it was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn divisions_snap_and_measure() {
        assert_eq!(snap_div(15.0), 16.0);
        assert_eq!(snap_div(100.0), 32.0);
        assert_eq!(snap_div(f64::NAN), 16.0);
        assert_eq!(div_frames(16.0, 24_000.0), 6_000.0);
        for k in TrickKind::ALL {
            assert_eq!(TrickKind::parse(k.name()), Some(k));
        }
        assert_eq!(TrickKind::parse("beat-repeat"), Some(TrickKind::Stutter));
    }

    #[test]
    fn a_stutter_repeats_the_slice_it_heard() {
        let mut s = Stutter::new(SR);
        // a 1/16 at 120: 6000 frames
        s.start(16.0, 24_000.0, 0.0);
        // a ramp, so a frame says where it came from
        let mut l: Vec<f32> = (0..18_000).map(|i| i as f32 / 18_000.0).collect();
        let mut r = l.clone();
        let live = l.clone();
        s.process(&mut l, &mut r);
        // the first slice plays live
        assert_eq!(&l[..6_000], &live[..6_000]);
        // then repeats it (the middle of each repeat, clear of the edges)
        for rep in 1..3 {
            let at = rep * 6_000 + 3_000;
            assert!(
                (l[at] - live[3_000]).abs() < 1e-4,
                "{rep}: {} vs {}",
                l[at],
                live[3_000]
            );
        }
        assert_eq!(s.repeats, 2);
        // off: live again after the fade
        s.stop();
        let mut l2: Vec<f32> = (0..1_000).map(|_| 0.7).collect();
        let mut r2 = l2.clone();
        s.process(&mut l2, &mut r2);
        assert!((l2[999] - 0.7).abs() < 1e-6);
        assert!(!s.is_active());
    }

    #[test]
    fn a_rising_stutter_speeds_up() {
        let mut s = Stutter::new(SR);
        s.start(8.0, 24_000.0, 12.0);
        let mut l = vec![0.5f32; 12_000 + 12_000 + 6_000 + 100];
        let mut r = l.clone();
        s.process(&mut l, &mut r);
        // 12000 recorded, a repeat at 1x (12000), then one at 2x (6000)
        assert!(s.repeats >= 2, "{}", s.repeats);
    }

    /// A kick every `every` frames: a 60 Hz burst decaying over 80 ms.
    fn kicks(n: usize, every: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = (i % every) as f64 / SR;
                (0.9 * libm::exp(-t / 0.08) * libm::sin(2.0 * core::f64::consts::PI * 60.0 * t))
                    as f32
            })
            .collect()
    }

    #[test]
    fn the_pump_ducks_on_each_kick() {
        let every = 24_000; // a beat at 120
        let side = kicks(4 * every, every);
        let mut p = Pump::new(SR);
        p.start(0.8, 8.0, 24_000.0, false);
        let mut l = vec![1.0f32; side.len()];
        let mut r = l.clone();
        p.process(&mut l, &mut r, Some(&side));
        assert_eq!(p.triggers, 4, "one per kick");
        // ducked just after each kick, back up before the next
        for k in 0..4 {
            let at = k * every;
            let low = l[at + 200..at + 2_000]
                .iter()
                .cloned()
                .fold(1.0f32, f32::min);
            assert!(low < 0.35, "kick {k}: {low}");
            assert!(
                l[at + 20_000] > 0.99,
                "kick {k} released: {}",
                l[at + 20_000]
            );
        }
        // off: the gain glides back to 1
        p.stop();
        let mut l = vec![1.0f32; 4_000];
        let mut r = l.clone();
        p.process(&mut l, &mut r, Some(&side[..4_000]));
        assert_eq!(l[3_999], 1.0);
        assert!(!p.is_active());
    }

    #[test]
    fn the_grid_pump_needs_no_kick() {
        let mut p = Pump::new(SR);
        p.start(1.0, 16.0, 12_000.0, true);
        let mut l = vec![1.0f32; 48_000];
        let mut r = l.clone();
        p.process(&mut l, &mut r, None);
        assert_eq!(p.triggers, 4);
        assert!(l[200] < 0.1);
        assert!(l[11_000] > 0.99);
        // quiet in, nothing out of the detector
        let mut q = Pump::new(SR);
        q.start(1.0, 8.0, 24_000.0, false);
        let mut l = vec![1.0f32; 48_000];
        let mut r = l.clone();
        q.process(&mut l, &mut r, Some(&vec![0.0; 48_000]));
        assert_eq!(q.triggers, 0);
        assert!(l.iter().all(|x| *x == 1.0));
    }

    #[test]
    fn a_throw_echoes_on_the_beat_and_rings_after_release() {
        let mut d = DubDelay::new(SR);
        let fpb = 24_000.0;
        d.throw(1.0, 0.75, fpb, 0.6);
        // a click while thrown
        let mut l = vec![0.0f32; 96_000];
        l[100] = 1.0;
        let mut r = l.clone();
        d.process(&mut l[..1_000], &mut r[..1_000]);
        d.release();
        d.process(&mut l[1_000..], &mut r[1_000..]);
        // the first echo a dotted eighth (18000 frames) later on the left, the next on the right
        let echo = |x: &[f32], around: usize| {
            x[around - 300..around + 300]
                .iter()
                .fold(0f32, |m, v| m.max(v.abs()))
        };
        assert!(echo(&l, 18_100) > 0.05, "first echo {}", echo(&l, 18_100));
        assert!(echo(&r, 36_100) > 0.01, "ping-pong {}", echo(&r, 36_100));
        assert!(echo(&r, 36_100) < echo(&l, 18_100), "they fade");
        // nothing between the echoes
        assert!(echo(&l, 9_000) < 1e-4);
        assert!(d.ringing());
        // a second click after the release is not echoed
        let mut l2 = vec![0.0f32; 48_000];
        l2[10] = 1.0;
        let mut r2 = l2.clone();
        let mut d2 = DubDelay::new(SR);
        d2.throw(1.0, 0.75, fpb, 0.0);
        d2.release();
        let mut silent = vec![0.0f32; 2_000];
        let mut silent_r = silent.clone();
        d2.process(&mut silent, &mut silent_r);
        d2.process(&mut l2, &mut r2);
        assert!(l2[18_010 - 200..18_010 + 200]
            .iter()
            .all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn released_before_the_first_echo_it_still_echoes() {
        let mut d = DubDelay::new(SR);
        d.throw(1.0, 0.75, 24_000.0, 0.5);
        // the send opens over 10 ms: the click comes after
        let mut l = vec![0.0f32; 40_960];
        l[600] = 1.0;
        let mut r = l.clone();
        d.process(&mut l[..1_024], &mut r[..1_024]);
        d.release();
        for k in 4..(40_960 / 256) {
            let a = 256 * k;
            d.process(&mut l[a..a + 256], &mut r[a..a + 256]);
        }
        assert!(l[18_600 - 300..18_600 + 300].iter().any(|v| v.abs() > 0.05));
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    #[test]
    fn the_stand_in_maps_decision_ones_knobs() {
        let s = stand_in_spec(0.0, 0.0);
        assert_eq!((s.seconds, s.damping), (0.25, 1_000.0));
        let s = stand_in_spec(1.0, 1.0);
        assert_eq!((s.seconds, s.damping), (8.0, 16_000.0));
    }

    #[test]
    fn a_wash_fades_the_dry_into_the_room_and_decays_after() {
        let mut v = VerbThrow::new(SR);
        v.wash(1.0, 1.0, 24_000.0);
        let src = noise(48_000, 3);
        let mut l = src.clone();
        let mut r = src.clone();
        v.process(&mut l, &mut r);
        // a beat in, the dry is gone: what is left is the room, not the input
        let tail = &l[30_000..48_000];
        let corr: f32 = tail
            .iter()
            .zip(&src[30_000..48_000])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / (rms(tail) * rms(&src[30_000..48_000]) * tail.len() as f32);
        assert!(rms(tail) > 0.02, "the room sounds: {}", rms(tail));
        assert!(corr.abs() < 0.2, "the dry is out: {corr}");
        v.release();
        // silence in: the room rings, then stops
        let mut z = vec![0.0f32; 24_000];
        let mut zr = z.clone();
        v.process(&mut z, &mut zr);
        assert!(rms(&z[..4_800]) > 0.01);
        for _ in 0..40 {
            let mut z = vec![0.0f32; 24_000];
            let mut zr = z.clone();
            v.process(&mut z, &mut zr);
        }
        assert!(!v.is_active());
        // and the dry is back at once
        let mut a = vec![0.25f32; 2_000];
        let mut b = a.clone();
        v.process(&mut a, &mut b);
        assert_eq!(a[1_999], 0.25);
    }

    #[test]
    fn a_freeze_holds_the_room_after_the_input_stops() {
        let mut v = VerbThrow::new(SR);
        v.freeze(1.0, 0.5, 24_000.0);
        let src = noise(24_000, 9);
        let mut l = src.clone();
        let mut r = src.clone();
        v.process(&mut l, &mut r);
        // silence for many seconds: the frozen room holds its level
        let mut first = 0.0;
        for k in 0..10 {
            let mut z = vec![0.0f32; 48_000];
            let mut zr = z.clone();
            v.process(&mut z, &mut zr);
            let lv = rms(&z);
            if k == 0 {
                first = lv;
                assert!(lv > 0.01, "{lv}");
            }
            assert!(lv > 0.5 * first, "second {k}: {lv} vs {first}");
        }
        // released, it decays (8.5 s room: well down a few seconds on)
        v.release();
        let mut last = 1.0;
        for _ in 0..6 {
            let mut z = vec![0.0f32; 48_000];
            let mut zr = z.clone();
            v.process(&mut z, &mut zr);
            last = rms(&z);
        }
        assert!(last < 0.5 * first, "{last} vs {first}");
    }

    #[test]
    fn a_tape_stop_slows_the_pitch_and_halts() {
        let mut t = TapeFx::new(SR);
        // a second of a 1 kHz tone in the tape first
        let tone = |n: usize, from: usize| -> Vec<f32> {
            (from..from + n)
                .map(|i| (2.0 * core::f64::consts::PI * 1000.0 * i as f64 / SR).sin() as f32 * 0.5)
                .collect()
        };
        let mut l = tone(48_000, 0);
        let mut r = l.clone();
        t.process(&mut l, &mut r);
        t.stop(1.0, 24_000.0);
        let mut l = tone(48_000, 48_000);
        let mut r = l.clone();
        let live = l.clone();
        t.process(&mut l, &mut r);
        // the first moments are the live signal, unbroken
        assert!((l[10] - live[10]).abs() < 0.05);
        // zero crossings: fewer per window as the tape slows
        let zc = |x: &[f32]| {
            x.windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count()
        };
        let early = zc(&l[0..4_800]);
        let late = zc(&l[16_000..20_800]);
        assert!(late * 2 < early, "{early} -> {late}");
        // halted: silence after the beat
        assert!(l[25_000..].iter().all(|x| x.abs() < 1e-6));
        assert!(t.is_halted());
        t.release();
        let mut l = tone(4_800, 96_000);
        let mut r = l.clone();
        let live = l.clone();
        t.process(&mut l, &mut r);
        assert!((l[4_000] - live[4_000]).abs() < 1e-6, "live again");
    }

    #[test]
    fn a_spin_back_plays_the_past_backwards() {
        let mut t = TapeFx::new(SR);
        // a ramp: a frame's value says when it was written
        let mut l: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let mut r = l.clone();
        t.process(&mut l, &mut r);
        t.spin_back(1.0, 0.5, 24_000.0);
        let mut l = vec![1.0f32; 12_000];
        let mut r = l.clone();
        t.process(&mut l, &mut r);
        // after the cut-in the head runs back through the ramp: values falling, well into it
        let a = l[1_000];
        let b = l[3_000];
        assert!(b < a && a < 1.0, "{a} {b}");
        assert!(l[11_999].abs() < 1e-6, "halted");
        assert!(t.is_spinning());
    }

    #[test]
    fn the_one_knob_maps_left_low_right_high() {
        assert_eq!(ride_cutoff(0.0), None);
        assert_eq!(ride_cutoff(0.01), None);
        let (hp, f) = ride_cutoff(-1.0).unwrap();
        assert!(!hp && (f - 60.0).abs() < 1e-6);
        let (hp, f) = ride_cutoff(1.0).unwrap();
        assert!(hp && (f - 10_000.0).abs() < 1e-6);
        let (_, a) = ride_cutoff(-0.3).unwrap();
        let (_, b) = ride_cutoff(-0.6).unwrap();
        assert!(b < a);
        assert!(ride_q(1.0) > ride_q(0.0));
    }

    #[test]
    fn the_filter_cuts_and_rings_and_rides() {
        let tone = |hz: f64, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| (2.0 * core::f64::consts::PI * hz * i as f64 / SR).sin() as f32 * 0.5)
                .collect()
        };
        // a low-pass well down: a 5 kHz tone is cut, a 100 Hz one passes
        let mut f = FilterRide::new(SR);
        f.set(-0.7);
        let mut hi = tone(5_000.0, 9_600);
        let mut hr = hi.clone();
        f.process(&mut hi, &mut hr);
        let mut f2 = FilterRide::new(SR);
        f2.set(-0.7);
        let mut lo = tone(100.0, 9_600);
        let mut lr = lo.clone();
        f2.process(&mut lo, &mut lr);
        assert!(
            rms(&hi[4_800..]) < 0.1 * rms(&lo[4_800..]),
            "{} vs {}",
            rms(&hi[4_800..]),
            rms(&lo[4_800..])
        );
        // resonance: a tone at the cutoff comes out louder with more of it
        let (_, fc) = ride_cutoff(-0.5).unwrap();
        let ring = |res: f64| {
            let mut f = FilterRide::new(SR);
            f.set_resonance(res);
            f.set(-0.5);
            let mut x = tone(fc, 9_600);
            let mut y = x.clone();
            f.process(&mut x, &mut y);
            rms(&x[4_800..])
        };
        assert!(ring(1.0) > 3.0 * ring(0.0));
        // a ride gets there in its own time; centre is dry
        let mut f = FilterRide::new(SR);
        f.ride(-0.8, 4_800.0);
        let mut x = vec![0.1f32; 2_400];
        let mut y = x.clone();
        f.process(&mut x, &mut y);
        assert!((f.position() + 0.4).abs() < 0.01, "{}", f.position());
        let mut x = vec![0.1f32; 4_800];
        let mut y = x.clone();
        f.process(&mut x, &mut y);
        assert!((f.position() + 0.8).abs() < 1e-9);
        f.set(0.0);
        let mut x = vec![0.1f32; 4_800];
        let mut y = x.clone();
        f.process(&mut x, &mut y);
        let mut x = vec![0.25f32; 100];
        let mut y = x.clone();
        f.process(&mut x, &mut y);
        assert_eq!(x[99], 0.25, "dry at centre");
        assert!(!f.is_active());
    }

    #[test]
    fn a_riser_rises_brightens_and_cuts_at_its_end() {
        let mut s = Riser::new(SR);
        s.start(0.5, 48_000.0);
        let mut l = vec![0.0f32; 48_000];
        let mut r = l.clone();
        s.process(&mut l, &mut r);
        let (early, late) = (rms(&l[2_000..8_000]), rms(&l[40_000..46_000]));
        assert!(late > 4.0 * early, "rises: {early} -> {late}");
        let bright = |x: &[f32]| {
            let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(x).max(1e-9)
        };
        assert!(
            bright(&l[40_000..46_000]) > 2.0 * bright(&l[10_000..16_000]),
            "brightens"
        );
        assert_ne!(l[45_000], r[45_000], "wide");
        // past its end: cut, silent
        let mut z = vec![0.0f32; 4_800];
        let mut zr = z.clone();
        s.process(&mut z, &mut zr);
        assert!(z.iter().all(|x| *x == 0.0));
        assert!(!s.is_active());
        // released early: a 5 ms fade
        s.start(1.0, 48_000.0);
        let mut a = vec![0.0f32; 30_000];
        let mut b = a.clone();
        s.process(&mut a, &mut b);
        s.stop();
        let mut a = vec![0.0f32; 1_000];
        let mut b = a.clone();
        s.process(&mut a, &mut b);
        assert!(a[0].abs() > 0.0 && a[400..].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn the_echoes_die_away() {
        let mut d = DubDelay::new(SR);
        d.throw(1.0, 0.25, 12_000.0, 0.9);
        let mut l: Vec<f32> = (0..4_800)
            .map(|i| ((i as f32) * 0.05).sin() * 0.8)
            .collect();
        let mut r = l.clone();
        d.process(&mut l, &mut r);
        d.release();
        for _ in 0..400 {
            let mut z = vec![0.0f32; 4_800];
            let mut zr = z.clone();
            d.process(&mut z, &mut zr);
            assert!(
                z.iter().all(|x| x.abs() <= 1.0),
                "saturated, never runs away"
            );
        }
        assert!(!d.is_active(), "silent at last");
    }

    #[test]
    fn stopped_before_the_slice_is_recorded_it_is_live() {
        let mut s = Stutter::new(SR);
        s.start(4.0, 24_000.0, 0.0);
        let mut l = vec![0.3f32; 1_000];
        let mut r = l.clone();
        s.process(&mut l, &mut r);
        s.stop();
        let mut l = vec![0.3f32; 1_000];
        let mut r = l.clone();
        s.process(&mut l, &mut r);
        assert!(l.iter().all(|x| (*x - 0.3).abs() < 1e-6));
    }
}
