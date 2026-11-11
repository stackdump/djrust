//! A composed deck plays the musicality pass (`PreparedTrack.played`), so each seed of a genre
//! has its own key, chords, bass and drums, not only its own melody.

use djrust::musicality::{self, Fixes};
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, PreparedTrack};

#[test]
fn a_composed_deck_plays_the_musicality_pass() {
    let (mut e, _r) = engine(EngineConfig::new(48_000.0));
    let spec = TrackSpec::new("techno", 42.0, None);
    assert!(e.generate("A", &spec));
    let played = e
        .deck("A")
        .unwrap()
        .project
        .as_ref()
        .unwrap()
        .canonical_json();
    let composed = spec.project();
    assert_eq!(
        played,
        musicality::apply(&composed, &spec, Fixes::ALL).canonical_json()
    );
    assert_ne!(played, composed.canonical_json());
}

#[test]
fn the_parity_engine_plays_compose_as_it_is() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, _r) = engine(c);
    let spec = TrackSpec::new("techno", 42.0, None);
    assert!(e.generate("A", &spec));
    let played = e
        .deck("A")
        .unwrap()
        .project
        .as_ref()
        .unwrap()
        .canonical_json();
    assert_eq!(played, spec.project().canonical_json());
}

/// `Deck.setRoleDisabled`: a locked-out role is muted now, stays out of every new compose,
/// and a toggle (a pad, a click) cannot bring it back; enabling it lets it play again.
#[test]
fn a_disabled_role_stays_out_of_new_tracks() {
    let (mut e, mut r) = engine(EngineConfig::new(48_000.0));
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let mut block = |r: &mut djrust::host::deck::Renderer| r.render_block(&mut l, &mut rr);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    block(&mut r);
    assert!(e.is_role_audible("A", "kick"));
    e.set_role_disabled("A", "kick", true);
    block(&mut r);
    assert!(e.is_role_disabled("A", "kick") && !e.is_role_audible("A", "kick"));
    assert!(!e.toggle_role("A", "kick"));
    // the next seed is compiled without it
    assert!(e.generate("A", &TrackSpec::new("techno", 43.0, None)));
    block(&mut r);
    e.enforce_disabled_roles();
    block(&mut r);
    assert!(!e.is_role_audible("A", "kick"));
    e.set_role_disabled("A", "kick", false);
    block(&mut r);
    assert!(!e.is_role_disabled("A", "kick") && e.is_role_audible("A", "kick"));
}

/// `djctl render --solo bass`: the deck's role filter rides in its options, so the nets the stem
/// filter silences lose their notes in the graph the runner compiles (`WaveRunner.prepare`
/// applies `opts.roleFilter` to the compiled graph only) and the rest are the composed
/// track's. The track itself (`PreparedTrack.played`, `Deck.netProject`) stays whole, and what
/// is measured on it (the loudness, the trims, the energy, the rests) is measured on the whole.
#[test]
fn a_stem_keeps_only_the_roles_it_is_asked_for() {
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let solo = djrust::wave::RoleFilter {
        mute: vec![],
        solo: Some(vec!["bass".into()]),
    };
    let (mut e, _r) = engine(EngineConfig::new(48_000.0));
    let preparer = e.preparer("A").unwrap();
    let whole = PreparedTrack::compose(&spec, &preparer, 0);
    let stem = PreparedTrack::compose(
        &spec,
        &preparer.clone().with_role_filter(Some(solo.clone())),
        0,
    );
    // the track is the whole track
    assert_eq!(stem.played.canonical_json(), whole.played.canonical_json());
    assert_eq!(stem.opts.role_filter, Some(solo.clone()));
    // measured on the whole project: the same loudness, trims, energy and rests
    let (a, b) = (stem.prepared.wave(), whole.prepared.wave());
    assert_eq!(a.gain, b.gain);
    assert_eq!(a.balance, b.balance);
    assert_eq!(a.energy, b.energy);
    assert_eq!(a.song_rests, b.song_rests);
    // the graph the runner compiles is the stem
    let mut bass_kept = 0;
    for (net, whole_net) in a.graph.nets.iter().zip(&b.graph.nets) {
        assert_eq!(net.id, whole_net.id);
        let nb = whole.played.nets.get(&net.id).unwrap();
        let kept = net.bundle.bindings.len();
        let group = nb.track.group.as_deref().unwrap_or("");
        if nb.riff_group == "bass" || net.id == "bass" || group == "bass" {
            assert_eq!(kept, nb.bindings.len(), "{}", net.id);
            bass_kept += kept;
        } else if nb.role != "control" {
            assert_eq!(kept, 0, "{} is silenced", net.id);
        }
    }
    assert!(bass_kept > 0, "the bass plays");
    // and the deck's own `generate_filtered` sets the filter on the deck the same way
    assert!(e.generate_filtered("A", &spec, Some(&solo)));
    assert_eq!(e.deck("A").unwrap().wave_options().role_filter, Some(solo));
    assert_eq!(
        e.deck("A")
            .unwrap()
            .project
            .as_ref()
            .unwrap()
            .canonical_json(),
        whole.played.canonical_json(),
        "Deck::project is the whole track"
    );
}
