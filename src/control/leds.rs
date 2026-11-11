//! `DeviceLEDs.swift`: the live LED mirror of beats' device map (lib/ui/device-map.js
//! `paint`, with the LED protocol of lib/backend/apc-mini-mk2.js).
//!
//! `SurfaceController::lights` is the controller's own LED state as data. [`DeviceLights`]
//! carries that state to the device-map list, [`DeviceLEDs::swatch`] turns one LED into what
//! the row draws (colour, brightness, blink), and [`APCPalette`] pins device-map.js's
//! velocity-to-colour table (`PALETTE`) for the indices it knows.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::control::controller::{LightLevel, PadLight};
use crate::json::js_round;

/// A colour with components 0…1 (the `ScopeColor` of the Swift scopes, as much of it as the
/// LED mirror uses).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const fn new(r: f64, g: f64, b: f64) -> Rgba {
        Rgba { r, g, b, a: 1.0 }
    }

    /// `"#rrggbb"` or `"#rgb"`; None for anything else.
    pub fn from_hex(hex: &str) -> Option<Rgba> {
        let s = hex.strip_prefix('#')?;
        if s.len() != 6 && s.len() != 3 {
            return None;
        }
        let mut digits: Vec<u32> = Vec::new();
        for c in s.chars() {
            digits.push(c.to_digit(16)?);
        }
        if digits.len() == 3 {
            digits = digits.iter().flat_map(|d| [*d, *d]).collect();
        }
        let c = |i: usize| f64::from(digits[i] * 16 + digits[i + 1]) / 255.0;
        Some(Rgba::new(c(0), c(2), c(4)))
    }

    /// 0...255 components rounded as a canvas would store them (ties up, like
    /// `Math.round`).
    pub fn rgb8(&self) -> (i64, i64, i64) {
        fn byte(v: f64) -> i64 {
            js_round(v.clamp(0.0, 1.0) * 255.0) as i64
        }
        (byte(self.r), byte(self.g), byte(self.b))
    }

    /// HSB to RGB (the colour model `Color(hue:saturation:brightness:)` uses)
    /// (`SectionTimeline.hsb`).
    pub fn from_hsb(h: f64, s: f64, v: f64) -> Rgba {
        let i = (h * 6.0) as i64 % 6;
        let f = h * 6.0 - ((h * 6.0) as i64) as f64;
        let p = v * (1.0 - s);
        let q = v * (1.0 - f * s);
        let t = v * (1.0 - (1.0 - f) * s);
        match i {
            0 => Rgba::new(v, t, p),
            1 => Rgba::new(q, v, p),
            2 => Rgba::new(p, v, t),
            3 => Rgba::new(p, q, v),
            4 => Rgba::new(t, p, v),
            _ => Rgba::new(v, p, q),
        }
    }
}

/// The APC mini mk2's velocity-to-colour chart, as device-map.js draws it (`PALETTE`), and
/// the colours of its two single-LED columns.
pub struct APCPalette;

impl APCPalette {
    /// device-map.js `PALETTE`, by palette index. `0` is `transparent` (off), not listed.
    pub const ENTRIES: [(u8, &'static str, &'static str); 8] = [
        (3, "white", "#ffffff"),
        (5, "red", "#ff2d2d"),
        (9, "orange", "#ff8c00"),
        (13, "yellow", "#ffd400"),
        (21, "green", "#19c34d"),
        (37, "cyan", "#1ec8ff"),
        (45, "blue", "#3a5cff"),
        (53, "magenta", "#d63ad6"),
    ];

    /// Indices dj lights that beats never does: the SHIFT loop page's violet.
    pub const EXTRAS: [(u8, &'static str, &'static str); 1] = [(48, "violet", "#b98cff")];

    /// The pinned colour of an index; None for off (0) and for an index neither beats nor
    /// dj draws.
    pub fn pinned(index: u8) -> Option<Rgba> {
        Self::ENTRIES
            .iter()
            .chain(Self::EXTRAS.iter())
            .find(|e| e.0 == index)
            .and_then(|e| Rgba::from_hex(e.2))
    }

    /// Any index as a colour: the pinned one, else a hue picked from the index (a stray
    /// index shows something rather than nothing). Off (0) is black.
    pub fn color(index: u8) -> Rgba {
        if index == 0 {
            return Rgba::new(0.0, 0.0, 0.0);
        }
        if let Some(c) = Self::pinned(index) {
            return c;
        }
        Rgba::from_hsb(f64::from(index % 64) / 64.0, 0.8, 0.9)
    }

    pub fn track_on() -> Rgba {
        Rgba::from_hex("#ff2d2d").unwrap()
    }
    pub fn track_off() -> Rgba {
        Rgba::from_hex("#1a0a0a").unwrap()
    }
    pub fn scene_on() -> Rgba {
        Rgba::from_hex("#19c34d").unwrap()
    }
    pub fn scene_off() -> Rgba {
        Rgba::from_hex("#0a1a0a").unwrap()
    }
    /// `.pn-dm-off` / the unlit scene button: `opacity: 0.4`.
    pub const OFF_OPACITY: f64 = 0.4;
}

/// One LED as a row of the device map draws it (`LEDSwatch`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LEDSwatch {
    pub color: Rgba,
    /// Brightness while steady (1 full, less for a dim RGB pad or an unlit single LED).
    pub opacity: f64,
    /// Whether the LED is on at all (off draws as a dark, faint dot).
    pub lit: bool,
    /// `pn-dm-blink`: alternates between full and 0.3.
    pub blinks: bool,
}

impl LEDSwatch {
    /// The opacity at time `t` seconds on any steady clock.
    pub fn opacity_at(&self, t: f64) -> f64 {
        if self.blinks {
            DeviceLEDs::blink_opacity(t)
        } else {
            self.opacity
        }
    }
}

pub struct DeviceLEDs;

impl DeviceLEDs {
    /// `animation: pn-dm-blink 600ms infinite`.
    pub const BLINK_PERIOD: f64 = 0.6;
    /// How bright a dim RGB pad is drawn: the hardware's own 50 % (`PAD_DIM = 0x92`).
    pub const DIM_BRIGHTNESS: f64 = 0.5;
    /// How bright a `dim` single LED is drawn.
    pub const DIM_SINGLE_OPACITY: f64 = 0.4;

    /// `@keyframes pn-dm-blink`: `0%, 49% { opacity: 1 } 50%, 100% { opacity: 0.3 }`.
    pub fn blink_opacity(t: f64) -> f64 {
        let x = t / Self::BLINK_PERIOD;
        let phase = x - libm::trunc(x); // truncatingRemainder(dividingBy: 1)
        let p = if phase < 0.0 { phase + 1.0 } else { phase };
        if p < 0.5 {
            1.0
        } else {
            0.3
        }
    }

    /// The colour of a single-colour LED group on the APC: the red track row, the green
    /// scene column. Any other single-LED group has no colour in the profile: white.
    pub fn single_colors(group: &str) -> (Rgba, Rgba) {
        match group {
            "track" => (APCPalette::track_on(), APCPalette::track_off()),
            "scene" => (APCPalette::scene_on(), APCPalette::scene_off()),
            _ => (Rgba::new(1.0, 1.0, 1.0), Rgba::new(0.1, 0.1, 0.1)),
        }
    }

    /// One LED of `group`. `rgb`: the group is an RGB grid (its `color_index` picks the
    /// colour); otherwise a single LED whose colour comes from the group.
    pub fn swatch(light: PadLight, group: &str, rgb: bool) -> LEDSwatch {
        if rgb {
            if light.color_index == 0 || light.level == LightLevel::Off {
                return LEDSwatch {
                    color: APCPalette::color(0),
                    opacity: 0.0,
                    lit: false,
                    blinks: false,
                };
            }
            let k = if light.level == LightLevel::Dim {
                Self::DIM_BRIGHTNESS
            } else {
                1.0
            };
            return LEDSwatch {
                color: APCPalette::color(light.color_index),
                opacity: k,
                lit: true,
                blinks: light.level == LightLevel::Blink,
            };
        }
        let c = Self::single_colors(group);
        match light.level {
            LightLevel::Off => LEDSwatch {
                color: c.1,
                opacity: APCPalette::OFF_OPACITY,
                lit: false,
                blinks: false,
            },
            LightLevel::Dim => LEDSwatch {
                color: c.0,
                opacity: Self::DIM_SINGLE_OPACITY,
                lit: true,
                blinks: false,
            },
            LightLevel::Full => LEDSwatch {
                color: c.0,
                opacity: 1.0,
                lit: true,
                blinks: false,
            },
            LightLevel::Blink => LEDSwatch {
                color: c.0,
                opacity: 1.0,
                lit: true,
                blinks: true,
            },
        }
    }
}

/// What every LED of one controller shows right now (`DeviceLights`).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceLights {
    /// LED state by group id, one entry per control of the group.
    pub groups: BTreeMap<String, Vec<PadLight>>,
    /// The groups whose LEDs take a palette colour.
    pub rgb_groups: BTreeSet<String>,
}

impl DeviceLights {
    /// The LED of control `index` of `group`, None where the control has none.
    pub fn swatch(&self, group: &str, index: usize) -> Option<LEDSwatch> {
        let g = self.groups.get(group)?;
        let light = *g.get(index)?;
        Some(DeviceLEDs::swatch(
            light,
            group,
            self.rgb_groups.contains(group),
        ))
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Does any LED blink (the list then needs a clock)?
    pub fn any_blinking(&self) -> bool {
        self.groups
            .values()
            .any(|g| g.iter().any(|l| l.level == LightLevel::Blink))
    }
}
