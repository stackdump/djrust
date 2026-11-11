//! `CompositionStore`: every composition the MCP plays — `dj_compose`, `dj_inspired`,
//! `dj_import` — is kept on disk for reference and replay, one JSON file per track, named
//! `<UTC time>-<title slug>-<share cid[:8]>.json`. A file holds the request that made it and
//! the raw-nets envelope that was loaded. Each top-level key sits on its own line, so the
//! file greps and diffs. The same envelope (same CID) is kept once. Nothing here publishes
//! anything.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::cid;
use crate::journal::time;
use crate::json::{JsonObject, JsonValue};
use crate::setplan::slug;

use super::env_or_support;

/// `CompositionStore.Entry`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The file name without `.json`.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    /// compose | inspired | import
    pub kind: String,
    pub deck: String,
    /// ISO-8601, UTC.
    pub saved: String,
    pub cid: String,
    pub note: Option<String>,
    pub tempo: Option<f64>,
    /// Set planning: how hard it hits (0…1), its key ("A minor") and its length.
    pub energy: Option<f64>,
    pub key: Option<String>,
    pub bars: Option<i64>,
}

/// `CompositionStore.StoreError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    NoMatch(String),
    Ambiguous(String, Vec<String>),
    Unreadable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NoMatch(q) => write!(
                f,
                "no saved composition matches `{q}` (see dj_compositions)"
            ),
            StoreError::Ambiguous(q, ids) => write!(
                f,
                "`{q}` matches {}: {}",
                ids.len(),
                ids.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            ),
            StoreError::Unreadable(id) => write!(f, "can't read the saved composition {id}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub struct CompositionStore {
    pub directory: PathBuf,
}

/// `(path, cid, existed)`.
pub struct Saved {
    pub path: PathBuf,
    pub cid: String,
    pub existed: bool,
}

impl CompositionStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        CompositionStore {
            directory: directory.into(),
        }
    }

    /// `$DJ_COMPOSITIONS_DIR`, else the support folder's `compositions`.
    pub fn default_directory() -> PathBuf {
        env_or_support("DJ_COMPOSITIONS_DIR", "compositions")
    }

    pub fn open_default() -> Self {
        Self::new(Self::default_directory())
    }

    fn stamp(now_ms: i64, compact: bool) -> String {
        let s = time::string_seconds(now_ms); // 2026-10-17T14:32:05Z
        if compact {
            // yyyyMMdd'T'HHmmss'Z'
            format!(
                "{}{}{}T{}{}{}Z",
                &s[0..4],
                &s[5..7],
                &s[8..10],
                &s[11..13],
                &s[14..16],
                &s[17..19]
            )
        } else {
            s
        }
    }

    /// Keep one composition. `request`: what made it (the roll, or the import's source and
    /// report). Returns the file and whether this envelope (its CID) was already kept.
    pub fn save(
        &self,
        kind: &str,
        deck: &str,
        envelope: &JsonValue,
        request: Option<&JsonValue>,
        now_ms: i64,
    ) -> io::Result<Saved> {
        let cid = cid::share_cid(envelope);
        let short: String = cid
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        std::fs::create_dir_all(&self.directory)?;
        let suffix = format!("-{short}.json");
        if let Some(old) = self.files().into_iter().find(|p| {
            p.file_name()
                .map_or(false, |n| n.to_string_lossy().ends_with(&suffix))
        }) {
            return Ok(Saved {
                path: old,
                cid,
                existed: true,
            });
        }
        let name = envelope
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or("untitled");
        let path = self.directory.join(format!(
            "{}-{}-{short}.json",
            Self::stamp(now_ms, true),
            slug(name)
        ));
        let mut head = JsonObject::from_pairs([
            ("v", JsonValue::int(1)),
            ("saved", JsonValue::String(Self::stamp(now_ms, false))),
            ("kind", JsonValue::str(kind)),
            ("deck", JsonValue::str(deck)),
            ("name", JsonValue::str(name)),
            ("cid", JsonValue::str(cid.as_str())),
        ]);
        if let Some(n) = envelope.get("note").and_then(JsonValue::as_str) {
            head.set("note", JsonValue::str(n));
        }
        if let Some(t) = envelope.get("tempo").and_then(JsonValue::as_f64) {
            head.set("tempo", JsonValue::Number(t));
        }
        if let Some(e) = request
            .and_then(|r| r.get("energy"))
            .and_then(JsonValue::as_f64)
        {
            head.set("energy", JsonValue::Number(e.min(1.0).max(0.0)));
        }
        if let Some(k) = request
            .and_then(|r| r.get("key"))
            .and_then(JsonValue::as_str)
            .filter(|k| !k.is_empty())
        {
            head.set("key", JsonValue::str(k));
        }
        if let Some(steps) = envelope
            .get("nets")
            .and_then(JsonValue::as_object)
            .and_then(|o| o.iter().next().map(|(_, n)| n))
            .and_then(|n| n.get("places"))
            .and_then(JsonValue::as_object)
            .map(|p| p.len())
        {
            head.set("bars", JsonValue::int((steps / 16) as i64));
        }
        if let Some(r) = request {
            head.set("request", r.clone());
        }
        head.set("envelope", envelope.clone());
        // one top-level key per line
        let lines: Vec<String> = head
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}:{}",
                    JsonValue::str(k).js_stringified(),
                    v.js_stringified()
                )
            })
            .collect();
        let text = format!("{{\n{}\n}}\n", lines.join(",\n"));
        std::fs::write(&path, text)?;
        Ok(Saved {
            path,
            cid,
            existed: false,
        })
    }

    /// The saved files, newest first.
    fn files(&self) -> Vec<PathBuf> {
        let mut names: Vec<String> = std::fs::read_dir(&self.directory)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by(|a, b| b.cmp(a));
        names.into_iter().map(|n| self.directory.join(n)).collect()
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Entry> {
        self.files()
            .into_iter()
            .filter_map(|path| {
                let doc = read(&path)?;
                let s = |k: &str| {
                    doc.get(k)
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string()
                };
                Some(Entry {
                    id: path.file_stem()?.to_string_lossy().into_owned(),
                    name: s("name"),
                    kind: s("kind"),
                    deck: s("deck"),
                    saved: s("saved"),
                    cid: s("cid"),
                    note: doc
                        .get("note")
                        .and_then(JsonValue::as_str)
                        .map(String::from),
                    tempo: doc.get("tempo").and_then(JsonValue::as_f64),
                    energy: doc.get("energy").and_then(JsonValue::as_f64),
                    key: doc.get("key").and_then(JsonValue::as_str).map(String::from),
                    bars: doc.get("bars").and_then(JsonValue::as_i64),
                    path,
                })
            })
            .collect()
    }

    /// The envelope of one saved composition: `query` is the file id, or any part of it (a
    /// title word, a CID tail) that picks exactly one.
    pub fn envelope(&self, query: &str) -> Result<(Entry, JsonValue), StoreError> {
        let all = self.list();
        let q = query.to_lowercase();
        let exact: Vec<&Entry> = all.iter().filter(|e| e.id.to_lowercase() == q).collect();
        let hits: Vec<&Entry> = if exact.is_empty() {
            all.iter()
                .filter(|e| e.id.to_lowercase().contains(&q))
                .collect()
        } else {
            exact
        };
        if hits.is_empty() {
            return Err(StoreError::NoMatch(query.into()));
        }
        if hits.len() != 1 {
            return Err(StoreError::Ambiguous(
                query.into(),
                hits.iter().map(|e| e.id.clone()).collect(),
            ));
        }
        let e = hits[0];
        let env = read(&e.path)
            .and_then(|d| d.get("envelope").cloned())
            .filter(|env| env.as_object().is_some())
            .ok_or_else(|| StoreError::Unreadable(e.id.clone()))?;
        Ok((e.clone(), env))
    }
}

fn read(path: &Path) -> Option<JsonValue> {
    super::read_json(path)
}
