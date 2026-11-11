//! Binary keccak Merkle tree with domain separation, and selective-disclosure leaves
//! (`Merkle.kt`).
//!
//! ```text
//! leaf  = keccak256(tag | 0x00 | preimage)
//! node  = keccak256(tag | 0x01 | left | right)
//! empty = keccak256(tag | 0x02)
//! ```
//! An odd node is promoted, never duplicated. Disclosure uses the empty tag; tagged schemes
//! (anchoring) must start with an ASCII letter, so the first byte of every tagged hash input is
//! at least 0x41 and can never be the 0x00 / 0x01 / 0x02 that starts an untagged one.

use crate::encoding::{hex, keccak_parts};
use crate::{jcs, Error};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub sibling: [u8; 32],
    pub sibling_on_left: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Proof {
    pub steps: Vec<Step>,
}

impl Proof {
    pub fn to_json(&self) -> Value {
        Value::Array(
            self.steps
                .iter()
                .map(|s| serde_json::json!({"sibling": hex::encode0x(&s.sibling), "left": s.sibling_on_left}))
                .collect(),
        )
    }

    pub fn from_json(v: &Value) -> Result<Self, Error> {
        let arr = v.as_array().ok_or_else(|| Error::invalid("proof must be an array"))?;
        let steps = arr
            .iter()
            .map(|s| {
                let sib = s.get("sibling").and_then(Value::as_str).ok_or_else(|| Error::invalid("bad proof step"))?;
                let left = s.get("left").and_then(Value::as_bool).ok_or_else(|| Error::invalid("bad proof step"))?;
                Ok(Step { sibling: hex::decode_n::<32>(sib)?, sibling_on_left: left })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Proof { steps })
    }
}

#[derive(Clone, Debug)]
pub struct Scheme {
    tag: Vec<u8>,
}

impl Scheme {
    pub fn new(tag: &str) -> Result<Self, Error> {
        if !tag.is_empty() && !tag.as_bytes()[0].is_ascii_alphabetic() {
            return Err(Error::invalid("non-empty tags must start with an ASCII letter"));
        }
        if !tag.is_ascii() {
            return Err(Error::invalid("tags are ASCII"));
        }
        Ok(Self { tag: tag.as_bytes().to_vec() })
    }

    /// The untagged scheme, used for selective disclosure.
    pub fn plain() -> Self {
        Self { tag: Vec::new() }
    }

    fn leaf_hash(&self, pre: &[u8]) -> [u8; 32] {
        keccak_parts(&[&self.tag, &[0x00], pre])
    }

    fn node_hash(&self, l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
        keccak_parts(&[&self.tag, &[0x01], l, r])
    }

    pub fn root<T: AsRef<[u8]>>(&self, leaves: &[T]) -> [u8; 32] {
        if leaves.is_empty() {
            return keccak_parts(&[&self.tag, &[0x02]]);
        }
        self.levels(leaves).last().unwrap()[0]
    }

    pub fn prove<T: AsRef<[u8]>>(&self, leaves: &[T], index: usize) -> Result<Proof, Error> {
        if index >= leaves.len() {
            return Err(Error::invalid("leaf index out of range"));
        }
        let lv = self.levels(leaves);
        let mut steps = Vec::new();
        let mut i = index;
        for level in &lv[..lv.len() - 1] {
            let sib = if i % 2 == 0 { i + 1 } else { i - 1 };
            if sib < level.len() {
                steps.push(Step { sibling: level[sib], sibling_on_left: sib < i });
            }
            i /= 2;
        }
        Ok(Proof { steps })
    }

    pub fn root_from_proof(&self, leaf: &[u8], proof: &Proof) -> [u8; 32] {
        let mut h = self.leaf_hash(leaf);
        for s in &proof.steps {
            h = if s.sibling_on_left { self.node_hash(&s.sibling, &h) } else { self.node_hash(&h, &s.sibling) };
        }
        h
    }

    pub fn verify(&self, root: &[u8; 32], leaf: &[u8], proof: &Proof) -> bool {
        &self.root_from_proof(leaf, proof) == root
    }

    /// The only leaf position `proof` can have in a tree of `leaf_count` leaves, or None if the
    /// path is impossible for that size. Binds a claimed index to its proof.
    pub fn index_of(&self, leaf_count: usize, proof: &Proof) -> Option<usize> {
        if leaf_count < 1 {
            return None;
        }
        let mut sizes = vec![leaf_count];
        let mut n = leaf_count;
        while n > 1 {
            n = (n + 1) / 2;
            sizes.push(n);
        }
        let mut idx = 0usize;
        let mut next = proof.steps.len() as isize - 1;
        for k in (0..sizes.len() - 1).rev() {
            let child = sizes[k];
            idx = if 2 * idx + 1 < child {
                if next < 0 {
                    return None;
                }
                let left = proof.steps[next as usize].sibling_on_left;
                next -= 1;
                2 * idx + left as usize
            } else {
                2 * idx
            };
        }
        (next == -1).then_some(idx)
    }

    fn levels<T: AsRef<[u8]>>(&self, leaves: &[T]) -> Vec<Vec<[u8; 32]>> {
        let mut cur: Vec<[u8; 32]> = leaves.iter().map(|l| self.leaf_hash(l.as_ref())).collect();
        let mut out = vec![cur.clone()];
        while cur.len() > 1 {
            cur = cur
                .chunks(2)
                .map(|p| if p.len() == 2 { self.node_hash(&p[0], &p[1]) } else { p[0] })
                .collect();
            out.push(cur.clone());
        }
        out
    }
}

/// Selective-disclosure leaves over a JSON document: one per primitive (or empty container),
/// addressed by RFC 6901 JSON Pointer, sorted by pointer (UTF-16 order, like Kotlin). Preimage =
/// JCS of `["<pointer>", <value>, "<salt hex>"]`, salt = keccak256(docSalt | pointer)[0..16).
pub mod disclosure {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub struct Leaf {
        pub pointer: String,
        pub value: Value,
    }

    pub fn flatten(doc: &Value) -> Vec<Leaf> {
        fn walk(p: String, e: &Value, out: &mut Vec<Leaf>) {
            match e {
                Value::Object(o) if !o.is_empty() => {
                    for (k, v) in o {
                        walk(format!("{p}/{}", escape(k)), v, out);
                    }
                }
                Value::Array(a) if !a.is_empty() => {
                    for (i, v) in a.iter().enumerate() {
                        walk(format!("{p}/{i}"), v, out);
                    }
                }
                _ => out.push(Leaf { pointer: p, value: e.clone() }),
            }
        }
        let mut out = Vec::new();
        walk(String::new(), doc, &mut out);
        out.sort_by(|a, b| jcs::utf16_cmp(&a.pointer, &b.pointer));
        out
    }

    fn escape(k: &str) -> String {
        k.replace('~', "~0").replace('/', "~1")
    }

    pub fn salt(doc_salt: &[u8], pointer: &str) -> [u8; 16] {
        keccak_parts(&[doc_salt, pointer.as_bytes()])[..16].try_into().unwrap()
    }

    pub fn preimage(pointer: &str, value: &Value, salt: &[u8]) -> Result<Vec<u8>, Error> {
        jcs::bytes(&Value::Array(vec![Value::String(pointer.into()), value.clone(), Value::String(hex::encode(salt))]))
    }

    pub fn leaf_preimages(leaves: &[Leaf], doc_salt: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        leaves.iter().map(|l| preimage(&l.pointer, &l.value, &salt(doc_salt, &l.pointer))).collect()
    }

    pub fn root(doc: &Value, doc_salt: &[u8]) -> Result<[u8; 32], Error> {
        Ok(Scheme::plain().root(&leaf_preimages(&flatten(doc), doc_salt)?))
    }
}
