//! Granular freeze (beyond Swift; docs/INPUT-HISTORY.md, "Freeze"): the last N beats of an
//! input's history held still and smeared into a cloud of grains.
//!
//! [`GrainFreeze::freeze`] copies the beats ending at the history's head into its own buffer
//! (once, at most its capacity); from then on grains are spawned [`FreezeSettings::density`]
//! times a second, each [`FreezeSettings::size_ms`] long under a Hann window, starting at the
//! freeze **position** (0..1 across the frozen beats) give or take
//! [`FreezeSettings::spread`], and played [`FreezeSettings::jitter`] semitones off at random.
//! The position stays where it is put: a jog wheel aimed at the history moves it
//! ([`GrainFreeze::scrub`]). Turning the freeze on or off crossfades with the live signal over
//! 10 ms; [`FreezeSettings::mix`] keeps some of the live signal under the cloud.
//!
//! Pure and real-time safe once built: the buffer and the grain pool are allocated up front,
//! and the random numbers come from a seeded xorshift, so a render is repeatable.

use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

use crate::history::History;
use crate::json::{JsonObject, JsonValue};

/// Grains sounding at once, at most.
pub const MAX_GRAINS: usize = 64;
/// The longest freeze, in beats.
pub const MAX_FREEZE_BEATS: f64 = 16.0;
/// Fade in / out, seconds.
const FADE_S: f64 = 0.010;

/// A freeze knob, in its own units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreezeParam {
    /// Beats frozen at the next freeze (0.25..16).
    Beats,
    /// Grain length, ms (10..500).
    Size,
    /// Grains a second (2..200).
    Density,
    /// How far from the position a grain may start, 0..1 of the frozen beats.
    Spread,
    /// Random pitch per grain, ± semitones (0..12).
    Jitter,
    /// Where grains start, 0..1 across the frozen beats.
    Position,
    /// The cloud against the live signal, 0..1.
    Mix,
}

impl FreezeParam {
    pub const ALL: [FreezeParam; 7] = [
        FreezeParam::Beats,
        FreezeParam::Size,
        FreezeParam::Density,
        FreezeParam::Spread,
        FreezeParam::Jitter,
        FreezeParam::Position,
        FreezeParam::Mix,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FreezeParam::Beats => "beats",
            FreezeParam::Size => "size",
            FreezeParam::Density => "density",
            FreezeParam::Spread => "spread",
            FreezeParam::Jitter => "jitter",
            FreezeParam::Position => "position",
            FreezeParam::Mix => "mix",
        }
    }

    pub fn parse(s: &str) -> Option<FreezeParam> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "beats" | "length" => FreezeParam::Beats,
            "size" | "grain" | "size_ms" | "grain_size" => FreezeParam::Size,
            "density" | "rate" => FreezeParam::Density,
            "spread" | "position_spread" => FreezeParam::Spread,
            "jitter" | "pitch_jitter" => FreezeParam::Jitter,
            "position" | "pos" => FreezeParam::Position,
            "mix" | "wet" => FreezeParam::Mix,
            _ => return None,
        })
    }

    pub fn range(self) -> (f64, f64) {
        match self {
            FreezeParam::Beats => (0.25, MAX_FREEZE_BEATS),
            FreezeParam::Size => (10.0, 500.0),
            FreezeParam::Density => (2.0, 200.0),
            FreezeParam::Jitter => (0.0, 12.0),
            _ => (0.0, 1.0),
        }
    }

    /// A knob's 0..1 travel in the param's units: beats step through 1/4 .. 16 by powers of
    /// two, size and density are logarithmic.
    pub fn from_knob(self, v: f64) -> f64 {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (lo, hi) = self.range();
        match self {
            FreezeParam::Beats => {
                // 0.25, 0.5, 1, 2, 4, 8, 16
                let k = libm::round(v * 6.0);
                0.25 * libm::pow(2.0, k)
            }
            FreezeParam::Size | FreezeParam::Density => lo * libm::pow(hi / lo, v),
            _ => lo + v * (hi - lo),
        }
    }

    pub fn to_knob(self, x: f64) -> f64 {
        let (lo, hi) = self.range();
        let x = x.clamp(lo, hi);
        match self {
            FreezeParam::Beats => libm::log2(x / 0.25) / 6.0,
            FreezeParam::Size | FreezeParam::Density => libm::log(x / lo) / libm::log(hi / lo),
            _ => (x - lo) / (hi - lo),
        }
    }
}

/// The freeze's knobs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreezeSettings {
    pub beats: f64,
    pub size_ms: f64,
    pub density: f64,
    pub spread: f64,
    pub jitter: f64,
    pub position: f64,
    pub mix: f64,
    /// Freeze and thaw on the next beat of the master clock (else at once).
    pub quantize: bool,
}

impl Default for FreezeSettings {
    fn default() -> Self {
        FreezeSettings {
            beats: 1.0,
            size_ms: 90.0,
            density: 30.0,
            spread: 0.15,
            jitter: 0.0,
            position: 0.5,
            mix: 1.0,
            quantize: true,
        }
    }
}

impl FreezeSettings {
    pub fn get(&self, p: FreezeParam) -> f64 {
        match p {
            FreezeParam::Beats => self.beats,
            FreezeParam::Size => self.size_ms,
            FreezeParam::Density => self.density,
            FreezeParam::Spread => self.spread,
            FreezeParam::Jitter => self.jitter,
            FreezeParam::Position => self.position,
            FreezeParam::Mix => self.mix,
        }
    }

    /// Set `p`, clamped (the position wraps). Returns the value kept, None for not a number.
    pub fn set(&mut self, p: FreezeParam, v: f64) -> Option<f64> {
        if !v.is_finite() {
            return None;
        }
        let (lo, hi) = p.range();
        let c = v.clamp(lo, hi);
        match p {
            FreezeParam::Beats => self.beats = c,
            FreezeParam::Size => self.size_ms = c,
            FreezeParam::Density => self.density = c,
            FreezeParam::Spread => self.spread = c,
            FreezeParam::Jitter => self.jitter = c,
            FreezeParam::Position => self.position = v - libm::floor(v),
            FreezeParam::Mix => self.mix = c,
        }
        Some(self.get(p))
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for p in FreezeParam::ALL {
            o.set(
                p.name(),
                JsonValue::from(libm::round(self.get(p) * 1000.0) / 1000.0),
            );
        }
        o.set("quantize", self.quantize.into());
        JsonValue::Object(o)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Grain {
    on: bool,
    pos: f64,
    inc: f64,
    age: usize,
    len: usize,
}

/// The freeze on one strip.
pub struct GrainFreeze {
    sample_rate: f64,
    buf: Vec<f32>,
    /// The right channel's frozen audio (empty: a mono freeze).
    buf_r: Vec<f32>,
    len: usize,
    grains: [Grain; MAX_GRAINS],
    rng: u32,
    /// Frames until the next grain.
    until: f64,
    s: FreezeSettings,
    /// The cloud's share of the output now, 0..1, chasing `want`.
    weight: f32,
    want: f32,
    step: f32,
}

impl GrainFreeze {
    /// A freeze whose buffer holds `capacity` frames (the most beats it can hold at the
    /// slowest tempo).
    pub fn new(sample_rate: f64, capacity: usize) -> GrainFreeze {
        GrainFreeze {
            sample_rate,
            buf: vec![0.0; capacity.max(2)],
            buf_r: Vec::new(),
            len: 0,
            grains: [Grain::default(); MAX_GRAINS],
            rng: 0x9E37_79B9,
            until: 0.0,
            s: FreezeSettings::default(),
            weight: 0.0,
            want: 0.0,
            step: (1.0 / (FADE_S * sample_rate)).min(1.0) as f32,
        }
    }

    /// A stereo freeze: both channels held, the same grains reading each (the image kept).
    pub fn new_stereo(sample_rate: f64, capacity: usize) -> GrainFreeze {
        let mut g = GrainFreeze::new(sample_rate, capacity);
        g.buf_r = vec![0.0; capacity.max(2)];
        g
    }

    pub fn settings(&self) -> &FreezeSettings {
        &self.s
    }

    /// Take new knobs (the grains sounding finish as they are).
    pub fn configure(&mut self, s: &FreezeSettings) {
        self.s = *s;
    }

    /// Freeze the `frames` ending at absolute index `end` of `hist` (as much as is held and fits)
    /// and fade the cloud in. False when nothing is recorded.
    pub fn freeze(&mut self, hist: &History, end: i64, frames: usize) -> bool {
        let held = (end - hist.oldest() as i64).max(0) as usize;
        let len = frames.min(self.buf.len()).min(held);
        if len < 64 {
            return false;
        }
        let start = end - len as i64;
        for k in 0..len {
            self.buf[k] = hist.get(start + k as i64);
        }
        self.start_cloud(len);
        true
    }

    /// [`freeze`](Self::freeze) of a stereo pair of histories (a [`new_stereo`](Self::new_stereo)
    /// freeze; a mono one takes the left).
    pub fn freeze_stereo(&mut self, left: &History, right: &History, end: i64, frames: usize) -> bool {
        let held = (end - left.oldest() as i64).max(0) as usize;
        let len = frames.min(self.buf.len()).min(held);
        if len < 64 {
            return false;
        }
        let start = end - len as i64;
        for k in 0..len {
            self.buf[k] = left.get(start + k as i64);
        }
        if !self.buf_r.is_empty() {
            for k in 0..len {
                self.buf_r[k] = right.get(start + k as i64);
            }
        }
        self.start_cloud(len);
        true
    }

    fn start_cloud(&mut self, len: usize) {
        self.len = len;
        for g in self.grains.iter_mut() {
            g.on = false;
        }
        self.until = 0.0;
        self.want = 1.0;
    }

    /// Fade the cloud out (the live signal comes back).
    pub fn thaw(&mut self) {
        self.want = 0.0;
    }

    /// The cloud is (fading) in.
    pub fn is_frozen(&self) -> bool {
        self.want > 0.0
    }

    /// The cloud is audible at all (in, or fading out).
    pub fn is_sounding(&self) -> bool {
        self.weight > 0.0 || self.want > 0.0
    }

    /// Frames frozen.
    pub fn frozen_frames(&self) -> usize {
        self.len
    }

    /// Move the position by `frames` of the frozen audio (negative: back), wrapping.
    pub fn scrub(&mut self, frames: f64) {
        if self.len == 0 || !frames.is_finite() {
            return;
        }
        let p = self.s.position + frames / self.len as f64;
        self.s.position = p - libm::floor(p);
    }

    pub fn position(&self) -> f64 {
        self.s.position
    }

    /// Grains sounding now.
    pub fn grains(&self) -> usize {
        self.grains.iter().filter(|g| g.on).count()
    }

    fn rand(&mut self) -> f64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x as f64 / u32::MAX as f64
    }

    fn spawn(&mut self) {
        let Some(k) = self.grains.iter().position(|g| !g.on) else {
            return;
        };
        let len = self.len as f64;
        let glen = ((self.s.size_ms / 1000.0) * self.sample_rate).clamp(16.0, len.max(16.0));
        let off = (self.rand() - 0.5) * self.s.spread * len;
        let mut start = self.s.position * len + off - glen / 2.0;
        start -= len * libm::floor(start / len);
        let semis = (self.rand() * 2.0 - 1.0) * self.s.jitter;
        self.grains[k] = Grain {
            on: true,
            pos: start,
            inc: libm::pow(2.0, semis / 12.0),
            age: 0,
            len: glen as usize,
        };
    }

    #[inline]
    fn at(buf: &[f32], len: usize, pos: f64) -> f32 {
        let i = libm::floor(pos) as usize % len;
        let f = (pos - libm::floor(pos)) as f32;
        let a = buf[i];
        let b = buf[(i + 1) % len];
        a + (b - a) * f
    }

    /// Replace `block` (the strip's live signal) with the cloud, crossfading as it freezes and
    /// thaws. Does nothing while thawed.
    pub fn process(&mut self, block: &mut [f32]) {
        self.process_pair(block, None);
    }

    /// [`process`](Self::process) on a stereo pair: a stereo freeze puts its right channel's
    /// cloud in `right`, a mono one the same cloud in both.
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.process_pair(left, Some(right));
    }

    fn process_pair(&mut self, block: &mut [f32], mut right: Option<&mut [f32]>) {
        if !self.is_sounding() || self.len == 0 {
            return;
        }
        let sr = self.sample_rate;
        let interval = sr / self.s.density.max(0.1);
        let overlap = (self.s.density * self.s.size_ms / 1000.0).max(1.0);
        // Hann grains overlapping `overlap` deep add up about as sqrt(overlap) in power
        let gain = (1.6 / libm::sqrt(overlap)) as f32;
        let mix = self.s.mix as f32;
        let len = self.len as f64;
        for (idx, x) in block.iter_mut().enumerate() {
            if self.want > 0.0 {
                self.until -= 1.0;
                if self.until <= 0.0 {
                    self.spawn();
                    // ±30 % around the interval, so the cloud has no pulse of its own
                    self.until += interval * (0.7 + 0.6 * self.rand());
                }
            }
            let mut y = 0f32;
            let mut yr = 0f32;
            let stereo = !self.buf_r.is_empty();
            for k in 0..MAX_GRAINS {
                let g = self.grains[k];
                if !g.on {
                    continue;
                }
                let w = libm::sin(PI * g.age as f64 / g.len as f64);
                let w2 = (w * w) as f32;
                y += Self::at(&self.buf, self.len, g.pos) * w2;
                if stereo {
                    yr += Self::at(&self.buf_r, self.len, g.pos) * w2;
                }
                let g = &mut self.grains[k];
                g.pos += g.inc;
                if g.pos >= len {
                    g.pos -= len;
                }
                g.age += 1;
                if g.age >= g.len {
                    g.on = false;
                }
            }
            if self.weight < self.want {
                self.weight = (self.weight + self.step).min(self.want);
            } else if self.weight > self.want {
                self.weight = (self.weight - self.step).max(self.want);
            }
            let wet = self.weight * mix;
            *x = *x * (1.0 - wet) + y * gain * wet;
            if let Some(r) = right.as_deref_mut() {
                if let Some(xr) = r.get_mut(idx) {
                    let c = if stereo { yr } else { y };
                    *xr = *xr * (1.0 - wet) + c * gain * wet;
                }
            }
        }
        if self.weight <= 0.0 && self.want <= 0.0 {
            for g in self.grains.iter_mut() {
                g.on = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn tone_history(hz: f64, n: usize) -> History {
        let mut h = History::new(n + 4096);
        let v: Vec<f32> = (0..n)
            .map(|i| 0.5 * libm::sin(2.0 * PI * hz * i as f64 / SR) as f32)
            .collect();
        h.push(&v);
        h
    }

    #[test]
    fn a_stereo_freeze_keeps_each_channel() {
        let left = tone_history(440.0, 48_000);
        let right = History::new(48_000 + 4096); // silence
        let mut right = right;
        right.push(&vec![0.0; 48_000]);
        let mut g = GrainFreeze::new_stereo(SR, 96_000);
        assert!(g.freeze_stereo(&left, &right, 48_000, 24_000));
        let (mut l, mut r) = (vec![0f32; 24_000], vec![0f32; 24_000]);
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            g.process_stereo(a, b);
        }
        assert!(rms(&l[4800..]) > 0.05, "{}", rms(&l[4800..]));
        assert!(rms(&r[4800..]) < 1e-6, "the right stays silent {}", rms(&r[4800..]));
        // a mono freeze puts its one cloud in both
        let mut m = GrainFreeze::new(SR, 96_000);
        assert!(m.freeze(&left, 48_000, 24_000));
        let (mut l, mut r) = (vec![0f32; 24_000], vec![0f32; 24_000]);
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            m.process_stereo(a, b);
        }
        assert_eq!(l, r);
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    #[test]
    fn a_freeze_holds_the_sound_after_the_input_stops() {
        let h = tone_history(220.0, 48_000);
        let mut f = GrainFreeze::new(SR, 96_000);
        assert!(f.freeze(&h, h.written() as i64, 24_000));
        assert_eq!(f.frozen_frames(), 24_000);
        // the live input is silent now: the cloud carries on
        let mut out = vec![0.0; 48_000];
        f.process(&mut out);
        let level = rms(&out[24_000..]);
        assert!(level > 0.1 && level < 0.8, "{level}");
        assert!(f.grains() > 0);
        // thaw: back to the (silent) live signal after the fade
        f.thaw();
        let mut out = vec![0.0; 4_800];
        f.process(&mut out);
        assert!(out[1_000..].iter().all(|x| *x == 0.0));
        assert!(!f.is_sounding());
    }

    #[test]
    fn thawed_it_leaves_the_signal_alone() {
        let mut f = GrainFreeze::new(SR, 1024);
        let mut x: Vec<f32> = (0..512).map(|i| i as f32 / 512.0).collect();
        let y = x.clone();
        f.process(&mut x);
        assert_eq!(x, y);
        // nothing recorded: no freeze
        let h = History::new(1024);
        assert!(!f.freeze(&h, 0, 512));
    }

    #[test]
    fn grains_start_around_the_position() {
        // a ramp: the value read is where in the buffer the grain is
        let mut h = History::new(50_000);
        let v: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        h.push(&v);
        let mut f = GrainFreeze::new(SR, 48_000);
        let s = FreezeSettings {
            spread: 0.0,
            size_ms: 20.0,
            density: 50.0,
            position: 0.25,
            ..FreezeSettings::default()
        };
        f.configure(&s);
        f.freeze(&h, 48_000, 48_000);
        let mut out = vec![0.0; 9_600];
        f.process(&mut out);
        assert!(rms(&out[2_000..]) > 0.05);
        // three quarters in, the ramp reads three times higher than a quarter in
        let mut f2 = GrainFreeze::new(SR, 48_000);
        f2.configure(&FreezeSettings {
            position: 0.75,
            ..s
        });
        f2.freeze(&h, 48_000, 48_000);
        let mut out2 = vec![0.0; 9_600];
        f2.process(&mut out2);
        assert!(rms(&out2[2_000..]) > rms(&out[2_000..]) * 2.0);
    }

    #[test]
    fn scrub_moves_the_position_and_wraps() {
        let h = tone_history(220.0, 48_000);
        let mut f = GrainFreeze::new(SR, 48_000);
        f.freeze(&h, 48_000, 24_000);
        f.scrub(6_000.0);
        assert!((f.position() - 0.75).abs() < 1e-9);
        f.scrub(12_000.0);
        assert!((f.position() - 0.25).abs() < 1e-9);
        f.scrub(-12_000.0);
        assert!((f.position() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn jitter_changes_pitch_and_is_repeatable() {
        let h = tone_history(440.0, 48_000);
        let render = |jitter: f64| {
            let mut f = GrainFreeze::new(SR, 48_000);
            f.configure(&FreezeSettings {
                jitter,
                ..FreezeSettings::default()
            });
            f.freeze(&h, 48_000, 24_000);
            let mut out = vec![0.0; 24_000];
            f.process(&mut out);
            out
        };
        assert_eq!(render(5.0), render(5.0));
        assert_ne!(render(5.0), render(0.0));
    }

    #[test]
    fn knobs_round_trip() {
        for p in FreezeParam::ALL {
            assert_eq!(FreezeParam::parse(p.name()), Some(p));
            let (lo, hi) = p.range();
            for k in [0.0, 0.5, 1.0] {
                let x = p.from_knob(k);
                assert!(x >= lo - 1e-9 && x <= hi + 1e-9, "{p:?} {x}");
                assert!((p.from_knob(p.to_knob(x)) - x).abs() < 1e-6, "{p:?}");
            }
        }
        assert_eq!(FreezeParam::Beats.from_knob(0.5), 2.0);
        let mut s = FreezeSettings::default();
        assert_eq!(s.set(FreezeParam::Position, 1.25), Some(0.25));
        assert_eq!(s.set(FreezeParam::Size, 1e9), Some(500.0));
        assert_eq!(s.set(FreezeParam::Mix, f64::NAN), None);
        assert_eq!(
            s.to_json().get("quantize").and_then(JsonValue::as_bool),
            Some(true)
        );
    }
}
