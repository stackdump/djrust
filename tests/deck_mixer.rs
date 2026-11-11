//! The mixer panel's engine side (docs/HUB.md, M1): SYNC lines a deck's tempo and bar up with
//! the other, the tempo fader's range, START / STOP, the loop channel's strip, and the whole
//! mixer read back and restored as one document.

use djrust::control::Action;
use djrust::deck::beat::SyncAlign;
use djrust::fx::StripKnob;
use djrust::mixer::{MixerState, TempoSource};
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn blocks(e: &mut Engine, r: &mut Renderer, n: usize) -> f32 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut peak = 0f32;
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        peak = l.iter().fold(peak, |p, v| p.max(v.abs()));
    }
    peak
}

fn two_decks(a: &str, b: &str) -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    assert!(e.generate("A", &TrackSpec::new(a, 42.0, None)));
    assert!(e.generate("B", &TrackSpec::new(b, 7.0, None)));
    (e, r)
}

#[test]
fn sync_matches_the_tempo_and_lines_up_the_bar() {
    let (mut e, mut r) = two_decks("techno", "house");
    e.set_pitch_range("B", 0.5); // whatever the two genres' tempos, B can reach A's
    e.set_crossfader(0.0); // A is the master tempo
    e.play("A");
    blocks(&mut e, &mut r, 40);
    e.play("B");
    blocks(&mut e, &mut r, 77); // B a bar fraction off
    assert_eq!(e.sync_target("B").as_deref(), Some("A"));
    let res = e
        .sync_to_master("B", SyncAlign::Bar)
        .expect("both decks have a clock");
    assert!(!res.tempo_clamped);
    blocks(&mut e, &mut r, 8);
    let (a, b) = (e.beat_state("A").unwrap(), e.beat_state("B").unwrap());
    let m = res.multiplier;
    assert!(
        (b.bpm * m - a.bpm).abs() < 1e-6,
        "tempo {} x {m} vs {}",
        b.bpm,
        a.bpm
    );
    // bar phase in A's beats, wrapped
    let mut err = (b.beat * m - a.beat) / 4.0;
    err -= err.round();
    assert!(
        err.abs() * 4.0 < 0.02,
        "{err} bars apart (A {a:?}, B {b:?})"
    );
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
}

#[test]
fn the_inpulse_sync_button_syncs() {
    let (mut e, mut r) = two_decks("techno", "techno");
    e.set_crossfader(0.0);
    e.play("A");
    e.play("B");
    e.nudge("B", 0.03);
    blocks(&mut e, &mut r, 20);
    e.apply_action(Action::Sync {
        deck: "B".into(),
        bar: false,
    });
    blocks(&mut e, &mut r, 4);
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
    let (a, b) = (e.beat_state("A").unwrap(), e.beat_state("B").unwrap());
    assert!((a.bpm - b.bpm).abs() < 1e-6);
    // nothing to sync to: counted, not applied
    e.pause("A");
    e.pause("B");
    let (mut lone, _r) = engine(EngineConfig::new(SR));
    lone.apply_action(Action::Sync {
        deck: "A".into(),
        bar: true,
    });
    assert_eq!(lone.unhandled.len(), 1);
}

#[test]
fn the_pitch_range_widens_the_tempo_fader() {
    let (mut e, mut r) = two_decks("techno", "house");
    let base = e.base_bpm("A").unwrap();
    assert!(e.set_pitch("A", 1.0));
    e.play("A");
    blocks(&mut e, &mut r, 4);
    assert!((e.beat_state("A").unwrap().bpm - base * 1.08).abs() < 1e-6);
    e.set_pitch_range("A", 0.17); // snaps to ±16 %
    assert_eq!(e.deck("A").unwrap().pitch_range, 0.16);
    e.set_pitch("A", -1.0);
    blocks(&mut e, &mut r, 4);
    assert!((e.beat_state("A").unwrap().bpm - base * 0.84).abs() < 1e-6);
    // narrowing pulls the nudge back inside
    e.set_pitch_range("A", 0.08);
    assert!((e.deck("A").unwrap().nudge + 0.08).abs() < 1e-12);
}

#[test]
fn start_and_stop_run_every_loaded_deck() {
    let (mut e, mut r) = two_decks("techno", "house");
    assert_eq!(e.master_tempo().0, TempoSource::Free);
    e.transport_start();
    blocks(&mut e, &mut r, 4);
    assert!(e.transport.running);
    assert!(e.decks.iter().all(|d| d.mirror.is_playing()));
    let a_tick = e.deck("A").unwrap().mirror.tick();
    let b_tick = e.deck("B").unwrap().mirror.tick();
    assert_eq!(a_tick, b_tick, "they started in the same block");
    e.set_crossfader(1.0);
    assert_eq!(e.master_tempo().0, TempoSource::Deck("B".into()));
    e.transport_stop();
    blocks(&mut e, &mut r, 4);
    assert!(!e.transport.running);
    assert!(e.decks.iter().all(|d| !d.mirror.is_playing()));
    assert_eq!(e.master_tempo().0, TempoSource::Free);
    // tap tempo sets the free tempo
    e.tap_tempo(1.0);
    assert_eq!(e.tap_tempo(1.5), Some(120.0));
    assert_eq!(e.master_tempo(), (TempoSource::Free, 120.0));
}

#[test]
fn the_loop_channel_has_a_strip() {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(1.0); // A out of the mix: only L is heard
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    e.play("A");
    blocks(&mut e, &mut r, 900);
    let id = e.grab_last_bars("A", 1).expect("a bar was played");
    e.set_looping(&id, true);
    e.pause("A");
    blocks(&mut e, &mut r, 400);
    assert!(blocks(&mut e, &mut r, 100) > 0.01, "L plays");
    e.set_loop_strip(StripKnob::Filter, 0.0); // the low-pass shut right down
    e.set_loop_strip(StripKnob::EqLow, 0.0);
    blocks(&mut e, &mut r, 20);
    let filtered = blocks(&mut e, &mut r, 100);
    e.set_loop_strip(StripKnob::Filter, 0.5);
    e.set_loop_strip(StripKnob::EqLow, 0.5);
    blocks(&mut e, &mut r, 20);
    let open = blocks(&mut e, &mut r, 100);
    assert!(filtered < open, "filtered {filtered} vs open {open}");
    e.set_loop_mute(true);
    blocks(&mut e, &mut r, 2);
    // A's cos(pi/2) crossfader gain leaks ~1e-17 of its tail; L itself is silent
    assert!(blocks(&mut e, &mut r, 50) < 1e-9, "muted");
    assert_eq!(e.loops.mirror.level(), 0.0);
}

#[test]
fn the_mixer_restores_from_its_document() {
    let (mut e, _r) = two_decks("techno", "house");
    let mut m = MixerState::new(&["A", "B", "C"]); // C: not on this engine, ignored
    m.strips[0].volume = 0.3;
    m.strips[0].eq.eq_high = 0.0;
    m.strips[1].mute = true;
    m.strips[1].pfl = true;
    m.strips[1].pitch_range = 0.16;
    m.loop_channel.volume = 0.4;
    m.loop_channel.mute = true;
    m.loop_channel.eq.filter = 0.7;
    m.crossfader = 0.2;
    m.master = 0.9;
    m.headphone_mix = 0.1;
    m.headphone_level = 0.6;
    m.transport.lock = Some("B".into());
    m.transport.free_bpm = 132.0;
    m.transport.running = true;
    e.apply_mixer_state(&m);
    let back = e.mixer_state();
    let mut want = m.clone();
    want.strips.truncate(2);
    want.transport.running = false; // restoring never starts the music
    // the engine keeps the master level as Swift's `Float` (`Engine::set_master`)
    want.master = 0.9f32 as f64;
    assert_eq!(back, want);
    // and through JSON
    let text = back.to_json().canonical_js();
    let parsed = MixerState::from_json(&djrust::JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(parsed, back);
}
