//! The journal file: one append-only JSONL file per app run under `<support>/journal/`
//! (`$DJ_JOURNAL_DIR` overrides; the Swift app's folder, [`default_directory`]), named by the
//! run's start time. The djrust app writes [`app_directory`] instead
//! (`<support>/djrust/journal`) and opens its run with [`Journal::open_run`]. Port of the
//! writing half of `Journal/Journal.swift`; the line codec, the recorder's bookkeeping and the
//! replay are `crate::journal`.
//!
//! Threads: events are recorded on the main / control side, never on the audio thread; the
//! sequence number, the entry and the `recent` ring are made under the caller's lock, and the
//! line is encoded and written on the journal's own thread (`Journal.swift`'s serial `queue`:
//! "cheap on the caller's thread"). A controller message or a frame never waits for the disk.
//!
//! Durability: each line is written with one `write` as soon as the writer gets to it (a few
//! microseconds after the event), so a crash loses at most the lines still queued and can cut
//! only the last line short (readers drop a torn last line). `sync_all` runs at most once a
//! second, on checkpoints and on close, and a second after a burst of lines ends (Swift's
//! `syncPending` / `asyncAfter`): enough against a power cut without paying a disk flush for
//! each of the hundreds of events a fader sweep makes. A flush is a barrier: it returns once
//! every line recorded before it is written and synced. On macOS `sync_all` is `F_FULLFSYNC`
//! and takes milliseconds, which is why it is not on the caller's thread.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::journal::event::{JournalEntry, JournalEvent};
use crate::journal::file::{self, Contents, JournalRecorder};
use crate::journal::stores::JournalStoreState;
use crate::journal::time;

use crate::host::stores::env_or_support;

pub const FILE_EXTENSION: &str = "jsonl";
/// How often the file is flushed to disk at most.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// `Journal.defaultDirectory`.
pub fn default_directory() -> PathBuf {
    env_or_support("DJ_JOURNAL_DIR", "journal")
}

/// Where the djrust app's journals live: `$DJ_JOURNAL_DIR` when set (the same override the Swift
/// app and `djctl journal` honour), else `<support>/djrust/journal`: not the Swift app's own
/// `<support>/journal` ([`default_directory`]), so the two apps' runs are not mixed in one list
/// (`djctl journal list --dir` reads this one).
pub fn app_directory() -> PathBuf {
    crate::host::stores::env_or_djrust("DJ_JOURNAL_DIR", "journal")
}

/// The mark a run's journal opens with (`app.start <version>`), after its checkpoint.
pub const APP_START_MARK: &str = "app.start";
/// The mark a run's journal ends with.
pub const APP_STOP_MARK: &str = "app.stop";

/// What the caller asks of the writer thread, in order.
enum Msg {
    /// Encode and write this entry's line; `urgent` (a checkpoint) syncs it at once.
    Entry {
        entry: Box<JournalEntry>,
        urgent: bool,
    },
    /// Sync, then tell the sender every message before this one has been done.
    Flush(Sender<()>),
    /// Hold the thread until the gate says go (a test's look at what has reached the disk).
    Gate(Receiver<()>),
}

/// How long the writer rests after a pass over what was queued. A send to a thread that is
/// parked has to wake it, and on a busy machine the wake-up can cost the sender milliseconds; a
/// writer that is still resting when the next line comes is not woken, and writes it in the
/// next pass with the others. So a fader sweep wakes it about once, not for every line. It also
/// bounds how long a line waits to be written (and how much a crash can lose).
pub const WRITER_REST: Duration = Duration::from_millis(5);

/// What the writer thread keeps: the file, and when it was last synced.
struct Writer {
    file: File,
    error: Arc<Mutex<Option<String>>>,
    last_sync: Instant,
    /// Written since the last sync (Swift's `syncPending`).
    pending: bool,
}

impl Writer {
    fn sync(&mut self) {
        let _ = self.file.sync_all();
        self.last_sync = Instant::now();
        self.pending = false;
    }

    /// Do one message.
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Entry { entry, urgent } => {
                let mut line = entry.line();
                line.push('\n');
                // one write per line
                if let Err(err) = self.file.write_all(line.as_bytes()) {
                    *lock_error(&self.error) = Some(err.to_string());
                    return;
                }
                self.pending = true;
                if urgent || self.last_sync.elapsed() >= SYNC_INTERVAL {
                    self.sync();
                }
            }
            Msg::Flush(done) => {
                self.sync();
                let _ = done.send(());
            }
            Msg::Gate(go) => {
                let _ = go.recv();
            }
        }
    }
}

/// The writer: `Journal.swift`'s `queue`. It owns the file; lines are written in the order they
/// were sent. A failed write is kept in `error` and the line is lost (Swift's `writeError`).
fn write_lines(file: File, jobs: Receiver<Msg>, error: Arc<Mutex<Option<String>>>) {
    let mut w = Writer {
        file,
        error,
        last_sync: Instant::now() - SYNC_INTERVAL,
        pending: false,
    };
    loop {
        let first = if w.pending {
            // a burst has ended: sync its tail once the interval since the last sync is up
            match jobs.recv_timeout(SYNC_INTERVAL.saturating_sub(w.last_sync.elapsed())) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    w.sync();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match jobs.recv() {
                Ok(m) => m,
                Err(_) => break,
            }
        };
        // everything queued, then a rest (a flush or a gate is answered at once)
        let mut lines = false;
        let mut next = Some(first);
        while let Some(m) = next {
            lines |= matches!(m, Msg::Entry { .. });
            w.handle(m);
            next = jobs.try_recv().ok();
        }
        if lines {
            std::thread::sleep(WRITER_REST);
        }
    }
    // the journal was closed or dropped: what was written is synced before the file goes
    let _ = w.file.sync_all();
}

/// A held writer (see [`JournalFile::hold_writer`]): it runs again when this is released or
/// dropped.
pub struct WriterHold(Option<Sender<()>>);

impl WriterHold {
    pub fn release(mut self) {
        if let Some(go) = self.0.take() {
            let _ = go.send(());
        }
    }
}

/// A journal being written.
pub struct JournalFile {
    pub path: PathBuf,
    pub started: i64,
    pub recorder: JournalRecorder,
    /// To the writer thread, until the file is closed.
    jobs: Option<Sender<Msg>>,
    writer: Option<JoinHandle<()>>,
    /// A write that failed (disk full…), if any.
    error: Arc<Mutex<Option<String>>>,
}

impl JournalFile {
    /// Open a new journal file for this run in `directory`; the caller then records the
    /// checkpoint as line 0.
    pub fn create(directory: &Path, started_ms: i64, sample_rate: f64) -> io::Result<JournalFile> {
        std::fs::create_dir_all(directory)?;
        let base = format!("journal-{}", time::file_stamp(started_ms));
        let mut n = 1;
        let (path, file) = loop {
            let path = if n == 1 {
                directory.join(format!("{base}.{FILE_EXTENSION}"))
            } else {
                directory.join(format!("{base}-{n}.{FILE_EXTENSION}"))
            };
            match OpenOptions::new().create_new(true).append(true).open(&path) {
                Ok(f) => break (path, f),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        let error = Arc::new(Mutex::new(None));
        let (tx, rx) = channel();
        let failed = error.clone();
        let writer = std::thread::Builder::new()
            .name("djrust-journal".into())
            .spawn(move || write_lines(file, rx, failed))?;
        Ok(JournalFile {
            path,
            started: started_ms,
            recorder: JournalRecorder::new(sample_rate),
            jobs: Some(tx),
            writer: Some(writer),
            error,
        })
    }

    /// Append an event stamped with the next sequence number, `time_ms` and `frame`, and
    /// queue its line for the writer. Cheap on the caller's thread: it never waits for the disk.
    pub fn record(&mut self, frame: i64, time_ms: i64, event: JournalEvent) -> JournalEntry {
        let urgent = matches!(event, JournalEvent::Checkpoint(_));
        let entry = self.recorder.record(frame, time_ms, event);
        if let Some(tx) = &self.jobs {
            let sent = tx.send(Msg::Entry {
                entry: Box::new(entry.clone()),
                urgent,
            });
            if sent.is_err() {
                *lock_error(&self.error) = Some("the journal's writer stopped".into());
            }
        }
        entry
    }

    /// Wait for every line recorded so far to be written, and sync it.
    pub fn flush(&mut self) {
        if let Some(tx) = &self.jobs {
            let (done, wait) = channel();
            if tx.send(Msg::Flush(done)).is_ok() {
                let _ = wait.recv();
            }
        }
    }

    /// Stop: write and sync what is queued, and close the file; nothing more is recorded.
    pub fn close(&mut self) {
        // the writer ends when its channel does, after the messages already in it
        self.jobs = None;
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }

    pub fn is_open(&self) -> bool {
        self.jobs.is_some()
    }

    /// A write that failed (disk full…), if any.
    pub fn error(&self) -> Option<String> {
        lock_error(&self.error).clone()
    }

    /// Hold the writer: nothing recorded after this reaches the file until the [`WriterHold`] is
    /// released (the tests' look at what a caller waits for).
    #[doc(hidden)]
    pub fn hold_writer(&self) -> WriterHold {
        let (go, gate) = channel();
        if let Some(tx) = &self.jobs {
            let _ = tx.send(Msg::Gate(gate));
        }
        WriterHold(Some(go))
    }
}

fn lock_error(e: &Mutex<Option<String>>) -> MutexGuard<'_, Option<String>> {
    e.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Drop for JournalFile {
    fn drop(&mut self) {
        self.close();
    }
}

/// `Journal.list`: the journal files in `directory`, newest first (by name: the start time).
pub fn list(directory: &Path) -> Vec<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    file::sort_newest_first(&mut names);
    names.into_iter().map(|n| directory.join(n)).collect()
}

/// `Journal.read`.
pub fn read(path: &Path) -> io::Result<Contents> {
    let data = std::fs::read(path)?;
    Ok(file::parse(&String::from_utf8_lossy(&data)))
}

/// `Journal.latestStores`: the stores as the newest readable journal in `directory` leaves
/// them. None when there is no readable journal.
pub fn latest_stores(directory: &Path) -> Option<JournalStoreState> {
    for path in list(directory) {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if let Some(s) = file::latest_stores(&String::from_utf8_lossy(&data)) {
            return Some(s);
        }
    }
    None
}

// MARK: attaching a journal to an engine

use crate::journal::replay::rewind_mark;

use crate::host::deck::{Engine, JournalSink};
use crate::host::replay::{replay, ClipLookup, JournalReplayOptions, SampleLookup};

/// Wall-clock milliseconds, the stamp every entry carries (metadata only: nothing the engine
/// decides reads it back).
pub type JournalClock = Box<dyn FnMut() -> i64 + Send>;

/// `JournalTime.now()`: the system clock in Unix milliseconds.
pub fn system_clock() -> JournalClock {
    Box::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    })
}

struct Shared {
    file: JournalFile,
    clock: JournalClock,
}

/// `Journal`: the app's always-on, append-only log, attached to an engine: line 0 is the
/// engine's checkpoint, and from then on every input the engine sees (`perform`, the router's
/// MIDI, a grid, a sample's file) lands in the file through the engine's [`JournalSink`]. A
/// handle: clones are the same journal, so the engine holds one and the host another (to write
/// the stores' events, checkpoints, and to close it).
#[derive(Clone)]
pub struct Journal {
    shared: Arc<Mutex<Shared>>,
}

impl Journal {
    /// `Journal.init`: open a new journal file for this run in `directory`, write its checkpoint
    /// (the engine's state now with `stores`), and attach it to `engine` (`DJEngine.journal`).
    pub fn attach(
        engine: &mut Engine,
        directory: &Path,
        started_ms: i64,
        stores: JournalStoreState,
    ) -> io::Result<Journal> {
        Journal::attach_with_clock(engine, directory, started_ms, stores, system_clock())
    }

    /// [`attach`](Self::attach) with the clock the entries are stamped by (a test's fixed one).
    pub fn attach_with_clock(
        engine: &mut Engine,
        directory: &Path,
        started_ms: i64,
        stores: JournalStoreState,
        clock: JournalClock,
    ) -> io::Result<Journal> {
        let file = JournalFile::create(directory, started_ms, engine.sample_rate)?;
        let j = Journal {
            shared: Arc::new(Mutex::new(Shared { file, clock })),
        };
        j.record(
            engine.frame(),
            JournalEvent::Checkpoint(engine.journal_state(stores, false)),
        );
        engine.journal = Some(Box::new(j.clone()));
        Ok(j)
    }

    /// `AppModel.openJournal`: this run's journal, opened once the engine stands where the last
    /// run left it. Line 0 is the engine's checkpoint (`JournalState(engine:)`, with no stores:
    /// the Rust app has none), then an `app.start <version>` mark. The system clock stamps the
    /// entries.
    pub fn open_run(engine: &mut Engine, directory: &Path, version: &str) -> io::Result<Journal> {
        let mut clock = system_clock();
        let started = clock();
        Journal::open_run_with_clock(
            engine,
            directory,
            started,
            JournalStoreState::default(),
            clock,
            version,
        )
    }

    /// [`open_run`](Self::open_run) with the start time, the stores the checkpoint carries and the
    /// clock the entries are stamped by (a test's fixed one).
    pub fn open_run_with_clock(
        engine: &mut Engine,
        directory: &Path,
        started_ms: i64,
        stores: JournalStoreState,
        clock: JournalClock,
        version: &str,
    ) -> io::Result<Journal> {
        let j = Journal::attach_with_clock(engine, directory, started_ms, stores, clock)?;
        j.record(
            engine.frame(),
            JournalEvent::Mark(format!("{APP_START_MARK} {version}")),
        );
        Ok(j)
    }

    /// `AppModel.closeJournal`: an `app.stop` mark, then [`close`](Self::close).
    pub fn close_run(&self, engine: &mut Engine) {
        self.record(engine.frame(), JournalEvent::Mark(APP_STOP_MARK.into()));
        self.close(engine);
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `Journal.record`: append an event stamped with the next sequence number, now and
    /// `frame` (the engine's: `engine.frame()`). Never from the audio thread.
    pub fn record(&self, frame: i64, event: JournalEvent) -> JournalEntry {
        let mut g = self.lock();
        let t = (g.clock)();
        g.file.record(frame, t, event)
    }

    /// `Journal.checkpoint`: a mid-file checkpoint: the engine's state now, with the decks'
    /// playheads (`playheads`) and without the stores (`hasStores` false: they are line 0's plus
    /// the store events, and a long set's favorites and notes would make every checkpoint heavy).
    pub fn checkpoint(&self, engine: &Engine, playheads: bool) -> JournalEntry {
        let mut state = engine.journal_state(JournalStoreState::default(), playheads);
        state.has_stores = false;
        self.record(engine.frame(), JournalEvent::Checkpoint(state))
    }

    /// `Journal.checkpointIfDue`: a mid-file checkpoint if one is due: something was recorded
    /// since the last one, and `checkpoint_entries` entries or `checkpoint_seconds` of engine
    /// time have gone by. An idle app writes none. Call it from the poll.
    pub fn checkpoint_if_due(&self, engine: &Engine) -> Option<JournalEntry> {
        let due = self.lock().file.recorder.checkpoint_due(engine.frame());
        due.then(|| self.checkpoint(engine, true))
    }

    /// `checkpointSeconds` and `checkpointEntries` (the defaults are 60 s and 2000).
    pub fn set_cadence(&self, seconds: f64, entries: i64) {
        let mut g = self.lock();
        g.file.recorder.checkpoint_seconds = seconds;
        g.file.recorder.checkpoint_entries = entries;
    }

    pub fn cadence(&self) -> (f64, i64) {
        let g = self.lock();
        (
            g.file.recorder.checkpoint_seconds,
            g.file.recorder.checkpoint_entries,
        )
    }

    /// `Journal.file`.
    pub fn path(&self) -> PathBuf {
        self.lock().file.path.clone()
    }

    /// `Journal.recent`: the last entries recorded (up to 400), oldest first.
    pub fn recent(&self) -> Vec<JournalEntry> {
        self.lock().file.recorder.recent().to_vec()
    }

    /// `Journal.lastSeq`: the sequence number of the last entry (0: only the checkpoint).
    pub fn last_seq(&self) -> i64 {
        self.lock().file.recorder.last_seq()
    }

    /// `Journal.error`: a write that failed (disk full…), if any.
    pub fn error(&self) -> Option<String> {
        self.lock().file.error()
    }

    /// `Journal.flush`: wait for every recorded line to be written and synced.
    pub fn flush(&self) {
        self.lock().file.flush();
    }

    /// `Journal.close`: detach from the engine, write what's pending, close the file.
    pub fn close(&self, engine: &mut Engine) {
        engine.journal = None;
        self.lock().file.close();
    }
}

impl JournalSink for Journal {
    /// What an engine says happened, on its frame: an entry (nothing once the file is closed).
    fn record(&mut self, frame: i64, event: JournalEvent) {
        if self.lock().file.is_open() {
            Journal::record(self, frame, event);
        }
    }
}

// MARK: rewind

/// `JournalRewind`: what a rewind did.
#[derive(Clone, Debug)]
pub struct JournalRewind {
    pub seq: i64,
    /// The checkpoint the replay to `seq` started from.
    pub start_seq: i64,
    /// The checkpoint the rewind wrote (with playheads).
    pub checkpoint_seq: i64,
    pub warnings: Vec<String>,
}

/// Why a rewind did not happen (`JournalRewindError`).
#[derive(Debug)]
pub enum RewindError {
    /// `seq` is not an entry of this journal (the last is `last`).
    BadSeq {
        seq: i64,
        last: i64,
    },
    Io(io::Error),
    Journal(crate::journal::replay::JournalError),
}

impl std::fmt::Display for RewindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RewindError::BadSeq { seq, last } => {
                write!(f, "no entry {seq} in this journal (the last is {last})")
            }
            RewindError::Io(e) => e.fmt(f),
            RewindError::Journal(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RewindError {}

impl Journal {
    /// `AppModel.rewindJournal`: put the engine back where the journal stood at `to_seq`. The
    /// file is flushed and replayed (from the nearest checkpoint) to `to_seq`; the engine is
    /// reset to that state ([`Engine::reset_to`]; `pump` lets the render thread take the
    /// commands between its phases: a no-op against a live callback, the offline conductor's
    /// ring drain otherwise) and the stores handed to `replace_stores` (the app's
    /// `replaceStores`, which journals them), then the journal gets a `rewind <seq>` mark and a
    /// checkpoint with playheads: a replay past it starts there. The journal stays append-only:
    /// everything after `to_seq` is still in it, line 0 too.
    ///
    /// `samples` and `clips` are the audio the replay and the reset look for (the row's and the
    /// removed clips this run has, then the library on disk).
    #[allow(clippy::too_many_arguments)]
    pub fn rewind(
        &self,
        engine: &mut Engine,
        to_seq: i64,
        samples: Option<SampleLookup>,
        clips: Option<ClipLookup>,
        pump: &mut dyn FnMut(&mut Engine),
        replace_stores: &mut dyn FnMut(&JournalStoreState),
    ) -> Result<JournalRewind, RewindError> {
        let last = self.last_seq();
        if !(0..=last).contains(&to_seq) {
            return Err(RewindError::BadSeq { seq: to_seq, last });
        }
        self.flush();
        let contents = read(&self.path()).map_err(RewindError::Io)?;
        let result = replay(
            &contents,
            &JournalReplayOptions {
                to_seq: Some(to_seq),
                playheads: true,
                samples,
                clips: clips.clone(),
                ..Default::default()
            },
        )
        .map_err(RewindError::Journal)?;
        let find = move |id: &str| clips.as_ref().and_then(|f| f(id));
        let mut warnings = result.warnings.clone();
        warnings.extend(engine.reset_to(&result.state, &find, pump));
        replace_stores(result.stores());
        self.record(engine.frame(), JournalEvent::Mark(rewind_mark(to_seq)));
        let cp = self.checkpoint(engine, true);
        Ok(JournalRewind {
            seq: to_seq,
            start_seq: result.start_seq,
            checkpoint_seq: cp.seq,
            warnings,
        })
    }
}
