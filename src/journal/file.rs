//! A journal file read back, and the pure bookkeeping of writing one. Port of the reading
//! half of `Journal/Journal.swift` (`Journal.Contents`, `parse`, `latestStores`'s folding)
//! and the recorder state (`record`, `checkpointIfDue`); the file itself (open, append,
//! fsync, list) is `crate::host::journal`.

use alloc::string::String;
use alloc::vec::Vec;

use super::event::{JournalEntry, JournalEvent};
use super::state::JournalState;
use super::stores::JournalStoreState;

/// `Journal.Contents`: a journal file read back.
#[derive(Clone, Debug, PartialEq)]
pub struct Contents {
    pub entries: Vec<JournalEntry>,
    /// Lines that didn't parse or are of a kind this build doesn't know (line numbers from 1).
    pub skipped: Vec<usize>,
    /// The last line was cut short (the app stopped mid-write).
    pub truncated: bool,
}

impl Contents {
    /// The checkpoint the file starts with.
    pub fn checkpoint(&self) -> Option<&JournalState> {
        match self.entries.first().map(|e| &e.event) {
            Some(JournalEvent::Checkpoint(s)) => Some(s),
            _ => None,
        }
    }

    /// Where the checkpoints are in `entries`, in order: line 0's, then the mid-file ones.
    pub fn checkpoint_indices(&self) -> Vec<usize> {
        (0..self.entries.len())
            .filter(|&i| matches!(self.entries[i].event, JournalEvent::Checkpoint(_)))
            .collect()
    }

    /// The checkpoints rewinds wrote (indices in `entries`): each one right after its mark.
    pub fn rewind_checkpoint_indices(&self) -> Vec<usize> {
        self.checkpoint_indices()
            .into_iter()
            .filter(|&i| i > 0 && self.entries[i - 1].event.is_rewind_mark())
            .collect()
    }
}

/// `Journal.parse`: a last line without its newline that doesn't parse is a torn write
/// (`truncated`), not an error; other bad lines are skipped and listed.
pub fn parse(text: &str) -> Contents {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let ends_clean = text.ends_with('\n') || text.is_empty();
    if ends_clean && lines.last() == Some(&"") {
        lines.pop();
    }
    let mut out = Contents {
        entries: Vec::new(),
        skipped: Vec::new(),
        truncated: false,
    };
    let n = lines.len();
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        if let Some(e) = JournalEntry::from_line(line) {
            out.entries.push(e);
        } else if i == n - 1 && !ends_clean {
            out.truncated = true;
        } else {
            out.skipped.push(i + 1);
        }
    }
    out
}

/// `Journal.latestStores`'s folding: the stores as a journal's text leaves them: its
/// checkpoint's, then its store events. Mid-file checkpoints carry no stores, so only line
/// 0's counts. None when the text does not start with a checkpoint. Only the checkpoint and
/// the store lines are parsed: a long set is mostly knobs.
pub fn latest_stores(text: &str) -> Option<JournalStoreState> {
    const STORE_KINDS: [&str; 5] = [
        "\"k\":\"favorite.",
        "\"k\":\"crate.",
        "\"k\":\"variation.",
        "\"k\":\"note.",
        "\"k\":\"stores.",
    ];
    let mut lines = text.split('\n').filter(|l| !l.is_empty());
    let head = JournalEntry::from_line(lines.next()?)?;
    let JournalEvent::Checkpoint(state) = head.event else {
        return None;
    };
    let mut s = state.stores;
    for line in lines {
        if STORE_KINDS.iter().any(|k| line.contains(k)) {
            if let Some(e) = JournalEntry::from_line(line) {
                apply_store_event(&mut s, &e.event);
            }
        }
    }
    Some(s)
}

/// `JournalStoreState.apply`: apply a store event; false for events that are not about the
/// stores.
pub fn apply_store_event(s: &mut JournalStoreState, e: &JournalEvent) -> bool {
    use super::stores::*;
    match e {
        JournalEvent::FavoriteAdd(f) => s.favorites = favorites_adding(f, &s.favorites),
        JournalEvent::FavoriteRemove { cid } => s.favorites = favorites_removing(cid, &s.favorites),
        JournalEvent::FavoriteTags { cid, tags } => {
            s.favorites = favorites_setting_tags(cid, tags, &s.favorites)
        }
        JournalEvent::CrateAdd { name } => {
            s.crates = JournalStoreState::adding_crate(name, &s.crates)
        }
        JournalEvent::CrateDelete { name } => s.crates.retain(|c| c != name),
        JournalEvent::VariationSave {
            instrument,
            variation,
        } => s.variations = variations_saving(variation, instrument, &s.variations),
        JournalEvent::VariationRename {
            instrument,
            name,
            to,
        } => s.variations = variations_renaming(name, to, instrument, &s.variations),
        JournalEvent::VariationDelete { instrument, name } => {
            s.variations = variations_deleting(name, instrument, &s.variations)
        }
        JournalEvent::NoteAdd(n) => s.notes = notes_adding(&s.notes, n),
        JournalEvent::NoteEdit { id, text, at } => s.notes = notes_editing(&s.notes, id, text, *at),
        JournalEvent::NoteDelete { id } => s.notes = notes_deleting(&s.notes, id),
        JournalEvent::StoresReplace(all) => *s = all.clone(),
        _ => return false,
    }
    true
}

/// Journal file names sort by start time: newest first (`Journal.list`).
pub fn sort_newest_first(names: &mut Vec<String>) {
    names.retain(|n| n.starts_with("journal-") && n.ends_with(".jsonl"));
    names.sort_by(|a, b| b.cmp(a));
}

/// The journal's own bookkeeping (`Journal.record`, `checkpointIfDue`, `recent`): sequence
/// numbers, the last checkpoint, the ring of recent entries. The caller stamps the time and
/// frame and writes the line.
#[derive(Clone, Debug)]
pub struct JournalRecorder {
    /// The cadence `checkpoint_due` keeps (tests shorten it).
    pub checkpoint_seconds: f64,
    pub checkpoint_entries: i64,
    pub sample_rate: f64,
    next_seq: i64,
    ring: Vec<JournalEntry>,
    last_checkpoint_seq: i64,
    last_checkpoint_frame: i64,
}

/// How many entries `recent` keeps (the History panel).
pub const RECENT_CAPACITY: usize = 400;
/// A mid-file checkpoint is due after this much engine time (seconds)…
pub const DEFAULT_CHECKPOINT_SECONDS: f64 = 60.0;
/// …or this many entries since the last one, whichever comes first.
pub const DEFAULT_CHECKPOINT_ENTRIES: i64 = 2000;

impl JournalRecorder {
    pub fn new(sample_rate: f64) -> Self {
        JournalRecorder {
            checkpoint_seconds: DEFAULT_CHECKPOINT_SECONDS,
            checkpoint_entries: DEFAULT_CHECKPOINT_ENTRIES,
            sample_rate,
            next_seq: 0,
            ring: Vec::new(),
            last_checkpoint_seq: 0,
            last_checkpoint_frame: 0,
        }
    }

    /// Append an event, stamped with the next sequence number, `time_ms` and `frame`.
    pub fn record(&mut self, frame: i64, time_ms: i64, event: JournalEvent) -> JournalEntry {
        let e = JournalEntry {
            seq: self.next_seq,
            time: time_ms,
            frame,
            event,
        };
        self.next_seq += 1;
        if matches!(e.event, JournalEvent::Checkpoint(_)) {
            self.last_checkpoint_seq = e.seq;
            self.last_checkpoint_frame = frame;
        }
        self.ring.push(e.clone());
        if self.ring.len() > RECENT_CAPACITY + RECENT_CAPACITY / 4 {
            let drop = self.ring.len() - RECENT_CAPACITY;
            self.ring.drain(0..drop);
        }
        e
    }

    /// A mid-file checkpoint is due: something was recorded since the last one, and
    /// `checkpoint_entries` entries or `checkpoint_seconds` of engine time have gone by. An
    /// idle app writes none.
    pub fn checkpoint_due(&self, frame: i64) -> bool {
        let since = self.next_seq - 1 - self.last_checkpoint_seq;
        if since <= 0 {
            return false;
        }
        since >= self.checkpoint_entries
            || (frame - self.last_checkpoint_frame) as f64
                >= self.checkpoint_seconds * self.sample_rate
    }

    /// The last entries recorded (up to `RECENT_CAPACITY`), oldest first.
    pub fn recent(&self) -> &[JournalEntry] {
        let n = self.ring.len();
        &self.ring[n.saturating_sub(RECENT_CAPACITY)..]
    }

    /// The sequence number of the last entry (0: only the checkpoint).
    pub fn last_seq(&self) -> i64 {
        self.next_seq - 1
    }
}
