//! The streaming stages of a post chain. Every one is real-time safe: buffers are allocated in
//! `new`, `process` allocates, locks and frees nothing. The offline path runs the same code
//! over a whole file (with its latency taken off), so live and offline sound the same.

use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, exp, log10, pow, sin, sqrt};

use crate::fx::limiter::PeakLimiter;
use crate::musicality::loudness::KWeighting;

use super::chain::Stage;

const TAU: f64 = 2.0 * core::f64::consts::PI;

fn db_to_lin(db: f64) -> f64 {
    pow(10.0, db / 20.0)
}

fn pow_db(p: f64) -> f64 {
    10.0 * log10(p.max(1e-30))
}

// ---- FFT --------------------------------------------------------------------------------------

/// A planned in-place complex radix-2 FFT of `n` points (f32 data, f64-built twiddles).
pub struct Fft {
    n: usize,
    rev: Vec<u32>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 4, "fft size");
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let half = n / 2;
        Fft {
            n,
            rev,
            cos: (0..half).map(|k| cos(TAU * k as f64 / n as f64) as f32).collect(),
            sin: (0..half).map(|k| -sin(TAU * k as f64 / n as f64) as f32).collect(),
        }
    }

    /// Forward (`e^{-i…}`) or inverse (`e^{+i…}`, unscaled) transform of `re + i·im`.
    pub fn process(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { -1.0f32 } else { 1.0 };
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let step = n / size;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.cos[k * step];
                    let wi = sign * self.sin[k * step];
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += size;
            }
            size *= 2;
        }
    }
}

// ---- Biquad -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    Highpass,
    Lowpass,
    HighShelf,
    LowShelf,
    Peak,
}

/// An RBJ-cookbook biquad on both channels (transposed direct form II, f64 state).
#[derive(Clone, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z: [[f64; 2]; 2],
}

impl Biquad {
    pub fn new(kind: BiquadKind, freq: f64, gain_db: f64, q: f64, sr: f64) -> Biquad {
        let f = freq.clamp(1.0, 0.49 * sr);
        let w = TAU * f / sr;
        let (cs, sn) = (cos(w), sin(w));
        let alpha = sn / (2.0 * q.max(1e-3));
        let a = pow(10.0, gain_db / 40.0);
        let sa = 2.0 * sqrt(a) * alpha;
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Highpass => (
                (1.0 + cs) / 2.0,
                -(1.0 + cs),
                (1.0 + cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            BiquadKind::Lowpass => (
                (1.0 - cs) / 2.0,
                1.0 - cs,
                (1.0 - cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            BiquadKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cs,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cs,
                1.0 - alpha / a,
            ),
            BiquadKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cs + sa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                a * ((a + 1.0) - (a - 1.0) * cs - sa),
                (a + 1.0) + (a - 1.0) * cs + sa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                (a + 1.0) + (a - 1.0) * cs - sa,
            ),
            BiquadKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cs + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                a * ((a + 1.0) + (a - 1.0) * cs - sa),
                (a + 1.0) - (a - 1.0) * cs + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cs),
                (a + 1.0) - (a - 1.0) * cs - sa,
            ),
        };
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z: [[0.0; 2]; 2],
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (c, ch) in [l, r].into_iter().enumerate() {
            let [mut z1, mut z2] = self.z[c];
            for x in ch.iter_mut() {
                let xi = *x as f64;
                let y = self.b0 * xi + z1;
                z1 = self.b1 * xi - self.a1 * y + z2;
                z2 = self.b2 * xi - self.a2 * y;
                *x = y as f32;
            }
            // flush denormals in long silences
            if z1.abs() < 1e-25 {
                z1 = 0.0;
            }
            if z2.abs() < 1e-25 {
                z2 = 0.0;
            }
            self.z[c] = [z1, z2];
        }
    }
}

// ---- Expander ---------------------------------------------------------------------------------

/// Downward expander, stereo-linked, RMS detection (10 ms), soft knee.
pub struct Expander {
    threshold_db: Option<f64>,
    below_program_db: f64,
    ratio: f64,
    range_db: f64,
    knee_db: f64,
    attack: f64,
    release: f64,
    det: f64,
    prog_c: f64,
    env: f64,
    prog: f64,
    gain_db: f64,
}

impl Expander {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        threshold_db: Option<f64>,
        below_program_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        range_db: f64,
        knee_db: f64,
        sr: f64,
        program_db: Option<f64>,
    ) -> Expander {
        let c = |s: f64| 1.0 - exp(-1.0 / (s * sr).max(1.0));
        Expander {
            threshold_db,
            below_program_db,
            ratio,
            range_db,
            knee_db,
            attack: c(attack_ms / 1000.0),
            release: c(release_ms / 1000.0),
            det: c(0.010),
            prog_c: c(3.0),
            env: 0.0,
            prog: pow(10.0, program_db.unwrap_or(-20.0) / 10.0),
            gain_db: 0.0,
        }
    }

    /// The gain (dB, ≤ 0) for a detector level `x` against threshold `t`.
    fn curve(&self, x: f64, t: f64) -> f64 {
        let w = self.knee_db;
        let r1 = self.ratio - 1.0;
        let g = if x >= t + w / 2.0 {
            0.0
        } else if w > 0.0 && x > t - w / 2.0 {
            let d = t + w / 2.0 - x;
            -r1 * d * d / (2.0 * w)
        } else {
            -r1 * (t - x)
        };
        g.max(-self.range_db)
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len().min(r.len()) {
            let (a, b) = (l[i] as f64, r[i] as f64);
            let p = 0.5 * (a * a + b * b);
            self.env += self.det * (p - self.env);
            let x = pow_db(self.env);
            // the program level: a slow average of what is not near silence
            let prog_db = pow_db(self.prog);
            if x > prog_db - 15.0 {
                self.prog += self.prog_c * (self.env - self.prog);
            }
            let t = self
                .threshold_db
                .unwrap_or(prog_db - self.below_program_db);
            let target = self.curve(x, t);
            let k = if target > self.gain_db {
                self.attack
            } else {
                self.release
            };
            self.gain_db += k * (target - self.gain_db);
            let g = db_to_lin(self.gain_db) as f32;
            l[i] *= g;
            r[i] *= g;
        }
    }
}

// ---- Minimum statistics ---------------------------------------------------------------------

/// Per-bin noise power by minimum statistics (Martin 2001, simplified): the minimum of a
/// smoothed periodogram over about 2 s, times a bias. What the denoise tracks and what the
/// measurement's noise-floor estimate reads.
pub struct MinStats {
    bins: usize,
    alpha: f32,
    bias: f32,
    ps: Vec<f32>,
    sub_min: Vec<f32>,
    slots: Vec<f32>,
    win_min: Vec<f32>,
    u: usize,
    u_count: usize,
    v: usize,
    v_count: usize,
    pub frames: usize,
}

impl MinStats {
    pub const SUBWINDOWS: usize = 8;

    /// `hop_seconds`: the time between frames.
    pub fn new(bins: usize, hop_seconds: f64) -> MinStats {
        let v = libm::round(0.25 / hop_seconds).max(2.0) as usize;
        MinStats {
            bins,
            alpha: 0.85,
            bias: 1.5,
            ps: vec![0.0; bins],
            sub_min: vec![f32::INFINITY; bins],
            slots: vec![f32::INFINITY; bins * Self::SUBWINDOWS],
            win_min: vec![f32::INFINITY; bins],
            u: 0,
            u_count: 0,
            v,
            v_count: 0,
            frames: 0,
        }
    }

    /// Frames until the estimate covers a whole subwindow.
    pub fn warm(&self) -> bool {
        self.u_count > 0
    }

    /// Take one frame's power per bin; `out` gets the noise estimate.
    #[allow(clippy::needless_range_loop)] // four arrays walked bin by bin
    pub fn update(&mut self, p: &[f32], out: &mut [f32]) {
        let a = self.alpha;
        for k in 0..self.bins {
            let s = if self.frames == 0 {
                p[k]
            } else {
                a * self.ps[k] + (1.0 - a) * p[k]
            };
            self.ps[k] = s;
            if s < self.sub_min[k] {
                self.sub_min[k] = s;
            }
        }
        self.frames += 1;
        self.v_count += 1;
        if self.v_count >= self.v {
            self.v_count = 0;
            let base = self.u * self.bins;
            self.slots[base..base + self.bins].copy_from_slice(&self.sub_min);
            self.u = (self.u + 1) % Self::SUBWINDOWS;
            self.u_count = (self.u_count + 1).min(Self::SUBWINDOWS);
            for k in 0..self.bins {
                let mut m = f32::INFINITY;
                for s in 0..Self::SUBWINDOWS {
                    m = m.min(self.slots[s * self.bins + k]);
                }
                self.win_min[k] = m;
                self.sub_min[k] = self.ps[k];
            }
        }
        for k in 0..self.bins {
            out[k] = self.bias * self.win_min[k].min(self.sub_min[k]);
        }
    }
}

// ---- Spectral denoise -------------------------------------------------------------------------

/// The FFT size a denoise stage runs at: as given, else 2048 offline and 1024 live (doubled
/// above 50 kHz).
pub fn denoise_fft_size(given: Option<usize>, sr: f64, live: bool) -> usize {
    given.unwrap_or(if live { 1024 } else { 2048 } * if sr > 50_000.0 { 2 } else { 1 })
}

/// STFT denoise on both channels with one gain per bin (so the stereo image stays): Hann
/// windows at 75 % overlap, the two channels packed into one complex FFT. Latency: the FFT
/// size.
pub struct Denoise {
    n: usize,
    hop: usize,
    fft: Fft,
    win: Vec<f32>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    fill: usize,
    power: Vec<f32>,
    noise: Vec<f32>,
    tracked: Vec<f32>,
    gain: Vec<f32>,
    prev_gain: Vec<f32>,
    prev_gamma: Vec<f32>,
    smooth: Vec<f32>,
    floor: Vec<f32>,
    lo: f32,
    hi: f32,
    gmin: f32,
    offset: f32,
    track: bool,
    stats: MinStats,
    norm: f32,
    first: bool,
}

impl Denoise {
    pub fn new(
        noise_floor_db: f64,
        reduction_db: f64,
        track: bool,
        track_range_db: f64,
        noise_offset_db: f64,
        fft_size: usize,
        sr: f64,
    ) -> Denoise {
        let n = fft_size;
        let hop = n / 4;
        let win: Vec<f32> = (0..n)
            .map(|i| (0.5 * (1.0 - cos(TAU * i as f64 / n as f64))) as f32)
            .collect();
        let sum_w2: f64 = win.iter().map(|w| (*w as f64) * (*w as f64)).sum();
        let bins = n / 2 + 1;
        // white noise of RMS σ: E|X_k|² = σ² Σw²
        let n0 = (pow(10.0, noise_floor_db / 10.0) * sum_w2) as f32;
        let range = pow(10.0, track_range_db / 10.0) as f32;
        // analysis × synthesis Hann at hop n/4 sums to 1.5
        let norm = (1.0 / 1.5 / n as f64) as f32;
        Denoise {
            n,
            hop,
            fft: Fft::new(n),
            win,
            in_l: vec![0.0; n],
            in_r: vec![0.0; n],
            out_l: vec![0.0; n],
            out_r: vec![0.0; n],
            re: vec![0.0; n],
            im: vec![0.0; n],
            fill: 0,
            power: vec![0.0; bins],
            noise: vec![n0; bins],
            tracked: vec![n0; bins],
            gain: vec![1.0; bins],
            prev_gain: vec![1.0; bins],
            prev_gamma: vec![1.0; bins],
            smooth: vec![1.0; bins],
            floor: vec![n0; bins],
            lo: 1.0 / range,
            hi: range,
            gmin: db_to_lin(-reduction_db) as f32,
            offset: pow(10.0, noise_offset_db / 10.0) as f32,
            track,
            stats: MinStats::new(bins, hop as f64 / sr),
            norm,
            first: true,
        }
    }

    pub fn latency(&self) -> usize {
        self.n
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let (n, h) = (self.n, self.hop);
        for i in 0..l.len().min(r.len()) {
            self.in_l[n - h + self.fill] = l[i];
            self.in_r[n - h + self.fill] = r[i];
            l[i] = self.out_l[self.fill];
            r[i] = self.out_r[self.fill];
            self.fill += 1;
            if self.fill == h {
                self.frame();
                self.fill = 0;
            }
        }
    }

    fn frame(&mut self) {
        let (n, h) = (self.n, self.hop);
        for i in 0..n {
            self.re[i] = self.in_l[i] * self.win[i];
            self.im[i] = self.in_r[i] * self.win[i];
        }
        self.fft.process(&mut self.re, &mut self.im, false);
        let bins = n / 2 + 1;
        // per-channel mean power of bin k: (|Z_k|² + |Z_{N-k}|²) / 4
        for k in 0..bins {
            let j = (n - k) % n;
            let p = self.re[k] * self.re[k]
                + self.im[k] * self.im[k]
                + self.re[j] * self.re[j]
                + self.im[j] * self.im[j];
            self.power[k] = 0.25 * p;
        }
        if self.track {
            self.stats.update(&self.power, &mut self.tracked);
            if self.stats.warm() {
                for k in 0..bins {
                    let f = self.floor[k];
                    self.noise[k] = self.tracked[k].clamp(f * self.lo, f * self.hi);
                }
            }
        }
        // decision-directed a priori SNR, Wiener gain, floored
        const A: f32 = 0.98;
        const XI_MIN: f32 = 0.003;
        for k in 0..bins {
            let nk = (self.noise[k] * self.offset).max(1e-30);
            let gamma = self.power[k] / nk;
            let xi = if self.first {
                (gamma - 1.0).max(XI_MIN)
            } else {
                (A * self.prev_gain[k] * self.prev_gain[k] * self.prev_gamma[k]
                    + (1.0 - A) * (gamma - 1.0).max(0.0))
                .max(XI_MIN)
            };
            let g = (xi / (1.0 + xi)).max(self.gmin);
            self.gain[k] = g;
            self.prev_gamma[k] = gamma.min(1e6);
        }
        self.first = false;
        // a little smoothing across frequency against musical noise
        for k in 0..bins {
            let a = self.gain[k.saturating_sub(1)];
            let b = self.gain[k];
            let c = self.gain[(k + 1).min(bins - 1)];
            self.smooth[k] = (0.25 * a + 0.5 * b + 0.25 * c).max(self.gmin);
        }
        self.prev_gain.copy_from_slice(&self.gain);
        for k in 0..n {
            let g = self.smooth[if k < bins { k } else { n - k }];
            self.re[k] *= g;
            self.im[k] *= g;
        }
        self.fft.process(&mut self.re, &mut self.im, true);
        // overlap-add: drop the hop already played, add the new frame
        self.out_l.copy_within(h.., 0);
        self.out_r.copy_within(h.., 0);
        for i in n - h..n {
            self.out_l[i] = 0.0;
            self.out_r[i] = 0.0;
        }
        let s = self.norm;
        for i in 0..n {
            let w = self.win[i] * s;
            self.out_l[i] += self.re[i] * w;
            self.out_r[i] += self.im[i] * w;
        }
        self.in_l.copy_within(h.., 0);
        self.in_r.copy_within(h.., 0);
    }

    /// The noise level the gain works against now, dBFS RMS (white-equivalent).
    pub fn noise_db(&self) -> f64 {
        noise_rms_db(&self.noise, self.n, &self.win, 0)
    }
}

/// The RMS (dBFS) of the noise a per-bin power profile describes, over bins from `from_bin`.
pub fn noise_rms_db(noise: &[f32], n: usize, win: &[f32], from_bin: usize) -> f64 {
    let sum_w2: f64 = win.iter().map(|w| (*w as f64) * (*w as f64)).sum();
    let bins = n / 2 + 1;
    let mut tot = 0.0f64;
    for (k, x) in noise.iter().enumerate().take(bins).skip(from_bin) {
        let mult = if k == 0 || k == bins - 1 { 1.0 } else { 2.0 };
        tot += mult * *x as f64;
    }
    pow_db(tot / (n as f64 * sum_w2))
}

// ---- True-peak limiter ----------------------------------------------------------------------

/// The deck's lookahead true-peak limiter ([`PeakLimiter`], −1 dBTP) moved to any ceiling: the
/// input is scaled so the ceiling lands on the limiter's, and back.
pub struct TpLimiter {
    inner: PeakLimiter,
    pre: f32,
    post: f32,
}

impl TpLimiter {
    pub fn new(sr: f64, ceiling_db: f64) -> TpLimiter {
        let pre = db_to_lin(PeakLimiter::CEILING_DB - ceiling_db) as f32;
        TpLimiter {
            inner: PeakLimiter::new(sr),
            pre,
            post: 1.0 / pre,
        }
    }

    pub fn latency(&self) -> usize {
        self.inner.lookahead
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        if self.pre != 1.0 {
            for i in 0..n {
                l[i] *= self.pre;
                r[i] *= self.pre;
            }
        }
        self.inner.process(&mut l[..n], Some(&mut r[..n]), n);
        if self.post != 1.0 {
            for i in 0..n {
                l[i] *= self.post;
                r[i] *= self.post;
            }
        }
    }

    pub fn gain_reduction_db(&self) -> f64 {
        self.inner.gain_reduction_db()
    }
}

// ---- Live leveler ---------------------------------------------------------------------------

/// The live stand-in for two-pass loudness: BS.1770 short-term loudness (K-weighted, 3 s of
/// 100 ms blocks) drives a slow gain toward the target (rising over `UP_SECONDS`, falling over
/// `DOWN_SECONDS`, within ±`MAX_DB`; held through near-silence and breakdowns more than
/// `GATE_LU` under the target), then the true-peak limiter at the ceiling.
pub struct Leveler {
    target: f64,
    kw: [KWeighting; 2],
    block: usize,
    acc: f64,
    count: usize,
    ring: [f64; 30],
    ring_at: usize,
    filled: usize,
    pub desired_db: f64,
    pub gain_db: f64,
    pub short_term_lufs: f64,
    up: f64,
    down: f64,
    limiter: TpLimiter,
}

impl Leveler {
    pub const UP_SECONDS: f64 = 3.0;
    pub const DOWN_SECONDS: f64 = 1.0;
    pub const MAX_DB: f64 = 18.0;
    pub const GATE_LU: f64 = 20.0;

    pub fn new(target_lufs: f64, ceiling_db: f64, sr: f64) -> Leveler {
        let c = |s: f64| 1.0 - exp(-1.0 / (s * sr));
        Leveler {
            target: target_lufs,
            kw: [KWeighting::new(sr), KWeighting::new(sr)],
            block: ((sr / 10.0) as usize).max(1),
            acc: 0.0,
            count: 0,
            ring: [0.0; 30],
            ring_at: 0,
            filled: 0,
            desired_db: 0.0,
            gain_db: 0.0,
            short_term_lufs: -200.0,
            up: c(Self::UP_SECONDS),
            down: c(Self::DOWN_SECONDS),
            limiter: TpLimiter::new(sr, ceiling_db),
        }
    }

    pub fn latency(&self) -> usize {
        self.limiter.latency()
    }

    pub fn limiter_reduction_db(&self) -> f64 {
        self.limiter.gain_reduction_db()
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        for i in 0..n {
            let a = self.kw[0].process(l[i] as f64);
            let b = self.kw[1].process(r[i] as f64);
            self.acc += a * a + b * b;
            self.count += 1;
            if self.count == self.block {
                self.ring[self.ring_at] = self.acc / self.block as f64;
                self.ring_at = (self.ring_at + 1) % self.ring.len();
                self.filled = (self.filled + 1).min(self.ring.len());
                self.acc = 0.0;
                self.count = 0;
                let ms: f64 = self.ring.iter().take(self.filled).sum::<f64>() / self.filled as f64;
                let st = -0.691 + 10.0 * log10(ms.max(1e-30));
                self.short_term_lufs = st;
                if st > -70.0 && st > self.target - Self::GATE_LU {
                    self.desired_db = (self.target - st).clamp(-Self::MAX_DB, Self::MAX_DB);
                }
            }
            let k = if self.desired_db > self.gain_db {
                self.up
            } else {
                self.down
            };
            self.gain_db += k * (self.desired_db - self.gain_db);
            let g = db_to_lin(self.gain_db) as f32;
            l[i] *= g;
            r[i] *= g;
        }
        self.limiter.process(&mut l[..n], &mut r[..n]);
    }
}

// ---- One stage, live --------------------------------------------------------------------------

/// A stage built to run (live, or streamed over a file offline). Held by value: a chain is
/// built once off the audio thread, and boxing the big variants would only add pointers.
#[allow(clippy::large_enum_variant)]
pub enum Running {
    Biquad(Biquad),
    Denoise(Denoise),
    Expander(Expander),
    Gain(f32),
    Leveler(Leveler),
    Limiter(TpLimiter),
}

impl Running {
    /// Build `stage` at `sr`. `live`: a loudness stage becomes the [`Leveler`] (offline it is
    /// two-pass and not built here: [`build`](Running::build) returns None for it).
    /// `program_db`: the expander's starting program level, if known.
    pub fn build(stage: &Stage, sr: f64, live: bool, program_db: Option<f64>) -> Option<Running> {
        use BiquadKind as K;
        Some(match *stage {
            Stage::Highpass { freq, q } => Running::Biquad(Biquad::new(K::Highpass, freq, 0.0, q, sr)),
            Stage::Lowpass { freq, q } => Running::Biquad(Biquad::new(K::Lowpass, freq, 0.0, q, sr)),
            Stage::HighShelf { freq, gain_db, q } => {
                Running::Biquad(Biquad::new(K::HighShelf, freq, gain_db, q, sr))
            }
            Stage::LowShelf { freq, gain_db, q } => {
                Running::Biquad(Biquad::new(K::LowShelf, freq, gain_db, q, sr))
            }
            Stage::Peak { freq, gain_db, q } => Running::Biquad(Biquad::new(K::Peak, freq, gain_db, q, sr)),
            Stage::Denoise {
                noise_floor_db,
                reduction_db,
                track,
                track_range_db,
                noise_offset_db,
                fft_size,
            } => Running::Denoise(Denoise::new(
                noise_floor_db,
                reduction_db,
                track,
                track_range_db,
                noise_offset_db,
                denoise_fft_size(fft_size, sr, live),
                sr,
            )),
            Stage::Expander {
                threshold_db,
                below_program_db,
                ratio,
                attack_ms,
                release_ms,
                range_db,
                knee_db,
            } => Running::Expander(Expander::new(
                threshold_db,
                below_program_db,
                ratio,
                attack_ms,
                release_ms,
                range_db,
                knee_db,
                sr,
                program_db,
            )),
            Stage::Gain { db } => Running::Gain(db_to_lin(db) as f32),
            Stage::Loudness {
                target_lufs,
                true_peak_db,
                ..
            } => {
                if !live {
                    return None;
                }
                Running::Leveler(Leveler::new(target_lufs, true_peak_db, sr))
            }
            Stage::Limiter { ceiling_db } => Running::Limiter(TpLimiter::new(sr, ceiling_db)),
        })
    }

    pub fn latency(&self) -> usize {
        match self {
            Running::Denoise(d) => d.latency(),
            Running::Leveler(l) => l.latency(),
            Running::Limiter(l) => l.latency(),
            _ => 0,
        }
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        match self {
            Running::Biquad(b) => b.process(l, r),
            Running::Denoise(d) => d.process(l, r),
            Running::Expander(e) => e.process(l, r),
            Running::Gain(g) => {
                for x in l.iter_mut().chain(r.iter_mut()) {
                    *x *= *g;
                }
            }
            Running::Leveler(v) => v.process(l, r),
            Running::Limiter(v) => v.process(l, r),
        }
    }
}
