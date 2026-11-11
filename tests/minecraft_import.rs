//! The Minecraft importer on the host: the assets on a real file system, the decoder
//! (`MinecraftImport.decode`, AVFoundation in Swift; symphonia here), the bank written to a
//! SoundFont folder, and the bank played back through rustysynth. The arithmetic is held to
//! Swift in `djengine/tests/minecraft_golden.rs`; this file holds the host's half to the same
//! golden (`swift/minecraft.json`) and to the Swift suite's playback tests
//! (`MinecraftTests.theBankPlaysInTune`, `octaveZonesPlayInTuneAndLevel`, `steadyTonesHold`).
//!
//! Every sound is synthetic (see `minecraft_support`); no Minecraft asset is read or needed. The
//! tests that decode need feature `decode`, the playback ones also `sf2`.

#[path = "minecraft_support/mod.rs"]
mod minecraft_support;

use std::path::{Path, PathBuf};

use djrust::json::JsonValue;
use djrust::host::minecraft as host;
use minecraft_support::*;

fn golden() -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/swift/minecraft.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    JsonValue::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A fresh scratch folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("djhost-minecraft-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn sub(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_tree(root: &Path, files: &[(String, Vec<u8>)]) {
    for (rel, bytes) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
}

#[cfg(feature = "decode")]
fn s<'a>(v: &'a JsonValue, key: &str) -> &'a str {
    v.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {key}"))
}

#[cfg(feature = "decode")]
fn n(v: &JsonValue, key: &str) -> f64 {
    v.get(key)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {key}"))
}

#[cfg(feature = "decode")]
fn rows<'a>(v: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {key}"))
}

// MARK: the host's file and folder logic

#[test]
fn the_default_root_follows_the_platform() {
    let home = Some(PathBuf::from("/home/me"));
    // the override is the only candidate
    assert_eq!(
        host::default_roots_in(Some("/x".into()), None, home.clone(), false),
        vec![PathBuf::from("/x")]
    );
    // macOS: PrismLauncher's folder, as Swift
    assert_eq!(
        host::default_roots_in(None, None, Some("/Users/me".into()), true),
        vec![PathBuf::from(
            "/Users/me/Library/Application Support/PrismLauncher/assets"
        )]
    );
    // elsewhere: PrismLauncher's, then the vanilla launcher's
    assert_eq!(
        host::default_roots_in(None, None, home.clone(), false),
        vec![
            PathBuf::from("/home/me/.local/share/PrismLauncher/assets"),
            PathBuf::from("/home/me/.minecraft/assets")
        ]
    );
    assert_eq!(
        host::default_roots_in(None, Some("/xdg".into()), home, false),
        vec![
            PathBuf::from("/xdg/PrismLauncher/assets"),
            PathBuf::from("/home/me/.minecraft/assets")
        ]
    );
    assert!(host::default_roots_in(None, None, None, false).is_empty());
}

#[test]
fn locating_on_disk_finds_what_the_in_memory_tree_does() {
    let g = golden();
    let dir = Scratch::new("locate");
    let root = dir.sub("assets");
    write_tree(&root, &tree_files(&g));
    let root_s = root.to_str().unwrap();
    let a = host::locate(root_s, None).unwrap();
    assert_eq!(a.index_id, "30", "the newest index");
    assert_eq!(a.object_count(), 16);
    assert_eq!(a.event_count(), 8);
    assert_eq!(host::locate(root_s, Some("29")).unwrap().index_id, "29");
    assert_eq!(
        a.sounds("block.note_block.flute"),
        vec!["note/flute_alt", "note/flute"]
    );
    let (path, sha1) = a.file(&host::StdFs, "note/harp2").unwrap();
    assert!(path.ends_with(&sha1) && path.starts_with(root_s), "{path}");
    // a folder without the game
    let err = host::locate(dir.sub("nothing").to_str().unwrap(), None).unwrap_err();
    assert!(
        err.to_string().starts_with("no Minecraft assets at "),
        "{err}"
    );
    // objects but no index
    std::fs::create_dir_all(dir.sub("bare/objects")).unwrap();
    let err = host::locate(dir.sub("bare").to_str().unwrap(), None).unwrap_err();
    assert!(err.to_string().ends_with("/indexes"), "{err}");
}

#[test]
fn the_bank_is_installed_when_the_file_and_a_readable_sidecar_are() {
    let dir = Scratch::new("installed");
    let sf = dir.path();
    assert!(!host::is_installed(sf));
    assert_eq!(host::load_sidecar(sf), None);
    std::fs::write(sf.join("minecraft-local.sf2"), b"RIFF").unwrap();
    assert!(!host::is_installed(sf), "the bank without a sidecar");
    std::fs::write(sf.join("minecraft-local.json"), "not json").unwrap();
    assert!(!host::is_installed(sf), "an unreadable sidecar");
    let side =
        djrust::sound::minecraft::Sidecar::new("32", Default::default(), vec!["harp2".into()], 2);
    std::fs::write(sf.join("minecraft-local.json"), side.file_text()).unwrap();
    assert!(host::is_installed(sf));
    assert_eq!(host::load_sidecar(sf), Some(side.clone()));
    assert_eq!(host::unavailable_reason(sf), None);
    // the library sees it, with the palette's version
    let lib = djrust::host::sampler::Library::scan(sf);
    assert!(lib.installed.minecraft_installed());
    assert_eq!(
        lib.installed.minecraft.as_ref().map(|m| m.version()),
        Some("32+b2".to_string())
    );
    assert_eq!(lib.installed.minecraft_reason(), None);
}

// MARK: the decoder

#[cfg(feature = "decode")]
mod decoding {
    use super::*;

    fn decode_case<'a>(g: &'a JsonValue, name: &str) -> &'a JsonValue {
        rows(g, "decodes")
            .iter()
            .find(|c| s(c, "case") == name)
            .unwrap_or_else(|| panic!("no case {name}"))
    }

    fn write_wav(dir: &Path, name: &str, frames: &[i16], channels: u16, rate: u32) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, wav(frames, channels, rate)).unwrap();
        p
    }

    /// `MinecraftImport.decode` of a 16-bit mono 44.1 kHz WAV: the samples as Int16 / 32768,
    /// peak-normalised to 0.891. Bit-identical to AVAudioFile + AVAudioConverter.
    #[test]
    fn mono_44100_decodes_bit_for_bit_as_swift_does() {
        let g = golden();
        let dir = Scratch::new("decode-mono");
        let mono = Synth::tri(372.5, 5_000).int16();
        let got = host::decode_sound(&write_wav(dir.path(), "mono", &mono, 1, 44_100)).unwrap();
        let want = decode_case(&g, "mono 44.1 kHz");
        assert_eq!(got.len(), n(want, "count") as usize);
        assert_eq!(floats_sha(&got), s(want, "sha"));
        // the quiet one is brought up to the same peak, the silent one stays silent
        let quiet: Vec<i16> = mono.iter().map(|v| v / 64).collect();
        let got = host::decode_sound(&write_wav(dir.path(), "quiet", &quiet, 1, 44_100)).unwrap();
        assert_eq!(floats_sha(&got), s(decode_case(&g, "quiet"), "sha"));
        let peak = got.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.891).abs() < 1e-6, "{peak}");
        let silent =
            host::decode_sound(&write_wav(dir.path(), "silent", &[0; 300], 1, 44_100)).unwrap();
        assert!(silent.len() == 300 && silent.iter().all(|&x| x == 0.0));
    }

    /// Swift's converter keeps only the first channel of a stereo file (the golden shows it is
    /// not the average); so does the Rust decode. Bit-identical.
    #[test]
    fn a_stereo_file_keeps_its_first_channel_as_swift_does() {
        let g = golden();
        let dir = Scratch::new("decode-stereo");
        let left = Synth::tri(300.0, 4_000).int16();
        let right = Synth::tri_hold(500.0, 4_000, 1_000).int16();
        let inter: Vec<i16> = left
            .iter()
            .zip(&right)
            .flat_map(|(l, r)| [*l, *r])
            .collect();
        let got = host::decode_sound(&write_wav(dir.path(), "stereo", &inter, 2, 44_100)).unwrap();
        let want = decode_case(&g, "stereo 44.1 kHz");
        assert_eq!(got.len(), n(want, "count") as usize);
        assert_eq!(floats_sha(&got), s(want, "sha"));
        assert_eq!(
            want.get("exactAverage").and_then(JsonValue::as_bool),
            Some(false),
            "Swift's output is not the average of the channels"
        );
    }

    /// 48 kHz is the one path that is not Swift's: AVAudioConverter's sample-rate converter
    /// against a windowed-sinc resampler (`import::resample`). The frame count agrees; the
    /// twelve samples the dump kept differ by 0.028 of full scale at worst on this triangle (its
    /// corners are the hardest thing for two different filters to agree on; the old linear
    /// interpolation was 0.095 off). On the game's own 48 kHz sounds the two banks agree to
    /// 46..92 dB below the signal (not committed: they are the game's).
    #[test]
    fn a_48k_file_is_resampled_to_the_same_length_and_near_apples_samples() {
        let g = golden();
        let dir = Scratch::new("decode-48k");
        let tone = Synth::tri(372.5, 24_000).int16();
        let got = host::decode_sound(&write_wav(dir.path(), "rate48", &tone, 1, 48_000)).unwrap();
        let want = decode_case(&g, "mono 48 kHz");
        assert_eq!(
            got.len(),
            n(want, "count") as usize,
            "same frame count as Apple's converter"
        );
        // Apple's first and middle samples, from the golden, against ours
        let bits = |x: &JsonValue| -> Vec<f32> {
            x.as_array()
                .unwrap()
                .iter()
                .map(|b| f32::from_bits(u32::from_str_radix(b.as_str().unwrap(), 16).unwrap()))
                .collect()
        };
        let peek = want.get("peek").unwrap();
        let head = bits(peek.get("head").unwrap());
        let mid = bits(peek.get("mid").unwrap());
        let at = got.len() / 2 - 3;
        let worst = head
            .iter()
            .zip(&got[..6])
            .chain(mid.iter().zip(&got[at..at + 6]))
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(
            worst < 0.05,
            "worst error against Apple's converter on the peeked samples: {worst}"
        );
    }

    /// The whole import on disk: `MinecraftImport.run` writes the bank and its sidecar, and
    /// they are the bytes Swift writes for the same tree (decoded by symphonia rather than
    /// AVAudioFile, which agree exactly on 16-bit mono 44.1 kHz).
    fn check_import_on_disk(index: &str, key: &str) {
        let g = golden();
        let dir = Scratch::new(&format!("import-{index}"));
        let root = dir.sub("assets");
        write_tree(&root, &tree_files(&g));
        let assets = host::locate(root.to_str().unwrap(), Some(index)).unwrap();
        let sf = dir.sub("soundfonts");
        let done = host::run(&assets, &sf).expect("the import runs");
        assert_eq!(done.path, sf.join("minecraft-local.sf2"));
        let want = g.get(key).unwrap();
        let bank = std::fs::read(&done.path).unwrap();
        assert_eq!(bank.len(), n(want, "length") as usize);
        assert_eq!(bytes_sha(&bank), s(want, "sha"), "{key}: the bank's bytes");
        let side = std::fs::read_to_string(sf.join("minecraft-local.json")).unwrap();
        assert_eq!(side, s(want, "sidecarText"), "{key}: the sidecar");
        assert_eq!(done.report.bytes, bank.len());
        assert_eq!(
            done.report.skipped,
            want.get("skipped").and_then(JsonValue::strings).unwrap()
        );
        assert_eq!(
            done.report
                .programs
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            rows(want, "programs")
                .iter()
                .map(|p| s(p, "name").to_string())
                .collect::<Vec<_>>()
        );
        // no temp file is left behind, and the host sees the installed bank
        let mut names: Vec<String> = std::fs::read_dir(&sf)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["minecraft-local.json", "minecraft-local.sf2"]);
        assert!(host::is_installed(&sf));
        let lib = djrust::host::sampler::Library::scan(&sf);
        assert_eq!(
            lib.installed
                .kits
                .iter()
                .map(|k| k.id.clone())
                .collect::<Vec<_>>(),
            vec!["minecraft-local.sf2#0:127".to_string()]
        );
        assert_eq!(
            lib.list_presets("minecraft-local.sf2")
                .iter()
                .filter(|p| p.contains("mc harp2"))
                .count(),
            1
        );
        // running it again overwrites with the same bytes
        host::run(&assets, &sf).unwrap();
        assert_eq!(std::fs::read(&done.path).unwrap(), bank);
    }

    #[test]
    fn the_import_at_index_29_writes_swifts_bank_to_disk() {
        check_import_on_disk("29", "import29");
    }

    #[test]
    fn the_import_at_index_30_writes_swifts_bank_to_disk() {
        check_import_on_disk("30", "import30");
    }

    #[test]
    fn a_damaged_or_undecodable_kit_object_stops_the_import_and_writes_nothing() {
        let g = golden();
        let dir = Scratch::new("import-broken");
        let root = dir.sub("assets");
        write_tree(&root, &tree_files(&g));
        let assets = host::locate(root.to_str().unwrap(), Some("30")).unwrap();
        let sf = dir.sub("soundfonts");
        // the snare object is not what the index says: refused by its hash, before any decode
        let (snare, _) = assets.file(&host::StdFs, "note/snare").unwrap();
        std::fs::write(&snare, b"not audio at all").unwrap();
        let err = host::run(&assets, &sf).unwrap_err();
        assert!(
            err.to_string()
                .contains("note/snare.ogg does not match its SHA-1"),
            "{err}"
        );
        assert!(!sf.exists(), "nothing was written");
        // an object that is what the index says but is not audio: the decoder's error stops it
        let mut sounds = vec![
            (
                "note/bd".to_string(),
                mono_wav(&Synth::tri_decay(55.0, 22_050, 15_000).int16()),
            ),
            (
                "note/snare".to_string(),
                b"hashes fine, decodes never".to_vec(),
            ),
            (
                "note/hat".to_string(),
                mono_wav(&Synth::noise(23, 3_000, 2_000).int16()),
            ),
        ];
        sounds.sort();
        let root = dir.sub("assets2");
        write_tree(&root, &custom_tree("32", &sounds));
        let assets = host::locate(root.to_str().unwrap(), None).unwrap();
        let err = host::run(&assets, &sf).unwrap_err();
        assert!(
            err.to_string().contains("not an audio file I can read"),
            "{err}"
        );
        assert!(!sf.exists(), "nothing was written");
    }

    /// Ogg Vorbis, what the real assets are, through symphonia. Needs `ffmpeg` (its native Vorbis
    /// encoder, stereo only: the mono tone is written on both channels and the decoder keeps the
    /// first) to make the file; skips with a message without it.
    #[test]
    fn ogg_vorbis_objects_decode_and_import() {
        use std::process::Command;
        let dir = Scratch::new("ogg");
        let tone = Synth::tri_hold(372.5, 22_050, 1_500).int16();
        let wav_path = write_wav(dir.path(), "tone.wav", &tone, 1, 44_100);
        let ogg_path = dir.sub("tone.ogg");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&wav_path)
            .args(["-c:a", "vorbis", "-strict", "-2", "-ac", "2"])
            .arg(&ogg_path)
            .status();
        if !matches!(made, Ok(st) if st.success()) || !ogg_path.exists() {
            eprintln!("skipped: no ffmpeg to make an Ogg Vorbis file with");
            return;
        }
        let pcm = host::decode_sound(&ogg_path).expect("symphonia reads Ogg Vorbis");
        // the codec adds a little padding; the length is within a block of the source
        assert!(
            (pcm.len() as i64 - tone.len() as i64).abs() < 2_048,
            "{} frames",
            pcm.len()
        );
        let peak = pcm.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.891).abs() < 1e-5, "peak-normalised: {peak}");
        // it is the same tone: the root measured on it is the source's, to a few cents
        let (root, cents) = djrust::sound::minecraft::import::measure_root(&pcm, 66);
        let (root0, cents0) = djrust::sound::minecraft::import::measure_root(
            &{
                let mut x = floats(&tone);
                djrust::sound::minecraft::import::peak_normalise(&mut x);
                x
            },
            66,
        );
        assert_eq!(root, root0);
        assert!(
            (cents - cents0).abs() < 3.0,
            "ogg {cents} cents, wav {cents0} cents"
        );
    }
}

// MARK: playing the built bank

#[cfg(all(feature = "decode", feature = "sf2"))]
mod playing {
    use super::*;
    use djrust::sound::minecraft as mc;
    use djrust::voices::pitch::pitch_class_error;
    use djrust::voices::SamplerSource;
    use djrust::host::sampler::{NoteEvent, Sf2Sampler};

    const SR: u32 = 48_000;

    /// Long tones (2 s) so the octave-up copies (1 s) outlast the measuring window: harp-like
    /// plucks that die in a second, a flute-like tone that holds.
    fn bank(tag: &str) -> (Scratch, PathBuf, djrust::sound::minecraft::import::Report) {
        let dir = Scratch::new(&format!("play-{tag}"));
        let long = 88_200;
        let pluck = |hz: f64| Synth::tri_decay(hz, long, 44_100);
        let mut sounds: Vec<(String, Vec<u8>)> = vec![
            ("note/harp2".into(), mono_wav(&pluck(372.5).int16())),
            ("note/bassattack".into(), mono_wav(&pluck(92.0).int16())),
            ("note/bell".into(), mono_wav(&pluck(1_475.0).int16())),
            (
                "note/flute".into(),
                mono_wav(&Synth::tri_hold(741.0, long, 1_800).int16()),
            ),
            (
                "note/bd".into(),
                mono_wav(&Synth::tri_decay(55.0, 22_050, 15_000).int16()),
            ),
            (
                "note/snare".into(),
                mono_wav(&Synth::noise(11, 8_000, 6_000).int16()),
            ),
            (
                "note/hat".into(),
                mono_wav(&Synth::noise(23, 3_000, 2_000).int16()),
            ),
        ];
        sounds.sort();
        let root = dir.sub("assets");
        write_tree(&root, &custom_tree("32", &sounds));
        let assets = host::locate(root.to_str().unwrap(), None).unwrap();
        let sf = dir.sub("soundfonts");
        let done = host::run(&assets, &sf).unwrap();
        (dir, done.path, done.report)
    }

    fn play(sampler: &mut Sf2Sampler, key: u8, seconds: f64, hold: f64) -> Vec<f32> {
        let frames = (seconds * SR as f64) as usize;
        let (l, r) = sampler.render_notes(
            &[NoteEvent {
                at: 0,
                frames: (hold * SR as f64) as usize,
                key,
                velocity: 100,
            }],
            frames,
        );
        sampler.silence();
        l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        let sum: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
        10.0 * (sum / x.len() as f64).max(1e-20).log10()
    }

    fn at(s: f64) -> usize {
        (s * SR as f64) as usize
    }

    // MinecraftTests.theBankPlaysInTune
    #[test]
    fn the_bank_plays_in_tune_at_its_roots() {
        let (_dir, path, report) = bank("roots");
        let path = path.to_str().unwrap().to_string();
        // every built tone loads as a preset of the bank
        for p in &report.programs {
            Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), p.program), SR)
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
        }
        for name in ["harp2", "flute", "bell", "bassattack"] {
            let prog = mc::bank_programs().iter().find(|p| p.name == name).unwrap();
            let mut s =
                Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), prog.program), SR).unwrap();
            let note = prog.root + if name == "bassattack" { 12 } else { 0 };
            let x = play(&mut s, note as u8, 0.4, 0.5);
            let (cents, _, level) = pitch_class_error(&x[at(0.03)..at(0.25)], note, SR as f64);
            assert!(level > 0.0, "{name}: silent");
            assert!(cents.abs() < 12.0, "{name}: {cents} cents");
        }
        // the kit loads as a kit
        let kit = SamplerSource::Kit {
            path: path.clone(),
            bank: 0,
            program: mc::KIT_PROGRAM,
        };
        let mut k = Sf2Sampler::open(&kit, SR).expect("the kit preset opens");
        assert!(
            k.normalize_kit(),
            "the kit is fit to play: {:?}",
            k.last_check
        );
    }

    // MinecraftTests.octaveZonesPlayInTuneAndLevel
    #[test]
    fn every_key_within_two_octaves_plays_in_tune_and_zones_meet_at_one_level() {
        let (_dir, path, report) = bank("zones");
        let path = path.to_str().unwrap().to_string();
        let built: Vec<&str> = report.programs.iter().map(|p| p.name.as_str()).collect();
        let mut worst = 0f64;
        for name in ["harp2", "flute", "bell", "bassattack"] {
            assert!(built.contains(&name));
            let prog = mc::bank_programs().iter().find(|p| p.name == name).unwrap();
            let mut s =
                Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), prog.program), SR).unwrap();
            let down_zone_end = djrust::sound::minecraft::import::octave_zones(prog.root)
                .iter()
                .find(|z| z.shift == 0)
                .map(|z| *z.keys.start())
                .unwrap();
            let mut note = prog.root - 24;
            while note <= prog.root + 24 {
                if (24..=108).contains(&note) {
                    let x = play(&mut s, note as u8, 0.5, 0.5);
                    // The octave-down zone plays the recording at half speed, so a held tone's
                    // attack lasts twice as long: measure those keys after it, on the steady
                    // loop. Everything else from as it speaks over 0.44 s.
                    let window = if prog.looped && note < down_zone_end {
                        at(0.25)..at(0.47)
                    } else {
                        at(0.03)..at(0.47)
                    };
                    let (cents, _, level) = pitch_class_error(&x[window], note, SR as f64);
                    assert!(level > 0.0, "{name} @{note}: silent");
                    worst = worst.max(cents.abs());
                    // within 5 cents, read on `pitchClassError`'s 1-cent grid
                    assert!(cents.abs() <= 5.0, "{name} @{note}: {cents} cents");
                }
                note += 3;
            }
            let zones = djrust::sound::minecraft::import::octave_zones(prog.root);
            for z in &zones[..zones.len() - 1] {
                let below = *z.keys.end();
                let a = rms_db(&play(&mut s, below as u8, 0.25, 0.25)[at(0.03)..at(0.25)]);
                let b = rms_db(&play(&mut s, below as u8 + 1, 0.25, 0.25)[at(0.03)..at(0.25)]);
                assert!(
                    (a - b).abs() <= 3.0,
                    "{name} split {below}|{}: {a} -> {b} dB",
                    below + 1
                );
            }
        }
        eprintln!("worst pitch error across the zones: {worst} cents");
    }

    // MinecraftTests.steadyTonesHold
    #[test]
    fn a_held_flute_note_sustains_on_its_loop_and_a_harp_pluck_dies_away() {
        let (_dir, path, _) = bank("hold");
        let path = path.to_str().unwrap().to_string();
        let tail = |name: &str| -> f64 {
            let prog = mc::bank_programs().iter().find(|p| p.name == name).unwrap();
            let mut s =
                Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), prog.program), SR).unwrap();
            let x = play(&mut s, prog.root as u8, 2.2, 2.5);
            rms_db(&x[at(1.8)..]) - rms_db(&x[at(0.05)..at(0.3)])
        };
        assert!(tail("flute") > -15.0, "flute holds");
        assert!(tail("harp2") < -30.0, "harp decays");
    }
}

/// The imported bank and its sidecar in the SoundFont folder make the Minecraft palette
/// playable in the sound chooser (local only: refused for publishing, not for local play).
/// What a booth build with `sf2` scans at launch.
#[test]
fn an_imported_bank_lights_the_minecraft_palette() {
    let dir = Scratch::new("palette");
    let sf = dir.path();
    let lib = djrust::host::sampler::Library::scan(sf);
    let card = |lib: &djrust::host::sampler::Library| {
        djrust::sound::PaletteCard::cards(
            djrust::sound::PaletteSet::shared(),
            None,
            lib.sounds(),
        )
        .into_iter()
        .find(|c| c.id == "minecraft")
        .expect("a minecraft card")
    };
    assert!(card(&lib).unavailable.is_some(), "nothing imported");
    std::fs::write(sf.join("minecraft-local.sf2"), b"RIFF").unwrap();
    let side =
        djrust::sound::minecraft::Sidecar::new("32", Default::default(), vec!["harp2".into()], 2);
    std::fs::write(sf.join("minecraft-local.json"), side.file_text()).unwrap();
    let lib = djrust::host::sampler::Library::scan(sf);
    let c = card(&lib);
    assert_eq!(c.unavailable, None, "playable here");
    assert!(c.local_only, "but never published");
}

/// A build without `sf2` never reads the SoundFont folder: its greyed palettes say so, rather
/// than "not imported yet" when the bank is there.
#[cfg(not(feature = "sf2"))]
#[test]
fn without_the_sampler_the_reason_names_the_build() {
    let (mut e, _r) = djrust::host::deck::engine(djrust::host::deck::EngineConfig::new(24_000.0));
    assert!(e.generate(
        "A",
        &djrust::TrackSpec::new("house", 11.0, Some("minimal"))
    ));
    let cards = e.palette_cards("A");
    let mc = cards.iter().find(|c| c.id == "minecraft").unwrap();
    let why = mc.unavailable.as_deref().unwrap();
    assert!(why.starts_with(djrust::host::deck::moves::NO_SAMPLER), "{why}");
    assert!(
        cards.iter().any(|c| c.unavailable.is_none()),
        "the synth palettes still play"
    );
}
