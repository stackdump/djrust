//! Port of `Musicality/SectionSegmenter.swift`: section boundaries of a render or a reference
//! file, in its own bars (ROADMAP 6.1). Bars from `BeatTracker`'s downbeats; per-bar features
//! (log-spaced band energies, a 12-bin chroma, the bar's RMS), z-scored; a cosine
//! self-similarity matrix; Foote novelty with a Gaussian-tapered checkerboard kernel; peaks.
//! Offline; the FFT is `musicality::fft` (f64) where Swift uses vDSP.

use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::beat_tracker::BeatTracker;
use crate::musicality::fft::{hann_norm, Fft};

#[derive(Clone, Debug, PartialEq)]
pub struct SectionSegmenter {
    /// Bar start times, seconds (the bars measured).
    pub bar_starts: Vec<f64>,
    /// The bar the tracker or caller gave, seconds (median).
    pub bar_seconds: f64,
    /// Tempo, BPM, when the bars came from `BeatTracker` (else 0).
    pub bpm: f64,
    /// The kernel used, bars a side.
    pub kernel: usize,
    /// Cosine self-similarity of the bars' feature vectors.
    pub similarity: Vec<Vec<f64>>,
    /// Foote novelty at each bar's start (0 where the kernel does not fit).
    pub novelty: Vec<f64>,
    /// Section starts, bar indices (bar 0 is the start of the first section, not listed).
    pub boundaries: Vec<usize>,
}

impl SectionSegmenter {
    pub const KERNEL_BARS: usize = 4;
    pub const BAND_COUNT: usize = 8;
    pub const MIN_HZ: f64 = 40.0;
    pub const MAX_HZ: f64 = 16_000.0;
    pub const CHROMA_MIN_HZ: f64 = 60.0;
    pub const CHROMA_MAX_HZ: f64 = 4_000.0;
    /// Bars the kernel needs on each side of a boundary.
    pub const MIN_SIDE: usize = 2;
    /// A peak must reach this many times the median novelty.
    pub const THRESHOLD: f64 = 1.25;
    /// The least novelty a peak must reach (cosine units, -2…2).
    pub const MIN_NOVELTY: f64 = 0.05;
    /// The least standard deviation a feature is scaled by.
    pub const SD_FLOOR_DB: f64 = 1.0;
    pub const SD_FLOOR_CHROMA: f64 = 0.01;
    /// The least norm a bar's vector is divided by in the similarity.
    pub const NORM_FLOOR: f64 = 0.5;

    fn empty(kernel: usize) -> SectionSegmenter {
        SectionSegmenter {
            bar_starts: Vec::new(),
            bar_seconds: 0.0,
            bpm: 0.0,
            kernel: kernel.max(1),
            similarity: Vec::new(),
            novelty: Vec::new(),
            boundaries: Vec::new(),
        }
    }

    /// Section starts, seconds.
    pub fn boundary_times(&self) -> Vec<f64> {
        self.boundaries
            .iter()
            .map(|&b| self.bar_starts[b])
            .collect()
    }

    /// The sections' lengths in bars, first to last (they sum to the bars measured).
    pub fn section_bars(&self) -> Vec<usize> {
        if self.bar_starts.is_empty() {
            return Vec::new();
        }
        let mut edges = vec![0usize];
        edges.extend(self.boundaries.iter().copied());
        edges.push(self.bar_starts.len());
        edges.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// Bars from `BeatTracker`'s downbeats over the mid of the two channels.
    pub fn from_stereo(
        left: &[f32],
        right: &[f32],
        sr: f64,
        kernel_bars: usize,
    ) -> SectionSegmenter {
        let n = left.len().min(right.len());
        let mono: Vec<f32> = (0..n).map(|i| 0.5 * (left[i] + right[i])).collect();
        Self::from_mono(&mono, sr, kernel_bars)
    }

    /// Bars from `BeatTracker`'s downbeats.
    pub fn from_mono(x: &[f32], sr: f64, kernel_bars: usize) -> SectionSegmenter {
        let b = BeatTracker::new(x, sr);
        let mut s = Self::with_downbeats(x, sr, &b.downbeats, kernel_bars);
        s.bpm = b.bpm;
        s
    }

    /// Bars on a known grid: `downbeats` (seconds, ascending), e.g. a render's own bar lines.
    pub fn with_downbeats(
        x: &[f32],
        sr: f64,
        downbeats: &[f64],
        kernel_bars: usize,
    ) -> SectionSegmenter {
        let mut s = Self::empty(kernel_bars);
        if sr <= 0.0 || downbeats.len() < 2 {
            return s;
        }
        let end = x.len() as f64 / sr;
        let mut gaps: Vec<f64> = downbeats.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        s.bar_seconds = gaps[gaps.len() / 2];
        let mut starts: Vec<f64> = downbeats
            .iter()
            .copied()
            .filter(|&d| d >= 0.0 && d < end)
            .collect();
        if let Some(&last) = starts.last() {
            if last + s.bar_seconds > end + 0.25 * s.bar_seconds {
                starts.pop();
            }
        }
        if starts.len() < 2 {
            return s;
        }
        s.bar_starts = starts;
        let features = Self::features(x, sr, &s.bar_starts, s.bar_seconds);
        s.similarity = Self::cosine_similarity(&features);
        s.novelty = Self::novelty(&s.similarity, s.kernel);
        s.boundaries = Self::peaks(&s.novelty, (s.kernel / 2).max(1));
        s
    }

    /// The analysis frame: the power of two nearest 93 ms.
    pub fn frame_size(sr: f64) -> usize {
        1usize << (libm::round(libm::log2(sr * 0.093)) as i64).max(9)
    }

    /// One vector per bar: bands (dB), chroma (sums to 1), RMS (dB); each dimension z-scored
    /// across bars, each group scaled by 1/√dims.
    pub fn features(x: &[f32], sr: f64, bar_starts: &[f64], bar_seconds: f64) -> Vec<Vec<f64>> {
        let bars = bar_starts.len();
        let size = Self::frame_size(sr);
        let half = size / 2;
        let hop = half;
        let bin_hz = sr / size as f64;
        // band and pitch class of each bin (-1: outside)
        let edges: Vec<f64> = (0..=Self::BAND_COUNT)
            .map(|i| {
                Self::MIN_HZ
                    * libm::pow(
                        Self::MAX_HZ / Self::MIN_HZ,
                        i as f64 / Self::BAND_COUNT as f64,
                    )
            })
            .collect();
        let mut band_of = vec![-1i64; half];
        let mut class_of = vec![-1i64; half];
        for k in 1..half {
            let f = k as f64 * bin_hz;
            if f >= Self::MIN_HZ && f < Self::MAX_HZ.min(sr / 2.0) {
                let last = edges.iter().rposition(|&e| e <= f).unwrap_or(0);
                band_of[k] = last.min(Self::BAND_COUNT - 1) as i64;
            }
            if f >= Self::CHROMA_MIN_HZ && f < Self::CHROMA_MAX_HZ {
                let midi = 69.0 + 12.0 * libm::log2(f / 440.0);
                class_of[k] = ((libm::round(midi) as i64 % 12) + 12) % 12;
            }
        }
        let mut band_sum = vec![vec![0.0f64; Self::BAND_COUNT]; bars];
        let mut chroma = vec![vec![0.0f64; 12]; bars];
        let mut frames = vec![0usize; bars];
        if x.len() >= size {
            let window = hann_norm(size);
            let mut fft = Fft::new(size);
            let mut buf = vec![0.0f64; size];
            for b in 0..bars {
                // Frames start on the bar's own first sample, so every bar is cut alike.
                let a = (libm::round(bar_starts[b] * sr) as i64).max(0) as usize;
                let bar_end = if b + 1 < bars {
                    libm::round(bar_starts[b + 1] * sr) as i64
                } else {
                    libm::round((bar_starts[b] + bar_seconds) * sr) as i64
                };
                let z = (x.len() as i64).min(bar_end).max(0) as usize;
                let mut start = a;
                while start + size <= z || (start == a && start + size <= x.len()) {
                    for i in 0..size {
                        buf[i] = x[start + i] as f64 * window[i];
                    }
                    let p = fft.zrip(&buf);
                    for k in 1..half {
                        let m = libm::sqrt(p.re[k] * p.re[k] + p.im[k] * p.im[k]);
                        if band_of[k] >= 0 {
                            band_sum[b][band_of[k] as usize] += m * m;
                        }
                        if class_of[k] >= 0 {
                            chroma[b][class_of[k] as usize] += m;
                        }
                    }
                    frames[b] += 1;
                    start += hop;
                }
            }
        }
        // RMS straight from the samples
        let mut rms = vec![0.0f64; bars];
        for b in 0..bars {
            let a = ((bar_starts[b] * sr) as i64).max(0).min(x.len() as i64) as usize;
            let e = if b + 1 < bars {
                (bar_starts[b + 1] * sr) as i64
            } else {
                ((bar_starts[b] + bar_seconds) * sr) as i64
            };
            let z = a.max((x.len() as i64).min(e).max(0) as usize);
            let mut ms = 0.0f64;
            if z > a {
                for v in &x[a..z] {
                    ms += *v as f64 * *v as f64;
                }
                ms /= (z - a) as f64;
            }
            rms[b] = 10.0 * libm::log10(ms + 1e-12);
        }
        let mut bands = band_sum.clone();
        for b in 0..bars {
            let n = frames[b].max(1) as f64;
            bands[b] = band_sum[b]
                .iter()
                .map(|s| 10.0 * libm::log10(s / n + 1e-12))
                .collect();
            let total: f64 = chroma[b].iter().sum();
            chroma[b] = if total > 0.0 {
                chroma[b].iter().map(|c| c / total).collect()
            } else {
                vec![1.0 / 12.0; 12]
            };
        }
        let rms_rows: Vec<Vec<f64>> = rms.iter().map(|&r| vec![r]).collect();
        let groups = [
            Self::z_scored(&bands, Self::SD_FLOOR_DB),
            Self::z_scored(&chroma, Self::SD_FLOOR_CHROMA),
            Self::z_scored(&rms_rows, Self::SD_FLOOR_DB),
        ];
        (0..bars)
            .map(|b| {
                groups
                    .iter()
                    .flat_map(|g| {
                        let s = 1.0 / libm::sqrt(g[b].len() as f64);
                        g[b].iter().map(move |v| v * s)
                    })
                    .collect()
            })
            .collect()
    }

    /// Each dimension minus its mean over the rows, over its standard deviation or `floor`,
    /// whichever is larger.
    pub fn z_scored(rows: &[Vec<f64>], floor: f64) -> Vec<Vec<f64>> {
        if rows.is_empty() {
            return rows.to_vec();
        }
        let dims = rows[0].len();
        let mut out = rows.to_vec();
        let n = rows.len() as f64;
        for d in 0..dims {
            let mean = rows.iter().map(|r| r[d]).sum::<f64>() / n;
            let sd = libm::sqrt(
                rows.iter()
                    .map(|r| (r[d] - mean) * (r[d] - mean))
                    .sum::<f64>()
                    / n,
            );
            let div = sd.max(floor);
            for r in 0..rows.len() {
                out[r][d] = if div > 1e-9 {
                    (rows[r][d] - mean) / div
                } else {
                    0.0
                };
            }
        }
        out
    }

    /// Cosine similarity of every pair of rows, each norm at least `NORM_FLOOR`.
    pub fn cosine_similarity(v: &[Vec<f64>]) -> Vec<Vec<f64>> {
        let norms: Vec<f64> = v
            .iter()
            .map(|r| libm::sqrt(r.iter().map(|x| x * x).sum::<f64>()).max(Self::NORM_FLOOR))
            .collect();
        let mut s = vec![vec![0.0f64; v.len()]; v.len()];
        for i in 0..v.len() {
            for j in i..v.len() {
                let d = norms[i] * norms[j];
                let mut dot = 0.0;
                for k in 0..v[i].len() {
                    dot += v[i][k] * v[j][k];
                }
                let c = dot / d;
                s[i][j] = c;
                s[j][i] = c;
            }
        }
        s
    }

    /// Foote novelty at each bar's start: a Gaussian-tapered checkerboard of `kernel` bars a
    /// side (σ = kernel / 2), cut at the ends and renormalised by the weight it keeps.
    pub fn novelty(s: &[Vec<f64>], k: usize) -> Vec<f64> {
        let n = s.len();
        let mut out = vec![0.0f64; n];
        let sigma = (k as f64 / 2.0).max(0.5);
        let side = Self::MIN_SIDE.min(k);
        if n < 2 * side {
            return out;
        }
        let ki = k as i64;
        for b in side..=(n - side) {
            if b >= n {
                break;
            }
            let (mut sum, mut weight) = (0.0, 0.0);
            for i in -ki..ki {
                let bi = b as i64 + i;
                if bi < 0 || bi >= n as i64 {
                    continue;
                }
                for j in -ki..ki {
                    let bj = b as i64 + j;
                    if bj < 0 || bj >= n as i64 {
                        continue;
                    }
                    let ti = i as f64 + 0.5;
                    let tj = j as f64 + 0.5;
                    let g = libm::exp(-(ti * ti + tj * tj) / (2.0 * sigma * sigma));
                    let sign = if (i < 0) == (j < 0) { 1.0 } else { -1.0 };
                    sum += sign * g * s[bi as usize][bj as usize];
                    weight += g;
                }
            }
            // novelty is mean within − mean across
            out[b] = if weight > 0.0 {
                2.0 * sum / weight
            } else {
                0.0
            };
        }
        out
    }

    /// Bars whose novelty is the largest within ±`radius`, at least `THRESHOLD` × the median
    /// of the bars where the kernel fits, and at least `MIN_NOVELTY`.
    pub fn peaks(nov: &[f64], radius: usize) -> Vec<usize> {
        let mut valid: Vec<f64> = nov.iter().copied().filter(|&v| v != 0.0).collect();
        valid.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        if valid.is_empty() {
            return Vec::new();
        }
        let mid = valid.len() / 2;
        let median = if valid.len() % 2 == 1 {
            valid[mid]
        } else {
            (valid[mid - 1] + valid[mid]) / 2.0
        };
        let floor = Self::MIN_NOVELTY.max(Self::THRESHOLD * median);
        let mut out = Vec::new();
        for b in 1..nov.len() {
            if nov[b] < floor {
                continue;
            }
            let lo = b.saturating_sub(radius);
            let hi = (nov.len() - 1).min(b + radius);
            // the largest in reach; on a tie, the first
            let is_peak =
                (lo..=hi).all(|j| j == b || nov[j] < nov[b] || (nov[j] == nov[b] && j > b));
            if is_peak {
                out.push(b);
            }
        }
        out
    }
}
