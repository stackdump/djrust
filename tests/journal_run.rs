//! The app's side of the journal: a run opens one at start (`AppModel.openJournal`: the
//! checkpoint, then `app.start`), checkpoints it from the poll (`pollJournal`), the controllers'
//! MIDI lands in it (`ControllerRouter.route`'s `dj.journal?.record(.midi)`) and the run closes it
//! with `app.stop` (`closeJournal`). The engine halves (the file format, replay, rewind) are
//! `deck_journal.rs`' and `journal_golden.rs`'; these are the host's: a `Rig` that carries the
//! journal, over a temporary folder and an engine that renders as the app's does. Nothing here
//! touches the real support folder.

use std::path::PathBuf;

use djrust::journal::event::JournalEvent;
use djrust::journal::state::JournalState;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, DEMO_ID};
use djrust::host::deck::{OfflineConductor, PerformCtx};
use djrust::host::journal::{self, read, Journal, JournalFile};
use djrust::host::replay::{offline_conductor, replay, JournalReplayOptions};
use djrust::host::stores::temp_path;
use djrust::host::take_library::TakeLibrary;

const APC: &str = "APC mini mk2 Control";
const RATE: f64 = 11_025.0;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        Dir(temp_path(tag))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn conductor() -> OfflineConductor {
    offline_conductor(vec!["A".into(), "B".into()], RATE)
}

fn perform(c: &mut OfflineConductor, a: DeckAction) {
    c.rig.engine.perform(&a, PerformCtx::default());
}

fn state_of(c: &OfflineConductor) -> JournalState {
    c.engine().journal_state(Default::default(), false)
}

fn demo(deck: &str) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Sample {
            id: DEMO_ID.into(),
            title: "demo".into(),
            grid: None,
        },
    }
}

fn events(path: &std::path::Path) -> Vec<JournalEvent> {
    read(path).unwrap().entries.into_iter().map(|e| e.event).collect()
}

// JournalModel.openJournal / closeJournal: the checkpoint is line 0, `app.start` follows it, and
// the run ends with `app.stop`
#[test]
fn a_run_opens_with_a_checkpoint_and_app_start_and_ends_with_app_stop() {
    let dir = Dir::new("run-marks");
    let mut c = conductor();
    perform(&mut c, demo("A"));
    let path = c.rig.open_journal(&dir.0, "1.2.3").unwrap();
    assert!(path.starts_with(&dir.0) && path.extension().is_some_and(|e| e == "jsonl"));
    assert!(c.engine().journal.is_some() && c.rig.journal.is_some());
    c.rig.journal.as_ref().unwrap().flush();
    let e = events(&path);
    assert!(matches!(e[0], JournalEvent::Checkpoint(_)), "line 0 is the state now");
    assert_eq!(e[1], JournalEvent::Mark("app.start 1.2.3".into()));
    let JournalEvent::Checkpoint(cp) = &e[0] else { unreachable!() };
    assert!(
        matches!(cp.decks["A"].source, DeckSource::Sample { .. }),
        "a deck loaded before the journal opened is in it"
    );
    c.rig.close_journal();
    assert!(c.engine().journal.is_none() && c.rig.journal.is_none());
    let e = events(&path);
    assert_eq!(e.last(), Some(&JournalEvent::Mark("app.stop".into())));
    assert_eq!(e.len(), 3, "checkpoint, start, stop");
    // closing again is nothing
    c.rig.close_journal();
    assert_eq!(events(&path).len(), 3);
}

// the journal of a set played on the controllers, written while the engine renders and read back
// (JournalReplayTests.replayingTheJournalRebuildsTheLiveState, through the app's rig)
#[test]
fn a_set_played_on_the_controllers_is_journaled_and_replays() {
    let dir = Dir::new("run-set");
    let mut c = conductor();
    perform(&mut c, demo("A"));
    let path = c.rig.open_journal(&dir.0, "test").unwrap();
    assert!(c.rig.attach(APC));
    let initial = state_of(&c).decks["A"].params.volume;
    let fader = [0xB0, 48, 64];
    let (down, up) = ([0x90, 48, 0x7F], [0x80, 48, 0x00]);
    for b in 0..120usize {
        c.step_with(|c| {
            let now = c.now();
            match b {
                0 => perform(c, DeckAction::Play { deck: "A".into(), on: true }),
                20 => {
                    c.rig.route(APC, &fader, now);
                }
                40 => {
                    c.rig.route(APC, &down, now);
                }
                44 => {
                    c.rig.route(APC, &up, now);
                }
                60 => perform(c, DeckAction::Crossfader(0.35)),
                _ => {}
            }
        });
    }
    let live = state_of(&c);
    assert_ne!(live.decks["A"].params.volume, initial, "the APC fader moved the strip");
    assert_eq!(live.crossfader, 0.35);
    c.rig.close_journal();

    let file = read(&path).unwrap();
    assert!(file.skipped.is_empty() && !file.truncated);
    let e: Vec<&JournalEvent> = file.entries.iter().map(|e| &e.event).collect();
    for m in [fader, down, up] {
        assert!(
            e.contains(&&JournalEvent::Midi { port: APC.into(), bytes: m.to_vec() }),
            "the router journals {m:?}"
        );
    }
    assert!(e.contains(&&JournalEvent::Action(DeckAction::Play { deck: "A".into(), on: true })));
    assert!(e.contains(&&JournalEvent::Action(DeckAction::Crossfader(0.35))));
    let seqs: Vec<i64> = file.entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (0..seqs.len() as i64).collect::<Vec<_>>());
    let frames: Vec<i64> = file.entries.iter().map(|e| e.frame).collect();
    assert!(frames.windows(2).all(|w| w[0] <= w[1]), "frames never go back");
    assert!(frames.last().unwrap() > &0, "stamped with the engine's frame, not 0");

    let r = replay(&file, &JournalReplayOptions::default()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(r.state.json().canonical_js(), live.json().canonical_js());
    assert!(r.state == live, "the journal rebuilds the state the set ended in");
}

// JournalCheckpointTests.checkpointsComeOnTheirCadence, from the poll: the host's tick is
// what writes the mid-file checkpoint
#[test]
fn the_tick_writes_a_checkpoint_when_one_is_due() {
    let dir = Dir::new("run-tick");
    let mut c = conductor();
    let path = c.rig.open_journal(&dir.0, "test").unwrap();
    let j = c.rig.journal.clone().unwrap();
    j.set_cadence(0.5, 5);
    let checkpoints = |j: &Journal| {
        j.flush();
        let f = read(&j.path()).unwrap();
        f.checkpoint_indices().len()
    };
    c.run_to((0.2 * RATE) as i64);
    assert_eq!(checkpoints(&j), 1, "an idle app writes none");
    for i in 0..5 {
        j.record(c.engine().frame(), JournalEvent::Mark(format!("m{i}")));
    }
    c.step_with(|_| {});
    c.step_with(|_| {});
    c.step_with(|_| {});
    assert_eq!(checkpoints(&j), 2, "five entries since line 0's: the tick checkpoints");
    // the stores stay out of a mid-file checkpoint, the playheads go in
    let f = read(&path).unwrap();
    let mid = f.checkpoint_indices()[1];
    let JournalEvent::Checkpoint(cp) = &f.entries[mid].event else { unreachable!() };
    assert!(!cp.has_stores);
    // without a journal the tick has nothing to write
    c.rig.close_journal();
    c.run_to(c.engine().frame() + RATE as i64);
    assert_eq!(read(&path).unwrap().checkpoint_indices().len(), 2);
}

#[test]
fn opening_another_journal_closes_the_first_and_a_blocked_folder_is_an_error() {
    let dir = Dir::new("run-twice");
    let mut c = conductor();
    let first = c.rig.open_journal(&dir.0, "one").unwrap();
    let second = c.rig.open_journal(&dir.0, "two").unwrap();
    assert_ne!(first, second);
    assert_eq!(events(&first).last(), Some(&JournalEvent::Mark("app.stop".into())));
    c.rig.journal.as_ref().unwrap().flush();
    assert_eq!(events(&second)[1], JournalEvent::Mark("app.start two".into()));
    // a file where the folder should be
    let blocked = temp_path("run-blocked");
    std::fs::write(&blocked, b"x").unwrap();
    assert!(c.rig.open_journal(&blocked, "three").is_err());
    assert!(c.rig.journal.is_none() && c.engine().journal.is_none());
    let _ = std::fs::remove_file(&blocked);
}

// the row's autosave is polled by the same tick, and the end of the run saves what is left
#[test]
fn the_tick_saves_the_row_and_the_end_of_the_run_closes_everything() {
    let dir = Dir::new("run-row");
    let jdir = Dir::new("run-row-journal");
    let mut c = conductor();
    let takes = TakeLibrary::open(&dir.0, &mut c.rig.engine);
    c.rig.takes = Some(takes);
    let path = c.rig.open_journal(&jdir.0, "test").unwrap();
    perform(&mut c, demo("A"));
    for b in 0..400usize {
        c.step_with(|c| match b {
            0 => perform(c, DeckAction::Play { deck: "A".into(), on: true }),
            30 => perform(c, DeckAction::LoopRecord { deck: "A".into(), on: true, bars: Some(1) }),
            32 => perform(c, DeckAction::LoopRecord { deck: "A".into(), on: false, bars: None }),
            _ => {}
        });
    }
    let clips: Vec<String> = c.engine().loops.row.clips.iter().map(|k| k.id.clone()).collect();
    assert!(!clips.is_empty(), "LOOP record made a take");
    assert_eq!(c.rig.takes.as_ref().unwrap().saves(), 1, "the tick queued its save");
    c.rig.close_persistence();
    assert!(c.rig.journal.is_none());
    assert_eq!(events(&path).last(), Some(&JournalEvent::Mark("app.stop".into())));
    // the next launch finds the take
    let mut e2 = offline_conductor(vec!["A".into(), "B".into()], RATE);
    let again = TakeLibrary::open(&dir.0, &mut e2.rig.engine);
    assert_eq!(again.restored(), clips.len());
    let back: Vec<String> = e2.engine().loops.row.clips.iter().map(|k| k.id.clone()).collect();
    assert_eq!(back, clips);
    assert!(c.rig.persistence_errors().is_empty());
}

// the end of the run (`AppModel.stop`) is the only protection for a change made in the last
// frame, which no tick has seen: it polls the row once more and waits for the saver
#[test]
fn a_change_made_in_the_last_frame_is_saved_when_the_run_ends() {
    let dir = Dir::new("run-last");
    let jdir = Dir::new("run-last-journal");
    let mut c = conductor();
    c.rig.takes = Some(TakeLibrary::open(&dir.0, &mut c.rig.engine));
    c.rig.open_journal(&jdir.0, "test").unwrap();
    perform(&mut c, demo("A"));
    for b in 0..400usize {
        c.step_with(|c| match b {
            0 => perform(c, DeckAction::Play { deck: "A".into(), on: true }),
            30 => perform(c, DeckAction::LoopRecord { deck: "A".into(), on: true, bars: Some(1) }),
            32 => perform(c, DeckAction::LoopRecord { deck: "A".into(), on: false, bars: None }),
            _ => {}
        });
    }
    let id = c.engine().loops.row.clips[0].id.clone();
    let saves = c.rig.takes.as_ref().unwrap().saves();
    assert_eq!(saves, 1, "the tick saved the take");
    // the last frame: a rename and a scene, and then the user quits before another tick
    c.rig.engine.rename_clip(&id, "last word");
    c.rig.engine.save_scene("Scene 1");
    assert_eq!(c.rig.takes.as_ref().unwrap().saves(), saves, "no tick has seen them");
    // the saver is busy for a moment (a slow disk): quitting has to wait for it
    let hold = c.rig.takes.as_ref().unwrap().hold();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        hold.release();
    });
    c.rig.close_persistence();
    assert_eq!(c.rig.takes.as_ref().unwrap().saves(), saves + 1, "the end of the run polled");
    // already on disk when it returns (the process may end right after it)
    let lib = djrust::host::stores::loops::LoopLibrary::new(&dir.0);
    let titles: Vec<String> = lib.load(RATE).iter().map(|k| k.title.clone()).collect();
    assert_eq!(titles, ["last word"], "the rename is on disk");
    let scenes = lib.load_scenes();
    assert_eq!(scenes.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Scene 1"]);
    release.join().unwrap();
}

// `Journal.record` is cheap on the caller's thread: the line is written by the journal's own
// writer, in order, and a flush, a close or a drop waits for it
#[test]
fn a_record_never_waits_for_the_disk_and_what_is_queued_is_written_in_order() {
    let dir = Dir::new("run-writer");
    let mut f = JournalFile::create(&dir.0, 1_760_000_000_000, RATE).unwrap();
    let path = f.path.clone();
    f.record(0, 1, JournalEvent::Mark("line 0".into()));
    f.flush();
    assert_eq!(read(&path).unwrap().entries.len(), 1);
    // the writer is stuck (a slow disk): records still return, and nothing has been written
    let hold = f.hold_writer();
    for i in 1..=200 {
        f.record(i, 1 + i, JournalEvent::Mark(format!("line {i}")));
    }
    assert_eq!(f.recorder.last_seq(), 200, "numbered on the caller's side");
    assert_eq!(read(&path).unwrap().entries.len(), 1, "no line was written by the caller");
    hold.release();
    f.flush();
    let e = read(&path).unwrap().entries;
    assert_eq!(e.len(), 201);
    assert!(e.iter().enumerate().all(|(i, x)| x.seq == i as i64), "in the order recorded");
    assert_eq!(e[200].event, JournalEvent::Mark("line 200".into()));
    assert!(f.error().is_none());

    // closing waits for what is queued, then nothing more is recorded
    let hold = f.hold_writer();
    f.record(201, 202, JournalEvent::Mark("last".into()));
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        hold.release();
    });
    f.close();
    assert!(!f.is_open());
    assert_eq!(read(&path).unwrap().entries.len(), 202, "close wrote the queued line");
    f.record(202, 203, JournalEvent::Mark("after".into()));
    assert_eq!(read(&path).unwrap().entries.len(), 202, "a closed journal records nothing");
    release.join().unwrap();

    // dropping it does too
    let mut g = JournalFile::create(&dir.0, 1_760_000_100_000, RATE).unwrap();
    let path2 = g.path.clone();
    let hold = g.hold_writer();
    g.record(0, 1, JournalEvent::Mark("only".into()));
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        hold.release();
    });
    drop(g);
    assert_eq!(read(&path2).unwrap().entries.len(), 1, "a drop wrote the queued line");
    release.join().unwrap();
}

// A fader sweep through the app's journal (one record every 2 ms for 6 s, as the controllers
// make it): how often `Journal::record` made its caller wait. Written on the caller's thread it
// stalled for 10 ms and more about once a second (`File::sync_all` is `F_FULLFSYNC` on macOS:
// 13.8 ms worst, 4 calls over 5 ms in one run, 10 to 24 in others on a busy machine); with the
// writer thread it is a few at most, and those are the machine's own scheduling (the measure
// is a timing, so it is not run by default; run it on a quiet machine:
// `cargo test -p djhost --test journal_run -- --ignored --nocapture`).
#[test]
#[ignore = "a six second timing measurement"]
fn a_fader_sweep_never_makes_record_wait_for_the_disk() {
    let dir = Dir::new("run-sweep");
    let mut c = conductor();
    c.rig.open_journal(&dir.0, "test").unwrap();
    let j = c.rig.journal.clone().unwrap();
    let ms = std::time::Duration::from_millis;
    let (mut worst, mut slow) = (std::time::Duration::ZERO, 0);
    let start = std::time::Instant::now();
    let mut n = 0;
    while start.elapsed() < std::time::Duration::from_secs(6) {
        let t = std::time::Instant::now();
        j.record(n, JournalEvent::Action(DeckAction::Crossfader((n % 100) as f64 / 100.0)));
        let took = t.elapsed();
        worst = worst.max(took);
        slow += usize::from(took > ms(5));
        n += 1;
        // every 2 ms, on the dot (a sleep is too coarse: macOS wakes it ten ms late)
        let next = start + ms(2) * n as u32;
        while std::time::Instant::now() < next {
            std::hint::spin_loop();
        }
    }
    eprintln!("{n} records, worst {worst:?}, {slow} over 5 ms");
    assert!(slow <= 3, "{slow} records over 5 ms (worst {worst:?}): is the machine busy?");
    j.flush();
    // the checkpoint and `app.start`, then every record
    assert_eq!(read(&j.path()).unwrap().entries.len() as i64, n + 2, "and every line is there");
}

// PORTING.md open decision 3, for the journal: djrust's own folder, not the Swift app's
#[test]
fn the_app_journals_in_its_own_folder() {
    if std::env::var_os("DJ_JOURNAL_DIR").is_none() {
        let d = journal::app_directory();
        assert!(d.ends_with("djrust/journal"), "{}", d.display());
        assert_ne!(d, journal::default_directory(), "not the Swift app's journal folder");
    }
}
