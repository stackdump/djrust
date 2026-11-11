//! `djrust::musicality`: the pass over a composed track. No oracle on this box (Swift is
//! not available): these hold the Rust port to the invariants Swift's `MusicalityTests`
//! assert on every track a deck plays.

use djrust::gen::tables::BeatsTables;
use djrust::musicality::{apply, Fixes};
use djrust::{Project, TrackSpec};

fn played(
    genre: &str,
    seed: f64,
    structure: Option<&str>,
    fixes: Fixes,
) -> (TrackSpec, Project, Project) {
    let spec = TrackSpec::new(genre, seed, structure);
    let composed = spec.project();
    let played = apply(&composed, &spec, fixes);
    (spec, composed, played)
}

/// Every flag has its own bit, a name `djctl render --fixes` takes, and is in `all`.
#[test]
fn fixes_registry_is_consistent() {
    let mut union = Fixes::EMPTY;
    let mut bits = std::collections::BTreeSet::new();
    for (name, f) in Fixes::NAMED {
        assert!(f.0 != 0 && f.0 & (f.0 - 1) == 0, "{name}: one bit per flag");
        assert!(bits.insert(f.0), "{name}: bits are unique");
        union |= *f;
        assert_eq!(Fixes::named(name), Some(*f));
    }
    assert_eq!(union, Fixes::ALL, "named and all list the same flags");
    assert_eq!(Fixes::named("none"), Some(Fixes::EMPTY));
    assert_eq!(Fixes::NAMED.len(), 53);
    // the runner-only subset is a subset of `all`
    assert!(Fixes::ALL.contains(Fixes::RUNNER_ONLY));
    assert_eq!(Fixes::RUNNER_ONLY.names().len(), 23);
    assert_eq!(Fixes::named("key").unwrap().0, 1);
    assert_eq!(Fixes::named("shimmer").unwrap().0, 1 << 52);
}

#[test]
fn smoke_every_genre_every_fix() {
    let tables = BeatsTables::shared();
    for genre in &tables.genre_ids {
        for seed in [7.0, 42.0] {
            for structure in [Some("standard"), None] {
                let (spec, composed, p) = played(genre, seed, structure, Fixes::ALL);
                assert_eq!(p.nets.keys(), composed.nets.keys(), "{genre} {seed}");
                let again = apply(&composed, &spec, Fixes::ALL);
                assert_eq!(
                    again.canonical_json(),
                    p.canonical_json(),
                    "{genre} deterministic"
                );
            }
        }
    }
}
