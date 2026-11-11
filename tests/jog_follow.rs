//! `--history-jog` follows the deck (docs/INPUT-HISTORY.md, "The jog follows the deck"): the
//! aimed deck's platter scrubs the input's history only while the deck is empty or a modifier
//! is held (the Inpulse's SHIFT layer on the wheel, or a `historyJog` control); with a loop on
//! the deck it scratches the loop. A touch latches its target, and each switch is traced.

use std::collections::BTreeMap;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::control::router::{ControllerRouter, RouteResult};
use djrust::deck::turntable::SampleBuffer;
use djrust::history::Mode;
use djrust::host::deck::jog_target::{JogTarget, JogWhy};
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::strip_recall::RecallId;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BEAT: usize = 24_000;
const INPULSE: &str = "DJControl Inpulse 200 MK3";

struct Rig {
    e: Engine,
    r: Renderer,
    feed: rtrb::Producer<f32>,
    fed: usize,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.inputs = vec!["TD-3".into()];
        let (mut e, r) = engine(c);
        let (feed, _) = e.take_input_feed(0).unwrap();
        e.set_input_trim(0, 0.0);
        e.set_input_volume(0, 1.0);
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        let mut g = Rig { e, r, feed, fed: 0 };
        g.run(BEAT * 4);
        g.e.set_history_jog(Some(("B", RecallId::Input(0))));
        g
    }

    fn run(&mut self, frames: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..frames / BLOCK {
            for k in 0..BLOCK {
                let f = self.fed + k;
                let _ = self.feed.push(if f % BEAT == 7 { 1.0 } else { 0.0 });
            }
            self.fed += BLOCK;
            self.r.render_block(&mut l, &mut rr);
        }
    }

    fn load_loop(&mut self) {
        let b = SampleBuffer::new(vec![0.1; 48_000], None, SR);
        assert!(self.e.load_sample("B", PreparedSample::new(b, "loop")));
    }

    fn history(&self) -> Mode {
        self.e.inputs[0].mirror.history_mode()
    }

    /// Touch deck B's platter through the router, SHIFT layer or not, and let the engine act.
    fn touch(&mut self, router: &mut ControllerRouter, shift: bool, down: bool) {
        let ch = if shift { 0x05 } else { 0x02 }; // deck B: channel 2, SHIFT adds 3
        let r = router.route(INPULSE, &[0x90 | ch, 0x08, if down { 0x7F } else { 0 }], 0.0, &mut self.e);
        assert!(matches!(r, RouteResult::Jog(_)), "{r:?}");
        self.e.service_jogs();
        self.run(BLOCK * 4);
    }

    fn trace_texts(&mut self) -> Vec<String> {
        self.e
            .trace
            .replace(Vec::new())
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.entry.text)
            .collect()
    }
}

fn hold(on: bool) -> Action {
    Action::Target {
        id: "historyJog".into(),
        deck: None,
        slot: None,
        index: 0,
        value: if on { 1.0 } else { 0.0 },
        pressed: on,
        args: [("deck".to_string(), "B".to_string())]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn an_empty_deck_scrubs_the_history_and_a_loaded_one_scratches_its_loop() {
    let mut g = Rig::new();
    // empty: the wheel and the CUE button are the history's
    assert_eq!(g.e.jog_target("B"), (JogTarget::History(RecallId::Input(0)), JogWhy::Empty));
    g.e.jog_touch("B", true);
    g.run(BLOCK * 4);
    assert_eq!(g.history(), Mode::Touch);
    g.e.jog_touch("B", false);
    g.run(BLOCK * 4);
    g.e.apply_action(Action::Cue { deck: "B".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.history(), Mode::Live);

    // a loop on the deck: the wheel follows it, the history stays live
    g.load_loop();
    assert_eq!(g.e.jog_target("B"), (JogTarget::Deck, JogWhy::Loaded));
    assert_eq!(g.e.history_aimed("B"), None);
    assert_eq!(g.e.jog_target_words("B", JogTarget::Deck), "LOOP");
    g.e.jog_touch("B", true);
    g.e.jog_scrub("B", -0.25);
    g.run(BLOCK * 4);
    assert_eq!(g.history(), Mode::Live, "the loaded deck's wheel left the history alone");
    g.e.jog_touch("B", false);
    g.run(BLOCK * 4);
    // the aim is kept, as --history-jog set it
    assert_eq!(g.e.history_jog_aim("B"), Some(RecallId::Input(0)));
    // the window's SHIFT key at touch-down sends it to the history
    g.e.jog_touch_shifted("B", true, true);
    g.run(BLOCK * 4);
    assert_eq!(g.history(), Mode::Touch);
    g.e.jog_touch("B", false);
    g.run(BLOCK * 4);
}

#[test]
fn shift_on_the_inpulse_or_a_history_jog_control_reaches_the_history_and_a_touch_latches() {
    let mut g = Rig::new();
    g.load_loop();
    let mut router = ControllerRouter::with_defaults(&["A".to_string(), "B".to_string()]);
    g.e.service_jogs(); // the load's switch, before the trace starts
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "Inpulse".into();

    // plain touch: the loop
    g.touch(&mut router, false, true);
    assert_eq!(g.history(), Mode::Live);
    g.touch(&mut router, false, false);

    // SHIFT held (the wheel's messages on the shift channel): the history
    g.touch(&mut router, true, true);
    assert_eq!(g.history(), Mode::Touch);
    assert_eq!(g.e.jog_target("B").1, JogWhy::Shift);
    // SHIFT let go mid-gesture: the scratch stays on the history until the hand lifts
    let r = router.route(INPULSE, &[0xB2, 0x0A, 0x7F], 0.0, &mut g.e);
    assert!(matches!(r, RouteResult::Jog(_)));
    g.e.service_jogs();
    g.run(BLOCK * 4);
    assert_eq!(g.e.jog_target("B").0, JogTarget::History(RecallId::Input(0)));
    g.touch(&mut router, false, false);
    assert_eq!(g.history(), Mode::Play, "let go on the history: it plays on, delayed");
    assert_eq!(g.e.jog_target("B"), (JogTarget::Deck, JogWhy::Loaded));

    // a historyJog control held: the wheel and the deck's buttons are the history's
    SurfaceHost::perform(&mut g.e, hold(true));
    assert_eq!(g.e.jog_target("B"), (JogTarget::History(RecallId::Input(0)), JogWhy::Hold));
    g.e.apply_action(Action::Cue { deck: "B".into() });
    g.run(BLOCK * 4);
    assert_eq!(g.history(), Mode::Live, "CUE: the history back to live");
    SurfaceHost::perform(&mut g.e, hold(false));
    assert_eq!(g.e.jog_target("B").0, JogTarget::Deck);

    let t = g.trace_texts();
    let switches: Vec<&String> = t.iter().filter(|x| x.contains("jog B")).collect();
    assert_eq!(
        switches,
        [
            "Inpulse: jog B switched to TD-3 HIST (SHIFT held)",
            "Inpulse: jog B switched to LOOP (deck loaded)",
            "Inpulse: jog B switched to TD-3 HIST (historyJog held)",
            "Inpulse: jog B switched to LOOP (deck loaded)",
        ],
        "{t:?}"
    );
}

#[test]
fn a_load_on_the_aimed_deck_is_traced_as_a_switch() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "app".into();
    g.load_loop();
    g.e.service_jogs();
    let t = g.trace_texts();
    assert!(
        t.iter().any(|x| x == "app: jog B switched to LOOP (deck loaded)"),
        "{t:?}"
    );
    // a deck with no aim never reports a jog target
    assert_eq!(g.e.jog_target("A"), (JogTarget::Deck, JogWhy::NoAim));
}
