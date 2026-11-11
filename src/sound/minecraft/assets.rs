//! `Audio/MinecraftAssets.swift`: the Minecraft sounds installed by a launcher (PrismLauncher,
//! or the vanilla launcher), read where they lie: the asset index (`indexes/<id>.json`:
//! "minecraft/sounds/note/harp2.ogg" to SHA-1), the content-addressed objects
//! (`objects/<xx>/<sha1>`) and `minecraft/sounds.json` (sound event to files). Nothing is
//! copied: the importer builds a local SoundFont from them.
//!
//! Licensing: the user's own game assets, for personal local use only. Neither the .ogg files
//! nor anything built from them (`minecraft-local.sf2`) may enter the repo, a release or the
//! CDN; `music/*` and `records/*` (the soundtrack and discs) are never read.
//!
//! The engine does no I/O, so every read goes through an [`AssetFs`] the host provides
//! (`crate::host::minecraft::StdFs`; the tests use an in-memory one).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::json::JsonValue;

/// The file system `MinecraftAssets` reads through (Foundation's `FileManager` and
/// `String(contentsOf:)` in Swift). Paths are the strings [`MinecraftAssets`] builds.
pub trait AssetFs {
    /// `fileExists(atPath:)`: a file or a folder.
    fn exists(&self, path: &str) -> bool;
    /// `contentsOfDirectory(atPath:)`: the names in a folder, or none when it can't be read.
    fn list(&self, dir: &str) -> Option<Vec<String>>;
    /// `contents(atPath:)`: a file's bytes, or none.
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// `MinecraftAssets.Unavailable`, with the same descriptions (they are shown to the user).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    NoAssets(String),
    NoIndex(String),
    NoSoundsJson(String),
    MissingObject(String),
    BadHash(String),
    /// Stands in for the Foundation errors Swift lets through when a file it found can't be
    /// read as UTF-8 text or parsed as JSON: the reason, as the error would say it.
    Unreadable(String),
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unavailable::NoAssets(d) => write!(
                f,
                "no Minecraft assets at {d} (install the game with PrismLauncher, or set DJ_MINECRAFT_ASSETS)"
            ),
            Unavailable::NoIndex(d) => write!(f, "no asset index in {d}/indexes"),
            Unavailable::NoSoundsJson(i) => {
                write!(f, "asset index {i} has no minecraft/sounds.json")
            }
            Unavailable::MissingObject(p) => write!(f, "{p} is in the index but not downloaded"),
            Unavailable::BadHash(p) => write!(f, "{p} does not match its SHA-1"),
            Unavailable::Unreadable(why) => write!(f, "{why}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Unavailable {}

/// `MinecraftAssets`: one installed asset index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinecraftAssets {
    /// The assets folder (`.../PrismLauncher/assets`).
    pub root: String,
    /// The asset index used (`"32"`: the newest installed, unless asked for another).
    pub index_id: String,
    /// Logical path (`minecraft/sounds/note/harp2.ogg`) to object SHA-1.
    objects: BTreeMap<String, String>,
    /// `sounds.json`: event to its sound names (`block.note_block.harp` to [`note/harp2`]).
    events: BTreeMap<String, Vec<String>>,
}

/// `NSString.appendingPathComponent` as `MinecraftAssets` uses it (see
/// [`append_path_component`](super::append_path_component)): the component may hold slashes.
fn join(root: &str, rest: &str) -> String {
    super::append_path_component(root, rest)
}

fn read_text(fs: &dyn AssetFs, path: &str) -> Result<String, Unavailable> {
    let bytes = fs.read(path).ok_or_else(|| {
        Unavailable::Unreadable(format!("The file \"{path}\" couldn't be opened."))
    })?;
    String::from_utf8(bytes).map_err(|_| {
        Unavailable::Unreadable(format!(
            "The file \"{path}\" couldn't be opened because it isn't in the correct format."
        ))
    })
}

fn parse_json(path: &str, text: &str) -> Result<JsonValue, Unavailable> {
    JsonValue::parse(text).map_err(|e| Unavailable::Unreadable(format!("{path}: {e}")))
}

impl MinecraftAssets {
    pub fn new(
        root: impl Into<String>,
        index_id: impl Into<String>,
        objects: BTreeMap<String, String>,
        events: BTreeMap<String, Vec<String>>,
    ) -> MinecraftAssets {
        MinecraftAssets {
            root: root.into(),
            index_id: index_id.into(),
            objects,
            events,
        }
    }

    /// `MinecraftAssets.locate(root:index:)`: the installed assets, with the newest index (or
    /// `index`), or why there are none.
    pub fn locate(
        fs: &dyn AssetFs,
        root: &str,
        index: Option<&str>,
    ) -> Result<MinecraftAssets, Unavailable> {
        if !fs.exists(&join(root, "objects")) {
            return Err(Unavailable::NoAssets(root.into()));
        }
        let dir = join(root, "indexes");
        let ids: Vec<String> = fs
            .list(&dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.ends_with(".json"))
            .map(|n| n[..n.len() - 5].to_string())
            .collect();
        let id = match index {
            Some(i) => Some(i.to_string()),
            None => newest(&ids).map(String::from),
        };
        let id = match id {
            Some(id) if ids.contains(&id) => id,
            _ => return Err(Unavailable::NoIndex(root.into())),
        };
        let index_path = join(&dir, &format!("{id}.json"));
        let parsed = parse_json(&index_path, &read_text(fs, &index_path)?)?;
        let mut objects = BTreeMap::new();
        if let Some(o) = parsed.get("objects").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                if let Some(h) = v.get("hash").and_then(JsonValue::as_str) {
                    objects.insert(k.to_string(), h.to_string());
                }
            }
        }
        let sj = objects
            .get("minecraft/sounds.json")
            .cloned()
            .ok_or_else(|| Unavailable::NoSoundsJson(id.clone()))?;
        let base = MinecraftAssets::new(root, id.clone(), objects, BTreeMap::new());
        let sounds_path = base.object_path(&sj);
        let sounds = parse_json(&sounds_path, &read_text(fs, &sounds_path)?)?;
        let mut events = BTreeMap::new();
        if let Some(o) = sounds.as_object() {
            for (event, def) in o.iter() {
                let names = def
                    .get("sounds")
                    .and_then(JsonValue::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| {
                                s.as_str()
                                    .or_else(|| s.get("name").and_then(JsonValue::as_str))
                                    .map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                events.insert(event.to_string(), names);
            }
        }
        Ok(MinecraftAssets { events, ..base })
    }

    /// `MinecraftAssets.unavailableReason(root:)`: why the assets can't be used, or none when
    /// they can (for a greyed-out palette).
    pub fn unavailable_reason(fs: &dyn AssetFs, root: &str) -> Option<String> {
        match MinecraftAssets::locate(fs, root, None) {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        }
    }

    /// `objectPath(_:)`: where the object named `hash` lies.
    pub fn object_path(&self, hash: &str) -> String {
        let prefix: String = hash.chars().take(2).collect();
        join(&self.root, &format!("objects/{prefix}/{hash}"))
    }

    /// `sounds(event:)`: the sound names an event plays (`block.note_block.harp` to
    /// [`note/harp2`]).
    pub fn sounds(&self, event: &str) -> Vec<String> {
        self.events.get(event).cloned().unwrap_or_default()
    }

    /// How many objects the index lists.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// How many sound events `sounds.json` named.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// The object SHA-1 the index names for a logical path (`minecraft/sounds/note/harp2.ogg`).
    pub fn object_hash(&self, logical: &str) -> Option<&str> {
        self.objects.get(logical).map(String::as_str)
    }

    /// `file(sound:)`: a sound (`note/harp2`) as a file on disk and its SHA-1, checked: the
    /// object exists and hashes to its name. Never the soundtrack or the discs.
    pub fn file(&self, fs: &dyn AssetFs, sound: &str) -> Result<(String, String), Unavailable> {
        if is_forbidden(sound) {
            return Err(Unavailable::MissingObject(sound.into()));
        }
        let logical = format!("minecraft/sounds/{sound}.ogg");
        let hash = self
            .objects
            .get(&logical)
            .ok_or_else(|| Unavailable::MissingObject(logical.clone()))?;
        let path = self.object_path(hash);
        let data = fs
            .read(&path)
            .ok_or_else(|| Unavailable::MissingObject(logical.clone()))?;
        if &sha1_hex(&data) != hash {
            return Err(Unavailable::BadHash(logical));
        }
        Ok((path, hash.clone()))
    }
}

/// `MinecraftAssets.isForbidden`: sound paths that are never read, the soundtrack and the
/// music discs.
pub fn is_forbidden(sound: &str) -> bool {
    sound.starts_with("music/") || sound.starts_with("records/")
}

/// `MinecraftAssets.newest`: the numerically newest index id ("5" < "27" < "32"). Ids that are
/// not numbers rank below every number, among themselves by name.
pub fn newest(ids: &[String]) -> Option<&str> {
    fn key(s: &str) -> (i64, &str) {
        (s.parse::<i64>().unwrap_or(-1), s)
    }
    let mut best: Option<&str> = None;
    for id in ids.iter().map(String::as_str) {
        best = match best {
            Some(b) if key(b) >= key(id) => Some(b),
            _ => Some(id),
        };
    }
    best
}

/// `MinecraftAssets.sha1`: the lowercase hex SHA-1 of `data` (RFC 3174). The object names are
/// SHA-1s, so this is an integrity check on the user's own files, not a security measure.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut block = |chunk: &[u8]| {
        let mut w = [0u32; 80];
        for (i, b) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    };
    let mut chunks = data.chunks_exact(64);
    for c in &mut chunks {
        block(c);
    }
    let rest = chunks.remainder();
    let mut tail = Vec::with_capacity(128);
    tail.extend_from_slice(rest);
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for c in tail.chunks_exact(64) {
        block(c);
    }
    let mut out = String::with_capacity(40);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}
