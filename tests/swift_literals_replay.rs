//! Swift tests that replay a recorded set, or level a take, and had no Rust counterpart, for
//! behaviour the engine already has (package F of the parity audit): sample-deck auto-gain
//! through sessions and journals (`SampleAutoGainTests`, `LevelMatchTests`), a take of a net deck
//! at the deck's loudness (`LevelMatchTests`), a set with clip-FX pokes replayed to the byte
//! (`LoopFxTests`) and the master macros on the loop channel (`LoopMasterFxTests`). Each test
//! names the Swift test it ports. Replay audio is compared Rust-live with Rust-replay, as in
//! `deck_session.rs` (PORTING.md section H).

mod common_row;

use std::sync::atomic::Ordering;

use common_row::{Rig, BAR, SR};
use djrust::deck::level_match::SampleLevel;
use djrust::fx::MasterMacro;
use djrust::journal::file::Contents;
use djrust::musicality::loudness::IntegratedLoudness;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckParams, DeckSource, Session, SessionEvent, DEMO_ID};
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::session::wav::RenderedAudio;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, OfflineConductor, PerformCtx};
use djrust::host::journal::{read, Journal};
use djrust::host::replay::{offline_conductor, render_session, replay, JournalReplayOptions, RenderOptions};

const RATE: f64 = 22_050.0;

fn perform(c: &mut OfflineConductor, a: DeckAction) {
    c.rig.engine.perform(&a, PerformCtx::default());
}

fn param(deck: &str, name: &str, value: f64) -> DeckAction {
    DeckAction::Param {
        deck: deck.into(),
        name: name.into(),
        value,
    }
}

fn play(deck: &str) -> DeckAction {
    DeckAction::Play {
        deck: deck.into(),
        on: true,
    }
}

fn identical(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn render(s: &Session) -> RenderedAudio {
    render_session(s, &RenderOptions::default()).unwrap().audio
}

fn append(out: &mut RenderedAudio, l: &[f32], r: &[f32]) {
    out.left.extend_from_slice(l);
    out.right.extend_from_slice(r);
}

fn empty_audio(rate: f64) -> RenderedAudio {
    RenderedAudio {
        sample_rate: rate,
        left: Vec::new(),
        right: Vec::new(),
    }
}

// MARK: SampleAutoGainTests

struct DemoSet {
    session: Session,
    audio: RenderedAudio,
    journal: Option<Contents>,
    /// Deck B's `autoGain` at the end.
    auto_gain: bool,
}

/// `SampleAutoGainTests.playDemo`: deck B plays the demo loop (levelled: its auto-gain is not 0),
/// auto-gain `start_on` when the set starts, switched off at block `off_at` (None: never);
/// recorded live, journaled too if asked.
fn play_demo(blocks: usize, start_on: bool, off_at: Option<usize>, journal: bool, tag: &str) -> DemoSet {
    let mut c = offline_conductor(vec!["A".into(), "B".into()], RATE);
    // the app sets it from its preference before anything loads
    perform(&mut c, param("B", "autoGain", if start_on { 1.0 } else { 0.0 }));
    c.rig.engine.load_demo("B");
    let dir = std::env::temp_dir().join(format!("swift-literals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let j = journal.then(|| {
        let mut t = 1_760_000_000_000i64;
        Journal::attach_with_clock(
            c.engine_mut(),
            &dir,
            t,
            Default::default(),
            Box::new(move || {
                t += 1;
                t
            }),
        )
        .unwrap()
    });
    c.engine_mut().start_session().unwrap();
    let mut audio = empty_audio(RATE);
    for b in 0..blocks {
        let (l, r) = c.step_with(|c| {
            if b == 0 {
                perform(c, play("B"));
            }
            if Some(b) == off_at {
                perform(c, param("B", "autoGain", 0.0));
            }
        });
        let (l, r) = (l.to_vec(), r.to_vec());
        append(&mut audio, &l, &r);
    }
    let session = c.engine_mut().finish_session().unwrap();
    let contents = j.map(|j| {
        j.close(c.engine_mut());
        let contents = read(&j.path()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        contents
    });
    DemoSet {
        session,
        audio,
        journal: contents,
        auto_gain: c.engine().auto_gain("B"),
    }
}

// SampleAutoGainTests.aSessionRecordedWithAutoGainOffReplaysWithItOff: a set recorded with
// auto-gain off (the user's choice, over the app's default) replays with it off: byte for byte
// the audio, at the sample's own level.
#[test]
fn a_session_recorded_with_auto_gain_off_replays_with_it_off() {
    let demo = djrust::deck::demo_loop::make(RATE);
    assert_ne!(SampleLevel::measure(&demo).auto_gain_db(), 0.0, "the demo is levelled");
    let off = play_demo(120, false, None, false, "off");
    assert!(!off.auto_gain);
    assert!(!off.session.params["B"].auto_gain);
    assert!(!off.session.canonical().contains("autoGain"), "off is the wire default");
    let replay = render(&off.session);
    assert!(replay.left.iter().any(|v| *v != 0.0));
    assert!(identical(&replay.left, &off.audio.left));
    assert!(identical(&replay.right, &off.audio.right));
    // it really is off: the same set with it on plays differently, and replays too
    let on = play_demo(120, true, None, false, "on");
    assert!(on.session.params["B"].auto_gain);
    assert!(!identical(&on.audio.left, &off.audio.left));
    assert!(identical(&render(&on.session).left, &on.audio.left));
}

// SampleAutoGainTests.switchingItOffMidSetReplays: switched off during the set, the setting is
// an action, so the replay switches it at the same frame.
#[test]
fn switching_it_off_mid_set_replays() {
    let live = play_demo(160, true, Some(60), false, "mid");
    assert!(!live.auto_gain);
    assert!(live.session.params["B"].auto_gain);
    let toggles = live
        .session
        .events
        .iter()
        .filter(|e| {
            matches!(&e.kind, djrust::session::model::SessionEventKind::Action(
                DeckAction::Param { name, .. }) if name == "autoGain")
        })
        .count();
    assert_eq!(toggles, 1);
    let replay = render(&live.session);
    assert!(identical(&replay.left, &live.audio.left));
    assert!(identical(&replay.right, &live.audio.right));
}

// SampleAutoGainTests.aJournalReplaysTheSetting (arguments true / false): the checkpoint holds
// the deck setting, the switch is an action; the replay ends with it off and plays what the live
// set did.
fn a_journal_replays_the_setting(start_on: bool) {
    let tag = if start_on { "journal-on" } else { "journal-off" };
    let live = play_demo(160, start_on, start_on.then_some(60), true, tag);
    let contents = live.journal.as_ref().expect("a journal");
    assert_eq!(contents.checkpoint().unwrap().decks["B"].params.auto_gain, start_on);
    let r = replay(
        contents,
        &JournalReplayOptions {
            render: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(!r.state.decks["B"].params.auto_gain);
    let audio = r.audio.unwrap();
    let n = audio.frames().min(live.audio.frames());
    assert!(n > 0);
    assert!(identical(&audio.left[..n], &live.audio.left[..n]));
}

#[test]
fn a_journal_replays_the_setting_on() {
    a_journal_replays_the_setting(true);
}

#[test]
fn a_journal_replays_the_setting_off() {
    a_journal_replays_the_setting(false);
}

// LevelMatchTests.anOldSessionWithoutTheKeyReplaysAsBefore: a session from before level
// matching has no `autoGain` key: it replays its sample at the sample's own level, byte for byte
// what it played; one recorded with it on is levelled.
#[test]
fn an_old_session_without_the_key_replays_as_before() {
    let rate = 24_000.0;
    let session = |params: Option<DeckParams>| {
        Session::new(
            rate,
            24_000,
            [(
                "A".to_string(),
                DeckSource::Sample {
                    id: DEMO_ID.into(),
                    title: "demo".into(),
                    grid: None,
                },
            )]
            .into(),
            params.map(|p| [("A".to_string(), p)].into()).unwrap_or_default(),
            0.5,
            1.0,
            vec![SessionEvent::action(0, play("A"))],
        )
    };
    let old = session(Some(DeckParams::default()));
    assert!(!old.canonical().contains("autoGain"));
    let revived = Session::decode(&old.canonical(), Some(&old.cid())).unwrap();
    assert!(!revived.params["A"].auto_gain);
    let replay = render(&revived);
    assert!(replay.left.iter().any(|v| *v != 0.0));
    // the same as no params at all, and as the setting off
    assert!(identical(&render(&session(None)).left, &replay.left));
    let off = DeckParams {
        auto_gain: false,
        ..Default::default()
    };
    assert!(identical(&render(&session(Some(off))).left, &replay.left));
    // on: the demo loop is levelled, so it plays differently
    assert_ne!(
        SampleLevel::measure(&djrust::deck::demo_loop::make(rate)).auto_gain_db(),
        0.0
    );
    let on = DeckParams {
        auto_gain: true,
        ..Default::default()
    };
    let levelled = session(Some(on));
    assert!(levelled.canonical().contains("autoGain") && levelled.cid() != old.cid());
    assert!(!identical(&render(&levelled).left, &replay.left));
}

// MARK: LevelMatchTests.aTakeOfANetDeckPlaysAtTheDecksLevel

/// A take of a net deck is the deck after its EQ (flat here), so on another deck (no auto-gain:
/// a clip keeps the level it was taken at) it plays at the net deck's loudness.
#[test]
fn a_take_of_a_net_deck_plays_at_the_decks_level() {
    // the app's engine: decks A and B, the app's options (loudness gain, limiter)
    let (e, r) = engine(EngineConfig::new(SR));
    let mut rig = Rig {
        e,
        r,
        master: Vec::new(),
    };
    let mut spec = TrackSpec::new("techno", 4242.0, None);
    spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
    assert!(rig.e.generate("A", &spec));
    rig.e.play("A");
    rig.run_to(2 * BAR + 100);
    assert!(rig.e.loop_record("A", true, Some(2)));
    assert!(rig.e.loop_record("A", false, None)); // a tap: exactly the 2 bars
    let id = rig.run_until_clip(6 * BAR).expect("the take lands");
    let clip = rig.e.clip(&id).unwrap();
    let take_l: Vec<f32> = clip.buffer.left.to_vec();
    let take_r: Vec<f32> = clip.buffer.right().to_vec();
    assert_eq!(take_l.len(), 2 * BAR as usize);
    let lufs = |l: &[f32], r: &[f32]| IntegratedLoudness::integrated(l, Some(r), SR).unwrap_or(-200.0);
    // the deck's own loudness over the same span (its meter, pre-EQ = post-flat-EQ)
    let deck_lufs = rig.e.deck("A").unwrap().mirror.short_term_lufs();
    let take_lufs = lufs(&take_l, &take_r);
    assert!((take_lufs - deck_lufs).abs() < 1.5, "take {take_lufs}, deck {deck_lufs}");

    // on B: no auto-gain on a clip, and it sounds at the take's level
    rig.e.pause("A");
    rig.e.perform(
        &DeckAction::Clip {
            deck: "B".into(),
            id: id.clone(),
        },
        PerformCtx::default(),
    );
    assert!(rig.e.sample_level("B").is_none() && rig.e.auto_gain_db("B") == 0.0);
    rig.e.play("B");
    let start = rig.e.deck("B").unwrap().mirror.rendered_frames.load(Ordering::Acquire);
    rig.run_to(rig.frame() + 2 * BAR + 24_000);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    rig.e.deck("B").unwrap().mirror.capture.copy(
        start + 24_000,
        start + 24_000 + 2 * BAR,
        &mut l,
        &mut r,
    );
    assert_eq!(l.len(), 2 * BAR as usize);
    // (within 1 LU, not 0.5: the clip is seeked onto A's beat at a fractional sample, and the
    // turntable's interpolation there softens the top that K-weighting counts: Swift's comment)
    let on_b = lufs(&l, &r);
    assert!((on_b - take_lufs).abs() < 1.0, "B {on_b}, take {take_lufs}");
}

// MARK: LoopFxTests.aSetWithFxChangesReplaysToTheByte

/// `loopFxSet`: a recorded set on the demo loop: a one-bar take looping under the mixer, its FX
/// set to `first`, then changed to `second` mid-loop, then off. The session and what played.
fn loop_fx_set(first: LoopFx, second: LoopFx, blocks: usize) -> (Session, RenderedAudio) {
    let rate = 48_000.0;
    let mut c = offline_conductor(vec!["A".into(), "B".into()], rate);
    c.rig.engine.load_demo("A");
    c.engine_mut().start_session().unwrap();
    let mut audio = empty_audio(rate);
    let bar = (rate as usize * 2) / 128; // the demo loop is 120 bpm; blocks of 128 frames
    let mut id: Option<String> = None;
    for b in 0..blocks {
        let (l, r) = c.step_with(|c| {
            if b == 0 {
                perform(c, play("A"));
            } else if b == bar + bar / 2 {
                perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: true,
                        bars: None,
                    },
                );
            } else if b == 2 * bar + bar / 2 {
                perform(
                    c,
                    DeckAction::LoopRecord {
                        deck: "A".into(),
                        on: false,
                        bars: None,
                    },
                );
            }
            if id.is_none() {
                if let Some(clip) = c.engine().loops.row.clips.first() {
                    let cid = clip.id.clone();
                    perform(
                        c,
                        DeckAction::ClipFx {
                            id: cid.clone(),
                            fx: first.clone(),
                        },
                    );
                    id = Some(cid);
                }
            }
            if let Some(cid) = &id {
                if b == 4 * bar {
                    perform(
                        c,
                        DeckAction::ClipFx {
                            id: cid.clone(),
                            fx: second.clone(),
                        },
                    );
                }
                if b == 5 * bar + 7 {
                    perform(
                        c,
                        DeckAction::ClipFx {
                            id: cid.clone(),
                            fx: LoopFx::default(),
                        },
                    );
                }
            }
        });
        let (l, r) = (l.to_vec(), r.to_vec());
        append(&mut audio, &l, &r);
    }
    (c.engine_mut().finish_session().unwrap(), audio)
}

#[test]
fn a_set_with_fx_changes_replays_to_the_byte() {
    let first = LoopFx::presets()[2].1.clone();
    let second = LoopFx {
        drive: 18.0,
        mode: LoopFxMode::Fold,
        amount: 0.6,
        tone: 0.5,
        ..Default::default()
    };
    let (session, live) = loop_fx_set(first, second, 4500);
    let pokes = session
        .events
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                djrust::session::model::SessionEventKind::Action(DeckAction::ClipFx { .. })
            )
        })
        .count();
    assert_eq!(pokes, 3, "on, changed, off: {pokes}");
    let a = render(&session);
    assert!(identical(&a.left, &live.left), "the replay is the live set");
    assert!(identical(&a.right, &live.right), "the replay is the live set");
    // and the FX were heard: the set differs from the same set with no FX at all
    let (_, dry) = loop_fx_set(LoopFx::default(), LoopFx::default(), 4500);
    assert!(!identical(&dry.left, &live.left));
}

// MARK: LoopMasterFxTests.netMacrosDoNothingOnLAndPanicRestores

/// Net macros mean nothing on the loop channel (no net there); the master macros do, and the
/// panic puts the chain back. (`deck_loop_master.rs` holds the panic half through the engine's
/// own methods; this goes through `perform`, as the Swift test does.)
#[test]
fn net_macros_do_nothing_on_l_and_panic_restores() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let mut run = |e: &mut djrust::host::deck::Engine, frames: usize| {
        for _ in 0..frames.div_ceil(512) {
            r.render_block(&mut l, &mut rr);
            e.service_loops();
        }
    };
    let mac = |id: &str| DeckAction::Macro {
        deck: "L".into(),
        id: id.into(),
    };
    e.perform(&mac("drop"), PerformCtx::default());
    e.perform(&mac("breakdown"), PerformCtx::default());
    assert!(e.active_loop_master_macros().is_empty());
    e.perform(&mac("riser"), PerformCtx::default());
    e.perform(&mac("build-crush"), PerformCtx::default());
    run(&mut e, BAR as usize + 8192);
    let mut got = e.active_loop_master_macros();
    got.sort_by_key(|m| m.raw());
    let mut want = vec![MasterMacro::Riser, MasterMacro::BuildCrush];
    want.sort_by_key(|m| m.raw());
    assert_eq!(got, want);
    e.perform(
        &DeckAction::Panic { deck: "L".into() },
        PerformCtx::default(),
    );
    run(&mut e, 2048);
    assert!(e.active_loop_master_macros().is_empty());
    run(&mut e, 24_000);
    assert!(e.loop_master_filter_corners().2, "bypassed again");
}
