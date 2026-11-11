//! Swift tests of the loop row and the loop channel that had no Rust counterpart, for behaviour
//! the engine already has (package F of the parity audit). Each test names the Swift test whose
//! literals it carries; the audio runs on the offline engine of `common_row` (512-frame blocks,
//! the takes serviced between them, the strips at unity).

mod common_row;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common_row::*;
use djrust::deck::clips::LoopFade;
use djrust::deck::turntable::SampleBuffer;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::model::{
    Session, SessionEventKind, DEMO_ID, LEGACY_LOOP_GAIN,
};
use djrust::session::player::{Conductor, SessionPlayer};
use djrust::host::deck::loops::LOOP_CHANNEL_GAIN;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, PerformCtx};
use djrust::host::replay::{session_engine, RenderOptions};

/// A sample whose every value says where it is: `left[i] = i / length` (LoopRecordTests.ramp).
fn ramp(bars: usize) -> PreparedSample {
    let n = bars * BAR as usize;
    let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
    let right: Vec<f32> = (0..n).map(|i| -(i as f32) / n as f32).collect();
    PreparedSample::with_grid(Arc::new(SampleBuffer::new(left, Some(right), SR)), "ramp", None)
}

fn sample_state(e: &Engine) -> (bool, Option<f64>, Option<f64>) {
    let s = e.deck("A").unwrap().sample.as_ref().unwrap();
    (s.looping, s.loop_start, s.loop_end)
}

// LoopRecordTests.aNewSampleForgetsTheOldLoopRegion: a deck that looped something (a clip moved
// onto it loops its whole length) and then loads a longer sample plays that sample to its end.
// (Swift sets the loop with `setLoop(0, bar / 2)`; the engine's public way in is LOOP IN at the
// top and LOOP OUT half a bar on, which closes it a few thousand frames short of half a bar
// because the platter spins up first. Either way the loop is well inside the first bar.)
#[test]
fn a_new_sample_forgets_the_old_loop_region() {
    let mut rig = Rig::new();
    assert!(rig.e.load_sample("A", ramp(1)));
    rig.e.loop_in("A"); // at the top, before the motor starts
    rig.e.play("A");
    rig.run_to(BAR / 2);
    rig.e.loop_out("A");
    let (looping, start, end) = sample_state(&rig.e);
    assert!(looping && start == Some(0.0), "the half-bar loop is set: {looping} {start:?}");
    let end = end.unwrap();
    assert!(end > BAR as f64 / 4.0 && end <= BAR as f64 / 2.0, "{end}");
    rig.run_to(BAR);
    assert!(
        rig.e.sample_position("A").unwrap() < end + BLOCK as f64,
        "the loop holds the playhead for the whole bar"
    );

    assert!(rig.e.load_sample("A", ramp(4)));
    let (looping, start, end) = sample_state(&rig.e);
    assert!(!looping && start.is_none() && end.is_none(), "{looping} {start:?} {end:?}");
    rig.run_to(rig.frame() + 3 * BAR);
    let p = rig.e.sample_position("A").unwrap();
    assert!(p > BAR as f64, "past the old loop's end: {p}");
}

// LoopRecordTests.theCaptureTapIsOnAndBeforeTheFader: the take is pre-fader, so a closed fader
// still records, and the master is silent meanwhile.
#[test]
fn the_capture_tap_is_on_and_before_the_fader() {
    let mut rig = Rig::new();
    assert!(rig.e.load_demo("A"));
    rig.e.set_volume("A", 0.0); // the take is pre-fader: a closed fader still records
    rig.e.play("A");
    rig.run_to(48_000);
    let m = &rig.e.deck("A").unwrap().mirror;
    let (_, end) = m.capture.span().expect("the tap is written");
    assert_eq!(end, m.rendered_frames.load(Ordering::Acquire), "the tap's end is the deck's rendered frames");
    let (mut l, mut r) = (Vec::new(), Vec::new());
    m.capture.copy(24_000, 48_000, &mut l, &mut r);
    assert!(l.len() == 24_000 && l.iter().any(|v| *v != 0.0), "{} frames", l.len());
    assert!(
        rig.master[24_000..].iter().all(|v| *v == 0.0),
        "the fader is closed: the master is silent"
    );
}

fn quiet_engine() -> Engine {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    engine(c).0
}

fn action_events(s: &Session) -> Vec<DeckAction> {
    s.events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

// LoopSoundTests.theRowsFadeAndKeyLockStartARecordedSession: at the defaults a session has
// nothing to say first; with the row's fade and key lock moved, it says them, in that order, so
// a replay from a fresh engine fades and locks as the set did.
#[test]
fn the_rows_fade_and_key_lock_start_a_recorded_session() {
    let mut e = quiet_engine();
    e.start_session().unwrap();
    assert!(e.finish_session().unwrap().events.is_empty(), "at the defaults there is nothing to say");

    e.perform(&DeckAction::LoopFade { mode: "bar".into() }, PerformCtx::default());
    e.perform(&DeckAction::LoopKeyLock { on: true }, PerformCtx::default());
    e.start_session().unwrap();
    let events = action_events(&e.finish_session().unwrap());
    assert_eq!(
        events,
        [DeckAction::LoopFade { mode: "bar".into() }, DeckAction::LoopKeyLock { on: true }]
    );

    let mut fresh = quiet_engine();
    assert!(fresh.loop_fade() != LoopFade::Bar && !fresh.loop_key_lock());
    for a in &events {
        fresh.perform(a, PerformCtx::default());
    }
    assert!(fresh.loop_fade() == LoopFade::Bar && fresh.loop_key_lock());
}

// LoopCaptureTests.aReplayedControllerTapTakesTheLengthItWasRecordedWith: the tap length is part
// of a session's starting state, then changes are recorded as they happen; a replay starts from
// a fresh engine (tap length 2), the events set it as it was live, and the replayed MIDI tap
// uses it.
#[test]
fn a_replayed_controller_tap_takes_the_length_it_was_recorded_with() {
    const INPULSE: &str = "DJControl Inpulse 200 Mk3";
    let tap = |bars| DeckAction::TapBars { bars: Some(bars) };
    // live: the app's tap length is 4, set before the recording starts
    let mut live = quiet_engine();
    live.perform(&tap(4), PerformCtx::default());
    live.start_session().unwrap();
    live.perform(&tap(8), PerformCtx::default()); // and changed during the set
    let events = action_events(&live.finish_session().unwrap());
    assert_eq!(events, [tap(4), tap(8)]);

    // a replay starts from a fresh engine (tap length 2): the events set it as it was live
    let mut fresh = quiet_engine();
    assert_eq!(fresh.loops.tap_take_bars, Some(2));
    fresh.perform(&events[0], PerformCtx::default());
    assert_eq!(fresh.loops.tap_take_bars, Some(4), "the controller's tap at the start records 4 bars");
    fresh.perform(&events[1], PerformCtx::default());
    assert_eq!(fresh.loops.tap_take_bars, Some(8));

    // and the replayed MIDI tap uses it
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.e.perform(&events[0], PerformCtx::default());
    rig.run_to(BAR + BAR / 3);
    let Rig { e, mut r, .. } = rig;
    let mut c = djrust::host::deck::Rig::new(e);
    assert!(c.attach(INPULSE));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut render = |c: &mut djrust::host::deck::Rig, frames: i64| {
        let to = c.engine.loop_frames() + frames;
        while c.engine.loop_frames() < to {
            r.render_block(&mut l, &mut rr);
            c.engine.service_loops();
        }
    };
    c.route(INPULSE, &[0x91, 0x03, 0x7F], 0.01); // deck A LOOP down
    render(&mut c, 2048);
    c.route(INPULSE, &[0x91, 0x03, 0x00], 0.02); // and up: a tap
    assert_eq!(c.engine.loop_take_progress("A").map(|p| p.0), Some(4));
    for _ in 0..4000 {
        if c.engine.loops.row.clips.is_empty() {
            render(&mut c, BLOCK as i64);
        }
    }
    assert_eq!(c.engine.loops.row.clips[0].bars(), 4);
}

// LoopChannelLevelTests.aSessionRecordedBeforeTheChangeKeepsTheOldTrim: a session written before
// L's trim was recorded has no `mixer.loopGain`: it decodes to the unity it was played at,
// re-encodes to the same text and CID, and replays L at that trim.
#[test]
fn a_session_recorded_before_the_change_keeps_the_old_trim() {
    let old = concat!(
        r#"{"decks":{"A":{"params":{"autoAdvance":false,"eqHigh":0.5,"eqLow":0.5,"eqMid":0.5,"#,
        r#""filter":0.5,"gain":0.5,"pitch":0,"quantize":"off","volume":0.8},"source":{"kind":"none"}}},"#,
        r#""events":[],"frames":4410,"mixer":{"crossfader":0.5,"master":0.8},"sampleRate":22050,"#,
        r#""schema":"djrust/session/v1"}"#
    );
    let s = Session::decode(old, None).unwrap();
    assert!(s.loop_gain == LEGACY_LOOP_GAIN && s.loop_gain == 0.5);
    assert!(s.json().get("mixer").unwrap().get("loopGain").is_none());
    assert_eq!(s.canonical(), JsonValue::parse(old).unwrap().canonical_js());
    assert_eq!(Session::decode(&s.canonical(), Some(&s.cid())).unwrap(), s);
    let replayed = replayed_loop_gains(&s);
    assert!(!replayed.is_empty() && replayed.iter().all(|g| *g == 0.5), "{replayed:?}");
}

/// `SessionPlayer.render` with `onBlock`: the loop channel's trim knob at every block.
fn replayed_loop_gains(s: &Session) -> Vec<f64> {
    let mut c = session_engine(s, &RenderOptions::default()).unwrap();
    let player = SessionPlayer::new(s);
    let mut gains = Vec::new();
    let mut next = 0;
    let mut frames = 0usize;
    while (frames as i64) < s.frames {
        let frame = c.frame();
        for ev in player.due(&mut next, frame) {
            c.apply(ev);
        }
        let (l, _) = c.step();
        if l.is_empty() {
            break;
        }
        frames += l.len();
        gains.push(c.rig.engine.loops.gain);
    }
    gains
}

// LoopChannelLevelTests.aNewSessionRecordsTheLoopChannelsTrim: a new recording carries L's trim,
// on the wire and back through a replay; a trim the user moved records as it was.
#[test]
fn a_new_session_records_the_loop_channels_trim() {
    let mut c = djrust::host::replay::offline_conductor(vec!["A".into()], 22_050.0);
    // a few blocks long: a session's length is the frames the engine rendered while it was
    // recording, and a zero-frame session replays no block to read L's trim from
    c.rig.engine.start_session().unwrap();
    for _ in 0..8 {
        c.step_with(|_| {});
    }
    let s = c.rig.engine.finish_session().unwrap();
    assert!(s.frames > 0);
    assert_eq!(s.loop_gain, LOOP_CHANNEL_GAIN);
    let wire = s.json().get("mixer").unwrap().get("loopGain").and_then(|v| v.as_f64());
    assert_eq!(wire, Some(LOOP_CHANNEL_GAIN));
    let back = Session::decode(&s.canonical(), Some(&s.cid())).unwrap();
    assert_eq!(back.loop_gain, LOOP_CHANNEL_GAIN);

    // a trim the user moved records as it was
    c.rig.engine.set_loop_gain(0.7);
    c.rig.engine.start_session().unwrap();
    assert_eq!(c.rig.engine.finish_session().unwrap().loop_gain, 0.7);

    let replayed = replayed_loop_gains(&back);
    assert!(
        !replayed.is_empty() && replayed.iter().all(|g| *g == LOOP_CHANNEL_GAIN),
        "{replayed:?}"
    );
    let _ = DEMO_ID;
}
