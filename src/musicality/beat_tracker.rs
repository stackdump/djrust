//! Port of `Musicality/BeatTracker.swift`: tempo, beats and downbeats of a render (ROADMAP
//! 6.1: a reference's section lengths are counted in its bars). Onset envelope (log-magnitude
//! spectral flux), tempo by autocorrelation with a log-Gaussian prior, beats by Ellis'
//! dynamic programme, downbeats by the strongest low-band phase. Offline. The FFT is
//! `musicality::fft` (f64) where Swift uses vDSP (f32): same algorithm, float-precision
//! differences.

use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::fft::{hann_norm, Fft};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeatTracker {
    /// Tempo, BPM (refined from the beats).
    pub bpm: f64,
    /// Beat times, seconds from the render's first sample.
    pub beats: Vec<f64>,
    /// Downbeat times (every fourth beat, from the strongest low-band phase).
    pub downbeats: Vec<f64>,
    /// Index in `beats` of the first downbeat (0…3).
    pub downbeat_phase: usize,
    /// How periodic the onsets are at `bpm`, 0…1.
    pub confidence: f64,
}

impl BeatTracker {
    pub const MIN_BPM: f64 = 70.0;
    pub const MAX_BPM: f64 = 180.0;
    pub const PRIOR_BPM: f64 = 120.0;
    pub const PRIOR_OCTAVES: f64 = 1.0;
    pub const HOP_SECONDS: f64 = 0.005;
    pub const LOW_BAND_HZ: f64 = 150.0;
    pub const TIGHTNESS: f64 = 100.0;
    /// How late the flux peak's frame centre reads an onset, seconds.
    pub const ONSET_LATENCY: f64 = 0.016;
    /// The double tempo wins when its autocorrelation is at least this share of the pick's.
    pub const DOUBLE_RATIO: f64 = 0.65;

    /// From the mid of the two channels.
    pub fn from_stereo(left: &[f32], right: &[f32], sr: f64) -> BeatTracker {
        let n = left.len().min(right.len());
        let mono: Vec<f32> = (0..n).map(|i| 0.5 * (left[i] + right[i])).collect();
        Self::new(&mono, sr)
    }

    pub fn new(x: &[f32], sr: f64) -> BeatTracker {
        let mut t = BeatTracker::default();
        if sr <= 0.0 {
            return t;
        }
        let hop = (libm::round(sr * Self::HOP_SECONDS) as usize).max(1);
        let fps = sr / hop as f64;
        let env = Self::onsets(x, sr, hop);
        if env.0.len() <= (fps * 4.0) as usize {
            return t;
        }
        let full = Self::normalise(&env.0, fps);

        // tempo: autocorrelation with its first harmonic, prior-weighted
        let min_lag = libm::floor(60.0 / Self::MAX_BPM * fps) as usize;
        let max_lag = libm::ceil(60.0 / Self::MIN_BPM * fps) as usize;
        if full.len() <= max_lag * 4 {
            return t;
        }
        let ac = Self::autocorrelation(&full, 2 * max_lag + 2);
        if ac[0] <= 0.0 {
            return t;
        }
        let mut best = min_lag;
        let mut best_score = f64::NEG_INFINITY;
        for lag in min_lag..=max_lag {
            let bpm = 60.0 * fps / lag as f64;
            let o = libm::log2(bpm / Self::PRIOR_BPM) / Self::PRIOR_OCTAVES;
            let score = (ac[lag] + ac[2 * lag]) * libm::exp(-0.5 * o * o);
            if score > best_score {
                best_score = score;
                best = lag;
            }
        }
        // half and double tempo sit equally under the prior: take the faster pulse when it
        // is nearly as periodic
        loop {
            let h = libm::round(best as f64 / 2.0) as i64;
            if h - 1 < min_lag as i64 {
                break;
            }
            // `max(by: <)` keeps the first of equal maxima
            let mut lag = (h - 1) as usize;
            for c in (h as usize)..=(h as usize + 1) {
                if ac[lag] < ac[c] {
                    lag = c;
                }
            }
            if ac[lag] < Self::DOUBLE_RATIO * ac[best] {
                break;
            }
            best = lag;
        }
        let mut period = best as f64;
        if best > 1 {
            let (a, b, c) = (ac[best - 1], ac[best], ac[best + 1]);
            let d = a - 2.0 * b + c;
            if d < 0.0 {
                period += (0.5 * (a - c) / d).max(-0.5).min(0.5);
            }
        }
        t.confidence = ((ac[best] + ac[2 * best]) / (2.0 * ac[0]))
            .max(0.0)
            .min(1.0);

        // beats: Ellis' DP on the whole band, then the phase the kick agrees with
        let low = Self::normalise(&env.1, fps);
        let mut frames = Self::track(&full, period);
        let shift = libm::round(period / 2.0) as i64;
        let reach = (shift - 1).min((0.1 * fps) as i64);
        let on_beat = Self::window_sum(&low, &frames, reach);
        let shifted: Vec<i64> = frames.iter().map(|f| f + shift).collect();
        let off_beat = Self::window_sum(&low, &shifted, reach);
        if off_beat > on_beat {
            frames = shifted
                .into_iter()
                .filter(|&f| (f as usize) < full.len())
                .collect();
        }
        // frame f covers samples [f·hop, f·hop + frameSize): its centre, plus the flux's latency
        let offset = Self::frame_size(sr) as f64 / 2.0 / sr + Self::ONSET_LATENCY;
        t.beats = frames.iter().map(|&f| f as f64 / fps + offset).collect();

        // tempo refined: a line through the beats
        if t.beats.len() >= 8 {
            let m = t.beats.len() as f64;
            let mi = (m - 1.0) / 2.0;
            let mt = t.beats.iter().sum::<f64>() / m;
            let (mut sxy, mut sxx) = (0.0, 0.0);
            for (i, tt) in t.beats.iter().enumerate() {
                sxy += (i as f64 - mi) * (tt - mt);
                sxx += (i as f64 - mi) * (i as f64 - mi);
            }
            let slope = sxy / sxx;
            t.bpm = if slope > 0.0 {
                60.0 / slope
            } else {
                60.0 * fps / period
            };
        } else {
            t.bpm = 60.0 * fps / period;
        }

        // downbeats: the phase whose beats hold the most kick
        let mut phase_sum = [0.0f64; 4];
        for (i, f) in frames.iter().enumerate() {
            phase_sum[i % 4] += Self::window_sum(&low, &[*f], reach);
        }
        let mut phase = 0;
        for p in 1..4 {
            if phase_sum[phase] < phase_sum[p] {
                phase = p;
            }
        }
        t.downbeat_phase = phase;
        t.downbeats = (phase..t.beats.len())
            .step_by(4)
            .map(|i| t.beats[i])
            .collect();
        t
    }

    /// The analysis frame: the power of two nearest 46 ms.
    pub fn frame_size(sr: f64) -> usize {
        1usize << (libm::round(libm::log2(sr * 0.046)) as i64).max(8)
    }

    /// Spectral flux of log magnitudes, whole spectrum and under `LOW_BAND_HZ`, one per hop.
    fn onsets(x: &[f32], sr: f64, hop: usize) -> (Vec<f64>, Vec<f64>) {
        let size = Self::frame_size(sr);
        let half = size / 2;
        if x.len() < size {
            return (Vec::new(), Vec::new());
        }
        let count = (x.len() - size) / hop + 1;
        let low_bins = ((Self::LOW_BAND_HZ * size as f64 / sr) as usize).max(2);
        let window = hann_norm(size);
        let mut fft = Fft::new(size);
        let mut buf = vec![0.0f64; size];
        let mut prev = vec![0.0f64; half];
        let mut mag = vec![0.0f64; half];
        let mut full = vec![0.0f64; count];
        let mut low = vec![0.0f64; count];
        for f in 0..count {
            for i in 0..size {
                buf[i] = x[f * hop + i] as f64 * window[i];
            }
            let p = fft.zrip(&buf);
            for k in 0..half {
                // (im[0] = 0: the Nyquist bin is dropped, as in Swift)
                mag[k] = libm::log1p(100.0 * libm::sqrt(p.re[k] * p.re[k] + p.im[k] * p.im[k]));
            }
            let (mut s, mut sl) = (0.0, 0.0);
            for k in 0..half {
                let d = (mag[k] - prev[k]).max(0.0);
                s += d;
                if k < low_bins {
                    sl += d;
                }
            }
            if f > 0 {
                full[f] = s;
                low[f] = sl;
            }
            core::mem::swap(&mut prev, &mut mag);
        }
        (full, low)
    }

    /// Minus a 0.5 s moving mean, half-wave rectified, scaled to unit standard deviation.
    pub fn normalise(e: &[f64], fps: f64) -> Vec<f64> {
        let n = e.len();
        if n == 0 {
            return e.to_vec();
        }
        let w = ((fps * 0.25) as usize).max(1);
        let mut prefix = vec![0.0f64; n + 1];
        for i in 0..n {
            prefix[i + 1] = prefix[i] + e[i];
        }
        let mut out = vec![0.0f64; n];
        for i in 0..n {
            let a = i.saturating_sub(w);
            let b = n.min(i + w + 1);
            out[i] = (e[i] - (prefix[b] - prefix[a]) / (b - a) as f64).max(0.0);
        }
        let mean = out.iter().sum::<f64>() / n as f64;
        let sd = libm::sqrt(out.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64);
        if sd > 0.0 {
            for x in out.iter_mut() {
                *x /= sd;
            }
        }
        out
    }

    /// Mean-removed autocorrelation, lags 0..<`max_lag`, each normalised by its overlap.
    pub fn autocorrelation(e: &[f64], max_lag: usize) -> Vec<f64> {
        let n = e.len();
        let mean = e.iter().sum::<f64>() / n as f64;
        let x: Vec<f64> = e.iter().map(|v| v - mean).collect();
        let mut ac = vec![0.0f64; max_lag];
        for lag in 0..max_lag.min(n) {
            let mut s = 0.0;
            for i in 0..n - lag {
                s += x[i] * x[i + lag];
            }
            ac[lag] = s / (n - lag) as f64;
        }
        ac
    }

    /// Σ over `frames` of the envelope from each frame to `reach` frames after it.
    pub fn window_sum(e: &[f64], frames: &[i64], reach: i64) -> f64 {
        let mut sum = 0.0;
        for &f in frames {
            if f >= 0 && (f as usize) < e.len() {
                let end = (e.len() as i64 - 1).min(f + reach);
                for k in f..=end {
                    sum += e[k as usize];
                }
            }
        }
        sum
    }

    /// Ellis' beat DP over the onset envelope at `period` frames: the beat frames, in order.
    pub fn track(onset: &[f64], period: f64) -> Vec<i64> {
        let n = onset.len();
        if n == 0 || period < 2.0 {
            return Vec::new();
        }
        let mut score = onset.to_vec();
        let mut back = vec![-1i64; n];
        let lo = libm::floor(period / 2.0) as i64;
        let hi = libm::ceil(period * 2.0) as i64;
        for t in 0..n as i64 {
            let mut best = f64::NEG_INFINITY;
            let mut arg = -1i64;
            let first = (t - hi).max(0);
            let last = t - lo;
            if last >= first {
                for tau in first..=last {
                    let r = libm::log((t - tau) as f64 / period);
                    let s = score[tau as usize] - Self::TIGHTNESS * r * r;
                    if s > best {
                        best = s;
                        arg = tau;
                    }
                }
            }
            if arg >= 0 {
                score[t as usize] = onset[t as usize] + best;
                back[t as usize] = arg;
            }
        }
        // the end: the best-scoring frame in the last period
        let tail_start = n.saturating_sub(libm::ceil(period) as usize);
        let mut end = tail_start;
        for t in tail_start..n {
            if score[t] > score[end] {
                end = t;
            }
        }
        let mut out = Vec::new();
        let mut t = end as i64;
        while t >= 0 {
            out.push(t);
            t = back[t as usize];
        }
        out.reverse();
        out
    }
}
