//! Auto-next every N bars (beyond Swift; docs/AUTO-NEXT.md, `djrust::host::deck::bar_next`): the
//! deck steps one seed on, as the browser wheel does, landing on the N-bar line of its grid.

mod common_track;

use std::collections::BTreeMap;

use common_track::Stepper;
use djrust::control::controller::{Action, SurfaceHost};
use djrust::session::arrangement::ArrangeOp;
use djrust::TrackSpec;

const RATE: f64 = 22_050.0;

fn song(seed: f64) -> TrackSpec {
    TrackSpec::new("techno", seed, Some("standard"))
}

/// Deck A playing a song, run to `tick`.
fn playing(to_tick: i64) -> Stepper {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &song(42.0)));
    st.e.play("A");
    st.run_to_tick("A", to_tick);
    st
}

fn seed(st: &Stepper) -> f64 {
    st.e.deck("A").unwrap().spec.as_ref().unwrap().seed
}

fn start_tick(st: &Stepper) -> i64 {
    st.e.deck("A")
        .unwrap()
        .track
        .latest
        .as_ref()
        .unwrap()
        .start_tick
}

/// Render until the seed changes; the tick it was asked at.
fn until_step(st: &mut Stepper, from: f64) -> i64 {
    assert!(
        st.until(200_000, |s| seed(s) != from),
        "no step from seed {from}"
    );
    st.tick("A")
}

#[test]
fn a_song_steps_one_seed_on_the_n_bar_line_and_again_n_bars_later() {
    // on mid-bar 1 (tick 20): the run counts from the line at 32, so 4 bars end at 96
    let mut st = playing(20);
    st.e.set_bar_next("A", Some(true), Some(4)).unwrap();
    assert_eq!(st.e.bar_next_left("A"), Some(4));
    let asked = until_step(&mut st, 42.0);
    // asked at the line that starts the last bar, so the compose has a whole bar
    assert!((80..96).contains(&asked), "asked at {asked}");
    assert_eq!(seed(&st), 43.0);
    assert_eq!(
        start_tick(&st),
        96,
        "the new seed comes in on the 4-bar line"
    );
    assert_eq!(st.e.bar_next_left("A"), Some(1));
    // the count restarts on the line it came in on: the next lands 4 bars on
    let asked = until_step(&mut st, 43.0);
    assert!((144..160).contains(&asked), "asked at {asked}");
    assert_eq!(seed(&st), 44.0);
    assert_eq!(start_tick(&st), 160);
    assert_eq!(st.e.deck("A").unwrap().bar_next.steps, 2);
    // the song settings stay: only the seed moved, as the wheel's step
    let s = st.e.deck("A").unwrap().spec.clone().unwrap();
    assert_eq!(s.genre, "techno");
    assert_eq!(s.structure.as_deref(), Some("standard"));
}

#[test]
fn the_countdown_reads_bars_left() {
    let mut st = playing(20);
    st.e.set_bar_next("A", Some(true), Some(8)).unwrap();
    st.run_to_tick("A", 33);
    assert_eq!(st.e.bar_next_left("A"), Some(8));
    st.run_to_tick("A", 49);
    assert_eq!(st.e.bar_next_left("A"), Some(7));
    st.run_to_tick("A", 32 + 7 * 16 + 1);
    assert_eq!(st.e.bar_next_left("A"), Some(1));
    let j = st.e.bar_next_json();
    let a = j.get("A").unwrap();
    assert_eq!(a.get("bars").and_then(|v| v.as_f64()), Some(8.0));
    assert_eq!(a.get("bars_left").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(a.get("pending").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn a_stopped_deck_does_not_count_and_off_never_steps() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &song(42.0)));
    st.e.set_bar_next("A", Some(true), Some(1)).unwrap();
    st.step(2_000);
    assert_eq!(seed(&st), 42.0);
    assert_eq!(st.e.bar_next_left("A"), None);
    let j = st.e.bar_next_json();
    assert_eq!(
        j.get("A").unwrap().get("idle").and_then(|v| v.as_str()),
        Some("stopped")
    );

    let mut st = playing(20);
    st.e.set_bar_next("A", Some(false), Some(1)).unwrap();
    st.run_to_tick("A", 200);
    assert_eq!(seed(&st), 42.0);
}

#[test]
fn a_seed_from_elsewhere_restarts_the_count() {
    let mut st = playing(20);
    st.e.set_bar_next("A", Some(true), Some(4)).unwrap();
    // the wheel at tick ~70 (bar 4): its seed comes in at 80, and the run counts from there
    st.run_to_tick("A", 70);
    st.e.step_seed("A", 5);
    assert_eq!(seed(&st), 47.0);
    let asked = until_step(&mut st, 47.0);
    assert!((128..144).contains(&asked), "asked at {asked}");
    assert_eq!(start_tick(&st), 144);
    assert_eq!(seed(&st), 48.0);
}

#[test]
fn a_loop_steps_from_its_top() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 5.0, None)));
    st.e.play("A");
    st.run_to_tick("A", 4);
    st.e.set_bar_next("A", Some(true), Some(2)).unwrap();
    let from = seed(&st);
    st.until(200_000, |s| seed(s) != from);
    assert_eq!(seed(&st), from + 1.0);
    assert_eq!(
        start_tick(&st),
        0,
        "a loop starts its new seed from the top, as the wheel"
    );
    let from = seed(&st);
    st.until(200_000, |s| seed(s) != from);
    assert_eq!(seed(&st), from + 1.0);
    assert_eq!(st.e.deck("A").unwrap().bar_next.steps, 2);
}

#[test]
fn settings_and_steps_are_traced_a_step_with_the_wheels_swap_op() {
    let mut st = playing(20);
    st.e.trace = Some(Vec::new());
    st.e.trace_via = "socket".into();
    st.e.set_bar_next("A", Some(true), Some(4)).unwrap();
    st.e.trace_via.clear();
    until_step(&mut st, 42.0);
    let t = st.e.trace.take().unwrap();
    let texts: Vec<&str> = t.iter().map(|x| x.entry.text.as_str()).collect();
    assert!(texts.contains(&"Auto-next A every 4 bars"), "{texts:?}");
    let step = t
        .iter()
        .find(|x| {
            x.entry
                .text
                .starts_with("Auto-next A (4 bars) +1 → seed 43")
        })
        .unwrap_or_else(|| panic!("{texts:?}"));
    assert_eq!(step.entry.source, "auto-next");
    match step.ops.as_slice() {
        [ArrangeOp::Swap { deck, .. }] => assert_eq!(deck, "A"),
        o => panic!("{o:?}"),
    }
    let set = t
        .iter()
        .find(|x| x.entry.text == "Auto-next A every 4 bars")
        .unwrap();
    assert_eq!(set.entry.source, "socket");
    assert!(
        set.ops.is_empty(),
        "a setting is words only: a replay steps by the swaps"
    );
}

fn target(op: &str, bars: Option<&str>) -> Action {
    let mut args = BTreeMap::new();
    args.insert("op".to_string(), op.to_string());
    if let Some(b) = bars {
        args.insert("bars".to_string(), b.to_string());
    }
    Action::Target {
        id: "autoNextBars".into(),
        deck: Some("A".into()),
        slot: None,
        index: 0,
        value: 1.0,
        pressed: true,
        args,
    }
}

#[test]
fn a_binding_toggles_and_steps_the_bar_count() {
    let mut st = playing(20);
    SurfaceHost::perform(&mut st.e, target("toggle", Some("8")));
    let b = st.e.deck("A").unwrap().bar_next.clone();
    assert!(b.on);
    assert_eq!(b.bars, 8);
    SurfaceHost::perform(&mut st.e, target("up", None));
    assert_eq!(st.e.deck("A").unwrap().bar_next.bars, 16);
    SurfaceHost::perform(&mut st.e, target("down", None));
    SurfaceHost::perform(&mut st.e, target("down", None));
    assert_eq!(st.e.deck("A").unwrap().bar_next.bars, 4);
    SurfaceHost::perform(&mut st.e, target("set", Some("12")));
    assert_eq!(st.e.deck("A").unwrap().bar_next.bars, 12);
    // a release does nothing
    let mut up = target("toggle", None);
    if let Action::Target { pressed, .. } = &mut up {
        *pressed = false;
    }
    SurfaceHost::perform(&mut st.e, up);
    assert!(st.e.deck("A").unwrap().bar_next.on);
    SurfaceHost::perform(&mut st.e, target("toggle", None));
    assert!(!st.e.deck("A").unwrap().bar_next.on);
    // the LED reads it
    use djrust::control::controller::DeckView;
    let on = |st: &Stepper| st.e.deck("A").map(|d| d.bar_next_on()).unwrap();
    assert!(!on(&st));
    SurfaceHost::perform(&mut st.e, target("on", None));
    assert!(on(&st));
}

// The live host's path: the step composes on a worker (as the wheel's does there) and lands on
// the N-bar line when it is ready within the last bar.
#[test]
fn on_a_live_engine_the_step_composes_in_the_background_and_lands_on_the_line() {
    let mut st = playing(20);
    st.e.deterministic = false;
    st.e.set_bar_next("A", Some(true), Some(4)).unwrap();
    let asked = st.until(200_000, |s| {
        s.e.deck("A")
            .unwrap()
            .track
            .seed_target
            .as_ref()
            .map(|t| t.seed)
            == Some(43.0)
    });
    assert!(asked);
    let at = st.tick("A");
    assert!((80..96).contains(&at), "asked at {at}");
    // the deck holds still while the worker composes (the stepper renders only when stepped)
    st.e.wait_composes("A");
    assert_eq!(seed(&st), 43.0);
    assert_eq!(start_tick(&st), 96);
    assert!(st.until_installed("A"));
    assert!(st.tick("A") >= 96);
}
