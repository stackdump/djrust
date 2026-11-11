//! The Swift suite for the Minecraft palette's bank (`Tests/DJCoreTests/MinecraftTests.swift`),
//! ported test for test and held to its literals (parity form (a) of the port rules), on the
//! synthetic assets tree instead of the game's: no Minecraft asset is used or needed.
//!
//! The tests that need the game's assets in Swift skip without them; here the tree
//! (`minecraft_support::tree_files`, the sounds and index texts the Swift dump built) stands in,
//! so they all run. The two that play the built bank through the sampler (`theBankPlaysInTune`,
//! `octaveZonesPlayInTuneAndLevel`, `steadyTonesHold`) are in `djhost/tests/minecraft_import.rs`
//! (they need rustysynth). `setPaletteRoundTrips`, `aPaletteIsATracksOverrideTheParityPathIgnores`
//! and `publishRefusesLocalOnlySounds` are held by the session and palette suites
//! (`tests/session*.rs`, `tests/sound.rs`).

mod common;
mod minecraft_support;

use std::collections::BTreeSet;

use common::*;
use djrust::json::JsonValue;
use djrust::sound::minecraft::assets::{
    is_forbidden, newest, AssetFs, MinecraftAssets, Unavailable,
};
use djrust::sound::minecraft::import::{build, octave_shift, octave_zones, Built, SAMPLE_RATE};
use djrust::sound::minecraft::{self as mc, Kind, Sidecar};
use djrust::sound::{Installed, PaletteSet};
use djrust::voices::bank::sf2_presets;
use djrust::voices::drumkits;
use djrust::voices::pitch::pitch_class_error;
use minecraft_support::*;

const ROOT: &str = "/mem/assets";

fn golden() -> JsonValue {
    fixture("minecraft")
}

fn tree() -> MemFs {
    MemFs::with(ROOT, &tree_files(&golden()))
}

fn import_at(index: &str) -> (MemFs, MinecraftAssets, Built) {
    let fs = tree();
    let assets = MinecraftAssets::locate(&fs, ROOT, Some(index)).unwrap();
    let built = {
        let mut decode = |p: &str| decode_mono_wav(&fs.read(p).ok_or("no such object")?);
        build(&assets, &fs, &mut decode).expect("the import builds")
    };
    (fs, assets, built)
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// MinecraftTests.theNewestIndexIsPicked
#[test]
fn the_newest_index_is_picked() {
    assert_eq!(newest(&strs(&["5", "27", "29", "30", "32"])), Some("32"));
    assert_eq!(newest(&strs(&["9", "10"])), Some("10"));
    assert_eq!(newest(&[]), None);
}

// MinecraftTests.theSoundtrackIsNeverRead
#[test]
fn the_soundtrack_is_never_read() {
    assert!(is_forbidden("music/game/calm1"));
    assert!(is_forbidden("records/cat"));
    assert!(!is_forbidden("note/harp2"));
    // no program, kit zone or palette sound reaches for them
    for p in mc::bank_programs() {
        assert!(!is_forbidden(&p.sound), "{}", p.name);
    }
    for z in mc::kit_zones() {
        assert!(!is_forbidden(z.sound));
    }
}

/// Even when the index lists a music path, `file(sound:)` refuses to open it (the synthetic
/// sounds.json maps the banjo event onto `music/game/calm1`).
#[test]
fn a_sound_event_that_names_the_soundtrack_is_skipped_not_read() {
    let (_, assets, built) = import_at("30");
    assert_eq!(
        assets.sounds("block.note_block.banjo"),
        strs(&["music/game/calm1"])
    );
    assert_eq!(
        built.report.skipped.iter().find(|s| s.starts_with("banjo")),
        Some(&"banjo: music/game/calm1 is in the index but not downloaded".to_string())
    );
    assert!(!built.sidecar.programs.contains(&"banjo".to_string()));
}

// MinecraftTests.missingAssetsMakeThePaletteUnavailable (the assets half; the palette half,
// `is_playable` / `unavailable_reason` / `sources` / `kit_for` without the bank, is in
// tests/sound.rs)
#[test]
fn missing_assets_make_the_palette_unavailable() {
    let none = MemFs::default();
    let err = MinecraftAssets::locate(&none, "/nonexistent/assets", None).unwrap_err();
    assert_eq!(err, Unavailable::NoAssets("/nonexistent/assets".into()));
    assert_eq!(
        err.to_string(),
        "no Minecraft assets at /nonexistent/assets (install the game with PrismLauncher, or set DJ_MINECRAFT_ASSETS)"
    );
    // the reason the palette shows: the assets', else "not imported yet"
    let mut inst = Installed::nothing("/empty");
    inst.minecraft_assets_missing =
        MinecraftAssets::unavailable_reason(&none, "/nonexistent/assets");
    assert_eq!(
        inst.minecraft_reason().as_deref(),
        Some(err.to_string().as_str())
    );
    inst.minecraft_assets_missing = None;
    assert_eq!(
        inst.minecraft_reason().as_deref(),
        Some("not imported yet: run `make minecraft` (djctl minecraft import)")
    );
    let palette_set = PaletteSet::shared();
    let p = palette_set.named(Some("minecraft")).unwrap();
    assert_eq!(p.label, "Minecraft");
}

// MinecraftTests.thePaletteNamesRealPrograms
#[test]
fn the_palette_names_real_programs() {
    let set = PaletteSet::shared();
    let p = set.named(Some("minecraft")).unwrap();
    assert_eq!(p.label, "Minecraft");
    assert!(p.is_local_only(set));
    for role in ["kit", "bass", "melody", "harmony", "arp", "stinger"] {
        let option = p
            .roles
            .iter()
            .find(|(r, _)| r == role)
            .and_then(|(_, o)| o.first())
            .unwrap_or_else(|| panic!("{role}"));
        let c = set
            .bank_category(option)
            .unwrap_or_else(|| panic!("{role}"));
        assert_eq!(c.bank, mc::FILE_NAME);
        assert_eq!(c.kit.is_some(), role == "kit", "{role}");
        for s in &c.sounds {
            assert!(mc::program_named(s).is_some(), "{role}: {s}");
        }
    }
    let programs: Vec<i64> = mc::bank_programs().iter().map(|p| p.program).collect();
    let unique: BTreeSet<i64> = programs.iter().copied().collect();
    assert_eq!(unique.len(), programs.len(), "program numbers are unique");
    assert!(!programs.contains(&mc::KIT_PROGRAM));
    assert_eq!(
        set.named(Some("minecraft@32")).map(|p| p.id.as_str()),
        Some("minecraft")
    );
    assert_eq!(mc::kit_id(), "minecraft-local.sf2#0:127");
}

// MinecraftTests.assetsResolveThroughSoundsJSON
#[test]
fn assets_resolve_through_sounds_json() {
    let fs = tree();
    let a = MinecraftAssets::locate(&fs, ROOT, None).unwrap();
    assert!(a.index_id.parse::<i64>().is_ok());
    assert_eq!(a.sounds("block.note_block.harp"), strs(&["note/harp2"]));
    // `sounds` entries are strings or `{name, volume}` objects, first one first
    assert_eq!(
        a.sounds("block.note_block.flute"),
        strs(&["note/flute_alt", "note/flute"])
    );
    assert_eq!(a.sounds("block.note_block.nothing"), Vec::<String>::new());
    assert_eq!(a.sounds("ambient.cave"), Vec::<String>::new());
    let (path, sha1) = a.file(&fs, "note/harp2").unwrap();
    assert_eq!(
        path.rsplit('/').next().unwrap(),
        sha1,
        "objects are named by their SHA-1"
    );
    assert!(matches!(
        a.file(&fs, "music/game/calm1"),
        Err(Unavailable::MissingObject(_))
    ));
    // a damaged object is refused by its hash
    assert_eq!(
        a.file(&fs, "note/cow_bell"),
        Err(Unavailable::BadHash(
            "minecraft/sounds/note/cow_bell.ogg".into()
        ))
    );
}

// MinecraftTests.theImportBuildsTheBank
#[test]
fn the_import_builds_the_bank() {
    let (fs, assets, built) = import_at("30");
    let report = &built.report;
    assert_eq!(report.asset_index, assets.index_id);
    // Swift expects at least 13 programs from the game; the synthetic tree has ten
    assert_eq!(report.programs.len(), 10, "skipped: {:?}", report.skipped);
    // the sidecar as it is written and read back
    let side = Sidecar::parse(&built.sidecar.file_text()).expect("the sidecar reads back");
    assert_eq!(side, built.sidecar);
    assert_eq!(side.asset_index, assets.index_id);
    assert_eq!(side.format, mc::FORMAT_VERSION);
    assert_eq!(
        side.version(),
        format!("{}+b{}", assets.index_id, mc::FORMAT_VERSION)
    );
    assert!(
        side.programs.contains(&"harp2".to_string())
            && side.programs.contains(&"bassattack".to_string())
    );
    assert_eq!(
        side.sha1.get("harp2").cloned(),
        Some(assets.file(&fs, "note/harp2").unwrap().1)
    );
    assert!(side.sha1.contains_key("kit:bd"));
    // a zone that reuses another's sound takes its hash
    assert_eq!(side.sha1.get("kit:crash"), side.sha1.get("kit:hat"));
    // the presets: one program per sound at its fixed number, the kit preset
    let presets = sf2_presets(&built.sf2);
    assert!(presets
        .iter()
        .any(|p| p.program == 0 && p.name == "mc harp2"));
    assert!(presets
        .iter()
        .any(|p| p.program == mc::KIT_PROGRAM && p.name == "Minecraft Kit"));
    let kits = drumkits::scan(&[(mc::path("/sf"), presets)]);
    assert_eq!(
        kits.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
        vec![mc::kit_id()]
    );
    // steady tones loop, plucks don't
    for (name, loops) in [("flute", true), ("didgeridoo", true), ("harp2", false)] {
        assert_eq!(
            report
                .programs
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.looped),
            Some(loops),
            "{name}"
        );
    }
    // the same assets build the same bytes
    let again = {
        let mut decode = |p: &str| decode_mono_wav(&fs.read(p).ok_or("no such object")?);
        build(&assets, &fs, &mut decode).unwrap()
    };
    assert_eq!(again.sf2, built.sf2, "deterministic");
    assert_eq!(again.sidecar, built.sidecar);
}

/// The kit is built from the quietest sound at full scale: the kick lands where it allows and
/// each other hit sits at its level against it (`MinecraftBank.kitZones`).
#[test]
fn the_kit_is_balanced_against_the_kick() {
    use djrust::sound::minecraft::import::hit_energy;
    let (fs, assets, built) = import_at("30");
    // read the three kit samples back out of the SF2: smpl chunk, headers in shdr
    let smpl = chunk_body(&built.sf2, "smpl");
    let shdr = chunk_body(&built.sf2, "shdr");
    let kit: Vec<Vec<f32>> = ["kit bd", "kit snare", "kit hat"]
        .iter()
        .map(|name| {
            let rec = shdr
                .chunks_exact(46)
                .find(|r| &r[..name.len()] == name.as_bytes() && r[name.len()] == 0)
                .unwrap_or_else(|| panic!("no sample {name}"));
            let start = u32::from_le_bytes([rec[20], rec[21], rec[22], rec[23]]) as usize;
            let end = u32::from_le_bytes([rec[24], rec[25], rec[26], rec[27]]) as usize;
            (start..end)
                .map(|k| i16::from_le_bytes([smpl[2 * k], smpl[2 * k + 1]]) as f32 / 32767.0)
                .collect()
        })
        .collect();
    let energy: Vec<f64> = kit.iter().map(|p| hit_energy(p)).collect();
    // each hit's energy against the kick's, within 16-bit rounding: bd 0, snare -3, hat -13 dB
    for (e, (want, name)) in energy
        .iter()
        .zip([(0.0, "bd"), (-3.0, "snare"), (-13.0, "hat")])
    {
        assert!(
            (e - energy[0] - want).abs() < 0.05,
            "{name}: {} dB against the kick",
            e - energy[0]
        );
    }
    // the quietest sound (against its level) is at its own full scale: nothing was boosted
    let raw: Vec<f64> = ["note/bd", "note/snare", "note/hat"]
        .iter()
        .map(|s| {
            let (p, _) = assets.file(&fs, s).unwrap();
            hit_energy(&decode_mono_wav(&fs.read(&p).unwrap()).unwrap())
        })
        .collect();
    for (e, r) in energy.iter().zip(&raw) {
        assert!(*e <= *r + 0.01, "cutting only: {e} > {r}");
    }
}

fn chunk_body(d: &[u8], id: &str) -> Vec<u8> {
    fn walk(d: &[u8], start: usize, end: usize, id: &str) -> Option<Vec<u8>> {
        let mut p = start;
        while p + 8 <= end {
            let cid = &d[p..p + 4];
            let size = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
            if cid == b"LIST" {
                if let Some(x) = walk(d, p + 12, p + 8 + size, id) {
                    return Some(x);
                }
            } else if cid == id.as_bytes() {
                return Some(d[p + 8..p + 8 + size].to_vec());
            }
            p += 8 + size + (size % 2);
        }
        None
    }
    walk(d, 12, d.len(), id).unwrap_or_else(|| panic!("no chunk {id}"))
}

// MinecraftTests.octaveZonesCoverTheKeyboard
#[test]
fn octave_zones_cover_the_keyboard() {
    for p in mc::bank_programs().iter().filter(|p| p.kind == Kind::Tone) {
        for root in (p.root - 1)..=(p.root + 1) {
            let zones = octave_zones(root);
            assert_eq!(
                zones.iter().map(|z| z.shift).collect::<Vec<_>>(),
                vec![-1, 0, 1],
                "{}",
                p.name
            );
            let mut next = 0;
            for z in &zones {
                assert_eq!(*z.keys.start(), next, "{}: contiguous", p.name);
                next = *z.keys.end() + 1;
                for k in z.keys.clone().filter(|k| (k - root).abs() <= 24) {
                    let stretch = (k - (root + 12 * z.shift)).abs();
                    let limit = if (k - root).abs() <= 18 { 6 } else { 12 };
                    assert!(stretch <= limit, "{} key {k}", p.name);
                }
            }
            assert_eq!(next, 128);
        }
    }
}

/// `MinecraftSupport.sineWave`: a sine at `freq` Hz, `seconds` long, amplitude 0.5.
fn sine_wave(freq: f64, sr: f64, seconds: f64) -> Vec<f32> {
    (0..(seconds * sr) as usize)
        .map(|k| (0.5 * libm::sin(2.0 * std::f64::consts::PI * freq * k as f64 / sr)) as f32)
        .collect()
}

/// `MinecraftSupport.toneLevelDB`: the level of one frequency in `x` (Hann-windowed DFT, dB re
/// a full-scale sine).
fn tone_level_db(x: &[f32], freq: f64, sr: f64) -> f64 {
    let n = x.len();
    if n <= 1 {
        return -200.0;
    }
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * libm::cos(2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64);
        let ph = 2.0 * std::f64::consts::PI * freq * i as f64 / sr;
        re += w * *v as f64 * libm::cos(ph);
        im -= w * *v as f64 * libm::sin(ph);
        wsum += w;
    }
    let mag = 2.0 * (re * re + im * im).sqrt() / wsum;
    20.0 * libm::log10(mag.max(1e-12))
}

fn rms_db(x: &[f32]) -> f64 {
    let sum: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * libm::log10((sum / x.len() as f64).max(1e-20))
}

// MinecraftTests.octaveShiftsAreBandLimitedOctaves
#[test]
fn octave_shifts_are_band_limited_octaves() {
    let rate = SAMPLE_RATE;
    let a4 = sine_wave(440.0, rate, 0.5);
    let body = |x: &[f32]| x[x.len() / 4..3 * x.len() / 4].to_vec();
    let (up, up_loop) = octave_shift(&a4, Some(1000..3000), 1);
    let (down, down_loop) = octave_shift(&a4, Some(1000..3000), -1);
    assert_eq!(up.len(), a4.len() / 2);
    assert_eq!(down.len(), a4.len() * 2);
    assert_eq!(up_loop, Some(500..1500));
    assert_eq!(down_loop, Some(2000..6000));
    assert_eq!(octave_shift(&a4, None, 0).0, a4);
    for (x, note) in [(&up, 81), (&down, 57)] {
        let (cents, octave, _) = pitch_class_error(&body(x), note, rate);
        assert!(
            octave == 0 && cents.abs() < 1.0,
            "{note}: {cents} cents, octave {octave}"
        );
        assert!(
            (rms_db(&body(x)) - rms_db(&body(&a4))).abs() < 0.1,
            "{note}: level"
        );
    }
    // 15 kHz an octave up would fold to 14.1 kHz; an octave down it leaves an image at
    // 14.55 kHz beside the 7.5 kHz it becomes
    let high = sine_wave(15_000.0, rate, 0.5);
    let reference = tone_level_db(&body(&high), 15_000.0, rate);
    let folded = octave_shift(&high, None, 1).0;
    assert!(
        rms_db(&body(&folded)) - rms_db(&body(&high)) < -60.0,
        "nothing folds back"
    );
    assert!(tone_level_db(&body(&folded), 14_100.0, rate) - reference < -60.0);
    let stretched = octave_shift(&high, None, -1).0;
    assert!((tone_level_db(&body(&stretched), 7_500.0, rate) - reference).abs() < 0.5);
    assert!(
        tone_level_db(&body(&stretched), 14_550.0, rate) - reference < -60.0,
        "no image"
    );
    assert_eq!(octave_shift(&high, None, 1).0, folded);
}

// MinecraftTests.theBankFormatIsInThePaletteVersion (the sidecar half; `recorded` is in
// tests/sound.rs)
#[test]
fn the_bank_format_is_in_the_palette_version() {
    let now = Sidecar::new(
        "32",
        Default::default(),
        strs(&["harp2"]),
        mc::FORMAT_VERSION,
    );
    // what the importer writes, read back
    let side = Sidecar::parse(&now.file_text()).unwrap();
    assert_eq!(side.format, mc::FORMAT_VERSION);
    assert_eq!(mc::FORMAT_VERSION, 2);
    assert_eq!(side.version(), "32+b2");
    // a bank built before formats (no `format` key) still records just its asset index
    let mut json = now.json();
    if let JsonValue::Object(o) = &mut json {
        o.remove("format");
    }
    let old = Sidecar::parse(&format!("{}\n", json.canonical_js())).unwrap();
    assert_eq!(old.format, 1);
    assert_eq!(old.version(), "32");
}

/// The sidecar the importer writes carries the schema, the index, the format, the programs and
/// every sample's SHA-1, keys sorted (`Sidecar.json` through `canonicalJS`).
#[test]
fn the_sidecar_file_is_canonical_json_and_a_newline() {
    let text = Sidecar::new(
        "32",
        [
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "1".to_string()),
        ]
        .into(),
        strs(&["harp2"]),
        2,
    )
    .file_text();
    assert_eq!(
        text,
        "{\"assetIndex\":\"32\",\"format\":2,\"programs\":[\"harp2\"],\"schema\":\"djrust/minecraft-local/v1\",\"sha1\":{\"a\":\"1\",\"b\":\"2\"}}\n"
    );
    assert_eq!(
        Sidecar::parse("{\"schema\":\"other\",\"assetIndex\":\"32\"}"),
        None
    );
    assert_eq!(Sidecar::parse("not json"), None);
}

#[test]
fn sha1_matches_the_published_test_vectors() {
    use djrust::sound::minecraft::assets::sha1_hex;
    // FIPS 180-1 / RFC 3174
    assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(
        sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    assert_eq!(
        sha1_hex(&vec![b'a'; 1_000_000]),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
}

/// The path helpers follow `NSString.appendingPathComponent`.
#[test]
fn paths_join_like_nsstring() {
    assert_eq!(mc::path("/a/b"), "/a/b/minecraft-local.sf2");
    assert_eq!(mc::path("/a/b/"), "/a/b/minecraft-local.sf2");
    assert_eq!(mc::path(""), "minecraft-local.sf2");
    assert_eq!(mc::sidecar_path("/"), "/minecraft-local.json");
}

/// The resampler the decode path uses for the game's 48 kHz sounds (`MinecraftImport.decode`
/// leaves the rate conversion to AVAudioConverter, which cannot be matched): a sine comes out
/// as the same sine at the new rate, a tone above the new Nyquist does not fold back, the
/// passband is flat to 18 kHz, the length is `round(n * to / from)`.
#[test]
fn resample_keeps_a_tone_and_refuses_what_would_fold_back() {
    use djrust::sound::minecraft::import::resample;
    let sine = |hz: f64, rate: f64, n: usize| -> Vec<f32> {
        (0..n)
            .map(|k| (0.5 * libm::sin(2.0 * std::f64::consts::PI * hz * k as f64 / rate)) as f32)
            .collect()
    };
    // 1 kHz, 48 -> 44.1 kHz: the output is the sine at the output's sample times
    let x = sine(1_000.0, 48_000.0, 48_000);
    let y = resample(&x, 48_000.0, 44_100.0);
    assert_eq!(y.len(), 44_100);
    let want = sine(1_000.0, 44_100.0, 44_100);
    let worst = (400..y.len() - 400)
        .map(|k| (y[k] - want[k]).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "a 1 kHz sine is reproduced to {worst}");
    // 44.1 -> 48 kHz up, and back
    let up = resample(&want, 44_100.0, 48_000.0);
    assert_eq!(up.len(), 48_000);
    let worst = (400..up.len() - 400)
        .map(|k| (up[k] - x[k]).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "upsampled to {worst}");
    // a passband tone keeps its level; 23 kHz (above 22.05) is gone, not folded to 21.1 kHz
    let level = |v: &[f32]| rms_db(&v[2_000..v.len() - 2_000]);
    let t18 = resample(&sine(18_000.0, 48_000.0, 48_000), 48_000.0, 44_100.0);
    let ref18 = rms_db(&sine(18_000.0, 48_000.0, 48_000)[2_000..46_000]);
    assert!(
        (level(&t18) - ref18).abs() < 0.1,
        "18 kHz: {} vs {ref18}",
        level(&t18)
    );
    let t23 = resample(&sine(23_000.0, 48_000.0, 48_000), 48_000.0, 44_100.0);
    assert!(
        level(&t23) - ref18 < -60.0,
        "23 kHz leaves {} dB",
        level(&t23) - ref18
    );
    // lengths and the identities
    assert_eq!(resample(&x[..27_527], 48_000.0, 44_100.0).len(), 25_290);
    assert_eq!(
        resample(&x[..1_000], 44_100.0, 44_100.0),
        x[..1_000].to_vec()
    );
    assert!(resample(&[], 48_000.0, 44_100.0).is_empty());
    assert_eq!(resample(&[0.5], 48_000.0, 44_100.0).len(), 1);
}
