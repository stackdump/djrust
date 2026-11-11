//! `Session/PianoRoll.swift` and `Session/LoopLayers.swift`. The asserts are the literals of
//! the Swift suites `PianoRollTests`, `PianoRollPatternTests`, `DJComposeSkillTests`,
//! `LoopLayersTests` and `LayerPoolTests`, read off the envelope's JSON (Swift reads the
//! same facts through `Share.rawNetsProject()`, which phase 1 ports). There is no Swift on
//! this box, so the CID pin at the end is a regression pin of this implementation, not a
//! Swift value: `tools/paritydump` (`rolls`) writes the Swift one on a Mac.

use djrust::cid::share_cid;
use djrust::json::JsonValue;
use djrust::layers::{self, StockLoop};
use djrust::roll;

fn parse(s: &str) -> JsonValue {
    JsonValue::parse(s).unwrap()
}

fn env(s: &str) -> JsonValue {
    roll::envelope(&parse(s)).unwrap()
}

fn message(s: &str) -> String {
    match roll::envelope(&parse(s)) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

/// A net of the envelope.
fn net<'a>(e: &'a JsonValue, id: &str) -> &'a JsonValue {
    e.get("nets")
        .unwrap()
        .get(id)
        .unwrap_or_else(|| panic!("no net {id}"))
}

fn places(n: &JsonValue) -> usize {
    n.get("places").unwrap().as_object().unwrap().len()
}

/// The MIDI binding of transition `t`, if it has one.
fn binding<'a>(n: &'a JsonValue, t: &str) -> Option<&'a JsonValue> {
    n.get("transitions")?.get(t)?.get("midi")
}

fn note(n: &JsonValue, t: &str) -> Option<i64> {
    binding(n, t)
        .and_then(|b| b.get("note"))
        .and_then(JsonValue::as_i64)
}

fn bindings(n: &JsonValue) -> usize {
    n.get("transitions")
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, t)| t.get("midi").is_some())
        .count()
}

fn net_ids(e: &JsonValue) -> Vec<String> {
    e.get("nets")
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .map(String::from)
        .collect()
}

// PianoRollTests.aRollBecomesOneRingPerPartWithTheNotesOnTheirSteps
#[test]
fn a_roll_becomes_one_ring_per_part_with_the_notes_on_their_steps() {
    let e = env(
        r#"{"name":"T","tempo":96,"parts":{"kick":{"notes":[[0],[8]]},
         "bass":{"instrument":"acid","notes":[[0,36,4,90],[16,43]]}}}"#,
    );
    assert_eq!(e.get("name").unwrap().as_str(), Some("T"));
    assert_eq!(e.get("tempo").unwrap().as_f64(), Some(96.0));
    let kick = net(&e, "kick");
    let bass = net(&e, "bass");
    // 17 + 2 steps → two bars: both rings are 32 long
    assert_eq!(places(kick), 32);
    assert_eq!(places(bass), 32);
    assert_eq!(
        kick.get("transitions").unwrap().as_object().unwrap().len(),
        32
    );
    let ch = |n: &JsonValue| n.get("track").unwrap().get("channel").unwrap().as_i64();
    assert_eq!(ch(kick), Some(10));
    assert_eq!(ch(bass), Some(6));
    assert_eq!(
        bass.get("track")
            .unwrap()
            .get("instrument")
            .unwrap()
            .as_str(),
        Some("acid")
    );
    // the transition leaving step i carries the note that starts there
    assert_eq!(note(kick, "t0"), Some(36));
    assert_eq!(note(kick, "t8"), Some(36)); // GM kick
    assert!(binding(kick, "t4").is_none());
    assert_eq!(note(bass, "t0"), Some(36));
    assert_eq!(
        binding(bass, "t0")
            .unwrap()
            .get("durationSteps")
            .unwrap()
            .as_i64(),
        Some(4)
    );
    assert_eq!(
        binding(bass, "t0")
            .unwrap()
            .get("velocity")
            .unwrap()
            .as_i64(),
        Some(90)
    );
    assert_eq!(note(bass, "t16"), Some(43));
    assert_eq!(
        binding(bass, "t16")
            .unwrap()
            .get("durationSteps")
            .unwrap()
            .as_i64(),
        Some(2)
    );
    // one token, in step 0
    let p = |id: &str| bass.get("places").unwrap().get(id).unwrap();
    assert_eq!(p("p0").get("initial").unwrap().ints(), Some(vec![1]));
    assert!(p("p1").get("initial").is_none()); // Swift reads that as [0]
                                               // the ring closes: the last transition returns to p0
    let arcs = bass.get("arcs").unwrap().as_array().unwrap();
    let has = |s: &str, t: &str| {
        arcs.iter().any(|a| {
            a.get("source").unwrap().as_str() == Some(s)
                && a.get("target").unwrap().as_str() == Some(t)
        })
    };
    assert!(has("t31", "p0") && has("p5", "t5"));
}

// PianoRollTests.notesOnTheSameStepSpillIntoSiblingNetsOfTheSameRole
#[test]
fn notes_on_the_same_step_spill_into_sibling_nets_of_the_same_role() {
    let e = env(r#"{"steps":16,"parts":{"harmony":{"notes":[
          {"step":0,"note":60,"dur":16},{"step":0,"note":64,"dur":16},{"step":0,"note":67,"dur":16}]}}}"#);
    assert_eq!(net_ids(&e), ["harmony", "harmony-b", "harmony-c"]);
    let mut notes: Vec<i64> = ["harmony", "harmony-b", "harmony-c"]
        .iter()
        .filter_map(|id| note(net(&e, id), "t0"))
        .collect();
    notes.sort();
    assert_eq!(notes, [60, 64, 67]);
    let b = net(&e, "harmony-b");
    assert_eq!(b.get("riffGroup").unwrap().as_str(), Some("harmony"));
    assert_eq!(b.get("riffVariant").unwrap().as_str(), Some("b"));
    assert_eq!(
        b.get("track").unwrap().get("channel").unwrap().as_i64(),
        Some(7)
    );
    assert_eq!(
        b.get("track").unwrap().get("group").unwrap().as_str(),
        Some("harmony")
    );
}

// PianoRollTests.theEnvelopeLoadsAsARawNetsShare
#[test]
fn the_envelope_loads_as_a_raw_nets_share() {
    let e = env(r#"{"parts":{"melody":{"notes":[[0,72]]}}}"#);
    let share = djrust::share::Share::from_payload(&e, Some(&share_cid(&e)));
    assert!(share.nets.is_some());
}

// PianoRollTests.badRollsSayWhatToFix
#[test]
fn bad_rolls_say_what_to_fix() {
    assert!(message("{}").contains("`parts`"));
    assert!(message(r#"{"parts":{"bass":{}}}"#).contains("needs `notes`"));
    assert!(message(r#"{"parts":{"bass":{"notes":[[0,200]]}}}"#).contains("0…127"));
    assert!(message(r#"{"parts":{"bass":{"notes":[[-1,40]]}}}"#).contains("`step`"));
    assert!(
        message(r#"{"steps":16,"parts":{"bass":{"notes":[[16,40]]}}}"#).contains("past `steps`")
    );
    assert!(message(r#"{"tempo":5,"parts":{"kick":{"notes":[[0]]}}}"#).contains("`tempo`"));
    assert!(message(r#"{"steps":5000,"parts":{"kick":{"notes":[[0]]}}}"#).contains("`steps`"));
}

// PianoRollPatternTests.aRepeatedPatternFillsTheRollFromWhereItStarts
#[test]
fn a_repeated_pattern_fills_the_roll_from_where_it_starts() {
    let e =
        env(r#"{"steps":256,"parts":{"kick":{"every":16,"from":64,"notes":[[0],[4],[8],[12]]}}}"#);
    let kick = net(&e, "kick");
    assert_eq!(places(kick), 256);
    assert!(binding(kick, "t0").is_none() && binding(kick, "t60").is_none());
    assert_eq!(note(kick, "t64"), Some(36));
    assert_eq!(note(kick, "t68"), Some(36));
    assert_eq!(note(kick, "t252"), Some(36));
    assert_eq!(bindings(kick), 48); // 12 bars × 4
}

// PianoRollPatternTests.segmentsAndUntilBuildAnArrangement
#[test]
fn segments_and_until_build_an_arrangement() {
    let e = env(r#"{"parts":{"melody":[
        {"every":16,"until":32,"notes":[[0,72,4]]},
        {"from":32,"until":64,"every":32,"notes":[[0,76,8],[16,79,8]]}]}}"#);
    let m = net(&e, "melody");
    assert_eq!(places(m), 64); // from the longest `until`
    assert_eq!(note(m, "t0"), Some(72));
    assert_eq!(note(m, "t16"), Some(72));
    assert_eq!(note(m, "t32"), Some(76));
    assert_eq!(note(m, "t48"), Some(79));
    assert_eq!(bindings(m), 4);
}

// PianoRollPatternTests.aRepeatNeedsAnEndAndNotesStayInsideIt
#[test]
fn a_repeat_needs_an_end_and_notes_stay_inside_it() {
    assert!(message(r#"{"parts":{"kick":{"every":16,"notes":[[0]]}}}"#).contains("`until`"));
    assert!(
        message(r#"{"parts":{"kick":{"every":0,"until":16,"notes":[[0]]}}}"#).contains("`every`")
    );
    assert_eq!(
        message(r#"{"steps":16,"parts":{"kick":{"every":8,"until":12,"notes":[[0],[6]]}}}"#),
        ""
    );
    assert!(
        message(r#"{"steps":16,"parts":{"kick":{"from":8,"notes":[[10]]}}}"#)
            .contains("past `steps`")
    );
}

// DJComposeSkillTests.theSkillExampleBuildsAPlayableRoll
#[test]
fn the_skill_example_builds_a_playable_roll() {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/beats-btw-dj/dj-compose-example.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let e = roll::envelope(&JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(e.get("tempo").unwrap().as_f64(), Some(124.0));
    let kick = net(&e, "kick");
    assert_eq!(places(kick), 256);
    assert_eq!(bindings(kick), 64);
    let clap = net(&e, "clap");
    assert!(binding(clap, "t4").is_none());
    assert_eq!(note(clap, "t68"), Some(39));
    let bass = net(&e, "bass");
    assert_eq!(note(bass, "t66"), Some(45));
    assert_eq!(note(bass, "t242"), Some(43));
    let ids = net_ids(&e);
    for want in ["harmony", "harmony-b", "harmony-c", "melody"] {
        assert!(ids.iter().any(|i| i == want), "{want} missing from {ids:?}");
    }
    let mel = net(&e, "melody");
    assert_eq!(note(mel, "t128"), Some(72));
    assert!(binding(mel, "t127").is_none());
    assert_eq!(
        net(&e, "harmony")
            .get("track")
            .unwrap()
            .get("instrument")
            .unwrap()
            .as_str(),
        Some("warm-pad")
    );
}

#[test]
fn inspired_by_names_the_piece_and_writes_the_note() {
    let e = env(
        r#"{"inspiredBy":"Daft Punk","brief":"122 BPM","key":"A minor","parts":{"kick":{"notes":[[0]]}}}"#,
    );
    assert_eq!(
        e.get("name").unwrap().as_str(),
        Some("Daft Punk-style (original)")
    );
    assert_eq!(
        e.get("note").unwrap().as_str(),
        Some("Original piece inspired by Daft Punk: style only, no melody or lyric taken. Brief: 122 BPM")
    );
    assert_eq!(e.get("djKey").unwrap().as_str(), Some("A minor"));
    assert_eq!(
        env(r#"{"parts":{"kick":{"notes":[[0]]}}}"#)
            .get("name")
            .unwrap()
            .as_str(),
        Some("Piano roll")
    );
}

/// Regression pin, not a Swift value: the same roll must keep producing the same canonical
/// text and CID. (`tools/paritydump` `rolls` records Swift's for comparison on a Mac.)
#[test]
fn the_envelope_is_deterministic_and_content_addressed() {
    let src = r#"{"name":"T","tempo":96,"parts":{"kick":{"notes":[[0],[8]]},"bass":{"instrument":"acid","notes":[[0,36,4,90],[16,43]]}}}"#;
    let a = env(src);
    let b = env(src);
    assert_eq!(a.canonical_js(), b.canonical_js());
    assert_eq!(share_cid(&a), share_cid(&b));
    assert_eq!(
        share_cid(&a),
        "z4EBG9jGP8fSkBPDViG6eJB7RuoZYyW47SvfdhRrDyQ2eAM6Z55"
    );
    let other = env(&src.replace("[16,43]", "[16,44]"));
    assert_ne!(share_cid(&a), share_cid(&other));
}

// ---- LoopLayers ----------------------------------------------------------------------

fn stock(name: &str, bpm: f64, beats: i64, category: &str, key: Option<&str>) -> StockLoop {
    StockLoop {
        path: format!("/lib/{name}.caf"),
        name: name.into(),
        pack: "14 Future Bass".into(),
        category: category.into(),
        subcategory: String::new(),
        genre: String::new(),
        beats,
        seconds: beats as f64 * 60.0 / bpm,
        key: key.map(Into::into),
    }
}

// LoopLayersTests.aLayerNamesALoopABarAndHowLongItStays
#[test]
fn a_layer_names_a_loop_a_bar_and_how_long_it_stays() {
    let specs = layers::parse(Some(&parse(
        r#"[{"loop":"Heavy Tech Riser 01","bar":28,"bars":4,"gain":-6},
       {"loop":"Chop Vox","bar":[13,29],"keyLock":false}]"#,
    )))
    .unwrap();
    assert_eq!(specs.len(), 2);
    assert!(
        specs[0].bars == [28]
            && specs[0].length == Some(4)
            && specs[0].gain_db == -6.0
            && specs[0].key_lock
    );
    assert!(
        specs[1].bars == [13, 29]
            && specs[1].length.is_none()
            && specs[1].gain_db == -3.0
            && !specs[1].key_lock
    );
    assert!(layers::parse(Some(&parse("[]"))).unwrap().is_empty());
    assert!(layers::parse(None).unwrap().is_empty());
    assert_eq!(layers::parse(Some(&layers::json(&specs))).unwrap(), specs);
}

// LoopLayersTests.badLayersSayWhatToFix
#[test]
fn bad_layers_say_what_to_fix() {
    let m = |j: &str| match layers::parse(Some(&parse(j))) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    };
    assert!(m(r#"{"loop":"x"}"#).contains("must be a list"));
    assert!(m(r#"[{"bar":3}]"#).contains("`loop`"));
    assert!(m(r#"[{"loop":"x"}]"#).contains("`bar`"));
    assert!(m(r#"[{"loop":"x","bar":0}]"#).contains("`bar`"));
    assert!(m(r#"[{"loop":"x","bar":2,"bars":99}]"#).contains("`bars`"));
    assert!(m(r#"[{"loop":"x","bar":2,"gain":6}]"#).contains("`gain`"));
    let many = format!("[{}]", vec![r#"{"loop":"x","bar":2}"#; 13].join(","));
    assert!(m(&many).contains("at most"));
}

// LoopLayersTests.windowsDefaultToOnePassOfTheLoopAndGainIsLinearUnderUnity
#[test]
fn windows_default_to_one_pass_of_the_loop_and_gain_is_linear_under_unity() {
    let s = layers::parse(Some(&parse(
        r#"[{"loop":"x","bar":[5,21]},{"loop":"y","bar":9,"bars":2}]"#,
    )))
    .unwrap();
    assert_eq!(layers::windows(&s[0], 16), [(5, 9), (21, 25)]);
    assert_eq!(
        layers::windows(&s[0], 6)
            .iter()
            .map(|w| w.1 - w.0)
            .collect::<Vec<_>>(),
        [2, 2]
    );
    assert_eq!(layers::windows(&s[1], 32), [(9, 11)]);
    assert!(
        layers::gain(0.0) == 1.0
            && (layers::gain(-6.0) - 0.501).abs() < 0.01
            && layers::gain(-60.0) < 0.01
    );
    assert_eq!(layers::gain(6.0), 1.0);
}

// LoopLayersTests.loopsAreFoundByPathNameOrUniquePartAndAmbiguityIsAsked
#[test]
fn loops_are_found_by_path_name_or_unique_part_and_ambiguity_is_asked() {
    let lib = [
        stock("Heavy Tech Riser 01", 140.0, 16, "Vocals", Some("F")),
        stock("Heavy Tech Riser 02", 140.0, 16, "Vocals", Some("F")),
        stock("Frisky Chop Vox 01", 130.0, 16, "Vocals", Some("F")),
    ];
    let name = |q: &str| layers::resolve(q, &lib, None).ok().map(|l| l.name);
    assert_eq!(
        name("/lib/Heavy Tech Riser 02.caf").as_deref(),
        Some("Heavy Tech Riser 02")
    );
    assert_eq!(
        name("heavy tech riser 01").as_deref(),
        Some("Heavy Tech Riser 01")
    );
    assert_eq!(name("frisky").as_deref(), Some("Frisky Chop Vox 01"));
    let amb = layers::resolve("heavy tech", &lib, None)
        .unwrap_err()
        .to_string();
    assert!(amb.contains("matches 2 loops") && amb.contains("Riser 02"));
    let none = layers::resolve("nothing like it", &lib, None)
        .unwrap_err()
        .to_string();
    assert!(none.contains("no loop called"));
}

// LoopLayersTests.theNearestTempoSettlesAnAmbiguousNameOnlyWhenItClearlyWins
#[test]
fn the_nearest_tempo_settles_an_ambiguous_name_only_when_it_clearly_wins() {
    let lib = [
        stock("Vox A 70", 70.0, 16, "Vocals", Some("F")),
        stock("Vox A 140", 140.0, 16, "Vocals", Some("F")),
    ];
    assert_eq!(
        layers::resolve("vox a", &lib, Some(140.0)).unwrap().name,
        "Vox A 140"
    );
    let close = [
        stock("Vox B 139", 139.0, 16, "Vocals", Some("F")),
        stock("Vox B 141", 141.0, 16, "Vocals", Some("F")),
    ];
    assert!(layers::resolve("vox b", &close, Some(140.0)).is_err());
}

// LoopLayersTests.aRollCarriesItsLayersInTheEnvelope
#[test]
fn a_roll_carries_its_layers_in_the_envelope() {
    let e = env(
        r#"{"parts":{"kick":{"notes":[[0]]}},"layers":[{"loop":"Heavy Tech Riser 01","bar":3,"bars":2,"gain":-9}]}"#,
    );
    let l = e.get("loopLayers").unwrap().as_array().unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(
        l[0].get("loop").unwrap().as_str(),
        Some("Heavy Tech Riser 01")
    );
    assert_eq!(
        l[0].get("bar").unwrap(),
        &JsonValue::Array(vec![JsonValue::Number(3.0)])
    );
    assert_eq!(l[0].get("gain").unwrap().as_f64(), Some(-9.0));
    assert!(env(r#"{"parts":{"kick":{"notes":[[0]]}}}"#)
        .get("loopLayers")
        .is_none());
    assert!(roll::envelope(&parse(
        r#"{"parts":{"kick":{"notes":[[0]]}},"layers":[{"bar":1}]}"#
    ))
    .is_err());
}

// LayerPoolTests.thePoolIsLayerCategoriesAtAMatchingTempoInAFittingKey
#[test]
fn the_pool_is_layer_categories_at_a_matching_tempo_in_a_fitting_key() {
    let lib = [
        stock("Vox Ab", 140.0, 16, "Vocals", Some("Ab")),
        stock("Vox F", 140.0, 16, "Vocals", Some("F")),
        stock("Vox D", 140.0, 16, "Vocals", Some("D")),
        stock("Riser", 140.0, 16, "Sound Effect", None),
        stock("Half Riser", 70.0, 16, "Sound Effect", None),
        stock("Slow Riser", 100.0, 16, "Sound Effect", None),
        stock("Drums", 140.0, 16, "Drums", None),
        stock("Texture", 140.0, 16, "Texture/Atmosphere", None),
    ];
    let names = |key: Option<&str>, bpm: f64| -> Vec<String> {
        layers::pool(&lib, bpm, key, 400)
            .into_iter()
            .map(|l| l.name)
            .collect()
    };
    assert_eq!(
        names(Some("C minor"), 140.0),
        ["Half Riser", "Riser", "Texture", "Vox Ab", "Vox F"]
    );
    assert_eq!(names(None, 140.0), ["Half Riser", "Riser", "Texture"]);
    let n = names(Some("C minor"), 140.0);
    assert!(!n.contains(&"Slow Riser".to_string()) && !n.contains(&"Drums".to_string()));
    let n70 = names(Some("C minor"), 70.0);
    assert!(n70.contains(&"Riser".to_string()) && n70.contains(&"Half Riser".to_string()));
    assert!(layers::pool(&lib, 0.0, Some("C minor"), 400).is_empty());
}

// LayerPoolTests.aBigLibraryIsSampledTheSameWayEveryTime
#[test]
fn a_big_library_is_sampled_the_same_way_every_time() {
    let lib: Vec<StockLoop> = (0..1500)
        .map(|i| stock(&format!("Fx {i}"), 140.0, 16, "Sound Effect", None))
        .collect();
    let a = layers::pool(&lib, 140.0, None, 200);
    assert_eq!(a.len(), 200);
    assert_eq!(a, layers::pool(&lib, 140.0, None, 200));
    let paths: std::collections::BTreeSet<_> = a.iter().map(|l| l.path.clone()).collect();
    assert_eq!(paths.len(), 200);
    assert_eq!(layers::pool(&lib, 140.0, None, 5000).len(), 1500);
}
