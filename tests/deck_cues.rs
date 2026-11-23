//! Beyond Swift: the beat jump on either kind of deck, and the loop buttons on a net deck
//! (`Engine::beat_jump`, `Engine::loop_beats` and the rest of `cues.rs`; the window the render
//! thread wraps in, `WaveRunner::net_loop_op`). A net deck's place is a song step: step `k` (a
//! beat is 4, a bar 16) is the one it plays when its tick goes from `k` to `k + 1`, so bar `b`
//! beat `n` (both from 1) is step `16 (b - 1) + 4 (n - 1)`.

mod common_beat;

use common_beat::{Stepper, BLOCK, SR};
use djrust::deck::beat::Quantize;
use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::deck::waveform::WaveformOverview;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{Applied, PerformCtx};
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::arrangement::ArrangeOp;
use djrust::session::model::{Session, SessionEventKind};
use djrust::wave::{EventCursor, FireEvent, TickAnchor, TickAnchorFlags};
use std::sync::atomic::Ordering;
use djrust::TrackSpec;

/// A techno song at 480 bpm: a step is 1500 frames at 48 kHz, a bar 24 000.
fn fast(seed: f64) -> TrackSpec {
    let mut s = TrackSpec::new("techno", seed, Some("standard"));
    s.extra.set("bpm", JsonValue::Number(480.0));
    s
}

const STEP: usize = 1500;

/// Blocks for `n` steps of the fast song.
fn steps(n: usize) -> usize {
    (n * STEP).div_ceil(BLOCK)
}

fn tick(st: &Stepper, deck: &str) -> i64 {
    st.e.deck(deck).unwrap().mirror.tick()
}

/// Where a playing net deck is, in steps: the step sounding and how far into it.
fn place(st: &Stepper, deck: &str) -> f64 {
    let m = &st.e.deck(deck).unwrap().mirror;
    (m.tick() - 1) as f64 + m.tick_phase()
}

/// Bar and beat (from 1) of a step.
fn bar_beat(step: i64) -> (i64, i64) {
    (step / 16 + 1, step % 16 / 4 + 1)
}

/// A reader of a deck's events from now on: its cursor, and the frame it was taken at (a fresh
/// cursor hands back the newest anchor already written too, which is not "from now on").
struct Tap {
    cursor: EventCursor,
    from: i64,
}

fn tap(st: &Stepper, deck: &str) -> Tap {
    let d = st.e.deck(deck).unwrap();
    Tap {
        cursor: st.e.event_cursor_now(deck),
        from: d.mirror.rendered_frames.load(Ordering::Acquire),
    }
}

fn read(st: &Stepper, deck: &str, tap: &mut Tap) -> (Vec<FireEvent>, Vec<TickAnchor>) {
    let (mut f, mut a) = (Vec::new(), Vec::new());
    let lost = st.e.read_events(deck, &mut tap.cursor, &mut f, &mut a);
    assert_eq!(lost, (0, 0));
    a.retain(|x| x.frame >= tap.from);
    assert!(f.iter().all(|x| x.frame >= tap.from));
    (f, a)
}

/// What a fire played: the step (as the tick after it), the net, the transition, the note and
/// the velocity.
type Hit = (i32, i32, i32, i16, u8);

fn hit(f: &FireEvent) -> Hit {
    (f.tick, f.net, f.transition, f.note, f.velocity)
}

/// The frames at which a loop `start..end` took the clock back to its start (an anchor after
/// the start step that does not continue the one before), and the fires of each whole pass
/// between two of them.
fn passes(fires: &[FireEvent], anchors: &[TickAnchor], start: i64) -> (Vec<i64>, Vec<Vec<Hit>>) {
    let wraps: Vec<i64> = anchors
        .iter()
        .filter(|a| {
            a.tick as i64 == start + 1 && a.flags.contains(TickAnchorFlags::DISCONTINUITY)
        })
        .map(|a| a.frame)
        .collect();
    let whole = wraps
        .windows(2)
        .map(|w| {
            fires
                .iter()
                .filter(|f| f.frame >= w[0] && f.frame < w[1])
                .map(hit)
                .collect()
        })
        .collect();
    (wraps, whole)
}

// MARK: the beat jump

#[test]
fn a_stopped_net_deck_jumps_whole_beats_from_the_step_it_starts_on() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    let at = |st: &mut Stepper, step: i64| {
        assert!(st.e.relocate("A", step, None, None), "stopped: at once");
        st.step(1);
        assert_eq!(tick(st, "A"), step);
    };
    at(&mut st, 32);
    assert_eq!(bar_beat(32), (3, 1));
    st.e.beat_jump("A", 8.0).unwrap();
    st.step(1);
    assert_eq!(bar_beat(tick(&st, "A")), (5, 1));
    at(&mut st, 32);
    st.e.beat_jump("A", -4.0).unwrap();
    st.step(1);
    assert_eq!(bar_beat(tick(&st, "A")), (2, 1));
    // a step past a beat stays a step past one
    at(&mut st, 17);
    st.e.beat_jump("A", 1.0).unwrap();
    st.step(1);
    assert_eq!(tick(&st, "A"), 21);
    // a net deck moves in steps (quarter beats), not past the song's end; back past the top
    // lands on it
    assert!(st.e.beat_jump("A", 0.3).is_err());
    let length: i64 = st.e.deck("A").unwrap().sections.iter().map(|s| s.steps).sum();
    assert!(length > 600);
    assert!(st.e.beat_jump("A", (length / 4) as f64).is_err());
    assert_eq!(tick(&st, "A"), 21, "a refused jump moves nothing");
    st.e.beat_jump("A", -64.0).unwrap();
    st.step(1);
    assert_eq!(tick(&st, "A"), 0);
    assert!(st.e.beat_jump("C", 1.0).is_err(), "no such deck");
}

#[test]
fn a_playing_net_deck_jumps_on_the_next_bar_line_in_its_place_in_the_beat() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    assert!(st.e.generate("B", &fast(42.0)));
    st.e.play("A");
    st.e.play("B");
    st.step(steps(37));
    let t = tick(&st, "A");
    assert!((33..44).contains(&t), "in bar 3: {t}");
    assert!((place(&st, "A") - place(&st, "B")).abs() < 1e-9, "twins");
    let (mut ca, mut cb) = (tap(&st, "A"), tap(&st, "B"));
    st.e.beat_jump("A", 8.0).unwrap();
    st.step(steps(24));
    // it lands on the bar line: where B plays bar 4 beat 1, A plays bar 6 beat 1
    let (_, a) = read(&st, "A", &mut ca);
    let (_, b) = read(&st, "B", &mut cb);
    let swap = a
        .iter()
        .position(|x| x.flags.contains(TickAnchorFlags::SWAP))
        .expect("the relocation swapped in");
    let landed = a[swap + 1];
    let twin = b.iter().find(|x| x.frame == landed.frame).unwrap();
    assert_eq!(bar_beat(twin.tick as i64 - 1), (4, 1));
    assert_eq!(bar_beat(landed.tick as i64 - 1), (6, 1));
    let d = place(&st, "A") - place(&st, "B");
    assert!((d - 32.0).abs() < 1e-3, "8 beats ahead of its twin, in phase: {d}");
    st.e.beat_jump("A", -4.0).unwrap();
    st.step(steps(24));
    let d = place(&st, "A") - place(&st, "B");
    assert!((d - 16.0).abs() < 1e-3, "then 4 beats back: {d}");
}

#[test]
fn a_sample_deck_jumps_by_its_grids_beat_and_a_loop_goes_with_it() {
    let mut st = Stepper::new();
    st.e.load_demo("A"); // 120 bpm: a beat is 24 000 frames
    st.step(1);
    let spb = SR * 0.5;
    let pos = |st: &Stepper| st.e.deck("A").unwrap().mirror.turntable_position();
    st.e.turntable("A", TurntableCommand::Seek(spb * 3.0 + 1000.0));
    st.step(1);
    st.e.beat_jump("A", 2.0).unwrap();
    st.step(1);
    assert_eq!(pos(&st), spb * 5.0 + 1000.0);
    st.e.beat_jump("A", -1.5).unwrap();
    st.step(1);
    assert_eq!(pos(&st), spb * 3.5 + 1000.0);
    // a loop that holds moves with it, the playhead as far into it
    st.e.set_quantize("A", Quantize::Beat);
    st.e.loop_beats("A", 2.0); // beats 3 to 5
    st.e.beat_jump("A", 4.0).unwrap();
    st.step(1);
    let s = st.e.deck("A").unwrap().sample.as_ref().unwrap();
    assert_eq!(
        (s.loop_start, s.loop_end, s.looping),
        (Some(spb * 7.0), Some(spb * 9.0), true)
    );
    assert_eq!(pos(&st), spb * 7.5 + 1000.0);

    // playing: as far ahead of a twin as the jump, wherever the platter is when it lands
    let mut st = Stepper::new();
    st.e.load_demo("A");
    st.e.load_demo("B");
    st.step(1);
    st.e.play("A");
    st.e.play("B");
    st.step(300);
    st.e.beat_jump("A", 2.0).unwrap();
    st.step(100);
    let p = |d: &str| st.e.deck(d).unwrap().mirror.turntable_position();
    assert!((p("A") - p("B") - 2.0 * spb).abs() < 1e-3, "{} {}", p("A"), p("B"));

    // no grid, nothing to jump by
    let blank = SampleBuffer::new(vec![0.0; 48_000], None, SR);
    let overview = WaveformOverview::new(&blank, 1200);
    st.e.load_sample(
        "B",
        PreparedSample {
            title: "x".into(),
            buffer: std::sync::Arc::new(blank),
            overview,
            grid: None,
        },
    );
    assert!(st.e.beat_jump("B", 1.0).is_err());
    assert!(!st.e.can_loop("B") && st.e.can_loop("A"));
}

#[test]
fn a_beat_jump_is_a_deck_action_a_session_records_and_an_arrangement_plays() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    st.step(1);
    let parse = |s: &str| JsonValue::parse(s).unwrap();
    st.e.start_session().unwrap();
    let a = DeckAction::from_json(&parse(r#"{"op":"beatJump","deck":"A","beats":8}"#)).unwrap();
    assert_eq!(
        a,
        DeckAction::BeatJump {
            deck: "A".into(),
            beats: 8.0
        }
    );
    assert_eq!(st.e.perform(&a, PerformCtx::default()), Applied::Done);
    st.step(1);
    assert_eq!(tick(&st, "A"), 32);
    let s = st.e.finish_session().unwrap();
    assert_eq!(
        s.events.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [SessionEventKind::Action(a)]
    );
    assert_eq!(Session::from_json(&s.json()).unwrap().events, s.events);
    // an arrangement's move, played
    let op = ArrangeOp::from_json(&parse(r#"{"op":"beatJump","deck":"A","beats":-4}"#)).unwrap();
    assert!(st.e.apply_arrange_op(&op));
    st.step(1);
    assert_eq!(tick(&st, "A"), 16);
    // how far is the action: without it there is none
    assert_eq!(DeckAction::from_json(&parse(r#"{"op":"beatJump","deck":"A"}"#)), None);
    let jump = |deck: &str| DeckAction::BeatJump {
        deck: deck.into(),
        beats: 1.0,
    };
    assert!(matches!(st.e.perform(&jump("L"), PerformCtx::default()), Applied::Ignored(_)));
    assert!(matches!(st.e.perform(&jump("B"), PerformCtx::default()), Applied::Ignored(_)));
}

// MARK: a net deck's loop

#[test]
fn a_four_beat_loop_on_a_net_deck_plays_the_same_four_beats_pass_after_pass() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    st.e.set_quantize("A", Quantize::Bar);
    st.e.play("A");
    st.step(steps(38));
    let t = tick(&st, "A");
    assert!((34..44).contains(&t), "in bar 3: {t}");
    assert!(st.e.can_loop("A"));
    let mut cursor = tap(&st, "A");
    st.e.loop_beats("A", 4.0);
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((32, 48)), "from the bar line at or before the playhead");
    st.step(steps(16 * 6));
    assert_eq!(st.e.net_loop("A"), Some((32, 48)), "still holding");
    assert!((33..=48).contains(&tick(&st, "A")));
    let (fires, anchors) = read(&st, "A", &mut cursor);
    let (wraps, whole) = passes(&fires, &anchors, 32);
    assert!(whole.len() >= 4, "{} whole passes", whole.len());
    for w in wraps.windows(2) {
        assert_eq!(w[1] - w[0], (16 * STEP) as i64, "a pass is 4 beats");
    }
    assert!(whole[0].len() > 8, "a pass plays notes: {:?}", whole[0]);
    for p in &whole[1..] {
        assert_eq!(p, &whole[0]);
    }
    // each pass plays the window's steps once each, in order
    let one: Vec<i32> = anchors
        .iter()
        .filter(|a| a.frame >= wraps[0] && a.frame < wraps[1])
        .map(|a| a.tick)
        .collect();
    assert_eq!(one, (33..=48).collect::<Vec<_>>());
    // and what the song played there before the first wrap is what every pass plays
    let before: Vec<Hit> = fires.iter().filter(|f| f.frame < wraps[0]).map(hit).collect();
    let same: Vec<Hit> = whole[0].iter().filter(|h| h.0 as i64 > t).copied().collect();
    assert!(!before.is_empty());
    assert_eq!(before, same);
}

#[test]
fn a_net_loop_rolls_four_two_one_half_and_exits_through_its_end() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(7.0)));
    st.e.set_quantize("A", Quantize::Bar);
    st.e.play("A");
    st.step(steps(66)); // bar 5 has begun
    st.e.loop_beats("A", 4.0);
    for (k, want) in [(64, 80), (64, 72), (64, 68), (64, 66)].into_iter().enumerate() {
        if k > 0 {
            st.e.loop_halve("A");
        }
        let mut cursor = tap(&st, "A");
        let len = (want.1 - want.0) as usize;
        st.step(steps(len * 4 + 1));
        assert_eq!(st.e.net_loop("A"), Some(want));
        let (fires, anchors) = read(&st, "A", &mut cursor);
        let (wraps, whole) = passes(&fires, &anchors, want.0);
        assert!(whole.len() >= 2, "{want:?}: {} whole passes", whole.len());
        for w in wraps.windows(2) {
            assert_eq!(w[1] - w[0], (len * STEP) as i64, "{want:?}");
        }
        assert!(!whole[0].is_empty(), "{want:?}");
        for p in &whole[1..] {
            assert_eq!(p, &whole[0], "{want:?}");
        }
    }
    // a step is the shortest
    st.e.loop_halve("A");
    st.e.loop_halve("A");
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((64, 65)));
    // out: on from where it is, through the window's end into the rest of the song
    let mut cursor = tap(&st, "A");
    st.e.exit_loop("A");
    st.step(steps(8));
    assert_eq!(st.e.net_loop("A"), None);
    let (_, anchors) = read(&st, "A", &mut cursor);
    assert!(
        anchors
            .iter()
            .all(|a| !a.flags.contains(TickAnchorFlags::DISCONTINUITY)),
        "nothing jumped"
    );
    let ticks: Vec<i32> = anchors.iter().map(|a| a.tick).collect();
    assert!(ticks.windows(2).all(|w| w[1] == w[0] + 1), "{ticks:?}");
    assert!(*ticks.last().unwrap() > 70, "past the window's end: {ticks:?}");
}

#[test]
fn loop_in_and_out_snap_a_net_loop_and_double_and_halve_it() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    st.e.set_quantize("A", Quantize::Beat);
    st.e.play("A");
    st.step(steps(21));
    let nearest = |p: f64| ((p / 4.0).round() * 4.0) as i64;
    let start = nearest(place(&st, "A"));
    st.e.loop_in("A");
    st.step(steps(13));
    let end = nearest(place(&st, "A"));
    assert!(end > start);
    st.e.loop_out("A");
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((start, end)));
    st.e.loop_double("A");
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((start, start + 2 * (end - start))));
    st.e.loop_halve("A");
    st.e.loop_halve("A");
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((start, start + (end - start) / 2)));
    // OUT while looping leaves the loop; a loop over 64 beats is refused
    st.e.loop_out("A");
    st.step(1);
    assert_eq!(st.e.net_loop("A"), None);
    st.e.loop_beats("A", 65.0);
    st.step(1);
    assert_eq!(st.e.net_loop("A"), None);
}

#[test]
fn a_jump_ends_a_net_loop_and_a_loop_the_history_cannot_reach_is_let_go() {
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    st.e.set_quantize("A", Quantize::Bar);
    st.e.play("A");
    st.step(steps(36));
    st.e.loop_beats("A", 4.0);
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((32, 48)));
    // a jump hands the deck another project: the loop ends at once, so the swap can land
    st.e.beat_jump("A", 8.0).unwrap();
    st.step(1);
    assert_eq!(st.e.net_loop("A"), None);
    st.step(steps(20));
    assert!(tick(&st, "A") > 80, "{}", tick(&st, "A"));

    // a loop whose start is before anything the net remembers (the project went in at step 37)
    let mut st = Stepper::new();
    assert!(st.e.generate("A", &fast(42.0)));
    assert!(st.e.relocate("A", 37, None, None));
    st.step(1);
    st.e.set_quantize("A", Quantize::Beat);
    st.e.play("A");
    st.step(steps(2));
    st.e.loop_beats("A", 4.0); // from the beat line 36
    st.step(1);
    assert_eq!(st.e.net_loop("A"), Some((36, 52)));
    let mut cursor = tap(&st, "A");
    st.step(steps(20));
    assert_eq!(st.e.net_loop("A"), None, "let go at its end, never half wrapped");
    let (_, anchors) = read(&st, "A", &mut cursor);
    assert!(anchors.windows(2).all(|w| w[1].tick == w[0].tick + 1), "it played straight on");
    assert!(anchors.iter().any(|a| a.tick > 53));
}
