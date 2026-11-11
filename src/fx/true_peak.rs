//! `Musicality/TruePeak.swift`: ITU-R BS.1770-4 Annex 2 true peak. Each channel is
//! oversampled 4x through the annex's 48-tap polyphase FIR (four phases of 12) and the largest
//! magnitude that comes out is the true peak. It catches the peaks between samples that a
//! sample-peak meter misses. [`Detector`] is the streaming form (one per channel, a fixed
//! 12-sample history, nothing allocated per sample); its output lags the input by [`DELAY`].
//! [`PeakLimiter`](super::limiter::PeakLimiter) runs the same FIR (`interpolated_peak`) on its
//! own buffers.
//!
//! This is the crate's one true peak: [`crate::loudness`] (`true_peak`, `dbtp`,
//! `TruePeakDetector`) and [`crate::musicality::loudness`] (`TruePeak`, `TruePeakDetector`) are
//! names for these items, not copies of them.

use alloc::vec;
use alloc::vec::Vec;
use libm::log10;

/// Taps per phase.
pub const TAPS: usize = 12;
/// Samples the oversampled output lags the input: the FIR's centre.
pub const DELAY: usize = 6;

/// `TruePeak.phases`: the annex's coefficients, phase by phase.
pub const PHASES: [[f64; TAPS]; 4] = [
    [
        0.0017089843750,
        0.0109863281250,
        -0.0196533203125,
        0.0332031250000,
        -0.0594482421875,
        0.1373291015625,
        0.9721679687500,
        -0.1022949218750,
        0.0476074218750,
        -0.0266113281250,
        0.0148925781250,
        -0.0083007812500,
    ],
    [
        -0.0291748046875,
        0.0292968750000,
        -0.0517578125000,
        0.0891113281250,
        -0.1665039062500,
        0.4650878906250,
        0.7797851562500,
        -0.2003173828125,
        0.1015625000000,
        -0.0582275390625,
        0.0330810546875,
        -0.0189208984375,
    ],
    [
        -0.0189208984375,
        0.0330810546875,
        -0.0582275390625,
        0.1015625000000,
        -0.2003173828125,
        0.7797851562500,
        0.4650878906250,
        -0.1665039062500,
        0.0891113281250,
        -0.0517578125000,
        0.0292968750000,
        -0.0291748046875,
    ],
    [
        -0.0083007812500,
        0.0148925781250,
        -0.0266113281250,
        0.0476074218750,
        -0.1022949218750,
        0.9721679687500,
        0.1373291015625,
        -0.0594482421875,
        0.0332031250000,
        -0.0196533203125,
        0.0109863281250,
        0.0017089843750,
    ],
];

/// `TruePeak.flatCoefficients`: `PHASES`, phase-major (4 x [`TAPS`]).
pub fn flat_coefficients() -> Vec<f64> {
    PHASES.iter().flatten().copied().collect()
}

/// `TruePeak.interpolatedPeak`: the largest magnitude of the four oversampled outputs for one
/// input. `coefficients` is [`flat_coefficients`], `history[k]` the input `k` samples ago
/// ([`TAPS`] of them, newest first). Allocates nothing.
#[inline]
pub fn interpolated_peak(coefficients: &[f64], history: &[f64]) -> f64 {
    let mut m = 0.0f64;
    for p in 0..4 {
        let mut y = 0.0;
        for k in 0..TAPS {
            y += coefficients[p * TAPS + k] * history[k];
        }
        let a = if y < 0.0 { -y } else { y };
        if a > m {
            m = a;
        }
    }
    m
}

/// `TruePeak.Detector`: one channel's 4x oversampled peak, fed a sample at a time.
#[derive(Clone, Debug)]
pub struct Detector {
    coefficients: Vec<f64>,
    /// The last 12 inputs, twice over, so a read never wraps: newest at `head` and `head + 12`.
    history: Vec<f64>,
    head: usize,
    /// The largest magnitude seen since the last `reset`, linear.
    pub peak: f64,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Detector {
        Detector {
            coefficients: flat_coefficients(),
            history: vec![0.0; 2 * TAPS],
            head: 0,
            peak: 0.0,
        }
    }

    /// Takes one input sample; returns the largest magnitude of the four oversampled outputs it
    /// produces.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.head = if self.head == 0 {
            TAPS - 1
        } else {
            self.head - 1
        };
        self.history[self.head] = x;
        self.history[self.head + TAPS] = x;
        let m = interpolated_peak(&self.coefficients, &self.history[self.head..]);
        if m > self.peak {
            self.peak = m;
        }
        m
    }

    /// Back to silence: history cleared, `peak` 0.
    pub fn reset(&mut self) {
        for v in self.history.iter_mut() {
            *v = 0.0;
        }
        self.head = 0;
        self.peak = 0.0;
    }
}

/// `TruePeak.peak(left:right:)`: true peak over both channels, linear (`right` None = mono).
/// The FIR's tail is flushed, so a peak in the last samples counts.
pub fn peak(left: &[f32], right: Option<&[f32]>) -> f64 {
    fn channel(x: &[f32]) -> f64 {
        let mut d = Detector::new();
        for &v in x {
            d.process(v as f64);
        }
        for _ in 0..TAPS {
            d.process(0.0);
        }
        d.peak
    }
    let l = channel(left);
    let r = right.map(channel).unwrap_or(0.0);
    if l > r {
        l
    } else {
        r
    }
}

/// `TruePeak.dbtp`: true peak in dBTP; -200 for silence.
pub fn dbtp(left: &[f32], right: Option<&[f32]>) -> f64 {
    let p = peak(left, right);
    if p > 0.0 {
        20.0 * log10(p)
    } else {
        -200.0
    }
}
