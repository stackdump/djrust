//! The loops a layer move draws from and the deck's live layer list (`LayerLoop`,
//! `LayerEntry`, `LoopLayers.defaultGain`).
//!
//! These are the session package's own types (`crate::layers`, next to the roll's `loopLayers`
//! parser): the walk once carried a copy of each, folded into one. The names stay here for the
//! walk's users.

pub use crate::layers::{default_gain, LayerEntry, LayerLoop};
