//! Ports of Swift `SectionHarmonyTests`, `SectionMelodyTests`, `HarmonicRhythmTests` (all
//! but the one that reads `WaveRunner.prepare`'s `harmony`, which is in `wave_harmony.rs`) and
//! `SlotWindowsTests`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use djrust::gen::core::scale_intervals;
use djrust::gen::structure::build_control_bundle;
use djrust::gen::tables::BeatsTables;
use djrust::gen::theme::build_track_theme;
use djrust::musicality::dj::pad_chords;
use djrust::musicality::energy_curve::EnergyCurve;
use djrust::musicality::note_log::{
    capture, capture_all, capture_all_with, group_name, length, ring_position,
};
use djrust::musicality::report::MusicalityReport;
use djrust::musicality::rest_gestures::layer_budgets;
use djrust::musicality::section_harmony::SectionHarmony;
use djrust::musicality::section_melody::{normalise_section_melody, section_melody_shifts};
use djrust::musicality::slot_windows::{part_only, slot_windows, SlotWindow};
use djrust::musicality::{apply, Fixes};
use djrust::pflow::{ControlBinding, NetBundle, Project, Section};
use djrust::wave::net::{ControlAction, WaveGraph};
use djrust::wave::rests::Class;
use djrust::wave::synth::is_drum_channel;
use djrust::TrackSpec;

fn played(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let spec = TrackSpec::new(genre, seed, structure);
    apply(&spec.project(), &spec, Fixes::ALL)
}

/// The first played project in `genres` × `structures` × seeds 1…6 with a section named
/// `name`.
fn with_section(name: &str) -> Option<Project> {
    for structure in ["extended", "standard", "jam", "ab", "build", "drop"] {
        for genre in BeatsTables::shared().genres.keys() {
            for seed in 1..=6 {
                let p = played(genre, seed as f64, Some(structure));
                if p.structure.iter().any(|s| s.name == name) && SectionHarmony::new(&p).is_some() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn pitch_classes(p: &Project) -> BTreeSet<i64> {
    djrust::musicality::util::key_pitch_classes(p)
}

fn major() -> Vec<i64> {
    scale_intervals("MajorScale").to_vec()
}
fn minor() -> Vec<i64> {
    scale_intervals("MinorScale").to_vec()
}

/// Loops, keys that aren't seven notes and songs with nothing to move get no plan.
#[test]
fn nil_cases() {
    assert!(SectionHarmony::new(&played("techno", 42.0, None)).is_none());
    let mut p = played("trance", 42.0, Some("standard"));
    assert!(p.structure.len() > 1);
    for scale in ["Pentatonic", "MinPentatonic", "Blues"] {
        p.scale_name = scale.into();
        assert!(SectionHarmony::new(&p).is_none(), "{scale}");
    }
    let mut flat = played("trance", 42.0, Some("standard"));
    for s in flat.structure.iter_mut() {
        if SectionHarmony::degree_shift(&s.name, &[0, 2, 4, 5, 7, 9, 11]) != 0 {
            s.name = "verse".into();
        }
    }
    assert!(flat.structure.len() > 1);
    assert!(SectionHarmony::new(&flat).is_none());
}

/// The degree table.
#[test]
fn degree_table() {
    assert_eq!(SectionHarmony::degree_shift("bridge", &major()), 5);
    assert_eq!(SectionHarmony::degree_shift("solo", &minor()), 4);
    assert_eq!(SectionHarmony::degree_shift("breakdown", &major()), -2);
    assert_eq!(SectionHarmony::degree_shift("breakdown", &minor()), 2);
    assert_eq!(
        SectionHarmony::degree_shift("breakdown", scale_intervals("DorianScale")),
        2
    );
    assert_eq!(
        SectionHarmony::degree_shift("breakdown", scale_intervals("MixolydianScale")),
        -2
    );
    for name in ["intro", "verse", "chorus", "drop", "buildup", "outro"] {
        assert_eq!(SectionHarmony::degree_shift(name, &major()), 0);
    }
}

/// The diatonic shift itself.
#[test]
fn diatonic_shift() {
    let m = major();
    assert_eq!(SectionHarmony::shift(60, 5, 0, &m), 69);
    assert_eq!(SectionHarmony::shift(60, -2, 0, &m), 57);
    assert_eq!(SectionHarmony::shift(64, -2, 0, &m), 60);
    assert_eq!(SectionHarmony::shift(61, 1, 0, &m), 63);
    assert_eq!(SectionHarmony::shift(71, 1, 0, &m), 72);
    assert_eq!(SectionHarmony::shift(48, 4, 9, &minor()), 55);
    for n in 0..128 {
        for d in -6..=6 {
            let r = SectionHarmony::shift(n, d, 2, &m);
            assert!((0..128).contains(&r));
        }
    }
}

/// The octave per group: the nearer way round, unless that takes a bass under E1.
#[test]
fn octave_per_group() {
    let m = major();
    assert_eq!(SectionHarmony::octave(5, 60, 72, 0, &m), -2);
    assert_eq!(SectionHarmony::octave(4, 60, 72, 0, &m), -3);
    assert_eq!(SectionHarmony::octave(-2, 60, 72, 0, &m), -2);
    // a bass from E1: a fourth down would leave the floor, so it goes up a fifth
    assert_eq!(SectionHarmony::octave(4, 28, 40, 0, &m), 4);
    // a lead at the top goes down even when up is nearer
    assert_eq!(SectionHarmony::octave(2, 100, 107, 0, &m), -5);
}

/// Every mapped note of every group and section is in key, in 0…127.
#[test]
fn mapped_notes_are_in_key_and_in_range() {
    for genre in ["trance", "techno", "house", "pop", "jazz", "lofi"] {
        let p = played(genre, 42.0, Some("standard"));
        let Some(h) = SectionHarmony::new(&p) else {
            continue;
        };
        let key = pitch_classes(&p);
        assert!(!h.groups.is_empty());
        assert_eq!(
            h.slots.len() as i64,
            h.start.len() as i64 * SectionHarmony::SLOTS_PER_CYCLE
        );
        assert_eq!(h.maps.len(), h.shifts.len() * h.groups.len() * 128);
        for (k, s) in h.start.iter().enumerate() {
            for g in 0..h.groups.len() {
                for n in 0..128 {
                    let m = h.note(n, g, *s);
                    assert!((0..128).contains(&m));
                    if h.degrees[k] == 0 {
                        assert_eq!(m, n);
                    }
                    if key.contains(&(n % 12)) {
                        assert!(key.contains(&(m % 12)), "{genre} {n}→{m}");
                    }
                }
            }
        }
        assert_eq!(h.note(60, h.groups.len(), 0), 60);
        assert_eq!(h.note(-1, 0, 0), -1);
        // the bound notes of each group stay within [E1, C8] or where they already were
        for (id, nb) in p.nets.iter() {
            if nb.role == "control" {
                continue;
            }
            let Some(g) = h.group_index(&group_name(nb, id)) else {
                continue;
            };
            for b in nb.bindings.values().filter(|b| !is_drum_channel(b.channel)) {
                for s in &h.start {
                    let m = h.note(b.note, g, *s);
                    assert!(
                        m >= b.note.min(28) - 12 && m <= b.note.max(108) + 12,
                        "{genre} {id}"
                    );
                }
            }
        }
    }
}

/// A breakdown in a major key moves to the relative minor, in a minor key to the relative major.
#[test]
fn breakdown_goes_relative() {
    let p = played("trance", 42.0, Some("standard"));
    let h = SectionHarmony::new(&p).unwrap();
    let k = p
        .structure
        .iter()
        .position(|s| s.name == "breakdown")
        .unwrap();
    let name = if p.scale_name.is_empty() {
        "Major".to_string()
    } else {
        p.scale_name.clone()
    };
    let intervals = scale_intervals(&format!("{name}Scale"));
    let expected = SectionHarmony::breakdown_shift(intervals);
    assert_eq!(h.degrees[k], expected);
    let root = 48 + p.root_note.unwrap_or(60) % 12;
    let target = (root + intervals[((expected + 7) % 7) as usize]) % 12;
    for g in 0..h.groups.len() {
        assert_eq!(h.note(root, g, h.start[k]) % 12, target);
    }
}

/// A bridge plays +5 degrees and a solo +4 (octave aside), when a song has one.
#[test]
fn bridge_and_solo() {
    for (name, d) in [("bridge", 5i64), ("solo", 4)] {
        let Some(p) = with_section(name) else {
            eprintln!("no project with a {name}");
            continue;
        };
        let h = SectionHarmony::new(&p).unwrap();
        let k = p.structure.iter().position(|s| s.name == name).unwrap();
        assert_eq!(h.degrees[k], d);
        let shift = h.shifts.iter().position(|&s| s == d).unwrap();
        for g in 0..h.groups.len() {
            let pick = h.group_degrees[shift * h.groups.len() + g];
            assert!(pick == d || pick == d - 7);
        }
    }
}

/// The section edges are `EnergyCurve`'s, and every slot of a section holds its shift.
#[test]
fn edges_match_energy_curve() {
    let mut checked = 0;
    for genre in ["trance", "techno", "pop"] {
        let p = played(genre, 42.0, Some("standard"));
        let (Some(h), Some(c)) = (SectionHarmony::new(&p), EnergyCurve::new(&p)) else {
            continue;
        };
        assert_eq!(h.start, c.start, "{genre}");
        for k in 0..h.start.len() {
            assert_eq!(h.section(h.start[k]), k);
            if k > 0 {
                assert_eq!(h.section(h.start[k] - 1), k - 1);
            }
            let n = SectionHarmony::SLOTS_PER_CYCLE as usize;
            let row: BTreeSet<u8> = h.slots[k * n..(k + 1) * n].iter().copied().collect();
            assert_eq!(row.len(), 1);
            assert_eq!(*row.iter().next().unwrap() == 0, h.degrees[k] == 0);
        }
        checked += 1;
    }
    assert!(checked > 0);
}

/// `harmony: nil` is the old capture; with a plan, home sections and drums are unchanged and
/// the moved sections' tonal notes are the plan's.
#[test]
fn note_log_harmony() {
    let p = played("trance", 42.0, Some("standard"));
    let h = SectionHarmony::new(&p).unwrap();
    let home = capture_all_with(&p, None, None);
    assert_eq!(capture(&p, None), home.0);
    let all = capture_all_with(&p, None, Some(&h));
    assert_eq!(all.0.len(), home.0.len());
    let mut changed = 0;
    for (a, b) in home.0.iter().zip(all.0.iter()) {
        assert!(a.tick == b.tick && a.net == b.net && a.velocity == b.velocity);
        let k = h.section(a.tick - 1);
        if a.is_drum() || h.degrees[k] == 0 || h.group_index(&a.group).is_none() {
            assert_eq!(a.note, b.note);
        } else {
            assert_eq!(
                b.note,
                h.note(a.note, h.group_index(&a.group).unwrap(), a.tick - 1)
            );
            if a.note != b.note {
                changed += 1;
            }
        }
    }
    assert!(changed > 0);
    assert_eq!(all.1, home.1);
}

// MARK: section melody

struct BridgeSong {
    spec: TrackSpec,
    composed: Project,
}

impl BridgeSong {
    fn played(&self, fixes: Fixes) -> Project {
        apply(&self.composed, &self.spec, fixes)
    }

    /// The first standard / extended song with a bridge, a plan and at least one melody net to
    /// normalise.
    fn first() -> Option<&'static BridgeSong> {
        static CELL: OnceLock<Option<BridgeSong>> = OnceLock::new();
        CELL.get_or_init(|| {
            for structure in ["standard", "extended"] {
                for genre in BeatsTables::shared().genres.keys() {
                    let mut seeds = vec![42.0];
                    seeds.extend((1..=6).map(|s| s as f64));
                    for seed in seeds {
                        let spec = TrackSpec::new(genre, seed, Some(structure));
                        let composed = spec.project();
                        if !(composed.structure.iter().any(|s| s.name == "bridge")
                            && composed.cohesion.as_deref() == Some("v2")
                            && SectionHarmony::new(&composed).is_some())
                        {
                            continue;
                        }
                        let early = apply(
                            &composed,
                            &spec,
                            Fixes::KEY | Fixes::PHRASES | Fixes::TIMING,
                        );
                        let theme = build_track_theme(genre, seed);
                        if section_melody_shifts(&early, &theme)
                            .iter()
                            .any(|s| s.1 == 5)
                        {
                            return Some(BridgeSong { spec, composed });
                        }
                    }
                }
            }
            None
        })
        .as_ref()
    }
}

fn bound_notes(p: &Project) -> BTreeMap<String, BTreeMap<String, i64>> {
    p.nets
        .iter()
        .map(|(k, nb)| {
            (
                k.to_string(),
                nb.bindings
                    .iter()
                    .map(|(t, b)| (t.to_string(), b.note))
                    .collect(),
            )
        })
        .collect()
}

fn agreement(r: &MusicalityReport, name: &str) -> Option<f64> {
    r.section_chord_agreement_by_section
        .iter()
        .filter(|s| s.0 == name)
        .map(|s| s.1)
        .fold(None, |m: Option<f64>, v| {
            Some(m.map_or(v, |m| if v < m { v } else { m }))
        })
}

/// Only melody / arp slot nets that sound only in bridge or solo sections change.
#[test]
fn only_bridge_and_solo_melody_nets_change() {
    let song = BridgeSong::first().expect("a bridge song");
    let early = Fixes::KEY | Fixes::PHRASES | Fixes::TIMING;
    let before = song.played(early);
    let after = song.played(early | Fixes::SECTION_HARMONY);
    let theme = build_track_theme(&song.spec.genre, song.spec.seed);
    let moved = section_melody_shifts(&before, &theme);
    assert!(!moved.is_empty());
    let ids: BTreeMap<String, i64> = moved.into_iter().collect();
    let mut windows = slot_windows(&before, "melody");
    for (k, v) in slot_windows(&before, "arp") {
        windows.entry(k).or_insert(v);
    }
    let root = (before.root_note.unwrap_or(60) % 12 + 12) % 12;
    let name = if before.scale_name.is_empty() {
        "Major".to_string()
    } else {
        before.scale_name.clone()
    };
    let intervals = scale_intervals(&format!("{name}Scale"));
    assert_eq!(after.nets.keys(), before.nets.keys());
    for id in before.nets.keys() {
        let a = before.nets.get(id).unwrap();
        let b = after.nets.get(id).unwrap();
        assert_eq!(a.places.keys(), b.places.keys(), "{id} places");
        assert_eq!(
            a.transitions.keys(),
            b.transitions.keys(),
            "{id} transitions"
        );
        assert_eq!(a.arcs.len(), b.arcs.len(), "{id} arcs");
        assert_eq!(
            a.bindings.keys(),
            b.bindings.keys(),
            "{id} bound transitions"
        );
        let Some(&d) = ids.get(id) else {
            let notes = |n: &NetBundle| -> Vec<(String, i64)> {
                n.bindings
                    .iter()
                    .map(|(k, b)| (k.to_string(), b.note))
                    .collect()
            };
            assert_eq!(notes(a), notes(b), "{id} is not a bridge / solo melody net");
            continue;
        };
        assert!(["melody", "arp"].contains(&a.riff_group.as_str()), "{id}");
        let names: BTreeSet<&str> = windows
            .get(id)
            .map(|w| w.iter().map(|w| w.section_name.as_str()).collect())
            .unwrap_or_default();
        assert!(
            !names.is_empty() && names.iter().all(|n| ["bridge", "solo"].contains(n)),
            "{id} plays in {names:?}"
        );
        for t in a.bindings.keys() {
            let bd = a.bindings.get(t).unwrap();
            if is_drum_channel(bd.channel) {
                continue;
            }
            assert_eq!(
                b.bindings.get(t).unwrap().note,
                SectionHarmony::shift(bd.note, -d, root, intervals),
                "{id} {t}"
            );
        }
    }
    assert_eq!(
        after.structure.iter().map(|s| s.steps).collect::<Vec<_>>(),
        before.structure.iter().map(|s| s.steps).collect::<Vec<_>>()
    );
}

/// In key, moving a note back by a section's shift and on again is the note itself.
#[test]
fn shift_back_and_forth_is_identity() {
    for scale in ["Major", "Minor", "Dorian"] {
        let iv = scale_intervals(&format!("{scale}Scale"));
        for root in [0i64, 2, 9] {
            let key: BTreeSet<i64> = iv.iter().map(|i| (root + i) % 12).collect();
            for n in 24..=100 {
                if !key.contains(&(n % 12)) {
                    continue;
                }
                for d in [5i64, 4, -2, 2] {
                    let back = SectionHarmony::shift(n, -d, root, iv);
                    assert_eq!(SectionHarmony::shift(back, d, root, iv), n);
                }
            }
        }
    }
}

/// Nothing to do where the runner wouldn't move the band.
#[test]
fn leaves_other_tracks_alone() {
    let song = BridgeSong::first().expect("a bridge song");
    let early = Fixes::KEY | Fixes::PHRASES | Fixes::TIMING;
    let mut pentatonic = song.played(early);
    pentatonic.scale_name = "Pentatonic".into();
    let mut p = pentatonic.clone();
    normalise_section_melody(&mut p, &song.spec);
    assert_eq!(bound_notes(&p), bound_notes(&pentatonic));

    let mut v1 = song.played(early);
    v1.cohesion = None;
    p = v1.clone();
    normalise_section_melody(&mut p, &song.spec);
    assert_eq!(bound_notes(&p), bound_notes(&v1));

    let mut hooked = song.spec.clone();
    hooked
        .extra
        .set("hook", djrust::json::JsonValue::Bool(true));
    p = song.played(early);
    let q = p.clone();
    normalise_section_melody(&mut p, &hooked);
    assert_eq!(bound_notes(&p), bound_notes(&q));

    let loop_spec = TrackSpec::new(&song.spec.genre, song.spec.seed, None);
    let lp = apply(&loop_spec.project(), &loop_spec, early);
    p = lp.clone();
    normalise_section_melody(&mut p, &loop_spec);
    assert_eq!(bound_notes(&p), bound_notes(&lp));
}

/// The exit numbers: with the flag the bridge agrees with its chords ≥ 0.9 and the song has
/// ≥ 2 progressions; without it the bridge agrees no better and has fewer progressions.
#[test]
fn bridge_agrees_with_its_chords() {
    let song = BridgeSong::first().expect("a bridge song");
    let on = song.played(Fixes::ALL);
    let without = Fixes::ALL & !(Fixes::SECTION_HARMONY | Fixes::HARMONIC_RHYTHM);
    let off = song.played(without);
    let harmony = SectionHarmony::playing(&on, Fixes::ALL).expect("a plan");
    assert!(SectionHarmony::playing(&off, without).is_none());
    let (on_notes, on_controls) = capture_all_with(&on, None, Some(&harmony));
    let (off_notes, off_controls) = capture_all(&off, None);
    let with = MusicalityReport::new(&on, &on_notes, &on_controls, None);
    let plain = MusicalityReport::new(&off, &off_notes, &off_controls, None);
    let a = agreement(&with, "bridge").unwrap();
    let b = agreement(&plain, "bridge").unwrap();
    assert!(a >= 0.9);
    assert!(b <= a);
    assert!(with.progressions.unwrap_or(0) >= 2);
    assert!(with.progressions.unwrap_or(0) > plain.progressions.unwrap_or(0));
}

// MARK: harmonic rhythm

const HR_GENRES: [&str; 10] = [
    "trance",
    "techno",
    "house",
    "edm",
    "synthwave",
    "lofi",
    "country",
    "funk",
    "metal",
    "reggae",
];
const HR_SEEDS: [f64; 3] = [42.0, 7.0, 1234.0];

fn songs() -> &'static Vec<(String, Project, SectionHarmony)> {
    static CELL: OnceLock<Vec<(String, Project, SectionHarmony)>> = OnceLock::new();
    CELL.get_or_init(|| {
        let both = Fixes::SECTION_HARMONY | Fixes::HARMONIC_RHYTHM;
        let mut out = Vec::new();
        for structure in ["standard", "extended"] {
            for genre in HR_GENRES {
                for seed in HR_SEEDS {
                    let p = played(genre, seed, Some(structure));
                    if let Some(h) = SectionHarmony::with_fixes(&p, both) {
                        out.push((format!("{genre} {seed} {structure}"), p, h));
                    }
                }
            }
        }
        out
    })
}

/// The pitch class the band's chord root is on at `step`.
fn band_root(p: &Project, h: &SectionHarmony, step: i64) -> Option<i64> {
    let (chords, bars) = pad_chords(p);
    if bars <= 0 || h.groups.is_empty() {
        return None;
    }
    let r = *chords.get(&((step / 16) % bars))?.first()?;
    Some(h.note(48 + r, 0, step) % 12)
}

fn rise(from: i64, to: i64) -> i64 {
    ((to - from) % 12 + 12) % 12
}

fn section_end(h: &SectionHarmony, k: usize) -> i64 {
    if k + 1 < h.start.len() {
        h.start[k + 1]
    } else {
        h.end
    }
}

#[test]
fn flags() {
    let (_, p, _) = songs().first().expect("a song with a rhythm plan");
    assert!(SectionHarmony::with_fixes(p, Fixes::EMPTY).is_none());
    assert!(SectionHarmony::playing(p, Fixes::EMPTY).is_none());
    if let Some(old) = SectionHarmony::new(p) {
        assert_eq!(
            Some(&old),
            SectionHarmony::with_fixes(p, Fixes::SECTION_HARMONY).as_ref()
        );
        assert!(old.tail.iter().all(|t| *t == SectionHarmony::NO_TAIL));
    }
    let rhythm = SectionHarmony::with_fixes(p, Fixes::HARMONIC_RHYTHM).expect("rhythm plan");
    assert!(rhythm.degrees.iter().all(|d| *d == 0));
    assert_eq!(
        rhythm.tail.len() as i64,
        rhythm.start.len() as i64 * SectionHarmony::TAIL_SLOTS
    );
    assert_eq!(
        rhythm.slots.len() as i64,
        rhythm.start.len() as i64 * SectionHarmony::SLOTS_PER_CYCLE
    );
    assert_eq!(
        SectionHarmony::playing(p, Fixes::HARMONIC_RHYTHM),
        Some(rhythm)
    );
    assert_eq!(
        SectionHarmony::playing(p, Fixes::ALL),
        SectionHarmony::with_fixes(p, Fixes::ALL)
    );
}

/// Intros, breakdowns and outros change chord every 2 bars.
#[test]
fn holding_sections_change_every_two_bars() {
    let mut checked = 0;
    for (name, p, h) in songs() {
        let (_, pad_bars) = pad_chords(p);
        if !(pad_bars >= 2 && 4 % pad_bars == 0) {
            continue;
        }
        for (k, s) in p.structure.iter().enumerate() {
            if !SectionHarmony::HOLDING_SECTIONS.contains(&s.name.as_str()) {
                continue;
            }
            let end = section_end(h, k);
            let tailed = h.tail[k * 2..k * 2 + 2]
                .iter()
                .any(|t| *t != SectionHarmony::NO_TAIL);
            for bar in (h.start[k] / 16 + 1)..(end / 16) {
                if bar % 2 != 1 {
                    continue;
                }
                if tailed && bar == end / 16 - 1 {
                    continue;
                }
                if bar - 1 < h.start[k] / 16 {
                    continue;
                }
                let (Some(a), Some(b)) =
                    (band_root(p, h, (bar - 1) * 16), band_root(p, h, bar * 16))
                else {
                    continue;
                };
                assert_eq!(a, b, "{name} {} bar {bar}: {a} → {b}", s.name);
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

/// Intro bass: its notes in each odd bar are the even bar's chord's.
#[test]
fn intro_bass_changes_every_two_bars() {
    let (mut checked, mut played_n) = (0, 0);
    for (name, p, h) in songs() {
        let (chords, pad_bars) = pad_chords(p);
        let Some(bass) = h.group_index("bass") else {
            continue;
        };
        if !(pad_bars >= 2 && 4 % pad_bars == 0) {
            continue;
        }
        let log = capture_all_with(p, None, Some(h)).0;
        for (k, s) in p.structure.iter().enumerate() {
            if !SectionHarmony::HOLDING_SECTIONS.contains(&s.name.as_str()) {
                continue;
            }
            let end = section_end(h, k);
            let tailed = h.tail[k * 2..k * 2 + 2]
                .iter()
                .any(|t| *t != SectionHarmony::NO_TAIL);
            for bar in (h.start[k] / 16 + 1)..(end / 16) {
                if bar % 2 != 1 {
                    continue;
                }
                if tailed && bar == end / 16 - 1 {
                    continue;
                }
                if bar - 1 < h.start[k] / 16 {
                    continue;
                }
                let (Some(&a), Some(&b)) = (
                    chords.get(&((bar - 1) % pad_bars)).and_then(|c| c.first()),
                    chords.get(&(bar % pad_bars)).and_then(|c| c.first()),
                ) else {
                    continue;
                };
                let even = h.note(36 + a, bass, (bar - 1) * 16) % 12;
                let odd = h.note(36 + b, bass, bar * 16) % 12;
                assert_eq!(
                    even, odd,
                    "{name} {} bass plan bar {bar}: {even} → {odd}",
                    s.name
                );
                assert_eq!(
                    Some(odd),
                    band_root(p, h, bar * 16),
                    "{name} {} bar {bar}",
                    s.name
                );
                checked += 1;
            }
            // what a bass there plays
            let mut downbeat: BTreeMap<i64, i64> = BTreeMap::new();
            for n in log.iter().filter(|n| {
                n.group == "bass"
                    && !n.is_drum()
                    && (n.tick - 1) % 16 == 0
                    && n.tick - 1 >= h.start[k]
                    && n.tick - 1 < end
            }) {
                downbeat.entry((n.tick - 1) / 16).or_insert(n.note % 12);
            }
            for (&bar, &pc) in &downbeat {
                if bar % 2 != 1 {
                    continue;
                }
                if tailed && bar == end / 16 - 1 {
                    continue;
                }
                let (Some(&prev), Some(r0), Some(r1)) = (
                    downbeat.get(&(bar - 1)),
                    band_root(p, h, (bar - 1) * 16),
                    band_root(p, h, bar * 16),
                ) else {
                    continue;
                };
                if prev != r0 {
                    continue;
                }
                assert_eq!(r0, r1, "{name} {} bar {bar}", s.name);
                assert_eq!(pc, prev, "{name} {} bass bar {bar}: {prev} → {pc}", s.name);
                played_n += 1;
            }
        }
    }
    let _ = played_n;
    assert!(checked > 0);
}

/// A buildup's last half-bar is a cadence into the next section's first chord.
#[test]
fn buildup_ends_on_the_dominant() {
    let mut checked = 0;
    for (name, p, h) in songs() {
        for k in 0..p.structure.len() - 1 {
            if p.structure[k].name != "buildup" {
                continue;
            }
            let end = section_end(h, k);
            let (Some(last), Some(next)) = (band_root(p, h, end - 1), band_root(p, h, end)) else {
                continue;
            };
            assert!(
                [5, 2].contains(&rise(last, next)),
                "{name}: {last} → {next}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// The bar before a chorus changes chord twice, and its second half is a cadence into it.
#[test]
fn chorus_run_in_plays_two_chords() {
    let mut checked = 0;
    for (name, p, h) in songs() {
        for k in 0..p.structure.len() - 1 {
            if !(p.structure[k + 1].name == "chorus"
                && !["chorus", "drop"].contains(&p.structure[k].name.as_str()))
            {
                continue;
            }
            let end = section_end(h, k);
            let (Some(before), Some(first), Some(second), Some(chorus)) = (
                band_root(p, h, end - 17),
                band_root(p, h, end - 16),
                band_root(p, h, end - 8),
                band_root(p, h, end),
            ) else {
                continue;
            };
            assert_ne!(first, second, "{name}: no change at the half-bar");
            assert_ne!(before, first, "{name}: no change at the bar");
            assert!(
                [5, 2].contains(&rise(second, chorus)),
                "{name}: {second} → {chorus}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// Drops keep 1 chord per bar.
#[test]
fn drops_keep_one_chord_per_bar() {
    let mut checked = 0;
    for (name, p, h) in songs() {
        let old = SectionHarmony::with_fixes(p, Fixes::SECTION_HARMONY);
        for (k, s) in p.structure.iter().enumerate() {
            if s.name != "drop" {
                continue;
            }
            for bar in (h.start[k] / 16)..(section_end(h, k) / 16) {
                let (Some(a), Some(b)) = (band_root(p, h, bar * 16), band_root(p, h, bar * 16 + 8))
                else {
                    continue;
                };
                assert_eq!(a, b, "{name} drop bar {bar}");
                if let Some(old) = &old {
                    assert_eq!(
                        band_root(p, old, bar * 16),
                        Some(a),
                        "{name} drop bar {bar}"
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

/// Every in-key note stays in key at every half-bar.
#[test]
fn in_key_is_unchanged() {
    for (name, p, h) in songs().iter().take(8) {
        let key = pitch_classes(p);
        let mut step = 0;
        while step < h.end {
            for g in 0..h.groups.len() {
                for n in 0..128 {
                    let m = h.note(n, g, step);
                    assert!((0..128).contains(&m));
                    assert!(
                        !(key.contains(&(n % 12)) && !key.contains(&(m % 12))),
                        "{name} step {step}: {n} → {m}"
                    );
                }
            }
            step += SectionHarmony::SLOT_STEPS;
        }
    }
}

// MARK: slot windows

fn section_starts(p: &Project) -> Vec<i64> {
    let mut t = 0;
    p.structure
        .iter()
        .map(|s| {
            let r = t;
            t += s.steps;
            r
        })
        .collect()
}

/// Every window of `role`, in song order.
fn timeline(w: &BTreeMap<String, Vec<SlotWindow>>) -> Vec<(String, SlotWindow)> {
    let mut v: Vec<(String, SlotWindow)> = w
        .iter()
        .flat_map(|(n, ws)| ws.iter().map(move |x| (n.clone(), x.clone())))
        .collect();
    v.sort_by_key(|x| x.1.start);
    v
}

const WINDOW_GENRES: [&str; 8] = [
    "techno",
    "house",
    "edm",
    "trance",
    "dnb",
    "lofi",
    "jazz",
    "synthwave",
];

/// In what a deck plays, a role's windows tile every section it plays in, start on bars and
/// never overlap; the sections it sits out have none.
#[test]
fn windows_tile_each_section_on_bars() {
    for genre in WINDOW_GENRES {
        let p = played(genre, 42.0, Some("standard"));
        let starts = section_starts(&p);
        let mut checked = 0;
        for role in ["melody", "bass"] {
            let all = timeline(&slot_windows(&p, role));
            for (si, s) in p.structure.iter().enumerate() {
                let ws: Vec<_> = all.iter().filter(|x| x.1.section == si).collect();
                if !s.phrases.contains_key(role) {
                    assert!(
                        ws.is_empty(),
                        "{genre} {role}: windows in {}, which it sits out",
                        s.name
                    );
                    continue;
                }
                checked += 1;
                assert_eq!(
                    ws.first().map(|x| x.1.start),
                    Some(starts[si]),
                    "{genre} {role} {} starts late",
                    s.name
                );
                assert_eq!(
                    ws.last().map(|x| x.1.end),
                    Some(starts[si] + s.steps),
                    "{genre} {role} {} ends early",
                    s.name
                );
                for w in ws.windows(2) {
                    assert_eq!(
                        w[0].1.end, w[1].1.start,
                        "{genre} {role} {}: gap or overlap at {}",
                        s.name, w[0].1.end
                    );
                }
                for x in &ws {
                    assert_eq!(
                        x.1.start % 16,
                        0,
                        "{genre} {role}: {} starts at step {}",
                        x.0,
                        x.1.start
                    );
                    assert!(x.1.end > x.1.start);
                    assert_eq!(x.1.section_name, s.name);
                    assert!(x.1.phrase < s.phrases.get(role).unwrap().len());
                }
            }
        }
        assert!(checked > 0, "{genre}: no role played anywhere");
    }
}

/// The composed track still tiles each section, from the slot's first change.
#[test]
fn composed_windows_tile() {
    for genre in ["techno", "lofi"] {
        let p = TrackSpec::new(genre, 42.0, Some("standard")).project();
        let starts = section_starts(&p);
        let all = timeline(&slot_windows(&p, "melody"));
        assert!(!all.is_empty());
        for (si, s) in p.structure.iter().enumerate() {
            if !s.phrases.contains_key("melody") {
                continue;
            }
            let ws: Vec<_> = all.iter().filter(|x| x.1.section == si).collect();
            let first = ws.first().map_or(-1, |x| x.1.start);
            assert!(
                first >= starts[si]
                    && first <= starts[si] + djrust::gen::structure::BOUNDARY_STAGGER + 1,
                "{genre} {}",
                s.name
            );
            assert_eq!(ws.last().map(|x| x.1.end), Some(starts[si] + s.steps));
            for w in ws.windows(2) {
                assert_eq!(w[0].1.end, w[1].1.start);
                assert!(w[0].1.phrase <= w[1].1.phrase);
            }
            assert_eq!(
                ws.last().map(|x| x.1.phrase),
                Some(s.phrases.get("melody").unwrap().len() - 1),
                "{genre} {}",
                s.name
            );
        }
    }
}

/// `ringStart` is where the net's token really sits at `start`, by simulating the nets, and
/// equals the phase `NoteLog` reports for the switch that opens a window.
#[test]
fn ring_start_is_where_the_token_sits() {
    for genre in ["techno", "trance", "lofi"] {
        let p = played(genre, 42.0, Some("standard"));
        let windows = slot_windows(&p, "melody");
        let mut g = WaveGraph::new(&p);
        let mut wanted: BTreeMap<i64, Vec<(String, i64)>> = BTreeMap::new();
        for (net, ws) in &windows {
            for w in ws {
                wanted
                    .entry(w.start)
                    .or_default()
                    .push((net.clone(), w.ring_start.expect("ring start")));
            }
        }
        let mut seen = 0;
        let check = |g: &WaveGraph, step: i64, seen: &mut usize| {
            for (net, ring) in wanted.get(&step).cloned().unwrap_or_default() {
                let n = &g.nets[g.by_id[&net]];
                assert_eq!(ring_position(n), Some(ring), "{genre} {net} at step {step}");
                *seen += 1;
            }
        };
        check(&g, 0, &mut seen);
        for step in 1..length(&p) {
            g.advance();
            check(&g, step, &mut seen);
        }
        assert_eq!(seen, windows.values().map(|w| w.len()).sum::<usize>());
        assert!(seen > 0);
        for c in capture_all(&p, None)
            .1
            .iter()
            .filter(|c| c.action == ControlAction::Slot && windows.contains_key(&c.target))
        {
            let w = windows[&c.target]
                .iter()
                .find(|w| w.start == c.tick)
                .unwrap_or_else(|| panic!("{}", c.target));
            assert_eq!(w.ring_start, c.phase);
        }
    }
}

/// The spike for 6.4: bars 5–8 of an 8-bar section are always a different net from bars 1–4.
#[test]
fn eight_bar_section_halves_are_two_nets() {
    let p = played("edm", 42.0, Some("extended"));
    let starts = section_starts(&p);
    let all = timeline(&slot_windows(&p, "melody"));
    let mut rings: Vec<String> = Vec::new();
    for (si, s) in p.structure.iter().enumerate() {
        if !(s.steps == 128 && s.phrases.get("melody").map(|p| p.len()) == Some(2)) {
            continue;
        }
        let ws: Vec<_> = all.iter().filter(|x| x.1.section == si).collect();
        assert_eq!(
            ws.len(),
            2,
            "{}: {:?}",
            s.name,
            ws.iter().map(|w| &w.0).collect::<Vec<_>>()
        );
        assert_ne!(ws[0].0, ws[1].0);
        assert!(ws[0].1.start == starts[si] && ws[0].1.end == starts[si] + 64);
        assert!(ws[1].1.start == starts[si] + 64 && ws[1].1.end == starts[si] + 128);
        assert_eq!(
            ws.iter().map(|w| w.1.phrase).collect::<Vec<_>>(),
            vec![0, 1]
        );
        for x in &ws {
            let length = x.1.ring_length.unwrap();
            assert_eq!(x.1.ring_start, Some(x.1.start % length));
        }
        rings.push(format!(
            "{} {}",
            s.name,
            ws.iter()
                .map(|w| format!("{}/{}", w.1.ring_start.unwrap(), w.1.ring_length.unwrap()))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    assert_eq!(
        rings,
        vec![
            "buildup 0/64 0/64",
            "breakdown 64/128 0/128",
            "buildup 0/64 0/64",
            "bridge 0/64 0/64",
            "outro 0/64 0/64"
        ]
    );
}

/// A 2-bar phrase plays the second half of its 4-bar ring.
#[test]
fn two_bar_phrases_start_mid_ring() {
    let p = played("techno", 42.0, Some("standard"));
    let starts = section_starts(&p);
    let si = p.structure.iter().position(|s| s.name == "drop").unwrap();
    let ws: Vec<_> = timeline(&slot_windows(&p, "melody"))
        .into_iter()
        .filter(|x| x.1.section == si)
        .collect();
    assert_eq!(
        ws.iter()
            .map(|w| w.1.start - starts[si])
            .collect::<Vec<_>>(),
        vec![0, 64, 128, 160]
    );
    assert_eq!(
        ws.iter().map(|w| w.1.phrase).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        ws.iter().map(|w| w.1.ring_start).collect::<Vec<_>>(),
        vec![Some(0), Some(0), Some(0), Some(32)]
    );
}

/// Simulating only the control nets acting on the role sees the same part changes as the
/// whole song.
#[test]
fn part_only_sees_the_songs_changes() {
    for genre in ["techno", "trance", "lofi", "jazz"] {
        let p = played(genre, 42.0, Some("standard"));
        for role in ["melody", "bass"] {
            let ids: BTreeSet<&str> = p
                .nets
                .iter()
                .filter(|(_, nb)| nb.riff_group == role)
                .map(|(k, _)| k)
                .collect();
            let full: Vec<_> = capture_all(&p, None)
                .1
                .into_iter()
                .filter(|c| ids.contains(c.target.as_str()))
                .collect();
            let part: Vec<_> = capture_all(&part_only(&p, &ids), Some(length(&p)))
                .1
                .into_iter()
                .filter(|c| ids.contains(c.target.as_str()))
                .collect();
            assert!(!full.is_empty() || ids.len() <= 1, "{genre} {role}");
            assert_eq!(
                part.iter().map(|c| c.tick).collect::<Vec<_>>(),
                full.iter().map(|c| c.tick).collect::<Vec<_>>(),
                "{genre} {role}"
            );
            assert_eq!(
                part.iter().map(|c| &c.target).collect::<Vec<_>>(),
                full.iter().map(|c| &c.target).collect::<Vec<_>>()
            );
            assert_eq!(
                part.iter().map(|c| c.action).collect::<Vec<_>>(),
                full.iter().map(|c| c.action).collect::<Vec<_>>()
            );
            assert_eq!(
                part.iter().map(|c| &c.net).collect::<Vec<_>>(),
                full.iter().map(|c| &c.net).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn no_windows_for_an_unknown_role_or_a_loop() {
    assert!(slot_windows(&played("techno", 42.0, Some("standard")), "nope").is_empty());
    let loop_p = TrackSpec::new("techno", 42.0, None).project();
    assert!(slot_windows(&loop_p, "melody").is_empty());
}

// MARK: part changes on a section boundary (raw nets)

/// A 16-step ring of `group` with a note on every beat.
fn beat_ring(group: &str, channel: i64, note: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for s in [0, 4, 8, 12] {
        n.bind(s, note, channel, 100, 200);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n.riff_group = group.into();
    n
}

/// A control net firing `action` on `target` at song step `step`.
fn control_at(step: i64, action: &str, target: &str, steps: i64) -> NetBundle {
    let mut m = BTreeMap::new();
    m.insert(step, ControlBinding::new(action, target));
    build_control_bundle(steps, &m, true, "p", "t")
}

fn boundary_project(controls_first: bool) -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    p.structure = vec![
        Section {
            name: "drop".into(),
            steps: 32,
            phrases: Default::default(),
        },
        Section {
            name: "breakdown".into(),
            steps: 32,
            phrases: Default::default(),
        },
    ];
    let controls = [
        ("ctl-bass", control_at(32, "mute-track", "bass", 64)),
        ("ctl-melody", control_at(32, "mute-track", "melody", 64)),
        ("ctl-harmony", control_at(32, "unmute-track", "harmony", 64)),
    ];
    if controls_first {
        for (id, nb) in &controls {
            p.nets.insert(*id, nb.clone());
        }
    }
    p.nets.insert("bass", beat_ring("bass", 6, 33));
    p.nets.insert("melody", beat_ring("melody", 1, 72));
    p.nets.insert("harmony", beat_ring("harmony", 2, 60));
    if !controls_first {
        for (id, nb) in &controls {
            p.nets.insert(*id, nb.clone());
        }
    }
    p.initial_mutes = vec!["harmony".into()];
    p
}

/// The steps (from 0) each part's notes actually play on.
fn played_steps(p: &Project) -> BTreeMap<String, BTreeSet<i64>> {
    let mut out: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for n in capture(p, None) {
        out.entry(n.net).or_default().insert(n.tick - 1);
    }
    out
}

/// A part muted / unmuted on a section's first step is attributed to the sections it really
/// plays in, step for step, whichever side of the parts its control net sits.
#[test]
fn boundary_changes_land_in_the_right_section() {
    for controls_first in [true, false] {
        let p = boundary_project(controls_first);
        let truth = played_steps(&p);
        for role in ["bass", "melody", "harmony"] {
            let ws = slot_windows(&p, role).remove(role).unwrap_or_default();
            let covered: BTreeSet<i64> = ws.iter().flat_map(|w| w.start..w.end).collect();
            for s in truth.get(role).cloned().unwrap_or_default() {
                assert!(
                    covered.contains(&s),
                    "{role} (controls first: {controls_first}): plays step {s} outside its windows"
                );
            }
            for w in &ws {
                assert!(
                    (w.start..w.end).any(|s| truth.get(role).map_or(false, |t| t.contains(&s))),
                    "{role} (controls first: {controls_first}): window {}..{} plays no note",
                    w.start,
                    w.end
                );
            }
        }
        if controls_first {
            // The change acts on step 32 itself.
            assert!(!truth["bass"].contains(&32));
            assert_eq!(truth["harmony"].iter().min(), Some(&32));
            let sec = |role: &str| -> Vec<usize> {
                slot_windows(&p, role)
                    .remove(role)
                    .unwrap()
                    .iter()
                    .map(|w| w.section)
                    .collect()
            };
            assert_eq!(sec("bass"), vec![0]);
            assert_eq!(sec("melody"), vec![0]);
            assert_eq!(sec("harmony"), vec![1]);
        }
    }
}

/// beats puts every control net after the parts, so in a composed (and a played) track every
/// change acts from the step its tick names.
#[test]
fn composed_changes_act_on_their_tick() {
    for genre in ["techno", "lofi", "edm"] {
        let p = played(genre, 42.0, Some("standard"));
        let controls = capture_all(&p, None).1;
        assert!(!controls.is_empty());
        assert!(controls.iter().all(|c| c.step == c.tick), "{genre}");
    }
}

/// `.layerBudget` on that breakdown (budget 1) keeps the one part that actually plays there.
#[test]
fn layer_budget_keeps_the_part_that_plays() {
    let p = boundary_project(true);
    let b = layer_budgets(&p);
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].playing, vec![Class::of("tonal", "harmony", "harmony")]);
    assert!(b[1].rested().is_empty());
}
