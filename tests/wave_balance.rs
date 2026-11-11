//! `.balance`: the parts trimmed toward `MixBalance::target_db`, measured by soloing each part
//! of a whole song (`MixBalance::measure`), and played by the runner as a gain on each synth
//! part's lane. Swift's `BalanceTests` ported (PORTING.md's proof, form (a)); the same trims
//! held against Swift's output sample for sample are the "balance" cases of
//! `tests/runner_golden.rs`.
//!
//! Swift measures through `WaveOptions.extended` with every fix, `.stereo`, `.hfTilt` and
//! `.loudness` among them; this runner plays the fixes of `RUNNER_FIXES` below (the ones it has),
//! and the thresholds are Swift's.

mod common;
use djrust::musicality::apply;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::mix_balance::{BalanceReport, MixBalance};
use djrust::musicality::mix_targets::MixTargets;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;
use djrust::TrackSpec;
use std::collections::BTreeMap;

/// The fixes the runner of this port plays (the others `Fixes::ALL` holds it ignores).
const RUNNER_FIXES: Fixes = Fixes(
    Fixes::SPACE.0
        | Fixes::SHIMMER.0
        | Fixes::ECHO.0
        | Fixes::DUCK.0
        | Fixes::ENERGY.0
        | Fixes::SWEEP.0
        | Fixes::SUBTRACTIVE.0
        | Fixes::UNISON.0
        | Fixes::SUB_OCTAVE.0
        | Fixes::BALANCE.0,
);

fn deck_options(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        instruments: Some(InstrumentTable::shared()),
        musicality: fixes,
        ..WaveOptions::extended()
    }
}

/// `levels(genre, fixes)`: each part's level against the mix, over four bars of the played
/// song's busiest section. `fixes` are the deck's (the runner's); the pass plays them all.
fn levels(genre: &str, structure: &str, fixes: Fixes) -> BTreeMap<String, f64> {
    let spec = TrackSpec::new(genre, 42.0, Some(structure));
    let played = apply(
        &spec.project(),
        &spec,
        Fixes::ALL & !(RUNNER_FIXES & !fixes),
    );
    let o = deck_options(fixes);
    MixBalance::measure(&played, &o.for_genre(genre), 4, 22_050.0)
}

/// BalanceTests.theKickNoLongerIsTheMix: in the dance genres the kick was the whole mix, the bass
/// 12 dB or more under it; balanced, the bass sits within 8 dB of the kick, the lead within 7,
/// and every part is nearer its target place against the others.
#[test]
fn the_kick_no_longer_is_the_mix() {
    for genre in ["techno", "trance", "funk"] {
        let before = levels(genre, "standard", RUNNER_FIXES & !Fixes::BALANCE);
        let after = levels(genre, "standard", RUNNER_FIXES);
        let gap_before = before["kick"] - before["bass"];
        let gap_after = after["kick"] - after["bass"];
        eprintln!("{genre}: kick - bass {gap_before:.2} -> {gap_after:.2} dB");
        assert!(
            gap_before > 10.0,
            "{genre}: kick - bass before {gap_before} dB"
        );
        assert!(gap_after < 8.0, "{genre}: kick - bass after {gap_after} dB");
        assert!(after["kick"] - after["lead"] < 7.0);
        let e0 = MixBalance::error(&before, None).unwrap();
        let e1 = MixBalance::error(&after, None).unwrap();
        eprintln!("{genre}: balance error {e0:.2} -> {e1:.2} dB");
        assert!(
            e1 < e0 - 2.0 && e1 < 3.0,
            "{genre}: balance error {e0} -> {e1} dB"
        );
    }
}

/// BalanceTests.trimsAreBoundedAndKept: trims stay within +-6 dB, follow the parts a song has
/// (none for a kick alone), and a deck's track keeps them across another start of the same song.
#[test]
fn trims_are_bounded_and_kept() {
    let spec = TrackSpec::new("edm", 42.0, Some("minimal"));
    let played = apply(&spec.project(), &spec, Fixes::ALL);
    let o = deck_options(Fixes::BALANCE).for_genre("edm");
    let w = WaveRunner::prepare(&played, 22_050.0, &o, 0);
    let trims = w.balance.clone().unwrap_or_default();
    for part in ["kick", "bass", "lead", "pad", "hats"] {
        assert!(trims.contains_key(part), "{part} in {trims:?}");
    }
    assert!(trims.values().all(|t| t.abs() <= MixBalance::max_trim_db()));
    assert!(trims["kick"] < 0.0 && trims["bass"] > 0.0);
    let mut kick_only = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(16);
    for s in [0, 4, 8, 12] {
        n.bind(s, 36, 10, 100, 50);
    }
    n.track.channel = 10;
    kick_only.nets.insert("kick", n);
    assert!(MixBalance::trims(&kick_only, Fixes::EMPTY, None, None).is_empty());
    // another start of the same song brings the playing project's trims: not measured again
    assert_eq!(
        WaveRunner::prepare(&played, 22_050.0, &o, 0).balance,
        w.balance
    );
    let mut carried = o.clone();
    carried.track_balance = w.balance.clone();
    assert_eq!(
        WaveRunner::prepare(&played, 22_050.0, &carried, 0).balance,
        w.balance
    );
}

/// BalanceTests.familiesPickTheirEntry (ROADMAP 6.6): chill has its own targets.json block, the
/// [RULE-OF-THUMB] jazz / lofi entry (bass and keys forward, hats down); jazz has none and
/// borrows it; dance and song, and no genre, keep the default.
#[test]
fn families_pick_their_entry() {
    assert_eq!(MixBalance::family(Some("lofi")), Some("chill"));
    assert_eq!(MixBalance::family(Some("jazz")), Some("jazz"));
    assert_eq!(MixBalance::family(Some("techno")), Some("dance"));
    assert_eq!(MixBalance::family(None), None);
    assert_eq!(MixBalance::entry(Some("chill")), "chill");
    assert_eq!(MixBalance::entry(Some("jazz")), "chill");
    for f in [Some("dance"), Some("song"), None] {
        assert_eq!(MixBalance::entry(f), "default");
        assert_eq!(MixBalance::targets(f), MixBalance::target_db());
    }
    let chill: BTreeMap<String, f64> = [
        ("kick", -2.0),
        ("bass", -1.0),
        ("lead", -4.0),
        ("snare", -8.0),
        ("pad", -4.0),
        ("arp", -8.0),
        ("hats", -14.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(MixBalance::targets(Some("chill")), chill);
    assert_eq!(MixBalance::targets(Some("jazz")), chill);
    assert!(MixTargets::shared().has_balance_family("chill"));
    let levels: BTreeMap<String, f64> = [("kick", -3.0), ("bass", -2.0), ("hats", -15.0)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let r = BalanceReport::new(levels, Some("bossa"));
    assert!(r.family.as_deref() == Some("jazz") && r.entry == "chill" && r.targets == chill);
    assert!(r.error_db.unwrap() < 1e-9);
}

/// BalanceTests.danceTrimsAreUnchanged: a dance track's trims are what they were before families
/// (the default entry) to within 0.1 dB, on what a deck prepares.
#[test]
fn dance_trims_are_unchanged() {
    for genre in ["techno", "house", "trance"] {
        let spec = TrackSpec::new(genre, 42.0, Some("minimal"));
        let played = apply(&spec.project(), &spec, Fixes::ALL);
        let o = deck_options(RUNNER_FIXES).for_genre(genre);
        let trims = WaveRunner::prepare(&played, 22_050.0, &o, 0)
            .balance
            .unwrap();
        let old = MixBalance::trims(&played, o.musicality, None, None);
        assert_eq!(
            trims.keys().collect::<Vec<_>>(),
            old.keys().collect::<Vec<_>>()
        );
        for (k, v) in &old {
            assert!(
                (trims[k] - v).abs() < 0.1,
                "{genre} {k}: {v} -> {}",
                trims[k]
            );
        }
    }
}

/// BalanceTests.chillFamiliesMeetTheirTargets: jazz and lofi, synth path, balanced to the chill
/// entry: within 2 dB of it, and the bass sits further over the hats than the default's trims
/// would put it.
#[test]
fn chill_families_meet_their_targets() {
    for genre in ["lofi", "jazz"] {
        let after = levels(genre, "standard", RUNNER_FIXES);
        let family = MixBalance::family(Some(genre));
        let e = MixBalance::error(&after, family).unwrap();
        eprintln!(
            "{genre} synth balance error {e:.2} dB against {}: {after:?}",
            MixBalance::entry(family)
        );
        assert!(e <= 2.0, "{genre}: balance error {e} dB, {after:?}");
        let spec = TrackSpec::new(genre, 42.0, Some("standard"));
        let played = apply(&spec.project(), &spec, Fixes::ALL);
        let own = MixBalance::trims(&played, RUNNER_FIXES, family, None);
        let plain = MixBalance::trims(&played, RUNNER_FIXES, None, None);
        if let (Some(b), Some(h), Some(b0), Some(h0)) = (
            own.get("bass"),
            own.get("hats"),
            plain.get("bass"),
            plain.get("hats"),
        ) {
            assert!(
                b - h >= b0 - h0,
                "{genre}: bass - hats trim {} -> {} dB",
                b0 - h0,
                b - h
            );
        }
    }
}

// ---- the trim as the runner plays it -----------------------------------------------------

/// A part's trim is a gain on its synth lane (`lane.st[LEVEL] *= 10^(db/20)`): a bass at +6 dB
/// plays twice (to a float) what it did, a part with no trim is bit-identical, and a fill net is
/// no part's.
#[test]
fn a_trim_is_a_gain_on_the_lane() {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut bass = NetBundle::ring(16);
    bass.bind(0, 36, 6, 100, 200);
    bass.track.channel = 6;
    bass.track.group = Some("bass".into());
    p.nets.insert("bass", bass);
    let mut lead = NetBundle::ring(16);
    lead.bind(0, 72, 4, 100, 200);
    lead.track.channel = 4;
    lead.track.group = Some("melody".into());
    p.nets.insert("melody", lead);
    let render = |trims: Option<BTreeMap<String, f64>>, fixes: Fixes| {
        let o = WaveOptions {
            master: Master::Linear,
            musicality: fixes,
            track_balance: trims,
            ..Default::default()
        };
        common::measure::render_mono(&p, &o, 22_050.0, 0.6)
    };
    let plain = render(None, Fixes::EMPTY);
    let trims: BTreeMap<String, f64> = [("bass", 6.0), ("lead", -6.0)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    // with the fix off, trims given are not played
    assert!(common::measure::identical(
        &plain,
        &render(Some(trims.clone()), Fixes::EMPTY)
    ));
    // empty trims with it on: nothing to play, nothing changes
    assert!(common::measure::identical(
        &plain,
        &render(Some(BTreeMap::new()), Fixes::BALANCE)
    ));
    // bass +6 dB, lead -6 dB: each part alone, against the plain render of that part
    for (keep, db) in [("bass", 6.0), ("melody", -6.0)] {
        let mut solo = p.clone();
        let drop: Vec<String> = solo
            .nets
            .keys()
            .to_vec()
            .into_iter()
            .filter(|k| k != keep)
            .collect();
        for k in drop {
            solo.nets.remove(&k);
        }
        let o = |trims: Option<BTreeMap<String, f64>>, fixes: Fixes| WaveOptions {
            master: Master::Linear,
            musicality: fixes,
            track_balance: trims,
            ..Default::default()
        };
        let a = common::measure::render_mono(&solo, &o(None, Fixes::EMPTY), 22_050.0, 0.6);
        let b = common::measure::render_mono(
            &solo,
            &o(Some(trims.clone()), Fixes::BALANCE),
            22_050.0,
            0.6,
        );
        let g = libm::pow(10.0, db / 20.0);
        for (x, y) in a.iter().zip(&b) {
            assert!(
                (*y as f64 - g * *x as f64).abs() < 1e-6 * (1.0 + x.abs() as f64),
                "{keep}: {x} {y}"
            );
        }
    }
}

/// FixesUnderGesturesTests.roleMutesLeaveTheTrims: a mute is a gesture, not a remix. The trims
/// (`.balance`), the slots' gains and the lanes' voices stay where prepare put them while roles
/// are muted and unmuted.
#[test]
fn role_mutes_leave_the_trims() {
    use djrust::wave::str_hash;
    let spec = TrackSpec::new("techno", 42.0, Some("minimal"));
    let played = apply(&spec.project(), &spec, Fixes::ALL);
    let o = WaveOptions::for_track("techno", 42.0);
    let sr = 22_050.0;
    let prepared = WaveRunner::prepare(&played, sr, &o, 0);
    let before: Vec<Vec<f64>> = prepared.lanes().iter().map(|l| l.voice_state()).collect();
    let gain = prepared.gain;
    let slots: Vec<f32> = (0..64).map(|i| prepared.slot_gain(i)).collect();
    let trims = prepared.balance.clone();
    let mut roles: Vec<String> = prepared
        .lanes()
        .iter()
        .map(|l| l.net_id.clone())
        .filter(|n| !n.is_empty())
        .collect();
    roles.sort();
    roles.dedup();
    let mut r = WaveRunner::new(sr, o);
    r.load_prepared(prepared);
    r.play();
    let mut buf = vec![0f32; 240];
    for (i, role) in roles.iter().enumerate() {
        r.set_role_muted(str_hash(role), i % 2 == 0);
        r.set_mute(role, i % 2 == 1);
        for _ in 0..40 {
            r.process(&mut buf, None, 240);
        }
    }
    let p = r.project().unwrap();
    let after: Vec<Vec<f64>> = p.lanes().iter().map(|l| l.voice_state()).collect();
    assert_eq!(after, before);
    assert_eq!(p.gain, gain);
    assert_eq!((0..64).map(|i| p.slot_gain(i)).collect::<Vec<_>>(), slots);
    assert_eq!(p.balance, trims);
}
