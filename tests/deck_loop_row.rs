//! The loop row on the engine (`LoopLaunchTests`, `LoopRecordTests`): launch boundaries and the
//! countdown to them, scenes, a clip's own boundary and tempo follow, clips onto a deck and
//! back, crop, takes that know where they came from, imports. Each test names the Swift test
//! it ports (PORTING.md's rule 4a); the audio assertions run on the offline engine.

mod common_row;

use common_row::*;
use djrust::deck::loops::{LoopFollow, LoopState, SceneClip};
use djrust::deck::clips::LoopFade;
use djrust::session::types::{LoopFx, LoopFxMode};

// LoopLaunchTests.withFourBarsAStartLandsOnAPhraseOfTheDeck
#[test]
fn with_four_bars_a_start_lands_on_a_phrase_of_the_deck() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();
    rig.decks_to_the_loops_only();
    rig.run(2048);
    rig.e.stop_all_loops();
    rig.settle();
    assert!(rig.e.looping_clips().is_empty());
    rig.e.set_loop_quantum(16);
    assert_eq!(rig.e.loop_quantum(), 16);
    let clock = rig.e.loop_clock(Some(&one), None);
    assert_eq!(clock.ref_deck.as_deref(), Some("A"));
    let now = rig.frame();
    let down = clock.next_boundary(now + djrust::host::deck::loops::LOOP_LEAD, 16);
    assert!(on_line(clock.beat(down), 16.0), "on a 16-beat line of A's own count");
    assert_eq!((clock.beat(down) / 4.0).round() as i64 % 4, 0, "bar 1, 5, 9...");
    assert!(down - now <= 16 * BEAT + djrust::host::deck::loops::LOOP_LEAD);

    rig.e.set_looping(&one, true);
    assert_eq!(rig.e.loop_state(&one), LoopState::Starting);
    assert_eq!(rig.e.loop_switch_frame(&one), Some(down));
    let cd = rig.e.loop_countdown(&one).unwrap();
    assert!((cd.0 - (down - now) as f64 / BEAT as f64).abs() < 0.05, "{cd:?}");
    assert!(cd.1 >= cd.0);
    assert_eq!(rig.e.loop_switch_frame(&two), None, "not waiting");

    // half way there it counts down; on the boundary it comes in, frame-exact
    rig.run_to(now + (down - now) / 2);
    let mid = rig.e.loop_countdown(&one).unwrap();
    assert!(mid.0 < cd.0 && mid.0 > 0.0);
    rig.run_to(down + BAR / 2);
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
    assert!(rig.e.loop_countdown(&one).is_none());
    let (before, after) = rig.edge(down);
    assert!(before == 0.0 && after > 0.001, "comes in at frame {down}: {before} {after}");

    // and goes out on a phrase line too
    rig.e.set_looping(&one, false);
    let out = rig.e.loop_switch_frame(&one).unwrap();
    assert!(on_line(rig.e.loop_clock(Some(&one), None).beat(out), 16.0));
}

// LoopLaunchTests.onTheBeatAStartWaitsAtMostABeat
#[test]
fn on_the_beat_a_start_waits_at_most_a_beat() {
    let mut rig = Rig::new();
    let (one, _) = rig.two_takes();
    rig.e.stop_all_loops();
    rig.settle();
    rig.e.set_loop_quantum(1);
    rig.e.set_loop_quantum(3); // not one of the boundaries offered: refused
    assert_eq!(rig.e.loop_quantum(), 1);
    let now = rig.frame();
    rig.e.set_looping(&one, true);
    let at = rig.e.loop_switch_frame(&one).unwrap();
    assert!(at - now <= BEAT + djrust::host::deck::loops::LOOP_LEAD);
    assert!(on_line(rig.e.loop_clock(Some(&one), None).beat(at), 1.0));
    use djrust::deck::loops::quantum_label;
    assert!(quantum_label(1) == "beat" && quantum_label(4) == "bar");
    assert!(quantum_label(8) == "2 bars" && quantum_label(32) == "8 bars");
}

const LEAD: i64 = djrust::host::deck::loops::LOOP_LEAD;

fn scene_clip(id: &str, gain: f64) -> SceneClip {
    SceneClip {
        clip_id: id.to_string(),
        gain,
        fx: None,
    }
}

// LoopLaunchTests.aSceneStartsExactlyItsClipsAndStopsTheOthersOnOneBoundary
#[test]
fn a_scene_starts_exactly_its_clips_and_stops_the_others_on_one_boundary() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();
    // "Scene 1": just one (at 0.6); "Scene 2": just two
    rig.e.set_looping(&two, false);
    rig.settle();
    rig.e.set_clip_gain(&one, 0.6, None);
    assert_eq!(rig.e.next_scene_name(), "Scene 1");
    let n = rig.e.next_scene_name();
    rig.e.save_scene(&n);
    rig.e.set_looping(&one, false);
    rig.e.set_looping(&two, true);
    rig.settle();
    let n = rig.e.next_scene_name();
    rig.e.save_scene(&n);
    let names: Vec<&str> = rig.e.scenes().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Scene 1", "Scene 2"]);
    assert_eq!(rig.e.scenes()[0].clips, [scene_clip(&one, 0.6)]);
    assert_eq!(rig.e.scenes()[1].clips, [scene_clip(&two, 1.0)]);
    rig.e.set_clip_gain(&one, 0.2, None);

    rig.e.set_loop_quantum(8);
    rig.e.launch_scene("Scene 1");
    assert_eq!(rig.e.loop_state(&one), LoopState::Starting);
    assert_eq!(rig.e.loop_state(&two), LoopState::Stopping);
    let at = rig.e.loop_switch_frame(&one).unwrap();
    assert_eq!(rig.e.loop_switch_frame(&two), Some(at), "in and out on the same frame");
    assert!(on_line(rig.e.loop_clock(Some(&one), None).beat(at), 8.0), "a 2-bar line");
    assert_eq!(rig.e.clip_gain(&one), 0.6, "the scene's level");
    rig.run_to(at + 4096);
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
    assert_eq!(rig.e.loop_state(&two), LoopState::Off);
    assert_eq!(rig.e.looping_clips(), [one.clone()]);

    // a scene with a clip already in keeps it in, and drops the rest
    rig.e.set_looping(&two, true);
    rig.settle();
    rig.e.launch_scene("Scene 2");
    assert_eq!(rig.e.loop_state(&two), LoopState::On, "already in: stays in");
    assert_eq!(rig.e.loop_state(&one), LoopState::Stopping);
    rig.settle();
    assert_eq!(rig.e.looping_clips(), [two.clone()]);

    // a recorded launch carries its clips, so a replay needs no saved scene
    rig.e.launch_scene_clips(&[scene_clip(&one, 0.5)]);
    rig.settle();
    assert!(rig.e.looping_clips() == [one.clone()] && rig.e.clip_gain(&one) == 0.5);
    assert_eq!(rig.e.launch_scene("nothing like it"), None);
    assert_eq!(rig.e.looping_clips(), [one], "an unknown scene does nothing");
}

// LoopLaunchTests.aWaitingLoopFollowsItsDeckWhenTheTempoMovesDuringTheCountdown
#[test]
fn a_waiting_loop_follows_its_deck_when_the_tempo_moves_during_the_countdown() {
    let mut rig = Rig::new();
    let (one, _) = rig.two_takes();
    rig.settle();
    rig.decks_to_the_loops_only();
    rig.run(2048);
    rig.e.stop_all_loops();
    rig.settle();
    rig.e.set_loop_quantum(32);
    // just past an 8-bar line, so the wait is a long one
    loop {
        let clock = rig.e.loop_clock(Some(&one), None);
        if clock.next_boundary(rig.frame() + LEAD, 32) - rig.frame() >= 5 * BAR {
            break;
        }
        rig.run(BAR / 2);
    }
    rig.e.set_looping(&one, true);
    let planned = rig.e.loop_switch_frame(&one).unwrap();
    let line = rig.e.loop_clock(Some(&one), None).beat(planned);
    assert!(on_line(line, 32.0));
    // a bar into the countdown, deck A's tempo fader goes up 5 %: its line comes sooner
    rig.run(BAR);
    rig.e.set_pitch("A", 0.625);
    rig.run(8 * BLOCK as i64);
    let moved = rig.e.loop_switch_frame(&one).unwrap();
    assert!(moved < planned - 10_000, "follows the deck: {moved} vs {planned}");
    let again = rig.e.loop_clock(Some(&one), None).beat(moved);
    assert!((again - line).abs() < 0.01, "the same line of A: {again} vs {line}");
    rig.run_to(moved + BAR / 2);
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
    let (before, after) = rig.edge(moved);
    assert!(before == 0.0 && after > 0.001, "comes in on the moved frame: {before} {after}");
    // and in phase with A, at A's new tempo
    let phase = rig.e.loop_playhead(&one).unwrap();
    let a = rig.e.beat_state("A").unwrap().bar_phase();
    let err = (phase - a).abs();
    assert!(err.min(1.0 - err) < 0.01, "{phase} vs {a}");
}

// LoopLaunchTests.aMuteDuringASceneCountdownLeavesTheScenesLevelForTheBoundary
#[test]
fn a_mute_during_a_scene_countdown_leaves_the_scenes_level_for_the_boundary() {
    let mut rig = Rig::new();
    let (x, y) = (test_clip(1), test_clip(2));
    let (xid, yid) = (x.id.clone(), y.id.clone());
    rig.e.restore_clips(&[x, y]);
    rig.e.set_looping(&xid, true);
    rig.e.set_looping(&yid, true);
    rig.settle();
    let level = |rig: &Rig, from: i64, to: i64| -> f64 {
        let s = &rig.master[from as usize..to as usize];
        s.iter().map(|v| v.abs() as f64).sum::<f64>() / s.len().max(1) as f64
    };
    rig.e.set_loop_quantum(16);
    let at = rig.e.launch_scene_clips(&[scene_clip(&xid, 0.5), scene_clip(&yid, 1.0)]);
    assert!(at - rig.frame() > BAR, "a 4-bar countdown");
    rig.run(4 * BLOCK as i64);
    let both = level(&rig, rig.frame() - 1024, rig.frame());
    rig.e.set_clip_mute(&yid, true);
    rig.run_to(at - 4096);
    let before = level(&rig, at - 8192, at - 4096);
    rig.run_to(at + 8192);
    let after = level(&rig, at + 2048, at + 6144);
    // x alone at 1 until the boundary (y muted), then at the scene's 0.5
    assert!((before / both - 0.0150 / 0.0400).abs() < 0.02, "x still at 1: {before} of {both}");
    assert!((after / before - 0.5).abs() < 0.02, "the scene's level on its boundary: {after}");
}

// LoopLaunchTests.scenesAreRenamedOverwrittenDeletedAndFollowACrop
#[test]
fn scenes_are_renamed_overwritten_deleted_and_follow_a_crop() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();
    rig.e.save_scene("Scene 1");
    let ids: Vec<String> = rig.e.scenes()[0].clips.iter().map(|c| c.clip_id.clone()).collect();
    assert_eq!(ids, [one.clone(), two.clone()], "in row order");
    rig.e.rename_scene("Scene 1", "drop");
    let n = rig.e.next_scene_name();
    rig.e.save_scene(&n);
    let names = |e: &djrust::host::deck::Engine| -> Vec<String> { e.scenes().iter().map(|s| s.name.clone()).collect() };
    assert_eq!(names(&rig.e), ["drop", "Scene 2"]);
    rig.e.rename_scene("drop", "Scene 2");
    assert_eq!(names(&rig.e), ["drop", "Scene 2"], "a name already taken is refused");
    // overwrite: the same name, what plays now
    rig.e.set_looping(&two, false);
    rig.settle();
    rig.e.save_scene("drop");
    assert_eq!(rig.e.scenes()[0].name, "drop");
    assert_eq!(rig.e.scenes()[0].clips.iter().map(|c| c.clip_id.clone()).collect::<Vec<_>>(), [one.clone()]);
    // a crop takes the original's place in its scenes
    let cropped = rig.e.crop_clip(&one, 0, 2).expect("a crop");
    let ids = |e: &djrust::host::deck::Engine, i: usize| -> Vec<String> { e.scenes()[i].clips.iter().map(|c| c.clip_id.clone()).collect() };
    assert_eq!(ids(&rig.e, 0), [cropped.clone()]);
    assert_eq!(ids(&rig.e, 1), [cropped, two]);
    rig.e.delete_scene("drop");
    assert_eq!(names(&rig.e), ["Scene 2"]);
    assert_eq!(rig.e.next_scene_name(), "Scene 3", "Scene 2 is taken");
}

// LoopLaunchTests.aSceneLaunchIsRecordedWithItsClips (the recorded form and the starting state;
// the session recorder itself is the later wave's)
#[test]
fn a_scene_launch_is_recorded_with_its_clips() {
    use djrust::session::action::DeckAction;
    let mut rig = Rig::new();
    let c = test_clip(1);
    let id = c.id.clone();
    rig.e.restore_clips(&[c]);
    rig.e.set_looping(&id, true);
    rig.e.set_clip_gain(&id, 0.7, None);
    rig.e.save_scene("S");
    rig.e.set_loop_quantum(16);
    // a recording starts by saying the settings that are not a fresh engine's: the boundary
    let defaults = djrust::journal::replay::default_loop_settings();
    let settings = rig.e.loop_settings();
    let differ: Vec<&DeckAction> = settings.iter().filter(|a| !defaults.contains(a)).collect();
    assert_eq!(differ, [&DeckAction::LoopQuantize { beats: 16 }]);
    let recorded = rig.e.recorded_loop_action(
        &DeckAction::SceneLaunch { name: "S".into(), clips: None },
        None,
    );
    assert_eq!(
        recorded,
        DeckAction::SceneLaunch { name: "S".into(), clips: Some(vec![scene_clip(&id, 0.7)]) },
        "the scene, spelled out"
    );
}

// LoopLaunchTests.aSceneSetsItsClipsFxOnItsBoundaryFrame
#[test]
fn a_scene_sets_its_clips_fx_on_its_boundary_frame() {
    let mut rig = Rig::new();
    let (c, d) = (test_clip(1), test_clip(2));
    let (cid, did) = (c.id.clone(), d.id.clone());
    rig.e.restore_clips(&[c, d]);
    rig.e.set_looping(&cid, true);
    rig.e.set_looping(&did, true);
    rig.settle();
    let crunch = LoopFx { drive: 9.0, mode: LoopFxMode::Hard, amount: 0.55, tone: 0.75, ..LoopFx::default() };
    let fuzz = LoopFx { drive: 18.0, mode: LoopFxMode::Soft, amount: 0.9, tone: 0.6, ..LoopFx::default() };
    rig.e.set_clip_fx(&cid, &crunch, None);
    rig.e.save_scene("S");
    let s = rig.e.scene("S").unwrap().clips.clone();
    assert_eq!(
        s,
        [
            SceneClip { clip_id: cid.clone(), gain: 1.0, fx: Some(crunch.clone()) },
            scene_clip(&did, 1.0)
        ],
        "FX kept only where they were on"
    );
    rig.e.set_clip_fx(&cid, &LoopFx::default(), None);
    rig.e.set_clip_fx(&did, &fuzz, None);
    rig.run(4096);
    let v = rig.e.voice_index(&cid).unwrap();
    assert!(!rig.e.loops.mirror.fx_running(v), "dry before the launch");

    rig.e.set_loop_quantum(4);
    let at = rig.e.launch_scene("S").unwrap();
    assert_eq!(rig.e.clip_fx(&cid), crunch, "the clip's FX are the scene's at once");
    assert_eq!(rig.e.clip_fx(&did), fuzz, "a clip saved dry keeps the FX it has");
    rig.run_to(at);
    assert!(!rig.e.loops.mirror.fx_running(v), "the voice waits for the boundary");
    rig.run_to(at + 1);
    assert!(rig.e.loops.mirror.fx_running(v), "and takes them on its frame");
    assert!(rig.e.loop_state(&cid) == LoopState::On && rig.e.loop_state(&did) == LoopState::On);
}

// LoopLaunchTests.sceneFxAreRecordedAndEncodedOnlyWhenSet
#[test]
fn scene_fx_are_recorded_and_encoded_only_when_set() {
    use djrust::session::types::{scene_clips_from_json, scene_clips_json};
    let crunch = LoopFx { drive: 9.0, mode: LoopFxMode::Hard, amount: 0.55, tone: 0.75, ..LoopFx::default() };
    // without FX a scene encodes as it did before scenes kept them
    assert_eq!(
        scene_clips_json(&[scene_clip("abc", 0.5)]).canonical_js(),
        r#"[{"clip":"abc","gain":0.5}]"#
    );
    let with_fx = vec![SceneClip { clip_id: "abc".into(), gain: 0.5, fx: Some(crunch.clone()) }, scene_clip("def", 1.0)];
    assert_eq!(scene_clips_from_json(Some(&scene_clips_json(&with_fx))), with_fx);

    let mut rig = Rig::new();
    let c = test_clip(1);
    let id = c.id.clone();
    rig.e.restore_clips(&[c]);
    rig.e.set_looping(&id, true);
    rig.e.set_clip_fx(&id, &crunch, None);
    rig.e.save_scene("S");
    use djrust::session::action::DeckAction;
    let rec = rig.e.recorded_loop_action(&DeckAction::SceneLaunch { name: "S".into(), clips: None }, None);
    let DeckAction::SceneLaunch { clips: Some(clips), .. } = rec else { panic!("{rec:?}") };
    assert_eq!(clips, [SceneClip { clip_id: id.clone(), gain: 1.0, fx: Some(crunch) }], "the recorded launch carries them");

    // an old scene (no FX) leaves the clip's alone; one with FX off turns them off
    let fuzz = LoopFx { drive: 18.0, mode: LoopFxMode::Soft, amount: 0.9, tone: 0.6, ..LoopFx::default() };
    rig.e.set_clip_fx(&id, &fuzz, None);
    rig.e.launch_scene_clips(&[scene_clip(&id, 1.0)]);
    assert_eq!(rig.e.clip_fx(&id), fuzz);
    rig.e.launch_scene_clips(&[SceneClip { clip_id: id.clone(), gain: 1.0, fx: Some(LoopFx::default()) }]);
    assert_eq!(rig.e.clip_fx(&id), LoopFx::default());
}

// LoopLaunchTests.aLevelChangeLandsOnItsFrame
#[test]
fn a_level_change_lands_on_its_frame() {
    use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
    use djrust::host::deck::loops::{LoopBank, LoopMsg};
    use std::sync::Arc;
    // the bank on its own: a constant clip, its level dropped to a quarter at frame 700
    let (mut bank, _) = LoopBank::new(SR);
    let n = 4096;
    let b = Arc::new(SampleBuffer::new(vec![0.5; n], None, SR));
    bank.apply(LoopMsg::Voice(0, TurntableCommand::Load(b)));
    bank.apply(LoopMsg::Voice(0, TurntableCommand::Loop { start: 0.0, end: n as f64 }));
    bank.apply(LoopMsg::Voice(0, TurntableCommand::Start));
    bank.apply(LoopMsg::Gain(0, 1.0));
    bank.apply(LoopMsg::Start { v: 0, frame: 0, fade: 0 });
    bank.apply(LoopMsg::GainAt(0, 0.25, 700));
    let mut out: Vec<f32> = Vec::new();
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    for b in 0..3 {
        bank.render_bank(b * 512, &mut l, &mut r);
        out.extend_from_slice(&l);
    }
    // it starts on its frame and ramps there over `ramp_frames`, like any level change
    let ramp = bank.ramp_frames() as usize;
    assert!(out[100..700].iter().all(|v| (v - 0.5).abs() < 1e-6), "{}", out[699]);
    assert!(out[701] < 0.5 && out[701] > 0.125, "{}", out[701]);
    assert!(out[700..700 + ramp].iter().zip(&out[701..]).all(|(a, b)| *b <= *a + 1e-6), "falling");
    assert!(out[700 + ramp..1536].iter().all(|v| (v - 0.125).abs() < 1e-6), "{}", out[700 + ramp]);
    // a plain level change cancels one still waiting
    bank.apply(LoopMsg::GainAt(0, 0.0, 2000));
    bank.apply(LoopMsg::Gain(0, 1.0));
    bank.render_bank(1536, &mut l, &mut r);
    bank.render_bank(2048, &mut l, &mut r);
    assert!(l.iter().all(|v| (v - 0.5).abs() < 1e-6));
}

// LoopLaunchTests.aClipsOwnBoundaryOverridesTheRowsButAScenesDoesNot
#[test]
fn a_clips_own_boundary_overrides_the_rows_but_a_scenes_does_not() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();
    rig.e.stop_all_loops();
    rig.settle();
    rig.e.set_loop_quantum(1);
    rig.e.set_clip_quantum(&one, Some(16));
    rig.e.set_clip_quantum(&two, Some(3)); // not offered: refused
    assert!(rig.e.clip_quantum(&one) == Some(16) && rig.e.clip_quantum(&two).is_none());
    assert!(rig.e.quantum_for(Some(&one)) == 16 && rig.e.quantum_for(Some(&two)) == 1);
    assert_eq!(rig.e.quantum_for(None), 1);

    // one waits for a 4-bar phrase line, two (following the row) at most a beat
    let now = rig.frame();
    rig.e.set_looping(&one, true);
    rig.e.set_looping(&two, true);
    let a = rig.e.loop_switch_frame(&one).unwrap();
    let b = rig.e.loop_switch_frame(&two).unwrap();
    assert!(on_line(rig.e.loop_clock(Some(&one), None).beat(a), 16.0), "its own boundary");
    assert!(b - now <= BEAT + LEAD, "the row's");
    assert!(on_line(rig.e.loop_clock(Some(&two), None).beat(b), 1.0));
    rig.settle();

    // its stop waits for its own line too
    rig.e.set_looping(&one, false);
    let out = rig.e.loop_switch_frame(&one).unwrap();
    assert!(on_line(rig.e.loop_clock(Some(&one), None).beat(out), 16.0));
    rig.settle();

    // a scene switches on the row's boundary, whatever its clips say
    rig.e.set_loop_quantum(4);
    rig.e.set_clip_quantum(&one, Some(32));
    let then = rig.frame();
    rig.e.launch_scene_clips(&[scene_clip(&one, 1.0)]);
    let at = rig.e.loop_switch_frame(&one).unwrap();
    assert!(at - then <= 4 * BEAT + LEAD, "a bar at most, not 8");
    assert_eq!(rig.e.loop_switch_frame(&two), Some(at));

    // None follows the row again
    rig.e.set_clip_quantum(&one, None);
    assert!(rig.e.clip_quantum(&one).is_none() && rig.e.quantum_for(Some(&one)) == 4);
}

// LoopLaunchTests.aClipsBoundaryAndFadeGoWithItAndAreForgottenWithIt
#[test]
fn a_clips_boundary_and_fade_go_with_it_and_are_forgotten_with_it() {
    use djrust::deck::clips::{ClipLaunch, ClipSound};
    use std::collections::BTreeMap;
    let mut rig = Rig::new();
    let (c, d) = (test_clip(1), test_clip(2));
    let (cid, did) = (c.id.clone(), d.id.clone());
    rig.e.restore_clips(&[c.clone(), d.clone()]);
    rig.e.set_clip_quantum(&cid, Some(8));
    rig.e.set_clip_fade(&cid, Some(LoopFade::Bar));
    let want = BTreeMap::from([(cid.clone(), ClipLaunch { quantum: Some(8), fade: Some(LoopFade::Bar), follow: None })]);
    assert_eq!(rig.e.clip_launch(), want, "only clips with one");
    // taken off and put back: as it was
    rig.e.remove_clip(&cid);
    assert!(rig.e.clip_quantum(&cid).is_none() && rig.e.clip_fade(&cid).is_none() && rig.e.clip_launch().is_empty());
    assert_eq!(
        rig.e.loops.row.removed.first().map(|r| r.sound.clone()),
        Some(ClipSound { quantum: Some(8), fade: Some(LoopFade::Bar), ..ClipSound::default() })
    );
    rig.e.restore_clip(&cid, None);
    assert!(rig.e.clip_quantum(&cid) == Some(8) && rig.e.clip_fade(&cid) == Some(LoopFade::Bar));
    // the library's, back on a fresh row (unknown ids skipped)
    let mut fresh = Rig::new();
    fresh.e.restore_clips(&[c, d]);
    fresh.e.restore_clip_launch(&BTreeMap::from([
        (cid.clone(), ClipLaunch { quantum: Some(16), ..ClipLaunch::default() }),
        ("gone".to_string(), ClipLaunch { fade: Some(LoopFade::Off), ..ClipLaunch::default() }),
    ]));
    assert!(fresh.e.clip_quantum(&cid) == Some(16) && fresh.e.clip_fade(&cid).is_none());
    assert_eq!(fresh.e.clip_launch().keys().cloned().collect::<Vec<_>>(), [cid]);
    let _ = did;
}

// LoopLaunchTests.theClipLaunchActionsRoundTripAndRecord (the actions on the engine and their
// replay; the codec and the journal's fold keys are held by the session golden)
#[test]
fn the_clip_launch_actions_apply_and_replay() {
    use djrust::session::action::DeckAction as A;
    let mut rig = Rig::new();
    let c = test_clip(1);
    let id = c.id.clone();
    rig.e.restore_clips(std::slice::from_ref(&c));
    let actions = [
        A::ClipQuantize { id: id.clone(), beats: Some(32) },
        A::ClipFade { id: id.clone(), mode: Some("beat".into()) },
        A::ClipFade { id: id.clone(), mode: Some("nonsense".into()) }, // not a fade: ignored
    ];
    for a in &actions {
        assert!(rig.e.apply_loop_action(a, None));
    }
    assert!(rig.e.clip_quantum(&id) == Some(32) && rig.e.clip_fade(&id) == Some(LoopFade::Beat));
    // a replay, on a fresh engine, does the same
    let mut fresh = Rig::new();
    fresh.e.restore_clips(&[c]);
    for a in &actions {
        fresh.e.apply_loop_action(a, None);
    }
    assert!(fresh.e.clip_quantum(&id) == Some(32) && fresh.e.clip_fade(&id) == Some(LoopFade::Beat));
    fresh.e.apply_loop_action(&A::ClipFade { id: id.clone(), mode: None }, None);
    assert!(fresh.e.clip_fade(&id).is_none());
    assert!(!fresh.e.apply_loop_action(&A::Rewind { deck: "A".into() }, None), "not a loop op");
}

// LoopLaunchTests.aClipFollowingItsOwnDeckIgnoresTheLouderOne
#[test]
fn a_clip_following_its_own_deck_ignores_the_louder_one() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes(); // both from A at 120
    rig.settle();
    use djrust::json::JsonValue;
    let mut spec = djrust::TrackSpec::new("house", 3.0, None);
    spec.extra.set("bpm", JsonValue::Number(126.0));
    assert!(rig.e.generate("B", &spec));
    rig.e.play("B");
    let tempo = |rig: &Rig, id: &str| rig.e.loop_target_bpm(id).unwrap_or(0.0);
    assert_eq!(rig.e.loop_follow(), LoopFollow::Louder);
    rig.e.set_clip_follow(&one, Some(LoopFollow::Source));
    assert!(rig.e.clip_follow(&one) == Some(LoopFollow::Source) && rig.e.clip_follow(&two).is_none());
    assert!(rig.e.follow_for(Some(&one)) == LoopFollow::Source && rig.e.follow_for(Some(&two)) == LoopFollow::Louder);
    rig.e.set_crossfader(1.0); // all B
    rig.run(4096);
    assert_eq!(rig.e.loops.dominant_deck().as_deref(), Some("B"));
    assert!((tempo(&rig, &two) - 126.0).abs() < 1e-6, "the row's: the louder deck");
    assert!((tempo(&rig, &one) - 120.0).abs() < 1e-6, "its own deck, not the louder");

    // the other way round: the row follows each loop's own deck, one clip the louder
    rig.e.set_clip_follow(&one, None);
    rig.e.set_loop_follow(LoopFollow::Source);
    rig.e.set_clip_follow(&two, Some(LoopFollow::Louder));
    rig.run(4096);
    assert_eq!(rig.e.loops.dominant_deck().as_deref(), Some("B"), "kept up while a clip follows it");
    assert!((tempo(&rig, &two) - 126.0).abs() < 1e-6, "its own: the louder deck");
    assert!((tempo(&rig, &one) - 120.0).abs() < 1e-6, "the row's: its own deck");
    assert_eq!(rig.e.loop_clock(Some(&two), None).ref_deck.as_deref(), Some("B"));
    assert_eq!(rig.e.loop_clock(Some(&one), None).ref_deck.as_deref(), Some("A"));

    // None follows the row again
    rig.e.set_clip_follow(&two, None);
    rig.run(4096);
    assert!(rig.e.clip_follow(&two).is_none());
    assert!((tempo(&rig, &two) - 120.0).abs() < 1e-6, "back on its own deck");
}

// LoopRecordTests.loopsFollowTheLouderDeck
#[test]
fn loops_follow_the_louder_deck() {
    let mut rig = Rig::new();
    let (one, _) = rig.two_takes(); // taken from A at 120
    rig.settle();
    use djrust::json::JsonValue;
    let mut spec = djrust::TrackSpec::new("house", 3.0, None);
    spec.extra.set("bpm", JsonValue::Number(126.0));
    assert!(rig.e.generate("B", &spec));
    rig.e.play("B");
    let tempo = |rig: &Rig| rig.e.loop_target_bpm(&one).unwrap_or(0.0);
    assert_eq!(rig.e.loop_follow(), LoopFollow::Louder, "the default");
    rig.run(4096);
    assert!(rig.e.loops.dominant_deck().as_deref() == Some("A") && (tempo(&rig) - 120.0).abs() < 1e-6, "A is up");
    rig.e.set_crossfader(1.0); // all B
    rig.run(4096);
    assert!(rig.e.loops.dominant_deck().as_deref() == Some("B") && (tempo(&rig) - 126.0).abs() < 1e-6, "B took over");
    rig.e.set_crossfader(0.5); // equal: no flapping back
    rig.run(4096);
    assert_eq!(rig.e.loops.dominant_deck().as_deref(), Some("B"));
    rig.e.set_loop_follow(LoopFollow::Source);
    rig.run(4096);
    assert!((tempo(&rig) - 120.0).abs() < 1e-6, "its own deck again");
    rig.e.set_loop_follow(LoopFollow::Louder);
    rig.e.set_deck_mute("B", true); // a silent deck is not the louder one
    rig.run(4096);
    assert_eq!(rig.e.loops.dominant_deck().as_deref(), Some("A"));
}

// LoopRecordTests.severalClipsLoopAtOnceInTimeWithTheirDeck
#[test]
fn several_clips_loop_at_once_in_time_with_their_deck() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    let looping = rig.e.looping_clips();
    assert!(looping.contains(&one) && looping.contains(&two), "a new take joins, it does not replace");
    rig.run(4096); // a fresh take comes in a lead's time after it lands
    assert!(rig.e.loop_state(&one) == LoopState::On && rig.e.loop_state(&two) == LoopState::On);
    // each voice sits at the same place in the bar as deck A
    rig.run(BAR / 3);
    let a = rig.e.beat_state("A").unwrap().bar_phase();
    for id in [&one, &two] {
        let c = rig.e.clip(id).unwrap();
        let phase = (rig.e.loop_playhead(id).unwrap() * c.bars() as f64) % 1.0;
        let err = (phase - a).abs();
        assert!(err.min(1.0 - err) < 0.01, "{} at {phase} of its bar, A at {a}", c.title);
    }
}

// LoopRecordTests.aClipGoesInAndOutOnTheDownbeat
#[test]
fn a_clip_goes_in_and_out_on_the_downbeat() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();
    rig.decks_to_the_loops_only();
    rig.run(2048);
    rig.e.set_looping(&two, false);
    rig.e.set_looping(&one, false);
    assert_eq!(rig.e.loop_state(&one), LoopState::Stopping);
    rig.run(BAR + 4096);
    assert!(rig.e.loop_state(&one) == LoopState::Off && rig.e.looping_clips().is_empty());
    // in again: silent until A's next downbeat, sounding from it
    let clock = rig.e.loop_clock(Some(&one), None);
    let down = clock.next_downbeat(rig.frame() + LEAD);
    assert_eq!(clock.ref_deck.as_deref(), Some("A"));
    // (Swift asserts 1e-9 of a beat here, which holds when `toGo * framesPerBeat` happens to land
    // on a whole number: the clock's origin is an exact anchor now, and this state's gets
    // 81124.00000000001 and `rounded(.up)` takes the frame after. The same arithmetic as Swift's,
    // `loopbank_golden.rs` pins it; the line is held to the frame it is rounded to)
    assert!(on_line(clock.beat(down), 4.0), "{}", clock.beat(down));
    rig.e.set_looping(&one, true);
    assert_eq!(rig.e.loop_state(&one), LoopState::Starting);
    rig.run_to(down + BAR / 2);
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
    let (before, after) = rig.edge(down);
    assert!(before == 0.0 && after > 0.001, "comes in at frame {down}: {before} {after}");
}

// LoopRecordTests.aClipMovesOntoADeckAndBack
#[test]
fn a_clip_moves_onto_a_deck_and_back() {
    let mut rig = Rig::new();
    let (one, _) = rig.two_takes();
    let (bpm, grid_bpm) = {
        let c = rig.e.clip(&one).unwrap();
        (c.bpm, c.grid().bpm)
    };
    rig.e.play_clip(&one, "B");
    let b = rig.e.deck("B").unwrap();
    assert!(b.clip_id.as_deref() == Some(one.as_str()), "B holds the clip");
    let s = b.sample.as_ref().unwrap();
    assert!(s.looping && s.grid.map(|g| g.bpm) == Some(grid_bpm) && grid_bpm == bpm);
    assert_ne!(rig.e.loop_state(&one), LoopState::On, "moved off the loops under the mixer");
    assert!(rig.e.loops.row.sample_library.contains_key(&one), "a session can name it");
    rig.run(4096);
    rig.e.play_clip(&one, "L");
    assert_eq!(rig.e.loop_state(&one), LoopState::Starting);
    rig.run(BAR + 4096);
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
}

// LoopRecordTests.aClipHasItsOwnLevel
#[test]
fn a_clip_has_its_own_level() {
    let mut rig = Rig::new();
    let (one, _) = rig.two_takes();
    rig.e.apply_loop_action(&djrust::session::action::DeckAction::ClipGain { id: one.clone(), value: 0.25 }, None);
    assert_eq!(rig.e.clip_gain(&one), 0.25);
    rig.e.set_looping(&one, false);
    rig.run(BAR + 4096);
    rig.e.set_looping(&one, true);
    assert_eq!(rig.e.clip_gain(&one), 0.25, "kept across a stop and a start");
}

// LoopRecordTests.cropKeepsTheBeatsBetweenTheEdges
#[test]
fn crop_keeps_the_beats_between_the_edges() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.e.set_clip_gain(&one, 0.5, None);
    let orig = rig.e.clip(&one).unwrap().clone();
    assert!(orig.beats == 4 && rig.e.loop_state(&one) == LoopState::On);
    // nothing to crop, or out of range: refused
    assert!(rig.e.crop_clip(&one, 0, 4).is_none());
    assert!(rig.e.crop_clip(&one, 2, 2).is_none());
    assert!(rig.e.crop_clip(&one, 1, 5).is_none());
    let cid = rig.e.crop_clip(&one, 1, 3).expect("a crop");
    let ids: Vec<String> = rig.e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
    assert!(cid != one && ids == [cid.clone(), two], "in the original's place");
    let c = rig.e.clip(&cid).unwrap();
    assert!(c.beats == 2 && c.length_label() == "2 beats" && c.bpm == orig.bpm);
    assert!(c.start_beat == orig.start_beat + 1.0 && c.source == "A");
    let per_beat = orig.buffer.length() / 4;
    assert_eq!(c.buffer.length(), 2 * per_beat);
    assert_eq!(c.buffer.left[..], orig.buffer.left[per_beat..3 * per_beat], "beats 2 and 3 of the original, sample for sample");
    assert_eq!(rig.e.clip_gain(&cid), 0.5, "its level carries over");
    rig.run(4096);
    assert_eq!(rig.e.loop_state(&one), LoopState::Off, "the original stopped looping");
    assert_eq!(rig.e.loop_state(&cid), LoopState::On, "the cropped one carries on");
    assert!(rig.e.loops.row.sample_library.contains_key(&cid));
}

// LoopRecordTests.aTakeKnowsWhereItCameFrom
#[test]
fn a_take_knows_where_it_came_from() {
    use djrust::control::controller::DeckView;
    use djrust::session::types::SequenceStep;
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.e.set_strip("A", djrust::fx::StripKnob::EqLow, 0.2);
    rig.e.set_pitch("A", 0.25); // the tempo fader (`a.pitch = 0.25`), not the momentary nudge
    rig.e.toggle_stem("A", "mic");
    rig.run_to(BAR + 100);
    rig.e.loop_record("A", true, None);
    rig.run_to(2 * BAR - 100);
    assert!(rig.e.is_recording_loop("A"));
    rig.e.loop_record("A", false, None);
    for _ in 0..200 {
        if !rig.e.loops.row.clips.is_empty() {
            break;
        }
        rig.run(BLOCK as i64);
    }
    let one = rig.e.loops.row.clips.first().cloned().expect("a take");
    let p = one.provenance.clone().expect("it has provenance");
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(String::from);
    assert!(s("deck").as_deref() == Some("A") && s("mode").as_deref() == Some("net"));
    assert_eq!(p.get("source").unwrap().get("kind").unwrap().as_str(), Some("net"));
    assert_eq!(p.get("source").unwrap().get("share").unwrap().get("genre").unwrap().as_str(), Some("techno"));
    let src = rig.e.deck("A").unwrap().playing_source().unwrap();
    assert_eq!(s("shareCID"), djrust::journal::stores::favorite_cid(&src), "names the track it came from");
    assert_eq!(p.get("bar").unwrap().as_i64(), Some(2));
    assert_eq!(p.get("startBeat").unwrap().as_f64(), Some(4.0));
    assert_eq!(p.get("bars").unwrap().as_i64(), Some(1));
    let tempo = p.get("tempo").unwrap();
    assert!((tempo.get("bpm").unwrap().as_f64().unwrap() - 120.0 * 1.02).abs() < 1e-6);
    assert_eq!(tempo.get("pitch").unwrap().as_f64(), Some(0.25));
    assert_eq!(p.get("params").unwrap().get("eqLow").unwrap().as_f64(), Some(0.2));
    assert_eq!(p.get("stemsMuted").and_then(|v| v.strings()), Some(vec!["mic".to_string()]));
    assert!(p.get("rolesOff").and_then(|v| v.strings()).is_some_and(|r| r.contains(&"melody".to_string())));
    assert!(s("recorded").is_some() && s("build").is_some());
    assert_eq!(DeckView::effective_bpm(rig.e.deck("A").unwrap()), Some(tempo.get("bpm").unwrap().as_f64().unwrap()));

    // a crop and a sequence say what they were made from
    let cropped = rig.e.crop_clip(&one.id, 0, 2).expect("a crop");
    let cp = rig.e.clip(&cropped).unwrap().provenance.clone().unwrap();
    assert_eq!(cp.get("op").unwrap().as_str(), Some("crop"));
    assert_eq!(cp.get("from").unwrap().as_str(), Some(one.id.as_str()));
    assert_eq!(cp.get("beats").unwrap().ints(), Some(vec![0, 2]));
    assert_eq!(cp.get("parent").unwrap().canonical_js(), p.canonical_js());
    let seq = rig.e.make_sequence(&[SequenceStep::new(&cropped, 2)], None).expect("a sequence");
    let sp = rig.e.clip(&seq).unwrap().provenance.clone().unwrap();
    assert_eq!(sp.get("op").unwrap().as_str(), Some("sequence"));
    let first = sp.get("steps").unwrap().as_array().unwrap()[0].get("provenance").unwrap().canonical_js();
    assert_eq!(first, cp.canonical_js(), "outlives the clips it used");
}

// `LoopProvenance.capture` writes `macros` from the net macros running (by Swift's case name)
// and the master ones; `ClipRetune` refuses a take with any (`LoopRetuneTests`'
// `sequencesMacrosAndBareClipsAreRefused` holds the refusal on a hand-made provenance: this is
// the capture that makes it happen). Rust-only: no Swift test records a take under a macro.
#[test]
fn a_take_recorded_under_a_net_macro_knows_it_and_is_not_retunable() {
    use djrust::wave::NetMacro;
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(BAR + 100);
    rig.e.fire_duration("A", NetMacro::BeatRepeat, Some(4));
    rig.run(4 * 512);
    rig.e.loop_record("A", true, None);
    rig.run_to(2 * BAR - 100);
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap().clone();
    let p = clip.provenance.as_ref().unwrap();
    assert_eq!(p.get("macros").and_then(|v| v.strings()), Some(vec!["beatRepeat".to_string()]));
    assert_eq!(clip.origin_macros(), ["beatRepeat"]);
    assert_eq!(rig.e.retune_clip(&id, &std::collections::BTreeMap::new(), None), None, "refused: a macro was running");
}

use djrust::deck::beat_grid::BeatGrid;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::host::deck::sample::PreparedSample;

/// A sample whose every value says where it is: left[i] = i / length (`ramp`).
fn ramp(bars: usize) -> SampleBuffer {
    let n = bars * BAR as usize;
    SampleBuffer::new(
        (0..n).map(|i| i as f32 / n as f32).collect(),
        Some((0..n).map(|i| -(i as f32) / n as f32).collect()),
        SR,
    )
}

/// The ramp on deck A with a 120 bpm grid, as `.load(.sample(id: "")…)` + `.grid` leave it.
fn ramp_on_a(rig: &mut Rig, bars: usize) -> SampleBuffer {
    let src = ramp(bars);
    let overview = WaveformOverview::new(&src, 1200);
    let p = PreparedSample {
        title: "ramp".into(),
        buffer: std::sync::Arc::new(src.clone()),
        overview,
        grid: Some(BeatGrid::new(120.0, 0.0, 1.0)),
    };
    assert!(rig.e.load_sample("A", p));
    rig.e.play("A");
    src
}

// LoopRecordTests.aSampleTakeNamesItsSampleByContent
#[test]
fn a_sample_take_names_its_sample_by_content() {
    let mut rig = Rig::new();
    let src = ramp_on_a(&mut rig, 8);
    rig.run_to(BAR + BAR / 2);
    rig.e.loop_record("A", true, None);
    rig.run_to(2 * BAR + BAR / 4);
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let s = rig.e.clip(&id).unwrap().provenance.clone().unwrap().get("source").unwrap().clone();
    assert!(s.get("kind").unwrap().as_str() == Some("sample") && s.get("title").unwrap().as_str() == Some("ramp"));
    assert_eq!(s.get("id").unwrap().as_str(), Some(src.content_id().as_str()));
    assert_eq!(s.get("grid").unwrap().get("bpm").unwrap().as_f64(), Some(120.0));
}

// LoopRecordTests.aSampleTakeSnapsToTheGrid
#[test]
fn a_sample_take_snaps_to_the_grid() {
    let mut rig = Rig::new();
    let src = ramp_on_a(&mut rig, 8);
    rig.run_to(BAR + BAR / 2);
    assert!(rig.e.loop_record("A", true, None));
    rig.run_to(3 * BAR + BAR / 4);
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 3 && clip.buffer.length() == 3 * BAR as usize);
    assert!((clip.bpm - 120.0).abs() < 1e-9);
    // The first sample is the source's bar line, give or take the motor's spin-up and a block.
    let at = clip.buffer.left[0] as f64 * src.length() as f64;
    let bar_line = (at / BAR as f64).round() * BAR as f64;
    assert!((at - bar_line).abs() < 2048.0, "starts {} samples off a bar line", at - bar_line);
}

// LoopRecordTests.pressingAStoppedDeckDoesNothing
#[test]
fn pressing_a_stopped_deck_does_nothing() {
    let mut rig = Rig::new();
    assert!(rig.e.load_demo("A"));
    assert!(!rig.e.loop_record("A", true, None));
    assert!(!rig.e.is_recording_loop("A"));
    assert!(!rig.e.loop_record("A", false, None));
    assert!(!rig.e.loop_record("L", true, None), "the loop channel does not record itself");
}

// LoopRecordTests.theLoopChannelHasItsOwnMute
#[test]
fn the_loop_channel_has_its_own_mute() {
    let mut rig = Rig::new();
    // L starts at trim -3 dB (`DJEngine.loopChannelGain`: trim 1/sqrt 2), so the strip plays trim
    // x volume, not the volume alone
    let trim = djrust::host::deck::trim_for_gain(rig.e.loops.gain);
    assert!((trim - 0.5f64.sqrt()).abs() < 1e-6);
    rig.e.set_loop_volume(1.0);
    assert!((rig.e.loop_channel_level() - trim).abs() < 1e-12);
    rig.e.set_loop_volume(0.5);
    assert_eq!(rig.e.loops.volume, 0.5);
    rig.e.set_loop_mute(true);
    assert_eq!(rig.e.loop_channel_level(), 0.0);
    rig.e.set_loop_mute(false);
    assert!((rig.e.loop_channel_level() - trim * 0.5).abs() < 1e-12);
    assert_eq!(djrust::host::deck::loops::LOOP_VOLUME, 0.8, "Deck.volume's default, not 0.9");
    // and it is the strip that plays it: a clip on L is silent while L is muted
    let c = test_clip(1);
    let id = c.id.clone();
    rig.e.restore_clips(&[c]);
    rig.e.set_loop_volume(1.0);
    rig.e.set_looping(&id, true);
    rig.settle();
    rig.e.set_loop_mute(true);
    rig.run(4096);
    let n = rig.master.len();
    rig.run(4096);
    assert!(rig.master[n..].iter().all(|v| v.abs() < 1e-9), "muted");
    rig.e.set_loop_mute(false);
    rig.run(4096);
    let n = rig.master.len();
    rig.run(4096);
    assert!(rig.master[n..].iter().any(|v| v.abs() > 0.001), "and back");
}

fn stock_audio() -> SampleBuffer {
    let n = (2 * BAR - 3) as usize;
    let tone: Vec<f32> = (0..n)
        .map(|i| (0.3 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 48_000.0).sin()) as f32)
        .collect();
    SampleBuffer::new(tone.clone(), Some(tone), SR)
}

// LoopRecordTests.anImportJoinsTheRowAndLoopsInTimeOnce (the session recorder and player are the
// later wave's: the recorded form is checked through `recorded_loop_action`)
#[test]
fn an_import_joins_the_row_and_loops_in_time_once() {
    use djrust::json::{JsonObject, JsonValue};
    use djrust::session::action::DeckAction;
    let mut rig = Rig::new();
    ramp_on_a(&mut rig, 8);
    rig.run_to(BAR + BAR / 3);
    let audio = stock_audio();
    let prov = JsonValue::Object(JsonObject::from_pairs([
        ("kind", JsonValue::str("stock")),
        ("title", JsonValue::str("Tone 01")),
    ]));
    let act = DeckAction::ClipImport { id: String::new(), title: "Tone 01".into(), bpm: 0.0, beats: 8, provenance: Some(prov.clone()) };
    let rec = rig.e.recorded_loop_action(&act, Some(&audio));
    let DeckAction::ClipImport { id: rid, bpm: rbpm, beats: rbeats, provenance: rp, .. } = &rec else { panic!() };
    assert!(*rid == audio.content_id() && *rbeats == 8 && rp.as_ref() == Some(&prov));
    assert!(rig.e.apply_loop_action(&act, Some(std::sync::Arc::new(audio.clone()))));
    let clip = rig.e.clip(&audio.content_id()).expect("on the row").clone();
    assert!(rig.e.loops.row.clips.len() == 1 && clip.source == "L" && clip.beats == 8);
    assert!(clip.title == "Tone 01" && clip.provenance.as_ref().unwrap().get("kind").unwrap().as_str() == Some("stock"));
    // its tempo from its length, so it lasts exactly 2 bars at A's tempo
    let want = 8.0 * 60.0 * 48_000.0 / audio.length() as f64;
    assert!((clip.bpm - want).abs() < 1e-9);
    assert!((rbpm - want).abs() < 1e-9, "recorded by content id and tempo");
    assert_eq!(rig.e.loop_state(&clip.id), LoopState::Starting, "it waits for the next launch boundary");
    rig.run_to(3 * BAR + BAR / 3);
    assert_eq!(rig.e.loop_state(&clip.id), LoopState::On);
    let phase = (rig.e.loop_playhead(&clip.id).unwrap() * 2.0) % 1.0;
    let a = rig.e.beat_state("A").unwrap().bar_phase();
    let err = (phase - a).abs();
    assert!(err.min(1.0 - err) < 0.01, "the import at {phase} of its bar, A at {a}");
    // the same audio again: still one clip, still looping
    let again = DeckAction::ClipImport { id: String::new(), title: "Tone 01 again".into(), bpm: 0.0, beats: 8, provenance: Some(prov.clone()) };
    rig.e.apply_loop_action(&again, Some(std::sync::Arc::new(audio.clone())));
    assert!(rig.e.loops.row.clips.len() == 1 && rig.e.loops.row.clips[0].title == "Tone 01");
    assert_eq!(rig.e.loop_state(&clip.id), LoopState::On);
    // removed, then imported again: the removed one comes back, not a second copy
    rig.e.apply_loop_action(&DeckAction::ClipRemove { id: clip.id.clone() }, None);
    rig.e.apply_loop_action(&act, Some(std::sync::Arc::new(audio.clone())));
    assert!(rig.e.loops.row.clips.len() == 1 && rig.e.loops.row.removed.is_empty());
    // replayed by content id on a fresh engine: Swift's literal is
    // `fresh.sampleResolver = { $0 == audio.contentID ? audio : nil }` with nothing in the
    // sample library (`buffer ?? sampleLibrary[id] ?? sampleResolver?(id)`)
    let mut fresh = Rig::new();
    let (cid, resolvable) = (audio.content_id(), std::sync::Arc::new(audio.clone()));
    fresh.e.sample_resolver = Some(Box::new(move |i| (i == cid).then(|| resolvable.clone())));
    assert!(fresh.e.loops.row.sample_library.is_empty());
    assert!(fresh.e.apply_loop_action(&rec, None));
    let back = fresh.e.clip(&audio.content_id()).expect("the resolver found the audio");
    assert!(back.beats == 8 && (back.bpm - clip.bpm).abs() < 1e-9 && back.source == "L");
    assert_eq!(back.provenance, Some(prov.clone()));
    // the library variant: the audio is in the engine's own sample library, no resolver
    let mut lib = Rig::new();
    lib.e.loops.row.sample_library.insert(audio.content_id(), std::sync::Arc::new(audio.clone()));
    assert!(lib.e.sample_resolver.is_none());
    assert!(lib.e.apply_loop_action(&rec, None));
    let back = lib.e.clip(&audio.content_id()).expect("the library had the audio");
    assert!(back.beats == 8 && (back.bpm - clip.bpm).abs() < 1e-9 && back.source == "L");
    assert_eq!(back.provenance, Some(prov));
    // a resolver that does not know the id leaves the row as it was
    let mut deaf = Rig::new();
    deaf.e.sample_resolver = Some(Box::new(|_| None));
    deaf.e.apply_loop_action(&rec, None);
    assert!(deaf.e.loops.row.clips.is_empty());
    // no audio to import: nothing happens
    let mut bare = Rig::new();
    bare.e.apply_loop_action(&rec, None);
    assert!(bare.e.loops.row.clips.is_empty());
}

// `importClip` puts a removed clip back with `restoreClip`, which pushes every voice's level again
// when the clip comes back soloed (the others go quiet): LoopSound.swift `pushClipLevels`,
// LoopClipEdits.swift `restoreClip`. Parity form (a): the master's level says who is heard.
#[test]
fn an_import_that_puts_back_a_soloed_clip_silences_the_others() {
    let mut rig = Rig::new();
    let (x, y) = (test_clip(1), test_clip(2));
    let (xid, yid) = (x.id.clone(), y.id.clone());
    let xaudio = (*x.buffer).clone();
    rig.e.restore_clips(&[x, y]);
    rig.e.set_loop_volume(1.0);
    rig.e.set_looping(&xid, true);
    rig.e.set_looping(&yid, true);
    rig.settle();
    // the clips are constant-ish (n x 0.01 + a tiny ramp): the mean says which are heard
    let mean = |rig: &Rig| -> f64 {
        let w = &rig.master[rig.master.len() - 4096..];
        w.iter().map(|v| *v as f64).sum::<f64>() / w.len() as f64
    };
    let both = mean(&rig);
    assert!(both > 0.02, "both are heard: {both}");
    rig.e.set_clip_solo(&xid, true);
    rig.run(8192);
    let solo = mean(&rig);
    assert!(solo > 0.005 && solo < both * 0.6, "x alone: {solo} of {both}");
    // taking the soloed clip off the row ends its solo: y is heard again
    rig.e.remove_clip(&xid);
    rig.run(8192);
    let y_alone = mean(&rig);
    assert!(y_alone > solo * 1.4, "y alone is louder than x alone: {y_alone} vs {solo}");
    // importing it again brings it back soloed: y goes quiet at once, x comes in on the next line
    assert_eq!(rig.e.import_clip(xaudio, "x", Some(120.0), 4, None).as_deref(), Some(xid.as_str()));
    assert!(rig.e.is_clip_soloed(&xid));
    rig.settle();
    let after = mean(&rig);
    assert!(after > 0.005 && after < both * 0.6, "x alone again: {after} of {both}");
}

// LoopRecordTests.inpulseLoopPadsSwitchLoopsAndLightWhileTheyPlay
#[test]
fn inpulse_loop_pads_switch_loops_and_light_while_they_play() {
    use djrust::host::deck::Rig as ControllerRig;
    const INPULSE: &str = "DJControl Inpulse 200 Mk3";
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    let mut c = ControllerRig::new(std::mem::replace(&mut rig.e, rig_engine_placeholder()));
    assert!(c.attach(INPULSE));
    // (the engine lives in the controller rig from here; frames are rendered through `rig.r`)
    let mut now = 0.0;
    let render = |c: &mut ControllerRig, rig: &mut Rig, frames: i64| {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let to = c.engine.loop_frames() + frames;
        while c.engine.loop_frames() < to {
            rig.r.render_block(&mut l, &mut r);
            c.engine.service_loops();
        }
    };
    c.engine.stop_all_loops();
    render(&mut c, &mut rig, BAR + 4096);
    assert!(c.engine.looping_clips().is_empty());
    let has = |frames: &[(String, Vec<[u8; 3]>)], m: [u8; 3]| frames.iter().any(|(_, v)| v.contains(&m));
    let _ = c.led_frames(now);
    // deck A, LOOP pad mode (channel 6): pad 1 (0x28) = loop 1, pad 2 (0x2A: the notes run
    // down the columns) = loop 2
    for b in [[0x96u8, 0x28, 0x7F], [0x96, 0x28, 0x00]] {
        now += 0.01;
        c.route(INPULSE, &b, now);
    }
    assert!(c.engine.loop_state(&one) == LoopState::Starting && c.engine.loop_state(&two) == LoopState::Off);
    render(&mut c, &mut rig, BAR + 4096);
    assert_eq!(c.engine.loop_state(&one), LoopState::On);
    assert!(has(&c.led_frames(now), [0x96, 0x28, 0x7F]), "its pad lights while it plays");
    for b in [[0x96u8, 0x2A, 0x7F], [0x96, 0x28, 0x7F]] {
        now += 0.01;
        c.route(INPULSE, &b, now);
    }
    assert!(c.engine.loop_state(&two) == LoopState::Starting && c.engine.loop_state(&one) == LoopState::Stopping);
    render(&mut c, &mut rig, BAR + 4096);
    assert!(c.engine.loop_state(&two) == LoopState::On && c.engine.loop_state(&one) == LoopState::Off);
    let leds = c.led_frames(now);
    assert!(has(&leds, [0x96, 0x28, 0x00]) && has(&leds, [0x96, 0x2A, 0x7F]));
    // a pad with no loop in its slot does nothing, and stays dark
    c.route(INPULSE, &[0x96, 0x2B, 0x7F], now + 0.1);
    assert_eq!(c.engine.looping_clips(), [two.clone()]);
    // deck B's pads are loops 5-8: none yet
    c.route(INPULSE, &[0x97, 0x28, 0x7F], now + 0.2);
    assert_eq!(c.engine.looping_clips(), [two]);
}

fn rig_engine_placeholder() -> djrust::host::deck::Engine {
    let mut c = djrust::host::deck::EngineConfig::new(SR);
    c.decks = Vec::new();
    djrust::host::deck::engine(c).0
}

/// The row's engine moved into a controller rig, the renderer kept to run it.
fn controllers(rig: Rig) -> (djrust::host::deck::Rig, djrust::host::deck::Renderer) {
    (djrust::host::deck::Rig::new(rig.e), rig.r)
}

fn render_frames(c: &mut djrust::host::deck::Rig, r: &mut djrust::host::deck::Renderer, frames: i64) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let to = c.engine.loop_frames() + frames;
    while c.engine.loop_frames() < to {
        r.render_block(&mut l, &mut rr);
        c.engine.service_loops();
    }
}

// LoopRecordTests.mpkBankBPadsAreLoopsOneToEight
#[test]
fn mpk_bank_b_pads_are_loops_one_to_eight() {
    const MPK: &str = "MPK Mini Mk II";
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    let (mut c, mut r) = controllers(rig);
    assert!(c.attach(MPK));
    c.engine.stop_all_loops();
    render_frames(&mut c, &mut r, BAR + 4096);
    assert!(c.engine.looping_clips().is_empty());
    // NOTE mode, bank B, channel 10: pad 1 (note 44) = loop 1, pad 2 (45) = loop 2
    c.route(MPK, &[0x99, 0x2C, 0x7F], 0.01);
    c.route(MPK, &[0x89, 0x2C, 0x00], 0.02);
    assert!(c.engine.loop_state(&one) == LoopState::Starting && c.engine.loop_state(&two) == LoopState::Off);
    c.route(MPK, &[0x99, 0x2D, 0x7F], 0.03);
    assert_eq!(c.engine.loop_state(&two), LoopState::Starting);
    // pad 8 (note 51) is loop 8: no loop there, nothing happens
    c.route(MPK, &[0x99, 0x33, 0x7F], 0.04);
    render_frames(&mut c, &mut r, BAR + 4096);
    let mut looping = c.engine.looping_clips();
    looping.sort();
    let mut want = vec![one, two];
    want.sort();
    assert_eq!(looping, want);
}

// LoopRecordTests.apcShiftTurnsTheGridIntoTheLoopPage
#[test]
fn apc_shift_turns_the_grid_into_the_loop_page() {
    const APC: &str = "APC mini mk2 Control";
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    let (mut c, mut r) = controllers(rig);
    assert!(c.attach(APC));
    c.engine.stop_all_loops();
    render_frames(&mut c, &mut r, BAR + 4096);
    assert!(c.engine.looping_clips().is_empty());
    let has = |frames: &[(String, Vec<[u8; 3]>)], m: [u8; 3]| frames.iter().any(|(_, v)| v.contains(&m));
    // the top-left pad (56) is the aimed deck's first stinger: white, dim
    assert!(has(&c.led_frames(0.0), [0x92, 56, 3]));
    c.route(APC, &[0x90, 0x7A, 0x7F], 0.01); // SHIFT down
    // the loop page: loops are dark until they play (loop 1 is there but off)
    assert!(has(&c.led_frames(0.02), [0x92, 56, 0]));
    c.route(APC, &[0x90, 56, 0x7F], 0.03); // SHIFT + top-left = loop 1
    c.route(APC, &[0x80, 56, 0x00], 0.04);
    assert!(c.engine.loop_state(&one) == LoopState::Starting && c.engine.loop_state(&two) == LoopState::Off);
    assert!(has(&c.led_frames(0.05), [0x9E, 56, 48]), "violet, blinking on its way in");
    render_frames(&mut c, &mut r, BAR + 4096);
    assert_eq!(c.engine.loop_state(&one), LoopState::On);
    assert!(has(&c.led_frames(0.06), [0x96, 56, 48]), "violet, full while it plays");
    // the pad right of it is loop 2
    c.route(APC, &[0x90, 57, 0x7F], 0.07);
    assert_eq!(c.engine.loop_state(&two), LoopState::Starting);
    // SHIFT up: the grid's own colours come back, and the loops keep playing
    c.route(APC, &[0x80, 0x7A, 0x00], 0.08);
    let back = c.led_frames(0.09);
    assert!(has(&back, [0x92, 56, 3]) && has(&back, [0x92, 57, 3]));
    render_frames(&mut c, &mut r, BAR + 4096);
    let mut looping = c.engine.looping_clips();
    looping.sort();
    let mut want = vec![one, two];
    want.sort();
    assert_eq!(looping, want);
}

// LoopLaunchTests.aClipsFollowRoundTripsRecordsAndGoesWithIt (the engine's parts: the launch
// overrides kept through a removal, a journal checkpoint holding the follow and saying nothing
// for clips without one, and restoring it into a fresh engine)
#[test]
fn a_clips_follow_goes_with_it_and_into_a_checkpoint() {
    use djrust::deck::clips::{ClipLaunch, ClipSound};
    use djrust::journal::state::ClipCheckpoint;
    use std::collections::BTreeMap;
    let mut rig = Rig::new();
    let (c, d) = (test_clip(1), test_clip(2));
    let (cid, did) = (c.id.clone(), d.id.clone());
    rig.e.restore_clips(&[c.clone(), d.clone()]);
    rig.e.set_clip_follow(&cid, Some(LoopFollow::Source));
    assert_eq!(rig.e.clip_launch(), BTreeMap::from([(cid.clone(), ClipLaunch { follow: Some(LoopFollow::Source), ..ClipLaunch::default() })]), "only clips with one");
    // kept with the launch overrides, through a removal
    let checkpoint = rig.e.clip_checkpoints();
    rig.e.remove_clip(&cid);
    assert!(rig.e.clip_follow(&cid).is_none() && rig.e.follow_for(Some(&cid)) == LoopFollow::Louder);
    assert_eq!(rig.e.loops.row.removed.first().map(|r| r.sound.clone()), Some(ClipSound { follow: Some(LoopFollow::Source), ..ClipSound::default() }));
    rig.e.restore_clip(&cid, None);
    assert_eq!(rig.e.clip_follow(&cid), Some(LoopFollow::Source));
    // a journal checkpoint holds it, and says nothing for clips without one
    assert_eq!(checkpoint.iter().find(|x| x.id == cid).unwrap().follow.as_deref(), Some("source"));
    assert!(checkpoint.iter().find(|x| x.id == did).unwrap().follow.is_none());
    let mut plain = djrust::journal::state::JournalState::new(48_000.0);
    plain.clips = vec![ClipCheckpoint::new("abc", "x")];
    assert!(!plain.json().canonical_js().contains("\"follow\""));
    // restored into a fresh engine
    let mut fresh = Rig::new();
    let find = |id: &str| if id == cid { Some(c.clone()) } else if id == did { Some(d.clone()) } else { None };
    let warnings = fresh.e.restore_clip_row(&[], &checkpoint, &[], &[], &find);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(fresh.e.clip_follow(&cid) == Some(LoopFollow::Source) && fresh.e.clip_follow(&did).is_none());
    // a clip with no audio is skipped with the warning
    let mut bare = Rig::new();
    let w = bare.e.restore_clip_row(&[], &checkpoint, &[], &[("gone".into(), 0)], &|_| None);
    assert!(w.contains(&format!("clip {cid} has no audio")) && w.contains(&"removed clip gone has no audio".to_string()), "{w:?}");
    assert!(bare.e.loops.row.clips.is_empty());
}

// JournalReplay.apply: a clip the replay didn't make comes from the library; an import with no
// audio is warned of
#[test]
fn a_replayed_action_finds_its_clip_in_the_library() {
    use djrust::session::action::DeckAction;
    let c = test_clip(1);
    let id = c.id.clone();
    let mut rig = Rig::new();
    let mut warnings = Vec::new();
    let a = DeckAction::ClipGain { id: id.clone(), value: 0.5 };
    assert!(rig.e.replay_loop_action(&a, &|i| (i == id).then(|| c.clone()), &mut warnings));
    assert!(rig.e.clip(&id).is_some() && rig.e.clip_gain(&id) == 0.5 && warnings.is_empty());
    let imp = DeckAction::ClipImport { id: "x".into(), title: "Tone".into(), bpm: 0.0, beats: 8, provenance: None };
    rig.e.replay_loop_action(&imp, &|_| None, &mut warnings);
    assert_eq!(warnings, ["imported clip Tone has no audio"]);
    rig.e.replay_library_restore("nope", 0, &|_| None, &mut warnings);
    assert_eq!(warnings[1], "library clip nope has no audio");
    assert!(!rig.e.replay_loop_action(&DeckAction::Rewind { deck: "A".into() }, &|_| None, &mut warnings));
}

// LoopChannelLevelTests.aTakeOnTheLoopChannelSitsLevelWithACentredDeck
// L sits level with A/B at the centred crossfader: A/B take its equal-power -3 dB and L none, so
// L's trim starts at -3 dB (`loopChannelGain`). The gap on the master, measured: the demo loop on A
// with auto-gain, a two-bar take of it looping on L, centred crossfader, the default faders; A
// alone (L muted) for four bars, then L alone (A muted) for four. Swift's bound is 1 LU.
#[test]
fn a_take_on_the_loop_channel_sits_level_with_a_centred_deck() {
    use djrust::host::deck::{engine, EngineConfig};
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    assert_eq!(e.crossfader, 0.5);
    let bar_blocks = (2.0 * SR) as usize / BLOCK; // blocks per 120 bpm bar
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut a_l, mut a_r, mut l_l, mut l_r) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for b in 0..16 * bar_blocks {
        if b == 0 {
            e.set_auto_gain("A", true);
            assert!(e.load_demo("A"));
            e.play("A");
            e.set_loop_mute(true); // the take loops on L at once: A alone first
        } else if b == bar_blocks + bar_blocks / 2 {
            e.loop_record("A", true, Some(2));
        } else if b == bar_blocks + bar_blocks / 2 + 2 {
            e.loop_record("A", false, None);
        } else if b == 9 * bar_blocks {
            e.set_deck_mute("A", true);
            e.set_loop_mute(false);
        }
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        if (5 * bar_blocks..9 * bar_blocks).contains(&b) {
            a_l.extend_from_slice(&l);
            a_r.extend_from_slice(&rr);
        }
        if (11 * bar_blocks..15 * bar_blocks).contains(&b) {
            l_l.extend_from_slice(&l);
            l_r.extend_from_slice(&rr);
        }
    }
    let clip = e.loops.row.clips.last().expect("the take");
    assert_eq!(e.loop_state(&clip.id), LoopState::On);
    assert!(e.auto_gain_db("A") != 0.0);
    let a_out = djrust::loudness::integrated(&a_l, Some(&a_r), SR).unwrap_or(-200.0);
    let l_out = djrust::loudness::integrated(&l_l, Some(&l_r), SR).unwrap_or(-200.0);
    let gap = l_out - a_out;
    eprintln!("A on the master {a_out:.2} LUFS, L {l_out:.2}; gap L - A {gap:.2} LU");
    assert!(a_out > -60.0 && l_out > -60.0);
    assert!(gap.abs() < 1.0, "gap {gap} LU");
}

// MARK: LoopCaptureTests

/// The clip's left channel against the master window (the strip at unity: sample for sample).
fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

// LoopCaptureTests.grabbingTakesTheLastWholeBarsANetDeckPlayed
#[test]
fn grabbing_takes_the_last_whole_bars_a_net_deck_played() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(3 * BAR + BAR / 3); // a third into bar 3
    let id = rig.e.grab_last_bars("A", 2).expect("lands at once: the bars are already played");
    let clip = rig.e.clip(&id).unwrap().clone();
    assert!(clip.bars() == 2 && clip.buffer.length() == 2 * BAR as usize, "bars 1 and 2, downbeat to downbeat");
    assert!(clip.start_beat == 4.0 && (clip.bpm - 120.0).abs() < 1e-9 && clip.source == "A");
    // sample for sample what the deck played after its EQ
    assert!(max_diff(&clip.buffer.left, &rig.master[BAR as usize..3 * BAR as usize]) < 1e-6);
    assert!(clip.buffer.left.iter().any(|v| *v != 0.0));
    let p = clip.provenance.as_ref().unwrap();
    assert_eq!(p.get("grabbed").and_then(|v| v.as_bool()), Some(true));
    assert!(p.get("bars").unwrap().as_i64() == Some(2) && p.get("bar").unwrap().as_i64() == Some(2));
    assert_eq!(p.get("source").unwrap().get("kind").unwrap().as_str(), Some("net"));
    // it loops like a fresh take: in at once, in phase
    assert_eq!(rig.e.loop_state(&id), LoopState::Starting);
    rig.run(4096);
    assert_eq!(rig.e.loop_state(&id), LoopState::On);
}

// LoopCaptureTests.grabbingASampleDeckEndsOnItsGridsLastDownbeat
#[test]
fn grabbing_a_sample_deck_ends_on_its_grids_last_downbeat() {
    let mut rig = Rig::new();
    let src = ramp_on_a(&mut rig, 12);
    rig.run_to(3 * BAR + BAR / 2);
    let id = rig.e.grab_last_bars("A", 2).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 2 && clip.buffer.length() == 2 * BAR as usize && clip.start_beat == 4.0);
    // whole bars of the sample: its first sample is a bar line of the ramp (give or take the
    // motor's spin-up and a block, as for a take)
    let at = clip.buffer.left[0] as f64 * src.length() as f64;
    let bar_line = (at / BAR as f64).round() * BAR as f64;
    assert!((at - bar_line).abs() < 2048.0, "starts {} samples off a bar line", at - bar_line);
    let p = clip.provenance.as_ref().unwrap();
    assert_eq!(p.get("source").unwrap().get("id").unwrap().as_str(), Some(src.content_id().as_str()));
    assert_eq!(p.get("grabbed").and_then(|v| v.as_bool()), Some(true));
}

// LoopCaptureTests.grabbingRefusesWhatTheHistoryDoesNotHold
#[test]
fn grabbing_refuses_what_the_history_does_not_hold() {
    let mut rig = Rig::new();
    assert!(rig.e.grab_last_bars("A", 1).is_none(), "a deck that is not playing");
    assert!(rig.e.grab_last_bars("L", 1).is_none(), "the loop channel");
    rig.net_deck(120.0);
    rig.run_to(3 * BAR + BAR / 3);
    assert!(rig.e.grab_last_bars("A", 4).is_none(), "before the deck started");
    assert!(rig.e.grab_last_bars("A", 0).is_none());
    assert!(rig.e.loops.row.clips.is_empty());
    // past the ring (2^20 frames, ~10.9 bars here): 11 bars back is gone, 10 are still there
    rig.run_to(12 * BAR + BAR / 2);
    assert!(rig.e.grab_last_bars("A", 11).is_none(), "beyond the capture history");
    let id = rig.e.grab_last_bars("A", 10).expect("ten bars are there");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 10 && clip.buffer.length() == 10 * BAR as usize && clip.start_beat == 8.0);
    assert!(max_diff(&clip.buffer.left, &rig.master[2 * BAR as usize..12 * BAR as usize]) < 1e-6);
}

// LoopCaptureTests.aTapRecordsExactlyTheBarsItNames
#[test]
fn a_tap_records_exactly_the_bars_it_names() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(BAR + BAR / 3);
    rig.e.loop_record("A", true, Some(2));
    rig.run(4096); // ~85 ms
    rig.e.loop_record("A", false, None);
    let p = rig.e.loop_take_progress("A").unwrap();
    assert!(p.0 == 2 && p.1, "shows the bars it will take while they finish");
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 2 && clip.buffer.length() == 2 * BAR as usize && clip.start_beat == 4.0);
    assert!(max_diff(&clip.buffer.left, &rig.master[BAR as usize..3 * BAR as usize]) < 1e-6);
    assert!(!rig.e.is_recording_loop("A"));
}

// LoopCaptureTests.aTapOnASampleDeckCountsBarsAtItsTempo
#[test]
fn a_tap_on_a_sample_deck_counts_bars_at_its_tempo() {
    let mut rig = Rig::new();
    ramp_on_a(&mut rig, 12);
    rig.run_to(BAR + BAR / 2);
    assert!(rig.e.loop_record("A", true, Some(4)));
    rig.run((0.25 * SR).round() as i64 - 512);
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(6 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 4 && clip.buffer.length() == 4 * BAR as usize);
}

// LoopCaptureTests.aHoldIsAHoldWhateverTheTapLength
#[test]
fn a_hold_is_a_hold_whatever_the_tap_length() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(BAR + BAR / 3);
    rig.e.loop_record("A", true, Some(4));
    rig.run((0.25 * SR).round() as i64 + 1024); // just longer than a tap
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 1 && clip.buffer.length() == BAR as usize, "to the next downbeat, as before");
}

// LoopCaptureTests.aTapWithoutALengthIsAShortHold
#[test]
fn a_tap_without_a_length_is_a_short_hold() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(BAR + BAR / 3);
    rig.e.loop_record("A", true, None);
    rig.run(2048);
    rig.e.loop_record("A", false, None);
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap();
    assert!(clip.bars() == 1 && clip.buffer.length() == BAR as usize);
}

// LoopCaptureTests.theInpulseLoopButtonTapsTheEnginesLength
#[test]
fn the_inpulse_loop_button_taps_the_engines_length() {
    const INPULSE: &str = "DJControl Inpulse 200 Mk3";
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.e.set_tap_take_bars(Some(4));
    rig.run_to(BAR + BAR / 3);
    let (mut c, mut r) = controllers(rig);
    assert!(c.attach(INPULSE));
    c.route(INPULSE, &[0x91, 0x03, 0x7F], 0.01); // deck A LOOP down
    render_frames(&mut c, &mut r, 2048);
    c.route(INPULSE, &[0x91, 0x03, 0x00], 0.02); // and up: a tap
    assert_eq!(c.engine.loop_take_progress("A").map(|p| p.0), Some(4));
    assert!(c.engine.deck("A").unwrap().loop_take == Some(true), "the LOOP button sees the take");
    for _ in 0..4000 {
        if c.engine.loops.row.clips.is_empty() {
            render_frames(&mut c, &mut r, BLOCK as i64);
        }
    }
    let clip = &c.engine.loops.row.clips[0];
    assert!(clip.bars() == 4 && clip.buffer.length() == 4 * BAR as usize && clip.start_beat == 4.0);
}

// LoopCaptureTests.aTapLengthOutOfRangeIsRefused (and the replayed tap length: the action sets it)
#[test]
fn a_tap_length_out_of_range_is_refused() {
    use djrust::session::action::DeckAction as A;
    let mut rig = Rig::new();
    rig.e.apply_loop_action(&A::TapBars { bars: Some(0) }, None);
    rig.e.apply_loop_action(&A::TapBars { bars: Some(65) }, None);
    assert_eq!(rig.e.loops.tap_take_bars, Some(2));
    rig.e.apply_loop_action(&A::TapBars { bars: Some(4) }, None);
    assert_eq!(rig.e.loops.tap_take_bars, Some(4), "the replayed tap records 4 bars");
    rig.e.apply_loop_action(&A::TapBars { bars: None }, None);
    assert_eq!(rig.e.loops.tap_take_bars, None, "taps off");
    // the recording says the tap length first, as a replay starts from a fresh engine's 2
    rig.e.apply_loop_action(&A::TapBars { bars: Some(4) }, None);
    assert!(rig.e.loop_settings().contains(&A::TapBars { bars: Some(4) }));
}

// LoopFxTests.thePokeSetsTheClipAndItsVoice (the engine-level half: a poke, a crop, a sequence,
// then the clip taken off the row and put back). Swift's literals: `LoopFX.presets[1].fx` is
// warm, `presets[3].fx` the sequence's own.
#[test]
fn the_poke_sets_the_clip_and_its_voice() {
    use djrust::session::action::DeckAction;
    use djrust::session::types::SequenceStep;
    use djrust::host::deck::PerformCtx;
    let mut rig = Rig::new();
    let c = test_clip(1);
    rig.e.restore_clips(std::slice::from_ref(&c));
    let presets = LoopFx::presets();
    let (warm, other) = (presets[1].1.clone(), presets[3].1.clone());
    rig.e.perform(&DeckAction::ClipFx { id: c.id.clone(), fx: warm.clone() }, PerformCtx::default());
    assert_eq!(rig.e.clip_fx(&c.id), warm);
    assert_eq!(rig.e.clip(&c.id).map(|c| c.fx.clone()), Some(warm.clone()));
    // a crop keeps it; a new sequence of it takes it, an edited one keeps its own
    let cropped = rig.e.crop_clip(&c.id, 0, 2).expect("a crop");
    assert_eq!(rig.e.clip_fx(&cropped), warm);
    let seq = rig.e.make_sequence(&[SequenceStep::new(&cropped, 2)], None).expect("a sequence");
    assert_eq!(rig.e.clip_fx(&seq), warm);
    rig.e.set_clip_fx(&seq, &other, None);
    let edited = rig.e.make_sequence(&[SequenceStep::new(&cropped, 3)], Some(&seq)).expect("an edited sequence");
    assert_eq!(rig.e.clip_fx(&edited), other);
    // removed and put back, it still has it
    rig.e.remove_clip(&cropped);
    assert!(rig.e.clip(&cropped).is_none(), "off the row");
    assert_eq!(rig.e.clip_fx(&cropped), warm, "a removed clip answers for its FX");
    assert!(rig.e.restore_clip(&cropped, None));
    assert_eq!(rig.e.clip_fx(&cropped), warm);
}

// Beyond the Swift literal (`editClip` edits the row and `removedClips`, so `setClipFx` and
// `DeckAction.clipFx` reach a removed clip): the FX set while it is off the row come back with it.
#[test]
fn the_fx_of_a_removed_clip_can_be_set_and_come_back_with_it() {
    use djrust::session::action::DeckAction;
    let mut rig = Rig::new();
    let c = test_clip(1);
    rig.e.restore_clips(std::slice::from_ref(&c));
    let presets = LoopFx::presets();
    let (warm, crunch, fuzz) = (presets[1].1.clone(), presets[2].1.clone(), presets[3].1.clone());
    rig.e.set_clip_fx(&c.id, &warm, None);
    rig.e.remove_clip(&c.id);
    // a direct set on a removed id
    rig.e.set_clip_fx(&c.id, &crunch, None);
    assert_eq!(rig.e.clip_fx(&c.id), crunch.clamped(), "changed in removedClips");
    assert!(rig.e.clip(&c.id).is_none(), "and it is still off the row");
    // and the recorded form: `DeckAction.clipFx` through the loop actions
    assert!(rig.e.apply_loop_action(&DeckAction::ClipFx { id: c.id.clone(), fx: fuzz.clone() }, None));
    assert_eq!(rig.e.clip_fx(&c.id), fuzz.clamped());
    assert!(rig.e.restore_clip(&c.id, None));
    assert_eq!(rig.e.clip(&c.id).map(|c| c.fx.clone()), Some(fuzz.clamped()), "back with the FX it was edited to");
    // an id nobody holds is off, and setting it changes nothing
    assert_eq!(rig.e.clip_fx("nobody"), LoopFx::default());
    rig.e.set_clip_fx("nobody", &warm, None);
    assert_eq!(rig.e.clip_fx("nobody"), LoopFx::default());
}

// "and its voice": the poke reaches the voice the clip plays on (`loopChannel.bank?.send(.fx)`).
#[test]
fn the_poke_reaches_the_voice_the_clip_plays_on() {
    let mut rig = Rig::new();
    let c = test_clip(1);
    rig.e.restore_clips(std::slice::from_ref(&c));
    rig.e.set_looping(&c.id, true);
    rig.settle();
    let v = rig.e.voice_index(&c.id).expect("a voice");
    assert!(!rig.e.loops.mirror.fx_running(v), "dry");
    rig.e.set_clip_fx(&c.id, &LoopFx::presets()[2].1, None);
    rig.run(2 * BLOCK as i64);
    assert!(rig.e.loops.mirror.fx_running(v), "the voice runs the clip's FX");
}

// LoopRecordTests.aNewSampleForgetsTheOldLoopRegion: a deck that looped something (a clip moved
// onto it loops its whole length) and then loads a longer sample plays that sample to its end.
#[test]
fn a_new_sample_forgets_the_old_loop_region() {
    use djrust::deck::turntable::TurntableCommand;
    let mut rig = Rig::new();
    let bar = BAR as f64;
    let src = ramp(1);
    let load = |rig: &mut Rig, src: &SampleBuffer, title: &str| {
        let p = PreparedSample {
            title: title.into(),
            buffer: std::sync::Arc::new(src.clone()),
            overview: WaveformOverview::new(src, 1200),
            grid: None, // `a.load(ramp(bars: 1), title: "short")`: no grid
        };
        assert!(rig.e.load_sample("A", p));
    };
    load(&mut rig, &src, "short");
    // `a.setLoop(0, Double(bar / 2))`: the state `Deck.setLoop` writes and the turntable's loop
    rig.e.turntable("A", TurntableCommand::Loop { start: 0.0, end: bar / 2.0 });
    {
        let s = rig.e.deck_mut("A").unwrap().sample.as_mut().unwrap();
        s.loop_start = Some(0.0);
        s.loop_end = Some(bar / 2.0);
        s.looping = true;
    }
    rig.e.play("A");
    rig.run_to(BAR);
    let p = rig.e.sample_position("A").unwrap();
    assert!(p < bar / 2.0 + 2048.0, "the half-bar loop holds the playhead: {p}");
    let long = ramp(4);
    load(&mut rig, &long, "long");
    let s = rig.e.deck("A").unwrap().sample.as_ref().unwrap();
    assert!(!s.looping && s.loop_start.is_none() && s.loop_end.is_none(), "the old region is forgotten");
    rig.run(3 * BAR);
    let p = rig.e.sample_position("A").unwrap();
    assert!(p > bar, "past the old loop's end: {p}");
}

// LoopRecordTests.theCaptureTapIsOnAndBeforeTheFader: the take is pre-fader, so a closed fader
// still records, and the master is silent meanwhile.
#[test]
fn the_capture_tap_is_on_and_before_the_fader() {
    let mut rig = Rig::new();
    assert!(rig.e.load_demo("A"));
    rig.e.set_volume("A", 0.0); // `a.volume = 0`
    rig.e.play("A");
    rig.run_to(48_000);
    let mirror = rig.e.deck("A").unwrap().mirror.clone();
    let (_, end) = mirror.capture.span().expect("the tap has written");
    assert_eq!(end, mirror.rendered_frames.load(std::sync::atomic::Ordering::Relaxed), "endFrame == renderedFrames");
    let (mut l, mut r) = (Vec::new(), Vec::new());
    mirror.capture.copy(24_000, 48_000, &mut l, &mut r);
    assert!(l.len() == 24_000 && r.len() == 24_000);
    assert!(l.iter().any(|v| *v != 0.0), "the deck played after its EQ, whatever the fader says");
    assert!(rig.master[24_000..].iter().all(|v| *v == 0.0), "the fader is closed");
}

// The tap listens on the deck's EQ unit ("a stereo history of one deck after its EQ and filter",
// CaptureTap.swift): a take sounds like the deck, EQ included. Swift's own check is the take being
// what the master played with the strip at unity (aNetTakeIsWholeBarsFromDownbeatToDownbeat); this
// one moves the EQ and reads the tap.
#[test]
fn the_capture_tap_is_after_the_eq() {
    use djrust::fx::StripKnob;
    let rms = |eq: f64| -> f64 {
        let mut rig = Rig::new();
        assert!(rig.e.load_demo("A"));
        for k in [StripKnob::EqLow, StripKnob::EqMid, StripKnob::EqHigh] {
            rig.e.set_strip("A", k, eq);
        }
        rig.e.play("A");
        rig.run_to(48_000);
        let mirror = rig.e.deck("A").unwrap().mirror.clone();
        let (mut l, mut r) = (Vec::new(), Vec::new());
        mirror.capture.copy(24_000, 48_000, &mut l, &mut r);
        (l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / l.len() as f64).sqrt()
    };
    let (flat, killed) = (rms(0.5), rms(0.0));
    assert!(flat > 1e-3, "the flat deck is heard: {flat}");
    assert!(killed < 0.5 * flat, "the tap hears the EQ: {killed} against {flat}");
}

// `CaptureTap.copy` pads what the ring lost with silence (a take of the right length with a hole),
// so a take whose reader fell behind the ring is not dropped: the press is consumed either way
// (`finishTake` needs `left.count >= n`). Rust-only (Swift has no test of `copy` across the edge).
#[test]
fn a_take_that_was_not_serviced_for_longer_than_the_ring_still_lands_whole() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.run_to(BAR + 100);
    assert!(rig.e.loop_record("A", true, None));
    let start = BAR; // snapped back to the downbeat of bar 1 (`clip.start_beat == 4`)
    // render 11.5 bars (more than the ring's ~10.9) without servicing the take
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let stop = rig.frame() + 11 * BAR + BAR / 2;
    while rig.frame() < stop {
        rig.r.render_block(&mut l, &mut r);
        rig.master.extend_from_slice(&l);
    }
    assert!(rig.e.loop_record("A", false, None));
    // what the ring still holds when the take is serviced for the first time (releasing it
    // services the take once, before another block is rendered)
    let mirror = rig.e.deck("A").unwrap().mirror.clone();
    let oldest = mirror.capture.span().unwrap().0;
    let id = rig.run_until_clip(4 * BAR).expect("a clip, not a dropped take");
    let clip = rig.e.clip(&id).unwrap().clone();
    assert!(clip.bars() >= 12 && clip.start_beat == 4.0, "a take of {} bars", clip.bars());
    assert_eq!(clip.buffer.length() as i64, clip.bars() * BAR, "the length it should have");
    // the head the ring lost is silence, the tail is what the deck played after its EQ
    let end = start + clip.buffer.length() as i64;
    assert!(oldest > start, "the start was lost to the ring: {oldest} vs {start}");
    let hole = (oldest - start) as usize;
    assert!(clip.buffer.left[..hole].iter().all(|v| *v == 0.0), "a hole of silence at the head");
    let from = end as usize - 4 * BAR as usize;
    assert!(
        max_diff(&clip.buffer.left[clip.buffer.length() - 4 * BAR as usize..], &rig.master[from..end as usize]) < 1e-6,
        "the last bars are sample for sample what the deck played"
    );
    assert!(clip.buffer.left[hole..].iter().any(|v| *v != 0.0));
}
