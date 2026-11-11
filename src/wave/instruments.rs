//! Port of `Beats/Wave/Instruments.swift`: per-instrument voices beyond beats' wave
//! engine, which folds every tonal instrument into a handful of wavetable families
//! (`tonal_voice`). An entry here overlays that family voice with its own engine and
//! parameters, taken from beats' Tone.js presets (audio/tone-engine.js), so
//! `acoustic-guitar` plucks a string instead of playing the default saw.
//!
//! Data, like the device profiles: `data/instruments.json` (the same file as the Swift
//! `Resources/voices/instruments.json`). The parity path (`WaveOptions::default()`, what
//! the beats fixtures run) does not consult it; the app does (`WaveOptions::extended`).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use super::synth::{tonal_voice, VoiceSpec};
use crate::json::JsonValue;

pub const INSTRUMENTS_JSON: &str = include_str!("../../data/instruments.json");

static SHARED: OnceBox<InstrumentTable> = OnceBox::new();

#[derive(Clone, Debug)]
pub struct InstrumentTable {
    /// Instrument name → overlay (only the fields that differ from its family voice).
    pub entries: BTreeMap<String, JsonValue>,
}

impl InstrumentTable {
    /// The table bundled with the crate, parsed once.
    pub fn shared() -> &'static InstrumentTable {
        SHARED.get_or_init(|| {
            Box::new(
                InstrumentTable::from_json(
                    &JsonValue::parse(INSTRUMENTS_JSON).expect("bundled instruments.json parses"),
                )
                .expect("bundled instruments.json has instruments"),
            )
        })
    }

    pub fn from_json(json: &JsonValue) -> Option<InstrumentTable> {
        let o = json.get("instruments")?.as_object()?;
        let mut entries = BTreeMap::new();
        for (k, v) in o.iter() {
            entries.insert(k.into(), v.clone());
        }
        Some(InstrumentTable { entries })
    }

    /// Sorted names.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// The voice for an instrument: its family voice, overlaid by its entry if it has one.
    pub fn voice(&self, instrument: Option<&str>, group: Option<&str>) -> VoiceSpec {
        let mut v = tonal_voice(instrument, group);
        if let Some(e) = instrument.and_then(|n| self.entries.get(n)) {
            v.overlay(e);
        }
        v
    }
}
