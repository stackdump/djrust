//! The journal on the Rust engine: the Swift suites' `JournalReplayTests`,
//! `JournalCheckpointTests`, `JournalRewindTests` and the engine halves of `JournalFileTests`
//! (parity form (a): each test names the Swift test it ports). A replay is deterministic per
//! engine, so the properties are the Swift tests' own: a journal replays to the state it was
//! made in, and from the nearest checkpoint to the same state as from line 0; a reset equals a
//! restore into a fresh engine. What a Swift engine reaches from the same journal text is
//! `journal_golden.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use djrust::automix::{AutoMixPhase, AutoMixRun};
use djrust::deck::clips::LoopClip;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::journal::event::{JournalEntry, JournalEvent};
use djrust::journal::file::{apply_store_event, parse, Contents};
use djrust::journal::replay::{rewind_mark, JournalError};
use djrust::journal::state::LOOP_FADES;
use djrust::journal::state::{DeckCheckpoint, JournalState};
use djrust::journal::stores::{
    favorite_cid, Favorite, JournalStoreState, NotePosition, NoteTarget, TrackNote, VoiceVariation,
};
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, Session, DEMO_ID};
use djrust::session::wav::RenderedAudio;
use djrust::wave::VoiceTweak;
use djrust::TrackSpec;
use djrust::host::deck::{OfflineConductor, PerformCtx};
use djrust::host::journal::{read, Journal};
use djrust::host::replay::{export_session_from, offline_conductor, replay, JournalReplayOptions};

const APC: &str = "APC mini mk2 Control";
const RATE: f64 = 11_025.0;

fn spec() -> TrackSpec {
    TrackSpec::new("techno", 7.0, Some("standard"))
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("deck-journal-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// `journalDate` / `wholeSecond`: stamps in Unix milliseconds.
fn at_ms(s: i64) -> i64 {
    s * 1000
}

fn perform(c: &mut OfflineConductor, a: DeckAction) {
    c.rig.engine.perform(&a, PerformCtx::default());
}

fn play(deck: &str, on: bool) -> DeckAction {
    DeckAction::Play {
        deck: deck.into(),
        on,
    }
}

fn param(deck: &str, name: &str, value: f64) -> DeckAction {
    DeckAction::Param {
        deck: deck.into(),
        name: name.into(),
        value,
    }
}

fn load_demo(deck: &str) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Sample {
            id: DEMO_ID.into(),
            title: "demo".into(),
            grid: None,
        },
    }
}

fn load_net(deck: &str, s: &TrackSpec) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Net(s.clone()),
    }
}

/// A run of `blocks` blocks; `inputs` runs before the first.
fn run(
    c: &mut OfflineConductor,
    blocks: usize,
    inputs: impl FnOnce(&mut OfflineConductor),
) -> RenderedAudio {
    let mut out = RenderedAudio {
        sample_rate: c.engine().sample_rate,
        left: Vec::new(),
        right: Vec::new(),
    };
    let mut inputs = Some(inputs);
    for _ in 0..blocks {
        let (l, r) = c.step_with(|c| {
            if let Some(f) = inputs.take() {
                f(c)
            }
        });
        out.left.extend_from_slice(l);
        out.right.extend_from_slice(r);
    }
    out
}

fn no_stores() -> JournalStoreState {
    JournalStoreState::default()
}

fn state_of(c: &OfflineConductor) -> JournalState {
    c.engine().journal_state(no_stores(), false)
}

/// `JournalState ==`: by canonical JSON.
fn same(a: &JournalState, b: &JournalState) -> bool {
    a.json().canonical_js() == b.json().canonical_js()
}

fn identical(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn peak(a: &[f32]) -> f32 {
    a.iter().fold(0f32, |p, v| p.max(v.abs()))
}

/// A fixed wall clock for the entries' stamps: one millisecond per entry.
fn clock() -> djrust::host::journal::JournalClock {
    let mut t = at_ms(1_760_000_000);
    Box::new(move || {
        t += 1;
        t
    })
}

// MARK: JournalReplayTests

struct Live {
    state: JournalState,
    contents: Contents,
    audio: RenderedAudio,
    /// The same performance through a `SessionRecorder` alongside (both sinks see each input).
    session: Session,
}

/// `JournalReplayTests.perform`: the scripted performance the replay tests share: two decks,
/// loads, knobs, a section jump, a LOOP take and a grab (of the sample deck), clip moves and
/// names, favorites and tags, a crate, a voice variation, a lock (with the MIDI it locks), a
/// note.
fn live_run(tag: &str) -> Live {
    let dir = tmp(tag);
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let starred = Favorite {
        tags: vec!["warm".into()],
        ..Favorite::new(
            "old one",
            DeckSource::Net(TrackSpec::new("house", 3.0, None)),
            at_ms(1_700_000_000),
        )
    };
    let mut stores = JournalStoreState {
        favorites: vec![starred.clone()],
        ..Default::default()
    };
    let journal = Journal::attach_with_clock(
        c.engine_mut(),
        &dir,
        at_ms(1_760_000_000),
        stores.clone(),
        clock(),
    )
    .unwrap();
    c.engine_mut().start_session().unwrap();
    c.rig.attach(APC);
    let net = spec();
    let mut audio = RenderedAudio {
        sample_rate: RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..400usize {
        let (l, r) = c.step_with(|c| {
            let mut store = |c: &OfflineConductor, e: JournalEvent| {
                journal.record(c.engine().frame(), e.clone());
                apply_store_event(&mut stores, &e);
            };
            match b {
                0 => {
                    perform(c, load_demo("A"));
                    perform(c, load_net("B", &net));
                    perform(c, play("A", true));
                    perform(c, play("B", true));
                }
                20 => {
                    perform(c, param("A", "volume", 0.7));
                    perform(c, DeckAction::Crossfader(0.35));
                    perform(c, param("B", "filter", 0.3));
                    // B's sampled instruments may vary between renders (round robin): keep it
                    // out of the mix and the takes, so audio and clip ids compare exactly
                    perform(c, param("B", "volume", 0.0));
                }
                30 => perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: true,
                        bars: Some(1),
                    },
                ),
                32 => perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: false,
                        bars: None,
                    },
                ),
                150 => perform(
                    c,
                    DeckAction::Section {
                        deck: "B".into(),
                        index: 1,
                    },
                ),
                160 => {
                    let fav = Favorite::new(
                        "B track",
                        DeckSource::Net(net.clone()),
                        at_ms(1_760_000_001),
                    );
                    store(c, JournalEvent::FavoriteAdd(fav));
                    store(
                        c,
                        JournalEvent::FavoriteTags {
                            cid: djrust::cid::share_cid(&net.payload()),
                            tags: vec!["peak".into(), "set 1".into()],
                        },
                    );
                    store(
                        c,
                        JournalEvent::CrateAdd {
                            name: "set 1".into(),
                        },
                    );
                    store(
                        c,
                        JournalEvent::FavoriteTags {
                            cid: starred.cid(),
                            tags: Vec::new(),
                        },
                    );
                }
                170 => {
                    perform(
                        c,
                        DeckAction::Lock {
                            key: "B.section".into(),
                            on: true,
                        },
                    );
                    let now = c.now();
                    c.rig.route(APC, &[0x90, 48, 0x7F], now);
                    c.rig.route(APC, &[0x80, 48, 0x00], now);
                }
                180 => {
                    let (target, title) = c.engine().note_target("B").unwrap();
                    let mut n =
                        TrackNote::new("n-drop", target, "drop hits here", at_ms(1_760_000_002));
                    n.title = title;
                    n.position = c.engine().note_position("B");
                    store(c, JournalEvent::NoteAdd(n));
                    store(
                        c,
                        JournalEvent::VariationSave {
                            instrument: "laser".into(),
                            variation: VoiceVariation::new("zap", VoiceTweak::default(), false),
                        },
                    );
                }
                200 => {
                    // the controller-only controls, as actions: a master FX slider and a rewind
                    // on the audible deck, the focus and a STEMS group on the net one
                    perform(
                        c,
                        DeckAction::MasterFx {
                            deck: "A".into(),
                            key: "lp-freq".into(),
                            value: 0.6,
                        },
                    );
                    perform(c, DeckAction::Rewind { deck: "A".into() });
                    perform(c, DeckAction::Focus { deck: "B".into() });
                    perform(
                        c,
                        DeckAction::StemMute {
                            deck: "B".into(),
                            stem: "music".into(),
                            on: true,
                        },
                    );
                }
                360 => perform(
                    c,
                    DeckAction::GrabBars {
                        deck: "A".into(),
                        bars: 1,
                    },
                ),
                370 => {
                    // both clips are on the row by now
                    if let Some(first) = c.engine().loops.row.clips.first().map(|k| k.id.clone()) {
                        perform(
                            c,
                            DeckAction::ClipRename {
                                id: first.clone(),
                                title: "the take".into(),
                            },
                        );
                        perform(
                            c,
                            DeckAction::ClipColor {
                                id: first.clone(),
                                color: Some(djrust::deck::clips::PALETTE[0].to_string()),
                            },
                        );
                        perform(
                            c,
                            DeckAction::ClipMove {
                                id: first.clone(),
                                before: None,
                            },
                        );
                        perform(
                            c,
                            DeckAction::ClipGain {
                                id: first,
                                value: 0.6,
                            },
                        );
                    }
                }
                380 => {
                    if let Some(id) = c.engine().loops.row.clips.first().map(|k| k.id.clone()) {
                        store(
                            c,
                            JournalEvent::NoteAdd(TrackNote::new(
                                "n-clip",
                                NoteTarget::Clip { id },
                                "use in the break",
                                at_ms(1_760_000_003),
                            )),
                        );
                    }
                    perform(
                        c,
                        DeckAction::SceneSave {
                            name: "Scene 1".into(),
                        },
                    );
                }
                395 => perform(c, param("A", "volume", 0.9)),
                _ => {}
            }
        });
        audio.left.extend_from_slice(l);
        audio.right.extend_from_slice(r);
    }
    assert_eq!(c.dropped(), 0);
    let state = c.engine().journal_state(stores, false);
    let session = c.engine_mut().finish_session().unwrap();
    journal.close(c.engine_mut());
    assert!(c.engine().journal.is_none());
    let contents = read(&journal.path()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Live {
        state,
        contents,
        audio,
        session,
    }
}

fn kind(e: &JournalEntry) -> &'static str {
    e.event.kind()
}

fn seek(c: &Contents, k: &str) -> JournalEntry {
    c.entries.iter().find(|e| kind(e) == k).unwrap().clone()
}

// JournalTests.JournalReplayTests.replayingTheJournalRebuildsTheLiveState
#[test]
fn replaying_the_journal_rebuilds_the_live_state() {
    let live = live_run("rebuild");
    let c = &live.contents;
    assert!(c.skipped.is_empty() && !c.truncated);
    assert!(matches!(
        c.entries.first().map(|e| &e.event),
        Some(JournalEvent::Checkpoint(_))
    ));
    let seqs: Vec<i64> = c.entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (0..c.entries.len() as i64).collect::<Vec<_>>());
    let frames: Vec<i64> = c.entries.iter().map(|e| e.frame).collect();
    let mut sorted = frames.clone();
    sorted.sort();
    assert_eq!(frames, sorted, "frames never go back");
    for k in [
        "checkpoint",
        "act",
        "midi",
        "favorite.add",
        "favorite.tags",
        "crate.add",
        "note.add",
        "variation.save",
    ] {
        assert!(c.entries.iter().any(|e| kind(e) == k), "journaled {k}");
    }
    let events: Vec<&JournalEvent> = c.entries.iter().map(|e| &e.event).collect();
    for a in [
        DeckAction::MasterFx {
            deck: "A".into(),
            key: "lp-freq".into(),
            value: 0.6,
        },
        DeckAction::Rewind { deck: "A".into() },
        DeckAction::Focus { deck: "B".into() },
        DeckAction::StemMute {
            deck: "B".into(),
            stem: "music".into(),
            on: true,
        },
    ] {
        assert!(
            events.contains(&&JournalEvent::Action(a.clone())),
            "journaled {a:?}"
        );
    }
    assert_eq!(
        live.state.clips.len(),
        2,
        "a take and a grab: {:?}",
        live.state.clips.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert_eq!(
        live.state.clips.last().map(|c| c.title.as_str()),
        Some("the take"),
        "moved to the end, renamed"
    );
    assert_eq!(live.state.locked, ["B.section"]);
    assert_eq!(live.state.stores.notes.len(), 2);
    assert_eq!(live.state.decks["B"].source, DeckSource::Net(spec()));

    let r = replay(c, &JournalReplayOptions::default()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(Some(r.last_seq), c.entries.last().map(|e| e.seq));
    assert_eq!(
        r.state.json().canonical_js(),
        live.state.json().canonical_js()
    );
    assert!(r.state == live.state);
    // the session recorder alongside saw the same inputs the journal did
    let actions = c
        .entries
        .iter()
        .filter(|e| matches!(e.event, JournalEvent::Action(_) | JournalEvent::Midi { .. }))
        .count();
    assert_eq!(live.session.events.len(), actions);
}

// JournalTests.JournalReplayTests.aReplayIsDeterministic
#[test]
fn a_replay_is_deterministic() {
    let live = live_run("deterministic");
    let render = JournalReplayOptions {
        render: true,
        ..Default::default()
    };
    let a = replay(&live.contents, &render).unwrap();
    let b = replay(&live.contents, &render).unwrap();
    assert!(a.state == b.state);
    let (aa, bb) = (a.audio.unwrap(), b.audio.unwrap());
    assert!(identical(&aa.left, &bb.left));
    assert!(identical(&aa.right, &bb.right));
    assert!(peak(&aa.left) > 0.05, "it plays");
    // and it is the live audio, from the checkpoint on
    let n = aa.frames().min(live.audio.frames());
    assert!(identical(&aa.left[..n], &live.audio.left[..n]));
    assert!(identical(&aa.right[..n], &live.audio.right[..n]));
}

// JournalTests.JournalReplayTests.aCrashedJournalStillReplaysToItsLastWholeLine
#[test]
fn a_crashed_journal_still_replays_to_its_last_whole_line() {
    let live = live_run("crashed");
    let lines: Vec<String> = live
        .contents
        .entries
        .iter()
        .map(JournalEntry::line)
        .collect();
    let torn = format!("{}\n{}", lines.join("\n"), &lines.last().unwrap()[..20]);
    let c = parse(&torn);
    assert!(c.truncated);
    assert_eq!(c.entries.len(), lines.len());
    let r = replay(&c, &JournalReplayOptions::default()).unwrap();
    assert!(r.state == live.state);
}

// JournalTests.JournalReplayTests.replayStopsWhereItIsTold
#[test]
fn replay_stops_where_it_is_told() {
    let live = live_run("stops");
    let c = &live.contents;
    // up to the favorites (seq of the first favorite.add): no note yet
    let fav = seek(c, "favorite.add");
    let early = replay(
        c,
        &JournalReplayOptions {
            to_seq: Some(fav.seq),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(early.last_seq, fav.seq);
    assert_eq!(early.stores().favorites.len(), 2);
    assert!(early.stores().notes.is_empty());
    assert!(early.stores().crates.is_empty());
    // by time: a second in, before the favorites
    let t = replay(
        c,
        &JournalReplayOptions {
            end_frame: Some(RATE as i64),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(t.frames >= RATE as i64);
    assert_eq!(t.stores().favorites.len(), 1);
}

// JournalTests.JournalReplayTests.aSliceExportsAsANormalSession
#[test]
fn a_slice_exports_as_a_normal_session() {
    let live = live_run("slice");
    let c = &live.contents;
    let section = c
        .entries
        .iter()
        .find(|e| matches!(e.event, JournalEvent::Action(DeckAction::Section { .. })))
        .unwrap();
    let last = c.entries.last().unwrap().seq;
    let session = export_session_from(c, section.seq, last, 2.0, None, None).unwrap();
    assert_eq!(session.decks["B"], DeckSource::Net(spec()));
    assert_eq!(
        session.decks["A"],
        DeckSource::Sample {
            id: "demo-loop".into(),
            title: "demo loop".into(),
            grid: Some(djrust::deck::beat_grid::BeatGrid::new(120.0, 0.0, 1.0)),
        }
    );
    assert_eq!(session.params["A"].volume, 0.7);
    let kinds: Vec<_> = session.events.iter().map(|e| &e.kind).collect();
    assert!(
        kinds.contains(&&djrust::session::model::SessionEventKind::Action(play(
            "A", true
        ))),
        "a deck that was playing starts"
    );
    assert!(
        kinds.contains(&&djrust::session::model::SessionEventKind::Action(
            DeckAction::Lock {
                key: "B.section".into(),
                on: true
            }
        ))
    );
    let first_section = session
        .events
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                djrust::session::model::SessionEventKind::Action(DeckAction::Section { .. })
            )
        })
        .unwrap();
    assert_eq!(first_section.frame, 0);
    // a normal session: it round-trips by CID and renders
    let back = Session::decode(&session.canonical(), Some(&session.cid())).unwrap();
    assert_eq!(back, session);
    let audio = djrust::host::replay::render_session(&session, &Default::default())
        .unwrap()
        .audio;
    assert_eq!(audio.frames() as i64, session.frames);
    assert_eq!(
        export_session_from(c, last + 1, last + 5, 2.0, None, None).err(),
        Some(JournalError::EmptySlice)
    );
}

// JournalTests.JournalFileTests.everyInputPathLandsInTheJournal
#[test]
fn every_input_path_lands_in_the_journal() {
    let dir = tmp("paths");
    let mut c = offline_conductor(vec!["A".into(), "B".into()], 48_000.0);
    let journal =
        Journal::attach_with_clock(c.engine_mut(), &dir, 0, no_stores(), clock()).unwrap();
    c.rig.attach(APC);
    perform(&mut c, DeckAction::Crossfader(0.2)); // the UI, MCP and the control socket all come this way
    c.rig.route(APC, &[0x90, 48, 0x7F], 1.0); // a controller (and on-screen pads)
    c.engine_mut().load_demo("A");
    c.engine_mut().set_bpm("A", 100.0); // a tap (the tempo analysis lands the same way)
    let recent: Vec<JournalEvent> = journal.recent().into_iter().map(|e| e.event).collect();
    assert!(recent.contains(&JournalEvent::Action(DeckAction::Crossfader(0.2))));
    assert!(recent.contains(&JournalEvent::Midi {
        port: APC.into(),
        bytes: vec![0x90, 48, 0x7F]
    }));
    assert!(recent.contains(&JournalEvent::Action(DeckAction::Grid {
        deck: "A".into(),
        bpm: 100.0,
        offset: 0.0
    })));
    journal.flush();
    let file = read(&journal.path()).unwrap();
    let events: Vec<JournalEvent> = file.entries.iter().map(|e| e.event.clone()).collect();
    assert_eq!(events, recent);
    assert_eq!(journal.last_seq(), recent.len() as i64 - 1);
    journal.close(c.engine_mut());
    perform(&mut c, DeckAction::Crossfader(0.9));
    assert_eq!(
        read(&journal.path()).unwrap().entries.len(),
        recent.len(),
        "closed: nothing more"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// JournalTests.JournalFileTests.notesKeyTracksByConfiguration (the engine half)
#[test]
fn notes_key_tracks_by_configuration() {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], 48_000.0);
    let s = TrackSpec::new("techno", 5.0, Some("standard"));
    perform(&mut c, load_net("A", &s));
    perform(&mut c, load_net("B", &s));
    let a = c.engine().note_target("A").unwrap();
    assert_eq!(
        a.0,
        NoteTarget::Track {
            cid: djrust::cid::share_cid(&s.payload())
        }
    );
    assert_eq!(
        c.engine().note_target("B").unwrap().0,
        a.0,
        "the same configuration is the same track"
    );
    perform(&mut c, load_net("B", &s.next_version()));
    assert_ne!(
        c.engine().note_target("B").unwrap().0,
        a.0,
        "the next seed is another"
    );
    assert_eq!(
        c.engine().note_position("A"),
        Some(NotePosition::new(1, 1, Some(0.0)))
    );
    perform(&mut c, load_demo("B"));
    assert_eq!(
        c.engine().note_target("B").unwrap().0,
        NoteTarget::Sample {
            id: "demo-loop".into(),
            file: None
        }
    );
    // a sample's file is what a journal'd `sampleFile` event says
    let buffer = Arc::new(SampleBuffer::new(
        (0..48_000).map(|i| (i % 100) as f32 * 1e-3).collect(),
        None,
        48_000.0,
    ));
    c.engine_mut().perform(
        &DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Sample {
                id: String::new(),
                title: "t".into(),
                grid: None,
            },
        },
        PerformCtx {
            prepared: None,
            buffer: Some(buffer.clone()),
        },
    );
    c.engine_mut().journal_sample_file("A", "/a.caf");
    assert_eq!(
        c.engine().note_target("A").unwrap().0,
        NoteTarget::Sample {
            id: buffer.content_id(),
            file: Some("/a.caf".into())
        }
    );
    let fav = Favorite::new("t", DeckSource::Net(s.clone()), 0);
    assert_eq!(favorite_cid(&fav.source), fav.cid().into());
}

// `DJEngine.notePosition`: Swift's test reads only a net's first bar; the rest is its arithmetic
// (a sixteenth is a tick, so a net's bar is `tick / 16 + 1`, its beat `(tick % 16) / 4 + 1` and
// its time `tick * 15 / max(netBaseBPM, 1)` seconds), here at tick 37: bar 3, beat 2
#[test]
fn a_net_decks_note_position_is_a_bar_a_beat_and_a_time() {
    let mut c = offline_conductor(vec!["A".into()], 48_000.0);
    let mut s = JournalState::new(48_000.0);
    let mut a = DeckCheckpoint::new(DeckSource::Net(spec()));
    a.position = Some(37.0);
    s.decks.insert("A".into(), a);
    let warnings = c.restore_state(&s, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    c.step_block();
    let d = c.engine().deck("A").unwrap();
    assert!(!d.playing);
    assert_eq!(d.mirror.tick(), 37);
    let bpm = d.base_tempo;
    assert!(bpm >= 1.0);
    assert_eq!(
        c.engine().note_position("A"),
        Some(NotePosition::new(3, 2, Some(37.0 * 15.0 / bpm)))
    );
}

// JournalTests.JournalFileTests.aReplayEndsWithExactlyTheReplacedStores: a `stores.replace`
// the app journals on a rewind is the whole of the stores from there: what the newest journal
// says they hold, and what the checkpoint plus the store events add up to, is that, with
// nothing from before it left over
#[test]
fn a_journal_ends_with_exactly_the_replaced_stores() {
    let dir = tmp("replaced");
    let mut c = offline_conductor(vec!["A".into()], RATE);
    let start = JournalStoreState {
        crates: vec!["old".into()],
        notes: vec![TrackNote::new(
            "n-1",
            NoteTarget::Track { cid: "x".into() },
            "drop hits here",
            at_ms(1_760_000_000),
        )],
        ..Default::default()
    };
    let j = Journal::attach_with_clock(c.engine_mut(), &dir, at_ms(1_760_000_000), start, clock())
        .unwrap();
    j.record(
        0,
        JournalEvent::CrateAdd {
            name: "before".into(),
        },
    );
    let replaced = JournalStoreState {
        favorites: vec![Favorite::new("f", DeckSource::Net(spec()), at_ms(5))],
        crates: vec!["after".into()],
        ..Default::default()
    };
    j.record(0, JournalEvent::StoresReplace(replaced.clone()));
    j.record(0, JournalEvent::Action(play("A", true)));
    j.close(c.engine_mut());
    assert!(djrust::host::journal::latest_stores(&dir).unwrap() == replaced);
    let contents = read(&j.path()).unwrap();
    let mut stores = contents.checkpoint().unwrap().stores.clone();
    for e in contents.entries.iter().skip(1) {
        apply_store_event(&mut stores, &e.event);
    }
    assert!(stores == replaced);
    let _ = std::fs::remove_dir_all(&dir);
}

// MARK: JournalCheckpointTests

fn with_positions() -> JournalState {
    let mut s = JournalState::new(48_000.0);
    let mut a = DeckCheckpoint::new(DeckSource::Sample {
        id: DEMO_ID.into(),
        title: "demo".into(),
        grid: None,
    });
    a.position = Some(12_345.5);
    let mut b = DeckCheckpoint::new(DeckSource::Net(spec()));
    b.playing = true;
    b.position = Some(96.0);
    s.decks.insert("A".into(), a);
    s.decks.insert("B".into(), b);
    s.decks
        .insert("C".into(), DeckCheckpoint::new(DeckSource::Net(spec())));
    s
}

// JournalCheckpointTests.aPositionRoundTripsThroughTheCheckpointLine
#[test]
fn a_position_round_trips_through_the_checkpoint_line() {
    let e = JournalEntry {
        seq: 0,
        time: at_ms(1_760_000_000),
        frame: 0,
        event: JournalEvent::Checkpoint(with_positions()),
    };
    let back = JournalEntry::from_line(&e.line()).unwrap();
    assert_eq!(back.line(), e.line(), "re-encodes to the same bytes");
    let JournalEvent::Checkpoint(s) = back.event else {
        panic!("not a checkpoint")
    };
    assert_eq!(s.decks["A"].position, Some(12_345.5));
    assert_eq!(s.decks["B"].position, Some(96.0));
    assert_eq!(s.decks["C"].position, None, "written only when set");
}

// JournalCheckpointTests.aCheckpointKeepsPlayheadsOnlyWhenAsked
#[test]
fn a_checkpoint_keeps_playheads_only_when_asked() {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    run(&mut c, 40, |c| {
        perform(c, load_demo("A"));
        perform(c, load_net("B", &spec()));
        perform(c, play("A", true));
        perform(c, play("B", true));
    });
    let plain = c.engine().journal_state(no_stores(), false);
    assert!(plain.decks.values().all(|d| d.position.is_none()), "opt-in");
    assert!(!plain.json().canonical_js().contains("\"position\""));
    let kept = c.engine().journal_state(no_stores(), true);
    assert!(kept.decks["A"].position.unwrap_or(0.0) > 0.0);
    assert_eq!(
        kept.decks["B"].position,
        Some(c.engine().deck("B").unwrap().mirror.tick() as f64)
    );
}

// JournalCheckpointTests.aRestoreSeeksTheDecksToTheirPlayheads: a live run that plays both
// decks a while, stops them and lets them settle; its state with playheads, restored into a
// fresh engine, stands where it stood
#[test]
fn a_restore_seeks_the_decks_to_their_playheads() {
    let mut live = offline_conductor(vec!["A".into(), "B".into()], RATE);
    for b in 0..400 {
        live.step_with(|c| match b {
            0 => {
                perform(c, load_demo("A"));
                perform(c, load_net("B", &spec()));
                perform(c, play("A", true));
                perform(c, play("B", true));
            }
            250 => {
                perform(c, play("A", false));
                perform(c, play("B", false));
            }
            _ => {}
        });
    }
    let state = live.engine().journal_state(no_stores(), true);
    let sample_pos = state.decks["A"].position.unwrap();
    let tick = state.decks["B"].position.unwrap();
    assert!(
        sample_pos > 0.0 && tick > 0.0,
        "the decks moved: {sample_pos} {tick}"
    );
    // through the checkpoint's JSON, as a journal file holds it
    let read_back = JournalState::from_json(
        &djrust::json::JsonValue::parse(&state.json().canonical_js()).unwrap(),
    )
    .unwrap();
    let mut dj = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let warnings = dj.restore_state(&read_back, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    dj.step_block();
    assert!(!dj.engine().deck("A").unwrap().playing && !dj.engine().deck("B").unwrap().playing);
    assert_eq!(
        dj.engine().deck("B").unwrap().mirror.tick(),
        tick as i64,
        "the net tick is exact"
    );
    let at = dj.engine().deck("A").unwrap().mirror.turntable_position();
    assert!(
        (at - sample_pos).abs() <= 128.0,
        "sample within a block: {at} vs {sample_pos}"
    );
    assert_eq!(dj.dropped(), 0);
}

// MARK: mid-file checkpoints

struct MidRun {
    contents: Contents,
    live: JournalState,
    mids: Vec<JournalEntry>,
}

/// `JournalCheckpointTests.midRun`: a live run with two mid-file checkpoints (blocks 100 and
/// 250) between knobs, a section jump and store edits on both sides of them.
fn mid_run(tag: &str) -> MidRun {
    let dir = tmp(tag);
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let mut stores = JournalStoreState {
        crates: vec!["old".into()],
        ..Default::default()
    };
    let journal = Journal::attach_with_clock(
        c.engine_mut(),
        &dir,
        at_ms(1_760_000_000),
        stores.clone(),
        clock(),
    )
    .unwrap();
    let mut mids = Vec::new();
    for b in 0..400usize {
        c.step_with(|c| {
            let mut store = |c: &OfflineConductor, e: JournalEvent| {
                journal.record(c.engine().frame(), e.clone());
                apply_store_event(&mut stores, &e);
            };
            match b {
                0 => {
                    perform(c, load_demo("A"));
                    perform(c, load_net("B", &spec()));
                    perform(c, play("A", true));
                    perform(c, play("B", true));
                }
                20 => {
                    perform(c, param("A", "volume", 0.7));
                    perform(c, param("B", "volume", 0.0));
                    store(
                        c,
                        JournalEvent::FavoriteAdd(Favorite::new(
                            "B track",
                            DeckSource::Net(spec()),
                            at_ms(1_760_000_001),
                        )),
                    );
                }
                100 => mids.push(journal.checkpoint(c.engine(), true)),
                150 => {
                    perform(
                        c,
                        DeckAction::Section {
                            deck: "B".into(),
                            index: 1,
                        },
                    );
                    perform(c, DeckAction::Crossfader(0.3));
                    store(
                        c,
                        JournalEvent::NoteAdd(TrackNote::new(
                            "n-1",
                            NoteTarget::Track { cid: "x".into() },
                            "here",
                            at_ms(1_760_000_002),
                        )),
                    );
                }
                250 => mids.push(journal.checkpoint(c.engine(), true)),
                300 => {
                    perform(c, param("A", "volume", 0.5));
                    store(
                        c,
                        JournalEvent::CrateAdd {
                            name: "set 1".into(),
                        },
                    );
                }
                350 => perform(
                    c,
                    DeckAction::Lock {
                        key: "B.section".into(),
                        on: true,
                    },
                ),
                _ => {}
            }
        });
    }
    let live = c.engine().journal_state(stores, false);
    journal.close(c.engine_mut());
    let contents = read(&journal.path()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    MidRun {
        contents,
        live,
        mids,
    }
}

// JournalCheckpointTests.aMidCheckpointKeepsPlayheadsAndLeavesTheStoresOut
#[test]
fn a_mid_checkpoint_keeps_playheads_and_leaves_the_stores_out() {
    let run = mid_run("mid-shape");
    let c = &run.contents;
    assert_eq!(run.mids.len(), 2);
    let idx = c.checkpoint_indices();
    assert_eq!(idx.len(), 3, "line 0 and two mid ones");
    assert_eq!(idx[0], 0);
    assert_eq!(
        idx[1..]
            .iter()
            .map(|&i| c.entries[i].seq)
            .collect::<Vec<_>>(),
        run.mids.iter().map(|m| m.seq).collect::<Vec<_>>()
    );
    let line0 = c.entries[0].line();
    assert!(line0.contains("\"stores\""), "line 0 carries the stores");
    assert!(!line0.contains("\"position\""), "and no playheads");
    for m in &run.mids {
        let line = m.line();
        assert!(
            !line.contains("\"stores\""),
            "a mid checkpoint has no stores"
        );
        assert!(line.contains("\"position\""), "but the playheads");
        let back = JournalEntry::from_line(&line).unwrap();
        assert_eq!(back.line(), line, "re-encodes to the same bytes");
        let JournalEvent::Checkpoint(s) = &back.event else {
            panic!("not a checkpoint")
        };
        assert!(!s.has_stores);
        assert!(m.event.summary().starts_with("checkpoint (mid)"));
    }
    assert!(
        c.checkpoint().unwrap().has_stores,
        "Contents.checkpoint is still line 0's"
    );
}

// JournalCheckpointTests.aReplayStartsFromTheNearestCheckpointAndEndsInTheSameState
#[test]
fn a_replay_starts_from_the_nearest_checkpoint_and_ends_in_the_same_state() {
    let run = mid_run("mid-replay");
    let c = &run.contents;
    assert_eq!(run.mids.len(), 2);
    let (m1, m2) = (&run.mids[0], &run.mids[1]);
    let last = c.entries.last().unwrap().seq;
    let go = |o: JournalReplayOptions| replay(c, &o).unwrap();
    let opts =
        |to_seq: Option<i64>, end_frame: Option<i64>, from_start: bool| JournalReplayOptions {
            to_seq,
            end_frame,
            from_start,
            ..Default::default()
        };

    let full = go(opts(None, None, true));
    let near = go(opts(None, None, false));
    assert_eq!(full.start_seq, 0);
    assert_eq!(near.start_seq, m2.seq, "the last checkpoint");
    assert!(full.last_seq == last && near.last_seq == last);
    assert!(near.state == full.state);
    assert_eq!(
        near.state.json().canonical_js(),
        run.live.json().canonical_js(),
        "the live state"
    );
    assert!(
        near.stores() == &run.live.stores,
        "line 0's stores and every store event"
    );
    assert_eq!(near.stores().crates, ["old", "set 1"]);
    assert_eq!(near.stores().notes.len(), 1);

    // by sequence number: between the two mid checkpoints
    let between = m1.seq + 1;
    let a = go(opts(Some(between), None, false));
    let b = go(opts(Some(between), None, true));
    assert!(a.start_seq == m1.seq && b.start_seq == 0);
    assert_eq!(a.last_seq, b.last_seq);
    assert!(a.state == b.state);
    assert_eq!(
        a.stores().favorites.len(),
        1,
        "the favorite before the checkpoint is folded in"
    );

    // on a checkpoint exactly: it is the start, nothing after it is applied
    let on = go(opts(Some(m2.seq), None, false));
    assert!(on.start_seq == m2.seq && on.last_seq == m2.seq);
    assert!(on.state == go(opts(Some(m2.seq), None, true)).state);

    // before the first mid checkpoint: line 0
    assert_eq!(go(opts(Some(m1.seq - 1), None, false)).start_seq, 0);

    // by time: between the two, frames still count from line 0
    let origin = c.entries[0].frame;
    let t = (m1.frame + m2.frame) / 2 - origin;
    let by_time = go(opts(None, Some(t), false));
    assert_eq!(by_time.start_seq, m1.seq);
    assert!(by_time.frames >= t);
    assert!(by_time.state == go(opts(None, Some(t), true)).state);
}

// JournalCheckpointTests.checkpointsComeOnTheirCadence
#[test]
fn checkpoints_come_on_their_cadence() {
    let dir = tmp("cadence");
    let mut c = offline_conductor(vec!["A".into()], RATE);
    let j = Journal::attach_with_clock(c.engine_mut(), &dir, 0, no_stores(), clock()).unwrap();
    assert_eq!(j.cadence(), (60.0, 2000), "the defaults");
    j.set_cadence(0.5, 5);
    assert!(
        j.checkpoint_if_due(c.engine()).is_none(),
        "nothing recorded since line 0"
    );
    let mark = |n: &str| j.record(c.engine().frame(), JournalEvent::Mark(n.into()));
    for i in 0..4 {
        mark(&format!("m{i}"));
    }
    assert!(
        j.checkpoint_if_due(c.engine()).is_none(),
        "4 entries, no time"
    );
    mark("m4");
    let by_count = j.checkpoint_if_due(c.engine()).expect("5 entries");
    assert_eq!(by_count.seq, 6);
    assert!(j.checkpoint_if_due(c.engine()).is_none(), "just written");
    mark("later");
    let f0 = c.engine().frame();
    while c.engine().frame() - f0 < (0.4 * RATE) as i64 {
        c.step_block();
    }
    assert!(
        j.checkpoint_if_due(c.engine()).is_none(),
        "not half a second yet"
    );
    while c.engine().frame() - f0 < (0.5 * RATE) as i64 + 128 {
        c.step_block();
    }
    let by_time = j
        .checkpoint_if_due(c.engine())
        .expect("half a second and an entry");
    assert_eq!(by_time.seq, 8);
    let f1 = c.engine().frame();
    while c.engine().frame() - f1 < RATE as i64 {
        c.step_block();
    }
    assert!(
        j.checkpoint_if_due(c.engine()).is_none(),
        "an idle engine writes none"
    );
    j.close(c.engine_mut());
    let file = read(&j.path()).unwrap();
    assert_eq!(
        file.checkpoint_indices()
            .iter()
            .map(|&i| file.entries[i].seq)
            .collect::<Vec<_>>(),
        [0, 6, 8]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// JournalCheckpointTests.theLatestStoresSkipMidCheckpoints
#[test]
fn the_latest_stores_skip_mid_checkpoints() {
    let dir = tmp("latest");
    let mut c = offline_conductor(vec!["A".into()], RATE);
    let mut stores = JournalStoreState {
        crates: vec!["old".into()],
        ..Default::default()
    };
    let j = Journal::attach_with_clock(c.engine_mut(), &dir, 0, stores.clone(), clock()).unwrap();
    let mut store = |e: JournalEvent| {
        j.record(0, e.clone());
        apply_store_event(&mut stores, &e);
    };
    store(JournalEvent::CrateAdd {
        name: "before".into(),
    });
    j.checkpoint(c.engine(), true);
    store(JournalEvent::NoteAdd(TrackNote::new(
        "n",
        NoteTarget::Track { cid: "x".into() },
        "after",
        at_ms(1_760_000_000),
    )));
    j.checkpoint(c.engine(), true);
    j.close(c.engine_mut());
    let s = djrust::host::journal::latest_stores(&dir).unwrap();
    assert!(s == stores);
    assert_eq!(s.crates, ["old", "before"]);
    assert_eq!(
        s.notes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["n"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// MARK: JournalRewindTests (48 kHz)

const RR: f64 = 48_000.0;

/// `fxTestClip`: a one-bar clip at 120 bpm whose samples say which clip it is.
fn clip(n: usize) -> LoopClip {
    let len = 96_000;
    let l: Vec<f32> = (0..len)
        .map(|i| n as f32 * 0.01 + (i % 100) as f32 * 1e-4)
        .collect();
    let b = SampleBuffer::new(l.clone(), Some(l.iter().map(|v| -v).collect()), RR);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(
        &b.content_id(),
        &format!("fx loop {n}"),
        "A",
        1,
        120.0,
        0.0,
        Arc::new(b),
        w,
        None,
    )
}

struct Library(Vec<LoopClip>);

impl Library {
    fn find(&self, id: &str) -> Option<LoopClip> {
        self.0.iter().find(|c| c.id == id).cloned()
    }
}

fn rewind_engine(deterministic: bool) -> OfflineConductor {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RR);
    c.engine_mut().deterministic = deterministic;
    c
}

/// The target: A on the demo loop, playing; B on a net; clip 1 looping under the mixer, clip 2 on
/// the row, clip 3 removed; a scene; a lock; the mixer and row settings moved.
fn target(c: &mut OfflineConductor, clips: &[LoopClip]) -> JournalState {
    let cs = clips.to_vec();
    run(c, 4, |c| {
        perform(c, load_demo("A"));
        perform(c, load_net("B", &spec()));
        c.engine_mut().restore_clips(&cs);
        perform(
            c,
            DeckAction::ClipRemove {
                id: cs[2].id.clone(),
            },
        );
        perform(
            c,
            DeckAction::ClipGain {
                id: cs[0].id.clone(),
                value: 0.7,
            },
        );
        perform(
            c,
            DeckAction::ClipMute {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(c, play("A", true));
        perform(c, DeckAction::Crossfader(0.3));
        perform(c, DeckAction::LoopQuantize { beats: 8 });
        perform(
            c,
            DeckAction::Lock {
                key: "A.volume".into(),
                on: true,
            },
        );
        perform(c, param("B", "eqLow", 0.2));
    });
    c.engine_mut().start_loop(&clips[0].id, true);
    c.engine_mut().save_scene("Scene 1");
    run(c, 20, |_| {});
    state_of(c)
}

/// Everything moved on from the target, and a clip made after it.
fn dirty(c: &mut OfflineConductor, clips: &[LoopClip], new_clip: &LoopClip) {
    let (cs, nc) = (clips.to_vec(), new_clip.clone());
    run(c, 4, |c| {
        c.engine_mut().restore_clips(std::slice::from_ref(&nc));
        perform(
            c,
            DeckAction::ClipLoop {
                id: nc.id.clone(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::ClipRemove {
                id: cs[0].id.clone(),
            },
        );
        perform(
            c,
            DeckAction::ClipRestore {
                id: cs[2].id.clone(),
                at: Some(0),
            },
        );
        perform(
            c,
            DeckAction::ClipSolo {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::ClipKeyLock {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(c, load_net("A", &spec()));
        perform(
            c,
            DeckAction::Load {
                deck: "B".into(),
                source: DeckSource::Sample {
                    id: DEMO_ID.into(),
                    title: "d".into(),
                    grid: None,
                },
            },
        );
        perform(c, play("B", true));
        perform(
            c,
            DeckAction::Follow {
                deck: "A".into(),
                leader: Some("B".into()),
            },
        );
        perform(
            c,
            DeckAction::RoleDisable {
                deck: "A".into(),
                role: "kick".into(),
                on: true,
            },
        );
        perform(c, param("A", "volume", 0.2));
        perform(c, DeckAction::Crossfader(0.9));
        perform(c, DeckAction::Master(0.4));
        perform(
            c,
            DeckAction::Lock {
                key: "A.volume".into(),
                on: false,
            },
        );
        perform(
            c,
            DeckAction::Lock {
                key: "B.filter".into(),
                on: true,
            },
        );
        perform(c, DeckAction::LoopQuantize { beats: 16 });
        perform(c, DeckAction::LoopFxLink { on: true });
        perform(
            c,
            DeckAction::LoopFade {
                mode: LOOP_FADES.last().unwrap().to_string(),
            },
        );
        perform(c, DeckAction::TapBars { bars: Some(4) });
        perform(
            c,
            DeckAction::SceneSave {
                name: "Scene 2".into(),
            },
        );
        perform(
            c,
            DeckAction::SceneDelete {
                name: "Scene 1".into(),
            },
        );
    });
    run(c, 20, |_| {});
}

fn clips3() -> Vec<LoopClip> {
    vec![clip(1), clip(2), clip(3)]
}

// JournalRewindTests.aDirtyEngineResetEqualsTheTarget
#[test]
fn a_dirty_engine_reset_equals_the_target() {
    let mut c = rewind_engine(true);
    let lib = Library(clips3());
    let want = target(&mut c, &lib.0);
    let late = clip(9);
    dirty(&mut c, &lib.0, &late);
    assert!(!same(&state_of(&c), &want), "the dirty engine moved on");

    let warnings = c.reset_to(&want, &|id| lib.find(id));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(same(&state_of(&c), &want));
    assert!(
        c.engine().clip(&late.id).is_none(),
        "no clip from after the target"
    );
    assert!(
        !c.engine()
            .loops
            .row
            .removed
            .iter()
            .any(|r| r.clip.id == late.id),
        "nor remembered as removed"
    );
    assert!(!c.engine().looping_clips().contains(&late.id));
    assert!(c.engine().deck("A").unwrap().leader.is_none());
    assert!(c.engine().deck("A").unwrap().disabled_roles.is_empty());
    run(&mut c, 20, |_| {});
    assert!(
        same(&state_of(&c), &want),
        "and it stays there as it renders"
    );
    assert_eq!(c.dropped(), 0);
}

// JournalRewindTests.aResetMatchesARestoreIntoAFreshEngine
#[test]
fn a_reset_matches_a_restore_into_a_fresh_engine() {
    let mut c = rewind_engine(true);
    let lib = Library(clips3());
    let want = target(&mut c, &lib.0);
    dirty(&mut c, &lib.0, &clip(9));
    c.reset_to(&want, &|id| lib.find(id));

    let mut fresh = rewind_engine(true);
    fresh.restore_state(&want, &|id| lib.find(id));
    run(&mut fresh, 1, |_| {});
    assert!(same(&state_of(&c), &state_of(&fresh)));
    let ids = |c: &OfflineConductor| -> Vec<String> {
        c.engine()
            .loops
            .row
            .clips
            .iter()
            .map(|k| k.id.clone())
            .collect()
    };
    assert_eq!(ids(&c), ids(&fresh));
    let removed = |c: &OfflineConductor| -> Vec<String> {
        c.engine()
            .loops
            .row
            .removed
            .iter()
            .map(|r| r.clip.id.clone())
            .collect()
    };
    assert_eq!(removed(&c), removed(&fresh));
    assert_eq!(c.engine().scenes(), fresh.engine().scenes());
}

// JournalRewindTests.aResetToAFreshStateEmptiesTheEngine
#[test]
fn a_reset_to_a_fresh_state_empties_the_engine() {
    let mut c = rewind_engine(true);
    let lib = Library(clips3());
    let fresh = state_of(&rewind_engine(true));
    target(&mut c, &lib.0);
    c.reset_to(&fresh, &|id| lib.find(id));
    assert!(same(&state_of(&c), &fresh));
    let e = c.engine();
    assert!(
        e.loops.row.clips.is_empty() && e.loops.row.removed.is_empty() && e.scenes().is_empty()
    );
    assert!(e
        .decks
        .iter()
        .all(|d| !d.playing && d.sample.is_none() && d.project.is_none()));
    assert_eq!(
        e.loop_settings(),
        djrust::journal::replay::default_loop_settings()
    );
    run(&mut c, 40, |_| {});
    assert!(c.engine().looping_clips().is_empty(), "the loops faded out");
}

// JournalRewindTests.aClipMissingFromTheLibraryIsAWarning
#[test]
fn a_clip_missing_from_the_library_is_a_warning() {
    let mut c = rewind_engine(true);
    let lib = Library(clips3());
    let want = target(&mut c, &lib.0);
    let missing = lib.0[1].id.clone();
    let warnings = c.reset_to(&want, &|id| if id == missing { None } else { lib.find(id) });
    assert_eq!(warnings, [format!("clip {missing} has no audio")]);
    assert!(c.engine().clip(&missing).is_none());
}

// JournalRewindTests.aLiveResetSupersedesACompileInFlight
#[test]
fn a_live_reset_supersedes_a_compile_in_flight() {
    use djrust::deck::slots::SwapStart;
    let mut c = rewind_engine(false);
    let empty = state_of(&c);
    // a compose on its way (the worker thread), then the reset
    c.engine_mut().begin_generate("A", &spec(), SwapStart::Top);
    c.reset_to(&empty, &|_| None);
    // whatever it finishes lands nowhere
    let i = c.engine().index("A").unwrap();
    c.engine_mut().settle(i, true);
    c.step_block();
    assert!(c.engine().deck("A").unwrap().project.is_none());
    assert!(same(&state_of(&c), &empty));
}

// JournalRewindTests.aLiveResetAwaitsTheNetDecksJump
#[test]
fn a_live_reset_awaits_the_net_decks_jump() {
    let mut c = rewind_engine(false);
    let mut b = DeckCheckpoint::new(DeckSource::Net(spec()));
    b.position = Some(96.0);
    let mut want = JournalState::new(RR);
    want.decks
        .insert("A".into(), DeckCheckpoint::new(DeckSource::None));
    want.decks.insert("B".into(), b);
    perform(&mut c, load_net("B", &TrackSpec::new("house", 3.0, None)));
    run(&mut c, 8, |_| {});
    let warnings = c.reset_to(&want, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    run(&mut c, 1, |_| {});
    let d = c.engine().deck("B").unwrap();
    assert_eq!(d.spec.as_ref(), Some(&spec()));
    assert_eq!(d.mirror.tick(), 96, "at the checkpoint's tick");
    assert!(!d.playing);
}

// the rewind flow (`AppModel.rewindJournal`): the engine back where the journal stood, a mark and
// a checkpoint with playheads written, the journal still append-only
#[test]
fn a_rewind_stands_the_engine_back_and_marks_the_journal() {
    let dir = tmp("rewind");
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let j = Journal::attach_with_clock(c.engine_mut(), &dir, 0, no_stores(), clock()).unwrap();
    for b in 0..200usize {
        c.step_with(|c| match b {
            0 => {
                perform(c, load_demo("A"));
                perform(c, load_net("B", &spec()));
                perform(c, play("A", true));
                perform(c, play("B", true));
                perform(c, param("B", "volume", 0.0));
            }
            40 => perform(c, param("A", "volume", 0.7)),
            100 => perform(c, DeckAction::Crossfader(0.2)),
            150 => perform(c, param("A", "volume", 0.1)),
            _ => {}
        });
    }
    j.flush();
    let file = read(&j.path()).unwrap();
    let crossfade = file
        .entries
        .iter()
        .find(|e| matches!(e.event, JournalEvent::Action(DeckAction::Crossfader(_))))
        .unwrap()
        .seq;
    let last_before = j.last_seq();
    assert!(
        (state_of(&c).decks["A"].params.volume - 0.1).abs() < 1e-9,
        "moved on after the target"
    );
    // an entry that is not in the journal is refused
    {
        let OfflineConductor { rig, renderer, .. } = &mut c;
        let bad = j.rewind(
            &mut rig.engine,
            last_before + 1,
            None,
            None,
            &mut |_| renderer.drain_commands(),
            &mut |_| {},
        );
        assert!(matches!(
            bad,
            Err(djrust::host::journal::RewindError::BadSeq { .. })
        ));
    }
    let want = replay(
        &file,
        &JournalReplayOptions {
            to_seq: Some(crossfade),
            ..Default::default()
        },
    )
    .unwrap();
    let mut replaced = Vec::new();
    let r = {
        let OfflineConductor { rig, renderer, .. } = &mut c;
        j.rewind(
            &mut rig.engine,
            crossfade,
            None,
            None,
            &mut |_| renderer.drain_commands(),
            &mut |s| replaced.push(s.clone()),
        )
        .unwrap()
    };
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(r.seq, crossfade);
    assert_eq!(replaced.len(), 1);
    assert!(&replaced[0] == want.stores());
    run(&mut c, 1, |_| {});
    assert!(
        same(&state_of(&c), &want.state),
        "the engine stands where the journal stood"
    );
    assert_eq!(c.engine().deck("A").unwrap().volume, 0.7);
    // the journal stays append-only: a mark, then a checkpoint with the playheads
    j.flush();
    let after = read(&j.path()).unwrap();
    assert!(after.entries.len() as i64 > last_before + 2);
    assert!(after.entries[..=last_before as usize]
        .iter()
        .zip(&file.entries)
        .all(|(a, b)| a.line() == b.line()));
    let mark = &after.entries[(last_before + 1) as usize];
    assert_eq!(mark.event, JournalEvent::Mark(rewind_mark(crossfade)));
    assert_eq!(r.checkpoint_seq, last_before + 2);
    let JournalEvent::Checkpoint(cp) = &after.entries[(last_before + 2) as usize].event else {
        panic!("a checkpoint follows the mark")
    };
    assert!(!cp.has_stores);
    assert!(cp.decks["B"].position.is_some(), "with the playheads");
    // a replay past the rewind starts there, even from the start
    let from_start = replay(
        &after,
        &JournalReplayOptions {
            from_start: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(from_start.start_seq, r.checkpoint_seq);
    // and an export that spans it is refused
    assert_eq!(
        export_session_from(&after, crossfade, last_before + 2, 2.0, None, None).err(),
        Some(JournalError::SpansRewind(last_before + 1))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// MARK: what a replay cannot rebuild is a warning (Journal.apply, JournalState.restore)

/// A journal made by hand: the checkpoint, then `events` at their frames.
fn journal_of(state: JournalState, events: Vec<(i64, JournalEvent)>) -> Contents {
    let mut entries = vec![JournalEntry {
        seq: 0,
        time: 0,
        frame: 0,
        event: JournalEvent::Checkpoint(state),
    }];
    for (i, (frame, event)) in events.into_iter().enumerate() {
        entries.push(JournalEntry {
            seq: i as i64 + 1,
            time: 0,
            frame,
            event,
        });
    }
    Contents {
        entries,
        skipped: Vec::new(),
        truncated: false,
    }
}

fn tone(rate: f64) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer::new(
        (0..rate as usize * 2)
            .map(|i| ((i % 200) as f32 / 200.0 - 0.5) * 0.4)
            .collect(),
        None,
        rate,
    ))
}

fn sample_source(id: &str, title: &str) -> DeckSource {
    DeckSource::Sample {
        id: id.into(),
        title: title.into(),
        grid: None,
    }
}

// JournalReplay.swift: `sample <id> not found` per sample the resolver cannot supply; `samples`
// finds one by its content id (the deck then names it by that id again)
#[test]
fn a_sample_nobody_has_is_a_warning_and_one_the_lookup_has_is_found() {
    let buffer = tone(RATE);
    let id = buffer.content_id();
    let mut state = JournalState::new(RATE);
    state
        .decks
        .insert("A".into(), DeckCheckpoint::new(sample_source(&id, "file")));
    state.decks.insert(
        "B".into(),
        DeckCheckpoint::new(sample_source("deadbeef", "gone")),
    );
    let c = journal_of(state, vec![(0, JournalEvent::Mark("x".into()))]);
    let b = buffer.clone();
    let want = id.clone();
    let r = replay(
        &c,
        &JournalReplayOptions::default().with_samples(move |i| (i == want).then(|| b.clone())),
    )
    .unwrap();
    assert_eq!(r.warnings, ["sample deadbeef not found"]);
    assert_eq!(r.state.decks["A"].source, sample_source(&id, "file"));
    assert_eq!(r.state.decks["B"].source, DeckSource::None);
    // with no lookup at all, both are missing
    let none = replay(&c, &JournalReplayOptions::default()).unwrap();
    assert_eq!(
        none.warnings,
        [
            format!("sample {id} not found"),
            "sample deadbeef not found".into()
        ]
    );
    assert_eq!(none.state.decks["A"].source, DeckSource::None);
}

// JournalReplay.swift apply: an imported clip with its audio nowhere is a warning; with it in the
// lookup it lands on the row; a library restore needs the library's clip
#[test]
fn an_import_with_no_audio_and_a_library_restore_without_the_clip_are_warnings() {
    let buffer = tone(RATE);
    let id = buffer.content_id();
    let import = JournalEvent::Action(DeckAction::ClipImport {
        id: id.clone(),
        title: "found".into(),
        bpm: 120.0,
        beats: 4,
        provenance: None,
    });
    let lost = JournalEvent::Action(DeckAction::ClipImport {
        id: "nowhere".into(),
        title: String::new(),
        bpm: 120.0,
        beats: 4,
        provenance: None,
    });
    let restore = JournalEvent::LibraryRestore {
        id: "elsewhere".into(),
        index: 0,
    };
    let c = journal_of(
        JournalState::new(RATE),
        vec![(0, import), (100, lost), (200, restore)],
    );
    let b = buffer.clone();
    let want = id.clone();
    let r = replay(
        &c,
        &JournalReplayOptions::default().with_samples(move |i| (i == want).then(|| b.clone())),
    )
    .unwrap();
    assert_eq!(
        r.warnings,
        [
            "imported clip nowhere has no audio".to_string(),
            "library clip elsewhere has no audio".into()
        ]
    );
    assert_eq!(r.state.clips.len(), 1, "the one with audio is on the row");
    assert_eq!(r.state.clips[0].title, "found");
    // the library has the restored clip: it is remembered as removed, with its index
    let kept = clip(5);
    let restore = JournalEvent::LibraryRestore {
        id: kept.id.clone(),
        index: 3,
    };
    let c = journal_of(JournalState::new(RR), vec![(0, restore)]);
    let k = kept.clone();
    let r = replay(
        &c,
        &JournalReplayOptions::default().with_clips(move |i| (i == k.id).then(|| k.clone())),
    )
    .unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(r.state.removed, [(kept.id, 3)]);
}

// JournalReplay.swift apply: a clip a replay did not make comes from the library, so the moves on
// it still land (a live take that rendered differently)
#[test]
fn a_move_on_a_clip_the_replay_did_not_make_finds_it_in_the_library() {
    let kept = clip(6);
    let gain = JournalEvent::Action(DeckAction::ClipGain {
        id: kept.id.clone(),
        value: 0.4,
    });
    let c = journal_of(JournalState::new(RR), vec![(0, gain)]);
    let k = kept.clone();
    let with = replay(
        &c,
        &JournalReplayOptions::default().with_clips(move |i| (i == k.id).then(|| k.clone())),
    )
    .unwrap();
    assert_eq!(with.state.clips.len(), 1);
    assert_eq!(with.state.clips[0].gain, 0.4);
    let without = replay(&c, &JournalReplayOptions::default()).unwrap();
    assert!(without.state.clips.is_empty());
    assert!(without.warnings.is_empty());
}

// JournalReplay.swift replay: a journal that does not start with a checkpoint is refused
#[test]
fn a_journal_without_a_checkpoint_is_refused() {
    let c = parse("");
    assert_eq!(
        replay(&c, &JournalReplayOptions::default()).err(),
        Some(JournalError::NoCheckpoint)
    );
}

// the command ring holds 512 messages: a restore of a busy row between blocks loses none
#[test]
fn a_restore_of_a_busy_row_loses_no_command() {
    let clips: Vec<LoopClip> = (1..=60).map(clip).collect();
    let mut state = JournalState::new(RR);
    for k in &clips {
        let mut cp = djrust::journal::state::ClipCheckpoint::new(&k.id, &k.title);
        cp.looping = true;
        cp.gain = 0.8;
        cp.muted = true;
        state.clips.push(cp);
    }
    state
        .decks
        .insert("A".into(), DeckCheckpoint::new(DeckSource::Net(spec())));
    state
        .decks
        .insert("B".into(), DeckCheckpoint::new(DeckSource::Net(spec())));
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RR);
    let lib = Library(clips);
    let warnings = c.restore_state(&state, &|id| lib.find(id));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(c.dropped(), 0, "the ring never overflowed");
    assert_eq!(c.engine().loops.row.clips.len(), 60);
    // every voice the bank has is going (a row of 60 loops has 16 of them under the mixer)
    assert_eq!(
        c.engine().looping_clips().len(),
        djrust::host::deck::loops::LOOP_VOICES
    );
}

// MARK: FxUnitReplayTests

const FX_RATE: f64 = 22_050.0;

struct LoopMasterSet {
    session: Session,
    audio: RenderedAudio,
    journal: Contents,
}

/// `loopMasterSet` (LoopMasterSupport.swift): the demo loop on A with a one-bar LOOP take, and a
/// `script(bar, block)` poking the FX unit and the loops' master FX (bars of 128-frame blocks
/// since the start). Recorded by a session recorder and a journal side by side.
fn loop_master_set(
    tag: &str,
    blocks: usize,
    script: impl Fn(&mut OfflineConductor, usize, usize),
) -> LoopMasterSet {
    let dir = tmp(tag);
    let mut c = offline_conductor(vec!["A".into(), "B".into()], FX_RATE);
    let journal = Journal::attach_with_clock(
        c.engine_mut(),
        &dir,
        at_ms(1_760_000_000),
        no_stores(),
        clock(),
    )
    .unwrap();
    c.engine_mut().start_session().unwrap();
    let bar = (FX_RATE * 2.0) as usize / OfflineConductor::BLOCK_FRAMES;
    let mut audio = RenderedAudio {
        sample_rate: FX_RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    for b in 0..blocks {
        let (l, r) = c.step_with(|c| {
            if b == 0 {
                perform(c, load_demo("A"));
                perform(c, play("A", true));
            } else if b == bar + bar / 2 {
                perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: true,
                        bars: Some(1),
                    },
                );
            } else if b == bar + bar / 2 + 2 {
                perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: false,
                        bars: None,
                    },
                );
            }
            script(c, bar, b);
        });
        audio.left.extend_from_slice(l);
        audio.right.extend_from_slice(r);
    }
    assert_eq!(c.dropped(), 0);
    let session = c.engine_mut().finish_session().unwrap();
    journal.close(c.engine_mut());
    let contents = read(&journal.path()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    LoopMasterSet {
        session,
        audio,
        journal: contents,
    }
}

fn fx(deck: &str, param: &str, value: f64) -> DeckAction {
    DeckAction::Fx {
        deck: deck.into(),
        param: param.into(),
        value,
    }
}

/// The session replays to the byte, and so does the journal (from the checkpoint on, up to the
/// frame the run ended at).
fn replays_to_the_byte(live: &LoopMasterSet, blocks: usize) {
    let r = djrust::host::replay::render_session(&live.session, &Default::default()).unwrap();
    assert!(identical(&r.audio.left, &live.audio.left), "session replay");
    assert!(
        identical(&r.audio.right, &live.audio.right),
        "session replay"
    );
    let j = replay(
        &live.journal,
        &JournalReplayOptions {
            end_frame: Some((blocks * 128) as i64),
            render: true,
            ..Default::default()
        },
    )
    .unwrap();
    let audio = j.audio.unwrap();
    let n = audio.frames().min(live.audio.frames());
    assert!(n > blocks * 128 - 1024);
    assert!(
        identical(&audio.left[..n], &live.audio.left[..n]),
        "journal replay"
    );
}

// FxUnitTests.FxUnitReplayTests.apcFadersOnTheFxUnitReplayToTheByte: the APC's faders and kill /
// reset pads, moved through the router, replay to the byte from the session's and the journal's
// MIDI: deck A's filter sweep and pitch, then aimed at the loops, crush, a kill on the beat, a
// reset
#[test]
fn apc_faders_on_the_fx_unit_replay_to_the_byte() {
    let midi = |c: &mut OfflineConductor, bar: usize, b: usize| {
        let mut send = |bytes: &[u8]| {
            let now = c.now();
            c.rig.route(APC, bytes, now);
        };
        match b {
            0 => {
                c.rig.attach(APC);
            }
            x if x >= 2 * bar && x <= 2 * bar + 40 => {
                send(&[0xB0, 0x30, (10 + 2 * (b - 2 * bar)) as u8]) // filter
            }
            x if x == 3 * bar => send(&[0xB0, 0x36, 100]), // pitch
            x if x == 4 * bar => {
                // aim at the loops: SHIFT + aim A
                send(&[0x90, 0x7A, 0x7F]);
                send(&[0x90, 0x73, 0x7F]);
                send(&[0x80, 0x73, 0]);
                send(&[0x80, 0x7A, 0]);
            }
            x if x == 4 * bar + 3 => send(&[0xB0, 0x34, 90]), // crush on L
            x if x == 5 * bar => send(&[0xB0, 0x31, 50]),     // hp on L
            x if x == 6 * bar => send(&[0x90, 7, 0x7F]),      // kill, on the beat
            x if x == 6 * bar + 100 => send(&[0x90, 7, 0x7F]), // and back
            x if x == 7 * bar => send(&[0x90, 6, 0x7F]),      // reset L
            _ => {}
        }
    };
    let blocks = 8 * 344;
    let live = loop_master_set("fx-apc", blocks, |c, bar, b| {
        // (the pads attach at block 0, ahead of any press)
        midi(c, bar, b);
    });
    assert!(live.session.events.iter().any(|e| matches!(
        e.kind,
        djrust::session::model::SessionEventKind::Midi { .. }
    )));
    replays_to_the_byte(&live, blocks);
    let dry = loop_master_set("fx-apc-dry", blocks, |_, _, _| {});
    assert!(
        !identical(&dry.audio.left, &live.audio.left),
        "the faders were heard"
    );
}

// FxUnitTests.FxUnitReplayTests.aSetWithTheFxUnitReplaysToTheByte: a set that drags, kills (on the
// beat), resets and stacks a macro replays to the byte, by its session and by its journal
#[test]
fn a_set_with_the_fx_unit_replays_to_the_byte() {
    let script = |c: &mut OfflineConductor, bar: usize, b: usize| {
        if b >= 2 * bar && b <= 2 * bar + 40 {
            perform(c, fx("A", "filter", -0.8 + 0.03 * (b - 2 * bar) as f64));
        } else if b == 3 * bar {
            perform(c, fx("A", "pitch", 4.0));
        } else if b == 4 * bar {
            perform(c, fx("L", "crush", 0.5));
        } else if b == 4 * bar + 7 {
            perform(
                c,
                DeckAction::Macro {
                    deck: "L".into(),
                    id: "sweep-hp".into(),
                },
            );
        } else if b == 5 * bar {
            perform(c, fx("L", "hp", 0.4));
        } else if b == 6 * bar {
            perform(
                c,
                DeckAction::FxKill {
                    deck: "L".into(),
                    on: true,
                    on_beat: true,
                },
            );
        } else if b == 6 * bar + 100 {
            perform(
                c,
                DeckAction::FxKill {
                    deck: "L".into(),
                    on: false,
                    on_beat: false,
                },
            );
        } else if b == 7 * bar {
            perform(
                c,
                DeckAction::FxReset {
                    deck: "A".into(),
                    on_beat: true,
                },
            );
        } else if b == 7 * bar + 50 {
            perform(
                c,
                DeckAction::FxReset {
                    deck: "L".into(),
                    on_beat: false,
                },
            );
        }
    };
    let blocks = 9 * 344;
    let live = loop_master_set("fx-set", blocks, script);
    assert!(live.session.events.iter().any(|e| e.kind
        == djrust::session::model::SessionEventKind::Action(DeckAction::FxKill {
            deck: "L".into(),
            on: true,
            on_beat: true
        })));
    replays_to_the_byte(&live, blocks);
    let dry = loop_master_set("fx-set-dry", blocks, |_, _, _| {});
    assert!(
        !identical(&dry.audio.left, &live.audio.left),
        "the unit was heard"
    );
    let before = 2 * 344 * 128;
    assert!(identical(
        &dry.audio.left[..before],
        &live.audio.left[..before]
    ));
}

// MARK: auto-mix in the journal (AutoMixTests, the journal's halves)

const AM_RATE: f64 = 22_050.0;

fn am_spec() -> TrackSpec {
    TrackSpec::new("dnb", 42.0, None)
}

/// `AutoMixRig`'s two decks (A plays a net, B holds the demo loop), auto-mix on with 4-bar
/// hand-offs, A playing, Mix now: the hand-off bar is A's bar 8. The inputs of the first
/// blocks; what a journal attached before them records.
fn am_start(c: &mut OfflineConductor) -> RenderedAudio {
    let mut audio = run(c, 4, |c| {
        perform(c, load_net("A", &am_spec()));
        perform(c, load_demo("B"));
        perform(
            c,
            DeckAction::AutoMix {
                on: true,
                bars: 4,
                every: 0,
            },
        );
        perform(c, play("A", true));
    });
    let now = run(c, 1, |c| perform(c, DeckAction::AutoMixNow));
    audio.left.extend_from_slice(&now.left);
    audio.right.extend_from_slice(&now.right);
    audio
}

/// `AutoMixRig.run(until:)`: blocks until `done` (at most 40 000), their samples appended.
fn am_run_until(
    c: &mut OfflineConductor,
    audio: &mut RenderedAudio,
    done: impl Fn(&OfflineConductor) -> bool,
) -> bool {
    let mut n = 0;
    while !done(c) && n < 40_000 {
        let b = run(c, 1, |_| {});
        audio.left.extend_from_slice(&b.left);
        audio.right.extend_from_slice(&b.right);
        n += 1;
    }
    done(c)
}

/// The hand-off is under way and B is well into its mix-in.
fn am_mixing(c: &OfflineConductor) -> bool {
    let e = c.engine();
    e.auto_mix.phase == AutoMixPhase::Mixing && e.deck("B").unwrap().mirror.tick() > 20
}

fn am_handed_off(c: &OfflineConductor) -> bool {
    c.engine().auto_mix.log.len() == 1
}

struct HandOff {
    contents: Contents,
    /// The state at the end of the run (stores aside).
    live: JournalState,
    /// The mid-file checkpoint written with the hand-off under way (with playheads).
    mid: JournalEntry,
    /// The engine's state at that moment, without playheads, and its run.
    mid_state: JournalState,
    mid_run: AutoMixRun,
    audio: RenderedAudio,
}

/// A journaled set with one hand-off: A to B, a checkpoint taken while B mixes in.
fn hand_off_run(tag: &str) -> HandOff {
    let dir = tmp(tag);
    let mut c = offline_conductor(vec!["A".into(), "B".into()], AM_RATE);
    let journal = Journal::attach_with_clock(
        c.engine_mut(),
        &dir,
        at_ms(1_760_000_000),
        no_stores(),
        clock(),
    )
    .unwrap();
    let mut audio = am_start(&mut c);
    assert!(am_run_until(&mut c, &mut audio, am_mixing));
    let mid = journal.checkpoint(c.engine(), true);
    let mid_state = state_of(&c);
    let mid_run = c.engine().auto_mix.run.clone().expect("a hand-off runs");
    assert!(am_run_until(&mut c, &mut audio, am_handed_off));
    let tail = run(&mut c, 100, |_| {});
    audio.left.extend_from_slice(&tail.left);
    audio.right.extend_from_slice(&tail.right);
    assert_eq!(c.dropped(), 0);
    let live = state_of(&c);
    journal.close(c.engine_mut());
    let contents = read(&journal.path()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    HandOff {
        contents,
        live,
        mid,
        mid_state,
        mid_run,
        audio,
    }
}

// AutoMixTests.aCheckpointMidHandOffCarriesIt (the journal's half: `JournalState(engine:)`, the
// checkpoint's text, `restore(into:)` into a fresh engine): the run goes through the text and a
// fresh engine takes the hand-off up where it was, and ends it on its bar line
#[test]
fn a_checkpoint_mid_hand_off_carries_it() {
    let h = hand_off_run("am-checkpoint");
    let s = &h.mid_state;
    let v = s.json();
    let run_in = v
        .get("mixer")
        .and_then(|m| m.get("autoMix"))
        .and_then(|a| a.get("run"))
        .and_then(|r| r.get("in"))
        .and_then(|i| i.as_str());
    assert_eq!(run_in, Some("B"));
    let back = JournalState::from_json(&v).unwrap();
    assert!(&back == s);
    assert_eq!(back.auto_mix.run.as_ref(), Some(&h.mid_run));
    // the checkpoint the journal wrote is that state, with the playheads
    let JournalEvent::Checkpoint(written) = &h.mid.event else {
        panic!("not a checkpoint")
    };
    assert_eq!(written.auto_mix.run.as_ref(), Some(&h.mid_run));
    assert!(written.decks["A"].position.is_some());

    // stood in a fresh engine, it takes the hand-off up where it was
    let mut fresh = offline_conductor(vec!["A".into(), "B".into()], AM_RATE);
    let warnings = fresh.restore_state(&back, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    let e = fresh.engine();
    assert_eq!(e.auto_mix.run.as_ref(), Some(&h.mid_run));
    assert_eq!(e.auto_mix.settings, back.auto_mix.settings);
    assert_eq!(e.auto_mix.handoffs, 1);
    assert!(e.deck("A").unwrap().walk.held && e.deck("B").unwrap().walk.held);
    assert_eq!(e.deck("B").unwrap().leader.as_deref(), Some("A"));
    assert!(e.deck("A").unwrap().track.auto_mixing_out);
    assert_eq!(e.auto_mix.phase, AutoMixPhase::Preparing);
    assert!(same(&state_of(&fresh), s), "and reads as the checkpoint");
    // the hand-off still ends on its bar line: the walks let go, the decks swapped
    let mut sink = RenderedAudio {
        sample_rate: AM_RATE,
        left: Vec::new(),
        right: Vec::new(),
    };
    assert!(am_run_until(&mut fresh, &mut sink, am_handed_off));
    assert_eq!(fresh.dropped(), 0);
    let e = fresh.engine();
    assert!(e.auto_mix.run.is_none());
    assert_eq!(e.auto_mix.handoffs, 1);
    assert!(e.deck("B").unwrap().playing && !e.deck("A").unwrap().playing);
    assert!(!e.deck("A").unwrap().walk.held && !e.deck("B").unwrap().walk.held);
    assert_eq!(e.deck("B").unwrap().leader, None);
}

// AutoMixTests.aSetWithAutoMixReplaysToTheByte, through the journal: the actions that turned
// auto-mix on and asked for the hand-off replay the hand-off, to the state the live run ended in
// and, from line 0, the same samples
#[test]
fn a_journal_with_a_hand_off_replays_to_the_live_state_and_the_byte() {
    let h = hand_off_run("am-replay");
    let c = &h.contents;
    let actions: Vec<&DeckAction> = c
        .entries
        .iter()
        .filter_map(|e| match &e.event {
            JournalEvent::Action(a) => Some(a),
            _ => None,
        })
        .collect();
    assert!(actions.contains(&&DeckAction::AutoMixNow));
    assert!(actions.contains(&&DeckAction::AutoMix {
        on: true,
        bars: 4,
        every: 0
    }));
    assert_eq!(h.live.auto_mix.handoffs, 1);
    assert!(h.live.auto_mix.settings.on);
    assert_eq!(h.live.auto_mix.playing.as_deref(), Some("B"));

    let opts = JournalReplayOptions {
        render: true,
        from_start: true,
        end_frame: Some(h.audio.frames() as i64),
        ..Default::default()
    };
    let a = replay(c, &opts).unwrap();
    let b = replay(c, &opts).unwrap();
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    assert!(
        a.state == h.live,
        "{}\n{}",
        a.state.json().canonical_js(),
        h.live.json().canonical_js()
    );
    assert!(a.state == b.state);
    let (x, y) = (a.audio.unwrap(), b.audio.unwrap());
    assert!(identical(&x.left, &y.left) && identical(&x.right, &y.right));
    assert!(peak(&x.left) > 0.05, "it plays");
    let n = x.frames().min(h.audio.frames());
    assert!(
        n as i64 > h.mid.frame,
        "the hand-off is inside what was rendered"
    );
    assert!(identical(&x.left[..n], &h.audio.left[..n]), "the live set");
    assert!(identical(&x.right[..n], &h.audio.right[..n]));
}

// JournalCheckpointTests.aReplayStartsFromTheNearestCheckpointAndEndsInTheSameState, with the
// checkpoint taken while a hand-off was under way: the replay stands the hand-off up from it and
// ends where the live run, and the replay from line 0, ended
#[test]
fn a_replay_from_a_mid_hand_off_checkpoint_ends_where_the_live_run_did() {
    let h = hand_off_run("am-mid-replay");
    let c = &h.contents;
    let go = |from_start: bool| {
        replay(
            c,
            &JournalReplayOptions {
                from_start,
                end_frame: Some(h.audio.frames() as i64),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let near = go(false);
    let full = go(true);
    assert_eq!(full.start_seq, 0);
    assert_eq!(near.start_seq, h.mid.seq, "the checkpoint with the run");
    assert!(near.warnings.is_empty(), "{:?}", near.warnings);
    assert!(
        near.state == full.state,
        "{}\n{}",
        near.state.json().canonical_js(),
        full.state.json().canonical_js()
    );
    assert!(near.state == h.live);
    assert_eq!(near.state.auto_mix.handoffs, 1);
    assert!(near.state.auto_mix.run.is_none(), "the hand-off ended");
}

// DJEngine.reset(to:clip:): `restoreAutoMix(AutoMixState())` first, a hand-off in flight lets go
// of its decks; and a reset to a state with one takes it up, as a restore into a fresh engine
// does (JournalRewindTests.aResetMatchesARestoreIntoAFreshEngine)
#[test]
fn a_reset_lets_go_of_a_hand_off_and_stands_one() {
    let h = hand_off_run("am-reset");
    // an engine with a hand-off in flight, reset to a state with auto-mix at its default
    let mut c = offline_conductor(vec!["A".into(), "B".into()], AM_RATE);
    let mut sink = am_start(&mut c);
    assert!(am_run_until(&mut c, &mut sink, am_mixing));
    assert!(c.engine().auto_mix.run.is_some());
    let fresh = state_of(&offline_conductor(vec!["A".into(), "B".into()], AM_RATE));
    let warnings = c.reset_to(&fresh, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    let e = c.engine();
    assert!(e.auto_mix.run.is_none());
    assert!(e.auto_mix.settings.is_default() && e.auto_mix.handoffs == 0);
    assert_eq!(e.auto_mix.phase, AutoMixPhase::Off);
    assert!(!e.deck("A").unwrap().walk.held && !e.deck("B").unwrap().walk.held);
    assert!(!e.deck("A").unwrap().track.auto_mixing_out);
    assert_eq!(e.deck("B").unwrap().leader, None);
    assert!(same(&state_of(&c), &fresh));
    run(&mut c, 40, |_| {});
    assert!(
        c.engine().auto_mix.run.is_none() && c.engine().auto_mix.log.is_empty(),
        "and nothing hands off any more"
    );
    assert!(same(&state_of(&c), &fresh), "it stays there as it renders");

    // the same engine reset to the checkpoint with the run: the hand-off is stood up
    let warnings = c.reset_to(&h.mid_state, &|_| None);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(c.engine().auto_mix.run.as_ref(), Some(&h.mid_run));
    assert!(same(&state_of(&c), &h.mid_state));
    let mut restored = offline_conductor(vec!["A".into(), "B".into()], AM_RATE);
    restored.restore_state(&h.mid_state, &|_| None);
    run(&mut restored, 1, |_| {});
    run(&mut c, 1, |_| {});
    assert!(same(&state_of(&c), &state_of(&restored)));
    assert_eq!(c.engine().auto_mix.run, restored.engine().auto_mix.run);
    assert_eq!(c.dropped(), 0);
}
