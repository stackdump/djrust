//! The loop / sample browser's index (docs/BROWSER.md): audio files from the places a DJ
//! keeps them, with what their headers say (tempo, key, length, Apple Loops' category), searched
//! by name and filtered by kind. Pure std; the app shows it and the control socket queries it.
//!
//! Sources: the capture library (the app's own loops, listed by the caller), recent files (a
//! small list in the support folder), `~/Music`, and the system's Apple Loops
//! (`/Library/Audio/Apple Loops/<vendor>/<pack>`, and `~/Library/Audio/Apple Loops` if present).
//! Apple Loops are licensed for making music, not for redistribution: they are only ever read
//! where they are installed, never copied anywhere (the index cache holds names and numbers,
//! no audio).
//!
//! The file-system scan runs on a thread ([`Index::start_scan`]); a cache under the user's
//! cache folder (`~/Library/Caches/djapp/browser-index.json`) makes the next start instant,
//! and a rescan reuses a cached entry whose size and mtime have not changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::json::{JsonObject, JsonValue};

/// The extensions the browser lists (what [`crate::host::decode`] reads).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "caf", "wav", "aif", "aiff", "flac", "mp3", "m4a", "aac", "ogg", "wave",
];

/// Where an entry comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Captures,
    Recent,
    Music,
    AppleLoops,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Captures, Source::Recent, Source::Music, Source::AppleLoops];

    pub fn id(self) -> &'static str {
        match self {
            Source::Captures => "captures",
            Source::Recent => "recent",
            Source::Music => "music",
            Source::AppleLoops => "apple-loops",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Captures => "Captures",
            Source::Recent => "Recent",
            Source::Music => "~/Music",
            Source::AppleLoops => "Apple Loops",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        let t = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Some(match t.as_str() {
            "captures" | "capture" | "library" | "capture-library" => Source::Captures,
            "recent" | "recents" | "recent-files" => Source::Recent,
            "music" | "~/music" => Source::Music,
            "apple-loops" | "apple" | "loops" | "applel" | "garageband" => Source::AppleLoops,
            _ => return None,
        })
    }
}

/// The kind filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Vocals,
    Drums,
    Bass,
    Synth,
    Fx,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Vocals, Kind::Drums, Kind::Bass, Kind::Synth, Kind::Fx];

    pub fn id(self) -> &'static str {
        match self {
            Kind::Vocals => "vocals",
            Kind::Drums => "drums",
            Kind::Bass => "bass",
            Kind::Synth => "synth",
            Kind::Fx => "fx",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "vocals" | "vocal" | "vox" | "voice" | "voices" => Kind::Vocals,
            "drums" | "drum" | "beats" | "beat" | "percussion" | "perc" => Kind::Drums,
            "bass" => Kind::Bass,
            "synth" | "synths" | "synthesizer" | "keys" => Kind::Synth,
            "fx" | "sfx" | "effects" | "sound-effect" => Kind::Fx,
            _ => return None,
        })
    }
}

/// One file in the browser.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// The title (the file's name without its extension, or a capture's title).
    pub name: String,
    pub source: Option<Source>,
    /// Its folder: an Apple Loops pack, a folder under `~/Music`, "captures".
    pub pack: String,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub genre: Option<String>,
    pub descriptors: Option<String>,
    pub key: Option<String>,
    pub tempo: Option<f64>,
    pub beats: Option<f64>,
    pub seconds: Option<f64>,
    /// A capture library loop's id (its audio is `path`).
    pub library_id: Option<String>,
    pub size: u64,
    pub mtime: u64,
}

fn has_word(hay: &str, words: &[&str]) -> bool {
    hay.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| words.contains(&w))
}

impl Entry {
    /// Is it of this kind? Apple Loops say (category, subcategory); anything else goes by its
    /// name.
    pub fn is(&self, k: Kind) -> bool {
        let cat = self.category.as_deref().unwrap_or("").to_ascii_lowercase();
        let sub = self.subcategory.as_deref().unwrap_or("").to_ascii_lowercase();
        let name = self.name.to_ascii_lowercase();
        match k {
            Kind::Vocals => {
                cat.contains("vocal")
                    || ["vox", "vocal", "voice", "choir", "acapella", "a cappella"]
                        .iter()
                        .any(|w| name.contains(w))
            }
            Kind::Drums => {
                cat == "drums"
                    || cat == "percussion"
                    || (cat.is_empty()
                        && has_word(
                            &name,
                            &[
                                "beat", "beats", "drum", "drums", "kick", "snare", "hat", "hats",
                                "hihat", "perc", "percussion", "break", "breakbeat", "clap",
                                "tom", "toms", "cymbal", "shaker", "groove",
                            ],
                        ))
            }
            Kind::Bass => {
                cat == "bass"
                    || (cat.is_empty() && (has_word(&name, &["bass", "sub", "808"]) || name.contains("bassline")))
            }
            Kind::Synth => {
                sub == "synthesizer"
                    || (cat != "bass"
                        && (name.contains("synth")
                            || has_word(&name, &["pad", "pads", "lead", "arp", "pluck", "stab", "stabs", "chord", "chords"])))
            }
            Kind::Fx => {
                cat == "sound effect"
                    || cat.starts_with("texture")
                    || has_word(&name, &["fx", "sfx", "riser", "impact", "sweep", "whoosh", "noise", "transition"])
            }
        }
    }

    /// The kinds it is.
    pub fn kinds(&self) -> Vec<Kind> {
        Kind::ALL.into_iter().filter(|k| self.is(*k)).collect()
    }

    fn haystack(&self) -> String {
        let mut h = format!("{} {}", self.name, self.pack);
        for x in [&self.category, &self.subcategory, &self.genre, &self.descriptors, &self.key]
            .into_iter()
            .flatten()
        {
            h.push(' ');
            h.push_str(x);
        }
        h.to_lowercase()
    }

    /// Every word of `text` is in its name, pack, category, genre, descriptors or key.
    pub fn matches(&self, text: &str) -> bool {
        let t = text.trim().to_lowercase();
        if t.is_empty() {
            return true;
        }
        let h = self.haystack();
        t.split_whitespace().all(|w| h.contains(w))
    }

    /// "120 bpm · Ab minor · 8 beats · 0:04"
    pub fn detail(&self) -> String {
        let mut v = Vec::new();
        if let Some(t) = self.tempo {
            v.push(format!("{} bpm", js_num(t)));
        }
        if let Some(k) = &self.key {
            v.push(k.clone());
        }
        if let Some(b) = self.beats {
            v.push(format!("{} beats", js_num(b)));
        }
        if let Some(s) = self.seconds {
            v.push(clock(s));
        }
        v.join(" · ")
    }

    pub fn to_json(&self) -> JsonValue {
        let o = |v: &Option<String>| v.as_ref().map_or(JsonValue::Null, |s| JsonValue::str(s.clone()));
        let f = |v: Option<f64>| v.map_or(JsonValue::Null, |x| JsonValue::from((x * 1000.0).round() / 1000.0));
        let mut m = JsonObject::from_pairs([
            ("name", JsonValue::str(self.name.clone())),
            ("path", JsonValue::str(self.path.to_string_lossy().into_owned())),
            ("source", self.source.map_or(JsonValue::Null, |s| s.id().into())),
            ("pack", JsonValue::str(self.pack.clone())),
            ("category", o(&self.category)),
            ("subcategory", o(&self.subcategory)),
            ("genre", o(&self.genre)),
            ("key", o(&self.key)),
            ("tempo", f(self.tempo)),
            ("beats", f(self.beats)),
            ("seconds", f(self.seconds)),
            (
                "kinds",
                JsonValue::Array(self.kinds().into_iter().map(|k| k.id().into()).collect()),
            ),
        ]);
        if let Some(d) = &self.descriptors {
            m.set("descriptors", JsonValue::str(d.clone()));
        }
        if let Some(id) = &self.library_id {
            m.set("libraryId", JsonValue::str(id.clone()));
        }
        JsonValue::Object(m)
    }

    /// The cache's form: [`to_json`](Entry::to_json) plus size and mtime.
    fn to_cache(&self) -> JsonValue {
        let mut v = self.to_json();
        if let JsonValue::Object(m) = &mut v {
            m.remove("kinds");
            m.set("size", JsonValue::from(self.size as f64));
            m.set("mtime", JsonValue::from(self.mtime as f64));
        }
        v
    }

    fn from_cache(v: &JsonValue) -> Option<Entry> {
        let s = |k: &str| v.get(k).and_then(JsonValue::as_str).map(str::to_string);
        let f = |k: &str| v.get(k).and_then(JsonValue::as_f64);
        Some(Entry {
            path: PathBuf::from(s("path")?),
            name: s("name")?,
            source: s("source").as_deref().and_then(Source::parse),
            pack: s("pack").unwrap_or_default(),
            category: s("category"),
            subcategory: s("subcategory"),
            genre: s("genre"),
            descriptors: s("descriptors"),
            key: s("key"),
            tempo: f("tempo"),
            beats: f("beats"),
            seconds: f("seconds"),
            library_id: s("libraryId"),
            size: f("size").unwrap_or(0.0) as u64,
            mtime: f("mtime").unwrap_or(0.0) as u64,
        })
    }
}

fn js_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// m:ss
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

// ---- reading one file ---------------------------------------------------------------------

pub(crate) fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A WAV's or AIFF's length in seconds, from its header.
fn pcm_seconds(path: &Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    let riff = &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE";
    let aiff = &head[0..4] == b"FORM" && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC");
    if !riff && !aiff {
        return None;
    }
    let (mut rate, mut frame_bytes, mut frames) = (0.0f64, 0u64, None::<u64>);
    for _ in 0..64 {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            break;
        }
        let size = if riff {
            u32::from_le_bytes(ch[4..8].try_into().ok()?)
        } else {
            u32::from_be_bytes(ch[4..8].try_into().ok()?)
        } as u64;
        let id = &ch[0..4];
        if riff && id == b"fmt " {
            let mut b = vec![0u8; size.min(64) as usize];
            f.read_exact(&mut b).ok()?;
            rate = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?) as f64;
            frame_bytes = u16::from_le_bytes(b.get(12..14)?.try_into().ok()?) as u64;
            f.seek(SeekFrom::Current(size as i64 - b.len() as i64 + (size & 1) as i64)).ok()?;
        } else if riff && id == b"data" {
            frames = size.checked_div(frame_bytes);
            break;
        } else if aiff && id == b"COMM" {
            let mut b = vec![0u8; size.min(64) as usize];
            f.read_exact(&mut b).ok()?;
            frames = Some(u32::from_be_bytes(b.get(2..6)?.try_into().ok()?) as u64);
            rate = extended80(b.get(8..18)?);
            break;
        } else {
            f.seek(SeekFrom::Current((size + (size & 1)) as i64)).ok()?;
        }
    }
    let n = frames?;
    (rate > 0.0 && n > 0).then(|| n as f64 / rate)
}

/// An AIFF's 80-bit extended float.
fn extended80(b: &[u8]) -> f64 {
    let exp = (((b[0] as i32) & 0x7f) << 8 | b[1] as i32) - 16383;
    let mant = u64::from_be_bytes(b[2..10].try_into().unwrap_or([0; 8]));
    let v = mant as f64 / (1u64 << 63) as f64 * 2f64.powi(exp);
    if b[0] & 0x80 != 0 {
        -v
    } else {
        v
    }
}

/// A tempo in a file name: "120bpm", "120 BPM", "_128_".
pub fn tempo_in_name(name: &str) -> Option<f64> {
    let l = name.to_ascii_lowercase();
    if let Some(i) = l.find("bpm") {
        let digits: String = l[..i]
            .trim_end_matches([' ', '_', '-'])
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(v) = digits.parse::<f64>() {
            if (40.0..=300.0).contains(&v) {
                return Some(v);
            }
        }
    }
    None
}

/// What one file's header says, as an entry of `source` in folder `pack`.
pub fn read_entry(path: &Path, source: Source, pack: &str) -> Entry {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let md = std::fs::metadata(path).ok();
    let mut e = Entry {
        path: path.to_path_buf(),
        name,
        source: Some(source),
        pack: pack.to_string(),
        size: md.as_ref().map_or(0, |m| m.len()),
        mtime: md.as_ref().map_or(0, mtime_of),
        ..Entry::default()
    };
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "caf" {
        if let Some(c) = crate::host::caf::read_info_file(path) {
            e.seconds = c.seconds();
            e.beats = c.beats();
            e.tempo = c.tempo();
            e.key = c.key();
            let s = |k: &str| c.get(k).map(str::to_string);
            e.category = s("category");
            e.subcategory = s("subcategory");
            e.genre = s("genre");
            e.descriptors = s("descriptors");
        }
    } else if matches!(ext.as_str(), "wav" | "wave" | "aif" | "aiff") {
        e.seconds = pcm_seconds(path);
    }
    if e.tempo.is_none() {
        e.tempo = tempo_in_name(&e.name);
    }
    e
}

fn mtime_of(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

// ---- the places -------------------------------------------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The folders the scan walks, by source: `$DJ_BROWSE_APPLE_LOOPS` / `$DJ_BROWSE_MUSIC`
/// (colon-separated) replace the defaults.
pub fn roots() -> Vec<(Source, PathBuf)> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| v.split(':').map(PathBuf::from).collect::<Vec<_>>())
    };
    let mut v = Vec::new();
    let apple = env("DJ_BROWSE_APPLE_LOOPS").unwrap_or_else(|| {
        vec![
            PathBuf::from("/Library/Audio/Apple Loops"),
            home().join("Library/Audio/Apple Loops"),
        ]
    });
    for p in apple {
        v.push((Source::AppleLoops, p));
    }
    for p in env("DJ_BROWSE_MUSIC").unwrap_or_else(|| vec![home().join("Music")]) {
        v.push((Source::Music, p));
    }
    v
}

/// Packages and app libraries a walk does not go into (they hold no loose loops, or a lot of
/// files that are not audio).
fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || [".band", ".logicx", ".photoslibrary", ".musiclibrary", ".tvlibrary", ".app"]
            .iter()
            .any(|x| name.ends_with(x))
}

/// The pack an Apple Loops file is in: its folders under the root, without the vendor folder
/// ("Apple") when there is one below it.
fn pack_of(source: Source, root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let mut parts: Vec<String> = rel
        .parent()
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    if source == Source::AppleLoops && parts.len() > 1 {
        parts.remove(0);
    }
    if parts.is_empty() {
        return match source {
            Source::Music => "~/Music".into(),
            _ => root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
    }
    parts.join(" / ")
}

/// Most files one scan lists, and how deep it goes.
const MAX_FILES: usize = 60_000;
const MAX_DEPTH: usize = 10;

/// Walk `roots` for audio files, reusing `cached` entries whose size and mtime match.
/// Sorted by source, pack, name.
pub fn scan(roots: &[(Source, PathBuf)], cached: &HashMap<PathBuf, Entry>) -> Vec<Entry> {
    let mut out = Vec::new();
    for (source, root) in roots {
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for de in rd.flatten() {
                if out.len() >= MAX_FILES {
                    break;
                }
                let p = de.path();
                let name = de.file_name().to_string_lossy().into_owned();
                let Ok(ft) = de.file_type() else { continue };
                if ft.is_dir() {
                    if depth < MAX_DEPTH && !skip_dir(&name) {
                        stack.push((p, depth + 1));
                    }
                    continue;
                }
                if name.starts_with('.') || !is_audio(&p) {
                    continue;
                }
                let md = de.metadata().ok();
                let (size, mtime) = md.as_ref().map_or((0, 0), |m| (m.len(), mtime_of(m)));
                if let Some(c) = cached.get(&p) {
                    if c.size == size && c.mtime == mtime && c.source == Some(*source) {
                        out.push(c.clone());
                        continue;
                    }
                }
                out.push(read_entry(&p, *source, &pack_of(*source, root, &p)));
            }
        }
    }
    sort(&mut out);
    out
}

fn sort(v: &mut [Entry]) {
    v.sort_by(|a, b| {
        (a.source, a.pack.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.source, b.pack.to_lowercase(), b.name.to_lowercase()))
    });
}

/// The user's cache folder for djapp (`~/Library/Caches/djapp`, `$XDG_CACHE_HOME/djapp`,
/// `~/.cache/djapp`).
pub fn cache_dir() -> PathBuf {
    crate::host::platform::cache_dir_in(&home()).join("djapp")
}

// ---- queries ------------------------------------------------------------------------------

/// What to list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub kind: Option<Kind>,
    pub source: Option<Source>,
    /// A pack (folder) name, exactly.
    pub pack: Option<String>,
    pub offset: usize,
    /// 0: no limit.
    pub limit: usize,
}

impl Query {
    pub fn admits(&self, e: &Entry) -> bool {
        self.source.is_none_or(|s| e.source == Some(s))
            && self.pack.as_deref().is_none_or(|p| e.pack == p)
            && self.kind.is_none_or(|k| e.is(k))
            && e.matches(&self.text)
    }
}

/// The packs of `entries` from `source`, with how many files each holds, in order.
pub fn packs<'a>(entries: impl IntoIterator<Item = &'a Entry>, source: Option<Source>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for e in entries {
        if source.is_some_and(|s| e.source != Some(s)) {
            continue;
        }
        match at.get(&e.pack) {
            Some(&i) => v[i].1 += 1,
            None => {
                at.insert(e.pack.clone(), v.len());
                v.push((e.pack.clone(), 1));
            }
        }
    }
    v
}

// ---- the index ----------------------------------------------------------------------------

/// The scanned sources (Music, Apple Loops), with a scan in the background.
pub struct Index {
    pub entries: Vec<Entry>,
    pub roots: Vec<(Source, PathBuf)>,
    /// None: not written (tests).
    pub cache_file: Option<PathBuf>,
    scanning: Option<Receiver<Vec<Entry>>>,
    pub scans: u64,
    pub scanned_at: Option<std::time::SystemTime>,
}

impl Index {
    pub fn new(roots: Vec<(Source, PathBuf)>, cache_file: Option<PathBuf>) -> Index {
        let mut ix = Index {
            entries: Vec::new(),
            roots,
            cache_file,
            scanning: None,
            scans: 0,
            scanned_at: None,
        };
        ix.load_cache();
        ix
    }

    /// The default roots and cache file.
    pub fn open_default() -> Index {
        Index::new(roots(), Some(cache_dir().join("browser-index.json")))
    }

    fn load_cache(&mut self) {
        let Some(p) = &self.cache_file else { return };
        let Ok(text) = std::fs::read_to_string(p) else { return };
        let Ok(v) = JsonValue::parse(&text) else { return };
        if let Some(a) = v.get("entries").and_then(JsonValue::as_array) {
            self.entries = a.iter().filter_map(Entry::from_cache).collect();
        }
    }

    fn save_cache(&self) {
        let Some(p) = &self.cache_file else { return };
        let v = JsonValue::Object(JsonObject::from_pairs([
            ("version", JsonValue::from(1.0)),
            (
                "entries",
                JsonValue::Array(self.entries.iter().map(Entry::to_cache).collect()),
            ),
        ]));
        let _ = crate::host::stores::write_atomic(p, v.js_stringified().as_bytes());
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.is_some()
    }

    /// Walk the roots again on a thread (a no-op while one is running).
    pub fn start_scan(&mut self) {
        if self.scanning.is_some() {
            return;
        }
        let roots = self.roots.clone();
        let cached: HashMap<PathBuf, Entry> =
            self.entries.iter().map(|e| (e.path.clone(), e.clone())).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("browse-scan".into())
            .spawn(move || {
                let _ = tx.send(scan(&roots, &cached));
            })
            .ok();
        self.scanning = Some(rx);
    }

    /// Take a finished scan. True when the entries changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.scanning else { return false };
        match rx.try_recv() {
            Ok(v) => {
                self.scanning = None;
                self.scans += 1;
                self.scanned_at = Some(std::time::SystemTime::now());
                let changed = v != self.entries;
                self.entries = v;
                if changed {
                    self.save_cache();
                }
                changed
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.scanning = None;
                false
            }
        }
    }

    /// Block until the scan running now ends (tests, the CLI).
    pub fn wait(&mut self) {
        if let Some(rx) = self.scanning.take() {
            if let Ok(v) = rx.recv() {
                self.scans += 1;
                self.scanned_at = Some(std::time::SystemTime::now());
                self.entries = v;
                self.save_cache();
            }
        }
    }
}

// ---- recent files -------------------------------------------------------------------------

/// The files loaded lately, newest first (`support_dir()/browser-recent.json`).
pub struct Recent {
    pub file: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
}

/// How many recent files are kept.
pub const RECENT_MAX: usize = 50;

impl Recent {
    pub fn open(file: Option<PathBuf>) -> Recent {
        let paths = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| JsonValue::parse(&t).ok())
            .and_then(|v| v.strings())
            .map(|v| v.into_iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        Recent { file, paths }
    }

    pub fn open_default() -> Recent {
        Recent::open(Some(crate::host::stores::support_dir().join("browser-recent.json")))
    }

    /// `path` to the top.
    pub fn push(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(RECENT_MAX);
        if let Some(f) = &self.file {
            let v = JsonValue::Array(
                self.paths
                    .iter()
                    .map(|p| JsonValue::str(p.to_string_lossy().into_owned()))
                    .collect(),
            );
            let _ = crate::host::stores::write_atomic(f, v.js_stringified().as_bytes());
        }
    }

    /// Its files as entries (an indexed one's metadata is reused), those still on disk.
    pub fn entries(&self, index: &[Entry]) -> Vec<Entry> {
        let by: HashMap<&Path, &Entry> = index.iter().map(|e| (e.path.as_path(), e)).collect();
        self.paths
            .iter()
            .filter(|p| p.exists())
            .map(|p| {
                let mut e = match by.get(p.as_path()) {
                    Some(e) => (*e).clone(),
                    None => read_entry(p, Source::Recent, ""),
                };
                e.source = Some(Source::Recent);
                if e.pack.is_empty() {
                    e.pack = p
                        .parent()
                        .and_then(|d| d.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                }
                e
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("djhost-browse-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(name: &str, cat: Option<&str>, sub: Option<&str>) -> Entry {
        Entry {
            name: name.into(),
            category: cat.map(Into::into),
            subcategory: sub.map(Into::into),
            ..Entry::default()
        }
    }

    #[test]
    fn kinds_follow_apple_categories_then_names() {
        assert!(entry("Deep Down Vocal FX 02", Some("Vocals"), Some("Male")).is(Kind::Vocals));
        assert!(entry("Afloat Beat", Some("Drums"), Some("Electronic Beats")).is(Kind::Drums));
        assert!(!entry("Afloat Beat", Some("Drums"), None).is(Kind::Bass));
        assert!(entry("Afloat Sub Bass", Some("Bass"), Some("Synthetic Bass")).is(Kind::Bass));
        assert!(!entry("Afloat Sub Bass", Some("Bass"), Some("Synthetic Bass")).is(Kind::Synth));
        assert!(entry("Afloat Pad", Some("Keyboards"), Some("Synthesizer")).is(Kind::Synth));
        assert!(entry("my vox chop 120bpm", None, None).is(Kind::Vocals));
        assert!(entry("kick_loop", None, None).is(Kind::Drums));
        assert!(entry("808 slide", None, None).is(Kind::Bass));
        assert!(entry("acid lead", None, None).is(Kind::Synth));
        assert_eq!(Kind::parse("vox"), Some(Kind::Vocals));
    }

    #[test]
    fn search_needs_every_word() {
        let mut e = entry("Afloat Beat", Some("Drums"), None);
        e.pack = "01 Hip Hop".into();
        assert!(e.matches("afloat"));
        assert!(e.matches("hip beat"));
        assert!(e.matches("drums"));
        assert!(!e.matches("afloat bass"));
        assert!(e.matches("  "));
    }

    #[test]
    fn a_tempo_in_a_file_name() {
        assert_eq!(tempo_in_name("loop_124bpm"), Some(124.0));
        assert_eq!(tempo_in_name("Loop 90 BPM"), Some(90.0));
        assert_eq!(tempo_in_name("loop"), None);
    }

    #[test]
    fn scans_packs_reads_caf_metadata_and_reuses_the_cache() {
        let root = tmp("scan");
        let pack = root.join("Apple/01 Test Pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::create_dir_all(root.join("Apple/x.band")).unwrap();
        let caf = crate::host::caf::test_caf(
            88200,
            44100.0,
            &[("category", "Vocals"), ("beat count", "4"), ("key signature", "C"), ("key type", "major")],
        );
        std::fs::write(pack.join("Choir.caf"), &caf).unwrap();
        std::fs::write(root.join("Apple/x.band/hidden.wav"), b"RIFF").unwrap();
        std::fs::write(pack.join("notes.txt"), b"not audio").unwrap();
        let roots = vec![(Source::AppleLoops, root.clone())];
        let cache = root.join("cache.json");
        let mut ix = Index::new(roots.clone(), Some(cache.clone()));
        ix.start_scan();
        ix.wait();
        assert_eq!(ix.entries.len(), 1, "{:?}", ix.entries);
        let e = &ix.entries[0];
        assert_eq!(e.name, "Choir");
        assert_eq!(e.pack, "01 Test Pack");
        assert_eq!(e.source, Some(Source::AppleLoops));
        assert_eq!(e.tempo, Some(120.0));
        assert_eq!(e.key.as_deref(), Some("C major"));
        assert_eq!(e.seconds, Some(2.0));
        assert!(e.is(Kind::Vocals));
        // the cache comes back as it was written
        let again = Index::new(roots, Some(cache));
        assert_eq!(again.entries, ix.entries);
        let q = Query { kind: Some(Kind::Drums), ..Query::default() };
        assert!(!q.admits(e));
        let q = Query { text: "choir".into(), source: Some(Source::AppleLoops), ..Query::default() };
        assert!(q.admits(e));
        assert_eq!(packs(&ix.entries, None), vec![("01 Test Pack".to_string(), 1)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recent_keeps_newest_first_without_duplicates() {
        let d = tmp("recent");
        let a = d.join("a.wav");
        let b = d.join("b.wav");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let f = d.join("recent.json");
        let mut r = Recent::open(Some(f.clone()));
        r.push(&a);
        r.push(&b);
        r.push(&a);
        assert_eq!(r.paths, vec![a.clone(), b.clone()]);
        let r2 = Recent::open(Some(f));
        assert_eq!(r2.paths, vec![a.clone(), b]);
        let es = r2.entries(&[]);
        assert_eq!(es[0].source, Some(Source::Recent));
        assert_eq!(es[0].name, "a");
        let _ = std::fs::remove_dir_all(&d);
    }
}
