//! The engine's own background composes, run where the host says.
//!
//! An engine composes on its own as it plays: the next version of a song (auto-advance, the
//! media controls' next), a Wander or auto-next swap and its late restart, Wander's look-ahead,
//! a relocation (a section launch, a palette, a seek, a mix-out), auto-mix's incoming track and
//! its mix-out a phrase ahead, a deck copy. Live, each runs on a worker thread ([`Task::spawn`])
//! and lands on [`Engine::service`]; `deterministic`, inline. The helpers here (`bg_*`) are
//! where every one of them starts.
//!
//! **Offloaded** (feature `transfer`, [`Engine::set_offload`]): a host whose engine has no
//! thread to spare (djweb: the engine lives in an AudioWorklet, the only thread it has) runs
//! them in another engine instance instead (a Worker). Each becomes a request
//! ([`Engine::take_offload`]: the preparer and what to run with it, as bytes) and a
//! [`Task::remote`] the deck holds as it would a thread's. The host carries the request to the
//! other instance, which runs it ([`answer_offload`](super::transfer::answer_offload)) exactly
//! as the thread would have, and brings the answer back ([`Engine::offload_answer`]); the
//! engine takes it in a byte budget at a time ([`Engine::offload_step`], one step per render
//! quantum) and finishes the task, and the deck lands it on its next service. Nothing is
//! composed, measured or compiled on the engine's thread.
//!
//! A start the request is made at (a `SwapStart::NextBar`, a late restart's bar) is read when
//! the request is made, where a thread reads it when it starts: the same tick to within the
//! time a thread takes to start, and a compose that lands late is restarted as a thread's is.
//! A compose the deck dropped meanwhile (superseded, orphaned and reaped) is not sent, or not
//! installed. A preparer that holds something of this process (a sampler bank, the host's
//! installed sounds) cannot travel: that compose runs inline, as it would `deterministic`.

use crate::spec::TrackSpec;

use super::engine::{Engine, Prepared};
use super::track::{MoveRequest, PreparedTrack, Preparer, Task, TrackSource};

impl Engine {
    /// Are the engine's own composes offloaded to the host ([`set_offload`](Self::set_offload))?
    pub fn offloading(&self) -> bool {
        #[cfg(feature = "transfer")]
        {
            self.offload.is_some()
        }
        #[cfg(not(feature = "transfer"))]
        {
            false
        }
    }

    /// `PreparedTrack::compose(spec, preparer, start_tick)` off this thread.
    pub(crate) fn bg_compose(&mut self, preparer: Preparer, spec: TrackSpec, start_tick: i64) -> Task<PreparedTrack> {
        #[cfg(feature = "transfer")]
        if self.offload.is_some() {
            let work = wire::Work::Track { source: TrackSource::Net(spec.clone()), start_tick };
            if let Some(task) = self.offload_track(&preparer, work) {
                return task;
            }
            return Task::run(|| PreparedTrack::compose(&spec, &preparer, start_tick));
        }
        Task::spawn(move || PreparedTrack::compose(&spec, &preparer, start_tick))
    }

    /// `PreparedTrack::from_source(source, preparer, start_tick)` off this thread.
    pub(crate) fn bg_from_source(
        &mut self,
        preparer: Preparer,
        source: TrackSource,
        start_tick: i64,
    ) -> Task<Option<PreparedTrack>> {
        #[cfg(feature = "transfer")]
        if self.offload.is_some() {
            let work = wire::Work::Track { source: source.clone(), start_tick };
            if let Some(task) = self.offload_source(&preparer, work) {
                return task;
            }
            return Task::run(|| PreparedTrack::from_source(&source, &preparer, start_tick));
        }
        Task::spawn(move || PreparedTrack::from_source(&source, &preparer, start_tick))
    }

    /// A relocation's compile ([`MoveRequest::prepare`]) off this thread.
    pub(crate) fn bg_move(&mut self, req: MoveRequest) -> Task<Prepared> {
        #[cfg(feature = "transfer")]
        if self.offload.is_some() {
            let serial = req.track_serial;
            match self.offload_prepare(&req.preparer, req.project, req.start_tick, req.opts, serial) {
                Ok(task) => return task,
                Err((project, opts)) => {
                    let req = MoveRequest { preparer: req.preparer, project, opts, start_tick: req.start_tick, track_serial: serial };
                    return Task::run(|| req.prepare());
                }
            }
        }
        Task::spawn(move || req.prepare())
    }

    /// `late.restarted(preparer, tick)` off this thread (a compose that came in after its
    /// tick, prepared again from a later one).
    pub(crate) fn bg_restart(&mut self, late: PreparedTrack, preparer: Preparer, tick: i64) -> Task<PreparedTrack> {
        #[cfg(feature = "transfer")]
        if self.offload.is_some() {
            let (start, opts, serial) = late.restart_args(tick);
            let project = late.prepared.source.clone();
            match self.offload_restart(&preparer, project, start, opts, serial, late) {
                Ok(task) => return task,
                Err(late) => return Task::run(|| late.restarted(&preparer, tick)),
            }
        }
        Task::spawn(move || late.restarted(&preparer, tick))
    }
}

#[cfg(feature = "transfer")]
pub use offloaded::OffloadLanded;
#[cfg(feature = "transfer")]
pub(crate) use offloaded::Offload;
#[cfg(feature = "transfer")]
pub(crate) use wire::{work_wire, OffloadRequest, PreparerWire, Work, WorkWire};

#[cfg(feature = "transfer")]
mod offloaded {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Request ids, process-wide: an answer still on its way for an engine a host has replaced
    /// (a restore) never matches a request of the one that replaced it.
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    use crate::pflow::Project;
    use crate::wave::WaveOptions;

    use super::super::engine::{Engine, Prepared};
    use super::super::track::{Fulfil, PreparedTrack, Preparer, Task};
    use super::super::transfer::{self, Install, PreparedInstall};
    use super::wire::{OffloadRequest, PreparerWire, Work};

    /// Where a finished request's answer goes.
    enum Answer {
        /// A composed spec: always a track.
        Track(Fulfil<PreparedTrack>),
        /// A track source (a raw-`nets` share may make none).
        Source(Fulfil<Option<PreparedTrack>>),
        /// A compile, another start of the song `serial` numbers (None: a song of its own).
        Prepared(Fulfil<Prepared>, Option<i32>),
        /// A late track's restart: the compile goes into the track.
        Restart(Fulfil<PreparedTrack>, Box<PreparedTrack>, i32),
    }

    impl Answer {
        fn wanted(&self) -> bool {
            match self {
                Answer::Track(f) | Answer::Restart(f, ..) => f.wanted(),
                Answer::Source(f) => f.wanted(),
                Answer::Prepared(f, _) => f.wanted(),
            }
        }
    }

    struct Asked {
        id: u64,
        /// Taken when the host takes the request.
        request: Option<OffloadRequest>,
        /// What the answer is installed for: the deck as it was when this was asked.
        preparer: Preparer,
        answer: Answer,
    }

    enum Stepping {
        Track(Install),
        Prepared(PreparedInstall),
    }

    struct Arriving {
        asked: Asked,
        install: Stepping,
        steps: u32,
        bytes: usize,
    }

    /// The engine's offloaded composes: asked, out with the host, and coming in.
    pub(crate) struct Offload {
        queued: VecDeque<Asked>,
        out: Vec<Asked>,
        arriving: VecDeque<Arriving>,
        /// `deterministic` before offloading started, put back when it stops.
        was_deterministic: bool,
    }

    /// What [`Engine::offload_step`] finished.
    #[derive(Debug, Clone, PartialEq)]
    pub struct OffloadLanded {
        pub id: u64,
        /// The answer's size and the steps it took in.
        pub bytes: usize,
        pub steps: u32,
        /// Why it was not delivered (it did not fit the deck any more, bad bytes, or nothing
        /// holds the compose now: `"unwanted"`); the compose then lands as a failed one.
        pub error: Option<String>,
    }

    impl Engine {
        /// Offload this engine's own composes to the host (see [`super`]): on, the engine is
        /// live (`deterministic` off) and every compose it starts on its own is a request
        /// [`take_offload`](Self::take_offload) hands out; off, the requests not answered yet
        /// fail and `deterministic` is what it was.
        pub fn set_offload(&mut self, on: bool) {
            match (on, self.offload.is_some()) {
                (true, false) => {
                    self.offload = Some(Offload {
                        queued: VecDeque::new(),
                        out: Vec::new(),
                        arriving: VecDeque::new(),
                        was_deterministic: self.deterministic,
                    });
                    self.deterministic = false;
                }
                (false, true) => {
                    if let Some(o) = self.offload.take() {
                        self.deterministic = o.was_deterministic;
                    }
                }
                _ => {}
            }
        }

        /// The next request for the host's composer, as `(id, bytes)`: the bytes go to
        /// [`answer_offload`](super::super::transfer::answer_offload) in the other instance, its
        /// answer comes back to [`offload_answer`](Self::offload_answer) under the same id. A
        /// compose the deck has dropped since it was asked is not handed out.
        pub fn take_offload(&mut self) -> Option<(u64, Vec<u8>)> {
            let o = self.offload.as_mut()?;
            while let Some(mut asked) = o.queued.pop_front() {
                if !asked.answer.wanted() {
                    continue;
                }
                let Some(req) = asked.request.take() else { continue };
                // (bytes that cannot be made fail the compose: the deck lands it as failed)
                let Ok(bytes) = transfer::encode_request(&req) else { continue };
                let id = asked.id;
                o.out.push(asked);
                return Some((id, bytes));
            }
            None
        }

        /// Requests asked and not taken yet.
        pub fn offload_queued(&self) -> usize {
            self.offload.as_ref().map_or(0, |o| o.queued.len())
        }

        /// Offloaded composes not finished yet: asked, out with the host, or coming in.
        pub fn offload_pending(&self) -> usize {
            self.offload.as_ref().map_or(0, |o| o.queued.len() + o.out.len() + o.arriving.len())
        }

        /// The composer's answer to request `id`: installed by [`offload_step`](Self::offload_step)
        /// from here (`Ok`), or the compose fails (`Err`: the composer could not run it). An
        /// answer for a compose the deck has dropped is let go. Errors: no such request, or an
        /// answer that is not one (the compose fails).
        pub fn offload_answer(&mut self, id: u64, answer: Result<Vec<u8>, String>) -> Result<(), String> {
            let o = self.offload.as_mut().ok_or("not offloading")?;
            let at = o.out.iter().position(|a| a.id == id).ok_or_else(|| format!("no offloaded compose {id}"))?;
            let asked = o.out.remove(at);
            let Ok(bytes) = answer else { return Ok(()) };
            if !asked.answer.wanted() {
                return Ok(());
            }
            let len = bytes.len();
            let install = match &asked.answer {
                Answer::Source(_) if bytes.is_empty() => {
                    if let Answer::Source(f) = asked.answer {
                        f.done(None);
                    }
                    return Ok(());
                }
                Answer::Track(_) | Answer::Source(_) => {
                    Stepping::Track(Install::new(bytes, &asked.preparer).map_err(|e| e.to_string())?)
                }
                Answer::Prepared(..) | Answer::Restart(..) => {
                    Stepping::Prepared(PreparedInstall::new(bytes, &asked.preparer).map_err(|e| e.to_string())?)
                }
            };
            o.arriving.push_back(Arriving { asked, install, steps: 0, bytes: len });
            Ok(())
        }

        /// Is request `id` out with the host, waiting for its answer?
        pub fn offload_waiting(&self, id: u64) -> bool {
            self.offload.as_ref().is_some_and(|o| o.out.iter().any(|a| a.id == id))
        }

        /// Is an answer coming in ([`offload_step`](Self::offload_step) has work)?
        pub fn offload_busy(&self) -> bool {
            self.offload.as_ref().is_some_and(|o| !o.arriving.is_empty())
        }

        /// About `budget` bytes of the oldest answer coming in; once it is all in, the compose
        /// it answers is finished, and the deck lands it on its next service.
        pub fn offload_step(&mut self, budget: usize) -> Option<OffloadLanded> {
            let o = self.offload.as_mut()?;
            let a = o.arriving.front_mut()?;
            a.steps += 1;
            let landed = |a: &Arriving, error: Option<String>| OffloadLanded { id: a.asked.id, bytes: a.bytes, steps: a.steps, error };
            if !a.asked.answer.wanted() {
                let a = o.arriving.pop_front()?;
                return Some(landed(&a, Some("unwanted".into())));
            }
            let done = match &mut a.install {
                Stepping::Track(i) => i.step(budget),
                Stepping::Prepared(i) => i.step(budget),
            };
            match done {
                Ok(false) => None,
                Err(e) => {
                    let a = o.arriving.pop_front()?;
                    Some(landed(&a, Some(e.to_string())))
                }
                Ok(true) => {
                    let a = o.arriving.pop_front()?;
                    let out = landed(&a, None);
                    let Arriving { asked, install, .. } = a;
                    let p = &asked.preparer;
                    let delivered = match (install, asked.answer) {
                        (Stepping::Track(i), Answer::Track(f)) => i.finish(p).map(|t| f.done(t)),
                        (Stepping::Track(i), Answer::Source(f)) => i.finish(p).map(|t| f.done(Some(t))),
                        (Stepping::Prepared(i), Answer::Prepared(f, serial)) => i.finish(p, serial).map(|x| f.done(x)),
                        (Stepping::Prepared(i), Answer::Restart(f, late, serial)) => {
                            i.finish(p, Some(serial)).map(|x| f.done(late.with_prepared(x)))
                        }
                        _ => Err(transfer::TransferError::Bytes("answer of another kind".into())),
                    };
                    Some(match delivered {
                        Ok(()) => out,
                        Err(e) => OffloadLanded { error: Some(e.to_string()), ..out },
                    })
                }
            }
        }

        fn ask(&mut self, preparer: &Preparer, wire: PreparerWire, work: Work, answer: Answer) {
            let Some(o) = self.offload.as_mut() else { return };
            o.queued.push_back(Asked {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                request: Some(OffloadRequest { preparer_wire: wire, work }),
                preparer: preparer.clone(),
                answer,
            });
        }

        /// Offload a spec's compose; None if the preparer cannot travel.
        pub(super) fn offload_track(&mut self, preparer: &Preparer, work: Work) -> Option<Task<PreparedTrack>> {
            let wire = PreparerWire::of(preparer)?;
            let (task, f) = Task::remote();
            self.ask(preparer, wire, work, Answer::Track(f));
            Some(task)
        }

        pub(super) fn offload_source(&mut self, preparer: &Preparer, work: Work) -> Option<Task<Option<PreparedTrack>>> {
            let wire = PreparerWire::of(preparer)?;
            let (task, f) = Task::remote();
            self.ask(preparer, wire, work, Answer::Source(f));
            Some(task)
        }

        /// Offload a compile; the project and options back if they cannot travel.
        pub(super) fn offload_prepare(
            &mut self,
            preparer: &Preparer,
            project: Project,
            start_tick: i64,
            opts: WaveOptions,
            track_serial: Option<i32>,
        ) -> Result<Task<Prepared>, (Project, WaveOptions)> {
            let (Some(wire), true) = (PreparerWire::of(preparer), PreparerWire::opts_travel(&opts)) else {
                return Err((project, opts));
            };
            let (task, f) = Task::remote();
            self.ask(preparer, wire, Work::Prepare { project, start_tick, opts, track_serial }, Answer::Prepared(f, track_serial));
            Ok(task)
        }

        /// Offload a late track's restart; the track back if it cannot travel.
        pub(super) fn offload_restart(
            &mut self,
            preparer: &Preparer,
            project: Project,
            start_tick: i64,
            opts: WaveOptions,
            serial: i32,
            late: PreparedTrack,
        ) -> Result<Task<PreparedTrack>, PreparedTrack> {
            let (Some(wire), true) = (PreparerWire::of(preparer), PreparerWire::opts_travel(&opts)) else {
                return Err(late);
            };
            let (task, f) = Task::remote();
            let work = Work::Prepare { project, start_tick, opts, track_serial: Some(serial) };
            self.ask(preparer, wire, work, Answer::Restart(f, Box::new(late), serial));
            Ok(task)
        }
    }
}

#[cfg(feature = "transfer")]
mod wire {
    //! An offloaded compose as data: what [`super::super::transfer`] writes and reads.

    use std::collections::BTreeMap;

    use crate::musicality::energy_curve::EnergyCurve;
    use crate::musicality::synth_voice::{HfTilt, VoiceDesign};
    use crate::musicality::echo::EchoSpec;
    use crate::musicality::space::SpaceSpec;
    use crate::musicality::Fixes;
    use crate::pflow::Project;
    use crate::wave::groove::GrooveTemplate;
    use crate::wave::instruments::InstrumentTable;
    use crate::wave::rests::{Entry, MixOutTail, RestSchedule};
    use crate::wave::synth::VoiceSpec;
    use crate::wave::{Master, RoleFilter, StingerPolicy, WaveMod, WaveOptions};
    use serde::{Deserialize, Serialize};

    use super::super::track::{Preparer, TrackSource};
    use super::super::transfer::TransferError;

    /// What an offloaded compose runs.
    pub(crate) enum Work {
        /// `PreparedTrack::from_source(source, preparer, start_tick)`.
        Track { source: TrackSource, start_tick: i64 },
        /// `preparer.prepare_opts(project, start_tick, opts, track_serial)`.
        Prepare { project: Project, start_tick: i64, opts: WaveOptions, track_serial: Option<i32> },
    }

    pub(crate) struct OffloadRequest {
        pub(crate) preparer_wire: PreparerWire,
        pub(crate) work: Work,
    }

    #[derive(Clone, Serialize, Deserialize)]
    pub(crate) enum WorkWire {
        Track { source: TrackSource, start_tick: i64 },
        /// The project follows the header as frames.
        Prepare { start_tick: i64, opts: OptsWire, track_serial: Option<i32> },
    }

    /// The header half of `work`, and the project a compile carries as frames.
    pub(crate) fn work_wire(work: &Work) -> (WorkWire, Option<&Project>) {
        match work {
            Work::Track { source, start_tick } => (WorkWire::Track { source: source.clone(), start_tick: *start_tick }, None),
            Work::Prepare { project, start_tick, opts, track_serial } => (
                WorkWire::Prepare {
                    start_tick: *start_tick,
                    // (checked when it was asked: `opts_travel`)
                    opts: OptsWire::of(opts).expect("offloaded options travel"),
                    track_serial: *track_serial,
                },
                Some(project),
            ),
        }
    }

    impl WorkWire {
        pub(crate) fn needs_project(&self) -> bool {
            matches!(self, WorkWire::Prepare { .. })
        }

        pub(crate) fn into_work(self, project: Option<Project>) -> Result<Work, TransferError> {
            Ok(match self {
                WorkWire::Track { source, start_tick } => Work::Track { source, start_tick },
                WorkWire::Prepare { start_tick, opts, track_serial } => Work::Prepare {
                    project: project.ok_or_else(|| TransferError::Bytes("no project".into()))?,
                    start_tick,
                    opts: opts.into_options(),
                    track_serial,
                },
            })
        }
    }

    /// A [`Preparer`] with nothing of its process in it: the rate, the deck's options and its
    /// role filter.
    #[derive(Clone, Serialize, Deserialize)]
    pub(crate) struct PreparerWire {
        sample_rate: u64,
        role_filter: Option<RoleFilter>,
        opts: OptsWire,
    }

    impl PreparerWire {
        /// None if `p` holds something only this process has (a sampler bank, installed sounds).
        pub(crate) fn of(p: &Preparer) -> Option<PreparerWire> {
            if !p.is_bare() {
                return None;
            }
            Some(PreparerWire { sample_rate: p.sample_rate.to_bits(), role_filter: p.role_filter.clone(), opts: OptsWire::of(&p.opts)? })
        }

        pub(crate) fn opts_travel(o: &WaveOptions) -> bool {
            OptsWire::of(o).is_some()
        }

        pub(crate) fn preparer(self) -> Result<Preparer, TransferError> {
            Ok(Preparer::bare(f64::from_bits(self.sample_rate), self.opts.into_options(), self.role_filter))
        }
    }

    /// [`WaveOptions`] as data: the instrument table is the shared one or none, and a sampler
    /// binding (feature `sf2`) does not travel.
    #[derive(Clone, Serialize, Deserialize)]
    pub(crate) struct OptsWire {
        master: Master,
        master_gain: f64,
        voices: BTreeMap<String, VoiceSpec>,
        mods: Vec<WaveMod>,
        instruments: bool,
        role_filter: Option<RoleFilter>,
        rests: Option<RestSchedule>,
        tail: Option<MixOutTail>,
        extra_rest: Option<Entry>,
        groove: bool,
        feel: Option<GrooveTemplate>,
        drift: bool,
        voicing_seed: f64,
        record_hits: bool,
        synth_only: bool,
        bank_rate: f64,
        musicality: Fixes,
        space: Option<SpaceSpec>,
        echo: Option<EchoSpec>,
        track_balance: Option<BTreeMap<String, f64>>,
        probe_levels: BTreeMap<String, f64>,
        duck_depth: f64,
        track_energy: Option<EnergyCurve>,
        voice_design: VoiceDesign,
        sub_octave: bool,
        genre: Option<String>,
        stingers: Option<StingerPolicy>,
        turntable_brake: bool,
        track_gain: Option<f64>,
        brushes: bool,
        hf_tilt: Option<HfTilt>,
    }

    impl OptsWire {
        fn of(o: &WaveOptions) -> Option<OptsWire> {
            if o.sample_binder.is_some() || o.bank.is_some() || o.part_level.is_some() {
                return None;
            }
            let instruments = match o.instruments {
                None => false,
                Some(t) if std::ptr::eq(t, InstrumentTable::shared()) => true,
                Some(_) => return None,
            };
            // every field, so a new one does not compile until it is decided here
            let WaveOptions {
                master,
                master_gain,
                voices,
                mods,
                instruments: _,
                role_filter,
                rests,
                tail,
                extra_rest,
                groove,
                feel,
                drift,
                voicing_seed,
                record_hits,
                synth_only,
                sample_binder: _,
                bank: _,
                part_level: _,
                bank_rate,
                musicality,
                space,
                echo,
                track_balance,
                probe_levels,
                duck_depth,
                track_energy,
                voice_design,
                sub_octave,
                genre,
                stingers,
                turntable_brake,
                track_gain,
                brushes,
                hf_tilt,
            } = o.clone();
            Some(OptsWire {
                master,
                master_gain,
                voices,
                mods,
                instruments,
                role_filter,
                rests,
                tail,
                extra_rest,
                groove,
                feel,
                drift,
                voicing_seed,
                record_hits,
                synth_only,
                bank_rate,
                musicality,
                space,
                echo,
                track_balance,
                probe_levels,
                duck_depth,
                track_energy,
                voice_design,
                sub_octave,
                genre,
                stingers,
                turntable_brake,
                track_gain,
                brushes,
                hf_tilt,
            })
        }

        fn into_options(self) -> WaveOptions {
            WaveOptions {
                master: self.master,
                master_gain: self.master_gain,
                voices: self.voices,
                mods: self.mods,
                instruments: self.instruments.then(InstrumentTable::shared),
                role_filter: self.role_filter,
                rests: self.rests,
                tail: self.tail,
                extra_rest: self.extra_rest,
                groove: self.groove,
                feel: self.feel,
                drift: self.drift,
                voicing_seed: self.voicing_seed,
                record_hits: self.record_hits,
                sample_binder: None,
                synth_only: self.synth_only,
                bank: None,
                part_level: None,
                bank_rate: self.bank_rate,
                musicality: self.musicality,
                space: self.space,
                echo: self.echo,
                track_balance: self.track_balance,
                probe_levels: self.probe_levels,
                duck_depth: self.duck_depth,
                track_energy: self.track_energy,
                voice_design: self.voice_design,
                sub_octave: self.sub_octave,
                genre: self.genre,
                stingers: self.stingers,
                turntable_brake: self.turntable_brake,
                track_gain: self.track_gain,
                brushes: self.brushes,
                hf_tilt: self.hf_tilt,
            }
        }
    }
}
