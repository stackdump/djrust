//! A net deck's source with an instrument changed: the share envelope's `tracks` override
//! edited on the descriptor itself. Port of `DeckSourceDescriptor.withInstrument`
//! (`DeckVoices.swift`); `withTrackField` / `withKit` / `withPalette` (`DeckKits.swift`) are in
//! [`super::kits`].
//!
//! A `DeckSource::Net` holds the [`TrackSpec`](crate::spec::TrackSpec) itself, as Swift's
//! `.net(TrackSpec)` does, so the edit is `wander::carry`'s (`Wander.setInstrument`) on its
//! `layers`, the `tracks` override that `SessionMix` and the deck compose with. A `RawNets`
//! source carries the envelope payload, which has no spec at all, so the edit is made on the
//! JSON here.

use alloc::string::ToString;

use super::model::DeckSource;
use crate::json::JsonValue;
use crate::wander::carry::set_instrument;

impl DeckSource {
    /// `withInstrument`: this net with the nets on `channel` playing `instrument`: the share
    /// envelope's `tracks` override (`{"<channel>": {"instrument": …}}`, merged into any
    /// already there), so the change records in a session, survives a favorite and goes out
    /// in a share link. None for a source that isn't a net.
    pub fn with_instrument(&self, instrument: &str, channel: i64) -> Option<DeckSource> {
        fn merged(tracks: Option<&JsonValue>, instrument: &str, channel: i64) -> JsonValue {
            let mut all = tracks
                .and_then(JsonValue::as_object)
                .cloned()
                .unwrap_or_default();
            let key = channel.to_string();
            let mut entry = all
                .get(&key)
                .and_then(JsonValue::as_object)
                .cloned()
                .unwrap_or_default();
            entry.set("instrument", JsonValue::str(instrument));
            all.set(key, JsonValue::Object(entry));
            JsonValue::Object(all)
        }
        match self {
            DeckSource::Net(spec) => {
                let mut spec = spec.clone();
                set_instrument(&mut spec, instrument, channel);
                Some(DeckSource::Net(spec))
            }
            DeckSource::RawNets(payload) => {
                let mut o = payload.as_object()?.clone();
                let tracks = merged(o.get("tracks"), instrument, channel);
                o.set("tracks", tracks);
                Some(DeckSource::RawNets(JsonValue::Object(o)))
            }
            DeckSource::None | DeckSource::Sample { .. } => None,
        }
    }
}
