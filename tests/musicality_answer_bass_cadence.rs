//! Ports of Swift `AnswerTests`, `BassMotifTests` and `HarmonicCadenceTests`.

use djrust::gen::tables::BeatsTables;
use djrust::musicality::answer::answer_phrases;
use djrust::musicality::bass_motif::restate_bass_rhythm;
use djrust::musicality::dj::{diatonic, pad_chords, BASS_FLOOR};
use djrust::musicality::harmonic_cadence::{cadences_from_flat_seven, CADENCE_RISES};
use djrust::musicality::note_log::{capture, capture_all};
use djrust::musicality::report::{is_tonal_event, MusicalityReport};
use djrust::musicality::slot_windows::slot_windows;
use djrust::musicality::track_variety::seven_note_key;
use djrust::musicality::util::{is_bar_ring, key_pitch_classes, part, pc, ring_notes};
use djrust::musicality::{apply, Fixes};
use djrust::pflow::{NetBundle, Project, Section};
use djrust::TrackSpec;

fn ring(n: i64, group: &str, channel: i64, notes: &[(i64, i64)]) -> NetBundle {
    let mut nb = NetBundle::ring(n);
    for (step, note) in notes {
        nb.bind(*step, *note, channel, 100, 100);
    }
    nb.track.channel = channel;
    nb.track.group = Some(group.into());
    nb.riff_group = group.into();
    nb
}

/// A one-bar pad of `notes`, one net per note (a binding plays one note).
fn pad(p: &mut Project, notes: &[i64]) {
    for (i, n) in notes.iter().enumerate() {
        p.nets
            .insert(format!("pad{i}"), ring(16, "harmony", 4, &[(0, *n)]));
    }
}

/// C major, a C-major pad every bar, and one 8-bar melody ring: bars 1–4 end on C (closed
/// already), bars 5–8 on E.
fn pair_project() -> Project {
    let mut p = Project::default();
    p.root_note = Some(60);
    p.scale_name = "Major".into();
    pad(&mut p, &[48, 52, 55]);
    p.nets.insert(
        "melody",
        ring(
            128,
            "melody",
            1,
            &[
                (0, 72),
                (4, 74),
                (16, 72),
                (20, 74), // bars 1–2: the idea
                (36, 67),
                (56, 72), // question ends on the tonic
                (64, 76),
                (72, 74),
                (80, 76),
                (88, 74),
                (100, 71),
                (120, 76), // answer ends on E
            ],
        ),
    );
    let mut s = Section {
        name: "verse".into(),
        steps: 128,
        phrases: Default::default(),
    };
    s.phrases.insert("melody", vec!["A".into()]);
    p.structure = vec![s];
    p
}

fn played(genre: &str, seed: f64, fixes: Fixes) -> Project {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    apply(&spec.project(), &spec, fixes)
}

fn report(p: &Project) -> MusicalityReport {
    let (notes, controls) = capture_all(p, None);
    MusicalityReport::new(p, &notes, &controls, None)
}

/// The section 8-bar pairs whose halves both end in bars 3–4 on the melody's slot nets, and
/// of them the share that end open (not the tonic) then closed (the tonic).
fn section_answers(p: &Project) -> (usize, f64) {
    let nets: std::collections::BTreeSet<String> =
        slot_windows(p, "melody").keys().cloned().collect();
    let log: Vec<_> = capture(p, None)
        .into_iter()
        .filter(|e| nets.contains(&e.net) && is_tonal_event(e))
        .collect();
    let tonic = pc(p.root_note.unwrap_or(60));
    let (mut pairs, mut answered) = (0usize, 0usize);
    let mut start = 0;
    for s in &p.structure {
        let mut pair = start;
        while pair <= start + s.steps - 128 {
            let last = |a: i64| {
                let mut best: Option<&djrust::musicality::note_log::NoteEvent> = None;
                for e in log
                    .iter()
                    .filter(|e| e.tick - 1 >= a && e.tick - 1 < a + 64)
                {
                    if best.map_or(true, |b| b.tick < e.tick) {
                        best = Some(e);
                    }
                }
                best
            };
            if let (Some(q), Some(a)) = (last(pair), last(pair + 64)) {
                if q.tick - 1 - pair >= 32 && a.tick - 1 - pair - 64 >= 32 {
                    pairs += 1;
                    if pc(q.note) != tonic && pc(a.note) == tonic {
                        answered += 1;
                    }
                }
            }
            pair += 128;
        }
        start += s.steps;
    }
    (
        pairs,
        if pairs > 0 {
            answered as f64 / pairs as f64
        } else {
            0.0
        },
    )
}

fn run_answer(p: &Project, spec: &TrackSpec) -> Project {
    let mut q = p.clone();
    answer_phrases(
        &mut q,
        BeatsTables::shared().genres.get(&spec.genre).unwrap(),
        spec,
    );
    q
}

/// The question's last note goes to the chord's fifth, the answer's to the tonic; every other
/// binding stays.
#[test]
fn question_ends_open_answer_closes() {
    let p = pair_project();
    let spec = TrackSpec::new("techno", 42.0, None);
    let q = apply(&p, &spec, Fixes::ANSWER);
    let mb = &q.nets.get("melody").unwrap().bindings;
    assert_eq!(mb.get("t56").unwrap().note, 67);
    assert_eq!(mb.get("t120").unwrap().note, 72);
    for (k, b) in p.nets.get("melody").unwrap().bindings.iter() {
        if k != "t56" && k != "t120" {
            assert_eq!(mb.get(k).unwrap().note, b.note, "{k} moved");
        }
    }
    let pads: Vec<i64> = (0..3)
        .map(|i| {
            q.nets
                .get(&format!("pad{i}"))
                .unwrap()
                .bindings
                .get("t0")
                .unwrap()
                .note
        })
        .collect();
    assert_eq!(pads, vec![48, 52, 55]);
    // the report's per-net pair agrees
    assert_eq!(report(&p).answer_phrases, Some(0.0));
    assert_eq!(report(&q).answer_phrases, Some(1.0));
}

/// Where the fifth is the tonic (an F chord in C), the question takes the chord's third.
#[test]
fn question_avoids_the_tonic() {
    let mut p = pair_project();
    pad(&mut p, &[53, 57, 60]);
    p.nets
        .get_mut("melody")
        .unwrap()
        .bindings
        .get_mut("t56")
        .unwrap()
        .note = 74; // D
    let q = apply(&p, &TrackSpec::new("techno", 42.0, None), Fixes::ANSWER);
    assert_eq!(
        q.nets
            .get("melody")
            .unwrap()
            .bindings
            .get("t56")
            .unwrap()
            .note,
        69
    );
}

/// A section shorter than 8 bars, or a pair whose half ends in bars 1–2, is left alone.
#[test]
fn short_sections_and_early_ends_stay() {
    let notes = |p: &Project| -> Vec<i64> {
        p.nets
            .get("melody")
            .unwrap()
            .bindings
            .values()
            .map(|b| b.note)
            .collect()
    };
    let spec = TrackSpec::new("techno", 42.0, None);
    let mut p = pair_project();
    p.structure[0].steps = 112;
    assert_eq!(notes(&apply(&p, &spec, Fixes::ANSWER)), notes(&p));
    p = pair_project();
    p.nets.get_mut("melody").unwrap().bindings.remove("t36");
    p.nets.get_mut("melody").unwrap().bindings.remove("t56");
    assert_eq!(notes(&apply(&p, &spec, Fixes::ANSWER)), notes(&p));
    let _ = run_answer;
}

/// On what decks play: ≥ 0.9 of a section's 8-bar pairs answer with the flag, < 0.5 without;
/// the idea stays, and so do the key and the clashes.
#[test]
fn played_tracks_answer() {
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "dnb",
        "synthwave",
        "lofi",
        "funk",
        "jazz",
    ] {
        for seed in [7.0, 42.0] {
            let with = played(genre, seed, Fixes::ALL);
            let without = played(genre, seed, Fixes::ALL & !Fixes::ANSWER);
            let a = section_answers(&with);
            let b = section_answers(&without);
            if a.0 == 0 {
                continue;
            }
            assert!(a.1 >= 0.9, "{genre}/{seed}: {} → {}", b.1, a.1);
            assert!(b.1 < 0.5, "{genre}/{seed}: already answers ({})", b.1);
            let rw = report(&with);
            let ro = report(&without);
            if let Some(x) = rw.answer_phrases {
                assert!(x >= ro.answer_phrases.unwrap_or(0.0), "{genre}/{seed}");
            }
            assert_eq!(
                rw.melody_idea_repeats, ro.melody_idea_repeats,
                "{genre}/{seed}: idea moved"
            );
            assert!(rw.in_key >= ro.in_key - 0.005, "{genre}/{seed}");
            assert!(
                rw.semitone_clashes <= ro.semitone_clashes + 0.5,
                "{genre}/{seed}"
            );
        }
    }
}

/// The pass rewrites bindings only.
#[test]
fn answer_bindings_only() {
    let with = played("trance", 42.0, Fixes::ALL);
    let without = played("trance", 42.0, Fixes::ALL & !Fixes::ANSWER);
    assert_eq!(with.nets.keys(), without.nets.keys());
    for (id, nb) in with.nets.iter() {
        let o = without.nets.get(id).unwrap();
        assert!(nb.places.len() == o.places.len() && nb.transitions.len() == o.transitions.len());
        assert_eq!(nb.arcs.len(), o.arcs.len());
    }
}

// MARK: bass motif

const FIGURE_GENRES: [&str; 11] = [
    "techno",
    "house",
    "edm",
    "trance",
    "dnb",
    "dubstep",
    "synthwave",
    "funk",
    "reggae",
    "garage",
    "metal",
];

/// Bar 2 of each 4-bar bass phrase plays bar 1's rhythm, still starting on the chord's root as
/// often as without the pass, every note in key.
#[test]
fn bar_two_echoes_bar_one() {
    for genre in FIGURE_GENRES {
        for seed in [7.0, 42.0] {
            let plain = report(&played(genre, seed, Fixes::ALL & !Fixes::BASS_MOTIF));
            let p = played(genre, seed, Fixes::ALL);
            let r = report(&p);
            if let Some(b) = r.bass_idea_repeats {
                assert!(
                    b >= 0.95,
                    "{genre}/{seed}: bidea {:?} → {b}",
                    plain.bass_idea_repeats
                );
            }
            if let Some(before) = plain.bass_on_root {
                let after = r.bass_on_root.expect("bassOnRoot");
                assert!(
                    after >= before,
                    "{genre}/{seed}: bass on root {before} → {after}"
                );
            }
            let key = key_pitch_classes(&p);
            assert!(
                capture(&p, None)
                    .iter()
                    .filter(|e| e.group == "bass")
                    .all(|e| key.contains(&pc(e.note))),
                "{genre}/{seed}"
            );
            assert!(r.in_key >= plain.in_key - 0.001, "{genre}/{seed}: in key");
            assert_eq!(r.harmony_in_bass_range, Some(0.0), "{genre}/{seed}");
        }
    }
}

/// Walking and bossa basses are the genre's figure: the pass leaves the song as it was.
#[test]
fn walking_and_bossa_basses_stay() {
    for genre in ["jazz", "blues", "lofi", "bossa"] {
        for seed in [7.0, 42.0] {
            assert_eq!(
                played(genre, seed, Fixes::ALL).canonical_json(),
                played(genre, seed, Fixes::ALL & !Fixes::BASS_MOTIF).canonical_json(),
                "{genre}/{seed}"
            );
        }
    }
}

/// On a hand-set ring.
#[test]
fn bar_two_takes_bar_ones_figure_and_keeps_the_pickup() {
    let spec = TrackSpec::new("techno", 42.0, None);
    let composed = spec.project();
    let mut p = apply(
        &composed,
        &spec,
        Fixes::ALL & !(Fixes::BASS_MOTIF | Fixes::APPROACH),
    );
    let genre = BeatsTables::shared().genres.get("techno").unwrap();
    let id = p
        .nets
        .keys()
        .iter()
        .find(|id| {
            let nb = p.nets.get(id).unwrap();
            is_bar_ring(nb)
                && part(nb) == "bass"
                && nb.transitions.len() % 64 == 0
                && !ring_notes(nb).is_empty()
        })
        .cloned()
        .expect("a bass ring");
    let mut nb = p.nets.get(&id).unwrap().clone();
    let template = *nb.bindings.get(&ring_notes(&nb)[0].key).unwrap();
    let (chords, pad_bars) = pad_chords(&p);
    assert!(pad_bars > 0);
    let scale = seven_note_key(&p).unwrap().0;
    // each bar's chord root, in the bass's room
    let root = |bar: i64| -> i64 {
        let r = chords[&(bar % pad_bars)][0];
        (BASS_FLOOR..BASS_FLOOR + 12).find(|n| pc(*n) == r).unwrap()
    };
    let (r1, r2) = (root(0), root(1));
    let fifth = diatonic(r1, 4, &scale).unwrap();
    for k in nb.bindings.keys().to_vec() {
        nb.bindings.remove(&k);
    }
    let put = |nb: &mut NetBundle, step: i64, note: i64| {
        let mut b = template;
        b.note = note;
        nb.bindings.insert(format!("t{step}"), b);
    };
    put(&mut nb, 0, r1);
    put(&mut nb, 6, r1);
    put(&mut nb, 10, fifth);
    put(&mut nb, 16, r2);
    put(&mut nb, 24, r2);
    put(&mut nb, 30, r2);
    put(&mut nb, 32, root(2));
    put(&mut nb, 48, root(3));
    p.nets.insert(id.clone(), nb);
    restate_bass_rhythm(&mut p, genre, &spec);
    let out = ring_notes(p.nets.get(&id).unwrap());
    let bar = |b: i64| -> Vec<_> { out.iter().filter(|s| s.step / 16 == b).cloned().collect() };
    assert_eq!(
        bar(0).iter().map(|s| s.step % 16).collect::<Vec<_>>(),
        vec![0, 6, 10, 14]
    );
    assert_eq!(
        bar(1).iter().map(|s| s.step % 16).collect::<Vec<_>>(),
        vec![0, 6, 10, 14]
    );
    // bar 2 from its root, the same scale steps as bar 1 (the fifth may fold an octave down)
    assert_eq!(bar(1)[0].note, r2);
    assert_eq!(bar(1)[1].note, r2);
    let up = diatonic(r2, 4, &scale).unwrap();
    assert!([up, up - 12].contains(&bar(1)[2].note));
    // the pickup stays; bar 1's is the same steps from its root
    assert_eq!(bar(1)[3].note, r2);
    assert_eq!(bar(0)[3].note, r1);
    assert_eq!(bar(2).iter().map(|s| s.step).collect::<Vec<_>>(), vec![32]);
    assert_eq!(bar(3).iter().map(|s| s.step).collect::<Vec<_>>(), vec![48]);
}

/// Bar 2 already on bar 1's rhythm: nothing to restate.
#[test]
fn a_repeated_rhythm_stays_as_composed() {
    let spec = TrackSpec::new("techno", 42.0, None);
    let composed = apply(
        &spec.project(),
        &spec,
        Fixes::ALL & !(Fixes::BASS_MOTIF | Fixes::VARIETY),
    );
    let mut p = composed.clone();
    restate_bass_rhythm(
        &mut p,
        BeatsTables::shared().genres.get("techno").unwrap(),
        &spec,
    );
    for (id, nb) in composed.nets.iter() {
        if part(nb) != "bass" {
            continue;
        }
        let before = ring_notes(nb);
        let rhythm = |bar: i64| -> std::collections::BTreeSet<i64> {
            before
                .iter()
                .filter(|s| s.step / 16 == bar)
                .map(|s| s.step % 16)
                .collect()
        };
        if !is_bar_ring(nb) || rhythm(0) != rhythm(1) {
            continue;
        }
        let after = ring_notes(p.nets.get(id).unwrap());
        let phrase = |x: &[djrust::musicality::util::StepNote]| -> Vec<(i64, i64)> {
            x.iter()
                .filter(|s| s.step < 32)
                .map(|s| (s.step, s.note))
                .collect()
        };
        assert_eq!(phrase(&after), phrase(&before), "{id}");
    }
}

/// A pure function of (project, spec).
#[test]
fn bass_motif_deterministic() {
    for genre in ["techno", "dnb", "house"] {
        assert_eq!(
            played(genre, 4242.0, Fixes::ALL).canonical_json(),
            played(genre, 4242.0, Fixes::ALL).canonical_json()
        );
    }
}

// MARK: harmonic cadence

const CAD_GENRES: [&str; 8] = [
    "techno",
    "house",
    "trance",
    "edm",
    "dnb",
    "jazz",
    "synthwave",
    "funk",
];
const CAD_SEEDS: [f64; 3] = [42.0, 7.0, 1234.0];

/// The semitones the pad cycle's last bar's root rises into bar 0's; None without both chords.
fn last_bar_rise(p: &Project) -> Option<i64> {
    let (chords, bars) = pad_chords(p);
    if bars < 2 {
        return None;
    }
    let last = *chords.get(&(bars - 1))?.first()?;
    let first = *chords.get(&0)?.first()?;
    Some(pc(first - last))
}

/// Every 4-bar line of the song is a harmonic cadence (or nearly: ≥ 0.9).
#[test]
fn phrase_ends_are_cadences() {
    for genre in CAD_GENRES {
        for seed in CAD_SEEDS {
            let r = report(&played(genre, seed, Fixes::ALL));
            let cad = r
                .harmonic_cadences
                .unwrap_or_else(|| panic!("{genre} {seed}"));
            assert!(cad >= 0.9, "{genre} {seed}: harmonicCadences {cad}");
        }
    }
}

/// The resolved chord is voice-led like the rest, and moving it does not add semitone rubs.
#[test]
fn pad_motion_and_rubs_hold() {
    for genre in CAD_GENRES {
        for seed in CAD_SEEDS {
            let on = report(&played(genre, seed, Fixes::ALL));
            let off = report(&played(genre, seed, Fixes::ALL & !Fixes::HARMONIC_CADENCE));
            if on.pad_motion.is_some() || off.pad_motion.is_some() {
                let motion = on.pad_motion.unwrap_or_else(|| panic!("{genre} {seed}"));
                let off_motion = off.pad_motion.unwrap_or_else(|| panic!("{genre} {seed}"));
                assert!(
                    motion <= 4.0f64.max(off_motion + 1.6),
                    "{genre} {seed}: padMotion {off_motion} → {motion}"
                );
            }
            assert!(
                on.semitone_clashes <= off.semitone_clashes + 0.3,
                "{genre} {seed}: rubs {} → {}",
                off.semitone_clashes,
                on.semitone_clashes
            );
        }
    }
}

/// Pentatonic and blues genres keep their plan.
#[test]
fn pentatonic_genres_are_unchanged() {
    for genre in ["ambient", "lofi", "blues"] {
        for seed in CAD_SEEDS {
            assert_eq!(
                played(genre, seed, Fixes::ALL).canonical_json(),
                played(genre, seed, Fixes::ALL & !Fixes::HARMONIC_CADENCE).canonical_json(),
                "{genre} {seed}"
            );
        }
    }
}

/// On the pass's own output: a last bar that already resolves stays exactly as it was; one
/// that did not now rises a fourth or a whole step into bar 0.
#[test]
fn last_bar_resolves_or_stays() {
    for genre in CAD_GENRES {
        let def = BeatsTables::shared().genres.get(genre).unwrap();
        for seed in CAD_SEEDS {
            let spec = TrackSpec::new(genre, seed, Some("standard"));
            let composed = spec.project();
            let before = apply(&composed, &spec, Fixes::VARIETY);
            let after = apply(&composed, &spec, Fixes::VARIETY | Fixes::HARMONIC_CADENCE);
            let rise = last_bar_rise(&before).unwrap_or_else(|| panic!("{genre} {seed}"));
            if CADENCE_RISES.contains(&rise) {
                assert_eq!(
                    after.canonical_json(),
                    before.canonical_json(),
                    "{genre} {seed}: rise {rise}"
                );
                continue;
            }
            let now = last_bar_rise(&after).unwrap();
            assert!(
                CADENCE_RISES.contains(&now),
                "{genre} {seed}: rise {rise} → {now}"
            );
            let (a, bars) = pad_chords(&before);
            let (b, _) = pad_chords(&after);
            if cadences_from_flat_seven(def)
                && a.get(&0).and_then(|c| c.first()).copied()
                    == Some(pc(after.root_note.unwrap_or(60)))
            {
                assert_eq!(
                    now, 2,
                    "{genre} {seed}: ♭VII → i in a minor dance genre, rose {now}"
                );
            }
            for bar in 0..bars {
                if bar % 4 != 3 {
                    assert_eq!(a.get(&bar), b.get(&bar), "{genre} {seed} bar {bar}");
                }
            }
            for id in composed.nets.keys() {
                let x = composed.nets.get(id).unwrap();
                let y = after.nets.get(id).unwrap();
                assert_eq!(x.places.keys(), y.places.keys(), "{genre} {id} places");
                assert_eq!(
                    x.transitions.keys(),
                    y.transitions.keys(),
                    "{genre} {id} transitions"
                );
                assert_eq!(x.arcs.len(), y.arcs.len(), "{genre} {id} arcs");
            }
        }
    }
}

#[test]
fn flag_is_registered() {
    assert!(Fixes::ALL.contains(Fixes::HARMONIC_CADENCE));
    assert_eq!(
        Fixes::named("harmonicCadence"),
        Some(Fixes::HARMONIC_CADENCE)
    );
}
