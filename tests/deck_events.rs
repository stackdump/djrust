//! The deck's event rings: every audible net fire and every tick of the net clock, in overwrite
//! rings the render thread writes without allocating (`LatestRing`, 8192 fires and 2048 anchors per
//! deck), each reader with its own cursor (`EventCursor`, `readEvents`). Port of the ring half of
//! `FireEventTests.swift` and `DeckFireTests`; each test names the Swift test it ports (form a:
//! the Swift suite's literals).

mod common_track;

use std::sync::Arc;

use common_track::Stepper;
use djrust::wave::{
    read_events, EventCursor, FireEvent, LatestRing, TickAnchor, WaveOptions, WaveRunner,
};
use djrust::{compose, JsonObject, TrackSpec};

fn project(genre: &str, seed: f64) -> djrust::Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    compose(genre, &ov.into())
}

// LatestRingTests.readsByAbsoluteSequenceNumber / overwritesTheOldestWhenFullAndReportsWhatWasLost
// in brief (the full port is `crates/djengine/tests/latest_ring.rs`): the sequence numbers, the
// lap, the count of what a late reader missed
#[test]
fn a_ring_numbers_what_it_takes_and_a_reader_that_fell_a_lap_behind_is_told() {
    let ring = LatestRing::<u32>::new(5);
    assert_eq!(ring.capacity(), 8, "rounded up to a power of two");
    assert_eq!(LatestRing::<u32>::new(0).capacity(), 2);
    for v in 0..20u32 {
        ring.write(v);
    }
    assert_eq!(ring.written(), 20);
    let mut out = [0u32; 32];
    // from the start: only the last lap is left, and 12 were overwritten
    let (count, lost) = ring.read(0, &mut out);
    assert_eq!((count, lost), (8, 12));
    assert_eq!(&out[..8], &[12, 13, 14, 15, 16, 17, 18, 19]);
    // a reader that is up to date sees nothing new, then one value
    assert_eq!(ring.read(20, &mut out), (0, 0));
    ring.write(20);
    assert_eq!(ring.read(20, &mut out), (1, 0));
    assert_eq!(out[0], 20);
    // a small buffer takes what fits: the cursor moves by `lost + count`
    let (count, lost) = ring.read(14, &mut out[..3]);
    assert_eq!((count, lost), (3, 0));
    assert_eq!(&out[..3], &[14, 15, 16]);
}

// A smaller scenario than LatestRingTests.aReaderLappedInTheMiddleOfItsCopyDropsTheTornPrefix (a
// ring read from its start, lapped by three slots); the Swift test's own literals are ported in
// `crates/djengine/tests/latest_ring.rs`
#[test]
fn a_reader_lapped_during_its_copy_drops_the_torn_prefix_and_counts_it_lost() {
    let ring = Arc::new(LatestRing::<u64>::new(8));
    for v in 0..8u64 {
        ring.write(v);
    }
    let mut out = [0u64; 8];
    let producer = ring.clone();
    // the reader is preempted between its two halves while the producer laps three slots
    let (count, lost) = ring.read_hooked(0, &mut out, move || {
        for v in 8..11u64 {
            producer.write(v);
        }
    });
    // slots 0..3 were overwritten while they were being copied: lost, never returned
    assert_eq!(lost, 3);
    assert_eq!(count, 5);
    assert_eq!(&out[..5], &[3, 4, 5, 6, 7], "what is kept is whole and in order");
}

// FireEventTests.aReaderMoreThanARingBehindLosesTheOldestAndIsToldHowMany
#[test]
fn a_reader_more_than_a_ring_behind_loses_the_oldest_and_is_told_how_many() {
    let p = project("techno", 42.0);
    let sr = 8000.0;
    let fires = Arc::new(LatestRing::<FireEvent>::new(100));
    let anchors = Arc::new(LatestRing::<TickAnchor>::new(50));
    let mut runner = WaveRunner::new(sr, WaveOptions::default());
    runner.fire_ring = Some(fires.clone());
    runner.anchor_ring = Some(anchors.clone());
    runner.load(&p);
    runner.set_tempo(3000.0); // 40 frames a tick
    runner.play();
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    for b in 0..300 {
        runner.process_at(&mut l, Some(&mut r), 128, b * 128); // nobody reads
    }
    assert!(fires.written() > fires.capacity() && anchors.written() > anchors.capacity());
    assert!(
        fires.capacity() == 128 && anchors.capacity() == 64,
        "rounded up to powers of two"
    );
    let mut cursor = EventCursor::new();
    let (mut kept, mut kept_anchors) = (Vec::new(), Vec::new());
    let (lf, la) = read_events(&fires, &anchors, &mut cursor, &mut kept, &mut kept_anchors);
    assert!(kept.len() == 128 && kept_anchors.len() == 64);
    assert_eq!(lf + la, fires.written() - 128 + anchors.written() - 64);
    // what is kept is the newest, in order and gap-free (anchors are one per tick)
    assert_eq!(
        kept_anchors.last().unwrap().tick as i64,
        runner.graph().unwrap().tick
    );
    assert!(kept_anchors.windows(2).all(|w| w[1].tick == w[0].tick + 1));
    assert!(kept.windows(2).all(|w| w[1].frame >= w[0].frame));
    // the reader is up to date now; the ring goes on taking events
    for b in 300..305 {
        runner.process_at(&mut l, Some(&mut r), 128, b * 128);
    }
    let (mut more, mut more_anchors) = (Vec::new(), Vec::new());
    let (lf, la) = read_events(&fires, &anchors, &mut cursor, &mut more, &mut more_anchors);
    assert!(!more.is_empty() && (lf, la) == (0, 0));
}

// FireEventTests.theDefaultRingsHoldSecondsOfTheDensestSong, and the ring's size on a deck
#[test]
fn a_decks_rings_hold_8192_fires_and_2048_anchors() {
    let st = Stepper::new(22_050.0, true);
    let m = &st.e.deck("A").unwrap().mirror;
    assert!(m.fires.capacity() >= 4096 && m.anchors.capacity() >= 1024);
    assert_eq!((m.fires.capacity(), m.anchors.capacity()), (8192, 2048));
}

// FireEventTests.readersAreIndependentAndAFreshOneStartsWhereYouSay
#[test]
fn readers_are_independent_and_a_fresh_one_starts_where_you_say() {
    let mut st = Stepper::new(22_050.0, false);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    st.e.set_tempo("A", 300.0);
    st.e.play("A");
    let mut early = st.e.event_cursor_now("A"); // from the beginning: nothing written yet
    st.step(100);
    let mut late = st.e.event_cursor_now("A"); // subscribes now
    let (mut f, mut a) = (Vec::new(), Vec::new());
    let mut fresh = EventCursor::new();
    assert_eq!(st.e.read_events("A", &mut fresh, &mut f, &mut a), (0, 0));
    assert!(!f.is_empty() && !a.is_empty(), "a fresh cursor reads the past");
    let (mut from_start, mut from_start_a) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut early, &mut from_start, &mut from_start_a);
    let (mut newest_a, mut late_f) = (Vec::new(), Vec::new());
    // the late reader sees no fires of the past, but starts from the latest anchor
    let mut probe = late;
    st.e.read_events("A", &mut probe, &mut late_f, &mut newest_a);
    assert!(late_f.is_empty() && newest_a.len() == 1, "starts from the latest anchor");
    assert_eq!(
        newest_a[0].tick as i64,
        st.e.deck("A").unwrap().mirror.tick(),
        "which is the clock as it stands"
    );
    st.step(100);
    let (mut newer, mut newer_a) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut late, &mut newer, &mut newer_a);
    let (mut newest, mut newest_a2) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut early, &mut newest, &mut newest_a2);
    assert!(!from_start.is_empty() && !newer.is_empty());
    assert_eq!(
        newer.iter().map(|e| e.frame).collect::<Vec<_>>(),
        newest.iter().map(|e| e.frame).collect::<Vec<_>>(),
        "the same events, each reader its own"
    );
    // a second read finds nothing new
    let (mut again, mut again_a) = (Vec::new(), Vec::new());
    assert_eq!(st.e.read_events("A", &mut late, &mut again, &mut again_a), (0, 0));
    assert!(again.is_empty() && again_a.is_empty());
    let now = st.e.event_cursor_now("A");
    assert_eq!(late.fires, now.fires, "up to date");
    // an anchor for every tick
    assert!(newer_a.windows(2).all(|w| w[1].tick == w[0].tick + 1));
}

// FireEventTests.audioIsUnchangedByReporting: a runner with its rings wired against a bare one
#[test]
fn audio_is_unchanged_by_reporting() {
    let p = project("techno", 42.0);
    let sr = 22_050.0;
    let mut bare = WaveRunner::new(sr, WaveOptions::default());
    bare.load(&p);
    bare.set_tempo(300.0);
    bare.play();
    assert!(bare.fire_ring.is_none() && bare.anchor_ring.is_none());
    let mut wired = WaveRunner::new(sr, WaveOptions::default());
    wired.fire_ring = Some(Arc::new(LatestRing::new(8192)));
    wired.anchor_ring = Some(Arc::new(LatestRing::new(2048)));
    wired.load(&p);
    wired.set_tempo(300.0);
    wired.play();
    let (mut want, mut got) = (Vec::new(), Vec::new());
    let (mut l, mut l2) = (vec![0f32; 128], vec![0f32; 128]);
    for b in 0..500 {
        bare.process_at(&mut l, None, 128, b * 128);
        want.extend_from_slice(&l);
        wired.process_at(&mut l2, None, 128, b * 128);
        got.extend_from_slice(&l2);
    }
    assert!(got == want, "sample-identical");
    assert!(got.iter().any(|&x| x != 0.0));
    assert!(wired.fire_ring.as_ref().unwrap().written() > 20);
}

// The ring path writes the very records the `Vec` path (`record_events`, whose content the
// runner's own tests hold) does: the same fires and anchors, in the same order, for a song with a
// macro and a mute in it. (FireEventTests compares the ring's tail with its `oracle`; this ties
// the ring to the records that are compared there.)
#[test]
fn the_rings_hold_the_records_the_vecs_hold() {
    let p = project("techno", 42.0);
    let mut runner = WaveRunner::new(22_050.0, WaveOptions::default());
    let fires = Arc::new(LatestRing::<FireEvent>::new(8192));
    let anchors = Arc::new(LatestRing::<TickAnchor>::new(2048));
    runner.fire_ring = Some(fires.clone());
    runner.anchor_ring = Some(anchors.clone());
    runner.record_events = true;
    runner.load(&p);
    runner.set_tempo(300.0);
    runner.play();
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    for b in 0..600 {
        if b == 150 {
            runner.fire_macro(djrust::wave::NetMacro::Drop, 16);
        }
        runner.process_at(&mut l, Some(&mut r), 128, b * 128);
    }
    let mut cursor = EventCursor::new();
    let (mut f, mut a) = (Vec::new(), Vec::new());
    assert_eq!(read_events(&fires, &anchors, &mut cursor, &mut f, &mut a), (0, 0));
    assert!(f.len() > 100 && a.len() > 50, "{} fires, {} anchors", f.len(), a.len());
    assert!(f == runner.fires, "the fires");
    assert!(a == runner.anchors, "the anchors");
    assert!(
        f.iter().any(|e| e.flags.contains(djrust::wave::FireFlags::MACRO)),
        "a macro's fires are flagged"
    );
}

// DeckFireTests.aDeckStampsFiresInItsRenderedFrames
#[test]
fn a_deck_stamps_fires_in_its_rendered_frames() {
    let mut st = Stepper::new(22_050.0, false);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    st.e.set_tempo("A", 300.0);
    let m = st.e.deck("A").unwrap().mirror.clone();
    let mut cursor = EventCursor::new();
    st.step(40); // stopped: frames pass
    let rendered = || m.rendered_frames.load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(rendered(), 40 * 128);
    st.e.play("A");
    st.step(400);
    let (mut f, mut a) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut cursor, &mut f, &mut a);
    assert!(a.len() > 20);
    // the play command is applied at the top of the next block
    assert_eq!(a[0].frame, 40 * 128, "the first tick lands on the first sample after play");
    assert!(a.iter().all(|x| x.frame < rendered()));
    assert!(f.iter().all(|x| x.frame < rendered()));
    assert!(!f.is_empty() && f[0].frame == a[0].frame, "the first tick fires on the first sample");
}

// DeckFireTests.aDeckThatLeavesNetModeStopsFeedingTheNetMirrorsSoReadersGateOnItsMode and
// aSampleDeckProducesNoNetEventsButKeepsCounting
#[test]
fn a_sample_deck_makes_no_net_events_but_keeps_counting() {
    let mut st = Stepper::new(22_050.0, false);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    st.e.set_tempo("A", 300.0);
    st.e.play("A");
    let mut cursor = EventCursor::new();
    st.step(200);
    let (mut f, mut a) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut cursor, &mut f, &mut a);
    assert!(!f.is_empty() && !a.is_empty());
    let tick = st.e.deck("A").unwrap().mirror.tick();
    assert!(tick > 5);
    assert!(st.e.load_demo("A")); // back to a sample: the net is no longer rendered
    st.step(200);
    let m = st.e.deck("A").unwrap().mirror.clone();
    assert_eq!(
        m.rendered_frames.load(std::sync::atomic::Ordering::Acquire),
        400 * 128,
        "the deck's time base goes on"
    );
    let (mut f2, mut a2) = (Vec::new(), Vec::new());
    st.e.read_events("A", &mut cursor, &mut f2, &mut a2);
    assert!(f2.is_empty() && a2.is_empty(), "no events from a sample deck");
    assert_eq!(m.tick(), tick, "the net mirrors stop where they were");
    // a deck that never played a net: the same
    assert!(st.e.load_demo("B"));
    st.e.play("B");
    st.step(50);
    let mut c2 = EventCursor::new();
    let (mut f3, mut a3) = (Vec::new(), Vec::new());
    st.e.read_events("B", &mut c2, &mut f3, &mut a3);
    assert!(f3.is_empty() && a3.is_empty());
}
