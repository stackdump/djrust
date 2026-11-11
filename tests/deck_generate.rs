//! The track pipeline on the live engine (`DeckGenerator.swift`, `DeckPerform.swift`):
//! `generate`, `swap`, `stepSeed`, the look-ahead, the next track and `service`, relocation with
//! the song's options, `mixOut` / `mixIn`.
//!
//! Parity form (a): each test names the Swift test whose literals it asserts
//! (`WanderSwapTimingTests`, `DeckNetTests`, `AutoNextTests`, `BrowserSeedTests`,
//! `FixesUnderJumpsTests`, `MixOutActionTests`, `MixOutTailTests`). The `swap` golden
//! (`swap_golden.rs`) holds the same calls to the Swift engine's output.
//!
//! A Swift `await deck.generate(...)` is `Engine::generate_at` (it blocks until the track is
//! loaded); the background paths are driven with `begin_generate` / `settle` /
//! `wait_composes` and `deterministic = false`.

mod common_track;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common_track::Stepper;
use djrust::deck::slots::{LookAheadKey, SwapStart};
use djrust::deck::swap::Latest;
use djrust::gen::structure::build_control_bundle;
use djrust::json::JsonValue;
use djrust::pflow::{ControlBinding, NetBundle, Section};
use djrust::wave::rests::{Entry, MixOutTail};
use djrust::{Project, TrackSpec};
use djrust::host::deck::{PreparedTrack, TrackSource};

const RATE: f64 = 22_050.0;

fn song() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("standard"))
}

fn next() -> TrackSpec {
    TrackSpec::new("techno", 43.0, Some("standard"))
}

/// `WanderSwapTimingTests.playing(toTick:)`: deck A playing `song()` in the offline engine, run
/// to `tick`, B the demo loop. The engine only renders when stepped, so the deck stays put
/// while a `generate` composes.
fn playing(to_tick: i64) -> Stepper {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &song()));
    assert!(st.e.load_demo("B"));
    st.e.play("A");
    st.run_to_tick("A", to_tick);
    st
}

fn latest(st: &Stepper, deck: &str) -> Latest {
    st.e.deck(deck).unwrap().track.latest.clone().unwrap()
}

fn spec_of(st: &Stepper, deck: &str) -> Option<TrackSpec> {
    st.e.deck(deck).unwrap().spec.clone()
}

// MARK: Task

#[test]
fn a_task_finishes_off_the_calling_thread_and_hands_back_its_value_once() {
    use djrust::host::deck::Task;
    let t = Task::spawn(|| (1..=10).sum::<u32>());
    assert_eq!(t.wait(), 55);
    assert!(t.try_take().is_none(), "taken");
    let now = Task::immediate(7);
    assert!(now.is_finished());
    assert_eq!(now.try_take(), Some(7));
    assert_eq!(now.try_take(), None);
    let ran = Task::run(|| "inline");
    assert_eq!(ran.wait(), "inline");
    // a compose that panics does not hang its waiter forever
    let bad = Task::spawn(|| -> u32 { panic!("boom") });
    while !bad.is_finished() {
        std::thread::yield_now();
    }
    assert!(bad.try_take().is_none());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bad.wait())).is_err());
    // the housekeeping asks without a panic of its own
    let bad = Task::spawn(|| -> u32 { panic!("boom") });
    assert_eq!(bad.wait_ok(), None);
}

// MARK: WanderSwapTimingTests

// WanderSwapTimingTests.aSwapComposedPastItsBoundaryStartsAtTheNextBar
#[test]
fn a_swap_composed_past_its_boundary_starts_at_the_next_bar() {
    // in bar 3: bar 3's boundary (32) passed
    let mut st = playing(40);
    let now = st.tick("A");
    assert!((40..48).contains(&now), "tick {now}");
    assert!(st.e.generate_at("A", &next(), SwapStart::Tick(32)));
    assert_eq!(
        latest(&st, "A").start_tick,
        48,
        "starts at the bar the install lands on"
    );
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert!(st.until_installed("A"));
    let d = st.e.deck("A").unwrap();
    assert!(d.playing && d.mirror.is_playing());
    assert!(st.tick("A") >= 48, "not a bar back: tick {}", st.tick("A"));
}

// WanderSwapTimingTests.anOnTimeSwapStillStartsAtItsBoundary
#[test]
fn an_on_time_swap_still_starts_at_its_boundary() {
    let mut st = playing(40);
    assert!(st.tick("A") < 48);
    assert!(st.e.generate_at("A", &next(), SwapStart::Tick(48)));
    assert_eq!(latest(&st, "A").start_tick, 48);
    assert!(st.until_installed("A"));
    let t = st.tick("A");
    assert!((48..64).contains(&t), "tick {t}");
}

// WanderSwapTimingTests.aRestartKeepsTheTrackAndItsMeasurements
#[test]
fn a_restart_keeps_the_track_and_its_measurements() {
    let st = Stepper::new(RATE, true);
    let pr = st.e.preparer("A").unwrap();
    let track = PreparedTrack::compose(&next(), &pr, 32);
    assert_eq!(track.prepared.wave().start_tick, 32);
    let again = track.restarted(&pr, 48);
    let (a, b) = (again.prepared.wave(), track.prepared.wave());
    assert_eq!(a.start_tick, 48);
    assert_eq!(again.source, track.source);
    assert_eq!(
        again.played.to_json().canonical_js(),
        track.played.to_json().canonical_js()
    );
    assert_eq!(a.track_serial, b.track_serial, "the same song");
    assert!(a.generation > b.generation);
    assert_eq!(a.gain, b.gain);
    assert_eq!(a.energy, b.energy);
    assert_eq!(a.balance, b.balance);
    assert_eq!(a.rests, b.rests);
    assert_eq!(a.space, b.space);
    assert_eq!(a.echo, b.echo);
    assert_eq!(a.duck_depth, b.duck_depth);
    // past the song's end it starts at the top, as `init(spec:startTick:)` does
    assert_eq!(
        track.restarted(&pr, 10_000_000).prepared.wave().start_tick,
        0
    );
    // (and a track composed there starts at the top too)
    assert_eq!(
        PreparedTrack::compose(&next(), &pr, 10_000_000)
            .prepared
            .wave()
            .start_tick,
        0
    );
}

fn key(step: i64, spec: &TrackSpec, start: SwapStart) -> LookAheadKey {
    LookAheadKey {
        step,
        spec: spec.clone(),
        start,
        role_filter: None,
    }
}

// WanderSwapTimingTests.aPlannedSwapIsComposedBeforeItsPhraseEnds, the slot's half (the plan is
// Wander's): composed ahead for a swap at tick 64, not ready until it is, composed from 64
// whatever the deck's clock; the move takes it, and it is what the deck loads.
#[test]
fn a_look_ahead_is_composed_for_its_tick_and_loaded_as_it_is() {
    let mut st = playing(40);
    st.e.deterministic = false;
    let k = key(3, &next(), SwapStart::Tick(64));
    assert!(st.e.look_ahead_key("A").is_none());
    assert!(st.e.request_look_ahead("A", k.clone()));
    assert_eq!(st.e.look_ahead_key("A"), Some(k.clone()));
    st.e.wait_look_ahead("A");
    assert!(st.e.look_ahead_ready("A"));
    assert!(st.tick("A") < 48);
    let before = latest(&st, "A").generation;
    let taken = st.e.take_look_ahead("A", &k).expect("the key matches");
    assert!(
        st.e.look_ahead_key("A").is_none(),
        "the slot is empty after"
    );
    let ahead = taken.ready.as_ref().expect("composed");
    assert_eq!(ahead.prepared.wave().start_tick, 64);
    assert_eq!(ahead.spec(), Some(&next()));
    let identity = ahead.prepared.wave().generation;
    st.e.swap_with_ahead("A", &next(), SwapStart::Tick(64), taken);
    let l = latest(&st, "A");
    assert_eq!(l.generation, identity, "the look-ahead's prepared, itself");
    assert!(l.generation > before);
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert!(
        st.e.deck("A").unwrap().track.seed_target.is_none(),
        "nothing is still composing"
    );
    assert!(st.until_installed("A"));
    let t = st.tick("A");
    assert!((64..80).contains(&t), "installed tick {t}");
}

// WanderSwapTimingTests.aPokeBetweenDropsTheLookAhead: a look-ahead only serves the key it was
// composed for (a poke replans the move: another key), and either way the slot is empty after.
#[test]
fn a_look_ahead_serves_only_its_own_key() {
    let mut st = playing(40);
    let k = key(3, &next(), SwapStart::Tick(64));
    st.e.request_look_ahead("A", k.clone());
    st.e.wait_look_ahead("A");
    let replanned = key(
        3,
        &TrackSpec::new("house", 7.0, Some("standard")),
        SwapStart::Tick(64),
    );
    assert!(st.e.take_look_ahead("A", &replanned).is_none());
    assert!(st.e.look_ahead_key("A").is_none(), "dropped, not kept");
    // a role filter is part of the key
    st.e.request_look_ahead("A", k.clone());
    let filtered = LookAheadKey {
        role_filter: Some(djrust::wave::RoleFilter {
            mute: vec!["bass".into()],
            solo: None,
        }),
        ..k.clone()
    };
    assert!(st.e.take_look_ahead("A", &filtered).is_none());
    // `drop()`
    st.e.request_look_ahead("A", k.clone());
    st.e.wait_look_ahead("A");
    st.e.drop_look_ahead("A");
    assert!(st.e.look_ahead_key("A").is_none() && !st.e.look_ahead_ready("A"));
    // a newer request replaces the one held
    st.e.request_look_ahead("A", k.clone());
    st.e.request_look_ahead("A", replanned.clone());
    assert_eq!(st.e.look_ahead_key("A"), Some(replanned));
    st.e.wait_composes("A");
}

// DeckGenerator.swift `swap(to:start:ahead:)`: composed already but late (`start.passed`), the
// look-ahead is installed as a generate would: prepared again from the bar after the deck's tick.
#[test]
fn a_late_look_ahead_is_prepared_again_from_the_next_bar() {
    let mut st = playing(40);
    st.e.deterministic = false;
    let k = key(1, &next(), SwapStart::Tick(32));
    st.e.request_look_ahead("A", k.clone());
    st.e.wait_look_ahead("A");
    let taken = st.e.take_look_ahead("A", &k).unwrap();
    assert_eq!(taken.ready.as_ref().unwrap().prepared.wave().start_tick, 32);
    st.e.swap_with_ahead("A", &next(), SwapStart::Tick(32), taken);
    // the deck waits for it: `seed_target` holds the spec while it lands
    assert_eq!(
        st.e.deck("A").unwrap().track.seed_target.as_ref(),
        Some(&next())
    );
    st.e.wait_composes("A");
    assert_eq!(latest(&st, "A").start_tick, 48);
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert!(st.e.deck("A").unwrap().track.seed_target.is_none());
    assert!(st.until_installed("A"));
    assert!(st.tick("A") >= 48);
}

// DeckGenerator.swift `swap(to:start:)`: inline in `deterministic` replay, else in the background
// with the spec held in `seed_target`.
#[test]
fn swap_to_composes_inline_in_replay_and_in_the_background_otherwise() {
    let mut st = playing(40);
    assert!(st.e.deterministic);
    let generation = st.e.deck("A").unwrap().track.generation;
    assert!(st.e.swap_to("A", &next(), SwapStart::Tick(64)));
    assert_eq!(latest(&st, "A").start_tick, 64);
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert!(st.e.deck("A").unwrap().track.seed_target.is_none());
    assert_eq!(
        st.e.deck("A").unwrap().track.generation,
        generation,
        "a replay's swap is not a `generate`"
    );
    // in the background: the target is held until it lands
    let mut st = playing(40);
    st.e.deterministic = false;
    assert!(st.e.swap_to("A", &next(), SwapStart::NextBar));
    assert_eq!(
        st.e.deck("A").unwrap().track.seed_target.as_ref(),
        Some(&next())
    );
    assert_eq!(spec_of(&st, "A"), Some(song()), "not loaded yet");
    st.e.wait_composes("A");
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert!(st.e.deck("A").unwrap().track.seed_target.is_none());
    let nb = SwapStart::NextBar.tick(st.tick("A"));
    assert!(latest(&st, "A").start_tick <= nb);
}

// MARK: DeckNetTests

// DeckNetTests.generatedTrackPlaysThroughTheDeck
#[test]
fn a_generated_track_plays_through_the_deck() {
    let mut st = Stepper::new(48_000.0, true);
    assert!(
        !st.e.deck("A").unwrap().is_net(),
        "a fresh deck is a sample deck"
    );
    assert!(st.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    let d = st.e.deck("A").unwrap();
    assert!(d.is_net());
    assert_eq!(d.spec.as_ref().unwrap().seed, 42.0);
    assert_eq!(d.project.as_ref().unwrap().name.get(..6), Some("techno"));
    st.e.play("A");
    st.step(375); // 48,000 frames
    assert!(st.take_peak() > 0.05);
    let d = st.e.deck("A").unwrap();
    assert!(d.mirror.is_playing() && d.mirror.tick() > 4);
    assert_eq!(
        d.mirror.turntable_position(),
        0.0,
        "the turntable stays parked"
    );
}

// DeckNetTests.theLatestGenerateWins
#[test]
fn the_latest_generate_wins() {
    let mut st = Stepper::new(RATE, true);
    let first =
        st.e.begin_generate(
            "A",
            &TrackSpec::new("jazz", 1.0, Some("standard")),
            SwapStart::Top,
        )
        .unwrap();
    let second =
        st.e.begin_generate("A", &TrackSpec::new("dnb", 2.0, None), SwapStart::Top)
            .unwrap();
    let r = st.e.settle(0, true);
    assert_eq!(r, vec![(first, false), (second, true)]);
    assert_eq!(spec_of(&st, "A").unwrap().genre, "dnb");
    // the service tick lands what finished, too
    st.e.deterministic = false;
    let g =
        st.e.begin_generate("A", &TrackSpec::new("house", 3.0, None), SwapStart::Top)
            .unwrap();
    assert!(g > second);
    let mut waited = 0;
    while spec_of(&st, "A").unwrap().genre != "house" {
        st.e.service();
        std::thread::sleep(std::time::Duration::from_millis(2));
        waited += 1;
        assert!(waited < 5_000, "the compose never landed");
    }
    assert!(!st.e.deck("A").unwrap().track.is_busy());
}

// DeckNetTests.nextTrackIsComposedInTheBackgroundThenAdvanced
#[test]
fn the_next_track_is_composed_in_the_background_then_advanced() {
    let mut st = Stepper::new(RATE, true);
    st.e.deterministic = false;
    assert!(st.e.generate("A", &TrackSpec::new("techno", 1.0, None)));
    assert!(st.e.next_ready("A").is_none() && !st.e.advance_to_next("A"));
    let spec = TrackSpec::new("house", 9.0, Some("minimal"));
    st.e.queue_next("A", &spec);
    assert_eq!(st.e.next_composing("A"), Some(&spec));
    assert!(st.e.next_ready("A").is_none());
    st.e.wait_composes("A");
    assert_eq!(st.e.next_ready("A"), Some(&spec));
    assert!(st.e.next_composing("A").is_none());
    assert!(st.e.advance_to_next("A"));
    assert_eq!(spec_of(&st, "A"), Some(spec.clone()));
    assert!(st
        .e
        .deck("A")
        .unwrap()
        .project
        .as_ref()
        .unwrap()
        .name
        .starts_with("house"));
    assert!(st.e.next_ready("A").is_none(), "taken");
    // a newer request supersedes an older one; cancel forgets both
    st.e.queue_next("A", &TrackSpec::new("lofi", 1.0, None));
    st.e.queue_next("A", &TrackSpec::new("dnb", 2.0, None));
    st.e.wait_composes("A");
    assert_eq!(st.e.next_ready("A").unwrap().genre, "dnb");
    st.e.cancel_next("A");
    assert!(st.e.next_ready("A").is_none() && st.e.next_composing("A").is_none());
    assert!(!st.e.advance_to_next("A"));
    // in replay it is composed inline, ready at once
    st.e.deterministic = true;
    st.e.queue_next("A", &spec);
    assert_eq!(st.e.next_ready("A"), Some(&spec));
}

// DeckNetTests.aSongThatEndsHandsOverToThePrecomposedNext
#[test]
fn a_song_that_ends_hands_over_to_the_precomposed_next() {
    let mut st = Stepper::new(RATE, true);
    st.e.deck_mut("A").unwrap().auto_advance = true;
    assert!(st
        .e
        .generate("A", &TrackSpec::new("ambient", 3.0, Some("minimal"))));
    st.e.queue_next("A", &TrackSpec::new("techno", 5.0, Some("minimal")));
    st.e.wait_composes("A");
    assert!(st.e.next_ready("A").is_some());
    st.e.set_tempo("A", 3000.0); // ~5 ms per tick: the 320-tick song ends in ~1.6 s
    st.e.play("A");
    let handed = st.until(40 * 94, |s| {
        s.e.deck("A").unwrap().spec.as_ref().unwrap().genre == "techno"
    });
    assert!(
        handed,
        "service() must start the next track when the song ends"
    );
    assert!(st.e.deck("A").unwrap().playing);
}

// DeckGenerator.swift `service()`: "a song just handed over: until the audio thread starts it, the
// old one still reads as ended, and taking that for a new ending would skip a whole version".
// Two ticks with no block between them (the housekeeping runs ~100 Hz, the audio thread in
// its own time) hand over once.
#[test]
fn a_song_just_handed_over_is_not_taken_for_ending_again() {
    let mut st = Stepper::new(RATE, true);
    st.e.deck_mut("A").unwrap().auto_advance = true;
    assert!(st
        .e
        .generate("A", &TrackSpec::new("ambient", 3.0, Some("minimal"))));
    st.e.set_tempo("A", 3000.0);
    st.e.play("A");
    st.e.service(); // auto-next composes the next version
    assert!(st.e.next_ready("A").is_some());
    // the song ends on the audio thread; the housekeeping has not seen it yet
    let mut blocks = 0;
    while !st
        .e
        .deck("A")
        .unwrap()
        .mirror
        .stopped
        .load(Ordering::Relaxed)
    {
        st.r.render_block(&mut st.l, &mut st.rr);
        blocks += 1;
        assert!(blocks < 40_000, "the song never ended");
    }
    assert!(st.e.deck("A").unwrap().playing, "not noticed yet");
    st.e.service();
    let d = st.e.deck("A").unwrap();
    assert_eq!(d.track.advances, 1);
    assert!(d.playing && d.track.starting_next);
    // the audio thread has not started it: the old song still reads as ended
    st.e.service();
    let d = st.e.deck("A").unwrap();
    assert_eq!(d.track.advances, 1, "no second version skipped");
    assert!(d.playing && d.track.starting_next);
    // once it plays, the check is open again
    st.step(2);
    st.e.service();
    let d = st.e.deck("A").unwrap();
    assert!(d.mirror.is_playing() && !d.track.starting_next);
    assert_eq!(d.track.advances, 1);
}

// DeckNetTests.nextProviderKeepsOneComing
#[test]
fn a_next_provider_keeps_one_coming() {
    let mut st = Stepper::new(RATE, true);
    st.e.deterministic = false;
    let n = Arc::new(AtomicUsize::new(0));
    let counter = n.clone();
    st.e.deck_mut("A").unwrap().track.next_provider = Some(Box::new(move || {
        let k = counter.fetch_add(1, Ordering::SeqCst) + 1;
        TrackSpec::new("lofi", k as f64, None)
    }));
    assert!(st.e.generate("A", &TrackSpec::new("techno", 1.0, None)));
    st.e.service();
    assert!(st.e.next_composing("A").is_some() || st.e.next_ready("A").is_some());
    st.e.wait_composes("A");
    assert!(st.e.next_ready("A").is_some());
    let before = n.load(Ordering::SeqCst);
    st.e.service();
    assert_eq!(
        n.load(Ordering::SeqCst),
        before,
        "no new request while one is ready"
    );
    assert!(st.e.advance_to_next("A"));
    st.e.service();
    assert_eq!(n.load(Ordering::SeqCst), before + 1);
}

// DisableTests.AutoNextTests.autoNextComposesAndPlaysTheNextVersion
#[test]
fn auto_next_composes_and_plays_the_next_version() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.deterministic, "composes inline");
    let spec = TrackSpec::new("techno", 42.0, Some("minimal"));
    assert!(st.e.generate("A", &spec));
    st.e.play("A");
    // turned on after the track is already playing: no Queue next, nothing set beforehand
    st.e.deck_mut("A").unwrap().auto_advance = true;
    st.e.service();
    assert_eq!(
        st.e.next_ready("A"),
        Some(&spec.next_version()),
        "the next version is composed at once"
    );
    let nv = spec.next_version();
    assert!(nv.seed != 42.0 && nv.genre == "techno");
    assert_eq!(nv.structure.as_deref(), Some("minimal"));
    st.e.set_tempo("A", 3000.0); // get to the end of the song quickly
    let handed = st.until(4000 * 16, |s| s.e.deck("A").unwrap().track.advances > 0);
    assert!(handed);
    assert_eq!(
        st.e.deck("A").unwrap().track.advances,
        1,
        "the song ended and handed over"
    );
    assert_eq!(spec_of(&st, "A").unwrap().seed, nv.seed);
    assert!(st.e.deck("A").unwrap().playing);
    st.e.service();
    assert_eq!(
        st.e.next_ready("A"),
        Some(&nv.next_version()),
        "and the one after is on its way"
    );
}

// DeckGenerator.swift `skipToNextTrack` / FixesUnderJumpsTests.skipToNextTrackKeepsEveryFix:
// the hand-over plays the next track from its top and keeps the deck playing; with none ready
// nothing happens; a stopped deck stays stopped unless auto-next is on.
#[test]
fn skipping_hands_over_to_the_queued_track_and_keeps_playing() {
    let mut st = Stepper::new(RATE, true);
    let spec = |seed: f64| TrackSpec::new("techno", seed, Some("standard"));
    assert!(st.e.generate("A", &spec(42.0)));
    st.e.skip_to_next_track("A");
    assert_eq!(spec_of(&st, "A"), Some(spec(42.0)), "nothing queued");
    st.e.queue_next("A", &spec(43.0));
    st.e.skip_to_next_track("A");
    assert_eq!(spec_of(&st, "A").unwrap().seed, 43.0);
    assert!(!st.e.deck("A").unwrap().playing, "it was stopped");
    assert_eq!(st.e.deck("A").unwrap().track.advances, 1);
    assert_eq!(latest(&st, "A").start_tick, 0);
    st.e.play("A");
    st.step(8);
    st.e.queue_next("A", &spec(44.0));
    st.e.skip_to_next_track("A");
    assert_eq!(spec_of(&st, "A").unwrap().seed, 44.0);
    assert!(st.e.deck("A").unwrap().playing);
    // a stopped deck with auto-next on starts the track it hands over to
    st.e.pause("A");
    st.e.deck_mut("A").unwrap().auto_advance = true;
    st.e.queue_next("A", &spec(45.0));
    st.e.skip_to_next_track("A");
    assert!(st.e.deck("A").unwrap().playing);
    assert_eq!(st.e.deck("A").unwrap().track.advances, 3);
}

// DeckNetTests.aRawNetsSharePlaysOnADeck (the part before the session recorder)
#[test]
fn a_raw_nets_share_plays_on_a_deck() {
    let path = format!(
        "{}/tests/fixtures/js/share-urls.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc = JsonValue::parse(&text).unwrap();
    let rows = doc.get("rawNets").and_then(JsonValue::as_array).unwrap();
    let row = rows
        .iter()
        .find(|r| {
            r.get("payload")
                .and_then(|p| p.get("name"))
                .and_then(JsonValue::as_str)
                == Some("Hand Made")
        })
        .expect("the Hand Made row");
    let payload = row.get("payload").unwrap().clone();
    let want = row.get("project").unwrap();
    let mut st = Stepper::new(RATE, true);
    let pr = st.e.preparer("A").unwrap();
    let track = PreparedTrack::from_raw_nets(&payload, &pr).expect("nets");
    assert!(track.spec().is_none());
    assert_eq!(track.source, TrackSource::RawNets(payload.clone()));
    // no pass, the runner's own fixes: what Swift records
    assert!(!track.pass_applied);
    assert_eq!(
        track.fixes,
        djrust::musicality::Fixes::ALL & djrust::musicality::Fixes::RUNNER_ONLY
    );
    assert!(st.e.load_net("A", track));
    let d = st.e.deck("A").unwrap();
    assert!(d.is_net() && d.spec.is_none());
    assert_eq!(d.track.raw_nets.as_ref(), Some(&payload));
    assert_eq!(
        d.project.as_ref().unwrap().name,
        want.get("name").and_then(JsonValue::as_str).unwrap()
    );
    assert_eq!(
        d.base_tempo,
        want.get("tempo").and_then(JsonValue::as_f64).unwrap()
    );
    let cid = want.get("modelCID").and_then(JsonValue::as_str).unwrap();
    assert_eq!(d.project.as_ref().unwrap().model_cid(), cid);
    assert!(!d.track.pass_applied);
    st.e.play("A");
    st.step(750); // 96,000 frames
    assert!(st.take_peak() > 0.05);
    let d = st.e.deck("A").unwrap();
    assert!(d.mirror.is_playing() && d.mirror.tick() > 4);
    // built with the deck's options, any key order: the same project
    let sorted = JsonValue::parse(&payload.canonical_js()).unwrap();
    let again = PreparedTrack::from_raw_nets(&sorted, &pr).unwrap();
    assert_eq!(again.project.model_cid(), cid);
    // a payload without nets is no track
    let mut o = djrust::json::JsonObject::new();
    o.set("genre", "x".into());
    assert!(PreparedTrack::from_raw_nets(&JsonValue::Object(o), &pr).is_none());
    // a composed track after it takes the deck back: the payload is let go
    assert!(st.e.generate("A", &song()));
    let d = st.e.deck("A").unwrap();
    assert!(d.track.raw_nets.is_none() && d.spec.is_some() && d.track.pass_applied);
    assert_eq!(d.track.fixes, Some(djrust::musicality::Fixes::ALL));
}

// RunnerOnlyFixesTests.theDeckRecordsWhatItPlaysWith
#[test]
fn the_deck_records_what_it_plays_with() {
    let mut st = Stepper::new(RATE, true);
    let d = st.e.deck("A").unwrap();
    assert!(d.track.fixes.is_none() && !d.track.pass_applied);
    assert!(st
        .e
        .generate("A", &TrackSpec::new("house", 3.0, Some("minimal"))));
    let d = st.e.deck("A").unwrap();
    assert_eq!(d.track.fixes, Some(djrust::musicality::Fixes::ALL));
    assert!(d.track.pass_applied);
    // a deck without the pass: nothing is rewritten, no fixes asked
    let mut bare = Stepper::new(RATE, false);
    assert!(bare
        .e
        .generate("A", &TrackSpec::new("house", 3.0, Some("minimal"))));
    let d = bare.e.deck("A").unwrap();
    assert!(!d.track.pass_applied);
    assert!(d.track.fixes.unwrap().is_empty());
}

// MARK: BrowserSeedTests

mod wheel {
    use super::*;
    use djrust::host::deck::Rig;

    const INPULSE: &str = "DJControl Inpulse 200 Mk3";
    const CW: [u8; 3] = [0xB0, 0x01, 0x01];
    const CCW: [u8; 3] = [0xB0, 0x01, 0x7F];

    struct Wheel {
        rig: Rig,
        r: djrust::host::deck::Renderer,
        now: f64,
    }

    impl Wheel {
        fn new(deterministic: bool) -> Wheel {
            let mut c = djrust::host::deck::EngineConfig::new(RATE);
            c.extended = true;
            let (mut e, r) = djrust::host::deck::engine(c);
            e.deterministic = deterministic;
            e.load_demo("A");
            e.load_demo("B");
            let mut rig = Rig::new(e);
            assert!(rig.attach(INPULSE));
            Wheel { rig, r, now: 0.0 }
        }
        fn click(&mut self, bytes: [u8; 3]) {
            self.now += 0.01;
            self.rig.route(INPULSE, &bytes, self.now).unwrap();
        }
        fn net(&mut self, deck: &str, seed: f64, structure: Option<&str>) {
            assert!(self
                .rig
                .engine
                .generate(deck, &TrackSpec::new("techno", seed, structure)));
        }
        fn spec(&self, deck: &str) -> Option<TrackSpec> {
            self.rig.engine.deck(deck).unwrap().spec.clone()
        }
    }

    // BrowserSeedTests.eachClickMovesThePlayingLoopOneSeed
    #[test]
    fn each_click_moves_the_playing_loop_one_seed() {
        let mut w = Wheel::new(true);
        w.net("A", 5.0, None);
        w.rig.engine.set_playing("A", true);
        w.click(CW);
        assert_eq!(w.spec("A"), Some(TrackSpec::new("techno", 6.0, None)));
        w.click(CW);
        w.click(CCW);
        w.click(CCW);
        w.click(CCW);
        assert_eq!(w.spec("A").unwrap().seed, 4.0);
        let d = w.rig.engine.deck("A").unwrap();
        assert!(d.playing && d.is_net_loop());
        // the deck plays seed 4 as its pass plays it (`.variety` moves a loop's key, so not the
        // composed project itself)
        let four = TrackSpec::new("techno", 4.0, None);
        let fixes = w.rig.engine.deck("A").unwrap().wave_options().musicality;
        let played = djrust::musicality::apply(&four.project(), &four, fixes);
        assert_eq!(d.project.as_ref().unwrap().model_cid(), played.model_cid());
        let _ = &mut w.r;
    }

    // BrowserSeedTests.onlyAPlayingComposedNetMoves
    #[test]
    fn only_a_playing_composed_net_moves() {
        let mut w = Wheel::new(true);
        w.net("A", 5.0, None); // stopped
        w.click(CW);
        assert_eq!(w.spec("A").unwrap().seed, 5.0, "not playing");
        w.rig.engine.set_playing("B", true); // a sample deck
        w.click(CW);
        assert!(!w.rig.engine.deck("B").unwrap().is_net());
        assert_eq!(w.spec("A").unwrap().seed, 5.0);
        // a song moves too (Wander's steering), keeping its structure
        w.net("A", 5.0, Some("minimal"));
        w.rig.engine.set_playing("A", true);
        w.click(CW);
        assert_eq!(
            w.spec("A"),
            Some(TrackSpec::new("techno", 6.0, Some("minimal")))
        );
    }

    // BrowserSeedTests.theFocusDeckWinsWhenTwoLoopsPlay
    #[test]
    fn the_focus_deck_wins_when_two_loops_play() {
        let mut w = Wheel::new(true);
        w.net("A", 1.0, None);
        w.net("B", 10.0, None);
        w.rig.engine.set_playing("A", true);
        w.rig.engine.set_playing("B", true);
        w.rig.engine.focus = "B".into();
        w.click(CW);
        assert_eq!(w.spec("B").unwrap().seed, 11.0);
        assert_eq!(w.spec("A").unwrap().seed, 1.0);
        // one loop playing: it moves whatever the focus, and takes the focus
        w.rig.engine.set_playing("B", false);
        w.click(CW);
        assert_eq!(w.spec("A").unwrap().seed, 2.0);
        assert_eq!(w.rig.engine.focus, "A");
    }

    // BrowserSeedTests.aFastTurnCountsEveryClickWhileItComposes
    #[test]
    fn a_fast_turn_counts_every_click_while_it_composes() {
        let mut w = Wheel::new(false);
        w.net("A", 20.0, None);
        w.rig.engine.set_playing("A", true);
        for _ in 0..3 {
            w.click(CW);
        }
        assert_eq!(
            w.rig
                .engine
                .deck("A")
                .unwrap()
                .track
                .seed_target
                .as_ref()
                .map(|s| s.seed),
            Some(23.0),
            "the target counts every click"
        );
        let mut waited = 0;
        while w.spec("A").unwrap().seed != 23.0 {
            w.rig.engine.service();
            std::thread::sleep(std::time::Duration::from_millis(2));
            waited += 1;
            assert!(
                waited < 5_000,
                "three clicks land on seed 23, got {:?}",
                w.spec("A")
            );
        }
        assert!(w.rig.engine.deck("A").unwrap().track.seed_target.is_none());
    }

    // SurfaceController `.seedStep`: a song keeps the transport going (the new seed comes in at
    // the bar after the swap); a loop restarts from its top.
    #[test]
    fn a_song_swaps_at_the_next_bar_and_a_loop_from_its_top() {
        let mut w = Wheel::new(true);
        w.net("A", 5.0, Some("standard"));
        w.rig.engine.set_playing("A", true);
        let mut st = 0;
        while w.rig.engine.deck("A").unwrap().mirror.tick() < 20 && st < 4000 {
            w.r.render_block(&mut vec![0.0; 128], &mut vec![0.0; 128]);
            st += 1;
        }
        let tick = w.rig.engine.deck("A").unwrap().mirror.tick();
        w.click(CW);
        let l = w
            .rig
            .engine
            .deck("A")
            .unwrap()
            .track
            .latest
            .clone()
            .unwrap();
        assert_eq!(l.start_tick, SwapStart::NextBar.tick(tick));
        w.net("B", 5.0, None);
        w.rig.engine.set_playing("B", true);
        w.rig.engine.focus = "B".into();
        w.click(CW);
        assert_eq!(
            w.rig
                .engine
                .deck("B")
                .unwrap()
                .track
                .latest
                .as_ref()
                .unwrap()
                .start_tick,
            0
        );
    }
}

// DeckGenerator.swift `stepSeed`: `spec.seed = max(1, spec.seed + Double(delta))` (the Generate
// panel's range starts at 1); a delta of 0, and a deck that holds no composed net, do nothing.
#[test]
fn the_seed_wheel_stops_at_one() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 2.0, None)));
    assert_eq!(st.e.step_seed("A", -1).map(|s| s.seed), Some(1.0));
    assert_eq!(st.e.step_seed("A", -5).map(|s| s.seed), Some(1.0));
    assert_eq!(spec_of(&st, "A").unwrap().seed, 1.0);
    assert_eq!(st.e.step_seed("A", 3).map(|s| s.seed), Some(4.0));
    assert!(st.e.step_seed("A", 0).is_none());
    assert!(st.e.step_seed("B", 1).is_none(), "a sample deck");
    assert_eq!(spec_of(&st, "A").unwrap().seed, 4.0);
}

// MARK: relocation (FixesUnderJumpsTests, MixOutTailTests, RestScheduleTests)

// FixesUnderJumpsTests.sectionJumpsKeepEveryFix, over what this runner measures: a jump into
// each of the first three sections is the same song (its serial) at the section's start tick,
// prepared with the figures the song was measured with.
#[test]
fn section_jumps_keep_the_song_and_what_it_measured() {
    for genre in ["techno", "lofi", "jazz", "dnb", "house", "edm"] {
        let mut st = Stepper::new(RATE, true);
        assert!(st
            .e
            .generate("A", &TrackSpec::new(genre, 42.0, Some("standard"))));
        let song = latest(&st, "A");
        let secs = st.e.deck("A").unwrap().sections.clone();
        assert!(secs.len() >= 3, "{genre}");
        for (index, sec) in secs.iter().enumerate().take(3) {
            assert!(st.e.jump_to_section("A", index), "{genre} section {index}");
            let jumped = latest(&st, "A");
            assert_eq!(jumped.start_tick, sec.start_tick, "{genre} {index}");
            assert_eq!(jumped.track_serial, song.track_serial, "{genre} {index}");
            assert!(jumped.generation > song.generation);
            assert_eq!(jumped.gain, song.gain, "{genre} {index}");
            assert_eq!(jumped.energy, song.energy, "{genre} {index}");
            assert_eq!(jumped.duck_depth, song.duck_depth, "{genre} {index}");
            assert_eq!(jumped.space, song.space, "{genre} {index}");
            assert_eq!(jumped.echo, song.echo, "{genre} {index}");
            assert_eq!(jumped.balance, song.balance, "{genre} {index}");
            assert_eq!(jumped.song_rests, song.song_rests, "{genre} {index}");
            assert_eq!(jumped.feel, song.feel, "{genre} {index}");
            assert_eq!(jumped.voice_design, song.voice_design, "{genre} {index}");
        }
    }
}

// MixOutTailTests.MixOutTailDeckTests.sameSongOptionsKeepTheTail, on the deck: a section jump
// keeps the tail the song plays, and a new track drops it.
#[test]
fn a_jump_keeps_the_tail_and_a_new_track_drops_it() {
    let mut st = Stepper::new(RATE, true);
    assert!(st
        .e
        .generate("A", &TrackSpec::new("edm", 42.0, Some("standard"))));
    assert!(latest(&st, "A").tail.is_none());
    let tail = MixOutTail::new(128, 16);
    assert!(st.e.relocate("A", 0, Some(tail), None));
    let tailed = latest(&st, "A");
    assert_eq!(tailed.tail, Some(tail));
    assert_eq!(tailed.rests, tailed.song_rests.with_tail(&tail));
    assert!(st.e.jump_to_section("A", 1));
    let after = latest(&st, "A");
    assert_eq!(after.tail, Some(tail), "kept through a jump");
    assert_eq!(after.song_rests, tailed.song_rests);
    assert_eq!(after.track_serial, tailed.track_serial);
    assert!(st
        .e
        .generate("A", &TrackSpec::new("edm", 43.0, Some("standard"))));
    assert!(latest(&st, "A").tail.is_none(), "a new track drops it");
    assert_ne!(latest(&st, "A").track_serial, tailed.track_serial);
}

// DeckPerform.relocate, off the main thread: prepared on a worker, landed by `service`, refused
// when a newer request (a `generate`) superseded it.
#[test]
fn a_relocation_in_the_background_lands_unless_superseded() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &song()));
    let song0 = latest(&st, "A");
    st.e.deterministic = false;
    assert!(st.e.relocate("A", 64, None, None));
    assert_eq!(
        latest(&st, "A").generation,
        song0.generation,
        "not loaded yet"
    );
    st.e.wait_composes("A");
    let moved = latest(&st, "A");
    assert_eq!(moved.start_tick, 64);
    assert_eq!(moved.track_serial, song0.track_serial);
    // superseded by a newer generate
    assert!(st.e.relocate("A", 32, None, None));
    assert!(st.e.generate("A", &next()));
    st.e.wait_composes("A");
    assert_eq!(spec_of(&st, "A"), Some(next()));
    assert_eq!(
        latest(&st, "A").start_tick,
        0,
        "the relocation did not land over it"
    );
    // service() lands one that finished
    assert!(st.e.relocate("A", 48, None, None));
    let mut waited = 0;
    while latest(&st, "A").start_tick != 48 {
        st.e.service();
        std::thread::sleep(std::time::Duration::from_millis(2));
        waited += 1;
        assert!(waited < 5_000, "the relocation never landed");
    }
    // only a net relocates
    assert!(!st.e.relocate("B", 0, None, None));
}

// MARK: mixOut / mixIn (MixOutActionTests)

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for &(step, note) in notes {
        n.bind(step, note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

/// `tailProject()`: eight bars (four of verse, four of outro) of kick, snare, bass and a
/// melody on channel 1 that hits every bar's first step, ending in `struct-stop`; 120 BPM.
fn tail_project() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert(
        "kick",
        ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
    );
    p.nets
        .insert("snare", ring("drums", 10, &[(4, 38), (12, 38)], 60));
    p.nets.insert(
        "bass",
        ring("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
    );
    p.nets.insert(
        "melody",
        ring(
            "melody",
            1,
            &[(0, 69), (3, 69), (6, 69), (8, 69), (11, 69), (14, 69)],
            100,
        ),
    );
    p.structure = ["verse", "outro"]
        .iter()
        .map(|n| Section {
            name: (*n).into(),
            steps: 64,
            phrases: Default::default(),
        })
        .collect();
    let mut ctrl = BTreeMap::new();
    ctrl.insert(127, ControlBinding::new("stop-transport", ""));
    p.nets.insert(
        "struct-stop",
        build_control_bundle(128, &ctrl, true, "p", "t"),
    );
    p
}

/// A playing deck on `tail_project()` at 48 kHz, replaying inline (`deterministic`).
fn tail_deck() -> Stepper {
    let mut st = Stepper::new(48_000.0, false);
    assert!(st.e.load("A", &tail_project(), 0.0));
    st.e.deck_mut("A").unwrap();
    st.r.deck_mut("A").unwrap().runner.record_events = true;
    st.e.play("A");
    st
}

fn played_step(f: &djrust::wave::FireEvent) -> i64 {
    f.tick as i64 - 1
}

// MixOutActionTests.silenceLandsSixteenBarsAfterTheJump (the tail it makes)
#[test]
fn a_mix_out_in_the_verse_jumps_to_the_outro() {
    let mut st = tail_deck();
    st.run_to_tick("A", 20);
    let t = st.tick("A");
    assert!((20..31).contains(&t), "in the verse's second bar");
    let before = latest(&st, "A").generation;
    let tail = st.e.mix_out("A", 16).expect("a net");
    assert_eq!(tail, MixOutTail::new(64, 16), "from the outro");
    assert_eq!(latest(&st, "A").tail, Some(tail));
    assert!(latest(&st, "A").generation > before);
    assert!(!st.e.is_mixing_out("A"), "it swaps in at the next bar");
    st.run_to_tick("A", 66);
    assert!(st.e.is_mixing_out("A"));
    assert_eq!(
        st.e.deck("A").unwrap().mirror.tail_end_tick(),
        Some(tail.end_tick())
    );
    // only the drums-forward lanes in the tail: no melody fire from the outro on
    let fires = &st.r.deck("A").unwrap().runner.fires;
    let tailed: Vec<_> = fires.iter().filter(|f| f.generation != before).collect();
    assert!(tailed.iter().any(|f| f.channel == 10));
    assert!(tailed
        .iter()
        .filter(|f| f.channel == 1)
        .all(|f| played_step(f) < tail.from_tick));
}

// MixOutActionTests.aMixOutInTheOutroPlaysOnFromTheNextBar
#[test]
fn a_mix_out_in_the_outro_plays_on_from_the_next_bar() {
    let mut st = tail_deck();
    st.run_to_tick("A", 70);
    assert_eq!(st.e.mix_out("A", 2), Some(MixOutTail::new(80, 2)));
    assert_eq!(latest(&st, "A").tail, Some(MixOutTail::new(80, 2)));
}

// DeckPerform.mixOutPoint (AutoMixTests' mix-out ahead): bar h's line, or the outro if it is ahead
#[test]
fn the_mix_out_point_is_the_outro_or_the_bar_line() {
    let mut st = tail_deck();
    // the verse is four bars: a mix-out for the bar-2 line prepares from the outro
    assert_eq!(
        st.e.mix_out_point("A", 2, 4),
        Some((64, MixOutTail::new(64, 4)))
    );
    // from the outro's own bar line it prepares from where the line is
    assert_eq!(
        st.e.mix_out_point("A", 6, 4),
        Some((6 * 16 - 1, MixOutTail::new(96, 4)))
    );
    st.run_to_tick("A", 1);
}

// MixOutActionTests.theFirstMelodicNoteLandsNBarsAfterTheActionBar
#[test]
fn the_first_melodic_note_lands_n_bars_after_the_action_bar() {
    let mut st = tail_deck();
    st.run_to_tick("A", 20);
    let before = latest(&st, "A").generation;
    let action_bar = 32;
    let bars = 1;
    let rest = st.e.mix_in("A", bars).expect("a net");
    assert_eq!(rest, Entry::new(32, 48, MixOutTail::REST_MASK));
    assert!(st.e.is_mixing_in("A"));
    assert_eq!(
        latest(&st, "A").rests.mask(action_bar),
        MixOutTail::REST_MASK
    );
    st.run_to_tick("A", 60);
    let fires = &st.r.deck("A").unwrap().runner.fires;
    let after: Vec<_> = fires.iter().filter(|f| f.generation != before).collect();
    let mut melody: Vec<i64> = after
        .iter()
        .filter(|f| f.channel == 1)
        .map(|f| played_step(f))
        .collect();
    melody.sort_unstable();
    let first = melody
        .iter()
        .copied()
        .find(|s| *s >= action_bar)
        .expect("a melodic note");
    assert_eq!(
        first,
        action_bar + bars * 16,
        "the first melodic note is 1 bar after the bar"
    );
    assert!(!melody
        .iter()
        .any(|s| *s >= action_bar && *s < action_bar + bars * 16));
    let drums: Vec<i64> = after
        .iter()
        .filter(|f| f.channel == 10)
        .map(|f| played_step(f))
        .collect();
    assert!(
        drums.contains(&action_bar) && drums.contains(&(action_bar + 16)),
        "drums play through"
    );
    assert!(!st.e.is_mixing_in("A"), "over");
    // the reference: the same song without the action hits those bar lines
    let mut r = tail_deck();
    r.run_to_tick("A", 60);
    let refm: Vec<i64> =
        r.r.deck("A")
            .unwrap()
            .runner
            .fires
            .iter()
            .filter(|f| f.channel == 1)
            .map(played_step)
            .collect();
    assert!(refm.contains(&action_bar) && refm.contains(&(action_bar + 16)));
}

// MixOutActionTests.aSampleDeckIgnoresThem, and a mix-in of no bars is nothing
#[test]
fn a_sample_deck_ignores_them() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.load_demo("A"));
    assert!(st.e.mix_out("A", 16).is_none() && st.e.mix_in("A", 2).is_none());
    assert!(!st.e.is_mixing_out("A") && !st.e.is_mixing_in("A"));
    let mut st = tail_deck();
    assert!(st.e.mix_in("A", 0).is_none());
    assert!(st.e.mix_out("B", 4).is_none(), "an empty deck has no net");
}

// A new track lets go of a mix-in (`mixInEndTick = nil`), a relocation keeps it.
#[test]
fn a_new_track_clears_the_mix_in_and_a_relocation_keeps_it() {
    let mut st = tail_deck();
    st.run_to_tick("A", 20);
    st.e.mix_in("A", 2);
    assert_eq!(st.e.deck("A").unwrap().track.mix_in_end_tick, Some(64));
    assert!(st.e.jump_to_section("A", 0));
    assert_eq!(st.e.deck("A").unwrap().track.mix_in_end_tick, Some(64));
    assert!(st.e.generate("A", &song()));
    assert_eq!(st.e.deck("A").unwrap().track.mix_in_end_tick, None);
}

// DeckNetTests.loadingAudioLeavesNetMode, and `Deck.isDemo`
#[test]
fn loading_audio_leaves_net_mode() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 1.0, None)));
    assert!(!st.e.is_demo("A"));
    st.e.play("A");
    assert!(st.e.load_demo("A"));
    let d = st.e.deck("A").unwrap();
    assert!(!d.is_net() && d.project.is_none() && !d.playing);
    assert!(d.track.fixes.is_none() && !d.track.pass_applied);
    assert!(st.e.is_demo("A"));
    st.e.play("A");
    st.step(200);
    assert!(st.take_peak() > 0.1);
    assert!(!st.r.deck("A").unwrap().runner.playing());
    // the demo loop is the one flag that says "nothing of the user's": another sample is not it
    let buffer = djrust::deck::turntable::SampleBuffer::new(vec![0.0; 4410], None, RATE);
    let overview = djrust::deck::waveform::WaveformOverview::new(
        &buffer,
        djrust::host::deck::sample::OVERVIEW_COLUMNS,
    );
    assert!(st.e.load_sample(
        "A",
        djrust::host::deck::sample::PreparedSample {
            title: "blank".into(),
            buffer: std::sync::Arc::new(buffer),
            overview,
            grid: None,
        }
    ));
    assert!(!st.e.is_demo("A"));
    assert!(!st.e.is_demo("B"), "nothing loaded");
}

// AutoMixTests.theMixOutAheadIsTheHandMixOut: the mix-out auto-mix prepares a phrase early
// (`move_request(...).prepare()`, on any thread) is the project the hand's `mix_out` makes in
// bar H - 1: same start, tail, rests, measurements and song.
#[test]
fn a_mix_out_prepared_ahead_is_the_hand_mix_out() {
    let mut st = playing(0);
    let h = 4;
    let (tick, tail) = st.e.mix_out_point("A", h, 4).expect("a deck");
    let req =
        st.e.move_request("A", tick, Some(tail), None)
            .expect("a net");
    let ahead = std::thread::spawn(move || req.prepare()).join().unwrap();
    // the hand's, in bar H - 1
    st.run_to_tick("A", (h - 1) * 16 + 2);
    assert!(st.tick("A") < h * 16);
    let tailed = st.e.mix_out("A", 4).unwrap();
    assert_eq!(tailed, tail);
    let hand = latest(&st, "A");
    let a = &ahead.latest;
    assert_eq!(a.start_tick, hand.start_tick);
    assert_eq!(a.tail, hand.tail);
    assert_eq!(a.rests, hand.rests);
    assert_eq!(a.song_rests, hand.song_rests);
    assert_eq!(a.gain, hand.gain);
    assert_eq!(a.track_serial, hand.track_serial, "the same song");
    assert_eq!(a.energy, hand.energy);
    assert_eq!(a.duck_depth, hand.duck_depth);
    // and the ahead one loads as a relocation does: the song's, nothing new
    let before = latest(&st, "A");
    assert!(st.e.load_move("A", ahead));
    let after = latest(&st, "A");
    assert_ne!(after.generation, before.generation, "a project of its own");
    assert_eq!(after.track_serial, before.track_serial);
    assert_eq!(after.tail, before.tail);
}
