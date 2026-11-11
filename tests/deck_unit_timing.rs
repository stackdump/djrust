//! The FX unit's `automated()` is evaluated at each automation event, as `WaveDeckSource.handle`
//! does inline inside `fx.advance` (WaveDeckSource.swift:591-593, 725-733; `FxUnit.automated`
//! reads `automation.isAnimating(key)` at that instant): a macro's slider dispatch earlier in a
//! block sees its animation as still running, even when the animation ends later in the same
//! block. Only PhaserDepth / PhaserFreq read it (`animating || s != rest ? s : u`).
//!
//! No Swift test or golden holds this (FxUnitTests has only an idle automation): the literals
//! are the combination rule's own and the events of `phaser-drone`.

use std::sync::Arc;

use djrust::fx::unit::{FxParam, FxUnitCommand};
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::pflow::{NetBundle, Project};
use djrust::wave::{Master, WaveOptions, WaveRunner};
use djrust::host::deck::{Command, DeckCore, DeckMirror};

const SR: f64 = 22_050.0;
const BLOCK: usize = 1024;

fn silent_project() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(16);
    n.track.channel = 4;
    n.track.group = Some("melody".into());
    p.nets.insert("melody", n);
    p
}

/// A deck playing a silent net at `bpm`, the FX unit's phaser depth knob at 0.9, `phaser-drone`
/// fired at transport 0; every PhaserDepth dispatch of the next `seconds`: (transport second
/// of its block, what the chain was asked for).
fn depth_ops(bpm: f64, seconds: f64) -> Vec<(f64, f64)> {
    let opts = WaveOptions {
        master: Master::Linear,
        ..Default::default()
    };
    let mut core = DeckCore::new("A", SR, opts.clone(), false, Arc::new(DeckMirror::new()));
    core.apply(Command::Load(WaveRunner::prepare(
        &silent_project(),
        SR,
        &opts,
        0,
    )));
    core.apply(Command::Tempo(bpm));
    core.apply(Command::FxUnit(FxUnitCommand::Set {
        param: FxParam::PhaserDepth.index() as u8,
        value: 0.9,
        snap: true,
    }));
    core.apply(Command::Play);
    core.apply(Command::MasterMacro {
        m: MasterMacro::PhaserDrone,
        bars: -1,
    });
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::new();
    let mut frame = 0i64;
    while (frame as f64) < seconds * SR {
        core.render(&mut l, &mut r, frame);
        for (key, v, _) in core.last_block_ops() {
            if key == MasterFxKey::PhaserDepth {
                out.push((frame as f64 / SR, v));
            }
        }
        frame += BLOCK as i64;
    }
    out
}

/// At 60.6 bpm `phaser-drone`'s depth hold has a step on 50 (the key's rest value) at 7.92 s with
/// the animation still running, and ends 0.8 ms later, in the same block. In Swift the step is
/// dispatched while it animates, so the slider's own 50 stands over the unit's 90 (`animating ||
/// s != rest ? s : u`); the end, with nothing animating, is the unit's 90.
#[test]
fn a_step_on_the_rest_value_while_animating_is_the_sliders_not_the_units() {
    let ops = depth_ops(60.6, 9.0);
    let near = |a: f64, b: f64| (a - b).abs() < 0.06;
    let mid: Vec<f64> = ops
        .iter()
        .filter(|(t, _)| near(*t, 7.92))
        .map(|(_, v)| *v)
        .collect();
    assert_eq!(
        mid,
        vec![50.0, 90.0],
        "the step (animating) then the end (the unit's knob): {ops:?}"
    );
    // (only that step is on the rest value: the other steps are the slider's own either way)
    assert_eq!(ops.iter().filter(|(_, v)| *v == 50.0).count(), 1);
}

/// The combination rule for the two keys that read the animation, with the state of the
/// instant given: the slider's when animating or moved off its rest value, else the unit's
/// (`FxUnitTests` literals: depth 0.9 under a macro's 50 and 100).
#[test]
fn the_animation_state_is_the_one_of_the_instant_given() {
    use djrust::fx::unit::FxUnitLayer;
    let mut u = FxUnitLayer::new(48_000.0);
    u.take(FxUnitCommand::Set {
        param: FxParam::PhaserDepth.index() as u8,
        value: 0.9,
        snap: true,
    });
    u.take(FxUnitCommand::Set {
        param: FxParam::PhaserRate.index() as u8,
        value: 0.7,
        snap: true,
    });
    for (key, rest, unit) in [
        (MasterFxKey::PhaserDepth, 50, 90.0),
        (MasterFxKey::PhaserFreq, 0, 70.0),
    ] {
        assert_eq!(u.automated_at(key, rest, true), rest as f64, "{key:?} animating");
        assert_eq!(u.automated_at(key, rest, false), unit, "{key:?} at rest: the knob");
        assert_eq!(u.automated_at(key, 80, false), 80.0, "{key:?} moved off rest");
    }
}
