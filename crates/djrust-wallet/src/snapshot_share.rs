//! A signed whole-session snapshot (`bitwrap-snapshot-share.v1`): "I share this mix". The
//! `DjSnapshot` document (`dj-snapshot.v1`) stays unsigned data, stored and served by its CID
//! (cdn.bitwrap.io `/ipfs/<cid>/snapshot.json`). The credential names it by that CID and carries
//! nothing else, so its size does not grow with the session: a busy mix with eight vocoders signs
//! as small as an empty one.
//!
//! `credentialSubject = {"@type":"BitwrapSnapshotShare","v":1,"id":"ipfs://<DjSnapshot CID>"}`,
//! issued by whoever shares it, with or without an expiry.

use crate::chart::{cid_of, subject_id};
use crate::credential::{self, Credential, Verified};
use crate::{cid, schema, Error, Failure, Reason};
use serde_json::{json, Value};

pub fn data(snapshot_cid: &str) -> Result<Value, Error> {
    if !cid::is_valid(snapshot_cid) {
        return Err(Error::invalid("snapshot is not a CID"));
    }
    Ok(json!({"@type": "BitwrapSnapshotShare", "v": 1, "id": subject_id(snapshot_cid)}))
}

/// What a valid share says, once [check] has passed.
#[derive(Clone, Debug)]
pub struct Share {
    pub verified: Verified,
    /// The CID of the shared `DjSnapshot` document.
    pub snapshot: String,
}

/// None when `doc` looks like a `DjSnapshot` v1, else why not. Only the outer shape: the engine
/// that loads it checks the rest (and every deck's track CID) before touching any state.
pub fn snapshot_violation(doc: &Value) -> Option<String> {
    let Some(o) = doc.as_object() else { return Some("a snapshot is a JSON object".into()) };
    if o.get("@type").and_then(Value::as_str) != Some("DjSnapshot") {
        return Some("@type is not DjSnapshot".into());
    }
    if o.get("v").and_then(Value::as_i64) != Some(1) {
        return Some("v is not 1".into());
    }
    for k in ["decks", "mixer", "setup"] {
        if !o.get(k).is_some_and(Value::is_object) {
            return Some(format!("{k} is missing"));
        }
    }
    None
}

/// Schema, signature, window and shape. With `doc` (the snapshot, fetched or saved), also that
/// it is a `DjSnapshot` and hashes to the CID the share names; without it, the share alone,
/// which is enough to know who shared which CID.
pub fn check(c: &Credential, doc: Option<&Value>, now: u64, skew: u64) -> Result<Share, Failure> {
    let verified = credential::verify_as(c, &schema::BITWRAP_SNAPSHOT_SHARE_V1, now, skew)?;
    let o = credential::subject_shape(c, "BitwrapSnapshotShare", &[&["@type", "id", "v"]])?;
    let snapshot = o["id"].as_str().and_then(cid_of).ok_or_else(|| Failure::with(Reason::MalformedData, "id"))?.to_string();
    if let Some(doc) = doc {
        if let Some(why) = snapshot_violation(doc) {
            return Err(Failure::with(Reason::SnapshotInvalid, &why));
        }
        if cid::of_json(doc).ok().as_deref() != Some(snapshot.as_str()) {
            return Err(Failure::with(Reason::SnapshotInvalid, "the snapshot document does not hash to the shared CID"));
        }
    }
    Ok(Share { verified, snapshot })
}
