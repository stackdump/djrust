//! The journal: `JournalCodecTests`, `JournalCheckpointTests` (the codec and the pure
//! bookkeeping; replays of a live engine need the engine), the notes / store parts of
//! `JournalFileTests`, and the replay planning of `JournalReplay.swift` driven by a fake
//! engine. Asserts marked "Swift literal" are the Swift suites' own.

use std::collections::BTreeMap;

use djrust::journal::event::{action_summary, JournalEntry, JournalEvent};
use djrust::journal::file::{self, JournalRecorder};
use djrust::journal::replay::{self, ReplayEngine, ReplayOptions};
use djrust::journal::state::{
    ClipCheckpoint, DeckCheckpoint, JournalState, LoopScene, VoiceSetting,
};
use djrust::journal::stores::{
    all_tags, favorites_adding, notes_on, notes_on_favorite, tagged, Favorite, JournalStoreState,
    NotePosition, NoteTarget, TrackNote, VoiceVariation,
};
use djrust::journal::time;
use djrust::journal::{parse, JournalError};
use djrust::json::JsonValue;
use djrust::session::types::{AutoMixSettings, AutoMixState, SceneClip, VoiceTweak};
use djrust::session::{DeckAction, DeckParams, DeckSource, SessionEventKind};
use djrust::spec::TrackSpec;

const APC: &str = "APC mini mk2 Control";

fn spec() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("standard"))
}

/// `journalDate(s)`: a stamp at the millisecond.
fn jd(s: f64) -> i64 {
    time::rounded_ms(s)
}
/// `wholeSecond(s)`.
fn ws(s: f64) -> i64 {
    libm::round(s) as i64 * 1000
}

fn note() -> TrackNote {
    let mut n = TrackNote::new(
        "n-1",
        NoteTarget::Track {
            cid: djrust::cid::share_cid(&spec().payload()),
        },
        "drop hits here",
        jd(1_760_000_000.123),
    );
    n.title = "Neon Vale".into();
    n.position = Some(NotePosition::new(17, 1, Some(31.75)));
    n.edited = Some(jd(1_760_000_100.5));
    n
}

fn every_event() -> Vec<JournalEvent> {
    let mut s = JournalState::new(48_000.0);
    let mut a = DeckCheckpoint::new(DeckSource::Net(spec()));
    a.playing = true;
    a.leader = Some("B".into());
    a.voices.insert(
        3,
        VoiceSetting {
            tweak: VoiceTweak::default(),
            use_synth: true,
            instrument: "laser".into(),
        },
    );
    s.decks.insert("A".into(), a);
    s.decks.insert(
        "B".into(),
        DeckCheckpoint::new(DeckSource::Sample {
            id: "demo-loop".into(),
            title: "demo".into(),
            grid: None,
        }),
    );
    let mut clip = ClipCheckpoint::new("abc", "A loop 1");
    clip.color = Some("red".into());
    clip.gain = 0.5;
    clip.muted = true;
    clip.key_lock = Some(true);
    clip.quantum = Some(16);
    clip.fade = Some("bar".into());
    clip.looping = true;
    s.clips = vec![clip];
    s.scenes = vec![LoopScene {
        name: "Scene 1".into(),
        clips: vec![SceneClip {
            clip_id: "abc".into(),
            gain: 0.5,
            fx: None,
        }],
    }];
    s.removed = vec![("def".into(), 2)];
    s.locked = vec!["A.play".into()];
    s.stores = JournalStoreState {
        favorites: vec![Favorite::new(
            "x",
            DeckSource::Net(spec()),
            ws(1_700_000_000.0),
        )],
        crates: vec!["intro".into()],
        variations: BTreeMap::from([(
            "laser".to_string(),
            vec![VoiceVariation::new("zap", VoiceTweak::default(), false)],
        )]),
        notes: vec![note()],
    };
    let mut sample_fav = Favorite::new(
        "loop",
        DeckSource::Sample {
            id: String::new(),
            title: "loop".into(),
            grid: None,
        },
        ws(1_700_000_001.0),
    );
    sample_fav.file = Some("/x.caf".into());
    sample_fav.tags = vec!["a".into(), "b".into()];
    let mut n2 = TrackNote::new(
        "n-2",
        NoteTarget::Sample {
            id: "abc".into(),
            file: Some("/x.caf".into()),
        },
        "no place",
        jd(1_760_000_000.0),
    );
    n2.title = String::new();
    let n3 = TrackNote::new("n-3", NoteTarget::Clip { id: "abc".into() }, "c", jd(1.0));
    let d = |x: &str| x.to_string();
    vec![
        JournalEvent::Checkpoint(s.clone()),
        JournalEvent::Action(DeckAction::Play {
            deck: d("A"),
            on: true,
        }),
        JournalEvent::Action(DeckAction::ClipMove {
            id: d("abc"),
            before: Some(d("def")),
        }),
        JournalEvent::Action(DeckAction::ClipMove {
            id: d("abc"),
            before: None,
        }),
        JournalEvent::Action(DeckAction::Lock {
            key: d("A.hotcue.1"),
            on: true,
        }),
        JournalEvent::Action(DeckAction::Lock {
            key: d("A.hotcue.1"),
            on: false,
        }),
        JournalEvent::Action(DeckAction::ClipQuantize {
            id: d("abc"),
            beats: Some(8),
        }),
        JournalEvent::Action(DeckAction::ClipFade {
            id: d("abc"),
            mode: None,
        }),
        JournalEvent::Midi {
            port: d(APC),
            bytes: vec![0x90, 48, 0x7F],
        },
        JournalEvent::SampleFile {
            id: d("abc123"),
            path: d("/tmp/a b.wav"),
        },
        JournalEvent::LibraryRestore {
            id: d("abc"),
            index: 3,
        },
        JournalEvent::FavoriteAdd(sample_fav),
        JournalEvent::FavoriteRemove { cid: d("z4x") },
        JournalEvent::FavoriteTags {
            cid: d("z4x"),
            tags: vec![d("intro"), d("heavy")],
        },
        JournalEvent::FavoriteTags {
            cid: d("z4x"),
            tags: vec![],
        },
        JournalEvent::CrateAdd { name: d("set 1") },
        JournalEvent::CrateDelete { name: d("set 1") },
        JournalEvent::VariationSave {
            instrument: d("laser"),
            variation: VoiceVariation::new("short", VoiceTweak::default(), true),
        },
        JournalEvent::VariationRename {
            instrument: d("laser"),
            name: d("short"),
            to: d("shorter"),
        },
        JournalEvent::VariationDelete {
            instrument: d("laser"),
            name: d("shorter"),
        },
        JournalEvent::NoteAdd(note()),
        JournalEvent::NoteAdd(n2),
        JournalEvent::NoteAdd(n3),
        JournalEvent::NoteEdit {
            id: d("n-1"),
            text: d("the drop"),
            at: jd(1_760_000_200.25),
        },
        JournalEvent::NoteDelete { id: d("n-1") },
        JournalEvent::StoresReplace(s.stores.clone()),
        JournalEvent::StoresReplace(JournalStoreState::default()),
        JournalEvent::Mark(d("session.start")),
    ]
}

// JournalCodecTests.everyEventKindRoundTripsThroughItsLine
#[test]
fn every_event_kind_round_trips_through_its_line() {
    let events = every_event();
    let mut kinds: Vec<&str> = events.iter().map(JournalEvent::kind).collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds.len(), 18, "one of each kind: {kinds:?}"); // Swift literal
    for (i, ev) in events.iter().enumerate() {
        let e = JournalEntry {
            seq: i as i64,
            time: jd(1_760_000_000.5 + i as f64),
            frame: i as i64 * 128,
            event: ev.clone(),
        };
        let line = e.line();
        assert!(!line.contains('\n'));
        let back =
            JournalEntry::from_line(&line).unwrap_or_else(|| panic!("{} decodes", ev.kind()));
        assert_eq!(back, e, "{} round-trips", ev.kind());
        assert_eq!(
            back.line(),
            line,
            "{} re-encodes to the same bytes",
            ev.kind()
        );
    }
}

// JournalCodecTests.aLineCarriesTheFieldsTheFormatPromises
#[test]
fn a_line_carries_the_fields_the_format_promises() {
    let e = JournalEntry {
        seq: 7,
        time: jd(1_760_000_000.25),
        frame: 96_000,
        event: JournalEvent::Action(DeckAction::Param {
            deck: "A".into(),
            name: "volume".into(),
            value: 0.5,
        }),
    };
    let v = JsonValue::parse(&e.line()).unwrap();
    assert_eq!(v.get("v").unwrap().as_i64(), Some(1));
    assert_eq!(v.get("seq").unwrap().as_i64(), Some(7));
    assert_eq!(v.get("f").unwrap().as_i64(), Some(96_000));
    assert_eq!(v.get("k").unwrap().as_str(), Some("act"));
    assert_eq!(
        v.get("t").unwrap().as_str(),
        Some("2025-10-09T08:53:20.250Z")
    ); // Swift literal
    assert_eq!(
        v.get("a").unwrap().get("op").unwrap().as_str(),
        Some("param")
    );
}

#[test]
fn times_format_and_parse_as_iso_8601() {
    assert_eq!(time::string(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        time::string(jd(1_760_000_000.25)),
        "2025-10-09T08:53:20.250Z"
    );
    assert_eq!(time::string(-1), "1969-12-31T23:59:59.999Z");
    assert_eq!(time::string(951_782_400_000), "2000-02-29T00:00:00.000Z");
    for ms in [0, 1, 999, 1_760_000_000_250, 4_102_444_800_123, -86_400_000] {
        assert_eq!(time::date(&time::string(ms)), Some(ms));
    }
    assert_eq!(
        time::date("2025-10-09T08:53:20Z"),
        None,
        "fractional seconds are required"
    );
    assert_eq!(
        time::date_seconds("2025-10-09T08:53:20Z"),
        Some(1_760_000_000_000)
    );
    assert_eq!(time::date_seconds("2025-10-09T08:53:20.250Z"), None);
    assert_eq!(
        time::date("2025-10-09T10:53:20.250+02:00"),
        Some(jd(1_760_000_000.25))
    );
    assert_eq!(time::date("nonsense"), None);
    assert_eq!(
        time::string_seconds(1_700_000_000_999),
        "2023-11-14T22:13:20Z",
        "fractions drop, not round"
    );
    assert_eq!(
        time::file_stamp(jd(1_760_000_000.25)),
        "20251009T085320.250Z"
    );
}

// JournalCodecTests.aTornLastLineIsDroppedNotFatal
#[test]
fn a_torn_last_line_is_dropped_not_fatal() {
    let lines: Vec<String> = every_event()
        .into_iter()
        .take(4)
        .enumerate()
        .map(|(i, ev)| {
            JournalEntry {
                seq: i as i64,
                time: jd(i as f64),
                frame: i as i64,
                event: ev,
            }
            .line()
        })
        .collect();
    let whole = lines.join("\n") + "\n";
    assert_eq!(parse(&whole).entries.len(), 4);
    assert!(!parse(&whole).truncated);
    // the app stopped mid-write: the last line is cut short and has no newline
    let torn = &whole[..whole.len() - 9];
    let c = parse(torn);
    assert_eq!(c.entries.len(), 3);
    assert!(c.truncated);
    assert!(c.skipped.is_empty());
    assert!(c.checkpoint().is_some());
    // a bad line in the middle is skipped and reported, the rest still read
    let middle = [
        lines[0].as_str(),
        "{not json",
        lines[1].as_str(),
        r#"{"v":9,"seq":2,"f":0,"k":"from.the.future"}"#,
        lines[2].as_str(),
    ]
    .join("\n")
        + "\n";
    let m = parse(&middle);
    assert_eq!(
        m.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(m.skipped, [2, 4]);
    assert!(!m.truncated);
    assert_eq!(parse("").entries.len(), 0);
}

// JournalCodecTests.theStoreEventsDoWhatTheStoresDo
#[test]
fn the_store_events_do_what_the_stores_do() {
    let mut s = JournalStoreState::default();
    let f = Favorite::new("t", DeckSource::Net(spec()), ws(1.0));
    assert!(file::apply_store_event(
        &mut s,
        &JournalEvent::FavoriteAdd(f.clone())
    ));
    file::apply_store_event(
        &mut s,
        &JournalEvent::FavoriteTags {
            cid: f.cid(),
            tags: vec![" intro ".into(), "Intro".into(), "".into(), "heavy".into()],
        },
    );
    assert_eq!(s.favorites[0].tags, ["intro", "heavy"]);
    // starring again keeps the tags, as FavoritesStore.add does
    file::apply_store_event(
        &mut s,
        &JournalEvent::FavoriteAdd(Favorite::new("t2", DeckSource::Net(spec()), ws(2.0))),
    );
    assert!(
        s.favorites.len() == 1 && s.favorites[0].title == "t2" && s.favorites[0].tags.len() == 2
    );
    file::apply_store_event(
        &mut s,
        &JournalEvent::CrateAdd {
            name: " set 1 ".into(),
        },
    );
    file::apply_store_event(
        &mut s,
        &JournalEvent::CrateAdd {
            name: "set 1".into(),
        },
    );
    assert_eq!(s.crates, ["set 1"]);
    file::apply_store_event(
        &mut s,
        &JournalEvent::VariationSave {
            instrument: "laser".into(),
            variation: VoiceVariation::new("a", VoiceTweak::default(), false),
        },
    );
    file::apply_store_event(
        &mut s,
        &JournalEvent::VariationRename {
            instrument: "laser".into(),
            name: "a".into(),
            to: "b".into(),
        },
    );
    assert_eq!(
        s.variations["laser"]
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["b"]
    );
    file::apply_store_event(
        &mut s,
        &JournalEvent::VariationDelete {
            instrument: "laser".into(),
            name: "b".into(),
        },
    );
    assert!(s.variations.get("laser").is_none());
    file::apply_store_event(&mut s, &JournalEvent::NoteAdd(note()));
    file::apply_store_event(
        &mut s,
        &JournalEvent::NoteEdit {
            id: "n-1".into(),
            text: "  ".into(),
            at: jd(5.0),
        },
    ); // blank: ignored
    assert_eq!(s.notes[0].text, "drop hits here");
    file::apply_store_event(
        &mut s,
        &JournalEvent::NoteEdit {
            id: "n-1".into(),
            text: "big drop".into(),
            at: jd(5.0),
        },
    );
    assert!(s.notes[0].text == "big drop" && s.notes[0].edited == Some(jd(5.0)));
    assert_eq!(
        notes_on_favorite(&s.notes, &f).len(),
        1,
        "a favorite of the track carries its note"
    );
    file::apply_store_event(&mut s, &JournalEvent::FavoriteRemove { cid: f.cid() });
    file::apply_store_event(&mut s, &JournalEvent::NoteDelete { id: "n-1".into() });
    assert_eq!(
        s,
        JournalStoreState {
            crates: vec!["set 1".into()],
            ..Default::default()
        }
    );
    let handled = file::apply_store_event(
        &mut s,
        &JournalEvent::Action(DeckAction::Play {
            deck: "A".into(),
            on: true,
        }),
    );
    assert!(!handled, "a poke is not a store event");
}

// JournalCodecTests.aStoresReplaceSetsEveryStore
#[test]
fn a_stores_replace_sets_every_store() {
    let mut s = JournalStoreState::default();
    file::apply_store_event(
        &mut s,
        &JournalEvent::FavoriteAdd(Favorite::new("gone", DeckSource::Net(spec()), ws(1.0))),
    );
    file::apply_store_event(
        &mut s,
        &JournalEvent::CrateAdd {
            name: "old crate".into(),
        },
    );
    file::apply_store_event(&mut s, &JournalEvent::NoteAdd(note()));
    let mut fav = Favorite::new(
        "new",
        DeckSource::Net(TrackSpec::new("house", 7.0, None)),
        ws(2.0),
    );
    fav.tags = vec!["warm".into()];
    let replaced = JournalStoreState {
        favorites: vec![fav],
        crates: vec!["warm".into(), "set 2".into()],
        variations: BTreeMap::from([(
            "bass".to_string(),
            vec![VoiceVariation::new("deep", VoiceTweak::default(), true)],
        )]),
        notes: vec![TrackNote::new(
            "n-9",
            NoteTarget::Clip { id: "c".into() },
            "replaced",
            jd(3.0),
        )],
    };
    assert!(file::apply_store_event(
        &mut s,
        &JournalEvent::StoresReplace(replaced.clone())
    ));
    assert_eq!(s, replaced, "nothing of the old stores is left");
    file::apply_store_event(
        &mut s,
        &JournalEvent::CrateDelete {
            name: "set 2".into(),
        },
    );
    assert!(s.crates == ["warm"] && s.favorites == replaced.favorites);
    file::apply_store_event(
        &mut s,
        &JournalEvent::StoresReplace(JournalStoreState::default()),
    );
    assert_eq!(s, JournalStoreState::default());
    let e = JournalEntry {
        seq: 1,
        time: jd(4.0),
        frame: 0,
        event: JournalEvent::StoresReplace(replaced.clone()),
    };
    assert_eq!(
        JournalEntry::from_line(&e.line()).unwrap().event,
        JournalEvent::StoresReplace(replaced)
    );
    assert!(e.event.fold_key().is_none() && !e.event.is_note());
}

// JournalCodecTests.theControllerActionsRoundTripFoldAndRead
#[test]
fn the_controller_actions_round_trip_fold_and_read() {
    let d = |x: &str| x.to_string();
    let actions = [
        DeckAction::Rewind { deck: d("A") },
        DeckAction::Focus { deck: d("L") },
        DeckAction::StemMute {
            deck: d("B"),
            stem: d("music"),
            on: true,
        },
        DeckAction::StemMute {
            deck: d("B"),
            stem: d("mic"),
            on: false,
        },
        DeckAction::MasterFx {
            deck: d("A"),
            key: d("lp-freq"),
            value: 0.25,
        },
        DeckAction::MasterFx {
            deck: d("L"),
            key: d("master-pitch"),
            value: 1.0,
        },
    ];
    for (i, a) in actions.iter().enumerate() {
        assert_eq!(
            DeckAction::from_json(&JsonValue::parse(&a.json().js_stringified()).unwrap()).as_ref(),
            Some(a)
        );
        let e = JournalEntry {
            seq: i as i64 + 1,
            time: jd(1.0),
            frame: i as i64,
            event: JournalEvent::Action(a.clone()),
        };
        assert_eq!(
            JournalEntry::from_line(&e.line()).unwrap().event,
            JournalEvent::Action(a.clone())
        );
    }
    assert!(DeckAction::from_json(
        &JsonValue::parse(r#"{"op":"masterFx","deck":"A","value":1}"#).unwrap()
    )
    .is_none());
    let fx = JournalEvent::Action(DeckAction::MasterFx {
        deck: d("A"),
        key: d("lp-freq"),
        value: 0.25,
    });
    assert_eq!(fx.fold_key().as_deref(), Some("A.fx.lp-freq"));
    assert_eq!(
        JournalEvent::Action(DeckAction::MasterFx {
            deck: d("A"),
            key: d("lp-freq"),
            value: 0.9
        })
        .fold_key(),
        fx.fold_key()
    );
    assert_ne!(
        JournalEvent::Action(DeckAction::MasterFx {
            deck: d("A"),
            key: d("hp-freq"),
            value: 0.9
        })
        .fold_key(),
        fx.fold_key()
    );
    assert_ne!(
        fx.fold_key(),
        JournalEvent::Action(DeckAction::Param {
            deck: d("A"),
            name: d("filter"),
            value: 0.0
        })
        .fold_key()
    );
    assert!(JournalEvent::Action(DeckAction::Rewind { deck: d("A") })
        .fold_key()
        .is_none());
    assert_eq!(fx.summary(), "A fx lp-freq 0.25");
    assert_eq!(
        JournalEvent::Action(DeckAction::Rewind { deck: d("A") }).summary(),
        "A rewind"
    );
    assert_eq!(
        JournalEvent::Action(DeckAction::Focus { deck: d("L") }).summary(),
        "focus L"
    );
    assert_eq!(
        JournalEvent::Action(DeckAction::StemMute {
            deck: d("B"),
            stem: d("mic"),
            on: true
        })
        .summary(),
        "B stem mic muted"
    );
    // a knob turned folds
    let cc = |v: u8| JournalEvent::Midi {
        port: d(APC),
        bytes: vec![0xB0, 7, v],
    };
    assert_eq!(cc(1).fold_key(), cc(90).fold_key());
    assert_eq!(
        cc(1).fold_key().as_deref(),
        Some("cc.APC mini mk2 Control.176.7")
    );
    assert!(JournalEvent::Midi {
        port: d(APC),
        bytes: vec![0x90, 1, 1]
    }
    .fold_key()
    .is_none());
}

#[test]
fn people_readable_summaries() {
    let d = |x: &str| x.to_string();
    assert_eq!(
        action_summary(&DeckAction::Play {
            deck: d("A"),
            on: false
        }),
        "A pause"
    );
    assert_eq!(
        action_summary(&DeckAction::Load {
            deck: d("A"),
            source: DeckSource::Net(spec())
        }),
        "A load net techno #42"
    );
    assert_eq!(
        action_summary(&DeckAction::Crossfader(0.5)),
        "crossfader 0.50"
    );
    assert_eq!(
        action_summary(&DeckAction::AutoMix {
            on: true,
            bars: 16,
            every: 0
        }),
        "auto-mix on · 16 bars · to the outro"
    );
    assert_eq!(
        action_summary(&DeckAction::AutoMix {
            on: false,
            bars: 8,
            every: 8
        }),
        "auto-mix off · 8 bars · every 8 phrases"
    );
    assert_eq!(
        action_summary(&DeckAction::HotCue {
            deck: d("A"),
            slot: 2
        }),
        "A hotCue slot=2"
    );
    assert_eq!(
        action_summary(&DeckAction::Follow {
            deck: d("B"),
            leader: None
        }),
        "B follow"
    );
    assert_eq!(
        action_summary(&DeckAction::ClipRename {
            id: d("x"),
            title: d("a very long clip title here")
        }),
        "clipRename id=x title=a very long …"
    );
    assert_eq!(
        JournalEvent::Mark(d("session.start")).summary(),
        "— session.start"
    );
    assert_eq!(
        JournalEvent::CrateAdd { name: d("set 1") }.summary(),
        "crate + set 1"
    );
}

// JournalCheckpointTests.aPositionRoundTripsThroughTheCheckpointLine
fn with_positions() -> JournalState {
    let s = spec();
    let mut st = JournalState::new(48_000.0);
    let mut a = DeckCheckpoint::new(DeckSource::Sample {
        id: "demo-loop".into(),
        title: "demo".into(),
        grid: None,
    });
    a.position = Some(12_345.5);
    let mut b = DeckCheckpoint::new(DeckSource::Net(s.clone()));
    b.playing = true;
    b.position = Some(96.0);
    st.decks.insert("A".into(), a);
    st.decks.insert("B".into(), b);
    st.decks
        .insert("C".into(), DeckCheckpoint::new(DeckSource::Net(s)));
    st
}

#[test]
fn a_position_round_trips_through_the_checkpoint_line() {
    let e = JournalEntry {
        seq: 0,
        time: jd(1_760_000_000.0),
        frame: 0,
        event: JournalEvent::Checkpoint(with_positions()),
    };
    let back = JournalEntry::from_line(&e.line()).unwrap();
    assert_eq!(back, e);
    assert_eq!(back.line(), e.line(), "re-encodes to the same bytes");
    let JournalEvent::Checkpoint(s) = &back.event else {
        panic!("not a checkpoint")
    };
    assert_eq!(s.decks["A"].position, Some(12_345.5));
    assert_eq!(s.decks["B"].position, Some(96.0));
    assert_eq!(s.decks["C"].position, None, "written only when set");
}

// JournalCheckpointTests.anOldCheckpointHasNoPositionAndStaysByteForByte
#[test]
fn an_old_checkpoint_has_no_position_and_stays_byte_for_byte() {
    let mut old = with_positions();
    for d in old.decks.values_mut() {
        d.position = None;
    }
    let text = old.json().canonical_js();
    assert!(!text.contains("\"position\""), "no key without a position");
    assert!(text.contains("djrust/state/v1"), "schema stays v1");
    let back = JournalState::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    assert!(back.decks.values().all(|d| d.position.is_none()));
    assert_eq!(back.json().canonical_js(), text);
}

// Not a Swift test: a checkpoint's `mixer.autoMix` is `AutoMixState.checkpointJSON`, the type
// `session::types` shares with `automix` (its codec is held to Swift by `wander_golden`): what
// is in flight on the engine survives the checkpoint line, in Swift's key order.
#[test]
fn a_checkpoint_carries_the_auto_mix_in_flight() {
    let mut st = JournalState::new(48_000.0);
    st.auto_mix = AutoMixState {
        settings: AutoMixSettings::new(true, 16, 8),
        handoffs: 3,
        playing: Some("A".into()),
        paused: true,
        quiet_bars: 3,
        start_bar: Some(8),
        now_requested: true,
        cued: Some("B".into()),
        ..AutoMixState::default()
    };
    let json = st.json();
    assert_eq!(
        json.get("mixer")
            .unwrap()
            .get("autoMix")
            .unwrap()
            .js_stringified(),
        r#"{"on":true,"bars":16,"every":8,"handoffs":3,"playing":"A","paused":true,"quietBars":3,"startBar":8,"now":true,"cued":"B"}"#
    );
    let back = JournalState::from_json(&json).unwrap();
    assert_eq!(back.auto_mix, st.auto_mix);
    assert_eq!(back.json().js_stringified(), json.js_stringified());
    // a state at rest writes nothing
    let rest = JournalState::new(48_000.0).json();
    assert!(rest.get("mixer").unwrap().get("autoMix").is_none());
}

// JournalCodecTests.aCheckpointKeepsAClipsLaunchBoundaryAndFade (the codec half)
#[test]
fn a_checkpoint_keeps_a_clips_launch_boundary_and_fade_only_when_set() {
    let plain = ClipCheckpoint::new("abc", "A loop 1");
    let mut s = JournalState::new(48_000.0);
    s.clips = vec![plain.clone()];
    let before = s.json().canonical_js();
    assert!(!before.contains("quantum") && !before.contains("\"fade\""));
    s.clips[0].quantum = Some(16);
    s.clips[0].fade = Some("off".into());
    let back =
        JournalState::from_json(&JsonValue::parse(&s.json().canonical_js()).unwrap()).unwrap();
    assert_eq!(back.clips, s.clips);
    s.clips = vec![plain];
    assert_eq!(
        s.json().canonical_js(),
        before,
        "without them: byte for byte as before"
    );
}

#[test]
fn a_checkpoint_without_stores_says_so_and_names_its_samples() {
    let mut s = with_positions();
    s.has_stores = false;
    let back =
        JournalState::from_json(&JsonValue::parse(&s.json().canonical_js()).unwrap()).unwrap();
    assert!(!back.has_stores && back.stores == JournalStoreState::default());
    assert!(!s.json().canonical_js().contains("\"stores\""));
    let mut t = JournalState::new(1.0);
    t.decks.insert(
        "A".into(),
        DeckCheckpoint::new(DeckSource::Sample {
            id: "abc".into(),
            title: "x".into(),
            grid: None,
        }),
    );
    t.decks.insert(
        "B".into(),
        DeckCheckpoint::new(DeckSource::Sample {
            id: "demo-loop".into(),
            title: "x".into(),
            grid: None,
        }),
    );
    let mut c = DeckCheckpoint::new(DeckSource::Sample {
        id: "def".into(),
        title: "x".into(),
        grid: None,
    });
    c.clip = Some("def".into());
    t.decks.insert("C".into(), c);
    assert_eq!(
        t.sample_ids(),
        ["abc"],
        "the demo loop and a deck holding a clip are not sample files"
    );
}

// ---- notes, favorites, variations ------------------------------------------------------------------------

// JournalFileTests.notesKeyTracksByConfiguration (the key and matching halves)
#[test]
fn notes_key_tracks_by_configuration() {
    let target = NoteTarget::Track {
        cid: djrust::cid::share_cid(&spec().payload()),
    };
    let same = NoteTarget::Track {
        cid: djrust::cid::share_cid(&spec().payload()),
    };
    assert_eq!(target, same, "the same configuration is the same track");
    let mut next = TrackSpec::new("techno", 43.0, Some("standard"));
    next.cohesion = "v2".into();
    assert_ne!(
        target,
        NoteTarget::Track {
            cid: djrust::cid::share_cid(&next.payload())
        }
    );
    let fav = Favorite::new("t", DeckSource::Net(spec()), 0);
    assert!(target.matches(&fav));
    let mut s = Favorite::new(
        "s",
        DeckSource::Sample {
            id: String::new(),
            title: "s".into(),
            grid: None,
        },
        0,
    );
    s.file = Some("/a.caf".into());
    assert!(NoteTarget::Sample {
        id: "x".into(),
        file: Some("/a.caf".into())
    }
    .matches(&s));
    assert!(!NoteTarget::Clip { id: "x".into() }.matches(&s));
    assert_eq!(
        target.key(),
        format!(
            "track:{}",
            match &target {
                NoteTarget::Track { cid } => cid,
                _ => unreachable!(),
            }
        )
    );
    assert_eq!(
        NoteTarget::Sample {
            id: "demo-loop".into(),
            file: None
        }
        .key(),
        "sample:demo-loop"
    );
    assert_eq!(NoteTarget::Clip { id: "c".into() }.key(), "clip:c");
}

#[test]
fn notes_list_in_track_order_and_edit_by_pure_functions() {
    let t = NoteTarget::Clip { id: "c".into() };
    let mk = |id: &str, bar: Option<i64>, created: i64| {
        let mut n = TrackNote::new(id, t.clone(), id, created);
        n.position = bar.map(|b| NotePosition::new(b, 1, None));
        n
    };
    let notes = vec![
        mk("late", Some(9), 1),
        mk("early", Some(2), 5),
        mk("loose", None, 3),
        mk("loose2", None, 2),
    ];
    let on: Vec<String> = notes_on(&notes, "clip:c")
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(
        on,
        ["loose2", "loose", "early", "late"],
        "unplaced first, then by bar, then by when"
    );
    assert!(notes_on(&notes, "clip:other").is_empty());
    assert_eq!(NotePosition::new(17, 3, None).label(), "bar 17.3");
    assert_eq!(NotePosition::new(17, 1, None).label(), "bar 17");
}

#[test]
fn favorites_tags_and_crates() {
    let a = Favorite::new("a", DeckSource::Net(spec()), ws(1.0));
    let mut b = Favorite::new(
        "b",
        DeckSource::Net(TrackSpec::new("house", 1.0, None)),
        ws(2.0),
    );
    b.tags = vec!["warm".into(), "intro".into()];
    let list = favorites_adding(&b, &favorites_adding(&a, &[]));
    assert_eq!(
        list.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(),
        ["b", "a"],
        "newest first"
    );
    let mut a2 = a.clone();
    a2.tags = vec!["intro".into()];
    let list = favorites_adding(&a2, &list);
    assert_eq!(
        all_tags(&list),
        ["intro", "warm"],
        "most used first, then A–Z"
    );
    assert_eq!(tagged(&list, Some("intro")).len(), 2);
    assert_eq!(tagged(&list, None).len(), 2);
    // a sample with no file is no favorite; an empty cid adds nothing
    let none = Favorite::new(
        "x",
        DeckSource::Sample {
            id: "i".into(),
            title: "x".into(),
            grid: None,
        },
        0,
    );
    assert_eq!(none.cid(), "");
    assert_eq!(favorites_adding(&none, &list), list);
    // favorites survive their own json (the "added" stamp keeps whole seconds)
    for f in &list {
        assert_eq!(Favorite::from_json(&f.json()).as_ref(), Some(f));
    }
    // a favorite whose source names nothing no longer decodes
    assert!(Favorite::from_json(
        &JsonValue::parse(r#"{"title":"x","source":{"kind":"none"}}"#).unwrap()
    )
    .is_none());
}

// ---- the recorder's bookkeeping ----------------------------------------------------------------------------

// JournalCheckpointTests.checkpointsComeOnTheirCadence (the due rule)
#[test]
fn checkpoints_come_on_their_cadence() {
    let mut r = JournalRecorder::new(1000.0);
    r.checkpoint_seconds = 1.0;
    r.checkpoint_entries = 5;
    let cp = || JournalEvent::Checkpoint(JournalState::new(1000.0));
    let e = r.record(0, 0, cp());
    assert_eq!(e.seq, 0);
    assert!(!r.checkpoint_due(10_000), "an idle app writes none");
    r.record(10, 1, JournalEvent::Mark("a".into()));
    assert!(
        !r.checkpoint_due(500),
        "something was recorded, but neither bound is reached"
    );
    assert!(r.checkpoint_due(1000), "a second of engine time");
    r.record(1000, 2, cp());
    assert!(!r.checkpoint_due(1000));
    for i in 0..5 {
        r.record(1001 + i, 3 + i, JournalEvent::Mark("x".into()));
    }
    assert!(r.checkpoint_due(1010), "five entries");
    assert_eq!(r.last_seq(), 7);
    assert_eq!(r.recent().len(), 8);
    for i in 0..600 {
        r.record(2000, 10 + i, JournalEvent::Mark("y".into()));
    }
    assert_eq!(r.recent().len(), 400);
    assert_eq!(r.recent().last().unwrap().seq, r.last_seq());
}

// ---- replay planning --------------------------------------------------------------------------------------------------

fn journal_with_mid_checkpoints() -> String {
    let mut lines = Vec::new();
    let mut seq = 0;
    let mut push = |frame: i64, event: JournalEvent| {
        lines.push(
            JournalEntry {
                seq,
                time: jd(1_760_000_000.0 + seq as f64),
                frame,
                event,
            }
            .line(),
        );
        seq += 1;
    };
    let mut head = JournalState::new(1000.0);
    head.stores.crates = vec!["old".into()];
    push(0, JournalEvent::Checkpoint(head)); // 0
    push(
        10,
        JournalEvent::CrateAdd {
            name: "before".into(),
        },
    ); // 1
    push(
        20,
        JournalEvent::Action(DeckAction::Play {
            deck: "A".into(),
            on: true,
        }),
    ); // 2
    let mut mid = JournalState::new(1000.0);
    mid.has_stores = false;
    let mut a = DeckCheckpoint::new(DeckSource::None);
    a.position = Some(44.0);
    mid.decks.insert("A".into(), a);
    push(30, JournalEvent::Checkpoint(mid.clone())); // 3
    push(40, JournalEvent::NoteAdd(note())); // 4
    push(
        50,
        JournalEvent::Midi {
            port: APC.into(),
            bytes: vec![0x90, 1, 2],
        },
    ); // 5
    push(60, JournalEvent::Checkpoint(mid)); // 6
    push(70, JournalEvent::Action(DeckAction::Crossfader(0.1))); // 7
    push(80, JournalEvent::Mark("rewind 3".into())); // 8
    let mut rw = JournalState::new(1000.0);
    rw.stores.crates = vec!["rewound".into()];
    push(80, JournalEvent::Checkpoint(rw)); // 9
    push(90, JournalEvent::Action(DeckAction::Master(0.3))); // 10
    lines.join("\n") + "\n"
}

#[test]
fn checkpoint_and_rewind_indices() {
    let c = parse(&journal_with_mid_checkpoints());
    assert_eq!(
        c.checkpoint_indices()
            .iter()
            .map(|&i| c.entries[i].seq)
            .collect::<Vec<_>>(),
        [0, 3, 6, 9]
    );
    assert_eq!(c.rewind_checkpoint_indices(), [9]);
    assert!(replay::rewind_mark(3) == "rewind 3" && c.entries[8].event.is_rewind_mark());
}

// JournalCheckpointTests.aReplayStartsFromTheNearestCheckpointAndEndsInTheSameState (the plan)
#[test]
fn a_replay_starts_from_the_nearest_checkpoint() {
    let c = parse(&journal_with_mid_checkpoints());
    // up to seq 5: the mid checkpoint at 3, with line 0's stores plus the store events before it
    let p = replay::plan(
        &c,
        &ReplayOptions {
            to_seq: Some(5),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        (p.start_index, p.start_seq, p.base, p.offset),
        (3, 3, 30, 30)
    );
    assert!(p.state.has_stores);
    assert_eq!(
        p.state.stores.crates,
        ["old", "before"],
        "a mid checkpoint has no stores: line 0's, then the events before it"
    );
    assert_eq!(p.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
    assert_eq!(p.end, 20);
    // from the start: line 0
    let p = replay::plan(
        &c,
        &ReplayOptions {
            to_seq: Some(5),
            from_start: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((p.start_index, p.offset), (0, 0));
    assert_eq!(p.events.len(), 5);
    // all of it: the rewind's checkpoint is as far back as it goes, even from the start
    let p = replay::plan(
        &c,
        &ReplayOptions {
            from_start: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((p.start_index, p.start_seq), (9, 9));
    assert_eq!(p.state.stores.crates, ["rewound"]);
    assert_eq!(p.events.len(), 1);
    // a frame bound
    let p = replay::plan(
        &c,
        &ReplayOptions {
            end_frame: Some(45),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p.start_seq, 3);
    assert_eq!(p.end_here, Some(15));
    assert_eq!(p.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [4]);
    // not a journal
    assert_eq!(
        replay::plan(&parse(""), &ReplayOptions::default()).unwrap_err(),
        JournalError::NoCheckpoint
    );
}

struct FakeEngine {
    frame: i64,
    applied: Vec<(i64, String)>,
}
impl ReplayEngine for FakeEngine {
    fn frame(&self) -> i64 {
        self.frame
    }
    fn apply(&mut self, e: &JournalEvent, _w: &mut Vec<String>) {
        self.applied.push((self.frame, e.summary()));
    }
    fn step(&mut self) -> (Vec<f32>, Vec<f32>) {
        self.frame += 128;
        (vec![1.0; 128], vec![2.0; 128])
    }
}

#[test]
fn the_replay_loop_applies_events_at_their_frames_and_folds_the_stores() {
    let c = parse(&journal_with_mid_checkpoints());
    let p = replay::plan(
        &c,
        &ReplayOptions {
            to_seq: Some(5),
            ..Default::default()
        },
    )
    .unwrap();
    let mut e = FakeEngine {
        frame: 0,
        applied: vec![],
    };
    let out = replay::run(&p, &mut e, true, 1000.0, vec!["restored".into()]);
    // the note is a store event; the midi is the engine's; both at frame <= the engine's
    assert_eq!(e.applied.len(), 1);
    assert!(e.applied[0].1.starts_with("midi APC"), "{:?}", e.applied);
    assert_eq!(out.stores.notes.len(), 1);
    assert_eq!(out.stores.crates, ["old", "before"]);
    assert_eq!((out.start_seq, out.last_seq), (3, 5));
    assert_eq!(out.warnings, ["restored"]);
    assert_eq!(out.frames, e.frame + 30);
    assert!(out.audio.is_none() == false);
    // at least one block is rendered so the last event's effects are heard
    assert!(e.frame >= 128);
    let silent = replay::run(
        &p,
        &mut FakeEngine {
            frame: 0,
            applied: vec![],
        },
        false,
        1000.0,
        vec![],
    );
    assert!(silent.audio.is_none());
}

// JournalReplayTests.aSliceExportsAsANormalSession (the slice → session half)
#[test]
fn a_slice_exports_as_a_normal_session() {
    let c = parse(&journal_with_mid_checkpoints());
    let slice = replay::export_slice(&c, 4, 7).unwrap();
    assert_eq!((slice.replay_to_seq, slice.replay_end_frame), (3, 40));
    let mut at = JournalState::new(1000.0);
    let mut a = DeckCheckpoint::new(DeckSource::Net(spec()));
    a.playing = true;
    at.decks.insert("A".into(), a);
    at.locked = vec!["A.play".into()];
    at.loop_settings = vec![
        DeckAction::LoopQuantize { beats: 8 },
        DeckAction::LoopFade {
            mode: "10ms".into(),
        },
    ];
    let s = replay::export_session(&slice, &at, 2.0);
    // settings away from the defaults, locks, plays, then the slice's actions and MIDI, rebased
    let kinds: Vec<String> = s
        .events
        .iter()
        .map(|e| match &e.kind {
            SessionEventKind::Action(a) => format!("{}@{}", a.op(), e.frame),
            SessionEventKind::Midi { .. } => format!("midi@{}", e.frame),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "loopQuantize@0",
            "lock@0",
            "play@0",
            "midi@10",
            "crossfader@30"
        ]
    );
    assert_eq!(s.frames, 70 - 40 + 2000);
    assert_eq!(s.decks["A"], DeckSource::Net(spec()));
    assert_eq!(s.params["A"], DeckParams::default());
    assert_eq!(
        replay::export_slice(&c, 100, 200).unwrap_err(),
        JournalError::EmptySlice
    );
    assert_eq!(
        replay::export_slice(&c, 7, 10).unwrap_err(),
        JournalError::SpansRewind(8)
    );
}

// JournalCheckpointTests.theLatestStoresSkipMidCheckpoints
#[test]
fn the_latest_stores_skip_mid_checkpoints() {
    let text = journal_with_mid_checkpoints();
    // the whole file ends at the rewind's replaced stores? no: store events fold, checkpoints don't
    let s = file::latest_stores(&text).unwrap();
    assert_eq!(s.crates, ["old", "before"]);
    assert_eq!(
        s.notes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["n-1"]
    );
    assert!(file::latest_stores("").is_none());
    assert!(file::latest_stores("{not a checkpoint}\n").is_none());
    let mut names = vec![
        "journal-20261017T143205.123Z.jsonl".to_string(),
        "other.txt".into(),
        "journal-20261018T000000.000Z.jsonl".into(),
    ];
    file::sort_newest_first(&mut names);
    assert_eq!(
        names,
        [
            "journal-20261018T000000.000Z.jsonl",
            "journal-20261017T143205.123Z.jsonl"
        ]
    );
}

#[test]
fn clip_ids_name_what_an_action_touches() {
    use djrust::journal::event::clip_ids;
    use djrust::session::types::SequenceStep;
    assert_eq!(
        clip_ids(&DeckAction::ClipGain {
            id: "a".into(),
            value: 1.0
        }),
        ["a"]
    );
    assert_eq!(
        clip_ids(&DeckAction::ClipMove {
            id: "a".into(),
            before: Some("b".into())
        }),
        ["a", "b"]
    );
    assert_eq!(
        clip_ids(&DeckAction::ClipSequence {
            steps: vec![SequenceStep::new("x", 1), SequenceStep::new("y", 2)],
            replacing: Some("z".into())
        }),
        ["x", "y", "z"]
    );
    assert!(clip_ids(&DeckAction::Play {
        deck: "A".into(),
        on: true
    })
    .is_empty());
}
