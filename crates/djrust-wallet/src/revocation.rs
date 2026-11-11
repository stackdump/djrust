//! Pre-signed revocation (`bitwrap-revocation.v1`): an identity revokes itself. Signed while
//! the fresh key is still in memory, kept by the user, submitted only if the key is lost or
//! stolen. It names no successor, so whoever holds it can only end the identity, never take it
//! over. `credentialSubject = {"@type":"BitwrapRevocation","v":1,"id":did}`, issued by that
//! same did, no expiry.

use crate::credential::{self, Credential, Verification};
use crate::{schema, Failure, Reason};
use serde_json::{json, Value};

pub fn data(did: &str) -> Value {
    json!({"@type": "BitwrapRevocation", "v": 1, "id": did})
}

/// Schema, signature, window, shape, subject == issuer, no expiry.
pub fn check(c: &Credential, now: u64, skew: u64) -> Verification {
    let v = credential::verify_as(c, &schema::BITWRAP_REVOCATION_V1, now, skew)?;
    let o = credential::subject_shape(c, "BitwrapRevocation", &[&["@type", "id", "v"]])?;
    if o.get("id").and_then(Value::as_str) != Some(v.signer_did.as_str()) {
        return Err(Failure::with(Reason::ClaimMismatch, "a revocation is issued by the identity it revokes"));
    }
    if c.valid_until() != 0 {
        return Err(Failure::with(Reason::BadWindow, "revocations do not expire"));
    }
    Ok(v)
}
