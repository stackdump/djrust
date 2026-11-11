//! Swift real-time tests of the runner that had no Rust counterpart (package F of the parity
//! audit): a swept laser lane renders without allocating
//! (`WaveDeckSourceTests.laserSweepsDoNotAllocate`). Measured, not asserted: a counting global
//! allocator, per thread (a const thread-local, so it never allocates itself), as in
//! `mirrors_rt.rs`. Compiling and loading happen outside the counted window.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::wave::{WaveOptions, WaveRunner};
use djrust::{compose, JsonObject};

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

// WaveDeckSourceTests.laserSweepsDoNotAllocate: a song whose melody is a laser (synth voice: no
// sampler bank here), tempo 480, 200 blocks of 128 to warm, then 3000 measured. A laser is a
// pitch sweep (`pm_on`), the lane the Swift test was written to measure.
#[test]
fn laser_sweeps_do_not_allocate() {
    let sr = 48_000.0;
    let opts = WaveOptions::extended();
    let mut ov = JsonObject::new();
    ov.set("seed", 7.0.into());
    ov.set("structure", "minimal".into());
    let mut p = compose("trance", &ov.into());
    let mut lasers = 0;
    for id in p.nets.keys().to_vec() {
        let net = p.nets.get_mut(&id).unwrap();
        if net.track.group.as_deref() == Some("melody") {
            net.track.instrument = "laser".into();
            lasers += 1;
        }
    }
    let prepared = WaveRunner::prepare(&p, sr, &opts, 0);
    assert!(
        lasers > 0 && prepared.lanes().iter().any(|l| l.pm_on()),
        "a swept lane to measure"
    );
    let mut runner = WaveRunner::new(sr, opts);
    runner.load_prepared(prepared);
    runner.set_tempo(480.0);
    runner.play();
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    let mut peak = 0f32;
    let mut run = |runner: &mut WaveRunner, n: usize, frame: &mut i64| {
        for _ in 0..n {
            runner.process_at(&mut l, Some(&mut r), 128, *frame);
            *frame += 128;
            peak = l.iter().chain(&r).fold(peak, |m, v| m.max(v.abs()));
        }
    };
    let mut frame = 0i64;
    run(&mut runner, 200, &mut frame);
    // (Swift measures 3000 blocks; 1500 is 3.8 s of audio at tempo 480, some 120 laser notes,
    // and keeps the test under a second or two at the test profile's speed)
    let (_, allocs, frees) = count(|| run(&mut runner, 1500, &mut frame));
    assert_eq!(allocs, 0, "process allocated {allocs} times");
    assert_eq!(frees, 0, "process freed {frees} times");
    // the song was heard: the measured window is not silence
    assert!(peak > 0.01, "peak {peak}");
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
