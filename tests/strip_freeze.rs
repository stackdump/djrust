//! Freeze, the jog and the history loop on any strip, offline (docs/INPUT-HISTORY.md, "Any
//! strip"): the decks, the loop row L, the vocoder V and the master M keep a history like the
//! input strips, so FRZ, `freeze` / `freezeKnob` bindings, a jog aimed at one and the snap back
//! to live work the same everywhere.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::fx::granular::FreezeParam;
use djrust::history::Mode;
use djrust::TrackSpec;
use djrust::host::deck::strip_recall::{RecallId, StripId};
use djrust::host::deck::vocoder::{CarrierPick, VocoderSwitch};
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

struct Rig {
    e: Engine,
    r: Renderer,
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.history_beats = Some(8.0);
        c.strip_history = true;
        let (mut e, r) = engine(c);
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        Rig {
            e,
            r,
            out: Vec::new(),
        }
    }

    fn run(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.out.clear();
        for _ in 0..blocks {
            self.r.render_block(&mut l, &mut rr);
            self.e.sync_history_tempo();
            self.e.service_loops();
            self.out.extend_from_slice(&l);
        }
    }

    /// RMS of the last run's second half.
    fn level(&self) -> f32 {
        let x = &self.out[self.out.len() / 2..];
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Deck A playing a track, alone in the master.
    fn deck_a(&mut self) {
        self.e.set_crossfader(0.0);
        self.e.set_volume("A", 1.0);
        assert!(self.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        self.e.play("A");
        self.run(800); // ~4 s: the history has beats in it
    }
}

fn target(id: &str, value: f64, pressed: bool, args: &[(&str, &str)]) -> Action {
    Action::Target {
        id: id.into(),
        deck: None,
        slot: None,
        index: 0,
        value,
        pressed,
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn every_strip_has_a_history_when_asked() {
    let g = Rig::new();
    let e = &g.e;
    assert_eq!(e.recall_id("A"), Some(RecallId::Strip(StripId::Deck(0))));
    assert_eq!(e.recall_id("b"), Some(RecallId::Strip(StripId::Deck(1))));
    assert_eq!(e.recall_id("L"), Some(RecallId::Strip(StripId::Loops)));
    assert_eq!(e.recall_id("vocoder"), Some(RecallId::Strip(StripId::Vocoder)));
    assert_eq!(e.recall_id("M"), Some(RecallId::Strip(StripId::Master)));
    assert_eq!(e.recall_id("nope"), None);
    assert_eq!(e.recall_ids().len(), 5);
    // off by default (a test engine, a host that does not ask)
    let (plain, _) = engine(EngineConfig::new(SR));
    assert_eq!(plain.recall_id("A"), None);
    assert!(plain.strip_recalls.is_empty());
}

#[test]
fn a_frozen_deck_rings_on_after_it_stops() {
    let mut g = Rig::new();
    g.deck_a();
    let playing = g.level();
    assert!(playing > 0.01, "{playing}");
    let a = g.e.recall_id("A").unwrap();
    assert!(g.e.input_freeze(a, true, Some(2.0), Some(false)));
    g.run(40);
    assert_eq!(g.e.recall_view(a).unwrap().freeze_state(), 2);
    assert!(g.e.recall_view(a).unwrap().freeze_beats() > 1.9);
    g.e.pause("A");
    g.run(400);
    let frozen = g.level();
    assert!(frozen > playing * 0.2, "the cloud carries on: {frozen} vs {playing}");
    // thawed: the stopped deck again
    g.e.input_freeze(a, false, None, Some(false));
    g.run(400);
    assert!(g.level() < frozen * 0.05, "{}", g.level());
}

#[test]
fn the_master_freezes_and_a_quantised_freeze_waits_for_the_beat() {
    let mut g = Rig::new();
    g.deck_a();
    let m = g.e.recall_id("M").unwrap();
    // on the beat (the deck's clock is the master clock)
    assert!(g.e.input_freeze(m, true, Some(1.0), None));
    assert!(g.e.is_frozen(m));
    g.run(1);
    let st = g.e.recall_view(m).unwrap().freeze_state();
    assert!(st == 1 || st == 2, "{st}");
    g.run(120);
    assert_eq!(g.e.recall_view(m).unwrap().freeze_state(), 2);
    g.e.pause("A");
    g.run(400);
    assert!(g.level() > 0.005, "the master's cloud {}", g.level());
}

#[test]
fn v_and_l_freeze_too() {
    // V: the synth, frozen, rings on after its notes are let go
    let mut g = Rig::new();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_note(57, true);
    g.run(600);
    let v = g.e.recall_id("V").unwrap();
    g.e.input_freeze(v, true, Some(1.0), Some(false));
    g.run(20);
    g.e.vocoder_note(57, false);
    g.run(400);
    assert!(g.level() > 0.02, "V's cloud {}", g.level());
    // L: a loop on the row, frozen, rings on after the row stops
    let mut h = Rig::new();
    let tone: Vec<f32> = (0..96_000)
        .map(|i| 0.4 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / SR).sin() as f32)
        .collect();
    let id = h.e.add_audio_clip("tone", tone.clone(), tone, 1, false);
    h.e.start_loop(&id, true);
    h.run(600);
    assert!(h.level() > 0.05);
    let l = h.e.recall_id("L").unwrap();
    h.e.input_freeze(l, true, Some(1.0), Some(false));
    h.run(20);
    h.e.stop_all_loops();
    h.run(800);
    assert!(h.level() > 0.02, "L's cloud {}", h.level());
}

/// A deck's jog aimed at another strip's history (here B's jog at the master): it scrubs that
/// history, moves the freeze position while frozen, and the snap (a `historySnap` button) puts
/// it back to live.
#[test]
fn a_jog_scrubs_any_history_and_snaps_back_to_live() {
    let mut g = Rig::new();
    g.deck_a();
    let m = g.e.recall_id("M").unwrap();
    g.e.set_history_jog(Some(("B", m)));
    assert_eq!(g.e.history_aimed("B"), Some(m));
    // the wheel: touched and turned back two beats
    assert!(g.e.input_touch(m));
    assert!(g.e.input_scrub_beats(m, -2.0)); // negative: back in time
    g.run(20);
    let v = g.e.recall_view(m).unwrap();
    assert_ne!(v.history_mode(), Mode::Live);
    assert!(v.history_lag_beats() > 1.0, "{}", v.history_lag_beats());
    // snap: back to now, from a button bound to historySnap
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "APC".into();
    SurfaceHost::perform(&mut g.e, target("historySnap", 1.0, true, &[("deck", "B")]));
    g.run(20);
    assert_eq!(g.e.recall_view(m).unwrap().history_mode(), Mode::Live);
    let t = g.e.trace.take().unwrap();
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0].entry.text, "APC: history M snapped to live (B's jog)");
    // another deck's snap does nothing
    assert!(!g.e.history_snap(Some("A")));
    // frozen, the wheel moves the freeze position instead
    g.e.input_freeze(m, true, Some(2.0), Some(false));
    g.run(20);
    let before = g.e.freeze_settings(m).unwrap().position;
    g.e.input_touch(m);
    g.e.input_scrub_beats(m, 0.5);
    g.run(4);
    let after = g.e.freeze_settings(m).unwrap().position;
    assert!((after - before).abs() > 0.05, "{before} -> {after}");
    assert!((g.e.recall_view(m).unwrap().freeze_position() as f64 - after).abs() < 1e-3);
}

#[test]
fn a_strip_loops_its_own_history() {
    let mut g = Rig::new();
    g.deck_a();
    let a = g.e.recall_id("A").unwrap();
    assert!(g.e.input_loop(a, 1.0));
    g.run(10);
    assert_eq!(g.e.recall_view(a).unwrap().history_mode(), Mode::Loop);
    assert!(g.e.input_loop_step(a, 1));
    assert_eq!(g.e.recall_loop_beats(a), 2.0);
    assert!(g.e.input_live(a));
    g.run(10);
    assert_eq!(g.e.recall_view(a).unwrap().history_mode(), Mode::Live);
}

/// The freeze and freezeKnob bindings name any strip; their moves are logged in the strip's
/// name.
#[test]
fn freeze_bindings_take_any_strip_and_log_it() {
    let mut g = Rig::new();
    g.deck_a();
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "MPK".into();
    SurfaceHost::perform(&mut g.e, target("freezeKnob", 0.8, false, &[("input", "M"), ("param", "size")]));
    SurfaceHost::perform(&mut g.e, target("freeze", 1.0, true, &[("input", "A"), ("quant", "now")]));
    let a = g.e.recall_id("A").unwrap();
    assert!(g.e.is_frozen(a));
    let m = g.e.recall_id("M").unwrap();
    let size = g.e.freeze_settings(m).unwrap().size_ms;
    assert!((size - FreezeParam::Size.from_knob(0.8)).abs() < 1e-9);
    let t: Vec<String> = g.e.trace.take().unwrap().into_iter().map(|t| t.entry.text).collect();
    assert_eq!(t.len(), 2, "{t:?}");
    assert!(t[0].starts_with("M grain size "), "{t:?}");
    assert!(t[1].starts_with("A freeze "), "{t:?}");
    // the JSON names the strip
    let j = g.e.freeze_json(a);
    assert_eq!(j.get("strip").and_then(|s| s.as_str()), Some("A"));
}

#[test]
fn the_render_thread_does_not_allocate_with_every_strip_frozen() {
    let mut g = Rig::new();
    g.deck_a();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_note(57, true);
    for name in ["A", "B", "L", "V", "M"] {
        let id = g.e.recall_id(name).unwrap();
        g.e.input_freeze(id, true, Some(1.0), Some(false));
    }
    let m = g.e.recall_id("M").unwrap();
    g.e.set_history_jog(Some(("B", m)));
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    g.r.render_block(&mut l, &mut r);
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..50 {
        g.r.render_block(&mut l, &mut r);
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0);
}
