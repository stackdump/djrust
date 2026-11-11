//! Port of `BeatGrid.swift`: a constant-tempo grid over a sample buffer and the analyzer that
//! finds it from onsets.
//!
//!  1. an energy-onset envelope at 200 Hz (half-wave rectified energy rise);
//!  2. its autocorrelation, summed over a few beat multiples with a gentle prior toward dance
//!     tempos, picks the tempo to ~1 %;
//!  3. a comb filter swept finely over tempo and phase refines both.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    /// Sample index of a beat (the first one at or after 0).
    pub offset: f64,
    /// 0…1-ish: how clearly the tempo stood out (peak over mean of the autocorrelation).
    pub confidence: f64,
}

impl BeatGrid {
    pub fn new(bpm: f64, offset: f64, confidence: f64) -> BeatGrid {
        BeatGrid {
            bpm,
            offset,
            confidence,
        }
    }

    pub fn samples_per_beat(&self, sample_rate: f64) -> f64 {
        sample_rate * 60.0 / self.bpm
    }
}

/// `BPMAnalyzer.analyze(_:range:)` over a buffer's channels (`right` = `left` for mono). The
/// default range is 70...180 BPM.
pub fn analyze(left: &[f32], right: &[f32], sample_rate: f64) -> Option<BeatGrid> {
    analyze_in(left, right, sample_rate, 70.0, 180.0)
}

pub fn analyze_in(
    left: &[f32],
    right: &[f32],
    sr: f64,
    range_lo: f64,
    range_hi: f64,
) -> Option<BeatGrid> {
    let length = left.len();
    let hop = ((sr / 200.0) as usize).max(1);
    let n = length / hop;
    if n <= 400 {
        return None; // < 2 s: nothing to measure
    }

    // Energy per hop, then its half-wave-rectified rise.
    let mut energy = vec![0.0f64; n];
    for (i, e) in energy.iter_mut().enumerate() {
        let mut sum = 0.0;
        let base = i * hop;
        for j in 0..hop {
            let l = left[base + j] as f64;
            let r = right[base + j] as f64;
            let m = (l + r) * 0.5;
            sum += m * m;
        }
        *e = libm::sqrt(sum / hop as f64);
    }
    let mut onset = vec![0.0f64; n];
    for i in 1..n {
        onset[i] = (energy[i] - energy[i - 1]).max(0.0);
    }
    let mean = onset.iter().fold(0.0, |a, b| a + b) / n as f64;
    if mean <= 1e-9 {
        return None;
    }
    let centred: Vec<f64> = onset.iter().map(|o| o - mean).collect();

    let env_rate = sr / hop as f64;
    let lag_min = (env_rate * 60.0 / range_hi) as usize;
    let lag_max = (n / 2).min((env_rate * 60.0 / range_lo) as usize);
    if lag_min < 2 || lag_max <= lag_min {
        return None;
    }

    // Autocorrelation up to 4 beats of the slowest tempo.
    let max_lag = (n - 1).min(lag_max * 4);
    let mut ac = vec![0.0f64; max_lag + 1];
    for lag in 1..=max_lag {
        let mut s = 0.0;
        for i in 0..(n - lag) {
            s += centred[i] * centred[i + lag];
        }
        ac[lag] = s / (n - lag) as f64;
    }
    let ac_at = |lag: f64| -> f64 {
        let l = lag as i64;
        if l < 1 || (l + 1) as usize > max_lag {
            return 0.0;
        }
        let l = l as usize;
        let f = lag - l as f64;
        ac[l] + (ac[l + 1] - ac[l]) * f
    };

    let mut best_score = f64::NEG_INFINITY;
    let mut best_lag = 0usize;
    for lag in lag_min..=lag_max {
        let mut s = ac_at(lag as f64);
        s += 0.5 * ac_at((lag * 2) as f64);
        s += 0.25 * ac_at((lag * 4) as f64);
        let bpm = env_rate * 60.0 / lag as f64;
        let prior = libm::exp(-0.5 * libm::pow(libm::log2(bpm / 125.0) / 0.7, 2.0));
        s *= 0.6 + 0.4 * prior;
        if s > best_score {
            best_score = s;
            best_lag = lag;
        }
    }
    if best_score <= 0.0 {
        return None;
    }
    let ac_mean =
        ac[lag_min..=lag_max].iter().fold(0.0, |a, b| a + b) / (lag_max - lag_min + 1) as f64;
    let coarse_bpm = env_rate * 60.0 / best_lag as f64;

    // Comb refinement: tempo ±2.5 % in 0.05 bpm steps, every phase.
    let interp = |x: f64| -> f64 {
        let i = x as i64;
        if i < 0 || (i + 1) as usize >= n {
            return 0.0;
        }
        let i = i as usize;
        onset[i] + (onset[i + 1] - onset[i]) * (x - i as f64)
    };
    let mut fine_score = f64::NEG_INFINITY;
    let mut fine_bpm = coarse_bpm;
    let mut fine_phase = 0.0;
    let mut bpm = coarse_bpm * 0.975;
    while bpm <= coarse_bpm * 1.025 {
        let period = env_rate * 60.0 / bpm;
        let mut phase = 0.0;
        while phase < period {
            let mut s = 0.0;
            let mut x = phase;
            while x < (n - 1) as f64 {
                s += interp(x);
                x += period;
            }
            if s > fine_score {
                fine_score = s;
                fine_bpm = bpm;
                fine_phase = phase;
            }
            phase += 0.5;
        }
        bpm += 0.05;
    }
    // Onset i = energy rose within hop i, so the event sits mid-hop.
    let offset = (fine_phase + 0.5) * hop as f64;
    let beat = sr * 60.0 / fine_bpm;
    Some(BeatGrid {
        bpm: fine_bpm,
        offset: libm::fmod(offset, beat),
        confidence: if ac_mean > 0.0 {
            best_score / (ac_mean * 1.0 + 1e-12)
        } else {
            0.0
        },
    })
}
