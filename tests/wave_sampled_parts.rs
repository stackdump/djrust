//! What a sampled part needs of the runner: its slot's gain (`.balance`), its send to the room
//! and the echo, and its level against the lane's own synth voice (`ownPartLevelDB`). The Swift
//! suite for this is `SampledBalanceTests` (and `EchoTests.sampledLeadsEcho`), which needs
//! Apple's AUSampler bank to measure a slot; here a fake bank binds slots and answers with
//! fixed part levels, and the assertions are the Swift tests' own relations between the
//! measured level, the trims and the slot gains (PORTING.md's proof, form (a)). The synth side
//! of the levels is held to Swift's numbers in `tests/runner_golden.rs`.

use djrust::musicality::echo::EchoSpec;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::loudness::Loudness;
use djrust::musicality::mix_balance::{MixBalance, SampledBalance};
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;
use djrust::{compose, JsonObject};
use std::collections::BTreeMap;

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        ov.set("structure", s.into());
    }
    compose(genre, &ov.into())
}

const SR: f64 = 48_000.0;

fn bind_tonal(_instrument: &str, _part: &str) -> Option<usize> {
    // one sampler slot per part, as a bank loads one instrument for each
    match _part {
        "bass" => Some(1),
        "harmony" | "pad" => Some(2),
        "melody" => Some(3),
        "arp" => Some(4),
        _ => Some(5),
    }
}

/// Slot's part level, dB, against the group's plain synth voice (`slotPartLevelDB`).
fn measured(slot: usize, group: &str) -> Option<f64> {
    let _ = group;
    // an empty slot has none; the rest differ by slot so the tests tell them apart
    (slot != 5).then(|| -2.0 - slot as f64)
}

fn opts(fixes: Fixes) -> WaveOptions {
    let mut trims = BTreeMap::new();
    for (part, db) in [
        ("bass", 3.0),
        ("harmony", -2.0),
        ("melody", 1.5),
        ("arp", 20.0),
    ] {
        trims.insert(part.to_string(), db);
    }
    WaveOptions {
        master: Master::Linear,
        instruments: Some(InstrumentTable::shared()),
        musicality: fixes,
        sample_binder: Some(bind_tonal),
        part_level: Some(measured),
        bank_rate: SR,
        track_balance: Some(trims),
        ..Default::default()
    }
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// The tonal lanes that play a sampler, with their group.
fn sampled_lanes(p: &WaveProject) -> Vec<(&Lane, String)> {
    p.lanes()
        .iter()
        .filter_map(|l| {
            let g = p.sampled_groups.get(&l.net_id)?;
            (l.kind == "tonal" && l.sample_slot().is_some()).then(|| (l, g.clone()))
        })
        .collect()
}

// SampledBalanceTests.gainIsTheTrimLessTheGapClamped: the trim less the gap, clamped; unmeasured,
// the trim alone. The probe then stands in at the trimmed synth voice's level unless the clamp
// left some of the gap.
#[test]
fn gain_is_the_trim_less_the_gap_clamped() {
    assert_eq!(SampledBalance::gain_db(3.0, Some(-5.0)), 8.0);
    assert_eq!(SampledBalance::gain_db(-2.0, Some(4.0)), -6.0);
    assert_eq!(SampledBalance::gain_db(2.0, None), 2.0);
    assert_eq!(SampledBalance::gain_db(6.0, Some(-20.0)), SampledBalance::RANGE_DB);
    assert_eq!(SampledBalance::gain_db(-6.0, Some(20.0)), -SampledBalance::RANGE_DB);
    let fit = SampledBalance::probe_level(3.0, Some(-5.0), "bass", 8.0);
    assert!((fit - 1.0).abs() < 1e-9);
    // clamped at +12 against a 20 dB gap: the sampler sits 8 dB under its trimmed voice
    let short = SampledBalance::probe_level(0.0, Some(-20.0), "bass", 12.0);
    assert!((db(short) + 8.0).abs() < 1e-9);
    // unmeasured: the palette's average gap, as before
    let avg = SampledBalance::probe_level(2.0, None, "harmony", 2.0);
    assert!((avg - Loudness::sampled_part_level("harmony", None)).abs() < 1e-9);
    let other = SampledBalance::probe_level(0.0, None, "x", 0.0);
    assert!((other - Loudness::sampled_part_level("melody", None)).abs() < 1e-9);
}

// SampledBalanceTests.preparedSlotsCarryTheirGain
#[test]
fn prepared_slots_carry_their_gain() {
    for genre in ["country", "lofi", "jazz"] {
        let p = project(genre, 7.0, Some("minimal"));
        let o = opts(Fixes::BALANCE);
        let prepared = WaveRunner::prepare(&p, SR, &o, 0);
        let sampled = sampled_lanes(&prepared);
        assert!(!sampled.is_empty(), "{genre}: a sampled part to measure");
        let trims = prepared.balance.clone().unwrap_or_default();
        let mut seen = std::collections::BTreeSet::new();
        for (lane, group) in &sampled {
            let slot = lane.sample_slot().unwrap();
            let trim = MixBalance::part_of_kind("tonal", group)
                .and_then(|p| trims.get(p).copied())
                .unwrap_or(0.0);
            // (the slot's level against this net's own synth voice, `ownPartLevelDB`)
            let level = prepared.sampled_part_db.get(&lane.net_id).copied();
            assert_eq!(level.is_none(), measured(slot, group).is_none());
            let want = SampledBalance::gain_db(trim, level);
            // a slot two nets share keeps the first's gain
            if seen.insert(slot) {
                assert!(
                    (db(prepared.slot_gain(slot) as f64) - want).abs() < 1e-3,
                    "{genre} {}",
                    lane.net_id
                );
            }
            let bit = 1u64 << slot;
            assert_eq!(
                prepared.gain_slots() & bit != 0,
                (prepared.slot_gain(slot) - 1.0).abs() > 1e-4
            );
            let stand = prepared.probe_levels[&lane.net_id];
            if level.is_some() && want.abs() < SampledBalance::RANGE_DB {
                assert!(
                    db(stand).abs() < 1e-3,
                    "{genre} {}: the probe plays it as trimmed",
                    lane.net_id
                );
            }
        }
        for slot in 0..64 {
            if !seen.contains(&slot) {
                assert_eq!(
                    prepared.slot_gain(slot),
                    1.0,
                    "{genre}: slot {slot} plays nothing here"
                );
            }
        }
    }
}

// SampledBalanceTests.withoutBalanceSlotsStayAtUnity
#[test]
fn without_balance_slots_stay_at_unity() {
    let p = project("country", 7.0, Some("minimal"));
    let prepared = WaveRunner::prepare(&p, SR, &opts(Fixes::EMPTY), 0);
    let sampled = sampled_lanes(&prepared);
    assert!(!sampled.is_empty());
    assert_eq!(prepared.gain_slots(), 0);
    assert!((0..64).all(|s| prepared.slot_gain(s) == 1.0));
    for (lane, group) in &sampled {
        let level = prepared.sampled_part_db.get(&lane.net_id).copied();
        assert!(level.is_some() == measured(lane.sample_slot().unwrap(), group).is_some());
        assert_eq!(
            prepared.probe_levels[&lane.net_id],
            Loudness::sampled_part_level(group, level)
        );
    }
}

// SampledBalanceTests.noBankNoSlotGains
#[test]
fn no_bank_no_slot_gains() {
    let p = project("country", 7.0, Some("minimal"));
    let mut o = opts(Fixes::BALANCE);
    o.sample_binder = None;
    let prepared = WaveRunner::prepare(&p, SR, &o, 0);
    assert_eq!(prepared.gain_slots(), 0);
    assert!(prepared.probe_levels.is_empty());
    let parity = WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0);
    assert_eq!(parity.gain_slots(), 0);
}

/// A part the slot measured moves to be against the lane's own voice: `measured + plain - own`,
/// and the sampled net's own synth lane is stood in at the probe level (`LevelMatch`'s probe).
#[test]
fn the_part_level_is_taken_against_the_lanes_own_voice() {
    let p = project("country", 7.0, Some("minimal"));
    let prepared = WaveRunner::prepare(&p, SR, &opts(Fixes::EMPTY), 0);
    let table = InstrumentTable::shared();
    for (lane, group) in sampled_lanes(&prepared) {
        let Some(got) = prepared.sampled_part_db.get(&lane.net_id).copied() else {
            continue;
        };
        let voice = table.voice(Some(&lane.instrument), Some(&group));
        let want = own_part_level_db(
            measured(lane.sample_slot().unwrap(), &group),
            &voice,
            &group,
            SR,
            &|_| {},
        )
        .unwrap();
        assert_eq!(got, want, "{}", lane.net_id);
        let plain = SamplerPartLevels::synth_level_db(SamplerPartLevels::probe_group(&group), SR);
        let own = own_synth_part_db(&voice, &group, SR, &|_| {});
        assert!(
            (got - (measured(lane.sample_slot().unwrap(), &group).unwrap() + plain - own)).abs()
                < 1e-12
        );
    }
}

/// The loudness probe's stand-in (`WaveOptions.probeLevels`): a level on the net's tonal lane.
#[test]
fn the_probe_stand_in_scales_the_tonal_lane() {
    let p = project("country", 7.0, Some("minimal"));
    let base = WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0);
    let mut o = WaveOptions::default();
    o.probe_levels.insert("melody".into(), 0.5);
    let scaled = WaveRunner::prepare(&p, SR, &o, 0);
    for (a, b) in base.lanes().iter().zip(scaled.lanes()) {
        let want = if a.net_id == "melody" && a.kind == "tonal" {
            a.level() * 0.5
        } else {
            a.level()
        };
        assert_eq!(b.level(), want, "{}", a.net_id);
    }
}

// EchoTests.sampledLeadsEcho (the prepared project's half: the slot sends)
#[test]
fn a_sampled_lead_sends_to_the_echo_and_the_room() {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(64);
    n.bind(0, 72, 4, 110, 0);
    n.track.channel = 4;
    n.track.group = Some("melody".into());
    p.nets.insert("melody", n);
    let o = WaveOptions {
        musicality: Fixes::ECHO | Fixes::SPACE,
        echo: EchoSpec::for_genre("trance"),
        sample_binder: Some(bind_tonal),
        ..Default::default()
    };
    let prepared = WaveRunner::prepare(&p, SR, &o, 0);
    let lane = prepared
        .lanes()
        .iter()
        .find(|l| l.net_id == "melody")
        .unwrap();
    let slot = lane.sample_slot().unwrap();
    assert_eq!(prepared.echo_slots(), 1 << slot);
    assert_eq!(prepared.send_slots(), 1 << slot);
    assert_eq!(
        prepared.slot_echo(slot),
        EchoSpec::send("tonal", "melody") as f32
    );
    // without the echo, a sampled lead is not taken off its group path for nothing
    let plain = WaveRunner::prepare(
        &p,
        SR,
        &WaveOptions {
            musicality: Fixes::SPACE,
            ..o.clone()
        },
        0,
    );
    assert_eq!(plain.echo_slots(), 0);
    // and without a bank there are no sampler slots to send from
    let nobank = WaveRunner::prepare(
        &p,
        SR,
        &WaveOptions {
            sample_binder: None,
            ..o
        },
        0,
    );
    assert_eq!(nobank.echo_slots(), 0);
    assert_eq!(nobank.send_slots(), 0);
}
