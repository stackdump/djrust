//! Shared by the two `loops` golden tests (`tests/loops_golden.rs` for the pure DSP,
//! `djhost/tests/loops_golden.rs` for the bank, which includes this file by path): the sources
//! `tools/paritydump/loops.swift` builds from exact operations only, and the comparison of a
//! render against the section's per-block stats, hash and decimated waveform.

#![allow(dead_code)]

use djrust::cid::{hex, sha256};
use djrust::json::JsonValue;

/// `lcgNoise`.
pub fn lcg_noise(seed: u32, n: usize) -> Vec<f32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((x as i32) >> 8) as f32 / 8_388_608.0
        })
        .collect()
}

/// `loopSource`: a source with harmonics everywhere, from exact operations only.
pub fn loop_source(n: usize, variant: usize) -> (Vec<f32>, Vec<f32>) {
    let noise_l = lcg_noise(if variant == 0 { 12345 } else { 67890 }, n);
    let noise_r = lcg_noise(if variant == 0 { 24680 } else { 13579 }, n);
    let saw_step = if variant == 0 { 37 } else { 53 };
    let tri_period: usize = if variant == 0 { 311 } else { 197 };
    // a rotation (an oscillator without sin)
    let c: f32 = if variant == 0 {
        0.998_795_46
    } else {
        0.997_180_8
    };
    let sn: f32 = if variant == 0 {
        0.049_067_674
    } else {
        0.075_022_85
    };
    let (mut x, mut y) = (0f32, 0.5f32);
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    for i in 0..n {
        let saw = (((i * saw_step) % 256) as i64 - 128) as f32 / 512.0;
        let p = i % tri_period;
        let tri = ((p * 2) as i64 - tri_period as i64).abs() as f32 / tri_period as f32 - 0.5;
        let nx = c * x - sn * y;
        let ny = sn * x + c * y;
        x = nx;
        y = ny;
        let a = 0.35f32 * y;
        let b = 0.3f32 * saw;
        let d = 0.2f32 * tri;
        let e = 0.04f32 * noise_l[i];
        l[i] = a + b + d + e;
        let f = 0.3f32 * x;
        let g = 0.25f32 * tri;
        let h = 0.2f32 * saw;
        let k = 0.04f32 * noise_r[i];
        r[i] = f + g + h + k;
    }
    (l, r)
}

/// `hashFloats`: sha256 of the little-endian float32 bits.
pub fn hash_floats(x: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for v in x {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    hex(&sha256(&bytes))
}

pub fn nums(v: &JsonValue, key: &str) -> Vec<f64> {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {key}"))
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// How a render differs from Swift's (`render` in loops.swift).
#[derive(Debug, Default, Clone, Copy)]
pub struct Measured {
    /// Both channels hash to Swift's: bit-identical.
    pub exact: bool,
    pub frames_match: bool,
    /// The largest difference in the decimated waveform, and in the per-block peak and RMS.
    pub max_wave: f64,
    pub max_peak: f64,
    pub max_rms: f64,
}

fn peak_of(x: &[f32]) -> f64 {
    x.iter().fold(0f32, |p, v| p.max(v.abs())) as f64
}

fn rms_of(x: &[f32]) -> f64 {
    (x.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// Compare a render (`l`, `r`) with the case Swift wrote.
pub fn compare(case: &JsonValue, l: &[f32], r: &[f32]) -> Measured {
    let frames = case.get("frames").unwrap().as_f64().unwrap() as usize;
    let block = case.get("block").unwrap().as_f64().unwrap() as usize;
    let decimate = case.get("decimate").unwrap().as_f64().unwrap() as usize;
    let mut m = Measured {
        frames_match: l.len() == frames && r.len() == frames,
        ..Measured::default()
    };
    if !m.frames_match {
        return m;
    }
    m.exact = case.get("hashL").unwrap().as_str() == Some(&hash_floats(l))
        && case.get("hashR").unwrap().as_str() == Some(&hash_floats(r));
    let (peak, rms) = (nums(case, "peak"), nums(case, "rms"));
    let mut k = 0;
    let mut from = 0;
    while from < frames {
        let to = (from + block).min(frames);
        for ch in [l, r] {
            m.max_peak = m.max_peak.max((peak_of(&ch[from..to]) - peak[k]).abs());
            m.max_rms = m.max_rms.max((rms_of(&ch[from..to]) - rms[k]).abs());
            k += 1;
        }
        from = to;
    }
    for (key, ch) in [("waveL", l), ("waveR", r)] {
        for (j, want) in nums(case, key).into_iter().enumerate() {
            m.max_wave = m.max_wave.max((ch[j * decimate] as f64 - want).abs());
        }
    }
    m
}

pub fn by_name<'a>(cases: &'a [JsonValue], name: &str) -> &'a JsonValue {
    cases
        .iter()
        .find(|c| c.get("name").and_then(JsonValue::as_str) == Some(name))
        .unwrap_or_else(|| panic!("no case {name}"))
}

/// A float32's bits as Swift wrote them (8 hex digits).
pub fn bits32(f: f32) -> String {
    format!("{:08x}", f.to_bits())
}
