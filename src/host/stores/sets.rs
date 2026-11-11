//! `SetStore`: a named, ordered run of saved compositions laid out on an energy curve, kept
//! at `<support>/sets/<slug>.json` (`$DJ_SETS_DIR`). The models and the planner are
//! `crate::setplan`.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::json::JsonValue;
use crate::setplan::{slug, DjSet};

use super::{env_or_support, pretty_sorted, read_json, write_atomic};

/// `SetStore.StoreError`.
#[derive(Debug, PartialEq, Eq)]
pub enum SetStoreError {
    NoSet(String),
}

impl fmt::Display for SetStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetStoreError::NoSet(n) => {
                write!(
                    f,
                    "no set called `{n}` (dj_set list; dj_set plan creates one)"
                )
            }
        }
    }
}

impl std::error::Error for SetStoreError {}

pub struct SetStore {
    pub directory: PathBuf,
}

impl SetStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        SetStore {
            directory: directory.into(),
        }
    }
    pub fn default_directory() -> PathBuf {
        env_or_support("DJ_SETS_DIR", "sets")
    }
    pub fn open_default() -> Self {
        Self::new(Self::default_directory())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(format!("{}.json", slug(name)))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    pub fn save(&self, set: &DjSet) -> io::Result<()> {
        write_atomic(
            &self.path(&set.name),
            pretty_sorted(&set.to_json()).as_bytes(),
        )
    }

    pub fn load(&self, name: &str) -> Result<DjSet, SetStoreError> {
        read_json(&self.path(name))
            .and_then(|v: JsonValue| DjSet::from_json(&v))
            .ok_or_else(|| SetStoreError::NoSet(name.into()))
    }

    /// All sets, by file name.
    pub fn list(&self) -> Vec<DjSet> {
        let mut names: Vec<String> = std::fs::read_dir(&self.directory)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
            .into_iter()
            .filter_map(|f| read_json(&self.directory.join(f)).and_then(|v| DjSet::from_json(&v)))
            .collect()
    }
}
