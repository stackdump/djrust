//! Installing tracks the composer Worker prepared (`djrust::host::deck::transfer`) a few frames per
//! render quantum, so the worklet never stops for a track: `DjComposer::prepare` composes,
//! measures and compiles it in the Worker's instance and encodes it; here an [`Installer`] takes
//! the bytes in [`step`](Installer::step) by step (a byte budget each, one step per `process()`)
//! and loads the track once all of it is in. Loading it is a handover: nothing is composed,
//! measured or compiled on this thread.
//!
//! The newest track asked of a deck wins: beginning one drops whatever that deck had pending.
//! A restore is a job of the same kind: a fresh engine, a track installing per `Net` deck, and
//! the session stood on it ([`snapshot::stand_prepared`]) and swapped in once every one is in.

use std::collections::VecDeque;
use std::ops::Range;

use crate::session::action::DeckAction;
use crate::session::model::DeckSource;
use crate::host::deck::engine::{Engine, Renderer};
use crate::host::deck::perform::PerformCtx;
use crate::host::deck::track::{PreparedTrack, TrackSource};
use crate::host::deck::transfer::{Install, PreparedInstall};

use crate::wasm::snapshot::{self, Checked};

/// What a [`step`](Installer::step) finished.
pub enum Landed {
    /// A track is on `deck`: the load's answer.
    Track { deck: String, answer: String, bytes: usize, steps: u32 },
    /// A track did not install (it no longer fits the deck, or the bytes are bad): nothing was
    /// loaded; the caller composes it inline instead.
    Failed { deck: String, error: String },
    /// The restore is stood on `engine`: swap it in.
    Restored { engine: Engine, renderer: Renderer, cid: String, refused: Vec<String> },
}

struct Pending {
    deck: String,
    install: Install,
    steps: u32,
}

/// A restore, one step at a time like everything else on this thread: warm the memory its
/// engine will take ([`WARM_STEP`] a step), boot it (a step), take in one packed entry per step (copied out of `packed`, its header read), install
/// the tracks and the relocations a byte budget at a time, stand the session.
struct RestoreJob {
    checked: Checked,
    /// Memory still to warm before the boot, and what is warmed (freed just before it).
    warm_left: usize,
    warm: Vec<Vec<u8>>,
    /// What boots the fresh engine, until it is booted.
    boot: Option<Box<dyn FnOnce() -> (Engine, Renderer)>>,
    rig: Option<(Engine, Renderer)>,
    packed: Vec<u8>,
    /// Where each packed entry is in `packed`, not yet taken in.
    entries: VecDeque<(String, Range<usize>)>,
    installs: VecDeque<(String, Install)>,
    ready: Vec<(String, PreparedTrack)>,
    /// The decks' relocations to their saved positions, installing, then installed.
    moves: VecDeque<(String, PreparedInstall)>,
    moved: Vec<(String, PreparedInstall)>,
}

/// Memory warmed per step before a restore boots its engine. An engine's buffers (its capture
/// rings above all) are ~60 MB, which the instance grows by on the first restore; the
/// worklet's thread takes ~36 ms to touch that much fresh memory the first time (the main
/// thread ~3), so booting into it stopped the audio for ~70-100 ms. Warmed 2 MB a quantum
/// (~1 ms) first, the boot finds it ready: ~3 ms. A restore after the first one finds the
/// memory the last engine freed, already warm, and this costs a few µs a step.
pub const WARM_STEP: usize = 2 << 20;

/// `n` bytes, every page of them written.
fn touched(n: usize) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    let p = v.spare_capacity_mut();
    for i in (0..n).step_by(4096) {
        p[i].write(0);
    }
    std::hint::black_box(&v);
    v
}

/// The instance's memory, in bytes (wasm32; 0 elsewhere).
pub fn memory_bytes() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// A packed entry that is deck `name`'s relocation (`prepare_snapshot`), not its track.
pub const MOVE_PREFIX: &str = "move:";

#[derive(Default)]
pub struct Installer {
    pending: VecDeque<Pending>,
    restore: Option<RestoreJob>,
}

pub fn deck_source(t: &TrackSource) -> DeckSource {
    match t {
        TrackSource::Net(spec) => DeckSource::Net(spec.clone()),
        TrackSource::RawNets(p) => DeckSource::RawNets(p.clone()),
    }
}

impl Installer {
    /// Start installing `bytes` (a `DjComposer::prepare` track) on `deck` of `e`. Errors (an
    /// `unfit: …` track, bad bytes) mean nothing is pending for the deck now.
    pub fn begin(&mut self, e: &Engine, deck: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.cancel(deck);
        let p = e.preparer(deck).ok_or("closed")?;
        let install = Install::new(bytes, &p).map_err(|e| e.to_string())?;
        self.pending.push_back(Pending { deck: deck.into(), install, steps: 0 });
        Ok(())
    }

    /// Drop what `deck` has pending (a newer track for it is composed some other way).
    pub fn cancel(&mut self, deck: &str) {
        self.pending.retain(|p| p.deck != deck);
    }

    /// Start a restore of `checked` on a fresh engine (`boot`), with the packed tracks
    /// (`pack`) a `DjComposer::prepare_snapshot` made for its decks. What was pending is
    /// dropped: it was for the engine the restore replaces. A deck whose track is missing or
    /// does not fit composes inline when the session is stood; one whose relocation to its
    /// position (a `move:` entry) is missing or does not fit relocates inline.
    /// `boot` makes the fresh engine, once `warm` bytes are warmed (what booting one grew the
    /// memory by: [`memory_bytes`]). Only `packed`'s framing is read here.
    pub fn begin_restore(
        &mut self,
        checked: Checked,
        boot: Box<dyn FnOnce() -> (Engine, Renderer)>,
        warm: usize,
        packed: Vec<u8>,
    ) -> Result<(), String> {
        let entries = index(&packed)?.into();
        self.pending.clear();
        self.restore = Some(RestoreJob {
            checked,
            warm_left: warm,
            warm: Vec::new(),
            boot: Some(boot),
            rig: None,
            packed,
            entries,
            installs: VecDeque::new(),
            ready: Vec::new(),
            moves: VecDeque::new(),
            moved: Vec::new(),
        });
        Ok(())
    }

    pub fn busy(&self) -> bool {
        self.restore.is_some() || !self.pending.is_empty()
    }

    /// About `budget` bytes of the oldest job (a restore first), loading the track (or standing
    /// the restore) when it is all in.
    pub fn step(&mut self, e: &mut Engine, budget: usize) -> Option<Landed> {
        if let Some(job) = &mut self.restore {
            let Some((engine, _)) = job.rig.as_mut() else {
                if job.warm_left > 0 {
                    let n = job.warm_left.min(WARM_STEP);
                    job.warm_left -= n;
                    job.warm.push(touched(n));
                    return None;
                }
                // freed into the allocator, which the engine's buffers then come out of
                job.warm.clear();
                job.rig = job.boot.take().map(|b| b());
                return None;
            };
            if let Some((name, at)) = job.entries.pop_front() {
                let bytes = job.packed[at].to_vec();
                if job.entries.is_empty() {
                    job.packed = Vec::new();
                }
                if let Some(deck) = name.strip_prefix(MOVE_PREFIX) {
                    if let Some(i) = engine.preparer(deck).and_then(|p| PreparedInstall::new(bytes, &p).ok()) {
                        job.moves.push_back((deck.to_string(), i));
                    }
                    return None;
                }
                let wanted = job.checked.decks.iter().find(|d| d.0 == name).map(|d| &d.1);
                if let Some(i) = engine.preparer(&name).and_then(|p| Install::new(bytes, &p).ok()) {
                    if wanted == Some(&deck_source(i.source())) {
                        job.installs.push_back((name, i));
                    }
                }
                return None;
            }
            if let Some((name, i)) = job.installs.front_mut() {
                match i.step(budget) {
                    Ok(false) => return None,
                    Ok(true) => {
                        let (name, i) = (name.clone(), job.installs.pop_front().unwrap().1);
                        if let Some(t) = engine.preparer(&name).and_then(|p| i.finish(&p).ok()) {
                            job.ready.push((name, t));
                        }
                    }
                    // that deck composes inline when the session is stood
                    Err(_) => drop(job.installs.pop_front()),
                }
                return None;
            }
            if let Some((_, i)) = job.moves.front_mut() {
                match i.step(budget) {
                    Ok(false) => {}
                    Ok(true) => job.moved.push(job.moves.pop_front().unwrap()),
                    // that deck relocates inline when the session is stood
                    Err(_) => drop(job.moves.pop_front()),
                }
                return None;
            }
            let job = self.restore.take().unwrap();
            let (mut engine, renderer) = job.rig.unwrap();
            let refused = snapshot::stand_prepared(&mut engine, &job.checked, job.ready, snapshot::Seeks::Prepared(job.moved));
            return Some(Landed::Restored { engine, renderer, cid: job.checked.cid, refused });
        }
        let p = self.pending.front_mut()?;
        p.steps += 1;
        match p.install.step(budget) {
            Ok(false) => None,
            Ok(true) => {
                let Pending { deck, install, steps } = self.pending.pop_front().unwrap();
                let source = deck_source(install.source());
                let bytes = install.len();
                let finished = e.preparer(&deck).ok_or_else(|| "closed".to_string()).and_then(|pr| install.finish(&pr).map_err(|x| x.to_string()));
                Some(match finished {
                    Ok(track) => {
                        let a = e.perform(
                            &DeckAction::Load { deck: deck.clone(), source },
                            PerformCtx { prepared: Some(track), ..Default::default() },
                        );
                        Landed::Track { deck, answer: format!("{a:?}"), bytes, steps }
                    }
                    Err(error) => Landed::Failed { deck, error },
                })
            }
            Err(x) => {
                let deck = self.pending.pop_front().unwrap().deck;
                Some(Landed::Failed { deck, error: x.to_string() })
            }
        }
    }
}

/// The Worker's half of a restore (`DjComposer::prepare_snapshot`), on `fresh`, a fresh engine
/// booted as the worklet boots one: each `Net` deck's track, and each net deck's relocation to
/// its saved position, from the session stood on `fresh` as the worklet will stand it (on the
/// tracks as they arrive there). Packed (`pack`) for [`Installer::begin_restore`].
pub fn prepare_restore(fresh: &mut Engine, checked: &Checked) -> Result<Vec<u8>, crate::host::deck::transfer::TransferError> {
    let (mut out, mut tracks) = (Vec::new(), Vec::new());
    for (name, source, ..) in &checked.decks {
        if let DeckSource::Net(spec) = source {
            if let Some(p) = fresh.preparer(name) {
                let track = PreparedTrack::compose(spec, &p, 0);
                let bytes = crate::host::deck::transfer::encode(track, &p)?;
                tracks.push((name.clone(), crate::host::deck::transfer::decode(&bytes, &p)?));
                out.push((name.clone(), bytes));
            }
        }
    }
    let mut moves = Vec::new();
    snapshot::stand_prepared(fresh, checked, tracks, snapshot::Seeks::Prepare(&mut moves));
    out.extend(moves.into_iter().map(|(d, b)| (format!("{MOVE_PREFIX}{d}"), b)));
    Ok(pack(&out))
}

/// Named byte strings, one after another: `u32` LE name length, name, `u32` LE length, bytes.
pub fn pack(items: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, b) in items {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    }
    out
}

pub fn unpack(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    Ok(index(b)?.into_iter().map(|(name, at)| (name, b[at].to_vec())).collect())
}

/// [`unpack`]'s entries as where they are in `b`.
pub fn index(b: &[u8]) -> Result<Vec<(String, Range<usize>)>, String> {
    fn take<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8], String> {
        let s = b.get(*at..*at + n).ok_or("packed tracks cut short")?;
        *at += n;
        Ok(s)
    }
    let (mut at, mut out) = (0usize, Vec::new());
    while at < b.len() {
        let n = u32::from_le_bytes(take(b, &mut at, 4)?.try_into().unwrap()) as usize;
        let name = String::from_utf8(take(b, &mut at, n)?.to_vec()).map_err(|_| "packed tracks: bad name")?;
        let n = u32::from_le_bytes(take(b, &mut at, 4)?.try_into().unwrap()) as usize;
        take(b, &mut at, n)?;
        out.push((name, at - n..at));
    }
    Ok(out)
}
