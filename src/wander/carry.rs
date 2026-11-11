//! Spec and project edits the walk makes (`DeckSourceDescriptor.withTrackField` and friends
//! from `DeckKits.swift`, `Wander.setInstrument` / `channel(of:in:)`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};
use crate::pflow::Project;
use crate::sound::is_drum_channel;
use crate::spec::TrackSpec;

/// `withTrackField`: this spec with `field` set to `value` (None or "" clears it) in the
/// `tracks` override of each of `channels`, merged into any entries already there.
pub fn with_track_field(spec: &mut TrackSpec, field: &str, value: Option<&str>, channels: &[i64]) {
    set_track_field(&mut spec.layers, field, value, channels);
}

/// `withTrackField`'s `set`, on any object that holds a `tracks` override: a spec's layers, or
/// a raw-`nets` share's payload (`DeckSource::with_track_field`).
pub fn set_track_field(o: &mut JsonObject, field: &str, value: Option<&str>, channels: &[i64]) {
    let mut all = o
        .get("tracks")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    for channel in channels {
        let key = channel.to_string();
        let mut entry = all
            .get(&key)
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        match value {
            Some(v) if !v.is_empty() => entry.set(field, JsonValue::str(v)),
            _ => {
                entry.remove(field);
            }
        }
        if entry.is_empty() {
            all.remove(&key);
        } else {
            all.set(key, entry.into());
        }
    }
    if all.is_empty() {
        o.remove("tracks");
    } else {
        o.set("tracks", all.into());
    }
}

/// `withPalette(_:channels:)`.
pub fn with_palette(spec: &mut TrackSpec, palette: Option<&str>, channels: &[i64]) {
    with_track_field(spec, "palette", palette, channels);
}

/// `withKit(_:channels:)`.
pub fn with_kit(spec: &mut TrackSpec, kit: Option<&str>, channels: &[i64]) {
    with_track_field(spec, "kit", kit, channels);
}

/// `Wander.setInstrument`: the `tracks` override's `instrument` on `channel`.
pub fn set_instrument(spec: &mut TrackSpec, instrument: &str, channel: i64) {
    let mut all = spec
        .layers
        .get("tracks")
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
    all.set(key, entry.into());
    spec.layers.set("tracks", all.into());
}

/// `TrackSpec.palette`: the palette the spec names (on any channel), as recorded.
pub fn spec_palette(spec: &TrackSpec) -> Option<String> {
    let tracks = spec.layers.get("tracks").and_then(JsonValue::as_object)?;
    for (_, o) in tracks.iter() {
        if let Some(p) = o.get("palette").and_then(JsonValue::as_str) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// `Wander.specNamesKit`: does the spec's `tracks` override name `kit` on a channel?
pub fn spec_names_kit(spec: &TrackSpec, kit: Option<&str>) -> bool {
    let Some(kit) = kit else { return true };
    spec.layers
        .get("tracks")
        .and_then(JsonValue::as_object)
        .map(|t| {
            t.iter()
                .any(|(_, o)| o.get("kit").and_then(JsonValue::as_str) == Some(kit))
        })
        .unwrap_or(false)
}

/// `Project.channels`: the channels of every net that plays (not control nets), sorted.
pub fn project_channels(p: &Project) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for (_, nb) in p.nets.iter() {
        if nb.role != "control" && !out.contains(&nb.track.channel) {
            out.push(nb.track.channel);
        }
    }
    out.sort_unstable();
    out
}

/// `Project.drumChannels`: the channels of the drum nets (kick, snare, hats, clap), sorted.
pub fn project_drum_channels(p: &Project) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for (_, nb) in p.nets.iter() {
        if nb.role != "control"
            && is_drum_channel(nb.track.channel)
            && !out.contains(&nb.track.channel)
        {
            out.push(nb.track.channel);
        }
    }
    out.sort_unstable();
    out
}

/// `Wander.channel(of:in:)`: the MIDI channel `role` (a riff group or a lone net's id) plays on
/// in `project`.
pub fn channel_of(role: &str, project: &Project) -> Option<i64> {
    let mut ids: Vec<&str> = project.nets.keys().iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.into_iter().find_map(|id| {
        let nb = project.nets.get(id)?;
        (nb.role != "control" && (nb.riff_group == role || id == role)).then_some(nb.track.channel)
    })
}

/// The empty `layers` object of a fresh spec.
pub fn empty_layers() -> JsonObject {
    JsonObject::new()
}
