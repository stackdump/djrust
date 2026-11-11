//! The fine-tune rows on a deck (`Deck.roleSounds`, `Deck.playingPalette`): the `roleSounds`
//! assertions of `PaletteTests.swift` that the first ports of `PaletteDeckTests` left out
//! ("without the panel rows": `deck_actions.rs`, `deck_wander.rs`), through [`Engine::role_sounds`]
//! and the real `DeckAction` pokes. Each test names the Swift test it ports. The rows against a
//! real Swift deck are `rolesounds_golden.rs`; the pure builder is `djengine/tests/role_sounds.rs`.

mod common_track;

use common_track::Stepper;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, SessionEventKind};
use djrust::sound::{PaletteSet, RoleSoundRow};
use djrust::TrackSpec;
use djrust::host::deck::{Applied, PerformCtx};

fn perform(st: &mut Stepper, a: DeckAction) -> Applied {
    st.e.perform(&a, PerformCtx::default())
}

/// `PaletteDeckTests.deck(_:)`: a house track at seed 11 on deck A from `palette`, 24 kHz.
fn deck(palette: &str) -> Stepper {
    let mut st = Stepper::new(24_000.0, true);
    let s = st.e.with_sounds(|ctx| {
        TrackSpec::new("house", 11.0, Some("standard")).with_palette(Some(palette), ctx.installed)
    });
    perform(
        &mut st,
        DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Net(s),
        },
    );
    st
}

fn voice(st: &mut Stepper, role: &str, voice: Option<&str>) {
    perform(
        st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: role.into(),
            voice: voice.map(String::from),
        },
    );
}

fn row(st: &Stepper, role: &str) -> RoleSoundRow {
    st.e.role_sounds("A")
        .into_iter()
        .find(|r| r.role == role)
        .unwrap_or_else(|| panic!("no {role} row"))
}

// PaletteChooserModelTests.theFineTuneRowsShowWhatThePalettePicked (the deck form: "nothing on the
// deck", a palette loaded, the rows)
#[test]
fn the_fine_tune_rows_show_what_the_palette_picked() {
    let empty = Stepper::new(24_000.0, true);
    assert!(empty.e.role_sounds("A").is_empty(), "nothing on the deck");
    assert!(
        empty.e.playing_palette("A").is_none(),
        "a sample deck plays no palette"
    );
    let mut st = deck("synthwave");
    let rows = st.e.role_sounds("A");
    assert_eq!(rows.first().map(|r| r.role.as_str()), Some("kit"));
    let roles: Vec<&str> = rows.iter().map(|r| r.role.as_str()).collect();
    for want in ["kit", "bass", "melody"] {
        assert!(roles.contains(&want), "{roles:?}");
    }
    let p = PaletteSet::shared().named(Some("synthwave")).unwrap();
    assert_eq!(
        st.e.playing_palette("A").map(|p| p.id.as_str()),
        Some("synthwave")
    );
    for r in &rows {
        // one option is selected in every row but the Hits': with no Apple Loops to curate them
        // (a Mac without them, and this build, which has none) a Hit is a one-shot voice
        // (`synth:airhorn`) that is no option of the palette, so the stinger row selects none.
        // Swift's own row says so on such a machine (`swift/rolesounds.json`, "house on
        // synthwave"); the Swift test's `== 1` for every row holds where the Hits are loops.
        let selected = r.choices.iter().filter(|c| c.selected).count();
        if r.role == "stinger" {
            assert!(selected <= 1, "{}", r.role);
        } else {
            assert_eq!(selected, 1, "{}", r.role);
        }
        let pool = p.pool(&r.role).unwrap_or(&[]);
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
    let melody = row(&st, "melody");
    assert_eq!(
        melody.playing,
        djrust::sound::synth_name(melody.option.as_deref().unwrap_or("")).unwrap_or("")
    );
    // a default deck plays the default palette; a sample deck again has nothing
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: None,
        },
    );
    assert_eq!(
        st.e.playing_palette("A").map(|p| p.id.clone()),
        Some(PaletteSet::shared().default_id.clone())
    );
}

// PaletteDeckTests.overridesSurviveRecomputesUntilReset (the rows: the pick, the override, the
// selected choice, and the reset back to the palette's own pick)
#[test]
fn an_override_shows_in_its_row_and_the_reset_puts_the_palettes_pick_back() {
    let mut st = deck("jazz");
    assert_eq!(st.e.palette("A").as_deref(), Some("jazz"));
    let pick = row(&st, "melody");
    assert!(pick.override_.is_none() && !pick.is_overridden());
    let other = pick
        .choices
        .iter()
        .find(|c| !c.selected && c.unavailable.is_none())
        .expect("another option that can play")
        .option
        .clone();
    voice(&mut st, "melody", Some(&other));
    let r = row(&st, "melody");
    assert_eq!(r.override_.as_deref(), Some(other.as_str()));
    assert!(r.is_overridden());
    assert_eq!(
        r.choices
            .iter()
            .find(|c| c.selected)
            .map(|c| c.option.as_str()),
        Some(other.as_str())
    );
    // the row and the net agree
    let melody_net = djrust::sound::project_sound_net(
        st.e.deck("A").unwrap().project.as_ref().unwrap(),
        "melody",
    )
    .unwrap();
    assert_eq!(
        melody_net.track.sound.option.as_deref(),
        Some(other.as_str())
    );
    // Next and the seed wheel keep it
    st.e.step_seed("A", 3);
    assert_eq!(
        row(&st, "melody").override_.as_deref(),
        Some(other.as_str())
    );
    // reset to the palette: the palette's own pick comes back
    voice(&mut st, "melody", None);
    let back = row(&st, "melody");
    assert!(back.override_.is_none() && !back.is_overridden());
    let seed = st.e.deck("A").unwrap().seed;
    let set = PaletteSet::shared();
    let expected = st.e.with_sounds(|ctx| {
        set.named(Some("jazz"))
            .unwrap()
            .pick("melody", "melody", seed, set, ctx)
    });
    assert_eq!(back.option, expected);
}

// PaletteDeckTests.anInstrumentPickIsAnOverrideToo
#[test]
fn an_instrument_pick_makes_its_row_overridden() {
    let mut st = deck("synthwave");
    voice(&mut st, "bass", Some("inst:acid"));
    assert!(row(&st, "bass").is_overridden());
    voice(&mut st, "bass", None);
    assert!(!row(&st, "bass").is_overridden());
}

// PaletteDeckTests.setPaletteKeepsOverridesAndTheDefaultClearsIt (the row part)
#[test]
fn a_new_palette_changes_the_options_and_keeps_the_override() {
    let mut st = deck("lofi");
    voice(&mut st, "bass", Some("gm:32"));
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("club".into()),
        },
    );
    assert_eq!(
        row(&st, "melody")
            .choices
            .first()
            .map(|c| c.option.as_str()),
        Some("synth:rave-stab")
    );
    assert_eq!(row(&st, "bass").override_.as_deref(), Some("gm:32"));
}

// PaletteDeckTests.theDrumsFollowThePaletteUntilAKitIsPicked (the rows: the kit pick and its reset)
#[test]
fn the_drums_follow_the_palette_until_a_kit_is_picked() {
    let mut st = deck("club");
    let kit = row(&st, "kit");
    assert!(!kit.is_overridden() && kit.option.is_some());
    let id = "MuseScore_General.sf2#128:32";
    perform(
        &mut st,
        DeckAction::SetKit {
            deck: "A".into(),
            kit: Some(id.into()),
        },
    );
    assert_eq!(row(&st, "kit").kit_pick.as_deref(), Some(id));
    assert!(row(&st, "kit").is_overridden());
    voice(&mut st, "kit", None);
    assert!(st.e.kit("A").is_none() && !row(&st, "kit").is_overridden());
}

// PaletteDeckTests.aReplayPlaysTheSameSound (`d2.roleSounds == d.roleSounds`)
#[test]
fn a_replay_plays_the_same_rows() {
    let mut st = deck("ambient");
    st.e.start_session().unwrap();
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("orchestral".into()),
        },
    );
    voice(&mut st, "harmony", Some("gm:48"));
    let session = st.e.finish_session().unwrap();
    assert_eq!(session.events.len(), 2);
    let mut st2 = deck("ambient");
    for e in &session.events {
        if let SessionEventKind::Action(a) = &e.kind {
            st2.e.perform(a, PerformCtx::default());
        }
    }
    assert_eq!(st2.e.role_sounds("A"), st.e.role_sounds("A"));
}
