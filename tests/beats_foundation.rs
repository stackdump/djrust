//! `BeatsFoundationTests`: the foundation (JS number printing, JSON, mulberry32, base58, CIDs,
//! share envelopes, stored projects) against what beats' own JavaScript produced
//! (`fixtures/js/{foundation,envelopes,projects,modules}.json`, from `scripts/beats-parity.mjs`),
//! never against Swift. `foundation.rs` and `share.rs` hold the same pieces to Swift's dump.

mod common;
use common::*;
use djrust::cid;
use djrust::json::{js_number_string, js_to_int32, JsonValue};
use djrust::rng::Mulberry32;
use djrust::share::Share;

fn bits(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

/// (a) JSNumberTests.numberToStringMatchesJS: 642 doubles, their `String(x)`, `JSON.stringify`
/// and `x | 0`.
#[test]
fn number_to_string_matches_js() {
    let doc = js_fixture("foundation");
    let all = rows(&doc, "numbers");
    assert!(all.len() > 500);
    let mut d = Diffs::default();
    for r in all {
        let x = bits(s(r, "bits"));
        let tag = s(r, "bits");
        d.eq(tag, js_number_string(x), s(r, "str").to_string());
        // JSON.stringify differs from String() only for non-finite values (null)
        d.eq(
            tag,
            JsonValue::Number(x).js_stringified(),
            s(r, "json").to_string(),
        );
        d.eq(tag, js_to_int32(x) as f64, f(r, "int32"));
    }
    d.finish("numbers");
}

/// (a) JSNumberTests.knownFormats
#[test]
fn known_formats() {
    assert_eq!(js_number_string(1e21), "1e+21");
    assert_eq!(js_number_string(1e-7), "1e-7");
    assert_eq!(js_number_string(0.000001), "0.000001");
    assert_eq!(
        js_number_string(123_456_789_012_345_680_000.0),
        "123456789012345680000"
    );
    assert_eq!(js_number_string(-0.0), "0");
    assert_eq!(js_number_string(0.1 + 0.2), "0.30000000000000004");
}

/// (a) JSONTests.parseStringifyCanonicalMatchJS
#[test]
fn parse_stringify_canonical_match_js() {
    let doc = js_fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "json") {
        let input = s(r, "input");
        let v = JsonValue::parse(input).unwrap_or_else(|e| panic!("{input}: {e:?}"));
        d.eq(
            format!("stringify {input}"),
            v.js_stringified(),
            s(r, "stringified").to_string(),
        );
        d.eq(
            format!("canonical {input}"),
            v.canonical_js(),
            s(r, "canonical").to_string(),
        );
        d.eq(
            format!("go {input}"),
            v.canonical_go(),
            s(r, "canonicalGo").to_string(),
        );
    }
    d.finish("json");
}

/// (a) JSONTests.integerKeysComeFirstLikeJS
#[test]
fn integer_keys_come_first_like_js() {
    let v = JsonValue::parse(r#"{"b":1,"2":"x","a":2,"1":"y"}"#).unwrap();
    assert_eq!(v.js_stringified(), r#"{"1":"y","2":"x","b":1,"a":2}"#);
}

/// (a) JSONTests.rejectsMalformed
#[test]
fn rejects_malformed() {
    for bad in [
        "", "{", "[1,]", "{\"a\":}", "01", "1.", "\"\\x\"", "nul", "[1] 2",
    ] {
        assert!(JsonValue::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

/// (a) RNGTests.mulberry32MatchesCreateRng: `next()` bit for bit, `nextInt(7)` and the 63-bit
/// integers.
#[test]
fn mulberry32_matches_create_rng() {
    let doc = js_fixture("foundation");
    let all = rows(&doc, "rng");
    assert!(all.len() >= 16);
    let mut d = Diffs::default();
    for r in all {
        let seed = f(r, "seed");
        let mut a = Mulberry32::new(seed);
        for (k, want) in rows(r, "next").iter().enumerate() {
            d.eq(
                format!("seed {seed} next[{k}]"),
                a.next().to_bits(),
                bits(want.as_str().unwrap()).to_bits(),
            );
        }
        let mut b = Mulberry32::new(seed);
        d.eq(
            format!("seed {seed} nextInt(7)"),
            rows(r, "nextInt7")
                .iter()
                .map(|_| b.next_int(7) as i64)
                .collect::<Vec<_>>(),
            rows(r, "nextInt7")
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect::<Vec<_>>(),
        );
        let mut c = Mulberry32::new(seed);
        d.eq(
            format!("seed {seed} nextInt63"),
            rows(r, "nextInt63")
                .iter()
                .map(|_| c.next_int63())
                .collect::<Vec<_>>(),
            rows(r, "nextInt63")
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect::<Vec<_>>(),
        );
    }
    d.finish("mulberry32");
}

/// (a) CIDTests.base58MatchesJS
#[test]
fn base58_matches_js() {
    let doc = js_fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "base58") {
        let raw = unhex(s(r, "hex"));
        let text = s(r, "b58");
        d.eq(s(r, "hex"), cid::encode_base58(&raw), text.to_string());
        d.eq(s(r, "hex"), cid::decode_base58(text), Some(raw));
    }
    d.finish("base58");
}

/// (a) CIDTests.cidV1MatchesJS
#[test]
fn cid_v1_matches_js() {
    let doc = js_fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "cids") {
        let input = s(r, "input");
        let hash = cid::sha256(input.as_bytes());
        d.eq(input, cid::hex(&hash), s(r, "sha256").to_string());
        d.eq(
            input,
            cid::hex(&cid::cid_v1_bytes(&hash)),
            s(r, "cidBytes").to_string(),
        );
        d.eq(
            input,
            cid::share_cid_canonical(input),
            s(r, "cid").to_string(),
        );
    }
    d.finish("cids");
}

/// (a) CIDTests.shareEnvelopesCanonicalizeAndHashLikeBeats, and beyond the Swift test: the
/// `share` field is beats' own `shareFromPayload` of the payload, so `Share::from_payload`
/// is held to it too.
#[test]
fn share_envelopes_canonicalize_and_hash_like_beats() {
    let doc = js_fixture("envelopes");
    let all = rows(&doc, "envelopes");
    assert!(all.len() >= 10);
    let mut d = Diffs::default();
    for r in all {
        let payload = r.get("payload").unwrap();
        let tag = payload.js_stringified();
        d.eq(
            format!("{tag} canonical"),
            payload.canonical_js(),
            s(r, "canonical").to_string(),
        );
        d.eq(
            format!("{tag} cid"),
            cid::share_cid(payload),
            s(r, "cid").to_string(),
        );
        d.eq(
            format!("{tag} shareFromPayload"),
            Share::from_payload(payload, Some(s(r, "cid")))
                .to_json()
                .js_stringified(),
            r.get("share").unwrap().js_stringified(),
        );
    }
    d.finish("envelopes");
}

/// (a) CIDTests.storedProjectsRoundTripToTheirModelCID: re-serialising the parsed model
/// reproduces JS's bytes exactly (sort order, number formatting, Go escaping).
#[test]
fn stored_projects_round_trip_to_their_model_cid() {
    let doc = js_fixture("projects");
    let all = rows(&doc, "projects");
    assert_eq!(all.len(), 19 * 4 * 2 * 2);
    let mut full = 0;
    for row in all {
        let Some(canonical) = row.get("canonical").and_then(JsonValue::as_str) else {
            continue;
        };
        full += 1;
        let parsed = JsonValue::parse(canonical).unwrap();
        assert_eq!(parsed.canonical_go(), canonical);
        assert_eq!(canonical.len() as i64, i(row, "canonicalBytes"));
        assert_eq!(cid::model_cid(canonical), s(row, "modelCID"));
    }
    assert_eq!(full, 3);
}

/// (a) CIDTests.moduleVectorsAreWrittenForTheComposerGates
#[test]
fn module_vectors_are_written_for_the_composer_gates() {
    let m = js_fixture("modules");
    assert!(!rows(&m, "bjorklund").is_empty());
    assert_eq!(rows(&m, "structures").len(), 19 * 8 * 2);
    assert_eq!(rows(&m, "themes").len(), 19 * 4);
    assert!(!rows(&m, "motifs").is_empty());
}
