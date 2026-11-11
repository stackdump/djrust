//! One loudness on the sample decks (`LevelMatchTests.swift`, the engine half): a sample loads
//! at the app's reference loudness when the deck's auto-gain is on, and plays at its own level
//! when it is off; a clip keeps the level it was taken at. Each test names the Swift test it
//! ports ("(a)" in PORTING.md's parity rule). The measurement itself (`SampleLevel`) is held to
//! Swift's in `djengine/tests/level_match.rs` and `tests/loops_golden.rs`.

use djrust::deck::level_match::LevelMatch;
use djrust::deck::turntable::SampleBuffer;
use djrust::musicality::loudness::IntegratedLoudness;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 48_000.0;

/// `seconds` of three sines (100 Hz, 1 kHz, 5 kHz), each at `amplitude`.
fn tones(amplitude: f32, seconds: f64) -> SampleBuffer {
    let n = (seconds * SR) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let s: f64 = [100.0, 1000.0, 5000.0]
                .iter()
                .map(|f| (2.0 * std::f64::consts::PI * f * i as f64 / SR).sin())
                .sum();
            amplitude * s as f32
        })
        .collect();
    SampleBuffer::new(l.clone(), Some(l), SR)
}

/// Deck A alone on the master at unity: crossfader on A, fader and master up.
fn engine_a() -> (djrust::host::deck::Engine, djrust::host::deck::Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.set_master(1.0);
    (e, r)
}

fn play(r: &mut djrust::host::deck::Renderer, seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let (mut ol, mut or) = (Vec::new(), Vec::new());
    let blocks = (seconds * SR / 512.0).ceil() as usize;
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        ol.extend_from_slice(&l);
        or.extend_from_slice(&rr);
    }
    (ol, or)
}

fn peak_db(l: &[f32], r: &[f32]) -> f64 {
    let p = l.iter().chain(r).fold(0f32, |p, v| p.max(v.abs()));
    20.0 * (p.max(1e-10) as f64).log10()
}

// LevelMatchTests.aSampleLoadsAtTheReference
#[test]
fn a_sample_loads_at_the_reference() {
    let (mut e, mut r) = engine_a();
    let quiet = tones(0.01, 4.0); // about -30 LUFS, peak -30 dBFS: room to lift it
    assert!(!e.auto_gain("A"), "off by default, as Deck's");
    assert!(e.load_sample("A", PreparedSample::new(quiet, "quiet")));
    e.set_auto_gain("A", true); // the app's default; off in DJCore
    let level = e.sample_level("A").expect("a sample was loaded");
    assert!(!level.partial());
    assert_eq!(e.auto_gain_db("A"), level.auto_gain_db());
    assert!(e.auto_gain_db("A") > 0.0);
    e.play("A");
    let (l, rr) = play(&mut r, 3.0);
    let (l, rr) = (&l[24_000..], &rr[24_000..]);
    let measured = IntegratedLoudness::integrated(l, Some(rr), SR).unwrap();
    let expected = LevelMatch::REFERENCE_LUFS.min(level.lufs.unwrap() + LevelMatch::MAX_GAIN_DB);
    // (the master chain's channel gain for A at the centred fader is the crossfade's, 1 here)
    assert!((measured - expected).abs() < 0.5, "{measured} LUFS");
    assert!(peak_db(l, rr) <= LevelMatch::SAMPLE_PEAK_CEILING_DB + 0.01);
}

// LevelMatchTests.autoGainOffPlaysTheAudioAsItIs
#[test]
fn auto_gain_off_plays_the_audio_as_it_is() {
    let (mut e, mut r) = engine_a();
    e.set_auto_gain("A", false);
    assert!(!e.auto_gain("A"));
    let b = tones(0.01, 4.0);
    let own = SampleBufferPeak::of(&b);
    assert!(e.load_sample("A", PreparedSample::new(b, "as is")));
    assert!(e.auto_gain_db("A") == 0.0 && e.sample_level("A").is_some());
    e.play("A");
    let (l, rr) = play(&mut r, 0.5);
    // full speed from the start is not guaranteed (motor spin-up), so compare levels
    assert!(
        (peak_db(&l, &rr) - own.0).abs() < 0.2,
        "{} vs {}",
        peak_db(&l, &rr),
        own.0
    );
    // on again: the gain glides in
    e.set_auto_gain("A", true);
    assert_eq!(
        e.auto_gain_db("A"),
        e.sample_level("A").unwrap().auto_gain_db()
    );
    assert!(e.auto_gain_db("A") > 0.0);
}

struct SampleBufferPeak(f64);

impl SampleBufferPeak {
    fn of(b: &SampleBuffer) -> SampleBufferPeak {
        SampleBufferPeak(peak_db(&b.left, b.right()))
    }
}

// LevelMatchTests.aTakeOfANetDeckPlaysAtTheDecksLevel (the clip half: `level: false` keeps the
// level it was taken at, no auto-gain even with the deck's setting on)
#[test]
fn a_clip_keeps_the_level_it_was_taken_at() {
    let (mut e, mut r) = engine_a();
    e.set_auto_gain("A", true);
    let b = tones(0.01, 4.0);
    let own = SampleBufferPeak::of(&b).0;
    assert!(e.load_sample_with("A", PreparedSample::new(b, "clip"), false));
    assert!(e.sample_level("A").is_none() && e.auto_gain_db("A") == 0.0);
    e.play("A");
    let (l, rr) = play(&mut r, 0.5);
    assert!((peak_db(&l, &rr) - own).abs() < 0.2);
}

// LevelMatchTests.theSettingIsADeckParamThatRoundTrips (the default: off, per deck)
#[test]
fn the_setting_is_per_deck_and_off_by_default() {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, _r) = engine(c);
    assert!(!e.auto_gain("A") && !e.auto_gain("B"));
    e.set_auto_gain("B", true);
    assert!(!e.auto_gain("A") && e.auto_gain("B"));
    assert!(!e.auto_gain("nope"));
}
