//! A small forward FFT for the offline measurements (`Spectrum`, `BeatTracker`,
//! `SectionSegmenter`), standing in for vDSP's `vDSP_fft_zrip` on Apple platforms. Plain
//! iterative radix-2 on f64 with `libm` twiddles, so it is `no_std` and the same on every
//! target. Results are within float precision of vDSP's, not bit-identical.

use alloc::vec;
use alloc::vec::Vec;

/// A planned power-of-two real FFT of `n` points.
pub struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<usize>,
    re: Vec<f64>,
    im: Vec<f64>,
}

/// What `vDSP_fft_zrip` returns for `n` real points: `n / 2` packed bins at **twice** the DFT
/// (`re[k] = 2·Re X[k]`, `im[k] = 2·Im X[k]`), the Nyquist bin's real part (also doubled) in
/// `nyquist` where vDSP packs it into `im[0]`; `im[0]` here is 0.
pub struct Packed {
    pub re: Vec<f64>,
    pub im: Vec<f64>,
    pub nyquist: f64,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 2);
        let mut cos = Vec::with_capacity(n / 2);
        let mut sin = Vec::with_capacity(n / 2);
        for k in 0..n / 2 {
            let a = -2.0 * core::f64::consts::PI * k as f64 / n as f64;
            cos.push(libm::cos(a));
            sin.push(libm::sin(a));
        }
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| (i.reverse_bits() >> (usize::BITS - bits)) as usize)
            .collect();
        Fft {
            n,
            cos,
            sin,
            rev,
            re: vec![0.0; n],
            im: vec![0.0; n],
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    /// The forward DFT of the real `x` (`n` points) in `packed` form (see [`Packed`]).
    pub fn zrip(&mut self, x: &[f64]) -> Packed {
        let n = self.n;
        for i in 0..n {
            self.re[self.rev[i]] = x[i];
            self.im[self.rev[i]] = 0.0;
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut i = 0;
            while i < n {
                for j in 0..half {
                    let (wr, wi) = (self.cos[j * step], self.sin[j * step]);
                    let (a, b) = (i + j, i + j + half);
                    let tr = self.re[b] * wr - self.im[b] * wi;
                    let ti = self.re[b] * wi + self.im[b] * wr;
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
                i += len;
            }
            len <<= 1;
        }
        let half = n / 2;
        let mut re = Vec::with_capacity(half);
        let mut im = Vec::with_capacity(half);
        for k in 0..half {
            re.push(2.0 * self.re[k]);
            im.push(if k == 0 { 0.0 } else { 2.0 * self.im[k] });
        }
        Packed {
            re,
            im,
            nyquist: 2.0 * self.re[half],
        }
    }
}

/// `vDSP_hann_window` with `vDSP_HANN_DENORM`: `0.5 (1 − cos(2πn/N))`.
pub fn hann_denorm(size: usize) -> Vec<f64> {
    (0..size)
        .map(|n| 0.5 * (1.0 - libm::cos(2.0 * core::f64::consts::PI * n as f64 / size as f64)))
        .collect()
}

/// `vDSP_hann_window` with `vDSP_HANN_NORM`: the Hann scaled to unit mean square
/// (`sqrt(8/3)`).
pub fn hann_norm(size: usize) -> Vec<f64> {
    let s = libm::sqrt(8.0 / 3.0);
    hann_denorm(size).into_iter().map(|w| w * s).collect()
}
