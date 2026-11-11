//! Canonical snapshot of a deterministic beats track (`Snapshot.kt`): (genre, seed, structure)
//! as JSON-LD with the CC0 dedication in-band. Not signed, not owned.
//! (Not to be confused with a whole-session `DjSnapshot`, schema `dj-snapshot.v1`.)

use crate::{cid, Error};
use serde_json::{json, Value};

pub const CONTEXT: &str = "https://beats.bitwrap.io/schema/beats-share.context.jsonld";
pub const CC0_URL: &str = "https://creativecommons.org/publicdomain/zero/1.0/";
const MAX_SEED: u64 = (1 << 53) - 1;

pub fn of(genre: &str, seed: u64, structure: &str) -> Result<Value, Error> {
    let s = json!({"@context": CONTEXT, "@type": "BeatsSnapshot", "v": 1, "genre": genre, "seed": seed,
                   "structure": structure, "license": CC0_URL});
    match validate(&s) {
        None => Ok(s),
        Some(r) => Err(Error::invalid(r)),
    }
}

pub fn cid(snapshot: &Value) -> Result<String, Error> {
    cid::of_json(snapshot)
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// None for a well-formed snapshot, else a short reason. Mirrors beats-snapshot.v1.
pub fn validate(e: &Value) -> Option<String> {
    let Some(o) = e.as_object() else { return Some("snapshot must be an object".into()) };
    const ALLOWED: [&str; 7] = ["@context", "@type", "v", "genre", "seed", "structure", "license"];
    if let Some(k) = o.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
        return Some(format!("unknown member {k}"));
    }
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    if s("@context") != Some(CONTEXT) {
        return Some("bad @context".into());
    }
    if s("@type") != Some("BeatsSnapshot") {
        return Some("bad @type".into());
    }
    if o.get("v").and_then(Value::as_i64) != Some(1) {
        return Some("bad v".into());
    }
    let Some(genre) = s("genre") else { return Some("genre required".into()) };
    if genre.is_empty() || utf16_len(genre) > 64 {
        return Some("genre length".into());
    }
    let seed = match o.get("seed") {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    let Some(seed) = seed else { return Some("seed must be an integer".into()) };
    if seed < 0 || seed as u64 > MAX_SEED {
        return Some("seed out of range".into());
    }
    let Some(structure) = s("structure") else { return Some("structure required".into()) };
    if structure.is_empty() || utf16_len(structure) > 32 {
        return Some("structure length".into());
    }
    if s("license") != Some(CC0_URL) {
        return Some("snapshots must carry the CC0 dedication".into());
    }
    None
}
