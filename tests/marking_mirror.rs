//! `MarkingLayout` and `MarkingMirror`: the audio thread publishes every net's tokens and
//! mutes, the main thread reads them without locks and without ever seeing half of one state
//! and half of another. Port of `MarkingMirrorTests.swift` and `MarkingStress.swift`: the same
//! literals, driven through the `Source` harness (a bare `WaveDeckSource` split into its audio
//! and main sides). Form (a) of the parity rules: the Swift suite's own numbers. The
//! Swift-generated snapshot words (form b) are in `tests/mirrors_golden.rs`.
//!
//! Two Swift tests reach above `WaveDeckSource`: `aDecksSectionsFlipAtLoadButTheSoundingLayout
//! KeepsItsOwnUntilTheBar` runs a `DJEngine` deck; here the same facts are asserted on the
//! source (`SectionInfo::sections_of` of the project loaded stands in for `Deck.sections`).

mod mirror_support;

use std::sync::Arc;

use djrust::deck::timeline::SectionInfo;
use djrust::wave::*;
use mirror_support::stress::*;
use mirror_support::*;

const SR: f64 = 22_050.0;

/// `source(p, tempo:, startTick:)`: loaded, tempo set, not playing.
fn source(p: &djrust::Project, tempo: f64, start_tick: i64) -> (Source, Loaded) {
    let mut src = Source::new(SR);
    let loaded = src.load(prepare(p, SR, start_tick, None));
    src.set_tempo(tempo);
    (src, loaded)
}

fn run(src: &mut Source, from: i64, blocks: usize) -> i64 {
    let mut frame = from;
    for _ in 0..blocks {
        src.render(128, frame);
        frame += 128;
    }
    frame
}

// ---- MarkingLayoutTests ----------------------------------------------------------------------

// MarkingLayoutTests.theLayoutNamesEveryNetAndPlaceInOrder
#[test]
fn the_layout_names_every_net_and_place_in_order() {
    let prepared = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    let layout = &prepared.mirror.layout;
    let g = &prepared.project.graph;
    assert_eq!(layout.generation, prepared.generation);
    let ids: Vec<&String> = g.nets.iter().map(|n| &n.id).collect();
    assert_eq!(layout.net_ids.iter().collect::<Vec<_>>(), ids);
    assert!(layout.net_count() == g.nets.len() && layout.net_count() > 5);
    assert!(layout.place_offsets.len() == layout.net_count() + 1 && layout.place_offsets[0] == 0);
    let mut total = 0;
    for (i, n) in g.nets.iter().enumerate() {
        assert_eq!(layout.place_ids[i], n.place_ids);
        assert_eq!(layout.places_of_net(i), total..total + n.n_places);
        total += n.n_places;
    }
    assert!(layout.place_count() == total && total > 50);
}

// MarkingLayoutTests.everyPreparedProjectGetsItsOwnIncreasingGeneration
#[test]
fn every_prepared_project_gets_its_own_increasing_generation() {
    let a = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    let b = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    assert!(a.generation > 0 && b.generation > a.generation);
    assert!(!Arc::ptr_eq(&a.mirror, &b.mirror));
}

// MarkingLayoutIdentityTests.everyNewProjectIsANewTrackUnlessToldItIsTheSameSong
#[test]
fn every_new_project_is_a_new_track_unless_told_it_is_the_same_song() {
    let p = project("techno", 42.0, "standard");
    let a = prepare(&p, SR, 0, None);
    let b = prepare(&p, SR, 0, None);
    assert!(
        a.track_serial > 0 && b.track_serial > a.track_serial,
        "a new prepare is a new track"
    );
    let again = prepare(&p, SR, 200, Some(a.track_serial));
    assert_eq!(
        again.track_serial, a.track_serial,
        "a relaunch of the same song keeps its serial"
    );
    assert!(
        again.generation > b.generation,
        "but is a new project: tick numbers start over"
    );
    assert!(a.start_tick == 0 && again.start_tick == 200);
    assert_eq!(prepare(&p, SR, -5, None).start_tick, 0);
    // the layout carries both, so a consumer holding only an event's generation can tell
    let layout = &again.mirror.layout;
    assert!(layout.generation == again.generation && layout.track_serial == a.track_serial);
    assert_eq!(layout.start_tick, 200);
    assert_ne!(
        a.mirror.layout.track_serial,
        b.mirror.layout.track_serial
    );
}

// MarkingLayoutIdentityTests.theLayoutDescribesEveryNetForDisplay
#[test]
fn the_layout_describes_every_net_for_display() {
    let p = project("techno", 42.0, "standard");
    let prepared = prepare(&p, SR, 0, None);
    let layout = &prepared.mirror.layout;
    let g = &prepared.project.graph;
    assert!(layout.nets.len() == g.nets.len() && layout.nets.len() == layout.net_count());
    let (mut drums, mut tonal, mut stingers, mut controls) = (0, 0, 0, 0);
    for (i, n) in g.nets.iter().enumerate() {
        let info = &layout.nets[i];
        assert!(info.id == n.id && info.id == layout.net_ids[i]);
        assert!(info.role == n.bundle.role && info.riff_group == n.bundle.riff_group);
        assert!(info.instrument == n.bundle.track.instrument && info.group == n.bundle.track.group);
        assert_eq!(info.channel, n.bundle.track.channel);
        assert_eq!(info.is_drum, is_drum_channel(n.bundle.track.channel));
        assert_eq!(info.is_stinger, g.is_stinger_net_at(i));
        assert_eq!(info.transition_ids, n.trans_ids);
        assert!(info.place_x.len() == n.n_places && info.place_y.len() == n.n_places);
        assert!(info.transition_x.len() == n.n_trans && info.transition_y.len() == n.n_trans);
        let voiced: Vec<i64> = (0..n.n_trans)
            .filter(|&t| n.lane[t] >= 0)
            .map(|t| n.note[t])
            .collect();
        if voiced.is_empty() {
            assert!(
                info.note_low == -1 && info.note_high == -1,
                "{} never sounds",
                n.id
            );
            if info.role == "control" {
                controls += 1;
            }
        } else {
            assert!(info.note_low == *voiced.iter().min().unwrap());
            assert!(info.note_high == *voiced.iter().max().unwrap());
            assert!(info.note_low <= info.note_high);
            if info.is_drum {
                drums += 1;
            } else {
                tonal += 1;
            }
        }
        if info.is_stinger {
            stingers += 1;
        }
        // positions are the composed ring layout, in place / transition order
        let composed = p.nets.get(&n.id).unwrap();
        for (k, id) in n.place_ids.iter().enumerate() {
            let place = composed.places.get(id).unwrap();
            assert!(info.place_x[k] == place.x && info.place_y[k] == place.y);
        }
        for (k, id) in n.trans_ids.iter().enumerate() {
            let t = composed.transitions.get(id).unwrap();
            assert!(info.transition_x[k] == t.x && info.transition_y[k] == t.y);
        }
        // the extras the Swift suite does not assert but the layout promises
        assert_eq!(info.transition_has_midi, n.midi);
        assert_eq!(info.arc_count, composed.arcs.len());
        assert!(info.arcs.len() <= info.arc_count);
        assert!(
            info.role != "control" || info.arcs.is_empty(),
            "the stage never draws a control net"
        );
    }
    assert!(
        drums > 2 && tonal > 2 && stingers > 0 && controls > 0,
        "a real song: all kinds"
    );
    assert!(
        layout.nets.iter().any(|n| n.place_x.iter().any(|&x| x != 0.0)
            && n.transition_y.iter().any(|&y| y != 0.0)),
        "the ring layout positions are in there"
    );
}

// MarkingLayoutIdentityTests.theLayoutKnowsTheSectionsOfItsOwnProject
#[test]
fn the_layout_knows_the_sections_of_its_own_project() {
    let song = prepare(&project("techno", 42.0, "standard"), SR, 0, None);
    let sections = &song.mirror.layout.sections;
    assert!(sections.len() >= 5 && sections[0].start_tick == 0);
    assert!(sections.iter().map(|s| s.steps).sum::<i64>() > 600);
    assert!(sections
        .windows(2)
        .all(|w| w[1].start_tick == w[0].start_tick + w[0].steps));
    assert_eq!(
        *sections,
        SectionInfo::sections_of(&project("techno", 42.0, "standard"))
    );
    assert!(prepare(&project("techno", 42.0, "loop"), SR, 0, None)
        .mirror
        .layout
        .sections
        .is_empty());
}

// MarkingLayoutIdentityTests.aDecksSectionsFlipAtLoadButTheSoundingLayoutKeepsItsOwnUntilTheBar
// (on the source: `Deck.sections` is `SectionInfo.sections(of:)` of the project loaded)
#[test]
fn a_decks_sections_flip_at_load_but_the_sounding_layout_keeps_its_own_until_the_bar() {
    let mut src = Source::new(SR);
    let first = src.load(prepare(&project("techno", 42.0, "standard"), SR, 0, None));
    src.set_tempo(600.0);
    src.play();
    let mut frame = run(&mut src, 0, 64); // 8192 frames
    let next = project("ambient", 5.0, "short");
    src.load(prepare(&next, SR, 0, None)); // queued behind the playing song
    let deck_sections = SectionInfo::sections_of(&next);
    assert_ne!(
        deck_sections, first.layout.sections,
        "the deck already shows the new song"
    );
    let sounding = src.marking_layout_of(src.active_generation()).unwrap();
    assert_eq!(sounding.generation, first.generation);
    assert_eq!(
        sounding.sections, first.layout.sections,
        "the sounding one is still the first"
    );
    frame = run(&mut src, frame, 64 * 4); // past the bar
    let _ = frame;
    assert_eq!(src.active_generation(), src.latest_generation().unwrap());
    let now = src.marking_layout_of(src.active_generation()).unwrap();
    assert_eq!(now.sections, deck_sections);
}

// MarkingLayoutIdentityTests.everyFireResolvesToItsNetThroughTheLayoutOfItsGeneration
#[test]
fn every_fire_resolves_to_its_net_through_the_layout_of_its_generation() {
    let mut src = Source::new(SR);
    src.audio.runner.record_events = true;
    let first = src.load(prepare(&project("techno", 42.0, "loop"), SR, 0, None));
    src.set_tempo(600.0);
    src.play();
    let mut frame = run(&mut src, 0, 200);
    let second = prepare(&project("house", 4.0, "loop"), SR, 0, None);
    let third = prepare(&project("dnb", 3.0, "loop"), SR, 0, None);
    let third_generation = third.generation;
    src.load(second);
    src.load(third); // replaces `second` while it waits for the bar: its events never come
    frame = run(&mut src, frame, 600);
    let _ = frame;
    let fires = std::mem::take(&mut src.audio.runner.fires);
    assert!(fires.len() > 100);
    let mut generations: Vec<i32> = fires.iter().map(|f| f.generation).collect();
    generations.sort();
    generations.dedup();
    let mut want = vec![first.generation, third_generation];
    want.sort();
    assert_eq!(generations, want);
    for f in &fires {
        let layout = src.marking_layout_of(f.generation);
        assert!(layout.is_some(), "generation {} resolves", f.generation);
        let layout = layout.unwrap();
        let net = &layout.nets[f.net as usize];
        assert!((f.transition as usize) < net.transition_ids.len());
        assert!(f.note as i64 >= net.note_low && f.note as i64 <= net.note_high);
        assert_eq!(f.flags.contains(FireFlags::DRUM), net.is_drum);
        assert_eq!(f.flags.contains(FireFlags::STINGER), net.is_stinger);
        assert_ne!(net.role, "control", "control nets never fire");
    }
}

// ---- MarkingMirrorTests ----------------------------------------------------------------------

// MarkingMirrorTests.aFreshProjectIsReadableBeforeAnythingRenders
#[test]
fn a_fresh_project_is_readable_before_anything_renders() {
    let (src, loaded) = source(&project("techno", 42.0, "loop"), 300.0, 0);
    let mut snap = MarkingSnapshot::new();
    assert_eq!(src.active_generation(), 0, "the audio thread has not run it");
    assert!(src.read_marking(&mut snap));
    assert!(snap.generation() == loaded.generation && snap.header.sequence == 1);
    assert!(snap.tick() == 0 && snap.header.frame == 0);
    assert!(matches(&snap, &loaded.graph0));
    assert!(
        snap.tokens.iter().any(|&t| t > 0.0),
        "the initial marking has tokens in it"
    );
}

// MarkingMirrorTests.aSeekedProjectIsSeededWithItsRebuiltMarking
#[test]
fn a_seeked_project_is_seeded_with_its_rebuilt_marking() {
    let (src, loaded) = source(&project("techno", 42.0, "standard"), 300.0, 200);
    let mut snap = MarkingSnapshot::new();
    assert!(src.read_marking(&mut snap));
    assert!(snap.tick() == 200 && matches(&snap, &loaded.graph0));
    let fresh = prepare(&project("techno", 42.0, "standard"), SR, 0, None);
    let mut other = MarkingSnapshot::new();
    assert!(fresh.mirror.read_into(&mut other));
    assert_ne!(other.tokens, snap.tokens, "tick 200 is not the starting marking");
}

// MarkingMirrorTests.nothingLoadedMeansNoSnapshot
#[test]
fn nothing_loaded_means_no_snapshot() {
    let src = Source::new(SR);
    let mut snap = MarkingSnapshot::new();
    assert!(!src.read_marking(&mut snap));
    assert!(!src.read_marking_of(99, &mut snap));
    assert!(src.marking_layout().is_none() && src.marking_layout_of(1).is_none());
}

// MarkingMirrorTests.theSnapshotFollowsTheGraphBlockByBlock
#[test]
fn the_snapshot_follows_the_graph_block_by_block() {
    let (mut src, loaded) = source(&project("house", 7.0, "loop"), 900.0, 0);
    src.play();
    let mut snap = MarkingSnapshot::new();
    let mut frame = 0;
    let (mut changes, mut last_seq) = (0, 0);
    for _ in 0..500 {
        frame = run(&mut src, frame, 1);
        assert!(src.read_marking(&mut snap));
        assert!(matches(&snap, src.graph()), "tick {}", src.graph().tick);
        assert!(snap.header.frame <= frame && snap.generation() == loaded.generation);
        if snap.header.sequence != last_seq {
            changes += 1;
        }
        last_seq = snap.header.sequence;
    }
    assert_eq!(src.active_generation(), loaded.generation);
    assert!(
        src.graph().tick > 80 && changes > 80,
        "ticks went by and were published"
    );
}

// MarkingMirrorTests.afterNTicksTheSnapshotEqualsMarkingOnTheStoppedRunner
#[test]
fn after_n_ticks_the_snapshot_equals_marking_on_the_stopped_runner() {
    let (mut src, _) = source(&project("dnb", 3.0, "loop"), 600.0, 0);
    src.play();
    run(&mut src, 0, 800);
    src.pause();
    run(&mut src, 800 * 128, 4); // the pause lands; nothing moves
    let mut snap = MarkingSnapshot::new();
    assert!(src.read_marking(&mut snap));
    assert!(snap.tick() > 100 && snap.tick() == src.tick());
    let truth = src.graph().marking(); // allocating: fine here, the audio thread is idle
    assert_eq!(truth.len(), snap.layout.as_ref().unwrap().net_count());
    for (net_id, places) in &truth {
        for (place_id, tokens) in places {
            assert_eq!(
                snap.token_at(net_id, place_id),
                Some(*tokens),
                "{net_id}.{place_id}"
            );
        }
    }
    assert_eq!(snap.token_at("no-such-net", "x"), None);
}

// MarkingMirrorTests.theSequenceMovesOnlyWhenSomethingChanges
#[test]
fn the_sequence_moves_only_when_something_changes() {
    let (mut src, _) = source(&project("techno", 42.0, "loop"), 300.0, 0);
    let mut snap = MarkingSnapshot::new();
    src.play();
    let mut frame = run(&mut src, 0, 60);
    src.pause();
    frame = run(&mut src, frame, 3);
    src.read_marking(&mut snap);
    let seq = snap.header.sequence;
    frame = run(&mut src, frame, 200); // paused: nothing changes
    src.read_marking(&mut snap);
    assert_eq!(snap.header.sequence, seq, "no tick, no mute change, no publish");
    // a mute changes the snapshot without any tick
    let kick = src.graph().by_id["kick"];
    assert_eq!(snap.muted[kick], 0);
    src.set_mute("kick", true);
    frame = run(&mut src, frame, 2);
    src.read_marking(&mut snap);
    assert!(snap.header.sequence == seq + 1 && snap.muted[kick] == 1 && snap.is_muted(kick));
    assert!(matches(&snap, src.graph()));
    src.set_role_muted("kick", false);
    frame = run(&mut src, frame, 2);
    let _ = frame;
    src.read_marking(&mut snap);
    assert!(snap.header.sequence == seq + 2 && snap.muted[kick] == 0);
}

// MarkingMirrorTests.macrosShowUpAsMutes
#[test]
fn macros_show_up_as_mutes() {
    let (mut src, _) = source(&project("techno", 42.0, "loop"), 300.0, 0);
    src.play();
    let mut frame = run(&mut src, 0, 40);
    src.fire(NetMacro::Drop, Some(8));
    frame = run(&mut src, frame, 2);
    let mut snap = MarkingSnapshot::new();
    src.read_marking(&mut snap);
    assert!(matches(&snap, src.graph()));
    assert!(snap.muted.contains(&1), "the drop muted the non-drums");
    frame = run(&mut src, frame, 400);
    let _ = frame;
    src.read_marking(&mut snap);
    assert!(matches(&snap, src.graph()));
}

// MarkingMirrorTests.scrubbingMovesTheMirrorToo
#[test]
fn scrubbing_moves_the_mirror_too() {
    let (mut src, _) = source(&project("techno", 42.0, "loop"), 300.0, 0);
    src.play();
    let mut frame = run(&mut src, 0, 80);
    let mut snap = MarkingSnapshot::new();
    src.freeze();
    src.scrub(10.0);
    frame = run(&mut src, frame, 120);
    src.read_marking(&mut snap);
    assert!(
        matches(&snap, src.graph()) && snap.tick() > 10,
        "scratching forward is published"
    );
    let forward = snap.tick();
    src.scrub(-6.0);
    frame = run(&mut src, frame, 120);
    let _ = frame;
    src.read_marking(&mut snap);
    assert!(
        matches(&snap, src.graph()) && snap.tick() < forward,
        "and backwards"
    );
}

// MarkingMirrorTests.aSwapSwitchesGenerationAndLayoutTogether
#[test]
fn a_swap_switches_generation_and_layout_together() {
    let a = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    let b = prepare(&project("ambient", 5.0, "loop"), SR, 0, None);
    assert_ne!(
        a.mirror.layout.place_count(),
        b.mirror.layout.place_count(),
        "different shapes"
    );
    let mut src = Source::new(SR);
    let la = src.load(a);
    src.set_tempo(600.0);
    src.play();
    let mut frame = run(&mut src, 0, 100);
    let lb = src.load(b); // while playing: takes over at the next bar
    assert_eq!(
        src.marking_layout().unwrap().generation,
        lb.generation,
        "the latest layout is already B's"
    );
    let mut snap = MarkingSnapshot::new();
    let (mut saw_a, mut saw_b, mut swapped) = (0, 0, false);
    let mut last_a: Option<MarkingSnapshot> = None;
    for _ in 0..400 {
        frame = run(&mut src, frame, 1);
        assert!(src.read_marking(&mut snap));
        let layout = snap.layout.clone().unwrap();
        assert_eq!(snap.generation(), layout.generation);
        assert!(snap.tokens.len() == layout.place_count() && snap.muted.len() == layout.net_count());
        if snap.generation() == la.generation {
            saw_a += 1;
            assert!(!swapped, "once B has taken over A does not come back");
            assert!(matches(&snap, src.graph()));
            assert_eq!(src.active_generation(), la.generation);
            assert!(
                Arc::ptr_eq(&src.marking_layout_of(la.generation).unwrap(), &layout),
                "A's layout is still available while A runs"
            );
            last_a = Some(snap.clone());
        } else {
            swapped = true;
            saw_b += 1;
            assert!(snap.generation() == lb.generation && matches(&snap, src.graph()));
            assert_eq!(src.active_generation(), lb.generation);
        }
    }
    assert!(saw_a > 0 && saw_b > 0, "saw both (A {saw_a} times, B {saw_b})");
    assert!(
        src.marking_layout_of(la.generation).is_some(),
        "old layouts stay readable"
    );
    let mut old = MarkingSnapshot::new();
    assert!(src.read_marking_of(la.generation, &mut old));
    // Swift: `matches(old, a.graph)`. A's graph has moved into the runner's fading slot, so
    // compare with the last state read while A ran: A's graph stopped ticking at the swap, so
    // that is its last published state.
    let last_a = last_a.unwrap();
    assert!(old.generation() == la.generation, "A's last published state");
    assert!(old.header == last_a.header && old.tokens == last_a.tokens && old.muted == last_a.muted);
}

// MarkingMirrorTests.theActiveProjectsMirrorSurvivesAFloodOfLoads
#[test]
fn the_active_projects_mirror_survives_a_flood_of_loads() {
    let a = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    let mut src = Source::new(SR);
    let la = src.load(a);
    src.set_tempo(300.0);
    src.play();
    let frame = run(&mut src, 0, 40);
    assert_eq!(src.active_generation(), la.generation);
    let mut flood: Vec<i32> = Vec::new();
    for seed in 1..=20 {
        // queued behind A; each replaces the one waiting for the bar
        let l = src.load(prepare(&project("house", seed as f64, "loop"), SR, 0, None));
        flood.push(l.generation);
    }
    assert!(
        src.marking_layout_of(la.generation).is_some(),
        "A is running: never pruned"
    );
    // (beyond Swift's literals) the book itself is bounded: A and the newest seven stay, the
    // older loads are forgotten
    assert_eq!(src.book.len(), MarkingBook::KEPT);
    assert!(src.marking_layout_of(flood[0]).is_none());
    assert!(src.marking_layout_of(flood[12]).is_none());
    assert!(src.marking_layout_of(flood[13]).is_some());
    assert!(src.marking_layout_of(flood[19]).is_some());
    let mut snap = MarkingSnapshot::new();
    run(&mut src, frame, 1);
    assert!(src.read_marking(&mut snap) && snap.generation() == la.generation);
    assert!(src.marking_layout_of(9999).is_none());
}

// MarkingMirrorTests.tooSmallABufferIsRefused
#[test]
fn too_small_a_buffer_is_refused() {
    let prepared = prepare(&project("techno", 42.0, "loop"), SR, 0, None);
    let layout = &prepared.mirror.layout;
    let mut tokens = vec![0.0; layout.place_count() - 1];
    let mut muted = vec![0u8; layout.net_count()];
    assert_eq!(prepared.mirror.read(&mut tokens, &mut muted), None);
}

// MarkingMirrorTests.theSnapshotBufferIsReusedAcrossReads
#[test]
fn the_snapshot_buffer_is_reused_across_reads() {
    let (mut src, _) = source(&project("techno", 42.0, "loop"), 300.0, 0);
    src.play();
    let mut snap = MarkingSnapshot::new();
    run(&mut src, 0, 5);
    src.read_marking(&mut snap);
    let first = snap.tokens.as_ptr();
    run(&mut src, 5 * 128, 500);
    src.read_marking(&mut snap);
    let second = snap.tokens.as_ptr();
    assert_eq!(first, second, "no reallocation for the same layout");
}

// ---- MarkingMirrorThreadTests ----------------------------------------------------------------

fn mix_of_two_states(preempted: bool) {
    // preempted: the reader is held up halfway through each copy, so the writer laps it
    let report = stress_mirror(20_000, preempted);
    eprintln!("marking mirror stress (preempted {preempted}): {report:?}");
    assert_eq!(report.torn, 0, "{} snapshots mixed two states", report.torn);
    assert_eq!(report.backwards, 0);
    assert_eq!(report.published, 20_000);
    assert!(
        report.reads > 10 && report.changes_seen > 5,
        "the reader really saw the writer move"
    );
    if preempted {
        assert!(
            report.retried > 0,
            "the writer really did lap the reader copy: {report:?}"
        );
    }
}

// MarkingMirrorThreadTests.aReaderNeverSeesAMixOfTwoStates(preempted: false)
#[test]
fn a_reader_never_sees_a_mix_of_two_states() {
    mix_of_two_states(false);
}

// MarkingMirrorThreadTests.aReaderNeverSeesAMixOfTwoStates(preempted: true)
#[test]
fn a_reader_never_sees_a_mix_of_two_states_when_preempted() {
    mix_of_two_states(true);
}

// MarkingMirrorThreadTests.swapsUnderLoadNeverPairALayoutWithTheWrongTokens
#[test]
fn swaps_under_load_never_pair_a_layout_with_the_wrong_tokens() {
    let genres = [
        ("techno", 42.0),
        ("ambient", 5.0),
        ("house", 7.0),
        ("dnb", 3.0),
    ];
    let projects: Vec<Prepared> = (0..12)
        .map(|i| {
            let (g, s) = genres[i % genres.len()];
            prepare(&project(g, s + i as f64, "loop"), 8000.0, 0, None)
        })
        .collect();
    let mut shapes: Vec<usize> = projects
        .iter()
        .map(|p| p.mirror.layout.place_count())
        .collect();
    shapes.sort();
    shapes.dedup();
    assert!(shapes.len() >= 3, "varied shapes");
    let n = projects.len();
    let report = stress_engine_mirror(projects, 8000.0);
    eprintln!("engine mirror stress: {report:?}");
    assert_eq!(
        report.mismatched, 0,
        "a snapshot's arrays did not fit its own layout"
    );
    assert_eq!(report.unknown_layouts, 0);
    assert_eq!(report.backwards, 0);
    assert!(report.reads > 1000);
    assert_eq!(
        report.swaps_seen, n,
        "the audio thread swapped through every project under the reader ({})",
        report.swaps_seen
    );
    assert!(report.frames_rendered > 0);
}

// ---- MarkingBeforeTheFirstRenderTests --------------------------------------------------------

fn warm_source() -> (Source, Loaded, i64) {
    let mut src = Source::new(SR);
    let first = src.load(prepare(&project("techno", 42.0, "loop"), SR, 0, None));
    src.set_tempo(300.0);
    src.play();
    let frame = run(&mut src, 0, 60);
    (src, first, frame)
}

// MarkingBeforeTheFirstRenderTests.aStoppedDeckReportsTheProjectLoadedSinceItLastRendered
#[test]
fn a_stopped_deck_reports_the_project_loaded_since_it_last_rendered() {
    let (mut src, first, frame) = warm_source();
    src.pause();
    let after = run(&mut src, frame, 3);
    let mut snap = MarkingSnapshot::new();
    assert!(src.read_marking(&mut snap) && snap.generation() == first.generation);
    // a new song is loaded onto the paused deck; no block has run since
    let next = src.load(prepare(&project("house", 4.0, "loop"), SR, 0, None));
    assert_eq!(
        src.active_generation(),
        first.generation,
        "the audio thread has not seen it yet"
    );
    assert!(src.read_marking(&mut snap), "...but it is what will sound");
    assert!(snap.generation() == next.generation && snap.header.sequence == 1);
    assert!(matches(&snap, &next.graph0) && snap.tick() == 0);
    run(&mut src, after, 1);
    assert_eq!(src.active_generation(), next.generation);
    assert!(src.read_marking(&mut snap) && snap.generation() == next.generation);
}

// MarkingBeforeTheFirstRenderTests.aPlayingDeckKeepsReportingTheSoundingProjectUntilTheBar
#[test]
fn a_playing_deck_keeps_reporting_the_sounding_project_until_the_bar() {
    let (mut src, first, frame) = warm_source();
    src.load(prepare(&project("house", 4.0, "loop"), SR, 0, None));
    let mut snap = MarkingSnapshot::new();
    assert!(src.read_marking(&mut snap) && snap.generation() == first.generation);
    let after = run(&mut src, frame, 1); // queued behind the playing one
    assert!(src.read_marking(&mut snap) && snap.generation() == first.generation);
    // pause before the bar: it stays queued, and the sounding one is still the one reported
    src.pause();
    run(&mut src, after, 3);
    assert!(src.read_marking(&mut snap) && snap.generation() == first.generation);
    assert_eq!(src.active_generation(), first.generation);
}
