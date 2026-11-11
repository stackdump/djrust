//! `GuitarVoicingTests`, the part that needs no sampler: the generator writes tracks whose
//! guitars play sampled guitars. beats' composer picks `acoustic-guitar` / `electric-guitar` /
//! `distorted-guitar` for a role in some genres (`instrumentSets`); `VoiceCatalog` turns that
//! into a seeded pick from the category's installed sounds, then its SoundFont programs.
//!
//! The Swift tests install nothing themselves: they need Logic / GarageBand EXS guitars (or a
//! SoundFont for the distorted one) on the Mac and skip otherwise. Here the installed content
//! is a SoundFont bank (no EXS exists off the Mac), named on the catalog the way
//! `sound_fonts_in` lists a folder, so every test runs. Each test finds, as Swift's `find`
//! does, the first seed (0, 1, ...; genres in order within a seed) whose track gives the
//! instrument to a net whose voicing pick is a sample, and checks (2) of Swift's `check`: that
//! it is a real sample of the category (a SoundFont program of `gm[category]`), not Apple's
//! General MIDI. The audition and the rendered stem (3) need a deck core with a loaded bank and a
//! per-net mute: they are `djhost/tests/guitar_voicing_stem.rs`.
//!
//! The "gap" the Swift header records (by design of beats' tables) is asserted: only `melody`
//! and `bass` ever get a guitar.

use std::collections::BTreeSet;

use djrust::compose;
use djrust::json::{JsonObject, JsonValue};
use djrust::voices::{SamplerSource, VoiceCatalog};

/// Genres whose `instrumentSets` hold each guitar, in the order they are swept.
const GENRES: [(&str, &[&str]); 3] = [
    (
        "acoustic-guitar",
        &["country", "blues", "reggae", "lofi", "bossa"],
    ),
    ("electric-guitar", &["country", "blues", "funk"]),
    ("distorted-guitar", &["metal", "speedcore"]),
];

const BANK: &str = "/sf/MuseScore_General.sf2";

fn catalog() -> VoiceCatalog {
    let mut c = VoiceCatalog::bundled();
    c.sound_fonts = vec![BANK.to_string()];
    c
}

/// A sample, not a synth voice or Apple's GM: an EXS or a SoundFont program.
fn sampled(s: Option<&SamplerSource>) -> bool {
    matches!(s, Some(SamplerSource::Exs(_) | SamplerSource::Sf2(..)))
}

struct Found {
    genre: &'static str,
    seed: i64,
    role: String,
    nets: Vec<String>,
    source: SamplerSource,
}

/// Swift's `find`, on the catalog alone.
fn find(c: &VoiceCatalog, instrument: &str) -> Option<Found> {
    let genres = GENRES.iter().find(|(i, _)| *i == instrument).unwrap().1;
    for seed in 0..200 {
        for genre in genres {
            let ov = JsonValue::Object(JsonObject::from_pairs([(
                "seed",
                JsonValue::Number(seed as f64),
            )]));
            let p = compose(genre, &ov);
            let nets: Vec<String> = p
                .nets
                .iter()
                .filter(|(_, n)| n.role != "control" && n.track.instrument == instrument)
                .map(|(id, _)| id.to_string())
                .collect();
            let Some(first) = nets.first() else { continue };
            let b = p.nets.get(first).unwrap();
            let role = b.track.group.clone().unwrap_or(b.riff_group.clone());
            let sources = c.sources(instrument, &role, seed as f64, false);
            // the pick is the synth voice (keepSynth) or nothing is installed: next seed
            let Some(top) = sources.as_ref().and_then(|s| s.first()) else {
                continue;
            };
            if !sampled(Some(top)) {
                continue;
            }
            let mut nets = nets;
            nets.sort();
            return Some(Found {
                genre,
                seed,
                role,
                nets,
                source: top.clone(),
            });
        }
    }
    None
}

fn check(instrument: &str, category: &str) {
    let c = catalog();
    let f = find(&c, instrument)
        .unwrap_or_else(|| panic!("no seed < 200 voices {instrument} with a sample"));
    eprintln!(
        "{category}: {} seed {} role {} nets {:?} -> {}",
        f.genre, f.seed, f.role, f.nets, f.source
    );
    // a real sample from the category - its pool or its SoundFont programs - not Apple's General
    // MIDI (and the lane is not the synth)
    match &f.source {
        SamplerSource::Exs(path) => assert!(
            c.pool(category)
                .iter()
                .any(|s| s.sources.contains(&SamplerSource::Exs(path.clone()))),
            "{path} is not in {category}"
        ),
        SamplerSource::Sf2(bank, program) => {
            assert_eq!(bank, BANK);
            assert!(
                c.gm[category].contains(program),
                "{} is not in {category}",
                f.source
            );
        }
        other => panic!("{category} fell back to {other}"),
    }
    // the guitar is a lead or a bass
    assert!(["melody", "bass"].contains(&f.role.as_str()), "{}", f.role);
    // every net that plays the guitar gets the same role's pick (the seed and the role make it)
    assert!(!f.nets.is_empty());
}

/// (a) GuitarVoicingTests.acousticGuitarTracksPlayASampledGuitar
#[test]
fn acoustic_guitar_tracks_play_a_sampled_guitar() {
    check("acoustic-guitar", "guitar.acoustic");
}

/// (a) GuitarVoicingTests.electricGuitarTracksPlayASampledGuitar
#[test]
fn electric_guitar_tracks_play_a_sampled_guitar() {
    check("electric-guitar", "guitar.electric");
}

/// (a) GuitarVoicingTests.distortedGuitarTracksPlayASampledGuitar: `guitar.distorted` is
/// `keepSynth` and no EXS ships for it, so with a SoundFont an empty pool offers no synth pick
/// and the bank's Distortion Guitar (GM 30, 0-based) comes first, else its Overdriven Guitar
/// (29); without one, metal and speedcore guitars play the synth voice.
#[test]
fn distorted_guitar_tracks_play_a_sampled_guitar() {
    check("distorted-guitar", "guitar.distorted");
    let c = catalog();
    let f = find(&c, "distorted-guitar").unwrap();
    assert_eq!(f.source, SamplerSource::Sf2(BANK.into(), 30));
    // no SoundFont: the synth voice, nothing to find
    assert!(find(&VoiceCatalog::bundled(), "distorted-guitar").is_none());
}

/// (a) the header of GuitarVoicingTests: "only `melody` and `bass` ever get a guitar".
/// `harmony` is always `warm-pad` / `dark-pad`, and `arp` exists only in edm / synthwave /
/// trance with pluck pools.
#[test]
fn only_melody_and_bass_ever_get_a_guitar() {
    let mut roles = BTreeSet::new();
    let mut hits = 0;
    for (instrument, genres) in GENRES {
        for genre in genres {
            for seed in 0..40 {
                let ov = JsonValue::Object(JsonObject::from_pairs([(
                    "seed",
                    JsonValue::Number(seed as f64),
                )]));
                let p = compose(genre, &ov);
                for (_, n) in p.nets.iter() {
                    if n.role != "control" && n.track.instrument == instrument {
                        hits += 1;
                        roles.insert(
                            n.track
                                .group
                                .clone()
                                .unwrap_or_else(|| n.riff_group.clone()),
                        );
                    }
                }
            }
        }
    }
    assert!(hits > 20, "{hits} guitar nets");
    assert!(
        roles.iter().all(|r| r == "melody" || r == "bass"),
        "{roles:?}"
    );
}
