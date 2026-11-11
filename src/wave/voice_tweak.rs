//! Port of `Beats/Wave/VoiceTweak.swift`: the performer's own shaping of one channel's
//! synth voice (the sound panel). Each field set replaces that part of the instrument's
//! voice (`InstrumentTable`), each `None` leaves the instrument's own. Empty = the
//! instrument exactly.
//!
//! A poke like any other, applied live to the playing song (`WaveRunner::set_voice`, at
//! the next block). Session-only: never written into the share payload or the canonical
//! text.

use alloc::string::String;
use alloc::vec::Vec;

use super::synth::{biquad, VoiceSpec};
use crate::json::{JsonObject, JsonValue};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VoiceTweak {
    /// Gain on the instrument's level, dB.
    pub gain_db: Option<f64>,
    /// Attack time constant, seconds.
    pub attack: Option<f64>,
    /// A note's decay time constant as a multiple of its length…
    pub decay_scale: Option<f64>,
    /// …at most this long, seconds.
    pub max_decay: Option<f64>,
    /// Brightness: the post lowpass on the voice sum, Hz (0 = off).
    pub lowpass: Option<f64>,
    /// Drive: Tone.Distortion amount, 0…1 (0 = off).
    pub distortion: Option<f64>,
    pub vibrato_rate: Option<f64>,
    pub vibrato_depth: Option<f64>,
    /// Pitch sweep, applied as a group: a tweak that sets one sets all three.
    pub pitch_from: Option<f64>,
    pub pitch_to: Option<f64>,
    pub pitch_time: Option<f64>,
}

impl VoiceTweak {
    /// The JSON keys, in the panel's order.
    pub const KEYS: [&'static str; 11] = [
        "gainDB",
        "attack",
        "decayScale",
        "maxDecay",
        "lowpass",
        "distortion",
        "vibratoRate",
        "vibratoDepth",
        "pitchFrom",
        "pitchTo",
        "pitchTime",
    ];

    /// Where each value may go (anything else is clamped into it).
    pub fn range(key: &str) -> Option<(f64, f64)> {
        Some(match key {
            "gainDB" => (-48.0, 24.0),
            "attack" => (0.0005, 4.0),
            "decayScale" => (0.05, 20.0),
            "maxDecay" => (0.03, 10.0),
            "lowpass" => (0.0, 20_000.0),
            "distortion" => (0.0, 1.0),
            "vibratoRate" => (0.0, 20.0),
            "vibratoDepth" => (0.0, 0.1),
            "pitchFrom" | "pitchTo" => (0.05, 32.0),
            "pitchTime" => (0.0, 4.0),
            _ => return None,
        })
    }

    fn slot(&mut self, key: &str) -> Option<&mut Option<f64>> {
        Some(match key {
            "gainDB" => &mut self.gain_db,
            "attack" => &mut self.attack,
            "decayScale" => &mut self.decay_scale,
            "maxDecay" => &mut self.max_decay,
            "lowpass" => &mut self.lowpass,
            "distortion" => &mut self.distortion,
            "vibratoRate" => &mut self.vibrato_rate,
            "vibratoDepth" => &mut self.vibrato_depth,
            "pitchFrom" => &mut self.pitch_from,
            "pitchTo" => &mut self.pitch_to,
            "pitchTime" => &mut self.pitch_time,
            _ => return None,
        })
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        let mut c = *self;
        c.slot(key).and_then(|s| *s)
    }

    /// `subscript(key) = newValue`: set (clamped into its range) or clear a field.
    pub fn set(&mut self, key: &str, value: Option<f64>) {
        let v = value.map(|v| Self::clamp(key, v));
        if let Some(s) = self.slot(key) {
            *s = v;
        }
    }

    fn clamp(key: &str, v: f64) -> f64 {
        let Some((lo, hi)) = Self::range(key) else {
            return 0.0;
        };
        if !v.is_finite() {
            return lo;
        }
        // a cutoff below 40 Hz is "off"
        if key == "lowpass" && v < 40.0 {
            return 0.0;
        }
        v.max(lo).min(hi)
    }

    pub fn is_empty(&self) -> bool {
        Self::KEYS.iter().all(|k| self.get(k).is_none())
    }

    /// Does it set the pitch sweep?
    pub fn sets_pitch(&self) -> bool {
        self.pitch_from.is_some() || self.pitch_to.is_some() || self.pitch_time.is_some()
    }

    /// `voice` with this tweak on it (what the panel shows, and what the lanes play).
    pub fn applied_to(&self, voice: &VoiceSpec) -> VoiceSpec {
        let mut v = voice.clone();
        if let Some(g) = self.gain_db {
            v.level = voice.level * libm::pow(10.0, g / 20.0);
        }
        if let Some(x) = self.attack {
            v.attack = x;
        }
        if let Some(x) = self.decay_scale {
            v.decay_scale = x;
        }
        if let Some(x) = self.max_decay {
            v.max_decay = x;
        }
        if let Some(x) = self.lowpass {
            v.lowpass = x;
        }
        if let Some(x) = self.distortion {
            v.distortion = x;
        }
        if let Some(x) = self.vibrato_rate {
            v.vibrato_rate = x;
        }
        if let Some(x) = self.vibrato_depth {
            v.vibrato_depth = x;
        }
        if let Some(x) = self.pitch_from {
            v.pitch_from = x;
        }
        if let Some(x) = self.pitch_to {
            v.pitch_to = x;
        }
        if let Some(x) = self.pitch_time {
            v.pitch_time = x;
        }
        v
    }

    /// The set fields only, in key order (an empty object for an empty tweak).
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for k in Self::KEYS {
            if let Some(x) = self.get(k) {
                o.set(k, x.into());
            }
        }
        o.into()
    }

    /// Reads the fields it knows (clamped); ignores the rest.
    pub fn from_json(v: Option<&JsonValue>) -> VoiceTweak {
        let mut t = VoiceTweak::default();
        for k in Self::KEYS {
            if let Some(x) = v.and_then(|v| v.get(k)).and_then(JsonValue::as_f64) {
                t.set(k, Some(x));
            }
        }
        t
    }
}

/// A channel's voice settings on a deck: the tweak, whether a sampled channel plays its
/// synth voice instead ("Use synth voice"), and the instrument they were made for.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSetting {
    pub tweak: VoiceTweak,
    pub use_synth: bool,
    pub instrument: String,
}

impl VoiceSetting {
    pub fn is_default(&self) -> bool {
        self.tweak.is_empty() && !self.use_synth
    }
}

/// A tweak made concrete for the lanes of one channel: the coefficients the audio thread
/// installs as they are. A field whose bit is clear in `mask` puts the lane's own
/// (prepared) value back: every lane restores exactly what its instrument had.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceLaneParams {
    pub channel: i16,
    pub(crate) mask: u16,
    pub(crate) use_synth: bool,
    pub(crate) level_mul: f64,
    pub(crate) att_mul: f64,
    pub(crate) decay_scale: f64,
    pub(crate) max_decay: f64,
    pub(crate) post_on: bool,
    pub(crate) b: [f64; 5],
    pub(crate) distortion: f64,
    pub(crate) vib_inc: f64,
    pub(crate) vib_depth: f64,
    pub(crate) pm_on: bool,
    pub(crate) pitch_from: f64,
    pub(crate) pitch_to: f64,
    pub(crate) pm_mul: f64,
}

impl VoiceLaneParams {
    pub const LEVEL: u16 = 1 << 0;
    pub const ATTACK: u16 = 1 << 1;
    pub const DECAY_SCALE: u16 = 1 << 2;
    pub const MAX_DECAY: u16 = 1 << 3;
    pub const LOWPASS: u16 = 1 << 4;
    pub const DRIVE: u16 = 1 << 5;
    pub const VIB_RATE: u16 = 1 << 6;
    pub const VIB_DEPTH: u16 = 1 << 7;
    pub const PITCH: u16 = 1 << 8;
    pub const SYNTH: u16 = 1 << 9;

    pub fn has(&self, bit: u16) -> bool {
        self.mask & bit != 0
    }
    pub fn mask(&self) -> u16 {
        self.mask
    }
    pub fn pm_on(&self) -> bool {
        self.pm_on
    }
    pub fn pitch_from(&self) -> f64 {
        self.pitch_from
    }
    pub fn pitch_to(&self) -> f64 {
        self.pitch_to
    }

    /// `tweak` (and the synth switch) for channel `channel` at `sr`; `voice` is the
    /// instrument's own, which fills a pitch sweep the tweak only partly sets. The
    /// arithmetic is prepare's (`tonal_lane`), so a value equal to the instrument's gives
    /// the same bits.
    pub fn new(
        channel: i64,
        t: &VoiceTweak,
        use_synth: bool,
        voice: Option<&VoiceSpec>,
        sr: f64,
    ) -> VoiceLaneParams {
        let mut p = VoiceLaneParams {
            channel: channel.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
            mask: 0,
            use_synth: false,
            level_mul: 1.0,
            att_mul: 0.0,
            decay_scale: 0.0,
            max_decay: 0.0,
            post_on: false,
            b: [0.0; 5],
            distortion: 0.0,
            vib_inc: 0.0,
            vib_depth: 0.0,
            pm_on: false,
            pitch_from: 1.0,
            pitch_to: 1.0,
            pm_mul: 1.0,
        };
        if let Some(g) = t.gain_db {
            p.mask |= Self::LEVEL;
            p.level_mul = libm::pow(10.0, g / 20.0);
        }
        if let Some(a) = t.attack {
            p.mask |= Self::ATTACK;
            p.att_mul = libm::exp(-1.0 / (a * sr));
        }
        if let Some(x) = t.decay_scale {
            p.mask |= Self::DECAY_SCALE;
            p.decay_scale = x;
        }
        if let Some(x) = t.max_decay {
            p.mask |= Self::MAX_DECAY;
            p.max_decay = x;
        }
        if let Some(fc) = t.lowpass {
            p.mask |= Self::LOWPASS;
            p.post_on = fc > 0.0;
            if p.post_on {
                p.b = biquad("lowpass", fc, 1.0, sr);
            }
        }
        if let Some(x) = t.distortion {
            p.mask |= Self::DRIVE;
            p.distortion = x;
        }
        if let Some(x) = t.vibrato_rate {
            p.mask |= Self::VIB_RATE;
            p.vib_inc = x / sr;
        }
        if let Some(x) = t.vibrato_depth {
            p.mask |= Self::VIB_DEPTH;
            p.vib_depth = x;
        }
        if t.sets_pitch() {
            p.mask |= Self::PITCH;
            p.pitch_from = t.pitch_from.or(voice.map(|v| v.pitch_from)).unwrap_or(1.0);
            p.pitch_to = t.pitch_to.or(voice.map(|v| v.pitch_to)).unwrap_or(1.0);
            let time = t.pitch_time.or(voice.map(|v| v.pitch_time)).unwrap_or(0.0);
            p.pm_on = time > 0.0 && p.pitch_from != p.pitch_to;
            if p.pm_on {
                p.pm_mul = libm::pow(p.pitch_to / p.pitch_from, 1.0 / (time * sr));
            }
        }
        if use_synth {
            p.mask |= Self::SYNTH;
            p.use_synth = true;
        }
        p
    }
}

/// Channels' tweaks for a deck, in channel order.
pub type VoiceSettings = Vec<(i64, VoiceSetting)>;
