//! `PaletteWanderTests` (3, Swift suite, form (a)): Wander's palette hop, on `Wander::plan` with
//! a palette in play. Swift asks `SoundPalette` what this Mac has installed; here the hop's
//! targets are built from a machine this test describes (`Installed`), so nothing depends on
//! the box it runs on: the floor SoundFont and Apple's GM bank, on which jazz's neighbours
//! play.

use std::collections::BTreeSet;

use djrust::sound::{Installed, PaletteSet, Sounds};
use djrust::voices::VoiceCatalog;
use djrust::wander::{
    Wander, WanderAction, WanderContext, WanderLock, WanderMoveKind, WanderPalettes, WanderPlan,
    WanderVoice,
};
use djrust::TrackSpec;

fn installed() -> Installed {
    Installed {
        dir: "/sf".into(),
        files: vec!["MuseScore_General.sf2".into()],
        gm_bank: true,
        kits: vec![],
        minecraft: None,
        minecraft_assets_missing: None,
    }
}

/// `PaletteWanderTests.context`.
fn context(locks: &[&str], palette: Option<&str>) -> WanderContext {
    let inst = installed();
    let spec = TrackSpec::new("house", 11.0, Some("standard")).with_palette(palette, &inst);
    let roles = ["kick", "snare", "hihat", "bass", "melody", "harmony"];
    let mut bass = WanderVoice::new(&["bass"], 6, "bass", &["bass", "acid"]);
    bass.option = Some("gm:32".into());
    let mut lead = WanderVoice::new(&["melody"], 4, "lead", &["lead", "pluck"]);
    lead.option = Some("gm:66".into());
    let mut c = WanderContext::new("A", spec, false, &roles, &roles, locks, vec![bass, lead]);
    c.palette = palette.map(String::from);
    c.channels = vec![4, 6, 7, 10, 11, 12, 13];
    let catalog = {
        let mut cat = VoiceCatalog::bundled();
        cat.sound_fonts = inst.sound_fonts();
        cat
    };
    c.palettes = WanderPalettes::from_sounds(
        PaletteSet::shared(),
        palette,
        Sounds {
            catalog: &catalog,
            installed: &inst,
        },
    );
    c
}

/// `PaletteWanderTests.hops`.
fn hops(c: &WanderContext, steps: i64) -> Vec<WanderPlan> {
    (0..steps)
        .filter_map(|s| Wander::plan(c, s))
        .filter(|p| p.kind == WanderMoveKind::Palette)
        .collect()
}

/// `sourceNames`: does the spec's `tracks` override set `field` to `value` on any channel?
fn names(spec: &TrackSpec, field: &str, value: &str) -> bool {
    spec.layers
        .get("tracks")
        .and_then(|t| t.as_object())
        .is_some_and(|tracks| {
            tracks
                .iter()
                .any(|(_, v)| v.get(field).and_then(|f| f.as_str()) == Some(value))
        })
}

// PaletteWanderTests.aPaletteHopGoesToANeighbourAndRespectsLocks: rare, to a neighbour only,
// and never under the palette or instruments lock.
#[test]
fn a_palette_hop_goes_to_a_neighbour_and_respects_locks() {
    let free = hops(&context(&[], Some("jazz")), 3000);
    assert!(
        !free.is_empty() && free.len() < 150,
        "rare: {} in 3000",
        free.len()
    );
    let set = PaletteSet::shared();
    let neighbours: BTreeSet<&str> = set
        .named(Some("jazz"))
        .expect("jazz")
        .neighbours
        .iter()
        .map(String::as_str)
        .collect();
    for h in &free {
        let WanderAction::Swap(s) = &h.action else {
            panic!("a hop swaps: {}", h.detail);
        };
        let palette = s.palette();
        let to = set
            .named(palette.as_deref())
            .map_or("studio", |p| p.id.as_str());
        assert!(neighbours.contains(to), "{}", h.detail);
        assert!(s.seed == 11.0 && s.genre == "house", "the same song");
    }
    assert!(hops(&context(&["palette"], Some("jazz")), 3000).is_empty());
    assert!(hops(&context(&["instruments"], Some("jazz")), 3000).is_empty());
    let w = WanderMoveKind::Palette.weight();
    assert!(w > 0 && w <= 3);
    // (Swift's literal is 100 and its comment says "out of 100", but the three layer moves, 6 + 4 + 3,
    // joined the table after that test was written: at the pinned commit the weights
    // `WanderMoveKind.weight` holds, which the table here equals, add up to 113)
    assert_eq!(WanderMoveKind::ALL.iter().map(|k| k.weight()).sum::<i64>(), 113);
}

// PaletteWanderTests.aKeptRoleRidesThroughAHop: a kept role keeps the sound it plays through a
// hop; the default palette hops too.
#[test]
fn a_kept_role_rides_through_a_hop() {
    let keep = WanderLock::keep("melody");
    let c = context(&[keep.as_str()], Some("jazz"));
    let h = hops(&c, 3000).into_iter().next().expect("a hop");
    let WanderAction::Swap(s) = &h.action else {
        panic!("a hop swaps");
    };
    assert!(names(s, "voice", "gm:66"));
    assert!(!names(s, "voice", "gm:32"), "only the kept role");
    assert!(!hops(&context(&[], None), 3000).is_empty(), "from Studio too");
}

// PaletteWanderTests.swapsCarryThePalette: every other swap keeps the deck's palette.
#[test]
fn swaps_carry_the_palette() {
    let c = context(&["palette"], Some("jazz"));
    let set = PaletteSet::shared();
    let mut n = 0;
    for step in 0..400 {
        let Some(p) = Wander::plan(&c, step) else { continue };
        let WanderAction::Swap(s) = &p.action else { continue };
        n += 1;
        let palette = s.palette();
        assert_eq!(
            set.named(palette.as_deref()).map(|p| p.id.as_str()),
            Some("jazz"),
            "{}",
            p.detail
        );
    }
    assert!(n > 10, "{n} swaps");
}
