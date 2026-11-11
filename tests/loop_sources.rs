//! Loop grab / record from any source, offline (docs/LOOP-SOURCES.md): the vocoder strip V
//! (after its fader by default, before it with `pre`), the master output M and the input strips
//! by label, besides the decks. A take lands on the loop row, in the library, and in the action
//! log.
//!
//! The rig: a mic (a tone) vocodes the TD-3 (a saw) onto V; both input strips sit at zero, so V
//! is all the master plays until a take starts looping on the row.

use djrust::control::controller::{Action, SurfaceHost};
use djrust::fx::vocoder::VocoderParam;
use djrust::host::deck::loop_source::{Bus, LoopSource};
use djrust::host::deck::vocoder::{CarrierPick, ModulatorPick, VocoderSwitch};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use djrust::host::stores::captures::CaptureLibrary;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const MIC: usize = 0;
const TD3: usize = 1;
/// A beat at 120 BPM.
const BEAT: usize = 24_000;
const BAR: usize = 4 * BEAT;

struct Rig {
    e: Engine,
    r: Renderer,
    mic: rtrb::Producer<f32>,
    td3: rtrb::Producer<f32>,
    t: usize,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.inputs = vec!["MIC".into(), "TD-3".into()];
        let (mut e, r) = engine(c);
        let (mic, _) = e.take_input_feed(MIC).unwrap();
        let (td3, _) = e.take_input_feed(TD3).unwrap();
        for i in [MIC, TD3] {
            e.set_input_trim(i, 0.0);
            // the vocoder taps its sources before their faders: these play nothing themselves
            e.set_input_volume(i, 0.0);
        }
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
        e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
        e.vocoder_switch(VocoderSwitch::On, true);
        e.vocoder_set(VocoderParam::Volume, 1.0);
        Rig {
            e,
            r,
            mic,
            td3,
            t: 0,
            out_l: Vec::new(),
            out_r: Vec::new(),
        }
    }

    fn run(&mut self, frames: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..frames / BLOCK {
            for k in 0..BLOCK {
                let t = (self.t + k) as f64 / SR;
                let voice = 0.4 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()
                    + 0.1 * (2.0 * std::f64::consts::PI * 3000.0 * t).sin();
                let saw = 0.5 * (2.0 * ((110.0 * t) % 1.0) - 1.0);
                let _ = self.mic.push(voice as f32);
                let _ = self.td3.push(saw as f32);
            }
            self.t += BLOCK;
            self.r.render_block(&mut l, &mut rr);
            self.e.sync_history_tempo();
            self.e.service_loops();
            self.out_l.extend_from_slice(&l);
            self.out_r.extend_from_slice(&rr);
        }
    }

    fn clip_audio(&self, id: &str) -> (Vec<f32>, Option<Vec<f32>>) {
        let c = self.e.clip(id).expect("on the row");
        (c.buffer.left.clone(), c.buffer.right.clone())
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("djrust-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn names_resolve_to_sources() {
    let g = Rig::new();
    let e = &g.e;
    assert_eq!(e.loop_source("A", false), Some(LoopSource::Deck("A".into())));
    assert_eq!(e.loop_source("b", false), Some(LoopSource::Deck("B".into())));
    assert_eq!(e.loop_source("V", false), Some(LoopSource::Bus(Bus::Vocoder)));
    assert_eq!(e.loop_source("vocoder", true), Some(LoopSource::Bus(Bus::VocoderPre)));
    assert_eq!(e.loop_source("M", false), Some(LoopSource::Bus(Bus::Master)));
    assert_eq!(e.loop_source("master", true), Some(LoopSource::Bus(Bus::Master)));
    assert_eq!(e.loop_source("TD-3", false), Some(LoopSource::Input(TD3)));
    assert_eq!(e.loop_source("td-3", false), Some(LoopSource::Input(TD3)));
    assert_eq!(e.loop_source("nope", false), None);
    assert_eq!(e.loop_source_names(), ["A", "B", "V", "M", "L", "MIC", "TD-3"]);
    assert_eq!(e.loop_source("L", false), Some(LoopSource::Bus(Bus::Loops)));
}

#[test]
fn a_grab_of_v_is_what_v_played_after_its_fader() {
    let mut g = Rig::new();
    let dir = temp_dir("loopsrc-v");
    g.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    g.run(2 * BAR);
    let end = g.e.loop_frames() as usize;
    let id = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).expect("a grab");
    let c = g.e.clip(&id).unwrap();
    assert_eq!((c.beats, c.source.as_str()), (4, "V"));
    let (l, r) = g.clip_audio(&id);
    let r = r.expect("stereo");
    assert_eq!(l.len(), BAR);
    assert!(rms(&l) > 0.01, "V is playing: {}", rms(&l));
    // V is all the master plays here, so the take is the master output, sample for sample
    assert_eq!(l, g.out_l[end - BAR..end]);
    assert_eq!(r, g.out_r[end - BAR..end]);
    // and it is saved, stereo, under V
    let lib = CaptureLibrary::new(&dir);
    let m = lib.meta(&id).expect("in the library");
    assert_eq!((m.source.as_str(), m.channels, m.beats), ("V", 2, 4));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v_pre_fader_ignores_the_fader_post_fader_follows_it() {
    let mut g = Rig::new();
    g.e.vocoder_set(VocoderParam::Volume, 0.5);
    g.run(2 * BAR);
    let post = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    let pre = g.e.source_grab(&LoopSource::Bus(Bus::VocoderPre), 1).unwrap();
    let (a, b) = (rms(&g.clip_audio(&post).0), rms(&g.clip_audio(&pre).0));
    assert!((b / a - 2.0).abs() < 0.01, "pre {b} post {a}");
    // the fader down: post-fader is silence, pre-fader still has it
    g.e.vocoder_set(VocoderParam::Volume, 0.0);
    g.run(2 * BAR);
    let pre2 = g.e.source_grab(&LoopSource::Bus(Bus::VocoderPre), 1).unwrap();
    assert!(rms(&g.clip_audio(&pre2).0) > 0.01);
    // (a silent take: every sample zero)
    let post2 = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    assert_eq!(rms(&g.clip_audio(&post2).0), 0.0);
}

#[test]
fn as_an_insert_v_is_the_inserts_output() {
    let mut g = Rig::new();
    g.e.vocoder_switch(VocoderSwitch::Insert, true);
    g.e.vocoder_set(VocoderParam::Volume, 0.0);
    g.run(2 * BAR);
    let id = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    assert!(rms(&g.clip_audio(&id).0) > 0.01);
}

#[test]
fn a_grab_of_m_is_the_master_output() {
    let mut g = Rig::new();
    g.e.set_master(0.5);
    g.run(2 * BAR);
    let end = g.e.loop_frames() as usize;
    let id = g.e.source_grab(&LoopSource::Bus(Bus::Master), 1).unwrap();
    let (l, r) = g.clip_audio(&id);
    assert_eq!(l, g.out_l[end - BAR..end]);
    assert_eq!(r.unwrap(), g.out_r[end - BAR..end]);
    assert_eq!(g.e.clip(&id).unwrap().source, "M");
}

#[test]
fn not_that_much_yet_is_no_grab() {
    let mut g = Rig::new();
    g.run(BEAT);
    assert_eq!(g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1), None);
    assert!(g.e.loops.row.clips.is_empty());
}

#[test]
fn recording_v_ends_on_the_bar_after_the_release() {
    let mut g = Rig::new();
    g.run(BAR);
    let v = LoopSource::Bus(Bus::Vocoder);
    assert!(g.e.source_record(&v, true, None));
    // one take per bus at a time (V pre and post are one strip)
    assert!(!g.e.source_record(&LoopSource::Bus(Bus::VocoderPre), true, None));
    g.run(6 * BEAT);
    assert_eq!(g.e.source_take_progress(&v), Some((2, false)));
    assert!(g.e.source_record(&v, false, None));
    assert_eq!(g.e.source_take_progress(&v), Some((2, true)));
    g.run(3 * BAR);
    assert_eq!(g.e.source_take_progress(&v), None);
    let c = g.e.loops.row.clips.last().expect("a take");
    assert_eq!((c.beats, c.source.as_str()), (8, "V"));
    assert_eq!(c.buffer.length(), 2 * BAR);
    assert!(c.buffer.right.is_some());
}

#[test]
fn a_tap_with_bars_records_exactly_that_many() {
    let mut g = Rig::new();
    g.run(BAR);
    let m = LoopSource::Bus(Bus::Master);
    assert!(g.e.source_record(&m, true, Some(2)));
    g.run(BLOCK * 4);
    assert!(g.e.source_record(&m, false, None));
    g.run(3 * BAR);
    let c = g.e.loops.row.clips.last().expect("a take");
    assert_eq!((c.beats, c.source.as_str()), (8, "M"));
}

#[test]
fn an_input_by_label_goes_through_its_capture() {
    let mut g = Rig::new();
    g.run(2 * BAR);
    let src = g.e.loop_source("TD-3", false).unwrap();
    let id = g.e.source_grab(&src, 1).expect("a grab");
    let c = g.e.clip(&id).unwrap();
    assert_eq!((c.beats, c.source.as_str()), (4, "TD-3"));
    assert!(c.buffer.right.is_none(), "an input is mono");
}

#[test]
fn bus_takes_go_in_the_action_log_once() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    g.run(2 * BAR);
    g.e.trace_via = "socket".into();
    g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    g.e.source_grab(&LoopSource::Bus(Bus::Master), 64);
    g.e.trace_via = "APC".into();
    // a capture control naming V grabs V (whole bars), one line for it
    SurfaceHost::perform(
        &mut g.e,
        Action::CaptureGrab {
            input: Some("V".into()),
            beats: Some(8.0),
        },
    );
    // a LOOP binding with `source: M`
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "M".into(), on: true });
    g.run(BAR);
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "M".into(), on: false });
    g.run(3 * BAR);
    // a take is a `source` live op (an arrangement takes it again); a refusal and the take's
    // landing are words only
    let traced = g.e.trace.take().unwrap();
    let ops: Vec<String> = traced
        .iter()
        .flat_map(|t| t.ops.iter())
        .map(|o| match o {
            djrust::session::arrangement::ArrangeOp::Live { method, params } => {
                format!("{method} {}", params.js_stringified())
            }
            o => panic!("{o:?}"),
        })
        .collect();
    assert_eq!(
        ops,
        [
            r#"source {"bus":"V","action":"grab","bars":1}"#,
            r#"source {"bus":"V","action":"grab","bars":2}"#,
            r#"source {"bus":"M","action":"start","bars":2}"#,
            r#"source {"bus":"M","action":"end"}"#,
        ]
    );
    let lines: Vec<(String, String)> = traced
        .into_iter()
        .map(|t| (t.entry.source, t.entry.text))
        .collect();
    let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts.len(), 6, "{texts:?}");
    assert!(texts[0].starts_with("grab 1 bar of V → V 4b"), "{texts:?}");
    assert_eq!(texts[1], "grab 16 bars of M: not that much yet");
    assert!(texts[2].starts_with("grab 2 bars of V → V 8b"), "{texts:?}");
    assert_eq!(texts[3], "loop record M down");
    assert_eq!(texts[4], "loop record M up");
    assert!(texts[5].starts_with("loop take of M → M 4b"), "{texts:?}");
    assert_eq!(lines[0].0, "socket");
    assert_eq!(lines[2].0, "APC");
    assert_eq!(g.e.loops.row.clips.iter().filter(|c| c.source == "M").count(), 1);
}

#[test]
fn an_input_named_on_a_loop_binding_is_logged_in_words() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    g.run(2 * BAR);
    g.e.trace_via = "MPK".into();
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "TD-3".into(), on: true });
    let t = g.e.trace.take().unwrap();
    assert_eq!(t.len(), 1);
    // the engine's capture call says what it did (a `capture` live op), not the binding
    assert_eq!(t[0].entry.text, "capture TD-3 started");
    assert_eq!(t[0].entry.source, "MPK");
    assert_eq!(t[0].ops.len(), 1);
    assert!(g.e.capture_progress(TD3).is_some());
}

#[test]
fn a_deck_still_records_as_before() {
    let mut g = Rig::new();
    // not playing: nothing to take, as ever
    assert!(!g.e.source_record(&LoopSource::Deck("A".into()), true, None));
    assert_eq!(g.e.source_grab(&LoopSource::Deck("A".into()), 4), None);
}

/// `loops stop` only stops and `loops play` only plays (the socket's play / stop): a stop on
/// an Off clip leaves it off, a second stop keeps the first stop's boundary, a play on a clip
/// that is in does nothing.
#[test]
fn stop_only_stops_and_play_only_plays() {
    use djrust::host::deck::loops::LoopState;
    let mut g = Rig::new();
    g.e.set_volume("A", 1.0);
    assert!(g.e.generate("A", &djrust::TrackSpec::new("techno", 42.0, None)));
    g.e.play("A");
    g.run(3 * BAR);
    let id = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    g.run(BAR / 2);
    assert_eq!(g.e.loop_state(&id), LoopState::On);
    // play on a clip that is in: nothing
    assert!(!g.e.loop_play(&id));
    assert_eq!(g.e.loop_state(&id), LoopState::On);
    // stop, then stop again: the first stop's boundary stands
    assert!(g.e.loop_stop(&id));
    assert_eq!(g.e.loop_state(&id), LoopState::Stopping);
    let at = g.e.loop_stops_in(&id).unwrap();
    g.run(BLOCK * 4);
    assert!(!g.e.loop_stop(&id));
    let later = g.e.loop_stops_in(&id).unwrap();
    assert!(later < at, "kept its stop: {at} then {later}");
    g.run(2 * BAR);
    assert_eq!(g.e.loop_state(&id), LoopState::Off);
    // stop on an Off clip, many times, over bars: it stays off
    for _ in 0..8 {
        assert!(!g.e.loop_stop(&id));
        g.run(BAR / 2);
        assert_eq!(g.e.loop_state(&id), LoopState::Off);
    }
    // play brings it in on the boundary; a stop before it is in keeps it out
    assert!(g.e.loop_play(&id));
    assert_eq!(g.e.loop_state(&id), LoopState::Starting);
    assert!(g.e.loop_stop(&id));
    g.run(2 * BAR);
    assert_eq!(g.e.loop_state(&id), LoopState::Off);
    // and play only plays
    assert!(g.e.loop_play(&id));
    g.run(2 * BAR);
    assert_eq!(g.e.loop_state(&id), LoopState::On);
}

/// A V take goes straight onto a deck (clipToDeck): it leaves the row's channel and plays on
/// the deck (keeping that deck's transport), stereo, from the take's own audio.
#[test]
fn a_v_take_goes_onto_a_deck() {
    let mut g = Rig::new();
    g.run(2 * BAR);
    let id = g.e.source_grab(&LoopSource::Bus(Bus::Vocoder), 1).unwrap();
    let take = g.clip_audio(&id);
    for deck in ["A", "B"] {
        let moved = g.e.clip_to_deck(Some(&id), None, deck).expect("onto the deck");
        assert_eq!(moved, id);
        g.run(BAR);
        let d = g.e.deck(deck).unwrap();
        assert_eq!(d.clip_id.as_deref(), Some(id.as_str()), "{deck} holds the take");
        let s = d.sample.as_ref().expect("a sample deck");
        assert_eq!(s.buffer.left, take.0);
        assert_eq!(s.buffer.right, take.1);
    }
    // the SurfaceHost path (a clipToDeck binding: slot, deck) lands the same take
    let slot = g.e.loops.row.clips.iter().position(|c| c.id == id).unwrap();
    SurfaceHost::perform(
        &mut g.e,
        Action::Target {
            id: "clipToDeck".into(),
            deck: Some("A".into()),
            slot: Some(slot as i64),
            index: 0,
            value: 1.0,
            pressed: true,
            args: Default::default(),
        },
    );
    assert_eq!(g.e.deck("A").unwrap().clip_id.as_deref(), Some(id.as_str()));
}
