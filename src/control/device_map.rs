//! `DeviceMap.swift`: every control of a device with what it does, in words.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::surface::{Binding, DJActionKind, SurfaceProfile};

/// One control of a device and what it does (`DeviceMapRow`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMapRow {
    pub group: String,
    pub index: usize,
    /// The control's name from the profile, or its number.
    pub control: String,
    /// MIDI note / CC number (None for pitch bend, program change).
    pub number: Option<i64>,
    /// What it does with the shift layer off, and with it held (None: same).
    pub action: String,
    pub shifted_action: Option<String>,
    pub parked: bool,
}

impl Binding {
    /// A human description: "volume · deck A", "section 3 · deck B",
    /// "unbound — needs BPM" (`Binding.summary`).
    pub fn summary(&self) -> String {
        use DJActionKind as K;
        let on = match self.deck.as_deref() {
            Some("*") => " · aimed deck".to_string(),
            Some(d) => format!(" · deck {d}"),
            None => String::new(),
        };
        let slot = self.slot.unwrap_or(0);
        let role = self.role.as_deref().unwrap_or("?");
        let macro_id = self.macro_id.as_deref().unwrap_or("?");
        match self.action {
            K::Unbound => {
                let mut s = "unbound".to_string();
                if let Some(r) = &self.reason {
                    s.push_str(" — ");
                    s.push_str(r);
                }
                s
            }
            K::Hotcue => format!("hot cue {}{on}", slot + 1),
            K::Clearcue => format!("clear cue {}{on}", slot + 1),
            K::Section => format!("section {}{on}", slot + 1),
            K::SectionPrev => format!("previous section{on}"),
            K::SectionNext => format!("next section{on}"),
            K::SoundPrev => format!("previous sound palette{on}"),
            K::SoundNext => format!("next sound palette{on}"),
            K::RoleMute => format!("mute {role}{on}"),
            K::StemMute => format!("STEMS {role} (mute group){on}"),
            K::Macro => format!("macro {macro_id}{on}"),
            K::Stinger => format!("stinger {}{on}", slot + 1),
            K::Sync => format!(
                "{}{on}",
                if self.slot == Some(1) {
                    "sync to bar"
                } else {
                    "sync"
                }
            ),
            K::Pfl => format!("headphone cue{on}"),
            K::LoopIn => format!("loop in{on}"),
            K::LoopOut => format!("loop out{on}"),
            K::LoopHalve => format!("loop ÷2{on}"),
            K::LoopDouble => format!("loop ×2{on}"),
            K::LoopRecord => format!("loop record (hold){on}"),
            K::ClipToggle => format!("loop {} in/out", slot + 1),
            K::CaptureGrab => format!("capture last beats to a loop{on}"),
            K::CaptureArm => format!("capture live to a loop (hold; tap = last beats){on}"),
            K::CaptureBeats => format!("capture length ½ / ×2{on}"),
            K::LibraryLoad => format!("library loop {} to the row", slot + 1),
            K::LibrarySave => format!("save loop {} to the library", slot + 1),
            K::MasterFx => format!("FX {macro_id}{on}"),
            K::AuxSend => format!(
                "aux send {}{on}",
                self.args.get("strip").map_or_else(|| self.deck.as_deref().unwrap_or("aimed deck").to_string(), |s| s.clone())
            ),
            K::AuxReturn => format!("aux FX return{on}"),
            K::AuxFx => format!("aux {}{on}", self.args.get("key").map_or("effect", String::as_str)),
            K::AuxMute => format!("aux FX return mute{on}"),
            K::PostToggle => format!("master post chain on/off{on}"),
            K::PostPreset => format!(
                "master post preset {}{on}",
                self.args.get("op").map_or("next", String::as_str)
            ),
            K::ReverbWet => format!("reverb wet{on}"),
            K::ReverbSize => format!("reverb size{on}"),
            K::FxUnit => format!("FX unit {}{on}", self.param.as_deref().unwrap_or("?")),
            K::FxKill => format!("FX unit kill (next beat){on}"),
            K::FxReset => format!("FX unit reset (next beat){on}"),
            K::FocusDeck => {
                if self.slot == Some(2) {
                    "aim at the loops (L)".to_string()
                } else {
                    format!(
                        "aim at deck {}",
                        if self.slot == Some(1) { "B" } else { "A" }
                    )
                }
            }
            K::Regenerate => format!("regenerate (next seed){on}"),
            K::AutoNext => format!("auto-next{on}"),
            K::AutoNextBars => format!(
                "auto-next every N bars: {}{}{on}",
                self.args.get("op").map_or("toggle", String::as_str),
                self.args.get("bars").map_or(String::new(), |b| format!(" {b}"))
            ),
            K::EqLow => format!("EQ low{on}"),
            K::EqMid => format!("EQ mid{on}"),
            K::EqHigh => format!("EQ high{on}"),
            K::HeadphoneMix => "headphone mix".to_string(),
            K::HeadphoneLevel => "headphone level".to_string(),
            K::NextTrack => format!("next track{on}"),
            K::Keys => "live keys (by key mode: a deck, the loops or the TD-3)".to_string(),
            K::Nudge => "tempo nudge (focus deck)".to_string(),
            K::SeedStep => "seed ± (armed decks, else the playing net; grabs wander)".to_string(),
            K::DeckArm => format!("arm for the seed wheel{on}"),
            K::ClipToDeck => format!(
                "loop clip {} onto a deck{on}",
                self.slot.map_or("(newest)".to_string(), |s| (s + 1).to_string())
            ),
            K::CopyDeck => format!("copy the other deck here{on}"),
            K::Wander => "wander on/off (focus deck)".to_string(),
            K::WanderPace => "wander pace ± (focus deck)".to_string(),
            K::AutoMix => "auto-mix on/off".to_string(),
            K::AutoMixNow => "auto-mix: mix now".to_string(),
            K::Shift => "SHIFT".to_string(),
            K::OneShot => format!(
                "one-shot {}",
                self.args.get("sample").map_or("(no sample)", String::as_str)
            ),
            K::LoopFile => format!(
                "loop {}",
                self.args.get("sample").map_or("(no sample)", String::as_str)
            ),
            K::MapPreset => format!(
                "binding preset {}",
                self.args.get("preset").map_or("next", String::as_str)
            ),
            K::Transpose => "transpose loops to the key".to_string(),
            K::LoopRoot => "set the loop root to the key".to_string(),
            other => format!("{}{on}", other.raw()),
        }
    }
}

impl SurfaceProfile {
    /// Every control of the device with its action(s), grouped in profile order
    /// (`SurfaceProfile.deviceMap()`).
    pub fn device_map(&self) -> Vec<DeviceMapRow> {
        use crate::control::surface::ControlType;
        let mut rows = Vec::new();
        for g in &self.groups {
            let nums = g.control_numbers();
            let count = if g.kind == ControlType::Pitchbend {
                1
            } else {
                nums.len()
            };
            for i in 0..count {
                let mut candidates: Vec<&Binding> = self
                    .layout
                    .iter()
                    .filter(|b| b.group == g.id && (b.index == Some(i) || b.index.is_none()))
                    .collect();
                // exact entries before group-wide ones (a stable sort)
                candidates.sort_by_key(|b| if b.index.is_some() { 0 } else { 1 });
                let plain = candidates.iter().find(|b| b.shift != Some(true)).copied();
                let shifted = candidates.iter().find(|b| b.shift == Some(true)).copied();
                let action = plain
                    .map(Binding::summary)
                    .or_else(|| shifted.map(Binding::summary))
                    .unwrap_or_else(|| "unmapped".to_string());
                let shifted_text = if shifted.is_some() {
                    shifted.map(Binding::summary)
                } else {
                    None
                };
                rows.push(DeviceMapRow {
                    group: g.id.clone(),
                    index: i,
                    control: g.name(i).map_or_else(|| format!("#{i}"), String::from),
                    number: nums.get(i).map(|n| i64::from(*n)),
                    action,
                    shifted_action: shifted_text,
                    parked: plain
                        .or(shifted)
                        .map_or(false, |b| b.action == DJActionKind::Unbound),
                });
            }
        }
        rows
    }
}
