//! A performance as data: the decks' starting contents, the mixer's starting state and the
//! stream of inputs (controller messages and UI actions), each stamped with the engine
//! frame it arrived on. Canonical JSON → CID, the way a beats track is content-addressed
//! (share/codec.js: canonicalizeJSON → sha256 → CIDv1 dag-json), so a set can be named,
//! compared and re-rendered by anyone holding the same samples.
//!
//! Port of `Session/SessionModel.swift` (the model and its JSON; the `DeckAction` codec is
//! [`super::action`]).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use super::action::DeckAction;
use super::types::{AutoMixState, BeatGrid, FxUnitSettings, WanderState};
use crate::cid;
use crate::json::{JsonObject, JsonValue};
use crate::share::Share;
use crate::spec::TrackSpec;

/// `SessionError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// A session starts with every deck stopped at its beginning.
    NotAtRest {
        deck: String,
    },
    /// The deck holds something a session can't name (a hand-built net, not a `TrackSpec`).
    Unrecordable {
        deck: String,
    },
    BadSchema,
    CidMismatch,
    MissingSample {
        id: String,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotAtRest { deck } => write!(f, "deck {deck} is not at rest"),
            SessionError::Unrecordable { deck } => {
                write!(f, "deck {deck} holds an unrecordable track")
            }
            SessionError::BadSchema => f.write_str("not a session"),
            SessionError::CidMismatch => f.write_str("the session does not hash to its CID"),
            SessionError::MissingSample { id } => write!(f, "missing sample {id}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SessionError {}

/// What a deck holds (`DeckSourceDescriptor`).
#[derive(Clone, Debug)]
pub enum DeckSource {
    None,
    /// Audio, by content id. The built-in demo loop is `demo-loop`. The grid is recorded so a
    /// replay doesn't depend on re-running the tempo analysis.
    Sample {
        id: String,
        title: String,
        grid: Option<BeatGrid>,
    },
    Net(TrackSpec),
    /// A raw-`nets` beats share: the envelope payload as given (its CID is `shareCID` of it).
    RawNets(JsonValue),
}

/// `DeckSourceDescriptor.demoID`.
pub const DEMO_ID: &str = "demo-loop";

impl DeckSource {
    /// The seed a raw-`nets` deck picks its sampled instruments with: the envelope's `seed`
    /// when it has a number there, else 0.
    pub fn voicing_seed(raw_nets: &JsonValue) -> f64 {
        raw_nets
            .get("seed")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.0)
    }

    /// `DeckSourceDescriptor.json`.
    pub fn json(&self) -> JsonValue {
        match self {
            DeckSource::None => {
                JsonValue::Object(JsonObject::from_pairs([("kind", JsonValue::str("none"))]))
            }
            DeckSource::Net(spec) => {
                let payload = spec.payload();
                let mut o = JsonObject::from_pairs([
                    ("kind", JsonValue::str("net")),
                    ("share", payload.clone()),
                    ("cid", JsonValue::String(cid::share_cid(&payload))),
                ]);
                if let Some(seeds) = spec.part_seeds_json() {
                    o.set("partSeeds", seeds);
                }
                JsonValue::Object(o)
            }
            DeckSource::RawNets(payload) => JsonValue::Object(JsonObject::from_pairs([
                ("kind", JsonValue::str("rawNets")),
                ("share", payload.clone()),
                ("cid", JsonValue::String(cid::share_cid(payload))),
            ])),
            DeckSource::Sample { id, title, grid } => {
                let mut o = JsonObject::from_pairs([
                    ("kind", JsonValue::str("sample")),
                    ("id", JsonValue::str(id.as_str())),
                    ("title", JsonValue::str(title.as_str())),
                ]);
                if let Some(g) = grid {
                    o.set(
                        "grid",
                        JsonValue::Object(JsonObject::from_pairs([
                            ("bpm", JsonValue::Number(g.bpm)),
                            ("offset", JsonValue::Number(g.offset)),
                        ])),
                    );
                }
                JsonValue::Object(o)
            }
        }
    }

    /// `DeckSourceDescriptor.init?(json:)`.
    pub fn from_json(v: &JsonValue) -> Option<DeckSource> {
        match v.get("kind")?.as_str()? {
            "none" => Some(DeckSource::None),
            "net" => {
                let spec = TrackSpec::from_payload(v.get("share")?)?;
                Some(DeckSource::Net(spec.with_part_seeds(v.get("partSeeds"))))
            }
            "rawNets" => {
                let p = v.get("share")?;
                // `Share(payload: p).nets != nil`
                Share::from_payload(p, None).nets.as_ref()?;
                Some(DeckSource::RawNets(p.clone()))
            }
            "sample" => {
                let id = v.get("id")?.as_str()?.to_string();
                let grid = v.get("grid").and_then(|g| {
                    let bpm = g.get("bpm")?.as_f64()?;
                    Some(BeatGrid::new(
                        bpm,
                        g.get("offset").and_then(JsonValue::as_f64).unwrap_or(0.0),
                        1.0,
                    ))
                });
                Some(DeckSource::Sample {
                    id,
                    title: v
                        .get("title")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string(),
                    grid,
                })
            }
            _ => None,
        }
    }
}

impl PartialEq for DeckSource {
    /// Raw-`nets` payloads are equal when their canonical JSON is (a session read back from
    /// its canonical text has them in sorted key order).
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DeckSource::None, DeckSource::None) => true,
            (
                DeckSource::Sample { id, title, grid },
                DeckSource::Sample {
                    id: i2,
                    title: t2,
                    grid: g2,
                },
            ) => id == i2 && title == t2 && grid == g2,
            (DeckSource::Net(a), DeckSource::Net(b)) => a == b,
            (DeckSource::RawNets(p), DeckSource::RawNets(q)) => {
                p == q || p.canonical_js() == q.canonical_js()
            }
            _ => false,
        }
    }
}

/// Per-deck knobs at the start of a session (`DeckParams`).
#[derive(Clone, Debug, PartialEq)]
pub struct DeckParams {
    pub volume: f64,
    pub gain: f64,
    pub filter: f64,
    pub eq_low: f64,
    pub eq_mid: f64,
    pub eq_high: f64,
    pub pitch: f64,
    pub quantize: String,
    pub auto_advance: bool,
    pub reverb_wet: f64,
    pub reverb_size: f64,
    pub reverb_tone: f64,
    /// Sample loads auto-levelled. Off by default: a session from before level matching
    /// replays its samples at their own level.
    pub auto_gain: bool,
    /// Roles disabled on the deck, sorted.
    pub disabled_roles: Vec<String>,
    /// The deck's walk as the session starts.
    pub wander: WanderState,
    /// The FX unit on the deck's chain; written only when it is not neutral.
    pub fx_unit: FxUnitSettings,
}

impl Default for DeckParams {
    fn default() -> Self {
        DeckParams {
            volume: 0.8,
            gain: 0.5,
            filter: 0.5,
            eq_low: 0.5,
            eq_mid: 0.5,
            eq_high: 0.5,
            pitch: 0.0,
            quantize: "off".to_string(),
            auto_advance: false,
            reverb_wet: 0.0,
            reverb_size: 0.5,
            reverb_tone: 0.5,
            auto_gain: false,
            disabled_roles: Vec::new(),
            wander: WanderState::default(),
            fx_unit: FxUnitSettings::default(),
        }
    }
}

impl DeckParams {
    /// `DeckParams.json`.
    pub fn json(&self) -> JsonValue {
        let n = JsonValue::Number;
        let mut o = JsonObject::from_pairs([
            ("volume", n(self.volume)),
            ("gain", n(self.gain)),
            ("filter", n(self.filter)),
            ("eqLow", n(self.eq_low)),
            ("eqMid", n(self.eq_mid)),
            ("eqHigh", n(self.eq_high)),
            ("pitch", n(self.pitch)),
            ("quantize", JsonValue::str(self.quantize.as_str())),
            ("autoAdvance", JsonValue::Bool(self.auto_advance)),
        ]);
        // only when touched, so sets from before the reverb keep their canonical text and CID
        if !self.disabled_roles.is_empty() {
            o.set(
                "disabledRoles",
                JsonValue::Array(
                    self.disabled_roles
                        .iter()
                        .map(|r| JsonValue::str(r.as_str()))
                        .collect(),
                ),
            );
        }
        if self.wander != WanderState::default() {
            o.set("wander", self.wander.json());
        }
        if self.auto_gain {
            o.set("autoGain", JsonValue::Bool(true));
        }
        if self.fx_unit != FxUnitSettings::default() {
            o.set("fxUnit", self.fx_unit.json());
        }
        if self.reverb_wet != 0.0 || self.reverb_size != 0.5 || self.reverb_tone != 0.5 {
            o.set("reverbWet", n(self.reverb_wet));
            o.set("reverbSize", n(self.reverb_size));
            o.set("reverbTone", n(self.reverb_tone));
        }
        JsonValue::Object(o)
    }

    /// `DeckParams.init(json:)`.
    pub fn from_json(v: &JsonValue) -> DeckParams {
        let mut p = DeckParams::default();
        let num = |k: &str, d: f64| v.get(k).and_then(JsonValue::as_f64).unwrap_or(d);
        p.volume = num("volume", p.volume);
        p.gain = num("gain", p.gain);
        p.filter = num("filter", p.filter);
        p.eq_low = num("eqLow", p.eq_low);
        p.eq_mid = num("eqMid", p.eq_mid);
        p.eq_high = num("eqHigh", p.eq_high);
        p.pitch = num("pitch", p.pitch);
        if let Some(q) = v.get("quantize").and_then(JsonValue::as_str) {
            p.quantize = q.to_string();
        }
        p.auto_advance = v
            .get("autoAdvance")
            .and_then(JsonValue::as_bool)
            .unwrap_or(p.auto_advance);
        p.reverb_wet = num("reverbWet", p.reverb_wet);
        p.reverb_size = num("reverbSize", p.reverb_size);
        p.reverb_tone = num("reverbTone", p.reverb_tone);
        p.auto_gain = v
            .get("autoGain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(p.auto_gain);
        p.disabled_roles = v
            .get("disabledRoles")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(JsonValue::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(w) = v.get("wander") {
            p.wander = WanderState::from_json(w);
        }
        if let Some(f) = v.get("fxUnit") {
            p.fx_unit = FxUnitSettings::from_json(Some(f));
        }
        p
    }
}

/// `SessionEventKind`.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEventKind {
    Midi { port: String, bytes: Vec<u8> },
    Action(DeckAction),
}

/// `SessionEvent`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEvent {
    /// Engine frame (samples rendered since the session began) at which it was applied.
    pub frame: i64,
    pub kind: SessionEventKind,
}

impl SessionEvent {
    pub fn action(frame: i64, a: DeckAction) -> Self {
        SessionEvent {
            frame,
            kind: SessionEventKind::Action(a),
        }
    }
    pub fn midi(frame: i64, port: &str, bytes: &[u8]) -> Self {
        SessionEvent {
            frame,
            kind: SessionEventKind::Midi {
                port: port.to_string(),
                bytes: bytes.to_vec(),
            },
        }
    }

    pub fn json(&self) -> JsonValue {
        match &self.kind {
            SessionEventKind::Midi { port, bytes } => JsonValue::Object(JsonObject::from_pairs([
                ("f", JsonValue::int(self.frame)),
                ("k", JsonValue::str("midi")),
                ("p", JsonValue::str(port.as_str())),
                (
                    "b",
                    JsonValue::Array(bytes.iter().map(|b| JsonValue::int(*b as i64)).collect()),
                ),
            ])),
            SessionEventKind::Action(a) => JsonValue::Object(JsonObject::from_pairs([
                ("f", JsonValue::int(self.frame)),
                ("k", JsonValue::str("act")),
                ("a", a.json()),
            ])),
        }
    }

    pub fn from_json(v: &JsonValue) -> Option<SessionEvent> {
        let f = v.get("f")?.as_i64()?;
        match v.get("k")?.as_str()? {
            "midi" => {
                let p = v.get("p")?.as_str()?.to_string();
                let b = v.get("b")?.ints()?;
                Some(SessionEvent {
                    frame: f,
                    kind: SessionEventKind::Midi {
                        port: p,
                        bytes: b.into_iter().map(|x| x.clamp(0, 255) as u8).collect(),
                    },
                })
            }
            "act" => Some(SessionEvent {
                frame: f,
                kind: SessionEventKind::Action(DeckAction::from_json(v.get("a")?)?),
            }),
            _ => None,
        }
    }
}

/// `Session.schema` (the Swift app still writes the old name; the CID depends on it).
pub const SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/session/v1");

/// L's trim before `DJEngine.loopChannelGain`: unity.
pub const LEGACY_LOOP_GAIN: f64 = 0.5;

/// `Session`.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub sample_rate: f64,
    /// Length of the performance in frames.
    pub frames: i64,
    pub decks: BTreeMap<String, DeckSource>,
    pub params: BTreeMap<String, DeckParams>,
    pub crossfader: f64,
    pub master: f64,
    /// The loop channel's trim knob at the start; written only when it differs from
    /// [`LEGACY_LOOP_GAIN`].
    pub loop_gain: f64,
    /// Auto-mix at the start; written only when not the default.
    pub auto_mix: AutoMixState,
    pub events: Vec<SessionEvent>,
}

impl Session {
    pub fn new(
        sample_rate: f64,
        frames: i64,
        decks: BTreeMap<String, DeckSource>,
        params: BTreeMap<String, DeckParams>,
        crossfader: f64,
        master: f64,
        events: Vec<SessionEvent>,
    ) -> Session {
        Session {
            sample_rate,
            frames,
            decks,
            params,
            crossfader,
            master,
            loop_gain: LEGACY_LOOP_GAIN,
            auto_mix: AutoMixState::default(),
            events,
        }
    }

    pub fn duration(&self) -> f64 {
        self.frames as f64 / self.sample_rate
    }

    /// The envelope as JSON. Key order is irrelevant: the canonical form sorts keys.
    pub fn json(&self) -> JsonValue {
        let mut decks = JsonObject::new();
        for (name, source) in &self.decks {
            let default = DeckParams::default();
            decks.set(
                name.as_str(),
                JsonValue::Object(JsonObject::from_pairs([
                    ("source", source.json()),
                    ("params", self.params.get(name).unwrap_or(&default).json()),
                ])),
            );
        }
        let mut mixer = JsonObject::from_pairs([
            ("crossfader", JsonValue::Number(self.crossfader)),
            ("master", JsonValue::Number(self.master)),
        ]);
        if self.loop_gain != LEGACY_LOOP_GAIN {
            mixer.set("loopGain", JsonValue::Number(self.loop_gain));
        }
        if !self.auto_mix.is_session_default() {
            mixer.set("autoMix", self.auto_mix.session_json());
        }
        JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("sampleRate", JsonValue::Number(self.sample_rate)),
            ("frames", JsonValue::int(self.frames)),
            ("decks", JsonValue::Object(decks)),
            ("mixer", JsonValue::Object(mixer)),
            (
                "events",
                JsonValue::Array(self.events.iter().map(SessionEvent::json).collect()),
            ),
        ]))
    }

    /// `canonicalizeJSON` of the envelope — the bytes the CID hashes.
    pub fn canonical(&self) -> String {
        self.json().canonical_js()
    }

    /// CIDv1 (dag-json, sha2-256, base58btc), the same function beats names a share with.
    pub fn cid(&self) -> String {
        cid::share_cid_canonical(&self.canonical())
    }

    /// `Session.init(json:)`.
    pub fn from_json(v: &JsonValue) -> Result<Session, SessionError> {
        let bad = || SessionError::BadSchema;
        if v.get("schema").and_then(JsonValue::as_str) != Some(SCHEMA) {
            return Err(bad());
        }
        let sr = v
            .get("sampleRate")
            .and_then(JsonValue::as_f64)
            .ok_or_else(bad)?;
        let frames = v
            .get("frames")
            .and_then(JsonValue::as_i64)
            .ok_or_else(bad)?;
        let decks_obj = v
            .get("decks")
            .and_then(JsonValue::as_object)
            .ok_or_else(bad)?;
        let mut decks = BTreeMap::new();
        let mut params = BTreeMap::new();
        for (name, d) in decks_obj.iter() {
            let s = d
                .get("source")
                .and_then(DeckSource::from_json)
                .ok_or_else(bad)?;
            decks.insert(name.to_string(), s);
            params.insert(
                name.to_string(),
                d.get("params")
                    .map(DeckParams::from_json)
                    .unwrap_or_default(),
            );
        }
        let mut events = Vec::new();
        if let Some(arr) = v.get("events").and_then(JsonValue::as_array) {
            for e in arr {
                events.push(SessionEvent::from_json(e).ok_or_else(bad)?);
            }
        }
        let mixer = v.get("mixer");
        let mnum = |k: &str, d: f64| {
            mixer
                .and_then(|m| m.get(k))
                .and_then(JsonValue::as_f64)
                .unwrap_or(d)
        };
        Ok(Session {
            sample_rate: sr,
            frames,
            decks,
            params,
            crossfader: mnum("crossfader", 0.5),
            master: mnum("master", 0.8),
            loop_gain: mnum("loopGain", LEGACY_LOOP_GAIN),
            auto_mix: AutoMixState::from_start_json(mixer.and_then(|m| m.get("autoMix"))),
            events,
        })
    }

    /// `Session.decode`: parse envelope text; with `cid`, the content must hash to it.
    pub fn decode(text: &str, cid: Option<&str>) -> Result<Session, SessionError> {
        let v = JsonValue::parse(text).map_err(|_| SessionError::BadSchema)?;
        if let Some(cid) = cid {
            if cid::share_cid(&v) != cid {
                return Err(SessionError::CidMismatch);
            }
        }
        Session::from_json(&v)
    }
}

/// Content id of decoded audio (`SampleBuffer.contentID`): sha256 over the rate's bit pattern
/// and both channels' float bits, little-endian. A mono buffer (`right` None, or the same
/// samples shared) hashes its one channel.
pub fn sample_content_id(sample_rate: f64, left: &[f32], right: Option<&[f32]>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(sample_rate.to_bits().to_le_bytes());
    for x in left {
        h.update(x.to_bits().to_le_bytes());
    }
    if let Some(r) = right {
        for x in r {
            h.update(x.to_bits().to_le_bytes());
        }
    }
    cid::hex(&h.finalize())
}
