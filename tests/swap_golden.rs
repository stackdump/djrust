//! Swift goldens for the track pipeline: generate, swap, stepSeed, the look-ahead, the next
//! track and `service`, relocation with the song's options, `mixOut` / `mixIn`
//! (`tools/paritydump/swap.swift`, section "swap", `tests/fixtures/swift/swap.json`).
//!
//! The dump drives a real `DJEngine(offline: true)` at 22.05 kHz, two decks, `deterministic` on:
//! blocks of 128 frames, the housekeeping tick (`Deck.service`) before the block that finds it
//! due, one list of ops per scenario. The ops are replayed here through [`Engine`] and
//! [`Renderer`] the same way (`common_track::Stepper`), and the state both decks are in after
//! *every* op is compared with what Swift read: the tick, the transport flags, the installed
//! project's start tick and tail, which song it is, the played project's model CID, the spec
//! and the slots.
//!
//! Parity form (b) of the port rules. Everything is compared exactly. Process-global counters
//! (`WaveProject.generation`, `trackSerial`) are not in the dump: it says whether a deck's
//! generation moved since the last op and which song (the n-th distinct serial seen in the
//! scenario) a project belongs to, and this side computes the same from its own counters.
//! A "bare" scenario loads tracks with no musicality (Swift) / on an engine without the pass
//! (here), so the rest schedules, which this runner builds none of by itself, are compared
//! entry by entry.

mod common_fixture;
mod common_track;

use common_track::Stepper;
use djrust::deck::slots::{LookAheadKey, SwapStart};
use djrust::json::{JsonObject, JsonValue};
use djrust::musicality::Fixes;
use djrust::wave::rests::{Entry, MixOutTail, RestSchedule};
use djrust::TrackSpec;
use djrust::host::deck::PreparedTrack;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("swap")
}

fn bits(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

fn ob(v: Option<f64>) -> JsonValue {
    v.map_or(JsonValue::Null, |v| JsonValue::String(bits(v)))
}

fn num(v: i64) -> JsonValue {
    JsonValue::Number(v as f64)
}

fn oi(v: Option<i64>) -> JsonValue {
    v.map_or(JsonValue::Null, num)
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

fn spec_text(spec: Option<&TrackSpec>) -> JsonValue {
    spec.map_or(JsonValue::Null, |s| {
        JsonValue::String(s.payload().canonical_js())
    })
}

fn start_text(st: SwapStart) -> String {
    match st {
        SwapStart::Top => "top".into(),
        SwapStart::NextBar => "nextBar".into(),
        SwapStart::Tick(t) => format!("tick:{t}"),
    }
}

fn start_of(op: &JsonValue) -> SwapStart {
    match os(op, "start").as_deref().unwrap_or("top") {
        "top" => SwapStart::Top,
        "nextBar" => SwapStart::NextBar,
        t => SwapStart::Tick(t.strip_prefix("tick:").expect("tick:N").parse().unwrap()),
    }
}

fn spec_of(op: &JsonValue) -> TrackSpec {
    TrackSpec::new(
        &s(op, "genre"),
        f(op, "seed"),
        op.get("structure").and_then(JsonValue::as_str),
    )
}

fn entries(r: &RestSchedule) -> JsonValue {
    JsonValue::Array(
        r.entries
            .iter()
            .map(|e| JsonValue::Array(vec![num(e.from), num(e.to), num(e.mask.0 as i64)]))
            .collect(),
    )
}

/// What the dump counts instead of the process-global numbers.
#[derive(Default)]
struct Tracker {
    serials: Vec<i32>,
    last_generation: std::collections::BTreeMap<String, i32>,
}

impl Tracker {
    fn serial_index(&mut self, serial: i32) -> usize {
        if let Some(k) = self.serials.iter().position(|s| *s == serial) {
            return k;
        }
        self.serials.push(serial);
        self.serials.len() - 1
    }
}

fn fixes_name(f: Fixes) -> &'static str {
    if f.is_empty() {
        "empty"
    } else if f == Fixes::ALL {
        "all"
    } else if f == Fixes::RUNNER_ONLY {
        "runnerOnly"
    } else {
        "other"
    }
}

fn state_of(st: &mut Stepper, tr: &mut Tracker, name: &str, bare: bool) -> JsonValue {
    let look = st.e.look_ahead_key(name);
    let look_ready = st.e.look_ahead_ready(name);
    let d = st.e.deck(name).unwrap();
    let net = d.is_net();
    let mut latest = JsonValue::Null;
    if let Some(l) = &d.track.latest {
        let moved = tr.last_generation.get(name) != Some(&l.generation);
        tr.last_generation.insert(name.to_string(), l.generation);
        let mut o = JsonObject::new();
        o.set("startTick", num(l.start_tick));
        o.set("serial", num(tr.serial_index(l.track_serial) as i64));
        o.set("newGeneration", JsonValue::Bool(moved));
        o.set(
            "tail",
            l.tail.map_or(JsonValue::Null, |t| {
                JsonValue::Array(vec![num(t.from_tick), num(t.bars)])
            }),
        );
        if bare {
            o.set("rests", entries(&l.rests));
            o.set("songRests", entries(&l.song_rests));
        }
        latest = JsonValue::Object(o);
    }
    let installed = d.track.latest.as_ref().is_none_or(|l| {
        d.mirror
            .generation
            .load(std::sync::atomic::Ordering::Relaxed)
            == l.generation
    });
    let mut o = JsonObject::new();
    let mut put = |k: &str, v: JsonValue| o.set(k, v);
    put(
        "mode",
        JsonValue::String(if net { "net" } else { "sample" }.into()),
    );
    put("playing", JsonValue::Bool(d.playing));
    put("wavePlaying", JsonValue::Bool(net && d.mirror.is_playing()));
    put("tick", num(d.mirror.tick()));
    put(
        "ended",
        JsonValue::Bool(net && d.mirror.stopped.load(std::sync::atomic::Ordering::Relaxed)),
    );
    put(
        "tailEnd",
        oi(net.then(|| d.mirror.tail_end_tick()).flatten()),
    );
    put("installed", JsonValue::Bool(installed));
    put("latest", latest);
    put("spec", spec_text(d.spec.as_ref()));
    put("rawNets", JsonValue::Bool(d.track.raw_nets.is_some()));
    put("passApplied", JsonValue::Bool(d.track.pass_applied));
    put(
        "fixes",
        d.track
            .fixes
            .map_or(JsonValue::Null, |f| JsonValue::String(fixes_name(f).into())),
    );
    put(
        "projectCID",
        d.project
            .as_ref()
            .map_or(JsonValue::Null, |p| JsonValue::String(p.model_cid())),
    );
    put("seedTarget", spec_text(d.track.seed_target.as_ref()));
    put("advances", num(d.track.advances as i64));
    put("awaitingNext", JsonValue::Bool(d.track.awaiting_next));
    put("startingNext", JsonValue::Bool(d.track.starting_next));
    put("autoAdvance", JsonValue::Bool(d.auto_advance));
    put("nextReady", spec_text(d.track.next.ready_spec()));
    put("nextComposing", spec_text(d.track.next.pending_spec()));
    put("isNetLoop", JsonValue::Bool(d.is_net_loop()));
    put(
        "isMixingOut",
        JsonValue::Bool(net && d.mirror.tail_end_tick().is_some()),
    );
    put(
        "isMixingIn",
        JsonValue::Bool(
            net && djrust::deck::perform::is_mixing_in(d.track.mix_in_end_tick, d.mirror.tick()),
        ),
    );
    put("mixInEndTick", oi(d.track.mix_in_end_tick));
    put("sections", num(d.sections.len() as i64));
    put(
        "section",
        oi(
            djrust::deck::perform::current_section_index(&d.sections, d.mirror.tick())
                .map(|i| i as i64),
        ),
    );
    put("netBaseBPM", JsonValue::String(bits(d.base_tempo)));
    put("pitch", JsonValue::String(bits(d.pitch)));
    put("baseBPM", ob(d.base_bpm()));
    put("effectiveBPM", ob(d.effective_bpm()));
    put(
        "lookAhead",
        look.map_or(JsonValue::Null, |k| {
            let mut l = JsonObject::new();
            l.set("step", num(k.step));
            l.set("spec", spec_text(Some(&k.spec)));
            l.set("start", JsonValue::String(start_text(k.start)));
            l.set("ready", JsonValue::Bool(look_ready));
            JsonValue::Object(l)
        }),
    );
    JsonValue::Object(o)
}

fn tail_of(op: &JsonValue) -> Option<MixOutTail> {
    let t = op.get("tail")?;
    Some(MixOutTail::new(f(t, "from") as i64, f(t, "bars") as i64))
}

fn rest_of(op: &JsonValue) -> Option<Entry> {
    let r = op.get("rest")?;
    Some(Entry::new(
        f(r, "from") as i64,
        f(r, "to") as i64,
        MixOutTail::REST_MASK,
    ))
}

/// One op (`run` in the dump); its result as the dump writes it.
fn run(st: &mut Stepper, op: &JsonValue) -> JsonValue {
    let kind = s(op, "op");
    if kind == "render" {
        st.step(f(op, "blocks") as usize);
        return JsonValue::Null;
    }
    let deck = os(op, "deck").unwrap_or_else(|| "A".into());
    let d = deck.as_str();
    match kind.as_str() {
        "net" => {
            let pr = st.e.preparer(d).unwrap();
            let track = PreparedTrack::compose(&spec_of(op), &pr, 0);
            assert!(st.e.load_net(d, track));
        }
        "demo" => assert!(st.e.load_demo(d)),
        "play" => {
            st.e.set_playing(d, true);
        }
        "pause" => {
            st.e.set_playing(d, false);
        }
        "toTick" => {
            let want = f(op, "tick") as i64;
            let dn = d.to_string();
            st.until(40_000, |s| s.tick(&dn) >= want);
        }
        "setTempo" => {
            st.e.set_tempo(d, f(op, "bpm"));
        }
        "setPitch" => {
            st.e.set_pitch(d, f(op, "value"));
        }
        "generate" => {
            return JsonValue::Bool(st.e.generate_at(d, &spec_of(op), start_of(op)));
        }
        "swap" => {
            st.e.swap_to(d, &spec_of(op), start_of(op));
        }
        "stepSeed" => {
            return spec_text(st.e.step_seed(d, f(op, "delta") as i64).as_ref());
        }
        "queueNext" => st.e.queue_next(d, &spec_of(op)),
        "advance" => return JsonValue::Bool(st.e.advance_to_next(d)),
        "skip" => st.e.skip_to_next_track(d),
        "cancelNext" => st.e.cancel_next(d),
        "autoAdvance" => {
            st.e.deck_mut(d).unwrap().auto_advance =
                op.get("on").and_then(JsonValue::as_bool).unwrap();
        }
        "service" => st.e.service(),
        "lookAhead" => {
            let key = LookAheadKey {
                step: f(op, "step") as i64,
                spec: spec_of(op),
                start: start_of(op),
                role_filter: None,
            };
            assert!(st.e.request_look_ahead(d, key));
            st.e.wait_look_ahead(d);
        }
        "takeSwap" => {
            let spec = spec_of(op);
            let start = start_of(op);
            let key = LookAheadKey {
                step: f(op, "step") as i64,
                spec: spec.clone(),
                start,
                role_filter: None,
            };
            let Some(taken) = st.e.take_look_ahead(d, &key) else {
                return JsonValue::Bool(false);
            };
            st.e.swap_with_ahead(d, &spec, start, taken);
            st.e.wait_composes(d);
            return JsonValue::Bool(true);
        }
        "deterministic" => {
            st.e.deterministic = op.get("on").and_then(JsonValue::as_bool).unwrap();
        }
        "settle" => st.e.wait_composes(d),
        "relocateAsync" => {
            st.e.relocate(d, f(op, "tick") as i64, tail_of(op), rest_of(op));
            st.e.wait_composes(d);
        }
        "rawNets" => {
            let pr = st.e.preparer(d).unwrap();
            let track =
                PreparedTrack::from_raw_nets(op.get("payload").unwrap(), &pr).expect("nets");
            assert!(st.e.load_net(d, track));
        }
        "relocate" => {
            st.e.relocate(d, f(op, "tick") as i64, tail_of(op), rest_of(op));
        }
        "section" => {
            return JsonValue::Bool(st.e.jump_to_section(d, f(op, "index") as usize));
        }
        "mixOut" => {
            return st
                .e
                .mix_out(d, f(op, "bars") as i64)
                .map_or(JsonValue::Null, |t| {
                    JsonValue::Array(vec![num(t.from_tick), num(t.bars)])
                });
        }
        "mixIn" => {
            return st
                .e
                .mix_in(d, f(op, "bars") as i64)
                .map_or(JsonValue::Null, |r| {
                    JsonValue::Array(vec![num(r.from), num(r.to), num(r.mask.0 as i64)])
                });
        }
        "restart" => {
            let pr = st.e.preparer(d).unwrap();
            let t = PreparedTrack::compose(&spec_of(op), &pr, f(op, "startTick") as i64);
            let again = t.restarted(&pr, f(op, "at") as i64);
            let (a, b) = (again.prepared.wave(), t.prepared.wave());
            let flag = |v: bool| JsonValue::Bool(v);
            let mut o = JsonObject::new();
            o.set("firstStart", num(b.start_tick));
            o.set("start", num(a.start_tick));
            o.set("sameSong", flag(a.track_serial == b.track_serial));
            o.set("newGeneration", flag(a.generation != b.generation));
            o.set("sameSource", flag(again.source == t.source));
            o.set(
                "samePlayed",
                flag(again.played.to_json() == t.played.to_json()),
            );
            o.set("sameGain", flag(a.gain == b.gain));
            o.set("sameEnergy", flag(a.energy == b.energy));
            o.set("sameBalance", flag(a.balance == b.balance));
            o.set("sameRests", flag(a.rests == b.rests));
            o.set("sameSpace", flag(a.space == b.space));
            o.set("sameEcho", flag(a.echo == b.echo));
            o.set("sameDuck", flag(a.duck_depth == b.duck_depth));
            o.set(
                "pastEnd",
                num(t.restarted(&pr, 10_000_000).prepared.wave().start_tick),
            );
            return JsonValue::Object(o);
        }
        other => panic!("unknown op {other}"),
    }
    JsonValue::Null
}

/// The first place two JSON values differ, as a path and both sides.
fn first_diff(path: &str, want: &JsonValue, got: &JsonValue) -> Option<String> {
    match (want, got) {
        (JsonValue::Object(w), JsonValue::Object(g)) => {
            for (k, wv) in w.iter() {
                let gv = g.get(k).cloned().unwrap_or(JsonValue::Null);
                if let Some(d) = first_diff(&format!("{path}.{k}"), wv, &gv) {
                    return Some(d);
                }
            }
            for (k, _) in g.iter() {
                if w.get(k).is_none() {
                    return Some(format!("{path}.{k}: not in the golden"));
                }
            }
            None
        }
        _ if want.js_stringified() == got.js_stringified() => None,
        _ => Some(format!(
            "{path}: golden {} / rust {}",
            want.js_stringified(),
            got.js_stringified()
        )),
    }
}

/// One scenario against the golden: the first place Rust disagrees, if it does; and how many deck
/// states were compared.
fn replay(sc: &JsonValue, sr: f64) -> (Result<(), String>, usize) {
    let name = s(sc, "name");
    let bare = sc.get("bare").and_then(JsonValue::as_bool).unwrap();
    let mut st = Stepper::new(sr, !bare);
    let mut tr = Tracker::default();
    let mut compared = 0;
    for (n, step) in sc
        .get("steps")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .enumerate()
    {
        let op = step.get("op").unwrap();
        let result = run(&mut st, op);
        let ctx = format!("{name} #{n} {}", op.js_stringified());
        if let Some(d) = first_diff("result", step.get("result").unwrap(), &result) {
            return (Err(format!("{ctx}: {d}")), compared);
        }
        let decks = step.get("decks").unwrap();
        for deck in ["A", "B"] {
            let got = state_of(&mut st, &mut tr, deck, bare);
            if let Some(d) = first_diff(deck, decks.get(deck).unwrap(), &got) {
                return (Err(format!("{ctx}: {d}")), compared);
            }
            compared += 1;
        }
    }
    (Ok(()), compared)
}

#[test]
fn the_track_pipeline_matches_the_swift_engine() {
    let g = golden();
    let sr = f(&g, "sr");
    let scenarios = g.get("scenarios").and_then(JsonValue::as_array).unwrap();
    assert!(scenarios.len() >= 12, "{} scenarios", scenarios.len());
    let (mut compared, mut failures) = (0, Vec::new());
    for sc in scenarios {
        let (r, n) = replay(sc, sr);
        compared += n;
        if let Err(e) = r {
            failures.push(e);
        }
    }
    eprintln!(
        "swap_golden: {compared} deck states compared over {} scenarios",
        scenarios.len()
    );
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ from Swift:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
