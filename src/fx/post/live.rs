//! A post chain on live audio: the same stages, built once on the main thread, run block by
//! block on the audio thread. The loudness stage is the [`Leveler`](super::dsp::Leveler)
//! (short-term LUFS AGC + true-peak limiter), since two passes need the whole file.

use alloc::vec::Vec;

use super::chain::PostChain;
use super::dsp::Running;

/// What a live chain reads back for a meter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveMeter {
    /// The leveler's gain now, dB (0 without one).
    pub gain_db: f64,
    /// The leveler's short-term loudness (its input), LUFS.
    pub short_term_lufs: f64,
    /// The limiter's gain reduction at the last block, dB (≤ 0).
    pub limiter_db: f64,
    /// The denoise's noise estimate, dBFS (None without one).
    pub noise_db: Option<f64>,
}

pub struct LivePost {
    pub chain: PostChain,
    stages: Vec<Running>,
    latency: usize,
}

impl LivePost {
    /// Build `chain` for live use at `sr`. Allocates: call it off the audio thread.
    pub fn new(chain: &PostChain, sr: f64) -> LivePost {
        let stages: Vec<Running> = chain
            .stages
            .iter()
            .filter_map(|s| Running::build(s, sr, true, None))
            .collect();
        let latency = stages.iter().map(Running::latency).sum();
        LivePost {
            chain: chain.clone(),
            stages,
            latency,
        }
    }

    /// Samples the chain delays the audio by (the denoise's FFT, the limiter's lookahead).
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Process one block in place. Real-time safe.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        for s in self.stages.iter_mut() {
            s.process(&mut l[..n], &mut r[..n]);
        }
    }

    pub fn meter(&self) -> LiveMeter {
        let mut m = LiveMeter {
            short_term_lufs: -200.0,
            ..Default::default()
        };
        for s in &self.stages {
            match s {
                Running::Leveler(v) => {
                    m.gain_db = v.gain_db;
                    m.short_term_lufs = v.short_term_lufs;
                    m.limiter_db = v.limiter_reduction_db();
                }
                Running::Limiter(v) => m.limiter_db = v.gain_reduction_db(),
                Running::Denoise(d) => m.noise_db = Some(d.noise_db()),
                _ => {}
            }
        }
        m
    }
}
