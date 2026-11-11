//! Swift tests of the deck's master chain and the headphone cue that had no Rust counterpart, for
//! behaviour the engine already has (package F of the parity audit): the Feel macros and the
//! transport clock at the deck (`MasterFxDeckTests`), and the headphone cue (`CueOutputTests`).
//! Each test names the Swift test it ports.

use std::sync::Arc;

use djrust::fx::{MasterMacro, StripKnob};
use djrust::json::js_round;
use djrust::pflow::Project;
use djrust::wave::{WaveOptions, WaveRunner};
use djrust::{compose, JsonObject};
use djrust::host::deck::output::{Interleaver, Routing};
use djrust::host::deck::{engine, Command, DeckCore, DeckMirror, Engine, EngineConfig, Renderer};

// MARK: MasterFxDeckTests: the master chain on a net deck

const RATE: f64 = 22_050.0;
const BLOCK: usize = 128;

/// `MasterFxDeckTests.project`: `structure` "loop" leaves the override out.
fn project(genre: &str, seed: f64, structure: &str) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if structure != "loop" {
        ov.set("structure", structure.into());
    }
    compose(genre, &ov.into())
}

/// `MasterFxDeckTests.source` and `render`: a `WaveDeckSource` at 22,050 Hz, blocks of 128,
/// commands applied at the top of a block (the main thread's sends).
struct Src {
    core: DeckCore,
    mirror: Arc<DeckMirror>,
    frame: i64,
}

impl Src {
    fn new(p: &Project, tempo: Option<f64>) -> Src {
        let mirror = Arc::new(DeckMirror::new());
        let opts = WaveOptions::default();
        let mut core = DeckCore::new("A", RATE, opts.clone(), false, mirror.clone());
        core.apply(Command::Load(WaveRunner::prepare(p, RATE, &opts, 0)));
        if let Some(t) = tempo {
            core.apply(Command::Tempo(t));
        }
        core.apply(Command::Play);
        Src {
            core,
            mirror,
            frame: 0,
        }
    }

    fn send(&mut self, c: Command) {
        self.core.apply(c);
    }

    fn fire(&mut self, m: MasterMacro, bars: i16) {
        self.send(Command::MasterMacro { m, bars });
    }

    fn render(&mut self, blocks: usize) {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..blocks {
            self.core.render(&mut l, &mut r, self.frame);
            self.frame += BLOCK as i64;
        }
    }

    fn active(&self, m: MasterMacro) -> bool {
        self.mirror.is_master_active(m)
    }
}

// MasterFxDeckTests.feelMovesTheTempoAndPutsItBack: a 1-bar snap to Chill: tempo = round(genre
// BPM x 0.6), restored at the end. (The Tempo events reach the runner through the deck's
// `dispatch_events`: no deck.json case fires a Feel macro.)
#[test]
fn feel_moves_the_tempo_and_puts_it_back() {
    let p = project("techno", 42.0, "loop");
    let mut src = Src::new(&p, None);
    let genre = p.tempo;
    src.render(1);
    src.fire(MasterMacro::FeelChill, 1);
    src.render(4);
    assert_eq!(src.mirror.tempo(), js_round(genre * 0.6));
    assert!(src.active(MasterMacro::FeelChill));
    src.render((RATE * 60.0 / js_round(genre * 0.6) * 4.0 / 128.0) as usize + 20);
    assert_eq!(src.mirror.tempo(), genre);
    assert!(!src.active(MasterMacro::FeelChill));
}

// MasterFxDeckTests.panicPutsTheSlidersBackAndStopRestoresToo
#[test]
fn panic_puts_the_sliders_back_and_stop_restores_too() {
    let p = project("techno", 42.0, "loop");
    let mut src = Src::new(&p, Some(120.0));
    src.render(1);
    src.fire(MasterMacro::SweepHp, 4);
    src.render(149);
    src.send(Command::Panic);
    src.render(50);
    assert!(src.mirror.active_master_macros().is_empty());
    src.render(40);
    let (hp, _, bypassed) = src.mirror.master_filter_corners();
    assert_eq!(hp, 20.0);
    assert!(bypassed);
    src.render(1);
    src.fire(MasterMacro::OctaveDown, 4);
    src.render(19);
    assert!(src.active(MasterMacro::OctaveDown));
    src.send(Command::Stop);
    src.render(2);
    assert!(src.mirror.active_master_macros().is_empty());
}

// MasterFxDeckTests.theTransportClockStopsWhilePaused: the macro's clock is the deck's, so a
// pause holds it (the fx flush path): 3 s paused and the sweep (0.8 s) is still in effect, and
// a second of play ends it.
#[test]
fn the_transport_clock_stops_while_paused() {
    let p = project("techno", 42.0, "loop");
    let mut src = Src::new(&p, Some(300.0));
    src.render(1);
    src.fire(MasterMacro::SweepLp, 1);
    src.render(3);
    src.send(Command::Pause);
    src.render((3.0 * RATE / 128.0) as usize);
    assert!(src.active(MasterMacro::SweepLp));
    src.send(Command::Play);
    src.render((1.0 * RATE / 128.0) as usize);
    assert!(!src.active(MasterMacro::SweepLp));
}

// MARK: CueOutputTests: the headphone cue on a 4-channel interface

const CUE_RATE: f64 = 22_050.0;

/// `CueOutputTests.make`: two decks with the demo loop, a 4-channel interface, master on 1/2 and
/// the cue on 3/4. The app's engine, and Swift's cue defaults (mix 0, level 0.7): the engine's
/// own defaults are 0.5 / 0.5, which `swift_cue_defaults_are_swifts` holds (ignored).
fn cue_rig() -> (Engine, Renderer, Interleaver) {
    let (mut e, r) = engine(EngineConfig::new(CUE_RATE));
    assert!(e.load_demo("A") && e.load_demo("B"));
    e.set_headphone_mix(0.0);
    e.set_headphone_level(0.7);
    (e, r, Interleaver::new())
}

/// `renderChannels`: the next `frames` frames, one `Vec` per channel.
fn channels(r: &mut Renderer, inter: &mut Interleaver, frames: usize) -> Vec<Vec<f32>> {
    let routing = Routing::Pairs {
        master_pair: 0,
        cue_pair: 2,
    };
    let mut out = vec![0f32; 4 * 512];
    let mut c = vec![Vec::new(); 4];
    while c[0].len() < frames {
        inter.fill(r, &routing, 4, &mut out, None);
        for f in out.chunks(4) {
            for (k, ch) in c.iter_mut().enumerate() {
                ch.push(f[k]);
            }
        }
    }
    c.iter_mut().for_each(|ch| ch.truncate(frames));
    c
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

const TAIL: usize = 4000;
const SECOND: usize = 22_050;

// CueOutputTests.theMasterMixIsOnTheFirstPairOnly: nothing cued, headphone mix = cue only (0): the
// cue pair is silent while the master pair plays.
#[test]
fn the_master_mix_is_on_the_first_pair_only() {
    let (mut e, mut r, mut inter) = cue_rig();
    e.play("A"); // no PFL anywhere
    let c = channels(&mut r, &mut inter, SECOND);
    assert!(rms(&c[0][TAIL..]) > 0.01 && rms(&c[1][TAIL..]) > 0.01);
    assert!(
        rms(&c[2][TAIL..]) < 1e-5 && rms(&c[3][TAIL..]) < 1e-5,
        "nothing cued, headphone mix = cue only"
    );
}

// CueOutputTests.theHeadphoneMixBlendsInTheMaster: mix 1 (all master): the same signal, scaled by
// the headphone level.
#[test]
fn the_headphone_mix_blends_in_the_master() {
    let (mut e, mut r, mut inter) = cue_rig();
    e.play("A");
    e.set_headphone_mix(1.0); // all master
    let c = channels(&mut r, &mut inter, SECOND);
    assert!(rms(&c[2][TAIL..]) > 0.01, "the master reaches the headphones");
    let ratio = rms(&c[2][TAIL..]) / rms(&c[0][TAIL..]);
    assert!((ratio - e.headphone_level as f32).abs() < 0.02, "ratio {ratio}");
}

// CueOutputTests.cueIsPreFaderAndFollowsTheDecksEQ: the channel fader and the crossfader do not
// touch the cue (within 0.35), the deck's EQ does (killed: under half).
#[test]
fn cue_is_pre_fader_and_follows_the_decks_eq() {
    let (mut e, mut r, mut inter) = cue_rig();
    e.set_pfl("A", true);
    e.play("A");
    let loud = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    e.set_volume("A", 0.1);
    e.set_crossfader(1.0); // deck A also faded out by the crossfader
    let still = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    assert!(
        (still - loud).abs() / loud < 0.35,
        "the channel fader and crossfader don't touch the cue: {loud} -> {still}"
    );
    // kill the lows: the cue hears it
    for k in [StripKnob::EqLow, StripKnob::EqMid, StripKnob::EqHigh] {
        e.set_strip("A", k, 0.0);
    }
    let killed = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    assert!(killed < still * 0.5, "post-EQ: {still} -> {killed}");
}

// CueOutputTests.levelAndPFLToggleLive: the level knob halves the cue (0.7 -> 0.35, within 0.1 of
// half) and PFL off drops it under 2 %.
#[test]
fn level_and_pfl_toggle_live() {
    let (mut e, mut r, mut inter) = cue_rig();
    e.play("A");
    e.set_pfl("A", true);
    let on = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    e.set_headphone_level(0.35);
    let half = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    assert!((half / on - 0.5).abs() < 0.1, "{on} -> {half}");
    e.set_pfl("A", false);
    let off = rms(&channels(&mut r, &mut inter, SECOND)[2][TAIL..]);
    assert!(off < on * 0.02, "{on} -> {off}");
}

// The Swift engine starts with the headphone mix at 0 (cue only) and the level at 0.7
// (`DJEngine._headphoneMix`, `_headphoneLevel`), which is what makes CueOutputTests'
// `theMasterMixIsOnTheFirstPairOnly` and `levelAndPFLToggleLive` hold on a bare engine. The Rust
// engine starts at 0.5 / 0.5 (engine.rs; audit gap E3, a decision for the owner: the hub's
// booth has no cue routed, so the defaults have no audible effect there).
#[test]
#[ignore = "audit gap E3: the engine's headphone defaults are 0.5 / 0.5, Swift's are mix 0 and level 0.7"]
fn swift_cue_defaults_are_swifts() {
    let (e, _r) = engine(EngineConfig::new(CUE_RATE));
    assert_eq!(e.headphone_mix, 0.0, "nothing cued: the headphones hear the cue only");
    assert_eq!(e.headphone_level, 0.7);
}
