//! An input's history, offline (docs/INPUT-HISTORY.md): the strip records what it plays, a
//! jog scrubs back through it, letting go plays a delay, CUE returns to live, a loop repeats
//! whole beats and follows the tempo, and none of it allocates on the render thread.
//!
//! The input is a click on every beat (an impulse of 1.0), so the output's click times say
//! where the head is.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::history::Mode;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

struct Counting;
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
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
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
}
#[global_allocator]
static A: Counting = Counting;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// A beat at 120 BPM.
const BEAT: usize = 24_000;

struct Rig {
    e: Engine,
    r: Renderer,
    feed: rtrb::Producer<f32>,
    /// Frames fed so far.
    fed: usize,
    /// Frames rendered, and everything rendered (left channel).
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.inputs = vec!["TD-3".into()];
        let (mut e, r) = engine(c);
        let (feed, _) = e.take_input_feed(0).unwrap();
        e.set_input_trim(0, 0.0);
        e.set_input_volume(0, 1.0);
        e.set_crossfader(0.5);
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        Rig {
            e,
            r,
            feed,
            fed: 0,
            out: Vec::new(),
        }
    }

    /// Feed and render `frames` (a multiple of the block).
    fn run(&mut self, frames: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..frames / BLOCK {
            for k in 0..BLOCK {
                let f = self.fed + k;
                // a click, then silence; offset 7 so it is not on a block edge
                let x = if f % BEAT == 7 { 1.0 } else { 0.0 };
                let _ = self.feed.push(x);
            }
            self.fed += BLOCK;
            self.r.render_block(&mut l, &mut rr);
            self.out.extend_from_slice(&l);
        }
    }

    /// Output frames (absolute render index) holding a click.
    fn clicks_since(&self, from: usize) -> Vec<usize> {
        let mut v = vec![];
        let mut k = from;
        while k < self.out.len() {
            if self.out[k].abs() > 0.3 {
                v.push(k);
                k += 2000; // one per click
            } else {
                k += 1;
            }
        }
        v
    }
}

#[test]
fn live_is_untouched_and_the_history_fills_in_beats() {
    let mut g = Rig::new();
    g.run(BEAT * 5);
    let m = &g.e.inputs[0].mirror;
    assert_eq!(m.history_mode(), Mode::Live);
    let filled = m.history_filled_beats();
    assert!((4.5..=5.01).contains(&filled), "filled {filled} beats");
    // the live click arrives every beat
    let clicks = g.clicks_since(BEAT);
    assert!(clicks.len() >= 3, "{clicks:?}");
    for w in clicks.windows(2) {
        assert!(((w[1] - w[0]) as i64 - BEAT as i64).abs() <= 1, "{clicks:?}");
    }
}

#[test]
fn scrubbing_back_then_letting_go_plays_a_delay_and_cue_returns_to_live() {
    let mut g = Rig::new();
    g.run(BEAT * 8);
    // the strip waits for a cushion before it plays, so live clicks sit at a fixed phase
    let phase = g.clicks_since(BEAT)[0] % BEAT;
    g.e.input_touch(0);
    g.e.input_scrub_beats(0, -2.0);
    g.run(BLOCK * 60); // the head settles on the wheel (the platter is held)
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Touch);
    g.e.input_release(0);
    g.run(BEAT * 3);
    let m = &g.e.inputs[0].mirror;
    assert_eq!(m.history_mode(), Mode::Play);
    let lag = m.history_lag_beats() as f64;
    // scrubbed two beats, plus the 60 blocks the platter was held while the input played on
    let held = (BLOCK * 60) as f64 / BEAT as f64;
    assert!((lag - (2.0 + held)).abs() < 0.01, "lag {lag} beats");
    // the output's clicks are the input's, `lag` beats late
    let mark = g.out.len() - BEAT * 2;
    let want_delay = (lag * BEAT as f64).round() as i64;
    let got = g.clicks_since(mark);
    assert!(!got.is_empty());
    for c in got {
        let live_click = (c as i64 - want_delay) as usize; // when the live click was fed
        let d = (live_click % BEAT).abs_diff(phase);
        assert!(
            d <= 2 || BEAT - d <= 2,
            "click at {c}: its live time {live_click} is off the beat (phase {phase})"
        );
    }
    // CUE: back to live, no delay
    g.e.input_live(0);
    g.run(BEAT * 2);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Live);
    let mark = g.out.len() - BEAT;
    for c in g.clicks_since(mark) {
        let d = (c % BEAT).abs_diff(phase);
        assert!(d <= 2 || BEAT - d <= 2, "live again: click at {c} (phase {phase})");
    }
}

#[test]
fn a_loop_repeats_whole_beats_and_the_tempo_stretches_it() {
    let mut g = Rig::new();
    g.run(BEAT * 8 + 12_000); // half a beat past a click
    g.e.input_loop(0, 2.0);
    g.run(BEAT * 2); // fade-in settles
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Loop);
    assert_eq!(g.e.inputs[0].mirror.history_loop_beats(), Some(2.0));
    // the loop holds 2 beats = two clicks; they repeat every 2 beats
    let mark = g.out.len();
    g.run(BEAT * 6);
    let c = g.clicks_since(mark);
    assert!(c.len() >= 5, "{c:?}");
    for w in c.windows(2) {
        assert!(((w[1] - w[0]) as i64 - BEAT as i64).abs() <= 2, "{c:?}");
    }
    assert!(((c[2] - c[0]) as i64 - 2 * BEAT as i64).abs() <= 2);
    // 150 BPM: the loop plays 1.25x faster, so its clicks come every 19 200 frames
    g.e.set_free_bpm(150.0);
    g.e.sync_history_tempo();
    g.run(BLOCK * 4);
    let mark = g.out.len();
    g.run(BEAT * 6);
    let c = g.clicks_since(mark);
    assert!(c.len() >= 5, "{c:?}");
    for w in c.windows(2) {
        assert!(
            ((w[1] - w[0]) as i64 - 19_200).abs() <= 3,
            "click spacing {} at 150 BPM: {c:?}",
            w[1] - w[0]
        );
    }
    // back to live
    g.e.input_live(0);
    g.run(BEAT);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Live);
}

#[test]
fn the_window_counts_beats_so_a_faster_tempo_holds_fewer_frames() {
    let mut g = Rig::new();
    g.e.set_input_history_beats(0, 4.0);
    g.run(BEAT * 6);
    let f = g.e.inputs[0].mirror.history_filled_beats();
    assert!((f - 4.0).abs() < 0.01, "4-beat window, filled {f}");
    // a longer window
    g.e.set_input_history_beats(0, 16.0);
    g.run(BEAT * 2);
    let f = g.e.inputs[0].mirror.history_filled_beats();
    assert!((7.9..=8.2).contains(&f), "16-beat window with 8 beats recorded: {f}");
}

#[test]
fn the_jog_wheel_of_the_aimed_deck_scrubs_the_input() {
    use djrust::control::controller::Action;
    let mut g = Rig::new();
    g.run(BEAT * 6);
    g.e.set_history_jog(Some(("A", djrust::host::deck::strip_recall::RecallId::Input(0))));
    // the platter on deck A: touch, turn back half a second, let go
    g.e.jog_touch("A", true);
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Touch);
    g.e.jog_scrub("A", -0.5);
    g.run(BLOCK * 80);
    g.e.jog_touch("A", false);
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Play);
    let lag = g.e.inputs[0].mirror.history_lag_beats() as f64 * BEAT as f64;
    let wanted = 0.5 * SR + (BLOCK * 84) as f64;
    // the platter was held for a few blocks first
    assert!(lag >= 0.5 * SR && lag <= wanted + BLOCK as f64 * 4.0, "lag {lag}");
    // CUE goes live, LOOP IN loops, HALVE halves
    g.e.apply_action(Action::Cue { deck: "A".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Live);
    g.e.apply_action(Action::LoopIn { deck: "A".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_loop_beats(), Some(4.0));
    g.e.apply_action(Action::LoopHalve { deck: "A".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_loop_beats(), Some(2.0));
    g.e.apply_action(Action::LoopDouble { deck: "A".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_loop_beats(), Some(4.0));
    g.e.apply_action(Action::LoopOut { deck: "A".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Live);
    // un-aimed, deck A's wheel is its own again
    g.e.set_history_jog(None);
    g.e.jog_touch("A", true);
    g.run(BLOCK * 2);
    assert_eq!(g.e.inputs[0].mirror.history_mode(), Mode::Live);
}

#[test]
fn a_strip_with_no_history_ignores_all_of_it() {
    let mut c = EngineConfig::new(SR);
    c.inputs = vec!["TD-3".into()];
    c.history_beats = None;
    let (mut e, _r) = engine(c);
    assert!(!e.input_touch(0));
    assert!(!e.input_loop(0, 4.0));
    assert!(!e.set_input_history_beats(0, 8.0));
}

#[test]
fn the_render_path_with_a_history_allocates_nothing() {
    let mut g = Rig::new();
    g.run(BEAT * 4);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let measured = |g: &mut Rig, what: &str, l: &mut [f32], rr: &mut [f32]| {
        ALLOCS.with(|c| c.set(0));
        ON.with(|o| o.set(true));
        for _ in 0..400 {
            for k in 0..BLOCK {
                let _ = g.feed.push(if (g.fed + k) % BEAT == 7 { 1.0 } else { 0.0 });
            }
            g.fed += BLOCK;
            g.r.render_block(l, rr);
        }
        ON.with(|o| o.set(false));
        let a = ALLOCS.with(|c| c.get());
        assert_eq!(a, 0, "{what}: {a} allocations/frees on the render thread");
    };
    measured(&mut g, "live", &mut l, &mut rr);
    g.e.input_touch(0);
    g.e.input_scrub(0, -0.7);
    measured(&mut g, "scrubbing", &mut l, &mut rr);
    g.e.input_release(0);
    measured(&mut g, "delay", &mut l, &mut rr);
    g.e.input_loop(0, 8.0);
    measured(&mut g, "loop", &mut l, &mut rr);
    g.e.input_live(0);
    measured(&mut g, "live again", &mut l, &mut rr);
}
