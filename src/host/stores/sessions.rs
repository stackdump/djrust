//! `SessionStore`: sessions on disk, named by CID: `<dir>/<cid>.json` holds the canonical
//! envelope, so the file is its own checksum — `load` re-hashes and refuses anything that
//! doesn't match its name.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::session::wav::{encode_wav, RenderedAudio};
use crate::session::{Session, SessionError};

use super::{support_dir, write_atomic};
use crate::host::replay::{render_session, RenderOptions};

/// What `export_wav` can fail on: the render (a sample the set names is not in hand) or the file.
#[derive(Debug)]
pub enum ExportError {
    Session(SessionError),
    Io(io::Error),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Session(e) => e.fmt(f),
            ExportError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<SessionError> for ExportError {
    fn from(e: SessionError) -> Self {
        ExportError::Session(e)
    }
}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        ExportError::Io(e)
    }
}

pub struct SessionStore {
    pub directory: PathBuf,
}

impl SessionStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        SessionStore {
            directory: directory.into(),
        }
    }
    pub fn default_directory() -> PathBuf {
        support_dir().join("sessions")
    }

    /// Write the session; returns its CID and file.
    pub fn save(&self, session: &Session) -> io::Result<(String, PathBuf)> {
        let cid = session.cid();
        let path = self.directory.join(format!("{cid}.json"));
        write_atomic(&path, (session.canonical() + "\n").as_bytes())?;
        Ok((cid, path))
    }

    pub fn load(&self, cid: &str) -> Result<Session, SessionError> {
        let text = std::fs::read_to_string(self.directory.join(format!("{cid}.json")))
            .map_err(|_| SessionError::BadSchema)?;
        Session::decode(text.trim(), Some(cid))
    }

    /// `SessionStore.exportWAV`: render the session and write it to `path` (16-bit PCM, or
    /// `float32`), atomically. Returns what it wrote (its levels are what `djctl` reports).
    pub fn export_wav(
        session: &Session,
        path: &Path,
        float32: bool,
        opts: &RenderOptions,
    ) -> Result<RenderedAudio, ExportError> {
        let audio = render_session(session, opts)?.audio;
        write_atomic(path, &encode_wav(&audio, float32))?;
        Ok(audio)
    }

    /// CIDs stored here, newest first.
    pub fn list(&self) -> Vec<String> {
        let mut files: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(&self.directory)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
                    .map(|e| {
                        (
                            e.metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH),
                            e.path()
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        files.into_iter().map(|f| f.1).collect()
    }
}
