//! Fixture hygiene: a Swift golden must fail when its committed fixture is gone.
//!
//! Ten goldens used to print "SKIPPED ... is missing" and pass when `swift/<section>.json` was
//! absent, so a green run would not have noticed a deleted fixture. They now read it through
//! `common::fixture` (`djengine`) or `common_fixture::swift_fixture` (`djhost`), which panic naming
//! the file and the `tools/paritydump/regen.sh` command. This file keeps it that way. It reads the
//! test sources of every crate; it is not a parity test and holds no Swift value.
//!
//! The goldens that are legitimately optional are not of this kind: they need something that is not
//! committed (the real `MuseScore_General.sf2`, `ffmpeg`) and say so in their module docs.

mod common;

use std::path::{Path, PathBuf};

/// Every `.rs` file under `tests`, one directory level down included (the `common*`
/// and `*_support` modules), except this file.
fn test_sources() -> Vec<(PathBuf, String)> {
    fn rs_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth == 0 && p.file_name().is_some_and(|n| n != "fixtures") {
                rs_files(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"), 0, &mut files);
    files.retain(|p| p.file_name().is_some_and(|n| n != "fixtures_required.rs" && n != "host_fixtures_required.rs"));
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            (p, text)
        })
        .collect()
}

#[test]
fn a_missing_swift_fixture_panics_and_names_the_regen_command() {
    let err = std::panic::catch_unwind(|| common::fixture("no-such-section")).unwrap_err();
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("a string panic");
    assert!(msg.contains("no-such-section.json"), "{msg}");
    assert!(
        msg.contains("tools/paritydump/regen.sh <swift app repo> HEAD no-such-section"),
        "{msg}"
    );
}

/// Every committed `swift/*.json` is read by at least one test, by name: through the helpers
/// (`fixture("x")`, `swift_fixture("x")`) or by its path (`swift/x.json`).
#[test]
fn every_committed_swift_fixture_has_a_reader() {
    let sources = test_sources();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/swift");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "json").then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(names.len() >= 30, "found only {names:?}");
    let unread: Vec<&String> = names
        .iter()
        .filter(|n| {
            let by_helper = format!("fixture(\"{n}\")");
            let by_path = format!("swift/{n}.json");
            !sources
                .iter()
                .any(|(_, t)| t.contains(&by_helper) || t.contains(&by_path))
        })
        .collect();
    assert!(unread.is_empty(), "no test reads {unread:?}");
}

/// No test prints a "skipped" or "is missing" line naming the fixture path and passes: the shape
/// of the ten skips this file replaced (`eprintln!("SKIPPED: {path} is missing (...)")`).
#[test]
fn no_golden_skips_when_its_fixture_is_missing() {
    let mut found = Vec::new();
    for (path, text) in test_sources() {
        for (n, line) in text.lines().enumerate() {
            let l = line.to_lowercase();
            if line.contains("{path}") && (l.contains("skipped") || l.contains("is missing")) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "a golden skips when its fixture is gone:\n{}", found.join("\n"));
}
