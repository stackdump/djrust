//! Drum kits a net's drum lanes can play instead of beats' synth kit (`Audio/DrumKits.swift`):
//! a preset of an installed sound bank, named by `file#bank:program`
//! (`MuseScore_General.sf2#128:0`) so a share names the same kit on any machine that has
//! the bank.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::bank::BankPreset;
use super::{join_path, last_component, SamplerSource};

/// A drum kit (`DrumKit`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrumKit {
    pub id: String,
    /// The preset's name in its bank (`Standard`, `Room`, `Minecraft Kit`).
    pub name: String,
    /// The bank's file name without its extension (`MuseScore_General`, `gs_instruments`).
    pub bank: String,
    pub source: SamplerSource,
}

impl DrumKit {
    /// `Standard · MuseScore_General`.
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, self.bank)
    }
}

/// What a drum lane plays on a kit: its kind on the General MIDI key for it
/// (`DrumKits.gmNote`).
pub fn gm_note(kind: &str) -> i64 {
    match kind {
        "kick" => 36,
        "snare" => 38,
        "clap" => 39,
        "hat" => 42,
        "openhat" => 46,
        "crash" => 49,
        _ => 36,
    }
}

/// How long a kit hit is held before its note-off: a cymbal rings, a drum stops on its own
/// (`DrumKits.holdMs`).
pub fn hold_ms(kind: &str) -> f64 {
    match kind {
        "crash" => 2500.0,
        "openhat" => 600.0,
        _ => 400.0,
    }
}

pub use super::sampler::velocity_scale;

/// A melodic preset that is a kit by its name (`DrumKits.isKitName`).
pub fn is_kit_name(name: &str) -> bool {
    let n = format!(" {} ", name.to_lowercase());
    n.contains(" kit ") || n.contains(" drum kit") || n.ends_with(" kit ")
}

/// The kits in `banks` (path, its presets), bank by bank, each bank's in program order
/// (`DrumKits.scan`, with the preset reading done by the caller).
pub fn scan(banks: &[(String, Vec<BankPreset>)]) -> Vec<DrumKit> {
    let mut out = Vec::new();
    for (path, presets) in banks {
        let file = last_component(path);
        let stem = match file.rfind('.') {
            Some(i) if i > 0 => &file[..i],
            _ => file,
        };
        let mut sorted: Vec<&BankPreset> = presets.iter().collect();
        sorted.sort_by_key(|p| (p.bank, p.program));
        for p in sorted {
            if p.bank == 128 || is_kit_name(&p.name) {
                out.push(DrumKit {
                    id: format!("{file}#{}:{}", p.bank, p.program),
                    name: p.name.clone(),
                    bank: stem.to_string(),
                    source: SamplerSource::Kit {
                        path: path.clone(),
                        bank: p.bank,
                        program: p.program,
                    },
                });
            }
        }
    }
    out
}

/// `file#bank:program` → its parts (`DrumKits.parse`).
pub fn parse(id: &str) -> Option<(String, i64, i64)> {
    let hash = id.rfind('#')?;
    let file = &id[..hash];
    let rest = &id[hash + 1..];
    let parts: Vec<&str> = rest.split(':').filter(|s| !s.is_empty()).collect();
    if file.is_empty() || file.contains('/') || parts.len() != 2 {
        return None;
    }
    let b: i64 = parts[0].parse().ok()?;
    let p: i64 = parts[1].parse().ok()?;
    if !(0..=128).contains(&b) || !(0..=127).contains(&p) {
        return None;
    }
    Some((file.to_string(), b, p))
}

/// The kit an id names, wherever its bank is installed: `dir` for the SoundFont folder
/// (`files` = its listing), Apple's GM bank for `gs_instruments.dls` (`gm_bank` = whether it
/// is present). `None` for an id that names no installed bank. Resolving reads no bank.
/// `DrumKits.source(for:dir:)`.
pub fn source_for(id: &str, dir: &str, files: &[String], gm_bank: bool) -> Option<SamplerSource> {
    let (file, bank, program) = parse(id)?;
    let path = if file == super::GM_BANK_FILE {
        if !gm_bank {
            return None;
        }
        super::GM_BANK_PATH.to_string()
    } else {
        if !files.contains(&file) {
            return None;
        }
        join_path(dir, &file)
    };
    Some(SamplerSource::Kit {
        path,
        bank,
        program,
    })
}

/// GM2 / GS's Brush kit program (`DrumKits.brushProgram`).
pub const BRUSH_PROGRAM: i64 = 40;

/// The Brush kit among a bank's presets: bank 128 program 40, else the lowest bank-128
/// program whose name has "brush" in it (`DrumKits.brushPreset`).
pub fn brush_preset(presets: &[BankPreset]) -> Option<&BankPreset> {
    let mut kits: Vec<&BankPreset> = presets.iter().filter(|p| p.bank == 128).collect();
    kits.sort_by_key(|p| p.program);
    kits.iter()
        .find(|p| p.program == BRUSH_PROGRAM)
        .or_else(|| {
            kits.iter()
                .find(|p| p.name.to_lowercase().contains("brush"))
        })
        .copied()
}

/// The Brush kit of `kit`'s bank given that bank's presets (`DrumKits.brushKit(for:)`); nil
/// for a bank without one or a kit that isn't a bank-128 one.
pub fn brush_kit(kit: &SamplerSource, presets: &[BankPreset]) -> Option<SamplerSource> {
    let SamplerSource::Kit { path, bank, .. } = kit else {
        return None;
    };
    if *bank != 128 {
        return None;
    }
    let p = brush_preset(presets)?;
    Some(SamplerSource::Kit {
        path: path.clone(),
        bank: p.bank,
        program: p.program,
    })
}
