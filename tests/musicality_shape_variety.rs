//! Ports of Swift `MelodyShapeTests`, `MelodyBigramsTests` (the table contract) and
//! `VarietyTests` (`.shape`, `.variety`, the seeded `.arpeggio`).

use std::collections::{BTreeMap, BTreeSet};

use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::musicality::dj::{pad_chords, ArpFigure};
use djrust::musicality::melody_bigrams::{MelodyBigrams, MELODY_BIGRAMS_JSON};
use djrust::musicality::melody_shape::shape_melody;
use djrust::musicality::note_log::{capture, capture_all};
use djrust::musicality::report::{round3, MusicalityReport};
use djrust::musicality::synth_voice::KickTuning;
use djrust::musicality::track_variety::{progressions, scale_key, seven_note_key};
use djrust::musicality::util::{key_pitch_classes, part, pc};
use djrust::musicality::{apply, Fixes};
use djrust::{Project, TrackSpec};

fn report(p: &Project) -> MusicalityReport {
    let (notes, controls) = capture_all(p, None);
    MusicalityReport::new(p, &notes, &controls, None)
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

const SHAPE_GENRES: [&str; 6] = ["trance", "house", "synthwave", "lofi", "funk", "jazz"];
const SHAPE_SEEDS: [f64; 2] = [42.0, 7.0];

fn later() -> Fixes {
    Fixes::CADENCE | Fixes::HOOK | Fixes::MOTIF | Fixes::ANSWER | Fixes::APPROACH
}

fn spec(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, Some("standard"))
}

/// Pooled over the songs, the melody as `.shape` leaves it meets the 6.4 contour targets it
/// can: repeats ≤ 0.30, steps ≥ 0.45, and it recovers more leaps and peaks later than the
/// same chain without it.
#[test]
fn contour_meets_the_corpus_targets() {
    let (mut rep, mut steps, mut leap_r, mut peak) = (vec![], vec![], vec![], vec![]);
    let (mut off_leap_r, mut off_peak) = (vec![], vec![]);
    for genre in SHAPE_GENRES {
        for seed in SHAPE_SEEDS {
            let s = spec(genre, seed);
            let composed = s.project();
            let on = report(&apply(&composed, &s, Fixes::ALL & !later()));
            let off = report(&apply(
                &composed,
                &s,
                (Fixes::ALL & !later()) & !Fixes::SHAPE,
            ));
            if let Some(v) = on.melody_repeats {
                rep.push(v);
            }
            if let Some(v) = on.melody_steps {
                steps.push(v);
            }
            if let Some(v) = on.leap_recovery {
                leap_r.push(v);
            }
            if let Some(v) = on.melody_peak_position {
                peak.push(v);
            }
            if let Some(v) = off.leap_recovery {
                off_leap_r.push(v);
            }
            if let Some(v) = off.melody_peak_position {
                off_peak.push(v);
            }
        }
    }
    assert!(
        !rep.is_empty() && !leap_r.is_empty() && !peak.is_empty(),
        "no melody measured"
    );
    assert!(mean(&rep) <= 0.30, "repeats {}", mean(&rep));
    assert!(mean(&steps) >= 0.45, "steps {}", mean(&steps));
    assert!(
        mean(&leap_r) > mean(&off_leap_r),
        "leap recovery {} (without .shape {})",
        mean(&leap_r),
        mean(&off_leap_r)
    );
    assert!(
        mean(&peak) >= mean(&off_peak),
        "peak {} (without .shape {})",
        mean(&peak),
        mean(&off_peak)
    );
    let _ = round3(0.0);
}

/// Every played note stays in the key, and the chord tones on the beats are what they were
/// without `.shape`.
#[test]
fn key_and_chord_tones_on_the_beat_hold() {
    for genre in SHAPE_GENRES {
        for seed in SHAPE_SEEDS {
            let s = spec(genre, seed);
            let composed = s.project();
            let on = report(&apply(&composed, &s, Fixes::ALL & !later()));
            let off = report(&apply(
                &composed,
                &s,
                (Fixes::ALL & !later()) & !Fixes::SHAPE,
            ));
            assert!(
                on.in_key >= off.in_key,
                "{genre}/{seed}: inKey {} → {}",
                off.in_key,
                on.in_key
            );
            assert_eq!(
                on.chord_tones_on_beats, off.chord_tones_on_beats,
                "{genre}/{seed}"
            );
        }
    }
}

/// The pass itself: only off-beat melody notes change, onto the key; every other net and
/// every note on a quarter is as it was. Something moves somewhere.
#[test]
fn only_off_beat_melody_notes_move() {
    let before = Fixes::KEY
        | Fixes::PHRASES
        | Fixes::TIMING
        | Fixes::VARIETY
        | Fixes::REGISTER
        | Fixes::BASS_LINE
        | Fixes::CONTOUR;
    let mut moved = 0;
    for genre in SHAPE_GENRES {
        for seed in SHAPE_SEEDS {
            let s = spec(genre, seed);
            let base = apply(&s.project(), &s, before);
            let mut shaped = base.clone();
            let g = BeatsTables::shared().genres.get(genre).unwrap();
            shape_melody(&mut shaped, g, &s);
            let key = key_pitch_classes(&base);
            for (id, nb) in base.nets.iter() {
                let after = shaped.nets.get(id).unwrap();
                for (t, b) in nb.bindings.iter() {
                    let a = after
                        .bindings
                        .get(t)
                        .unwrap_or_else(|| panic!("{genre}/{seed} {id} lost {t}"));
                    if a.note == b.note {
                        continue;
                    }
                    moved += 1;
                    let step: i64 = t[1..].parse().unwrap();
                    assert_eq!(part(nb), "melody", "{genre}/{seed}: {id} is not melody");
                    assert!(step % 4 != 0, "{genre}/{seed}: {id} {t} is on a beat");
                    assert!(
                        key.contains(&pc(a.note)),
                        "{genre}/{seed}: {id} {t} off key"
                    );
                }
                assert_eq!(
                    after.bindings.len(),
                    nb.bindings.len(),
                    "{genre}/{seed}: {id}"
                );
            }
        }
    }
    assert!(moved > 0, "nothing moved");
}

/// A pure function of (project, spec): the same song plays the same notes twice.
#[test]
fn shape_is_deterministic() {
    let s = spec("trance", 42.0);
    let composed = s.project();
    assert_eq!(
        capture(&apply(&composed, &s, Fixes::ALL), None),
        capture(&apply(&composed, &s, Fixes::ALL), None)
    );
}

// MARK: melody bigrams

#[test]
fn table_loads_and_rows_sum_to_one() {
    let all = MelodyBigrams::all().unwrap();
    assert!(all.iter().any(|t| t.name == "default"));
    let t = MelodyBigrams::default_table();
    assert!(t.songs >= 190 && t.bigrams > 50_000);
    assert!(t.next.len() == 7 && t.moves.len() == 7 && t.counts.len() == 7);
    for i in 0..7 {
        assert!(
            (t.next[i].iter().sum::<f64>() - 1.0).abs() < 1e-3,
            "next[{i}]"
        );
        assert!(
            (t.moves[i].iter().sum::<f64>() - 1.0).abs() < 1e-3,
            "moves[{i}]"
        );
        assert_eq!(t.moves[i][i], 0.0);
        assert!(t.next[i].iter().all(|x| *x >= 0.0));
    }
    assert!((t.unigram.iter().sum::<f64>() - 1.0).abs() < 1e-3);
    // The tonic is the most sung degree, then 3, then 5.
    let mut by_use: Vec<usize> = (0..7).collect();
    by_use.sort_by(|a, b| t.unigram[*b].partial_cmp(&t.unigram[*a]).unwrap());
    assert_eq!(&by_use[..3], &[0, 2, 4]);
}

#[test]
fn interval_shares_match_the_corpus() {
    let t = MelodyBigrams::default_table();
    assert!((t.step_share() - t.stats.steps).abs() < 2e-3);
    assert!((t.repeat_share() - t.stats.repeats).abs() < 2e-3);
    assert!(t.repeat_share() <= 0.35);
    assert!(t.stats.steps_of_moves >= 0.45);
    assert!((t.stats.steps_of_moves - t.step_share() / (1.0 - t.repeat_share())).abs() < 2e-3);
    assert!(t.step_share() >= 0.38);
    assert!(t.stats.pitch_steps >= 0.38 && t.stats.pitch_repeats <= 0.35);
    assert!((t.stats.pitch_repeats + t.stats.pitch_steps + t.stats.pitch_leaps - 1.0).abs() < 1e-3);
}

#[test]
fn likely_next_degrees() {
    let t = MelodyBigrams::default_table();
    assert_eq!(t.most_likely(1, false), 0); // 2 falls to the tonic
    assert_eq!(t.most_likely(3, false), 2); // 4 falls to 3
    assert_eq!(t.most_likely(6, true), 0); // the leading tone resolves
    assert_ne!(t.most_likely(0, true), 0);
    for d in 0..7 {
        assert_ne!(t.most_likely(d, true), d as usize);
    }
    // Degrees wrap: 7 is the tonic an octave up, −1 the 7th below.
    assert_eq!(t.probability(7, -1), t.next[0][6]);
    assert_eq!(t.most_likely(8, false), t.most_likely(1, false));
}

#[test]
fn parse_rejects_a_broken_table() {
    // A row that no longer sums to 1.
    let edited = MELODY_BIGRAMS_JSON.replacen("[0.4248, ", "[0.9248, ", 1);
    assert_ne!(edited, MELODY_BIGRAMS_JSON);
    assert!(MelodyBigrams::parse(&edited).is_err());
    // Degrees other than 1…7.
    let edited = MELODY_BIGRAMS_JSON.replacen("\"7\"]", "\"8\"]", 1);
    assert!(MelodyBigrams::parse(&edited).is_err());
    let _ = JsonValue::Null;
}

// MARK: variety

fn seeds() -> Vec<f64> {
    (0..30)
        .map(|i| (((i * 104_729 + 12_345) % 999_999) + 1) as f64)
        .collect()
}

/// A 4-bar loop of `genre` / `seed` as the deck plays it with `fixes`.
fn loop_of(genre: &str, seed: f64, fixes: Fixes) -> Project {
    let spec = TrackSpec::new(genre, seed, None);
    apply(&spec.project(), &spec, fixes)
}

/// The first four bars of each part, as (tick, note) pairs; drums are one part.
fn phrases(p: &Project) -> BTreeMap<String, Vec<(i64, i64)>> {
    let mut out: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for e in capture(p, Some(64)) {
        if e.group != "stinger" && !e.group.starts_with("fill-") {
            out.entry(if e.is_drum() {
                "drums".into()
            } else {
                e.group.clone()
            })
            .or_default()
            .push((e.tick, e.note));
        }
    }
    out
}

/// How many different 4-bar patterns each part plays across the seeds.
fn distinct(genre: &str, fixes: Fixes) -> BTreeMap<String, usize> {
    let mut seen: BTreeMap<String, BTreeSet<Vec<(i64, i64)>>> = BTreeMap::new();
    for s in seeds() {
        for (part, notes) in phrases(&loop_of(genre, s, fixes)) {
            seen.entry(part).or_default().insert(notes);
        }
    }
    seen.into_iter().map(|(k, v)| (k, v.len())).collect()
}

fn jaccard(a: &[(i64, i64)], b: &[(i64, i64)]) -> f64 {
    let x: BTreeSet<_> = a.iter().collect();
    let y: BTreeSet<_> = b.iter().collect();
    x.intersection(&y).count() as f64 / x.union(&y).count().max(1) as f64
}

/// Every part changes with the seed.
#[test]
fn every_part_varies_with_the_seed() {
    for genre in ["techno", "house", "trance", "lofi", "jazz", "dnb"] {
        let n = distinct(genre, Fixes::ALL);
        assert!(n["bass"] >= 8, "{genre}: {n:?}");
        assert!(n["harmony"] >= 6, "{genre}: {n:?}");
        assert!(n["drums"] >= 4, "{genre}: {n:?}");
        assert_eq!(n["melody"], seeds().len(), "{genre}: {n:?}");
        if let Some(arp) = n.get("arp") {
            assert!(*arp >= 10, "{genre}: {n:?}");
        }
        // A/B: without the flag the backing is the genre's, whatever the seed
        let plain = distinct(genre, Fixes::ALL & !Fixes::VARIETY);
        for part in ["bass", "harmony", "drums"] {
            assert!(
                n[part] > plain.get(part).copied().unwrap_or(0),
                "{genre} {part}: {plain:?} → {n:?}"
            );
        }
        assert!(plain["bass"] <= 3, "{genre}: the backing was the genre's");
    }
}

/// The melodies of two seeds share few notes (pitch at a tick).
#[test]
fn melodies_share_few_notes() {
    for genre in ["techno", "house", "trance", "lofi", "jazz", "dnb"] {
        let tunes: Vec<Vec<(i64, i64)>> = seeds()
            .iter()
            .take(12)
            .map(|&s| {
                phrases(&loop_of(genre, s, Fixes::ALL))
                    .remove("melody")
                    .unwrap_or_default()
            })
            .collect();
        let (mut sum, mut pairs) = (0.0, 0);
        for i in 0..tunes.len() {
            for j in i + 1..tunes.len() {
                sum += jaccard(&tunes[i], &tunes[j]);
                pairs += 1;
            }
        }
        assert!(
            sum / pairs as f64 <= 0.25,
            "{genre}: mean pitch Jaccard {}",
            sum / pairs as f64
        );
    }
}

/// Not only another key: the chords themselves change.
#[test]
fn progressions_change_not_just_the_key() {
    for genre in [
        "techno", "house", "trance", "jazz", "dnb", "lofi", "ambient",
    ] {
        let mut shapes: BTreeSet<Vec<(i64, i64)>> = BTreeSet::new();
        for s in seeds() {
            let p = loop_of(genre, s, Fixes::ALL);
            let root = p.root_note.unwrap_or(0);
            shapes.insert(
                phrases(&p)
                    .remove("harmony")
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(t, n)| (t, ((n - root) % 12 + 12) % 12))
                    .collect(),
            );
        }
        assert!(
            shapes.len() >= 3,
            "{genre}: {} progressions in {} seeds",
            shapes.len(),
            seeds().len()
        );
    }
}

/// The arp's figure is the seed's (and the slot's).
#[test]
fn arp_figures_vary_with_the_seed() {
    let mut songs: BTreeSet<Vec<(i64, i64)>> = BTreeSet::new();
    for s in seeds().iter().take(12) {
        let spec = TrackSpec::new("trance", *s, Some("minimal"));
        let p = apply(&spec.project(), &spec, Fixes::ALL & !Fixes::VARIETY);
        songs.insert(
            capture(&p, None)
                .into_iter()
                .filter(|e| e.group == "arp")
                .map(|e| (e.tick, e.note))
                .collect(),
        );
    }
    assert!(songs.len() >= 10, "{} arps in 12 seeds", songs.len());
    let figures: std::collections::HashSet<ArpFigure> = seeds()
        .iter()
        .map(|&s| ArpFigure::seeded(s, "A", None))
        .collect();
    assert!(figures.len() >= 10);
    assert_eq!(ArpFigure::seeded(42.0, "C", None).rate, 2);
}

/// A pure function of the spec.
#[test]
fn deterministic_from_the_seed() {
    for genre in ["techno", "jazz", "trance"] {
        let spec = TrackSpec::new(genre, 4242.0, Some("standard"));
        let a = apply(&spec.project(), &spec, Fixes::ALL);
        let b = apply(&spec.project(), &spec, Fixes::ALL);
        assert_eq!(a.canonical_json(), b.canonical_json(), "{genre}");
    }
}

/// A key-locked walk pins the key.
#[test]
fn a_pinned_key_holds_across_seeds() {
    for genre in ["techno", "jazz"] {
        for offset in [-5i64, 0, 3, 6] {
            for s in seeds().iter().take(6) {
                let mut spec = TrackSpec::new(genre, *s, None);
                spec.extra.set("keyOffset", (offset as f64).into());
                let composed = spec.project();
                let played = apply(&composed, &spec, Fixes::ALL);
                assert_eq!(
                    played.root_note,
                    Some(composed.root_note.unwrap_or(60) + offset),
                    "{genre} {s} {offset}"
                );
            }
        }
    }
}

/// The key moves with the root.
#[test]
fn still_in_key_and_on_the_chords() {
    for genre in [
        "techno", "house", "trance", "jazz", "dnb", "lofi", "ambient", "edm",
    ] {
        let (mut moved, mut retuned) = (0, 0);
        let (mut with, mut without) = (0.0, 0.0);
        for s in seeds().iter().take(10) {
            let spec = TrackSpec::new(genre, *s, Some("standard"));
            let composed = spec.project();
            let a = apply(&composed, &spec, Fixes::ALL & !Fixes::VARIETY);
            let b = apply(&composed, &spec, Fixes::ALL);
            let ra = MusicalityReport::new(&a, &capture(&a, None), &[], None);
            let rb = MusicalityReport::new(&b, &capture(&b, None), &[], None);
            assert!(
                rb.in_key >= ra.in_key - 0.01,
                "{genre}/{s}: in key {} → {}",
                ra.in_key,
                rb.in_key
            );
            if let (Some(x), Some(y)) = (ra.chord_tones_on_beats, rb.chord_tones_on_beats) {
                assert!(y >= x - 0.1, "{genre}/{s}: chord tones on beats {x} → {y}");
            }
            with += rb.semitone_clashes;
            without += ra.semitone_clashes;
            if b.root_note != a.root_note {
                moved += 1;
            }
            if KickTuning::hz(b.root_note) != KickTuning::hz(a.root_note) {
                retuned += 1;
            }
        }
        assert!(moved >= 5, "{genre}: {moved} of 10 seeds in another key");
        assert!(
            retuned >= 3,
            "{genre}: the kick follows the key in {retuned} of 10"
        );
        assert!(
            with <= without + 10.0,
            "{genre}: rubs per 100 pairs, ×10 seeds ({with}, {without})"
        );
    }
}

/// Pentatonic genres change their chords, not only their key.
#[test]
fn pentatonic_progressions_stay_in_key() {
    for genre in ["lofi", "ambient"] {
        let n = distinct(genre, Fixes::ALL);
        assert!(n["harmony"] >= 6, "{genre}: {n:?}");
        let (mut with, mut without) = (0.0, 0.0);
        let tried: Vec<f64> = seeds().into_iter().take(10).collect();
        for s in &tried {
            let spec = TrackSpec::new(genre, *s, Some("standard"));
            let composed = spec.project();
            let a = apply(&composed, &spec, Fixes::ALL & !Fixes::VARIETY);
            let b = apply(&composed, &spec, Fixes::ALL);
            let ra = MusicalityReport::new(&a, &capture(&a, None), &[], None);
            let rb = MusicalityReport::new(&b, &capture(&b, None), &[], None);
            let rc = MusicalityReport::new(&composed, &capture(&composed, None), &[], None);
            assert!(
                rb.in_key >= ra.in_key - 1e-9,
                "{genre}/{s}: .variety {} → {}",
                ra.in_key,
                rb.in_key
            );
            assert!(
                rb.in_key >= rc.in_key - 0.03,
                "{genre}/{s}: in key {}, composed {}",
                rb.in_key,
                rc.in_key
            );
            with += rb.semitone_clashes;
            without += ra.semitone_clashes;
        }
        let rise = (with - without) / tried.len() as f64;
        assert!(
            rise <= 1.0,
            "{genre}: rubs per 100 pairs rise {rise} on average"
        );
    }
}

/// The pool by key size.
#[test]
fn progression_pools_by_key_size() {
    let genres = &BeatsTables::shared().genres;
    let lofi = progressions(&genres["lofi"]);
    assert!(lofi.len() >= 4);
    assert!(
        lofi.iter().all(|p| p.iter().all(|d| (0..5).contains(d))),
        "{lofi:?}"
    );
    assert!(
        lofi.contains(&vec![1, 4, 0, 0]),
        "beats' plan stays a choice: {lofi:?}"
    );
    let ambient = progressions(&genres["ambient"]);
    assert!(ambient.len() >= 3);
    assert!(
        ambient.iter().all(|p| p.iter().all(|d| (0..5).contains(d))),
        "{ambient:?}"
    );
    assert!(progressions(&genres["blues"]).is_empty());
    // the seven-note helpers stay seven-note
    let spec = TrackSpec::new("lofi", 42.0, None);
    assert!(seven_note_key(&spec.project()).is_none());
    assert_eq!(scale_key(&spec.project()).1.len(), 5);
    let _ = pad_chords;
}
