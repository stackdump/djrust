//! Performance tricks on the decks, offline (docs/TRICKS.md): each trick's sound, its timing on
//! the deck's grid, the `trick` binding (momentary and latched), the trace, and no allocation on
//! the render thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::fx::tricks::{TrickArgs, TrickKind};
use djrust::TrackSpec;
use djrust::host::deck::tricks::TrickQuant;
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
    /// Deck A playing a techno track at full volume, crossfader on A.
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (mut e, r) = engine(c);
        e.set_volume("A", 1.0);
        e.set_crossfader(0.0);
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        e.play("A");
        let mut g = Rig {
            e,
            r,
            out: Vec::new(),
        };
        g.run(200);
        g
    }

    fn run(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.out.clear();
        for _ in 0..blocks {
            self.r.render_block(&mut l, &mut rr);
            self.e.service();
            self.e.service_jogs();
            self.out.extend_from_slice(&l);
        }
    }

    fn fpb(&self) -> f64 {
        let bpm = self.e.deck("A").unwrap().beat_state(SR).unwrap().bpm;
        60.0 * SR / bpm
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// How alike `x` is to itself `lag` frames later (1: the same).
fn self_similarity(x: &[f32], lag: usize) -> f32 {
    let n = x.len() - lag;
    let (mut d, mut e) = (0f32, 0f32);
    for i in 0..n {
        d += (x[i] - x[i + lag]).powi(2);
        e += x[i].powi(2);
    }
    1.0 - d / e.max(1e-9)
}

#[test]
fn a_stutter_repeats_a_sixteenth_on_the_grid() {
    let mut g = Rig::new();
    let fpb = g.fpb();
    let len = (fpb / 4.0).round() as usize;
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "APC".into();
    g.e.trick(
        "A",
        TrickKind::Stutter,
        true,
        Some(TrickArgs {
            div: 16.0,
            ..TrickArgs::default()
        }),
        TrickQuant::Default,
    )
    .unwrap();
    // let the fire land and the slice record, then listen to the repeats
    g.run((2.0 * fpb) as usize / BLOCK);
    g.run((2.0 * fpb) as usize / BLOCK);
    let s = self_similarity(&g.out, len);
    assert!(s > 0.98, "repeats every 1/16: {s}");
    assert!(g.e.deck("A").unwrap().mirror.tricks_sounding() & 1 != 0);
    // a plain deck does not repeat like that
    let mut plain = Rig::new();
    plain.run((4.0 * fpb) as usize / BLOCK);
    assert!(self_similarity(&plain.out, len) < 0.9);
    // off, and the deck is itself again: the same audio as a deck that never stuttered
    g.e.trick("A", TrickKind::Stutter, false, None, TrickQuant::Now)
        .unwrap();
    g.run(40);
    let tr = g.e.trace.take().unwrap();
    let texts: Vec<&str> = tr.iter().map(|t| t.entry.text.as_str()).collect();
    assert_eq!(texts, vec!["A stutter 1/16 on the 1/16", "A stutter off"]);
    assert!(tr.iter().all(|t| t.entry.key == "trick:A:stutter"));
    // each fire is also the `trick` live op that makes it again (an arrangement keeps it): the
    // args as fired, the grid in beats
    let ops: Vec<String> = tr
        .iter()
        .flat_map(|t| t.ops.iter().map(|o| o.json().js_stringified()))
        .collect();
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert!(ops[0].contains(r#""action":"on""#) && ops[0].contains(r#""div":16"#) && ops[0].contains(r#""quant":0.25"#), "{}", ops[0]);
    assert!(ops[1].contains(r#""action":"off""#) && ops[1].contains(r#""quant":"now""#), "{}", ops[1]);
}

#[test]
fn the_fire_lands_on_the_decks_grid() {
    // two identical decks; one fires mid-sixteenth. Its output matches the plain deck up to the
    // sixteenth line, then repeats from there
    let mut a = Rig::new();
    let mut b = Rig::new();
    let fpb = a.fpb();
    a.e.trick("A", TrickKind::Stutter, true, None, TrickQuant::Beats(0.25))
        .unwrap();
    a.run((fpb as usize) / BLOCK + 1);
    b.run((fpb as usize) / BLOCK + 1);
    // the first difference: where the first repeat starts (a sixteenth after the grid line)
    let first = a
        .out
        .iter()
        .zip(&b.out)
        .position(|(x, y)| (x - y).abs() > 1e-6)
        .expect("it stutters");
    // the repeat starts one sixteenth after a sixteenth line: a multiple of a sixteenth from
    // the deck's beat origin. Measure against the plain deck's beat clock instead: the gap from
    // the run's start to the first difference, mod a sixteenth, equals the run's start phase
    let six = fpb / 4.0;
    let run_start_beat =
        b.e.deck("A").unwrap().beat_state(SR).unwrap().beat - (b.out.len() as f64) / fpb;
    let line = ((run_start_beat * 4.0).ceil() / 4.0 - run_start_beat) * fpb;
    let phase = ((first as f64 - line) / six).fract();
    assert!(
        phase < 0.02 || phase > 0.98,
        "first repeat at {first}, grid line at {line:.0}: phase {phase}"
    );
}

#[test]
fn a_stopped_deck_fires_at_once() {
    let mut g = Rig::new();
    g.e.pause("A");
    g.run(10);
    g.e.trace = Some(Vec::new());
    g.e.trick("A", TrickKind::Stutter, true, None, TrickQuant::Default)
        .unwrap();
    let t = g.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "A stutter 1/16");
    assert!(g
        .e
        .trick("Z", TrickKind::Stutter, true, None, TrickQuant::Now)
        .is_err());
}

fn press(deck: &str, pressed: bool, args: &[(&str, &str)]) -> Action {
    Action::Target {
        id: "trick".into(),
        deck: Some(deck.into()),
        slot: None,
        index: 0,
        value: if pressed { 1.0 } else { 0.0 },
        pressed,
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn pads_fire_tricks_momentary_or_latched() {
    let mut g = Rig::new();
    let m = [
        ("kind", "stutter"),
        ("div", "8"),
        ("rise", "1"),
        ("quant", "now"),
    ];
    SurfaceHost::perform(&mut g.e, press("A", true, &m));
    assert!(g.e.trick_on("A", TrickKind::Stutter));
    let a = g.e.deck("A").unwrap().tricks.on[&TrickKind::Stutter];
    assert_eq!((a.div, a.rise), (8.0, 1.0));
    SurfaceHost::perform(&mut g.e, press("A", false, &m));
    assert!(
        !g.e.trick_on("A", TrickKind::Stutter),
        "momentary: off on release"
    );
    let l = [("kind", "stutter"), ("mode", "latched"), ("quant", "now")];
    SurfaceHost::perform(&mut g.e, press("A", true, &l));
    SurfaceHost::perform(&mut g.e, press("A", false, &l));
    assert!(g.e.trick_on("A", TrickKind::Stutter), "latched: stays");
    SurfaceHost::perform(&mut g.e, press("A", true, &l));
    assert!(!g.e.trick_on("A", TrickKind::Stutter));
    SurfaceHost::perform(&mut g.e, press("A", true, &[("kind", "nope")]));
    assert!(g.e.unhandled.last().unwrap().contains("trick"));
    let j = g.e.tricks_json();
    assert!(j.get("A").and_then(|a| a.get("stutter")).is_some());
}

#[test]
fn the_render_thread_does_not_allocate() {
    let mut g = Rig::new();
    let fpb = g.fpb();
    // the gear's tricks are MIDI, never on the render thread
    for k in TrickKind::ALL.into_iter().filter(|k| !k.is_gear()) {
        g.e.trick("A", k, true, None, TrickQuant::Beats(0.25))
            .unwrap();
    }
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    // the fires are waiting on the render thread: count across their landing
    g.r.render_block(&mut l, &mut r);
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..(2.0 * fpb) as usize / BLOCK {
        g.r.render_block(&mut l, &mut r);
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0);
    assert!(rms(&l) >= 0.0);
}

/// Deck A at full volume on the master; deck B playing too but faded out (its kick is still
/// there pre-fader, for a pump to hear).
fn two_decks(b_plays: bool) -> Rig {
    let mut g = Rig::new();
    assert!(g.e.generate("B", &TrackSpec::new("techno", 7.0, None)));
    g.e.set_volume("B", 0.0);
    if b_plays {
        g.e.play("B");
    }
    g.run(100);
    g
}

#[test]
fn a_grid_pump_ducks_after_each_beat() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    let grid = djrust::fx::tricks::TrickArgs {
        source: djrust::fx::tricks::TrickSource::Grid,
        amount: 1.0,
        div: 8.0,
        ..TrickArgs::default()
    };
    a.e.trick("A", TrickKind::Pump, true, Some(grid), TrickQuant::Default)
        .unwrap();
    let beats = 4.0;
    a.run((beats * fpb) as usize / BLOCK);
    plain.run((beats * fpb) as usize / BLOCK);
    // the beat lines in this run, from the plain deck's clock
    let end_beat = plain.e.deck("A").unwrap().beat_state(SR).unwrap().beat;
    let start_beat = end_beat - plain.out.len() as f64 / fpb;
    let mut ducked = 0;
    let mut lines = 0;
    let mut b = start_beat.ceil() + 1.0; // the first beat may be before the pump landed
    while b < end_beat - 0.5 {
        let at = ((b - start_beat) * fpb) as usize;
        let w = (fpb / 32.0) as usize;
        let after = rms(&a.out[at + 200..at + w]) / rms(&plain.out[at + 200..at + w]).max(1e-6);
        let late = (fpb * 0.75) as usize;
        let before = rms(&a.out[at + late..at + late + w])
            / rms(&plain.out[at + late..at + late + w]).max(1e-6);
        lines += 1;
        if after < 0.3 && before > 0.95 {
            ducked += 1;
        }
        b += 1.0;
    }
    assert!(
        lines >= 2 && ducked == lines,
        "{ducked} of {lines} beats ducked"
    );
}

#[test]
fn a_pump_from_the_other_decks_kick() {
    let mut a = two_decks(true);
    let mut plain = two_decks(true);
    a.e.trace = Some(Vec::new());
    a.e.trick("A", TrickKind::Pump, true, None, TrickQuant::Now)
        .unwrap();
    let t = a.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "A pump 80% from B's kick, release 1/8");
    let fpb = a.fpb();
    a.run((4.0 * fpb) as usize / BLOCK);
    plain.run((4.0 * fpb) as usize / BLOCK);
    // ducked just after B's beats (its kick), not late in them
    let bfpb = 60.0 * SR / plain.e.deck("B").unwrap().beat_state(SR).unwrap().bpm;
    let end_beat = plain.e.deck("B").unwrap().beat_state(SR).unwrap().beat;
    let start_beat = end_beat - plain.out.len() as f64 / bfpb;
    let (mut ducked, mut lines) = (0, 0);
    let mut ratios = vec![];
    let mut b = start_beat.ceil() + 1.0;
    while b < end_beat - 0.5 {
        let at = ((b - start_beat) * bfpb) as usize;
        let w = (bfpb / 16.0) as usize;
        let r =
            rms(&a.out[at + 300..at + 300 + w]) / rms(&plain.out[at + 300..at + 300 + w]).max(1e-6);
        ratios.push(r);
        lines += 1;
        if r < 0.7 {
            ducked += 1;
        }
        b += 1.0;
    }
    assert!(
        lines >= 2 && ducked == lines,
        "{ducked} of {lines}: {ratios:?}"
    );
    let (x, y) = (rms(&a.out), rms(&plain.out));
    assert!(x < y, "pumped {x} vs {y}");
    assert!(a.e.deck("A").unwrap().mirror.tricks_sounding() & 2 != 0);

    // B silent (stopped): its kick is gone, so A plays untouched
    let mut quiet = two_decks(false);
    let mut plain = two_decks(false);
    let b_only = djrust::fx::tricks::TrickArgs {
        source: djrust::fx::tricks::TrickSource::Deck(1),
        ..TrickKind::Pump.default_args()
    };
    quiet
        .e
        .trick("A", TrickKind::Pump, true, Some(b_only), TrickQuant::Now)
        .unwrap();
    quiet.run(200);
    plain.run(200);
    assert_eq!(quiet.out, plain.out);
    // with B stopped, auto falls back to the grid
    let mut auto = two_decks(false);
    auto.e
        .trick("A", TrickKind::Pump, true, None, TrickQuant::Now)
        .unwrap();
    assert_eq!(
        auto.e.deck("A").unwrap().tricks.on[&TrickKind::Pump].source,
        djrust::fx::tricks::TrickSource::Grid
    );
}

#[test]
fn a_pump_pad_takes_a_source() {
    let mut g = two_decks(true);
    let m = [
        ("kind", "pump"),
        ("source", "b"),
        ("amount", "0.5"),
        ("div", "16"),
        ("mode", "latched"),
    ];
    SurfaceHost::perform(&mut g.e, press("A", true, &m));
    let a = g.e.deck("A").unwrap().tricks.on[&TrickKind::Pump];
    assert_eq!(
        (a.source, a.amount, a.div),
        (djrust::fx::tricks::TrickSource::Deck(1), 0.5, 16.0)
    );
}

#[test]
fn a_delay_throw_rings_on_after_release() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    a.e.trace = Some(Vec::new());
    a.e.trick("A", TrickKind::Delay, true, None, TrickQuant::Default)
        .unwrap();
    a.run((fpb / 2.0) as usize / BLOCK);
    plain.run((fpb / 2.0) as usize / BLOCK);
    a.e.trick("A", TrickKind::Delay, false, None, TrickQuant::Default)
        .unwrap();
    // the deck goes quiet (paused on both), and only the echoes are left on the throw's side
    a.e.pause("A");
    plain.e.pause("A");
    a.run((2.0 * fpb) as usize / BLOCK);
    plain.run((2.0 * fpb) as usize / BLOCK);
    // what the throw left behind: the difference from the deck that never threw
    let diff: Vec<f32> = a.out.iter().zip(&plain.out).map(|(x, y)| x - y).collect();
    let (echo, dry) = (rms(&diff), rms(&plain.out));
    assert!(
        echo > 0.5 * dry && echo > 0.01,
        "rings after release: {echo} vs {dry}"
    );
    assert!(a.e.deck("A").unwrap().mirror.tricks_sounding() & 4 != 0);
    let t = a.e.trace.take().unwrap();
    assert_eq!(
        t[0].entry.text,
        "A dub delay 0.75 beats feedback 60% send 80%"
    );
    assert_eq!(t[1].entry.text, "A delay off");
    // it does die away
    a.run((60.0 * fpb) as usize / BLOCK);
    assert!(rms(&a.out[a.out.len() - 4_800..]) < 1e-4);
}

#[test]
fn a_wash_takes_the_deck_into_the_room() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    a.e.trace = Some(Vec::new());
    a.e.trick("A", TrickKind::Wash, true, None, TrickQuant::Default)
        .unwrap();
    a.run((2.0 * fpb) as usize / BLOCK);
    plain.run((2.0 * fpb) as usize / BLOCK);
    // the last beat: no longer the deck itself, but sounding
    let half = a.out.len() / 2;
    let (x, y) = (&a.out[half..], &plain.out[half..]);
    let corr = x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>()
        / (rms(x) * rms(y) * x.len() as f32).max(1e-9);
    assert!(
        rms(x) > 0.01 && corr < 0.5,
        "washed: rms {} corr {corr}",
        rms(x)
    );
    assert!(a.e.deck("A").unwrap().mirror.tricks_sounding() & (1 << 3) != 0);
    a.e.trick("A", TrickKind::Wash, false, None, TrickQuant::Default)
        .unwrap();
    let t = a.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "A reverb wash-out over 1 beat");
}

#[test]
fn a_verb_freeze_holds_after_the_deck_stops() {
    let mut a = Rig::new();
    let fpb = a.fpb();
    a.e.trick("A", TrickKind::VerbFreeze, true, None, TrickQuant::Default)
        .unwrap();
    a.run((fpb / 2.0) as usize / BLOCK + 20);
    a.e.stop("A");
    a.run(2 * 48_000 / BLOCK);
    let held = rms(&a.out[a.out.len() - 9_600..]);
    assert!(held > 0.005, "the frozen room after 2 s: {held}");
    a.e.trick("A", TrickKind::VerbFreeze, false, None, TrickQuant::Default)
        .unwrap();
    a.run(30 * 48_000 / BLOCK);
    assert!(
        rms(&a.out[a.out.len() - 9_600..]) < 0.05 * held,
        "decays once released"
    );
}

#[test]
fn a_tape_stop_halts_the_deck_until_released() {
    let mut a = Rig::new();
    let fpb = a.fpb();
    a.e.trick("A", TrickKind::TapeStop, true, None, TrickQuant::Default)
        .unwrap();
    a.run((1.5 * fpb) as usize / BLOCK);
    let tail = &a.out[a.out.len() - 4_800..];
    assert!(tail.iter().all(|x| x.abs() < 1e-6), "halted after a beat");
    assert!(a.e.deck("A").unwrap().mirror.tricks_sounding() & (1 << 5) != 0);
    a.e.trick("A", TrickKind::TapeStop, false, None, TrickQuant::Default)
        .unwrap();
    a.run(40);
    assert!(rms(&a.out[a.out.len() - 4_800..]) > 0.01, "the deck again");
}

#[test]
fn a_spin_back_reaches_into_what_just_played() {
    // the tape listens before any trick: a spin-back right away has the deck's past to play
    let mut a = Rig::new();
    a.e.trace = Some(Vec::new());
    a.e.trick("A", TrickKind::SpinBack, true, None, TrickQuant::Default)
        .unwrap();
    let fpb = a.fpb();
    a.run((0.25 * fpb) as usize / BLOCK);
    assert!(rms(&a.out) > 0.01, "the past, backwards: {}", rms(&a.out));
    a.run((fpb) as usize / BLOCK);
    assert!(a.out[a.out.len() - 2_000..].iter().all(|x| x.abs() < 1e-6));
    let t = a.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "A spin-back over 0.5 beats");
}

fn beat(g: &Rig) -> f64 {
    g.e.deck("A").unwrap().beat_state(SR).unwrap().beat
}

#[test]
fn half_and_double_time_run_the_clock_and_land_back_in_phase() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    let bpm = a.e.effective_bpm("A").unwrap();
    a.e.trace = Some(Vec::new());
    a.e.trick("A", TrickKind::HalfTime, true, None, TrickQuant::Now)
        .unwrap();
    let (b0, p0) = (beat(&a), beat(&plain));
    a.run((4.0 * fpb) as usize / BLOCK);
    plain.run((4.0 * fpb) as usize / BLOCK);
    let (da, dp) = (beat(&a) - b0, beat(&plain) - p0);
    assert!(
        (da / dp - 0.5).abs() < 0.05,
        "half time: {da} beats against {dp}"
    );
    assert_eq!(
        a.e.effective_bpm("A").unwrap(),
        bpm,
        "the deck's own tempo stays"
    );
    // straight into double time
    a.e.trick("A", TrickKind::DoubleTime, true, None, TrickQuant::Now)
        .unwrap();
    assert!(!a.e.trick_on("A", TrickKind::HalfTime));
    let (b1, p1) = (beat(&a), beat(&plain));
    a.run((2.0 * fpb) as usize / BLOCK);
    plain.run((2.0 * fpb) as usize / BLOCK);
    let (da, dp) = (beat(&a) - b1, beat(&plain) - p1);
    assert!(
        (da / dp - 2.0).abs() < 0.1,
        "double time: {da} beats against {dp}"
    );
    // back: in phase with the deck that never left normal time
    a.e.trick("A", TrickKind::DoubleTime, false, None, TrickQuant::Now)
        .unwrap();
    a.run(8);
    plain.run(8);
    let gap = beat(&a) - beat(&plain);
    assert!(gap.abs() < 0.05, "back in phase: {gap} beats off");
    let t = a.e.trace.take().unwrap();
    let texts: Vec<&str> = t.iter().map(|x| x.entry.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["A half time", "A double time", "A back to normal time"]
    );
}

#[test]
fn half_time_waits_for_the_beat() {
    let mut a = Rig::new();
    let fpb = a.fpb();
    a.e.trick("A", TrickKind::HalfTime, true, None, TrickQuant::Default)
        .unwrap();
    assert_eq!(a.e.deck("A").unwrap().tricks.time_scale(), 1.0, "not yet");
    a.run((1.1 * fpb) as usize / BLOCK + 2);
    assert_eq!(a.e.deck("A").unwrap().tricks.time_scale(), 0.5);
    let j = a.e.tricks_json();
    let on = j
        .get("A")
        .and_then(|x| x.get("half_time"))
        .and_then(|x| x.get("sounding"));
    assert_eq!(on.and_then(|v| v.as_bool()), Some(true));
}

/// The high end: RMS of the first difference (rises with frequency).
fn brightness(x: &[f32]) -> f32 {
    let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
    rms(&d)
}

#[test]
fn the_filter_knob_and_its_ride() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    a.e.trace = Some(Vec::new());
    a.e.trace_via = "APC".into();
    // a hand on the knob: well into the low-pass
    let knob = |v: f64| Action::Target {
        id: "trickKnob".into(),
        deck: Some("A".into()),
        slot: None,
        index: 0,
        value: v,
        pressed: false,
        args: [("kind".to_string(), "filter".to_string())]
            .into_iter()
            .collect(),
    };
    SurfaceHost::perform(&mut a.e, knob(0.2));
    SurfaceHost::perform(&mut a.e, knob(0.15));
    a.run(100);
    plain.run(100);
    let (x, y) = (brightness(&a.out[4_800..]), brightness(&plain.out[4_800..]));
    assert!(x < 0.5 * y, "low-passed: {x} vs {y}");
    let t = a.e.trace.take().unwrap();
    assert_eq!(t.len(), 2);
    assert!(t
        .iter()
        .all(|x| x.entry.fold && x.entry.key == "trick:A:filter:knob"));
    assert!(
        t[1].entry.text.starts_with("A filter low-pass"),
        "{}",
        t[1].entry.text
    );
    // back to centre: dry, bit for bit
    SurfaceHost::perform(&mut a.e, knob(0.5));
    a.run(20);
    plain.run(20);
    a.run(50);
    plain.run(50);
    assert_eq!(a.out, plain.out);

    // a ride up into the high-pass over a bar, from now
    let up = TrickArgs {
        amount: 0.7,
        bars: 1.0,
        ..TrickKind::Filter.default_args()
    };
    a.e.trick("A", TrickKind::Filter, true, Some(up), TrickQuant::Now)
        .unwrap();
    a.run((4.5 * fpb) as usize / BLOCK);
    plain.run((4.5 * fpb) as usize / BLOCK);
    let n = a.out.len();
    let (x, y) = (rms(&a.out[n - 9_600..]), rms(&plain.out[n - 9_600..]));
    assert!(x < 0.5 * y, "the high-pass takes the low end: {x} vs {y}");
    a.e.trick("A", TrickKind::Filter, false, None, TrickQuant::Now)
        .unwrap();
    a.run((2.0 * fpb) as usize / BLOCK);
    plain.run((2.0 * fpb) as usize / BLOCK);
    a.run(50);
    plain.run(50);
    assert_eq!(a.out, plain.out, "rode back open");
}

#[test]
fn td3_accent_and_slide_are_flags_the_hub_reads() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    let m = [("kind", "td3_accent")];
    SurfaceHost::perform(&mut g.e, press("A", true, &m));
    assert!(g.e.gear_tricks.accent && g.e.trick_on("A", TrickKind::Td3Accent));
    SurfaceHost::perform(&mut g.e, press("A", false, &m));
    assert!(!g.e.gear_tricks.accent, "momentary");
    g.e.trick("anything", TrickKind::Td3Slide, true, None, TrickQuant::Now)
        .unwrap();
    assert!(g.e.gear_tricks.slide, "the deck does not matter");
    let t = g.e.trace.take().unwrap();
    let texts: Vec<&str> = t.iter().map(|x| x.entry.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["Acid Bass accents", "Acid Bass accents off", "Acid Bass slides"]
    );
    assert!(t.iter().all(|x| x.entry.key.starts_with("trick:TD-3:")));
}

#[test]
fn a_cutoff_ride_needs_a_cutoff_cc() {
    let mut g = Rig::new();
    let e =
        g.e.trick("A", TrickKind::Td3Cutoff, true, None, TrickQuant::Now)
            .unwrap_err();
    assert!(e.contains("no cutoff CC"), "{e}");
    assert!(g.e.gear_tricks.cutoff.is_none());
    // gear that has one: a ride from the middle to open over two bars
    g.e.gear_tricks.caps.cutoff_cc = Some(74);
    let fpb = g.fpb();
    g.e.trick("A", TrickKind::Td3Cutoff, true, None, TrickQuant::Now)
        .unwrap();
    let mut sent = vec![];
    for _ in 0..((8.5 * fpb) as usize / BLOCK) {
        g.run(1);
        if let Some(m) = g.e.gear_tricks.cutoff_due(g.e.loop_frames(), 1) {
            sent.push(m);
        }
    }
    assert!(sent.iter().all(|m| m[0] == 0xB0 && m[1] == 74));
    assert_eq!(sent.first().map(|m| m[2]), Some(64));
    assert_eq!(sent.last().map(|m| m[2]), Some(127));
    assert!(
        sent.windows(2).all(|w| w[1][2] > w[0][2]),
        "only changes, rising"
    );
    // off: back to the middle over a beat
    g.e.trick("A", TrickKind::Td3Cutoff, false, None, TrickQuant::Now)
        .unwrap();
    g.run((1.2 * fpb) as usize / BLOCK);
    assert_eq!(
        g.e.gear_tricks.cutoff_due(g.e.loop_frames(), 1),
        Some([0xB0, 74, 64])
    );
}

#[test]
fn a_riser_runs_its_bars_from_the_bar_and_cuts() {
    let mut a = Rig::new();
    let mut plain = Rig::new();
    let fpb = a.fpb();
    a.e.trace = Some(Vec::new());
    let one = TrickArgs {
        bars: 1.0,
        amount: 0.8,
        ..TrickKind::Riser.default_args()
    };
    a.e.trick("A", TrickKind::Riser, true, Some(one), TrickQuant::Default)
        .unwrap();
    // up to a bar to the bar line, then a bar of rising
    let mut added = vec![];
    for _ in 0..((9.0 * fpb) as usize / BLOCK) {
        a.run(1);
        plain.run(1);
        added.extend(a.out.iter().zip(&plain.out).map(|(x, y)| x - y));
    }
    let start = added.iter().position(|x| x.abs() > 1e-6).expect("it rises");
    let end = added.iter().rposition(|x| x.abs() > 1e-6).unwrap();
    let len = (end - start) as f64 / fpb;
    assert!((len - 4.0).abs() < 0.1, "a bar long: {len} beats");
    // louder toward the end
    let q = (end - start) / 4;
    assert!(rms(&added[start + 3 * q..end]) > 3.0 * rms(&added[start..start + q]));
    assert!(!a.e.trick_on("A", TrickKind::Riser), "over once it has run");
    let t = a.e.trace.take().unwrap();
    assert_eq!(
        t[0].entry.text,
        "A noise riser over 1 bar to 80% on the bar"
    );
}
