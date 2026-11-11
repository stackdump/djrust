//! Ports of Swift `BackbeatTests` and `HookTests` (`.backbeat`, `.hook`): property checks on
//! what the passes do to composed tracks. No Swift oracle on this box.

use djrust::gen::structure::{build_accumulator_net, StructureEvent};
use djrust::gen::tables::BeatsTables;
use djrust::gen::techniques::{build_hook, hook_mode, HookCell};
use djrust::gen::theme::motif_mode;
use djrust::gen::variety::drum_fill_net;
use djrust::json::JsonValue;
use djrust::musicality::backbeat::{backbeat, beat_style, BACKBEAT_ROLES};
use djrust::musicality::dj::pad_chords;
use djrust::musicality::hook::{hook_bar, place_hook};
use djrust::musicality::note_log::capture_all;
use djrust::musicality::report::MusicalityReport;
use djrust::musicality::slot_windows::slot_windows;
use djrust::musicality::util::{is_bar_ring, pc, ring_notes};
use djrust::musicality::{apply, Fixes};
use djrust::pflow::{ControlBinding, NetBundle, Project, Section, Track};
use djrust::TrackSpec;

fn chill() -> [&'static str; 3] {
    ["lofi", "jazz", "bossa"]
}
const SEEDS: [f64; 3] = [42.0, 7.0, 1234.0];

fn composed(genre: &str, seed: f64) -> (TrackSpec, Project) {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    let p = spec.project();
    (spec, p)
}

fn report(p: &Project, genre: &str) -> MusicalityReport {
    let (notes, controls) = capture_all(p, None);
    MusicalityReport::new(p, &notes, &controls, Some(genre))
}

/// Bound transition indices of a net, in order.
fn hits(nb: &NetBundle) -> Vec<i64> {
    let mut v: Vec<i64> = nb
        .bindings
        .keys()
        .iter()
        .filter_map(|k| k.strip_prefix('t').and_then(|s| s.parse().ok()))
        .collect();
    v.sort();
    v
}

/// A one-bar drum ring on channel 10 with `note` on `steps`.
fn drum_ring(note: i64, steps: &[i64]) -> NetBundle {
    let mut nb = NetBundle::ring(16);
    for s in steps {
        nb.bind(*s, note, 10, 100, 50);
    }
    nb.track = Track {
        channel: 10,
        default_velocity: 100,
        group: Some("drums".into()),
        ..Default::default()
    };
    nb
}

/// Notes of net `id` in each section of `p`.
fn per_section(p: &Project, id: &str) -> Vec<usize> {
    let (log, _) = capture_all(p, None);
    let mut start = 0;
    p.structure
        .iter()
        .map(|s| {
            let n = log
                .iter()
                .filter(|e| e.net == id && e.tick > start && e.tick <= start + s.steps)
                .count();
            start += s.steps;
            n
        })
        .collect()
}

/// The exit: lofi, jazz and bossa have a kick and snare in every section their style expects
/// one, at three seeds, with the pass alone and with every fix.
#[test]
fn chill_family_has_a_beat() {
    for genre in chill() {
        for seed in SEEDS {
            let (spec, p) = composed(genre, seed);
            let alone = apply(&p, &spec, Fixes::TIMING | Fixes::BACKBEAT);
            let r = report(&alone, genre);
            assert_eq!(
                r.has_beat,
                Some(1.0),
                "{genre} {seed} backbeat: {:?}",
                r.beat_by_section
            );
            let all = report(&apply(&p, &spec, Fixes::ALL), genre);
            assert_eq!(
                all.has_beat,
                Some(1.0),
                "{genre} {seed} all: {:?}",
                all.beat_by_section
            );
        }
    }
}

/// Sections outside the style keep what the composer gave them.
#[test]
fn other_sections_keep_the_composers_parts() {
    for genre in chill() {
        let style = beat_style(genre).unwrap();
        for seed in SEEDS {
            let (spec, p) = composed(genre, seed);
            let played = apply(&p, &spec, Fixes::TIMING | Fixes::BACKBEAT);
            for role in BACKBEAT_ROLES {
                let before = per_section(&apply(&p, &spec, Fixes::TIMING), role);
                let after = per_section(&played, role);
                for (k, s) in p.structure.iter().enumerate() {
                    if !style.contains(&s.name.as_str()) {
                        assert_eq!(
                            before[k] > 0,
                            after[k] > 0,
                            "{genre} {seed} {role} in {}",
                            s.name
                        );
                    } else {
                        assert!(after[k] > 0, "{genre} {seed} {role} in {}", s.name);
                    }
                }
            }
        }
    }
}

/// The part comes in on the section's downbeat bar and is gone by the next section's.
#[test]
fn edges_land_on_section_starts() {
    let (spec, p) = composed("lofi", 42.0);
    let played = apply(&p, &spec, Fixes::TIMING | Fixes::BACKBEAT);
    let style = beat_style("lofi").unwrap();
    let (log, _) = capture_all(&played, None);
    let mut start = 0;
    for s in &played.structure {
        let snares: Vec<_> = log
            .iter()
            .filter(|e| e.net == "snare" && e.tick > start && e.tick <= start + s.steps)
            .collect();
        if style.contains(&s.name.as_str()) {
            assert!(
                snares.first().map_or(false, |e| e.tick <= start + 16),
                "{} at {start}",
                s.name
            );
        } else {
            assert!(snares.is_empty(), "{} at {start}", s.name);
        }
        start += s.steps;
    }
}

/// Each family's figure on the composed rings.
#[test]
fn figures_by_genre() {
    let nets = |genre: &str| {
        let (spec, p) = composed(genre, 42.0);
        let q = apply(&p, &spec, Fixes::BACKBEAT);
        (
            q.nets.get("kick").unwrap().clone(),
            q.nets.get("snare").unwrap().clone(),
        )
    };
    let (lk, ls) = nets("lofi");
    assert_eq!(hits(&lk), vec![0, 7, 10]);
    assert_eq!(hits(&ls), vec![4, 12]);
    assert_eq!(ls.bindings.get("t4").map(|b| b.note), Some(38));

    let (jk, js) = nets("jazz");
    assert_eq!(
        jk.transitions.len(),
        12,
        "beats' jazz drums are 12-step rings"
    );
    assert_eq!(hits(&jk), vec![0, 4, 8]);
    assert_eq!(hits(&js), vec![0, 4, 8]);
    let kick_peak = jk.bindings.values().map(|b| b.velocity).max().unwrap();
    assert!(kick_peak < 80, "feathered: {kick_peak}");

    let (bk, bs) = nets("bossa");
    assert_eq!(hits(&bk), vec![0, 6, 8, 14]);
    assert!(bk.bindings.get("t8").unwrap().velocity > bk.bindings.get("t6").unwrap().velocity);
    assert_eq!(hits(&bs), vec![0, 3, 6, 10, 13]);
    assert!(bs.bindings.values().all(|b| b.note == 37), "rim");
}

/// Bindings and markings only: same nets, places, transitions and arcs; the CID is untouched.
#[test]
fn keeps_topology_and_cid() {
    for genre in chill() {
        for seed in SEEDS {
            let (spec, p) = composed(genre, seed);
            let cid = p.model_cid();
            let q = apply(&p, &spec, Fixes::TIMING | Fixes::BACKBEAT);
            assert!(
                p.model_cid() == cid && spec.project().model_cid() == cid,
                "{genre} {seed}"
            );
            assert_eq!(q.nets.keys(), p.nets.keys());
            for id in p.nets.keys() {
                let a = p.nets.get(id).unwrap();
                let b = q.nets.get(id).unwrap();
                assert_eq!(a.places.keys(), b.places.keys(), "{genre} {id}");
                assert_eq!(a.transitions.keys(), b.transitions.keys(), "{genre} {id}");
                let arcs = |n: &NetBundle| -> Vec<String> {
                    n.arcs
                        .iter()
                        .map(|x| format!("{}>{}{}", x.source, x.target, x.inhibit))
                        .collect()
                };
                assert_eq!(arcs(a), arcs(b), "{genre} {id}");
                assert!(
                    b.bindings
                        .keys()
                        .iter()
                        .all(|k| b.transitions.contains_key(k)),
                    "{genre} {id}"
                );
                assert!(
                    b.control_bindings
                        .keys()
                        .iter()
                        .all(|k| b.transitions.contains_key(k)),
                    "{genre} {id}"
                );
                if !id.starts_with("struct-") {
                    let init = |n: &NetBundle| -> Vec<Vec<f64>> {
                        n.places.values().map(|p| p.initial.clone()).collect()
                    };
                    assert_eq!(init(a), init(b), "{genre} {id}");
                }
            }
            assert_eq!(
                q.structure.iter().map(|s| s.steps).collect::<Vec<_>>(),
                p.structure.iter().map(|s| s.steps).collect::<Vec<_>>()
            );
        }
    }
}

/// Ambient (no beat style) and the genres outside the family are untouched.
#[test]
fn other_genres_unchanged() {
    for genre in ["ambient", "techno", "funk"] {
        let (spec, p) = composed(genre, 42.0);
        assert_eq!(
            apply(&p, &spec, Fixes::ALL).canonical_json(),
            apply(&p, &spec, Fixes::ALL & !Fixes::BACKBEAT).canonical_json(),
            "{genre}"
        );
        let mut q = p.clone();
        assert!(backbeat(&mut q, genre).is_empty());
        assert_eq!(q.canonical_json(), p.canonical_json(), "{genre}");
    }
}

// MARK: synthetic projects

/// intro, verse, bridge, verse; the snare composed into the bridge only (two stages).
fn song(snare_stages: bool, fills: bool) -> Project {
    let mut p = Project::default();
    p.structure = [("intro", 64), ("verse", 64), ("bridge", 64), ("verse", 64)]
        .iter()
        .map(|(n, s)| Section {
            name: n.to_string(),
            steps: *s,
            phrases: Default::default(),
        })
        .collect();
    p.nets.insert("kick", drum_ring(36, &[0, 8]));
    p.nets.insert("snare", drum_ring(38, &[6]));
    p.nets.insert("struct-kick", build_accumulator_net(vec![]));
    p.nets.insert(
        "struct-snare",
        build_accumulator_net(if snare_stages {
            vec![
                StructureEvent {
                    tick: 128,
                    binding: ControlBinding::new("unmute-track", "snare"),
                },
                StructureEvent {
                    tick: 192,
                    binding: ControlBinding::new("mute-track", "snare"),
                },
            ]
        } else {
            vec![]
        }),
    );
    if fills {
        for (i, b) in [64, 128, 192, 256].iter().enumerate() {
            p.nets.insert(format!("fill-{i}"), drum_fill_net(*b, 4));
        }
    }
    p.initial_mutes = vec!["kick".into(), "snare".into()];
    p
}

/// With as many stages as edges, the struct net's own stages are retimed (no fill chain
/// needed); the composer's bridge snare stays.
#[test]
fn retimes_the_struct_nets_own_stages() {
    let mut p = song(true, false);
    let skipped = backbeat(&mut p, "lofi");
    assert!(!skipped.contains_key("snare"), "{skipped:?}");
    assert_eq!(
        per_section(&p, "snare")
            .iter()
            .map(|n| *n > 0)
            .collect::<Vec<_>>(),
        [false, true, true, true]
    );
    assert_eq!(
        p.nets
            .get("struct-snare")
            .unwrap()
            .places
            .keys()
            .iter()
            .filter(|k| k.starts_with("p_delay_"))
            .count(),
        2
    );
    // no fill chains to carry the kick's two edges: it is left as composed
    assert!(skipped
        .get("kick")
        .map_or(false, |s| s.starts_with("no stage or fill chain")));
    assert!(per_section(&p, "kick").iter().all(|n| *n == 0));
}

/// With too few stages, the edges ride on the fill chains, and the struct net agrees.
#[test]
fn carries_edges_on_fill_chains() {
    let mut p = song(false, true);
    let skipped = backbeat(&mut p, "lofi");
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(
        per_section(&p, "kick")
            .iter()
            .map(|n| *n > 0)
            .collect::<Vec<_>>(),
        [false, true, false, true]
    );
    assert_eq!(
        per_section(&p, "snare")
            .iter()
            .map(|n| *n > 0)
            .collect::<Vec<_>>(),
        [false, true, false, true]
    );
    assert_eq!(hits(p.nets.get("snare").unwrap()), vec![4, 12]);
    let carried: usize = p
        .nets
        .iter()
        .filter(|(k, _)| k.starts_with("fill-"))
        .map(|(_, nb)| nb.control_bindings.len())
        .sum();
    assert_eq!(
        carried, 6,
        "kick and snare in at each verse, out at the bridge"
    );
    assert!(
        p.initial_mutes.iter().any(|m| m == "kick") && p.initial_mutes.iter().any(|m| m == "snare")
    );
}

/// A song that starts with a beat drops the part's initial mute.
#[test]
fn drops_the_initial_mute() {
    let mut p = song(false, true);
    p.structure[0].name = "verse".into();
    backbeat(&mut p, "bossa");
    assert!(
        !p.initial_mutes.iter().any(|m| m == "kick")
            && !p.initial_mutes.iter().any(|m| m == "snare")
    );
    assert_eq!(
        per_section(&p, "snare")
            .iter()
            .map(|n| *n > 0)
            .collect::<Vec<_>>(),
        [true, true, false, true]
    );
}

/// A slotted part is skipped with a reason; the other part still comes in.
#[test]
fn skips_slotted_parts() {
    let mut p = song(false, true);
    p.nets.remove("kick");
    for i in 0..2 {
        let mut slot = drum_ring(36, &[0]);
        slot.riff_group = "kick".into();
        p.nets.insert(format!("kick-{i}"), slot);
    }
    let before = p.nets.get("kick-0").unwrap().bindings.keys().to_vec();
    let skipped = backbeat(&mut p, "lofi");
    assert!(skipped
        .get("kick")
        .map_or(false, |s| s.starts_with("slotted")));
    assert_eq!(p.nets.get("kick-0").unwrap().bindings.keys(), &before[..]);
    assert!(!skipped.contains_key("snare"));
    assert_eq!(
        per_section(&p, "snare")
            .iter()
            .map(|n| *n > 0)
            .collect::<Vec<_>>(),
        [false, true, false, true]
    );
}

// MARK: hook

const SONG_GENRES: [&str; 6] = ["techno", "house", "edm", "trance", "lofi", "synthwave"];

fn spec(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, Some("standard"))
}

fn played(s: &TrackSpec, fixes: Fixes) -> Project {
    apply(&s.project(), s, fixes)
}

fn moves(notes: &[i64]) -> Vec<i64> {
    notes.windows(2).map(|w| (w[1] - w[0]).signum()).collect()
}

/// A one-bar hook with a known shape: up, up, down, level.
fn cell() -> HookCell {
    let mut mask = vec![false; 16];
    let mut target = vec![-1i64; 16];
    for (s, t) in [(0usize, 0i64), (3, 2), (6, 4), (10, 2), (12, 2)] {
        mask[s] = true;
        target[s] = t;
    }
    HookCell { mask, target }
}

fn c_major() -> std::collections::BTreeSet<i64> {
    [0, 2, 4, 5, 7, 9, 11].into_iter().collect()
}

/// Each mode on a hand-built hook.
#[test]
fn modes_shape_the_bar() {
    let key = c_major();
    let scale: Vec<i64> = (0..=127).filter(|n| key.contains(&(n % 12))).collect();
    let c = cell();
    let bar = |mode: i64, chord: &[i64]| hook_bar(&c, mode, 72, chord, &key, &scale);
    let play = bar(motif_mode::PLAY, &[0, 4, 7]);
    assert_eq!(
        play.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![0, 3, 6, 10, 12]
    );
    assert_eq!(
        moves(&play.iter().map(|x| x.1).collect::<Vec<_>>()),
        vec![1, 1, -1, 0]
    );
    assert_eq!(play[0].1, 72);
    let transposed = bar(motif_mode::TRANSPOSED, &[0, 4, 7]);
    assert_eq!(
        transposed.iter().map(|x| x.0).collect::<Vec<_>>(),
        play.iter().map(|x| x.0).collect::<Vec<_>>()
    );
    assert_eq!(
        moves(&transposed.iter().map(|x| x.1).collect::<Vec<_>>()),
        vec![1, 1, -1, 0]
    );
    assert!(transposed[0].1 > play[0].1);
    let inverted = bar(motif_mode::INVERTED, &[0, 4, 7]);
    assert_eq!(
        inverted.iter().map(|x| x.0).collect::<Vec<_>>(),
        play.iter().map(|x| x.0).collect::<Vec<_>>()
    );
    assert_eq!(
        moves(&inverted.iter().map(|x| x.1).collect::<Vec<_>>()),
        vec![-1, -1, 1, 0]
    );
    assert_eq!(
        bar(motif_mode::FRAGMENT, &[0, 4, 7])
            .iter()
            .map(|x| x.0)
            .collect::<Vec<_>>(),
        vec![0, 3, 6]
    );
    assert_eq!(
        bar(motif_mode::AUGMENTED, &[0, 4, 7])
            .iter()
            .map(|x| x.0)
            .collect::<Vec<_>>(),
        vec![0, 6, 12]
    );
    for m in [
        motif_mode::PLAY,
        motif_mode::TRANSPOSED,
        motif_mode::INVERTED,
    ] {
        // on another chord (A minor) the shape stays and every note is its tone
        let am = bar(m, &[9, 0, 4]);
        assert!(
            am.iter().all(|x| [9, 0, 4].contains(&(x.1 % 12))),
            "mode {m}"
        );
        assert_eq!(
            moves(&am.iter().map(|x| x.1).collect::<Vec<_>>()),
            moves(&bar(m, &[0, 4, 7]).iter().map(|x| x.1).collect::<Vec<_>>()),
            "mode {m}"
        );
    }
}

/// In what a deck plays, every melody window's first bar is the hook: its sixteenths, on the
/// bar's chord, with its moves (turned in a bridge).
#[test]
fn every_melody_window_states_the_hook() {
    for genre in SONG_GENRES {
        let s = spec(genre, 42.0);
        let g = BeatsTables::shared().genres.get(genre).unwrap();
        let mut p = played(&s, Fixes::ALL & !Fixes::HOOK);
        place_hook(&mut p, g, &s, None);
        let hook = build_hook(genre, s.seed, 1, g.melody_density);
        let rhythm: Vec<i64> = (0..16)
            .filter(|&i| hook.mask[i])
            .map(|i| i as i64)
            .collect();
        let shape = moves(
            &rhythm
                .iter()
                .map(|&i| hook.target[i as usize])
                .collect::<Vec<_>>(),
        );
        let (chords, pad_bars) = pad_chords(&p);
        assert!(pad_bars > 0);
        let mut checked = 0;
        for (id, ws) in slot_windows(&p, "melody") {
            let nb = p.nets.get(&id).unwrap();
            let (true, Some(w)) = (is_bar_ring(nb), ws.first()) else {
                continue;
            };
            let Some(r) = w.ring_start else { continue };
            let bar: Vec<_> = ring_notes(nb)
                .into_iter()
                .filter(|s| s.step >= r && s.step < r + 16)
                .collect();
            assert_eq!(
                bar.iter().map(|s| s.step - r).collect::<Vec<_>>(),
                rhythm,
                "{genre} {id} in {}",
                w.section_name
            );
            let chord = chords
                .get(&((w.start / 16) % pad_bars))
                .cloned()
                .unwrap_or_default();
            assert!(
                chord.is_empty() || bar.iter().all(|s| chord.contains(&pc(s.note))),
                "{genre} {id}: {:?} on {chord:?}",
                bar.iter().map(|s| s.note).collect::<Vec<_>>()
            );
            let turned = hook_mode(&w.section_name) == motif_mode::INVERTED;
            let want: Vec<i64> = if turned {
                shape.iter().map(|x| -x).collect()
            } else {
                shape.clone()
            };
            assert_eq!(
                moves(&bar.iter().map(|s| s.note).collect::<Vec<_>>()),
                want,
                "{genre} {id}"
            );
            checked += 1;
        }
        assert!(checked > 0, "{genre}: no melody window");
    }
}

/// The hook recurs in every later melodic section and bar 1 of each phrase is on the chord,
/// while bar 2 still restates bar 1 (`.motif`).
#[test]
fn hook_recurs_across_sections() {
    for genre in SONG_GENRES {
        let s = spec(genre, 42.0);
        let report_of = |p: &Project| {
            let (notes, controls) = capture_all(p, None);
            MusicalityReport::new(p, &notes, &controls, None)
        };
        let on = report_of(&played(&s, Fixes::ALL));
        if let Some(h) = on.hook_recurrence {
            assert!(h >= 0.9, "{genre}: hookRecurrence {h}");
        }
        if let Some(b) = on.bar_one_chord_tones {
            assert!(b >= 0.95, "{genre}: barOneChordTones {b}");
        }
        if let Some(i) = on.melody_idea_repeats {
            assert!(i == 1.0, "{genre}: melodyIdeaRepeats {i}");
        }
    }
}

/// With compose's own `hook` technique the slots already are the hook: the stage leaves them
/// alone.
#[test]
fn compose_hook_turns_the_stage_off() {
    let mut s = spec("edm", 42.0);
    s.extra.set("hook", JsonValue::Bool(true));
    let melody = |p: &Project| -> Vec<(i64, i64)> {
        djrust::musicality::note_log::capture(p, None)
            .into_iter()
            .filter(|e| e.group == "melody")
            .map(|e| (e.tick, e.note))
            .collect()
    };
    let with = melody(&played(&s, Fixes::ALL));
    assert!(!with.is_empty());
    assert_eq!(with, melody(&played(&s, Fixes::ALL & !Fixes::HOOK)));
}

/// Seeds keep their own melodies: two seeds' songs share few notes.
#[test]
fn seeds_keep_their_own_melodies() {
    for genre in ["techno", "trance", "lofi"] {
        let seeds = [7.0, 42.0, 1234.0, 98_765.0, 31_337.0, 500_001.0];
        let tunes: Vec<std::collections::BTreeSet<(i64, i64)>> = seeds
            .iter()
            .map(|&seed| {
                djrust::musicality::note_log::capture(
                    &played(&spec(genre, seed), Fixes::ALL),
                    None,
                )
                .into_iter()
                .filter(|e| e.group == "melody")
                .map(|e| (e.tick, e.note))
                .collect()
            })
            .collect();
        let (mut sum, mut pairs) = (0.0, 0);
        for i in 0..tunes.len() {
            for j in i + 1..tunes.len() {
                let inter = tunes[i].intersection(&tunes[j]).count() as f64;
                let uni = tunes[i].union(&tunes[j]).count().max(1) as f64;
                sum += inter / uni;
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
