//! The mirrors hold to the audio-thread rules, measured, not asserted
//! (`MacroMirrorRealTimeTests.publishingMacroStateOnTheAudioThreadDoesNotAllocate`): across
//! mute timers, a delayed half, the beat repeater and tempo effects, a block of the runner plus
//! the mirrors' publication (`WaveDeckSource.render`'s epilogue) allocates nothing and frees
//! nothing. The reader side is measured too (a sized snapshot reads without allocating).
//!
//! The counter is per thread (a const thread-local, so it never allocates itself), as in
//! `crates/djhost/tests/deck_rt.rs`. The main side's work (compiling a project, queueing
//! commands) happens outside the counted windows; the commands are popped and applied inside
//! them only in the first test's warm-up, never in a measured window.
//!
//! This file measures the mirrors alone. They are on `djhost`'s `DeckCore` render path too (the
//! project owns its marking mirror and the runner publishes it each block), where
//! `deck_rt.rs` and `djhost/tests/swift_literals_rt.rs` measure them.

mod mirror_support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::wave::*;
use mirror_support::*;

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.with(|o| o.get()) {
            FREES.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// (allocations, frees) made by `f` on this thread.
fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    (r, ALLOCS.with(|c| c.get()), FREES.with(|c| c.get()))
}

// MacroMirrorRealTimeTests.publishingMacroStateOnTheAudioThreadDoesNotAllocate
// (runs in an optimized build here too: the test profile is opt-level 2)
#[test]
fn publishing_macro_state_on_the_audio_thread_does_not_allocate() {
    let (src, _) = make_source(&project("techno", 42.0, "loop"), 480.0, 48_000.0);
    let mut d = Driver::new(src);
    d.run(200); // warm up
                // every kind of macro at once: mute timers, a delayed half, the repeater, tempo effects
    d.src.fire(NetMacro::Drop, Some(24));
    d.src.fire(NetMacro::BeatRepeat, Some(24));
    d.src.fire(NetMacro::DoubleDrop, Some(8));
    d.src.fire(NetMacro::TapeStop, Some(8));
    d.run(2); // the commands go through (outside the measured region)
    let (mut allocs, mut frees) = (0, 0);
    let mut seen: Vec<NetMacro> = Vec::new();
    let mut max_held = 0;
    for _ in 0..60 {
        let (_, a, f) = count(|| d.run(20));
        allocs += a;
        frees += f;
        for m in d.src.active_macros() {
            if !seen.contains(&m) {
                seen.push(m);
            }
        }
        let held = (0..64).filter(|&i| d.src.is_net_held_by_macro(i)).count();
        max_held = max_held.max(held);
    }
    assert!(
        allocs == 0 && frees == 0,
        "{allocs} allocations and {frees} frees in 1200 blocks"
    );
    for m in [
        NetMacro::Drop,
        NetMacro::BeatRepeat,
        NetMacro::DoubleDrop,
        NetMacro::TapeStop,
    ] {
        assert!(seen.contains(&m), "the mirror carried every macro: {seen:?}");
    }
    assert!(max_held >= 3, "and held nets: {max_held}");
    assert!(
        d.src.active_macros().is_empty(),
        "all of them ran out inside the measured blocks"
    );
}

/// The marking mirror: a project that changes every tick, published and read back around the
/// runner, over a long stretch with project swaps outside the windows.
#[test]
fn publishing_and_reading_the_marking_does_not_allocate() {
    let rate = 48_000.0;
    let mut src = Source::new(rate);
    let first = src.load(prepare(&project("house", 7.0, "standard"), rate, 0, None));
    src.set_tempo(480.0);
    src.play();
    let mut d = Driver::new(src);
    d.run(200);
    let mut snap = MarkingSnapshot::new();
    assert!(d.src.read_marking(&mut snap)); // sized here, once
    d.src.fire(NetMacro::Cut, Some(8));
    d.src.set_role_muted("bass", true);
    d.run(2);
    let mut reads = 0;
    let mut sequences: Vec<i64> = Vec::new();
    let (mut allocs, mut frees) = (0, 0);
    for _ in 0..60 {
        let (ok, a, f) = count(|| {
            d.run(20);
            // main-thread reads, same thread here: still allocation-free once sized
            d.src.read_marking(&mut snap)
        });
        assert!(ok);
        allocs += a;
        frees += f;
        reads += 1;
        if sequences.last() != Some(&snap.header.sequence) {
            sequences.push(snap.header.sequence);
        }
        assert_eq!(snap.generation(), first.generation);
    }
    assert!(
        allocs == 0 && frees == 0,
        "{allocs} allocations and {frees} frees over {reads} windows of 20 blocks"
    );
    assert!(
        sequences.len() > 30,
        "the marking changed and was published: {} changes",
        sequences.len()
    );
}

/// The counter itself works: a test that cannot fail proves nothing.
#[test]
fn the_counter_sees_allocations() {
    let (v, a, f) = count(|| {
        let v = vec![1u8; 100];
        let w = v.clone();
        drop(w);
        v
    });
    assert_eq!(v.len(), 100);
    assert_eq!((a, f), (2, 1));
    // and the mirrors' own constructors, which allocate by design, are seen
    let p = project("techno", 42.0, "loop");
    let wp = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::default(), 0);
    let (_m, a, _) = count(|| MarkingMirror::new(wp.generation, 1, 0, &wp.graph, &p));
    assert!(a > 5, "building a mirror allocates (layout, buffers): {a}");
    let (_m, a, _) = count(MacroMirror::new);
    assert!(a >= 1);
}
