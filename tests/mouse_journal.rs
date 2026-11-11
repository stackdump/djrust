//! The journal of a set played with the mouse (docs/ACTION-LOG.md, "The journal"): what the
//! window and the socket do that is neither a `DeckAction` nor a controller's MIDI is a
//! `live` line (`JournalEvent::Live`), in order and on its frame; what a controller does stays
//! the MIDI that made it (a live op it traces is not journalled again); and a replay applies
//! them to its own engine, which has the input strips the lines name.

use std::path::PathBuf;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::journal::event::JournalEvent;
use djrust::json::JsonValue;
use djrust::host::deck::OfflineConductor;
use djrust::host::journal::read;
use djrust::host::replay::{offline_conductor_with_inputs, replay, JournalReplayOptions};
use djrust::host::stores::temp_path;

const RATE: f64 = 11_025.0;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn conductor() -> OfflineConductor {
    offline_conductor_with_inputs(vec!["A".into(), "B".into()], RATE, vec!["mic".into()])
}

fn lives(path: &std::path::Path) -> Vec<(String, String)> {
    read(path)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|e| match e.event {
            JournalEvent::Live { method, params } => Some((method, params.js_stringified())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_windows_moves_are_journaled_as_live_lines_and_a_controllers_are_not() {
    let dir = Dir(temp_path("mouse-journal"));
    let mut c = conductor();
    let path = c.rig.open_journal(&dir.0, "test").unwrap();
    c.rig.engine.trace_via = "window".into();
    c.step_with(|_| {});
    c.step_with(|c| {
        let e = &mut c.rig.engine;
        e.set_pfl("A", true);
        e.set_headphone_mix(0.25);
        e.set_input_volume(0, 0.5);
        e.set_input_pfl(0, true);
        e.set_pitch_range("B", 0.16);
        e.set_free_bpm(128.0);
        e.transport_start();
    });
    c.step_with(|c| {
        let e = &mut c.rig.engine;
        e.jog_touch("A", true);
        e.jog_scrub("A", 0.02);
        e.nudge_by_hand("B", 0.03);
        e.stop_all_loops();
    });
    // a controller's: the journal has the MIDI; the engine journals no line for what it made
    c.rig.engine.trace_via = "APC".into();
    SurfaceHost::perform(&mut c.rig.engine, Action::HeadphoneLevel(0.3));
    SurfaceHost::perform(&mut c.rig.engine, Action::PflToggle { deck: "B".into() });
    c.step_with(|_| {});
    c.rig.close_journal();

    let got = lives(&path);
    let want: Vec<(&str, &str)> = vec![
        ("cue", r#"{"deck":"A","pfl":true}"#),
        ("cue", r#"{"mix":0.25}"#),
        ("input", r#"{"input":"mic","volume":0.5}"#),
        ("input", r#"{"input":"mic","pfl":true}"#),
        ("pitch_range", r#"{"deck":"B","range":0.16}"#),
        ("transport", r#"{"action":"free_bpm","bpm":128}"#),
        ("transport", r#"{"action":"start"}"#),
        ("jog", r#"{"deck":"A","touch":true}"#),
        ("jog", r#"{"deck":"A","scrub":0.02}"#),
        ("nudge", r#"{"deck":"B","ratio":0.03}"#),
        ("loops", r#"{"action":"stop_all"}"#),
    ];
    let got: Vec<(&str, &str)> = got.iter().map(|(m, p)| (m.as_str(), p.as_str())).collect();
    assert_eq!(got, want);
    // each is stamped with the frame it was made on, in order
    let file = read(&path).unwrap();
    assert!(file.skipped.is_empty(), "{:?}", file.skipped);
    let frames: Vec<i64> = file.entries.iter().map(|e| e.frame).collect();
    assert!(frames.windows(2).all(|w| w[0] <= w[1]));

    // the engine that replays it makes them again: the strip the lines name exists, nothing is
    // refused
    let r = replay(&file, &JournalReplayOptions { render: true, ..Default::default() }).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);

    // and applied to a fresh engine, each line leaves it as the window left the live one
    let mut fresh = conductor();
    for e in &file.entries {
        if let JournalEvent::Live { method, params } = &e.event {
            fresh.rig.engine.apply_live_op(method, params).unwrap();
        }
    }
    let (a, b) = (c.engine(), fresh.engine());
    assert_eq!(a.deck("A").unwrap().pfl, b.deck("A").unwrap().pfl);
    assert_eq!(a.headphone_mix, b.headphone_mix);
    assert_eq!(a.inputs[0].settings.volume, b.inputs[0].settings.volume);
    assert_eq!(a.inputs[0].settings.pfl, b.inputs[0].settings.pfl);
    assert_eq!(a.deck("B").unwrap().pitch_range, b.deck("B").unwrap().pitch_range);
    assert_eq!(a.transport.free_bpm, b.transport.free_bpm);
    assert_eq!(a.transport.running, b.transport.running);
    // the controller's moves are not among them
    assert_eq!(a.headphone_level, 0.3);
    assert_ne!(b.headphone_level, 0.3);
    assert!(!b.deck("B").unwrap().pfl);
}

#[test]
fn a_line_the_engine_cannot_apply_is_a_warning_not_a_stop() {
    let dir = Dir(temp_path("mouse-journal-bad"));
    let mut c = conductor();
    let path = c.rig.open_journal(&dir.0, "test").unwrap();
    let j = c.rig.journal.clone().unwrap();
    c.step_with(|_| {});
    let frame = c.engine().frame();
    j.record(
        frame,
        JournalEvent::Live {
            method: "cue".into(),
            params: JsonValue::parse(r#"{"deck":"Z","pfl":true}"#).unwrap(),
        },
    );
    c.step_with(|_| {});
    c.rig.close_journal();
    let file = read(&path).unwrap();
    let r = replay(&file, &JournalReplayOptions::default()).unwrap();
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert!(r.warnings[0].starts_with("live cue:"), "{:?}", r.warnings);
}
