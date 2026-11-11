//! Tempo-preserving pitch shift for loops: resample, then WSOLA time-stretch back to the
//! original length. Pure, deterministic, allocation only on the calling (non-audio) thread.
//!
//! Why this and not a plain resample: a loop transposed by resampling plays faster or slower
//! (+12 semitones = half as long), so it falls off the bar grid. Here the output has *exactly*
//! the input's length, so it swaps in under a playing turntable in phase
//! (`TurntableCommand::Swap`) and the loop stays on the beat.
//!
//! WSOLA (waveform-similarity overlap-add): output frames are laid down on a fixed hop;
//! each takes its input from near where the stretch ratio says, nudged to the offset whose
//! waveform best continues the previous frame. The search runs on the mono sum so both
//! channels use the same offsets (the stereo image does not wobble). `circular` tiles the
//! loop three times and keeps the middle, so the seam where the loop wraps is continuous.
//!
//! Not for every material: drums smear a little at large shifts. Past ±6 semitones
//! expect artefacts; the host exposes `Resample` ("tape") mode for that case.

use alloc::vec;
use alloc::vec::Vec;

const FRAME: usize = 2048;
const HOP: usize = FRAME / 2;
const TOLERANCE: i64 = 384;
const COARSE: i64 = 4;
const DECIMATE: usize = 4;

/// Frequency ratio of `semitones` equal-tempered steps.
pub fn ratio(semitones: i32) -> f64 {
    libm::exp2(semitones as f64 / 12.0)
}

/// `x` read at `r` times speed (4-point Hermite). Length `n / r`.
pub fn resample(x: &[f32], r: f64) -> Vec<f32> {
    if x.is_empty() || !(r > 0.0) {
        return Vec::new();
    }
    let m = libm::round(x.len() as f64 / r) as usize;
    let at = |i: i64| -> f32 {
        if i < 0 || i as usize >= x.len() {
            0.0
        } else {
            x[i as usize]
        }
    };
    (0..m)
        .map(|k| {
            let p = k as f64 * r;
            let i = libm::floor(p) as i64;
            let t = (p - i as f64) as f32;
            let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            let c1 = 0.5 * (c - a);
            let c2 = a - 2.5 * b + 2.0 * c - 0.5 * d;
            let c3 = 0.5 * (d - a) + 1.5 * (b - c);
            ((c3 * t + c2) * t + c1) * t + b
        })
        .collect()
}

fn hann() -> Vec<f32> {
    (0..FRAME)
        .map(|i| {
            let p = 2.0 * core::f64::consts::PI * i as f64 / FRAME as f64;
            (0.5 - 0.5 * libm::cos(p)) as f32
        })
        .collect()
}

/// WSOLA: stretch the channels `chans` (equal lengths) to `out_len` frames, same pitch.
fn stretch(chans: &[&[f32]], out_len: usize) -> Vec<Vec<f32>> {
    let n = chans[0].len();
    let mut out: Vec<Vec<f32>> = chans.iter().map(|_| vec![0.0; out_len]).collect();
    if n < FRAME * 2 || out_len == 0 {
        // too short to window: nearest-neighbour fit keeps the length contract
        for (c, o) in chans.iter().zip(out.iter_mut()) {
            for (k, v) in o.iter_mut().enumerate() {
                *v = c[(k * n / out_len).min(n - 1)];
            }
        }
        return out;
    }
    let mono: Vec<f32> = (0..n)
        .map(|i| chans.iter().map(|c| c[i]).sum::<f32>() / chans.len() as f32)
        .collect();
    let win = hann();
    let scale = n as f64 / out_len as f64;
    let at = |x: &[f32], i: i64| -> f32 {
        if i < 0 || i as usize >= x.len() {
            0.0
        } else {
            x[i as usize]
        }
    };
    let frames = out_len / HOP + 2;
    let mut prev: Option<i64> = None;
    for k in 0..frames {
        let o = k as i64 * HOP as i64 - HOP as i64;
        let nominal = libm::round(o as f64 * scale) as i64;
        let chosen = match prev {
            None => nominal,
            Some(p) => {
                // what the previous frame would have played next
                let target = p + HOP as i64;
                let score = |cand: i64| -> f64 {
                    let (mut dot, mut e) = (0.0f64, 1e-9f64);
                    let mut j = 0;
                    while j < FRAME {
                        let a = at(&mono, target + j as i64) as f64;
                        let b = at(&mono, cand + j as i64) as f64;
                        dot += a * b;
                        e += b * b;
                        j += DECIMATE;
                    }
                    dot / libm::sqrt(e)
                };
                let mut best = (nominal, f64::MIN);
                let mut d = -TOLERANCE;
                while d <= TOLERANCE {
                    let s = score(nominal + d);
                    if s > best.1 {
                        best = (nominal + d, s);
                    }
                    d += COARSE;
                }
                let centre = best.0;
                for d in -(COARSE - 1)..COARSE {
                    let s = score(centre + d);
                    if s > best.1 {
                        best = (centre + d, s);
                    }
                }
                best.0
            }
        };
        prev = Some(chosen);
        for (c, o) in chans.iter().zip(out.iter_mut()) {
            for j in 0..FRAME {
                let pos = o_index(o_start(k), j);
                if pos < 0 || pos as usize >= out_len {
                    continue;
                }
                o[pos as usize] += win[j] * at(c, chosen + j as i64);
            }
        }
    }
    out
}

fn o_start(k: usize) -> i64 {
    k as i64 * HOP as i64 - HOP as i64
}
fn o_index(start: i64, j: usize) -> i64 {
    start + j as i64
}

/// Transpose by `semitones`, keeping the length exactly. `chans` are equal-length channels.
/// `circular`: the audio is a loop, so make the wrap seamless.
pub fn pitch_shift(chans: &[&[f32]], semitones: i32, circular: bool) -> Vec<Vec<f32>> {
    let n = chans.first().map_or(0, |c| c.len());
    if semitones == 0 || n == 0 {
        return chans.iter().map(|c| c.to_vec()).collect();
    }
    let r = ratio(semitones);
    let tiled: Vec<Vec<f32>>;
    let src: Vec<&[f32]> = if circular {
        tiled = chans
            .iter()
            .map(|c| {
                let mut t = Vec::with_capacity(n * 3);
                for _ in 0..3 {
                    t.extend_from_slice(c);
                }
                t
            })
            .collect();
        tiled.iter().map(|v| v.as_slice()).collect()
    } else {
        chans.to_vec()
    };
    let total = src[0].len();
    let resampled: Vec<Vec<f32>> = src.iter().map(|c| resample(c, r)).collect();
    let views: Vec<&[f32]> = resampled.iter().map(|v| v.as_slice()).collect();
    let stretched = stretch(&views, total);
    if circular {
        stretched.into_iter().map(|v| v[n..2 * n].to_vec()).collect()
    } else {
        stretched
    }
}

/// Stretch (not pitch) `chans` to `out_len` frames, same pitch: fit a vocal or sample to
/// the master tempo.
pub fn time_fit(chans: &[&[f32]], out_len: usize) -> Vec<Vec<f32>> {
    if chans.is_empty() {
        return Vec::new();
    }
    stretch(chans, out_len)
}

/// The pitch class (0 = C) and cents offset of a frequency, against A4 = 440.
pub fn pitch_class(hz: f64) -> Option<(u8, f64)> {
    if !(hz > 0.0) {
        return None;
    }
    let midi = 69.0 + 12.0 * libm::log2(hz / 440.0);
    let nearest = libm::round(midi);
    Some((
        (nearest as i64).rem_euclid(12) as u8,
        (midi - nearest) * 100.0,
    ))
}

/// Semitones to move a loop whose root is pitch class `root` so that it sits on `key`
/// (any MIDI note): the shortest way round, -6..=5, so quality stays as high as it can.
pub fn semitones_to(root: u8, key: u8) -> i32 {
    let d = (key as i32 - root as i32).rem_euclid(12);
    if d > 5 {
        d - 12
    } else {
        d
    }
}
