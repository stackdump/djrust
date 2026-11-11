//! base64url, gzip framing, gunzip of Swift- and JS-made streams, corrupt streams.

mod common;
use common::*;
use djrust::codec;
use djrust::json::JsonValue;

#[test]
fn base64url_and_crc_match_swift() {
    let doc = fixture("codec");
    let mut d = Diffs::default();
    for r in rows(&doc, "gzip") {
        let text = s(r, "text");
        let tag = format!("{} bytes", text.len());
        d.eq(
            &tag,
            codec::base64_url(text.as_bytes()),
            s(r, "b64").to_string(),
        );
        d.eq(&tag, codec::crc32(text.as_bytes()) as i64, i(r, "crc32"));
    }
    for r in rows(&doc, "decode") {
        let got = codec::base64_url_decode(s(r, "in")).map(|b| djrust::cid::hex(&b));
        let want = r.get("hex").and_then(JsonValue::as_str).map(String::from);
        d.eq(format!("decode {:?}", s(r, "in")), got, want);
    }
    for r in rows(&doc, "b64json") {
        let v = JsonValue::parse(s(r, "input")).unwrap();
        let enc = codec::b64url_encode(&v);
        d.eq(s(r, "input"), enc.clone(), s(r, "encoded").to_string());
        let dec = codec::b64url_decode(&enc).map(|v| v.js_stringified());
        d.eq(
            s(r, "input"),
            dec,
            r.get("decoded")
                .and_then(JsonValue::as_str)
                .map(String::from),
        );
    }
    d.finish("base64url");
}

#[test]
fn swift_gzip_streams_inflate_and_rust_framing_matches() {
    let doc = fixture("codec");
    let mut d = Diffs::default();
    let mut identical = 0;
    let gz = rows(&doc, "gzip");
    for r in gz {
        let text = s(r, "text");
        let tag = format!("{} bytes", text.len());
        // Swift's stream decodes here.
        d.eq(
            &tag,
            codec::b64url_to_gunzip(s(r, "z")).ok(),
            Some(text.to_string()),
        );
        // Ours decodes here, and has the same 10-byte header and 8-byte trailer.
        let ours = codec::gzip(text.as_bytes());
        let theirs = codec::base64_url_decode(s(r, "z")).unwrap();
        d.eq(
            &tag,
            codec::gunzip(&ours).ok(),
            Some(text.as_bytes().to_vec()),
        );
        d.eq(
            format!("{tag} header"),
            ours[..10].to_vec(),
            theirs[..10].to_vec(),
        );
        d.eq(
            format!("{tag} trailer"),
            ours[ours.len() - 8..].to_vec(),
            theirs[theirs.len() - 8..].to_vec(),
        );
        if ours == theirs {
            identical += 1;
        }
    }
    eprintln!(
        "gzip: {identical} of {} streams byte-identical to Apple's deflate",
        gz.len()
    );
    d.finish("gzip");
}

#[test]
fn gunzip_accepts_and_refuses_what_swift_does() {
    let doc = fixture("codec");
    let mut d = Diffs::default();
    for r in rows(&doc, "gunzip") {
        let got = codec::b64url_to_gunzip(s(r, "z")).ok();
        let want = r.get("text").and_then(JsonValue::as_str).map(String::from);
        d.eq(
            format!(
                "{} {:?}",
                s(r, "label"),
                &s(r, "z")[..s(r, "z").len().min(24)]
            ),
            got,
            want,
        );
    }
    d.finish("gunzip");
}
