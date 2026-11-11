//! The track pipeline's pure rules (`djrust::deck::swap`): where a track starts, which
//! options another start of the same song is prepared with, and the song serial a restart keeps.
//!
//! Parity form (a): each test names the Swift test whose literals it asserts
//! (`WanderSwapTimingTests`, `FixesUnderJumpsTests`, `MixOutTailTests`, `RestScheduleTests`,
//! `RunnerOnlyFixesTests`, `PaletteTests`). The pipeline built on them is held to Swift in
//! `crates/djhost/tests/deck_generate.rs` and by the `swap` golden.

use djrust::deck::swap::{
    moved_options, project_palette, restarted_options, same_song_options, song_length, start_for,
    Latest,
};
use djrust::musicality::echo::EchoSpec;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::rest_gestures::build_rests;
use djrust::musicality::space::SpaceSpec;
use djrust::wave::rests::{Entry, MixOutTail, RestSchedule};
use djrust::wave::{WaveOptions, WaveRunner};
use djrust::TrackSpec;

const SR: f64 = 22_050.0;

fn spec(genre: &str, seed: f64, structure: Option<&str>) -> TrackSpec {
    TrackSpec::new(genre, seed, structure)
}

/// What the app prepares with: the table, the limiting master and every fix.
fn all() -> WaveOptions {
    WaveOptions {
        musicality: Fixes::ALL,
        ..WaveOptions::extended()
    }
}

fn latest_of(o: &WaveOptions, project: &djrust::Project) -> Latest {
    let wp = WaveRunner::prepare(project, SR, o, 0);
    Latest::of(&wp, &o.voice_design)
}

// WanderSwapTimingTests.aRestartKeepsTheTrackAndItsMeasurements, the clamp: "past the song's
// end it starts at the top, as `init(spec:startTick:)` does"; DeckGenerator.swift
// `startTick > 0 && startTick < length ? startTick : 0`.
#[test]
fn a_track_starts_where_asked_only_inside_its_song() {
    assert_eq!(start_for(48, 320), 48);
    assert_eq!(start_for(1, 320), 1);
    assert_eq!(start_for(319, 320), 319);
    assert_eq!(start_for(0, 320), 0);
    assert_eq!(start_for(320, 320), 0, "at the end: the top");
    assert_eq!(start_for(10_000_000, 320), 0, "past it: the top");
    assert_eq!(start_for(-5, 320), 0);
    assert_eq!(
        start_for(40, 0),
        0,
        "a loop has no length: any tick is the top"
    );
}

// DeckNetTests.aSongThatEndsHandsOverToThePrecomposedNext: "the 320-tick song" (ambient:3:minimal).
#[test]
fn a_song_is_as_long_as_its_sections() {
    let p = spec("ambient", 3.0, Some("minimal")).project();
    assert_eq!(song_length(&p), 320);
    let sum: i64 = p.structure.iter().map(|s| s.steps).sum();
    assert_eq!(song_length(&p), sum);
    assert_eq!(
        song_length(&spec("techno", 1.0, None).project()),
        0,
        "a loop"
    );
}

// PaletteSound.swift `forPalette`: the palette's room and echo, else the genre's, for the fixes
// the options hold; "none" is dry.
#[test]
fn a_palette_brings_its_room_and_echo() {
    let base = all();
    // no palette: the genre's own
    let plain = base.for_genre("dnb").for_palette(None, "dnb");
    assert_eq!(plain.space, base.for_genre("dnb").space);
    assert_eq!(plain.echo, base.for_genre("dnb").echo);
    // chiptune plays in techno's room, with no echo (palettes.json hints)
    let chip = base.for_genre("dnb").for_palette(Some("chiptune"), "dnb");
    assert_eq!(chip.space, Some(SpaceSpec::for_genre("techno", true)));
    assert_eq!(chip.echo, None);
    // club: house's room, edm's echo; a recorded asset version is ignored
    let club = base.for_genre("lofi").for_palette(Some("club@7"), "lofi");
    assert_eq!(club.space, Some(SpaceSpec::for_genre("house", true)));
    assert_eq!(club.echo, EchoSpec::for_genre("edm"));
    // without the fixes nothing is set
    let bare = WaveOptions {
        musicality: Fixes::EMPTY,
        ..WaveOptions::extended()
    }
    .for_palette(Some("club"), "lofi");
    assert!(bare.space.is_none() && bare.echo.is_none());
    // an unknown palette is none
    let unknown = base.for_genre("dnb").for_palette(Some("nonesuch"), "dnb");
    assert_eq!(unknown.space, plain.space);
}

#[test]
fn a_projects_palette_is_the_first_playing_nets() {
    let mut p = spec("techno", 42.0, Some("minimal")).project();
    assert_eq!(project_palette(&p), None);
    let ids: Vec<String> = p.nets.keys().to_vec();
    for id in &ids {
        let nb = p.nets.get_mut(id).unwrap();
        if nb.role != "control" {
            nb.track.palette = Some("jazz".into());
        }
    }
    assert_eq!(project_palette(&p).as_deref(), Some("jazz"));
}

// FixesUnderJumpsTests.sameSongOptionsCarryTheGenre
#[test]
fn same_song_options_carry_the_genre() {
    for genre in ["jazz", "dnb", "techno", "lofi"] {
        let project = spec(genre, 42.0, Some("standard")).project();
        let base = all();
        let latest = latest_of(&base.for_genre(genre), &project);
        let o = same_song_options(&base, Some(Fixes::ALL), Some(genre), Some(&latest), None);
        let fresh = all().for_genre(genre);
        assert_eq!(o.genre.as_deref(), Some(genre));
        assert_eq!(o.sub_octave, fresh.sub_octave, "{genre}");
        assert_eq!(o.feel, fresh.feel, "{genre}");
        assert_eq!(o.voice_design, fresh.voice_design, "{genre}");
        assert_eq!(o.duck_depth, fresh.duck_depth, "{genre}");
        if genre == "dnb" {
            assert!(o.sub_octave);
        }
    }
}

// RunnerOnlyFixesTests.theDeckRecordsWhatItPlaysWith: "a section jump re-prepares with the
// deck's options: still the runner's only" / "`.all`".
#[test]
fn a_jump_plays_with_the_fixes_the_song_was_loaded_with() {
    let base = all();
    let o = same_song_options(&base, Some(Fixes::RUNNER_ONLY), None, None, None);
    assert_eq!(o.musicality, Fixes::RUNNER_ONLY);
    let o = same_song_options(&base, Some(Fixes::ALL), None, None, None);
    assert_eq!(o.musicality, Fixes::ALL);
    // a hand-built net has none recorded: the deck's own
    let o = same_song_options(&base, None, None, None, None);
    assert_eq!(o.musicality, Fixes::ALL);
    // a raw-`nets` share has no genre: what only `forGenre` sets stays as it was
    let o = same_song_options(&base, Some(Fixes::RUNNER_ONLY), None, None, None);
    assert_eq!(o.genre, None);
}

// MixOutTailTests.MixOutTailDeckTests.sameSongOptionsKeepTheTail
#[test]
fn same_song_options_keep_the_tail() {
    let song = spec("edm", 42.0, Some("standard")).project();
    let base = all();
    let first = latest_of(&base.for_genre("edm"), &song);
    let o = same_song_options(&base, Some(Fixes::ALL), Some("edm"), Some(&first), None);
    assert!(o.tail.is_none());
    let tail = MixOutTail::new(128, 16);
    let mut o = o;
    o.tail = Some(tail);
    let wp = WaveRunner::prepare(&song, SR, &o, 0);
    let tailed = Latest::of(&wp, &o.voice_design);
    assert_eq!(tailed.tail, Some(tail));
    assert_eq!(tailed.rests, tailed.song_rests.with_tail(&tail));
    let again = same_song_options(&base, Some(Fixes::ALL), Some("edm"), Some(&tailed), None);
    assert_eq!(again.tail, Some(tail));
    assert_eq!(again.rests, Some(tailed.song_rests.clone()));
    // a tail asked for wins
    let mut other = base.clone();
    other.tail = Some(MixOutTail::new(0, 1));
    let o = same_song_options(&other, Some(Fixes::ALL), Some("edm"), Some(&tailed), None);
    assert_eq!(o.tail, other.tail);
}

// RestScheduleTests.RestScheduleDeckTests.sectionJumpsKeepTheSongsRests: the jump carries the
// loaded project's schedule, and what the project measured (`sameSongOptions`: the loudness
// gain as `trackGain`, so the jump does not measure it again).
#[test]
fn same_song_options_carry_what_the_project_measured() {
    let song = spec("edm", 42.0, Some("standard")).project();
    let mut base = all().for_genre("edm");
    base.rests = Some(RestSchedule::empty().with_rest(Entry::new(32, 64, MixOutTail::REST_MASK)));
    let wp = WaveRunner::prepare(&song, SR, &base, 0);
    let latest = Latest::of(&wp, &base.voice_design);
    assert!(latest.energy.is_some() && latest.balance.is_some());
    assert_ne!(wp.gain, 1.0, "`.loudness` measured a gain at prepare");
    // (a different start of the same song, from the options of a deck that knows nothing yet)
    let o = same_song_options(&all(), Some(Fixes::ALL), Some("edm"), Some(&latest), None);
    assert_eq!(o.rests.as_ref(), Some(&wp.rests));
    assert_eq!(o.track_gain, Some(wp.gain));
    assert_eq!(o.track_energy, wp.energy);
    assert_eq!(o.track_balance, wp.balance);
    assert_eq!(o.duck_depth, wp.duck_depth);
    assert_eq!(o.space, wp.space);
    assert_eq!(o.echo, wp.echo);
    assert_eq!(o.feel, wp.feel);
    // and prepared with them it measures the same, from a later tick, as the same song
    let again = WaveRunner::prepare_serial(&song, SR, &o, 48, Some(wp.track_serial));
    assert_eq!(again.start_tick, 48);
    assert_eq!(again.track_serial, wp.track_serial);
    assert!(again.generation > wp.generation);
    assert_eq!(again.rests, wp.rests);
    assert_eq!(again.gain, wp.gain);
    assert_eq!(again.energy, wp.energy);
    assert_eq!(again.balance, wp.balance);
}

// MixOutTailTests / DeckPerform.movedOptions: a tail passed in wins, and trackRests = the song's
// with the rest joined.
#[test]
fn moved_options_bring_a_tail_and_a_rest() {
    let base = all();
    let tail = MixOutTail::new(64, 4);
    let rest = Entry::new(32, 64, MixOutTail::REST_MASK);
    let o = moved_options(&base, None, None, None, None, Some(tail), Some(rest));
    assert_eq!(o.tail, Some(tail));
    assert_eq!(o.extra_rest, Some(rest));
    let o = moved_options(&base, None, None, None, None, None, None);
    assert!(o.tail.is_none() && o.extra_rest.is_none());
    // the schedule the project plays is the song's own (`prepare` builds it when `trackRests` is
    // nil: `RestSchedule.build`, which this test used to find empty) with the rest joined
    // (`song.with(rest:)`)
    let song = spec("edm", 42.0, Some("standard")).project();
    let built = build_rests(&song, base.musicality, None);
    assert!(
        !built.is_empty(),
        "the song rests somewhere without the rest"
    );
    let wp = WaveRunner::prepare(
        &song,
        SR,
        &moved_options(&base, None, None, None, None, None, Some(rest)),
        0,
    );
    assert_eq!(wp.rests, built.with_rest(rest));
    assert_eq!(
        wp.rests.mask(32),
        built.mask(32).union(MixOutTail::REST_MASK)
    );
    assert_eq!(wp.rests.mask(31), built.mask(31));
    assert_eq!(wp.rests.mask(64), built.mask(64));
    // and `song_rests` is the song's with it: a later start carries the rest too
    assert_eq!(wp.song_rests, wp.rests);
}

// WanderSwapTimingTests.aRestartKeepsTheTrackAndItsMeasurements: the options a restart prepares
// with are what the first prepare measured.
#[test]
fn a_restart_keeps_the_track_and_its_measurements() {
    let song = spec("techno", 43.0, Some("standard")).project();
    let o = all().for_genre("techno");
    let first = WaveRunner::prepare(&song, SR, &o, 32);
    let r = restarted_options(&o, &first);
    assert_eq!(
        r.track_gain,
        Some(first.gain),
        "`trackGain = prepared.gain`"
    );
    let again = WaveRunner::prepare_serial(&song, SR, &r, 48, Some(first.track_serial));
    assert_eq!(again.start_tick, 48);
    assert_eq!(again.track_serial, first.track_serial, "the same song");
    assert_eq!(again.gain, first.gain);
    assert_eq!(again.energy, first.energy);
    assert_eq!(again.balance, first.balance);
    assert_eq!(again.rests, first.rests);
    assert_eq!(again.space, first.space);
    assert_eq!(again.echo, first.echo);
    assert_eq!(again.duck_depth, first.duck_depth);
    // a new prepare is a new song with a new serial
    let other = WaveRunner::prepare(&song, SR, &o, 0);
    assert_ne!(other.track_serial, first.track_serial);
    assert!(other.track_serial > first.track_serial);
}
