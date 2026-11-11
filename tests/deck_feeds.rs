//! What a deck's display reads: the marking mirror wired end to end (`WaveProject.marking`,
//! `WaveRunner::publish_marking`, the deck's book of mirrors, `readMarking` / `markingLayout`),
//! `Deck.macroActivity`, the per-macro lamp of `SurfaceNetTests.apcPadsAreBeatsMacroGridInGroupColours`,
//! the always-on loudness meter (`shortTermLUFS` / `momentaryLUFS`) and `Deck.advances`.
//!
//! Parity form (a): each test names the Swift test whose facts it asserts
//! (`MarkingMirrorTests`, `MarkingLayoutIdentityTests`, `MacroPulseTests`, `RingSpinTests`,
//! `DeckLoudnessTests`). The deck-level Swift output (form b) is in `deck_golden.rs`: the
//! `marking-swap-at-the-bar` and `marking-seeded-launch` cases hold `readMarking` and both layouts
//! to Swift's for every block.

mod common_track;

use std::sync::atomic::Ordering;

use common_track::Stepper;
use djrust::control::controller::DeckView;
use djrust::deck::timeline::SectionInfo;
use djrust::wave::{MarkingSnapshot, NetMacro};
use djrust::TrackSpec;

const RATE: f64 = 22_050.0;

fn standard(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, Some("standard"))
}

/// The loop form of a track: every role plays from the first bar (the macro tests need them on).
fn looped(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, None)
}

fn playing(spec: &TrackSpec) -> Stepper {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", spec));
    st.e.set_tempo("A", 600.0);
    st.e.play("A");
    st
}

// MarkingMirrorTests.nothingLoadedMeansNoSnapshot
#[test]
fn nothing_loaded_means_no_snapshot_and_no_layout() {
    let st = Stepper::new(RATE, true);
    let mut snap = MarkingSnapshot::new();
    assert!(!st.e.read_marking("A", &mut snap));
    assert!(st.e.marking_layout("A").is_none());
    assert!(st.e.marking_layout_of("A", 1).is_none());
    assert!(!st.e.read_marking_of("A", 1, &mut snap));
}

// MarkingMirrorTests.aFreshProjectIsReadableBeforeAnythingRenders
#[test]
fn a_fresh_project_is_readable_before_anything_renders() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.generate("A", &standard("techno", 42.0)));
    let generation = st.e.deck("A").unwrap().track.latest.as_ref().unwrap().generation;
    let mut snap = MarkingSnapshot::new();
    assert_eq!(
        st.e.deck("A").unwrap().mirror.generation.load(Ordering::Relaxed),
        0,
        "the render thread has not run it"
    );
    assert!(st.e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), generation);
    assert_eq!(snap.header.sequence, 1);
    assert!(snap.tick() == 0 && snap.header.frame == 0);
    assert!(
        snap.tokens.iter().any(|&t| t > 0.0),
        "the initial marking has tokens in it"
    );
    // after a block it is the project the render thread runs
    st.step(4);
    assert!(st.e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), generation);
    assert_eq!(
        st.e.deck("A").unwrap().mirror.generation.load(Ordering::Relaxed),
        generation
    );
}

// MarkingMirrorTests.aSeekedProjectIsSeededWithItsRebuiltMarking: a section launch builds a new
// project seeded at the section's tick, with the song's own serial
#[test]
fn a_launched_section_is_seeded_with_its_rebuilt_marking_and_keeps_its_song() {
    let mut st = playing(&standard("techno", 42.0));
    st.step(40);
    let first = st.e.marking_layout("A").unwrap();
    let section = st.e.deck("A").unwrap().sections[2].start_tick;
    assert!(st.e.jump_to_section("A", 2));
    let launched = st.e.marking_layout("A").unwrap();
    assert!(launched.generation > first.generation);
    assert_eq!(launched.start_tick, section);
    assert_eq!(
        launched.track_serial, first.track_serial,
        "the same song: a section launch keeps its serial"
    );
    // the book still has the first project's layout (an event stamped with it resolves)
    assert!(st.e.marking_layout_of("A", first.generation).is_some());
    // a stopped-then-played deck shows the launched project's marking at its tick
    let mut snap = MarkingSnapshot::new();
    st.step(2);
    assert!(st.e.read_marking("A", &mut snap));
    assert!(snap.layout.is_some());
}

// MarkingLayoutIdentityTests.aDecksSectionsFlipAtLoadButTheSoundingLayoutKeepsItsOwnUntilTheBar
#[test]
fn a_decks_sections_flip_at_load_but_the_sounding_layout_keeps_its_own_until_the_bar() {
    let mut st = playing(&standard("techno", 42.0));
    st.step(64); // 8192 frames
    let first = st.e.deck("A").unwrap().track.latest.clone().unwrap();
    let first_layout = st.e.marking_layout_of("A", first.generation).unwrap();
    // queued behind the playing song
    assert!(st.e.generate("A", &TrackSpec::new("ambient", 5.0, Some("short"))));
    let deck_sections = st.e.deck("A").unwrap().sections.clone();
    assert_ne!(
        deck_sections, first_layout.sections,
        "the deck already shows the new song"
    );
    let active = st.e.deck("A").unwrap().mirror.generation.load(Ordering::Relaxed);
    let sounding = st.e.marking_layout_of("A", active).unwrap();
    assert_eq!(sounding.generation, first.generation);
    assert_eq!(
        sounding.sections, first_layout.sections,
        "the sounding one is still the first"
    );
    st.step(64 * 4); // past the bar
    let latest = st.e.deck("A").unwrap().track.latest.clone().unwrap();
    let active = st.e.deck("A").unwrap().mirror.generation.load(Ordering::Relaxed);
    assert_eq!(active, latest.generation);
    let now = st.e.marking_layout_of("A", active).unwrap();
    assert_eq!(now.sections, st.e.deck("A").unwrap().sections);
    assert_eq!(
        now.sections,
        SectionInfo::sections_of(st.e.deck("A").unwrap().project.as_ref().unwrap())
    );
}

// WaveDeckSource.readMarking: a playing deck reports the old project until it hands over at the
// bar, and a deck that is not playing the project loaded last
#[test]
fn a_playing_deck_reports_the_old_marking_until_the_bar_and_a_paused_one_the_new() {
    let mut st = playing(&standard("techno", 42.0));
    st.step(40);
    let first = st.e.deck("A").unwrap().track.latest.clone().unwrap().generation;
    assert!(st.e.generate("A", &standard("house", 4.0)));
    let second = st.e.deck("A").unwrap().track.latest.clone().unwrap().generation;
    let mut snap = MarkingSnapshot::new();
    assert!(st.e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), first, "the old project sounds until the bar");
    assert!(st.e.read_marking_of("A", second, &mut snap) && snap.generation() == second);
    st.step(64 * 4);
    assert!(st.e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), second, "handed over at the bar");
    // paused: the next load is the one to show (it takes over at the next block)
    st.e.pause("A");
    st.step(2);
    assert!(st.e.generate("A", &standard("dnb", 3.0)));
    let third = st.e.deck("A").unwrap().track.latest.clone().unwrap().generation;
    assert!(st.e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), third);
}

// the book keeps eight mirrors, never the one the render thread runs or the newest
// (WaveDeckSource.mirrorsKept)
#[test]
fn the_deck_keeps_the_layouts_of_its_last_eight_projects() {
    let mut st = Stepper::new(RATE, true);
    let mut generations = Vec::new();
    for seed in 0..12 {
        assert!(st.e.generate("A", &TrackSpec::new("techno", 40.0 + seed as f64, None)));
        generations.push(st.e.deck("A").unwrap().track.latest.clone().unwrap().generation);
    }
    let kept = generations
        .iter()
        .filter(|&&g| st.e.marking_layout_of("A", g).is_some())
        .count();
    assert_eq!(kept, 8);
    assert!(st.e.marking_layout_of("A", generations[0]).is_none());
    assert_eq!(
        st.e.marking_layout("A").unwrap().generation,
        *generations.last().unwrap()
    );
}

// MacroPulseTests.twoOverlappingMacrosEachChaseOnlyTheirOwnRoles
#[test]
fn two_overlapping_macros_each_chase_only_their_own_roles() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(86); // half a second
    let drums = ["kick", "snare", "hihat", "clap"];
    let roles = st.e.roles("A");
    let on: Vec<String> = roles
        .iter()
        .filter(|r| st.e.is_role_audible("A", r))
        .cloned()
        .collect();
    st.e.fire("A", NetMacro::Drop, None); // holds the non-drums...
    st.e.fire("A", NetMacro::Cut, None); // ...and the cut finds only the drums still sounding
    st.step(4);
    let activity = st.e.macro_activity("A");
    let drop = activity.targets.get(&NetMacro::Drop).cloned().unwrap_or_default();
    let cut = activity.targets.get(&NetMacro::Cut).cloned().unwrap_or_default();
    let want_drop: Vec<&String> = on.iter().filter(|r| !drums.contains(&r.as_str())).collect();
    let want_cut: Vec<&String> = on.iter().filter(|r| drums.contains(&r.as_str())).collect();
    assert!(!drop.is_empty() && !cut.is_empty(), "drop {drop:?}, cut {cut:?}");
    assert_eq!(drop.iter().collect::<Vec<_>>(), want_drop, "drop");
    assert_eq!(cut.iter().collect::<Vec<_>>(), want_cut, "cut");
    assert!(drop.iter().all(|r| !cut.contains(r)), "no role is chased twice");
    assert_eq!(activity.running, vec![NetMacro::Drop, NetMacro::Cut]);
}

// MacroPulseTests.aBreakdownChasesTheDrumsAndPanicEndsIt
#[test]
fn a_breakdown_chases_the_drums_and_panic_ends_it() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(86);
    let drums: Vec<String> = st
        .e
        .roles("A")
        .into_iter()
        .filter(|r| ["kick", "snare", "hihat", "clap"].contains(&r.as_str()))
        .filter(|r| st.e.is_role_audible("A", r))
        .collect();
    assert!(!drums.is_empty());
    st.e.fire("A", NetMacro::Breakdown, None);
    st.step(4);
    let activity = st.e.macro_activity("A");
    assert_eq!(activity.running, vec![NetMacro::Breakdown]);
    assert_eq!(activity.targets.get(&NetMacro::Breakdown), Some(&drums));
    st.e.panic("A");
    st.step(4);
    assert_eq!(
        st.e.macro_activity("A"),
        djrust::control::chase::MacroActivity::idle(),
        "panic restores everything"
    );
}

// MacroPulseTests.theBeatRepeatAndATempoMacroShowRunningWithoutAChase
#[test]
fn the_beat_repeat_and_a_tempo_macro_show_running_without_a_chase() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(40);
    st.e.fire("A", NetMacro::BeatRepeat, None);
    st.e.fire("A", NetMacro::HalfTime, None);
    st.step(4);
    let a = st.e.macro_activity("A");
    assert!(a.running.contains(&NetMacro::BeatRepeat) && a.running.contains(&NetMacro::HalfTime));
    assert!(a.targets.is_empty(), "neither chases a role: {:?}", a.targets);
}

// a sample deck, and a deck with nothing on it, are idle (`guard mode == .net`)
#[test]
fn a_sample_deck_has_no_macro_activity() {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.load_demo("B"));
    st.e.play("B");
    st.step(8);
    assert_eq!(
        st.e.macro_activity("B"),
        djrust::control::chase::MacroActivity::idle()
    );
    assert_eq!(st.e.macro_activity("A"), djrust::control::chase::MacroActivity::idle());
}

// SurfaceNetTests.apcPadsAreBeatsMacroGridInGroupColours: the pad is bright while its own macro
// runs (`d.wave.activeMacros.contains(m)`), not while any macro does
#[test]
fn a_macro_pad_is_lit_for_its_own_macro_only() {
    let mut st = playing(&looped("techno", 42.0));
    st.step(40);
    st.e.fire("A", NetMacro::Drop, None);
    st.step(4);
    let d = st.e.deck("A").unwrap();
    assert!(d.net_macro_active("drop"), "drop: bright while it runs");
    assert!(!d.net_macro_active("cut"), "cut is not running");
    assert!(!d.net_macro_active("half-time"));
    // an id that names no macro asks whether anything runs (what the app's lamp asks)
    assert!(d.net_macro_active(""));
    st.e.panic("A");
    st.step(4);
    let d = st.e.deck("A").unwrap();
    assert!(!d.net_macro_active("drop") && !d.net_macro_active(""));
}

// DeckRenderer.meter: always on, before the EQ and the fader (`Deck.shortTermLUFS`)
#[test]
fn the_loudness_meter_hears_the_deck_before_the_strip() {
    let mut st = playing(&standard("techno", 42.0));
    st.e.set_volume("A", 0.0); // the fader is after the meter
    let m = st.e.deck("A").unwrap().mirror.clone();
    assert_eq!((m.short_term_lufs(), m.momentary_lufs()), (-200.0, -200.0), "silent: -200");
    st.step(260); // 1.5 s
    let (st_lufs, mo_lufs) = (m.short_term_lufs(), m.momentary_lufs());
    assert!(
        st_lufs > -45.0 && st_lufs < 0.0 && mo_lufs > -45.0 && mo_lufs < 0.0,
        "a playing techno track: short-term {st_lufs}, momentary {mo_lufs}"
    );
    assert_eq!(st.e.master_level(), 0.0, "the fader is down: the master is silent");
    // a deck that is paused falls silent again, 400 ms later for the momentary reading
    st.e.pause("A");
    st.step(260);
    st.step(260);
    assert!(m.momentary_lufs() < mo_lufs - 20.0, "{}", m.momentary_lufs());
}

// RingSpinTests.DeckAdvanceCountTests: `advances` counts hand-overs to the next track, not loads
#[test]
fn advances_counts_hand_overs_and_not_loads() {
    let mut st = Stepper::new(RATE, true);
    assert_eq!(st.e.advances("A"), 0);
    assert!(st.e.generate("A", &standard("techno", 42.0)));
    assert_eq!(st.e.advances("A"), 0, "a load is not a hand-over");
    st.e.queue_next("A", &standard("techno", 43.0));
    assert!(st.e.advance_to_next("A"));
    assert_eq!(st.e.advances("A"), 1);
    assert!(!st.e.advance_to_next("A"), "nothing waiting");
    assert_eq!(st.e.advances("A"), 1);
    assert_eq!(st.e.advances("B"), 0);
}
