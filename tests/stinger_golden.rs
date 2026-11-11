//! Swift goldens for a curated Hit that launches a loop (`tools/paritydump/stinger.swift`,
//! section "stinger"): the Swift offline `DJEngine` fires Hits assigned a library clip, a clip
//! that is not on the row and a stock loop that is not there, on deck A and on a second net deck
//! playing at another tempo; the Rust engine does the same operations and is compared step by
//! step: the loop row's voices (state, switch frame, the frame it goes out at, its reference deck
//! and tempo), the stops waiting on a voice, the loops looping, which decks play and the one the
//! loops follow, and which Hit nets of each deck are unmuted.
//!
//! Parity form (b) of the port rules. Frames are integers and compared within the 2 frames the
//! loop clock's origin is known to sit off Swift's (3 on deck B; see `loopbank_golden.rs`: Swift
//! lines a loop up with the deck's newest tick anchor, the Rust engine with the deck's published
//! beat state); everything else exactly: ids, states, tempi by their bits, nets, names.

mod common_row;

use common_row::*;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::StingerAssignment;
use djrust::sound::stinger::{AssignedLoop, AssignmentKind};
use djrust::TrackSpec;
use djrust::host::deck::loops::LoopClip;
use std::sync::Arc;

fn golden() -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/swift/stinger.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    JsonValue::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn bits(x: f64) -> JsonValue {
    JsonValue::str(format!("{:016x}", x.to_bits()))
}

fn int(x: i64) -> JsonValue {
    JsonValue::int(x)
}

fn obj<const N: usize>(p: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(p))
}

fn state(s: djrust::deck::loops::LoopState) -> &'static str {
    use djrust::deck::loops::LoopState::*;
    match s {
        Off => "off",
        Starting => "starting",
        On => "on",
        Stopping => "stopping",
    }
}

/// `stClip`: a clip of `bars` bars at 120 bpm whose samples say which clip it is (`test_clip` is
/// one bar).
fn clip_bars(n: usize, bars: usize) -> LoopClip {
    let l: Vec<f32> = (0..bars * 96_000).map(|i| n as f32 * 0.01 + (i % 100) as f32 * 1e-4).collect();
    let b = SampleBuffer::new(l.clone(), Some(l.iter().map(|v| -v).collect()), SR);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(&b.content_id(), &format!("A loop {n}"), "A", bars as i64, 120.0, 0.0, Arc::new(b), w, None)
}

/// `stLoopHit`: a Hit that fires a loop, a library clip's id or a stock loop's file.
fn loop_hit(slot: usize, clip: Option<&str>, path: Option<&str>) -> StingerAssignment {
    StingerAssignment {
        slot,
        kind: AssignmentKind::Loop,
        candidate: Some("test-loop".into()),
        label: "Test loop".into(),
        option: None,
        macro_id: None,
        bars: None,
        loop_: Some(AssignedLoop {
            path: path.map(String::from),
            clip_id: clip.map(String::from),
            name: "Test loop".into(),
            beats: 4,
            bpm: 120.0,
        }),
        local_only: path.is_some(),
    }
}

/// The Hit nets' indices in a deck's compiled graph, as the golden recorded them.
fn nets_of(want: &JsonValue, key: &str) -> Vec<i64> {
    want.get(key).unwrap().as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()
}

/// Which of Hit 1..4 of deck `name` ring (`stSnapshot`'s `unmuted`).
fn unmuted(rig: &Rig, name: &str, nets: &[i64]) -> String {
    let m = &rig.e.deck(name).unwrap().mirror;
    nets.iter().map(|&i| if i >= 0 && !m.is_net_muted(i as usize) { '1' } else { '0' }).collect()
}

struct Run {
    rig: Rig,
    clips: [LoopClip; 2],
    nets_a: Vec<i64>,
    nets_b: Vec<i64>,
    steps: Vec<JsonValue>,
}

impl Run {
    /// `stSnapshot`.
    fn snap(&mut self, name: &str) {
        let e = &self.rig.e;
        let clips: Vec<JsonValue> = self
            .clips
            .iter()
            .map(|c| {
                obj([
                    ("id", JsonValue::str(c.id.as_str())),
                    ("state", JsonValue::str(state(e.loop_state(&c.id)))),
                    ("switch", e.loop_switch_frame(&c.id).map_or(JsonValue::Null, int)),
                    ("stop", e.loop_stop_frame(&c.id).map_or(JsonValue::Null, int)),
                    ("ref", e.loop_reference(&c.id).map_or(JsonValue::Null, JsonValue::str)),
                    ("target", e.loop_target_bpm(&c.id).map_or(JsonValue::Null, bits)),
                    ("playhead", e.loop_playhead(&c.id).map_or(JsonValue::Null, bits)),
                ])
            })
            .collect();
        let stops: Vec<JsonValue> = e
            .stinger_stops()
            .iter()
            .map(|(id, f)| obj([("id", JsonValue::str(id.as_str())), ("stopAt", int(*f))]))
            .collect();
        let playing: String = ["A", "B"]
            .into_iter()
            .filter(|d| e.deck(d).unwrap().mirror.is_playing())
            .collect();
        let step = obj([
            ("name", JsonValue::str(name)),
            ("frame", int(e.loop_frames())),
            ("clips", JsonValue::Array(clips)),
            ("looping", JsonValue::Array(e.looping_clips().into_iter().map(JsonValue::str).collect())),
            ("stops", JsonValue::Array(stops)),
            ("playing", JsonValue::str(playing)),
            ("dominant", e.loops.dominant_deck().map_or(JsonValue::Null, JsonValue::str)),
            ("hitsUnmutedA", JsonValue::str(unmuted(&self.rig, "A", &self.nets_a))),
            ("hitsUnmutedB", JsonValue::str(unmuted(&self.rig, "B", &self.nets_b))),
        ]);
        self.steps.push(step);
    }

    fn stop_of(&self, id: &str) -> Option<i64> {
        self.rig.e.loop_stop_frame(id)
    }
}

/// `dumpStingerRun`: the same operations on the Rust engine.
fn run_steps(want: &JsonValue) -> Vec<JsonValue> {
    let hits = want.get("hits").unwrap();
    let mut rig = Rig::new();
    let mut a = TrackSpec::new("techno", 1.0, None);
    a.extra.set("bpm", JsonValue::Number(120.0));
    let mut b = TrackSpec::new("house", 2.0, None);
    b.extra.set("bpm", JsonValue::Number(90.0));
    assert!(rig.e.generate("A", &a));
    assert!(rig.e.generate("B", &b));
    rig.e.play("A");
    rig.run_to(2 * BAR + 1_024);
    // x is two bars long: launched on an odd bar of A it would come in half way through, if the
    // Hit did not start it from its top
    let (x, y) = (clip_bars(1, 2), test_clip(2));
    rig.e.restore_clips(&[x.clone(), y.clone()]);
    for d in ["A", "B"] {
        rig.e.deck_mut(d).unwrap().project.as_mut().unwrap().stingers = vec![
            StingerAssignment::fallback(0),
            loop_hit(1, Some(&x.id), None),
            loop_hit(2, Some("not-on-the-row"), None),
            loop_hit(3, None, Some("/nowhere/riser.caf")),
        ];
    }
    // what the Hits are, as `dj_state` shows them
    for (d, key) in [("A", "A"), ("B", "B")] {
        let got: Vec<JsonValue> = rig.e.stingers(d).iter().map(|a| a.json()).collect();
        assert_eq!(JsonValue::Array(got), *hits.get(key).unwrap(), "deck {d}'s Hits");
    }
    let mut r = Run {
        rig,
        clips: [x.clone(), y],
        nets_a: nets_of(hits, "netsA"),
        nets_b: nets_of(hits, "netsB"),
        steps: Vec::new(),
    };
    r.snap("idle");

    // Hit 2: x from its top on the next bar, one pass
    r.rig.e.stinger("A", 1, 1);
    r.snap("hit 2 fired");
    r.rig.run(3_000);
    r.snap("3000 frames on");
    let to = r.rig.e.loop_switch_frame(&x.id).unwrap_or(r.rig.frame()) + 8_192;
    r.rig.run_to(to);
    r.snap("x in");
    r.rig.e.stinger("A", 1, 1);
    r.snap("hit 2 again while it sounds");
    let stop = r.stop_of(&x.id).unwrap_or(r.rig.frame());
    r.rig.run_to(stop - 4_096);
    r.snap("x near its end");
    r.rig.e.stinger("A", 1, 1);
    r.snap("hit 2 again near its end");
    r.rig.run_to(stop + 8_192);
    r.snap("x out");

    // fired, then stopped before it is in: it never starts
    r.rig.e.stinger("A", 1, 1);
    r.rig.e.set_looping(&x.id, false);
    r.snap("hit 2 fired and stopped");
    r.rig.run(3 * BLOCK as i64);
    r.snap("three blocks on");
    r.rig.run(2 * 96_000);
    r.snap("the cancelled Hit settled");

    // the other Hits play their stinger nets
    for (n, what) in [(2, "hit 3 (no such clip)"), (3, "hit 4 (a stock loop that is not there)"), (0, "hit 1 (no candidate)")] {
        r.rig.e.stinger("A", n, 1);
        r.snap(what);
        r.rig.run(4_096);
        r.snap(&format!("hit {} ringing", n + 1));
        r.rig.run(96_000 + 8_192);
        r.snap(&format!("hit {} done", n + 1));
    }

    // deck B at 90 bpm, playing: Hit 2 there lines up with B
    r.rig.e.play("B");
    r.rig.run(3 * 128_000);
    r.snap("deck B playing");
    r.rig.e.stinger("B", 1, 1);
    r.snap("hit 2 on deck B");
    let to = r.rig.e.loop_switch_frame(&x.id).unwrap_or(r.rig.frame()) + 8_192;
    r.rig.run_to(to);
    r.snap("x in from deck B's Hit");
    let stop_b = r.stop_of(&x.id).unwrap_or(r.rig.frame());
    r.rig.run_to(stop_b + 8_192);
    r.snap("x out");
    r.steps
}

/// How far Rust's frames may sit from Swift's, measured (see the module doc): 2 on deck A.
const FRAMES: i64 = 2;
/// On deck B (90 bpm, started later) the loop clock's origin is 3 frames off Swift's: the beat
/// state it is read from is the one the deck published at the end of its last block, a few
/// frames older than the anchor Swift reads. Every frame after the first Hit on B inherits it.
const FRAMES_B: i64 = 3;

fn unbits(v: &JsonValue) -> Option<f64> {
    u64::from_str_radix(v.as_str()?, 16).ok().map(f64::from_bits)
}

fn compare(got: &JsonValue, want: &JsonValue, path: &str, tol: i64, worst: &mut i64, bad: &mut Vec<String>) {
    match (got, want) {
        (JsonValue::Object(g), JsonValue::Object(w)) => {
            for (k, wv) in w.iter() {
                let p = format!("{path}.{k}");
                let Some(gv) = g.get(k) else {
                    bad.push(format!("{p}: missing"));
                    continue;
                };
                if k == "playhead" {
                    // the clip's position when the step was taken: its frames-off from Swift's
                    // launch over its 192,000 frames, and a rounding of a block's worth of adds
                    match (unbits(gv), unbits(wv)) {
                        (Some(a), Some(b)) if (a - b).abs() <= 5e-5 => {}
                        _ if gv == wv => {}
                        _ => bad.push(format!("{p}: rust {} swift {}", gv.canonical_js(), wv.canonical_js())),
                    }
                } else if matches!(k, "switch" | "frame" | "stop" | "stopAt") {
                    match (gv.as_i64(), wv.as_i64()) {
                        (Some(a), Some(b)) => {
                            *worst = (*worst).max((a - b).abs());
                            if (a - b).abs() > tol {
                                bad.push(format!("{p}: rust {a} swift {b}"));
                            }
                        }
                        _ if gv == wv => {}
                        _ => bad.push(format!("{p}: rust {} swift {}", gv.canonical_js(), wv.canonical_js())),
                    }
                } else {
                    compare(gv, wv, &p, tol, worst, bad);
                }
            }
        }
        (JsonValue::Array(g), JsonValue::Array(w)) if g.len() == w.len() => {
            for (i, (gv, wv)) in g.iter().zip(w).enumerate() {
                compare(gv, wv, &format!("{path}[{i}]"), tol, worst, bad);
            }
        }
        _ if got == want => {}
        _ => bad.push(format!("{path}: rust {} swift {}", got.canonical_js(), want.canonical_js())),
    }
}

#[test]
fn a_hit_that_launches_a_loop_matches_swift() {
    let g = golden();
    let want = g.get("run").unwrap().as_array().unwrap();
    let got = run_steps(&g);
    assert_eq!(got.len(), want.len(), "same number of steps");
    let (mut worst, mut bad) = (0, Vec::new());
    let mut tol = FRAMES;
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let name = w.get("name").and_then(JsonValue::as_str).unwrap_or("?");
        if name == "hit 2 on deck B" {
            tol = FRAMES_B;
        }
        compare(g, w, &format!("step {i} ({name})"), tol, &mut worst, &mut bad);
    }
    assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    assert!(worst <= FRAMES_B, "worst frame offset {worst}");
}
