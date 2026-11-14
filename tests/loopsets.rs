//! Loop sets (`djrust::loopsets`): every genre's curated set, generated sets that are the same set
//! again from the same genre and seed, and loops that load as one- to twelve-bar projects.

use djrust::loopsets::{self, cut, genres, played, LoopSet, Side, GROUPS};
use djrust::wave::synth::is_drum_channel;

#[test]
fn every_genre_has_a_curated_set() {
    for g in genres() {
        let s = LoopSet::curated(&g).unwrap_or_else(|| panic!("{g} has no curated set"));
        assert_eq!(s.groups.len(), GROUPS, "{g}");
        assert_eq!(s.seed, None);
        assert_eq!(s.spec(), g);
        assert_eq!(s.transpose, 0);
        let mut names: Vec<&str> = s.groups.iter().map(|g| g.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), GROUPS, "{g}: group names repeat");
        assert_eq!(s.loops().len(), GROUPS * 2);
    }
}

#[test]
fn a_loop_is_a_project_at_the_sets_tempo() {
    let s = LoopSet::curated("house").unwrap();
    for l in s.loops() {
        let p = s.project(&l.id, true).unwrap();
        assert_eq!(p.tempo, s.bpm, "{}", l.id);
        assert!(p.structure.is_empty() && p.initial_mutes.is_empty());
        assert!(p
            .nets
            .values()
            .all(|n| n.role != "control" && n.riff_group.is_empty()));
        for part in ["kick", "bass", "melody"] {
            assert!(
                p.nets.contains_key(part),
                "{} has no {part}: {:?}",
                l.id,
                p.nets.keys()
            );
        }
        let bars = p.bars.unwrap();
        assert!((1..=12).contains(&bars), "{} is {bars} bars", l.id);
        let dry = s.project(&l.id, false).unwrap();
        assert!(dry.nets.values().all(|n| !is_drum_channel(n.track.channel)));
        assert!(dry.nets.contains_key("bass"));
    }
    assert!(s.project("7A", true).is_none() && s.project("1C", true).is_none());
}

#[test]
fn b_answers_a() {
    let p = played("techno", 5);
    let a = cut(&p, Side::A, true);
    let b = cut(&p, Side::B, true);
    let notes = |p: &djrust::Project| {
        p.nets
            .iter()
            .flat_map(|(k, n)| {
                n.bindings
                    .iter()
                    .map(move |(t, b)| (k.to_string(), t.to_string(), b.note))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(a.nets.keys(), b.nets.keys());
    assert_ne!(notes(&a), notes(&b));
}

#[test]
fn a_generated_set_is_the_same_set_again() {
    let a = LoopSet::generate("techno", 4411).unwrap();
    let b = LoopSet::parse(" Techno:4411 ").unwrap();
    assert_eq!(a, b);
    assert_eq!(a.spec(), "techno:4411");
    assert_eq!(a.name(), "Techno #4411");
    assert_eq!(a.cid(), b.cid());
    assert!(a.cid().starts_with('z'));
    assert_eq!(
        a.project("3B", true).unwrap().model_cid(),
        b.project("3b", true).unwrap().model_cid()
    );
    let c = LoopSet::generate("techno", 4412).unwrap();
    assert_ne!(a.cid(), c.cid());
    assert_ne!(a.groups, c.groups);
    // its own key and tempo, near the genre's
    let base = LoopSet::curated("techno").unwrap();
    assert!(
        (a.bpm / base.bpm - 1.0).abs() <= 0.041,
        "{} vs {}",
        a.bpm,
        base.bpm
    );
    assert!((-5..=6).contains(&a.transpose));
    assert_eq!(a.root, base.root + a.transpose);
    assert!(LoopSet::generate("nope", 1).is_none() && LoopSet::parse("techno:x").is_none());
}

#[test]
fn a_transposed_set_moves_its_tonal_parts_only() {
    let s = (1..50)
        .find_map(|seed| LoopSet::generate("house", seed).filter(|s| s.transpose != 0))
        .unwrap();
    let mut plain = cut(&played(&s.genre, s.groups[0].seed), Side::A, true);
    plain.tempo = s.bpm;
    let moved = s.project("1A", true).unwrap();
    let bass = |p: &djrust::Project| {
        p.nets
            .get("bass")
            .unwrap()
            .bindings
            .values()
            .map(|b| b.note)
            .collect::<Vec<_>>()
    };
    let kick = |p: &djrust::Project| {
        p.nets
            .get("kick")
            .unwrap()
            .bindings
            .values()
            .map(|b| b.note)
            .collect::<Vec<_>>()
    };
    assert_eq!(kick(&plain), kick(&moved));
    let want: Vec<i64> = bass(&plain)
        .iter()
        .map(|n| (n + s.transpose).clamp(0, 127))
        .collect();
    assert_eq!(bass(&moved), want);
}

#[test]
fn the_cid_names_the_document() {
    let s = LoopSet::curated("lofi").unwrap();
    let doc = s.to_json();
    assert_eq!(
        doc.get("@schema").and_then(|v| v.as_str()),
        Some(loopsets::SCHEMA)
    );
    assert_eq!(
        doc.get("tracks")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(GROUPS)
    );
    assert_eq!(s.cid(), djrust::cid::share_cid(&doc));
}
