//! Offline render against the live engine (docs/HEADLESS.md): an engine driven the way djapp
//! drives one (its trace on, the moves made through the engine calls the window, the socket and
//! the controllers make, an input strip fed block by block like a capture stream) records an
//! arrangement exactly as djapp's action log does (`Engine::arrange_start` at the start, each
//! traced op stamped with its beat). That arrangement, through its JSON and rendered offline by
//! `render_arrangement`, must give the live engine's output sample for sample: both decks,
//! the vocoder (input modulator, synth carrier, then a deck as carrier), the granular freeze,
//! tricks, the FX unit and the mixer.
//!
//! The live engine composes inline (`deterministic`) so the test does not race a background
//! compose; otherwise it is the app's engine, rendering in the app's blocks.

use std::sync::Arc;

use djrust::deck::turntable::SampleBuffer;
use djrust::fx::tricks::TrickKind;
use djrust::fx::vocoder::VocoderParam;
use djrust::json::JsonValue;
use djrust::session::arrangement::{ArrangeEvent, Arrangement};
use djrust::session::DeckAction;
use djrust::TrackSpec;
use djrust::host::deck::offline::{render_arrangement, ArrangeRenderOptions, OfflineConductor};
use djrust::host::deck::perform::PerformCtx;
use djrust::host::deck::tricks::TrickQuant;
use djrust::host::deck::vocoder::{CarrierPick, ModulatorPick, VocoderSwitch};
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 44_100.0;
const BLOCK: usize = OfflineConductor::BLOCK_FRAMES;

/// A "voice" for the mic: a buzz that comes and goes, 3 s.
fn voice() -> Arc<SampleBuffer> {
    let n = (3.0 * SR) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / SR;
            let env = (0.5 + 0.5 * (2.0 * std::f64::consts::PI * 2.0 * t).sin()) as f32;
            env * 0.4 * ((2.0 * std::f64::consts::PI * 180.0 * t).sin().signum() as f32)
        })
        .collect();
    Arc::new(SampleBuffer::new(l.clone(), Some(l), SR))
}

/// The live side: the app's engine with a "mic" input strip, trace on.
struct Live {
    c: OfflineConductor,
    feed: rtrb::Producer<f32>,
    mono: Vec<f32>,
    at: usize,
    bpm: f64,
    events: Vec<ArrangeEvent>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    /// An input recorder is running: give its thread a moment after each run (the offline
    /// engine renders far faster than the clock, a live one never outruns its disk).
    recording: bool,
}

impl Live {
    fn new() -> Live {
        let mut config = EngineConfig::new(SR);
        config.inputs = vec!["mic".into()];
        // the strip keeps a history, as the offline engine's does (a capture takes from it)
        config.history_max_beats = djrust::history::MAX_BEATS;
        let (mut e, r) = engine(config);
        e.deterministic = true;
        let mut c = OfflineConductor::new(e, r);
        let (feed, _) = c.rig.engine.take_input_feed(0).unwrap();
        let v = voice();
        let mono = v
            .left
            .iter()
            .zip(v.right())
            .map(|(a, b)| 0.5 * (a + b))
            .collect();
        Live {
            c,
            feed,
            mono,
            at: 0,
            bpm: 120.0,
            events: Vec::new(),
            out_l: Vec::new(),
            out_r: Vec::new(),
            recording: false,
        }
    }

    fn push(&mut self, n: usize) {
        for _ in 0..n {
            let _ = self
                .feed
                .push(self.mono.get(self.at).copied().unwrap_or(0.0));
            self.at += 1;
        }
    }

    fn beat(&self) -> f64 {
        self.c.engine().frame() as f64 / SR * self.bpm / 60.0
    }

    /// What djapp's `service_session_log` does with the trace: each op stamped with the beat.
    fn take_trace(&mut self) {
        let beat = self.beat();
        let traced = self
            .c
            .engine_mut()
            .trace
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for x in traced {
            for op in x.ops {
                self.events.push(ArrangeEvent {
                    at: beat,
                    bpm: self.bpm,
                    op,
                    via: x.entry.source.clone(),
                    text: x.entry.text.clone(),
                });
            }
        }
    }

    /// Render `blocks` blocks, feeding the mic a block ahead as a capture stream does.
    fn run(&mut self, blocks: usize) {
        for _ in 0..blocks {
            self.push(BLOCK);
            let (l, r) = self.c.step_block();
            self.out_l.extend_from_slice(l);
            self.out_r.extend_from_slice(r);
        }
        if self.recording {
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }

    /// One move, between blocks, then into the arrangement.
    fn act(&mut self, f: impl FnOnce(&mut djrust::host::deck::Engine)) {
        f(self.c.engine_mut());
        self.take_trace();
    }
}

fn record() -> (Live, Arrangement) {
    let mut live = Live::new();
    {
        let e = live.c.engine_mut();
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        assert!(e.generate("B", &TrackSpec::new("house", 7.0, None)));
    }
    let e = live.c.engine_mut();
    e.set_volume("A", 0.9);
    e.set_volume("B", 0.9);
    e.set_crossfader(0.3);
    // set before the recording starts: the arrangement's start carries it
    e.set_input_trim(0, 3.0);
    e.play("A");
    live.bpm = e.master_tempo().1;
    e.trace = Some(Vec::new());
    live.c.pump();
    // beat 0 is frame 0, as the offline render has it
    assert_eq!(live.c.engine().frame(), 0);
    let (decks, start) = live.c.engine().arrange_start();
    // prime the mic as a capture stream does
    live.push(((SR * 0.02) as usize).max(1024));
    live.run(40);
    live.act(|e| {
        e.vocoder_carrier(CarrierPick::Synth).unwrap();
        e.vocoder_modulator(ModulatorPick::Input(0)).unwrap();
        e.vocoder_set_notes(&[45, 48, 52]);
        e.vocoder_set(VocoderParam::Mix, 0.8);
        // a synth preset: the arrangement keeps its knobs
        let preset = e.synth_preset_names().last().map(|(n, _)| n.clone()).unwrap();
        e.synth_preset_load(&preset).unwrap();
        e.vocoder_switch(VocoderSwitch::On, true);
    });
    live.run(60);
    live.act(|e| {
        e.trick("A", TrickKind::Stutter, true, None, TrickQuant::Default)
            .unwrap();
        // the mic's strip, as its window knobs or the socket's `input` move it
        e.set_input_volume(0, 0.6);
        e.set_input_strip(0, djrust::fx::StripKnob::EqLow, 0.3);
    });
    live.run(40);
    live.act(|e| {
        e.trick("A", TrickKind::Stutter, false, None, TrickQuant::Default)
            .unwrap();
        e.perform(
            &DeckAction::Play {
                deck: "B".into(),
                on: true,
            },
            PerformCtx::default(),
        );
    });
    live.run(40);
    live.act(|e| {
        assert!(e.input_freeze(0, true, Some(2.0), Some(false)));
        e.vocoder_note(55, true);
        // the window's crossfader and FX knobs, as djapp makes them (through perform)
        e.perform(&DeckAction::Crossfader(0.6), PerformCtx::default());
        e.perform(
            &DeckAction::Fx {
                deck: "A".into(),
                param: "echo".into(),
                value: 0.5,
            },
            PerformCtx::default(),
        );
    });
    live.run(60);
    live.act(|e| {
        // the deck as the carrier, the mic still the modulator
        e.vocoder_carrier(CarrierPick::Deck("B".into())).unwrap();
        e.vocoder_switch(VocoderSwitch::Insert, true);
        assert!(e.input_freeze(0, false, None, Some(false)));
        e.trick("B", TrickKind::Delay, true, None, TrickQuant::Now)
            .unwrap();
    });
    live.run(80);
    let mut arr = Arrangement::new(live.bpm, decks, live.events.clone());
    arr.start = start;
    (live, arr)
}

#[test]
fn an_arrangement_renders_offline_to_what_the_live_engine_played() {
    let (live, arr) = record();
    // the moves the vocoder, the tricks and the freeze made are in the arrangement
    let ops: Vec<String> = arr.events.iter().map(|e| e.op.op()).collect();
    for want in ["vocoder", "trick", "freeze", "play", "input", "fx", "crossfader"] {
        assert!(ops.iter().any(|o| o == want), "{want} in {ops:?}");
    }
    // through its JSON, as a file would carry it
    let text = arr.json().js_stringified();
    let (back, bad) = Arrangement::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(bad, 0);
    assert_eq!(back, arr);

    let frames = live.out_l.len();
    let beats = frames as f64 / SR * live.bpm / 60.0;
    let r = render_arrangement(
        &back,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(beats),
            inputs: vec![("mic".into(), voice())],
            ..Default::default()
        },
    );
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(r.dropped, 0);
    assert_eq!(r.audio.left.len(), frames);
    let mut worst = 0f32;
    let mut first = None;
    for i in 0..frames {
        let d = (r.audio.left[i] - live.out_l[i])
            .abs()
            .max((r.audio.right[i] - live.out_r[i]).abs());
        if d > 0.0 && first.is_none() {
            first = Some(i);
        }
        worst = worst.max(d);
    }
    let peak = live.out_l.iter().fold(0f32, |p, v| p.max(v.abs()));
    assert!(peak > 0.05, "the live run made sound ({peak})");
    assert_eq!(
        worst,
        0.0,
        "offline differs from live by up to {worst}, first at frame {first:?} (block {:?})",
        first.map(|f| f / BLOCK)
    );
}

#[test]
fn stems_add_up_to_the_master() {
    let (_, arr) = record();
    let r = render_arrangement(
        &arr,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(16.0),
            inputs: vec![("mic".into(), voice())],
            stems: true,
            ..Default::default()
        },
    );
    let names: Vec<&str> = r.stems.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["A", "B", "loops", "mic", "vocoder", "aux"]);
    let n = r.audio.left.len();
    let mut worst = 0f32;
    for i in 0..n {
        let sum: f32 = r.stems.iter().map(|(_, a)| a.left[i]).sum();
        worst = worst.max((sum - r.audio.left[i]).abs());
    }
    assert!(worst < 1e-4, "stems are off the master by {worst}");
    // the vocoder's stem is not silent once it is on, and deck A's carries the deck
    let loud = |name: &str| {
        r.stems
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0f32, |(_, a)| {
                a.left.iter().fold(0f32, |p, v| p.max(v.abs()))
            })
    };
    assert!(loud("A") > 0.01);
    assert!(loud("vocoder") > 0.001, "vocoder stem {}", loud("vocoder"));
}

#[test]
fn the_same_arrangement_renders_the_same_twice() {
    let (_, arr) = record();
    let opts = || ArrangeRenderOptions {
        sample_rate: SR,
        beats: Some(12.0),
        inputs: vec![("mic".into(), voice())],
        ..Default::default()
    };
    let a = render_arrangement(&arr, opts());
    let b = render_arrangement(&arr, opts());
    assert_eq!(a.audio.left, b.audio.left);
    assert_eq!(a.audio.right, b.audio.right);
}

// MARK: a set played with the mouse

/// A set played only with what the window does: the headphone cue, the platter, the tempo
/// chips, START / STOP, the transport's tempo, the mic strip's history and capture, the loop
/// row's pads, the nudge the socket makes. None of it is a `DeckAction` (but the loop pads and
/// the mixer, which go through `perform`), so the arrangement can only carry it as live ops.
fn record_mouse() -> (Live, Arrangement) {
    let (live, arr, _) = record_mouse_recording(None);
    (live, arr)
}

/// [`record_mouse`], with the inputs recorded to files in `dir` (the recorder's finish).
fn record_mouse_recording(
    dir: Option<&std::path::Path>,
) -> (Live, Arrangement, Vec<(String, u64)>) {
    use djrust::host::deck::strip_recall::RecallId;
    let mut live = Live::new();
    {
        let e = live.c.engine_mut();
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        assert!(e.generate("B", &TrackSpec::new("house", 7.0, None)));
    }
    let e = live.c.engine_mut();
    e.set_volume("A", 0.9);
    e.set_volume("B", 0.9);
    e.play("A");
    live.bpm = e.master_tempo().1;
    e.trace = Some(Vec::new());
    e.trace_via = "window".into();
    live.c.pump();
    live.recording = dir.is_some();
    let mut recorder = dir.map(|d| {
        djrust::host::input_record::InputRecorder::start(live.c.engine_mut(), d, "t").unwrap()
    });
    let (decks, start) = live.c.engine().arrange_start();
    live.push(((SR * 0.02) as usize).max(1024));
    // the mic has been talking for a while: a bar and a half of history to take from
    live.run(260);
    live.act(|e| {
        e.set_pfl("A", true);
        e.set_headphone_mix(0.7);
        e.set_headphone_level(0.6);
        e.set_input_pfl(0, true);
        e.set_pitch_range("B", 0.16);
        e.transport_start();
        e.set_tempo_lock(Some("A"));
    });
    live.run(30);
    live.act(|e| {
        // the platter by hand
        e.jog_touch("A", true);
        e.jog_scrub("A", 0.03);
    });
    live.run(10);
    live.act(|e| {
        e.jog_scrub("A", -0.02);
        e.jog_touch("A", false);
        e.nudge_by_hand("B", 0.02);
    });
    live.run(30);
    // the mic's history: a loop of its last two beats, halved, then back to live
    live.act(|e| {
        e.input_loop(RecallId::Input(0), 2.0);
    });
    live.run(30);
    live.act(|e| {
        e.input_loop_step(RecallId::Input(0), -1);
    });
    live.run(30);
    live.act(|e| {
        e.input_live(RecallId::Input(0));
        // capture to loop: the last two beats of the mic onto the row, looping at once
        assert!(e.capture_grab(0, Some(2.0)).is_some(), "the mic has two beats of history");
    });
    live.run(60);
    let clip = live.c.engine().loops.row.clips.first().map(|c| c.id.clone()).expect("a clip");
    live.act(|e| {
        // the pad, as the window clicks it: through perform
        e.perform(&DeckAction::ClipLoop { id: clip.clone(), on: false }, PerformCtx::default());
        e.set_free_bpm(126.0);
        e.set_tempo_lock(None);
    });
    live.run(30);
    live.act(|e| {
        e.perform(&DeckAction::ClipLoop { id: clip.clone(), on: true }, PerformCtx::default());
        e.transport_stop();
    });
    live.run(30);
    live.act(|e| {
        e.transport_start();
        e.stop_all_loops();
        e.set_pfl("A", false);
        e.set_input_pfl(0, false);
    });
    live.run(60);
    let written = recorder
        .as_mut()
        .map(|r| r.finish(live.c.engine_mut()))
        .unwrap_or_default();
    let mut arr = Arrangement::new(live.bpm, decks, live.events.clone());
    arr.start = start;
    (live, arr, written)
}

#[test]
fn a_set_played_with_the_mouse_renders_offline_to_what_the_live_engine_played() {
    let (live, arr) = record_mouse();
    let ops: Vec<String> = arr.events.iter().map(|e| e.op.op()).collect();
    for want in [
        "cue", "jog", "nudge", "pitch_range", "transport", "history", "capture", "clipLoop",
        "loops",
    ] {
        assert!(ops.iter().any(|o| o == want), "{want} in {ops:?}");
    }
    let text = arr.json().js_stringified();
    let (back, bad) = Arrangement::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(bad, 0, "every move parses back");
    assert_eq!(back, arr);
    let frames = live.out_l.len();
    let beats = frames as f64 / SR * live.bpm / 60.0;
    let r = render_arrangement(
        &back,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(beats),
            inputs: vec![("mic".into(), voice())],
            ..Default::default()
        },
    );
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(r.dropped, 0);
    assert_eq!(r.audio.left.len(), frames);
    let mut worst = 0f32;
    let mut first = None;
    for i in 0..frames {
        let d = (r.audio.left[i] - live.out_l[i])
            .abs()
            .max((r.audio.right[i] - live.out_r[i]).abs());
        if d > 0.0 && first.is_none() {
            first = Some(i);
        }
        worst = worst.max(d);
    }
    let peak = live.out_l.iter().fold(0f32, |p, v| p.max(v.abs()));
    assert!(peak > 0.05, "the live run made sound ({peak})");
    assert_eq!(
        worst,
        0.0,
        "offline differs from live by up to {worst}, first at frame {first:?} (block {:?})",
        first.map(|f| f / BLOCK)
    );
}

/// The mic recorded to a file while the set is played: the arrangement names the file, and an
/// offline render with no audio given feeds the strip from it. The history scrub and the capture
/// to loop of the set take the same audio they took live, sample for sample.
#[test]
fn recorded_inputs_let_the_offline_render_reproduce_the_history_and_the_capture() {
    let dir = djrust::host::stores::temp_path("record-inputs");
    let (live, arr, written) = record_mouse_recording(Some(&dir));
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].0, "mic");
    assert!(written[0].1 as usize >= live.out_l.len() - 2 * BLOCK, "the whole run: {written:?}");
    let file = arr.start.inputs[0].file.clone().expect("the arrangement names the file");
    assert!(file.ends_with("t-mic.wav"), "{file}");
    // the file holds what the strip was fed
    let (heard, rate) = djrust::host::input_record::read_recording(std::path::Path::new(&file), 0).unwrap();
    assert_eq!(rate as f64, SR);
    assert_eq!(heard.len() as u64, written[0].1);
    assert!(heard.iter().fold(0f32, |p, v| p.max(v.abs())) > 0.1);
    // through JSON, then rendered with no input audio given
    let (back, bad) =
        Arrangement::from_json(&JsonValue::parse(&arr.json().js_stringified()).unwrap()).unwrap();
    assert_eq!(bad, 0);
    assert_eq!(back.start.inputs[0].file.as_deref(), Some(file.as_str()));
    let frames = live.out_l.len();
    let r = render_arrangement(
        &back,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(frames as f64 / SR * live.bpm / 60.0),
            ..Default::default()
        },
    );
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    let worst = (0..frames)
        .map(|i| (r.audio.left[i] - live.out_l[i]).abs().max((r.audio.right[i] - live.out_r[i]).abs()))
        .fold(0f32, f32::max);
    assert_eq!(worst, 0.0, "offline from the recorded file differs from live by {worst}");
    // a file at another rate is refused, not played wrong
    let mut other = back.clone();
    other.start.inputs[0].file = Some(file);
    let r = render_arrangement(
        &other,
        ArrangeRenderOptions { sample_rate: 48_000.0, beats: Some(4.0), ..Default::default() },
    );
    assert!(r.refused.iter().any(|(_, m)| m.contains("44100 Hz")), "{:?}", r.refused);
    let _ = std::fs::remove_dir_all(&dir);
}
