//! Where a loop clip came from, kept with it (`LoopProvenance.swift`): what the deck was
//! playing and how it was set when LOOP went down, so a loop can be traced back to its track
//! and recreated. Crops, retunes and sequences say what they were made from.
//!
//! It is plain JSON, written once and never read back by the engine: nothing in it changes a
//! clip's audio or its id. The engine has no clock and no build stamp, so the builders take
//! `recorded` (an ISO 8601 "Z" time with no fractions:
//! [`journal::time::string_seconds`](crate::journal::time::string_seconds)) and `build`
//! (`BuildInfo.current.build`: "dev" unbundled, a short commit in the app) as arguments.
//! The readers are [`LoopClip`]'s `origin_*` accessors.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::beat_grid::BeatGrid;
use super::clips::LoopClip;
use crate::json::{JsonObject, JsonValue};
use crate::layers::StockLoop;
use crate::session::action::voices_json;
use crate::session::model::DeckSource;
use crate::session::types::SequenceStep;

fn s(x: &str) -> JsonValue {
    JsonValue::str(x)
}

/// `finish`: fill in what is known at the end of a take: its length (`bars`) and tempo, and a
/// sample's content id: `sample_content_id` is `buffer.contentID` of what was loaded (None
/// when the deck held no sample), and it replaces the `source`'s id when that is a sample
/// source whose id is "" ("hashing a whole track is not for the press").
pub fn finish(
    mut o: JsonObject,
    bars: i64,
    bpm: f64,
    sample_content_id: Option<&str>,
) -> JsonValue {
    o.set("bars", JsonValue::int(bars));
    o.set("bpm", JsonValue::Number(bpm));
    if let Some(id) = sample_content_id {
        if let Some(JsonValue::Object(src)) = o.get("source") {
            if src.get("kind") == Some(&s("sample")) && src.get("id") == Some(&s("")) {
                let mut src = src.clone();
                src.set("id", s(id));
                o.set("source", JsonValue::Object(src));
            }
        }
    }
    JsonValue::Object(o)
}

/// `stock`: a stock loop (an installed Apple Loop) put on the row as it is
/// (`DJEngine.importClip`): the file and its Apple Loops metadata. `id` is the decoded
/// audio's content id; the source names it as a sample with the loop's own tempo as its grid,
/// so it can go on a deck whole. `kind: "stock"` is what `Session.importedClips` and the
/// publish refusal look for.
pub fn stock(l: &StockLoop, id: &str, build: &str, recorded: &str) -> JsonValue {
    let mut o = JsonObject::from_pairs([
        ("op", s("import")),
        ("kind", s("stock")),
        ("recorded", s(recorded)),
        ("build", s(build)),
        ("title", s(&l.name)),
        ("path", s(&l.path)),
        ("pack", s(&l.pack)),
        ("category", s(&l.category)),
        ("subcategory", s(&l.subcategory)),
        ("genre", s(&l.genre)),
        ("beats", JsonValue::int(l.beats)),
        ("bpm", JsonValue::Number(l.bpm())),
        (
            "source",
            DeckSource::Sample {
                id: id.to_string(),
                title: l.name.clone(),
                grid: Some(BeatGrid::new(l.bpm(), 0.0, 1.0)),
            }
            .json(),
        ),
    ]);
    if let Some(k) = &l.key {
        o.set("key", s(k));
    }
    JsonValue::Object(o)
}

/// `crop`: a crop of `parent`: its beats `from..<to`.
pub fn crop(parent: &LoopClip, from: i64, to: i64) -> JsonValue {
    let mut o = JsonObject::from_pairs([
        ("op", s("crop")),
        ("from", s(&parent.id)),
        (
            "beats",
            JsonValue::Array(Vec::from([JsonValue::int(from), JsonValue::int(to)])),
        ),
    ]);
    if let Some(p) = &parent.provenance {
        o.set("parent", p.clone());
    }
    JsonValue::Object(o)
}

/// `retune`: `parent` rendered again with `voices` (MIDI channel → beats instrument) over the
/// instruments it had (`ClipRetune`): the same bars, so its beats and tempo are the parent's.
pub fn retune(
    parent: &LoopClip,
    voices: &BTreeMap<i64, String>,
    build: &str,
    recorded: &str,
) -> JsonValue {
    let mut o = JsonObject::from_pairs([
        ("op", s("retune")),
        ("from", s(&parent.id)),
        ("recorded", s(recorded)),
        ("build", s(build)),
        ("voices", voices_json(voices)),
    ]);
    if let Some(p) = &parent.provenance {
        o.set("parent", p.clone());
    }
    JsonValue::Object(o)
}

/// `sequence`: a sequence of `steps` (each with its clip's own provenance, so it outlives the
/// clips).
pub fn sequence(steps: &[(SequenceStep, &LoopClip)], recorded: &str) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs([
        ("op", s("sequence")),
        ("recorded", s(recorded)),
        (
            "steps",
            JsonValue::Array(
                steps
                    .iter()
                    .map(|(st, c)| {
                        let mut o = JsonObject::from_pairs([
                            ("clip", s(&c.id)),
                            ("title", s(&c.title)),
                            ("repeats", JsonValue::int(st.repeats)),
                        ]);
                        if let Some(p) = &c.provenance {
                            o.set("provenance", p.clone());
                        }
                        JsonValue::Object(o)
                    })
                    .collect(),
            ),
        ),
    ]))
}
