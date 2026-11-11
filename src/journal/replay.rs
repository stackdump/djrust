//! Rebuilding state from a journal. Port of `Journal/JournalReplay.swift` and the pure half
//! of `JournalRewind.swift`.
//!
//! A fresh offline engine is stood where the checkpoint says, then every event is applied at
//! its frame by the same conductor steps `SessionPlayer` takes, and the stores are rebuilt by
//! the stores' own edit functions. Deterministic: two replays of one file are the same state
//! and the same audio.
//!
//! A replay starts from the nearest checkpoint at or before where it is asked to stop (a
//! mid-file one puts the decks at their playheads), so seeking late into a long set doesn't
//! render it all. That gives the same *state* as a replay from line 0, not the same audio.
//! `from_start` replays from line 0. A rewind (`Journal.rewindMark` + a checkpoint with
//! playheads) is as far back as a replay goes.
//!
//! The engine is behind [`ReplayEngine`]; [`plan`] and [`run`] are the parts of
//! `Journal.replay` that do not depend on it.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use super::event::{JournalEntry, JournalEvent};
use super::file::{apply_store_event, Contents};
use super::state::JournalState;
use super::stores::JournalStoreState;
use crate::session::action::DeckAction;
use crate::session::model::{Session, SessionEvent, SessionEventKind};
use crate::session::wav::RenderedAudio;

/// `JournalError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    /// The file doesn't start with a checkpoint (empty, or not a journal).
    NoCheckpoint,
    /// `export-session` was given a slice with nothing in it.
    EmptySlice,
    /// `export-session` was given a slice with a rewind in it (the rewind mark's seq).
    SpansRewind(i64),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::NoCheckpoint => {
                f.write_str("the journal does not start with a checkpoint")
            }
            JournalError::EmptySlice => f.write_str("no events in that slice"),
            JournalError::SpansRewind(seq) => write!(
                f,
                "the slice spans a rewind (seq {seq}): export either side of it"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for JournalError {}

/// `Journal.rewindMark`: the mark a rewind to `seq` writes.
pub fn rewind_mark(seq: i64) -> String {
    format!("rewind {seq}")
}

/// What the replay was asked for.
#[derive(Clone, Debug, Default)]
pub struct ReplayOptions {
    /// Stop after this sequence number (all with None).
    pub to_seq: Option<i64>,
    /// Render at least to this many frames after line 0.
    pub end_frame: Option<i64>,
    pub render: bool,
    pub from_start: bool,
}

/// Where a replay starts and what it applies.
#[derive(Clone, Debug)]
pub struct ReplayPlan {
    /// The checkpoint's index in `entries`.
    pub start_index: usize,
    pub start_seq: i64,
    /// The checkpoint's state, with its stores folded in when it had none.
    pub state: JournalState,
    /// `start.frame`.
    pub base: i64,
    /// `base - origin`: frames before the start checkpoint.
    pub offset: i64,
    /// The events after the start checkpoint, in bounds.
    pub events: Vec<JournalEntry>,
    /// `sample.file` paths by content id.
    pub sample_paths: Vec<(String, String)>,
    /// Frames (from the start checkpoint) the loop must reach.
    pub end: i64,
    pub end_here: Option<i64>,
}

/// The first half of `Journal.replay`: pick the start checkpoint and the events to apply.
pub fn plan(contents: &Contents, opts: &ReplayOptions) -> Result<ReplayPlan, JournalError> {
    let (Some(head), Some(first)) = (contents.checkpoint(), contents.entries.first()) else {
        return Err(JournalError::NoCheckpoint);
    };
    let origin = first.frame;
    let in_bounds = |e: &JournalEntry| {
        opts.to_seq.map_or(true, |s| e.seq <= s)
            && opts.end_frame.map_or(true, |f| e.frame - origin <= f)
    };
    // the start: the last checkpoint in bounds; a rewind's checkpoint counts from its mark on
    let rewinds: BTreeSet<usize> = contents.rewind_checkpoint_indices().into_iter().collect();
    let startable = |i: usize| {
        in_bounds(&contents.entries[i])
            || (rewinds.contains(&i) && in_bounds(&contents.entries[i - 1]))
    };
    let checkpoints: Vec<usize> = contents
        .checkpoint_indices()
        .into_iter()
        .filter(|&i| startable(i))
        .collect();
    let mut start_index = checkpoints
        .iter()
        .rev()
        .copied()
        .find(|i| rewinds.contains(i))
        .unwrap_or(0);
    if !opts.from_start {
        start_index = checkpoints.last().copied().unwrap_or(0);
    }
    let start = &contents.entries[start_index];
    let JournalEvent::Checkpoint(state) = &start.event else {
        return Err(JournalError::NoCheckpoint);
    };
    let mut state = state.clone();
    if !state.has_stores {
        // a mid checkpoint has no stores: line 0's, then the store events before it
        let mut s = head.stores.clone();
        for e in contents.entries.iter().take(start_index).skip(1) {
            apply_store_event(&mut s, &e.event);
        }
        state.stores = s;
        state.has_stores = true;
    }
    let base = start.frame;
    let offset = base - origin;
    let events: Vec<JournalEntry> = contents.entries[start_index + 1..]
        .iter()
        .filter(|e| in_bounds(e))
        .cloned()
        .collect();
    let mut paths: Vec<(String, String)> = Vec::new();
    for e in &contents.entries {
        if let JournalEvent::SampleFile { id, path } = &e.event {
            paths.retain(|p| p.0 != *id);
            paths.push((id.clone(), path.clone()));
        }
    }
    let last_frame = events.last().map(|e| e.frame - base).unwrap_or(0);
    let end_here = opts.end_frame.map(|f| (f - offset).max(0));
    let end = end_here.unwrap_or(0).max(last_frame);
    Ok(ReplayPlan {
        start_index,
        start_seq: start.seq,
        state,
        base,
        offset,
        events,
        sample_paths: paths,
        end,
        end_here,
    })
}

/// What the replay drives (the engine, router and conductor of `Journal.replay`).
pub trait ReplayEngine {
    /// Frames rendered so far (`dj.frame`).
    fn frame(&self) -> i64;
    /// Apply one event that is not about the stores (`Journal.apply`: actions, MIDI,
    /// library restores). `warnings` collects what could not be rebuilt.
    fn apply(&mut self, event: &JournalEvent, warnings: &mut Vec<String>);
    /// One block: housekeeping then render. Empty on failure.
    fn step(&mut self) -> (Vec<f32>, Vec<f32>);
}

/// `JournalReplayResult` without the state (the engine's `JournalState(engine:…)` is the
/// host's to take).
#[derive(Clone, Debug)]
pub struct ReplayOutcome {
    /// The stores as rebuilt.
    pub stores: JournalStoreState,
    pub start_seq: i64,
    /// The last entry applied (the start checkpoint's if none).
    pub last_seq: i64,
    /// Engine frames from line 0 to where the replay stopped.
    pub frames: i64,
    /// The audio, when asked for.
    pub audio: Option<RenderedAudio>,
    pub warnings: Vec<String>,
}

/// The second half of `Journal.replay`: the stepping loop. `warnings` starts with what
/// restoring the checkpoint reported.
pub fn run<E: ReplayEngine>(
    plan: &ReplayPlan,
    engine: &mut E,
    render: bool,
    sample_rate: f64,
    mut warnings: Vec<String>,
) -> ReplayOutcome {
    let mut stores = plan.state.stores.clone();
    let mut left: Vec<f32> = Vec::new();
    let mut right: Vec<f32> = Vec::new();
    let mut next = 0usize;
    let mut last_seq = plan.start_seq;
    // At least one block, so the last event's effects are rendered.
    loop {
        while next < plan.events.len() && plan.events[next].frame - plan.base <= engine.frame() {
            let e = &plan.events[next];
            if !apply_store_event(&mut stores, &e.event) {
                engine.apply(&e.event, &mut warnings);
            }
            last_seq = e.seq;
            next += 1;
        }
        let (l, r) = engine.step();
        if l.is_empty() {
            break;
        }
        if render {
            left.extend(l);
            right.extend(r);
        }
        if !(next < plan.events.len() || engine.frame() < plan.end) {
            break;
        }
    }
    if render {
        if let Some(end_here) = plan.end_here {
            left.truncate(end_here as usize);
            right.truncate(end_here as usize);
        }
    }
    ReplayOutcome {
        stores,
        start_seq: plan.start_seq,
        last_seq,
        frames: engine.frame() + plan.offset,
        audio: if render {
            Some(RenderedAudio {
                sample_rate,
                left,
                right,
            })
        } else {
            None
        },
        warnings,
    }
}

/// `DJEngine.defaultLoopSettings`: the row's settings of a fresh engine.
pub fn default_loop_settings() -> Vec<DeckAction> {
    alloc::vec![
        DeckAction::LoopQuantize { beats: 4 },
        DeckAction::LoopFade {
            mode: "10ms".into()
        },
        DeckAction::LoopKeyLock { on: false },
        DeckAction::TapBars { bars: Some(2) },
        DeckAction::LoopFollow {
            mode: "louder".into()
        },
    ]
}

/// What `export_session` needs replayed first: the state at the start of the slice.
#[derive(Clone, Debug)]
pub struct ExportSlice {
    pub from_seq: i64,
    /// `replay(to_seq: start.seq - 1, end_frame: start.frame - first.frame)`.
    pub replay_to_seq: i64,
    pub replay_end_frame: i64,
    slice: Vec<JournalEntry>,
    start_frame: i64,
    last_frame: i64,
}

/// The first half of `Journal.exportSession`: the slice `from…to` (sequence numbers,
/// inclusive), refusing an empty one or one with a rewind mark in it.
pub fn export_slice(contents: &Contents, from: i64, to: i64) -> Result<ExportSlice, JournalError> {
    let Some(first) = contents.entries.first() else {
        return Err(JournalError::NoCheckpoint);
    };
    if contents.checkpoint().is_none() {
        return Err(JournalError::NoCheckpoint);
    }
    let slice: Vec<JournalEntry> = contents
        .entries
        .iter()
        .filter(|e| e.seq >= from.max(1) && e.seq <= to)
        .cloned()
        .collect();
    let (Some(start), Some(last)) = (slice.first(), slice.last()) else {
        return Err(JournalError::EmptySlice);
    };
    if let Some(r) = slice.iter().find(|e| e.event.is_rewind_mark()) {
        return Err(JournalError::SpansRewind(r.seq));
    }
    Ok(ExportSlice {
        from_seq: from,
        replay_to_seq: start.seq - 1,
        replay_end_frame: start.frame - first.frame,
        start_frame: start.frame,
        last_frame: last.frame,
        slice: slice.clone(),
    })
}

/// The second half of `Journal.exportSession`: the slice as a normal `Session`. `at` is the
/// state the replay to the slice's start ended in: its sources and knobs are the session's
/// start; a deck that was playing gets a `play` at frame 0; then the slice's actions and
/// MIDI, rebased. A session starts every deck at its beginning with an empty loop row, so
/// playheads and clips already on the row at `from` are not in it. `tail`: seconds kept
/// after the last event.
pub fn export_session(slice: &ExportSlice, at: &JournalState, tail: f64) -> Session {
    let mut events: Vec<SessionEvent> = Vec::new();
    let defaults = default_loop_settings();
    for a in &at.loop_settings {
        if !defaults.contains(a) {
            events.push(SessionEvent::action(0, a.clone()));
        }
    }
    for k in &at.locked {
        events.push(SessionEvent::action(
            0,
            DeckAction::Lock {
                key: k.clone(),
                on: true,
            },
        ));
    }
    for (name, d) in &at.decks {
        if d.playing {
            events.push(SessionEvent::action(
                0,
                DeckAction::Play {
                    deck: name.clone(),
                    on: true,
                },
            ));
        }
    }
    for e in &slice.slice {
        let f = e.frame - slice.start_frame;
        match &e.event {
            JournalEvent::Action(a) => events.push(SessionEvent::action(f, a.clone())),
            JournalEvent::Midi { port, bytes } => events.push(SessionEvent {
                frame: f,
                kind: SessionEventKind::Midi {
                    port: port.clone(),
                    bytes: bytes.clone(),
                },
            }),
            _ => {}
        }
    }
    let params = at
        .decks
        .iter()
        .map(|(k, d)| (k.clone(), d.params.clone()))
        .collect();
    let decks = at
        .decks
        .iter()
        .map(|(k, d)| (k.clone(), d.source.clone()))
        .collect();
    let mut s = Session::new(
        at.sample_rate,
        slice.last_frame - slice.start_frame + libm::trunc(tail * at.sample_rate) as i64,
        decks,
        params,
        at.crossfader,
        at.master,
        events,
    );
    s.auto_mix = at.auto_mix.start();
    s
}

/// A short description of what a replay could not rebuild, for `djctl journal replay`.
pub fn describe_warnings(w: &[String]) -> String {
    w.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
