//! The take library: the loop row saved as it changes and put back at launch
//! (`AppModel.restoreLoops` / `pollLoops`, `LoopLibraryModel`'s removed loops), over a temporary
//! folder and a real engine. Each test names the Swift app test it ports (`LoopRowViewTests`,
//! `LoopLibraryViewTests`, `LaunchBarTests`); a new engine over the same folder stands for the
//! next launch. Nothing here touches the real support folder.

mod common_row;

use std::path::{Path, PathBuf};

use common_row::{Rig, SR};
use djrust::journal::event::JournalEvent;
use djrust::looplib::LoopMeta;
use djrust::session::action::DeckAction;
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::host::deck::{engine, Engine, EngineConfig, PerformCtx};
use djrust::host::journal::{read, Journal};
use djrust::host::stores::captures::CaptureLibrary;
use djrust::host::stores::loops::LoopLibrary;
use djrust::host::stores::temp_path;
use djrust::host::take_library::{default_directory, keeps, row_signature, TakeLibrary};

/// A fresh engine for the "next launch": no decks loaded, the same rate.
fn launch() -> Engine {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    engine(c).0
}

fn ids(e: &Engine) -> Vec<String> {
    e.loops.row.clips.iter().map(|c| c.id.clone()).collect()
}

fn perform(e: &mut Engine, a: DeckAction) {
    e.perform(&a, PerformCtx::default());
}

/// A temporary folder, removed on drop (the library is created by the first save).
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        Dir(temp_path(tag))
    }
    fn lib(&self) -> LoopLibrary {
        LoopLibrary::new(&self.0)
    }
    fn removed_files(&self) -> Vec<String> {
        names(&self.0.join("removed"))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// `LoopLibraryViewTests.model`: the app opens over `dir` (an empty library), deck A plays a
/// net track at 120 bpm and LOOP record takes two different bars of it.
fn app(dir: &Dir) -> (Rig, TakeLibrary, Vec<String>) {
    let mut rig = Rig::new();
    let takes = TakeLibrary::open(&dir.0, &mut rig.e);
    assert_eq!(takes.restored(), 0, "an empty folder is an empty library");
    let (one, two) = rig.two_takes();
    (rig, takes, vec![one, two])
}

// LoopRowViewTests.theRowIsSavedAndComesBackOnTheNextLaunch
#[test]
fn the_row_is_saved_and_comes_back_on_the_next_launch() {
    let dir = Dir::new("take-row");
    let (rig, mut takes, clips) = app(&dir);
    assert!(takes.poll(&rig.e), "the row changed: a save is queued");
    takes.flush();
    let saved = dir.lib().load(SR);
    assert_eq!(saved.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), clips);
    // a second app over a fresh engine finds it there
    let mut e2 = launch();
    let again = TakeLibrary::open(&dir.0, &mut e2);
    assert_eq!(again.restored(), 2);
    assert_eq!(ids(&e2), clips, "the same clips, in the same order");
    assert!(
        e2.looping_clips().is_empty() && clips.iter().all(|id| e2.loop_state(id) == djrust::host::deck::loops::LoopState::Off),
        "back on the row, not looping"
    );
    assert!(
        e2.loops.row.clips.iter().zip(&saved).all(|(a, b)| a.buffer.left == b.buffer.left),
        "the audio, sample for sample"
    );
    // restoring is not a change: the first poll saves nothing
    let mut again = again;
    assert!(!again.poll(&e2) && again.saves() == 0);
}

// LoopLibraryViewTests.namesAndColoursAreSavedAndRecorded (+ LoopFxTests.theLibraryKeepsAClipsFx
// through the app's signature)
#[test]
fn names_colours_and_fx_are_saved_and_come_back() {
    let dir = Dir::new("take-names");
    let (mut rig, mut takes, clips) = app(&dir);
    takes.poll(&rig.e);
    takes.flush();
    let id = clips[0].clone();
    perform(&mut rig.e, DeckAction::ClipRename { id: id.clone(), title: "big drop".into() });
    perform(&mut rig.e, DeckAction::ClipColor { id: id.clone(), color: Some("teal".into()) });
    let crunch = LoopFx { drive: 9.0, mode: LoopFxMode::Hard, amount: 0.55, tone: 0.75, ..LoopFx::default() };
    perform(&mut rig.e, DeckAction::ClipFx { id: id.clone(), fx: crunch.clone() });
    let c = rig.e.clip(&id).unwrap();
    assert!(c.title == "big drop" && c.color.as_deref() == Some("teal") && c.fx == crunch);
    assert!(takes.poll(&rig.e));
    takes.flush();
    let saved = dir.lib().load(SR);
    let saved = saved.iter().find(|c| c.id == id).expect("saved");
    assert!(saved.title == "big drop" && saved.color.as_deref() == Some("teal"));
    assert_eq!(saved.fx, crunch, "the FX is kept with the clip");
    // the next launch
    let mut e2 = launch();
    TakeLibrary::open(&dir.0, &mut e2);
    let back = e2.clip(&id).unwrap();
    assert!(back.title == "big drop" && back.color.as_deref() == Some("teal") && back.fx == crunch);
    assert_eq!(ids(&e2), clips, "and the order");
}

// AppModel.rowSignature: id|title|colour|fx, so those and the scenes are what a poll saves
#[test]
fn only_what_the_signature_covers_is_saved() {
    let dir = Dir::new("take-signature");
    let (mut rig, mut takes, clips) = app(&dir);
    assert!(takes.poll(&rig.e));
    assert_eq!(takes.saves(), 1);
    assert!(!takes.poll(&rig.e), "nothing changed: nothing is saved again");
    // a level, a mute and the launch boundary are not in the library
    rig.e.set_clip_gain(&clips[0], 0.4, None);
    rig.e.set_clip_mute(&clips[1], true);
    rig.e.set_loop_quantum(8);
    assert!(!takes.poll(&rig.e) && takes.saves() == 1);
    // each of these is
    let mut saves = 1;
    let mut changed = |rig: &mut Rig, takes: &mut TakeLibrary, what: &str, f: &dyn Fn(&mut Engine)| {
        f(&mut rig.e);
        assert!(takes.poll(&rig.e), "{what} is a change");
        saves += 1;
        assert_eq!(takes.saves(), saves);
        assert!(!takes.poll(&rig.e), "and only once");
    };
    let id = clips[0].clone();
    changed(&mut rig, &mut takes, "a rename", &|e| e.rename_clip(&id, "renamed"));
    changed(&mut rig, &mut takes, "a colour", &|e| e.set_clip_color(&id, Some("pink")));
    changed(&mut rig, &mut takes, "an FX", &|e| {
        e.set_clip_fx(&id, &LoopFx { drive: 5.0, ..LoopFx::default() }, None)
    });
    changed(&mut rig, &mut takes, "a move", &|e| e.move_clip(&id, None));
    changed(&mut rig, &mut takes, "a scene", &|e| {
        e.save_scene("Scene 1");
    });
    changed(&mut rig, &mut takes, "a scene's name", &|e| {
        perform(e, DeckAction::SceneRename { name: "Scene 1".into(), to: "drop".into() })
    });
    changed(&mut rig, &mut takes, "a removal", &|e| e.remove_clip(&clips[1]));
    takes.flush();
    assert!(takes.error().is_none());
}

// LaunchBarTests.scenesAreChipsSavedWithTheLibrary (the library half)
#[test]
fn scenes_are_saved_with_the_library() {
    let dir = Dir::new("take-scenes");
    let (mut rig, mut takes, clips) = app(&dir);
    rig.settle();
    // the takes are saved before any scene exists, so what follows is the scenes' own change
    assert!(takes.poll(&rig.e));
    takes.flush();
    let id = clips[0].clone();
    rig.e.set_looping(&clips[1], false);
    rig.settle();
    let name = rig.e.next_scene_name();
    perform(&mut rig.e, DeckAction::SceneSave { name });
    assert_eq!(rig.e.scenes().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Scene 1"]);
    perform(&mut rig.e, DeckAction::SceneRename { name: "Scene 1".into(), to: "drop".into() });
    let name = rig.e.next_scene_name();
    perform(&mut rig.e, DeckAction::SceneSave { name });
    assert_eq!(rig.e.scenes().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["drop", "Scene 2"]);
    perform(&mut rig.e, DeckAction::SceneDelete { name: "Scene 2".into() });
    takes.poll(&rig.e);
    takes.flush();
    let scenes = dir.lib().load_scenes();
    assert_eq!(scenes.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["drop"]);
    assert_eq!(scenes[0].clips.iter().map(|c| c.clip_id.clone()).collect::<Vec<_>>(), [id]);
    // the next launch finds it there
    let mut e2 = launch();
    TakeLibrary::open(&dir.0, &mut e2);
    assert_eq!(e2.scenes().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["drop"]);
    assert_eq!(e2.scenes(), scenes.as_slice());
}

// LoopLibraryViewTests.theLibraryKeepsRemovedLoopsAcrossLaunches
#[test]
fn the_library_keeps_removed_loops_across_launches() {
    let dir = Dir::new("take-removed");
    let (mut rig, mut takes, ids_) = app(&dir);
    let original_provenance = rig.e.clip(&ids_[0]).unwrap().provenance.clone();
    assert!(original_provenance.is_some(), "a take knows where it came from");
    takes.poll(&rig.e);
    takes.flush();
    perform(&mut rig.e, DeckAction::ClipRemove { id: ids_[0].clone() });
    takes.poll(&rig.e);
    takes.flush();
    assert!(dir.removed_files().contains(&format!("{}.wav", ids_[0])), "the audio is kept");
    assert_eq!(
        dir.lib().load(SR).iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        [ids_[1].clone()]
    );
    // this run lists it under Removed, from the engine's own list
    assert_eq!(takes.removed(&rig.e).iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [ids_[0].clone()]);

    // the next launch lists it, and restores it from removed/ where it was
    let mut e2 = launch();
    let mut takes2 = TakeLibrary::open(&dir.0, &mut e2);
    let list = takes2.removed(&e2);
    assert_eq!(list.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), [ids_[0].clone()]);
    assert_eq!(ids(&e2), [ids_[1].clone()]);
    assert_eq!(list[0].index, 0, "the place it had");
    assert!(list[0].beats == 4 && !list[0].is_sequence);
    assert!(takes2.restore_removed(&mut e2, &ids_[0]).is_ok());
    assert_eq!(ids(&e2), ids_, "back where it was");
    assert_eq!(
        e2.clip(&ids_[0]).unwrap().provenance.as_ref().map(|p| p.canonical_js()),
        original_provenance.as_ref().map(|p| p.canonical_js()),
        "with its provenance"
    );
    takes2.flush();
    assert!(dir.lib().removed().is_empty());
    assert_eq!(dir.lib().load(SR).iter().map(|c| c.id.clone()).collect::<Vec<_>>(), ids_);
    assert!(takes2.removed(&e2).is_empty());

    // and delete forever deletes
    perform(&mut e2, DeckAction::ClipRemove { id: ids_[1].clone() });
    takes2.poll(&e2);
    takes2.flush();
    assert!(dir.removed_files().contains(&format!("{}.wav", ids_[1])));
    takes2.delete_forever(&mut e2, &ids_[1]);
    takes2.flush();
    assert!(!dir.removed_files().contains(&format!("{}.wav", ids_[1])));
    assert!(takes2.removed(&e2).is_empty());
    assert!(takes2.restore_removed(&mut e2, &ids_[1]).is_err(), "it is gone");
    assert!(takes2.error().is_none());
}

// LoopLibraryViewTests.deleteForeverIsNotUndoneByASaveAlreadyUnderWay
#[test]
fn delete_forever_is_not_undone_by_a_save_already_under_way() {
    let dir = Dir::new("take-epoch");
    let (mut rig, mut takes, ids_) = app(&dir);
    takes.poll(&rig.e);
    takes.flush();
    // the removal's save is still queued (it reads removed/ after writing) when the delete comes
    let hold = takes.hold();
    perform(&mut rig.e, DeckAction::ClipRemove { id: ids_[0].clone() });
    takes.poll(&rig.e);
    let between = takes.peek(); // what the list says after that save, before the delete
    takes.delete_forever(&mut rig.e, &ids_[0]);
    hold.release();
    takes.flush();
    assert!(
        between.recv().unwrap().is_empty(),
        "the save read removed/ before the delete: its list is dropped, not shown"
    );
    assert!(
        !takes.removed(&rig.e).iter().any(|r| r.id == ids_[0]),
        "{:?}",
        takes.removed(&rig.e)
    );
    assert!(!takes.disk_removed().iter().any(|r| r.id == ids_[0]));
    assert!(dir.lib().removed().is_empty() && !dir.removed_files().contains(&format!("{}.wav", ids_[0])));
    // a later removal still shows up, from the library's list
    perform(&mut rig.e, DeckAction::ClipRemove { id: ids_[1].clone() });
    takes.poll(&rig.e);
    takes.flush();
    assert_eq!(
        takes.disk_removed().iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        [ids_[1].clone()]
    );
}

// AppModel.restoreRemoved: the library's loop is journaled as a restore from the library (so a
// replay finds it), then the restore itself
#[test]
fn a_restore_from_the_library_is_journaled() {
    let dir = Dir::new("take-journal");
    let (mut rig, mut takes, ids_) = app(&dir);
    takes.poll(&rig.e);
    perform(&mut rig.e, DeckAction::ClipRemove { id: ids_[0].clone() });
    takes.poll(&rig.e);
    takes.flush();
    let mut e2 = launch();
    let mut takes2 = TakeLibrary::open(&dir.0, &mut e2);
    let jdir = temp_path("take-journal-file");
    let j = Journal::attach(&mut e2, &jdir, 0, Default::default()).unwrap();
    assert!(takes2.restore_removed(&mut e2, &ids_[0]).is_ok());
    j.flush();
    let events: Vec<JournalEvent> = read(&j.path()).unwrap().entries.into_iter().map(|e| e.event).collect();
    let restore = events
        .iter()
        .position(|e| *e == JournalEvent::LibraryRestore { id: ids_[0].clone(), index: 0 })
        .expect("the library's restore");
    let act = events
        .iter()
        .position(|e| *e == JournalEvent::Action(DeckAction::ClipRestore { id: ids_[0].clone(), at: None }))
        .expect("the restore");
    assert!(restore < act, "the clip is known before it is restored");
    // one restored from this run's own removed list has no library step
    perform(&mut e2, DeckAction::ClipRemove { id: ids_[0].clone() });
    assert!(takes2.restore_removed(&mut e2, &ids_[0]).is_ok());
    j.flush();
    let n = read(&j.path())
        .unwrap()
        .entries
        .iter()
        .filter(|e| matches!(e.event, JournalEvent::LibraryRestore { .. }))
        .count();
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(&jdir);
}

#[test]
fn a_removed_loop_whose_audio_is_gone_says_so() {
    let dir = Dir::new("take-gone");
    let (mut rig, mut takes, ids_) = app(&dir);
    takes.poll(&rig.e);
    perform(&mut rig.e, DeckAction::ClipRemove { id: ids_[0].clone() });
    takes.poll(&rig.e);
    takes.flush();
    std::fs::remove_file(dir.0.join("removed").join(format!("{}.wav", ids_[0]))).unwrap();
    let mut e2 = launch();
    let mut takes2 = TakeLibrary::open(&dir.0, &mut e2);
    assert_eq!(takes2.removed(&e2).len(), 1, "its entry is still listed");
    assert_eq!(takes2.restore_removed(&mut e2, &ids_[0]), Err("that loop's audio is gone".into()));
    assert_eq!(ids(&e2), [ids_[1].clone()]);
    assert!(takes2.restore_removed(&mut e2, "no-such-id").is_err());
}

// The pad and input clips are the hub's: the capture library keeps those
fn saved_capture(lib: &CaptureLibrary, id: &str, kind: &str, n: f32) {
    let audio: Vec<f32> = (0..96_000).map(|i| n + (i % 50) as f32 * 1e-4).collect();
    let meta = LoopMeta {
        id: id.into(),
        title: format!("{kind} {id}"),
        bpm: 120.0,
        beats: 4,
        sample_rate: SR,
        frames: audio.len(),
        channels: 1,
        source: if kind == "input" { "TD-3".into() } else { "A".into() },
        kind: kind.into(),
        created: 1,
        row: true,
        from: String::new(),
    };
    lib.save(&meta, &audio, None).unwrap();
}

#[test]
fn input_captures_are_left_to_the_capture_library() {
    let dir = Dir::new("take-inputs");
    let caps = Dir::new("take-inputs-captures");
    let capture_library = CaptureLibrary::new(&caps.0);
    saved_capture(&capture_library, "cap-input", "input", 0.1);
    saved_capture(&capture_library, "cap-deck", "deck", 0.2);
    let mut rig = Rig::new();
    let mut takes = TakeLibrary::open(&dir.0, &mut rig.e);
    rig.e.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    assert_eq!(rig.e.library_load_row(), 2);
    assert!(rig.e.clip_extra("cap-input").from_input && !rig.e.clip_extra("cap-deck").from_input);
    assert!(!keeps(&rig.e.clip_extra("cap-input")) && keeps(&rig.e.clip_extra("cap-deck")));
    assert_eq!(row_signature(&rig.e).len(), 1, "the signature covers the clips the library keeps");
    assert!(takes.poll(&rig.e));
    takes.flush();
    let saved: Vec<String> = dir.lib().load(SR).iter().map(|c| c.id.clone()).collect();
    assert_eq!(saved, ["cap-deck"], "the input capture is not in this folder");
    assert!(!dir.0.join("cap-input.wav").exists() && !dir.0.join("removed").join("cap-input.wav").exists());
    // taking the capture off the row and putting it back (the removed menu) returns it at start
    rig.e.remove_clip("cap-input");
    let meta = |e: &Engine| e.library_entries().into_iter().find(|m| m.id == "cap-input").unwrap();
    assert!(!meta(&rig.e).row, "off the row: it does not come back at start");
    assert!(takes.restore_removed(&mut rig.e, "cap-input").is_ok());
    assert!(meta(&rig.e).row, "back on the row: it comes back at start again");
    takes.poll(&rig.e);
    takes.flush();
    assert_eq!(dir.lib().load(SR).len(), 1, "still not in this folder");
}

#[test]
fn a_deck_loop_both_libraries_have_is_restored_once_with_what_this_one_kept() {
    let dir = Dir::new("take-both");
    let caps = Dir::new("take-both-captures");
    saved_capture(&CaptureLibrary::new(&caps.0), "cap-deck", "deck", 0.2);
    // run 1: the capture library's loop is on the row, coloured
    let mut rig = Rig::new();
    let mut takes = TakeLibrary::open(&dir.0, &mut rig.e);
    rig.e.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    rig.e.library_load_row();
    rig.e.set_clip_color("cap-deck", Some("teal"));
    takes.poll(&rig.e);
    takes.flush();
    drop(takes);
    // run 2: this library first (as the app opens it), then the capture library's row
    let mut e2 = launch();
    let takes2 = TakeLibrary::open(&dir.0, &mut e2);
    e2.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    assert_eq!(takes2.restored(), 1);
    assert_eq!(e2.library_load_row(), 0, "already on the row");
    assert_eq!(ids(&e2), ["cap-deck"]);
    assert_eq!(e2.clip("cap-deck").unwrap().color.as_deref(), Some("teal"));
}

// The take library restores a loop the capture library also holds *first* (the app opens it
// before `library_load_row`), so the capture library never loads it and never marks it saved:
// a removal then left the capture library's `row` flag on, and the loop came back at the next
// launch with nothing listed under Removed.
#[test]
fn a_deck_loop_both_libraries_have_stays_removed_across_two_more_launches() {
    let dir = Dir::new("take-both-removed");
    let caps = Dir::new("take-both-removed-captures");
    saved_capture(&CaptureLibrary::new(&caps.0), "cap-deck", "deck", 0.2);
    // launch 1: the capture library puts the loop on the row; the take library keeps the row
    let mut e1 = launch();
    let mut takes1 = TakeLibrary::open(&dir.0, &mut e1);
    e1.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    assert_eq!(e1.library_load_row(), 1);
    takes1.poll(&e1);
    takes1.flush();
    drop(takes1);
    // launch 2: the take library restores it first, so the capture library finds it there. It
    // is still the capture library's loop: it is saved, and the Library menu says so
    let mut e2 = launch();
    let mut takes2 = TakeLibrary::open(&dir.0, &mut e2);
    e2.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    assert_eq!(takes2.restored(), 1);
    assert_eq!(e2.library_load_row(), 0, "already on the row");
    assert!(e2.clip_extra("cap-deck").saved, "the capture library's loop is saved");
    // the user removes it (right-click, as the app does through perform)
    perform(&mut e2, DeckAction::ClipRemove { id: "cap-deck".into() });
    takes2.poll(&e2);
    takes2.flush();
    assert!(ids(&e2).is_empty());
    assert_eq!(takes2.removed(&e2).len(), 1);
    drop(takes2);
    // launch 3: neither library puts it back, and it is listed under Removed
    let mut e3 = launch();
    let takes3 = TakeLibrary::open(&dir.0, &mut e3);
    e3.set_loop_library(Some(CaptureLibrary::new(&caps.0)));
    assert_eq!(takes3.restored(), 0);
    assert_eq!(e3.library_load_row(), 0, "the capture library no longer has it on the row");
    assert!(ids(&e3).is_empty(), "it stays off the row");
    let gone: Vec<String> = takes3.removed(&e3).iter().map(|r| r.id.clone()).collect();
    assert_eq!(gone, ["cap-deck"], "and it is listed under Removed");
}

#[test]
fn a_clip_the_hub_pads_own_is_not_kept() {
    let dir = Dir::new("take-pads");
    let mut rig = Rig::new();
    let mut takes = TakeLibrary::open(&dir.0, &mut rig.e);
    let tone = |n: f32| -> Vec<f32> { (0..96_000).map(|i| n + (i % 77) as f32 * 1e-4).collect() };
    let plain = rig.e.add_audio_clip("plain", tone(0.1), tone(0.1), 1, false);
    let shot = rig.e.add_audio_clip("shot", tone(0.3), tone(0.3), 1, true);
    assert!(keeps(&rig.e.clip_extra(&plain)) && !keeps(&rig.e.clip_extra(&shot)));
    takes.poll(&rig.e);
    takes.flush();
    let saved: Vec<String> = dir.lib().load(SR).iter().map(|c| c.id.clone()).collect();
    assert_eq!(saved, [plain], "a one-shot would come back as a loop");
}

#[test]
fn the_saves_still_queued_are_written_when_the_library_is_dropped() {
    let dir = Dir::new("take-drop");
    let (rig, mut takes, ids_) = app(&dir);
    takes.poll(&rig.e);
    drop(takes);
    assert_eq!(dir.lib().load(SR).iter().map(|c| c.id.clone()).collect::<Vec<_>>(), ids_);
}

#[test]
fn a_save_that_fails_is_reported_until_one_works() {
    let dir = Dir::new("take-error");
    let mut rig = Rig::new();
    // a file where the folder should be: every save fails
    std::fs::write(&dir.0, b"not a folder").unwrap();
    let mut takes = TakeLibrary::open(&dir.0, &mut rig.e);
    let (_one, _two) = rig.two_takes();
    assert!(takes.poll(&rig.e));
    takes.flush();
    let err = takes.error().expect("the save failed");
    assert!(err.starts_with("loop library save:"), "{err}");
    assert!(takes.removed(&rig.e).is_empty());
    std::fs::remove_file(&dir.0).unwrap();
    rig.e.rename_clip(&rig.e.loops.row.clips[0].id.clone(), "again");
    assert!(takes.poll(&rig.e));
    takes.flush();
    assert!(takes.error().is_none(), "the next save worked");
}

// PORTING.md open decision 3: not the Swift app's folder
#[test]
fn the_default_folder_is_djrusts_own() {
    if std::env::var_os("DJ_ROW_DIR").is_none() {
        assert!(default_directory().ends_with("djrust/loops"), "{}", default_directory().display());
        assert_ne!(default_directory(), LoopLibrary::default_dir(), "two apps on one library.json are not safe");
    }
    use djrust::host::stores::dir_from;
    assert_eq!(dir_from(Some("/x/y".into()), || "/fallback".into()), PathBuf::from("/x/y"));
    assert_eq!(dir_from(Some("".into()), || "/fallback".into()), PathBuf::from("/fallback"), "empty is unset");
    assert_eq!(dir_from(None, || "/fallback".into()), PathBuf::from("/fallback"));
}
