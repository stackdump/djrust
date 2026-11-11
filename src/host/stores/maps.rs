//! Saved binding maps: one file per device, `maps/<device>.json`, holding all its presets
//! and which one is live (`crate::control::map::MapSet`).

use std::io;
use std::path::PathBuf;

use crate::control::map::MapSet;
use crate::control::profiles;

use super::{pretty_sorted, support_dir, write_atomic};

/// The shipped "performance" preset used to bind the keys straight to `transpose` on the loops.
/// A saved set that still has exactly that gets the shipped binding (the `keys` target, which
/// transposes in transpose mode and plays the deck's lead voice otherwise). A binding the user
/// changed in any way is left alone.
fn migrate_keys(saved: &mut MapSet, shipped: &MapSet) {
    use crate::control::surface::DJActionKind;
    let Some(new) = shipped
        .presets
        .iter()
        .find(|p| p.name == "performance")
        .and_then(|p| p.get("keys", None, None))
        .cloned()
    else {
        return;
    };
    let Some(perf) = saved.presets.iter_mut().find(|p| p.name == "performance") else {
        return;
    };
    let old_shipped = perf.get("keys", None, None).is_some_and(|b| {
        b.action == DJActionKind::Transpose
            && b.deck.as_deref() == Some("L")
            && b.slot.is_none()
            && b.args.len() == 2
            && b.args.get("quant").map(String::as_str) == Some("bar")
            && b.args.get("mode").map(String::as_str) == Some("keep-tempo")
    });
    if old_shipped {
        perf.set(new);
    }
}

pub struct MapStore {
    pub dir: PathBuf,
}

fn slug(device: &str) -> String {
    device
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

impl MapStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        MapStore { dir: dir.into() }
    }
    pub fn default_dir() -> PathBuf {
        support_dir().join("maps")
    }

    fn file(&self, device: &str) -> PathBuf {
        self.dir.join(format!("{}.json", slug(device)))
    }

    /// The saved set for `device`. None if there is none or it does not decode.
    pub fn load(&self, device: &str) -> Option<MapSet> {
        MapSet::parse(&std::fs::read_to_string(self.file(device)).ok()?).ok()
    }

    /// The saved set, else the one that ships for the device (the MPK's performance
    /// preset), else an empty one. A saved set is topped up with the shipped presets it was
    /// never offered (the vocals preset, say) and has the old keys-transpose binding of the
    /// shipped "performance" preset replaced by the `keys` target, which transposes when the
    /// key mode says so (docs/MPK.md).
    pub fn load_or_default(&self, device: &str) -> MapSet {
        let shipped = (device == "MPK Mini Mk II")
            .then(|| MapSet::parse(profiles::MPK_PERFORMANCE_MAP_JSON).ok())
            .flatten();
        match self.load(device) {
            Some(mut saved) => {
                if let Some(s) = &shipped {
                    migrate_keys(&mut saved, s);
                    saved.merge_shipped(s);
                }
                saved
            }
            None => shipped.unwrap_or_else(|| MapSet::new(device)),
        }
    }

    pub fn save(&self, set: &MapSet) -> io::Result<()> {
        let text = pretty_sorted(&set.to_json()) + "\n";
        write_atomic(&self.file(&set.device), text.as_bytes())
    }
}

/// The pad notes learned from a device (`crate::gear::mpk::PadMap`): `maps/<device>.pads.json`,
/// beside its binding presets. It replaces the profile's `padNotes` group at start-up.
pub struct PadStore {
    pub dir: PathBuf,
}

impl PadStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        PadStore { dir: dir.into() }
    }
    pub fn default_dir() -> PathBuf {
        MapStore::default_dir()
    }
    pub fn file(&self, device: &str) -> PathBuf {
        self.dir.join(format!("{}.pads.json", slug(device)))
    }
    pub fn load(&self, device: &str) -> Option<crate::gear::mpk::PadMap> {
        crate::gear::mpk::PadMap::parse(&std::fs::read_to_string(self.file(device)).ok()?).ok()
    }
    pub fn save(&self, map: &crate::gear::mpk::PadMap) -> io::Result<()> {
        let text = pretty_sorted(&map.to_json()) + "\n";
        write_atomic(&self.file(&map.device), text.as_bytes())
    }
    /// Forget the learned notes, so the profile's own show again.
    pub fn clear(&self, device: &str) -> io::Result<bool> {
        match std::fs::remove_file(self.file(device)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}
