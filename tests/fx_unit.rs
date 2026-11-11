//! The FX unit (mirrors the Swift `FxUnitLayerTests`, the settings half of `FxUnitCodecTests`,
//! and the combination rule): a move glides to its target, macros ride on top of the unit's
//! setting, a quantised reset lands on its frame or under a hand, a drag is coalesced to 30 Hz.
//! The bus / deck / replay suites need the audio graph and are the host's.

use djrust::fx::unit::{FxParam, FxUnitCoalescer, FxUnitCommand, FxUnitLayer, FxUnitSettings};
use djrust::fx::{FxAutomation, MasterFxKey};
use djrust::json::JsonValue;

const SR: f64 = 48_000.0;

fn set(u: &mut FxUnitLayer, p: FxParam, v: f64, snap: bool) {
    u.take(FxUnitCommand::Set {
        param: p.index() as u8,
        value: v,
        snap,
    });
}

#[test]
fn a_move_glides_to_its_target() {
    let a = FxAutomation::new(120.0, 120.0);
    let mut u = FxUnitLayer::new(SR);
    set(&mut u, FxParam::Hp, 0.8, false);
    let mut emitted: Vec<f64> = vec![];
    let mut frame = 0;
    let mut block = |u: &mut FxUnitLayer| {
        u.begin_block(512, frame, &a, &mut |k, v| {
            if k == MasterFxKey::HpFreq {
                emitted.push(v);
            }
        });
        frame += 512;
    };
    block(&mut u);
    assert!(
        u.value(FxParam::Hp) > 0.2 && u.value(FxParam::Hp) < 0.6,
        "one block (10.7 ms): part of the way"
    );
    for _ in 0..60 {
        block(&mut u);
    }
    assert_eq!(u.value(FxParam::Hp), 0.8, "settled exactly on the target");
    assert!(u.settled());
    assert!(emitted.len() > 5 && emitted.len() < 62);
    assert!(
        emitted.windows(2).all(|w| w[0] < w[1]),
        "rises, never jumps back"
    );
    assert_eq!(
        *emitted.last().unwrap(),
        80.0,
        "the chain is asked for slider 80"
    );
    // nothing more once it is there
    let mut more = 0;
    u.begin_block(512, frame, &a, &mut |_, _| more += 1);
    assert_eq!(more, 0);
}

/// The combination rule: at neutral every macro dispatch passes through exactly; with the unit
/// set, HP the higher, LP the lower, crush / drive / mix the larger, pitch adds.
#[test]
fn macros_ride_on_the_units_setting() {
    let a = FxAutomation::new(120.0, 120.0);
    let mut u = FxUnitLayer::new(SR);
    for k in MasterFxKey::ALL {
        let mut v = k.min();
        while v <= k.max() {
            assert_eq!(u.automated(k, v, &a), v as f64, "{k:?} {v} at neutral");
            v += 7;
        }
    }
    for (p, v) in [
        (FxParam::Hp, 0.3),
        (FxParam::Lp, 0.6),
        (FxParam::Pitch, 3.0),
        (FxParam::Crush, 0.5),
        (FxParam::PhaserDepth, 0.9),
    ] {
        set(&mut u, p, v, true);
    }
    assert_eq!(u.automated(MasterFxKey::HpFreq, 10, &a), 30.0);
    assert_eq!(u.automated(MasterFxKey::HpFreq, 80, &a), 80.0);
    assert_eq!(u.automated(MasterFxKey::LpFreq, 100, &a), 60.0);
    assert_eq!(u.automated(MasterFxKey::LpFreq, 5, &a), 5.0);
    assert_eq!(u.automated(MasterFxKey::MasterPitch, 12, &a), 15.0);
    assert_eq!(u.automated(MasterFxKey::MasterPitch, -12, &a), -9.0);
    assert_eq!(u.automated(MasterFxKey::CrushBits, 20, &a), 50.0);
    // depth: the unit's while no macro moves it, the macro's when it does
    assert_eq!(u.automated(MasterFxKey::PhaserDepth, 50, &a), 90.0);
    assert_eq!(u.automated(MasterFxKey::PhaserDepth, 100, &a), 100.0);
    // the bipolar filter: left is the LP, right the HP, against the knobs' own
    set(&mut u, FxParam::Filter, -0.7, true);
    assert!(
        (u.automated(MasterFxKey::LpFreq, 100, &a) - 30.0).abs() < 1e-9,
        "the lower of 30 and 60"
    );
    set(&mut u, FxParam::Filter, 0.5, true);
    assert_eq!(
        u.automated(MasterFxKey::HpFreq, 0, &a),
        50.0,
        "the higher of 50 and 30"
    );
    // killed: every macro passes through again
    u.take(FxUnitCommand::Kill { on: true, frame: 0 });
    for _ in 0..81 {
        u.begin_block(512, 0, &a, &mut |_, _| {});
    }
    assert_eq!(u.automated(MasterFxKey::HpFreq, 10, &a), 10.0);
    assert_eq!(u.automated(MasterFxKey::MasterPitch, 12, &a), 12.0);
}

/// A quantised reset waits for its frame; a knob moved meanwhile lands it at once.
#[test]
fn a_quantised_reset_lands_on_its_frame_or_under_a_hand() {
    let a = FxAutomation::new(120.0, 120.0);
    let mut u = FxUnitLayer::new(SR);
    set(&mut u, FxParam::Drive, 0.7, true);
    u.take(FxUnitCommand::Reset { frame: 10_000 });
    u.begin_block(512, 9_000, &a, &mut |_, _| {});
    assert_eq!(
        u.value(FxParam::Drive),
        0.7,
        "not yet: the block ends before frame 10 000"
    );
    u.begin_block(512, 9_600, &a, &mut |_, _| {});
    assert!(u.value(FxParam::Drive) < 0.7, "the block holding it");
    set(&mut u, FxParam::Crush, 0.4, true);
    u.take(FxUnitCommand::Reset { frame: 50_000 });
    set(&mut u, FxParam::Pitch, 2.0, true);
    assert_eq!(u.value(FxParam::Crush), 0.4);
    u.begin_block(512, 10_100, &a, &mut |_, _| {});
    assert!(
        u.value(FxParam::Crush) < 0.4,
        "the hand landed the waiting reset"
    );
    assert_eq!(u.value(FxParam::Pitch), 2.0, "and its own move stands");
}

/// Dragging is recorded at most 30 times a second, the latest value winning, the last one sent.
#[test]
fn a_drag_is_coalesced_to_30hz() {
    let mut c = FxUnitCoalescer::default();
    let mut sent: Vec<f64> = vec![];
    let mut t = 0.0;
    for i in 0..200 {
        // 2 s of mouse events at 100 Hz
        let v = i as f64 / 200.0;
        if c.offer("A|filter", v, t) {
            sent.push(v);
        }
        sent.extend(c.due(t).into_iter().map(|x| x.1));
        t += 0.01;
    }
    if let Some(last) = c.finish("A|filter") {
        sent.push(last);
    }
    assert!(sent.len() <= 2 * 30 + 2, "{} actions for 2 s", sent.len());
    assert!(sent.len() >= 40);
    assert_eq!(
        *sent.last().unwrap(),
        199.0 / 200.0,
        "the last value is the one that lands"
    );
    assert!(c.offer("A|filter", 0.5, t), "a new drag sends at once");
}

#[test]
fn settings_round_trip_and_clamp() {
    let mut s = FxUnitSettings::new();
    assert_eq!(
        s.json(),
        JsonValue::parse("{}").unwrap(),
        "a neutral unit keeps old sets' canonical text"
    );
    s.set(FxParam::Filter, 0.25);
    s.set(FxParam::Pitch, -3.0);
    s.killed = true;
    let back =
        FxUnitSettings::from_json(Some(&JsonValue::parse(&s.json().js_stringified()).unwrap()));
    assert_eq!(back, s);
    assert_eq!(back.moved(), vec![FxParam::Filter, FxParam::Pitch]);
    assert_eq!(
        s.json().js_stringified(),
        r#"{"filter":0.25,"pitch":-3,"killed":true}"#
    );
    let mut clamped = FxUnitSettings::new();
    clamped.set(FxParam::Pitch, 40.0);
    clamped.set(FxParam::Hp, f64::NAN);
    assert!(clamped.get(FxParam::Pitch) == 12.0 && clamped.get(FxParam::Hp) == 0.0);
    assert!(!s.is_active() && !s.is_neutral());
}

#[test]
fn readouts_say_what_a_knob_does() {
    assert_eq!(FxParam::Filter.readout(0.0), "off");
    assert_eq!(FxParam::Lp.readout(1.0), "off");
    assert_eq!(FxParam::Pitch.readout(3.0), "+3.0 st");
    assert_eq!(FxParam::Pitch.readout(0.0), "0 st");
    assert_eq!(FxParam::Crush.readout(0.8), "4.0 bit");
    assert_eq!(FxParam::PhaserDepth.readout(0.5), "3.0 oct");
    assert_eq!(FxParam::Room.readout(1.0), "+12.0 dB");
    assert_eq!(
        FxParam::Hp.readout(0.3),
        format!("{:.0} Hz", 20.0 * 250f64.powf(0.3))
    );
    assert!(FxParam::Echo.is_send() && !FxParam::Hp.is_send());
    for p in FxParam::ALL {
        assert_eq!(FxParam::from_id(p.id()), Some(p));
    }
}
