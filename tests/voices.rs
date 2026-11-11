//! Voice catalog, fallback ranking, sampler rules and drum kits (ports of
//! `VoiceCatalogTests`, `InstrumentPickTests`, `SampledKitTests` (pure parts) and the
//! sampler's load-time rules). No SoundFont or Logic library is needed: the installed
//! content is built here. Golden: none (the literals are the Swift tests' own).

use std::collections::BTreeMap;

use djrust::gen::BeatsTables;
use djrust::voices::bank::{dls_presets, sf2_presets, BankPreset};
use djrust::voices::drumkits::{self, DrumKit};
use djrust::voices::sampler as rules;
use djrust::voices::{
    general_midi_name, matches, scan, sound_fonts_in, stable_hash, swift_double, CatalogSound,
    SamplerSource, VoiceCatalog, SYNTH_ONLY,
};

fn catalog() -> VoiceCatalog {
    VoiceCatalog::bundled()
}

/// Every Swift `rulesSortInstrumentsByName` argument.
#[test]
fn rules_sort_instruments_by_name() {
    let c = catalog();
    for (path, category) in [
        (
            "Logic/02 Bass/02 Electric Bass/Fingerstyle Electric Bass.exs",
            "bass.electric",
        ),
        (
            "Logic/02 Bass/01 Acoustic Bass/Upright Jazz Bass.exs",
            "bass.acoustic",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/80s Pluck Bass.exs",
            "bass.synth",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Alphabet City 808 Bass.exs",
            "bass.sub",
        ),
        ("Logic/08 Guitars/Vintage Strat.exs", "guitar.electric"),
        ("Logic/08 Guitars/Acoustic Guitar.exs", "guitar.acoustic"),
        (
            "Logic/04 Keyboards/Producer Packs/Shimmering Electric Piano.exs",
            "keys.epiano",
        ),
        (
            "Logic/01 Acoustic Pianos/Steinway Grand Piano 2.exs",
            "keys.piano",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Heavenly Voices Choir.exs",
            "vocal",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Velvetine Synth Pad.exs",
            "pad",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Azure Crystal Synth Bells.exs",
            "synth.bell",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Lime Drops Synth Pluck.exs",
            "synth.pluck",
        ),
        (
            "Logic/04 Keyboards/Producer Packs/Supreme Lead Synth.exs",
            "synth.lead",
        ),
        (
            "Logic/09 Orchestral/01 Woodwinds/Alto Sax.exs",
            "woodwind.reed",
        ),
        ("Logic/09 Orchestral/02 Brass/Floating Flugels.exs", "brass"),
        ("Logic/03 Drums & Percussion/Something/Kit.exs", "skip"),
        ("GarageBand/Live Rock Kit.exs", "skip"),
        ("GarageBand/Hip Hop Drum Machine.exs", "skip"),
        ("Logic/09 Orchestral/07 Pipe Organ/Bass 1.exs", "keys.organ"),
        ("GarageBand/Double Bass.exs", "bass.acoustic"),
        (
            "Logic/09 Orchestral/01 Woodwinds/Bassoon Solo.exs",
            "woodwind.reed",
        ),
        ("GarageBand/Palm Muted Bass.exs", "bass.electric"),
        (
            "Logic/04 Keyboards/Producer Packs/Sweet Overdrive Kalimba.exs",
            "mallet",
        ),
        ("Logic/08 Guitars/Warm Electric.exs", "guitar.electric"),
        ("GarageBand/Classical Grand.exs", "keys.piano"),
        ("GarageBand/Japanese Traditional/Koto.exs", "ethnic"),
        ("Logic/04 Keyboards/Toy Celesta.exs", "mallet"),
        (
            "Logic/04 Keyboards/Producer Packs/Dreamy Smooth Vox.exs",
            "vocal",
        ),
    ] {
        assert_eq!(
            VoiceCatalog::categorize(path, &c.rules).as_deref(),
            Some(category),
            "{path}"
        );
    }
}

#[test]
fn every_tonal_beats_instrument_has_a_category() {
    let c = catalog();
    let t = BeatsTables::shared();
    let mut names = std::collections::BTreeSet::new();
    for set in t.instrument_sets.values() {
        for pool in set.values() {
            names.extend(pool.iter().cloned());
        }
    }
    assert!(names.len() > 40);
    for n in &names {
        if n.starts_with("drums") || SYNTH_ONLY.contains(&n.as_str()) {
            continue;
        }
        let cat = c.category_of.get(n);
        assert!(cat.is_some(), "{n} has no category");
        let cat = cat.unwrap();
        assert!(
            !c.pool(cat).is_empty() || c.gm.contains_key(cat) || c.keep_synth.contains(cat),
            "{n} → {cat} can play nothing"
        );
    }
    for n in SYNTH_ONLY {
        assert!(
            !c.category_of.contains_key(n),
            "{n} is synth-only: no sample category"
        );
    }
}

#[test]
fn articulations_and_copies_count_once() {
    assert_eq!(VoiceCatalog::canonical("Trumpets+").0, "Trumpets");
    assert_eq!(VoiceCatalog::canonical("Trumpets Staccato").0, "Trumpets");
    assert_eq!(VoiceCatalog::canonical("Flute iOS KB").0, "Flute");
    assert!(VoiceCatalog::canonical("Trumpets+").1 < VoiceCatalog::canonical("Trumpets Legato").1);
    assert_eq!(VoiceCatalog::canonical("Flute iOS KB").1, 3);
    assert_eq!(VoiceCatalog::canonical("Strings iOS").1, 3);
}

/// `VoiceCatalog.scan` on a tiny library: Logic's copy first, articulations folded into one
/// sound, the combined patch preferred, `skip` and non-EXS paths left out, names sorted.
#[test]
fn the_scan_folds_copies_into_ranked_sounds() {
    let c = catalog();
    let logic = "/Library/Application Support/Logic/Sampler Instruments/".to_string();
    let gb =
        "/Library/Application Support/GarageBand/Instrument Library/Sampler/Sampler Instruments/"
            .to_string();
    let roots = vec![
        // given out of order on purpose: Logic sorts first
        (
            gb.clone(),
            vec![
                "Trumpets.exs".to_string(),
                "Trumpets iOS.exs".to_string(),
                "Live Rock Kit.exs".to_string(),
                "readme.txt".to_string(),
            ],
        ),
        (
            logic.clone(),
            vec![
                "09 Orchestral/02 Brass/Trumpets+.exs".to_string(),
                "09 Orchestral/02 Brass/Trumpets Legato.exs".to_string(),
                "09 Orchestral/02 Brass/Floating Flugels.exs".to_string(),
            ],
        ),
    ];
    let sounds = scan(&roots, &c.rules);
    assert_eq!(sounds.keys().collect::<Vec<_>>(), vec!["brass"]);
    let brass = &sounds["brass"];
    assert_eq!(
        brass.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["Floating Flugels", "Trumpets"]
    );
    let t = &brass[1];
    // rank*10 + root index: Trumpets+ (0, Logic), Trumpets (10 + GB), Legato (20, Logic), iOS (30)
    let names: Vec<String> = t.sources.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        names,
        vec![
            "exs:Trumpets+.exs",
            "exs:Trumpets.exs",
            "exs:Trumpets Legato.exs",
            "exs:Trumpets iOS.exs"
        ]
    );
    assert_eq!(
        t.sources[0],
        SamplerSource::Exs(format!("{logic}09 Orchestral/02 Brass/Trumpets+.exs"))
    );
}

fn fake_pool(c: &mut VoiceCatalog, category: &str, names: &[&str]) {
    let v = names
        .iter()
        .map(|n| CatalogSound {
            name: n.to_string(),
            category: category.to_string(),
            sources: vec![SamplerSource::Exs(format!("/lib/{n}.exs"))],
        })
        .collect();
    c.sounds.insert(category.to_string(), v);
}

#[test]
fn picks_are_stable_and_spread_across_the_pool() {
    let mut c = catalog();
    fake_pool(&mut c, "bass.electric", &["A", "B", "C", "D", "E"]);
    let a = c.sources("bass", "bass", 42.0, false);
    assert_eq!(a, c.sources("bass", "bass", 42.0, false));
    let pool = c.pool("bass.electric");
    let mut firsts = std::collections::BTreeSet::new();
    for seed in 0..60 {
        if let Some(s) = c.sources("bass", "bass", seed as f64, false) {
            firsts.insert(s[0].clone());
        }
    }
    assert!(firsts.len() >= pool.len().min(3), "{}", firsts.len());
}

#[test]
fn a_pick_falls_back_through_its_pool_then_gm() {
    let mut c = catalog();
    fake_pool(&mut c, "keys.piano", &["Grand", "Upright", "Honky"]);
    let s = c.sources("piano", "melody", 1.0, false).unwrap();
    let pool: Vec<SamplerSource> = c
        .pool("keys.piano")
        .into_iter()
        .flat_map(|p| p.sources)
        .collect();
    let mut head: Vec<_> = s[..pool.len()].to_vec();
    let mut want = pool.clone();
    head.sort();
    want.sort();
    assert_eq!(head, want, "the whole pool, rotated");
    assert_eq!(
        &s[s.len() - 2..],
        &[SamplerSource::Gm(0), SamplerSource::Gm(1)]
    );

    // with SoundFonts installed their programs come before Apple's GM
    c.sound_fonts = vec!["/sf/Other.sf2".into(), "/sf/MuseScore_General.sf2".into()];
    let s = c.sources("piano", "melody", 1.0, false).unwrap();
    let tail: Vec<String> = s[pool.len()..].iter().map(|x| x.to_string()).collect();
    assert_eq!(
        tail,
        vec![
            "sf2:Other.sf2#0",
            "sf2:Other.sf2#1",
            "sf2:MuseScore_General.sf2#0",
            "sf2:MuseScore_General.sf2#1",
            "gm:0",
            "gm:1"
        ]
    );
}

/// With nothing installed, a plain category is just its GM programs; an instrument with no
/// category has nothing.
#[test]
fn an_empty_library_ranks_gm_only() {
    let c = catalog();
    assert_eq!(
        c.sources("piano", "melody", 7.0, false),
        Some(vec![SamplerSource::Gm(0), SamplerSource::Gm(1)])
    );
    assert_eq!(c.sources("laser", "melody", 7.0, false), None);
    assert_eq!(c.sources("not-an-instrument", "melody", 7.0, false), None);
    // a keepSynth category with no sounds and no SoundFont has nothing at all: its synth voice
    assert_eq!(c.sources("supersaw", "melody", 3.0, false), None);
}

#[test]
fn synth_categories_sometimes_keep_the_synth_voice() {
    let mut c = catalog();
    fake_pool(&mut c, "synth.lead", &["Lead One", "Lead Two"]);
    let synth = (0..80)
        .filter(|s| c.sources("supersaw", "melody", *s as f64, false).is_none())
        .count();
    assert!(synth > 0 && synth < 80, "{synth}");
}

#[test]
fn synth_categories_fall_back_to_their_synth_not_gm() {
    let mut c = catalog();
    for s in 0..20 {
        for x in c
            .sources("sitar", "melody", s as f64, false)
            .unwrap_or_default()
        {
            assert!(!matches!(x, SamplerSource::Gm(_)), "{x}");
        }
    }
    c.sound_fonts = vec!["/sf/MuseScore_General.sf2".into()];
    let s = c.sources("sitar", "melody", 3.0, false).unwrap();
    assert!(s.iter().all(|x| matches!(x, SamplerSource::Sf2(..))));
}

#[test]
fn a_picked_instrument_chooses_a_sound_by_its_name() {
    let mut c = catalog();
    fake_pool(
        &mut c,
        "synth.lead",
        &[
            "Wobble Bass Lead",
            "Bitrate Crush",
            "Tape Lead",
            "Plain One",
            "Plain Two",
            "Plain Three",
            "Hoover Mk2",
        ],
    );
    // unpicked: every lead name plays the category's one pick
    let a = c.sources("sync-lead", "melody", 7.0, false);
    let b = c.sources("hoover", "melody", 7.0, false);
    assert_eq!(a.as_ref().map(|v| &v[0]), b.as_ref().map(|v| &v[0]));
    // picked: a sound whose name fits comes first
    for (name, fit) in [
        ("wobble-lead", "Wobble Bass Lead"),
        ("chiptune", "Bitrate Crush"),
        ("tape-lead", "Tape Lead"),
    ] {
        let s = c.sources(name, "melody", 7.0, true).unwrap();
        assert_eq!(
            s[0],
            SamplerSource::Exs(format!("/lib/{fit}.exs")),
            "{name}"
        );
    }
    // and different picked names reach different sounds
    let names = [
        "sync-lead",
        "hoover",
        "supersaw",
        "lead",
        "duo-lead",
        "big-saw",
        "trance-lead",
    ];
    let firsts: std::collections::BTreeSet<String> = names
        .iter()
        .filter_map(|n| c.sources(n, "melody", 7.0, true).map(|s| s[0].to_string()))
        .collect();
    assert!(firsts.len() >= 4, "{firsts:?}");
}

#[test]
fn name_matching_ignores_vague_words() {
    assert!(matches("Wobble Lead", "wobble-lead"));
    assert!(
        !matches("Big Lead", "lead"),
        "every word of `lead` is vague"
    );
    assert!(matches("Music Box 2", "music-box"));
    assert!(matches("Rhodes Mk1", "electric-piano"));
    assert!(matches("Chip Tune", "chiptune"));
    assert!(!matches("Piano", "sync-lead"));
}

#[test]
fn stable_hash_is_fixed() {
    assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
}

/// The pick keys interpolate a Swift `Double`.
#[test]
fn swift_doubles_print_as_swift_prints_them() {
    assert_eq!(swift_double(42.0), "42.0");
    assert_eq!(swift_double(1984.0), "1984.0");
    assert_eq!(swift_double(0.5), "0.5");
    assert_eq!(swift_double(-3.0), "-3.0");
    assert_eq!(swift_double(0.0), "0.0");
    assert_eq!(swift_double(123456789.0), "123456789.0");
    assert_eq!(swift_double(1e16), "1e+16");
    assert_eq!(swift_double(1.5e-7), "1.5e-07");
}

/// `Deck.fallbackOptions` (InstrumentPickTests).
#[test]
fn a_role_beats_lists_nothing_for_still_has_instruments() {
    let c = catalog();
    let harmony = c.fallback_options("harmony");
    let first = harmony.first().map(String::as_str);
    assert!(
        matches!(first, Some("am-pad") | Some("dark-pad") | Some("pad")),
        "{first:?}"
    );
    assert!(harmony.contains(&"strings".to_string()) && harmony.contains(&"choir".to_string()));
    assert!(!harmony.contains(&"acid".to_string()));
    assert!(
        !c.fallback_options("lead").contains(&"sub-bass".to_string()),
        "no basses for a lead"
    );
    assert!(!c.fallback_options("lead").is_empty());
}

#[test]
fn sound_fonts_come_by_name_with_the_floor_last() {
    let names: Vec<String> = [
        "MuseScore_General.sf2",
        "Zed.SF2",
        "notes.txt",
        "minecraft-local.sf2",
        "Alpha.sf2",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        sound_fonts_in("/sf", &names),
        vec!["/sf/Alpha.sf2", "/sf/Zed.SF2", "/sf/MuseScore_General.sf2"]
    );
    assert_eq!(sound_fonts_in("/sf/", &names)[0], "/sf/Alpha.sf2");
    assert!(sound_fonts_in("/sf", &[]).is_empty());
}

#[test]
fn source_descriptions_match_swift() {
    assert_eq!(SamplerSource::Gm(5).to_string(), "gm:5");
    assert_eq!(
        SamplerSource::Sf2("/a/b/X.sf2".into(), 3).to_string(),
        "sf2:X.sf2#3"
    );
    assert_eq!(
        SamplerSource::Kit {
            path: "/a/X.sf2".into(),
            bank: 128,
            program: 0
        }
        .to_string(),
        "kit:X.sf2#128:0"
    );
    assert_eq!(
        SamplerSource::exs_named("Logic/08 Guitars/Vintage Strat.exs"),
        SamplerSource::Exs(
            "/Library/Application Support/Logic/Sampler Instruments/08 Guitars/Vintage Strat.exs"
                .into()
        )
    );
    assert_eq!(
        SamplerSource::exs_named("/abs/x.exs"),
        SamplerSource::Exs("/abs/x.exs".into())
    );
    assert!(SamplerSource::Sf2("/x/minecraft-local.sf2".into(), 0).is_local_only());
    assert!(!SamplerSource::Sf2("/x/MuseScore_General.sf2".into(), 0).is_local_only());
}

#[test]
fn general_midi_names() {
    assert_eq!(general_midi_name(0), "Grand Piano");
    assert_eq!(general_midi_name(32), "Acoustic Bass");
    assert_eq!(general_midi_name(127), "Gun Shot");
    assert_eq!(general_midi_name(200), "GM 200");
}

// MARK: sampler rules

#[test]
fn the_sampler_rules_are_swifts() {
    assert_eq!(rules::TARGET_RMS_DB, -27.5);
    assert_eq!(rules::MAX_DETUNE_CENTS, 30.0);
    assert_eq!(rules::SILENT_RMS_DB, -70.0);
    assert_eq!(rules::KIT_TARGET_RMS_DB, -15.0);
    assert_eq!(rules::REFERENCE_PHRASE.len(), 7);
    assert_eq!(rules::REFERENCE_PHRASE[0].note, 48);
    // a bar of 16 hats + 4 kicks + 2 snares
    let kp = rules::kit_phrase();
    assert_eq!(kp.len(), 22);
    assert_eq!(kp.iter().filter(|n| n.note == 36).count(), 4);
    assert_eq!(kp.iter().filter(|n| n.note == 38).count(), 2);
    assert_eq!(kp[0].note, 42);
    assert_eq!(kp[1].note, 36);
    // gain lands on the target and is limited to 0.02…20
    assert!((rules::gain_for(-27.5, -27.5) - 1.0).abs() < 1e-12);
    assert!((rules::gain_for(-37.5, -27.5) - 10f64.powf(0.5)).abs() < 1e-12);
    assert_eq!(rules::gain_for(-200.0, -27.5), 20.0);
    assert_eq!(rules::gain_for(0.0, -80.0), 0.02);
    // turned away: silent, undetectable, or out of tune
    assert!(rules::tonal_fit(-30.0, true, 1.0, 29.0));
    assert!(!rules::tonal_fit(-30.0, true, 1.0, 31.0));
    assert!(!rules::tonal_fit(-30.0, true, 1.0, -31.0));
    assert!(
        !rules::tonal_fit(-30.0, true, 0.0, 0.0),
        "the held note died"
    );
    assert!(!rules::tonal_fit(-71.0, true, 1.0, 0.0));
    assert!(
        rules::tonal_fit(-30.0, false, 0.0, 99.0),
        "a local bank skips the tuning check"
    );
    assert!(!rules::tonal_fit(-71.0, false, 1.0, 0.0));
    assert!(rules::kit_fit(-20.0, -40.0));
    assert!(!rules::kit_fit(-20.0, -80.0));
    // Swift starts the running minimum at 0
    assert_eq!(rules::quietest_of([-10.0, -40.0]), -40.0);
    assert_eq!(rules::quietest_of([]), 0.0);
    assert!((rules::velocity_scale(-6.0) - 10f64.powf(-0.15)).abs() < 1e-12);
    assert_eq!(rules::rms_db(0.0, 100), -200.0);
}

#[test]
fn pitch_class_error_finds_a_detuned_partial_in_any_octave() {
    let sr = 24_000.0;
    // A3 (note 57) = 220 Hz; play it 20 cents sharp, two octaves up
    let hz = 220.0 * 4.0 * 2f64.powf(20.0 / 1200.0);
    let x: Vec<f32> = (0..(0.3 * sr) as usize)
        .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / sr).sin() as f32 * 0.5)
        .collect();
    let (cents, oct, level) = djrust::voices::pitch::pitch_class_error(&x, 57, sr);
    assert!((cents - 20.0).abs() <= 1.0, "{cents}");
    assert_eq!(oct, 2);
    assert!(level > 0.0);
    // silence: nothing sounded
    let (_, _, level) = djrust::voices::pitch::pitch_class_error(&vec![0.0; 4000], 57, sr);
    assert_eq!(level, 0.0);
    assert_eq!(
        djrust::voices::pitch::pitch_class_error(&[0.0; 10], 57, sr),
        (0.0, 0, 0.0)
    );
}

// MARK: banks and kits

/// A SoundFont with only the RIFF/pdta/phdr structure: enough for a preset list.
pub fn sf2_with_presets(presets: &[(&str, u16, u16)]) -> Vec<u8> {
    let mut phdr = Vec::new();
    for (name, program, bank) in presets
        .iter()
        .copied()
        .chain(std::iter::once(("EOP", 0, 0)))
    {
        let mut n = [0u8; 20];
        n[..name.len()].copy_from_slice(name.as_bytes());
        phdr.extend_from_slice(&n);
        phdr.extend_from_slice(&program.to_le_bytes());
        phdr.extend_from_slice(&bank.to_le_bytes());
        phdr.extend_from_slice(&[0u8; 14]);
    }
    let mut pdta = b"pdta".to_vec();
    pdta.extend_from_slice(b"phdr");
    pdta.extend_from_slice(&(phdr.len() as u32).to_le_bytes());
    pdta.extend_from_slice(&phdr);
    let info = {
        let mut v = b"INFO".to_vec();
        v.extend_from_slice(b"ifil");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&[2, 0, 4, 0]);
        v
    };
    let mut body = b"sfbk".to_vec();
    body.extend_from_slice(b"LIST");
    body.extend_from_slice(&(info.len() as u32).to_le_bytes());
    body.extend_from_slice(&info);
    body.extend_from_slice(b"LIST");
    body.extend_from_slice(&(pdta.len() as u32).to_le_bytes());
    body.extend_from_slice(&pdta);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn an_sf2_preset_list_reads_without_samples() {
    let bytes = sf2_with_presets(&[
        ("Standard", 0, 128),
        ("Room", 8, 128),
        ("Grand Piano", 0, 0),
        ("Minecraft Kit", 127, 0),
        ("Brush", 40, 128),
    ]);
    let p = sf2_presets(&bytes);
    assert_eq!(p.len(), 5);
    assert_eq!(
        p[0],
        BankPreset {
            name: "Standard".into(),
            bank: 128,
            program: 0
        }
    );
    assert_eq!(p[3].name, "Minecraft Kit");
    assert!(sf2_presets(&b"RIFF\0\0\0\0WAVE"[..]).is_empty());
    assert!(sf2_presets(&[1u8, 2, 3][..]).is_empty());
}

#[test]
fn drum_kit_ids_parse() {
    assert_eq!(
        drumkits::parse("MuseScore_General.sf2#128:0"),
        Some(("MuseScore_General.sf2".into(), 128, 0))
    );
    assert_eq!(
        drumkits::parse("minecraft-local.sf2#0:127"),
        Some(("minecraft-local.sf2".into(), 0, 127))
    );
    for bad in [
        "",
        "x.sf2",
        "x.sf2#",
        "x.sf2#1",
        "x.sf2#a:b",
        "../x.sf2#128:0",
        "x.sf2#129:0",
        "x.sf2#1:128",
    ] {
        assert_eq!(drumkits::parse(bad), None, "{bad}");
    }
    assert_eq!(
        drumkits::source_for("nowhere.sf2#128:0", "/sf", &[], false),
        None
    );
    let files = vec!["K.sf2".to_string()];
    assert_eq!(
        drumkits::source_for("K.sf2#128:3", "/sf", &files, false),
        Some(SamplerSource::Kit {
            path: "/sf/K.sf2".into(),
            bank: 128,
            program: 3
        })
    );
    assert_eq!(
        drumkits::source_for("gs_instruments.dls#128:0", "/sf", &files, false),
        None
    );
    assert!(matches!(
        drumkits::source_for("gs_instruments.dls#128:0", "/sf", &files, true),
        Some(SamplerSource::Kit { .. })
    ));
}

#[test]
fn drum_lanes_play_their_gm_keys() {
    let keys: Vec<i64> = ["kick", "snare", "clap", "hat", "openhat", "crash"]
        .iter()
        .map(|k| drumkits::gm_note(k))
        .collect();
    assert_eq!(keys, vec![36, 38, 39, 42, 46, 49]);
    assert_eq!(drumkits::gm_note("anything"), 36);
    assert_eq!(drumkits::hold_ms("crash"), 2500.0);
    assert_eq!(drumkits::hold_ms("openhat"), 600.0);
    assert_eq!(drumkits::hold_ms("kick"), 400.0);
}

#[test]
fn kits_are_every_percussion_preset_and_every_named_kit() {
    let presets = sf2_presets(&sf2_with_presets(&[
        ("Grand Piano", 0, 0),
        ("Room", 8, 128),
        ("Standard", 0, 128),
        ("Minecraft Kit", 127, 0),
        ("Skit Comedy", 5, 0),
        ("Drum Kit Lite", 6, 0),
    ]));
    let kits = drumkits::scan(&[("/sf/MuseScore_General.sf2".to_string(), presets)]);
    let ids: Vec<&str> = kits.iter().map(|k| k.id.as_str()).collect();
    // by (bank, program): the melodic kits (bank 0) come before the percussion bank (128)
    assert_eq!(
        ids,
        vec![
            "MuseScore_General.sf2#0:6",
            "MuseScore_General.sf2#0:127",
            "MuseScore_General.sf2#128:0",
            "MuseScore_General.sf2#128:8"
        ]
    );
    assert_eq!(kits[2].label(), "Standard · MuseScore_General");
    assert_eq!(
        kits[2].source,
        SamplerSource::Kit {
            path: "/sf/MuseScore_General.sf2".into(),
            bank: 128,
            program: 0
        }
    );
    assert!(drumkits::is_kit_name("Minecraft Kit"));
    assert!(drumkits::is_kit_name("Rock Drum Kit 2"));
    assert!(!drumkits::is_kit_name("Skit"));
}

#[test]
fn the_brush_kit_is_gm2s_program_40_or_the_first_brush_named() {
    let p = |n: &str, b, pr| BankPreset {
        name: n.into(),
        bank: b,
        program: pr,
    };
    let a = vec![
        p("Standard", 128, 0),
        p("Brush", 128, 40),
        p("Jazz", 128, 32),
    ];
    assert_eq!(drumkits::brush_preset(&a).unwrap().program, 40);
    let b = vec![
        p("Standard", 128, 0),
        p("Wire Brushes", 128, 41),
        p("Brush 2", 128, 50),
    ];
    assert_eq!(drumkits::brush_preset(&b).unwrap().program, 41);
    assert!(drumkits::brush_preset(&[p("Standard", 128, 0)]).is_none());
    let kit = SamplerSource::Kit {
        path: "/sf/X.sf2".into(),
        bank: 128,
        program: 0,
    };
    assert_eq!(
        drumkits::brush_kit(&kit, &a),
        Some(SamplerSource::Kit {
            path: "/sf/X.sf2".into(),
            bank: 128,
            program: 40
        })
    );
    let melodic = SamplerSource::Kit {
        path: "/sf/X.sf2".into(),
        bank: 0,
        program: 127,
    };
    assert_eq!(drumkits::brush_kit(&melodic, &a), None);
}

/// A DLS bank built in memory: one melodic instrument, one drum instrument.
fn dls_with(instruments: &[(&str, u32, u32)]) -> Vec<u8> {
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }
    fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = kind.to_vec();
        b.extend_from_slice(body);
        chunk(b"LIST", &b)
    }
    let mut lins = Vec::new();
    for (name, bank, program) in instruments {
        let mut insh = Vec::new();
        insh.extend_from_slice(&1u32.to_le_bytes()); // regions
        insh.extend_from_slice(&bank.to_le_bytes());
        insh.extend_from_slice(&program.to_le_bytes());
        let mut nm = name.as_bytes().to_vec();
        nm.push(0);
        let info = list(b"INFO", &chunk(b"INAM", &nm));
        let mut ins = chunk(b"insh", &insh);
        ins.extend_from_slice(&info);
        lins.extend_from_slice(&list(b"ins ", &ins));
    }
    let mut body = b"DLS ".to_vec();
    body.extend_from_slice(&list(b"lins", &lins));
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn a_dls_bank_lists_its_instruments() {
    let data = dls_with(&[
        ("Piano", 0, 0),
        ("Standard Kit", 0x8000_0000, 0),
        ("Room Kit", 0x8000_0000, 8),
    ]);
    let p = dls_presets(&data);
    assert_eq!(
        p,
        vec![
            BankPreset {
                name: "Piano".into(),
                bank: 0,
                program: 0
            },
            BankPreset {
                name: "Standard Kit".into(),
                bank: 128,
                program: 0
            },
            BankPreset {
                name: "Room Kit".into(),
                bank: 128,
                program: 8
            },
        ]
    );
    assert!(dls_presets(b"RIFF\0\0\0\0WAVE").is_empty());
}

#[test]
fn kit_labels() {
    let k = DrumKit {
        id: "a.sf2#128:0".into(),
        name: "Standard".into(),
        bank: "a".into(),
        source: SamplerSource::Gm(0),
    };
    assert_eq!(k.label(), "Standard · a");
    let _ = BTreeMap::<String, String>::new();
}
