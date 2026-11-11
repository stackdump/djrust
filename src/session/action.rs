//! `DeckAction`: every input a performer (or a UI) can give a deck, and its session JSON
//! codec. Port of `DeckAction` and its `json` / `init?(json:)` in
//! `Session/SessionModel.swift`. Controller hardware arrives as MIDI bytes; these are the
//! other ways in.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::model::DeckSource;
use super::types::{
    scene_clips_from_json, scene_clips_json, sequence_steps_from_json, sequence_steps_json,
    AutoMixSettings, LoopFx, SceneClip, SequenceStep, VoiceTweak,
};
use crate::json::{JsonObject, JsonValue};
use crate::spec::TrackSpec;

/// `DeckAction`. Fields named as in Swift (snake_cased); a deck is "A", "B" or the loop
/// channel "L".
#[derive(Clone, Debug, PartialEq)]
pub enum DeckAction {
    Play {
        deck: String,
        on: bool,
    },
    Cue {
        deck: String,
    },
    HotCue {
        deck: String,
        slot: i64,
    },
    ClearCue {
        deck: String,
        slot: i64,
    },
    /// `op`: in | out | halve | double.
    Loop {
        deck: String,
        op: String,
    },
    /// `name`: volume | filter | gain | eqLow | eqMid | eqHigh | pitch | mute | reverbWet |
    /// reverbSize | reverbTone | autoGain.
    Param {
        deck: String,
        name: String,
        value: f64,
    },
    Crossfader(f64),
    Master(f64),
    Load {
        deck: String,
        source: DeckSource,
    },
    QueueNext {
        deck: String,
        spec: TrackSpec,
    },
    NextTrack {
        deck: String,
    },
    AutoAdvance {
        deck: String,
        on: bool,
    },
    Section {
        deck: String,
        index: i64,
    },
    Role {
        deck: String,
        role: String,
        on: bool,
    },
    /// Disable a role on a deck — through new tracks too — or enable it.
    RoleDisable {
        deck: String,
        role: String,
        on: bool,
    },
    Macro {
        deck: String,
        id: String,
    },
    Panic {
        deck: String,
    },
    Stinger {
        deck: String,
        n: i64,
    },
    /// `align`: beat | bar | tempo.
    Sync {
        deck: String,
        align: String,
    },
    /// `mode`: off | beat | bar.
    Quantize {
        deck: String,
        mode: String,
    },
    Follow {
        deck: String,
        leader: Option<String>,
    },
    /// The tempo analysis (or a tap) put a beat grid on a sample deck.
    Grid {
        deck: String,
        bpm: f64,
        offset: f64,
    },
    /// LOOP record held (`on`) or let go on a deck; `bars` (on the press): a tap records
    /// exactly that many bars.
    LoopRecord {
        deck: String,
        on: bool,
        bars: Option<i64>,
    },
    /// Take the last `bars` whole bars a deck has already played.
    GrabBars {
        deck: String,
        bars: i64,
    },
    /// Put a LOOP-record clip on a deck or the loop channel ("L").
    Clip {
        deck: String,
        id: String,
    },
    /// Start (`on`) or stop a clip looping under the mixer, at the next downbeat.
    ClipLoop {
        id: String,
        on: bool,
    },
    /// A clip's level under the mixer, 0…1.
    ClipGain {
        id: String,
        value: f64,
    },
    /// Crop a clip to its beats `from..<to`.
    ClipCrop {
        id: String,
        from: i64,
        to: i64,
    },
    /// Render a net clip's bars again with `voices` (MIDI channel → beats instrument).
    ClipRetune {
        id: String,
        voices: BTreeMap<i64, String>,
    },
    /// Join clips into a sequence, or rewrite the one `replacing` names.
    ClipSequence {
        steps: Vec<SequenceStep>,
        replacing: Option<String>,
    },
    ClipRemove {
        id: String,
    },
    ClipRestore {
        id: String,
        at: Option<i64>,
    },
    ClipForget {
        id: String,
    },
    ClipRename {
        id: String,
        title: String,
    },
    ClipColor {
        id: String,
        color: Option<String>,
    },
    /// What loops start and stop on, in beats: 1 | 4 | 8 | 16 | 32.
    LoopQuantize {
        beats: i64,
    },
    /// Whose tempo the loops follow: "louder" or "source".
    LoopFollow {
        mode: String,
    },
    SceneSave {
        name: String,
    },
    /// Recorded with the scene's `clips`, so a replay needs no saved scenes.
    SceneLaunch {
        name: String,
        clips: Option<Vec<SceneClip>>,
    },
    SceneRename {
        name: String,
        to: String,
    },
    SceneDelete {
        name: String,
    },
    ClipMute {
        id: String,
        on: bool,
    },
    ClipSolo {
        id: String,
        on: bool,
    },
    ClipKeyLock {
        id: String,
        on: bool,
    },
    /// The row's key lock, for clips without their own setting.
    LoopKeyLock {
        on: bool,
    },
    /// off | 10ms | beat | bar.
    LoopFade {
        mode: String,
    },
    ClipQuantize {
        id: String,
        beats: Option<i64>,
    },
    ClipFade {
        id: String,
        mode: Option<String>,
    },
    ClipFollow {
        id: String,
        mode: Option<String>,
    },
    TapBars {
        bars: Option<i64>,
    },
    /// Shape the voice of a net deck's channel; session-only.
    VoiceTweak {
        deck: String,
        channel: i64,
        params: VoiceTweak,
    },
    VoiceSynth {
        deck: String,
        channel: i64,
        on: bool,
    },
    ClipMove {
        id: String,
        before: Option<String>,
    },
    /// Lock an action or unlock it: a controller binding with this key does nothing while
    /// locked.
    Lock {
        key: String,
        on: bool,
    },
    Wander {
        deck: String,
        on: bool,
    },
    WanderPace {
        deck: String,
        bars: i64,
    },
    WanderLock {
        deck: String,
        dimension: String,
        on: bool,
    },
    SetKit {
        deck: String,
        kit: Option<String>,
    },
    SetPalette {
        deck: String,
        palette: Option<String>,
    },
    ClipFx {
        id: String,
        fx: LoopFx,
    },
    LoopFxLink {
        on: bool,
    },
    SetRoleVoice {
        deck: String,
        role: String,
        voice: Option<String>,
    },
    /// Outside audio (a stock loop) onto the row; `provenance` is `{"kind": "stock", …}` for
    /// a stock loop, which a publish refuses.
    ClipImport {
        id: String,
        title: String,
        bpm: f64,
        beats: i64,
        provenance: Option<JsonValue>,
    },
    MixOut {
        deck: String,
        bars: i64,
    },
    MixIn {
        deck: String,
        bars: i64,
    },
    Rewind {
        deck: String,
    },
    Focus {
        deck: String,
    },
    StemMute {
        deck: String,
        stem: String,
        on: bool,
    },
    MasterFx {
        deck: String,
        key: String,
        value: f64,
    },
    Fx {
        deck: String,
        param: String,
        value: f64,
    },
    FxReset {
        deck: String,
        on_beat: bool,
    },
    FxKill {
        deck: String,
        on: bool,
        on_beat: bool,
    },
    AutoMix {
        on: bool,
        bars: i64,
        every: i64,
    },
    AutoMixNow,
    /// Recorded by the engine, live only.
    AutoMixDefer {
        handoff: i64,
    },
}

/// `MixOutTail.defaultBars`.
pub const MIX_OUT_DEFAULT_BARS: i64 = 16;
/// `DeckAction.defaultMixInBars`.
pub const DEFAULT_MIX_IN_BARS: i64 = 4;
/// `DJEngine.defaultLoopQuantum` (LoopLaunch.swift): the one number is the loop row's.
pub use crate::deck::loops::DEFAULT_LOOP_QUANTUM;

/// Every `op` `from_json` decodes, in its order (`DeckAction.jsonOps`).
pub const JSON_OPS: [&str; 75] = [
    "play",
    "cue",
    "hotCue",
    "clearCue",
    "loop",
    "param",
    "crossfader",
    "master",
    "load",
    "queueNext",
    "nextTrack",
    "autoAdvance",
    "section",
    "role",
    "roleDisable",
    "macro",
    "panic",
    "stinger",
    "sync",
    "quantize",
    "follow",
    "grid",
    "loopRecord",
    "grabBars",
    "clip",
    "clipLoop",
    "clipGain",
    "clipSequence",
    "clipCrop",
    "clipRemove",
    "clipRestore",
    "clipForget",
    "clipRename",
    "clipColor",
    "loopQuantize",
    "loopFollow",
    "sceneSave",
    "sceneLaunch",
    "sceneRename",
    "sceneDelete",
    "clipMute",
    "clipSolo",
    "clipKeyLock",
    "loopKeyLock",
    "loopFade",
    "clipQuantize",
    "clipFade",
    "clipFollow",
    "tapBars",
    "voiceTweak",
    "voiceSynth",
    "clipMove",
    "lock",
    "wander",
    "wanderPace",
    "wanderLock",
    "setKit",
    "setPalette",
    "clipFx",
    "loopFxLink",
    "setRoleVoice",
    "clipImport",
    "mixOut",
    "mixIn",
    "rewind",
    "focus",
    "stemMute",
    "masterFx",
    "clipRetune",
    "fx",
    "fxReset",
    "fxKill",
    "autoMix",
    "autoMixNow",
    "autoMixDefer",
];

fn s(v: &str) -> JsonValue {
    JsonValue::str(v)
}
fn opt_s(v: &Option<String>) -> JsonValue {
    v.as_ref().map(|x| s(x)).unwrap_or(JsonValue::Null)
}
fn opt_i(v: Option<i64>) -> JsonValue {
    v.map(JsonValue::int).unwrap_or(JsonValue::Null)
}
fn b(v: bool) -> JsonValue {
    JsonValue::Bool(v)
}
fn n(v: f64) -> JsonValue {
    JsonValue::Number(v)
}
fn i(v: i64) -> JsonValue {
    JsonValue::int(v)
}

/// `DeckAction.voicesJSON`: instruments by MIDI channel as JSON, channels in order.
pub fn voices_json(voices: &BTreeMap<i64, String>) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(
        voices
            .iter()
            .map(|(k, v)| (k.to_string(), JsonValue::str(v.as_str()))),
    ))
}

/// `DeckAction.voices(json:)`: entries that aren't a channel and an instrument are left out.
pub fn voices_from_json(v: Option<&JsonValue>) -> BTreeMap<i64, String> {
    let mut out = BTreeMap::new();
    if let Some(o) = v.and_then(JsonValue::as_object) {
        for (k, name) in o.iter() {
            if let (Ok(ch), Some(name)) = (k.parse::<i64>(), name.as_str()) {
                if !name.is_empty() {
                    out.insert(ch, name.to_string());
                }
            }
        }
    }
    out
}

impl DeckAction {
    /// `DeckAction.json`.
    pub fn json(&self) -> JsonValue {
        fn o(op: &str, rest: Vec<(&str, JsonValue)>) -> JsonValue {
            let mut obj = JsonObject::new();
            obj.set("op", JsonValue::str(op));
            for (k, v) in rest {
                obj.set(k, v);
            }
            JsonValue::Object(obj)
        }
        fn d(deck: &str) -> (&'static str, JsonValue) {
            ("deck", JsonValue::str(deck))
        }
        use DeckAction::*;
        match self {
            Play { deck, on } => o("play", vec![d(deck), ("on", b(*on))]),
            Cue { deck } => o("cue", vec![d(deck)]),
            HotCue { deck, slot } => o("hotCue", vec![d(deck), ("slot", i(*slot))]),
            ClearCue { deck, slot } => o("clearCue", vec![d(deck), ("slot", i(*slot))]),
            Loop { deck, op } => o("loop", vec![d(deck), ("loop", s(op))]),
            Param { deck, name, value } => o(
                "param",
                vec![d(deck), ("name", s(name)), ("value", n(*value))],
            ),
            Crossfader(v) => o("crossfader", vec![("value", n(*v))]),
            Master(v) => o("master", vec![("value", n(*v))]),
            Load { deck, source } => o("load", vec![d(deck), ("source", source.json())]),
            QueueNext { deck, spec } => {
                let mut fields = vec![d(deck), ("share", spec.payload())];
                if let Some(seeds) = spec.part_seeds_json() {
                    fields.push(("partSeeds", seeds));
                }
                o("queueNext", fields)
            }
            NextTrack { deck } => o("nextTrack", vec![d(deck)]),
            AutoAdvance { deck, on } => o("autoAdvance", vec![d(deck), ("on", b(*on))]),
            Section { deck, index } => o("section", vec![d(deck), ("index", i(*index))]),
            Role { deck, role, on } => o("role", vec![d(deck), ("role", s(role)), ("on", b(*on))]),
            RoleDisable { deck, role, on } => o(
                "roleDisable",
                vec![d(deck), ("role", s(role)), ("on", b(*on))],
            ),
            Macro { deck, id } => o("macro", vec![d(deck), ("macro", s(id))]),
            Panic { deck } => o("panic", vec![d(deck)]),
            Stinger { deck, n: k } => o("stinger", vec![d(deck), ("n", i(*k))]),
            Sync { deck, align } => o("sync", vec![d(deck), ("align", s(align))]),
            Quantize { deck, mode } => o("quantize", vec![d(deck), ("mode", s(mode))]),
            Follow { deck, leader } => o("follow", vec![d(deck), ("leader", opt_s(leader))]),
            Grid { deck, bpm, offset } => o(
                "grid",
                vec![d(deck), ("bpm", n(*bpm)), ("offset", n(*offset))],
            ),
            LoopRecord { deck, on, bars } => {
                // `bars` only when set: a hold-only take encodes as it always has
                let mut f = vec![d(deck), ("on", b(*on))];
                if let Some(x) = bars {
                    f.push(("bars", i(*x)));
                }
                o("loopRecord", f)
            }
            GrabBars { deck, bars } => o("grabBars", vec![d(deck), ("bars", i(*bars))]),
            Clip { deck, id } => o("clip", vec![d(deck), ("id", s(id))]),
            ClipLoop { id, on } => o("clipLoop", vec![("id", s(id)), ("on", b(*on))]),
            ClipGain { id, value } => o("clipGain", vec![("id", s(id)), ("value", n(*value))]),
            ClipCrop { id, from, to } => o(
                "clipCrop",
                vec![("id", s(id)), ("from", i(*from)), ("to", i(*to))],
            ),
            ClipRetune { id, voices } => o(
                "clipRetune",
                vec![("id", s(id)), ("voices", voices_json(voices))],
            ),
            ClipSequence { steps, replacing } => o(
                "clipSequence",
                vec![
                    ("steps", sequence_steps_json(steps)),
                    ("replacing", opt_s(replacing)),
                ],
            ),
            ClipRemove { id } => o("clipRemove", vec![("id", s(id))]),
            ClipRestore { id, at } => o("clipRestore", vec![("id", s(id)), ("at", opt_i(*at))]),
            ClipForget { id } => o("clipForget", vec![("id", s(id))]),
            ClipRename { id, title } => o("clipRename", vec![("id", s(id)), ("title", s(title))]),
            ClipColor { id, color } => o("clipColor", vec![("id", s(id)), ("color", opt_s(color))]),
            LoopQuantize { beats } => o("loopQuantize", vec![("beats", i(*beats))]),
            LoopFollow { mode } => o("loopFollow", vec![("mode", s(mode))]),
            SceneSave { name } => o("sceneSave", vec![("name", s(name))]),
            SceneLaunch { name, clips } => o(
                "sceneLaunch",
                vec![
                    ("name", s(name)),
                    (
                        "clips",
                        clips
                            .as_ref()
                            .map(|c| scene_clips_json(c))
                            .unwrap_or(JsonValue::Null),
                    ),
                ],
            ),
            SceneRename { name, to } => o("sceneRename", vec![("name", s(name)), ("to", s(to))]),
            SceneDelete { name } => o("sceneDelete", vec![("name", s(name))]),
            ClipMute { id, on } => o("clipMute", vec![("id", s(id)), ("on", b(*on))]),
            ClipSolo { id, on } => o("clipSolo", vec![("id", s(id)), ("on", b(*on))]),
            ClipKeyLock { id, on } => o("clipKeyLock", vec![("id", s(id)), ("on", b(*on))]),
            LoopKeyLock { on } => o("loopKeyLock", vec![("on", b(*on))]),
            LoopFade { mode } => o("loopFade", vec![("fade", s(mode))]),
            ClipQuantize { id, beats } => o(
                "clipQuantize",
                vec![("id", s(id)), ("beats", opt_i(*beats))],
            ),
            ClipFade { id, mode } => o("clipFade", vec![("id", s(id)), ("fade", opt_s(mode))]),
            ClipFollow { id, mode } => o("clipFollow", vec![("id", s(id)), ("mode", opt_s(mode))]),
            TapBars { bars } => o("tapBars", vec![("bars", opt_i(*bars))]),
            VoiceTweak {
                deck,
                channel,
                params,
            } => o(
                "voiceTweak",
                vec![
                    d(deck),
                    ("channel", i(*channel)),
                    ("params", params.to_json()),
                ],
            ),
            VoiceSynth { deck, channel, on } => o(
                "voiceSynth",
                vec![d(deck), ("channel", i(*channel)), ("on", b(*on))],
            ),
            ClipMove { id, before } => {
                o("clipMove", vec![("id", s(id)), ("before", opt_s(before))])
            }
            Lock { key, on } => o("lock", vec![("key", s(key)), ("on", b(*on))]),
            Wander { deck, on } => o("wander", vec![d(deck), ("on", b(*on))]),
            WanderPace { deck, bars } => o("wanderPace", vec![d(deck), ("bars", i(*bars))]),
            WanderLock {
                deck,
                dimension,
                on,
            } => o(
                "wanderLock",
                vec![d(deck), ("dimension", s(dimension)), ("on", b(*on))],
            ),
            SetKit { deck, kit } => o("setKit", vec![d(deck), ("kit", opt_s(kit))]),
            SetPalette { deck, palette } => {
                o("setPalette", vec![d(deck), ("palette", opt_s(palette))])
            }
            ClipFx { id, fx } => o("clipFx", vec![("id", s(id)), ("fx", fx.json())]),
            LoopFxLink { on } => o("loopFxLink", vec![("on", b(*on))]),
            SetRoleVoice { deck, role, voice } => o(
                "setRoleVoice",
                vec![d(deck), ("role", s(role)), ("voice", opt_s(voice))],
            ),
            ClipImport {
                id,
                title,
                bpm,
                beats,
                provenance,
            } => o(
                "clipImport",
                vec![
                    ("id", s(id)),
                    ("title", s(title)),
                    ("bpm", n(*bpm)),
                    ("beats", i(*beats)),
                    ("provenance", provenance.clone().unwrap_or(JsonValue::Null)),
                ],
            ),
            MixOut { deck, bars } => o("mixOut", vec![d(deck), ("bars", i(*bars))]),
            MixIn { deck, bars } => o("mixIn", vec![d(deck), ("bars", i(*bars))]),
            Rewind { deck } => o("rewind", vec![d(deck)]),
            Focus { deck } => o("focus", vec![d(deck)]),
            StemMute { deck, stem, on } => {
                o("stemMute", vec![d(deck), ("stem", s(stem)), ("on", b(*on))])
            }
            MasterFx { deck, key, value } => o(
                "masterFx",
                vec![d(deck), ("key", s(key)), ("value", n(*value))],
            ),
            Fx { deck, param, value } => o(
                "fx",
                vec![d(deck), ("param", s(param)), ("value", n(*value))],
            ),
            FxReset { deck, on_beat } => {
                let mut f = vec![d(deck)];
                if *on_beat {
                    f.push(("onBeat", b(true)));
                }
                o("fxReset", f)
            }
            FxKill { deck, on, on_beat } => {
                let mut f = vec![d(deck), ("on", b(*on))];
                if *on_beat {
                    f.push(("onBeat", b(true)));
                }
                o("fxKill", f)
            }
            AutoMix { on, bars, every } => o(
                "autoMix",
                vec![("on", b(*on)), ("bars", i(*bars)), ("every", i(*every))],
            ),
            AutoMixNow => o("autoMixNow", vec![]),
            AutoMixDefer { handoff } => o("autoMixDefer", vec![("handoff", i(*handoff))]),
        }
    }

    /// `DeckAction.init?(json:)`.
    pub fn from_json(v: &JsonValue) -> Option<DeckAction> {
        let op = v.get("op")?.as_str()?;
        let deck = v
            .get("deck")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let st = |k: &str| v.get(k).and_then(JsonValue::as_str).map(String::from);
        let st_or = |k: &str, d: &str| st(k).unwrap_or_else(|| d.to_string());
        let nu = |k: &str, d: f64| v.get(k).and_then(JsonValue::as_f64).unwrap_or(d);
        let it = |k: &str| v.get(k).and_then(JsonValue::as_i64);
        let it_or = |k: &str, d: i64| it(k).unwrap_or(d);
        let bo = |k: &str, d: bool| v.get(k).and_then(JsonValue::as_bool).unwrap_or(d);
        use DeckAction::*;
        Some(match op {
            "play" => Play {
                deck,
                on: bo("on", false),
            },
            "cue" => Cue { deck },
            "hotCue" => HotCue {
                deck,
                slot: it_or("slot", 0),
            },
            "clearCue" => ClearCue {
                deck,
                slot: it_or("slot", 0),
            },
            "loop" => Loop {
                deck,
                op: st_or("loop", ""),
            },
            "param" => Param {
                deck,
                name: st_or("name", ""),
                value: nu("value", 0.0),
            },
            "crossfader" => Crossfader(nu("value", 0.5)),
            "master" => Master(nu("value", 0.8)),
            "load" => Load {
                deck,
                source: DeckSource::from_json(v.get("source")?)?,
            },
            "queueNext" => {
                let spec = TrackSpec::from_payload(v.get("share")?)?;
                QueueNext {
                    deck,
                    spec: spec.with_part_seeds(v.get("partSeeds")),
                }
            }
            "nextTrack" => NextTrack { deck },
            "autoAdvance" => AutoAdvance {
                deck,
                on: bo("on", false),
            },
            "section" => Section {
                deck,
                index: it_or("index", 0),
            },
            "role" => Role {
                deck,
                role: st_or("role", ""),
                on: bo("on", true),
            },
            "roleDisable" => RoleDisable {
                deck,
                role: st_or("role", ""),
                on: bo("on", true),
            },
            "macro" => Macro {
                deck,
                id: st_or("macro", ""),
            },
            "panic" => Panic { deck },
            "stinger" => Stinger {
                deck,
                n: it_or("n", 0),
            },
            "sync" => Sync {
                deck,
                align: st_or("align", "beat"),
            },
            "quantize" => Quantize {
                deck,
                mode: st_or("mode", "off"),
            },
            "follow" => Follow {
                deck,
                leader: st("leader"),
            },
            "grid" => Grid {
                deck,
                bpm: nu("bpm", 120.0),
                offset: nu("offset", 0.0),
            },
            "loopRecord" => LoopRecord {
                deck,
                on: bo("on", false),
                bars: it("bars"),
            },
            "grabBars" => GrabBars {
                deck,
                bars: it_or("bars", 1),
            },
            "clip" => Clip {
                deck,
                id: st_or("id", ""),
            },
            "clipLoop" => ClipLoop {
                id: st_or("id", ""),
                on: bo("on", false),
            },
            "clipGain" => ClipGain {
                id: st_or("id", ""),
                value: nu("value", 1.0),
            },
            "clipSequence" => ClipSequence {
                steps: sequence_steps_from_json(v.get("steps")),
                replacing: st("replacing"),
            },
            "clipCrop" => ClipCrop {
                id: st_or("id", ""),
                from: it_or("from", 0),
                to: it_or("to", 0),
            },
            "clipRetune" => ClipRetune {
                id: st_or("id", ""),
                voices: voices_from_json(v.get("voices")),
            },
            "clipRemove" => ClipRemove {
                id: st_or("id", ""),
            },
            "clipRestore" => ClipRestore {
                id: st_or("id", ""),
                at: it("at"),
            },
            "clipForget" => ClipForget {
                id: st_or("id", ""),
            },
            "clipRename" => ClipRename {
                id: st_or("id", ""),
                title: st_or("title", ""),
            },
            "clipColor" => ClipColor {
                id: st_or("id", ""),
                color: st("color"),
            },
            "loopQuantize" => LoopQuantize {
                beats: it_or("beats", DEFAULT_LOOP_QUANTUM),
            },
            "loopFollow" => LoopFollow {
                mode: st_or("mode", "louder"),
            },
            "sceneSave" => SceneSave {
                name: st_or("name", ""),
            },
            "sceneLaunch" => SceneLaunch {
                name: st_or("name", ""),
                clips: v
                    .get("clips")
                    .and_then(JsonValue::as_array)
                    .map(|a| scene_clips_from_json(Some(&JsonValue::Array(a.clone())))),
            },
            "sceneRename" => SceneRename {
                name: st_or("name", ""),
                to: st_or("to", ""),
            },
            "sceneDelete" => SceneDelete {
                name: st_or("name", ""),
            },
            "clipMute" => ClipMute {
                id: st_or("id", ""),
                on: bo("on", false),
            },
            "clipSolo" => ClipSolo {
                id: st_or("id", ""),
                on: bo("on", false),
            },
            "clipKeyLock" => ClipKeyLock {
                id: st_or("id", ""),
                on: bo("on", false),
            },
            "loopKeyLock" => LoopKeyLock {
                on: bo("on", false),
            },
            "loopFade" => LoopFade {
                mode: st_or("fade", "10ms"),
            },
            "clipQuantize" => ClipQuantize {
                id: st_or("id", ""),
                beats: it("beats"),
            },
            "clipFade" => ClipFade {
                id: st_or("id", ""),
                mode: st("fade"),
            },
            "clipFollow" => ClipFollow {
                id: st_or("id", ""),
                mode: st("mode"),
            },
            "tapBars" => TapBars { bars: it("bars") },
            "voiceTweak" => VoiceTweak {
                deck,
                channel: it("channel")?,
                params: super::types::VoiceTweak::from_json(v.get("params")),
            },
            "voiceSynth" => VoiceSynth {
                deck,
                channel: it("channel")?,
                on: bo("on", false),
            },
            "clipMove" => ClipMove {
                id: st("id")?,
                before: st("before"),
            },
            "lock" => Lock {
                key: st("key")?,
                on: bo("on", true),
            },
            "wander" => Wander {
                deck,
                on: bo("on", false),
            },
            "wanderPace" => WanderPace {
                deck,
                bars: it("bars")?,
            },
            "wanderLock" => WanderLock {
                deck,
                dimension: st("dimension")?,
                on: bo("on", true),
            },
            "setKit" => SetKit {
                deck,
                kit: st("kit"),
            },
            "setPalette" => SetPalette {
                deck,
                palette: st("palette"),
            },
            "clipFx" => ClipFx {
                id: st("id")?,
                fx: LoopFx::from_json(v.get("fx")),
            },
            "loopFxLink" => LoopFxLink {
                on: bo("on", false),
            },
            "setRoleVoice" => {
                let role = st("role").filter(|r| !r.is_empty())?;
                SetRoleVoice {
                    deck,
                    role,
                    voice: st("voice"),
                }
            }
            "clipImport" => ClipImport {
                id: st("id")?,
                title: st_or("title", ""),
                bpm: nu("bpm", 0.0),
                beats: it_or("beats", 0),
                provenance: v
                    .get("provenance")
                    .filter(|p| **p != JsonValue::Null)
                    .cloned(),
            },
            "mixOut" => MixOut {
                deck,
                bars: it_or("bars", MIX_OUT_DEFAULT_BARS),
            },
            "mixIn" => MixIn {
                deck,
                bars: it_or("bars", DEFAULT_MIX_IN_BARS),
            },
            "rewind" => Rewind { deck },
            "focus" => Focus { deck },
            "stemMute" => StemMute {
                deck,
                stem: st_or("stem", "music"),
                on: bo("on", true),
            },
            "masterFx" => MasterFx {
                deck,
                key: st("key")?,
                value: nu("value", 0.0),
            },
            "fx" => Fx {
                deck,
                param: st("param")?,
                value: v.get("value").and_then(JsonValue::as_f64)?,
            },
            "fxReset" => FxReset {
                deck,
                on_beat: bo("onBeat", false),
            },
            "fxKill" => FxKill {
                deck,
                on: bo("on", true),
                on_beat: bo("onBeat", false),
            },
            "autoMix" => {
                // clamped to what the engine offers (`AutoMixSettings`)
                let m = AutoMixSettings::new(
                    bo("on", false),
                    it_or("bars", AutoMixSettings::DEFAULT_BARS),
                    it_or("every", 0),
                );
                AutoMix {
                    on: m.on,
                    bars: m.bars,
                    every: m.every,
                }
            }
            "autoMixNow" => AutoMixNow,
            "autoMixDefer" => AutoMixDefer {
                handoff: it("handoff")?,
            },
            _ => return None,
        })
    }

    /// `DeckAction.op` as the JSON names it.
    pub fn op(&self) -> String {
        self.json()
            .get("op")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// The deck an action addresses, when it addresses one.
    pub fn deck(&self) -> Option<String> {
        self.json()
            .get("deck")
            .and_then(JsonValue::as_str)
            .map(String::from)
    }
}
