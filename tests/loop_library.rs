//! The loop library on disk (`LoopLibraryTests`, `LoopLibraryRemovalTests`, and the library
//! parts of `LoopLaunchTests`): the row saved and brought back, a removed clip's audio kept under
//! `removed/` with its place, another sample rate, a failed move, scenes and a clip's launch
//! settings kept with the row, sequences and provenance. Each test names its Swift one.

use std::collections::BTreeMap;
use std::sync::Arc;

use djrust::deck::clips::{ClipLaunch, ClipSound, LoopClip, LoopFade, sequence_id};
use djrust::deck::loops::{LoopFollow, LoopScene, LoopState, SceneClip};
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::json::{JsonObject, JsonValue};
use djrust::session::types::SequenceStep;
use djrust::host::deck::{engine, Engine, EngineConfig};
use djrust::host::stores::loops::LoopLibrary;
use djrust::host::stores::temp_path;

/// A clip of `bars` bars at 120 bpm whose samples say which clip it is.
fn clip(n: usize, bars: usize, rate: f64) -> LoopClip {
    let len = (rate * 2.0) as usize * bars;
    let l: Vec<f32> = (0..len).map(|i| n as f32 * 0.01 + (i % 100) as f32 * 1e-4).collect();
    let b = SampleBuffer::new(l.clone(), Some(l.iter().map(|v| -v).collect()), rate);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(
        &b.content_id(),
        &format!("A loop {n}"),
        if n % 2 == 0 { "B" } else { "A" },
        bars as i64,
        120.0,
        (4 * n) as f64,
        Arc::new(b),
        w,
        None,
    )
}

fn c(n: usize) -> LoopClip {
    clip(n, 1, 48_000.0)
}

struct Lib(LoopLibrary);

impl Lib {
    fn new() -> Lib {
        Lib(LoopLibrary::new(temp_path("loops")))
    }
    fn save(&self, clips: &[LoopClip]) -> std::io::Result<()> {
        self.0.save(clips, None, None, 1_700_000_000_000)
    }
    fn files(&self) -> Vec<String> {
        names(&self.0.dir)
    }
    fn removed_files(&self) -> Vec<String> {
        names(&self.0.removed_dir())
    }
    fn ids(&self, rate: f64) -> Vec<String> {
        self.0.load(rate).iter().map(|c| c.id.clone()).collect()
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        // a blocked folder is made writable again before it goes
        let _ = std::fs::remove_dir_all(&self.0.dir);
    }
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn dj(rate: f64) -> Engine {
    let mut cfg = EngineConfig::new(rate);
    cfg.extended = false;
    engine(cfg).0
}

fn ids(e: &Engine) -> Vec<String> {
    e.loops.row.clips.iter().map(|c| c.id.clone()).collect()
}

// LoopLibraryTests.aSavedRowComesBackAsItWas
#[test]
fn a_saved_row_comes_back_as_it_was() {
    let lib = Lib::new();
    assert!(lib.0.load(48_000.0).is_empty(), "no library yet");
    let row = [c(1), clip(2, 2, 48_000.0), c(3)];
    lib.save(&row).unwrap();
    let back = lib.0.load(48_000.0);
    assert_eq!(back.iter().map(|c| &c.id).collect::<Vec<_>>(), row.iter().map(|c| &c.id).collect::<Vec<_>>(), "same audio, same order");
    assert_eq!(back.iter().map(|c| &c.title).collect::<Vec<_>>(), row.iter().map(|c| &c.title).collect::<Vec<_>>());
    assert_eq!(back.iter().map(|c| c.bars()).collect::<Vec<_>>(), [1, 2, 1]);
    assert_eq!(back.iter().map(|c| c.source.as_str()).collect::<Vec<_>>(), ["A", "B", "A"]);
    assert_eq!(back.iter().map(|c| c.start_beat).collect::<Vec<_>>(), [4.0, 8.0, 12.0]);
    assert!(back.iter().all(|c| c.bpm == 120.0));
    // sample for sample: a float WAV loses nothing
    assert_eq!(back[1].buffer.left, row[1].buffer.left);
    assert_eq!(back[1].buffer.right(), row[1].buffer.right());
}

// LoopLibraryTests.takingAClipOffTheRowMovesItsAudioToRemoved
#[test]
fn taking_a_clip_off_the_row_moves_its_audio_to_removed() {
    let lib = Lib::new();
    let (a, b) = (c(1), c(2));
    lib.save(&[a.clone(), b.clone()]).unwrap();
    lib.save(std::slice::from_ref(&b)).unwrap();
    let mut want = vec![format!("{}.wav", b.id), "library.json".into(), "removed".into()];
    want.sort();
    assert_eq!(lib.files(), want);
    let mut want = vec![format!("{}.wav", a.id), "removed.json".into()];
    want.sort();
    assert_eq!(lib.removed_files(), want);
    assert_eq!(lib.ids(48_000.0), [b.id]);
}

// LoopLibraryTests.anotherSampleRateResamplesAndKeepsTheBars
#[test]
fn another_sample_rate_resamples_and_keeps_the_bars() {
    let lib = Lib::new();
    lib.save(&[clip(1, 2, 48_000.0)]).unwrap();
    let back = lib.0.load(44_100.0).remove(0);
    assert!(back.bars() == 2 && back.bpm == 120.0);
    assert!((back.buffer.length() as i64 - 2 * 88_200).abs() < 64, "two bars at 44.1 kHz: {}", back.buffer.length());
}

// LoopLibraryTests.anotherSampleRateKeepsTheIdsSoScenesAndFilesStay
#[test]
fn another_sample_rate_keeps_the_ids_so_scenes_and_files_stay() {
    let lib = Lib::new();
    let row = [c(1), clip(2, 2, 48_000.0), c(3)];
    let scenes = vec![LoopScene { name: "Scene 1".into(), clips: row[..2].iter().map(|c| scene(&c.id)).collect() }];
    lib.0.save(&row, Some(&scenes), None, 0).unwrap();
    lib.save(&row[..2]).unwrap(); // clip 3 taken off the row
    // relaunched at 44.1 kHz: the audio is resampled, the ids are the ones saved
    let back = lib.0.load(44_100.0);
    assert_eq!(back.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), [row[0].id.clone(), row[1].id.clone()]);
    assert!(back.iter().all(|c| c.buffer.sample_rate == 44_100.0));
    assert_eq!(lib.0.load_scenes(), scenes);
    let files = lib.files();
    lib.save(&back).unwrap();
    assert_eq!(lib.files(), files, "no new WAVs, none moved away");
    assert_eq!(lib.0.removed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [row[2].id.clone()], "only the loop really removed");
    let gone = lib.0.removed_clip(&row[2].id, 44_100.0).unwrap();
    assert_eq!(gone.id, row[2].id, "a removed one decodes under its own id too");
}

fn scene(id: &str) -> SceneClip {
    SceneClip { clip_id: id.to_string(), gain: 1.0, fx: None }
}

// LoopLibraryTests.aResampledLibraryClipLoopsAndLaunchesInItsScene
#[test]
fn a_resampled_library_clip_loops_and_launches_in_its_scene() {
    let lib = Lib::new();
    let row = [c(1), c(2)];
    lib.0.save(&row, Some(&[LoopScene { name: "S".into(), clips: vec![scene(&row[1].id)] }]), None, 0).unwrap();
    let mut e = dj(44_100.0);
    e.restore_clips(&lib.0.load(44_100.0));
    e.restore_scenes(&lib.0.load_scenes());
    e.launch_scene("S");
    assert_eq!(e.looping_clips(), [row[1].id.clone()], "the scene finds its clip");
    let clip = e.clip(&row[1].id).unwrap();
    assert!(e.loops.row.sample_library.contains_key(&clip.buffer.content_id()), "its resampled audio is named too");
}

// LoopLibraryTests.aFailedMoveLeavesTheRemovedLoopListed (a folder in the way of the move)
#[test]
fn a_failed_move_leaves_the_removed_loop_listed() {
    let lib = Lib::new();
    let (a, b) = (c(1), c(2));
    lib.save(&[a.clone(), b.clone()]).unwrap();
    // `removed/<id>.wav` is a folder that cannot be cleared (it holds a file)
    let inner = lib.0.removed_audio_file(&b.id).join("locked");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("f"), "x").unwrap();
    assert!(lib.save(std::slice::from_ref(&a)).is_err());
    // its entry reached removed.json before anything moved, and the index still has it: it is
    // listed on the row (its audio is still there), and the next save finishes the job
    assert_eq!(lib.0.removed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [b.id.clone()]);
    assert_eq!(lib.ids(48_000.0), [a.id.clone(), b.id.clone()]);
    std::fs::remove_dir_all(lib.0.removed_audio_file(&b.id)).unwrap();
    lib.save(std::slice::from_ref(&a)).unwrap();
    assert!(lib.0.removed().iter().map(|r| r.id.clone()).eq([b.id.clone()]) && lib.0.removed_clip(&b.id, 48_000.0).is_some());
    assert_eq!(lib.ids(48_000.0), [a.id]);
}

// LoopLibraryTests.restoredClipsJoinTheRowAndReorder
#[test]
fn restored_clips_join_the_row_and_reorder() {
    let mut e = dj(48_000.0);
    let row = [c(1), c(2), c(3)];
    e.restore_clips(&row);
    e.restore_clips(&row[..1]); // already there: not twice
    assert_eq!(ids(&e), row.iter().map(|c| c.id.clone()).collect::<Vec<_>>());
    assert!(e.loops.row.sample_library.contains_key(&row[1].id), "a session can name them");
    e.move_clip(&row[2].id, Some(&row[0].id));
    assert_eq!(ids(&e), [row[2].id.clone(), row[0].id.clone(), row[1].id.clone()]);
    e.move_clip(&row[2].id, None);
    assert_eq!(ids(&e), row.iter().map(|c| c.id.clone()).collect::<Vec<_>>());
    e.set_looping(&row[1].id, true);
    assert_eq!(e.loop_state(&row[1].id), LoopState::Starting, "a restored clip loops like a fresh one");
}

// LoopLibraryTests.aSequenceIsItsStepsBackToBack
#[test]
fn a_sequence_is_its_steps_back_to_back() {
    let mut e = dj(48_000.0);
    let (a, b) = (c(1), clip(2, 2, 48_000.0));
    e.restore_clips(&[a.clone(), b.clone()]);
    let id = e.make_sequence(&[SequenceStep::new(&a.id, 2), SequenceStep::new(&b.id, 1)], None).unwrap();
    let seq = e.clip(&id).unwrap();
    assert_eq!(ids(&e), [a.id.clone(), b.id.clone(), id.clone()], "on the end of the row");
    assert!(seq.beats == 4 * 2 + 8 && seq.bpm == 120.0 && seq.source == a.source);
    assert_eq!(seq.steps, Some(vec![SequenceStep::new(&a.id, 2), SequenceStep::new(&b.id, 1)]));
    let mut want = a.buffer.left.clone();
    want.extend_from_slice(&a.buffer.left);
    want.extend_from_slice(&b.buffer.left);
    assert_eq!(seq.buffer.left, want, "exactly, sample for sample");
    assert_eq!(seq.title, "seq 1\u{d7}2\u{2192}2");
    assert_eq!(id, sequence_id(&seq.buffer));
}

// LoopLibraryTests.anotherTempoIsStretchedToTheFirstClips
#[test]
fn another_tempo_is_stretched_to_the_first_clips() {
    let mut e = dj(48_000.0);
    let a = c(1);
    let fast = c(2); // same audio length, but say it is at 240 bpm: one bar takes half as long
    let quick = LoopClip::from_bars(&fast.id, &fast.title, &fast.source, 1, 240.0, 0.0, fast.buffer.clone(), fast.waveform.clone(), None);
    e.restore_clips(&[a.clone(), quick.clone()]);
    let id = e.make_sequence(&[SequenceStep::new(&a.id, 1), SequenceStep::new(&quick.id, 1)], None).unwrap();
    let seq = e.clip(&id).unwrap();
    assert!(seq.bpm == 120.0 && seq.beats == 8);
    // a's bar, then the 240 bpm bar played at 120: twice as long as it was
    assert_eq!(seq.buffer.length(), a.buffer.length() + 2 * quick.buffer.length());
}

// LoopLibraryTests.editingASequenceKeepsItsPlaceAndItsLoop
#[test]
fn editing_a_sequence_keeps_its_place_and_its_loop() {
    let mut e = dj(48_000.0);
    let (a, b, cc) = (c(1), c(2), c(3));
    e.restore_clips(&[a.clone(), b.clone(), cc.clone()]);
    let id = e.make_sequence(&[SequenceStep::new(&a.id, 1), SequenceStep::new(&b.id, 1)], None).unwrap();
    e.move_clip(&id, Some(&cc.id));
    e.set_clip_gain(&id, 0.3, None);
    e.set_looping(&id, true);
    assert!(e.make_sequence(&[SequenceStep::new("gone", 1)], Some(&id)).is_none(), "nothing left");
    use djrust::session::action::DeckAction;
    e.apply_loop_action(
        &DeckAction::ClipSequence {
            steps: vec![SequenceStep::new(&b.id, 4), SequenceStep::new("gone", 1)],
            replacing: Some(id.clone()),
        },
        None,
    );
    let new = e.loops.row.clips[2].clone();
    assert_eq!(ids(&e), [a.id, b.id.clone(), new.id.clone(), cc.id], "where it was");
    assert_eq!(new.steps, Some(vec![SequenceStep::new(&b.id, 4)]), "a missing clip is left out");
    assert!(new.beats == 16 && e.clip_gain(&new.id) == 0.3);
    assert_eq!(e.loop_state(&new.id), LoopState::Starting, "it keeps looping");
    assert_ne!(e.loop_state(&id), LoopState::On);
}

// LoopLibraryTests.theLibraryKeepsASequencesSteps
#[test]
fn the_library_keeps_a_sequences_steps() {
    let lib = Lib::new();
    let plain = clip(5, 2, 48_000.0);
    let mut seq = LoopClip::new(&sequence_id(&plain.buffer), "seq x\u{d7}2\u{2192}y", "A", 8, 120.0, 0.0, plain.buffer.clone(), plain.waveform.clone());
    seq.steps = Some(vec![SequenceStep::new("x", 2), SequenceStep::new("y", 1)]);
    lib.save(&[c(1), seq.clone()]).unwrap();
    let back = lib.0.load(48_000.0);
    assert!(back[0].steps.is_none() && back[1].steps == seq.steps);
    assert_eq!(back[1].id, seq.id, "still a sequence's id after a reload");
    // provenance is kept as it was written
    let p = JsonValue::Object(JsonObject::from_pairs([("deck", JsonValue::str("B")), ("bar", JsonValue::int(9))]));
    let mut with_p = LoopClip::from_bars(&plain.id, &plain.title, "B", 2, 120.0, 32.0, plain.buffer.clone(), plain.waveform.clone(), Some(p.clone()));
    with_p.beats = 8;
    lib.save(&[with_p]).unwrap();
    assert_eq!(lib.0.load(48_000.0)[0].provenance.as_ref().map(|v| v.canonical_js()), Some(p.canonical_js()));
}

fn take(n: usize, provenance: Option<JsonValue>) -> LoopClip {
    let base = c(n);
    let p = provenance.unwrap_or_else(|| {
        JsonValue::Object(JsonObject::from_pairs([
            ("deck", JsonValue::str("A")),
            ("bar", JsonValue::int(n as i64)),
            ("title", JsonValue::str("t")),
        ]))
    });
    LoopClip::from_bars(&base.id, &base.title, "A", 1, 120.0, (4 * n) as f64, base.buffer.clone(), base.waveform.clone(), Some(p))
}

/// LOOP-record provenance of a net deck playing techno seed 7, in section 2.
fn net_provenance() -> JsonValue {
    fn f<const N: usize>(p: [(&str, JsonValue); N]) -> JsonObject {
        JsonObject::from_pairs(p)
    }
    let o = JsonValue::Object;
    let s = JsonValue::str;
    o(f([
        ("deck", s("B")),
        ("title", s("techno 7")),
        ("mode", s("net")),
        ("source", o(f([("kind", s("net")), ("share", o(f([("genre", s("techno")), ("seed", JsonValue::int(7))])))]))),
        ("section", o(f([("index", JsonValue::int(2)), ("name", s("drop"))]))),
    ]))
}

// LoopLibraryRemovalTests.removalKeepsTheAudioUnderRemovedWithItsPlace
#[test]
fn removal_keeps_the_audio_under_removed_with_its_place() {
    let lib = Lib::new();
    let (a, b, cc) = (take(1, None), take(2, None), take(3, None));
    lib.save(&[a.clone(), b.clone(), cc.clone()]).unwrap();
    lib.save(&[a.clone(), cc.clone()]).unwrap();
    assert!(lib.removed_files().contains(&format!("{}.wav", b.id)), "the audio is kept");
    let gone = lib.0.removed();
    assert!(gone.len() == 1 && gone[0].id == b.id && gone[0].index == 1 && gone[0].title == b.title);
    assert_eq!(gone[0].removed, "2023-11-14T22:13:20Z", "with when");
    lib.save(std::slice::from_ref(&cc)).unwrap();
    assert_eq!(lib.0.removed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [a.id.clone(), b.id.clone()], "newest first");
    assert_eq!(lib.ids(48_000.0), [cc.id]);
}

// LoopLibraryRemovalTests.restoringBringsItBackWithItsProvenanceAndPlace
#[test]
fn restoring_brings_it_back_with_its_provenance_and_place() {
    let lib = Lib::new();
    let (a, b, cc) = (take(1, None), take(2, Some(net_provenance())), take(3, None));
    let mut e = dj(48_000.0);
    e.restore_clips(&[a.clone(), b.clone(), cc.clone()]);
    e.set_clip_color(&b.id, Some("teal"));
    lib.save(&e.loops.row.clips).unwrap(); // the app saves the row whenever it changes
    e.remove_clip(&b.id);
    assert_eq!(ids(&e), [a.id.clone(), cc.id.clone()]);
    assert_eq!(e.loops.row.removed.first().map(|r| r.index), Some(1));
    lib.save(&e.loops.row.clips).unwrap();
    // the next launch: the row from the library, the removed clip from removed/
    let mut e2 = dj(48_000.0);
    e2.restore_clips(&lib.0.load(48_000.0));
    let entry = lib.0.removed().remove(0);
    let back = lib.0.removed_clip(&entry.id, 48_000.0).unwrap();
    e2.remember_removed_clip(back, entry.index);
    e2.restore_clip(&b.id, None);
    assert_eq!(ids(&e2), [a.id.clone(), b.id.clone(), cc.id.clone()], "where it was");
    let restored = e2.clip(&b.id).unwrap();
    assert_eq!(restored.provenance.as_ref().map(|v| v.canonical_js()), Some(net_provenance().canonical_js()));
    assert!(restored.color.as_deref() == Some("teal") && restored.title == b.title);
    assert!(e2.loops.row.removed.is_empty());
    lib.save(&e2.loops.row.clips).unwrap();
    assert!(lib.0.removed().is_empty() && !lib.removed_files().contains(&format!("{}.wav", b.id)));
    assert!(lib.files().contains(&format!("{}.wav", b.id)), "moved back");
    assert_eq!(lib.ids(48_000.0), [a.id, b.id, cc.id]);
}

// LoopLibraryRemovalTests.deleteForeverDeletesTheAudio
#[test]
fn delete_forever_deletes_the_audio() {
    let lib = Lib::new();
    let (a, b) = (take(1, None), take(2, None));
    lib.save(&[a.clone(), b.clone()]).unwrap();
    lib.save(std::slice::from_ref(&a)).unwrap();
    lib.0.delete_forever(&b.id).unwrap();
    assert!(lib.0.removed().is_empty() && lib.removed_files() == ["removed.json"]);
    assert!(lib.0.removed_clip(&b.id, 48_000.0).is_none());
    let mut e = dj(48_000.0);
    e.restore_clips(&[a.clone(), b.clone()]);
    e.remove_clip(&b.id);
    e.forget_removed_clip(&b.id);
    assert!(!e.restore_clip(&b.id, None) && ids(&e) == [a.id], "no longer restorable");
}

// LoopLibraryTests.scenesSurviveTheLibrary (LoopLaunchTests)
#[test]
fn scenes_survive_the_library() {
    let lib = Lib::new();
    let row = [c(1), c(2)];
    let scenes = vec![
        LoopScene { name: "Scene 1".into(), clips: vec![SceneClip { clip_id: row[0].id.clone(), gain: 0.25, fx: None }] },
        LoopScene { name: "drop".into(), clips: row.iter().map(|c| scene(&c.id)).collect() },
    ];
    lib.0.save(&row, Some(&scenes), None, 0).unwrap();
    assert_eq!(lib.0.load_scenes(), scenes);
    assert_eq!(lib.ids(48_000.0), row.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), "the loops as before");
    lib.save(&row[1..]).unwrap();
    assert_eq!(lib.0.load_scenes(), scenes, "saving the row alone keeps the scenes");
    lib.0.save(&row, Some(&[]), None, 0).unwrap();
    assert!(lib.0.load_scenes().is_empty());
    assert!(Lib::new().0.load_scenes().is_empty(), "no library, no scenes");
}

// LoopLaunchTests.clipLaunchSurvivesTheLibraryAndIsLeftOutWithout and
// aClipsFollowSurvivesTheLibraryAndIsLeftOutWithout
#[test]
fn a_clips_launch_settings_survive_the_library_and_are_left_out_without() {
    let lib = Lib::new();
    let row = [c(1), c(2)];
    lib.save(&row).unwrap();
    let plain = std::fs::read_to_string(lib.0.index_file()).unwrap();
    assert!(!plain.contains("quantum") && !plain.contains("\"fade\"") && !plain.contains("\"follow\""), "nothing when unset");
    let launch = BTreeMap::from([(
        row[0].id.clone(),
        ClipLaunch { quantum: Some(8), fade: Some(LoopFade::Beat), follow: None },
    ), (
        row[1].id.clone(),
        ClipLaunch { quantum: None, fade: Some(LoopFade::Bar), follow: Some(LoopFollow::Source) },
    )]);
    lib.0.save(&row, None, Some(&launch), 0).unwrap();
    assert_eq!(lib.0.load_launch(), launch);
    lib.save(&row).unwrap();
    assert_eq!(lib.0.load_launch(), launch, "saving the row alone keeps them");
    lib.0.save(&row, None, Some(&BTreeMap::new()), 0).unwrap();
    assert!(lib.0.load_launch().is_empty());
    assert_eq!(std::fs::read_to_string(lib.0.index_file()).unwrap(), plain, "byte for byte");
}

// LoopLibraryRemovalTests.namesAndColoursAreKeptAndRecorded
#[test]
fn names_and_colours_are_kept() {
    let lib = Lib::new();
    let mut e = dj(48_000.0);
    let (a, b) = (take(1, None), take(2, None));
    e.restore_clips(&[a.clone(), b.clone()]);
    e.rename_clip(&a.id, "  big drop  ");
    e.rename_clip(&b.id, "   ");
    e.set_clip_color(&a.id, Some("orange"));
    e.set_clip_color(&b.id, Some("chartreuse"));
    let titles: Vec<String> = e.loops.row.clips.iter().map(|c| c.title.clone()).collect();
    assert_eq!(titles, ["big drop", &b.title], "blank names are ignored");
    let colours: Vec<Option<String>> = e.loops.row.clips.iter().map(|c| c.color.clone()).collect();
    assert_eq!(colours, [Some("orange".to_string()), None], "only palette colours");
    e.crop_clip(&a.id, 0, 2);
    assert!(e.loops.row.clips[0].title == "big drop" && e.loops.row.clips[0].color.as_deref() == Some("orange"), "a crop keeps both");
    lib.save(&e.loops.row.clips).unwrap();
    let back = lib.0.load(48_000.0);
    assert_eq!(back.iter().map(|c| c.title.clone()).collect::<Vec<_>>(), ["big drop", &b.title]);
    assert_eq!(back.iter().map(|c| c.color.clone()).collect::<Vec<_>>(), [Some("orange".to_string()), None]);
}

// LoopLibraryRemovalTests.aClipKnowsTheTrackItCameFrom
#[test]
fn a_clip_knows_the_track_it_came_from() {
    use djrust::deck::beat_grid::BeatGrid;
    use djrust::session::model::DeckSource;
    let mut e = dj(48_000.0);
    let net = take(1, Some(net_provenance()));
    let other = take(9, None);
    let sample_prov = JsonValue::Object(JsonObject::from_pairs([
        ("deck", JsonValue::str("A")),
        ("source", DeckSource::Sample { id: other.id.clone(), title: "kick.wav".into(), grid: Some(BeatGrid::new(120.0, 0.0, 1.0)) }.json()),
    ]));
    let sampled = take(2, Some(sample_prov));
    e.restore_clips(&[net.clone(), sampled.clone()]);
    let o = e.clip_origin(&net.id).unwrap();
    let DeckSource::Net(spec) = &o.source else { panic!("a net: {:?}", o.source) };
    assert!(spec.genre == "techno" && spec.seed == 7.0 && o.section == Some(2));
    assert!(net.origin_genre().as_deref() == Some("techno") && net.origin_title().as_deref() == Some("techno 7"));
    assert!(e.clip_origin(&sampled.id).is_none(), "its audio is not in the library");
    e.loops.row.sample_library.insert(other.id.clone(), other.buffer.clone());
    assert!(e.clip_origin(&sampled.id).unwrap().buffer.is_some());
    // a crop (and a crop of a crop) still knows
    let crop = e.crop_clip(&net.id, 0, 2).unwrap();
    let crop2 = e.crop_clip(&crop, 1, 2).unwrap();
    assert_eq!(e.clip_origin(&crop2).unwrap().section, Some(2));
    let seq = e.make_sequence(&[SequenceStep::new(&crop2, 1)], None).unwrap();
    assert_eq!(e.clip(&seq).unwrap().origin_genre().as_deref(), Some("techno"), "and a sequence, by its first step");
}

// LoopLibraryRemovalTests.undoingARemovalPutsItBackAsItWas / KeepsItsMuteSoloAndKeyLock
#[test]
fn undoing_a_removal_puts_it_back_as_it_was() {
    let mut e = dj(48_000.0);
    let (a, b, cc) = (take(1, None), take(2, None), take(3, None));
    e.restore_clips(&[a.clone(), b.clone(), cc.clone()]);
    e.set_clip_gain(&a.id, 0.4, None);
    e.remove_clip(&a.id);
    e.remove_clip(&cc.id);
    assert_eq!(e.loops.row.removed.iter().map(|r| r.clip.id.clone()).collect::<Vec<_>>(), [cc.id.clone(), a.id.clone()], "newest first");
    e.restore_clip(&a.id, None);
    assert!(ids(&e) == [a.id.clone(), b.id.clone()] && e.clip_gain(&a.id) == 0.4);
    e.restore_clip(&cc.id, Some(0));
    assert_eq!(ids(&e), [cc.id.clone(), a.id.clone(), b.id.clone()], "or where it is asked to go");
    e.restore_clip(&cc.id, None);
    assert_eq!(e.loops.row.clips.len(), 3, "not twice");

    // mute, solo and key lock go with it
    let mut e = dj(48_000.0);
    e.restore_clips(&[a.clone(), b.clone(), cc.clone()]);
    e.set_clip_mute(&a.id, true);
    e.set_clip_key_lock(&a.id, true);
    e.set_clip_solo(&b.id, true);
    e.set_clip_key_lock(&cc.id, false);
    for x in [&a, &b, &cc] {
        e.remove_clip(&x.id);
    }
    assert!(e.loops.row.settings.solos.is_empty() && e.loops.row.settings.mutes.is_empty(), "off the row, they are not heard");
    for x in [&a, &b, &cc] {
        e.restore_clip(&x.id, None);
    }
    assert!(e.is_clip_muted(&a.id) && e.is_key_locked(&a.id) && !e.is_clip_soloed(&a.id));
    assert!(e.is_clip_soloed(&b.id) && !e.is_clip_muted(&b.id));
    assert_eq!(e.loops.row.settings.key_locks.get(&cc.id), Some(&false), "its own setting, not the row's");
    assert_eq!(e.audible_clip_gain(&cc.id), 0.0, "b is soloed again");
    let _ = ClipSound::default();
}

// LoopLibraryRemovalTests.aClipIsAWAVFile
#[test]
fn a_clip_is_a_wav_file() {
    let mut cl = take(4, None);
    let data = cl.wav_data();
    assert!(data.len() > 96_000 * 8 && &data[..4] == b"RIFF");
    cl.title = "drop: A/B?".into();
    assert_eq!(cl.file_name(), "drop- A-B-.wav");
}

// the library's WAV reader takes both of `WAV.encode`'s formats, and nothing else
#[test]
fn the_wav_reader_takes_float_and_pcm16_and_nothing_else() {
    use djrust::host::decode::read_wav;
    let l: Vec<f32> = (0..64).map(|i| (i as f32 / 64.0) - 0.5).collect();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let f = djrust::wav::wav_bytes(&l, &r, 44_100.0, true);
    let (b, rate) = read_wav(&f).unwrap();
    assert_eq!((rate, &b.left, b.right()), (44_100.0, &l, &r[..]));
    let p = djrust::wav::wav_bytes(&l, &r, 22_050.0, false);
    let (b, rate) = read_wav(&p).unwrap();
    assert_eq!(rate, 22_050.0);
    assert!(b.left.iter().zip(&l).all(|(a, b)| (a - b).abs() < 1.0 / 32767.0));
    assert!(read_wav(b"RIFF....WAVEjunk").is_none() && read_wav(&f[..30]).is_none());
}
