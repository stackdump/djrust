//! Controllers drive decks: the hardware capture (`captured-messages.json`: every control of
//! an Inpulse 200 MK3, an APC mini mk2 and an MPK Mini mk2, pressed once) replays through
//! `djrust::control`'s router into the real engine, and decks react (`SurfaceNetTests`,
//! `DeckNetTests`).

use djrust::control::controller::{Action, SurfaceHost};
use djrust::json::JsonValue;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer, Rig};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

const PORTS: [(&str, &str); 3] = [
    ("inpulse", "DJControl Inpulse 200 Mk3"),
    ("apc", "APC mini mk2 Control"),
    ("mpk", "MPK Mini Mk II"),
];

fn captured(device: &str) -> Vec<Vec<u8>> {
    let path = format!(
        "{}/tests/fixtures/js/captured-messages.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc.get(device)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.ints().unwrap().into_iter().map(|b| b as u8).collect())
        .collect()
}

fn render(r: &mut Renderer, blocks: usize) -> f64 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut e = 0.0;
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        e += l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    (e / (blocks * BLOCK) as f64).sqrt()
}

fn rig() -> (Rig, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard")));
    e.generate("B", &TrackSpec::new("house", 7.0, Some("standard")));
    e.set_tempo("A", 480.0);
    render(&mut r, 1);
    (Rig::new(e), r)
}

const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";

fn press(rig: &mut Rig, port: &str, group: &str, index: usize, now: f64) {
    assert!(
        rig.router
            .press(port, group, index, false, now, &mut rig.engine),
        "{group}[{index}]"
    );
}

/// The whole capture, routed the way the hub routes live input, into real decks: the
/// crossfader, master, faders and focus move, play/pause toggles, the FX unit is killed and
/// the tempo is nudged; nothing is dropped by the command ring.
#[test]
fn the_hardware_capture_drives_the_decks() {
    let (mut rig, mut r) = rig();
    let mut t = 0.0;
    for (dev, port) in PORTS {
        assert!(rig.attach(port), "{port}");
        for m in captured(dev) {
            t += 0.01;
            rig.route(port, &m, t);
            render(&mut r, 2);
            rig.tick(t);
        }
    }
    let e = &rig.engine;
    assert!(
        e.crossfader > 0.9,
        "the crossfader travelled: {}",
        e.crossfader
    );
    assert!(e.master < 0.9 && e.master > 0.0, "master {}", e.master);
    assert_eq!(e.focus, "B", "the last aim");
    assert_eq!(e.dropped, 0);
    let (a, b) = (e.deck("A").unwrap(), e.deck("B").unwrap());
    assert!(
        a.volume < 0.1 && b.volume < 0.1,
        "volume faders: {} {}",
        a.volume,
        b.volume
    );
    assert!(a.mirror.is_playing(), "A's play button started it");
    assert!(a.pfl, "A is cued");
    assert!(b.fx_killed, "the FX kill pad");
    assert!(b.pitch < -0.9, "B's tempo fader is down: {}", b.pitch);
    assert!(b.net_nudge != 0.0, "and the ring bent it");
    // the fader and the ring multiply (`Deck.effectiveBPM`); the ring does not replace the fader
    let want = b.base_tempo * (1.0 + b.pitch * 0.08) * (1.0 + b.net_nudge);
    assert!(
        (b.mirror.tempo() - want).abs() < 1e-6,
        "{}",
        b.mirror.tempo()
    );
    assert!(
        (b.mirror.tempo() - 113.871).abs() < 0.01,
        "{}",
        b.mirror.tempo()
    );
    // the strip knobs (EQ, filter) land on the decks, and none of them is unhandled
    let moved = |s: djrust::fx::StripSettings| {
        [s.filter, s.eq_low, s.eq_mid, s.eq_high]
            .iter()
            .any(|v| (v - 0.5).abs() > 0.01)
    };
    assert!(
        moved(a.strip) && moved(b.strip),
        "{:?} {:?}",
        a.strip,
        b.strip
    );
    assert!(
        !e.unhandled
            .iter()
            .any(|u| u.contains("Eq") || u.contains("Filter")),
        "{:?}",
        e.unhandled
    );
    // what a deck has no answer for is counted, not lost: the hot cues are handled (a net
    // deck's are ticks), and so are the walk, the live keys (the MPK's keys play the focus
    // deck's live voice) and the auto-mix
    assert!(
        !e.unhandled.iter().any(|u| {
            u.contains("HotCue")
                || u.contains("KeyDown")
                || u.contains("KeyUp")
                || u.contains("AutoMix")
        }),
        "{:?}",
        e.unhandled
    );
}

/// The Inpulse's browser wheel steps the seed of the deck it would move, and touches auto-mix
/// only then: with a playing composed net deck the countdown pauses, with only a sample deck
/// playing (nothing for the wheel to move) it carries on. `SurfaceController.noteAutoMixTouch`
/// `.seedStep` makes the touch on the resolved deck (`if let d`); the port sent a deck-less
/// touch whatever the decks were doing, which paused auto-mix for a wheel that did nothing.
/// Swift has no test of it: the expectations are read off its code.
#[test]
fn the_seed_wheel_pauses_auto_mix_only_when_it_moves_a_deck() {
    const WHEEL: [u8; 3] = [0xB0, 0x01, 0x01]; // browser, clockwise
    fn counting_down(rig: &mut Rig, r: &mut Renderer) {
        rig.engine.set_auto_mix(true, 4, 4);
        rig.engine.play("A");
        for i in 0..8 {
            render(r, 4);
            rig.tick(0.1 * i as f64);
        }
        assert!(!rig.engine.auto_mix.paused, "counting down");
    }
    fn turn(rig: &mut Rig, r: &mut Renderer) {
        assert!(rig.route(INPULSE, &WHEEL, 5.0).is_some());
        render(r, 4);
        rig.tick(5.1);
    }

    // a playing composed net: the wheel steps its seed and the countdown pauses
    let (mut rig, mut r) = rig();
    assert!(rig.attach(INPULSE));
    counting_down(&mut rig, &mut r);
    let seed = rig.engine.deck("A").unwrap().spec.as_ref().unwrap().seed;
    turn(&mut rig, &mut r);
    assert!(rig.engine.deck("A").unwrap().spec.as_ref().unwrap().seed != seed, "the wheel moved A");
    assert!(rig.engine.auto_mix.paused, "a steering touch on the deck it moved");

    // only a sample deck is playing: nothing for the wheel to move, so auto-mix is left alone
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    assert!(e.load_demo("A") && e.load_demo("B"));
    let mut rig = Rig::new(e);
    assert!(rig.attach(INPULSE));
    counting_down(&mut rig, &mut r);
    turn(&mut rig, &mut r);
    assert!(!rig.engine.auto_mix.paused, "the wheel moved nothing: no touch");
    assert!(
        rig.engine.unhandled.iter().any(|u| u.contains("seedStep")),
        "and the miss is counted: {:?}",
        rig.engine.unhandled
    );
}

#[test]
fn role_pads_mute_and_unmute_a_deck() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(INPULSE));
    let kick = rig.engine.roles("A")[0].clone();
    assert_eq!(kick, "kick");
    assert!(rig.engine.is_role_audible("A", "kick"));
    press(&mut rig, INPULSE, "deckA.fxPads", 0, 1.0);
    render(&mut r, 1);
    assert!(
        !rig.engine.is_role_audible("A", "kick"),
        "the pad muted the kick"
    );
    assert!(rig.engine.is_role_audible("A", "hihat") || rig.engine.is_role_audible("A", "snare"));
    press(&mut rig, INPULSE, "deckA.fxPads", 0, 2.0);
    render(&mut r, 1);
    assert!(
        rig.engine.is_role_audible("A", "kick"),
        "and again brought it back"
    );
}

#[test]
fn the_play_button_toggles_the_transport() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(INPULSE));
    assert!(!rig.engine.deck("A").unwrap().mirror.is_playing());
    press(&mut rig, INPULSE, "deckA.buttons", 4, 1.0);
    render(&mut r, 4);
    assert!(rig.engine.deck("A").unwrap().mirror.is_playing());
    assert!(!rig.engine.deck("B").unwrap().mirror.is_playing());
    press(&mut rig, INPULSE, "deckA.buttons", 4, 2.0);
    render(&mut r, 4);
    assert!(!rig.engine.deck("A").unwrap().mirror.is_playing());
}

#[test]
fn an_apc_macro_pad_fires_on_the_aimed_deck() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(APC));
    rig.engine.play("A");
    render(&mut r, 8);
    assert_eq!(SurfaceHost::focus(&rig.engine), "A");
    // pad 9 is sweep-hp: a master-chain macro, on the aimed deck's chain
    press(&mut rig, APC, "pads", 9, 1.0);
    render(&mut r, 40);
    assert!(rig
        .engine
        .deck("A")
        .unwrap()
        .mirror
        .is_master_active(djrust::fx::MasterMacro::SweepHp));
    assert!(!rig
        .engine
        .deck("B")
        .unwrap()
        .mirror
        .is_master_active(djrust::fx::MasterMacro::SweepHp));
    // pad 0 is drop: a net macro, which holds nets muted for its length
    press(&mut rig, APC, "pads", 0, 2.0);
    render(&mut r, 8);
    assert!(rig
        .engine
        .deck("A")
        .unwrap()
        .mirror
        .macro_active
        .load(std::sync::atomic::Ordering::Relaxed));
    // panic (scene 8) restores both
    press(&mut rig, APC, "scene", 7, 3.0);
    render(&mut r, 60);
    assert!(!rig
        .engine
        .deck("A")
        .unwrap()
        .mirror
        .is_master_active(djrust::fx::MasterMacro::SweepHp));
    assert_eq!(rig.engine.dropped, 0);
}

/// The Inpulse's NEXT button is `.nextTrack`: `Deck.skipToNextTrack`, a hand-over to the track
/// composed ahead (`advanceToNext`), and nothing while none is ready. (It was wired to
/// regenerate, which composes `spec.nextVersion` on the spot.)
#[test]
fn next_track_hands_over_to_the_one_queued_ahead() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(INPULSE));
    let before = rig.engine.deck("A").unwrap().spec.clone().unwrap();
    press(&mut rig, INPULSE, "deckA.buttons", 8, 1.0);
    render(&mut r, 2);
    assert_eq!(
        rig.engine.deck("A").unwrap().spec.as_ref(),
        Some(&before),
        "nothing is ready: the button does nothing"
    );
    assert!(rig.engine.unhandled.is_empty());
    rig.engine.queue_next("A", &before.next_version());
    press(&mut rig, INPULSE, "deckA.buttons", 8, 2.0);
    render(&mut r, 2);
    let after = rig.engine.deck("A").unwrap().spec.clone().unwrap();
    assert_eq!(after, before.next_version());
    assert_eq!(after.genre, before.genre);
    assert!(after.seed != before.seed);
    assert_eq!(rig.engine.deck("A").unwrap().track.advances, 1);
    // a stinger pad fires without a hitch
    press(&mut rig, INPULSE, "deckA.samplerPads", 0, 3.0);
    render(&mut r, 4);
    assert_eq!(rig.engine.dropped, 0);
}

#[test]
fn leds_follow_the_decks() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(APC));
    render(&mut r, 2);
    let first = rig.led_frames(0.0);
    assert!(
        first.iter().any(|(_, f)| !f.is_empty()),
        "the first frame lights the surface"
    );
    let still = rig.led_frames(0.01);
    assert!(
        still.iter().all(|(_, f)| f.is_empty()),
        "an unchanged surface sends nothing"
    );
    // muting a role changes a light
    rig.engine.set_role_muted("A", "kick", true);
    render(&mut r, 1);
    let changed = rig.led_frames(0.02);
    assert!(
        changed.iter().any(|(_, f)| !f.is_empty()),
        "the kick's pad went dark"
    );
    let _ = Action::Focus { deck: "A".into() };
}

/// The strip's DJ filter is in the signal path: swept to its low-pass end, the deck loses
/// most of its energy; back at the centre detent it is bypassed again.
#[test]
fn the_strip_filter_shapes_the_deck() {
    use djrust::fx::StripKnob;
    let render_twin = |knob: Option<(StripKnob, f64)>| {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (mut e, mut r) = engine(c);
        e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard")));
        e.set_crossfader(0.0);
        if let Some((k, v)) = knob {
            assert!(e.set_strip("A", k, v));
        }
        e.play("A");
        // the RMS of the first difference: the highs, mostly
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let (mut acc, mut prev) = (0.0, 0f32);
        for _ in 0..400 {
            r.render_block(&mut l, &mut rr);
            for &v in &l {
                acc += ((v - prev) as f64).powi(2);
                prev = v;
            }
        }
        (acc / (400 * BLOCK) as f64).sqrt()
    };
    let flat = render_twin(None);
    let centre = render_twin(Some((StripKnob::Filter, 0.5)));
    let dark = render_twin(Some((StripKnob::Filter, 0.0)));
    let thin = render_twin(Some((StripKnob::EqHigh, 0.0)));
    assert!(flat > 0.01, "the deck plays: {flat}");
    assert_eq!(flat, centre, "the centre detent is a bypass");
    assert!(dark < flat * 0.2, "low-pass at 200 Hz: {dark} vs {flat}");
    assert!(thin < flat * 0.5, "the high kill: {thin} vs {flat}");
}

/// The Inpulse's platter scratches a net deck (`Deck`'s `JogClock`): touch freezes the clock,
/// turning back undoes ticks, turning forward plays them, and letting go hands the clock back
/// to the tempo. The outer ring (bend) only nudges.
#[test]
fn the_jog_wheel_scratches_the_net_clock() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(INPULSE));
    rig.engine.set_tempo("A", 120.0);
    rig.engine.play("A");
    render(&mut r, 400); // ~4 s: some 30 ticks in
    let before = rig.engine.deck("A").unwrap().mirror.tick();
    assert!(before > 20, "{before}");
    let mut t = 10.0;
    let mut step = |rig: &mut Rig, r: &mut Renderer, bytes: &[u8]| {
        t += 0.002;
        rig.route(INPULSE, bytes, t);
        rig.tick(t);
        render(r, 1);
    };
    step(&mut rig, &mut r, &[0x91, 8, 127]); // touch
    for _ in 0..300 {
        step(&mut rig, &mut r, &[0xB1, 10, 0x7F]); // one tick backwards
    }
    render(&mut r, 100); // let the platter catch up with the hand
    let back = rig.engine.deck("A").unwrap().mirror.tick();
    assert!(back < before - 8, "scratched back: {before} -> {back}");
    for _ in 0..150 {
        step(&mut rig, &mut r, &[0xB1, 10, 0x01]); // forwards
    }
    render(&mut r, 100);
    let fwd = rig.engine.deck("A").unwrap().mirror.tick();
    assert!(fwd > back + 4, "scratched forward: {back} -> {fwd}");
    // held still: the clock stays put
    render(&mut r, 200);
    assert_eq!(rig.engine.deck("A").unwrap().mirror.tick(), fwd);
    step(&mut rig, &mut r, &[0x91, 8, 0]); // let go
    render(&mut r, 200);
    assert!(
        rig.engine.deck("A").unwrap().mirror.tick() > fwd + 10,
        "plays on"
    );
    assert_eq!(
        rig.engine.deck("A").unwrap().pitch,
        0.0,
        "the platter is not the pitch"
    );
}

/// Unplugged and plugged back in, the APC gets its whole LED frame again (it comes up dark).
#[test]
fn a_replugged_surface_is_repainted() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(APC));
    render(&mut r, 2);
    let full = rig.led_frames(0.0);
    let n = full.iter().map(|(_, f)| f.len()).sum::<usize>();
    assert!(n >= 80, "every pad and button: {n}");
    assert!(rig.led_frames(0.01).iter().all(|(_, f)| f.is_empty()));
    assert!(rig.detach(APC));
    assert!(rig.attach(APC));
    let again = rig.led_frames(0.02);
    assert_eq!(again.iter().map(|(_, f)| f.len()).sum::<usize>(), n);
}

/// The APC's faders drive the mixer, the loop channel included.
#[test]
fn apc_faders_move_the_mixer() {
    let (mut rig, mut r) = rig();
    assert!(rig.attach(APC));
    for (cc, v) in [
        (0x30u8, 127u8),
        (0x31, 0),
        (0x32, 32),
        (0x35, 0),
        (0x36, 127),
    ] {
        rig.route(APC, &[0xB0, cc, v], 0.0);
    }
    render(&mut r, 1);
    let e = &rig.engine;
    assert_eq!(e.deck("A").unwrap().volume, 1.0);
    assert_eq!(e.deck("B").unwrap().volume, 0.0);
    assert!((e.loops.volume - 32.0 / 127.0).abs() < 1e-9);
    assert_eq!(e.loop_channel.strip.filter, 0.0);
    assert_eq!(e.crossfader, 1.0);
    assert!(e.unhandled.is_empty(), "{:?}", e.unhandled);
}
