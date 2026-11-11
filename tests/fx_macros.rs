//! Oracle: beats' real macro code (catalog.js, effects.js, runtime.js, feel/axes.js,
//! controllers.js, tone-engine.js's master setters) and Tone 15.1.22's own Param timeline, run
//! by beats-btw-dj's `scripts/beats-macro-fx.mjs` into `fixtures/js/macro-fx.json`. Every case
//! is replayed through `FxAutomation` (the slider / tempo dispatches) and `MasterParams` (the
//! Tone-level calls and the parameter values Tone read back mid-run). Mirrors the Swift
//! `MasterFxParityTests`.

use djrust::fx::master_macro::{Kind, Op};
use djrust::fx::{
    FxAutomation, FxAutomationEvent, MasterFxKey, MasterMacro, MasterParamId, MasterParamOp,
    MasterParams,
};
use djrust::json::JsonValue;

fn fixture() -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/js/macro-fx.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    JsonValue::parse(&text).unwrap()
}

fn near(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-9 * 1.0f64.max(a.abs()).max(b.abs())
}

fn n(v: &JsonValue) -> f64 {
    v.as_f64().unwrap()
}

fn s(v: &JsonValue) -> &str {
    v.as_str().unwrap()
}

struct Replay {
    fx: Vec<(f64, &'static str, i32)>,
    tempo: Vec<(f64, f64)>,
    ops: Vec<MasterParamOp>,
    probes: Vec<(f64, String, f64)>,
    automation: FxAutomation,
}

struct State {
    params: MasterParams,
    fx: Vec<(f64, &'static str, i32)>,
    tempo: Vec<(f64, f64)>,
    ops: Vec<MasterParamOp>,
    probes: Vec<(f64, String, f64)>,
    pending: Vec<(f64, String)>,
    next: usize,
}

impl State {
    fn read_probes(&mut self, t: f64, inclusive: bool) {
        while self.next < self.pending.len()
            && (if inclusive {
                self.pending[self.next].0 <= t
            } else {
                self.pending[self.next].0 < t
            })
        {
            let (pt, name) = self.pending[self.next].clone();
            let v = self
                .params
                .value(MasterParamId::from_raw(&name).unwrap(), pt);
            self.probes.push((pt, name, v));
            self.next += 1;
        }
    }

    fn handle(&mut self, e: FxAutomationEvent) {
        match e {
            FxAutomationEvent::Fx {
                time, key, value, ..
            } => {
                self.read_probes(time, false);
                self.fx.push((time, key.id(), value));
                let ops = &mut self.ops;
                self.params
                    .apply(key, value, time, Some(&mut |o| ops.push(o)));
            }
            FxAutomationEvent::Tempo { time, bpm } => self.tempo.push((time, bpm)),
        }
    }
}

/// Replays a fixture case: the steps at their times (steps of one instant together, before the
/// instant's events), every dispatch into `MasterParams`, the probes read as the run passes them.
fn replay(c: &JsonValue) -> Replay {
    let bpm = n(c.get("bpm").unwrap());
    let mut a = FxAutomation::new(bpm, n(c.get("genreBpm").unwrap()));
    if let Some(p) = c.get("puck").and_then(JsonValue::as_array) {
        a.set_puck(n(&p[0]), n(&p[1]));
    }
    let mut pending: Vec<(f64, String)> = c
        .get("probes")
        .and_then(JsonValue::as_array)
        .map(|p| {
            p.iter()
                .map(|x| {
                    (
                        n(&x.at(0).unwrap().clone()),
                        s(x.at(1).unwrap()).to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    pending.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let mut st = State {
        params: MasterParams::new(48_000.0),
        fx: vec![],
        tempo: vec![],
        ops: vec![],
        probes: vec![],
        pending,
        next: 0,
    };
    let steps = c.get("steps").and_then(JsonValue::as_array).unwrap();
    let mut i = 0;
    while i < steps.len() {
        let t = n(steps[i].at(0).unwrap());
        a.advance(t, &mut |e| st.handle(e));
        a.set_now(t);
        while i < steps.len() && n(steps[i].at(0).unwrap()) == t {
            let step = &steps[i];
            match s(step.at(1).unwrap()) {
                "fire" => {
                    let d = step.at(3).and_then(JsonValue::as_f64).map(|d| d as i32);
                    let m = MasterMacro::from_id(s(step.at(2).unwrap())).unwrap();
                    a.fire(m, d, &mut |e| st.handle(e));
                }
                "panic" => a.panic(&mut |e| st.handle(e)),
                "set" => a.set_slider(
                    MasterFxKey::from_id(s(step.at(2).unwrap())).unwrap(),
                    n(step.at(3).unwrap()),
                ),
                other => panic!("unknown step {other}"),
            }
            i += 1;
        }
    }
    a.advance(f64::INFINITY, &mut |e| st.handle(e));
    st.read_probes(f64::INFINITY, true);
    Replay {
        fx: st.fx,
        tempo: st.tempo,
        ops: st.ops,
        probes: st.probes,
        automation: a,
    }
}

#[test]
fn fixture_is_there() {
    let f = fixture();
    assert!(f.get("cases").unwrap().as_array().unwrap().len() > 600);
    assert_eq!(
        s(f.get("meta")
            .unwrap()
            .get("tone")
            .unwrap()
            .get("version")
            .unwrap()),
        "15.1.22"
    );
}

/// The Rust catalog is beats' catalog, minus the slider moves that never reach wave audio.
#[test]
fn catalog_matches_beats() {
    let f = fixture();
    let dropped = ["reverb-wet", "reverb-size", "reverb-damp"];
    let entries = f
        .get("meta")
        .unwrap()
        .get("catalog")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), MasterMacro::ALL.len());
    for e in entries {
        let id = s(e.get("id").unwrap());
        let m = MasterMacro::from_id(id).unwrap_or_else(|| panic!("{id} is ported"));
        assert_eq!(m.label(), s(e.get("label").unwrap()));
        assert_eq!(m.group(), s(e.get("group").unwrap()));
        assert_eq!(
            e.get("durationUnit").and_then(JsonValue::as_str),
            Some("bar")
        );
        assert_eq!(
            m.default_duration(),
            n(e.get("defaultDuration").unwrap()) as i32
        );
        let opts: Vec<i32> = e
            .get("durationOpts")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|x| n(x) as i32)
            .collect();
        assert_eq!(m.duration_options(), &opts[..]);
        let ops: Vec<Op> = e
            .get("ops")
            .and_then(JsonValue::as_array)
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter(|o| !dropped.contains(&s(o.get("fxKey").unwrap())))
            .map(|o| Op {
                key: MasterFxKey::from_id(s(o.get("fxKey").unwrap())).unwrap(),
                to: n(o.get("toValue").unwrap()),
            })
            .collect();
        let tail = e.get("tailFrac").and_then(JsonValue::as_f64).unwrap_or(0.6);
        match (s(e.get("kind").unwrap()), m.kind()) {
            ("fx-sweep", Kind::Sweep(o)) => assert_eq!(o, &ops[..], "{id}"),
            ("fx-hold", Kind::Hold(o, t)) => {
                assert_eq!(o, &ops[..], "{id}");
                assert_eq!(t, tail, "{id}");
            }
            ("compound", Kind::Compound) => {
                let steps: Vec<(String, f64)> = e
                    .get("steps")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| {
                        (
                            s(x.get("macroId").unwrap()).to_string(),
                            n(x.get("offsetMs").unwrap()),
                        )
                    })
                    .collect();
                assert_eq!(
                    steps.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
                    ["tape-stop", "octave-down"]
                );
                assert_eq!(steps.iter().map(|x| x.1).collect::<Vec<_>>(), [0.0, 60.0]);
            }
            ("feel-snap", Kind::FeelSnap { x, y }) | ("feel-sweep", Kind::FeelSweep { x, y }) => {
                let t = e.get("target").unwrap().as_array().unwrap();
                assert_eq!([x, y], [n(&t[0]), n(&t[1])], "{id}");
            }
            ("genre-reset", Kind::GenreReset) => {}
            (k, other) => panic!("{id}: kind {k} vs {other:?}"),
        }
    }
}

/// Every case: the same slider dispatches at the same times (1e-9), the same tempo writes, the
/// same end state.
#[test]
fn dispatches_match_beats() {
    let f = fixture();
    let mut failures = 0;
    let cases = f.get("cases").unwrap().as_array().unwrap();
    for c in cases {
        let name = s(c.get("name").unwrap());
        let r = replay(c);
        let mut want: Vec<(f64, String, i32)> = vec![];
        let mut want_tempo: Vec<(f64, f64)> = vec![];
        for e in c.get("log").unwrap().as_array().unwrap() {
            if s(e.at(1).unwrap()) == "fx" {
                want.push((
                    n(e.at(0).unwrap()),
                    s(e.at(2).unwrap()).to_string(),
                    n(e.at(3).unwrap()) as i32,
                ));
            } else {
                want_tempo.push((n(e.at(0).unwrap()), n(e.at(2).unwrap())));
            }
        }
        // within one instant the order of different sliders is not observable: compare sorted
        let mut got: Vec<(f64, String, i32)> =
            r.fx.iter().map(|x| (x.0, x.1.to_string(), x.2)).collect();
        let order = |a: &(f64, String, i32), b: &(f64, String, i32)| {
            a.0.partial_cmp(&b.0).unwrap().then_with(|| a.1.cmp(&b.1))
        };
        got.sort_by(order);
        want.sort_by(order);
        let mut ok = got.len() == want.len()
            && got
                .iter()
                .zip(&want)
                .all(|(g, w)| near(g.0, w.0) && g.1 == w.1 && g.2 == w.2);
        ok = ok
            && r.tempo.len() == want_tempo.len()
            && r.tempo
                .iter()
                .zip(&want_tempo)
                .all(|(a, b)| near(a.0, b.0) && a.1 == b.1);
        let sliders = c.get("sliders").unwrap();
        for k in MasterFxKey::ALL {
            if r.automation.slider(k) != n(sliders.get(k.id()).unwrap()) as i32 {
                ok = false;
            }
        }
        if r.automation.tempo != n(c.get("tempo").unwrap()) {
            ok = false;
        }
        if r.automation.is_busy() {
            ok = false;
        }
        if !ok {
            failures += 1;
            if failures <= 5 {
                eprintln!(
                    "{name}:\n  got  {:?}\n  want {:?}\n  tempo got {:?} want {:?}",
                    &got[..got.len().min(40)],
                    &want[..want.len().min(40)],
                    r.tempo,
                    want_tempo
                );
            }
        }
    }
    assert_eq!(failures, 0, "{failures} cases differ");
    eprintln!("{} cases match", cases.len());
}

/// The cases that carry them: the same Tone-level calls, and Tone's values read back mid-run.
#[test]
fn tone_calls_and_values_match_beats() {
    let f = fixture();
    let mut failures = 0;
    let mut probes = 0usize;
    let mut worst = 0.0f64;
    for c in f.get("cases").unwrap().as_array().unwrap() {
        if c.get("ops").is_none() {
            continue;
        }
        let name = s(c.get("name").unwrap());
        let r = replay(c);
        // calls on different parameters at one instant are independent: compare per parameter,
        // in order (a stable sort by time then parameter)
        let mut want: Vec<&JsonValue> = c.get("ops").unwrap().as_array().unwrap().iter().collect();
        want.sort_by(|a, b| {
            n(a.at(0).unwrap())
                .partial_cmp(&n(b.at(0).unwrap()))
                .unwrap()
                .then_with(|| s(a.at(1).unwrap()).cmp(s(b.at(1).unwrap())))
        });
        let mut got: Vec<&MasterParamOp> = r.ops.iter().collect();
        got.sort_by(|a, b| {
            a.time
                .partial_cmp(&b.time)
                .unwrap()
                .then_with(|| a.param.raw_value().cmp(b.param.raw_value()))
        });
        let mut ok = got.len() == want.len();
        if ok {
            for (g, w) in got.iter().zip(&want) {
                let kind = s(w.at(2).unwrap());
                let same = near(g.time, n(w.at(0).unwrap()))
                    && g.param.raw_value() == s(w.at(1).unwrap())
                    && g.kind.raw_value() == kind
                    && near(g.a, n(w.at(3).unwrap()))
                    && (kind != "ramp" || near(g.b, n(w.at(4).unwrap())));
                if !same {
                    ok = false;
                    break;
                }
            }
        }
        let mut want_probes: Vec<(f64, String, f64)> = c
            .get("probes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    n(p.at(0).unwrap()),
                    s(p.at(1).unwrap()).to_string(),
                    n(p.at(2).unwrap()),
                )
            })
            .collect();
        want_probes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then_with(|| a.1.cmp(&b.1)));
        let mut got_probes = r.probes.clone();
        got_probes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then_with(|| a.1.cmp(&b.1)));
        if got_probes.len() != want_probes.len() {
            ok = false;
        }
        for (g, w) in got_probes.iter().zip(&want_probes) {
            probes += 1;
            let err = (g.2 - w.2).abs() / 1.0f64.max(w.2.abs());
            worst = worst.max(err);
            if !(g.0 == w.0 && g.1 == w.1 && near(g.2, w.2)) {
                ok = false;
            }
        }
        if !ok {
            failures += 1;
            if failures <= 5 {
                let bad = got_probes
                    .iter()
                    .zip(&want_probes)
                    .find(|(g, w)| !(g.0 == w.0 && g.1 == w.1 && near(g.2, w.2)));
                eprintln!(
                    "{name}: ops {} vs {}; first bad probe {:?}",
                    r.ops.len(),
                    want.len(),
                    bad
                );
            }
        }
    }
    eprintln!("{probes} probes, worst relative error {worst:e}");
    assert_eq!(
        failures, 0,
        "{failures} cases differ ({probes} probes, worst relative error {worst})"
    );
    assert!(probes > 20_000);
}
