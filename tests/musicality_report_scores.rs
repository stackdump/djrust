//! The `MusicalityReport` exact-score suites of the Swift tests, ported so the report holds
//! the numbers Swift's does: `FormReportTests` (3), `HarmonyReportTests` (6),
//! `PhraseMetricsTests` (9), `RealismMirrorTests` (the audit's three figures for every genre
//! at two seeds, and its 3 hand-made logs) and `MusicalityTests.fixesRegistryIsConsistent`.
//!
//! Each test names its Swift test and says how it is proven:
//! * (a) the Swift suite's own literals (the hand-made logs: exact figures), or
//! * (b) `swift/musicalityreport.json`, which `tools/paritydump` writes from the Swift DJCore:
//!   the whole `MusicalityReport.dictionary` of played tracks, and the realism figures. (The
//!   `.all` report of the fix matrix is compared in `musicality_golden.rs`.)

mod common;
use common::*;
use djrust::gen::core::scale_intervals;
use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::musicality::note_log::{capture_all, capture_all_with, NoteEvent, PartChange};
use djrust::musicality::report::{is_tonal_event, round3, MusicalityReport};
use djrust::musicality::section_harmony::SectionHarmony;
use djrust::musicality::{apply, Fixes};
use djrust::pflow::{ControlBinding, NetBundle, Project, Section};
use djrust::wave::net::ControlAction;
use djrust::TrackSpec;
use std::collections::{BTreeMap, BTreeSet};

fn sections(list: &[(&str, i64)]) -> Vec<Section> {
    list.iter()
        .map(|(name, steps)| Section {
            name: (*name).into(),
            steps: *steps,
            phrases: Default::default(),
        })
        .collect()
}

/// `project(_ sections:)` of the Swift tests: C major with these sections.
fn project(list: &[(&str, i64)]) -> Project {
    Project {
        root_note: Some(60),
        scale_name: "Major".into(),
        structure: sections(list),
        ..Project::default()
    }
}

fn report(p: &Project, log: &[NoteEvent]) -> MusicalityReport {
    MusicalityReport::new(p, log, &[], None)
}

fn dict_f(r: &MusicalityReport, key: &str) -> Option<f64> {
    r.dictionary().get(key).and_then(JsonValue::as_f64)
}

/// `NoteEvent(tick:net:group:note:velocity:channel:steps:)` with velocity 100, channel 1.
fn note(tick: i64, net: &str, group: &str, note: i64, steps: f64) -> NoteEvent {
    NoteEvent {
        tick,
        net: net.into(),
        group: group.into(),
        note,
        velocity: 100,
        channel: 1,
        steps,
    }
}

// ---------------------------------------------------------------------------
// FormReportTests

/// The report of what `fixes` leave of a composed track (`formReport` in FormReportTests).
fn form_report(genre: &str, seed: f64, structure: Option<&str>, fixes: Fixes) -> MusicalityReport {
    let spec = TrackSpec::new(genre, seed, structure);
    let p = apply(&spec.project(), &spec, fixes);
    let (notes, controls) = capture_all(&p, None);
    MusicalityReport::new(&p, &notes, &controls, None)
}

/// (a) FormReportTests.latenessIsGoneWithTimingAndPhrases
#[test]
fn lateness_is_gone_with_timing_and_phrases() {
    for genre in ["techno", "house"] {
        let raw = form_report(genre, 42.0, Some("standard"), Fixes::EMPTY);
        assert!(
            raw.max_lateness_ticks.unwrap_or(0) > 0,
            "{genre}: unfixed changes land late"
        );
        let fixed = form_report(
            genre,
            42.0,
            Some("standard"),
            Fixes::TIMING | Fixes::PHRASES,
        );
        assert_eq!(
            fixed.max_lateness_ticks,
            Some(0),
            "{genre}: every change on a bar"
        );
        assert!(fixed.structure_on_4_bars.is_some());
        assert!(fixed.structure_on_4_bars.unwrap_or(0.0) >= raw.structure_on_4_bars.unwrap_or(0.0));
    }
}

/// (a) FormReportTests.introOutroAndLength
#[test]
fn intro_outro_and_length() {
    let song = form_report("techno", 42.0, Some("standard"), Fixes::ALL);
    assert_eq!(song.intro_bars, Some(4));
    assert!(song.outro_bars.is_some());
    assert!(song.song_seconds.unwrap_or(0.0) > 0.0);
    assert!(!song.phrase_bars.is_empty());
    assert!(song.phrase_bars.iter().all(|b| *b > 0.0));
    let d = song.dictionary();
    assert_eq!(d.get("introBars").and_then(JsonValue::as_f64), Some(4.0));
    assert!(d.get("songSeconds").is_some());

    let lp = form_report("techno", 42.0, None, Fixes::ALL);
    assert_eq!(lp.intro_bars, None);
    assert_eq!(lp.outro_bars, None);
    assert_eq!(lp.song_seconds, None);
    assert!(lp.phrase_bars.is_empty());
    assert_eq!(lp.structure_on_4_bars, None);
}

/// (a) FormReportTests.handMadeChanges
#[test]
fn hand_made_changes() {
    let p = Project {
        tempo: 120.0,
        structure: sections(&[("intro", 64), ("drop", 128), ("outro", 64)]),
        ..Project::default()
    };
    // PartChange(tick:net:action:target:): the acting step defaults to the tick
    let change = |tick: i64, action| PartChange {
        tick,
        net: "struct-a".into(),
        action,
        target: "a".into(),
        phase: None,
        step: tick,
    };
    let controls = [
        change(64, ControlAction::Unmute),
        change(130, ControlAction::Mute),
    ];
    let r = MusicalityReport::new(&p, &[], &controls, None);
    assert_eq!(r.structure_on_4_bars, Some(0.5));
    assert_eq!(r.max_lateness_ticks, Some(2));
    assert_eq!(r.intro_bars, Some(4));
    assert_eq!(r.outro_bars, Some(4));
    assert_eq!(r.song_seconds, Some(256.0 * 60.0 / (120.0 * 4.0)));
    // events at 0, 64, 130, 192 and the end at 256
    assert_eq!(r.phrase_bars, vec![62.0 / 16.0, 4.0, 66.0 / 16.0]);
}

// ---------------------------------------------------------------------------
// HarmonyReportTests

mod harmony {
    use super::*;

    fn tone(tick: i64, group: &str, n: i64) -> NoteEvent {
        note(tick, group, group, n, 4.0)
    }

    /// One pad chord (root, third, fifth) per bar, struck on the bar's first tick, root first.
    pub fn pad(roots: &[i64]) -> Vec<NoteEvent> {
        roots
            .iter()
            .enumerate()
            .flat_map(|(b, r)| {
                [*r, r + 4, r + 7]
                    .into_iter()
                    .map(move |n| tone(16 * b as i64 + 1, "harmony", n))
            })
            .collect()
    }

    /// (a) HarmonyReportTests.cadencesAndProgressionsScoreExactly
    #[test]
    fn cadences_and_progressions_score_exactly() {
        let roots = [60, 65, 67, 60, 57, 65, 67, 58, 60, 65, 67, 60];
        let r = report(&project(&[("song", 192)]), &pad(&roots));
        assert_eq!(r.harmonic_cadences, Some(0.5));
        assert_eq!(r.progressions, Some(2));
    }

    /// (a) HarmonyReportTests.cadenceIntervals
    #[test]
    fn cadence_intervals() {
        let cadence = |last: i64, next: i64| {
            report(&project(&[("song", 128)]), &pad(&[60, 60, 60, last, next])).harmonic_cadences
        };
        assert_eq!(cadence(67, 60), Some(1.0));
        assert_eq!(cadence(55, 60), Some(1.0));
        assert_eq!(cadence(58, 60), Some(1.0));
        assert_eq!(cadence(65, 60), Some(0.0));
        assert_eq!(cadence(60, 60), Some(0.0));
    }

    /// (a) HarmonyReportTests.oneLoopIsOneProgression
    #[test]
    fn one_loop_is_one_progression() {
        let lp = [60, 67, 69, 65];
        let four: Vec<i64> = lp.iter().cycle().take(16).copied().collect();
        let r = report(&project(&[("song", 256)]), &pad(&four));
        assert_eq!(r.progressions, Some(1));
        assert_eq!(
            r.harmonic_cadences,
            Some(0.0),
            "IV -> I is plagal, not a cadence here"
        );
        // a partial block at the end is not a progression of its own
        let partial: Vec<i64> = lp.iter().cycle().take(8).copied().chain([60, 67]).collect();
        let r = report(&project(&[("song", 96)]), &pad(&partial));
        assert_eq!(r.progressions, Some(1));
    }

    /// (a) HarmonyReportTests.sectionChordAgreementPerSection
    #[test]
    fn section_chord_agreement_per_section() {
        let mut log = pad(&[60, 65, 67, 60, 60, 65, 67, 60, 60, 65, 67, 60]);
        for b in 0..12i64 {
            let root = [60, 65, 67, 60][(b % 4) as usize];
            // a fourth up: off the triad
            let melody = if b >= 8 && b % 2 == 0 {
                root + 5
            } else {
                root + 12
            };
            log.push(tone(16 * b + 1, "melody", melody));
            log.push(tone(16 * b + 3, "melody", root + 1)); // off the beat: not counted
            if b >= 8 {
                log.push(tone(16 * b + 5, "bass", root - 24));
            }
        }
        let r = report(&project(&[("verse", 128), ("bridge", 64)]), &log);
        let by = &r.section_chord_agreement_by_section;
        assert_eq!(
            by.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
            ["verse", "bridge"]
        );
        assert_eq!(by.iter().map(|s| s.1).collect::<Vec<_>>(), [1.0, 0.75]);
        assert_eq!(r.section_chord_agreement, Some(0.75));
        let d = r.dictionary();
        let sections: Vec<&str> = d
            .get("sectionChordAgreementBySection")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .filter_map(|s| s.get("section").and_then(JsonValue::as_str))
            .collect();
        assert_eq!(sections, ["verse", "bridge"]);
    }

    /// (a) HarmonyReportTests.noHarmonyNoMetrics
    #[test]
    fn no_harmony_no_metrics() {
        let log: Vec<NoteEvent> = (0..8).map(|k| tone(16 * k + 1, "melody", 72)).collect();
        let r = report(&project(&[("song", 128)]), &log);
        assert_eq!(r.harmonic_cadences, None);
        assert_eq!(r.progressions, None);
        assert_eq!(r.section_chord_agreement, None);
        assert!(r.section_chord_agreement_by_section.is_empty());
        let d = r.dictionary();
        for k in ["harmonicCadences", "progressions", "sectionChordAgreement"] {
            assert!(d.get(k).is_none(), "{k}");
        }
    }
}

/// (a) HarmonyReportTests.playedTracksReportHarmony (Swift prints the baseline lines it
/// records; here the ranges are asserted, and the figures themselves are held to Swift's by
/// `reports_match_swift` and `played_tracks_match_swift`).
#[test]
fn played_tracks_report_harmony() {
    for genre in ["techno", "house", "trance", "jazz", "lofi", "blues"] {
        let spec = TrackSpec::new(genre, 42.0, Some("standard"));
        let played = apply(&spec.project(), &spec, Fixes::ALL);
        let (notes, controls) = capture_all(&played, None);
        let r = MusicalityReport::new(&played, &notes, &controls, None);
        if r.chord_tones_on_beats.is_none() && r.pad_color.is_none() {
            continue;
        }
        let d = r.dictionary();
        assert!(d.get("harmonicCadences").is_some(), "{genre}");
        assert!(d.get("progressions").is_some(), "{genre}");
        assert!(d.get("sectionChordAgreement").is_some(), "{genre}");
        assert!(
            (0.0..=1.0).contains(&r.harmonic_cadences.unwrap_or(-1.0)),
            "{genre}"
        );
        assert!(r.progressions.unwrap_or(0) >= 1, "{genre}");
        assert!(
            (0.0..=1.0).contains(&r.section_chord_agreement.unwrap_or(-1.0)),
            "{genre}"
        );
    }
}

// ---------------------------------------------------------------------------
// PhraseMetricsTests

mod phrase {
    use super::*;

    fn tone(tick: i64, group: &str, n: i64, net: Option<&str>, steps: f64) -> NoteEvent {
        note(tick, net.unwrap_or(group), group, n, steps)
    }

    /// A note on bar `bar` (0-based), sixteenth `step` of it.
    fn at(bar: i64, step: i64, group: &str, n: i64) -> NoteEvent {
        tone(16 * bar + step + 1, group, n, None, 2.0)
    }

    fn at_net(bar: i64, step: i64, group: &str, n: i64, net: &str) -> NoteEvent {
        tone(16 * bar + step + 1, group, n, Some(net), 2.0)
    }

    /// One C-major triad per bar, root first, on the bar's first tick.
    fn pad(bars: i64) -> Vec<NoteEvent> {
        (0..bars)
            .flat_map(|b| {
                [60, 64, 67]
                    .into_iter()
                    .map(move |n| tone(16 * b + 1, "harmony", n, None, 2.0))
            })
            .collect()
    }

    /// (a) PhraseMetricsTests.answerPhrasesScoreExactly
    #[test]
    fn answer_phrases_score_exactly() {
        let log = [
            at(0, 0, "melody", 72),
            at(3, 8, "melody", 67), // question: ends on the fifth
            at(4, 0, "melody", 74),
            at(7, 8, "melody", 72), // answer: ends on the tonic
            at(8, 0, "melody", 64),
            at(11, 8, "melody", 60), // closed already
            at(12, 0, "melody", 62),
            at(15, 8, "melody", 84),
            at_net(0, 0, "melody", 79, "lead2"),
            at_net(2, 0, "melody", 77, "lead2"),
        ];
        let r = report(&project(&[("song", 256)]), &log);
        assert_eq!(r.answer_phrases, Some(0.5));
        assert_eq!(dict_f(&r, "answerPhrases"), Some(0.5));
    }

    /// (a) PhraseMetricsTests.answerPhrasesFollowTheKey
    #[test]
    fn answer_phrases_follow_the_key() {
        let mut p = project(&[("song", 128)]);
        p.root_note = Some(55);
        let log = [at(3, 0, "melody", 62), at(7, 0, "melody", 79)];
        assert_eq!(report(&p, &log).answer_phrases, Some(1.0));
        p.root_note = Some(60);
        assert_eq!(report(&p, &log).answer_phrases, Some(0.0));
    }

    /// (a) PhraseMetricsTests.hookRecurrenceScoresExactly
    #[test]
    fn hook_recurrence_scores_exactly() {
        let log = [
            at(0, 0, "melody", 60),
            at(1, 0, "melody", 72),
            at(1, 4, "melody", 74),
            at(1, 8, "melody", 76), // the hook
            at(5, 0, "melody", 67),
            at(5, 4, "melody", 69),
            at(5, 8, "melody", 71), // verse: transposed
            at(8, 0, "melody", 76),
            at(8, 4, "melody", 74),
            at(8, 8, "melody", 72), // chorus: inverted
            at(12, 0, "melody", 72),
            at(12, 4, "melody", 74),
            at(12, 12, "melody", 76), // bridge: other
        ];
        let sections = [
            ("intro", 64),
            ("verse", 64),
            ("chorus", 64),
            ("bridge", 64),
            ("break", 64),
        ];
        let r = report(&project(&sections), &log);
        assert_eq!(r.hook_recurrence, Some(2.0 / 3.0));
        assert_eq!(dict_f(&r, "hookRecurrence"), Some(0.667));
        // one section: nothing comes after the hook
        let one = report(&project(&[("song", 256)]), &log);
        assert_eq!(one.hook_recurrence, None);
    }

    /// (a) PhraseMetricsTests.barOneChordTonesScoreExactly
    #[test]
    fn bar_one_chord_tones_score_exactly() {
        let mut log = pad(8);
        log.extend([
            at(0, 0, "melody", 72),
            at(0, 4, "melody", 74),
            at(1, 0, "melody", 73),
            at(4, 0, "melody", 76),
            at(4, 8, "melody", 79),
        ]);
        let r = report(&project(&[("song", 128)]), &log);
        assert_eq!(r.bar_one_chord_tones, Some(0.75));
        // without a pad there is no chord to be a tone of
        let bare: Vec<NoteEvent> = log.into_iter().filter(|e| e.group != "harmony").collect();
        assert_eq!(
            report(&project(&[("song", 128)]), &bare).bar_one_chord_tones,
            None
        );
    }

    /// (a) PhraseMetricsTests.bassIdeaRepeatsScoreExactly
    #[test]
    fn bass_idea_repeats_score_exactly() {
        let log = [
            at(0, 0, "bass", 36),
            at(0, 6, "bass", 36),
            at(0, 10, "bass", 43),
            at(1, 0, "bass", 41),
            at(1, 6, "bass", 41),
            at(1, 10, "bass", 48),
            at(4, 0, "bass", 36),
            at(4, 8, "bass", 36),
            at(5, 0, "bass", 36),
            at(5, 4, "bass", 36),
            at(8, 0, "bass", 36),
            at(9, 0, "bass", 36),
        ];
        let r = report(&project(&[("song", 192)]), &log);
        assert_eq!(r.bass_idea_repeats, Some(0.5));
        assert_eq!(dict_f(&r, "bassIdeaRepeats"), Some(0.5));
    }

    /// (a) PhraseMetricsTests.melodyPeakPositionScoresExactly
    #[test]
    fn melody_peak_position_scores_exactly() {
        let log = [
            at(0, 0, "melody", 60),
            at(2, 0, "melody", 67),
            at(2, 8, "melody", 72),
            at(3, 12, "melody", 64),
            at(4, 0, "melody", 79),
            at(5, 0, "melody", 72),
            at(6, 0, "melody", 79),
            at(8, 0, "melody", 84), // one note: not a phrase
        ];
        let r = report(&project(&[("song", 192)]), &log);
        assert_eq!(r.melody_peak_position, Some(0.3125));
    }

    /// (a) PhraseMetricsTests.leapRecoveryScoresExactly
    #[test]
    fn leap_recovery_scores_exactly() {
        let line = |ns: &[i64]| -> Vec<NoteEvent> {
            ns.iter()
                .enumerate()
                .map(|(k, n)| tone(4 * k as i64 + 1, "melody", *n, None, 2.0))
                .collect()
        };
        let p = project(&[("song", 64)]);
        let r = report(&p, &line(&[60, 67, 65, 72, 74, 62]));
        assert_eq!(r.leap_recovery, Some(0.5));
        // a leap down recovered by a step up
        assert_eq!(report(&p, &line(&[72, 64, 65])).leap_recovery, Some(1.0));
        // half a bar or more between the leap and the next note: not followed, not counted
        let late = [
            tone(1, "melody", 60, None, 2.0),
            tone(5, "melody", 67, None, 2.0),
            tone(17, "melody", 65, None, 2.0),
        ];
        assert_eq!(report(&p, &late).leap_recovery, None);
    }

    /// (a) PhraseMetricsTests.noPartsNoMetrics
    #[test]
    fn no_parts_no_metrics() {
        let r = report(&project(&[("song", 128)]), &pad(8));
        assert_eq!(r.answer_phrases, None);
        assert_eq!(r.hook_recurrence, None);
        assert_eq!(r.bar_one_chord_tones, None);
        assert_eq!(r.bass_idea_repeats, None);
        assert_eq!(r.melody_peak_position, None);
        assert_eq!(r.leap_recovery, None);
        let d = r.dictionary();
        for k in [
            "answerPhrases",
            "hookRecurrence",
            "barOneChordTones",
            "bassIdeaRepeats",
            "melodyPeakPosition",
            "leapRecovery",
        ] {
            assert!(d.get(k).is_none(), "{k}");
        }
    }
}

/// (a) PhraseMetricsTests.playedTracksReportPhraseMetrics
#[test]
fn played_tracks_report_phrase_metrics() {
    for genre in ["techno", "house", "trance", "jazz", "lofi", "synthwave"] {
        let spec = TrackSpec::new(genre, 42.0, Some("standard"));
        let played = apply(&spec.project(), &spec, Fixes::ALL);
        let (notes, controls) = capture_all(&played, None);
        let r = MusicalityReport::new(&played, &notes, &controls, None);
        let values = [
            r.answer_phrases,
            r.hook_recurrence,
            r.bar_one_chord_tones,
            r.bass_idea_repeats,
            r.melody_peak_position,
            r.leap_recovery,
        ];
        for v in values.into_iter().flatten() {
            assert!((0.0..=1.0).contains(&v), "{genre}: {v}");
        }
        assert!(r.melody_peak_position.is_some(), "{genre} has a melody");
    }
}

// ---------------------------------------------------------------------------
// MusicalityTests.fixesRegistryIsConsistent

/// (a) MusicalityTests.fixesRegistryIsConsistent: every flag has its own bit, a name
/// `djctl render --fixes` takes, and is in `all`.
#[test]
fn fixes_registry_is_consistent() {
    let flags: Vec<(&str, Fixes)> = Fixes::NAMED.to_vec();
    let bits: Vec<u64> = flags.iter().map(|(_, f)| f.0).collect();
    assert!(
        bits.iter().all(|b| *b != 0 && b & (b - 1) == 0),
        "one bit per flag"
    );
    assert_eq!(
        bits.iter().collect::<BTreeSet<_>>().len(),
        bits.len(),
        "bits are unique"
    );
    let union = flags.iter().fold(Fixes::EMPTY, |a, (_, f)| a | *f);
    assert_eq!(union, Fixes::ALL, "named and all list the same flags");
    assert_eq!(Fixes::named("none"), Some(Fixes::EMPTY));
    // Swift's `named` has these 53 flags and "none": names are unique, every name finds its
    // flag back, and the set of names is the one the dump iterates (musicality.json)
    let names: BTreeSet<&str> = flags.iter().map(|(n, _)| *n).collect();
    assert_eq!(names.len(), flags.len(), "names are unique");
    assert_eq!(flags.len(), 53);
    for (name, f) in &flags {
        assert_eq!(Fixes::named(name), Some(*f), "{name}");
        assert_eq!(f.names(), vec![name.to_string()], "{name}");
    }
    // bits 0..=52 in order, as Swift's
    assert_eq!(bits, (0..53).map(|k| 1u64 << k).collect::<Vec<_>>());
    assert_eq!(Fixes::ALL.names().len(), 53);
}

// ---------------------------------------------------------------------------
// RealismMirrorTests (docs/research/realism-2026-10.md section 5)
//
// The three figures are computed by the helpers below, which are the Swift test's private
// helpers; Swift's report keeps its own neighbours of them.

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Figure {
    Scale,
    StrongBeats,
    Fills,
}

/// The tracks that miss a figure today, keyed "genre seed" (RealismMirrorTests.knownIssues).
/// Swift records them with `withKnownIssue`, which fails when the figure is met after all:
/// the test below does the same.
fn known_issues(key: &str) -> &'static [Figure] {
    use Figure::*;
    match key {
        "blues 42" => &[Scale],
        "blues 4242" => &[Scale, StrongBeats],
        "jazz 42" => &[Scale],
        "lofi 42" => &[Scale],
        "lofi 4242" => &[Scale],
        "dnb 4242" => &[Fills],
        "dubstep 42" => &[Fills],
        "garage 42" => &[Fills],
        "garage 4242" => &[Fills],
        "techno 4242" => &[Fills],
        "trance 42" => &[Fills],
        "trap 42" => &[Fills],
        _ => &[],
    }
}

const SCALE_TARGET: f64 = 0.97;
const STRONG_BEAT_TARGET: f64 = 0.8;

/// What a deck plays for (genre, seed): the played project and its notes and part changes.
/// `WaveOptions.extended.musicality` is `.all`.
fn played_log(genre: &str, seed: f64) -> (Project, Vec<NoteEvent>, Vec<PartChange>) {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    let fixes = Fixes::ALL;
    let played = apply(&spec.project(), &spec, fixes);
    let harmony = SectionHarmony::playing(&played, fixes);
    let (notes, controls) = capture_all_with(&played, None, harmony.as_ref());
    (played, notes, controls)
}

fn key_classes(p: &Project) -> BTreeSet<i64> {
    let root = p.root_note.unwrap_or(60);
    let name = if p.scale_name.is_empty() {
        "Major"
    } else {
        &p.scale_name
    };
    scale_intervals(&format!("{name}Scale"))
        .iter()
        .map(|i| (root + i).rem_euclid(12))
        .collect()
}

fn pc(n: i64) -> i64 {
    n.rem_euclid(12)
}

/// M2a: per tonal role (group), the share of its notes in the key, approach notes left out;
/// a role whose notes are all approach notes is left out too.
fn scale_consistency(p: &Project, log: &[NoteEvent]) -> BTreeMap<String, f64> {
    let key = key_classes(p);
    let mut by_group: BTreeMap<&str, Vec<&NoteEvent>> = BTreeMap::new();
    for e in log.iter().filter(|e| is_tonal_event(e)) {
        by_group.entry(e.group.as_str()).or_default().push(e);
    }
    let mut out = BTreeMap::new();
    for (group, es) in by_group {
        let mut by_net: BTreeMap<&str, Vec<&NoteEvent>> = BTreeMap::new();
        for e in es {
            by_net.entry(e.net.as_str()).or_default().push(e);
        }
        let (mut counted, mut in_key) = (0usize, 0usize);
        for line in by_net.values() {
            let mut sorted = line.clone();
            sorted.sort_by_key(|e| (e.tick, e.note));
            for (k, e) in sorted.iter().enumerate() {
                if key.contains(&pc(e.note)) {
                    counted += 1;
                    in_key += 1;
                    continue;
                }
                // an approach: the next onset of the net, within a beat, a semitone away and
                // in key
                let next = sorted[k + 1..].iter().find(|n| n.tick > e.tick);
                if let Some(next) = next {
                    if next.tick - e.tick <= 4
                        && (next.note - e.note).abs() == 1
                        && key.contains(&pc(next.note))
                    {
                        continue;
                    }
                }
                counted += 1;
            }
        }
        if counted > 0 {
            out.insert(group.to_string(), in_key as f64 / counted as f64);
        }
    }
    out
}

/// M2b: melody notes on beats 1 and 3 of a bar with a pad chord that are tones of that chord;
/// None without a pad or a melody note on a strong beat.
fn strong_beat_chord_tones(log: &[NoteEvent]) -> Option<f64> {
    let mut chord: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for e in log
        .iter()
        .filter(|e| e.group == "harmony" && is_tonal_event(e))
    {
        chord
            .entry((e.tick - 1) / 16)
            .or_default()
            .insert(pc(e.note));
    }
    let (mut total, mut hits) = (0usize, 0usize);
    for e in log
        .iter()
        .filter(|e| e.group == "melody" && (e.tick - 1) % 8 == 0)
    {
        let Some(c) = chord.get(&((e.tick - 1) / 16)) else {
            continue;
        };
        total += 1;
        if c.contains(&pc(e.note)) {
            hits += 1;
        }
    }
    (total > 0).then(|| hits as f64 / total as f64)
}

/// Each section's (energy, filter) from the `feel-curve` net, by section index; None for a
/// section the curve does not mark (it marks each name at its first start).
fn section_feel(p: &Project) -> Vec<Option<(f64, f64)>> {
    let Some(feel) = p.nets.get("feel-curve") else {
        return p.structure.iter().map(|_| None).collect();
    };
    let mut points: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    for (t, c) in feel.control_bindings.iter() {
        if c.action != "set-feel" {
            continue;
        }
        let Some(step) = t.strip_prefix("ft").and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        let (Some(x), Some(y)) = (
            c.macro_params
                .as_ref()
                .and_then(|m| m.get("x"))
                .and_then(JsonValue::as_f64),
            c.macro_params
                .as_ref()
                .and_then(|m| m.get("y"))
                .and_then(JsonValue::as_f64),
        ) else {
            continue;
        };
        points.insert(step, (x, y));
    }
    let mut by_name: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    let mut at = 0;
    for s in &p.structure {
        if !by_name.contains_key(s.name.as_str()) {
            if let Some(pt) = points.get(&at) {
                by_name.insert(s.name.as_str(), *pt);
            }
        }
        at += s.steps;
    }
    p.structure
        .iter()
        .map(|s| by_name.get(s.name.as_str()).copied())
        .collect()
}

/// One M5 boundary: the step it falls on, and the notes of the bar before it that the
/// section's other bars never play.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FillBoundary {
    step: i64,
    from: String,
    to: String,
    new_notes: usize,
}

/// M5: the boundaries (not the song's end) where energy or filter changes, each with the
/// number of notes in the section's last bar that differ from the section's loop: a note is
/// (group, sixteenth in the bar, pitch), and it differs when no other bar of the section has
/// it. A section of one bar has no loop to differ from: all its notes count.
fn fill_boundaries(p: &Project, log: &[NoteEvent]) -> Vec<FillBoundary> {
    type Hit = (String, i64, i64);
    let feel = section_feel(p);
    let mut by_bar: BTreeMap<i64, BTreeSet<Hit>> = BTreeMap::new();
    for e in log {
        by_bar.entry((e.tick - 1) / 16).or_default().insert((
            e.group.clone(),
            (e.tick - 1) % 16,
            e.note,
        ));
    }
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..p.structure.len().saturating_sub(1) {
        let s = &p.structure[i];
        let end = start + s.steps;
        let this_start = start;
        start = end;
        let (Some(a), Some(b)) = (feel[i], feel[i + 1]) else {
            continue;
        };
        if !(a.0 != b.0 || a.1 != b.1) || s.steps < 16 {
            continue;
        }
        let last = end / 16 - 1;
        let mut lp: BTreeSet<Hit> = BTreeSet::new();
        for bar in this_start / 16..last {
            if let Some(h) = by_bar.get(&bar) {
                lp.extend(h.iter().cloned());
            }
        }
        let fresh = by_bar.get(&last).map_or(0, |h| h.difference(&lp).count());
        out.push(FillBoundary {
            step: end,
            from: s.name.clone(),
            to: p.structure[i + 1].name.clone(),
            new_notes: fresh,
        });
    }
    out
}

fn tone(tick: i64, net: &str, group: &str, n: i64, channel: i64) -> NoteEvent {
    NoteEvent {
        tick,
        net: net.into(),
        group: group.into(),
        note: n,
        velocity: 100,
        channel,
        steps: 1.0,
    }
}

fn c_major(sections: &[(&str, i64)]) -> Project {
    project(sections)
}

/// (a) RealismMirrorTests.playedTracksMeetTheAudit, for every genre at seeds 42 and 4242:
/// each figure meets the audit's target, or (listed in `known_issues`) misses it, as Swift
/// records with `withKnownIssue`.
#[test]
fn played_tracks_meet_the_audit() {
    let mut genres: Vec<String> = BeatsTables::shared().beats_genre_ids();
    genres.sort();
    let mut problems = Vec::new();
    for genre in &genres {
        for seed in [42.0, 4242.0] {
            let key = format!("{genre} {}", seed as i64);
            let known = known_issues(&key);
            let (p, notes, _) = played_log(genre, seed);
            assert!(!notes.is_empty(), "{key}: no notes");
            let scale = scale_consistency(&p, &notes);
            assert!(!scale.is_empty(), "{key}: no tonal role");
            let strong = strong_beat_chord_tones(&notes);
            let fills = fill_boundaries(&p, &notes);

            let scale_met = scale.values().all(|share| *share >= SCALE_TARGET);
            let mut check = |fig: Figure, met: bool, what: String| {
                if known.contains(&fig) {
                    if met {
                        problems.push(format!("{key}: {fig:?} is a known issue but now passes"));
                    }
                } else if !met {
                    problems.push(format!("{key}: {fig:?} {what}"));
                }
            };
            check(Figure::Scale, scale_met, format!("{:?}", scale));
            if let Some(strong) = strong {
                check(
                    Figure::StrongBeats,
                    strong >= STRONG_BEAT_TARGET,
                    format!("{}", round3(strong)),
                );
            }
            let bare: Vec<&FillBoundary> = fills.iter().filter(|f| f.new_notes == 0).collect();
            check(
                Figure::Fills,
                bare.is_empty(),
                format!("no fill at {bare:?}"),
            );
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// (a) RealismMirrorTests.scaleConsistencyLeavesOutApproaches: a walking line's chromatic
/// approach (C# into D, a sixteenth later) is not counted; an F# that goes nowhere a semitone
/// away is. Per role.
#[test]
fn scale_consistency_leaves_out_approaches() {
    let log = [
        tone(1, "bass-0", "bass", 36, 1),
        tone(5, "bass-0", "bass", 37, 1),
        tone(6, "bass-0", "bass", 38, 1),
        tone(9, "bass-0", "bass", 40, 1),
        tone(1, "melody-0", "melody", 72, 1),
        tone(5, "melody-0", "melody", 78, 1),
        tone(9, "melody-0", "melody", 76, 1),
        tone(13, "melody-0", "melody", 74, 1),
        tone(1, "kick", "drums", 36, 10),
    ];
    let r = scale_consistency(&c_major(&[("song", 64)]), &log);
    assert_eq!(r.get("bass"), Some(&1.0));
    assert_eq!(r.get("melody"), Some(&0.75));
    assert_eq!(r.get("drums"), None);
    // an approach more than a beat early, or a whole tone away, counts against the role
    let late = scale_consistency(
        &c_major(&[("song", 64)]),
        &[
            tone(1, "b", "bass", 37, 1),
            tone(6, "b", "bass", 38, 1),
            tone(9, "b", "bass", 36, 1),
        ],
    );
    assert!(late
        .get("bass")
        .is_some_and(|v| (v - 2.0 / 3.0).abs() < 1e-9));
}

/// (a) RealismMirrorTests.strongBeatsAgainstThePad: beats 1 and 3 count, 2 and 4 do not; a bar
/// without a pad chord is not measured.
#[test]
fn strong_beats_against_the_pad() {
    let mut log: Vec<NoteEvent> = [60, 64, 67]
        .iter()
        .map(|n| tone(1, "pad", "harmony", *n, 1))
        .collect();
    log.extend([
        tone(1, "m", "melody", 72, 1),  // C on beat 1: a chord tone
        tone(5, "m", "melody", 74, 1),  // D on beat 2: not counted
        tone(9, "m", "melody", 77, 1),  // F on beat 3: off the chord
        tone(17, "m", "melody", 74, 1), // bar 2 has no pad
    ]);
    assert_eq!(strong_beat_chord_tones(&log), Some(0.5));
    let bare: Vec<NoteEvent> = log.into_iter().filter(|e| e.group != "harmony").collect();
    assert_eq!(strong_beat_chord_tones(&bare), None);
}

/// (a) RealismMirrorTests.fillsAtEnergyChanges: a section whose last bar repeats its loop has
/// no fill; a snare roll in the last bar is one. Boundaries where the energy stays the same
/// are not measured.
#[test]
fn fills_at_energy_changes() {
    let mut p = c_major(&[("intro", 64), ("drop", 64), ("outro", 64)]);
    let mut feel = NetBundle::default();
    for (step, level) in [(0, 0.3), (64, 0.9), (128, 0.9)] {
        let mut c = ControlBinding::new("set-feel", "");
        c.macro_params =
            Some(JsonValue::parse(&format!("{{\"x\":{level},\"y\":{level}}}")).unwrap());
        feel.control_bindings.insert(format!("ft{step}"), c);
    }
    p.nets.insert("feel-curve", feel);
    let mut log = Vec::new();
    for bar in 0..12 {
        for beat in 0..4 {
            log.push(tone(16 * bar + 4 * beat + 1, "kick", "drums", 36, 1));
        }
    }
    assert_eq!(
        fill_boundaries(&p, &log),
        vec![FillBoundary {
            step: 64,
            from: "intro".into(),
            to: "drop".into(),
            new_notes: 0
        }]
    );
    for s in 12..16 {
        log.push(tone(16 * 3 + s + 1, "fill-0", "drums", 38, 1));
    }
    let counts: Vec<usize> = fill_boundaries(&p, &log)
        .iter()
        .map(|f| f.new_notes)
        .collect();
    assert_eq!(counts, [4]);
}

// ---------------------------------------------------------------------------
// (b) Swift goldens

/// (b) The whole `MusicalityReport.dictionary` of the fix matrix's tracks without fixes and
/// with `.timing` + `.phrases` (the unfixed numbers: late changes, unmapped phrases); the `.all`
/// variant's is compared in `musicality_golden.rs`.
#[test]
fn reports_match_swift() {
    let doc = fixture("musicalityreport");
    let mut d = Diffs::default();
    for row in rows(&doc, "cases") {
        let structure = row.get("structure").and_then(JsonValue::as_str);
        let genre = s(row, "genre");
        let spec = TrackSpec::new(genre, f(row, "seed"), structure);
        let composed = spec.project();
        for (name, fixes) in [
            ("unfixed", Fixes::EMPTY),
            ("timingPhrases", Fixes::TIMING | Fixes::PHRASES),
        ] {
            let p = apply(&composed, &spec, fixes);
            let (notes, controls) = capture_all(&p, None);
            let r = MusicalityReport::new(&p, &notes, &controls, Some(genre));
            d.eq(
                format!("{genre} {} {structure:?} {name}", f(row, "seed")),
                r.dictionary().canonical_js(),
                row.get(name).unwrap().canonical_js(),
            );
        }
    }
    d.finish("report dictionaries");
}

/// (b) What a deck plays, every genre at seeds 42 and 4242: the note count, the whole report
/// (with the section plan the runner plays), and the realism figures, equal to Swift's.
#[test]
fn played_tracks_match_swift() {
    let doc = fixture("musicalityreport");
    let mut d = Diffs::default();
    let realism = rows(&doc, "realism");
    let mut genres: Vec<String> = BeatsTables::shared().beats_genre_ids();
    genres.sort();
    assert_eq!(realism.len(), genres.len() * 2);
    for row in realism {
        let (genre, seed) = (s(row, "genre"), f(row, "seed"));
        let tag = format!("{genre} {seed}");
        let (p, notes, controls) = played_log(genre, seed);
        d.eq(format!("{tag} notes"), notes.len() as i64, i(row, "notes"));
        let r = MusicalityReport::new(&p, &notes, &controls, Some(genre));
        d.eq(
            format!("{tag} report"),
            r.dictionary().canonical_js(),
            row.get("report").unwrap().canonical_js(),
        );
        let scale = scale_consistency(&p, &notes);
        d.eq(
            format!("{tag} scale"),
            scale
                .iter()
                .map(|(k, v)| format!("{k}={v:?}"))
                .collect::<Vec<_>>()
                .join(","),
            row.get("scale")
                .and_then(JsonValue::as_object)
                .unwrap()
                .iter()
                .map(|(k, v)| format!("{k}={:?}", v.as_f64().unwrap()))
                .collect::<Vec<_>>()
                .join(","),
        );
        d.eq(
            format!("{tag} strong"),
            strong_beat_chord_tones(&notes),
            row.get("strong").and_then(JsonValue::as_f64),
        );
        let fills: Vec<(i64, String, String, i64)> = fill_boundaries(&p, &notes)
            .into_iter()
            .map(|b| (b.step, b.from, b.to, b.new_notes as i64))
            .collect();
        let want: Vec<(i64, String, String, i64)> = rows(row, "fills")
            .iter()
            .map(|b| {
                (
                    i(b, "step"),
                    s(b, "from").into(),
                    s(b, "to").into(),
                    i(b, "newNotes"),
                )
            })
            .collect();
        d.eq(format!("{tag} fills"), fills, want);
    }
    d.finish("played tracks");
}
