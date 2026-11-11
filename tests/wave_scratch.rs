//! Scratching the net clock (`WaveRunner.freeze` / `scrub` / `release` / `setChase`): the Swift
//! suites' own tests, ported so they assert the same numbers (PORTING.md's proof, form (a)),
//! each named for the test it comes from (`WaveLiveTests.WaveScratchTests`, `FireEventTests`).
//! The same gestures held against Swift's output sample for sample, hits included, are the
//! "scratch" cases of `tests/runner_golden.rs`.

use djrust::pflow::Project;
use djrust::wave::*;
use djrust::{compose, JsonObject};

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        ov.set("structure", s.into());
    }
    compose(genre, &ov.into())
}

fn max_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

const SR: f64 = 8000.0;

fn runner(p: &Project, tempo: f64, play: bool) -> WaveRunner {
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.load(p);
    r.set_tempo(tempo);
    if play {
        r.play();
    }
    r
}

fn run(r: &mut WaveRunner, blocks: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut l = vec![0f32; 128];
    for _ in 0..blocks {
        r.process(&mut l, None, 128);
        out.extend_from_slice(&l);
    }
    out
}

fn marking(r: &WaveRunner) -> Vec<Vec<f64>> {
    r.graph()
        .unwrap()
        .nets
        .iter()
        .map(|n| n.state.clone())
        .collect()
}

fn tick(r: &WaveRunner) -> i64 {
    r.graph().unwrap().tick
}

// WaveScratchTests.scratchingForwardAndBackLeavesTheMarkingYouLeft
#[test]
fn scratching_forward_and_back_leaves_the_marking_you_left() {
    let mut r = runner(&project("techno", 42.0, Some("standard")), 240.0, true);
    run(&mut r, 90);
    r.freeze();
    run(&mut r, 1);
    let t0 = tick(&r);
    let m0 = marking(&r);
    let mutes0 = r.graph().unwrap().muted.clone();
    assert!(r.is_scrubbing() && t0 > 5);

    r.scrub(24.0);
    run(&mut r, 60);
    assert_eq!(tick(&r), t0 + 24, "reached the target");
    assert!(marking(&r) != m0);

    r.scrub(-24.0);
    run(&mut r, 60);
    assert_eq!(tick(&r), t0);
    assert!(marking(&r) == m0, "the marking is the one we left");
    assert!(r.graph().unwrap().muted == mutes0);
}

// WaveScratchTests.scratchingPlaysTheNotesItCrosses
#[test]
fn scratching_plays_the_notes_it_crosses() {
    let mut r = runner(&project("techno", 42.0, None), 120.0, false);
    r.freeze();
    let silent = max_abs(&run(&mut r, 40));
    assert_eq!(silent, 0.0, "a frozen, stopped net is silent");
    r.scrub(12.0);
    let forward = run(&mut r, 60);
    assert!(max_abs(&forward) > 0.05);
    r.scrub(-12.0);
    let backward = run(&mut r, 60);
    assert!(max_abs(&backward) > 0.05, "going back plays the notes too");
}

// WaveScratchTests.theStartOfTheSongIsAWall
#[test]
fn the_start_of_the_song_is_a_wall() {
    let mut r = runner(&project("techno", 42.0, None), 120.0, false);
    r.freeze();
    r.scrub(6.0);
    run(&mut r, 40);
    assert_eq!(tick(&r), 6);
    r.scrub(-50.0);
    run(&mut r, 80);
    assert_eq!(tick(&r), 0, "can't go before tick 0 (at {})", tick(&r));
    r.scrub(9.0); // the hand was far past the wall; it still comes back
    run(&mut r, 80);
    assert!(tick(&r) >= 0);
    r.scrub(40.0);
    run(&mut r, 200);
    assert!(tick(&r) > 5);
}

// WaveScratchTests.releaseResumesTheClockFromWhereThePlatterIs
#[test]
fn release_resumes_the_clock_from_where_the_platter_is() {
    let mut r = runner(&project("techno", 42.0, None), 240.0, true);
    run(&mut r, 30);
    r.freeze();
    r.scrub(5.0);
    run(&mut r, 60);
    let held = tick(&r);
    r.release();
    assert!(!r.is_scrubbing());
    run(&mut r, 30); // ~0.5 s at 240 bpm = 8 ticks
    assert!(
        tick(&r) >= held + 5 && tick(&r) <= held + 10,
        "running again: {held} -> {}",
        tick(&r)
    );
}

// WaveScratchTests.theSameGestureSoundsTheSameEveryTime
#[test]
fn the_same_gesture_sounds_the_same_every_time() {
    fn gesture() -> Vec<f32> {
        let mut r = runner(&project("jazz", 7.0, Some("standard")), 200.0, true);
        run(&mut r, 40);
        r.freeze();
        let mut out = Vec::new();
        for d in [10.0, -4.0, 7.0, -13.0, 6.0] {
            r.scrub(d);
            out.extend(run(&mut r, 25));
        }
        r.release();
        out.extend(run(&mut r, 20));
        out
    }
    let a = gesture();
    assert!(a == gesture());
    assert!(a.iter().any(|x| *x != 0.0));
}

// FireEventTests.scratchingTheNetClockMakesNoEventsButTheClockResumesAfterward
#[test]
fn scratching_makes_no_events_but_the_clock_resumes_afterward() {
    let p = project("techno", 42.0, None);
    let opts = WaveOptions::default();
    let mut r = WaveRunner::new(22_050.0, opts.clone());
    r.record_events = true;
    r.load_prepared(WaveRunner::prepare(&p, 22_050.0, &opts, 0));
    r.set_tempo(120.0);
    r.play();
    let mut l = vec![0f32; 128];
    let mut frame = 0;
    let mut blocks = |r: &mut WaveRunner, n: usize| {
        for _ in 0..n {
            r.process_at(&mut l, None, 128, frame);
            frame += 128;
        }
    };
    blocks(&mut r, 100);
    r.fires.clear();
    r.anchors.clear();
    let before = r.graph().unwrap().tick;
    r.freeze();
    r.scrub(12.0);
    r.set_chase(0.01);
    blocks(&mut r, 150);
    assert!(
        r.graph().unwrap().tick > before + 6,
        "the scratch moved the net clock"
    );
    assert!(
        r.fires.is_empty() && r.anchors.is_empty(),
        "scrubbed hits are not tick events"
    );
    r.scrub(-20.0); // backwards: un-firing
    blocks(&mut r, 150);
    assert!(r.fires.is_empty() && r.anchors.is_empty());
    let at_release = r.graph().unwrap().tick;
    r.release();
    blocks(&mut r, 200);
    assert!(
        !r.fires.is_empty() && !r.anchors.is_empty(),
        "back on the tick clock"
    );
    let a = &r.anchors;
    assert!(
        a[0].flags.contains(TickAnchorFlags::DISCONTINUITY),
        "the tick jumped while the hand was on it"
    );
    assert!(a[0].tick as i64 == at_release + 1 || a[0].tick as i64 == at_release);
    assert!(a[1..].iter().all(|x| x.flags == TickAnchorFlags::default()));
}

/// Swift's scrub path goes through `hit`, so `onHit` hears a scrubbed note too, at the sample
/// it falls on and the graph's counter then (forward: after the tick, backward: before it
/// is taken back). Golden: `tests/runner_golden.rs` ("scratch-*" cases hold the lists).
#[test]
fn a_scrubbed_note_is_reported_to_on_hit() {
    let p = project("techno", 42.0, None);
    let opts = WaveOptions {
        record_hits: true,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, opts.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &opts, 0));
    r.set_tempo(120.0);
    r.freeze();
    r.scrub(6.0);
    run(&mut r, 60);
    let forward = std::mem::take(&mut r.hits);
    assert!(!forward.is_empty(), "the notes it crossed");
    assert!(forward.iter().all(|h| h.tick >= 1 && h.tick <= 6));
    assert!(forward.windows(2).all(|w| w[0].tick <= w[1].tick));
    r.scrub(-6.0);
    run(&mut r, 60);
    let backward = std::mem::take(&mut r.hits);
    assert!(!backward.is_empty(), "going back plays them too");
    assert!(backward.iter().all(|h| h.tick >= 1 && h.tick <= 6));
    assert!(backward.windows(2).all(|w| w[0].tick >= w[1].tick));
}
