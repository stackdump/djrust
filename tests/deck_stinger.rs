//! A Hit that launches a loop (`StingerLaunch.swift`: `fireStingerLoop`, `launchStingerClip`,
//! `serviceStingerStops`), by the edges the Swift golden (`stinger_golden.rs`) does not name.
//!
//! Parity form (a) in spirit only: no Swift test calls `fireStingerLoop` or `onStingerLoop`
//! (`StingerCurationTests` covers the curation, ported in `deck_options.rs` and the pure half
//! in `deck_pure.rs`), so these are Rust tests of what the Swift code says, and the numbers in
//! them are the ones the golden has Swift producing.

mod common_row;

use common_row::*;
use djrust::deck::loops::LoopState;
use djrust::json::JsonValue;
use djrust::pflow::StingerAssignment;
use djrust::sound::stinger::{AssignedLoop, AssignmentKind};

fn loop_hit(slot: usize, clip: Option<&str>, path: Option<&str>) -> StingerAssignment {
    StingerAssignment {
        slot,
        kind: AssignmentKind::Loop,
        candidate: Some("test-loop".into()),
        label: "Test loop".into(),
        option: None,
        macro_id: None,
        bars: None,
        loop_: Some(AssignedLoop {
            path: path.map(String::from),
            clip_id: clip.map(String::from),
            name: "Test loop".into(),
            beats: 4,
            bpm: 120.0,
        }),
        local_only: path.is_some(),
    }
}

/// The Hit nets' indices in deck A's graph, as the Swift golden recorded them (the golden test
/// holds the Rust compile to the same).
fn hit_nets() -> Vec<usize> {
    let path = format!(
        "{}/tests/fixtures/swift/stinger.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let g = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    g.get("hits").unwrap().get("netsA").unwrap().as_array().unwrap().iter()
        .map(|v| v.as_i64().unwrap() as usize)
        .collect()
}

fn ringing(rig: &Rig, nets: &[usize]) -> Vec<bool> {
    let m = &rig.e.deck("A").unwrap().mirror;
    nets.iter().map(|&i| !m.is_net_muted(i)).collect()
}

/// Deck A at 120 bpm, a clip x on the row, Hit 2 on x, Hit 3 on a stock loop (`path`).
fn rig_with_hits() -> (Rig, String) {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(2 * BAR + 1_024);
    let x = test_clip(1);
    rig.e.restore_clips(std::slice::from_ref(&x));
    rig.e.deck_mut("A").unwrap().project.as_mut().unwrap().stingers = vec![
        StingerAssignment::fallback(0),
        loop_hit(1, Some(&x.id), None),
        loop_hit(2, None, Some("/nowhere/riser.caf")),
    ];
    (rig, x.id)
}

#[test]
fn a_loop_hit_starts_its_clip_from_the_top_and_puts_it_out_after_one_pass() {
    let (mut rig, x) = rig_with_hits();
    rig.e.stinger("A", 1, 1);
    assert_eq!(rig.e.loop_state(&x), LoopState::Starting);
    // A's next bar line after the lead (a bar is 96,000 frames; the loop clock is within 2 frames)
    let start = rig.e.loop_switch_frame(&x).unwrap();
    assert!((start - 3 * BAR).abs() <= 2, "{start}");
    assert_eq!(rig.e.loop_reference(&x).as_deref(), Some("A"));
    // one pass of its four beats at A's tempo
    let stops = rig.e.stinger_stops().clone();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[&x], start + BAR);
    // the stop is scheduled the moment the voice is in
    rig.run_to(start + 4_096);
    assert!(rig.e.stinger_stops().is_empty());
    assert_eq!(rig.e.loop_stop_frame(&x), Some(start + BAR));
    assert_eq!(rig.e.loop_state(&x), LoopState::Stopping);
    let p = rig.e.loop_playhead(&x).unwrap();
    assert!(p < 0.06, "from its top, not in phase: {p}");
    rig.run_to(start + BAR + 8_192);
    assert_eq!(rig.e.loop_state(&x), LoopState::Off);
    assert!(rig.e.looping_clips().is_empty());
}

#[test]
fn a_hit_whose_clip_left_the_row_plays_its_stinger_net() {
    let nets = hit_nets();
    let (mut rig, x) = rig_with_hits();
    rig.e.remove_clip(&x);
    rig.run(4_096);
    assert_eq!(ringing(&rig, &nets), [false; 4]);
    rig.e.stinger("A", 1, 1);
    assert_eq!(rig.e.loop_state(&x), LoopState::Off);
    assert!(rig.e.stinger_stops().is_empty());
    rig.run(4_096);
    assert_eq!(ringing(&rig, &nets), [false, true, false, false], "Hit 2's net, not the loop");
}

#[test]
fn a_stock_loop_hit_plays_its_stinger_net_off_the_mac() {
    // Swift decodes the Apple Loop into `stingerClips` and answers false when it cannot: the Hit
    // then plays its net. There is no Apple Loops index here, so the answer is always false.
    let nets = hit_nets();
    let (mut rig, x) = rig_with_hits();
    rig.e.stinger("A", 2, 1);
    assert!(rig.e.looping_clips().is_empty() && rig.e.stinger_stops().is_empty());
    assert_eq!(rig.e.loop_state(&x), LoopState::Off);
    rig.run(4_096);
    assert_eq!(ringing(&rig, &nets), [false, false, true, false]);
}

#[test]
fn a_hit_on_a_sample_deck_fires_nothing() {
    let (mut rig, x) = rig_with_hits();
    assert!(rig.e.load_demo("B"));
    rig.e.stinger("B", 1, 1);
    assert!(rig.e.stingers("B").is_empty());
    assert!(rig.e.looping_clips().is_empty() && rig.e.stinger_stops().is_empty());
    assert_eq!(rig.e.loop_state(&x), LoopState::Off);
}
