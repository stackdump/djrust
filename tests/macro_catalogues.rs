//! The controller layer's catalogues against the engine's own (`control::macros` against
//! `wave::macros`, `fx::MasterMacro` / `MasterFxKey` and `fx::unit::FxParam`): they are the
//! same types and tables, one definition each. These held the controllers' own copies equal
//! to the engine's before the copies were deleted (the fold of 2026-10-29), and now pin that
//! `control::macros` is a facade over them: the ids, groups, ranges and the slider arithmetic a
//! controller's FX fader and `DeckAction.masterFx` share.

use djrust::control::macros::{self, master_fx_slider_value, master_macro_group};
use djrust::fx::unit::FxParam;
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::wave::{MacroTargets, NetMacro};

#[test]
fn the_net_macros_are_the_wave_engines() {
    let a: macros::NetMacro = NetMacro::Drop;
    assert_eq!(a, NetMacro::Drop);
    // chase targets (`MacroChase.swift`): the mute kind, and nothing for the rest
    let chased: Vec<(NetMacro, Option<MacroTargets>)> =
        NetMacro::ALL.iter().map(|m| (*m, m.chase_targets())).collect();
    assert_eq!(
        chased,
        vec![
            (NetMacro::Drop, Some(MacroTargets::NonDrums)),
            (NetMacro::Breakdown, Some(MacroTargets::DrumsOnly)),
            (NetMacro::SoloDrums, Some(MacroTargets::NonDrums)),
            (NetMacro::Cut, Some(MacroTargets::Everything)),
            (NetMacro::DoubleDrop, None),
            (NetMacro::BeatRepeat, None),
            (NetMacro::HalfTime, None),
            (NetMacro::TapeStop, None),
            (NetMacro::TempoAnchor, None),
        ]
    );
    let tempo: Vec<&str> = NetMacro::ALL.iter().filter(|m| m.is_tempo()).map(|m| m.id()).collect();
    assert_eq!(tempo, ["half-time", "tape-stop", "tempo-anchor"]);
    // the canonical enum has a fourth target the controllers never chase (beat repeat's)
    assert!(NetMacro::ALL.iter().all(|m| m.chase_targets() != Some(MacroTargets::NonKick)));
    for m in NetMacro::ALL {
        assert_eq!(NetMacro::from_id(m.id()), Some(m));
    }
}

#[test]
fn the_master_macros_have_the_ids_and_groups_the_controllers_read() {
    // (the table the controllers used to carry, order and all)
    let table: [(&str, &str); 18] = [
        ("sweep-lp", "FX"),
        ("sweep-hp", "FX"),
        ("reverb-wash", "FX"),
        ("riser", "FX"),
        ("build-crush", "FX"),
        ("phaser-drone", "FX"),
        ("filter-res", "FX"),
        ("octave-up", "Pitch"),
        ("octave-down", "Pitch"),
        ("pitch-bend", "Pitch"),
        ("vinyl-brake", "Pitch"),
        ("feel-chill", "Feel"),
        ("feel-drive", "Feel"),
        ("feel-ambient", "Feel"),
        ("feel-euphoric", "Feel"),
        ("feel-build", "Feel"),
        ("feel-wind-down", "Feel"),
        ("feel-reset", "Feel"),
    ];
    let engine: Vec<(&str, &str)> = MasterMacro::ALL.iter().map(|m| (m.id(), m.group())).collect();
    assert_eq!(engine, table);
    for (id, group) in table {
        assert_eq!(master_macro_group(id), Some(group), "{id}");
    }
    assert_eq!(master_macro_group("drop"), None);
    assert_eq!(master_macro_group(""), None);
}

#[test]
fn a_master_fx_fader_sets_the_sliders_own_units() {
    // (the keys the controllers used to carry, in order)
    let keys = [
        "hp-freq",
        "lp-freq",
        "phaser-freq",
        "phaser-depth",
        "phaser-wet",
        "crush-bits",
        "distortion",
        "master-pitch",
    ];
    let engine: Vec<&str> = MasterFxKey::ALL.iter().map(|k| k.id()).collect();
    assert_eq!(engine, keys);
    // (`MasterFxKey.sliderValue`: min + position (max - min), rounded; a NaN position is 0)
    for id in keys {
        let (min, max) = if id == "master-pitch" { (-12.0, 12.0) } else { (0.0, 100.0) };
        for p in [-0.5, 0.0, 0.1, 0.25, 0.333, 0.5, 0.505, 0.75, 0.999, 1.0, 1.5, f64::NAN] {
            let q = if p.is_nan() { 0.0 } else { f64::clamp(p, 0.0, 1.0) };
            let want = (min + q * (max - min)).round() as i64;
            assert_eq!(master_fx_slider_value(id, p), Some(want), "{id} {p}");
        }
    }
    assert_eq!(master_fx_slider_value("tempo", 0.5), None);
}

#[test]
fn the_fx_params_are_the_fx_units() {
    let a: macros::FxParam = FxParam::Echo;
    assert_eq!(a, FxParam::Echo);
    let mut sorted = FxParam::ALL;
    sorted.sort();
    assert_eq!(sorted, FxParam::ALL, "in catalogue order");
    for p in FxParam::ALL {
        assert_eq!(macros::FxParam::from_id(p.id()), Some(p));
    }
}
