//! "Charted by" (`bitwrap-chart-claim.v1`): credit, like naming a comet, for the first member to
//! find a deterministic track. Never ownership: anyone can play, share and remix any track.
//!
//! `credentialSubject = {"@type":"BitwrapChartClaim","v":1,"id":"ipfs://<track CID>",
//! "chartedBy":did,"member":{"height","hash"},"render":{"engine","digest"}}`, issued by the
//! charter, no expiry. `member` names an identity-log row at or after the one that sealed the
//! charter's signup; `render` is the engine that rendered the track and the sha2-256 of its
//! deterministic output. Whether `member` is real, and who was first, is the storage
//! service's to decide (it needs the log).

use crate::credential::{self, Credential, Verified};
use crate::encoding::hex;
use crate::{cid, schema, snapshot, Error, Failure, Reason};
use serde_json::{json, Value};

/// `ipfs://<cid>`: how a credential subject names a CID-addressed document.
pub fn subject_id(track_cid: &str) -> String {
    format!("ipfs://{track_cid}")
}

/// The CID inside an `ipfs://` subject id.
pub fn cid_of(subject_id: &str) -> Option<&str> {
    subject_id.strip_prefix("ipfs://").filter(|c| cid::is_valid(c))
}

pub fn data(track_cid: &str, charted_by: &str, member_height: u64, member_hash: &[u8; 32], engine: &str, render_digest: &[u8; 32]) -> Result<Value, Error> {
    if !cid::is_valid(track_cid) {
        return Err(Error::invalid("track is not a CID"));
    }
    if member_height >= 1 << 53 {
        return Err(Error::invalid("member.height is too large"));
    }
    if !is_engine(engine) {
        return Err(Error::invalid("engine must be name@version"));
    }
    Ok(json!({
        "@type": "BitwrapChartClaim", "v": 1, "id": subject_id(track_cid), "chartedBy": charted_by,
        "member": {"height": member_height, "hash": hex::encode0x(member_hash)},
        "render": {"engine": engine, "digest": hex::encode0x(render_digest)},
    }))
}

/// What a valid claim says, once [check] has passed.
#[derive(Clone, Debug)]
pub struct Claim {
    pub verified: Verified,
    pub track: String,
    pub member_height: u64,
    pub member_hash: [u8; 32],
    pub engine: String,
    pub render_digest: [u8; 32],
}

fn is_lower_hex32(s: &str) -> bool {
    s.len() == 66 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_engine(s: &str) -> bool {
    let Some((name, ver)) = s.split_once('@') else { return false };
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && (1..=32).contains(&ver.len())
        && ver.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
}

/// Schema, signature, window, shape, chartedBy == issuer, no expiry, and `track_doc` is a
/// valid CC0 beats snapshot whose CID the subject names.
pub fn check(c: &Credential, track_doc: &Value, now: u64, skew: u64) -> Result<Claim, Failure> {
    let v = credential::verify_as(c, &schema::BITWRAP_CHART_CLAIM_V1, now, skew)?;
    let bad = |d: &str| Failure::with(Reason::MalformedData, d);
    let o = credential::subject_shape(c, "BitwrapChartClaim", &[&["@type", "chartedBy", "id", "member", "render", "v"]])?;
    let track = o["id"].as_str().and_then(cid_of).ok_or_else(|| bad("id"))?.to_string();
    if o["chartedBy"].as_str() != Some(v.signer_did.as_str()) {
        return Err(Failure::with(Reason::ClaimMismatch, "chartedBy is not the issuer"));
    }
    if c.valid_until() != 0 {
        return Err(Failure::with(Reason::BadWindow, "claims do not expire"));
    }
    let m = o["member"].as_object().ok_or_else(|| bad("member"))?;
    let mut mk: Vec<&str> = m.keys().map(String::as_str).collect();
    mk.sort();
    let height = m.get("height").and_then(Value::as_u64).filter(|h| *h < (1 << 53));
    let mhash = m.get("hash").and_then(Value::as_str).filter(|h| is_lower_hex32(h));
    let (Some(height), Some(mhash), true) = (height, mhash, mk == ["hash", "height"]) else { return Err(bad("member")) };
    let r = o["render"].as_object().ok_or_else(|| bad("render"))?;
    let mut rk: Vec<&str> = r.keys().map(String::as_str).collect();
    rk.sort();
    let engine = r.get("engine").and_then(Value::as_str).filter(|e| is_engine(e));
    let rdig = r.get("digest").and_then(Value::as_str).filter(|h| is_lower_hex32(h));
    let (Some(engine), Some(rdig), true) = (engine, rdig, rk == ["digest", "engine"]) else { return Err(bad("render")) };
    if let Some(why) = snapshot::validate(track_doc) {
        return Err(Failure::with(Reason::SnapshotInvalid, &why));
    }
    if snapshot::cid(track_doc).ok().as_deref() != Some(track.as_str()) {
        return Err(Failure::with(Reason::SnapshotInvalid, "the track document does not hash to the track CID"));
    }
    Ok(Claim {
        verified: v,
        track,
        member_height: height,
        member_hash: hex::decode_n::<32>(mhash).expect("checked"),
        engine: engine.into(),
        render_digest: hex::decode_n::<32>(rdig).expect("checked"),
    })
}
