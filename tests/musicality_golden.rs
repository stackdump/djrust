//! Swift goldens for `DJMusicality` (`tools/paritydump`, section "musicality"),
//! `tests/fixtures/swift/musicality.json`: the played project's hash, the note log, and the
//! whole `MusicalityReport.dictionary` of the `.all` variant. `tests/musicality*.rs` hold the
//! Swift suite's own invariants; the report dictionaries of unfixed and partly fixed tracks and
//! of every genre as a deck plays it are compared in `musicality_report_scores.rs`.

mod common;
use common::*;
use djrust::cid::{hex, sha256};
use djrust::json::JsonValue;
use djrust::musicality::note_log::capture_all;
use djrust::musicality::report::MusicalityReport;
use djrust::musicality::{apply, Fixes};
use djrust::TrackSpec;

fn sha(s: &str) -> String {
    hex(&sha256(s.as_bytes()))
}

#[test]
fn musicality_matches_swift() {
    let doc = fixture("musicality");
    let mut d = Diffs::default();
    for row in rows(&doc, "cases") {
        let structure = row.get("structure").and_then(JsonValue::as_str);
        let spec = TrackSpec::new(s(row, "genre"), f(row, "seed"), structure);
        let composed = spec.project();
        for v in rows(row, "variants") {
            let name = s(v, "fixes");
            let fixes = if name == "all" {
                Fixes::ALL
            } else {
                Fixes::named(name).expect(name)
            };
            let p = apply(&composed, &spec, fixes);
            let (notes, controls) = capture_all(&p, None);
            let tag = format!(
                "{} {} {:?} {name}",
                s(row, "genre"),
                f(row, "seed"),
                structure
            );
            d.eq(
                format!("{tag} sha"),
                sha(&p.canonical_json()),
                s(v, "sha").to_string(),
            );
            d.eq(format!("{tag} notes"), notes.len() as i64, i(v, "notes"));
            let log: Vec<String> = notes
                .iter()
                .map(|e| format!("{}:{}:{}:{}", e.tick, e.net, e.note, e.velocity))
                .collect();
            d.eq(
                format!("{tag} logSha"),
                sha(&log.join(",")),
                s(v, "logSha").to_string(),
            );
            d.eq(
                format!("{tag} controls"),
                controls.len() as i64,
                i(v, "controls"),
            );
            if let Some(rep) = v.get("report") {
                let r = MusicalityReport::new(&p, &notes, &controls, Some(s(row, "genre")));
                d.eq(
                    format!("{tag} report"),
                    r.dictionary().canonical_js(),
                    rep.canonical_js(),
                );
            }
        }
    }
    d.finish("musicality");
}
