//! The bundled device profiles (`Resources/profiles/*.json`, loaded by `profileURL`
//! in Swift). Profiles are data: `data/profiles/<id>.json` is beats-btw-dj's file
//! verbatim, embedded with `include_str!`.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use crate::control::jog::{JogProfile, ProfileError};
use crate::control::surface::SurfaceProfile;

pub const INPULSE_200_MK3_JSON: &str = include_str!("../../data/profiles/inpulse-200-mk3.json");
pub const INPULSE_200_MK3_SURFACE_JSON: &str =
    include_str!("../../data/profiles/inpulse-200-mk3-surface.json");
pub const APC_MINI_MK2_JSON: &str = include_str!("../../data/profiles/apc-mini-mk2.json");
pub const MPK_MINI_MK2_JSON: &str = include_str!("../../data/profiles/mpk-mini-mk2.json");
/// The Behringer TD-3, as far as it sends anything (docs/BINDINGS-MAP.md).
pub const TD3_JSON: &str = include_str!("../../data/profiles/td3.json");
/// The Lekato PD705 drum pad's pads, trigger and foot inputs.
pub const PD705_JSON: &str = include_str!("../../data/profiles/pd705.json");
/// The computer keyboard as a controller: the software fallback.
pub const KEYBOARD_JSON: &str = include_str!("../../data/profiles/keyboard.json");

/// A ready-made binding set for the MPK: keys transpose the loops, bank A pads are loop
/// clips 1-8 (NOTE-mode pads), bank B pads mute the aimed deck's parts, knobs 2 and 3 are the
/// loop channel's volume and filter. Preset "decks" is the shipped profile unchanged.
pub const MPK_PERFORMANCE_MAP_JSON: &str = include_str!("../../data/maps/mpk-performance.json");

/// The profile ids Swift's `SurfaceProfile.load` / `JogProfile.load` take.
pub fn source(id: &str) -> Option<&'static str> {
    Some(match id {
        "inpulse-200-mk3" => INPULSE_200_MK3_JSON,
        "inpulse-200-mk3-surface" => INPULSE_200_MK3_SURFACE_JSON,
        "apc-mini-mk2" => APC_MINI_MK2_JSON,
        "mpk-mini-mk2" => MPK_MINI_MK2_JSON,
        "td3" => TD3_JSON,
        "pd705" => PD705_JSON,
        "keyboard" => KEYBOARD_JSON,
        _ => return None,
    })
}

/// `JogProfile.load(id)`.
pub fn load_jog(id: &str) -> Result<JogProfile, ProfileError> {
    JogProfile::from_json(
        source(id).ok_or_else(|| ProfileError(alloc::format!("no profile {id:?}")))?,
    )
}

/// `SurfaceProfile.load(id)`.
pub fn load_surface(id: &str) -> Result<SurfaceProfile, ProfileError> {
    SurfaceProfile::from_json(
        source(id).ok_or_else(|| ProfileError(alloc::format!("no profile {id:?}")))?,
    )
}

macro_rules! bundled_surface {
    ($name:ident, $cell:ident, $id:literal) => {
        static $cell: OnceBox<Arc<SurfaceProfile>> = OnceBox::new();
        pub fn $name() -> Arc<SurfaceProfile> {
            $cell
                .get_or_init(|| {
                    Box::new(Arc::new(
                        load_surface($id)
                            .unwrap_or_else(|e| panic!("bundled profile {}: {e}", $id)),
                    ))
                })
                .clone()
        }
    };
}

bundled_surface!(apc_mini_mk2, APC, "apc-mini-mk2");
bundled_surface!(mpk_mini_mk2, MPK, "mpk-mini-mk2");
bundled_surface!(inpulse_200_mk3, INPULSE, "inpulse-200-mk3-surface");
bundled_surface!(td3, TD3, "td3");
bundled_surface!(pd705, PD705, "pd705");
bundled_surface!(keyboard, KEYBOARD, "keyboard");

static JOG: OnceBox<Arc<JogProfile>> = OnceBox::new();

/// `JogProfile.inpulse200MK3`.
pub fn inpulse_200_mk3_jog() -> Arc<JogProfile> {
    JOG.get_or_init(|| {
        Box::new(Arc::new(
            load_jog("inpulse-200-mk3").unwrap_or_else(|e| panic!("bundled profile: {e}")),
        ))
    })
    .clone()
}

/// `SurfaceProfile.all`: APC mini mk2, MPK Mini mk2, Inpulse 200 MK3, then the devices the
/// port added (TD-3, PD705, the computer keyboard).
pub fn all_surfaces() -> Vec<Arc<SurfaceProfile>> {
    alloc::vec![
        apc_mini_mk2(),
        mpk_mini_mk2(),
        inpulse_200_mk3(),
        td3(),
        pd705(),
        keyboard()
    ]
}

/// The name prefix of a generic profile.
pub const GENERIC_PREFIX: &str = "Generic MIDI: ";

/// Is `name` a profile made by [`generic`]?
pub fn is_generic(name: &str) -> bool {
    name.starts_with(GENERIC_PREFIX)
}

/// A profile for a MIDI device nobody wrote one for, so that any port can be learned and
/// mapped. It claims everything a controller can send, one control per number: `notes` (the
/// note on/off of any channel), `cc` (a 7-bit CC of any channel), `bend` (the pitch wheel) and
/// `program` (program changes). Its layout is empty: bind a control with MIDI learn or in the
/// Bindings window, and the map keeps it. The `match` is the port's name, as written
/// (case-insensitively, as a substring), so the profile is found on that port and on nothing
/// else.
///
/// The group channel is `None`: the decoder takes the first message that fits, so a device
/// that puts two things on one number on different channels learns them as one control. That is
/// the price of a profile that needs no knowledge of the device.
pub fn generic(port: &str) -> SurfaceProfile {
    use crate::control::regex::PortPattern;
    use crate::control::surface::{ControlType, Group};
    let group = |id: &str, kind: ControlType, count: Option<u8>, comment: &str| Group {
        id: id.into(),
        kind,
        first: count.map(|_| 0),
        count,
        numbers: None,
        names: None,
        channel: None,
        comment: Some(comment.into()),
    };
    let mut bend = group("bend", ControlType::Pitchbend, None, "the pitch wheel (one control)");
    bend.names = Some(alloc::vec!["bend".into()]);
    SurfaceProfile {
        name: alloc::format!("{GENERIC_PREFIX}{port}"),
        manufacturer: "unknown".into(),
        match_pattern: PortPattern::literal(port),
        match_comment: Some("this port's name".into()),
        source: Some(
            "Made for a device with no profile (docs/BINDINGS-MAP.md, \"Unknown devices\"). \
             Controls are the MIDI numbers 0-127 of any channel."
                .into(),
        ),
        jog: None,
        groups: alloc::vec![
            group("notes", ControlType::Note, Some(128), "notes 0-127, any channel"),
            group("cc", ControlType::Cc, Some(128), "CC 0-127, any channel"),
            bend,
            group("program", ControlType::Program, Some(128), "program change 0-127"),
        ],
        leds: None,
        layout: Vec::new(),
    }
}
