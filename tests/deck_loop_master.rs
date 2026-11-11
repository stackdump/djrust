//! The loops' master FX (`LoopMasterFxTests.swift`, `LoopMasterBusTests` and the engine's
//! `LoopMasterEngineTests`): the decks' master chain and macros on the loop channel "L", after
//! the voices are summed and before the limiter, clocked by the loops' bar grid. Off is exact; a
//! sweep is a loop bar long and lands on the loops' bar line; vinyl brake slows every loop and
//! drops them back in time; panic restores; the limiter still holds; A / B and L stay apart
//! unless linked. Each test names the Swift test it ports.
//!
//! Not here: the APC aiming at L (`SurfaceController`), the poke round trip and the journal
//! checkpoint, and the recorded-set replay: sessions and the controller glue are other slices.

mod common_loops;

use common_loops::*;
use djrust::fx::loop_fx::LoopFxLimiter;
use djrust::fx::loop_master::LoopMasterCommand;
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::session::types::LoopFx;
use djrust::host::deck::loops::LoopMsg;
use djrust::host::deck::{engine, EngineConfig};

const BAR120: usize = 96_000; // one bar at 120 bpm, 48 kHz

/// Two sines a loop: a low one (375 Hz) and a bright one (6 kHz), whole periods.
fn two_tones() -> Vec<std::sync::Arc<djrust::deck::turntable::SampleBuffer>> {
    vec![sine_buffer(375.0, 0.2, 1.0), sine_buffer(6000.0, 0.2, 1.0)]
}

fn fire(rig: &mut FxBankRig, m: MasterMacro, bars: i16, frame: i64, bpm: f64) {
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Macro {
        id: m.raw(),
        bars,
        frame,
        bpm,
    }));
}

fn is_active(rig: &FxBankRig, m: MasterMacro) -> bool {
    rig.bank.master().mask() & m.bit() != 0
}

fn at_rest(rig: &FxBankRig) -> bool {
    let (_, _, idle) = rig.bank.master().filter_corners();
    idle && rig.bank.master().mask() == 0
}

// LoopMasterBusTests.atRestAndAfterAMacroTheBusIsBitIdentical
#[test]
fn at_rest_and_after_a_macro_the_bus_is_bit_identical() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(4 * BAR120 / 2);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    fire(&mut rig, MasterMacro::SweepLp, 1, 0, 240.0); // one bar = 1 s
    let wet = rig.render(4 * BAR120 / 2);
    assert!(
        wet.0[24_000..48_000] != dry.0[24_000..48_000],
        "the sweep is heard"
    );
    // 1 s, then the filter's last ramp: well after, the same bytes again
    let from = 96_000;
    assert!(wet.0[from..] == dry.0[from..]);
    assert!(wet.1[from..] == dry.1[from..]);
    assert!(at_rest(&rig));
    assert!(rig.bank.limiter().idle());
}

// LoopMasterBusTests.anLPSweepTakesTheTopOffTheLoopsOverItsBar
#[test]
fn an_lp_sweep_takes_the_top_off_the_loops_over_its_bar() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(2 * BAR120);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    fire(&mut rig, MasterMacro::SweepLp, 1, 0, 120.0);
    let wet = rig.render(2 * BAR120);
    // 0.8 s in, the LP is half way down (slider ~50: ~1.5 kHz); 4608 frames = whole periods
    let w = 38_000..(38_000 + 4608);
    let hf_dry = tone_amplitude(&dry.0[w.clone()], 6000.0);
    let hf_wet = tone_amplitude(&wet.0[w.clone()], 6000.0);
    let lf_dry = tone_amplitude(&dry.0[w.clone()], 375.0);
    let lf_wet = tone_amplitude(&wet.0[w], 375.0);
    assert!(
        db(hf_wet / hf_dry) < -30.0,
        "the 6 kHz tone is swept out: {} dB",
        db(hf_wet / hf_dry)
    );
    assert!(
        db(hf_wet / hf_dry) < db(lf_wet / lf_dry) - 15.0,
        "far more than the 375 Hz one"
    );
    // a bar and a bit later, all of it is back
    let after = (BAR120 + 24_000)..(BAR120 + 24_000 + 4608);
    let hf_back = tone_amplitude(&wet.0[after], 6000.0);
    assert!(db(hf_back / hf_dry).abs() < 0.1);
}

// LoopMasterBusTests.aOneBarSweepSpansExactlyOneLoopBar
#[test]
fn a_one_bar_sweep_spans_exactly_one_loop_bar() {
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    let fire_at = 10_000usize; // inside a block
    fire(&mut rig, MasterMacro::SweepLp, 1, fire_at as i64, 120.0);
    let (mut last_active_end, mut first_inactive_end) = (0usize, 0usize);
    while (rig.frame as usize) < 3 * BAR120 {
        rig.skip(512);
        if is_active(&rig, MasterMacro::SweepLp) {
            last_active_end = rig.frame as usize;
        } else if first_inactive_end == 0 {
            first_inactive_end = rig.frame as usize;
        }
    }
    // running through every block that ends by fire + 1 bar, done in the block holding its end
    assert!(
        last_active_end <= fire_at + BAR120 && last_active_end > fire_at + BAR120 - 512,
        "{last_active_end}"
    );
    assert!(
        first_inactive_end > fire_at + BAR120 && first_inactive_end <= fire_at + BAR120 + 512,
        "{first_inactive_end}"
    );
}

// LoopMasterBusTests.aFireLandsOnItsFrameInsideABlock
#[test]
fn a_fire_lands_on_its_frame_inside_a_block() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(BAR120);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    let at = 10_000usize;
    fire(&mut rig, MasterMacro::VinylBrake, 1, at as i64, 120.0);
    let wet = rig.render(BAR120);
    assert!(wet.0[..at] == dry.0[..at], "nothing before its frame");
    assert!(
        wet.0[at..at + 256] != dry.0[at..at + 256],
        "the brake from it"
    );
}

// LoopMasterBusTests.vinylBrakeSlowsEveryLoopAndDropsThemBackInTime
#[test]
fn vinyl_brake_slows_every_loop_and_drops_them_back_in_time() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(3 * BAR120);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    let at = 4096usize;
    fire(&mut rig, MasterMacro::VinylBrake, 1, at as i64, 120.0);
    let mut mid: Option<bool> = None;
    let mut out: Vec<f32> = Vec::new();
    while out.len() < 3 * BAR120 {
        out.extend(rig.render(512).0);
        if mid.is_none() && rig.frame as usize >= at + BAR120 / 2 {
            mid = Some(is_active(&rig, MasterMacro::VinylBrake));
        }
    }
    assert_eq!(mid, Some(true), "the brake shows while it runs");
    // spinning down: the bright tone falls in pitch (fewer crossings in a window than dry)
    let crossings = |x: &[f32]| {
        x.windows(2)
            .filter(|w| (w[1] >= 0.0) != (w[0] >= 0.0))
            .count()
    };
    let w = (at + BAR120 / 2)..(at + BAR120 / 2 + 4800);
    assert!((crossings(&out[w.clone()]) as f64) < 0.6 * crossings(&dry.0[w]) as f64);
    // stopped for its last quarter
    let stop = (at + BAR120 * 77 / 100)..(at + BAR120 * 99 / 100);
    assert!(peak(&out[stop]) < 1e-4);
    // released at fire + 1 bar over 5 ms: every loop where it would have been, to the byte
    let back = at + BAR120 + 480;
    assert!(out[back..dry.0.len()] == dry.0[back..]);
    assert!(!is_active(&rig, MasterMacro::VinylBrake));
}

// LoopMasterBusTests.panicRestoresTheLoopsAtOnce
#[test]
fn panic_restores_the_loops_at_once() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(3 * BAR120);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    fire(&mut rig, MasterMacro::SweepLp, 4, 0, 120.0);
    fire(&mut rig, MasterMacro::OctaveUp, 4, 0, 120.0);
    rig.skip(BAR120);
    assert!(is_active(&rig, MasterMacro::SweepLp) && is_active(&rig, MasterMacro::OctaveUp));
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Panic));
    rig.skip(512);
    assert_eq!(rig.bank.master().mask(), 0);
    let start = rig.frame as usize;
    let rest = rig.render(3 * BAR120 - start);
    // the LP's jump back and the pitch shifter's fade out, then the dry loops again
    let from = BAR120 / 2;
    assert!(rest.0[from..] == dry.0[start + from..]);
}

// LoopMasterBusTests.panicReleasesABrakeInTime
#[test]
fn panic_releases_a_brake_in_time() {
    let dry = FxBankRig::new(&two_tones(), &[]).render(2 * BAR120);
    let mut rig = FxBankRig::new(&two_tones(), &[]);
    fire(&mut rig, MasterMacro::VinylBrake, 2, 0, 120.0);
    rig.skip(BAR120 / 2);
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Panic));
    let rest = rig.render(2 * BAR120 - rig.frame as usize);
    let start = BAR120 / 2;
    assert!(rest.0[1024..] == dry.0[start + 1024..], "back in time");
}

// LoopMasterBusTests.theLimiterStillBoundsThePeaksAfterTheChain
#[test]
fn the_limiter_still_bounds_the_peaks_after_the_chain() {
    // loud loops (just under full scale together), pitched up and distorted on the bus
    let clips: Vec<_> = (0..6)
        .map(|k| sine_buffer(375.0 + 125.0 * k as f64, 0.155, 1.0))
        .collect();
    let mut rig = FxBankRig::new(&clips, &[]);
    for (key, value) in [(MasterFxKey::Distortion, 100), (MasterFxKey::CrushBits, 60)] {
        rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Slider {
            key: key.index() as u8,
            value,
        }));
    }
    fire(&mut rig, MasterMacro::OctaveUp, 2, 0, 120.0);
    let out = rig.render(2 * BAR120);
    let p = peak(&out.0).max(peak(&out.1));
    assert!(p <= LoopFxLimiter::CEILING + 1e-6, "{p}");
    assert!(p > 0.8, "and it is loud: {p}");
    assert!(out.0.iter().all(|v| v.is_finite()));
}

// LoopMasterEngineTests.aMacroOnLLandsOnTheLoopsNextBarAndLastsItsBarsOfTheirTempo (the
// engine-level half: a fire waits for the loops' launch line, at the loops' tempo, and the
// mirror shows it at once and until it is over)
#[test]
fn a_macro_on_l_lands_on_the_loops_next_bar_and_lasts_its_bars_of_their_tempo() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.load_demo("A"));
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let mut step = |e: &mut djrust::host::deck::Engine, r: &mut djrust::host::deck::Renderer, n: usize| {
        for _ in 0..n {
            r.render_block(&mut l, &mut rr);
            e.service_loops();
        }
    };
    step(&mut e, &mut r, 40);
    // the demo loop plays at 120 bpm: its clock is the loops' clock
    let at = e.loop_master_fire_frame();
    let now = e.loop_frames();
    assert!(
        at > now && at <= now + BAR120 as i64 + 4096,
        "{at} vs {now}"
    );
    e.fire_loop_master(MasterMacro::SweepLp, Some(1), false);
    assert!(
        e.is_loop_master_active(MasterMacro::SweepLp),
        "shows at once (waiting for its bar)"
    );
    let until = |e: &mut djrust::host::deck::Engine, r: &mut djrust::host::deck::Renderer, frame: i64| {
        let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
        while e.loop_frames() < frame {
            r.render_block(&mut l, &mut rr);
            e.service_loops();
        }
    };
    until(&mut e, &mut r, at - 1024);
    assert!(e.loop_master_filter_corners().2, "not before its bar");
    until(&mut e, &mut r, at + BAR120 as i64 / 2);
    assert!(e.is_loop_master_active(MasterMacro::SweepLp));
    let (_, lp, idle) = e.loop_master_filter_corners();
    assert!(!idle && lp < 20_000.0);
    // a 1-bar one: exactly one loop bar, from the next bar line
    until(&mut e, &mut r, at + BAR120 as i64 - 1024);
    assert!(e.is_loop_master_active(MasterMacro::SweepLp));
    until(&mut e, &mut r, at + BAR120 as i64 + 1024);
    assert!(
        !e.is_loop_master_active(MasterMacro::SweepLp),
        "over after one loop bar"
    );
}

// LoopMasterEngineTests.deckMacrosLeaveTheLoopsAloneUnlessLinked (the link half): a deck's own
// fire and panic (`Deck.fire` / `Deck.panic` end in `onMasterMacro`), not the hook called by hand
#[test]
fn deck_macros_leave_the_loops_alone_unless_linked() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let deck_has = |e: &djrust::host::deck::Engine, m: MasterMacro| {
        e.deck("A").unwrap().mirror.is_master_active(m)
    };
    assert!(!e.loop_fx_link());
    assert!(e.fire_master("A", MasterMacro::SweepHp, None));
    r.render_block(&mut l, &mut rr);
    r.render_block(&mut l, &mut rr);
    assert!(deck_has(&e, MasterMacro::SweepHp), "the deck runs it");
    assert!(
        e.active_loop_master_macros().is_empty(),
        "not linked: the loops keep theirs"
    );
    e.set_loop_fx_link(true);
    assert!(e.fire_master("A", MasterMacro::OctaveDown, None));
    r.render_block(&mut l, &mut rr);
    r.render_block(&mut l, &mut rr);
    assert_eq!(
        e.active_loop_master_macros(),
        [MasterMacro::OctaveDown],
        "linked: the loops take it at once"
    );
    assert!(deck_has(&e, MasterMacro::OctaveDown), "and the deck too");
    assert!(e.panic("A"));
    r.render_block(&mut l, &mut rr);
    r.render_block(&mut l, &mut rr);
    assert!(
        e.active_loop_master_macros().is_empty(),
        "and the deck's panic releases them"
    );
    // a deck that is not there neither fires nor reaches the loops
    assert!(!e.fire_master("Z", MasterMacro::SweepLp, None));
    r.render_block(&mut l, &mut rr);
    assert!(e.active_loop_master_macros().is_empty());
    // L's own fire never reaches the deck
    e.set_loop_fx_link(false);
    e.fire_loop_master(MasterMacro::PhaserDrone, None, true);
    r.render_block(&mut l, &mut rr);
    assert!(e.is_loop_master_active(MasterMacro::PhaserDrone));
    assert!(!deck_has(&e, MasterMacro::PhaserDrone));
}

// LoopMasterEngineTests.netMacrosDoNothingOnLAndPanicRestores (the panic half: the master
// macros riser and build-crush run, panic restores, and the chain is at rest again)
#[test]
fn panic_on_l_restores_the_chain() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    e.fire_loop_master(MasterMacro::Riser, None, true);
    e.fire_loop_master(MasterMacro::BuildCrush, None, true);
    for _ in 0..40 {
        r.render_block(&mut l, &mut rr);
    }
    let mut got = e.active_loop_master_macros();
    got.sort_by_key(|m| m.raw());
    assert_eq!(got, [MasterMacro::Riser, MasterMacro::BuildCrush]);
    e.loop_master_panic();
    for _ in 0..8 {
        r.render_block(&mut l, &mut rr);
    }
    assert!(e.active_loop_master_macros().is_empty());
    for _ in 0..60 {
        r.render_block(&mut l, &mut rr);
    }
    assert!(e.loop_master_filter_corners().2, "bypassed again");
    let _ = LoopFx::default();
}
