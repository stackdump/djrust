//! Time stretching for key lock (`Audio/TimeStretch.swift`): a loop played at another tempo
//! keeps its pitch by being re-rendered at that tempo, off the audio thread, and swapped in.
//!
//! WSOLA (waveform-similarity overlap-add, Verhelst & Roelands 1993): the output is built from
//! Hann-windowed frames of the input laid down at a fixed hop. Each frame is read near where the
//! tempo says it should be (`m * hop * ratio`), shifted by up to `tolerance` samples to the
//! place that looks most like the natural continuation of the frame before it (the
//! cross-correlation peak), so the overlapping halves add in phase and the pitch is the
//! input's.
//!
//! A loop is periodic, so input and output are both treated as circular: frames read across the
//! loop's end from its start, and the output's last frame overlaps its first. The result is a
//! seamless loop of exactly `length / ratio` samples (rounded), holding the same beats.
//!
//! Swift scores the candidates with `vDSP_conv` and picks the best with `vDSP_maxvi`; here the
//! correlation is a plain f32 sum in index order, so a score can differ from vDSP's in the last
//! bits (PORTING.md section H). A tie or near-tie in a periodic input can then pick another
//! period of the same waveform; the golden test measures how far that moves the output.

use alloc::vec;
use alloc::vec::Vec;

use crate::deck::turntable::SampleBuffer;

/// Frame length in samples at `sample_rate`: ~21 ms (1024 at 48 kHz), even (`frameLength`).
pub fn frame_length(sample_rate: f64) -> usize {
    2 * 32usize.max((sample_rate * 0.01067) as usize)
}

/// The loop `left` / `right` played `ratio` times as fast with its pitch kept: the output is
/// `round(count / ratio)` samples. Stereo channels share one alignment (worked out on their
/// mid), so the image holds. A loop shorter than two frames is resampled instead
/// (`TimeStretch.wsola`).
pub fn wsola(left: &[f32], right: &[f32], ratio: f64, sample_rate: f64) -> (Vec<f32>, Vec<f32>) {
    assert!(left.len() == right.len() && ratio > 0.0);
    let n_in = left.len();
    let n_out = 1usize.max(libm::round(n_in as f64 / ratio) as usize);
    let frame = frame_length(sample_rate);
    if !(n_in >= 2 * frame && n_out >= 2 * frame) {
        return (resample(left, n_out), resample(right, n_out));
    }
    let hop = frame / 2;
    let tolerance = frame / 2;
    // the analysis rate that lands exactly n_in input samples on n_out output samples
    let rate = n_in as f64 / n_out as f64;
    let window: Vec<f32> = (0..frame)
        .map(|k| 0.5 - 0.5 * libm::cosf(2.0 * core::f32::consts::PI * k as f32 / frame as f32))
        .collect();
    let mid: Vec<f32> = left.iter().zip(right).map(|(a, b)| (a + b) * 0.5).collect();

    let mut out_l = vec![0f32; n_out];
    let mut out_r = vec![0f32; n_out];
    let mut weight = vec![0f32; n_out];
    // scratch for the search: the region around the nominal position, the continuation, scores
    let mut region = vec![0f32; frame + 2 * tolerance];
    let mut natural = vec![0f32; frame];
    let mut score = vec![0f32; 2 * tolerance + 1];

    let frames = n_out.div_ceil(hop);
    let mut read: i64 = 0; // where the previous frame was read from (input samples, unwrapped)
    for m in 0..frames {
        if m > 0 {
            // the frame that would follow the last one seamlessly, and the best match near nominal
            gather(&mid, read + hop as i64, &mut natural);
            let nominal = libm::round((m * hop) as f64 * rate) as i64;
            gather(&mid, nominal - tolerance as i64, &mut region);
            correlate(&region, &natural, &mut score);
            let mut best: f32 = 0.0;
            let mut at = 0usize;
            for (i, &s) in score.iter().enumerate() {
                if i == 0 || s > best {
                    best = s;
                    at = i;
                }
            }
            read = nominal - tolerance as i64 + at as i64;
        }
        overlap_add(
            left,
            right,
            read,
            m * hop,
            &window,
            &mut out_l,
            &mut out_r,
            &mut weight,
        );
    }
    for i in 0..n_out {
        if weight[i] > 1e-6 {
            out_l[i] /= weight[i];
            out_r[i] /= weight[i];
        }
    }
    (out_l, out_r)
}

/// `vDSP_conv` with positive strides: `score[n] = sum_p region[n + p] * filter[p]`.
fn correlate(region: &[f32], filter: &[f32], score: &mut [f32]) {
    for (n, s) in score.iter_mut().enumerate() {
        let r = &region[n..n + filter.len()];
        let mut acc: f32 = 0.0;
        for (a, b) in r.iter().zip(filter) {
            acc += a * b;
        }
        *s = acc;
    }
}

/// Copy `into.len()` samples of `x` from `start` (any integer: the loop wraps) into `into`
/// (`gather`).
fn gather(x: &[f32], start: i64, into: &mut [f32]) {
    let n_in = x.len();
    let mut p = start.rem_euclid(n_in as i64) as usize;
    for d in into.iter_mut() {
        *d = x[p];
        p += 1;
        if p == n_in {
            p = 0;
        }
    }
}

/// Add one windowed frame of the input, read from `from`, at output sample `to` (both wrap)
/// (`overlapAdd`).
#[allow(clippy::too_many_arguments)]
fn overlap_add(
    left: &[f32],
    right: &[f32],
    from: i64,
    to: usize,
    window: &[f32],
    out_l: &mut [f32],
    out_r: &mut [f32],
    weight: &mut [f32],
) {
    let n_in = left.len();
    let n_out = out_l.len();
    let mut p = from.rem_euclid(n_in as i64) as usize;
    let mut q = to % n_out;
    for &w in window {
        out_l[q] += left[p] * w;
        out_r[q] += right[p] * w;
        weight[q] += w;
        p += 1;
        if p == n_in {
            p = 0;
        }
        q += 1;
        if q == n_out {
            q = 0;
        }
    }
}

/// `x` read at a constant rate to `n` samples (linear interpolation, wrapping): the fallback
/// for a loop too short to stretch (`resample`).
pub fn resample(x: &[f32], n: usize) -> Vec<f32> {
    let step = x.len() as f64 / n as f64;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let i0 = (p as usize).min(x.len() - 1);
            let i1 = if i0 + 1 == x.len() { 0 } else { i0 + 1 };
            x[i0] + (x[i1] - x[i0]) * (p - i0 as f64) as f32
        })
        .collect()
}

/// A loop's audio stretched to play at `ratio` times its speed with its pitch kept
/// (`TimeStretch.stretch`).
pub fn stretch(b: &SampleBuffer, ratio: f64) -> SampleBuffer {
    let (l, r) = wsola(&b.left, b.right(), ratio, b.sample_rate);
    SampleBuffer::new(l, Some(r), b.sample_rate)
}
