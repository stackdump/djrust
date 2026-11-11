//! The loop library's index, without the disk (`Session/LoopLibrary.swift`): one entry per
//! clip (`library.json`'s `loops`, and `removed/removed.json`'s `removed`), the documents
//! they sit in, and the decision `save` makes about what moves into `removed/`. The host
//! reads and writes the files, decodes and encodes the audio ([`LoopClip::wav_data`]) and
//! moves them, in the order [`plan_move_off_the_row`] is written for.
//!
//! Layout: `<dir>/library.json` `{schema: "<ns>/loops/v1", loops: […], scenes?: […]}`,
//! `<dir>/<id>.wav` (float32 stereo), `<dir>/removed/removed.json`
//! `{schema: "<ns>/loops-removed/v1", removed: […]}` (newest first, each entry the
//! clip's own plus `index` and `removed`), `<dir>/removed/<id>.wav`. Both documents are
//! written as their canonical JSON and a newline.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::clips::{ClipLaunch, LoopClip, LoopFade, RemovedClip, WAVEFORM_COLUMNS};
use super::loops::LoopFollow;
use super::turntable::SampleBuffer;
use super::waveform::WaveformOverview;
use crate::journal::state::LoopScene;
use crate::json::{JsonObject, JsonValue};
use crate::session::types::{sequence_steps_from_json, sequence_steps_json, LoopFx};

pub const SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/loops/v1");
pub const REMOVED_SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/loops-removed/v1");

fn s(x: &str) -> JsonValue {
    JsonValue::str(x)
}

/// `LoopLibrary.entry`: a clip's index entry, with its own launch boundary, fade and tempo
/// follow if it has them. `fx` only when it isn't the default, `color`, `steps` and
/// `provenance` only when set, so older indexes read the same.
pub fn entry(c: &LoopClip, launch: &ClipLaunch) -> JsonObject {
    let mut o = JsonObject::from_pairs([
        ("id", s(&c.id)),
        ("title", s(&c.title)),
        ("source", s(&c.source)),
        ("beats", JsonValue::int(c.beats)),
        ("bpm", JsonValue::Number(c.bpm)),
        ("startBeat", JsonValue::Number(c.start_beat)),
    ]);
    if let Some(color) = &c.color {
        o.set("color", s(color));
    }
    if let Some(steps) = &c.steps {
        o.set("steps", sequence_steps_json(steps));
    }
    if let Some(p) = &c.provenance {
        o.set("provenance", p.clone());
    }
    if c.fx != LoopFx::default() {
        o.set("fx", c.fx.json());
    }
    if let Some(q) = launch.quantum {
        o.set("quantum", JsonValue::int(q));
    }
    if let Some(f) = launch.fade {
        o.set("fade", s(f.raw()));
    }
    if let Some(f) = launch.follow {
        o.set("follow", s(f.raw()));
    }
    o
}

/// `LoopLibrary.launch`: the launch boundary, fade and follow a saved entry gives its clip
/// (none when it follows the row; an unknown raw string is none).
pub fn launch_from_entry(e: &JsonValue) -> ClipLaunch {
    ClipLaunch {
        quantum: e.get("quantum").and_then(JsonValue::as_i64),
        fade: e
            .get("fade")
            .and_then(JsonValue::as_str)
            .and_then(LoopFade::from_raw),
        follow: e
            .get("follow")
            .and_then(JsonValue::as_str)
            .and_then(LoopFollow::from_raw),
    }
}

/// `LoopLibrary.loadLaunch`: the saved clips' own launch boundaries, fades and follows, by
/// clip id (only clips that have one), from the index's `loops` entries.
pub fn load_launch(entries: &[JsonValue]) -> BTreeMap<String, ClipLaunch> {
    let mut out = BTreeMap::new();
    for e in entries {
        let Some(id) = e.get("id").and_then(JsonValue::as_str) else {
            continue;
        };
        let l = launch_from_entry(e);
        if !l.is_empty() {
            out.insert(id.to_string(), l);
        }
    }
    out
}

/// `LoopLibrary.clip`: a clip from its index entry and its audio. `audio` decodes the clip's
/// WAV, at the engine's sample rate, for an id; it is asked last, after the entry has an id,
/// a length in beats (`beats`, else the older `bars` times 4; above 0) and a tempo. The clip
/// keeps the id it was saved under: decoded at another sample rate the audio is resampled,
/// and its content id would change with it, so scenes, sequences and the library's own files
/// (all by id) would lose it. Title defaults to "loop", source to "A", start beat to 0.
pub fn clip_from_entry(
    e: &JsonValue,
    audio: impl FnOnce(&str) -> Option<SampleBuffer>,
) -> Option<LoopClip> {
    let id = e.get("id")?.as_str()?;
    let beats = e
        .get("beats")
        .and_then(JsonValue::as_i64)
        .or_else(|| e.get("bars").and_then(JsonValue::as_i64).map(|b| b * 4))?;
    if beats <= 0 {
        return None;
    }
    let bpm = e.get("bpm")?.as_f64()?;
    let buffer = audio(id)?;
    let waveform = WaveformOverview::new(&buffer, WAVEFORM_COLUMNS);
    Some(LoopClip {
        id: id.to_string(),
        title: e
            .get("title")
            .and_then(JsonValue::as_str)
            .unwrap_or("loop")
            .to_string(),
        color: e.get("color").and_then(JsonValue::as_str).map(String::from),
        source: e
            .get("source")
            .and_then(JsonValue::as_str)
            .unwrap_or("A")
            .to_string(),
        beats,
        bpm,
        start_beat: e.get("startBeat").and_then(JsonValue::as_f64).unwrap_or(0.0),
        buffer: Arc::new(buffer),
        waveform,
        steps: e.get("steps").map(|v| sequence_steps_from_json(Some(v))),
        provenance: e.get("provenance").cloned(),
        fx: LoopFx::from_json(e.get("fx")),
    })
}

/// A document of the library as `LoopLibrary.write` writes it: `{schema, <key>: list, …extra}`
/// as canonical JSON and a newline.
pub fn document_text(
    schema: &str,
    key: &str,
    list: Vec<JsonValue>,
    extra: Vec<(&str, JsonValue)>,
) -> String {
    let mut o = JsonObject::from_pairs([("schema", s(schema)), (key, JsonValue::Array(list))]);
    for (k, v) in extra {
        o.set(k, v);
    }
    let mut text = JsonValue::Object(o).canonical_js();
    text.push('\n');
    text
}

/// `library.json` for `clips` in this order: each entry with its launch settings (`launch`,
/// by id), and the `scenes` if there are any.
pub fn index_text(
    clips: &[LoopClip],
    launch: &BTreeMap<String, ClipLaunch>,
    scenes: &[LoopScene],
) -> String {
    let empty = ClipLaunch::default();
    let list = clips
        .iter()
        .map(|c| JsonValue::Object(entry(c, launch.get(&c.id).unwrap_or(&empty))))
        .collect();
    let extra = if scenes.is_empty() {
        Vec::new()
    } else {
        Vec::from([(
            "scenes",
            JsonValue::Array(scenes.iter().map(LoopScene::json).collect()),
        )])
    };
    document_text(SCHEMA, "loops", list, extra)
}

/// What `moveOffTheRow` writes into `removed/removed.json` before it moves any audio.
///
/// `before`: the `loops` entries of the index as it is on disk; `keep`: the ids of the clips
/// that stay on the row; `old`: the entries `removed.json` has now; `stamp`: when (ISO 8601);
/// `has_audio(id)`: does the clip's WAV exist in `removed/` or in the library folder (an
/// entry is kept while its audio is in `removed/`, or about to be moved there);
/// `strays`: how many `.wav` files in the folder are not on the row (they move too).
///
/// None: nothing leaves the row, no stray audio, and no old entry for a clip now on the row,
/// so `removed.json` is left alone. Otherwise the new list, newest first: the entries of the
/// clips leaving the row, each with its `index` on the row and `removed` stamp, then the old
/// ones that are not on the row, not just gone and still have their audio. (An old entry for a
/// clip that is on the row again is dropped: it was put back.)
pub fn plan_move_off_the_row(
    before: &[JsonValue],
    keep: &BTreeSet<String>,
    old: &[JsonValue],
    stamp: &str,
    has_audio: &dyn Fn(&str) -> bool,
    strays: usize,
) -> Option<Vec<JsonValue>> {
    let mut gone: Vec<JsonValue> = Vec::new();
    for (i, e) in before.iter().enumerate() {
        let Some(id) = e.get("id").and_then(JsonValue::as_str) else {
            continue;
        };
        let (false, Some(o)) = (keep.contains(id), e.as_object()) else {
            continue;
        };
        let mut o = o.clone();
        o.set("index", JsonValue::int(i as i64));
        o.set("removed", s(stamp));
        gone.push(JsonValue::Object(o));
    }
    let old_on_row = old.iter().any(|e| {
        e.get("id")
            .and_then(JsonValue::as_str)
            .is_some_and(|id| keep.contains(id))
    });
    if gone.is_empty() && strays == 0 && !old_on_row {
        return None;
    }
    let gone_ids: BTreeSet<&str> = gone
        .iter()
        .filter_map(|e| e.get("id").and_then(JsonValue::as_str))
        .collect();
    let kept = |e: &JsonValue| -> bool {
        e.get("id")
            .and_then(JsonValue::as_str)
            .is_some_and(has_audio)
    };
    let mut list: Vec<JsonValue> = gone.iter().filter(|e| kept(e)).cloned().collect();
    for e in old {
        let Some(id) = e.get("id").and_then(JsonValue::as_str) else {
            continue;
        };
        if !keep.contains(id) && !gone_ids.contains(id) && kept(e) {
            list.push(e.clone());
        }
    }
    Some(list)
}

/// `removed.json` for `list` (see [`plan_move_off_the_row`]).
pub fn removed_index_text(list: Vec<JsonValue>) -> String {
    document_text(REMOVED_SCHEMA, "removed", list, Vec::new())
}

/// A clip in the library's `removed/`, as its entry says (`RemovedLoop`).
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedLoop {
    pub id: String,
    pub title: String,
    pub source: String,
    pub beats: i64,
    pub bpm: f64,
    pub color: Option<String>,
    /// Its place on the row when it was taken off.
    pub index: i64,
    /// When (ISO 8601).
    pub removed: String,
    pub is_sequence: bool,
}

impl RemovedLoop {
    /// `init?(entry:)`: needs an id and a tempo; title defaults to "loop", source to "A",
    /// `beats` to the older `bars` times 4 (0 for neither), index to 0, `removed` to "".
    pub fn from_entry(e: &JsonValue) -> Option<RemovedLoop> {
        let id = e.get("id")?.as_str()?.to_string();
        let bpm = e.get("bpm")?.as_f64()?;
        let int = |k: &str| e.get(k).and_then(JsonValue::as_i64);
        Some(RemovedLoop {
            id,
            title: e
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or("loop")
                .to_string(),
            source: e
                .get("source")
                .and_then(JsonValue::as_str)
                .unwrap_or("A")
                .to_string(),
            beats: int("beats").unwrap_or(int("bars").unwrap_or(0) * 4),
            bpm,
            color: e.get("color").and_then(JsonValue::as_str).map(String::from),
            index: int("index").unwrap_or(0),
            removed: e
                .get("removed")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            is_sequence: e.get("steps").is_some(),
        })
    }

    /// `init(_:removed:)`: the same, for a clip removed in this launch.
    pub fn from_removed(r: &RemovedClip, removed: &str) -> RemovedLoop {
        RemovedLoop {
            id: r.clip.id.clone(),
            title: r.clip.title.clone(),
            source: r.clip.source.clone(),
            beats: r.clip.beats,
            bpm: r.clip.bpm,
            color: r.clip.color.clone(),
            index: r.index,
            removed: removed.to_string(),
            is_sequence: r.clip.steps.is_some(),
        }
    }
}
