//! Port of `Beats/Wave/MarkingMirror.swift`: a copy of every net's place tokens (and which nets
//! are muted) that the audio thread publishes as the net clock runs, and any other thread reads
//! without locks, without allocating on the audio side, and without ever seeing half of one
//! state and half of another. The feed draws live tokens from it; it never calls
//! `WaveGraph::marking` (which allocates maps) off a stopped graph.
//!
//! Layout. What the numbers mean is fixed per project and built on the main thread when the
//! project is prepared ([`MarkingLayout`], immutable): net order, and each net's place offsets
//! in one flat array of tokens. A snapshot always comes with the layout of the very project
//! that produced it.
//!
//! Publishing. Each project owns a [`MarkingMirror`]: two buffers and a seqlock. The audio
//! thread, the only writer, fills the buffer that is *not* the newest, bracketing the write
//! with a per-buffer stamp (odd while writing), then flips `newest`. A reader copies the newest
//! buffer and re-reads its stamp: if a writer got in during the copy the stamp moved and the
//! reader retries. Nothing here allocates, locks, or retains. The audio thread only ever
//! writes into the mirror of the project it is running; a project swap is just the audio
//! thread switching to another mirror, so a reader can never pair one project's layout with
//! another project's tokens.
//!
//! Swift keeps the buffers as plain memory under the seqlock; here every word is an atomic
//! accessed `Relaxed` between the same fences, which is the textbook seqlock for a memory model
//! that forbids racing plain reads (the protocol, order and values are unchanged).
//!
//! The pieces, with the Swift they mirror:
//!
//! - [`NetInfo`], [`MarkingLayout`], [`MarkingHeader`], [`MarkingSnapshot`], [`MarkingMirror`]
//!   are the types of the same names;
//! - [`MarkingBook`] is the main thread's side of `WaveDeckSource` (`mirrors`, `remember`,
//!   `mirror(generation:)`, `markingLayout(generation:)`, `readMarking`);
//! - [`next_track_serial`] is `WaveProject.tracks`.
//!
//! As in Swift, `WaveProject.marking` is a mirror per project, created at prepare time and
//! written by `WaveRunner.publishMarking(endFrame:)` after each block: `WaveProject` owns its
//! [`MarkingMirror`], `WaveRunner::publish_marking` publishes the running graph into it, and the
//! deck's [`MarkingBook`] remembers it when the project is loaded. Tests and offline hosts that
//! have no project can still build a [`MarkingMirror`] from a graph and call
//! [`MarkingMirror::publish`] themselves.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{fence, AtomicI32, AtomicI64, AtomicU64, AtomicU8, Ordering};

use super::net::{CompiledNet, WaveGraph};
use super::synth::is_drum_channel;
use crate::deck::timeline::SectionInfo;
use crate::pflow::{NetBundle, Project};

/// `WaveProject.tracks`: a process-wide, 1-based counter. A new track gets the next number;
/// relaunching the same song (a section launch, a jump to a tick) keeps its serial.
pub fn next_track_serial() -> i32 {
    static TRACKS: AtomicI32 = AtomicI32::new(0);
    TRACKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

/// One arc of a net, end to end, in the net's coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct NetArcGeometry {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// What a display needs to know about one net of a project, fixed once the project is
/// prepared: which lane it belongs on, how to colour and place it. Everything an event's
/// `net` / `transition` indices (or a snapshot's net order) refer to.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct NetInfo {
    pub id: String,
    /// "music", or "control" (a control net steers other nets and never sounds).
    pub role: String,
    /// The lane merge key: the riff group the net belongs to ("" for a net on its own).
    pub riff_group: String,
    pub riff_variant: String,
    /// The track's group ("kick", "snare", "bass", "melody", ...) if it has one.
    pub group: Option<String>,
    pub instrument: String,
    /// The track's MIDI channel; 10...15 are drums. (A binding may override it per transition:
    /// `FireEvent::channel` is the channel that actually played.)
    pub channel: i64,
    pub is_drum: bool,
    /// A one-bar accent the performer fires (`hit1`...).
    pub is_stinger: bool,
    /// Lowest and highest MIDI note over the net's transitions that have a voice; both -1 for
    /// a net that never sounds.
    pub note_low: i64,
    pub note_high: i64,
    /// A fire's `transition` indexes these (and `transition_x` / `transition_y`).
    pub transition_ids: Vec<String>,
    /// The net's own ring layout, as composed (the compiled graph drops coordinates, so these
    /// come from the project that was prepared): positions in the order of the net's place ids
    /// (the layout's) and of `transition_ids`. All zero for a project that carries no
    /// coordinates.
    pub place_x: Vec<f64>,
    pub place_y: Vec<f64>,
    pub transition_x: Vec<f64>,
    pub transition_y: Vec<f64>,
    /// Per transition (the order of `transition_ids`): does it carry a note (a MIDI binding)?
    pub transition_has_midi: Vec<bool>,
    /// The net's arcs as the stage draws them: from the centre of the source node to the centre
    /// of the target node, in the net's own coordinates. An arc with an endpoint that has no
    /// coordinates is left out. Empty for a control net: the stage never draws one.
    pub arcs: Vec<NetArcGeometry>,
    /// How many arcs the net has, drawn or not (the stage's stats line counts them all).
    pub arc_count: usize,
}

impl NetInfo {
    /// `NetInfo.init(_:isStinger:geometry:)`. `n` is a net of a graph whose lanes are built
    /// (`lane` says which transitions have a voice); `geometry` is the same net of the project
    /// that was prepared (the one with coordinates), if any.
    pub fn new(n: &CompiledNet, is_stinger: bool, geometry: Option<&NetBundle>) -> NetInfo {
        let mut low = i64::MAX;
        let mut high = i64::MIN;
        for t in 0..n.n_trans {
            if n.lane[t] >= 0 {
                low = low.min(n.note[t]);
                high = high.max(n.note[t]);
            }
        }
        let place_xy = |p: &String| geometry.and_then(|g| g.places.get(p)).map(|p| (p.x, p.y));
        let trans_xy = |t: &String| {
            geometry
                .and_then(|g| g.transitions.get(t))
                .map(|t| (t.x, t.y))
        };
        let mut drawn: Vec<NetArcGeometry> = Vec::new();
        if let Some(geometry) = geometry {
            if n.bundle.role != "control" {
                // stage.js: `net.places[arc.source] || net.transitions[arc.source]`.
                let point = |id: &String| place_xy(id).or_else(|| trans_xy(id));
                for arc in &geometry.arcs {
                    let (Some(a), Some(b)) = (point(&arc.source), point(&arc.target)) else {
                        continue;
                    };
                    drawn.push(NetArcGeometry {
                        x1: a.0,
                        y1: a.1,
                        x2: b.0,
                        y2: b.1,
                    });
                }
            }
        }
        NetInfo {
            id: n.id.clone(),
            role: n.bundle.role.clone(),
            riff_group: n.bundle.riff_group.clone(),
            riff_variant: n.bundle.riff_variant.clone(),
            group: n.bundle.track.group.clone(),
            instrument: n.bundle.track.instrument.clone(),
            channel: n.bundle.track.channel,
            is_drum: is_drum_channel(n.bundle.track.channel),
            is_stinger,
            note_low: if low == i64::MAX { -1 } else { low },
            note_high: if high == i64::MIN { -1 } else { high },
            transition_ids: n.trans_ids.clone(),
            place_x: n
                .place_ids
                .iter()
                .map(|p| place_xy(p).map_or(0.0, |v| v.0))
                .collect(),
            place_y: n
                .place_ids
                .iter()
                .map(|p| place_xy(p).map_or(0.0, |v| v.1))
                .collect(),
            transition_x: n
                .trans_ids
                .iter()
                .map(|t| trans_xy(t).map_or(0.0, |v| v.0))
                .collect(),
            transition_y: n
                .trans_ids
                .iter()
                .map(|t| trans_xy(t).map_or(0.0, |v| v.1))
                .collect(),
            transition_has_midi: n.midi.clone(),
            arcs: drawn,
            arc_count: geometry.map_or(n.bundle.arcs.len(), |g| g.arcs.len()),
        }
    }
}

/// Where every net's places sit in the flat token array of one project, and what each net is.
/// Immutable; built on the main thread at prepare time. Share it by `Arc`.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct MarkingLayout {
    /// The project this describes (`WaveProject::generation`).
    pub generation: i32,
    /// Which song this project plays (`WaveProject.trackSerial`): equal across a section launch
    /// or jump of the same song, different for a new track.
    pub track_serial: i32,
    /// The tick this project was seeded at (`WaveProject::start_tick`): 0 from the top.
    pub start_tick: i64,
    /// Net ids in `WaveGraph::nets` order (project order); an event's `net` indexes this.
    pub net_ids: Vec<String>,
    /// What each net is, in the same order as `net_ids`.
    pub nets: Vec<NetInfo>,
    /// The song's sections with their start ticks, as of this project (empty for a loop).
    pub sections: Vec<SectionInfo>,
    /// Place ids of net `i`, in the order of its tokens.
    pub place_ids: Vec<Vec<String>>,
    /// `place_offsets[i]..place_offsets[i + 1]` are net `i`'s slots in the flat token array.
    pub place_offsets: Vec<usize>,
    /// The project's length in bars as the composer recorded it (0 when it did not).
    pub bars: i64,
}

impl MarkingLayout {
    /// `MarkingLayout.init(generation:trackSerial:startTick:graph:geometry:)`: `graph` is the
    /// prepared project's graph (lanes built), `geometry` the project it was prepared from.
    pub fn new(
        generation: i32,
        track_serial: i32,
        start_tick: i64,
        graph: &WaveGraph,
        geometry: &Project,
    ) -> MarkingLayout {
        let mut offsets = vec![0usize];
        for n in &graph.nets {
            offsets.push(offsets[offsets.len() - 1] + n.n_places);
        }
        MarkingLayout {
            generation,
            track_serial,
            start_tick,
            net_ids: graph.nets.iter().map(|n| n.id.clone()).collect(),
            nets: graph
                .nets
                .iter()
                .map(|n| NetInfo::new(n, graph.is_stinger_net[n.index], geometry.nets.get(&n.id)))
                .collect(),
            sections: SectionInfo::sections_of(geometry),
            place_ids: graph.nets.iter().map(|n| n.place_ids.clone()).collect(),
            place_offsets: offsets,
            bars: geometry.bars.unwrap_or(0),
        }
    }

    pub fn net_count(&self) -> usize {
        self.net_ids.len()
    }

    /// Total places over all nets: the length of a snapshot's `tokens`.
    pub fn place_count(&self) -> usize {
        self.place_offsets.last().copied().unwrap_or(0)
    }

    /// Net `i`'s slots in a snapshot's `tokens`.
    pub fn places_of_net(&self, i: usize) -> Range<usize> {
        self.place_offsets[i]..self.place_offsets[i + 1]
    }
}

/// Which snapshot you got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkingHeader {
    /// The project it belongs to; its layout is the snapshot's own `layout`.
    pub generation: i32,
    /// Counts this project's publishes: 1 is the initial state, then +1 for every change (a
    /// tick, a mute). Equal sequence = same snapshot.
    pub sequence: i64,
    /// The graph's tick counter (`FireEvent::tick`'s numbering).
    pub tick: i64,
    /// Deck frame at the end of the render block that first showed this marking (0 for the
    /// initial state seeded at prepare time).
    pub frame: i64,
}

/// A reusable main-thread buffer for one snapshot. Fill it with [`MarkingMirror::read_into`];
/// the arrays are sized on first use and whenever the project (so the layout) changes, and
/// reused otherwise.
#[derive(Clone, Debug, Default)]
pub struct MarkingSnapshot {
    pub header: MarkingHeader,
    /// The layout `tokens` / `muted` are indexed by (the snapshot's own project).
    pub layout: Option<Arc<MarkingLayout>>,
    /// Tokens of every place, net after net (see [`MarkingLayout::places_of_net`]).
    pub tokens: Vec<f64>,
    /// 1 where the net is muted, one entry per net.
    pub muted: Vec<u8>,
}

impl MarkingSnapshot {
    pub fn new() -> MarkingSnapshot {
        MarkingSnapshot::default()
    }

    pub fn generation(&self) -> i32 {
        self.header.generation
    }

    pub fn tick(&self) -> i64 {
        self.header.tick
    }

    /// Tokens of net `i`'s places, in `layout.place_ids[i]` order (empty for an unknown net).
    pub fn tokens_of_net(&self, i: usize) -> &[f64] {
        match &self.layout {
            Some(l) if i < l.net_count() => &self.tokens[l.places_of_net(i)],
            _ => &[],
        }
    }

    pub fn is_muted(&self, i: usize) -> bool {
        i < self.muted.len() && self.muted[i] != 0
    }

    /// Token count of one place by ids, None if either is unknown. For display and tests; a
    /// per-frame draw should walk `tokens` by offset.
    pub fn token_at(&self, net_id: &str, place_id: &str) -> Option<f64> {
        let l = self.layout.as_ref()?;
        let n = l.net_ids.iter().position(|id| id == net_id)?;
        let p = l.place_ids[n].iter().position(|id| id == place_id)?;
        Some(self.tokens[l.place_offsets[n] + p])
    }
}

/// The published copy of one project's marking. See the module comment for the protocol.
///
/// Threading: [`publish`](Self::publish) is for the one thread that runs the project (the
/// audio thread); [`read`](Self::read) for any thread, any number of them. Created (and
/// seeded) on the preparing thread before the project is shared.
pub struct MarkingMirror {
    pub layout: Arc<MarkingLayout>,
    net_count: usize,
    place_count: usize,
    // Two buffers each: tokens (f64 bits), muted flags, [tick, frame, sequence].
    tokens: Box<[AtomicU64]>,
    muted: Box<[AtomicU8]>,
    meta: [AtomicI64; 6],
    // Per-buffer seqlock stamps: even = stable, odd = being written.
    stamp: [AtomicI64; 2],
    /// Index of the newest complete buffer, -1 before anything is published.
    newest: AtomicI64,
    publishes: AtomicI64,
    /// Test seam: runs in the middle of a reader's copy, to model a reader that gets preempted
    /// there while the writer laps it. None in production.
    mid_copy_hook: Option<Box<dyn Fn() + Send + Sync>>,
    retry_count: AtomicI64,
}

impl MarkingMirror {
    /// `MarkingMirror.init`: lays the project out and seeds the mirror with `graph`'s initial
    /// state (a prepared project's, or the one rebuilt for a section launch), so it is
    /// readable before the first render.
    pub fn new(
        generation: i32,
        track_serial: i32,
        start_tick: i64,
        graph: &WaveGraph,
        geometry: &Project,
    ) -> MarkingMirror {
        let layout = MarkingLayout::new(generation, track_serial, start_tick, graph, geometry);
        let m = MarkingMirror::unseeded(layout);
        m.publish(graph, 0); // seed: the initial state is readable before the first render
        m
    }

    /// A mirror on `layout` with nothing published yet: [`new`](Self::new) seeds it at once; a
    /// transferred project's is seeded when it is installed ([`WaveProject::installed`]).
    ///
    /// [`WaveProject::installed`]: super::runner::WaveProject::installed
    pub fn unseeded(layout: MarkingLayout) -> MarkingMirror {
        let layout = Arc::new(layout);
        let net_count = layout.net_count();
        let place_count = layout.place_count();
        MarkingMirror {
            layout,
            net_count,
            place_count,
            tokens: (0..2 * place_count.max(1))
                .map(|_| AtomicU64::new(0))
                .collect(),
            muted: (0..2 * net_count.max(1)).map(|_| AtomicU8::new(0)).collect(),
            meta: Default::default(),
            stamp: Default::default(),
            newest: AtomicI64::new(-1),
            publishes: AtomicI64::new(0),
            mid_copy_hook: None,
            retry_count: AtomicI64::new(0),
        }
    }

    /// Test seam (`midCopyHook`): `f` runs in the middle of every reader's copy. Set it before
    /// the mirror is shared.
    pub fn set_mid_copy_hook(&mut self, f: impl Fn() + Send + Sync + 'static) {
        self.mid_copy_hook = Some(Box::new(f));
    }

    /// How many times a reader's copy was invalidated by the writer and redone.
    pub fn retries(&self) -> i64 {
        self.retry_count.load(Ordering::Relaxed)
    }

    /// Writer. Publish `graph`'s marking, mutes and tick as the state at the end of the render
    /// block that ends at `end_frame`, if it differs from what is already published. Returns
    /// whether it published. Allocation-free. `graph` must be the graph this mirror was built
    /// from (same nets, same places).
    pub fn publish(&self, graph: &WaveGraph, end_frame: i64) -> bool {
        let cur = self.newest.load(Ordering::Relaxed); // only this thread writes it
        if cur >= 0 && self.unchanged(graph, cur as usize) {
            return false;
        }
        let w = if cur == 0 { 1 } else { 0 };
        let seq = self.publishes.load(Ordering::Relaxed) + 1;
        self.stamp[w].fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release); // "being written" is visible before the data changes
        let tw = w * self.place_count;
        let mut o = 0;
        for n in &graph.nets {
            for p in 0..n.n_places {
                self.tokens[tw + o + p].store(n.state[p].to_bits(), Ordering::Relaxed);
            }
            o += n.n_places;
        }
        let mw = w * self.net_count;
        for i in 0..self.net_count {
            self.muted[mw + i].store(graph.muted[i], Ordering::Relaxed);
        }
        self.meta[w * 3].store(graph.tick, Ordering::Relaxed);
        self.meta[w * 3 + 1].store(end_frame, Ordering::Relaxed);
        self.meta[w * 3 + 2].store(seq, Ordering::Relaxed);
        fence(Ordering::Release); // the data is complete before the stamp says so
        self.stamp[w].fetch_add(1, Ordering::Release);
        self.newest.store(w as i64, Ordering::Release);
        self.publishes.store(seq, Ordering::Relaxed);
        true
    }

    fn unchanged(&self, graph: &WaveGraph, b: usize) -> bool {
        if self.meta[b * 3].load(Ordering::Relaxed) != graph.tick {
            return false;
        }
        let mb = b * self.net_count;
        for i in 0..self.net_count {
            if self.muted[mb + i].load(Ordering::Relaxed) != graph.muted[i] {
                return false;
            }
        }
        let tb = b * self.place_count;
        let mut o = 0;
        for n in &graph.nets {
            for p in 0..n.n_places {
                if f64::from_bits(self.tokens[tb + o + p].load(Ordering::Relaxed)) != n.state[p] {
                    return false;
                }
            }
            o += n.n_places;
        }
        true
    }

    /// Reader. Copy the newest snapshot into `out` (`layout.place_count()` entries at least)
    /// and `out_muted` (`layout.net_count()`). Returns None if the buffers are too small, if
    /// nothing is published, or if a writer kept lapping the copy (not seen in practice: it
    /// retries). Never blocks, never allocates.
    pub fn read(&self, out: &mut [f64], out_muted: &mut [u8]) -> Option<MarkingHeader> {
        if out.len() < self.place_count || out_muted.len() < self.net_count {
            return None;
        }
        for _ in 0..16 {
            let b = self.newest.load(Ordering::Acquire);
            if b < 0 {
                return None;
            }
            let b = b as usize;
            let s1 = self.stamp[b].load(Ordering::Acquire);
            if s1 & 1 != 0 {
                continue;
            }
            if self.place_count > 0 {
                let half = self.place_count / 2;
                let base = b * self.place_count;
                for (o, w) in out[..half].iter_mut().zip(&self.tokens[base..base + half]) {
                    *o = f64::from_bits(w.load(Ordering::Relaxed));
                }
                if let Some(hook) = &self.mid_copy_hook {
                    hook();
                }
                for (o, w) in out[half..self.place_count]
                    .iter_mut()
                    .zip(&self.tokens[base + half..base + self.place_count])
                {
                    *o = f64::from_bits(w.load(Ordering::Relaxed));
                }
            }
            if self.net_count > 0 {
                let base = b * self.net_count;
                for (o, w) in out_muted[..self.net_count]
                    .iter_mut()
                    .zip(&self.muted[base..base + self.net_count])
                {
                    *o = w.load(Ordering::Relaxed);
                }
            }
            let tick = self.meta[b * 3].load(Ordering::Relaxed);
            let frame = self.meta[b * 3 + 1].load(Ordering::Relaxed);
            let seq = self.meta[b * 3 + 2].load(Ordering::Relaxed);
            fence(Ordering::Acquire); // the copy is done before we look at the stamp again
            let s2 = self.stamp[b].load(Ordering::Relaxed);
            if s1 == s2 {
                return Some(MarkingHeader {
                    generation: self.layout.generation,
                    sequence: seq,
                    tick,
                    frame,
                });
            }
            self.retry_count.fetch_add(1, Ordering::Relaxed);
        }
        None
    }

    /// Reader. [`read`](Self::read) into a reusable snapshot (arrays resized only if the layout
    /// differs from what the snapshot last held).
    pub fn read_into(&self, snap: &mut MarkingSnapshot) -> bool {
        if snap.tokens.len() != self.place_count {
            snap.tokens = vec![0.0; self.place_count];
        }
        if snap.muted.len() != self.net_count {
            snap.muted = vec![0; self.net_count];
        }
        let Some(h) = self.read(&mut snap.tokens, &mut snap.muted) else {
            return false;
        };
        snap.header = h;
        snap.layout = Some(self.layout.clone());
        true
    }
}

/// The main thread's book of recently loaded projects' mirrors, oldest first, so a snapshot or
/// an event can be matched to the layout of the project that produced it even after newer
/// projects have been loaded (`WaveDeckSource.mirrors`). Not the projects themselves (about
/// 6 MB each for a big song): a mirror with its layout is about 0.35 MB (measured in Swift,
/// 6000-place techno), 8 of them a few MB per deck.
#[derive(Default)]
pub struct MarkingBook {
    mirrors: Vec<Arc<MarkingMirror>>,
}

impl MarkingBook {
    /// How many mirrors a book keeps (`WaveDeckSource.mirrorsKept`).
    pub const KEPT: usize = 8;

    pub fn new() -> MarkingBook {
        MarkingBook::default()
    }

    /// `WaveDeckSource.remember`: a project was loaded. Drops the oldest mirrors over
    /// [`KEPT`](Self::KEPT), but never the one the audio thread is running (`active_generation`)
    /// or the newest.
    pub fn remember(&mut self, m: Arc<MarkingMirror>, active_generation: i32) {
        self.mirrors.push(m);
        let mut i = 0;
        while self.mirrors.len() > Self::KEPT && i < self.mirrors.len() - 1 {
            if self.mirrors[i].layout.generation == active_generation {
                i += 1;
            } else {
                self.mirrors.remove(i);
            }
        }
    }

    /// `WaveDeckSource.mirror(generation:)`: the newest mirror of that project, if it is still
    /// kept.
    pub fn mirror(&self, generation: i32) -> Option<&Arc<MarkingMirror>> {
        self.mirrors
            .iter()
            .rev()
            .find(|m| m.layout.generation == generation)
    }

    /// How many mirrors the book holds (at most [`KEPT`](Self::KEPT), or one more while the
    /// running project is older than the newest ones).
    pub fn len(&self) -> usize {
        self.mirrors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mirrors.is_empty()
    }

    /// The most recently loaded project's mirror.
    pub fn latest(&self) -> Option<&Arc<MarkingMirror>> {
        self.mirrors.last()
    }

    /// `WaveDeckSource.markingLayout`: the layout of the most recently loaded project.
    pub fn latest_layout(&self) -> Option<Arc<MarkingLayout>> {
        self.latest().map(|m| m.layout.clone())
    }

    /// `WaveDeckSource.markingLayout(generation:)`: the layout of a recent project by
    /// generation (the active one, the latest, and a few older), e.g. the generation stamped
    /// on a `FireEvent` or a `TickAnchor`.
    pub fn layout(&self, generation: i32) -> Option<Arc<MarkingLayout>> {
        self.mirror(generation).map(|m| m.layout.clone())
    }

    /// `WaveDeckSource.readMarking(into:)`: copy the marking of the project that is sounding
    /// into `snap`. On a deck that is not playing, a project loaded since the last render is
    /// the one reported (its initial marking, or the one rebuilt for a section launch) because
    /// it takes over at the next block; on a playing deck the old project is reported until it
    /// hands over at the bar. `active_generation` and `pending_generation` are the audio
    /// thread's published `WaveRunner::generation` / `pending_generation`, `is_playing` its
    /// published `playing`. False if nothing is loaded.
    ///
    /// Never touches the audio thread's graph.
    pub fn read_marking(
        &self,
        snap: &mut MarkingSnapshot,
        active_generation: i32,
        is_playing: bool,
        pending_generation: i32,
    ) -> bool {
        let mut m = if active_generation != 0 {
            self.mirror(active_generation)
        } else {
            None
        };
        // A project the audio thread has not seen yet, on a deck that is not playing, is
        // installed at once by the next block: it is the one to show. (One already queued for a
        // bar is not.)
        if let Some(newest) = self.latest() {
            let g = newest.layout.generation;
            if !is_playing && g != active_generation && g != pending_generation {
                m = Some(newest);
            }
        }
        let Some(m) = m.or(self.latest()) else {
            return false;
        };
        m.read_into(snap)
    }

    /// `WaveDeckSource.readMarking(generation:into:)`: for a particular project (a recent
    /// generation). False if unknown.
    pub fn read_marking_of(&self, generation: i32, snap: &mut MarkingSnapshot) -> bool {
        match self.mirror(generation) {
            Some(m) => m.read_into(snap),
            None => false,
        }
    }
}

/// `#[serde(with)]` for a project's mirror: it travels as its layout and arrives unseeded (the
/// atomics are this engine's own; [`WaveProject::installed`] seeds them).
///
/// [`WaveProject::installed`]: super::runner::WaveProject::installed
#[cfg(feature = "transfer")]
pub(crate) mod transfer {
    //! The mirror on the wire is its layout without the per-net parts (`net_ids`, `nets`,
    //! `place_ids`), which travel a frame per net after the project
    //! (`WaveProject::into_frames`); it arrives unseeded, to be filled in and installed.
    use super::{MarkingLayout, MarkingMirror, SectionInfo};
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize)]
    struct ShellRef<'a> {
        generation: i32,
        track_serial: i32,
        start_tick: i64,
        sections: &'a Vec<SectionInfo>,
        place_offsets: &'a Vec<usize>,
        bars: i64,
    }

    #[derive(Deserialize)]
    struct Shell {
        generation: i32,
        track_serial: i32,
        start_tick: i64,
        sections: Vec<SectionInfo>,
        place_offsets: Vec<usize>,
        bars: i64,
    }

    pub fn serialize<S: Serializer>(m: &Arc<MarkingMirror>, s: S) -> Result<S::Ok, S::Error> {
        let l = &m.layout;
        ShellRef {
            generation: l.generation,
            track_serial: l.track_serial,
            start_tick: l.start_tick,
            sections: &l.sections,
            place_offsets: &l.place_offsets,
            bars: l.bars,
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<MarkingMirror>, D::Error> {
        let s = Shell::deserialize(d)?;
        Ok(Arc::new(MarkingMirror::unseeded(MarkingLayout {
            generation: s.generation,
            track_serial: s.track_serial,
            start_tick: s.start_tick,
            net_ids: Vec::new(),
            nets: Vec::new(),
            sections: s.sections,
            place_ids: Vec::new(),
            place_offsets: s.place_offsets,
            bars: s.bars,
        })))
    }
}
