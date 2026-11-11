//! `FavoritesStore` and `NotesStore`: one JSON file each, canonical text plus a newline.
//! The values and edit functions are `crate::journal::stores`.

use std::io;
use std::path::PathBuf;

use crate::journal::stores::{
    favorites_adding, favorites_removing, favorites_setting_tags, notes_adding, notes_deleting,
    notes_editing, Favorite, TrackNote,
};
use crate::json::{JsonObject, JsonValue};

use super::{read_json, support_dir, write_atomic};

pub const FAVORITES_SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/favorites/v1");
pub const NOTES_SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/notes/v1");

pub struct FavoritesStore {
    pub file: PathBuf,
}

impl FavoritesStore {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        FavoritesStore { file: file.into() }
    }
    pub fn default_file() -> PathBuf {
        support_dir().join("favorites.json")
    }

    /// Every favorite, newest first. A missing file is an empty list; entries that no longer
    /// decode are skipped rather than failing the rest.
    pub fn list(&self) -> Vec<Favorite> {
        read_json(&self.file)
            .and_then(|d| d.get("favorites").and_then(JsonValue::as_array).cloned())
            .map(|a| a.iter().filter_map(Favorite::from_json).collect())
            .unwrap_or_default()
    }

    pub fn contains(&self, cid: &str) -> bool {
        self.list().iter().any(|f| f.cid() == cid)
    }

    /// Add (or move to the top, retitled) a favorite. Returns the new list.
    pub fn add(&self, favorite: &Favorite) -> io::Result<Vec<Favorite>> {
        if favorite.cid().is_empty() {
            return Ok(self.list());
        }
        let all = favorites_adding(favorite, &self.list());
        self.write(&all)?;
        Ok(all)
    }

    pub fn remove(&self, cid: &str) -> io::Result<Vec<Favorite>> {
        let all = favorites_removing(cid, &self.list());
        self.write(&all)?;
        Ok(all)
    }

    /// Replace the tags of the favorite with this CID (trimmed, blanks and repeats dropped).
    pub fn set_tags(&self, cid: &str, tags: &[String]) -> io::Result<Vec<Favorite>> {
        let all = favorites_setting_tags(cid, tags, &self.list());
        self.write(&all)?;
        Ok(all)
    }

    /// Make the file exactly `all` (a journal replay rebuilding the store).
    pub fn replace_all(&self, all: &[Favorite]) -> io::Result<()> {
        self.write(all)
    }

    fn write(&self, all: &[Favorite]) -> io::Result<()> {
        let doc = JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(FAVORITES_SCHEMA)),
            (
                "favorites",
                JsonValue::Array(all.iter().map(Favorite::json).collect()),
            ),
        ]));
        write_atomic(&self.file, (doc.canonical_js() + "\n").as_bytes())
    }
}

/// `NotesStore`: the notes on disk, the projection of the journal's note events.
pub struct NotesStore {
    pub file: PathBuf,
}

impl NotesStore {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        NotesStore { file: file.into() }
    }
    pub fn default_file() -> PathBuf {
        support_dir().join("notes.json")
    }

    /// Every note, in the order made. A missing or unreadable file is none.
    pub fn list(&self) -> Vec<TrackNote> {
        read_json(&self.file)
            .and_then(|d| d.get("notes").and_then(JsonValue::as_array).cloned())
            .map(|a| a.iter().filter_map(TrackNote::from_json).collect())
            .unwrap_or_default()
    }

    pub fn add(&self, note: &TrackNote) -> io::Result<Vec<TrackNote>> {
        self.write(notes_adding(&self.list(), note))
    }
    pub fn edit(&self, id: &str, text: &str, at_ms: i64) -> io::Result<Vec<TrackNote>> {
        self.write(notes_editing(&self.list(), id, text, at_ms))
    }
    pub fn delete(&self, id: &str) -> io::Result<Vec<TrackNote>> {
        self.write(notes_deleting(&self.list(), id))
    }
    /// Make the file exactly `all`.
    pub fn replace_all(&self, all: &[TrackNote]) -> io::Result<Vec<TrackNote>> {
        self.write(all.to_vec())
    }

    fn write(&self, all: Vec<TrackNote>) -> io::Result<Vec<TrackNote>> {
        let doc = JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(NOTES_SCHEMA)),
            (
                "notes",
                JsonValue::Array(all.iter().map(TrackNote::json).collect()),
            ),
        ]));
        write_atomic(&self.file, (doc.canonical_js() + "\n").as_bytes())?;
        Ok(all)
    }
}
