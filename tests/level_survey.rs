//! `LevelSurvey` (`djctl levels`): the statistics over a set of `SampleLevel`s, held to Swift's
//! literals (`SampleAutoGainTests`' survey section, always on) and to the real Swift objects
//! (`tools/paritydump/levelsurvey.swift`, section "levelsurvey": the entries' errors by bit
//! pattern, `limitedBy`, the dictionaries, the spreads, the exceptions and the summary, over
//! the tolerance edges, partial loops, silence, failures and the empty survey; and `pick`).

mod common;

use common::*;
use djrust::deck::level_match::{LevelMatch, LevelSurvey, SampleLevel, SurveyEntry};
use djrust::json::{JsonObject, JsonValue};
use djrust::layers::StockLoop;

/// The test's `entry(_:peak:low:high:)`.
fn entry(lufs: Option<f64>, peak: f64, low: f64, high: f64) -> SurveyEntry {
    SurveyEntry::new(
        &format!("{}", lufs.unwrap_or(0.0)),
        "",
        SampleLevel::new(lufs, peak, low, high),
    )
}

fn entry_d(lufs: Option<f64>) -> SurveyEntry {
    entry(lufs, -10.0, 0.3, 0.1)
}

fn int(o: &JsonObject, key: &str) -> Option<i64> {
    o.get(key).and_then(JsonValue::as_i64)
}

// SampleAutoGainTests.theSurveyReportsTheSpreadBeforeAndAfter
#[test]
fn the_survey_reports_the_spread_before_and_after() {
    let r = LevelMatch::REFERENCE_LUFS;
    let s = LevelSurvey::new(
        vec![
            entry(Some(r - 6.0), -12.0, 0.3, 0.1), // +6 dB, room under the ceiling: lands
            entry_d(Some(r + 4.0)),                // -4 dB: lands
            entry(Some(r - 20.0), -30.0, 0.3, 0.1), // past the +-12 dB clamp: 8 LU short
            entry(Some(r - 6.0), -3.0, 0.3, 0.1),  // the peak allows only +2 dB: 4 LU short
            entry(Some(r - 20.0), 0.0, 0.3, 0.1),  // past the clamp too, the peak allows no boost
            entry(Some(r - 20.0), -12.0, 0.3, 0.1), // past the clamp, the peak allows +11 dB: peak
            entry_d(None),                         // silence: not counted
        ],
        vec![],
    );
    assert_eq!(s.spread_before(), Some(24.0));
    assert!((s.spread_after().unwrap_or(0.0) - 20.0).abs() < 1e-9);
    let limited: Vec<Option<&str>> = s.exceptions().iter().map(|e| e.limited_by()).collect();
    assert_eq!(
        limited,
        [Some("maxGain"), Some("peak"), Some("peak"), Some("peak")]
    );
    let summary = s.summary();
    assert_eq!(int(&summary, "within"), Some(2));
    assert_eq!(int(&summary, "silent"), Some(1));
    assert_eq!(int(&summary, "measured"), Some(7));
    // a partial loop is measured against its own target (4 LU under)
    let bass = entry(Some(r + LevelMatch::PARTIAL_OFFSET_DB), -10.0, 0.9, 0.0);
    assert!(
        bass.level.partial() && bass.error_before() == Some(0.0) && bass.error_after() == Some(0.0)
    );
}

fn stock(category: &str, i: usize) -> StockLoop {
    StockLoop {
        path: format!("/{category}/{}{i}", if i < 10 { "0" } else { "" }),
        name: i.to_string(),
        pack: "p".into(),
        category: category.into(),
        subcategory: String::new(),
        genre: String::new(),
        beats: 8,
        seconds: 4.0,
        key: None,
    }
}

// SampleAutoGainTests.theStockPickIsTheSameEveryRun
#[test]
fn the_stock_pick_is_the_same_every_run() {
    let mut loops: Vec<StockLoop> = (0..10).map(|i| stock("Bass", i)).collect();
    loops.extend((0..3).map(|i| stock("Drums", i)));
    let mut reversed = loops.clone();
    reversed.reverse();
    let pick = LevelSurvey::pick(&reversed, 4);
    let paths: Vec<&str> = pick.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/Bass/00",
            "/Bass/02",
            "/Bass/05",
            "/Bass/07",
            "/Drums/00",
            "/Drums/01",
            "/Drums/02"
        ]
    );
    // the same pick whatever order the library lists them in (Swift shuffles; this is a fixed
    // interleaving of the two categories)
    let mut mixed = Vec::new();
    for i in 0..10 {
        mixed.push(loops[(i * 7) % 10].clone());
        if i < 3 {
            mixed.push(loops[10 + (2 - i)].clone());
        }
    }
    assert_eq!(LevelSurvey::pick(&mixed, 4), pick);
    // nothing asked, nothing picked
    assert!(LevelSurvey::pick(&loops, 0).is_empty());
}

// the tolerance (`toleranceLU` 1.5) is an inclusive edge: a sample landing exactly on it is within
#[test]
fn a_sample_exactly_on_the_tolerance_is_within() {
    let r = LevelMatch::REFERENCE_LUFS;
    assert_eq!(LevelSurvey::TOLERANCE_LU, 1.5);
    // clamp-limited: the gain stops at +12, so lufs r-13.5 ends 1.5 short
    let edge = entry(Some(r - 13.5), -30.0, 0.3, 0.1);
    assert_eq!(edge.error_after(), Some(-1.5));
    assert_eq!(edge.limited_by(), None);
    assert_eq!(
        entry(Some(r - 13.6), -30.0, 0.3, 0.1).limited_by(),
        Some("maxGain")
    );
    // peak-limited: the peak (-5.5 dBFS) allows +4.5 dB of the +6 it wanted
    let peak_edge = entry(Some(r - 6.0), -5.5, 0.3, 0.1);
    assert_eq!(peak_edge.error_after(), Some(-1.5));
    assert_eq!(peak_edge.limited_by(), None);
    assert_eq!(
        entry(Some(r - 6.0), -5.4, 0.3, 0.1).limited_by(),
        Some("peak")
    );
    let s = LevelSurvey::new(vec![edge, peak_edge], vec![]);
    assert!(s.exceptions().is_empty());
    assert_eq!(int(&s.summary(), "within"), Some(2));
}

// an empty survey has no spreads and no maxima; failures are reported only when there are some
#[test]
fn an_empty_survey_has_no_spread_and_failures_are_listed() {
    let empty = LevelSurvey::default();
    assert_eq!(empty.spread_before(), None);
    assert_eq!(empty.spread_after(), None);
    let s = empty.summary();
    assert_eq!(int(&s, "measured"), Some(0));
    assert_eq!(int(&s, "within"), Some(0));
    for k in [
        "spreadBeforeLU",
        "spreadAfterLU",
        "maxErrorBeforeLU",
        "maxErrorAfterLU",
        "failures",
    ] {
        assert!(!s.contains_key(k), "{k}");
    }
    let f = LevelSurvey::new(vec![], vec![("a.wav".into(), "unreadable".into())]);
    assert_eq!(
        JsonValue::Object(f.summary()).canonical_js(),
        r#"{"exceptions":[],"failures":[{"error":"unreadable","name":"a.wav"}],"measured":0,"silent":0,"toleranceLU":1.5,"within":0}"#
    );
}

fn number(v: &JsonValue, key: &str) -> Option<f64> {
    match v.get(key) {
        Some(JsonValue::Null) | None => None,
        Some(x) => x.as_f64(),
    }
}

fn bits(v: &JsonValue, key: &str) -> Option<String> {
    match v.get(key) {
        Some(JsonValue::Null) | None => None,
        Some(x) => x.as_str().map(String::from),
    }
}

fn opt_bits(x: Option<f64>) -> Option<String> {
    x.map(bits_hex)
}

/// `levelsurvey.json`: every survey through the real Swift objects.
#[test]
fn surveys_match_swift() {
    let doc = fixture("levelsurvey");
    assert_eq!(f(&doc, "toleranceLU"), LevelSurvey::TOLERANCE_LU);
    let mut d = Diffs::default();
    for case in rows(&doc, "surveys") {
        let name = s(case, "name");
        let entries: Vec<SurveyEntry> = rows(case, "entries")
            .iter()
            .map(|e| {
                SurveyEntry::new(
                    s(e, "name"),
                    s(e, "category"),
                    SampleLevel::new(number(e, "lufs"), f(e, "peak"), f(e, "low"), f(e, "high")),
                )
            })
            .collect();
        let failures: Vec<(String, String)> = rows(case, "failures")
            .iter()
            .map(|x| (s(x, "name").to_string(), s(x, "error").to_string()))
            .collect();
        let survey = LevelSurvey::new(entries, failures);
        let results = rows(case, "results");
        d.eq(
            format!("{name}: entries"),
            survey.entries.len(),
            results.len(),
        );
        for (e, want) in survey.entries.iter().zip(results) {
            let tag = format!("{name}/{}", e.name);
            d.eq(
                format!("{tag}: errorBefore"),
                opt_bits(e.error_before()),
                bits(want, "errorBefore"),
            );
            d.eq(
                format!("{tag}: errorAfter"),
                opt_bits(e.error_after()),
                bits(want, "errorAfter"),
            );
            d.eq(
                format!("{tag}: limitedBy"),
                e.limited_by().map(String::from),
                bits(want, "limitedBy"),
            );
            d.eq(
                format!("{tag}: dictionary"),
                JsonValue::Object(e.dictionary()).canonical_js(),
                want.get("dictionary").unwrap().canonical_js(),
            );
        }
        d.eq(
            format!("{name}: spreadBefore"),
            opt_bits(survey.spread_before()),
            bits(case, "spreadBefore"),
        );
        d.eq(
            format!("{name}: spreadAfter"),
            opt_bits(survey.spread_after()),
            bits(case, "spreadAfter"),
        );
        let names: Vec<String> = survey.exceptions().iter().map(|e| e.name.clone()).collect();
        let want_names: Vec<String> = rows(case, "exceptions")
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        d.eq(format!("{name}: exceptions"), names, want_names);
        d.eq(
            format!("{name}: summary"),
            JsonValue::Object(survey.summary()).canonical_js(),
            case.get("summary").unwrap().canonical_js(),
        );
    }
    d.finish("levelsurvey surveys");
}

/// `levelsurvey.json`: `LevelSurvey.pick` over scrambled lists.
#[test]
fn picks_match_swift() {
    let doc = fixture("levelsurvey");
    let mut d = Diffs::default();
    for case in rows(&doc, "picks") {
        let name = s(case, "name");
        let loops: Vec<StockLoop> = rows(case, "loops")
            .iter()
            .map(|l| StockLoop {
                path: s(l, "path").into(),
                name: s(l, "path").into(),
                pack: "p".into(),
                category: s(l, "category").into(),
                subcategory: String::new(),
                genre: String::new(),
                beats: 8,
                seconds: 4.0,
                key: None,
            })
            .collect();
        for p in rows(case, "picks") {
            let per = i(p, "perCategory") as usize;
            let got: Vec<String> = LevelSurvey::pick(&loops, per)
                .into_iter()
                .map(|l| l.path)
                .collect();
            let want: Vec<String> = rows(p, "paths")
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            d.eq(format!("{name} x{per}"), got, want);
        }
    }
    d.finish("levelsurvey picks");
}
