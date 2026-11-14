//! The Toy Box voices, synthesized from first principles: additive harmonic and inharmonic
//! partial banks, ratio-1 FM, filtered noise from a fixed-seed generator, and envelopes. Every
//! number is computed with `libm` and plain `f64` arithmetic in a fixed order, so the samples
//! (and the bank built from them) are the same bytes on every platform.
//!
//! Nothing here reads, resamples or is fitted to any recording: each voice is a recipe.

use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;
use core::ops::Range;

use libm::{cos, exp, fabs, floor, log2, pow, round, sin, sqrt};

/// Every sample's rate, Hz.
pub const SR: f64 = 32_000.0;
const TAU: f64 = 2.0 * PI;
/// Partials stop below this fraction of the sample rate (band-limited, nothing folds back).
const NYQUIST_GUARD: f64 = 0.45;
/// Amplitudes of the harmonic bank are evaluated every `BLOCK` frames and interpolated; loop
/// starts and lengths are multiples of it, so a looped voice repeats exactly.
const BLOCK: usize = 16;

/// The frequency of MIDI key `key` (A4 = 69 = 440 Hz, equal temperament).
pub fn midi_hz(key: f64) -> f64 {
    440.0 * pow(2.0, (key - 69.0) / 12.0)
}

fn frames(seconds: f64) -> usize {
    round(seconds * SR) as usize
}

fn block_frames(seconds: f64) -> usize {
    (frames(seconds) / BLOCK).max(1) * BLOCK
}

/// A rendered sample: mono at [`SR`], and its loop (frames, end exclusive) when it sustains.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub pcm: Vec<f64>,
    pub loop_: Option<Range<usize>>,
}

impl Rendered {
    fn once(pcm: Vec<f64>) -> Rendered {
        Rendered { pcm, loop_: None }
    }
}

/// xorshift32 with a fixed seed: the only source of noise.
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(if seed == 0 { 0x9E37_79B9 } else { seed })
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in [-1, 1).
    pub fn white(&mut self) -> f64 {
        (self.next_u32() as f64 / 4_294_967_296.0) * 2.0 - 1.0
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        self.next_u32() as f64 / 4_294_967_296.0
    }
}

fn noise(len: usize, seed: u32) -> Vec<f64> {
    let mut r = Rng::new(seed);
    (0..len).map(|_| r.white()).collect()
}

// MARK: filters

/// One-pole lowpass at `hz`, run over `x` in place.
fn lowpass(x: &mut [f64], hz: f64) {
    let a = 1.0 - exp(-TAU * hz / SR);
    let mut y = 0.0;
    for v in x.iter_mut() {
        y += a * (*v - y);
        *v = y;
    }
}

/// One-pole highpass at `hz` (the input less its lowpass), in place.
fn highpass(x: &mut [f64], hz: f64) {
    let a = 1.0 - exp(-TAU * hz / SR);
    let mut y = 0.0;
    for v in x.iter_mut() {
        y += a * (*v - y);
        *v -= y;
    }
}

/// An RBJ bandpass biquad (0 dB peak).
#[derive(Clone, Copy)]
struct Bandpass {
    b0: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Bandpass {
    fn new(hz: f64, q: f64) -> Bandpass {
        let mut b = Bandpass {
            b0: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        b.tune(hz, q);
        b
    }
    fn tune(&mut self, hz: f64, q: f64) {
        let w = TAU * hz.min(SR * 0.45) / SR;
        let alpha = sin(w) / (2.0 * q);
        let a0 = 1.0 + alpha;
        self.b0 = alpha / a0;
        self.b2 = -alpha / a0;
        self.a1 = -2.0 * cos(w) / a0;
        self.a2 = (1.0 - alpha) / a0;
    }
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// `len` frames of noise bandpassed at `hz`, periodic with period `len` (filtered around the
/// ring twice, the second pass kept), so it can sit inside a loop without a seam.
fn ring_noise(len: usize, seed: u32, hz: f64, q: f64) -> Vec<f64> {
    let src = noise(len, seed);
    let mut bp = Bandpass::new(hz, q);
    let mut out = vec![0.0; len];
    for pass in 0..2 {
        for i in 0..len {
            let y = bp.run(src[i]);
            if pass == 1 {
                out[i] = y;
            }
        }
    }
    out
}

// MARK: envelopes

/// A raised-cosine fade in over the first `n` frames.
fn fade_in(x: &mut [f64], n: usize) {
    let n = n.min(x.len());
    for i in 0..n {
        x[i] *= 0.5 - 0.5 * cos(PI * i as f64 / n as f64);
    }
}

/// A raised-cosine fade out over the last `n` frames, the last frame silent.
fn fade_out(x: &mut [f64], n: usize) {
    let len = x.len();
    let n = n.min(len);
    for i in 0..n {
        let g = 0.5 + 0.5 * cos(PI * (i + 1) as f64 / n as f64);
        x[len - n + i] *= g;
    }
}

/// 0 → 1 over `secs` (smoothstep), 1 after.
fn rise(t: f64, secs: f64) -> f64 {
    if t >= secs {
        return 1.0;
    }
    let u = t / secs;
    u * u * (3.0 - 2.0 * u)
}

// MARK: oscillator banks

/// A harmonic voice: `sum_k amp(k, t) * sin(k * phase[n])` for k in 1..=kmax, given the
/// fundamental's `(sin, cos)` a frame ([`ramp`], [`trig`]); the amplitudes evaluated every
/// [`BLOCK`] frames (at `n = j * BLOCK`) and interpolated, the harmonics by the Chebyshev
/// recurrence.
fn harmonic_bank(
    osc: &[(f64, f64)],
    kmax: usize,
    amp: &mut dyn FnMut(usize, f64) -> f64,
) -> Vec<f64> {
    let len = osc.len();
    let mut out = vec![0.0; len];
    if kmax == 0 {
        return out;
    }
    let mut a0 = vec![0.0; kmax + 1];
    let mut a1 = vec![0.0; kmax + 1];
    for k in 1..=kmax {
        a0[k] = amp(k, 0.0);
    }
    let mut b = 0;
    while b < len {
        let t1 = (b + BLOCK) as f64 / SR;
        for k in 1..=kmax {
            a1[k] = amp(k, t1);
        }
        let end = (b + BLOCK).min(len);
        for n in b..end {
            let fr = (n - b) as f64 / BLOCK as f64;
            let (sn, cs) = osc[n];
            let c2 = 2.0 * cs;
            let (mut prev, mut cur) = (0.0, sn);
            let mut acc = 0.0;
            for k in 1..=kmax {
                acc += (a0[k] + (a1[k] - a0[k]) * fr) * cur;
                let next = c2 * cur - prev;
                prev = cur;
                cur = next;
            }
            out[n] = acc;
        }
        core::mem::swap(&mut a0, &mut a1);
        b += BLOCK;
    }
    out
}

/// The harmonics a fundamental at `hz` can have below the guard, at most `cap`.
fn kmax(hz: f64, cap: usize) -> usize {
    (floor(NYQUIST_GUARD * SR / hz) as usize).min(cap)
}

/// A steady oscillator at `hz` as `(sin, cos)` a frame, by a rotating phasor (no
/// transcendental call a frame).
fn ramp(len: usize, hz: f64) -> Vec<(f64, f64)> {
    let w = TAU * hz / SR;
    let (cw, sw) = (cos(w), sin(w));
    let (mut re, mut im) = (1.0f64, 0.0f64);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push((im, re));
        let r = re * cw - im * sw;
        im = re * sw + im * cw;
        re = r;
    }
    out
}

/// A phase track as `(sin, cos)` a frame.
fn trig(phase: &[f64]) -> Vec<(f64, f64)> {
    phase.iter().map(|&p| (sin(p), cos(p))).collect()
}

/// The phase of a glide: the instantaneous frequency `hz(t)` summed frame by frame.
fn glide(len: usize, hz: &dyn Fn(f64) -> f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(len);
    let mut ph = 0.0;
    for n in 0..len {
        out.push(ph);
        ph += TAU * hz(n as f64 / SR) / SR;
    }
    out
}

/// An exponentially decaying sine partial: `ratio` of the fundamental, `amp`, time constant
/// `tau` seconds.
#[derive(Clone, Copy, Debug)]
struct Partial {
    ratio: f64,
    amp: f64,
    tau: f64,
}

const fn p(ratio: f64, amp: f64, tau: f64) -> Partial {
    Partial { ratio, amp, tau }
}

/// Partials at `hz`, struck at frame `at`, their decays scaled by `stretch`, added into `out`
/// (a rotating phasor and a per-frame decay factor: no transcendental call a frame).
fn strike(out: &mut [f64], at: usize, hz: f64, partials: &[Partial], stretch: f64, gain: f64) {
    for q in partials {
        let f = hz * q.ratio;
        if f >= NYQUIST_GUARD * SR || at >= out.len() {
            continue;
        }
        let w = TAU * f / SR;
        let (cw, sw) = (cos(w), sin(w));
        let d = exp(-1.0 / (q.tau * stretch * SR));
        let (mut re, mut im) = (1.0f64, 0.0f64);
        let mut a = q.amp * gain;
        for v in out[at..].iter_mut() {
            *v += a * im;
            let r = re * cw - im * sw;
            im = re * sw + im * cw;
            re = r;
            a *= d;
            if a < 1e-7 {
                break;
            }
        }
    }
}

/// A short burst of filtered noise at frame `at`, decaying with time constant `tau`.
fn knock(out: &mut [f64], at: usize, seed: u32, lo_hz: f64, hi_hz: f64, tau: f64, gain: f64) {
    let len = out.len().saturating_sub(at).min(frames(tau * 8.0));
    let mut n = noise(len, seed);
    if hi_hz > 0.0 {
        lowpass(&mut n, hi_hz);
    }
    if lo_hz > 0.0 {
        highpass(&mut n, lo_hz);
    }
    for (i, v) in n.iter().enumerate() {
        out[at + i] += gain * v * exp(-(i as f64) / (tau * SR));
    }
}

/// The loop that holds a sustained voice: a length in `[lo, hi]` seconds (a multiple of
/// [`BLOCK`]) holding a whole number of periods, and the fundamental that makes it exact —
/// within a fraction of a cent of `hz`, the closest the candidates allow.
fn loop_fit(hz: f64, lo: f64, hi: f64) -> (usize, f64) {
    let mut best = (0usize, hz, f64::INFINITY);
    let mut l = block_frames(lo);
    let top = frames(hi);
    while l <= top {
        let m = round(hz * l as f64 / SR);
        if m >= 1.0 {
            let f = m * SR / l as f64;
            let e = fabs(log2(f / hz));
            if e < best.2 {
                best = (l, f, e);
            }
        }
        l += BLOCK;
    }
    (best.0, best.1)
}

/// A sustained sample: `start` frames of onset, then one loop of `len`, then 64 frames past
/// it (the same waveform carried on, so an interpolating player reads true samples there).
fn looped(pcm: Vec<f64>, start: usize, len: usize) -> Rendered {
    let mut pcm = pcm;
    pcm.truncate(start + len + 64);
    fade_in(&mut pcm, frames(0.002));
    Rendered {
        pcm,
        loop_: Some(start..start + len),
    }
}

// MARK: tonal voices
//
// Each takes the zone's root frequency `hz` and `stretch`, how much longer the zone rings
// than the program's middle one (lower zones ring longer, as strings and bars do).

/// A plucked string as harmonics: each harmonic's level from the pluck position (`pos`, a
/// fraction of the string) and a brightness tilt, through a body's resonances
/// (`(hz, gain, width)`), the higher ones dying faster (`spread`).
#[allow(clippy::too_many_arguments)]
fn pluck(
    hz: f64,
    stretch: f64,
    pos: f64,
    tilt: f64,
    tau1: f64,
    spread: f64,
    dur: f64,
    body: &[(f64, f64, f64)],
    seed: u32,
) -> Rendered {
    let len = frames(dur * stretch);
    let k = kmax(hz, 64);
    let mut level = vec![0.0; k + 1];
    let mut tau = vec![0.0; k + 1];
    for i in 1..=k {
        let f = hz * i as f64;
        let mut g = fabs(sin(PI * i as f64 * pos)) / pow(i as f64, tilt);
        for &(bh, bg, bw) in body {
            let d = (f - bh) / bw;
            g *= 1.0 + bg * exp(-d * d);
        }
        level[i] = g;
        tau[i] = tau1 * stretch / (1.0 + spread * (i as f64 - 1.0));
    }
    let mut x = harmonic_bank(&ramp(len, hz), k, &mut |i, t| level[i] * exp(-t / tau[i]));
    let peak = x.iter().fold(0.0f64, |m, v| m.max(fabs(*v)));
    knock(&mut x, 0, seed, hz, 6000.0, 0.003, 0.15 * peak);
    fade_in(&mut x, frames(0.0015));
    fade_out(&mut x, len / 6);
    Rendered::once(x)
}

fn pluck_harp(hz: f64, s: f64) -> Rendered {
    pluck(hz, s, 0.13, 0.35, 0.75, 0.45, 0.9, &[], 11)
}

fn nylon_pick(hz: f64, s: f64) -> Rendered {
    pluck(
        hz,
        s,
        0.21,
        0.6,
        0.7,
        0.35,
        0.95,
        &[(210.0, 0.8, 90.0), (480.0, 0.5, 160.0)],
        13,
    )
}

fn twang(hz: f64, s: f64) -> Rendered {
    // bright and short, with a nasal drum-head formant
    pluck(
        hz,
        s,
        0.07,
        0.15,
        0.42,
        0.6,
        0.8,
        &[(1100.0, 1.6, 350.0), (2600.0, 1.0, 600.0)],
        17,
    )
}

fn thump_bass(hz: f64, s: f64) -> Rendered {
    let len = frames(0.9 * s);
    let parts = [
        p(1.0, 1.0, 0.42),
        p(2.0, 0.45, 0.18),
        p(3.0, 0.22, 0.09),
        p(4.0, 0.1, 0.05),
    ];
    let mut x = vec![0.0; len];
    strike(&mut x, 0, hz, &parts, s, 1.0);
    // the felt of the hit: a dull low thump of noise
    knock(&mut x, 0, 19, 0.0, 260.0, 0.018, 1.4);
    fade_in(&mut x, frames(0.003));
    fade_out(&mut x, len / 5);
    Rendered::once(x)
}

fn chip_square(hz: f64, _s: f64) -> Rendered {
    let (l, f) = loop_fit(hz, 0.10, 0.15);
    let start = block_frames(0.064);
    let len = start + l + 64;
    let k = kmax(f, 200);
    // a 25 % pulse: harmonic k at sin(pi k d) / k
    let level: Vec<f64> = (0..=k)
        .map(|i| {
            if i == 0 {
                0.0
            } else {
                sin(PI * i as f64 * 0.25) / i as f64
            }
        })
        .collect();
    let t0 = start as f64 / SR;
    // a 20 % accent that has died away (to the last bit) by the loop
    let x = harmonic_bank(&ramp(len, f), k, &mut |i, t| {
        level[i]
            * if t >= t0 {
                0.8
            } else {
                0.8 + 0.2 * exp(-t / 0.008)
            }
    });
    looped(x, start, l)
}

fn reed_drone(hz: f64, _s: f64) -> Rendered {
    let (l, f) = loop_fit(hz, 0.36, 0.48);
    let start = block_frames(0.24);
    let len = start + l + 64;
    let k = kmax(f, 90);
    let lfo = SR / l as f64; // one cycle a loop
    let t0 = start as f64 / SR;
    let x = harmonic_bank(&ramp(len, f), k, &mut |i, t| {
        let fi = f * i as f64;
        // a mouth formant swinging around 650 Hz, once a loop
        let centre = 650.0 + 170.0 * sin(TAU * lfo * t);
        let d = (fi - centre) / 280.0;
        let second = (fi - 1700.0) / 400.0;
        let shape = (1.0 + 2.4 * exp(-d * d) + 0.8 * exp(-second * second)) / pow(i as f64, 0.75);
        let growl = 1.0 + 0.12 * sin(TAU * 2.0 * lfo * t + 0.3 * i as f64);
        let env = if t >= t0 { 1.0 } else { rise(t, 0.12) };
        shape * growl * env
    });
    looped(x, start, l)
}

fn reed_flute(hz: f64, _s: f64) -> Rendered {
    let (l, f) = loop_fit(hz, 0.36, 0.48);
    let start = block_frames(0.2);
    let len = start + l + 64;
    let fv = 2.0 * SR / l as f64; // two vibrato cycles a loop, about 5 Hz
    let depth = 0.0035 * f / fv; // phase deviation for ±6 cents
    let t0 = start as f64 / SR;
    let phase: Vec<f64> = (0..len)
        .map(|n| {
            let t = n as f64 / SR;
            let b = if t >= t0 {
                1.0
            } else {
                rise((t - 0.08).max(0.0), t0 - 0.08)
            };
            TAU * f * t + depth * b * sin(TAU * fv * t)
        })
        .collect();
    let level = [0.0, 1.0, 0.22, 0.1, 0.05, 0.025, 0.012];
    let k = kmax(f, level.len() - 1);
    let mut x = harmonic_bank(&trig(&phase), k, &mut |i, t| {
        let swell = if t >= t0 {
            1.0
        } else {
            rise(t, 0.07) * (1.0 + 0.08 * exp(-t / 0.03))
        };
        level[i] * swell
    });
    // breath: a hiss around the second harmonic, periodic with the loop
    let breath = ring_noise(l, 23, (2.0 * f).min(9000.0), 1.5);
    for n in 0..len {
        let t = n as f64 / SR;
        let env = if t >= t0 { 1.0 } else { rise(t, 0.07) };
        x[n] += 0.25 * env * breath[n % l];
    }
    // chiff: a puff at the onset, gone before the loop
    let chiff_len = start.min(frames(0.08));
    let mut chiff = noise(chiff_len, 29);
    highpass(&mut chiff, f);
    lowpass(&mut chiff, 4.0 * f);
    for (n, v) in chiff.iter().enumerate() {
        let t = n as f64 / SR;
        let g = 0.5 * exp(-t / 0.015) * (1.0 - n as f64 / chiff_len as f64);
        x[n] += g * v;
    }
    looped(x, start, l)
}

fn toy_horn(hz: f64, _s: f64) -> Rendered {
    let (l, f) = loop_fit(hz, 0.36, 0.48);
    let start = block_frames(0.26);
    let len = start + l + 64;
    let fv = 2.0 * SR / l as f64;
    let depth = 0.0025 * f / fv;
    let t0 = start as f64 / SR;
    let phase: Vec<f64> = (0..len)
        .map(|n| {
            let t = n as f64 / SR;
            // a small scoop up into the note
            let scoop = 0.03 * 0.025 * (1.0 - exp(-t / 0.025));
            let b = if t >= t0 {
                1.0
            } else {
                rise((t - 0.1).max(0.0), t0 - 0.1)
            };
            TAU * f * (t - scoop) + depth * b * sin(TAU * fv * t)
        })
        .collect();
    let k = kmax(f, 20);
    let x = harmonic_bank(&trig(&phase), k, &mut |i, t| {
        let fi = f * i as f64;
        let d = (fi - 1300.0) / 550.0;
        let level = exp(-0.24 * (i as f64 - 1.0)) * (1.0 + 1.4 * exp(-d * d));
        // brass speaks low first: each harmonic comes in a little later than the one below
        let speak = if t >= t0 {
            1.0
        } else {
            rise(t, 0.02 + 0.01 * i as f64)
        };
        level * speak
    });
    looped(x, start, l)
}

fn glass_tine(hz: f64, s: f64) -> Rendered {
    // ratio-1 FM whose index falls away (bright → round), and a quick high tine
    let len = frames(1.0 * s);
    let tine = 14.0 * hz < NYQUIST_GUARD * SR;
    let x: Vec<f64> = (0..len)
        .map(|n| {
            let t = n as f64 / SR;
            let ph = TAU * hz * t;
            let index = 1.8 * exp(-t / 0.1) + 0.45;
            let mut v = sin(ph + index * sin(ph)) * exp(-t / (0.42 * s));
            if tine {
                v += 0.12 * exp(-t / 0.025) * sin(14.0 * ph);
            }
            v
        })
        .collect();
    let mut x = x;
    fade_in(&mut x, frames(0.0015));
    fade_out(&mut x, len / 5);
    Rendered::once(x)
}

fn bar(hz: f64, s: f64, dur: f64, parts: &[Partial], click: f64, seed: u32) -> Rendered {
    let len = frames(dur * s);
    let mut x = vec![0.0; len];
    strike(&mut x, 0, hz, parts, s, 1.0);
    if click > 0.0 {
        knock(&mut x, 0, seed, 0.0, (4.0 * hz).min(12_000.0), 0.004, click);
    }
    fade_in(&mut x, frames(0.001));
    fade_out(&mut x, len / 5);
    Rendered::once(x)
}

fn steel_mallet(hz: f64, s: f64) -> Rendered {
    bar(
        hz,
        s,
        1.05,
        &[p(1.0, 1.0, 0.65), p(4.0, 0.3, 0.25), p(9.8, 0.08, 0.06)],
        0.15,
        31,
    )
}

fn wood_mallet(hz: f64, s: f64) -> Rendered {
    bar(
        hz,
        s,
        0.55,
        &[p(1.0, 1.0, 0.2), p(3.0, 0.28, 0.06), p(6.2, 0.08, 0.025)],
        0.35,
        37,
    )
}

fn clank_bell(hz: f64, s: f64) -> Rendered {
    bar(
        hz,
        s,
        0.7,
        &[
            p(1.0, 1.0, 0.28),
            p(1.48, 0.5, 0.2),
            p(2.0, 0.2, 0.14),
            p(2.96, 0.16, 0.08),
            p(3.7, 0.08, 0.05),
        ],
        0.25,
        41,
    )
}

fn bright_bell(hz: f64, s: f64) -> Rendered {
    bar(
        hz,
        s,
        1.35,
        &[
            p(0.5, 0.2, 1.0),
            p(1.0, 1.0, 0.8),
            p(1.19, 0.32, 0.7),
            p(1.5, 0.22, 0.5),
            p(2.0, 0.3, 0.45),
            p(2.52, 0.12, 0.25),
            p(3.0, 0.1, 0.2),
            p(4.07, 0.06, 0.12),
        ],
        0.1,
        43,
    )
}

fn ice_chimes(hz: f64, s: f64) -> Rendered {
    // a main bar and two smaller ones an octave and two octaves up, touched just after it
    let len = frames(1.3 * s);
    let bar = [
        p(1.0, 1.0, 0.75),
        p(2.756, 0.4, 0.4),
        p(5.404, 0.18, 0.18),
        p(8.933, 0.08, 0.08),
    ];
    let mut x = vec![0.0; len];
    strike(&mut x, 0, hz, &bar, s, 1.0);
    strike(&mut x, frames(0.07), 2.0 * hz, &bar, 0.6 * s, 0.32);
    strike(&mut x, frames(0.15), 4.0 * hz, &bar, 0.4 * s, 0.18);
    fade_in(&mut x, frames(0.001));
    fade_out(&mut x, len / 5);
    Rendered::once(x)
}

/// A tonal voice by program name, at zone root `hz` with ring `stretch`.
pub fn tone(name: &str, hz: f64, stretch: f64) -> Option<Rendered> {
    Some(match name {
        "pluck-harp" => pluck_harp(hz, stretch),
        "thump-bass" => thump_bass(hz, stretch),
        "reed-drone" => reed_drone(hz, stretch),
        "nylon-pick" => nylon_pick(hz, stretch),
        "chip-square" => chip_square(hz, stretch),
        "twang" => twang(hz, stretch),
        "glass-tine" => glass_tine(hz, stretch),
        "steel-mallet" => steel_mallet(hz, stretch),
        "reed-flute" => reed_flute(hz, stretch),
        "clank-bell" => clank_bell(hz, stretch),
        "bright-bell" => bright_bell(hz, stretch),
        "wood-mallet" => wood_mallet(hz, stretch),
        "ice-chimes" => ice_chimes(hz, stretch),
        "toy-horn" => toy_horn(hz, stretch),
        _ => return None,
    })
}

// MARK: effects (fixed pitch)

fn rise_sweep() -> Vec<f64> {
    let dur = 1.4;
    let len = frames(dur);
    let hz = |t: f64| 200.0 * pow(2.0, 3.0 * (t / 1.3).min(1.0));
    let phase = glide(len, &hz);
    let src = noise(len, 47);
    let mut bp = Bandpass::new(400.0, 4.0);
    let mut x = vec![0.0; len];
    for n in 0..len {
        let t = n as f64 / SR;
        if n % 32 == 0 {
            bp.tune(2.0 * hz(t), 4.0);
        }
        let ph = phase[n];
        let tone = sin(ph) + 0.4 * sin(2.0 * ph) + 0.2 * sin(3.0 * ph);
        let env = pow((t / 1.3).min(1.0), 1.5);
        x[n] = env * (0.6 * tone + 1.2 * bp.run(src[n]));
    }
    fade_out(&mut x, frames(0.1));
    x
}

fn sparkle() -> Vec<f64> {
    let len = frames(1.2);
    let keys = [84.0, 86.0, 88.0, 91.0, 93.0, 96.0, 98.0, 100.0];
    let mut r = Rng::new(53);
    let mut x = vec![0.0; len];
    let parts = [p(1.0, 1.0, 0.09), p(2.76, 0.25, 0.04)];
    for i in 0..16 {
        let at = frames(i as f64 * 0.055 + 0.02 * r.unit());
        let key = keys[(r.next_u32() % keys.len() as u32) as usize];
        strike(&mut x, at, midi_hz(key), &parts, 1.0, pow(0.88, i as f64));
    }
    fade_in(&mut x, frames(0.001));
    fade_out(&mut x, frames(0.15));
    x
}

fn thud() -> Vec<f64> {
    let len = frames(0.8);
    let phase = glide(len, &|t| 42.0 + 90.0 * exp(-t / 0.05));
    let mut x: Vec<f64> = phase
        .iter()
        .enumerate()
        .map(|(n, ph)| sin(*ph) * exp(-(n as f64 / SR) / 0.2))
        .collect();
    knock(&mut x, 0, 59, 0.0, 400.0, 0.03, 2.0);
    strike(
        &mut x,
        0,
        180.0,
        &[p(1.0, 0.3, 0.04), p(2.3, 0.15, 0.025)],
        1.0,
        1.0,
    );
    fade_in(&mut x, frames(0.0015));
    fade_out(&mut x, frames(0.15));
    x
}

fn coin_blip() -> Vec<f64> {
    let len = frames(0.55);
    let mut x = vec![0.0; len];
    let parts = [p(1.0, 1.0, 0.09), p(2.0, 0.2, 0.05), p(3.0, 0.1, 0.03)];
    strike(&mut x, 0, midi_hz(88.0), &parts, 1.0, 0.8);
    strike(&mut x, frames(0.07), midi_hz(95.0), &parts, 1.6, 1.0);
    fade_in(&mut x, frames(0.001));
    fade_out(&mut x, frames(0.12));
    x
}

fn horn_call() -> Vec<f64> {
    let len = frames(1.8);
    let key = |t: f64| {
        if t < 0.25 {
            57.0
        } else if t < 0.55 {
            57.0 + 7.0 * rise(t - 0.25, 0.3)
        } else {
            64.0
        }
    };
    let vib = |t: f64| 1.0 + 0.006 * rise((t - 0.7).max(0.0), 0.3) * sin(TAU * 5.2 * t);
    let phase = glide(len, &|t| midi_hz(key(t)) * vib(t));
    let k = kmax(midi_hz(64.0) * 1.01, 18);
    let mut x = harmonic_bank(&trig(&phase), k, &mut |i, t| {
        let fi = midi_hz(key(t)) * i as f64;
        let d = (fi - 1200.0) / 500.0;
        let level = exp(-0.25 * (i as f64 - 1.0)) * (1.0 + 1.3 * exp(-d * d));
        let swell = rise(t, 0.04 + 0.012 * i as f64) * (0.7 + 0.3 * rise(t, 0.9));
        level * swell
    });
    fade_in(&mut x, frames(0.003));
    fade_out(&mut x, frames(0.35));
    x
}

/// An effect by program name, played as rendered on any key.
pub fn fx(name: &str) -> Option<Vec<f64>> {
    Some(match name {
        "rise-sweep" => rise_sweep(),
        "sparkle" => sparkle(),
        "thud" => thud(),
        "coin-blip" => coin_blip(),
        "horn-call" => horn_call(),
        _ => return None,
    })
}

// MARK: the kit

fn metallic(len: usize, tau: f64, seed: u32, hp: f64) -> Vec<f64> {
    let mut n = noise(len, seed);
    highpass(&mut n, hp);
    highpass(&mut n, hp);
    let mut x = vec![0.0; len];
    // a cluster of inharmonic tones, the cymbal's ring
    strike(
        &mut x,
        0,
        1.0,
        &[
            p(3150.0, 0.12, 1.0),
            p(4730.0, 0.1, 1.0),
            p(5590.0, 0.1, 1.0),
            p(6720.0, 0.08, 1.0),
            p(7900.0, 0.08, 1.0),
            p(9030.0, 0.06, 1.0),
        ],
        tau,
        1.0,
    );
    for i in 0..len {
        let t = i as f64 / SR;
        x[i] += n[i] * exp(-t / tau);
    }
    lowpass(&mut x, 12_000.0);
    fade_in(&mut x, frames(0.0007));
    fade_out(&mut x, len / 4);
    x
}

/// A kit sound by name: `kick`, `snare`, `hat`, `open-hat`, `crash`.
pub fn kit(name: &str) -> Option<Vec<f64>> {
    Some(match name {
        "kick" => {
            let len = frames(0.5);
            let phase = glide(len, &|t| 46.0 + 120.0 * exp(-t / 0.03));
            let mut x: Vec<f64> = phase
                .iter()
                .enumerate()
                .map(|(n, ph)| sin(*ph) * exp(-(n as f64 / SR) / 0.26))
                .collect();
            knock(&mut x, 0, 61, 1500.0, 0.0, 0.002, 0.5);
            fade_in(&mut x, frames(0.0007));
            fade_out(&mut x, frames(0.1));
            x
        }
        "snare" => {
            let len = frames(0.4);
            let mut x = vec![0.0; len];
            strike(
                &mut x,
                0,
                1.0,
                &[p(185.0, 0.6, 0.05), p(330.0, 0.3, 0.03)],
                1.0,
                1.0,
            );
            let mut n = noise(len, 67);
            highpass(&mut n, 900.0);
            lowpass(&mut n, 7000.0);
            for i in 0..len {
                x[i] += 0.9 * n[i] * exp(-(i as f64 / SR) / 0.085);
            }
            fade_in(&mut x, frames(0.0007));
            fade_out(&mut x, frames(0.08));
            x
        }
        "hat" => metallic(frames(0.15), 0.028, 71, 6500.0),
        "open-hat" => metallic(frames(0.6), 0.2, 73, 6000.0),
        "crash" => metallic(frames(1.4), 0.45, 79, 4000.0),
        _ => return None,
    })
}

// MARK: levels

/// Root-mean-square level of `x`, dBFS.
pub fn rms_db(x: &[f64]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let mut sum = 0.0;
    for v in x {
        sum += v * v;
    }
    let r = sqrt(sum / x.len() as f64);
    if r <= 1e-10 {
        -200.0
    } else {
        20.0 * libm::log10(r)
    }
}

/// The largest magnitude in `x`.
pub fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0f64, |m, v| m.max(fabs(*v)))
}
