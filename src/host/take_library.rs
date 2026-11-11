//! The take library: the LOOP row saved as it changes and put back at launch, the host half of
//! `AppModel.restoreLoops` / `pollLoops` and of the removed-loops panel
//! (`DJViews/AppViews.swift`, `LoopLibraryModel.swift`). The disk format is
//! [`LoopLibrary`](crate::host::stores::loops::LoopLibrary) (`library.json`, `<id>.wav`, `removed/`);
//! this is when it is written and read.
//!
//! - **At launch** ([`TakeLibrary::open`]): the saved clips come back on the row (in order, with
//!   their titles, colours and FX), then the scenes. The clips taken off the row in an earlier
//!   run are only listed (their audio stays in `removed/`): [`TakeLibrary::restore_removed`]
//!   decodes one when it is asked for, as Swift's `restoreRemoved` does.
//! - **While it runs** ([`TakeLibrary::poll`], from [`Rig::tick`](crate::host::deck::Rig::tick)): the
//!   row's *signature* (`id|title|colour|FX` of every clip, in order) and the scenes are compared
//!   with what was last saved; when either differs the row and the scenes go to one saver thread
//!   (Swift's `saveQueue`), which writes them and re-reads `removed/`. A take, a crop, a
//!   sequence, a rename, a colour, an FX change, a move, a removal and a scene edit all show in
//!   it. Nothing else does: a clip's level, mute or launch boundary is not kept (and Swift's app
//!   never saves a clip's launch settings either: `restoreClipLaunch` has no caller there).
//! - **Which clips**: not a clip the hub's pads or the input strips own ([`keeps`]). A capture
//!   from an input lives in the capture library
//!   ([`CaptureLibrary`](crate::host::stores::captures::CaptureLibrary)), which cannot share this
//!   folder, because [`LoopLibrary::save`] moves a WAV it does not index into `removed/`.
//!
//! The folder is [`default_directory`] (`<support>/djrust/loops`), **not** the Swift app's own
//! `loops` folder: two apps writing one `library.json` are not safe (PORTING.md, open decision
//! 3).
//!
//! Threads: everything but the saver is the caller's (the main thread). The saver owns the
//! `LoopLibrary` and does every read and write of it, in order, so a restore that decodes from
//! `removed/` waits for the saves queued before it, as Swift's `saveQueue.sync` does. The audio
//! thread never sees any of this.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::deck::clips::LoopClip;
use crate::deck::library::RemovedLoop;
use crate::journal::event::JournalEvent;
use crate::journal::state::LoopScene;
use crate::session::action::DeckAction;

use crate::host::deck::loop_pads::ClipExtra;
use crate::host::deck::{Engine, PerformCtx};
use crate::host::stores::loops::LoopLibrary;

/// Where djrust keeps its row: `$DJ_ROW_DIR`, else `<support>/djrust/loops`
/// (`LoopLibrary.defaultDir` is the Swift app's `<support>/loops`: see the module doc).
pub fn default_directory() -> PathBuf {
    crate::host::stores::env_or_djrust("DJ_ROW_DIR", "loops")
}

/// Whether the take library keeps a clip with these extras. Not the ones the hub's own
/// libraries and pads own: a capture from an input strip (the capture library keeps it), a
/// one-shot or choked clip and a file pad's clip (their pad loads the file again; a restored
/// plain loop would lose the one-shot). A clip the capture library also saved by hand
/// (`library_save`) is kept here too, so its colour, FX and place come back with it.
pub fn keeps(x: &ClipExtra) -> bool {
    !x.from_input && !x.once && x.choke.is_none() && x.from.is_empty()
}

/// `AppModel.rowSignature`: what the library keeps of the row, to tell when it needs saving
/// (the clips [`keeps`] says).
pub fn row_signature(engine: &Engine) -> Vec<String> {
    engine
        .loops
        .row
        .clips
        .iter()
        .filter(|c| keeps(&engine.clip_extra(&c.id)))
        .map(|c| {
            format!(
                "{}|{}|{}|{}",
                c.id,
                c.title,
                c.color.as_deref().unwrap_or(""),
                c.fx.json().canonical_js()
            )
        })
        .collect()
}

/// What the saver thread and the caller share.
#[derive(Default)]
struct Shared {
    /// The library's `removed/` as last read (`diskRemoved`).
    removed: Vec<RemovedLoop>,
    /// Bumped by every change to `removed` the caller makes (`deleteForever`): a list read on the
    /// saver before it is stale, and is dropped (`diskRemovedEpoch`, `publishRemoved`).
    epoch: u64,
    /// The last save or delete that failed (Swift prints it), until one succeeds.
    error: Option<String>,
}

fn lock(s: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Job {
    /// `store.save(row, scenes:)`, then publish `removed/`.
    Save {
        row: Vec<LoopClip>,
        scenes: Vec<LoopScene>,
        epoch: u64,
    },
    /// `store.deleteForever(id)`, then publish `removed/`.
    Delete { id: String, epoch: u64 },
    /// A removed clip with its audio, decoded to `rate` (`store.removedClip`).
    Removed {
        id: String,
        rate: f64,
        reply: Sender<Option<LoopClip>>,
    },
    /// Tell the sender everything before this has been done (`saveQueue.sync {}`).
    Flush(Sender<()>),
    /// Hold the thread until the gate says go (a test's `holdQueue`).
    Gate(Receiver<()>),
    /// Answer with the list of `removed/` as published at this point of the queue (a test's
    /// look between two jobs).
    Peek(Sender<Vec<RemovedLoop>>),
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `publishRemoved`: the library's `removed/` as just read, unless the caller changed the list
/// since the read was queued.
fn publish(shared: &Mutex<Shared>, gone: Vec<RemovedLoop>, epoch: u64) {
    let mut g = lock(shared);
    if g.epoch == epoch {
        g.removed = gone;
    }
}

fn note(shared: &Mutex<Shared>, what: &str, r: std::io::Result<()>) {
    lock(shared).error = r.err().map(|e| format!("loop library {what}: {e}"));
}

/// The saver: `saveQueue`, a serial queue that owns the library.
fn work(lib: LoopLibrary, jobs: Receiver<Job>, shared: Arc<Mutex<Shared>>) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Save { row, scenes, epoch } => {
                note(&shared, "save", lib.save(&row, Some(&scenes), None, now_ms()));
                publish(&shared, lib.removed(), epoch);
            }
            Job::Delete { id, epoch } => {
                note(&shared, "delete", lib.delete_forever(&id));
                publish(&shared, lib.removed(), epoch);
            }
            Job::Removed { id, rate, reply } => {
                let _ = reply.send(lib.removed_clip(&id, rate));
            }
            Job::Flush(done) => {
                let _ = done.send(());
            }
            Job::Gate(go) => {
                let _ = go.recv();
            }
            Job::Peek(reply) => {
                let _ = reply.send(lock(&shared).removed.clone());
            }
        }
    }
}

/// The row's autosave and its restore, over one folder. Held by the host
/// ([`Rig::takes`](crate::host::deck::Rig::takes)); dropping it finishes the saves still queued.
pub struct TakeLibrary {
    dir: PathBuf,
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
    /// What the library keeps of the row, as last handed to the saver (`savedRow`).
    saved_row: Vec<String>,
    /// The scenes as last handed to the saver (`savedScenes`).
    saved_scenes: Vec<LoopScene>,
    restored: usize,
    saves: usize,
}

/// A held saver (see [`TakeLibrary::hold`]): it runs again when this is released or dropped.
pub struct Hold(Option<Sender<()>>);

impl Hold {
    pub fn release(mut self) {
        if let Some(go) = self.0.take() {
            let _ = go.send(());
        }
    }
}

impl TakeLibrary {
    /// `AppModel.restoreLoops` over the library in `dir`: the saved clips go back on `engine`'s row
    /// (`restoreClips`), then the scenes (`restoreScenes`: they name clips, so the clips come
    /// first); the signature is taken so the first [`poll`](Self::poll) saves nothing; and the
    /// clips of `removed/` are read for the list. A folder with nothing in it is an empty
    /// library, and an unreadable entry is skipped (as [`LoopLibrary::load`] does).
    pub fn open(dir: impl Into<PathBuf>, engine: &mut Engine) -> TakeLibrary {
        let lib = LoopLibrary::new(dir);
        let dir = lib.dir.clone();
        let before = engine.loops.row.clips.len();
        engine.restore_clips(&lib.load(engine.sample_rate));
        engine.restore_scenes(&lib.load_scenes());
        let shared = Arc::new(Mutex::new(Shared {
            removed: lib.removed(),
            ..Shared::default()
        }));
        let (tx, rx) = channel();
        let saver = shared.clone();
        let thread = std::thread::Builder::new()
            .name("djrust-takes".into())
            .spawn(move || work(lib, rx, saver))
            .ok();
        TakeLibrary {
            dir,
            jobs: thread.is_some().then_some(tx),
            thread,
            shared,
            saved_row: row_signature(engine),
            saved_scenes: engine.scenes().to_vec(),
            restored: engine.loops.row.clips.len() - before,
            saves: 0,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many clips [`open`](Self::open) put back on the row.
    pub fn restored(&self) -> usize {
        self.restored
    }

    /// How many saves [`poll`](Self::poll) has handed to the saver.
    pub fn saves(&self) -> usize {
        self.saves
    }

    fn send(&self, job: Job) -> bool {
        self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok())
    }

    /// `AppModel.pollLoops`: save the row and the scenes if they differ from what was last saved.
    /// Call it from the poll (`Rig::tick`) and after an edit. True if a save was queued.
    pub fn poll(&mut self, engine: &Engine) -> bool {
        let signature = row_signature(engine);
        if signature == self.saved_row && engine.scenes() == self.saved_scenes.as_slice() {
            return false;
        }
        self.saved_row = signature;
        self.saved_scenes = engine.scenes().to_vec();
        let row: Vec<LoopClip> = engine
            .loops
            .row
            .clips
            .iter()
            .filter(|c| keeps(&engine.clip_extra(&c.id)))
            .cloned()
            .collect();
        let epoch = lock(&self.shared).epoch;
        let queued = self.send(Job::Save {
            row,
            scenes: self.saved_scenes.clone(),
            epoch,
        });
        self.saves += usize::from(queued);
        queued
    }

    /// The clips of the library's `removed/` as last read, newest first (`diskRemoved`).
    pub fn disk_removed(&self) -> Vec<RemovedLoop> {
        lock(&self.shared).removed.clone()
    }

    /// `AppModel.pollRemoved`'s list: this run's removed clips, then the library's that are
    /// neither among them nor back on the row; newest first.
    pub fn removed(&self, engine: &Engine) -> Vec<RemovedLoop> {
        let mut list: Vec<RemovedLoop> = engine
            .loops
            .row
            .removed
            .iter()
            .map(|r| RemovedLoop::from_removed(r, ""))
            .collect();
        let seen: Vec<String> = list.iter().map(|r| r.id.clone()).collect();
        list.extend(
            self.disk_removed()
                .into_iter()
                .filter(|r| !seen.contains(&r.id) && engine.clip(&r.id).is_none()),
        );
        list
    }

    /// `AppModel.restoreRemoved`: put a removed loop back on the row where it was: from this
    /// run's, or decoded from the library's `removed/` (a `libraryRestore` is journaled for that,
    /// so a replay finds it); the restore itself goes through `perform`, so it records. An
    /// error says why not: the id is not removed, or its audio is gone.
    pub fn restore_removed(&mut self, engine: &mut Engine, id: &str) -> Result<(), String> {
        if !engine.loops.row.removed.iter().any(|r| r.clip.id == id) {
            let entry = self
                .disk_removed()
                .into_iter()
                .find(|r| r.id == id)
                .ok_or_else(|| format!("no removed loop {id}"))?;
            let clip = self
                .removed_clip(id, engine.sample_rate)
                .ok_or("that loop's audio is gone")?;
            engine.remember_removed_clip(clip, entry.index);
            let frame = engine.frame();
            if let Some(j) = engine.journal.as_mut() {
                j.record(
                    frame,
                    JournalEvent::LibraryRestore {
                        id: id.to_string(),
                        index: entry.index,
                    },
                );
            }
        }
        engine.perform(
            &DeckAction::ClipRestore {
                id: id.to_string(),
                at: None,
            },
            PerformCtx::default(),
        );
        // a captured loop that was taken off the row comes back at start again
        engine.library_rerow(id);
        self.poll(engine);
        if engine.clip(id).is_some() {
            Ok(())
        } else {
            Err(format!("loop {id} did not come back"))
        }
    }

    /// `AppModel.deleteForever`: a removed loop is deleted for good, its audio too. A save
    /// already queued read `removed/` before this: its list would bring the loop back, so the
    /// epoch moves on and that list is dropped.
    pub fn delete_forever(&mut self, engine: &mut Engine, id: &str) {
        engine.perform(
            &DeckAction::ClipForget { id: id.to_string() },
            PerformCtx::default(),
        );
        let epoch = {
            let mut g = lock(&self.shared);
            g.removed.retain(|r| r.id != id);
            g.epoch += 1;
            g.epoch
        };
        self.send(Job::Delete {
            id: id.to_string(),
            epoch,
        });
        self.poll(engine);
    }

    /// A removed clip's audio, read on the saver (after the saves queued before it).
    fn removed_clip(&self, id: &str, rate: f64) -> Option<LoopClip> {
        let (reply, answer) = channel();
        self.send(Job::Removed {
            id: id.to_string(),
            rate,
            reply,
        });
        answer.recv().ok().flatten()
    }

    /// Wait for the saves queued so far to be written (`flushLoopLibrary`, `saveQueue.sync {}`).
    pub fn flush(&self) {
        let (done, wait) = channel();
        if self.send(Job::Flush(done)) {
            let _ = wait.recv();
        }
    }

    /// A save or delete that failed (disk full, folder not writable), until one succeeds.
    pub fn error(&self) -> Option<String> {
        lock(&self.shared).error.clone()
    }

    /// Hold the saver: nothing queued after this is done until the [`Hold`] is released (the
    /// tests' `holdQueue`).
    #[doc(hidden)]
    pub fn hold(&self) -> Hold {
        let (go, gate) = channel();
        self.send(Job::Gate(gate));
        Hold(Some(go))
    }

    /// The published `removed/` list as it stands when the saver gets to this point of its queue
    /// (after the saves queued so far, before the ones queued later): a test's look between a
    /// save and the delete that follows it.
    #[doc(hidden)]
    pub fn peek(&self) -> Receiver<Vec<RemovedLoop>> {
        let (reply, answer) = channel();
        self.send(Job::Peek(reply));
        answer
    }
}

impl Drop for TakeLibrary {
    /// The saves still queued are written before the folder is let go of.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
