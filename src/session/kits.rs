//! A deck source's sound edits (`DeckSourceDescriptor.withTrackField` / `withKit` /
//! `withPalette` from `DeckKits.swift`): the poke that sets a `tracks` override field on some
//! channels of the source a deck holds, so a favorite or a share keeps the choice.
//! A `Net` source holds a [`TrackSpec`](crate::spec::TrackSpec), so its edit is
//! `wander::carry`'s on the spec.

use super::model::DeckSource;
use crate::json::JsonValue;
use crate::wander::carry::{set_track_field, with_track_field};

impl DeckSource {
    /// This net with `field` set to `value` (None or "" clears it) in the `tracks` override of
    /// each of `channels`, merged into any entries already there. None for a source that isn't
    /// a net (`withTrackField`).
    pub fn with_track_field(
        &self,
        field: &str,
        value: Option<&str>,
        channels: &[i64],
    ) -> Option<DeckSource> {
        match self {
            DeckSource::Net(spec) => {
                let mut spec = spec.clone();
                with_track_field(&mut spec, field, value, channels);
                Some(DeckSource::Net(spec))
            }
            DeckSource::RawNets(payload) => {
                let mut o = payload.as_object()?.clone();
                set_track_field(&mut o, field, value, channels);
                Some(DeckSource::RawNets(JsonValue::Object(o)))
            }
            DeckSource::None | DeckSource::Sample { .. } => None,
        }
    }

    /// This net's drums playing `kit` (a `DrumKit.id`; None = beats' synth kit) (`withKit`).
    pub fn with_kit(&self, kit: Option<&str>, channels: &[i64]) -> Option<DeckSource> {
        self.with_track_field("kit", kit, channels)
    }

    /// This net with every one of `channels` playing from `palette` (None = none)
    /// (`withPalette(_:channels:)`).
    pub fn with_palette(&self, palette: Option<&str>, channels: &[i64]) -> Option<DeckSource> {
        self.with_track_field("palette", palette, channels)
    }
}
