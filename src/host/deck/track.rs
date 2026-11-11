//! The track pipeline (`DeckGenerator.swift`): a track composed and compiled off the audio
//! thread ([`PreparedTrack`]), the deck calls that load one (`generate`, `swap`, `stepSeed`,
//! `queueNext`, `advanceToNext`, `service`), and the slots a track waits in.
//!
//! **Threads.** Swift composes in `Task.detached` and installs back on the main actor. Here a
//! compose runs on a worker thread ([`Task`]) from a [`Preparer`], a `Send` snapshot of what
//! [`Engine::prepare_with`] reads (the sample rate, the deck's options and role filter, its
//! sampler bank), and what it makes waits for [`Engine::service`], which the host calls about
//! every 10 ms (`Rig::tick` does): that is where Swift's `await` resumes, up to one tick later.
//! `deterministic` ([`Engine::deterministic`]) composes inline instead, as Swift's replay does;
//! [`Engine::generate_at`] and [`Engine::settle`] block until the work is in, as `await` does.
//! A host with no threads to spare (djweb: the engine lives on the audio thread) can offload
//! those composes to another engine instance instead (feature `transfer`, [`super::offload`]):
//! each becomes a request the host carries to it and a [`Task`] the host finishes with the
//! answer, and the deck lands it as it would a thread's.
//!
//! **What stays pure**: where a track starts and which options another start of the song takes
//! are `crate::deck::swap`; the three slots' bookkeeping is `crate::deck::slots`.
//!
//! **What a track is prepared with** is what Swift's `PreparedTrack.init` does: the deck's
//! options (`Deck.waveOptions`, [`WaveOptions::extended_with`] over the deck's bank, the track's
//! seed and the deck's role filter) hold the fixes the pass runs on the composed project
//! (`musicality::apply`, then the stingers curated for the genre and palette), and are then
//! `for_genre` and `for_palette` for the settings the runner plays it with. A raw-`nets` share
//! gets no pass, the runner's own fixes, and its genre's settings when it names one.
//!
//! What a new track clears on the deck (`Deck.loadNet`) is in [`Engine::load_net`] and
//! [`Engine::hand_over_with`]: the channels' voice settings (kept for the same song,
//! `keepsVoiceSettings`), the layers (`resetLayers`) and the STEMS mutes.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::deck::slots::{LookAheadKey, LookAheadSlot, NextTrackSlot, SwapStart};
use crate::deck::swap::{self, Latest};
use crate::json::JsonValue;
use crate::musicality::{self, Fixes};
use crate::pflow::Project;
use crate::session::model::DeckSource;
use crate::share::Share;
use crate::sound::stinger::StingerContent;
use crate::sound::{apply_project_sound, PaletteSet, Sounds};
use crate::spec::TrackSpec;
use crate::wander::deck::{curate_project_stingers, resolved_project_stingers};
use crate::wave::net::str_hash;
use crate::wave::{RoleFilter, StingerPolicy, WaveOptions, WaveRunner};

use super::core::DeckMirror;
use super::engine::{Engine, Prepared, SectionInfo, ROLE_ORDER};
#[cfg(feature = "sf2")]
use super::samplers::{bind_thunk, part_level_thunk, Binding, HostSlots, SamplerBank};

// MARK: Task

enum TaskState<T> {
    Running,
    Done(T),
    Failed,
    Taken,
}

struct TaskInner<T> {
    state: Mutex<TaskState<T>>,
    done: Condvar,
    /// Finished by the host ([`Fulfil`]: a compose offloaded to another engine instance), not
    /// by a thread of this process: never waited for.
    remote: bool,
}

/// A compose running on a worker thread (Swift's `Task<PreparedTrack, Never>`): poll it with
/// [`try_take`](Task::try_take), or block on [`wait`](Task::wait).
pub struct Task<T> {
    inner: Arc<TaskInner<T>>,
}

impl<T: Send + 'static> Task<T> {
    /// Run `f` on a new thread.
    pub fn spawn(f: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let inner = Arc::new(TaskInner {
            state: Mutex::new(TaskState::Running),
            done: Condvar::new(),
            remote: false,
        });
        let theirs = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("djhost-compose".into())
            .spawn(move || {
                let r = catch_unwind(AssertUnwindSafe(f));
                let mut g = theirs.state.lock().unwrap_or_else(PoisonError::into_inner);
                *g = match r {
                    Ok(v) => TaskState::Done(v),
                    Err(_) => TaskState::Failed,
                };
                theirs.done.notify_all();
            });
        if spawned.is_err() {
            *inner.state.lock().unwrap_or_else(PoisonError::into_inner) = TaskState::Failed;
        }
        Task { inner }
    }

    /// Run `f` here, now (replay mode): a task that is finished already.
    pub fn run(f: impl FnOnce() -> T) -> Task<T> {
        Task::immediate(f())
    }

    /// A task that is finished already, with this value.
    pub fn immediate(v: T) -> Task<T> {
        Task {
            inner: Arc::new(TaskInner {
                state: Mutex::new(TaskState::Done(v)),
                done: Condvar::new(),
                remote: false,
            }),
        }
    }

    /// A task nothing here runs: the host finishes it through the [`Fulfil`] (an offloaded
    /// compose, [`super::offload`]). Dropping the `Fulfil` unfinished fails it.
    #[cfg_attr(not(feature = "transfer"), allow(dead_code))]
    pub(crate) fn remote() -> (Task<T>, Fulfil<T>) {
        let inner = Arc::new(TaskInner {
            state: Mutex::new(TaskState::Running),
            done: Condvar::new(),
            remote: true,
        });
        (Task { inner: inner.clone() }, Fulfil { inner: Some(inner) })
    }

    fn lock(&self) -> MutexGuard<'_, TaskState<T>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Is it finished by the host ([`remote`](Self::remote)), not by a thread here?
    pub(crate) fn is_remote(&self) -> bool {
        self.inner.remote
    }

    /// Has the work finished (or failed)?
    pub fn is_finished(&self) -> bool {
        !matches!(*self.lock(), TaskState::Running)
    }

    /// The value, if the work has finished and it has not been taken.
    pub fn try_take(&self) -> Option<T> {
        let mut g = self.lock();
        match std::mem::replace(&mut *g, TaskState::Taken) {
            TaskState::Done(v) => Some(v),
            TaskState::Running => {
                *g = TaskState::Running;
                None
            }
            other => {
                *g = other;
                None
            }
        }
    }

    /// Block until the work has finished, and take its value; None if the work panicked (the
    /// panic's message went to stderr from its thread) or the value was taken already.
    ///
    /// A [`remote`](Self::remote) task still out with the host is not waited for (its answer
    /// comes back through this thread, which would be the one waiting): None, as for a compose
    /// that failed.
    pub fn wait_ok(&self) -> Option<T> {
        let mut g = self.lock();
        if self.inner.remote && matches!(*g, TaskState::Running) {
            return None;
        }
        while matches!(*g, TaskState::Running) {
            g = self
                .inner
                .done
                .wait(g)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match std::mem::replace(&mut *g, TaskState::Taken) {
            TaskState::Done(v) => Some(v),
            _ => None,
        }
    }

    /// [`wait_ok`](Self::wait_ok), panicking where it would be None.
    pub fn wait(&self) -> T {
        self.wait_ok()
            .expect("a compose thread panicked, or its value was taken already")
    }
}

/// The host's end of a [`Task::remote`]: finish it with the work's value, or drop it to fail it.
pub(crate) struct Fulfil<T> {
    inner: Option<Arc<TaskInner<T>>>,
}

#[cfg_attr(not(feature = "transfer"), allow(dead_code))]
impl<T> Fulfil<T> {
    /// The work is in: the task holds `v` now.
    pub(crate) fn done(mut self, v: T) {
        if let Some(i) = self.inner.take() {
            *i.state.lock().unwrap_or_else(PoisonError::into_inner) = TaskState::Done(v);
            i.done.notify_all();
        }
    }

    /// Does anything still hold the task? (A compose the deck superseded and dropped need not
    /// be prepared or installed.)
    pub(crate) fn wanted(&self) -> bool {
        self.inner.as_ref().is_some_and(|i| Arc::strong_count(i) > 1)
    }
}

impl<T> Drop for Fulfil<T> {
    fn drop(&mut self) {
        if let Some(i) = self.inner.take() {
            let mut g = i.state.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*g, TaskState::Running) {
                *g = TaskState::Failed;
            }
            drop(g);
            i.done.notify_all();
        }
    }
}

// MARK: Preparer

/// What [`Engine::prepare_with`] reads, as a value that crosses threads: a deck's compile
/// options and role filter, its sampler bank and the installed sounds. A compose on a worker
/// thread takes one, as Swift's detached task captures `wave.samplers` and `roleFilter`.
#[derive(Clone)]
pub struct Preparer {
    pub sample_rate: f64,
    /// The deck's options (`Deck.waveOptions` without the seed): the fixes in `musicality` are
    /// the pass a composed track gets (`DJMusicality.apply`) and the runner's own, `stingers`
    /// what its Hit 1-4 are curated from.
    pub opts: WaveOptions,
    /// The roles left out of every net it compiles (`Deck.roleFilter`).
    pub role_filter: Option<RoleFilter>,
    /// The deck's mirror, for the tick a `SwapStart.nextBar` reads on the compose thread.
    pub mirror: Arc<DeckMirror>,
    gate: Arc<Mutex<()>>,
    #[cfg(feature = "sf2")]
    bank: Option<Arc<Mutex<SamplerBank>>>,
    #[cfg(feature = "sf2")]
    library: Option<Arc<crate::host::sampler::Library>>,
    /// What the host says is installed (`Engine::installed`); first choice over `library`.
    installed: Option<Arc<super::wander::InstalledSounds>>,
}

impl Preparer {
    /// A snapshot for a render that has no deck: `opts` (the app's: `WaveOptions::extended()`)
    /// over a sampler bank of its own, opened on `library` (none: every instrument on a synth
    /// voice), with the sounds the host says are `installed` (else the library's, else none). What
    /// `ClipRetune.render` builds with `WaveDeckSource(sampleRate:, opts: .extended(samplers:
    /// nil))`, which makes the bank it plays through.
    pub(crate) fn standalone(
        sample_rate: f64,
        opts: WaveOptions,
        installed: Option<Arc<super::wander::InstalledSounds>>,
        #[cfg(feature = "sf2")] library: Option<Arc<crate::host::sampler::Library>>,
    ) -> Preparer {
        Preparer {
            sample_rate,
            opts,
            role_filter: None,
            mirror: Arc::new(DeckMirror::new()),
            gate: Arc::new(Mutex::new(())),
            #[cfg(feature = "sf2")]
            bank: library
                .clone()
                .map(|lib| Arc::new(Mutex::new(SamplerBank::new(lib, sample_rate as u32)))),
            #[cfg(feature = "sf2")]
            library,
            installed,
        }
    }

    /// A snapshot that holds nothing of a process (no sampler bank, no installed sounds): what
    /// another engine instance composes an offloaded request with ([`super::offload`]).
    #[cfg(feature = "transfer")]
    pub(crate) fn bare(sample_rate: f64, opts: WaveOptions, role_filter: Option<RoleFilter>) -> Preparer {
        Preparer {
            sample_rate,
            opts,
            role_filter,
            mirror: Arc::new(DeckMirror::new()),
            gate: Arc::new(Mutex::new(())),
            #[cfg(feature = "sf2")]
            bank: None,
            #[cfg(feature = "sf2")]
            library: None,
            installed: None,
        }
    }

    /// Does it hold nothing of this process ([`bare`](Self::bare)), so it can travel?
    #[cfg(feature = "transfer")]
    pub(crate) fn is_bare(&self) -> bool {
        #[cfg(feature = "sf2")]
        if self.bank.is_some() || self.library.is_some() {
            return false;
        }
        self.installed.is_none()
    }

    /// The sampler bank this snapshot binds through (feature `sf2`): the slots a track bound and
    /// what each measured on the part probes (`SamplerBank::slot_part_level_db`).
    #[cfg(feature = "sf2")]
    pub fn sampler_bank(&self) -> Option<Arc<Mutex<SamplerBank>>> {
        self.bank.clone()
    }

    /// The samplers this snapshot's bank has loaded and not sent: for a host that plays what it
    /// prepared on a core of its own (an offline render) and must send them all
    /// ([`SamplerBank::take_ready`]). A deck's engine sends them when it is handed a project.
    #[cfg(feature = "sf2")]
    pub fn take_samplers(&self) -> Vec<(usize, crate::host::sampler::Sf2Sampler)> {
        self.bank
            .as_ref()
            .map(|b| b.lock().unwrap().take_ready())
            .unwrap_or_default()
    }

    /// The deck's options with the track's voicing seed and the role filter
    /// (`.extended(samplers:seed:roleFilter:)`).
    pub fn options(&self, seed: f64) -> WaveOptions {
        let mut o = self.opts.clone();
        o.voicing_seed = seed;
        o.role_filter = self.role_filter.clone();
        o
    }

    /// This snapshot with another role filter (`.extended(samplers:roleFilter:)`: Wander's
    /// look-ahead composes with the filter its key names).
    pub fn with_role_filter(mut self, filter: Option<RoleFilter>) -> Preparer {
        self.role_filter = filter;
        self
    }

    /// `TrackSpec.project()` with the sounds installed here (the palettes' picks), or none
    /// when the host has no library.
    pub fn project_of(&self, spec: &TrackSpec) -> Project {
        if let Some(i) = &self.installed {
            return spec.project_in(i.sounds());
        }
        #[cfg(feature = "sf2")]
        if let Some(lib) = &self.library {
            return spec.project_in(lib.sounds());
        }
        spec.project()
    }

    /// What is installed here, which a palette resolves its sounds against: what the host says
    /// ([`Engine::installed`]), else the scanned library (feature `sf2`), else nothing.
    fn sounds(&self) -> Sounds<'_> {
        if let Some(i) = &self.installed {
            return i.sounds();
        }
        #[cfg(feature = "sf2")]
        if let Some(lib) = &self.library {
            return lib.sounds();
        }
        Sounds::nothing()
    }

    /// `Project.applySound(seed:)` against what is installed here.
    fn apply_sound(&self, project: &mut Project, seed: f64) {
        apply_project_sound(project, seed, PaletteSet::shared(), self.sounds());
    }

    /// `p.curateStingers(p.resolvedStingers(genre:seed:installed:), seed:)`: Hit 1-4 curated for
    /// the genre and palette from `policy` (`WaveOptions.stingers`), on what plays and never on
    /// the composition. `Here` is what is installed here (no Apple Loops: no stock loops).
    pub fn curate_stingers(
        &self,
        played: &mut Project,
        genre: &str,
        seed: f64,
        policy: StingerPolicy,
    ) {
        let set = PaletteSet::shared();
        let ctx = self.sounds();
        let content = match policy {
            StingerPolicy::Here => StingerContent::here(set, ctx, Vec::new()),
            StingerPolicy::Everything => StingerContent::everything(),
            StingerPolicy::Nothing => StingerContent::nothing(),
        };
        let hits = resolved_project_stingers(played, set, genre, seed, &content);
        curate_project_stingers(played, hits, seed, set, ctx);
    }

    /// `WaveRunner.prepare` for `deck`: compile `project` from `start_tick`, binding sampled
    /// instruments through the deck's bank, and read what the deck will want to know of the
    /// result. `opts` are the full options (see [`options`](Self::options)); `track_serial`
    /// is the song's when this is another start of it. One prepare at a time per deck, so
    /// the samplers a prepare binds are the ones it takes.
    pub fn prepare_opts(
        &self,
        project: &Project,
        start_tick: i64,
        #[allow(unused_mut)] mut opts: WaveOptions,
        track_serial: Option<i32>,
    ) -> Prepared {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(feature = "sf2")]
        let mut slots: Option<std::sync::Arc<HostSlots>> = None;
        #[cfg(feature = "sf2")]
        let _binding = self.bank.as_ref().map(|b| {
            b.lock().unwrap().seed = opts.voicing_seed;
            opts.sample_binder = Some(bind_thunk);
            // what each slot measured on the part probes, read back as the runner binds a lane
            // (`bank.slotPartLevelDB`): the `.balance` gain and the `.loudness` probe take the
            // sampled part's own level, not the palette's average
            opts.part_level = Some(part_level_thunk);
            opts.bank_rate = b.lock().unwrap().sample_rate() as f64;
            // (Swift binds through the bank only with the instrument table, `.extended`: beats'
            // parity engine with a library keeps binding by name alone)
            let host = std::sync::Arc::new(HostSlots::new(b.clone()));
            if opts.instruments.is_some() {
                opts.bank = Some(host.clone());
            }
            slots = Some(host);
            Binding::set(b.clone())
        });
        #[allow(unused_mut)]
        let mut wave =
            WaveRunner::prepare_serial(project, self.sample_rate, &opts, start_tick, track_serial);
        // the lanes bound by name (no instrument table) took their references through the bind
        // thunk, which has no handle to give them back by: the project gets the bank's
        #[cfg(feature = "sf2")]
        if let Some(host) = slots {
            wave.hold_slots_of(host);
        }
        let mut roles = Vec::new();
        for r in ROLE_ORDER {
            if let Some(ix) = wave.graph.role_net_indices(str_hash(r)) {
                roles.push((r.to_string(), ix));
            }
        }
        // the rings a snapshot shows: those roles and the four hits, each with the nets it may
        // be (`Deck.netSnapshot`)
        let mut ring_roles = Vec::new();
        for r in ROLE_ORDER.iter().chain(&["hit1", "hit2", "hit3", "hit4"]) {
            if let Some(ix) = wave.graph.role_net_indices(str_hash(r)) {
                let nets = ix
                    .into_iter()
                    .map(|i| (i, wave.graph.nets[i].id.clone()))
                    .collect();
                ring_roles.push((r.to_string(), nets));
            }
        }
        let latest = Latest::of(&wave, &opts.voice_design);
        Prepared {
            project: wave,
            source: project.clone(),
            roles,
            ring_roles,
            sections: SectionInfo::sections_of(project),
            latest,
        }
    }
}

// MARK: PreparedTrack

/// What a track was made from (`DeckSourceDescriptor`: `.net(spec)` or `.rawNets(payload:)`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum TrackSource {
    Net(TrackSpec),
    /// A raw-`nets` beats share, the envelope payload as given.
    RawNets(JsonValue),
}

/// A track composed (or, for a raw-`nets` share, built) and compiled off the audio thread,
/// ready to hand to a deck (`PreparedTrack`).
pub struct PreparedTrack {
    pub source: TrackSource,
    pub project: Project,
    /// What the deck plays: `project` after dj's musicality pass, or `project` itself for a
    /// raw-`nets` share or a deck without the pass.
    pub played: Project,
    pub prepared: Prepared,
    /// What `prepared` was prepared with: the deck's options with the track's voicing seed and
    /// its genre's and palette's settings applied.
    pub opts: WaveOptions,
    /// The fixes the deck plays this track with: all the deck asks for on a composed track,
    /// [`Fixes::RUNNER_ONLY`] of them on a raw-`nets` share (`PreparedTrack.fixes`; the pass's,
    /// since the runner's own options carry theirs).
    pub fixes: Fixes,
    /// Whether dj's pass rewrote `played`: false for a raw-`nets` share and for a deck without
    /// the pass.
    pub pass_applied: bool,
}

impl PreparedTrack {
    /// The composed track's spec; None for a raw-`nets` share.
    pub fn spec(&self) -> Option<&TrackSpec> {
        match &self.source {
            TrackSource::Net(s) => Some(s),
            TrackSource::RawNets(_) => None,
        }
    }

    /// `PreparedTrack.init(spec:sampleRate:opts:startTick:)`: compose `spec` and compile it.
    /// `start_tick`: where the net starts (its control nets replayed to there), so a song
    /// swapped in mid-song keeps the transport where it is instead of going back to its first
    /// bar. A tick at or past the song's end, or any tick on a loop (no structure), starts
    /// at 0.
    pub fn compose(spec: &TrackSpec, preparer: &Preparer, start_tick: i64) -> PreparedTrack {
        let project = preparer.project_of(spec);
        let start = swap::start_for(start_tick, swap::song_length(&project));
        Self::build(
            TrackSource::Net(spec.clone()),
            project,
            spec.seed,
            preparer,
            start,
        )
    }

    /// `PreparedTrack.init?(rawNets:sampleRate:opts:)`: a raw-`nets` share envelope's project
    /// (`Share.rawNetsProject()`, named by the payload's share CID as a link to it would be).
    /// The payload is read in canonical key order, the order a share link's payload decodes in,
    /// so a payload handed over in any order builds the same project. None when it has no
    /// `nets`.
    pub fn from_raw_nets(payload: &JsonValue, preparer: &Preparer) -> Option<PreparedTrack> {
        let canonical = payload.canonical_js();
        let ordered = JsonValue::parse(&canonical).unwrap_or_else(|_| payload.clone());
        let share = Share::from_payload(
            &ordered,
            Some(&crate::cid::share_cid_canonical(&canonical)),
        );
        let mut project = share.raw_nets_project()?;
        let seed = DeckSource::voicing_seed(payload);
        preparer.apply_sound(&mut project, seed);
        Some(Self::build(
            TrackSource::RawNets(payload.clone()),
            project,
            seed,
            preparer,
            0,
        ))
    }

    /// `PreparedTrack.init?(source:...)`: either kind of net source.
    pub fn from_source(
        source: &TrackSource,
        preparer: &Preparer,
        start_tick: i64,
    ) -> Option<PreparedTrack> {
        match source {
            TrackSource::Net(spec) => Some(Self::compose(spec, preparer, start_tick)),
            TrackSource::RawNets(payload) => Self::from_raw_nets(payload, preparer),
        }
    }

    /// A raw-`nets` payload's `genre`, when it is one of beats' genres (`forGenre`'s settings).
    fn raw_genre(payload: &JsonValue) -> Option<String> {
        let g = payload.get("genre")?.as_str()?;
        crate::gen::tables::BeatsTables::shared()
            .genres
            .contains_key(g)
            .then(|| g.to_string())
    }

    fn build(
        source: TrackSource,
        project: Project,
        seed: f64,
        preparer: &Preparer,
        start_tick: i64,
    ) -> PreparedTrack {
        let (played, pass_applied) = match &source {
            TrackSource::Net(spec) => {
                let o = preparer.options(seed);
                let mut p = if o.musicality.is_empty() {
                    project.clone()
                } else {
                    musicality::apply(&project, spec, o.musicality)
                };
                // Hit 1-4 curated for the genre and palette: on what plays, never on the
                // composition
                if let Some(policy) = o.stingers {
                    preparer.curate_stingers(&mut p, &spec.genre, seed, policy);
                }
                (p, !o.musicality.is_empty())
            }
            TrackSource::RawNets(_) => (project.clone(), false),
        };
        let o = Self::track_options(&source, swap::project_palette(&project).as_deref(), seed, preparer);
        let fixes = o.musicality;
        let prepared = preparer.prepare_opts(&played, start_tick, o.clone(), None);
        PreparedTrack {
            source,
            project,
            played,
            prepared,
            opts: o,
            fixes,
            pass_applied,
        }
    }

    /// The options a track of `source` (composed with `palette`, voiced with `seed`) is prepared
    /// with, before the runner measures anything: the deck's, with the genre's and the palette's
    /// settings; a raw-`nets` share keeps only the runner's own fixes, and its payload's genre's
    /// settings (neutral ones without a genre this app knows).
    pub(crate) fn track_options(source: &TrackSource, palette: Option<&str>, seed: f64, preparer: &Preparer) -> WaveOptions {
        let mut o = preparer.options(seed);
        match source {
            TrackSource::Net(spec) => o
                .for_genre(&spec.genre)
                .for_palette(palette, &spec.genre),
            TrackSource::RawNets(payload) => {
                o.musicality = o.musicality & Fixes::RUNNER_ONLY;
                if let Some(genre) = Self::raw_genre(payload) {
                    o = o
                        .for_genre(&genre)
                        .for_palette(palette, &genre);
                }
                o
            }
        }
    }

    /// `PreparedTrack.restarted(atTick:)`: this track starting at `tick` instead (clamped as
    /// [`compose`](Self::compose) clamps): the same project and options, and the loudness,
    /// energy, duck depth, room, echo, part trims and rest schedule `prepared` measured (as
    /// `sameSongOptions` keeps them), so nothing is composed or measured again; only the net is
    /// replayed to the new tick. The song's serial is kept.
    ///
    /// (The samplers `self` bound wait in the bank: this prepare finds their slots numbered
    /// already and binds nothing, and the deck is sent them with whichever project it is handed
    /// first.)
    ///
    /// It reads the project the first prepare was made from (`prepared.source`, which is
    /// `played`), so a track that arrived as bytes ([`super::transfer`], no `played`) restarts
    /// too.
    pub fn restarted(&self, preparer: &Preparer, tick: i64) -> PreparedTrack {
        let (start, o, serial) = self.restart_args(tick);
        let prepared = preparer.prepare_opts(&self.prepared.source, start, o, Some(serial));
        self.with_prepared(prepared)
    }

    /// What [`restarted`](Self::restarted) prepares with: the start tick, the options and the
    /// song's serial.
    pub(crate) fn restart_args(&self, tick: i64) -> (i64, WaveOptions, i32) {
        let start = swap::start_for(tick, swap::song_length(&self.prepared.source));
        let o = swap::restarted_options(&self.opts, self.prepared.wave());
        (start, o, self.prepared.wave().track_serial)
    }

    /// This track with `prepared` (another start of it) in place of its own.
    pub(crate) fn with_prepared(&self, prepared: Prepared) -> PreparedTrack {
        PreparedTrack {
            source: self.source.clone(),
            project: self.project.clone(),
            played: self.played.clone(),
            prepared,
            opts: self.opts.clone(),
            fixes: self.fixes,
            pass_applied: self.pass_applied,
        }
    }
}

// MARK: relocation

/// A relocation's inputs (`Deck.MoveRequest`), read on the main thread: [`prepare`] it on any
/// thread (auto-mix prepares a mix-out a phrase ahead with it), then hand the result to
/// [`Engine::load_move`].
///
/// [`prepare`]: MoveRequest::prepare
pub struct MoveRequest {
    pub preparer: Preparer,
    pub project: Project,
    pub opts: WaveOptions,
    pub start_tick: i64,
    pub track_serial: Option<i32>,
}

impl MoveRequest {
    pub fn prepare(&self) -> Prepared {
        self.preparer.prepare_opts(
            &self.project,
            self.start_tick,
            self.opts.clone(),
            self.track_serial,
        )
    }
}

// MARK: deck state

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The compose is running; when it is in, check it is still wanted and still on time.
    Compose,
    /// It was late: running again from the bar after the deck's tick.
    Restart,
}

/// A `generate` whose result is not installed yet (`Deck.generate` suspended at its `await`).
struct PendingInstall {
    generation: u64,
    start: SwapStart,
    stage: Stage,
    task: Task<PreparedTrack>,
}

/// What became of a finished compose.
enum Install {
    /// Loaded (`true`) or refused (`false`): a newer request superseded it.
    Decided(bool),
    /// It was late and runs again.
    Again(PendingInstall),
}

/// A relocation composing in the background.
struct PendingMove {
    generation: u64,
    task: Task<Prepared>,
}

/// A look-ahead taken off its slot by the move that wanted it (`LookAheadSlot.Taken`): the
/// track if it had finished, else the compose to wait for.
pub struct TakenAhead {
    pub ready: Option<PreparedTrack>,
    task: Option<Task<PreparedTrack>>,
}

/// What a deck keeps for the track pipeline (the `Deck` fields of `DJEngine.swift` that
/// `DeckGenerator.swift` reads and writes).
#[derive(Default)]
pub struct TrackState {
    /// The raw-`nets` share payload the deck is playing, when it is playing one (then
    /// `MainDeck::spec` is None) (`currentRawNets`).
    pub raw_nets: Option<JsonValue>,
    /// The fixes the net on the deck plays with (`Deck.fixes`; None off a net).
    pub fixes: Option<Fixes>,
    /// Whether dj's pass rewrote the net (`passApplied`).
    pub pass_applied: bool,
    /// Bumped by every `generate`, relocation and look-ahead hand-over: only the latest
    /// request installs (`Deck.generation`).
    pub generation: u64,
    /// The spec `step_seed` last asked for, while it composes (None once any track loads), so
    /// a fast turn of the wheel counts every click from the seed it is heading to.
    pub seed_target: Option<TrackSpec>,
    /// How many times the deck has handed over to its pre-composed next track
    /// ([`Engine::advance_to_next`]).
    pub advances: u64,
    /// The song ended with auto-next on before its next version was composed: play that the
    /// moment it is ready.
    pub awaiting_next: bool,
    /// The next version was just started: wait for the audio thread to start it before looking
    /// for the end of a song again.
    pub starting_next: bool,
    /// Auto-mix is mixing this deck out: auto-next does not restart it when the tail ends
    /// (`Deck.autoMixingOut`; set by auto-mix).
    pub auto_mixing_out: bool,
    /// Called whenever no next track is composed or composing: keeps one coming.
    pub next_provider: Option<Box<dyn FnMut() -> TrackSpec + Send>>,
    /// The next track (`Deck.next`).
    pub next: NextTrackSlot<PreparedTrack>,
    /// dj-only: what the next track was asked for (a Queue next, the provider) before the
    /// deck's sound was carried onto it ([`TrackSpec::carrying_deck_sound`]); None when the
    /// slot holds auto-next's own next version. Read to compose it again when the deck's
    /// sound changes while it waits.
    pub next_base: Option<TrackSpec>,
    next_task: Option<(u64, Task<PreparedTrack>)>,
    /// The swap Wander's next move plans, composed a bar early (`Deck.lookAhead`).
    pub look_ahead: LookAheadSlot<PreparedTrack>,
    look_task: Option<(u64, Task<PreparedTrack>)>,
    /// Composes nobody waits for any more (their slot was replaced, dropped or cancelled while
    /// they ran): [`Engine::service`] drops what they made once it lands, which gives the slots'
    /// references back (the samplers they bound wait in the bank, not in the track).
    orphans: Vec<Task<PreparedTrack>>,
    installs: Vec<PendingInstall>,
    moves: Vec<PendingMove>,
    /// The song tick the last `mix_in`'s rest ends on (`mixInEndTick`); None once a track loads.
    pub mix_in_end_tick: Option<i64>,
    /// What the deck loaded last (`Deck.wave.latest`).
    pub latest: Option<Latest>,
    gate: Arc<Mutex<()>>,
}

impl TrackState {
    /// Is a track that `generate` or a swap asked for (the seed wheel, Regenerate, Wander's
    /// swap) still composing, or composed and not yet landed by [`Engine::service`]? What a
    /// window shows a spinner for; unlike [`is_busy`](Self::is_busy) the next track and the
    /// look-ahead, which are always being kept, do not count.
    pub fn is_composing(&self) -> bool {
        !self.installs.is_empty()
    }

    /// Any compose still running, or finished and not yet landed?
    pub fn is_busy(&self) -> bool {
        !self.installs.is_empty()
            || !self.moves.is_empty()
            || self.next_task.is_some()
            || self.look_task.is_some()
            || !self.orphans.is_empty()
    }

    /// Has every compose that is running finished? (What it made lands, or is dropped, at the
    /// next [`Engine::service`].)
    pub fn composes_finished(&self) -> bool {
        self.installs.iter().all(|p| p.task.is_finished())
            && self.moves.iter().all(|m| m.task.is_finished())
            && self.orphans.iter().all(Task::is_finished)
            && self.next_task.iter().all(|(_, t)| t.is_finished())
            && self.look_task.iter().all(|(_, t)| t.is_finished())
    }
}

impl super::engine::MainDeck {
    /// `Deck.isNetLoop`: a composed net loop (a spec with no structure: it plays until
    /// stopped).
    pub fn is_net_loop(&self) -> bool {
        self.is_net()
            && self
                .spec
                .as_ref()
                .is_some_and(|s| s.structure.as_deref().is_none_or(str::is_empty))
    }
}

// MARK: the deck calls

impl Engine {
    /// A snapshot of what compiling for `deck` reads, which a worker thread can use.
    pub fn preparer(&self, deck: &str) -> Option<Preparer> {
        let d = self.deck(deck)?;
        Some(Preparer {
            sample_rate: self.sample_rate,
            opts: d.opts.clone(),
            role_filter: d.opts.role_filter.clone(),
            mirror: d.mirror.clone(),
            gate: d.track.gate.clone(),
            #[cfg(feature = "sf2")]
            bank: d.bank.clone(),
            #[cfg(feature = "sf2")]
            library: self.library.clone(),
            installed: self.installed.clone(),
        })
    }

    /// Let go of a track that is not wanted after all. Its slots' references come back as it
    /// drops; the samplers it bound were never in it, they wait in the bank for the next project
    /// the deck is handed ([`Outbox`](super::samplers::Outbox)).
    pub(crate) fn discard(&mut self, i: usize, track: PreparedTrack) {
        let _ = (i, track);
    }

    /// Let go of a track composed for `deck` that is not going to load (a compose another
    /// superseded, a load `perform` turned down). Its slots' references come back as it drops;
    /// the samplers it bound were never in it, they wait in the deck's bank for the next project
    /// the deck is handed ([`Outbox`](super::samplers::Outbox)), so dropping it plain loses
    /// nothing either. This is the one place a host says a track is not wanted: the engine's own
    /// composes (`generate`, the next track, the look-ahead) go through [`discard`](Self::discard),
    /// and a host that composes with [`PreparedTrack::from_source`] and then decides against the
    /// load calls this (or uses [`ComposeJobs`](super::ComposeJobs), which does).
    pub fn give_back_track(&mut self, deck: &str, track: PreparedTrack) {
        match self.index(deck) {
            Some(i) => self.discard(i, track),
            None => drop(track),
        }
    }

    pub(crate) fn discard_prepared(&mut self, i: usize, p: Prepared) {
        let _ = (i, p);
    }

    /// A compose nobody waits for any more (its slot was replaced, dropped or cancelled). The track
    /// it makes is dropped unused, now if it has finished, else when it lands
    /// ([`reap_orphans`](Self::reap_orphans)); its samplers are not lost with it (Swift's live in
    /// the bank), they wait there to be sent.
    fn orphan(&mut self, i: usize, task: Task<PreparedTrack>) {
        // an offloaded compose still out is let go: unheld, the host neither sends nor installs
        // it, and what it would have made is only discarded
        if task.is_remote() && !task.is_finished() {
            return;
        }
        if !task.is_finished() {
            self.decks[i].track.orphans.push(task);
        } else if let Some(track) = task.try_take() {
            self.discard(i, track);
        }
    }

    /// Drop what the orphaned composes made (`block`: wait for the ones still running).
    fn reap_orphans(&mut self, i: usize, block: bool) {
        let orphans = std::mem::take(&mut self.decks[i].track.orphans);
        for task in orphans {
            if !block && !task.is_finished() {
                self.decks[i].track.orphans.push(task);
            } else if let Some(track) = task.wait_ok() {
                self.discard(i, track);
            }
        }
    }

    /// `Deck.loadNet(_ track: PreparedTrack)`: put a track composed elsewhere on the deck: a
    /// playing deck swaps at the next bar, a stopped one switches at once. The spec (or the
    /// raw-`nets` payload), the fixes and the pass are the deck's from here on; what
    /// belonged to the track before (the mix-in's end, the seed `step_seed` was heading to,
    /// the hot cues, the STEMS mutes, the layers, and the channels' voice settings unless this
    /// is the same song with another instrument picked) goes.
    pub fn load_net(&mut self, deck: &str, track: PreparedTrack) -> bool {
        match self.index(deck) {
            Some(i) => self.load_net_at(i, track),
            None => false,
        }
    }

    fn load_net_at(&mut self, i: usize, track: PreparedTrack) -> bool {
        self.load_net_at_keeping(i, track, false)
    }

    /// [`load_net_at`](Self::load_net_at); `keep_voices` keeps the sound panel's voice
    /// settings through a new song too (auto-next, a queued next).
    fn load_net_at_keeping(&mut self, i: usize, track: PreparedTrack, keep_voices: bool) -> bool {
        let PreparedTrack {
            source,
            prepared,
            fixes,
            pass_applied,
            ..
        } = track;
        let name = self.decks[i].name.clone();
        let keep_voices = keep_voices || self.keeps_voice_settings(&name, &source);
        // (a sample deck is taken back, and `hand_over` clears the hot cues and the ring
        // shapes, stages the project, records what `wave.latest` shows and sets the tempo again
        // behind it)
        if !self.hand_over_with(&name, prepared, true, keep_voices) {
            return false;
        }
        let d = &mut self.decks[i];
        // a new track: its layer list is its own `loopLayers`, else empty (`resetLayers(from:)`)
        let (layers, layer_key) = super::moves::layers_of(&source);
        d.walk.layer_entries = layers;
        d.walk.layer_key_name = layer_key;
        d.walk.layer_epoch += 1;
        match source {
            TrackSource::Net(spec) => {
                d.seed = spec.seed;
                d.spec = Some(spec);
                d.track.raw_nets = None;
            }
            TrackSource::RawNets(payload) => {
                d.seed = DeckSource::voicing_seed(&payload);
                d.spec = None;
                d.track.raw_nets = Some(payload);
            }
        }
        d.track.fixes = Some(fixes);
        d.track.pass_applied = pass_applied;
        d.track.seed_target = None;
        d.track.mix_in_end_tick = None;
        true
    }

    /// `Deck.generate(_:startTick:)`, without waiting: compose `spec` on a worker thread
    /// (inline in `deterministic` mode), then [`service`](Self::service) loads it (at the next
    /// bar if the deck is playing) unless a newer `generate`, `swap` or relocation superseded
    /// it meanwhile. Returns this request's generation (see [`settle`](Self::settle)).
    ///
    /// `start`: where it starts, so a song swapped in while one plays keeps the transport
    /// going; `NextBar` reads the deck's tick on the compose thread just before it prepares
    /// and starts the song at the bar after it.
    ///
    /// A `Tick` the deck has already reached by the time the compose finishes (a Wander swap
    /// composed slower than its phrase's last bar) would install at the next bar a bar or more
    /// back in the song; it is prepared again ([`PreparedTrack::restarted`], nothing composed
    /// or measured again) to start at the bar after the deck's tick then. Such a late swap
    /// lands a bar (or more) later than the Wander log says.
    pub fn begin_generate(
        &mut self,
        deck: &str,
        spec: &TrackSpec,
        start: SwapStart,
    ) -> Option<u64> {
        let i = self.index(deck)?;
        let preparer = self.preparer(deck)?;
        let mirror = preparer.mirror.clone();
        let spec = spec.clone();
        let task = if self.offloading() {
            // (the start is read now, where a thread reads it as it starts)
            let at = start.tick(mirror.tick());
            self.bg_compose(preparer, spec, at)
        } else {
            let work = move || {
                let at = start.tick(mirror.tick());
                PreparedTrack::compose(&spec, &preparer, at)
            };
            if self.deterministic {
                Task::run(work)
            } else {
                Task::spawn(work)
            }
        };
        let t = &mut self.decks[i].track;
        t.generation += 1;
        let mine = t.generation;
        t.installs.push(PendingInstall {
            generation: mine,
            start,
            stage: Stage::Compose,
            task,
        });
        Some(mine)
    }

    /// `await deck.generate(spec, startTick: start)`: compose `spec` and load it, and return
    /// whether it was loaded (false if a newer request superseded it meanwhile).
    pub fn generate_at(&mut self, deck: &str, spec: &TrackSpec, start: SwapStart) -> bool {
        let Some(g) = self.begin_generate(deck, spec, start) else {
            return false;
        };
        let i = self.index(deck).unwrap_or(0);
        self.settle(i, true).iter().any(|&(gen, ok)| gen == g && ok)
    }

    /// Land what the background composes of deck `i` have finished: the tracks `generate`
    /// asked for (installed, prepared again first if their `Tick` has passed), and the
    /// relocations. With `block`, wait for every one still running. Returns the generation of
    /// each install that was decided and whether it was loaded.
    pub fn settle(&mut self, i: usize, block: bool) -> Vec<(u64, bool)> {
        let mut results = Vec::new();
        // relocations first: they were asked for first as often as not, and a newer generation
        // refuses them anyway
        let moves = std::mem::take(&mut self.decks[i].track.moves);
        let mut waiting = Vec::new();
        for m in moves {
            if !block && !m.task.is_finished() {
                waiting.push(m);
                continue;
            }
            let Some(prepared) = m.task.wait_ok() else {
                continue;
            };
            let d = &self.decks[i];
            if m.generation == d.track.generation && d.is_net() {
                let name = d.name.clone();
                self.hand_over(&name, prepared, false);
            } else {
                self.discard_prepared(i, prepared);
            }
        }
        self.decks[i].track.moves.extend(waiting);
        loop {
            let pending = std::mem::take(&mut self.decks[i].track.installs);
            if pending.is_empty() {
                break;
            }
            let mut again = Vec::new();
            let mut progressed = false;
            for p in pending {
                if !block && !p.task.is_finished() {
                    again.push(p);
                    continue;
                }
                progressed = true;
                let Some(track) = p.task.wait_ok() else {
                    results.push((p.generation, false));
                    continue;
                };
                match self.install(i, track, p.start, p.generation, p.stage) {
                    Install::Again(next) => again.push(next),
                    Install::Decided(loaded) => results.push((p.generation, loaded)),
                }
            }
            self.decks[i].track.installs.extend(again);
            if !block || !progressed {
                break;
            }
        }
        results
    }

    /// The end of `generate` (`Deck.install`): load `composed` unless a newer request
    /// superseded it, prepared again first if its `Tick` has passed.
    fn install(
        &mut self,
        i: usize,
        composed: PreparedTrack,
        start: SwapStart,
        mine: u64,
        stage: Stage,
    ) -> Install {
        if mine != self.decks[i].track.generation {
            self.discard(i, composed);
            return Install::Decided(false);
        }
        let mirror = self.decks[i].mirror.clone();
        if stage == Stage::Compose {
            if let SwapStart::Tick(t) = start {
                if t > 0 && composed.prepared.wave().start_tick == t && mirror.tick() >= t {
                    let name = self.decks[i].name.clone();
                    let Some(preparer) = self.preparer(&name) else {
                        return Install::Decided(false);
                    };
                    let late = composed;
                    // (the samplers the first compose bound were never sent, and the restart finds
                    // their slots numbered already: they wait in the bank for the track that loads)
                    let task = if self.offloading() {
                        let tick = SwapStart::NextBar.tick(mirror.tick());
                        self.bg_restart(late, preparer, tick)
                    } else {
                        let work = move || {
                            late.restarted(&preparer, SwapStart::NextBar.tick(mirror.tick()))
                        };
                        if self.deterministic {
                            Task::run(work)
                        } else {
                            Task::spawn(work)
                        }
                    };
                    return Install::Again(PendingInstall {
                        generation: mine,
                        start,
                        stage: Stage::Restart,
                        task,
                    });
                }
            }
        }
        Install::Decided(self.load_net_at(i, composed))
    }

    /// `Deck.isNetLoop`.
    pub fn is_net_loop(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.is_net_loop())
    }

    /// `Deck.stepSeed`: the browser wheel on a net deck: the same genre (and everything else
    /// the spec says), `delta` seeds on, composed and swapped in at the next bar. A loop
    /// starts the new seed from its top, as it always has; a song keeps the transport going:
    /// the new seed comes in at the bar after the swap, not at its first bar. Clicks that land
    /// while one composes count from the seed it is heading to; the latest wins. None (and
    /// nothing happens) unless the deck holds a composed net.
    pub fn step_seed(&mut self, deck: &str, delta: i64) -> Option<TrackSpec> {
        let d = self.deck(deck)?;
        if delta == 0 || !d.is_net() {
            return None;
        }
        let mut spec = d.track.seed_target.clone().or_else(|| d.spec.clone())?;
        let start = if d.is_net_loop() {
            SwapStart::Top
        } else {
            SwapStart::NextBar
        };
        spec.seed = (spec.seed + delta as f64).max(1.0);
        self.swap_to(deck, &spec, start);
        Some(spec)
    }

    /// `Deck.swap(to:start:)`: compose `spec` and hand it over at the next bar, starting where
    /// `start` says: inline in `deterministic` mode, else in the background
    /// (`track.seed_target` holds it while it composes).
    pub fn swap_to(&mut self, deck: &str, spec: &TrackSpec, start: SwapStart) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        if self.deterministic {
            let Some(preparer) = self.preparer(deck) else {
                return false;
            };
            let at = start.tick(self.decks[i].mirror.tick());
            let track = PreparedTrack::compose(spec, &preparer, at);
            return self.load_net_at(i, track);
        }
        self.decks[i].track.seed_target = Some(spec.clone());
        self.begin_generate(deck, spec, start).is_some()
    }

    /// `Deck.swap(to:start:ahead:)`: `swap_to` with Wander's look-ahead for it
    /// ([`take_look_ahead`](Self::take_look_ahead)): composed already and on time, it loads
    /// now; still composing (or late), it is awaited and installed as `generate` would. Never
    /// in `deterministic` mode (no look-ahead is made there).
    pub fn swap_with_ahead(
        &mut self,
        deck: &str,
        spec: &TrackSpec,
        start: SwapStart,
        ahead: TakenAhead,
    ) {
        let Some(i) = self.index(deck) else { return };
        let now = self.decks[i].mirror.tick();
        let TakenAhead { ready, task } = ahead;
        match ready {
            Some(track) if !start.passed(now) => {
                self.decks[i].track.generation += 1; // supersedes any `generate` still composing
                self.load_net_at(i, track);
            }
            ready => {
                let task = match (ready, task) {
                    (Some(track), _) => Task::immediate(track),
                    (None, Some(t)) => t,
                    (None, None) => return,
                };
                let t = &mut self.decks[i].track;
                t.seed_target = Some(spec.clone());
                t.generation += 1;
                let mine = t.generation;
                t.installs.push(PendingInstall {
                    generation: mine,
                    start,
                    stage: Stage::Compose,
                    task,
                });
            }
        }
    }

    // MARK: look-ahead

    /// Compose `key.spec` in the background for Wander's look-ahead, replacing what the slot
    /// held (`LookAheadSlot.request`): from `key.start` (`Top` or `Tick`, never the deck's
    /// clock), with the role filter the key names.
    pub fn request_look_ahead(&mut self, deck: &str, key: LookAheadKey) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        let Some(preparer) = self.preparer(deck) else {
            return false;
        };
        let preparer = preparer.with_role_filter(key.role_filter.clone());
        let spec = key.spec.clone();
        let req = self.decks[i].track.look_ahead.request(key);
        if let Some(old) = req.replaced {
            self.discard(i, old);
        }
        let start = req.start_tick;
        let task = self.bg_compose(preparer, spec, start);
        if let Some((_, old)) = self.decks[i].track.look_task.replace((req.token, task)) {
            self.orphan(i, old);
        }
        true
    }

    /// Move a finished look-ahead compose into its slot.
    fn poll_look_ahead(&mut self, i: usize) {
        let t = &mut self.decks[i].track;
        let Some((token, task)) = &t.look_task else {
            return;
        };
        if !task.is_finished() {
            return;
        }
        let token = *token;
        let track = task.try_take();
        t.look_task = None;
        if let Some(track) = track {
            if let Err(track) = self.decks[i].track.look_ahead.complete(token, track) {
                self.discard(i, track);
            }
        }
    }

    /// What the look-ahead slot holds (composed or composing); None when empty.
    pub fn look_ahead_key(&mut self, deck: &str) -> Option<LookAheadKey> {
        let i = self.index(deck)?;
        self.poll_look_ahead(i);
        self.decks[i].track.look_ahead.key().cloned()
    }

    /// Has the held look-ahead finished composing?
    pub fn look_ahead_ready(&mut self, deck: &str) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        self.poll_look_ahead(i);
        self.decks[i].track.look_ahead.is_ready()
    }

    /// Wait for the held look-ahead to finish composing (a test's `await lookAhead.task`).
    pub fn wait_look_ahead(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        if let Some((token, task)) = self.decks[i].track.look_task.take() {
            if let Some(track) = task.wait_ok() {
                if let Err(track) = self.decks[i].track.look_ahead.complete(token, track) {
                    self.discard(i, track);
                }
            }
        }
    }

    /// `LookAheadSlot.take`: the look-ahead for `key`, off the slot; None when it holds
    /// another (or none). Either way the slot is empty after.
    pub fn take_look_ahead(&mut self, deck: &str, key: &LookAheadKey) -> Option<TakenAhead> {
        let i = self.index(deck)?;
        self.poll_look_ahead(i);
        if self.decks[i].track.look_ahead.key() != Some(key) {
            self.drop_look_ahead(deck);
            return None;
        }
        let taken = self.decks[i].track.look_ahead.take(key)?;
        let task = match self.decks[i].track.look_task.take() {
            Some((token, task)) if token == taken.token => Some(task),
            Some((_, other)) => {
                self.orphan(i, other);
                None
            }
            None => None,
        };
        Some(TakenAhead {
            ready: taken.ready,
            task,
        })
    }

    /// `LookAheadSlot.drop`: let go of what the slot holds.
    pub fn drop_look_ahead(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        if let Some((_, old)) = self.decks[i].track.look_task.take() {
            self.orphan(i, old);
        }
        if let Some(old) = self.decks[i].track.look_ahead.drop_held() {
            self.discard(i, old);
        }
    }

    // MARK: the next track

    /// `Deck.queueNext`: start composing the next track (cancels an earlier request): in the
    /// background, inline in `deterministic` mode.
    ///
    /// The deck's sound (palette, kit, role voices, instrument picks) is carried onto `spec`
    /// where it does not pin its own, and carried again if the deck's sound changes before
    /// the track plays.
    pub fn queue_next(&mut self, deck: &str, spec: &TrackSpec) {
        let Some(i) = self.index(deck) else { return };
        self.decks[i].track.next_base = Some(spec.clone());
        if let Some(want) = self.wanted_next(i) {
            self.compose_next(deck, &want);
        }
    }

    /// The next track this deck should have ready, its sound carried from the deck's: the
    /// one asked for ([`TrackState::next_base`]), else (auto-next on) this song's next
    /// version. None when neither applies.
    fn wanted_next(&self, i: usize) -> Option<TrackSpec> {
        let d = &self.decks[i];
        let cur = d.spec.as_ref();
        match (&d.track.next_base, cur) {
            (Some(b), Some(c)) => Some(b.carrying_deck_sound(c)),
            (Some(b), None) => Some(b.clone()),
            (None, Some(c)) if d.auto_advance => Some(c.next_version().carrying_deck_sound(c)),
            (None, _) => None,
        }
    }

    /// The next track in the slot (ready or composing) was made with another sound than the
    /// deck plays now: compose it again before it is handed over.
    fn next_is_stale(&self, i: usize) -> bool {
        let t = &self.decks[i].track;
        let Some(have) = t.next.ready_spec().or(t.next.pending_spec()) else {
            return false;
        };
        self.wanted_next(i).is_some_and(|w| &w != have)
    }

    /// Compose `spec` into the deck's next slot (`Deck.queueNext`), exactly as given.
    fn compose_next(&mut self, deck: &str, spec: &TrackSpec) {
        let Some(i) = self.index(deck) else { return };
        let Some(preparer) = self.preparer(deck) else {
            return;
        };
        if self.deterministic {
            let track = PreparedTrack::compose(spec, &preparer, 0);
            if let Some((_, old)) = self.decks[i].track.next_task.take() {
                self.orphan(i, old);
            }
            if let Some(old) = self.decks[i].track.next.request_sync(spec.clone(), track) {
                self.discard(i, old);
            }
        } else {
            let (token, old) = self.decks[i].track.next.request(spec.clone());
            if let Some(old) = old {
                self.discard(i, old);
            }
            let task = self.bg_compose(preparer, spec.clone(), 0);
            if let Some((_, old)) = self.decks[i].track.next_task.replace((token, task)) {
                self.orphan(i, old);
            }
        }
    }

    fn poll_next(&mut self, i: usize) {
        let t = &mut self.decks[i].track;
        let Some((token, task)) = &t.next_task else {
            return;
        };
        if !task.is_finished() {
            return;
        }
        let token = *token;
        let track = task.try_take();
        t.next_task = None;
        if let Some(track) = track {
            if let Err(track) = self.decks[i].track.next.complete(token, track) {
                self.discard(i, track);
            }
        }
    }

    /// `Deck.nextReady`: the spec of the next track, once composed (as of the last
    /// [`service`](Self::service)).
    pub fn next_ready(&self, deck: &str) -> Option<&TrackSpec> {
        self.deck(deck)?.track.next.ready_spec()
    }

    /// `Deck.nextComposing`.
    pub fn next_composing(&self, deck: &str) -> Option<&TrackSpec> {
        self.deck(deck)?.track.next.pending_spec()
    }

    /// `Deck.cancelNext`.
    pub fn cancel_next(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        self.decks[i].track.next_base = None;
        if let Some((_, old)) = self.decks[i].track.next_task.take() {
            self.orphan(i, old);
        }
        if let Some(old) = self.decks[i].track.next.cancel() {
            self.discard(i, old);
        }
    }

    /// `Deck.advanceToNext`: move the pre-composed track onto the deck. False if none is ready
    /// yet.
    pub fn advance_to_next(&mut self, deck: &str) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        self.poll_next(i);
        self.advance_at(i)
    }

    fn advance_at(&mut self, i: usize) -> bool {
        // a next track composed with another sound than the deck plays now is not handed
        // over: it is composed again (auto-next waits for it; in `deterministic` mode it is
        // ready at once)
        if self.next_is_stale(i) {
            if let Some(want) = self.wanted_next(i) {
                let name = self.decks[i].name.clone();
                self.compose_next(&name, &want);
            }
        }
        let Some(track) = self.decks[i].track.next.take() else {
            return false;
        };
        self.decks[i].track.next_base = None;
        // the sound panel's voice settings ride into the next track: a channel whose
        // instrument changed starts afresh at the hand-over
        self.load_net_at_keeping(i, track, true);
        self.decks[i].track.advances += 1;
        true
    }

    /// `Deck.skipToNextTrack`: hand over to the pre-composed next track and keep playing.
    pub fn skip_to_next_track(&mut self, deck: &str) {
        let Some(d) = self.deck(deck) else { return };
        let was = d.playing;
        let auto = d.auto_advance;
        if self.advance_to_next(deck) && (was || auto) {
            self.set_playing(deck, true);
        }
    }

    /// Wait for every background compose of `deck` (the track `generate` asked for, the
    /// relocations, the next track and the look-ahead) and land it: what a test does where
    /// Swift awaits or polls with `eventually`.
    pub fn wait_composes(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        self.settle(i, true);
        if let Some((token, task)) = self.decks[i].track.next_task.take() {
            if let Some(track) = task.wait_ok() {
                if let Err(track) = self.decks[i].track.next.complete(token, track) {
                    self.discard(i, track);
                }
            }
        }
        self.wait_look_ahead(deck);
        self.reap_orphans(i, true);
    }

    // MARK: housekeeping

    /// `Deck.service` for every deck, the hub's ~100 Hz tick: free what the audio thread let
    /// go of, land the composes that finished, notice a song that ended and keep the next
    /// track coming; then every deck's walk ([`service_wander`](Self::service_wander)) and,
    /// last, auto-mix ([`service_auto_mix`](Self::service_auto_mix)), once per call. Swift's
    /// hub runs those two after the loop takes, which `Rig::tick` runs after this call (the
    /// controllers come before it, as in Swift); neither the walk nor auto-mix reads or writes
    /// the loop row, so the order between them does not show.
    pub fn service(&mut self) {
        self.drain_retired();
        self.enforce_disabled_roles();
        for i in 0..self.decks.len() {
            self.service_deck(i);
        }
        // auto-next every N bars (beyond Swift), before the walk: a step grabs it as the
        // wheel's does
        self.service_bar_next();
        // keys held back for the grid (docs/MPK.md)
        self.service_keys();
        // every deck's walk, right after the decks' own service (`DJEngine.serviceWander`)
        self.service_wander();
        // auto-mix last, after the walk (`DJEngine.serviceAutoMix`)
        self.service_auto_mix();
    }

    fn service_deck(&mut self, i: usize) {
        // (Swift's installs resume on the main actor between ticks)
        self.settle(i, false);
        self.poll_next(i);
        self.poll_look_ahead(i);
        self.reap_orphans(i, false);
        // follow the leader's tempo changes (the same multiplier rule as sync)
        self.follow_leader(i);
        let d = &self.decks[i];
        if !d.is_net() {
            return;
        }
        let name = d.name.clone();
        // a song just handed over: until the audio thread starts it, the old one still reads
        // as ended, and taking that for a new ending would skip a whole version
        if d.track.starting_next {
            if d.mirror.is_playing() || !d.playing {
                self.decks[i].track.starting_next = false;
            } else {
                return;
            }
        }
        let d = &self.decks[i];
        let ended = d.playing
            && !d.mirror.is_playing()
            && d.mirror.stopped.load(std::sync::atomic::Ordering::Relaxed);
        if ended {
            self.decks[i].playing = false;
            let d = &self.decks[i];
            // a deck auto-mix mixed out stays stopped (the next track is on the other deck)
            if d.auto_advance && !d.track.auto_mixing_out {
                // the next version, now; or as soon as it is composed
                if self.advance_at(i) {
                    self.start_next(&name);
                } else {
                    self.decks[i].track.awaiting_next = true;
                }
            }
        }
        if self.decks[i].track.awaiting_next {
            let d = &self.decks[i];
            if !d.auto_advance || d.playing {
                self.decks[i].track.awaiting_next = false;
            } else if self.advance_at(i) {
                self.decks[i].track.awaiting_next = false;
                self.start_next(&name);
            }
        }
        let d = &mut self.decks[i];
        let busy = d.track.next.is_busy();
        let provided = if busy {
            None
        } else {
            d.track.next_provider.as_mut().map(|provide| provide())
        };
        if let Some(spec) = provided {
            self.queue_next(&name, &spec);
        } else if !busy {
            // auto-next keeps the next version of this song composed: no Queue next needed
            self.decks[i].track.next_base = None;
            if let Some(want) = self.wanted_next(i) {
                self.compose_next(&name, &want);
            }
        } else if self.next_is_stale(i) {
            // the deck's sound changed since the next track was asked for: carry it again
            if let Some(want) = self.wanted_next(i) {
                self.compose_next(&name, &want);
            }
        }
    }

    /// Play the track that just took over (auto-next), and hold off end-of-song checks until
    /// it is playing.
    fn start_next(&mut self, deck: &str) {
        self.set_playing(deck, true);
        if let Some(d) = self.deck_mut(deck) {
            d.track.starting_next = true;
        }
    }

    // MARK: relocation

    /// `Deck.moveRequest`: what [`relocate`](Self::relocate) prepares, read on the main
    /// thread: `prepare()` it on any thread. None off a net.
    pub fn move_request(
        &self,
        deck: &str,
        tick: i64,
        tail: Option<crate::wave::MixOutTail>,
        rest: Option<crate::wave::rests::Entry>,
    ) -> Option<MoveRequest> {
        let d = self.deck(deck)?;
        let project = d.project.clone().filter(|_| d.is_net())?;
        let preparer = self.preparer(deck)?;
        let palette = swap::project_palette(&project);
        let opts = swap::moved_options(
            &preparer.options(d.seed),
            d.track.fixes,
            d.spec.as_ref().map(|s| s.genre.as_str()),
            d.track.latest.as_ref(),
            palette.as_deref(),
            tail,
            rest,
        );
        Some(MoveRequest {
            preparer,
            project,
            opts,
            start_tick: tick.max(0),
            track_serial: d.track.latest.as_ref().map(|l| l.track_serial),
        })
    }

    /// Hand over a relocation prepared elsewhere ([`MoveRequest::prepare`]): the same song
    /// from another tick, swapped in at the next bar. The deck keeps its cues; a new track
    /// is not loaded.
    pub fn load_move(&mut self, deck: &str, prepared: Prepared) -> bool {
        self.hand_over(deck, prepared, false)
    }

    /// Queue a relocation's compose on a worker thread; it lands unless a newer request or a
    /// track change superseded it (`jumpToTick`'s `generation` check).
    pub(crate) fn begin_move(&mut self, deck: &str, req: MoveRequest) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        let task = self.bg_move(req);
        let t = &mut self.decks[i].track;
        t.generation += 1;
        let generation = t.generation;
        t.moves.push(PendingMove { generation, task });
        true
    }
}

/// What a worker thread takes and hands back is `Send`: checked when this compiles.
#[allow(dead_code)]
fn _threads_can_carry() {
    fn send<T: Send>() {}
    send::<PreparedTrack>();
    send::<Preparer>();
    send::<MoveRequest>();
    send::<Prepared>();
}
