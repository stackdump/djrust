//! The key of a loop, estimated from its audio (docs/MPK.md, "Loop root"): the pitch class a
//! loop sits on, so a key pressed on the controller transposes it by the right interval instead
//! of from an assumed C.
//!
//! The method is the textbook one: a 12-bin chroma (the FFT magnitudes between 60 Hz and 5 kHz
//! folded onto pitch classes, summed over the loop), correlated against the 24 rotations of the
//! Krumhansl-Kessler major and minor key profiles (C. L. Krumhansl and E. J. Kessler, "Tracing
//! the dynamic changes in perceived tonal organization in a spatial representation of musical
//! keys", Psychological Review 89, 1982: the probe-tone ratings, published numbers). The best
//! rotation is the key; its tonic is the root. A loop with no pitch to speak of (drums, noise)
//! has a flat chroma and no key: [`LoopKey::tonal`] says so and the root is left alone.
//!
//! The estimate is a guess about audio: a loop that stays on one chord answers with that chord's
//! tonic, a bass line with its root, a melody with the key it is in. `confidence` is how much
//! better the winner is than the best key of the other mode and a fifth away (0 to 1). A hand
//! (`loopRoot`) always wins.

use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::fft::{hann_norm, Fft};

/// Krumhansl-Kessler probe-tone profile, major, tonic first (1982, table 1).
const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
/// The same, minor.
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// Pitch class names, sharps.
pub const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// FFT size and hop of the chroma, at the loop's sample rate.
const SIZE: usize = 8192;
const HOP: usize = 4096;
/// The band folded onto pitch classes.
const MIN_HZ: f64 = 60.0;
const MAX_HZ: f64 = 5000.0;
/// The most audio read (seconds): a loop is a few bars; longer is thinned, not truncated.
const MAX_SECONDS: f64 = 40.0;

/// A loop's estimated key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopKey {
    /// The tonic's pitch class, 0 = C.
    pub root: u8,
    pub minor: bool,
    /// The correlation with the winning profile, -1..1.
    pub correlation: f64,
    /// How clearly it won, 0..1.
    pub confidence: f64,
    /// Enough pitch to trust: false for drums and noise.
    pub tonal: bool,
}

impl LoopKey {
    /// "A minor", "C major".
    pub fn name(&self) -> alloc::string::String {
        alloc::format!(
            "{} {}",
            NAMES[self.root as usize % 12],
            if self.minor { "minor" } else { "major" }
        )
    }
}

/// Pitch-class name to number ("C#", "Db", "a"): 0 = C. None for anything else.
pub fn pitch_class(name: &str) -> Option<u8> {
    let mut chars = name.trim().chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let acc = match chars.next() {
        None => 0,
        Some('#') | Some('♯') => 1,
        Some('b') | Some('♭') => -1,
        _ => return None,
    };
    if chars.next().is_some() {
        return None;
    }
    Some(((base + acc + 12) % 12) as u8)
}

/// A key in a sample's file name, as sample packs write it: `Am`, `F#min`, `C_major`, `Gmaj`,
/// `D-minor`, `Cm7` is not (it is a chord). Looks at the `_ - space .` separated words. The
/// first word that is a key wins; a lone `A` or `E` is not taken (they are too often just a
/// letter): a key needs its mode, `m`, `min`, `minor`, `maj` or `major`.
pub fn key_in_name(name: &str) -> Option<(u8, bool)> {
    let words: Vec<alloc::string::String> = name
        .split(['_', '-', ' ', '.', '(', ')', '[', ']'])
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let is_mode = |r: &str| matches!(r, "m" | "min" | "minor" | "maj" | "major");
    for (i, l) in words.iter().enumerate() {
        let letter = match l.chars().next() {
            Some(c) if ('a'..='g').contains(&c) => c,
            _ => continue,
        };
        let rest = &l[1..];
        // an accidental: '#', or a 'b' with a mode after it ("bbmaj" is B flat major, "bmin" B
        // minor); the mode is in this word ("am") or the next ("a", "minor")
        let (acc, rest) = if let Some(r) = rest.strip_prefix('#') {
            ("#", r)
        } else if let Some(r) = rest
            .strip_prefix('b')
            .filter(|r| is_mode(r) || (r.is_empty() && words.get(i + 1).is_some_and(|n| is_mode(n))))
        {
            ("b", r)
        } else {
            ("", rest)
        };
        let mode = if rest.is_empty() {
            match words.get(i + 1) {
                Some(n) if is_mode(n) => n.as_str(),
                _ => continue,
            }
        } else {
            rest
        };
        if !is_mode(mode) {
            continue;
        }
        let minor = matches!(mode, "m" | "min" | "minor");
        let mut n = alloc::string::String::new();
        n.push(letter.to_ascii_uppercase());
        n.push_str(acc);
        if let Some(pc) = pitch_class(&n) {
            return Some((pc, minor));
        }
    }
    None
}

/// Pearson correlation of two 12-vectors.
fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let (ma, mb) = (a.iter().sum::<f64>() / 12.0, b.iter().sum::<f64>() / 12.0);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        return 0.0;
    }
    num / libm::sqrt(da * db)
}

/// The 12-bin chroma of `x` (mono) at `sr`: magnitudes folded onto pitch classes, summed over
/// the frames, then normalised to sum to 1. All zeros for silence.
pub fn chroma(x: &[f32], sr: f64) -> [f64; 12] {
    let mut out = [0.0f64; 12];
    if x.len() < SIZE || sr.is_nan() || sr <= 0.0 {
        return out;
    }
    let half = SIZE / 2;
    let bin_hz = sr / SIZE as f64;
    let mut class_of: Vec<i8> = vec![-1; half];
    for (k, c) in class_of.iter_mut().enumerate().skip(1) {
        let f = k as f64 * bin_hz;
        if f >= MIN_HZ && f < MAX_HZ.min(sr / 2.0) {
            let midi = 69.0 + 12.0 * libm::log2(f / 440.0);
            *c = (((libm::round(midi) as i64) % 12 + 12) % 12) as i8;
        }
    }
    // a long loop is read with a larger hop, so the cost stays bounded
    let max_frames = (MAX_SECONDS * sr / HOP as f64) as usize;
    let frames = (x.len() - SIZE) / HOP + 1;
    let step = (frames / max_frames.max(1)).max(1);
    let window = hann_norm(SIZE);
    let mut fft = Fft::new(SIZE);
    let mut buf = vec![0.0f64; SIZE];
    let mut f = 0;
    while f < frames {
        let start = f * HOP;
        for i in 0..SIZE {
            buf[i] = x[start + i] as f64 * window[i];
        }
        let p = fft.zrip(&buf);
        for k in 1..half {
            if class_of[k] >= 0 {
                // the square root keeps one loud partial from owning the frame
                out[class_of[k] as usize] += libm::sqrt(libm::sqrt(p.re[k] * p.re[k] + p.im[k] * p.im[k]));
            }
        }
        f += step;
    }
    let total: f64 = out.iter().sum();
    if total > 0.0 {
        for v in out.iter_mut() {
            *v /= total;
        }
    }
    out
}

/// The key of a mono signal. None for a signal shorter than the analysis frame or silent.
pub fn estimate(x: &[f32], sr: f64) -> Option<LoopKey> {
    let c = chroma(x, sr);
    if c.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    Some(from_chroma(&c))
}

/// Mix a stereo (or mono) pair to mono and estimate.
pub fn estimate_stereo(left: &[f32], right: Option<&[f32]>, sr: f64) -> Option<LoopKey> {
    match right {
        Some(r) if r.len() == left.len() => {
            let m: Vec<f32> = left.iter().zip(r).map(|(a, b)| (a + b) * 0.5).collect();
            estimate(&m, sr)
        }
        _ => estimate(left, sr),
    }
}

/// The key a chroma points at.
pub fn from_chroma(c: &[f64; 12]) -> LoopKey {
    let mut best = (f64::NEG_INFINITY, 0usize, false);
    // the best of the other mode / a fifth away, to say how clearly the winner won
    let mut scores = [[0.0f64; 12]; 2];
    for (m, profile) in [MAJOR, MINOR].iter().enumerate() {
        for tonic in 0..12 {
            let mut rotated = [0.0f64; 12];
            for i in 0..12 {
                rotated[(i + tonic) % 12] = profile[i];
            }
            let r = pearson(c, &rotated);
            scores[m][tonic] = r;
            if r > best.0 {
                best = (r, tonic, m == 1);
            }
        }
    }
    let (r, root, minor) = best;
    // the runner-up that is a different key (not the winner)
    let mut second = f64::NEG_INFINITY;
    for (m, row) in scores.iter().enumerate() {
        for (t, r) in row.iter().enumerate() {
            if (t, m == 1) != (root, minor) && *r > second {
                second = *r;
            }
        }
    }
    let confidence = ((r - second) / 0.25).clamp(0.0, 1.0);
    // flat chroma: drums and noise fold onto every pitch class alike
    let mean = 1.0 / 12.0;
    let peak = c.iter().cloned().fold(0.0f64, f64::max);
    let tonal = r >= 0.55 && peak >= mean * 1.6;
    LoopKey {
        root: root as u8,
        minor,
        correlation: r,
        confidence,
        tonal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const SR: f64 = 44_100.0;

    fn midi_hz(n: f64) -> f64 {
        440.0 * libm::pow(2.0, (n - 69.0) / 12.0)
    }

    /// Notes (midi, seconds each) as a sum of a few harmonics each, one after the other.
    fn melody(notes: &[i32], seconds_each: f64) -> Vec<f32> {
        let mut out = Vec::new();
        for n in notes {
            let f = midi_hz(*n as f64);
            let len = (seconds_each * SR) as usize;
            for i in 0..len {
                let t = i as f64 / SR;
                let env = libm::sin(core::f64::consts::PI * i as f64 / len as f64);
                let mut v = 0.0;
                for (h, a) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.25)] {
                    v += a * libm::sin(2.0 * core::f64::consts::PI * f * h * t);
                }
                out.push((0.2 * env * v) as f32);
            }
        }
        out
    }

    fn noise(n: usize) -> Vec<f32> {
        // a fixed xorshift: white noise, no pitch
        let mut s = 0x2545F4914F6CDD1Du64;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 40) as f32 / (1u64 << 23) as f32 - 1.0) * 0.3
            })
            .collect()
    }

    #[test]
    fn a_c_major_scale_and_chord_tones_read_as_c_major() {
        // C D E F G A B C E G C: the scale then the chord
        let x = melody(&[60, 62, 64, 65, 67, 69, 71, 72, 64, 67, 60, 60, 64, 67], 0.4);
        let k = estimate(&x, SR).unwrap();
        assert!(k.tonal, "{k:?}");
        assert_eq!((k.root, k.minor), (0, false), "{}", k.name());
    }

    #[test]
    fn an_a_minor_line_reads_as_a_minor_not_its_relative() {
        // A B C D E F G# A, then A C E A
        let x = melody(&[57, 59, 60, 62, 64, 65, 68, 69, 57, 60, 64, 57, 57, 60, 64], 0.4);
        let k = estimate(&x, SR).unwrap();
        assert!(k.tonal, "{k:?}");
        assert_eq!((k.root, k.minor), (9, true), "{}", k.name());
    }

    #[test]
    fn a_transposed_line_moves_the_root_by_the_same_interval() {
        let line = [60, 62, 64, 65, 67, 69, 71, 72, 64, 67, 60, 60, 64, 67];
        let c = estimate(&melody(&line, 0.4), SR).unwrap();
        for up in [2, 5, 7] {
            let shifted: Vec<i32> = line.iter().map(|n| n + up).collect();
            let k = estimate(&melody(&shifted, 0.4), SR).unwrap();
            assert_eq!(k.root as i32, (c.root as i32 + up) % 12, "+{up}: {}", k.name());
            assert_eq!(k.minor, c.minor);
        }
    }

    #[test]
    fn noise_and_silence_have_no_key() {
        let k = estimate(&noise(SR as usize * 4), SR).unwrap();
        assert!(!k.tonal, "{k:?}");
        assert!(estimate(&vec![0.0; SR as usize], SR).is_none());
        assert!(estimate(&[0.0; 100], SR).is_none(), "shorter than a frame");
    }

    #[test]
    fn names_and_file_names_give_pitch_classes() {
        assert_eq!(pitch_class("C"), Some(0));
        assert_eq!(pitch_class("c#"), Some(1));
        assert_eq!(pitch_class("Db"), Some(1));
        assert_eq!(pitch_class("Bb"), Some(10));
        assert_eq!(pitch_class("H"), None);
        assert_eq!(pitch_class("Cm"), None);
        assert_eq!(key_in_name("Telefrico_Vox_128_Am_phrase"), Some((9, true)));
        assert_eq!(key_in_name("vocal 120bpm F#min hook"), Some((6, true)));
        assert_eq!(key_in_name("Pad-C_major-01"), Some((0, false)));
        assert_eq!(key_in_name("vox_Bbmaj"), Some((10, false)));
        assert_eq!(key_in_name("Bm lead"), Some((11, true)));
        assert_eq!(key_in_name("bmin_loop"), Some((11, true)));
        assert_eq!(key_in_name("vox A take"), None, "a letter is not a key");
        assert_eq!(key_in_name("kick 01"), None);
        assert_eq!(key_in_name("Cm7_chord"), None);
    }

    #[test]
    fn the_profiles_are_the_published_ones() {
        // the tonic is the strongest degree of each profile
        for p in [MAJOR, MINOR] {
            assert!(p[0] >= p.iter().cloned().fold(0.0, f64::max) - 1e-9);
        }
        assert_eq!(MAJOR.len(), 12);
    }
}
