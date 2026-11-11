//! Import of beats.bitwrap.io's saved MIDI bindings (`localStorage['pn-midi-device-bindings']`,
//! beats-bitwrap-io `public/lib/backend/audio-io.js`, MIT) into a device's map.
//!
//! beats keys its bindings by the connected device's name and splits them in two lists of
//! `[number, binding]` pairs (the channel is not kept):
//!
//! ```json
//! { "MPK Mini Mk II": { "cc":  [[74, {"key":"fx:reverb-wet","selector":"..."}], [20, {"type":"mute","target":"bass"}]],
//!                       "pad": [[44, "drop"], [45, {"type":"click","selector":"[data-midi-action=\"generate\"]","label":"Generate"}]] } }
//! ```
//!
//! A binding is a macro id (a pad's bare string), a `mute` of a part or section, a `click` on
//! a button (found by its `data-midi-action`) or a slider (`fx:<id>`, `tempo`,
//! `mix:<group>:<control>`). The ones djrust has a target for become [`Binding`]s aimed at the
//! control with that number in the profile; the rest are listed in [`Imported::skipped`] with
//! the reason, so nothing disappears quietly.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::jog::ProfileError;
use crate::control::map::{template, UserMap};
use crate::control::registry;
use crate::control::surface::{Binding, ControlType, DJActionKind, SurfaceProfile};
use crate::json::JsonValue;

/// What beats bound a control to.
#[derive(Clone, Debug, PartialEq)]
pub enum BeatsBinding {
    /// A pad that fires a macro (`"drop"`).
    Macro(String),
    /// A mute toggle of a part, riff group or section.
    Mute { target: String },
    /// A click on a button tagged `data-midi-action`, or on another selector.
    Click { selector: String, label: String },
    /// A slider by its key (`fx:reverb-wet`, `tempo`, `mix:drums:pn-mixer-vol`).
    Slider { key: String },
    /// Something this version does not know.
    Other(String),
}

/// One device's saved bindings.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatsDevice {
    /// The name beats stored them under (the port name).
    pub name: String,
    /// `(CC number, binding)`.
    pub cc: Vec<(u8, BeatsBinding)>,
    /// `(note number, binding)`.
    pub pad: Vec<(u8, BeatsBinding)>,
}

fn binding_of(v: &JsonValue) -> BeatsBinding {
    if let Some(s) = v.as_str() {
        return BeatsBinding::Macro(s.to_string());
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    match s("type").as_deref() {
        Some("mute") => BeatsBinding::Mute { target: s("target").unwrap_or_default() },
        Some("click") => BeatsBinding::Click {
            selector: s("selector").unwrap_or_default(),
            label: s("label").unwrap_or_default(),
        },
        Some(other) => BeatsBinding::Other(other.to_string()),
        None => match s("key") {
            Some(key) => BeatsBinding::Slider { key },
            None => BeatsBinding::Other("unrecognised".to_string()),
        },
    }
}

fn pairs(v: Option<&JsonValue>) -> Vec<(u8, BeatsBinding)> {
    let Some(a) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    a.iter()
        .filter_map(|pair| {
            let p = pair.as_array()?;
            let n = p.first()?.as_f64()?;
            if !(0.0..=127.0).contains(&n) {
                return None;
            }
            Some((n as u8, binding_of(p.get(1)?)))
        })
        .collect()
}

/// Parse the stored object (the whole `pn-midi-device-bindings` value). Every device in it, in
/// the order stored.
pub fn parse(text: &str) -> Result<Vec<BeatsDevice>, ProfileError> {
    let j = JsonValue::parse(text).map_err(|e| ProfileError(format!("not JSON: {e}")))?;
    let o = j
        .as_object()
        .ok_or_else(|| ProfileError("expected an object of devices".to_string()))?;
    let mut out = Vec::new();
    for (name, entry) in o.iter() {
        if !entry.is_object_like() {
            continue;
        }
        out.push(BeatsDevice {
            name: name.to_string(),
            cc: pairs(entry.get("cc")),
            pad: pairs(entry.get("pad")),
        });
    }
    if out.is_empty() {
        return Err(ProfileError("no devices in it".to_string()));
    }
    Ok(out)
}

/// One control beats had that djrust left out, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// "CC 74" or "note 36".
    pub control: String,
    pub why: String,
}

/// One binding made, with how it was read.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub binding: Binding,
    /// "CC 20" / "note 44".
    pub control: String,
    /// The nearest target rather than the same thing (shuffle instruments → next sound).
    pub approximate: bool,
}

/// What an import makes of one device.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    /// beats' name for the device.
    pub from: String,
    /// The djrust profile the bindings are aimed at.
    pub profile: String,
    pub made: Vec<Made>,
    pub skipped: Vec<Skipped>,
}

impl Imported {
    /// Lay the bindings over `map` (they replace what the map had for those controls).
    pub fn apply(&self, map: &mut UserMap) {
        for m in &self.made {
            map.set(m.binding.clone());
        }
    }

    /// A few words per line, for a report.
    pub fn report(&self) -> String {
        let mut s = format!(
            "{} → {}: {} bound, {} left out\n",
            self.from,
            self.profile,
            self.made.len(),
            self.skipped.len()
        );
        for m in &self.made {
            s.push_str(&format!(
                "  {}{}: {}\n",
                m.control,
                if m.approximate { " (nearest)" } else { "" },
                m.binding.summary()
            ));
        }
        for k in &self.skipped {
            s.push_str(&format!("  {}: left out, {}\n", k.control, k.why));
        }
        s
    }
}

/// The profile beats' device name belongs to: the first whose `match` finds it.
pub fn profile_for<'a, I: IntoIterator<Item = &'a SurfaceProfile>>(
    device: &str,
    profiles: I,
) -> Option<&'a SurfaceProfile> {
    profiles.into_iter().find(|p| p.matches(device))
}

/// Where the control with this number is in `profile`: the first group of one of `kinds` that
/// holds it.
fn find_control(profile: &SurfaceProfile, number: u8, kinds: &[ControlType]) -> Option<(String, usize)> {
    profile.groups.iter().find_map(|g| {
        if !kinds.contains(&g.kind) {
            return None;
        }
        g.control_numbers()
            .iter()
            .position(|n| *n == number)
            .map(|i| (g.id.clone(), i))
    })
}

fn action_of(label_or_selector: &str) -> Option<&str> {
    // `[data-midi-action="generate"]`
    let rest = label_or_selector.split("data-midi-action=\"").nth(1)?;
    rest.split('"').next()
}

/// What djrust target a beats binding is, if there is one. `(binding with the target filled in,
/// nearest-not-same)`.
fn target_of(b: &BeatsBinding) -> Result<(Binding, bool), String> {
    use crate::fx::{MasterFxKey, MasterMacro};
    use crate::wave::macros::NetMacro;
    match b {
        BeatsBinding::Macro(id) => {
            if NetMacro::from_id(id).is_some() || MasterMacro::from_id(id).is_some() {
                let mut t = template(DJActionKind::Macro);
                t.macro_id = Some(id.clone());
                Ok((t, false))
            } else {
                Err(format!("no macro called {id:?} here"))
            }
        }
        BeatsBinding::Mute { target } => {
            let role = target.to_lowercase();
            let roles = registry::info(DJActionKind::RoleMute)
                .params
                .iter()
                .find(|p| p.kind == registry::ParamKind::Role)
                .map_or(&[][..], |p| p.choices);
            if roles.contains(&role.as_str()) {
                let mut t = template(DJActionKind::RoleMute);
                t.role = Some(role);
                Ok((t, false))
            } else {
                Err(format!("mute of {target:?} is a beats riff group or section, not a part here"))
            }
        }
        BeatsBinding::Click { selector, label } => {
            match action_of(selector).or_else(|| action_of(label)).unwrap_or("") {
                "generate" => Ok((template(DJActionKind::Regenerate), false)),
                "shuffle" => Ok((template(DJActionKind::SoundNext), true)),
                "" => Err(format!("a click on {selector:?}, which has no target here")),
                other => Err(format!("the {other:?} button has no target here")),
            }
        }
        BeatsBinding::Slider { key } => {
            if let Some(id) = key.strip_prefix("fx:") {
                let plain = match id {
                    "reverb-wet" => Some(DJActionKind::ReverbWet),
                    "reverb-size" => Some(DJActionKind::ReverbSize),
                    "master-vol" => Some(DJActionKind::Master),
                    _ => None,
                };
                if let Some(k) = plain {
                    return Ok((template(k), false));
                }
                if MasterFxKey::from_id(id).is_some() {
                    let mut t = template(DJActionKind::MasterFx);
                    t.macro_id = Some(id.to_string());
                    return Ok((t, false));
                }
                return Err(format!("the {id:?} FX slider has no master FX here"));
            }
            if key == "tempo" {
                return Err("beats' tempo slider (60-300) has no counterpart: decks run their own tempo".to_string());
            }
            Err(format!("the {key:?} slider has no target here"))
        }
        BeatsBinding::Other(t) => Err(format!("a {t:?} binding is not one this version knows")),
    }
}

/// Convert one device's bindings for `profile`: each CC and pad is aimed at the control of that
/// number. A binding whose target djrust lacks, or whose number the profile has no control for,
/// is skipped with the reason.
pub fn convert(dev: &BeatsDevice, profile: &SurfaceProfile) -> Imported {
    let mut made = Vec::new();
    let mut skipped = Vec::new();
    let mut one = |kind: &str, number: u8, b: &BeatsBinding, kinds: &[ControlType]| {
        let control = format!("{kind} {number}");
        let Some((group, index)) = find_control(profile, number, kinds) else {
            skipped.push(Skipped { control, why: format!("{} has no {kind} {number}", profile.name) });
            return;
        };
        match target_of(b) {
            Ok((mut binding, approximate)) => {
                binding.group = group;
                binding.index = Some(index);
                made.push(Made { binding, control, approximate });
            }
            Err(why) => skipped.push(Skipped { control, why }),
        }
    };
    for (n, b) in &dev.cc {
        one("CC", *n, b, &[ControlType::Cc, ControlType::Cc14, ControlType::Relative]);
    }
    for (n, b) in &dev.pad {
        one("note", *n, b, &[ControlType::Note]);
    }
    Imported { from: dev.name.clone(), profile: profile.name.clone(), made, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::profiles;

    const SAMPLE: &str = r#"{
      "MPK Mini Mk II": {
        "cc": [
          [1,  {"key":"fx:reverb-wet","selector":".pn-fx-slider[data-fx=\"reverb-wet\"]"}],
          [2,  {"type":"mute","target":"bass"}],
          [3,  {"type":"mute","target":"drums-group-3"}],
          [4,  {"key":"tempo","selector":".pn-tempo input","min":60,"max":300}],
          [5,  {"key":"mix:bass:pn-mixer-vol","selector":".pn-mixer-row .pn-mixer-vol"}],
          [99, {"type":"mute","target":"kick"}]
        ],
        "pad": [
          [48, "drop"],
          [49, "no-such-macro"],
          [50, {"type":"click","selector":"[data-midi-action=\"generate\"]","label":"Generate"}],
          [51, {"type":"click","selector":"[data-midi-action=\"shuffle\"]","label":"Shuffle"}],
          [52, {"type":"click","selector":"[data-midi-action=\"stage\"]","label":"Stage"}],
          [53, {"type":"mute","target":"snare"}]
        ]
      },
      "LaunchKey 49": { "cc": [[21, {"key":"fx:master-vol","selector":"x"}]], "pad": [] }
    }"#;

    #[test]
    fn the_stored_object_is_read_device_by_device() {
        let d = parse(SAMPLE).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "MPK Mini Mk II");
        assert_eq!(d[0].cc.len(), 6);
        assert_eq!(d[0].pad.len(), 6);
        assert_eq!(d[0].pad[0], (48, BeatsBinding::Macro("drop".into())));
        assert!(matches!(d[0].cc[1].1, BeatsBinding::Mute { .. }));
        assert_eq!(d[1].cc[0].0, 21);
        assert!(parse("[]").is_err());
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn what_has_a_target_is_bound_to_the_control_with_that_number() {
        let mpk = profiles::mpk_mini_mk2();
        let d = parse(SAMPLE).unwrap();
        let imp = convert(&d[0], &mpk);
        let by = |c: &str| imp.made.iter().find(|m| m.control == c).map(|m| &m.binding);
        // CC 1 is knob 1 of the MPK profile
        let b = by("CC 1").unwrap();
        assert_eq!((b.group.as_str(), b.index, b.action), ("knobs", Some(0), DJActionKind::ReverbWet));
        // a mute of a part
        let b = by("CC 2").unwrap();
        assert_eq!((b.action, b.role.as_deref()), (DJActionKind::RoleMute, Some("bass")));
        // note 48 is a key (the first group of notes holding it): the macro
        let b = by("note 48").unwrap();
        assert_eq!((b.group.as_str(), b.index, b.action), ("keys", Some(48), DJActionKind::Macro));
        assert_eq!(b.macro_id.as_deref(), Some("drop"));
        assert_eq!(by("note 50").unwrap().action, DJActionKind::Regenerate);
        assert_eq!(by("note 53").unwrap().role.as_deref(), Some("snare"));
        assert!(imp.made.iter().find(|m| m.control == "note 51").unwrap().approximate);
        assert_eq!(imp.made.len(), 6, "{}", imp.report());
    }

    #[test]
    fn what_has_no_target_is_left_out_with_a_reason() {
        let mpk = profiles::mpk_mini_mk2();
        let imp = convert(&parse(SAMPLE).unwrap()[0], &mpk);
        let why = |c: &str| imp.skipped.iter().find(|s| s.control == c).map(|s| s.why.as_str()).unwrap_or("");
        assert!(why("CC 3").contains("riff group"), "{}", why("CC 3"));
        assert!(why("CC 4").contains("tempo"), "{}", why("CC 4"));
        assert!(why("CC 5").contains("slider"), "{}", why("CC 5"));
        assert!(why("CC 99").contains("no CC 99"), "a number the profile lacks: {}", why("CC 99"));
        assert!(why("note 49").contains("no macro"), "{}", why("note 49"));
        assert!(why("note 52").contains("stage"), "{}", why("note 52"));
        assert_eq!(imp.skipped.len(), 6);
        let r = imp.report();
        assert!(r.contains("6 bound, 6 left out") && r.contains("(nearest)"), "{r}");
    }

    #[test]
    fn a_device_is_found_by_the_profile_that_matches_its_name() {
        let all = profiles::all_surfaces();
        let p = profile_for("MPK Mini Mk II", all.iter().map(|p| p.as_ref())).unwrap();
        assert_eq!(p.name, "MPK Mini Mk II");
        assert!(profile_for("LaunchKey 49", all.iter().map(|p| p.as_ref())).is_none());
    }

    #[test]
    fn an_unknown_device_imports_into_a_generic_profile_by_number() {
        let g = profiles::generic("LaunchKey 49");
        let d = parse(SAMPLE).unwrap();
        let imp = convert(&d[1], &g);
        assert_eq!(imp.made.len(), 1, "{}", imp.report());
        let b = &imp.made[0].binding;
        assert_eq!((b.group.as_str(), b.index, b.action), ("cc", Some(21), DJActionKind::Master));
    }

    #[test]
    fn applying_lays_the_bindings_over_a_map_and_the_profile_shows_them() {
        let mpk = profiles::mpk_mini_mk2();
        let imp = convert(&parse(SAMPLE).unwrap()[0], &mpk);
        let mut map = UserMap::new("beats");
        imp.apply(&mut map);
        assert_eq!(map.bindings.len(), 6);
        let p = crate::control::map::overlay(&mpk, &map);
        let m = crate::control::bindmap::build(&p);
        let c = m.cell("knobs", 0).unwrap();
        assert_eq!(c.action, Some(DJActionKind::ReverbWet));
        assert_eq!(c.family, "FX");
        // a master FX key (the filters, phaser, crush) goes to the master FX target
        let hp = convert(
            &parse(r#"{"x":{"cc":[[1,{"key":"fx:hp-freq"}]],"pad":[]}}"#).unwrap()[0],
            &mpk,
        );
        assert_eq!(hp.made[0].binding.action, DJActionKind::MasterFx);
        assert_eq!(hp.made[0].binding.macro_id.as_deref(), Some("hp-freq"));
    }
}
