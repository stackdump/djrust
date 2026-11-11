//! `GuitarVoicingTests`, the part `djengine/tests/guitar_voicing.rs` leaves: the loaded sound
//! itself in tune through the deck's own slot (the audition) and the role's stem from the
//! generated track with everything else muted, loud enough and in tune (steps (2)'s lane check
//! and (3) of Swift's `check`). The generator writes tracks whose guitars play sampled guitars:
//! beats' composer picks `acoustic-guitar` / `electric-guitar` / `distorted-guitar` for a role
//! in some genres, `VoiceCatalog` turns that into a seeded pick from the category's SoundFont
//! programs, the deck's bank loads it.
//!
//! Swift needs Logic / GarageBand EXS guitars on the Mac (or a SoundFont for the distorted one)
//! and skips otherwise; here the bank is a SoundFont, in turn a synthetic one (a looped sine
//! under 128 programs: always) and the real MuseScore_General.sf2 (when it is there). EXS is
//! n/a off the Mac (PORTING.md section H).
#![cfg(feature = "sf2")]

use std::sync::Arc;

use djrust::json::{JsonObject, JsonValue};
use djrust::voices::pitch::pitch_class_error;
use djrust::voices::sampler as rules;
use djrust::voices::SamplerSource;
use djrust::wave::net::str_hash;
use djrust::compose;
use djrust::host::deck::{engine, Command, DeckCore, DeckMirror, EngineConfig};
use djrust::host::sampler::{
    sound_font_dir, synthetic_sf2, Library, NoteEvent, Sf2Sampler,
};

const SR: f64 = 48_000.0;

/// Genres whose `instrumentSets` hold each guitar, in the order they are swept.
const GENRES: [(&str, &[&str]); 3] = [
    (
        "acoustic-guitar",
        &["country", "blues", "reggae", "lofi", "bossa"],
    ),
    ("electric-guitar", &["country", "blues", "funk"]),
    ("distorted-guitar", &["metal", "speedcore"]),
];

fn category(instrument: &str) -> &'static str {
    match instrument {
        "acoustic-guitar" => "guitar.acoustic",
        "electric-guitar" => "guitar.electric",
        _ => "guitar.distorted",
    }
}

fn banks(tag: &str) -> Vec<(&'static str, std::path::PathBuf)> {
    let d = std::env::temp_dir().join(format!("djhost-guitar-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    std::fs::write(d.join("MuseScore_General.sf2"), synthetic_sf2(&presets)).unwrap();
    let mut v = vec![("synthetic", d)];
    let real = sound_font_dir();
    if real.join("MuseScore_General.sf2").exists() {
        v.push(("MuseScore", real));
    } else {
        eprintln!(
            "SKIPPED the real bank: no MuseScore_General.sf2 in {} (make soundfonts)",
            real.display()
        );
    }
    v
}

/// 10 log10 of the mean square (`rmsDB`).
fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let s: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * (s / x.len() as f64).max(1e-20).log10()
}

fn midi_hz(n: i64) -> f64 {
    440.0 * 2f64.powf((n - 69) as f64 / 12.0)
}

/// `gridDeviations`: over up to `windows` loud stretches of `size` samples, the strongest semitone
/// partial in `notes` (Hann-windowed DFT), refined to its peak within +-60 cents. A bass's own
/// fundamentals smear at this window length, so `notes` start above them.
fn grid_deviations(x: &[f32], sr: f64) -> Vec<f64> {
    let (size, windows) = (16_384usize, 12usize);
    if x.len() < size {
        return Vec::new();
    }
    let hann: Vec<f64> = (0..size)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (size - 1) as f64).cos())
        .collect();
    let starts: Vec<usize> = (0..=x.len() - size)
        .step_by(size / 2)
        .filter(|s| rms_db(&x[*s..*s + size]) > -50.0)
        .collect();
    let picked: Vec<usize> = (0..starts.len())
        .step_by((starts.len() / windows).max(1))
        .map(|i| starts[i])
        .collect();
    let mag = |s: usize, f: f64| -> f64 {
        let k = 2.0 * std::f64::consts::PI * f / sr;
        let (cr, ci) = (k.cos(), -k.sin());
        let (mut pr, mut pi) = (1.0f64, 0.0f64);
        let (mut re, mut im) = (0.0, 0.0);
        for i in 0..size {
            let v = x[s + i] as f64 * hann[i];
            re += v * pr;
            im += v * pi;
            let t = pr * cr - pi * ci;
            pi = pr * ci + pi * cr;
            pr = t;
        }
        re * re + im * im
    };
    picked
        .iter()
        .map(|&s| {
            let n = (52..=96)
                .map(|n| (n, mag(s, midi_hz(n))))
                .fold((52, -1.0f64), |b, c| if c.1 > b.1 { c } else { b })
                .0;
            let mut best = (0.0f64, -1.0f64);
            let mut c = -60.0;
            while c <= 60.0 {
                let m = mag(s, midi_hz(n) * 2f64.powf(c / 1200.0));
                if m > best.1 {
                    best = (c, m);
                }
                c += 1.0;
            }
            best.0
        })
        .collect()
}

fn check(instrument: &str) {
    for (name, dir) in banks(instrument) {
        let library = Arc::new(Library::scan(&dir));
        let mut c = EngineConfig::new(SR);
        c.library = Some(library.clone());
        let (e, _r) = engine(c);
        let preparer = e.preparer("A").unwrap();
        let bank = preparer.sampler_bank().unwrap();
        let cat = category(instrument);

        // `find`: the first seed (0, 1, ...; genres in order within a seed) whose track gives
        // `instrument` to a role whose voicing pick is a sampled sound, prepared on the bank
        let mut found = None;
        'seeds: for seed in 0..200 {
            for genre in GENRES.iter().find(|g| g.0 == instrument).unwrap().1 {
                let mut ov = JsonObject::new();
                ov.set("seed", JsonValue::Number(seed as f64));
                let p = compose(genre, &JsonValue::Object(ov));
                let nets: Vec<String> = p
                    .nets
                    .keys()
                    .iter()
                    .filter(|k| {
                        let nb = p.nets.get(k).unwrap();
                        nb.role != "control" && nb.track.instrument == instrument
                    })
                    .cloned()
                    .collect();
                let Some(first) = nets.first() else { continue };
                let b = p.nets.get(first).unwrap();
                let role = b.track.group.clone().unwrap_or_else(|| b.riff_group.clone());
                // the pick is the synth voice (keepSynth) or nothing is installed: next seed
                let pick = library.catalog.sources(instrument, &role, seed as f64, false);
                if !matches!(
                    pick.as_ref().and_then(|v| v.first()),
                    Some(SamplerSource::Exs(_) | SamplerSource::Sf2(..))
                ) {
                    continue;
                }
                let opts = preparer.options(seed as f64);
                let prepared = preparer.prepare_opts(&p, 0, opts, None);
                let lane = prepared
                    .wave()
                    .lanes()
                    .iter()
                    .find(|l| &l.net_id == first && l.is_tonal() && l.sample_slot().is_some())
                    .map(|l| l.sample_slot().unwrap());
                let Some(slot) = lane else {
                    // not on a sampler after all: next seed (what it bound waits in the bank)
                    continue;
                };
                found = Some((genre.to_string(), seed, role, nets, p, prepared, slot));
                break 'seeds;
            }
        }
        let (genre, seed, role, mut nets, project, prepared, slot) =
            found.unwrap_or_else(|| panic!("{name}: no seed < 200 voices {instrument} with a sample"));
        nets.sort();
        let source = bank.lock().unwrap().source_of(slot).unwrap().to_string();
        println!("{name}: {cat}: {genre} seed {seed} role {role} nets {nets:?} -> {source}");

        // (2) a real sample from the category (its SoundFont programs), not Apple's General
        // MIDI (and the lane is not the synth)
        let program: i64 = source
            .rsplit('#')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{name}: {source} is not a SoundFont program"));
        assert!(source.starts_with("sf2:"), "{name}: {cat} fell back to {source}");
        assert!(
            library
                .catalog
                .gm
                .get(cat)
                .is_some_and(|ps| ps.contains(&program)),
            "{name}: {source} is not in {cat}"
        );
        for n in &nets {
            for l in prepared.wave().lanes().iter().filter(|l| &l.net_id == n && l.is_tonal()) {
                assert_eq!(l.sample_slot(), Some(slot), "{name}: {n} plays the role's pick");
            }
        }
        // the loaded sound itself, through the deck's own slot: in tune, as the bank promised
        let note: u8 = if role == "bass" { 45 } else { 57 };
        let mut samplers = preparer.take_samplers();
        let (_, sampler): &mut (usize, Sf2Sampler) = samplers
            .iter_mut()
            .find(|(i, _)| *i == slot)
            .unwrap_or_else(|| panic!("{name}: slot {slot} was bound by this prepare"));
        let total = (0.5 * SR) as usize;
        let (l, r) = sampler.render_notes(
            &[NoteEvent {
                at: 0,
                frames: SR as usize,
                key: note,
                velocity: 100,
            }],
            total,
        );
        sampler.silence();
        let mid: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
        let (cents, _, _) = pitch_class_error(&mid[4800..], note as i64, SR);
        assert!(
            cents.abs() <= rules::MAX_DETUNE_CENTS + 2.0,
            "{name}: audition {cents} cents"
        );

        // (3) the role's stem from the generated track: everything else muted
        let mut o = preparer.options(seed as f64);
        o.master = djrust::wave::Master::Linear;
        let gain = prepared.wave().gain;
        let mut core = DeckCore::new("stem", SR, o, false, Arc::new(DeckMirror::new()));
        for (slot, s) in samplers {
            core.apply(Command::Sampler(slot, s));
        }
        core.apply(Command::Load(prepared.project));
        for id in project.nets.keys() {
            if !nets.contains(id) && project.nets.get(id).unwrap().role != "control" {
                core.apply(Command::Mute {
                    hash: str_hash(id),
                    muted: true,
                });
            }
        }
        core.apply(Command::Play);
        let frames = (8.0 * SR) as usize;
        let mut stem = vec![0f32; frames];
        let (mut bl, mut br) = (vec![0f32; 512], vec![0f32; 512]);
        let (mut off, mut at) = (0usize, 0i64);
        while off < frames {
            let n = 512.min(frames - off);
            core.render(&mut bl[..n], &mut br[..n], at);
            for i in 0..n {
                stem[off + i] = 0.5 * (bl[i] + br[i]);
            }
            off += n;
            at += n as i64;
        }
        // as loud as the sound itself plays: the track's loudness gain (`Loudness`) taken out
        let level = rms_db(&stem) - 20.0 * gain.log10();
        let mut dev: Vec<f64> = grid_deviations(&stem, SR).iter().map(|d| d.abs()).collect();
        dev.sort_by(f64::total_cmp);
        println!("  stem {level:.1} dB, cents off the grid per window {dev:?}");
        assert!(level > -45.0, "{name}: the {role} stem is silent ({level} dB)");
        assert!(dev.len() >= 4, "{name}: too few loud windows to judge pitch");
        let median = dev[dev.len() / 2];
        assert!(
            median <= rules::MAX_DETUNE_CENTS,
            "{name}: the {role} stem is {median} cents off"
        );
    }
}

// GuitarVoicingTests.acousticGuitarTracksPlayASampledGuitar
#[test]
fn acoustic_guitar_tracks_play_a_sampled_guitar() {
    check("acoustic-guitar");
}

// GuitarVoicingTests.electricGuitarTracksPlayASampledGuitar
#[test]
fn electric_guitar_tracks_play_a_sampled_guitar() {
    check("electric-guitar");
}

// GuitarVoicingTests.distortedGuitarTracksPlayASampledGuitar
#[test]
fn distorted_guitar_tracks_play_a_sampled_guitar() {
    check("distorted-guitar");
}
