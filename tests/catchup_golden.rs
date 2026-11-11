//! Swift golden for `WaveRunner.catchUp(ticks:)` (`tools/paritydump`, section "catchup":
//! `swift/catchup.json`): the silent run of the tick clock a turntable brake's release makes.
//! It has no Swift test of its own, so its branches were held by nothing in either language: a
//! queued project that takes over on its bar inside the catch-up, a mix-out tail whose end tick
//! falls inside it (the deck stops there), the song's stop-transport (the deck stops, unless a
//! tail plays on past it), and the guards (not playing, zero or negative ticks).
//!
//! A bare runner at 8 kHz (blocks of 128, tempo 480, a linear master) is driven through the same
//! scripted ops as the dump, and after each op compared with Swift: which project sounds and
//! which waits (by index: generations count prepares process-wide), `stopped`, `playing`, the tick
//! phase (bits), the tail's end tick, and the sounding project's marking (its tick and a hash of
//! every token and mute), published after every op.

mod common;
use common::*;
use djrust::cid;
use djrust::json::JsonValue;
use djrust::wave::{
    MarkingMirror, MarkingSnapshot, Master, MixOutTail, WaveOptions, WaveProject, WaveRunner,
};
use djrust::{compose, JsonObject};
use std::sync::Arc;

const SR: f64 = 8000.0;

fn project(genre: &str, seed: f64, structure: Option<&str>) -> djrust::pflow::Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(st) = structure {
        ov.set("structure", st.into());
    }
    compose(genre, &ov.into())
}

#[test]
fn catch_up_matches_swift() {
    let doc = fixture("catchup");
    assert_eq!(f(&doc, "sampleRate"), SR);
    let mut d = Diffs::default();
    for sc in rows(&doc, "scenarios") {
        let name = s(sc, "name");
        let tail = sc
            .get("tail")
            .filter(|t| t.as_object().is_some())
            .map(|t| MixOutTail::new(i(t, "fromTick"), i(t, "bars")));
        let opts = WaveOptions {
            master: Master::Linear,
            tail,
            ..Default::default()
        };
        let mut waiting: Vec<Option<WaveProject>> = Vec::new();
        let mut mirrors: Vec<(i32, Arc<MarkingMirror>)> = Vec::new();
        for p in rows(sc, "projects") {
            let wp = WaveRunner::prepare(
                &project(s(p, "genre"), f(p, "seed"), p.get("structure").and_then(JsonValue::as_str)),
                SR,
                &opts,
                0,
            );
            mirrors.push((wp.generation, wp.marking.clone()));
            waiting.push(Some(wp));
        }
        let index = |g: i32| -> i64 {
            mirrors.iter().position(|(m, _)| *m == g).map_or(0, |k| k as i64 + 1)
        };
        let mut runner = WaveRunner::new(SR, opts.clone());
        runner.load_prepared(waiting[0].take().unwrap());
        runner.set_tempo(480.0);
        let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
        let mut frame = 0i64;

        let state = |runner: &mut WaveRunner, frame: i64| -> JsonValue {
            runner.publish_marking(frame);
            let mut snap = MarkingSnapshot::new();
            let mut marking = None;
            if let Some((_, m)) = mirrors.iter().find(|(g, _)| *g == runner.generation()) {
                if m.read_into(&mut snap) {
                    let tokens: Vec<String> = snap.tokens.iter().map(|t| bits_hex(*t)).collect();
                    let muted: String = snap.muted.iter().map(|m| m.to_string()).collect();
                    marking = Some((
                        index(snap.header.generation),
                        snap.header.tick,
                        cid::hex(&cid::sha256(format!("{}|{muted}", tokens.join(",")).as_bytes())),
                    ));
                }
            }
            let mut o = JsonObject::new();
            o.set("project", JsonValue::int(index(runner.generation())));
            o.set("pending", JsonValue::int(index(runner.pending_generation())));
            o.set("stopped", JsonValue::Bool(runner.stopped()));
            o.set("playing", JsonValue::Bool(runner.playing()));
            o.set("tickPhase", JsonValue::str(bits_hex(runner.tick_phase())));
            o.set("tailEndTick", JsonValue::int(runner.tail_end_tick().unwrap_or(-1)));
            o.set(
                "marking",
                marking.map_or(JsonValue::Null, |(p, tick, sha)| {
                    let mut m = JsonObject::new();
                    m.set("project", JsonValue::int(p));
                    m.set("tick", JsonValue::int(tick));
                    m.set("sha", JsonValue::str(sha));
                    JsonValue::Object(m)
                }),
            );
            JsonValue::Object(o)
        };

        let states = rows(sc, "states");
        let check = |d: &mut Diffs, label: String, got: JsonValue, want: &JsonValue| {
            for key in ["project", "pending", "stopped", "playing", "tickPhase", "tailEndTick"] {
                d.eq(
                    format!("{label} {key}"),
                    got.get(key).unwrap().js_stringified(),
                    want.get(key).unwrap().js_stringified(),
                );
            }
            d.eq(
                format!("{label} marking"),
                got.get("marking").unwrap().js_stringified(),
                want.get("marking").unwrap().js_stringified(),
            );
        };
        check(&mut d, format!("{name}[start]"), state(&mut runner, frame), &states[0]);
        for (k, op) in rows(sc, "ops").iter().enumerate() {
            let (kind, arg) = (s(op, "op"), f(op, "arg"));
            match kind {
                "play" => runner.play(),
                "stop" => runner.stop(),
                "render" => {
                    for _ in 0..arg as usize {
                        runner.process_at(&mut l, Some(&mut r), 128, frame);
                        frame += 128;
                    }
                }
                "catchUp" => runner.catch_up(arg),
                "queue" => runner.load_prepared(waiting[arg as usize].take().expect("queued once")),
                other => panic!("unknown op {other}"),
            }
            check(&mut d, format!("{name}[{} {kind} {arg}]", k + 1), state(&mut runner, frame), &states[k + 1]);
        }
    }
    d.finish("catch-up");
}
