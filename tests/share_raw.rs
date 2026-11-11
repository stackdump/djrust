//! Raw-`nets` envelopes, `TrackSpec` payloads and the pflow edge documents, held to the
//! JS-oracle fixtures (`tests/fixtures/js`: share-urls.json, pflow.json, arrange.json).

mod common;
use common::*;
use djrust::cid;
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::{duration_steps_from_ms, resolve_duration_ms, Project};
use djrust::share::{Share, ShareLink};
use djrust::{compose, TrackSpec};

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

fn num(v: Option<&JsonValue>) -> f64 {
    v.and_then(JsonValue::as_f64).unwrap_or(f64::NAN)
}

#[test]
fn raw_nets_project_matches_what_the_browser_loads() {
    let doc = js_fixture("share-urls");
    let all = rows(&doc, "rawNets");
    assert!(all.len() >= 6);
    let mut d = Diffs::default();
    let (mut built, mut arranged) = (0, 0);
    for r in all {
        let tag = r.get("payload").unwrap().js_stringified();
        let Some(ShareLink::Envelope(share)) = ShareLink::parse(s(r, "search")) else {
            panic!("no envelope: {tag}");
        };
        d.eq(
            format!("{tag} cid"),
            share.cid.clone(),
            Some(s(r, "cid").to_string()),
        );
        d.eq(
            format!("{tag} share"),
            share.to_json().js_stringified(),
            r.get("share").unwrap().js_stringified(),
        );
        let needs = r
            .get("needsArrange")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        d.eq(format!("{tag} needsArrange"), share.needs_arrange(), needs);
        if needs {
            // Arranged: share-urls.json records the boot project before arrange; the
            // arranged one is proven by arrange.json (tests/arrange.rs).
            assert!(share.project().is_ok(), "{tag}");
            arranged += 1;
            continue;
        }
        let want = r.get("project").unwrap();
        let p = share.project().unwrap();
        d.eq(
            format!("{tag} name"),
            p.name.clone(),
            s(want, "name").to_string(),
        );
        d.eq(format!("{tag} tempo"), p.tempo, f(want, "tempo"));
        d.eq(
            format!("{tag} bytes"),
            p.canonical_json().len() as i64,
            i(want, "canonicalBytes"),
        );
        d.eq(
            format!("{tag} cid"),
            p.model_cid(),
            s(want, "modelCID").to_string(),
        );
        d.eq(
            format!("{tag} doc"),
            sha(&p.to_json().js_stringified()),
            s(want, "docSha").to_string(),
        );
        built += 1;
    }
    assert_eq!((built, arranged), (5, 1));
    d.finish("raw-nets projects");
}

/// compose -> projectToJSON -> raw-nets envelope -> share URL -> decode -> same model CID.
#[test]
fn raw_nets_of_a_composed_project_reproduce_its_model_cid() {
    let composed = compose(
        "techno",
        &JsonObject::from_pairs([("seed", 42.0.into()), ("cohesion", "v2".into())]).into(),
    );
    let mut payload = JsonObject::new();
    payload.set("genre", "techno".into());
    payload.set("name", composed.name.as_str().into());
    payload.set("tempo", composed.tempo.into());
    payload.set("swing", composed.swing.into());
    payload.set("humanize", composed.humanize.into());
    payload.set("nets", composed.to_json().get("nets").unwrap().clone());
    let payload: JsonValue = payload.into();
    let forms = ShareLink::forms(&payload, "https://beats.bitwrap.io/");
    let Some(ShareLink::Envelope(share)) = ShareLink::parse(&forms.full) else {
        panic!("no envelope");
    };
    assert_eq!(share.cid.as_deref(), Some(forms.cid.as_str()));
    assert_eq!(share.project().unwrap().model_cid(), composed.model_cid());
}

#[test]
fn derived_name_follows_the_cid_then_the_seed() {
    use djrust::gen::generate_track_name;
    let from_cid = Share::from_payload(
        &JsonValue::parse(r#"{"genre":"house","nets":{}}"#).unwrap(),
        Some("zabc"),
    );
    let mut h: i32 = 5381;
    for u in "zabc".encode_utf16() {
        h = h.wrapping_mul(33) ^ u as i32;
    }
    assert_eq!(
        from_cid.derived_name(),
        generate_track_name("house", h as u32 as f64)
    );
    let no_cid = Share::from_payload(
        &JsonValue::parse(r#"{"genre":"house","seed":77,"nets":{}}"#).unwrap(),
        None,
    );
    assert_eq!(no_cid.derived_name(), generate_track_name("house", 77.0));
    let none = Share::from_payload(&JsonValue::parse(r#"{"nets":{}}"#).unwrap(), None);
    assert_eq!(none.derived_name(), generate_track_name("custom", 1.0));
}

#[test]
fn spec_payloads_write_traits_where_beats_reads_them() {
    let doc = js_fixture("share-urls");
    let all = rows(&doc, "specPayloads");
    assert!(all.len() >= 4);
    let mut d = Diffs::default();
    for r in all {
        let payload = r.get("payload").unwrap();
        let tag = payload.js_stringified();
        let share = Share::from_payload(payload, None);
        let spec = TrackSpec::from_share(&share).unwrap();
        assert!(
            spec.extra.get("traits").is_none() && !spec.extra.is_empty(),
            "{tag}"
        );
        // We write exactly the envelope JS was given...
        d.eq(
            format!("{tag} payload"),
            spec.payload().canonical_js(),
            payload.canonical_js(),
        );
        // ...which JS reads to these params, and composes to the project the deck composes.
        let cid_text = s(r, "cid");
        d.eq(
            format!("{tag} cid"),
            cid::share_cid(&spec.payload()),
            cid_text.to_string(),
        );
        d.eq(
            format!("{tag} share"),
            Share::from_payload(&spec.payload(), Some(cid_text))
                .to_json()
                .js_stringified(),
            r.get("share").unwrap().js_stringified(),
        );
        d.eq(
            format!("{tag} model"),
            compose(&spec.genre, &spec.overrides()).model_cid(),
            s(r, "modelCID").to_string(),
        );
        // The old form (top-level traits): beats composes it without them...
        let old = r.get("oldPayload").unwrap();
        d.eq(
            format!("{tag} old params"),
            Share::from_payload(old, None).params,
            r.get("oldParams").unwrap().as_object().unwrap().clone(),
        );
        assert_ne!(s(r, "oldModelCID"), s(r, "modelCID"));
        // ...and the app reads both forms back to the same spec.
        let from_old = TrackSpec::from_payload(old).unwrap();
        d.eq(
            format!("{tag} old canonical"),
            from_old.payload().canonical_js(),
            payload.canonical_js(),
        );
        d.eq(
            format!("{tag} old extra"),
            from_old.extra.clone(),
            spec.extra.clone(),
        );
        d.eq(
            format!("{tag} roundtrip"),
            TrackSpec::from_payload(&spec.payload()),
            Some(spec),
        );
    }
    d.finish("spec payloads");
}

#[test]
fn old_and_new_session_payloads_round_trip() {
    let mut extra = JsonObject::new();
    extra.set("bpm", 126.0.into());
    extra.set("ghost-notes", 0.3.into());
    let mut layers = JsonObject::new();
    layers.set("tempo", 130.0.into());
    let mut spec = TrackSpec::new("techno", 42.0, Some("standard"));
    spec.extra = extra;
    spec.layers = layers;
    // As sessions recorded before this change wrote it: traits at the top level.
    let old = JsonValue::parse(
        r#"{"genre":"techno","seed":42,"structure":"standard","cohesion":"v2","bpm":126,"ghost-notes":0.3,"tempo":130}"#,
    )
    .unwrap();
    let new = JsonValue::parse(
        r#"{"genre":"techno","seed":42,"structure":"standard","cohesion":"v2","traits":{"bpm":126,"ghost-notes":0.3},"tempo":130}"#,
    )
    .unwrap();
    assert_eq!(spec.payload(), new);
    for form in [&old, &new] {
        assert_eq!(TrackSpec::from_payload(form).as_ref(), Some(&spec));
    }
    // A spec with no extras writes what it always did.
    let plain = TrackSpec::new("jazz", 3.0, None);
    assert_eq!(
        plain.payload().js_stringified(),
        r#"{"genre":"jazz","seed":3,"cohesion":"v2"}"#
    );
    // Keys `overrides` sets itself are not traits.
    let mut shadow = TrackSpec::new("jazz", 3.0, Some("minimal"));
    shadow.extra.set("seed", 9.0.into());
    shadow.extra.set("structure", "x".into());
    shadow.extra.set("bpm", 99.0.into());
    assert_eq!(
        shadow.payload().get("traits").unwrap(),
        &JsonObject::from_pairs([("bpm", 99.0.into())]).into()
    );
    let plain = {
        let mut p = TrackSpec::new("techno", 42.0, Some("standard"));
        p.extra.set("bpm", 126.0.into());
        p
    };
    assert_eq!(
        plain.payload().js_stringified(),
        r#"{"genre":"techno","seed":42,"structure":"standard","cohesion":"v2","traits":{"bpm":126}}"#
    );
    assert_eq!(TrackSpec::from_payload(&plain.payload()), Some(plain));
}

#[test]
fn next_version_is_a_park_miller_step_and_carries_the_sound() {
    let mut spec = TrackSpec::new("house", 42.0, Some("standard"));
    spec.extra.set("bpm", 126.0.into());
    let mut tracks = JsonObject::new();
    tracks.set(
        "5",
        JsonObject::from_pairs([
            ("palette", "synthwave".into()),
            ("instrument", "bass".into()),
        ])
        .into(),
    );
    spec.layers.set("tracks", tracks.into());
    spec.layers.set("tempo", 130.0.into());
    let next = spec.next_version();
    assert_eq!(
        next.seed,
        ((42i64 * 48_271) % 2_147_483_647 % 999_999 + 1) as f64
    );
    assert_eq!(next.genre, "house");
    assert_eq!(next.structure.as_deref(), Some("standard"));
    assert_eq!(next.extra, spec.extra);
    // only the palette rides along: not the instrument pick, not the tempo layer
    let want = JsonObject::from_pairs([(
        "5",
        JsonObject::from_pairs([("palette", "synthwave".into())]).into(),
    )]);
    assert_eq!(next.layers.get("tracks").unwrap().as_object(), Some(&want));
    assert!(next.layers.get("tempo").is_none());
}

/// A spec built from a share envelope plays the project the envelope describes, overrides
/// and overlay included (the JS digest); a recorded session's payload replays the same.
#[test]
fn specs_play_the_envelopes_project() {
    let doc = js_fixture("arrange");
    let all: Vec<&JsonValue> = rows(&doc, "shares")
        .iter()
        .filter(|r| r.get("netsFrom") == Some(&JsonValue::Null))
        .collect();
    let digest = |p: &Project| {
        (
            p.nets.len() as i64,
            p.canonical_json().len() as i64,
            p.model_cid(),
            sha(&p.to_json().js_stringified()),
        )
    };
    let mut d = Diffs::default();
    let mut layered = 0;
    for (index, row) in all.iter().enumerate() {
        let share = Share::from_payload(row.get("payload").unwrap(), Some(s(row, "cid")));
        let spec = TrackSpec::from_share(&share).unwrap();
        let tag = format!(
            "composer share {index} {}",
            row.get("payload").unwrap().js_stringified()
        );
        let r = row.get("result").unwrap();
        let want = (
            i(r, "nets"),
            i(r, "canonicalBytes"),
            s(r, "modelCID").to_string(),
            s(r, "docSha").to_string(),
        );
        d.eq(
            format!("{tag} project"),
            digest(&spec.project()),
            want.clone(),
        );
        let back = TrackSpec::from_payload(&spec.payload()).unwrap();
        d.eq(format!("{tag} spec"), back.clone(), spec.clone());
        d.eq(format!("{tag} back"), digest(&back.project()), want.clone());
        // The session payload is itself an envelope that reads back to the same project.
        d.eq(
            format!("{tag} payload"),
            digest(
                &Share::from_payload(&spec.payload(), None)
                    .project()
                    .unwrap(),
            ),
            want,
        );
        if !spec.layers.is_empty() {
            layered += 1;
        }
    }
    assert!(layered >= 10);
    d.finish("spec projects");
}

#[test]
fn pflow_edge_documents_parse_serialize_and_canonicalize_like_js() {
    let doc = js_fixture("pflow");
    let all = rows(&doc, "docs");
    assert_eq!(all.len(), 7);
    let mut d = Diffs::default();
    for (n, row) in all.iter().enumerate() {
        let project = Project::from_json(row.get("doc").unwrap());
        d.eq(
            format!("projectToJSON doc {n}"),
            project.to_json().js_stringified(),
            s(row, "projectToJSON").to_string(),
        );
        d.eq(
            format!("canonical doc {n}"),
            project.canonical_json(),
            s(row, "canonical").to_string(),
        );
        d.eq(
            format!("modelCID doc {n}"),
            project.model_cid(),
            s(row, "modelCID").to_string(),
        );
        let mut stamped = Project::from_json(row.get("doc").unwrap());
        stamped.stamp_duration_steps();
        d.eq(
            format!("stamped doc {n}"),
            stamped.to_json().js_stringified(),
            s(row, "stampedProjectToJSON").to_string(),
        );
        d.eq(
            format!("stamped CID doc {n}"),
            stamped.model_cid(),
            s(row, "stampedModelCID").to_string(),
        );
        let mut again = stamped.clone();
        again.stamp_duration_steps(); // idempotent
        d.eq(
            format!("idempotent {n}"),
            again.model_cid(),
            stamped.model_cid(),
        );
    }
    d.finish("pflow edge docs");
}

/// compose() output, serialized by JS projectToJSON: parse it, reproduce the same document
/// bytes, and reach the same model CID the composed object had.
#[test]
fn composer_output_round_trips_to_the_same_model_cid() {
    let doc = js_fixture("pflow");
    let all = rows(&doc, "generated");
    assert_eq!(all.len(), 40);
    let mut d = Diffs::default();
    for row in all {
        let tag = format!(
            "{}/{}/{}/{}",
            s(row, "genre"),
            f(row, "seed"),
            s(row, "structure"),
            s(row, "cohesion")
        );
        let jdoc = row.get("doc").unwrap();
        d.eq(
            format!("doc {tag}"),
            sha(&jdoc.js_stringified()),
            s(row, "docSha").to_string(),
        );
        let project = Project::from_json(jdoc);
        d.eq(
            format!("roundtrip {tag}"),
            sha(&project.to_json().js_stringified()),
            s(row, "roundTripSha").to_string(),
        );
        d.eq(
            format!("modelCID {tag}"),
            project.model_cid(),
            s(row, "modelCID").to_string(),
        );
        d.eq(
            format!("viaCompose {tag}"),
            project.model_cid(),
            s(row, "viaCompose").to_string(),
        );
    }
    d.finish("generated pflow docs");
}

#[test]
fn durations_steps_and_resolves() {
    let doc = js_fixture("pflow");
    let mut d = Diffs::default();
    for r in rows(&doc, "durations") {
        d.eq(
            format!("ms {} bpm {}", num(r.get("ms")), num(r.get("bpm"))),
            duration_steps_from_ms(num(r.get("ms")), num(r.get("bpm"))),
            i(r, "steps"),
        );
    }
    for r in rows(&doc, "resolves") {
        let midi = r.get("midi");
        if midi == Some(&JsonValue::Null) {
            d.eq("null midi", f(r, "ms"), 0.0);
            continue;
        }
        let got = resolve_duration_ms(
            midi.and_then(|m| m.get("durationSteps"))
                .and_then(JsonValue::as_f64),
            midi.and_then(|m| m.get("duration"))
                .and_then(JsonValue::as_f64),
            num(r.get("bpm")),
        );
        d.eq(
            format!("{:?}", midi.map(|m| m.js_stringified())),
            got,
            f(r, "ms"),
        );
    }
    d.finish("durations");
}
