//! Two decisions `WaveRunner.prepare` makes that no audio golden pins, against the Swift's own
//! (`swift/runnerext.json`, `tools/paritydump/runnerext.swift`):
//!
//! - every non-pluck saw / square tonal lane carries its band-limited tables from prepare
//!   (`tonalLane`: `mip` / `mipTop` / `mipBase`; a sine, a triangle and a string have none), and
//!   every non-pluck tonal lane its `pm`, with `mipOn` only where a sweep or the filter asks;
//! - the groove template is on the project only with `.feel` in the musicality
//!   (`feel: opts.musicality.contains(.feel) ? opts.feel : nil`).

mod common;

use common::{fixture, i, rows, s};
use djrust::musicality::fixes::Fixes;
use djrust::wave::{GrooveTemplate, WaveOptions, WaveRunner};
use djrust::TrackSpec;

const SR: f64 = 22_050.0;

#[test]
fn every_saw_and_square_lane_is_prepared_with_its_tables_as_swift_does() {
    let doc = fixture("runnerext");
    let table = rows(&doc, "lanes");
    assert!(table.len() > 100, "{} lane rows", table.len());
    let mut compared = 0;
    for (genre, seed) in [("country", 7.0), ("techno", 42.0), ("lofi", 3.0), ("trance", 9.0)] {
        let project = TrackSpec::new(genre, seed, Some("minimal")).project();
        for (name, opts) in [
            ("extended", WaveOptions::extended()),
            ("beats", WaveOptions::default()),
        ] {
            let case = format!("{genre} {} {name}", seed as i64);
            let wp = WaveRunner::prepare(&project, SR, &opts, 0);
            let want: Vec<_> = table.iter().filter(|r| s(r, "case") == case).collect();
            let got: Vec<_> = wp.lanes().iter().filter(|l| l.is_tonal()).collect();
            assert_eq!(got.len(), want.len(), "{case}: tonal lanes");
            for (l, w) in got.iter().zip(&want) {
                let ctx = format!("{case} {}", l.net_id);
                assert_eq!(l.net_id, s(w, "net"), "{ctx}");
                assert_eq!(l.voice.wave, s(w, "wave"), "{ctx}: wave");
                assert_eq!(l.is_pluck(), w.get("pluck").unwrap().as_bool().unwrap(), "{ctx}");
                let [mip, top, base, pm] = l.mip_counts();
                assert_eq!(
                    [mip as i64, top as i64, base as i64, pm as i64],
                    [i(w, "mip"), i(w, "mipTop"), i(w, "mipBase"), i(w, "pm")],
                    "{ctx}: mip, mipTop, mipBase, pm"
                );
                assert_eq!(
                    l.mip_on(),
                    w.get("mipOn").unwrap().as_bool().unwrap(),
                    "{ctx}: mipOn"
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, table.len(), "every row compared");
    // the fixture holds what this is about: saw / square lanes with tables, all others without
    assert!(table.iter().any(|r| i(r, "mip") > 0 && !r.get("mipOn").unwrap().as_bool().unwrap()));
    assert!(table.iter().any(|r| i(r, "mip") == 0 && i(r, "pm") > 0));
}

#[test]
fn the_template_is_on_the_project_only_with_the_feel_flag_as_swift_has_it() {
    let doc = fixture("runnerext");
    let project = TrackSpec::new("lofi", 3.0, Some("minimal")).project();
    let lofi = GrooveTemplate::for_genre("lofi").cloned();
    for w in rows(&doc, "feel") {
        let case = s(w, "case");
        let (fixes, feel) = match case {
            "none+lofi" => (Fixes::EMPTY, lofi.clone()),
            "groove+lofi" => (Fixes::GROOVE, lofi.clone()),
            "feel+lofi" => (Fixes::FEEL, lofi.clone()),
            "feel+nil" => (Fixes::FEEL, None),
            other => panic!("{other}"),
        };
        let opts = WaveOptions {
            musicality: fixes,
            feel,
            ..Default::default()
        };
        let wp = WaveRunner::prepare(&project, SR, &opts, 0);
        let family = wp.feel.as_ref().map(|f| f.family.as_str());
        assert_eq!(family, w.get("family").and_then(|f| f.as_str()), "{case}");
    }
}
