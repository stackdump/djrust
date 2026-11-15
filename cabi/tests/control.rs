//! Control nets' `fire-macro` in the engine (`djw_set_control_macros(1)`).
//!
//! The oracle for the dispatch is the page path: the same project with control macros off and
//! the page firing the same ids, for the same durations, before the quantum after the one whose
//! tick fired the control, must give the same bytes. With every id disabled it must give
//! 0.2.0's bytes (0.2.0 ignores `fire-macro`: the exact oracle for the disabled list).

mod common;

use common::*;
use djrust::json::JsonValue;
use djwasm::*;

const SR: f64 = 48000.0;

fn macro_curve(quanta: usize) -> Case {
    Case {
        name: "techno:11:macro-curve".into(),
        source: Source::Compose(MACRO_CURVE_SPEC.into()),
        sample_rate: SR,
        quanta,
        script: vec![],
    }
}

/// The control fires the engine ran: (quantum they land before, id, duration in the macro's
/// unit as the control's macroBars, or -1).
fn control_fires(doc: &str, events: &[(usize, JsonValue)]) -> Vec<(usize, String, i32)> {
    // macroBars per id, from the doc's own fire-macro controls (one entry per id here)
    let v = JsonValue::parse(doc).unwrap();
    let mut bars = std::collections::HashMap::new();
    for (_, n) in v.get("nets").and_then(JsonValue::as_object).unwrap().iter() {
        if let Some(ts) = n.get("transitions").and_then(JsonValue::as_object) {
            for (_, t) in ts.iter() {
                if let Some(c) = t.get("control") {
                    if c.get("action").and_then(JsonValue::as_str) == Some("fire-macro") {
                        let id = c.get("macro").and_then(JsonValue::as_str).unwrap();
                        let b = c
                            .get("macroBars")
                            .and_then(JsonValue::as_f64)
                            .unwrap_or(-1.0);
                        bars.insert(id.to_string(), b as i32);
                    }
                }
            }
        }
    }
    events
        .iter()
        .filter(|(_, e)| {
            ev_str(e, "k") == "fired" && ev_str(e, "src") == "control" && ev_num(e, "res") == 0.0
        })
        .map(|(_, e)| {
            let frame = ev_num(e, "frame") as usize;
            assert_eq!(frame % Q, 0, "events land on quantum boundaries");
            let id = ev_str(e, "id").to_string();
            let d = *bars.get(&id).unwrap_or(&-1);
            (frame / Q, id, d)
        })
        .collect()
}

#[test]
fn engine_dispatch_equals_the_page_firing_the_same_ids() {
    let _g = lock();
    let case = macro_curve(48000 * 120 / Q); // 2 min: several sections
    let doc = case.source.doc();
    let mut events = Vec::new();
    let engine = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || djw_set_control_macros(1),
        &mut |q| {
            for e in drain_events() {
                events.push((q, e));
            }
        },
    );
    for e in drain_events() {
        events.push((case.quanta, e));
    }
    let fires = control_fires(&doc, &events);
    for (q, e) in &events {
        eprintln!("q{q} {}", e.js_stringified());
    }
    assert!(
        fires.len() >= 2,
        "too few net-macro control fires: {fires:?}"
    );
    // master / aux ids are reported to the page as unsupported, the unknown id as ERR_ARG
    let res_of = |id: &str| {
        events
            .iter()
            .find(|(_, e)| ev_str(e, "src") == "control" && ev_str(e, "id") == id)
            .map(|(_, e)| ev_num(e, "res") as i32)
    };
    for (id, want) in [
        ("sweep-lp", ERR_UNSUPPORTED),
        ("ping-pong", ERR_UNSUPPORTED),
    ] {
        if let Some(r) = res_of(id) {
            assert_eq!(r, want, "{id}");
        }
    }
    // the page path: control macros off, the same fires before the same quanta
    let page = run_with(&case, &|sr| djw_init(sr), &mut |q| {
        for (at, id, d) in &fires {
            if *at == q {
                assert_eq!(fire(id, *d), 0, "{id}");
            }
        }
    });
    assert_eq!(
        engine.hashes.pcm, page.hashes.pcm,
        "engine dispatch != page fires"
    );
    assert_eq!(engine.hashes.muted, page.hashes.muted);
    assert_eq!(engine.hashes.marking, page.hashes.marking);
    // and the macros were audible: not 0.2.0
    let plain = run(&case);
    assert_ne!(engine.hashes.pcm, plain.hashes.pcm);
}

#[test]
fn every_id_disabled_is_0_2_0() {
    let _g = lock();
    let case = macro_curve(48000 * 60 / Q);
    let plain = run(&case);
    let ids = catalog_ids();
    let mut skipped = 0;
    let all_off = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || {
            djw_set_control_macros(1);
            for id in &ids {
                disable(id, true);
            }
        },
        &mut |_| {
            for e in drain_events() {
                match (ev_str(&e, "k"), ev_str(&e, "id")) {
                    ("skipped", _) => skipped += 1,
                    // the curve's "no-such-macro": no catalog id, so nothing to disable
                    ("fired", "") => assert_eq!(ev_num(&e, "res") as i32, ERR_ARG),
                    other => panic!("{other:?}"),
                }
            }
        },
    );
    assert!(skipped >= 2);
    assert_eq!(all_off.hashes, plain.hashes);
    // disabling only the net ids that fire is enough (master/aux ones do nothing in 0.3)
    let net_only = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || {
            djw_set_control_macros(1);
            for id in [
                "drop",
                "breakdown",
                "solo-drums",
                "cut",
                "beat-repeat",
                "double-drop",
                "half-time",
                "tape-stop",
                "tempo-anchor",
            ] {
                disable(id, true);
            }
        },
        &mut |_| {
            drain_events();
        },
    );
    assert_eq!(net_only.hashes, plain.hashes);
    // and re-enabling one makes it fire again
    let one_back = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || {
            djw_set_control_macros(1);
            for id in &ids {
                disable(id, true);
            }
            assert_eq!(disable("drop", false), 0);
        },
        &mut |_| {
            drain_events();
        },
    );
    let drop_fires = {
        let mut n = 0;
        run_full(
            &case,
            &|sr| djw_init(sr),
            &mut || djw_set_control_macros(1),
            &mut |_| {
                n += drain_events()
                    .iter()
                    .filter(|e| ev_str(e, "id") == "drop" && ev_str(e, "k") == "fired")
                    .count();
            },
        );
        n
    };
    if drop_fires > 0 {
        assert_ne!(one_back.hashes.pcm, plain.hashes.pcm);
    }
}

#[test]
fn an_example_share_with_mixed_macros() {
    let _g = lock();
    // voltage-rush: riser + sweep-hp (master: the page's) and beat-repeat (a net macro)
    let case = Case {
        name: "voltage-rush".into(),
        source: Source::File("examples/voltage-rush.json".into()),
        sample_rate: SR,
        quanta: 48000 * 90 / Q,
        script: vec![],
    };
    let mut seen = Vec::new();
    let r = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || djw_set_control_macros(1),
        &mut |_| {
            for e in drain_events() {
                seen.push((
                    ev_str(&e, "k").to_string(),
                    ev_str(&e, "id").to_string(),
                    ev_num(&e, "res") as i32,
                ));
            }
        },
    );
    eprintln!("{seen:?}");
    for (k, id, res) in &seen {
        match (k.as_str(), id.as_str()) {
            ("fired", "beat-repeat") => assert_eq!(*res, 0),
            ("fired", "riser" | "sweep-hp") => assert_eq!(*res, ERR_UNSUPPORTED),
            ("restored", "beat-repeat") | ("tempo", "") => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    let plain = run(&case);
    if seen
        .iter()
        .any(|(k, id, _)| k == "fired" && id == "beat-repeat")
    {
        assert_ne!(r.hashes.pcm, plain.hashes.pcm);
    }
}

/// A two-net project whose control net fires `id` through `control` on its first tick.
fn fire_net_doc(control: &str) -> String {
    format!(
        r#"{{"tempo":120,"nets":{{"kick":{{"role":"music","track":{{"channel":10,"instrument":"kick","group":"drums"}},"places":{{"p0":{{"initial":[1]}},"p1":{{"initial":[0]}}}},"transitions":{{"t0":{{"midi":{{"note":36,"channel":10,"velocity":100,"duration":100}}}},"t1":{{}}}},"arcs":[{{"source":"p0","target":"t0"}},{{"source":"t0","target":"p1"}},{{"source":"p1","target":"t1"}},{{"source":"t1","target":"p0"}}]}},"bass":{{"role":"music","track":{{"channel":2,"instrument":"bass","group":"bass"}},"places":{{"p0":{{"initial":[1]}}}},"transitions":{{"t0":{{"midi":{{"note":40,"channel":2,"velocity":100,"duration":100}}}}}},"arcs":[{{"source":"p0","target":"t0"}},{{"source":"t0","target":"p0"}}]}},"zz-fire":{{"role":"control","track":{{"channel":1}},"places":{{"p0":{{"initial":[1]}},"p1":{{"initial":[0]}}}},"transitions":{{"t0":{{"control":{control}}}}},"arcs":[{{"source":"p0","target":"t0"}},{{"source":"t0","target":"p1"}}]}}}}}}"#
    )
}

fn fired_ids(doc: &str) -> Vec<(String, i32, f64)> {
    assert_eq!(djw_init(SR), 4096);
    assert!(call(djw_load, doc) > 0, "{}", out());
    djw_set_control_macros(1);
    djw_play();
    let mut v = Vec::new();
    let mut fired_tick = None;
    for _ in 0..3000 {
        djw_process(Q as u32);
        for e in drain_events() {
            if ev_str(&e, "k") == "fired" {
                fired_tick = Some(ev_num(&e, "tick"));
            }
            let span = match (ev_str(&e, "k"), fired_tick) {
                ("restored", Some(t)) => ev_num(&e, "tick") - t,
                _ => -1.0,
            };
            v.push((
                format!("{}:{}", ev_str(&e, "k"), ev_str(&e, "id")),
                ev_num(&e, "res") as i32,
                span,
            ));
        }
    }
    v
}

#[test]
fn the_id_is_macro_then_legacy_macro_id_then_target_net() {
    let _g = lock();
    let canonical = fire_net_doc(
        r#"{"action":"fire-macro","macro":"breakdown","macroId":"cut","targetNet":"drop"}"#,
    );
    let legacy = fire_net_doc(r#"{"action":"fire-macro","macroId":"cut","targetNet":"drop"}"#);
    let target = fire_net_doc(r#"{"action":"fire-macro","targetNet":"drop"}"#);
    let unknown = fire_net_doc(r#"{"action":"fire-macro","macroId":"nope"}"#);
    let first = |doc: &str| fired_ids(doc).into_iter().next().unwrap();
    assert_eq!(first(&canonical).0, "fired:breakdown");
    assert_eq!(first(&legacy).0, "fired:cut");
    assert_eq!(first(&target).0, "fired:drop");
    assert_eq!(first(&unknown), ("fired:".to_string(), ERR_ARG, -1.0));
}

#[test]
fn macro_bars_is_the_duration_in_the_macros_unit() {
    let _g = lock();
    // drop: bars (2 bars = 32 ticks); cut: ticks (macroBars 3 = 3 ticks); none: the default
    for (control, id, span) in [
        (
            r#"{"action":"fire-macro","macro":"drop","macroBars":2}"#,
            "drop",
            32.0,
        ),
        (r#"{"action":"fire-macro","macro":"drop"}"#, "drop", 16.0),
        (
            r#"{"action":"fire-macro","macro":"cut","macroBars":3}"#,
            "cut",
            3.0,
        ),
        (r#"{"action":"fire-macro","macro":"cut"}"#, "cut", 2.0),
    ] {
        let ev = fired_ids(&fire_net_doc(control));
        let restored = ev
            .iter()
            .find(|(k, _, _)| k == &format!("restored:{id}"))
            .unwrap_or_else(|| panic!("{control}: {ev:?}"));
        assert_eq!(restored.2, span, "{control}: {ev:?}");
    }
}

/// The legacy `macroId` is read, never written: a document's model CID is the same with or
/// without it (djrust's canonical text drops it, as beats' and Go's do).
#[test]
fn legacy_macro_id_leaves_the_model_cid_alone() {
    let _g = lock();
    let with = fire_net_doc(r#"{"action":"fire-macro","macroId":"cut","targetNet":""}"#);
    let without = fire_net_doc(r#"{"action":"fire-macro","targetNet":""}"#);
    assert_eq!(call(djw_model_cid, &with), 64);
    let a = out();
    assert_eq!(call(djw_model_cid, &without), 64);
    assert_eq!(a, out());
    let p = djrust::Project::from_json(&JsonValue::parse(&with).unwrap());
    let c = p
        .nets
        .get("zz-fire")
        .unwrap()
        .control_bindings
        .get("t0")
        .unwrap();
    assert_eq!(c.macro_id.as_deref(), Some("cut"));
    assert!(!p.to_json().js_stringified().contains("macroId"));
}

#[test]
fn control_macros_off_is_0_2_0_and_reports_nothing() {
    let _g = lock();
    let doc = fire_net_doc(r#"{"action":"fire-macro","macro":"cut"}"#);
    assert_eq!(djw_init(SR), 4096);
    assert!(call(djw_load, &doc) > 0);
    djw_play();
    for _ in 0..500 {
        djw_process(Q as u32);
        assert_eq!(djw_event_count(), 0);
        assert_eq!(djw_macro_mask(), 0);
    }
}

#[test]
fn page_fires_report_and_restore() {
    let _g = lock();
    let doc = Source::Compose(r#"{"genre":"techno","seed":42}"#.into()).doc();
    assert_eq!(djw_init(SR), 4096);
    assert_eq!(fire("drop", -1), ERR_NOT_READY);
    assert!(call(djw_load, &doc) > 0);
    djw_play();
    render_quanta(100, &mut |_| {});
    assert_eq!(fire("drop", 1), 0);
    assert_eq!(fire("sweep-lp", -1), ERR_UNSUPPORTED);
    assert_eq!(fire("hit2", -1), ERR_UNSUPPORTED);
    assert_eq!(
        fire("drop", 0),
        ERR_UNSUPPORTED,
        "permanent stays page-side"
    );
    assert_eq!(fire("nope", -1), ERR_ARG);
    assert_eq!(djw_macro_mask(), 1, "drop is bit 0");
    let ev = drain_events();
    let ks: Vec<String> = ev
        .iter()
        .map(|e| {
            format!(
                "{}:{}:{}:{}",
                ev_str(e, "k"),
                ev_str(e, "id"),
                ev_str(e, "src"),
                ev_num(e, "res")
            )
        })
        .collect();
    assert_eq!(
        ks,
        [
            "fired:drop:page:0",
            "fired:sweep-lp:page:-3",
            "fired:hit2:page:-3",
            "fired:drop:page:-3"
        ]
    );
    assert_eq!(ev_num(&ev[0], "frame"), 100.0 * Q as f64);
    // a tempo macro moves the tempo and reports it; panic restores everything now
    let t0 = djw_tempo();
    assert_eq!(fire("half-time", -1), 0);
    render_quanta(2, &mut |_| {});
    assert!(djw_tempo() < t0);
    let ev = drain_events();
    assert!(ev
        .iter()
        .any(|e| ev_str(e, "k") == "tempo" && ev_num(e, "bpm") == djw_tempo()));
    assert_ne!(djw_macro_mask(), 0);
    djw_panic();
    assert_eq!(djw_macro_mask(), 0);
    assert_eq!(djw_tempo(), t0);
    let ev = drain_events();
    let restored: Vec<&str> = ev
        .iter()
        .filter(|e| ev_str(e, "k") == "restored")
        .map(|e| ev_str(e, "id"))
        .collect();
    assert_eq!(restored, ["drop", "half-time"]);
    assert!(ev
        .iter()
        .any(|e| ev_str(e, "k") == "tempo" && ev_num(e, "bpm") == t0));
}

#[test]
fn the_ring_overflows_by_counting() {
    let _g = lock();
    let doc = Source::Compose(r#"{"genre":"techno","seed":42}"#.into()).doc();
    assert_eq!(djw_init(SR), 4096);
    assert!(call(djw_load, &doc) > 0);
    for _ in 0..100 {
        fire("sweep-lp", -1);
    }
    assert_eq!(djw_event_count(), 65);
    let ev = drain_events();
    assert_eq!(ev.len(), 65);
    assert_eq!(ev_str(&ev[64], "k"), "overflow");
    assert_eq!(ev_num(&ev[64], "res"), 36.0);
    assert_eq!(djw_event_count(), 0);
    assert_eq!(djw_events(), 2);
    assert_eq!(out(), "[]");
}

#[test]
fn keys_describe_the_build() {
    let _g = lock();
    assert!(djw_keys() > 0);
    let k = JsonValue::parse(&out()).unwrap();
    assert_eq!(
        k.get("build").and_then(JsonValue::as_f64),
        Some(djw_features() as f64)
    );
    assert_eq!(k.get("fx").and_then(JsonValue::as_array).unwrap().len(), 14);
    assert_eq!(
        k.get("voice").and_then(JsonValue::as_array).unwrap().len(),
        12
    );
    assert_eq!(catalog_ids().len(), 42);
    // a mask bit is maskBits[group] + index: drop 0, tempo-anchor 8
    assert_eq!(
        k.get("macros")
            .unwrap()
            .get("net")
            .and_then(JsonValue::as_array)
            .unwrap()[8]
            .as_str(),
        Some("tempo-anchor")
    );
}
