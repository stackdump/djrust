//! Swift tests of the controllers on a real deck that had no Rust counterpart, for behaviour the
//! engine already has (package F of the parity audit): the jog wheel on a net deck
//! (`DeckScratchTests`, `JogAdapterTests`, `JogEventTests`' shift and deck channels), run from
//! the Inpulse's own MIDI bytes through the router into the engine, and the APC's lights as the
//! device map reads them (`DeviceLightsFromTheRouterTests`). Each test names the Swift test it
//! ports.

use djrust::control::controller::{LightLevel, PadLight};
use djrust::control::leds::LEDSwatch;
use djrust::fx::MasterMacro;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer, Rig};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";

fn render_frames(r: &mut Renderer, frames: usize) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for _ in 0..frames.div_ceil(BLOCK) {
        r.render_block(&mut l, &mut rr);
    }
}

/// A net deck playing techno 1 at 120 bpm (the Swift tests' `TrackSpec(genre: "techno", seed: 1,
/// extra: bpm 120)`), the app's engine at 48 kHz, played for `frames` first.
fn net_deck(decks: &[&str], frames: usize) -> (Rig, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.decks = decks.iter().map(|d| d.to_string()).collect();
    let (mut e, mut r) = engine(c);
    for d in decks {
        let mut spec = TrackSpec::new("techno", 1.0, None);
        spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
        assert!(e.generate(d, &spec));
        e.play(d);
    }
    render_frames(&mut r, frames);
    (Rig::new(e), r)
}

fn tick(rig: &Rig, deck: &str) -> i64 {
    rig.engine.deck(deck).unwrap().mirror.tick()
}

fn scrubbing(rig: &Rig, deck: &str) -> bool {
    rig.engine.deck(deck).unwrap().mirror.is_scrubbing()
}

// DeckScratchTests.jogClockDrivesTheNetClockOnANetDeck: one platter second at 120 bpm = 2 beats
// = 8 ticks, forward and back again, and the release plays on.
#[test]
fn jog_clock_drives_the_net_clock_on_a_net_deck() {
    let (mut rig, mut r) = net_deck(&["A"], 24_000);
    let t0 = tick(&rig, "A");
    rig.engine.jog_touch("A", true);
    rig.engine.jog_scrub("A", 1.0);
    render_frames(&mut r, 12_000);
    assert!(scrubbing(&rig, "A"));
    assert!((tick(&rig, "A") - (t0 + 8)).abs() <= 1, "tick {} vs {}", tick(&rig, "A"), t0 + 8);
    rig.engine.jog_scrub("A", -1.0);
    render_frames(&mut r, 12_000);
    assert!((tick(&rig, "A") - t0).abs() <= 1, "tick {} vs {t0}", tick(&rig, "A"));
    rig.engine.jog_touch("A", false);
    render_frames(&mut r, 24_000);
    assert!(!scrubbing(&rig, "A") && tick(&rig, "A") > t0 + 3);
}

/// The wheel's messages at 100 Hz of controller time: route, tick, render a block each.
struct Wheel {
    rig: Rig,
    r: Renderer,
    t: f64,
}

impl Wheel {
    fn new(decks: &[&str]) -> Wheel {
        let (mut rig, r) = net_deck(decks, 96_000); // 2 s: 16 ticks in, room to scratch back
        assert!(rig.attach(INPULSE));
        Wheel { rig, r, t: 10.0 }
    }

    /// One message, then the 10 ms housekeeping tick (the jog's decay and the engine's service).
    fn send(&mut self, bytes: &[u8]) {
        self.t += 0.01;
        self.rig.route(INPULSE, bytes, self.t).expect("channel voice");
        self.rig.tick(self.t);
    }

    /// `frames` of audio with the housekeeping running every ~10 ms of controller time.
    fn idle(&mut self, frames: usize) {
        let mut done = 0;
        while done < frames {
            self.t += 0.01;
            self.rig.tick(self.t);
            render_frames(&mut self.r, 480);
            done += 480;
        }
    }
}

// DeckScratchTests.theJogAdapterScratchesANetDeck: touch freezes the clock, 400 ticks of the
// wheel move it, letting go hands it back. (Swift hands the adapter `.scratch(ticks: 400)`; the
// wheel sends one tick per message, so it is 400 messages.)
#[test]
fn the_jog_adapter_scratches_a_net_deck() {
    let mut w = Wheel::new(&["A"]);
    w.send(&[0x91, 0x08, 0x7F]); // touch
    render_frames(&mut w.r, 2048);
    assert!(scrubbing(&w.rig, "A"));
    let t0 = tick(&w.rig, "A");
    for _ in 0..400 {
        w.t += 0.002;
        w.rig.route(INPULSE, &[0xB1, 0x0A, 0x01], w.t); // a tick forward
    }
    w.rig.tick(w.t);
    render_frames(&mut w.r, 24_000);
    assert!(tick(&w.rig, "A") > t0, "the wheel moved the net clock: {t0} -> {}", tick(&w.rig, "A"));
    w.send(&[0x91, 0x08, 0x00]); // let go
    render_frames(&mut w.r, 2048);
    assert!(!scrubbing(&w.rig, "A"));
}

// JogAdapterTests.bendSettlesAtDepthAtPlatterSpeed, through the rig: ticks of the outer ring at
// platter speed (248 ticks a turn at 33 1/3 rpm) settle the deck's bend at the 10 % depth; with
// no more ticks it decays back to the tempo (JogAdapterTests.untouchedScratchBendsAndDecays).
#[test]
fn the_ring_bend_settles_at_depth_and_decays() {
    let mut w = Wheel::new(&["A"]);
    let rate = 248.0 * 100.0 / 3.0 / 60.0; // ticks a second
    let (mut next, start) = (w.t, w.t);
    while w.t < start + 2.0 {
        w.t += 0.001;
        if w.t >= next {
            w.rig.route(INPULSE, &[0xB1, 0x09, 0x01], w.t); // a tick of the ring
            next += 1.0 / rate;
        }
        if ((w.t - start) * 1000.0).round() as i64 % 10 == 0 {
            w.rig.tick(w.t);
        }
    }
    let nudge = w.rig.engine.deck("A").unwrap().net_nudge;
    assert!((nudge - 0.10).abs() < 0.01, "settles at depth: {nudge}");
    // the ring stops: a few time constants (0.15 s) later it is gone
    w.idle(2 * SR as usize);
    assert_eq!(w.rig.engine.deck("A").unwrap().net_nudge, 0.0);
}

// JogAdapterTests.touchClearsNudge, through the rig.
#[test]
fn a_touch_clears_the_bend() {
    let mut w = Wheel::new(&["A"]);
    for _ in 0..10 {
        w.send(&[0xB1, 0x09, 0x01]);
    }
    assert!(w.rig.engine.deck("A").unwrap().net_nudge > 0.0, "the ring bent the deck");
    w.send(&[0x91, 0x08, 0x7F]); // touch
    w.send(&[0x91, 0x08, 0x7F]);
    assert_eq!(w.rig.engine.deck("A").unwrap().net_nudge, 0.0);
}

// JogEventTests.parsesDecksShiftAndKinds: SHIFT's channel (0xB4) is the coarse seek, four times
// the platter seconds of the same ticks on the deck's own channel.
#[test]
fn shift_seeks_four_times_as_far() {
    let mut w = Wheel::new(&["A"]);
    w.send(&[0x91, 0x08, 0x7F]); // touch deck A
    render_frames(&mut w.r, 2048);
    let ticks = 200;
    let t0 = tick(&w.rig, "A");
    for _ in 0..ticks {
        w.send(&[0xB1, 0x0A, 0x01]);
    }
    render_frames(&mut w.r, 48_000); // the clock follows the hand at its own pace
    let plain = tick(&w.rig, "A") - t0;
    let t1 = tick(&w.rig, "A");
    for _ in 0..ticks {
        w.send(&[0xB4, 0x0A, 0x01]); // the same ticks with SHIFT down
    }
    render_frames(&mut w.r, 48_000);
    let shifted = tick(&w.rig, "A") - t1;
    assert!(plain > 8, "{plain}");
    assert!((shifted - 4 * plain).abs() <= 4, "{plain} plain, {shifted} shifted");
}

// JogEventTests.parsesDecksShiftAndKinds / otherDeckIgnored: deck B has its own wheel (channel
// 2, touch 0x92): it scratches B and leaves A where the tempo has it.
#[test]
fn deck_b_has_its_own_wheel() {
    let mut w = Wheel::new(&["A", "B"]);
    let (a0, b0) = (tick(&w.rig, "A"), tick(&w.rig, "B"));
    w.send(&[0x92, 0x08, 0x7F]); // touch deck B
    render_frames(&mut w.r, 2048);
    assert!(scrubbing(&w.rig, "B") && !scrubbing(&w.rig, "A"));
    let held = tick(&w.rig, "B");
    for _ in 0..200 {
        w.send(&[0xB2, 0x0A, 0x7F]); // a tick back
    }
    render_frames(&mut w.r, 12_000);
    let back = tick(&w.rig, "B");
    assert!(back < held - 4, "scratched back: {held} -> {back}");
    assert!(tick(&w.rig, "A") > a0, "deck A played on");
    assert!(tick(&w.rig, "A") > tick(&w.rig, "B"), "A kept the tempo, B went back");
    let _ = b0;
    w.send(&[0x92, 0x08, 0x00]); // let go
    render_frames(&mut w.r, 2048);
    assert!(!scrubbing(&w.rig, "B"));
}

// MARK: DeviceLightsFromTheRouterTests

/// `make`: the app's engine at 22,050 Hz, a net on A, the demo loop on B, the APC attached.
fn lights_rig() -> (Rig, Renderer) {
    let (mut e, mut r) = engine(EngineConfig::new(22_050.0));
    assert!(e.load_demo("B"));
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
    render_frames(&mut r, BLOCK);
    let mut rig = Rig::new(e);
    assert!(rig.attach(APC));
    (rig, r)
}

fn lights(rig: &Rig, name: &str) -> Option<djrust::control::leds::DeviceLights> {
    rig.router.device_lights(name, 0.0, &rig.engine)
}

// DeviceLightsFromTheRouterTests.theAPCsLightsCarryItsRGBGroupAndEveryLED
#[test]
fn the_apcs_lights_carry_its_rgb_group_and_every_led() {
    let (mut rig, mut r) = lights_rig();
    let name = rig.router.profile_name(APC).unwrap();
    let l = lights(&rig, &name).expect("attached");
    assert_eq!(l.rgb_groups.iter().map(String::as_str).collect::<Vec<_>>(), ["pads"]);
    assert!(l.groups["pads"].len() == 64 && l.groups["track"].len() == 8);
    // the macro grid, aimed at deck A (a net): drop and sweep-lp lit dim in their group colours
    let drop: LEDSwatch = l.swatch("pads", 0).expect("pad 0 has an LED");
    let sweep: LEDSwatch = l.swatch("pads", 8).expect("pad 8 has an LED");
    assert!(drop.lit && drop.opacity == 0.5 && sweep.lit, "{drop:?} {sweep:?}");
    assert!(drop.color != sweep.color, "Mute red vs FX cyan");
    // delay-throw: Swift leaves its pad unbound and dark; djrust binds it (docs/AUX.md, PORTING.md
    // "djrust divergence"), lit in FX cyan like its row
    let throw: LEDSwatch = l.swatch("pads", 11).expect("pad 11 has an LED");
    assert!(throw.lit && throw.color == sweep.color, "{throw:?}");
    // a gap in beats' grid: dark
    assert!(!l.swatch("pads", 13).expect("pad 13 has an LED").lit);
    // the aimed deck's play button blinks while it plays, and the map is told
    assert!(!l.any_blinking());
    rig.engine.play("A");
    render_frames(&mut r, 2 * BLOCK);
    let playing = lights(&rig, &name).unwrap();
    assert!(playing.any_blinking(), "the play LED blinks while the aimed deck plays");
}

// DeviceLightsFromTheRouterTests.aControllerThatIsNotAttachedHasNoLights
#[test]
fn a_controller_that_is_not_attached_has_no_lights() {
    let (rig, _r) = lights_rig();
    let mpk = rig.router.profile_name("MPK Mini Mk II").expect("a bundled profile");
    assert!(lights(&rig, &mpk).is_none(), "not attached");
    assert!(lights(&rig, "no such device").is_none());
    let inpulse = rig.router.profile_name(INPULSE).expect("a bundled profile");
    assert!(lights(&rig, &inpulse).is_none(), "profile has LEDs, not attached");
}

// LoopMasterFxTests.theApcAimsAtTheLoopsAndItsFadersAndPadsActOnThem, the pads and lights half.
// SHIFT + scene 4 aims the surface at the loops; a master macro pad fires on the loop channel's
// bus and reads bright in its colour while it runs; a net macro pad is dark and does nothing
// there; scene 8 (panic) clears the loops' macros; aiming A again gives the surface back to the
// deck. (Swift's fader line, "fader 3 = the loops' LP", is left out: djrust's APC fader layout
// is its own, HUB.md M4, and the loops' FX unit through the faders is held by
// `deck_loop_master.rs` and `apc_kill_and_reset_pads_act_on_the_aimed_target`.)
#[test]
fn the_apc_aims_at_the_loops_and_its_pads_act_on_them() {
    let (mut rig, mut r) = lights_rig();
    let name = rig.router.profile_name(APC).unwrap();
    let send = |rig: &mut Rig, bytes: &[u8]| {
        let t = rig.recorded.len() as f64;
        rig.route(APC, bytes, 100.0 + t);
    };
    assert_eq!(rig.engine.focus, "A");
    send(&mut rig, &[0x90, 0x7A, 0x7F]); // SHIFT
    send(&mut rig, &[0x90, 0x73, 0x7F]); // + aim A: aim at the loops
    send(&mut rig, &[0x80, 0x73, 0x00]);
    send(&mut rig, &[0x80, 0x7A, 0x00]);
    assert_eq!(rig.engine.focus, "L");
    // LEDs: the deck aim buttons blink; SHIFT shows the loops' aim lit
    let frames = rig.led_frames(1e12);
    let (_, aim) = frames.iter().find(|(p, _)| p == APC).expect("the APC's frame");
    assert!(
        aim.contains(&[0x90, 0x73, 2]) && aim.contains(&[0x90, 0x74, 2]),
        "the deck aim buttons blink: {aim:?}"
    );
    let shifted = rig
        .router
        .lights(&name, 1e12, Some(true), &rig.engine)
        .expect("attached")
        .1;
    assert_eq!(shifted["scene"][3].level, LightLevel::Full, "SHIFT shows the loops' aim lit");
    // a master macro pad fires on the loops
    send(&mut rig, &[0x90, 8, 0x7F]); // sweep-lp
    assert!(rig.engine.is_loop_master_active(MasterMacro::SweepLp));
    send(&mut rig, &[0x90, 0, 0x7F]); // drop: a net macro, nothing to act on
    render_frames(&mut r, 4 * BLOCK);
    assert_eq!(
        rig.engine.active_loop_master_macros(),
        vec![MasterMacro::SweepLp],
        "the sweep runs on the loops and the drop did not"
    );
    // past the presses' flash (`FLASH_SECONDS`): every pressed pad would still read flashed
    let later = 1e12;
    let l = rig
        .router
        .device_lights(&name, later, &rig.engine)
        .expect("attached");
    let pads = &l.groups["pads"];
    assert_eq!(
        pads[0],
        PadLight::new(0, LightLevel::Dim),
        "drop: dark on the loops"
    );
    assert_eq!(
        pads[8],
        PadLight::new(37, LightLevel::Full),
        "sweep-lp bright cyan while it runs on the loops"
    );
    let frames = rig.led_frames(later);
    let (_, apc) = frames.iter().find(|(p, _)| p == APC).expect("the APC's frame");
    assert!(
        apc.contains(&[0x96, 8, 37]),
        "the LED frame carries pad 8 at full cyan: {apc:?}"
    );
    // scene 8: panic
    send(&mut rig, &[0x90, 0x77, 0x7F]);
    render_frames(&mut r, 4 * BLOCK);
    assert!(rig.engine.active_loop_master_macros().is_empty());
    // aim A again: the surface goes back to the deck
    send(&mut rig, &[0x90, 0x73, 0x7F]);
    assert_eq!(rig.engine.focus, "A");
}
