//! Per-loop after effects (`Audio/LoopFx.swift`): an insert chain on each loop-bank voice,
//! after its playback and before its level x fade, plus a safety limiter on the loop channel.
//! Non-destructive: a clip's audio stays dry and the FX are a value on the clip
//! ([`LoopFx`](crate::session::types::LoopFx)) that the voice plays it through.
//!
//! ```text
//! playback -> drive (0...+24 dB) -> shaper (clean | soft | hard | tube | fold | crush, amount)
//!          -> tone (2-pole lowpass) -> auto-level -> dry/wet mix -> trim -> level x fade -> sum
//! sum (the loop channel) -> safety limiter (only while any voice has FX, or its gain is back up)
//! ```
//!
//! The four waveshaping modes run at 2x through a 47-tap Kaiser halfband (23 base samples of
//! latency, matched by a delay on the dry side of the mix); clean is linear and crush is not
//! oversampled (aliasing is the sound). Bypass is exact: a voice whose FX are off
//! ([`LoopFx::is_active`] false) never enters the chain, so its samples are the turntable's
//! bytes. Switching FX on or off, or a mode / sample-and-hold change, crossfades the chain in
//! and out over 10 ms; every continuous parameter is smoothed (one pole, ~10 ms) toward its
//! target.
//!
//! Real-time: [`LoopFxVoice::process`] and [`LoopFxLimiter::process`] allocate nothing (every
//! buffer is made in `new`). Parameters cross as [`LoopFxParams`] (plain floats), made on the
//! main thread from a `LoopFx`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::master_chain::{crush_quantize, crush_step};
use crate::session::types::{LoopFx, LoopFxMode};
use crate::wave::synth::tone_distortion;

impl LoopFxMode {
    /// `LoopFXMode.code`: the byte the audio thread switches on.
    pub fn code(self) -> u8 {
        match self {
            LoopFxMode::Clean => 0,
            LoopFxMode::Soft => 1,
            LoopFxMode::Hard => 2,
            LoopFxMode::Tube => 3,
            LoopFxMode::Fold => 4,
            LoopFxMode::Crush => 5,
        }
    }

    /// `LoopFXMode.oversampled`: runs at 2x (the waveshapers).
    pub fn oversampled(self) -> bool {
        self != LoopFxMode::Clean && self != LoopFxMode::Crush
    }
}

impl LoopFx {
    pub const DRIVE_RANGE: (f64, f64) = (0.0, 24.0);
    pub const BITS_RANGE: (f64, f64) = (1.0, 16.0);
    pub const DOWNSAMPLE_RANGE: (i64, i64) = (1, 32);
    pub const TRIM_RANGE: (f64, f64) = (-24.0, 12.0);

    /// `LoopFX.isActive`: does the chain change the sound at all? Off, bypassed, or set to
    /// nothing: the voice plays its clip untouched.
    pub fn is_active(&self) -> bool {
        if self.bypass {
            return false;
        }
        if self.trim != 0.0 {
            return true;
        }
        if self.mix <= 0.0 {
            return false;
        }
        self.drive > 0.0 || (self.mode != LoopFxMode::Clean && self.amount > 0.0) || self.tone < 1.0
    }

    /// `LoopFX.summary`: "drive +12 dB · soft 60 %" for a badge's help, "off".
    pub fn summary(&self) -> String {
        if !self.is_active() {
            return String::from(if self.bypass { "bypassed" } else { "off" });
        }
        let mut parts: Vec<String> = Vec::new();
        if self.drive > 0.0 {
            parts.push(format!("drive +{:.0} dB", self.drive));
        }
        if self.mode != LoopFxMode::Clean && self.amount > 0.0 {
            parts.push(if self.mode == LoopFxMode::Crush {
                let mut s = format!("crush {:.0} bit", self.bits);
                if self.downsample > 1 {
                    s.push_str(&format!(" \u{f7}{}", self.downsample));
                }
                s
            } else {
                format!("{} {:.0}%", self.mode.raw(), self.amount * 100.0)
            });
        }
        if self.tone < 1.0 {
            parts.push(format!("tone {:.0}%", self.tone * 100.0));
        }
        if self.mix < 1.0 {
            parts.push(format!("mix {:.0}%", self.mix * 100.0));
        }
        if self.trim != 0.0 {
            parts.push(format!("trim {:+.1} dB", self.trim));
        }
        if !self.auto_level {
            parts.push(String::from("auto-level off"));
        }
        parts.join(" \u{b7} ")
    }

    /// `LoopFX.presets`: the right-click menu's quick settings, in order.
    pub fn presets() -> Vec<(&'static str, LoopFx)> {
        let d = LoopFx::default;
        vec![
            ("Off", d()),
            (
                "Warm drive",
                LoopFx {
                    drive: 6.0,
                    mode: LoopFxMode::Tube,
                    amount: 0.35,
                    tone: 0.8,
                    ..d()
                },
            ),
            (
                "Crunch",
                LoopFx {
                    drive: 9.0,
                    mode: LoopFxMode::Hard,
                    amount: 0.55,
                    tone: 0.75,
                    ..d()
                },
            ),
            (
                "Fuzz",
                LoopFx {
                    drive: 18.0,
                    mode: LoopFxMode::Soft,
                    amount: 0.9,
                    tone: 0.6,
                    ..d()
                },
            ),
            (
                "Crush",
                LoopFx {
                    mode: LoopFxMode::Crush,
                    amount: 1.0,
                    bits: 6.0,
                    downsample: 4,
                    tone: 0.85,
                    mix: 0.8,
                    ..d()
                },
            ),
            (
                "Loud",
                LoopFx {
                    drive: 9.0,
                    mode: LoopFxMode::Soft,
                    amount: 0.15,
                    auto_level: false,
                    ..d()
                },
            ),
        ]
    }

    /// `LoopFX.presetName`: the preset these settings are, if any.
    pub fn preset_name(&self) -> Option<&'static str> {
        Self::presets()
            .into_iter()
            .find(|(_, fx)| fx == self)
            .map(|(n, _)| n)
    }
}

// MARK: shared DSP

/// The tube shaper's bias and the stiffness of its Tone.Distortion curve.
const TUBE_BIAS: f64 = 0.08;
const TUBE_CURVE: f64 = 0.2;

#[inline]
fn tube(v: f64) -> f64 {
    tone_distortion(v + TUBE_BIAS, TUBE_CURVE) - tone_distortion(TUBE_BIAS, TUBE_CURVE)
}

/// A triangle fold into -1...1.
#[inline]
fn fold(v: f32) -> f32 {
    let mut t = (v + 1.0) % 4.0;
    if t < 0.0 {
        t += 4.0;
    }
    if t < 2.0 {
        t - 1.0
    } else {
        3.0 - t
    }
}

/// The targets a voice's chain heads for, as the audio thread uses them (gains linear,
/// coefficients ready): made on the main thread from a `LoopFx` (`LoopFxParams`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopFxParams {
    pub active: bool,
    pub mode: u8,
    /// Sample-and-hold length (crush).
    pub hold: i32,
    pub drive: f32,
    pub amount: f32,
    /// Gain into the curve, and the curve's normalisation (full scale in, full scale out).
    pub pre: f32,
    pub norm: f32,
    /// Crusher step (2^(1 - bits)).
    pub step: f32,
    /// The lowpass's g = tan(pi fc / fs), and how much of it is heard (0 at tone 1).
    pub tone_g: f32,
    pub tone_wet: f32,
    pub mix: f32,
    pub trim: f32,
    /// Static auto-level (1 / drive) and whether auto-level is on (0 / 1).
    pub comp: f32,
    pub auto: f32,
}

impl Default for LoopFxParams {
    fn default() -> Self {
        LoopFxParams {
            active: false,
            mode: 0,
            hold: 1,
            drive: 1.0,
            amount: 0.0,
            pre: 1.0,
            norm: 1.0,
            step: 1.0 / 128.0,
            tone_g: 1.0,
            tone_wet: 0.0,
            mix: 1.0,
            trim: 1.0,
            comp: 1.0,
            auto: 1.0,
        }
    }
}

impl LoopFxParams {
    /// `LoopFxParams(_:sampleRate:)`.
    pub fn new(fx: &LoopFx, sample_rate: f64) -> LoopFxParams {
        let sr = sample_rate;
        let f = fx.clamped();
        let drive = libm::pow(10.0, f.drive / 20.0);
        let pre = libm::pow(10.0, f.amount * 30.0 / 20.0);
        let norm = match f.mode {
            LoopFxMode::Soft => (1.0 / libm::tanh(pre)) as f32,
            LoopFxMode::Tube => (1.0 / libm::fabs(tube(pre)).max(libm::fabs(tube(-pre)))) as f32,
            _ => 1.0,
        };
        let fc = (400.0 * libm::pow(50.0, f.tone)).min(0.45 * sr);
        LoopFxParams {
            active: f.is_active(),
            mode: f.mode.code(),
            hold: f.downsample as i32,
            drive: drive as f32,
            amount: if f.mode == LoopFxMode::Clean {
                0.0
            } else {
                f.amount as f32
            },
            pre: pre as f32,
            norm,
            step: crush_step(f.bits) as f32,
            tone_g: libm::tan(core::f64::consts::PI * fc / sr) as f32,
            tone_wet: if f.tone < 1.0 { 1.0 } else { 0.0 },
            mix: f.mix as f32,
            trim: libm::pow(10.0, f.trim / 20.0) as f32,
            comp: (1.0 / drive) as f32,
            auto: if f.auto_level { 1.0 } else { 0.0 },
        }
    }

    /// Every continuous value at its target (`isSettled(at:)`).
    fn is_settled_at(&self, t: &LoopFxParams) -> bool {
        self.drive == t.drive
            && self.amount == t.amount
            && self.pre == t.pre
            && self.norm == t.norm
            && self.step == t.step
            && self.tone_g == t.tone_g
            && self.tone_wet == t.tone_wet
            && self.mix == t.mix
            && self.trim == t.trim
            && self.comp == t.comp
            && self.auto == t.auto
    }

    /// One step of one-pole smoothing toward `t`, landing exactly once within a hair of it
    /// (`approach(_:_:)`).
    fn approach(&mut self, t: &LoopFxParams, k: f32) {
        #[inline(always)]
        fn mv(v: &mut f32, to: f32, k: f32) {
            let d = to - *v;
            *v = if d.abs() <= 1e-6f32 * to.abs().max(1.0) {
                to
            } else {
                *v + k * d
            };
        }
        mv(&mut self.drive, t.drive, k);
        mv(&mut self.amount, t.amount, k);
        mv(&mut self.pre, t.pre, k);
        mv(&mut self.norm, t.norm, k);
        mv(&mut self.step, t.step, k);
        mv(&mut self.tone_g, t.tone_g, k);
        mv(&mut self.tone_wet, t.tone_wet, k);
        mv(&mut self.mix, t.mix, k);
        mv(&mut self.trim, t.trim, k);
        mv(&mut self.comp, t.comp, k);
        mv(&mut self.auto, t.auto, k);
    }
}

/// Taps in the halfband's non-zero half: 24 side taps (the centre is 0.5).
pub const HALFBAND_TAPS: usize = 24;

/// The 2x halfband (47 taps, Kaiser beta 7): its 24 non-zero side taps. Scaled so the side taps
/// sum to exactly 0.5 (unity at DC both ways) (`halfbandTaps`).
pub fn halfband_taps() -> [f32; HALFBAND_TAPS] {
    let n = 47i32;
    let mid = n / 2;
    fn i0(x: f64) -> f64 {
        let mut sum = 1.0;
        let mut term = 1.0;
        for k in 1..32 {
            term *= (x / 2.0) / k as f64;
            sum += term * term;
        }
        sum
    }
    let beta = 7.0;
    let mut taps = [0f64; HALFBAND_TAPS];
    let mut i = 0;
    let mut k = 0;
    while k < n {
        let j = (k - mid) as f64; // odd: -23, -21, ... 23
        let sinc = libm::sin(core::f64::consts::PI * j / 2.0) / (core::f64::consts::PI * j / 2.0);
        let r = j / mid as f64;
        let w = i0(beta * libm::sqrt(1.0 - r * r)) / i0(beta);
        taps[i] = 0.5 * sinc * w;
        i += 1;
        k += 2;
    }
    let mut s = 0.0;
    for t in taps {
        s += t;
    }
    let mut out = [0f32; HALFBAND_TAPS];
    for (o, t) in out.iter_mut().zip(taps) {
        *o = (t * 0.5 / s) as f32;
    }
    out
}

/// A short history, newest first, readable as one contiguous run (`History`): `at(k)` = k
/// samples ago.
struct History {
    buf: Vec<f32>,
    length: usize,
    pos: usize,
}

impl History {
    fn new(length: usize) -> History {
        History {
            buf: vec![0.0; 2 * length],
            length,
            pos: 0,
        }
    }
    #[inline]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 {
            self.length - 1
        } else {
            self.pos - 1
        };
        self.buf[self.pos] = x;
        self.buf[self.pos + self.length] = x;
    }
    #[inline]
    fn at(&self, k: usize) -> f32 {
        self.buf[self.pos + k]
    }
    /// The run from now back: `run()[k]` = k samples ago.
    #[inline]
    fn run(&self) -> &[f32] {
        &self.buf[self.pos..]
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

/// One channel's chain state (`FxChannel`).
struct FxChannel {
    /// Input (base rate) for the upsampler and the dry side's delay.
    input: History,
    /// The shaped signal at 2x: its even samples (for the decimating filter) and its odd ones
    /// (12 samples back is the centre tap).
    even: History,
    odd: History,
    /// The tone lowpass (TPT state variable filter).
    ic1: f32,
    ic2: f32,
    /// DC blocker (tube).
    dc_x: f32,
    dc_y: f32,
    /// Crush's held value.
    held: f32,
}

impl FxChannel {
    fn new() -> FxChannel {
        FxChannel {
            input: History::new(24),
            even: History::new(24),
            odd: History::new(13),
            ic1: 0.0,
            ic2: 0.0,
            dc_x: 0.0,
            dc_y: 0.0,
            held: 0.0,
        }
    }
    fn clear(&mut self) {
        self.input.clear();
        self.even.clear();
        self.odd.clear();
        self.ic1 = 0.0;
        self.ic2 = 0.0;
        self.dc_x = 0.0;
        self.dc_y = 0.0;
        self.held = 0.0;
    }
}

/// One loop-bank voice's FX chain (`LoopFxVoice`). Audio thread: `set` (from the bank's
/// command), `process`.
pub struct LoopFxVoice {
    taps: [f32; HALFBAND_TAPS],
    ch: [FxChannel; 2],
    /// Where the chain is heading, and the settings waiting for a crossfade through dry (a mode
    /// or hold change, or FX switched on while the old ones fade out).
    target: LoopFxParams,
    pending: Option<LoopFxParams>,
    /// The values in use (smoothed toward `target`).
    cur: LoopFxParams,
    mode: u8,
    hold: i32,
    hold_count: i32,
    /// Chain in (1) or out (0) of the voice, and its ramp.
    engage: f32,
    engage_step: f32,
    engage_left: i64,
    engage_frames: i64,
    /// One-pole smoothing per sample, and the auto-level followers' coefficient.
    smooth: f32,
    follow: f32,
    power_in: f32,
    power_wet: f32,
    dc_r: f32,
    /// Whether the chain is in the signal path (FX on, or fading out).
    running: bool,
}

impl LoopFxVoice {
    /// Base samples the oversampled modes are late by (the dry side waits as long).
    pub const LATENCY: usize = 23;

    pub fn new(sample_rate: f64) -> LoopFxVoice {
        let sr = sample_rate;
        LoopFxVoice {
            taps: halfband_taps(),
            ch: [FxChannel::new(), FxChannel::new()],
            target: LoopFxParams::default(),
            pending: None,
            cur: LoopFxParams::default(),
            mode: 0,
            hold: 1,
            hold_count: 0,
            engage: 0.0,
            engage_step: 0.0,
            engage_left: 0,
            engage_frames: ((sr * 0.01) as i64).max(1),
            smooth: (1.0 - libm::exp(-1.0 / (0.01 * sr))) as f32,
            follow: (1.0 - libm::exp(-1.0 / (0.3 * sr))) as f32,
            power_in: 0.0,
            power_wet: 0.0,
            dc_r: (1.0 - 2.0 * core::f64::consts::PI * 10.0 / sr) as f32,
            running: false,
        }
    }

    /// Whether the chain is in the signal path (FX on, or fading out).
    pub fn running(&self) -> bool {
        self.running
    }

    /// New targets. `sounding`: the voice is playing (changes are smoothed and crossfaded);
    /// otherwise they take effect at once, from a clean state.
    pub fn set(&mut self, p: &LoopFxParams, sounding: bool) {
        if !sounding {
            self.target = *p;
            self.pending = None;
            self.cur = *p;
            self.mode = p.mode;
            self.hold = p.hold.max(1);
            self.reset();
            self.engage = if p.active { 1.0 } else { 0.0 };
            self.engage_left = 0;
            self.engage_step = 0.0;
            self.running = p.active;
            return;
        }
        if !self.running {
            if !p.active {
                self.target = *p;
                self.cur = *p;
                return;
            }
            // in from dry: the chain starts clean at the new settings and fades in
            self.target = *p;
            self.cur = *p;
            self.mode = p.mode;
            self.hold = p.hold.max(1);
            self.reset();
            self.running = true;
            self.ramp(1.0);
            return;
        }
        let swaps = p.mode != self.mode || p.hold.max(1) != self.hold;
        if !p.active || swaps {
            // out through dry first; `pending` comes in after (if it is on)
            self.pending = if p.active { Some(*p) } else { None };
            if !p.active {
                self.target.active = false;
            }
            self.ramp(0.0);
            return;
        }
        self.pending = None;
        self.target = *p;
        if self.engage < 1.0 || self.engage_step < 0.0 {
            self.ramp(1.0);
        }
    }

    fn ramp(&mut self, t: f32) {
        self.engage_left = 1i64.max((self.engage_frames as f32 * (t - self.engage).abs()) as i64);
        self.engage_step = (t - self.engage) / self.engage_left as f32;
        if self.engage_step == 0.0 {
            self.engage_left = 0;
        }
    }

    fn reset(&mut self) {
        self.ch[0].clear();
        self.ch[1].clear();
        self.hold_count = 0;
        self.power_in = 0.0;
        self.power_wet = 0.0;
    }

    /// Run `n` samples of the voice's playback (in place) through the chain.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], n: usize) {
        if !self.running {
            return;
        }
        let settled = self.cur.is_settled_at(&self.target);
        for i in 0..n {
            if !settled {
                let t = self.target;
                self.cur.approach(&t, self.smooth);
            }
            let xl = l[i];
            let xr = r[i];
            let (yl, yr) = self.chain(xl, xr);
            // the chain in or out, through the dry signal as it is now
            if self.engage_left > 0 {
                self.engage_left -= 1;
                self.engage = if self.engage_left == 0 {
                    if self.engage_step > 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    self.engage + self.engage_step
                };
                if self.engage_left == 0 {
                    self.engage_step = 0.0;
                }
            }
            let e = self.engage;
            l[i] = (1.0 - e) * xl + e * yl;
            r[i] = (1.0 - e) * xr + e * yr;
            if self.engage == 0.0 && self.engage_left == 0 {
                // out: the next settings come in clean, or the chain leaves the path
                if let Some(p) = self.pending.take() {
                    self.target = p;
                    self.cur = p;
                    self.mode = p.mode;
                    self.hold = p.hold.max(1);
                    self.reset();
                    self.ramp(1.0);
                } else {
                    self.reset();
                    self.running = false;
                    return; // the rest of the block is the voice's own samples
                }
            }
        }
    }

    /// One stereo sample through the chain at the current settings (`chain`).
    #[inline(always)]
    fn chain(&mut self, xl: f32, xr: f32) -> (f32, f32) {
        let c = self.cur;
        let mut wl: f32;
        let mut wr: f32;
        let mut dl = xl;
        let mut dr = xr;
        match self.mode {
            0 => {
                wl = xl * c.drive;
                wr = xr * c.drive;
            }
            5 => {
                if self.hold_count == 0 {
                    // a converter's range: driven past full scale it clips, then the bits go
                    let step = c.step as f64;
                    self.ch[0].held =
                        crush_quantize((xl * c.drive).clamp(-1.0, 1.0) as f64, step) as f32;
                    self.ch[1].held =
                        crush_quantize((xr * c.drive).clamp(-1.0, 1.0) as f64, step) as f32;
                    self.hold_count = self.hold;
                }
                self.hold_count -= 1;
                let ul = xl * c.drive;
                let ur = xr * c.drive;
                wl = (1.0 - c.amount) * ul + c.amount * self.ch[0].held;
                wr = (1.0 - c.amount) * ur + c.amount * self.ch[1].held;
            }
            _ => {
                wl = oversample(&mut self.ch[0], xl, &c, self.mode, &self.taps);
                wr = oversample(&mut self.ch[1], xr, &c, self.mode, &self.taps);
                dl = self.ch[0].input.at(Self::LATENCY);
                dr = self.ch[1].input.at(Self::LATENCY);
                if self.mode == 3 {
                    wl = dc_block(&mut self.ch[0], wl, self.dc_r);
                    wr = dc_block(&mut self.ch[1], wr, self.dc_r);
                }
            }
        }
        // tone
        if c.tone_wet > 0.0 {
            wl += c.tone_wet * (lowpass(&mut self.ch[0], wl, c.tone_g) - wl);
            wr += c.tone_wet * (lowpass(&mut self.ch[1], wr, c.tone_g) - wr);
        } else {
            let _ = lowpass(&mut self.ch[0], wl, c.tone_g);
            let _ = lowpass(&mut self.ch[1], wr, c.tone_g);
        }
        // auto-level: 1 / drive, then the followers' ratio for what the shaping did to the level
        let sl = wl * c.comp;
        let sr = wr * c.comp;
        self.power_in += self.follow * (0.5 * (dl * dl + dr * dr) - self.power_in);
        self.power_wet += self.follow * (0.5 * (sl * sl + sr * sr) - self.power_wet);
        let agc = libm::sqrtf((self.power_in + 1e-9) / (self.power_wet + 1e-9)).clamp(0.25, 4.0);
        let g = 1.0 + c.auto * (c.comp * agc - 1.0);
        (
            c.trim * ((1.0 - c.mix) * dl + c.mix * wl * g),
            c.trim * ((1.0 - c.mix) * dr + c.mix * wr * g),
        )
    }
}

/// One base-rate sample of a waveshaping mode, at 2x: up (polyphase halfband), shaped twice,
/// down (halfband, decimated). Late by `LATENCY` samples (`oversample`).
#[inline(always)]
fn oversample(
    s: &mut FxChannel,
    x: f32,
    c: &LoopFxParams,
    mode: u8,
    taps: &[f32; HALFBAND_TAPS],
) -> f32 {
    s.input.push(x);
    let run = s.input.run();
    let mut even: f32 = 0.0;
    for k in 0..HALFBAND_TAPS {
        even += taps[k] * run[k];
    }
    even *= 2.0;
    let odd = run[11];
    s.even.push(shape(even * c.drive, c, mode));
    s.odd.push(shape(odd * c.drive, c, mode));
    let e = s.even.run();
    let mut y: f32 = 0.0;
    for k in 0..HALFBAND_TAPS {
        y += taps[k] * e[k];
    }
    y + 0.5 * s.odd.at(12)
}

#[inline(always)]
fn shape(u: f32, c: &LoopFxParams, mode: u8) -> f32 {
    let v = u * c.pre;
    let s: f32 = match mode {
        // Swift: `tanhf(v)`, Apple's. The libm crate's `tanhf` lands 1 ulp off it in many samples;
        // the f64 `tanh` rounded to f32 is closer (see tests/loops_golden.rs for the measure)
        1 => (libm::tanh(v as f64) as f32) * c.norm,
        2 => v.clamp(-1.0, 1.0),
        3 => (tube(v as f64) as f32) * c.norm,
        _ => fold(v),
    };
    (1.0 - c.amount) * u + c.amount * s
}

/// TPT state-variable lowpass, Q = 1/sqrt 2 (`lowpass`). Swift's literal `1.41421356`, not
/// `SQRT_2`, so the coefficients match bit for bit.
#[allow(clippy::approx_constant, clippy::excessive_precision)]
#[inline(always)]
fn lowpass(s: &mut FxChannel, x: f32, g: f32) -> f32 {
    let k: f32 = 1.41421356;
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = x - s.ic2;
    let v1 = a1 * s.ic1 + a2 * v3;
    let v2 = s.ic2 + a2 * s.ic1 + a3 * v3;
    s.ic1 = 2.0 * v1 - s.ic1;
    s.ic2 = 2.0 * v2 - s.ic2;
    v2
}

#[inline(always)]
fn dc_block(s: &mut FxChannel, x: f32, r: f32) -> f32 {
    let y = x - s.dc_x + r * s.dc_y;
    s.dc_x = x;
    s.dc_y = y;
    y
}

/// The loop channel's safety limiter (`LoopFxLimiter`): transparent at or under -0.5 dBFS,
/// instant attack (no look-ahead: the channel keeps its timing), 120 ms release. The bank runs
/// it only while some voice's FX are on or its gain is still coming back up, so a row without
/// FX is untouched.
pub struct LoopFxLimiter {
    /// The gain (f64: an f32 stalls short of 1 on a slow release).
    gain: f64,
    release: f64,
}

impl LoopFxLimiter {
    /// -0.5 dBFS.
    pub const CEILING: f32 = 0.944;

    pub fn new(sample_rate: f64) -> LoopFxLimiter {
        LoopFxLimiter {
            gain: 1.0,
            release: 1.0 - libm::exp(-1.0 / (0.12 * sample_rate)),
        }
    }

    pub fn gain(&self) -> f64 {
        self.gain
    }

    pub fn idle(&self) -> bool {
        self.gain >= 1.0
    }

    pub fn process(&mut self, l: &mut [f32], mut r: Option<&mut [f32]>, n: usize) {
        let mut g = self.gain;
        let t = Self::CEILING as f64;
        for i in 0..n {
            let rv = r.as_deref().map_or(0.0, |r| r[i].abs());
            let p = l[i].abs().max(rv) as f64;
            if g < 1.0 {
                g += self.release * (1.0 - g);
                if g > 0.999_999 {
                    g = 1.0;
                }
            }
            if p * g > t {
                g = t / p;
            }
            if g < 1.0 {
                let f = g as f32;
                l[i] *= f;
                if let Some(r) = r.as_deref_mut() {
                    r[i] *= f;
                }
            }
        }
        self.gain = g;
    }
}
