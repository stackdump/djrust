//! A deck source's sound edits (`DeckSourceDescriptor.withKit` / `withPalette` /
//! `withTrackField`, `DeckKits.swift`): the poke lands in the source's `tracks` override, so a
//! favorite or a share keeps it. Ports of `SampledKitTests.aKitIsATracksOverride` and
//! `MinecraftTests.aPaletteIsATracksOverrideTheParityPathIgnores` (their render halves are
//! the project equality below: the override is dj-only, never encoded).

mod common;
use common::*;
use djrust::json::JsonValue;
use djrust::session::DeckSource;
use djrust::sound::is_drum_channel;
use djrust::wander::carry::{project_channels, project_drum_channels};
use djrust::TrackSpec;

const FLOOR_KIT: &str = "MuseScore_General.sf2#128:32";

/// The spec a net source holds, layers and all (what a deck composes).
fn spec_of(source: &DeckSource) -> TrackSpec {
    let DeckSource::Net(spec) = source else {
        panic!("not a net: {source:?}")
    };
    spec.clone()
}

// SampledKitTests.aKitIsATracksOverride
#[test]
fn a_kit_is_a_tracks_override() {
    let spec = TrackSpec::new("techno", 7.0, Some("minimal"));
    let project = spec.project();
    let channels = project_drum_channels(&project);
    assert!(!channels.is_empty() && channels.iter().all(|c| is_drum_channel(*c)));
    let source = DeckSource::Net(spec.clone());
    let picked = source.with_kit(Some(FLOOR_KIT), &channels).unwrap();
    let p2 = spec_of(&picked).project();
    // the drum nets carry it, nothing else does
    let mut drums = 0;
    for (id, nb) in p2.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        let drum = is_drum_channel(nb.track.channel);
        assert_eq!(nb.track.kit.is_some(), drum, "{id}");
        if drum {
            assert_eq!(nb.track.kit.as_deref(), Some(FLOOR_KIT));
            drums += 1;
        }
    }
    assert!(drums > 0);
    // the same compose: the kit is dj-only, so the project's text is beats'
    assert_eq!(p2.nets.keys(), project.nets.keys());
    assert_eq!(p2.canonical_json(), project.canonical_json());
    // clearing it leaves no trace
    let cleared = picked.with_kit(None, &channels).unwrap();
    assert_eq!(cleared, source);
}

// MinecraftTests.aPaletteIsATracksOverrideTheParityPathIgnores
#[test]
fn a_palette_is_a_tracks_override() {
    let spec = TrackSpec::new("house", 3.0, Some("minimal"));
    let project = spec.project();
    let channels = project_channels(&project);
    let source = DeckSource::Net(spec);
    let d = source
        .with_palette(Some("minecraft@32"), &channels)
        .unwrap();
    let p2 = spec_of(&d).project();
    for (id, nb) in p2.nets.iter() {
        if nb.role != "control" {
            assert_eq!(nb.track.palette.as_deref(), Some("minecraft@32"), "{id}");
        }
    }
    assert_eq!(p2.canonical_json(), project.canonical_json());
    assert_eq!(d.with_palette(None, &channels).unwrap(), source);
}

/// `withTrackField` on a raw-`nets` share edits the payload; anything that is no net has no
/// `tracks` to edit.
#[test]
fn the_edit_reaches_a_raw_nets_payload_and_refuses_the_rest() {
    let payload =
        JsonValue::parse(r#"{"nets":{},"seed":4,"tracks":{"6":{"instrument":"acid"}}}"#).unwrap();
    let raw = DeckSource::RawNets(payload);
    let edited = raw.with_palette(Some("jazz"), &[6, 4]).unwrap();
    let DeckSource::RawNets(p) = &edited else {
        panic!("{edited:?}")
    };
    assert_eq!(
        // integer-like keys come in ascending order, as JS lists them
        p.get("tracks").unwrap().js_stringified(),
        r#"{"4":{"palette":"jazz"},"6":{"instrument":"acid","palette":"jazz"}}"#
    );
    // and back: the entries that held only the palette go, the others keep their fields
    let back = edited.with_palette(None, &[6, 4]).unwrap();
    assert_eq!(back, raw);
    // a payload that is not an object has no `tracks` to edit
    assert!(DeckSource::RawNets(JsonValue::Null)
        .with_kit(Some(FLOOR_KIT), &[10])
        .is_none());
    assert!(DeckSource::None.with_palette(Some("jazz"), &[4]).is_none());
    let sample = DeckSource::Sample {
        id: "x".into(),
        title: "x".into(),
        grid: None,
    };
    assert!(sample.with_kit(None, &[10]).is_none());
}

/// The source a golden row describes (`spec.swift`'s `described`).
fn source_of(v: &JsonValue) -> DeckSource {
    match s(v, "kind") {
        "net" => DeckSource::Net(TrackSpec::from_payload(v.get("payload").unwrap()).unwrap()),
        "rawNets" => DeckSource::RawNets(v.get("payload").unwrap().clone()),
        "none" => DeckSource::None,
        "sample" => DeckSource::Sample {
            id: "x".into(),
            title: "x".into(),
            grid: None,
        },
        other => panic!("kind {other}"),
    }
}

/// The row's JSON for a source (what Swift's `described` writes for the result).
fn described(d: Option<DeckSource>) -> String {
    let Some(d) = d else {
        return JsonValue::Null.js_stringified();
    };
    let (kind, payload) = match &d {
        DeckSource::Net(spec) => ("net", Some(spec.payload())),
        DeckSource::RawNets(p) => ("rawNets", Some(p.clone())),
        DeckSource::None => ("none", None),
        DeckSource::Sample { .. } => ("sample", None),
    };
    let mut o = djrust::json::JsonObject::new();
    o.set("kind", JsonValue::str(kind));
    if let Some(p) = payload {
        o.set("payload", p);
    }
    JsonValue::Object(o).js_stringified()
}

/// `DeckSourceDescriptor.withTrackField` (`withKit` and `withPalette` are its `kit` and
/// `palette`) against Swift's, over ten sources (a plain net, nets with `tracks` overrides of
/// other fields, of a kit, of a palette alone, a raw-`nets` payload with and without `tracks`
/// and one that is no object, the sources that are no net) and nine edits (set, clear with
/// None and with "", channels out of order, none at all). Swift: `spec.json`, `trackFields`.
#[test]
fn track_field_edits_match_swift() {
    let doc = fixture("spec");
    let mut d = Diffs::default();
    let mut edited = 0;
    for r in rows(&doc, "trackFields") {
        let source = source_of(r.get("input").unwrap());
        let field = s(r, "field");
        let value = r.get("value").and_then(JsonValue::as_str);
        let channels = r.get("channels").unwrap().ints().unwrap();
        let tag = format!("{} {field}={value:?} on {channels:?}", s(r, "source"));
        let got = source.with_track_field(field, value, &channels);
        edited += got.is_some() as usize;
        d.eq(
            tag.clone(),
            described(got),
            r.get("out").unwrap().js_stringified(),
        );
        // withKit / withPalette are the same edit under their field names
        let wrapped = match field {
            "kit" => Some(source.with_kit(value, &channels)),
            "palette" => Some(source.with_palette(value, &channels)),
            _ => None,
        };
        if let Some(w) = wrapped {
            d.eq(
                format!("{tag} (wrapper)"),
                described(w),
                r.get("out").unwrap().js_stringified(),
            );
        }
    }
    assert!(edited > 30, "the nets and the objects are edited: {edited}");
    d.finish("DeckSource.with_track_field");
}
