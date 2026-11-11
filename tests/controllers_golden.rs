//! Swift golden for the controller decoders (`tools/paritydump`, section "controllers":
//! `swift/controllers.json`): `SurfaceDecoder.decode` and `JogProfile.parse` over every message of
//! the hardware capture (119 Inpulse, 203 APC, 65 MPK, replayed in order so the 14-bit MSB memory
//! is in play) and over a sweep the capture never held (seven channels x five statuses x ten first
//! data bytes x four second ones, and a few short or over-long messages). `control_decode.rs`
//! holds "every captured message decodes" and the Swift tests' literals; this compares what each
//! message decodes to, field by field, with Swift's own decode: group, index, value (as its float
//! bit pattern), pressed, isCC, delta, name, and the jog event.
//!
//! Not here (parts 2 and 3 of the audit's proposal): the LED frames after scripted state changes
//! and the JogAdapter's nudge stream on an engine.

mod common;
use common::*;
use djrust::control::jog::JogEvent;
use djrust::control::profiles;
use djrust::control::surface::{ControlEvent, SurfaceDecoder};
use djrust::json::JsonValue;

fn bytes(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

/// `eventString`: the decode as one string, or null.
fn event_string(e: Option<ControlEvent>) -> Option<String> {
    e.map(|e| {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            e.group,
            e.index,
            bits_hex(e.value),
            e.pressed,
            e.is_cc,
            e.delta,
            e.name.unwrap_or_default()
        )
    })
}

/// `jogString`.
fn jog_string(e: Option<JogEvent>) -> Option<String> {
    e.map(|e| match e {
        JogEvent::Touch { deck, shift, down } => format!("touch|{deck}|{shift}|{down}"),
        JogEvent::Scratch { deck, shift, ticks } => format!("scratch|{deck}|{shift}|{ticks}"),
        JogEvent::Bend { deck, shift, ticks } => format!("bend|{deck}|{shift}|{ticks}"),
    })
}

fn golden(v: &JsonValue) -> Option<String> {
    v.as_str().map(String::from)
}

fn check(d: &mut Diffs, device: &str, set: &str, rows: &[JsonValue], with_jog: bool) {
    let profile = match device {
        "inpulse" => profiles::inpulse_200_mk3(),
        "apc" => profiles::apc_mini_mk2(),
        "mpk" => profiles::mpk_mini_mk2(),
        other => panic!("{other}"),
    };
    let jog = with_jog.then(profiles::inpulse_200_mk3_jog);
    let mut decoder = SurfaceDecoder::new(profile);
    for r in rows {
        let r = r.as_array().unwrap();
        let hex = r[0].as_str().unwrap();
        let m = bytes(hex);
        let tag = format!("{device} {set} [{hex}]");
        d.eq(format!("{tag} event"), event_string(decoder.decode(&m)), golden(&r[1]));
        if let Some(j) = &jog {
            d.eq(format!("{tag} jog"), jog_string(j.parse(&m)), golden(&r[2]));
        }
    }
}

#[test]
fn the_decoders_read_every_message_as_swift_does() {
    let doc = fixture("controllers");
    let mut d = Diffs::default();
    for dev in rows(&doc, "devices") {
        let device = s(dev, "device");
        let with_jog = device == "inpulse";
        // the groups in the profile, in order, are the same list Swift loaded
        let profile = match device {
            "inpulse" => profiles::inpulse_200_mk3(),
            "apc" => profiles::apc_mini_mk2(),
            _ => profiles::mpk_mini_mk2(),
        };
        d.eq(
            format!("{device} profile name"),
            profile.name.clone(),
            s(dev, "profile").to_string(),
        );
        d.eq(
            format!("{device} groups"),
            profile.groups.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
            dev.get("groups").unwrap().strings().unwrap(),
        );
        check(&mut d, device, "captured", rows(dev, "captured"), with_jog);
        check(&mut d, device, "sweep", rows(dev, "sweep"), with_jog);
    }
    d.finish("controller decoders");
}
