//! The pure remainder of `PaletteTests`: what needs no deck, no runner and no installed sounds.
//! (`sound.rs` holds the data, resolution and chooser tests; `session.rs` the publish guard.)
//!
//! Each test names its Swift test; the proof is the Swift suite's own literals.
//!
//! Not here, because they run on a `Deck` / `DJEngine` / `PreparedTrack`: PaletteDeckTests
//! (`overridesSurviveRecomputesUntilReset`, `anInstrumentPickIsAnOverrideToo`,
//! `setPaletteKeepsOverridesAndTheDefaultClearsIt`, `theDrumsFollowThePaletteUntilAKitIsPicked`,
//! `aReplayPlaysTheSameSound`: `djhost/tests/deck_actions.rs` and `deck_wander.rs`),
//! PaletteDataTests (`theSynthPaletteIsTheSynthRender`, `everyPaletteSitsAtTheSynthPalettesLevel`
//! render a palette-resolved track: `djhost/tests/deck_acceptance.rs`, `deck_kit_levels.rs`),
//! PaletteChooserModelTests.theFineTuneRowsShowWhatThePalettePicked (`Deck.roleSounds`:
//! `role_sounds.rs`, `djhost/tests/deck_role_sounds.rs` and the Swift golden
//! `djhost/tests/rolesounds_golden.rs`). PaletteWanderTests (3, on `Wander.plan` with a palette
//! hop) are `palette_wander.rs`.

use std::collections::BTreeMap;

use djrust::json::JsonValue;
use djrust::musicality::loudness::IntegratedLoudness;
use djrust::session::publish::{LocalOnly, PublishError, SessionBundle};
use djrust::session::{
    DeckAction, DeckParams, DeckSource, Session, SessionEvent, SessionEventKind,
};
use djrust::spec::TrackSpec;

/// (a) PaletteTests.theNewActionRoundTrips: `DeckAction(json: a.json) == a`.
#[test]
fn the_new_action_round_trips() {
    for a in [
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "melody".into(),
            voice: Some("gm:4".into()),
        },
        DeckAction::SetRoleVoice {
            deck: "B".into(),
            role: "kit".into(),
            voice: Some("synth:drums-808".into()),
        },
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: None,
        },
    ] {
        assert_eq!(DeckAction::from_json(&a.json()), Some(a.clone()), "{a:?}");
    }
    assert!(DeckAction::from_json(
        &DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some("jazz".into())
        }
        .json()
    )
    .is_some());
}

fn local_only() -> LocalOnly {
    let dir = format!("{}/tests/fixtures/beats-btw-dj", env!("CARGO_MANIFEST_DIR"));
    let read = |n: &str| {
        JsonValue::parse(&std::fs::read_to_string(format!("{dir}/{n}")).unwrap()).unwrap()
    };
    LocalOnly::from_json(&read("categories.json"), &read("palettes.json"))
}

/// `PalettePublishTests.session(_:events:)`.
fn session(source: DeckSource, events: Vec<SessionEvent>) -> Session {
    Session::new(
        48_000.0,
        48_000,
        BTreeMap::from([("A".to_string(), source)]),
        BTreeMap::from([("A".to_string(), DeckParams::default())]),
        0.5,
        0.8,
        events,
    )
}

fn poke(a: DeckAction) -> Vec<SessionEvent> {
    vec![SessionEvent {
        frame: 1,
        kind: SessionEventKind::Action(a),
    }]
}

/// `TrackSpec.withPalette(_:)`: `palette` in the `tracks` override of every role's channel of
/// the genre (`TrackSpec.soundChannels`), as the Swift test writes it, through the real
/// `withPalette` port. (Swift also runs the id through `SoundPalette.normalized`, which adds the
/// asset version; `palette_is_local_only` ignores the `@version`, so the verdicts are the same.)
fn with_palette(spec: &TrackSpec, palette: &str) -> TrackSpec {
    let mut d = spec.clone();
    djrust::wander::carry::with_palette(
        &mut d,
        Some(palette),
        &TrackSpec::sound_channels(&spec.genre),
    );
    d
}

/// (a) PalettePublishTests.publishRefusesEveryLocalOnlySource: local-only content (Apple's GM
/// bank, the Minecraft bank) is refused however it gets in - a palette, a role's option, a kit -
/// and anything else publishes. This is the Swift list literally: `session.rs` has the
/// palette / kit-by-SoundFont cases, this adds `dls:` voices and the DLS bank's kit.
#[test]
fn publish_refuses_every_local_only_source() {
    let local = local_only();
    let spec = TrackSpec::new("house", 3.0, Some("minimal"));
    for id in ["jazz", "synthwave", "orchestral", "studio"] {
        let s = session(DeckSource::Net(with_palette(&spec, id)), vec![]);
        assert!(s.local_only_sounds(&local).is_empty(), "{id}");
    }
    assert!(local.palette_is_local_only("retro"));
    assert!(local.palette_is_local_only("minecraft"));
    assert!(!local.palette_is_local_only("jazz"));

    let plain = || DeckSource::Net(spec.clone());
    let refused = [
        session(DeckSource::Net(with_palette(&spec, "retro")), vec![]),
        session(
            plain(),
            poke(DeckAction::SetPalette {
                deck: "A".into(),
                palette: Some("retro".into()),
            }),
        ),
        session(
            plain(),
            poke(DeckAction::SetRoleVoice {
                deck: "A".into(),
                role: "melody".into(),
                voice: Some("dls:0".into()),
            }),
        ),
        session(
            plain(),
            poke(DeckAction::SetKit {
                deck: "A".into(),
                kit: Some("gs_instruments.dls#128:0".into()),
            }),
        ),
    ];
    for s in &refused {
        assert!(!s.local_only_sounds(&local).is_empty());
        // refused with the default settings, and allowed when overridden
        let r = SessionBundle::new(s, None, None, "x", false, false, &local);
        assert!(matches!(r, Err(PublishError::LocalOnlySounds(_))));
        assert!(SessionBundle::new(s, None, None, "x", false, true, &local).is_ok());
    }
    let fine = session(
        plain(),
        poke(DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "melody".into(),
            voice: Some("gm:0".into()),
        }),
    );
    assert!(fine.local_only_sounds(&local).is_empty());
}

/// (a) PaletteDataTests.theLoudnessMeterReadsASineAsBS1770Does: a 997 Hz sine at -20 dB RMS on
/// both channels reads -20 + 3 (two channels) - 0.69 + 0.69 = about -17 LUFS; silence -200.
#[test]
fn the_loudness_meter_reads_a_sine_as_bs1770_does() {
    let sr = 48_000.0;
    // PaletteSupport.sineWave(hz, rmsDB:, seconds:, sampleRate:)
    let a = 10f64.powf(-20.0 / 20.0) * 2f64.sqrt();
    let x: Vec<f32> = (0..(sr * 3.0) as usize)
        .map(|k| (a * (2.0 * std::f64::consts::PI * 997.0 * k as f64 / sr).sin()) as f32)
        .collect();
    let l = IntegratedLoudness::lufs(&x, &x, sr);
    assert!((l - -17.0).abs() < 0.3, "{l}");
    assert_eq!(IntegratedLoudness::lufs(&[0.0], &[0.0], sr), -200.0);
}
