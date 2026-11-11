//! The twins folded on 2026-10-29 (PORTING.md section I): one definition each, held here to
//! what the copies they replaced said (`Deck.roleOrder`, `isSynthDrumKit`, `isDrumChannel`), so a
//! second copy cannot drift back unseen. Each pre-fold equality was run against the old bodies
//! before they were deleted.

use djrust::cid::{hex, sha256};
use djrust::deck::perform::ROLE_ORDER;
use djrust::fx::master_macro::MasterMacro;
use djrust::json::JsonValue;
use djrust::musicality::{dj, energy_curve, space, synth_voice};
use djrust::sound;
use djrust::voices::VoiceCatalog;
use djrust::wave::instruments::{InstrumentTable, INSTRUMENTS_JSON};
use djrust::wave::macros::NetMacro;
use djrust::wave::synth;
use djrust::TrackSpec;

#[test]
fn the_role_order_is_the_mixers() {
    // `Deck.roleOrder`: the roles in mixer order, the one the sound panel lists them in too
    assert_eq!(
        ROLE_ORDER,
        ["kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp"]
    );
}

#[test]
fn the_drum_helpers_are_the_wave_engines() {
    // `isSynthDrumKit`: beats' six synth kits, nothing else
    let kits = synth::synth_drum_kits();
    assert_eq!(
        kits,
        ["drums", "drums-808", "drums-breakbeat", "drums-cr78", "drums-lofi", "drums-v8"]
    );
    for k in &kits {
        assert!(sound::is_synth_drum_kit(k) && synth::is_synth_drum_kit(k), "{k}");
    }
    for n in ["", "drum", "kit", "drums-909", "Drums", "gm:1", "drums-v8 "] {
        assert!(!sound::is_synth_drum_kit(n) && !synth::is_synth_drum_kit(n), "{n:?}");
    }
    // `isDrumChannel`: MIDI channels 10...15 (0-based), as the generator writes them
    for c in -3..24 {
        let drum = (10..=15).contains(&c);
        assert_eq!(sound::is_drum_channel(c), drum, "{c}");
        assert_eq!(synth::is_drum_channel(c), drum, "{c}");
    }
}

// MARK: the duplicates the 2026-10-30 audit found (PORTING.md section I): equal before the fold

#[test]
fn the_sound_fields_are_one_list() {
    // `TrackSpec.soundFields` (PaletteSound.swift:132)
    assert_eq!(TrackSpec::SOUND_FIELDS, ["palette", "kit", "voice"]);
    assert_eq!(sound::SOUND_FIELDS, TrackSpec::SOUND_FIELDS);
}

#[test]
fn the_default_loop_quantum_is_one_number() {
    // `DJEngine.defaultLoopQuantum` (LoopLaunch.swift:37)
    assert_eq!(djrust::deck::loops::DEFAULT_LOOP_QUANTUM, 4);
    assert_eq!(djrust::session::action::DEFAULT_LOOP_QUANTUM, 4);
}

#[test]
fn the_instrument_table_is_parsed_from_one_file() {
    // `SoundPalette.knownInstrument` reads `InstrumentTable.shared.entries` (SoundPalette.swift:287):
    // `sound::known_instrument` asks the wave engine's table, which is parsed once from the one
    // `data/instruments.json` (the second include_str! and parse in `sound` are gone)
    let table = InstrumentTable::shared();
    let names = table.names();
    assert!(names.len() > 20, "{names:?}");
    assert_eq!(table.entries.len(), names.len());
    let catalog = VoiceCatalog::bundled();
    for n in &names {
        assert!(sound::known_instrument(n, &catalog), "{n}");
    }
    // a name that is in no table, catalog, genre set or stinger set
    assert!(!sound::known_instrument("no-such-instrument", &catalog));
    // the shared table is the same text the module bundles
    let again = InstrumentTable::from_json(&JsonValue::parse(INSTRUMENTS_JSON).unwrap()).unwrap();
    assert_eq!(again.names(), names);
}

#[test]
fn the_macro_ids_are_the_enums() {
    // `Deck.isMacro(id:)`: `NetMacro(id:) != nil || MasterMacro(id:) != nil` (DeckPerform.swift:435).
    // The 9 net and 18 master ids the stinger table used to repeat as strings, in their order.
    let net: Vec<&str> = NetMacro::ALL.iter().map(|m| m.id()).collect();
    let master: Vec<&str> = MasterMacro::ALL.iter().map(|m| m.id()).collect();
    assert_eq!(
        net,
        [
            "drop", "breakdown", "solo-drums", "cut", "double-drop", "beat-repeat", "half-time",
            "tape-stop", "tempo-anchor"
        ]
    );
    assert_eq!(
        master,
        [
            "sweep-lp", "sweep-hp", "reverb-wash", "riser", "build-crush", "phaser-drone",
            "filter-res", "octave-up", "octave-down", "pitch-bend", "vinyl-brake", "feel-chill",
            "feel-drive", "feel-ambient", "feel-euphoric", "feel-build", "feel-wind-down",
            "feel-reset"
        ]
    );
    for id in net.iter().chain(&master) {
        assert!(sound::stinger::is_macro(id), "{id}");
    }
    for id in ["", "Drop", "drop ", "feel", "sweep", "stinger", "net", "nothing"] {
        assert!(!sound::stinger::is_macro(id), "{id:?}");
    }
}

/// A deterministic stereo signal with a DC part, a side part and noise. `libm`, not `f32::sin`:
/// that one is the platform's, and a last-bit difference moves the hash below.
fn side_input(n: usize) -> Vec<(f32, f32)> {
    let mut x = 0x2545_f491_u32;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    (0..n)
        .map(|i| {
            let tone = libm::sinf(i as f32 * 0.037);
            (0.3 + 0.5 * tone + 0.2 * next(), 0.3 - 0.4 * tone + 0.2 * next())
        })
        .collect()
}

/// sha256 over every output sample's bits, for several corners and rates (the last corner is
/// above 0.45 x the rate, which `new` clamps).
macro_rules! side_hash {
    ($ty:ty) => {{
        let input = side_input(4096);
        let mut bytes = Vec::new();
        for (hz, sr) in [(150.0, 48_000.0), (300.0, 22_050.0), (300.0, 44_100.0), (600.0, 11_025.0), (20_000.0, 22_050.0)] {
            let mut f = <$ty>::new(hz, sr);
            for &(l0, r0) in &input {
                let (mut l, mut r) = (l0, r0);
                f.process(&mut l, &mut r);
                bytes.extend_from_slice(&l.to_bits().to_le_bytes());
                bytes.extend_from_slice(&r.to_bits().to_le_bytes());
            }
            f.reset();
            let (mut l, mut r) = (0.5f32, -0.25f32);
            f.process(&mut l, &mut r);
            bytes.extend_from_slice(&l.to_bits().to_le_bytes());
            bytes.extend_from_slice(&r.to_bits().to_le_bytes());
        }
        hex(&sha256(&bytes))
    }};
}

#[test]
fn the_side_highpass_is_one_filter() {
    // `SideHighpass` (EnergyCurve.swift:156): Space.swift's reverb and WaveRunner's sampled slots
    // both use that one struct. The hash is what the two copies this replaced both produced
    // (checked equal, bit for bit, before the second was deleted).
    let want = "5640be124d9058525d7da606aea519a51a9c6a23e0dbb00ec64a268d29e9ee49";
    assert_eq!(side_hash!(energy_curve::SideHighpass), want);
    // and the reverb's name for it is that type
    assert_eq!(side_hash!(space::SideHighpass), want);
}

#[test]
fn the_side_corner_is_300_hz_in_each_place_swift_names_it() {
    // Swift names three constants that are all 300: `Unison.sideHz` (SynthVoice.swift:74),
    // `FdnReverb.sideHz` (Space.swift:158) and `DJMusicality.sampledSideHz`
    // (DJMusicality.swift:1030). They are separate tunables there, so they stay separate here.
    assert_eq!(synth_voice::Unison::SIDE_HZ, 300.0);
    assert_eq!(space::FdnReverb::SIDE_HZ, 300.0);
    assert_eq!(dj::SAMPLED_SIDE_HZ, 300.0);
}
