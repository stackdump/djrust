//! One K-weighting and integrated-loudness core, one true-peak: `djrust::loudness` (the
//! offline meter `SessionMix.report` uses) and `djrust::musicality::loudness` (the meter the
//! per-track gain and the level match use) used to carry a copy each of `KWeighting`, the
//! BS.1770 gating, the EBU Tech 3342 range and the Annex 2 true peak, beside
//! `fx::true_peak` (the limiter's). They are now facades over one implementation each; this
//! holds them to each other.
//!
//! Form (b) of the parity rules, but against the Rust that was there before, not Swift: every
//! number below is compared bit for bit (`to_bits`), on a sine and on a noisy fixture, at
//! three sample rates. The test was first run, and passed, with the three bodies still
//! separate; that is the proof the fold changed nothing. Swift's own numbers for these
//! functions are held by `musicality_dsp.rs` (`LoudnessRangeTests`) and `fx_limiter.rs`
//! (`TruePeakLimiterTests`).

use std::f64::consts::PI;

use djrust::fx::true_peak as fx;
use djrust::loudness as flat;
use djrust::musicality::loudness::{IntegratedLoudness, KWeighting, TruePeak, TruePeakDetector};

fn sine(hz: f64, amplitude: f64, seconds: f64, sr: f64) -> Vec<f32> {
    (0..(seconds * sr) as usize)
        .map(|i| (amplitude * (2.0 * PI * hz * i as f64 / sr).sin()) as f32)
        .collect()
}

/// xorshift64*: a fixed noise, the same on every machine.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let r = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// White noise in steps of level (loud, quiet, near silent, silent, loud again) with a sine
/// laid under it: gating, the relative threshold and the range all have something to do.
fn noisy(seed: u64, seconds: f64, sr: f64) -> Vec<f32> {
    let mut n = Noise(seed);
    let len = (seconds * sr) as usize;
    let under = sine(220.0, 0.2, seconds, sr);
    (0..len)
        .map(|i| {
            let t = i as f64 / len as f64;
            let level = if t < 0.3 {
                0.5
            } else if t < 0.55 {
                0.08
            } else if t < 0.65 {
                0.0005
            } else if t < 0.75 {
                0.0
            } else {
                0.3
            };
            (level * n.next() + under[i] as f64 * if level > 0.0 { 1.0 } else { 0.0 }) as f32
        })
        .collect()
}

struct Fixture {
    name: String,
    sr: f64,
    left: Vec<f32>,
    right: Vec<f32>,
}

fn fixtures() -> Vec<Fixture> {
    let mut v = Vec::new();
    for sr in [8_000.0, 44_100.0, 48_000.0] {
        v.push(Fixture {
            name: format!("sine 997 Hz -20 dBFS @ {sr}"),
            sr,
            left: sine(997.0, 0.1, 8.0, sr),
            right: sine(997.0, 0.1, 8.0, sr),
        });
        v.push(Fixture {
            name: format!("full-scale sine, channels apart @ {sr}"),
            sr,
            left: sine(sr / 4.0, 1.0, 4.0, sr),
            right: sine(3_001.0, 0.4, 4.0, sr),
        });
        v.push(Fixture {
            name: format!("noisy steps @ {sr}"),
            sr,
            left: noisy(0x9E37_79B9_7F4A_7C15, 9.0, sr),
            right: noisy(0xD1B5_4A32_D192_ED03, 9.0, sr),
        });
    }
    v
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

fn bits_opt(x: Option<f64>) -> Option<u64> {
    x.map(f64::to_bits)
}

#[test]
fn the_fixtures_are_what_the_comparisons_need() {
    // not a comparison: a guard that the noisy fixture reaches both gates and the range
    let f = fixtures()
        .into_iter()
        .find(|f| f.sr == 48_000.0 && f.name.starts_with("noisy"))
        .unwrap();
    let lufs = IntegratedLoudness::lufs(&f.left, &f.right, f.sr);
    let lra = IntegratedLoudness::range(&f.left, Some(&f.right), f.sr);
    assert!(lufs > -40.0 && lufs < -5.0, "lufs {lufs}");
    assert!(lra > 3.0, "lra {lra}");
    assert!(f.left.contains(&0.0), "a stretch of true silence");
}

#[test]
fn integrated_loudness_is_the_same_number_through_either_module() {
    for f in fixtures() {
        let (l, r, sr) = (&f.left[..], &f.right[..], f.sr);
        let n = f.name.as_str();
        assert_eq!(
            bits(flat::lufs(l, r, sr)),
            bits(IntegratedLoudness::lufs(l, r, sr)),
            "lufs {n}"
        );
        assert_eq!(
            bits_opt(flat::integrated(l, Some(r), sr)),
            bits_opt(IntegratedLoudness::integrated(l, Some(r), sr)),
            "integrated stereo {n}"
        );
        assert_eq!(
            bits_opt(flat::integrated(l, None, sr)),
            bits_opt(IntegratedLoudness::integrated(l, None, sr)),
            "integrated mono {n}"
        );
        // a prefix shorter than a block, a loop primed from its own end, the whole
        for (frames, prime) in [
            (l.len() / 40, 0),
            (l.len() / 2, 0),
            (l.len() / 2, 4_800),
            (l.len(), 0),
            (l.len(), 12_345),
        ] {
            assert_eq!(
                bits_opt(flat::integrated_frames(l, Some(r), frames, sr, prime)),
                bits_opt(IntegratedLoudness::integrated_count(
                    l,
                    Some(r),
                    frames,
                    sr,
                    prime
                )),
                "integrated_frames {frames} {prime} {n}"
            );
            assert_eq!(
                bits_opt(flat::integrated_frames(l, None, frames, sr, prime)),
                bits_opt(IntegratedLoudness::integrated_count(
                    l, None, frames, sr, prime
                )),
                "integrated_frames mono {frames} {prime} {n}"
            );
        }
        assert_eq!(
            bits(flat::range(l, Some(r), sr)),
            bits(IntegratedLoudness::range(l, Some(r), sr)),
            "range {n}"
        );
        assert_eq!(
            bits(flat::range(l, None, sr)),
            bits(IntegratedLoudness::range(l, None, sr)),
            "range mono {n}"
        );
    }
    // silence and nothing at all
    let z = vec![0.0f32; 48_000];
    assert_eq!(
        bits(flat::lufs(&z, &z, 48_000.0)),
        bits(IntegratedLoudness::lufs(&z, &z, 48_000.0))
    );
    assert_eq!(flat::lufs(&[], &[], 48_000.0), -200.0);
    assert_eq!(
        flat::range(&[], None, 48_000.0),
        IntegratedLoudness::range(&[], None, 48_000.0)
    );
}

#[test]
fn the_k_weighting_filters_the_same_stream_the_same_way() {
    for f in fixtures() {
        let mut a = flat::KWeighting::new(f.sr);
        let mut b = KWeighting::new(f.sr);
        for (i, &x) in f.left.iter().enumerate() {
            let (ya, yb) = (a.process(x as f64), b.process(x as f64));
            assert_eq!(bits(ya), bits(yb), "{} at {i}", f.name);
        }
    }
}

// Form (a): `TruePeak.phases` in `Musicality/TruePeak.swift`, copied literal for literal. With one
// copy of the annex's FIR left, nothing above can see a changed coefficient (every comparison is
// of the implementation with itself) and the Swift-literal true-peak suites have a 0.2 dB
// tolerance, so the coefficients are pinned here exactly.
#[test]
fn the_annex_coefficients_are_swifts() {
    #[rustfmt::skip]
    let swift: [[f64; 12]; 4] = [
        [0.0017089843750, 0.0109863281250, -0.0196533203125, 0.0332031250000, -0.0594482421875,
         0.1373291015625, 0.9721679687500, -0.1022949218750, 0.0476074218750, -0.0266113281250,
         0.0148925781250, -0.0083007812500],
        [-0.0291748046875, 0.0292968750000, -0.0517578125000, 0.0891113281250, -0.1665039062500,
         0.4650878906250, 0.7797851562500, -0.2003173828125, 0.1015625000000, -0.0582275390625,
         0.0330810546875, -0.0189208984375],
        [-0.0189208984375, 0.0330810546875, -0.0582275390625, 0.1015625000000, -0.2003173828125,
         0.7797851562500, 0.4650878906250, -0.1665039062500, 0.0891113281250, -0.0517578125000,
         0.0292968750000, -0.0291748046875],
        [-0.0083007812500, 0.0148925781250, -0.0266113281250, 0.0476074218750, -0.1022949218750,
         0.9721679687500, 0.1373291015625, -0.0594482421875, 0.0332031250000, -0.0196533203125,
         0.0109863281250, 0.0017089843750],
    ];
    for (p, row) in swift.iter().enumerate() {
        for (k, &want) in row.iter().enumerate() {
            assert_eq!(
                fx::PHASES[p][k].to_bits(),
                want.to_bits(),
                "phase {p} tap {k}"
            );
        }
    }
    // `flatCoefficients`: phase-major
    let flat: Vec<f64> = swift.iter().flatten().copied().collect();
    assert_eq!(fx::flat_coefficients(), flat);
}

#[test]
fn true_peak_is_one_implementation_in_three_modules() {
    assert_eq!((flat::TP_TAPS, flat::TP_DELAY), (fx::TAPS, fx::DELAY));
    assert_eq!((TruePeak::TAPS, TruePeak::DELAY), (fx::TAPS, fx::DELAY));
    assert_eq!(TruePeak::PHASES, fx::PHASES);
    assert_eq!(flat::flat_coefficients(), fx::flat_coefficients());
    assert_eq!(TruePeak::flat_coefficients(), fx::flat_coefficients());
    for f in fixtures() {
        let (l, r) = (&f.left[..], &f.right[..]);
        let n = f.name.as_str();
        for right in [Some(r), None] {
            let want = bits(fx::peak(l, right));
            assert_eq!(bits(flat::true_peak(l, right)), want, "peak {n}");
            assert_eq!(bits(TruePeak::peak(l, right)), want, "TruePeak::peak {n}");
            let want = bits(fx::dbtp(l, right));
            assert_eq!(bits(flat::dbtp(l, right)), want, "dbtp {n}");
            assert_eq!(bits(TruePeak::dbtp(l, right)), want, "TruePeak::dbtp {n}");
        }
    }
    assert_eq!(flat::dbtp(&[0.0; 3], None), -200.0);
    assert_eq!(TruePeak::dbtp(&[0.0; 3], None), -200.0);
    assert_eq!(fx::dbtp(&[0.0; 3], None), -200.0);
}

#[test]
fn the_streaming_detectors_agree_sample_for_sample() {
    for f in fixtures() {
        let mut a = fx::Detector::new();
        let mut b = TruePeakDetector::new();
        let mut c = flat::TruePeakDetector::new();
        for (i, &x) in f.right.iter().enumerate() {
            let want = bits(a.process(x as f64));
            assert_eq!(
                bits(b.process(x as f64)),
                want,
                "musicality {} at {i}",
                f.name
            );
            assert_eq!(
                bits(c.process(x as f64)),
                want,
                "loudness {} at {i}",
                f.name
            );
        }
        assert_eq!(bits(b.peak), bits(a.peak), "peak {}", f.name);
        assert_eq!(bits(c.peak), bits(a.peak), "peak {}", f.name);
        a.reset();
        b.reset();
        c.reset();
        assert_eq!((a.peak, b.peak, c.peak), (0.0, 0.0, 0.0));
        // history cleared: the first sample after a reset sees nothing of the stream before
        let want = bits(a.process(0.25));
        assert_eq!(bits(b.process(0.25)), want);
        assert_eq!(bits(c.process(0.25)), want);
    }
}

#[test]
fn the_interpolated_peak_is_one_function() {
    let coefficients = fx::flat_coefficients();
    let mut n = Noise(0x0123_4567_89AB_CDEF);
    for _ in 0..2_000 {
        let history: Vec<f64> = (0..fx::TAPS).map(|_| n.next()).collect();
        let want = bits(fx::interpolated_peak(&coefficients, &history));
        assert_eq!(bits(flat::interpolated_peak(&coefficients, &history)), want);
        assert_eq!(
            bits(TruePeak::interpolated_peak(&coefficients, &history)),
            want
        );
    }
}
