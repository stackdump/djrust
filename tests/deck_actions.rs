//! `Engine::perform(DeckAction)` against the Swift suites' own literals (parity form (a)); the
//! Swift-generated state goldens are `perform_golden.rs`. Each test names the Swift test it
//! ports. The ones that replay a recorded set to the byte (`SessionPlayer`, the journal's
//! replay) are the replay slice's: here the half that records.

mod common_track;

use common_track::Stepper;
use djrust::control::controller::SurfaceHost;
use djrust::fx::unit::FxParam;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, SessionError, SessionEventKind};
use djrust::wave::VoiceTweak;
use djrust::TrackSpec;
use djrust::host::deck::{engine, Applied, EngineConfig, PerformCtx, Rig};

const SR: f64 = 22_050.0;

fn spec(genre: &str, seed: f64, structure: Option<&str>) -> TrackSpec {
    TrackSpec::new(genre, seed, structure)
}

fn load(deck: &str, s: &TrackSpec) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Net(s.clone()),
    }
}

fn perform(st: &mut Stepper, a: DeckAction) -> Applied {
    st.e.perform(&a, PerformCtx::default())
}

fn shaped() -> VoiceTweak {
    VoiceTweak {
        gain_db: Some(9.0),
        attack: Some(0.04),
        decay_scale: Some(2.0),
        max_decay: Some(3.0),
        lowpass: Some(900.0),
        distortion: Some(0.4),
        vibrato_rate: Some(6.0),
        vibrato_depth: Some(0.02),
        pitch_from: Some(3.0),
        pitch_to: Some(1.0),
        pitch_time: Some(0.15),
    }
}

// DisableTests.aDisabledRoleStaysOffThroughANewTrack
#[test]
fn a_disabled_role_stays_off_through_a_new_track() {
    let mut st = Stepper::new(SR, true);
    perform(&mut st, load("A", &spec("techno", 42.0, None)));
    perform(
        &mut st,
        DeckAction::Play {
            deck: "A".into(),
            on: true,
        },
    );
    st.step(60);
    assert!(st.e.is_role_audible("A", "bass"));
    perform(
        &mut st,
        DeckAction::RoleDisable {
            deck: "A".into(),
            role: "bass".into(),
            on: true,
        },
    );
    st.step(60);
    assert!(!st.e.is_role_audible("A", "bass"));
    assert_eq!(st.e.deck("A").unwrap().disabled_roles, ["bass"]);
    // clicking it does nothing while it is disabled
    assert!(!st.e.toggle_role("A", "bass"));
    perform(
        &mut st,
        DeckAction::Role {
            deck: "A".into(),
            role: "bass".into(),
            on: true,
        },
    );
    st.step(60);
    assert!(!st.e.is_role_audible("A", "bass"));
    // a re-generation: the new song has no bass playing
    perform(&mut st, load("A", &spec("techno", 7.0, None)));
    for _ in 0..40 {
        st.step(60);
    }
    assert_eq!(st.e.deck("A").unwrap().spec.as_ref().unwrap().seed, 7.0);
    assert!(
        !st.e.is_role_audible("A", "bass"),
        "off in the new track too"
    );
    // a section that unmutes it is put back by the housekeeping tick
    st.e.set_role("A", "bass", true);
    st.step(60);
    st.step(60);
    assert!(!st.e.is_role_audible("A", "bass"));
    // enabled again: it plays (only the mute comes off, as in Swift)
    perform(
        &mut st,
        DeckAction::RoleDisable {
            deck: "A".into(),
            role: "bass".into(),
            on: false,
        },
    );
    st.step(60);
    assert!(st.e.is_role_audible("A", "bass") && st.e.deck("A").unwrap().disabled_roles.is_empty());
}

// SessionTests.refusesToStartMidPerformance
#[test]
fn a_session_will_not_start_mid_performance() {
    let mut st = Stepper::new(SR, true);
    st.e.load_demo("A");
    st.e.load_demo("B");
    st.e.play("A");
    assert_eq!(
        st.e.start_session().unwrap_err(),
        SessionError::NotAtRest { deck: "A".into() }
    );
    st.e.pause("A");
    st.e.hot_cue("B", 0);
    assert_eq!(
        st.e.start_session().unwrap_err(),
        SessionError::NotAtRest { deck: "B".into() }
    );
}

// SessionTests.capturesTheStartingState
#[test]
fn a_session_captures_the_starting_state() {
    let mut st = Stepper::new(SR, true);
    perform(&mut st, load("A", &spec("jazz", 3.0, None)));
    st.e.load_demo("B");
    st.e.set_volume("A", 0.31);
    st.e.set_pitch("B", -0.25);
    st.e.set_crossfader(0.2);
    st.e.set_master(0.55);
    st.e.start_session().unwrap();
    assert!(st.e.is_recording());
    let s = st.e.finish_session().unwrap();
    assert!(!st.e.is_recording());
    assert_eq!(s.decks["A"], DeckSource::Net(spec("jazz", 3.0, None)));
    match &s.decks["B"] {
        DeckSource::Sample { id, title, grid } => {
            assert_eq!((id.as_str(), title.as_str()), ("demo-loop", "demo loop"));
            assert_eq!(grid.map(|g| (g.bpm, g.offset)), Some((120.0, 0.0)));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.params["A"].volume, 0.31);
    assert_eq!(s.params["B"].pitch, -0.25);
    // (the master is the mixer node's Float: Swift's test says `abs(s.master - 0.55) < 1e-6` too)
    assert_eq!(s.crossfader, 0.2);
    assert!((s.master - 0.55).abs() < 1e-6);
    assert_eq!(s.master, 0.55f32 as f64);
    assert!(s.events.is_empty() && s.frames == 0);
}

// WanderTests.WanderReplayTests.aReplayWalksTheSamePath (the recording half; the replay is the
// replay slice's): a lock set before the recording starts is part of its starting state, the
// walk's controls are recorded as the actions that made them, and the moves it takes are not
// recorded (they are derived)
#[test]
fn a_walk_is_recorded_by_its_controls_and_its_locks_are_the_starting_state() {
    let mut st = Stepper::new(SR, true);
    let mut s = spec("techno", 42.0, Some("standard"));
    s.extra.set("bpm", JsonValue::Number(240.0));
    perform(&mut st, load("A", &s));
    st.e.load_demo("B");
    st.e.set_wander_lock("A", "genre", true);
    st.e.set_wander_lock("A", "instruments", true);
    st.e.start_session().unwrap();
    for act in [
        DeckAction::WanderPace {
            deck: "A".into(),
            bars: 4,
        },
        DeckAction::Wander {
            deck: "A".into(),
            on: true,
        },
        DeckAction::Play {
            deck: "A".into(),
            on: true,
        },
    ] {
        assert_eq!(perform(&mut st, act), Applied::Done);
    }
    st.step(700);
    let lock = DeckAction::WanderLock {
        deck: "A".into(),
        dimension: "fx".into(),
        on: true,
    };
    assert_eq!(perform(&mut st, lock), Applied::Done);
    st.step(700);
    let log = st.e.deck("A").unwrap().walk.wander.log.clone();
    assert!(
        log.len() >= 2,
        "{:?}",
        log.iter().map(|m| m.detail.clone()).collect::<Vec<_>>()
    );
    let session = st.e.finish_session().unwrap();
    let locks = |p: &djrust::session::model::DeckParams| {
        p.wander.locks.iter().cloned().collect::<Vec<_>>()
    };
    assert_eq!(locks(&session.params["A"]), ["genre", "instruments"]);
    let ops: Vec<String> = session
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(a) => Some(a.op().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(ops, ["wanderPace", "wander", "play", "wanderLock"]);
    // a replay's engine starts its walk from that state: no log, no bar seen
    let mut st2 = Stepper::new(SR, true);
    perform(&mut st2, load("A", &s));
    st2.e.apply_params("A", &session.params["A"]);
    let w = &st2.e.deck("A").unwrap().walk.wander;
    assert_eq!(w.locks, session.params["A"].wander.locks);
    assert!(w.log.is_empty() && w.last_bar.is_none() && !w.on);
}

// DeckNetTests.aRawNetsSharePlaysOnADeck (the recorded half)
#[test]
fn a_raw_nets_share_loads_by_perform_and_is_recorded_as_such() {
    let path = format!(
        "{}/tests/fixtures/js/share-urls.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    let rows = doc.get("rawNets").and_then(JsonValue::as_array).unwrap();
    let row = rows
        .iter()
        .find(|r| {
            r.get("payload")
                .and_then(|p| p.get("name"))
                .and_then(JsonValue::as_str)
                == Some("Hand Made")
        })
        .unwrap();
    let payload = row.get("payload").unwrap().clone();
    let mut st = Stepper::new(SR, true);
    st.e.start_session().unwrap();
    let a = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::RawNets(payload.clone()),
    };
    assert_eq!(st.e.perform(&a, PerformCtx::default()), Applied::Done);
    let s = st.e.finish_session().unwrap();
    assert_eq!(
        s.events.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [SessionEventKind::Action(a)]
    );
    let d = st.e.deck("A").unwrap();
    assert!(d.is_net() && d.spec.is_none());
    assert_eq!(d.track.raw_nets.as_ref(), Some(&payload));
    assert_eq!(
        d.project.as_ref().unwrap().name,
        row.get("project")
            .unwrap()
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap()
    );
    // it names itself again (a session could start from it)
    assert_eq!(
        st.e.source_descriptor("A").unwrap(),
        DeckSource::RawNets(payload)
    );
}

// MixOutActionTests.aSetWithAMixOutReplaysToTheByte (the recording half): one recorded action
// each, nothing for the relocations
#[test]
fn a_set_with_a_mix_out_records_one_action_each() {
    let mut st = Stepper::new(SR, true);
    perform(&mut st, load("A", &spec("techno", 42.0, Some("standard"))));
    st.e.load_demo("B");
    st.e.start_session().unwrap();
    for b in 0..1_400 {
        match b {
            0 => {
                perform(
                    &mut st,
                    DeckAction::Play {
                        deck: "A".into(),
                        on: true,
                    },
                );
            }
            10 => {
                perform(
                    &mut st,
                    DeckAction::MixIn {
                        deck: "A".into(),
                        bars: 1,
                    },
                );
            }
            400 => {
                perform(
                    &mut st,
                    DeckAction::MixOut {
                        deck: "A".into(),
                        bars: 1,
                    },
                );
            }
            _ => {}
        }
        st.block();
    }
    let s = st.e.finish_session().unwrap();
    let actions: Vec<DeckAction> = s
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEventKind::Action(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        [
            DeckAction::Play {
                deck: "A".into(),
                on: true
            },
            DeckAction::MixIn {
                deck: "A".into(),
                bars: 1
            },
            DeckAction::MixOut {
                deck: "A".into(),
                bars: 1
            },
        ]
    );
    assert_eq!(
        s.events.iter().map(|e| e.frame).collect::<Vec<_>>(),
        [0, 1_280, 51_200],
        "stamped with the engine frame they arrived on"
    );
    // the JSON round-trips with the actions in it
    assert_eq!(
        djrust::session::model::Session::decode(&s.canonical(), Some(&s.cid())).unwrap(),
        s
    );
}

// VoiceTweakTests.VoiceTweakDeckTests.tweaksAreDeckActionsAndSurviveARecompileOfTheSong
#[test]
fn tweaks_are_deck_actions_and_survive_a_recompile_of_the_song() {
    let mut st = Stepper::new(SR, true);
    let s = spec("techno", 42.0, Some("minimal"));
    perform(&mut st, load("A", &s));
    let voices = st.e.role_voices("A");
    let melody = voices
        .iter()
        .find(|v| v.role.contains("melody"))
        .unwrap()
        .clone();
    let ch = melody.channel;
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: ch,
            params: shaped(),
        },
    );
    assert_eq!(st.e.voice_tweak("A", ch), shaped());
    assert_eq!(
        st.e.voice_setting("A", ch).unwrap().instrument,
        melody.instrument
    );
    st.block();
    let gen =
        st.e.deck("A")
            .unwrap()
            .track
            .latest
            .as_ref()
            .unwrap()
            .generation;
    // a section jump / relocation prepares the song again: the new project carries the tweak
    st.e.relocate("A", 64, None, None);
    assert_ne!(
        st.e.deck("A")
            .unwrap()
            .track
            .latest
            .as_ref()
            .unwrap()
            .generation,
        gen,
        "a new project"
    );
    assert_eq!(st.e.voice_tweak("A", ch), shaped());
    perform(
        &mut st,
        DeckAction::VoiceSynth {
            deck: "A".into(),
            channel: ch,
            on: true,
        },
    );
    assert!(st.e.uses_synth_voice("A", ch) && st.e.voice_tweak("A", ch) == shaped());
    perform(
        &mut st,
        DeckAction::VoiceSynth {
            deck: "A".into(),
            channel: ch,
            on: false,
        },
    );
    // Reset
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: ch,
            params: VoiceTweak::default(),
        },
    );
    assert!(st.e.voice_setting("A", ch).is_none());
}

// VoiceTweakTests.VoiceTweakDeckTests.anotherInstrumentPickKeepsTheOtherChannelsTweaks
#[test]
fn another_instrument_pick_keeps_the_other_channels_tweaks() {
    let mut st = Stepper::new(SR, true);
    let s = spec("techno", 42.0, Some("minimal"));
    perform(&mut st, load("A", &s));
    let voices = st.e.role_voices("A");
    let melody = voices
        .iter()
        .find(|v| v.role.contains("melody"))
        .unwrap()
        .clone();
    let bass = voices
        .iter()
        .find(|v| v.role.contains("bass"))
        .unwrap()
        .clone();
    let new_bass = bass
        .options
        .iter()
        .find(|o| **o != bass.instrument)
        .unwrap()
        .clone();
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: melody.channel,
            params: shaped(),
        },
    );
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: bass.channel,
            params: VoiceTweak {
                gain_db: Some(-3.0),
                ..Default::default()
            },
        },
    );
    st.block();
    let source =
        st.e.deck("A")
            .unwrap()
            .net_source()
            .unwrap()
            .with_instrument(&new_bass, bass.channel)
            .unwrap();
    perform(
        &mut st,
        DeckAction::Load {
            deck: "A".into(),
            source,
        },
    );
    assert_eq!(
        st.e.voice_tweak("A", melody.channel),
        shaped(),
        "the song's other tweaks stay"
    );
    assert!(
        st.e.voice_setting("A", bass.channel).is_none(),
        "a new instrument starts from its own voice"
    );
}

// VoiceTweakTests.VoiceTweakDeckTests.aFavoriteReloadOrAnotherSongStartsFromTheInstrumentsOwnVoice
#[test]
fn a_reload_or_another_song_starts_from_the_instruments_own_voice() {
    let mut st = Stepper::new(SR, true);
    let s = spec("techno", 42.0, Some("minimal"));
    perform(&mut st, load("A", &s));
    let ch =
        st.e.role_voices("A")
            .iter()
            .find(|v| v.role.contains("melody"))
            .unwrap()
            .channel;
    st.block();
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: ch,
            params: shaped(),
        },
    );
    // the same song loaded again (a favorite of it, its share pasted)
    let again = st.e.deck("A").unwrap().net_source().unwrap();
    perform(
        &mut st,
        DeckAction::Load {
            deck: "A".into(),
            source: again,
        },
    );
    assert!(st.e.voice_setting("A", ch).is_none());
    // another song (Generate, auto-next's next version)
    perform(
        &mut st,
        DeckAction::VoiceTweak {
            deck: "A".into(),
            channel: ch,
            params: shaped(),
        },
    );
    perform(&mut st, load("A", &s.next_version()));
    assert!(st.e.deck("A").unwrap().voice_settings.is_empty());
}

// FxUnitTests.FxUnitDeckTests.theSettingFollowsTheDeckAcrossModes
#[test]
fn the_fx_setting_follows_the_deck_across_modes() {
    let mut st = Stepper::new(SR, true);
    st.e.load_demo("A");
    perform(
        &mut st,
        DeckAction::Fx {
            deck: "A".into(),
            param: "drive".into(),
            value: 0.4,
        },
    );
    perform(
        &mut st,
        DeckAction::Fx {
            deck: "L".into(),
            param: "filter".into(),
            value: -0.5,
        },
    );
    assert_eq!(st.e.fx_unit("A").unwrap().get(FxParam::Drive), 0.4);
    assert_eq!(st.e.fx_unit("L").unwrap().get(FxParam::Filter), -0.5);
    assert!(st.e.fx_unit("B").unwrap().is_neutral());
    assert!(
        !st.e.fx_unit_controls("A").contains(&FxParam::Room),
        "a sample deck has no room send"
    );
    assert!(st.e.fx_unit_missing("A", FxParam::Echo).is_some());
    assert!(st.e.fx_unit_missing("L", FxParam::Room).is_some());
    assert!(st.e.fx_unit_missing("A", FxParam::Crush).is_none());
    perform(
        &mut st,
        DeckAction::FxKill {
            deck: "A".into(),
            on: true,
            on_beat: false,
        },
    );
    let u = st.e.fx_unit("A").unwrap();
    assert!(u.killed && !u.is_active() && !u.is_neutral());
    perform(
        &mut st,
        DeckAction::FxReset {
            deck: "A".into(),
            on_beat: true,
        },
    );
    let u = st.e.fx_unit("A").unwrap();
    assert!(u.is_neutral() && !u.killed);
    assert!(st.e.current_params("A").unwrap().fx_unit.is_neutral());
    assert_eq!(
        st.e.current_params("L")
            .unwrap()
            .fx_unit
            .get(FxParam::Filter),
        -0.5
    );
    // a starting state restored at once
    let mut s = djrust::fx::unit::FxUnitSettings::new();
    s.set(FxParam::Crush, 0.3);
    let mut p = st.e.current_params("A").unwrap();
    p.fx_unit = s;
    st.e.apply_params("A", &p);
    assert_eq!(st.e.fx_unit("A").unwrap().get(FxParam::Crush), 0.3);
    // a net deck with a room and an echo has those sends
    perform(&mut st, load("B", &spec("trance", 7.0, Some("minimal"))));
    let c = st.e.fx_unit_controls("B");
    // FxUnitRealTimeTests: a trance net deck on the app's options has both sends (the extended
    // engine's decks play every fix, `.space` and the echo among them)
    assert!(c.contains(&FxParam::Room) && c.contains(&FxParam::Echo));
    let l = st.e.deck("B").unwrap().track.latest.as_ref().unwrap();
    assert_eq!(c.contains(&FxParam::Room), l.space.is_some());
    assert_eq!(c.contains(&FxParam::Echo), l.echo.is_some());
}

// StingerCurationTests.aDeckFiresItsAssignments: a deck's Hits are its track's, they follow a
// palette change, and every Hit fires without touching the composition
#[test]
fn a_deck_fires_its_assignments() {
    let mut st = Stepper::new(SR, true);
    st.e.deterministic = true;
    let s = spec("house", 11.0, Some("standard"));
    // curated Hits are the app's options (`WaveOptions.stingers = .here`, which the extended
    // engine's deck options hold: `Stepper::new(.., true)`)
    assert_eq!(
        st.e.deck("A").unwrap().wave_options().stingers,
        Some(djrust::wave::StingerPolicy::Here)
    );
    let preparer = st.e.preparer("A").unwrap();
    let track = djrust::host::deck::PreparedTrack::compose(&s, &preparer, 0);
    st.e.perform(
        &load("A", &s),
        PerformCtx {
            prepared: Some(track),
            buffer: None,
        },
    );
    let hits = st.e.stingers("A");
    assert_eq!(hits.len(), 4);
    let here = |e: &djrust::host::deck::Engine| {
        let set = djrust::sound::PaletteSet::shared();
        let project = e.deck("A").unwrap().project.as_ref().unwrap();
        e.with_sounds(|ctx| {
            let installed = djrust::sound::stinger::StingerContent::here(set, ctx, Vec::new());
            project.resolved_stingers("house", 11.0, &installed, set)
        })
    };
    assert_eq!(hits, here(&st.e));
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("orchestral".into()),
        },
    );
    let after = st.e.stingers("A");
    assert_eq!(after, here(&st.e));
    let d = st.e.deck("A").unwrap();
    assert!(d
        .spec
        .as_ref()
        .is_some_and(|s| s.genre == "house" && s.seed == 11.0));
    // every Hit fires without touching the composition
    let before = d.project.as_ref().unwrap().canonical_json();
    for n in 0..4 {
        perform(
            &mut st,
            DeckAction::Stinger {
                deck: "A".into(),
                n,
            },
        );
    }
    assert_eq!(
        st.e.deck("A")
            .unwrap()
            .project
            .as_ref()
            .unwrap()
            .canonical_json(),
        before
    );
}

// ---- the controller path through the same methods (SurfaceNetTests) ----

const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";
const MPK: &str = "MPK Mini Mk II";

struct Audio {
    r: djrust::host::deck::Renderer,
}

fn rig_with_audio(structure: Option<&str>) -> (Rig, Audio) {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.load_demo("B");
    e.generate("A", &spec("techno", 42.0, structure));
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    r.render_block(&mut l, &mut rr);
    (Rig::new(e), Audio { r })
}

impl Audio {
    /// Render `frames`, returning the loudest sample.
    fn render(&mut self, frames: usize) -> f32 {
        let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
        let mut peak = 0f32;
        for _ in 0..frames.div_ceil(512) {
            self.r.render_block(&mut l, &mut rr);
            peak = l.iter().chain(&rr).fold(peak, |p, v| p.max(v.abs()));
        }
        peak
    }
}

fn press(rig: &mut Rig, port: &str, bytes: &[u8]) {
    let t = rig.recorded.len() as f64;
    rig.route(port, bytes, 100.0 + t);
}

// SurfaceNetTests.inpulseStemsButtonsMuteTheMusicOrTheLead
#[test]
fn inpulse_stems_buttons_mute_the_music_or_the_lead() {
    let (mut rig, mut audio) = rig_with_audio(None); // a loop: every role plays
    assert!(rig.attach(INPULSE));
    rig.engine.play("A");
    audio.render(2048);
    let roles = rig.engine.roles("A");
    let music: Vec<&String> = roles
        .iter()
        .filter(|r| ["bass", "melody", "harmony", "arp"].contains(&r.as_str()))
        .collect();
    assert!(music.iter().any(|r| *r == "melody"));
    assert!(music.iter().all(|r| rig.engine.is_role_audible("A", r)));
    assert!(rig.engine.is_role_audible("A", "kick"));
    rig.led_frames(1.0);
    // STEMS music (note icon, 0x09): everything but the drums goes, and its LED lights
    press(&mut rig, INPULSE, &[0x91, 0x09, 0x7F]);
    press(&mut rig, INPULSE, &[0x91, 0x09, 0x00]);
    audio.render(2048);
    assert!(music.iter().all(|r| !rig.engine.is_role_audible("A", r)));
    assert!(rig.engine.is_role_audible("A", "kick"));
    assert!(rig.engine.is_stem_muted("A", "music") && !rig.engine.is_stem_muted("A", "mic"));
    let lit = |frames: Vec<(String, Vec<[u8; 3]>)>| -> Vec<[u8; 3]> {
        frames.into_iter().flat_map(|(_, f)| f).collect()
    };
    assert!(lit(rig.led_frames(2.0)).contains(&[0x91, 0x09, 0x7F]));
    // STEMS mic (0x0A) on top, then music back: the lead stays out until mic is pressed again
    press(&mut rig, INPULSE, &[0x91, 0x0A, 0x7F]);
    press(&mut rig, INPULSE, &[0x91, 0x09, 0x7F]);
    audio.render(2048);
    assert!(rig.engine.is_role_audible("A", "bass") && !rig.engine.is_role_audible("A", "melody"));
    assert!(!rig.engine.is_stem_muted("A", "music"));
    press(&mut rig, INPULSE, &[0x91, 0x0A, 0x7F]);
    audio.render(2048);
    assert!(music.iter().all(|r| rig.engine.is_role_audible("A", r)));
    assert!(!rig.engine.is_stem_muted("A", "mic"));
    // only changes go out: mic's LED went on and off between frames, music's went off
    assert_eq!(lit(rig.led_frames(3.0)), [[0x91, 0x09, 0x00]]);
    // deck B's buttons are deck B's
    press(&mut rig, INPULSE, &[0x92, 0x0A, 0x7F]);
    assert!(rig.engine.focus == "B" && !rig.engine.is_stem_muted("A", "mic"));
}

// SurfaceNetTests.aStemBringsBackOnlyWhatItSilenced: a song's intro has its music silent by
// arrangement: muting and unmuting the stem must not bring it in early
#[test]
fn a_stem_brings_back_only_what_it_silenced() {
    let (mut rig, mut audio) = rig_with_audio(Some("standard"));
    assert!(rig.attach(INPULSE));
    rig.engine.play("A");
    audio.render(2048);
    let silent: Vec<String> = rig
        .engine
        .roles("A")
        .into_iter()
        .filter(|r| {
            ["bass", "melody", "harmony", "arp"].contains(&r.as_str())
                && !rig.engine.is_role_audible("A", r)
        })
        .collect();
    assert!(!silent.is_empty(), "the intro holds some of the music back");
    press(&mut rig, INPULSE, &[0x91, 0x09, 0x7F]);
    audio.render(2048);
    assert!(rig.engine.is_stem_muted("A", "music"));
    press(&mut rig, INPULSE, &[0x91, 0x09, 0x7F]);
    audio.render(2048);
    assert!(!rig.engine.is_stem_muted("A", "music"));
    assert!(silent.iter().all(|r| !rig.engine.is_role_audible("A", r)));
}

// SurfaceNetTests.apcKillAndResetPadsActOnTheAimedTarget: pad 7 toggles the kill and pad 6
// resets, on the aimed target (the loop channel too)
#[test]
fn apc_kill_and_reset_pads_act_on_the_aimed_target() {
    let (mut rig, _audio) = rig_with_audio(None);
    assert!(rig.attach(APC));
    // djrust's APC layout (docs/HUB.md M4): SHIFT + fader 5 is the crush
    press(&mut rig, APC, &[0x90, 0x7A, 0x7F]);
    press(&mut rig, APC, &[0xB0, 0x34, 127]); // crush
    press(&mut rig, APC, &[0x80, 0x7A, 0x00]);
    press(&mut rig, APC, &[0x90, 7, 0x7F]); // kill
    press(&mut rig, APC, &[0x80, 7, 0]);
    let u = rig.engine.fx_unit("A").unwrap();
    assert!(u.killed && u.get(FxParam::Crush) == 1.0);
    press(&mut rig, APC, &[0x90, 7, 0x7F]); // kill off again
    assert!(!rig.engine.fx_unit("A").unwrap().killed);
    press(&mut rig, APC, &[0x90, 6, 0x7F]); // reset
    let u = rig.engine.fx_unit("A").unwrap();
    assert!(u.is_neutral() && !u.killed);
    // aimed at the loops: the same pads act there
    press(&mut rig, APC, &[0x90, 0x7A, 0x7F]);
    press(&mut rig, APC, &[0x90, 0x73, 0x7F]); // SHIFT + aim A = L
    press(&mut rig, APC, &[0x80, 0x7A, 0x00]);
    assert_eq!(rig.engine.focus, "L");
    press(&mut rig, APC, &[0x90, 7, 0x7F]);
    assert!(rig.engine.fx_unit("L").unwrap().killed && !rig.engine.fx_unit("A").unwrap().killed);
    // and the second press lets it go: the pad reads the loop channel's own state
    press(&mut rig, APC, &[0x90, 7, 0x7F]);
    assert!(!rig.engine.fx_unit("L").unwrap().killed);
}

// SurfaceNetTests.apcMacroPadsFireMacrosAndPanicRestores
#[test]
fn apc_macro_pads_fire_macros_and_panic_restores() {
    let (mut rig, mut audio) = rig_with_audio(None);
    assert!(rig.attach(APC));
    rig.engine.play("A");
    audio.render(2048);
    assert!(rig.engine.is_role_audible("A", "bass") && rig.engine.is_role_audible("A", "kick"));
    press(&mut rig, APC, &[0x90, 0, 0x7F]); // bottom row, col 0: drop (aimed deck A)
    audio.render(2048);
    assert!(!rig.engine.is_role_audible("A", "bass") && rig.engine.is_role_audible("A", "kick"));
    press(&mut rig, APC, &[0x90, 0x77, 0x7F]); // scene 8: panic
    audio.render(2048);
    assert!(rig.engine.is_role_audible("A", "bass"));
    press(&mut rig, APC, &[0x90, 1, 0x7F]); // breakdown mutes the drums
    audio.render(2048);
    assert!(!rig.engine.is_role_audible("A", "kick") && rig.engine.is_role_audible("A", "bass"));
}

// SurfaceNetTests.macroPadsDoNothingOnASampleDeck
#[test]
fn macro_pads_do_nothing_on_a_sample_deck() {
    let (mut rig, _audio) = rig_with_audio(Some("standard"));
    assert!(rig.attach(APC));
    assert!(!rig.engine.deck("B").unwrap().is_net());
    // aimed at B (a sample): its drop pad, a part mute: must not crash or change anything
    press(&mut rig, APC, &[0x90, 0x74, 0x7F]);
    press(&mut rig, APC, &[0x90, 0, 0x7F]);
    press(&mut rig, APC, &[0x90, 100, 0x7F]);
    let b = rig.engine.deck("B").unwrap();
    assert!(!b.is_net() && rig.engine.roles("B").is_empty());
    assert!(
        rig.engine.unhandled.is_empty(),
        "{:?}",
        rig.engine.unhandled
    );
}

// MasterFxDeckTests.aSampleDeckSliderAndPanicReachItsBus, and the actions of
// MasterFxSessionTests.aSampleDeckSetWithMasterMacrosReplaysToTheByte: a master macro, a slider
// and a panic aimed at a sample deck reach the deck's own chain, and are not "unhandled"
#[test]
fn a_sample_decks_macro_slider_and_panic_reach_its_chain() {
    use djrust::fx::{MasterFxKey, MasterMacro};
    let mut st = Stepper::new(SR, true);
    let demo = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Sample {
            id: djrust::session::model::DEMO_ID.into(),
            title: "demo loop".into(),
            grid: None,
        },
    };
    perform(&mut st, demo);
    perform(
        &mut st,
        DeckAction::Play {
            deck: "A".into(),
            on: true,
        },
    );
    st.step(8);
    assert!(!st.e.deck("A").unwrap().is_net());
    // `deck.fire(.sweepHp, duration: 4)`, `deck.setMasterFx(.lpFreq, 40)`
    assert!(
        st.e.fire_macro_id("A", "sweep-hp", Some(4)),
        "a master macro on a sample deck fires"
    );
    st.e.set_master_fx("A", MasterFxKey::LpFreq, 40);
    st.step(8);
    let d = st.e.deck("A").unwrap();
    assert_eq!(
        d.master_fx_set.get(&(MasterFxKey::LpFreq.index() as u8)),
        Some(&40)
    );
    assert!(d.mirror.is_master_active(MasterMacro::SweepHp));
    // `deck.panic()`
    assert_eq!(
        perform(&mut st, DeckAction::Panic { deck: "A".into() }),
        Applied::Done
    );
    st.step(4);
    assert!(st
        .e
        .deck("A")
        .unwrap()
        .mirror
        .active_master_macros()
        .is_empty());
    // the same through `perform` (what the set test records), as a master macro by its id
    assert_eq!(
        perform(
            &mut st,
            DeckAction::Macro {
                deck: "A".into(),
                id: "sweep-lp".into()
            }
        ),
        Applied::Done
    );
    st.step(8);
    assert!(st
        .e
        .deck("A")
        .unwrap()
        .mirror
        .is_master_active(MasterMacro::SweepLp));
    assert!(st.e.unhandled.is_empty(), "{:?}", st.e.unhandled);
}

// (no Swift twin: `Deck.decode` resamples to the deck's rate, so Swift never holds audio at
// another one, and its turntable traps on it) audio at another rate than the deck's is converted
// to it on load, and `perform` says it did it
#[test]
fn audio_at_another_rate_is_loaded_converted() {
    use djrust::deck::turntable::SampleBuffer;
    use std::sync::Arc;
    let odd = Arc::new(SampleBuffer::new(vec![0.1; 4_410], None, 44_100.0));
    let mut st = Stepper::new(SR, true);
    let load = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Sample {
            id: "odd".into(),
            title: "odd".into(),
            grid: None,
        },
    };
    let ctx = PerformCtx {
        prepared: None,
        buffer: Some(odd.clone()),
    };
    assert_eq!(st.e.perform(&load, ctx), Applied::Done);
    let held =
        st.e.deck("A")
            .unwrap()
            .sample
            .as_ref()
            .unwrap()
            .buffer
            .clone();
    assert_eq!(held.sample_rate, SR);
    assert_eq!(held.length(), (4_410.0 * SR / 44_100.0).round() as usize);
    // the session and the library name what came in
    assert!(Arc::ptr_eq(
        &st.e.loops.row.sample_library[&odd.content_id()],
        &held
    ));
}

// SurfaceNetTests.mpkKeysPlayTheLiveVoiceInScale
#[test]
fn mpk_keys_play_the_live_voice_in_scale() {
    let (mut rig, mut audio) = rig_with_audio(None);
    assert!(rig.attach(MPK));
    rig.engine.play("A");
    for r in ["kick", "snare", "hihat", "bass", "melody", "harmony"] {
        rig.engine.set_role("A", r, false);
    }
    audio.render(8192);
    let quiet = audio.render(4096);
    // 61 is outside the scale of this project's key; the controller snaps it.
    press(&mut rig, MPK, &[0x90, 61, 110]);
    let loud = audio.render(8192);
    assert!(
        loud > quiet + 0.02,
        "the held key is audible ({loud} vs {quiet})"
    );
    let want = rig.engine.scale_locked("A", 61);
    assert_ne!(want, 61, "the scale moved it");
    assert_eq!(
        rig.engine.deck("A").unwrap().held_keys.get(&61),
        Some(&want)
    );
    press(&mut rig, MPK, &[0x80, 61, 0]);
    assert_eq!(rig.engine.deck("A").unwrap().held_keys.get(&61), None);
}

/// A saved set of actions applied one after another through `perform` keeps the order
/// (`DJEngine.perform` records after it applies).
#[test]
fn perform_records_in_the_order_it_applies() {
    let mut st = Stepper::new(SR, true);
    st.e.start_session().unwrap();
    perform(&mut st, DeckAction::Crossfader(0.3));
    st.step(3);
    perform(&mut st, DeckAction::Master(0.6));
    perform(
        &mut st,
        DeckAction::Focus {
            deck: "nobody".into(),
        },
    );
    let s = st.e.finish_session().unwrap();
    assert_eq!(
        s.events.len(),
        3,
        "an action that did nothing is recorded all the same"
    );
    assert_eq!(
        s.events.iter().map(|e| e.frame).collect::<Vec<_>>(),
        [0, 384, 384]
    );
    assert_eq!(
        st.e.owners().get("crossfader").map(String::as_str),
        Some("ui")
    );
    assert_eq!(st.e.focus, "A", "an unknown deck is not focused");
}

// a DeckView for the loop channel reads its own FX unit, not a deck's
#[test]
fn the_loop_channel_is_a_surface_target() {
    let mut st = Stepper::new(SR, true);
    let l = SurfaceHost::deck(&st.e, "L").expect("the loop channel");
    assert!(l.has_loop_bus() && !l.fx_killed() && l.fx_neutral());
    perform(
        &mut st,
        DeckAction::FxKill {
            deck: "L".into(),
            on: true,
            on_beat: false,
        },
    );
    assert!(SurfaceHost::deck(&st.e, "L").unwrap().fx_killed());
    assert!(SurfaceHost::deck(&st.e, "Z").is_none());
}

// ---- the sound pokes (PaletteTests.PaletteDeckTests, the ones that need no Wander) ----

/// `sourceNames(source, field, value)`: the `tracks` override of the source names `value` in
/// `field` on some channel.
fn source_names(src: Option<DeckSource>, field: &str, value: &str) -> bool {
    let tracks = match src {
        Some(DeckSource::Net(s)) => s.layers.get("tracks").cloned(),
        Some(DeckSource::RawNets(p)) => p.get("tracks").cloned(),
        _ => None,
    };
    tracks
        .as_ref()
        .and_then(JsonValue::as_object)
        .is_some_and(|o| {
            o.iter()
                .any(|(_, e)| e.get(field).and_then(JsonValue::as_str) == Some(value))
        })
}

fn palette_deck(palette: &str) -> Stepper {
    let mut st = Stepper::new(24_000.0, true);
    let s = st.e.with_sounds(|ctx| {
        spec("house", 11.0, Some("standard")).with_palette(Some(palette), ctx.installed)
    });
    perform(&mut st, load("A", &s));
    st
}

fn sound_net<'a>(e: &'a djrust::host::deck::Engine, role: &str) -> &'a djrust::pflow::NetBundle {
    djrust::sound::project_sound_net(e.deck("A").unwrap().project.as_ref().unwrap(), role)
        .unwrap_or_else(|| panic!("no {role} net"))
}

// PaletteDeckTests.anInstrumentPickIsAnOverrideToo (without the panel rows: `deck_role_sounds.rs`)
#[test]
fn an_instrument_pick_is_an_override_too() {
    let mut st = palette_deck("synthwave");
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: Some("inst:acid".into()),
        },
    );
    let bass = sound_net(&st.e, "bass");
    assert_eq!(bass.track.instrument, "acid");
    assert!(bass.track.picked);
    let next =
        st.e.deck("A")
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .next_version()
            .project();
    assert_eq!(
        djrust::sound::project_sound_net(&next, "bass")
            .unwrap()
            .track
            .instrument,
        "acid"
    );
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: None,
        },
    );
    let b = sound_net(&st.e, "bass");
    assert!(!b.track.picked && b.track.sound.override_.is_none());
}

// PaletteDeckTests.setPaletteKeepsOverridesAndTheDefaultClearsIt
#[test]
fn set_palette_keeps_overrides_and_the_default_clears_it() {
    let mut st = palette_deck("lofi");
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: Some("gm:32".into()),
        },
    );
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("club".into()),
        },
    );
    let id = st.e.palette("A");
    assert_eq!(
        djrust::sound::PaletteSet::shared()
            .playing(id.as_deref())
            .map(|p| p.id.as_str()),
        Some("club")
    );
    assert_eq!(
        sound_net(&st.e, "bass").track.sound.option.as_deref(),
        Some("gm:32")
    );
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("studio".into()),
        },
    );
    assert_eq!(st.e.palette("A"), None);
    assert_eq!(
        st.e.deck("A").unwrap().spec.as_ref().unwrap().palette(),
        None
    );
    assert!(
        source_names(st.e.deck("A").unwrap().net_source(), "voice", "gm:32"),
        "the override stays"
    );
}

// PaletteDeckTests.theDrumsFollowThePaletteUntilAKitIsPicked (the part that reads the source)
#[test]
fn a_picked_kit_goes_to_the_next_version_and_reset_clears_it() {
    let mut st = palette_deck("club");
    let kit = "MuseScore_General.sf2#128:32";
    perform(
        &mut st,
        DeckAction::SetKit {
            deck: "A".into(),
            kit: Some(kit.into()),
        },
    );
    assert_eq!(st.e.kit("A").as_deref(), Some(kit));
    let next =
        st.e.deck("A")
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .next_version()
            .project();
    assert_eq!(next.drum_kit().as_deref(), Some(kit));
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "kit".into(),
            voice: None,
        },
    );
    assert_eq!(st.e.kit("A"), None);
}

// PaletteDeckTests.aReplayPlaysTheSameSound: the pokes are recorded, the picks derived
#[test]
fn a_replay_of_the_sound_pokes_lands_on_the_same_sound() {
    let mut st = palette_deck("ambient");
    st.e.start_session().unwrap();
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("orchestral".into()),
        },
    );
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "harmony".into(),
            voice: Some("gm:48".into()),
        },
    );
    let session = st.e.finish_session().unwrap();
    assert_eq!(session.events.len(), 2);
    let mut st2 = palette_deck("ambient");
    for e in &session.events {
        if let SessionEventKind::Action(a) = &e.kind {
            st2.e.perform(a, PerformCtx::default());
        }
    }
    assert_eq!(
        st2.e.deck("A").unwrap().net_source(),
        st.e.deck("A").unwrap().net_source()
    );
    assert_eq!(
        sound_net(&st2.e, "harmony").track.sound.option,
        sound_net(&st.e, "harmony").track.sound.option
    );
}

// JournalTests.everyInputPathLandsInTheJournal: the engine's hooks (the file is the journal's)
#[derive(Default, Clone)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<(i64, djrust::journal::event::JournalEvent)>>>);

impl djrust::host::deck::JournalSink for Sink {
    fn record(&mut self, frame: i64, event: djrust::journal::event::JournalEvent) {
        self.0.lock().unwrap().push((frame, event));
    }
}

#[test]
fn every_input_path_lands_in_the_journal_and_the_session() {
    use djrust::journal::event::JournalEvent as J;
    let (mut e, _r) = engine(EngineConfig::new(48_000.0));
    e.load_demo("A");
    let sink = Sink::default();
    e.journal = Some(Box::new(sink.clone()));
    e.start_session().unwrap();
    let mut rig = Rig::new(e);
    assert!(rig.attach(INPULSE));
    // a UI action, then the hardware: the router records what arrived, and the action that a
    // MIDI message makes through `perform` is not recorded twice
    rig.engine
        .perform(&DeckAction::Crossfader(0.25), PerformCtx::default());
    rig.route(INPULSE, &[0x91, 0x07, 0x7F], 1.0); // deck A PLAY
    let events = sink.0.lock().unwrap().clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].1, J::Action(DeckAction::Crossfader(0.25)));
    assert_eq!(
        events[1].1,
        J::Midi {
            port: INPULSE.into(),
            bytes: vec![0x91, 0x07, 0x7F]
        }
    );
    let s = rig.engine.finish_session().unwrap();
    assert_eq!(s.events.len(), 2);
    assert!(matches!(s.events[1].kind, SessionEventKind::Midi { .. }));
    assert!(
        rig.engine.deck("A").unwrap().playing,
        "and the button played the deck"
    );
}

// SurfaceNetTests.apcFadersSetTheAimedDecksFxUnit, on djrust's APC layout (docs/HUB.md M4,
// diverging from Swift): the faders are the mixer, SHIFT + fader 1-8 the aimed target's FX unit
#[test]
fn apc_faders_set_the_aimed_decks_fx_unit() {
    let (mut rig, _audio) = rig_with_audio(None);
    assert!(rig.attach(APC));
    press(&mut rig, APC, &[0xB0, 0x30, 127]); // fader 1 without SHIFT: A's volume
    assert_eq!(rig.engine.deck("A").unwrap().volume, 1.0);
    assert!(rig.engine.fx_unit("A").unwrap().is_neutral(), "no FX unit without SHIFT");
    press(&mut rig, APC, &[0x90, 0x7A, 0x7F]);
    for (cc, v) in [
        (0x30u8, 0u8), // fader 1: filter hard down (LP)
        (0x31, 127),   // fader 2: HP all the way
        (0x32, 0),     // fader 3: LP closed
        (0x33, 127),   // fader 4: phaser mix
        (0x34, 127),   // fader 5: crush
        (0x35, 64),    // fader 6: drive
        (0x36, 127),   // fader 7: pitch +12
        (0x37, 127),   // fader 8: room
    ] {
        press(&mut rig, APC, &[0xB0, cc, v]);
    }
    press(&mut rig, APC, &[0x80, 0x7A, 0x00]);
    let u = rig.engine.fx_unit("A").unwrap().clone();
    assert!(u.get(FxParam::Filter) == -1.0 && u.get(FxParam::Hp) == 1.0);
    assert!(u.get(FxParam::Lp) == 0.0 && u.get(FxParam::PhaserMix) == 1.0);
    assert!(u.get(FxParam::Crush) == 1.0 && (u.get(FxParam::Drive) - 64.0 / 127.0).abs() < 1e-12);
    assert!(u.get(FxParam::Pitch) == 12.0 && u.get(FxParam::Room) == 1.0);
    assert!(rig.engine.fx_unit("B").unwrap().is_neutral(), "B untouched");
    assert_eq!(rig.engine.deck("A").unwrap().volume, 1.0, "the mixer kept its place");
    // aimed at B: SHIFT + fader moves B's
    press(&mut rig, APC, &[0x90, 0x74, 0x7F]);
    press(&mut rig, APC, &[0x90, 0x7A, 0x7F]);
    press(&mut rig, APC, &[0xB0, 0x34, 64]);
    press(&mut rig, APC, &[0x80, 0x7A, 0x00]);
    let (a, b) = (
        rig.engine.fx_unit("A").unwrap(),
        rig.engine.fx_unit("B").unwrap(),
    );
    assert!((b.get(FxParam::Crush) - 64.0 / 127.0).abs() < 1e-12 && a.get(FxParam::Crush) == 1.0);
    // the master fader is the master, with or without SHIFT
    press(&mut rig, APC, &[0xB0, 0x38, 64]);
    assert!((rig.engine.master - 64.0 / 127.0).abs() < 1e-6);
}

// SessionTests.audioLoadedMidSetIsNamedByContentAndReplaysWithItsLibrary: audio handed to
// `perform` is recorded by its content id, kept in the library, and a replay finds it again
// through the resolver
#[test]
fn audio_loaded_by_perform_is_named_by_content_and_replays_with_its_library() {
    use djrust::deck::turntable::SampleBuffer;
    use std::sync::Arc;
    let tone: Vec<f32> = (0..22_050 * 2)
        .map(|i| (i as f32 * 0.05).sin() * 0.4)
        .collect();
    let buffer = Arc::new(SampleBuffer::new(tone, None, SR));
    let id = buffer.content_id();
    let mut st = Stepper::new(SR, true);
    st.e.start_session().unwrap();
    let load = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Sample {
            id: "whatever".into(),
            title: "tone".into(),
            grid: None,
        },
    };
    let ctx = PerformCtx {
        prepared: None,
        buffer: Some(buffer.clone()),
    };
    assert_eq!(st.e.perform(&load, ctx), Applied::Done);
    assert!(st.e.loops.row.sample_library.contains_key(&id));
    let d = st.e.deck("A").unwrap();
    assert_eq!(d.sample.as_ref().unwrap().title, "tone");
    let s = st.e.finish_session().unwrap();
    // a deterministic engine measures no tempo: the record names the audio and nothing else
    assert_eq!(
        s.events[0].kind,
        SessionEventKind::Action(DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Sample {
                id: id.clone(),
                title: "tone".into(),
                grid: None
            }
        })
    );
    // a replay engine: the resolver finds the audio by its id
    let mut replay = Stepper::new(SR, true);
    let lib = buffer.clone();
    let want = id.clone();
    replay.e.sample_resolver = Some(Box::new(move |i| (i == want).then(|| lib.clone())));
    let SessionEventKind::Action(a) = &s.events[0].kind else {
        unreachable!()
    };
    assert_eq!(replay.e.perform(a, PerformCtx::default()), Applied::Done);
    assert_eq!(
        replay.e.deck("A").unwrap().sample.as_ref().unwrap().title,
        "tone"
    );
    // an id nothing resolves loads nothing
    let mut none = Stepper::new(SR, true);
    assert!(matches!(
        none.e.perform(a, PerformCtx::default()),
        Applied::Ignored(_)
    ));
    assert!(none.e.deck("A").unwrap().sample.is_none());
    // a recorded grid stands in for the analysis, and is recorded back
    let with_grid = DeckAction::Load {
        deck: "B".into(),
        source: DeckSource::Sample {
            id: id.clone(),
            title: "tone".into(),
            grid: Some(djrust::deck::beat_grid::BeatGrid::new(100.0, 0.25, 1.0)),
        },
    };
    replay.e.start_session().unwrap();
    replay.e.perform(&with_grid, PerformCtx::default());
    let g = replay
        .e
        .deck("B")
        .unwrap()
        .sample
        .as_ref()
        .unwrap()
        .grid
        .unwrap();
    assert_eq!((g.bpm, g.offset), (100.0, 0.25));
    let s2 = replay.e.finish_session().unwrap();
    assert_eq!(s2.events[0].kind, SessionEventKind::Action(with_grid));
}
