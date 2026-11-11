//! What a new track clears on a deck (`Deck.loadNet`) and what a same-song load keeps: the
//! channels' voice settings (the sound panel: `VoiceTweakDeckTests`, `keepsVoiceSettings`), the
//! deck's layers (`resetLayers`, `WanderLayerDeckTests.aRawTracksLayersAndKeyComeFromItsPayload`)
//! and the STEMS mutes (`stemsMuted = [:]`). Form (a): each test names the Swift test it
//! asserts. The rendering side of a voice setting (the lane's coefficients) is `wave_kits.rs` and
//! `voice_tweak.rs`'s.

use djrust::deck::voices::{keeps_voice_settings, RoleVoice};
use djrust::json::JsonValue;
use djrust::session::model::DeckSource;
use djrust::voices::VoiceCatalog;
use djrust::wave::VoiceTweak;
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, PreparedTrack, Renderer, TrackSource};

const RATE: f64 = 22_050.0;

/// `VoiceTweakTests.shaped`.
fn shaped() -> VoiceTweak {
    VoiceTweak {
        gain_db: Some(9.0),
        attack: Some(0.04),
        decay_scale: Some(2.0),
        max_decay: Some(3.0),
        lowpass: Some(900.0),
        distortion: Some(0.4),
        vibrato_rate: Some(6.0),
        vibrato_depth: Some(0.02),
        pitch_from: Some(3.0),
        pitch_to: Some(1.0),
        pitch_time: Some(0.15),
    }
}

fn song() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("minimal"))
}

/// `VoiceTweakDeckTests.deck()`: a deck with a composed song on it, everything inline.
fn deck() -> (Engine, Renderer) {
    let (mut e, r) = engine(EngineConfig::new(RATE));
    assert!(e.generate("A", &song()));
    (e, r)
}

fn step(r: &mut Renderer, blocks: usize) {
    let (mut l, mut rr) = (vec![0f32; 256], vec![0f32; 256]);
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
    }
}

/// `lanes(d.wave, ch)`: every lane of the playing project on `ch`, as its voice state.
fn lanes(r: &Renderer, ch: i64) -> Vec<Vec<f64>> {
    r.deck("A")
        .unwrap()
        .runner
        .project()
        .unwrap()
        .lanes()
        .iter()
        .filter(|l| l.channel == ch)
        .map(|l| l.voice_state())
        .collect()
}

/// The role voices of the song on the deck (`Deck.roleVoices`).
fn role_voices(e: &Engine) -> Vec<RoleVoice> {
    let d = e.deck("A").unwrap();
    let roles: Vec<String> = ["bass", "melody"].iter().map(|r| r.to_string()).collect();
    RoleVoice::list(
        d.project.as_ref().unwrap(),
        &roles,
        &d.spec.as_ref().unwrap().genre,
        &VoiceCatalog::bundled(),
    )
}

fn voice_of(voices: &[RoleVoice], role: &str) -> RoleVoice {
    voices
        .iter()
        .find(|v| v.role.contains(role))
        .unwrap_or_else(|| panic!("a {role} voice"))
        .clone()
}

fn track_of(source: &DeckSource, e: &Engine) -> PreparedTrack {
    let source = match source {
        DeckSource::Net(spec) => TrackSource::Net(spec.clone()),
        DeckSource::RawNets(p) => TrackSource::RawNets(p.clone()),
        other => panic!("{other:?}"),
    };
    PreparedTrack::from_source(&source, &e.preparer("A").unwrap(), 0).expect("a track")
}

// VoiceTweakDeckTests.tweaksAreDeckActionsAndSurviveARecompileOfTheSong
#[test]
fn tweaks_are_deck_calls_and_survive_a_recompile_of_the_song() {
    let (mut e, mut r) = deck();
    let melody = voice_of(&role_voices(&e), "melody");
    let ch = melody.channel;
    assert!(e.set_voice_tweak("A", &shaped(), ch));
    assert_eq!(e.voice_tweak("A", ch), shaped());
    assert_eq!(
        e.voice_setting("A", ch).unwrap().instrument,
        melody.instrument
    );
    step(&mut r, 1);
    let tweaked = lanes(&r, ch);
    let own = {
        // another deck with the same song and no tweak: the instrument's own voice differs
        let (_e2, mut r2) = deck();
        step(&mut r2, 1);
        lanes(&r2, ch)
    };
    assert_ne!(tweaked, own, "the tweak changed the lanes");
    let gen = e
        .deck("A")
        .unwrap()
        .track
        .latest
        .as_ref()
        .unwrap()
        .generation;
    // a section jump / relocation prepares the song again: the new project carries the tweak
    assert!(e.relocate("A", 64, None, None));
    assert_ne!(
        e.deck("A")
            .unwrap()
            .track
            .latest
            .as_ref()
            .unwrap()
            .generation,
        gen,
        "a new project"
    );
    step(&mut r, 1);
    assert_eq!(lanes(&r, ch), tweaked);
    assert_eq!(e.voice_tweak("A", ch), shaped());
    assert!(e.set_synth_voice("A", true, ch));
    assert!(e.uses_synth_voice("A", ch) && e.voice_tweak("A", ch) == shaped());
    assert!(e.set_synth_voice("A", false, ch));
    // Reset
    assert!(e.set_voice_tweak("A", &VoiceTweak::default(), ch));
    assert!(e.voice_setting("A", ch).is_none());
    // no net on a channel nobody plays: nothing to set
    assert!(!e.set_voice_tweak("A", &shaped(), 99));
    assert!(e.channel_voice("A", 99).is_none());
    // a sample deck has no sound panel
    assert!(e.load_demo("B"));
    assert!(!e.set_voice_tweak("B", &shaped(), ch) && e.channel_voice("B", ch).is_none());
}

// VoiceTweakDeckTests.anotherInstrumentPickKeepsTheOtherChannelsTweaks
#[test]
fn another_instrument_pick_keeps_the_other_channels_tweaks() {
    let (mut e, mut r) = deck();
    let voices = role_voices(&e);
    let melody = voice_of(&voices, "melody");
    let bass = voice_of(&voices, "bass");
    let new_bass = bass
        .options
        .iter()
        .find(|o| **o != bass.instrument)
        .expect("another bass")
        .clone();
    assert!(e.set_voice_tweak("A", &shaped(), melody.channel));
    assert!(e.set_voice_tweak(
        "A",
        &VoiceTweak {
            gain_db: Some(-3.0),
            ..Default::default()
        },
        bass.channel
    ));
    step(&mut r, 1);
    let tweaked = lanes(&r, melody.channel);
    let source = e
        .net_source("A")
        .unwrap()
        .with_instrument(&new_bass, bass.channel)
        .unwrap();
    let track = track_of(&source, &e);
    assert!(e.load_net("A", track));
    step(&mut r, 1);
    assert_eq!(
        e.voice_tweak("A", melody.channel),
        shaped(),
        "the song's other tweaks stay"
    );
    assert_eq!(lanes(&r, melody.channel), tweaked);
    assert!(
        e.voice_setting("A", bass.channel).is_none(),
        "a new instrument starts from its own voice"
    );
}

// VoiceTweakDeckTests.aFavoriteReloadOrAnotherSongStartsFromTheInstrumentsOwnVoice
#[test]
fn a_favorite_reload_or_another_song_starts_from_the_instruments_own_voice() {
    let (mut e, mut r) = deck();
    let ch = voice_of(&role_voices(&e), "melody").channel;
    step(&mut r, 1);
    let own = lanes(&r, ch);
    assert!(e.set_voice_tweak("A", &shaped(), ch));
    // the same song loaded again (a favorite of it, its share pasted)
    let again = track_of(&e.net_source("A").unwrap(), &e);
    assert!(e.load_net("A", again));
    assert!(e.voice_setting("A", ch).is_none());
    step(&mut r, 1);
    assert_eq!(lanes(&r, ch), own);
    // another song (Generate, auto-next's next version)
    assert!(e.set_voice_tweak("A", &shaped(), ch));
    assert!(e.generate("A", &song().next_version()));
    assert!(e.deck("A").unwrap().voice_settings.is_empty());
    // a hand-built net is another song too
    assert!(e.set_voice_tweak("A", &shaped(), ch));
    let project = song().project();
    assert!(e.load("A", &project, 42.0));
    assert!(e.deck("A").unwrap().voice_settings.is_empty());
}

/// `Deck.keepsVoiceSettings(loading:)`: only the same song (but for its `tracks` override) keeps
/// them; the same source, another song, a sample or nothing playing does not.
#[test]
fn only_the_same_song_with_another_pick_keeps_the_voice_settings() {
    let spec = song();
    let now = DeckSource::Net(spec.clone());
    // the same source again: not kept
    assert!(!keeps_voice_settings(
        Some(&now),
        &DeckSource::Net(spec.clone())
    ));
    // the same song with an instrument picked on a channel: kept
    let picked = now.with_instrument("synth-bass", 6).unwrap();
    assert!(keeps_voice_settings(Some(&now), &picked));
    // and one pick to another
    let other = picked.with_instrument("electric-bass", 6).unwrap();
    assert!(keeps_voice_settings(Some(&picked), &other));
    // another seed, another genre: not the song
    assert!(!keeps_voice_settings(
        Some(&now),
        &DeckSource::Net(spec.next_version())
    ));
    assert!(!keeps_voice_settings(
        Some(&now),
        &DeckSource::Net(TrackSpec::new("house", 42.0, Some("minimal")))
    ));
    // nothing playing, or nothing to load: not kept
    assert!(!keeps_voice_settings(None, &picked));
    assert!(!keeps_voice_settings(Some(&now), &DeckSource::None));
    assert!(!keeps_voice_settings(
        Some(&now),
        &DeckSource::Sample {
            id: "x".into(),
            title: "t".into(),
            grid: None
        }
    ));
    // a raw-`nets` share the same way: the payload without its `tracks`
    let raw = JsonValue::parse(r#"{"genre":"techno","seed":1,"nets":{}}"#).unwrap();
    let raw_now = DeckSource::RawNets(raw.clone());
    let with_tracks = raw_now.with_instrument("synth-bass", 6).unwrap();
    assert!(keeps_voice_settings(Some(&raw_now), &with_tracks));
    assert!(!keeps_voice_settings(Some(&raw_now), &raw_now));
    // a net and a raw share are never the same song
    assert!(!keeps_voice_settings(Some(&now), &raw_now));
}

// WanderLayerDeckTests.aRawTracksLayersAndKeyComeFromItsPayload (on the deck), and the clear
// of a composed track after it
#[test]
fn a_new_track_resets_the_decks_layers() {
    let (mut e, _r) = engine(EngineConfig::new(RATE));
    let env = djrust::roll::envelope(
        &JsonValue::parse(
            r#"{"name":"L","tempo":128,"steps":128,"key":"A minor",
                "layers":[{"loop":"/lib/Vox A.caf","name":"Vox A","bar":3,"bars":4,"beats":16,"category":"Vocals"}],
                "parts":{"kick":{"every":16,"until":128,"notes":[[0],[4],[8],[12]]},
                         "bass":{"every":16,"until":128,"notes":[[2,45,2],[6,45,2],[10,45,2],[14,45,2]]}}}"#,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(e.deck("A").unwrap().walk.layer_epoch, 0);
    let track = PreparedTrack::from_raw_nets(&env, &e.preparer("A").unwrap()).expect("nets");
    assert!(e.load_net("A", track));
    let d = e.deck("A").unwrap();
    assert!(d.walk.layer_entries.len() == 1 && d.walk.layer_key_name.as_deref() == Some("A minor"));
    let l = &d.walk.layer_entries[0];
    assert_eq!(
        (
            l.loop_.name.as_str(),
            l.loop_.category.as_str(),
            l.start,
            l.length
        ),
        ("Vox A", "Vocals", 3, 4)
    );
    assert_eq!(d.walk.layer_epoch, 1);
    // a composed track has none, and it is another epoch
    assert!(e.generate("A", &song()));
    let d = e.deck("A").unwrap();
    assert!(d.walk.layer_entries.is_empty() && d.walk.layer_key_name.is_none());
    assert_eq!(d.walk.layer_epoch, 2);
    // a hand-built net does not come through `resetLayers` (Swift's `loadNet(_:project:)`)
    assert!(e.load("A", &song().project(), 1.0));
    assert_eq!(e.deck("A").unwrap().walk.layer_epoch, 2);
}

/// `loadNet`: `stemsMuted = [:]`: the STEMS buttons' mutes are the song's.
#[test]
fn a_new_track_clears_the_stems_mutes() {
    let (mut e, _r) = deck();
    let roles: Vec<String> = ["kick", "bass", "melody"]
        .iter()
        .map(|r| r.to_string())
        .collect();
    let calls = e
        .deck_mut("A")
        .unwrap()
        .walk
        .stems_muted
        .toggle("mic", &roles, &|_| true);
    assert_eq!(calls, vec![("melody".to_string(), false)]);
    assert!(e.deck("A").unwrap().walk.stems_muted.is_muted("mic"));
    // another track of the same song
    let again = track_of(&e.net_source("A").unwrap(), &e);
    assert!(e.load_net("A", again));
    assert!(!e.deck("A").unwrap().walk.stems_muted.is_muted("mic"));
    // and a hand-built net
    e.deck_mut("A")
        .unwrap()
        .walk
        .stems_muted
        .toggle("music", &roles, &|_| true);
    assert!(e.load("A", &song().project(), 1.0));
    assert!(!e.deck("A").unwrap().walk.stems_muted.is_muted("music"));
}
