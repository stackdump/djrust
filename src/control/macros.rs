//! The macro and FX-unit catalogues the controller layer needs by id: `NetMacro`
//! (WaveMacros.swift), `MasterMacro` / `MasterFxKey` (MasterFx/MasterMacro.swift) and
//! `FxParam` (Audio/FxUnit.swift). The wave engine and master chain own them (the types, the
//! ids, groups and ranges); the controllers read them from here by id, which is all that is
//! left of this module besides the two things only the controllers know of a `NetMacro` (which
//! roles it chases, and whether it is a tempo macro) and the slider arithmetic.

use crate::fx::{MasterFxKey, MasterMacro};

pub use crate::fx::unit::FxParam;
pub use crate::wave::macros::{MacroTargets, NetMacro};

impl NetMacro {
    /// The catalogue's selector for the macros whose UI the JS chases: the `mute` kind
    /// (`drop`, `breakdown`, `solo-drums`, `cut`). The compound macro, the beat repeat and
    /// the tempo macros never call `macroPulse` in the JS: None (`chaseTargets`, `MacroChase`).
    pub fn chase_targets(self) -> Option<MacroTargets> {
        match self {
            NetMacro::Drop | NetMacro::SoloDrums => Some(MacroTargets::NonDrums),
            NetMacro::Breakdown => Some(MacroTargets::DrumsOnly),
            NetMacro::Cut => Some(MacroTargets::Everything),
            _ => None,
        }
    }

    /// Half-time, tape-stop and tempo-anchor are the tempo group (yellow on the APC).
    pub fn is_tempo(self) -> bool {
        matches!(
            self,
            NetMacro::HalfTime | NetMacro::TapeStop | NetMacro::TempoAnchor
        )
    }
}

/// The group ("FX", "Pitch", "Feel") of master macro `id`, or None when `id` is not a master
/// macro (`MasterMacro(id:)?.group`).
pub fn master_macro_group(id: &str) -> Option<&'static str> {
    MasterMacro::from_id(id).map(MasterMacro::group)
}

/// The group ("Pan", "Shape", "FX") of aux macro `id` (one of the eleven of [`crate::fx::AuxMacro`]),
/// or None when `id` is not one.
pub fn aux_macro_group(id: &str) -> Option<&'static str> {
    crate::fx::AuxMacro::from_id(id).map(crate::fx::AuxMacro::group)
}

/// A slider position, 0…1 of its travel, in the slider's own units (its range, rounded):
/// what a controller's master FX fader and `DeckAction.masterFx` both set
/// (`MasterFxKey.sliderValue`). None when `id` is not a master FX key.
pub fn master_fx_slider_value(id: &str, position: f64) -> Option<i64> {
    MasterFxKey::from_id(id).map(|k| k.slider_value(position))
}
