//! "Safe and limited" user text (`TextPolicy.kt`). The allowlist and length cap come from the
//! schema's `x-textPolicy`, so the schemaCID covers them. A char loop, not a regex. Lengths and
//! indices are in UTF-16 units, as in the Kotlin core and JS.

use crate::schema::SchemaDoc;
use crate::Error;

#[derive(Clone, Debug)]
pub struct TextPolicy {
    allowed: [bool; 128],
    pub min_length: usize,
    pub max_length: usize,
}

impl TextPolicy {
    pub fn new(allowed_chars: &str, min_length: usize, max_length: usize) -> Result<Self, Error> {
        let mut allowed = [false; 128];
        for c in allowed_chars.chars() {
            if !(0x20..=0x7e).contains(&(c as u32)) {
                return Err(Error::invalid("allowlist must be printable ASCII"));
            }
            allowed[c as usize] = true;
        }
        if min_length > max_length {
            return Err(Error::invalid("minLength > maxLength"));
        }
        Ok(Self { allowed, min_length, max_length })
    }

    pub fn from_schema(schema: &SchemaDoc) -> Result<Self, Error> {
        let p = schema.json().get("x-textPolicy").ok_or_else(|| Error::invalid("schema has no x-textPolicy"))?;
        let s = p.get("allowedChars").and_then(|v| v.as_str()).ok_or_else(|| Error::invalid("allowedChars"))?;
        let min = p.get("minLength").and_then(|v| v.as_u64()).ok_or_else(|| Error::invalid("minLength"))?;
        let max = p.get("maxLength").and_then(|v| v.as_u64()).ok_or_else(|| Error::invalid("maxLength"))?;
        Self::new(s, min as usize, max as usize)
    }

    /// None if acceptable, else a short reason (never echoes the text).
    pub fn violation(&self, text: &str) -> Option<String> {
        let len = text.encode_utf16().count();
        if len < self.min_length {
            return Some("too short".into());
        }
        if len > self.max_length {
            return Some("too long".into());
        }
        for (i, u) in text.encode_utf16().enumerate() {
            if u >= 128 || !self.allowed[u as usize] {
                return Some(format!("character not allowed at index {i}"));
            }
        }
        if text.starts_with(' ') || text.ends_with(' ') {
            return Some("leading or trailing space".into());
        }
        if text.contains("  ") {
            return Some("consecutive spaces".into());
        }
        None
    }

    pub fn is_valid(&self, text: &str) -> bool {
        self.violation(text).is_none()
    }
}
