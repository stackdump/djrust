//! `BeatsComposerTests` (the 2.4 gate) and its `ModuleVectorTests`, against the fixtures that
//! beats' own JavaScript wrote (`fixtures/js/{projects,compose,modules}.json`, from
//! `scripts/beats-parity.mjs`): `compose()` must reproduce beats' project for the same
//! (genre, overrides) - canonical model bytes, model CID and the full serialized document - and
//! the generator's modules must produce beats' vectors. Nothing here was written by Swift, so
//! this holds the generator to a non-Swift oracle (the Swift-written `swift/projects.json` and
//! `swift/modules.json` are what `projects.rs` and `modules.rs` compare).

mod common;
use common::*;
use djrust::cid;
use djrust::compose;
use djrust::gen::euclidean::bjorklund;
use djrust::gen::structure::{generate_structure, SongTemplate};
use djrust::gen::theme::{build_track_theme, cohesion_genre_supported, render_motif, MotifCell};
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::Project;
use djrust::rng::Mulberry32;

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

/// `check` of BeatsComposerTests: the canonical bytes, the model CID (the sha256 of the
/// canonical text) and the document hash.
fn check(d: &mut Diffs, tag: &str, p: &Project, row: &JsonValue) {
    let canonical = p.canonical_json();
    d.eq(
        format!("{tag} canonical bytes"),
        canonical.len() as i64,
        i(row, "canonicalBytes"),
    );
    d.eq(
        format!("{tag} model CID"),
        sha(&canonical),
        s(row, "modelCID").to_string(),
    );
    d.eq(
        format!("{tag} document"),
        sha(&p.to_json().js_stringified()),
        s(row, "docSha").to_string(),
    );
}

/// (a) BeatsComposerTests/ComposerGateTests.gateRejectsADifferentSeed: the gate has to be able
/// to fail.
#[test]
fn the_gate_rejects_a_different_seed() {
    let doc = js_fixture("projects");
    let row = &rows(&doc, "projects")[0];
    let mut ov = JsonObject::new();
    ov.set("seed", JsonValue::Number(f(row, "seed") + 1.0));
    ov.set("cohesion", row.get("cohesion").unwrap().clone());
    let project = compose(s(row, "genre"), &ov.into());
    assert_ne!(sha(&project.canonical_json()), s(row, "modelCID"));
}

/// (a) ComposerGateTests.matrixMatchesBeats: 19 genres x 4 seeds x loop/standard x v1/v2.
#[test]
fn the_matrix_matches_beats() {
    let doc = js_fixture("projects");
    let all = rows(&doc, "projects");
    assert_eq!(all.len(), 304);
    let mut d = Diffs::default();
    for row in all {
        let (genre, size) = (s(row, "genre"), s(row, "structure"));
        let mut ov = JsonObject::new();
        ov.set("seed", row.get("seed").unwrap().clone());
        ov.set("cohesion", row.get("cohesion").unwrap().clone());
        if size != "loop" {
            ov.set("structure", size.into());
        }
        let tag = format!("{genre}/{}/{size}/{}", f(row, "seed"), s(row, "cohesion"));
        let project = compose(genre, &ov.into());
        check(&mut d, &tag, &project, row);
        d.eq(
            format!("{tag} name"),
            project.name.clone(),
            s(row, "name").to_string(),
        );
    }
    d.finish("projects.json matrix");
}

/// (a) ComposerGateTests.overrideVariationsMatchBeats: every trait override beats reads.
#[test]
fn override_variations_match_beats() {
    let doc = js_fixture("compose");
    let mut d = Diffs::default();
    let all = rows(&doc, "cases");
    assert!(all.len() > 100, "{} cases", all.len());
    for row in all {
        let genre = s(row, "genre");
        let ov = row.get("overrides").unwrap();
        let project = compose(genre, ov);
        check(
            &mut d,
            &format!("{genre} {}", ov.js_stringified()),
            &project,
            row,
        );
    }
    d.finish("compose.json overrides");
}

// ---------------------------------------------------------------------------
// ModuleVectorTests (modules.json)

fn motif_json(c: &MotifCell) -> JsonValue {
    JsonObject::from_pairs([
        (
            "degrees",
            JsonValue::Array(c.degrees.iter().map(|d| JsonValue::int(*d)).collect()),
        ),
        (
            "mask",
            JsonValue::Array(c.mask.iter().map(|m| JsonValue::Bool(*m)).collect()),
        ),
        ("contour", JsonValue::int(c.contour)),
    ])
    .into()
}

/// The Swift test's `json(_ t: SongTemplate)`: beats' template shape (`active` is an object of
/// `true`, `slotMap` an empty object).
fn template_json(t: &SongTemplate) -> JsonValue {
    let sections = t
        .sections
        .iter()
        .map(|sec| {
            let mut phrases = JsonObject::new();
            for (k, v) in sec.phrases.iter() {
                phrases.set(
                    k,
                    JsonValue::Array(v.iter().map(|x| x.as_str().into()).collect()),
                );
            }
            let mut active = JsonObject::new();
            for k in sec.active.keys() {
                active.set(k.as_str(), JsonValue::Bool(true));
            }
            JsonObject::from_pairs([
                ("name", sec.name.as_str().into()),
                ("steps", JsonValue::int(sec.steps)),
                ("active", active.into()),
                ("phrases", phrases.into()),
            ])
            .into()
        })
        .collect();
    JsonObject::from_pairs([
        ("name", t.name.as_str().into()),
        ("sections", JsonValue::Array(sections)),
        ("slotMap", JsonObject::new().into()),
    ])
    .into()
}

/// (a) ModuleVectorTests.bjorklundMatches
#[test]
fn bjorklund_matches_beats() {
    let doc = js_fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "bjorklund") {
        let (k, n) = (i(r, "k"), i(r, "n"));
        d.eq(
            format!("bjorklund({k}, {n})"),
            bjorklund(k, n),
            r.get("bits").unwrap().ints().unwrap(),
        );
    }
    d.finish("bjorklund");
}

/// (a) ModuleVectorTests.generateStructureMatches: 19 genres x 8 sizes x 2 seeds.
#[test]
fn generate_structure_matches_beats() {
    let doc = js_fixture("modules");
    let all = rows(&doc, "structures");
    assert_eq!(all.len(), 19 * 8 * 2);
    let mut d = Diffs::default();
    for r in all {
        let (g, size, seed) = (s(r, "genre"), s(r, "size"), f(r, "seed"));
        let mut rng = Mulberry32::new(seed);
        let t = generate_structure(g, size, &mut rng);
        d.eq(
            format!("{g} {size} seed {seed}"),
            template_json(&t).js_stringified(),
            r.get("result").unwrap().js_stringified(),
        );
    }
    d.finish("structures");
}

/// (a) ModuleVectorTests.trackThemesMatch: the motif, the chord plan and the groove of
/// cohesion v2's theme.
#[test]
fn track_themes_match_beats() {
    let doc = js_fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "themes") {
        let (genre, seed) = (s(r, "genre"), f(r, "seed"));
        let tag = format!("{genre} {seed}");
        d.eq(
            format!("{tag} supported"),
            cohesion_genre_supported(genre),
            r.get("supported").unwrap().as_bool().unwrap(),
        );
        let theme = build_track_theme(genre, seed);
        let want = r.get("theme").unwrap();
        d.eq(
            format!("{tag} motif"),
            motif_json(&theme.motif).js_stringified(),
            want.get("motif").unwrap().js_stringified(),
        );
        let plan = want.get("plan").unwrap();
        d.eq(
            format!("{tag} stepsPerChord"),
            theme.plan.steps_per_chord,
            i(plan, "stepsPerChord"),
        );
        d.eq(
            format!("{tag} chords"),
            JsonValue::Array(theme.plan.chords.iter().map(|c| c.json()).collect()).js_stringified(),
            plan.get("chords").unwrap().js_stringified(),
        );
        d.eq(format!("{tag} groove"), theme.groove, i(want, "groove"));
    }
    d.finish("themes");
}

/// (a) ModuleVectorTests.renderMotifMatches (the theme of seed 42, every mode and offset).
#[test]
fn render_motif_matches_beats() {
    let doc = js_fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "motifs") {
        let genre = s(r, "genre");
        let theme = build_track_theme(genre, 42.0);
        let (mode, off) = (i(r, "mode"), i(r, "offset"));
        let out = render_motif(&theme.motif, mode, off);
        d.eq(
            format!("{genre} mode {mode} off {off}"),
            motif_json(&out).js_stringified(),
            r.get("out").unwrap().js_stringified(),
        );
    }
    d.finish("motifs");
}
