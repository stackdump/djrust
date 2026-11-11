//! Generator building blocks: seeds, bjorklund, song templates, cohesion-v2 themes,
//! motif rendering, track names.

mod common;
use common::*;
use djrust::gen::composer::generate_track_name;
use djrust::gen::core::drum_seed;
use djrust::gen::euclidean::bjorklund;
use djrust::gen::structure::{generate_structure, SongTemplate};
use djrust::gen::theme::{
    build_track_theme, cohesion_genre_supported, hash_str, render_motif, MotifCell,
};
use djrust::json::{JsonObject, JsonValue};
use djrust::rng::Mulberry32;

fn strs(v: &[String]) -> JsonValue {
    JsonValue::Array(v.iter().map(|s| s.as_str().into()).collect())
}

fn template_json(t: &SongTemplate) -> JsonValue {
    let sections = t
        .sections
        .iter()
        .map(|sec| {
            let mut phrases = JsonObject::new();
            for (k, v) in sec.phrases.iter() {
                phrases.set(k, strs(v));
            }
            JsonObject::from_pairs([
                ("name", sec.name.as_str().into()),
                ("steps", sec.steps.into()),
                ("active", strs(sec.active.keys())),
                ("phrases", phrases.into()),
            ])
            .into()
        })
        .collect();
    JsonObject::from_pairs([
        ("name", t.name.as_str().into()),
        ("sections", JsonValue::Array(sections)),
    ])
    .into()
}

fn motif_json(c: &MotifCell) -> JsonValue {
    JsonObject::from_pairs([
        (
            "degrees",
            JsonValue::Array(c.degrees.iter().map(|d| (*d).into()).collect()),
        ),
        (
            "mask",
            JsonValue::Array(c.mask.iter().map(|m| (*m).into()).collect()),
        ),
        ("contour", c.contour.into()),
    ])
    .into()
}

#[test]
fn bjorklund_matches_swift() {
    let doc = fixture("modules");
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

#[test]
fn song_templates_match_swift() {
    let doc = fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "structures") {
        let (g, size, seed) = (s(r, "genre"), s(r, "size"), f(r, "seed"));
        let mut rng = Mulberry32::new(seed);
        let t = generate_structure(g, size, &mut rng);
        let tag = format!("{g}/{size}/{seed}");
        d.eq(
            &tag,
            template_json(&t).js_stringified(),
            r.get("result").unwrap().js_stringified(),
        );
        d.eq(
            format!("{tag} rng after"),
            bits_hex(rng.next()),
            s(r, "after").to_string(),
        );
    }
    d.finish("structures");
}

#[test]
fn track_themes_match_swift() {
    let doc = fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "themes") {
        let (g, seed) = (s(r, "genre"), f(r, "seed"));
        let tag = format!("{g}/{seed}");
        let th = build_track_theme(g, seed);
        d.eq(
            &tag,
            cohesion_genre_supported(g),
            r.get("supported").unwrap().as_bool().unwrap(),
        );
        d.eq(
            &tag,
            motif_json(&th.motif).js_stringified(),
            r.get("motif").unwrap().js_stringified(),
        );
        d.eq(&tag, th.plan.steps_per_chord, i(r, "stepsPerChord"));
        let chords = JsonValue::Array(th.plan.chords.iter().map(|c| c.json()).collect());
        d.eq(
            &tag,
            chords.js_stringified(),
            r.get("chords").unwrap().js_stringified(),
        );
        d.eq(&tag, th.groove, i(r, "groove"));
        let mut energy = JsonObject::new();
        for (sec, p) in th.energy.iter() {
            let mut roles = JsonObject::new();
            for (role, rp) in p.roles.iter() {
                roles.set(
                    role,
                    JsonObject::from_pairs([
                        ("active", rp.active.into()),
                        (
                            "motifMode",
                            rp.motif_mode
                                .map(JsonValue::from)
                                .unwrap_or(JsonValue::Null),
                        ),
                        (
                            "registerShift",
                            rp.register_shift
                                .map(JsonValue::from)
                                .unwrap_or(JsonValue::Null),
                        ),
                    ])
                    .into(),
                );
            }
            energy.set(
                sec,
                JsonObject::from_pairs([
                    ("roles", roles.into()),
                    ("filterOpen", p.filter_open.into()),
                    ("energy", p.energy.into()),
                ])
                .into(),
            );
        }
        d.eq(
            &tag,
            JsonValue::Object(energy).js_stringified(),
            r.get("energy").unwrap().js_stringified(),
        );
    }
    d.finish("themes");
}

#[test]
fn rendered_motifs_match_swift() {
    let doc = fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "motifs") {
        let (g, seed, mode, off) = (s(r, "genre"), f(r, "seed"), i(r, "mode"), i(r, "offset"));
        let th = build_track_theme(g, seed);
        let out = render_motif(&th.motif, mode, off);
        d.eq(
            format!("{g}/{seed} mode {mode} offset {off}"),
            motif_json(&out).js_stringified(),
            r.get("out").unwrap().js_stringified(),
        );
    }
    d.finish("motifs");
}

#[test]
fn track_names_match_swift() {
    let doc = fixture("modules");
    let mut d = Diffs::default();
    for r in rows(&doc, "names") {
        let (g, seed) = (s(r, "genre"), f(r, "seed"));
        d.eq(
            format!("{g}/{seed}"),
            generate_track_name(g, seed),
            s(r, "name").to_string(),
        );
    }
    d.finish("names");
}

#[test]
fn seeds_match_swift() {
    let doc = fixture("foundation"); // seeds live with the foundation vectors
    let mut d = Diffs::default();
    for r in rows(&doc, "seeds") {
        let input = s(r, "input");
        d.eq(
            format!("drumSeed({input:?})"),
            bits_hex(drum_seed(input)),
            bits_hex(f(r, "drumSeed")),
        );
        d.eq(
            format!("hashStr({input:?})"),
            hash_str(input) as i64,
            i(r, "hashStr"),
        );
    }
    d.finish("seeds");
}
