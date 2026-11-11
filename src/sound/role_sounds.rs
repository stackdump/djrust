//! The fine-tune rows of a net deck's sound panel (`Audio/PaletteSound.swift`: `RoleSoundRow`,
//! `RoleSoundChoice`, `Deck.roleSounds`, `Deck.playingPalette`): one row per palette role of the
//! playing track, with what it plays, whether the user overrode it, and the options it could
//! play. `RoleSoundRow::json` is what `dj_state` (`sound`) and `dj_palettes` (with a deck) show,
//! and what `setRoleVoice` validates a role against.
//!
//! A pure function of the project, the voicing seed, the palette data and what the host has
//! installed ([`Sounds`]): the host's `Deck` supplies the first two (the deck's project and its
//! voicing seed), [`crate::sound::Installed`] the last, so a row says an option "can't play
//! here" for the same reasons the palette does.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};
use crate::pflow::Project;
use crate::sound::{
    channels_for_role, option_label, picked_name, project_sound_net, unavailable_reason_option,
    PaletteSet, SoundNet, SoundPalette, Sounds, ROLE_ORDER,
};
use crate::wave::synth::synth_drum_kits;

/// One role of a net deck's track as the palette chooser's fine-tune area shows it: what it
/// plays, whether the user overrode it, and the options to choose from (`RoleSoundRow`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSoundRow {
    /// The palette role: kit, bass, melody, harmony, arp, stinger.
    pub role: String,
    pub channels: Vec<i64>,
    /// What it plays now, as a name ("Feedback Rhodes", "pwm-lead", "TR-808 · MuseScore_General").
    pub playing: String,
    /// The option it plays (None: its instrument's own sound, no palette choice).
    pub option: Option<String>,
    /// The user's override (`DeckAction.setRoleVoice`), or None.
    pub override_: Option<String>,
    /// An instrument the user picked for it the older way (a `tracks` instrument), or None.
    pub picked: Option<String>,
    /// A kit the user picked (`DeckAction.setKit`), for the drums.
    pub kit_pick: Option<String>,
    pub choices: Vec<RoleSoundChoice>,
}

/// One option a role can be set to (`RoleSoundChoice`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSoundChoice {
    pub option: String,
    pub label: String,
    /// Why it can't play here, None when it can.
    pub unavailable: Option<String>,
    pub selected: bool,
}

impl RoleSoundRow {
    /// `isOverridden`: the user chose this role's sound: "reset to palette" undoes it.
    pub fn is_overridden(&self) -> bool {
        self.override_.is_some() || self.picked.is_some() || self.kit_pick.is_some()
    }

    /// `json`: as `dj_state` / `dj_palettes` show it (`role`, `channels`, `plays`, `overridden`,
    /// then `option` / `override` / `picked` / `kit` when set, and `options`).
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("role", JsonValue::str(self.role.as_str())),
            (
                "channels",
                JsonValue::Array(self.channels.iter().map(|&c| JsonValue::int(c)).collect()),
            ),
            ("plays", JsonValue::str(self.playing.as_str())),
            ("overridden", JsonValue::Bool(self.is_overridden())),
        ]);
        if let Some(v) = &self.option {
            o.set("option", JsonValue::str(v.as_str()));
        }
        if let Some(v) = &self.override_ {
            o.set("override", JsonValue::str(v.as_str()));
        }
        if let Some(v) = &self.picked {
            o.set("picked", JsonValue::str(v.as_str()));
        }
        if let Some(v) = &self.kit_pick {
            o.set("kit", JsonValue::str(v.as_str()));
        }
        o.set(
            "options",
            JsonValue::Array(
                self.choices
                    .iter()
                    .map(|c| {
                        let mut e = JsonObject::from_pairs([
                            ("option", JsonValue::str(c.option.as_str())),
                            ("label", JsonValue::str(c.label.as_str())),
                        ]);
                        if let Some(u) = &c.unavailable {
                            e.set("unavailable", JsonValue::str(u.as_str()));
                        }
                        JsonValue::Object(e)
                    })
                    .collect(),
            ),
        );
        JsonValue::Object(o)
    }
}

/// `Deck.playingPalette` for a net deck: the palette the project's track plays (its own, else the
/// default). The deck's mode check (a sample deck has none) is the host's.
pub fn playing_palette<'a>(project: &Project, set: &'a PaletteSet) -> Option<&'a SoundPalette> {
    set.playing(project.palette().as_deref())
}

/// `Deck.roleSounds` for a net deck's `project`: every palette role the track plays
/// (`Project.soundRoles`, in the chooser's order) as a row, `voicing_seed` being the deck's
/// (`Deck.voicingSeed`: the spec's seed, or a raw-nets share's). Empty when the palette set has
/// no default palette.
pub fn role_sounds(
    project: &Project,
    voicing_seed: f64,
    set: &PaletteSet,
    ctx: Sounds,
) -> Vec<RoleSoundRow> {
    let Some(palette) = playing_palette(project, set) else {
        return Vec::new();
    };
    let seed = voicing_seed;
    let mut rows = Vec::new();
    for role in ROLE_ORDER {
        // `Project.soundRoles` is the roles some playing net has, `soundNet(role)` its first net
        let Some(nb) = project_sound_net(project, role) else {
            continue;
        };
        let t = &nb.track;
        let is_kit = role == "kit";
        let own = if is_kit { "synth" } else { "own" };
        let mut options: Vec<String> = palette
            .pool(role)
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        let extras: Vec<String> = if is_kit {
            core::iter::once("synth".to_string())
                .chain(synth_drum_kits().iter().map(|k| format!("synth:{k}")))
                .collect()
        } else {
            alloc::vec![own.to_string(), "synth".to_string()]
        };
        for extra in extras {
            if !options.contains(&extra) {
                options.push(extra);
            }
        }
        if let Some(o) = &t.sound.override_ {
            if !options.contains(o) {
                options.push(o.clone());
            }
        }
        let current = t.sound.option.as_deref().unwrap_or(own);
        let legacy_pick = t.picked && t.sound.override_.as_deref().and_then(picked_name).is_none();
        let playing = match &t.kit {
            Some(k) if is_kit => option_label(set, k, "kit", 0.0, ctx),
            _ if legacy_pick => t.instrument.clone(),
            _ if current == "own" => {
                if is_kit {
                    format!("{} synth kit", t.instrument)
                } else {
                    t.instrument.clone()
                }
            }
            _ => option_label(set, current, role, seed, ctx),
        };
        let choices = options
            .iter()
            .map(|o| RoleSoundChoice {
                option: o.clone(),
                label: option_label(set, o, role, seed, ctx),
                unavailable: unavailable_reason_option(set, o, role, ctx),
                selected: !legacy_pick && (!is_kit || t.kit.is_none()) && o == current,
            })
            .collect();
        rows.push(RoleSoundRow {
            role: role.to_string(),
            channels: channels_for_role(&SoundNet::from_project(project, None), role),
            playing,
            option: t.sound.option.clone(),
            override_: t.sound.override_.clone(),
            picked: legacy_pick.then(|| t.instrument.clone()),
            kit_pick: if is_kit { t.kit.clone() } else { None },
            choices,
        });
    }
    rows
}
