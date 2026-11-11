//! The beat clock, the transport, CUE and quantize, sync and follow, relocation and the views, on
//! net and sample decks: the Swift suites' own literals (`DeckBeatTests`, `QuantizeTests`,
//! `SurfaceNetTests`, `SurfaceBeatTests`, `SurfaceTests`, `DeckNetTests`, `NetSnapshotTests`,
//! `DeckOverlayTests`, `SectionTimelineTests`), each test naming the one it ports. The same
//! calls are held to Swift's own output in `beat_golden.rs`.

mod common_beat;

use common_beat::{Stepper, BLOCK, SR};
use djrust::control::controller::{DeckView, SurfaceHost};
use djrust::control::profiles::inpulse_200_mk3;
use djrust::deck::beat::{Quantize, SyncAlign, PITCH_RANGE};
use djrust::deck::turntable::TurntableCommand;
use djrust::json::JsonValue;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer, Rig};

const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";
const MPK: &str = "MPK Mini Mk II";

/// A net spec at exactly `bpm`.
fn net_spec(genre: &str, seed: f64, structure: Option<&str>, bpm: Option<f64>) -> TrackSpec {
    let mut s = TrackSpec::new(genre, seed, structure);
    if let Some(bpm) = bpm {
        s.extra.set("bpm", JsonValue::Number(bpm));
    }
    s
}

/// `frames` frames of 128, as the dump and the Swift suites render them (rounded to blocks).
fn frames(n: usize) -> usize {
    n.div_ceil(BLOCK)
}

fn wrapped(d: f64, period: f64) -> f64 {
    d - (d / period).round() * period
}

fn beat(st: &Stepper, deck: &str) -> f64 {
    st.e.deck(deck).unwrap().beat_state(SR).unwrap().beat
}

// MARK: DeckBeatTests

// DeckBeatTests.netBeatPositionTracksTheClock
#[test]
fn a_net_decks_beat_position_tracks_the_clock() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(120.0)));
    assert_eq!(beat(&st, "A"), 0.0);
    st.e.play("A");
    st.step(frames(48_000)); // 1 s = 2 beats at 120 bpm
    let s = st.e.deck("A").unwrap().beat_state(SR).unwrap();
    assert!((s.bpm - 120.0).abs() < 1e-9);
    assert!((s.beat - 2.0).abs() < 0.05, "beat {}", s.beat);
    assert!(s.beat_in_bar() == s.beat as i64 && s.bar() == 0);
}

// DeckBeatTests.sampleBeatPositionUsesTheGrid
#[test]
fn a_sample_decks_beat_position_uses_the_grid() {
    let mut st = Stepper::new();
    assert!(st.e.deck("A").unwrap().beat_state(SR).is_none());
    st.e.load_demo("A");
    assert_eq!(
        st.e.deck("A")
            .unwrap()
            .sample
            .as_ref()
            .unwrap()
            .grid
            .map(|g| g.bpm),
        Some(120.0)
    );
    st.e.play("A");
    st.step(frames(48_000));
    let s = st.e.deck("A").unwrap().beat_state(SR).unwrap();
    assert!((s.beat - 2.0).abs() < 0.15, "beat {}", s.beat); // minus the motor's spin-up
}

fn sync_pair(bpm: f64) -> Stepper {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(bpm)));
    st.e.load_demo("B"); // 120
    st.step(1);
    st
}

// DeckBeatTests.syncMatchesTempoWithinTheFaderRange
#[test]
fn sync_matches_tempo_within_the_fader_range() {
    let mut st = sync_pair(128.0);
    let r = st.e.sync("B", "A", SyncAlign::TempoOnly).unwrap();
    assert!(!r.tempo_clamped && r.multiplier == 1.0);
    assert!((st.e.effective_bpm("B").unwrap() - 128.0).abs() < 1e-9);
    assert!((r.pitch - (128.0 / 120.0 - 1.0) / PITCH_RANGE).abs() < 1e-9);
}

// DeckBeatTests.syncClampsWhenTheTempiAreTooFarApart
#[test]
fn sync_clamps_when_the_tempi_are_too_far_apart() {
    let mut st = sync_pair(150.0);
    let r = st.e.sync("B", "A", SyncAlign::TempoOnly).unwrap();
    assert!(r.tempo_clamped && (r.pitch - 1.0).abs() < 1e-9);
    assert!((st.e.effective_bpm("B").unwrap() - 120.0 * 1.08).abs() < 1e-9);
}

// DeckBeatTests.syncPicksHalfOrDoubleTimeWhenThatFits
#[test]
fn sync_picks_half_or_double_time_when_that_fits() {
    let mut st = sync_pair(140.0);
    st.e.set_bpm("B", 70.0);
    let r = st.e.sync("B", "A", SyncAlign::TempoOnly).unwrap();
    assert!(
        r.multiplier == 0.5 && !r.tempo_clamped && r.pitch == 0.0,
        "{r:?}"
    );
    assert!((st.e.effective_bpm("B").unwrap() - 70.0).abs() < 1e-9);
}

// DeckBeatTests.syncWithoutAGridDoesNothing
#[test]
fn sync_without_a_grid_does_nothing() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(128.0)));
    assert!(
        st.e.sync("B", "A", SyncAlign::Beat).is_none(),
        "nothing loaded"
    );
    let blank = djrust::deck::turntable::SampleBuffer::new(vec![0.0; 48_000], None, SR);
    let overview = djrust::deck::waveform::WaveformOverview::new(&blank, 1200);
    st.e.load_sample(
        "B",
        djrust::host::deck::sample::PreparedSample {
            title: "x".into(),
            buffer: std::sync::Arc::new(blank),
            overview,
            grid: None,
        },
    );
    assert!(
        st.e.sync("B", "A", SyncAlign::Beat).is_none(),
        "no tempo known yet"
    );
}

// DeckBeatTests.netDecksLockToTheSameBarGrid
#[test]
fn net_decks_lock_to_the_same_bar_grid() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(128.0)));
    st.e.generate("B", &net_spec("house", 2.0, None, Some(124.0)));
    st.e.play("A");
    st.step(frames(17_000)); // A runs ~0.35 s alone
    st.e.play("B");
    st.step(frames(30_000));
    let before = wrapped(beat(&st, "B") - beat(&st, "A"), 4.0);
    assert!(
        before.abs() > 0.3,
        "the decks start out of phase ({before})"
    );

    let r = st.e.sync("B", "A", SyncAlign::Bar).unwrap();
    assert!(!r.tempo_clamped);
    st.step(frames(4096));
    assert!((st.e.effective_bpm("B").unwrap() - 128.0).abs() < 1e-9);
    let after = wrapped(beat(&st, "B") - beat(&st, "A"), 4.0);
    assert!(after.abs() < 0.08, "bar-aligned: off by {after} beats");
    // and it holds: same tempo, so the offset stays put
    st.step(frames(96_000));
    let later = wrapped(beat(&st, "B") - beat(&st, "A"), 4.0);
    assert!(later.abs() < 0.1, "still locked 2 s later ({later})");
}

// DeckBeatTests.aSampleDeckSnapsOntoANetDecksBeat
#[test]
fn a_sample_deck_snaps_onto_a_net_decks_beat() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(126.0)));
    st.e.load_demo("B");
    st.e.play("A");
    st.step(frames(10_000));
    st.e.play("B");
    st.step(frames(21_345)); // arbitrary offset
    st.e.sync("B", "A", SyncAlign::Beat);
    st.step(frames(8192)); // the seek lands
    let off = wrapped(beat(&st, "B") - beat(&st, "A"), 1.0);
    assert!(off.abs() < 0.06, "beat-aligned: off by {off}");
}

// DeckBeatTests.followKeepsTheTempoLocked
#[test]
fn follow_keeps_the_tempo_locked() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(124.0)));
    st.e.load_demo("B");
    st.e.follow("B", Some("A"));
    assert!((st.e.effective_bpm("B").unwrap() - 124.0).abs() < 1e-9);
    st.e.set_pitch("A", 0.5); // +4 %
    st.step(frames(960)); // the housekeeping tick runs
    assert!((st.e.effective_bpm("B").unwrap() - 124.0 * 1.04).abs() < 1e-6);
    st.e.follow("B", None);
    st.e.set_pitch("A", 0.0);
    st.step(frames(960));
    assert!(
        (st.e.effective_bpm("B").unwrap() - 124.0 * 1.04).abs() < 1e-6,
        "let go: it stays where it was"
    );
}

// MARK: QuantizeTests (the demo loop: 120 bpm, a beat every half second)

fn demo(st: &mut Stepper) -> f64 {
    st.e.load_demo("A");
    st.step(1);
    SR * 0.5
}

fn seek(st: &mut Stepper, to: f64) {
    st.e.turntable("A", TurntableCommand::Seek(to));
    st.step(1);
}

// QuantizeTests.hotCuesSnapToTheGrid
#[test]
fn hot_cues_snap_to_the_grid() {
    let mut st = Stepper::new();
    let spb = demo(&mut st);
    seek(&mut st, spb * 3.0 + 4000.0);
    st.e.hot_cue("A", 0);
    assert!(
        (st.e.deck("A").unwrap().hot_cues[&0] - (spb * 3.0 + 4000.0)).abs() < 1.0,
        "off: set where we are"
    );
    st.e.clear_cue("A", 0);
    st.e.set_quantize("A", Quantize::Beat);
    st.e.hot_cue("A", 0);
    assert_eq!(
        st.e.deck("A").unwrap().hot_cues[&0],
        spb * 3.0,
        "beat: snapped to the nearest beat"
    );
    st.e.clear_cue("A", 1);
    st.e.set_quantize("A", Quantize::Bar);
    seek(&mut st, spb * 5.2);
    st.e.hot_cue("A", 1);
    assert_eq!(
        st.e.deck("A").unwrap().hot_cues[&1],
        spb * 4.0,
        "bar: nearest bar line (4 beats)"
    );
}

// QuantizeTests.loopsSnapAndLoopBeatsIsExact
#[test]
fn loops_snap_and_loop_beats_is_exact() {
    let mut st = Stepper::new();
    let spb = demo(&mut st);
    st.e.set_quantize("A", Quantize::Beat);
    seek(&mut st, spb * 2.0 + 3000.0);
    st.e.loop_in("A");
    let s = |st: &Stepper| {
        let d = st.e.deck("A").unwrap().sample.as_ref().unwrap();
        (d.loop_start, d.loop_end, d.looping)
    };
    assert_eq!(s(&st).0, Some(spb * 2.0));
    seek(&mut st, spb * 4.0 + 9000.0);
    st.e.loop_out("A");
    assert!(s(&st).2 && s(&st).1 == Some(spb * 4.0));
    st.e.loop_halve("A");
    assert_eq!(s(&st).1, Some(spb * 3.0), "halving keeps the grid");
    st.e.loop_double("A");
    assert_eq!(s(&st).1, Some(spb * 4.0));
    seek(&mut st, spb * 1.2 + 500.0);
    st.e.loop_beats("A", 4.0);
    assert!(s(&st).0 == Some(spb) && s(&st).1 == Some(spb * 5.0));
}

// QuantizeTests.quantizedJumpsKeepThePhase
#[test]
fn quantized_jumps_keep_the_phase() {
    let mut st = Stepper::new();
    let spb = demo(&mut st);
    st.e.set_quantize("A", Quantize::Beat);
    seek(&mut st, spb * 6.0);
    st.e.hot_cue("A", 0); // cue at beat 6
    st.e.play("A");
    seek(&mut st, spb * 2.0 + 0.2 * spb); // 0.2 beat past a line
    st.e.turntable("A", TurntableCommand::Transport(false)); // freeze the position for a clean read
    st.step(1);
    let here = st.e.deck("A").unwrap().mirror.turntable_position();
    st.e.hot_cue("A", 0);
    st.step(1);
    let there = st.e.deck("A").unwrap().mirror.turntable_position();
    assert!(
        ((there - spb * 6.0) - (here - spb * 2.0)).abs() < spb * 0.02,
        "same offset past the grid line"
    );
}

// The cues are in the platter's own count: past the end of the audio the position keeps
// counting, and a loop set there is in the same count (a wrapped cue would put the loop's start
// behind the playhead).
#[test]
fn cues_and_loops_use_the_unwrapped_platter() {
    let mut st = Stepper::new();
    demo(&mut st);
    let len =
        st.e.deck("A")
            .unwrap()
            .sample
            .as_ref()
            .unwrap()
            .buffer
            .length() as f64;
    seek(&mut st, len * 2.0 + 1000.0);
    st.e.hot_cue("A", 0);
    st.e.loop_in("A");
    let d = st.e.deck("A").unwrap();
    assert!(
        d.hot_cues[&0] > len * 2.0,
        "the cue is where the platter counts: {}",
        d.hot_cues[&0]
    );
    assert!(d.sample.as_ref().unwrap().loop_start.unwrap() > len * 2.0);
}

// Deck.launch: a sample starts at full speed this block, with no motor spin-up
#[test]
fn launch_starts_a_sample_at_full_speed() {
    let mut st = Stepper::new();
    st.e.load_demo("A");
    st.e.load_demo("B");
    st.step(1);
    st.e.launch("A");
    st.e.play("B");
    st.step(1);
    let (a, b) = (st.e.deck("A").unwrap(), st.e.deck("B").unwrap());
    assert!(a.playing && b.playing);
    assert_eq!(a.mirror.turntable_rate(), 1.0, "launched: no spin-up");
    assert!(
        b.mirror.turntable_rate() < 0.1,
        "played: the motor eases in"
    );
}

// MARK: net cues, relocation

// SurfaceNetTests.hotCuesOnANetDeckAreTicks
#[test]
fn hot_cues_on_a_net_deck_are_ticks() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st.step(frames(30_000));
    let t1 = st.e.deck("A").unwrap().mirror.tick();
    assert!(t1 > 20);
    st.e.hot_cue("A", 0);
    assert_eq!(st.e.deck("A").unwrap().hot_cues[&0], t1 as f64);
    assert!(
        st.e.deck("A").unwrap().hot_cue_set(0),
        "the pad lights on a net deck"
    );
    st.step(frames(60_000));
    st.e.hot_cue("A", 0); // jump back
    let mut back = false;
    for _ in 0..40 {
        st.step(frames(4096));
        // swapped in at a bar, then ran on a little
        if (st.e.deck("A").unwrap().mirror.tick() - t1).abs() < 80 {
            back = true;
            break;
        }
    }
    assert!(back);
}

// DJEngine.cue on a net deck: paused it marks the tick; playing it pauses and returns to it.
#[test]
fn cue_on_a_net_deck_marks_a_tick_and_returns_to_it() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st.step(frames(20_000));
    st.e.pause("A");
    st.step(2);
    let t0 = st.e.deck("A").unwrap().mirror.tick();
    st.e.cue("A");
    assert_eq!(st.e.deck("A").unwrap().net_cue_tick, t0 as f64);
    st.e.play("A");
    st.step(frames(30_000));
    assert!(st.e.deck("A").unwrap().mirror.tick() > t0 + 10);
    st.e.cue("A");
    assert!(!st.e.deck("A").unwrap().playing, "CUE while playing pauses");
    st.step(2);
    assert_eq!(
        st.e.deck("A").unwrap().mirror.tick(),
        t0,
        "and went back to the cue"
    );
}

// WaveLiveTests.launchSectionRepositionsTheNet
#[test]
fn launching_a_section_repositions_the_net() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
    let secs = st.e.deck("A").unwrap().sections.clone();
    assert!(secs.len() >= 5 && secs[0].start_tick == 0);
    assert!(secs.iter().map(|s| s.steps).sum::<i64>() > 600);
    assert!(st.e.launch_section("A", 3));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st.step(frames(4096));
    assert!(st.e.deck("A").unwrap().mirror.tick() >= secs[3].start_tick);
    assert_eq!(st.e.current_section("A"), Some(3));
    // a step is clamped to the song
    assert!(st.e.step_section("A", 99));
    // the swap lands at the next bar, and the song's own tempo comes in with it (the 600 above
    // was the runner's, not the deck's fader): a few seconds
    st.step(frames(48_000 * 4));
    assert_eq!(st.e.current_section("A"), Some(secs.len() - 1));
    assert!(!st.e.launch_section("A", secs.len()), "no such section");
}

// DeckBeat.rewind on a net: relocate(toTick: 0), a loop too (a loop has no sections)
#[test]
fn rewind_takes_a_net_loop_back_to_tick_zero() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("house", 5.0, None, None));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st.step(frames(48_000));
    assert!(st.e.deck("A").unwrap().mirror.tick() > 20);
    assert!(st.e.deck("A").unwrap().sections.is_empty());
    assert!(st.e.rewind("A"));
    st.step(frames(48_000));
    assert!(
        st.e.deck("A").unwrap().mirror.tick() < 60,
        "back at the top"
    );
}

// DeckPerform.relocate: a net only
#[test]
fn only_a_net_deck_relocates() {
    let mut st = Stepper::new();
    assert!(!st.e.relocate("A", 10, None, None), "nothing loaded");
    st.e.load_demo("A");
    assert!(!st.e.relocate("A", 10, None, None), "a sample deck");
    assert!(!st.e.launch_section("A", 0));
    assert!(st.e.rewind("A"), "a sample deck rewinds by seeking");
}

// MARK: the tempo fader and the nudge

// DeckNetTests.tempoFaderScalesTheNetClock
#[test]
fn the_tempo_fader_scales_the_net_clock() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, None));
    let base = st.e.deck("A").unwrap().base_tempo;
    st.e.set_pitch("A", 0.5);
    st.e.play("A");
    st.step(frames(4096));
    let tempo = st.e.deck("A").unwrap().mirror.tempo();
    assert!(
        (tempo - base * (1.0 + 0.5 * PITCH_RANGE)).abs() < 1e-9,
        "{tempo} vs {base}"
    );
}

// DJEngine.applyNetTempo: the fader and the nudge multiply, and stay through a new track
#[test]
fn the_fader_and_the_nudge_multiply_and_a_new_track_keeps_them() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 1.0, None, Some(128.0)));
    st.e.set_pitch("A", 0.5);
    st.e.nudge("A", 0.03);
    st.step(2);
    let d = st.e.deck("A").unwrap();
    assert!((d.mirror.tempo() - 128.0 * 1.04 * 1.03).abs() < 1e-9);
    assert!((d.effective_bpm().unwrap() - 128.0 * 1.04 * 1.03).abs() < 1e-9);
    // a stopped deck takes a new track at once: the fader and the ring are still on it
    st.e.generate("A", &net_spec("house", 3.0, None, Some(120.0)));
    st.step(2);
    let d = st.e.deck("A").unwrap();
    assert_eq!(d.base_tempo, 120.0);
    assert!(
        (d.mirror.tempo() - 120.0 * 1.04 * 1.03).abs() < 1e-9,
        "{}",
        d.mirror.tempo()
    );
    // a SYNC takes the ring off
    st.e.load_demo("B");
    st.step(1);
    st.e.sync("A", "B", SyncAlign::TempoOnly);
    assert_eq!(st.e.deck("A").unwrap().net_nudge, 0.0);
}

// MARK: controllers

fn rig_with(f: impl FnOnce(&mut djrust::host::deck::Engine)) -> (Rig, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    f(&mut e);
    (Rig::new(e), r)
}

fn send(rig: &mut Rig, r: &mut Renderer, port: &str, bytes: &[u8], now: &mut f64) {
    *now += 0.01;
    assert!(rig.route(port, bytes, *now).is_some());
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    r.render_block(&mut l, &mut rr);
    rig.tick(*now);
}

// SurfaceTests.pitchFaderHasCentreDetent
#[test]
fn the_pitch_fader_has_a_centre_detent() {
    let (mut rig, mut r) = rig_with(|e| {
        e.load_demo("A");
    });
    assert!(rig.attach(INPULSE));
    let mut t = 0.0;
    send(&mut rig, &mut r, INPULSE, &[0xB1, 0x08, 64], &mut t); // 64/127 is just off centre
    send(&mut rig, &mut r, INPULSE, &[0xB1, 0x28, 0], &mut t);
    assert_eq!(rig.engine.deck("A").unwrap().pitch, 0.0);
    send(&mut rig, &mut r, INPULSE, &[0xB1, 0x08, 127], &mut t);
    send(&mut rig, &mut r, INPULSE, &[0xB1, 0x28, 127], &mut t);
    assert_eq!(rig.engine.deck("A").unwrap().pitch, 1.0);
}

// SurfaceNetTests.mpkPadsTurnTheAimedDecksPartsOffAndOnAndTheJoystickNudgesTempo (the joystick)
#[test]
fn the_joystick_nudges_the_tempo_by_up_to_eight_percent() {
    let (mut rig, mut r) = rig_with(|e| {
        e.generate("A", &net_spec("techno", 42.0, None, None));
        e.play("A");
    });
    assert!(rig.attach(MPK));
    let mut t = 0.0;
    for _ in 0..16 {
        send(&mut rig, &mut r, MPK, &[0x80, 0, 0], &mut t);
    }
    let base = rig.engine.deck("A").unwrap().base_tempo;
    send(&mut rig, &mut r, MPK, &[0xE0, 0x7F, 0x7F], &mut t); // joystick full right
    for _ in 0..16 {
        send(&mut rig, &mut r, MPK, &[0x80, 0, 0], &mut t);
    }
    let d = rig.engine.deck("A").unwrap();
    assert!(
        (d.mirror.tempo() - base * (1.0 + PITCH_RANGE)).abs() < 1e-6,
        "{}",
        d.mirror.tempo()
    );
    send(&mut rig, &mut r, MPK, &[0xE0, 0x00, 0x40], &mut t); // centre
    for _ in 0..16 {
        send(&mut rig, &mut r, MPK, &[0x80, 0, 0], &mut t);
    }
    assert!((rig.engine.deck("A").unwrap().mirror.tempo() - base).abs() < 0.05);
}

// SurfaceTests.hotCueSetsThenClears (a sample deck), and the same pads on a net deck
#[test]
fn the_hot_cue_pads_set_then_clear_on_either_kind_of_deck() {
    let (mut rig, mut r) = rig_with(|e| {
        e.load_demo("A");
        e.generate("B", &net_spec("techno", 42.0, None, None));
    });
    assert!(rig.attach(INPULSE));
    let mut t = 0.0;
    // deck A pad 1 (HOT CUE mode): empty, so set here; SHIFT + pad 1 clears it
    send(&mut rig, &mut r, INPULSE, &[0x96, 0x00, 0x7F], &mut t);
    assert!(rig.engine.deck("A").unwrap().hot_cues.contains_key(&0));
    send(&mut rig, &mut r, INPULSE, &[0x96, 0x08, 0x7F], &mut t);
    assert!(!rig.engine.deck("A").unwrap().hot_cues.contains_key(&0));
    // deck B (channel 8 for its pads): the same, in ticks
    send(&mut rig, &mut r, INPULSE, &[0x97, 0x00, 0x7F], &mut t);
    assert!(
        rig.engine.deck("B").unwrap().hot_cues.contains_key(&0),
        "a net deck sets a cue too"
    );
    send(&mut rig, &mut r, INPULSE, &[0x97, 0x08, 0x7F], &mut t);
    assert!(!rig.engine.deck("B").unwrap().hot_cues.contains_key(&0));
    assert!(
        !rig.engine.unhandled.iter().any(|u| u.contains("HotCue")),
        "{:?}",
        rig.engine.unhandled
    );
}

// SurfaceBeatTests.inpulseSyncRewindAndLoadButtons
#[test]
fn the_inpulse_sync_and_rewind_buttons() {
    let (mut rig, mut r) = rig_with(|e| {
        e.generate("A", &net_spec("techno", 1.0, None, Some(127.0)));
        e.load_demo("B");
    });
    assert!(rig.attach(INPULSE));
    let prof = inpulse_200_mk3();
    let mut t = 0.0;
    // deck B (channel 2): SYNC is deckB.buttons[2]
    let sync_b = prof.number("deckB.buttons", 2).unwrap();
    send(&mut rig, &mut r, INPULSE, &[0x92, sync_b, 0x7F], &mut t);
    assert!((rig.engine.effective_bpm("B").unwrap() - 127.0).abs() < 1e-9);
    let leds = prof.leds.as_ref().unwrap();
    let frames = rig.led_frames(t + 0.01);
    let lit: [u8; 3] = [0x92, sync_b, leds.single_on];
    assert!(
        frames.iter().any(|(_, f)| f.contains(&lit)),
        "SYNC lights while the tempi match: {frames:?}"
    );
    // shift + SYNC = bar sync
    let shift_sync_b = prof.number("deckB.shiftButtons", 0).unwrap();
    rig.engine.set_pitch("B", 0.0);
    send(
        &mut rig,
        &mut r,
        INPULSE,
        &[0x95, shift_sync_b, 0x7F],
        &mut t,
    );
    assert!((rig.engine.effective_bpm("B").unwrap() - 127.0).abs() < 1e-9);
    // rewind (shift + CUE)
    rig.engine.turntable("B", TurntableCommand::Seek(30_000.0));
    let shift_cue_b = prof.number("deckB.shiftButtons", 1).unwrap();
    send(
        &mut rig,
        &mut r,
        INPULSE,
        &[0x95, shift_cue_b, 0x7F],
        &mut t,
    );
    send(
        &mut rig,
        &mut r,
        INPULSE,
        &[0x95, shift_cue_b, 0x00],
        &mut t,
    );
    assert!(rig.engine.deck("B").unwrap().mirror.turntable_position() < 1000.0);
}

// SurfaceBeatTests.apcScenesAimPlayAndAutoNext, SurfaceTests.playTogglesOnPressOnly
#[test]
fn the_apc_scenes_aim_and_play_toggles_on_press_only() {
    let (mut rig, mut r) = rig_with(|e| {
        e.load_demo("A");
        e.load_demo("B");
    });
    assert!(rig.attach(APC));
    let mut t = 0.0;
    send(&mut rig, &mut r, APC, &[0x90, 0x70, 0x7F], &mut t); // play: the aimed deck, A
    send(&mut rig, &mut r, APC, &[0x90, 0x70, 0x00], &mut t); // a release does nothing
    let (a, b) = (rig.engine.deck("A").unwrap(), rig.engine.deck("B").unwrap());
    assert!(a.playing && !b.playing);
    // the main thread's flag is the deck's, at once: two presses in a block are two toggles
    rig.route(APC, &[0x90, 0x70, 0x7F], 0.5);
    rig.route(APC, &[0x90, 0x70, 0x7F], 0.51);
    assert!(rig.engine.deck("A").unwrap().playing);
    send(&mut rig, &mut r, APC, &[0x90, 0x74, 0x7F], &mut t); // aim at B
    send(&mut rig, &mut r, APC, &[0x90, 0x70, 0x7F], &mut t);
    assert!(rig.engine.deck("B").unwrap().playing);
    send(&mut rig, &mut r, APC, &[0x90, 0x72, 0x7F], &mut t); // auto-next on B
    let (a, b) = (rig.engine.deck("A").unwrap(), rig.engine.deck("B").unwrap());
    assert!(b.auto_advance && !a.auto_advance);
    let frames = rig.led_frames(t + 0.01);
    assert!(
        frames.iter().any(|(_, f)| f.contains(&[0x90, 0x72, 1])),
        "{frames:?}"
    );
}

// MARK: unhandled

#[test]
fn what_a_net_deck_does_with_the_beat_actions_is_not_unhandled() {
    use djrust::control::controller::Action;
    let (mut rig, _r) = rig_with(|e| {
        e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
        e.load_demo("B");
    });
    for a in [
        Action::Cue { deck: "A".into() },
        Action::HotCue {
            deck: "A".into(),
            slot: 0,
        },
        Action::ClearCue {
            deck: "A".into(),
            slot: 0,
        },
        Action::LoopIn { deck: "A".into() }, // a no-op on a net, as in Swift
        Action::LoopOut { deck: "A".into() },
        Action::LoopHalve { deck: "A".into() },
        Action::LoopDouble { deck: "A".into() },
        Action::Sync {
            deck: "B".into(),
            bar: false,
        },
        Action::QuantizeCycle { deck: "B".into() },
        Action::Rewind { deck: "A".into() },
    ] {
        SurfaceHost::perform(&mut rig.engine, a);
    }
    assert!(
        rig.engine.unhandled.is_empty(),
        "{:?}",
        rig.engine.unhandled
    );
    assert_eq!(rig.engine.deck("B").unwrap().quantize, Quantize::Beat);
    assert!(
        !rig.engine.deck("B").unwrap().quantize_off(),
        "the QUANTIZE light"
    );
    SurfaceHost::perform(&mut rig.engine, Action::QuantizeCycle { deck: "B".into() });
    SurfaceHost::perform(&mut rig.engine, Action::QuantizeCycle { deck: "B".into() });
    assert!(
        rig.engine.deck("B").unwrap().quantize_off(),
        "off, beat, bar, off"
    );
    // nothing is left for `unhandled` (the walk is done: `deck_wander`; so is auto-mix: the
    // controller's press reaches `set_auto_mix` with the settings it has, `deck_automix`)
    SurfaceHost::perform(&mut rig.engine, Action::AutoMixSet { on: true });
    assert!(rig.engine.auto_mix.settings.on);
    SurfaceHost::perform(&mut rig.engine, Action::AutoMixNow);
    assert!(
        rig.engine.unhandled.is_empty(),
        "{:?}",
        rig.engine.unhandled
    );
}

// MARK: views

fn views_engine() -> Stepper {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
    st
}

// NetSnapshotTests.ringsCarryTheirHitsAndRoles
#[test]
fn a_ring_snapshot_carries_the_rings_hits_and_roles() {
    let mut st = Stepper::new();
    st.e.generate("A", &net_spec("techno", 42.0, None, None));
    let snap = st.e.net_snapshot("A").unwrap();
    assert_eq!(snap.tick, 0);
    let role = |r: &str| snap.rings.iter().find(|x| x.role == r);
    for r in [
        "kick", "snare", "hihat", "bass", "melody", "harmony", "hit1", "hit4",
    ] {
        assert!(
            role(r).is_some(),
            "{r} in {:?}",
            snap.rings.iter().map(|x| &x.role).collect::<Vec<_>>()
        );
    }
    let kick = role("kick").unwrap();
    assert!(
        kick.size == 16 && kick.hits.iter().filter(|h| **h).count() == 4,
        "techno kick: 4 on the floor"
    );
    assert!(kick.token == 0 && kick.last_fired == -1 && !kick.muted);
    assert_eq!(role("harmony").unwrap().size, 64);
    assert!(role("hit1").unwrap().muted, "stingers start muted");
    assert!(kick
        .hits
        .iter()
        .zip(&kick.velocities)
        .all(|(h, v)| if *h { *v > 0 } else { *v == 0 }));
}

// NetSnapshotTests.songsShowTheAudibleSlotOfEachRole
#[test]
fn a_snapshot_shows_the_audible_slot_of_each_role() {
    let mut st = views_engine();
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st.step(frames(4096));
    let snap = st.e.net_snapshot("A").unwrap();
    let bass = snap.rings.iter().find(|r| r.role == "bass").unwrap();
    assert!(bass.id.starts_with("bass-"), "a slot net: {}", bass.id);
    st.e.set_role_muted("A", "bass", true);
    st.step(frames(2048));
    assert!(
        st.e.net_snapshot("A")
            .unwrap()
            .rings
            .iter()
            .find(|r| r.role == "bass")
            .unwrap()
            .muted
    );
}

// NetSnapshotTests.tokensAreWhereTheExecutorPutThem
#[test]
fn a_ring_snapshots_tokens_are_where_the_executor_put_them() {
    let mut st = Stepper::new();
    let spec = net_spec("techno", 42.0, None, Some(480.0));
    st.e.generate("A", &spec);
    let mut g = djrust::wave::WaveGraph::new(&spec.project());
    st.e.play("A");
    let (mut checked, mut last) = (0, -1);
    for _ in 0..400 {
        st.step(1);
        let snap = st.e.net_snapshot("A").unwrap();
        while g.tick < snap.tick {
            g.advance();
        }
        if snap.tick == last || g.tick != snap.tick {
            continue;
        }
        last = snap.tick;
        for ring in &snap.rings {
            let net = &g.nets[g.by_id[&ring.id]];
            let token = (0..net.n_places).find(|&p| net.state[p] == 1.0);
            assert_eq!(token, Some(ring.token), "{} @tick {}", ring.id, snap.tick);
            checked += 1;
        }
    }
    assert!(checked > 200, "{checked}");
}

// NetSnapshotTests.sampleDecksHaveNoNetView
#[test]
fn a_sample_deck_has_no_net_view() {
    let mut st = Stepper::new();
    st.e.load_demo("A");
    assert!(st.e.net_snapshot("A").is_none() && st.e.section_timeline("A").is_none());
}

// DeckOverlayTests.overlayPlacesPlayheadBarsCuesAndLoop
#[test]
fn the_overlay_places_the_playhead_bars_cues_and_loop() {
    let mut st = Stepper::new();
    assert!(st.e.waveform_overlay("A").is_none());
    st.e.load_demo("A"); // 8 beats at 120 bpm: two bars, offset 0
    st.step(1);
    let len =
        st.e.deck("A")
            .unwrap()
            .sample
            .as_ref()
            .unwrap()
            .buffer
            .length() as f64;
    seek(&mut st, len * 0.25);
    st.e.hot_cue("A", 1);
    st.e.loop_in("A");
    seek(&mut st, len * 0.75);
    st.e.loop_out("A");
    let o = st.e.waveform_overlay("A").unwrap();
    assert!((o.playhead - 0.75).abs() < 1e-3);
    assert_eq!(o.bar_lines, vec![0.0, 0.5]);
    assert!((o.cues[&1] - 0.25).abs() < 1e-3);
    let (lo, hi) = o.loop_range.unwrap();
    assert!((lo - 0.25).abs() < 1e-3 && (hi - 0.75).abs() < 1e-3);
    assert!(o.loop_active);
}

// SectionTimelineTests.segmentsCoverTheSongAndFollowTheClock
#[test]
fn the_section_timeline_covers_the_song_and_follows_the_clock() {
    let mut st = Stepper::new();
    assert!(st.e.section_timeline("A").is_none());
    st.e.generate("A", &net_spec("techno", 42.0, Some("standard"), None));
    let t0 = st.e.section_timeline("A").unwrap();
    let n = st.e.deck("A").unwrap().sections.len();
    assert!(t0.segments.len() == n && t0.segments[0].start == 0.0);
    assert!((t0.segments.iter().map(|s| s.width).sum::<f64>() - 1.0).abs() < 1e-9);
    for w in t0.segments.windows(2) {
        assert!(
            (w[1].start - (w[0].start + w[0].width)).abs() < 1e-9,
            "contiguous"
        );
    }
    assert!(t0.segments[0].is_current && t0.playhead == 0.0);
    st.e.set_tempo("A", 900.0);
    st.e.play("A");
    st.step(frames(48_000 * 4));
    let t1 = st.e.section_timeline("A").unwrap();
    assert!(t1.playhead > 0.0 && t1.playhead < 1.0);
    let cur = t1.segments.iter().position(|s| s.is_current).unwrap();
    assert!(
        t1.segments[cur].start <= t1.playhead
            && t1.playhead < t1.segments[cur].start + t1.segments[cur].width + 1e-9
    );
    assert_eq!(t1.segments.iter().filter(|s| s.is_current).count(), 1);
}
