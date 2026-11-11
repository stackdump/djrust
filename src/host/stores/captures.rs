//! The capture library: loops captured from an input that persist across restarts
//! (docs/INPUT-HISTORY.md). One pair of files per loop in `$DJ_CAPTURES_DIR` (else
//! `<support>/captures`): `<id>.wav` (the audio,
//! 32-bit float, mono for an input) and `<id>.json` (`crate::looplib::LoopMeta`: tempo, beats,
//! source input, whether it returns to the row at start). The id is a hash of the audio, so
//! saving the same capture twice is one entry.
//!
//! Not the LOOP row's library ([`super::loops::LoopLibrary`], `<support>/loops`, the Swift
//! app's): that one moves any WAV it does not index into `removed/`, so the two cannot share a
//! folder.
//!
//! The audio is the user's own recording; it stays out of git (`*.wav` is already ignored) and is
//! never published (docs/HUB.md, rule 4).

use std::io;
use std::path::{Path, PathBuf};

use crate::deck::turntable::SampleBuffer;
use crate::looplib::{wav_decode, wav_encode, LoopMeta};

use super::{env_or_support, pretty_sorted, read_json, write_atomic};

pub struct CaptureLibrary {
    pub directory: PathBuf,
}

/// A loop read back from disk.
pub struct SavedLoop {
    pub meta: LoopMeta,
    pub buffer: SampleBuffer,
}

impl CaptureLibrary {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        CaptureLibrary {
            directory: directory.into(),
        }
    }
    pub fn default_directory() -> PathBuf {
        env_or_support("DJ_CAPTURES_DIR", "captures")
    }
    pub fn open_default() -> Self {
        Self::new(Self::default_directory())
    }

    fn wav_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.wav"))
    }
    fn json_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }

    /// Write a loop: audio first, then metadata, so a half-written entry has no metadata and is
    /// invisible. `right: None` is mono. The id must be a plain file stem.
    pub fn save(&self, meta: &LoopMeta, left: &[f32], right: Option<&[f32]>) -> io::Result<()> {
        if meta.id.is_empty() || meta.id.contains(['/', '\\', '.']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad loop id"));
        }
        let wav = wav_encode(meta.sample_rate as u32, left, right);
        write_atomic(&self.wav_path(&meta.id), &wav)?;
        self.write_meta(meta)
    }

    /// Rewrite just the metadata (a rename, whether it returns to the row).
    pub fn write_meta(&self, meta: &LoopMeta) -> io::Result<()> {
        let text = pretty_sorted(&meta.to_json()) + "\n";
        write_atomic(&self.json_path(&meta.id), text.as_bytes())
    }

    /// Every entry with readable metadata, oldest first (then by id). Entries that do not
    /// decode are skipped.
    pub fn list(&self) -> Vec<LoopMeta> {
        let Ok(rd) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut v: Vec<LoopMeta> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| read_json(&p))
            .filter_map(|j| LoopMeta::from_json(&j))
            .filter(|m| self.wav_path(&m.id).is_file())
            .collect();
        v.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
        v
    }

    pub fn meta(&self, id: &str) -> Option<LoopMeta> {
        read_json(&self.json_path(id)).and_then(|j| LoopMeta::from_json(&j))
    }

    /// Read one loop, resampled to `sample_rate` if it was saved at another (its beats and tempo
    /// are kept: only the rate changes).
    pub fn load(&self, id: &str, sample_rate: f64) -> Option<SavedLoop> {
        let mut meta = self.meta(id)?;
        let wav = wav_decode(&std::fs::read(self.wav_path(id)).ok()?)?;
        let rate = wav.sample_rate as f64;
        let conv = |x: Vec<f32>| {
            if (rate - sample_rate).abs() < 0.5 {
                x
            } else {
                crate::host::decode::resample(&x, rate, sample_rate)
            }
        };
        let left = conv(wav.left);
        let right = wav.right.map(conv);
        if left.is_empty() {
            return None;
        }
        meta.frames = left.len();
        meta.sample_rate = sample_rate;
        Some(SavedLoop {
            meta,
            buffer: SampleBuffer::new(left, right, sample_rate),
        })
    }

    /// Delete an entry's files. Returns whether there was one.
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        if id.is_empty() || id.contains(['/', '\\', '.']) {
            return Ok(false);
        }
        let had = self.json_path(id).exists() || self.wav_path(id).exists();
        for p in [self.json_path(id), self.wav_path(id)] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(had)
    }

    pub fn exists(&self) -> bool {
        Path::new(&self.directory).is_dir()
    }
}
