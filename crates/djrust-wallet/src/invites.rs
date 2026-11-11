//! Bearer invite codes (`bitwrap-invite.v1`): a member vouches for whoever redeems the code.
//! The credential's subject is `{"@type":"BitwrapInvite","v":1,"inviter":did,"code":"0x…16 bytes",
//! "securedIn"?:{"height":seq,"hash":"0x…"}}`, issued by the inviter. The redeemer's countersign
//! is [accept_data] / [check_accept] (`bitwrap-invite-accept.v1`); single use is the storage
//! service's rule.

use crate::credential::{self, Credential, Verification};
use crate::encoding::hex;
use crate::{schema, Error, Failure, Reason};
use serde_json::{json, Value};

/// The root-log row that sealed the inviter's membership: its seq and its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecuredIn {
    pub height: u64,
    pub hash: [u8; 32],
}

/// The largest height JSON carries exactly (2^53 - 1).
const MAX_HEIGHT: u64 = (1 << 53) - 1;

/// 16 fresh random bytes.
pub fn new_code() -> Result<[u8; 16], Error> {
    let mut c = [0u8; 16];
    getrandom::getrandom(&mut c).map_err(|e| Error::invalid(format!("no randomness: {e}")))?;
    Ok(c)
}

pub fn data(inviter_did: &str, code: &[u8; 16], secured: Option<&SecuredIn>) -> Result<Value, Error> {
    let mut d = json!({"@type": "BitwrapInvite", "v": 1, "inviter": inviter_did, "code": hex::encode0x(code)});
    if let Some(s) = secured {
        if s.height > MAX_HEIGHT {
            return Err(Error::invalid("securedIn.height is too large"));
        }
        d["securedIn"] = json!({"height": s.height, "hash": hex::encode0x(&s.hash)});
    }
    Ok(d)
}

fn is_lower_hex(s: &str, bytes: usize) -> bool {
    s.len() == 2 + 2 * bytes && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The invite's code and securedIn, once [check] has passed.
pub fn parts(data: &Value) -> Option<(String, Option<SecuredIn>)> {
    let code = data.get("code")?.as_str()?.to_string();
    let secured = match data.get("securedIn") {
        None => None,
        Some(s) => Some(SecuredIn {
            height: s.get("height")?.as_u64()?,
            hash: hex::decode_n::<32>(s.get("hash")?.as_str()?).ok()?,
        }),
    };
    Some((code, secured))
}

/// Schema, signature, window, the shape, and inviter == issuer.
pub fn check(c: &Credential, now: u64, skew: u64) -> Verification {
    let v = credential::verify_as(c, &schema::BITWRAP_INVITE_V1, now, skew)?;
    let malformed = || Failure::new(Reason::MalformedData);
    let o = credential::subject_shape(
        c,
        "BitwrapInvite",
        &[&["@type", "code", "inviter", "v"], &["@type", "code", "inviter", "securedIn", "v"]],
    )?;
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    if !s("code").is_some_and(|c| is_lower_hex(c, 16)) {
        return Err(malformed());
    }
    if let Some(sec) = o.get("securedIn") {
        let so = sec.as_object().ok_or_else(malformed)?;
        let mut sk: Vec<&str> = so.keys().map(String::as_str).collect();
        sk.sort();
        let height_ok = so.get("height").and_then(Value::as_u64).is_some_and(|h| h <= MAX_HEIGHT);
        let hash_ok = so.get("hash").and_then(Value::as_str).is_some_and(|h| is_lower_hex(h, 32));
        if sk != ["hash", "height"] || !height_ok || !hash_ok {
            return Err(malformed());
        }
    }
    if s("inviter") != Some(v.signer_did.as_str()) {
        return Err(Failure::with(Reason::ClaimMismatch, "inviter is not the issuer"));
    }
    Ok(v)
}

/// The redeemer's half (`bitwrap-invite-accept.v1`): `member` accepts the invite whose
/// credential CID is `invite_cid`, issued by `inviter_did`. Accepts do not expire.
pub fn accept_data(invite_cid: &str, inviter_did: &str, member_did: &str) -> Result<Value, Error> {
    if member_did == inviter_did {
        return Err(Error::invalid("an invite cannot be redeemed by the identity that signed it"));
    }
    Ok(json!({"@type": "BitwrapInviteAccept", "v": 1, "invite": invite_cid, "inviter": inviter_did, "member": member_did}))
}

/// An accept checked with its invite: both credentials, both shapes, and the edge. The accept
/// names this invite (its credential CID) and its issuer, the member issued the accept, and the
/// member is not the inviter. Returns the accept's verification (signer = the member).
pub fn check_accept(accept: &Credential, invite: &Credential, now: u64, skew: u64) -> Verification {
    let inv = check(invite, now, skew)?;
    let v = credential::verify_as(accept, &schema::BITWRAP_INVITE_ACCEPT_V1, now, skew)?;
    let o = credential::subject_shape(accept, "BitwrapInviteAccept", &[&["@type", "invite", "inviter", "member", "v"]])?;
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    let invite_cid = invite.cid();
    if s("invite") != Some(invite_cid.as_str()) {
        return Err(Failure::with(Reason::ClaimMismatch, "the accept names a different invite"));
    }
    if s("inviter") != Some(inv.signer_did.as_str()) {
        return Err(Failure::with(Reason::ClaimMismatch, "inviter is not the invite's issuer"));
    }
    let member = s("member").ok_or(Failure::new(Reason::MalformedData))?;
    if member != v.signer_did {
        return Err(Failure::with(Reason::ClaimMismatch, "member is not the issuer"));
    }
    if member == inv.signer_did {
        return Err(Failure::with(Reason::ClaimMismatch, "an inviter cannot redeem their own invite"));
    }
    if accept.valid_until() != 0 {
        return Err(Failure::with(Reason::BadWindow, "accepts do not expire"));
    }
    Ok(v)
}
