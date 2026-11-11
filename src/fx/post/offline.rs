//! A post chain over a whole render: each streaming stage run as it runs live (its latency
//! taken off, so the output lines up with the input), the loudness stage in two passes
//! (measure, then apply, like ffmpeg's `loudnorm`), and the measurements before and after.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use libm::{exp, log10, pow};

use crate::json::{JsonObject, JsonValue};
use crate::musicality::loudness::{IntegratedLoudness, KWeighting};

use super::chain::{LoudnessMode, PostChain, Stage};
use super::dsp::{noise_rms_db, Fft, MinStats, Running, TpLimiter};

/// What a file measures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measurement {
    pub seconds: f64,
    /// BS.1770-4 integrated loudness; None for silence.
    pub lufs: Option<f64>,
    /// EBU Tech 3342 loudness range, LU.
    pub lra: f64,
    /// BS.1770-4 Annex 2 true peak, dBTP.
    pub true_peak_db: f64,
    pub sample_peak_db: f64,
    /// The steady noise under the music above 200 Hz, dBFS RMS: the median over the file of a
    /// minimum-statistics noise estimate (what the denoise tracks). None for silence.
    pub noise_floor_db: Option<f64>,
    /// The level of the quietest moments: the 5th percentile of 50 ms RMS windows (digital
    /// silence left out), dBFS.
    pub quiet_floor_db: Option<f64>,
}

fn r2(x: f64) -> JsonValue {
    if x.is_finite() {
        JsonValue::Number(libm::round(x * 100.0) / 100.0)
    } else {
        JsonValue::Null
    }
}

fn opt2(x: Option<f64>) -> JsonValue {
    x.map_or(JsonValue::Null, r2)
}

impl Measurement {
    pub fn json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("seconds", r2(self.seconds)),
            ("lufs", opt2(self.lufs)),
            ("lra", r2(self.lra)),
            ("true_peak_dbtp", r2(self.true_peak_db)),
            ("sample_peak_dbfs", r2(self.sample_peak_db)),
            ("noise_floor_dbfs", opt2(self.noise_floor_db)),
            ("quiet_floor_dbfs", opt2(self.quiet_floor_db)),
        ]))
    }
}

fn pow_db(p: f64) -> f64 {
    10.0 * log10(p.max(1e-30))
}

/// Measure a stereo render.
pub fn measure(l: &[f32], r: &[f32], sr: f64) -> Measurement {
    let n = l.len().min(r.len());
    let (l, r) = (&l[..n], &r[..n]);
    let mut peak = 0f32;
    for i in 0..n {
        peak = peak.max(l[i].abs()).max(r[i].abs());
    }
    Measurement {
        seconds: n as f64 / sr,
        lufs: IntegratedLoudness::integrated(l, Some(r), sr),
        lra: IntegratedLoudness::range(l, Some(r), sr),
        true_peak_db: crate::fx::true_peak::dbtp(l, Some(r)),
        sample_peak_db: if peak > 0.0 {
            20.0 * log10(peak as f64)
        } else {
            f64::NEG_INFINITY
        },
        noise_floor_db: noise_floor(l, r, sr),
        quiet_floor_db: quiet_floor(l, r, sr),
    }
}

fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let k = libm::round(p * (v.len() - 1) as f64) as usize;
    Some(v[k.min(v.len() - 1)])
}

fn quiet_floor(l: &[f32], r: &[f32], sr: f64) -> Option<f64> {
    let w = ((sr * 0.05) as usize).max(1);
    let mut levels = Vec::with_capacity(l.len() / w + 1);
    let mut at = 0;
    while at + w <= l.len() {
        let mut s = 0.0f64;
        for i in at..at + w {
            s += (l[i] as f64) * (l[i] as f64) + (r[i] as f64) * (r[i] as f64);
        }
        let p = s / (2 * w) as f64;
        if p > 1e-10 {
            levels.push(pow_db(p));
        }
        at += w;
    }
    percentile(&mut levels, 0.05)
}

fn noise_floor(l: &[f32], r: &[f32], sr: f64) -> Option<f64> {
    let n = if sr > 50_000.0 { 4096 } else { 2048 };
    let hop = n / 2;
    if l.len() < n * 4 {
        return None;
    }
    let fft = Fft::new(n);
    let win: Vec<f32> = (0..n)
        .map(|i| (0.5 * (1.0 - libm::cos(2.0 * core::f64::consts::PI * i as f64 / n as f64))) as f32)
        .collect();
    let bins = n / 2 + 1;
    let from = (200.0 * n as f64 / sr) as usize;
    let mut stats = MinStats::new(bins, hop as f64 / sr);
    let (mut re, mut im) = (vec![0f32; n], vec![0f32; n]);
    let (mut p, mut est) = (vec![0f32; bins], vec![0f32; bins]);
    let mut out = Vec::new();
    let mut at = 0;
    while at + n <= l.len() {
        let mut energy = 0.0f32;
        for i in 0..n {
            re[i] = l[at + i] * win[i];
            im[i] = r[at + i] * win[i];
            energy += re[i] * re[i] + im[i] * im[i];
        }
        fft.process(&mut re, &mut im, false);
        for k in 0..bins {
            let j = (n - k) % n;
            p[k] = 0.25 * (re[k] * re[k] + im[k] * im[k] + re[j] * re[j] + im[j] * im[j]);
        }
        stats.update(&p, &mut est);
        // digital silence (a gap, the tail) says nothing about the floor
        if stats.warm() && energy > 1e-12 {
            out.push(noise_rms_db(&est, n, &win, from));
        }
        at += hop;
    }
    percentile(&mut out, 0.5)
}

/// What one loudness stage did.
#[derive(Clone, Debug, PartialEq)]
pub struct LoudnessPass {
    pub mode: LoudnessMode,
    /// The gain applied (linear), or the range of gains (dynamic), dB.
    pub gain_db: (f64, f64),
    pub passes: usize,
}

/// What a chain did to a file.
#[derive(Clone, Debug, PartialEq)]
pub struct PostReport {
    pub chain: PostChain,
    pub before: Measurement,
    pub after: Measurement,
    pub loudness: Vec<LoudnessPass>,
}

impl PostReport {
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("preset", JsonValue::str(String::from(self.chain.label()))),
            ("chain", self.chain.json()),
            ("before", self.before.json()),
            ("after", self.after.json()),
        ]);
        if !self.loudness.is_empty() {
            o.set(
                "loudness",
                JsonValue::Array(
                    self.loudness
                        .iter()
                        .map(|p| {
                            JsonValue::Object(JsonObject::from_pairs([
                                ("mode", JsonValue::str(p.mode.id())),
                                ("gain_min_db", r2(p.gain_db.0)),
                                ("gain_max_db", r2(p.gain_db.1)),
                                ("passes", JsonValue::int(p.passes as i64)),
                            ]))
                        })
                        .collect(),
                ),
            );
        }
        JsonValue::Object(o)
    }
}

/// Run `chain` over `l` / `r` (same length) in place; measure before and after.
pub fn process(l: &mut Vec<f32>, r: &mut Vec<f32>, sr: f64, chain: &PostChain) -> PostReport {
    let n = l.len().min(r.len());
    l.truncate(n);
    r.truncate(n);
    let before = measure(l, r, sr);
    let mut passes = Vec::new();
    for stage in &chain.stages {
        match stage {
            Stage::Loudness {
                target_lufs,
                true_peak_db,
                lra,
                mode,
            } => passes.push(loudness(l, r, sr, *target_lufs, *true_peak_db, *lra, *mode)),
            other => {
                let program = IntegratedLoudness::integrated(l, Some(r), sr);
                if let Some(mut s) = Running::build(other, sr, false, program) {
                    stream(&mut s, l, r);
                }
            }
        }
    }
    let after = if chain.is_empty() {
        before.clone()
    } else {
        measure(l, r, sr)
    };
    PostReport {
        chain: chain.clone(),
        before,
        after,
        loudness: passes,
    }
}

/// Run a streaming stage over the whole buffer, its latency taken off.
pub fn stream(s: &mut Running, l: &mut Vec<f32>, r: &mut Vec<f32>) {
    let lat = s.latency();
    let n = l.len();
    l.resize(n + lat, 0.0);
    r.resize(n + lat, 0.0);
    const CHUNK: usize = 4096;
    let mut at = 0;
    while at < l.len() {
        let e = (at + CHUNK).min(l.len());
        s.process(&mut l[at..e], &mut r[at..e]);
        at = e;
    }
    l.drain(..lat);
    r.drain(..lat);
}

fn apply_limiter(l: &mut Vec<f32>, r: &mut Vec<f32>, sr: f64, ceiling: f64) {
    let mut lim = Running::Limiter(TpLimiter::new(sr, ceiling));
    stream(&mut lim, l, r);
}

/// Per-100 ms K-weighted mean square (both channels summed), as BS.1770 sums them.
fn block_energies(l: &[f32], r: &[f32], sr: f64) -> (usize, Vec<f64>) {
    let sub = ((sr / 10.0) as usize).max(1);
    let mut kw = [KWeighting::new(sr), KWeighting::new(sr)];
    let mut out = Vec::with_capacity(l.len() / sub + 1);
    let mut acc = 0.0;
    let mut c = 0;
    for i in 0..l.len() {
        let a = kw[0].process(l[i] as f64);
        let b = kw[1].process(r[i] as f64);
        acc += a * a + b * b;
        c += 1;
        if c == sub {
            out.push(acc / sub as f64);
            acc = 0.0;
            c = 0;
        }
    }
    if c > 0 {
        out.push(acc / c as f64);
    }
    (sub, out)
}

/// The loudness stage: measure, decide (linear or dynamic, as `loudnorm` does), apply, limit,
/// re-measure and trim until within 0.1 LU (at most three passes).
fn loudness(
    l: &mut Vec<f32>,
    r: &mut Vec<f32>,
    sr: f64,
    target: f64,
    ceiling: f64,
    lra_target: f64,
    mode: LoudnessMode,
) -> LoudnessPass {
    let Some(i_in) = IntegratedLoudness::integrated(l, Some(r), sr) else {
        return LoudnessPass {
            mode,
            gain_db: (0.0, 0.0),
            passes: 0,
        };
    };
    let tp_in = crate::fx::true_peak::dbtp(l, Some(r));
    let lra_in = IntegratedLoudness::range(l, Some(r), sr);
    let offset = target - i_in;
    let mode = match mode {
        LoudnessMode::Auto => {
            if tp_in + offset <= ceiling && lra_in <= lra_target {
                LoudnessMode::Linear
            } else {
                LoudnessMode::Dynamic
            }
        }
        m => m,
    };
    let (orig_l, orig_r) = (l.clone(), r.clone());
    // the gain curve, one value per 100 ms block, before any trim
    let (sub, curve) = match mode {
        LoudnessMode::Dynamic => dynamic_curve(l, r, sr, target, i_in),
        _ => (l.len().max(1), vec![offset]),
    };
    let mut trim = 0.0;
    let mut passes = 0;
    loop {
        passes += 1;
        l.copy_from_slice(&orig_l);
        r.copy_from_slice(&orig_r);
        apply_curve(l, r, sub, &curve, trim);
        // linear and under the ceiling already: no limiter (bit-for-bit one gain)
        if mode != LoudnessMode::Linear || tp_in + offset + trim > ceiling {
            apply_limiter(l, r, sr, ceiling);
        }
        let Some(i_out) = IntegratedLoudness::integrated(l, Some(r), sr) else {
            break;
        };
        let miss = target - i_out;
        if miss.abs() <= 0.1 || passes >= 3 {
            break;
        }
        trim += miss;
    }
    let (lo, hi) = curve
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), g| (a.min(*g), b.max(*g)));
    LoudnessPass {
        mode,
        gain_db: (lo + trim, hi + trim),
        passes,
    }
}

/// Apply a per-block dB curve (block centres, linearly interpolated) plus `trim` dB.
fn apply_curve(l: &mut [f32], r: &mut [f32], sub: usize, curve: &[f64], trim: f64) {
    if curve.len() == 1 {
        let g = pow(10.0, (curve[0] + trim) / 20.0) as f32;
        for x in l.iter_mut().chain(r.iter_mut()) {
            *x *= g;
        }
        return;
    }
    let half = sub as f64 / 2.0;
    for i in 0..l.len() {
        let t = (i as f64 - half) / sub as f64;
        let j = libm::floor(t).max(0.0) as usize;
        let f = (t - j as f64).clamp(0.0, 1.0);
        let a = curve[j.min(curve.len() - 1)];
        let b = curve[(j + 1).min(curve.len() - 1)];
        let g = pow(10.0, (a + (b - a) * f + trim) / 20.0) as f32;
        l[i] *= g;
        r[i] *= g;
    }
}

/// `loudnorm`'s dynamic mode, offline (so its windows can be centred): each 100 ms block's gain
/// is the target minus the short-term loudness (3 s around it), or the linear offset where the
/// short-term loudness is under the relative gate (I − 10 LU: breakdowns are not pumped up),
/// within ±12 dB of the linear offset, smoothed by a Gaussian (σ 350 ms).
fn dynamic_curve(l: &[f32], r: &[f32], sr: f64, target: f64, i_in: f64) -> (usize, Vec<f64>) {
    let (sub, e) = block_energies(l, r, sr);
    let offset = target - i_in;
    let gate = i_in - 10.0;
    let m = e.len();
    let mut raw = vec![offset; m];
    let mut prefix = vec![0.0f64; m + 1];
    for j in 0..m {
        prefix[j + 1] = prefix[j] + e[j];
    }
    for (j, g) in raw.iter_mut().enumerate() {
        let a = j.saturating_sub(15);
        let b = (j + 15).min(m);
        let ms = (prefix[b] - prefix[a]) / (b - a).max(1) as f64;
        let st = -0.691 + 10.0 * log10(ms.max(1e-30));
        if st > -70.0 && st >= gate {
            *g = (target - st).clamp(offset - 12.0, offset + 12.0);
        }
    }
    // Gaussian smoothing, σ 3.5 blocks, ±10 blocks (loudnorm's 21-tap filter)
    let taps: Vec<f64> = (-10i64..=10)
        .map(|k| exp(-((k * k) as f64) / (2.0 * 3.5 * 3.5)))
        .collect();
    let mut out = vec![0.0; m];
    for (j, o) in out.iter_mut().enumerate() {
        let (mut s, mut w) = (0.0, 0.0);
        for (t, k) in taps.iter().zip(-10i64..=10) {
            let x = j as i64 + k;
            if x >= 0 && (x as usize) < m {
                s += t * raw[x as usize];
                w += t;
            }
        }
        *o = s / w;
    }
    (sub, out)
}
