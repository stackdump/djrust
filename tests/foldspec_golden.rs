//! One `TrackSpec`: an edit made through a deck's source is what the set composes.
//!
//! Swift has a single spec type, `DeckSourceDescriptor.net(TrackSpec)`. `withKit` /
//! `withInstrument` / `withTrackField` write the spec's `layers["tracks"]`, and `SessionMix` /
//! `Session.playing` compose that same spec (`spec.project()` = compose + `finish`, which
//! applies `layers`). The Rust had a second type, `session::spec::DeckSpec { spec, layers,
//! part_seeds }`, and `SessionMix` projected its bare `spec`, so a kit or an instrument set
//! through `DeckSource::with_kit` / `with_instrument` was missing from the project the mix
//! composed.
//!
//! Two forms of proof, said in each test:
//! * (b) `swift/foldspec.json`, written by `tools/paritydump/foldspec.swift`: each case edits
//!   specs through the descriptor, plans `SessionMix` over them and composes each mix track's
//!   project. The Rust replays the edits through `DeckSource::{with_kit, with_instrument,
//!   with_track_field}` and compares the sources' JSON, the mix (timings, CID, title), the
//!   composed projects (nets' sound fields, initial mutes, tempo, model sha) and
//!   `Session.playing`'s CID.
//! * (a) from the Swift source (there is no Swift test of this): `SessionMix.init` composes
//!   `spec.project()` for the spec it is given.
//!
//! The mix's own numbers (title, tempo, steps, frames) cannot depend on `layers`: `finish`
//! never touches a song's name, tempo or sections, and the dump's cases show that (a case with
//! an envelope's `tempo` override plans the same mix). What the bug hid was the project.

mod common;
use common::*;
use djrust::cid::{hex, sha256};
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::Project;
use djrust::session::mix::SessionMix;
use djrust::session::{DeckSource, Session};
use djrust::wander::carry::project_drum_channels;
use djrust::TrackSpec;

fn golden() -> JsonValue {
    fixture("foldspec")
}

fn opt(o: Option<&str>) -> JsonValue {
    o.map_or(JsonValue::Null, JsonValue::from)
}

fn channels(v: &JsonValue) -> Vec<i64> {
    v.get("channels")
        .and_then(JsonValue::ints)
        .unwrap_or_else(|| panic!("no channels in {}", v.js_stringified()))
}

fn value(v: &JsonValue) -> Option<String> {
    v.get("value").and_then(JsonValue::as_str).map(String::from)
}

/// One of the dump's edits, replayed through the source (`DeckSource::with_*`).
fn edit(source: DeckSource, e: &JsonValue) -> DeckSource {
    let v = value(e);
    match s(e, "op") {
        "kit" => source.with_kit(v.as_deref(), &channels(e)),
        "instrument" => source.with_instrument(v.as_deref().unwrap(), i(e, "channel")),
        "field" => source.with_track_field(s(e, "field"), v.as_deref(), &channels(e)),
        other => panic!("op {other}"),
    }
    .unwrap_or_else(|| panic!("a net takes the edit {}", e.js_stringified()))
}

/// The case's specs: read from the envelope payload when the dump built one that way,
/// composed from genre / seed / structure otherwise, then edited through their sources.
fn specs_of(case: &JsonValue) -> (Vec<TrackSpec>, Vec<DeckSource>) {
    let specs = rows(case, "specs");
    let payloads = rows(case, "payloads");
    let edits = rows(case, "edits");
    let mut out = Vec::new();
    let mut sources = Vec::new();
    for (k, sp) in specs.iter().enumerate() {
        let plain = match payloads[k].as_str() {
            Some(text) => TrackSpec::from_payload(&JsonValue::parse(text).unwrap()).unwrap(),
            None => TrackSpec::new(
                s(sp, "genre"),
                f(sp, "seed"),
                sp.get("structure").and_then(JsonValue::as_str),
            ),
        };
        let mut source = DeckSource::Net(plain);
        for e in edits[k].as_array().unwrap() {
            source = edit(source, e);
        }
        let DeckSource::Net(spec) = &source else {
            panic!("an edit turned a net into {source:?}")
        };
        out.push(spec.clone());
        sources.push(source);
    }
    (out, sources)
}

/// The dump's `projectRow` for a composed project.
fn project_row(p: &Project) -> JsonValue {
    let nets: Vec<JsonValue> = p
        .nets
        .iter()
        .map(|(id, n)| {
            let t = &n.track;
            JsonValue::Object(JsonObject::from_pairs([
                ("id", JsonValue::from(id)),
                ("role", JsonValue::from(n.role.as_str())),
                ("channel", JsonValue::int(t.channel)),
                ("instrument", JsonValue::from(t.instrument.as_str())),
                ("picked", JsonValue::Bool(t.picked)),
                ("kit", opt(t.kit.as_deref())),
                ("palette", opt(t.palette.as_deref())),
                ("composed", opt(t.sound.composed.as_deref())),
            ]))
        })
        .collect();
    JsonValue::Object(JsonObject::from_pairs([
        ("nets", JsonValue::Array(nets)),
        (
            "initialMutes",
            JsonValue::Array(
                p.initial_mutes
                    .iter()
                    .map(|m| JsonValue::from(m.as_str()))
                    .collect(),
            ),
        ),
        ("tempo", JsonValue::from(bits_hex(p.tempo).as_str())),
        (
            "modelSha",
            JsonValue::from(hex(&sha256(p.canonical_json().as_bytes())).as_str()),
        ),
    ]))
}

/// (b) Edits through the source, the mix over the edited specs, and the project each mix track
/// composes, against Swift's.
#[test]
fn an_edit_through_the_source_is_what_the_mix_composes() {
    let g = golden();
    let mut d = Diffs::default();
    let mut kits_seen = 0;
    for case in rows(&g, "cases") {
        let name = s(case, "name");
        let (specs, sources) = specs_of(case);

        // what a session names each deck by
        for (k, source) in sources.iter().enumerate() {
            d.eq(
                format!("{name}: source {k}"),
                source.json().js_stringified(),
                rows(case, "sources")[k].as_str().unwrap().to_string(),
            );
        }

        // the mix
        let mix = SessionMix::new(&specs, 12.0, 44_100.0).unwrap();
        let want = case.get("mix").unwrap();
        let tracks = rows(want, "tracks");
        d.eq(format!("{name}: tracks"), mix.tracks.len(), tracks.len());
        for (k, (got, w)) in mix.tracks.iter().zip(tracks).enumerate() {
            let tag = format!("{name}: track {k}");
            d.eq(format!("{tag} title"), got.title.as_str(), s(w, "title"));
            d.eq(format!("{tag} deck"), got.deck.as_str(), s(w, "deck"));
            d.eq(
                format!("{tag} bpm"),
                bits_hex(got.bpm),
                s(w, "bpm").to_string(),
            );
            d.eq(
                format!("{tag} played bpm"),
                bits_hex(got.played_bpm),
                s(w, "playedBPM").to_string(),
            );
            d.eq(format!("{tag} steps"), got.steps, i(w, "steps"));
            d.eq(format!("{tag} in"), got.in_frame, i(w, "inFrame"));
            d.eq(format!("{tag} out"), got.out_frame, i(w, "outFrame"));
            // the mix holds the spec it was given, edits and all
            d.eq(format!("{tag} spec"), &got.spec, &specs[k]);
        }
        d.eq(
            format!("{name}: cid"),
            mix.session.cid(),
            s(want, "cid").to_string(),
        );
        d.eq(
            format!("{name}: canonical sha"),
            hex(&sha256(mix.session.canonical().as_bytes())),
            s(want, "canonicalSha").to_string(),
        );
        d.eq(
            format!("{name}: title"),
            mix.title(),
            s(want, "title").to_string(),
        );

        // the project each mix track composes: `spec.project()`, `layers` applied
        let projects = rows(case, "projects");
        d.eq(
            format!("{name}: projects"),
            mix.tracks.len(),
            projects.len(),
        );
        for (k, (t, w)) in mix.tracks.iter().zip(projects).enumerate() {
            let got = project_row(&t.spec.project());
            kits_seen += rows(w, "nets")
                .iter()
                .filter(|n| n.get("kit").and_then(JsonValue::as_str).is_some())
                .count();
            d.eq(
                format!("{name}: project {k}"),
                got.canonical_js(),
                w.canonical_js(),
            );
        }

        // `Session.playing`
        let playing = Session::playing(&specs[0], None, 2.0, 44_100.0).map(|s| s.cid());
        d.eq(
            format!("{name}: playing"),
            playing,
            case.get("playing")
                .and_then(JsonValue::as_str)
                .map(String::from),
        );
    }
    // the dump has kits on composed nets, or the comparison above would hold for a project that
    // ignored `layers`
    assert!(kits_seen > 0, "no composed net carries a kit in the dump");
    d.finish("foldspec");
}

/// (a) `SessionMix.init` composes `spec.project()` for each spec it is given, and an edit
/// through the source lives in that spec (`layers["tracks"]`): the kit is on every drum net of
/// the project the mix track composes, and on nothing else; the deck's session source names it.
/// With two types the bare spec was projected, so the same assertion read "no kit".
#[test]
fn a_kit_set_through_the_source_is_in_the_project_the_mix_composes() {
    const KIT: &str = "MuseScore_General.sf2#128:32";
    let plain = TrackSpec::new("techno", 42.0, Some("standard"));
    let drums = project_drum_channels(&plain.project());
    assert!(!drums.is_empty());
    let source = DeckSource::Net(plain.clone())
        .with_kit(Some(KIT), &drums)
        .unwrap();
    let DeckSource::Net(edited) = &source else {
        panic!("not a net")
    };
    let other = TrackSpec::new("house", 7.0, Some("standard"));
    let mix = SessionMix::new(&[edited.clone(), other.clone()], 12.0, 44_100.0).unwrap();

    // the mix keeps the edited spec, as the track's and as deck A's source
    assert_eq!(&mix.tracks[0].spec, edited);
    assert_eq!(mix.session.decks.get("A"), Some(&source));

    // the project it composes: the kit on the drum nets, nothing on the others
    let project = mix.tracks[0].spec.project();
    let mut drum_nets = 0;
    for (id, nb) in project.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        if drums.contains(&nb.track.channel) {
            assert_eq!(nb.track.kit.as_deref(), Some(KIT), "{id}");
            drum_nets += 1;
        } else {
            assert_eq!(nb.track.kit, None, "{id}");
        }
    }
    assert!(drum_nets > 0);
    // and it is the project a deck plays for that source (`spec.project()`), not the bare compose
    assert_eq!(
        project_row(&project).canonical_js(),
        project_row(&edited.project()).canonical_js()
    );
    assert_ne!(
        project_row(&project).canonical_js(),
        project_row(&plain.project()).canonical_js(),
        "the kit is in the project"
    );

    // the edit does not move the set: same song, same timing; only the deck's source differs
    let unedited = SessionMix::new(&[plain, other], 12.0, 44_100.0).unwrap();
    let times = |m: &SessionMix| -> Vec<(String, i64, i64, i64)> {
        m.tracks
            .iter()
            .map(|t| (t.title.clone(), t.steps, t.in_frame, t.out_frame))
            .collect()
    };
    assert_eq!(times(&mix), times(&unedited));
    assert_ne!(mix.session.cid(), unedited.session.cid());
}
