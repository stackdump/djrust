//! The Swift goldens' fixtures (`crates/djengine/tests/fixtures/swift/<section>.json`, written by
//! `tools/paritydump`) are committed, so a golden that reads one must fail when the file is gone,
//! never say "skipped" and pass: a green run would not notice a deleted fixture. The same helper
//! as `djengine`'s `tests/common::fixture`, for the tests that live in `djhost`.

#![allow(dead_code)]

use djrust::json::JsonValue;

/// Where a section's fixture is committed.
pub fn swift_fixture_path(section: &str) -> String {
    format!(
        "{}/tests/fixtures/swift/{section}.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// The command that rewrites a section's fixture. `regen.sh` documents `journal` and `automixdeck`
/// as release builds (`PARITYDUMP_CONFIG`).
pub fn regen_command(section: &str) -> String {
    // `samplerstereo-sf2` is the same section run against a DJCore with the EXS library hidden
    if section == "samplerstereo-sf2" {
        return "PARITYDUMP_NO_EXS=1 PARITYDUMP_WORK=<a dir of its own> tools/paritydump/regen.sh \
                <swift app repo> HEAD samplerstereo"
            .into();
    }
    let config = if matches!(section, "journal" | "automixdeck") {
        "PARITYDUMP_CONFIG=release "
    } else {
        ""
    };
    format!("{config}tools/paritydump/regen.sh <swift app repo> HEAD {section}")
}

/// The text of `swift/<section>.json`; panics, naming the file and the command that rewrites it,
/// when it cannot be read.
pub fn swift_fixture_text(section: &str) -> String {
    let path = swift_fixture_path(section);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{path}: {e}\n  the fixture is committed, so a golden must not pass without it; \
             regenerate it on a Mac with\n  {}",
            regen_command(section)
        )
    })
}

/// `swift/<section>.json` parsed; panics when it is missing or is not JSON.
pub fn swift_fixture(section: &str) -> JsonValue {
    let text = swift_fixture_text(section);
    JsonValue::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", swift_fixture_path(section)))
}
