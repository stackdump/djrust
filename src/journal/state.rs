//! The checkpoint: everything a replay needs to stand where the app stood — the decks'
//! sources and knobs (as a session names them), the mixer, the loop row by clip id, the row's
//! settings, the locks, and the persistent stores. Audio is never in it: samples are named
//! by content id, clips by id. Port of `Journal/JournalState.swift`; standing an engine on
//! it (`restore(into:)`) is the host's job.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::stores::JournalStoreState;
use crate::json::{JsonObject, JsonValue};
use crate::session::action::DeckAction;
use crate::session::model::{DeckParams, DeckSource, DEMO_ID};
use crate::session::types::{
    scene_clips_from_json, scene_clips_json, AutoMixState, LoopFx, SceneClip, VoiceTweak,
};

/// `LoopFade` raw values (`Audio/LoopSound.swift`).
pub const LOOP_FADES: [&str; 4] = ["off", "10ms", "beat", "bar"];
/// `LoopFollow` raw values.
pub const LOOP_FOLLOWS: [&str; 2] = ["source", "louder"];

/// `VoiceSetting`: what the sound panel set on one channel. The engine's own
/// (`wave::voice_tweak`); a checkpoint keeps one per channel.
pub use crate::wave::voice_tweak::VoiceSetting;

/// `DeckCheckpoint`: a deck at a checkpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckCheckpoint {
    pub source: DeckSource,
    pub params: DeckParams,
    pub playing: bool,
    /// The deck it follows the tempo of.
    pub leader: Option<String>,
    /// The row's clip it holds, when it holds one.
    pub clip: Option<String>,
    /// The sound panel's settings per channel.
    pub voices: BTreeMap<i64, VoiceSetting>,
    /// Where the deck was, when the checkpoint asked for playheads: the sample frame on a
    /// sample deck, the net tick on a net deck.
    pub position: Option<f64>,
}

impl DeckCheckpoint {
    pub fn new(source: DeckSource) -> Self {
        DeckCheckpoint {
            source,
            params: DeckParams::default(),
            playing: false,
            leader: None,
            clip: None,
            voices: BTreeMap::new(),
            position: None,
        }
    }
}

/// `ClipCheckpoint`: a clip on the row at a checkpoint (its audio is the loop library's).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipCheckpoint {
    pub id: String,
    pub title: String,
    pub color: Option<String>,
    pub gain: f64,
    pub muted: bool,
    pub soloed: bool,
    /// Its own key lock; None follows the row's.
    pub key_lock: Option<bool>,
    /// Its own launch boundary (beats) and fade; None follows the row's.
    pub quantum: Option<i64>,
    pub fade: Option<String>,
    /// Whose tempo it follows; None follows the row's.
    pub follow: Option<String>,
    pub looping: bool,
    pub fx: LoopFx,
}

impl ClipCheckpoint {
    pub fn new(id: &str, title: &str) -> Self {
        ClipCheckpoint {
            id: id.to_string(),
            title: title.to_string(),
            color: None,
            gain: 1.0,
            muted: false,
            soloed: false,
            key_lock: None,
            quantum: None,
            fade: None,
            follow: None,
            looping: false,
            fx: LoopFx::default(),
        }
    }
}

/// `LoopScene`: a named set of loops.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopScene {
    pub name: String,
    pub clips: Vec<SceneClip>,
}

impl LoopScene {
    pub fn json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("name", JsonValue::str(self.name.as_str())),
            ("clips", scene_clips_json(&self.clips)),
        ]))
    }
    pub fn from_json(v: &JsonValue) -> Option<LoopScene> {
        let name = v.get("name")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        Some(LoopScene {
            name: name.to_string(),
            clips: scene_clips_from_json(v.get("clips")),
        })
    }
}

/// `JournalState`: the full rebuildable state at one moment.
#[derive(Clone, Debug)]
pub struct JournalState {
    pub sample_rate: f64,
    pub decks: BTreeMap<String, DeckCheckpoint>,
    /// The loop channel's strip: volume, trim, reverb (in `params`) and mute.
    pub loop_channel: DeckParams,
    pub loop_mute: bool,
    pub crossfader: f64,
    pub master: f64,
    /// The row's settings, as the actions that set them.
    pub loop_settings: Vec<DeckAction>,
    pub clips: Vec<ClipCheckpoint>,
    pub scenes: Vec<LoopScene>,
    /// Clips taken off the row this run (newest first) with the place each had.
    pub removed: Vec<(String, i64)>,
    /// The engine's locked actions, sorted.
    pub locked: Vec<String>,
    pub stores: JournalStoreState,
    /// Auto-mix: settings, counter, and a hand-off in flight.
    pub auto_mix: AutoMixState,
    /// Whether `stores` is part of this state. Line 0 always carries them; a mid-file
    /// checkpoint leaves them out.
    pub has_stores: bool,
}

pub const SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/state/v1");

impl PartialEq for JournalState {
    /// States compare by their JSON.
    fn eq(&self, other: &Self) -> bool {
        self.json() == other.json()
    }
}

impl JournalState {
    pub fn new(sample_rate: f64) -> Self {
        JournalState {
            sample_rate,
            decks: BTreeMap::new(),
            loop_channel: DeckParams::default(),
            loop_mute: false,
            crossfader: 0.5,
            master: 0.8,
            loop_settings: Vec::new(),
            clips: Vec::new(),
            scenes: Vec::new(),
            removed: Vec::new(),
            locked: Vec::new(),
            stores: JournalStoreState::default(),
            auto_mix: AutoMixState::default(),
            has_stores: true,
        }
    }

    pub fn json(&self) -> JsonValue {
        let mut decks = JsonObject::new();
        for (name, d) in &self.decks {
            let mut o = JsonObject::from_pairs([
                ("source", d.source.json()),
                ("params", d.params.json()),
                ("playing", JsonValue::Bool(d.playing)),
            ]);
            if let Some(l) = &d.leader {
                o.set("leader", JsonValue::str(l.as_str()));
            }
            if let Some(c) = &d.clip {
                o.set("clip", JsonValue::str(c.as_str()));
            }
            if let Some(p) = d.position {
                o.set("position", JsonValue::Number(p));
            }
            if !d.voices.is_empty() {
                o.set(
                    "voices",
                    JsonValue::Array(
                        d.voices
                            .iter()
                            .map(|(ch, v)| {
                                JsonValue::Object(JsonObject::from_pairs([
                                    ("channel", JsonValue::int(*ch)),
                                    ("instrument", JsonValue::str(v.instrument.as_str())),
                                    ("tweak", v.tweak.to_json()),
                                    ("synth", JsonValue::Bool(v.use_synth)),
                                ]))
                            })
                            .collect(),
                    ),
                );
            }
            decks.set(name.as_str(), JsonValue::Object(o));
        }
        let mut loop_o = self
            .loop_channel
            .json()
            .as_object()
            .cloned()
            .unwrap_or_default();
        loop_o.set("mute", JsonValue::Bool(self.loop_mute));
        let mut mixer = JsonObject::from_pairs([
            ("crossfader", JsonValue::Number(self.crossfader)),
            ("master", JsonValue::Number(self.master)),
        ]);
        if self.auto_mix.settings.on || !self.auto_mix.is_session_default() {
            mixer.set("autoMix", self.auto_mix.checkpoint_json());
        }
        let clips = self
            .clips
            .iter()
            .map(|c| {
                let mut o = JsonObject::from_pairs([
                    ("id", JsonValue::str(c.id.as_str())),
                    ("title", JsonValue::str(c.title.as_str())),
                    ("gain", JsonValue::Number(c.gain)),
                    ("muted", JsonValue::Bool(c.muted)),
                    ("soloed", JsonValue::Bool(c.soloed)),
                    ("looping", JsonValue::Bool(c.looping)),
                ]);
                if let Some(color) = &c.color {
                    o.set("color", JsonValue::str(color.as_str()));
                }
                if let Some(k) = c.key_lock {
                    o.set("keyLock", JsonValue::Bool(k));
                }
                // only when set: checkpoints without them encode as before
                if let Some(q) = c.quantum {
                    o.set("quantum", JsonValue::int(q));
                }
                if let Some(f) = &c.fade {
                    o.set("fade", JsonValue::str(f.as_str()));
                }
                if let Some(f) = &c.follow {
                    o.set("follow", JsonValue::str(f.as_str()));
                }
                if c.fx != LoopFx::default() {
                    o.set("fx", c.fx.json());
                }
                JsonValue::Object(o)
            })
            .collect();
        let mut out = JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("sampleRate", JsonValue::Number(self.sample_rate)),
            ("decks", JsonValue::Object(decks)),
            ("loopChannel", JsonValue::Object(loop_o)),
            ("mixer", JsonValue::Object(mixer)),
            (
                "loopSettings",
                JsonValue::Array(self.loop_settings.iter().map(DeckAction::json).collect()),
            ),
            ("clips", JsonValue::Array(clips)),
            (
                "scenes",
                JsonValue::Array(self.scenes.iter().map(LoopScene::json).collect()),
            ),
            (
                "removed",
                JsonValue::Array(
                    self.removed
                        .iter()
                        .map(|(id, index)| {
                            JsonValue::Object(JsonObject::from_pairs([
                                ("id", JsonValue::str(id.as_str())),
                                ("index", JsonValue::int(*index)),
                            ]))
                        })
                        .collect(),
                ),
            ),
            (
                "locked",
                JsonValue::Array(
                    self.locked
                        .iter()
                        .map(|l| JsonValue::str(l.as_str()))
                        .collect(),
                ),
            ),
        ]);
        if self.has_stores {
            out.set("stores", self.stores.json());
        }
        JsonValue::Object(out)
    }

    pub fn from_json(v: &JsonValue) -> Option<JournalState> {
        if v.get("schema").and_then(JsonValue::as_str) != Some(SCHEMA) {
            return None;
        }
        let sr = v.get("sampleRate")?.as_f64()?;
        let mut decks = BTreeMap::new();
        if let Some(o) = v.get("decks").and_then(JsonValue::as_object) {
            for (name, d) in o.iter() {
                let source = DeckSource::from_json(d.get("source")?)?;
                let mut voices = BTreeMap::new();
                if let Some(a) = d.get("voices").and_then(JsonValue::as_array) {
                    for e in a {
                        let Some(ch) = e.get("channel").and_then(JsonValue::as_i64) else {
                            continue;
                        };
                        voices.insert(
                            ch,
                            VoiceSetting {
                                tweak: VoiceTweak::from_json(e.get("tweak")),
                                use_synth: e
                                    .get("synth")
                                    .and_then(JsonValue::as_bool)
                                    .unwrap_or(false),
                                instrument: e
                                    .get("instrument")
                                    .and_then(JsonValue::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            },
                        );
                    }
                }
                decks.insert(
                    name.to_string(),
                    DeckCheckpoint {
                        source,
                        params: d
                            .get("params")
                            .map(DeckParams::from_json)
                            .unwrap_or_default(),
                        playing: d
                            .get("playing")
                            .and_then(JsonValue::as_bool)
                            .unwrap_or(false),
                        leader: d
                            .get("leader")
                            .and_then(JsonValue::as_str)
                            .map(String::from),
                        clip: d.get("clip").and_then(JsonValue::as_str).map(String::from),
                        voices,
                        position: d.get("position").and_then(JsonValue::as_f64),
                    },
                );
            }
        }
        let loop_v = v.get("loopChannel");
        let mixer = v.get("mixer");
        let mnum = |k: &str, d: f64| {
            mixer
                .and_then(|m| m.get(k))
                .and_then(JsonValue::as_f64)
                .unwrap_or(d)
        };
        let clips = v
            .get("clips")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        let id = c.get("id")?.as_str()?;
                        Some(ClipCheckpoint {
                            id: id.to_string(),
                            title: c
                                .get("title")
                                .and_then(JsonValue::as_str)
                                .unwrap_or("loop")
                                .to_string(),
                            color: c.get("color").and_then(JsonValue::as_str).map(String::from),
                            gain: c.get("gain").and_then(JsonValue::as_f64).unwrap_or(1.0),
                            muted: c.get("muted").and_then(JsonValue::as_bool).unwrap_or(false),
                            soloed: c
                                .get("soloed")
                                .and_then(JsonValue::as_bool)
                                .unwrap_or(false),
                            key_lock: c.get("keyLock").and_then(JsonValue::as_bool),
                            quantum: c.get("quantum").and_then(JsonValue::as_i64),
                            fade: c
                                .get("fade")
                                .and_then(JsonValue::as_str)
                                .filter(|f| LOOP_FADES.contains(f))
                                .map(String::from),
                            follow: c
                                .get("follow")
                                .and_then(JsonValue::as_str)
                                .filter(|f| LOOP_FOLLOWS.contains(f))
                                .map(String::from),
                            looping: c
                                .get("looping")
                                .and_then(JsonValue::as_bool)
                                .unwrap_or(false),
                            fx: LoopFx::from_json(c.get("fx")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(JournalState {
            sample_rate: sr,
            decks,
            loop_channel: loop_v.map(DeckParams::from_json).unwrap_or_default(),
            loop_mute: loop_v
                .and_then(|l| l.get("mute"))
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            crossfader: mnum("crossfader", 0.5),
            master: mnum("master", 0.8),
            auto_mix: AutoMixState::from_json(mixer.and_then(|m| m.get("autoMix"))),
            loop_settings: v
                .get("loopSettings")
                .and_then(JsonValue::as_array)
                .map(|a| a.iter().filter_map(DeckAction::from_json).collect())
                .unwrap_or_default(),
            clips,
            scenes: v
                .get("scenes")
                .and_then(JsonValue::as_array)
                .map(|a| a.iter().filter_map(LoopScene::from_json).collect())
                .unwrap_or_default(),
            removed: v
                .get("removed")
                .and_then(JsonValue::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            let id = e.get("id")?.as_str()?;
                            Some((
                                id.to_string(),
                                e.get("index").and_then(JsonValue::as_i64).unwrap_or(0),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            locked: v
                .get("locked")
                .and_then(JsonValue::strings)
                .unwrap_or_default(),
            has_stores: v.get("stores").is_some(),
            stores: JournalStoreState::from_json(v.get("stores")),
        })
    }

    /// Every sample id the state names (decks' sources), for a replay to find before it
    /// starts.
    pub fn sample_ids(&self) -> Vec<String> {
        self.decks
            .values()
            .filter_map(|d| match &d.source {
                DeckSource::Sample { id, .. } if id != DEMO_ID && d.clip.is_none() => {
                    Some(id.clone())
                }
                _ => None,
            })
            .collect()
    }
}
