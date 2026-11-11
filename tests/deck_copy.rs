//! Copy a deck onto the other (docs/COPY-DECK.md): the same song and sound, landing on the
//! source's next bar in sync; a sample deck's audio at its place.

use djrust::deck::turntable::SampleBuffer;
use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const RATE: f64 = 22_050.0;
const BLOCK: usize = 256;

fn block(e: &mut Engine, r: &mut Renderer) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    r.render_block(&mut l, &mut rr);
    e.service_copies();
}

#[test]
fn a_net_deck_copies_onto_a_stopped_deck_on_the_next_bar_in_sync() {
    let (mut e, mut r) = engine(EngineConfig::new(RATE));
    let spec = TrackSpec::new("techno", 4213.0, None);
    assert!(e.generate("A", &spec));
    e.play("A");
    for _ in 0..40 {
        block(&mut e, &mut r);
    }
    e.trace = Some(Vec::new());
    let what = e.copy_deck("A", "B").unwrap();
    assert!(what.contains("A → B"), "{what}");
    assert_eq!(e.pending_copies().len(), 1);
    let mut n = 0;
    while !e.pending_copies().is_empty() && n < 4000 {
        block(&mut e, &mut r);
        n += 1;
    }
    assert!(e.pending_copies().is_empty(), "the copy landed");
    let (a, b) = (e.deck("A").unwrap(), e.deck("B").unwrap());
    assert_eq!(a.spec, b.spec, "the same song and sound choices");
    assert!(b.mirror.is_playing());
    assert!(
        (e.effective_bpm("A").unwrap() - e.effective_bpm("B").unwrap()).abs() < 1e-6,
        "the same tempo"
    );
    block(&mut e, &mut r);
    let (ta, tb) = (e.deck("A").unwrap().mirror.tick(), e.deck("B").unwrap().mirror.tick());
    assert!((ta - tb).abs() <= 1, "the same place in the song: A {ta}, B {tb}");
    let lines: Vec<String> = e.trace.as_ref().unwrap().iter().map(|t| t.entry.text.clone()).collect();
    assert!(lines.iter().any(|l| l.contains("copy A → B")), "{lines:?}");
}

#[test]
fn copy_needs_two_decks_and_something_on_the_source() {
    let (mut e, _r) = engine(EngineConfig::new(RATE));
    assert!(e.copy_deck("A", "A").is_err());
    assert!(e.copy_deck("A", "Z").is_err());
    assert!(e.copy_deck("A", "B").is_err(), "an empty deck has nothing to copy");
}

#[test]
fn a_sample_deck_copies_its_audio_and_place() {
    let (mut e, mut r) = engine(EngineConfig::new(RATE));
    let audio: Vec<f32> = (0..(RATE as usize * 4)).map(|i| ((i as f32) * 0.01).sin() * 0.3).collect();
    let s = PreparedSample::with_grid(std::sync::Arc::new(SampleBuffer::new(audio, None, RATE)), "tone", None);
    assert!(e.load_sample("A", s));
    e.seek("A", 1000.0);
    let what = e.copy_deck("A", "B").unwrap();
    assert!(what.contains("sample tone"), "{what}");
    block(&mut e, &mut r);
    assert!(e.pending_copies().is_empty());
    let b = e.deck("B").unwrap();
    assert_eq!(b.sample.as_ref().map(|s| s.title.as_str()), Some("tone"));
    assert!(std::sync::Arc::ptr_eq(
        &b.sample.as_ref().unwrap().buffer,
        &e.deck("A").unwrap().sample.as_ref().unwrap().buffer
    ));
}


#[test]
fn a_net_deck_copies_over_a_playing_deck_on_the_bar_in_sync() {
    let (mut e, mut r) = engine(EngineConfig::new(RATE));
    assert!(e.generate("A", &TrackSpec::new("techno", 4213.0, None)));
    assert!(e.generate("B", &TrackSpec::new("house", 7.0, None)));
    e.play("A");
    for _ in 0..30 {
        block(&mut e, &mut r);
    }
    e.play("B");
    for _ in 0..47 {
        block(&mut e, &mut r);
    }
    e.copy_deck("A", "B").unwrap();
    let mut n = 0;
    while !e.pending_copies().is_empty() && n < 4000 {
        block(&mut e, &mut r);
        n += 1;
    }
    assert!(e.pending_copies().is_empty());
    for _ in 0..20 {
        block(&mut e, &mut r);
    }
    assert_eq!(e.deck("A").unwrap().spec, e.deck("B").unwrap().spec);
    let (ta, tb) = (e.deck("A").unwrap().mirror.tick(), e.deck("B").unwrap().mirror.tick());
    assert!((ta - tb).abs() <= 1, "A {ta}, B {tb}");
    assert!((e.effective_bpm("A").unwrap() - e.effective_bpm("B").unwrap()).abs() < 1e-6);
}
