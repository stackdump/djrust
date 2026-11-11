//! Swift goldens for auto-mix on decks (`AutoMixEngine.swift`; `tools/paritydump/automixdeck.swift`,
//! section "automixdeck", `tests/fixtures/swift/automixdeck.json`).
//!
//! The dump drives a real `DJEngine(offline: true)` at 22.05 kHz, two decks, `deterministic` on:
//! blocks of 128 frames, the housekeeping (each deck's `service()`, `serviceWander()`,
//! `serviceAutoMix()`) before the block that finds it due. Each scenario is a list of ops; the
//! ops are replayed here through [`Engine`] and [`Renderer`] the same way
//! (`common_track::Stepper`, whose housekeeping is `Engine::service`, which ends in
//! `service_auto_mix`), and the *rows* are compared: one after every op, one after every block
//! in which anything auto-mix or either deck shows changed, and one every 512th block. A row
//! holds auto-mix's state (the fields of `AutoMixState` and `AutoMixRun`, the checkpoint's hash,
//! `dj_state.autoMix` and the status line), the crossfader's bits and its owner, and both decks:
//! transport, tick, tempo bits, effective BPM bits, tempo fader, leader, holds, the spec, the
//! start tick and tail of what they loaded last.
//!
//! Parity form (b) of the port rules, compared exactly: this is control logic over an integer
//! clock, no audio is compared. The rows land where the Swift engine's did, tick for tick and
//! frame for frame: the prep point, the compose landing, the mix-out point, the start (the
//! align in ticks, as the tempo bits show), the correction, each crossfader step and the
//! finish.
//!
//! The pokes (`play`, `autoMix`, `autoMixNow`, `crossfader`, `hotCue`, `load`) go through
//! `Engine::perform`, as the dump's go through `DJEngine.perform`: so the golden also holds the
//! applier's wiring (the touch noted before the action, the three auto-mix ops). The recorded
//! session's text is not compared (the dump does not write it); what the engine decides to
//! record by itself (the defer) is in `AutoMixHost::recorded_defers`.

mod common_fixture;
mod common_track;

use common_track::Stepper;
use std::collections::BTreeSet;

use djrust::automix::{AutoMixRun, AutoMixState};
use djrust::cid::{hex, sha256, share_cid};
use djrust::control::controller::SurfaceHost;
use djrust::deck::beat_grid::BeatGrid;
use djrust::deck::turntable::SampleBuffer;
use djrust::json::{JsonObject, JsonValue};
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::wave::rests::MixOutTail;
use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{PerformCtx, PreparedTrack};

fn golden() -> JsonValue {
    common_fixture::swift_fixture("automixdeck")
}

fn bits(v: f64) -> JsonValue {
    JsonValue::String(format!("{:016x}", v.to_bits()))
}
fn ob(v: Option<f64>) -> JsonValue {
    v.map_or(JsonValue::Null, bits)
}
fn num(v: i64) -> JsonValue {
    JsonValue::Number(v as f64)
}
fn oi(v: Option<i64>) -> JsonValue {
    v.map_or(JsonValue::Null, num)
}
fn os(v: Option<&str>) -> JsonValue {
    v.map_or(JsonValue::Null, |s| JsonValue::String(s.into()))
}
fn text(v: &str) -> JsonValue {
    JsonValue::String(v.into())
}
fn flag(v: bool) -> JsonValue {
    JsonValue::Bool(v)
}
fn sha(s: &str) -> JsonValue {
    JsonValue::String(hex(&sha256(s.as_bytes())))
}

fn obj(pairs: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(pairs))
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

fn spec_of(op: &JsonValue) -> TrackSpec {
    TrackSpec::new(
        &s(op, "genre"),
        f(op, "seed"),
        op.get("structure").and_then(JsonValue::as_str),
    )
}

fn entries_mask(masks: &[i64], at: &[i64]) -> JsonValue {
    let _ = at;
    JsonValue::Array(masks.iter().map(|m| num(*m)).collect())
}

fn deck_state(st: &Stepper, name: &str, ticks: bool) -> JsonValue {
    let d = st.e.deck(name).unwrap();
    let net = d.is_net();
    let mut latest = JsonValue::Null;
    if let Some(l) = &d.track.latest {
        let at = [0i64, 16, 32];
        latest = obj(vec![
            ("startTick", num(l.start_tick)),
            (
                "tail",
                l.tail.map_or(JsonValue::Null, |t| {
                    JsonValue::Array(vec![num(t.from_tick), num(t.bars)])
                }),
            ),
            (
                "restMasks",
                entries_mask(&at.map(|t| l.rests.mask(t).0 as i64), &at),
            ),
            (
                "songRestMasks",
                entries_mask(&at.map(|t| l.song_rests.mask(t).0 as i64), &at),
            ),
        ]);
    }
    let installed = d.track.latest.as_ref().is_none_or(|l| {
        d.mirror
            .generation
            .load(std::sync::atomic::Ordering::Relaxed)
            == l.generation
    });
    let mut v = vec![
        ("mode", text(if net { "net" } else { "sample" })),
        ("playing", flag(d.playing)),
        ("wavePlaying", flag(net && d.mirror.is_playing())),
        ("started", flag(d.mirror.tick() > 0)),
        // (the runner's tempo means something once it holds the deck's project: before
        // that it is the idle default, 0 in Swift and 120 here)
        (
            "tempo",
            if net && installed && d.track.latest.is_some() {
                bits(d.mirror.tempo())
            } else {
                JsonValue::Null
            },
        ),
        ("effectiveBPM", ob(d.effective_bpm())),
        ("pitch", bits(d.pitch)),
        ("leader", os(d.leader.as_deref())),
        ("wanderHeld", flag(d.walk.held)),
        ("autoMixingOut", flag(d.track.auto_mixing_out)),
        (
            "spec",
            d.spec.as_ref().map_or(JsonValue::Null, |s| {
                JsonValue::String(share_cid(&s.payload()))
            }),
        ),
        ("isDemo", flag(d.sample.as_ref().is_some_and(|s| s.is_demo))),
        ("latest", latest),
        ("installed", flag(installed)),
        ("mixInEndTick", oi(d.track.mix_in_end_tick)),
        ("disabledRoles", {
            let mut r = d.disabled_roles.clone();
            r.sort();
            JsonValue::Array(r.into_iter().map(JsonValue::String).collect())
        }),
        ("autoAdvance", flag(d.auto_advance)),
    ];
    if ticks {
        v.push(("tick", num(d.mirror.tick())));
        v.push(("tickPhase", bits(d.mirror.tick_phase())));
    }
    obj(v)
}

fn run_state(r: Option<&AutoMixRun>) -> JsonValue {
    let Some(r) = r else { return JsonValue::Null };
    obj(vec![
        (
            "plan",
            JsonValue::String(format!("{}:{}", r.plan.n, r.plan.kind)),
        ),
        ("out", text(&r.out)),
        ("in", text(&r.inc)),
        ("handoffBar", num(r.handoff_bar)),
        ("prep", num(r.prep)),
        ("bars", num(r.bars)),
        ("outIsNet", flag(r.out_is_net)),
        ("ready", flag(r.incoming_ready)),
        ("mixOutTick", oi(r.mix_out_tick)),
        ("outHandoffTick", oi(r.out_handoff_tick)),
        ("mixOutLoaded", flag(r.mix_out_loaded)),
        ("armed", flag(r.armed)),
        ("corrected", flag(r.corrected)),
        ("c0", ob(r.c0)),
        ("xEnd", bits(r.x_end)),
        ("faderTaken", flag(r.fader_taken)),
        ("lastFader", ob(r.last_fader)),
        ("prevLeader", os(r.prev_leader.as_deref())),
        ("mult", bits(r.mult)),
    ])
}

fn state(st: &Stepper, ticks: bool, full: bool) -> JsonValue {
    let e = &st.e;
    let am = &e.auto_mix;
    let mut a = vec![
        ("on", flag(am.settings.on)),
        ("bars", num(am.settings.bars)),
        ("every", num(am.settings.every)),
        ("phase", text(am.phase.raw())),
        ("idle", os(am.idle.as_deref())),
        ("handoffs", num(am.handoffs)),
        ("playing", os(am.playing.as_deref())),
        ("paused", flag(am.paused)),
        ("quietBars", num(am.quiet_bars)),
        ("startBar", oi(am.start_bar)),
        ("handoffBar", oi(am.handoff_bar)),
        ("nowRequested", flag(am.now_requested)),
        ("cued", os(am.cued.as_deref())),
        (
            "keptPlan",
            am.kept.as_ref().map_or(JsonValue::Null, |k| {
                JsonValue::String(format!("{}:{}", k.n, k.kind))
            }),
        ),
        ("run", run_state(am.run.as_ref())),
        ("stateHash", sha(&am.checkpoint_json().canonical_js())),
        (
            "logRows",
            JsonValue::Array(
                am.log
                    .iter()
                    .map(|h| {
                        JsonValue::String(format!(
                            "{} {}>{} bar {} {} {}",
                            h.n,
                            h.from,
                            h.to,
                            h.bar,
                            h.kind,
                            h.cid().as_deref().unwrap_or("-")
                        ))
                    })
                    .collect(),
            ),
        ),
    ];
    if ticks {
        a.push(("lastBar", oi(am.last_bar)));
        // the whole text on the rows after an op, its hash on the rest
        let json = e.auto_mix_json().canonical_js();
        a.push(if full {
            ("json", text(&json))
        } else {
            ("jsonHash", sha(&json))
        });
        a.push(("status", text(&e.auto_mix_status())));
    }
    obj(vec![
        ("auto", obj(a)),
        ("crossfader", bits(e.crossfader)),
        ("owner", os(SurfaceHost::owner(e, "crossfader").as_deref())),
        (
            "decks",
            obj(vec![
                ("A", deck_state(st, "A", ticks)),
                ("B", deck_state(st, "B", ticks)),
            ]),
        ),
    ])
}

/// What a row is emitted for a change in (the dump's `Digest`): auto-mix's whole state, the
/// crossfader, and each deck's transport, tempo, leader, holds, track and tail.
#[derive(PartialEq)]
struct DeckDigest {
    net: bool,
    playing: bool,
    wave_playing: bool,
    started: bool,
    tempo: u64,
    effective: Option<u64>,
    pitch: u64,
    leader: Option<String>,
    held: bool,
    mixing_out: bool,
    spec: Option<TrackSpec>,
    is_demo: bool,
    start_tick: Option<i64>,
    tail: Option<MixOutTail>,
    installed: bool,
    mix_in_end_tick: Option<i64>,
    disabled: BTreeSet<String>,
    auto_advance: bool,
}

impl DeckDigest {
    fn of(st: &Stepper, name: &str) -> DeckDigest {
        let d = st.e.deck(name).unwrap();
        let net = d.is_net();
        let installed = d.track.latest.as_ref().is_none_or(|l| {
            d.mirror
                .generation
                .load(std::sync::atomic::Ordering::Relaxed)
                == l.generation
        });
        DeckDigest {
            net,
            playing: d.playing,
            wave_playing: net && d.mirror.is_playing(),
            started: d.mirror.tick() > 0,
            tempo: if net && d.track.latest.is_some() && installed {
                d.mirror.tempo().to_bits()
            } else {
                0
            },
            effective: d.effective_bpm().map(f64::to_bits),
            pitch: d.pitch.to_bits(),
            leader: d.leader.clone(),
            held: d.walk.held,
            mixing_out: d.track.auto_mixing_out,
            spec: d.spec.clone(),
            is_demo: !net && d.sample.as_ref().is_some_and(|s| s.is_demo),
            start_tick: d.track.latest.as_ref().map(|l| l.start_tick),
            tail: d.track.latest.as_ref().and_then(|l| l.tail),
            installed,
            mix_in_end_tick: d.track.mix_in_end_tick,
            disabled: d.disabled_roles.iter().cloned().collect(),
            auto_advance: d.auto_advance,
        }
    }
}

#[derive(PartialEq)]
struct Digest {
    auto: AutoMixState,
    crossfader: u64,
    owner: Option<String>,
    a: DeckDigest,
    b: DeckDigest,
}

fn digest(st: &Stepper) -> Digest {
    // (the bar bookkeeping moves every bar: a row has it, a change in it alone is not worth one)
    let mut auto = st.e.auto_mix.clone();
    auto.last_bar = None;
    Digest {
        auto,
        crossfader: st.e.crossfader.to_bits(),
        owner: SurfaceHost::owner(&st.e, "crossfader"),
        a: DeckDigest::of(st, "A"),
        b: DeckDigest::of(st, "B"),
    }
}

struct Rig {
    st: Stepper,
    rows: Vec<JsonValue>,
    last_digest: Option<Digest>,
    steps: i64,
}

impl Rig {
    fn new(sr: f64) -> Rig {
        Rig {
            st: Stepper::new(sr, true),
            rows: Vec::new(),
            last_digest: None,
            steps: 0,
        }
    }

    fn step(&mut self) {
        self.st.block();
        self.steps += 1;
        self.probe("tick", false);
    }

    fn probe(&mut self, at: &str, force: bool) {
        let d = digest(&self.st);
        if force || self.last_digest.as_ref() != Some(&d) || self.steps % 1024 == 0 {
            self.last_digest = Some(d);
            self.rows.push(obj(vec![
                ("at", text(at)),
                ("frame", num(self.st.frame)),
                ("state", state(&self.st, true, force)),
            ]));
        }
    }
}

fn cond(rig: &Rig, name: &str) -> bool {
    let am = &rig.st.e.auto_mix;
    match name {
        "run" => am.run.is_some(),
        "noRun" => am.run.is_none(),
        "mixing" => am.phase == djrust::automix::AutoMixPhase::Mixing && rig.st.tick("B") > 20,
        "resumed" => !am.paused,
        "log1" => !am.log.is_empty(),
        "log2" => am.log.len() >= 2,
        "armed" => am.run.as_ref().is_some_and(|r| r.armed),
        "mixOutLoaded" => am.run.as_ref().is_some_and(|r| r.mix_out_loaded),
        other => panic!("unknown condition {other}"),
    }
}

/// One op (`run` in the dump); its result as the dump writes it.
fn run(rig: &mut Rig, op: &JsonValue) -> JsonValue {
    let kind = s(op, "op");
    let name = op
        .get("deck")
        .and_then(JsonValue::as_str)
        .unwrap_or("A")
        .to_string();
    let d = name.as_str();
    let sr = rig.st.sr;
    match kind.as_str() {
        "render" => {
            for _ in 0..f(op, "blocks") as usize {
                rig.step();
            }
        }
        "toTick" => {
            let want = f(op, "tick") as i64;
            let mut k = 0;
            while rig.st.tick(d) < want && k < 40_000 {
                rig.step();
                k += 1;
            }
            return flag(rig.st.tick(d) >= want);
        }
        "until" => {
            let limit = op
                .get("limit")
                .and_then(JsonValue::as_f64)
                .unwrap_or(40_000.0) as usize;
            let c = s(op, "cond");
            let mut k = 0;
            while !cond(rig, &c) && k < limit {
                rig.step();
                k += 1;
            }
            return flag(cond(rig, &c));
        }
        "net" => {
            let pr = rig.st.e.preparer(d).unwrap();
            let track = PreparedTrack::compose(&spec_of(op), &pr, 0);
            assert!(rig.st.e.load_net(d, track));
        }
        "demo" => assert!(rig.st.e.load_demo(d)),
        "grid" => {
            assert!(rig
                .st
                .e
                .set_beat_grid(d, BeatGrid::new(f(op, "bpm"), 0.0, 1.0)));
        }
        "sample" => {
            let frames = f(op, "frames") as usize;
            let b = SampleBuffer::new(vec![0.0; frames], None, sr);
            assert!(rig.st.e.load_sample(d, PreparedSample::new(b, "x")));
        }
        "setPlaying" => {
            rig.st
                .e
                .set_playing(d, op.get("on").and_then(JsonValue::as_bool).unwrap());
        }
        "setCrossfader" => rig.st.e.set_crossfader(f(op, "v")),
        "pitch" => {
            rig.st.e.set_pitch(d, f(op, "value"));
        }
        "autoAdvance" => {
            rig.st.e.deck_mut(d).unwrap().auto_advance =
                op.get("on").and_then(JsonValue::as_bool).unwrap();
        }
        "disableRole" => rig.st.e.set_role_disabled(d, &s(op, "role"), true),
        "lock" => rig.st.e.set_wander_lock(d, &s(op, "dim"), true),
        "armDelay" => rig.st.e.auto_mix_host.arm_delay = f(op, "n") as i64,
        "crossfadeOnly" => {
            rig.st.e.auto_mix_host.crossfade_only =
                op.get("on").and_then(JsonValue::as_bool).unwrap();
        }
        "relocate" => {
            rig.st.e.relocate(d, f(op, "tick") as i64, None, None);
        }
        "serviceAutoMix" => rig.st.e.service_auto_mix(),
        // `DJEngine.perform`: `noteAutoMixTouch` first, then the call, as the dump does
        "play" => perform(
            rig,
            DeckAction::Play {
                deck: name.clone(),
                on: op.get("on").and_then(JsonValue::as_bool).unwrap(),
            },
        ),
        "autoMix" => perform(
            rig,
            DeckAction::AutoMix {
                on: op.get("on").and_then(JsonValue::as_bool).unwrap(),
                bars: f(op, "bars") as i64,
                every: f(op, "every") as i64,
            },
        ),
        "autoMixNow" => perform(rig, DeckAction::AutoMixNow),
        "crossfader" => perform(rig, DeckAction::Crossfader(f(op, "v"))),
        "hotCue" => perform(
            rig,
            DeckAction::HotCue {
                deck: name.clone(),
                slot: f(op, "slot") as i64,
            },
        ),
        "load" => perform(
            rig,
            DeckAction::Load {
                deck: name.clone(),
                source: DeckSource::Net(spec_of(op)),
            },
        ),
        other => panic!("unknown op {other}"),
    }
    JsonValue::Null
}

/// `DJEngine.perform(action)` on the Rust engine: the one applier, which the dump's pokes go
/// through too.
fn perform(rig: &mut Rig, a: DeckAction) {
    rig.st.e.perform(&a, PerformCtx::default());
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

/// One scenario against the golden: the first place Rust disagrees, if it does; and how many
/// rows were compared.
fn replay(sc: &JsonValue, sr: f64) -> (Result<(), String>, usize) {
    let name = s(sc, "name");
    let mut rig = Rig::new(sr);
    rig.probe("setup", true);
    let ops = sc.get("ops").and_then(JsonValue::as_array).unwrap();
    let results = sc.get("results").and_then(JsonValue::as_array).unwrap();
    for (k, op) in ops.iter().enumerate() {
        let got = run(&mut rig, op);
        if let Some(d) = first_diff("result", &results[k], &got) {
            return (Err(format!("{name} #{k} {}: {d}", op.js_stringified())), 0);
        }
        rig.probe(&format!("op:{k}"), true);
    }
    let want = sc.get("rows").and_then(JsonValue::as_array).unwrap();
    for (k, (w, g)) in want.iter().zip(&rig.rows).enumerate() {
        if let Some(d) = first_diff("row", w, g) {
            return (
                Err(format!(
                    "{name}: row {k} (golden at {} frame {}): {d}",
                    s(w, "at"),
                    f(w, "frame")
                )),
                k,
            );
        }
    }
    if want.len() != rig.rows.len() {
        return (
            Err(format!(
                "{name}: {} rows in the golden, {} here",
                want.len(),
                rig.rows.len()
            )),
            want.len().min(rig.rows.len()),
        );
    }
    (Ok(()), want.len())
}

#[test]
fn auto_mix_on_decks_matches_the_swift_engine() {
    let g = golden();
    let sr = f(&g, "sr");
    let scenarios = g.get("scenarios").and_then(JsonValue::as_array).unwrap();
    assert!(scenarios.len() >= 15, "{} scenarios", scenarios.len());
    let (mut compared, mut failures) = (0, Vec::new());
    for sc in scenarios {
        let (r, n) = replay(sc, sr);
        compared += n;
        if let Err(e) = r {
            failures.push(e);
        }
    }
    eprintln!(
        "automixdeck_golden: {compared} rows compared over {} scenarios",
        scenarios.len()
    );
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ from Swift:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
