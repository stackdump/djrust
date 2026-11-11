//! Port of `Musicality/Spectrum.swift`: the long-term average spectrum of a stereo render in
//! third-octave bands from 25 Hz to 16 kHz, and the mastering and translation checks built
//! on it (slope, no 16 kHz bump, fold to mono). One Welch pass (Hann, `FFT_SIZE` points, 50 %
//! overlap) over both channels; the mono spectrum is (X_L + X_R) / 2 from the same FFTs.
//! Computed in f64 (Swift's vDSP is f32): equal to within float precision.

use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::fft::{hann_denorm, Fft};

pub const FFT_SIZE: usize = 8192;
pub const MAX_SEGMENTS: usize = 256;
/// Nominal third-octave centres (IEC 61260), 25 Hz … 16 kHz.
pub const BANDS_HZ: [f64; 29] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10000.0, 12500.0, 16000.0,
];
/// The slope fit's range (exit M7): 63 Hz … 8 kHz.
pub const SLOPE_RANGE: (f64, f64) = (63.0, 8000.0);
/// The bands the 16 kHz bump is read in.
pub const BUMP_BANDS: [f64; 2] = [12500.0, 16000.0];

#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    /// Mono power spectral density per band, dB re full scale² per Hz. −200 for a silent
    /// band or one past Nyquist.
    pub ltas_db: Vec<f64>,
    /// Least-squares slope of `ltas_db` against octaves over `SLOPE_RANGE`, dB per octave.
    pub slope_db_per_oct: f64,
    /// The fit's level at 1 kHz, dB.
    pub fit_at_1k_db: f64,
    /// How far the 12.5 and 16 kHz bands sit over the fit extrapolated to them, dB (mean).
    pub hf_bump_db: f64,
    /// Per band: energy of (L + R) / 2 against the mean of L² and R², dB.
    pub mono_fold_loss_db: Vec<f64>,
    /// The same over every band together.
    pub mono_fold_db: f64,
}

impl Spectrum {
    pub fn new(left: &[f32], right: &[f32], sr: f64) -> Spectrum {
        let bands = BANDS_HZ.len();
        let mut s = Spectrum {
            ltas_db: vec![-200.0; bands],
            slope_db_per_oct: 0.0,
            fit_at_1k_db: -200.0,
            hf_bump_db: 0.0,
            mono_fold_loss_db: vec![0.0; bands],
            mono_fold_db: 0.0,
        };
        let n = left.len().min(right.len());
        let size = FFT_SIZE;
        let half = size / 2;
        if n < size || sr <= 0.0 {
            return s;
        }
        // segment starts: every half window, or `MAX_SEGMENTS` spread evenly over the render
        let hop = size / 2;
        let available = (n - size) / hop + 1;
        let count = available.min(MAX_SEGMENTS);
        let starts: Vec<usize> = if count == available {
            (0..count).map(|i| i * hop).collect()
        } else {
            (0..count)
                .map(|i| (n - size) * i / (count - 1).max(1))
                .collect()
        };
        let window = hann_denorm(size);
        let w2: f64 = window.iter().map(|w| w * w).sum();
        let mut fft = Fft::new(size);
        let mut psd_l = vec![0.0f64; half + 1];
        let mut psd_r = vec![0.0f64; half + 1];
        let mut psd_m = vec![0.0f64; half + 1];
        let mut buf = vec![0.0f64; size];
        for &st in &starts {
            for i in 0..size {
                buf[i] = left[st + i] as f64 * window[i];
            }
            let l = fft.zrip(&buf);
            for i in 0..size {
                buf[i] = right[st + i] as f64 * window[i];
            }
            let r = fft.zrip(&buf);
            // packed: the Nyquist bin's real part
            psd_l[half] += l.nyquist * l.nyquist;
            psd_r[half] += r.nyquist * r.nyquist;
            let nm = 0.5 * (l.nyquist + r.nyquist);
            psd_m[half] += nm * nm;
            for k in 0..half {
                psd_l[k] += l.re[k] * l.re[k] + l.im[k] * l.im[k];
                psd_r[k] += r.re[k] * r.re[k] + r.im[k] * r.im[k];
                let (mr, mi) = (0.5 * (l.re[k] + r.re[k]), 0.5 * (l.im[k] + r.im[k]));
                psd_m[k] += mr * mr + mi * mi;
            }
        }
        // zrip returns 2 × the DFT; one-sided PSD = 2 |X|² / (fs Σw²), averaged over segments
        let scale = 2.0 / (4.0 * sr * w2 * starts.len() as f64);
        let bin_hz = sr / size as f64;

        let mut total_mono = 0.0;
        let mut total_mean = 0.0;
        for b in 0..bands {
            let fc = 1000.0 * libm::pow(2.0, (b as f64 - 16.0) / 3.0);
            let lo = fc / libm::pow(2.0, 1.0 / 6.0);
            let hi = fc * libm::pow(2.0, 1.0 / 6.0);
            if hi >= sr / 2.0 {
                continue;
            }
            let mut first = libm::ceil(lo / bin_hz) as i64;
            let mut last = libm::ceil(hi / bin_hz) as i64 - 1;
            if last < first {
                // a band narrower than a bin: its nearest one
                first = libm::round(fc / bin_hz) as i64;
                last = first;
            }
            let (mut m, mut mean) = (0.0, 0.0);
            let top = last.min(half as i64);
            for k in first..=top {
                let k = k as usize;
                m += psd_m[k];
                mean += (psd_l[k] + psd_r[k]) / 2.0;
            }
            total_mono += m;
            total_mean += mean;
            let nbins = (top - first + 1) as f64;
            if m > 0.0 {
                s.ltas_db[b] = 10.0 * libm::log10(m * scale / nbins);
            }
            if mean > 0.0 {
                s.mono_fold_loss_db[b] = (10.0 * libm::log10(m.max(1e-30) / mean)).max(-100.0);
            }
        }
        if total_mean > 0.0 {
            s.mono_fold_db = (10.0 * libm::log10(total_mono.max(1e-30) / total_mean)).max(-100.0);
        }

        // the slope: least squares of level against octaves from 1 kHz
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for (b, f) in BANDS_HZ.iter().enumerate() {
            if *f >= SLOPE_RANGE.0 && *f <= SLOPE_RANGE.1 && s.ltas_db[b] > -190.0 {
                xs.push((b as f64 - 16.0) / 3.0);
                ys.push(s.ltas_db[b]);
            }
        }
        if xs.len() < 2 {
            return s;
        }
        let mx = xs.iter().sum::<f64>() / xs.len() as f64;
        let my = ys.iter().sum::<f64>() / ys.len() as f64;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for i in 0..xs.len() {
            sxy += (xs[i] - mx) * (ys[i] - my);
            sxx += (xs[i] - mx) * (xs[i] - mx);
        }
        s.slope_db_per_oct = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        s.fit_at_1k_db = my - s.slope_db_per_oct * mx;
        let mut bump: Vec<f64> = Vec::new();
        for (b, f) in BANDS_HZ.iter().enumerate() {
            if BUMP_BANDS.contains(f) && s.ltas_db[b] > -190.0 {
                bump.push(
                    s.ltas_db[b] - (s.fit_at_1k_db + s.slope_db_per_oct * (b as f64 - 16.0) / 3.0),
                );
            }
        }
        if !bump.is_empty() {
            s.hf_bump_db = bump.iter().sum::<f64>() / bump.len() as f64;
        }
        s
    }
}
