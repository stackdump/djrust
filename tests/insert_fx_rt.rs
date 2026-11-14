//! Insert effects on the audio thread ([`djrust::host::deck::inserts`]): toggling them and
//! moving their knobs while the mixer renders allocates nothing, frees nothing, never misses a
//! block's deadline, and makes no click.
//!
//! The allocation counter is per thread (a const thread-local, as in `mirrors_rt.rs`): the
//! main side's work (building a chain, queueing switches) happens outside the counted windows;
//! a chain's arrival (the boxed install) and every switch and knob are popped and applied
//! inside them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use djrust::fx::insert::{division_knob, InsertChain, InsertKind, InsertParam, InsertSettings};
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

fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    (r, ALLOCS.with(|c| c.get()), FREES.with(|c| c.get()))
}

const SR: f64 = 48_000.0;
/// A phone-sized callback: 192 frames, 4 ms.
const BLOCK: usize = 192;

fn rig() -> (Engine, Renderer, rtrb::Producer<f32>) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    c.inputs = vec!["wheel".into()];
    let (mut e, r) = engine(c);
    let (feed, _) = e.take_input_feed(0).expect("the wheel's ring");
    e.set_crossfader(0.5);
    e.set_input_volume(0, 1.0);
    (e, r, feed)
}

fn tone(feed: &mut rtrb::Producer<f32>, at: &mut usize, n: usize) {
    for _ in 0..n {
        let t = *at as f32 / SR as f32;
        let _ = feed.push(0.5 * (2.0 * std::f32::consts::PI * 220.0 * t).sin());
        *at += 1;
    }
}

/// A small deterministic generator for the toggles.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 10_000) as f64 / 10_000.0
    }
}

#[test]
fn toggling_inserts_while_rendering_allocates_nothing_and_never_underruns() {
    let (mut e, mut r, mut feed) = rig();
    let targets = e.insert_targets();
    assert_eq!(targets, vec!["A", "B", "input:wheel", "master"]);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut at = 0;
    tone(&mut feed, &mut at, 4096);
    // warm up the mixer itself (outside the measured windows)
    for _ in 0..50 {
        tone(&mut feed, &mut at, BLOCK);
        r.render_block(&mut l, &mut rr);
    }
    // every chain is built on this thread now; its install is popped inside a counted block
    for t in &targets {
        assert!(e.set_insert_on(t, InsertKind::Delay, true), "{t}");
    }
    let budget = Duration::from_secs_f64(BLOCK as f64 / SR);
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let (mut allocs, mut frees) = (0, 0);
    let mut rng = Lcg(7);
    let blocks = 3_000;
    for b in 0..blocks {
        // the hand: a few switches and knobs between blocks, on any target
        for _ in 0..(rng.next() % 4) {
            let t = &targets[(rng.next() % targets.len() as u64) as usize];
            if rng.next() % 2 == 0 {
                let k = InsertKind::ALL[(rng.next() % 4) as usize];
                e.set_insert_on(t, k, rng.next() % 3 != 0);
            } else {
                let p = InsertParam::ALL[(rng.next() % InsertParam::COUNT as u64) as usize];
                e.set_insert_param(t, p, rng.unit());
            }
        }
        if b % 500 == 250 {
            // everything on everywhere for a while: the worst case
            for t in &targets {
                for k in InsertKind::ALL {
                    e.set_insert_on(t, k, true);
                }
            }
        }
        tone(&mut feed, &mut at, BLOCK);
        let t0 = Instant::now();
        let (_, a, f) = count(|| r.render_block(&mut l, &mut rr));
        let dt = t0.elapsed();
        allocs += a;
        frees += f;
        worst = worst.max(dt);
        total += dt;
        assert!(l.iter().chain(&rr).all(|v| v.is_finite()), "block {b} is not finite");
    }
    assert_eq!((allocs, frees), (0, 0), "the render thread allocated or freed");
    assert!(e.dropped == 0, "the command ring overflowed: {}", e.dropped);
    let mean = total / blocks;
    println!(
        "insert render: mean {:?}, worst {:?}, budget {:?} ({} frames)",
        mean, worst, budget, BLOCK
    );
    assert!(worst < budget, "a block took {worst:?}, the deadline is {budget:?}");
}

/// The largest second difference of a signal: a sine of 220 Hz at 0.5 has 4e-4; a step of
/// `d` shows as about `d`.
fn roughness(v: &[f32]) -> f32 {
    v.windows(3).map(|w| (w[0] - 2.0 * w[1] + w[2]).abs()).fold(0.0, f32::max)
}

fn sine_block(at: &mut usize, n: usize) -> (Vec<f32>, Vec<f32>) {
    let v: Vec<f32> = (0..n)
        .map(|_| {
            let x = (0.5 * (2.0 * std::f64::consts::PI * 220.0 * *at as f64 / SR).sin()) as f32;
            *at += 1;
            x
        })
        .collect();
    (v.clone(), v)
}

/// Run `blocks` blocks of the sine through `c`, calling `hand` before each, and return the
/// output's roughness.
fn run(c: &mut InsertChain, blocks: usize, mut hand: impl FnMut(&mut InsertChain, usize)) -> f32 {
    let mut at = 0;
    let mut out = Vec::new();
    for b in 0..blocks {
        hand(c, b);
        let (mut l, mut r) = sine_block(&mut at, BLOCK);
        c.process(&mut l, &mut r);
        out.extend_from_slice(&l);
        out.extend_from_slice(&r[..0]);
    }
    roughness(&out)
}

#[test]
fn switching_an_effect_on_and_off_makes_no_click() {
    let dry = run(&mut InsertChain::new(SR), 200, |_, _| {});
    for k in [InsertKind::Filter, InsertKind::Delay, InsertKind::Reverb] {
        let mut c = InsertChain::new(SR);
        let mut s = InsertSettings::default();
        // the most audible settings for a switch: full wet, the filter far down
        s.set(InsertParam::FilterSweep, 0.08);
        s.set(InsertParam::FilterMix, 1.0);
        s.set(InsertParam::DelayMix, 1.0);
        s.set(InsertParam::ReverbMix, 1.0);
        c.apply(&s, true);
        let rough = run(&mut c, 400, |c, b| {
            if b % 20 == 0 {
                c.set_on(k, b % 40 == 0);
            }
        });
        println!("{:?}: roughness {rough:.5} (dry {dry:.5})", k);
        assert!(rough < 0.01, "{k:?} clicks when switched: {rough}");
    }
}

#[test]
fn knobs_jumping_make_no_click() {
    type Hand = fn(&mut InsertChain, bool);
    let hands: [(&str, Hand); 4] = [
        ("filter sweep", |c, far| c.set(InsertParam::FilterSweep, if far { 0.1 } else { 0.9 })),
        ("delay division", |c, far| c.set(InsertParam::DelayTime, division_knob(if far { 0 } else { 7 }))),
        ("delay wet", |c, far| c.set(InsertParam::DelayMix, if far { 1.0 } else { 0.0 })),
        ("tempo", |c, far| c.set_bpm(if far { 90.0 } else { 140.0 })),
    ];
    for (name, hand) in hands {
        let mut c = InsertChain::new(SR);
        let mut s = InsertSettings::default();
        s.set_on(InsertKind::Filter, true);
        s.set_on(InsertKind::Delay, true);
        s.set(InsertParam::DelayMix, 0.6);
        s.set(InsertParam::FilterRes, 0.0);
        c.apply(&s, true);
        c.set_bpm(124.0);
        let rough = run(&mut c, 400, |c, b| {
            if b % 25 == 0 {
                hand(c, b % 50 == 0);
            }
        });
        println!("{name}: roughness {rough:.5}");
        assert!(rough < 0.01, "{name} clicks: {rough}");
    }
}

/// The cost of each effect on one stereo strip, as a share of real time at 48 kHz (printed;
/// asserted loosely so a slow CI box does not fail it).
#[test]
fn cpu_cost_per_effect() {
    let n = 48_000 * 2;
    let measure = |on: &[InsertKind]| {
        let mut c = InsertChain::new(SR);
        let mut s = InsertSettings::default();
        for k in on {
            s.set_on(*k, true);
        }
        s.set(InsertParam::FilterSweep, 0.2);
        c.apply(&s, true);
        let mut at = 0;
        let blocks: Vec<(Vec<f32>, Vec<f32>)> = (0..n / BLOCK).map(|_| sine_block(&mut at, BLOCK)).collect();
        let t0 = Instant::now();
        for (l, r) in blocks {
            let (mut l, mut r) = (l, r);
            c.process(&mut l, &mut r);
            std::hint::black_box(&l);
        }
        t0.elapsed().as_secs_f64() / (n as f64 / SR)
    };
    let none = measure(&[]);
    let mut all = 0.0;
    for k in InsertKind::ALL {
        let x = measure(&[k]);
        println!("{:>8}: {:.3} % of real time per stereo strip", k.label(), x * 100.0);
    }
    all += measure(&InsertKind::ALL);
    println!("     off: {:.4} %   all four: {:.3} %", none * 100.0, all * 100.0);
    assert!(all < 0.25, "the whole chain takes {:.1} % of real time", all * 100.0);
}
