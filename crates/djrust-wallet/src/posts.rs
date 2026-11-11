//! Short text posts on a snapshot, signed under beats-post.v1.
//! `credentialSubject = {"@type":"BeatsPost","v":1,"snapshotCID":…,"text":…,["parent":<credential CID>]}`.
//! The text policy is enforced at sign time and again at verify time.

use crate::credential::{self, Credential, Verification};
use crate::text_policy::TextPolicy;
use crate::{cid, schema, Error, Failure, Reason};
use serde_json::{json, Value};
use std::sync::OnceLock;

pub fn policy() -> &'static TextPolicy {
    static P: OnceLock<TextPolicy> = OnceLock::new();
    P.get_or_init(|| TextPolicy::from_schema(&schema::BEATS_POST_V1).expect("beats-post.v1 has a text policy"))
}

/// The post payload; errors if anything is out of policy.
pub fn data(snapshot_cid: &str, text: &str, parent: Option<&str>) -> Result<Value, Error> {
    if !cid::is_valid(snapshot_cid) {
        return Err(Error::invalid("snapshotCID is not a CID"));
    }
    if parent.is_some_and(|p| !cid::is_valid(p)) {
        return Err(Error::invalid("parent is not a CID"));
    }
    if let Some(r) = policy().violation(text) {
        return Err(Error::invalid(format!("post text rejected: {r}")));
    }
    let mut d = json!({"@type": "BeatsPost", "v": 1, "snapshotCID": snapshot_cid, "text": text});
    if let Some(p) = parent {
        d["parent"] = Value::String(p.into());
    }
    Ok(d)
}

pub fn check(c: &Credential, now: u64, skew: u64) -> Verification {
    let v = credential::verify_as(c, &schema::BEATS_POST_V1, now, skew)?;
    let o = c.subject().as_object().ok_or(Failure::new(Reason::MalformedData))?;
    const KEYS: [&str; 5] = ["@type", "v", "snapshotCID", "text", "parent"];
    if o.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return Err(Failure::with(Reason::MalformedData, "unknown member"));
    }
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    if s("@type") != Some("BeatsPost") || o.get("v").and_then(Value::as_i64) != Some(1) {
        return Err(Failure::new(Reason::MalformedData));
    }
    s("snapshotCID").filter(|c| cid::is_valid(c)).ok_or(Failure::new(Reason::MalformedData))?;
    match o.get("parent") {
        None | Some(Value::Null) => {}
        Some(_) if s("parent").is_some_and(cid::is_valid) => {}
        Some(_) => return Err(Failure::with(Reason::MalformedData, "parent")),
    }
    let text = s("text").ok_or(Failure::with(Reason::MalformedData, "text"))?;
    if let Some(r) = policy().violation(text) {
        return Err(Failure::with(Reason::PolicyViolation, &r));
    }
    Ok(v)
}
