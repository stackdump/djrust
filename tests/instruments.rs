//! `InstrumentTableTests.swift`: the per-instrument voices beyond beats' wave engine.
//! Golden: none (dj-only); the Swift suite's own measurements (in tune, rings and decays,
//! within 3 dB of the lead voice, the laser's dive, the sitar's bend) are the oracle.

use djrust::gen::tables::BeatsTables;
use djrust::wave::*;
use djrust::{compose, JsonObject};

const SR: f64 = 48_000.0;

fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let s: f64 = x.iter().map(|&v| v as f64 * v as f64).sum();
    10.0 * (s / x.len() as f64).max(1e-20).log10()
}

/// Fundamental over 40 Hz…2 kHz (first strong peak of the autocorrelation).
fn fundamental(x: &[f32], sr: f64) -> f64 {
    let v: Vec<f64> = x.iter().map(|&a| a as f64).collect();
    let min_lag = (sr / 2000.0) as usize;
    let max_lag = ((sr / 40.0) as usize).min(v.len() / 2);
    let mut r = vec![0.0; max_lag + 2];
    for lag in (min_lag - 1)..=(max_lag + 1) {
        let (mut s, mut ea, mut eb) = (0.0, 0.0, 0.0);
        for i in 0..(v.len() - lag) {
            s += v[i] * v[i + lag];
            ea += v[i] * v[i];
            eb += v[i + lag] * v[i + lag];
        }
        r[lag] = s / (ea * eb).sqrt().max(1e-30);
    }
    let best = r[min_lag..=max_lag]
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let mut lag = min_lag;
    while lag < max_lag && r[lag] < 0.9 * best {
        lag += 1;
    }
    while lag < max_lag && r[lag + 1] > r[lag] {
        lag += 1;
    }
    let (a, b, c) = (r[lag - 1], r[lag], r[lag + 1]);
    let p = (a - c) / (2.0 * (a - 2.0 * b + c));
    sr / (lag as f64 + if p.is_finite() { p } else { 0.0 })
}

fn cents(f: f64, r: f64) -> f64 {
    1200.0 * (f / r).log2()
}

fn midi_hz(n: i64) -> f64 {
    440.0 * 2f64.powf((n - 69) as f64 / 12.0)
}

/// Frequency of the strongest partial within ±100 cents of `near` (Hann-windowed DFT,
/// scanned in 0.5-cent steps).
fn partial_peak(x: &[f32], near: f64, sr: f64) -> f64 {
    let n = x.len();
    let w: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())
        .collect();
    let (mut best, mut best_mag) = (near, -1.0);
    let mut c = -100.0;
    while c <= 100.0 {
        let f = near * 2f64.powf(c / 1200.0);
        let k = 2.0 * std::f64::consts::PI * f / sr;
        let (mut re, mut im) = (0.0, 0.0);
        for i in 0..n {
            let s = x[i] as f64 * w[i];
            re += s * (k * i as f64).cos();
            im -= s * (k * i as f64).sin();
        }
        let mag = re * re + im * im;
        if mag > best_mag {
            best_mag = mag;
            best = f;
        }
        c += 0.5;
    }
    best
}

const PHRASE: [(i64, f64, f64); 7] = [
    (48, 0.0, 0.5),
    (52, 0.02, 0.5),
    (55, 0.04, 0.5),
    (60, 0.06, 0.5),
    (64, 0.5, 0.25),
    (67, 0.75, 0.25),
    (72, 1.0, 0.5),
];

fn table() -> &'static InstrumentTable {
    InstrumentTable::shared()
}

fn voice(name: &str) -> VoiceSpec {
    table().voice(Some(name), Some("melody"))
}

fn render(name: &str, notes: &[(i64, f64, f64)], s: f64) -> Vec<f32> {
    audition(&voice(name), notes, SR, s, 100)
}

fn plucked() -> Vec<String> {
    table()
        .names()
        .into_iter()
        .filter(|n| voice(n).engine == "pluck")
        .map(String::from)
        .collect()
}

fn has_voice(name: &str) -> bool {
    voice(name) != tonal_voice(Some(name), Some("melody"))
}

fn lead() -> Vec<f32> {
    audition(
        &tonal_voice(Some("supersaw"), Some("melody")),
        &PHRASE,
        SR,
        2.0,
        100,
    )
}

#[test]
fn every_entry_is_a_beats_instrument_with_a_known_engine() {
    let mut used = std::collections::BTreeSet::new();
    for set in BeatsTables::shared().instrument_sets.values() {
        for pool in set.values() {
            used.extend(pool.iter().cloned());
        }
    }
    assert!(plucked().len() >= 8);
    for n in table().names() {
        assert!(
            used.contains(n),
            "{n} is an instrument beats' generator picks"
        );
        assert!(["table", "pluck"].contains(&voice(n).engine.as_str()));
    }
}

#[test]
fn the_parity_path_ignores_the_table() {
    let plain = tonal_voice(Some("acoustic-guitar"), Some("melody"));
    assert_eq!(plain.engine, "table");
    assert_eq!(voice("acoustic-guitar").engine, "pluck");
    assert_eq!(
        voice("supersaw"),
        tonal_voice(Some("supersaw"), Some("melody"))
    );
}

#[test]
fn strings_play_in_tune() {
    for name in [
        "acoustic-guitar",
        "electric-guitar",
        "distorted-guitar",
        "harpsichord",
        "pluck",
        "bright-pluck",
    ] {
        for note in [40, 52, 64, 76] {
            let a = render(name, &[(note, 0.0, 1.0)], 0.5);
            let f = partial_peak(&a[960..20160], midi_hz(note), SR);
            let c = cents(f, midi_hz(note));
            assert!(c.abs() < 5.0, "{name} note {note}: {c} cents");
        }
    }
}

#[test]
fn strings_ring_and_die() {
    for name in plucked() {
        let a = render(&name, &[(57, 0.0, 2.0)], 3.0);
        let early = rms_db(&a[0..2400]);
        let late = rms_db(&a[a.len() - 4800..]);
        assert!(early > -40.0, "{name} sounds ({early} dB)");
        assert!(early - late > 20.0, "{name} decays ({early} -> {late} dB)");
    }
    // a palm-muted string dies much faster than a sustaining distorted one
    fn t20(name: &str) -> usize {
        let a = render(name, &[(57, 0.0, 2.0)], 3.0);
        let r = rms_db(&a[2400..4800]);
        let mut i = 4800;
        while i + 2400 < a.len() && rms_db(&a[i..i + 2400]) > r - 20.0 {
            i += 2400;
        }
        i
    }
    assert!(t20("muted-pluck") * 3 < t20("distorted-guitar"));
}

#[test]
fn levels_sit_with_the_lead_voice() {
    let reference = rms_db(&lead());
    // every entry with a voice of its own, not just the strings: a new one must sit in the mix
    for name in table().names() {
        if has_voice(name) {
            let db = rms_db(&render(name, &PHRASE, 2.0));
            assert!(
                (db - reference).abs() < 3.0,
                "{name} at {db} dB vs lead {reference} dB"
            );
        }
    }
}

/// The melody instruments that all fell into the default saw now each sound like
/// themselves: none renders the same as the plain saw lead.
#[test]
fn melody_leads_no_longer_share_one_saw() {
    let saw = lead();
    for name in [
        "flute",
        "trumpet",
        "sax",
        "brass",
        "sync-lead",
        "lead",
        "duo-lead",
        "detuned-saw",
        "distorted-lead",
        "hoover",
        "wobble-lead",
        "screech",
        "big-saw",
        "trance-lead",
        "talkbox",
        "tape-lead",
        "scream-lead",
        "laser",
    ] {
        assert!(has_voice(name), "{name} has its own voice");
        assert!(
            render(name, &PHRASE, 2.0) != saw,
            "{name} is not the plain saw"
        );
    }
}

/// A laser is a zap: each note starts 12x high and dives exponentially to 0.42x over 0.4 s,
/// then dies. Pitch by zero crossings (two per cycle of a saw).
#[test]
fn laser_zaps() {
    let a = render("laser", &[(60, 0.0, 0.5)], 0.5);
    let v = voice("laser");
    assert!(v.pitch_time == 0.4 && v.pitch_from > v.pitch_to);
    let expected =
        |t: f64| midi_hz(60) * v.pitch_from * (v.pitch_to / v.pitch_from).powf(t / v.pitch_time);
    let measured = |t: f64, span: f64| {
        let lo = (t * SR) as usize;
        let hi = ((t + span) * SR) as usize;
        let mut crossings = 0;
        for i in (lo + 1)..hi {
            if (a[i - 1] < 0.0) != (a[i] < 0.0) {
                crossings += 1;
            }
        }
        crossings as f64 / 2.0 / span
    };
    let mut last = f64::INFINITY;
    for (t, span) in [(0.01, 0.01), (0.05, 0.02), (0.1, 0.03), (0.2, 0.05)] {
        let f = measured(t, span);
        let want = expected(t + span / 2.0);
        assert!(
            cents(f, want).abs() < 100.0,
            "at {t} s: {f} Hz vs {want} Hz"
        );
        assert!(f < last, "it falls");
        last = f;
    }
    assert!(
        rms_db(&a[(0.45 * SR) as usize..]) < rms_db(&a[0..4800]) - 20.0,
        "and dies"
    );
}

#[test]
fn sitar_bends() {
    let spread = |name: &str| {
        let a = render(name, &[(57, 0.0, 2.0)], 0.8);
        let fs: Vec<f64> = (2400..26_400)
            .step_by(1200)
            .map(|s| fundamental(&a[s..s + 4800], SR))
            .collect();
        cents(
            fs.iter().cloned().fold(f64::MIN, f64::max),
            fs.iter().cloned().fold(f64::MAX, f64::min),
        )
    };
    let s = spread("sitar");
    assert!(s > 10.0 && s < 80.0, "vibrato spread {s} cents");
    assert!(spread("acoustic-guitar") < 5.0);
}

#[test]
fn auditions_are_deterministic() {
    assert!(render("electric-guitar", &PHRASE, 1.0) == render("electric-guitar", &PHRASE, 1.0));
}

#[test]
fn app_tracks_get_their_strings() {
    let mut ov = JsonObject::new();
    ov.set("seed", 7.0.into());
    let p = compose("country", &ov.into());
    let ext = WaveRunner::prepare(&p, SR, &WaveOptions::extended(), 0);
    let plain = WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0);
    let has_entry = p
        .nets
        .values()
        .any(|n| table().entries.contains_key(&n.track.instrument));
    if has_entry {
        assert!(ext.lanes().iter().any(|l| l.is_pluck()));
    }
    assert!(!plain.lanes().iter().any(|l| l.is_pluck()));
}
