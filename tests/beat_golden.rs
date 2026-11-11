//! Swift goldens for the beat clock, the transport, CUE, quantize, sync / follow and relocation
//! on net and sample decks (`tools/paritydump/beat.swift`, section "beat",
//! `tests/fixtures/swift/beat.json`).
//!
//! The dump drives a real `DJEngine` (never started) two decks wide: blocks of 128 frames, the
//! housekeeping tick before the block that finds it due, one list of ops per scenario. The ops
//! are replayed here through [`Engine`] and [`Renderer`] the same way (`common_beat::Stepper`),
//! and the state both decks are in after *every* op is compared with what Swift read: the tick,
//! the transport flags, the tempo, the fader and the nudge, the beat state, the cues, the loop,
//! the platter.
//!
//! Parity form (b) of the port rules. Everything is compared exactly: integers, flags, names and
//! ticks, and every float as its bit pattern, the platter's positions and rates included (the
//! motor's smoothing constants use `exp`, which Apple's libm and the `libm` crate both round the
//! same way for them: every float in the dump is bit-identical, so no tolerance is needed).

mod common_beat;
mod common_fixture;

use common_beat::{Stepper, SR};
use djrust::control::controller::{DeckMode, DeckView};
use djrust::deck::beat::{Quantize, SyncAlign};
use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::deck::waveform::WaveformOverview;
use djrust::json::JsonValue;
use djrust::wave::MixOutTail;
use djrust::TrackSpec;
use djrust::host::deck::sample::{PreparedSample, OVERVIEW_COLUMNS};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("beat")
}

fn bits(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

fn opt(v: Option<f64>) -> JsonValue {
    v.map_or(JsonValue::Null, |v| JsonValue::String(bits(v)))
}

fn s(v: &JsonValue, key: &str) -> String {
    v.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {key} in {}", v.js_stringified()))
        .to_string()
}

fn f(v: &JsonValue, key: &str) -> f64 {
    v.get(key)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {key} in {}", v.js_stringified()))
}

fn os(v: &JsonValue, key: &str) -> Option<String> {
    v.get(key).and_then(JsonValue::as_str).map(String::from)
}

/// The state of a deck as the dump reads it (`stateOf`), as the same JSON.
fn state_of(st: &Stepper, name: &str) -> JsonValue {
    let e = &st.e;
    let d = e.deck(name).unwrap();
    let sample = d.sample.as_ref();
    let bs = d.beat_state(SR);
    let mut cues: Vec<JsonValue> = d
        .hot_cues
        .iter()
        .map(|(k, v)| {
            JsonValue::Array(vec![
                JsonValue::Number(*k as f64),
                JsonValue::String(bits(*v)),
            ])
        })
        .collect();
    cues.sort_by(|a, b| {
        let k = |v: &JsonValue| v.as_array().unwrap()[0].as_f64().unwrap();
        k(a).partial_cmp(&k(b)).unwrap()
    });
    let net = DeckView::mode(d) == DeckMode::Net;
    let mut o = djrust::json::JsonObject::new();
    let mut put = |k: &str, v: JsonValue| o.set(k, v);
    put(
        "mode",
        JsonValue::String(if net { "net" } else { "sample" }.into()),
    );
    put("playing", JsonValue::Bool(d.playing));
    put("tick", JsonValue::Number(d.mirror.tick() as f64));
    put("tempo", JsonValue::String(bits(d.mirror.tempo())));
    put(
        "wavePlaying",
        JsonValue::Bool(!d.mirror.is_sample_mode() && d.mirror.is_playing()),
    );
    put(
        "ended",
        JsonValue::Bool(d.mirror.stopped.load(std::sync::atomic::Ordering::Relaxed)),
    );
    put(
        "tailEnd",
        d.mirror
            .tail_end_tick()
            .map_or(JsonValue::Null, |t| JsonValue::Number(t as f64)),
    );
    put("pitch", JsonValue::String(bits(d.pitch)));
    put("netNudge", JsonValue::String(bits(d.net_nudge)));
    put("baseBPM", opt(d.base_bpm()));
    put("effectiveBPM", opt(d.effective_bpm()));
    put("beatBpm", opt(bs.map(|b| b.bpm)));
    put("beat", opt(bs.map(|b| b.beat)));
    put(
        "quantize",
        JsonValue::String(
            match d.quantize {
                Quantize::Off => "off",
                Quantize::Beat => "beat",
                Quantize::Bar => "bar",
            }
            .into(),
        ),
    );
    put("hotCues", JsonValue::Array(cues));
    put("cuePoint", opt(Some(sample.map_or(0.0, |s| s.cue_point))));
    put("netCueTick", JsonValue::String(bits(d.net_cue_tick)));
    put("loopStart", opt(sample.and_then(|s| s.loop_start)));
    put("loopEnd", opt(sample.and_then(|s| s.loop_end)));
    put(
        "looping",
        JsonValue::Bool(sample.is_some_and(|s| s.looping)),
    );
    put(
        "position",
        JsonValue::String(bits(d.mirror.turntable_position())),
    );
    put("rate", JsonValue::String(bits(d.mirror.turntable_rate())));
    put(
        "length",
        JsonValue::Number(
            d.mirror
                .tt_length
                .load(std::sync::atomic::Ordering::Relaxed) as f64,
        ),
    );
    put(
        "leader",
        d.leader
            .as_ref()
            .map_or(JsonValue::Null, |l| JsonValue::String(l.clone())),
    );
    put("sections", JsonValue::Number(d.sections.len() as f64));
    put(
        "section",
        e.current_section(name)
            .map_or(JsonValue::Null, |i| JsonValue::Number(i as f64)),
    );
    JsonValue::Object(o)
}

fn spec_of(op: &JsonValue) -> TrackSpec {
    let mut spec = TrackSpec::new(
        &s(op, "genre"),
        f(op, "seed"),
        op.get("structure").and_then(JsonValue::as_str),
    );
    if let Some(bpm) = op.get("bpm").and_then(JsonValue::as_f64) {
        spec.extra.set("bpm", JsonValue::Number(bpm));
    }
    spec
}

/// One op (`run` in the dump); the `sync` result as the dump writes it.
fn run(st: &mut Stepper, op: &JsonValue) -> JsonValue {
    let kind = s(op, "op");
    if kind == "render" {
        st.step(f(op, "blocks") as usize);
        return JsonValue::Null;
    }
    let deck = os(op, "deck").unwrap_or_else(|| "A".into());
    let d = deck.as_str();
    if kind == "renderUntilEnded" {
        for _ in 0..f(op, "max") as usize {
            st.block();
            if st.e.deck(d).unwrap().mirror.stopped.load(Ordering::Relaxed) {
                break;
            }
        }
        return JsonValue::Null;
    }
    let e = &mut st.e;
    match kind.as_str() {
        "net" => assert!(e.generate(d, &spec_of(op))),
        "demo" => assert!(e.load_demo(d)),
        "blank" => {
            let buffer = SampleBuffer::new(vec![0.0; f(op, "frames") as usize], None, SR);
            let overview = WaveformOverview::new(&buffer, OVERVIEW_COLUMNS);
            assert!(e.load_sample(
                d,
                PreparedSample {
                    title: "blank".into(),
                    buffer: Arc::new(buffer),
                    overview,
                    grid: None,
                }
            ));
        }
        "play" => {
            e.set_playing(d, true);
        }
        "pause" => {
            e.set_playing(d, false);
        }
        "cue" => e.cue(d),
        "clearCue" => e.clear_cue(d, f(op, "slot") as usize),
        "hotCue" => e.hot_cue(d, f(op, "slot") as usize),
        "loopIn" => e.loop_in(d),
        "loopOut" => e.loop_out(d),
        "loopHalve" => e.loop_halve(d),
        "loopDouble" => e.loop_double(d),
        "loopBeats" => e.loop_beats(d, f(op, "beats")),
        "rewind" => {
            e.rewind(d);
        }
        "follow" => {
            let leader = os(op, "leader");
            e.follow(d, leader.as_deref());
        }
        "setBPM" => {
            e.set_bpm(d, f(op, "bpm"));
        }
        "downbeat" => {
            e.set_downbeat_here(d);
        }
        "quantize" => {
            let q = match s(op, "mode").as_str() {
                "beat" => Quantize::Beat,
                "bar" => Quantize::Bar,
                _ => Quantize::Off,
            };
            e.set_quantize(d, q);
        }
        "pitch" => {
            e.set_pitch(d, f(op, "value"));
        }
        "nudge" => {
            e.nudge(d, f(op, "ratio"));
        }
        "sync" => {
            let other = e.sync_partner(d).unwrap();
            let align = match s(op, "align").as_str() {
                "bar" => SyncAlign::Bar,
                "tempo" => SyncAlign::TempoOnly,
                _ => SyncAlign::Beat,
            };
            return match e.sync(d, &other, align) {
                None => JsonValue::Null,
                Some(r) => {
                    let mut o = djrust::json::JsonObject::new();
                    o.set("pitch", JsonValue::String(bits(r.pitch)));
                    o.set("multiplier", JsonValue::String(bits(r.multiplier)));
                    o.set("tempoClamped", JsonValue::Bool(r.tempo_clamped));
                    o.set("shiftedBeats", JsonValue::String(bits(r.shifted_beats)));
                    JsonValue::Object(o)
                }
            };
        }
        "section" => {
            e.launch_section(d, f(op, "index") as usize);
        }
        "stepSection" => {
            e.step_section(d, f(op, "delta") as i64);
        }
        "relocate" => {
            e.relocate(d, f(op, "tick") as i64, None, None);
        }
        "relocateTail" => {
            let tail = MixOutTail::new(f(op, "from") as i64, f(op, "bars") as i64);
            e.relocate(d, f(op, "tick") as i64, Some(tail), None);
        }
        "launch" => {
            e.launch(d);
        }
        "seek" => {
            e.turntable(d, TurntableCommand::Seek(f(op, "sample")));
        }
        "transportOff" => {
            e.turntable(d, TurntableCommand::Transport(false));
        }
        "setTempo" => {
            e.set_tempo(d, f(op, "bpm"));
        }
        other => panic!("unknown op {other}"),
    }
    JsonValue::Null
}

#[derive(Default)]
struct Tally {
    checked: usize,
    floats: usize,
    failures: Vec<String>,
}

fn compare(t: &mut Tally, ctx: &str, step: usize, deck: &str, want: &JsonValue, got: &JsonValue) {
    let (JsonValue::Object(w), JsonValue::Object(g)) = (want, got) else {
        panic!("not objects")
    };
    // a runner with nothing installed reads 0 BPM in Swift and 120 here (nothing looks at it):
    // a deck that never held a net, or a net just handed over that no block has installed yet
    let net = w.get("mode").and_then(JsonValue::as_str) == Some("net");
    let nothing_installed = w.get("tempo").and_then(JsonValue::as_str) == Some("0000000000000000");
    for (k, wv) in w.iter() {
        if k == "tempo" && (!net || nothing_installed) {
            continue;
        }
        // a sample deck's runner is not rendered, so Swift never consumes the `pause` that
        // `leaveNetMode` sent it: its `isPlaying` stays as the net left it (stale, and nothing
        // reads it off a net), where the runner here is paused with the switch
        if k == "wavePlaying" && !net {
            continue;
        }
        let gv = g.get(k).unwrap_or(&JsonValue::Null);
        t.checked += 1;
        if wv == gv {
            // a bit pattern is a 16-digit hex string
            if wv
                .as_str()
                .is_some_and(|s| s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()))
            {
                t.floats += 1;
            }
            continue;
        }
        t.failures.push(format!(
            "{ctx} step {step} {deck}.{k}: swift {} rust {}",
            wv.js_stringified(),
            gv.js_stringified()
        ));
    }
}

fn replay(only: Option<&str>) -> Tally {
    let doc = golden();
    let mut t = Tally::default();
    for sc in doc.get("scenarios").and_then(JsonValue::as_array).unwrap() {
        let name = s(sc, "name");
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let mut st = Stepper::new();
        for (n, step) in sc
            .get("steps")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .enumerate()
        {
            let op = step.get("op").unwrap();
            let result = run(&mut st, op);
            let want_result = step.get("result").unwrap();
            t.checked += 1;
            if *want_result != result {
                t.failures.push(format!(
                    "{name} step {n} {} result: swift {} rust {}",
                    op.js_stringified(),
                    want_result.js_stringified(),
                    result.js_stringified()
                ));
            }
            for deck in ["A", "B"] {
                let got = state_of(&st, deck);
                compare(
                    &mut t,
                    &format!("{name} {}", op.js_stringified()),
                    n,
                    deck,
                    step.get(deck).unwrap(),
                    &got,
                );
            }
        }
    }
    t
}

#[test]
fn every_scenario_ends_every_op_in_the_state_swift_read() {
    let t = replay(None);
    assert!(
        t.checked > 5000 && t.floats > 3000,
        "only {} checks, {} floats",
        t.checked,
        t.floats
    );
    assert!(
        t.failures.is_empty(),
        "{} of {} differ:\n{}",
        t.failures.len(),
        t.checked,
        t.failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The scenarios the dump carries are the ones this file means to hold: a renamed or dropped
/// one would otherwise only shrink the comparison.
#[test]
fn the_dump_carries_the_scenarios() {
    let doc = golden();
    let names: Vec<String> = doc
        .get("scenarios")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|sc| s(sc, "name"))
        .collect();
    for want in [
        "quantize_hot_cues",
        "quantize_loops",
        "quantize_phase_kept",
        "sample_cue_and_loop",
        "net_cues",
        "net_sections",
        "sync_tempo",
        "sync_clamped",
        "sync_half_time",
        "sync_nothing",
        "sync_bar_net",
        "sync_sample_to_net",
        "follow",
        "pitch_and_nudge",
        "grid_edit",
        "grid_unwrapped",
        "reload_while_playing",
        "relocate_tail",
        "song_end",
        "relocate_after_end",
        "launch",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} missing from {names:?}"
        );
    }
}
