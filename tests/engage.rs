//! Engaging effects (host::deck::engage): every family on one entry point, starting on the
//! bar, lasting 1 to 4 bars or latched, transitions momentary.

use djrust::fx::catalogue::{self, Category};
use djrust::fx::tricks::TrickKind;
use djrust::host::deck::offline::OfflineConductor;
use djrust::host::deck::{engine, Engagement, Engine, EngineConfig};
use djrust::TrackSpec;

const SR: f64 = 44_100.0;
const BLOCK: usize = OfflineConductor::BLOCK_FRAMES;

fn live() -> OfflineConductor {
    let (mut e, r) = engine(EngineConfig::new(SR));
    e.deterministic = true;
    let mut c = OfflineConductor::new(e, r);
    {
        let e = c.engine_mut();
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        e.set_volume("A", 0.9);
        e.set_crossfader(0.0);
        e.play("A");
    }
    c.pump();
    c
}

fn run_to(c: &mut OfflineConductor, frame: i64) {
    while c.engine().frame() < frame {
        c.step_block();
    }
}

/// Frames in a bar at the deck's tempo.
fn bar(e: &Engine) -> i64 {
    let (_, bpm) = e.sample_master_fire("A");
    (4.0 * 60.0 / bpm * SR) as i64
}

#[test]
fn it_refuses_what_a_category_does_not_allow() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let e = c.engine_mut();
    assert!(e.engage("A", "nope", Engagement::Latch).is_err(), "an id that is merged away or never was");
    assert!(e.engage("A", "solo-drums", Engagement::Bars(1)).is_err());
    assert!(e.engage("A", "tape_stop", Engagement::Bars(2)).is_err(), "a one-shot has no bar length");
    assert!(e.engage("A", "tape_stop", Engagement::Latch).is_err(), "and no latch");
    assert!(e.engage("A", "feel-build", Engagement::Latch).is_err(), "a sweep has no latch");
    assert!(e.engage("A", "stutter", Engagement::Bars(0)).is_err());
    assert!(e.engage("A", "feel-build", Engagement::Bars(2)).is_ok());
    // every entry is in a category that says whether it latches
    for x in catalogue::ENTRIES.iter() {
        assert_eq!(x.latchable(), !matches!(x.category, Category::Transition | Category::Sweeps));
    }
}

#[test]
fn a_trick_starts_on_the_bar_and_a_latch_stays_until_the_pad_is_pressed_again() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    let bar = bar(c.engine());
    c.engine_mut().engage("A", "stutter", Engagement::Latch).unwrap();
    let s = c.engine().fx_state("A");
    assert_eq!(s.len(), 1);
    assert!(s[0].latched && s[0].pending, "asked for, waiting for its bar: {s:?}");
    c.step_block();
    run_to(&mut c, start + 2 * BLOCK as i64);
    let s = c.engine().fx_state("A");
    assert!(!s[0].pending && s[0].latched, "{s:?}");
    // three bars on, still on
    run_to(&mut c, start + 3 * bar);
    assert!(c.engine().trick_on("A", TrickKind::Stutter));
    assert!(c.engine().fx_state("A")[0].latched);
    // pressed again: it lets go on the next bar, not at once
    c.engine_mut().unlatch("A", "stutter").unwrap();
    assert!(c.engine().fx_state("A").iter().all(|f| !f.latched));
    run_to(&mut c, start + 5 * bar);
    assert!(!c.engine().trick_on("A", TrickKind::Stutter));
    assert!(c.engine().fx_state("A").is_empty());
}

#[test]
fn bars_run_whole_bars_and_end_themselves() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    let bar = bar(c.engine());
    c.engine_mut().engage("A", "delay", Engagement::Bars(2)).unwrap();
    run_to(&mut c, start + bar + bar / 2);
    assert!(c.engine().trick_on("A", TrickKind::Delay), "in its second bar");
    run_to(&mut c, start + 3 * bar);
    assert!(!c.engine().trick_on("A", TrickKind::Delay), "over after two bars");
    assert!(c.engine().fx_state("A").is_empty());
}

#[test]
fn a_released_hold_lets_go_at_once_and_one_before_its_bar_never_sounds() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    c.engine_mut().engage("A", "pump", Engagement::Momentary).unwrap();
    c.engine_mut().release("A", "pump").unwrap();
    assert!(!c.engine().trick_on("A", TrickKind::Pump));
    assert!(c.engine().fx_state("A").is_empty());
}

#[test]
fn a_song_macro_waits_for_the_bar_and_a_latch_holds_it_until_let_go() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    let bar = bar(c.engine());
    c.engine_mut().engage("A", "breakdown", Engagement::Latch).unwrap();
    // before the bar, nothing is muted by a macro
    while c.engine().frame() + 1500 < start {
        c.step_block();
        assert!(c.engine().deck("A").unwrap().mirror.active_macros().is_empty(), "frame {}", c.engine().frame());
    }
    run_to(&mut c, start + 6 * BLOCK as i64);
    assert!(!c.engine().deck("A").unwrap().mirror.active_macros().is_empty(), "on its bar");
    // longer than any bar count: still held
    run_to(&mut c, start + 9 * bar);
    assert!(!c.engine().deck("A").unwrap().mirror.active_macros().is_empty(), "latched");
    c.engine_mut().release("A", "breakdown").unwrap();
    let f = c.engine().frame() + 4 * BLOCK as i64;
    run_to(&mut c, f);
    assert!(c.engine().deck("A").unwrap().mirror.active_macros().is_empty(), "let go");
}

#[test]
fn a_latched_aux_macro_holds_past_any_bar_count_and_lets_go_when_released() {
    use djrust::fx::AuxMacro;
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    let bar = bar(c.engine());
    c.engine_mut().engage("A", "cathedral", Engagement::Latch).unwrap();
    run_to(&mut c, start + 9 * bar);
    assert!(c.engine().aux_mirror.is_running(AuxMacro::Cathedral), "latched");
    c.engine_mut().release("A", "cathedral").unwrap();
    let f = c.engine().frame() + 4 * BLOCK as i64;
    run_to(&mut c, f);
    assert_eq!(c.engine().aux_macros_running(), 0, "let go");
    // a pan macro needs a net deck: A is one
    assert!(c.engine_mut().engage("A", "auto-pan", Engagement::Bars(2)).is_ok());
}

#[test]
fn a_held_transition_stops_when_the_pad_lifts_and_a_song_one_shot_needs_no_release() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    c.engine_mut().engage("A", "tape_stop", Engagement::Momentary).unwrap();
    run_to(&mut c, start + 4 * BLOCK as i64);
    assert!(c.engine().trick_on("A", TrickKind::TapeStop));
    assert_eq!(c.engine().fx_state("A").len(), 1);
    c.engine_mut().release("A", "tape_stop").unwrap();
    assert!(!c.engine().trick_on("A", TrickKind::TapeStop));
    assert!(c.engine().fx_state("A").is_empty());
    // a cut ends itself; releasing it is harmless
    c.engine_mut().engage("A", "cut", Engagement::Momentary).unwrap();
    c.engine_mut().release("A", "cut").unwrap();
}

#[test]
fn panic_lets_go_of_a_latch_and_of_what_waits_for_its_bar() {
    let mut c = live();
    c.run_to(BLOCK as i64 * 40);
    let (start, _) = c.engine().sample_master_fire("A");
    c.engine_mut().engage("A", "stutter", Engagement::Latch).unwrap();
    run_to(&mut c, start + 4 * BLOCK as i64);
    assert!(c.engine().trick_on("A", TrickKind::Stutter));
    c.engine_mut().engage("A", "breakdown", Engagement::Latch).unwrap();
    assert_eq!(c.engine().fx_state("A").len(), 2);
    c.engine_mut().panic("A");
    assert!(!c.engine().trick_on("A", TrickKind::Stutter));
    assert!(c.engine().fx_state("A").is_empty());
    // the song macro that was waiting for its bar never starts
    let bar = bar(c.engine());
    let f = c.engine().frame() + 2 * bar;
    run_to(&mut c, f);
    assert!(c.engine().deck("A").unwrap().mirror.active_macros().is_empty());
}
