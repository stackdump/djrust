//! Ports of DeviceLEDsTests (palette, swatches), MacroChaseTests and RingSpinTests
//! (beats-btw-dj): the pinned numbers are device-map.js's `PALETTE`, petri-note.css and
//! runtime.js, worked out there from the JS.

use std::collections::{BTreeMap, BTreeSet};

use djrust::control::chase::{
    Glow, MacroActivity, MacroChase, MacroChaseBoard, MacroPulseLevel, MacroPulseState,
    MacroPulseStyle,
};
use djrust::control::controller::{LightLevel, PadLight};
use djrust::control::leds::{APCPalette, DeviceLEDs, DeviceLights, Rgba};
use djrust::control::macros::NetMacro;
use djrust::control::ring::{RingArcs, RingPlayhead, RingSpin};

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn rgb(c: Rgba) -> [i64; 3] {
    let (r, g, b) = c.rgb8();
    [r, g, b]
}

// MARK: DeviceLEDsTests

#[test]
fn the_palette_is_device_map_js_table() {
    let want: [(u8, &str, [i64; 3]); 8] = [
        (3, "white", [255, 255, 255]),
        (5, "red", [255, 45, 45]),
        (9, "orange", [255, 140, 0]),
        (13, "yellow", [255, 212, 0]),
        (21, "green", [25, 195, 77]),
        (37, "cyan", [30, 200, 255]),
        (45, "blue", [58, 92, 255]),
        (53, "magenta", [214, 58, 214]),
    ];
    assert_eq!(APCPalette::ENTRIES.len(), want.len());
    for (i, name, c) in want {
        assert_eq!(rgb(APCPalette::pinned(i).unwrap()), c, "{i} {name}");
        assert_eq!(rgb(APCPalette::color(i)), c);
        assert_eq!(
            APCPalette::ENTRIES.iter().find(|e| e.0 == i).unwrap().1,
            name
        );
    }
    assert!(APCPalette::pinned(0).is_none(), "0 is `transparent`: off");
    assert_eq!(rgb(APCPalette::color(0)), [0, 0, 0]);
    // an index beats does not draw still shows a colour (a hue picked from the index)
    assert!(APCPalette::pinned(17).is_none());
    assert_eq!(APCPalette::color(17), Rgba::from_hsb(17.0 / 64.0, 0.8, 0.9));
    assert_eq!(
        APCPalette::color(17 + 64),
        APCPalette::color(17),
        "wraps at 64"
    );
}

#[test]
fn the_single_led_colours_are_the_device_maps_css() {
    assert_eq!(rgb(APCPalette::track_on()), [255, 45, 45]);
    assert_eq!(rgb(APCPalette::track_off()), [26, 10, 10]);
    assert_eq!(rgb(APCPalette::scene_on()), [25, 195, 77]);
    assert_eq!(rgb(APCPalette::scene_off()), [10, 26, 10]);
    assert_eq!(APCPalette::OFF_OPACITY, 0.4);
}

#[test]
fn blink_is_six_hundred_milliseconds_half_on_half_dim() {
    assert_eq!(DeviceLEDs::BLINK_PERIOD, 0.6);
    let o = DeviceLEDs::blink_opacity;
    assert!(o(0.0) == 1.0 && o(0.2) == 1.0 && o(0.29) == 1.0, "0%...49%");
    assert!(
        o(0.3) == 0.3 && o(0.45) == 0.3 && o(0.59) == 0.3,
        "50%...100%"
    );
    assert!(o(0.6) == 1.0 && o(0.95) == 0.3 && o(1.3) == 1.0 && o(6.1) == 1.0 && o(6.4) == 0.3);
    assert_eq!(o(-0.1), 0.3, "a negative clock wraps the same way");
}

#[test]
fn rgb_pads_take_their_palette_colour_and_brightness() {
    let full = DeviceLEDs::swatch(PadLight::new(9, LightLevel::Full), "pads", true);
    assert!(rgb(full.color) == [255, 140, 0] && full.opacity == 1.0 && full.lit && !full.blinks);
    let dim = DeviceLEDs::swatch(PadLight::new(9, LightLevel::Dim), "pads", true);
    assert!(
        rgb(dim.color) == [255, 140, 0] && dim.opacity == 0.5 && dim.lit,
        "PAD_DIM is solid 50%"
    );
    // an unbound pad (index 0), or an LED that is off, is dark
    for l in [
        PadLight::new(0, LightLevel::Dim),
        PadLight::new(9, LightLevel::Off),
    ] {
        let off = DeviceLEDs::swatch(l, "pads", true);
        assert!(!off.lit && off.opacity == 0.0 && !off.blinks);
    }
}

#[test]
fn single_leds_are_red_on_the_track_row_and_green_on_the_scene_column() {
    let s = |level, group| DeviceLEDs::swatch(PadLight::new(0, level), group, false);
    assert!(
        rgb(s(LightLevel::Full, "track").color) == [255, 45, 45]
            && s(LightLevel::Full, "track").opacity == 1.0
    );
    assert_eq!(rgb(s(LightLevel::Full, "scene").color), [25, 195, 77]);
    let off = s(LightLevel::Off, "track");
    assert!(
        rgb(off.color) == [26, 10, 10] && off.opacity == 0.4 && !off.lit,
        ".pn-dm-off"
    );
    assert!(s(LightLevel::Dim, "scene").opacity == 0.4 && s(LightLevel::Dim, "scene").lit);
    let blink = s(LightLevel::Blink, "scene");
    assert!(
        blink.blinks && blink.lit && blink.opacity_at(0.0) == 1.0 && blink.opacity_at(0.35) == 0.3
    );
    assert_eq!(
        s(LightLevel::Full, "track").opacity_at(0.35),
        1.0,
        "a steady LED ignores the clock"
    );
    // any other single-LED group (the Inpulse's buttons) is white: its colour is not in the profile
    assert_eq!(
        rgb(s(LightLevel::Full, "deckA.buttons").color),
        [255, 255, 255]
    );
}

#[test]
fn device_lights_looks_up_one_control() {
    let lights = DeviceLights {
        groups: BTreeMap::from([
            ("pads".to_string(), vec![PadLight::new(5, LightLevel::Full)]),
            (
                "scene".to_string(),
                vec![
                    PadLight::new(0, LightLevel::Blink),
                    PadLight::new(0, LightLevel::Off),
                ],
            ),
        ]),
        rgb_groups: BTreeSet::from(["pads".to_string()]),
    };
    assert_eq!(
        lights.swatch("pads", 0).map(|s| rgb(s.color)),
        Some([255, 45, 45])
    );
    assert_eq!(lights.swatch("scene", 0).map(|s| s.blinks), Some(true));
    assert_eq!(lights.swatch("scene", 1).map(|s| s.lit), Some(false));
    assert!(lights.swatch("pads", 1).is_none(), "past the group");
    assert!(lights.swatch("faders", 0).is_none(), "a group with no LEDs");
    assert!(lights.any_blinking() && !lights.is_empty());
    let empty = DeviceLights {
        groups: BTreeMap::new(),
        rgb_groups: BTreeSet::new(),
    };
    assert!(!empty.any_blinking());
}

// MARK: MacroChaseTests

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_running_light_moves_every_hundred_and_twenty_milliseconds() {
    let mut c = MacroChase::new(NetMacro::Drop, names(&["bass", "melody", "harmony"]));
    assert_eq!(MacroChase::STEP, 0.12);
    assert!(
        c.hot() == Some("bass") && c.hot_index() == Some(0),
        "the first frame lights element 0 at once"
    );
    c.advance(0.119);
    assert_eq!(c.hot(), Some("bass"), "just before the boundary");
    c.advance(0.001);
    assert_eq!(c.hot(), Some("melody"), "120 ms: the second");
    c.advance(0.12);
    assert!(c.hot() == Some("harmony") && c.steps() == 2);
    c.advance(0.12);
    assert!(
        c.hot() == Some("bass") && c.steps() == 3,
        "and it wraps round"
    );
    c.advance(0.36);
    assert!(c.hot() == Some("bass") && c.steps() == 6, "two more laps");
}

#[test]
fn a_clock_of_small_steps_sits_on_the_boundary_not_under_it() {
    let mut c = MacroChase::new(NetMacro::Cut, names(&["a", "b"]));
    for _ in 0..3 {
        c.advance(0.04); // 0.12000000000000001
    }
    assert_eq!(c.hot(), Some("b"));
    let mut d = MacroChase::new(NetMacro::Cut, names(&["a", "b"]));
    for _ in 0..12 {
        d.advance(0.01); // 0.12 summed in hundredths: 0.12 or a hair under
    }
    assert_eq!(
        d.hot(),
        Some("b"),
        "a hair under 120 ms must still count: {}",
        d.elapsed()
    );
    // a 30 Hz poll: 33.3 ms per frame
    let mut e = MacroChase::new(NetMacro::Cut, names(&["a", "b", "c"]));
    let mut seen = Vec::new();
    for _ in 0..12 {
        e.advance(1.0 / 30.0);
        seen.push(e.hot().unwrap().to_string());
    }
    assert_eq!(
        seen,
        ["a", "a", "a", "b", "b", "b", "b", "c", "c", "c", "a", "a"],
        "{seen:?}"
    );
    assert_eq!(e.steps(), 3, "0.4 s is three 120 ms steps");
}

#[test]
fn no_targets_means_nothing_is_hot() {
    let mut c = MacroChase::new(NetMacro::Drop, vec![]);
    assert!(c.hot().is_none() && c.hot_index().is_none());
    c.advance(1.0);
    assert!(c.hot().is_none());
    c.retarget(names(&["bass"]));
    assert_eq!(c.hot(), Some("bass"));
}

#[test]
fn retargeting_keeps_the_light_where_it_is() {
    let mut c = MacroChase::new(NetMacro::Drop, names(&["bass", "melody", "harmony", "arp"]));
    c.advance(0.25); // two steps: harmony
    assert_eq!(c.hot(), Some("harmony"));
    c.retarget(names(&["bass", "melody"])); // the engine now holds two: step 2 % 2 = 0
    assert!(c.hot() == Some("bass") && c.elapsed() == 0.25);
}

#[test]
fn negative_and_non_finite_time_is_ignored() {
    let mut c = MacroChase::new(NetMacro::Drop, names(&["a", "b"]));
    c.advance(-1.0);
    c.advance(f64::NAN);
    c.advance(f64::INFINITY);
    assert!(c.elapsed() == 0.0 && c.hot() == Some("a"));
}

#[test]
fn the_catalogue_says_which_macros_chase_and_what_they_touch() {
    use djrust::control::macros::MacroTargets::*;
    // catalog.js: drop / solo-drums -> nonDrums, breakdown -> drumsOnly, cut -> everything
    assert_eq!(NetMacro::Drop.chase_targets(), Some(NonDrums));
    assert_eq!(NetMacro::SoloDrums.chase_targets(), Some(NonDrums));
    assert_eq!(NetMacro::Breakdown.chase_targets(), Some(DrumsOnly));
    assert_eq!(NetMacro::Cut.chase_targets(), Some(Everything));
    // compound, beat-repeat and tempo kinds never call macroPulse in the JS
    for m in [
        NetMacro::DoubleDrop,
        NetMacro::BeatRepeat,
        NetMacro::HalfTime,
        NetMacro::TapeStop,
        NetMacro::TempoAnchor,
    ] {
        assert!(m.chase_targets().is_none(), "{m:?}");
    }
}

fn activity(running: &[NetMacro], targets: &[(NetMacro, &[&str])]) -> MacroActivity {
    MacroActivity {
        running: running.to_vec(),
        targets: targets.iter().map(|(m, t)| (*m, names(t))).collect(),
    }
}

#[test]
fn the_board_starts_a_chase_when_a_macro_appears_and_ends_it_when_it_does() {
    let mut board = MacroChaseBoard::new();
    assert_eq!(board.state(), MacroPulseState::default());
    let drop = activity(
        &[NetMacro::Drop],
        &[(NetMacro::Drop, &["bass", "melody", "harmony"])],
    );
    board.update(&drop, 0.5); // the first update starts the chase: no time has passed yet
    assert!(
        board.state().hot == set(&["bass"])
            && board.state().pulsing == set(&["bass", "melody", "harmony"])
    );
    assert!(board.state().running == BTreeSet::from([NetMacro::Drop]) && board.chases.len() == 1);
    board.update(&drop, 0.12);
    assert_eq!(board.state().hot, set(&["melody"]));
    board.update(&drop, 0.12);
    assert_eq!(board.state().hot, set(&["harmony"]));
    board.update(&drop, 0.12);
    assert_eq!(board.state().hot, set(&["bass"]), "wraps");
    board.update(&MacroActivity::idle(), 0.033); // the engine says the macro is over
    assert!(
        board.state() == MacroPulseState::default() && board.chases.is_empty(),
        "end on macro end"
    );
    // the next fire starts from the first target again
    board.update(&drop, 0.033);
    assert_eq!(board.state().hot, set(&["bass"]));
}

#[test]
fn a_macro_without_a_chase_only_shows_running() {
    let mut board = MacroChaseBoard::new();
    board.update(
        &activity(&[NetMacro::BeatRepeat, NetMacro::HalfTime], &[]),
        0.1,
    );
    let s = board.state();
    assert_eq!(
        s.running,
        BTreeSet::from([NetMacro::BeatRepeat, NetMacro::HalfTime])
    );
    assert!(s.pulsing.is_empty() && s.hot.is_empty() && board.chases.is_empty());
    assert!(s.is_running(NetMacro::BeatRepeat) && !s.is_running(NetMacro::Drop));
}

#[test]
fn two_macros_chase_at_once_and_each_ends_on_its_own() {
    let mut board = MacroChaseBoard::new();
    let both = activity(
        &[NetMacro::Drop, NetMacro::Breakdown],
        &[
            (NetMacro::Drop, &["bass", "melody"]),
            (NetMacro::Breakdown, &["kick", "snare", "hihat"]),
        ],
    );
    board.update(&both, 0.0);
    assert_eq!(
        board.state().hot,
        set(&["bass", "kick"]),
        "one light per macro"
    );
    assert_eq!(
        board.state().pulsing,
        set(&["bass", "melody", "kick", "snare", "hihat"])
    );
    board.update(&both, 0.12);
    assert_eq!(board.state().hot, set(&["melody", "snare"]));
    board.update(
        &activity(
            &[NetMacro::Breakdown],
            &[(NetMacro::Breakdown, &["kick", "snare"])],
        ),
        0.12,
    );
    let s = board.state();
    assert!(s.running == BTreeSet::from([NetMacro::Breakdown]) && board.chases.len() == 1);
    assert_eq!(
        s.pulsing,
        set(&["kick", "snare"]),
        "retargeted to what the engine still holds"
    );
    assert_eq!(s.hot, set(&["kick"]), "step 2 of 2 targets");
    assert!(s.level("kick") == MacroPulseLevel::Hot && s.level("snare") == MacroPulseLevel::Dim);
    assert_eq!(s.level("bass"), MacroPulseLevel::None);
}

#[test]
fn a_role_hot_in_one_chase_is_hot_whatever_the_other_says() {
    let mut board = MacroChaseBoard::new();
    let both = activity(
        &[NetMacro::Drop, NetMacro::SoloDrums],
        &[
            (NetMacro::Drop, &["bass", "melody"]),
            (NetMacro::SoloDrums, &["melody", "bass"]),
        ],
    );
    board.update(&both, 0.0);
    assert_eq!(board.state().hot, set(&["bass", "melody"]));
    assert!(
        board.state().level("bass") == MacroPulseLevel::Hot
            && board.state().level("melody") == MacroPulseLevel::Hot
    );
}

#[test]
fn the_glow_numbers_are_beats_css() {
    assert_eq!(
        MacroPulseStyle::DIM,
        Glow {
            spread: 1.0,
            blur: 4.0,
            alpha: 0.35
        }
    );
    assert_eq!(
        MacroPulseStyle::HOT,
        Glow {
            spread: 3.0,
            blur: 10.0,
            alpha: 0.95
        }
    );
    assert!(MacroPulseStyle::glow(MacroPulseLevel::None).is_none());
    assert_eq!(
        MacroPulseStyle::glow(MacroPulseLevel::Dim),
        Some(MacroPulseStyle::DIM)
    );
    assert_eq!(
        MacroPulseStyle::glow(MacroPulseLevel::Hot),
        Some(MacroPulseStyle::HOT)
    );
    assert_eq!(MacroPulseStyle::color().rgb8(), (233, 69, 96));
    assert_eq!(MacroPulseStyle::running_fill().rgb8(), (233, 69, 96));
    assert_eq!(MacroPulseStyle::running_border().rgb8(), (248, 179, 192));
}

#[test]
fn the_running_button_breathes_on_a_one_second_ease_in_out() {
    type S = MacroPulseStyle;
    assert_eq!(S::RUNNING_PERIOD, 1.0);
    let rest = S::running_glow_at(0.0);
    assert!(
        rest == S::RUNNING_REST && S::running_glow_at(1.0) == S::RUNNING_REST,
        "0% and 100%"
    );
    let peak = S::running_glow_at(0.5);
    assert!(
        near(peak.spread, 3.0) && near(peak.blur, 10.0) && near(peak.alpha, 0.95),
        "50%"
    );
    // a quarter of the way: half of each half, ease-in-out(0.5) = 0.5
    let q = S::running_glow_at(0.25);
    assert!(
        near(q.spread, 1.5) && near(q.blur, 5.0) && near(q.alpha, 0.825),
        "{q:?}"
    );
    // an eighth: progress 0.25 of the rise, eased: cubic-bezier(.42, 0, .58, 1)(0.25) = 0.12916193...
    let e = S::running_glow_at(0.125);
    assert!(
        (e.blur - 10.0 * 0.1291619310473198).abs() < 1e-6,
        "{}",
        e.blur
    );
    // symmetrical on the way down
    assert!((S::running_glow_at(0.875).blur - e.blur).abs() < 1e-9);
    // and periodic
    assert!(
        (S::running_glow_at(3.125).blur - e.blur).abs() < 1e-9
            && (S::running_glow_at(-0.875).blur - e.blur).abs() < 1e-9
    );
}

#[test]
fn ease_in_out_matches_the_css_curve() {
    // reference values from an independent bisection in node
    let want = [
        (0.0, 0.0),
        (0.1, 0.019722453548311196),
        (0.25, 0.1291619310473198),
        (0.5, 0.5),
        (0.75, 0.8708380689526802),
        (0.9, 0.9802775464516887),
        (1.0, 1.0),
    ];
    for (x, y) in want {
        assert!(
            (MacroPulseStyle::ease_in_out(x) - y).abs() < 1e-9,
            "x = {x}"
        );
    }
    assert!(MacroPulseStyle::ease_in_out(-1.0) == 0.0 && MacroPulseStyle::ease_in_out(2.0) == 1.0);
}

// MARK: RingSpinTests

#[test]
fn easing_is_a_cubic_ease_out() {
    assert_eq!(RingSpin::eased(0.0), 0.0);
    assert!(near(RingSpin::eased(0.25), 0.578125)); // 1 - 0.75^3
    assert!(near(RingSpin::eased(0.5), 0.875)); // 1 - 0.5^3
    assert_eq!(RingSpin::eased(1.0), 1.0);
    assert!(
        RingSpin::eased(-3.0) == 0.0 && RingSpin::eased(7.0) == 1.0,
        "t is clamped"
    );
}

#[test]
fn a_ring_that_never_spun_sits_at_zero() {
    let s = RingSpin::new();
    assert!(s.angle_at(0.0) == 0.0 && s.angle_at(1e9) == 0.0);
    assert!(!s.is_spinning(5.0) && !s.reverse_arrows());
}

#[test]
fn the_first_spin_swings_ninety_degrees_in_eight_hundred_milliseconds() {
    let mut s = RingSpin::new();
    s.spin(1, 10.0);
    assert_eq!(s.angle_at(10.0), 0.0, "t = 0");
    assert!(
        near(s.angle_at(10.2), 90.0 * 0.578125),
        "a quarter of the way in time"
    );
    assert!(near(s.angle_at(10.4), 78.75), "t = 0.5: 90 * 0.875");
    assert!(near(s.angle_at(10.8), 90.0), "t = 1");
    assert!(
        s.angle_at(11.0) == 90.0 && s.angle_at(600.0) == 90.0,
        "and it stays"
    );
    assert_eq!(s.angle_at(9.5), 0.0, "before the start it has not moved");
    assert!(s.is_spinning(10.79) && !s.is_spinning(10.8));
}

#[test]
fn direction_alternates_between_spins_and_the_arrows_follow() {
    let mut s = RingSpin::new();
    s.spin(1, 0.0);
    assert!(
        s.target_angle() == 90.0 && !s.reverse_arrows(),
        "dir +1: arrows forward"
    );
    s.spin(1, 5.0);
    assert!(
        s.target_angle() == 0.0 && s.reverse_arrows(),
        "dir -1: back to 0, arrows reversed"
    );
    s.spin(1, 10.0);
    assert!(s.target_angle() == 90.0 && !s.reverse_arrows());
}

#[test]
fn steps_scale_the_swing_and_zero_does_nothing() {
    let mut s = RingSpin::new();
    s.spin(0, 1.0);
    assert!(
        s.target_angle() == 0.0 && s.angle_at(2.0) == 0.0,
        "autoDjSpin returns for steps <= 0"
    );
    s.spin(3, 1.0);
    assert_eq!(s.target_angle(), 270.0);
    assert!(near(s.angle_at(1.8), 270.0));
}

#[test]
fn a_spin_during_a_swing_continues_from_where_it_is() {
    let mut s = RingSpin::new();
    s.spin(1, 10.0); // 0 -> 90
    s.spin(1, 10.4); // from 78.75, target 90 - 90 = 0
    assert!(near(s.angle_at(10.4), 78.75));
    assert!(
        near(s.angle_at(10.8), 78.75 * (1.0 - 0.875)),
        "t = 0.5 of the second swing"
    );
    assert!(near(s.angle_at(11.2), 0.0));
}

#[test]
fn the_first_look_only_records_and_each_hand_over_spins_once() {
    let mut s = RingSpin::new();
    s.observe(7, 100.0);
    assert_eq!(
        s.target_angle(),
        0.0,
        "a deck already on its eighth track does not swing"
    );
    s.observe(7, 101.0);
    assert_eq!(s.target_angle(), 0.0);
    s.observe(8, 102.0);
    assert!(s.target_angle() == 90.0 && s.is_spinning(102.1));
    s.observe(8, 103.0);
    assert_eq!(s.target_angle(), 90.0, "same count again: nothing");
    s.observe(9, 104.0);
    assert_eq!(s.target_angle(), 0.0);
}

#[test]
fn the_arc_layer_dims_while_playing() {
    let stopped = RingArcs {
        playing: false,
        angle_degrees: 0.0,
        reverse_arrows: false,
    };
    let playing = RingArcs {
        playing: true,
        ..stopped
    };
    assert!(stopped.alpha() == 1.0 && playing.alpha() == 0.3);
    assert!(stopped.line_width(1.0) == 2.0 && playing.line_width(1.0) == 1.0);
    assert_eq!(stopped.line_width(0.5), 4.0, "(playing ? 1 : 2) / scale");
    assert_eq!(playing.line_width(0.25), 4.0);
    assert_eq!(RingArcs::STROKE_HEX, "#4a90d9");
}

#[test]
fn arcs_carry_the_spin_for_a_frame() {
    let mut s = RingSpin::new();
    s.spin(1, 0.0);
    let a = s.arcs(true, 0.4);
    assert!(a.playing && !a.reverse_arrows && near(a.angle_degrees, 78.75));
    s.spin(1, 2.0);
    assert!(s.arcs(false, 2.0).reverse_arrows);
}

#[test]
fn the_playhead_is_the_beats_gold_with_its_two_glows() {
    assert_eq!(RingPlayhead::HEX, "#ffd36a");
    assert_eq!(RingPlayhead::RGB8, (255, 211, 106));
    assert!(RingPlayhead::OUTER_GLOW_ALPHA == 0.7 && RingPlayhead::INNER_GLOW_ALPHA == 0.35);
}

#[test]
fn the_playhead_sizes_are_the_css_ones_in_the_rings_own_units() {
    for base in [4.0, 4.5, 5.0] {
        // above the one-point floors
        let r = RingPlayhead::radius(base);
        assert!(
            near(RingPlayhead::line_width(base) / r, 3.0 / 20.0),
            "border 3 px of 20"
        );
        assert!(
            near(RingPlayhead::outer_glow_radius(base) / r, 3.0 / 20.0),
            "blur 6 px = sigma 3 px"
        );
        assert!(
            near(RingPlayhead::inner_glow_radius(base) / r, 4.0 / 20.0),
            "inset blur 4 px"
        );
    }
    // the largest dot (base 5): a 19 pt ring, 0.475 pt to the CSS px
    assert!(near(RingPlayhead::points_per_px(5.0), 0.475));
    assert!(near(RingPlayhead::line_width(5.0), 1.425));
}

#[test]
fn the_playhead_sizes_never_fall_below_one_point() {
    // a 64-step ring: base 1.5, where 3 px of the CSS node would be 0.43 pt
    assert_eq!(RingPlayhead::line_width(1.5), 1.0);
    assert_eq!(RingPlayhead::outer_glow_radius(1.5), 1.0);
    assert_eq!(RingPlayhead::inner_glow_radius(1.5), 1.0);
}

#[test]
fn an_arrowhead_is_drawn_only_where_it_fits_between_the_dots() {
    // 8 steps in a 66 pt square: chord 2 * 27 * sin(pi/8) = 20.66, dots of 5
    let c8 = 2.0 * 27.0 * 0.3826834323650898;
    assert!(
        near(RingArcs::head_length(c8, 5.0).unwrap_or(-1.0), 4.0),
        "wings of min(0.35 chord, 4)"
    );
    // 16 steps in a 66 pt square: chord 10.5, dots of 3.7: tip set-backs and wing don't fit
    let c16 = 2.0 * 27.0 * 0.19509032201612825;
    assert!(RingArcs::head_length(c16, 3.7).is_none());
    // 16 steps in 130 pt: chord 23, dots of 5: 14 + 4 = 18 fits
    let big = 2.0 * 59.0 * 0.19509032201612825;
    assert!(RingArcs::head_length(big, 5.0).is_some());
    // a short chord that would fit a head shrinks it: 0.35 of 13 is 4.55 -> 4; of 10 is 3.5
    assert!(near(RingArcs::head_length(10.0, 1.5).unwrap_or(-1.0), 3.5));
    assert!(RingArcs::head_length(0.0, 1.5).is_none());
}

#[test]
fn the_arrowhead_is_js_draw_arrowhead_scaled_to_the_ring() {
    assert!(
        near(RingArcs::WING_ANGLE, std::f64::consts::PI / 6.0),
        "+-30 degrees"
    );
    assert!(
        near(RingArcs::tip_set_back(5.0), 7.0),
        "the tip stops one dot radius (1.4 base) short"
    );
}
