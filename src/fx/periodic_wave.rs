//! Chrome's OscillatorNode, for the pitch shifter's LFOs (`PeriodicWave.swift`;
//! third_party/blink/renderer/modules/webaudio/periodic_wave_handler.cc and
//! oscillator_handler.cc).
//!
//! A waveform is a set of band-limited wavetables, `ranges` of them, three per octave, each
//! keeping fewer partials, all scaled by the peak of the fullest one. A sample is read from the
//! two tables bracketing the fundamental, interpolated within each (linear, or 3- / 5-point
//! Lagrange at low rates) and then between them. The pitch shifter's sawtooths reset abruptly;
//! how Chrome band-limits that reset is part of how Tone's PitchShift sounds, so it is ported
//! rather than idealised.
//!
//! Swift builds the tables with `vDSP_DFT` (single precision, Accelerate). Here an inverse
//! radix-2 FFT in f64 does the same sum (rounded to f32 afterwards), with `libm` for sin/cos so
//! the tables are identical on every target, wasm included. Tables are built once per sample
//! rate by whoever makes the chain, off the audio thread, and shared through an `Arc`.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, log2f, pow, sin};
use once_cell::race::OnceBox;

/// The wavetables of one waveform at one sample rate (immutable once built).
pub struct PeriodicWave {
    /// `PeriodicWaveSize()`.
    pub size: usize,
    /// `NumberOfRanges()`.
    pub ranges: usize,
    /// `lowest_fundamental_frequency_` = nyquist / (size / 2).
    pub lowest_fundamental: f32,
    /// `RateScale()` = size / sample rate.
    pub rate_scale: f32,
    /// `ranges` tables of `size` samples, range 0 (all partials) first.
    tables: Vec<f32>,
}

/// `PeriodicWaveSize()` for a sample rate.
fn wave_size(sample_rate: f64) -> usize {
    if sample_rate <= 24000.0 {
        2048
    } else if sample_rate <= 88200.0 {
        4096
    } else {
        16384
    }
}

/// In-place radix-2 FFT with a positive exponent (an unnormalised inverse DFT).
fn inverse_fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    // bit reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * core::f64::consts::PI / len as f64;
        let half = len / 2;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (wr, wi) = (cos(ang * k as f64), sin(ang * k as f64));
                let (a, b) = (start + k, start + k + half);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

impl PeriodicWave {
    /// `real` / `imag` are the cosine / sine coefficients from index 0 (DC), as handed to
    /// `createPeriodicWave`; normalised as Chrome does (by the peak of range 0).
    pub fn new(sample_rate: f64, real: &[f32], imag: &[f32]) -> PeriodicWave {
        let sr = sample_rate as f32;
        let size = wave_size(sample_rate);
        let ranges = (0.5 + 3.0 * log2f(size as f32)) as usize;
        let half = size / 2;
        let lowest_fundamental = 0.5 * sr / half as f32;
        let rate_scale = size as f32 / sr;
        let components = real.len().min(half);
        let mut tables = vec![0.0f32; ranges * size];
        let mut in_re = vec![0.0f64; size];
        let mut in_im = vec![0.0f64; size];
        let mut scale = 1.0f32;
        for r in 0..ranges {
            // NumberOfPartialsForRange
            let cents_to_cull = r as f32 * (1200.0 / 3.0);
            let culling = pow(2.0, (-cents_to_cull / 1200.0) as f64) as f32;
            let partials = (culling * half as f32) as usize;
            in_re.iter_mut().for_each(|x| *x = 0.0);
            in_im.iter_mut().for_each(|x| *x = 0.0);
            // bins 1 ..< min(components, partials + 1); DC and Nyquist cleared
            for n in 1..components.min(partials + 1) {
                // X[n] = (a - i b) / 2, X[N - n] = conj(X[n])
                in_re[n] = (real[n] / 2.0) as f64;
                in_im[n] = (-imag[n] / 2.0) as f64;
                in_re[size - n] = (real[n] / 2.0) as f64;
                in_im[size - n] = (imag[n] / 2.0) as f64;
            }
            inverse_fft(&mut in_re, &mut in_im);
            let t = &mut tables[r * size..(r + 1) * size];
            for k in 0..size {
                t[k] = in_re[k] as f32;
            }
            if r == 0 {
                let mut peak = 0.0f32;
                for &x in t.iter() {
                    peak = peak.max(x.abs());
                }
                if peak > 0.0 {
                    scale = 1.0 / peak;
                }
            }
            for x in t.iter_mut() {
                *x *= scale;
            }
        }
        PeriodicWave {
            size,
            ranges,
            lowest_fundamental,
            rate_scale,
            tables,
        }
    }

    #[inline]
    fn table(&self, r: usize) -> &[f32] {
        &self.tables[r * self.size..(r + 1) * self.size]
    }

    /// `WaveDataForFundamentalFrequency`: the two tables (fewer partials, more partials) and
    /// the factor between them, as `(lower, higher, factor)`.
    pub fn tables_for(&self, fundamental: f32) -> (usize, usize, f32) {
        let ff = fundamental.abs();
        let ratio: f32 = if ff > 0.0 {
            ff / self.lowest_fundamental
        } else {
            0.5
        };
        let cents = log2f(ratio) * 1200.0;
        let mut pitch_range = 1.0 + cents / (1200.0 / 3.0);
        pitch_range = pitch_range.min((self.ranges - 1) as f32).max(0.0);
        let r1 = pitch_range as usize;
        let r2 = if r1 < self.ranges - 1 { r1 + 1 } else { r1 };
        (r2, r1, pitch_range - r1 as f32)
    }

    // MARK: the three waveforms of Tone's PitchShift, per sample rate

    /// Chrome's built-in "sawtooth" (`GenerateBasicWaveform`, coefficients in float).
    pub fn sawtooth(sample_rate: f64) -> Arc<PeriodicWave> {
        Self::shared(sample_rate, 0)
    }
    /// Tone's sawtooth at phase 180 (Oscillator.js `_getRealImaginary`, a custom PeriodicWave).
    pub fn sawtooth180(sample_rate: f64) -> Arc<PeriodicWave> {
        Self::shared(sample_rate, 1)
    }
    /// Tone's triangle at phase 90.
    pub fn triangle90(sample_rate: f64) -> Arc<PeriodicWave> {
        Self::shared(sample_rate, 2)
    }

    /// The waveform `which` at `sr`, built once per process for the first sample rate asked
    /// for (every chain an engine makes runs at its one rate) and shared from then on:
    /// building one is tens of ms (the FFTs of every range), and an engine has four chains
    /// (each deck's, the loop row's, the sample bus's), so building them per chain made booting
    /// an engine cost a quarter of a second. Another rate is built fresh each time. The tables
    /// are a pure function of (rate, waveform), so a shared one is the one that would be built.
    fn shared(sr: f64, which: u8) -> Arc<PeriodicWave> {
        static CACHE: [OnceBox<(u64, Arc<PeriodicWave>)>; 3] =
            [OnceBox::new(), OnceBox::new(), OnceBox::new()];
        let cell = &CACHE[which as usize];
        let (bits, wave) = cell.get_or_init(|| Box::new((sr.to_bits(), Arc::new(Self::make(sr, which)))));
        if *bits == sr.to_bits() {
            wave.clone()
        } else {
            Arc::new(Self::make(sr, which))
        }
    }

    fn make(sr: f64, which: u8) -> PeriodicWave {
        if which == 0 {
            // GenerateBasicWaveform(SAWTOOTH): half-size coefficients, b = pi_factor * ±1 (float)
            let half = wave_size(sr) / 2;
            let mut imag = vec![0.0f32; half];
            for n in 1..half {
                let pi_factor = 2.0 / (n as f32 * core::f32::consts::PI);
                imag[n] = pi_factor * if n & 1 == 1 { 1.0 } else { -1.0 };
            }
            return PeriodicWave::new(sr, &vec![0.0f32; half], &imag);
        }
        // Tone: fftSize 4096 -> 2048 coefficients; real[n] = -b sin(n phi), imag[n] = b cos(n phi)
        let count = 2048;
        let phase = if which == 1 {
            core::f64::consts::PI
        } else {
            core::f64::consts::PI / 2.0
        };
        let mut real = vec![0.0f32; count];
        let mut imag = vec![0.0f32; count];
        for n in 1..count {
            let pi_factor = 2.0 / (n as f64 * core::f64::consts::PI);
            let b: f64 = if which == 1 {
                pi_factor * if n & 1 == 1 { 1.0 } else { -1.0 }
            } else if n & 1 == 1 {
                2.0 * (pi_factor * pi_factor) * if ((n - 1) >> 1) & 1 == 1 { -1.0 } else { 1.0 }
            } else {
                0.0
            };
            if b != 0.0 {
                real[n] = (-b * sin(phase * n as f64)) as f32;
                imag[n] = (b * cos(phase * n as f64)) as f32;
            }
        }
        PeriodicWave::new(sr, &real, &imag)
    }
}

/// One Chrome oscillator reading a [`PeriodicWave`] at a rate that changes only now and then.
#[derive(Clone, Debug, Default)]
pub struct WaveOscillator {
    /// `virtual_read_index` (0 ..< size).
    index: f64,
    incr: f32,
    lower: usize,
    higher: usize,
    factor: f32,
}

impl WaveOscillator {
    pub fn new() -> WaveOscillator {
        WaveOscillator::default()
    }

    pub fn index(&self) -> f64 {
        self.index
    }

    /// The fundamental changed (`frequency * rate_scale`, the tables bracketing it).
    pub fn set_frequency(&mut self, f: f64, wave: &PeriodicWave) {
        let ff = f as f32;
        self.incr = ff * wave.rate_scale;
        let (lower, higher, factor) = wave.tables_for(ff);
        self.lower = lower;
        self.higher = higher;
        self.factor = factor;
    }

    /// This sample's value, then advance (`DoInterpolation` and the read-index update).
    #[inline]
    pub fn next(&mut self, wave: &PeriodicWave) -> f64 {
        let size = wave.size;
        let mask = size - 1;
        let lo = wave.table(self.lower);
        let hi = wave.table(self.higher);
        let i0 = self.index as i64 as usize;
        let s_lo;
        let s_hi;
        let step = self.incr.abs();
        if step >= 0.3 {
            let i1 = (i0 + 1) & mask;
            let j0 = i0 & mask;
            let f = ((self.index as f32) - (i0 as f32)) as f64;
            s_hi = (1.0 - f) * hi[j0] as f64 + f * hi[i1] as f64;
            s_lo = (1.0 - f) * lo[j0] as f64 + f * lo[i1] as f64;
        } else if step >= 0.16 {
            let t = self.index - i0 as f64;
            let a0 = 0.5 * t * (t - 1.0);
            let a1 = 1.0 - t * t;
            let a2 = 0.5 * t * (t + 1.0);
            let k0 = i0.wrapping_sub(1) & mask;
            let k1 = i0 & mask;
            let k2 = (i0 + 1) & mask;
            s_lo = a0 * lo[k0] as f64 + a1 * lo[k1] as f64 + a2 * lo[k2] as f64;
            s_hi = a0 * hi[k0] as f64 + a1 * hi[k1] as f64 + a2 * hi[k2] as f64;
        } else {
            let t = self.index - i0 as f64;
            let t2 = t * t;
            let a0 = t * (t2 - 1.0) * (t - 2.0) / 24.0;
            let a1 = -t * (t - 1.0) * (t2 - 4.0) / 6.0;
            let a2 = (t2 - 1.0) * (t2 - 4.0) / 4.0;
            let a3 = -t * (t + 1.0) * (t2 - 4.0) / 6.0;
            let a4 = t * (t2 - 1.0) * (t + 2.0) / 24.0;
            let k0 = i0.wrapping_sub(2) & mask;
            let k1 = i0.wrapping_sub(1) & mask;
            let k2 = i0 & mask;
            let k3 = (i0 + 1) & mask;
            let k4 = (i0 + 2) & mask;
            s_lo = a0 * lo[k0] as f64
                + a1 * lo[k1] as f64
                + a2 * lo[k2] as f64
                + a3 * lo[k3] as f64
                + a4 * lo[k4] as f64;
            s_hi = a0 * hi[k0] as f64
                + a1 * hi[k1] as f64
                + a2 * hi[k2] as f64
                + a3 * hi[k3] as f64
                + a4 * hi[k4] as f64;
        }
        let sample = ((1.0 - self.factor as f64) * s_hi + self.factor as f64 * s_lo) as f32;
        self.index += self.incr as f64;
        self.index -= libm::floor(self.index * (1.0 / size as f64)) * size as f64;
        sample as f64
    }
}
