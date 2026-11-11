//! What the engine was like when an arrangement starts (beyond Swift; docs/HEADLESS.md): the
//! arrangement's optional `"start"` object. `decks` says what each deck holds; this says the
//! rest a render needs to sound like the room did at beat 0: the mixer, every deck's knobs,
//! which decks were playing, the input strips, where the decks' audio files are, the capture
//! library the loop row came from, and the moves that set up the parts no `DeckParams` holds
//! (the vocoder's sources, knobs and switches, a trick left on).
//!
//! ```json
//! "start": {
//!   "crossfader": 0, "master": 0.8, "loop_gain": 1,
//!   "params": { "A": {...DeckParams...}, "B": {...} },
//!   "playing": ["A"],
//!   "inputs": [ {"label": "TD-3", "history_beats": 16, "file": "/…/td3.wav", "file_start": 0} ],
//!   "samples": { "<content id>": "/path/to/file.wav" },
//!   "loops_dir": "/…/captures", "loop_row": true,
//!   "setup": [ {"op": "vocoder", "carrier": "synth"}, {"op": "vocoder", "on": true} ]
//! }
//! ```
//!
//! Every field is optional: an arrangement written by hand, or recorded before `start` existed,
//! renders on an engine's defaults, with each deck playing from beat 0 unless its first `play`
//! move turns it on (then it starts stopped).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::arrangement::ArrangeOp;
use super::model::DeckParams;
use crate::json::{JsonObject, JsonValue};

/// An input strip the arrangement's engine has.
#[derive(Clone, Debug, PartialEq)]
pub struct StartInput {
    pub label: String,
    /// Beats of history it keeps (the freeze, captures and `capture:` modulators read it).
    pub history_beats: Option<f64>,
    /// The audio the strip heard, recorded to a mono WAV while the set was played
    /// (`djapp --record-inputs`): an offline render feeds it to the strip, so a history scrub
    /// or a capture to loop takes the same audio it took live.
    pub file: Option<String>,
    /// The sample of `file` that falls on beat 0 (the file may have started earlier).
    pub file_start: u64,
}

/// The arrangement's `start` (all optional).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ArrangeStart {
    pub crossfader: Option<f64>,
    pub master: Option<f64>,
    pub loop_gain: Option<f64>,
    /// Each deck's knobs (and the loop channel's, as `L`).
    pub params: BTreeMap<String, DeckParams>,
    /// The decks playing at beat 0. None: each deck plays unless its first `play` turns it on.
    pub playing: Option<Vec<String>>,
    pub inputs: Vec<StartInput>,
    /// A sample deck's content id → the audio file it was loaded from.
    pub samples: BTreeMap<String, String>,
    /// The capture library the loop row and `library:` sources read.
    pub loops_dir: Option<String>,
    /// Fill the loop row from the library at the start, as djapp does when it opens.
    pub loop_row: bool,
    /// Moves made before beat 0, in order (live ops, `DeckAction`s).
    pub setup: Vec<ArrangeOp>,
}

impl ArrangeStart {
    pub fn is_empty(&self) -> bool {
        *self == ArrangeStart::default()
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        let mut num = |k: &str, v: Option<f64>| {
            if let Some(v) = v {
                o.set(k, JsonValue::Number(v));
            }
        };
        num("crossfader", self.crossfader);
        num("master", self.master);
        num("loop_gain", self.loop_gain);
        if !self.params.is_empty() {
            let mut p = JsonObject::new();
            for (k, v) in &self.params {
                p.set(k.clone(), v.json());
            }
            o.set("params", JsonValue::Object(p));
        }
        if let Some(pl) = &self.playing {
            o.set(
                "playing",
                JsonValue::Array(pl.iter().map(|d| JsonValue::str(d.clone())).collect()),
            );
        }
        if !self.inputs.is_empty() {
            o.set(
                "inputs",
                JsonValue::Array(
                    self.inputs
                        .iter()
                        .map(|i| {
                            let mut x = JsonObject::from_pairs([(
                                "label",
                                JsonValue::str(i.label.clone()),
                            )]);
                            if let Some(b) = i.history_beats {
                                x.set("history_beats", JsonValue::Number(b));
                            }
                            if let Some(f) = &i.file {
                                x.set("file", JsonValue::str(f.clone()));
                                if i.file_start > 0 {
                                    x.set("file_start", JsonValue::Number(i.file_start as f64));
                                }
                            }
                            JsonValue::Object(x)
                        })
                        .collect(),
                ),
            );
        }
        if !self.samples.is_empty() {
            let mut s = JsonObject::new();
            for (k, v) in &self.samples {
                s.set(k.clone(), JsonValue::str(v.clone()));
            }
            o.set("samples", JsonValue::Object(s));
        }
        if let Some(d) = &self.loops_dir {
            o.set("loops_dir", JsonValue::str(d.clone()));
        }
        if self.loop_row {
            o.set("loop_row", JsonValue::Bool(true));
        }
        if !self.setup.is_empty() {
            o.set(
                "setup",
                JsonValue::Array(self.setup.iter().map(ArrangeOp::json).collect()),
            );
        }
        JsonValue::Object(o)
    }

    /// Read `start` back; fields that don't parse are left out.
    pub fn from_json(v: &JsonValue) -> ArrangeStart {
        let f = |k: &str| v.get(k).and_then(JsonValue::as_f64);
        let mut params = BTreeMap::new();
        if let Some(o) = v.get("params").and_then(JsonValue::as_object) {
            for (k, p) in o.iter() {
                params.insert(k.to_string(), DeckParams::from_json(p));
            }
        }
        let mut samples = BTreeMap::new();
        if let Some(o) = v.get("samples").and_then(JsonValue::as_object) {
            for (k, p) in o.iter() {
                if let Some(p) = p.as_str() {
                    samples.insert(k.to_string(), p.to_string());
                }
            }
        }
        let inputs = v
            .get("inputs")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|i| match i {
                        JsonValue::String(l) => Some(StartInput {
                            label: l.clone(),
                            history_beats: None,
                            file: None,
                            file_start: 0,
                        }),
                        _ => Some(StartInput {
                            label: i.get("label")?.as_str()?.to_string(),
                            history_beats: i.get("history_beats").and_then(JsonValue::as_f64),
                            file: i.get("file").and_then(JsonValue::as_str).map(String::from),
                            file_start: i
                                .get("file_start")
                                .and_then(JsonValue::as_f64)
                                .map_or(0, |v| v.max(0.0) as u64),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default();
        ArrangeStart {
            crossfader: f("crossfader"),
            master: f("master"),
            loop_gain: f("loop_gain"),
            params,
            playing: v.get("playing").and_then(JsonValue::strings),
            inputs,
            samples,
            loops_dir: v
                .get("loops_dir")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
            loop_row: v
                .get("loop_row")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            setup: v
                .get("setup")
                .and_then(JsonValue::as_array)
                .map(|a| a.iter().filter_map(ArrangeOp::from_json).collect())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_round_trips_and_an_empty_one_writes_nothing() {
        assert_eq!(ArrangeStart::default().json().js_stringified(), "{}");
        let mut s = ArrangeStart {
            crossfader: Some(0.0),
            master: Some(0.8),
            playing: Some(alloc::vec!["A".into()]),
            loops_dir: Some("/x".into()),
            loop_row: true,
            ..Default::default()
        };
        s.params.insert("A".into(), DeckParams::default());
        s.inputs.push(StartInput {
            label: "TD-3".into(),
            history_beats: Some(16.0),
            file: Some("/in/td3.wav".into()),
            file_start: 44100,
        });
        s.samples.insert("abc".into(), "/a.wav".into());
        let back = ArrangeStart::from_json(&s.json());
        assert_eq!(back, s);
        // a bare label is an input with no history
        let v = JsonValue::parse(r#"{"inputs":["mic"]}"#).unwrap();
        assert_eq!(ArrangeStart::from_json(&v).inputs[0].label, "mic");
    }
}
