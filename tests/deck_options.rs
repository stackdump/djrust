//! What a deck prepares a track with (`PreparedTrack.init`, `Deck.waveOptions`,
//! `StingerCurationTests`, `RunnerOnlyFixesTests`): the deck's options hold every fix, the pass
//! runs on a composed track and the stingers are curated on what plays, then `forGenre` and
//! `forPalette`; a raw-`nets` share gets the runner's own fixes; the role filter rides in the
//! deck's options. Form (a): each test names the Swift one (or the source line) it asserts.
//! `options_golden.rs` holds the same to Swift's output.

use djrust::json::JsonValue;
use djrust::musicality::{self, Fixes};
use djrust::sound::stinger::AssignmentKind;
use djrust::sound::{unavailable_reason_option, PaletteSet, Sounds};
use djrust::wave::{Master, RoleFilter, StingerPolicy, WaveOptions};
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, PreparedTrack};

const RATE: f64 = 22_050.0;

fn spec(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, Some("standard"))
}

fn parity() -> EngineConfig {
    let mut c = EngineConfig::new(RATE);
    c.extended = false;
    c
}

/// `Deck.waveOptions` = `.extended(samplers:seed:roleFilter:)`: every fix, the instrument table,
/// the limiting master, the stingers and the brake; what `PreparedTrack` makes of them for a
/// composed track is `extended` + the seed + `forGenre` + `forPalette`.
#[test]
fn a_deck_prepares_a_track_with_every_fix_and_its_genres_settings() {
    let (e, _r) = engine(EngineConfig::new(RATE));
    let preparer = e.preparer("A").unwrap();
    let deck = e.deck("A").unwrap().wave_options();
    assert_eq!(deck.musicality, Fixes::ALL);
    assert_eq!(deck.master, Master::Limit);
    assert_eq!(deck.stingers, Some(StingerPolicy::Here));
    assert!(deck.turntable_brake && deck.instruments.is_some());
    for (genre, seed) in [
        ("techno", 42.0),
        ("jazz", 110.0),
        ("dnb", 5.0),
        ("trance", 9.0),
    ] {
        let track = PreparedTrack::compose(&spec(genre, seed), &preparer, 0);
        let want = WaveOptions::extended_with(None, seed, None)
            .for_genre(genre)
            .for_palette(None, genre);
        let o = &track.opts;
        assert_eq!(o.musicality, Fixes::ALL, "{genre}");
        assert_eq!(track.fixes, Fixes::ALL, "{genre}");
        assert!(track.pass_applied, "{genre}");
        assert_eq!(o.voicing_seed, seed, "{genre}");
        assert_eq!(o.genre.as_deref(), Some(genre));
        assert_eq!(o.master, Master::Limit);
        assert_eq!(o.stingers, want.stingers);
        assert_eq!(o.space, want.space, "{genre}");
        assert_eq!(o.echo, want.echo, "{genre}");
        assert_eq!(o.duck_depth, want.duck_depth, "{genre}");
        assert_eq!(o.feel, want.feel, "{genre}");
        assert_eq!(o.brushes, want.brushes, "{genre}");
        assert_eq!(o.sub_octave, want.sub_octave, "{genre}");
        assert_eq!(o.hf_tilt, want.hf_tilt, "{genre}");
        assert_eq!(o.voice_design, want.voice_design, "{genre}");
        // the pass ran: what plays is not what was composed
        assert_ne!(
            track.played.canonical_json(),
            track.project.canonical_json()
        );
        assert_eq!(
            track.played.nets.keys().to_vec(),
            track.project.nets.keys().to_vec()
        );
        // and what it measured is the project's own (the loudness: `.loudness` is in the options)
        assert!(track.prepared.wave().gain > 0.0 && track.prepared.wave().gain != 1.0);
        assert!(track.prepared.wave().balance.is_some(), "{genre}");
    }
}

/// RunnerOnlyFixesTests: a raw-`nets` share has no pass, plays with the runner's own fixes, and
/// with its genre's settings when the payload names one of beats' genres.
#[test]
fn a_raw_nets_share_plays_with_the_runners_fixes_and_its_genres_settings() {
    let path = format!(
        "{}/tests/fixtures/js/share-urls.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = JsonValue::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let rows = doc.get("rawNets").and_then(JsonValue::as_array).unwrap();
    let (e, _r) = engine(EngineConfig::new(RATE));
    let preparer = e.preparer("A").unwrap();
    for row in rows {
        let payload = row.get("payload").unwrap();
        let Some(track) = PreparedTrack::from_raw_nets(payload, &preparer) else {
            continue;
        };
        assert!(!track.pass_applied);
        assert_eq!(track.fixes, Fixes::ALL & Fixes::RUNNER_ONLY);
        assert_eq!(track.opts.musicality, track.fixes);
        assert_eq!(
            track.played.canonical_json(),
            track.project.canonical_json(),
            "its nets are the sharer's, played as sent"
        );
        // (no pass, no curation of the Hits either: a raw share's `.stingers` is not applied)
        assert!(track.played.stingers.is_empty());
        let genre = payload
            .get("genre")
            .and_then(JsonValue::as_str)
            .filter(|g| {
                djrust::gen::tables::BeatsTables::shared()
                    .genres
                    .contains_key(*g)
            });
        assert_eq!(track.opts.genre.as_deref(), genre);
    }
}

/// StingerCurationTests.composeAndTheCIDAreUnchanged: the curation lives on what the deck plays:
/// the composed project (its canonical text and model CID) is beats', the Hits are on `played`,
/// a curated one-shot is its stinger net's sound; and the parity engine curates nothing.
#[test]
fn the_curation_lives_on_what_plays_and_never_on_the_composition() {
    let (e, _r) = engine(EngineConfig::new(RATE));
    let preparer = e.preparer("A").unwrap();
    let (p, _rp) = engine(parity());
    let bare = p.preparer("A").unwrap();
    for genre in ["techno", "lofi", "jazz"] {
        let s = spec(genre, 5.0);
        let composed = s.project();
        let track = PreparedTrack::compose(&s, &preparer, 0);
        assert_eq!(
            track.project.canonical_json(),
            composed.canonical_json(),
            "{genre}"
        );
        assert_eq!(track.project.model_cid(), composed.model_cid());
        assert!(
            track.project.stingers.is_empty(),
            "the composition carries no curation"
        );
        assert_eq!(track.spec(), Some(&s));
        assert_eq!(
            track.played.stingers.len(),
            4,
            "{genre}: what plays is curated"
        );
        for a in track
            .played
            .stingers
            .iter()
            .filter(|a| a.kind == AssignmentKind::OneShot)
        {
            let nb = track.played.nets.get(&a.net_id()).unwrap();
            assert_eq!(nb.track.sound.curated, a.option);
            assert_eq!(nb.track.sound.option, a.option);
            assert_eq!(
                composed.nets.get(&a.net_id()).unwrap().track.sound.curated,
                None
            );
        }
        // the parity engine: no pass, nothing curated
        let parity = PreparedTrack::compose(&s, &bare, 0);
        assert!(parity.played.stingers.is_empty());
        assert_eq!(parity.played.canonical_json(), composed.canonical_json());
        for id in ["hit1", "hit2", "hit3", "hit4"] {
            assert_eq!(
                parity.played.nets.get(id).unwrap().track.sound.curated,
                None
            );
        }
        assert!(!parity.pass_applied && parity.fixes.is_empty());
    }
}

/// `opts.stingers` is the switch: `.here` curates from what is installed here, `.everything`
/// from the whole table, `.nothing` leaves every Hit to fall back unless a macro fits. With no
/// library installed, what `.here` plays is what plays here (`SoundPalette.unavailableReason`).
#[test]
fn the_stinger_policy_decides_what_the_hits_are_curated_from() {
    let (e, _r) = engine(EngineConfig::new(RATE));
    let base = e.preparer("A").unwrap();
    let set = PaletteSet::shared();
    let hits = |policy: Option<StingerPolicy>| {
        let mut pr = base.clone();
        pr.opts.stingers = policy;
        (0..6)
            .flat_map(|seed| {
                PreparedTrack::compose(&spec("techno", seed as f64), &pr, 0)
                    .played
                    .stingers
            })
            .collect::<Vec<_>>()
    };
    // None: the stinger nets as composed
    assert!(hits(None).is_empty());
    let nothing = hits(Some(StingerPolicy::Nothing));
    assert_eq!(nothing.len(), 24);
    assert!(nothing
        .iter()
        .all(|a| matches!(a.kind, AssignmentKind::Macro | AssignmentKind::Fallback)));
    let here = hits(Some(StingerPolicy::Here));
    let everything = hits(Some(StingerPolicy::Everything));
    assert!(
        here.iter()
            .chain(&everything)
            .any(|a| a.kind == AssignmentKind::OneShot),
        "some Hits are one-shots"
    );
    for a in here.iter().filter(|a| a.kind == AssignmentKind::OneShot) {
        let option = a.option.as_deref().unwrap();
        assert_eq!(
            unavailable_reason_option(set, option, "stinger", Sounds::nothing()),
            None,
            "{option} plays here"
        );
    }
    // (nothing installed here, so every sampled candidate drops out of `.here`)
    assert_ne!(here, everything);
}

/// `Deck.roleFilter` rides in the deck's options: `setRoleDisabled` adds the role to it (and
/// takes it out again, leaving none when it is empty), every compose takes the deck's filter
/// as its own, and `generate_filtered` (an offline stem) sets it.
#[test]
fn the_role_filter_is_in_the_decks_options() {
    let (mut e, _r) = engine(EngineConfig::new(RATE));
    assert_eq!(e.deck("A").unwrap().wave_options().role_filter, None);
    e.set_role_disabled("A", "kick", true);
    e.set_role_disabled("A", "bass", true);
    let f = e
        .deck("A")
        .unwrap()
        .wave_options()
        .role_filter
        .clone()
        .unwrap();
    assert_eq!(f.mute, vec!["kick".to_string(), "bass".to_string()]);
    assert_eq!(f.solo, None);
    // a compose takes it: the kick and the bass nets compile without their notes
    let preparer = e.preparer("A").unwrap();
    assert_eq!(preparer.role_filter, Some(f.clone()));
    let track = PreparedTrack::compose(&spec("techno", 42.0), &preparer, 0);
    assert_eq!(track.opts.role_filter, Some(f));
    for net in &track.prepared.wave().graph.nets {
        let nb = track.played.nets.get(&net.id).unwrap();
        if nb.role != "control" && (net.id == "kick" || nb.riff_group == "bass") {
            assert!(net.bundle.bindings.is_empty(), "{} is disabled", net.id);
        }
    }
    e.set_role_disabled("A", "kick", false);
    e.set_role_disabled("A", "bass", false);
    assert_eq!(e.deck("A").unwrap().wave_options().role_filter, None);
    // an offline stem: the filter is the deck's from then on
    let solo = RoleFilter {
        mute: vec![],
        solo: Some(vec!["bass".into()]),
    };
    assert!(e.generate_filtered("A", &spec("techno", 42.0), Some(&solo)));
    assert_eq!(e.deck("A").unwrap().wave_options().role_filter, Some(solo));
    // the whole track is on the deck, and the filter changes what is compiled only
    let whole = musicality::apply(
        &spec("techno", 42.0).project(),
        &spec("techno", 42.0),
        Fixes::ALL,
    );
    let on_deck = e.deck("A").unwrap().project.clone().unwrap();
    assert_eq!(on_deck.nets.keys().to_vec(), whole.nets.keys().to_vec());
    assert_eq!(on_deck.tempo, whole.tempo);
}

/// `Deck.roleFilter` is one value: what Wander keys a look-ahead on (`Engine::role_filter`) is
/// the deck's options' filter, however it was set (a disabled role, an offline stem). A role
/// disabled before a stem was asked for does not change what the stem is.
#[test]
fn the_planners_role_filter_is_the_decks_own() {
    let (mut e, _r) = engine(EngineConfig::new(RATE));
    assert_eq!(e.role_filter("A"), None);
    e.set_role_disabled("A", "kick", true);
    let want = e.deck("A").unwrap().wave_options().role_filter.clone();
    assert!(want.is_some());
    assert_eq!(e.role_filter("A"), want);
    let solo = RoleFilter {
        mute: vec![],
        solo: Some(vec!["bass".into()]),
    };
    assert!(e.generate_filtered("A", &spec("techno", 42.0), Some(&solo)));
    assert_eq!(e.role_filter("A"), Some(solo));
}
