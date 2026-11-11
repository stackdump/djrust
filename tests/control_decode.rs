//! Controller decode: ports of JogEventTests, JogAdapterTests, SurfaceProfileTests,
//! ControllerLayoutTests, ControlWalkTests and DeviceMapTests (beats-btw-dj), plus the replay
//! of the messages captured from real hardware (fixtures/js/captured-messages.json), which
//! is the decode oracle.

use std::collections::BTreeSet;

use djrust::control::jog::{
    decode_relative, encode_relative, Encoding, JogAdapter, JogClock, JogEvent,
};
use djrust::control::layout::ControllerLayout;
use djrust::control::profiles::{
    all_surfaces, apc_mini_mk2, inpulse_200_mk3, inpulse_200_mk3_jog, mpk_mini_mk2,
};
use djrust::control::surface::{ControlEvent, DJActionKind, SurfaceDecoder};
use djrust::control::walk::{ControlWalk, WalkOutcome};
use djrust::json::JsonValue;

fn hexs(m: &[u8]) -> String {
    m.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn captured() -> Vec<(String, Vec<Vec<u8>>)> {
    let path = format!(
        "{}/tests/fixtures/js/captured-messages.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            let msgs = v
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m.ints().unwrap().into_iter().map(|b| b as u8).collect())
                .collect();
            (k.to_string(), msgs)
        })
        .collect()
}

// MARK: JogEventTests (mirrors the parser cases in web/jog/jog.test.mjs)

#[test]
fn bundled_jog_profile_loads() {
    let p = inpulse_200_mk3_jog();
    assert_eq!(p.ticks_per_rev, 248);
    assert_eq!(
        p.decks,
        vec![("A".to_string(), 1u8), ("B".to_string(), 2u8)]
    );
    assert!(p.matches("DJControl Inpulse 200 Mk3"));
    assert!(!p.matches("btw Bus 1"));
}

#[test]
fn twos_complement() {
    for (value, ticks) in [
        (0x01u8, 1i64),
        (0x7F, -1),
        (0x02, 2),
        (0x7E, -2),
        (0x3F, 63),
        (0x40, -64),
    ] {
        assert_eq!(decode_relative(value, Encoding::Twos), ticks);
        assert_eq!(encode_relative(ticks, Encoding::Twos), value);
    }
}

#[test]
fn offset64() {
    assert_eq!(decode_relative(0x41, Encoding::Offset64), 1);
    assert_eq!(decode_relative(0x3F, Encoding::Offset64), -1);
    assert_eq!(encode_relative(-1, Encoding::Offset64), 0x3F);
}

#[test]
fn parses_decks_shift_and_kinds() {
    let p = inpulse_200_mk3_jog();
    let a = || "A".to_string();
    let b = || "B".to_string();
    assert_eq!(
        p.parse(&[0xB1, 0x0A, 0x01]),
        Some(JogEvent::Scratch {
            deck: a(),
            shift: false,
            ticks: 1
        })
    );
    assert_eq!(
        p.parse(&[0xB2, 0x09, 0x7F]),
        Some(JogEvent::Bend {
            deck: b(),
            shift: false,
            ticks: -1
        })
    );
    assert_eq!(
        p.parse(&[0xB4, 0x0A, 0x7F]),
        Some(JogEvent::Scratch {
            deck: a(),
            shift: true,
            ticks: -1
        })
    );
    assert_eq!(
        p.parse(&[0x91, 0x08, 0x7F]),
        Some(JogEvent::Touch {
            deck: a(),
            shift: false,
            down: true
        })
    );
    assert_eq!(
        p.parse(&[0x91, 0x08, 0x00]),
        Some(JogEvent::Touch {
            deck: a(),
            shift: false,
            down: false
        })
    );
    assert_eq!(
        p.parse(&[0x81, 0x08, 0x40]),
        Some(JogEvent::Touch {
            deck: a(),
            shift: false,
            down: false
        })
    );
}

#[test]
fn ignores_non_jog() {
    let p = inpulse_200_mk3_jog();
    assert_eq!(p.parse(&[0xB0, 0x0A, 0x01]), None); // mixer channel
    assert_eq!(p.parse(&[0xB1, 0x0A, 0x00]), None); // zero ticks
    assert_eq!(p.parse(&[0xB1, 0x30, 0x01]), None); // other CC
    assert_eq!(p.parse(&[0xB1, 0x0A]), None); // short
}

#[test]
fn seconds_per_tick() {
    assert!((inpulse_200_mk3_jog().seconds_per_tick() - 1.8 / 248.0).abs() < 1e-12);
}

// MARK: JogAdapterTests

#[derive(Default)]
struct RecordingClock {
    calls: Vec<String>,
}

impl JogClock for RecordingClock {
    fn freeze(&mut self) {
        self.calls.push("freeze".into());
    }
    fn scrub_by(&mut self, seconds: f64) {
        self.calls.push(format!("scrub {seconds:.6}"));
    }
    fn release(&mut self) {
        self.calls.push("release".into());
    }
    fn nudge(&mut self, ratio: f64) {
        self.calls.push(format!("nudge {ratio:.4}"));
    }
}

fn ev_touch(down: bool) -> JogEvent {
    JogEvent::Touch {
        deck: "A".into(),
        shift: false,
        down,
    }
}

#[test]
fn touch_scratch_release() {
    let profile = inpulse_200_mk3_jog();
    let spt = profile.seconds_per_tick();
    let mut clock = RecordingClock::default();
    let mut a = JogAdapter::new(&profile, "A");
    a.handle(&ev_touch(true), &mut clock);
    a.handle(&ev_touch(true), &mut clock); // repeat ignored
    a.handle(
        &JogEvent::Scratch {
            deck: "A".into(),
            shift: false,
            ticks: 2,
        },
        &mut clock,
    );
    a.handle(
        &JogEvent::Scratch {
            deck: "A".into(),
            shift: true,
            ticks: -1,
        },
        &mut clock,
    );
    a.handle(&ev_touch(false), &mut clock);
    assert_eq!(
        clock.calls,
        vec![
            "freeze".to_string(),
            format!("scrub {:.6}", 2.0 * spt),
            format!("scrub {:.6}", -4.0 * spt),
            "release".to_string()
        ]
    );
}

#[test]
fn other_deck_ignored() {
    let profile = inpulse_200_mk3_jog();
    let mut clock = RecordingClock::default();
    let mut a = JogAdapter::new(&profile, "B");
    a.handle(&ev_touch(true), &mut clock);
    a.handle(
        &JogEvent::Scratch {
            deck: "A".into(),
            shift: false,
            ticks: 1,
        },
        &mut clock,
    );
    assert!(clock.calls.is_empty());
}

#[test]
fn untouched_scratch_bends_and_decays() {
    let profile = inpulse_200_mk3_jog();
    let mut clock = RecordingClock::default();
    let mut a = JogAdapter::new(&profile, "A");
    a.handle(
        &JogEvent::Scratch {
            deck: "A".into(),
            shift: false,
            ticks: 5,
        },
        &mut clock,
    );
    assert!(a.nudge() > 0.0);
    assert!(clock.calls.is_empty()); // nudges go out on decay ticks
    a.decay(0.0, &mut clock);
    assert_eq!(clock.calls.len(), 1);
    a.decay(0.15, &mut clock); // one time constant
    let after = a.nudge();
    a.decay(10.0, &mut clock);
    assert_eq!(a.nudge(), 0.0);
    assert_eq!(clock.calls.last().unwrap(), "nudge 0.0000");
    assert!(after > 0.0 && after < 0.5);
}

#[test]
fn bend_settles_at_depth_at_platter_speed() {
    let profile = inpulse_200_mk3_jog();
    let mut clock = RecordingClock::default();
    let mut a = JogAdapter::new(&profile, "A");
    // Feed ticks at platter speed (ticksPerRev * rpm/60 per second) for 2 s.
    let rate = profile.ticks_per_rev as f64 * profile.platter_rpm / 60.0;
    let mut t = 0.0;
    a.decay(t, &mut clock);
    while t < 2.0 {
        t += 1.0 / rate;
        a.handle(
            &JogEvent::Bend {
                deck: "A".into(),
                shift: false,
                ticks: 1,
            },
            &mut clock,
        );
        a.decay(t, &mut clock);
    }
    assert!((a.nudge() - 0.10).abs() < 0.01, "{}", a.nudge());
}

#[test]
fn touch_clears_nudge() {
    let profile = inpulse_200_mk3_jog();
    let mut clock = RecordingClock::default();
    let mut a = JogAdapter::new(&profile, "A");
    a.handle(
        &JogEvent::Bend {
            deck: "A".into(),
            shift: false,
            ticks: 10,
        },
        &mut clock,
    );
    a.handle(&ev_touch(true), &mut clock);
    assert_eq!(a.nudge(), 0.0);
}

// MARK: SurfaceProfileTests

#[test]
fn profiles_load_and_match_port_names() {
    let (apc, mpk) = (apc_mini_mk2(), mpk_mini_mk2());
    assert!(apc.matches("APC mini mk2 Control"));
    assert!(!apc.matches("APC mini mk2 Notes"));
    assert!(!apc.matches("APC mini")); // mk1 has a different protocol
    assert!(mpk.matches("MPK Mini Mk II"));
    assert!(mpk.matches("MPKmini2"));
    assert!(!mpk.matches("DJControl Inpulse 200 Mk3"));
}

fn gi(e: &Option<ControlEvent>) -> Option<(String, usize)> {
    e.as_ref().map(|e| (e.group.clone(), e.index))
}

#[test]
fn apc_parses_pads_buttons_and_faders() {
    let apc = apc_mini_mk2();
    assert_eq!(
        apc.parse(&[0x90, 0x00, 0x7F]),
        Some(ControlEvent {
            group: "pads".into(),
            index: 0,
            value: 1.0,
            pressed: true,
            is_cc: false,
            delta: 0,
            name: None
        })
    );
    assert_eq!(
        apc.parse(&[0x90, 0x3F, 0x00]).map(|e| e.pressed),
        Some(false)
    );
    assert_eq!(
        apc.parse(&[0x80, 0x09, 0x40]).map(|e| e.pressed),
        Some(false)
    );
    assert_eq!(
        apc.parse(&[0x90, 0x64, 0x7F]).map(|e| e.group),
        Some("track".to_string())
    );
    assert_eq!(
        gi(&apc.parse(&[0x90, 0x77, 0x7F])),
        Some(("scene".into(), 7))
    );
    assert_eq!(
        apc.parse(&[0xB0, 0x38, 127]),
        Some(ControlEvent {
            group: "faders".into(),
            index: 8,
            value: 1.0,
            pressed: true,
            is_cc: true,
            delta: 0,
            name: None
        })
    );
    assert_eq!(apc.parse(&[0xB0, 0x39, 10]), None); // past master fader
    assert_eq!(apc.parse(&[0x90, 0x40, 0x7F]), None); // gap above the grid
}

#[test]
fn mpk_matches_the_measured_preset() {
    let mpk = mpk_mini_mk2();
    assert_eq!(gi(&mpk.parse(&[0x90, 48, 100])), Some(("keys".into(), 48)));
    assert_eq!(gi(&mpk.parse(&[0xB0, 8, 64])), Some(("knobs".into(), 7)));
    let pc = mpk.parse(&[0xC0, 3, 0]).unwrap();
    assert_eq!((pc.group.as_str(), pc.index, pc.pressed), ("pads", 3, true));
    assert_eq!(
        mpk.parse(&[0x99, 36, 100]).map(|e| e.group),
        Some("padNotes".to_string())
    );
    let bend = mpk.parse(&[0xE0, 0x00, 0x40]).unwrap(); // centre = 8192
    assert_eq!(bend.group, "joystickX");
    assert!((bend.value - 8192.0 / 16383.0).abs() < 1e-9);
    assert_eq!(mpk.parse(&[0xB0, 77, 64]), None); // beats' factory map, not this preset
}

#[test]
fn inpulse_decodes_14_bit_and_shift_channels() {
    let inpulse = inpulse_200_mk3();
    let mut d = SurfaceDecoder::new(inpulse.clone());
    let msb = d.decode(&[0xB1, 0x00, 0x40]); // deck A volume, MSB
    assert_eq!(msb.and_then(|e| e.name), Some("volume".to_string()));
    let lsb = d.decode(&[0xB1, 0x20, 0x7F]).unwrap(); // LSB refines it
    assert_eq!(lsb.group, "deckA.faders");
    assert!((lsb.value - f64::from(0x40u32 << 7 | 0x7F) / 16383.0).abs() < 1e-12);
    assert_eq!(
        inpulse.parse(&[0x91, 0x07, 0x7F]).and_then(|e| e.name),
        Some("play".to_string())
    );
    let cue = inpulse.parse(&[0x92, 0x06, 0x7F]).unwrap();
    assert_eq!(
        (cue.group.as_str(), cue.name.as_deref()),
        ("deckB.buttons", Some("cue"))
    );
    assert_eq!(
        inpulse.parse(&[0x94, 0x09, 0x7F]).map(|e| e.group),
        Some("deckA.shiftButtons".into())
    );
    assert_eq!(
        inpulse.parse(&[0x96, 0x02, 0x7F]).map(|e| e.group),
        Some("deckA.hotcuePads".into())
    );
    assert_eq!(
        inpulse.parse(&[0xB0, 0x01, 0x7F]).map(|e| e.delta),
        Some(-1)
    ); // browser encoder
}

/// Phase 1 contract: every control in every profile is bound or parked on purpose — none
/// falls through.
#[test]
fn every_control_is_covered() {
    use djrust::control::surface::ControlType;
    for p in all_surfaces() {
        for g in &p.groups {
            let wide = p
                .layout
                .iter()
                .any(|b| b.group == g.id && b.index.is_none());
            for i in 0..g.control_numbers().len() {
                if wide {
                    break;
                }
                assert!(
                    p.layout
                        .iter()
                        .any(|b| b.group == g.id && b.index == Some(i)),
                    "{}: {}[{i}] has no layout entry",
                    p.name,
                    g.id
                );
            }
            if g.kind == ControlType::Pitchbend {
                assert!(
                    p.layout.iter().any(|b| b.group == g.id),
                    "{}: {} has no layout entry",
                    p.name,
                    g.id
                );
            }
        }
        let deckless: BTreeSet<DJActionKind> = [
            DJActionKind::Crossfader,
            DJActionKind::Master,
            DJActionKind::Shift,
            DJActionKind::Unbound,
            DJActionKind::Keys,
            DJActionKind::Nudge,
            DJActionKind::SeedStep,
            DJActionKind::HeadphoneMix,
            DJActionKind::HeadphoneLevel,
            DJActionKind::ClipToggle,
            DJActionKind::FocusDeck,
            DJActionKind::Wander,
            DJActionKind::WanderPace,
        ]
        .into_iter()
        .collect();
        for b in &p.layout {
            let g = p
                .group(&b.group)
                .unwrap_or_else(|| panic!("{}: layout names unknown group {}", p.name, b.group));
            if let (Some(i), true) = (b.index, g.kind != ControlType::Pitchbend) {
                assert!(
                    p.number(&b.group, i).is_some(),
                    "{}: {}[{i}] out of range",
                    p.name,
                    b.group
                );
            }
            if b.action == DJActionKind::Unbound {
                assert!(
                    b.reason.is_some(),
                    "{}: {} parked without a reason",
                    p.name,
                    b.group
                );
            }
            if !deckless.contains(&b.action) {
                assert!(b.deck.is_some(), "{}: {b:?}", p.name);
            }
        }
        let keys: Vec<String> = p
            .layout
            .iter()
            .map(|b| {
                format!(
                    "{}#{}#{}",
                    b.group,
                    b.index.map_or("*".to_string(), |i| i.to_string()),
                    b.shift.map_or("-".to_string(), |s| s.to_string())
                )
            })
            .collect();
        let set: BTreeSet<_> = keys.iter().collect();
        assert_eq!(set.len(), keys.len(), "{} binds a control twice", p.name);
    }
}

// MARK: the hardware oracle

/// Every distinct message the real hardware sent on 2026-10-07 decodes and lands on a layout
/// entry (jog messages go to the jog profile).
#[test]
fn every_captured_message_is_covered() {
    let caps = captured();
    let names: BTreeSet<&str> = caps.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["apc", "inpulse", "mpk"]));
    let jog = inpulse_200_mk3_jog();
    let mut checked = 0;
    let mut problems = Vec::new();
    for (dev, msgs) in &caps {
        let p = match dev.as_str() {
            "inpulse" => inpulse_200_mk3(),
            "apc" => apc_mini_mk2(),
            _ => mpk_mini_mk2(),
        };
        let mut d = SurfaceDecoder::new(p.clone());
        for m in msgs {
            if dev == "inpulse" && jog.parse(m).is_some() {
                continue;
            }
            if dev == "inpulse" && m[0] & 0xF0 == 0xB0 && [0x09, 0x0A].contains(&m[1]) && m[2] == 0
            {
                continue; // zero-tick jog
            }
            let Some(e) = d.decode(m) else {
                problems.push(format!("{dev}: {} does not decode", hexs(m)));
                continue;
            };
            let covered = p
                .layout
                .iter()
                .any(|b| b.group == e.group && (b.index.is_none() || b.index == Some(e.index)));
            if !covered {
                problems.push(format!(
                    "{dev}: {}[{}] decoded but has no layout entry",
                    e.group, e.index
                ));
            }
            checked += 1;
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(checked > 280, "only {checked} captured messages checked");
}

fn inpulse_captured() -> BTreeSet<(String, usize)> {
    let inpulse = inpulse_200_mk3();
    let jog = inpulse_200_mk3_jog();
    let mut d = SurfaceDecoder::new(inpulse);
    let mut out = BTreeSet::new();
    let (_, msgs) = captured()
        .into_iter()
        .find(|(k, _)| k == "inpulse")
        .unwrap();
    for m in msgs {
        if jog.parse(&m).is_some() {
            continue;
        }
        if let Some(e) = d.decode(&m) {
            out.insert((e.group, e.index));
        }
    }
    out
}

/// The Inpulse controls the real hardware sent that are still parked: each needs naming on
/// the unit first (ROADMAP Phase 1 hardware pass) or is a hardware layer.
#[test]
fn inpulse_captured_but_parked_is_the_known_set() {
    let inpulse = inpulse_200_mk3();
    let mut names = BTreeSet::new();
    for (group, index) in inpulse_captured() {
        let bs: Vec<_> = inpulse
            .layout
            .iter()
            .filter(|b| b.group == group && (b.index.is_none() || b.index == Some(index)))
            .collect();
        if !bs.is_empty() && bs.iter().all(|b| b.action == DJActionKind::Unbound) {
            let n = inpulse
                .group(&group)
                .and_then(|g| g.name(index))
                .unwrap_or("?")
                .to_string();
            names.insert(format!("{group}.{n}"));
        }
    }
    let want: BTreeSet<String> = [
        "deckA.shiftButtons.vinyl",
        "deckA.buttons.loopPadMode",
        "deckA.buttons.loopPadModeHeld",
        "deckA.buttons.hotCueMode",
        "deckA.buttons.fxMode",
        "deckB.buttons.hotCueMode",
        "deckB.buttons.fxMode",
        "mixer.buttons.centre01",
        "mixer.buttons.centre02",
        "mixer.buttons.centre03",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(names, want);
}

/// A binding or parked control that no captured message reaches rests on Mixxx's
/// original-200 map or a guess, and says so: `[MIXXX]` / `[UNVERIFIED]` in its group's
/// comment (or, for a parked control, its reason).
#[test]
fn inpulse_uncaptured_controls_are_marked() {
    let inpulse = inpulse_200_mk3();
    let captured = inpulse_captured();
    let mut bound = 0;
    for b in &inpulse.layout {
        if captured
            .iter()
            .any(|(g, i)| *g == b.group && (b.index.is_none() || b.index == Some(*i)))
        {
            continue;
        }
        let marks = [
            inpulse
                .group(&b.group)
                .and_then(|g| g.comment.clone())
                .unwrap_or_default(),
            b.reason.clone().unwrap_or_default(),
        ]
        .iter()
        .any(|s| s.contains("[MIXXX]") || s.contains("[UNVERIFIED]"));
        assert!(
            marks,
            "{}[{:?}] {:?} is not captured",
            b.group, b.index, b.action
        );
        if b.action != DJActionKind::Unbound {
            bound += 1;
        }
    }
    assert!(bound > 0); // the hot-cue clear pads at least: the check is live
}

// MARK: ControllerLayoutTests

#[test]
fn every_control_is_drawn() {
    for layout in ControllerLayout::all() {
        let profile = all_surfaces()
            .into_iter()
            .find(|p| p.name == layout.profile_name)
            .unwrap();
        let drawn: BTreeSet<String> = layout
            .spots
            .iter()
            .flat_map(|s| {
                s.bindings
                    .iter()
                    .map(|b| format!("{}/{}", b.group, b.index))
            })
            .collect();
        let missing: Vec<String> = profile
            .device_map()
            .iter()
            .map(|r| format!("{}/{}", r.group, r.index))
            .filter(|k| !drawn.contains(k))
            .filter(|k| {
                !k.starts_with("drumPads/") && !k.starts_with("keys/") && !k.contains("padBend")
            })
            .collect();
        assert!(
            missing.is_empty(),
            "{}: not drawn {missing:?}",
            layout.title
        );
    }
}

#[test]
fn spots_stay_on_their_device_and_apart() {
    for layout in ControllerLayout::all() {
        for s in &layout.spots {
            let cap = s.caption.clone().unwrap_or_default();
            assert!(s.x >= -0.01 && s.y >= -0.01, "{} {cap}", layout.title);
            assert!(
                s.x + s.w <= layout.width + 0.01 && s.y + s.h <= layout.height + 0.01,
                "{} {cap}",
                layout.title
            );
        }
        // no two controls on top of each other (centres at least 0.3 units apart)
        for (i, a) in layout.spots.iter().enumerate() {
            for b in &layout.spots[i + 1..] {
                let d = ((a.x + a.w / 2.0) - (b.x + b.w / 2.0)).abs()
                    + ((a.y + a.h / 2.0) - (b.y + b.h / 2.0)).abs();
                assert!(
                    d > 0.3,
                    "{}: {:?} over {:?}",
                    layout.title,
                    a.caption,
                    b.caption
                );
            }
        }
    }
}

#[test]
fn unverified_bindings_are_known() {
    let inpulse = inpulse_200_mk3();
    assert!(!inpulse.is_unverified("deckA.loopPads", 0), "measured");
    assert!(
        inpulse.is_unverified("deckB.loopPads", 0),
        "deck B mirrors A"
    );
    assert!(mpk_mini_mk2().is_unverified("padNotes", 9));
}

// MARK: ControlWalkTests

fn walk_rows() -> Vec<djrust::control::device_map::DeviceMapRow> {
    ["a", "b", "c"]
        .iter()
        .enumerate()
        .map(|(i, n)| djrust::control::device_map::DeviceMapRow {
            group: "g".into(),
            index: i,
            control: n.to_string(),
            number: Some(i as i64),
            action: "x".into(),
            shifted_action: None,
            parked: false,
        })
        .collect()
}

#[test]
fn target_advances_and_others_are_heard() {
    let mut w = ControlWalk::new("d", walk_rows());
    assert!(!w.feed("g", 2));
    assert_eq!(w.heard(), Some("g[2]"));
    assert!(w.feed("g", 0));
    assert_eq!(w.position(), 1);
    // the finished control keeps streaming: ignored
    assert!(!w.feed("g", 0));
    assert_eq!(w.heard(), None);
}

#[test]
fn skip_records_silent_or_wrong() {
    let mut w = ControlWalk::new("d", walk_rows());
    w.skip();
    assert_eq!(w.outcome("g", 0), Some(&WalkOutcome::Skipped));
    w.feed("g", 2);
    w.skip();
    assert_eq!(
        w.outcome("g", 1),
        Some(&WalkOutcome::HeardInstead("g[2]".into()))
    );
    assert!(w.feed("g", 2));
    assert!(w.is_done());
    assert_eq!(w.counts(), (1, 1, 1));
    assert!(w.report().contains("WRONG    g[1]"));
}

#[test]
fn back_undoes() {
    let mut w = ControlWalk::new("d", walk_rows());
    w.feed("g", 0);
    w.back();
    assert!(w.position() == 0 && w.outcome("g", 0).is_none());
}

#[test]
fn real_profile_has_rows() {
    for p in all_surfaces() {
        assert!(!ControlWalk::new(&p.name, p.device_map()).rows.is_empty());
    }
}

// MARK: DeviceMapTests

#[test]
fn every_control_of_every_device_has_a_row() {
    use djrust::control::surface::ControlType;
    for p in all_surfaces() {
        let rows = p.device_map();
        let expected: usize = p
            .groups
            .iter()
            .map(|g| {
                if g.kind == ControlType::Pitchbend {
                    1
                } else {
                    g.control_numbers().len()
                }
            })
            .sum();
        assert_eq!(rows.len(), expected, "{}", p.name);
        assert!(
            !rows.iter().any(|r| r.action == "unmapped"),
            "{}: a control fell through",
            p.name
        );
    }
}

#[test]
fn rows_read_like_the_layout() {
    let apc = apc_mini_mk2().device_map();
    let row = |g: &str, i: usize| {
        apc.iter()
            .find(|r| r.group == g && r.index == i)
            .unwrap()
            .clone()
    };
    assert_eq!(row("pads", 0).action, "macro drop · aimed deck");
    assert_eq!(row("pads", 8).action, "macro sweep-lp · aimed deck");
    // (the eleven pan, shape and aux-FX macros Swift leaves unbound are bound here, docs/AUX.md)
    assert_eq!(row("pads", 11).action, "macro delay-throw · aimed deck");
    assert_eq!(row("pads", 24).action, "macro ping-pong · aimed deck");
    assert_eq!(row("pads", 62).action, "macro pulse · aimed deck");
    assert_eq!(row("pads", 56).action, "stinger 1 · aimed deck");
    assert_eq!(row("track", 0).action, "mute kick · aimed deck");
    assert_eq!(row("scene", 3).action, "aim at deck A");
    assert_eq!(row("scene", 4).action, "aim at deck B");
    assert_eq!(row("scene", 7).action, "panic · aimed deck");
    // djrust's layout: the faders are the mixer, SHIFT makes them the FX unit
    assert_eq!(row("faders", 0).action, "volume · deck A");
    assert_eq!(row("faders", 2).action, "volume · deck L");
    assert_eq!(row("faders", 6).action, "crossfader");
    assert_eq!(
        row("faders", 0).shifted_action.as_deref(),
        Some("FX unit filter · aimed deck")
    );
    assert_eq!(
        row("faders", 4).shifted_action.as_deref(),
        Some("FX unit crush · aimed deck")
    );
    assert_eq!(
        row("faders", 7).shifted_action.as_deref(),
        Some("FX unit room · aimed deck")
    );
    assert_eq!(row("faders", 8).action, "master");
    assert_eq!(
        row("pads", 6).action,
        "FX unit reset (next beat) · aimed deck"
    );
    assert_eq!(
        row("pads", 7).action,
        "FX unit kill (next beat) · aimed deck"
    );
    // SHIFT turns the grid into the loop page, read like text from the top-left
    assert_eq!(
        row("pads", 56).shifted_action.as_deref(),
        Some("loop 1 in/out")
    );
    assert_eq!(
        row("pads", 63).shifted_action.as_deref(),
        Some("loop 8 in/out")
    );
    assert_eq!(
        row("pads", 48).shifted_action.as_deref(),
        Some("loop 9 in/out")
    );
    assert_eq!(
        row("pads", 0).shifted_action.as_deref(),
        Some("loop 57 in/out")
    );
    assert_eq!(
        row("pads", 7).shifted_action.as_deref(),
        Some("loop 64 in/out")
    );
    assert_eq!(row("shift", 0).action, "SHIFT");
    let mpk = mpk_mini_mk2().device_map();
    let find = |g: &str, i: Option<usize>| {
        mpk.iter()
            .find(|r| r.group == g && i.map_or(true, |i| r.index == i))
            .map(|r| r.action.clone())
    };
    assert_eq!(
        find("keys", Some(60)).as_deref(),
        Some("live keys (by key mode: a deck, the loops or the TD-3)")
    );
    assert_eq!(
        find("padNotes", Some(0)).as_deref(),
        Some("mute kick · aimed deck")
    );
    assert_eq!(
        find("joystickX", None).as_deref(),
        Some("tempo nudge (focus deck)")
    );
    let ip = inpulse_200_mk3().device_map();
    let ia = |g: &str, i: usize| {
        ip.iter()
            .find(|r| r.group == g && r.index == i)
            .map(|r| r.action.clone())
    };
    assert_eq!(ia("deckB.buttons", 2).as_deref(), Some("sync · deck B"));
    assert_eq!(
        ia("deckB.buttons", 7).as_deref(),
        Some("headphone cue · deck B")
    );
    assert_eq!(
        ia("deckA.shiftButtons", 0).as_deref(),
        Some("sync to bar · deck A")
    );
    // the browser wheel names the seed step (BrowserSeedTests.theDeviceMapNamesIt)
    let wheel = ip.iter().find(|r| r.group == "mixer.browser").unwrap();
    assert!(wheel.action.contains("seed"));
}

#[test]
fn parked_controls_say_why() {
    let rows: Vec<_> = inpulse_200_mk3()
        .device_map()
        .into_iter()
        .filter(|r| r.parked)
        .collect();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r.action.starts_with("unbound — ")));
}

#[test]
fn numbers_are_present() {
    let apc = apc_mini_mk2().device_map();
    assert_eq!(
        apc.iter()
            .find(|r| r.group == "pads" && r.index == 10)
            .unwrap()
            .number,
        Some(10)
    );
    assert_eq!(
        apc.iter()
            .find(|r| r.group == "track" && r.index == 3)
            .unwrap()
            .number,
        Some(103)
    );
}
