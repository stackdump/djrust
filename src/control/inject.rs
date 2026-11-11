//! Software controls: the MIDI a drawn control sends, so clicking it on screen reaches the
//! binding layer exactly as the hardware would (docs/BINDINGS-MAP.md, "Clicking a control").
//!
//! [`encode`] is the inverse of [`SurfaceDecoder`](super::surface::SurfaceDecoder): a profile
//! control and a gesture become the channel-voice messages the device would put on its port.
//! The host feeds them to the router as it feeds the real port's, so learn, the lock keys,
//! the session recorder, the journal and the LEDs see no difference. A device that is not
//! plugged in is reached through a *software port*, [`software_port`], whose name every
//! profile matches (it contains the profile's name).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::control::jog::{Encoding, JogProfile};
use crate::control::surface::{ControlType, SurfaceProfile};

/// Velocity of a software press (a full hit, like the pads' loudest).
pub const PRESS_VELOCITY: u8 = 127;

/// What the pointer does to a control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// The button goes down (a note on, a CC at full, a program change).
    Press,
    /// The button comes up (a note off, a CC at zero).
    Release,
    /// A fader, knob or bend at `0...1`.
    Value(f64),
    /// A wheel's signed steps (a relative encoder).
    Delta(i64),
}

/// The name of the port a missing device is reached by. It contains the profile's name, so the
/// profile's `match` finds it, and no real port is called that.
pub fn software_port(profile_name: &str) -> String {
    format!("software:{profile_name}")
}

/// Is `port` one of the software ports?
pub fn is_software_port(port: &str) -> bool {
    port.starts_with("software:")
}

fn unit_to_7(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0 + 0.5) as u8
}

fn unit_to_14(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * 16383.0 + 0.5) as u16
}

/// The messages (normalised to three bytes, as `normalize_midi` leaves the hardware's) that
/// make `profile` decode `gesture` on control `index` of `group`. None if the group or the
/// index does not exist, or the gesture does not fit the control (a delta on a button).
///
/// - a note: `Press` is a note on at full velocity, `Release` a note off, `Value(v)` a note on
///   at velocity `v` (0 is a note off);
/// - a CC: `Value(v)` is the 7-bit value, `Press` full and `Release` zero;
/// - a 14-bit CC: the MSB then the LSB, as the device sends them;
/// - a relative CC: `Delta(n)`, two's complement;
/// - pitch bend: `Value(v)`, 14 bits, one control per channel (`Release` is the centre);
/// - a program change: `Press` only.
pub fn encode(
    profile: &SurfaceProfile,
    group: &str,
    index: usize,
    gesture: Gesture,
) -> Option<Vec<[u8; 3]>> {
    let g = profile.group(group)?;
    let ch = g.channel.unwrap_or(0) & 0x0F;
    let number = if g.kind == ControlType::Pitchbend {
        if index != 0 {
            return None;
        }
        0
    } else {
        *g.control_numbers().get(index)?
    };
    let out = match (g.kind, gesture) {
        (ControlType::Note, Gesture::Press) => alloc::vec![[0x90 | ch, number, PRESS_VELOCITY]],
        (ControlType::Note, Gesture::Release) => alloc::vec![[0x80 | ch, number, 0]],
        (ControlType::Note, Gesture::Value(v)) => match unit_to_7(v) {
            0 => alloc::vec![[0x80 | ch, number, 0]],
            vel => alloc::vec![[0x90 | ch, number, vel]],
        },
        (ControlType::Cc, Gesture::Press) => alloc::vec![[0xB0 | ch, number, 127]],
        (ControlType::Cc, Gesture::Release) => alloc::vec![[0xB0 | ch, number, 0]],
        (ControlType::Cc, Gesture::Value(v)) => alloc::vec![[0xB0 | ch, number, unit_to_7(v)]],
        (ControlType::Cc14, Gesture::Press | Gesture::Release | Gesture::Value(_)) => {
            let v = match gesture {
                Gesture::Press => 1.0,
                Gesture::Release => 0.0,
                Gesture::Value(v) => v,
                Gesture::Delta(_) => unreachable!(),
            };
            let v14 = unit_to_14(v);
            alloc::vec![
                [0xB0 | ch, number, (v14 >> 7) as u8],
                [0xB0 | ch, number.wrapping_add(32) & 0x7F, (v14 & 0x7F) as u8],
            ]
        }
        (ControlType::Relative, Gesture::Delta(n)) => {
            let n = n.clamp(-63, 63);
            alloc::vec![[0xB0 | ch, number, (n as i8 as u8) & 0x7F]]
        }
        (ControlType::Pitchbend, Gesture::Value(v)) => {
            let v14 = unit_to_14(v);
            alloc::vec![[0xE0 | ch, (v14 & 0x7F) as u8, (v14 >> 7) as u8]]
        }
        (ControlType::Pitchbend, Gesture::Release) => {
            // the stick springs back to its centre (8192)
            alloc::vec![[0xE0 | ch, 0, 0x40]]
        }
        (ControlType::Program, Gesture::Press) => alloc::vec![[0xC0 | ch, number, 0]],
        _ => return None,
    };
    Some(out)
}

/// What a hand does to a jog wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JogGesture {
    /// The platter is touched (true) or let go (false).
    Touch(bool),
    /// The platter is turned by signed ticks while touched (a scratch).
    Scratch(i64),
    /// The ring is nudged by signed ticks (a pitch bend).
    Bend(i64),
}

/// The message a jog wheel sends for `gesture` on `deck`, `shift` held or not
/// (the inverse of [`JogProfile::parse`]). None for a deck the profile does not know.
pub fn encode_jog(profile: &JogProfile, deck: &str, shift: bool, gesture: JogGesture) -> Option<[u8; 3]> {
    let ch = profile.deck_channel(deck)?;
    let ch = if shift { ch.wrapping_add(profile.shift_channel_offset) } else { ch } & 0x0F;
    let ticks = |t: i64| -> u8 {
        let t = t.clamp(-profile.max_magnitude.min(63), profile.max_magnitude.min(63));
        match profile.encoding {
            Encoding::Twos => (t as i8 as u8) & 0x7F,
            Encoding::Offset64 => (t + 0x40) as u8,
        }
    };
    Some(match gesture {
        JogGesture::Touch(true) => [0x90 | ch, profile.touch.note, profile.touch.on_velocity],
        JogGesture::Touch(false) if profile.touch.release_as_note_off => [0x80 | ch, profile.touch.note, 0],
        JogGesture::Touch(false) => [0x90 | ch, profile.touch.note, 0],
        JogGesture::Scratch(t) => [0xB0 | ch, profile.scratch_cc, ticks(t)],
        JogGesture::Bend(t) => [0xB0 | ch, profile.bend_cc, ticks(t)],
    })
}

/// A turn of `ticks` as the messages the wheel would send for it: one per `max_magnitude` ticks
/// (the Inpulse sends one tick a message), at most `cap` of them. `bend` nudges the ring,
/// otherwise the platter scratches.
pub fn encode_jog_turn(
    profile: &JogProfile,
    deck: &str,
    shift: bool,
    bend: bool,
    ticks: i64,
    cap: usize,
) -> Vec<[u8; 3]> {
    let step = profile.max_magnitude.clamp(1, 63);
    let n = ((ticks.abs() + step - 1) / step).min(cap as i64);
    let one = ticks.signum() * step;
    let g = |t| if bend { JogGesture::Bend(t) } else { JogGesture::Scratch(t) };
    (0..n)
        .filter_map(|_| encode_jog(profile, deck, shift, g(one)))
        .collect()
}

/// The gestures a drawn control of this type answers to, for the picture: a button is
/// clicked, a fader or knob dragged, a wheel scrolled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// Down on press, up on release.
    Button,
    /// A position, 0...1.
    Position,
    /// Signed steps.
    Steps,
}

/// How the pointer drives control `index` of `group` (None if the group does not exist).
pub fn drive_of(profile: &SurfaceProfile, group: &str) -> Option<Drive> {
    Some(match profile.group(group)?.kind {
        ControlType::Note | ControlType::Program => Drive::Button,
        ControlType::Cc | ControlType::Cc14 | ControlType::Pitchbend => Drive::Position,
        ControlType::Relative => Drive::Steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::profiles;
    use crate::control::surface::SurfaceDecoder;
    use alloc::sync::Arc;

    fn profiles_all() -> Vec<Arc<SurfaceProfile>> {
        profiles::all_surfaces()
    }

    /// Decode `msgs` the way the router does, keeping the last event.
    fn decode_all(p: &Arc<SurfaceProfile>, msgs: &[[u8; 3]]) -> Option<crate::control::ControlEvent> {
        let mut d = SurfaceDecoder::new(p.clone());
        let mut last = None;
        for m in msgs {
            last = d.decode(m).or(last);
        }
        last
    }

    /// An earlier group in the profile claims the same message (the decoder takes the first
    /// match), so this control cannot be reached under its own group: not an encoding fault.
    fn shadowed(p: &SurfaceProfile, group: &str, ev: &crate::control::ControlEvent) -> bool {
        let pos = |g: &str| p.groups.iter().position(|x| x.id == g).unwrap();
        ev.group != group && pos(&ev.group) < pos(group)
    }

    #[test]
    fn every_button_of_every_profile_decodes_back_to_itself() {
        let mut checked = 0;
        for p in profiles_all() {
            for g in &p.groups {
                if !matches!(g.kind, ControlType::Note | ControlType::Program) {
                    continue;
                }
                for i in 0..g.control_numbers().len() {
                    let msgs = encode(&p, &g.id, i, Gesture::Press).unwrap();
                    let ev = decode_all(&p, &msgs).unwrap_or_else(|| panic!("{} {}[{i}]", p.name, g.id));
                    if shadowed(&p, &g.id, &ev) {
                        continue;
                    }
                    assert_eq!((ev.group.as_str(), ev.index), (g.id.as_str(), i), "{}", p.name);
                    assert!(ev.pressed, "{} {}[{i}] pressed", p.name, g.id);
                    if g.kind == ControlType::Note {
                        let up = encode(&p, &g.id, i, Gesture::Release).unwrap();
                        let ev = decode_all(&p, &up).unwrap();
                        assert!(!ev.pressed, "{} {}[{i}] released", p.name, g.id);
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 300, "{checked}");
    }

    #[test]
    fn faders_knobs_and_bends_round_trip_their_value() {
        for p in profiles_all() {
            for g in &p.groups {
                if !matches!(g.kind, ControlType::Cc | ControlType::Cc14 | ControlType::Pitchbend) {
                    continue;
                }
                let n = if g.kind == ControlType::Pitchbend { 1 } else { g.control_numbers().len() };
                for i in 0..n {
                    for want in [0.0, 0.25, 0.5, 1.0] {
                        let msgs = encode(&p, &g.id, i, Gesture::Value(want)).unwrap();
                        let ev = decode_all(&p, &msgs).unwrap();
                        if shadowed(&p, &g.id, &ev) {
                            continue;
                        }
                        assert_eq!(ev.group, g.id, "{}", p.name);
                        assert_eq!(ev.index, i);
                        let tol = if g.kind == ControlType::Cc { 0.5 / 127.0 } else { 1.0 / 16383.0 };
                        assert!((ev.value - want).abs() <= tol + 1e-9, "{} {} {want}: {}", p.name, g.id, ev.value);
                    }
                }
            }
        }
    }

    #[test]
    fn a_14_bit_fader_sends_msb_then_lsb() {
        let p = profiles::inpulse_200_mk3();
        let m = encode(&p, "mixer.faders", 0, Gesture::Value(1.0)).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0][2], 127);
        assert_eq!(m[1][1], m[0][1] + 32);
        assert_eq!(m[1][2], 127);
    }

    #[test]
    fn wheel_steps_are_twos_complement() {
        let p = profiles::inpulse_200_mk3();
        for d in [1, 3, -1, -5, 63, -63] {
            let m = encode(&p, "mixer.browser", 0, Gesture::Delta(d)).unwrap();
            let ev = decode_all(&p, &m).unwrap();
            assert_eq!((ev.group.as_str(), ev.delta), ("mixer.browser", d));
        }
        // huge steps are clamped to what 7 bits carry
        let m = encode(&p, "mixer.browser", 0, Gesture::Delta(500)).unwrap();
        assert_eq!(decode_all(&p, &m).unwrap().delta, 63);
    }

    #[test]
    fn a_gesture_that_does_not_fit_is_refused() {
        let p = profiles::apc_mini_mk2();
        assert!(encode(&p, "pads", 0, Gesture::Delta(1)).is_none(), "delta on a pad");
        assert!(encode(&p, "pads", 64, Gesture::Press).is_none(), "no pad 64");
        assert!(encode(&p, "nope", 0, Gesture::Press).is_none());
        assert!(encode(&p, "faders", 0, Gesture::Delta(1)).is_none());
        let mpk = profiles::mpk_mini_mk2();
        assert!(encode(&mpk, "joystickX", 1, Gesture::Value(0.5)).is_none(), "one bend per channel");
        assert!(encode(&mpk, "pads", 0, Gesture::Release).is_none(), "a program change has no release");
    }

    #[test]
    fn a_bend_released_is_centred() {
        let p = profiles::mpk_mini_mk2();
        let m = encode(&p, "joystickX", 0, Gesture::Release).unwrap();
        let ev = decode_all(&p, &m).unwrap();
        // 8192 of 16383: what the hardware sends at rest
        assert!((ev.value - 0.5).abs() < 1e-3, "{}", ev.value);
    }

    #[test]
    fn every_profile_is_found_on_its_software_port() {
        for p in profiles_all() {
            assert!(p.matches(&software_port(&p.name)), "{}", p.name);
            assert!(is_software_port(&software_port(&p.name)));
        }
        assert!(!is_software_port("APC mini mk2"));
    }

    #[test]
    fn a_jog_wheel_gesture_parses_back_to_itself() {
        use crate::control::jog::JogEvent;
        let j = profiles::inpulse_200_mk3_jog();
        for deck in ["A", "B"] {
            for shift in [false, true] {
                let m = encode_jog(&j, deck, shift, JogGesture::Touch(true)).unwrap();
                assert_eq!(j.parse(&m), Some(JogEvent::Touch { deck: deck.into(), shift, down: true }));
                let m = encode_jog(&j, deck, shift, JogGesture::Touch(false)).unwrap();
                assert_eq!(j.parse(&m), Some(JogEvent::Touch { deck: deck.into(), shift, down: false }));
                let top = j.max_magnitude.min(63);
                for t in [1, -1, top, -top] {
                    let m = encode_jog(&j, deck, shift, JogGesture::Scratch(t)).unwrap();
                    assert_eq!(j.parse(&m), Some(JogEvent::Scratch { deck: deck.into(), shift, ticks: t }));
                    let m = encode_jog(&j, deck, shift, JogGesture::Bend(t)).unwrap();
                    assert_eq!(j.parse(&m), Some(JogEvent::Bend { deck: deck.into(), shift, ticks: t }));
                }
            }
        }
        assert!(encode_jog(&j, "C", false, JogGesture::Touch(true)).is_none());
        // a huge turn is clamped to what the wheel sends in one message
        let m = encode_jog(&j, "A", false, JogGesture::Scratch(5000)).unwrap();
        let top = j.max_magnitude.min(63);
        assert!(matches!(j.parse(&m), Some(JogEvent::Scratch { ticks, .. }) if ticks == top));
    }

    #[test]
    fn a_turn_is_as_many_messages_as_the_wheel_would_send() {
        use crate::control::jog::JogEvent;
        let j = profiles::inpulse_200_mk3_jog();
        let m = encode_jog_turn(&j, "B", false, false, -5, 64);
        assert_eq!(m.len(), 5);
        let total: i64 = m
            .iter()
            .map(|b| match j.parse(b) {
                Some(JogEvent::Scratch { ticks, .. }) => ticks,
                other => panic!("{other:?}"),
            })
            .sum();
        assert_eq!(total, -5);
        assert_eq!(encode_jog_turn(&j, "B", false, true, 3, 64).len(), 3);
        assert_eq!(encode_jog_turn(&j, "B", false, false, 1000, 8).len(), 8, "capped");
        assert!(encode_jog_turn(&j, "B", false, false, 0, 8).is_empty());
    }

    #[test]
    fn drives_follow_the_control_type() {
        let p = profiles::inpulse_200_mk3();
        assert_eq!(drive_of(&p, "deckA.buttons"), Some(Drive::Button));
        assert_eq!(drive_of(&p, "mixer.faders"), Some(Drive::Position));
        assert_eq!(drive_of(&p, "mixer.browser"), Some(Drive::Steps));
        assert_eq!(drive_of(&p, "nope"), None);
    }
}
