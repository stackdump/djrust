//! Port of Swift `MusicalityTests`: the musical invariants of what a deck plays
//! (`PreparedTrack.played`, after `DJMusicality`), measured on the note log. Parts that read
//! a `WaveRunner.prepare` result's lanes (`byNet`, voices) are the runner's and not ported;
//! `tracksPlayAtOneLoudness` uses `Loudness::gain` directly.

use std::collections::{BTreeMap, BTreeSet};

use djrust::gen::core::scale_intervals;
use djrust::gen::tables::BeatsTables;
use djrust::musicality::dj::{
    phrase_lengths, BASS_CEILING, BASS_FLOOR, CRASH_NOTE, LEGATO_MAX, OPEN_HAT_NOTE, PAD_FLOOR,
};
use djrust::musicality::loudness::Loudness;
use djrust::musicality::note_log::{capture, capture_all, NoteEvent};
use djrust::musicality::report::{is_tonal_event, MusicalityReport};
use djrust::musicality::{apply, Fixes};
use djrust::{Project, TrackSpec};

struct Played {
    spec: TrackSpec,
    composed: Project,
    played: Project,
}

fn played(genre: &str, seed: f64) -> Played {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    let composed = spec.project();
    let p = apply(&composed, &spec, Fixes::ALL);
    Played {
        spec,
        composed,
        played: p,
    }
}

fn report(p: &Project) -> MusicalityReport {
    let (notes, controls) = capture_all(p, None);
    MusicalityReport::new(p, &notes, &controls, None)
}

fn scale_pcs(p: &Project) -> BTreeSet<i64> {
    let root = p.root_note.unwrap_or(60);
    scale_intervals(&format!("{}Scale", p.scale_name))
        .iter()
        .map(|i| (root + i) % 12)
        .collect()
}

/// Fixes that rewrite single notes by their neighbours (`.cadence`, `.approach`) or bars by
/// others (`.motif`, `.bassMotif`): a test that isolates an earlier fix by A/B leaves them out
/// of both sides.
fn phrasing() -> Fixes {
    Fixes::CADENCE | Fixes::APPROACH | Fixes::MOTIF | Fixes::BASS_MOTIF
}

/// `t` played with every fix except `without` and `phrasing`.
fn played_without(t: &Played, without: Fixes) -> Project {
    apply(&t.composed, &t.spec, (Fixes::ALL & !phrasing()) & !without)
}

fn with(t: &Played, remove: Fixes) -> Project {
    apply(&t.composed, &t.spec, Fixes::ALL & !remove)
}

fn tension_genres() -> Vec<String> {
    BeatsTables::shared()
        .genre_ids
        .iter()
        .filter(|g| BeatsTables::shared().genres[*g].tension_curve)
        .cloned()
        .collect()
}

/// The pass rewrites bindings and markings only: same nets, places, transitions and arcs.
#[test]
fn pass_keeps_the_net_topology() {
    for genre in ["techno", "edm", "jazz"] {
        let t = played(genre, 42.0);
        assert_eq!(t.played.nets.keys(), t.composed.nets.keys());
        for id in t.composed.nets.keys() {
            let a = t.composed.nets.get(id).unwrap();
            let b = t.played.nets.get(id).unwrap();
            assert_eq!(a.places.keys(), b.places.keys(), "{genre} {id} places");
            assert_eq!(
                a.transitions.keys(),
                b.transitions.keys(),
                "{genre} {id} transitions"
            );
            assert_eq!(a.arcs.len(), b.arcs.len(), "{genre} {id} arcs");
            let unvaried = apply(
                &t.composed,
                &t.spec,
                Fixes::ALL & !(Fixes::VARIETY | Fixes::BASS_MOTIF),
            )
            .nets
            .get(id)
            .unwrap()
            .clone();
            assert!(
                b.bindings
                    .keys()
                    .iter()
                    .all(|k| b.transitions.contains_key(k)),
                "{genre} {id}"
            );
            if id.starts_with("fill-") {
                continue;
            } else if djrust::musicality::backbeat::backbeat_style(genre).is_some()
                && djrust::musicality::backbeat::BACKBEAT_ROLES.contains(&id.as_str())
            {
                assert!(!b.bindings.keys().is_empty(), "{genre} {id}");
            } else if b.track.group.as_deref() == Some("arp")
                || b.track.group.as_deref() == Some("melody")
            {
                assert!(!b.bindings.keys().is_empty(), "{genre} {id}");
            } else if b.track.group.as_deref() == Some("harmony") {
                assert!(
                    a.bindings.keys().iter().all(|k| b.bindings.contains_key(k)),
                    "{genre} {id}"
                );
            } else {
                assert_eq!(
                    a.bindings.len(),
                    unvaried.bindings.len(),
                    "{genre} {id} bound transitions"
                );
            }
        }
        assert_eq!(
            t.played
                .structure
                .iter()
                .map(|s| s.steps)
                .collect::<Vec<_>>(),
            t.composed
                .structure
                .iter()
                .map(|s| s.steps)
                .collect::<Vec<_>>()
        );
    }
}

/// `compose()` itself is not touched: the parity path still gets beats' project.
#[test]
fn compose_is_unchanged() {
    let spec = TrackSpec::new("edm", 42.0, Some("standard"));
    let t = played("edm", 42.0);
    assert_eq!(t.composed.canonical_json(), spec.project().canonical_json());
    assert_ne!(
        t.played.canonical_json(),
        t.composed.canonical_json(),
        "the pass must do something"
    );
    assert_eq!(
        apply(&t.composed, &spec, Fixes::EMPTY).canonical_json(),
        t.composed.canonical_json()
    );
}

/// Every tonal note in key, in every genre with tension variants.
#[test]
fn every_note_in_key() {
    for genre in tension_genres() {
        for seed in [7.0, 42.0] {
            let t = played(&genre, seed);
            let pcs = scale_pcs(&t.played);
            let mut by_part: BTreeMap<String, Vec<NoteEvent>> = BTreeMap::new();
            for e in capture(&t.played, None).into_iter().filter(is_tonal_event) {
                by_part.entry(e.group.clone()).or_default().push(e);
            }
            for notes in by_part.values() {
                for (i, n) in notes.iter().enumerate() {
                    if pcs.contains(&(n.note % 12)) {
                        continue;
                    }
                    let resolves = notes.get(i + 1).map_or(true, |nx| {
                        (nx.note - n.note).abs() == 1 || nx.tick - n.tick > 8
                    });
                    assert!(
                        n.group == "bass" && resolves,
                        "{genre}/{seed} {} tick {}: {} out of key",
                        n.net,
                        n.tick,
                        n.note
                    );
                }
            }
        }
    }
}

/// The composed B / C sections really are out of key.
#[test]
fn composed_variants_were_out_of_key() {
    let t = played("edm", 42.0);
    assert!(report(&t.composed).in_key < 0.95);
    assert_eq!(report(&t.played).in_key, 1.0);
}

/// The bass starts each bar on the chord's root.
#[test]
fn bass_on_the_chord_root() {
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "dnb",
        "dubstep",
        "synthwave",
    ] {
        let r = report(&played(genre, 42.0).played);
        let on_root = r.bass_on_root.expect("bassOnRoot");
        assert!(
            on_root >= 0.95,
            "{genre}: bass on the root in {on_root} of bars"
        );
    }
}

/// Structure changes take effect at bar lines, and a part that enters a section plays its
/// downbeat.
#[test]
fn sections_start_on_the_downbeat() {
    for genre in ["techno", "edm", "jazz", "lofi"] {
        let t = played(genre, 42.0);
        let (notes, controls) = capture_all(&t.played, None);
        assert!(!controls.is_empty());
        for c in &controls {
            assert_eq!(
                c.tick % 16,
                0,
                "{genre}: {} changes at tick {}",
                c.net,
                c.tick
            );
        }
        let mut start = 0;
        let mut checked = 0;
        for s in &t.played.structure[..t.played.structure.len() - 1] {
            start += s.steps;
            for n in notes.iter().filter(|n| {
                (n.net.starts_with("kick") || n.group == "harmony") && n.tick == start + 17
            }) {
                checked += 1;
                assert!(
                    notes.iter().any(|m| m.net == n.net && m.tick == start + 1),
                    "{genre}: {} misses the downbeat at step {start}",
                    n.net
                );
            }
        }
        assert!(checked > 0);
        // The composed project does lose them (else this proves nothing).
        let before = capture_all(&t.composed, None).1;
        assert!(before.iter().filter(|c| c.tick % 16 == 0).count() < before.len() / 2);
    }
}

/// Bass, pad and melody each in their own register.
#[test]
fn parts_in_their_own_register() {
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "synthwave",
        "dubstep",
        "dnb",
        "lofi",
        "jazz",
        "bossa",
    ] {
        let t = played(genre, 42.0);
        let r = report(&t.played);
        let bass = r.median_note["bass"];
        assert!(
            bass <= BASS_CEILING && bass >= BASS_FLOOR,
            "{genre} bass {bass}"
        );
        if let Some(overlap) = r.harmony_in_bass_range {
            assert_eq!(overlap, 0.0, "{genre}: pad in the bass range");
        }
        if let Some(pad) = r.median_note.get("harmony") {
            assert!(*pad >= PAD_FLOOR, "{genre} pad {pad}");
        }
        if let (Some(pad), Some(lead)) = (r.median_note.get("harmony"), r.median_note.get("melody"))
        {
            assert!(lead > pad, "{genre}: melody {lead} under the pad {pad}");
        }
        // Octaves only: the same notes in the same order, pitch classes unchanged.
        let without = played_without(&t, Fixes::REGISTER);
        let pcs =
            |p: &Project| -> Vec<i64> { capture(p, None).iter().map(|e| e.note % 12).collect() };
        assert_eq!(pcs(&without), pcs(&played_without(&t, Fixes::EMPTY)));
    }
}

/// Melodic parts accented by meter.
#[test]
fn melodic_parts_have_metric_accents() {
    for genre in ["techno", "edm", "house", "lofi"] {
        let t = played(genre, 42.0);
        let notes: Vec<NoteEvent> = capture(&t.played, None)
            .into_iter()
            .filter(|e| e.group == "melody" || e.group == "bass")
            .collect();
        let down: Vec<i64> = notes
            .iter()
            .filter(|e| (e.tick - 1) % 16 == 0)
            .map(|e| e.velocity)
            .collect();
        let offs: Vec<i64> = notes
            .iter()
            .filter(|e| (e.tick - 1) % 2 == 1)
            .map(|e| e.velocity)
            .collect();
        assert!(!down.is_empty() && !offs.is_empty());
        let mean = |v: &[i64]| v.iter().sum::<i64>() as f64 / v.len() as f64;
        assert!(
            mean(&down) >= mean(&offs) + 10.0,
            "{genre}: downbeats {} vs off {}",
            mean(&down),
            mean(&offs)
        );
        assert!(
            report(&t.played)
                .velocity_spread
                .get("melody")
                .copied()
                .unwrap_or(0.0)
                > 4.0
        );
        // The composed track had none (the test must be able to fail).
        assert!(
            report(&t.composed)
                .velocity_spread
                .get("melody")
                .copied()
                .unwrap_or(0.0)
                < 1.0
                || genre == "edm"
        );
    }
}

/// Every genre's busiest section at about one loudness: the gain `Loudness` picks brings
/// genres that were 10 dB apart to within 1.5 dB of the target.
#[test]
fn tracks_play_at_one_loudness() {
    let mut raw = vec![];
    for genre in ["techno", "lofi", "ambient", "metal"] {
        let p = played(genre, 42.0).played;
        let Some(db) = Loudness::measure(&p, None, Loudness::PROBE_RATE) else {
            panic!("{genre} measured silent");
        };
        raw.push(db);
        let g = Loudness::gain(
            &p,
            Fixes::EMPTY,
            None,
            &Default::default(),
            false,
            None,
            None,
            1.0,
        );
        let after = db + 20.0 * g.log10();
        assert!(
            (after - Loudness::target_db()).abs() < 1.5,
            "{genre}: {db} dB × {g} → {after}"
        );
    }
    let hi = raw.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let lo = raw.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(hi - lo > 6.0, "the genres must start apart: {raw:?}");
    // lofi/42 is the quietest of them
    assert!(
        Loudness::gain(
            &played("lofi", 42.0).played,
            Fixes::EMPTY,
            None,
            &Default::default(),
            false,
            None,
            None,
            1.0
        ) > 1.2
    );
}

/// In four-on-the-floor dance genres the bass leaves the kick's steps, and still starts each
/// bar on the chord's root.
#[test]
fn bass_off_the_kick_in_four_on_the_floor() {
    for genre in ["techno", "edm", "trance"] {
        let t = played(genre, 42.0);
        assert!(report(&t.composed).bass_on_kick.unwrap() > 0.7);
        let r = report(&t.played);
        assert!(r.bass_on_kick.unwrap() < 0.1, "{genre}");
        assert!(r.bass_on_root.unwrap() >= 0.95);
    }
    // House (its own groove), dnb, dubstep keep their bass lines.
    let h = played("house", 42.0);
    let ticks = |p: &Project| -> Vec<i64> {
        capture(p, None)
            .into_iter()
            .filter(|e| e.group == "bass")
            .map(|e| e.tick)
            .collect()
    };
    assert_eq!(ticks(&h.played), ticks(&with(&h, Fixes::OFFBEAT_BASS)));
}

/// Phrase changes on the 2-bar grid from their section's start.
#[test]
fn phrase_changes_on_the_two_bar_grid() {
    assert_eq!(phrase_lengths(192, 4), vec![64, 64, 32, 32]);
    assert_eq!(phrase_lengths(192, 2), vec![128, 64]);
    assert_eq!(phrase_lengths(128, 4), vec![32, 32, 32, 32]);
    assert_eq!(phrase_lengths(64, 2), vec![32, 32]);
    for genre in ["techno", "edm", "jazz"] {
        let t = played(genre, 42.0);
        assert_eq!(
            report(&t.played).changes_on_phrases.unwrap(),
            1.0,
            "{genre}"
        );
        assert!(
            report(&t.composed).changes_on_phrases.unwrap() < 0.8,
            "{genre} composed"
        );
    }
}

/// A drum fill in the bar before every section change, swelling into it.
#[test]
fn drum_fills_before_section_changes() {
    for genre in ["techno", "edm", "jazz", "lofi"] {
        let t = played(genre, 42.0);
        assert_eq!(
            report(&t.played).fills_before_sections.unwrap(),
            1.0,
            "{genre}"
        );
        assert_eq!(report(&t.composed).fills_before_sections.unwrap(), 0.0);
        let fills: Vec<NoteEvent> = capture(&t.played, None)
            .into_iter()
            .filter(|e| e.net.starts_with("fill-") && e.note != CRASH_NOTE)
            .collect();
        let mut by_net: BTreeMap<&str, Vec<&NoteEvent>> = BTreeMap::new();
        for e in &fills {
            by_net.entry(e.net.as_str()).or_default().push(e);
        }
        for (_, mut roll) in by_net {
            roll.sort_by_key(|e| e.tick);
            let v: Vec<i64> = roll.iter().map(|e| e.velocity).collect();
            let mut s = v.clone();
            s.sort();
            assert!(
                v == s && v[0] < *v.last().unwrap(),
                "{genre}: a fill swells: {v:?}"
            );
        }
    }
}

/// The pad's chords lead into each other.
#[test]
fn pad_chords_lead_into_each_other() {
    for genre in [
        "techno",
        "trance",
        "dubstep",
        "synthwave",
        "ambient",
        "jazz",
    ] {
        let t = played(genre, 42.0);
        let blocks = apply(
            &t.composed,
            &t.spec,
            Fixes::ALL & !(Fixes::VOICING | Fixes::COLOR),
        );
        let led = apply(&t.composed, &t.spec, Fixes::ALL & !Fixes::COLOR);
        let before = report(&blocks).pad_motion.expect("padMotion");
        let after = report(&led).pad_motion.expect("padMotion");
        assert!(after < before, "{genre}: {before} → {after}");
        assert!(after <= 5.0, "{genre}: {after} semitones per chord change");
        let pad = |p: &Project| -> Vec<i64> {
            capture(p, None)
                .into_iter()
                .filter(|e| e.group == "harmony")
                .map(|e| e.note % 12)
                .collect()
        };
        assert_eq!(pad(&blocks), pad(&led));
        assert!(report(&t.played).pad_motion.unwrap() <= 5.0);
        assert_eq!(report(&t.played).harmony_in_bass_range, Some(0.0));
    }
}

/// Melodies step instead of stalling on one pitch.
#[test]
fn melodies_step_rather_than_repeat() {
    for genre in ["techno", "trance", "jazz", "dubstep"] {
        let t = played(genre, 42.0);
        let flat = played_without(
            &t,
            Fixes::CONTOUR | Fixes::VARIETY | Fixes::SHAPE | Fixes::HOOK,
        );
        let stepped = played_without(&t, Fixes::VARIETY | Fixes::SHAPE | Fixes::HOOK);
        assert!(
            report(&played_without(&t, Fixes::EMPTY))
                .melody_repeats
                .unwrap()
                < 0.45,
            "{genre} as played"
        );
        let a = report(&flat);
        let b = report(&stepped);
        let before = a.melody_repeats.unwrap();
        let after = b.melody_repeats.unwrap();
        assert!(
            after < 0.7 * before && after < 0.45,
            "{genre}: repeats {before} → {after}"
        );
        assert!(b.melody_steps.unwrap() > a.melody_steps.unwrap_or(0.0) + 0.1);
        assert!(b.semitone_clashes <= a.semitone_clashes + 0.1);
        assert!(b.melody_recurrence.unwrap() >= a.melody_recurrence.unwrap_or(0.0) - 0.1);
        let on_beats = |p: &Project| -> Vec<i64> {
            capture(p, None)
                .into_iter()
                .filter(|e| e.group == "melody" && (e.tick - 1) % 4 == 0)
                .map(|e| e.note)
                .collect()
        };
        assert_eq!(on_beats(&flat), on_beats(&stepped));
    }
}

/// Sevenths where the style has them.
#[test]
fn pad_chords_are_sevenths_where_the_style_is() {
    for genre in ["jazz", "bossa", "house", "garage", "funk"] {
        let mut color = 0.0;
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let triads = with(&t, Fixes::COLOR);
            let a = report(&triads);
            let b = report(&t.played);
            let c = b.pad_color.expect("padColor");
            assert!(c > 0.0, "{genre}/{seed}");
            color += c;
            assert_eq!(a.pad_color, Some(0.0));
            assert!(b.in_key >= a.in_key);
            assert_eq!(b.harmony_in_bass_range, Some(0.0));
            let plain = Fixes::ALL & !Fixes::VARIETY;
            let x = report(&apply(&t.composed, &t.spec, plain & !Fixes::COLOR));
            let y = report(&apply(&t.composed, &t.spec, plain));
            assert!(
                y.semitone_clashes <= x.semitone_clashes + 1.0,
                "{genre}/{seed}"
            );
        }
        assert!(color / 2.0 >= 0.5, "{genre}: pad colour {}", color / 2.0);
        let techno = played("techno", 42.0);
        assert_eq!(
            with(&techno, Fixes::COLOR).canonical_json(),
            techno.played.canonical_json()
        );
    }
}

/// On `.variety`'s progressions too, as the deck plays them.
#[test]
fn seventhss_do_not_add_rubs_on_varietys_progressions() {
    for genre in ["house", "garage", "jazz", "bossa", "funk"] {
        let mut color = 0.0;
        let seeds = [1.0, 7.0, 13.0, 21.0, 42.0, 64.0, 99.0, 123.0, 777.0, 2024.0];
        for seed in seeds {
            let t = played(genre, seed);
            let triads = report(&with(&t, Fixes::COLOR));
            let sevenths = report(&t.played);
            assert!(
                sevenths.semitone_clashes <= triads.semitone_clashes + 1.0,
                "{genre}/{seed}: rubs {} → {}",
                triads.semitone_clashes,
                sevenths.semitone_clashes
            );
            color += sevenths.pad_color.unwrap();
        }
        let floor = if genre == "funk" { 0.45 } else { 0.6 };
        assert!(
            color / seeds.len() as f64 >= floor,
            "{genre}: pad colour {}",
            color / seeds.len() as f64
        );
    }
}

/// The arp plays the chords.
#[test]
fn arp_plays_the_chords() {
    for genre in ["trance", "edm", "synthwave"] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let motif = with(&t, Fixes::ARPEGGIO | Fixes::HOOK);
            let a = report(&motif);
            let b = report(&t.played);
            assert!(
                a.arp_doubles_melody.unwrap() > 0.5,
                "{genre}/{seed}: composed arp doubles"
            );
            assert_eq!(b.arp_chord_tones, Some(1.0), "{genre}/{seed}");
            assert!(b.arp_doubles_melody.unwrap() < 0.3, "{genre}/{seed}");
            assert!(b.in_key >= a.in_key);
            let steady = with(&t, Fixes::ARP_DENSITY);
            let mut by_bar: BTreeMap<i64, usize> = BTreeMap::new();
            for e in capture(&steady, None).iter().filter(|e| e.group == "arp") {
                *by_bar.entry((e.tick - 1) / 16).or_insert(0) += 1;
            }
            for (_, n) in by_bar {
                assert!(n >= 8, "{genre}/{seed}: {n} arp notes in a bar");
            }
            assert!(b.median_note["arp"] > b.median_note.get("harmony").copied().unwrap_or(0));
        }
    }
}

/// Every buildup builds into what follows.
#[test]
fn buildups_roll_into_the_drop() {
    for genre in ["techno", "edm", "trance", "dnb", "dubstep"] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            if !t.played.structure[..t.played.structure.len() - 1]
                .iter()
                .any(|s| s.name == "buildup")
            {
                continue;
            }
            assert_eq!(
                report(&t.played).buildups_build,
                Some(1.0),
                "{genre}/{seed}"
            );
            let without = with(&t, Fixes::BUILDUP);
            assert_eq!(report(&without).buildups_build, Some(0.0));
            let mut end = 0;
            for s in &t.played.structure[..t.played.structure.len() - 1] {
                end += s.steps;
                if s.name != "buildup" {
                    continue;
                }
                let mut roll: Vec<NoteEvent> = capture(&t.played, None)
                    .into_iter()
                    .filter(|e| e.net.starts_with("fill-") && e.tick > end - 128 && e.tick <= end)
                    .collect();
                roll.sort_by_key(|e| e.tick);
                let v: Vec<i64> = roll.iter().map(|e| e.velocity).collect();
                let mut sorted = v.clone();
                sorted.sort();
                assert!(
                    v == sorted && v[0] + 40 < *v.last().unwrap(),
                    "{genre}/{seed}: {v:?}"
                );
                assert_eq!(
                    roll.last().map(|e| e.tick),
                    Some(end),
                    "the roll lands on the change"
                );
                let last16: Vec<i64> = roll[roll.len() - 16..].iter().map(|e| e.tick).collect();
                assert_eq!(
                    last16,
                    ((end - 15)..=end).collect::<Vec<_>>(),
                    "sixteenths at the end"
                );
            }
        }
    }
}

/// A crash on the downbeat of every section after the first.
#[test]
fn sections_start_with_a_crash() {
    for genre in ["techno", "edm", "jazz", "lofi", "funk"] {
        let t = played(genre, 42.0);
        assert_eq!(report(&t.played).crash_on_sections, Some(1.0), "{genre}");
        assert_eq!(report(&t.composed).crash_on_sections, Some(0.0));
        let crashes: Vec<NoteEvent> = capture(&t.played, None)
            .into_iter()
            .filter(|e| e.is_drum() && e.note == 49)
            .collect();
        assert_eq!(
            crashes.len(),
            t.played.structure.len() - 1,
            "one per section change"
        );
        if let Some(drop) = t
            .played
            .structure
            .iter()
            .position(|s| s.name == "drop")
            .filter(|d| *d > 0)
        {
            let at: i64 = t.played.structure[..drop]
                .iter()
                .map(|s| s.steps)
                .sum::<i64>()
                + 1;
            let loud = crashes.iter().find(|e| e.tick == at).unwrap().velocity;
            assert!(crashes.iter().all(|e| e.velocity <= loud));
        }
    }
}

/// The bass steps between chord roots instead of leaping.
#[test]
fn bass_steps_between_roots() {
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "dnb",
        "synthwave",
        "jazz",
        "funk",
    ] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let leaping = played_without(&t, Fixes::BASS_LINE);
            let stepping = played_without(&t, Fixes::EMPTY);
            let a = report(&leaping);
            let b = report(&stepping);
            let before = a.bass_leap.unwrap();
            let after = b.bass_leap.unwrap();
            assert!(after <= before, "{genre}/{seed}: {before} → {after}");
            assert!(after <= 5.0, "{genre}/{seed}: {after} semitones per change");
            let bass = |p: &Project| -> Vec<i64> {
                capture(p, None)
                    .into_iter()
                    .filter(|e| e.group == "bass")
                    .map(|e| e.note % 12)
                    .collect()
            };
            assert_eq!(bass(&leaping), bass(&stepping));
            assert!(b.bass_on_root.unwrap() >= a.bass_on_root.unwrap_or(0.0));
            assert_eq!(b.harmony_in_bass_range, Some(0.0));
            let median = b.median_note["bass"];
            assert!(median >= BASS_FLOOR && median <= BASS_CEILING + 2);
        }
    }
}

/// Four-on-the-floor dance genres open the hat on every off-beat eighth it plays.
#[test]
fn offbeat_hats_open() {
    for genre in ["techno", "house", "edm", "trance"] {
        let t = played(genre, 42.0);
        let hats: Vec<NoteEvent> = capture(&t.played, None)
            .into_iter()
            .filter(|e| e.is_drum() && e.net.starts_with("hihat"))
            .collect();
        let offbeat: Vec<&NoteEvent> = hats.iter().filter(|e| (e.tick - 1) % 4 == 2).collect();
        assert!(!offbeat.is_empty());
        assert!(offbeat.iter().all(|e| e.note == OPEN_HAT_NOTE), "{genre}");
        assert!(!hats
            .iter()
            .any(|e| (e.tick - 1) % 4 != 2 && e.note == OPEN_HAT_NOTE));
        let closed = with(&t, Fixes::OPEN_HATS);
        let before: Vec<NoteEvent> = capture(&closed, None)
            .into_iter()
            .filter(|e| e.is_drum() && e.net.starts_with("hihat"))
            .collect();
        assert_eq!(
            before.iter().map(|e| e.tick).collect::<Vec<_>>(),
            hats.iter().map(|e| e.tick).collect::<Vec<_>>()
        );
        assert_eq!(
            before.iter().map(|e| e.velocity).collect::<Vec<_>>(),
            hats.iter().map(|e| e.velocity).collect::<Vec<_>>()
        );
        for other in ["dnb", "jazz", "lofi"] {
            let o = played(other, 42.0);
            assert_eq!(
                o.played.canonical_json(),
                with(&o, Fixes::OPEN_HATS).canonical_json(),
                "{other}"
            );
        }
    }
}

/// Melody notes last until the next one (at most a beat).
#[test]
fn melodies_are_held() {
    for genre in ["techno", "trance", "funk", "lofi", "jazz", "ambient"] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let blips = with(&t, Fixes::LEGATO);
            let before = report(&blips).melody_legato.unwrap();
            let after = report(&t.played).melody_legato.unwrap();
            assert!(before < 0.9, "{genre}/{seed}: already held ({before})");
            assert!(after >= 0.99, "{genre}/{seed}: {after}");
            let melody = |p: &Project| -> Vec<NoteEvent> {
                capture(p, None)
                    .into_iter()
                    .filter(|e| e.group == "melody")
                    .collect()
            };
            let a = melody(&blips);
            let b = melody(&t.played);
            assert_eq!(
                a.iter().map(|e| e.tick).collect::<Vec<_>>(),
                b.iter().map(|e| e.tick).collect::<Vec<_>>()
            );
            assert_eq!(
                a.iter().map(|e| e.note).collect::<Vec<_>>(),
                b.iter().map(|e| e.note).collect::<Vec<_>>()
            );
            assert_eq!(
                a.iter().map(|e| e.velocity).collect::<Vec<_>>(),
                b.iter().map(|e| e.velocity).collect::<Vec<_>>()
            );
            assert!(b.iter().all(|e| e.steps <= LEGATO_MAX as f64));
        }
    }
}

/// A melody phrase ends on a tone of the bar's chord.
#[test]
fn phrases_end_on_chord_tones() {
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "dnb",
        "dubstep",
        "synthwave",
        "funk",
    ] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let open = apply(
                &t.composed,
                &t.spec,
                Fixes::ALL & !(Fixes::CADENCE | Fixes::MOTIF | Fixes::ANSWER),
            );
            let closed = apply(
                &t.composed,
                &t.spec,
                Fixes::ALL & !(Fixes::MOTIF | Fixes::ANSWER),
            );
            let closes = report(&apply(&t.composed, &t.spec, Fixes::ALL & !Fixes::ANSWER))
                .phrase_ends_on_chord_tones
                .unwrap();
            assert!(closes >= 0.95, "{genre}/{seed}: {closes}");
            let after = report(&t.played).phrase_ends_on_chord_tones.unwrap();
            assert!(after >= 0.85, "{genre}/{seed}: {after}");
            assert!(report(&closed).phrase_ends_on_chord_tones.unwrap() >= 0.95);
            let melody = |p: &Project| -> Vec<NoteEvent> {
                capture(p, None)
                    .into_iter()
                    .filter(|e| e.group == "melody")
                    .collect()
            };
            let a = melody(&open);
            let b = melody(&closed);
            assert_eq!(
                a.iter().map(|e| e.tick).collect::<Vec<_>>(),
                b.iter().map(|e| e.tick).collect::<Vec<_>>()
            );
            assert!(a.iter().zip(&b).all(|(x, y)| (x.note - y.note).abs() <= 5));
            assert!(report(&closed).in_key >= report(&open).in_key);
        }
    }
    // the open endings were really there
    let t = played("techno", 42.0);
    let open = with(&t, Fixes::CADENCE);
    assert!(report(&open).phrase_ends_on_chord_tones.unwrap() < 0.8);
}

/// Where the bass line moves, it walks into the next chord.
#[test]
fn bass_approaches_chord_changes() {
    for genre in ["house", "dnb", "dubstep", "synthwave", "funk"] {
        for seed in [7.0, 42.0] {
            let t = played(genre, seed);
            let plain = with(&t, Fixes::APPROACH);
            let before = report(&plain).bass_stepwise.unwrap();
            let after = report(&t.played).bass_stepwise.unwrap();
            assert!(after >= 0.95, "{genre}/{seed}: {before} → {after}");
            assert!(after >= before);
            let bass = |p: &Project| -> Vec<NoteEvent> {
                capture(p, None)
                    .into_iter()
                    .filter(|e| e.group == "bass")
                    .collect()
            };
            let a = bass(&plain);
            let b = bass(&t.played);
            assert_eq!(
                a.iter().map(|e| e.tick).collect::<Vec<_>>(),
                b.iter().map(|e| e.tick).collect::<Vec<_>>()
            );
            let pcs = scale_pcs(&t.played);
            for (x, y) in a.iter().zip(&b) {
                if x.note != y.note {
                    assert!(
                        (x.tick - 1) % 16 >= 8,
                        "{genre}/{seed}: moved a first-half note at {}",
                        x.tick
                    );
                    assert!(pcs.contains(&(y.note % 12)));
                }
            }
            let rooted = report(&plain).bass_on_root.unwrap();
            assert!(report(&t.played).bass_on_root.unwrap() >= rooted);
        }
    }
    for other in ["techno", "trance", "jazz"] {
        let o = played(other, 42.0);
        assert_eq!(
            o.played.canonical_json(),
            with(&o, Fixes::APPROACH).canonical_json(),
            "{other}"
        );
    }
}

/// Each 4-bar melody phrase states its idea twice.
#[test]
fn ideas_are_stated_twice() {
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
            let t = played(genre, seed);
            let plain = apply(
                &t.composed,
                &t.spec,
                Fixes::ALL & !(Fixes::MOTIF | Fixes::ARP_DENSITY),
            );
            let stated = apply(&t.composed, &t.spec, Fixes::ALL & !Fixes::ARP_DENSITY);
            let a = report(&plain);
            let b = report(&stated);
            let before = a.melody_idea_repeats.unwrap();
            let after = b.melody_idea_repeats.unwrap();
            assert!(before < 0.4, "{genre}/{seed}: already repeats ({before})");
            assert!(after >= 0.8, "{genre}/{seed}: {before} → {after}");
            let pcs = scale_pcs(&stated);
            let out_of_key = |p: &Project| -> usize {
                capture(p, None)
                    .iter()
                    .filter(|e| e.group == "melody" && !pcs.contains(&(e.note % 12)))
                    .count()
            };
            assert!(
                out_of_key(&stated) <= out_of_key(&plain),
                "{genre}/{seed}: in key {} → {}",
                a.in_key,
                b.in_key
            );
            assert!(
                b.chord_tones_on_beats.unwrap() >= a.chord_tones_on_beats.unwrap_or(0.0) - 0.02,
                "{genre}/{seed}"
            );
            assert!(
                b.phrase_ends_on_chord_tones.unwrap()
                    >= a.phrase_ends_on_chord_tones.unwrap_or(0.0) - 0.02
            );
            // only melody bars 2 (mod 4) changed
            let other = |p: &Project| -> Vec<NoteEvent> {
                capture(p, None)
                    .into_iter()
                    .filter(|e| e.group != "melody" || (e.tick - 1) % 64 / 16 != 1)
                    .collect()
            };
            let x = other(&plain);
            let y = other(&stated);
            assert_eq!(
                x.iter().map(|e| e.tick).collect::<Vec<_>>(),
                y.iter().map(|e| e.tick).collect::<Vec<_>>(),
                "{genre}/{seed}"
            );
            assert_eq!(
                x.iter().map(|e| e.note).collect::<Vec<_>>(),
                y.iter().map(|e| e.note).collect::<Vec<_>>(),
                "{genre}/{seed}"
            );
        }
    }
}
