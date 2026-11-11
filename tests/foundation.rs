//! JSON number printing, ToInt32, canonical JSON, RNG streams, base58, CIDs.

mod common;
use common::*;
use djrust::cid;
use djrust::json::{js_number_string, js_to_int32, JsonValue};
use djrust::rng::{Lcg, Mulberry32};

#[test]
fn numbers_print_and_truncate_like_swift() {
    let doc = fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "numbers") {
        let x = f64::from_bits(u64::from_str_radix(s(r, "bits"), 16).unwrap());
        let tag = s(r, "bits");
        d.eq(tag, js_number_string(x), s(r, "str").to_string());
        d.eq(
            tag,
            JsonValue::Number(x).js_stringified(),
            s(r, "json").to_string(),
        );
        d.eq(tag, js_to_int32(x) as i64, i(r, "int32"));
    }
    d.finish("numbers");
}

#[test]
fn rng_streams_match_swift() {
    let doc = fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "rng") {
        let seed = f(r, "seed");
        let mut m = Mulberry32::new(seed);
        for (k, want) in rows(r, "next").iter().enumerate() {
            d.eq(
                format!("mulberry32({seed})[{k}]"),
                bits_hex(m.next()),
                want.as_str().unwrap().to_string(),
            );
        }
        let mut m = Mulberry32::new(seed);
        for (k, want) in rows(r, "int63").iter().enumerate() {
            d.eq(
                format!("int63({seed})[{k}]"),
                m.next_int63(),
                want.as_i64().unwrap(),
            );
        }
        let mut l = Lcg::new(seed);
        for (k, want) in rows(r, "lcg").iter().enumerate() {
            d.eq(
                format!("lcg({seed})[{k}]"),
                bits_hex(l.float64()),
                want.as_str().unwrap().to_string(),
            );
        }
    }
    d.finish("rng");
}

#[test]
fn json_serializations_match_swift() {
    let doc = fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "json") {
        let input = s(r, "input");
        let v = JsonValue::parse(input).unwrap();
        d.eq(input, v.js_stringified(), s(r, "stringified").to_string());
        d.eq(input, v.canonical_js(), s(r, "canonical").to_string());
        d.eq(input, v.canonical_go(), s(r, "canonicalGo").to_string());
    }
    d.finish("json");
}

#[test]
fn base58_and_cids_match_swift() {
    let doc = fixture("foundation");
    let mut d = Diffs::default();
    for r in rows(&doc, "base58") {
        let bytes = unhex(s(r, "hex"));
        let b = cid::encode_base58(&bytes);
        d.eq(s(r, "hex"), b.clone(), s(r, "b58").to_string());
        d.eq(
            s(r, "hex"),
            cid::hex(&cid::decode_base58(&b).unwrap()),
            s(r, "decoded").to_string(),
        );
    }
    for r in rows(&doc, "cids") {
        let input = s(r, "input");
        let h = cid::sha256(input.as_bytes());
        d.eq(input, cid::hex(&h), s(r, "sha256").to_string());
        d.eq(
            input,
            cid::hex(&cid::cid_v1_bytes(&h)),
            s(r, "cidBytes").to_string(),
        );
        d.eq(
            input,
            cid::share_cid_canonical(input),
            s(r, "cid").to_string(),
        );
    }
    assert_eq!(
        cid::decode_base58("0OIl"),
        None,
        "non-alphabet characters are refused"
    );
    d.finish("base58/cids");
}
