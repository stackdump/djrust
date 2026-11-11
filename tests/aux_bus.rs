//! The aux send/return bus and the eleven macros that drive it (docs/AUX.md, HUB.md M4):
//! sends on A, B, L and the inputs, an FX return on the mixer, bindable and replayable.
//!
//! - an untouched aux changes nothing, bit for bit (every other golden depends on it);
//! - a send puts a strip into the room and the echo, post-fader; the return fader and mute scale it;
//! - the delay-throw / dub-delay / cathedral macros raise the aimed strip's send and move the
//!   bus's sliders for their bars, then put them back; the pan and shape macros hold the aimed
//!   deck's parts; all of them start on the aimed deck's next bar;
//! - **live == offline**: an engine driven as djapp drives one, its trace recorded as an
//!   arrangement, through JSON and rendered offline, is the live output sample for sample, aux
//!   moves and macros included; stems (with `aux`) add up to the master.

use std::sync::Arc;

use djrust::deck::turntable::SampleBuffer;
use djrust::fx::aux_bus::AuxFxKey;
use djrust::fx::AuxMacro;
use djrust::json::JsonValue;
use djrust::session::arrangement::{ArrangeEvent, ArrangeOp, Arrangement};
use djrust::session::DeckAction;
use djrust::TrackSpec;
use djrust::host::deck::offline::{render_arrangement, ArrangeRenderOptions, OfflineConductor};
use djrust::host::deck::perform::PerformCtx;
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 44_100.0;
const BLOCK: usize = OfflineConductor::BLOCK_FRAMES;
/// Blocks in a second.
const SECOND: usize = SR as usize / BLOCK;
/// Blocks in a bar at 120 BPM (two seconds).
const BAR: usize = 2 * SECOND;

fn voice() -> Arc<SampleBuffer> {
    let n = (4.0 * SR) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / SR;
            let env = (0.5 + 0.5 * (2.0 * std::f64::consts::PI * 2.0 * t).sin()) as f32;
            env * 0.4 * ((2.0 * std::f64::consts::PI * 180.0 * t).sin().signum() as f32)
        })
        .collect();
    Arc::new(SampleBuffer::new(l.clone(), Some(l), SR))
}

struct Live {
    c: OfflineConductor,
    feed: Option<rtrb::Producer<f32>>,
    mono: Vec<f32>,
    at: usize,
    bpm: f64,
    events: Vec<ArrangeEvent>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
}

impl Live {
    fn new(inputs: bool) -> Live {
        let mut config = EngineConfig::new(SR);
        if inputs {
            config.inputs = vec!["mic".into()];
        }
        let (mut e, r) = engine(config);
        e.deterministic = true;
        let mut c = OfflineConductor::new(e, r);
        let feed = if inputs { c.rig.engine.take_input_feed(0).map(|f| f.0) } else { None };
        let v = voice();
        let mono = v.left.iter().zip(v.right()).map(|(a, b)| 0.5 * (a + b)).collect();
        let mut l = Live {
            c,
            feed,
            mono,
            at: 0,
            bpm: 120.0,
            events: Vec::new(),
            out_l: Vec::new(),
            out_r: Vec::new(),
        };
        {
            let e = l.c.engine_mut();
            assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
            assert!(e.generate("B", &TrackSpec::new("house", 7.0, None)));
            e.set_volume("A", 0.9);
            e.set_volume("B", 0.9);
            e.set_crossfader(0.5);
            e.play("A");
            e.play("B");
        }
        l.c.pump();
        l
    }

    fn push(&mut self, n: usize) {
        if let Some(f) = self.feed.as_mut() {
            for _ in 0..n {
                let _ = f.push(self.mono.get(self.at).copied().unwrap_or(0.0));
                self.at += 1;
            }
        }
    }

    fn beat(&self) -> f64 {
        self.c.engine().frame() as f64 / SR * self.bpm / 60.0
    }

    fn take_trace(&mut self) {
        let beat = self.beat();
        let traced = self.c.engine_mut().trace.as_mut().map(std::mem::take).unwrap_or_default();
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

    fn run(&mut self, blocks: usize) {
        for _ in 0..blocks {
            self.push(BLOCK);
            let (l, r) = self.c.step_block();
            self.out_l.extend_from_slice(l);
            self.out_r.extend_from_slice(r);
        }
    }

    fn act(&mut self, f: impl FnOnce(&mut djrust::host::deck::Engine)) {
        f(self.c.engine_mut());
        self.take_trace();
    }
}

fn macro_action(deck: &str, id: &str) -> DeckAction {
    DeckAction::Macro { deck: deck.into(), id: id.into() }
}

#[test]
fn an_untouched_aux_changes_nothing_bit_for_bit() {
    let go = |touch: bool| {
        let mut l = Live::new(false);
        if touch {
            // everything but a send: the return, its mute, the sliders
            let e = l.c.engine_mut();
            e.set_aux_return(1.0);
            e.set_aux_fx(AuxFxKey::DelayWet, 100.0);
            e.set_aux_fx(AuxFxKey::ReverbWet, 100.0);
            e.set_aux_mute(false);
        }
        l.run(SECOND);
        (l.out_l, l.out_r)
    };
    let (a, b) = (go(false), go(true));
    assert_eq!(a, b);
    assert!(a.0.iter().fold(0f32, |p, v| p.max(v.abs())) > 0.05, "the decks made sound");
}

#[test]
fn a_send_puts_the_strip_into_the_effects_and_the_return_fader_and_mute_scale_it() {
    let render = |send: f64, ret: f64, mute: bool| {
        let mut l = Live::new(false);
        let e = l.c.engine_mut();
        e.set_aux_fx(AuxFxKey::DelayWet, 100.0);
        e.set_aux_fx(AuxFxKey::ReverbWet, 80.0);
        e.set_aux_send("A", send);
        e.set_aux_return(ret);
        e.set_aux_mute(mute);
        l.run(SECOND);
        let peak = l.c.engine().aux_peak();
        (l.out_l, l.out_r, peak)
    };
    let dry = render(0.0, 0.8, false);
    let wet = render(0.7, 0.8, false);
    let wet_half = render(0.7, 0.4, false);
    let muted = render(0.7, 0.8, true);
    assert_ne!(dry.0, wet.0, "a send changes the mix");
    assert!(wet.2 > 0.01, "the bus returned something: {}", wet.2);
    assert_eq!(dry.2, 0.0);
    // the return is the difference from the dry mix; half the fader is half the return
    let diff = |a: &[f32], b: &[f32]| -> f64 { a.iter().zip(b).map(|(x, y)| ((x - y) as f64).powi(2)).sum::<f64>().sqrt() };
    let full = diff(&wet.0, &dry.0);
    let half = diff(&wet_half.0, &dry.0);
    assert!(((half / full) - 0.5).abs() < 0.08, "return fader ratio {}", half / full);
    // muted: the return is gone (the smoothing leaves the first few ms)
    assert!(diff(&muted.0, &dry.0) < 0.1 * full, "mute leaves {}", diff(&muted.0, &dry.0));
}

#[test]
fn sends_work_on_l_and_on_an_input_and_are_post_fader() {
    let mut l = Live::new(true);
    {
        let e = l.c.engine_mut();
        e.set_aux_fx(AuxFxKey::DelayWet, 100.0);
        e.set_aux_send("mic", 0.8);
        assert_eq!(e.aux_send("mic"), Some(0.8));
        assert!(e.set_aux_send("L", 0.5));
        assert!(!e.set_aux_send("nonesuch", 0.5));
        assert_eq!(e.aux_send("L"), Some(0.5));
        // a muted mic strip sends nothing: the send is after the fader
        e.set_input_mute(0, true);
    }
    l.push(2048);
    l.run(60);
    assert!(l.c.engine().aux_peak() < 1e-9, "a muted input, no send: {}", l.c.engine().aux_peak());
    l.c.engine_mut().set_input_mute(0, false);
    l.run(60);
    assert!(l.c.engine().aux_peak() > 1e-3, "the mic reaches the bus: {}", l.c.engine().aux_peak());
}

#[test]
fn the_mixer_state_carries_the_aux() {
    let mut l = Live::new(false);
    let e = l.c.engine_mut();
    e.set_aux_send("A", 0.3);
    e.set_aux_send("L", 0.2);
    e.set_aux_return(0.55);
    e.set_aux_fx(AuxFxKey::DelayTime, 40.0);
    let m = e.mixer_state();
    assert_eq!((m.strips[0].aux, m.loop_channel.aux, m.aux.level), (0.3, 0.2, 0.55));
    let text = m.to_json().canonical_js();
    let back = djrust::mixer::MixerState::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    // into a fresh engine
    let mut other = Live::new(false);
    other.c.engine_mut().apply_mixer_state(&back);
    assert_eq!(other.c.engine().mixer_state(), m);
}

#[test]
fn a_delay_throw_throws_the_aimed_deck_into_the_echo_for_its_bar_and_puts_everything_back() {
    let mut l = Live::new(false);
    {
        let e = l.c.engine_mut();
        // the bus has its returns up, the sends are all zero: only the macro sends
        e.set_aux_fx(AuxFxKey::DelayWet, 15.0);
        e.set_aux_fx(AuxFxKey::ReverbWet, 0.0);
    }
    l.run(20);
    assert_eq!(l.c.engine().aux_peak(), 0.0);
    let (start, bpm) = l.c.engine().sample_master_fire("A");
    assert!(start > l.c.engine().frame(), "on a bar ahead of now");
    l.act(|e| {
        e.perform(&macro_action("A", "delay-throw"), PerformCtx::default());
    });
    // until its bar: nothing
    while l.c.engine().frame() + (BLOCK as i64) <= start {
        l.run(1);
        assert_eq!(l.c.engine().aux_peak(), 0.0, "before the bar, frame {}", l.c.engine().frame());
    }
    // the echo is 0.38 s (the throw's time), so its first repeat comes back then
    l.run(SECOND / 2 + 20);
    assert!(l.c.engine().aux_peak() > 1e-3, "thrown into the echo");
    assert!(l.c.engine().aux_mirror.is_running(AuxMacro::DelayThrow));
    assert_eq!(l.c.engine().aux_macros_running() & AuxMacro::DelayThrow.bit(), AuxMacro::DelayThrow.bit());
    // one bar at the deck's tempo: 4 beats; then the macro is over and the send is back to 0
    let bar_frames = (4.0 * 60.0 / bpm * SR) as i64;
    let over = start + bar_frames + 2 * BLOCK as i64;
    while l.c.engine().frame() < over {
        l.run(1);
    }
    assert_eq!(l.c.engine().aux_macros_running(), 0);
    // the echo's tail rings out and the bus sleeps: with no send, nothing returns in the end
    l.run(13 * SECOND);
    assert_eq!(l.c.engine().aux_peak(), 0.0);
    assert_eq!(l.c.engine().aux_send("A"), Some(0.0), "the knob was never touched");
}

#[test]
fn pan_and_shape_macros_hold_the_aimed_decks_parts_from_the_bar() {
    // hard left on A: from the bar, the right channel loses A's non-drum parts
    let go = |fire: bool| {
        let mut l = Live::new(false);
        {
            let e = l.c.engine_mut();
            e.set_crossfader(0.0); // only A
            e.pause("B");
        }
        l.run(10);
        let (start, _) = l.c.engine().sample_master_fire("A");
        if fire {
            l.act(|e| {
                e.perform(&macro_action("A", "hard-left"), PerformCtx::default());
            });
        }
        l.run(BAR + SECOND);
        (l.out_l, l.out_r, start)
    };
    let (dl, dr, start) = go(false);
    let (ml, mr, _) = go(true);
    // identical until the bar
    let s = start as usize;
    assert!(s < dl.len());
    let first = (0..dl.len()).find(|&i| dl[i] != ml[i] || dr[i] != mr[i]).expect("the macro changed the mix");
    assert!(first >= s - BLOCK && first <= s + 2 * BLOCK, "first change at {first}, bar at {s}");
    // right loses energy, left gains (the pad and the lead moved over); the kick is still both
    let e = |x: &[f32]| x[s + 2 * BLOCK..].iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    assert!(e(&mr) < 0.97 * e(&dr), "right {} vs {}", e(&mr), e(&dr));
    assert!(e(&ml) > 1.03 * e(&dl), "left {} vs {}", e(&ml), e(&dl));
    // a pan or shape macro needs a net deck, and a deck: refused elsewhere
    let mut l = Live::new(false);
    let e = l.c.engine_mut();
    assert!(!e.fire_aux_macro("nonesuch", AuxMacro::Mono, None));
    assert!(!e.fire_aux_macro("L", AuxMacro::Mono, None), "the loop channel has no parts to pan");
    assert!(e.fire_aux_macro("L", AuxMacro::Cathedral, Some(2)), "but it can go into the room");
    assert!(e.fire_aux_macro("B", AuxMacro::Pulse, None));
}

#[test]
fn every_one_of_the_eleven_fires_from_perform_and_shows_it_is_running() {
    for m in AuxMacro::ALL {
        let mut l = Live::new(false);
        l.c.engine_mut().trace = Some(Vec::new());
        l.act(|e| {
            let r = e.perform(&macro_action("A", m.id()), PerformCtx::default());
            assert!(r.ran(), "{} refused: {:?}", m.id(), r);
        });
        l.run(20);
        let per_deck = l.c.engine().deck("A").unwrap().mirror.aux_mask.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(per_deck & m.bit(), m.bit(), "{} not running on A", m.id());
        // and the action log has it as a replayable macro op
        assert!(
            l.events.iter().any(|ev| matches!(&ev.op, ArrangeOp::Deck(DeckAction::Macro { id, .. }) if id == m.id())),
            "{} in the log",
            m.id()
        );
    }
}

/// The aux moves, as the log records them, and the macros, then offline.
fn record() -> (Live, Arrangement) {
    let mut live = Live::new(true);
    {
        let e = live.c.engine_mut();
        // set before the recording starts: the arrangement's start carries it
        e.set_aux_send("B", 0.25);
        e.set_aux_fx(AuxFxKey::ReverbSize, 70.0);
        e.set_aux_return(0.7);
    }
    live.bpm = live.c.engine().master_tempo().1;
    live.c.engine_mut().trace = Some(Vec::new());
    live.c.pump();
    let (decks, start) = live.c.engine().arrange_start();
    live.push(((SR * 0.02) as usize).max(1024));
    live.run(40);
    live.act(|e| {
        e.set_aux_send("A", 0.5);
        e.set_aux_send("mic", 0.6);
        e.set_aux_fx(AuxFxKey::DelayTime, 33.0);
        e.set_aux_fx(AuxFxKey::DelayFeedback, 55.0);
        e.set_aux_fx(AuxFxKey::DelayWet, 60.0);
    });
    live.run(SECOND);
    live.act(|e| {
        e.perform(&macro_action("A", "ping-pong"), PerformCtx::default());
        e.perform(&macro_action("B", "delay-throw"), PerformCtx::default());
    });
    // past the bar they start on, into the middle of them
    live.run(BAR + SECOND);
    live.act(|e| {
        e.set_aux_send("L", 0.3);
        e.set_aux_return(0.9);
        e.perform(&macro_action("B", "tighten"), PerformCtx::default());
        e.perform(&macro_action("A", "cathedral"), PerformCtx::default());
    });
    live.run(BAR + SECOND);
    live.act(|e| {
        e.set_aux_mute(true);
        e.perform(&macro_action("A", "auto-pan"), PerformCtx::default());
    });
    live.run(SECOND);
    live.act(|e| {
        e.set_aux_mute(false);
        e.set_aux_send("mic", 0.0);
        e.perform(&macro_action("B", "pulse"), PerformCtx::default());
        e.perform(&macro_action("A", "mono"), PerformCtx::default());
    });
    live.run(2 * BAR);
    let mut arr = Arrangement::new(live.bpm, decks, live.events.clone());
    arr.start = start;
    (live, arr)
}

#[test]
fn an_arrangement_with_aux_sends_and_macros_renders_offline_to_what_the_live_engine_played() {
    let (live, arr) = record();
    // the aux moves are replayable ops, the macros ordinary macro actions
    let ops: Vec<String> = arr.events.iter().map(|e| e.op.op()).collect();
    for want in ["aux", "macro"] {
        assert!(ops.iter().any(|o| o == want), "{want} in {ops:?}");
    }
    // the start carries what was set before the recording: B's send, the room, the return
    let setup = arr.start.setup.iter().map(|o| o.json().js_stringified()).collect::<Vec<_>>().join(" ");
    assert!(setup.contains(r#""strip":"B""#) && setup.contains(r#""send":0.25"#), "{setup}");
    assert!(setup.contains(r#""reverb-size":70"#) && setup.contains(r#""return":0.7"#), "{setup}");
    // through its JSON
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
        let d = (r.audio.left[i] - live.out_l[i]).abs().max((r.audio.right[i] - live.out_r[i]).abs());
        if d > 0.0 && first.is_none() {
            first = Some(i);
        }
        worst = worst.max(d);
    }
    assert_eq!(
        worst,
        0.0,
        "offline differs from live by up to {worst}, first at frame {first:?} (block {:?})",
        first.map(|f| f / BLOCK)
    );
    // and the aux was in it: the same arrangement without its aux moves is a different mix
    let mut plain = back.clone();
    plain.events.retain(|e| e.op.op() != "aux");
    plain.start.setup.clear();
    let p = render_arrangement(
        &plain,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(beats),
            inputs: vec![("mic".into(), voice())],
            ..Default::default()
        },
    );
    assert_ne!(p.audio.left, r.audio.left, "the aux made no difference");
}

#[test]
fn stems_with_the_aux_add_up_to_the_master() {
    let (_, arr) = record();
    let r = render_arrangement(
        &arr,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(40.0),
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
    let aux = r.stems.iter().find(|(n, _)| n == "aux").unwrap();
    assert!(aux.1.left.iter().fold(0f32, |p, v| p.max(v.abs())) > 1e-3, "the aux stem is silent");
}

#[test]
fn the_same_aux_arrangement_renders_the_same_twice() {
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

#[test]
fn the_aux_live_op_refuses_what_it_cannot_do_and_a_hand_written_one_plays() {
    let mut l = Live::new(true);
    let e = l.c.engine_mut();
    let p = |s: &str| JsonValue::parse(s).unwrap();
    assert!(e.apply_live_op("aux", &p(r#"{"strip":"Z","send":0.5}"#)).is_err());
    assert!(e.apply_live_op("aux", &p(r#"{"strip":"A"}"#)).is_err(), "a strip needs a send");
    e.apply_live_op("aux", &p(r#"{"strip":"mic","send":0.4,"return":0.6,"delay-wet":90,"mute":false}"#)).unwrap();
    assert_eq!(e.aux_send("mic"), Some(0.4));
    assert_eq!(e.aux_settings().level, 0.6);
    assert_eq!(e.aux_settings().get(AuxFxKey::DelayWet), 90.0);
    e.apply_live_op("aux", &p(r#"{"reset":true}"#)).unwrap();
    assert_eq!(e.aux_settings().get(AuxFxKey::DelayWet), 15.0);
    assert_eq!(e.aux_settings().level, 0.6, "reset is the sliders, not the fader");
}

#[test]
fn the_aux_binding_targets_move_the_bus() {
    use djrust::control::controller::Action;
    let mut l = Live::new(true);
    let e = l.c.engine_mut();
    let t = |id: &str, deck: Option<&str>, value: f64, args: &[(&str, &str)], pressed: bool| Action::Target {
        id: id.into(),
        deck: deck.map(Into::into),
        slot: None,
        index: 0,
        value,
        pressed,
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    };
    e.apply_action(t("auxSend", Some("A"), 0.5, &[], true));
    assert_eq!(e.aux_send("A"), Some(0.5));
    // an input's label overrides the deck
    e.apply_action(t("auxSend", Some("A"), 0.25, &[("strip", "mic")], true));
    assert_eq!(e.aux_send("mic"), Some(0.25));
    assert_eq!(e.aux_send("A"), Some(0.5));
    e.apply_action(t("auxReturn", None, 0.3, &[], true));
    assert_eq!(e.aux_settings().level, 0.3);
    // a fader's travel across the slider's own range
    e.apply_action(t("auxFx", None, 1.0, &[("key", "delay-feedback")], true));
    assert_eq!(e.aux_settings().get(AuxFxKey::DelayFeedback), 90.0);
    e.apply_action(t("auxFx", None, 0.0, &[("key", "delay-time")], true));
    assert_eq!(e.aux_settings().get(AuxFxKey::DelayTime), 1.0);
    // mute toggles on the press, ignores the release
    e.apply_action(t("auxMute", None, 1.0, &[], true));
    assert!(e.aux_settings().mute);
    e.apply_action(t("auxMute", None, 0.0, &[], false));
    assert!(e.aux_settings().mute);
    e.apply_action(t("auxMute", None, 1.0, &[], true));
    assert!(!e.aux_settings().mute);
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
    // a slider that is none of the six is noted, not guessed
    e.apply_action(t("auxFx", None, 0.5, &[("key", "nonesuch")], true));
    assert_eq!(e.unhandled.len(), 1);
}
