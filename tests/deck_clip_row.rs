//! The loop row's bookkeeping (`ClipRow`), against the Swift suites' literals: the state half
//! of `LoopLibraryTests`, `LoopLibraryRemovalTests`, `LoopRecordTests` (crop, import),
//! `LoopLaunchTests` (scenes) and `LoopRetuneTests` (the retuned clip's place). The voices
//! those tests also watch (`loopState`, a clip carrying on looping) are the engine's: the
//! comments say which assertion that leaves to the host.

use std::collections::BTreeMap;

use djrust::deck::clips::*;
use djrust::deck::loops::LoopFollow;
use djrust::deck::row::*;
use djrust::deck::turntable::SampleBuffer;
use djrust::json::JsonValue;
use djrust::session::model::DeckSource;
use djrust::session::types::{LoopFx, LoopFxMode, SequenceStep};

const RATE: f64 = 48_000.0;

/// A one-bar clip at 120 bpm whose samples say which clip it is (`LoopLibraryRemovalTests.take`).
fn take(n: i32, provenance: Option<JsonValue>) -> LoopClip {
    let l: Vec<f32> = (0..96_000)
        .map(|i| n as f32 * 0.01f32 + (i % 100) as f32 * 1e-4f32)
        .collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    let mut c = LoopClip::from_buffer(
        &format!("A loop {n}"),
        "A",
        4,
        120.0,
        4.0 * n as f64,
        SampleBuffer::new(l, Some(r), RATE),
    );
    c.provenance = provenance.or_else(|| {
        Some(JsonValue::parse(&format!(r#"{{"deck":"A","bar":{n},"title":"t"}}"#)).unwrap())
    });
    c
}

fn ids(row: &ClipRow) -> Vec<String> {
    row.clips.iter().map(|c| c.id.clone()).collect()
}

fn net_provenance() -> JsonValue {
    JsonValue::parse(
        r#"{"deck":"B","title":"techno 7","mode":"net","source":{"kind":"net","share":{"genre":"techno","seed":7}},"section":{"index":2,"name":"drop"}}"#,
    )
    .unwrap()
}

const REC: &str = "2026-10-29T10:00:00Z";

#[test]
fn restored_clips_join_the_row_and_reorder() {
    // LoopLibraryTests.restoredClipsJoinTheRowAndReorder
    let mut row = ClipRow::new();
    let all = [take(1, None), take(2, None), take(3, None)];
    row.restore_clips(&all);
    row.restore_clips(&all[..1]); // already there: not twice
    assert_eq!(ids(&row), all.iter().map(|c| c.id.clone()).collect::<Vec<_>>());
    assert!(row.sample_library.contains_key(&all[1].id), "a session can name them");
    assert_eq!(row.loop_count, 3);
    row.move_clip(&all[2].id, Some(&all[0].id));
    assert_eq!(ids(&row), [all[2].id.clone(), all[0].id.clone(), all[1].id.clone()]);
    row.move_clip(&all[2].id, None);
    assert_eq!(ids(&row), [all[0].id.clone(), all[1].id.clone(), all[2].id.clone()]);
    // worked out from moveClip: itself, an unknown clip and an unknown target
    row.move_clip(&all[0].id, Some(&all[0].id));
    row.move_clip("nope", Some(&all[0].id));
    assert_eq!(ids(&row)[0], all[0].id);
    row.move_clip(&all[0].id, Some("nowhere"));
    assert_eq!(ids(&row), [all[1].id.clone(), all[2].id.clone(), all[0].id.clone()], "to the end");
}

#[test]
fn a_resampled_clip_is_named_by_its_content_too() {
    // registerClipAudio: a library clip at another rate keeps its saved id, and its content
    // id names the audio as well; a sequence's id is its own
    let mut row = ClipRow::new();
    let mut c = take(1, None);
    let saved = c.id.clone();
    c.id = "saved-id".into();
    row.restore_clips(&[c.clone()]);
    assert!(row.sample_library.contains_key("saved-id"));
    assert!(row.sample_library.contains_key(&saved), "its content id too");
    let mut seq = take(2, None);
    seq.id = "seq-abc".into();
    row.restore_clips(&[seq]);
    assert!(row.sample_library.contains_key("seq-abc"));
    assert_eq!(row.sample_library.len(), 3, "no second name for a sequence");
}

#[test]
fn a_sequence_is_its_steps_back_to_back_on_the_row() {
    // LoopLibraryTests.aSequenceIsItsStepsBackToBack: "on the end of the row"
    let mut row = ClipRow::new();
    let (a, b) = (take(1, None), take(2, None));
    row.restore_clips(&[a.clone(), b.clone()]);
    let made = row
        .make_sequence(
            &[SequenceStep::new(&a.id, 2), SequenceStep::new(&b.id, 1)],
            None,
            RATE,
            REC,
        )
        .unwrap();
    assert_eq!(made.replaced, None);
    assert_eq!(ids(&row), [a.id.clone(), b.id.clone(), made.id.clone()]);
    let seq = row.clip(&made.id).unwrap();
    assert_eq!(seq.beats, 12);
    assert!(row.sample_library.contains_key(&made.id));
    // the same steps again: the row already has that audio, nothing is added
    let again = row
        .make_sequence(
            &[SequenceStep::new(&a.id, 2), SequenceStep::new(&b.id, 1)],
            None,
            RATE,
            REC,
        )
        .unwrap();
    assert_eq!(again.id, made.id);
    assert_eq!(row.clips.len(), 3);
}

#[test]
fn editing_a_sequence_keeps_its_place_and_its_level() {
    // LoopLibraryTests.editingASequenceKeepsItsPlaceAndItsLoop (the row half: place, level,
    // steps; "it keeps looping" is the host's)
    let mut row = ClipRow::new();
    let (a, b, c) = (take(1, None), take(2, None), take(3, None));
    row.restore_clips(&[a.clone(), b.clone(), c.clone()]);
    let id = row
        .make_sequence(&[SequenceStep::new(&a.id, 1), SequenceStep::new(&b.id, 1)], None, RATE, REC)
        .unwrap()
        .id;
    row.move_clip(&id, Some(&c.id));
    row.set_clip_gain(&id, 0.3);
    // nothing left: None, and nothing changes
    assert!(row
        .make_sequence(&[SequenceStep::new("gone", 1)], Some(&id), RATE, REC)
        .is_none());
    let made = row
        .make_sequence(
            &[SequenceStep::new(&b.id, 4), SequenceStep::new("gone", 1)],
            Some(&id),
            RATE,
            REC,
        )
        .unwrap();
    let new = row.clips[2].clone();
    assert_eq!(ids(&row), [a.id.clone(), b.id.clone(), new.id.clone(), c.id.clone()], "where it was");
    assert_eq!(new.steps.as_deref(), Some(&[SequenceStep::new(&b.id, 4)][..]), "a missing clip is left out");
    assert_eq!((new.beats, row.clip_gain(&new.id)), (16, 0.3));
    assert_eq!(made.replaced, Some(Replaced { old: id.clone(), new: new.id.clone() }));
    assert!(row.clip(&id).is_none());
}

#[test]
fn an_edited_sequence_keeps_its_own_fx_off_included() {
    // makeSequence: a new sequence takes its steps' shared FX, an edited one keeps its own
    let fx = LoopFx {
        drive: 9.0,
        mode: LoopFxMode::Hard,
        ..LoopFx::default()
    };
    let mut row = ClipRow::new();
    let (mut a, mut b) = (take(1, None), take(2, None));
    a.fx = fx.clone();
    b.fx = fx.clone();
    row.restore_clips(&[a.clone(), b.clone()]);
    let steps = [SequenceStep::new(&a.id, 1), SequenceStep::new(&b.id, 1)];
    let id = row.make_sequence(&steps, None, RATE, REC).unwrap().id;
    assert_eq!(row.clip(&id).unwrap().fx, fx, "the steps' shared FX");
    row.edit_clip(&id, |c| c.fx = LoopFx::default());
    let id2 = row
        .make_sequence(&[SequenceStep::new(&a.id, 2)], Some(&id), RATE, REC)
        .unwrap()
        .id;
    assert_eq!(row.clip(&id2).unwrap().fx, LoopFx::default(), "off, as it had");
}

#[test]
fn crop_keeps_the_beats_between_the_edges_in_the_originals_place() {
    // LoopRecordTests.cropKeepsTheBeatsBetweenTheEdges (the row half)
    let mut row = ClipRow::new();
    let (one, two) = (take(1, None), take(2, None));
    row.restore_clips(&[one.clone(), two.clone()]);
    row.set_clip_gain(&one.id, 0.5);
    row.settings.set_mute(&one.id, true);
    assert!(row.crop_clip(&one.id, 0, 4).is_none());
    assert!(row.crop_clip(&one.id, 2, 2).is_none());
    assert!(row.crop_clip(&one.id, 1, 5).is_none());
    assert!(row.crop_clip("nope", 1, 3).is_none());
    assert_eq!(ids(&row), [one.id.clone(), two.id.clone()], "refusals change nothing");
    let r = row.crop_clip(&one.id, 1, 3).unwrap();
    assert_eq!(r.old, one.id);
    let c = row.clips[0].clone();
    assert!(c.id != one.id && ids(&row) == [c.id.clone(), two.id.clone()], "in the original's place");
    assert_eq!(r.new, c.id);
    assert_eq!(row.clip_gain(&c.id), 0.5, "its level carries over");
    assert!(row.settings.is_muted(&c.id) && !row.settings.is_muted(&one.id), "and its settings");
    assert!(row.sample_library.contains_key(&c.id));
    assert!(row.sample_library.contains_key(&one.id), "a session may still name the original");
}

#[test]
fn names_and_colours_are_kept() {
    // LoopLibraryRemovalTests.namesAndColoursAreKeptAndRecorded
    let mut row = ClipRow::new();
    let (a, b) = (take(1, None), take(2, None));
    row.restore_clips(&[a.clone(), b.clone()]);
    row.rename_clip(&a.id, "  big drop  ");
    row.rename_clip(&b.id, "   ");
    row.set_clip_color(&a.id, Some("orange"));
    row.set_clip_color(&b.id, Some("chartreuse"));
    assert_eq!(
        row.clips.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
        ["big drop".to_string(), b.title.clone()],
        "blank names are ignored"
    );
    assert_eq!(
        row.clips.iter().map(|c| c.color.clone()).collect::<Vec<_>>(),
        [Some("orange".to_string()), None],
        "only palette colours"
    );
    let r = row.crop_clip(&a.id, 0, 2).unwrap();
    let c = row.clip(&r.new).unwrap();
    assert!(c.title == "big drop" && c.color.as_deref() == Some("orange"), "a crop keeps both");
    // a colour that is not in the palette clears; so does None
    row.set_clip_color(&r.new, Some("teal"));
    assert_eq!(row.clip(&r.new).unwrap().color.as_deref(), Some("teal"));
    row.set_clip_color(&r.new, None);
    assert_eq!(row.clip(&r.new).unwrap().color, None);
    // renaming reaches a removed clip too
    row.remove_clip(&b.id).unwrap();
    row.rename_clip(&b.id, "gone but named");
    assert_eq!(row.removed[0].clip.title, "gone but named");
}

#[test]
fn removal_is_newest_first_and_undoing_it_puts_a_clip_back_as_it_was() {
    // LoopLibraryRemovalTests.undoingARemovalPutsItBackAsItWas
    let mut row = ClipRow::new();
    let (a, b, c) = (take(1, None), take(2, None), take(3, None));
    row.restore_clips(&[a.clone(), b.clone(), c.clone()]);
    row.set_clip_gain(&a.id, 0.4);
    assert_eq!(row.remove_clip(&a.id).unwrap().index, 0);
    assert_eq!(row.remove_clip(&c.id).unwrap().index, 1);
    assert!(row.remove_clip("nope").is_none());
    assert_eq!(
        row.removed.iter().map(|r| r.clip.id.clone()).collect::<Vec<_>>(),
        [c.id.clone(), a.id.clone()],
        "newest first"
    );
    assert!(row.restore_clip(&a.id, None).found());
    assert_eq!(ids(&row), [a.id.clone(), b.id.clone()]);
    assert_eq!(row.clip_gain(&a.id), 0.4);
    assert!(row.restore_clip(&c.id, Some(0)).found());
    assert_eq!(ids(&row), [c.id.clone(), a.id.clone(), b.id.clone()], "or where it is asked to go");
    // "not twice": it is no longer removed
    assert_eq!(row.restore_clip(&c.id, None), Restore::NotRemoved);
    assert_eq!(row.clips.len(), 3);
    // `at` is clamped to the row
    row.remove_clip(&c.id).unwrap();
    assert!(row.restore_clip(&c.id, Some(99)).found());
    assert_eq!(row.clips.last().unwrap().id, c.id);
    row.remove_clip(&c.id).unwrap();
    assert!(row.restore_clip(&c.id, Some(-4)).found());
    assert_eq!(row.clips[0].id, c.id);
}

#[test]
fn undoing_a_removal_keeps_its_mute_solo_and_key_lock() {
    // LoopLibraryRemovalTests.undoingARemovalKeepsItsMuteSoloAndKeyLock
    let mut row = ClipRow::new();
    let (a, b, c) = (take(1, None), take(2, None), take(3, None));
    row.restore_clips(&[a.clone(), b.clone(), c.clone()]);
    row.settings.set_mute(&a.id, true);
    row.settings.set_key_lock(&a.id, true);
    row.settings.set_solo(&b.id, true);
    row.settings.set_key_lock(&c.id, false);
    let pushed: Vec<bool> = [&a, &b, &c]
        .iter()
        .map(|x| row.remove_clip(&x.id).unwrap().push_levels)
        .collect();
    assert_eq!(pushed, [false, true, false]);
    assert!(row.settings.solos.is_empty() && row.settings.mutes.is_empty(), "off the row, they are not heard");
    let back: Vec<Restore> = [&a, &b, &c].iter().map(|x| row.restore_clip(&x.id, None)).collect();
    assert_eq!(
        back,
        [
            Restore::Restored { push_levels: false },
            Restore::Restored { push_levels: true },
            Restore::Restored { push_levels: false }
        ]
    );
    assert!(row.settings.is_muted(&a.id) && row.settings.is_key_locked(&a.id, false) && !row.settings.is_soloed(&a.id));
    assert!(row.settings.is_soloed(&b.id) && !row.settings.is_muted(&b.id));
    assert_eq!(row.settings.key_locks.get(&c.id), Some(&false), "its own setting, not the row's");
    assert_eq!(row.settings.audible(&c.id, row.clip_gain(&c.id)), 0.0, "b is soloed again");
}

#[test]
fn a_clips_boundary_and_fade_go_with_it_on_the_row() {
    // LoopLaunchTests.aClipsBoundaryAndFadeGoWithItAndAreForgottenWithIt (through the row)
    let mut row = ClipRow::new();
    let (c, d) = (take(1, None), take(2, None));
    row.restore_clips(&[c.clone(), d.clone()]);
    row.settings.set_quantum(&c.id, Some(8));
    row.settings.set_fade(&c.id, Some(LoopFade::Bar));
    assert_eq!(
        row.clip_launch(),
        BTreeMap::from([(
            c.id.clone(),
            ClipLaunch { quantum: Some(8), fade: Some(LoopFade::Bar), follow: None }
        )])
    );
    row.remove_clip(&c.id).unwrap();
    assert!(row.settings.quantum(&c.id).is_none() && row.settings.fade(&c.id).is_none() && row.clip_launch().is_empty());
    assert_eq!(
        row.removed[0].sound,
        ClipSound { quantum: Some(8), fade: Some(LoopFade::Bar), ..ClipSound::default() }
    );
    row.restore_clip(&c.id, None);
    assert_eq!((row.settings.quantum(&c.id), row.settings.fade(&c.id)), (Some(8), Some(LoopFade::Bar)));
    // the library's, back on a fresh row (unknown ids skipped)
    let mut fresh = ClipRow::new();
    fresh.restore_clips(&[c.clone(), d.clone()]);
    fresh.restore_clip_launch(&BTreeMap::from([
        (c.id.clone(), ClipLaunch { quantum: Some(16), ..ClipLaunch::default() }),
        ("gone".to_string(), ClipLaunch { fade: Some(LoopFade::Off), ..ClipLaunch::default() }),
    ]));
    assert_eq!((fresh.settings.quantum(&c.id), fresh.settings.fade(&c.id)), (Some(16), None));
    assert_eq!(fresh.clip_launch().keys().collect::<Vec<_>>(), [&c.id]);
    // a clip's follow
    fresh.restore_clip_launch(&BTreeMap::from([(
        d.id.clone(),
        ClipLaunch { follow: Some(LoopFollow::Louder), ..ClipLaunch::default() },
    )]));
    assert_eq!(fresh.settings.follow(&d.id), Some(LoopFollow::Louder));
}

#[test]
fn a_removed_clip_comes_back_from_the_library_with_its_provenance_and_place() {
    // LoopLibraryRemovalTests.restoringBringsItBackWithItsProvenanceAndPlace (the next-launch
    // half: rememberRemovedClip from the library, then clipRestore)
    let (a, b, c) = (take(1, None), take(2, Some(net_provenance())), take(3, None));
    let mut dj2 = ClipRow::new();
    dj2.restore_clips(&[a.clone(), c.clone()]);
    let mut back = b.clone();
    back.color = Some("teal".into());
    dj2.remember_removed_clip(back, 1, ClipSound::default());
    assert!(dj2.sample_library.contains_key(&b.id), "the audio joins the sample library");
    assert!(dj2.restore_clip(&b.id, None).found());
    assert_eq!(ids(&dj2), [a.id.clone(), b.id.clone(), c.id.clone()], "where it was");
    let restored = dj2.clip(&b.id).unwrap();
    assert_eq!(
        restored.provenance.as_ref().unwrap().canonical_js(),
        net_provenance().canonical_js()
    );
    assert!(restored.color.as_deref() == Some("teal") && restored.title == b.title);
    assert!(dj2.removed.is_empty());
    // remembering twice keeps one, the newest
    dj2.remember_removed_clip(b.clone(), 0, ClipSound::default());
    dj2.remember_removed_clip(b.clone(), 2, ClipSound::default());
    assert_eq!(dj2.removed.len(), 1);
    assert_eq!(dj2.removed[0].index, 2);
    // restoring a clip that is on the row drops the entry and changes nothing else
    assert_eq!(dj2.restore_clip(&b.id, None), Restore::AlreadyOnRow);
    assert!(dj2.removed.is_empty() && dj2.clips.len() == 3);
}

#[test]
fn delete_forever_forgets_a_removed_clip() {
    // LoopLibraryRemovalTests.deleteForeverDeletesTheAudio (the row half)
    let mut row = ClipRow::new();
    let (a, b) = (take(1, None), take(2, None));
    row.restore_clips(&[a.clone(), b.clone()]);
    row.remove_clip(&b.id).unwrap();
    row.forget_removed_clip(&b.id);
    assert!(!row.restore_clip(&b.id, None).found() && ids(&row) == [a.id.clone()], "no longer restorable");
}

#[test]
fn a_clip_knows_the_track_it_came_from_on_the_row() {
    // LoopLibraryRemovalTests.aClipKnowsTheTrackItCameFrom
    let mut row = ClipRow::new();
    let net = take(1, Some(net_provenance()));
    let other = take(9, None);
    let sample_prov = JsonValue::parse(&format!(
        r#"{{"deck":"A","source":{{"kind":"sample","id":"{}","title":"kick.wav","grid":{{"bpm":120,"offset":0}}}}}}"#,
        other.id
    ))
    .unwrap();
    let sampled = take(2, Some(sample_prov));
    row.restore_clips(&[net.clone(), sampled.clone()]);
    let o = row.clip_origin(&net.id).unwrap();
    let DeckSource::Net(spec) = &o.source else { panic!("a net: {:?}", o.source) };
    assert_eq!(
        (spec.genre.as_str(), spec.seed, o.section),
        ("techno", 7.0, Some(2))
    );
    assert!(row.clip_origin(&sampled.id).is_none(), "its audio is not in the library");
    row.sample_library.insert(other.id.clone(), other.buffer.clone());
    assert!(row.clip_origin(&sampled.id).unwrap().buffer.is_some());
    // a crop (and a crop of a crop) still knows
    let crop = row.crop_clip(&net.id, 0, 2).unwrap().new;
    let crop2 = row.crop_clip(&crop, 1, 2).unwrap().new;
    assert_eq!(row.clip_origin(&crop2).unwrap().section, Some(2));
    let seq = row
        .make_sequence(&[SequenceStep::new(&crop2, 1)], None, RATE, REC)
        .unwrap()
        .id;
    assert_eq!(row.clip(&seq).unwrap().origin_genre().as_deref(), Some("techno"), "and a sequence, by its first step");
    // a removed clip is found too
    row.remove_clip(&sampled.id).unwrap();
    assert!(row.clip_origin(&sampled.id).unwrap().buffer.is_some());
}

#[test]
fn scenes_are_saved_in_row_order_and_follow_a_crop() {
    // LoopLaunchTests.scenesAreRenamedOverwrittenDeletedAndFollowACrop
    let mut row = ClipRow::new();
    let (one, two) = (take(1, None), take(2, None));
    row.restore_clips(&[one.clone(), two.clone()]);
    let looping = [two.id.clone(), one.id.clone()];
    let s = row.save_scene("Scene 1", &looping).unwrap();
    assert_eq!(
        s.clips.iter().map(|c| c.clip_id.clone()).collect::<Vec<_>>(),
        [one.id.clone(), two.id.clone()],
        "in row order"
    );
    row.scenes.rename("Scene 1", "drop");
    let next = row.scenes.next_name();
    row.save_scene(&next, &looping);
    assert_eq!(row.scenes.list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["drop", "Scene 2"]);
    row.scenes.rename("drop", "Scene 2");
    assert_eq!(row.scenes.list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["drop", "Scene 2"], "a name already taken is refused");
    // overwrite: the same name, what plays now
    row.save_scene("drop", std::slice::from_ref(&one.id));
    assert_eq!(row.scenes.list[0].clips.iter().map(|c| c.clip_id.clone()).collect::<Vec<_>>(), [one.id.clone()]);
    // a crop takes the original's place in its scenes
    let cropped = row.crop_clip(&one.id, 0, 2).unwrap().new;
    assert_eq!(row.scenes.list[0].clips[0].clip_id, cropped);
    assert_eq!(
        row.scenes.list[1].clips.iter().map(|c| c.clip_id.clone()).collect::<Vec<_>>(),
        [cropped.clone(), two.id.clone()]
    );
    row.scenes.delete("drop");
    assert_eq!(row.scenes.next_name(), "Scene 3", "Scene 2 is taken");
    // a scene keeps a clip's level and its FX only when it has any (sceneFxAreRecordedAndEncodedOnlyWhenSet)
    row.set_clip_gain(&two.id, 0.25);
    row.edit_clip(&two.id, |c| {
        c.fx = LoopFx { drive: 6.0, ..LoopFx::default() }
    });
    let s = row.save_scene("fx", &[cropped.clone(), two.id.clone()]).unwrap();
    assert_eq!((s.clips[0].gain, s.clips[0].fx.is_none()), (1.0, true));
    assert_eq!((s.clips[1].gain, s.clips[1].fx.as_ref().map(|f| f.drive)), (0.25, Some(6.0)));
    assert!(row.save_scene("", &[]).is_none());
}

#[test]
fn a_retuned_clip_takes_its_place_and_keeps_its_settings() {
    // LoopRetuneTests.aRetunedClipTakesItsPlaceAndKeepsItsSettings (the row half: place, level,
    // colour, FX, key lock, scene, the sequence's step, provenance; "carries on looping" is the
    // host's)
    let mut row = ClipRow::new();
    let before = take(5, None);
    let clip = take(1, Some(net_provenance()));
    row.restore_clips(&[before.clone(), clip.clone()]);
    let seq = row
        .make_sequence(&[SequenceStep::new(&clip.id, 2)], None, RATE, REC)
        .unwrap()
        .id;
    row.move_clip(&clip.id, None);
    let index = row.clips.iter().position(|c| c.id == clip.id).unwrap();
    assert_eq!(index, row.clips.len() - 1);
    let fx = LoopFx { drive: 6.0, mode: LoopFxMode::Tube, ..LoopFx::default() };
    row.set_clip_gain(&clip.id, 0.4);
    row.set_clip_color(&clip.id, Some("teal"));
    row.edit_clip(&clip.id, |c| c.fx = fx.clone());
    row.settings.set_key_lock(&clip.id, true);
    row.save_scene("one", std::slice::from_ref(&clip.id));

    let render = {
        let b = take(2, None);
        SampleBuffer::new(b.buffer.left.clone(), Some(b.buffer.right().to_vec()), RATE)
    };
    let render_id = render.content_id();
    let voices = BTreeMap::from([(3, "square".to_string())]);
    let (new_id, replaced) = row.retune_clip(&clip.id, &voices, render, "dev", REC).unwrap();
    assert!(new_id != clip.id && new_id == render_id);
    assert_eq!(replaced, Some(Replaced { old: clip.id.clone(), new: new_id.clone() }));
    assert_eq!(row.clips[index].id, new_id, "in the take's place");
    assert!(row.clip(&clip.id).is_none());
    let new = row.clip(&new_id).unwrap().clone();
    assert!(new.beats == clip.beats && new.bpm == clip.bpm && new.start_beat == clip.start_beat);
    assert!(new.title == clip.title && new.color.as_deref() == Some("teal") && new.fx == fx);
    assert!(row.clip_gain(&new_id) == 0.4 && row.settings.is_key_locked(&new_id, false));
    assert!(row.scenes.scene("one").unwrap().clips.iter().any(|c| c.clip_id == new_id), "its scene has it");
    assert_eq!(
        row.clip(&seq).unwrap().steps.as_ref().unwrap().iter().map(|s| s.clip_id.clone()).collect::<Vec<_>>(),
        [new_id.clone()],
        "the sequence uses it from now on"
    );
    let p = new.provenance.as_ref().unwrap();
    assert_eq!(p.get("op").and_then(JsonValue::as_str), Some("retune"));
    assert_eq!(p.get("from").and_then(JsonValue::as_str), Some(clip.id.as_str()));
    assert_eq!(new.retuned_voices(), voices);
    let taken = DeckSource::from_json(net_provenance().get("source").unwrap()).unwrap();
    assert_eq!(new.origin_source().unwrap(), taken.with_instrument("square", 3).unwrap());
    assert_eq!(new.origin_section(), clip.origin_section());
    // the same audio: the old id, nothing changes; a clip off the row, or an empty render: None
    let again = {
        let b = row.clip(&new_id).unwrap();
        SampleBuffer::new(b.buffer.left.clone(), b.buffer.right.clone(), RATE)
    };
    // (a stereo buffer hashes as the stereo buffer it was)
    let same = row.retune_clip(&new_id, &voices, again, "dev", REC);
    assert!(matches!(same, Some((ref id, None)) if *id == new_id), "{same:?}");
    assert!(row.retune_clip("nope", &voices, take(3, None).buffer.as_ref().clone_buffer(), "dev", REC).is_none());
}

trait CloneBuffer {
    fn clone_buffer(&self) -> SampleBuffer;
}

impl CloneBuffer for SampleBuffer {
    fn clone_buffer(&self) -> SampleBuffer {
        SampleBuffer::new(self.left.clone(), self.right.clone(), self.sample_rate)
    }
}

#[test]
fn an_import_joins_the_row_once_and_a_removed_one_comes_back() {
    // LoopRecordTests.anImportJoinsTheRowAndLoopsInTimeOnce (the row half)
    let audio = |hz: f32| {
        let n = 2 * 96_000 - 3;
        let l: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        SampleBuffer::new(l.clone(), Some(l), RATE)
    };
    let stock = audio(220.0);
    let id = stock.content_id();
    let len = stock.length();
    let prov = JsonValue::parse(r#"{"op":"import","kind":"stock","title":"Tone 01"}"#).unwrap();
    let mut row = ClipRow::new();
    let got = row
        .import_clip(audio(220.0), "Tone 01", None, 8, Some(prov.clone()), "L")
        .unwrap();
    assert_eq!(got, id);
    let clip = row.clip(&id).unwrap().clone();
    assert!(row.clips.len() == 1 && clip.source == "L" && clip.beats == 8);
    assert_eq!(clip.title, "Tone 01");
    assert_eq!(clip.provenance.as_ref().unwrap().get("kind").and_then(JsonValue::as_str), Some("stock"));
    // its tempo from its length, so it lasts exactly 2 bars at A's tempo
    assert!((clip.bpm - 8.0 * 60.0 * 48_000.0 / len as f64).abs() < 1e-9);
    assert_eq!(clip.start_beat, 0.0);
    assert!(row.sample_library.contains_key(&id));
    // the same audio again: still one clip, the first title kept
    row.import_clip(audio(220.0), "Tone 01 again", None, 8, Some(prov.clone()), "L").unwrap();
    assert!(row.clips.len() == 1 && row.clips[0].title == "Tone 01");
    // removed, then imported again: the removed one comes back, not a second copy
    row.remove_clip(&id).unwrap();
    row.import_clip(audio(220.0), "Tone 01", None, 8, Some(prov), "L").unwrap();
    assert!(row.clips.len() == 1 && row.removed.is_empty());
    // a tempo given is kept; nothing to import is refused
    let mut row = ClipRow::new();
    let id2 = row.import_clip(audio(330.0), "x", Some(97.5), 8, None, "L").unwrap();
    assert_eq!(row.clip(&id2).unwrap().bpm, 97.5);
    assert!(row.import_clip(audio(440.0), "x", None, 0, None, "L").is_none(), "no beats");
    assert!(row.import_clip(audio(440.0), "x", Some(f64::NAN), 8, None, "L").is_none());
    assert!(row.import_clip(audio(440.0), "x", Some(-3.0), 8, None, "L").is_none());
    assert!(row.import_clip(audio(440.0), "x", Some(0.0), 8, None, "L").is_none());
    assert_eq!(row.clips.len(), 1);
}

#[test]
fn clearing_the_row_empties_it_for_a_reset() {
    // clearLoopRow: clips, removed, levels, mutes, solos, key locks, the take count
    let mut row = ClipRow::new();
    let (a, b) = (take(1, None), take(2, None));
    row.restore_clips(&[a.clone(), b.clone()]);
    row.set_clip_gain(&a.id, 0.5);
    row.settings.set_mute(&a.id, true);
    row.settings.set_solo(&b.id, true);
    row.settings.set_key_lock(&b.id, true);
    row.remove_clip(&b.id);
    row.clear();
    assert!(row.clips.is_empty() && row.removed.is_empty() && row.gains.is_empty());
    assert!(row.settings.mutes.is_empty() && row.settings.solos.is_empty() && row.settings.key_locks.is_empty());
    assert_eq!(row.loop_count, 0);
    assert_eq!(row.clip_gain(&a.id), 1.0);
}
