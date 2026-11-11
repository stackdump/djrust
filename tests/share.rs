//! Share envelopes (canonical JSON, CID, shareFromPayload) and share-link parsing.

mod common;
use common::*;
use djrust::codec;
use djrust::json::JsonValue;
use djrust::share::{Share, ShareLink};

#[test]
fn envelopes_match_swift() {
    let doc = fixture("envelopes");
    let mut d = Diffs::default();
    for r in rows(&doc, "envelopes") {
        let p = r.get("payload").unwrap();
        let tag = s(r, "canonical");
        let forms = ShareLink::forms(p, "https://beats.bitwrap.io/");
        d.eq(tag, p.canonical_js(), s(r, "canonical").to_string());
        d.eq(tag, forms.cid.clone(), s(r, "cid").to_string());
        d.eq(tag, forms.short.clone(), s(r, "short").to_string());
        // The `z` deflate bytes may differ from Apple's; everything up to it must not,
        // and our `z` must carry the canonical payload.
        let (ours_head, ours_z) = forms.full.split_once("&z=").unwrap();
        let (want_head, _) = s(r, "full").split_once("&z=").unwrap();
        d.eq(tag, ours_head.to_string(), want_head.to_string());
        d.eq(
            tag,
            codec::b64url_to_gunzip(ours_z).ok(),
            Some(p.canonical_js()),
        );
        // Swift's full URL parses here. A URL carries the payload in canonical key
        // order, so it reads back as the share of the canonical payload.
        let got = match ShareLink::parse(s(r, "full")) {
            Some(ShareLink::Envelope(sh)) => sh.to_json().js_stringified(),
            other => format!("{other:?}"),
        };
        let canonical = JsonValue::parse(s(r, "canonical")).unwrap();
        let want = Share::from_payload(&canonical, Some(&forms.cid))
            .to_json()
            .js_stringified();
        d.eq(tag, got, want);
        let share = Share::from_payload(p, Some(&forms.cid));
        d.eq(
            tag,
            share.to_json().js_stringified(),
            r.get("share").unwrap().js_stringified(),
        );
    }
    d.finish("envelopes");
}

#[test]
fn share_links_parse_like_swift() {
    let doc = fixture("envelopes");
    let mut d = Diffs::default();
    for r in rows(&doc, "links") {
        let url = s(r, "url");
        let want_kind = r.get("kind").and_then(JsonValue::as_str);
        let (kind, detail) = match ShareLink::parse(url) {
            Some(ShareLink::Envelope(sh)) => (Some("envelope"), sh.to_json().js_stringified()),
            Some(ShareLink::NeedsFetch { cid, legacy }) => (
                Some("needsFetch"),
                format!(
                    "{cid} {}",
                    legacy.unwrap_or(JsonValue::Null).js_stringified()
                ),
            ),
            Some(ShareLink::Legacy(v)) => (Some("legacy"), v.js_stringified()),
            None => (None, String::new()),
        };
        d.eq(url, kind, want_kind);
        let want = match want_kind {
            Some("envelope") => r.get("share").unwrap().js_stringified(),
            Some("needsFetch") => format!(
                "{} {}",
                s(r, "cid"),
                r.get("legacy").unwrap().js_stringified()
            ),
            Some("legacy") => r.get("legacy").unwrap().js_stringified(),
            _ => String::new(),
        };
        d.eq(url, detail, want);
    }
    d.finish("share links");
}

#[test]
fn a_tampered_payload_is_refused() {
    let p = JsonValue::parse(r#"{"genre":"techno","seed":42}"#).unwrap();
    let forms = ShareLink::forms(&p, "https://beats.bitwrap.io/");
    assert!(matches!(
        ShareLink::parse(&forms.full),
        Some(ShareLink::Envelope(_))
    ));
    let other = codec::gzip_to_b64url(r#"{"genre":"techno","seed":43}"#);
    let forged = format!("https://beats.bitwrap.io/?cid={}&z={other}", forms.cid);
    assert!(
        ShareLink::parse(&forged).is_none(),
        "a payload that doesn't hash to its cid must not load"
    );
    assert!(Share::decode(r#"{"genre":"techno","seed":43}"#, Some(&forms.cid)).is_err());
}
