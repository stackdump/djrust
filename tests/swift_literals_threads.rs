//! Two-thread Swift stress tests that ran on `WaveDeckSource` and had no Rust counterpart on a
//! real `DeckCore` (package F of the parity audit): a reader on another thread holds the anchor
//! of every fire it has read, also after a mid-tick resubscribe
//! (`FireEventTests.aReaderAlwaysHoldsTheAnchorOfEveryFireItHasRead`,
//! `aReaderThatSubscribesMidTickStillHoldsTheAnchorOfEveryFire`, with `EventOrderStress`), and
//! swaps under load never pair a layout with the wrong tokens
//! (`MarkingMirrorThreadTests.swapsUnderLoadNeverPairALayoutWithTheWrongTokens`,
//! `MarkingStress.stressEngineMirror`). The producer is a real thread running
//! `DeckCore::render`; the reader polls `DeckMirror`, which is what the hub's feeds read.
//!
//! The Rust rings hold 8192 fires and 2048 anchors where Swift's stress asks for 1<<17 and 1<<16
//! (`DeckMirror` has no way to size them). The 1,000 blocks of 64 frames at tempo 3000 and 8 kHz
//! are 1,600 ticks, which fit, so `lost == 0` holds whatever the reader's pace.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use djrust::pflow::Project;
use djrust::wave::{EventCursor, MarkingSnapshot, WaveOptions, WaveProject, WaveRunner};
use djrust::{compose, JsonObject};
use djrust::host::deck::{Command, DeckCore, DeckMirror};

fn project(genre: &str, seed: f64) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    compose(genre, &ov.into())
}

fn prepare(p: &Project, rate: f64) -> WaveProject {
    WaveRunner::prepare(p, rate, &WaveOptions::default(), 0)
}

// MARK: the order of fires and anchors across threads

#[derive(Debug, Default)]
struct EventOrderReport {
    fires: usize,
    anchors: usize,
    polls: usize,
    lost: usize,
    /// Fires returned by a read whose own tick's anchor had not been read by the end of that read.
    fire_without_anchor: usize,
}

/// `EventOrderStress.stressEventOrder`: render `blocks` blocks of `size` frames on a producer
/// thread, as fast as it can go, at a tempo so high that ticks come every few blocks; read the
/// event streams from this thread the whole time.
///
/// `paced`: the producer never runs more than `PACE_BLOCKS` blocks ahead of the newest anchor the
/// reader has read. A run longer than the rings (8192 fires, 2048 anchors) would otherwise lose
/// records whenever the reader is held up a few tens of milliseconds, and a fire whose anchor was
/// lost is not a violation of the order (`read_events` says so): pacing keeps the loss at zero
/// whatever the scheduler does, and the producer still races the reader flat out in between.
fn stress_event_order(
    p: &Project,
    blocks: usize,
    size: usize,
    resubscribe_every: usize,
    paced: bool,
) -> EventOrderReport {
    const PACE_BLOCKS: i64 = 500;
    let rate = 8000.0;
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", rate, WaveOptions::default(), false, mirror.clone());
    core.apply(Command::Load(prepare(p, rate)));
    core.apply(Command::Tempo(3000.0));
    core.apply(Command::Play);
    let done = Arc::new(AtomicBool::new(false));
    let read_frame = Arc::new(AtomicI64::new(0));
    let producer = {
        let (done, read_frame) = (done.clone(), read_frame.clone());
        std::thread::spawn(move || {
            let (mut l, mut r) = (vec![0f32; size], vec![0f32; size]);
            let mut frame = 0i64;
            for _ in 0..blocks {
                while paced && frame - read_frame.load(Ordering::Acquire) > PACE_BLOCKS * size as i64 {
                    std::thread::yield_now();
                }
                core.render(&mut l, &mut r, frame);
                frame += size as i64;
            }
            done.store(true, Ordering::Release);
            core // (dropped by the joiner, off the render loop)
        })
    };
    let mut report = EventOrderReport::default();
    let mut cursor = if resubscribe_every > 0 {
        mirror.event_cursor_now()
    } else {
        EventCursor::new()
    };
    let mut seen: HashSet<(i32, i32)> = HashSet::new(); // (generation, tick) of every anchor read
    let (mut fires, mut anchors) = (Vec::new(), Vec::new());
    loop {
        let finished = done.load(Ordering::Acquire);
        if resubscribe_every > 0 && report.polls % resubscribe_every == 0 {
            cursor = mirror.event_cursor_now(); // a new feed opening on a deck that is mid-tick
            seen.clear();
        }
        fires.clear();
        anchors.clear();
        let (fires_lost, anchors_lost) = mirror.read_events(&mut cursor, &mut fires, &mut anchors);
        report.polls += 1;
        report.lost += fires_lost + anchors_lost;
        for a in &anchors {
            seen.insert((a.generation, a.tick));
            read_frame.fetch_max(a.frame, Ordering::Release);
        }
        for f in &fires {
            if !seen.contains(&(f.generation, f.tick)) {
                report.fire_without_anchor += 1;
            }
        }
        report.fires += fires.len();
        report.anchors += anchors.len();
        if finished && fires.is_empty() && anchors.is_empty() {
            break;
        }
    }
    drop(producer.join().unwrap());
    report
}

// FireEventTests.aReaderAlwaysHoldsTheAnchorOfEveryFireItHasRead: a reader on another thread
// polls while the deck renders flat out at a tempo that puts a tick every few blocks: the anchor
// is written before the tick's fires.
#[test]
fn a_reader_always_holds_the_anchor_of_every_fire_it_has_read() {
    let report = stress_event_order(&project("techno", 42.0), 1_000, 64, 0, false);
    eprintln!("event order stress: {report:?}");
    assert_eq!(report.lost, 0, "the rings were big enough for the reader to keep up: {report:?}");
    assert!(
        report.fires > 200 && report.anchors > 200 && report.polls > 50,
        "{report:?}"
    );
    assert_eq!(
        report.fire_without_anchor, 0,
        "{} fires read without their anchor",
        report.fire_without_anchor
    );
}

// The same, 50 times as long. A race is caught by being run for long: at 1,000 blocks (Swift's
// literal) a reader that took the anchors before the fires, instead of after, got through; at
// 100,000 it was caught. The rings wrap here (8192 fires, 2048 anchors, the Swift stress asks for
// 1<<17 and 1<<16), so the producer is paced to stay within 500 blocks of the reader: nothing is
// lost whatever the scheduler does, and every fire the reader gets has its anchor.
#[test]
fn a_reader_holds_the_anchor_of_every_fire_over_a_long_run() {
    let report = stress_event_order(&project("techno", 42.0), 50_000, 64, 0, true);
    assert_eq!(report.lost, 0, "{report:?}");
    assert!(report.fires > 100_000 && report.anchors > 20_000, "{report:?}");
    assert_eq!(
        report.fire_without_anchor, 0,
        "{} fires read without their anchor: {report:?}",
        report.fire_without_anchor
    );
}

// FireEventTests.aReaderThatSubscribesMidTickStillHoldsTheAnchorOfEveryFire: feeds open and
// close all the time. One that opens while a tick is half written must not be handed fires whose
// anchor is already behind its cursor.
#[test]
fn a_reader_that_subscribes_mid_tick_still_holds_the_anchor_of_every_fire() {
    let report = stress_event_order(&project("techno", 42.0), 1_000, 64, 3, false);
    assert_eq!(report.lost, 0, "{report:?}");
    assert!(report.fires > 100 && report.polls > 100, "{report:?}");
    assert_eq!(
        report.fire_without_anchor, 0,
        "{} fires without an anchor",
        report.fire_without_anchor
    );
}

// MARK: swaps under load on a real deck

#[derive(Debug, Default)]
struct EngineMirrorReport {
    reads: usize,
    /// Distinct generations observed.
    swaps_seen: usize,
    /// Snapshot arrays not the size of its own layout, or generation != layout's.
    mismatched: usize,
    /// Sequence or tick went back within one generation.
    backwards: usize,
    unknown_layouts: usize,
    frames_rendered: i64,
}

/// `MarkingStress.stressEngineMirror`: the audio thread is a real thread running `render` in a
/// loop on a net deck; this thread loads one prepared project after another (so the audio thread
/// swaps at bar boundaries) and reads the mirror as fast as it can. Every snapshot must be sized
/// like its own layout.
fn stress_engine_mirror(projects: Vec<WaveProject>, rate: f64) -> EngineMirrorReport {
    let total = projects.len();
    let generations: Vec<i32> = projects.iter().map(|p| p.generation).collect();
    let mut queue: std::collections::VecDeque<WaveProject> = projects.into();
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", rate, WaveOptions::default(), false, mirror.clone());
    let first = queue.pop_front().unwrap();
    mirror.remember_marking(first.marking.clone());
    core.apply(Command::Load(first));
    core.apply(Command::Tempo(3000.0));
    core.apply(Command::Play);
    let (tx, rx) = channel::<Command>();
    let stop = Arc::new(AtomicBool::new(false));
    // frames rendered so far, for the main loop to wait on: a loaded machine can start the render
    // thread later than the reads finish, and then nothing would have been measured
    let progress = Arc::new(AtomicI64::new(0));
    let renderer = {
        let (stop, progress) = (stop.clone(), progress.clone());
        std::thread::spawn(move || {
            let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
            let mut f = 0i64;
            while !stop.load(Ordering::Acquire) {
                while let Ok(c) = rx.try_recv() {
                    core.apply(c);
                }
                core.render(&mut l, &mut r, f);
                f += 128;
                progress.store(f, Ordering::Release);
                while let Some(p) = core.runner.pop_retired() {
                    drop(p); // freed here, not on the render path of a real deck
                }
            }
            f
        })
    };
    let started = Instant::now() + Duration::from_secs(120);
    while progress.load(Ordering::Acquire) == 0 && Instant::now() < started {
        std::thread::yield_now();
    }
    let mut report = EngineMirrorReport::default();
    let mut snap = MarkingSnapshot::new();
    let mut last_seq: BTreeMap<i32, i64> = BTreeMap::new();
    let mut last_tick: BTreeMap<i32, i64> = BTreeMap::new();
    let mut seen: BTreeSet<i32> = BTreeSet::new();
    let mut next = 1; // the next project to hand over
    let mut after_last = 0;
    let mut on_current = 0; // reads since the current project was first seen
    let deadline = Instant::now() + Duration::from_secs(120);
    // Paced by progress, not by the clock: the next project is handed over only once a snapshot
    // of the previous one has been read, so a loaded machine only makes this slower, and every
    // project is guaranteed to be observed while it runs.
    while after_last < 300 && Instant::now() < deadline {
        if seen.contains(&generations[next - 1]) {
            on_current += 1;
        }
        if next < total && on_current >= 200 {
            let p = queue.pop_front().unwrap();
            mirror.remember_marking(p.marking.clone());
            tx.send(Command::Load(p)).unwrap();
            next += 1;
            on_current = 0;
        }
        if next == total && on_current >= 200 {
            after_last += 1;
        }
        if !mirror.read_marking(&mut snap) {
            continue;
        }
        let Some(layout) = snap.layout.clone() else {
            continue;
        };
        report.reads += 1;
        let h = snap.header;
        if h.generation != layout.generation
            || snap.tokens.len() != layout.place_count()
            || snap.muted.len() != layout.net_count()
        {
            report.mismatched += 1;
        }
        if mirror.marking_layout_of(h.generation).is_none() {
            report.unknown_layouts += 1;
        }
        if last_seq.get(&h.generation).is_some_and(|&s| h.sequence < s) {
            report.backwards += 1;
        }
        if last_tick.get(&h.generation).is_some_and(|&t| h.tick < t) {
            report.backwards += 1;
        }
        last_seq.insert(h.generation, h.sequence);
        last_tick.insert(h.generation, h.tick);
        seen.insert(h.generation);
    }
    stop.store(true, Ordering::Release);
    report.frames_rendered = renderer.join().unwrap();
    report.swaps_seen = seen.len();
    report
}

// MarkingMirrorThreadTests.swapsUnderLoadNeverPairALayoutWithTheWrongTokens, against a real deck
// (`DeckCore::render` on the audio thread, `DeckMirror`'s book on the reader's).
#[test]
fn swaps_under_load_never_pair_a_layout_with_the_wrong_tokens_on_a_real_deck() {
    let genres = [("techno", 42.0), ("ambient", 5.0), ("house", 7.0), ("dnb", 3.0)];
    let projects: Vec<WaveProject> = (0..12)
        .map(|i| {
            let (g, s) = genres[i % genres.len()];
            prepare(&project(g, s + i as f64), 8000.0)
        })
        .collect();
    let shapes: BTreeSet<usize> = projects.iter().map(|p| p.marking.layout.place_count()).collect();
    assert!(shapes.len() >= 3, "varied shapes");
    let n = projects.len();
    let report = stress_engine_mirror(projects, 8000.0);
    eprintln!("deck mirror stress: {report:?}");
    assert_eq!(report.mismatched, 0, "a snapshot's arrays did not fit its own layout");
    assert_eq!(report.unknown_layouts, 0);
    assert_eq!(report.backwards, 0);
    assert!(report.reads > 1000, "{report:?}");
    assert_eq!(
        report.swaps_seen, n,
        "the audio thread swapped through every project under the reader ({})",
        report.swaps_seen
    );
    assert!(report.frames_rendered > 0);
}
