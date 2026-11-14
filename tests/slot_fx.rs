//! The catalogue's audio tricks on a strip with no deck (host::deck::slot_fx): an input strip
//! (the phone's Wheel) and the master. They fire like a deck's (on the lead deck's bar, for bars
//! or latched, a transition held), work the sound where the module says, refuse what only a
//! deck can do, and render without allocating.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::host::deck::offline::OfflineConductor;
use djrust::host::deck::{engine, Engagement, Engine, EngineConfig, Renderer, TrickSlot};
use djrust::TrackSpec;

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

const SR: f64 = 48_000.0;
const BLOCK: usize = 192;
const WHEEL: TrickSlot = TrickSlot::Input(0);

/// No deck playing; one input strip, "wheel", fed a 220 Hz tone at full volume.
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

/// Render `secs` with the tone fed and the service tick run, the output's RMS over its last
/// quarter.
fn listen(e: &mut Engine, r: &mut Renderer, feed: &mut rtrb::Producer<f32>, at: &mut usize, secs: f64) -> f32 {
    let blocks = (secs * SR / BLOCK as f64) as usize;
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut sum, mut count) = (0f64, 0usize);
    for b in 0..blocks {
        tone(feed, at, BLOCK);
        r.render_block(&mut l, &mut rr);
        e.sync_trick_tempo();
        if b >= blocks * 3 / 4 {
            sum += l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            count += BLOCK;
        }
    }
    (sum / count.max(1) as f64).sqrt() as f32
}

#[test]
fn only_the_audio_tricks_play_on_a_slot() {
    let (mut e, _r, _f) = rig();
    for id in ["stutter", "pump", "filter", "delay", "wash"] {
        assert!(e.engage_slot(WHEEL, id, Engagement::Bars(1)).is_ok(), "{id}");
        assert!(e.engage_slot(TrickSlot::Master, id, Engagement::Latch).is_ok(), "{id}");
    }
    for id in ["riser", "tape_stop", "spin_back", "verb_freeze"] {
        assert!(e.engage_slot(WHEEL, id, Engagement::Momentary).is_ok(), "{id}");
        assert!(e.engage_slot(WHEEL, id, Engagement::Bars(2)).is_err(), "{id}: a one-shot has no bar length");
    }
    // a deck's tempo, its parts, its own chain and sends: not a slot's
    for id in ["half_time", "double_time", "drop", "cut", "beat-repeat", "build-crush", "feel-build", "cathedral", "mono"] {
        let err = e.engage_slot(WHEEL, id, Engagement::Bars(1)).unwrap_err();
        assert!(err.contains("plays on a deck"), "{id}: {err}");
    }
    assert!(e.engage_slot(TrickSlot::Input(3), "stutter", Engagement::Bars(1)).is_err(), "no such input");
    assert!(e.engage_slot(WHEEL, "nope", Engagement::Latch).is_err());
}

#[test]
fn a_tape_stop_on_the_wheel_stops_the_wheel_and_lets_go() {
    let (mut e, mut r, mut feed) = rig();
    let mut at = 0;
    let open = listen(&mut e, &mut r, &mut feed, &mut at, 1.0);
    assert!(open > 0.02, "the tone through the wheel's strip: {open}");
    // no deck plays: at once
    e.engage_slot(WHEEL, "tape_stop", Engagement::Momentary).unwrap();
    assert_eq!(e.fx_state_slot(WHEEL).len(), 1);
    assert!(e.fx_state_slot(TrickSlot::Master).is_empty(), "the master has nothing of it");
    let stopped = listen(&mut e, &mut r, &mut feed, &mut at, 3.0);
    assert!(stopped < 0.05 * open, "stopped: {stopped} (open {open})");
    e.release_slot(WHEEL, "tape_stop").unwrap();
    assert!(e.fx_state_slot(WHEEL).is_empty());
    let back = listen(&mut e, &mut r, &mut feed, &mut at, 1.0);
    assert!(back > 0.5 * open, "back: {back} (open {open})");
}

#[test]
fn a_tape_stop_on_the_master_stops_everything() {
    let (mut e, mut r, mut feed) = rig();
    let mut at = 0;
    let open = listen(&mut e, &mut r, &mut feed, &mut at, 1.0);
    e.engage_slot(TrickSlot::Master, "tape_stop", Engagement::Momentary).unwrap();
    assert!(e.fx_state_slot(WHEEL).is_empty());
    let stopped = listen(&mut e, &mut r, &mut feed, &mut at, 3.0);
    assert!(stopped < 0.05 * open, "stopped: {stopped} (open {open})");
    e.clear_slot_engagements(TrickSlot::Master);
    let back = listen(&mut e, &mut r, &mut feed, &mut at, 1.0);
    assert!(back > 0.5 * open, "PANIC lets it go: {back}");
}

#[test]
fn a_bar_length_ends_by_itself() {
    let (mut e, mut r, mut feed) = rig();
    let mut at = 0;
    e.engage_slot(WHEEL, "stutter", Engagement::Bars(1)).unwrap();
    assert_eq!(e.fx_state_slot(WHEEL).len(), 1);
    // a bar at the master tempo (120 when nothing plays: 2 s), and a little
    listen(&mut e, &mut r, &mut feed, &mut at, 3.0);
    assert!(e.fx_state_slot(WHEEL).is_empty(), "{:?}", e.fx_state_slot(WHEEL));
}

#[test]
fn with_a_deck_playing_a_slot_waits_for_its_bar() {
    let mut cfg = EngineConfig::new(44_100.0);
    cfg.inputs = vec!["wheel".into()];
    let (mut e, r) = engine(cfg);
    e.deterministic = true;
    let mut c = OfflineConductor::new(e, r);
    {
        let e = c.engine_mut();
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        e.set_volume("A", 0.9);
        e.set_crossfader(0.0);
        e.play("A");
    }
    c.pump();
    c.run_to(OfflineConductor::BLOCK_FRAMES as i64 * 40);
    assert_eq!(c.engine().slot_lead().as_deref(), Some("A"));
    c.engine_mut().engage_slot(WHEEL, "stutter", Engagement::Latch).unwrap();
    let s = c.engine().fx_state_slot(WHEEL);
    assert!(s[0].latched && s[0].pending, "waiting for A's bar: {s:?}");
    let (bar, _) = c.engine().sample_master_fire("A");
    while c.engine().frame() < bar + 2 * OfflineConductor::BLOCK_FRAMES as i64 {
        c.step_block();
    }
    let s = c.engine().fx_state_slot(WHEEL);
    assert!(!s[0].pending && s[0].latched, "on, on the bar: {s:?}");
}

#[test]
fn slot_tricks_render_without_allocating() {
    let (mut e, mut r, mut feed) = rig();
    let mut at = 0;
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for _ in 0..50 {
        tone(&mut feed, &mut at, BLOCK);
        r.render_block(&mut l, &mut rr);
    }
    let ids = ["stutter", "pump", "filter", "delay", "wash", "verb_freeze", "riser", "tape_stop", "spin_back"];
    let mut allocs = 0;
    for b in 0..2_000usize {
        if b % 40 == 0 {
            let id = ids[(b / 40) % ids.len()];
            let slot = if b % 80 == 0 { WHEEL } else { TrickSlot::Master };
            let mode = if matches!(id, "riser" | "tape_stop" | "spin_back" | "verb_freeze") { Engagement::Momentary } else { Engagement::Bars(1) };
            let _ = e.engage_slot(slot, id, mode);
        }
        if b % 40 == 20 {
            let _ = e.slot_knob(WHEEL, (b % 7) as f64 / 6.0);
            e.clear_slot_engagements(TrickSlot::Master);
        }
        tone(&mut feed, &mut at, BLOCK);
        ALLOCS.with(|c| c.set(0));
        ON.with(|o| o.set(true));
        r.render_block(&mut l, &mut rr);
        ON.with(|o| o.set(false));
        allocs += ALLOCS.with(|c| c.get());
        e.sync_trick_tempo();
        assert!(l.iter().chain(&rr).all(|v| v.is_finite()), "block {b}");
    }
    assert_eq!(allocs, 0, "the render thread allocated");
}
