//! A piano roll → a raw-`nets` share envelope (`dj_compose`): the "raw / text track
//! format". Port of `Session/PianoRoll.swift`.
//!
//! The caller (an LLM, a script) writes the notes; each part becomes a step-sequencer net
//! the deck plays as given: a ring of N sixteenth-step places, one token, and the
//! transition leaving place `i` carries the MIDI binding of the note that starts on step
//! `i` (no binding = a rest). Rings of one song share the same length, so the parts stay
//! in phase. The envelope then goes the way `loadShare` takes any raw-nets share, so it
//! records, replays and gets a CID like one.
//!
//! Not part of `compose()`: the notes are the configuration, and a raw-nets deck gets no
//! musicality pass.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::json::{JsonObject, JsonValue};
use crate::layers;

/// `PianoRollError.bad`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PianoRollError(pub String);

impl PianoRollError {
    pub fn bad(m: &str) -> Self {
        PianoRollError(m.to_string())
    }
}

impl fmt::Display for PianoRollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PianoRollError {}

pub const MAX_STEPS: i64 = 1024;

/// A role's defaults: net id = role, its channel (beats' own), instrument, and the GM note a
/// drum part plays when a note leaves `note` out.
#[derive(Clone, Debug)]
struct RoleDefaults {
    channel: i64,
    instrument: &'static str,
    drum_note: Option<i64>,
}

fn role_defaults(id: &str) -> RoleDefaults {
    let (channel, instrument, drum_note) = match id {
        "kick" => (10, "drums", Some(36)),
        "snare" => (11, "drums", Some(38)),
        "hihat" => (12, "drums", Some(42)),
        "clap" => (13, "drums", Some(39)),
        "bass" => (6, "sub-bass", None),
        "melody" => (4, "square-lead", None),
        "harmony" => (7, "warm-pad", None),
        "arp" => (5, "electric-piano", None),
        _ => (1, "piano", None),
    };
    RoleDefaults {
        channel,
        instrument,
        drum_note,
    }
}

#[derive(Clone, Copy, Debug)]
struct Note {
    step: i64,
    note: i64,
    duration: i64,
    velocity: i64,
}

struct Segment {
    notes: Vec<Note>,
    from: i64,
    every: Option<i64>,
    until: Option<i64>,
}

struct Parsed {
    id: String,
    channel: i64,
    instrument: String,
    segments: Vec<Segment>,
}

/// `int(_:)`: `v.double.map { Int($0.rounded()) }`.
fn int(v: Option<&JsonValue>) -> Option<i64> {
    v.and_then(JsonValue::as_f64).map(|d| libm::round(d) as i64)
}

/// `PianoRoll.envelope`. `params`:
/// `{name?, tempo?, swing?, steps?, inspiredBy?, brief?, layers?, key?, parts: {role:
/// {instrument?, notes}}}`. A note is `[step, note, dur?, vel?]` or `{step, note?, dur?,
/// vel?}` (steps count sixteenths from 0; `dur` in sixteenths; drum parts may leave `note`
/// out). Returns the envelope.
pub fn envelope(params: &JsonValue) -> Result<JsonValue, PianoRollError> {
    let parts = match params.get("parts").and_then(JsonValue::as_object) {
        Some(p) if !p.is_empty() => p,
        _ => return Err(PianoRollError::bad(
            "`parts` is required: {role: {instrument?, notes: [[step, note, dur?, vel?], …]}}, \
                 roles kick snare hihat clap bass melody harmony arp",
        )),
    };
    // A part is one segment or an array of them: {from?, every?, until?, notes}.
    let mut parsed: Vec<Parsed> = Vec::new();
    for (id, part) in parts.iter() {
        let d = role_defaults(id);
        let single = [part.clone()];
        let list: &[JsonValue] = match part.as_array() {
            Some(a) => a,
            None => &single,
        };
        if list.is_empty() {
            return Err(PianoRollError::bad(&format!(
                "part `{id}` is an empty list"
            )));
        }
        let mut segments = Vec::new();
        let mut channel = d.channel;
        let mut instrument = d.instrument.to_string();
        for seg in list {
            let from = int(seg.get("from")).unwrap_or(0);
            let every = int(seg.get("every"));
            if !(from >= 0 && every.map_or(true, |e| e >= 1)) {
                return Err(PianoRollError::bad(&format!(
                    "part `{id}`: `from` must be ≥ 0 and `every` ≥ 1 step"
                )));
            }
            segments.push(Segment {
                notes: notes_of(seg, id, d.drum_note)?,
                from,
                every,
                until: int(seg.get("until")),
            });
            if let Some(i) = seg.get("instrument").and_then(JsonValue::as_str) {
                instrument = i.to_string();
            }
            if let Some(ch) = int(seg.get("channel")) {
                channel = ch.clamp(1, 16);
            }
        }
        parsed.push(Parsed {
            id: id.to_string(),
            channel,
            instrument,
            segments,
        });
    }

    let explicit = int(params.get("steps"));
    if let Some(e) = explicit {
        if !(1..=MAX_STEPS).contains(&e) {
            return Err(PianoRollError::bad(&format!(
                "`steps` must be 1…{MAX_STEPS} (sixteenths; 16 = one bar)"
            )));
        }
    }
    let mut longest = 0i64;
    for p in &parsed {
        for seg in &p.segments {
            if seg.every.is_some() {
                let Some(end) = seg.until.or(explicit) else {
                    return Err(PianoRollError::bad(&format!(
                        "part `{}`: a repeating pattern (`every`) needs `until` or the roll's `steps`",
                        p.id
                    )));
                };
                longest = longest.max(end);
            } else {
                let reach = seg
                    .notes
                    .iter()
                    .map(|n| n.step + n.duration)
                    .max()
                    .unwrap_or(0);
                longest = longest.max(seg.from + reach);
            }
        }
    }
    let steps = explicit.unwrap_or(if longest <= 0 {
        16
    } else {
        (longest + 15) / 16 * 16
    });
    if steps > MAX_STEPS {
        return Err(PianoRollError::bad(&format!(
            "the roll is {steps} steps; the most is {MAX_STEPS} (64 bars)"
        )));
    }
    // expand the segments into one note list per part
    struct Expanded {
        id: String,
        channel: i64,
        instrument: String,
        notes: Vec<Note>,
    }
    let mut expanded: Vec<Expanded> = Vec::new();
    for p in &parsed {
        let mut notes: Vec<Note> = Vec::new();
        for seg in &p.segments {
            if let Some(every) = seg.every {
                // a pattern repeated from `from` up to `until`
                let bound = steps.min(seg.until.unwrap_or(steps));
                let mut offset = seg.from;
                while offset < bound {
                    for n in &seg.notes {
                        if offset + n.step < bound {
                            let mut m = *n;
                            m.step = offset + n.step;
                            m.duration = n.duration.min(steps - m.step);
                            notes.push(m);
                        }
                    }
                    offset += every;
                }
            } else {
                for n in &seg.notes {
                    let at = seg.from + n.step;
                    if at >= steps {
                        return Err(PianoRollError::bad(&format!(
                            "a note starts on step {at}, past `steps` {steps}"
                        )));
                    }
                    let mut m = *n;
                    m.step = at;
                    m.duration = n.duration.min(steps - at);
                    notes.push(m);
                }
            }
        }
        expanded.push(Expanded {
            id: p.id.clone(),
            channel: p.channel,
            instrument: p.instrument.clone(),
            notes,
        });
    }

    let mut nets = JsonObject::new();
    for p in &expanded {
        // One note starts per step per net: chord tones (notes on the same step) spill into
        // sibling nets (`melody-b`, …) grouped under the same role, played together.
        let mut lanes: Vec<BTreeMap<i64, Note>> = Vec::new();
        let mut sorted = p.notes.clone();
        sorted.sort_by_key(|n| (n.step, n.note));
        for n in sorted {
            if let Some(lane) = lanes.iter_mut().find(|l| !l.contains_key(&n.step)) {
                lane.insert(n.step, n);
            } else {
                let mut l = BTreeMap::new();
                l.insert(n.step, n);
                lanes.push(l);
            }
        }
        if lanes.is_empty() {
            lanes.push(BTreeMap::new());
        }
        for (i, lane) in lanes.iter().enumerate() {
            let id = if i == 0 {
                p.id.clone()
            } else {
                format!("{}-{}", p.id, (97 + i.min(25) as u8) as char)
            };
            let group = if i == 0 {
                None
            } else {
                Some((p.id.as_str(), id.as_str()))
            };
            nets.set(
                id.as_str(),
                ring(steps, lane, p.channel, &p.instrument, group),
            );
        }
    }

    let mut env = JsonObject::new();
    // `dj_inspired`: an original piece in an artist's general style, in the share's note.
    let inspired_by = params
        .get("inspiredBy")
        .and_then(JsonValue::as_str)
        .filter(|s| !s.is_empty());
    let name = match params.get("name").and_then(JsonValue::as_str) {
        Some(n) => n.to_string(),
        None => match inspired_by {
            Some(i) => format!("{i}-style (original)"),
            None => "Piano roll".to_string(),
        },
    };
    env.set("name", JsonValue::String(name));
    if let Some(i) = inspired_by {
        let mut note =
            format!("Original piece inspired by {i}: style only, no melody or lyric taken.");
        if let Some(b) = params
            .get("brief")
            .and_then(JsonValue::as_str)
            .filter(|s| !s.is_empty())
        {
            note.push_str(&format!(" Brief: {b}"));
        }
        env.set("note", JsonValue::String(note));
    }
    if let Some(t) = params.get("tempo").and_then(JsonValue::as_f64) {
        if !(40.0..=300.0).contains(&t) {
            return Err(PianoRollError::bad("`tempo` must be 40…300 BPM"));
        }
        env.set("tempo", JsonValue::Number(t));
    }
    if let Some(s) = params.get("swing").and_then(JsonValue::as_f64) {
        env.set("swing", JsonValue::Number(s));
    }
    // the key the author said it is in (`djKey`): layer moves pick tonal loops that fit it
    if let Some(k) = params
        .get("key")
        .and_then(JsonValue::as_str)
        .filter(|s| !s.is_empty())
    {
        env.set("djKey", JsonValue::str(k));
    }
    env.set("nets", JsonValue::Object(nets));
    // library sounds that come in over the track as parallel tracks
    let layer_src = params
        .get("layers")
        .or_else(|| params.get(layers::ENVELOPE_KEY));
    let specs = layers::parse(layer_src)?;
    if !specs.is_empty() {
        env.set(layers::ENVELOPE_KEY, layers::json(&specs));
    }
    Ok(JsonValue::Object(env))
}

fn notes_of(
    part: &JsonValue,
    id: &str,
    drum_note: Option<i64>,
) -> Result<Vec<Note>, PianoRollError> {
    let Some(list) = part.get("notes").and_then(JsonValue::as_array) else {
        return Err(PianoRollError::bad(&format!(
            "part `{id}` needs `notes`: [[step, note, dur?, vel?], …]"
        )));
    };
    let mut out = Vec::new();
    for (k, raw) in list.iter().enumerate() {
        let (step, note, dur, vel);
        if let Some(a) = raw.as_array() {
            step = int(a.first());
            note = if a.len() > 1 { int(a.get(1)) } else { None };
            dur = if a.len() > 2 { int(a.get(2)) } else { None };
            vel = if a.len() > 3 { int(a.get(3)) } else { None };
        } else {
            step = int(raw.get("step"));
            note = int(raw.get("note"));
            dur = int(raw.get("dur").or_else(|| raw.get("duration")));
            vel = int(raw.get("vel").or_else(|| raw.get("velocity")));
        }
        let step = match step {
            Some(s) if s >= 0 => s,
            _ => {
                return Err(PianoRollError::bad(&format!(
                    "part `{id}` note {k}: `step` must be an integer ≥ 0"
                )))
            }
        };
        let n = match note.or(drum_note) {
            Some(n) if (0..=127).contains(&n) => n,
            _ => {
                return Err(PianoRollError::bad(&format!(
                    "part `{id}` note {k}: `note` must be a MIDI number 0…127"
                )))
            }
        };
        out.push(Note {
            step,
            note: n,
            duration: dur
                .unwrap_or(if drum_note.is_none() { 2 } else { 1 })
                .max(1),
            velocity: vel.unwrap_or(100).clamp(1, 127),
        });
    }
    Ok(out)
}

fn ring(
    steps: i64,
    lane: &BTreeMap<i64, Note>,
    channel: i64,
    instrument: &str,
    group: Option<(&str, &str)>,
) -> JsonValue {
    let mut places = JsonObject::new();
    let mut transitions = JsonObject::new();
    let mut arcs: Vec<JsonValue> = Vec::new();
    for i in 0..steps {
        let mut p = JsonObject::new();
        if i == 0 {
            p.set("initial", JsonValue::Array(alloc::vec![JsonValue::int(1)]));
        }
        places.set(format!("p{i}"), JsonValue::Object(p));
        let mut t = JsonObject::new();
        if let Some(n) = lane.get(&i) {
            t.set(
                "midi",
                JsonValue::Object(JsonObject::from_pairs([
                    ("note", JsonValue::int(n.note)),
                    ("channel", JsonValue::int(channel)),
                    ("velocity", JsonValue::int(n.velocity)),
                    ("durationSteps", JsonValue::int(n.duration)),
                ])),
            );
        }
        transitions.set(format!("t{i}"), JsonValue::Object(t));
        arcs.push(JsonValue::Object(JsonObject::from_pairs([
            ("source", JsonValue::String(format!("p{i}"))),
            ("target", JsonValue::String(format!("t{i}"))),
        ])));
        arcs.push(JsonValue::Object(JsonObject::from_pairs([
            ("source", JsonValue::String(format!("t{i}"))),
            ("target", JsonValue::String(format!("p{}", (i + 1) % steps))),
        ])));
    }
    let mut track = JsonObject::from_pairs([
        ("channel", JsonValue::int(channel)),
        ("instrument", JsonValue::str(instrument)),
    ]);
    if let Some((role, _)) = group {
        track.set("group", JsonValue::str(role));
    }
    let mut net = JsonObject::from_pairs([
        ("track", JsonValue::Object(track)),
        ("places", JsonValue::Object(places)),
        ("transitions", JsonValue::Object(transitions)),
        ("arcs", JsonValue::Array(arcs)),
    ]);
    if let Some((role, id)) = group {
        net.set("riffGroup", JsonValue::str(role));
        let variant = id
            .split('-')
            .filter(|s| !s.is_empty())
            .last()
            .unwrap_or("b");
        net.set("riffVariant", JsonValue::str(variant));
    }
    JsonValue::Object(net)
}
