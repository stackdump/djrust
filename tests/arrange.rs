//! ROADMAP 2.8 gate, in Rust: `arrange_with_opts`, the overlay directives and the share
//! overrides must reproduce beats' JS exactly: node count, canonical model bytes, model CID
//! and the full serialized document, on fixtures written by beats' own JS
//! (`tests/fixtures/js/arrange.json`, scripts/beats-arrange.mjs). A fixture whose JS run
//! threw (`result.error`) must make this throw too.

mod common;
use common::*;
use djrust::cid;
use djrust::compose;
use djrust::gen::arrange::{arrange_with_opts, ArrangeOpts, ArrangeTables};
use djrust::gen::counter_melody::{counter_seed, js_to_fixed6};
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::Project;
use djrust::share::Share;
use std::collections::HashMap;

fn fixture() -> JsonValue {
    js_fixture("arrange")
}

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

/// The JS script's `digest`: node count, canonical bytes / CID and the serialized document.
#[derive(PartialEq, Eq, Debug)]
struct Digest {
    nets: i64,
    counter: i64,
    bytes: i64,
    cid: String,
    doc: String,
}

impl Digest {
    fn of(p: &Project) -> Digest {
        let canonical = p.canonical_json();
        Digest {
            nets: p.nets.len() as i64,
            counter: p
                .nets
                .keys()
                .iter()
                .filter(|k| k.starts_with("counter-melody-"))
                .count() as i64,
            bytes: canonical.len() as i64,
            cid: sha(&canonical),
            doc: sha(&p.to_json().js_stringified()),
        }
    }
    fn expected(r: &JsonValue) -> Digest {
        Digest {
            nets: i(r, "nets"),
            counter: i(r, "counter"),
            bytes: i(r, "canonicalBytes"),
            cid: s(r, "modelCID").to_string(),
            doc: s(r, "docSha").to_string(),
        }
    }
}

fn compose_base(genre: &str, seed: f64, structure: &str, cohesion: &str) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    ov.set("cohesion", cohesion.into());
    if !structure.is_empty() {
        ov.set("structure", structure.into());
    }
    compose(genre, &ov.into())
}

/// Composing is the slow part; the fixtures reuse a few hundred bases.
#[derive(Default)]
struct BaseCache(HashMap<String, Project>);

impl BaseCache {
    fn base(&mut self, genre: &str, seed: f64, structure: &str, cohesion: &str) -> Project {
        let key = format!("{genre}/{seed}/{structure}/{cohesion}");
        self.0
            .entry(key)
            .or_insert_with(|| compose_base(genre, seed, structure, cohesion))
            .clone()
    }
}

/// `wireOf` in the script: the unparsed document the share path hands to arrange.
fn wire_doc(p: &Project, with_mutes: bool) -> JsonValue {
    let mut o = JsonObject::new();
    o.set("name", p.name.as_str().into());
    o.set("tempo", p.tempo.into());
    o.set("swing", p.swing.into());
    o.set("humanize", p.humanize.into());
    o.set("nets", p.to_json().get("nets").unwrap().clone());
    if with_mutes {
        o.set(
            "initialMutes",
            JsonValue::Array(p.initial_mutes.iter().map(|m| m.as_str().into()).collect()),
        );
    }
    o.into()
}

const CHUNKS: usize = 4;

fn slice(rows: &[JsonValue], chunk: usize) -> Vec<(usize, &JsonValue)> {
    rows.iter()
        .enumerate()
        .filter(|(i, _)| i % CHUNKS == chunk)
        .collect()
}

#[test]
fn tables_match_the_js_source() {
    let want = fixture().get("tables").unwrap().js_stringified();
    assert_eq!(ArrangeTables::json().js_stringified(), want);
}

/// JS toFixed rounds the exact binary value half up; printf rounds exact ties to even.
#[test]
fn to_fixed_six_matches_js() {
    let cases: &[(f64, &str)] = &[
        (0.0078125, "0.007813"),
        (0.5, "0.500000"),
        (0.7, "0.700000"),
        (0.0, "0.000000"),
        (1.0, "1.000000"),
        (0.9999995, "1.000000"),
        (0.0000005, "0.000000"),
        (5e-7, "0.000000"),
        (-5e-7, "-0.000000"),
        (0.0000015, "0.000002"),
        (0.1250005, "0.125000"),
        (123456.0078125, "123456.007813"),
        (0.1234565, "0.123456"),
        (0.123456789, "0.123457"),
        (0.33333333, "0.333333"),
        (7.0, "7.000000"),
        (-0.25, "-0.250000"),
        (0.0000004, "0.000000"),
        (1e21, "1e+21"),
        (0.99999951, "1.000000"),
        (0.015625, "0.015625"),
        (0.0234375, "0.023438"),
    ];
    for (x, want) in cases {
        assert_eq!(js_to_fixed6(*x), *want, "{x}");
    }
}

#[test]
fn counter_seed_is_fnv32a() {
    // fnv32a("") and ("a"), the standard vectors.
    assert_eq!(counter_seed(&["".into()]), 0x811c_9dc5);
    assert_eq!(counter_seed(&["a".into()]), 0xe40c_292c);
    assert_eq!(
        counter_seed(&["foo".into(), "bar".into()]),
        counter_seed(&["foobar".into()])
    );
}

/// The fixtures reach every directive in every genre for both cohesion engines, with the JS
/// completing, and the interesting JS throws are part of the oracle.
#[test]
fn fixtures_cover_the_directives() {
    let fxt = fixture();
    let f = &fxt;
    assert!(s(f.get("meta").unwrap(), "beatsCommit").starts_with("668978e"));
    let overlays = rows(&f, "overlays");
    let arranges = rows(&f, "arranges");
    let errors: std::collections::BTreeSet<&str> = overlays
        .iter()
        .chain(arranges)
        .filter_map(|r| r.get("result")?.get("error")?.as_str())
        .collect();
    for e in [
        "nb.resetState is not a function",
        "Cannot read properties of undefined (reading 'map')",
        "Cannot read properties of null (reading 'active')",
        "Cannot read properties of null (reading 'length')",
        "Cannot read properties of null (reading 'section')",
    ] {
        assert!(errors.contains(e), "{e}");
    }
    assert!(
        arranges
            .iter()
            .filter(|r| r.get("result").unwrap().get("error").is_some())
            .count()
            >= 10
    );
}

fn run_overlays(chunk: usize) {
    let fx = fixture();
    let all = rows(&fx, "overlays");
    assert!(all.len() >= 150);
    let mut cache = BaseCache::default();
    let mut d = Diffs::default();
    for (index, row) in slice(all, chunk) {
        let tag = format!(
            "overlays:{index} {}/{}/{}/{} {}",
            s(row, "genre"),
            f(row, "seed"),
            s(row, "structure"),
            s(row, "cohesion"),
            row.get("opts").unwrap().js_stringified()
        );
        let mut proj = cache.base(
            s(row, "genre"),
            f(row, "seed"),
            s(row, "structure"),
            s(row, "cohesion"),
        );
        let mut opts = ArrangeOpts::from_json(row.get("opts").unwrap());
        opts.overlay_only = true;
        let before = row.get("noop").map(|_| Digest::of(&proj));
        let result = arrange_with_opts(&mut proj, s(row, "genre"), "", &opts);
        let want = row.get("result").unwrap();
        let expect_error = want.get("error").is_some();
        d.eq(format!("{tag} threw"), result.is_err(), expect_error);
        if expect_error {
            continue;
        }
        d.eq(
            format!("{tag} digest"),
            Digest::of(&proj),
            Digest::expected(want),
        );
        if let Some(noop) = row.get("noop").and_then(JsonValue::as_str) {
            let got = Digest::of(&proj);
            d.eq(format!("{tag} noop"), got.cid.as_str(), noop);
            d.eq(format!("{tag} noop"), Some(got), before);
        }
    }
    d.finish("overlays");
}

#[test]
fn overlays_match_beats_0() {
    run_overlays(0)
}
#[test]
fn overlays_match_beats_1() {
    run_overlays(1)
}
#[test]
fn overlays_match_beats_2() {
    run_overlays(2)
}
#[test]
fn overlays_match_beats_3() {
    run_overlays(3)
}

/// In the browser `el._project` is plain JSON, where the JS counterMelody throws.
#[test]
fn counter_melody_throws_on_unparsed_nets() {
    let fx = fixture();
    let probes = rows(&fx, "wireProbe");
    assert!(!probes.is_empty());
    for p in probes {
        assert_eq!(s(p, "error"), "nb.resetState is not a function");
        let base = compose_base(s(p, "genre"), f(p, "seed"), "standard", "v2");
        let mut proj = Project::from_wire_json(&wire_doc(&base, false));
        proj.structure = base.structure.clone();
        let mut opts = ArrangeOpts::new();
        opts.overlay_only = true;
        opts.wire_form = true;
        let mut e = JsonObject::new();
        e.set("section", base.structure[0].name.as_str().into());
        e.set("mode", "answer".into());
        opts.counter_melody = vec![e.into()];
        assert!(arrange_with_opts(&mut proj, s(p, "genre"), "", &opts).is_err());
    }
}

fn run_arranges(chunk: usize) {
    let fx = fixture();
    let all = rows(&fx, "arranges");
    assert!(all.len() >= 150);
    let mut cache = BaseCache::default();
    let mut d = Diffs::default();
    for (index, row) in slice(all, chunk) {
        let src = row.get("source").unwrap();
        let genre = s(src, "genre");
        let base = cache.base(genre, f(src, "seed"), "", s(src, "cohesion"));
        let wire = s(row, "form") == "wire";
        let mut proj = if wire {
            Project::from_wire_json(&wire_doc(&base, true))
        } else {
            base
        };
        let mut opts = ArrangeOpts::from_json(row.get("opts").unwrap());
        opts.wire_form = wire;
        // A row may arrange under another genre name than the one it composed.
        let arrange_genre = row
            .get("genre")
            .and_then(JsonValue::as_str)
            .unwrap_or(genre);
        let tag = format!(
            "arranges:{index} {genre}/{}/{arrange_genre}/{}/{} {}",
            f(src, "seed"),
            s(row, "size"),
            s(row, "form"),
            row.get("opts").unwrap().js_stringified()
        );
        let result = arrange_with_opts(&mut proj, arrange_genre, s(row, "size"), &opts);
        let want = row.get("result").unwrap();
        let expect_error = want.get("error").is_some();
        d.eq(format!("{tag} threw"), result.is_err(), expect_error);
        if !expect_error {
            d.eq(
                format!("{tag} digest"),
                Digest::of(&proj),
                Digest::expected(want),
            );
        }
    }
    d.finish("arranges");
}

#[test]
fn arrange_with_opts_matches_beats_0() {
    run_arranges(0)
}
#[test]
fn arrange_with_opts_matches_beats_1() {
    run_arranges(1)
}
#[test]
fn arrange_with_opts_matches_beats_2() {
    run_arranges(2)
}
#[test]
fn arrange_with_opts_matches_beats_3() {
    run_arranges(3)
}

/// Hand-written wire documents: omitted durations (the JS stamp skips them), sparse
/// bindings, a stinger, velocity deltas that find nothing to tweak, a fractional ms, a
/// positive `durationSteps` the parse then drops.
#[test]
fn hand_authored_documents_match_beats() {
    let fx = fixture();
    let docs = rows(&fx, "docs");
    assert_eq!(docs.len(), 12);
    let mut d = Diffs::default();
    for (index, row) in docs.iter().enumerate() {
        let mut proj = Project::from_wire_json(row.get("doc").unwrap());
        let mut opts = ArrangeOpts::from_json(row.get("opts").unwrap());
        opts.wire_form = true;
        let tag = format!("docs:{index} {}", s(row.get("doc").unwrap(), "name"));
        arrange_with_opts(&mut proj, s(row, "genre"), s(row, "size"), &opts)
            .unwrap_or_else(|e| panic!("{tag}: {e}"));
        d.eq(
            tag,
            Digest::of(&proj),
            Digest::expected(row.get("result").unwrap()),
        );
    }
    d.finish("hand-authored documents");
}

/// The gate must be able to fail: arranging a different source must not satisfy a fixture.
#[test]
fn gate_rejects_a_different_source() {
    let fx = fixture();
    let row = rows(&fx, "arranges")
        .iter()
        .find(|r| s(r, "form") == "parsed" && r.get("result").unwrap().get("error").is_none())
        .unwrap();
    let src = row.get("source").unwrap();
    let mut proj = compose_base(
        s(src, "genre"),
        f(src, "seed") + 1.0,
        "",
        s(src, "cohesion"),
    );
    arrange_with_opts(
        &mut proj,
        s(src, "genre"),
        s(row, "size"),
        &ArrangeOpts::from_json(row.get("opts").unwrap()),
    )
    .unwrap();
    assert_ne!(
        Digest::of(&proj),
        Digest::expected(row.get("result").unwrap())
    );
}

#[test]
fn whole_envelopes_build_the_browsers_project() {
    let fx = fixture();
    let all = rows(&fx, "shares");
    let mut cache = BaseCache::default();
    let mut d = Diffs::default();
    let mut raw_nets = 0;
    for (index, row) in all.iter().enumerate() {
        let mut payload = row.get("payload").unwrap().clone();
        if let Some(from) = row.get("netsFrom").filter(|f| **f != JsonValue::Null) {
            let base = cache.base(s(from, "genre"), f(from, "seed"), "", s(from, "cohesion"));
            let mut o = payload.as_object().unwrap().clone();
            o.set("nets", wire_doc(&base, false).get("nets").unwrap().clone());
            payload = o.into();
            raw_nets += 1;
        }
        let cid_text = s(row, "cid");
        let tag = format!(
            "shares:{index} {}",
            row.get("payload").unwrap().js_stringified()
        );
        d.eq(
            format!("{tag} share CID"),
            cid::share_cid(&payload),
            cid_text.to_string(),
        );
        let share = Share::from_payload(&payload, Some(cid_text));
        let project = share.project().unwrap_or_else(|e| panic!("{tag}: {e:?}"));
        d.eq(
            tag,
            Digest::of(&project),
            Digest::expected(row.get("result").unwrap()),
        );
    }
    assert!(raw_nets >= 10);
    d.finish("share envelopes");
}

#[test]
fn overrides_match_the_real_apply_share_overrides() {
    let fx = fixture();
    let all = rows(&fx, "overrides");
    assert_eq!(all.len(), 27);
    let mut cache = BaseCache::default();
    let mut d = Diffs::default();
    let mut fx_only = 0;
    for (index, row) in all.iter().enumerate() {
        let mut proj = cache.base(
            s(row, "genre"),
            f(row, "seed"),
            s(row, "structure"),
            s(row, "cohesion"),
        );
        // The envelope path filters first (shareFromPayload), so go through it.
        let mut payload = JsonObject::new();
        payload.set("genre", s(row, "genre").into());
        for (k, v) in row.get("ov").unwrap().as_object().unwrap().iter() {
            payload.set(k, v.clone());
        }
        let share = Share::from_payload(&payload.into(), None);
        if let Some(ov) = &share.overrides {
            proj.apply_share_overrides(ov);
        }
        let tag = format!(
            "overrides:{index} {} {}",
            s(row, "genre"),
            row.get("ov").unwrap().js_stringified()
        );
        let got = Digest::of(&proj);
        if row
            .get("ov")
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>()
            == ["fx"]
        {
            // `fx` is slider state, not model: the document must be untouched.
            fx_only += 1;
            d.eq(format!("{tag} fx"), got.doc.as_str(), s(row, "plain"));
        }
        d.eq(tag, got, Digest::expected(row.get("result").unwrap()));
    }
    assert_eq!(fx_only, 3);
    d.finish("overrides");
}
