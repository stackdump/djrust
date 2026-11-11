//! The host-side replay path: raw bytes in, routed controls out, no hardware. The captured
//! messages are the real devices' (djengine/tests/fixtures/js/captured-messages.json).

use djrust::control::controller::{Action, SurfaceOutcome};
use djrust::control::router::RouteResult;
use djrust::json::JsonValue;
use djrust::host::controllers::{
    describe, find_surface, normalize_midi, replay, split_messages, ControllerSession,
};

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

const PORTS: [(&str, &str); 3] = [
    ("inpulse", "DJControl Inpulse 200 Mk3"),
    ("apc", "APC mini mk2 Control"),
    ("mpk", "MPK Mini Mk II"),
];

#[test]
fn normalisation_matches_coremidis_ump_conversion() {
    assert_eq!(
        normalize_midi(&[0x90, 0x3C, 0x7F]),
        Some([0x90, 0x3C, 0x7F])
    );
    assert_eq!(
        normalize_midi(&[0xC0, 0x03]),
        Some([0xC0, 0x03, 0x00]),
        "program change is padded"
    );
    assert_eq!(
        normalize_midi(&[0xD1, 0x40]),
        Some([0xD1, 0x40, 0x00]),
        "so is channel pressure"
    );
    assert_eq!(
        normalize_midi(&[0xB0, 0xFF, 0xFF]),
        Some([0xB0, 0x7F, 0x7F]),
        "7-bit data"
    );
    assert_eq!(normalize_midi(&[0xF8]), None, "clock");
    assert_eq!(normalize_midi(&[0xF0, 0x7E, 0xF7]), None, "sysex");
    assert_eq!(normalize_midi(&[0x90, 0x3C]), None, "truncated");
    assert_eq!(
        normalize_midi(&[0x3C, 0x40, 0x40]),
        None,
        "running status is the backend's job"
    );
}

#[test]
fn a_byte_stream_splits_at_status_bytes() {
    assert_eq!(
        split_messages(&[0x90, 0, 0x7F, 0x90, 0, 0, 0xC0, 3]),
        vec![vec![0x90, 0, 0x7F], vec![0x90, 0, 0], vec![0xC0, 3]]
    );
}

/// The whole hardware capture, routed the way the hub routes live input: every message of
/// every device decodes (jog messages go to the jog wheels), and nothing is a profile gap.
#[test]
fn the_hardware_capture_replays_without_a_gap() {
    let caps = captured();
    for (dev, port) in PORTS {
        let (_, msgs) = caps.iter().find(|(k, _)| k == dev).unwrap();
        let (session, routed) = replay(msgs.iter().map(|m| (port, m.as_slice())));
        assert_eq!(
            routed.len(),
            msgs.len(),
            "{dev}: every captured message is channel-voice MIDI"
        );
        let st = session.stats[port];
        // zero-tick jog messages parse as nothing and fall through to the surface: the
        // captures contain a few, and the hub counts them undecoded just the same
        let zero_tick_jog = msgs
            .iter()
            .filter(|m| {
                dev == "inpulse" && m[0] & 0xF0 == 0xB0 && [0x09, 0x0A].contains(&m[1]) && m[2] == 0
            })
            .count();
        assert_eq!(st.unmapped, 0, "{dev}: a profile gap");
        assert_eq!(st.undecoded, zero_tick_jog, "{dev}: {st:?}");
        assert_eq!(
            st.bound + st.unbound + st.jog + st.undecoded,
            msgs.len(),
            "{dev}: {st:?}"
        );
        assert!(st.bound > 0, "{dev}");
        if dev == "inpulse" {
            assert!(st.jog > 0, "the wheels are routed to the jog adapters");
        }
    }
}

#[test]
fn a_replayed_press_says_what_it_does() {
    let (session, routed) = replay([
        ("APC mini mk2 Control", &[0x90u8, 0x00, 0x7F][..]),
        ("APC mini mk2 Control", &[0x90, 0x74, 0x7F][..]), // aim at B
        ("APC mini mk2 Control", &[0x90, 0x00, 0x7F][..]),
        ("btw Bus 1", &[0x90, 0x00, 0x7F][..]), // someone else's port
    ]);
    assert_eq!(routed[0].text.as_deref(), Some("pads[0]=1.000 → macro *"));
    assert!(routed[0].actions.contains(&Action::Macro {
        deck: "A".into(),
        id: "drop".into()
    }));
    assert!(routed[1]
        .actions
        .contains(&Action::Focus { deck: "B".into() }));
    assert!(routed[2].actions.contains(&Action::Macro {
        deck: "B".into(),
        id: "drop".into()
    }));
    assert_eq!(routed[3].result, RouteResult::Ignored);
    assert_eq!(routed[3].text, None);
    assert_eq!(
        session.host.recorded.len(),
        3,
        "the unowned port is not recorded"
    );
    assert_eq!(
        session.devices,
        vec![(
            "APC mini mk2 Control".to_string(),
            "APC mini mk2".to_string()
        )]
    );
}

#[test]
fn leds_come_back_as_note_ons_and_only_when_they_change() {
    let mut s = ControllerSession::new();
    assert!(s.attach("APC mini mk2 Control"));
    assert!(!s.attach("Some Keyboard"));
    let frames = s.led_frames(100.0);
    assert_eq!(frames.len(), 1);
    let (port, msgs) = &frames[0];
    assert_eq!(port, "APC mini mk2 Control");
    assert_eq!(msgs.len(), 64 + 8 + 8);
    assert!(s.led_frames(100.0)[0].1.is_empty());
    // blackout resets the cache so the next frame lights everything again
    assert_eq!(s.blackouts()[0].1.len(), 80);
    assert_eq!(s.led_frames(101.0)[0].1.len(), 80);
}

#[test]
fn describe_reports_unmapped_and_soft_takeover_like_the_hub() {
    let mut s = ControllerSession::new();
    s.attach("MPK Mini Mk II");
    let r = s.route("MPK Mini Mk II", &[0xB0, 2, 64], 0.0).unwrap();
    assert!(matches!(
        r.result,
        RouteResult::Surface(_, SurfaceOutcome::Bound(_))
    ));
    assert_eq!(
        describe(&r.result).unwrap(),
        "knobs[1] knob2=0.504 → volume A"
    );
    // a program change arrives from midir as two bytes and still decodes
    let pc = s.route("MPK Mini Mk II", &[0xC0, 3], 0.0).unwrap();
    assert!(
        matches!(pc.result, RouteResult::Surface(..)),
        "{:?}",
        pc.result
    );
}

#[test]
fn profiles_are_found_by_id_or_name() {
    for (key, name) in [
        ("apc", "APC mini mk2"),
        ("apc-mini-mk2", "APC mini mk2"),
        ("MPK", "MPK Mini Mk II"),
        ("inpulse", "DJControl Inpulse 200 Mk3"),
        ("inpulse-200-mk3-surface", "DJControl Inpulse 200 Mk3"),
    ] {
        assert_eq!(
            find_surface(key).map(|p| p.name.clone()).as_deref(),
            Some(name),
            "{key}"
        );
    }
    assert!(find_surface("nope").is_none());
}
