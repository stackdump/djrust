//! Insert effects: a small fixed chain on one strip (a deck, an input strip or the master),
//! filter -> crush -> delay -> reverb, each with an on switch, its own knobs and a wet/dry mix.
//!
//! Every knob is 0...1 (see [`InsertParam`] for what each maps to, [`InsertParam::display`] for
//! the words a panel shows). The chain is built on the main thread ([`InsertChain::new`]
//! allocates: the delay's lines and the room) and runs on the render thread:
//! [`InsertChain::process`] allocates nothing, takes no lock and makes no system call.
//!
//! No clicks: every knob is smoothed per sample (a one-pole, [`SMOOTH_SECONDS`]; the delay time
//! glides over [`GLIDE_SECONDS`], like tape), the filter's coefficients follow its smoothed
//! corner every [`COEF_EVERY`] frames, and an effect switched on or off fades its share in or out
//! over [`FADE_SECONDS`]. The delay and the reverb let their tails ring out when switched off
//! (the send closes, the return stays); once a tail is silent the effect stops costing anything.
//! An effect that is off and silent is skipped; a chain with everything off costs a branch.
//!
//! The delay is tempo synced: its time is a division of a beat ([`DIVISIONS`]) at the BPM the
//! owner hands it each block ([`InsertChain::set_bpm`]), up to [`MAX_DELAY_SECONDS`].

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use alloc::format;
use core::f64::consts::PI;
use libm::{exp, pow, tan};

use super::master_chain::{crush_quantize, crush_step};
use super::strip_reverb::{decay_seconds, RETURN_BASE, RETURN_EXPONENT};
use crate::json::{JsonObject, JsonValue};
use crate::musicality::space::{FdnReverb, SpaceSpec};

/// A knob's one-pole smoothing time constant.
pub const SMOOTH_SECONDS: f64 = 0.02;
/// The delay time's glide for a small change (the tempo drifting: a slight bend of pitch).
pub const GLIDE_SECONDS: f64 = 0.08;
/// A bigger change of the delay time (a new division, a tempo jump) crossfades from the old
/// tap to the new over this long instead: no pitch sweep, no click.
pub const XFADE_SECONDS: f64 = 0.05;
/// Changes of the delay time up to this fraction glide; larger ones crossfade.
pub const GLIDE_MAX: f32 = 0.06;
/// An effect switched on or off fades over this long.
pub const FADE_SECONDS: f64 = 0.015;
/// The filter's coefficients are recomputed every this many frames.
pub const COEF_EVERY: usize = 16;
/// The longest delay line: one 4/4 bar down to 80 BPM.
pub const MAX_DELAY_SECONDS: f64 = 3.0;
/// The delay divisions, in beats, and their names.
pub const DIVISIONS: [(f64, &str); 8] = [
    (0.25, "1/16"),
    (0.5, "1/8"),
    (0.75, "3/16"),
    (1.0, "1/4"),
    (1.5, "3/8"),
    (2.0, "1/2"),
    (3.0, "3/4"),
    (4.0, "1 bar"),
];

/// Below this a signal is silence (the tails' idle test).
const SILENT: f32 = 1.0e-5;
/// Added (alternating sign) into feedback paths so a decaying tail never goes subnormal.
const GUARD: f32 = 1.0e-20;

/// One effect of the chain, in signal order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InsertKind {
    Filter,
    Crush,
    Delay,
    Reverb,
}

impl InsertKind {
    pub const ALL: [InsertKind; 4] =
        [InsertKind::Filter, InsertKind::Crush, InsertKind::Delay, InsertKind::Reverb];

    pub fn id(self) -> &'static str {
        match self {
            InsertKind::Filter => "filter",
            InsertKind::Crush => "crush",
            InsertKind::Delay => "delay",
            InsertKind::Reverb => "reverb",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InsertKind::Filter => "Filter",
            InsertKind::Crush => "Bitcrush",
            InsertKind::Delay => "Delay",
            InsertKind::Reverb => "Reverb",
        }
    }

    pub fn from_id(id: &str) -> Option<InsertKind> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }

    /// Its knobs, in panel order (the mix last).
    pub fn params(self) -> &'static [InsertParam] {
        use InsertParam::*;
        match self {
            InsertKind::Filter => &[FilterSweep, FilterRes, FilterMix],
            InsertKind::Crush => &[CrushBits, CrushRate, CrushMix],
            InsertKind::Delay => &[DelayTime, DelayFeedback, DelayTone, DelaySpread, DelayMix],
            InsertKind::Reverb => &[ReverbSize, ReverbTone, ReverbMix],
        }
    }
}

/// A knob, 0...1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InsertParam {
    /// One-knob DJ filter: 0.5 open, down a low-pass to 60 Hz, up a high-pass to 8 kHz.
    FilterSweep,
    /// Resonance, Q 0.7 to 10 (eased in away from the open middle).
    FilterRes,
    FilterMix,
    /// 16 bits down to 2.
    CrushBits,
    /// Sample-rate reduction: hold each sample 1 to 32 frames.
    CrushRate,
    CrushMix,
    /// A division of the beat ([`DIVISIONS`], the knob picks the nearest).
    DelayTime,
    /// 0 to 95 % fed back.
    DelayFeedback,
    /// The repeats' low-pass, 1 kHz to 18 kHz.
    DelayTone,
    /// 0 a straight stereo delay, 1 ping-pong.
    DelaySpread,
    DelayMix,
    /// Decay 0.25 s to 8 s (as the strip reverb's size).
    ReverbSize,
    /// The room's damping, dark to bright.
    ReverbTone,
    ReverbMix,
}

impl InsertParam {
    pub const ALL: [InsertParam; 14] = [
        InsertParam::FilterSweep,
        InsertParam::FilterRes,
        InsertParam::FilterMix,
        InsertParam::CrushBits,
        InsertParam::CrushRate,
        InsertParam::CrushMix,
        InsertParam::DelayTime,
        InsertParam::DelayFeedback,
        InsertParam::DelayTone,
        InsertParam::DelaySpread,
        InsertParam::DelayMix,
        InsertParam::ReverbSize,
        InsertParam::ReverbTone,
        InsertParam::ReverbMix,
    ];
    pub const COUNT: usize = 14;

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn kind(self) -> InsertKind {
        use InsertParam::*;
        match self {
            FilterSweep | FilterRes | FilterMix => InsertKind::Filter,
            CrushBits | CrushRate | CrushMix => InsertKind::Crush,
            DelayTime | DelayFeedback | DelayTone | DelaySpread | DelayMix => InsertKind::Delay,
            ReverbSize | ReverbTone | ReverbMix => InsertKind::Reverb,
        }
    }

    /// "filter.sweep", "delay.mix", …
    pub fn id(self) -> &'static str {
        use InsertParam::*;
        match self {
            FilterSweep => "filter.sweep",
            FilterRes => "filter.res",
            FilterMix => "filter.mix",
            CrushBits => "crush.bits",
            CrushRate => "crush.rate",
            CrushMix => "crush.mix",
            DelayTime => "delay.time",
            DelayFeedback => "delay.feedback",
            DelayTone => "delay.tone",
            DelaySpread => "delay.spread",
            DelayMix => "delay.mix",
            ReverbSize => "reverb.size",
            ReverbTone => "reverb.tone",
            ReverbMix => "reverb.mix",
        }
    }

    pub fn from_id(id: &str) -> Option<InsertParam> {
        Self::ALL.iter().copied().find(|p| p.id() == id)
    }

    pub fn label(self) -> &'static str {
        use InsertParam::*;
        match self {
            FilterSweep => "Sweep",
            FilterRes => "Resonance",
            CrushBits => "Bits",
            CrushRate => "Rate",
            DelayTime => "Time",
            DelayFeedback => "Feedback",
            DelayTone | ReverbTone => "Tone",
            DelaySpread => "Ping-pong",
            ReverbSize => "Size",
            FilterMix | CrushMix | DelayMix | ReverbMix => "Wet",
        }
    }

    /// Where a new chain starts.
    pub fn default_value(self) -> f64 {
        use InsertParam::*;
        match self {
            FilterSweep => 0.5,
            FilterRes => 0.3,
            FilterMix => 1.0,
            CrushBits => 0.6,
            CrushRate => 0.3,
            CrushMix => 1.0,
            DelayTime => division_knob(2),
            DelayFeedback => 0.45,
            DelayTone => 0.6,
            DelaySpread => 0.5,
            DelayMix => 0.35,
            ReverbSize => 0.5,
            ReverbTone => 0.5,
            ReverbMix => 0.3,
        }
    }

    /// Discrete knobs snap (the panel shows steps).
    pub fn steps(self) -> u32 {
        match self {
            InsertParam::DelayTime => DIVISIONS.len() as u32,
            _ => 0,
        }
    }

    /// What the knob reads as: "LP 1.2 kHz", "3/16", "8 bit", "45 %".
    pub fn display(self, v: f64) -> String {
        use InsertParam::*;
        let v = clamp01(v);
        match self {
            FilterSweep => match sweep_corners(v) {
                (Some(lp), _) if lp < 19_000.0 => format!("LP {}", hz(lp)),
                (_, Some(hp)) if hp > 21.0 => format!("HP {}", hz(hp)),
                _ => String::from("open"),
            },
            FilterRes => format!("Q {:.1}", res_q(v)),
            CrushBits => format!("{:.0} bit", crush_bits(v)),
            CrushRate => format!("÷{:.0}", crush_hold(v)),
            DelayTime => String::from(DIVISIONS[division_of(v)].1),
            DelayFeedback => format!("{:.0} %", delay_feedback(v) * 100.0),
            DelayTone => hz(delay_tone_hz(v)),
            ReverbSize => format!("{:.1} s", decay_seconds(v)),
            _ => format!("{:.0} %", v * 100.0),
        }
    }
}

fn hz(f: f64) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{:.0} Hz", f)
    }
}

fn clamp01(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// The knob value of division `i`.
pub fn division_knob(i: usize) -> f64 {
    i.min(DIVISIONS.len() - 1) as f64 / (DIVISIONS.len() - 1) as f64
}

/// The division a knob value picks.
pub fn division_of(v: f64) -> usize {
    let n = DIVISIONS.len() - 1;
    ((clamp01(v) * n as f64) + 0.5) as usize
}

/// The sweep knob's corners: (low-pass, high-pass). Both are always running; open is
/// LP 20 kHz + HP 20 Hz, so crossing the middle is seamless.
pub fn sweep_corners(v: f64) -> (Option<f64>, Option<f64>) {
    let v = clamp01(v);
    const DEAD: f64 = 0.02;
    if v < 0.5 - DEAD {
        let t = (0.5 - DEAD - v) / (0.5 - DEAD);
        (Some(20_000.0 * pow(60.0 / 20_000.0, t)), None)
    } else if v > 0.5 + DEAD {
        let t = (v - 0.5 - DEAD) / (0.5 - DEAD);
        (None, Some(20.0 * pow(8_000.0 / 20.0, t)))
    } else {
        (None, None)
    }
}

fn res_q(v: f64) -> f64 {
    0.707 + clamp01(v) * clamp01(v) * 9.3
}
fn crush_bits(v: f64) -> f64 {
    16.0 - 14.0 * clamp01(v)
}
fn crush_hold(v: f64) -> f64 {
    pow(32.0, clamp01(v))
}
fn delay_feedback(v: f64) -> f64 {
    0.95 * clamp01(v)
}
fn delay_tone_hz(v: f64) -> f64 {
    1_000.0 * pow(18.0, clamp01(v))
}
fn reverb_damping_hz(v: f64) -> f64 {
    1_500.0 * pow(12.0, clamp01(v))
}

/// One knob's per-sample one-pole glide.
#[derive(Clone, Copy, Debug)]
struct Smooth {
    cur: f32,
    target: f32,
    coef: f32,
}

impl Smooth {
    fn new(v: f64, seconds: f64, sample_rate: f64) -> Smooth {
        Smooth {
            cur: v as f32,
            target: v as f32,
            coef: (1.0 - exp(-1.0 / (seconds * sample_rate))) as f32,
        }
    }
    #[inline]
    fn next(&mut self) -> f32 {
        self.cur += (self.target - self.cur) * self.coef;
        if (self.target - self.cur).abs() < 1.0e-6 {
            self.cur = self.target;
        }
        self.cur
    }
    fn settled(&self) -> bool {
        self.cur == self.target
    }
}

/// The on switch's fade, linear.
#[derive(Clone, Copy, Debug)]
struct Fade {
    on: bool,
    level: f32,
    step: f32,
}

impl Fade {
    fn new(sample_rate: f64) -> Fade {
        Fade { on: false, level: 0.0, step: (1.0 / (FADE_SECONDS * sample_rate)) as f32 }
    }
    #[inline]
    fn next(&mut self) -> f32 {
        if self.on {
            if self.level < 1.0 {
                self.level = (self.level + self.step).min(1.0);
            }
        } else if self.level > 0.0 {
            self.level = (self.level - self.step).max(0.0);
        }
        self.level
    }
    /// Fully off.
    fn idle(&self) -> bool {
        !self.on && self.level == 0.0
    }
}

/// A TPT state-variable filter's coefficients (Zavalishin / Simper): modulation-stable.
#[derive(Clone, Copy, Debug, Default)]
struct SvfCoef {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoef {
    fn new(fc: f64, q: f64, sample_rate: f64) -> SvfCoef {
        let fc = fc.clamp(10.0, sample_rate * 0.45);
        let g = tan(PI * fc / sample_rate);
        let k = 1.0 / q.max(0.5);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        SvfCoef { k: k as f32, a1: a1 as f32, a2: a2 as f32, a3: (g * a2) as f32 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SvfState {
    ic1: f32,
    ic2: f32,
}

impl SvfState {
    /// (low-pass, high-pass).
    #[inline]
    fn tick(&mut self, c: &SvfCoef, x: f32) -> (f32, f32) {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, x - c.k * v1 - v2)
    }
}

struct Filter {
    fade: Fade,
    sweep: Smooth,
    res: Smooth,
    mix: Smooth,
    lp: SvfCoef,
    hp: SvfCoef,
    lp_s: [SvfState; 2],
    hp_s: [SvfState; 2],
    count: usize,
}

impl Filter {
    fn new(sr: f64) -> Filter {
        let mut f = Filter {
            fade: Fade::new(sr),
            sweep: Smooth::new(0.5, SMOOTH_SECONDS, sr),
            res: Smooth::new(0.3, SMOOTH_SECONDS, sr),
            mix: Smooth::new(1.0, SMOOTH_SECONDS, sr),
            lp: SvfCoef::default(),
            hp: SvfCoef::default(),
            lp_s: [SvfState::default(); 2],
            hp_s: [SvfState::default(); 2],
            count: 0,
        };
        f.coefs(sr);
        f
    }

    fn coefs(&mut self, sr: f64) {
        let v = self.sweep.cur as f64;
        let (lp, hp) = sweep_corners(v);
        // resonance only away from the open middle (no ringing peak at 20 kHz / 20 Hz)
        let away = ((v - 0.5).abs() * 8.0).min(1.0);
        let q = 0.707 + (res_q(self.res.cur as f64) - 0.707) * away;
        self.lp = SvfCoef::new(lp.unwrap_or(20_000.0), if lp.is_some() { q } else { 0.707 }, sr);
        self.hp = SvfCoef::new(hp.unwrap_or(20.0), if hp.is_some() { q } else { 0.707 }, sr);
    }

    fn reset(&mut self) {
        self.lp_s = [SvfState::default(); 2];
        self.hp_s = [SvfState::default(); 2];
    }

    fn process(&mut self, sr: f64, l: &mut [f32], r: &mut [f32]) {
        if self.fade.idle() {
            return;
        }
        for i in 0..l.len() {
            if self.count == 0 {
                self.coefs(sr);
            }
            self.count = (self.count + 1) % COEF_EVERY;
            self.sweep.next();
            self.res.next();
            let m = self.mix.next() * self.fade.next();
            let (xl, xr) = (l[i], r[i]);
            let yl = self.hp_s[0].tick(&self.hp, self.lp_s[0].tick(&self.lp, xl).0).1;
            let yr = self.hp_s[1].tick(&self.hp, self.lp_s[1].tick(&self.lp, xr).0).1;
            l[i] = xl + m * (yl - xl);
            r[i] = xr + m * (yr - xr);
        }
        if self.fade.idle() {
            self.reset();
        }
    }
}

struct Crush {
    fade: Fade,
    bits: Smooth,
    mix: Smooth,
    hold: f32,
    phase: f32,
    held: [f32; 2],
}

impl Crush {
    fn new(sr: f64) -> Crush {
        Crush {
            fade: Fade::new(sr),
            bits: Smooth::new(crush_bits(0.6), SMOOTH_SECONDS, sr),
            mix: Smooth::new(1.0, SMOOTH_SECONDS, sr),
            hold: crush_hold(0.3) as f32,
            phase: 0.0,
            held: [0.0; 2],
        }
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.fade.idle() {
            return;
        }
        let mut step = crush_step(self.bits.cur as f64);
        for i in 0..l.len() {
            if !self.bits.settled() {
                step = crush_step(self.bits.next() as f64);
            }
            let m = self.mix.next() * self.fade.next();
            let (xl, xr) = (l[i], r[i]);
            self.phase += 1.0;
            if self.phase >= self.hold {
                self.phase -= self.hold;
                self.held[0] = crush_quantize(xl as f64, step) as f32;
                self.held[1] = crush_quantize(xr as f64, step) as f32;
            }
            l[i] = xl + m * (self.held[0] - xl);
            r[i] = xr + m * (self.held[1] - xr);
        }
    }
}

struct Delay {
    fade: Fade,
    line: [Vec<f32>; 2],
    pos: usize,
    /// The delay in frames, gliding (the tap heard, or the old one while a crossfade runs).
    time: Smooth,
    /// A crossfade in progress: the new tap's time, and how far along (0...1).
    next: Option<f32>,
    xfade: f32,
    xfade_step: f32,
    /// A time asked for while a crossfade runs: taken up when it ends.
    queued: Option<f32>,
    feedback: Smooth,
    spread: Smooth,
    mix: Smooth,
    tone_a: f32,
    tone_s: [f32; 2],
    /// Frames written with the send closed and nothing left above [`SILENT`].
    quiet: usize,
    tail: bool,
    guard: f32,
}

impl Delay {
    fn new(sr: f64) -> Delay {
        let len = (MAX_DELAY_SECONDS * sr) as usize + 4;
        let mut d = Delay {
            fade: Fade::new(sr),
            line: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            time: Smooth::new(0.375 * sr, GLIDE_SECONDS, sr),
            next: None,
            xfade: 0.0,
            xfade_step: (1.0 / (XFADE_SECONDS * sr)) as f32,
            queued: None,
            feedback: Smooth::new(delay_feedback(0.45), SMOOTH_SECONDS, sr),
            spread: Smooth::new(0.5, SMOOTH_SECONDS, sr),
            mix: Smooth::new(0.35, SMOOTH_SECONDS, sr),
            tone_a: 0.0,
            tone_s: [0.0; 2],
            quiet: 0,
            tail: false,
            guard: GUARD,
        };
        d.set_tone(0.6, sr);
        d
    }

    fn set_tone(&mut self, v: f64, sr: f64) {
        self.tone_a = (1.0 - exp(-2.0 * PI * delay_tone_hz(v) / sr)) as f32;
    }

    fn set_time(&mut self, beats: f64, bpm: f64, sr: f64, snap: bool) {
        let len = self.line[0].len();
        let frames = (beats * 60.0 / bpm.clamp(20.0, 400.0) * sr).clamp(1.0, (len - 3) as f64) as f32;
        if snap {
            self.time.cur = frames;
            self.time.target = frames;
            self.next = None;
            self.queued = None;
        } else if self.next.is_some() {
            self.queued = Some(frames);
        } else {
            self.retarget(frames);
        }
    }

    /// Glide to `frames` when it is near, else crossfade to a tap there.
    fn retarget(&mut self, frames: f32) {
        let cur = self.time.cur;
        if (frames - cur).abs() <= GLIDE_MAX * cur {
            self.time.target = frames;
        } else {
            self.time.target = cur;
            self.next = Some(frames);
            self.xfade = 0.0;
        }
    }

    #[inline]
    fn read(line: &[f32], pos: usize, d: f32) -> f32 {
        let len = line.len();
        let back = d.max(1.0);
        let whole = back as usize;
        let frac = back - whole as f32;
        let a = (pos + len - whole) % len;
        let b = (a + len - 1) % len;
        line[a] + frac * (line[b] - line[a])
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.fade.idle() && !self.tail {
            return;
        }
        let len = self.line[0].len();
        let mut loud = false;
        for i in 0..l.len() {
            let send = self.fade.next();
            let d = self.time.next();
            let fb = self.feedback.next();
            let s = self.spread.next();
            let mix = self.mix.next();
            let (xl, xr) = (l[i], r[i]);
            let mut wl = Self::read(&self.line[0], self.pos, d);
            let mut wr = Self::read(&self.line[1], self.pos, d);
            if let Some(dn) = self.next {
                self.xfade = (self.xfade + self.xfade_step).min(1.0);
                let g = self.xfade;
                wl += g * (Self::read(&self.line[0], self.pos, dn) - wl);
                wr += g * (Self::read(&self.line[1], self.pos, dn) - wr);
                if g >= 1.0 {
                    self.time.cur = dn;
                    self.time.target = dn;
                    self.next = None;
                    if let Some(q) = self.queued.take() {
                        self.retarget(q);
                    }
                }
            }
            self.tone_s[0] += self.tone_a * (wl - self.tone_s[0]);
            self.tone_s[1] += self.tone_a * (wr - self.tone_s[1]);
            let (fl, fr) = (self.tone_s[0], self.tone_s[1]);
            let mono = 0.5 * (xl + xr);
            let in_l = xl + s * (mono - xl);
            let in_r = xr * (1.0 - s);
            self.guard = -self.guard;
            let nl = send * in_l + fb * (fl + s * (fr - fl)) + self.guard;
            let nr = send * in_r + fb * (fr + s * (fl - fr)) + self.guard;
            self.line[0][self.pos] = nl;
            self.line[1][self.pos] = nr;
            self.pos = if self.pos + 1 == len { 0 } else { self.pos + 1 };
            if nl.abs() > SILENT || nr.abs() > SILENT || wl.abs() > SILENT || wr.abs() > SILENT {
                loud = true;
            }
            // the dry comes back as the fade closes; the return rings out at the mix
            l[i] = xl * (1.0 - mix * send) + mix * wl;
            r[i] = xr * (1.0 - mix * send) + mix * wr;
        }
        if self.fade.idle() {
            // a tail: silent for a whole line's length, then it is over
            self.quiet = if loud { 0 } else { self.quiet + l.len() };
            self.tail = self.quiet < len;
        } else {
            self.quiet = 0;
            self.tail = true;
        }
    }
}

struct Reverb {
    fade: Fade,
    room: FdnReverb,
    mix: Smooth,
    size: f64,
    tone: f64,
    dirty: bool,
    /// Frames of tail left after the send closed.
    tail: usize,
    guard: f64,
}

impl Reverb {
    fn new(sr: f64) -> Reverb {
        let mut r = Reverb {
            fade: Fade::new(sr),
            room: FdnReverb::with_input_highpass(sr, false),
            mix: Smooth::new(0.3, SMOOTH_SECONDS, sr),
            size: 0.5,
            tone: 0.5,
            dirty: true,
            tail: 0,
            guard: GUARD as f64,
        };
        r.tune();
        r
    }

    fn tune(&mut self) {
        let seconds = decay_seconds(self.size);
        let spec = SpaceSpec {
            seconds,
            // the strip reverb's level-vs-decay fit, so a long room is not just louder; x2:
            // at full wet the return is about as loud as the dry
            wet: 2.0 * RETURN_BASE * pow(seconds, RETURN_EXPONENT),
            damping: reverb_damping_hz(self.tone),
            shimmer: 0.0,
        };
        self.room.tune(Some(&spec));
        self.dirty = false;
    }

    fn process(&mut self, sr: f64, l: &mut [f32], r: &mut [f32]) {
        if self.fade.idle() && self.tail == 0 {
            return;
        }
        if self.dirty {
            self.tune();
        }
        for i in 0..l.len() {
            let send = self.fade.next();
            let mix = self.mix.next();
            let (xl, xr) = (l[i], r[i]);
            self.guard = -self.guard;
            let x = 0.5 * (xl as f64 + xr as f64) * send as f64 + self.guard;
            let (rl, rr) = self.room.process(x);
            l[i] = xl * (1.0 - mix * send) + mix * rl as f32;
            r[i] = xr * (1.0 - mix * send) + mix * rr as f32;
        }
        if self.fade.idle() {
            self.tail = self.tail.saturating_sub(l.len());
            if self.tail == 0 {
                self.room.reset();
            }
        } else {
            // two decays after the send closes: 120 dB down
            self.tail = (2.0 * decay_seconds(self.size) * sr) as usize + l.len();
        }
    }
}

/// The settings of one chain: the switches and the knobs. What the main thread keeps, persists
/// and restores; [`InsertChain::apply`] makes a chain match one.
#[derive(Clone, Debug, PartialEq)]
pub struct InsertSettings {
    pub on: [bool; 4],
    pub values: [f64; InsertParam::COUNT],
}

impl Default for InsertSettings {
    fn default() -> Self {
        let mut values = [0.0; InsertParam::COUNT];
        for p in InsertParam::ALL {
            values[p.index()] = p.default_value();
        }
        InsertSettings { on: [false; 4], values }
    }
}

impl InsertSettings {
    pub fn get(&self, p: InsertParam) -> f64 {
        self.values[p.index()]
    }
    pub fn set(&mut self, p: InsertParam, v: f64) {
        self.values[p.index()] = clamp01(v);
    }
    pub fn is_on(&self, k: InsertKind) -> bool {
        self.on[k as usize]
    }
    pub fn set_on(&mut self, k: InsertKind, on: bool) {
        self.on[k as usize] = on;
    }
    pub fn any_on(&self) -> bool {
        self.on.iter().any(|&o| o)
    }

    /// `{"on": ["delay"], "delay.mix": 0.4, …}`: every knob, the switches that are on.
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        let on: Vec<JsonValue> = InsertKind::ALL
            .iter()
            .filter(|k| self.is_on(**k))
            .map(|k| JsonValue::String(k.id().into()))
            .collect();
        o.set("on", JsonValue::Array(on));
        for p in InsertParam::ALL {
            o.set(p.id(), JsonValue::Number(self.get(p)));
        }
        JsonValue::Object(o)
    }

    /// The defaults, with whatever `v` names over them (unknown keys are ignored).
    pub fn from_json(v: Option<&JsonValue>) -> InsertSettings {
        let mut s = InsertSettings::default();
        let Some(v) = v else { return s };
        if let Some(on) = v.get("on").and_then(JsonValue::as_array) {
            for k in on.iter().filter_map(JsonValue::as_str).filter_map(InsertKind::from_id) {
                s.set_on(k, true);
            }
        }
        for p in InsertParam::ALL {
            if let Some(x) = v.get(p.id()).and_then(JsonValue::as_f64) {
                s.set(p, x);
            }
        }
        s
    }
}

/// One strip's chain, render-thread side.
pub struct InsertChain {
    sample_rate: f64,
    bpm: f64,
    delay_beats: f64,
    filter: Filter,
    crush: Crush,
    delay: Delay,
    reverb: Reverb,
}

impl InsertChain {
    /// A chain with every effect off and the default knobs. Allocates (main thread): about
    /// `MAX_DELAY_SECONDS` of stereo f32 plus the room's lines.
    pub fn new(sample_rate: f64) -> InsertChain {
        let mut c = InsertChain {
            sample_rate,
            bpm: 120.0,
            delay_beats: DIVISIONS[2].0,
            filter: Filter::new(sample_rate),
            crush: Crush::new(sample_rate),
            delay: Delay::new(sample_rate),
            reverb: Reverb::new(sample_rate),
        };
        c.apply(&InsertSettings::default(), true);
        c
    }

    /// Match `s`; `snap`: the knobs jump there (a fresh chain), else they glide.
    pub fn apply(&mut self, s: &InsertSettings, snap: bool) {
        for p in InsertParam::ALL {
            self.set(p, s.get(p));
        }
        for k in InsertKind::ALL {
            self.set_on(k, s.is_on(k));
        }
        if snap {
            for sm in [
                &mut self.filter.sweep,
                &mut self.filter.res,
                &mut self.filter.mix,
                &mut self.crush.bits,
                &mut self.crush.mix,
                &mut self.delay.feedback,
                &mut self.delay.spread,
                &mut self.delay.mix,
                &mut self.reverb.mix,
            ] {
                sm.cur = sm.target;
            }
            self.filter.coefs(self.sample_rate);
            let t = self.delay.queued.take().or(self.delay.next.take()).unwrap_or(self.delay.time.target);
            self.delay.time.cur = t;
            self.delay.time.target = t;
        }
    }

    /// Switch an effect on or off (it fades). Real-time safe.
    pub fn set_on(&mut self, k: InsertKind, on: bool) {
        match k {
            InsertKind::Filter => self.filter.fade.on = on,
            InsertKind::Crush => self.crush.fade.on = on,
            InsertKind::Delay => {
                if on && self.delay.fade.idle() && !self.delay.tail {
                    // from silence: no glide in from wherever the time was
                    let t = self.delay.queued.take().or(self.delay.next.take()).unwrap_or(self.delay.time.target);
                    self.delay.time.cur = t;
                    self.delay.time.target = t;
                }
                self.delay.fade.on = on
            }
            InsertKind::Reverb => self.reverb.fade.on = on,
        }
    }

    /// Move a knob (0...1; it glides). Real-time safe.
    pub fn set(&mut self, p: InsertParam, v: f64) {
        let v = clamp01(v);
        let sr = self.sample_rate;
        use InsertParam::*;
        match p {
            FilterSweep => self.filter.sweep.target = v as f32,
            FilterRes => self.filter.res.target = v as f32,
            FilterMix => self.filter.mix.target = v as f32,
            CrushBits => self.crush.bits.target = crush_bits(v) as f32,
            CrushRate => self.crush.hold = crush_hold(v) as f32,
            CrushMix => self.crush.mix.target = v as f32,
            DelayTime => {
                self.delay_beats = DIVISIONS[division_of(v)].0;
                self.delay.set_time(self.delay_beats, self.bpm, sr, false);
            }
            DelayFeedback => self.delay.feedback.target = delay_feedback(v) as f32,
            DelayTone => self.delay.set_tone(v, sr),
            DelaySpread => self.delay.spread.target = v as f32,
            DelayMix => self.delay.mix.target = v as f32,
            ReverbSize => {
                self.reverb.size = v;
                self.reverb.dirty = true;
            }
            ReverbTone => {
                self.reverb.tone = v;
                self.reverb.dirty = true;
            }
            ReverbMix => self.reverb.mix.target = v as f32,
        }
    }

    /// The tempo the delay follows (ignored unless 20...400). Real-time safe; cheap to call
    /// every block.
    pub fn set_bpm(&mut self, bpm: f64) {
        if !(20.0..=400.0).contains(&bpm) || bpm == self.bpm {
            return;
        }
        self.bpm = bpm;
        self.delay.set_time(self.delay_beats, bpm, self.sample_rate, false);
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    /// The delay's time now, seconds (where its glide or crossfade is heading).
    pub fn delay_seconds(&self) -> f64 {
        let d = &self.delay;
        d.queued.or(d.next).unwrap_or(d.time.target) as f64 / self.sample_rate
    }

    /// Anything sounding or fading: false costs [`process`](Self::process) a few branches.
    pub fn is_active(&self) -> bool {
        !self.filter.fade.idle()
            || !self.crush.fade.idle()
            || !self.delay.fade.idle()
            || self.delay.tail
            || !self.reverb.fade.idle()
            || self.reverb.tail > 0
    }

    /// In place, both channels the same length. Real-time safe: no allocation, no lock.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let (l, r) = (&mut l[..n], &mut r[..n]);
        let sr = self.sample_rate;
        self.filter.process(sr, l, r);
        self.crush.process(l, r);
        self.delay.process(l, r);
        self.reverb.process(sr, l, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn sine(n: usize, hz: f64, at: usize) -> (Vec<f32>, Vec<f32>) {
        let v: Vec<f32> = (0..n)
            .map(|i| (0.5 * libm::sin(2.0 * PI * hz * (at + i) as f64 / SR)) as f32)
            .collect();
        (v.clone(), v)
    }

    fn rms(v: &[f32]) -> f64 {
        libm::sqrt(v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / v.len() as f64)
    }

    #[test]
    fn off_is_bit_exact() {
        let mut c = InsertChain::new(SR);
        let (mut l, mut r) = sine(512, 440.0, 0);
        let (l0, r0) = (l.clone(), r.clone());
        c.process(&mut l, &mut r);
        assert_eq!(l, l0);
        assert_eq!(r, r0);
        assert!(!c.is_active());
    }

    #[test]
    fn low_pass_cuts_highs_and_open_passes() {
        let mut c = InsertChain::new(SR);
        c.set_on(InsertKind::Filter, true);
        c.set(InsertParam::FilterSweep, 0.1);
        c.apply(&{
            let mut s = InsertSettings::default();
            s.set_on(InsertKind::Filter, true);
            s.set(InsertParam::FilterSweep, 0.1);
            s
        }, true);
        let mut at = 0;
        let mut out = 0.0;
        for _ in 0..40 {
            let (mut l, mut r) = sine(512, 5_000.0, at);
            c.process(&mut l, &mut r);
            out = rms(&l);
            at += 512;
        }
        assert!(out < 0.02, "5 kHz through a ~200 Hz low-pass: rms {out}");
        // the open middle passes (nearly) unchanged
        c.set(InsertParam::FilterSweep, 0.5);
        for _ in 0..40 {
            let (mut l, mut r) = sine(512, 5_000.0, at);
            c.process(&mut l, &mut r);
            out = rms(&l);
            at += 512;
        }
        assert!((out - 0.3535).abs() < 0.02, "open: rms {out}");
    }

    #[test]
    fn delay_repeats_at_the_synced_time() {
        let mut c = InsertChain::new(SR);
        let mut s = InsertSettings::default();
        s.set_on(InsertKind::Delay, true);
        s.set(InsertParam::DelayTime, division_knob(3)); // 1/4
        s.set(InsertParam::DelayMix, 1.0);
        s.set(InsertParam::DelaySpread, 0.0);
        s.set(InsertParam::DelayTone, 1.0);
        c.apply(&s, true);
        c.set_bpm(120.0);
        c.delay.time.cur = c.delay.time.target;
        // let the fade open, then an impulse
        let (mut l, mut r) = (vec![0.0f32; 2048], vec![0.0f32; 2048]);
        c.process(&mut l, &mut r);
        let n = 30_000;
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        l[0] = 1.0;
        r[0] = 1.0;
        c.process(&mut l, &mut r);
        let peak = (1..n).max_by(|a, b| l[*a].abs().total_cmp(&l[*b].abs())).unwrap();
        // a quarter at 120 BPM = 0.5 s
        assert!((peak as i64 - 24_000).abs() <= 2, "echo at {peak}");
        assert!((c.delay_seconds() - 0.5).abs() < 1e-3);
    }

    #[test]
    fn tails_end_and_the_chain_goes_idle() {
        let mut c = InsertChain::new(SR);
        c.set_on(InsertKind::Reverb, true);
        c.set_on(InsertKind::Delay, true);
        c.set(InsertParam::ReverbSize, 0.0);
        c.set(InsertParam::DelayFeedback, 0.3);
        let mut at = 0;
        for _ in 0..50 {
            let (mut l, mut r) = sine(512, 220.0, at);
            c.process(&mut l, &mut r);
            at += 512;
        }
        c.set_on(InsertKind::Reverb, false);
        c.set_on(InsertKind::Delay, false);
        let mut blocks = 0;
        while c.is_active() {
            let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
            c.process(&mut l, &mut r);
            blocks += 1;
            assert!(blocks < 2_000, "tails never ended");
        }
    }

    #[test]
    fn settings_round_trip_json() {
        let mut s = InsertSettings::default();
        s.set_on(InsertKind::Delay, true);
        s.set(InsertParam::FilterSweep, 0.2);
        let j = s.json();
        let back = InsertSettings::from_json(Some(&j));
        assert_eq!(back, s);
    }

    #[test]
    fn displays() {
        assert_eq!(InsertParam::DelayTime.display(division_knob(2)), "3/16");
        assert_eq!(InsertParam::FilterSweep.display(0.5), "open");
        assert!(InsertParam::FilterSweep.display(0.0).starts_with("LP 60"));
        assert!(InsertParam::FilterSweep.display(1.0).starts_with("HP 8.0 kHz"));
        assert_eq!(InsertParam::CrushBits.display(1.0), "2 bit");
    }
}
