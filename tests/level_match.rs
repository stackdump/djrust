//! One loudness (`LevelMatchTests.swift`, the sample-deck half): a sample, a loop or a take
//! lands at `LevelMatch::REFERENCE_LUFS` at unity knobs. Each test names the Swift test it
//! ports ("(a)" in PORTING.md's parity rule); `SampleLevel`'s numbers are held to Swift's in
//! `tests/loops_golden.rs`.
//!
//! Not here: the net-deck tests (`aNetTrackPlaysAtTheReference`, the sampled-track and
//! cold-cache ones: whole songs through a deck, the loudness gain is `musicality::loudness`'s
//! and held by its own tests), the engine ones (a take of a net deck, `autoGain` as a deck
//! param: `djhost/tests/deck_level.rs` and `session.rs`) and the `DJ_LEVEL_SURVEY`
//! measurements, which are printouts, not checks.

use djrust::deck::level_match::{LevelMatch, LoudnessMeter, SampleLevel};
use djrust::deck::turntable::SampleBuffer;
use djrust::musicality::loudness::IntegratedLoudness;

const SR: f64 = 48_000.0;

/// `seconds` of sines (100 Hz, 1 kHz, 5 kHz by default: energy across the spectrum, a "full
/// mix" to `SampleLevel`), each at `amplitude`, plus a spike at the middle if given (`tones`).
fn tones(amplitude: f32, seconds: f64, hz: &[f64], spike: Option<f32>) -> SampleBuffer {
    let n = (seconds * SR) as usize;
    let mut l = vec![0f32; n];
    for (i, v) in l.iter_mut().enumerate() {
        let mut s = 0.0f64;
        for f in hz {
            s += (2.0 * std::f64::consts::PI * f * i as f64 / SR).sin();
        }
        *v = amplitude * s as f32;
    }
    if let Some(sp) = spike {
        l[n / 2] = sp;
    }
    SampleBuffer::new(l.clone(), Some(l), SR)
}

fn full(amplitude: f32) -> SampleBuffer {
    tones(amplitude, 4.0, &[100.0, 1000.0, 5000.0], None)
}

// LevelMatchTests.aSineReadsWhereBS1770SaysItShould
#[test]
fn a_sine_reads_where_bs1770_says_it_should() {
    let b = tones(0.1, 4.0, &[997.0], None); // -20 dBFS peak = -23 dBFS RMS
    let l = IntegratedLoudness::integrated_count(&b.left, Some(b.right()), b.length(), SR, 0)
        .expect("a sine has a loudness");
    assert!((l - (-23.01 + 3.01 - 0.691 + 0.691)).abs() < 0.3, "{l}");
    // silence is None, not -inf
    let quiet = vec![0f32; 48_000];
    assert_eq!(
        IntegratedLoudness::integrated(&quiet, Some(&quiet), SR),
        None
    );
}

// LevelMatchTests.aSampleLoadsAtTheReference (the policy half: the deck plays the sample at the
// auto-gain, here the same gain applied to the samples)
#[test]
fn a_sample_loads_at_the_reference() {
    let quiet = full(0.01); // about -30 LUFS, peak -30 dBFS: room to lift it
    let level = SampleLevel::measure(&quiet);
    assert!(!level.partial());
    assert!(level.auto_gain_db() > 0.0);
    let g = 10f64.powf(level.auto_gain_db() / 20.0) as f32;
    let (l, r): (Vec<f32>, Vec<f32>) = (
        quiet.left.iter().map(|v| v * g).collect(),
        quiet.right().iter().map(|v| v * g).collect(),
    );
    let measured = IntegratedLoudness::integrated(&l, Some(&r), SR).unwrap();
    let expected = LevelMatch::REFERENCE_LUFS.min(level.lufs.unwrap() + LevelMatch::MAX_GAIN_DB);
    assert!((measured - expected).abs() < 0.5, "{measured} LUFS");
    let peak = l.iter().chain(&r).fold(0f32, |p, v| p.max(v.abs()));
    let peak_db = 20.0 * (peak as f64).log10();
    assert!(
        peak_db <= LevelMatch::SAMPLE_PEAK_CEILING_DB + 0.01,
        "{peak_db}"
    );
}

// LevelMatchTests.theAutoGainIsClampedAndCappedByThePeak
#[test]
fn the_auto_gain_is_clamped_and_capped_by_the_peak() {
    // near silence: at most +12 dB
    let silent = SampleLevel::measure(&full(0.0005));
    assert_eq!(silent.auto_gain_db(), LevelMatch::MAX_GAIN_DB);
    // very loud: at most -12 dB
    let hot = SampleLevel::measure(&tones(0.33, 4.0, &[100.0, 1000.0, 5000.0], None));
    assert!(hot.auto_gain_db() >= -LevelMatch::MAX_GAIN_DB && hot.auto_gain_db() < 0.0);
    // quiet but with a peak near full scale: not lifted past -1 dBFS
    let spiky = SampleLevel::measure(&tones(0.01, 4.0, &[100.0, 1000.0, 5000.0], Some(0.95)));
    assert!(
        spiky.auto_gain_db()
            <= 0.0f64.max(LevelMatch::SAMPLE_PEAK_CEILING_DB - spiky.peak_db) + 0.01
    );
    assert!(spiky.auto_gain_db() < 1.0);
    // digital silence: no gain
    let zero = SampleBuffer::new(vec![0.0; 48_000], None, SR);
    let z = SampleLevel::measure(&zero);
    assert!(z.lufs.is_none() && z.auto_gain_db() == 0.0);
}

// LevelMatchTests.partialLoopsSitUnderAFullMix
#[test]
fn partial_loops_sit_under_a_full_mix() {
    let bass = SampleLevel::measure(&tones(0.01, 4.0, &[60.0], None));
    assert!(bass.partial() && bass.low_share > 0.9);
    assert_eq!(
        bass.target_lufs(),
        LevelMatch::REFERENCE_LUFS + LevelMatch::PARTIAL_OFFSET_DB
    );
    let hats = SampleLevel::measure(&tones(0.01, 4.0, &[7000.0, 9000.0], None));
    assert!(hats.partial() && hats.high_share > 0.5);
    let f = SampleLevel::measure(&full(0.01));
    assert!(!f.partial() && f.target_lufs() == LevelMatch::REFERENCE_LUFS);
}

// LevelMatchTests.theAutoGainIsAPureFunctionOfTheAudio
#[test]
fn the_auto_gain_is_a_pure_function_of_the_audio() {
    let (a, b) = (full(0.02), full(0.02));
    assert_eq!(SampleLevel::measure(&a), SampleLevel::measure(&b));
    // a mono buffer reads as the same audio on both channels (counted twice)
    let mono = SampleBuffer::new(a.left.clone(), None, SR);
    let (m, s) = (SampleLevel::measure(&mono), SampleLevel::measure(&a));
    assert_eq!(m.lufs, s.lufs);
    assert_eq!((m.low_share, m.high_share), (s.low_share, s.high_share));
}

// LevelMatch constants and the genre target (LevelMatch.targetLUFS(genre:), every family today)
#[test]
fn the_reference_and_its_limits() {
    assert_eq!(LevelMatch::REFERENCE_LUFS, -12.0);
    assert_eq!(LevelMatch::PARTIAL_OFFSET_DB, -4.0);
    assert_eq!(LevelMatch::MAX_GAIN_DB, 12.0);
    assert_eq!(LevelMatch::SAMPLE_PEAK_CEILING_DB, -1.0);
    assert_eq!(LevelMatch::MAX_SPREAD_LU, 2.0);
    assert_eq!(LevelMatch::CROSSFADE_TOLERANCE_LU, 1.0);
    assert_eq!(LevelMatch::target_lufs(Some("techno")), -12.0);
    assert_eq!(LevelMatch::target_lufs(None), -12.0);
}

// LoudnessMeter (the deck's short-term and momentary readings): a steady 997 Hz sine at -20
// dBFS reads where BS.1770 says, silence reads -200, and nothing is published before the first
// 100 ms
#[test]
fn the_meter_reads_a_sine_and_silence() {
    let mut m = LoudnessMeter::new(SR);
    assert_eq!(m.short_term_lufs(), -200.0);
    let b = tones(0.1, 4.0, &[997.0], None);
    m.push(&b.left[..4000], Some(b.right()), 4000);
    assert_eq!(m.short_term_lufs(), -200.0, "under 100 ms: nothing yet");
    m.push(&b.left[4000..], Some(&b.right()[4000..]), b.length() - 4000);
    assert!(
        (m.short_term_lufs() - -20.0).abs() < 0.3,
        "{}",
        m.short_term_lufs()
    );
    assert!((m.momentary_lufs() - m.short_term_lufs()).abs() < 0.1);
    let quiet = vec![0f32; 48_000 * 4];
    m.push(&quiet, None, quiet.len());
    assert_eq!(m.short_term_lufs(), -200.0);
    assert_eq!(m.momentary_lufs(), -200.0);
}
