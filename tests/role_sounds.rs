//! The fine-tune rows (`Deck.roleSounds`, `RoleSoundRow`, `RoleSoundChoice`, `Deck.playingPalette`
//! in `PaletteSound.swift`) on the project half: the pure builder over a composed project, the
//! deck's voicing seed and what is installed. Each test names the Swift test it ports; the deck
//! and engine halves (`Engine::role_sounds`, the pokes through `DeckAction`) are
//! `djhost/tests/deck_role_sounds.rs`, and the rows against a real Swift deck are
//! `djhost/tests/rolesounds_golden.rs`.
//!
//! These run on a machine with the floor SoundFont (`MuseScore_General.sf2`) installed, so the
//! sampled options a palette lists are playable, and on `Sounds::nothing()` where a test is about
//! what cannot play.

use djrust::json::JsonValue;
use djrust::pflow::Project;
use djrust::sound::{
    playing_palette, role_sounds, synth_name, Installed, PaletteSet, RoleSoundRow, Sounds,
};
use djrust::voices::VoiceCatalog;
use djrust::wander::deck::{set_project_palette, set_project_role_voice};
use djrust::TrackSpec;

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

/// A house track at seed 11 playing from `palette` (`PaletteDeckTests.deck`, without the engine).
fn house(palette: Option<&str>, ctx: Sounds) -> Project {
    TrackSpec::new("house", 11.0, Some("standard"))
        .with_palette(palette, ctx.installed)
        .project_in(ctx)
}

fn row<'a>(rows: &'a [RoleSoundRow], role: &str) -> &'a RoleSoundRow {
    rows.iter()
        .find(|r| r.role == role)
        .unwrap_or_else(|| panic!("no {role} row"))
}

// PaletteChooserModelTests.theFineTuneRowsShowWhatThePalettePicked
#[test]
fn the_fine_tune_rows_show_what_the_palette_picked() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let p = house(Some("synthwave"), ctx);
    let rows = role_sounds(&p, 11.0, set, ctx);
    assert_eq!(rows.first().map(|r| r.role.as_str()), Some("kit"));
    let roles: Vec<&str> = rows.iter().map(|r| r.role.as_str()).collect();
    for want in ["kit", "bass", "melody"] {
        assert!(roles.contains(&want), "{roles:?}");
    }
    let pal = set.named(Some("synthwave")).unwrap();
    for r in &rows {
        assert_eq!(
            r.choices.iter().filter(|c| c.selected).count(),
            1,
            "{}",
            r.role
        );
        let pool = pal.pool(&r.role).unwrap_or(&[]);
        let head: Vec<&str> = r
            .choices
            .iter()
            .take(pool.len())
            .map(|c| c.option.as_str())
            .collect();
        assert_eq!(head, pool, "{}", r.role);
        assert!(
            r.choices.iter().any(|c| c.option == "synth"),
            "beats' own voice is always there"
        );
        assert!(!r.playing.is_empty() && !r.is_overridden(), "{}", r.role);
    }
    let melody = row(&rows, "melody");
    assert_eq!(
        melody.playing,
        synth_name(melody.option.as_deref().unwrap_or("")).unwrap_or("")
    );
    // the palette is the one the project names, and an unnamed project plays the default
    assert_eq!(
        playing_palette(&p, set).map(|p| p.id.as_str()),
        Some("synthwave")
    );
    let plain = house(None, ctx);
    assert_eq!(
        playing_palette(&plain, set).map(|p| p.id.as_str()),
        Some(set.default_id.as_str())
    );
}

// PaletteDeckTests.overridesSurviveRecomputesUntilReset (the fine-tune rows: the pick, the
// override, the selected choice, the reset)
#[test]
fn an_override_shows_in_its_row_until_reset() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let mut p = house(Some("jazz"), ctx);
    let pick = row(&role_sounds(&p, 11.0, set, ctx), "melody").clone();
    assert!(pick.override_.is_none() && !pick.is_overridden());
    let other = pick
        .choices
        .iter()
        .find(|c| !c.selected && c.unavailable.is_none())
        .expect("another option that can play")
        .option
        .clone();
    set_project_role_voice(&mut p, "melody", Some(&other), 11.0, set, ctx).expect("a change");
    let rows = role_sounds(&p, 11.0, set, ctx);
    let r = row(&rows, "melody");
    assert_eq!(r.override_.as_deref(), Some(other.as_str()));
    assert!(r.is_overridden());
    assert_eq!(
        r.choices
            .iter()
            .find(|c| c.selected)
            .map(|c| c.option.as_str()),
        Some(other.as_str())
    );
    // reset to the palette
    set_project_role_voice(&mut p, "melody", None, 11.0, set, ctx).expect("a change");
    let back = role_sounds(&p, 11.0, set, ctx);
    let b = row(&back, "melody");
    assert!(b.override_.is_none() && !b.is_overridden());
    assert_eq!(b.option, pick.option, "the palette's own pick comes back");
}

// PaletteDeckTests.anInstrumentPickIsAnOverrideToo (the row: `isOverridden`)
#[test]
fn an_instrument_pick_makes_its_row_overridden() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let mut p = house(Some("synthwave"), ctx);
    set_project_role_voice(&mut p, "bass", Some("inst:acid"), 11.0, set, ctx).expect("a change");
    let rows = role_sounds(&p, 11.0, set, ctx);
    let bass = row(&rows, "bass");
    assert!(bass.is_overridden());
    // the pick plays as `inst:`: its name, the override listed among the options and selected
    assert_eq!(bass.override_.as_deref(), Some("inst:acid"));
    assert_eq!(bass.playing, "acid");
    let sel: Vec<&str> = bass
        .choices
        .iter()
        .filter(|c| c.selected)
        .map(|c| c.option.as_str())
        .collect();
    assert_eq!(sel, ["inst:acid"]);
    set_project_role_voice(&mut p, "bass", None, 11.0, set, ctx).expect("a change");
    assert!(!row(&role_sounds(&p, 11.0, set, ctx), "bass").is_overridden());
}

// PaletteDeckTests.setPaletteKeepsOverridesAndTheDefaultClearsIt (the row part: the club's
// melody pool starts with its own voice, and the override of the bass stays)
#[test]
fn a_new_palette_changes_the_options_and_keeps_the_override() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let mut p = house(Some("lofi"), ctx);
    set_project_role_voice(&mut p, "bass", Some("gm:32"), 11.0, set, ctx).expect("a change");
    set_project_palette(&mut p, Some("club"));
    let rows = role_sounds(&p, 11.0, set, ctx);
    assert_eq!(
        row(&rows, "melody")
            .choices
            .first()
            .map(|c| c.option.as_str()),
        Some("synth:rave-stab")
    );
    let bass = row(&rows, "bass");
    assert_eq!(bass.override_.as_deref(), Some("gm:32"));
    assert!(
        bass.choices
            .iter()
            .any(|c| c.option == "gm:32" && c.selected),
        "an override that is no option of the palette is listed, and is the one selected"
    );
}

// PaletteDeckTests.theDrumsFollowThePaletteUntilAKitIsPicked
#[test]
fn the_drums_follow_the_palette_until_a_kit_is_picked() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let mut p = house(Some("club"), ctx);
    let kit = row(&role_sounds(&p, 11.0, set, ctx), "kit").clone();
    assert!(!kit.is_overridden() && kit.option.is_some());
    // the deck's poke (`Deck.setKit`) writes the kit on the drum nets
    let id = "MuseScore_General.sf2#128:32";
    p.set_drum_kit(Some(id));
    let rows = role_sounds(&p, 11.0, set, ctx);
    let k = row(&rows, "kit");
    assert_eq!(k.kit_pick.as_deref(), Some(id));
    assert!(k.is_overridden());
    assert!(
        k.choices.iter().all(|c| !c.selected),
        "a picked kit selects none of the palette's options"
    );
    // reset (`setRoleVoice(kit, nil)`)
    set_project_role_voice(&mut p, "kit", None, 11.0, set, ctx).expect("a change");
    assert_eq!(p.drum_kit(), None);
    assert!(!row(&role_sounds(&p, 11.0, set, ctx), "kit").is_overridden());
}

// `RoleSoundRow.json`: the keys in Swift's order, `option` / `override` / `picked` / `kit` only when
// set, and `options` with `unavailable` only on a choice that cannot play
#[test]
fn the_json_has_swifts_keys_in_swifts_order() {
    let ctx = Sounds::nothing();
    let set = PaletteSet::shared();
    let p = house(Some("jazz"), ctx);
    let rows = role_sounds(&p, 11.0, set, ctx);
    let melody = row(&rows, "melody");
    let j = melody.json();
    let keys: Vec<&str> = j.as_object().unwrap().keys().collect();
    let mut want = vec!["role", "channels", "plays", "overridden"];
    if melody.option.is_some() {
        want.push("option");
    }
    want.push("options");
    assert_eq!(keys, want);
    assert_eq!(j.get("role").and_then(JsonValue::as_str), Some("melody"));
    assert_eq!(j.get("overridden"), Some(&JsonValue::Bool(false)));
    // nothing installed: the palette's sampled options say why they cannot play
    let options = j.get("options").and_then(JsonValue::as_array).unwrap();
    assert!(!options.is_empty());
    assert!(
        options.iter().any(|o| o.get("unavailable").is_some()),
        "jazz's melody lists a sampled option that cannot play with nothing installed"
    );
    for o in options {
        let ok: Vec<&str> = o.as_object().unwrap().keys().collect();
        assert!(ok == ["option", "label"] || ok == ["option", "label", "unavailable"]);
    }
    // an override and a kit pick appear under their own keys
    let mut q = p.clone();
    set_project_role_voice(&mut q, "melody", Some("gm:4"), 11.0, set, ctx).expect("a change");
    q.set_drum_kit(Some("MuseScore_General.sf2#128:32"));
    let rows = role_sounds(&q, 11.0, set, ctx);
    let mj = row(&rows, "melody").json();
    assert_eq!(mj.get("override").and_then(JsonValue::as_str), Some("gm:4"));
    assert_eq!(mj.get("overridden"), Some(&JsonValue::Bool(true)));
    let kj = row(&rows, "kit").json();
    assert_eq!(
        kj.get("kit").and_then(JsonValue::as_str),
        Some("MuseScore_General.sf2#128:32")
    );
}

// an instrument picked the older way (the spec's `tracks` instrument, `track.picked`) is not an
// `inst:` override: its row plays that instrument, shows it as `picked`, selects no option and is
// overridden until reset (`RoleSoundRow.picked`, `legacyPick`)
#[test]
fn an_instrument_picked_the_older_way_selects_nothing_and_resets() {
    let (inst, cat) = floor();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let set = PaletteSet::shared();
    let mut spec = TrackSpec::new("house", 11.0, Some("standard"))
        .with_palette(Some("synthwave"), ctx.installed);
    djrust::wander::carry::with_track_field(&mut spec, "instrument", Some("acid"), &[6]);
    let mut p = spec.project_in(ctx);
    let rows = role_sounds(&p, 11.0, set, ctx);
    let bass = row(&rows, "bass");
    assert_eq!(bass.picked.as_deref(), Some("acid"));
    assert_eq!(bass.playing, "acid");
    assert!(bass.override_.is_none() && bass.option.is_none());
    assert!(bass.is_overridden());
    assert!(
        bass.choices.iter().all(|c| !c.selected),
        "a picked instrument selects none of the palette's options"
    );
    assert_eq!(
        bass.json().get("picked").and_then(JsonValue::as_str),
        Some("acid")
    );
    // the other rows are the palette's
    assert!(!row(&rows, "melody").is_overridden());
    // reset: the palette's own pick comes back and the row is not overridden
    set_project_role_voice(&mut p, "bass", None, 11.0, set, ctx).expect("a change");
    let back = role_sounds(&p, 11.0, set, ctx);
    let b = row(&back, "bass");
    assert!(b.picked.is_none() && !b.is_overridden() && b.option.is_some());
    assert_eq!(b.choices.iter().filter(|c| c.selected).count(), 1);
}
