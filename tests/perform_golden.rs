//! Swift goldens for `Engine::perform(DeckAction)` (`tools/paritydump/perform.swift`, section
//! "perform", `tests/fixtures/swift/perform.json`).
//!
//! The dump drives a real `DJEngine` at 22.05 kHz with two decks, started and stepped by
//! `OfflineConductor` (blocks of 128 frames, the housekeeping before the block that finds it
//! due), with a `SessionRecorder` attached: each scenario is a list of `DeckAction`s and renders
//! applied through `DJEngine.perform`. The same list is replayed here through
//! [`Engine::perform`] and [`common_track::Stepper`], and the state of both decks, the mixer and
//! the loop channel after *every* op is compared with what Swift read, then the recorded
//! session's canonical text and CID.
//!
//! Parity form (b) of the port rules. State, not samples: the AU strip (`AVAudioUnitEQ`, the AU
//! reverb) and the mixer nodes make engine audio goldens impossible (PORTING.md H). Everything
//! is exact: integers, flags, names and strings, and every float as its bit pattern.

mod common_fixture;
mod common_track;

use common_track::Stepper;
use djrust::control::controller::{DeckMode, DeckView};
use djrust::deck::beat::Quantize;
use djrust::fx::MasterFxKey;
use djrust::json::{JsonObject, JsonValue};
use djrust::session::action::DeckAction;
use djrust::host::deck::{Applied, PerformCtx};

const SR: f64 = 22_050.0;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("perform")
}

fn bits(v: f64) -> JsonValue {
    JsonValue::String(format!("{:016x}", v.to_bits()))
}

fn opt(v: Option<f64>) -> JsonValue {
    v.map_or(JsonValue::Null, bits)
}

fn st(v: &str) -> JsonValue {
    JsonValue::String(v.to_string())
}

fn n(v: i64) -> JsonValue {
    JsonValue::Number(v as f64)
}

fn sha(s: &str) -> JsonValue {
    JsonValue::String(djrust::cid::hex(&djrust::cid::sha256(s.as_bytes())))
}

fn arr(v: Vec<JsonValue>) -> JsonValue {
    JsonValue::Array(v)
}

fn obj(pairs: Vec<(&str, JsonValue)>) -> JsonValue {
    let mut o = JsonObject::new();
    for (k, v) in pairs {
        o.set(k, v);
    }
    JsonValue::Object(o)
}

fn spec_sha(spec: Option<&djrust::TrackSpec>) -> JsonValue {
    spec.map_or(JsonValue::Null, |s| sha(&s.payload().canonical_js()))
}

fn deck_state(st_: &Stepper, name: &str) -> JsonValue {
    let e = &st_.e;
    let d = e.deck(name).unwrap();
    let net = DeckView::mode(d) == DeckMode::Net;
    let sample = d.sample.as_ref();
    let mut cues: Vec<(i64, f64)> = d.hot_cues.iter().map(|(k, v)| (*k, *v)).collect();
    cues.sort_by_key(|c| c.0);
    let roles = e.roles(name);
    let mut stems: Vec<(String, Vec<String>)> = d
        .walk
        .stems_muted
        .groups
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    stems.sort();
    let mut disabled = d.disabled_roles.clone();
    disabled.sort();
    let voices: Vec<JsonValue> = d
        .voice_settings
        .iter()
        .map(|(ch, v)| {
            arr(vec![
                n(*ch),
                st(&v.tweak.to_json().canonical_js()),
                JsonValue::Bool(v.use_synth),
                st(&v.instrument),
            ])
        })
        .collect();
    let title = match (net, sample) {
        (true, _) => d
            .project
            .as_ref()
            .map_or("—".to_string(), |p| p.name.clone()),
        (false, Some(s)) => s.title.clone(),
        (false, None) => "—".to_string(),
    };
    obj(vec![
        ("mode", st(if net { "net" } else { "sample" })),
        ("playing", JsonValue::Bool(d.playing)),
        ("tick", n(d.mirror.tick())),
        (
            "tempo",
            // (Swift's `wave.tempo` is 0 until the net's first block has rendered; this mirror
            // publishes a project-less runner's 120 from the deck's first block of any kind)
            if !net {
                JsonValue::Null
            } else if d
                .mirror
                .has_project
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                bits(d.mirror.tempo())
            } else {
                bits(0.0)
            },
        ),
        ("effectiveBPM", opt(d.effective_bpm())),
        ("pitch", bits(d.pitch)),
        ("netNudge", bits(d.net_nudge)),
        (
            "quantize",
            st(match d.quantize {
                Quantize::Off => "off",
                Quantize::Beat => "beat",
                Quantize::Bar => "bar",
            }),
        ),
        (
            "hotCues",
            arr(cues
                .iter()
                .map(|(k, v)| arr(vec![n(*k), bits(*v)]))
                .collect()),
        ),
        ("cuePoint", bits(sample.map_or(0.0, |s| s.cue_point))),
        ("netCueTick", bits(d.net_cue_tick)),
        ("loopStart", opt(sample.and_then(|s| s.loop_start))),
        ("loopEnd", opt(sample.and_then(|s| s.loop_end))),
        (
            "looping",
            JsonValue::Bool(sample.is_some_and(|s| s.looping)),
        ),
        ("roles", arr(roles.iter().map(|r| st(r)).collect())),
        (
            "rolesOn",
            arr(roles
                .iter()
                .filter(|r| net && e.is_role_audible(name, r))
                .map(|r| st(r))
                .collect()),
        ),
        (
            "stemsMuted",
            arr(stems
                .iter()
                .map(|(k, v)| arr(vec![st(k), arr(v.iter().map(|r| st(r)).collect())]))
                .collect()),
        ),
        (
            "disabledRoles",
            arr(disabled.iter().map(|r| st(r)).collect()),
        ),
        (
            "mixInEnd",
            d.track.mix_in_end_tick.map_or(JsonValue::Null, n),
        ),
        ("mixingIn", JsonValue::Bool(e.is_mixing_in(name))),
        ("mixingOut", JsonValue::Bool(e.is_mixing_out(name))),
        (
            "leader",
            d.leader.as_ref().map_or(JsonValue::Null, |l| st(l)),
        ),
        ("autoAdvance", JsonValue::Bool(d.auto_advance)),
        ("advances", n(d.track.advances as i64)),
        ("nextReady", spec_sha(e.next_ready(name))),
        ("spec", spec_sha(d.spec.as_ref())),
        (
            "rawNets",
            d.track
                .raw_nets
                .as_ref()
                .map_or(JsonValue::Null, |p| sha(&p.canonical_js())),
        ),
        ("fxUnit", st(&d.fx_unit.json().canonical_js())),
        ("fxKilled", JsonValue::Bool(d.fx_unit.killed)),
        // the sliders last set by hand, in key order
        (
            "masterFx",
            arr(d
                .master_fx_set
                .iter()
                .map(|(k, v)| {
                    let key = MasterFxKey::from_index(*k as usize).unwrap();
                    arr(vec![st(key.id()), n(*v as i64)])
                })
                .collect()),
        ),
        ("kit", e.kit(name).map_or(JsonValue::Null, |k| st(&k))),
        (
            "palette",
            e.palette(name).map_or(JsonValue::Null, |k| st(&k)),
        ),
        ("voices", arr(voices)),
        ("volume", bits(d.volume)),
        ("gain", bits(d.gain)),
        ("filter", bits(d.strip.filter)),
        (
            "eq",
            arr(vec![
                bits(d.strip.eq_low),
                bits(d.strip.eq_mid),
                bits(d.strip.eq_high),
            ]),
        ),
        ("mute", JsonValue::Bool(d.mute)),
        ("autoGain", JsonValue::Bool(d.auto_gain)),
        (
            "reverb",
            arr(vec![
                bits(d.reverb_wet),
                bits(d.reverb_size),
                bits(d.reverb_tone),
            ]),
        ),
        ("title", st(&title)),
        ("isDemo", JsonValue::Bool(sample.is_some_and(|s| s.is_demo))),
        (
            "beatGrid",
            sample
                .and_then(|s| s.grid)
                .map_or(JsonValue::Null, |g| arr(vec![bits(g.bpm), bits(g.offset)])),
        ),
    ])
}

fn engine_state(st_: &Stepper) -> JsonValue {
    let e = &st_.e;
    let owners: Vec<(String, String)> = e
        .owners()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut locked = e.locked.clone();
    locked.sort();
    let l = &e.loop_channel;
    obj(vec![
        ("frame", n(e.frame())),
        ("crossfader", bits(e.crossfader)),
        // the main mixer keeps its volume as a Float
        ("master", bits(e.master as f32 as f64)),
        ("focus", st(&e.focus)),
        (
            "owners",
            arr(owners
                .iter()
                .map(|(k, v)| arr(vec![st(k), st(v)]))
                .collect()),
        ),
        ("locked", arr(locked.iter().map(|k| st(k)).collect())),
        (
            "loop",
            obj(vec![
                ("volume", bits(e.loops.volume)),
                ("gain", bits(e.loops.gain)),
                ("mute", JsonValue::Bool(e.loops.mute)),
                ("filter", bits(l.strip.filter)),
                (
                    "eq",
                    arr(vec![
                        bits(l.strip.eq_low),
                        bits(l.strip.eq_mid),
                        bits(l.strip.eq_high),
                    ]),
                ),
                (
                    "reverb",
                    arr(vec![
                        bits(l.reverb_wet),
                        bits(l.reverb_size),
                        bits(l.reverb_tone),
                    ]),
                ),
                ("fxUnit", st(&l.fx_unit.json().canonical_js())),
                // `Deck.currentParams` of the loop channel's `Deck`: the deck-only ops aimed at
                // L (quantize, autoAdvance, autoGain, roleDisable, the walk, the pitch) are in it
                (
                    "params",
                    st(&e.current_params("L").expect("L").json().canonical_js()),
                ),
                (
                    "masterFx",
                    arr(MasterFxKey::ALL
                        .iter()
                        .filter_map(|k| {
                            e.loop_master_slider_set(*k)
                                .map(|v| arr(vec![st(k.id()), n(v as i64)]))
                        })
                        .collect()),
                ),
            ]),
        ),
        // where a quantised FX unit move would land now (0 = at once), per deck's frames
        (
            "fxBeat",
            arr(["A", "B", "L"]
                .iter()
                .map(|d| n(e.fx_beat_frame(d)))
                .collect()),
        ),
    ])
}

fn state_of(st_: &Stepper, op: &str) -> JsonValue {
    obj(vec![
        ("op", st(op)),
        ("A", deck_state(st_, "A")),
        ("B", deck_state(st_, "B")),
        ("engine", engine_state(st_)),
    ])
}

/// Compare two states field by field, so a failure names the field.
fn compare(ctx: &str, want: &JsonValue, got: &JsonValue) -> Vec<String> {
    let mut diffs = Vec::new();
    match (want, got) {
        (JsonValue::Object(w), JsonValue::Object(g)) => {
            for (k, wv) in w.iter() {
                match g.get(k) {
                    Some(gv) => diffs.extend(compare(&format!("{ctx}.{k}"), wv, gv)),
                    None => diffs.push(format!("{ctx}: missing in Rust")),
                }
            }
        }
        _ => {
            if want.js_stringified() != got.js_stringified() {
                diffs.push(format!(
                    "{ctx}: swift {} rust {}",
                    want.js_stringified(),
                    got.js_stringified()
                ));
            }
        }
    }
    diffs
}

#[test]
fn every_scenario_matches_swift() {
    let g = golden();
    assert_eq!(g.get("sampleRate").and_then(JsonValue::as_f64), Some(SR));
    let scenarios = g.get("scenarios").and_then(JsonValue::as_array).unwrap();
    assert!(!scenarios.is_empty());
    let mut failures = Vec::new();
    let mut checked = 0usize;
    for sc in scenarios {
        let name = sc.get("name").and_then(JsonValue::as_str).unwrap();
        let ops = sc.get("ops").and_then(JsonValue::as_array).unwrap();
        let states = sc.get("states").and_then(JsonValue::as_array).unwrap();
        let mut stp = Stepper::new(SR, true);
        stp.e.start_session().expect("the empty engine is at rest");
        let mut diffs = compare(
            &format!("{name}[start]"),
            &states[0],
            &state_of(&stp, "start"),
        );
        for (i, op) in ops.iter().enumerate() {
            let kind = op.get("op").and_then(JsonValue::as_str).unwrap();
            match kind {
                "act" => {
                    let a = DeckAction::from_json(op.get("a").unwrap()).expect("decodes");
                    stp.e.perform(&a, PerformCtx::default());
                }
                "render" => {
                    stp.step(op.get("blocks").and_then(JsonValue::as_f64).unwrap() as usize)
                }
                "until" => {
                    let deck = op
                        .get("deck")
                        .and_then(JsonValue::as_str)
                        .unwrap()
                        .to_string();
                    for _ in 0..op.get("max").and_then(JsonValue::as_f64).unwrap() as usize {
                        stp.block();
                        let ended = stp
                            .e
                            .deck(&deck)
                            .unwrap()
                            .mirror
                            .stopped
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if ended {
                            break;
                        }
                    }
                }
                other => panic!("unknown op {other}"),
            }
            let label = format!("{name}[{}:{}]", i + 1, kind);
            let d = compare(&label, &states[i + 1], &state_of(&stp, kind));
            if !d.is_empty() && diffs.len() < 30 {
                // after the first divergence the rest is noise: keep the first op's
                diffs.extend(d);
            }
            checked += 1;
        }
        let session = stp.e.finish_session().expect("recording");
        let want_text = sc.get("session").and_then(JsonValue::as_str).unwrap();
        if session.canonical() != want_text {
            diffs.push(format!(
                "{name}: the recorded session differs:\n swift {want_text}\n rust  {}",
                session.canonical()
            ));
        }
        let want_cid = sc.get("cid").and_then(JsonValue::as_str).unwrap();
        if session.cid() != want_cid {
            diffs.push(format!(
                "{name}: cid swift {want_cid} rust {}",
                session.cid()
            ));
        }
        assert_eq!(
            session.frames,
            sc.get("frames").and_then(JsonValue::as_f64).unwrap() as i64,
            "{name}: frames"
        );
        failures.extend(diffs);
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} op states differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 100, "{checked}");
}

/// E9: `dj["L"]` is the loop channel's own `Deck`, so the deck-only ops aimed at it set fields
/// that `Deck.currentParams` reads back, and a journal checkpoint's `loopChannel` carries them
/// (`JournalState.swift`: `loopChannel = dj.loopChannel.currentParams`). The scenario
/// `loop_channel_deck_ops` of the golden above holds the values against Swift's; this holds the
/// checkpoint text, and that a fresh engine's L is the default.
#[test]
fn a_checkpoint_carries_the_deck_only_ops_aimed_at_the_loop_channel() {
    use djrust::session::model::DeckParams;
    let mut stp = Stepper::new(SR, true);
    let e = &mut stp.e;
    assert_eq!(e.journal_state(Default::default(), false).loop_channel.quantize, "off");
    let p = |e: &mut djrust::host::deck::Engine, a: DeckAction| e.perform(&a, PerformCtx::default());
    let l = || "L".to_string();
    p(e, DeckAction::Quantize { deck: l(), mode: "bar".into() });
    p(e, DeckAction::AutoAdvance { deck: l(), on: true });
    p(e, DeckAction::Param { deck: l(), name: "autoGain".into(), value: 1.0 });
    p(e, DeckAction::RoleDisable { deck: l(), role: "drums".into(), on: true });
    p(e, DeckAction::RoleDisable { deck: l(), role: "bass".into(), on: true });
    p(e, DeckAction::RoleDisable { deck: l(), role: "drums".into(), on: false });
    p(e, DeckAction::Wander { deck: l(), on: true });
    p(e, DeckAction::WanderPace { deck: l(), bars: 8 });
    p(e, DeckAction::WanderLock { deck: l(), dimension: "keep:bass".into(), on: true });
    let cp = e.journal_state(Default::default(), false).loop_channel;
    assert_eq!(cp.quantize, "bar");
    assert!(cp.auto_advance && cp.auto_gain);
    assert_eq!(cp.disabled_roles, ["bass"]);
    assert!(cp.wander.on && cp.wander.pace == 8 && cp.wander.is_locked("keep:bass"));
    // and it is in the text, which round-trips
    let text = cp.json().canonical_js();
    for key in ["\"quantize\":\"bar\"", "\"autoAdvance\":true", "\"autoGain\":true", "\"disabledRoles\":[\"bass\"]", "\"wander\":"] {
        assert!(text.contains(key), "{key} in {text}");
    }
    assert_eq!(DeckParams::from_json(&cp.json()), cp);
}

/// Applied reports what an op did: a loop in on a net deck falls
/// through, the walk's controls and auto-mix's run (the deck's own `set_wander`, the engine's
/// `set_auto_mix`...) and none is counted as unhandled.
#[test]
fn applied_says_what_each_op_did() {
    let mut stp = Stepper::new(SR, true);
    let e = &mut stp.e;
    let p = |e: &mut djrust::host::deck::Engine, a: DeckAction| e.perform(&a, PerformCtx::default());
    assert_eq!(
        p(
            e,
            DeckAction::Param {
                deck: "A".into(),
                name: "reverbWet".into(),
                value: 0.4
            }
        ),
        Applied::Done
    );
    assert_eq!(
        p(
            e,
            DeckAction::Param {
                deck: "A".into(),
                name: "volume".into(),
                value: 0.4
            }
        ),
        Applied::Done
    );
    assert!(matches!(
        p(
            e,
            DeckAction::Loop {
                deck: "A".into(),
                op: "in".into()
            }
        ),
        Applied::Done
    ));
    assert!(matches!(
        p(
            e,
            DeckAction::Play {
                deck: "L".into(),
                on: true
            }
        ),
        Applied::Ignored(_)
    ));
    assert!(matches!(
        p(e, DeckAction::Cue { deck: "Z".into() }),
        Applied::Ignored(_)
    ));
    assert!(e.unhandled.is_empty());
    // the walk is the deck's (`Deck.setWander`, `setWanderPace`, `setWanderLock`)
    assert_eq!(
        p(
            e,
            DeckAction::Wander {
                deck: "A".into(),
                on: true
            }
        ),
        Applied::Done
    );
    assert_eq!(
        p(
            e,
            DeckAction::WanderPace {
                deck: "A".into(),
                bars: 8
            }
        ),
        Applied::Done
    );
    assert_eq!(
        p(
            e,
            DeckAction::WanderLock {
                deck: "A".into(),
                dimension: "tempo".into(),
                on: true
            }
        ),
        Applied::Done
    );
    let w = &e.deck("A").unwrap().walk.wander;
    assert!(w.on && w.pace == 8 && w.is_locked("tempo"));
    // the loop channel's `Deck` takes the deck-only ops that set a field its `currentParams`
    // reads back (E9): the walk is settings there, and a checkpoint's loopChannel carries it
    assert_eq!(
        p(
            e,
            DeckAction::Wander {
                deck: "L".into(),
                on: true
            }
        ),
        Applied::Done
    );
    assert!(e.loop_channel.wander.on);
    assert_eq!(
        p(
            e,
            DeckAction::Quantize {
                deck: "L".into(),
                mode: "bar".into()
            }
        ),
        Applied::Done
    );
    assert_eq!(e.current_params("L").unwrap().quantize, "bar");
    assert!(e.current_params("L").unwrap().wander.on);
    // auto-mix is on the decks (`setAutoMix`, `autoMixNow`, `deferAutoMix`): each runs, and
    // nothing is left for `unhandled`. Not a Swift golden: what the engine does with them is
    // `automixdeck_golden`'s.
    assert_eq!(p(e, DeckAction::AutoMixNow), Applied::Done);
    assert!(!e.auto_mix.now_requested, "off: mix now asks for nothing");
    assert_eq!(
        p(
            e,
            DeckAction::AutoMix {
                on: true,
                bars: 16,
                every: 4
            }
        ),
        Applied::Done
    );
    assert_eq!(
        e.auto_mix.settings,
        djrust::automix::AutoMixSettings::new(true, 16, 4)
    );
    assert_eq!(p(e, DeckAction::AutoMixNow), Applied::Done);
    assert!(e.auto_mix.now_requested);
    // no hand-off is running, so there is nothing to defer
    assert_eq!(
        p(e, DeckAction::AutoMixDefer { handoff: 0 }),
        Applied::Done
    );
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
}
