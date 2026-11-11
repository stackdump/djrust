//! `SessionPlayer.render` on the Rust engine: the Swift suite's `SessionReplayTests`,
//! `RawNetsSessionTests` and `SessionStoreTests.exportsAWAVOfTheReplay` (parity form (a): each
//! test names the Swift test it ports). A replay is deterministic per engine, so the property
//! under test is the Swift tests' own: a set performed live and recorded replays to the same
//! samples, bit for bit, and two renders of one session are byte-identical. Audio is not
//! compared with Swift's (PORTING.md H); the state a Swift engine reaches is `journal_golden.rs`.

use std::sync::Arc;

use djrust::automix::{AutoMixPhase, AutoMixSettings, AutoMixState};
use djrust::deck::turntable::SampleBuffer;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, Session, SessionError, SessionEventKind, DEMO_ID};
use djrust::session::player::SessionPlayer;
use djrust::session::wav::{encode_wav, RenderedAudio};
use djrust::TrackSpec;
use djrust::host::deck::{OfflineConductor, PerformCtx};
use djrust::host::replay::{offline_conductor, render_session, RenderOptions};

const APC: &str = "APC mini mk2 Control";
const MPK: &str = "MPK Mini Mk II";
const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const RATE: f64 = 22_050.0;

fn perform(c: &mut OfflineConductor, a: DeckAction) {
    c.rig.engine.perform(&a, PerformCtx::default());
}

fn midi(c: &mut OfflineConductor, port: &str, bytes: &[u8]) {
    let now = c.now();
    c.rig.route(port, bytes, now);
}

fn play(deck: &str) -> DeckAction {
    DeckAction::Play {
        deck: deck.into(),
        on: true,
    }
}

// `OfflineConductor.housekeepingEvery` = `max(1, Int(sampleRate / 100))`: the poll every set is
// rendered with is the live hub's 100 Hz, whatever the rate (Swift's own literal: nothing else
// pins it, since a live run and its replay take the same steps either way)
#[test]
fn the_housekeeping_runs_at_100_hz() {
    for (rate, every) in [
        (48_000.0, 480),
        (44_100.0, 441),
        (22_050.0, 220),
        (11_025.0, 110),
    ] {
        let c = offline_conductor(vec!["A".into()], rate);
        assert_eq!(c.housekeeping_every(), every, "at {rate} Hz");
    }
}

/// The scripted set (`performance(_:_:block:)`): hardware (three controllers) and UI inputs, in
/// both deck kinds.
fn performance(c: &mut OfflineConductor, b: usize) {
    match b {
        0 => {
            for p in [APC, MPK, INPULSE] {
                c.rig.attach(p);
            }
            perform(c, play("A"));
            midi(c, INPULSE, &[0x92, 0x07, 0x7F]); // deck B PLAY
        }
        20 => midi(c, APC, &[0x90, 48, 0x7F]), // deck A macro: drop
        60 => {
            perform(
                c,
                DeckAction::Param {
                    deck: "B".into(),
                    name: "filter".into(),
                    value: 0.2,
                },
            );
            perform(c, DeckAction::Crossfader(0.7));
        }
        80 => midi(c, INPULSE, &[0x92, 0x08, 0x7F]), // touch deck B
        82 | 84 | 86 | 88 | 90 => midi(c, INPULSE, &[0xB2, 0x0A, 0x05]), // scratch
        100 => midi(c, INPULSE, &[0x92, 0x08, 0x00]), // release
        104 => midi(c, INPULSE, &[0xB2, 0x09, 0x04]), // ring bend
        120 => {
            midi(c, APC, &[0x90, 18, 0x7F]); // deck A section 2
            midi(c, APC, &[0x90, 101, 0x7F]); // role: kick
        }
        160 => perform(
            c,
            DeckAction::Sync {
                deck: "B".into(),
                align: "beat".into(),
            },
        ),
        200 => {
            perform(
                c,
                DeckAction::Load {
                    deck: "A".into(),
                    source: DeckSource::Net(TrackSpec::new("house", 9.0, Some("minimal"))),
                },
            );
            perform(c, play("A"));
        }
        260 => perform(
            c,
            DeckAction::HotCue {
                deck: "B".into(),
                slot: 0,
            },
        ),
        300 => {
            perform(
                c,
                DeckAction::HotCue {
                    deck: "B".into(),
                    slot: 0,
                },
            );
            perform(
                c,
                DeckAction::Loop {
                    deck: "B".into(),
                    op: "in".into(),
                },
            );
        }
        330 => perform(
            c,
            DeckAction::Loop {
                deck: "B".into(),
                op: "out".into(),
            },
        ),
        350 => perform(
            c,
            DeckAction::Macro {
                deck: "A".into(),
                id: "beat-repeat".into(),
            },
        ),
        380 => {
            midi(c, MPK, &[0x99, 36, 100]); // pad 1: kick off on the aimed deck
            midi(c, MPK, &[0x90, 61, 110]); // live key
        }
        420 => midi(c, MPK, &[0x80, 61, 0]),
        440 => midi(c, APC, &[0x90, 59, 0x7F]), // panic
        _ => {}
    }
}

fn append(out: &mut RenderedAudio, l: &[f32], r: &[f32]) {
    out.left.extend_from_slice(l);
    out.right.extend_from_slice(r);
}

/// `SessionReplayTests.perform`: the set performed live (recording), its session and audio.
fn live(blocks: usize, auto_gain: bool) -> (Session, RenderedAudio) {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    {
        let e = &mut c.rig.engine;
        // the app turns auto-gain on before anything loads (AppModel.applyAutoGainPreference)
        if auto_gain {
            e.perform(
                &DeckAction::Param {
                    deck: "B".into(),
                    name: "autoGain".into(),
                    value: 1.0,
                },
                PerformCtx::default(),
            );
        }
        // compiled with the deck's options, as the app does and the replay will
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
        e.load_demo("B");
        e.set_volume("A", 0.85);
        e.start_session().unwrap();
    }
    let mut audio = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..blocks {
        let (l, r) = c.step_with(|c| performance(c, b));
        let (l, r) = (l.to_vec(), r.to_vec());
        append(&mut audio, &l, &r);
    }
    assert_eq!(c.dropped(), 0);
    (c.rig.engine.finish_session().unwrap(), audio)
}

fn render(s: &Session) -> RenderedAudio {
    render_session(s, &RenderOptions::default()).unwrap().audio
}

fn identical(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn peak(a: &[f32]) -> f32 {
    a.iter().fold(0f32, |p, v| p.max(v.abs()))
}

// SessionTests.SessionReplayTests.theReplayIsTheSameAudioAsTheLiveSet
#[test]
fn the_replay_is_the_same_audio_as_the_live_set() {
    let (session, live) = live(520, false);
    assert_eq!(session.frames, live.frames() as i64);
    assert!(
        session.events.len() >= 25,
        "{} events",
        session.events.len()
    );
    assert!(peak(&live.left) > 0.1);
    let replay = render(&session);
    assert_eq!(replay.frames(), live.frames());
    assert!(identical(&replay.left, &live.left));
    assert!(identical(&replay.right, &live.right));
}

// SessionTests.SessionReplayTests.theReplayIsTheSameAudioWithAutoGainOn: auto-gain on before the
// sample loads, so the live deck plays at the levelled gain from its first sample; the replay's
// starting state must too (no glide in)
#[test]
fn the_replay_is_the_same_audio_with_auto_gain_on() {
    let (session, live) = live(300, true);
    assert!(session.params["B"].auto_gain);
    assert_ne!(
        djrust::deck::level_match::SampleLevel::measure(&djrust::deck::demo_loop::make(RATE))
            .auto_gain_db(),
        0.0
    );
    let replay = render(&session);
    assert_eq!(replay.frames(), live.frames());
    assert!(identical(&replay.left, &live.left));
    assert!(identical(&replay.right, &live.right));
}

// SessionTests.SessionReplayTests.twoRendersAreByteIdentical
#[test]
fn two_renders_are_byte_identical() {
    let (session, _) = live(300, false);
    let a = encode_wav(&render(&session), true);
    let b = encode_wav(&render(&session), true);
    assert_eq!(a, b);
    assert_eq!(a.len(), 44 + session.frames as usize * 8);
}

// SessionTests.SessionReplayTests.aSessionSurvivesItsOwnEnvelope
#[test]
fn a_session_survives_its_own_envelope() {
    let (session, live) = live(300, false);
    let revived = Session::decode(&session.canonical(), Some(&session.cid())).unwrap();
    assert_eq!(revived, session);
    let replay = render(&revived);
    assert!(identical(&replay.left, &live.left[..replay.frames()]));
}

// SessionTests.SessionReplayTests.theRecordedInputsAreWhatWasDone
#[test]
fn the_recorded_inputs_are_what_was_done() {
    let (s, _) = live(520, false);
    let kinds: Vec<&SessionEventKind> = s.events.iter().map(|e| &e.kind).collect();
    assert!(kinds.contains(&&SessionEventKind::Midi {
        port: APC.into(),
        bytes: vec![0x90, 48, 0x7F]
    }));
    assert!(kinds.contains(&&SessionEventKind::Action(DeckAction::Sync {
        deck: "B".into(),
        align: "beat".into()
    })));
    assert!(
        kinds.contains(&&SessionEventKind::Action(DeckAction::Macro {
            deck: "A".into(),
            id: "beat-repeat".into()
        }))
    );
    let frames: Vec<i64> = s.events.iter().map(|e| e.frame).collect();
    let mut sorted = frames.clone();
    sorted.sort();
    assert_eq!(frames, sorted);
    let key = s
        .events
        .iter()
        .find(|e| {
            e.kind
                == SessionEventKind::Midi {
                    port: MPK.into(),
                    bytes: vec![0x90, 61, 110],
                }
        })
        .unwrap();
    assert_eq!(key.frame, 380 * 128);
}

// SessionTests.SessionReplayTests.differentInputsGiveDifferentAudio
#[test]
fn different_inputs_give_different_audio() {
    let (session, live) = live(300, false);
    let mut changed = session.clone();
    let i = changed
        .events
        .iter()
        .position(|e| e.kind == SessionEventKind::Action(DeckAction::Crossfader(0.7)))
        .unwrap();
    changed.events[i].kind = SessionEventKind::Action(DeckAction::Crossfader(0.1));
    let other = render(&changed);
    assert!(!identical(&other.left, &live.left[..other.frames()]));
}

/// "A file": not the demo loop, so it is named by its content id.
fn file_buffer() -> Arc<SampleBuffer> {
    let l: Vec<f32> = (0..44_100)
        .map(|i| ((i as f64 * 0.05).sin() as f32) * 0.4 * if i % 5512 < 3000 { 1.0 } else { 0.2 })
        .collect();
    Arc::new(SampleBuffer::new(l, None, RATE))
}

// SessionTests.SessionReplayTests.audioLoadedMidSetIsNamedByContentAndReplaysWithItsLibrary
#[test]
fn audio_loaded_mid_set_is_named_by_content_and_replays_with_its_library() {
    let file = file_buffer();
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    c.rig.engine.start_session().unwrap();
    let mut out = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..250 {
        let f = file.clone();
        let (l, r) = c.step_with(|c| {
            if b == 5 {
                c.rig.engine.perform(
                    &DeckAction::Load {
                        deck: "A".into(),
                        source: DeckSource::Sample {
                            id: String::new(),
                            title: "file".into(),
                            grid: None,
                        },
                    },
                    PerformCtx {
                        prepared: None,
                        buffer: Some(f),
                    },
                );
                perform(c, play("A"));
            }
            if b == 120 {
                perform(
                    c,
                    DeckAction::Load {
                        deck: "B".into(),
                        source: DeckSource::Sample {
                            id: DEMO_ID.into(),
                            title: "demo".into(),
                            grid: None,
                        },
                    },
                );
            }
            if b == 125 {
                perform(c, play("B"));
            }
        });
        out.left.extend_from_slice(l);
        out.right.extend_from_slice(r);
    }
    let session = c.rig.engine.finish_session().unwrap();
    assert_eq!(session.decks["A"], DeckSource::None);
    let named: Vec<String> = session
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(DeckAction::Load {
                source: DeckSource::Sample { id, .. },
                ..
            }) => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(named.first(), Some(&file.content_id()));
    assert_eq!(named.last().map(String::as_str), Some("demo-loop"));
    let kept = c.rig.engine.loops.row.sample_library[&file.content_id()].clone();
    assert!(
        Arc::ptr_eq(&kept, &file),
        "the library holds the very buffer"
    );
    let library = c.rig.engine.loops.row.sample_library.clone();
    let opts = RenderOptions::default().with_samples(move |id| library.get(id).cloned());
    let replay = render_session(&session, &opts).unwrap().audio;
    assert!(identical(&replay.left, &out.left));
    assert!(peak(&out.left) > 0.1);
    assert_eq!(
        render_session(&session, &RenderOptions::default()).err(),
        Some(SessionError::MissingSample {
            id: file.content_id()
        })
    );
}

/// The same tone as `file_buffer`, stored at 44.1 kHz: a second of it, where the engine runs at
/// 22.05 kHz.
fn file_at_44100() -> Arc<SampleBuffer> {
    let l: Vec<f32> = (0..44_100)
        .map(|i| {
            ((i as f64 * 0.025).sin() as f32) * 0.4 * if i % 11_025 < 6_000 { 1.0 } else { 0.2 }
        })
        .collect();
    Arc::new(SampleBuffer::new(l, None, 44_100.0))
}

// Swift's turntable takes no other rate ("resample before loading"): `Deck.decode` converts a file
// to the engine's rate, and what is loaded is that. Audio that arrives at another rate is
// converted the same way on load, named by the id of what came in (the recorded session and the
// journal name the original, which a replay is handed again), and the converted audio is what the
// library holds under that id. No Swift test covers it (Swift traps): form (a) from the source.
#[test]
fn audio_at_another_rate_is_converted_on_load_and_replays_to_the_byte() {
    let file = file_at_44100();
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    c.rig.engine.start_session().unwrap();
    let mut out = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..200 {
        let f = file.clone();
        let (l, r) = c.step_with(|c| {
            if b == 5 {
                let applied = c.rig.engine.perform(
                    &DeckAction::Load {
                        deck: "A".into(),
                        source: DeckSource::Sample {
                            id: String::new(),
                            title: "file".into(),
                            grid: None,
                        },
                    },
                    PerformCtx {
                        prepared: None,
                        buffer: Some(f),
                    },
                );
                assert_eq!(applied, djrust::host::deck::Applied::Done, "it loads");
                perform(c, play("A"));
            }
        });
        out.left.extend_from_slice(l);
        out.right.extend_from_slice(r);
    }
    assert!(peak(&out.left) > 0.1, "it plays");
    let session = c.rig.engine.finish_session().unwrap();
    let id = file.content_id();
    let named: Vec<String> = session
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(DeckAction::Load {
                source: DeckSource::Sample { id, .. },
                ..
            }) => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(named, vec![id.clone()], "named by what came in");
    // the deck holds the converted audio, the library holds it under the same name
    let held = c
        .rig
        .engine
        .deck("A")
        .unwrap()
        .sample
        .as_ref()
        .unwrap()
        .buffer
        .clone();
    assert_eq!(held.sample_rate, RATE);
    assert_eq!(held.length(), 22_050);
    assert!(Arc::ptr_eq(
        &c.rig.engine.loops.row.sample_library[&id],
        &held
    ));
    assert_eq!(
        c.rig.engine.sample_id(&held),
        id,
        "a checkpoint names it by that id"
    );
    assert!(Arc::ptr_eq(
        &c.rig.engine.resolve_sample(&id).unwrap(),
        &held
    ));
    // a replay handed the original comes to the same samples, and so does one handed the library's
    let original = file.clone();
    let from_original = RenderOptions::default()
        .with_samples(move |i| (i == original.content_id()).then(|| original.clone()));
    let replay = render_session(&session, &from_original).unwrap().audio;
    assert!(identical(&replay.left, &out.left) && identical(&replay.right, &out.right));
    let library = c.rig.engine.loops.row.sample_library.clone();
    let from_library = RenderOptions::default().with_samples(move |i| library.get(i).cloned());
    let again = render_session(&session, &from_library).unwrap().audio;
    assert!(identical(&again.left, &out.left));
}

// SessionTests.SessionReplayTests.aMissingSampleIsReportedNotInvented
#[test]
fn a_missing_sample_is_reported_not_invented() {
    let mut s = Session::new(
        RATE,
        1024,
        [(
            "B".to_string(),
            DeckSource::Sample {
                id: "deadbeef".into(),
                title: "x".into(),
                grid: None,
            },
        )]
        .into(),
        Default::default(),
        0.5,
        0.8,
        Vec::new(),
    );
    assert_eq!(
        render_session(&s, &RenderOptions::default()).err(),
        Some(SessionError::MissingSample {
            id: "deadbeef".into()
        })
    );
    let buf = Arc::new(djrust::deck::demo_loop::make(RATE));
    let opts =
        RenderOptions::default().with_samples(move |id| (id == "deadbeef").then(|| buf.clone()));
    let ok = render_session(&s, &opts).unwrap().audio;
    assert_eq!(ok.frames(), 1024);
    // (and the demo loop is built in)
    s.decks.insert(
        "B".into(),
        DeckSource::Sample {
            id: DEMO_ID.into(),
            title: "demo".into(),
            grid: None,
        },
    );
    assert_eq!(render(&s).frames(), 1024);
}

fn share_payloads() -> (JsonValue, JsonValue) {
    let path = format!(
        "{}/tests/fixtures/js/share-urls.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    let rows = doc.get("rawNets").and_then(JsonValue::as_array).unwrap();
    let hand_made = rows
        .iter()
        .find(|r| {
            let p = r.get("payload");
            p.and_then(|p| p.get("name")).and_then(JsonValue::as_str) == Some("Hand Made")
                && p.and_then(|p| p.get("genre")).is_some()
        })
        .and_then(|r| r.get("payload"))
        .unwrap()
        .clone();
    // nameless, needs arrange (lofi, structure standard): it has sections
    let arranged = rows
        .iter()
        .find(|r| r.get("needsArrange").and_then(JsonValue::as_bool) == Some(true))
        .and_then(|r| r.get("payload"))
        .unwrap()
        .clone();
    (hand_made, arranged)
}

// SessionTests.RawNetsSessionTests.aRawNetsSetReplaysToTheByte
#[test]
fn a_raw_nets_set_replays_to_the_byte() {
    let (hand_made, arranged) = share_payloads();
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    {
        let e = &mut c.rig.engine;
        e.perform(
            &DeckAction::Load {
                deck: "A".into(),
                source: DeckSource::RawNets(hand_made.clone()),
            },
            PerformCtx::default(),
        );
        e.load_demo("B");
        e.start_session().unwrap();
    }
    let mut out = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..320 {
        let load_b = DeckAction::Load {
            deck: "B".into(),
            source: DeckSource::RawNets(arranged.clone()),
        };
        let (l, r) = c.step_with(|c| match b {
            0 => perform(c, play("A")),
            60 => perform(c, DeckAction::Crossfader(0.4)),
            100 => {
                perform(c, load_b);
                perform(c, play("B"));
            }
            180 => perform(
                c,
                DeckAction::Section {
                    deck: "B".into(),
                    index: 1,
                },
            ),
            240 => perform(
                c,
                DeckAction::Role {
                    deck: "A".into(),
                    role: "kick".into(),
                    on: false,
                },
            ),
            _ => {}
        });
        out.left.extend_from_slice(l);
        out.right.extend_from_slice(r);
    }
    let session = c.rig.engine.finish_session().unwrap();
    assert_eq!(session.decks["A"], DeckSource::RawNets(hand_made.clone()));
    assert!(session.events.iter().any(|e| e.kind
        == SessionEventKind::Action(DeckAction::Load {
            deck: "B".into(),
            source: DeckSource::RawNets(arranged.clone())
        })));
    assert!(
        c.rig.engine.deck("B").unwrap().sections.len() > 1,
        "the arranged share has sections"
    );
    assert!(peak(&out.left) > 0.05);
    // the envelope names the share; it reads back, CID checked, to the same session
    let json = session.json();
    let source = json
        .get("decks")
        .unwrap()
        .get("A")
        .unwrap()
        .get("source")
        .unwrap();
    assert_eq!(
        source.get("kind").and_then(JsonValue::as_str),
        Some("rawNets")
    );
    assert_eq!(
        source.get("cid").and_then(JsonValue::as_str),
        Some(djrust::cid::share_cid(&hand_made).as_str())
    );
    let revived = Session::decode(&session.canonical(), Some(&session.cid())).unwrap();
    assert_eq!(revived, session);
    let replay = render(&revived);
    assert_eq!(replay.frames(), out.frames());
    assert!(identical(&replay.left, &out.left));
    assert!(identical(&replay.right, &out.right));
    let again = render(&revived);
    assert_eq!(
        encode_wav(&again, true),
        encode_wav(&replay, true),
        "byte-identical"
    );
}

// SessionTests.SessionStoreTests.exportsAWAVOfTheReplay
#[test]
fn exports_a_wav_of_the_replay() {
    use djrust::host::stores::sessions::SessionStore;
    let dir = std::env::temp_dir().join(format!("deck-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (session, live) = live(200, false);
    let path = dir.join("set.wav");
    SessionStore::export_wav(&session, &path, true, &RenderOptions::default()).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes, encode_wav(&live, true));
    // a set that names audio nobody has is refused, and nothing is written
    let mut missing = session.clone();
    missing.decks.insert(
        "B".into(),
        DeckSource::Sample {
            id: "deadbeef".into(),
            title: "x".into(),
            grid: None,
        },
    );
    let other = dir.join("missing.wav");
    assert!(SessionStore::export_wav(&missing, &other, false, &RenderOptions::default()).is_err());
    assert!(!other.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// WanderTests.WanderReplayTests.aReplayWalksTheSamePath: a set with wander on, at a fast tempo so
// phrases go by: performed live (recorded), then replayed. The moves are derived, not recorded:
// the replay's walk is the live one, and it sounds the same.
#[test]
fn a_replay_walks_the_same_path() {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let mut spec = TrackSpec::new("techno", 42.0, Some("standard"));
    spec.extra
        .set("bpm", djrust::json::JsonValue::Number(240.0));
    {
        let e = &mut c.rig.engine;
        assert!(e.generate("A", &spec));
        e.load_demo("B");
        // a lock set before the recording starts is part of its starting state
        e.set_wander_lock("A", "genre", true);
        e.set_wander_lock("A", "instruments", true);
        e.start_session().unwrap();
    }
    let mut live = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..1400usize {
        let (l, r) = c.step_with(|c| match b {
            0 => {
                perform(
                    c,
                    DeckAction::WanderPace {
                        deck: "A".into(),
                        bars: 4,
                    },
                );
                perform(
                    c,
                    DeckAction::Wander {
                        deck: "A".into(),
                        on: true,
                    },
                );
                perform(c, play("A"));
            }
            700 => perform(
                c,
                DeckAction::WanderLock {
                    deck: "A".into(),
                    dimension: "fx".into(),
                    on: true,
                },
            ),
            _ => {}
        });
        live.left.extend_from_slice(l);
        live.right.extend_from_slice(r);
    }
    let log = c.rig.engine.deck("A").unwrap().walk.wander.log.clone();
    let session = c.rig.engine.finish_session().unwrap();
    assert!(
        log.len() >= 2,
        "{:?}",
        log.iter().map(|m| &m.detail).collect::<Vec<_>>()
    );
    assert_eq!(
        session.params["A"].wander.locks.iter().collect::<Vec<_>>(),
        ["genre", "instruments"]
    );
    assert!(
        !session
            .events
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::Action(DeckAction::Macro { .. }))),
        "the moves are derived, not recorded"
    );
    let replay = render_session(&session, &RenderOptions::default()).unwrap();
    assert_eq!(replay.dropped, 0);
    assert_eq!(replay.wander_logs["A"], log);
    assert_eq!(replay.audio.frames(), live.frames());
    assert!(
        identical(&replay.audio.left, &live.left),
        "and sounds the same"
    );
    assert!(identical(&replay.audio.right, &live.right));
}

// SessionPlayer.roleFilter (RenderStemTests' engine half): roles left out of every net deck's
// render, applied when a deck compiles: the session, and its CID, are untouched
#[test]
fn a_role_filter_leaves_roles_out_of_the_render_not_the_session() {
    use djrust::wave::RoleFilter;
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let session = Session::playing(&spec, Some(3.0), 2.0, RATE).unwrap();
    let cid = session.cid();
    let whole = render(&session);
    assert!(peak(&whole.left) > 0.05);
    let with = |f: RoleFilter| {
        render_session(
            &session,
            &RenderOptions {
                role_filter: Some(f),
                samples: None,
            },
        )
        .unwrap()
        .audio
    };
    let no_kick = with(RoleFilter {
        mute: vec!["kick".into()],
        solo: None,
    });
    let bass_only = with(RoleFilter {
        mute: Vec::new(),
        solo: Some(vec!["bass".into()]),
    });
    assert_eq!(no_kick.frames(), whole.frames());
    assert!(!identical(&no_kick.left, &whole.left), "the kick is gone");
    assert!(
        !identical(&bass_only.left, &no_kick.left) && peak(&bass_only.left) < peak(&whole.left),
        "one role is less than the whole"
    );
    assert_eq!(session.cid(), cid, "the session is untouched");
    // an empty filter is no filter
    let none = render_session(
        &session,
        &RenderOptions {
            role_filter: Some(RoleFilter {
                mute: Vec::new(),
                solo: None,
            }),
            samples: None,
        },
    )
    .unwrap()
    .audio;
    assert!(identical(&none.left, &whole.left));
}

// MARK: auto-mix in a set (AutoMixTests.AutoMixSessionTests)

/// `AutoMixRig`'s offline two-deck set: A plays a composed net, B holds the demo loop, auto-mix
/// set as `autoMix` before the recorder starts (so the session's start carries it).
fn auto_mix_set(spec: &TrackSpec, settings: Option<(bool, i64, i64)>) -> OfflineConductor {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let e = &mut c.rig.engine;
    assert!(e.generate("A", spec));
    assert!(e.load_demo("B"));
    if let Some((on, bars, every)) = settings {
        e.set_auto_mix(on, bars, every);
    }
    e.start_session().unwrap();
    c
}

/// One block, `a` performed first (as a set's pokes are), its samples appended to `audio`.
fn block(c: &mut OfflineConductor, audio: &mut RenderedAudio, a: Option<DeckAction>) {
    let (l, r) = c.step_with(|c| {
        if let Some(a) = a {
            perform(c, a)
        }
    });
    audio.left.extend_from_slice(l);
    audio.right.extend_from_slice(r);
}

/// `AutoMixRig.run(until:)`: blocks until `done` (at most 40 000), the first one after `a`.
fn run_until(
    c: &mut OfflineConductor,
    audio: &mut RenderedAudio,
    mut a: Option<DeckAction>,
    done: impl Fn(&OfflineConductor) -> bool,
) -> bool {
    let mut n = 0;
    while !done(c) && n < 40_000 {
        block(c, audio, a.take());
        n += 1;
    }
    done(c)
}

fn silence() -> RenderedAudio {
    RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    }
}

// AutoMixTests.AutoMixSessionTests.aSetWithAutoMixReplaysToTheByte: two hand-offs (A to B, then
// B to A), recorded with the settings the set started with; the replay stands auto-mix from the
// session's start and re-applies the two pokes, and is the live set to the bit
#[test]
fn a_set_with_auto_mix_replays_to_the_byte() {
    let mut c = auto_mix_set(&TrackSpec::new("dnb", 42.0, None), Some((true, 4, 0)));
    let mut live = silence();
    block(&mut c, &mut live, Some(play("A")));
    for _ in 0..3 {
        block(&mut c, &mut live, None);
    }
    // two hand-offs: A to B, then B to A
    assert!(run_until(
        &mut c,
        &mut live,
        Some(DeckAction::AutoMixNow),
        |c| c.engine().auto_mix.log.len() == 1
    ));
    assert!(run_until(
        &mut c,
        &mut live,
        Some(DeckAction::AutoMixNow),
        |c| c.engine().auto_mix.log.len() == 2
    ));
    for _ in 0..200 {
        block(&mut c, &mut live, None);
    }
    assert_eq!(c.dropped(), 0);
    let log = &c.engine().auto_mix.log;
    assert_eq!(
        log.iter()
            .map(|h| (h.from.as_str(), h.to.as_str(), h.n))
            .collect::<Vec<_>>(),
        [("A", "B", 0), ("B", "A", 1)]
    );
    let (a, b) = (c.engine().deck("A").unwrap(), c.engine().deck("B").unwrap());
    assert!(a.playing && !b.playing);
    let session = c.rig.engine.finish_session().unwrap();
    assert_eq!(session.auto_mix.settings, AutoMixSettings::new(true, 4, 0));
    let actions: Vec<DeckAction> = session
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        [play("A"), DeckAction::AutoMixNow, DeckAction::AutoMixNow],
        "only the pokes"
    );
    let back = Session::decode(&session.canonical(), Some(&session.cid())).unwrap();
    assert_eq!(back, session);
    let x = render(&back);
    let y = render(&back);
    assert!(identical(&x.left, &live.left), "the replay is the live set");
    assert!(
        identical(&x.right, &live.right),
        "the replay is the live set"
    );
    assert!(identical(&x.left, &y.left), "two renders, same bytes");
    assert!(identical(&x.right, &y.right), "two renders, same bytes");
    assert!(peak(&x.left) > 0.05, "it plays");
}

// the starting state of a replay is the live engine's shape: a session that began with auto-mix
// on and two hand-offs behind it replays with the same settings and counter standing, and the
// pokes it records reach the same engine (a replay without them would hand off nowhere)
#[test]
fn a_replay_stands_auto_mix_from_the_sessions_start() {
    let spec = TrackSpec::new("dnb", 42.0, None);
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    {
        let e = &mut c.rig.engine;
        assert!(e.generate("A", &spec));
        assert!(e.load_demo("B"));
        e.set_auto_mix(true, 16, 4);
        e.auto_mix.handoffs = 3;
        e.start_session().unwrap();
    }
    let mut live = silence();
    for _ in 0..40 {
        block(&mut c, &mut live, None);
    }
    let session = c.rig.engine.finish_session().unwrap();
    assert_eq!(
        session.auto_mix,
        AutoMixState::with_start(AutoMixSettings::new(true, 16, 4), 3)
    );
    let mut r = djrust::host::replay::session_engine(&session, &RenderOptions::default()).unwrap();
    let e = r.engine();
    assert_eq!(e.auto_mix.settings, AutoMixSettings::new(true, 16, 4));
    assert_eq!(e.auto_mix.handoffs, 3);
    assert_eq!(e.auto_mix.phase, AutoMixPhase::Playing);
    let a = SessionPlayer::new(&session).drive(&mut r);
    assert!(identical(&a.left, &live.left), "and plays the same");
}

// AutoMixTests.AutoMixSessionTests.aDeferIsRecordedAndReplayed: live, the first compose never
// lands and the engine defers the hand-off a phrase (recorded as `autoMixDefer`); the replay
// (inline composes, never late) defers at the same frame and walks the same way
#[test]
fn a_defer_is_recorded_and_replayed() {
    let mut c = auto_mix_set(&TrackSpec::new("dnb", 42.0, None), None);
    {
        let e = c.engine_mut();
        e.deterministic = false; // live: composes on a worker thread
        e.auto_mix_host.compose_stalls = true; // the first compose never lands
    }
    let mut audio = silence();
    perform(
        &mut c,
        DeckAction::AutoMix {
            on: true,
            bars: 4,
            every: 0,
        },
    );
    block(&mut c, &mut audio, Some(play("A")));
    for _ in 0..3 {
        block(&mut c, &mut audio, None);
    }
    // to the mix-out point: the incoming is not there, the hand-off moves a phrase on
    assert!(run_until(
        &mut c,
        &mut audio,
        Some(DeckAction::AutoMixNow),
        |c| c
            .engine()
            .auto_mix
            .run
            .as_ref()
            .is_some_and(|r| r.handoff_bar == 16)
    ));
    // the second compose (asked for at the defer) lands while the set waits
    c.engine_mut().wait_auto_mix();
    assert_eq!(
        c.engine().auto_mix.run.as_ref().map(|r| r.incoming_ready),
        Some(true)
    );
    while !c.engine().auto_mix_host.ahead.is_finished() {
        std::thread::yield_now();
    }
    assert!(run_until(&mut c, &mut audio, None, |c| !c
        .engine()
        .auto_mix
        .log
        .is_empty()));
    let first = c.engine().auto_mix.log[0].clone();
    assert_eq!(first.bar, 16);
    let session = c.rig.engine.finish_session().unwrap();
    let defers = session
        .events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Action(DeckAction::AutoMixDefer { handoff: 0 }))
        .count();
    assert_eq!(defers, 1);
    // the replay (inline composes, never late) defers at the same frame and walks the same way
    let mut r = djrust::host::replay::session_engine(&session, &RenderOptions::default()).unwrap();
    SessionPlayer::new(&session).drive(&mut r);
    let log = &r.engine().auto_mix.log;
    assert_eq!(log.first().map(|h| (h.bar, h.n)), Some((16, 0)));
    assert_eq!(log.first().map(|h| &h.spec), Some(&first.spec));
}

/// The loop channel's trim as each block of a replay finds it (`SessionPlayer.onBlock` reading
/// `dj.loopChannel.gain`): the engine `session_engine` builds, stepped until the session's frames.
fn loop_gain_per_block(session: &Session) -> Vec<f64> {
    let mut c = djrust::host::replay::session_engine(session, &RenderOptions::default()).unwrap();
    let mut seen = Vec::new();
    while (seen.len() * OfflineConductor::BLOCK_FRAMES) < session.frames as usize {
        c.step_block();
        seen.push(c.engine().loops.gain);
    }
    seen
}

// LoopChannelLevelTests.aSessionRecordedBeforeTheChangeKeepsTheOldTrim: a session written before
// L's trim was recorded has no `mixer.loopGain`: it decodes to the unity it was played at,
// re-encodes to the same text and CID, and replays L at that trim.
#[test]
fn a_session_recorded_before_the_loop_trim_replays_the_old_trim() {
    use djrust::session::model::LEGACY_LOOP_GAIN;
    // Swift's literal
    let old = concat!(
        r#"{"decks":{"A":{"params":{"autoAdvance":false,"eqHigh":0.5,"eqLow":0.5,"eqMid":0.5,"#,
        r#""filter":0.5,"gain":0.5,"pitch":0,"quantize":"off","volume":0.8},"source":{"kind":"none"}}},"#,
        r#""events":[],"frames":4410,"mixer":{"crossfader":0.5,"master":0.8},"sampleRate":22050,"#,
        r#""schema":"djrust/session/v1"}"#
    );
    let s = Session::decode(old, None).unwrap();
    assert!(s.loop_gain == LEGACY_LOOP_GAIN && s.loop_gain == 0.5);
    assert!(s.json().get("mixer").unwrap().get("loopGain").is_none());
    assert_eq!(s.canonical(), JsonValue::parse(old).unwrap().canonical_js());
    assert_eq!(Session::decode(&s.canonical(), Some(&s.cid())).unwrap(), s);
    let replayed = loop_gain_per_block(&s);
    assert!(!replayed.is_empty() && replayed.iter().all(|g| *g == 0.5), "{replayed:?}");
}

// LoopChannelLevelTests.aNewSessionRecordsTheLoopChannelsTrim: a new recording carries L's trim,
// on the wire and back through a replay; a trim the user moved records as it was.
#[test]
fn a_new_session_records_the_loop_channels_trim_and_replays_it() {
    use djrust::host::deck::loops::LOOP_CHANNEL_GAIN;
    let mut c = offline_conductor(vec!["A".into()], RATE);
    // a few blocks long: a session's length is the frames the engine rendered while it was
    // recording, and a zero-frame session replays no block to read L's trim from
    c.engine_mut().start_session().unwrap();
    for _ in 0..8 {
        c.step_block();
    }
    let s = c.engine_mut().finish_session().unwrap();
    assert!(s.frames > 0);
    assert_eq!(s.loop_gain, LOOP_CHANNEL_GAIN);
    assert_eq!(
        s.json().get("mixer").unwrap().get("loopGain").and_then(JsonValue::as_f64),
        Some(LOOP_CHANNEL_GAIN)
    );
    let back = Session::decode(&s.canonical(), Some(&s.cid())).unwrap();
    assert_eq!(back.loop_gain, LOOP_CHANNEL_GAIN);
    // a trim the user moved records as it was
    c.engine_mut().set_loop_gain(0.7);
    c.engine_mut().start_session().unwrap();
    assert_eq!(c.engine_mut().finish_session().unwrap().loop_gain, 0.7);
    let replayed = loop_gain_per_block(&back);
    assert!(
        !replayed.is_empty() && replayed.iter().all(|g| *g == LOOP_CHANNEL_GAIN),
        "{replayed:?}"
    );
    // (beyond Swift's literal: the default is also what a replay that ignored the trim would
    // play, so a moved one shows that the replay reads it)
    let mut moved = back.clone();
    moved.loop_gain = 0.7;
    let replayed = loop_gain_per_block(&moved);
    assert!(!replayed.is_empty() && replayed.iter().all(|g| *g == 0.7), "{replayed:?}");
}
