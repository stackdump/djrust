//! Vocal pads (docs/MPK.md, "Vocals"): pads that play the files of a folder, one-shot, on the
//! beat, cutting the one before, stretched to the master tempo.
//!
//! A pad is an ordinary `oneShot` binding whose `sample` is `vox:N`: the Nth file of the page the
//! pads are on (0-based, 16 to a page: bank A is files 0-7 of it, bank B 8-15). That keeps every
//! option a one-shot has (`quant`, `choke`, `gain`, `bpm`) and adds the folder:
//!
//! * **the folder** is `vox.dir`, else `vox/` in the loop library. It is read recursively (3 deep),
//!   files sorted by their path without case, so the order is the order you see in a file
//!   manager and does not change from run to run;
//! * **pages**: the `voxPage` target steps through the files 16 at a time, wrapping, so a folder
//!   of hundreds is reachable with two buttons;
//! * **tempo**: a vocal phrase has no grid of its own, so it is stretched to the master tempo
//!   only when its tempo is known: the file name says it (`..._128bpm_...`), the pad's `bpm` arg,
//!   or the folder's `vox.bpm` for a pack that is all at one tempo. Otherwise it plays at its own
//!   speed. A stretch is made when the file is first played; after the master tempo moves more
//!   than 2 % the idle ones are dropped and made again at the next press;
//! * **choke** defaults to one group, `vox`: a new phrase cuts the one still sounding;
//! * **quantise** defaults to the next beat.
//!
//! Only the files you put in the folder are played: djrust ships none.

use std::path::{Path, PathBuf};

use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::loop_pads::Quant;
use crate::host::browse::{is_audio, tempo_in_name};

/// Files to a page: 8 pads on bank A, 8 on bank B.
pub const PAGE: usize = 16;
/// How deep the folder is read, and the most files kept.
const MAX_DEPTH: usize = 3;
const MAX_FILES: usize = 5000;
/// The master tempo may move this much before an idle vocal is stretched again.
const RESTRETCH: f64 = 0.02;

/// One file of the folder.
#[derive(Clone, Debug, PartialEq)]
pub struct VoxFile {
    pub path: PathBuf,
    /// Its path under the folder, without the extension: what a pad is labelled with.
    pub name: String,
    /// The tempo its name says.
    pub bpm: Option<f64>,
    /// The key its name says: (pitch class, minor).
    pub key: Option<(u8, bool)>,
}

/// The vocal pads' settings and the folder's files.
#[derive(Clone, Debug)]
pub struct VoxState {
    /// `vox.dir`; None: `vox/` in the loop library.
    pub dir: Option<PathBuf>,
    pub files: Vec<VoxFile>,
    /// Which 16 the pads play.
    pub page: usize,
    /// The choke group every vocal pad is in unless its binding says. Empty: none.
    pub choke: String,
    /// Stretch to the master tempo when the file's tempo is known.
    pub stretch: bool,
    /// How a press lands unless the binding says.
    pub quant: Quant,
    /// A tempo for files whose name has none (a pack at one tempo); None: leave them.
    pub bpm: Option<f64>,
    /// The master tempo the loaded vocals were stretched to.
    stretched_at: Option<f64>,
    /// The folder read, and whether it could be.
    scanned: Option<PathBuf>,
}

impl Default for VoxState {
    fn default() -> Self {
        VoxState {
            dir: None,
            files: Vec::new(),
            page: 0,
            choke: "vox".into(),
            stretch: true,
            quant: Quant::Beat,
            bpm: None,
            stretched_at: None,
            scanned: None,
        }
    }
}

/// The audio files under `dir`, sorted, with what their names say.
pub fn scan_dir(dir: &Path) -> Vec<VoxFile> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for de in rd.flatten() {
            if found.len() >= MAX_FILES {
                break;
            }
            let p = de.path();
            let name = de.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((p, depth + 1));
                }
            } else if is_audio(&p) {
                found.push(p);
            }
        }
    }
    let mut out: Vec<VoxFile> = found
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(dir).unwrap_or(&p).with_extension("");
            let name = rel.to_string_lossy().into_owned();
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            VoxFile {
                bpm: tempo_in_name(&stem),
                key: crate::musicality::loop_key::key_in_name(&stem),
                path: p,
                name,
            }
        })
        .collect();
    out.sort_by_key(|f| (f.name.to_lowercase(), f.name.clone()));
    out
}

impl Engine {
    /// The folder the vocal pads read: `vox.dir`, else `vox/` in the loop library, else none.
    pub fn vox_dir(&self) -> Option<PathBuf> {
        self.vox.dir.clone().or_else(|| {
            self.capture_library
                .as_ref()
                .map(|l| l.directory.join("vox"))
        })
    }

    /// Point the pads at a folder (None: the loop library's `vox/`) and read it.
    pub fn set_vox_dir(&mut self, dir: Option<PathBuf>) -> usize {
        self.vox.dir = dir;
        self.vox.page = 0;
        self.vox_scan()
    }

    /// Read the folder again (files added or taken away). The page stays if it still exists.
    pub fn vox_scan(&mut self) -> usize {
        let dir = self.vox_dir();
        self.vox.files = dir.as_deref().map(scan_dir).unwrap_or_default();
        self.vox.scanned = dir;
        if self.vox.page * PAGE >= self.vox.files.len() {
            self.vox.page = 0;
        }
        self.vox.files.len()
    }

    /// The folder is read the first time a pad needs it (and again if the folder it names moved).
    fn vox_ready(&mut self) {
        if self.vox.scanned != self.vox_dir() || self.vox.scanned.is_none() {
            self.vox_scan();
        }
    }

    /// Read the folder if it has not been (a status before any pad was pressed).
    pub fn vox_ready_for_status(&mut self) {
        self.vox_ready();
    }

    /// Pages in the folder (at least one).
    pub fn vox_pages(&self) -> usize {
        self.vox.files.len().div_ceil(PAGE).max(1)
    }

    /// Step the page: "next", "prev" or "first", wrapping. The idle vocals of the page left are
    /// taken off the row, so paging through a folder does not fill it.
    pub fn vox_page_step(&mut self, op: &str) -> usize {
        self.vox_ready();
        let n = self.vox_pages();
        self.vox.page = match op {
            "prev" | "back" => (self.vox.page + n - 1) % n,
            "first" | "reset" => 0,
            _ => (self.vox.page + 1) % n,
        };
        self.vox_evict();
        self.vox.page
    }

    /// Pad `pad` (0-15) of the current page.
    pub fn vox_file(&self, pad: usize) -> Option<&VoxFile> {
        if pad >= PAGE {
            return None;
        }
        self.vox.files.get(self.vox.page * PAGE + pad)
    }

    /// Take the vocals that are not sounding off the row, so they are made again when next
    /// pressed (another page, a new tempo).
    fn vox_evict(&mut self) {
        let Some(dir) = self.vox_dir() else { return };
        let ids: Vec<String> = self
            .loops
            .row
            .clips
            .iter()
            .filter(|c| {
                self.loops.pads.extra(&c.id).once
                    && Path::new(&self.loops.pads.extra(&c.id).from).starts_with(&dir)
            })
            .map(|c| c.id.clone())
            .filter(|id| self.voice_of(id).is_none())
            .collect();
        for id in ids {
            self.remove_clip(&id);
            self.loops.row.forget_removed_clip(&id);
        }
    }

    /// A vocal pad: the file `vox:N` names, with the folder's defaults. Returns what to launch,
    /// or why not.
    pub fn vox_launch(
        &mut self,
        pad: usize,
        quant: Option<&str>,
        choke: Option<&str>,
        bpm: Option<f64>,
        gain: f32,
    ) -> Result<super::loop_pads::FileLaunch, String> {
        self.vox_ready();
        let master = self.hub_clock().bpm;
        // the vocals stretched to a tempo that has moved on are made again
        if let (Some(at), Some(now)) = (self.vox.stretched_at, master) {
            if at > 0.0 && ((now - at) / at).abs() > RESTRETCH {
                self.vox_evict();
                self.vox.stretched_at = None;
            }
        }
        if self.vox.stretched_at.is_none() {
            self.vox.stretched_at = master;
        }
        let f = self.vox_file(pad).cloned().ok_or_else(|| {
            format!(
                "vox pad {} has no file (page {} of {}, {} file{} in {})",
                pad.saturating_add(1),
                self.vox.page + 1,
                self.vox_pages(),
                self.vox.files.len(),
                if self.vox.files.len() == 1 { "" } else { "s" },
                self.vox_dir()
                    .map_or("no folder: set vox.dir".to_string(), |d| d.display().to_string())
            )
        })?;
        let own = if self.vox.stretch {
            bpm.or(f.bpm).or(self.vox.bpm)
        } else {
            None
        };
        let choke = choke
            .map(str::to_string)
            .unwrap_or_else(|| self.vox.choke.clone());
        Ok(super::loop_pads::FileLaunch {
            path: f.path,
            bars: None,
            bpm: own,
            once: true,
            quant: Quant::parse(quant, self.vox.quant),
            gain,
            choke: (!choke.is_empty()).then_some(choke),
            launch: true,
        })
    }

    /// The pads, the page and the settings, for the socket's `vox status` and the window.
    pub fn vox_json(&self) -> JsonValue {
        let pads: Vec<JsonValue> = (0..PAGE)
            .map(|i| {
                let f = self.vox_file(i);
                JsonValue::Object(JsonObject::from_pairs([
                    ("pad", JsonValue::int(i as i64 + 1)),
                    ("bank", JsonValue::str(if i < 8 { "A" } else { "B" })),
                    ("file", f.map_or(JsonValue::Null, |f| JsonValue::str(f.name.clone()))),
                    ("bpm", f.and_then(|f| f.bpm).map_or(JsonValue::Null, JsonValue::Number)),
                    (
                        "key",
                        f.and_then(|f| f.key).map_or(JsonValue::Null, |(pc, minor)| {
                            JsonValue::str(format!(
                                "{}{}",
                                crate::musicality::loop_key::NAMES[pc as usize % 12],
                                if minor { "m" } else { "" }
                            ))
                        }),
                    ),
                ]))
            })
            .collect();
        JsonValue::Object(JsonObject::from_pairs([
            (
                "dir",
                self.vox_dir()
                    .map_or(JsonValue::Null, |d| JsonValue::str(d.display().to_string())),
            ),
            ("explicit", JsonValue::Bool(self.vox.dir.is_some())),
            ("files", JsonValue::int(self.vox.files.len() as i64)),
            ("page", JsonValue::int(self.vox.page as i64 + 1)),
            ("pages", JsonValue::int(self.vox_pages() as i64)),
            ("choke", JsonValue::str(self.vox.choke.clone())),
            ("stretch", JsonValue::Bool(self.vox.stretch)),
            ("quant", JsonValue::str(self.vox.quant.name())),
            ("bpm", self.vox.bpm.map_or(JsonValue::Null, JsonValue::Number)),
            ("pads", JsonValue::Array(pads)),
        ]))
    }
}
