//! The user binding layer: a map overlays a device profile's `layout`, so any control can be
//! bound to any registry target ([`registry`](super::registry)), and the result is an
//! ordinary [`SurfaceProfile`], which means the device drawing, the walk, the monitor and the
//! controller all show and obey it with no further plumbing.
//!
//! A [`MapSet`] is one device's saved presets ("scenes") and which one is live. Switching is
//! a data change plus [`overlay`]; a `mapPreset` target does it from a pad.
//!
//! JSON, schema `djrust/map/v1`:
//! `{schema, device, active, presets: [{name, bindings: [<profile layout entries>]}]}`.
//! An entry whose action is `unbound` clears a control the profile binds.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::jog::ProfileError;
use crate::control::registry::{self, TargetKind};
use crate::control::surface::{Binding, ControlEvent, DJActionKind, SurfaceProfile};
use crate::json::{JsonObject, JsonValue};

pub const SCHEMA: &str = "djrust/map/v1";

/// One preset: bindings that replace the profile's for the controls they name.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct UserMap {
    pub name: String,
    pub bindings: Vec<Binding>,
}

fn same_control(a: &Binding, b: &Binding) -> bool {
    a.group == b.group && a.index == b.index && a.shift == b.shift
}

impl UserMap {
    pub fn new(name: &str) -> UserMap {
        UserMap {
            name: name.to_string(),
            bindings: Vec::new(),
        }
    }

    /// Bind (or rebind) the control `b` names.
    pub fn set(&mut self, b: Binding) {
        self.bindings.retain(|x| !same_control(x, &b));
        self.bindings.push(b);
    }

    /// Forget the override for a control, so the profile's own binding shows through.
    pub fn forget(&mut self, group: &str, index: Option<usize>, shift: Option<bool>) -> bool {
        let n = self.bindings.len();
        self.bindings
            .retain(|x| !(x.group == group && x.index == index && x.shift == shift));
        self.bindings.len() != n
    }

    pub fn get(&self, group: &str, index: Option<usize>, shift: Option<bool>) -> Option<&Binding> {
        self.bindings
            .iter()
            .find(|x| x.group == group && x.index == index && x.shift == shift)
    }
}

/// A device's presets and the live one.
#[derive(Clone, Debug, PartialEq)]
pub struct MapSet {
    /// The surface profile's name.
    pub device: String,
    pub active: usize,
    pub presets: Vec<UserMap>,
    /// The names of the shipped presets this set has already been offered, so one that ships
    /// later is added once and one you deleted does not come back. Empty in a file from before
    /// the field (see [`MapSet::LEGACY_OFFERED`]).
    pub offered: Vec<String>,
}

impl MapSet {
    /// The presets that shipped before `offered` was kept.
    pub const LEGACY_OFFERED: [&'static str; 3] = ["performance", "vocoder", "decks"];

    pub fn new(device: &str) -> MapSet {
        MapSet {
            device: device.to_string(),
            active: 0,
            presets: alloc::vec![UserMap::new("default")],
            offered: Vec::new(),
        }
    }

    pub fn current(&self) -> &UserMap {
        &self.presets[self.active.min(self.presets.len() - 1)]
    }

    pub fn current_mut(&mut self) -> &mut UserMap {
        let i = self.active.min(self.presets.len() - 1);
        &mut self.presets[i]
    }

    /// Switch to the preset called `name`. False if there is none.
    pub fn select(&mut self, name: &str) -> bool {
        match self.presets.iter().position(|p| p.name == name) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    /// Step through the presets, wrapping (a "next scene" pad).
    pub fn step(&mut self, delta: i64) {
        let n = self.presets.len() as i64;
        self.active = (self.active as i64 + delta).rem_euclid(n) as usize;
    }

    /// A new empty preset, or a copy of the live one, and switch to it.
    pub fn add(&mut self, name: &str, copy_current: bool) {
        let mut p = if copy_current {
            self.current().clone()
        } else {
            UserMap::new("")
        };
        p.name = name.to_string();
        self.presets.retain(|x| x.name != name);
        self.presets.push(p);
        self.active = self.presets.len() - 1;
    }

    /// Drop the live preset (the last one stays, emptied).
    pub fn remove_current(&mut self) {
        if self.presets.len() > 1 {
            self.presets.remove(self.active.min(self.presets.len() - 1));
            self.active = self.active.min(self.presets.len() - 1);
        } else {
            self.presets[0].bindings.clear();
        }
    }

    /// Apply what a `mapPreset` binding asks: `"next"`, `"prev"` or a preset name.
    pub fn apply_request(&mut self, preset: &str) {
        match preset {
            "next" | "" => self.step(1),
            "prev" => self.step(-1),
            name => {
                self.select(name);
            }
        }
    }

    /// Add the presets `shipped` has that this set was never offered, and mark them offered.
    /// Returns the names added. A set without `offered` (an older file) was offered the presets
    /// that shipped before the field.
    pub fn merge_shipped(&mut self, shipped: &MapSet) -> Vec<String> {
        let offered: Vec<String> = if self.offered.is_empty() {
            Self::LEGACY_OFFERED.iter().map(|s| s.to_string()).collect()
        } else {
            self.offered.clone()
        };
        let mut added = Vec::new();
        let active = self.current().name.clone();
        for p in &shipped.presets {
            let known = offered.contains(&p.name) || self.presets.iter().any(|x| x.name == p.name);
            if !known {
                // before "decks" (the empty preset the cycle ends on), when it is there
                let at = self
                    .presets
                    .iter()
                    .position(|x| x.name == "decks")
                    .unwrap_or(self.presets.len());
                self.presets.insert(at, p.clone());
                added.push(p.name.clone());
            }
        }
        let mut now = offered;
        for p in &shipped.presets {
            if !now.contains(&p.name) {
                now.push(p.name.clone());
            }
        }
        self.offered = now;
        self.select(&active);
        added
    }

    pub fn to_json(&self) -> JsonValue {
        let presets = self
            .presets
            .iter()
            .map(|p| {
                JsonValue::Object(JsonObject::from_pairs([
                    ("name", JsonValue::str(p.name.clone())),
                    (
                        "bindings",
                        JsonValue::Array(p.bindings.iter().map(Binding::to_json).collect()),
                    ),
                ]))
            })
            .collect();
        let mut o = JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("device", JsonValue::str(self.device.clone())),
            (
                "active",
                JsonValue::str(self.current().name.clone()),
            ),
            ("presets", JsonValue::Array(presets)),
        ]);
        if !self.offered.is_empty() {
            o.set(
                "offered",
                JsonValue::Array(self.offered.iter().map(|n| JsonValue::str(n.clone())).collect()),
            );
        }
        JsonValue::Object(o)
    }

    pub fn from_json(j: &JsonValue) -> Result<MapSet, ProfileError> {
        if j.get("schema").and_then(|s| s.as_str()) != Some(SCHEMA) {
            return Err(ProfileError("not a djrust map".into()));
        }
        let device = j
            .get("device")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProfileError("map: device".into()))?
            .to_string();
        let mut presets = Vec::new();
        for p in j
            .get("presets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ProfileError("map: presets".into()))?
        {
            let mut m = UserMap::new(p.get("name").and_then(|v| v.as_str()).unwrap_or("preset"));
            for b in p.get("bindings").and_then(|v| v.as_array()).into_iter().flatten() {
                m.set(Binding::from_json(b)?);
            }
            presets.push(m);
        }
        if presets.is_empty() {
            presets.push(UserMap::new("default"));
        }
        let offered = j
            .get("offered")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let mut set = MapSet {
            device,
            active: 0,
            presets,
            offered,
        };
        if let Some(a) = j.get("active").and_then(|v| v.as_str()) {
            set.select(a);
        }
        Ok(set)
    }

    pub fn parse(text: &str) -> Result<MapSet, ProfileError> {
        MapSet::from_json(&JsonValue::parse(text).map_err(|e| ProfileError(e.to_string()))?)
    }
}

/// `profile` with `map`'s bindings laid over its layout. An `unbound` entry parks the
/// control. The profile's own entries stay for every control the map does not name.
pub fn overlay(profile: &SurfaceProfile, map: &UserMap) -> SurfaceProfile {
    let mut p = profile.clone();
    for b in &map.bindings {
        p.layout.retain(|x| !same_control(x, b));
        p.layout.push(b.clone());
    }
    p
}

/// MIDI learn: the binding for the control that just moved, with `template` (the target the
/// user picked, with its arguments) aimed at it. A keyed target (transpose, keys) takes the
/// whole group, so one learned key binds every key; anything else binds that one control.
/// `shift`: bind the SHIFT layer.
pub fn learn(event: &ControlEvent, template: &Binding, shift: Option<bool>) -> Binding {
    let keyed = registry::info(template.action).drive == TargetKind::Keyed;
    let mut b = template.clone();
    b.group = event.group.clone();
    b.index = if keyed { None } else { Some(event.index) };
    b.shift = shift;
    b
}

/// A blank template for target `k`, with the deck defaulted to the aimed one when it takes
/// a deck.
pub fn template(k: DJActionKind) -> Binding {
    let wants_deck = registry::info(k)
        .params
        .iter()
        .any(|p| p.kind == registry::ParamKind::Deck);
    Binding {
        group: String::new(),
        index: None,
        action: k,
        deck: wants_deck.then(|| "*".to_string()),
        slot: None,
        shift: None,
        reason: None,
        role: None,
        macro_id: None,
        param: None,
        args: Default::default(),
    }
}
