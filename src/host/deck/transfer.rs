//! A [`PreparedTrack`] as bytes (feature `transfer`): prepared in one engine instance, installed
//! in another that only has to copy it in.
//!
//! A host whose audio thread is the only thread its engine has (djweb: the engine lives in an
//! AudioWorklet) cannot prepare a track without stopping the music: composing, dj's musicality
//! pass, compiling the net, building the lanes and measuring them take from tens of ms to
//! seconds. With this, a second instance of the engine (a Web Worker) prepares the track as a
//! deck of the first would, [`encode`]s it, and the first [`decode`]s it and loads it through
//! `perform(.load, prepared:)` as it would a track from a compose worker ([`ComposeJob`]).
//!
//! **Installing in steps**: the bytes are frames (a project net, a compiled net or a lane each),
//! and an [`Install`] takes them in [`step`](Install::step) by [`step`](Install::step) with a
//! byte budget, so a host whose audio thread does the installing spreads it over as many render
//! quanta as it needs instead of stopping for one. [`decode`] is all the steps at once.
//!
//! **What travels**: the source, the project the deck plays and the compiled graph's own copy
//! of it (what compiling it through its JSON left: not the same project, and remaking it
//! would cost the receiver more than reading it), the compiled project (graph, lanes, mix settings,
//! every measurement), the roles and sections the deck reads off it. Not the composed
//! project before dj's pass, nor the one the pass made (`PreparedTrack.project` / `.played`):
//! a deck loads neither, and a decoded track has them empty, so it is a track to load, not one
//! to [`restarted`](PreparedTrack::restarted). **What does not travel**: what belongs to the
//! process. The generation, lane uids, track serial and marking
//! mirror are taken again on arrival ([`WaveProject::installed`]); the shared band-limited
//! tables are this process's; the options are worked out again from the receiving deck
//! ([`PreparedTrack::track_options`], cheap); and a track with sampled lanes is refused (its slot
//! references are in the sender's bank).
//!
//! **Fit**: a track is prepared for what a deck compiles with. The bytes carry the preparer's
//! role filter and rate, and [`decode`] refuses a deck that differs ([`TransferError::Unfit`]),
//! so a host can compose inline instead. Same build on both ends (not a storage format).
//!
//! **A bare prepare** ([`encode_prepared`], [`PreparedInstall`]) is the same frames without the
//! track around them: a relocation's or a restart's compile ([`MoveRequest`]), which the
//! receiver hands to its deck as another start of the song it plays (the song's serial kept).
//!
//! **Offloaded requests** ([`super::offload`]): what an engine's own background compose would
//! run, as bytes another instance answers with [`answer_offload`].
//!
//! [`ComposeJob`]: super::compose_job::ComposeJob
//! [`MoveRequest`]: super::track::MoveRequest

use crate::deck::swap::{self, Latest};
use crate::deck::timeline::SectionInfo;
use crate::pflow::Project;
use crate::transfer::{self, FrameReader, Frames, ProjectBuild};
use crate::wave::runner::WaveBuild;
use crate::wave::{RoleFilter, WaveProject};
use serde::{Deserialize, Serialize};

use super::engine::Prepared;
use super::offload::{OffloadRequest, Work};
use super::track::{PreparedTrack, Preparer, TrackSource};

#[derive(Debug, Clone, PartialEq)]
pub enum TransferError {
    /// The bytes are not a track (or are from another build).
    Bytes(String),
    /// The track was prepared for a deck that compiles otherwise (role filter, rate).
    Unfit(String),
}

impl core::fmt::Display for TransferError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransferError::Bytes(e) => write!(f, "not a transferable track: {e}"),
            TransferError::Unfit(e) => write!(f, "unfit: {e}"),
        }
    }
}

impl From<transfer::Error> for TransferError {
    fn from(e: transfer::Error) -> Self {
        TransferError::Bytes(e.0)
    }
}

const MAGIC: u32 = 0x444a_5432; // "DJT2"
const PREPARED_MAGIC: u32 = 0x444a_5031; // "DJP1"
const REQUEST_MAGIC: u32 = 0x444a_4f31; // "DJO1"

#[derive(Serialize, Deserialize)]
struct Header {
    magic: u32,
    sample_rate: u64,
    role_filter: Option<RoleFilter>,
    seed: f64,
    pass_applied: bool,
    /// `project_palette` of the composed project (what [`PreparedTrack::track_options`] reads)
    palette: Option<String>,
    source: TrackSource,
    roles: Vec<(String, Vec<usize>)>,
    ring_roles: Vec<(String, Vec<(usize, String)>)>,
    sections: Vec<SectionInfo>,
    latest: Latest,
}

/// `track`, prepared by `preparer` (the deck snapshot it was prepared with), as bytes.
pub fn encode(track: PreparedTrack, preparer: &Preparer) -> Result<Vec<u8>, TransferError> {
    let PreparedTrack { source, project, prepared, opts, pass_applied, .. } = track;
    let Prepared { project: wave, source: played, roles, ring_roles, sections, latest } = prepared;
    let mut f = Frames::new();
    f.push(&Header {
        magic: MAGIC,
        sample_rate: preparer.sample_rate.to_bits(),
        role_filter: preparer.role_filter.clone(),
        seed: opts.voicing_seed,
        pass_applied,
        palette: swap::project_palette(&project),
        source,
        roles,
        ring_roles,
        sections,
        latest,
    })?;
    write_body(&played, wave, &mut f)?;
    Ok(f.bytes)
}

/// The project the deck plays and its compiled form, after a header: what [`Body`] reads.
fn write_body(played: &Project, wave: WaveProject, f: &mut Frames) -> Result<(), TransferError> {
    transfer::write_project(played, f)?;
    let mut wave_frames = Frames::new();
    let graph = wave.into_frames(&mut wave_frames)?;
    transfer::write_project(&graph, f)?;
    f.bytes.extend_from_slice(&wave_frames.bytes);
    Ok(())
}

/// The track in `bytes`, made this engine's for the deck `preparer` snapshots: ready for
/// `perform(.load, prepared:)`. Refused ([`TransferError::Unfit`]) if it was prepared for a deck
/// that compiles otherwise. All of an [`Install`] at once.
pub fn decode(bytes: &[u8], preparer: &Preparer) -> Result<PreparedTrack, TransferError> {
    let mut i = Install::new(bytes.to_vec(), preparer)?;
    while !i.step(usize::MAX)? {}
    i.finish(preparer)
}

enum Stage {
    Source(ProjectBuild),
    Graph { source: Project, b: ProjectBuild },
    Wave { source: Project, b: WaveBuild },
    Done { source: Project, wave: WaveBuild },
}

/// The frames after a header, taken in a step at a time: the project the deck plays, the
/// graph's copy of it, the compiled project.
struct Body {
    r: FrameReader,
    stage: Option<Stage>,
}

impl Body {
    fn start(mut r: FrameReader) -> Result<Body, TransferError> {
        let b = ProjectBuild::start(&mut r)?;
        Ok(Body { r, stage: Some(Stage::Source(b)) })
    }

    fn step(&mut self, budget: usize) -> Result<bool, TransferError> {
        let start = self.r.pos();
        loop {
            let stage = self.stage.take().ok_or_else(|| TransferError::Bytes("install failed".into()))?;
            let next = self.advance(stage)?;
            let done = matches!(next, Stage::Done { .. });
            self.stage = Some(next);
            if done {
                return Ok(true);
            }
            if self.r.pos() - start >= budget {
                return Ok(false);
            }
        }
    }

    fn advance(&mut self, stage: Stage) -> Result<Stage, TransferError> {
        let r = &mut self.r;
        Ok(match stage {
            Stage::Source(mut b) if !b.done() => {
                b.step(r)?;
                Stage::Source(b)
            }
            Stage::Source(b) => Stage::Graph { source: b.finish()?, b: ProjectBuild::start(r)? },
            Stage::Graph { source, mut b } if !b.done() => {
                b.step(r)?;
                Stage::Graph { source, b }
            }
            Stage::Graph { source, b } => Stage::Wave { source, b: WaveBuild::start(r, b.finish()?)? },
            Stage::Wave { source, mut b } => {
                if !b.done() {
                    b.step(r)?;
                }
                if !b.done() {
                    Stage::Wave { source, b }
                } else if !r.at_end() {
                    return Err(TransferError::Bytes("trailing bytes".into()));
                } else {
                    Stage::Done { source, wave: b }
                }
            }
            done @ Stage::Done { .. } => done,
        })
    }

    /// The project the deck plays and the compiled one, this engine's: `track_serial` keeps
    /// the song's (another start of it), None takes the next.
    fn finish(self, sample_rate: f64, track_serial: Option<i32>) -> Result<(Project, WaveProject), TransferError> {
        let Some(Stage::Done { source, wave }) = self.stage else {
            return Err(TransferError::Bytes("not all in yet".into()));
        };
        Ok((source, wave.finish(sample_rate, track_serial)?))
    }
}

/// What the deck reads of the project once it is loaded, with the numbers this process gave
/// it on arrival: the sender's generation and serial mean nothing here (a relocation prepared
/// later takes the serial from it).
fn latest_here(mut latest: Latest, wave: &WaveProject) -> Latest {
    latest.generation = wave.generation;
    latest.track_serial = wave.track_serial;
    latest
}

/// A track being installed from [`encode`]'s bytes a few frames at a time: [`new`](Self::new)
/// (the header: the fit is checked here), [`step`](Self::step) until it says done,
/// [`finish`](Self::finish).
pub struct Install {
    header: Header,
    body: Body,
    len: usize,
}

impl Install {
    pub fn new(bytes: Vec<u8>, preparer: &Preparer) -> Result<Install, TransferError> {
        let len = bytes.len();
        let mut r = FrameReader::new(bytes);
        let header: Header = r.next()?;
        if header.magic != MAGIC {
            return Err(TransferError::Bytes("bad magic".into()));
        }
        fit(&header, preparer)?;
        Ok(Install { header, body: Body::start(r)?, len })
    }

    /// What the track is (its spec, or a raw-`nets` share).
    pub fn source(&self) -> &TrackSource {
        &self.header.source
    }

    /// Bytes taken in so far, of [`len`](Self::len).
    pub fn pos(&self) -> usize {
        self.body.r.pos()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Takes frames in until about `budget` bytes of them are taken (at least one step's
    /// worth). True once everything is in.
    pub fn step(&mut self, budget: usize) -> Result<bool, TransferError> {
        self.body.step(budget)
    }

    /// The track, this engine's ([`WaveProject::installed`](crate::wave::WaveProject::installed)),
    /// with the options the deck `preparer` snapshots works out for it.
    /// The fit is checked again: the deck may have changed (a role switched off) while the
    /// steps ran.
    pub fn finish(self, preparer: &Preparer) -> Result<PreparedTrack, TransferError> {
        fit(&self.header, preparer)?;
        let (played, wave) = self.body.finish(preparer.sample_rate, None)?;
        let h = self.header;
        let opts = PreparedTrack::track_options(&h.source, h.palette.as_deref(), h.seed, preparer);
        let fixes = opts.musicality;
        let latest = latest_here(h.latest, &wave);
        Ok(PreparedTrack {
            source: h.source,
            prepared: Prepared {
                project: wave,
                source: played,
                roles: h.roles,
                ring_roles: h.ring_roles,
                sections: h.sections,
                latest,
            },
            project: Project::default(),
            played: Project::default(),
            opts,
            fixes,
            pass_applied: h.pass_applied,
        })
    }
}

/// Would the deck `preparer` snapshots compile the track `h` describes as it was compiled?
fn fit(h: &Header, preparer: &Preparer) -> Result<(), TransferError> {
    fit_parts(h.sample_rate, &h.role_filter, preparer)
}

fn fit_parts(sample_rate: u64, role_filter: &Option<RoleFilter>, preparer: &Preparer) -> Result<(), TransferError> {
    if sample_rate != preparer.sample_rate.to_bits() {
        return Err(TransferError::Unfit(format!(
            "prepared at {} Hz, the deck runs at {} Hz",
            f64::from_bits(sample_rate),
            preparer.sample_rate
        )));
    }
    if *role_filter != preparer.role_filter {
        return Err(TransferError::Unfit(format!(
            "prepared with role filter {:?}, the deck has {:?}",
            role_filter, preparer.role_filter
        )));
    }
    Ok(())
}

// MARK: a bare prepare

#[derive(Serialize, Deserialize)]
struct PreparedHeader {
    magic: u32,
    sample_rate: u64,
    role_filter: Option<RoleFilter>,
    roles: Vec<(String, Vec<usize>)>,
    ring_roles: Vec<(String, Vec<(usize, String)>)>,
    sections: Vec<SectionInfo>,
    latest: Latest,
}

/// A compile's result alone ([`Preparer::prepare_opts`]: a relocation, a restart), prepared by
/// `preparer`, as bytes.
pub fn encode_prepared(prepared: Prepared, preparer: &Preparer) -> Result<Vec<u8>, TransferError> {
    let Prepared { project: wave, source: played, roles, ring_roles, sections, latest } = prepared;
    let mut f = Frames::new();
    f.push(&PreparedHeader {
        magic: PREPARED_MAGIC,
        sample_rate: preparer.sample_rate.to_bits(),
        role_filter: preparer.role_filter.clone(),
        roles,
        ring_roles,
        sections,
        latest,
    })?;
    write_body(&played, wave, &mut f)?;
    Ok(f.bytes)
}

/// [`encode_prepared`]'s bytes installed a step at a time, as [`Install`] does a track's.
pub struct PreparedInstall {
    header: PreparedHeader,
    body: Body,
    len: usize,
}

impl PreparedInstall {
    pub fn new(bytes: Vec<u8>, preparer: &Preparer) -> Result<PreparedInstall, TransferError> {
        let len = bytes.len();
        let mut r = FrameReader::new(bytes);
        let header: PreparedHeader = r.next()?;
        if header.magic != PREPARED_MAGIC {
            return Err(TransferError::Bytes("bad magic".into()));
        }
        fit_parts(header.sample_rate, &header.role_filter, preparer)?;
        Ok(PreparedInstall { header, body: Body::start(r)?, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn step(&mut self, budget: usize) -> Result<bool, TransferError> {
        self.body.step(budget)
    }

    /// The compile, this engine's, as another start of the song `track_serial` numbers (None:
    /// a song of its own).
    pub fn finish(self, preparer: &Preparer, track_serial: Option<i32>) -> Result<Prepared, TransferError> {
        fit_parts(self.header.sample_rate, &self.header.role_filter, preparer)?;
        let (played, wave) = self.body.finish(preparer.sample_rate, track_serial)?;
        let h = self.header;
        let latest = latest_here(h.latest, &wave);
        Ok(Prepared { project: wave, source: played, roles: h.roles, ring_roles: h.ring_roles, sections: h.sections, latest })
    }
}

// MARK: offloaded requests

#[derive(Serialize, Deserialize)]
struct RequestHeader {
    magic: u32,
    preparer: super::offload::PreparerWire,
    work: super::offload::WorkWire,
}

/// An offloaded compose as bytes: the preparer and what to run with it, and for a bare
/// prepare the project it compiles.
pub(crate) fn encode_request(req: &OffloadRequest) -> Result<Vec<u8>, TransferError> {
    let mut f = Frames::new();
    let (wire, project) = super::offload::work_wire(&req.work);
    f.push(&RequestHeader { magic: REQUEST_MAGIC, preparer: req.preparer_wire.clone(), work: wire })?;
    if let Some(p) = project {
        transfer::write_project(p, &mut f)?;
    }
    Ok(f.bytes)
}

/// Run an offloaded request (in the instance that composes for the engine that asked): the
/// bytes [`Engine::offload_answer`](super::engine::Engine::offload_answer) takes. A track
/// source that makes no track answers empty bytes.
pub fn answer_offload(request: &[u8]) -> Result<Vec<u8>, TransferError> {
    let mut r = FrameReader::new(request.to_vec());
    let h: RequestHeader = r.next()?;
    if h.magic != REQUEST_MAGIC {
        return Err(TransferError::Bytes("not an offload request".into()));
    }
    let preparer = h.preparer.preparer()?;
    let project = match h.work.needs_project() {
        true => {
            let mut b = ProjectBuild::start(&mut r)?;
            while !b.done() {
                b.step(&mut r)?;
            }
            Some(b.finish()?)
        }
        false => None,
    };
    if !r.at_end() {
        return Err(TransferError::Bytes("trailing bytes".into()));
    }
    match h.work.into_work(project)? {
        Work::Track { source, start_tick } => match PreparedTrack::from_source(&source, &preparer, start_tick) {
            Some(track) => encode(track, &preparer),
            None => Ok(Vec::new()),
        },
        Work::Prepare { project, start_tick, opts, track_serial } => {
            let prepared = preparer.prepare_opts(&project, start_tick, opts, track_serial);
            encode_prepared(prepared, &preparer)
        }
    }
}
