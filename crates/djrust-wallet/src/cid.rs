//! Content identifiers, byte-compatible with beats.bitwrap.io and djengine `cid::share_cid`:
//! CIDv1 | dag-json (0xa9 0x02) | sha2-256 multihash, base58btc, multibase `z` (`Cid.kt`).

use crate::{encoding::base58, jcs, Error};
use serde_json::Value;
use sha2::{Digest, Sha256};

const PREFIX: [u8; 5] = [0x01, 0xa9, 0x02, 0x12, 0x20];

/// CID of already-canonical bytes.
pub fn of_bytes(canonical: &[u8]) -> String {
    let mut raw = PREFIX.to_vec();
    raw.extend_from_slice(&Sha256::digest(canonical));
    format!("z{}", base58::encode(&raw))
}

/// CID of a JSON document: JCS first, then hash.
pub fn of_json(v: &Value) -> Result<String, Error> {
    Ok(of_bytes(&jcs::bytes(v)?))
}

pub fn of_json_str(json: &str) -> Result<String, Error> {
    Ok(of_bytes(jcs::canonicalize_str(json)?.as_bytes()))
}

/// The sha2-256 digest inside `cid`, or None if it is not a CID of this shape.
pub fn digest(cid: &str) -> Option<[u8; 32]> {
    let raw = base58::decode(cid.strip_prefix('z')?).ok()?;
    if raw.len() != PREFIX.len() + 32 || raw[..PREFIX.len()] != PREFIX {
        return None;
    }
    raw[PREFIX.len()..].try_into().ok()
}

pub fn is_valid(cid: &str) -> bool {
    digest(cid).is_some()
}
