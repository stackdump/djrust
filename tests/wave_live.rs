//! Live-performance behaviour of the wave engine, mirroring `WaveHistoryTests.swift` and
//! `FireEventTests.swift`. Golden: none (dj-only behaviour: the Swift suites' own
//! literal expectations, against a second copy of the graph advanced by hand).

use djrust::wave::*;
use djrust::{compose, JsonObject, Project};

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        if s != "loop" {
            ov.set("structure", s.into());
        }
    }
    compose(genre, &ov.into())
}

// ---- WaveHistoryTests ------------------------------------------------------

#[derive(PartialEq, Debug)]
struct Snap {
    tick: i64,
    states: Vec<Vec<f64>>,
    muted: Vec<u8>,
    groups: Vec<u8>,
    repeater: (i64, i64),
    pending: usize,
}

fn snap(g: &WaveGraph) -> Snap {
    Snap {
        tick: g.tick,
        states: g.nets.iter().map(|n| n.state.clone()).collect(),
        muted: g.muted.clone(),
        groups: g.muted_groups.clone(),
        repeater: g.repeater_state(),
        pending: g.pending_macro_timers(),
    }
}

#[test]
fn rewinding_restores_every_tick_exactly() {
    for (genre, seed, structure) in [
        ("techno", 42.0, "standard"),
        ("jazz", 7.0, "extended"),
        ("edm", 11.0, "standard"),
        ("ambient", 3.0, "minimal"),
    ] {
        let mut g = WaveGraph::new(&project(genre, seed, Some(structure)));
        let mut forward = vec![snap(&g)];
        for _ in 0..480 {
            g.advance();
            forward.push(snap(&g));
        }
        assert_eq!(g.rewindable_ticks(), 480);
        for want in forward[..forward.len() - 1].iter().rev() {
            assert!(g.unadvance(|_, _| {}));
            assert_eq!(&snap(&g), want, "{genre} tick {}", want.tick);
        }
        assert!(!g.unadvance(|_, _| {}), "nothing before tick 0");
        // and forward again reproduces the same history (deterministic conflict resolution)
        for want in &forward[1..] {
            g.advance();
            assert_eq!(&snap(&g), want, "{genre} replay tick {}", want.tick);
        }
    }
}

#[test]
fn the_log_is_bounded_and_the_newest_ticks_stay_rewindable() {
    let mut g = WaveGraph::new(&project("techno", 42.0, Some("extended")));
    let mut history = std::collections::BTreeMap::new();
    for _ in 0..1100 {
        g.advance();
        history.insert(g.tick, snap(&g));
    }
    let depth = g.rewindable_ticks();
    assert!(depth > 100 && depth <= WaveGraph::HISTORY_TICKS);
    let mut undone = 0;
    while g.unadvance(|_, _| {}) {
        undone += 1;
        assert_eq!(&snap(&g), &history[&g.tick], "tick {}", g.tick);
    }
    assert_eq!(undone, depth);
}

#[test]
fn undoing_ticks_reports_the_notes_they_played() {
    let mut g = WaveGraph::new(&project("techno", 42.0, None));
    let mut played: Vec<Vec<String>> = Vec::new();
    for _ in 0..32 {
        let mut hits = Vec::new();
        g.advance_with(|n, t| hits.push(format!("{}.{}", n.id, n.trans_ids[t])));
        played.push(hits);
    }
    assert!(played.iter().any(|h| !h.is_empty()));
    for expected in played.iter().rev() {
        let mut hits = Vec::new();
        g.unadvance(|n, t| hits.push(format!("{}.{}", n.id, n.trans_ids[t])));
        let mut a = hits.clone();
        let mut b = expected.clone();
        a.sort();
        b.sort();
        assert_eq!(a, b, "the same notes, undone");
    }
}

#[test]
fn a_macro_fired_just_before_a_tick_is_undone_with_it() {
    let mut g = WaveGraph::new(&project("techno", 42.0, None));
    for _ in 0..10 {
        g.advance();
    }
    let before = snap(&g);
    g.fire_mute(MacroTargets::NonDrums, 12, 0);
    g.fire_beat_repeat(8, 2, 1);
    for _ in 0..30 {
        g.advance();
    }
    for _ in 0..30 {
        g.unadvance(|_, _| {});
    }
    assert_eq!(snap(&g), before);
}

#[test]
fn user_mutes_survive_a_rewind() {
    let mut g = WaveGraph::new(&project("techno", 42.0, None));
    for _ in 0..20 {
        g.advance();
    }
    g.set_role_muted(str_hash("kick"), true);
    for _ in 0..10 {
        g.advance();
    }
    for _ in 0..25 {
        g.unadvance(|_, _| {});
    }
    assert_eq!(
        g.muted[g.by_id["kick"]], 1,
        "a ring net has no control changes to undo"
    );
}

#[test]
fn stingers_and_fast_forward_start_a_fresh_log() {
    let mut g = WaveGraph::new(&project("jazz", 7.0, Some("standard")));
    g.fast_forward(200);
    assert!(g.tick == 200 && g.rewindable_ticks() == 0);
    for _ in 0..8 {
        g.advance();
    }
    assert_eq!(g.rewindable_ticks(), 8);
    g.fire_stinger(str_hash("hit1"), 4);
    for _ in 0..6 {
        g.advance();
    }
    for _ in 0..6 {
        g.unadvance(|_, _| {});
    }
    assert_eq!(
        g.muted[g.by_id["hit1"]], 1,
        "the stinger's unmute was undone"
    );
}

// ---- FireEventTests --------------------------------------------------------

const SR: f64 = 22_050.0;

fn make(p: &Project, tempo: f64, play: bool, start_tick: i64) -> WaveRunner {
    let opts = WaveOptions::default();
    let mut r = WaveRunner::new(SR, opts.clone());
    r.record_events = true;
    r.load_prepared(WaveRunner::prepare(p, SR, &opts, start_tick));
    r.set_tempo(tempo);
    if play {
        r.play();
    }
    r
}

/// Render `blocks` blocks of `size` frames at frame `from`; returns the frame after.
fn run(r: &mut WaveRunner, from: i64, blocks: usize, size: usize) -> i64 {
    let mut l = vec![0f32; size];
    let mut rr = vec![0f32; size];
    let mut frame = from;
    for _ in 0..blocks {
        r.process_at(&mut l, Some(&mut rr), size, frame);
        frame += size as i64;
    }
    frame
}

fn fires(r: &mut WaveRunner) -> Vec<FireEvent> {
    std::mem::take(&mut r.fires)
}
fn anchors(r: &mut WaveRunner) -> Vec<TickAnchor> {
    std::mem::take(&mut r.anchors)
}

#[derive(PartialEq, Debug)]
struct Hit {
    tick: i64,
    net: usize,
    transition: usize,
    note: i64,
    velocity: i64,
    channel: i64,
    dur_steps: f32,
    dur_ms: f32,
}

/// What the graph says is audible on each of the next `ticks` ticks, from a second copy
/// advanced by hand: the oracle for the event stream.
fn oracle(p: &Project, ticks: i64, tempo: f64, muting: &[&str]) -> Vec<Hit> {
    let mut prepared = WaveRunner::prepare(p, SR, &WaveOptions::default(), 0);
    let g = &mut prepared.graph;
    for id in muting {
        let i = g.by_id[*id];
        g.muted[i] = 1;
    }
    let mut out = Vec::new();
    for _ in 0..ticks {
        let tick_next = g.tick + 1;
        g.advance_with(|n, t| {
            if n.lane[t] < 0 {
                return;
            }
            let b = n.bundle.bindings.get(&n.trans_ids[t]).unwrap();
            let steps = n.dur_steps[t];
            out.push(Hit {
                tick: tick_next,
                net: n.index,
                transition: t,
                note: n.note[t],
                velocity: n.vel[t],
                channel: if b.channel != 0 {
                    b.channel
                } else {
                    n.bundle.track.channel
                },
                dur_steps: steps as f32,
                dur_ms: (if steps > 0.0 {
                    steps * 60000.0 / (tempo * 4.0)
                } else {
                    n.dur_ms[t]
                }) as f32,
            });
        });
    }
    out
}

fn hits(events: &[FireEvent]) -> Vec<Hit> {
    events
        .iter()
        .map(|e| Hit {
            tick: e.tick as i64,
            net: e.net as usize,
            transition: e.transition as usize,
            note: e.note as i64,
            velocity: e.velocity as i64,
            channel: e.channel as i64,
            dur_steps: e.dur_steps,
            dur_ms: e.dur_ms,
        })
        .collect()
}

fn per_tick(tempo: f64) -> f64 {
    SR * 60.0 / (tempo * 4.0)
}

fn tick(r: &WaveRunner) -> i64 {
    r.graph().unwrap().tick
}

#[test]
fn the_event_stream_is_exactly_the_audible_hits_of_the_graph() {
    let p = project("techno", 42.0, None);
    let mut r = make(&p, 300.0, true, 0);
    let end = run(&mut r, 0, 1600, 128);
    let got = fires(&mut r);
    let ticks_run = tick(&r);
    assert!(ticks_run > 100, "the run crosses plenty of ticks");
    let want = oracle(&p, ticks_run, 300.0, &[]);
    assert_eq!(hits(&got), want);
    assert!(want.len() > 100);
    assert_eq!(end, 1600 * 128);
}

#[test]
fn every_field_comes_from_the_binding_and_the_lane() {
    let p = project("techno", 42.0, None);
    let mut r = make(&p, 300.0, true, 0);
    run(&mut r, 0, 400, 128);
    let got = fires(&mut r);
    let proj = r.project().unwrap();
    let g = &proj.graph;
    let (mut drums, mut tonal) = (0, 0);
    for e in &got {
        assert_eq!(e.generation, proj.generation);
        let n = &g.nets[e.net as usize];
        let t = e.transition as usize;
        assert!(n.midi[t] && n.lane[t] >= 0, "only transitions with a voice");
        assert!(e.note as i64 == n.note[t] && e.velocity as i64 == n.vel[t]);
        assert!((1..=16).contains(&e.channel));
        assert_eq!(
            e.flags.contains(FireFlags::DRUM),
            is_drum_channel(e.channel as i64)
        );
        if e.flags.contains(FireFlags::DRUM) {
            drums += 1;
        } else {
            tonal += 1;
        }
    }
    assert!(
        drums > 10 && tonal > 5,
        "both kinds of lane show up ({drums} drums, {tonal} tonal)"
    );
}

#[test]
fn each_fire_is_stamped_at_its_ticks_anchor_and_ticks_are_monotonic() {
    let p = project("house", 7.0, None);
    let mut r = make(&p, 900.0, true, 0);
    run(&mut r, 0, 800, 128);
    let a = anchors(&mut r);
    let f = fires(&mut r);
    assert!(a.len() > 100 && f.len() > 100);
    assert_eq!(
        a[0].frame, 0,
        "play lands the first tick on the first sample"
    );
    let per = per_tick(900.0);
    for w in a.windows(2) {
        let (x, y) = (&w[0], &w[1]);
        assert!(y.frame > x.frame && y.tick == x.tick + 1 && y.generation == x.generation);
        let gap = (y.frame - x.frame) as f64;
        assert!(
            gap == libm_floor(per) || gap == libm_floor(per) + 1.0,
            "gap {gap} vs {per}"
        );
    }
    assert!(a.iter().all(|x| x.tempo == 900.0));
    assert_eq!(
        a[0].flags,
        TickAnchorFlags::DISCONTINUITY,
        "the first anchor starts a run"
    );
    assert!(a[1..].iter().all(|x| x.flags == TickAnchorFlags::default()));
    // no accumulated drift: the k-th tick is within a frame of k * per
    for x in &a {
        assert!((x.frame as f64 - (x.tick - 1) as f64 * per).abs() < 1.5);
    }
    // every fire sits exactly on the anchor of its own tick
    let by_tick: std::collections::BTreeMap<i32, i64> =
        a.iter().map(|x| (x.tick, x.frame)).collect();
    for e in &f {
        assert_eq!(by_tick[&e.tick], e.frame);
    }
    for w in f.windows(2) {
        assert!(w[1].frame >= w[0].frame && w[1].tick >= w[0].tick);
    }
}

fn libm_floor(x: f64) -> f64 {
    x.floor()
}

#[test]
fn the_stamps_use_the_block_start_frame_whatever_the_block_size() {
    let p = project("techno", 42.0, None);
    let mut stamps: Vec<Vec<i64>> = Vec::new();
    for size in [32usize, 128, 509, 4096] {
        let mut r = make(&p, 300.0, true, 0);
        run(&mut r, 10_000, 60_000 / size, size);
        stamps.push(anchors(&mut r).iter().take(40).map(|a| a.frame).collect());
    }
    assert_eq!(stamps[0].len(), 40);
    assert!(stamps.iter().all(|s| *s == stamps[0]));
    assert_eq!(
        stamps[0][0], 10_000,
        "offset by the frame the block started at"
    );
}

#[test]
fn the_tick_stays_the_same_when_play_starts_mid_stream() {
    let p = project("techno", 42.0, None);
    let mut r = make(&p, 300.0, false, 0);
    let at = run(&mut r, 0, 7, 128); // stopped: frames pass, nothing fires
    assert!(anchors(&mut r).is_empty() && fires(&mut r).is_empty());
    r.play();
    run(&mut r, at, 50, 128);
    assert_eq!(anchors(&mut r)[0].frame, at);
}

#[test]
fn muted_nets_and_stopped_decks_make_no_events() {
    let p = project("techno", 42.0, None);
    let mut r = make(&p, 300.0, false, 0);
    r.set_mute("kick", true);
    r.play();
    run(&mut r, 0, 400, 128);
    let got = fires(&mut r);
    let kick = r.graph().unwrap().by_id["kick"];
    assert!(!got.is_empty() && !got.iter().any(|e| e.net as usize == kick));
    assert_eq!(hits(&got), oracle(&p, tick(&r), 300.0, &["kick"]));
    // pause: the clock stops, so do the events
    r.pause();
    run(&mut r, 400 * 128, 1, 128);
    fires(&mut r);
    anchors(&mut r);
    run(&mut r, 401 * 128, 300, 128);
    assert!(fires(&mut r).is_empty() && anchors(&mut r).is_empty());
}

#[test]
fn mute_changes_take_effect_on_the_next_tick() {
    let p = project("techno", 42.0, None);
    let mut r = make(&p, 300.0, true, 0);
    run(&mut r, 0, 100, 128);
    r.set_role_muted(str_hash("kick"), true);
    run(&mut r, 100 * 128, 1, 128);
    fires(&mut r);
    run(&mut r, 101 * 128, 300, 128);
    let kick = r.graph().unwrap().by_id["kick"];
    let got = fires(&mut r);
    assert!(!got.is_empty() && !got.iter().any(|e| e.net as usize == kick));
    r.set_role_muted(str_hash("kick"), false);
    run(&mut r, 401 * 128, 300, 128);
    assert!(fires(&mut r).iter().any(|e| e.net as usize == kick));
}

#[test]
fn silent_replays_make_no_events() {
    let p = project("techno", 42.0, Some("standard"));
    // a section launch rebuilds the marking by replaying 200 ticks: silent
    let mut r = make(&p, 300.0, false, 200);
    run(&mut r, 0, 20, 128);
    assert!(
        fires(&mut r).is_empty() && anchors(&mut r).is_empty(),
        "the replay ran no events"
    );
    r.play();
    run(&mut r, 20 * 128, 200, 128);
    assert_eq!(
        anchors(&mut r)[0].tick,
        201,
        "the song resumes where the replay left it"
    );
    assert!(fires(&mut r).iter().all(|e| e.tick >= 201));

    // align() advances whole ticks silently: the ticks it skips have no events, and the
    // anchor after it says the numbering jumped
    let mut moved = make(&p, 300.0, true, 0);
    run(&mut moved, 0, 60, 128);
    fires(&mut moved);
    let before = anchors(&mut moved);
    let last_tick = before.last().unwrap().tick;
    moved.align(5.0);
    run(&mut moved, 60 * 128, 60, 128);
    let after = anchors(&mut moved);
    assert_eq!(
        after[0].tick,
        last_tick + 6,
        "5 ticks were replayed silently, then the next ran"
    );
    assert!(after[0].flags.contains(TickAnchorFlags::DISCONTINUITY));
    assert!(after[1..]
        .iter()
        .all(|a| a.flags == TickAnchorFlags::default()));
    let skipped = (last_tick + 1)..=(last_tick + 5);
    assert!(!fires(&mut moved).iter().any(|e| skipped.contains(&e.tick)));
}

#[test]
fn align_with_negative_ticks_holds_the_clock_back_and_is_flagged() {
    let mut r = make(&project("techno", 42.0, None), 300.0, true, 0);
    let frame = run(&mut r, 0, 60, 128);
    let before = anchors(&mut r);
    r.align(-2.5); // the next tick comes 2.5 ticks later
    run(&mut r, frame, 120, 128);
    let after = anchors(&mut r);
    assert_eq!(
        after[0].tick,
        before.last().unwrap().tick + 1,
        "no steps were skipped"
    );
    assert!(after[0].flags.contains(TickAnchorFlags::DISCONTINUITY));
    let gap = (after[0].frame - before.last().unwrap().frame) as f64;
    assert!(gap > 3.0 * per_tick(300.0), "the late tick: {gap} frames");
}

#[test]
fn starting_stopping_and_resuming_the_clock_are_discontinuities() {
    let mut r = make(&project("techno", 42.0, None), 300.0, true, 0);
    let mut frame = run(&mut r, 0, 60, 128);
    let first = anchors(&mut r);
    assert!(first[0].flags == TickAnchorFlags::DISCONTINUITY);
    assert!(first[1..]
        .iter()
        .all(|a| a.flags == TickAnchorFlags::default()));
    // pause and resume: the tick numbers go on, the spacing does not
    r.pause();
    frame = run(&mut r, frame, 30, 128);
    assert!(anchors(&mut r).is_empty());
    r.play();
    frame = run(&mut r, frame, 60, 128);
    let resumed = anchors(&mut r);
    assert!(resumed[0].tick == first.last().unwrap().tick + 1);
    assert_eq!(resumed[0].flags, TickAnchorFlags::DISCONTINUITY);
    assert!(resumed[1..]
        .iter()
        .all(|a| a.flags == TickAnchorFlags::default()));
    // stop resets the song to the start: the numbering begins again, within the same project
    r.stop();
    frame = run(&mut r, frame, 10, 128);
    r.play();
    run(&mut r, frame, 60, 128);
    let restarted = anchors(&mut r);
    assert!(restarted[0].tick == 1 && restarted[0].generation == first[0].generation);
    assert_eq!(restarted[0].flags, TickAnchorFlags::DISCONTINUITY);
}

#[test]
fn a_project_loaded_on_a_stopped_deck_is_a_generation_change_without_a_swap() {
    let mut r = make(&project("techno", 42.0, None), 300.0, true, 0);
    let mut frame = run(&mut r, 0, 60, 128);
    r.pause();
    frame = run(&mut r, frame, 5, 128);
    anchors(&mut r);
    fires(&mut r);
    let next = WaveRunner::prepare(&project("house", 4.0, None), SR, &WaveOptions::default(), 0);
    let generation = next.generation;
    r.load_prepared(next); // not playing: installed at once, no bar to wait for
    frame = run(&mut r, frame, 5, 128);
    r.play();
    run(&mut r, frame, 60, 128);
    let a = anchors(&mut r);
    assert!(a.iter().all(|x| x.generation == generation));
    assert!(!a.iter().any(|x| x.flags.contains(TickAnchorFlags::SWAP)));
    assert!(a[0].flags == TickAnchorFlags::DISCONTINUITY && a[0].tick == 1);
}

#[test]
fn a_project_swap_is_an_anchor_with_a_new_generation() {
    let mut r = make(&project("techno", 42.0, None), 300.0, true, 0);
    run(&mut r, 0, 50, 128);
    let first = r.generation();
    let next = WaveRunner::prepare(&project("house", 4.0, None), SR, &WaveOptions::default(), 0);
    let next_gen = next.generation;
    r.load_prepared(next);
    assert!(next_gen > first);
    run(&mut r, 50 * 128, 600, 128);
    let a = anchors(&mut r);
    let f = fires(&mut r);
    let swaps: Vec<&TickAnchor> = a
        .iter()
        .filter(|x| x.flags.contains(TickAnchorFlags::SWAP))
        .collect();
    assert_eq!(swaps.len(), 1);
    let swap = swaps[0];
    assert!(
        swap.generation == next_gen && swap.tick == 0,
        "the new project's clock restarts"
    );
    assert_eq!(
        swap.flags,
        TickAnchorFlags::SWAP,
        "time stays regular across a swap"
    );
    let idx = a
        .iter()
        .position(|x| x.flags.contains(TickAnchorFlags::SWAP))
        .unwrap();
    assert!(a[..idx].iter().all(|x| x.generation == first));
    assert!(a[idx + 1..].iter().all(|x| x.generation == next_gen));
    assert_eq!(a[idx + 1].tick, 1);
    assert_eq!(
        (a[idx - 1].tick + 1) % 16,
        0,
        "the swap lands on a bar boundary"
    );
    assert!(f
        .iter()
        .filter(|e| e.generation == first)
        .all(|e| e.frame < swap.frame));
    assert!(f
        .iter()
        .filter(|e| e.generation == next_gen)
        .all(|e| e.frame > swap.frame));
    assert!(f.iter().any(|e| e.generation == next_gen));
}

#[test]
fn macros_and_stingers_are_flagged() {
    let p = project("techno", 42.0, Some("standard"));
    let mut r = make(&p, 300.0, true, 0);
    run(&mut r, 0, 100, 128);
    fires(&mut r);
    assert!(!r.macro_active());
    r.fire_macro(NetMacro::BeatRepeat, 32);
    r.fire_stinger(str_hash("hit1"), 32);
    let mut frame = run(&mut r, 100 * 128, 150, 128);
    assert!(r.macro_active(), "the beat repeat is running");
    let during = fires(&mut r);
    assert!(!during.is_empty());
    assert!(during.iter().all(|e| e.flags.contains(FireFlags::MACRO)));
    let hit1 = r.graph().unwrap().by_id["hit1"];
    let stingers: Vec<_> = during
        .iter()
        .filter(|e| e.flags.contains(FireFlags::STINGER))
        .collect();
    assert!(stingers.iter().all(|e| e.net as usize == hit1));
    assert_eq!(
        stingers.len(),
        during.iter().filter(|e| e.net as usize == hit1).count()
    );
    frame = run(&mut r, frame, 300, 128); // let both run out
    fires(&mut r);
    run(&mut r, frame, 300, 128);
    let after = fires(&mut r);
    assert!(!after.is_empty());
    assert!(!after
        .iter()
        .any(|e| e.flags.contains(FireFlags::MACRO) || e.flags.contains(FireFlags::STINGER)));
    assert!(!r.macro_active(), "and it is over");
}

#[test]
fn a_macro_that_changed_nothing_is_still_running_for_exactly_its_duration() {
    let mut r = make(&project("techno", 42.0, Some("standard")), 300.0, true, 0);
    for role in ["bass", "melody", "harmony", "arp"] {
        r.set_role_muted(str_hash(role), true);
    }
    let mut frame = run(&mut r, 0, 60, 128);
    fires(&mut r);
    frame = run(&mut r, frame, 60, 128);
    assert!(
        !fires(&mut r).is_empty() && !r.macro_active(),
        "the drums play, no macro"
    );
    let fired = tick(&r);
    r.fire_macro(NetMacro::SoloDrums, 16);
    frame = run(&mut r, frame, 1, 128);
    assert_eq!(
        r.graph().unwrap().pending_macro_timers(),
        0,
        "nothing to mute, no timer"
    );
    assert!(r.macro_active(), "but a macro was fired");
    fires(&mut r);
    run(&mut r, frame, 400, 128);
    let after = fires(&mut r);
    assert!(!r.macro_active(), "it ran out");
    let flagged: Vec<_> = after
        .iter()
        .filter(|e| e.flags.contains(FireFlags::MACRO))
        .collect();
    let plain: Vec<_> = after
        .iter()
        .filter(|e| !e.flags.contains(FireFlags::MACRO))
        .collect();
    assert!(
        flagged.len() >= 6,
        "the drums' hits during it: {}",
        flagged.len()
    );
    assert!(flagged
        .iter()
        .all(|e| e.tick as i64 > fired && e.tick as i64 <= fired + 16));
    assert!(plain
        .iter()
        .all(|e| e.tick as i64 <= fired || e.tick as i64 > fired + 16));
    assert!(
        plain.iter().any(|e| e.tick as i64 > fired + 16),
        "and the drums go on after it"
    );
}

#[test]
fn every_macro_kind_and_a_stinger_marks_its_duration() {
    for m in NetMacro::ALL {
        let mut r = make(&project("techno", 42.0, Some("standard")), 300.0, true, 0);
        run(&mut r, 0, 10, 128);
        assert!(!r.macro_active());
        r.fire_macro(m, 4);
        run(&mut r, 10 * 128, 2, 128);
        assert!(r.macro_active(), "{m:?} marks itself running");
        run(&mut r, 12 * 128, 120, 128); // 4 ticks and a good margin
        assert!(!r.macro_active(), "{m:?} is over");
    }
    let mut r = make(&project("techno", 42.0, Some("standard")), 300.0, true, 0);
    run(&mut r, 0, 10, 128);
    r.fire_stinger(str_hash("no-such-net"), 4);
    run(&mut r, 10 * 128, 2, 128);
    assert!(r.macro_active(), "a stinger marks itself running");
    run(&mut r, 12 * 128, 120, 128);
    assert!(!r.macro_active());
}

#[test]
fn panic_and_stop_clear_the_marked_macro() {
    let mut r = make(&project("techno", 42.0, Some("standard")), 300.0, true, 0);
    let mut frame = run(&mut r, 0, 10, 128);
    r.fire_stinger(str_hash("no-such-net"), 64);
    frame = run(&mut r, frame, 2, 128);
    assert!(r.macro_active());
    r.panic();
    frame = run(&mut r, frame, 2, 128);
    assert!(
        !r.macro_active(),
        "panic: beats effects.js sets _runningMacro back to null"
    );
    r.fire_stinger(str_hash("no-such-net"), 64);
    frame = run(&mut r, frame, 2, 128);
    assert!(r.macro_active());
    r.stop();
    run(&mut r, frame, 2, 128);
    assert!(!r.macro_active());
}

#[test]
fn the_macro_flag_follows_mute_timers_and_tempo_effects() {
    let mut r = make(&project("techno", 42.0, Some("standard")), 300.0, true, 0);
    let mut frame = run(&mut r, 0, 50, 128);
    assert!(!r.macro_active());
    r.fire_macro(NetMacro::Cut, 8); // mutes what is audible, restores it after 8 ticks
    frame = run(&mut r, frame, 20, 128);
    assert!(r.macro_active());
    frame = run(&mut r, frame, 400, 128);
    assert!(!r.macro_active());
    r.fire_macro(NetMacro::HalfTime, 8); // a tempo effect
    frame = run(&mut r, frame, 20, 128);
    assert!(r.macro_active());
    assert!(r.tempo() < 300.0, "half time slowed the clock");
    r.panic();
    run(&mut r, frame, 5, 128);
    assert!(!r.macro_active(), "panic cancels everything");
    assert_eq!(r.tempo(), 300.0, "and gives the tempo back");
}

#[test]
fn reporting_does_not_change_the_audio() {
    let p = project("techno", 42.0, None);
    let render = |record: bool| {
        let mut r = make(&p, 300.0, true, 0);
        r.record_events = record;
        let mut out = Vec::new();
        let mut l = vec![0f32; 128];
        for b in 0..200 {
            r.process_at(&mut l, None, 128, b * 128);
            out.extend_from_slice(&l);
        }
        out
    };
    assert!(render(true) == render(false));
}

// ---- the sampler hook --------------------------------------------------------

fn bind_all_tonal(_instrument: &str, _part: &str) -> Option<usize> {
    Some(3)
}

/// A sampled lane makes no sound of its own: every note it would have struck is reported
/// for the host's sampler, at the frame of its tick; the drums still sound. Golden: none
/// (the hook is this port's; the Swift app's `Sampler` is the model).
#[test]
fn a_sampled_lane_reports_its_notes_and_stays_silent() {
    let p = project("techno", 42.0, None);
    let opts = WaveOptions {
        master: Master::Linear,
        sample_binder: Some(bind_all_tonal),
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, opts.clone());
    r.record_events = true;
    r.load_prepared(WaveRunner::prepare(&p, SR, &opts, 0));
    r.set_tempo(300.0);
    r.play();
    let tonal_lanes = r.project().unwrap().lanes().iter().filter(|l| l.is_tonal());
    assert!(
        tonal_lanes.clone().count() > 0 && tonal_lanes.clone().all(|l| l.sample_slot() == Some(3))
    );
    let mut out = vec![0f32; 128];
    for b in 0..600 {
        r.process_at(&mut out, None, 128, b * 128);
        assert!(r
            .project()
            .unwrap()
            .lanes()
            .iter()
            .filter(|l| l.is_tonal())
            .all(|l| l.envelope() == 0.0));
    }
    let notes = std::mem::take(&mut r.sample_notes);
    let fires = fires(&mut r);
    let tonal_fires: Vec<_> = fires
        .iter()
        .filter(|e| !e.flags.contains(FireFlags::DRUM))
        .collect();
    assert!(tonal_fires.len() > 5 && notes.len() == tonal_fires.len());
    for (n, e) in notes.iter().zip(&tonal_fires) {
        assert!(
            n.slot == 3
                && n.frame == e.frame
                && n.note == e.note as i64
                && n.velocity == e.velocity as i64
        );
        assert!(n.dur_ms > 0.0);
    }
    // the same song without the binder sounds the tonal lanes
    let mut plain = make(&p, 300.0, true, 0);
    run(&mut plain, 0, 600, 128);
    assert!(plain.sample_notes.is_empty());
}
