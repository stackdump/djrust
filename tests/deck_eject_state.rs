//! What `Deck.eject` (a reset's first step, `DJEngine.reset(to:clip:)`) leaves of what the deck's
//! feeds and its sample half keep: the display feeds of `deck_feeds.rs` (the marking book, the
//! role tables behind `Deck.macroActivity`, the fire and anchor rings), the clip's FX chain and
//! the FX unit's place of `deck_sample_master.rs`, against the replay half's `eject` /
//! `reset_deck` (`journal.rs`).
//!
//! Parity form (a): each test says which Swift lines it holds the Rust to. `eject()` is
//! `leaveNetMode(); setPlaying(false); ...; clipID = nil; ...` (DJEngine.swift); `leaveNetMode`
//! pauses the wave, goes back to `.sample` mode and `syncFxUnit()`s; `clipID`'s `didSet` is
//! `if clipID == nil { setClipFx(.off, now: true) }`; `macroActivity` is `guard mode == .net`.
//! Nothing in them touches `wave.latest`, the `mirrors` of the marking book, the rings or the
//! sample deck's master bus, so those stay as they were.

mod common_track;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common_track::Stepper;
use djrust::control::chase::MacroActivity;
use djrust::control::controller::DeckView;
use djrust::deck::beat_grid::BeatGrid;
use djrust::deck::turntable::SampleBuffer;
use djrust::fx::unit::{FxParam, FxUnitSettings};
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::wave::{EventCursor, MarkingSnapshot, NetMacro};
use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const RATE: f64 = 22_050.0;

fn looped(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, None)
}

fn playing(spec: &TrackSpec) -> Stepper {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", spec));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st
}

// `macroActivity`: `guard mode == .net`. An ejected net deck is back in `.sample` mode in Swift;
// here it keeps its paused runner, and with it the macros the runner was running, but is no net
// (`MainDeck::is_net` wants the project): none of them is the deck's activity any more. (Holds
// with and without any special case in `macro_activity`: this pins the seam.)
#[test]
fn an_ejected_net_deck_has_no_macro_activity() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(86);
    st.e.fire("A", NetMacro::Breakdown, None);
    st.step(4);
    let before = st.e.macro_activity("A");
    assert_eq!(before.running, vec![NetMacro::Breakdown]);
    assert!(!before.targets[&NetMacro::Breakdown].is_empty());

    st.e.eject("A");
    st.step(4);
    assert_eq!(st.e.macro_activity("A"), MacroActivity::idle());
    // the pad lamps read the wave itself (`SurfaceController`: `d.wave.activeMacros.contains(m)`,
    // no mode check), so they do not change with the eject
    assert!(
        st.e.deck("A").unwrap().net_macro_active("breakdown"),
        "the paused runner still holds its macro"
    );
}

// `eject` leaves `wave.latest` and the marking mirrors alone (`leaveNetMode` pauses the wave and
// nothing more): `readMarking` and `markingLayout` still answer for the project the deck held, a
// project loaded after it is the newest, and the earlier one is still in the book.
#[test]
fn an_ejected_deck_keeps_its_marking_book_and_role_tables() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(40);
    let first = st.e.deck("A").unwrap().track.latest.clone().unwrap();
    let layout = st.e.marking_layout("A").unwrap();
    assert_eq!(layout.generation, first.generation);

    st.e.eject("A");
    st.step(4);
    assert!(st.e.deck("A").unwrap().project.is_none() && !st.e.deck("A").unwrap().playing);
    let mut snap = MarkingSnapshot::new();
    assert!(
        st.e.read_marking("A", &mut snap),
        "`latest?.marking` is still there"
    );
    assert_eq!(snap.generation(), first.generation);
    assert_eq!(
        st.e.marking_layout("A").unwrap().generation,
        first.generation
    );
    assert!(
        st.e.deck("A")
            .unwrap()
            .track
            .latest
            .as_ref()
            .is_some_and(|l| l.generation == first.generation),
        "the role tables stay with it"
    );

    // another project lands: it is the newest, and the first stays readable by its generation
    assert!(st.e.generate("A", &looped("house", 3.0)));
    let second = st.e.deck("A").unwrap().track.latest.clone().unwrap();
    assert!(second.generation > first.generation);
    assert_eq!(
        st.e.marking_layout("A").unwrap().generation,
        second.generation
    );
    assert!(st.e.marking_layout_of("A", first.generation).is_some());
    assert!(st.e.read_marking_of("A", first.generation, &mut snap));
}

// `eject` leaves the fires and anchors rings where they are (`WaveDeckSource.fires` / `anchors`
// are the wave's, never cleared) and the deck's frame count keeps running: a reader that held a
// cursor across a reset reads on, and a reader that subscribes after it starts at the newest.
#[test]
fn an_ejected_deck_keeps_its_event_rings_and_frame_count() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(86);
    let mut cursor = st.e.event_cursor_now("A");
    st.step(40);
    let frames =
        st.e.deck("A")
            .unwrap()
            .mirror
            .rendered_frames
            .load(Ordering::Acquire);
    let written = st.e.deck("A").unwrap().mirror.anchors.written();
    assert!(written > 0);

    st.e.reset_deck("A");
    st.step(8);
    let m = st.e.deck("A").unwrap().mirror.clone();
    assert_eq!(
        m.anchors.written(),
        written,
        "a paused runner ticks no more, and nothing is cleared"
    );
    assert!(
        m.rendered_frames.load(Ordering::Acquire) > frames,
        "the deck still counts its frames"
    );
    let (mut fires, mut anchors) = (Vec::new(), Vec::new());
    let lost = st.e.read_events("A", &mut cursor, &mut fires, &mut anchors);
    assert_eq!(lost, (0, 0));
    assert!(
        !anchors.is_empty(),
        "what was written before the reset is still readable"
    );
    // a subscriber after the reset starts at the newest anchor
    let mut late: EventCursor = st.e.event_cursor_now("A");
    anchors.clear();
    fires.clear();
    st.e.read_events("A", &mut late, &mut fires, &mut anchors);
    assert!(anchors.len() <= 1 && fires.is_empty());
}

fn sine(hz: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
    let n = (RATE * seconds) as usize;
    (0..n)
        .map(|i| (amplitude * (std::f64::consts::TAU * hz * i as f64 / RATE).sin()) as f32)
        .collect()
}

/// An engine whose deck A is empty, with one clip on the loop row (2 s of a 375 Hz sine at 60 bpm:
/// 2 beats) whose FX are `fx`; the clip's id.
fn clip_rig(fx: &LoopFx) -> (Engine, Renderer, String) {
    let mut c = EngineConfig::new(RATE);
    c.decks = vec!["A".into()];
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    let b = sine(375.0, 0.5, 2.0);
    let id = e
        .import_clip(
            SampleBuffer::new(b.clone(), Some(b), RATE),
            "tone",
            Some(60.0),
            2,
            None,
        )
        .expect("a clip");
    e.set_clip_fx(&id, fx, None);
    (e, r, id)
}

fn blocks(r: &mut Renderer, n: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&rr);
    }
    (out_l, out_r)
}

// `eject`'s `clipID = nil`, whose `didSet` is `setClipFx(.off, now: true)`: a reset of a deck that
// holds a clip with FX ends them with the clip's stay, as anything else loaded there does
// (`LoopFxDeckTests.anythingElseLoadedThereIsDry`). The same sample loaded after it plays to the
// byte what it plays on a deck that never had the FX.
#[test]
fn a_reset_ends_a_clips_fx_with_its_stay() {
    let fx = LoopFx {
        mode: LoopFxMode::Soft,
        drive: 18.0,
        amount: 0.9,
        ..LoopFx::default()
    };
    let (mut dry_e, mut dry_r, dry_id) = clip_rig(&LoopFx::default());
    let (mut e, mut r, id) = clip_rig(&fx);
    dry_e.play_clip(&dry_id, "A");
    e.play_clip(&id, "A");
    dry_e.launch("A");
    e.launch("A");
    blocks(&mut dry_r, 40);
    blocks(&mut r, 40);
    assert!(e.deck("A").unwrap().clip_fx_on);
    assert!(
        e.deck("A").unwrap().mirror.clip_fx_running(),
        "the chain is in the path"
    );

    for en in [&mut e, &mut dry_e] {
        en.reset_deck("A");
        assert!(en.deck("A").unwrap().clip_id.is_none());
    }
    assert!(
        !e.deck("A").unwrap().clip_fx_on,
        "its FX went with its stay"
    );

    let other = SampleBuffer::new(sine(440.0, 0.4, 1.0), None, RATE);
    for en in [&mut e, &mut dry_e] {
        let g = BeatGrid::new(120.0, 0.0, 1.0);
        let p = PreparedSample::with_grid(Arc::new(other.clone()), "other", Some(g));
        assert!(en.load_sample_with("A", p, false));
        en.launch("A");
    }
    blocks(&mut dry_r, 40);
    blocks(&mut r, 40);
    let (dry_other, wet_other) = (blocks(&mut dry_r, 8), blocks(&mut r, 8));
    assert!(
        wet_other.0 == dry_other.0 && wet_other.1 == dry_other.1,
        "dry again, to the byte"
    );
    assert!(
        !e.deck("A").unwrap().mirror.clip_fx_running(),
        "and the chain is out of the path"
    );
}

/// 300 blocks of 128 of deck A playing a techno net at 22.05 kHz, however it came to be loaded.
fn net_deck(prepare: impl FnOnce(&mut Engine)) -> Vec<f32> {
    let (mut e, mut r) = engine(EngineConfig::new(RATE));
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    prepare(&mut e);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal"))));
    e.set_tempo("A", 300.0);
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    let mut out = Vec::new();
    for _ in 0..300 {
        r.render_block(&mut l, &mut rr);
        out.extend_from_slice(&l);
    }
    out
}

// `leaveNetMode` / `loadNet`'s `syncFxUnit()`: the unit is on whichever chain the deck plays
// through. A sample deck's setting lives on its bus; the deck that is ejected and then given a
// net plays it through the unit it had, as one given the same setting at once does
// (`FxUnitDeckTests.theSettingFollowsTheDeckAcrossModes`, with an eject between the two modes).
#[test]
fn an_ejected_sample_deck_hands_its_fx_unit_to_the_net_chain() {
    let mut unit = FxUnitSettings::new();
    unit.set(FxParam::Filter, -0.6);
    unit.set(FxParam::Drive, 0.4);
    let dry = net_deck(|_| {});
    let direct = net_deck(|e| e.apply_fx_unit("A", &unit));
    assert!(
        direct != dry,
        "the unit is audible on the net, or this proves nothing"
    );
    let ejected = net_deck(|e| {
        assert!(e.load_demo("A"));
        e.set_fx("A", FxParam::Filter, -0.6);
        e.set_fx("A", FxParam::Drive, 0.4);
        e.eject("A");
    });
    assert!(
        ejected == direct,
        "the net plays through the unit the sample deck had"
    );
}
