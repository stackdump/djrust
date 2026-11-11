//! Swift golden for the deck generator's remaining oracles (`tools/paritydump`, section
//! "deckgen": `swift/deckgen.json`): `SoundPalette`'s direct API against Swift's own output (why
//! a palette cannot play, what each role picks and plays from, the option reasons, labels, kit
//! ids and sources, `PaletteCard.cards`), `StingerContent.here` (what the app plays) over five
//! genres x every palette, and `TrackSpec.nextVersion` on fractional and negative seeds.
//!
//! `spec_golden.rs` holds the nets a palette resolves to; the reason strings Wander and
//! `dj_palettes` show, the labels and the source lists were held by literals in `sound.rs` only.
//! Each scenario is a SoundFont folder the dump built (nothing, the floor bank, more banks, the
//! Minecraft bank with and without its sidecar), rebuilt here as an `Installed` plus a
//! `VoiceCatalog`, with what the Mac that ran the dump had (`gmBank`, `exsPools`,
//! `minecraftAssets`). Left out of the dump, and so of the comparison: `.exs` sources (the Rust
//! catalog is built from names) and the labels of kits (read from the banks on that Mac).

mod common;
use common::*;
use djrust::json::JsonValue;
use djrust::sound::{
    option_label, sources_for_option, unavailable_reason_option, Installed, MinecraftSidecar,
    PaletteCard, PaletteSet, Sounds,
};
use djrust::sound::stinger::StingerContent;
use djrust::voices::{CatalogSound, SamplerSource, VoiceCatalog};
use djrust::wander::deck::resolved_project_stingers;
use djrust::TrackSpec;
use std::collections::BTreeMap;

/// One scenario as the host would have scanned it.
struct World {
    installed: Installed,
    catalog: VoiceCatalog,
}

impl World {
    fn sounds(&self) -> Sounds<'_> {
        Sounds {
            catalog: &self.catalog,
            installed: &self.installed,
        }
    }
}

fn world(doc: &JsonValue, scenario: &str) -> World {
    let sc = rows(doc, "scenarios")
        .iter()
        .find(|r| s(r, "name") == scenario)
        .unwrap_or_else(|| panic!("no scenario {scenario}"));
    let installed = Installed {
        dir: format!("/golden/{scenario}"),
        files: sc.get("files").unwrap().strings().unwrap(),
        gm_bank: doc.get("gmBank").and_then(JsonValue::as_bool).unwrap(),
        kits: vec![],
        minecraft: sc.get("sidecar").and_then(MinecraftSidecar::from_json),
        // why the game's assets are not there to import from, on the Mac that ran the dump
        minecraft_assets_missing: doc
            .get("minecraftAssets")
            .and_then(JsonValue::as_str)
            .map(String::from),
    };
    let mut sounds: BTreeMap<String, Vec<CatalogSound>> = BTreeMap::new();
    for (category, names) in doc.get("exsPools").unwrap().as_object().unwrap().iter() {
        let list = names
            .as_array()
            .unwrap()
            .iter()
            .map(|n| CatalogSound {
                name: n.as_str().unwrap().to_string(),
                category: category.to_string(),
                sources: vec![],
            })
            .collect();
        sounds.insert(category.to_string(), list);
    }
    let catalog = VoiceCatalog::with_installed(sounds, installed.sound_fonts());
    World { installed, catalog }
}

fn worlds(doc: &JsonValue) -> BTreeMap<String, World> {
    rows(doc, "scenarios")
        .iter()
        .map(|r| (s(r, "name").to_string(), world(doc, s(r, "name"))))
        .collect()
}

fn opt(r: &JsonValue, key: &str) -> Option<String> {
    r.get(key).and_then(JsonValue::as_str).map(String::from)
}

/// `optList`: the sources as `SamplerSource.description`s, without the `.exs` ones; null stays None.
fn source_list(v: Option<Vec<SamplerSource>>) -> Option<Vec<String>> {
    v.map(|l| {
        l.iter()
            .filter(|s| !matches!(s, SamplerSource::Exs(_)))
            .map(|s| s.to_string())
            .collect()
    })
}

fn golden_list(v: &JsonValue) -> Option<Vec<String>> {
    v.as_array().map(|a| {
        a.iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    })
}

/// `SoundPalette.unavailableReason`, `roleReport`, `isPlayable`, `kit`, `kit(for:)`, `isLocalOnly`,
/// `banks`, and what each role picks and plays from first, for every shipped palette in every
/// scenario.
#[test]
fn the_palette_api_matches_swift() {
    let doc = fixture("deckgen");
    let set = PaletteSet::shared();
    let worlds = worlds(&doc);
    let mut d = Diffs::default();
    for r in rows(&doc, "palettes") {
        let sc = s(r, "scenario");
        let w = &worlds[sc];
        let id = s(r, "id");
        let p = set.named(Some(id)).unwrap_or_else(|| panic!("no palette {id}"));
        let ctx = w.sounds();
        let tag = format!("{sc} {id}");
        d.eq(format!("{tag} unavailable"), p.unavailable_reason(set, ctx), opt(r, "unavailable"));
        let report: Vec<(String, Option<String>)> = r
            .get("roleReport")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let e = e.as_array().unwrap();
                (e[0].as_str().unwrap().to_string(), e[1].as_str().map(String::from))
            })
            .collect();
        d.eq(format!("{tag} roleReport"), p.role_report(set, ctx), report);
        d.eq(
            format!("{tag} playable"),
            p.is_playable(set, ctx),
            r.get("playable").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(format!("{tag} kit"), p.kit(set), opt(r, "kit"));
        d.eq(format!("{tag} kit(for:)"), set.kit_for(Some(id), ctx), opt(r, "kitFor"));
        d.eq(
            format!("{tag} localOnly"),
            p.is_local_only(set),
            r.get("localOnly").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(
            format!("{tag} banks"),
            p.banks(set).into_iter().collect::<Vec<_>>(),
            golden_list(r.get("banks").unwrap()).unwrap(),
        );
        for pick in r.get("picks").unwrap().as_array().unwrap() {
            let (role, seed) = (s(pick, "role"), f(pick, "seed"));
            let t = format!("{tag} {role} seed {seed}");
            d.eq(format!("{t} pick"), p.pick(role, role, seed, set, ctx), opt(pick, "pick"));
            d.eq(
                format!("{t} sources"),
                source_list(p.sources(role, seed, set, ctx)),
                golden_list(pick.get("sources").unwrap()),
            );
        }
    }
    d.finish("palette api");
}

/// `unavailableReason(option:role:)`, `kitID(option:)`, `isLocalOnly(option:)`, `label` and
/// `sources(option:role:seed:)` for every option a palette names in a role, at three seeds.
#[test]
fn the_option_api_matches_swift() {
    let doc = fixture("deckgen");
    let set = PaletteSet::shared();
    let worlds = worlds(&doc);
    let mut d = Diffs::default();
    for r in rows(&doc, "options") {
        let sc = s(r, "scenario");
        let ctx = worlds[sc].sounds();
        let (option, role) = (s(r, "option"), s(r, "role"));
        let tag = format!("{sc} {option} as {role}");
        d.eq(
            format!("{tag} unavailable"),
            unavailable_reason_option(set, option, role, ctx),
            opt(r, "unavailable"),
        );
        d.eq(format!("{tag} kitID"), set.kit_id(option), opt(r, "kitID"));
        d.eq(
            format!("{tag} localOnly"),
            set.is_local_only_option(option),
            r.get("localOnly").and_then(JsonValue::as_bool).unwrap(),
        );
        let seeds = [1.0, 7.0, 42.5];
        let labels = r.get("labels").unwrap().as_array().unwrap();
        let sources = r.get("sources").unwrap().as_array().unwrap();
        for (k, seed) in seeds.iter().enumerate() {
            // a null label is one the dump left out (a kit's, read from this Mac's banks)
            if let Some(want) = labels[k].as_str() {
                d.eq(
                    format!("{tag} label @{seed}"),
                    option_label(set, option, role, *seed, ctx),
                    want.to_string(),
                );
            }
            d.eq(
                format!("{tag} sources @{seed}"),
                source_list(sources_for_option(set, option, role, *seed, ctx)),
                golden_list(&sources[k]),
            );
        }
    }
    d.finish("option api");
}

/// `PaletteCard.cards`: every card's label, icon, colour, description, why it is grey, whether it
/// is local-only and which is lit, for four current palettes in each scenario.
#[test]
fn the_palette_cards_match_swift() {
    let doc = fixture("deckgen");
    let set = PaletteSet::shared();
    let worlds = worlds(&doc);
    let mut d = Diffs::default();
    for r in rows(&doc, "cards") {
        let sc = s(r, "scenario");
        let current = opt(r, "current");
        let got = PaletteCard::cards(set, current.as_deref(), worlds[sc].sounds());
        let want = r.get("cards").unwrap().as_array().unwrap();
        d.eq(format!("{sc} {current:?} count"), got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            let tag = format!("{sc} {current:?} {}", g.id);
            d.eq(format!("{tag} id"), g.id.as_str(), s(w, "id"));
            d.eq(format!("{tag} label"), g.label.as_str(), s(w, "label"));
            d.eq(format!("{tag} icon"), g.icon.as_str(), s(w, "icon"));
            d.eq(format!("{tag} color"), g.color.as_str(), s(w, "color"));
            d.eq(format!("{tag} description"), g.description.as_str(), s(w, "description"));
            d.eq(format!("{tag} unavailable"), g.unavailable.clone(), opt(w, "unavailable"));
            d.eq(
                format!("{tag} localOnly"),
                g.local_only,
                w.get("localOnly").and_then(JsonValue::as_bool).unwrap(),
            );
            d.eq(
                format!("{tag} selected"),
                g.selected,
                w.get("selected").and_then(JsonValue::as_bool).unwrap(),
            );
        }
    }
    d.finish("palette cards");
}

/// `StingerContent.here` (the policy the app plays): the Hits `resolvedStingers` gives the played
/// project, as `dj_state` shows them, for five genres x every palette in each scenario. The stock
/// shelf is empty (Apple Loops are n/a), as in the dump.
#[test]
fn the_here_policy_resolves_the_hits_as_swift_does() {
    let doc = fixture("deckgen");
    let set = PaletteSet::shared();
    let worlds = worlds(&doc);
    let mut d = Diffs::default();
    for r in rows(&doc, "stingers") {
        let sc = s(r, "scenario");
        let w = &worlds[sc];
        let (genre, seed) = (s(r, "genre"), f(r, "seed"));
        let palette = opt(r, "palette");
        let mut spec = TrackSpec::new(genre, seed, r.get("structure").and_then(JsonValue::as_str));
        if let Some(id) = &palette {
            spec = spec.with_palette(Some(id), &w.installed);
        }
        let project = spec.project_in(w.sounds());
        let content = StingerContent::here(set, w.sounds(), vec![]);
        let hits: Vec<String> = resolved_project_stingers(&project, set, genre, spec.seed, &content)
            .iter()
            .map(|a| a.json().js_stringified())
            .collect();
        d.eq(
            format!("{sc} {genre} {palette:?}"),
            hits,
            golden_list(r.get("hits").unwrap()).unwrap(),
        );
    }
    d.finish("stingers (here)");
}

/// `TrackSpec.nextVersion`'s Park-Miller step on seeds the chains of `spec.json` never use:
/// Swift rounds `Int64(seed.rounded())` half away from zero, so a negative tie goes away from
/// zero too (`-2.5` steps from -3: 144814, where a `Math.round` would step from -2). A seed with
/// a fraction in the Generate panel is a typed one; a recorded set chains through it.
#[test]
fn next_version_matches_swift_on_fractional_seeds() {
    let doc = fixture("deckgen");
    let mut d = Diffs::default();
    for r in rows(&doc, "nextVersion") {
        let seed = f(r, "seed");
        let next = TrackSpec::new("house", seed, None).next_version().seed;
        d.eq(format!("nextVersion({seed})"), next, f(r, "next"));
    }
    d.finish("nextVersion");
}

/// The negative ties by name, with the numbers Swift's own run gave (`deckgen.json` holds them
/// too, above): `Int64(seed.rounded())` goes half away from zero, so -2.5 steps from -3 (Swift
/// 144814), -0.5 from -1 (48272); `Math.round`'s ties toward +infinity would give 96543 and 1.
#[test]
fn next_version_matches_swift_on_negative_ties() {
    for (seed, swift) in [
        (-3.5, 193_085.0),
        (-2.5, 144_814.0),
        (-1.5, 96_543.0),
        (-0.5, 48_272.0),
        (-123_456.5, 427_218.0),
    ] {
        let next = TrackSpec::new("house", seed, None).next_version().seed;
        assert_eq!(next, swift, "nextVersion({seed})");
    }
    // and the golden rows of the same seeds agree with the literals
    let doc = fixture("deckgen");
    let ties: Vec<(f64, f64)> = rows(&doc, "nextVersion")
        .iter()
        .map(|r| (f(r, "seed"), f(r, "next")))
        .filter(|(seed, _)| *seed < 0.0 && seed.fract() == -0.5)
        .collect();
    assert_eq!(ties.len(), 5, "the five negative ties are in the dump: {ties:?}");
    for (seed, next) in ties {
        assert_eq!(
            TrackSpec::new("house", seed, None).next_version().seed,
            next,
            "nextVersion({seed})"
        );
    }
}
