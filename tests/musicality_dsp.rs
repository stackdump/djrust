//! Ports of the Swift DSP / measurement tests: `LoudnessRangeTests` (loudness range and true
//! peak), `SpectrumTests`, `PitchMeasureTests`, `BeatTrackerTests`, `SectionSegmenterTests`
//! (synthetic signals; the two that analyse the app's own renders are in
//! `musicality_renders.rs`), `TargetsTableTests` (the data contract), and the Space / Echo
//! suites driven at block level (`FdnReverb`, `PingPong`). (The runner mixes them in now; its
//! sends are held to Swift's renders by `swift/runner.json`.)

use std::f64::consts::PI;

use djrust::gen::tables::BeatsTables;
use djrust::musicality::beat_tracker::BeatTracker;
use djrust::musicality::dj::{duck_depth, sampled_width, stereo_placement};
use djrust::musicality::echo::{EchoSpec, PingPong};
use djrust::musicality::loudness::{IntegratedLoudness, KWeighting, TruePeak, TruePeakDetector};
use djrust::musicality::mix_balance::MixBalance;
use djrust::musicality::mix_targets::{MixTargets, REFERENCE_LUFS};
use djrust::musicality::pitch_measure;
use djrust::musicality::section_segmenter::SectionSegmenter;
use djrust::musicality::space::{FdnReverb, SpaceSpec};
use djrust::musicality::spectrum::{Spectrum, BANDS_HZ};
use djrust::musicality::synth_voice::{HfTilt, KickTuning};
use djrust::wave::groove::FAMILY_NAMES;
use djrust::wave::synth::biquad;

// MARK: loudness range, true peak

const SR: f64 = 48_000.0;

fn sine(hz: f64, amplitude: f64, seconds: f64, phase: f64) -> Vec<f32> {
    (0..(seconds * SR) as usize)
        .map(|i| (amplitude * (2.0 * PI * hz * i as f64 / SR + phase).sin()) as f32)
        .collect()
}

fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

#[test]
fn steady_sine_has_no_range() {
    let x = sine(1000.0, db(-20.0), 20.0, 0.0);
    let lra = IntegratedLoudness::range(&x, Some(&x), SR);
    assert!((0.0..0.2).contains(&lra), "lra {lra}");
}

#[test]
fn two_levels_ten_db_apart_read_ten_lu() {
    let mut x = sine(1000.0, db(-20.0), 20.0, 0.0);
    x.extend(sine(1000.0, db(-30.0), 20.0, 0.0));
    let lra = IntegratedLoudness::range(&x, Some(&x), SR);
    assert!((lra - 10.0).abs() <= 1.0, "lra {lra}");
}

#[test]
fn silence_and_short_render_have_no_range() {
    let quiet = vec![0.0f32; (5.0 * SR) as usize];
    assert_eq!(IntegratedLoudness::range(&quiet, None, SR), 0.0);
    let short = sine(1000.0, 0.5, 2.0, 0.0);
    assert_eq!(IntegratedLoudness::range(&short, None, SR), 0.0);
}

#[test]
fn quarter_rate_sine_at_45_degrees_peaks_between_samples() {
    let x = sine(SR / 4.0, 1.0, 1.0, PI / 4.0);
    let sample_peak = 20.0 * (x.iter().map(|v| v.abs()).fold(0.0f32, f32::max) as f64).log10();
    let tp = TruePeak::dbtp(&x, None);
    assert!(
        (tp - sample_peak - 3.0).abs() <= 0.3,
        "tp {tp} sample peak {sample_peak}"
    );
}

#[test]
fn full_scale_997_hz_reads_zero_dbtp() {
    let x = sine(997.0, 1.0, 1.0, 0.0);
    let tp = TruePeak::dbtp(&x, Some(&x));
    assert!(tp.abs() <= 0.2, "tp {tp}");
}

#[test]
fn true_peak_takes_the_louder_channel() {
    let l = sine(997.0, db(-12.0), 0.5, 0.0);
    let r = sine(997.0, db(-6.0), 0.5, 0.0);
    assert!((TruePeak::dbtp(&l, Some(&r)) + 6.0).abs() <= 0.2);
    assert_eq!(TruePeak::dbtp(&[0.0, 0.0, 0.0], None), -200.0);
}

#[test]
fn detector_streams_without_growing_and_resets() {
    let mut d = TruePeakDetector::new();
    for v in sine(997.0, 0.5, 0.1, 0.0) {
        d.process(v as f64);
    }
    assert!((20.0 * d.peak.log10() - 20.0 * 0.5f64.log10()).abs() <= 0.2);
    d.reset();
    assert_eq!(d.peak, 0.0);
    assert_eq!(d.process(0.0), 0.0);
}

/// BS.1770: a 997 Hz sine at −20 dBFS on both channels reads −20 LUFS (the K-weighting is
/// +0.69 dB there, which is the −0.691 of the loudness formula); mono counts twice.
#[test]
fn integrated_loudness_of_a_reference_sine() {
    let x = sine(997.0, db(-20.0), 5.0, 0.0);
    let stereo = IntegratedLoudness::lufs(&x, &x, SR);
    assert!((stereo - -20.0).abs() < 0.3, "stereo {stereo}");
    let mono = IntegratedLoudness::integrated(&x, None, SR).unwrap();
    assert!((mono - stereo).abs() < 1e-9, "mono {mono}");
    let one = IntegratedLoudness::lufs(&x, &vec![0.0f32; x.len()], SR);
    assert!((one - -23.0).abs() < 0.35, "one channel {one}");
    let silent = vec![0.0f32; 4800];
    assert_eq!(IntegratedLoudness::lufs(&silent, &silent, SR), -200.0);
    let _ = (REFERENCE_LUFS, KWeighting::new(SR));
}

// MARK: spectrum

const SSR: f64 = 44100.0;

/// Deterministic white noise, uniform in ±0.5.
fn white(count: usize, seed: u64) -> Vec<f32> {
    let mut x = seed;
    (0..count)
        .map(|_| {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            ((z >> 11) as f64 / (1u64 << 53) as f64 - 0.5) as f32
        })
        .collect()
}

/// Paul Kellett's refined pink filter over white noise.
fn pink(count: usize, seed: u64) -> Vec<f32> {
    let mut b = [0.0f64; 7];
    white(count, seed)
        .into_iter()
        .map(|f| {
            let w = f as f64;
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.1538520;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let y = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
            b[6] = w * 0.115926;
            (y * 0.11) as f32
        })
        .collect()
}

fn seconds() -> usize {
    (10.0 * SSR) as usize
}

#[test]
fn white_noise_is_flat() {
    let x = white(seconds(), 1);
    let s = Spectrum::new(&x, &x, SSR);
    assert!(
        s.slope_db_per_oct.abs() < 0.3,
        "slope {}",
        s.slope_db_per_oct
    );
    assert!(s.hf_bump_db.abs() < 1.0, "bump {}", s.hf_bump_db);
}

#[test]
fn pink_noise_falls_three_db_an_octave() {
    let x = pink(seconds(), 2);
    let s = Spectrum::new(&x, &x, SSR);
    assert!(
        (s.slope_db_per_oct + 3.0).abs() < 0.4,
        "slope {}",
        s.slope_db_per_oct
    );
    assert!(s.hf_bump_db.abs() < 1.5, "bump {}", s.hf_bump_db);
}

#[test]
fn sine_is_loudest_in_its_third_octave() {
    let x: Vec<f32> = (0..seconds())
        .map(|i| (0.5 * (2.0 * PI * 1000.0 * i as f64 / SSR).sin()) as f32)
        .collect();
    let s = Spectrum::new(&x, &x, SSR);
    let mut loudest = 0;
    for i in 0..s.ltas_db.len() {
        if s.ltas_db[loudest] < s.ltas_db[i] {
            loudest = i;
        }
    }
    assert_eq!(BANDS_HZ[loudest], 1000.0);
}

#[test]
fn boost_at_14_khz_is_a_bump() {
    let mut x = pink(seconds(), 2);
    // band-passed white noise at 14 kHz, well over the pink there
    let mut hiss: Vec<f64> = white(seconds(), 9).into_iter().map(|v| v as f64).collect();
    let bp = biquad("bandpass", 14000.0, 2.0, SSR);
    for _ in 0..2 {
        let (mut z1, mut z2) = (0.0, 0.0);
        for h in hiss.iter_mut() {
            let v = *h;
            let y = bp[0] * v + z1;
            z1 = bp[1] * v - bp[3] * y + z2;
            z2 = bp[2] * v - bp[4] * y;
            *h = y;
        }
    }
    for i in 0..x.len() {
        x[i] += hiss[i] as f32;
    }
    let s = Spectrum::new(&x, &x, SSR);
    assert!(s.hf_bump_db > 6.0, "bump {}", s.hf_bump_db);
    assert!(
        (s.slope_db_per_oct + 3.0).abs() < 1.0,
        "slope {}",
        s.slope_db_per_oct
    );
}

#[test]
fn mono_folds_without_loss_and_inverted_loses_everything() {
    let x = pink(seconds(), 2);
    let mono = Spectrum::new(&x, &x, SSR);
    assert!(
        mono.mono_fold_loss_db.iter().all(|v| v.abs() < 0.01),
        "{:?}",
        mono.mono_fold_loss_db
    );
    assert!(mono.mono_fold_db.abs() < 0.01);
    let neg: Vec<f32> = x.iter().map(|v| -v).collect();
    let inverted = Spectrum::new(&x, &neg, SSR);
    assert!(
        inverted.mono_fold_loss_db.iter().all(|v| *v < -60.0),
        "{:?}",
        inverted.mono_fold_loss_db
    );
    assert!(inverted.mono_fold_db < -60.0);
    // uncorrelated channels fold 3 dB down
    let wide = Spectrum::new(&x, &pink(seconds(), 5), SSR);
    assert!(
        (wide.mono_fold_db + 3.0).abs() < 0.5,
        "{}",
        wide.mono_fold_db
    );
}

/// A long render costs at most `MAX_SEGMENTS` segments, spread over the whole of it.
#[test]
fn long_render_is_decimated() {
    let x = pink((60.0 * SSR) as usize, 2);
    let s = Spectrum::new(&x, &x, SSR);
    assert!(
        (s.slope_db_per_oct + 3.0).abs() < 0.4,
        "slope {}",
        s.slope_db_per_oct
    );
}

// MARK: pitch measure

fn sine_rms(hz: f64, rms_db: f64, seconds: f64, sr: f64) -> Vec<f32> {
    let a = 10f64.powf(rms_db / 20.0) * 2f64.sqrt();
    (0..(seconds * sr) as usize)
        .map(|i| (a * (2.0 * PI * hz * i as f64 / sr).sin()) as f32)
        .collect()
}

#[test]
fn a3_sine_reads_on_note_57() {
    let sr = 48_000.0;
    let x = sine_rms(220.0, -12.0, 1.0, sr);
    let r = pitch_measure::error(&x, 57, sr);
    assert!(r.cents.abs() < 1.0, "{}", r.cents);
    assert_eq!(r.octave, 0);
    assert!((r.hz - 220.0).abs() < 0.2, "{}", r.hz);
    assert!((r.level - 0.355).abs() < 0.02, "{}", r.level);
    let f = pitch_measure::fundamental(&x, sr);
    assert!(f.map_or(false, |f| (f - 220.0).abs() < 0.5), "{f:?}");
}

#[test]
fn twenty_cents_sharp_reads_twenty() {
    let sr = 48_000.0;
    let x = sine_rms(220.0 * 2f64.powf(20.0 / 1200.0), -12.0, 1.0, sr);
    let r = pitch_measure::error(&x, 57, sr);
    assert!((r.cents - 20.0).abs() <= 2.0, "{}", r.cents);
}

#[test]
fn silence_reads_level_zero() {
    let sr = 48_000.0;
    let x = vec![0.0f32; sr as usize];
    assert_eq!(pitch_measure::error(&x, 57, sr).level, 0.0);
    assert!(pitch_measure::fundamental(&x, sr).is_none());
}

// MARK: beat tracker

const BSR: f64 = 22_050.0;

/// A click track: a 2 kHz blip on every beat, and (with `accent`) a 60 Hz thump on every
/// fourth one from beat `accent_phase`; the first beat at `lead` seconds.
fn clicks(
    bpm: f64,
    seconds: f64,
    sr: f64,
    accent: bool,
    accent_phase: usize,
    lead: f64,
) -> Vec<f32> {
    let mut x = vec![0.0f32; (seconds * sr) as usize];
    let period = 60.0 / bpm * sr;
    let mut k = 0usize;
    loop {
        let start = (lead * sr + k as f64 * period).round() as usize;
        if start >= x.len() {
            break;
        }
        let len = (0.03 * sr) as usize;
        for i in 0..len.min(x.len() - start) {
            let t = i as f64 / sr;
            let mut v = 0.4 * (2.0 * PI * 2000.0 * t).sin() * (-t / 0.005).exp();
            if accent && k % 4 == accent_phase {
                v += 0.6 * (2.0 * PI * 60.0 * t).sin() * (-t / 0.02).exp();
            }
            x[start + i] = v as f32;
        }
        k += 1;
    }
    x
}

/// The share of the true beats in `range` (seconds) that have a tracked beat within `tol`.
fn hit_rate(tracked: &[f64], bpm: f64, range: (f64, f64), tol: f64) -> f64 {
    let period = 60.0 / bpm;
    let (mut hits, mut total) = (0, 0);
    let mut k = 0;
    while k as f64 * period <= range.1 {
        let t = k as f64 * period;
        k += 1;
        if t < range.0 {
            continue;
        }
        total += 1;
        if tracked.iter().any(|b| (b - t).abs() <= tol) {
            hits += 1;
        }
    }
    if total > 0 {
        hits as f64 / total as f64
    } else {
        0.0
    }
}

fn noise(seconds: f64, sr: f64) -> Vec<f32> {
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..(seconds * sr) as usize)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((((s >> 11) as i64) % 2_000_000) as f32 / 2_000_000.0) * 0.3
        })
        .collect()
}

#[test]
fn a_click_track_reads_124() {
    let x = clicks(124.0, 30.0, BSR, false, 0, 0.0);
    let b = BeatTracker::new(&x, BSR);
    assert!((b.bpm - 124.0).abs() <= 0.5, "{} BPM", b.bpm);
    let rate = hit_rate(&b.beats, 124.0, (1.0, 29.0), 0.030);
    assert!(rate >= 0.99, "{rate} of beats within 30 ms");
    assert!(b.confidence > 0.5, "confidence {}", b.confidence);
}

/// The prior keeps 70–180 BPM clicks at their own tempo, not half or double.
#[test]
fn clicks_read_their_own_tempo() {
    for bpm in [72.0, 95.0, 140.0, 174.0] {
        let b = BeatTracker::new(&clicks(bpm, 30.0, BSR, false, 0, 0.0), BSR);
        assert!((b.bpm - bpm).abs() <= 0.5, "{bpm}: {} BPM", b.bpm);
    }
}

#[test]
fn downbeats_follow_the_low_band() {
    for phase in 0..4usize {
        let x = clicks(124.0, 30.0, BSR, true, phase, 0.25);
        let b = BeatTracker::new(&x, BSR);
        let period = 60.0 / 124.0;
        let truth = 0.25 + phase as f64 * period;
        let first = b
            .beats
            .iter()
            .position(|t| (t - truth).abs() < 0.03)
            .expect("a beat on the accent");
        assert_eq!(b.downbeat_phase, first % 4);
        // every downbeat sits on an accented beat
        assert!(!b.downbeats.is_empty());
        for d in &b.downbeats {
            let bars = (d - truth) / (4.0 * period);
            assert!(
                (bars - bars.round()).abs() * 4.0 * period < 0.03,
                "phase {phase}: downbeat at {d}"
            );
        }
    }
}

#[test]
fn noise_has_low_confidence() {
    let b = BeatTracker::new(&noise(20.0, BSR), BSR);
    assert!(b.confidence < 0.3, "confidence {}", b.confidence);
}

#[test]
fn silence_and_short_input_read_nothing() {
    let b = BeatTracker::new(&vec![0.0f32; 1000], BSR);
    assert!(b.bpm == 0.0 && b.beats.is_empty());
}

// MARK: section segmenter

/// A made-up song on a known grid: each section a different chord, level and click pattern.
fn sections(bars: &[usize], bpm: f64, sr: f64) -> Vec<f32> {
    let bar = 4.0 * 60.0 / bpm;
    let total = (bars.iter().sum::<usize>() as f64 * bar * sr) as usize;
    let mut x = vec![0.0f32; total];
    let roots = [110.0, 164.8, 130.8, 196.0, 146.8, 123.5];
    let levels = [0.15, 0.3, 0.1, 0.25, 0.2, 0.35];
    let mut start = 0.0;
    for (s, n) in bars.iter().enumerate() {
        let root = roots[s % roots.len()];
        let level = levels[s % levels.len()];
        let a = (start * sr) as usize;
        let z = total.min(((start + *n as f64 * bar) * sr) as usize);
        for i in a..z {
            let t = i as f64 / sr;
            let mut v = 0.0;
            for (k, ratio) in [1.0, 1.25, 1.5].iter().enumerate() {
                v += (2.0 * PI * root * ratio * t).sin() / (k + 1) as f64;
            }
            v *= level;
            // a click each beat (odd sections: each half beat, brighter)
            let sub = if s % 2 == 0 { 60.0 / bpm } else { 30.0 / bpm };
            let since = (t - start) % sub;
            let hz = if s % 2 == 0 { 2000.0 } else { 5000.0 };
            v += 0.3 * (2.0 * PI * hz * since).sin() * (-since / 0.004).exp();
            x[i] = v as f32;
        }
        start += *n as f64 * bar;
    }
    x
}

/// The share of `truth` (seconds) with a found boundary within `tol`, and the spurious ones.
fn score(found: &[f64], truth: &[f64], tol: f64) -> (f64, usize) {
    let hits = truth
        .iter()
        .filter(|t| found.iter().any(|f| (f - **t).abs() <= tol))
        .count();
    let spurious = found
        .iter()
        .filter(|f| !truth.iter().any(|t| (*t - **f).abs() <= tol))
        .count();
    (
        if truth.is_empty() {
            1.0
        } else {
            hits as f64 / truth.len() as f64
        },
        spurious,
    )
}

/// Sections of 8, 4, 8, 4 and 8 bars, each its own chord, level and clicks, on the true grid:
/// every boundary on its bar, nothing else.
#[test]
fn distinct_sections_split_on_their_bars() {
    let sr = 22_050.0;
    let bars = [8usize, 4, 8, 4, 8];
    let x = sections(&bars, 120.0, sr);
    let grid: Vec<f64> = (0..bars.iter().sum::<usize>())
        .map(|i| i as f64 * 2.0)
        .collect();
    let s = SectionSegmenter::with_downbeats(&x, sr, &grid, 4);
    assert_eq!(s.bar_starts.len(), 32);
    assert_eq!(
        s.boundaries,
        vec![8, 12, 20, 24],
        "{:?}, novelty {:?}",
        s.boundaries,
        s.novelty
    );
    assert_eq!(s.section_bars(), bars.to_vec());
    assert_eq!(s.section_bars().iter().sum::<usize>(), s.bar_starts.len());
    assert_eq!(s.boundary_times(), vec![16.0, 24.0, 40.0, 48.0]);
}

/// The same song through `BeatTracker`'s downbeats: boundaries within a bar of the truth.
#[test]
fn tracked_bars_find_the_same_boundaries() {
    let sr = 22_050.0;
    let x = sections(&[8, 4, 8, 4, 8], 120.0, sr);
    let s = SectionSegmenter::from_mono(&x, sr, 4);
    assert!((s.bpm - 120.0).abs() <= 1.0, "{} BPM", s.bpm);
    let (recall, spurious) = score(&s.boundary_times(), &[16.0, 24.0, 40.0, 48.0], 2.0);
    assert!(recall >= 0.75, "{:?}", s.boundary_times());
    assert!(spurious <= 1, "{:?}", s.boundary_times());
}

/// One unchanging loop has no sections.
#[test]
fn a_steady_loop_has_no_boundaries() {
    let sr = 22_050.0;
    let x = sections(&[32], 120.0, sr);
    let grid: Vec<f64> = (0..32).map(|i| i as f64 * 2.0).collect();
    let s = SectionSegmenter::with_downbeats(&x, sr, &grid, 4);
    assert!(s.boundaries.is_empty(), "{:?}", s.boundaries);
    assert_eq!(s.section_bars(), vec![32]);
}

#[test]
fn segmenter_silence_and_short_input_find_nothing() {
    let s = SectionSegmenter::from_mono(&vec![0.0f32; 1000], 22_050.0, 4);
    assert!(s.bar_starts.is_empty() && s.boundaries.is_empty() && s.section_bars().is_empty());
}

/// The kernel is an option and is reported.
#[test]
fn kernel_is_an_option_and_reported() {
    let sr = 22_050.0;
    let bars = [8usize, 8, 8, 8];
    let x = sections(&bars, 120.0, sr);
    let grid: Vec<f64> = (0..32).map(|i| i as f64 * 2.0).collect();
    let s = SectionSegmenter::with_downbeats(&x, sr, &grid, 8);
    assert_eq!(s.kernel, 8);
    assert_eq!(s.boundaries, vec![8, 16, 24], "{:?}", s.boundaries);
    assert_eq!(s.section_bars(), bars.to_vec());
}

/// Foote novelty on a two-block similarity matrix peaks at the block edge.
#[test]
fn novelty_peaks_at_a_block_edge() {
    let n = 16;
    let sim: Vec<Vec<f64>> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| if (i < 8) == (j < 8) { 1.0 } else { -1.0 })
                .collect()
        })
        .collect();
    let nov = SectionSegmenter::novelty(&sim, 4);
    let mut top = 0;
    for i in 0..nov.len() {
        if nov[top] < nov[i] {
            top = i;
        }
    }
    assert_eq!(top, 8, "{nov:?}");
    assert_eq!(SectionSegmenter::peaks(&nov, 2), vec![8]);
}

// MARK: targets table (the data contract)

#[test]
fn loudness_is_the_calibrated_constant() {
    let t = MixTargets::shared();
    assert_eq!(t.probe_offset_db(), -0.4);
    assert_eq!(REFERENCE_LUFS, -12.0);
    let (lo, hi) = t.gain_range();
    assert_eq!((lo, hi), (0.25, 8.0));
}

#[test]
fn balance_is_the_rule_of_thumb() {
    let old: std::collections::BTreeMap<String, f64> = [
        ("kick", 0.0),
        ("bass", -3.0),
        ("lead", -4.0),
        ("snare", -6.0),
        ("pad", -7.0),
        ("arp", -8.0),
        ("hats", -10.0),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), *v))
    .collect();
    assert_eq!(MixBalance::target_db(), old);
    let binding = MixBalance::target_db();
    let keys: std::collections::BTreeSet<&str> = binding.keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, MixBalance::PARTS.iter().copied().collect());
    assert_eq!(MixBalance::max_trim_db(), 6.0);
    assert_eq!(MixTargets::shared().balance_db(Some("dance")), old);
}

#[test]
fn rooms_are_the_rule_of_thumb() {
    let old = |name: &str| -> SpaceSpec {
        match name {
            "ambient" => SpaceSpec::new(4.0, 0.27, 5000.0),
            "trance" | "synthwave" => SpaceSpec::new(2.4, 0.45, 7500.0),
            "edm" => SpaceSpec::new(2.0, 0.38, 7000.0),
            "techno" | "house" | "garage" => SpaceSpec::new(1.3, 0.33, 6000.0),
            "funk" | "disco" => SpaceSpec::new(1.0, 0.22, 5500.0),
            "lofi" => SpaceSpec::new(1.8, 0.16, 3500.0),
            "jazz" | "bossa" => SpaceSpec::new(1.6, 0.19, 4500.0),
            "dnb" | "dubstep" | "trap" => SpaceSpec::new(1.6, 0.33, 6500.0),
            _ => SpaceSpec::new(1.5, 0.3, 6000.0),
        }
    };
    assert_eq!(SpaceSpec::room(), old("none"));
    for g in BeatsTables::shared()
        .genre_ids
        .iter()
        .map(|s| s.as_str())
        .chain(["wrapped", "none"])
    {
        assert_eq!(SpaceSpec::for_genre(g, false), old(g), "{g}");
    }
}

#[test]
fn stereo_placement_is_the_rule_of_thumb() {
    let old = |kind: &str, group: &str| -> (f64, f64) {
        match kind {
            "hat" | "openhat" => (0.3, 0.0),
            "crash" => (-0.3, 0.0),
            "tonal" => match group {
                "harmony" => (0.0, 0.5),
                "arp" => (0.0, 0.45),
                _ => (0.0, 0.0),
            },
            _ => (0.0, 0.0),
        }
    };
    for k in [
        "kick", "snare", "clap", "hat", "openhat", "crash", "tom", "tonal",
    ] {
        for g in [
            "bass", "harmony", "pad", "melody", "arp", "kick", "hihat", "",
        ] {
            assert_eq!(stereo_placement(k, g), old(k, g), "{k} {g}");
        }
    }
}

#[test]
fn bands_are_there_and_empty() {
    let t = MixTargets::shared();
    for m in MixTargets::METRICS {
        let fams: std::collections::BTreeSet<String> = t.band_families(m).into_iter().collect();
        assert_eq!(
            fams,
            FAMILY_NAMES.iter().map(|s| s.to_string()).collect(),
            "{m}"
        );
        for f in FAMILY_NAMES {
            assert!(t.band(m, f).is_none(), "{m}.{f}");
        }
    }
}

// MARK: space, echo, voice design (block level)

#[test]
fn rooms_by_genre() {
    let s = |g: &str| SpaceSpec::for_genre(g, false);
    assert!(s("ambient").seconds > s("trance").seconds);
    assert!(s("trance").seconds > s("techno").seconds);
    assert!(s("techno").seconds > s("funk").seconds);
    assert!(s("lofi").damping < s("trance").damping);
    assert_eq!(SpaceSpec::send("kick", "drums"), 0.0);
    assert_eq!(SpaceSpec::send("tonal", "bass"), 0.0);
    assert!(SpaceSpec::send("tonal", "harmony") > SpaceSpec::send("snare", "drums"));
    assert!(SpaceSpec::for_genre("trance", true).shimmer >= 0.0);
    assert!(SpaceSpec::shimmers("tonal", "harmony") && !SpaceSpec::shimmers("tonal", "melody"));
}

/// The pad rings on in the room: a short burst in, a tail that falls at about the room's
/// rate (60 dB per `seconds`).
#[test]
fn a_burst_leaves_a_tail_that_decays_at_the_rooms_rate() {
    let sr = 48_000.0;
    let mut rev = FdnReverb::new(sr);
    rev.tune(Some(&SpaceSpec::new(2.0, 0.4, 7000.0)));
    assert!(rev.out_gain() > 0.0);
    let n = (4.0 * sr) as usize;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    let src = white(2400, 3);
    for i in 0..n {
        let x = if i < src.len() { src[i] as f64 } else { 0.0 };
        let (a, b) = rev.process(x);
        l[i] = a as f32;
        r[i] = b as f32;
    }
    let mid: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a + b) / 2.0).collect();
    let rms = |x: &[f32], a: f64, b: f64| -> f64 {
        let s = &x[(a * sr) as usize..(b * sr) as usize];
        10.0 * (s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / s.len() as f64)
            .max(1e-20)
            .log10()
    };
    let early = rms(&mid, 1.2, 1.4);
    let late = rms(&mid, 2.2, 2.4);
    assert!(early > -90.0, "tail at 1.2 s: {early} dB");
    let fall = early - late;
    assert!(
        fall > 20.0 && fall < 40.0,
        "fell {fall} dB in a second (a 2 s room falls 30)"
    );
    // the tail is wide (its side close to its mid)
    let side: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a - b) / 2.0).collect();
    let width = rms(&side, 1.2, 2.0) - rms(&mid, 1.2, 2.0);
    assert!(width > -8.0, "tail side/mid {width} dB");
    // a room of no wet is silent
    rev.tune(Some(&SpaceSpec::new(2.0, 0.0, 7000.0)));
    assert_eq!(rev.out_gain(), 0.0);
}

fn at(x: &[f32], t: f64) -> f64 {
    let a = (t * SR) as usize;
    let s = &x[a..a + (0.06 * SR) as usize];
    10.0 * (s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / s.len() as f64)
        .max(1e-20)
        .log10()
}

fn echo_of(spec: EchoSpec, bpm: f64, seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let mut e = PingPong::new(SR);
    e.tune(Some(spec), bpm);
    let n = (seconds * SR) as usize;
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    // one short note: 5 ms of a 1 kHz burst
    for i in 0..n {
        let x = if i < 240 {
            0.5 * (2.0 * PI * 1000.0 * i as f64 / SR).sin()
        } else {
            0.0
        };
        let (a, b) = e.process(x);
        l[i] = a as f32;
        r[i] = b as f32;
    }
    (l, r)
}

/// Dotted eighths at 120 BPM are 375 ms: the first repeat comes back on the left, the second
/// on the right, each quieter than the last.
#[test]
fn repeats_ping_pong_on_the_beat_grid() {
    let spec = EchoSpec {
        steps: 3.0,
        feedback: 0.42,
        wet: 0.32,
        damping: 5000.0,
    };
    let (l, r) = echo_of(spec, 120.0, 2.0);
    assert!(at(&l, 0.1) < -120.0, "nothing before the first repeat");
    let first = (at(&l, 0.375), at(&r, 0.375));
    let second = (at(&l, 0.75), at(&r, 0.75));
    let third = (at(&l, 1.125), at(&r, 1.125));
    assert!(
        first.0 > -60.0 && first.0 > first.1 + 10.0,
        "first repeat L {} R {}",
        first.0,
        first.1
    );
    assert!(
        second.1 > second.0 + 10.0,
        "second repeat L {} R {}",
        second.0,
        second.1
    );
    assert!(
        third.0 > third.1 + 10.0,
        "third repeat L {} R {}",
        third.0,
        third.1
    );
    assert!(
        second.1 < first.0 - 3.0 && third.0 < second.1 - 3.0,
        "repeats fall"
    );
    // between the repeats, quiet
    assert!(at(&l, 0.55) < first.0 - 20.0);
}

/// The delay follows the tempo: at 140 BPM a dotted eighth is 321 ms.
#[test]
fn the_delay_follows_the_tempo() {
    let spec = EchoSpec {
        steps: 3.0,
        feedback: 0.42,
        wet: 0.32,
        damping: 5000.0,
    };
    let (l, _) = echo_of(spec, 140.0, 2.0);
    let t = 3.0 * 60.0 / (140.0 * 4.0);
    assert!(
        at(&l, t) > -60.0 && at(&l, t - 0.07) < -120.0,
        "the repeat lands at {t} s"
    );
}

#[test]
fn only_leads_echo() {
    assert!(EchoSpec::for_genre("jazz").is_none() && EchoSpec::for_genre("techno").is_none());
    assert!(
        EchoSpec::for_genre("ambient").unwrap().steps
            > EchoSpec::for_genre("trance").unwrap().steps
    );
    assert_eq!(EchoSpec::send("tonal", "melody"), 0.4);
    assert_eq!(EchoSpec::send("tonal", "arp"), 0.22);
    for g in ["bass", "harmony"] {
        assert_eq!(EchoSpec::send("tonal", g), 0.0, "{g}");
    }
    assert_eq!(EchoSpec::send("snare", "drums"), 0.0);
}

/// A delay a hair over a whole sample put the read at exactly the buffer's length when the
/// write head reached that sample. It crashed the live app on the audio thread.
#[test]
fn a_delay_a_hair_over_a_whole_sample_reads_inside_the_line() {
    let mut echo = PingPong::new(48_000.0);
    echo.tune(EchoSpec::for_genre("trance"), 120.0f64.next_down());
    let mut peak = 0.0f64;
    for i in 0..60_000 {
        let (l, r) = echo.process(if i < 480 { 0.5 } else { 0.0 });
        peak = peak.max(l.abs()).max(r.abs());
    }
    assert!(peak > 0.0 && peak.is_finite());
}

#[test]
fn voice_design_and_tuning_data() {
    // `.kickTune`: the root or fifth nearest 50 Hz, between about 37 and 73 Hz
    for root in 0..12 {
        let hz = KickTuning::hz(Some(48 + root)).unwrap();
        assert!(hz > 35.0 && hz < 75.0, "{root}: {hz}");
    }
    assert!(KickTuning::hz(None).is_none());
    // `.duck`: half under a four-on-the-floor kick, nothing outside dance
    assert_eq!(duck_depth("techno"), 0.5);
    assert_eq!(duck_depth("synthwave"), 0.4);
    assert_eq!(duck_depth("jazz"), 0.0);
    assert_eq!(duck_depth("nope"), 0.0);
    // `.hfTilt`
    let t = HfTilt::new(9000.0, None);
    assert!(t.lowpass_hz("hat") == Some(9000.0) && t.lowpass_hz("openhat") == Some(9000.0));
    assert_eq!(t.lowpass_hz("crash"), Some(9000.0));
    for kind in ["kick", "snare", "clap", "tonal"] {
        assert!(t.lowpass_hz(kind).is_none());
    }
    for genre in ["techno", "jazz", "lofi", "funk", "edm", "ambient"] {
        let tilt = HfTilt::for_genre(genre).expect(genre);
        assert!(tilt.hat_hz > 6000.0 && tilt.hat_hz < 16000.0, "{genre}");
    }
    // sampled instruments' width by category
    assert_eq!(sampled_width(Some("keys.piano"), "harmony").width, 1.2);
    assert_eq!(sampled_width(Some("bass.electric"), "melody").width, 0.0);
    assert_eq!(sampled_width(None, "bass").width, 0.0);
    assert_eq!(sampled_width(Some("synth.lead"), "melody").width, 0.6);
    assert_eq!(sampled_width(Some("guitar.acoustic"), "melody").width, 0.8);
    assert_eq!(sampled_width(Some("brass"), "melody").width, 1.0);
}
