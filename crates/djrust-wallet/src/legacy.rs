//! beats.bitwrap.io's in-band `eth` signing, as its server accepts it today (`BeatsTrack.kt`
//! `BeatsLegacy`): EIP-191 personal_sign over the UTF-8 CID string of the share envelope with
//! `signature` removed (`signer` kept).

use crate::encoding::hex;
use crate::key::{self, EthSignature};
use crate::{cid, Error};
use serde_json::{json, Map, Value};

fn without(o: &Map<String, Value>, drop: &[&str]) -> Map<String, Value> {
    o.iter().filter(|(k, _)| !drop.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Bytes that get personal_sign'd: UTF-8 of the pre-signature CID string.
pub fn signed_bytes(envelope: &Map<String, Value>) -> Result<Vec<u8>, Error> {
    Ok(cid::of_json(&Value::Object(without(envelope, &["signature"])))?.into_bytes())
}

/// Stamps `signer` (lowercase 0x address, as beats does) and `signature`.
pub fn sign(envelope: &Map<String, Value>, priv32: &[u8]) -> Result<Map<String, Value>, Error> {
    let addr = hex::encode0x(&key::address_bytes(&key::public_key(priv32)?));
    let mut stamped = without(envelope, &["signature"]);
    stamped.insert("signer".into(), json!({"type": "eth", "address": addr}));
    let sig = key::sign(&key::personal_message_hash(&signed_bytes(&stamped)?), priv32)?;
    stamped.insert("signature".into(), Value::String(sig.hex()));
    Ok(stamped)
}

/// The lowercase 0x signer address if an `eth` signature verifies, else None.
pub fn verify(envelope: &Map<String, Value>) -> Option<String> {
    let signer = envelope.get("signer")?.as_object()?;
    if signer.get("type")?.as_str()? != "eth" {
        return None;
    }
    let claimed = signer.get("address")?.as_str()?;
    let sig = EthSignature::from_hex(envelope.get("signature")?.as_str()?).ok()?;
    let rec = key::recover_address(&key::personal_message_hash(&signed_bytes(envelope).ok()?), &sig)?;
    let recovered = hex::encode0x(&rec);
    recovered.eq_ignore_ascii_case(claimed).then_some(recovered)
}

/// The share payload without in-band provenance: what a new-style envelope signs.
pub fn payload_of(share: &Map<String, Value>) -> Value {
    Value::Object(without(share, &["signer", "signature", "source"]))
}
