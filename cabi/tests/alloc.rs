//! Zero allocations on the audio thread: `djw_process` and every 0.3.0 message export, counted by
//! a global allocator. `djw_init`, `djw_init_with` and `djw_load` are the calls allowed to
//! allocate (and 0.2.0's text reports, `djw_report` / `djw_marking` / `djw_muted` /
//! `djw_compose` / `djw_keys`, which are not message exports).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use djwasm::*;

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static COUNT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Bytes allocated and not yet freed on this thread.
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn live(delta: isize) {
    // `try_with`: the thread-local may be gone while a thread is torn down
    let _ = LIVE.try_with(|v| v.set(v.get() + delta));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        live(l.size() as isize);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        live(-(l.size() as isize));
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        live(n as isize - l.size() as isize);
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Allocations `f` made on this thread.
fn counted(f: impl FnOnce()) -> usize {
    let before = COUNT.load(Ordering::Relaxed);
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.load(Ordering::Relaxed) - before
}

fn process_n(n: usize) {
    for _ in 0..n {
        djw_process(Q as u32);
    }
}

/// Every message export, every path through it (success, unsupported, refused), with the
/// engine busy: control macros on over a project whose control nets fire net, master, aux and
/// unknown ids; net macros fired, stacked, restored and panicked; tempo macros sweeping; seeks
/// while playing and stopped; groove on a groove instance; the event ring filled past its
/// capacity and drained.
#[test]
fn process_and_every_message_export_allocate_nothing() {
    let _g = lock();
    for flags in [0, DJW_GROOVE] {
        assert_eq!(djw_init_with(48000.0, flags), 4096);
        let doc = Source::Compose(MACRO_CURVE_SPEC.into()).doc();
        assert!(call(djw_load, &doc) > 0);
        let ids = catalog_ids();
        // no warm-up: nothing (the first block, the first fire) may be special
        let (late, mask, early) = (Cell::new(0u32), Cell::new(0u64), Cell::new(0u32));
        let n = counted(|| {
            djw_set_control_macros(1);
            disable("half-time", true);
            disable("bogus", true);
            disable("half-time", false);
            djw_set_groove(30.0, 20.0);
            djw_play();
            process_n(3000); // control fires: beat-repeat, drop (8 s in)
            early.set(djw_event_count());
            for id in &ids {
                fire(id, -1);
                fire(id, 2);
                fire(id, 0);
            }
            mask.set(djw_macro_mask());
            fire("bogus", -1);
            fire("drop", -9);
            for id in ["drop", "sweep-lp", "bogus"] {
                unsafe { djw_release_macro(id.as_ptr(), id.len()) };
            }
            process_n(400); // restores, tempo sweeps (tape-stop), the ring overflowing
            djw_macro_mask();
            djw_tempo();
            djw_frame();
            djw_event_count();
            djw_events();
            djw_events();
            fire("half-time", 1);
            fire("tape-stop", 1);
            process_n(50);
            djw_panic();
            djw_panic();
            djw_events();
            djw_seek(0.0);
            djw_seek(123.0);
            djw_seek(-4.0);
            djw_seek(f64::NAN);
            process_n(200);
            djw_pause();
            djw_seek(40.0);
            djw_play();
            process_n(200);
            djw_stop();
            djw_seek(7.0);
            djw_play();
            djw_set_groove(0.0, 0.0);
            djw_set_groove(f64::INFINITY, 0.0);
            djw_set_tempo(133.0);
            djw_seek(440.0); // just before the curve's master (449), aux (641), unknown (897)
            process_n(21000);
            late.set(djw_event_count());
            djw_set_control_macros(0);
            process_n(100);
            djw_events();
            djw_features();
        });
        assert_eq!(n, 0, "flags {flags}: {n} allocations");
        // the paths ran: control fires early (net) and late (master, aux, unknown), macros held
        assert!(early.get() >= 2, "early control events {}", early.get());
        assert!(late.get() >= 3, "late control events {}", late.get());
        assert_ne!(mask.get(), 0);
    }
}

/// The calls that may allocate do (a sanity check that the counter counts).
#[test]
fn the_counter_counts() {
    let _g = lock();
    let n = counted(|| {
        djw_init(48000.0);
    });
    assert!(n > 0);
}

/// `djw_load` frees the projects the runner retired (swapped out at a bar, faded out), so a
/// session of many loads holds a bounded few projects, not one per load (0.2.0 kept the
/// runner's whole retire list, up to 32 projects of about 9 MB each in wasm).
#[test]
fn loads_free_the_retired_projects() {
    let _g = lock();
    let docs: Vec<String> = [
        r#"{"genre":"techno","seed":42,"structure":"standard"}"#,
        r#"{"genre":"jazz","seed":7}"#,
    ]
    .iter()
    .map(|s| Source::Compose(s.to_string()).doc())
    .collect();
    let now = || LIVE.with(|v| v.get());
    assert_eq!(djw_init(48000.0), 4096);
    let empty = now();
    assert!(call(djw_load, &docs[0]) > 0);
    let one = now() - empty;
    djw_play();
    let mut after = Vec::new();
    for i in 0..40 {
        process_n(800); // past a bar: the pending project swaps in, the old one rings out
        assert!(call(djw_load, &docs[i % 2]) > 0);
        after.push(now());
    }
    let growth = after[39] - after[5];
    assert!(
        growth < one / 2,
        "35 loads grew the live bytes by {growth} (one project is {one})"
    );
}
