//! The generator end to end: `compose(genre, overrides)` must produce Swift's project
//! byte for byte — canonical model JSON (and so the model CID), the full
//! `projectToJSON` document, and the note/control event lists.

mod common;
use common::*;
use djrust::cid;
use djrust::compose;
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::Project;
use djrust::spec::TrackSpec;

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

/// The harness's `events`: each net's bindings in transition order.
fn events(p: &Project) -> JsonValue {
    let mut out = JsonObject::new();
    for (id, nb) in p.nets.iter() {
        let mut evs = Vec::new();
        for tid in nb.transitions.keys() {
            if let Some(m) = nb.bindings.get(tid) {
                evs.push(JsonValue::Array(vec![
                    tid.as_str().into(),
                    m.note.into(),
                    m.channel.into(),
                    m.velocity.into(),
                    m.duration.into(),
                    m.duration_steps.into(),
                ]));
            }
            if let Some(c) = nb.control_bindings.get(tid) {
                evs.push(JsonValue::Array(vec![
                    tid.as_str().into(),
                    c.action.as_str().into(),
                    c.target_net.as_str().into(),
                    c.macro_name
                        .clone()
                        .map(JsonValue::from)
                        .unwrap_or(JsonValue::Null),
                    c.macro_bars.map(JsonValue::from).unwrap_or(JsonValue::Null),
                    c.macro_params.clone().unwrap_or(JsonValue::Null),
                ]));
            }
        }
        out.set(id, JsonValue::Array(evs));
    }
    out.into()
}

fn check(d: &mut Diffs, tag: &str, p: &Project, r: &JsonValue) {
    let canonical = p.canonical_json();
    let doc = p.to_json().js_stringified();
    d.eq(
        format!("{tag} name"),
        p.name.clone(),
        s(r, "name").to_string(),
    );
    d.eq(format!("{tag} nets"), p.nets.len() as i64, i(r, "nets"));
    d.eq(
        format!("{tag} canonical bytes"),
        canonical.len() as i64,
        i(r, "canonicalBytes"),
    );
    d.eq(
        format!("{tag} model CID"),
        p.model_cid(),
        s(r, "modelCID").to_string(),
    );
    d.eq(
        format!("{tag} doc bytes"),
        doc.len() as i64,
        i(r, "docBytes"),
    );
    d.eq(
        format!("{tag} doc sha"),
        sha(&doc),
        s(r, "docSha").to_string(),
    );
    d.eq(
        format!("{tag} initialMutes"),
        p.initial_mutes.clone(),
        r.get("initialMutes").unwrap().strings().unwrap(),
    );
    d.eq(
        format!("{tag} cohesion"),
        p.cohesion.clone(),
        r.get("cohesion")
            .and_then(JsonValue::as_str)
            .map(String::from),
    );
    if let Some(want) = r.get("canonical").and_then(JsonValue::as_str) {
        d.eq(format!("{tag} canonical"), canonical.as_str(), want);
    }
    if let Some(want) = r.get("doc").and_then(JsonValue::as_str) {
        d.eq(format!("{tag} doc"), doc.as_str(), want);
    }
    if let Some(want) = r.get("events") {
        // Per net, so a mismatch names the net.
        let got = events(p);
        for (id, w) in want.as_object().unwrap().iter() {
            let g = got
                .get(id)
                .map(JsonValue::js_stringified)
                .unwrap_or_default();
            d.eq(format!("{tag} events {id}"), g, w.js_stringified());
        }
        d.eq(
            format!("{tag} event nets"),
            got.as_object().unwrap().len(),
            want.as_object().unwrap().len(),
        );
    }
}

#[test]
fn compose_matrix_matches_swift() {
    let doc = fixture("projects");
    let mut d = Diffs::default();
    let cases = rows(&doc, "cases");
    assert_eq!(cases.len(), 525);
    for r in cases {
        let genre = s(r, "genre");
        let ov = r.get("overrides").unwrap();
        let tag = format!("{genre} {}", ov.js_stringified());
        let p = compose(genre, ov);
        check(&mut d, &tag, &p, r);
    }
    d.finish("compose matrix");
}

#[test]
fn track_specs_match_swift() {
    let doc = fixture("projects");
    let mut d = Diffs::default();
    for r in rows(&doc, "specs") {
        let spec = TrackSpec::parse(s(r, "spec")).unwrap();
        d.eq(
            format!("{} overrides", s(r, "spec")),
            spec.overrides().js_stringified(),
            r.get("overrides").unwrap().js_stringified(),
        );
        check(&mut d, s(r, "spec"), &spec.project(), r);
    }
    d.finish("track specs");
}

#[test]
fn the_gate_can_fail() {
    // A different seed must not satisfy a fixture row.
    let doc = fixture("projects");
    let r = &rows(&doc, "specs")[0];
    let mut spec = TrackSpec::parse(s(r, "spec")).unwrap();
    spec.seed += 1.0;
    assert_ne!(spec.project().model_cid(), s(r, "modelCID"));
}

#[test]
fn projects_round_trip_through_their_document() {
    // parseProject(projectToJSON(p)) keeps the model: same canonical JSON and CID.
    for spec in ["synthwave:1984:standard", "jazz:110", "house:7:minimal"] {
        let p = TrackSpec::parse(spec).unwrap().project();
        let back = Project::from_json(&JsonValue::parse(&p.to_json().js_stringified()).unwrap());
        assert_eq!(back.canonical_json(), p.canonical_json(), "{spec}");
        assert_eq!(
            back.to_json().js_stringified(),
            p.to_json().js_stringified(),
            "{spec}"
        );
    }
}
