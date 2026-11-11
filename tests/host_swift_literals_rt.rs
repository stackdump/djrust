//! A Swift real-time test of the deck that had no exact Rust counterpart (package F of the
//! parity audit): the marking read inside the measured window, across swaps, the performance
//! surface, a scratch, and a stop that resets the marking in place
//! (`ScopeRealTimeTests.aNetDeckWithTapFiresAndMarkingDoesNotAllocate`). Swift reads the marking
//! outside its measured regions; `deck_rt.rs` reads it inside one, but never across a stop, the
//! reset-in-place, or a scrub, so this reads at the end of every counted window.
//!
//! The counter is per thread (a const thread-local, so it never allocates itself), as in
//! `deck_rt.rs`. Composing a track and sending commands happen outside the counted windows; the
//! commands are popped and applied inside them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::fx::MasterMacro;
use djrust::wave::{MarkingSnapshot, NetMacro};
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

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

/// `blocks` blocks of `size` frames in a counted window (nothing allocated, nothing freed), then
/// a read of deck A's marking: the first read after a swap may grow the snapshot to the new
/// project's layout (main-thread work, outside any window), and a second read into the sized
/// snapshot allocates nothing, counted too.
fn measure(what: &str, e: &Engine, r: &mut Renderer, snap: &mut MarkingSnapshot, blocks: usize, size: usize) {
    let (mut l, mut rr) = (vec![0f32; size], vec![0f32; size]);
    let ((), a, f) = count(|| {
        for _ in 0..blocks {
            r.render_block(&mut l, &mut rr);
        }
    });
    assert_eq!(
        (a, f),
        (0, 0),
        "{what}: render allocated {a} times and freed {f} times"
    );
    assert!(e.read_marking("A", snap), "{what}: a marking to read");
    let (ok, a, f) = count(|| e.read_marking("A", snap));
    assert!(ok);
    assert_eq!(
        (a, f),
        (0, 0),
        "{what}: a read into the sized snapshot allocated {a} times and freed {f} times"
    );
}

fn track(e: &mut Engine, genre: &str, seed: f64) -> i32 {
    assert!(e.generate("A", &TrackSpec::new(genre, seed, Some("standard"))));
    // (the generation the new project will have once it is the sounding one)
    e.marking_layout("A").map_or(0, |m| m.generation)
}

#[test]
fn the_marking_reads_without_allocating_across_swaps_a_scratch_and_a_stop() {
    let sr = 22_050.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let first = track(&mut e, "techno", 42.0);
    e.set_tempo("A", 480.0); // dense ticks: many fires, control nets, section changes
    e.play("A");
    let mut snap = MarkingSnapshot::new();
    assert!(e.read_marking("A", &mut snap)); // sized here, once
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    for _ in 0..300 {
        r.render_block(&mut l, &mut rr); // warm
    }
    assert!(e.read_marking("A", &mut snap));
    let seq0 = snap.header.sequence;
    assert_eq!(snap.generation(), first);

    // a project queued behind the playing one, taken over at a bar inside the region
    track(&mut e, "dnb", 3.0);
    e.set_mute("A", "kick", true);
    e.set_mute("A", "kick", false);
    measure("a swap", &e, &mut r, &mut snap, 300, 128);
    e.set_tempo("A", 520.0);
    measure("the second song", &e, &mut r, &mut snap, 500, 128);
    let second = snap.generation();
    assert_ne!(second, first, "the swap landed in the region");

    // the performance surface
    e.play("A");
    e.set_role_muted("A", "bass", true);
    e.set_role_muted("A", "bass", false);
    e.fire("A", NetMacro::Drop, None);
    e.fire("A", NetMacro::BeatRepeat, None);
    e.fire("A", NetMacro::HalfTime, None);
    e.fire("A", NetMacro::TapeStop, None);
    e.fire_stinger("A", "hit1", 16);
    measure("the surface", &e, &mut r, &mut snap, 400, 128);

    // a panic, a cut, and a third song queued in the same block (smaller blocks)
    e.play("A");
    e.panic("A");
    e.fire("A", NetMacro::Cut, None);
    let third = track(&mut e, "house", 4.0);
    measure("a swap at 64", &e, &mut r, &mut snap, 300, 64);
    e.set_tempo("A", 480.0);
    measure("the third song", &e, &mut r, &mut snap, 600, 64);
    assert_eq!(snap.generation(), third);
    e.play("A");

    // scratch: freeze, forward over ticks (9 ticks at 480 bpm = 0.28125 s of platter), back over
    // them and past (un-firing), let go
    e.jog_touch("A", true);
    e.jog_scrub("A", 0.28125);
    measure("scratch forward", &e, &mut r, &mut snap, 150, 128);
    e.jog_scrub("A", -0.4375);
    measure("scratch back", &e, &mut r, &mut snap, 150, 128);
    e.jog_touch("A", false);
    measure("release", &e, &mut r, &mut snap, 300, 512);

    // pause and stop: the stop resets the marking in place, and the reads go on
    e.pause("A");
    e.stop("A");
    measure("stopped", &e, &mut r, &mut snap, 20, 128);
    e.play("A");
    measure("playing again", &e, &mut r, &mut snap, 300, 1024);
    e.fire_master("A", MasterMacro::SweepLp, Some(1));
    measure("a master macro", &e, &mut r, &mut snap, 100, 128);

    // and the plumbing really ran
    assert!(snap.header.sequence > 1 && snap.header.sequence != seq0, "{}", snap.header.sequence);
    assert_eq!(snap.generation(), third);
    e.drain_retired();
    assert_eq!(e.dropped, 0);
}

/// The counter itself works: a test that cannot fail proves nothing.
#[test]
fn the_counter_sees_allocations() {
    let (_, a, f) = count(|| {
        let v = vec![0u8; 1024];
        std::hint::black_box(&v);
    });
    assert!(a >= 1 && f >= 1, "{a} {f}");
}
