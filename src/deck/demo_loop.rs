//! Port of `DemoLoop.swift`: a two-bar, 120 BPM drum-and-bass loop synthesized in code, so a
//! deck has something scratchable with no files. Deterministic: same rate, same samples.

use alloc::vec;

use super::turntable::SampleBuffer;

pub const BPM: f64 = 120.0;
pub const BARS: i64 = 2;

pub fn make(sample_rate: f64) -> SampleBuffer {
    let sr = sample_rate;
    let beat = 60.0 / BPM;
    let n = libm::round((BARS * 4) as f64 * beat * sr) as usize;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    let sixteenth = beat / 4.0;
    let mut seed: u32 = 0x9E37_79B9;

    // xorshift32
    let mut noise = move || -> f32 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };

    fn add(
        l: &mut [f32],
        r: &mut [f32],
        sr: f64,
        start: f64,
        seconds: f64,
        pan: f32,
        mut sample: impl FnMut(f64) -> f32,
    ) {
        let n = l.len();
        let s0 = (start * sr) as usize;
        for i in 0..(seconds * sr) as usize {
            if s0 + i >= n {
                continue;
            }
            let v = sample(i as f64 / sr);
            l[s0 + i] += v * (1.0 - pan);
            r[s0 + i] += v * (1.0 + pan);
        }
    }

    // Bass: a 5-in-16 Euclidean rhythm, root and fifth of A.
    let bass_steps = [0, 3, 6, 10, 13];
    let tau = 2.0 * core::f64::consts::PI;
    for step in 0..(BARS * 16) {
        let t = step as f64 * sixteenth;
        if step % 4 == 0 {
            // kick: pitch-swept sine
            add(&mut l, &mut r, sr, t, 0.25, 0.0, |u| {
                let f = 45.0 + 120.0 * libm::exp(-u * 30.0);
                (libm::sin(tau * f * u) * libm::exp(-u * 9.0)) as f32 * 0.9
            });
        }
        if step % 8 == 4 {
            // clap: filtered-ish noise burst
            add(&mut l, &mut r, sr, t, 0.15, 0.15, |u| {
                noise() * libm::exp(-u * 30.0) as f32 * 0.35
            });
        }
        if step % 2 == 1 {
            // hat
            add(&mut l, &mut r, sr, t, 0.05, -0.3, |u| {
                noise() * libm::exp(-u * 90.0) as f32 * 0.18
            });
        }
        if bass_steps.contains(&(step % 16)) {
            let f = if step % 16 == 10 { 82.41 } else { 55.0 }; // E2 / A1
            add(&mut l, &mut r, sr, t, sixteenth * 1.8, 0.0, |u| {
                let saw = 2.0 * (u * f - libm::floor(u * f + 0.5));
                (saw * libm::exp(-u * 6.0)) as f32 * 0.3
            });
        }
    }
    SampleBuffer::new(l, Some(r), sr)
}
