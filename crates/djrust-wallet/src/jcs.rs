//! JSON Canonicalization Scheme, RFC 8785 (`Jcs.kt`).
//!
//! Object keys sort by UTF-16 code units. Integers must be within ±(2^53−1); a bare integer
//! literal outside that range is an error rather than a silent rounding. Non-integers use the
//! ECMAScript Number-to-string algorithm (`ryu-js`).
//!
//! One deliberate difference from the Kotlin core: a JSON string holding a lone UTF-16 surrogate
//! (`"\ud800"`) cannot be represented in Rust and is rejected at parse time instead of being
//! re-escaped. No schema here admits such strings.

use crate::Error;
use serde_json::{Number, Value};
use std::cmp::Ordering;

pub fn parse(json: &str) -> Result<Value, Error> {
    serde_json::from_str(json).map_err(|e| Error::invalid(format!("bad JSON: {e}")))
}

pub fn canonicalize_str(json: &str) -> Result<String, Error> {
    canonicalize(&parse(json)?)
}

pub fn canonicalize(v: &Value) -> Result<String, Error> {
    let mut s = String::new();
    write(&mut s, v)?;
    Ok(s)
}

pub fn bytes(v: &Value) -> Result<Vec<u8>, Error> {
    canonicalize(v).map(String::into_bytes)
}

/// RFC 8785 key order: UTF-16 code units.
pub fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write(s: &mut String, v: &Value) -> Result<(), Error> {
    match v {
        Value::Null => s.push_str("null"),
        Value::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => s.push_str(&number(n)?),
        Value::String(t) => write_string(s, t),
        Value::Array(a) => {
            s.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write(s, e)?;
            }
            s.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            s.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write_string(s, k);
                s.push(':');
                write(s, &o[k])?;
            }
            s.push('}');
        }
    }
    Ok(())
}

/// The escaping of ECMAScript `JSON.stringify`.
fn write_string(s: &mut String, t: &str) {
    s.push('"');
    for c in t.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\u{08}' => s.push_str("\\b"),
            '\u{0c}' => s.push_str("\\f"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

pub const MAX_SAFE: i128 = (1i128 << 53) - 1;

fn number(n: &Number) -> Result<String, Error> {
    let lit = n.to_string();
    if !lit.contains(['.', 'e', 'E']) {
        // A bare integer literal must survive exactly; never round it through a double.
        let i: i128 = lit
            .parse()
            .map_err(|_| Error::invalid(format!("integer {lit} exceeds 2^53-1; encode as a string")))?;
        if i.abs() > MAX_SAFE {
            return Err(Error::invalid(format!("integer {lit} exceeds 2^53-1; encode as a string")));
        }
        return Ok(i.to_string());
    }
    let d: f64 = lit.parse().map_err(|_| Error::invalid("bad number"))?;
    if !d.is_finite() {
        return Err(Error::invalid("non-finite number"));
    }
    if d == 0.0 {
        return Ok("0".into());
    }
    Ok(ryu_js::Buffer::new().format_finite(d).to_string())
}
