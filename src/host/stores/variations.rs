//! `VoiceVariationStore`: the saved variations on disk, one JSON file
//! (`<support>/voices.json`), instrument name → its variations in the order saved. The values
//! and edit functions are `crate::journal::stores`.

use std::io;
use std::path::PathBuf;

use crate::journal::stores::{
    variations_deleting, variations_renaming, variations_saving, Variations, VoiceVariation,
};
use crate::json::{JsonObject, JsonValue};

use super::{read_json, support_dir, write_atomic};

pub const SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/voices/v1");

pub struct VoiceVariationStore {
    pub file: PathBuf,
}

impl VoiceVariationStore {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        VoiceVariationStore { file: file.into() }
    }
    pub fn default_file() -> PathBuf {
        support_dir().join("voices.json")
    }

    /// Every instrument's variations. A missing or unreadable file is none.
    pub fn all(&self) -> Variations {
        let mut out = Variations::new();
        let Some(doc) = read_json(&self.file) else {
            return out;
        };
        let Some(o) = doc.get("instruments").and_then(JsonValue::as_object) else {
            return out;
        };
        for (instrument, list) in o.iter() {
            let vs: Vec<VoiceVariation> = list
                .as_array()
                .map(|a| a.iter().filter_map(VoiceVariation::from_json).collect())
                .unwrap_or_default();
            if !vs.is_empty() {
                out.insert(instrument.to_string(), vs);
            }
        }
        out
    }

    pub fn list(&self, instrument: &str) -> Vec<VoiceVariation> {
        self.all().remove(instrument).unwrap_or_default()
    }

    /// Save `variation` for `instrument` (replacing one of the same name, in its place).
    /// Returns the instrument's new list.
    pub fn save(
        &self,
        variation: &VoiceVariation,
        instrument: &str,
    ) -> io::Result<Vec<VoiceVariation>> {
        let before = self.all();
        let mut after = variations_saving(variation, instrument, &before);
        if after != before {
            self.write(&after)?;
        }
        Ok(after.remove(instrument).unwrap_or_default())
    }

    /// Rename one (a name already taken replaces that one).
    pub fn rename(
        &self,
        name: &str,
        to: &str,
        instrument: &str,
    ) -> io::Result<Vec<VoiceVariation>> {
        let before = self.all();
        let mut after = variations_renaming(name, to, instrument, &before);
        if after != before {
            self.write(&after)?;
        }
        Ok(after.remove(instrument).unwrap_or_default())
    }

    pub fn delete(&self, name: &str, instrument: &str) -> io::Result<Vec<VoiceVariation>> {
        let mut after = variations_deleting(name, instrument, &self.all());
        self.write(&after)?;
        Ok(after.remove(instrument).unwrap_or_default())
    }

    /// Make the file exactly `all` (a journal replay rebuilding the library).
    pub fn replace_all(&self, all: &Variations) -> io::Result<()> {
        self.write(all)
    }

    fn write(&self, all: &Variations) -> io::Result<()> {
        let instruments = JsonObject::from_pairs(all.iter().map(|(k, vs)| {
            (
                k.as_str(),
                JsonValue::Array(vs.iter().map(VoiceVariation::json).collect()),
            )
        }));
        let doc = JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("instruments", JsonValue::Object(instruments)),
        ]));
        write_atomic(&self.file, (doc.js_stringified() + "\n").as_bytes())
    }
}
