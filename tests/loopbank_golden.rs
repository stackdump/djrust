//! Swift goldens for the loop row on the engine (`tools/paritydump/loopbank.swift`, section
//! "loopbank"): the Swift offline `DJEngine` driven through launches on a beat, a bar and a
//! phrase, a clip's own boundary, a scene on one boundary, a start waiting on a long line while
//! the deck's tempo moves, and the row through `LoopLibrary.save`; the Rust engine does the
//! same operations at the same frames and is compared exactly: switch frames, states, countdowns
//! (by their bits), target tempi, ids, and the library's files as text.
//!
//! Parity form (b) of the port rules. Frames are integers and compared exactly (the loop clock
//! takes its origin from the deck's newest tick anchor, as Swift's does, so no frame is off by
//! one any more); the dump's floats are compared by bit pattern.

mod common_fixture;
mod common_row;

use common_row::*;
use djrust::deck::loops::{LoopFollow, LoopState};
use djrust::json::{JsonObject, JsonValue};
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::host::deck::loops::LoopClip;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("loopbank")
}

fn bits(x: f64) -> JsonValue {
    JsonValue::str(format!("{:016x}", x.to_bits()))
}

fn int(x: i64) -> JsonValue {
    JsonValue::int(x)
}

fn state(s: LoopState) -> &'static str {
    match s {
        LoopState::Off => "off",
        LoopState::Starting => "starting",
        LoopState::On => "on",
        LoopState::Stopping => "stopping",
    }
}

fn obj<const N: usize>(p: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(p))
}

/// `lbSnapshot`.
fn snapshot(name: &str, rig: &Rig, clips: &[LoopClip]) -> JsonValue {
    let e = &rig.e;
    obj([
        ("name", JsonValue::str(name)),
        ("frame", int(e.loop_frames())),
        (
            "clips",
            JsonValue::Array(
                clips
                    .iter()
                    .map(|c| {
                        let cd = e.loop_countdown(&c.id);
                        obj([
                            ("id", JsonValue::str(c.id.as_str())),
                            ("state", JsonValue::str(state(e.loop_state(&c.id)))),
                            ("switch", e.loop_switch_frame(&c.id).map_or(JsonValue::Null, int)),
                            ("countdown", cd.map_or(JsonValue::Null, |c| bits(c.0))),
                            ("countdownOf", cd.map_or(JsonValue::Null, |c| bits(c.1))),
                            ("target", e.loop_target_bpm(&c.id).map_or(JsonValue::Null, bits)),
                            ("gain", bits(e.clip_gain(&c.id))),
                            ("quantum", int(e.quantum_for(Some(&c.id)))),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "looping",
            JsonValue::Array(e.looping_clips().into_iter().map(JsonValue::str).collect()),
        ),
        (
            "dominant",
            e.loops.dominant_deck().map_or(JsonValue::Null, JsonValue::str),
        ),
    ])
}

fn launch_steps() -> Vec<JsonValue> {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(2 * BAR + 1_024);
    let (x, y) = (test_clip(1), test_clip(2));
    rig.e.restore_clips(&[x.clone(), y.clone()]);
    let both = [x.clone(), y.clone()];
    let mut steps = vec![snapshot("idle", &rig, &both)];
    macro_rules! snap {
        ($name:expr) => {
            steps.push(snapshot($name.as_ref(), &rig, &both))
        };
    }
    macro_rules! settle {
        ($frames:expr) => {{
            rig.run($frames);
            snap!("settled".to_string());
        }};
    }
    for q in [1i64, 4, 16] {
        rig.e.set_loop_quantum(q);
        rig.e.set_looping(&x.id, true);
        snap!(format!("x on, quantum {q}"));
        rig.run(3_000);
        snap!("x on, 3000 frames on".to_string());
        settle!(q * 24_000 + 8_192);
        rig.e.set_looping(&x.id, false);
        snap!(format!("x off, quantum {q}"));
        settle!(q * 24_000 + 8_192);
    }
    // a clip's own boundary, and the row's for the other
    rig.e.set_loop_quantum(1);
    rig.e.set_clip_quantum(&y.id, Some(8));
    rig.e.set_clip_quantum(&x.id, Some(3)); // refused
    rig.e.set_looping(&x.id, true);
    rig.e.set_looping(&y.id, true);
    snap!("x on the row's beat, y on its own two bars".to_string());
    settle!(8 * 24_000 + 8_192);
    rig.e.set_looping(&y.id, false);
    snap!("y off on its own line".to_string());
    settle!(8 * 24_000 + 8_192);

    // a scene: x at 0.6, then y alone at 1, launched on a 2-bar line
    rig.e.set_clip_gain(&x.id, 0.6, None);
    rig.e.save_scene("Scene 1");
    rig.e.set_looping(&y.id, true);
    settle!(8 * 24_000 + 8_192);
    rig.e.set_looping(&x.id, false);
    settle!(2 * 24_000 + 8_192);
    rig.e.set_clip_gain(&x.id, 0.2, None);
    rig.e.set_clip_quantum(&y.id, None);
    rig.e.set_loop_quantum(8);
    let at = rig.e.launch_scene("Scene 1");
    steps.push(obj([("name", JsonValue::str("scene launched")), ("at", at.map_or(JsonValue::Null, int))]));
    snap!("scene 1 launched".to_string());
    rig.run_to(at.unwrap_or(rig.frame()) + 4_096);
    snap!("scene 1 on".to_string());
    steps.push(obj([
        ("name", JsonValue::str("unknown scene")),
        ("at", rig.e.launch_scene("nope").map_or(JsonValue::Null, int)),
    ]));

    // a start waiting on an 8-bar line follows deck A's tempo when it moves
    rig.e.set_looping(&x.id, false);
    rig.e.set_looping(&y.id, false);
    settle!(2 * 24_000 + 8_192);
    rig.e.set_loop_quantum(32);
    loop {
        let clock = rig.e.loop_clock(Some(&x.id), None);
        let f = rig.e.loops.mirror.rendered.load(std::sync::atomic::Ordering::Relaxed);
        if clock.next_boundary(f + djrust::host::deck::loops::LOOP_LEAD, 32) - f >= 5 * BAR {
            break;
        }
        rig.run(48_000);
    }
    rig.e.set_looping(&x.id, true);
    snap!("x waiting on an 8-bar line".to_string());
    rig.run(BAR);
    rig.e.set_pitch("A", 0.625); // Swift: `a.pitch = 0.625` (the fader, not the jog nudge)
    rig.run(8 * BLOCK as i64);
    snap!("deck A's tempo moved".to_string());
    let moved = rig.e.loop_switch_frame(&x.id).unwrap_or(rig.frame());
    rig.run_to(moved + 48_000);
    snap!("x in on the moved frame".to_string());
    steps
}

/// Rust against Swift, exactly: switch frames, launch lines and `at`s are integers, countdowns
/// are the bits of the beat count, and the rest (ids, states, targets, gains, names) is text.
///
/// Swift lines a loop up with the deck's newest tick anchor (an exact frame and beat); so does the
/// Rust engine now (`ClockDeck::anchor`, read from the deck's anchor ring). Until it did, it
/// read the deck's beat state as published at the end of the last block, which put the clock's
/// origin up to 2 frames off, and 15 frames off after deck A's tempo moved (26 beats of the new
/// tempo lie between the estimate and the anchor): this comparison then had tolerances.
fn compare(got: &JsonValue, want: &JsonValue, path: &str, bad: &mut Vec<String>) {
    match (got, want) {
        (JsonValue::Object(g), JsonValue::Object(w)) => {
            for (k, wv) in w.iter() {
                let p = format!("{path}.{k}");
                match g.get(k) {
                    Some(gv) => compare(gv, wv, &p, bad),
                    None => bad.push(format!("{p}: missing")),
                }
            }
        }
        (JsonValue::Array(g), JsonValue::Array(w)) if g.len() == w.len() => {
            for (i, (gv, wv)) in g.iter().zip(w).enumerate() {
                compare(gv, wv, &format!("{path}[{i}]"), bad);
            }
        }
        _ if got == want => {}
        _ => bad.push(format!("{path}: rust {} swift {}", got.canonical_js(), want.canonical_js())),
    }
}

#[test]
fn launch_frames_states_and_countdowns_match_swift() {
    let g = golden();
    let want = g.get("launch").unwrap().as_array().unwrap();
    let got = launch_steps();
    assert_eq!(got.len(), want.len(), "same number of steps");
    let mut bad = Vec::new();
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let name = w.get("name").and_then(JsonValue::as_str).unwrap_or("?");
        compare(g, w, &format!("step {i} ({name})"), &mut bad);
    }
    assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn the_row_through_the_library_matches_swift() {
    use djrust::host::stores::loops::LoopLibrary;
    let g = golden();
    let want = g.get("library").unwrap();
    let mut rig = Rig::new();
    let e = &mut rig.e;
    let (x, y, z) = (test_clip(1), test_clip(2), test_clip(3));
    e.restore_clips(&[x.clone(), y.clone(), z.clone()]);
    e.set_clip_quantum(&y.id, Some(16));
    e.set_clip_fade(&y.id, Some(djrust::deck::clips::LoopFade::Bar));
    e.set_clip_follow(&z.id, Some(LoopFollow::Source));
    e.set_clip_color(&x.id, Some("teal"));
    e.rename_clip(&z.id, "  the three  ");
    e.set_clip_gain(&y.id, 0.4, None);
    e.set_looping(&y.id, true);
    e.save_scene("S");
    let crunch = LoopFx { drive: 9.0, mode: LoopFxMode::Hard, amount: 0.55, tone: 0.75, ..LoopFx::default() };
    e.set_clip_fx(&x.id, &crunch, None);
    let dir = djrust::host::stores::temp_path("loopbank");
    let lib = LoopLibrary::new(&dir);
    let when = 1_700_000_000_000;
    lib.save(&e.loops.row.clips, Some(e.scenes()), Some(&e.clip_launch()), when).unwrap();
    let cropped = e.crop_clip(&z.id, 1, 3);
    e.remove_clip(&x.id);
    lib.save(&e.loops.row.clips, Some(e.scenes()), Some(&e.clip_launch()), when).unwrap();

    let strs = |v: &JsonValue| -> Vec<String> { v.strings().unwrap() };
    let row: Vec<String> = e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
    assert_eq!(row, strs(want.get("rowIds").unwrap()));
    let removed: Vec<String> = e.loops.row.removed.iter().map(|r| r.clip.id.clone()).collect();
    assert_eq!(removed, strs(want.get("removedIds").unwrap()));
    assert_eq!(cropped.as_deref(), want.get("cropped").and_then(JsonValue::as_str));
    // the launch settings of the row
    let launch = e.clip_launch();
    let wl = want.get("clipLaunch").unwrap().as_array().unwrap();
    assert_eq!(launch.len(), wl.len());
    for w in wl {
        let l = &launch[w.get("id").unwrap().as_str().unwrap()];
        assert_eq!(l.quantum, w.get("quantum").and_then(JsonValue::as_i64));
        assert_eq!(l.fade.map(|f| f.raw()), w.get("fade").and_then(JsonValue::as_str));
        assert_eq!(l.follow.map(|f| f.raw()), w.get("follow").and_then(JsonValue::as_str));
    }
    // the files, as text
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    assert_eq!(read(lib.index_file()), want.get("library").unwrap().as_str().unwrap());
    assert_eq!(read(lib.removed_index_file()), want.get("removed").unwrap().as_str().unwrap());
    let names = |d: std::path::PathBuf| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    assert_eq!(names(dir.clone()), strs(want.get("files").unwrap()));
    assert_eq!(names(lib.removed_dir()), strs(want.get("removedFiles").unwrap()));
    // and back
    let mut again = Rig::new();
    again.e.restore_clips(&lib.load(48_000.0));
    again.e.restore_scenes(&lib.load_scenes());
    again.e.restore_clip_launch(&lib.load_launch());
    let e2 = &again.e;
    assert_eq!(e2.loops.row.clips.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), strs(want.get("loadedIds").unwrap()));
    assert_eq!(e2.loops.row.clips.iter().map(|c| c.title.clone()).collect::<Vec<_>>(), strs(want.get("loadedTitles").unwrap()));
    let colours: Vec<Option<String>> = want.get("loadedColors").unwrap().as_array().unwrap().iter().map(|v| v.as_str().map(String::from)).collect();
    assert_eq!(e2.loops.row.clips.iter().map(|c| c.color.clone()).collect::<Vec<_>>(), colours);
    assert_eq!(e2.loops.row.clips.iter().map(|c| c.fx.json().canonical_js()).collect::<Vec<_>>(), strs(want.get("loadedFx").unwrap()));
    let scenes: Vec<String> = e2.scenes().iter().map(|s| s.json().canonical_js()).collect();
    let want_scenes: Vec<String> = want.get("loadedScenes").unwrap().as_array().unwrap().iter().map(|v| v.canonical_js()).collect();
    assert_eq!(scenes, want_scenes);
    assert_eq!(e2.clip_launch().keys().cloned().collect::<Vec<_>>(), strs(want.get("loadedLaunchKeys").unwrap()));
    let _ = std::fs::remove_dir_all(dir);
}

/// `lbClipAt`: the one-bar test clip claiming tempo `bpm`.
fn clip_at(n: usize, bpm: f64) -> LoopClip {
    let mut c = test_clip(n);
    c.bpm = bpm;
    c
}

/// `deck` of `dumpClipOnDeck`: what a deck reads after the load and a block.
fn deck_row(name: &str, rig: &Rig, deck: &str) -> JsonValue {
    use djrust::control::controller::DeckView;
    let d = rig.e.deck(deck).unwrap();
    let opt_bits = |v: Option<f64>| v.map_or(JsonValue::Null, bits);
    let (looping, start, end) = d
        .sample
        .as_ref()
        .map_or((false, None, None), |s| (s.looping, s.loop_start, s.loop_end));
    obj([
        ("name", JsonValue::str(name)),
        ("deck", JsonValue::str(deck)),
        ("pitch", bits(d.pitch)),
        ("netNudge", bits(d.net_nudge)),
        ("baseBPM", opt_bits(d.base_bpm())),
        ("effectiveBPM", opt_bits(DeckView::effective_bpm(d))),
        ("clipID", d.clip_id.as_deref().map_or(JsonValue::Null, JsonValue::str)),
        ("playing", JsonValue::Bool(d.playing)),
        ("looping", JsonValue::Bool(looping)),
        ("loopStart", opt_bits(start)),
        ("loopEnd", opt_bits(end)),
        ("position", bits(d.mirror.turntable_position())),
    ])
}

/// `dumpClipOnDeck` (loopbank.swift): `DeckAction.clip` on deck B.
fn clip_on_deck_rows() -> Vec<JsonValue> {
    use djrust::session::action::DeckAction;
    use djrust::host::deck::PerformCtx;
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(2 * BAR + 1_024);
    let x = test_clip(1); // 120 bpm
    let half = clip_at(3, 60.0); // half time of A
    let far = clip_at(4, 100.0); // out of the fader's reach
    let y = test_clip(2);
    rig.e.restore_clips(&[x.clone(), y.clone(), half.clone(), far.clone()]);
    let mut rows: Vec<JsonValue> = Vec::new();
    macro_rules! put {
        ($name:expr, $id:expr) => {{
            let a = DeckAction::Clip { deck: "B".into(), id: $id.clone() };
            rig.e.perform(&a, PerformCtx::default());
            rig.run(BLOCK as i64);
            rows.push(deck_row($name, &rig, "B"));
        }};
    }
    rig.e.set_pitch("A", 0.5);
    rig.run(BLOCK as i64);
    rows.push(deck_row("A moved", &rig, "A"));
    rig.e.set_pitch("B", 0.4);
    rig.e.nudge("B", 0.02);
    rig.run(BLOCK as i64);
    rows.push(deck_row("B before the clip", &rig, "B"));
    put!("clip at 120 on B, against A at 124.8", x.id);
    rig.e.nudge("B", 0.0);
    rig.run(BLOCK as i64);
    rows.push(deck_row("B after nudge(0)", &rig, "B"));
    put!("clip at 60: half time", half.id);
    put!("clip at 100: clamped to the fader's end", far.id);
    rig.e.set_pitch("A", -1.0);
    rig.run(BLOCK as i64);
    put!("clip at 120 against A at 110.4", x.id);
    rig.e.nudge("B", 0.02);
    rig.e.set_pitch("B", 0.4);
    rig.e.pause("A");
    rig.run(BLOCK as i64);
    rows.push(deck_row("A paused, B stale", &rig, "B"));
    put!("clip with no reference deck", y.id);
    rows
}

/// `Deck.loadClip` puts a clip on deck A or B on the tempo fader: `pitch = (tempoMatch(base:
/// clip.bpm, target: ref.effectiveBPM).ratio - 1) / pitchRange` against the deck that is
/// playing, 0 with none, and the momentary nudge (`netNudge`) is not touched: so a stale fader
/// is overwritten, a held bend stays and a later `nudge(0)` leaves the match. The Rust port set
/// the nudge instead (fader untouched), which `DeckParams.pitch` (the journal checkpoint,
/// `dj_state`) never saw. Swift's own tests only look at the clip id, the loop and the grid
/// (`LoopRecordTests.aClipMovesOntoADeckAndBack`), so this is a paritydump section: every
/// number is Swift's, compared by bit pattern.
#[test]
fn a_clip_on_a_deck_takes_the_tempo_fader_as_swift_does() {
    let g = golden();
    let want = g.get("clipOnDeck").unwrap().as_array().unwrap();
    let got = clip_on_deck_rows();
    assert_eq!(got.len(), want.len(), "same number of rows");
    let mut bad = Vec::new();
    for (g, w) in got.iter().zip(want) {
        let name = w.get("name").and_then(JsonValue::as_str).unwrap_or("?");
        compare(g, w, name, &mut bad);
    }
    assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
}

/// The same behaviour as plain assertions (so a failure names it): the fader carries the match,
/// the bend is kept, and with no reference the fader goes to 0. Swift: `Deck.loadClip`.
#[test]
fn a_clip_on_a_deck_sets_the_fader_not_the_nudge() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(2 * BAR + 1_024);
    let (x, y) = (test_clip(1), test_clip(2));
    rig.e.restore_clips(&[x.clone(), y.clone()]);
    rig.e.set_pitch("A", 0.5); // 124.8 bpm
    rig.e.set_pitch("B", 0.4);
    rig.e.nudge("B", 0.02);
    rig.e.play_clip(&x.id, "B");
    let b = rig.e.deck("B").unwrap();
    let want = (124.8f64 / 120.0 - 1.0) / 0.08;
    assert!((b.pitch - want).abs() < 1e-12, "the fader is the match: {} vs {want}", b.pitch);
    assert_eq!(b.net_nudge, 0.02, "the held bend is not overwritten");
    rig.e.nudge("B", 0.0);
    assert!((rig.e.deck("B").unwrap().pitch - want).abs() < 1e-12, "a release leaves the fader");
    // no deck playing: back to 0
    rig.e.pause("A");
    rig.run(BLOCK as i64);
    rig.e.set_pitch("B", 0.4);
    rig.e.play_clip(&y.id, "B");
    assert_eq!(rig.e.deck("B").unwrap().pitch, 0.0);
}

/// `dumpProvenance` (loopbank.swift): `LoopProvenance.capture` over a deck as it is set, as the
/// canonical text of what was written, less the clock. Parity form (b): exact text, every key
/// and its order, so the knobs `currentParams` reads (quantize, the strip reverb, the walk), the
/// STEMS groups, the net macros (by Swift's case name) before the master ones (by id), the kit
/// and a raw-nets deck's palette are all held to what Swift writes.
#[test]
fn the_capture_of_a_deck_matches_swift() {
    use djrust::deck::beat::Quantize;
    use djrust::fx::{MasterMacro, StripKnob};
    use djrust::wave::NetMacro;
    use djrust::host::deck::track::PreparedTrack;

    let g = golden();
    let want = g.get("provenance").unwrap();
    let roll = want.get("roll").unwrap();
    // `DJEngine(decks: ["A", "B"], offline: true)` as the app builds it: the app's options (the
    // fixes move the track's key, which the walk's key lock holds)
    let (e, r) = djrust::host::deck::engine(djrust::host::deck::EngineConfig::new(SR));
    let mut rig = Rig { e, r, master: Vec::new() };
    rig.e.set_crossfader(0.0);
    rig.e.set_master(1.0);
    for d in ["A", "B"] {
        rig.e.set_volume(d, 1.0);
    }
    rig.net_deck(120.0);
    rig.run_to(2 * BAR + 1_024);
    let mut rows: Vec<(String, String, String)> = Vec::new();
    macro_rules! cap {
        ($name:expr, $deck:expr) => {{
            let mut p = rig.e.capture_provenance($deck, 4.0);
            p.remove("recorded"); // the wall clock
            rows.push(($name.to_string(), $deck.to_string(), JsonValue::Object(p).canonical_js()));
        }};
    }
    // the knobs `currentParams` reads: EQ and the tempo fader, then the ones the old capture left
    // out (quantize, the strip reverb, the walk)
    rig.e.set_strip("A", StripKnob::EqLow, 0.2);
    rig.e.set_pitch("A", 0.25);
    rig.run(1_000);
    cap!("EQ and the tempo fader", "A");
    rig.e.set_quantize("A", Quantize::Bar);
    rig.e.set_reverb("A", "reverbWet", 0.35);
    rig.e.set_reverb("A", "reverbSize", 0.6);
    rig.e.set_reverb("A", "reverbTone", 0.4);
    rig.e.deck_mut("A").unwrap().auto_advance = true;
    rig.e.set_wander("A", true);
    rig.e.set_wander_pace("A", 8);
    rig.e.set_wander_lock("A", "key", true);
    cap!("quantize, the strip reverb and the walk", "A");
    rig.e.set_wander("A", false);
    rig.e.deck_mut("A").unwrap().auto_advance = false;

    // a STEMS group and a role muted
    rig.e.toggle_stem("A", "mic");
    rig.e.set_role("A", "kick", false);
    rig.run(1_000);
    cap!("a stem and a role muted", "A");
    rig.e.toggle_stem("A", "music");
    rig.run(1_000);
    cap!("two stems muted", "A");
    rig.e.toggle_stem("A", "mic");
    rig.e.toggle_stem("A", "music");
    rig.e.set_role("A", "kick", true);
    rig.run(1_000);
    cap!("everything back", "A");

    // macros: the net ones in catalogue order, then the master ones
    rig.e.fire_duration("A", NetMacro::BeatRepeat, None);
    rig.e.fire_duration("A", NetMacro::Drop, None);
    rig.e.fire_master("A", MasterMacro::SweepLp, None);
    rig.run(12_000);
    cap!("net and master macros running", "A");
    rig.run(3 * 96_000);
    cap!("the macros out", "A");

    // a kit and a palette
    rig.e.set_kit("A", Some("kit-x"));
    rig.e.set_palette("A", Some("jazz"));
    rig.run(2 * 96_000);
    cap!("a kit and a palette", "A");

    // a raw-nets deck: the palette is the project's, not a spec's
    let pr = rig.e.preparer("B").unwrap();
    let track = PreparedTrack::from_raw_nets(roll, &pr).expect("a roll composes");
    assert!(rig.e.load_net("B", track));
    rig.run(2_000);
    cap!("a raw-nets deck", "B");
    rig.e.set_palette("B", Some("jazz"));
    rig.run(2_000);
    cap!("a raw-nets deck with a palette", "B");

    // a sample deck
    assert!(rig.e.load_demo("B"));
    rig.run(2_000);
    cap!("a sample deck", "B");

    let want_rows = want.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), want_rows.len(), "same number of rows");
    let mut bad = Vec::new();
    for ((name, deck, got), w) in rows.iter().zip(want_rows) {
        let wname = w.get("name").and_then(JsonValue::as_str).unwrap();
        assert_eq!((name.as_str(), deck.as_str()), (wname, w.get("deck").and_then(JsonValue::as_str).unwrap()));
        let text = w.get("provenance").and_then(JsonValue::as_str).unwrap();
        if got != text {
            // which keys differ, for the failure message
            let (g, t) = (JsonValue::parse(got).unwrap(), JsonValue::parse(text).unwrap());
            let mut keys: Vec<String> = Vec::new();
            for (k, tv) in t.as_object().unwrap().iter() {
                if g.get(k) != Some(tv) {
                    keys.push(format!("{k}: rust {} swift {}", g.get(k).map_or("-".into(), |v| v.canonical_js()), tv.canonical_js()));
                }
            }
            for (k, _) in g.as_object().unwrap().iter() {
                if t.get(k).is_none() {
                    keys.push(format!("{k}: rust only"));
                }
            }
            if keys.is_empty() {
                keys.push("same keys and values, a different order".into());
            }
            let short = |s: &String| if s.len() > 300 { format!("{}…", &s[..300]) } else { s.clone() };
            bad.push(format!("{name} ({deck}): {}", keys.iter().map(short).collect::<Vec<_>>().join("; ")));
        }
    }
    assert!(bad.is_empty(), "{} captures differ:\n{}", bad.len(), bad.join("\n"));
}
