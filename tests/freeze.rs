//! Granular freeze on an input strip, offline (docs/INPUT-HISTORY.md, "Freeze"): the last beats
//! of the history held as a grain cloud after the input stops, freezing on the beat, the jog
//! moving the position, the bindings, the trace, and no allocation on the render thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::fx::granular::FreezeParam;
use djrust::TrackSpec;
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
    feed: rtrb::Producer<f32>,
    t: usize,
    playing: bool,
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
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        Rig {
            e,
            r,
            feed,
            t: 0,
            playing: true,
            out: Vec::new(),
        }
    }

    fn run(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.out.clear();
        for _ in 0..blocks {
            for k in 0..BLOCK {
                let t = (self.t + k) as f64 / SR;
                let x = if self.playing {
                    0.5 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()
                } else {
                    0.0
                };
                let _ = self.feed.push(x as f32);
            }
            self.t += BLOCK;
            self.r.render_block(&mut l, &mut rr);
            self.e.service_jogs();
            self.e.service_loops();
            self.out.extend_from_slice(&l);
        }
    }

    fn level(&self) -> f32 {
        let x = &self.out[self.out.len() / 2..];
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }
}

fn get<'a>(j: &'a djrust::JsonValue, k: &str) -> &'a djrust::JsonValue {
    j.get(k)
        .unwrap_or_else(|| panic!("no {k} in {}", j.js_stringified()))
}

#[test]
fn a_freeze_holds_the_input_after_it_stops() {
    let mut g = Rig::new();
    g.run(400); // two seconds of tone in the history
    assert!(g.e.input_freeze(0, true, Some(1.0), Some(false)));
    g.run(4);
    assert_eq!(state(&g), "frozen");
    g.playing = false;
    g.run(400);
    let frozen = g.level();
    assert!(frozen > 0.05, "the cloud carries on: {frozen}");
    let j = g.e.freeze_json(0);
    assert!((get(&j, "held_beats").as_f64().unwrap() - 1.0).abs() < 0.01);
    assert!(get(&j, "grains").as_f64().unwrap() > 0.0);
    assert!(g.e.input_freeze(0, false, None, Some(false)));
    g.run(200);
    assert_eq!(state(&g), "off");
    assert!(g.level() < 1e-4, "thawed: the silent input again");
}

fn state(g: &Rig) -> String {
    get(&g.e.freeze_json(0), "state")
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_quantised_freeze_waits_for_the_beat() {
    let mut g = Rig::new();
    g.e.set_volume("A", 0.0);
    assert!(g.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    g.e.play("A");
    g.run(400);
    // just after a beat, so the next is most of a beat away
    let (beat, fpb) = loop {
        g.run(1);
        let (_, bpm) = g.e.master_tempo();
        let fpb = 60.0 * SR / bpm;
        let b = g.e.deck("A").unwrap().beat_state(SR).map(|s| s.beat);
        if let Some(b) = b {
            let frac = b - b.floor();
            if frac > 0.05 && frac < 0.3 {
                break (b, fpb);
            }
        }
    };
    assert!(beat > 0.0);
    assert!(g.e.input_freeze(0, true, Some(1.0), None)); // quantise is on by default
    g.run(1);
    assert_eq!(state(&g), "freezing", "waits for the beat");
    g.run((fpb as usize) / BLOCK + 2);
    assert_eq!(state(&g), "frozen");
    // thawing waits too
    g.e.input_freeze(0, false, None, None);
    g.run(1);
    assert_eq!(state(&g), "thawing");
    g.run((fpb as usize) / BLOCK + 2);
    assert_eq!(state(&g), "off");
}

#[test]
fn the_jog_moves_the_freeze_position_while_frozen() {
    let mut g = Rig::new();
    g.run(400);
    g.e.set_history_jog(Some(("A", djrust::host::deck::strip_recall::RecallId::Input(0))));
    g.e.input_freeze(0, true, Some(1.0), Some(false));
    g.e.freeze_set(0, FreezeParam::Position, 0.5);
    g.run(2);
    // a quarter of the frozen beat forward (a beat at 120 is 0.5 s of platter)
    assert!(g.e.input_scrub(0, 0.125));
    g.run(2);
    let p = g.e.inputs[0].mirror.freeze_position();
    assert!((p - 0.75).abs() < 0.01, "{p}");
    assert!((g.e.inputs[0].freeze.position - 0.75).abs() < 0.01);
    // the history head was not touched
    assert_eq!(
        g.e.inputs[0].mirror.history_mode(),
        djrust::history::Mode::Live
    );
    // thawed, the wheel scrubs the history again
    g.e.input_freeze(0, false, None, Some(false));
    g.e.input_scrub(0, -0.25);
    g.run(2);
    assert_ne!(
        g.e.inputs[0].mirror.history_mode(),
        djrust::history::Mode::Live
    );
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
fn bindings_drive_it_and_the_trace_says_so() {
    let mut g = Rig::new();
    g.run(400);
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "APC".into();
    let now = [("op", "hold"), ("quant", "now"), ("input", "TD-3")];
    SurfaceHost::perform(&mut g.e, target("freeze", 1.0, true, &now));
    g.run(2);
    assert_eq!(state(&g), "frozen");
    SurfaceHost::perform(&mut g.e, target("freeze", 0.0, false, &now));
    g.run(2);
    assert_ne!(state(&g), "frozen", "hold lets go with the pad");
    // latched toggle
    let t = [("op", "toggle"), ("quant", "now")];
    SurfaceHost::perform(&mut g.e, target("freeze", 1.0, true, &t));
    SurfaceHost::perform(&mut g.e, target("freeze", 0.0, false, &t));
    g.run(2);
    assert_eq!(state(&g), "frozen", "toggle stays after the release");
    // knobs: grain size across its log range, then density
    SurfaceHost::perform(
        &mut g.e,
        target("freezeKnob", 0.0, false, &[("param", "size")]),
    );
    SurfaceHost::perform(
        &mut g.e,
        target("freezeKnob", 1.0, false, &[("param", "size")]),
    );
    assert_eq!(g.e.inputs[0].freeze.size_ms, 500.0);
    SurfaceHost::perform(
        &mut g.e,
        target("freezeKnob", 1.0, false, &[("param", "jitter")]),
    );
    assert_eq!(g.e.inputs[0].freeze.jitter, 12.0);
    SurfaceHost::perform(&mut g.e, target("freeze", 1.0, true, &[("op", "quantize")]));
    assert!(!g.e.inputs[0].freeze.quantize);
    SurfaceHost::perform(
        &mut g.e,
        target("freezeKnob", 1.0, false, &[("param", "nope")]),
    );
    assert!(g.e.unhandled.last().unwrap().contains("freezeKnob"));

    let tr = g.e.trace.take().unwrap();
    let texts: Vec<&str> = tr.iter().map(|x| x.entry.text.as_str()).collect();
    // each move is also the `freeze` live op that makes it again, so an arrangement keeps it
    assert!(tr.iter().all(|x| x.entry.source == "APC"
        && x.ops.len() == 1
        && x.ops[0].op() == "freeze"
        && x.ops[0].json().get("input").and_then(djrust::json::JsonValue::as_str)
            == Some("TD-3")));
    assert!(
        tr.iter().all(|x| x.entry.key.starts_with("freeze:TD-3:")),
        "{texts:?}"
    );
    assert!(texts.contains(&"TD-3 freeze 1 beat"), "{texts:?}");
    assert!(texts.contains(&"TD-3 thaw"), "{texts:?}");
    assert!(texts.contains(&"TD-3 grain size 500 ms"), "{texts:?}");
    assert!(texts.contains(&"TD-3 freeze quantise off"), "{texts:?}");
    assert!(tr
        .iter()
        .filter(|x| x.entry.key.ends_with(":size"))
        .all(|x| x.entry.fold));
}

#[test]
fn no_history_no_freeze() {
    let mut c = EngineConfig::new(SR);
    c.inputs = vec!["MIC".into()];
    c.history_beats = None;
    let (mut e, _r) = engine(c);
    assert!(!e.input_freeze(0, true, None, Some(false)));
    assert!(e.freeze_set(0, FreezeParam::Size, 50.0).is_none());
    assert!(!e.is_frozen(0));
}

#[test]
fn the_render_thread_does_not_allocate() {
    let mut g = Rig::new();
    g.run(400);
    g.e.freeze_set(0, FreezeParam::Density, 200.0);
    g.e.freeze_set(0, FreezeParam::Jitter, 7.0);
    g.e.input_freeze(0, true, Some(4.0), Some(false));
    g.run(2);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..100 {
        g.r.render_block(&mut l, &mut r);
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0);
}
