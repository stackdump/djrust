//! Swift tests of the stock-loop value type that had no Rust counterpart (package F of the parity
//! audit): the tempo comes from the beat count over the length and snaps to a whole BPM
//! (`StockLoopsTests.tempoComesFromBeatsAndSnapsToAWholeBPM`). Matching, the categories list and
//! the Apple Loop metadata reader (`matchingFiltersThenSortsByTempoCountingHalfAndDoubleTime`,
//! `metadataPairsReadKeyValueStrings`) are not in the Rust port: Apple Loops are Mac-only, and
//! no `matching` / `categories` / `AppleLoopMetadata` exists to test.

use djrust::layers::StockLoop;

/// `StockLoopsTests.loop`: a loop of 8 beats that plays at `bpm`.
fn stock(name: &str, bpm: f64) -> StockLoop {
    StockLoop {
        path: format!("/x/{name}.caf"),
        name: name.into(),
        pack: "Pack".into(),
        category: "Drums".into(),
        subcategory: String::new(),
        genre: "House".into(),
        beats: 8,
        seconds: 8.0 * 60.0 / bpm,
        key: None,
    }
}

// StockLoopsTests.tempoComesFromBeatsAndSnapsToAWholeBPM: 8 beats in 3.9994 s (AAC runs a few
// frames short) is 120, not 120.02; a tempo that is not near a whole one is kept as it is.
#[test]
fn tempo_comes_from_beats_and_snaps_to_a_whole_bpm() {
    let l = StockLoop {
        path: String::new(),
        name: String::new(),
        pack: String::new(),
        category: String::new(),
        subcategory: String::new(),
        genre: String::new(),
        beats: 8,
        seconds: 3.9994104308390024,
        key: None,
    };
    assert_eq!(l.bpm(), 120.0);
    assert!((stock("x", 93.5).bpm() - 93.5).abs() < 1e-9);
}
