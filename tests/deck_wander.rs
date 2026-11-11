//! Wander on a live deck (`Wander.swift`, the `Deck` and `DJEngine` extensions): the walk's
//! controls, `advanceWander`, `takeWanderMove`, the look-ahead, and the moves on a deck (a role,
//! STEMS, a macro, a stinger, a palette hop with its pins, a layer list, a swap).
//!
//! Parity form (a): each test names the Swift test whose literals it asserts (`WanderDeckTests`,
//! `WanderSwapTimingTests`, `WanderKeepTests`, `WanderLayerDeckTests`, `WanderRawNetsTests`,
//! `PaletteDeckTests`, `StingerCurationTests`). `wanderdeck_golden.rs` holds the same calls to
//! the Swift engine's output (parity form (b)).
//!
//! Not ported: `WanderReplayTests` (a recorded set replayed by `SessionPlayer`: the replay is the
//! replay slice's; the recording half is `deck_actions.rs`'s). Where a Swift test goes through
//! `dj.perform(.wander...)` this calls the deck method the action ends in, and where it goes
//! through the controller (`SurfaceController.handle`) it routes the Inpulse bytes through the
//! same router `Rig` has.
//!
//! The Swift engine is not `deterministic` unless a test says so; the engine here is, by default
//! (see `Engine::deterministic`), so the tests that rely on a swap composing in the background
//! (the look-ahead) turn it off, and the ones that only read the walk leave it on.

use djrust::deck::slots::SwapStart;
use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::layers::{LayerEntry, LayerLoop};
use djrust::sound::stinger::StingerContent;
use djrust::sound::{Installed, PaletteSet};
use djrust::wander::deck::{project_kit, project_palette, resolved_project_stingers};
use djrust::wander::{Wander, WanderAction, WanderLock, WanderMoveKind};
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, InstalledSounds, PreparedTrack, Renderer, Rig};

const RATE: f64 = 22_050.0;
const BLOCK: usize = 128;
const INPULSE: &str = "DJControl Inpulse 200 Mk3";

/// `OfflineConductor`: blocks of 128 frames through a [`Rig`] (the router, the jogs, the decks'
/// service and the walk's, in `Rig::tick`'s order), the housekeeping tick every `sr / 100`
/// frames in front of the block that finds it due.
struct Conductor {
    rig: Rig,
    r: Renderer,
    frame: i64,
    next: i64,
    every: i64,
    l: Vec<f32>,
    rr: Vec<f32>,
}

impl Conductor {
    fn new() -> Conductor {
        let (mut e, r) = engine(EngineConfig::new(RATE));
        // the machine the Swift tests ran on had no palettes' banks here either
        e.installed = Some(InstalledSounds::new(Installed {
            dir: "/dj-soundfonts-none".into(),
            gm_bank: true,
            minecraft_assets_missing: Some("no game here".into()),
            ..Installed::default()
        }));
        Conductor {
            rig: Rig::new(e),
            r,
            frame: 0,
            next: 0,
            every: ((RATE / 100.0) as i64).max(1),
            l: vec![0.0; BLOCK],
            rr: vec![0.0; BLOCK],
        }
    }

    /// `make(spec)`: deck A holds the song, B the demo loop.
    fn with(spec: &TrackSpec) -> Conductor {
        let mut c = Conductor::new();
        assert!(c.rig.engine.generate("A", spec));
        assert!(c.rig.engine.load_demo("B"));
        c
    }

    fn e(&mut self) -> &mut djrust::host::deck::Engine {
        &mut self.rig.engine
    }

    fn block(&mut self) {
        if self.frame >= self.next {
            self.next = self.frame + self.every;
            self.rig.tick(self.frame as f64 / RATE);
        }
        self.r.render_block(&mut self.l, &mut self.rr);
        self.frame += BLOCK as i64;
    }

    fn tick(&self, deck: &str) -> i64 {
        self.rig.engine.deck(deck).unwrap().mirror.tick()
    }

    fn until(&mut self, max: usize, mut done: impl FnMut(&mut Conductor) -> bool) -> bool {
        for _ in 0..max {
            if done(self) {
                return true;
            }
            self.block();
        }
        done(self)
    }

    fn run_to_tick(&mut self, deck: &str, tick: i64) {
        let d = deck.to_string();
        assert!(
            self.until(40_000, |c| c.tick(&d) >= tick),
            "{deck} never reached tick {tick}"
        );
    }

    /// Render until the deck plays what it loaded last.
    fn until_installed(&mut self, deck: &str) -> bool {
        let d = deck.to_string();
        self.until(40_000, |c| {
            let dk = c.rig.engine.deck(&d).unwrap();
            dk.track.latest.as_ref().is_some_and(|l| {
                dk.mirror
                    .generation
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == l.generation
            })
        })
    }

    fn route(&mut self, bytes: &[u8]) {
        let now = self.frame as f64 / RATE;
        self.rig.route(INPULSE, bytes, now);
    }
}

fn techno() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("standard"))
}

fn log_bars(c: &mut Conductor) -> Vec<i64> {
    c.e()
        .deck("A")
        .unwrap()
        .walk
        .wander
        .log
        .iter()
        .map(|m| m.bar)
        .collect()
}

fn keep_everything(c: &mut Conductor) {
    for r in c.e().roles("A") {
        c.e().set_wander_lock("A", &WanderLock::keep(&r), true);
    }
}

fn advance(c: &mut Conductor, from: i64, to: i64) {
    for bar in from..to {
        c.e().advance_wander("A", bar);
    }
}

fn wander(c: &mut Conductor, deck: &str) -> djrust::wander::WanderState {
    c.e().deck(deck).unwrap().walk.wander.clone()
}

// MARK: WanderDeckTests

// WanderDeckTests.movesLandOnPaceBoundaries
#[test]
fn moves_land_on_pace_boundaries() {
    let mut c = Conductor::with(&techno());
    c.e().set_wander_pace("A", 4);
    // every part kept: the walk only plays macros and stingers (no recompose: a quick test)
    keep_everything(&mut c);
    c.e().set_wander("A", true);
    advance(&mut c, 0, 13);
    assert_eq!(
        log_bars(&mut c),
        [4, 8, 12],
        "the last bar of each 4-bar phrase"
    );
    assert_eq!(wander(&mut c, "A").step, 3);
    // the same bar again (polled 100x/s) takes nothing
    c.e().advance_wander("A", 12);
    assert_eq!(wander(&mut c, "A").step, 3);
    // pace 8: the next move is in bar 16 (the end of the phrase 8...15)
    c.e().set_wander_pace("A", 8);
    advance(&mut c, 13, 16);
    assert_eq!(log_bars(&mut c), [4, 8, 12, 16]);
}

// WanderDeckTests.aTempoLockSetBeforeAnyTrackHoldsTheFirstOneWalkedFrom
#[test]
fn a_tempo_lock_set_before_any_track_holds_the_first_one_walked_from() {
    let mut c = Conductor::new();
    // as the app restores it at launch: nothing loaded yet
    c.e().set_wander_lock("A", "tempo", true);
    c.e().set_wander_lock("A", "key", true);
    let w = wander(&mut c, "A");
    assert!(w.anchor_bpm.is_none() && w.key.is_none());
    let spec = TrackSpec::new("house", 5.0, Some("minimal"));
    assert!(c.e().generate("A", &spec));
    keep_everything(&mut c); // no recompose
    c.e().set_wander_pace("A", 4);
    c.e().set_wander("A", true);
    advance(&mut c, 1, 4);
    let w = wander(&mut c, "A");
    assert!(w.anchor_bpm == Some(124.0) && w.key.as_ref().unwrap().scale == "MinorScale");
    assert_eq!(w.log.len(), 1);
}

// WanderDeckTests.offOrOnASampleDeckNothingMoves
#[test]
fn off_or_on_a_sample_deck_nothing_moves() {
    let mut c = Conductor::with(&techno());
    c.e().set_wander_pace("A", 4);
    advance(&mut c, 0, 9);
    assert!(wander(&mut c, "A").log.is_empty(), "off");
    c.e().set_wander("B", true);
    for bar in 0..40 {
        c.e().advance_wander("B", bar);
    }
    assert!(wander(&mut c, "B").log.is_empty());
    assert!(c
        .e()
        .wander_idle_reason("B")
        .is_some_and(|r| r.contains("sample")));
    // a sample deck has no countdown either
    assert_eq!(c.e().wander_countdown("B"), None);
}

// WanderDeckTests.theWheelGrabsTheWalkAndAQuietPhraseGivesItBack
#[test]
fn the_wheel_grabs_the_walk_and_a_quiet_phrase_gives_it_back() {
    let mut c = Conductor::with(&techno());
    assert!(c.rig.attach(INPULSE));
    c.e().set_wander_pace("A", 4);
    c.e().set_wander("A", true);
    c.e().set_playing("A", true);
    c.e().focus = "A".into();
    c.route(&[0xB0, 0x01, 0x01]); // browser wheel, one click on
    let (w, seed) = (
        wander(&mut c, "A"),
        c.e().deck("A").unwrap().spec.as_ref().unwrap().seed,
    );
    assert!(w.paused && seed == 43.0);
    // a phrase of four quiet bars: no move while paused, then it walks again
    advance(&mut c, 1, 5);
    let w = wander(&mut c, "A");
    assert!(!w.paused && w.log.is_empty());
    // a turn mid-phrase starts the count again
    c.route(&[0xB0, 0x01, 0x7F]);
    let (w, seed) = (
        wander(&mut c, "A"),
        c.e().deck("A").unwrap().spec.as_ref().unwrap().seed,
    );
    assert!(w.paused && seed == 42.0);
    advance(&mut c, 5, 8);
    assert!(
        wander(&mut c, "A").paused,
        "three quiet bars are not a phrase"
    );
    c.e().advance_wander("A", 8);
    assert!(!wander(&mut c, "A").paused);
    c.e().advance_wander("A", 11);
    assert_eq!(log_bars(&mut c), [12]);
}

// WanderDeckTests.shiftAndTheWheelSetThePaceAndThePushTogglesIt
#[test]
fn shift_and_the_wheel_set_the_pace_and_the_push_toggles_it() {
    let mut c = Conductor::with(&techno());
    assert!(c.rig.attach(INPULSE));
    c.e().focus = "A".into();
    assert_eq!(wander(&mut c, "A").pace, 16);
    c.route(&[0xB3, 0x01, 0x01]);
    assert_eq!(wander(&mut c, "A").pace, 32);
    c.route(&[0xB3, 0x01, 0x01]);
    assert_eq!(wander(&mut c, "A").pace, 32, "stops at 32");
    for _ in 0..4 {
        c.route(&[0xB3, 0x01, 0x7F]);
    }
    assert_eq!(wander(&mut c, "A").pace, 4, "and at 4");
    c.route(&[0x90, 0x00, 0x7F]); // browser push
    assert!(wander(&mut c, "A").on);
    c.route(&[0x90, 0x00, 0x00]); // its release does nothing
    assert!(wander(&mut c, "A").on);
    c.route(&[0x90, 0x00, 0x7F]);
    assert!(!wander(&mut c, "A").on);
    // locked (right-click > Disable on the Wander button): the push does nothing
    c.e().locked = vec!["A.wander".into()];
    c.route(&[0x90, 0x00, 0x7F]);
    assert!(!wander(&mut c, "A").on);
}

// WanderDeckTests.theWheelStepsASongAndTheTransportKeepsGoing
#[test]
fn the_wheel_steps_a_song_and_the_transport_keeps_going() {
    let mut c = Conductor::with(&techno());
    assert!(c.rig.attach(INPULSE));
    c.e().set_playing("A", true);
    c.e().focus = "A".into();
    c.run_to_tick("A", 40); // into the third bar
    c.route(&[0xB0, 0x01, 0x01]);
    assert_eq!(
        c.e().deck("A").unwrap().spec,
        Some(TrackSpec::new("techno", 43.0, Some("standard")))
    );
    let before = c.tick("A");
    assert!(c.until_installed("A"));
    // the new seed came in at the next bar, where the song had got to: not at its first bar
    let d = c.e().deck("A").unwrap();
    assert!(d.playing && d.mirror.is_playing());
    let t = c.tick("A");
    assert!(t >= 48 && t >= before, "tick {t}");
}

// WanderDeckTests.aGenreMoveChangesGenreAndKeepsPlaying
#[test]
fn a_genre_move_changes_genre_and_keeps_playing() {
    let mut c = Conductor::with(&TrackSpec::new("techno", 42.0, Some("minimal")));
    c.e().set_playing("A", true);
    c.run_to_tick("A", 3 * 16); // in bar 4: a 4-bar phrase's last
                                // the first step this spec draws a genre move at
    let ctx = c.e().wander_context("A").unwrap();
    let step = (0..400)
        .find(|&s| Wander::plan(&ctx, s).is_some_and(|p| p.kind == WanderMoveKind::Genre))
        .expect("a genre move");
    c.e().set_wander("A", true);
    c.e().deck_mut("A").unwrap().walk.wander.step = step;
    let mv = c.e().take_wander_move("A", 3).expect("a move");
    assert!(mv.kind == WanderMoveKind::Genre && mv.cid.is_some());
    let genre = mv.spec.as_ref().unwrap().genre.clone();
    assert!(genre != "techno" && c.e().deck("A").unwrap().spec.as_ref().unwrap().genre == genre);
    assert!(c.until_installed("A"));
    let d = c.e().deck("A").unwrap();
    assert!(d.playing && d.mirror.is_playing());
    assert!(c.tick("A") >= 64, "in at the phrase start");
    let d = c.e().deck("A").unwrap();
    assert_eq!(d.base_tempo, BeatsTables::shared().genres[&genre].bpm);
}

// WanderDeckTests.theDeckStateShowsTheWalk
#[test]
fn the_deck_state_shows_the_walk() {
    let mut c = Conductor::with(&techno());
    c.e().set_wander_pace("A", 4);
    c.e().set_wander("A", true);
    advance(&mut c, 0, 4);
    let j = c.e().wander_json("A").unwrap();
    assert_eq!(j.get("on"), Some(&JsonValue::Bool(true)));
    assert_eq!(j.get("pace"), Some(&JsonValue::Number(4.0)));
    assert_eq!(
        j.get("log").and_then(JsonValue::as_array).map(Vec::len),
        Some(1)
    );
    assert!(j.get("nextMoveInBars").is_some());
    assert_eq!(c.e().wander_idle_reason("A"), None);
    for dim in WanderLock::DIMENSIONS {
        c.e().set_wander_lock("A", dim, true);
    }
    keep_everything(&mut c);
    assert!(c
        .e()
        .wander_idle_reason("A")
        .is_some_and(|r| r.contains("locked")));
    // the JSON's keys, in the order dj_state shows them
    let j = c.e().wander_json("A").unwrap();
    let keys: Vec<&str> = j.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "on",
            "pace",
            "paused",
            "step",
            "locks",
            "log",
            "nextMoveInBars",
            "idle",
            "tempoAnchor"
        ]
    );
}

// The housekeeping drives the walk: a bar of the deck's clock at a time, only while it plays
// (`Deck.serviceWander`).
#[test]
fn the_housekeeping_walks_a_playing_deck_and_only_a_playing_one() {
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let mut c = Conductor::with(&spec);
    c.e().set_tempo("A", 240.0); // phrases go by
    c.e().set_wander_pace("A", 4);
    keep_everything(&mut c);
    c.e().set_wander("A", true);
    // not playing: the clock does not run, and the walk is not looked at
    for _ in 0..50 {
        c.block();
    }
    assert!(wander(&mut c, "A").log.is_empty());
    c.e().set_playing("A", true);
    assert!(
        c.until(60_000, |c| c.e().deck("A").unwrap().walk.wander.log.len()
            >= 2)
    );
    // each move was taken on the last bar of a 4-bar phrase
    assert!(
        log_bars(&mut c).iter().all(|b| b % 4 == 0),
        "{:?}",
        log_bars(&mut c)
    );
    // paused again: the transport stops, the walk with it
    c.e().set_playing("A", false);
    for _ in 0..50 {
        c.block();
    }
    let n = wander(&mut c, "A").log.len();
    for _ in 0..4000 {
        c.block();
    }
    assert_eq!(wander(&mut c, "A").log.len(), n);
}

// MARK: WanderSwapTimingTests: the look-ahead

/// `WanderSwapTimingTests.playing(toTick:)`: deck A playing the song in an engine that is not
/// `deterministic` (composes in the background), run to `tick`.
fn playing_to(tick: i64) -> Conductor {
    let mut c = Conductor::with(&techno());
    c.e().deterministic = false;
    c.e().set_playing("A", true);
    c.run_to_tick("A", tick);
    c
}

/// `wanderingToASwap`: Wander on at pace 4 with its step on the first one whose plan is a
/// swap; the clock not yet read, so `advance_wander` takes every bar it is given.
fn wandering_to_a_swap(c: &mut Conductor) -> TrackSpec {
    c.e().set_wander("A", true);
    c.e().set_wander_pace("A", 4);
    c.e().deck_mut("A").unwrap().walk.wander.last_bar = None;
    let ctx = c.e().wander_context("A").unwrap();
    for step in 0..200 {
        if let Some(WanderAction::Swap(spec)) = Wander::plan(&ctx, step).map(|p| p.action) {
            c.e().deck_mut("A").unwrap().walk.wander.step = step;
            return spec;
        }
    }
    panic!("no swap in 200 steps");
}

// WanderSwapTimingTests.aPlannedSwapIsComposedBeforeItsPhraseEnds
#[test]
fn a_planned_swap_is_composed_before_its_phrase_ends() {
    let mut c = playing_to(40); // bar 2: the bar before bar 3, the last
    let planned = wandering_to_a_swap(&mut c);
    let step = wander(&mut c, "A").step;
    c.e().advance_wander("A", 2);
    let key = c
        .e()
        .look_ahead_key("A")
        .expect("a swap is planned: it is composed ahead");
    assert!(key.spec == planned && key.step == step && key.start == SwapStart::Tick(64));
    let w = wander(&mut c, "A");
    assert!(
        w.step == step && w.log.is_empty(),
        "the look-ahead moves nothing"
    );
    c.e().wait_look_ahead("A");
    assert!(c.e().look_ahead_ready("A"));
    assert!(c.tick("A") < 48, "composed before the phrase's last bar");
    let (ahead_generation, ahead_start) = {
        let t = c.e().look_ahead_track("A").expect("the composed track");
        (t.prepared.wave().generation, t.prepared.wave().start_tick)
    };
    assert_eq!(ahead_start, 64);

    c.e().advance_wander("A", 3); // the move
    let w = wander(&mut c, "A");
    assert_eq!(w.step, step + 1);
    assert_eq!(
        w.log.last().and_then(|m| m.spec.clone()),
        Some(planned.clone())
    );
    assert!(c.e().look_ahead_key("A").is_none());
    let d = c.e().deck("A").unwrap();
    assert_eq!(
        d.track.latest.as_ref().map(|l| l.generation),
        Some(ahead_generation),
        "the look-ahead is what loads: nothing composed"
    );
    assert_eq!(d.spec, Some(planned));
    assert!(c.until_installed("A"));
    let t = c.tick("A");
    assert!((64..80).contains(&t), "on time: tick {t}");
}

// WanderSwapTimingTests.aPokeBetweenDropsTheLookAhead
#[test]
fn a_poke_between_drops_the_look_ahead() {
    let mut c = playing_to(40);
    wandering_to_a_swap(&mut c);
    c.e().advance_wander("A", 2);
    c.e().wait_look_ahead("A");
    let (ahead_spec, ahead_generation) = {
        let t = c.e().look_ahead_track("A").expect("composed");
        (t.spec().cloned(), t.prepared.wave().generation)
    };
    // the poke: another song loaded on the deck, so the move plans from it
    let other = TrackSpec::new("house", 7.0, Some("standard"));
    let pr = c.e().preparer("A").unwrap();
    assert!(c.e().load_net("A", PreparedTrack::compose(&other, &pr, 0)));
    let replanned = c.e().wander_swap_key("A", 3);
    assert_ne!(replanned.as_ref().map(|k| k.spec.clone()), ahead_spec);
    let before = c
        .e()
        .deck("A")
        .unwrap()
        .track
        .latest
        .as_ref()
        .map(|l| l.generation);
    c.e().advance_wander("A", 3);
    assert!(c.e().look_ahead_key("A").is_none(), "dropped");
    let now = c
        .e()
        .deck("A")
        .unwrap()
        .track
        .latest
        .as_ref()
        .map(|l| l.generation);
    assert_ne!(now, Some(ahead_generation));
    if let Some(k) = replanned {
        assert_eq!(
            c.e().deck("A").unwrap().track.seed_target,
            Some(k.spec),
            "composed afresh"
        );
        assert_eq!(now, before, "not loaded until that compose is done");
    }
    c.e().wait_composes("A");
}

// WanderSwapTimingTests.aLockBetweenDropsTheLookAhead
#[test]
fn a_lock_between_drops_the_look_ahead() {
    let mut c = playing_to(40);
    wandering_to_a_swap(&mut c);
    c.e().advance_wander("A", 2);
    assert!(c.e().look_ahead_key("A").is_some());
    c.e().set_wander_lock("A", "genre", true);
    assert!(c.e().look_ahead_key("A").is_none());
    c.e().wait_composes("A");
}

// The browser wheel and switching the walk off drop it too (`grabWander`, `setWander(false)`).
#[test]
fn a_grab_and_wander_off_drop_the_look_ahead() {
    let mut c = playing_to(40);
    wandering_to_a_swap(&mut c);
    c.e().advance_wander("A", 2);
    assert!(c.e().look_ahead_key("A").is_some());
    c.e().grab_wander("A");
    assert!(c.e().look_ahead_key("A").is_none());
    assert!(wander(&mut c, "A").paused);
    c.e().advance_wander("A", 3);
    assert!(wander(&mut c, "A").log.is_empty(), "paused: no move");
    // off
    c.e().deck_mut("A").unwrap().walk.wander.paused = false;
    c.e().advance_wander("A", 6); // the bar before a move at 7: a look-ahead again
    c.e().set_wander("A", false);
    assert!(c.e().look_ahead_key("A").is_none());
    c.e().wait_composes("A");
}

// WanderSwapTimingTests.deterministicReplayDoesNotLookAhead
#[test]
fn deterministic_replay_does_not_look_ahead() {
    let mut c = playing_to(40);
    c.e().deterministic = true;
    wandering_to_a_swap(&mut c);
    c.e().advance_wander("A", 2);
    assert!(c.e().look_ahead_key("A").is_none());
    assert!(wander(&mut c, "A").log.is_empty());
}

// Deck.advanceWander: auto-mix holds the walk through a hand-off: no move, no look-ahead, and the
// held bars are not quiet bars (`Deck.wanderHeld`)
#[test]
fn a_held_walk_takes_no_move_makes_no_look_ahead_and_counts_no_quiet_bar() {
    let mut c = playing_to(40);
    wandering_to_a_swap(&mut c);
    let step = wander(&mut c, "A").step;
    c.e().set_wander_held("A", true);
    c.e().advance_wander("A", 2); // a look-ahead bar
    assert!(c.e().look_ahead_key("A").is_none());
    c.e().advance_wander("A", 3); // a move bar
    let w = wander(&mut c, "A");
    assert!(w.log.is_empty() && w.step == step);
    // grabbed and held: the held bars do not count toward the quiet phrase
    c.e().grab_wander("A");
    advance(&mut c, 4, 12);
    let w = wander(&mut c, "A");
    assert!(w.paused && w.quiet_bars == 0, "{w:?}");
    // released: the quiet bars count again
    c.e().set_wander_held("A", false);
    advance(&mut c, 12, 15);
    assert_eq!(wander(&mut c, "A").quiet_bars, 3);
    c.e().advance_wander("A", 15);
    assert!(!wander(&mut c, "A").paused, "a quiet phrase of four bars");
    c.e().wait_composes("A");
}

// MARK: WanderKeepTests

// WanderKeepTests.aDeckCarriesTheKeptPartSeedIntoItsSwaps
#[test]
fn a_deck_carries_the_kept_part_seed_into_its_swaps() {
    let mut c = Conductor::with(&TrackSpec::new("techno", 42.0, Some("minimal")));
    c.e().set_wander_lock("A", "genre", true);
    c.e().set_wander_lock("A", &WanderLock::keep("bass"), true);
    c.e().set_wander("A", true);
    for bar in [3, 7] {
        let ctx = c.e().wander_context("A").unwrap();
        let from = wander(&mut c, "A").step;
        let step = (from..from + 400)
            .find(|&s| Wander::plan(&ctx, s).is_some_and(|p| p.kind == WanderMoveKind::Seed))
            .expect("a seed move");
        c.e().deck_mut("A").unwrap().walk.wander.step = step;
        let mv = c.e().take_wander_move("A", bar).expect("a move");
        assert_eq!(mv.kind, WanderMoveKind::Seed);
        let want: std::collections::BTreeMap<String, f64> = [("bass".to_string(), 42.0)].into();
        assert_eq!(mv.spec.as_ref().unwrap().part_seeds, want);
        let spec = c.e().deck("A").unwrap().spec.clone().unwrap();
        assert_eq!(spec.part_seeds, want);
        assert!(spec.seed != 42.0);
    }
}

// MARK: WanderRawNetsTests

const ROLL: &str = r#"{"name":"Wander Roll","tempo":128,"steps":128,"parts":{
  "kick":{"every":16,"until":128,"notes":[[0],[4],[8],[12]]},
  "snare":{"every":16,"until":128,"notes":[[4],[12]]},
  "hihat":{"every":16,"until":128,"notes":[[2,46],[6,46],[10,46],[14,46]]},
  "bass":{"every":16,"until":128,"notes":[[2,45,2],[6,45,2],[10,45,2],[14,45,2]]},
  "melody":{"every":64,"until":128,"notes":[[0,72,4],[8,76,4],[16,79,8],[32,74,8]]},
  "harmony":{"every":32,"until":128,"notes":[[0,57,16],[0,60,16],[0,64,16]]}}}"#;

fn raw_deck(roll: &str) -> Conductor {
    let mut c = Conductor::new();
    let env = djrust::roll::envelope(&JsonValue::parse(roll).unwrap()).unwrap();
    let pr = c.e().preparer("A").unwrap();
    let track = PreparedTrack::from_raw_nets(&env, &pr).expect("nets");
    assert!(c.e().load_net("A", track));
    assert!(c.e().load_demo("B"));
    c
}

/// Walk `bars` bars at pace 4: one move per phrase, on its last bar.
fn walk(c: &mut Conductor, bars: i64) {
    c.e().set_wander("A", true);
    c.e().set_wander_pace("A", 4);
    advance(c, 0, bars);
}

// WanderRawNetsTests.aRawDeckHasAWalkToTake
#[test]
fn a_raw_deck_has_a_walk_to_take() {
    let mut c = raw_deck(ROLL);
    let d = c.e().deck("A").unwrap();
    assert!(d.spec.is_none() && d.track.raw_nets.is_some());
    c.e().set_wander("A", true);
    assert_eq!(c.e().wander_idle_reason("A"), None);
    assert!(c.e().wander_countdown("A").is_some());
}

// WanderRawNetsTests.itTakesOnlyTheMovesThatNeedNoSeedAndRecordsNoSpec
#[test]
fn it_takes_only_the_moves_that_need_no_seed_and_records_no_spec() {
    let mut c = raw_deck(ROLL);
    walk(&mut c, 80); // 20 phrases
    let w = wander(&mut c, "A");
    assert!(w.step == 20 && w.log.len() == 20);
    for m in &w.log {
        assert!(
            Wander::RAW_KINDS.contains(&m.kind),
            "{:?}: {}",
            m.kind,
            m.detail
        );
        assert!(m.spec.is_none() && m.cid.is_none());
    }
    // the mix did something: parts came and went, macros and FX fired
    let kinds: std::collections::BTreeSet<_> = w.log.iter().map(|m| m.kind.raw()).collect();
    assert!(kinds.contains("role") && kinds.len() >= 3, "{kinds:?}");
}

// WanderRawNetsTests.theSamePayloadWalksTheSamePathAndAnotherDoesNot
#[test]
fn the_same_payload_walks_the_same_path_and_another_does_not() {
    let mut a = raw_deck(ROLL);
    let mut b = raw_deck(ROLL);
    let mut other = raw_deck(&ROLL.replace("\"tempo\":128", "\"tempo\":130"));
    walk(&mut a, 80);
    walk(&mut b, 80);
    walk(&mut other, 80);
    let details = |c: &mut Conductor| -> Vec<String> {
        wander(c, "A")
            .log
            .iter()
            .map(|m| m.detail.clone())
            .collect()
    };
    assert_eq!(details(&mut a), details(&mut b));
    assert_ne!(details(&mut a), details(&mut other));
}

// WanderRawNetsTests.locksStillHoldAndEveryMoveLockedSaysSo
#[test]
fn locks_still_hold_and_every_move_locked_says_so() {
    let mut c = raw_deck(ROLL);
    c.e().set_wander("A", true);
    for lock in ["arrangement", "fx", "palette"] {
        c.e().set_wander_lock("A", lock, true);
    }
    assert_eq!(
        c.e().wander_idle_reason("A"),
        Some("every move is locked: nothing left to change")
    );
    // arrangement and palette stay locked, fx freed: only macros, FX and stingers remain
    c.e().set_wander_lock("A", "fx", false);
    walk(&mut c, 60);
    let w = wander(&mut c, "A");
    assert!(!w.log.is_empty());
    assert!(w.log.iter().all(|m| [
        WanderMoveKind::Macro,
        WanderMoveKind::Fx,
        WanderMoveKind::Stinger
    ]
    .contains(&m.kind)));
}

// WanderRawNetsTests.theWalkIsPureOverThePlannerToo
#[test]
fn the_walk_is_pure_over_the_planner_too() {
    let mut c = raw_deck(ROLL);
    let ctx = c.e().wander_context("A").unwrap();
    assert!(ctx.is_raw() && ctx.raw_base.is_some());
    let mut kinds = std::collections::BTreeSet::new();
    for step in 0..400 {
        if let Some(p) = Wander::plan(&ctx, step) {
            kinds.insert(p.kind.raw());
        }
    }
    let raw: std::collections::BTreeSet<_> = Wander::RAW_KINDS.iter().map(|k| k.raw()).collect();
    assert!(kinds.is_subset(&raw) && kinds.contains("role"));
    assert!(!kinds.contains("seed") && !kinds.contains("genre") && !kinds.contains("structure"));
}

// MARK: WanderLayerDeckTests

fn lp(n: &str, cat: &str, beats: i64) -> LayerLoop {
    LayerLoop::new(&format!("/lib/{n}.caf"), n, cat, beats)
}

fn pool() -> Vec<LayerLoop> {
    vec![
        lp("Vox A", "Vocals", 16),
        lp("Vox B", "Vocals", 16),
        lp("Vox C", "Vocals", 8),
        lp("Riser A", "Sound Effect", 16),
        lp("Riser B", "Sound Effect", 16),
        lp("Pad A", "Texture/Atmosphere", 32),
    ]
}

const LAYER_ROLL: &str = r#"{"name":"L","tempo":128,"steps":128,"key":"A minor",
 "layers":[{"loop":"/lib/Vox A.caf","name":"Vox A","bar":3,"bars":4,"beats":16,"category":"Vocals"}],
 "parts":{"kick":{"every":16,"until":128,"notes":[[0],[4],[8],[12]]},
          "bass":{"every":16,"until":128,"notes":[[2,45,2],[6,45,2],[10,45,2],[14,45,2]]}}}"#;

/// `walked(bars:pool:)`: a raw deck with a pool and one layer, its Wander walk run `bars` bars
/// at pace 4.
fn layer_walked(bars: i64) -> Conductor {
    let mut c = raw_deck(LAYER_ROLL);
    c.e().deck_mut("A").unwrap().walk.layer_pool = pool();
    c.e().set_wander("A", true);
    c.e().set_wander_pace("A", 4);
    advance(&mut c, 0, bars);
    c
}

// WanderLayerDeckTests.aRawTracksLayersAndKeyComeFromItsPayload
#[test]
fn a_raw_tracks_layers_and_key_come_from_its_payload() {
    let mut c = layer_walked(0);
    let d = c.e().deck("A").unwrap();
    assert!(d.walk.layer_entries.len() == 1 && d.walk.layer_key_name.as_deref() == Some("A minor"));
    assert_eq!(c.e().layer_key("A").as_deref(), Some("A minor"));
    let e: &LayerEntry = &c.e().deck("A").unwrap().walk.layer_entries[0];
    assert!(
        e.loop_.name == "Vox A" && e.loop_.category == "Vocals" && e.start == 3 && e.length == 4
    );
    assert_eq!(c.e().deck("A").unwrap().walk.layer_epoch, 1);
}

// WanderLayerDeckTests.wanderAddsSwapsAndRemovesLoopsOnTheDeckAndLogsIt
#[test]
fn wander_adds_swaps_and_removes_loops_on_the_deck_and_logs_it() {
    // everything but layers locked: a cheap walk made only of layer moves
    let mut c = layer_walked(0);
    for lock in ["arrangement", "fx", "palette"] {
        c.e().set_wander_lock("A", lock, true);
    }
    advance(&mut c, 0, 160); // 40 phrases
    let w = wander(&mut c, "A");
    let kinds: std::collections::BTreeSet<_> = w.log.iter().map(|m| m.kind.raw()).collect();
    assert_eq!(
        kinds,
        ["layerAdd", "layerRemove", "layerSwap"]
            .into_iter()
            .collect(),
        "{kinds:?}"
    );
    for m in &w.log {
        assert!(m.spec.is_none() && !m.detail.is_empty());
    }
    let d = c.e().deck("A").unwrap();
    assert_eq!(d.walk.layer_epoch, 1, "moves don't make it a new track");
    // the list holds only what the pool and the track gave it, at most three on at the end
    let mut paths: std::collections::BTreeSet<String> =
        pool().iter().map(|l| l.path.clone()).collect();
    paths.insert("/lib/Vox A.caf".into());
    assert!(d
        .walk
        .layer_entries
        .iter()
        .all(|e| paths.contains(&e.loop_.path)));
    assert!(
        d.walk
            .layer_entries
            .iter()
            .filter(|e| e.end() > 160)
            .count()
            <= Wander::MAX_LAYERS
    );
}

// WanderLayerDeckTests.theSameTrackWalksTheSameLoopsAndALockStopsThem
#[test]
fn the_same_track_walks_the_same_loops_and_a_lock_stops_them() {
    fn layers_only(c: &mut Conductor) {
        for lock in ["arrangement", "fx", "palette"] {
            c.e().set_wander_lock("A", lock, true);
        }
        advance(c, 0, 80);
    }
    let (mut a, mut b, mut c) = (layer_walked(0), layer_walked(0), layer_walked(0));
    layers_only(&mut a);
    layers_only(&mut b);
    let la = a.e().deck("A").unwrap().walk.layer_entries.clone();
    assert!(la == b.e().deck("A").unwrap().walk.layer_entries && !la.is_empty());
    for lock in ["layers", "arrangement", "fx", "palette"] {
        c.e().set_wander_lock("A", lock, true);
    }
    advance(&mut c, 0, 80);
    assert!(
        wander(&mut c, "A").log.is_empty(),
        "nothing is left to move"
    );
    assert_eq!(c.e().deck("A").unwrap().walk.layer_entries.len(), 1);
}

// MARK: the moves' pokes

// DeckPerform.toggleStem / isStemMuted: a group remembers the roles that were playing, a new
// track clears them (`Deck.loadNet`: `stemsMuted = [:]`)
#[test]
fn stems_remember_what_was_playing_and_a_new_track_clears_them() {
    let mut c = Conductor::with(&TrackSpec::new("techno", 3.0, Some("standard")));
    c.e().set_playing("A", true);
    for _ in 0..40 {
        c.block();
    }
    let roles = c.e().roles("A");
    let music: Vec<String> = ["bass", "melody", "harmony", "arp"]
        .iter()
        .filter(|r| roles.iter().any(|x| x == *r))
        .map(|r| r.to_string())
        .collect();
    // the group remembers the roles that were playing when it muted them
    let playing: Vec<String> = music
        .iter()
        .filter(|r| c.e().is_role_audible("A", r))
        .cloned()
        .collect();
    c.e().toggle_stem("A", "music");
    for _ in 0..2 {
        c.block();
    }
    assert!(c.e().is_stem_muted("A", "music") && !c.e().is_stem_muted("A", "mic"));
    assert_eq!(
        c.e()
            .deck("A")
            .unwrap()
            .walk
            .stems_muted
            .groups
            .get("music"),
        Some(&playing)
    );
    for r in &music {
        assert!(!c.e().is_role_audible("A", r), "{r} is muted by the group");
    }
    // the lead is in the group already: what the other group silenced is not "silent now" for
    // this one, it is remembered too
    c.e().toggle_stem("A", "mic");
    let held: Vec<String> = playing.iter().filter(|r| *r == "melody").cloned().collect();
    assert_eq!(
        c.e().deck("A").unwrap().walk.stems_muted.groups.get("mic"),
        Some(&held)
    );
    c.e().toggle_stem("A", "music");
    for _ in 0..2 {
        c.block();
    }
    assert!(
        !c.e().is_role_audible("A", "melody"),
        "mic still holds the lead (or it was never playing)"
    );
    for r in playing.iter().filter(|r| *r != "melody") {
        assert!(c.e().is_role_audible("A", r), "{r} is back");
    }
    // a new track clears them
    let pr = c.e().preparer("A").unwrap();
    let again = TrackSpec::new("techno", 4.0, Some("standard"));
    assert!(c.e().load_net("A", PreparedTrack::compose(&again, &pr, 0)));
    assert!(!c.e().is_stem_muted("A", "mic"));
    assert!(c.e().deck("A").unwrap().walk.stems_muted.groups.is_empty());
}

// MARK: PaletteDeckTests and StingerCurationTests, the deck halves

fn palette_deck(palette: Option<&str>) -> Conductor {
    let mut c = Conductor::new();
    let pr = c.e().preparer("A").unwrap();
    let base = TrackSpec::new("house", 11.0, Some("standard"));
    let installed = Installed::default();
    let spec = base.with_palette(palette, &installed);
    assert!(c.e().load_net("A", PreparedTrack::compose(&spec, &pr, 0)));
    c
}

fn melody_option(c: &mut Conductor) -> Option<String> {
    let p = c.e().deck("A").unwrap().project.clone().unwrap();
    djrust::sound::project_sound_net(&p, "melody").and_then(|nb| nb.track.sound.option.clone())
}

fn source_names(c: &mut Conductor, field: &str, value: &str) -> bool {
    let spec = c.e().deck("A").unwrap().spec.clone().unwrap();
    spec.layers
        .get("tracks")
        .and_then(JsonValue::as_object)
        .is_some_and(|t| {
            t.iter()
                .any(|(_, o)| o.get(field).and_then(JsonValue::as_str) == Some(value))
        })
}

// PaletteDeckTests.overridesSurviveRecomputesUntilReset (without the fine-tune rows, which are
// `deck_role_sounds.rs`'): a role override is written to the source, survives the next version,
// the seed wheel and Wander's swaps, and the reset puts the palette's own pick back.
#[test]
fn a_role_voice_survives_recomputes_until_reset() {
    // (a palette that picks the melody a sound on a machine with no bank installed)
    let name = ["synthwave", "club", "chiptune", "synth"]
        .into_iter()
        .find(|p| melody_option(&mut palette_deck(Some(p))).is_some())
        .expect("a palette that voices the melody here");
    let mut c = palette_deck(Some(name));
    let own = melody_option(&mut c);
    let set = PaletteSet::shared();
    let pool = set.named(Some(name)).unwrap().pool("melody").unwrap();
    let other = pool
        .iter()
        .find(|o| Some(o.as_str()) != own.as_deref() && o.starts_with("synth:"))
        .expect("another synth voice")
        .clone();
    c.e().set_role_voice("A", "melody", Some(&other));
    assert!(source_names(&mut c, "voice", &other));
    assert_eq!(melody_option(&mut c).as_deref(), Some(other.as_str()));
    // Next / auto-next: the next version keeps the palette and the override
    let spec = c.e().deck("A").unwrap().spec.clone().unwrap();
    let next = spec.next_version();
    assert!(next.seed != spec.seed && next.palette() == spec.palette());
    // the seed wheel
    c.e().step_seed("A", 3);
    assert_eq!(
        c.e().deck("A").unwrap().spec.as_ref().unwrap().seed,
        spec.seed + 3.0
    );
    assert_eq!(melody_option(&mut c).as_deref(), Some(other.as_str()));
    // Wander: every swap it plans carries it (a palette hop changes the palette, not this)
    let ctx = c.e().wander_context("A").unwrap();
    let mut swaps = 0;
    for step in 0..300 {
        if swaps >= 8 {
            break;
        }
        let Some(p) = Wander::plan(&ctx, step) else {
            continue;
        };
        let WanderAction::Swap(s) = p.action else {
            continue;
        };
        swaps += 1;
        let has = s
            .layers
            .get("tracks")
            .and_then(JsonValue::as_object)
            .is_some_and(|t| {
                t.iter().any(|(_, o)| {
                    o.get("voice").and_then(JsonValue::as_str) == Some(other.as_str())
                })
            });
        assert!(has, "{}", p.detail);
    }
    assert_eq!(swaps, 8);
    // reset to the palette
    c.e().set_role_voice("A", "melody", None);
    assert!(!source_names(&mut c, "voice", &other));
    let seed = c.e().deck("A").unwrap().seed;
    let expected = c.e().with_sounds(|ctx| {
        set.named(Some(name))
            .unwrap()
            .pick("melody", "melody", seed, set, ctx)
    });
    assert_eq!(melody_option(&mut c), expected);
}

// PaletteDeckTests.setPaletteKeepsOverridesAndTheDefaultClearsIt
#[test]
fn set_palette_keeps_overrides_and_the_default_clears_it() {
    let mut c = palette_deck(Some("lofi"));
    c.e().set_role_voice("A", "bass", Some("gm:32"));
    c.e().set_palette("A", Some("club"));
    let p = c.e().deck("A").unwrap().project.clone().unwrap();
    assert_eq!(project_palette(&p).as_deref(), Some("club"));
    let bass = djrust::sound::project_sound_net(&p, "bass").unwrap();
    assert_eq!(bass.track.sound.option.as_deref(), Some("gm:32"));
    c.e().set_palette("A", Some("studio"));
    let p = c.e().deck("A").unwrap().project.clone().unwrap();
    assert_eq!(project_palette(&p), None);
    let spec = c.e().deck("A").unwrap().spec.clone().unwrap();
    assert_eq!(spec.palette(), None);
    assert!(source_names(&mut c, "voice", "gm:32"), "the override stays");
}

// PaletteDeckTests.theDrumsFollowThePaletteUntilAKitIsPicked, the clearing half: back to the
// palette drops a picked kit (`setRoleVoice(kit, nil)`). The pick is the one `Deck.setKit` makes
// (`tracks` `kit` on the drum channels: this host has no `set_kit` yet, so the spec carries it
// in as `DeckSourceDescriptor.withKit` would write it); a voice set on top of it keeps it, and
// only the clearing drops it, from the project and from the source. (Not in the Swift golden:
// the dump has no `setKit`.)
#[test]
fn clearing_the_kit_voice_drops_a_picked_kit() {
    let pick = "MuseScore_General.sf2#128:32";
    let mut spec = TrackSpec::new("house", 11.0, Some("standard"));
    let drums: Vec<i64> = {
        let p = spec.project();
        let mut ch: Vec<i64> = p
            .nets
            .iter()
            .filter(|(_, nb)| {
                nb.role != "control" && djrust::sound::is_drum_channel(nb.track.channel)
            })
            .map(|(_, nb)| nb.track.channel)
            .collect();
        ch.sort_unstable();
        ch.dedup();
        ch
    };
    assert!(!drums.is_empty());
    djrust::wander::carry::with_kit(&mut spec, Some(pick), &drums);
    let mut c = Conductor::new();
    let pr = c.e().preparer("A").unwrap();
    assert!(c.e().load_net("A", PreparedTrack::compose(&spec, &pr, 0)));
    let kit = |c: &mut Conductor| {
        let p = c.e().deck("A").unwrap().project.clone().unwrap();
        project_kit(&p)
    };
    assert_eq!(kit(&mut c).as_deref(), Some(pick));
    assert!(source_names(&mut c, "kit", pick));
    // a voice on the drums does not drop the pick
    c.e().set_role_voice("A", "kit", Some("synth:drums-808"));
    assert!(source_names(&mut c, "voice", "synth:drums-808"));
    assert!(source_names(&mut c, "kit", pick), "the pick stays");
    // back to the palette: the voice and the pick both go
    c.e().set_role_voice("A", "kit", None);
    assert!(!source_names(&mut c, "voice", "synth:drums-808"));
    assert!(!source_names(&mut c, "kit", pick));
    assert_eq!(kit(&mut c), None);
}

// StingerCurationTests.aDeckFiresItsAssignments
#[test]
fn a_deck_fires_its_assignments() {
    let mut c = Conductor::new();
    let pr = c.e().preparer("A").unwrap();
    let spec = TrackSpec::new("house", 11.0, Some("standard"));
    assert!(c.e().load_net("A", PreparedTrack::compose(&spec, &pr, 0)));
    let hits = c.e().stingers("A");
    assert_eq!(hits.len(), 4);
    let resolved = |c: &mut Conductor| {
        let p = c.e().deck("A").unwrap().project.clone().unwrap();
        c.e().with_sounds(|ctx| {
            let set = PaletteSet::shared();
            let content = StingerContent::here(set, ctx, Vec::new());
            resolved_project_stingers(&p, set, "house", 11.0, &content)
        })
    };
    assert_eq!(hits, resolved(&mut c));
    c.e().set_palette("A", Some("orchestral"));
    let after = c.e().stingers("A");
    assert_eq!(after, resolved(&mut c));
    let d = c.e().deck("A").unwrap();
    assert!(d
        .spec
        .as_ref()
        .is_some_and(|s| s.genre == "house" && s.seed == 11.0));
    // every Hit fires without touching the composition
    let before = d.project.as_ref().unwrap().to_json().js_stringified();
    for n in 0..4 {
        c.e().stinger("A", n, 1);
    }
    let d = c.e().deck("A").unwrap();
    assert_eq!(
        d.project.as_ref().unwrap().to_json().js_stringified(),
        before
    );
    assert!(after.iter().all(|a| a.json().get("label").is_some()));
}

// A Hit curated as a macro fires the macro, not its stinger net (`Deck.stinger`): the commands
// that reached the runner are in its fires (`record_events`)
#[test]
fn a_stinger_n_out_of_range_does_nothing() {
    let mut c = Conductor::with(&techno());
    c.e().stinger("A", 4, 1);
    c.e().stinger("A", -1, 1);
    c.e().stinger("B", 0, 1); // a sample deck
    assert_eq!(c.e().dropped, 0);
}
