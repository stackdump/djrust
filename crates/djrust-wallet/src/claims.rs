//! Discovery claims (the check half): who looked first, never ownership.
//! `credentialSubject = {"@type":"BeatsDiscoveryClaim","v":1,"snapshotCID":…,"discoverer":did}`,
//! issued by the discoverer, no expiry. (The storage service takes `bitwrap-chart-claim.v1`
//! instead; see [crate::chart].)

use crate::credential::{self, Credential, Verification};
use crate::{cid, schema, Failure, Reason};
use serde_json::{json, Value};

pub fn data(snapshot_cid: &str, discoverer_did: &str) -> Value {
    json!({"@type": "BeatsDiscoveryClaim", "v": 1, "snapshotCID": snapshot_cid, "discoverer": discoverer_did})
}

/// Schema, signature, window, shape, discoverer == issuer, no expiry.
pub fn check(c: &Credential, now: u64, skew: u64) -> Verification {
    let v = credential::verify_as(c, &schema::BEATS_DISCOVERY_CLAIM_V1, now, skew)?;
    let o = credential::subject_shape(c, "BeatsDiscoveryClaim", &[&["@type", "discoverer", "snapshotCID", "v"]])?;
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    let malformed = || Failure::new(Reason::MalformedData);
    let (Some(snapshot_cid), Some(discoverer)) = (s("snapshotCID"), s("discoverer")) else { return Err(malformed()) };
    if !cid::is_valid(snapshot_cid) {
        return Err(malformed());
    }
    if discoverer != v.signer_did {
        return Err(Failure::with(Reason::ClaimMismatch, "discoverer is not the issuer"));
    }
    if c.valid_until() != 0 {
        return Err(Failure::with(Reason::BadWindow, "claims do not expire"));
    }
    Ok(v)
}

/// The snapshot a checked claim is about.
pub fn snapshot_cid(c: &Credential) -> Option<&str> {
    c.subject().get("snapshotCID")?.as_str()
}
