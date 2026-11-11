//! Content ids exactly as beats computes them, port of `Beats/CID.swift`:
//! share CIDs are CIDv1, dag-json (0x0129), sha2-256, base58btc with the multibase
//! `z` prefix; model CIDs are lowercase hex sha256 of the canonical project JSON.

use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

use crate::json::JsonValue;

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

pub fn encode_base58(bytes: &[u8]) -> String {
    let mut digits: Vec<u8> = Vec::new(); // base58 digits, least significant first
    for &b in bytes {
        let mut carry = b as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut s = String::with_capacity(zeros + digits.len());
    for _ in 0..zeros {
        s.push(ALPHABET[0] as char);
    }
    for &d in digits.iter().rev() {
        s.push(ALPHABET[d as usize] as char);
    }
    s
}

pub fn decode_base58(text: &str) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new(); // least significant first
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        let mut carry = v;
        for o in out.iter_mut() {
            carry += *o as u32 * 58;
            *o = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }
    let zeros = text.bytes().take_while(|&c| c == ALPHABET[0]).count();
    let mut r = alloc::vec![0u8; zeros];
    r.extend(out.iter().rev());
    Some(r)
}

/// `createCIDv1Bytes`: 0x01, dag-json varint (a9 02), sha2-256 (12 len), digest.
pub fn cid_v1_bytes(hash: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![0x01, 0xA9, 0x02, 0x12, hash.len() as u8];
    v.extend_from_slice(hash);
    v
}

/// `computeCidForJsonLd`: canonicalizeJSON → sha256 → CIDv1 → `z` + base58.
pub fn share_cid(doc: &JsonValue) -> String {
    share_cid_canonical(&doc.canonical_js())
}

pub fn share_cid_canonical(canonical: &str) -> String {
    let mut s = String::from("z");
    s.push_str(&encode_base58(&cid_v1_bytes(&sha256(canonical.as_bytes()))));
    s
}

/// Model CID: lowercase hex sha256 of `canonicalProjectJSON` (Go-escaped).
pub fn model_cid(canonical_project: &str) -> String {
    hex(&sha256(canonical_project.as_bytes()))
}
