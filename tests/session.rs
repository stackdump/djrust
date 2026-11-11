//! The session model and what is built on it: `SessionModelTests`,
//! `SessionRecorderGuardTests` (the part that is a pure rule), `SessionMixTests`,
//! `SessionPublishTests` and the local-only / imported-clip refusals from `PaletteTests`,
//! `MinecraftTests` and `LoopRecordTests`.
//!
//! Asserts marked "Swift literal" are the Swift suite's own; the replay-audio tests of
//! `SessionReplayTests` need the engine and are not portable here. There is no Swift on this
//! box, so no CID below is a Swift value except `z4EBG9…` (the CIDv1 dag-json sha2-256 prefix
//! every share CID has).

use std::collections::BTreeMap;

use djrust::cid::share_cid;
use djrust::deck::tempo_match;
use djrust::json::{JsonObject, JsonValue};
use djrust::session::action::{voices_json, JSON_OPS};
use djrust::session::mix::{report, MixReport, MixTrack, SessionMix, SessionMixError};
use djrust::session::model::{sample_content_id, SCHEMA};
use djrust::session::player::{Conductor, SessionPlayer};
use djrust::session::publish::{
    publish_session, BundleFile, CdnTransport, LocalOnly, PublicProbe, PublishError, PublishStep,
    SessionBundle,
};
use djrust::session::recorder::{source_descriptor, DeckSnapshot, SessionRecorder};
use djrust::session::source::ShareLoadError;
use djrust::session::types::{
    AutoMixSettings, AutoMixState, BeatGrid, FxParam, FxUnitSettings, LoopFx, LoopFxMode,
    SceneClip, SequenceStep, VoiceTweak, WanderKey, WanderState,
};
use djrust::session::wav::{encode_wav, RenderedAudio};
use djrust::session::{
    DeckAction, DeckParams, DeckSource, Session, SessionError, SessionEvent, SessionEventKind,
};
use djrust::spec::TrackSpec;

fn techno_spec() -> TrackSpec {
    let mut s = TrackSpec::new("techno", 42.0, Some("standard"));
    s.cohesion = "v2".into();
    s.extra = JsonObject::from_pairs([("bpm", JsonValue::Number(126.0))]);
    s
}

fn act(frame: i64, a: DeckAction) -> SessionEvent {
    SessionEvent::action(frame, a)
}

/// `SessionModelTests.sample()`.
fn sample() -> Session {
    let spec = techno_spec();
    let mut p_a = DeckParams::default();
    p_a.volume = 0.7;
    p_a.auto_advance = true;
    let mut decks = BTreeMap::new();
    decks.insert("A".to_string(), DeckSource::Net(spec.clone()));
    decks.insert(
        "B".to_string(),
        DeckSource::Sample {
            id: "abc123".into(),
            title: "Loop <1> & more".into(),
            grid: Some(BeatGrid::new(127.5, 312.25, 1.0)),
        },
    );
    decks.insert("C".to_string(), DeckSource::None);
    let mut params = BTreeMap::new();
    params.insert("A".to_string(), p_a);
    params.insert("B".to_string(), DeckParams::default());
    params.insert("C".to_string(), DeckParams::default());
    let d = |s: &str| s.to_string();
    let events = vec![
        SessionEvent::midi(0, "APC mini mk2 Control", &[0x90, 16, 0x7F]),
        act(
            128,
            DeckAction::Play {
                deck: d("A"),
                on: true,
            },
        ),
        act(
            256,
            DeckAction::Param {
                deck: d("B"),
                name: d("filter"),
                value: 0.31,
            },
        ),
        act(300, DeckAction::Crossfader(0.5)),
        act(301, DeckAction::Master(0.8)),
        act(
            400,
            DeckAction::Load {
                deck: d("A"),
                source: DeckSource::Net(spec.clone()),
            },
        ),
        act(
            401,
            DeckAction::QueueNext {
                deck: d("B"),
                spec: spec.clone(),
            },
        ),
        act(
            500,
            DeckAction::HotCue {
                deck: d("B"),
                slot: 2,
            },
        ),
        act(
            501,
            DeckAction::ClearCue {
                deck: d("B"),
                slot: 2,
            },
        ),
        act(502, DeckAction::Cue { deck: d("B") }),
        act(
            510,
            DeckAction::Loop {
                deck: d("B"),
                op: d("halve"),
            },
        ),
        act(
            600,
            DeckAction::Section {
                deck: d("A"),
                index: 3,
            },
        ),
        act(
            601,
            DeckAction::Role {
                deck: d("A"),
                role: d("bass"),
                on: false,
            },
        ),
        act(
            602,
            DeckAction::Macro {
                deck: d("A"),
                id: d("drop"),
            },
        ),
        act(603, DeckAction::Panic { deck: d("A") }),
        act(604, DeckAction::Stinger { deck: d("A"), n: 2 }),
        act(
            700,
            DeckAction::Sync {
                deck: d("B"),
                align: d("bar"),
            },
        ),
        act(
            701,
            DeckAction::Quantize {
                deck: d("B"),
                mode: d("beat"),
            },
        ),
        act(
            702,
            DeckAction::Follow {
                deck: d("B"),
                leader: Some(d("A")),
            },
        ),
        act(
            703,
            DeckAction::Follow {
                deck: d("B"),
                leader: None,
            },
        ),
        act(
            704,
            DeckAction::AutoAdvance {
                deck: d("A"),
                on: false,
            },
        ),
        act(705, DeckAction::NextTrack { deck: d("A") }),
        act(
            706,
            DeckAction::Grid {
                deck: d("B"),
                bpm: 128.0,
                offset: 10.0,
            },
        ),
        act(
            800,
            DeckAction::Load {
                deck: d("B"),
                source: DeckSource::Sample {
                    id: d("demo-loop"),
                    title: d("demo"),
                    grid: None,
                },
            },
        ),
    ];
    Session::new(48_000.0, 96_000, decks, params, 0.25, 0.9, events)
}

// SessionModelTests.jsonRoundTripsEveryEventKind
#[test]
fn json_round_trips_every_event_kind() {
    let s = sample();
    let back = Session::from_json(&JsonValue::parse(&s.json().js_stringified()).unwrap()).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.cid(), s.cid());
    // The decks' sources are plain beats share payloads.
    let json = s.json();
    let a = json
        .get("decks")
        .unwrap()
        .get("A")
        .unwrap()
        .get("source")
        .unwrap();
    let share = a.get("share").unwrap();
    assert_eq!(
        djrust::share::Share::from_payload(share, None)
            .genre
            .as_deref(),
        Some("techno")
    );
    assert_eq!(
        a.get("cid").unwrap().as_str(),
        Some(share_cid(share).as_str())
    );
}

// SessionModelTests.theCIDNamesTheContentAndNothingElse
#[test]
fn the_cid_names_the_content_and_nothing_else() {
    let s = sample();
    assert!(s.cid().starts_with("z4EBG9")); // Swift literal: CIDv1, dag-json, sha2-256, base58btc
    assert_eq!(s.canonical(), s.json().canonical_js());
    assert_eq!(s.cid(), djrust::cid::share_cid_canonical(&s.canonical()));
    let mut t = s.clone();
    t.events[3] = act(300, DeckAction::Crossfader(0.5000001));
    assert_ne!(t.cid(), s.cid(), "one changed value changes the name");
    let mut u = s.clone();
    u.events
        .push(act(900, DeckAction::Panic { deck: "A".into() }));
    assert_ne!(u.cid(), s.cid());
    // key order in the text doesn't matter
    let reordered = JsonValue::parse(&s.json().js_stringified())
        .unwrap()
        .canonical_js();
    assert_eq!(Session::decode(&reordered, None).unwrap().cid(), s.cid());
}

// SessionModelTests.decodeChecksTheCID
#[test]
fn decode_checks_the_cid() {
    let s = sample();
    assert_eq!(Session::decode(&s.canonical(), Some(&s.cid())).unwrap(), s);
    assert_eq!(
        Session::decode(&s.canonical(), Some("zdjNope")).unwrap_err(),
        SessionError::CidMismatch
    );
    assert_eq!(
        Session::decode(
            r#"{"schema":"other","sampleRate":1,"frames":1,"decks":{}}"#,
            None
        )
        .unwrap_err(),
        SessionError::BadSchema
    );
}

// SessionModelTests.contentIDsOfAudioAreStableAndSensitive (without DemoLoop)
#[test]
fn content_ids_of_audio_are_stable_and_sensitive() {
    let l = vec![0.25f32; 100];
    let a = sample_content_id(8000.0, &l, None);
    assert_eq!(a, sample_content_id(8000.0, &l, None));
    assert_eq!(a.len(), 64);
    let mut longer = l.clone();
    longer.push(0.0);
    assert_ne!(a, sample_content_id(8000.0, &longer, None));
    assert_ne!(a, sample_content_id(16000.0, &l, None));
}

#[test]
fn the_schema_name_is_the_one_the_swift_app_still_writes() {
    assert_eq!(SCHEMA, "djrust/session/v1");
    assert_eq!(
        sample().json().get("schema").unwrap().as_str(),
        Some(SCHEMA)
    );
}

/// `ReverbTests.sessionsKeepTheReverbAndOldSetsKeepTheirText`: an untouched deck leaves the
/// reverb out of the canonical text, and the three knobs round-trip.
#[test]
fn sessions_keep_the_reverb_and_old_sets_keep_their_text() {
    use djrust::session::model::DeckParams;
    let plain = DeckParams::default();
    assert!(plain.json().get("reverbWet").is_none(), "untouched: the text is as before");
    let p = DeckParams {
        reverb_wet: 0.3,
        reverb_size: 0.7,
        reverb_tone: 0.2,
        ..DeckParams::default()
    };
    assert_eq!(DeckParams::from_json(&p.json()), p);
    // one knob moved writes all three (Swift's `json` does)
    let q = DeckParams { reverb_size: 0.9, ..DeckParams::default() };
    let j = q.json();
    assert!(j.get("reverbWet").is_some() && j.get("reverbSize").is_some() && j.get("reverbTone").is_some());
    assert_eq!(DeckParams::from_json(&j), q);
}

/// A session leaves defaults out, so older sessions keep their text and CID.
#[test]
fn defaults_are_left_out_of_the_text() {
    let s = sample();
    let text = s.canonical();
    assert!(!text.contains("loopGain") && !text.contains("autoMix") && !text.contains("fxUnit"));
    assert!(!text.contains("reverbWet") && !text.contains("wander") && !text.contains("autoGain"));
    let mut t = s.clone();
    t.loop_gain = 0.7;
    t.auto_mix.settings.on = true;
    t.params
        .get_mut("A")
        .unwrap()
        .fx_unit
        .set(FxParam::Drive, 0.4);
    t.params.get_mut("A").unwrap().reverb_wet = 0.2;
    t.params.get_mut("A").unwrap().disabled_roles = vec!["bass".into()];
    t.params.get_mut("A").unwrap().wander.on = true;
    t.params.get_mut("A").unwrap().auto_gain = true;
    let back = Session::decode(&t.canonical(), Some(&t.cid())).unwrap();
    assert_eq!(back, t);
}

fn every_action() -> Vec<DeckAction> {
    let d = |s: &str| s.to_string();
    let mut spec_seeded = techno_spec();
    spec_seeded.part_seeds.insert("bass".into(), 7.0);
    let mut voices = BTreeMap::new();
    voices.insert(3, d("saw"));
    voices.insert(11, d("kit"));
    let mut tweak = VoiceTweak::default();
    tweak.set("gainDB", Some(-3.0));
    tweak.set("lowpass", Some(2400.0));
    vec![
        DeckAction::Play {
            deck: d("A"),
            on: true,
        },
        DeckAction::Cue { deck: d("A") },
        DeckAction::HotCue {
            deck: d("A"),
            slot: 1,
        },
        DeckAction::ClearCue {
            deck: d("A"),
            slot: 1,
        },
        DeckAction::Loop {
            deck: d("A"),
            op: d("in"),
        },
        DeckAction::Param {
            deck: d("A"),
            name: d("eqLow"),
            value: 0.1,
        },
        DeckAction::Crossfader(0.3),
        DeckAction::Master(0.7),
        DeckAction::Load {
            deck: d("A"),
            source: DeckSource::Net(spec_seeded.clone()),
        },
        DeckAction::QueueNext {
            deck: d("B"),
            spec: spec_seeded,
        },
        DeckAction::NextTrack { deck: d("A") },
        DeckAction::AutoAdvance {
            deck: d("A"),
            on: true,
        },
        DeckAction::Section {
            deck: d("A"),
            index: 2,
        },
        DeckAction::Role {
            deck: d("A"),
            role: d("kick"),
            on: false,
        },
        DeckAction::RoleDisable {
            deck: d("A"),
            role: d("kick"),
            on: true,
        },
        DeckAction::Macro {
            deck: d("A"),
            id: d("drop"),
        },
        DeckAction::Panic { deck: d("A") },
        DeckAction::Stinger { deck: d("A"), n: 1 },
        DeckAction::Sync {
            deck: d("B"),
            align: d("tempo"),
        },
        DeckAction::Quantize {
            deck: d("B"),
            mode: d("bar"),
        },
        DeckAction::Follow {
            deck: d("B"),
            leader: Some(d("A")),
        },
        DeckAction::Follow {
            deck: d("B"),
            leader: None,
        },
        DeckAction::Grid {
            deck: d("B"),
            bpm: 99.5,
            offset: 12.0,
        },
        DeckAction::LoopRecord {
            deck: d("A"),
            on: true,
            bars: None,
        },
        DeckAction::LoopRecord {
            deck: d("A"),
            on: false,
            bars: Some(4),
        },
        DeckAction::GrabBars {
            deck: d("A"),
            bars: 2,
        },
        DeckAction::Clip {
            deck: d("L"),
            id: d("abc"),
        },
        DeckAction::ClipLoop {
            id: d("abc"),
            on: true,
        },
        DeckAction::ClipGain {
            id: d("abc"),
            value: 0.5,
        },
        DeckAction::ClipCrop {
            id: d("abc"),
            from: 1,
            to: 3,
        },
        DeckAction::ClipRetune {
            id: d("abc"),
            voices,
        },
        DeckAction::ClipSequence {
            steps: vec![SequenceStep::new("a", 2), SequenceStep::new("b", 1)],
            replacing: Some(d("old")),
        },
        DeckAction::ClipSequence {
            steps: vec![],
            replacing: None,
        },
        DeckAction::ClipRemove { id: d("abc") },
        DeckAction::ClipRestore {
            id: d("abc"),
            at: Some(2),
        },
        DeckAction::ClipRestore {
            id: d("abc"),
            at: None,
        },
        DeckAction::ClipForget { id: d("abc") },
        DeckAction::ClipRename {
            id: d("abc"),
            title: d("Hey"),
        },
        DeckAction::ClipColor {
            id: d("abc"),
            color: Some(d("red")),
        },
        DeckAction::ClipColor {
            id: d("abc"),
            color: None,
        },
        DeckAction::LoopQuantize { beats: 8 },
        DeckAction::LoopFollow { mode: d("source") },
        DeckAction::SceneSave { name: d("Scene 1") },
        DeckAction::SceneLaunch {
            name: d("Scene 1"),
            clips: None,
        },
        DeckAction::SceneLaunch {
            name: d("Scene 1"),
            clips: Some(vec![
                SceneClip {
                    clip_id: d("a"),
                    gain: 0.5,
                    fx: None,
                },
                SceneClip {
                    clip_id: d("b"),
                    gain: 1.0,
                    fx: Some(LoopFx {
                        drive: 6.0,
                        mode: LoopFxMode::Tube,
                        amount: 0.35,
                        tone: 0.8,
                        ..LoopFx::default()
                    }),
                },
            ]),
        },
        DeckAction::SceneRename {
            name: d("a"),
            to: d("b"),
        },
        DeckAction::SceneDelete { name: d("a") },
        DeckAction::ClipMute {
            id: d("a"),
            on: true,
        },
        DeckAction::ClipSolo {
            id: d("a"),
            on: true,
        },
        DeckAction::ClipKeyLock {
            id: d("a"),
            on: true,
        },
        DeckAction::LoopKeyLock { on: true },
        DeckAction::LoopFade { mode: d("bar") },
        DeckAction::ClipQuantize {
            id: d("a"),
            beats: Some(16),
        },
        DeckAction::ClipQuantize {
            id: d("a"),
            beats: None,
        },
        DeckAction::ClipFade {
            id: d("a"),
            mode: Some(d("beat")),
        },
        DeckAction::ClipFollow {
            id: d("a"),
            mode: Some(d("louder")),
        },
        DeckAction::TapBars { bars: Some(4) },
        DeckAction::TapBars { bars: None },
        DeckAction::VoiceTweak {
            deck: d("A"),
            channel: 4,
            params: tweak,
        },
        DeckAction::VoiceSynth {
            deck: d("A"),
            channel: 4,
            on: true,
        },
        DeckAction::ClipMove {
            id: d("a"),
            before: Some(d("b")),
        },
        DeckAction::Lock {
            key: d("A.macro.drop"),
            on: true,
        },
        DeckAction::Wander {
            deck: d("A"),
            on: true,
        },
        DeckAction::WanderPace {
            deck: d("A"),
            bars: 8,
        },
        DeckAction::WanderLock {
            deck: d("A"),
            dimension: d("keep:bass"),
            on: true,
        },
        DeckAction::SetKit {
            deck: d("A"),
            kit: Some(d("MuseScore_General.sf2#128:0")),
        },
        DeckAction::SetKit {
            deck: d("A"),
            kit: None,
        },
        DeckAction::SetPalette {
            deck: d("A"),
            palette: Some(d("jazz")),
        },
        DeckAction::ClipFx {
            id: d("a"),
            fx: LoopFx {
                bypass: true,
                drive: 12.0,
                mode: LoopFxMode::Crush,
                bits: 6.0,
                downsample: 4,
                ..LoopFx::default()
            },
        },
        DeckAction::LoopFxLink { on: true },
        DeckAction::SetRoleVoice {
            deck: d("A"),
            role: d("melody"),
            voice: Some(d("gm:0")),
        },
        DeckAction::ClipImport {
            id: d("abc"),
            title: d("Tone"),
            bpm: 120.0,
            beats: 8,
            provenance: None,
        },
        DeckAction::ClipImport {
            id: d("abc"),
            title: d("Tone"),
            bpm: 119.5,
            beats: 4,
            provenance: Some(JsonValue::Object(JsonObject::from_pairs([(
                "kind",
                JsonValue::str("stock"),
            )]))),
        },
        DeckAction::MixOut {
            deck: d("A"),
            bars: 8,
        },
        DeckAction::MixIn {
            deck: d("A"),
            bars: 4,
        },
        DeckAction::Rewind { deck: d("A") },
        DeckAction::Focus { deck: d("B") },
        DeckAction::StemMute {
            deck: d("A"),
            stem: d("mic"),
            on: false,
        },
        DeckAction::MasterFx {
            deck: d("A"),
            key: d("hp"),
            value: 0.4,
        },
        DeckAction::Fx {
            deck: d("A"),
            param: d("phaser-mix"),
            value: 0.6,
        },
        DeckAction::FxReset {
            deck: d("A"),
            on_beat: false,
        },
        DeckAction::FxReset {
            deck: d("A"),
            on_beat: true,
        },
        DeckAction::FxKill {
            deck: d("A"),
            on: true,
            on_beat: false,
        },
        DeckAction::FxKill {
            deck: d("A"),
            on: false,
            on_beat: true,
        },
        DeckAction::AutoMix {
            on: true,
            bars: 16,
            every: 8,
        },
        DeckAction::AutoMixNow,
        DeckAction::AutoMixDefer { handoff: 3 },
    ]
}

// ControlTests.everyPerformOpIsDocumented's premise: every op the codec writes is in jsonOps,
// and a case exists for each op.
#[test]
fn every_action_round_trips_and_every_op_is_covered() {
    let mut ops: Vec<String> = Vec::new();
    for a in every_action() {
        let json = a.json();
        let back = DeckAction::from_json(&JsonValue::parse(&json.js_stringified()).unwrap())
            .unwrap_or_else(|| panic!("{} did not decode", json.js_stringified()));
        assert_eq!(back, a, "{}", json.js_stringified());
        let op = a.op();
        assert!(JSON_OPS.contains(&op.as_str()), "{op} is not in JSON_OPS");
        ops.push(op);
    }
    for op in JSON_OPS {
        assert!(ops.iter().any(|o| o == op), "no round-trip case for {op}");
    }
}

#[test]
fn optional_fields_are_written_only_when_set() {
    // "`bars` only when set: a hold-only take encodes as it always has"
    let t = DeckAction::LoopRecord {
        deck: "A".into(),
        on: true,
        bars: None,
    }
    .json()
    .js_stringified();
    assert_eq!(t, r#"{"op":"loopRecord","deck":"A","on":true}"#);
    let r = DeckAction::FxReset {
        deck: "A".into(),
        on_beat: false,
    }
    .json()
    .js_stringified();
    assert_eq!(r, r#"{"op":"fxReset","deck":"A"}"#);
    let k = DeckAction::FxKill {
        deck: "A".into(),
        on: true,
        on_beat: true,
    }
    .json()
    .js_stringified();
    assert_eq!(k, r#"{"op":"fxKill","deck":"A","on":true,"onBeat":true}"#);
    assert_eq!(
        DeckAction::Follow {
            deck: "B".into(),
            leader: None
        }
        .json()
        .js_stringified(),
        r#"{"op":"follow","deck":"B","leader":null}"#
    );
}

#[test]
fn decoding_fills_the_swift_defaults() {
    let p = |s: &str| DeckAction::from_json(&JsonValue::parse(s).unwrap());
    assert_eq!(
        p(r#"{"op":"crossfader"}"#),
        Some(DeckAction::Crossfader(0.5))
    );
    assert_eq!(p(r#"{"op":"master"}"#), Some(DeckAction::Master(0.8)));
    assert_eq!(
        p(r#"{"op":"mixOut","deck":"A"}"#),
        Some(DeckAction::MixOut {
            deck: "A".into(),
            bars: 16
        })
    );
    assert_eq!(
        p(r#"{"op":"mixIn","deck":"A"}"#),
        Some(DeckAction::MixIn {
            deck: "A".into(),
            bars: 4
        })
    );
    assert_eq!(
        p(r#"{"op":"loopFade"}"#),
        Some(DeckAction::LoopFade {
            mode: "10ms".into()
        })
    );
    assert_eq!(
        p(r#"{"op":"autoMix","on":true,"bars":7,"every":3}"#),
        Some(DeckAction::AutoMix {
            on: true,
            bars: 8,
            every: 0
        }),
        "clamped to what the engine offers"
    );
    assert_eq!(
        p(r#"{"op":"voiceTweak","deck":"A"}"#),
        None,
        "needs a channel"
    );
    assert_eq!(p(r#"{"op":"nope"}"#), None);
    assert_eq!(p(r#"{"deck":"A"}"#), None);
    assert_eq!(
        voices_json(&BTreeMap::from([(3, "saw".to_string())])).js_stringified(),
        r#"{"3":"saw"}"#
    );
}

// Not a Swift test: `session::types` and `journal::state` name the engine's own types, not
// copies of them. This only compiles while the paths are re-exports of the one definition.
#[test]
fn the_value_types_a_session_names_are_the_engines_own() {
    let _: djrust::wander::WanderState = WanderState::default();
    let _: djrust::wander::WanderKey = WanderKey {
        scale: "minor".into(),
        root_pc: 9,
        offset: 0,
    };
    let _: djrust::automix::AutoMixState = AutoMixState::default();
    let _: djrust::automix::AutoMixSettings = AutoMixSettings::default();
    let _: djrust::fx::unit::FxParam = FxParam::Pitch;
    let _: djrust::fx::unit::FxUnitSettings = FxUnitSettings::default();
    let _: djrust::wave::VoiceTweak = VoiceTweak::default();
    let _: djrust::deck::beat_grid::BeatGrid = BeatGrid::new(120.0, 0.0, 1.0);
    let _: djrust::wave::VoiceSetting = djrust::journal::state::VoiceSetting {
        tweak: VoiceTweak::default(),
        use_synth: false,
        instrument: String::new(),
    };
}

#[test]
fn small_value_types_round_trip_and_clamp() {
    // wander: an invalid pace and unknown locks fall away
    let w = WanderState::from_json(
        &JsonValue::parse(
            r#"{"on":true,"pace":5,"step":3,"locks":["tempo","bogus","keep:bass","keep:"]}"#,
        )
        .unwrap(),
    );
    assert!(w.on && w.pace == 16 && w.step == 3);
    assert_eq!(
        w.locks.iter().cloned().collect::<Vec<_>>(),
        ["keep:bass", "tempo"]
    );
    let mut w2 = WanderState::default();
    w2.on = true;
    w2.anchor_bpm = Some(124.0);
    w2.key = Some(WanderKey {
        scale: "minor".into(),
        root_pc: 9,
        offset: -3,
    });
    w2.paused = true;
    w2.quiet_bars = 2;
    assert_eq!(WanderState::from_json(&w2.json()), w2);
    // FX unit: only moved controls are written, values are clamped into their travel
    let mut fx = FxUnitSettings::default();
    assert_eq!(fx.json().js_stringified(), "{}");
    fx.set(FxParam::Pitch, 99.0);
    fx.set(FxParam::Lp, 0.5);
    fx.killed = true;
    assert_eq!(fx.get(FxParam::Pitch), 12.0);
    assert_eq!(
        fx.json().js_stringified(),
        r#"{"lp":0.5,"pitch":12,"killed":true}"#
    );
    assert_eq!(FxUnitSettings::from_json(Some(&fx.json())), fx);
    // voice tweaks clamp; a cutoff under 40 Hz is "off"
    let t = VoiceTweak::from_json(Some(
        &JsonValue::parse(r#"{"gainDB":100,"lowpass":10,"x":1}"#).unwrap(),
    ));
    assert_eq!(t.gain_db, Some(24.0));
    assert_eq!(t.lowpass, Some(0.0));
    assert_eq!(t.to_json().js_stringified(), r#"{"gainDB":24,"lowpass":0}"#);
    assert!(VoiceTweak::default().is_empty());
    // loop FX: every field written, clamped on the way in
    let f = LoopFx::from_json(Some(
        &JsonValue::parse(r#"{"drive":99,"mode":"fold","downsample":99}"#).unwrap(),
    ));
    assert_eq!(
        (f.drive, f.mode, f.downsample),
        (24.0, LoopFxMode::Fold, 32)
    );
    // auto-mix: session form vs checkpoint form
    let mut a = AutoMixState::default();
    assert!(a.is_session_default());
    a.handoffs = 2;
    a.playing = Some("A".into());
    assert!(!a.is_session_default());
    assert!(
        a.checkpoint_json().get("playing").is_some() && a.session_json().get("playing").is_none()
    );
    assert_eq!(
        AutoMixState::from_start_json(Some(&a.checkpoint_json())),
        a.start()
    );
}

// SessionRecorderGuardTests: the pure rule
#[test]
fn a_session_starts_only_at_rest_on_something_it_can_name() {
    let net =
        |playing, tick, cues, spec: Option<TrackSpec>, raw: Option<JsonValue>| DeckSnapshot::Net {
            playing,
            tick,
            has_hot_cues: cues,
            spec,
            raw_nets: raw,
        };
    assert_eq!(
        source_descriptor("A", &net(true, 0, false, Some(techno_spec()), None)).unwrap_err(),
        SessionError::NotAtRest { deck: "A".into() }
    );
    assert_eq!(
        source_descriptor("A", &net(false, 48, false, Some(techno_spec()), None)).unwrap_err(),
        SessionError::NotAtRest { deck: "A".into() }
    );
    // refusesANetItCannotName
    assert_eq!(
        source_descriptor("A", &net(false, 0, false, None, None)).unwrap_err(),
        SessionError::Unrecordable { deck: "A".into() }
    );
    assert_eq!(
        source_descriptor("A", &net(false, 0, false, Some(techno_spec()), None)).unwrap(),
        DeckSource::Net(techno_spec())
    );
    let sample = |position, cues, looping, cue| DeckSnapshot::Sample {
        playing: false,
        position,
        has_hot_cues: cues,
        looping,
        cue_point: cue,
        buffer_id: Some("abc".into()),
        is_demo: false,
        title: "t".into(),
        grid: None,
    };
    for bad in [
        sample(5.0, false, false, 0.0),
        sample(0.0, true, false, 0.0),
        sample(0.0, false, true, 0.0),
        sample(0.0, false, false, 9.0),
    ] {
        assert_eq!(
            source_descriptor("B", &bad).unwrap_err(),
            SessionError::NotAtRest { deck: "B".into() }
        );
    }
    assert_eq!(
        source_descriptor("B", &sample(0.0, false, false, 0.0)).unwrap(),
        DeckSource::Sample {
            id: "abc".into(),
            title: "t".into(),
            grid: None
        }
    );
    let demo = DeckSnapshot::Sample {
        playing: false,
        position: 0.0,
        has_hot_cues: false,
        looping: false,
        cue_point: 0.0,
        buffer_id: Some("whatever".into()),
        is_demo: true,
        title: "demo loop".into(),
        grid: Some(BeatGrid::new(120.0, 0.0, 1.0)),
    };
    assert_eq!(
        source_descriptor("B", &demo).unwrap(),
        DeckSource::Sample {
            id: "demo-loop".into(),
            title: "demo loop".into(),
            grid: Some(BeatGrid::new(120.0, 0.0, 1.0))
        }
    );
    let empty = DeckSnapshot::Sample {
        playing: false,
        position: 0.0,
        has_hot_cues: false,
        looping: false,
        cue_point: 0.0,
        buffer_id: None,
        is_demo: false,
        title: String::new(),
        grid: None,
    };
    assert_eq!(source_descriptor("C", &empty).unwrap(), DeckSource::None);
}

// SessionRecorderGuardTests.capturesTheStartingState + the event stamping of SessionRecorder
#[test]
fn the_recorder_stamps_events_relative_to_its_start() {
    let mut decks = BTreeMap::new();
    decks.insert("A".to_string(), DeckSource::Net(techno_spec()));
    let mut params = BTreeMap::new();
    let mut p = DeckParams::default();
    p.volume = 0.31;
    params.insert("A".to_string(), p);
    let auto = AutoMixState {
        playing: Some("A".into()),
        ..AutoMixState::default()
    };
    let mut r = SessionRecorder::new(1000, 22_050.0, decks, params, 0.2, 0.55, 0.5, &auto);
    assert!(r.events().is_empty());
    r.record_midi(1128, "APC", &[0x90, 48, 0x7F]);
    r.record(
        1256,
        DeckAction::Play {
            deck: "A".into(),
            on: true,
        },
    );
    let s = r.finish(3000);
    assert_eq!(s.frames, 2000);
    assert_eq!(
        s.events.iter().map(|e| e.frame).collect::<Vec<_>>(),
        [128, 256]
    );
    assert_eq!(
        s.events[0].kind,
        SessionEventKind::Midi {
            port: "APC".into(),
            bytes: vec![0x90, 48, 0x7F]
        }
    );
    assert_eq!(s.params["A"].volume, 0.31);
    assert_eq!(
        s.auto_mix,
        s.auto_mix.start(),
        "a session starts at rest: nothing in flight"
    );
    assert_eq!(
        (s.crossfader, s.master, s.sample_rate),
        (0.2, 0.55, 22_050.0)
    );
}

struct Fake {
    frame: i64,
    applied: Vec<(i64, i64)>,
    blocks: usize,
}
impl Conductor for Fake {
    fn frame(&self) -> i64 {
        self.frame
    }
    fn apply(&mut self, e: &SessionEvent) {
        self.applied.push((self.frame, e.frame));
    }
    fn step(&mut self) -> (Vec<f32>, Vec<f32>) {
        self.frame += 128;
        self.blocks += 1;
        (vec![0.5; 128], vec![0.25; 128])
    }
}

#[test]
fn the_player_applies_what_is_due_before_each_block_and_trims_to_the_length() {
    let mut s = sample();
    s.frames = 1000;
    s.events = vec![
        act(
            0,
            DeckAction::Play {
                deck: "A".into(),
                on: true,
            },
        ),
        act(129, DeckAction::Crossfader(0.1)),
        act(130, DeckAction::Crossfader(0.2)),
        act(900, DeckAction::Panic { deck: "A".into() }),
    ];
    let mut c = Fake {
        frame: 0,
        applied: vec![],
        blocks: 0,
    };
    let audio = SessionPlayer::new(&s).drive(&mut c);
    assert_eq!(audio.frames(), 1000);
    assert_eq!(c.blocks, 8);
    // events at 129/130 are due when the engine frame reaches 256 (the block boundary after)
    // the event at 900 would be due on the block after the session's 1000 frames: never applied
    assert_eq!(c.applied, [(0, 0), (256, 129), (256, 130)]);
    assert_eq!(audio.left[0], 0.5);
    assert_eq!(audio.right[999], 0.25);
}

// SessionReplayTests.aMissingSampleIsReportedNotInvented
#[test]
fn a_missing_sample_is_reported_not_invented() {
    let mut s = sample();
    s.decks = BTreeMap::from([(
        "B".to_string(),
        DeckSource::Sample {
            id: "deadbeef".into(),
            title: "x".into(),
            grid: None,
        },
    )]);
    s.events = vec![act(
        10,
        DeckAction::Load {
            deck: "B".into(),
            source: DeckSource::Sample {
                id: "demo-loop".into(),
                title: "d".into(),
                grid: None,
            },
        },
    )];
    let p = SessionPlayer::new(&s);
    assert_eq!(p.needed_samples(), ["deadbeef", "demo-loop"]);
    assert_eq!(
        p.check_samples(|_| false).unwrap_err(),
        SessionError::MissingSample {
            id: "deadbeef".into()
        }
    );
    assert!(p.check_samples(|id| id == "deadbeef").is_ok());
}

#[test]
fn wav_is_a_riff_of_the_right_size() {
    let a = RenderedAudio {
        sample_rate: 22_050.0,
        left: vec![0.5, -1.0, 2.0],
        right: vec![0.0, 0.25, -2.0],
    };
    let f = encode_wav(&a, true);
    assert_eq!(f.len(), 44 + 3 * 8); // SessionReplayTests: a.count == 44 + frames * 8
    assert_eq!(&f[0..4], b"RIFF");
    assert_eq!(&f[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([f[20], f[21]]), 3);
    assert_eq!(f32::from_le_bytes([f[44], f[45], f[46], f[47]]), 0.5);
    let p = encode_wav(&a, false);
    assert_eq!(p.len(), 44 + 3 * 4);
    assert_eq!(i16::from_le_bytes([p[44], p[45]]), 16384); // 0.5 * 32767 rounded
    assert_eq!(i16::from_le_bytes([p[46], p[47]]), 0);
    assert_eq!(i16::from_le_bytes([p[48], p[49]]), -32768);
    assert_eq!(i16::from_le_bytes([p[52], p[53]]), 32767, "clipped");
    let (peak, rms) = a.levels();
    assert!(peak > 5.9 && peak < 6.1 && rms.is_finite());
}

// ---- ShareSource -----------------------------------------------------------------------------------

#[test]
fn a_share_becomes_a_source_or_says_why_not() {
    let payload = r#"{"genre":"synthwave","seed":1984,"structure":"standard","cohesion":"v2","tracks":{"4":{"instrument":"lead"}},"fadeIn":[1]}"#;
    let DeckSource::Net(spec) = DeckSource::share(payload).unwrap() else {
        panic!("not a net")
    };
    assert_eq!(spec.genre, "synthwave");
    assert_eq!(spec.seed, 1984.0);
    assert_eq!(spec.structure.as_deref(), Some("standard"));
    assert!(spec.layers.contains_key("tracks") && spec.layers.contains_key("fadeIn"));
    // the spec's payload reads back to the same spec
    assert_eq!(TrackSpec::from_payload(&spec.payload()).unwrap(), spec);
    // raw nets
    let raw =
        r#"{"name":"Hand","nets":{"kick":{"places":{"p0":{}},"transitions":{"t0":{}},"arcs":[]}}}"#;
    assert!(matches!(
        DeckSource::share(raw).unwrap(),
        DeckSource::RawNets(_)
    ));
    assert_eq!(
        DeckSource::share(r#"{"nets":{}}"#).unwrap_err(),
        ShareLoadError::NoTrack
    );
    assert_eq!(
        DeckSource::share(r#"{"name":"x"}"#).unwrap_err(),
        ShareLoadError::NoTrack
    );
    assert_eq!(
        DeckSource::share(r#"{"genre":"nope","seed":1}"#).unwrap_err(),
        ShareLoadError::UnknownGenre("nope".into())
    );
    assert_eq!(
        DeckSource::share("[1]").unwrap_err(),
        ShareLoadError::Unreadable
    );
    assert_eq!(
        DeckSource::share("hello").unwrap_err(),
        ShareLoadError::Unreadable
    );
    assert!(matches!(
        DeckSource::share("?cid=zdpuAbc").unwrap_err(),
        ShareLoadError::NeedsFetch { .. }
    ));
}

// ---- SessionMix ------------------------------------------------------------------------------------

fn synthwave() -> Vec<TrackSpec> {
    vec![
        TrackSpec::new("synthwave", 1984.0, Some("standard")),
        TrackSpec::new("synthwave", 2077.0, Some("standard")),
    ]
}

fn actions(s: &Session) -> Vec<(i64, DeckAction)> {
    s.events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(a) => Some((e.frame, a.clone())),
            _ => None,
        })
        .collect()
}

// SessionMixTests.twoTracksMixInOrder
#[test]
fn two_tracks_mix_in_order() {
    let mix = SessionMix::new(&synthwave(), 20.0, 44_100.0).unwrap();
    let s = &mix.session;
    let ev = actions(s);
    assert_eq!(ev.len(), s.events.len(), "a mix is all actions, no MIDI");
    let frames: Vec<i64> = s.events.iter().map(|e| e.frame).collect();
    let mut sorted = frames.clone();
    sorted.sort();
    assert_eq!(frames, sorted);
    assert!(frames.iter().all(|f| f % SessionMix::GRAIN == 0));
    assert!(frames.iter().all(|&f| f >= 0 && f < s.frames));

    assert_eq!(ev[0].0, 0);
    assert_eq!(
        ev[0].1,
        DeckAction::Play {
            deck: "A".into(),
            on: true
        }
    );
    let find = |a: DeckAction| ev.iter().find(|e| e.1 == a).unwrap().0;
    let sync = find(DeckAction::Sync {
        deck: "B".into(),
        align: "bar".into(),
    });
    let play_b = find(DeckAction::Play {
        deck: "B".into(),
        on: true,
    });
    let stop_a = find(DeckAction::Play {
        deck: "A".into(),
        on: false,
    });
    let stop_b = find(DeckAction::Play {
        deck: "B".into(),
        on: false,
    });
    assert!(sync < play_b && play_b < stop_a && stop_a < stop_b);

    let fader: Vec<(i64, f64)> = ev
        .iter()
        .filter_map(|(f, a)| {
            if let DeckAction::Crossfader(v) = a {
                Some((*f, *v))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(fader.len(), (20.0 / SessionMix::RAMP_STEP) as usize);
    assert!(
        fader.windows(2).all(|w| w[0].1 <= w[1].1),
        "B is crossfader 1: it rises"
    );
    assert_eq!(fader.last().unwrap().1, 1.0);
    assert!(fader.iter().all(|f| f.0 > play_b && f.0 < stop_a));
    let ramp = (fader.last().unwrap().0 - play_b) as f64 / s.sample_rate;
    assert!((ramp - 20.0).abs() < 0.05);

    let t = &mix.tracks;
    assert_eq!(
        t.iter().map(|t| t.deck.as_str()).collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert!(t[0].in_frame == 0 && t[0].out_frame == stop_a);
    assert!(t[1].in_frame == play_b && t[1].out_frame == stop_b);
    assert!(s.frames > stop_b);
    assert!(
        s.frames - stop_b <= s.sample_rate as i64 + SessionMix::GRAIN,
        "about a second of tail"
    );
    assert!(t.iter().all(|t| t.seconds() > 60.0));
    assert_eq!(t[0].played_bpm, t[0].bpm);
    assert_eq!(mix.title(), format!("{} into {}", t[0].title, t[1].title));
    assert_eq!(s.decks["A"], DeckSource::Net(synthwave()[0].clone()));
    assert_eq!(s.decks["B"], DeckSource::Net(synthwave()[1].clone()));
    assert_eq!(s.crossfader, 0.0);
}

// SessionMixTests.thirdTrackReloadsTheFreedDeck
#[test]
fn third_track_reloads_the_freed_deck() {
    let mut specs = synthwave();
    specs.push(TrackSpec::new(
        "synthwave",
        7.0,
        Some("standard"),
    ));
    let mix = SessionMix::new(&specs, 20.0, 44_100.0).unwrap();
    let ev = actions(&mix.session);
    assert_eq!(
        mix.tracks
            .iter()
            .map(|t| t.deck.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "A"]
    );
    let load = ev
        .iter()
        .find(|e| matches!(&e.1, DeckAction::Load { deck, .. } if deck == "A"))
        .unwrap();
    let sync = ev
        .iter()
        .rev()
        .find(|e| {
            e.1 == DeckAction::Sync {
                deck: "A".into(),
                align: "bar".into(),
            }
        })
        .unwrap();
    assert!(load.0 > mix.tracks[0].out_frame && load.0 < sync.0);
    assert!(mix.tracks[2].in_frame > mix.tracks[1].in_frame);
    let frames: Vec<i64> = mix.session.events.iter().map(|e| e.frame).collect();
    let mut sorted = frames.clone();
    sorted.sort();
    assert_eq!(frames, sorted);
    let faders: Vec<f64> = ev
        .iter()
        .filter_map(|(_, a)| {
            if let DeckAction::Crossfader(v) = a {
                Some(*v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(faders.len(), 2 * (20.0 / SessionMix::RAMP_STEP) as usize);
    assert_eq!(*faders.last().unwrap(), 0.0);
}

// SessionMixTests.roundTripsThroughTheStoreWithAStableCID (the store half is in djhost)
#[test]
fn a_mix_is_deterministic_and_its_json_re_encodes() {
    let mix = SessionMix::new(&synthwave(), 20.0, 44_100.0).unwrap();
    let cid = mix.session.cid();
    let again = SessionMix::new(&synthwave(), 20.0, 44_100.0).unwrap();
    assert_eq!(again.session.cid(), cid);
    let back = Session::from_json(&JsonValue::parse(&mix.session.json().js_stringified()).unwrap())
        .unwrap();
    assert_eq!(back, mix.session);
    assert_eq!(back.cid(), cid);
    assert_ne!(
        SessionMix::new(&synthwave(), 12.0, 44_100.0)
            .unwrap()
            .session
            .cid(),
        cid
    );
    // master is a Float's 0.8
    assert_eq!(mix.session.master, 0.8f32 as f64);
}

// SessionMixTests.refusesWhatItCannotMix
#[test]
fn refuses_what_it_cannot_mix() {
    assert_eq!(
        SessionMix::new(&synthwave()[..1], 20.0, 44_100.0).unwrap_err(),
        SessionMixError::TooFewTracks
    );
    let bad = TrackSpec::new("nope", 1.0, Some("standard"));
    assert_eq!(
        SessionMix::new(&[synthwave()[0].clone(), bad], 20.0, 44_100.0).unwrap_err(),
        SessionMixError::UnknownGenre("nope".into())
    );
    // A song with no sections is an endless loop: there is no end to mix out of.
    let looped = TrackSpec::new("synthwave", 1.0, None);
    assert!(matches!(
        SessionMix::new(&[looped, synthwave()[1].clone()], 20.0, 44_100.0).unwrap_err(),
        SessionMixError::Endless(_)
    ));
}

#[test]
fn the_tempo_rule_picks_half_or_double_time_when_it_fits_the_fader() {
    let m = tempo_match(120.0, 124.0);
    assert!(m.mult == 1.0 && (m.ratio - 124.0 / 120.0).abs() < 1e-12 && !m.clamped);
    let m = tempo_match(70.0, 140.0); // half of 140 is 70
    assert!(m.mult == 0.5 && (m.ratio - 1.0).abs() < 1e-12 && !m.clamped);
    let m = tempo_match(100.0, 130.0);
    assert!(
        (m.ratio - 1.08).abs() < 1e-12 && m.clamped,
        "clamped to the pitch range"
    );
}

fn layout(gains_db: &[f64], sr: f64) -> (Vec<MixTrack>, RenderedAudio) {
    let spec = synthwave()[0].clone();
    let mut tracks = Vec::new();
    for i in 0..gains_db.len() {
        let in_f = (i as f64 * 30.0 * sr) as i64;
        let len = if i + 1 < gains_db.len() { 40.0 } else { 30.0 };
        tracks.push(MixTrack {
            spec: spec.clone(),
            title: format!("t{i}"),
            deck: SessionMix::DECKS[i % 2].into(),
            bpm: 120.0,
            played_bpm: 120.0,
            steps: 0,
            in_frame: in_f,
            out_frame: in_f + (len * sr) as i64,
        });
    }
    let n = (tracks.last().unwrap().out_frame + sr as i64) as usize;
    let mut left = vec![0.0f32; n];
    for (i, t) in tracks.iter().enumerate() {
        let a = 0.25 * 10f64.powf(gains_db[i] / 20.0);
        for f in t.in_frame..t.out_frame {
            let mut g: f64 = 1.0;
            if i > 0 {
                g = g.min((f - t.in_frame) as f64 / (10.0 * sr));
            }
            if i + 1 < tracks.len() {
                g = g.min((t.out_frame - f) as f64 / (10.0 * sr));
            }
            left[f as usize] +=
                (a * g * (2.0 * std::f64::consts::PI * 1000.0 * f as f64 / sr).sin()) as f32;
        }
    }
    (
        tracks,
        RenderedAudio {
            sample_rate: sr,
            left: left.clone(),
            right: left,
        },
    )
}

// SessionMixTests.reportMeasuresEachTrackAndEachStep
#[test]
fn report_measures_each_track_and_each_step() {
    let (tracks, audio) = layout(&[0.0, 1.5, 7.5], 16_000.0);
    let r = report(&tracks, &audio, 8.0);
    assert_eq!(
        r.tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect::<Vec<_>>(),
        ["t0", "t1", "t2"]
    );
    assert_eq!(
        r.tracks.iter().map(|t| t.deck.as_str()).collect::<Vec<_>>(),
        ["A", "B", "A"]
    );
    let reference = r.tracks[0].lufs;
    assert!((r.tracks[1].lufs - reference - 1.5).abs() < 0.1);
    assert!((r.tracks[2].lufs - reference - 7.5).abs() < 0.1);
    // A 0.25 sine reads about −12 dBTP; the loudest track sets the set's peak.
    assert!((r.tracks[0].true_peak - 20.0 * 0.25f64.log10()).abs() < 0.1);
    assert!((r.true_peak - r.tracks[2].true_peak).abs() < 0.01);
    assert_eq!(
        r.transitions.iter().map(|t| t.index).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!((r.transitions[0].step() - 1.5).abs() < 0.1);
    assert!((r.transitions[1].step() - 6.0).abs() < 0.1);
    assert!((r.seconds - audio.frames() as f64 / audio.sample_rate).abs() < 1e-9);
    assert!(r.lufs > reference && r.lufs < r.tracks[2].lufs);
    // The bar: 6 LU is over 2, and 91 s is under ten minutes.
    let p = r.default_problems();
    assert_eq!(p.len(), 2);
    assert!(p.iter().any(|x| x.contains("under 600")));
    assert!(p.iter().any(|x| x.contains("transition into 3")));
    assert!(r.problems(60.0, 6.5, -1.0).is_empty());
    assert_eq!(r.problems(60.0, 6.5, -20.0).len(), 1);
    assert_eq!(r.lines().len(), 3 + 2 + 2);
}

// SessionMixTests.reportOfALevelSetPasses
#[test]
fn report_of_a_level_set_passes() {
    let (tracks, audio) = layout(&[0.0, -1.0, 0.5], 16_000.0);
    let r: MixReport = report(&tracks, &audio, 8.0);
    assert!(r.transitions.iter().all(|t| t.step().abs() <= 2.0));
    assert!(r.problems(60.0, 2.0, -1.0).is_empty());
    assert!(
        r.lines().last().unwrap().contains("under 600"),
        "only the length misses the bar"
    );
    let quiet = RenderedAudio {
        sample_rate: audio.sample_rate,
        left: vec![0.0; audio.frames()],
        right: vec![0.0; audio.frames()],
    };
    let q = report(&tracks, &quiet, 8.0);
    assert!(q
        .tracks
        .iter()
        .all(|t| t.lufs == -200.0 && t.true_peak == -200.0));
}

// ---- SessionPublish ------------------------------------------------------------------------------------

fn publish_session_fixture() -> Session {
    let mut decks = BTreeMap::new();
    decks.insert(
        "A".to_string(),
        DeckSource::Net(TrackSpec::new(
            "techno",
            42.0,
            Some("standard"),
        )),
    );
    let mut params = BTreeMap::new();
    params.insert("A".to_string(), DeckParams::default());
    Session::new(
        44_100.0,
        44_100 * 3,
        decks,
        params,
        0.5,
        0.8,
        vec![act(
            100,
            DeckAction::Play {
                deck: "A".into(),
                on: true,
            },
        )],
    )
}

fn no_local() -> LocalOnly {
    LocalOnly::default()
}

fn bundle(
    s: &Session,
    audio: Option<&[u8]>,
    draft: bool,
    allow: bool,
) -> Result<SessionBundle, PublishError> {
    SessionBundle::new(
        s,
        None,
        audio,
        "2026-10-08T00:00:00Z",
        draft,
        allow,
        &no_local(),
    )
}

// SessionPublishTests.bundleIsNamedByItsOwnHash
#[test]
fn bundle_is_named_by_its_own_hash() {
    let s = publish_session_fixture();
    let b = bundle(&s, None, false, false).unwrap();
    assert_eq!(b.cid, s.cid());
    assert_eq!(
        b.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["session.json", "index.md"]
    );
    let json = String::from_utf8(b.files[0].data.clone()).unwrap();
    assert!(json.ends_with('\n'));
    let back = Session::decode(&json[..json.len() - 1], Some(&b.cid)).unwrap();
    assert_eq!(back, s);
    let md = String::from_utf8(b.files[1].data.clone()).unwrap();
    assert!(md.contains(&format!("cid: {}", b.cid)));
    assert!(md.contains("source: beats-btw-dj"));
    assert!(md.contains("genre: techno"));
    assert!(!md.contains("draft"));
    assert_eq!(
        b.public_url(),
        format!("{}{}/", djrust::session::publish::PUBLIC_BASE, b.cid)
    );
    // another CDN at run time, with or without the trailing slash
    for base in ["https://cdn.example.org/ipfs/", "https://cdn.example.org/ipfs"] {
        assert_eq!(b.public_url_at(base), format!("https://cdn.example.org/ipfs/{}/", b.cid));
    }
    // the public default, unless the build set DJRUST_PUBLISH_BASE
    if option_env!("DJRUST_PUBLISH_BASE").is_none() {
        assert_eq!(djrust::session::publish::PUBLIC_BASE, "https://cdn.bitwrap.io/ipfs/");
    }
}

// SessionPublishTests.audioAndDraftAreOptional
#[test]
fn audio_and_draft_are_optional() {
    let b = bundle(&publish_session_fixture(), Some(&[1, 2, 3]), true, false).unwrap();
    assert_eq!(
        b.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["session.json", "audio.wav", "index.md"]
    );
    assert!(String::from_utf8(b.files.last().unwrap().data.clone())
        .unwrap()
        .contains("draft: true"));
}

// SessionPublishTests.titlesAreYamlSafe
#[test]
fn titles_are_yaml_safe() {
    assert_eq!(SessionBundle::yaml_string("plain"), "plain");
    assert_eq!(SessionBundle::yaml_string("a: b"), "\"a: b\"");
    assert_eq!(
        SessionBundle::yaml_string("say \"hi\""),
        "\"say \\\"hi\\\"\""
    );
    assert_eq!(SessionBundle::yaml_string(""), "\"\"");
}

// SessionPublishTests.unsafeCIDsNeverReachAShell
#[test]
fn unsafe_cids_never_reach_a_shell() {
    assert!(SessionBundle::is_safe_cid(&format!(
        "bafyrei{}",
        "a".repeat(20)
    )));
    assert!(!SessionBundle::is_safe_cid("bafy; rm -rf ~"));
    assert!(!SessionBundle::is_safe_cid("../etc"));
    assert!(SessionBundle::is_safe_cid(
        "z4EBG9izgLW5Wfg3eGgh7YdtsgKgypw5g7S6uZFnFfxjkdPv9QE"
    ));
    assert!(!SessionBundle::is_safe_cid("abc"));
    assert!(!SessionBundle::is_safe_cid(""));
}

struct Log(std::cell::RefCell<Vec<String>>);
impl CdnTransport for Log {
    fn stage(
        &self,
        b: &SessionBundle,
        progress: &mut dyn FnMut(PublishStep),
    ) -> Result<(), PublishError> {
        for (i, f) in b.files.iter().enumerate() {
            progress(PublishStep::Staging {
                file: f.name.clone(),
                index: i + 1,
                of: b.files.len(),
            });
            self.0.borrow_mut().push(format!("stage {}", f.name));
        }
        Ok(())
    }
    fn reload(&self, cid: &str) -> Result<bool, PublishError> {
        self.0.borrow_mut().push(format!("reload {cid}"));
        Ok(true)
    }
    fn mirror(&self) -> Result<(), PublishError> {
        self.0.borrow_mut().push("mirror".into());
        Ok(())
    }
}

// SessionPublishTests.sshStagesInOrderAndMirrorsOnlyOnRequest / progressNarratesEachStepInOrder
#[test]
fn staging_goes_in_order_and_mirrors_only_on_request() {
    let b = bundle(&publish_session_fixture(), Some(&[9]), false, false).unwrap();
    let t = Log(Default::default());
    let mut steps = Vec::new();
    let quiet = publish_session(&b, &t, false, &mut |s| steps.push(s)).unwrap();
    assert!(!quiet.mirrored && quiet.reloaded);
    let calls = t.0.borrow().clone();
    assert_eq!(
        calls,
        [
            "stage session.json",
            "stage audio.wav",
            "stage index.md",
            &format!("reload {}", b.cid)
        ]
    );
    steps.clear();
    let _ = publish_session(&b, &t, true, &mut |s| steps.push(s)).unwrap();
    assert_eq!(
        steps,
        [
            PublishStep::Staging {
                file: "session.json".into(),
                index: 1,
                of: 3
            },
            PublishStep::Staging {
                file: "audio.wav".into(),
                index: 2,
                of: 3
            },
            PublishStep::Staging {
                file: "index.md".into(),
                index: 3,
                of: 3
            },
            PublishStep::Reloading,
            PublishStep::Mirroring,
            PublishStep::Done,
        ]
    );
    assert_eq!(t.0.borrow().last().unwrap(), "mirror");
}

// SessionPublishTests.probeWaitsForEveryFileThenGivesUp
#[test]
fn probe_waits_for_every_file_then_gives_up() {
    let b = bundle(&publish_session_fixture(), Some(&[9]), false, false).unwrap();
    let urls = PublicProbe::urls(&b);
    assert_eq!(
        urls,
        [
            format!("{}audio.wav", b.public_url()),
            format!("{}index.md", b.public_url())
        ]
    ); // session.json is not polled
       // index.md lands on the 3rd poll round, audio.wav is there from the start
    let mut n = 0;
    let mut head = |u: &str| {
        n += 1;
        if u.ends_with("audio.wav") {
            Some(200)
        } else if n >= 5 {
            Some(200)
        } else {
            Some(404)
        }
    };
    let mut attempts = Vec::new();
    assert!(PublicProbe { attempts: 5 }.wait(&b, &mut head, &mut || {}, &mut |a| attempts.push(a)));
    assert_eq!(attempts, [1, 2, 3]);
    let mut slept = 0;
    assert!(!PublicProbe { attempts: 3 }.wait(&b, &mut |_| None, &mut || slept += 1, &mut |_| {}));
    assert_eq!(slept, 2, "no sleep after the last attempt");
}

// LoopRecordTests: stock loops never reach the CDN, with no override
#[test]
fn stock_loops_on_the_row_are_never_published() {
    let mut s = publish_session_fixture();
    s.events = vec![act(
        0,
        DeckAction::ClipImport {
            id: "abcdef0123456789".into(),
            title: "Tone 01".into(),
            bpm: 120.0,
            beats: 8,
            provenance: Some(JsonValue::Object(JsonObject::from_pairs([(
                "kind",
                JsonValue::str("stock"),
            )]))),
        },
    )];
    assert_eq!(s.imported_clips(), ["stock loop Tone 01 @0"]); // Swift literal
    match bundle(&s, None, false, true).unwrap_err() {
        PublishError::ImportedClips(what) => assert_eq!(what, ["stock loop Tone 01 @0"]),
        e => panic!("refused for the wrong reason: {e}"),
    }
    assert!(bundle(&s, None, false, true)
        .unwrap_err()
        .to_string()
        .contains("stock loops"));
    s.events.clear();
    assert!(s.imported_clips().is_empty());
    assert!(bundle(&s, None, false, false).is_ok());
    // an import with no title is named by its id, without provenance by "outside"
    let t = {
        let mut t = s.clone();
        t.events = vec![act(
            5,
            DeckAction::ClipImport {
                id: "abcdef0123456789".into(),
                title: String::new(),
                bpm: 0.0,
                beats: 4,
                provenance: None,
            },
        )];
        t
    };
    assert_eq!(t.imported_clips(), ["outside loop abcdef0123 @5"]);
}

// PaletteTests / MinecraftTests: local-only sounds are refused unless overridden
#[test]
fn local_only_sounds_are_refused_unless_overridden() {
    let dir = format!("{}/tests/fixtures/beats-btw-dj", env!("CARGO_MANIFEST_DIR"));
    let read = |n: &str| {
        JsonValue::parse(&std::fs::read_to_string(format!("{dir}/{n}")).unwrap()).unwrap()
    };
    let local = LocalOnly::from_json(&read("categories.json"), &read("palettes.json"));
    // Swift literals (PaletteTests): retro and minecraft are local-only; jazz is not
    assert!(local.palette_is_local_only("retro") && local.palette_is_local_only("minecraft"));
    assert!(local.palette_is_local_only("minecraft@32"));
    assert!(!local.palette_is_local_only("jazz") && !local.palette_is_local_only("studio"));
    for id in ["jazz", "synthwave", "orchestral", "studio"] {
        assert!(!local.palette_is_local_only(id), "{id}");
    }
    assert!(local.is_local_only_option("dls:33"));
    assert!(local.is_local_only_option("bank:minecraft.bass"));
    assert!(local.is_local_only_option("gs_instruments.dls#128:0"));
    assert!(local.is_local_only_option("minecraft-local.sf2#128:127"));
    assert!(!local.is_local_only_option("gm:0"));
    assert!(!local.is_local_only_option("MuseScore_General.sf2#128:0"));

    let base = publish_session_fixture();
    let spec = TrackSpec::new("house", 3.0, Some("minimal"));
    let with_tracks = |palette: &str| {
        let mut sp = spec.clone();
        sp.layers = JsonObject::from_pairs([(
            "tracks",
            JsonValue::Object(JsonObject::from_pairs([(
                "4",
                JsonValue::Object(JsonObject::from_pairs([(
                    "palette",
                    JsonValue::str(palette),
                )])),
            )])),
        )]);
        sp
    };
    let mut refused = Vec::new();
    let mut s = base.clone();
    s.decks
        .insert("A".into(), DeckSource::Net(with_tracks("retro")));
    refused.push(s);
    let mut s = base.clone();
    s.events = vec![act(
        1,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("retro".into()),
        },
    )];
    refused.push(s);
    let mut s = base.clone();
    s.events = vec![act(
        10,
        DeckAction::SetKit {
            deck: "A".into(),
            kit: Some("minecraft-local.sf2#128:127".into()),
        },
    )];
    refused.push(s);
    for s in &refused {
        assert!(!s.local_only_sounds(&local).is_empty());
        let r = SessionBundle::new(s, None, None, "x", false, false, &local);
        assert!(matches!(r, Err(PublishError::LocalOnlySounds(_))));
        let r = SessionBundle::new(s, None, None, "x", true, false, &local); // draft or not
        assert!(matches!(r, Err(PublishError::LocalOnlySounds(_))));
        assert!(SessionBundle::new(s, None, None, "x", false, true, &local).is_ok());
    }
    let mut fine = base.clone();
    fine.events = vec![act(
        1,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "melody".into(),
            voice: Some("gm:0".into()),
        },
    )];
    assert!(fine.local_only_sounds(&local).is_empty());
    let mut ok = base.clone();
    ok.decks
        .insert("A".into(), DeckSource::Net(with_tracks("jazz")));
    assert!(ok.local_only_sounds(&local).is_empty());
    let line = refused[0].local_only_sounds(&local);
    assert_eq!(line, ["deck A ch 4 palette retro"]);
}

#[test]
fn bundle_files_have_their_names() {
    let f = BundleFile {
        name: "a".into(),
        data: vec![],
    };
    assert_eq!(f, f.clone());
}

// SpecRender.swift: Session.playing
#[test]
fn a_spec_plays_from_the_first_frame_to_the_end_of_its_song() {
    let spec = TrackSpec::new("synthwave", 1984.0, Some("standard"));
    let p = spec.project();
    let steps: i64 = p.structure.iter().map(|s| s.steps).sum();
    let s = Session::playing(&spec, None, 2.0, 44_100.0).unwrap();
    let want = (steps as f64 * 60.0 / (p.tempo * 4.0) + 2.0) * 44_100.0;
    assert_eq!(s.frames, want.round() as i64);
    assert_eq!(s.events.len(), 1);
    assert_eq!(s.decks["A"], DeckSource::Net(spec.clone()));
    // a loop has no end: it needs seconds
    let looped = TrackSpec::new("synthwave", 1.0, None);
    assert!(Session::playing(&looped, None, 2.0, 44_100.0).is_none());
    assert_eq!(
        Session::playing(&looped, Some(10.0), 2.0, 22_050.0)
            .unwrap()
            .frames,
        220_500
    );
}
