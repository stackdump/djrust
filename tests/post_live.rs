//! The live post chain on the master (docs/POST.md, "Live"):
//!
//! - off (the default) changes nothing, bit for bit, and so does turning it on and off again
//!   once the fade is over;
//! - on, the master leveler brings the decks to the target and the limiter holds the ceiling;
//!   the master fader still turns the finished sound down (the chain is before it);
//! - every change is a `post` live op in the trace, so an arrangement replays it offline to what
//!   the live engine played, and the arrangement's start carries a post that was on before it;
//! - the binding targets toggle it and step / pick the preset.

use djrust::control::controller::Action;
use djrust::fx::post::{measure, PostChain};
use djrust::json::JsonValue;
use djrust::session::arrangement::{ArrangeEvent, Arrangement};
use djrust::TrackSpec;
use djrust::host::deck::offline::{render_arrangement, ArrangeRenderOptions, OfflineConductor};
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 44_100.0;
const BLOCK: usize = OfflineConductor::BLOCK_FRAMES;
const SECOND: usize = SR as usize / BLOCK;

struct Live {
    c: OfflineConductor,
    bpm: f64,
    events: Vec<ArrangeEvent>,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Live {
    fn new(volume: f64) -> Live {
        let (mut e, r) = engine(EngineConfig::new(SR));
        e.deterministic = true;
        let mut c = OfflineConductor::new(e, r);
        {
            let e = c.engine_mut();
            assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
            e.set_volume("A", volume);
            e.set_crossfader(0.0);
            e.play("A");
        }
        c.pump();
        let bpm = c.engine().master_tempo().1;
        Live {
            c,
            bpm,
            events: Vec::new(),
            l: Vec::new(),
            r: Vec::new(),
        }
    }

    fn run(&mut self, blocks: usize) {
        for _ in 0..blocks {
            let (l, r) = self.c.step_block();
            self.l.extend_from_slice(l);
            self.r.extend_from_slice(r);
        }
    }

    fn act(&mut self, f: impl FnOnce(&mut djrust::host::deck::Engine)) {
        f(self.c.engine_mut());
        let beat = self.c.engine().frame() as f64 / SR * self.bpm / 60.0;
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
}

fn preset(name: &str) -> Option<PostChain> {
    PostChain::preset(name)
}

#[test]
fn off_changes_nothing_bit_for_bit() {
    let go = |touch: bool| {
        let mut l = Live::new(0.9);
        if touch {
            let e = l.c.engine_mut();
            e.set_post(false, preset("clean-strong"));
            e.set_post(true, None);
            e.set_post(false, None);
        }
        // past the fade: then it is the dry master again
        l.run(SECOND / 2);
        let (a, b) = (l.l.len(), l.l.len());
        l.run(2 * SECOND);
        (l.l[a..].to_vec(), l.r[b..].to_vec())
    };
    assert_eq!(go(false), go(true));
}

#[test]
fn on_the_leveler_brings_a_quiet_master_to_the_target_and_the_fader_still_works() {
    let mut l = Live::new(0.15);
    l.run(2 * SECOND);
    let quiet = measure(&l.l, &l.r, SR).lufs.unwrap();
    l.c.engine_mut().set_post(true, preset("master"));
    l.run(12 * SECOND);
    let at = l.l.len() - 4 * SECOND * BLOCK;
    let m = measure(&l.l[at..], &l.r[at..], SR);
    assert!(quiet < -25.0, "the deck plays quietly: {quiet}");
    assert!((m.lufs.unwrap() + 14.0).abs() < 1.5, "levelled to {:?}", m.lufs);
    assert!(m.true_peak_db <= -0.9, "tp {}", m.true_peak_db);
    let j = l.c.engine().post_json();
    assert_eq!(j.get("on").and_then(JsonValue::as_bool), Some(true));
    assert_eq!(j.get("preset").and_then(JsonValue::as_str), Some("master"));
    assert!(j.get("gain_db").and_then(JsonValue::as_f64).unwrap() > 8.0, "{}", j.js_stringified());
    // the master fader is after the chain: half the fader is half the level
    l.c.engine_mut().set_master(0.5);
    l.run(SECOND);
    let at = l.l.len() - SECOND / 2 * BLOCK;
    let half = measure(&l.l[at..], &l.r[at..], SR).lufs.unwrap();
    assert!((half - m.lufs.unwrap() + 6.0).abs() < 1.5, "{half} vs {:?}", m.lufs);
}

#[test]
fn post_moves_are_live_ops_and_an_arrangement_replays_them_offline() {
    let mut live = Live::new(0.5);
    // on before the recording: the start carries it
    live.c.engine_mut().set_post(true, preset("treble-shelf"));
    live.c.engine_mut().trace = Some(Vec::new());
    live.c.pump();
    let (decks, start) = live.c.engine().arrange_start();
    let setup: Vec<String> = start.setup.iter().map(|o| o.json().js_stringified()).collect();
    assert!(
        setup.iter().any(|s| s.contains(r#""op":"post""#) && s.contains("treble-shelf")),
        "{setup:?}"
    );
    live.run(SECOND);
    live.act(|e| e.set_post(true, preset("clean-medium")));
    live.run(2 * SECOND);
    live.act(|e| {
        e.apply_live_op("post", &JsonValue::parse(r#"{"toggle":true}"#).unwrap()).unwrap();
    });
    live.run(SECOND);
    live.act(|e| e.set_post(true, preset("master")));
    live.run(2 * SECOND);
    let ops: Vec<String> = live.events.iter().map(|e| e.op.json().js_stringified()).collect();
    assert_eq!(ops.iter().filter(|o| o.contains(r#""op":"post""#)).count(), 3, "{ops:?}");
    let mut arr = Arrangement::new(live.bpm, decks, live.events.clone());
    arr.start = start;
    let text = arr.json().js_stringified();
    let (back, bad) = Arrangement::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(bad, 0);
    let frames = live.l.len();
    let r = render_arrangement(
        &back,
        ArrangeRenderOptions {
            sample_rate: SR,
            beats: Some(frames as f64 / SR * live.bpm / 60.0),
            ..Default::default()
        },
    );
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(r.audio.left.len(), frames);
    let worst = (0..frames)
        .map(|i| (r.audio.left[i] - live.l[i]).abs().max((r.audio.right[i] - live.r[i]).abs()))
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "offline is the live master: worst {worst}");
}

#[test]
fn the_post_live_op_and_binding_targets() {
    let mut l = Live::new(0.5);
    let e = l.c.engine_mut();
    let p = |s: &str| JsonValue::parse(s).unwrap();
    assert!(!e.post.on, "off by default");
    assert!(e.apply_live_op("post", &p(r#"{"preset":"nonesuch"}"#)).is_err());
    e.apply_live_op("post", &p(r#"{"preset":"clean-light"}"#)).unwrap();
    assert!(e.post.on, "naming a preset turns it on");
    assert_eq!(e.post.chain.label(), "clean-light");
    e.apply_live_op("post", &p(r#"{"on":false}"#)).unwrap();
    assert!(!e.post.on);
    assert_eq!(e.post.chain.label(), "clean-light", "off keeps the chain");
    e.apply_live_op("post", &p(r#"{"chain":[{"stage":"highpass","freq":40},"master"]}"#)).unwrap();
    assert!(e.post.on);
    assert_eq!(e.post.chain.stages.len(), 2);
    let t = |id: &str, args: &[(&str, &str)], pressed: bool| Action::Target {
        id: id.into(),
        deck: None,
        slot: None,
        index: 0,
        value: if pressed { 1.0 } else { 0.0 },
        pressed,
        args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    };
    e.apply_action(t("postToggle", &[], true));
    assert!(!e.post.on);
    e.apply_action(t("postToggle", &[], false));
    assert!(!e.post.on, "the release does nothing");
    e.apply_action(t("postPreset", &[("op", "master")], true));
    assert!(e.post.on);
    assert_eq!(e.post.chain.label(), "master");
    e.apply_action(t("postPreset", &[], true));
    assert_eq!(e.post.chain.label(), "none", "next after master wraps");
    e.apply_action(t("postPreset", &[("op", "prev")], true));
    assert_eq!(e.post.chain.label(), "master");
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
    e.apply_action(t("postPreset", &[("op", "warp")], true));
    assert_eq!(e.unhandled.len(), 1);
}
