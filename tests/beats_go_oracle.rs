//! `BeatsGoOracleTests`: a second oracle (ROADMAP 2.7). `fixtures/js/go.json` holds what
//! beats' *Go* generator produced for every case the JS fixtures cover
//! (`scripts/beats-go-oracle.mjs`). Go and JS do not agree end to end: Go gives every drum voice
//! MIDI channel 10 (JS: kick 10, snare 11, hihat 12, clap 13) and draws from math/rand where JS
//! uses mulberry32 / the variety.js LCG, so every RNG-driven net differs. What does agree (kick,
//! stingers, the cohesion-v2 bass/melody/harmony/arp nets in most genres, whole projects for
//! some genres) is a three-way check: Rust must equal Go wherever Go equals JS.
//!
//! The generator follows the JS (what shipped share links were built with);
//! `beats_composer.rs` pins it to JS byte for byte. These tests add Go as an independent
//! witness.

mod common;
use common::*;
use djrust::cid;
use djrust::compose;
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::Project;
use std::collections::BTreeSet;

fn sha(s: &str) -> String {
    cid::hex(&cid::sha256(s.as_bytes()))
}

/// Go's view of drum voices: MIDI channels 10-15 collapse to 10.
fn drum_channels_to_10(v: &JsonValue) -> JsonValue {
    let Some(doc) = v.as_object() else {
        return v.clone();
    };
    let Some(nets) = doc.get("nets").and_then(JsonValue::as_object) else {
        return v.clone();
    };
    let mut new_nets = nets.clone();
    for (id, net) in nets.iter() {
        let Some(n) = net.as_object() else { continue };
        let Some(trans) = n.get("transitions").and_then(JsonValue::as_object) else {
            continue;
        };
        let mut new_trans = trans.clone();
        for (tid, t) in trans.iter() {
            let Some(to) = t.as_object() else { continue };
            let Some(midi) = to.get("midi").and_then(JsonValue::as_object) else {
                continue;
            };
            let Some(ch) = midi.get("channel").and_then(JsonValue::as_i64) else {
                continue;
            };
            if !(10..=15).contains(&ch) {
                continue;
            }
            let mut m = midi.clone();
            m.set("channel", JsonValue::int(10));
            let mut t2 = to.clone();
            t2.set("midi", m.into());
            new_trans.set(tid, t2.into());
        }
        let mut n2 = n.clone();
        n2.set("transitions", new_trans.into());
        new_nets.set(id, n2.into());
    }
    let mut out: JsonObject = doc.clone();
    out.set("nets", new_nets.into());
    out.into()
}

fn rust_project(row: &JsonValue) -> Project {
    compose(s(row, "genre"), row.get("overrides").unwrap())
}

fn strings(v: &JsonValue) -> Vec<String> {
    v.strings().unwrap()
}

/// (a) GoOracleTests.goAndJSFixturesCoverTheSameCases
#[test]
fn go_and_js_fixtures_cover_the_same_cases() {
    let doc = js_fixture("go");
    let all = rows(&doc, "cases");
    assert_eq!(all.len(), 542);
    assert_eq!(all.iter().filter(|r| r.get("nets").is_some()).count(), 76);
}

/// (a) GoOracleTests.wholeProjectsMatchGoWhereGoMatchesJS
#[test]
fn whole_projects_match_go_where_go_matches_js() {
    let doc = js_fixture("go");
    let agreeing: Vec<&JsonValue> = rows(&doc, "cases")
        .iter()
        .filter(|r| r.get("agreeModuloDrumChannel").and_then(JsonValue::as_bool) == Some(true))
        .collect();
    assert!(agreeing.len() >= 24, "the oracle must not be vacuous");
    let mut d = Diffs::default();
    for row in &agreeing {
        let project = rust_project(row);
        let mapped = drum_channels_to_10(&project.normalized_for_cid()).canonical_go();
        let go = row.get("go").unwrap();
        d.eq(
            format!("{} cid", s(row, "id")),
            sha(&mapped),
            s(go, "cid").to_string(),
        );
        d.eq(
            format!("{} bytes", s(row, "id")),
            mapped.len() as i64,
            i(go, "bytes"),
        );
    }
    d.finish("whole projects (Go)");
}

/// (a) GoOracleTests.netsGoAndJSAgreeOnMatchSwift (here: match the Rust generator).
#[test]
fn nets_go_and_js_agree_on_match_the_generator() {
    let doc = js_fixture("go");
    let mut compared = 0;
    let mut kinds = BTreeSet::new();
    let mut d = Diffs::default();
    for row in rows(&doc, "cases")
        .iter()
        .filter(|r| r.get("nets").is_some())
    {
        let id = s(row, "id");
        let project_doc = drum_channels_to_10(&rust_project(row).normalized_for_cid());
        let nets = project_doc
            .get("nets")
            .and_then(JsonValue::as_object)
            .unwrap();
        let n = row.get("nets").unwrap();
        for (net_id, prefix) in n.get("same").and_then(JsonValue::as_object).unwrap().iter() {
            let digest = sha(&nets.get(net_id).unwrap().canonical_go());
            d.eq(
                format!("{id} net {net_id}"),
                digest[..16].to_string(),
                prefix.as_str().unwrap().to_string(),
            );
            compared += 1;
            kinds.insert(net_id.to_string());
        }
        // Every net the generator produces is accounted for: same, differing, or Rust-only is
        // a bug.
        let mut accounted: BTreeSet<String> = n
            .get("same")
            .and_then(JsonValue::as_object)
            .unwrap()
            .keys()
            .map(String::from)
            .collect();
        accounted.extend(strings(n.get("differ").unwrap()));
        accounted.extend(strings(n.get("onlyJs").unwrap()));
        d.eq(
            format!("{id} net set"),
            accounted,
            nets.keys().map(String::from).collect::<BTreeSet<_>>(),
        );
    }
    assert!(compared > 500, "{compared}");
    for k in ["kick", "hit1", "hit4", "melody", "bass", "harmony"] {
        assert!(kinds.contains(k), "{k}");
    }
    d.finish("nets (Go)");
}

/// (a) GoOracleTests.theKnownGoDivergencesAreRecorded: if Go is ever fixed to share channels /
/// RNG with JS these counts change and this test says so; update the fixture and the
/// expectations together.
#[test]
fn the_known_go_divergences_are_recorded() {
    let doc = js_fixture("go");
    let all = rows(&doc, "cases");
    assert!(
        all.iter()
            .all(|r| r.get("agree").and_then(JsonValue::as_bool) != Some(true)),
        "no case agrees fully"
    );
    let loops: Vec<&JsonValue> = all.iter().filter(|r| r.get("nets").is_some()).collect();
    // After the drum-channel mapping only RNG-driven nets (and the clap) still differ.
    let allowed: BTreeSet<&str> = ["bass", "melody", "hihat", "clap"].into_iter().collect();
    for r in &loops {
        let n = r.get("nets").unwrap();
        let differ = strings(n.get("differ").unwrap());
        assert!(
            differ.iter().all(|d| allowed.contains(d.as_str())),
            "{}: {differ:?}",
            s(r, "id")
        );
        assert!(!differ.iter().any(|d| d == "snare"), "{}", s(r, "id"));
        assert!(
            n.get("same").and_then(|x| x.get("kick")).is_some(),
            "{}",
            s(r, "id")
        );
        assert!(
            n.get("onlyGo")
                .and_then(JsonValue::as_array)
                .unwrap()
                .is_empty(),
            "{}",
            s(r, "id")
        );
    }
}
