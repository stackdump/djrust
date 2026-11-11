//! Replaying what was recorded, through the engine: `SessionPlayer.render` (a [`Session`] to
//! audio) and `Journal.replay` / `Journal.exportSession` (a journal to a state, a slice of one to a
//! session). The schedules (which events are due before each block, where a replay starts, what
//! a slice exports) are `crate::session::player` and `crate::journal::replay`; this is the
//! engine they drive: an offline engine built the way Swift builds one
//! (`DJEngine(decks:offline:sampleRate:)`, `deterministic`), stood on the session's or the
//! checkpoint's state, and stepped by an [`OfflineConductor`].
//!
//! **Determinism.** A replay is deterministic per engine: two renders of one session are
//! byte-identical, and a live run recorded by a `SessionRecorder` or a journal replays to the
//! same samples (`SessionReplayTests`, `JournalReplayTests`). The tests hold this Rust-to-Rust;
//! audio is not byte-comparable with Swift's (PORTING.md H).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::deck::clips::LoopClip;
use crate::deck::turntable::SampleBuffer;
use crate::journal::event::{clip_ids, JournalEvent};
use crate::journal::file::Contents;
use crate::journal::replay::{
    self as journal_replay, JournalError, ReplayEngine, ReplayOptions,
};
use crate::journal::state::JournalState;
use crate::journal::stores::JournalStoreState;
use crate::session::action::DeckAction;
use crate::session::model::{DeckSource, Session, SessionError, DEMO_ID};
use crate::session::player::{Conductor, SessionPlayer};
use crate::session::wav::RenderedAudio;
use crate::wander::WanderMove;
use crate::wave::RoleFilter;

use crate::host::deck::{engine, EngineConfig, OfflineConductor, PerformCtx};

/// What resolves a sample's content id to audio (`SessionPlayer.samples`; the demo loop is
/// built in).
pub type SampleLookup = Arc<dyn Fn(&str) -> Option<Arc<SampleBuffer>> + Send + Sync>;

/// `SessionPlayer`'s settings.
#[derive(Clone, Default)]
pub struct RenderOptions {
    /// Roles left out of every net deck's render (stems for listening tests). Applied when a
    /// deck compiles, so the session, and its CID, are untouched (`SessionPlayer.roleFilter`).
    pub role_filter: Option<RoleFilter>,
    /// `SessionPlayer.samples`.
    pub samples: Option<SampleLookup>,
}

impl RenderOptions {
    pub fn with_samples(
        mut self,
        f: impl Fn(&str) -> Option<Arc<SampleBuffer>> + Send + Sync + 'static,
    ) -> Self {
        self.samples = Some(Arc::new(f));
        self
    }
}

/// What [`render_session`] made: the audio and, per deck, the walk the replay took
/// (`SessionPlayer.wanderLogs`).
pub struct RenderedSession {
    pub audio: RenderedAudio,
    /// Commands the engine's ring had no room for (0 unless the session is more than a block's
    /// inputs can carry): a render that lost one is not the set it renders.
    pub dropped: u64,
    pub wander_logs: std::collections::BTreeMap<String, Vec<WanderMove>>,
}

/// An offline engine for `decks` at `sample_rate`, replay mode: `DJEngine(decks:offline:
/// sampleRate:)` with `deterministic = true`.
pub fn offline_conductor(decks: Vec<String>, sample_rate: f64) -> OfflineConductor {
    offline_conductor_with_inputs(decks, sample_rate, Vec::new())
}

/// [`offline_conductor`] with input strips of those labels (a journal's `input` live ops name
/// them; with no audio fed they are silent).
pub fn offline_conductor_with_inputs(
    decks: Vec<String>,
    sample_rate: f64,
    inputs: Vec<String>,
) -> OfflineConductor {
    let mut c = EngineConfig::new(sample_rate);
    c.decks = decks;
    if !inputs.is_empty() {
        c.inputs = inputs;
        c.history_max_beats = crate::history::MAX_BEATS;
    }
    let (mut e, r) = engine(c);
    e.deterministic = true;
    OfflineConductor::new(e, r)
}

/// The engine `SessionPlayer.render` builds for `session`, at frame 0: the decks in name order,
/// deterministic, each loaded then set as the session starts, the crossfader and master, the
/// loop channel's trim and auto-mix, a missing sample refused before the first frame, a role
/// filter set on each deck's options. Not yet stepped.
pub fn session_engine(
    session: &Session,
    opts: &RenderOptions,
) -> Result<OfflineConductor, SessionError> {
    let mut decks: Vec<String> = session.decks.keys().cloned().collect();
    decks.sort();
    let mut c = offline_conductor(decks, session.sample_rate);
    let lookup = opts.samples.clone();
    let find = move |id: &str| lookup.as_ref().and_then(|f| f(id));
    {
        let find = find.clone();
        c.rig.engine.sample_resolver = Some(Box::new(move |id| find(id)));
    }
    // Every sample the set ever loads must be in hand before the first frame.
    SessionPlayer::new(session).check_samples(|id| find(id).is_some())?;
    if let Some(f) = opts.role_filter.as_ref().filter(|f| !f.is_empty()) {
        for name in session.decks.keys() {
            if let Some(d) = c.rig.engine.deck_mut(name) {
                d.opts.role_filter = Some(f.clone());
            }
        }
    }
    // Starting state: the decks in name order, each loaded then set.
    for (name, source) in &session.decks {
        c.rig.engine.apply_deck_action(
            &DeckAction::Load {
                deck: name.clone(),
                source: source.clone(),
            },
            PerformCtx::default(),
        );
        if let Some(p) = session.params.get(name) {
            c.rig.engine.apply_params(name, p);
        }
    }
    let e = &mut c.rig.engine;
    e.set_crossfader(session.crossfader);
    e.set_master(session.master);
    e.set_loop_gain(session.loop_gain);
    e.stand_auto_mix(&session.auto_mix);
    c.pump();
    Ok(c)
}

/// `SessionPlayer.render`: re-render `session` offline, the same engine with the same inputs at
/// the same frames ([`session_engine`], then each event applied before the block that finds it
/// due) until `session.frames` are rendered. `MissingSample` when a sample the set loads is not
/// in hand before the first frame.
pub fn render_session(
    session: &Session,
    opts: &RenderOptions,
) -> Result<RenderedSession, SessionError> {
    let mut c = session_engine(session, opts)?;
    let audio = SessionPlayer::new(session).drive(&mut c);
    let wander_logs = c
        .rig
        .engine
        .decks
        .iter()
        .map(|d| (d.name.clone(), d.walk.wander.log.clone()))
        .collect();
    Ok(RenderedSession {
        audio,
        dropped: c.dropped(),
        wander_logs,
    })
}

// MARK: Journal.replay

/// What finds a loop clip's audio by id (`Journal.replay`'s `clips`: the loop library, the row
/// first and then `removed/`).
pub type ClipLookup = Arc<dyn Fn(&str) -> Option<LoopClip> + Send + Sync>;

/// `Journal.replay`'s arguments.
#[derive(Clone, Default)]
pub struct JournalReplayOptions {
    /// Stop after this sequence number (all with None).
    pub to_seq: Option<i64>,
    /// Render at least to this many frames after line 0 (`--at SECONDS` x the rate).
    pub end_frame: Option<i64>,
    /// Keep the audio.
    pub render: bool,
    /// Start at line 0 rather than the nearest checkpoint: audio that is byte-identical to the
    /// live run's.
    pub from_start: bool,
    /// Keep where each deck ended up in the result's state (`DeckCheckpoint.position`: what a
    /// rewind stands the live decks at).
    pub playheads: bool,
    /// `samples`: a sample's content id to audio (the `sampleFile` events' paths are tried too).
    pub samples: Option<SampleLookup>,
    /// `clips`.
    pub clips: Option<ClipLookup>,
}

impl JournalReplayOptions {
    pub fn with_samples(
        mut self,
        f: impl Fn(&str) -> Option<Arc<SampleBuffer>> + Send + Sync + 'static,
    ) -> Self {
        self.samples = Some(Arc::new(f));
        self
    }

    pub fn with_clips(
        mut self,
        f: impl Fn(&str) -> Option<LoopClip> + Send + Sync + 'static,
    ) -> Self {
        self.clips = Some(Arc::new(f));
        self
    }
}

/// `JournalReplayResult`: what a replay rebuilt.
#[derive(Clone, Debug)]
pub struct JournalReplayResult {
    /// The state at the end (`JournalState(engine:stores:)` of the rebuilt engine).
    pub state: JournalState,
    /// The checkpoint the replay started from (0: line 0; else a mid-file one).
    pub start_seq: i64,
    /// The last entry applied (the start checkpoint's if none).
    pub last_seq: i64,
    /// Engine frames from line 0 to where the replay stopped (a replay from a mid checkpoint
    /// counts the frames before it too, though it didn't render them).
    pub frames: i64,
    /// The audio, when asked for: from the start checkpoint on.
    pub audio: Option<RenderedAudio>,
    /// What could not be rebuilt (a clip or sample whose audio is gone).
    pub warnings: Vec<String>,
}

impl JournalReplayResult {
    /// The stores as rebuilt.
    pub fn stores(&self) -> &JournalStoreState {
        &self.state.stores
    }
}

/// The engine a journal replays into, behind `djengine`'s [`ReplayEngine`]: `Journal.apply` and
/// the conductor of `Journal.replay`.
struct JournalDriver {
    c: OfflineConductor,
    samples: Option<SampleLookup>,
    clips: Option<ClipLookup>,
    /// `sampleFile` events: a content id's file, to decode when nothing else has the audio.
    paths: BTreeMap<String, String>,
    sample_rate: f64,
}

impl JournalDriver {
    fn find_clip(&self, id: &str) -> Option<LoopClip> {
        self.clips.as_ref().and_then(|f| f(id))
    }

    /// `dj.sampleResolver?(id)` of a replay: the engine's library, `samples`, then the
    /// journal's own record of the file the audio came from, decoded at the engine's rate and
    /// kept in the library under the journal's id (a Rust-decoded file need never hash to the
    /// content id the Swift app named it by).
    fn resolve(&mut self, id: &str) -> Option<Arc<SampleBuffer>> {
        let e = &mut self.c.rig.engine;
        if let Some(b) = e.loops.row.sample_library.get(id) {
            return Some(b.clone());
        }
        if let Some(b) = self.samples.as_ref().and_then(|f| f(id)) {
            return Some(b);
        }
        let path = self.paths.get(id)?;
        let b = Arc::new(crate::host::decode::decode_file(Path::new(path), self.sample_rate).ok()?);
        e.loops.row.sample_library.insert(id.to_string(), b.clone());
        Some(b)
    }
}

impl ReplayEngine for JournalDriver {
    fn frame(&self) -> i64 {
        self.c.rig.engine.frame()
    }

    /// `Journal.apply`: one event that is not about the stores.
    fn apply(&mut self, event: &JournalEvent, warnings: &mut Vec<String>) {
        match event {
            JournalEvent::Action(a) => {
                // A clip this replay didn't make (a live take that rendered differently) comes
                // from the library, so the moves on it still land.
                for id in clip_ids(a) {
                    let e = &self.c.rig.engine;
                    let removed = e.loops.row.removed.iter().any(|r| r.clip.id == id);
                    if e.clip(&id).is_none() && !removed {
                        if let Some(clip) = self.find_clip(&id) {
                            self.c.rig.engine.restore_clips(&[clip]);
                        }
                    }
                }
                let mut ctx = PerformCtx::default();
                match a {
                    // an import's audio comes by content id: the library, `samples`, or a
                    // `sampleFile` path
                    DeckAction::ClipImport { id, title, .. } => {
                        let on_row = self.c.rig.engine.clip(id).is_some();
                        ctx.buffer = self.resolve(id);
                        if !on_row && ctx.buffer.is_none() {
                            let what = if title.is_empty() { id } else { title };
                            warnings.push(format!("imported clip {what} has no audio"));
                        }
                    }
                    DeckAction::Load {
                        source: DeckSource::Sample { id, .. },
                        ..
                    } if id != DEMO_ID => ctx.buffer = self.resolve(id),
                    _ => {}
                }
                self.c.rig.engine.apply_deck_action(a, ctx);
            }
            JournalEvent::Midi { port, bytes } => self.c.route_midi(port, bytes),
            // a window or socket move that is no `DeckAction`: the engine makes it again
            JournalEvent::Live { method, params } => {
                if let Err(why) = self.c.rig.engine.apply_live_op(method, params) {
                    warnings.push(format!("live {method}: {why}"));
                }
            }
            JournalEvent::LibraryRestore { id, index } => {
                let found = self.find_clip(id);
                match found {
                    Some(c) => self.c.rig.engine.remember_removed_clip(c, *index),
                    None => warnings.push(format!("library clip {id} has no audio")),
                }
            }
            // a checkpoint, a sampleFile and a mark say nothing a replay applies
            _ => {}
        }
    }

    fn step(&mut self) -> (Vec<f32>, Vec<f32>) {
        Conductor::step(&mut self.c)
    }
}

/// `Journal.replay`: replay `contents` into a fresh offline engine: the checkpoint, then the
/// events in order at their frames, up to sequence number `to_seq`, rendering at least to
/// `end_frame` frames after line 0. It starts from the last checkpoint inside those bounds, or
/// line 0 with `from_start`; a rewind's checkpoint is as far back as it goes. Deterministic: two
/// replays of one file are the same state and the same audio.
pub fn replay(
    contents: &Contents,
    o: &JournalReplayOptions,
) -> Result<JournalReplayResult, JournalError> {
    let plan = journal_replay::plan(
        contents,
        &ReplayOptions {
            to_seq: o.to_seq,
            end_frame: o.end_frame,
            render: o.render,
            from_start: o.from_start,
        },
    )?;
    let state = plan.state.clone();
    // `DJEngine(decks: state.decks.keys.sorted(), offline: true, sampleRate:)`, deterministic
    // the input strips the run's live ops name (a strip the checkpoint knows nothing of)
    let mut labels: Vec<String> = Vec::new();
    for e in &plan.events {
        if let JournalEvent::Live { method, params } = &e.event {
            crate::host::deck::live_moves::input_labels(method, params, &mut labels);
        }
    }
    let c = offline_conductor_with_inputs(
        state.decks.keys().cloned().collect(),
        state.sample_rate,
        labels,
    );
    let mut d = JournalDriver {
        c,
        samples: o.samples.clone(),
        clips: o.clips.clone(),
        paths: plan.sample_paths.iter().cloned().collect(),
        sample_rate: state.sample_rate,
    };
    let lookup = o.samples.clone();
    d.c.rig.engine.sample_resolver = Some(Box::new(move |id| lookup.as_ref().and_then(|f| f(id))));
    let mut warnings = Vec::new();
    for id in state.sample_ids() {
        if d.resolve(&id).is_none() {
            warnings.push(format!("sample {id} not found"));
        }
    }
    let find = {
        let clips = o.clips.clone();
        move |id: &str| clips.as_ref().and_then(|f| f(id))
    };
    warnings.extend(d.c.restore_state(&state, &find));
    let mut out = journal_replay::run(&plan, &mut d, o.render, state.sample_rate, warnings);
    let end =
        d.c.rig
            .engine
            .journal_state(out.stores.clone(), o.playheads);
    // Swift's calls are immediate; here they are messages on a 512-deep ring.
    // A replay that lost one is not the run it replays, and says so.
    if d.c.dropped() > 0 {
        out.warnings.push(format!(
            "{} commands did not fit the engine's ring: the replay is not faithful",
            d.c.dropped()
        ));
    }
    Ok(JournalReplayResult {
        state: end,
        start_seq: out.start_seq,
        last_seq: out.last_seq,
        frames: out.frames,
        audio: out.audio,
        warnings: out.warnings,
    })
}

/// `Journal.exportSession`: the slice `from..=to` (sequence numbers) as a normal [`Session`]:
/// the state at `from` (replayed) is its start (the decks' sources and knobs; a deck that was
/// playing gets a `play` at frame 0), then the slice's actions and MIDI, rebased. A session
/// starts every deck at its beginning with an empty loop row, so playheads and clips already on
/// the row at `from` are not in it. `tail`: seconds kept after the last event.
pub fn export_session_from(
    contents: &Contents,
    from: i64,
    to: i64,
    tail: f64,
    samples: Option<SampleLookup>,
    clips: Option<ClipLookup>,
) -> Result<Session, JournalError> {
    let slice = journal_replay::export_slice(contents, from, to)?;
    let at = replay(
        contents,
        &JournalReplayOptions {
            to_seq: Some(slice.replay_to_seq),
            end_frame: Some(slice.replay_end_frame),
            samples,
            clips,
            ..Default::default()
        },
    )?
    .state;
    Ok(journal_replay::export_session(&slice, &at, tail))
}
