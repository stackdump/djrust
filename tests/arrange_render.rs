//! An arrangement rendered offline (`djrust::host::deck::offline::render_arrangement`), live ops
//! included: a house deck, a capture-library loop on the row and as the vocoder's modulator,
//! the internal synth as its carrier on a chord, a riser and a tape stop.

use std::collections::BTreeMap;

use djrust::json::JsonValue;
use djrust::looplib::LoopMeta;
use djrust::session::arrangement::{ArrangeEvent, ArrangeOp, Arrangement};
use djrust::session::model::DeckSource;
use djrust::session::DeckAction;
use djrust::TrackSpec;
use djrust::host::deck::offline::{render_arrangement, ArrangeRenderOptions};
use djrust::host::stores::captures::CaptureLibrary;

fn ev(at: f64, op: ArrangeOp) -> ArrangeEvent {
    ArrangeEvent {
        at,
        bpm: 124.0,
        op,
        via: String::new(),
        text: String::new(),
    }
}

fn live(json: &str) -> ArrangeOp {
    ArrangeOp::from_json(&JsonValue::parse(json).unwrap()).expect(json)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// A 4-beat 124 bpm "voice": bursts of a 300 Hz buzz on each beat.
fn library() -> (CaptureLibrary, std::path::PathBuf) {
    // one folder per call: the tests run in parallel
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let k = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("djhost-arrange-{}-{k}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sr = 44_100.0;
    let n = (4.0 * 60.0 / 124.0 * sr) as usize;
    let beat = n / 4;
    let audio: Vec<f32> = (0..n)
        .map(|i| {
            if i % beat < beat / 2 {
                let t = i as f64 / sr;
                (0.5 * (2.0 * std::f64::consts::PI * 300.0 * t).sin().signum()) as f32
            } else {
                0.0
            }
        })
        .collect();
    let lib = CaptureLibrary::new(&dir);
    let meta = LoopMeta {
        id: "voice".into(),
        title: "voice".into(),
        bpm: 124.0,
        beats: 4,
        sample_rate: sr,
        frames: n,
        channels: 1,
        source: "test".into(),
        kind: "input".into(),
        created: 0,
        row: false,
        from: String::new(),
    };
    lib.save(&meta, &audio, None).unwrap();
    (lib, dir)
}

fn arrangement(with_vocoder: bool) -> Arrangement {
    let decks = BTreeMap::from([(
        "A".to_string(),
        DeckSource::Net(TrackSpec::new("house", 7.0, None)),
    )]);
    let mut events = vec![
        ev(0.0, ArrangeOp::Deck(DeckAction::Play { deck: "A".into(), on: true })),
        ev(0.0, live(r#"{"op":"library","action":"play","id":"voice"}"#)),
        ev(8.0, live(r#"{"op":"trick","deck":"A","kind":"riser","action":"on","bars":2}"#)),
        ev(28.0, live(r#"{"op":"trick","deck":"A","kind":"tape_stop","action":"on","beats":2}"#)),
    ];
    if with_vocoder {
        events.push(ev(
            0.0,
            live(r#"{"op":"vocoder","modulator":"library:voice","carrier":"synth","notes":["A2","C3","E3"],"bands":24,"mix":1,"noise":0.5,"synth_level":0.6,"volume":0.9,"on":true}"#),
        ));
        events.push(ev(16.0, live(r#"{"op":"vocoder","notes":["F2","A2","C3"]}"#)));
    }
    Arrangement::new(124.0, decks, events)
}

#[test]
fn an_arrangement_with_live_ops_renders_offline() {
    let (lib, dir) = library();
    let opts = |lib| ArrangeRenderOptions {
        library: Some(lib),
        tail_beats: 4.0,
        ..ArrangeRenderOptions::default()
    };
    let r = render_arrangement(&arrangement(true), opts(lib));
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(r.dropped, 0);
    let sr = r.audio.sample_rate;
    let frames_per_beat = 60.0 / 124.0 * sr;
    // the moves' whole bars (to beat 32) + 4 of tail
    assert_eq!(arrangement(true).beats(), 32.0);
    assert_eq!(r.audio.frames(), (36.0 * frames_per_beat).round() as usize);
    let at = |b: f64| (b * frames_per_beat) as usize;
    let playing = rms(&r.audio.left[at(4.0)..at(8.0)]);
    assert!(playing > 0.01, "the deck and the vocoder play: {playing}");
    // the tape stop halts the deck; the row loop and the vocoder go on, so not silence, but
    // less than while the deck played
    let after = rms(&r.audio.left[at(31.0)..at(36.0)]);
    assert!(after < playing, "{after} vs {playing}");

    // the vocoder is heard: the same arrangement without it differs
    let (lib2, _) = (CaptureLibrary::new(&dir), ());
    let dry = render_arrangement(&arrangement(false), opts(lib2));
    let diff: f64 = r.audio.left[at(4.0)..at(8.0)]
        .iter()
        .zip(&dry.audio.left[at(4.0)..at(8.0)])
        .map(|(a, b)| ((a - b) as f64).abs())
        .sum();
    assert!(diff > 1.0, "vocoder made no difference: {diff}");

    // deterministic
    let again = render_arrangement(&arrangement(true), opts(CaptureLibrary::new(&dir)));
    assert_eq!(again.audio.left, r.audio.left);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_live_op_the_engine_refuses_is_reported_with_its_beat() {
    let decks = BTreeMap::from([(
        "A".to_string(),
        DeckSource::Net(TrackSpec::new("house", 7.0, None)),
    )]);
    let arr = Arrangement::new(
        124.0,
        decks,
        vec![
            ev(4.0, live(r#"{"op":"library","action":"play","id":"missing"}"#)),
            ev(5.0, live(r#"{"op":"trick","deck":"A","kind":"nope"}"#)),
        ],
    );
    let r = render_arrangement(&arr, ArrangeRenderOptions::default());
    assert_eq!(r.refused.len(), 2, "{:?}", r.refused);
    assert_eq!(r.refused[0].0, 4.0);
    assert!(r.refused[0].1.contains("missing"));
}

/// Talking drums offline: the library loop shapes the synth, then the roles swap, the bands
/// freeze, and the crossfader takes over the vocoder: every one a live op, so a recorded set
/// plays them back the same, deterministically.
#[test]
fn the_vocoders_swap_freeze_and_morph_render_offline() {
    let (lib, dir) = library();
    let opts = |lib| ArrangeRenderOptions {
        library: Some(lib),
        tail_beats: 0.0,
        ..ArrangeRenderOptions::default()
    };
    let arr = || {
        Arrangement::new(
            124.0,
            BTreeMap::new(),
            vec![
                ev(0.0, live(r#"{"op":"vocoder","modulator":"library:voice","carrier":"synth","notes":["A2","C3","E3"],"noise":0,"volume":1,"on":true}"#)),
                ev(8.0, live(r#"{"op":"vocoder","swap":true}"#)),
                ev(16.0, live(r#"{"op":"vocoder","freeze":true}"#)),
                ev(20.0, live(r#"{"op":"vocoder","notes":[]}"#)),
                ev(24.0, live(r#"{"op":"vocoder","freeze":false,"xfade":true}"#)),
                ev(24.0, ArrangeOp::Deck(DeckAction::Crossfader(0.0))),
                ev(28.0, ArrangeOp::Deck(DeckAction::Crossfader(1.0))),
                ev(32.0, ArrangeOp::Deck(DeckAction::Crossfader(1.0))),
            ],
        )
    };
    let r = render_arrangement(&arr(), opts(lib));
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    let fpb = 60.0 / 124.0 * r.audio.sample_rate;
    let at = |b: f64| (b * fpb) as usize;
    let seg = |a: f64, b: f64| rms(&r.audio.left[at(a)..at(b)]);
    let talking = seg(2.0, 8.0);
    let swapped = seg(10.0, 16.0);
    assert!(talking > 0.005, "talking drums {talking}");
    assert!(swapped > 0.001, "the synth shapes the loop {swapped}");
    // frozen, the synth's notes released: the loop (now the carrier) still sounds through the
    // held bands
    let frozen = seg(21.0, 24.0);
    assert!(frozen > 0.001, "held {frozen}");
    // the crossfader left: the carrier (the voice loop) alone; right: the modulator, the synth
    // with no notes, silent
    let left = seg(25.0, 28.0);
    let right = seg(29.0, 32.0);
    assert!(left > 0.05, "the carrier loop {left}");
    assert!(right < left * 0.05, "the silent synth {right} vs {left}");
    let again = render_arrangement(&arr(), opts(CaptureLibrary::new(&dir)));
    assert_eq!(again.audio.left, r.audio.left);
    let _ = std::fs::remove_dir_all(&dir);
}
