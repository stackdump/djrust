//! `MacroMirror` (SPEC mode 26, engine side): the audio thread publishes which macros are in
//! effect and which nets they hold muted; the main thread reads it. Port of
//! `MacroMirrorTests.swift` and `MacroMirrorStress.swift`: the same literals, driven through the
//! `Source` harness (a bare `WaveDeckSource`, block by block, as the deck's source node does).
//! Form (a) of the parity rules: the Swift suite's own numbers. The Swift-generated words
//! (form b) are in `tests/mirrors_golden.rs`; the allocation measurement is in
//! `tests/mirrors_rt.rs`.

mod mirror_support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use djrust::wave::*;
use mirror_support::stress::Gate;
use mirror_support::*;

fn techno() -> (Driver, Loaded) {
    let (src, loaded) = make_source(&project("techno", 42.0, "loop"), 300.0, 22_050.0);
    (Driver::new(src), loaded)
}

fn audible(d: &Driver, roles: &[&'static str]) -> Vec<&'static str> {
    roles
        .iter()
        .copied()
        .filter(|r| d.src.is_role_audible(r))
        .collect()
}

fn is_drum_role(r: &str) -> bool {
    DRUMS.contains(&r)
}

// MacroMirrorTests.aMuteMacroIsInEffectUntilItsRestore
#[test]
fn a_mute_macro_is_in_effect_until_its_restore() {
    let (mut d, _) = techno();
    d.run(50);
    assert!(d.src.active_macros().is_empty() && !d.src.is_macro_active());
    let roles = d.src.roles();
    assert!(d.src.roles_held_by_macros(&roles).is_empty());
    let sounding = audible(&d, &roles);
    d.src.fire(NetMacro::Drop, Some(16)); // 16 ticks = 800 ms = ~138 blocks
    d.run(3);
    assert!(d.src.active_macros() == [NetMacro::Drop]);
    assert!(d.src.is_active(NetMacro::Drop) && !d.src.is_active(NetMacro::Cut));
    // the non-drum roles that were sounding are the ones held; the drums are not
    let held = d.src.roles_held_by_macros(&roles);
    let want: Vec<&str> = sounding
        .iter()
        .copied()
        .filter(|r| !is_drum_role(r))
        .collect();
    assert_eq!(held, want);
    assert!(
        held.len() >= 3,
        "techno has bass, melody, harmony...: {held:?}"
    );
    assert_eq!(
        d.src.roles_held(NetMacro::Drop, &roles),
        held,
        "all of it is the drop's"
    );
    assert!(
        d.src.roles_held(NetMacro::Breakdown, &roles).is_empty(),
        "none of it is another's"
    );
    d.run(100);
    assert!(d.src.is_active(NetMacro::Drop), "still held at 600 ms");
    d.run(60);
    assert!(d.src.active_macros().is_empty(), "restored on the 16th tick");
    assert!(d.src.roles_held_by_macros(&roles).is_empty());
    assert_eq!(audible(&d, &roles), sounding, "and the roles are back");
}

// MacroMirrorTests.eachMacroHoldsTheRolesItsSelectorPicks
#[test]
fn each_macro_holds_the_roles_its_selector_picks() {
    type Expect = fn(&str) -> bool;
    let cases: [(NetMacro, i64, Expect); 3] = [
        (NetMacro::Breakdown, 16, |r| is_drum_role(r)),
        (NetMacro::SoloDrums, 16, |r| !is_drum_role(r)),
        (NetMacro::Cut, 8, |_| true),
    ];
    for (m, ticks, expect) in cases {
        let (mut d, _) = techno();
        d.run(50);
        let roles = d.src.roles();
        let sounding = audible(&d, &roles);
        d.src.fire(m, Some(ticks));
        d.run(3);
        assert!(d.src.active_macros() == [m], "{m:?}");
        let want: Vec<&str> = sounding.iter().copied().filter(|r| expect(r)).collect();
        assert_eq!(d.src.roles_held_by_macros(&roles), want, "{m:?}");
        d.run(400);
        assert!(d.src.active_macros().is_empty(), "{m:?} ends");
    }
}

// MacroMirrorTests.aRoleSomeoneMutedIsNotHeldByTheMacro
#[test]
fn a_role_someone_muted_is_not_held_by_the_macro() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.set_role_muted("bass", true);
    d.run(3);
    d.src.fire(NetMacro::Drop, Some(8));
    d.run(3);
    let roles = d.src.roles();
    let held = d.src.roles_held_by_macros(&roles);
    assert!(
        !held.contains(&"bass") && held.contains(&"melody"),
        "{held:?}"
    );
}

// MacroMirrorTests.aRoleThePerformerSwitchesBackOnIsNoLongerHeld
#[test]
fn a_role_the_performer_switches_back_on_is_no_longer_held() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::Drop, Some(32));
    d.run(3);
    let roles = d.src.roles();
    let held = d.src.roles_held(NetMacro::Drop, &roles);
    assert!(held.contains(&"bass") && held.contains(&"melody"));
    d.src.set_role_muted("bass", false); // back on, mid-drop
    d.run(3);
    let after = d.src.roles_held(NetMacro::Drop, &roles);
    let want: Vec<&str> = held.iter().copied().filter(|r| *r != "bass").collect();
    assert_eq!(after, want, "the bass is audible again: {after:?}");
    assert!(
        d.src.is_active(NetMacro::Drop),
        "the macro itself is still running"
    );
    assert!(d.src.is_role_audible("bass"));
}

// MacroMirrorTests.theBeatRepeatCountsWhileItRunsEvenBetweenBursts
#[test]
fn the_beat_repeat_counts_while_it_runs_even_between_bursts() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::BeatRepeat, Some(32)); // 1.6 s = ~275 blocks
    let (mut seen_held, mut seen_free) = (0, 0);
    let (mut kick_held, mut always) = (false, true);
    d.run_each(240, |src| {
        always = always && src.is_active(NetMacro::BeatRepeat);
        let roles = src.roles();
        let held = src.roles_held_by_macros(&roles);
        if held.is_empty() {
            seen_free += 1;
        } else {
            seen_held += 1;
        }
        kick_held = kick_held || held.contains(&"kick");
    });
    assert!(always, "in effect for the whole run");
    assert!(
        seen_held > 20 && seen_free > 20,
        "bursts hold nets and gaps do not: {seen_held}/{seen_free}"
    );
    assert!(!kick_held, "the kick keeps its pulse");
    d.run(200);
    assert!(d.src.active_macros().is_empty());
}

// MacroMirrorTests.theDoubleDropNeverLeavesTheMirrorBetweenItsHalves
#[test]
fn the_double_drop_never_leaves_the_mirror_between_its_halves() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::DoubleDrop, Some(8)); // cut for 2 ticks, then a drop 5 ticks in (260 ms) for 8
    let mut active: Vec<bool> = Vec::new();
    let mut held_counts: Vec<usize> = Vec::new();
    d.run_each(400, |src| {
        active.push(src.is_active(NetMacro::DoubleDrop));
        let roles = src.roles();
        held_counts.push(src.roles_held_by_macros(&roles).len());
    });
    let first = active.iter().position(|&a| a);
    let last = active.iter().rposition(|&a| a);
    assert!(first.is_some() && last.is_some());
    let (first, last) = (first.unwrap(), last.unwrap());
    assert!(first <= 1, "in effect from the block after the fire");
    assert!(
        active[first..=last].iter().all(|&a| a),
        "no gap while the second half waits on its timer"
    );
    // 2 ticks of cut + 3 of waiting + 8 of drop = ~13 ticks = 650 ms = ~112 blocks
    assert!(
        last - first > 90 && last - first < 140,
        "ran {} blocks",
        last - first
    );
    // the wait is a stretch with nothing held, between a cut that held more and a drop that held less
    let peak = held_counts[first];
    let waiting = held_counts[first..=last].iter().filter(|&&c| c == 0).count();
    assert!(
        waiting > 10,
        "the gap between the halves holds no role: {waiting} blocks"
    );
    let drop_held = held_counts[first..=last]
        .iter()
        .rev()
        .find(|&&c| c > 0)
        .copied()
        .unwrap_or(0);
    assert!(peak > drop_held, "cut holds more than drop: {peak}");
    d.run(100);
    assert!(d.src.active_macros().is_empty());
}

// MacroMirrorTests.tempoMacrosAreInEffectButHoldNoRole
#[test]
fn tempo_macros_are_in_effect_but_hold_no_role() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::HalfTime, Some(8));
    d.run(3);
    assert!(d.src.active_macros() == [NetMacro::HalfTime]);
    let roles = d.src.roles();
    assert!(
        d.src.roles_held_by_macros(&roles).is_empty(),
        "a tempo macro mutes nothing"
    );
    d.src.fire(NetMacro::TapeStop, Some(8)); // stacked on the first
    d.run(3);
    assert!(d.src.active_macros() == [NetMacro::HalfTime, NetMacro::TapeStop]);
    d.src.panic();
    d.run(3);
    assert!(d.src.active_macros().is_empty(), "panic ends them");
}

// MacroMirrorTests.panicClearsEveryMacroAndEveryHeldNet
#[test]
fn panic_clears_every_macro_and_every_held_net() {
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::Drop, Some(64));
    d.src.fire(NetMacro::BeatRepeat, Some(64));
    d.run(5);
    assert_eq!(d.src.active_macros().len(), 2);
    let roles = d.src.roles();
    assert!(!d.src.roles_held_by_macros(&roles).is_empty());
    d.src.panic();
    d.run(3);
    assert!(d.src.active_macros().is_empty() && d.src.roles_held_by_macros(&roles).is_empty());
}

// MacroMirrorTests.aMacroFiredOverAnotherChasesOnlyWhatItMutedItself
#[test]
fn a_macro_fired_over_another_chases_only_what_it_muted_itself() {
    let (mut d, _) = techno();
    d.run(50);
    let roles = d.src.roles();
    let sounding = audible(&d, &roles);
    d.src.fire(NetMacro::Drop, Some(24)); // holds the non-drums
    d.src.fire(NetMacro::Cut, Some(8)); // finds only the drums still sounding
    d.run(3);
    assert!(d.src.active_macros() == [NetMacro::Drop, NetMacro::Cut]);
    let drop = d.src.roles_held(NetMacro::Drop, &roles);
    let cut = d.src.roles_held(NetMacro::Cut, &roles);
    let non_drums: Vec<&str> = sounding
        .iter()
        .copied()
        .filter(|r| !is_drum_role(r))
        .collect();
    let drums: Vec<&str> = sounding.iter().copied().filter(|r| is_drum_role(r)).collect();
    assert!(drop == non_drums && !drop.is_empty(), "drop: {drop:?}");
    assert!(cut == drums && !cut.is_empty(), "cut: {cut:?}");
    assert!(
        drop.iter().all(|r| !cut.contains(r)),
        "two chases, no role in both"
    );
    let mut both = d.src.roles_held_by_macros(&roles);
    let mut all = sounding.clone();
    both.sort();
    all.sort();
    assert_eq!(both, all, "together they hold it all");
    // the cut ends first (8 ticks = 400 ms): the drop keeps what it had, the drums are back
    d.run(80);
    assert!(d.src.active_macros() == [NetMacro::Drop], "cut over");
    assert_eq!(d.src.roles_held(NetMacro::Drop, &roles), drop);
    assert!(d.src.roles_held(NetMacro::Cut, &roles).is_empty());
    assert!(drums.iter().all(|r| d.src.is_role_audible(r)));
}

// MacroMirrorTests.aHandOverWaitingForItsBarIsNotChasedThroughTheNewProjectsTables
#[test]
fn a_hand_over_waiting_for_its_bar_is_not_chased_through_the_new_projects_tables() {
    let rate = 22_050.0;
    let (mut d, _) = techno();
    d.run(50);
    d.src.fire(NetMacro::Drop, Some(64)); // 3.2 s: it will still be running at the hand-over
    d.run(3);
    let roles = d.src.roles();
    let before = d.src.roles_held(NetMacro::Drop, &roles);
    assert!(d.src.is_active(NetMacro::Drop) && !before.is_empty());
    // a new song is handed over; it is installed at the next bar line (16 ticks = 0.8 s), and
    // until then the running graph is the old one
    d.src.load(prepare(&project("house", 7.0, "loop"), rate, 0, None));
    d.run(3);
    assert!(
        d.src.is_active(NetMacro::Drop),
        "the old graph still runs its macro"
    );
    let roles = d.src.roles();
    assert!(
        d.src.roles_held(NetMacro::Drop, &roles).is_empty(),
        "its net numbers are not looked up in the new song's role table"
    );
    // at the bar line the new song starts clean: no macro, nothing held, and the chase has gone
    d.run(200);
    assert!(d.src.active_macros().is_empty());
    let roles = d.src.roles();
    assert!(d.src.roles_held_by_macros(&roles).is_empty());
    // and a macro fired on the new song is chased by its own roles again
    d.src.fire(NetMacro::Drop, Some(16));
    d.run(3);
    let roles = d.src.roles();
    assert!(!d.src.roles_held(NetMacro::Drop, &roles).is_empty());
}

// MacroMirrorTests.theSelectorIsTheRuleFireMuteUses
#[test]
fn the_selector_is_the_rule_fire_mute_uses() {
    // `selects` is what `fireMute` mutes by: the nets muted equal the nets selected that were sounding
    for which in [
        MacroTargets::NonDrums,
        MacroTargets::DrumsOnly,
        MacroTargets::Everything,
        MacroTargets::NonKick,
    ] {
        let mut g = WaveGraph::new(&project("techno", 42.0, "loop"));
        let before: Vec<bool> = g.nets.iter().map(|n| g.muted[n.index] != 0).collect();
        g.fire_mute(which, 4, 0);
        for n in &g.nets {
            let muted = g.muted[n.index] != 0;
            let expected = before[n.index] || g.macro_selects(which, n.index);
            assert_eq!(muted, expected, "{which:?} net {}", n.id);
        }
        assert!(!g
            .nets
            .iter()
            .any(|n| g.is_control_net_at(n.index) && g.macro_selects(which, n.index)));
    }
}

// MacroMirrorTests.theTimerTagsRideThroughTheRestoreAndTheDelayedHalf
#[test]
fn the_timer_tags_ride_through_the_restore_and_the_delayed_half() {
    let mut g = WaveGraph::new(&project("techno", 42.0, "loop"));
    fn holders(g: &WaveGraph, s: &MacroSnapshot, m: Option<NetMacro>) -> Vec<usize> {
        g.nets
            .iter()
            .filter(|n| match m {
                Some(m) => s.is_held_by(n.index as i64, m),
                None => s.is_held(n.index as i64),
            })
            .map(|n| n.index)
            .collect()
    }
    g.fire_mute(MacroTargets::NonDrums, 2, NetMacro::Drop.tag());
    g.fire_mute(MacroTargets::DrumsOnly, 2, 0); // untagged: not a macro's
    let mut s = g.macro_snapshot();
    assert_eq!(s.mask, NetMacro::Drop.bit());
    let held = holders(&g, &s, Some(NetMacro::Drop));
    assert!(
        !held.is_empty() && held.iter().all(|&i| !g.is_drum_net_at(i)),
        "only the tagged mutes"
    );
    assert_eq!(
        holders(&g, &s, None).len(),
        held.len(),
        "and nobody else holds anything"
    );
    g.schedule_mute(MacroTargets::NonDrums, 4, 3, NetMacro::DoubleDrop.tag());
    s = g.macro_snapshot();
    assert_eq!(
        s.mask,
        NetMacro::Drop.bit() | NetMacro::DoubleDrop.bit(),
        "the delayed half is tagged too"
    );
    for _ in 0..2 {
        g.advance(); // the first drop restores; the delayed half is still waiting
    }
    s = g.macro_snapshot();
    assert_eq!(
        s.mask,
        NetMacro::DoubleDrop.bit(),
        "a waiting timer keeps its macro in effect"
    );
    assert!(holders(&g, &s, None).is_empty(), "and holds nothing yet");
    for _ in 0..2 {
        g.advance(); // it fires: the tag rides on the mutes it makes
    }
    s = g.macro_snapshot();
    assert_eq!(s.mask, NetMacro::DoubleDrop.bit());
    assert!(
        !holders(&g, &s, Some(NetMacro::DoubleDrop)).is_empty()
            && holders(&g, &s, Some(NetMacro::Drop)).is_empty()
    );
    for _ in 0..3 {
        g.advance();
    }
    assert!(g.macro_snapshot().mask == 0 && g.pending_macro_timers() == 0);
}

// MacroMirrorTests.aNetIsHeldByTheMacroThatMutedItAndByNoOther
#[test]
fn a_net_is_held_by_the_macro_that_muted_it_and_by_no_other() {
    let mut g = WaveGraph::new(&project("techno", 42.0, "loop"));
    let silent: Vec<bool> = g.nets.iter().map(|n| g.muted[n.index] != 0).collect(); // muted from the start (stingers...)
    g.fire_mute(MacroTargets::NonDrums, 8, NetMacro::Drop.tag());
    // a cut over it: everything not yet muted, i.e. the drums only
    g.fire_mute(MacroTargets::Everything, 4, NetMacro::Cut.tag());
    let s = g.macro_snapshot();
    let (mut owned_drums, mut owned_others) = (0, 0);
    for n in g.nets.iter().filter(|n| !g.is_control_net_at(n.index)) {
        let i = n.index as i64;
        let drop = s.is_held_by(i, NetMacro::Drop);
        let cut = s.is_held_by(i, NetMacro::Cut);
        if silent[n.index] {
            assert!(
                !drop && !cut,
                "net {} was muted already: nobody's to restore",
                n.id
            );
            continue;
        }
        if g.is_drum_net_at(n.index) {
            owned_drums += 1;
        } else {
            owned_others += 1;
        }
        assert!(drop != cut, "net {} has exactly one owner", n.id);
        assert!(
            drop == !g.is_drum_net_at(n.index) && cut == g.is_drum_net_at(n.index),
            "net {}",
            n.id
        );
    }
    assert!(
        owned_drums >= 1 && owned_others >= 1,
        "both macros muted something: {owned_drums}/{owned_others}"
    );
    assert_eq!(s.mask, NetMacro::Drop.bit() | NetMacro::Cut.bit());
    // the ownership words are the macro's own: another macro's slot is empty
    assert!(!g
        .nets
        .iter()
        .any(|n| s.is_held_by(n.index as i64, NetMacro::Breakdown)));
}

// MacroMirrorTests.theRecordCoversTheFirstTwoHundredFiftySixNetsAndOnlyThose
#[test]
fn the_record_covers_the_first_256_nets_and_only_those() {
    let mut s = MacroSnapshot::default();
    s.hold(0, 0);
    s.hold(255, 8);
    s.hold(256, 0); // past the table: ignored
    s.hold(-1, 0);
    s.hold(3, 16); // past the room for macros: ignored
    assert!(s.is_held_by(0, NetMacro::Drop) && s.is_held_by(255, NetMacro::TempoAnchor));
    assert!(!s.is_held(256) && !s.is_held(-1));
    assert!(!s.is_held_by(255, NetMacro::Drop) && !s.is_held_by(0, NetMacro::TempoAnchor));
    assert!(!s.is_held(3));
    assert!(MacroSnapshot::CAPACITY >= NetMacro::ALL.len());
    // the bit packing itself: macro `k`, net `i` is bit `i % 64` of word `4k + i / 64`
    assert_eq!(s.held[0], 1);
    assert_eq!(s.held[8 * 4 + 3], 1 << 63);
    assert_eq!(s.held.iter().filter(|&&w| w != 0).count(), 2);
}

// MacroMirrorTests.theTagIsTheRawValuePlusOne
#[test]
fn the_tag_is_the_raw_value_plus_one() {
    for (k, m) in NetMacro::ALL.iter().enumerate() {
        assert!(m.tag() as usize == k + 1 && m.bit() == 1u32 << k);
    }
    assert!(NetMacro::ALL.len() <= 32, "the mask is 32 bits");
}

// ---- MacroMirrorStressTests ------------------------------------------------------------------

#[derive(Default, Debug)]
struct MacroStressReport {
    reads: usize,
    failed_reads: usize,
    torn: usize,
    backwards: usize,
    changes_seen: usize,
}

/// The record number `n` as the writer publishes it: the mask is `n`, the generation is `n`, and
/// word `k` is `n * C + k`.
fn word(n: i64, k: usize) -> u64 {
    (n as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(k as u64)
}

fn stress_macro_mirror(publishes: i64) -> MacroStressReport {
    let mirror = Arc::new(MacroMirror::new());
    let done = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gate::default());
    let words = NetMacro::ALL.len() * MacroSnapshot::WORDS_PER_MACRO;
    let writer = {
        let (mirror, done, gate) = (mirror.clone(), done.clone(), gate.clone());
        std::thread::spawn(move || {
            for n in 1..=publishes {
                gate.writer_wait(n);
                let mut s = MacroSnapshot {
                    mask: n as u32,
                    ..Default::default()
                };
                for k in 0..words {
                    s.held[k] = word(n, k);
                }
                mirror.publish(&s, n as i32);
            }
            done.store(true, Ordering::Release);
        })
    };
    let mut report = MacroStressReport::default();
    let mut last = 0i64;
    loop {
        let finished = done.load(Ordering::Acquire);
        let Some(r) = mirror.read() else {
            report.failed_reads += 1;
            if finished {
                break;
            }
            continue;
        };
        if r.generation != 0 {
            gate.reader_saw(r.generation as i64);
            // 0 is the state before the first publish
            report.reads += 1;
            let n = r.generation as i64;
            let mut ok = r.snapshot.mask == n as u32;
            for k in 0..words {
                if r.snapshot.held[k] != word(n, k) {
                    ok = false;
                }
            }
            if !ok {
                report.torn += 1;
            }
            if n < last {
                report.backwards += 1;
            }
            if n != last {
                report.changes_seen += 1;
            }
            last = n;
        }
        if finished {
            break;
        }
    }
    writer.join().unwrap();
    report
}

// MacroMirrorStressTests.aReaderNeverSeesHalfOfOneBlocksRecordAndHalfOfAnother
#[test]
fn a_reader_never_sees_half_of_one_blocks_record_and_half_of_another() {
    let report = stress_macro_mirror(150_000);
    eprintln!("macro mirror stress: {report:?}");
    assert!(
        report.reads > 1000,
        "the reader got records: {}",
        report.reads
    );
    assert_eq!(
        report.torn, 0,
        "torn reads: {} of {}",
        report.torn, report.reads
    );
    assert_eq!(report.backwards, 0, "a later read returned an older record");
    assert!(
        report.changes_seen > 50,
        "and it saw the writer move: {}",
        report.changes_seen
    );
}
