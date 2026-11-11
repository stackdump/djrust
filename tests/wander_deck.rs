//! The deck half of Wander that is a function of explicit inputs (`Wander.swift` 766-1041,
//! `djrust::wander::deck`): the voices a deck tells the planner, the context it builds, where
//! a swap starts, and the project edits a sound poke makes (`DeckKits.swift`,
//! `PaletteSound.swift`, `StingerCuration.swift`).
//!
//! Parity form (a) for the small rules (each names the Swift it mirrors), and the Swift engine's
//! own output for the whole of it on a live deck: `djhost/tests/wanderdeck_golden.rs`.

use std::collections::BTreeSet;

use djrust::deck::perform::ROLE_ORDER;
use djrust::deck::slots::SwapStart;
use djrust::pflow::Project;
use djrust::sound::stinger::{AssignmentKind, StingerContent};
use djrust::sound::{PaletteSet, Sounds};
use djrust::wander::deck::{
    curate_project_stingers, palette_channels, project_kit, project_palette, project_role_channels,
    resolved_project_stingers, set_project_palette, set_project_role_voice, stinger_labels,
    wander_context, wander_key, wander_swap_start, wander_voices, WanderDeckInputs,
};
use djrust::wander::{Wander, WanderState};
use djrust::TrackSpec;

fn roles_of(p: &Project) -> Vec<String> {
    ROLE_ORDER
        .iter()
        .filter(|r| {
            p.nets
                .iter()
                .any(|(id, nb)| nb.role != "control" && (nb.riff_group == **r || id == **r))
        })
        .map(|r| r.to_string())
        .collect()
}

// Deck.wanderSwapStart: the next phrase's first bar, a loop starts at its top
#[test]
fn a_swap_starts_at_the_next_phrase_or_at_the_top_of_a_loop() {
    assert_eq!(wander_swap_start(false, 3), SwapStart::Tick(64));
    assert_eq!(wander_swap_start(false, 7), SwapStart::Tick(128));
    assert_eq!(wander_swap_start(true, 3), SwapStart::Top);
}

// Deck.wanderKey: the genre's scale and the played project's root
#[test]
fn the_key_is_the_genres_scale_at_the_played_root() {
    let spec = TrackSpec::new("house", 5.0, Some("minimal"));
    let p = spec.project();
    let k = wander_key("house", Some(&p)).expect("a key");
    assert_eq!(k.scale, "MinorScale");
    assert_eq!(k.offset, 0);
    // a pass that moves keys moves the root: the offset is from the genre's root, folded to
    // -5...6
    let mut moved = p.clone();
    moved.root_note = Some(p.root_note.unwrap() + 9);
    let k = wander_key("house", Some(&moved)).unwrap();
    assert_eq!(k.offset, -3);
    assert_eq!(wander_key("not-a-genre", Some(&p)), None);
}

// Deck.wanderVoices: the channels with beats' instrument lists, roles on one channel merged
#[test]
fn voices_merge_roles_that_share_a_channel_and_list_the_genres_instruments_first() {
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let p = spec.project();
    let roles = roles_of(&p);
    assert!(roles.len() >= 6, "{roles:?}");
    let voices = wander_voices(&p, &roles, "techno");
    // one voice per channel, every role in exactly one
    let channels: BTreeSet<i64> = voices.iter().map(|v| v.channel).collect();
    assert_eq!(channels.len(), voices.len());
    let mut seen: Vec<&String> = voices.iter().flat_map(|v| &v.roles).collect();
    seen.sort();
    let mut want: Vec<&String> = roles.iter().collect();
    want.sort();
    assert_eq!(seen, want);
    // in role order: the first voice is the first role's
    assert_eq!(voices[0].roles[0], roles[0]);
    // the options: the genre's list for the role first, then the other genres' without repeats
    let sets = &djrust::gen::tables::BeatsTables::shared().instrument_sets;
    for v in &voices {
        let role = &v.roles[0];
        let own = sets["techno"].get(role).cloned().unwrap_or_default();
        assert_eq!(&v.options[..own.len()], &own[..], "{role}");
        let unique: BTreeSet<&String> = v.options.iter().collect();
        assert_eq!(unique.len(), v.options.len(), "{role}: no repeats");
        assert!(!v.overridden && v.option.is_none());
    }
    // no roles, no voices; a genre the tables do not know lists the other genres' only
    assert!(wander_voices(&p, &[], "techno").is_empty());
    let any = wander_voices(&p, &roles, "");
    assert_eq!(any.len(), voices.len());
}

fn inputs<'a>(
    spec: Option<&'a TrackSpec>,
    raw_cid: Option<String>,
    p: &'a Project,
    wander: &'a WanderState,
) -> WanderDeckInputs<'a> {
    WanderDeckInputs {
        deck: "A",
        spec,
        raw_cid,
        is_loop: false,
        roles: roles_of(p),
        roles_on: roles_of(p).into_iter().collect(),
        disabled_roles: BTreeSet::new(),
        stems_muted: BTreeSet::new(),
        locked_actions: BTreeSet::new(),
        wander,
        project: p,
        layers: Vec::new(),
        layer_pool: Vec::new(),
        stingers: Vec::new(),
        palettes: None,
    }
}

// Deck.wanderContext: nothing without a spec or a raw share; a raw share walks from its CID
#[test]
fn a_context_needs_a_spec_or_a_raw_share() {
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let p = spec.project();
    let wander = WanderState::default();
    assert!(wander_context(inputs(None, None, &p, &wander)).is_none());
    let c = wander_context(inputs(Some(&spec), None, &p, &wander)).unwrap();
    assert!(!c.is_raw());
    assert_eq!(c.spec, spec);
    assert_eq!(c.pace, WanderState::DEFAULT_PACE);
    assert_eq!(c.channels, djrust::wander::carry::project_channels(&p));
    let raw = wander_context(inputs(None, Some("zQmRawShare".into()), &p, &wander)).unwrap();
    assert!(raw.is_raw());
    assert_eq!(raw.spec, TrackSpec::new("", 0.0, None));
    assert_eq!(
        raw.raw_base,
        Some(djrust::wander::WanderRandom::base_of_cid("zQmRawShare"))
    );
    // a spec wins over a raw share
    assert!(
        !wander_context(inputs(Some(&spec), Some("z".into()), &p, &wander))
            .unwrap()
            .is_raw()
    );
    // the planner can plan from either
    assert!(Wander::plan(&c, 0).is_some() && Wander::plan(&raw, 0).is_some());
}

// StingerLaunch.Deck.stingers labels: a Hit that fell back is "stinger"
#[test]
fn a_hit_that_fell_back_is_labelled_stinger() {
    let hits = vec![djrust::pflow::StingerAssignment::fallback(0), {
        let mut a = djrust::pflow::StingerAssignment::fallback(1);
        a.kind = AssignmentKind::OneShot;
        a.label = "Airhorn".into();
        a
    }];
    assert_eq!(stinger_labels(&hits), ["stinger", "Airhorn"]);
}

// DeckKits.Project.palette / setPalette / drumKit, StingerCuration.curateStingers
#[test]
fn a_palette_goes_on_every_net_and_the_hits_are_resolved_for_it() {
    let spec = TrackSpec::new("house", 11.0, Some("standard"));
    let mut p = spec.project();
    assert_eq!(project_palette(&p), None);
    assert_eq!(project_kit(&p), None);
    set_project_palette(&mut p, Some("club"));
    assert_eq!(project_palette(&p).as_deref(), Some("club"));
    assert!(p
        .nets
        .iter()
        .all(|(_, nb)| nb.track.palette.as_deref() == Some("club")));
    // the channels a palette is written to include every role channel of the genre
    let channels = palette_channels(&p, Some("house"));
    for c in djrust::sound::sound_channels("house") {
        assert!(channels.contains(&c), "channel {c}");
    }
    assert!(
        channels.windows(2).all(|w| w[0] < w[1]),
        "sorted: {channels:?}"
    );

    let set = PaletteSet::shared();
    let ctx = Sounds::nothing();
    let content = StingerContent::here(set, ctx, Vec::new());
    let hits = resolved_project_stingers(&p, set, "house", 11.0, &content);
    assert_eq!(hits.len(), 4);
    curate_project_stingers(&mut p, hits.clone(), 11.0, set, ctx);
    assert_eq!(p.stingers, hits);
    // a one-shot's option is its net's curated sound
    for a in hits.iter().filter(|a| a.kind == AssignmentKind::OneShot) {
        let nb = p.nets.get(&a.net_id()).expect("the Hit's net");
        assert_eq!(nb.track.sound.curated, a.option);
    }
    // curating none puts the composer's Hits back
    curate_project_stingers(&mut p, Vec::new(), 11.0, set, ctx);
    assert!(p.stingers.is_empty());
}

// PaletteSound.Deck.setRoleVoice, the project half
#[test]
fn a_role_voice_goes_on_the_roles_channels_and_clearing_it_gives_the_palettes_pick_back() {
    let spec = TrackSpec::new("house", 11.0, Some("standard"));
    let mut p = spec.project();
    set_project_palette(&mut p, Some("synthwave"));
    let set = PaletteSet::shared();
    let ctx = Sounds::nothing();
    djrust::sound::apply_project_sound(&mut p, 11.0, set, ctx);
    let channels = project_role_channels(&p, "bass");
    assert!(!channels.is_empty());
    let got = set_project_role_voice(&mut p, "bass", Some("synth:acid"), 11.0, set, ctx);
    assert_eq!(got.as_deref(), Some(&channels[..]));
    let bass = djrust::sound::project_sound_net(&p, "bass").unwrap();
    assert_eq!(bass.track.sound.override_.as_deref(), Some("synth:acid"));
    // the same voice again changes nothing
    assert_eq!(
        set_project_role_voice(&mut p, "bass", Some("synth:acid"), 11.0, set, ctx),
        None
    );
    // a role the project does not play
    assert_eq!(
        set_project_role_voice(&mut p, "no-such-role", Some("x"), 11.0, set, ctx),
        None
    );
    // back to the palette's pick
    assert!(set_project_role_voice(&mut p, "bass", None, 11.0, set, ctx).is_some());
    let bass = djrust::sound::project_sound_net(&p, "bass").unwrap();
    assert_eq!(bass.track.sound.override_, None);
    assert!(!bass.track.picked);
}
