//! Port of `Musicality/PitchMeasure.swift`: the pitch of a held note, offline. `fundamental`
//! is YIN's cumulative-mean normalised difference with a parabolic fit (30 Hz – 2 kHz);
//! `error` wraps the sampler's own load-time check (`voices::pitch::pitch_class_error`).

use alloc::vec;

use crate::voices::pitch::pitch_class_error;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// The strongest partial near the note's pitch class, Hz (0 for silence).
    pub hz: f64,
    /// How far it sits from the note's pitch class, cents.
    pub cents: f64,
    /// The octave it speaks in, against the note.
    pub octave: i64,
    /// That partial's amplitude, linear.
    pub level: f64,
}

/// Fundamental of `samples` (mono, the first 8192 frames), Hz; None for silence or no clear
/// period.
pub fn fundamental(samples: &[f32], sr: f64) -> Option<f64> {
    let n = samples.len().min(8192);
    let min_lag = ((sr / 2000.0) as usize).max(2);
    let max_lag = (n / 2).min((sr / 30.0) as usize);
    if !(sr > 0.0 && max_lag > min_lag + 2) {
        return None;
    }
    let mut energy = 0.0;
    for i in 0..n {
        energy += samples[i] as f64 * samples[i] as f64;
    }
    if energy / n as f64 <= 1e-12 {
        return None;
    }
    let w = n - max_lag - 1;
    let mut d = vec![1.0f64; max_lag + 2];
    let mut running = 0.0;
    for tau in 1..=(max_lag + 1) {
        let mut s = 0.0;
        for i in 0..w {
            let e = samples[i] as f64 - samples[i + tau] as f64;
            s += e * e;
        }
        running += s;
        d[tau] = if running > 0.0 {
            s * tau as f64 / running
        } else {
            1.0
        };
    }
    let mut pick: Option<usize> = None;
    let mut tau = min_lag;
    while tau <= max_lag {
        if d[tau] < 0.1 {
            while tau + 1 <= max_lag && d[tau + 1] < d[tau] {
                tau += 1;
            }
            pick = Some(tau);
            break;
        }
        tau += 1;
    }
    if pick.is_none() {
        // `min(by: <)` keeps the first of equal minima
        let mut m = min_lag;
        for t in min_lag..=max_lag {
            if d[t] < d[m] {
                m = t;
            }
        }
        if d[m] < 0.3 {
            pick = Some(m);
        }
    }
    let t = pick?;
    let (a, b, c) = (d[t - 1], d[t], d[t + 1]);
    let den = a - 2.0 * b + c;
    let shift = if den != 0.0 {
        (0.5 * (a - c) / den).max(-1.0).min(1.0)
    } else {
        0.0
    };
    Some(sr / (t as f64 + shift))
}

/// Where `samples` (a held `note`, mono) sits against that note: the sampler's own check
/// with the partial's frequency and amplitude.
pub fn error(samples: &[f32], note: i64, sr: f64) -> Reading {
    let (cents, octave, level) = pitch_class_error(samples, note, sr);
    if !(level > 0.0 && samples.len() > 16) {
        return Reading {
            hz: 0.0,
            cents: 0.0,
            octave: 0,
            level: 0.0,
        };
    }
    // the Hann window's sum: a sine of amplitude A reads |X| = A · Σw / 2
    let sum_w = (samples.len() - 1) as f64 / 2.0;
    let hz = 440.0
        * libm::pow(
            2.0,
            ((note + 12 * octave - 69) as f64 + cents / 100.0) / 12.0,
        );
    Reading {
        hz,
        cents,
        octave,
        level: 2.0 * libm::sqrt(level) / sum_w,
    }
}
