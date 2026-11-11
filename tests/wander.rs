//! `Wander`'s planner, ported from `WanderTests.swift` (`WanderPlannerTests`,
//! `WanderCodecTests`), `WanderKeepTests.swift` (the planner halves), `WanderRawNetsTests`
//! (the pure planner test), `WanderLayerTests.swift` (`WanderLayerPlannerTests`) and
//! `WanderPaletteLockTests.swift`. The oracle is the Swift tests' own literals (counts, shares,
//! lock behaviour); `tests/wander_golden.rs` holds the Swift-generated goldens (every plan of
//! the contexts in `tools/paritydump`). The deck-level tests (the wheel, boundaries on a
//! running clock, replay, the look-ahead slot) need a live deck and are listed in PORTING.md;
//! the state halves of some are here (`WanderState::advance`, `set_on`, `set_lock_anchored`,
//! `deck_json`).

use std::collections::{BTreeMap, BTreeSet};

use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::musicality::note_log::capture;
use djrust::musicality::{apply, Fixes};
use djrust::sound::{Installed, MinecraftSidecar, PaletteSet, SoundPalette, Sounds};
use djrust::voices::VoiceCatalog;
use djrust::wander::*;
use djrust::TrackSpec;

const ROLES: [&str; 7] = ["kick", "snare", "hihat", "bass", "melody", "harmony", "arp"];

fn voices() -> Vec<WanderVoice> {
    vec![
        WanderVoice::new(
            &["kick", "snare", "hihat"],
            10,
            "drums",
            &["drums", "drums-909"],
        ),
        WanderVoice::new(&["bass"], 2, "sub-bass", &["sub-bass", "acid"]),
        WanderVoice::new(&["melody"], 1, "lead", &["lead", "pluck"]),
        WanderVoice::new(&["arp"], 4, "arp", &["arp", "bell"]),
    ]
}

fn techno() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("standard"))
}

/// A machine with every palette playable, as the Mac the Swift suites run on is.
fn full_machine() -> Installed {
    Installed {
        dir: "/sf".into(),
        files: vec![
            "MuseScore_General.sf2".into(),
            "minecraft-local.sf2".into(),
            "Kits.sf2".into(),
        ],
        gm_bank: true,
        kits: vec![],
        minecraft: Some(MinecraftSidecar {
            asset_index: "32".into(),
            sha1: Default::default(),
            programs: djrust::sound::minecraft::programs()
                .into_iter()
                .map(|(n, _)| n)
                .collect(),
            format: 1,
        }),
        minecraft_assets_missing: None,
    }
}

/// The hops from the palette a deck plays (`SoundPalette.playing(palette)`) on that machine.
fn palettes(palette: Option<&str>) -> Option<WanderPalettes> {
    let inst = full_machine();
    let mut catalog = VoiceCatalog::bundled();
    catalog.sound_fonts = inst.sound_fonts();
    let ctx = Sounds {
        catalog: &catalog,
        installed: &inst,
    };
    WanderPalettes::from_sounds(PaletteSet::shared(), palette, ctx)
}

/// A context like a playing techno song's, for the pure planner (palette hops from Studio,
/// as the Swift suites' planner has them).
fn context(spec: TrackSpec, locks: &[&str]) -> WanderContext {
    let mut c = WanderContext::new("A", spec.clone(), false, &ROLES, &ROLES, locks, voices());
    c.palettes = palettes(None);
    if locks.contains(&"tempo") {
        c.anchor_bpm = Some(Wander::bpm(&spec));
    }
    if locks.contains(&"key") {
        if let Some(g) = BeatsTables::shared().genres.get(&spec.genre) {
            c.key = Some(WanderKey {
                scale: g.scale_name.clone(),
                root_pc: g.root_note % 12,
                offset: 0,
            });
        }
    }
    c
}

/// Walk the planner `steps` moves: role and stem moves change the context's mix, swaps its spec.
fn walk(start: &WanderContext, steps: i64) -> Vec<WanderPlan> {
    let mut c = start.clone();
    let mut out = Vec::new();
    for step in 0..steps {
        let Some(p) = Wander::plan(&c, step) else {
            break;
        };
        match &p.action {
            WanderAction::Role(r, on) => {
                if *on {
                    c.roles_on.insert(r.clone());
                } else {
                    c.roles_on.remove(r);
                }
            }
            WanderAction::Stem(s) => {
                if !c.stems_muted.remove(s) {
                    c.stems_muted.insert(s.clone());
                }
            }
            WanderAction::Swap(spec) => c.spec = spec.clone(),
            _ => {}
        }
        out.push(p);
    }
    out
}

fn tracks_instrument(s: &TrackSpec, ch: &str) -> Option<String> {
    s.layers
        .get("tracks")?
        .get(ch)?
        .get("instrument")?
        .as_str()
        .map(String::from)
}

#[test]
fn the_same_spec_and_steps_give_the_same_moves() {
    let a = walk(&context(techno(), &[]), 200);
    let b = walk(&context(techno(), &[]), 200);
    assert!(a.len() == 200 && a == b);
    // another seed walks elsewhere
    let c = walk(
        &context(TrackSpec::new("techno", 43.0, Some("standard")), &[]),
        200,
    );
    assert_ne!(a, c);
}

#[test]
fn every_kind_turns_up_about_as_often_as_its_weight() {
    let mut counts: BTreeMap<WanderMoveKind, i64> = BTreeMap::new();
    // many independent draws from one configuration (a long walk drifts genres: count from here)
    let c = context(techno(), &[]);
    for step in 0..4000 {
        if let Some(p) = Wander::plan(&c, step) {
            *counts.entry(p.kind).or_default() += 1;
        }
    }
    // (layer moves need a pool and a layer list this context doesn't have)
    for k in WanderMoveKind::ALL {
        if matches!(
            k,
            WanderMoveKind::LayerAdd | WanderMoveKind::LayerSwap | WanderMoveKind::LayerRemove
        ) {
            continue;
        }
        let share = *counts.get(&k).unwrap_or(&0) as f64 / 4000.0;
        assert!(
            (share - k.weight() as f64 / 100.0).abs() < 0.03,
            "{k:?}: {share}"
        );
    }
}

#[test]
fn disabled_kept_and_locked_roles_are_never_touched() {
    let mut c = context(techno(), &[&WanderLock::keep("melody")]);
    c.locked_actions.insert("A.roleMute.kick".into());
    c.disabled_roles.insert("bass".into());
    c.roles_on.remove("bass");
    for p in walk(&c, 400) {
        if let WanderAction::Role(r, _) = &p.action {
            assert!(!["bass", "melody", "kick"].contains(&r.as_str()));
        }
        if let WanderAction::Stem(s) = &p.action {
            assert!(s != "music" && s != "mic", "both hold the melody");
        }
        if p.kind == WanderMoveKind::Instrument {
            assert!(!p.detail.contains("melody"));
        }
        // (a genre hop lands on the new genre's own channels, and a palette hop pins the kept
        // sound as a `voice`, not an `instrument`: this checks the plain carry)
        if let WanderAction::Swap(spec) = &p.action {
            if p.kind != WanderMoveKind::Genre && p.kind != WanderMoveKind::Palette {
                let picks: Vec<String> = spec
                    .layers
                    .get("tracks")
                    .and_then(JsonValue::as_object)
                    .map(|t| {
                        t.iter()
                            .filter_map(|(_, o)| {
                                o.get("instrument")
                                    .and_then(JsonValue::as_str)
                                    .map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(picks.contains(&"lead".to_string()), "{}", p.detail);
            }
        }
    }
}

#[test]
fn locked_buttons_are_skipped() {
    let mut c = context(techno(), &[]);
    for m in Wander::NET_MACROS {
        c.locked_actions.insert(format!("A.macro.{}", m.id()));
    }
    c.locked_actions.insert("A.stinger.0".into());
    c.locked_actions.insert("A.stinger.1".into());
    let plans = walk(&c, 400);
    assert!(
        !plans.iter().any(|p| p.kind == WanderMoveKind::Macro),
        "every net macro is locked"
    );
    for p in &plans {
        if let WanderAction::Stinger(n) = p.action {
            assert!(n >= 2);
        }
    }
}

#[test]
fn a_locked_dimensions_moves_never_occur() {
    use WanderMoveKind as K;
    let cases: [(&str, &[K]); 5] = [
        ("arrangement", &[K::Role, K::Stem]),
        ("fx", &[K::Macro, K::Fx, K::Stinger]),
        ("instruments", &[K::Instrument]),
        ("structure", &[K::Structure]),
        ("genre", &[K::Genre]),
    ];
    for (lock, gone) in cases {
        let plans = walk(&context(techno(), &[lock]), 400);
        assert_eq!(plans.len(), 400);
        assert!(plans.iter().all(|p| !gone.contains(&p.kind)), "{lock}");
    }
}

#[test]
fn with_genre_locked_the_walk_never_changes_genre() {
    let plans = walk(&context(techno(), &["genre"]), 600);
    for p in &plans {
        if let WanderAction::Swap(s) = &p.action {
            assert_eq!(s.genre, "techno");
        }
    }
    assert!(plans.iter().any(|p| p.kind == WanderMoveKind::Seed));
}

#[test]
fn the_tempo_lock_keeps_the_walk_near_its_tempo() {
    let c = context(techno(), &["tempo"]);
    let anchor = Wander::bpm(&c.spec);
    let mut hops = 0;
    for p in walk(&c, 800) {
        let WanderAction::Swap(s) = &p.action else {
            continue;
        };
        assert!(
            (Wander::bpm(s) / anchor - 1.0).abs() <= WanderLock::TEMPO_TOLERANCE,
            "{}",
            p.detail
        );
        if p.kind == WanderMoveKind::Genre {
            hops += 1;
        }
    }
    assert!(hops > 0, "techno has neighbours within 6 %");
    // unlocked, the walk does leave that range (it is the lock doing it)
    let free: Vec<f64> = walk(&context(techno(), &[]), 800)
        .iter()
        .filter_map(|p| match &p.action {
            WanderAction::Swap(s) => Some(Wander::bpm(s)),
            _ => None,
        })
        .collect();
    assert!(free
        .iter()
        .any(|b| (b / anchor - 1.0).abs() > WanderLock::TEMPO_TOLERANCE));
}

#[test]
fn the_key_lock_keeps_the_scale_and_root_and_pins_the_key() {
    let c = context(TrackSpec::new("house", 3.0, Some("standard")), &["key"]);
    let t = BeatsTables::shared();
    for p in walk(&c, 600) {
        let WanderAction::Swap(s) = &p.action else {
            continue;
        };
        let g = &t.genres[&s.genre];
        assert!(
            g.scale_name == "MinorScale" && g.root_note % 12 == 0,
            "{}",
            s.genre
        );
        assert_eq!(
            s.pinned_key_offset(),
            Some(0),
            "the key rides in the spec for a key-moving pass"
        );
    }
}

#[test]
fn the_structure_lock_keeps_the_family() {
    let t = BeatsTables::shared();
    for p in walk(&context(techno(), &["structure"]), 600) {
        let WanderAction::Swap(s) = &p.action else {
            continue;
        };
        assert!(
            s.structure.as_deref() == Some("standard")
                && t.family_of(&s.genre) == t.family_of("techno")
        );
    }
}

#[test]
fn the_instruments_lock_carries_every_instrument() {
    for p in walk(&context(techno(), &["instruments"]), 300) {
        // same-genre swaps only: a genre hop lands on the new genre's own channel numbers
        let WanderAction::Swap(s) = &p.action else {
            continue;
        };
        if s.genre != "techno"
            || p.kind == WanderMoveKind::Genre
            || p.kind == WanderMoveKind::Palette
        {
            continue;
        }
        assert_eq!(
            tracks_instrument(s, "2").as_deref(),
            Some("sub-bass"),
            "{}",
            p.detail
        );
        assert_eq!(tracks_instrument(s, "1").as_deref(), Some("lead"));
    }
}

#[test]
fn with_everything_locked_there_is_no_move() {
    let mut all: BTreeSet<String> = WanderLock::DIMENSIONS
        .iter()
        .map(|s| s.to_string())
        .collect();
    for r in ROLES {
        all.insert(WanderLock::keep(r));
    }
    let lock_refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let c = context(techno(), &lock_refs);
    assert!(Wander::plan(&c, 0).is_none());
    assert!(!Wander::can_move(&c));
    // one dimension free: it moves again
    let mut some = all.clone();
    some.remove("fx");
    let refs: Vec<&str> = some.iter().map(String::as_str).collect();
    assert_eq!(
        Wander::plan(&context(techno(), &refs), 0).map(|p| p.kind.lock()),
        Some(Some("fx"))
    );
}

#[test]
fn neighbours_are_near_in_tempo_and_stable() {
    let n = Wander::neighbours("techno");
    assert!(n.contains(&"house".to_string()) && n.contains(&"garage".to_string()));
    assert!(!n.contains(&"techno".to_string()));
    let mut sorted = n.clone();
    sorted.sort();
    assert_eq!(n, sorted);
    for g in &BeatsTables::shared().genre_ids {
        assert!(!Wander::neighbours(g).is_empty(), "{g}");
    }
}

#[test]
fn the_mix_never_empties() {
    let mut c = context(techno(), &[]);
    c.roles_on = ["kick", "bass"].iter().map(|s| s.to_string()).collect();
    for step in 0..200 {
        if let Some(WanderPlan {
            action: WanderAction::Role(_, on),
            ..
        }) = Wander::plan(&c, step)
        {
            assert!(on, "down to two parts: only bring one back");
        }
    }
}

// MARK: WanderCodecTests

#[test]
fn the_starting_wander_state_round_trips_and_stays_out_of_old_sets() {
    let mut s = WanderState::default();
    s.on = true;
    s.pace = 4;
    s.step = 17;
    s.locks = ["tempo", "key", &WanderLock::keep("bass")]
        .iter()
        .map(|s| s.to_string())
        .collect();
    s.anchor_bpm = Some(128.0);
    s.key = Some(WanderKey {
        scale: "MinorScale".into(),
        root_pc: 9,
        offset: -3,
    });
    s.paused = true;
    s.quiet_bars = 2;
    assert_eq!(WanderState::from_json(&s.json()), s);
    // pace outside the list reads back as the default; unknown locks are dropped
    let v = JsonValue::parse(
        r#"{"on":true,"pace":5,"locks":["genre","nonsense","keep:","keep:bass"]}"#,
    )
    .unwrap();
    let r = WanderState::from_json(&v);
    assert_eq!(r.pace, WanderState::DEFAULT_PACE);
    assert_eq!(
        r.locks.iter().map(String::as_str).collect::<Vec<_>>(),
        ["genre", "keep:bass"]
    );
}

#[test]
fn pace_steps_stop_at_4_and_32() {
    // shiftAndTheWheelSetThePaceAndThePushTogglesIt (the pace half)
    let mut s = WanderState::default();
    assert_eq!(s.pace, 16);
    s.step_pace(1);
    assert_eq!(s.pace, 32);
    s.step_pace(1);
    assert_eq!(s.pace, 32, "stops at 32");
    for _ in 0..4 {
        s.step_pace(-1);
    }
    assert_eq!(s.pace, 4, "and at 4");
    s.set_pace(7);
    assert_eq!(s.pace, 4, "anything else is ignored");
}

#[test]
fn moves_land_on_pace_boundaries() {
    // WanderDeckTests.movesLandOnPaceBoundaries, the state half: the last bar of each 4-bar
    // phrase takes a move, the same bar twice takes nothing
    let mut s = WanderState::default();
    s.set_pace(4);
    s.on = true;
    s.last_bar = Some(-1);
    let mut moves = vec![];
    for bar in 0..13 {
        if let Advance::Move(b) = s.advance(bar, false) {
            moves.push(b + 1);
            s.step += 1;
        }
    }
    assert_eq!(moves, [4, 8, 12]);
    assert_eq!(s.step, 3);
    assert_eq!(s.advance(12, false), Advance::Nothing);
    // pace 8: the next move is in bar 16 (the end of the phrase 8…15)
    s.set_pace(8);
    for bar in 13..16 {
        if let Advance::Move(b) = s.advance(bar, false) {
            moves.push(b + 1);
        }
    }
    assert_eq!(moves, [4, 8, 12, 16]);
}

#[test]
fn a_quiet_phrase_gives_the_walk_back() {
    // theWheelGrabsTheWalkAndAQuietPhraseGivesItBack, the state half
    let mut s = WanderState::default();
    s.set_pace(4);
    s.on = true;
    s.last_bar = Some(0);
    s.grab();
    assert!(s.paused);
    for bar in 1..=4 {
        assert_eq!(s.advance(bar, false), Advance::Nothing);
    }
    assert!(!s.paused, "four quiet bars are a phrase");
    s.grab();
    for bar in 5..=7 {
        s.advance(bar, false);
    }
    assert!(s.paused, "three quiet bars are not a phrase");
    s.advance(8, false);
    assert!(!s.paused);
    assert_eq!(s.advance(11, false), Advance::Move(11));
}

#[test]
fn the_countdown_counts_bars_to_the_phrase_end() {
    let mut s = WanderState::default();
    assert_eq!(s.countdown(0), None);
    s.on = true;
    s.pace = 8;
    assert_eq!(s.countdown(0), Some(7));
    assert_eq!(s.countdown(16 * 6), Some(1));
    assert_eq!(s.countdown(16 * 7), Some(8), "on the move bar: a full pace");
    s.paused = true;
    s.quiet_bars = 3;
    assert_eq!(s.countdown(0), Some(5));
}

// MARK: WanderKeepTests (planner halves)

fn swaps(start: &WanderContext, steps: i64) -> Vec<(WanderPlan, TrackSpec)> {
    let mut c = start.clone();
    let mut out = Vec::new();
    for step in 0..steps {
        let Some(p) = Wander::plan(&c, step) else {
            break;
        };
        if let WanderAction::Swap(s) = &p.action {
            out.push((p.clone(), s.clone()));
            c.spec = s.clone();
        }
    }
    out
}

/// The bass's hit ticks over the first 64 ticks of what `spec` plays (`.approach`, which steps
/// into the next root from the chords, left out).
fn bass_rhythm(spec: &TrackSpec) -> Vec<i64> {
    let p = apply(&spec.project(), spec, Fixes::ALL & !Fixes::APPROACH);
    capture(&p, Some(64))
        .into_iter()
        .filter(|n| !n.is_drum() && n.group == "bass")
        .map(|n| n.tick)
        .collect()
}

fn seeds(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn a_kept_bass_keeps_its_line_through_seed_moves() {
    let c = context(
        TrackSpec::new("techno", 42.0, None),
        &["genre", "structure", &WanderLock::keep("bass")],
    );
    let all = swaps(&c, 200);
    let seeded: Vec<&TrackSpec> = all
        .iter()
        .map(|(_, s)| s)
        .filter(|s| s.seed != 42.0)
        .collect();
    assert!(seeded.len() >= 3);
    for (p, s) in &all {
        assert_eq!(s.part_seeds, seeds(&[("bass", 42.0)]), "{}", p.detail);
    }
    // and the bass keeps its rhythm (the bass's hit ticks over the first 64 ticks)
    // (WanderKeepTests.aKeptBassKeepsItsLineThroughSeedMoves: `rhythms.count == 1`)
    let rhythms: BTreeSet<Vec<i64>> = std::iter::once(&c.spec)
        .chain(seeded.iter().take(4).copied())
        .map(bass_rhythm)
        .collect();
    assert_eq!(rhythms.len(), 1, "{} bass rhythms", rhythms.len());
    assert!(!rhythms.first().unwrap().is_empty());
    // without the keep the same seeds carry no part seeds
    let free = swaps(
        &context(TrackSpec::new("techno", 42.0, None), &["genre"]),
        200,
    );
    assert!(free.iter().all(|(_, s)| s.part_seeds.is_empty()));
}

#[test]
fn a_kept_part_keeps_its_own_seed() {
    let mut spec = techno();
    spec.part_seeds.insert("melody".into(), 777.0);
    let c = context(
        spec,
        &[
            "genre",
            &WanderLock::keep("melody"),
            &WanderLock::keep("harmony"),
        ],
    );
    let all = swaps(&c, 120);
    assert!(!all.is_empty());
    for (p, s) in &all {
        assert_eq!(s.part_seeds, seeds(&[("melody", 777.0)]), "{}", p.detail);
    }
}

#[test]
fn a_genre_hop_drops_the_part_seed() {
    let mut spec = techno();
    spec.part_seeds.insert("bass".into(), 9001.0);
    let c = context(spec, &[&WanderLock::keep("bass")]);
    let step = (0..400)
        .find(|&s| Wander::plan(&c, s).map(|p| p.kind) == Some(WanderMoveKind::Genre))
        .expect("a genre move turns up");
    match Wander::plan(&c, step).unwrap().action {
        WanderAction::Swap(s) => {
            assert_ne!(s.genre, "techno");
            assert!(s.part_seeds.is_empty());
        }
        _ => panic!("a genre move swaps"),
    }
    // and a seed move from the same spec keeps it
    let seed_step = (0..400)
        .find(|&s| Wander::plan(&c, s).map(|p| p.kind) == Some(WanderMoveKind::Seed))
        .unwrap();
    if let Some(WanderAction::Swap(n)) = Wander::plan(&c, seed_step).map(|p| p.action) {
        assert_eq!(n.part_seeds, seeds(&[("bass", 9001.0)]));
    }
}

#[test]
fn freeing_a_keep_drops_its_seed_at_the_next_swap() {
    let mut spec = techno();
    spec.part_seeds.insert("bass".into(), 42.0);
    spec.part_seeds.insert("kick".into(), 5.0);
    let c = context(spec, &["genre", &WanderLock::keep("kick")]);
    let all = swaps(&c, 120);
    assert!(all.iter().any(|(p, _)| p.kind == WanderMoveKind::Structure));
    assert!(all.iter().any(|(p, _)| p.kind == WanderMoveKind::Seed));
    for (p, s) in &all {
        assert_eq!(s.part_seeds, seeds(&[("kick", 5.0)]), "{}", p.detail);
    }
}

#[test]
fn the_walk_with_keeps_is_deterministic() {
    let c = context(
        techno(),
        &[&WanderLock::keep("bass"), &WanderLock::keep("hihat")],
    );
    let a: Vec<_> = swaps(&c, 200).into_iter().map(|x| x.1).collect();
    let b: Vec<_> = swaps(&c, 200).into_iter().map(|x| x.1).collect();
    assert!(!a.is_empty() && a == b);
}

// MARK: WanderRawNetsTests.theWalkIsPureOverThePlannerToo

#[test]
fn a_raw_decks_planner_takes_only_raw_kinds() {
    let mut c = WanderContext::new(
        "A",
        TrackSpec::new("", 0.0, None),
        false,
        &ROLES,
        &ROLES,
        &[],
        vec![],
    );
    c.raw_base = Some(WanderRandom::base_of_cid("z-test-payload"));
    assert!(c.is_raw());
    let mut kinds = BTreeSet::new();
    for step in 0..400 {
        if let Some(p) = Wander::plan(&c, step) {
            kinds.insert(p.kind);
        }
    }
    assert!(kinds.iter().all(|k| Wander::RAW_KINDS.contains(k)));
    assert!(kinds.contains(&WanderMoveKind::Role));
    assert!(!kinds.contains(&WanderMoveKind::Seed));
    assert!(!kinds.contains(&WanderMoveKind::Genre));
    assert!(!kinds.contains(&WanderMoveKind::Structure));
    // the same payload walks the same path, another does not
    let d = c.clone();
    let mut e = c.clone();
    e.raw_base = Some(WanderRandom::base_of_cid("z-other-payload"));
    let path = |c: &WanderContext| -> Vec<String> {
        (0..40)
            .filter_map(|s| Wander::plan(c, s).map(|p| p.detail))
            .collect()
    };
    assert_eq!(path(&c), path(&d));
    assert_ne!(path(&c), path(&e));
}

#[test]
fn a_raw_deck_with_everything_locked_has_no_move() {
    let mut c = WanderContext::new(
        "A",
        TrackSpec::new("", 0.0, None),
        false,
        &["kick", "bass", "melody"],
        &["kick", "bass", "melody"],
        &["arrangement", "fx", "palette"],
        vec![],
    );
    c.raw_base = Some(1);
    assert!(!Wander::can_move(&c));
    c.locks.remove("fx");
    assert!(Wander::can_move(&c));
    for step in 0..200 {
        let p = Wander::plan(&c, step).unwrap();
        assert!(matches!(
            p.kind,
            WanderMoveKind::Macro | WanderMoveKind::Fx | WanderMoveKind::Stinger
        ));
    }
}

// MARK: WanderLayerTests.WanderLayerPlannerTests

fn lp(n: &str, cat: &str, beats: i64) -> LayerLoop {
    LayerLoop::new(&format!("/lib/{n}.caf"), n, cat, beats)
}

/// `LayerEntry(loop:start:length:)`: gain -3 dB, key lock on (the memberwise defaults).
fn entry(l: LayerLoop, start: i64, length: i64) -> LayerEntry {
    LayerEntry::new(l, start, length, -3.0, true)
}

fn pool() -> Vec<LayerLoop> {
    vec![
        lp("Vox A", "Vocals", 16),
        lp("Vox B", "Vocals", 16),
        lp("Vox C", "Vocals", 8),
        lp("Riser A", "Sound Effect", 16),
        lp("Riser B", "Sound Effect", 16),
        lp("Pad A", "Texture/Atmosphere", 32),
    ]
}

fn lctx(layers: Vec<LayerEntry>, pool: Vec<LayerLoop>, locks: &[&str], bar: i64) -> WanderContext {
    let roles = ["kick", "bass", "melody"];
    let mut c = WanderContext::new(
        "A",
        TrackSpec::new("", 0.0, None),
        false,
        &roles,
        &roles,
        locks,
        vec![],
    );
    c.raw_base = Some(WanderRandom::base_of_cid("z-test-payload"));
    c.layers = layers;
    c.layer_pool = pool;
    c.bar = bar;
    c.pace = 8;
    c
}

fn plan_kind(kind: WanderMoveKind, c: &WanderContext) -> Option<WanderPlan> {
    (0..2000).find_map(|s| Wander::plan(c, s).filter(|p| p.kind == kind))
}

fn list(p: Option<WanderPlan>) -> Option<Vec<LayerEntry>> {
    match p?.action {
        WanderAction::Layers(l) => Some(l),
        _ => None,
    }
}

#[test]
fn an_added_layer_comes_in_from_the_next_bar_for_one_pass() {
    let pool = pool();
    let c = lctx(vec![], pool.clone(), &[], 8);
    let l = list(plan_kind(WanderMoveKind::LayerAdd, &c)).unwrap();
    assert_eq!(l.len(), 1);
    let e = &l[0];
    assert!(
        e.start == 9 && e.length == 16.min(8 + e.loop_.bars()),
        "a little over one phrase (pace 8)"
    );
    assert!(pool.contains(&e.loop_) && e.gain_db == default_gain(&e.loop_.category));
    assert!(plan_kind(WanderMoveKind::LayerAdd, &c)
        .unwrap()
        .detail
        .starts_with(&format!("+ {} · bars 9–", e.loop_.name)));
}

#[test]
fn no_more_than_three_at_once_and_no_loop_twice() {
    let pool = pool();
    let on: Vec<LayerEntry> = [&pool[0], &pool[1], &pool[3]]
        .iter()
        .enumerate()
        .map(|(i, l)| entry((*l).clone(), 5 + i as i64, 8))
        .collect();
    assert!(
        plan_kind(WanderMoveKind::LayerAdd, &lctx(on, pool.clone(), &[], 8)).is_none(),
        "three are live: no fourth"
    );
    // a window already over doesn't count
    let over = vec![entry(pool[0].clone(), 1, 4)];
    assert!(plan_kind(WanderMoveKind::LayerAdd, &lctx(over, pool.clone(), &[], 8)).is_some());
    // never one that is already live
    let live = entry(pool[0].clone(), 5, 8);
    let c = lctx(vec![live], pool.clone(), &[], 8);
    for step in 0..300 {
        if let Some(WanderAction::Layers(l)) = Wander::plan(&c, step).map(|p| p.action) {
            if l.len() == 2 {
                assert_eq!(l.iter().filter(|e| e.loop_ == pool[0]).count(), 1);
            }
        }
    }
}

#[test]
fn a_layer_that_is_on_goes_out_at_the_next_bar_and_one_not_yet_on_never_comes() {
    let pool = pool();
    let on = entry(pool[0].clone(), 5, 8); // bars 5–12, on now (bar 8)
    let l = list(plan_kind(
        WanderMoveKind::LayerRemove,
        &lctx(vec![on], pool.clone(), &[], 8),
    ))
    .unwrap();
    assert!(
        l.len() == 1 && l[0].start == 5 && l[0].end() == 9,
        "out after bar 8"
    );
    let later = entry(pool[1].clone(), 14, 4);
    let gone = list(plan_kind(
        WanderMoveKind::LayerRemove,
        &lctx(vec![later], pool.clone(), &[], 8),
    ))
    .unwrap();
    assert!(gone.is_empty());
    assert!(
        plan_kind(
            WanderMoveKind::LayerRemove,
            &lctx(vec![], pool.clone(), &[], 8)
        )
        .is_none(),
        "nothing to take out"
    );
    assert!(
        plan_kind(
            WanderMoveKind::LayerRemove,
            &lctx(vec![entry(pool[0].clone(), 1, 4)], pool.clone(), &[], 8)
        )
        .is_none(),
        "already over"
    );
}

#[test]
fn a_swap_ends_the_old_loop_and_brings_another_of_its_kind_in() {
    let pool = pool();
    let mut on = entry(pool[0].clone(), 5, 8);
    on.gain_db = -7.0;
    on.key_lock = false; // a vocal, on now
    let l = list(plan_kind(
        WanderMoveKind::LayerSwap,
        &lctx(vec![on.clone()], pool.clone(), &[], 8),
    ))
    .unwrap();
    assert_eq!(l.len(), 2);
    let old = l.iter().find(|e| e.loop_ == on.loop_).unwrap();
    let new = l.iter().find(|e| e.loop_ != on.loop_).unwrap();
    assert_eq!(old.end(), 9, "the old one is out at the next bar");
    assert!(
        new.start == 9 && new.loop_.category == "Vocals" && new.gain_db == -7.0 && !new.key_lock
    );
    assert_eq!(new.end(), 13, "for what was left of the old one's window");
    // one not yet on is replaced in place
    let later = entry(pool[3].clone(), 14, 4);
    let r = list(plan_kind(
        WanderMoveKind::LayerSwap,
        &lctx(vec![later.clone()], pool.clone(), &[], 8),
    ))
    .unwrap();
    assert!(
        r.len() == 1
            && r[0].start == 14
            && r[0].loop_ != later.loop_
            && r[0].loop_.category == "Sound Effect"
    );
    assert!(plan_kind(
        WanderMoveKind::LayerSwap,
        &lctx(vec![], pool.clone(), &[], 8)
    )
    .is_none());
}

#[test]
fn with_a_layer_on_the_walk_soon_takes_it_out_or_swaps_it() {
    let pool = pool();
    let on = vec![entry(pool[0].clone(), 5, 8)];
    let share = |kind: WanderMoveKind, c: &WanderContext| -> f64 {
        let n = (0..3000)
            .filter(|&s| Wander::plan(c, s).map(|p| p.kind) == Some(kind))
            .count();
        n as f64 / 3000.0
    };
    let live = lctx(on, pool.clone(), &[], 8);
    let none = lctx(vec![], pool.clone(), &[], 8);
    assert!(
        share(WanderMoveKind::LayerRemove, &live) > 0.10
            && share(WanderMoveKind::LayerSwap, &live) > 0.05,
        "out and swap come up"
    );
    assert!(
        share(WanderMoveKind::LayerAdd, &none) > share(WanderMoveKind::LayerAdd, &live),
        "one is likelier to come in when none is on"
    );
}

#[test]
fn without_a_pool_or_with_the_layers_lock_there_are_no_layer_moves() {
    let pool = pool();
    let on = vec![entry(pool[0].clone(), 5, 8)];
    for kind in [WanderMoveKind::LayerAdd, WanderMoveKind::LayerSwap] {
        assert!(plan_kind(kind, &lctx(on.clone(), vec![], &[], 8)).is_none());
    }
    assert!(
        plan_kind(
            WanderMoveKind::LayerRemove,
            &lctx(on.clone(), vec![], &[], 8)
        )
        .is_some(),
        "taking one out needs no pool"
    );
    let locked = lctx(on, pool, &["layers"], 8);
    for kind in [
        WanderMoveKind::LayerAdd,
        WanderMoveKind::LayerSwap,
        WanderMoveKind::LayerRemove,
    ] {
        assert!(plan_kind(kind, &locked).is_none());
    }
    assert!(WanderLock::is_valid("layers") && WanderLock::DIMENSIONS.contains(&"layers"));
}

#[test]
fn a_layer_walk_keeps_the_list_sane_and_is_the_same_every_time() {
    let pool = pool();
    let run = || -> Vec<Vec<LayerEntry>> {
        let mut c = lctx(vec![], pool.clone(), &[], 8);
        let mut lists = vec![];
        for step in 0..120 {
            c.bar = 8 + step * 4;
            let Some(p) = Wander::plan(&c, step) else {
                continue;
            };
            if let WanderAction::Layers(l) = p.action {
                let live = l.iter().filter(|e| e.end() > c.bar + 1).count();
                assert!(live <= Wander::MAX_LAYERS, "{live} live at bar {}", c.bar);
                assert!(l.iter().all(|e| e.length >= 1));
                c.layers = l.clone();
                lists.push(l);
            }
        }
        lists
    };
    let a = run();
    assert!(!a.is_empty());
    assert_eq!(a, run());
}

#[test]
fn seed_text_prints_whole_seeds_without_a_fraction() {
    assert_eq!(Wander::seed_text(41.0), "41");
    assert_eq!(Wander::seed_text(41.5), "41.5");
}

#[test]
fn the_rng_matches_splitmix64() {
    // SplitMix64 from state 0: the published first outputs of the reference implementation
    let mut r = WanderRandom::with_base(0, 0);
    assert_eq!(r.next(), 0xE220_A839_7B1D_CDAF);
    assert_eq!(r.next(), 0x6E78_9E6A_A1B9_65F4);
    assert_eq!(r.next(), 0x06C4_5D18_8009_454F);
    // FNV-1a of "" is the offset basis; of "a" the published 64-bit value
    assert_eq!(WanderRandom::base_of_cid(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(WanderRandom::base_of_cid("a"), 0xaf63_dc4c_8601_ec8c);
    let mut r = WanderRandom::with_base(7, 3);
    assert_eq!(r.int(1), 0);
    assert_eq!(r.int(0), 0);
}

// MARK: WanderPaletteLockTests

/// The specs a walk swaps to under a deck playing `minecraft@32` with a MuseScore kit
/// (`WanderPaletteLockTests.walkSwaps`): no voices, five roles, up to six swaps.
fn walk_swaps(locks: &[&str]) -> Vec<TrackSpec> {
    let roles = ["kick", "snare", "hihat", "bass", "melody"];
    let mut c = WanderContext::new(
        "A",
        TrackSpec::new("techno", 42.0, Some("standard")),
        false,
        &roles,
        &roles,
        locks,
        vec![],
    );
    c.palette = Some("minecraft@32".into());
    c.kit = Some("MuseScore_General.sf2#128:0".into());
    c.palettes = palettes(c.palette.as_deref());
    let mut swaps = vec![];
    for step in 0..300 {
        if swaps.len() >= 6 {
            break;
        }
        let Some(p) = Wander::plan(&c, step) else {
            break;
        };
        if let WanderAction::Swap(s) = p.action {
            swaps.push(s.clone());
            c.spec = s;
        }
    }
    swaps
}

/// `Project.palette`: the palette the track plays from (the first playing net's).
fn project_palette(s: &TrackSpec) -> Option<String> {
    let p = s.project();
    let first = p.nets.iter().find(|(_, nb)| nb.role != "control");
    first.and_then(|(_, nb)| nb.track.palette.clone())
}

/// `Project.drumKit`: the kit the drums play (the first drum net's).
fn project_drum_kit(s: &TrackSpec) -> Option<String> {
    let p = s.project();
    let first = p
        .nets
        .iter()
        .find(|(_, nb)| nb.role != "control" && djrust::sound::is_drum_channel(nb.track.channel));
    first.and_then(|(_, nb)| nb.track.kit.clone())
}

#[test]
fn the_palette_lock_carries_the_palette_and_kit() {
    // WanderPaletteLockTests.thePaletteLockCarriesThePaletteAndKit
    let locked = walk_swaps(&["palette"]);
    assert!(locked.len() >= 3, "swaps to look at");
    for s in &locked {
        assert_eq!(
            project_palette(s).as_deref(),
            Some("minecraft@32"),
            "{} {}",
            s.genre,
            s.seed
        );
        assert_eq!(
            project_drum_kit(s).as_deref(),
            Some("MuseScore_General.sf2#128:0"),
            "{} {}",
            s.genre,
            s.seed
        );
    }
    assert!(WanderLock::DIMENSIONS.contains(&"palette") && WanderLock::is_valid("palette"));
}

#[test]
fn without_the_lock_the_sound_carries_and_only_hops_to_a_neighbour() {
    // WanderPaletteLockTests.withoutTheLockTheSoundCarriesAndOnlyHopsToANeighbour
    let set = PaletteSet::shared();
    let neighbours: Vec<String> = set
        .named(Some("minecraft"))
        .map(|p: &SoundPalette| p.neighbours.clone())
        .unwrap_or_default();
    let swaps = walk_swaps(&[]);
    assert!(!swaps.is_empty());
    for s in &swaps {
        let pal = project_palette(s);
        let id = set.named(pal.as_deref()).map(|p| p.id.clone());
        assert!(
            id.as_deref() == Some("minecraft")
                || id.as_ref().is_some_and(|i| neighbours.contains(i)),
            "{pal:?}"
        );
        assert_eq!(
            project_drum_kit(s).as_deref(),
            Some("MuseScore_General.sf2#128:0"),
            "{} {}",
            s.genre,
            s.seed
        );
    }
}

// MARK: the deck's side of the walk, as pure state (Deck.setWander and friends)

#[test]
fn a_tempo_lock_set_before_any_track_holds_the_first_one_walked_from() {
    // WanderDeckTests.aTempoLockSetBeforeAnyTrackHoldsTheFirstOneWalkedFrom (the state half)
    let mut s = WanderState::default();
    // as the app restores it at launch: nothing loaded yet
    assert!(s.set_lock_anchored("tempo", true, None, None));
    assert!(s.set_lock_anchored("key", true, None, None));
    assert!(s.anchor_bpm.is_none() && s.key.is_none());
    // the first move fills them from what plays then
    let spec = TrackSpec::new("house", 5.0, Some("minimal"));
    s.fill_anchors(Some(Wander::bpm(&spec)), Wander::played_key("house", None));
    assert_eq!(s.anchor_bpm, Some(124.0));
    assert_eq!(s.key.as_ref().map(|k| k.scale.as_str()), Some("MinorScale"));
    // filled once: a later move keeps what the lock holds
    s.fill_anchors(Some(99.0), None);
    assert_eq!(s.anchor_bpm, Some(124.0));
    assert!(s.key.is_some());
}

#[test]
fn locking_tempo_and_key_holds_what_plays_and_freeing_lets_go() {
    // Deck.setWanderLock: tempo -> the spec's bpm, key -> the deck's key, freed -> nil
    let mut s = WanderState::default();
    let key = Wander::played_key("house", Some(3));
    assert!(s.set_lock_anchored("tempo", true, Some(128.0), key.clone()));
    assert!(s.set_lock_anchored("key", true, Some(128.0), key.clone()));
    assert!(s.set_lock_anchored("genre", true, Some(128.0), key.clone()));
    assert_eq!(s.anchor_bpm, Some(128.0));
    assert_eq!(s.key, key);
    assert!(s.set_lock_anchored("tempo", false, Some(99.0), None));
    assert_eq!(s.anchor_bpm, None);
    assert!(s.key.is_some(), "the key lock is still on");
    assert!(s.set_lock_anchored("key", false, None, None));
    assert!(s.key.is_none() && s.is_locked("genre"));
    // not a lock: nothing changes
    assert!(!s.set_lock_anchored("nonsense", true, Some(1.0), None));
    assert!(!s.set_lock_anchored("keep:", true, Some(1.0), None));
    assert_eq!(
        s.locks.iter().map(String::as_str).collect::<Vec<_>>(),
        ["genre"]
    );
}

#[test]
fn the_played_key_is_the_genres_scale_at_the_played_root() {
    // Deck.wanderKey (no Swift test of its own; the Swift source's arithmetic): the offset is
    // the played root's distance from the genre's, mod 12, folded into -5...+6
    let g = BeatsTables::shared().genres.get("house").unwrap();
    let key = |root: Option<i64>| Wander::played_key("house", root).unwrap();
    let k0 = key(None);
    assert_eq!((k0.scale.as_str(), k0.offset), ("MinorScale", 0));
    assert_eq!(k0.root_pc, g.root_note % 12);
    let k3 = key(Some(g.root_note + 3));
    assert_eq!((k3.offset, k3.root_pc), (3, (g.root_note + 3) % 12));
    let k8 = key(Some(g.root_note + 8));
    assert_eq!(
        k8.offset, -4,
        "a seventh up is five down… eight up is four down"
    );
    assert_eq!(k8.root_pc, (g.root_note + 8) % 12);
    assert_eq!(key(Some(g.root_note - 2)).offset, -2);
    assert_eq!(key(Some(g.root_note + 12)).offset, 0);
    assert_eq!(key(Some(g.root_note + 6)).offset, 6);
    assert_eq!(key(Some(g.root_note + 7)).offset, -5);
    assert!(Wander::played_key("no-such-genre", None).is_none());
}

#[test]
fn set_wander_does_not_count_the_bar_it_is_in() {
    // Deck.setWander: on, the bar the clock is in now is not a fresh one
    let mut s = WanderState::default();
    s.set_pace(4);
    s.paused = true;
    s.quiet_bars = 2;
    s.set_on(true, Some(5));
    assert!(s.on && !s.paused && s.quiet_bars == 0);
    assert_eq!(s.last_bar, Some(5));
    assert_eq!(s.advance(5, false), Advance::Nothing, "the same bar again");
    assert_eq!(s.advance(6, false), Advance::LookAhead(7));
    s.set_on(false, None);
    assert!(!s.on && s.last_bar.is_none());
}

#[test]
fn the_deck_state_shows_the_walk() {
    // WanderDeckTests.theDeckStateShowsTheWalk
    let mut s = WanderState::default();
    s.set_pace(4);
    s.set_on(true, Some(0));
    let c = context(techno(), &[]);
    for bar in 0..4 {
        if let Advance::Move(b) = s.advance(bar, false) {
            let p = Wander::plan(&c, s.step).unwrap();
            s.took_move(WanderMove {
                step: s.step,
                bar: b + 1,
                kind: p.kind,
                detail: p.detail.clone(),
                spec: Some(c.spec.clone()),
                cid: None,
            });
        }
    }
    let idle = s.idle_reason(true, Some(&c));
    assert_eq!(idle, None);
    let j = s.deck_json(s.countdown(16 * 3), idle);
    assert_eq!(j.get("on"), Some(&JsonValue::Bool(true)));
    assert_eq!(j.get("pace").and_then(JsonValue::as_i64), Some(4));
    assert_eq!(j.get("step").and_then(JsonValue::as_i64), Some(1));
    assert_eq!(
        j.get("log").and_then(JsonValue::as_array).map(Vec::len),
        Some(1)
    );
    assert!(j.get("nextMoveInBars").is_some() && j.get("resumesInBars").is_none());
    assert!(j.get("idle").is_none() && j.get("tempoAnchor").is_none());
    // key order: on, pace, paused, step, locks, log, then the optional fields
    let text = j.js_stringified();
    assert!(
        text.starts_with(r#"{"on":true,"pace":4,"paused":false,"step":1,"locks":[],"log":[{"step":0,"bar":4,"kind":"#),
        "{text}"
    );
    // paused: the countdown is when it resumes; a tempo anchor shows
    s.grab();
    s.anchor_bpm = Some(128.0);
    let j = s.deck_json(s.countdown(0), None);
    assert_eq!(j.get("resumesInBars").and_then(JsonValue::as_i64), Some(4));
    assert!(j.get("nextMoveInBars").is_none());
    assert_eq!(
        j.get("tempoAnchor").and_then(JsonValue::as_f64),
        Some(128.0)
    );
    // only the newest twelve moves show, newest first
    let mut many = WanderState::default();
    for k in 0..20 {
        many.took_move(WanderMove {
            step: k,
            bar: 4 * (k + 1),
            kind: WanderMoveKind::Macro,
            detail: "drop".into(),
            spec: None,
            cid: None,
        });
    }
    let log = many.deck_json(None, None);
    let log = log.get("log").and_then(JsonValue::as_array).unwrap();
    assert_eq!(log.len(), 12);
    assert_eq!(log[0].get("step").and_then(JsonValue::as_i64), Some(19));
    assert_eq!(log[11].get("step").and_then(JsonValue::as_i64), Some(8));
}

#[test]
fn a_walk_that_cannot_move_says_why() {
    // WanderDeckTests.offOrOnASampleDeckNothingMoves / theDeckStateShowsTheWalk (idle reasons)
    let mut s = WanderState::default();
    let c = context(techno(), &[]);
    assert_eq!(s.idle_reason(true, Some(&c)), None, "off: nothing to say");
    s.on = true;
    assert!(s.idle_reason(false, None).unwrap().contains("sample"));
    assert!(s.idle_reason(true, None).unwrap().contains("composed"));
    assert_eq!(s.idle_reason(true, Some(&c)), None);
    let mut locks: Vec<String> = WanderLock::DIMENSIONS
        .iter()
        .map(|d| d.to_string())
        .collect();
    locks.extend(ROLES.iter().map(|r| WanderLock::keep(r)));
    let refs: Vec<&str> = locks.iter().map(String::as_str).collect();
    let stuck = context(techno(), &refs);
    assert!(s
        .idle_reason(true, Some(&stuck))
        .unwrap()
        .contains("locked"));
}
