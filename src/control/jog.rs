//! Jog wheels: `JogProfile.swift`, `JogEvent.swift` and `JogAdapter.swift`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::regex::{PatternError, PortPattern};
use crate::json::JsonValue;

/// `JogProfile.Touch`.
#[derive(Clone, Debug, PartialEq)]
pub struct Touch {
    pub note: u8,
    pub on_velocity: u8,
    pub release_as_note_off: bool,
}

/// `JogProfile.Encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 0x01 = +1, 0x7F = -1 (7-bit two's complement)
    Twos,
    /// 0x41 = +1, 0x3F = -1 (centred on 0x40)
    Offset64,
}

/// A DJ controller's jog-wheel map (`JogProfile`), loaded from
/// `data/profiles/<id>.json`.
#[derive(Clone, Debug)]
pub struct JogProfile {
    pub name: String,
    pub manufacturer: String,
    /// Case-insensitive pattern matched against the MIDI port name.
    pub match_pattern: PortPattern,
    /// Deck name -> MIDI channel (0-based status nibble), sorted by deck name.
    pub decks: Vec<(String, u8)>,
    pub shift_channel_offset: u8,
    pub touch: Touch,
    pub scratch_cc: u8,
    pub bend_cc: u8,
    pub max_magnitude: i64,
    pub encoding: Encoding,
    pub ticks_per_rev: i64,
    pub platter_rpm: f64,
    pub usb_frame_ms: f64,
    pub coast_tau_sec: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileError(pub String);

impl core::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "profile: {}", self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ProfileError {}

impl From<PatternError> for ProfileError {
    fn from(e: PatternError) -> Self {
        ProfileError(e.to_string())
    }
}

pub(crate) fn req<'a>(j: &'a JsonValue, key: &str) -> Result<&'a JsonValue, ProfileError> {
    j.get(key)
        .ok_or_else(|| ProfileError(alloc::format!("missing {key:?}")))
}

pub(crate) fn req_str(j: &JsonValue, key: &str) -> Result<String, ProfileError> {
    req(j, key)?
        .as_str()
        .map(String::from)
        .ok_or_else(|| ProfileError(alloc::format!("{key:?} is not a string")))
}

pub(crate) fn req_f64(j: &JsonValue, key: &str) -> Result<f64, ProfileError> {
    req(j, key)?
        .as_f64()
        .ok_or_else(|| ProfileError(alloc::format!("{key:?} is not a number")))
}

pub(crate) fn req_u8(j: &JsonValue, key: &str) -> Result<u8, ProfileError> {
    let n = req_f64(j, key)?;
    if (0.0..=255.0).contains(&n) && n == libm::trunc(n) {
        Ok(n as u8)
    } else {
        Err(ProfileError(alloc::format!("{key:?} is not a byte")))
    }
}

impl JogProfile {
    /// Decode a profile JSON document.
    pub fn from_json(text: &str) -> Result<JogProfile, ProfileError> {
        let j = JsonValue::parse(text).map_err(|e| ProfileError(e.to_string()))?;
        let decks_obj = req(&j, "decks")?
            .as_object()
            .ok_or_else(|| ProfileError("decks is not an object".to_string()))?;
        let mut decks: Vec<(String, u8)> = Vec::new();
        for (k, v) in decks_obj.iter() {
            let ch = v
                .as_f64()
                .filter(|n| (0.0..=15.0).contains(n))
                .ok_or_else(|| ProfileError(alloc::format!("deck {k:?} channel")))?;
            decks.push((k.to_string(), ch as u8));
        }
        // Sorted so the result does not depend on dictionary order (as parse's loop does).
        decks.sort_by(|a, b| a.0.cmp(&b.0));
        let t = req(&j, "touch")?;
        let encoding = match req_str(&j, "encoding")?.as_str() {
            "twos" => Encoding::Twos,
            "offset64" => Encoding::Offset64,
            other => return Err(ProfileError(alloc::format!("encoding {other:?}"))),
        };
        Ok(JogProfile {
            name: req_str(&j, "name")?,
            manufacturer: req_str(&j, "manufacturer")?,
            match_pattern: PortPattern::new(&req_str(&j, "match")?)?,
            decks,
            shift_channel_offset: req_u8(&j, "shiftChannelOffset")?,
            touch: Touch {
                note: req_u8(t, "note")?,
                on_velocity: req_u8(t, "onVelocity")?,
                release_as_note_off: req(t, "releaseAsNoteOff")?.as_bool().unwrap_or(false),
            },
            scratch_cc: req_u8(&j, "scratchCC")?,
            bend_cc: req_u8(&j, "bendCC")?,
            max_magnitude: req_f64(&j, "maxMagnitude")? as i64,
            encoding,
            ticks_per_rev: req_f64(&j, "ticksPerRev")? as i64,
            platter_rpm: req_f64(&j, "platterRpm")?,
            usb_frame_ms: req_f64(&j, "usbFrameMs")?,
            coast_tau_sec: req_f64(&j, "coastTauSec")?,
        })
    }

    /// Seconds of 1x audio that one encoder tick represents.
    pub fn seconds_per_tick(&self) -> f64 {
        60.0 / self.platter_rpm / self.ticks_per_rev as f64
    }

    pub fn matches(&self, port_name: &str) -> bool {
        self.match_pattern.is_match(port_name)
    }

    /// The deck's MIDI channel, from the (sorted) deck table.
    pub fn deck_channel(&self, deck: &str) -> Option<u8> {
        self.decks.iter().find(|(d, _)| d == deck).map(|(_, c)| *c)
    }

    /// Returns None for anything that is not a jog message on one of this profile's decks
    /// (`JogProfile.parse`, the port of `parseJogMessage` in web/jog/inpulse-200-mk3.js).
    pub fn parse(&self, data: &[u8]) -> Option<JogEvent> {
        if data.len() < 3 {
            return None;
        }
        let kind = data[0] & 0xF0;
        let channel = data[0] & 0x0F;
        let mut found: Option<(&str, bool)> = None;
        for (name, ch) in &self.decks {
            if channel == *ch {
                found = Some((name, false));
                break;
            }
            if channel == ch.wrapping_add(self.shift_channel_offset) {
                found = Some((name, true));
                break;
            }
        }
        let (deck, shift) = found?;
        let deck = deck.to_string();
        if (kind == 0x90 || kind == 0x80) && data[1] == self.touch.note {
            return Some(JogEvent::Touch {
                deck,
                shift,
                down: kind == 0x90 && data[2] > 0,
            });
        }
        if kind == 0xB0 && (data[1] == self.scratch_cc || data[1] == self.bend_cc) {
            let ticks = decode_relative(data[2], self.encoding);
            if ticks == 0 {
                return None;
            }
            return Some(if data[1] == self.scratch_cc {
                JogEvent::Scratch { deck, shift, ticks }
            } else {
                JogEvent::Bend { deck, shift, ticks }
            });
        }
        None
    }
}

/// A jog-wheel event, decoded from raw MIDI 1.0 bytes (`JogEvent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JogEvent {
    Touch {
        deck: String,
        shift: bool,
        down: bool,
    },
    Scratch {
        deck: String,
        shift: bool,
        ticks: i64,
    },
    Bend {
        deck: String,
        shift: bool,
        ticks: i64,
    },
}

impl JogEvent {
    pub fn deck(&self) -> &str {
        match self {
            JogEvent::Touch { deck, .. }
            | JogEvent::Scratch { deck, .. }
            | JogEvent::Bend { deck, .. } => deck,
        }
    }
}

/// `decodeRelative`.
pub fn decode_relative(value: u8, encoding: Encoding) -> i64 {
    match encoding {
        Encoding::Offset64 => i64::from(value) - 0x40,
        Encoding::Twos => {
            if value < 0x40 {
                i64::from(value)
            } else {
                i64::from(value) - 0x80
            }
        }
    }
}

/// `encodeRelative`.
pub fn encode_relative(ticks: i64, encoding: Encoding) -> u8 {
    match encoding {
        Encoding::Offset64 => ((0x40 + ticks) & 0x7F) as u8,
        Encoding::Twos => (ticks & 0x7F) as u8,
    }
}

/// The default SHIFT coarse-seek factor on a scratch ([`JogAdapter::new`]).
pub const SHIFT_MULTIPLIER: f64 = 4.0;

/// What a jog wheel drives (`JogClock`; the `clock` interface in web/jog/jog-adapter.js).
pub trait JogClock {
    /// Touch-on: hand is on the platter, stop the clock.
    fn freeze(&mut self);
    /// Rotation while touched: move the playhead by this much 1x time.
    fn scrub_by(&mut self, seconds: f64);
    /// Touch-off: the clock runs on its own again.
    fn release(&mut self);
    /// Outer ring: run at (1 + ratio)x; 0 = normal.
    fn nudge(&mut self, ratio: f64);
}

/// Jog events -> clock control for one deck (`JogAdapter`). Knows the device (through a
/// profile) and time; knows nothing about audio. The clock is passed to each call instead
/// of being owned, so the host keeps its decks.
///
/// Scrub is expressed as position deltas, not speed: a wheel returned to the angle it
/// started at puts the playhead back exactly where it started.
#[derive(Clone, Debug)]
pub struct JogAdapter {
    pub deck: String,
    touching: bool,
    nudge: f64,
    seconds_per_tick: f64,
    shift_multiplier: f64,
    bend_per_tick: f64,
    bend_tau_sec: f64,
    bend_max: f64,
    last_sent: f64,
    last_decay: Option<f64>,
}

impl JogAdapter {
    /// Defaults: shift ×4 (coarse seek, as in Mixxx/Serato), bend depth 0.10, tau 0.15 s,
    /// max 0.5.
    pub fn new(profile: &JogProfile, deck: &str) -> JogAdapter {
        JogAdapter::with_options(profile, deck, SHIFT_MULTIPLIER, 0.10, 0.15, 0.5)
    }

    pub fn with_options(
        profile: &JogProfile,
        deck: &str,
        shift_multiplier: f64,
        bend_depth: f64,
        bend_tau_sec: f64,
        bend_max: f64,
    ) -> JogAdapter {
        JogAdapter {
            deck: deck.to_string(),
            touching: false,
            nudge: 0.0,
            seconds_per_tick: profile.seconds_per_tick(),
            shift_multiplier,
            // With exponential decay of bendTauSec, the level settles at bendDepth when
            // ticks arrive at platter speed.
            bend_per_tick: bend_depth
                / (bend_tau_sec * profile.ticks_per_rev as f64 * (profile.platter_rpm / 60.0)),
            bend_tau_sec,
            bend_max,
            last_sent: 0.0,
            last_decay: None,
        }
    }

    pub fn touching(&self) -> bool {
        self.touching
    }

    pub fn nudge(&self) -> f64 {
        self.nudge
    }

    pub fn handle(&mut self, event: &JogEvent, clock: &mut dyn JogClock) {
        match event {
            JogEvent::Touch { deck, down, .. } => {
                if *deck != self.deck || *down == self.touching {
                    return;
                }
                self.touching = *down;
                if *down {
                    self.nudge = 0.0;
                    clock.freeze();
                } else {
                    clock.release();
                }
            }
            JogEvent::Scratch { deck, shift, ticks } => {
                if *deck != self.deck {
                    return;
                }
                // Scratch with no touch: the note-on was missed or arrives in another
                // order than assumed. Bending is the safe reading.
                if !self.touching {
                    self.bend(*ticks);
                    return;
                }
                let mult = if *shift { self.shift_multiplier } else { 1.0 };
                clock.scrub_by(*ticks as f64 * self.seconds_per_tick * mult);
            }
            JogEvent::Bend { deck, ticks, .. } => {
                if *deck != self.deck {
                    return;
                }
                self.bend(*ticks);
            }
        }
    }

    /// Bend is a rate, so it needs a time base the message stream stops supplying once the
    /// ring stops. Call every ~10 ms.
    pub fn decay(&mut self, now: f64, clock: &mut dyn JogClock) {
        let dt = self.last_decay.map_or(0.0, |l| now - l);
        self.last_decay = Some(now);
        self.nudge *= libm::exp(-dt / self.bend_tau_sec);
        if self.nudge.abs() < 1e-4 {
            self.nudge = 0.0;
        }
        if self.nudge != self.last_sent {
            self.last_sent = self.nudge;
            clock.nudge(self.nudge);
        }
    }

    fn bend(&mut self, ticks: i64) {
        self.nudge =
            (self.nudge + ticks as f64 * self.bend_per_tick).clamp(-self.bend_max, self.bend_max);
    }
}
