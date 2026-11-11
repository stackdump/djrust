//! What `WaveRunner::prepare` measures and builds from the fixes it is given, where a caller
//! gave it nothing: the track's gain (`.loudness`: `Loudness::gain` with the trims, the sampled
//! parts' levels, the sub-octave, the tilt and the energy arc's section gain) and the rest
//! schedule (`RestSchedule::build`: `.dropGap`, `.layerBudget`, `.bandStop`, `.kickOut`). Swift's
//! `RestScheduleTests.trackRestsReplaceTheBuild`, `LoudnessSpreadTests` (the per-family target
//! tests) and the prepare halves of `LoudnessRange` / `MusicalityTests.tracksPlayAtOneLoudness`
//! (PORTING.md's form (a)); the measured gains and schedules held against Swift's are the "rests"
//! cases of `tests/runner_golden.rs`.

mod common;
use common::measure::*;
use djrust::musicality::apply;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::loudness::Loudness;
use djrust::musicality::mix_balance::MixBalance;
use djrust::musicality::mix_targets::{MixTargets, REFERENCE_LUFS};
use djrust::musicality::rest_gestures::build_rests;
use djrust::pflow::{NetBundle, Project, Section};
use djrust::wave::rests::RestSchedule;
use djrust::wave::*;
use djrust::TrackSpec;
use std::collections::BTreeMap;

const SR: f64 = 48_000.0;

fn played(genre: &str, seed: f64) -> Project {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    apply(&spec.project(), &spec, Fixes::ALL)
}

fn limit(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Limit,
        musicality: fixes,
        ..WaveOptions::extended()
    }
}

// ---- the gain ----------------------------------------------------------------------------

/// Without `.loudness` the gain is 1 (beats as it is); a given gain is what plays, measured
/// nothing.
#[test]
fn the_gain_is_one_without_the_fix_and_the_given_one_with_it() {
    let p = played("techno", 42.0);
    assert_eq!(
        WaveRunner::prepare(&p, 22_050.0, &limit(Fixes::EMPTY), 0).gain,
        1.0
    );
    assert_eq!(
        WaveRunner::prepare(&p, 22_050.0, &limit(Fixes::ENERGY), 0).gain,
        1.0
    );
    let mut o = limit(Fixes::LOUDNESS);
    o.track_gain = Some(0.37);
    assert_eq!(WaveRunner::prepare(&p, 22_050.0, &o, 0).gain, 0.37);
    // given, with the fix off, it is what plays too (another start of the same song)
    let mut o = limit(Fixes::EMPTY);
    o.track_gain = Some(0.5);
    assert_eq!(WaveRunner::prepare(&p, 22_050.0, &o, 0).gain, 0.5);
}

/// `.loudness` measures the gain the project would be given by `Loudness::gain` with nothing
/// else on, and the same on another start of the song (it is a function of the project alone).
#[test]
fn prepare_measures_the_gain() {
    for genre in ["techno", "lofi", "ambient", "metal"] {
        let p = played(genre, 42.0);
        let want = Loudness::gain(
            &p,
            Fixes::LOUDNESS,
            None,
            &Default::default(),
            false,
            None,
            None,
            1.0,
        );
        let got = WaveRunner::prepare(&p, 22_050.0, &limit(Fixes::LOUDNESS), 0).gain;
        assert_eq!(got, want, "{genre}");
        assert!(got != 1.0, "{genre}");
        let again = WaveRunner::prepare(&p, 22_050.0, &limit(Fixes::LOUDNESS), 64).gain;
        assert_eq!(again, want, "{genre}: another start of the song");
    }
}

/// The genre's family target (`targetDB(genre:)`), the voice fixes the deck plays (the probe
/// hears `.subOctave` where the deck's genre plays it) and the energy arc's gain on the probed
/// section all move the gain `prepare` measures.
#[test]
fn the_gain_hears_the_deck_it_is_measured_for() {
    let p = played("dnb", 42.0);
    let fixes = Fixes::LOUDNESS | Fixes::ENERGY | Fixes::BALANCE | Fixes::SUB_OCTAVE;
    let o = limit(fixes).for_genre("dnb");
    assert!(o.sub_octave);
    let prepared = WaveRunner::prepare(&p, 22_050.0, &o, 0);
    // what Swift passes: the trims prepare measured, the lanes' probe levels, the deck's
    // sub-octave and tilt, the genre, and the energy arc at the busiest start
    let trims = MixBalance::trims(&p, fixes, MixBalance::family(Some("dnb")), None);
    let energy = prepared.energy.clone().expect("the arc");
    let section = energy.at(Loudness::busiest_start(&p)).0;
    let want = Loudness::gain(
        &p,
        fixes,
        Some(&trims),
        &prepared.probe_levels,
        true,
        o.hf_tilt,
        Some("dnb"),
        section,
    );
    assert_eq!(prepared.gain, want);
    // each of them is heard: leaving one out moves it
    let without_arc = Loudness::gain(
        &p,
        fixes,
        Some(&trims),
        &BTreeMap::new(),
        true,
        o.hf_tilt,
        Some("dnb"),
        1.0,
    );
    let without_sub = Loudness::gain(
        &p,
        fixes,
        Some(&trims),
        &BTreeMap::new(),
        false,
        o.hf_tilt,
        Some("dnb"),
        section,
    );
    assert!(
        (without_arc - want).abs() > 1e-9 || (section - 1.0).abs() < 1e-9,
        "the section gain {section}"
    );
    assert!((without_sub - want).abs() > 1e-9, "the sub-octave is heard");
}

/// LoudnessSpreadTests.everyFamilyTakesTheSingleReferenceUntilItsBandIsMeasured: one target while
/// the reference bands are empty: every family's is the app's reference, and the probe aims at it.
#[test]
fn every_family_takes_the_single_reference_until_its_band_is_measured() {
    for genre in [
        Some("techno"),
        Some("house"),
        Some("edm"),
        Some("trance"),
        Some("dnb"),
        Some("dubstep"),
        Some("synthwave"),
        Some("lofi"),
        Some("jazz"),
        Some("funk"),
        Some("bossa"),
        Some("country"),
        None,
    ] {
        let family = MixBalance::family(genre);
        if genre.is_some() {
            assert!(family.is_some(), "{genre:?}");
        }
        if let Some(family) = family {
            assert!(MixTargets::shared().band("lufs", family).is_none());
        }
        assert_eq!(MixBalance::target_lufs(genre), REFERENCE_LUFS);
        assert_eq!(Loudness::target_db_for(genre), Loudness::target_db());
    }
}

/// `MixBalance.soloOptions`: every solo (and the mix) renders at one gain, on synth voices, so
/// each part sits at its place in the mix. A `.loudness` deck's options must not make `prepare`
/// measure each solo's own gain (`trackGain = 1`).
#[test]
fn the_solos_play_at_one_gain_on_synth_voices() {
    let p = played("techno", 42.0);
    let o = limit(Fixes::LOUDNESS);
    assert!(o.track_gain.is_none());
    let solo = MixBalance::solo_options(&p, &o);
    assert_eq!(solo.track_gain, Some(1.0));
    assert!(solo.bank.is_none());
    assert_eq!(
        WaveRunner::prepare(&MixBalance::solo(&p, "bass"), 22_050.0, &solo, 0).gain,
        1.0
    );
}

// ---- the rests ---------------------------------------------------------------------------

fn ring16(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

/// RestScheduleTests.restProject: four bars of buildup into four of drop, kick on the beats and
/// an offbeat bass.
fn rest_project() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert(
        "kick",
        ring16("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
    );
    p.nets.insert(
        "bass",
        ring16("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
    );
    for (name, steps) in [("buildup", 64), ("drop", 64)] {
        p.structure.push(Section {
            name: name.into(),
            steps,
            phrases: Default::default(),
        });
    }
    p
}

fn rests_options(fixes: Fixes, rests: Option<RestSchedule>) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        rests,
        ..Default::default()
    }
}

/// RestScheduleTests.trackRestsReplaceTheBuild: `trackRests` is what plays: given, `prepare` does
/// not build one (`.dropGap` on with no rests renders as without it).
#[test]
fn track_rests_replace_the_build() {
    let p = rest_project();
    let unscheduled = render_mono(
        &p,
        &rests_options(Fixes::DROP_GAP, Some(RestSchedule::empty())),
        SR,
        10.0,
    ) == render_mono(&p, &rests_options(Fixes::EMPTY, None), SR, 10.0);
    assert!(unscheduled);
    let built = build_rests(&p, Fixes::DROP_GAP, None);
    assert!(!built.is_empty());
    let same = render_mono(
        &p,
        &rests_options(Fixes::DROP_GAP, Some(built.clone())),
        SR,
        10.0,
    ) == render_mono(&p, &rests_options(Fixes::DROP_GAP, None), SR, 10.0);
    assert!(same);
    assert_eq!(
        WaveRunner::prepare(&p, SR, &rests_options(Fixes::DROP_GAP, None), 0).rests,
        built
    );
}

/// `OfflineRender.layersPerSection` builds the song's rest schedule once, from the song played
/// (`RestSchedule.build(played, fixes:)`: no genre, so no band stop), not in each `prepare` with
/// the options' genre: given `.bandStop` and a song family's genre but no schedule, the layers'
/// levels are those with the empty schedule the build gives, and not those with the stops the
/// genre would add. Golden form (a): the schedule is Swift's `layersPerSection` line.
#[test]
fn layers_per_section_builds_the_schedule_from_the_song_played() {
    use djrust::musicality::audio_metrics::layers_per_section;
    let p = played("blues", 42.0);
    let stops = build_rests(&p, Fixes::BAND_STOP, Some("blues"));
    assert!(!stops.is_empty(), "the song has a band stop");
    assert!(build_rests(&p, Fixes::BAND_STOP, None).is_empty());
    let with = |rests: Option<RestSchedule>| {
        let mut o = rests_options(Fixes::BAND_STOP, rests);
        o.genre = Some("blues".into());
        layers_per_section(&p, &o, 8_000.0, None)
    };
    let built = with(None);
    let none = with(Some(RestSchedule::empty()));
    let stopped = with(Some(stops));
    let levels = |x: &[djrust::musicality::audio_metrics::SectionLayers]| {
        x.iter().map(|s| s.levels_db.clone()).collect::<Vec<_>>()
    };
    assert_eq!(levels(&built), levels(&none));
    assert_ne!(levels(&built), levels(&stopped), "the stops are heard");
}

/// Each gesture's flag builds its own stretches inside `prepare`, `.bandStop` only for the song
/// family's genres, and none of them without a flag; a tail joins the song's schedule, not the
/// song's own (`song_rests`).
#[test]
fn prepare_builds_the_rest_schedule_from_the_fixes() {
    let p = played("edm", 42.0);
    let gestures = Fixes::DROP_GAP | Fixes::LAYER_BUDGET | Fixes::BAND_STOP | Fixes::KICK_OUT;
    for fix in [
        Fixes::DROP_GAP,
        Fixes::LAYER_BUDGET,
        Fixes::KICK_OUT,
        Fixes::BAND_STOP,
        gestures,
    ] {
        let o = rests_options(fix, None);
        let prepared = WaveRunner::prepare(&p, 22_050.0, &o, 0);
        assert_eq!(prepared.song_rests, build_rests(&p, fix, None), "{fix:?}");
        assert_eq!(prepared.rests, prepared.song_rests);
        if fix != Fixes::BAND_STOP {
            assert!(!prepared.song_rests.is_empty(), "{fix:?}");
        }
    }
    // no genre, no band stop; the song family's genre has one
    let blues = played("blues", 42.0);
    let none = WaveRunner::prepare(&blues, 22_050.0, &rests_options(Fixes::BAND_STOP, None), 0);
    assert!(none.song_rests.is_empty());
    let mut o = rests_options(Fixes::BAND_STOP, None);
    o.genre = Some("blues".into());
    let stops = WaveRunner::prepare(&blues, 22_050.0, &o, 0).song_rests;
    assert_eq!(stops, build_rests(&blues, Fixes::BAND_STOP, Some("blues")));
    // the other flags build nothing
    let plain = WaveRunner::prepare(
        &p,
        22_050.0,
        &rests_options(Fixes::ALL & !gestures, None),
        0,
    );
    assert!(plain.song_rests.is_empty());
    // a tail joins the schedule played, and the song's own stays
    let mut o = rests_options(Fixes::DROP_GAP, None);
    o.tail = Some(MixOutTail::new(200, 2));
    let tailed = WaveRunner::prepare(&p, 22_050.0, &o, 0);
    assert_eq!(tailed.song_rests, build_rests(&p, Fixes::DROP_GAP, None));
    assert_ne!(tailed.rests, tailed.song_rests);
}

/// `RestSchedule.build` reads the project the deck plays, not the one the role filter silences:
/// a soloed stem rests where the whole track does.
#[test]
fn a_filtered_stem_rests_as_the_track_does() {
    let p = played("edm", 42.0);
    let whole = WaveRunner::prepare(&p, 22_050.0, &rests_options(Fixes::DROP_GAP, None), 0);
    let mut o = rests_options(Fixes::DROP_GAP, None);
    o.role_filter = Some(RoleFilter {
        mute: vec![],
        solo: Some(vec!["hihat".into()]),
    });
    assert_eq!(
        WaveRunner::prepare(&p, 22_050.0, &o, 0).song_rests,
        whole.song_rests
    );
}
