//! `Audio/PeakLimiter.swift`: a stereo-linked lookahead true-peak limiter, the last stage of a
//! net deck on the app's path (`WaveOptions.master == Limit`), after the runner, the brake and
//! the master chain. The track's loudness gain puts it at the app's reference level; this only
//! catches the peaks that would cross [`PeakLimiter::CEILING_DB`] (dBTP), so the drums keep
//! their attack instead of going through beats' `tanh` curve.
//!
//! Per sample: the level each incoming sample needs held (the larger of BS.1770's 4x
//! oversampled true peak, both channels, [`DELAY`] samples behind, and the sample itself at the
//! FIR's centre) and the gain that needs (`ceiling / tp`, 1 under it); the minimum of that over
//! the window (a monotonic queue); then two envelopes, a fast one that falls at once and
//! recovers in `FAST_RELEASE_SECONDS`, and a slow one that falls over `SLOW_ATTACK_SECONDS` and
//! recovers in `SLOW_RELEASE_SECONDS`; the lower of the two, then a moving average over
//! `LOOKAHEAD_SECONDS`, applied to the sample `lookahead` behind. Under the ceiling the output
//! is the input, `lookahead` samples late.
//!
//! Real-time safe: every buffer is allocated in [`PeakLimiter::new`]; `process` allocates,
//! locks and frees nothing.

use alloc::vec;
use alloc::vec::Vec;
use libm::{exp, floor, log10, pow};

use super::true_peak::{flat_coefficients, interpolated_peak, DELAY, TAPS};

pub struct PeakLimiter {
    pub sample_rate: f64,
    /// Samples of delay: the moving average's length plus the FIR's reach on both sides.
    pub lookahead: usize,
    window: usize,
    span: usize,
    ceiling: f64,
    fast_release: f64,
    slow_attack: f64,
    slow_release: f64,
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_pos: usize,
    coefficients: Vec<f64>,
    in_l: Vec<f64>,
    in_r: Vec<f64>,
    out_l: Vec<f64>,
    out_r: Vec<f64>,
    head: usize,
    queue_cap: usize,
    q_value: Vec<f64>,
    q_index: Vec<i64>,
    q_head: usize,
    q_count: usize,
    boxcar: Vec<f64>,
    box_pos: usize,
    box_sum: f64,
    fast: f64,
    slow: f64,
    n: i64,
    histogram: Vec<u64>,
    output_peak: f64,
    block_gr_db: f64,
}

/// `GainReductionStats`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GainReductionStats {
    pub percentiles_db: Vec<f64>,
    pub max_db: f64,
    pub limited_share: f64,
    pub true_peak_dbtp: f64,
}

impl PeakLimiter {
    /// True-peak ceiling (dBTP): -1 leaves room for the deck's EQ boost and a lossy encode.
    pub const CEILING_DB: f64 = -1.0;
    /// Held under the ceiling by this much: the gain moves across the FIR's 12 taps.
    pub const CEILING_MARGIN_DB: f64 = 0.05;
    pub const LOOKAHEAD_SECONDS: f64 = 0.0015;
    pub const FAST_RELEASE_SECONDS: f64 = 0.015;
    pub const SLOW_ATTACK_SECONDS: f64 = 0.03;
    pub const SLOW_RELEASE_SECONDS: f64 = 0.25;
    pub const HISTOGRAM_BINS: usize = 301;

    pub fn new(sample_rate: f64) -> PeakLimiter {
        let window = {
            let w = floor(Self::LOOKAHEAD_SECONDS * sample_rate + 0.5) as usize;
            w.max(1)
        };
        let lookahead = window + 2 * DELAY;
        let span = window + 2 * DELAY;
        let queue_cap = span + 2;
        PeakLimiter {
            sample_rate,
            lookahead,
            window,
            span,
            ceiling: pow(10.0, (Self::CEILING_DB - Self::CEILING_MARGIN_DB) / 20.0),
            fast_release: exp(-1.0 / (Self::FAST_RELEASE_SECONDS * sample_rate)),
            slow_attack: 1.0 - exp(-1.0 / (Self::SLOW_ATTACK_SECONDS * sample_rate)),
            slow_release: 1.0 - exp(-1.0 / (Self::SLOW_RELEASE_SECONDS * sample_rate)),
            delay_l: vec![0.0; lookahead],
            delay_r: vec![0.0; lookahead],
            delay_pos: 0,
            coefficients: flat_coefficients(),
            in_l: vec![0.0; 2 * TAPS],
            in_r: vec![0.0; 2 * TAPS],
            out_l: vec![0.0; 2 * TAPS],
            out_r: vec![0.0; 2 * TAPS],
            head: 0,
            queue_cap,
            q_value: vec![1.0; queue_cap],
            q_index: vec![0; queue_cap],
            q_head: 0,
            q_count: 0,
            boxcar: vec![1.0; window],
            box_pos: 0,
            box_sum: window as f64,
            fast: 1.0,
            slow: 1.0,
            n: 0,
            histogram: vec![0; Self::HISTOGRAM_BINS],
            output_peak: 0.0,
            block_gr_db: 0.0,
        }
    }

    /// `lookahead` in seconds: how late a net deck's audio is against its transport.
    pub fn latency_seconds(&self) -> f64 {
        self.lookahead as f64 / self.sample_rate
    }

    /// Limit `frames` samples of `left` (and `right`) in place. Audio thread.
    pub fn process(&mut self, left: &mut [f32], mut right: Option<&mut [f32]>, frames: usize) {
        let mut deepest = 1.0f64;
        let c = self.ceiling as f32;
        let stereo = right.is_some();
        let mut out_peak = self.output_peak;
        for i in 0..frames {
            let xl = left[i];
            let xr = match right.as_deref() {
                Some(r) => r[i],
                None => xl,
            };
            self.head = if self.head == 0 {
                TAPS - 1
            } else {
                self.head - 1
            };
            let h = self.head;
            self.in_l[h] = xl as f64;
            self.in_l[h + TAPS] = xl as f64;
            self.in_r[h] = xr as f64;
            self.in_r[h + TAPS] = xr as f64;
            // the true peak around the sample `DELAY` behind, and that sample itself
            let mut x = interpolated_peak(&self.coefficients, &self.in_l[h..])
                .max(self.in_l[h + DELAY].abs());
            if stereo {
                x = x
                    .max(interpolated_peak(&self.coefficients, &self.in_r[h..]))
                    .max(self.in_r[h + DELAY].abs());
            }
            let need = if x > self.ceiling {
                self.ceiling / x
            } else {
                1.0
            };
            // the window's minimum: drop larger values from the back, stale ones from the front
            while self.q_count > 0 {
                let back = (self.q_head + self.q_count - 1) % self.queue_cap;
                if self.q_value[back] >= need {
                    self.q_count -= 1;
                } else {
                    break;
                }
            }
            let slot = (self.q_head + self.q_count) % self.queue_cap;
            self.q_value[slot] = need;
            self.q_index[slot] = self.n;
            self.q_count += 1;
            while self.q_index[self.q_head] < self.n - self.span as i64 {
                self.q_head = if self.q_head + 1 == self.queue_cap {
                    0
                } else {
                    self.q_head + 1
                };
                self.q_count -= 1;
            }
            let m = self.q_value[self.q_head];
            self.fast = if m < self.fast {
                m
            } else {
                m + (self.fast - m) * self.fast_release
            };
            self.slow += (m - self.slow)
                * if m < self.slow {
                    self.slow_attack
                } else {
                    self.slow_release
                };
            let env = self.fast.min(self.slow);
            self.box_sum += env - self.boxcar[self.box_pos];
            self.boxcar[self.box_pos] = env;
            self.box_pos += 1;
            if self.box_pos == self.window {
                self.box_pos = 0;
                // re-add exactly once per window, so the running sum never drifts
                let mut s = 0.0;
                for k in 0..self.window {
                    s += self.boxcar[k];
                }
                self.box_sum = s;
            }
            let g = (self.box_sum / self.window as f64).min(1.0);
            if g < deepest {
                deepest = g;
            }
            let gf = g as f32;
            let ol = self.delay_l[self.delay_pos];
            let or = self.delay_r[self.delay_pos];
            self.delay_l[self.delay_pos] = xl;
            self.delay_r[self.delay_pos] = xr;
            self.delay_pos = if self.delay_pos + 1 == self.lookahead {
                0
            } else {
                self.delay_pos + 1
            };
            let yl = (ol * gf).max(-c).min(c);
            let yr = (or * gf).max(-c).min(c);
            left[i] = yl;
            if let Some(r) = right.as_deref_mut() {
                r[i] = yr;
            }
            // the output's true peak (what `statistics` reports)
            self.out_l[h] = yl as f64;
            self.out_l[h + TAPS] = yl as f64;
            self.out_r[h] = yr as f64;
            self.out_r[h + TAPS] = yr as f64;
            out_peak = out_peak.max(interpolated_peak(&self.coefficients, &self.out_l[h..]));
            if stereo {
                out_peak = out_peak.max(interpolated_peak(&self.coefficients, &self.out_r[h..]));
            }
            let bin = if g >= 1.0 {
                0
            } else {
                ((-200.0 * log10(g)) as usize).min(Self::HISTOGRAM_BINS - 1)
            };
            self.histogram[bin] += 1;
            self.n += 1;
        }
        self.output_peak = out_peak;
        self.block_gr_db = if deepest >= 1.0 {
            0.0
        } else {
            -20.0 * log10(deepest)
        };
    }

    /// Deepest gain reduction of the last block, dB (0 = not limiting).
    pub fn gain_reduction_db(&self) -> f64 {
        self.block_gr_db
    }

    /// `statistics`: gain reduction over everything processed since the last
    /// `reset_statistics`. Not for the audio thread.
    pub fn statistics(&self, percentiles: &[f64]) -> GainReductionStats {
        let total: u64 = self.histogram.iter().sum();
        if total == 0 {
            return GainReductionStats::default();
        }
        let mut out = GainReductionStats::default();
        out.true_peak_dbtp = if self.output_peak > 0.0 {
            20.0 * log10(self.output_peak)
        } else {
            -200.0
        };
        let mut max_bin = 0;
        for (b, &c) in self.histogram.iter().enumerate() {
            if c > 0 {
                max_bin = b;
            }
        }
        out.max_db = (max_bin + 1) as f64 / 10.0;
        out.limited_share = (total - self.histogram[0]) as f64 / total as f64;
        for p in percentiles {
            let want = libm::ceil(p * total as f64) as u64;
            let (mut seen, mut at) = (0u64, 0usize);
            for (b, &c) in self.histogram.iter().enumerate() {
                seen += c;
                if seen >= want {
                    at = b;
                    break;
                }
            }
            out.percentiles_db
                .push(if at == 0 { 0.0 } else { at as f64 / 10.0 });
        }
        out
    }

    pub fn reset_statistics(&mut self) {
        for c in self.histogram.iter_mut() {
            *c = 0;
        }
        self.output_peak = 0.0;
    }
}
