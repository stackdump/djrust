//! Any other target: no real-time scheduling, `std::thread::sleep`, the XDG folders.

use std::path::{Path, PathBuf};

pub(super) fn promote_thread() -> bool {
    false
}

pub(super) fn precise_sleep(_d: f64) -> bool {
    false
}

pub(super) fn data_dir_in(home: &Path) -> PathBuf {
    super::xdg("XDG_DATA_HOME", home, ".local/share")
}

pub(super) fn cache_dir_in(home: &Path) -> PathBuf {
    super::xdg("XDG_CACHE_HOME", home, ".cache")
}
