//! Swift golden for `TrackSpec.finish` and the sound it resolves (`tools/paritydump`, section
//! "spec": `swift/spec.json`).
//!
//! Swift's `finish` ends in `Project.applySound`, which asks what the Mac has installed. The
//! dump ran every spec in six SoundFont folders it built itself (nothing, the floor bank, more
//! banks, the Minecraft bank with and without its sidecar, and an older sidecar) with the
//! sampled instruments the Mac that ran it had (`exsPools`); each scenario is rebuilt here as
//! an `Installed` plus a `VoiceCatalog`, and `TrackSpec::project_in` is asked the same
//! question. Compared exactly: the canonical model's length and sha-256, the
//! `projectToJSON` document's sha-256, every playing net's resolved instrument, `picked` and
//! `TrackSound` (runs of nets that came out the same are one row), the spec payloads
//! `withPalette` / `nextVersion` / `carryingSound` write, and the palette ids a track records.

mod common;
use common::*;
use djrust::cid;
use djrust::json::JsonValue;
use djrust::pflow::Project;
use djrust::sound::{Installed, MinecraftSidecar, PaletteSet, Sounds};
use djrust::voices::{CatalogSound, VoiceCatalog};
use djrust::TrackSpec;
use std::collections::BTreeMap;

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

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
        minecraft_assets_missing: None,
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

/// `spec.json`'s `netRows`: playing nets in order, a run of identical ones as one row.
fn net_rows(p: &Project) -> Vec<String> {
    let opt = |x: &Option<String>| x.as_deref().map(JsonValue::str).unwrap_or(JsonValue::Null);
    let mut rows: Vec<(String, i64, Vec<JsonValue>)> = Vec::new();
    for (id, nb) in p.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        let t = &nb.track;
        let rest = vec![
            JsonValue::int(t.channel),
            JsonValue::str(t.instrument.as_str()),
            JsonValue::Bool(t.picked),
            opt(&t.sound.option),
            opt(&t.sound.kit),
            opt(&t.sound.composed),
            opt(&t.sound.override_),
            opt(&t.kit),
            opt(&t.palette),
        ];
        match rows.last_mut() {
            Some(last) if last.2 == rest => last.1 += 1,
            _ => rows.push((id.to_string(), 1, rest)),
        }
    }
    rows.into_iter()
        .map(|(id, count, rest)| {
            let mut v = vec![JsonValue::str(id), JsonValue::int(count)];
            v.extend(rest);
            JsonValue::Array(v).js_stringified()
        })
        .collect()
}

fn golden_net_rows(r: &JsonValue) -> Vec<String> {
    r.get("nets")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(JsonValue::js_stringified)
        .collect()
}

/// The project columns of a row (`projectRow`).
fn check_project(d: &mut Diffs, tag: &str, p: &Project, r: &JsonValue) {
    let canonical = p.canonical_json();
    d.eq(
        format!("{tag} canonical bytes"),
        canonical.len() as i64,
        i(r, "canonicalBytes"),
    );
    d.eq(
        format!("{tag} canonical sha"),
        sha(&canonical),
        s(r, "canonicalSha").to_string(),
    );
    d.eq(
        format!("{tag} doc sha"),
        sha(&p.to_json().js_stringified()),
        s(r, "docSha").to_string(),
    );
    d.eq(format!("{tag} nets"), net_rows(p), golden_net_rows(r));
}

fn spec_of(r: &JsonValue) -> TrackSpec {
    TrackSpec::new(
        s(r, "genre"),
        f(r, "seed"),
        r.get("structure").and_then(JsonValue::as_str),
    )
}

/// `SoundPalette.normalized`: what a track writes for a palette id, in each folder.
/// Swift: `PaletteDataTests.studioIsNoPaletteAtAll` is the same rule; this holds the Minecraft
/// bank's asset version (`minecraft@32`, `@27`, `@32+b2`) to Swift's.
#[test]
fn normalized_palette_ids_match_swift() {
    let doc = fixture("spec");
    let set = PaletteSet::shared();
    let mut d = Diffs::default();
    for sc in rows(&doc, "normalized") {
        let w = world(&doc, s(sc, "scenario"));
        for r in rows(sc, "rows") {
            let id = r.get("id").and_then(JsonValue::as_str);
            let want = r
                .get("normalized")
                .and_then(JsonValue::as_str)
                .map(String::from);
            d.eq(
                format!("{} normalized({id:?})", s(sc, "scenario")),
                set.normalized(id, &w.installed),
                want,
            );
        }
    }
    d.finish("normalized palette ids");
}

/// A spec with no `layers` is the composed project whatever is installed: `finish` returns
/// before it asks (Swift's `guard !layers.entries.isEmpty`), so no palette-less project moves.
#[test]
fn a_spec_without_layers_is_the_composed_project() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    for r in rows(&doc, "plain") {
        let w = world(&doc, s(r, "scenario"));
        let spec = TrackSpec::from_payload(r.get("spec").unwrap()).unwrap();
        let tag = format!("{} {}", s(r, "scenario"), spec.payload().js_stringified());
        let p = spec.project_in(w.sounds());
        check_project(&mut d, &tag, &p, r);
        d.eq(
            format!("{tag} == project()"),
            p.canonical_json(),
            spec.project().canonical_json(),
        );
        d.eq(
            format!("{tag} == compose"),
            p.canonical_json(),
            djrust::compose(&spec.genre, &spec.overrides()).canonical_json(),
        );
    }
    d.finish("plain specs");
}

/// `TrackSpec.withPalette` for every palette on five specs, then `finish`: the payload the
/// palette is written as, the palette the spec names, and what every net resolves to.
#[test]
fn palettes_resolve_as_swift_does() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    let mut worlds: BTreeMap<String, World> = BTreeMap::new();
    for r in rows(&doc, "palettes") {
        let sc = s(r, "scenario");
        let w = worlds
            .entry(sc.to_string())
            .or_insert_with(|| world(&doc, sc));
        let id = s(r, "palette");
        let spec = spec_of(r).with_palette(Some(id), &w.installed);
        let tag = format!("{sc} {} {id}", s(r, "genre"));
        d.eq(
            format!("{tag} spec"),
            spec.payload().js_stringified(),
            r.get("spec").unwrap().js_stringified(),
        );
        d.eq(
            format!("{tag} palette"),
            spec.palette(),
            r.get("specPalette")
                .and_then(JsonValue::as_str)
                .map(String::from),
        );
        check_project(&mut d, &tag, &spec.project_in(w.sounds()), r);
    }
    d.finish("palettes");
}

/// Sound choices written into the layers (a role's `voice`, a picked `instrument`, a `kit`,
/// a palette on some channels, a palette Swift has never heard of) with the other layers
/// around them: the order is overrides, overlay, then the sound.
#[test]
fn sound_choices_in_the_layers_resolve_as_swift_does() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    let mut worlds: BTreeMap<String, World> = BTreeMap::new();
    for r in rows(&doc, "layers") {
        let sc = s(r, "scenario");
        let w = worlds
            .entry(sc.to_string())
            .or_insert_with(|| world(&doc, sc));
        let spec = TrackSpec::from_payload(r.get("spec").unwrap()).unwrap();
        let tag = format!("{sc} {} {}", s(r, "genre"), s(r, "case"));
        d.eq(
            format!("{tag} spec"),
            spec.payload().js_stringified(),
            r.get("spec").unwrap().js_stringified(),
        );
        d.eq(
            format!("{tag} palette"),
            spec.palette(),
            r.get("specPalette")
                .and_then(JsonValue::as_str)
                .map(String::from),
        );
        check_project(&mut d, &tag, &spec.project_in(w.sounds()), r);
    }
    d.finish("layers");
}

/// Auto-next: `nextVersion` keeps the palette, kit and role overrides and nothing else, and
/// each version resolves its own seed's sound. `carryingSound` both ways.
/// Swift: `PaletteDeckTests.overridesSurviveRecomputesUntilReset` (the spec-level half).
#[test]
fn the_next_versions_carry_the_sound_and_resolve_it() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    for r in rows(&doc, "chains") {
        let sc = s(r, "scenario");
        let w = world(&doc, sc);
        let chain = rows(r, "chain");
        let mut cur = TrackSpec::from_payload(chain[0].get("spec").unwrap()).unwrap();
        for (k, step) in chain.iter().enumerate() {
            let tag = format!("{sc} {} v{k}", s(r, "genre"));
            d.eq(
                format!("{tag} spec"),
                cur.payload().js_stringified(),
                step.get("spec").unwrap().js_stringified(),
            );
            check_project(&mut d, &tag, &cur.project_in(w.sounds()), step);
            cur = cur.next_version();
        }
        // the carried sound, onto a plain spec and from one that has none
        let base = TrackSpec::from_payload(chain[0].get("spec").unwrap()).unwrap();
        let mut plain = TrackSpec::new(&base.genre, base.seed + 1.0, base.structure.as_deref());
        plain.cohesion = base.cohesion.clone();
        d.eq(
            format!("{sc} {} carried", s(r, "genre")),
            plain.carrying_sound(&base).payload().js_stringified(),
            r.get("carried").unwrap().js_stringified(),
        );
        d.eq(
            format!("{sc} {} carried from plain", s(r, "genre")),
            base.carrying_sound(&plain).payload().js_stringified(),
            r.get("carriedFromPlain").unwrap().js_stringified(),
        );
    }
    d.finish("chains");
}

/// `nextVersion` rounds the seed with Swift's `.rounded()` (toNearestOrAwayFromZero), not with
/// JS's `Math.round` (ties toward +infinity): -2.5 rounds to -3 and -0.5 to -1. The chains above
/// only have positive integer seeds, so `swift/spec.json` carries `nextSeeds`, which Swift wrote
/// through the real `TrackSpec.nextVersion` for ties on both sides of zero.
/// Swift: `DeckGenerator.swift` `TrackSpec.nextVersion`.
#[test]
fn the_next_seed_rounds_half_away_from_zero_as_swift_does() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    let seeds = rows(&doc, "nextSeeds");
    assert!(seeds.len() >= 12, "the fixture has the tie seeds");
    for r in seeds {
        let seed = r.get("seed").and_then(JsonValue::as_f64).unwrap();
        let want = r.get("next").and_then(JsonValue::as_f64).unwrap();
        let got = TrackSpec::new("techno", seed, None).next_version().seed;
        d.eq(format!("seed {seed}"), got.to_string(), want.to_string());
    }
    d.finish("nextSeeds");
}

/// The same ties as literals worked out from Swift's formula (`Int64(seed.rounded()) * 48_271 %
/// 2_147_483_647`, then `abs(s) % 999_999 + 1`): -2.5 rounds to -3 (next seed 144814), -0.5 to
/// -1 (48272), 0.5 to 1 (48272) and 2.5 to 3 (144814). JS's half-up rule would give 96543 and 1
/// for the two negative ties.
#[test]
fn next_version_ties_are_worked_literals() {
    let next = |seed: f64| TrackSpec::new("techno", seed, None).next_version().seed;
    assert_eq!(next(-2.5), 144_814.0, "-2.5 rounds to -3");
    assert_eq!(next(-0.5), 48_272.0, "-0.5 rounds to -1");
    assert_eq!(next(0.5), 48_272.0, "0.5 rounds to 1");
    assert_eq!(next(2.5), 144_814.0, "2.5 rounds to 3");
    // a mirrored pair gives the same next seed (the abs of the product), unlike a half-up rule
    assert_eq!(next(-1.5), 96_543.0);
    assert_eq!(next(1.5), 96_543.0);
    // integers and non-ties are unchanged: the chain's own first step
    assert_eq!(next(11.0), 530_982.0);
}
