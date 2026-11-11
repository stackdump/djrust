//! Loop layers: library sounds (vocals, risers, impacts, textures) that a raw track
//! brings in as parallel tracks on the loop channel. Port of `Session/LoopLayers.swift`.
//!
//! A roll (or composition) names them in `loopLayers`:
//! `{"loop": "Heavy Tech Riser 01", "bar": 28, "bars": 4, "gain": -6}`. `bar` is the
//! 1-based bar it comes in on (one, or a list), `bars` how long it stays, `gain` dB under
//! unity (−24…0, default −3), `keyLock` keeps its pitch when the deck's tempo isn't its
//! own. The envelope names the loop, never carries its audio, and a track with layers is
//! local only.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

use crate::json::{JsonObject, JsonValue};
use crate::roll::PianoRollError;
use crate::setplan::SetPlanner;

/// `LoopLayerSpec`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopLayerSpec {
    /// A library path, a loop's name, or a unique part of one.
    pub loop_: String,
    /// The loop's name once resolved.
    pub name: Option<String>,
    pub bars: Vec<i64>,
    pub length: Option<i64>,
    pub gain_db: f64,
    pub key_lock: bool,
    /// Set once resolved: the loop's length in beats and its category.
    pub beats: Option<i64>,
    pub category: Option<String>,
}

impl LoopLayerSpec {
    pub fn new(loop_: &str, bars: Vec<i64>) -> Self {
        LoopLayerSpec {
            loop_: loop_.to_string(),
            name: None,
            bars,
            length: None,
            gain_db: -3.0,
            key_lock: true,
            beats: None,
            category: None,
        }
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("loop", JsonValue::str(self.loop_.as_str()));
        if let Some(n) = &self.name {
            o.set("name", JsonValue::str(n.as_str()));
        }
        o.set(
            "bar",
            JsonValue::Array(self.bars.iter().map(|&b| JsonValue::int(b)).collect()),
        );
        if let Some(l) = self.length {
            o.set("bars", JsonValue::int(l));
        }
        o.set("gain", JsonValue::Number(self.gain_db));
        o.set("keyLock", JsonValue::Bool(self.key_lock));
        if let Some(b) = self.beats {
            o.set("beats", JsonValue::int(b));
        }
        if let Some(c) = &self.category {
            o.set("category", JsonValue::str(c.as_str()));
        }
        JsonValue::Object(o)
    }
}

/// A library sound (`StockLoop`, an installed Apple Loop on the Mac): what `resolve` and `pool`
/// read, and what the stinger resolver ([`crate::sound::stinger`]) matches. Apple Loops are
/// Mac-only content; the type exists so a host that indexes a loop folder of its own can feed
/// both.
#[derive(Clone, Debug, PartialEq)]
pub struct StockLoop {
    pub path: String,
    pub name: String,
    pub pack: String,
    pub category: String,
    pub subcategory: String,
    pub genre: String,
    pub beats: i64,
    pub seconds: f64,
    pub key: Option<String>,
}

impl StockLoop {
    /// Tempo from the loop's own beat count over its length, snapped to a whole BPM when
    /// within 0.1 of one.
    pub fn bpm(&self) -> f64 {
        if self.seconds <= 0.0 {
            return 0.0;
        }
        let raw = self.beats as f64 * 60.0 / self.seconds;
        if (raw - libm::round(raw)).abs() < 0.1 {
            libm::round(raw)
        } else {
            raw
        }
    }
}

/// `LayerLoop`: a library sound a layer plays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LayerLoop {
    pub path: String,
    pub name: String,
    pub category: String,
    pub beats: i64,
}

impl LayerLoop {
    /// The memberwise `LayerLoop(path:name:category:beats:)`.
    pub fn new(path: &str, name: &str, category: &str, beats: i64) -> Self {
        LayerLoop {
            path: path.into(),
            name: name.into(),
            category: category.into(),
            beats,
        }
    }

    pub fn from_stock(l: &StockLoop) -> Self {
        LayerLoop {
            path: l.path.clone(),
            name: l.name.clone(),
            category: l.category.clone(),
            beats: l.beats,
        }
    }
    /// One pass of the loop in bars (at least 1).
    pub fn bars(&self) -> i64 {
        core::cmp::max(1, (self.beats + 3) / 4)
    }
}

/// `LayerEntry`: one window of one loop on a deck's layer list.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerEntry {
    pub loop_: LayerLoop,
    pub start: i64,
    pub length: i64,
    pub gain_db: f64,
    pub key_lock: bool,
}

impl LayerEntry {
    pub fn new(loop_: LayerLoop, start: i64, length: i64, gain_db: f64, key_lock: bool) -> Self {
        LayerEntry {
            loop_,
            start,
            length: core::cmp::max(1, length),
            gain_db,
            key_lock,
        }
    }
    /// The bar after the last one it is on.
    pub fn end(&self) -> i64 {
        self.start + self.length
    }
}

pub const MAX_LAYERS: usize = 12;
pub const ENVELOPE_KEY: &str = "loopLayers";
/// The categories Wander and the agent draw layers from.
pub const LAYER_CATEGORIES: [&str; 3] = ["Vocals", "Sound Effect", "Texture/Atmosphere"];

fn rint(d: f64) -> i64 {
    libm::round(d) as i64
}

/// `LoopLayers.parse`: validate and read a `loopLayers` / `layers` value.
pub fn parse(v: Option<&JsonValue>) -> Result<Vec<LoopLayerSpec>, PianoRollError> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let Some(list) = v.as_array() else {
        return Err(PianoRollError::bad(
            "`layers` must be a list: [{loop, bar, bars?, gain?, keyLock?}, …]",
        ));
    };
    if list.len() > MAX_LAYERS {
        return Err(PianoRollError::bad(&format!(
            "at most {MAX_LAYERS} layers (a mix that many loops wide is mud)"
        )));
    }
    let mut out = Vec::new();
    for (i, l) in list.iter().enumerate() {
        let loop_ = match l.get("loop").and_then(JsonValue::as_str) {
            Some(s) if !s.trim_matches(' ').is_empty() => s,
            _ => {
                return Err(PianoRollError::bad(&format!(
                    "layer {i}: `loop` is the loop's name or path (dj_stock_loops lists them)"
                )))
            }
        };
        let mut starts: Vec<i64> = Vec::new();
        let bar_n = l
            .get("bar")
            .and_then(JsonValue::as_f64)
            .or_else(|| l.get("at").and_then(JsonValue::as_f64));
        if let Some(n) = bar_n {
            starts = vec![rint(n)];
        } else if let Some(a) = l
            .get("bar")
            .or_else(|| l.get("at"))
            .and_then(JsonValue::as_array)
        {
            starts = a.iter().filter_map(|x| x.as_f64().map(rint)).collect();
        }
        if starts.is_empty() || !starts.iter().all(|s| (1..=256).contains(s)) {
            return Err(PianoRollError::bad(&format!(
                "layer {i} ({loop_}): `bar` is the bar it comes in on, 1 or more (or a list)"
            )));
        }
        let mut length = None;
        if let Some(b) = l.get("bars").and_then(JsonValue::as_f64) {
            if !(1..=64).contains(&rint(b)) {
                return Err(PianoRollError::bad(&format!(
                    "layer {i} ({loop_}): `bars` must be 1…64"
                )));
            }
            length = Some(rint(b));
        }
        let gain = l.get("gain").and_then(JsonValue::as_f64).unwrap_or(-3.0);
        if !(-24.0..=0.0).contains(&gain) {
            return Err(PianoRollError::bad(&format!(
                "layer {i} ({loop_}): `gain` is dB under unity, −24…0 (loops can't be boosted)"
            )));
        }
        starts.sort();
        out.push(LoopLayerSpec {
            loop_: loop_.to_string(),
            name: l.get("name").and_then(JsonValue::as_str).map(String::from),
            bars: starts,
            length,
            gain_db: gain,
            key_lock: l
                .get("keyLock")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
            beats: l.get("beats").and_then(JsonValue::as_f64).map(|d| d as i64),
            category: l
                .get("category")
                .and_then(JsonValue::as_str)
                .map(String::from),
        });
    }
    Ok(out)
}

/// `LoopLayers.json`.
pub fn json(specs: &[LoopLayerSpec]) -> JsonValue {
    JsonValue::Array(specs.iter().map(LoopLayerSpec::json).collect())
}

/// `LoopLayers.gain(db:)`: linear clip gain (0…1) for dB under unity.
pub fn gain(db: f64) -> f64 {
    let g = libm::pow(10.0, db.min(0.0) / 20.0);
    g.min(1.0).max(0.0)
}

/// `LoopLayers.windows`: each start with its end (exclusive).
pub fn windows(s: &LoopLayerSpec, loop_beats: i64) -> Vec<(i64, i64)> {
    let n = s
        .length
        .unwrap_or_else(|| core::cmp::max(1, (loop_beats + 3) / 4));
    s.bars.iter().map(|&b| (b, b + n)).collect()
}

/// `LoopLayers.entries(from:)`: a track's layers as windows, sorted by (start, path).
pub fn entries(specs: &[LoopLayerSpec]) -> Vec<LayerEntry> {
    let mut out = Vec::new();
    for s in specs {
        let name = s.name.clone().unwrap_or_else(|| {
            // URL(fileURLWithPath:).deletingPathExtension().lastPathComponent
            let last = s.loop_.rsplit('/').next().unwrap_or("");
            match last.rfind('.') {
                Some(i) if i > 0 => last[..i].to_string(),
                _ => last.to_string(),
            }
        });
        let l = LayerLoop {
            path: s.loop_.clone(),
            name,
            category: s.category.clone().unwrap_or_default(),
            beats: s.beats.unwrap_or(16),
        };
        for &b in &s.bars {
            out.push(LayerEntry::new(
                l.clone(),
                b,
                s.length.unwrap_or_else(|| l.bars()),
                s.gain_db,
                s.key_lock,
            ));
        }
    }
    out.sort_by(|a, b| (a.start, &a.loop_.path).cmp(&(b.start, &b.loop_.path)));
    out
}

/// `LoopLayers.defaultGain(category:)`.
pub fn default_gain(category: &str) -> f64 {
    if category == "Sound Effect" {
        -6.0
    } else {
        -9.0
    }
}

/// `LoopLayers.resolve`: a loop from the library by exact path, exact name, or a unique
/// substring of the name (nearest tempo to `bpm` first when given).
pub fn resolve(
    text: &str,
    library: &[StockLoop],
    near_bpm: Option<f64>,
) -> Result<StockLoop, PianoRollError> {
    let q = text.trim_matches(|c| c == ' ' || c == '\t');
    if let Some(l) = library.iter().find(|l| l.path == q) {
        return Ok(l.clone());
    }
    let ql = q.to_lowercase();
    let exact: Vec<&StockLoop> = library
        .iter()
        .filter(|l| l.name.to_lowercase() == ql)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    let pool: Vec<&StockLoop> = if exact.is_empty() {
        library
            .iter()
            .filter(|l| l.name.to_lowercase().contains(&ql))
            .collect()
    } else {
        exact
    };
    if pool.is_empty() {
        return Err(PianoRollError::bad(&format!(
            "no loop called `{q}` in the library (dj_stock_loops list query=…)"
        )));
    }
    if pool.len() == 1 {
        return Ok(pool[0].clone());
    }
    if let Some(bpm) = near_bpm {
        let mut sorted = pool.clone();
        sorted.sort_by(|a, b| {
            (a.bpm() - bpm)
                .abs()
                .partial_cmp(&(b.bpm() - bpm).abs())
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        if (sorted[0].bpm() - bpm).abs() + 0.5 < (sorted[1].bpm() - bpm).abs() {
            return Ok(sorted[0].clone());
        }
    }
    let names: Vec<String> = pool
        .iter()
        .take(6)
        .map(|l| format!("{} ({})", l.name, l.pack))
        .collect();
    Err(PianoRollError::bad(&format!(
        "`{q}` matches {} loops: {}: use the full name or the path",
        pool.len(),
        names.join("; ")
    )))
}

/// FNV-1a over the path, the order `pool` samples by.
fn path_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A loop's tonic (mode unknown) against the track's key: within a fifth.
pub fn keys_near(loop_key: &str, track_key: &str) -> bool {
    for mode in ["major", "minor"] {
        if let Some(d) = SetPlanner::key_distance(&format!("{loop_key} {mode}"), track_key) {
            if d <= 1 {
                return true;
            }
        }
    }
    false
}

/// `LoopLayers.pool`: the library loops that could go over a track at `bpm` in `key`.
pub fn pool(library: &[StockLoop], bpm: f64, key: Option<&str>, limit: usize) -> Vec<LayerLoop> {
    if bpm <= 0.0 {
        return Vec::new();
    }
    let mut ok: Vec<&StockLoop> = Vec::new();
    for l in library {
        if !LAYER_CATEGORIES.contains(&l.category.as_str()) || l.bpm() <= 0.0 {
            continue;
        }
        let lb = l.bpm();
        if ![1.0, 0.5, 2.0]
            .iter()
            .any(|m| (lb * m / bpm - 1.0).abs() <= 0.08)
        {
            continue;
        }
        match l.key.as_deref() {
            None | Some("") => ok.push(l),
            Some(lk) => {
                if let Some(k) = key {
                    if keys_near(lk, k) {
                        ok.push(l);
                    }
                }
            }
        }
    }
    let mut sampled: Vec<&StockLoop> = if ok.len() <= limit {
        ok
    } else {
        ok.sort_by_key(|l| path_hash(&l.path));
        ok.into_iter().take(limit).collect()
    };
    sampled.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
    sampled.into_iter().map(LayerLoop::from_stock).collect()
}

// MARK: a deck's layer list (DeckLayers.swift)

/// `Deck.resetLayers(from:)`: a new track is on the deck: its layer list is its own
/// `loopLayers` (a raw share's), else empty, and the key to pick layers in is the raw
/// share's `djKey`. A composed deck, a sample and an empty deck give `([], None)`. A
/// `loopLayers` that does not parse gives no layers (`try?`). (The deck's `layerEpoch` bump
/// and its Wander swaps, which carry layers across seed changes, are the host's.)
pub fn reset_layers(source: &crate::session::model::DeckSource) -> (Vec<LayerEntry>, Option<String>) {
    use crate::session::model::DeckSource;
    match source {
        DeckSource::RawNets(payload) => {
            let specs = parse(payload.get(ENVELOPE_KEY)).unwrap_or_default();
            (
                entries(&specs),
                payload
                    .get("djKey")
                    .and_then(JsonValue::as_str)
                    .map(String::from),
            )
        }
        DeckSource::Net(_) | DeckSource::None | DeckSource::Sample { .. } => (Vec::new(), None),
    }
}

/// `Deck.layerKey`: the key to pick layers in: the track's own (`djKey`, `key_name`), else a
/// composed deck's genre scale and root (`wander_key`: `(scale name, root pitch class)` of
/// `Deck.wanderKey`), else unknown. "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A",
/// "Bb", "B" and ` minor` for a scale that is minor, dorian, phrygian, aeolian or blues,
/// else ` major`.
pub fn layer_key(key_name: Option<&str>, wander_key: Option<(&str, i64)>) -> Option<String> {
    if let Some(k) = key_name {
        return Some(k.to_string());
    }
    let (scale, root_pc) = wander_key?;
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
    ];
    let scale = scale.to_lowercase();
    let minor = ["minor", "dorian", "phrygian", "aeolian", "blues"]
        .iter()
        .any(|m| scale.contains(m));
    Some(format!(
        "{} {}",
        NAMES[(((root_pc % 12) + 12) % 12) as usize],
        if minor { "minor" } else { "major" }
    ))
}
