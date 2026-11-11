//! Palettes, sound resolution and stinger curation (ports of `PaletteDataTests`,
//! `PaletteTests` (the pure parts) and `StingerCurationTests`). The installed content is
//! described by an `Installed` value built here, so nothing depends on this machine.
//! Golden: none (the literals are the Swift tests' own).

use std::collections::BTreeSet;

use djrust::gen::{compose_with, BeatsTables};
use djrust::json::JsonValue;
use djrust::sound::stinger::{
    self, compute, is_macro, resolve, AssignmentKind, CandidateKind, Key, LoopSource,
    StingerContent, StockLoop, Table,
};
use djrust::sound::*;
use djrust::voices::{CatalogSound, SamplerSource, VoiceCatalog};

fn minecraft() -> MinecraftSidecar {
    MinecraftSidecar {
        asset_index: "32".into(),
        sha1: Default::default(),
        programs: djrust::sound::minecraft::programs()
            .into_iter()
            .map(|(n, _)| n)
            .collect(),
        format: 2,
    }
}

/// A machine with the floor SoundFont, one more bank with kits, Apple's GM bank and the
/// Minecraft bank.
fn full() -> Installed {
    Installed {
        dir: "/sf".into(),
        files: vec![
            "MuseScore_General.sf2".into(),
            "minecraft-local.sf2".into(),
            "Kits.sf2".into(),
        ],
        gm_bank: true,
        kits: vec![],
        minecraft: Some(minecraft()),
        minecraft_assets_missing: None,
    }
}

fn empty() -> Installed {
    Installed::nothing("/empty")
}

fn catalog_with_sf(inst: &Installed) -> VoiceCatalog {
    let mut c = VoiceCatalog::bundled();
    c.sound_fonts = inst.sound_fonts();
    c
}

#[test]
fn the_shipped_palettes_are_well_formed() {
    let set = PaletteSet::shared();
    let all = &set.palettes;
    assert!(
        (8..=13).contains(&all.len()),
        "{:?}",
        all.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    let ids: BTreeSet<_> = all.iter().map(|p| &p.id).collect();
    assert_eq!(ids.len(), all.len(), "unique ids");
    assert_eq!(all[0].id, set.default_id);
    assert_eq!(set.default_id, "studio");
    assert!(
        set.named(Some("studio")).unwrap().roles.is_empty(),
        "the default plays every instrument's own sound"
    );
    let inst = empty();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    for p in all {
        assert!(
            !p.label.is_empty() && !p.icon.is_empty() && p.description.chars().count() > 20,
            "{}",
            p.id
        );
        assert!(p.color.starts_with('#') && p.color.len() == 7, "{}", p.id);
        assert!(!p.neighbours.is_empty(), "{} has somewhere to hop", p.id);
        for n in &p.neighbours {
            assert!(set.named(Some(n)).is_some() && *n != p.id, "{} → {n}", p.id);
        }
        for (role, opts) in &p.roles {
            assert!(
                ROLE_ORDER.contains(&role.as_str()) || role == "*" || role == "pad",
                "{}: {role}",
                p.id
            );
            for o in opts {
                let why = unavailable_reason_option(set, o, role, ctx).unwrap_or_default();
                for bad in [
                    "unknown option",
                    "not a kit",
                    "no instrument",
                    "no synth kit",
                    "not a",
                ] {
                    assert!(!why.starts_with(bad), "{} {role}: {o}: {why}", p.id);
                }
            }
        }
    }
    assert_eq!(set.named(Some("jazz@3")).unwrap().id, "jazz");
    assert_eq!(set.playing(None).unwrap().id, "studio");
}

/// With nothing installed a sampled palette says so, and a synth one still plays.
#[test]
fn an_empty_machine_plays_synth_palettes_only() {
    let set = PaletteSet::shared();
    let inst = empty();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let jazz = set.named(Some("jazz")).unwrap();
    assert!(jazz
        .unavailable_reason(set, ctx)
        .unwrap()
        .contains("not installed"));
    assert!(set.named(Some("synthwave")).unwrap().is_playable(set, ctx));
    assert!(set.named(Some("synth")).unwrap().is_playable(set, ctx));
    assert!(!set.named(Some("minecraft")).unwrap().is_playable(set, ctx));
}

/// Every palette plays on a full machine, and each role resolves for any seed to an option
/// that is installed.
#[test]
fn a_full_machine_plays_every_palette() {
    let set = PaletteSet::shared();
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    for p in &set.palettes {
        assert_eq!(p.unavailable_reason(set, ctx), None, "{}", p.id);
        for role in ROLE_ORDER {
            if p.pool(role).is_none() {
                continue;
            }
            assert!(!p.available(role, set, ctx).is_empty(), "{} {role}", p.id);
            for seed in [1.0, 11.0, 42.0, 777.0] {
                let o = p
                    .pick(role, role, seed, set, ctx)
                    .unwrap_or_else(|| panic!("{} {role}", p.id));
                assert_eq!(unavailable_reason_option(set, &o, role, ctx), None);
            }
        }
    }
}

#[test]
fn picks_are_deterministic_from_seed_and_role() {
    let set = PaletteSet::shared();
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let p = set.named(Some("synthwave")).unwrap();
    let mut melodies = BTreeSet::new();
    for seed in 1..=40 {
        let a = p.pick("melody", "melody", seed as f64, set, ctx);
        assert_eq!(a, p.pick("melody", "melody", seed as f64, set, ctx));
        melodies.insert(a.unwrap_or_default());
    }
    assert!(
        melodies.len() > 2,
        "seeds pick different leads: {melodies:?}"
    );
    // a pick that none of the pool can satisfy is None, not a panic
    let p = set.named(Some("studio")).unwrap();
    assert_eq!(p.pick("melody", "melody", 1.0, set, ctx), None);
}

#[test]
fn studio_is_no_palette_at_all() {
    let set = PaletteSet::shared();
    let inst = full();
    assert_eq!(set.normalized(Some("studio"), &inst), None);
    assert_eq!(set.normalized(None, &inst), None);
    assert_eq!(set.normalized(Some(""), &inst), None);
    assert_eq!(set.normalized(Some("jazz"), &inst).as_deref(), Some("jazz"));
    // the Minecraft palette records the asset version it was played with
    assert_eq!(
        set.normalized(Some("minecraft"), &inst).as_deref(),
        Some("minecraft@32+b2")
    );
    assert_eq!(
        set.normalized(Some("minecraft@32+b2"), &inst).as_deref(),
        Some("minecraft@32+b2")
    );
    assert_eq!(
        set.normalized(Some("nonesuch"), &inst).as_deref(),
        Some("nonesuch")
    );
    assert_eq!(version(Some("minecraft@32")).as_deref(), Some("32"));
    assert_eq!(version(Some("minecraft")), None);
    assert_eq!(version(Some("minecraft@")), None);
    let empty = empty();
    assert_eq!(
        set.normalized(Some("minecraft"), &empty).as_deref(),
        Some("minecraft")
    );
}

/// `Project.applySound` on a composed house track under the synthwave palette.
#[test]
fn a_palette_resolves_every_net_and_a_synth_option_voices_that_instrument() {
    let set = PaletteSet::shared();
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let project = compose_with(
        "house",
        &[
            ("seed", JsonValue::int(11)),
            ("structure", JsonValue::str("standard")),
        ],
    );
    let mut nets = SoundNet::from_project(&project, Some("synthwave"));
    assert!(nets.len() > 5);
    apply_sound(&mut nets, 11.0, set, ctx);
    let again = {
        let mut n = SoundNet::from_project(&project, Some("synthwave"));
        apply_sound(&mut n, 11.0, set, ctx);
        n
    };
    assert_eq!(
        nets, again,
        "a pure function of the nets, the seed and what's installed"
    );
    // idempotent: running it again changes nothing
    let mut twice = nets.clone();
    apply_sound(&mut twice, 11.0, set, ctx);
    assert_eq!(nets, twice);
    assert!(sound_roles(&nets).contains(&"kit".to_string()));
    assert!(sound_roles(&nets).contains(&"melody".to_string()));
    let melody = sound_net(&nets, "melody").unwrap();
    let opt = melody.sound.option.clone().expect("a palette pick");
    if let Some(name) = synth_name(&opt) {
        assert_eq!(name, melody.instrument, "{opt}");
        assert!(melody.sound.composed.is_some());
    }
    // the drums share one kit
    let kit_nets: Vec<_> = nets
        .iter()
        .filter(|n| !n.is_control && sound_role(n).0 == "kit")
        .collect();
    assert!(!kit_nets.is_empty());
    assert!(kit_nets
        .iter()
        .all(|n| n.sound.option == kit_nets[0].sound.option));
    assert!(channels_for_role(&nets, "kit")
        .iter()
        .all(|c| (10..=15).contains(c)));
    // another seed may sound different
    let mut other = SoundNet::from_project(&project, Some("synthwave"));
    let mut differs = false;
    for seed in 1..=12 {
        apply_sound(&mut other, seed as f64, set, ctx);
        if other != nets {
            differs = true;
        }
    }
    assert!(differs);
}

#[test]
fn the_users_choices_win_and_clearing_restores_the_composed_instrument() {
    let set = PaletteSet::shared();
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let project = compose_with(
        "synthwave",
        &[
            ("seed", JsonValue::int(5)),
            ("structure", JsonValue::str("minimal")),
        ],
    );
    let mut nets = SoundNet::from_project(&project, Some("synthwave"));
    apply_sound(&mut nets, 5.0, set, ctx);
    let bass_idx: Vec<usize> = (0..nets.len())
        .filter(|&i| !nets[i].is_control && sound_role(&nets[i]).0 == "bass")
        .collect();
    assert!(!bass_idx.is_empty());
    let composed = nets[bass_idx[0]]
        .sound
        .composed
        .clone()
        .unwrap_or_else(|| nets[bass_idx[0]].instrument.clone());
    // `inst:acid` plays as picked
    for &i in &bass_idx {
        nets[i].sound.override_ = Some("inst:acid".into());
    }
    apply_sound(&mut nets, 5.0, set, ctx);
    for &i in &bass_idx {
        assert_eq!(nets[i].instrument, "acid");
        assert!(nets[i].picked);
        assert_eq!(nets[i].sound.option.as_deref(), Some("inst:acid"));
        assert_eq!(nets[i].sound.composed.as_deref(), Some(composed.as_str()));
    }
    // reset: override cleared and un-picked, the composed instrument comes back under the palette
    for &i in &bass_idx {
        nets[i].sound.override_ = None;
        nets[i].picked = false;
    }
    apply_sound(&mut nets, 5.0, set, ctx);
    for &i in &bass_idx {
        let o = nets[i].sound.option.clone().unwrap();
        match synth_name(&o) {
            Some(n) => assert_eq!(nets[i].instrument, n),
            None => assert_eq!(nets[i].instrument, composed),
        }
    }
    // a kit override wins over the palette's pick on the drums
    let kit_idx: Vec<usize> = (0..nets.len())
        .filter(|&i| !nets[i].is_control && sound_role(&nets[i]).0 == "kit")
        .collect();
    for &i in &kit_idx {
        nets[i].sound.override_ = Some("synth:drums-808".into());
    }
    apply_sound(&mut nets, 5.0, set, ctx);
    for &i in &kit_idx {
        assert_eq!(nets[i].sound.option.as_deref(), Some("synth:drums-808"));
        assert_eq!(nets[i].sound.kit.as_deref(), Some("synth"));
        assert_eq!(nets[i].instrument, "drums-808");
    }
}

#[test]
fn options_resolve_to_sources_in_ranked_order() {
    let set = PaletteSet::shared();
    let inst = full();
    let mut c = catalog_with_sf(&inst);
    c.sounds.insert(
        "keys.piano".into(),
        vec![
            CatalogSound {
                name: "Grand".into(),
                category: "keys.piano".into(),
                sources: vec![SamplerSource::Exs("/lib/Grand.exs".into())],
            },
            CatalogSound {
                name: "Upright".into(),
                category: "keys.piano".into(),
                sources: vec![SamplerSource::Exs("/lib/Upright.exs".into())],
            },
        ],
    );
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    // gm: every SoundFont, floor last
    assert_eq!(
        sources_for_option(set, "gm:33", "bass", 1.0, ctx),
        Some(vec![
            SamplerSource::Sf2("/sf/Kits.sf2".into(), 33),
            SamplerSource::Sf2("/sf/MuseScore_General.sf2".into(), 33)
        ])
    );
    // dls: Apple's bank
    assert_eq!(
        sources_for_option(set, "dls:5", "bass", 1.0, ctx),
        Some(vec![SamplerSource::Gm(5)])
    );
    // cat: the seeded rotation of the pool, then the SoundFont programs
    let s = sources_for_option(set, "cat:keys.piano", "melody", 1.0, ctx).unwrap();
    assert_eq!(s.len(), 2 + 2 * 2);
    assert!(matches!(s[0], SamplerSource::Exs(_)) && matches!(s[1], SamplerSource::Exs(_)));
    assert_ne!(s[0], s[1]);
    assert_eq!(s[2], SamplerSource::Sf2("/sf/Kits.sf2".into(), 0));
    // synth options play no sampler
    assert_eq!(
        sources_for_option(set, "synth:acid", "bass", 1.0, ctx),
        None
    );
    assert_eq!(sources_for_option(set, "own", "bass", 1.0, ctx), None);
    // a local bank's programs by name, rotated by the seed
    let s = sources_for_option(set, "bank:minecraft.bass", "bass", 3.0, ctx).unwrap();
    assert_eq!(s.len(), 3);
    let mut progs: Vec<i64> = s
        .iter()
        .map(|x| match x {
            SamplerSource::Sf2(p, n) => {
                assert_eq!(p, "/sf/minecraft-local.sf2");
                *n
            }
            _ => panic!(),
        })
        .collect();
    progs.sort();
    assert_eq!(progs, vec![1, 2, 3]); // bassattack, didgeridoo, guitar
                                      // nothing installed: no sources
    let none = empty();
    let c2 = catalog_with_sf(&none);
    let ctx2 = Sounds {
        catalog: &c2,
        installed: &none,
    };
    assert_eq!(sources_for_option(set, "gm:33", "bass", 1.0, ctx2), None);
    assert_eq!(
        sources_for_option(set, "bank:minecraft.bass", "bass", 1.0, ctx2),
        None
    );
    // the palette-level entry point picks first
    let sw = set.named(Some("jazz")).unwrap();
    let _ = sw.sources("bass", 7.0, set, ctx);
}

#[test]
fn option_reasons_name_what_is_missing() {
    let set = PaletteSet::shared();
    let none = empty();
    let c = catalog_with_sf(&none);
    let ctx = Sounds {
        catalog: &c,
        installed: &none,
    };
    let why = |o: &str, r: &str| unavailable_reason_option(set, o, r, ctx);
    assert_eq!(why("own", "bass"), None);
    assert_eq!(why("synth", "kit"), None);
    assert_eq!(why("synth:acid", "bass"), None);
    assert_eq!(
        why("synth:zzz", "bass").as_deref(),
        Some("no instrument zzz")
    );
    assert_eq!(why("synth:drums-808", "kit"), None);
    assert_eq!(why("synth:zzz", "kit").as_deref(), Some("no synth kit zzz"));
    assert_eq!(why("gm:x", "bass").as_deref(), Some("not a program: gm:x"));
    assert_eq!(
        why("gm:33", "bass").as_deref(),
        Some("MuseScore_General.sf2 is not installed (make soundfonts)")
    );
    assert_eq!(
        why("dls:3", "bass").as_deref(),
        Some("Apple's gs_instruments.dls is not on this Mac")
    );
    assert!(why("cat:keys.piano", "melody")
        .unwrap()
        .starts_with("no keys.piano sound installed"));
    assert_eq!(why("zzz", "bass").as_deref(), Some("unknown option zzz"));
    assert_eq!(why("zzz", "kit").as_deref(), Some("not a kit: zzz"));
    assert_eq!(
        why("X.sf2#128:0", "kit").as_deref(),
        Some("X.sf2 is not installed")
    );
    assert_eq!(
        why("bank:minecraft.kit", "kit").as_deref(),
        Some("not imported yet: run `make minecraft` (djctl minecraft import)")
    );
    assert_eq!(
        why("bank:minecraft.bass", "kit").as_deref(),
        Some("bank:minecraft.bass has no kit")
    );
    assert_eq!(
        why("bank:minecraft.kit", "bass").as_deref(),
        Some("bank:minecraft.kit has no sounds")
    );
    // the game's assets are missing: that is the reason
    let mut m = empty();
    m.minecraft_assets_missing = Some("no Minecraft install".into());
    let c3 = catalog_with_sf(&m);
    let ctx3 = Sounds {
        catalog: &c3,
        installed: &m,
    };
    assert_eq!(
        unavailable_reason_option(set, "bank:minecraft.lead", "melody", ctx3).as_deref(),
        Some("no Minecraft install")
    );
}

#[test]
fn kits_resolve_to_ids() {
    let set = PaletteSet::shared();
    assert_eq!(set.kit_id("synth").as_deref(), Some("synth"));
    assert_eq!(set.kit_id("own").as_deref(), Some("synth"));
    assert_eq!(set.kit_id("synth:drums-808").as_deref(), Some("synth"));
    assert_eq!(
        set.kit_id("MuseScore_General.sf2#128:0").as_deref(),
        Some("MuseScore_General.sf2#128:0")
    );
    assert_eq!(
        set.kit_id("bank:minecraft.kit").as_deref(),
        Some("minecraft-local.sf2#0:127")
    );
    assert_eq!(
        set.kit_id("bank:minecraft.kit").unwrap(),
        djrust::sound::minecraft::kit_id()
    );
    assert_eq!(set.kit_id("cat:bass.electric"), None);
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    assert_eq!(
        set.kit_for(Some("minecraft"), ctx).as_deref(),
        Some("minecraft-local.sf2#0:127")
    );
    assert_eq!(set.kit_for(Some("studio"), ctx), None);
    assert_eq!(set.kit_for(None, ctx), None);
    let none = empty();
    let c2 = catalog_with_sf(&none);
    assert_eq!(
        set.kit_for(
            Some("minecraft"),
            Sounds {
                catalog: &c2,
                installed: &none
            }
        ),
        None
    );
}

#[test]
fn local_only_content_is_flagged() {
    let set = PaletteSet::shared();
    assert!(set.named(Some("minecraft")).unwrap().is_local_only(set));
    assert!(set.is_local_only_option("dls:3"));
    assert!(set.is_local_only_option("bank:minecraft.bass"));
    assert!(set.is_local_only_option("gs_instruments.dls#128:0"));
    assert!(set.is_local_only_option("minecraft-local.sf2#0:127"));
    assert!(!set.is_local_only_option("MuseScore_General.sf2#128:0"));
    assert!(!set.is_local_only_option("gm:3"));
    for id in ["jazz", "synthwave", "orchestral", "studio"] {
        assert!(
            !set.named(Some(id)).unwrap().is_local_only(set),
            "{id} is publishable"
        );
    }
    assert_eq!(
        set.named(Some("minecraft"))
            .unwrap()
            .banks(set)
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["minecraft-local.sf2".to_string()]
    );
}

#[test]
fn labels_name_the_sound() {
    let set = PaletteSet::shared();
    let mut inst = full();
    inst.kits = djrust::voices::drumkits::scan(&[(
        "/sf/MuseScore_General.sf2".into(),
        vec![djrust::voices::bank::BankPreset {
            name: "Standard".into(),
            bank: 128,
            program: 0,
        }],
    )]);
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let l = |o: &str, r: &str| option_label(set, o, r, 3.0, ctx);
    assert_eq!(l("own", "kit"), "synth kit");
    assert_eq!(l("own", "bass"), "own sound");
    assert_eq!(l("synth", "bass"), "synth voice");
    assert_eq!(l("synth:drums-808", "kit"), "808 synth kit");
    assert_eq!(l("synth:acid", "bass"), "acid");
    assert_eq!(l("inst:pluck", "bass"), "pluck");
    assert_eq!(l("gm:33", "bass"), "Fingered Bass");
    assert_eq!(l("dls:33", "bass"), "Fingered Bass (Apple)");
    assert_eq!(
        l("cat:bass.electric", "bass"),
        "Fingered Bass",
        "no sounds: the first GM program's name"
    );
    assert_eq!(
        l("MuseScore_General.sf2#128:0", "kit"),
        "Standard · MuseScore_General"
    );
    assert_eq!(l("Nowhere.sf2#128:0", "kit"), "Nowhere.sf2#128:0");
    assert!(
        ["bassattack", "didgeridoo", "guitar"].contains(&l("bank:minecraft.bass", "bass").as_str())
    );
    assert_eq!(general_name_of_first(&c, "bass.electric"), "Fingered Bass");
}

fn general_name_of_first(c: &VoiceCatalog, cat: &str) -> String {
    djrust::voices::general_midi_name(c.gm[cat][0])
}

#[test]
fn a_decks_sound_choices_carry_into_a_new_spec() {
    use djrust::json::JsonObject;
    let mine = JsonObject::from_pairs([(
        "4",
        JsonValue::Object(JsonObject::from_pairs([(
            "instrument",
            JsonValue::str("pluck"),
        )])),
    )]);
    let other = JsonObject::from_pairs([
        (
            "4",
            JsonValue::Object(JsonObject::from_pairs([
                ("palette", JsonValue::str("jazz")),
                ("instrument", JsonValue::str("ignored")),
                ("voice", JsonValue::str("gm:4")),
            ])),
        ),
        (
            "6",
            JsonValue::Object(JsonObject::from_pairs([(
                "kit",
                JsonValue::str("K.sf2#128:0"),
            )])),
        ),
        (
            "7",
            JsonValue::Object(JsonObject::from_pairs([(
                "instrument",
                JsonValue::str("x"),
            )])),
        ),
    ]);
    let out = carrying_sound(&mine, &other);
    let ch4 = out.get("4").unwrap();
    assert_eq!(
        ch4.get("instrument").unwrap().as_str(),
        Some("pluck"),
        "the spec's own override stays"
    );
    assert_eq!(ch4.get("palette").unwrap().as_str(), Some("jazz"));
    assert_eq!(ch4.get("voice").unwrap().as_str(), Some("gm:4"));
    assert!(ch4.get("ignored").is_none());
    assert_eq!(
        out.get("6").unwrap().get("kit").unwrap().as_str(),
        Some("K.sf2#128:0")
    );
    assert!(out.get("7").is_none(), "no sound fields, nothing carried");
    let ch = sound_channels("house");
    assert!(ch.windows(2).all(|w| w[0] < w[1]));
    assert!(ch.contains(&5) && ch.contains(&13));
    let g = &BeatsTables::shared().genres["house"];
    assert!(ch.contains(&g.melody_channel) && ch.contains(&g.bass_channel));
    assert!(sound_channels("nonesuch").contains(&4));
}

#[test]
fn the_chooser_cards_light_the_current_palette_and_grey_the_missing() {
    let set = PaletteSet::shared();
    let none = empty();
    let c = catalog_with_sf(&none);
    let ctx = Sounds {
        catalog: &c,
        installed: &none,
    };
    let cards = PaletteCard::cards(set, None, ctx);
    assert_eq!(cards.len(), set.palettes.len());
    assert_eq!(
        cards
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["studio"]
    );
    let jazz = cards.iter().find(|c| c.id == "jazz").unwrap();
    assert!(jazz.unavailable.is_some());
    assert!(
        cards
            .iter()
            .find(|c| c.id == "minecraft")
            .unwrap()
            .local_only
    );
    let lit = PaletteCard::cards(set, Some("jazz@2"), ctx);
    assert_eq!(
        lit.iter()
            .filter(|c| c.selected)
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["jazz"]
    );
}

// MARK: stingers

fn stock(name: &str, beats: i64, bpm: f64, category: &str) -> StockLoop {
    StockLoop {
        path: format!("/nowhere/{name}.caf"),
        name: name.into(),
        pack: "Test Pack".into(),
        category: category.into(),
        subcategory: "".into(),
        genre: "".into(),
        beats,
        seconds: beats as f64 * 60.0 / bpm,
        key: None,
    }
}

/// Every candidate parses and names something this app knows.
#[test]
fn every_stinger_candidate_parses_and_names_something_known() {
    let set = PaletteSet::shared();
    let c = VoiceCatalog::bundled();
    let table = Table::shared();
    let raw = JsonValue::parse(stinger::STINGERS_JSON).unwrap();
    assert_eq!(
        table.candidates.len(),
        raw.get("candidates").unwrap().as_array().unwrap().len(),
        "every entry parses"
    );
    assert!(table.candidates.len() >= 20);
    let ids: BTreeSet<_> = table.candidates.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), table.candidates.len(), "unique ids");
    let families: BTreeSet<String> = BeatsTables::shared()
        .genres
        .keys()
        .map(|g| stinger::family_of(g))
        .collect();
    for kind in CandidateKind::ALL {
        assert!(
            table.candidates.iter().any(|c| c.kind == kind),
            "the table has {} candidates",
            kind.name()
        );
    }
    for cand in &table.candidates {
        assert!(!cand.label.is_empty(), "{}", cand.id);
        assert!(
            ["any", "tonal"].contains(&cand.key.as_str()),
            "{}: key {}",
            cand.id,
            cand.key
        );
        assert!(
            cand.tempo.0 >= 0.0 && cand.tempo.1 > cand.tempo.0,
            "{}",
            cand.id
        );
        for f in &cand.families {
            assert!(f == "*" || families.contains(f), "{}: family {f}", cand.id);
        }
        for p in &cand.palettes {
            assert!(p == "*" || set.named(Some(p)).is_some(), "{}: {p}", cand.id);
        }
        match cand.kind {
            CandidateKind::OneShot => {
                let o = cand
                    .option
                    .as_deref()
                    .unwrap_or_else(|| panic!("{} has an option", cand.id));
                if let Some(name) = synth_name(o) {
                    assert!(known_instrument(name, &c), "{}: {name}", cand.id);
                } else if o.starts_with("gm:") || o.starts_with("dls:") {
                    let n: i64 = o.split(':').nth(1).unwrap().parse().unwrap();
                    assert!((0..128).contains(&n), "{}: {o}", cand.id);
                } else if let Some(cat) = o.strip_prefix("cat:") {
                    assert!(c.gm.contains_key(cat), "{}: category {cat}", cand.id);
                } else if let Some(b) = o.strip_prefix("bank:") {
                    assert!(set.categories.contains_key(b), "{}: {o}", cand.id);
                } else {
                    panic!("{}: {o} is not an option", cand.id);
                }
            }
            CandidateKind::Macro => {
                assert!(
                    is_macro(cand.macro_id.as_deref().unwrap()),
                    "{}: {:?}",
                    cand.id,
                    cand.macro_id
                )
            }
            CandidateKind::Loop => match &cand.loop_source {
                Some(LoopSource::Stock { query, .. }) => assert!(!query.is_empty(), "{}", cand.id),
                Some(LoopSource::Clip(id)) => assert!(!id.is_empty(), "{}", cand.id),
                None => panic!("{} names no loop", cand.id),
            },
        }
    }
    for g in BeatsTables::shared().genres.keys() {
        assert!(!stinger::family_of(g).is_empty(), "{g} has a family");
    }
    assert_eq!(table.slots.len(), stinger::SLOT_COUNT);
    assert!(table.slots.iter().all(|s| !s.is_empty()));
}

fn hits(
    set: &PaletteSet,
    genre: &str,
    palette: Option<&str>,
    seed: f64,
    c: &StingerContent,
) -> Vec<stinger::StingerAssignment> {
    resolve(set, genre, palette, seed, c, None, None)
}

fn ids(h: &[stinger::StingerAssignment]) -> Vec<Option<String>> {
    h.iter().map(|a| a.candidate.clone()).collect()
}

#[test]
fn the_resolver_is_deterministic() {
    let set = PaletteSet::shared();
    let all = StingerContent::everything();
    for genre in ["techno", "lofi", "dubstep", "jazz", "metal", "synthwave"] {
        for palette in [None, Some("club"), Some("orchestral"), Some("chiptune")] {
            for seed in [0.0, 1.0, 42.0, 123_456_789.0] {
                let a = hits(set, genre, palette, seed, &all);
                let b = hits(set, genre, palette, seed, &all);
                let pid = set
                    .playing(palette)
                    .map(|p| p.id.clone())
                    .unwrap_or("studio".into());
                let tempo = BeatsTables::shared()
                    .genres
                    .get(genre)
                    .map(|g| g.bpm)
                    .unwrap_or(120.0);
                let c = compute(set, genre, &pid, seed, &all, tempo, None);
                assert!(a == b && a == c, "{genre} {palette:?} {seed}");
                assert_eq!(a.len(), 4);
                assert_eq!(
                    a.iter().map(|x| x.slot).collect::<Vec<_>>(),
                    vec![0, 1, 2, 3]
                );
                let picked: Vec<_> = a.iter().filter_map(|x| x.candidate.clone()).collect();
                assert_eq!(
                    picked.iter().collect::<BTreeSet<_>>().len(),
                    picked.len(),
                    "no candidate twice"
                );
            }
        }
    }
    let seeds: BTreeSet<String> = (0..24)
        .map(|s| {
            hits(set, "techno", None, s as f64, &all)
                .iter()
                .filter_map(|a| a.candidate.clone())
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    assert!(seeds.len() > 1, "the seed moves the picks");
}

#[test]
fn hits_differ_across_genre_families() {
    let set = PaletteSet::shared();
    let all = StingerContent::everything();
    let table = Table::shared();
    let club_only: BTreeSet<&String> = table
        .candidates
        .iter()
        .filter(|c| {
            !c.families.iter().any(|f| f == "*") && !c.families.iter().any(|f| f == "chill")
        })
        .map(|c| &c.id)
        .collect();
    let mut differ = 0;
    for s in 0..20 {
        let club = hits(set, "techno", None, s as f64, &all);
        let chill = hits(set, "lofi", None, s as f64, &all);
        if ids(&club) != ids(&chill) {
            differ += 1;
        }
        assert!(chill
            .iter()
            .filter_map(|a| a.candidate.as_ref())
            .all(|c| !club_only.contains(c)));
    }
    assert!(differ >= 18, "{differ}/20 seeds differ");
    assert_ne!(stinger::family_of("techno"), stinger::family_of("lofi"));
}

#[test]
fn the_palette_steers_the_hits() {
    let set = PaletteSet::shared();
    let all = StingerContent::everything();
    let only: BTreeSet<String> = Table::shared()
        .candidates
        .iter()
        .filter(|c| c.palettes == ["orchestral"])
        .map(|c| c.id.clone())
        .collect();
    assert!(!only.is_empty());
    let mut seen = false;
    for s in 0..40 {
        let plain = hits(set, "trance", None, s as f64, &all);
        assert!(plain
            .iter()
            .filter_map(|a| a.candidate.as_ref())
            .all(|c| !only.contains(c)));
        let orch = hits(set, "trance", Some("orchestral"), s as f64, &all);
        if orch.iter().any(|a| {
            a.candidate
                .as_ref()
                .map(|c| only.contains(c))
                .unwrap_or(false)
        }) {
            seen = true;
        }
    }
    assert!(seen, "the orchestral palette's own hits get drawn");
}

#[test]
fn tonal_hits_keep_to_the_key() {
    let set = PaletteSet::shared();
    let all = StingerContent::everything();
    let f_sharp_major = Key::from_scale(66, "Major");
    assert!(!f_sharp_major.holds(stinger::STINGER_NOTE));
    assert!(Key::from_scale(60, "Major").holds(stinger::STINGER_NOTE));
    let tonal: BTreeSet<String> = Table::shared()
        .candidates
        .iter()
        .filter(|c| c.key == "tonal")
        .map(|c| c.id.clone())
        .collect();
    for s in 0..30 {
        let h = resolve(
            set,
            "house",
            None,
            s as f64,
            &all,
            None,
            Some(&f_sharp_major),
        );
        assert!(
            h.iter()
                .filter_map(|a| a.candidate.as_ref())
                .all(|c| !tonal.contains(c)),
            "seed {s}"
        );
    }
}

#[test]
fn missing_content_falls_back() {
    let set = PaletteSet::shared();
    let nothing = StingerContent::nothing();
    for genre in ["techno", "lofi", "jazz", "dnb"] {
        let h = hits(set, genre, None, 3.0, &nothing);
        assert!(
            h[0].kind == AssignmentKind::Fallback && h[1].kind == AssignmentKind::Fallback,
            "{genre}"
        );
        assert!(h[0].candidate.is_none() && h[0].net_id() == "hit1");
        assert_eq!(h[2].kind, AssignmentKind::Macro, "{genre}");
        assert!(
            h.iter()
                .all(|a| a.kind != AssignmentKind::Loop && a.kind != AssignmentKind::OneShot),
            "{genre}"
        );
    }
    let no_gm = StingerContent::new(|o| !o.starts_with("gm:"));
    for s in 0..20 {
        let h = hits(set, "trance", Some("orchestral"), s as f64, &no_gm);
        assert!(h
            .iter()
            .filter_map(|a| a.option.as_ref())
            .all(|o| !o.starts_with("gm:")));
    }
    let all = StingerContent::everything();
    for s in 0..20 {
        let h = hits(set, "edm", None, s as f64, &all);
        assert_ne!(h[3].kind, AssignmentKind::Loop);
    }
}

#[test]
fn a_stock_loop_is_assigned_when_it_fits() {
    let set = PaletteSet::shared();
    let riser = stock("Test FX Riser 01", 16, 128.0, "Sound Effect");
    let mut content = StingerContent::everything();
    content.stock = vec![
        riser.clone(),
        stock("Slow FX Riser", 16, 90.0, "Sound Effect"),
        stock("Long FX Riser", 64, 128.0, "Sound Effect"),
    ];
    for s in 0..10 {
        let h = hits(set, "techno", None, s as f64, &content);
        assert_eq!(h[3].kind, AssignmentKind::Loop, "seed {s}");
        assert_eq!(
            h[3].loop_.as_ref().unwrap().path.as_deref(),
            Some(riser.path.as_str())
        );
        assert!(h[3].local_only);
        assert_eq!(
            h[3].json()
                .get("loop")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str(),
            Some(riser.name.as_str())
        );
    }
    let lofi = hits(set, "lofi", None, 1.0, &content);
    assert_ne!(lofi[3].kind, AssignmentKind::Loop);
    // the stock matcher: tempo order, then names
    assert_eq!(riser.bpm(), 128.0);
    let m = stinger::matching(&content.stock, "Sound Effect", "fx riser", Some(128.0));
    // tempo ties go by name, then the slower one last
    assert_eq!(
        m.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        vec!["Long FX Riser", "Test FX Riser 01", "Slow FX Riser"]
    );
    assert_eq!(
        stinger::matching(&content.stock, "Drums", "", None).len(),
        0
    );
}

#[test]
fn curated_one_shots_land_on_their_stinger_nets() {
    let set = PaletteSet::shared();
    let inst = full();
    let c = catalog_with_sf(&inst);
    let ctx = Sounds {
        catalog: &c,
        installed: &inst,
    };
    let project = compose_with(
        "house",
        &[
            ("seed", JsonValue::int(11)),
            ("structure", JsonValue::str("standard")),
        ],
    );
    let all = StingerContent::everything();
    let h = hits(set, "house", None, 11.0, &all);
    assert_eq!(h.len(), 4);
    let mut nets = SoundNet::from_project(&project, None);
    stinger::curate_stingers(&mut nets, &h, 11.0, set, ctx);
    for a in &h {
        let n = nets.iter().find(|n| n.id == a.net_id());
        if let Some(n) = n {
            if a.kind == AssignmentKind::OneShot {
                assert_eq!(n.sound.curated, a.option);
            } else {
                assert_eq!(n.sound.curated, None);
            }
        }
    }
    // the assignments' help text and json
    assert!(h[0].help().starts_with("Hit 1: "));
    assert_eq!(
        stinger::StingerAssignment::fallback(2).help(),
        "Hit 3: the track's stinger"
    );
    let j = stinger::StingerAssignment::fallback(0).json();
    assert_eq!(j.get("kind").unwrap().as_str(), Some("fallback"));
    assert_eq!(j.get("hit").unwrap().as_i64(), Some(1));
}
