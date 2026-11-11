//! The host sampler: folder scan, preset lists, ranking over what is installed, and the
//! rustysynth path with the load-time checks. SoundFonts are synthesised here
//! (`synthetic_sf2`), so nothing is downloaded; a test against a real bank runs only when
//! `$DJ_SOUNDFONTS` holds one. Golden: none.

use std::path::PathBuf;

use djrust::voices::SamplerSource;
use djrust::host::sampler::{sound_font_dirs, Library};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("djhost-sampler-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_missing_folder_is_an_empty_library() {
    let lib = Library::scan(&std::env::temp_dir().join("djhost-sampler-nonesuch-dir"));
    assert!(lib.installed.files.is_empty() && lib.sound_fonts().is_empty());
    assert!(lib.installed.kits.is_empty());
    assert!(!lib.sounds().installed.gm_bank);
    // nothing installed: a category ranks Apple's GM only (which this host cannot play)
    assert_eq!(
        lib.catalog.sources("piano", "melody", 1.0, false),
        Some(vec![SamplerSource::Gm(0), SamplerSource::Gm(1)])
    );
}

#[test]
fn the_search_path_honours_the_override() {
    use djrust::host::sampler::sound_font_dirs_in;
    let p = |s: &str| Some(PathBuf::from(s));
    assert_eq!(
        sound_font_dirs_in(p("/some/where"), p("/x"), p("/home/u")),
        vec![PathBuf::from("/some/where")]
    );
    assert_eq!(
        sound_font_dirs_in(None, None, p("/home/u")),
        vec![
            PathBuf::from("/home/u/.local/share/beats-btw-dj/soundfonts"),
            PathBuf::from("/home/u/Library/Application Support/beats-btw-dj/soundfonts"),
            PathBuf::from("/home/u/Library/Application Support/djrust/soundfonts"),
        ]
    );
    assert_eq!(
        sound_font_dirs_in(None, p("/xdg"), p("/home/u"))[0],
        PathBuf::from("/xdg/beats-btw-dj/soundfonts")
    );
    assert!(sound_font_dirs_in(None, None, None).is_empty());
    let _ = sound_font_dirs();
}

#[cfg(feature = "sf2")]
mod sf2 {
    use super::*;
    use djrust::sound::{sources_for_option, PaletteSet};
    use djrust::voices::sampler as rules;
    use djrust::host::sampler::{
        open_first_fit, synthetic_sf2, synthetic_sf2_tuned, NoteEvent, Sf2Sampler,
    };

    fn write(dir: &std::path::Path, name: &str, bytes: Vec<u8>) -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn rms_db(l: &[f32], r: &[f32]) -> f64 {
        let sum: f64 = l
            .iter()
            .zip(r)
            .map(|(a, b)| (0.5 * (*a as f64 + *b as f64)).powi(2))
            .sum();
        rules::rms_db(sum, l.len())
    }

    #[test]
    fn the_library_lists_banks_presets_and_kits() {
        let d = scratch("lib");
        write(
            &d,
            "MuseScore_General.sf2",
            synthetic_sf2(&[
                ("Grand Piano", 0, 0),
                ("Standard", 128, 0),
                ("Room", 128, 8),
                ("Drum Kit Lite", 0, 6),
            ]),
        );
        write(&d, "Other.sf2", synthetic_sf2(&[("Piano 2", 0, 1)]));
        write(&d, "notes.txt", b"x".to_vec());
        let lib = Library::scan(&d);
        assert_eq!(
            lib.installed.files,
            vec!["MuseScore_General.sf2", "Other.sf2", "notes.txt"]
        );
        // other banks by name, the floor last
        let fonts: Vec<String> = lib
            .sound_fonts()
            .iter()
            .map(|p| p.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(fonts, vec!["Other.sf2", "MuseScore_General.sf2"]);
        assert_eq!(
            lib.list_presets("MuseScore_General.sf2"),
            vec![
                "000:000 Grand Piano",
                "128:000 Standard",
                "128:008 Room",
                "000:006 Drum Kit Lite"
            ]
        );
        let kit_ids: Vec<&str> = lib.installed.kits.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(
            kit_ids,
            vec![
                "MuseScore_General.sf2#0:6",
                "MuseScore_General.sf2#128:0",
                "MuseScore_General.sf2#128:8"
            ]
        );
        // piano: Other's program 0/1 first, then the floor's
        let s = lib.catalog.sources("piano", "melody", 1.0, false).unwrap();
        let names: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "sf2:Other.sf2#0",
                "sf2:Other.sf2#1",
                "sf2:MuseScore_General.sf2#0",
                "sf2:MuseScore_General.sf2#1",
                "gm:0",
                "gm:1"
            ]
        );
        // which of the category's programs each bank really has
        let have = lib.category_programs("keys.piano");
        assert!(have.contains(&(
            "MuseScore_General.sf2".into(),
            0,
            "Grand Piano".into(),
            Some("Grand Piano".into())
        )));
        assert!(have.contains(&(
            "Other.sf2".into(),
            1,
            "Bright Grand Piano".into(),
            Some("Piano 2".into())
        )));
        assert!(have.contains(&(
            "MuseScore_General.sf2".into(),
            1,
            "Bright Grand Piano".into(),
            None
        )));
        // palette options resolve over the scanned folder
        let set = PaletteSet::shared();
        let gm = sources_for_option(set, "gm:0", "bass", 1.0, lib.sounds()).unwrap();
        assert_eq!(gm.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_preset_plays_in_tune_and_is_levelled_to_the_lead_voice() {
        let d = scratch("play");
        let path = write(
            &d,
            "t.sf2",
            synthetic_sf2(&[("Sine", 0, 0), ("Kit", 128, 0)]),
        );
        let sr = 44_100;
        let mut s = Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), 0), sr).unwrap();
        assert!(s.normalize(true), "{:?}", s.last_check);
        let chk = s.last_check.unwrap();
        assert!(chk.held && chk.cents.abs() <= 1.0, "{chk:?}");
        assert!(s.gain > 0.02 && s.gain < 20.0);
        // the gain lands the reference phrase on the target
        let sr_f = sr as f64;
        let notes: Vec<NoteEvent> = rules::REFERENCE_PHRASE
            .iter()
            .map(|p| NoteEvent {
                at: (p.at * sr_f).round() as usize,
                frames: (p.seconds * sr_f) as usize,
                key: p.note,
                velocity: 100,
            })
            .collect();
        let (l, r) = s.render_notes(&notes, (2.0 * sr_f) as usize);
        let db = rms_db(&l, &r);
        assert!((db - rules::TARGET_RMS_DB).abs() < 0.3, "{db}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_kit_is_levelled_on_the_kit_phrase() {
        let d = scratch("kit");
        let path = write(
            &d,
            "k.sf2",
            synthetic_sf2(&[("Standard", 128, 0), ("Sine", 0, 0)]),
        );
        let kit = SamplerSource::Kit {
            path,
            bank: 128,
            program: 0,
        };
        let mut s = Sf2Sampler::open(&kit, 44_100).unwrap();
        assert!(s.normalize_kit(), "{:?}", s.last_check);
        let notes: Vec<NoteEvent> = rules::kit_phrase()
            .iter()
            .map(|p| NoteEvent {
                at: (p.at * 44_100.0_f64).round() as usize,
                frames: (p.seconds * 44_100.0) as usize,
                key: p.note,
                velocity: 100,
            })
            .collect();
        let (l, r) = s.render_notes(&notes, (rules::KIT_SECONDS * 44_100.0) as usize);
        let db = rms_db(&l, &r);
        assert!((db - rules::KIT_TARGET_RMS_DB).abs() < 0.3, "{db}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn notes_land_on_their_frame() {
        let d = scratch("timing");
        let path = write(&d, "t.sf2", synthetic_sf2(&[("Sine", 0, 0)]));
        let mut s = Sf2Sampler::open(&SamplerSource::Sf2(path, 0), 44_100).unwrap();
        let (l, r) = s.render_notes(
            &[NoteEvent {
                at: 1000,
                frames: 4000,
                key: 60,
                velocity: 100,
            }],
            8000,
        );
        assert!(
            l[..1000].iter().chain(&r[..1000]).all(|x| *x == 0.0),
            "silent before the note"
        );
        assert!(
            l[1000..2000].iter().any(|x| x.abs() > 1e-4),
            "sounding from it"
        );
        // released at 5000: the tail is quiet again well before the end
        assert!(l[7500..].iter().all(|x| x.abs() < 1e-3), "released");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sound_out_of_tune_is_turned_away() {
        let d = scratch("tune");
        let flat = write(&d, "flat.sf2", synthetic_sf2_tuned(&[("Sine", 0, 0)], 50));
        let near = write(&d, "near.sf2", synthetic_sf2_tuned(&[("Sine", 0, 0)], 20));
        let ok = write(&d, "ok.sf2", synthetic_sf2(&[("Sine", 0, 0)]));
        let mut s = Sf2Sampler::open(&SamplerSource::Sf2(flat.clone(), 0), 44_100).unwrap();
        assert!(!s.normalize(true));
        let c = s.last_check.unwrap();
        assert!((c.cents.abs() - 50.0).abs() < 2.0, "{c:?}");
        // a local-only bank skips the tuning check
        assert!(s.normalize(false));
        let mut n = Sf2Sampler::open(&SamplerSource::Sf2(near.clone(), 0), 44_100).unwrap();
        assert!(
            n.normalize(true),
            "20 cents is within 30: {:?}",
            n.last_check
        );
        // down a ranked list: EXS and Apple's bank are skipped (and said so, in the words of
        // `SamplerBank.loadFailure`), the out-of-tune bank is turned away, the last plays
        let exs = write(&d, "Grand.exs", b"not an exs".to_vec());
        let list = vec![
            SamplerSource::Exs("/lib/Grand.exs".into()), // not on disk
            SamplerSource::Exs(exs),                     // on disk, but only AUSampler reads it
            SamplerSource::Gm(0),
            SamplerSource::Sf2(flat, 0),
            SamplerSource::Sf2(ok.clone(), 5), // no such preset
            SamplerSource::Sf2(ok, 0),
        ];
        let (played, why) = open_first_fit(&list, 44_100).unwrap();
        assert_eq!(played.source, list[5]);
        assert_eq!(why.len(), 5);
        assert_eq!(why[0], "exs:Grand.exs: not on disk");
        assert!(
            why[1].contains("load failed: EXS needs AUSampler"),
            "{}",
            why[1]
        );
        // (Apple's bank: on a Mac the file is there and only AUSampler reads it)
        assert!(
            why[2].contains("not on disk")
                || why[2].contains("load failed: Apple's gs_instruments"),
            "{}",
            why[2]
        );
        assert!(
            why[3].starts_with("sf2:flat.sf2#0: out of tune (")
                && why[3].ends_with(" cents, limit 30)"),
            "{}",
            why[3]
        );
        assert!(why[4].contains("load failed: no preset 0:5"), "{}", why[4]);
        assert!(open_first_fit(&list[..3], 44_100).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `Sampler.swift` 96-180: an instrument costs the preset's own samples, never the whole file
    /// again. Every sampler opened on one file shares one parsed SoundFont (one `Arc`, which each
    /// synthesizer clones), so 24 slots on a 215 MB bank hold it once, and a source that is evicted
    /// and loaded again does not read it again.
    #[test]
    fn samplers_of_one_file_share_one_parsed_font() {
        use std::sync::Arc;
        let d = scratch("share");
        let path = write(&d, "two.sf2", synthetic_sf2(&[("A", 0, 0), ("B", 0, 1)]));
        let open = |p: i64| Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), p), 44_100);
        let a = open(0).unwrap();
        let b = open(1).unwrap();
        assert!(Arc::ptr_eq(a.font(), b.font()), "one parse for two presets");
        // another sample rate is another synthesizer over the same font
        let c = Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), 0), 22_050).unwrap();
        assert!(Arc::ptr_eq(a.font(), c.font()));
        // the second open came from the parsed font, not the file: it works with the file gone
        std::fs::remove_file(&path).unwrap();
        let e = open(1).expect("served from the parsed font, not the file");
        assert!(Arc::ptr_eq(a.font(), e.font()));
        assert!(open(5).is_err(), "a preset the font lacks is still refused");
        // a font nothing plays any more is freed with its file gone: not kept for the process's life
        drop((a, b, c, e));
        let err = open(0)
            .err()
            .expect("the font is gone with its last sampler");
        assert!(
            err.to_string().to_lowercase().contains("no such file"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The cache is keyed by the file as it was read: a bank reinstalled over the old one (another
    /// size and modification time) is read again, while the sampler on the old one plays on.
    #[test]
    fn a_reinstalled_font_is_read_again() {
        use std::sync::Arc;
        let d = scratch("reinstall");
        let path = write(&d, "bank.sf2", synthetic_sf2(&[("A", 0, 0)]));
        let open = |p: i64| Sf2Sampler::open(&SamplerSource::Sf2(path.clone(), p), 44_100);
        let mut old = open(0).unwrap();
        assert!(open(1).is_err(), "no program 1 yet");
        write(&d, "bank.sf2", synthetic_sf2(&[("A", 0, 0), ("B", 0, 1)]));
        let new = open(1).expect("the new file is read");
        assert!(
            !Arc::ptr_eq(old.font(), new.font()),
            "a second font, not the stale one"
        );
        assert!(old.normalize(true), "the old sampler is untouched");
        // and a third ask shares the new one
        assert!(Arc::ptr_eq(new.font(), open(0).unwrap().font()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Threads asking for one font at the same time read it once.
    #[test]
    fn threads_asking_for_one_font_share_it() {
        use std::sync::Arc;
        let d = scratch("threads");
        let path = write(&d, "bank.sf2", synthetic_sf2(&[("A", 0, 0), ("B", 0, 1)]));
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    Sf2Sampler::open(&SamplerSource::Sf2(path, i % 2), 44_100).unwrap()
                })
            })
            .collect();
        let samplers: Vec<Sf2Sampler> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for s in &samplers {
            assert!(Arc::ptr_eq(samplers[0].font(), s.font()));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// With a real General MIDI bank in `$DJ_SOUNDFONTS`: every GM program the catalog
    /// names exists in it and loads. Skips without one.
    #[test]
    fn a_real_bank_loads_when_there_is_one() {
        let Some(dir) = std::env::var_os("DJ_SOUNDFONTS")
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
        else {
            eprintln!("skip: no $DJ_SOUNDFONTS");
            return;
        };
        let lib = Library::scan(&dir);
        let Some(bank) = lib.sound_fonts().into_iter().next() else {
            eprintln!("skip: no .sf2 in $DJ_SOUNDFONTS");
            return;
        };
        let mut sampler = Sf2Sampler::open(&SamplerSource::Sf2(bank, 0), 44_100).unwrap();
        assert!(sampler.normalize(true), "{:?}", sampler.last_check);
    }
}
