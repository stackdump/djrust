//! Shared helpers for the golden-fixture tests. Fixtures are written by the Swift
//! DJCore (tools/paritydump); every assertion is "Rust produces the same bytes".

#![allow(dead_code)]

pub mod measure;

use djrust::json::JsonValue;

/// A Swift golden (`fixtures/swift/<name>.json`, written by `tools/paritydump`). They are
/// committed, so a test that reads one fails when it is gone and never skips: a green run would not
/// notice a deleted fixture.
pub fn fixture(name: &str) -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/swift/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{path}: {e}\n  the fixture is committed, so a golden must not pass without it; \
             regenerate it on a Mac with\n  tools/paritydump/regen.sh <swift app repo> HEAD {name}"
        )
    });
    JsonValue::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn rows<'a>(doc: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    doc.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("fixture has no {key}"))
}

pub fn s<'a>(v: &'a JsonValue, key: &str) -> &'a str {
    v.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {key} in {}", v.js_stringified()))
}

pub fn f(v: &JsonValue, key: &str) -> f64 {
    v.get(key)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {key}"))
}

pub fn i(v: &JsonValue, key: &str) -> i64 {
    f(v, key) as i64
}

pub fn bits_hex(d: f64) -> String {
    format!("{:016x}", d.to_bits())
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|k| u8::from_str_radix(&s[k..k + 2], 16).unwrap())
        .collect()
}

/// Collects mismatches so one run reports all of them, then fails once.
#[derive(Default)]
pub struct Diffs {
    pub items: Vec<String>,
    pub checked: usize,
}

impl Diffs {
    pub fn eq<T: PartialEq + std::fmt::Debug>(
        &mut self,
        tag: impl std::fmt::Display,
        got: T,
        want: T,
    ) {
        self.checked += 1;
        if got != want {
            self.items
                .push(format!("{tag}: got {got:?}, want {want:?}"));
        }
    }
    pub fn finish(self, what: &str) {
        assert!(self.checked > 0, "{what}: nothing was checked");
        if !self.items.is_empty() {
            let shown: Vec<_> = self
                .items
                .iter()
                .take(15)
                .map(|s| truncate(s, 400))
                .collect();
            panic!(
                "{what}: {} of {} checks differ from Swift; first:\n  {}",
                self.items.len(),
                self.checked,
                shown.join("\n  ")
            );
        }
        eprintln!("{what}: {} checks match Swift", self.checked);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// A fixture written by beats' own JS (`scripts/beats-*.mjs`, committed in the Swift repo's
/// `Tests/DJCoreTests/Fixtures/beats`): the JS-oracle goldens, no Mac needed.
pub fn js_fixture(name: &str) -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/js/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    JsonValue::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}
