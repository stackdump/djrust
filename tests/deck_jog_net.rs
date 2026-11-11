//! The Inpulse 200 MK3's jog wheels on a net deck, from the profile's own bytes through a `Rig`
//! into the engine: platter seconds to ticks of the net clock at the deck's tempo, the outer
//! ring's bend, SHIFT's coarse seek, deck B. Ports `WaveLiveTests.DeckScratchTests
//! .jogClockDrivesTheNetClockOnANetDeck` (:462-484) and `JogAdapterTests` (bend settles at depth
//! at platter speed, a touch clears the bend: :42-78), which Swift runs against a mock clock or a
//! deck directly; here the wheel is the controller's bytes.
//!
//! `Deck.scrubBy` turns platter seconds into ticks at `effectiveBPM ?? 120` (DJEngine.swift:838:
//! `wave.scrub(ticks: seconds * (effectiveBPM ?? 120) * 4 / 60)`), the tempo the deck is set to
//! run at (base x tempo fader x nudge), whatever a tempo macro does to the runner's tempo.

use djrust::fx::MasterMacro;
use djrust::json::JsonValue;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer, Rig};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const INPULSE: &str = "DJControl Inpulse 200 Mk3";

/// Seconds of 1x platter one wheel tick is (`JogProfile.secondsPerTick`: 60 / 33.33 rpm / 248 ticks
/// a turn): the profile's own numbers.
const SPT: f64 = 60.0 / (100.0 / 3.0) / 248.0;
/// Wheel ticks a second at platter speed (`ticksPerRev * rpm / 60`).
const PLATTER_RATE: f64 = 248.0 * (100.0 / 3.0) / 60.0;

fn spec(genre: &str, seed: f64, bpm: f64) -> TrackSpec {
    let mut s = TrackSpec::new(genre, seed, None);
    s.extra.set("bpm", JsonValue::Number(bpm));
    s
}

/// A techno loop on A at 120 and a house loop on B at 100, nothing playing yet.
struct Hand {
    rig: Rig,
    r: Renderer,
    t: f64,
}

impl Hand {
    fn new() -> Hand {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (mut e, r) = engine(c);
        assert!(e.generate("A", &spec("techno", 1.0, 120.0)));
        assert!(e.generate("B", &spec("house", 7.0, 100.0)));
        let mut rig = Rig::new(e);
        assert!(rig.attach(INPULSE));
        Hand { rig, r, t: 10.0 }
    }

    fn render(&mut self, frames: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..frames.div_ceil(BLOCK) {
            self.r.render_block(&mut l, &mut rr);
        }
    }

    /// One message from the controller, 2 ms of wall time on, a block rendered.
    fn send(&mut self, bytes: &[u8]) {
        self.t += 0.002;
        self.rig.route(INPULSE, bytes, self.t);
        self.rig.tick(self.t);
        self.render(BLOCK);
    }

    fn tick_of(&self, deck: &str) -> i64 {
        self.rig.engine.deck(deck).unwrap().mirror.tick()
    }

    fn scrubbing(&self, deck: &str) -> bool {
        self.r.deck(deck).unwrap().runner.is_scrubbing()
    }

    /// Where the hand has turned the net clock's platter to, in ticks.
    fn target(&self, deck: &str) -> f64 {
        self.r.deck(deck).unwrap().runner.scrub_target()
    }
}

/// `n` ticks of the wheel as platter seconds.
fn secs(n: i64) -> f64 {
    n as f64 * SPT
}

/// WaveLiveTests.jogClockDrivesTheNetClockOnANetDeck: one platter second at 120 bpm is 2 beats,
/// 8 ticks; the other way undoes them; letting go plays on. The platter is the Inpulse's: a touch
/// is note 8 on the deck's channel, a tick of the wheel is CC 10 with 0x01 / 0x7F.
#[test]
fn a_platter_second_is_eight_ticks_of_the_net_clock_at_120_bpm() {
    let mut h = Hand::new();
    assert_eq!(h.rig.engine.deck("A").unwrap().base_tempo, 120.0);
    h.rig.engine.play("A");
    h.render(24_000);
    let t0 = h.tick_of("A");
    assert!(t0 > 4, "the loop plays: {t0}");
    h.send(&[0x91, 8, 127]); // touch
    assert!(h.scrubbing("A"));
    let start = h.target("A");
    let n = 138; // 138 wheel ticks: 1.0016 platter seconds
    for _ in 0..n {
        h.send(&[0xB1, 10, 0x01]);
    }
    // the hand's ticks: seconds x bpm x 4 / 60 (the profile's seconds per tick, the deck's tempo)
    let want = secs(n) * 120.0 * 4.0 / 60.0;
    assert!((8.0..8.1).contains(&want), "{want}");
    assert!((h.target("A") - start - want).abs() < 1e-9, "{}", h.target("A") - start);
    h.render(12_000);
    assert!(h.scrubbing("A"));
    let there = h.tick_of("A");
    assert!((there - (t0 + 8)).abs() <= 1, "tick {there} vs {}", t0 + 8);
    // the wheel the other way: back to where it started
    for _ in 0..n {
        h.send(&[0xB1, 10, 0x7F]);
    }
    h.render(12_000);
    assert!((h.target("A") - start).abs() < 1e-9);
    assert!((h.tick_of("A") - t0).abs() <= 1, "back at {}", h.tick_of("A"));
    // let go: the tempo clock again, and it plays on
    h.send(&[0x91, 8, 0]);
    h.render(24_000);
    assert!(!h.scrubbing("A"));
    assert!(h.tick_of("A") > t0 + 3, "plays on: {} from {t0}", h.tick_of("A"));
}

/// The tempo the platter is scaled by is the deck's own setting: the tempo fader moves it (base x
/// (1 + pitch x 8 %)), and it is read when the wheel turns, not from what the runner last
/// published: a fader moved a moment ago has not reached a block yet.
#[test]
fn the_tempo_fader_scales_the_platter() {
    let mut h = Hand::new();
    h.rig.engine.play("A");
    h.render(24_000);
    h.send(&[0x91, 8, 127]);
    let start = h.target("A");
    // the fader moved, no block rendered since: the runner still runs at 120
    h.rig.engine.set_pitch("A", 0.5);
    assert_eq!(h.rig.engine.deck("A").unwrap().mirror.tempo(), 120.0);
    h.rig.engine.jog_scrub("A", 1.0);
    h.render(BLOCK);
    let bpm = 120.0 * (1.0 + 0.5 * 0.08);
    assert!(
        (h.target("A") - start - bpm * 4.0 / 60.0).abs() < 1e-9,
        "{} ticks for a platter second at {bpm} bpm",
        h.target("A") - start
    );
    // and at rest, a block later, the same
    let start = h.target("A");
    h.rig.engine.jog_scrub("A", 1.0);
    h.render(BLOCK);
    assert!((h.target("A") - start - bpm * 4.0 / 60.0).abs() < 1e-9);
}

/// A tempo macro (the Feel puck's tempo axis, half-time, tape-stop) moves the runner's tempo and
/// not the deck's setting: the hand keeps scaling by the setting, the nominal tempo.
#[test]
fn a_tempo_macro_under_the_hand_does_not_change_the_platters_scale() {
    let mut h = Hand::new();
    h.rig.engine.play("A");
    h.render(24_000);
    h.send(&[0x91, 8, 127]);
    h.rig
        .engine
        .fire_master("A", MasterMacro::FeelChill, Some(2));
    let mut moved = false;
    for _ in 0..400 {
        h.render(BLOCK);
        let tempo = h.rig.engine.deck("A").unwrap().mirror.tempo();
        if (tempo - 120.0).abs() > 5.0 {
            moved = true;
            break;
        }
    }
    assert!(moved, "the macro moved the runner's tempo off 120");
    assert_eq!(h.rig.engine.deck("A").unwrap().effective_bpm(), Some(120.0));
    let start = h.target("A");
    for _ in 0..10 {
        h.send(&[0xB1, 10, 0x01]);
    }
    assert!(
        (h.target("A") - start - secs(10) * 120.0 * 4.0 / 60.0).abs() < 1e-9,
        "{} ticks for {} platter seconds",
        h.target("A") - start,
        secs(10)
    );
}

/// Before the first block is published the runner's tempo reads 0 (the mirror is empty): a wheel
/// turned that early still moves the clock by the deck's tempo.
#[test]
fn a_wheel_turned_before_the_first_block_scrubs_at_the_decks_tempo() {
    let mut h = Hand::new();
    h.rig.engine.jog_touch("A", true);
    h.rig.engine.jog_scrub("A", 1.0);
    h.render(BLOCK);
    assert!(h.scrubbing("A"));
    let want = 120.0 * 4.0 / 60.0;
    assert!(
        (h.target("A") - want).abs() < 1e-9,
        "the clock was at 0: {} ticks, not {want}",
        h.target("A")
    );
}

/// JogAdapterTests.bendSettlesAtDepthAtPlatterSpeed, from the ring's bytes (CC 9) at platter
/// speed for 2 s: the deck's nudge settles at the bend depth, 10 %, on top of the tempo fader
/// (the net clock runs at base x fader x (1 + nudge)); and untouchedScratchBendsAndDecays: left
/// alone the bend dies away and the tempo is the deck's again.
#[test]
fn the_ring_bends_the_tempo_to_a_tenth_at_platter_speed_and_lets_go() {
    let mut h = Hand::new();
    h.rig.engine.play("A");
    h.render(24_000);
    h.rig.tick(h.t); // the adapter's time base
    let mut now = h.t;
    while now < h.t + 2.0 {
        now += 1.0 / PLATTER_RATE;
        h.rig.route(INPULSE, &[0xB1, 9, 0x01], now);
        h.rig.tick(now);
        h.render(BLOCK);
    }
    let nudge = h.rig.engine.deck("A").unwrap().net_nudge;
    assert!((nudge - 0.10).abs() < 0.01, "settles at depth: {nudge}");
    // the net clock follows: base x (1 + nudge)
    let tempo = h.rig.engine.deck("A").unwrap().mirror.tempo();
    assert!((tempo - 120.0 * (1.0 + nudge)).abs() < 1e-6, "{tempo}");
    // the ring stops: a time constant later the bend is a part of itself, much later it is gone
    now += 0.15;
    h.rig.tick(now);
    let after = h.rig.engine.deck("A").unwrap().net_nudge;
    assert!(after > 0.0 && after < nudge, "{after} < {nudge}");
    now += 10.0;
    h.rig.tick(now);
    h.render(BLOCK);
    assert_eq!(h.rig.engine.deck("A").unwrap().net_nudge, 0.0);
    assert_eq!(h.rig.engine.deck("A").unwrap().mirror.tempo(), 120.0);
}

/// JogAdapterTests.touchClearsNudge: a hand on the platter takes the bend away.
#[test]
fn a_touch_clears_the_bend() {
    let mut h = Hand::new();
    h.rig.engine.play("A");
    h.render(BLOCK);
    for _ in 0..10 {
        h.rig.route(INPULSE, &[0xB1, 9, 0x01], h.t);
    }
    h.rig.tick(h.t);
    assert!(h.rig.engine.deck("A").unwrap().net_nudge > 0.0, "bent");
    h.send(&[0x91, 8, 127]);
    assert_eq!(h.rig.engine.deck("A").unwrap().net_nudge, 0.0);
    assert!(h.scrubbing("A"));
}

/// SHIFT + the platter seeks four times as far (`JogAdapter`'s shift multiplier, the profile's
/// shifted channel: deck A's channel + 3): the same wheel ticks, four times the ticks of the net
/// clock.
#[test]
fn shift_seeks_four_times_as_far() {
    let mut h = Hand::new();
    h.rig.engine.play("A");
    h.render(24_000);
    h.send(&[0x91, 8, 127]);
    let a = h.target("A");
    for _ in 0..10 {
        h.send(&[0xB1, 10, 0x01]);
    }
    let plain = h.target("A") - a;
    assert!((plain - secs(10) * 8.0).abs() < 1e-9, "{plain}");
    let b = h.target("A");
    for _ in 0..10 {
        h.send(&[0xB4, 10, 0x01]); // the shifted channel
    }
    let shifted = h.target("A") - b;
    assert!((shifted - 4.0 * plain).abs() < 1e-9, "{shifted} vs 4 x {plain}");
    // and backwards
    let c = h.target("A");
    for _ in 0..10 {
        h.send(&[0xB4, 10, 0x7F]);
    }
    assert!((c - h.target("A") - 4.0 * plain).abs() < 1e-9);
}

/// Deck B has its own platter (channel 2, shifted 5), at its own tempo: its hand moves B's net
/// clock by B's ticks and leaves A alone.
#[test]
fn deck_b_has_its_own_platter_and_tempo() {
    let mut h = Hand::new();
    assert_eq!(h.rig.engine.deck("B").unwrap().base_tempo, 100.0);
    h.rig.engine.play("A");
    h.rig.engine.play("B");
    h.render(24_000);
    let a_before = h.target("A");
    h.send(&[0x92, 8, 127]); // B's touch
    assert!(h.scrubbing("B") && !h.scrubbing("A"));
    let b = h.target("B");
    for _ in 0..10 {
        h.send(&[0xB2, 10, 0x01]);
    }
    assert!((h.target("B") - b - secs(10) * 100.0 * 4.0 / 60.0).abs() < 1e-9);
    let b = h.target("B");
    for _ in 0..10 {
        h.send(&[0xB5, 10, 0x01]); // B's SHIFT
    }
    assert!((h.target("B") - b - 4.0 * secs(10) * 100.0 * 4.0 / 60.0).abs() < 1e-9);
    // A was never touched: no hand, no scrub
    assert!(!h.scrubbing("A"));
    assert_eq!(h.target("A"), a_before);
    // and B's ring is B's bend
    h.send(&[0x92, 8, 0]);
    for _ in 0..30 {
        h.rig.route(INPULSE, &[0xB2, 9, 0x01], h.t);
    }
    h.rig.tick(h.t + 0.001);
    assert!(h.rig.engine.deck("B").unwrap().net_nudge > 0.0);
    assert_eq!(h.rig.engine.deck("A").unwrap().net_nudge, 0.0);
    assert!(!h.scrubbing("B"), "released");
}

/// The same platter on a sample deck (the turntable): a touch holds the record, the wheel's ticks
/// move the playhead by their platter seconds (`.scrub(samples: seconds * sampleRate)`, no
/// tempo in it), and letting go hands it back to the motor.
#[test]
fn the_same_wheel_scratches_a_sample_decks_record_by_its_seconds() {
    let mut h = Hand::new();
    h.rig.engine.load_demo("A");
    h.rig.engine.play("A");
    h.render(24_000);
    let pos = |h: &Hand| h.rig.engine.deck("A").unwrap().mirror.turntable_position();
    h.send(&[0x91, 8, 127]);
    h.render(12_000); // (the record settles where the hand took it)
    let held = pos(&h);
    h.render(12_000);
    assert!((pos(&h) - held).abs() < 1.0, "a hand on the record: it stands still");
    for _ in 0..138 {
        h.send(&[0xB1, 10, 0x01]);
    }
    h.render(12_000);
    let moved = pos(&h) - held;
    let want = secs(138) * SR;
    assert!(
        (moved - want).abs() < 0.01 * want,
        "{moved} samples for {} platter seconds ({want})",
        secs(138)
    );
    h.send(&[0x91, 8, 0]);
    let at = pos(&h);
    h.render(12_000);
    assert!(pos(&h) > at + 5_000.0, "the motor plays on: {at} -> {}", pos(&h));
}
