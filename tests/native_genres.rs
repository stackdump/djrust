//! djrust's native genres (`data/genres-native.json`: electro, deephouse, italo, acid). They
//! have no Swift/JS counterpart, so their golden is pinned here: `tests/fixtures/native/
//! genres.json` holds each genre × seed's composed project digest. Regenerate (after an
//! intended change) with `NATIVE_GOLDEN_REGEN=1 cargo test -p djengine --test native_genres`.

use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::wander::Wander;
use djrust::{compose, JsonObject, Project};

const NATIVE: [&str; 4] = ["electro", "deephouse", "italo", "acid"];
const SEEDS: [f64; 3] = [1.0, 42.0, 1984.0];

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        ov.set("structure", s.into());
    }
    compose(genre, &ov.into())
}

/// The nets playing a role (`id` or `riffGroup`), arranged or not.
fn role<'a>(p: &'a Project, role: &str) -> Vec<&'a djrust::pflow::NetBundle> {
    p.nets
        .iter()
        .filter(|(id, n)| *id == role || n.riff_group == role)
        .map(|(_, n)| n)
        .collect()
}

fn fnv64(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[test]
fn native_genres_are_in_the_tables_after_beats() {
    let t = BeatsTables::shared();
    let n = t.genre_ids.len();
    assert_eq!(&t.genre_ids[n - 4..], &NATIVE.map(String::from));
    assert_eq!(t.beats_genre_ids().len(), n - 4);
    for g in NATIVE {
        assert!(t.is_native(g));
        assert!(t.genres.contains_key(g), "{g} in genres (pickers list these)");
        assert!(t.instrument_set(g).is_some() && !t.instrument_sets.contains_key(g));
        assert!(t.theme_table(g).is_some_and(|th| th.supported));
        assert!(!t.is_native(t.kin(g)), "{g} follows a beats genre");
    }
    assert_eq!(t.kin("house"), "house");
    assert_eq!(t.kin("deephouse"), "house");
}

#[test]
fn tempos_scales_and_families() {
    let t = BeatsTables::shared();
    let want = [
        ("electro", 110.0, 125.0, 0),
        ("deephouse", 120.0, 124.0, 0),
        ("italo", 115.0, 125.0, 1),
        ("acid", 120.0, 128.0, 0),
    ];
    for (g, lo, hi, fam) in want {
        let d = &t.genres[g];
        assert!(d.bpm >= lo && d.bpm <= hi, "{g} bpm {}", d.bpm);
        assert_eq!(t.family_of(g), fam, "{g} family");
        let p = project(g, 7.0, None);
        assert_eq!(p.tempo, d.bpm);
    }
    assert_eq!(t.genres["deephouse"].scale_name, "DorianScale");
}

/// The vocoder's carrier: every native genre has a sustained harmony pad, active in every
/// section of its energy table, at least three tones a chord, each held most of a bar.
#[test]
fn every_native_genre_has_a_sustained_harmony_role() {
    let t = BeatsTables::shared();
    for g in NATIVE {
        let th = t.theme_table(g).unwrap();
        for (sec, prof) in th.energy.iter() {
            assert!(
                prof.roles.get("harmony").is_some_and(|r| r.active),
                "{g} {sec}: harmony active"
            );
        }
        for seed in SEEDS {
            let p = project(g, seed, None);
            let pads = role(&p, "harmony");
            assert_eq!(pads.len(), 1, "{g}/{seed}: one harmony net");
            let pad = pads[0];
            let bar_ms = 4.0 * 60000.0 / p.tempo;
            assert!(pad.bindings.len() >= 3 * 2, "{g}/{seed}: chords");
            for (_, b) in pad.bindings.iter() {
                assert!(b.duration as f64 >= 0.9 * bar_ms, "{g}/{seed}: pad held {}", b.duration);
            }
            let set = t.instrument_set(g).unwrap();
            let harmony = set.get("harmony").unwrap();
            assert!(
                harmony.contains(&pad.track.instrument),
                "{g}/{seed}: pad plays {}",
                pad.track.instrument
            );
            // arranged, the pad is still there
            let arranged = project(g, seed, Some("standard"));
            assert!(!role(&arranged, "harmony").is_empty(), "{g}/{seed} arranged");
        }
    }
}

#[test]
fn deephouse_pads_are_seventh_chords() {
    // rootless voicings over the bass root: the pad's lowest tone is never the bass's root
    // pitch class in the tonic bar.
    for seed in SEEDS {
        let p = project("deephouse", seed, None);
        let pad = role(&p, "harmony")[0];
        let root_pc = p.root_note.unwrap() % 12;
        let first_bar: Vec<i64> = pad
            .bindings
            .iter()
            .map(|(_, b)| b.note)
            .take(3)
            .collect();
        assert!(!first_bar.iter().any(|n| n % 12 == root_pc), "{seed}: {first_bar:?}");
        assert_eq!(first_bar[0] % 12, (root_pc + 3) % 12, "minor third on top of the root");
    }
}

#[test]
fn italo_bass_bounces_octaves_and_acid_bass_is_a_sixteenth_line() {
    for seed in SEEDS {
        let p = project("italo", seed, None);
        let bass = role(&p, "bass")[0];
        let notes: Vec<i64> = bass.bindings.iter().map(|(_, b)| b.note).collect();
        assert_eq!(notes.len(), 32, "eight eighths a bar, four bars");
        for pair in notes.chunks(2) {
            assert_eq!(pair[1] - pair[0], 12, "{seed}: root then octave");
        }
        assert_eq!(bass.track.generator.as_deref(), Some("octave-bass"));

        let p = project("acid", seed, None);
        let bass = role(&p, "bass")[0];
        assert_eq!(bass.track.instrument, "acid", "303 voice");
        assert_eq!(bass.track.generator.as_deref(), Some("acid-bass"));
        let n = bass.bindings.len();
        assert!(n >= 16, "{seed}: busy line, {n} notes");
        let vels: std::collections::BTreeSet<i64> =
            bass.bindings.iter().map(|(_, b)| b.velocity).collect();
        assert!(vels.len() >= 2, "{seed}: accents");
    }
}

#[test]
fn electro_and_italo_have_arps_deephouse_and_acid_do_not() {
    for seed in SEEDS {
        assert!(!role(&project("electro", seed, None), "arp").is_empty());
        assert!(!role(&project("italo", seed, None), "arp").is_empty());
        assert!(role(&project("deephouse", seed, None), "arp").is_empty());
        assert!(role(&project("acid", seed, None), "arp").is_empty());
    }
}

#[test]
fn composing_is_deterministic() {
    for g in NATIVE {
        let a = project(g, 99.0, Some("standard")).to_json().canonical_js();
        let b = project(g, 99.0, Some("standard")).to_json().canonical_js();
        assert_eq!(a, b, "{g}");
    }
}

/// Native genres wander: they reach genres in their family and tempo; beats genres never
/// reach them (beats genres' walks stay Swift's).
#[test]
fn wander_neighbours() {
    let t = BeatsTables::shared();
    for g in NATIVE {
        let n = Wander::neighbours(g);
        assert!(!n.is_empty(), "{g} has neighbours");
        assert!(n.iter().all(|x| t.genres.contains_key(x)));
    }
    assert!(Wander::neighbours("deephouse").contains(&"house".to_string()));
    for g in t.beats_genre_ids() {
        assert!(
            Wander::neighbours(&g).iter().all(|x| !t.is_native(x)),
            "{g} stays in beats' catalogue"
        );
    }
}

#[test]
fn golden() {
    let path = format!(
        "{}/tests/fixtures/native/genres.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut rows = Vec::new();
    for g in NATIVE {
        for seed in SEEDS {
            for structure in [None, Some("standard")] {
                let p = project(g, seed, structure);
                let mut o = JsonObject::new();
                o.set("genre", g.into());
                o.set("seed", seed.into());
                o.set("structure", structure.unwrap_or("loop").into());
                o.set("tempo", p.tempo.into());
                o.set("nets", (p.nets.len() as f64).into());
                let mut inst = JsonObject::new();
                for (id, n) in p.nets.iter() {
                    if structure.is_none() {
                        inst.set(id, n.track.instrument.as_str().into());
                    }
                }
                o.set("instruments", inst.into());
                o.set("digest", fnv64(&p.to_json().canonical_js()).into());
                rows.push(JsonValue::from(o));
            }
        }
    }
    let got = JsonValue::Array(rows);
    if std::env::var("NATIVE_GOLDEN_REGEN").is_ok() {
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(&path, got.canonical_js()).unwrap();
        return;
    }
    let want = JsonValue::parse(&std::fs::read_to_string(&path).expect("golden; regen it"))
        .unwrap();
    let (got, want) = (got.as_array().unwrap(), want.as_array().unwrap());
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g.canonical_js(), w.canonical_js());
    }
}
