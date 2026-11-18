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
    /// The audio was not `beats` long at `bpm` and was padded or trimmed to be (see
    /// [`fit_to_grid`]): the file's frames before, and after.
    pub fitted: Option<(usize, usize)>,
}

/// How far a loop's length may stray from its beats at its tempo before it is fitted: a fifth of
/// a percent (the resampler's rounding is one sample).
const GRID_TOLERANCE: f64 = 0.002;
/// Past this the audio is not "that loop, a little short or long": it is something else, and its
/// tempo is read from its length as before.
const GRID_LIMIT: f64 = 0.25;

/// Make `x` (at `sample_rate`) exactly `beats` beats long at `bpm`: pad it with silence or trim
/// the tail. A loop that carries a tempo and a length in beats names its own grid, and the audio
/// that disagrees by a little (a tail cut by a tool working at another rate, a render that ran
/// short) is wrong, not the grid. Returns whether it changed; a loop already on its grid (within
/// [`GRID_TOLERANCE`]), one with no tempo or beats, or one off by more than [`GRID_LIMIT`] is
/// left alone.
pub fn fit_to_grid(x: &mut Vec<f32>, sample_rate: f64, bpm: f64, beats: i64) -> bool {
    if !(bpm > 0.0) || beats <= 0 || x.is_empty() {
        return false;
    }
    let want = (beats as f64 * 60.0 / bpm * sample_rate).round() as usize;
    let off = (x.len() as f64 - want as f64).abs() / want.max(1) as f64;
    if off <= GRID_TOLERANCE || off > GRID_LIMIT {
        return false;
    }
    x.resize(want, 0.0);
    true
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

    /// Read one loop at `sample_rate`. The WAV's own header says what rate it is (the metadata's
    /// `sampleRate` is only a hint): it is resampled from that. Its tempo and beats are kept
    /// (they are what the loop is), and audio that is a little too short or long for them is
    /// padded or trimmed to fit ([`fit_to_grid`]), so a loop made by another tool at another
    /// rate plays at the tempo it says.
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
        let mut left = conv(wav.left);
        let mut right = wav.right.map(conv);
        if left.is_empty() {
            return None;
        }
        let before = left.len();
        let fitted = fit_to_grid(&mut left, sample_rate, meta.bpm, meta.beats);
        if fitted {
            if let Some(r) = right.as_mut() {
                r.resize(left.len(), 0.0);
            }
        }
        meta.frames = left.len();
        meta.sample_rate = sample_rate;
        Some(SavedLoop {
            meta,
            fitted: fitted.then_some((before, left.len())),
            buffer: SampleBuffer::new(left, right, sample_rate),
        })
    }

    /// Add a WAV the app did not record (a render, a bounce, a loop from another tool) as a loop
    /// of `beats` beats at `bpm`. Its rate is the file's own header's, stored in the metadata, so
    /// it is read at that rate whatever the engine runs at; `bpm` and `beats` are trusted (the
    /// audio is fitted to them on load, [`fit_to_grid`]). `beats` 0 reads it from the audio's
    /// length and `bpm`. The id is a hash of the file.
    pub fn import_wav(&self, bytes: &[u8], title: &str, bpm: f64, beats: i64, source: &str) -> Result<LoopMeta, String> {
        let wav = wav_decode(bytes).ok_or("not a WAV I can read (PCM 16/24 or float 32, mono or stereo)")?;
        if !(bpm > 0.0) {
            return Err("a tempo (bpm) is needed".into());
        }
        let rate = wav.sample_rate as f64;
        let secs = wav.left.len() as f64 / rate;
        let beats = if beats > 0 { beats } else { (secs * bpm / 60.0).round() as i64 };
        if beats <= 0 {
            return Err("the audio is shorter than a beat at that tempo".into());
        }
        use sha2::{Digest, Sha256};
        let id: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        let meta = LoopMeta {
            id,
            title: title.to_string(),
            bpm,
            beats,
            sample_rate: rate,
            frames: wav.left.len(),
            channels: if wav.right.is_some() { 2 } else { 1 },
            source: source.to_string(),
            kind: "input".into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64),
            row: false,
            from: String::new(),
        };
        self.save(&meta, &wav.left, wav.right.as_deref()).map_err(|e| e.to_string())?;
        Ok(meta)
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
