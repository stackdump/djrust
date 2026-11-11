//! JSON with JavaScript semantics, port of `Beats/JSONValue.swift`.
//!
//! Objects remember key order the way a JS engine does (canonical array-index keys
//! first, ascending, then insertion order), numbers are f64 and print exactly as
//! `Number.prototype.toString` / `JSON.stringify` print them, and there are three
//! serializations: `JSON.stringify` (insertion order), `canonicalizeJSON` (sorted
//! keys) and `canonicalStringify` (sorted keys + Go's HTML escaping). Share CIDs hash
//! the second, model CIDs the third.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

/// A JSON object with JS property order.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct JsonObject {
    entries: Vec<(String, JsonValue)>,
    index: BTreeMap<String, usize>,
}

impl PartialEq for JsonObject {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl JsonObject {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs<I: IntoIterator<Item = (S, JsonValue)>, S: Into<String>>(pairs: I) -> Self {
        let mut o = Self::new();
        for (k, v) in pairs {
            o.set(k, v);
        }
        o
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut JsonValue> {
        let i = *self.index.get(key)?;
        Some(&mut self.entries[i].1)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Set a key: an existing key keeps its position; a new one goes last, except that
    /// array-index keys sort to the front as in JS.
    pub fn set(&mut self, key: impl Into<String>, value: JsonValue) {
        let key = key.into();
        if let Some(&i) = self.index.get(&key) {
            self.entries[i].1 = value;
            return;
        }
        let is_index = array_index(&key).is_some();
        self.index.insert(key.clone(), self.entries.len());
        self.entries.push((key, value));
        if is_index {
            self.normalize_order();
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<JsonValue> {
        let i = self.index.remove(key)?;
        let (_, v) = self.entries.remove(i);
        self.reindex();
        Some(v)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &JsonValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    fn normalize_order(&mut self) {
        let mut ints: Vec<(u32, (String, JsonValue))> = Vec::new();
        let mut rest = Vec::new();
        for e in self.entries.drain(..) {
            match array_index(&e.0) {
                Some(n) => ints.push((n, e)),
                None => rest.push(e),
            }
        }
        ints.sort_by_key(|(n, _)| *n);
        self.entries = ints.into_iter().map(|(_, e)| e).chain(rest).collect();
        self.reindex();
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, (k, _)) in self.entries.iter().enumerate() {
            self.index.insert(k.clone(), i);
        }
    }
}

/// The numeric value of a canonical array-index key ("0", "17", …, up to 2^32-2).
pub fn array_index(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b[0] == b'0' {
        return None;
    }
    match key.parse::<u32>() {
        Ok(n) if n != u32::MAX => Some(n),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(JsonObject),
}

impl JsonValue {
    pub fn int(n: i64) -> Self {
        JsonValue::Number(n as f64)
    }
    pub fn str(s: impl Into<String>) -> Self {
        JsonValue::String(s.into())
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(o) => o.get(key),
            _ => None,
        }
    }
    pub fn at(&self, i: usize) -> Option<&JsonValue> {
        match self {
            JsonValue::Array(a) => a.get(i),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(d) => Some(*d),
            _ => None,
        }
    }
    /// `Int(double)` as Swift's `JSONValue.int` does it: truncation toward zero.
    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|d| d as i64)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<JsonValue>> {
        match self {
            JsonValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            JsonValue::Object(o) => Some(o),
            _ => None,
        }
    }
    pub fn strings(&self) -> Option<Vec<String>> {
        self.as_array().map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
    }
    pub fn ints(&self) -> Option<Vec<i64>> {
        self.as_array()
            .map(|a| a.iter().filter_map(JsonValue::as_i64).collect())
    }

    /// JS truthiness.
    pub fn is_truthy(&self) -> bool {
        match self {
            JsonValue::Null => false,
            JsonValue::Bool(b) => *b,
            JsonValue::Number(d) => *d != 0.0 && !d.is_nan(),
            JsonValue::String(s) => !s.is_empty(),
            JsonValue::Array(_) | JsonValue::Object(_) => true,
        }
    }
    pub fn is_object_like(&self) -> bool {
        matches!(self, JsonValue::Array(_) | JsonValue::Object(_))
    }
}

// ---------------------------------------------------------------------------
// JS-faithful number formatting

/// `Number.prototype.toString()` for a double (ECMAScript Number::toString).
pub fn js_number_string(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d == 0.0 {
        return "0".into();
    }
    if d.is_infinite() {
        return if d < 0.0 {
            "-Infinity".into()
        } else {
            "Infinity".into()
        };
    }
    let sign = if d < 0.0 { "-" } else { "" };
    // `{:e}` gives the shortest round-tripping digits (as JS requires) with an
    // exponent: "1.2345e-7", "5e0". Re-lay them out the ECMAScript way.
    let text = format!("{:e}", d.abs());
    let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
    let exp10: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let k = digits.len() as i32;
    let n = exp10 + 1; // decimal point position relative to digits
    let mut out = String::from(sign);
    if k <= n && n <= 21 {
        out.push_str(digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(digits);
    } else {
        let e = n - 1;
        let es = if e < 0 {
            format!("-{}", -e)
        } else {
            format!("+{}", e)
        };
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push_str(&es);
    }
    out
}

/// ECMAScript ToInt32.
pub fn js_to_int32(d: f64) -> i32 {
    if !d.is_finite() {
        return 0;
    }
    let t = trunc(d);
    let m = t % 4_294_967_296.0;
    let u = if m < 0.0 { m + 4_294_967_296.0 } else { m };
    (u as u32) as i32
}

/// f64::trunc without std.
pub(crate) fn trunc(d: f64) -> f64 {
    if !d.is_finite() || d.abs() >= 4_503_599_627_370_496.0 {
        return d;
    }
    let t = d as i64 as f64;
    if t == 0.0 && d.is_sign_negative() {
        -0.0
    } else {
        t
    }
}

/// f64::floor without std.
#[doc(hidden)]
pub fn floor(d: f64) -> f64 {
    let t = trunc(d);
    if t > d {
        t - 1.0
    } else {
        t
    }
}

/// `Math.round`: ties toward +∞.
pub fn js_round(d: f64) -> f64 {
    if !d.is_finite() {
        return d;
    }
    let f = floor(d);
    if d - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

// ---------------------------------------------------------------------------
// Serialization

#[derive(Clone, Copy)]
enum Style {
    Insertion,
    Sorted,
    SortedGo,
}

impl JsonValue {
    /// `JSON.stringify(value)`: insertion order, compact, JS string escaping.
    pub fn js_stringified(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Style::Insertion);
        out
    }

    /// `canonicalizeJSON` (share/codec.js): sorted keys, plain JSON.stringify strings.
    /// What share envelopes are hashed over.
    pub fn canonical_js(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Style::Sorted);
        out
    }

    /// `canonicalStringify` (pflow.js): sorted keys plus Go's json.Marshal HTML
    /// escaping. What model CIDs are hashed over.
    pub fn canonical_go(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Style::SortedGo);
        out
    }

    fn write(&self, out: &mut String, style: Style) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Number(d) => {
                if d.is_finite() {
                    out.push_str(&js_number_string(*d))
                } else {
                    out.push_str("null")
                }
            }
            JsonValue::String(s) => write_string(s, out, matches!(style, Style::SortedGo)),
            JsonValue::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out, style);
                }
                out.push(']');
            }
            JsonValue::Object(o) => {
                let mut entries: Vec<&(String, JsonValue)> = o.entries.iter().collect();
                if !matches!(style, Style::Insertion) {
                    // Array.prototype.sort() default: UTF-16 code unit order.
                    entries.sort_by(|a, b| utf16_cmp(&a.0, &b.0));
                }
                out.push('{');
                for (i, (k, v)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, out, matches!(style, Style::SortedGo));
                    out.push(':');
                    v.write(out, style);
                }
                out.push('}');
            }
        }
    }
}

/// JS `a < b` ordering on strings (UTF-16 code units).
pub fn utf16_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_string(s: &str, out: &mut String, go_escape: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            '<' if go_escape => out.push_str("\\u003c"),
            '>' if go_escape => out.push_str("\\u003e"),
            '&' if go_escape => out.push_str("\\u0026"),
            '\u{2028}' if go_escape => out.push_str("\\u2028"),
            '\u{2029}' if go_escape => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Parsing: JSON.parse semantics (order kept, last duplicate wins, numbers are f64)

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub message: &'static str,
    pub offset: usize,
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "JSON syntax error at {}: {}", self.offset, self.message)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for JsonError {}

impl JsonValue {
    pub fn parse(text: &str) -> Result<JsonValue, JsonError> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(p.fail("trailing characters"));
        }
        Ok(v)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn fail(&self, message: &'static str) -> JsonError {
        JsonError {
            message,
            offset: self.i,
        }
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn value(&mut self) -> Result<JsonValue, JsonError> {
        match self.peek() {
            None => Err(self.fail("unexpected end")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(JsonValue::String(self.string()?)),
            Some(b't') => self.literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.literal("false", JsonValue::Bool(false)),
            Some(b'n') => self.literal("null", JsonValue::Null),
            _ => self.number(),
        }
    }
    fn literal(&mut self, s: &str, v: JsonValue) -> Result<JsonValue, JsonError> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(self.fail("bad literal"))
        }
    }
    fn digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
    }
    fn number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => self.digits(),
            _ => return Err(self.fail("bad number")),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.fail("bad fraction"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.fail("bad exponent"));
            }
            self.digits();
        }
        let s =
            core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.fail("bad number"))?;
        s.parse::<f64>()
            .map(JsonValue::Number)
            .map_err(|_| self.fail("bad number"))
    }
    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1; // opening quote
        let mut units: Vec<u16> = Vec::new();
        let mut raw_start = self.i;
        let flush = |units: &mut Vec<u16>, raw: &[u8]| {
            if !raw.is_empty() {
                units.extend(String::from_utf8_lossy(raw).encode_utf16());
            }
        };
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'"' {
                flush(&mut units, &self.b[raw_start..self.i]);
                self.i += 1;
                // Lone surrogates become U+FFFD, as in Swift.
                return Ok(String::from_utf16_lossy(&units));
            }
            if c < 0x20 {
                return Err(self.fail("control character in string"));
            }
            if c == b'\\' {
                flush(&mut units, &self.b[raw_start..self.i]);
                self.i += 1;
                let Some(e) = self.peek() else { break };
                self.i += 1;
                match e {
                    b'"' => units.push(0x22),
                    b'\\' => units.push(0x5C),
                    b'/' => units.push(0x2F),
                    b'b' => units.push(0x08),
                    b'f' => units.push(0x0C),
                    b'n' => units.push(0x0A),
                    b'r' => units.push(0x0D),
                    b't' => units.push(0x09),
                    b'u' => {
                        let hex = self
                            .b
                            .get(self.i..self.i + 4)
                            .and_then(|h| core::str::from_utf8(h).ok())
                            .and_then(|h| u16::from_str_radix(h, 16).ok())
                            .ok_or_else(|| self.fail("bad \\u escape"))?;
                        self.i += 4;
                        units.push(hex);
                    }
                    _ => return Err(self.fail("bad escape")),
                }
                raw_start = self.i;
            } else {
                self.i += 1;
            }
        }
        Err(self.fail("unterminated string"))
    }
    fn array(&mut self) -> Result<JsonValue, JsonError> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(JsonValue::Array(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(JsonValue::Array(out));
                }
                None => return Err(self.fail("unterminated array")),
                _ => return Err(self.fail("expected , or ]")),
            }
        }
    }
    fn object(&mut self) -> Result<JsonValue, JsonError> {
        self.i += 1;
        let mut obj = JsonObject::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(JsonValue::Object(obj));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(self.fail("expected key"));
            }
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(self.fail("expected :"));
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            obj.set(k, v);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(JsonValue::Object(obj));
                }
                None => return Err(self.fail("unterminated object")),
                _ => return Err(self.fail("expected , or }")),
            }
        }
    }
}

impl From<&str> for JsonValue {
    fn from(s: &str) -> Self {
        JsonValue::String(s.to_string())
    }
}
impl From<String> for JsonValue {
    fn from(s: String) -> Self {
        JsonValue::String(s)
    }
}
impl From<f64> for JsonValue {
    fn from(d: f64) -> Self {
        JsonValue::Number(d)
    }
}
impl From<i64> for JsonValue {
    fn from(n: i64) -> Self {
        JsonValue::Number(n as f64)
    }
}
impl From<bool> for JsonValue {
    fn from(b: bool) -> Self {
        JsonValue::Bool(b)
    }
}
impl From<JsonObject> for JsonValue {
    fn from(o: JsonObject) -> Self {
        JsonValue::Object(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_js() {
        for (d, s) in [
            (1.0, "1"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (-1.5e-9, "-1.5e-9"),
            (13.897, "13.897"),
        ] {
            assert_eq!(js_number_string(d), s);
        }
    }

    #[test]
    fn integer_keys_sort_first() {
        let v = JsonValue::parse(r#"{"b":1,"2":2,"a":3,"1":4,"01":5}"#).unwrap();
        assert_eq!(v.js_stringified(), r#"{"1":4,"2":2,"b":1,"a":3,"01":5}"#);
    }
}
