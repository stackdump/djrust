//! `ControllerLayout.swift`: where each control sits on the real hardware, for the
//! Controllers view: a drawing of every attached device with each control labelled by what
//! it is bound to. Geometry is in "units" (about one APC pad), origin top-left; positions
//! follow the panels (APC: Akai's grid with the track row below and the scene column right;
//! MPK: joystick, 2×4 pads, 2×4 knobs over 25 keys; Inpulse: deck A, the mixer, deck B). A
//! spot carries every binding the control has, one per mode (an Inpulse pad is a hot cue, a
//! loop, an FX…, depending on the pad mode).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::profiles;

/// `ControlSpot.Kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotKind {
    Pad,
    Button,
    Knob,
    Fader,
    Jog,
    Joystick,
    Keyboard,
    Encoder,
    /// A socket on the unit (a trigger or foot input), drawn round.
    Jack,
}

/// A profile control this physical control is: `(group, index, mode label)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotBinding {
    pub group: String,
    pub index: usize,
    pub mode: Option<String>,
}

fn sb(group: &str, index: usize, mode: Option<&str>) -> SpotBinding {
    SpotBinding {
        group: group.to_string(),
        index,
        mode: mode.map(String::from),
    }
}

/// One physical control (`ControlSpot`).
#[derive(Clone, Debug, PartialEq)]
pub struct ControlSpot {
    pub kind: SpotKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// The first is the one the drawing labels; the others show in its help.
    pub bindings: Vec<SpotBinding>,
    /// Printed on the panel (drawn small, above the binding).
    pub caption: Option<String>,
    /// A [`SpotKind::Keyboard`]: the first index and the number of keys, in the group of the
    /// first binding (a key's index is its note number).
    pub keys: Option<(usize, usize)>,
    /// Which of a pair this is ("A", "B" for a deck's jog), for what has no binding to say it.
    pub tag: Option<String>,
}

impl ControlSpot {
    /// A control of the panel that sends no MIDI (a knob on the unit, a switch): drawn, never
    /// bound.
    pub fn is_panel_only(&self) -> bool {
        self.bindings.is_empty() && self.kind != SpotKind::Jog
    }
}

fn spot(
    kind: SpotKind,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    bindings: Vec<SpotBinding>,
    caption: Option<&str>,
) -> ControlSpot {
    ControlSpot {
        kind,
        x,
        y,
        w,
        h,
        bindings,
        caption: caption.map(String::from),
        keys: None,
        tag: None,
    }
}

/// A device drawn as it looks (`ControllerLayout`).
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerLayout {
    /// The surface profile's name.
    pub profile_name: String,
    /// A short name for the card.
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub spots: Vec<ControlSpot>,
}

impl ControllerLayout {
    /// The devices the app knows, in the order the Controllers view shows them.
    pub fn all() -> Vec<ControllerLayout> {
        alloc::vec![
            Self::apc_mini_mk2(),
            Self::mpk_mini_mk2(),
            Self::inpulse_200_mk3(),
            Self::td3(),
            Self::pd705(),
            Self::computer_keyboard()
        ]
    }

    pub fn layout_for(profile_name: &str) -> Option<ControllerLayout> {
        Self::all()
            .into_iter()
            .find(|l| l.profile_name == profile_name)
    }

    /// The Behringer TD-3: seven knobs and the switches along the top, the 13 keys below. Only
    /// the keys can send MIDI (and that not yet seen on a unit: `[UNVERIFIED]` in the
    /// profile), so everything else is drawn dim and unbound, for where it is.
    pub fn td3() -> ControllerLayout {
        let mut s = Vec::new();
        for (i, name) in ["VOLUME", "TUNE", "CUTOFF", "RESO", "ENV MOD", "DECAY", "ACCENT"]
            .into_iter()
            .enumerate()
        {
            s.push(spot(
                SpotKind::Knob,
                0.6 + i as f64 * 1.7,
                0.4,
                0.95,
                0.95,
                Vec::new(),
                Some(name),
            ));
        }
        s.push(spot(
            SpotKind::Button,
            12.6,
            0.7,
            1.1,
            0.5,
            Vec::new(),
            Some("SAW / SQR"),
        ));
        for (i, name) in ["BACK", "PATTERN", "TEMPO", "TAP", "WRITE", "RUN / STOP"]
            .into_iter()
            .enumerate()
        {
            s.push(spot(
                SpotKind::Button,
                0.6 + i as f64 * 2.2,
                2.2,
                1.7,
                0.6,
                Vec::new(),
                Some(name),
            ));
        }
        for (i, name) in ["DN", "UP", "ACCENT", "SLIDE"].into_iter().enumerate() {
            s.push(spot(
                SpotKind::Button,
                0.6 + i as f64 * 1.45,
                3.15,
                1.2,
                0.5,
                Vec::new(),
                Some(name),
            ));
        }
        // pattern C = note 36 (C2); the 13 keys C .. C
        let mut keys = spot(
            SpotKind::Keyboard,
            0.6,
            4.0,
            9.6,
            2.2,
            alloc::vec![sb("keys", 36, None)],
            Some("keys"),
        );
        keys.keys = Some((36, 13));
        s.push(keys);
        ControllerLayout {
            profile_name: profiles::td3().name.clone(),
            title: "TD-3".into(),
            width: 14.0,
            height: 6.5,
            spots: s,
        }
    }

    /// The Lekato PD705: nine pads in a 3×3 block, the four trigger jacks along the top and the
    /// two foot jacks on the right. Pad 1 is top left and they count along the rows (a
    /// schematic: the unit's numbering is by its note map).
    pub fn pd705() -> ControllerLayout {
        let mut s = Vec::new();
        for i in 0..4usize {
            s.push(spot(
                SpotKind::Jack,
                0.7 + i as f64 * 1.5,
                0.3,
                0.7,
                0.7,
                alloc::vec![sb("triggers", i, None)],
                Some(&format!("TRIG {}", i + 1)),
            ));
        }
        for i in 0..9usize {
            s.push(spot(
                SpotKind::Pad,
                0.3 + (i % 3) as f64 * 2.0,
                1.5 + (i / 3) as f64 * 2.0,
                1.85,
                1.85,
                alloc::vec![sb("pads", i, None)],
                Some(&format!("PAD {}", i + 1)),
            ));
        }
        for i in 0..2usize {
            s.push(spot(
                SpotKind::Jack,
                6.9,
                2.0 + i as f64 * 1.8,
                0.8,
                0.8,
                alloc::vec![sb("feet", i, None)],
                Some(&format!("FOOT {}", i + 1)),
            ));
        }
        ControllerLayout {
            profile_name: profiles::pd705().name.clone(),
            title: "PD705".into(),
            width: 8.2,
            height: 7.7,
            spots: s,
        }
    }

    /// The computer keyboard, the keys the profile names, in their rows.
    pub fn computer_keyboard() -> ControllerLayout {
        let p = profiles::keyboard();
        let names: Vec<String> = p
            .group("keys")
            .and_then(|g| g.names.clone())
            .unwrap_or_default();
        let mut s = Vec::new();
        for (i, k) in names.iter().enumerate() {
            let (x, y, w, h) = match key_place(k) {
                Some(p) => p,
                None => continue,
            };
            let caption = match k.as_str() {
                "Space" => "SPACE".to_string(),
                "Tab" => "TAB".to_string(),
                "Enter" => "ENTER".to_string(),
                "Left" => "◀".to_string(),
                "Right" => "▶".to_string(),
                "Up" => "▲".to_string(),
                "Down" => "▼".to_string(),
                other => other.to_string(),
            };
            s.push(spot(
                SpotKind::Pad,
                x,
                y,
                w,
                h,
                alloc::vec![sb("keys", i, None)],
                Some(&caption),
            ));
        }
        ControllerLayout {
            profile_name: p.name.clone(),
            title: "Computer keyboard".into(),
            width: 13.2,
            height: 5.0,
            spots: s,
        }
    }

    /// A device with no drawing: the controls it has bound or has been seen to move, in rows
    /// by group (notes as pads, CCs as knobs, the pitch wheel as a fader, programs as
    /// buttons). Move a control and it appears.
    pub fn generic(profile: &crate::control::surface::SurfaceProfile, seen: &[(String, usize)]) -> ControllerLayout {
        use crate::control::surface::ControlType;
        const PER_ROW: usize = 12;
        let mut wanted: Vec<(usize, String, usize)> = Vec::new(); // group order, group, index
        let mut want = |g: &str, i: usize| {
            if let Some(gi) = profile.groups.iter().position(|x| x.id == g) {
                if !wanted.iter().any(|(_, g2, i2)| g2 == g && *i2 == i) {
                    wanted.push((gi, g.to_string(), i));
                }
            }
        };
        for b in &profile.layout {
            if let Some(i) = b.index {
                want(&b.group, i);
            }
        }
        for (g, i) in seen {
            want(g, *i);
        }
        wanted.sort();
        let mut s = Vec::new();
        let mut row = 0.0_f64;
        let mut col = 0usize;
        let mut last_group: Option<String> = None;
        for (_, g, i) in &wanted {
            if last_group.as_ref() != Some(g) {
                if last_group.is_some() {
                    row += if col > 0 { 1.6 } else { 0.0 } + 0.4;
                }
                col = 0;
                last_group = Some(g.clone());
            } else if col == PER_ROW {
                col = 0;
                row += 1.6;
            }
            let group = profile.group(g);
            let kind = match group.map(|g| g.kind) {
                Some(ControlType::Cc | ControlType::Cc14) => SpotKind::Knob,
                Some(ControlType::Relative) => SpotKind::Encoder,
                Some(ControlType::Pitchbend) => SpotKind::Fader,
                Some(ControlType::Program) => SpotKind::Button,
                _ => SpotKind::Pad,
            };
            let number = group
                .and_then(|g| g.control_numbers().get(*i).copied())
                .map_or(String::new(), |n| format!("{n}"));
            let caption = match group.map(|g| g.kind) {
                Some(ControlType::Note) => format!("note {number}"),
                Some(ControlType::Cc | ControlType::Cc14 | ControlType::Relative) => format!("CC {number}"),
                Some(ControlType::Program) => format!("PC {number}"),
                _ => "bend".to_string(),
            };
            let (w, h) = if kind == SpotKind::Fader { (1.4, 0.45) } else { (1.2, 1.2) };
            s.push(spot(
                kind,
                col as f64 * 1.4,
                row,
                w,
                h,
                alloc::vec![sb(g, *i, None)],
                Some(&caption),
            ));
            col += 1;
        }
        let rows_used = if wanted.is_empty() { 0.0 } else { row + 1.6 };
        ControllerLayout {
            profile_name: profile.name.clone(),
            title: profile.name.trim_start_matches(profiles::GENERIC_PREFIX).to_string(),
            width: PER_ROW as f64 * 1.4,
            height: rows_used.max(2.0),
            spots: s,
        }
    }

    /// 8×8 grid (pad 0 bottom-left, +1 right, +8 up), 8 track buttons below, 8 scene buttons
    /// right (top to bottom), SHIFT under the scene column, 9 faders along the bottom.
    pub fn apc_mini_mk2() -> ControllerLayout {
        let mut s = Vec::new();
        for i in 0..64usize {
            s.push(spot(
                SpotKind::Pad,
                (i % 8) as f64,
                (7 - i / 8) as f64,
                0.92,
                0.92,
                alloc::vec![sb("pads", i, None)],
                None,
            ));
        }
        for i in 0..8usize {
            s.push(spot(
                SpotKind::Button,
                i as f64,
                8.15,
                0.92,
                0.5,
                alloc::vec![sb("track", i, None)],
                None,
            ));
            s.push(spot(
                SpotKind::Button,
                8.25,
                i as f64 + 0.1,
                0.72,
                0.72,
                alloc::vec![sb("scene", i, None)],
                None,
            ));
        }
        s.push(spot(
            SpotKind::Button,
            8.25,
            8.15,
            0.72,
            0.5,
            alloc::vec![sb("shift", 0, None)],
            Some("SHIFT"),
        ));
        for i in 0..9usize {
            s.push(spot(
                SpotKind::Fader,
                if i == 8 { 8.35 } else { i as f64 + 0.2 },
                8.95,
                0.52,
                2.6,
                alloc::vec![sb("faders", i, None)],
                None,
            ));
        }
        ControllerLayout {
            profile_name: profiles::apc_mini_mk2().name.clone(),
            title: "APC mini mk2".into(),
            width: 9.05,
            height: 11.6,
            spots: s,
        }
    }

    /// Joystick top-left, 8 pads (5-8 over 1-4) in the middle, 8 knobs (1-4 over 5-8)
    /// right, 25 keys below. A pad is a program change, a bank A note or a bank B note, by
    /// pad mode.
    pub fn mpk_mini_mk2() -> ControllerLayout {
        let mut s = alloc::vec![spot(
            SpotKind::Joystick,
            0.2,
            0.3,
            1.7,
            1.7,
            alloc::vec![sb("joystickX", 0, None)],
            Some("joystick")
        )];
        for n in 0..8usize {
            let col = (n % 4) as f64;
            let row = if n < 4 { 1.35 } else { 0.2 }; // pads 1-4 are the bottom row
            s.push(spot(
                SpotKind::Pad,
                2.3 + col * 1.25,
                row,
                1.12,
                1.05,
                alloc::vec![
                    sb("pads", n, Some("PROG")),
                    sb("padNotes", n, Some("NOTE A")),
                    sb("padNotes", n + 8, Some("NOTE B")),
                ],
                Some(&format!("pad {}", n + 1)),
            ));
        }
        for k in 0..8usize {
            let col = (k % 4) as f64;
            let row = if k < 4 { 0.25 } else { 1.35 };
            s.push(spot(
                SpotKind::Knob,
                7.6 + col * 1.05,
                row,
                0.9,
                0.9,
                alloc::vec![sb("knobs", k, None)],
                Some(&format!("K{}", k + 1)),
            ));
        }
        let mut keys = spot(
            SpotKind::Keyboard,
            0.2,
            2.75,
            11.6,
            2.1,
            alloc::vec![sb("keys", 60, None)],
            Some("keys"),
        );
        // 25 keys, C3 to C5 at the default octave (measured: 48-72)
        keys.keys = Some((48, 25));
        s.push(keys);
        ControllerLayout {
            profile_name: profiles::mpk_mini_mk2().name.clone(),
            title: "MPK mini mk2".into(),
            width: 12.0,
            height: 5.05,
            spots: s,
        }
    }

    /// Deck A, the mixer, deck B. A deck: the jog, its pitch fader, the LOOP / STEMS /
    /// pad-mode buttons, four pads (numbered down the columns: pad 1 = index 0, the one
    /// under it index 1), SHIFT / SYNC / CUE / PLAY. The mixer: each deck's EQ and filter
    /// knobs, volume fader and headphone cue, the browser and LOAD buttons, master /
    /// headphones and the crossfader.
    pub fn inpulse_200_mk3() -> ControllerLayout {
        let mut s: Vec<ControlSpot> = Vec::new();
        for (deck, x0, strip) in [("A", 0.0_f64, 5.0_f64), ("B", 10.6, 9.1)] {
            let b = format!("deck{deck}.buttons");
            let sh = format!("deck{deck}.shiftButtons");
            let f = format!("deck{deck}.faders");
            // buttons index: loop 0, shift 1, sync 2, cue 3, play 4, stemsMusic 5,
            // stemsMic 6, pfl 7, load 8, hotCueMode 9, fxMode 10, loopPadMode 11,
            // loopPadModeHeld 12
            let mut jog = spot(
                SpotKind::Jog,
                x0 + 0.3,
                0.3,
                3.9,
                3.9,
                Vec::new(),
                Some("jog: scratch · bend · SHIFT seek"),
            );
            jog.tag = Some(deck.to_string());
            s.push(jog);
            let pitch_x = if deck == "A" { x0 + 4.35 } else { x0 - 0.5 };
            s.push(spot(
                SpotKind::Fader,
                pitch_x,
                0.4,
                0.42,
                3.6,
                alloc::vec![sb(&f, 5, None)],
                Some("pitch"),
            ));
            // SHIFT + STEMS are Mixxx's loop in / out (0x09 / 0x0A on the shift channel);
            // SHIFT + HOT CUE selects LOOP pad mode (0x11, with 0x40 while it is held)
            let top: [(usize, &str, Vec<SpotBinding>); 5] = [
                (0, "LOOP", alloc::vec![sb(&sh, 5, Some("SHIFT"))]),
                (5, "♪ STEMS", alloc::vec![sb(&sh, 3, Some("SHIFT"))]),
                (6, "MIC STEMS", alloc::vec![sb(&sh, 4, Some("SHIFT"))]),
                (
                    9,
                    "HOT CUE",
                    alloc::vec![sb(&b, 11, Some("SHIFT")), sb(&b, 12, Some("SHIFT, held"))],
                ),
                (10, "FX", Vec::new()),
            ];
            for (k, (idx, caption, extra)) in top.into_iter().enumerate() {
                let mut bs = alloc::vec![sb(&b, idx, None)];
                bs.extend(extra);
                s.push(spot(
                    SpotKind::Button,
                    x0 + 0.15 + k as f64 * 0.85,
                    4.45,
                    0.78,
                    0.5,
                    bs,
                    Some(caption),
                ));
            }
            for i in 0..4usize {
                let col = (i / 2) as f64;
                let row = (i % 2) as f64;
                let p = format!("deck{deck}");
                s.push(spot(
                    SpotKind::Pad,
                    x0 + 0.6 + col * 1.6,
                    5.15 + row * 1.15,
                    1.45,
                    1.0,
                    alloc::vec![
                        sb(&format!("{p}.hotcuePads"), i, Some("HOT CUE")),
                        sb(&format!("{p}.loopPads"), i, Some("LOOP")),
                        sb(&format!("{p}.rollPads"), i, Some("FX")),
                        sb(&format!("{p}.samplerPads"), i, Some("SAMPLER")),
                        sb(&format!("{p}.hotcueClearPads"), i, Some("SHIFT+HOT CUE")),
                        sb(&format!("{p}.fxPads"), i, Some("FX (old 200)")),
                    ],
                    None,
                ));
            }
            let bottom: [(usize, &str, Option<usize>); 4] = [
                (1, "SHIFT", None),
                (2, "SYNC", Some(0)),
                (3, "CUE", Some(1)),
                (4, "PLAY", Some(2)),
            ];
            for (k, (idx, caption, shifted)) in bottom.into_iter().enumerate() {
                let mut bs = alloc::vec![sb(&b, idx, None)];
                if let Some(si) = shifted {
                    bs.push(sb(&sh, si, Some("SHIFT")));
                }
                s.push(spot(
                    if idx == 4 || idx == 3 {
                        SpotKind::Pad
                    } else {
                        SpotKind::Button
                    },
                    x0 + 0.15 + k as f64 * 1.05,
                    7.6,
                    0.9,
                    0.75,
                    bs,
                    Some(caption),
                ));
            }
            // the mixer strip: high (gain on SHIFT), mid, low, filter, the volume fader, PFL
            let knobs: [(usize, &str); 4] = [(4, "HIGH"), (3, "MID"), (2, "LOW"), (1, "FILTER")];
            for (k, (idx, caption)) in knobs.into_iter().enumerate() {
                let mut bs = alloc::vec![sb(&f, idx, None)];
                if idx == 4 {
                    bs.push(sb(&format!("deck{deck}.shiftFaders"), 0, Some("SHIFT")));
                }
                s.push(spot(
                    SpotKind::Knob,
                    strip,
                    0.35 + k as f64 * 1.0,
                    0.8,
                    0.8,
                    bs,
                    Some(caption),
                ));
            }
            s.push(spot(
                SpotKind::Fader,
                strip + 0.2,
                4.4,
                0.42,
                2.7,
                alloc::vec![sb(&f, 0, None)],
                Some("vol"),
            ));
            s.push(spot(
                SpotKind::Button,
                strip,
                7.3,
                0.8,
                0.45,
                alloc::vec![sb(&b, 7, None)],
                Some("PFL"),
            ));
            s.push(spot(
                SpotKind::Button,
                strip,
                7.9,
                0.8,
                0.45,
                alloc::vec![sb(&b, 8, None)],
                Some("LOAD"),
            ));
        }
        s.push(spot(
            SpotKind::Encoder,
            6.9,
            0.35,
            1.1,
            1.1,
            alloc::vec![
                sb("mixer.browser", 0, None),
                sb("mixer.buttons", 0, Some("push")),
                sb("mixer.shiftBrowser", 0, Some("SHIFT")),
                sb("mixer.shiftButtons", 0, Some("SHIFT + push")),
            ],
            Some("browser"),
        ));
        s.push(spot(
            SpotKind::Knob,
            6.3,
            1.75,
            0.8,
            0.8,
            alloc::vec![sb("mixer.faders", 1, None)],
            Some("master"),
        ));
        s.push(spot(
            SpotKind::Knob,
            7.8,
            1.75,
            0.8,
            0.8,
            alloc::vec![sb("mixer.faders", 2, None)],
            Some("phones"),
        ));
        for (c, caption) in ["ASSIST", "MASTER", "GUIDE"].into_iter().enumerate() {
            s.push(spot(
                SpotKind::Button,
                6.25 + c as f64 * 0.8,
                3.0,
                0.7,
                0.45,
                alloc::vec![sb("mixer.buttons", c + 1, None)],
                Some(caption),
            ));
        }
        s.push(spot(
            SpotKind::Fader,
            6.2,
            7.75,
            2.5,
            0.42,
            alloc::vec![sb("mixer.faders", 0, None)],
            Some("crossfader"),
        ));
        ControllerLayout {
            profile_name: profiles::inpulse_200_mk3().name.clone(),
            title: "Inpulse 200 MK3".into(),
            width: 15.1,
            height: 8.55,
            spots: s,
        }
    }
}


/// Where a computer key sits: `(x, y, w, h)` in key units, rows staggered as on a keyboard.
fn key_place(name: &str) -> Option<(f64, f64, f64, f64)> {
    const DIGITS: &str = "12345678";
    const TOP: &str = "QWERTYUI";
    const HOME: &str = "ASDFGHJK";
    const BOTTOM: &str = "ZXCVBNM";
    if let Some(i) = DIGITS.find(name).filter(|_| name.len() == 1) {
        return Some((1.0 + i as f64, 0.0, 0.92, 0.92));
    }
    if let Some(i) = TOP.find(name).filter(|_| name.len() == 1) {
        return Some((1.5 + i as f64, 1.0, 0.92, 0.92));
    }
    if let Some(i) = HOME.find(name).filter(|_| name.len() == 1) {
        return Some((1.75 + i as f64, 2.0, 0.92, 0.92));
    }
    if let Some(i) = BOTTOM.find(name).filter(|_| name.len() == 1) {
        return Some((2.25 + i as f64, 3.0, 0.92, 0.92));
    }
    Some(match name {
        "Tab" => (0.0, 1.0, 1.42, 0.92),
        "Enter" => (9.75, 2.0, 1.75, 0.92),
        "Space" => (3.0, 4.0, 5.0, 0.92),
        "Left" => (9.25, 4.0, 0.92, 0.92),
        "Down" => (10.25, 4.0, 0.92, 0.92),
        "Right" => (11.25, 4.0, 0.92, 0.92),
        "Up" => (10.25, 3.0, 0.92, 0.92),
        _ => return None,
    })
}
