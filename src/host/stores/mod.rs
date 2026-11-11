//! The stores on disk: compositions, sets, favorites, voice variations, notes and sessions.
//! Ports the file halves of `Session/{CompositionStore, SetStore, FavoritesStore,
//! VoiceVariationStore, SessionStore}.swift` and `Journal/Notes.swift` (`NotesStore`); the
//! values and their edit functions are `djengine`. Directories honour the same environment
//! variables as the Swift app (`$DJ_COMPOSITIONS_DIR`, `$DJ_SETS_DIR`).
//!
//! The app-support folder is `~/Library/Application Support/<ns>` on macOS and
//! `$XDG_DATA_HOME/<ns>` (else `~/.local/share/<ns>`) elsewhere, where `<ns>` is the build-time
//! `DJRUST_FORMAT_NS` (default `djrust`; README.md, "Build-time settings"): the same folder
//! name, so a file moved between machines is found.

pub mod captures;
pub mod composition;
pub mod favorites;
pub mod loops;
pub mod maps;
pub mod sessions;
pub mod sets;
pub mod variations;

use std::io;
use std::path::{Path, PathBuf};

use crate::json::JsonValue;

/// The app's support folder (`FileManager.urls(for: .applicationSupportDirectory)` +
/// `DJRUST_FORMAT_NS`, default `djrust`).
pub fn support_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    crate::host::platform::data_dir_in(&home).join(env!("DJRUST_FORMAT_NS"))
}

/// `$name` when set and non-empty, else `support_dir()/sub`.
pub fn env_or_support(name: &str, sub: &str) -> PathBuf {
    match std::env::var_os(name).filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => support_dir().join(sub),
    }
}

/// The folder djrust's own persistence lives in, `<support dir>/djrust`: the always-on journal
/// ([`crate::host::journal::app_directory`]) and the loop row's takes
/// ([`crate::host::take_library::default_directory`]). Under the folder the Swift app uses but in a
/// name it never writes: two apps appending to one `library.json` are not safe (PORTING.md,
/// open decision 3), and one journal folder would mix two apps' runs in `djctl journal list`.
/// Follows the captures folder's convention (`<support>/captures`).
pub fn djrust_dir() -> PathBuf {
    support_dir().join("djrust")
}

/// `$name` when set and non-empty, else `djrust_dir()/sub`.
pub fn env_or_djrust(name: &str, sub: &str) -> PathBuf {
    dir_from(std::env::var_os(name), || djrust_dir().join(sub))
}

/// `value` when it is set and non-empty, else `fallback()` (the environment lookup, split out so
/// a test can give it a value without touching the process's environment).
pub fn dir_from(value: Option<std::ffi::OsString>, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    match value.filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => fallback(),
    }
}

/// Write `bytes` over `path` atomically (a temp file in the same folder, then a rename), the
/// way Swift's `.atomic` write does; creates the folder.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.part", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// A pretty-printed, key-sorted JSON text in `JSONEncoder`'s `.prettyPrinted + .sortedKeys`
/// style (`"key" : value`, two-space indent).
pub fn pretty_sorted(v: &JsonValue) -> String {
    fn go(v: &JsonValue, depth: usize, out: &mut String) {
        let pad = |n: usize| "  ".repeat(n);
        match v {
            JsonValue::Array(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, x) in a.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    go(x, depth + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            JsonValue::Object(o) if !o.is_empty() => {
                let mut keys: Vec<&str> = o.keys().collect();
                keys.sort_by(|a, b| crate::json::utf16_cmp(a, b));
                out.push_str("{\n");
                for (i, k) in keys.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&JsonValue::str(*k).js_stringified());
                    out.push_str(" : ");
                    go(o.get(k).unwrap(), depth + 1, out);
                    out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            other => out.push_str(&other.js_stringified()),
        }
    }
    let mut out = String::new();
    go(v, 0, &mut out);
    out
}

/// A fresh temporary folder path for tests (not created).
pub fn temp_path(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Parse a file as JSON; None when it is missing or unreadable.
#[doc(hidden)]
pub fn read_json(path: &Path) -> Option<JsonValue> {
    let text = std::fs::read_to_string(path).ok()?;
    JsonValue::parse(&text).ok()
}
