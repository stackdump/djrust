//! Controllers: the pure half of `Sources/DJCore`'s hardware layer. Profiles are data
//! (`data/profiles/*.json`), decoding is byte-for-byte, and what a control *does* comes out
//! as [`Action`] values for a host to apply. No I/O, no clock (callers pass `now`).
//!
//! | Swift | here |
//! |---|---|
//! | `JogProfile`, `JogEvent`, `JogAdapter` | [`jog`] |
//! | `Surface` (profile model, `SurfaceDecoder`) | [`surface`] |
//! | `SurfaceController`, `SurfaceOutcome`, `PadLight` | [`controller`] |
//! | `ControllerRouter` | [`router`] |
//! | `DeviceMap`, `ControlWalk` | [`device_map`], [`walk`] |
//! | `ControllerLayout` | [`layout`] |
//! | `DeviceLEDs` (`APCPalette`, `LEDSwatch`, `DeviceLights`) | [`leds`] |
//! | `RingSpin` | [`ring`] |
//! | `MacroChase` | [`chase`] |
//! | `NetMacro` / `MasterMacro` / `FxParam` ids | [`macros`] |

pub mod action_log;
pub mod bindmap;
pub mod chase;
pub mod controller;
pub mod device_map;
pub mod hwview;
pub mod import_beats;
pub mod inject;
pub mod jog;
pub mod layout;
pub mod leds;
pub mod macros;
pub mod map;
pub mod profiles;
pub mod regex;
pub mod registry;
pub mod ring;
pub mod router;
pub mod surface;
pub mod walk;

pub use controller::{
    Action, DeckMode, DeckView, LightLevel, LoopState, PadLight, SurfaceController, SurfaceHost,
    SurfaceOutcome,
};
pub use jog::{JogAdapter, JogClock, JogEvent, JogProfile};
pub use router::{ControllerRouter, RouteResult};
pub use surface::{ControlEvent, DJActionKind, SurfaceDecoder, SurfaceProfile};
