//! `pitchClassError` from `Audio/Sampler.swift`: the load-time tuning measurement.

use alloc::vec;
use alloc::vec::Vec;

/// How far `x` (a held `note`) sits from that note's pitch class, in cents, whatever octave
/// it sounds in (a glockenspiel speaks two octaves up): the strongest partial within ±100
/// cents of the note in octaves −2…+3, by Hann-windowed DFT in 1-cent steps. Returns
/// `(cents, octave, level)`; `level` is the partial's power, 0 for a window too short, so
/// `level > 0` also says "it sounded".
pub fn pitch_class_error(x: &[f32], note: i64, sample_rate: f64) -> (f64, i64, f64) {
    let n = x.len();
    if n <= 16 {
        return (0.0, 0, 0.0);
    }
    let mut w: Vec<f64> = vec![0.0; n];
    for i in 0..n {
        w[i] = x[i] as f64
            * (0.5 - 0.5 * libm::cos(2.0 * core::f64::consts::PI * i as f64 / (n - 1) as f64));
    }
    let mut best = (0.0f64, 0i64, -1.0f64);
    for oct in -2i64..=3 {
        let base = 440.0 * libm::pow(2.0, (note + 12 * oct - 69) as f64 / 12.0);
        if base >= sample_rate * 0.45 {
            continue;
        }
        let mut c = -100.0f64;
        while c <= 100.0 {
            let k = 2.0 * core::f64::consts::PI * base * libm::pow(2.0, c / 1200.0) / sample_rate;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            // rotate a phasor instead of calling cos/sin per sample
            let cr = libm::cos(k);
            let ci = -libm::sin(k);
            let (mut pr, mut pi) = (1.0f64, 0.0f64);
            for wi in w.iter() {
                re += wi * pr;
                im += wi * pi;
                let t = pr * cr - pi * ci;
                pi = pr * ci + pi * cr;
                pr = t;
            }
            let mag = re * re + im * im;
            if mag > best.2 {
                best = (c, oct, mag);
            }
            c += 1.0;
        }
    }
    best
}
