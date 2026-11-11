//! Ports of Swift `ArpDensityTests`, `ArpStyleTests` and `PartSeedTests` (all but the
//! session round trip, `PreparedTrack`, the donor cache and the timed prepare, which belong
//! to hosts and the session crate).

use std::collections::{BTreeMap, BTreeSet};

use djrust::cid::share_cid;
use djrust::musicality::dj::{
    arp_density_rates, melody_onsets_by_bar, ArpFigure, ARP_BUSY_ONSETS, ARP_DENSITY_DIRECTION,
    ARP_DENSITY_LEVELS,
};
use djrust::musicality::energy_curve::RAMP_SECTIONS;
use djrust::musicality::note_log::{capture, capture_all};
use djrust::musicality::report::MusicalityReport;
use djrust::musicality::slot_windows::slot_windows;
use djrust::musicality::util::part;
use djrust::musicality::{apply, Fixes};
use djrust::{Project, TrackSpec};

fn report(p: &Project) -> MusicalityReport {
    let (notes, controls) = capture_all(p, None);
    MusicalityReport::new(p, &notes, &controls, None)
}

fn played(genre: &str, seed: f64, fixes: Fixes) -> Project {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    apply(&spec.project(), &spec, fixes)
}

// MARK: arp density

const GENRES: [&str; 3] = ["trance", "edm", "synthwave"];
const SEEDS: [f64; 3] = [7.0, 42.0, 99.0];

/// Distinct onset ticks of `group` in each song bar it plays in.
fn onsets_by_bar(p: &Project, group: &str) -> BTreeMap<i64, i64> {
    let ticks: BTreeSet<i64> = capture(p, None)
        .into_iter()
        .filter(|e| e.group == group)
        .map(|e| e.tick)
        .collect();
    let mut out = BTreeMap::new();
    for t in ticks {
        *out.entry((t - 1) / 16).or_insert(0) += 1;
    }
    out
}

fn section_of_bar(p: &Project) -> BTreeMap<i64, String> {
    let mut out = BTreeMap::new();
    let mut at = 0;
    for s in &p.structure {
        let mut b = at;
        while b < at + s.steps {
            out.insert(b / 16, s.name.clone());
            b += 16;
        }
        at += s.steps;
    }
    out
}

fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.len() <= 2 {
        return None;
    }
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    if sxx <= 0.0 || syy <= 0.0 {
        return Some(0.0);
    }
    Some(sxy / (sxx * syy).sqrt())
}

/// Pooled over every song: (melody onsets, arp notes) in each bar where both play.
fn pairs(fixes: Fixes) -> (Vec<f64>, Vec<f64>) {
    let (mut xs, mut ys) = (vec![], vec![]);
    for genre in GENRES {
        for seed in SEEDS {
            let p = played(genre, seed, fixes);
            let melody = onsets_by_bar(&p, "melody");
            for (bar, n) in onsets_by_bar(&p, "arp") {
                let Some(m) = melody.get(&bar) else { continue };
                xs.push(*m as f64);
                ys.push(n as f64);
            }
        }
    }
    (xs, ys)
}

/// With the flag the arp's notes per bar follow the melody's onsets (r ≥ 0.5); without it
/// the figure keeps one rate and the two are unrelated (|r| < 0.2).
#[test]
fn arp_rate_follows_the_melodys_density() {
    let on = pairs(Fixes::ALL);
    let off = pairs(Fixes::ALL & !Fixes::ARP_DENSITY);
    let r_on = pearson(&on.0, &on.1).unwrap();
    let r_off = pearson(&off.0, &off.1).unwrap();
    assert!(
        r_on >= 0.5,
        "r = {r_on} over {} bars with .arpDensity",
        on.1.len()
    );
    assert!(
        r_off.abs() < 0.2,
        "r = {r_off} over {} bars without",
        off.1.len()
    );
}

/// Buildups get busier.
#[test]
fn buildups_get_busier() {
    let (mut busier, mut checked) = (0, 0);
    for genre in GENRES {
        for seed in SEEDS {
            let on = played(genre, seed, Fixes::ALL);
            let off = played(genre, seed, Fixes::ALL & !Fixes::ARP_DENSITY);
            let section = section_of_bar(&on);
            let mean = |p: &Project, buildup: bool| -> Option<f64> {
                let bars: Vec<i64> = onsets_by_bar(p, "arp")
                    .into_iter()
                    .filter(|(bar, _)| {
                        RAMP_SECTIONS.contains(&section.get(bar).map_or("", |s| s.as_str()))
                            == buildup
                    })
                    .map(|(_, n)| n)
                    .collect();
                if bars.is_empty() {
                    None
                } else {
                    Some(bars.iter().sum::<i64>() as f64 / bars.len() as f64)
                }
            };
            let (Some(b), Some(rest), Some(before)) =
                (mean(&on, true), mean(&on, false), mean(&off, true))
            else {
                continue;
            };
            checked += 1;
            assert!(
                b >= rest,
                "{genre}/{seed}: buildup {b} a bar, the rest {rest}"
            );
            if b > before {
                busier += 1;
            }
        }
    }
    assert!(checked > 0, "no song with an arp in a buildup");
    assert!(busier > 0, "no buildup got busier");
}

/// The arp plays in exactly the bars it did, on the chord.
#[test]
fn silent_bars_stay_silent_and_notes_keep_the_chord() {
    for genre in GENRES {
        for seed in SEEDS {
            let on = played(genre, seed, Fixes::ALL);
            let off = played(genre, seed, Fixes::ALL & !Fixes::ARP_DENSITY);
            let a: BTreeSet<i64> = onsets_by_bar(&on, "arp").keys().copied().collect();
            let b: BTreeSet<i64> = onsets_by_bar(&off, "arp").keys().copied().collect();
            assert_eq!(a, b, "{genre}/{seed}");
            assert_eq!(report(&on).arp_chord_tones, Some(1.0), "{genre}/{seed}");
        }
    }
}

/// Every rate is one of the three levels, and the rates vary within a song — unless the
/// melody is busy in every bar the arp plays outside a phrase's fourth bar.
#[test]
fn rates_are_quarters_eighths_or_sixteenths() {
    let mut pooled: BTreeSet<i64> = BTreeSet::new();
    for genre in GENRES {
        let spec = TrackSpec::new(genre, 42.0, Some("standard"));
        let p = apply(
            &spec.project(),
            &spec,
            Fixes::ALL & !(Fixes::ARPEGGIO | Fixes::ARP_DENSITY),
        );
        let rates = arp_density_rates(&p, 42.0, Some(genre));
        let all: Vec<i64> = rates.values().flat_map(|m| m.values().copied()).collect();
        assert!(!all.is_empty(), "{genre}: no arp rates");
        assert!(
            all.iter().all(|r| ARP_DENSITY_LEVELS.contains(r)),
            "{genre}"
        );
        pooled.extend(all.iter().copied());
        let distinct: BTreeSet<i64> = all.iter().copied().collect();
        if distinct.len() == 1 {
            let melody = melody_onsets_by_bar(&p);
            let mut starts = vec![];
            let mut at = 0;
            for s in &p.structure {
                starts.push(at);
                at += s.steps;
            }
            let mut calm: Vec<i64> = vec![];
            for ws in slot_windows(&p, "arp").values() {
                for w in ws {
                    let mut step = w.start;
                    while step < w.end {
                        if (step - starts[w.section]) / 16 % 4 != 3
                            && !RAMP_SECTIONS.contains(&w.section_name.as_str())
                            && melody.get(&(step / 16)).copied().unwrap_or(0) < ARP_BUSY_ONSETS
                        {
                            calm.push(step);
                        }
                        step += 16;
                    }
                }
            }
            assert!(
                all[0] == *ARP_DENSITY_LEVELS.last().unwrap() && calm.is_empty(),
                "{genre}: one rate everywhere {distinct:?}, calm bars {:?}",
                calm.iter().map(|s| s / 16).collect::<Vec<_>>()
            );
        }
    }
    assert!(pooled.len() > 1, "one rate in every genre {pooled:?}");
}

/// The direction lives in one constant and follows the melody; levels run sparse to busy.
#[test]
fn direction_is_one_constant() {
    assert_eq!(ARP_DENSITY_DIRECTION, 1);
    assert_eq!(ARP_DENSITY_LEVELS, [4, 2, 1]);
}

/// Without `.arpeggio` the flag does nothing; it is in `Fixes.all` and named.
#[test]
fn needs_arpeggio_and_is_listed() {
    let a = played("trance", 42.0, Fixes::ALL & !Fixes::ARPEGGIO);
    let b = played(
        "trance",
        42.0,
        Fixes::ALL & !(Fixes::ARPEGGIO | Fixes::ARP_DENSITY),
    );
    assert_eq!(a.canonical_json(), b.canonical_json());
    assert!(Fixes::ALL.contains(Fixes::ARP_DENSITY));
    assert_eq!(Fixes::named("arpDensity"), Some(Fixes::ARP_DENSITY));
}

// MARK: arp style

fn style_seeds() -> Vec<f64> {
    (0..30)
        .map(|i| (((i * 104_729 + 12_345) % 999_999) + 1) as f64)
        .collect()
}

/// Trance: an up-and-down over 4–6 tones, every sixteenth.
#[test]
fn trance_rolls_up_and_down_in_sixteenths() {
    for s in style_seeds() {
        for variant in ["A", "B"] {
            let f = ArpFigure::seeded(s, variant, Some("trance"));
            assert!(f.shape == 2 && f.rate == 1 && !f.octaves, "{s}/{variant}");
            assert!((4..=6).contains(&f.span), "{s}/{variant}: span {}", f.span);
            let tones: Vec<i64> = (0..16).map(|i| f.tone(i)).collect();
            for w in tones.windows(2) {
                assert_eq!((w[0] - w[1]).abs(), 1, "{s}/{variant}: {tones:?}");
            }
            assert!(tones.iter().all(|t| (0..f.span).contains(t)));
        }
    }
    assert_eq!(ArpFigure::seeded(42.0, "C", Some("trance")).rate, 2);
}

/// Synthwave: eighths only, each ladder tone played twice, the second an octave up.
#[test]
fn synthwave_climbs_in_eighths_with_octave_jumps() {
    for s in style_seeds() {
        let f = ArpFigure::seeded(s, "A", Some("synthwave"));
        assert!(f.rate == 2 && f.octaves, "{s}");
        for q in (0..16).step_by(4) {
            assert_eq!(f.tone(q), f.tone(q + 2), "{s}: step {q}");
            assert!(f.octave(q) == 0 && f.octave(q + 2) == 12, "{s}: step {q}");
        }
    }
}

/// EDM: up or broken, every sixteenth.
#[test]
fn edm_plays_up_or_broken_sixteenths() {
    let mut shapes = BTreeSet::new();
    for s in style_seeds() {
        let f = ArpFigure::seeded(s, "A", Some("edm"));
        assert!(
            [0, 3].contains(&f.shape) && f.rate == 1 && !f.octaves,
            "{s}"
        );
        shapes.insert(f.shape);
    }
    assert_eq!(shapes, [0, 3].into_iter().collect());
}

/// The seed still picks within each pool, and a genre without a style draws from the full pool.
#[test]
fn the_seed_still_picks_within_the_pool() {
    for genre in ["trance", "synthwave", "edm"] {
        let figures: std::collections::HashSet<ArpFigure> = style_seeds()
            .iter()
            .map(|&s| ArpFigure::seeded(s, "A", Some(genre)))
            .collect();
        assert!(
            figures.len() >= 8,
            "{genre}: {} figures in 30 seeds",
            figures.len()
        );
    }
    for genre in ["techno", "house", "dnb", "ambient"] {
        for s in style_seeds().into_iter().take(8) {
            assert_eq!(
                ArpFigure::seeded(s, "A", Some(genre)),
                ArpFigure::seeded(s, "A", None)
            );
        }
    }
}

/// As played: every arp note still a tone of the bar's chord, and the styled genres' arps
/// differ from the full pool's.
#[test]
fn played_arps_keep_the_chords() {
    for genre in ["trance", "edm", "synthwave"] {
        for seed in [7.0, 42.0] {
            // the figure's own rate (`.arpDensity` varies it bar by bar)
            let p = played(genre, seed, Fixes::ALL & !Fixes::ARP_DENSITY);
            assert_eq!(report(&p).arp_chord_tones, Some(1.0), "{genre}/{seed}");
            let arp: Vec<_> = capture(&p, None)
                .into_iter()
                .filter(|e| e.group == "arp")
                .collect();
            assert!(!arp.is_empty(), "{genre}/{seed}");
            let plain: Vec<_> = capture(
                &played(
                    genre,
                    seed,
                    Fixes::ALL & !(Fixes::ARP_STYLE | Fixes::ARP_DENSITY),
                ),
                None,
            )
            .into_iter()
            .filter(|e| e.group == "arp")
            .collect();
            let key = |v: &[djrust::musicality::note_log::NoteEvent]| -> Vec<(i64, i64)> {
                v.iter().map(|e| (e.tick, e.note)).collect()
            };
            assert_ne!(key(&arp), key(&plain), "{genre}/{seed}");
            let mut by_bar: BTreeMap<i64, usize> = BTreeMap::new();
            for e in &arp {
                *by_bar.entry((e.tick - 1) / 16).or_insert(0) += 1;
            }
            for (_, n) in by_bar {
                assert!(n >= 8, "{genre}/{seed}: {n} arp notes in a bar");
            }
            if genre != "synthwave" {
                continue;
            }
            assert!(
                arp.iter().all(|e| (e.tick - 1) % 2 == 0),
                "{genre}/{seed}: off the eighths"
            );
            let mut at: BTreeMap<String, i64> = BTreeMap::new();
            for e in &arp {
                at.insert(format!("{}/{}", e.net, e.tick), e.note);
            }
            for e in arp.iter().filter(|e| (e.tick - 1) % 4 == 2) {
                let Some(first) = at.get(&format!("{}/{}", e.net, e.tick - 2)) else {
                    continue;
                };
                assert_eq!(e.note, first + 12, "{genre}/{seed}: tick {}", e.tick);
            }
        }
    }
}

/// Genres without a style (house has no arp role at all) play exactly what they did.
#[test]
fn other_genres_are_unchanged() {
    for genre in ["house", "techno", "dnb"] {
        let a = played(genre, 42.0, Fixes::ALL);
        let b = played(genre, 42.0, Fixes::ALL & !Fixes::ARP_STYLE);
        assert_eq!(a.canonical_json(), b.canonical_json(), "{genre}");
    }
}

// MARK: part seeds

const PS_SEEDS: [f64; 6] = [17.0, 4242.0, 90_001.0, 123_457.0, 777_777.0, 31_337.0];

fn played_parts(
    genre: &str,
    seed: f64,
    parts: &[(&str, f64)],
    structure: Option<&str>,
    fixes: Fixes,
) -> Project {
    let mut spec = TrackSpec::new(genre, seed, structure);
    for (k, v) in parts {
        spec.part_seeds.insert(k.to_string(), *v);
    }
    apply(&spec.project(), &spec, fixes)
}

/// Each part's (tick, note) pairs over `ticks` (the whole song when None); drums are one part.
fn parts_of(p: &Project, ticks: Option<i64>) -> BTreeMap<String, Vec<(i64, i64)>> {
    let mut out: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for e in capture(p, ticks) {
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

fn ticks_of(events: Option<&Vec<(i64, i64)>>) -> Vec<i64> {
    events
        .map(|v| v.iter().map(|e| e.0).collect())
        .unwrap_or_default()
}

/// Seeded drum parts play the same hits under every track seed; the same track seeds without
/// part seeds do not.
#[test]
fn seeded_drums_keep_their_pattern_across_seeds() {
    for genre in ["house", "techno", "dnb"] {
        let drum_seeds = [("kick", 501.0), ("snare", 502.0), ("hihat", 503.0)];
        let seeded: BTreeSet<Vec<(i64, i64)>> = PS_SEEDS
            .iter()
            .map(|&s| {
                parts_of(
                    &played_parts(genre, s, &drum_seeds, None, Fixes::ALL),
                    Some(64),
                )
                .remove("drums")
                .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            seeded.len(),
            1,
            "{genre}: {} drum patterns with seeded drums",
            seeded.len()
        );
        let own: BTreeSet<Vec<(i64, i64)>> = PS_SEEDS
            .iter()
            .map(|&s| {
                parts_of(&played_parts(genre, s, &[], None, Fixes::ALL), Some(64))
                    .remove("drums")
                    .unwrap_or_default()
            })
            .collect();
        assert!(own.len() >= seeded.len(), "{genre}: {} without", own.len());
    }
}

/// A seeded bass keeps its rhythm under every track seed.
#[test]
fn a_seeded_bass_keeps_its_rhythm() {
    for genre in ["techno", "house", "trance"] {
        let fixes = Fixes::ALL & !Fixes::APPROACH;
        let rhythms: BTreeSet<Vec<i64>> = PS_SEEDS
            .iter()
            .map(|&s| {
                ticks_of(
                    parts_of(
                        &played_parts(genre, s, &[("bass", 9_001.0)], None, fixes),
                        Some(64),
                    )
                    .get("bass"),
                )
            })
            .collect();
        assert_eq!(rhythms.len(), 1, "{genre}: {} bass rhythms", rhythms.len());
    }
}

/// A seeded melody is that seed's melody.
#[test]
fn a_seeded_melody_is_its_seeds_melody() {
    for genre in ["techno", "trance", "lofi"] {
        let donor = parts_of(
            &played_parts(genre, 55_555.0, &[], None, Fixes::KEY),
            Some(64),
        )
        .remove("melody");
        for s in PS_SEEDS.iter().take(3) {
            let p = parts_of(
                &played_parts(genre, *s, &[("melody", 55_555.0)], None, Fixes::KEY),
                Some(64),
            );
            assert_eq!(p.get("melody"), donor.as_ref(), "{genre} {s}");
            assert_eq!(
                p.get("harmony"),
                parts_of(&played_parts(genre, *s, &[], None, Fixes::KEY), Some(64)).get("harmony"),
                "{genre} {s}"
            );
        }
        let rhythms: BTreeSet<Vec<i64>> = PS_SEEDS
            .iter()
            .map(|&s| {
                ticks_of(
                    parts_of(
                        &played_parts(
                            genre,
                            s,
                            &[("melody", 55_555.0)],
                            None,
                            Fixes::KEY | Fixes::VARIETY,
                        ),
                        Some(64),
                    )
                    .get("melody"),
                )
            })
            .collect();
        assert_eq!(
            rhythms.len(),
            1,
            "{genre}: {} melody rhythms",
            rhythms.len()
        );
    }
}

/// Song structures too (slot nets by id): a seeded melody transplants into the song's sections.
#[test]
fn a_seeded_melody_transplants_into_a_song() {
    let a = played_parts(
        "trance",
        4242.0,
        &[("melody", 808.0)],
        Some("minimal"),
        Fixes::LOUDNESS,
    );
    let b = TrackSpec::new("trance", 808.0, Some("minimal")).project();
    let own = TrackSpec::new("trance", 4242.0, Some("minimal")).project();
    let melody = |p: &Project| -> BTreeSet<String> {
        p.nets
            .iter()
            .filter(|(_, nb)| nb.role != "control" && part(nb) == "melody")
            .map(|(k, _)| k.to_string())
            .collect()
    };
    let shared: BTreeSet<String> = melody(&a)
        .intersection(&melody(&b))
        .filter(|id| {
            a.nets.get(id).unwrap().transitions.keys() == b.nets.get(id).unwrap().transitions.keys()
        })
        .cloned()
        .collect();
    assert!(shared.len() >= 4, "{shared:?}");
    let bound = |p: &Project, id: &str| -> Vec<(String, djrust::pflow::MidiBinding)> {
        let mut v: Vec<_> = p
            .nets
            .get(id)
            .unwrap()
            .bindings
            .iter()
            .map(|(k, b)| (k.to_string(), *b))
            .collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    let same: BTreeSet<String> = shared
        .iter()
        .filter(|id| bound(&a, id) == bound(&b, id))
        .cloned()
        .collect();
    assert_eq!(
        same,
        shared,
        "not the donor's: {:?}",
        shared.difference(&same).collect::<Vec<_>>()
    );
    assert!(
        shared.iter().any(|id| bound(&a, id) != bound(&own, id)),
        "the track's own melody"
    );
    // the transplanted nets play in the first drop (ticks 32..<160) and the last (288..<416)
    let ticks: Vec<i64> = capture(&a, None)
        .into_iter()
        .filter(|e| shared.contains(&e.net))
        .map(|e| e.tick)
        .collect();
    assert!(ticks.iter().any(|t| (32..160).contains(t)));
    assert!(ticks.iter().any(|t| (288..416).contains(t)));
}

/// A part seed moves only its part.
#[test]
fn a_part_seed_moves_only_its_part() {
    let mut arp_moved = false;
    for s in PS_SEEDS.iter().take(4) {
        let plain = parts_of(
            &played_parts("trance", *s, &[], Some("minimal"), Fixes::ALL),
            None,
        );
        let arp = parts_of(
            &played_parts("trance", *s, &[("arp", 31.0)], Some("minimal"), Fixes::ALL),
            None,
        );
        let keys: BTreeSet<&String> = plain.keys().chain(arp.keys()).collect();
        for part in keys {
            if part != "arp" {
                assert_eq!(plain.get(part), arp.get(part), "{s} {part}");
            }
        }
        if plain.get("arp") != arp.get("arp") {
            arp_moved = true;
        }
        let hats = parts_of(
            &played_parts("house", *s, &[("hihat", 31.0)], None, Fixes::ALL),
            Some(64),
        );
        let house = parts_of(&played_parts("house", *s, &[], None, Fixes::ALL), Some(64));
        let keys: BTreeSet<&String> = house.keys().chain(hats.keys()).collect();
        for part in keys {
            if part != "drums" {
                assert_eq!(house.get(part), hats.get(part), "house {s} {part}");
            }
        }
    }
    assert!(arp_moved);
}

/// A part seed equal to the track's seed changes nothing; deterministic from the spec.
#[test]
fn own_seed_is_a_no_op_and_the_result_is_pure() {
    let all: Vec<(&str, f64)> = TrackSpec::SEEDABLE_PARTS
        .iter()
        .map(|p| (*p, 4242.0))
        .collect();
    let a = played_parts("techno", 4242.0, &[], Some("standard"), Fixes::ALL);
    let b = played_parts("techno", 4242.0, &all, Some("standard"), Fixes::ALL);
    assert_eq!(a.canonical_json(), b.canonical_json());
    let c = played_parts(
        "techno",
        4242.0,
        &[("melody", 12.0), ("bass", 13.0)],
        Some("standard"),
        Fixes::ALL,
    );
    let d = played_parts(
        "techno",
        4242.0,
        &[("melody", 12.0), ("bass", 13.0)],
        Some("standard"),
        Fixes::ALL,
    );
    assert_eq!(c.canonical_json(), d.canonical_json());
    assert_ne!(c.canonical_json(), a.canonical_json());
}

/// dj-only: the share payload, its CID and the composed project are the same with or without
/// part seeds.
#[test]
fn share_cid_and_compose_ignore_part_seeds() {
    for genre in ["techno", "jazz"] {
        let plain = TrackSpec::new(genre, 4242.0, Some("standard"));
        let mut seeded = plain.clone();
        seeded.part_seeds.insert("melody".into(), 9.0);
        seeded.part_seeds.insert("kick".into(), 10.0);
        seeded.part_seeds.insert("arp".into(), 11.0);
        assert_eq!(
            seeded.payload().canonical_js(),
            plain.payload().canonical_js()
        );
        assert_eq!(share_cid(&seeded.payload()), share_cid(&plain.payload()));
        assert!(seeded.payload().get("partSeeds").is_none());
        assert_eq!(
            seeded.project().canonical_json(),
            plain.project().canonical_json()
        );
    }
}
