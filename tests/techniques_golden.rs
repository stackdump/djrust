//! Swift goldens for the dj-only techniques (`tools/paritydump`, section "techniques"),
//! `tests/fixtures/swift/techniques.json`. `tests/techniques.rs` holds the Swift suite's own
//! expectations.

mod common;
use common::*;
use djrust::gen::techniques::{build_hook, nearest_pitch, render_hook, thin_cell};
use djrust::gen::theme::build_track_theme;
use djrust::gen::BeatsTables;
use djrust::json::JsonValue;

fn cell_json(c: &djrust::gen::theme::MotifCell) -> String {
    use djrust::json::JsonObject;
    JsonValue::Object(JsonObject::from_pairs([
        (
            "degrees",
            JsonValue::Array(c.degrees.iter().map(|d| JsonValue::int(*d)).collect()),
        ),
        (
            "mask",
            JsonValue::Array(c.mask.iter().map(|m| JsonValue::Bool(*m)).collect()),
        ),
        ("contour", JsonValue::int(c.contour)),
    ]))
    .js_stringified()
}

#[test]
fn techniques_match_swift() {
    let doc = fixture("techniques");
    let mut d = Diffs::default();
    for r in rows(&doc, "cases") {
        let p = djrust::compose(s(r, "genre"), r.get("overrides").unwrap());
        let tag = r.get("overrides").unwrap().js_stringified();
        d.eq(
            format!("{tag} name"),
            p.name.clone(),
            s(r, "name").to_string(),
        );
        d.eq(format!("{tag} nets"), p.nets.len() as i64, i(r, "nets"));
        d.eq(
            format!("{tag} model"),
            p.model_cid(),
            s(r, "modelCID").to_string(),
        );
        let sha = |t: &str| djrust::cid::hex(&djrust::cid::sha256(t.as_bytes()));
        d.eq(
            format!("{tag} doc"),
            sha(&p.to_json().js_stringified()),
            s(r, "docSha").to_string(),
        );
    }
    let tables = BeatsTables::shared();
    for r in rows(&doc, "hooks") {
        let genre = s(r, "genre");
        let (seed, bars) = (f(r, "seed"), i(r, "bars"));
        let h = build_hook(genre, seed, bars, tables.genres[genre].melody_density);
        let tag = format!("{genre}/{seed}/{bars}");
        d.eq(
            format!("{tag} mask"),
            JsonValue::Array(h.mask.iter().map(|m| JsonValue::Bool(*m)).collect()),
            r.get("mask").unwrap().clone(),
        );
        d.eq(
            format!("{tag} target"),
            JsonValue::Array(h.target.iter().map(|t| JsonValue::int(*t)).collect()),
            r.get("target").unwrap().clone(),
        );
        let plan = build_track_theme(genre, seed).plan;
        for m in r.get("renders").and_then(JsonValue::as_array).unwrap() {
            d.eq(
                format!("{tag} render {}", i(m, "mode")),
                cell_json(&render_hook(&h, i(m, "mode"), &plan)),
                m.get("cell").unwrap().js_stringified(),
            );
        }
    }
    let theme = build_track_theme("edm", 4242.0);
    for r in rows(&doc, "thin") {
        d.eq(
            format!("thin {}", f(r, "density")),
            cell_json(&thin_cell(&theme.motif, f(r, "density"))),
            r.get("cell").unwrap().js_stringified(),
        );
    }
    for r in rows(&doc, "pitches") {
        let pcs: Vec<i64> = r.get("pcs").unwrap().ints().unwrap();
        d.eq(
            format!("pitch {:?} {}", pcs, i(r, "note")),
            nearest_pitch(i(r, "note"), &pcs),
            i(r, "out"),
        );
    }
    d.finish("techniques");
}
