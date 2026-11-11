//! secp256k1 for Ethereum identities (`EthKey.kt`): keygen, RFC 6979 low-S signing, ecrecover,
//! EIP-55 and did:pkh. The core never stores a key; callers pass the secret for one call.

use crate::encoding::{hex, keccak, keccak_parts};
use crate::Error;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

/// A 65-byte Ethereum signature: r(32) | s(32) | v (27 or 28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthSignature(pub [u8; 65]);

impl EthSignature {
    pub fn v(&self) -> u8 {
        self.0[64]
    }
    pub fn hex(&self) -> String {
        hex::encode0x(&self.0)
    }
    pub fn from_hex(s: &str) -> Result<Self, Error> {
        let b = hex::decode(s)?;
        let a: [u8; 65] = b.try_into().map_err(|_| Error::invalid("signature must be 65 bytes"))?;
        Ok(Self(a))
    }
    pub fn from_slice(b: &[u8]) -> Result<Self, Error> {
        let a: [u8; 65] = b.try_into().map_err(|_| Error::invalid("signature must be 65 bytes"))?;
        Ok(Self(a))
    }
}

const HALF_N: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

fn signing_key(priv32: &[u8]) -> Result<SigningKey, Error> {
    SigningKey::from_slice(priv32).map_err(|_| Error::invalid("not a valid secp256k1 private key"))
}

/// Fresh private key from the OS CSPRNG, rejection-sampled into [1, n-1].
pub fn generate_private_key() -> Result<Zeroizing<[u8; 32]>, Error> {
    let mut k = Zeroizing::new([0u8; 32]);
    loop {
        getrandom::getrandom(&mut k[..]).map_err(|e| Error::invalid(format!("no randomness: {e}")))?;
        if SigningKey::from_slice(&k[..]).is_ok() {
            return Ok(k);
        }
    }
}

pub fn is_valid_private_key(priv32: &[u8]) -> bool {
    priv32.len() == 32 && SigningKey::from_slice(priv32).is_ok()
}

/// 65-byte uncompressed public key (0x04 | X | Y).
pub fn public_key(priv32: &[u8]) -> Result<[u8; 65], Error> {
    let p = signing_key(priv32)?.verifying_key().to_encoded_point(false);
    Ok(p.as_bytes().try_into().expect("uncompressed point is 65 bytes"))
}

/// Raw 20-byte address: last 20 bytes of keccak256(X|Y).
pub fn address_bytes(pub65: &[u8; 65]) -> [u8; 20] {
    keccak(&pub65[1..])[12..].try_into().unwrap()
}

/// EIP-55 mixed-case checksum address, 0x-prefixed.
pub fn checksum_address(addr20: &[u8; 20]) -> String {
    let lower = hex::encode(addr20);
    let h = hex::encode(&keccak(lower.as_bytes()));
    let mut s = String::from("0x");
    for (c, hc) in lower.chars().zip(h.chars()) {
        s.push(if c.is_ascii_lowercase() && hc >= '8' { c.to_ascii_uppercase() } else { c });
    }
    s
}

pub fn address(priv32: &[u8]) -> Result<String, Error> {
    Ok(checksum_address(&address_bytes(&public_key(priv32)?)))
}

/// Signs a 32-byte digest: deterministic (RFC 6979), low-S, v = 27 + recovery id.
pub fn sign(hash32: &[u8; 32], priv32: &[u8]) -> Result<EthSignature, Error> {
    let sk = signing_key(priv32)?;
    let (sig, rec) = sk.sign_prehash_recoverable(hash32).map_err(|_| Error::invalid("signing failed"))?;
    let (sig, rec) = match sig.normalize_s() {
        Some(low) => (low, RecoveryId::new(!rec.is_y_odd(), rec.is_x_reduced())),
        None => (sig, rec),
    };
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + rec.to_byte();
    Ok(EthSignature(out))
}

/// The signer's 20-byte address, or None if the signature does not recover.
/// Accepts high-S (like ecrecover); verifiers reject it separately with [is_low_s].
pub fn recover_address(hash32: &[u8; 32], sig: &EthSignature) -> Option<[u8; 20]> {
    let rec = match sig.v() {
        27 | 28 => sig.v() - 27,
        0 | 1 => sig.v(),
        _ => return None,
    };
    let s = Signature::from_slice(&sig.0[..64]).ok()?;
    let mut rid = RecoveryId::from_byte(rec)?;
    let s = match s.normalize_s() {
        Some(low) => {
            rid = RecoveryId::new(!rid.is_y_odd(), rid.is_x_reduced());
            low
        }
        None => s,
    };
    let vk = VerifyingKey::recover_from_prehash(hash32, &s, rid).ok()?;
    let p = vk.to_encoded_point(false);
    Some(address_bytes(p.as_bytes().try_into().ok()?))
}

/// EIP-2: s must be at most n/2.
pub fn is_low_s(sig: &EthSignature) -> bool {
    sig.0[32..64] <= HALF_N[..]
}

/// EIP-191 personal_sign digest: keccak256("\x19Ethereum Signed Message:\n" + len + msg).
pub fn personal_message_hash(message: &[u8]) -> [u8; 32] {
    let prefix = format!("\u{19}Ethereum Signed Message:\n{}", message.len());
    keccak_parts(&[prefix.as_bytes(), message])
}

/// did:pkh for an EVM account (CAIP-10 on eip155), its DID document, and did:web profiles.
pub mod did {
    use crate::encoding::hex;
    use serde_json::{json, Value};

    pub const DID_CONTEXT: &str = "https://www.w3.org/ns/did/v1";
    /// The did:pkh method's verification method type (W3C CCG).
    pub const RECOVERY_METHOD: &str = "EcdsaSecp256k1RecoveryMethod2020";
    pub const RECOVERY_CONTEXT: &str = "https://w3id.org/security/suites/secp256k1recovery-2020/v2";
    pub const FRAGMENT: &str = "blockchainAccountId";

    pub fn pkh(address_checksum: &str) -> String {
        pkh_on(address_checksum, 1)
    }

    pub fn pkh_on(address_checksum: &str, chain_id: u64) -> String {
        format!("did:pkh:eip155:{chain_id}:{address_checksum}")
    }

    /// (chainId, 20-byte address), or None if `did` is not a did:pkh:eip155 DID.
    pub fn parse(did: &str) -> Option<(u64, [u8; 20])> {
        let rest = did.strip_prefix("did:pkh:eip155:")?;
        let (chain, addr) = rest.split_once(':')?;
        if chain.is_empty() || !chain.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let a = addr.strip_prefix("0x")?;
        if a.len() != 40 || !a.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some((chain.parse().ok()?, hex::decode_n::<20>(a).ok()?))
    }

    /// The CAIP-10 account id inside a did:pkh: `eip155:<chain>:<address>`.
    pub fn account_id(did: &str) -> Option<&str> {
        parse(did)?;
        did.strip_prefix("did:pkh:")
    }

    /// The one verification method of a did:pkh: `<did>#blockchainAccountId`.
    pub fn verification_method(did: &str) -> String {
        format!("{did}#{FRAGMENT}")
    }

    fn method(id: String, controller: &str, account: &str) -> Value {
        json!({"id": id, "type": RECOVERY_METHOD, "controller": controller, "blockchainAccountId": account})
    }

    /// The DID document a did:pkh resolves to (the did:pkh method generates it from the DID).
    pub fn pkh_document(did: &str) -> Option<Value> {
        let account = account_id(did)?;
        let vm = verification_method(did);
        Some(json!({
            "@context": [DID_CONTEXT, RECOVERY_CONTEXT],
            "id": did,
            "verificationMethod": [method(vm.clone(), did, account)],
            "authentication": [vm], "assertionMethod": [vm],
        }))
    }

    /// `did:web:<host>` for a per-user profile host such as `alice.bitwrap.io` (no port, no path).
    pub fn web(host: &str) -> Option<String> {
        let ok = !host.is_empty()
            && host.len() <= 253
            && host.split('.').count() >= 2
            && host.split('.').all(|l| {
                !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-')
                    && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            });
        ok.then(|| format!("did:web:{host}"))
    }

    /// Where a did:web document is served: `https://<host>/.well-known/did.json`.
    pub fn web_document_url(host: &str) -> Option<String> {
        web(host).map(|_| format!("https://{host}/.well-known/did.json"))
    }

    /// The document a user's profile host serves: the same key as their did:pkh, controlled by
    /// it, and `alsoKnownAs` the did:pkh. A credential is still issued by the did:pkh; the
    /// did:web is a resolvable name for the same identity.
    pub fn web_document(host: &str, pkh_did: &str) -> Option<Value> {
        let id = web(host)?;
        let account = account_id(pkh_did)?;
        let vm = format!("{id}#{FRAGMENT}");
        Some(json!({
            "@context": [DID_CONTEXT, RECOVERY_CONTEXT],
            "id": id,
            "alsoKnownAs": [pkh_did],
            "controller": pkh_did,
            "verificationMethod": [method(vm.clone(), pkh_did, account)],
            "authentication": [vm], "assertionMethod": [vm],
        }))
    }

    /// The did:pkh a fetched did:web document stands for, if it is exactly [web_document] of
    /// one (fetching it is the app's: this crate does no I/O).
    pub fn check_web_document(host: &str, doc: &Value) -> Option<String> {
        let pkh = doc.get("alsoKnownAs")?.as_array()?.first()?.as_str()?.to_string();
        (web_document(host, &pkh).as_ref() == Some(doc)).then_some(pkh)
    }
}
