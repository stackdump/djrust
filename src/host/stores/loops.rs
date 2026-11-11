//! The LOOP-record clips on disk, so the row under the mixer comes back on the next launch
//! (`Session/LoopLibrary.swift`): one float WAV per clip, named by its content id, and an index
//! (`library.json`) that keeps the row's order and each clip's beats, tempo, source, name,
//! colour and FX, its own launch boundary, fade and tempo follow where it has them, and the
//! saved scenes.
//!
//! Nothing is deleted by taking a clip off the row: its WAV moves into `removed/` with its
//! index entry (`removed/removed.json`, newest first, with the place it had and when), so it can
//! be put back. Only [`LoopLibrary::delete_forever`] deletes audio.
//!
//! The entries, the documents and the decision about what moves are `crate::deck::library`;
//! this is the disk half. The folder is a parameter: [`LoopLibrary::default_dir`] is
//! `<support dir>/loops`, the Swift app's own folder on a Mac. Two apps writing one
//! `library.json` are not safe.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::deck::clips::{ClipLaunch, LoopClip};
use crate::deck::library::{
    clip_from_entry, index_text, launch_from_entry, load_launch, plan_move_off_the_row,
    removed_index_text, RemovedLoop,
};
use crate::journal::state::LoopScene;
use crate::json::JsonValue;

use super::{read_json, support_dir, write_atomic};
use crate::host::decode::decode_audio;

pub struct LoopLibrary {
    pub dir: PathBuf,
}

impl LoopLibrary {
    pub fn new(dir: impl Into<PathBuf>) -> LoopLibrary {
        LoopLibrary { dir: dir.into() }
    }

    /// `<support dir>/loops` (`LoopLibrary.defaultDir`).
    pub fn default_dir() -> PathBuf {
        support_dir().join("loops")
    }

    pub fn index_file(&self) -> PathBuf {
        self.dir.join("library.json")
    }

    pub fn audio_file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.wav"))
    }

    pub fn removed_dir(&self) -> PathBuf {
        self.dir.join("removed")
    }

    pub fn removed_index_file(&self) -> PathBuf {
        self.removed_dir().join("removed.json")
    }

    pub fn removed_audio_file(&self, id: &str) -> PathBuf {
        self.removed_dir().join(format!("{id}.wav"))
    }

    /// `entries(file, key:)`: the array under `key` of an index (none without a readable one).
    fn entries(file: &Path, key: &str) -> Vec<JsonValue> {
        read_json(file)
            .and_then(|d| d.get(key).and_then(JsonValue::as_array).map(|a| a.to_vec()))
            .unwrap_or_default()
    }

    /// The saved clips in row order, decoded (and resampled) to `sample_rate`. A missing index
    /// is an empty library; an entry whose audio is gone or unreadable is skipped.
    pub fn load(&self, sample_rate: f64) -> Vec<LoopClip> {
        Self::entries(&self.index_file(), "loops")
            .iter()
            .filter_map(|e| {
                clip_from_entry(e, |id| decode_audio(&self.audio_file(id), sample_rate).ok())
            })
            .collect()
    }

    /// The saved clips' own launch boundaries, fades and follows, by clip id (only clips that
    /// have one).
    pub fn load_launch(&self) -> BTreeMap<String, ClipLaunch> {
        load_launch(&Self::entries(&self.index_file(), "loops"))
    }

    /// The saved scenes, in order (none without an index).
    pub fn load_scenes(&self) -> Vec<LoopScene> {
        Self::entries(&self.index_file(), "scenes")
            .iter()
            .filter_map(LoopScene::from_json)
            .collect()
    }

    /// Make the row on disk exactly `clips`, in this order: write the audio of any clip not
    /// saved yet (or move it back out of `removed/`), the index, and move the audio of clips no
    /// longer on the row into `removed/`, each with its entry, its place and when
    /// (`removed_at_ms`, Unix milliseconds). `scenes`: the scenes to keep with them (None keeps
    /// the ones already saved). `launch`: the clips' own launch boundaries and fades, by id
    /// (`Engine::clip_launch`; None keeps what each saved entry says).
    ///
    /// The order is chosen so a crash (or a failed move) at any step leaves every clip
    /// findable: `removed.json` gets the entries of the clips leaving the row first, then their
    /// audio moves, and only then is `library.json` written without them. Until then the old
    /// index still names them (and the next save tries again); an entry in both is shown on the
    /// row.
    pub fn save(
        &self,
        clips: &[LoopClip],
        scenes: Option<&[LoopScene]>,
        launch: Option<&BTreeMap<String, ClipLaunch>>,
        removed_at_ms: i64,
    ) -> io::Result<()> {
        let scenes = scenes.map(|s| s.to_vec()).unwrap_or_else(|| self.load_scenes());
        std::fs::create_dir_all(&self.dir)?;
        let before = Self::entries(&self.index_file(), "loops");
        let launch = launch.cloned().unwrap_or_else(|| self.load_launch());
        for c in clips {
            if !self.audio_file(&c.id).exists() {
                if self.removed_audio_file(&c.id).exists() {
                    std::fs::rename(self.removed_audio_file(&c.id), self.audio_file(&c.id))?;
                } else {
                    write_atomic(&self.audio_file(&c.id), &c.wav_data())?;
                }
            }
        }
        let keep: BTreeSet<String> = clips.iter().map(|c| c.id.clone()).collect();
        self.move_off_the_row(&keep, &before, removed_at_ms)?;
        write_atomic(
            &self.index_file(),
            index_text(clips, &launch, &scenes).as_bytes(),
        )
    }

    /// What left the row (in `before`, the index as it was, and not in `keep`) goes to
    /// `removed/`, newest first: the entries first, then the audio (any WAV in the folder not on
    /// the row).
    fn move_off_the_row(
        &self,
        keep: &BTreeSet<String>,
        before: &[JsonValue],
        removed_at_ms: i64,
    ) -> io::Result<()> {
        let stamp = crate::journal::time::string_seconds(removed_at_ms);
        let mut strays: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|f| {
                        f.strip_suffix(".wav")
                            .is_some_and(|id| !keep.contains(id))
                    })
                    .collect()
            })
            .unwrap_or_default();
        strays.sort();
        let old = Self::entries(&self.removed_index_file(), "removed");
        let has_audio =
            |id: &str| self.removed_audio_file(id).exists() || self.audio_file(id).exists();
        let Some(list) =
            plan_move_off_the_row(before, keep, &old, &stamp, &has_audio, strays.len())
        else {
            return Ok(());
        };
        std::fs::create_dir_all(self.removed_dir())?;
        write_atomic(
            &self.removed_index_file(),
            removed_index_text(list).as_bytes(),
        )?;
        for f in strays {
            let to = self.removed_dir().join(&f);
            let _ = std::fs::remove_file(&to); // the same id is the same audio
            std::fs::rename(self.dir.join(&f), to)?;
        }
        Ok(())
    }

    /// The clips taken off the row, newest first (their entries only: no audio is read).
    pub fn removed(&self) -> Vec<RemovedLoop> {
        Self::entries(&self.removed_index_file(), "removed")
            .iter()
            .filter_map(RemovedLoop::from_entry)
            .collect()
    }

    /// A removed clip with its audio, decoded to `sample_rate`, to put back on the row.
    pub fn removed_clip(&self, id: &str, sample_rate: f64) -> Option<LoopClip> {
        let e = Self::entries(&self.removed_index_file(), "removed")
            .into_iter()
            .find(|e| e.get("id").and_then(JsonValue::as_str) == Some(id))?;
        clip_from_entry(&e, |id| {
            decode_audio(&self.removed_audio_file(id), sample_rate).ok()
        })
    }

    /// The launch settings one saved entry gives its clip.
    pub fn launch_of(entry: &JsonValue) -> ClipLaunch {
        launch_from_entry(entry)
    }

    /// Delete a removed clip for good: its audio and its entry.
    pub fn delete_forever(&self, id: &str) -> io::Result<()> {
        if self.removed_audio_file(id).exists() {
            std::fs::remove_file(self.removed_audio_file(id))?;
        }
        let list: Vec<JsonValue> = Self::entries(&self.removed_index_file(), "removed")
            .into_iter()
            .filter(|e| e.get("id").and_then(JsonValue::as_str) != Some(id))
            .collect();
        if self.removed_index_file().exists() {
            write_atomic(
                &self.removed_index_file(),
                removed_index_text(list).as_bytes(),
            )?;
        }
        Ok(())
    }
}
