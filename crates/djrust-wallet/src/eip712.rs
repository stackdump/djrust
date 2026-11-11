//! EIP-712 typed structured data hashing (`Eip712.kt`).
//!
//! Member types: string, bool, address, bytes, bytesN, uintN (values up to 128 bits) and nested
//! structs. Arrays and signed ints are rejected rather than mis-encoded.

use crate::encoding::{hex, keccak, keccak_parts};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: String,
}

impl Field {
    pub fn new(name: &str, ty: &str) -> Self {
        Self { name: name.into(), ty: ty.into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// string, or hex for address / bytes / bytesN
    Str(String),
    Uint(u128),
    Bool(bool),
    Struct(BTreeMap<String, Val>),
}

#[derive(Clone, Debug)]
pub struct TypedData {
    pub types: BTreeMap<String, Vec<Field>>,
    pub primary_type: String,
    pub domain: BTreeMap<String, Val>,
    pub message: BTreeMap<String, Val>,
}

const DOMAIN_ORDER: [(&str, &str); 5] = [
    ("name", "string"),
    ("version", "string"),
    ("chainId", "uint256"),
    ("verifyingContract", "address"),
    ("salt", "bytes32"),
];

impl TypedData {
    /// keccak256(0x19 0x01 | domainSeparator | hashStruct(message)): the digest that is signed.
    pub fn signing_hash(&self) -> Result<[u8; 32], Error> {
        Ok(keccak_parts(&[&[0x19, 0x01], &self.domain_separator()?, &self.hash_struct()?]))
    }

    pub fn domain_separator(&self) -> Result<[u8; 32], Error> {
        let all = self.all_types()?;
        hash_struct("EIP712Domain", &self.domain, &all)
    }

    pub fn hash_struct(&self) -> Result<[u8; 32], Error> {
        hash_struct(&self.primary_type, &self.message, &self.all_types()?)
    }

    pub fn encode_type(&self, ty: &str) -> Result<String, Error> {
        Ok(encode_type(ty, &self.all_types()?))
    }

    fn all_types(&self) -> Result<BTreeMap<String, Vec<Field>>, Error> {
        let mut all = self.types.clone();
        if !all.contains_key("EIP712Domain") {
            let fields: Vec<Field> = DOMAIN_ORDER
                .iter()
                .filter(|(n, _)| self.domain.contains_key(*n))
                .map(|(n, t)| Field::new(n, t))
                .collect();
            if fields.len() != self.domain.len() {
                return Err(Error::invalid("unknown domain member"));
            }
            all.insert("EIP712Domain".into(), fields);
        }
        Ok(all)
    }
}

fn collect(ty: &str, all: &BTreeMap<String, Vec<Field>>, out: &mut Vec<String>) {
    if !all.contains_key(ty) || out.iter().any(|t| t == ty) {
        return;
    }
    out.push(ty.into());
    for f in &all[ty] {
        collect(f.ty.trim_end_matches("[]"), all, out);
    }
}

fn encode_type(ty: &str, all: &BTreeMap<String, Vec<Field>>) -> String {
    let mut deps = Vec::new();
    collect(ty, all, &mut deps);
    let rest: BTreeSet<&String> = deps.iter().filter(|t| *t != ty).collect();
    let mut s = String::new();
    for t in std::iter::once(&ty.to_string()).chain(rest.into_iter()) {
        s.push_str(t);
        s.push('(');
        let parts: Vec<String> = all[t].iter().map(|f| format!("{} {}", f.ty, f.name)).collect();
        s.push_str(&parts.join(","));
        s.push(')');
    }
    s
}

fn hash_struct(ty: &str, value: &BTreeMap<String, Val>, all: &BTreeMap<String, Vec<Field>>) -> Result<[u8; 32], Error> {
    let fields = all.get(ty).ok_or_else(|| Error::invalid(format!("unknown type {ty}")))?;
    let mut enc = Vec::with_capacity(32 * (fields.len() + 1));
    enc.extend_from_slice(&keccak(encode_type(ty, all).as_bytes()));
    for f in fields {
        let v = value.get(&f.name).ok_or_else(|| Error::invalid(format!("missing member {} of {ty}", f.name)))?;
        enc.extend_from_slice(&encode_value(&f.ty, v, all)?);
    }
    Ok(keccak(&enc))
}

fn word(n: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&n.to_be_bytes());
    w
}

fn encode_value(ty: &str, v: &Val, all: &BTreeMap<String, Vec<Field>>) -> Result<[u8; 32], Error> {
    let bad = || Error::invalid(format!("value does not fit type {ty}"));
    if ty.ends_with(']') {
        return Err(Error::invalid("array types are not supported"));
    }
    if all.contains_key(ty) {
        return match v {
            Val::Struct(m) => hash_struct(ty, m, all),
            _ => Err(bad()),
        };
    }
    match (ty, v) {
        ("string", Val::Str(s)) => Ok(keccak(s.as_bytes())),
        ("bytes", Val::Str(s)) => Ok(keccak(&hex::decode(s)?)),
        ("bool", Val::Bool(b)) => Ok(word(*b as u128)),
        ("address", Val::Str(s)) => {
            let a = hex::decode(s)?;
            if a.len() != 20 {
                return Err(Error::invalid("address must be 20 bytes"));
            }
            let mut w = [0u8; 32];
            w[12..].copy_from_slice(&a);
            Ok(w)
        }
        (t, Val::Uint(n)) if t.starts_with("uint") => {
            let bits: u32 = t[4..].parse().map_err(|_| Error::invalid(format!("bad type {t}")))?;
            if !(8..=256).contains(&bits) || bits % 8 != 0 {
                return Err(Error::invalid(format!("bad type {t}")));
            }
            if bits < 128 && *n >> bits != 0 {
                return Err(Error::invalid(format!("value out of range for {t}")));
            }
            Ok(word(*n))
        }
        (t, Val::Str(s)) if t.starts_with("bytes") => {
            let n: usize = t[5..].parse().map_err(|_| Error::invalid(format!("bad type {t}")))?;
            if !(1..=32).contains(&n) {
                return Err(Error::invalid(format!("bad type {t}")));
            }
            let raw = hex::decode(s)?;
            if raw.len() != n {
                return Err(Error::invalid(format!("{t} needs {n} bytes")));
            }
            let mut w = [0u8; 32];
            w[..n].copy_from_slice(&raw);
            Ok(w)
        }
        _ => Err(bad()),
    }
}
