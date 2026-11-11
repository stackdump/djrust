//! The small value types a session names its decks' starting state with.
//!
//! Most of them live in the engine's own modules and are re-exported here, so a session, a
//! journal checkpoint and the engine share one type each: `WanderState` and `WanderKey`
//! (`wander`), `FxUnitSettings` / `FxParam` (`fx::unit`), `AutoMixSettings` / `AutoMixState`
//! (`automix`), `VoiceTweak` (`wave::voice_tweak`) and `BeatGrid` (`deck::beat_grid`). A
//! session reads and writes the part of each that `SessionModel.swift` names
//! (`WanderState.settings`, `AutoMixState.start`): the fields and JSON codec, nothing of the
//! engines.
//!
//! Defined here is what has no home of its own yet: `LoopFx` / `LoopFxMode`, `SceneClip` and
//! `SequenceStep`, with their JSON.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};

pub use crate::automix::{AutoMixSettings, AutoMixState};
pub use crate::deck::beat_grid::BeatGrid;
pub use crate::fx::unit::{FxParam, FxUnitSettings};
pub use crate::wander::{WanderKey, WanderState};
pub use crate::wave::voice_tweak::VoiceTweak;

fn num(v: Option<&JsonValue>) -> Option<f64> {
    v.and_then(JsonValue::as_f64)
}
fn int(v: Option<&JsonValue>) -> Option<i64> {
    v.and_then(JsonValue::as_i64)
}
fn boolean(v: Option<&JsonValue>) -> Option<bool> {
    v.and_then(JsonValue::as_bool)
}

// ---- loop FX ----------------------------------------------------------------------------------

/// `LoopFXMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopFxMode {
    Clean,
    Soft,
    Hard,
    Tube,
    Fold,
    Crush,
}

impl LoopFxMode {
    pub fn raw(self) -> &'static str {
        match self {
            LoopFxMode::Clean => "clean",
            LoopFxMode::Soft => "soft",
            LoopFxMode::Hard => "hard",
            LoopFxMode::Tube => "tube",
            LoopFxMode::Fold => "fold",
            LoopFxMode::Crush => "crush",
        }
    }
    pub fn from_raw(s: &str) -> Option<Self> {
        [
            LoopFxMode::Clean,
            LoopFxMode::Soft,
            LoopFxMode::Hard,
            LoopFxMode::Tube,
            LoopFxMode::Fold,
            LoopFxMode::Crush,
        ]
        .into_iter()
        .find(|m| m.raw() == s)
    }
}

/// `LoopFX`: a loop's after effects. The default is off.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopFx {
    pub bypass: bool,
    pub drive: f64,
    pub mode: LoopFxMode,
    pub amount: f64,
    pub bits: f64,
    pub downsample: i64,
    pub tone: f64,
    pub mix: f64,
    pub trim: f64,
    pub auto_level: bool,
}

impl Default for LoopFx {
    fn default() -> Self {
        LoopFx {
            bypass: false,
            drive: 0.0,
            mode: LoopFxMode::Clean,
            amount: 0.5,
            bits: 8.0,
            downsample: 1,
            tone: 1.0,
            mix: 1.0,
            trim: 0.0,
            auto_level: true,
        }
    }
}

impl LoopFx {
    /// `clamped`: every value inside its range.
    pub fn clamped(&self) -> LoopFx {
        fn c(v: f64, lo: f64, hi: f64) -> f64 {
            if v.is_finite() {
                v.max(lo).min(hi)
            } else {
                lo
            }
        }
        let mut f = self.clone();
        f.drive = c(self.drive, 0.0, 24.0);
        f.amount = c(self.amount, 0.0, 1.0);
        f.bits = c(self.bits, 1.0, 16.0);
        f.downsample = self.downsample.max(1).min(32);
        f.tone = c(self.tone, 0.0, 1.0);
        f.mix = c(self.mix, 0.0, 1.0);
        f.trim = c(self.trim, -24.0, 12.0);
        f
    }

    /// Every field, so a recorded poke means the same whatever the defaults become.
    pub fn json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("bypass", JsonValue::Bool(self.bypass)),
            ("drive", JsonValue::Number(self.drive)),
            ("mode", JsonValue::str(self.mode.raw())),
            ("amount", JsonValue::Number(self.amount)),
            ("bits", JsonValue::Number(self.bits)),
            ("downsample", JsonValue::int(self.downsample)),
            ("tone", JsonValue::Number(self.tone)),
            ("mix", JsonValue::Number(self.mix)),
            ("trim", JsonValue::Number(self.trim)),
            ("autoLevel", JsonValue::Bool(self.auto_level)),
        ]))
    }

    /// Missing fields take their defaults; values are clamped into their ranges.
    pub fn from_json(v: Option<&JsonValue>) -> Self {
        let d = LoopFx::default();
        let g = |k: &str| v.and_then(|v| v.get(k));
        LoopFx {
            bypass: boolean(g("bypass")).unwrap_or(d.bypass),
            drive: num(g("drive")).unwrap_or(d.drive),
            mode: g("mode")
                .and_then(JsonValue::as_str)
                .and_then(LoopFxMode::from_raw)
                .unwrap_or(d.mode),
            amount: num(g("amount")).unwrap_or(d.amount),
            bits: num(g("bits")).unwrap_or(d.bits),
            downsample: int(g("downsample")).unwrap_or(d.downsample),
            tone: num(g("tone")).unwrap_or(d.tone),
            mix: num(g("mix")).unwrap_or(d.mix),
            trim: num(g("trim")).unwrap_or(d.trim),
            auto_level: boolean(g("autoLevel")).unwrap_or(d.auto_level),
        }
        .clamped()
    }
}

// ---- scenes and sequences ---------------------------------------------------------------------

/// `SceneClip`: one clip of a scene, at the level (and with the FX) it had when saved.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneClip {
    pub clip_id: String,
    pub gain: f64,
    /// Nil: the scene leaves its FX alone.
    pub fx: Option<LoopFx>,
}

impl SceneClip {
    pub fn new(clip_id: &str) -> Self {
        SceneClip {
            clip_id: clip_id.to_string(),
            gain: 1.0,
            fx: None,
        }
    }
}

/// `[SceneClip].json`: `fx` only when the clip carries it.
pub fn scene_clips_json(clips: &[SceneClip]) -> JsonValue {
    JsonValue::Array(
        clips
            .iter()
            .map(|c| {
                let mut o = JsonObject::from_pairs([
                    ("clip", JsonValue::str(c.clip_id.as_str())),
                    ("gain", JsonValue::Number(c.gain)),
                ]);
                if let Some(fx) = &c.fx {
                    o.set("fx", fx.json());
                }
                JsonValue::Object(o)
            })
            .collect(),
    )
}

/// `[SceneClip](json:)`.
pub fn scene_clips_from_json(v: Option<&JsonValue>) -> Vec<SceneClip> {
    v.and_then(JsonValue::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let id = e.get("clip")?.as_str()?;
                    Some(SceneClip {
                        clip_id: id.to_string(),
                        gain: num(e.get("gain")).unwrap_or(1.0),
                        fx: e.get("fx").map(|f| LoopFx::from_json(Some(f))),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `SequenceStep`: a clip, played `repeats` times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceStep {
    pub clip_id: String,
    pub repeats: i64,
}

impl SequenceStep {
    pub fn new(clip_id: &str, repeats: i64) -> Self {
        SequenceStep {
            clip_id: clip_id.to_string(),
            repeats,
        }
    }
}

/// `[SequenceStep].json`.
pub fn sequence_steps_json(steps: &[SequenceStep]) -> JsonValue {
    JsonValue::Array(
        steps
            .iter()
            .map(|s| {
                JsonValue::Object(JsonObject::from_pairs([
                    ("clip", JsonValue::str(s.clip_id.as_str())),
                    ("repeats", JsonValue::int(s.repeats)),
                ]))
            })
            .collect(),
    )
}

/// `[SequenceStep](json:)`.
pub fn sequence_steps_from_json(v: Option<&JsonValue>) -> Vec<SequenceStep> {
    v.and_then(JsonValue::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let id = e.get("clip")?.as_str()?;
                    Some(SequenceStep {
                        clip_id: id.to_string(),
                        repeats: int(e.get("repeats")).unwrap_or(1),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
