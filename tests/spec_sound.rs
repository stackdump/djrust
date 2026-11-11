//! `TrackSpec` and the sound it resolves, from the Swift suite (`PaletteTests.swift`: the
//! spec-level halves of `PaletteDataTests` and `PaletteDeckTests`, which need no deck). The
//! numbers are Swift's; `spec_golden.rs` holds the same code to Swift's output.
//!
//! These run in `Sounds::nothing()` (no SoundFont, no sampled instrument), which is what
//! `TrackSpec::project()` asks in, and in a folder with the floor bank where a test needs the
//! General MIDI options to be there.

use djrust::json::JsonValue;
use djrust::pflow::{NetBundle, Project};
use djrust::sound::{project_sound_net, synth_name, Installed, PaletteSet, Sounds};
use djrust::voices::VoiceCatalog;
use djrust::TrackSpec;

fn house() -> TrackSpec {
    TrackSpec::new("house", 11.0, Some("standard"))
}

/// `Project.soundNet(role)`: the first playing net (by id) whose palette role is `role`.
fn sound_net<'a>(p: &'a Project, role: &str) -> &'a NetBundle {
    project_sound_net(p, role).unwrap_or_else(|| panic!("no {role} net"))
}

fn options(p: &Project) -> Vec<String> {
    let mut ids: Vec<&str> = p.nets.keys().iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.into_iter()
        .filter_map(|id| p.nets.get(id).and_then(|nb| nb.track.sound.option.clone()))
        .collect()
}

fn floor() -> (Installed, VoiceCatalog) {
    let inst = Installed {
        dir: "/sf".into(),
        files: vec!["MuseScore_General.sf2".into()],
        ..Installed::default()
    };
    let mut c = VoiceCatalog::bundled();
    c.sound_fonts = inst.sound_fonts();
    (inst, c)
}

/// PaletteDataTests.picksAreDeterministicFromSeedAndRole (the composed project's half).
#[test]
fn a_palette_resolves_the_composed_project_the_same_way_twice() {
    let picks = |seed: f64| {
        let spec = TrackSpec::new("house", seed, Some("standard"))
            .with_palette(Some("synthwave"), &Installed::default());
        options(&spec.project())
    };
    assert!(picks(11.0) == picks(11.0) && !picks(11.0).is_empty());
    assert!((1..=12).any(|s| picks(s as f64) != picks(11.0)));
    // a synth option voices that instrument (the composer's is kept to restore)
    let spec = house().with_palette(Some("synthwave"), &Installed::default());
    let pr = spec.project();
    let melody = sound_net(&pr, "melody");
    let opt = melody.track.sound.option.clone().expect("a pick");
    assert_eq!(synth_name(&opt), Some(melody.track.instrument.as_str()));
    assert!(melody.track.sound.composed.is_some());
}

/// PaletteDataTests.studioIsNoPaletteAtAll.
#[test]
fn studio_is_no_palette_at_all() {
    let set = PaletteSet::shared();
    let inst = Installed::default();
    let spec = house();
    assert_eq!(set.normalized(Some("studio"), &inst), None);
    assert_eq!(set.normalized(None, &inst), None);
    assert_eq!(spec.with_palette(Some("studio"), &inst), spec);
    assert_eq!(
        spec.with_palette(Some("jazz"), &inst)
            .with_palette(Some("studio"), &inst)
            .palette(),
        None
    );
    assert_eq!(
        spec.with_palette(Some("jazz"), &inst).palette().as_deref(),
        Some("jazz")
    );
}

/// PaletteDeckTests.anInstrumentPickIsAnOverrideToo (the spec's half: the next version keeps
/// the pick, and clearing it puts the palette's own pick back).
#[test]
fn an_instrument_pick_survives_the_next_version_and_clears() {
    let inst = Installed::default();
    let channels = djrust::sound::sound_channels("house");
    assert!(channels.contains(&6));
    let mut spec = house().with_palette(Some("synthwave"), &inst);
    djrust::wander::carry::with_track_field(&mut spec, "voice", Some("inst:acid"), &[6]);
    let pr = spec.project();
    let b = sound_net(&pr, "bass");
    assert!(b.track.instrument == "acid" && b.track.picked);
    let next = spec.next_version().project();
    assert_eq!(sound_net(&next, "bass").track.instrument, "acid");
    // reset: back to the palette's pick, un-picked
    djrust::wander::carry::with_track_field(&mut spec, "voice", None, &[6]);
    let back = spec.project();
    let b = sound_net(&back, "bass");
    assert!(!b.track.picked && b.track.sound.override_.is_none());
    let want = PaletteSet::shared().named(Some("synthwave")).unwrap().pick(
        "bass",
        "bass",
        11.0,
        PaletteSet::shared(),
        Sounds::nothing(),
    );
    assert_eq!(b.track.sound.option, want);
}

/// PaletteDeckTests.overridesSurviveRecomputesUntilReset (the spec's half: Next and the seed
/// wheel keep the palette and the role's override; a pick of the palette's own comes back).
#[test]
fn a_role_override_survives_next_and_the_seed_wheel() {
    let inst = Installed::default();
    let mut spec = house().with_palette(Some("jazz"), &inst);
    djrust::wander::carry::with_track_field(&mut spec, "voice", Some("synth:sync-lead"), &[4]);
    let pr = spec.project();
    assert_eq!(
        sound_net(&pr, "melody").track.sound.option.as_deref(),
        Some("synth:sync-lead")
    );
    let next = spec.next_version();
    assert!(next.seed != spec.seed && next.palette() == spec.palette());
    assert_eq!(
        sound_net(&next.project(), "melody")
            .track
            .sound
            .option
            .as_deref(),
        Some("synth:sync-lead")
    );
    // the seed wheel: the same song, another seed
    let mut wheel = spec.clone();
    wheel.seed += 3.0;
    assert_eq!(
        sound_net(&wheel.project(), "melody")
            .track
            .sound
            .option
            .as_deref(),
        Some("synth:sync-lead")
    );
    // reset to the palette: jazz's melody has no synth option, and nothing is installed
    djrust::wander::carry::with_track_field(&mut spec, "voice", None, &[4]);
    assert_eq!(
        sound_net(&spec.project(), "melody").track.sound.option,
        None
    );
}

/// PaletteDeckTests.theDrumsFollowThePaletteUntilAKitIsPicked (the spec's half): the kit a
/// user picked rides into the next version; the palette's kit is the drums' otherwise.
#[test]
fn a_picked_kit_rides_into_the_next_version() {
    let inst = Installed::default();
    let kit = "MuseScore_General.sf2#128:32";
    let mut spec = house().with_palette(Some("club"), &inst);
    djrust::wander::carry::with_kit(&mut spec, Some(kit), &[10, 11, 12, 13]);
    let drum_kit = |p: &Project| {
        p.nets
            .iter()
            .find(|(_, nb)| {
                nb.role != "control" && djrust::sound::is_drum_channel(nb.track.channel)
            })
            .and_then(|(_, nb)| nb.track.kit.clone())
    };
    assert_eq!(
        drum_kit(&spec.next_version().project()).as_deref(),
        Some(kit)
    );
    // the palette's own kit: club's first kit option is a bank kit that is not installed
    // here, so its pick is the next one that is (the synth 808)
    let plain = house().with_palette(Some("club"), &inst).project();
    let k = sound_net(&plain, "kit");
    assert_eq!(k.track.sound.option.as_deref(), Some("synth:drums-808"));
    assert_eq!(k.track.sound.kit.as_deref(), Some("synth"));
    assert!(drum_kit(&plain).is_none());
}

/// PaletteDataTests.everyPaletteResolvesEveryRoleHereOrSaysWhy: with nothing installed a
/// sampled palette plays every instrument's own sound, and with the floor bank it plays the
/// General MIDI programs it names.
#[test]
fn what_is_installed_decides_what_a_palette_plays() {
    let spec = house().with_palette(Some("jazz"), &Installed::default());
    let own = spec.project();
    assert!(options(&own).is_empty(), "{:?}", options(&own));
    let composed = house().project();
    assert_eq!(own.canonical_json(), composed.canonical_json());

    let (inst, catalog) = floor();
    let sounds = Sounds {
        catalog: &catalog,
        installed: &inst,
    };
    let played = spec.project_in(sounds);
    let melody = sound_net(&played, "melody")
        .track
        .sound
        .option
        .clone()
        .unwrap();
    assert!(
        ["gm:66", "gm:65", "gm:59", "gm:26"].contains(&melody.as_str()),
        "{melody}"
    );
    // a synth palette plays either way
    let synth = house().with_palette(Some("synth"), &Installed::default());
    assert!(options(&synth.project()).iter().all(|o| o == "synth"));
}

/// `TrackSpec.finish` is a no-op without layers, and asks only after the other layers.
#[test]
fn finish_does_nothing_without_layers() {
    let spec = house();
    let mut p = djrust::compose(&spec.genre, &spec.overrides());
    let before = p.canonical_json();
    spec.finish(&mut p);
    assert_eq!(p.canonical_json(), before);
    assert!(options(&p).is_empty());
    let mut layered = house();
    layered.layers.set(
        "initialMutes",
        JsonValue::Array(vec![JsonValue::str("bass")]),
    );
    assert_eq!(layered.project().initial_mutes, vec!["bass".to_string()]);
}

/// dj-only: what a deck carries into its next track. Its palette, kit, role voices and
/// instrument picks land where the next spec has none of its own; a field the next spec pins
/// on a channel stays.
#[test]
fn the_deck_sound_fills_what_the_next_spec_does_not_pin() {
    use djrust::json::{JsonObject, JsonValue};
    let tracks = |pairs: Vec<(&str, Vec<(&str, &str)>)>| {
        let mut o = JsonObject::new();
        for (ch, fields) in pairs {
            o.set(
                ch,
                JsonObject::from_pairs(fields.into_iter().map(|(k, v)| (k, JsonValue::str(v))))
                    .into(),
            );
        }
        JsonValue::Object(o)
    };
    let mut deck = house();
    deck.layers.set(
        "tracks",
        tracks(vec![
            ("4", vec![("palette", "jazz"), ("voice", "synth:sync-lead")]),
            ("6", vec![("palette", "jazz"), ("instrument", "synth-bass")]),
            (
                "10",
                vec![("kit", "MuseScore_General.sf2#128:32"), ("fx", "x")],
            ),
        ]),
    );
    let mut queued = TrackSpec::new("house", 77.0, Some("minimal"));
    queued
        .layers
        .set("tracks", tracks(vec![("4", vec![("palette", "club")])]));
    let got = queued.carrying_deck_sound(&deck);
    assert_eq!(
        got.layers.get("tracks"),
        Some(&tracks(vec![
            ("4", vec![("palette", "club"), ("voice", "synth:sync-lead")]),
            ("6", vec![("palette", "jazz"), ("instrument", "synth-bass")]),
            ("10", vec![("kit", "MuseScore_General.sf2#128:32")]),
        ])),
        "its own palette stays, the rest of the deck's sound rides along, not its fx"
    );
    assert_eq!((got.seed, got.genre.as_str()), (77.0, "house"));
    // nothing picked on the deck: the spec as it was
    assert_eq!(queued.carrying_deck_sound(&house()), queued);
}
