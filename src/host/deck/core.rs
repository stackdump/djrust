//! One deck on the render thread. As a net deck (`WaveDeckSource.swift` + the net half of
//! `DeckRenderer.swift`): the wave runner, its samplers, the master chain with the macro
//! automation that drives it, the FX unit, the vinyl brake and the limiter, in the order
//! Swift renders them. As a sample deck (the sample half of `DeckRenderer.swift`): the turntable
//! and the clip's FX through the deck's own master bus (`LoopMasterBus`) and a safety limiter.
//! Either way the deck's loudness meter hears the block and the mirrors are published.
//!
//! Real-time rules, as in Swift: [`DeckCore::render`] allocates nothing, takes no lock and
//! frees nothing (measured by `tests/deck_rt.rs` with a counting allocator). Everything else
//! reaches it through the command ring ([`Command`]), and whatever it lets go of goes back
//! through the retire ring.

use std::collections::VecDeque;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::deck::level_match::LoudnessMeter;
use crate::deck::turntable::{SampleBuffer, Turntable, TurntableCommand};
use crate::fx::brake::BrakeSource;
use crate::fx::brake::TurntableBrake;
use crate::fx::unit::FxUnitCommand;
use crate::fx::unit::FxUnitLayer;
use crate::fx::{
    FxAutomation, FxAutomationEvent, LoopFxLimiter, LoopFxParams, LoopFxVoice, LoopMasterBus,
    LoopMasterCommand, MasterChain, MasterFxKey, MasterMacro, PeakLimiter,
};
use crate::wave::net::WaveGraph;
use crate::wave::{
    read_events, roles_held, roles_held_in, EventCursor, FireEvent, LatestRing, Master, NetMacro,
    RoleTables, TickAnchor, VoiceLaneParams, WaveOptions, WaveProject, WaveRunner,
};
use crate::wave::{
    MacroMirror, MacroRead, MarkingBook, MarkingLayout, MarkingMirror, MarkingSnapshot,
};

use super::samplers::SamplerSet;
#[cfg(feature = "sf2")]
use crate::host::sampler::Sf2Sampler;

/// `WaveCommand`: main thread to render thread.
// (`Load` moves the whole compiled project through the ring, as Swift hands over a retained
// pointer; the smaller commands are the common ones, and a load happens once a song)
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// A compiled project: at once when stopped, at the next bar when playing.
    Load(WaveProject),
    /// A sampler the project being loaded plays, numbered by the bank.
    #[cfg(feature = "sf2")]
    Sampler(usize, Sf2Sampler),
    Play,
    Pause,
    Stop,
    Panic,
    Tempo(f64),
    /// Mute a net by `strHash(netId)`.
    Mute {
        hash: i32,
        muted: bool,
    },
    /// Mute a role (riff group or plain net) by `strHash(role)`.
    RoleMute {
        hash: i32,
        muted: bool,
    },
    /// `activate-slot` on the net with this hash.
    Slot(i32),
    Macro {
        m: NetMacro,
        ticks: i64,
    },
    Stinger {
        hash: i32,
        ticks: i64,
    },
    /// Shift the beat grid by this many ticks (`WaveRunner.align`).
    Align(f64),
    /// A key goes down on the live voice, already scale-locked by the sender (`noteOn`; build it
    /// with [`Command::note_on`], which clamps as `Int16(clamping:)` / `UInt8(clamping:)`).
    NoteOn {
        note: i16,
        velocity: u8,
    },
    /// The key comes up (`noteOff`; see [`Command::note_off`]).
    NoteOff {
        note: i16,
    },
    /// Every held key comes up (`allNotesOff`).
    AllNotesOff,
    /// Seconds the net clock takes to catch up with the hand (`chase`; no sender in the app, the
    /// scratch tests use it).
    Chase(f64),
    /// A master-chain macro; `bars` < 0: its default duration, 0: permanent.
    MasterMacro {
        m: MasterMacro,
        bars: i16,
    },
    /// A hand on a master FX slider (the slider's own units).
    MasterFx {
        key: MasterFxKey,
        value: i32,
    },
    /// A channel's voice settings, from the sound panel.
    Voice(VoiceLaneParams),
    /// The FX unit: a control, a reset, the kill.
    FxUnit(FxUnitCommand),
    /// A hand on the platter (the jog's touch): the net clock follows the hand.
    Freeze,
    /// Move the hand by this many ticks (the jog turning while touched).
    Scrub(f64),
    /// The hand lets go: back to the tempo clock.
    Release,
    /// To the deck's turntable (sample mode).
    Turntable(TurntableCommand),
    /// A clip's FX for the sample deck's chain (`DeckRenderer.sendFx`): `sounding` smooths and
    /// crossfades the change (the clip is playing on), otherwise the chain starts clean at them
    /// (a load).
    ClipFx {
        params: LoopFxParams,
        sounding: bool,
    },
    /// To the sample deck's master bus (`DeckRenderer.master`, a `LoopMasterBus`): a macro on the
    /// deck's bar grid, a slider, the panic, the FX unit.
    SampleMaster(LoopMasterCommand),
    /// Sample mode (the turntable plays) or net mode (the runner plays).
    SampleMode(bool),
}

impl Command {
    /// `WaveDeckSource.noteOn`: the note and velocity clamped into the command's types.
    pub fn note_on(note: i64, velocity: i64) -> Command {
        Command::NoteOn {
            note: note.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
            velocity: velocity.clamp(0, u8::MAX as i64) as u8,
        }
    }

    /// `WaveDeckSource.noteOff`.
    pub fn note_off(note: i64) -> Command {
        Command::NoteOff {
            note: note.clamp(i16::MIN as i64, i16::MAX as i64) as i16,
        }
    }
}

/// What the render thread publishes for the main thread, once per block (relaxed atomics:
/// two of them may be one block apart).
#[derive(Default)]
pub struct DeckMirror {
    /// Which performance tricks sound (a bit per `TrickKind`; docs/TRICKS.md).
    pub tricks: AtomicU32,
    pub tick: AtomicI64,
    phase_bits: AtomicU64,
    tempo_bits: AtomicU64,
    pub playing: AtomicBool,
    pub stopped: AtomicBool,
    pub has_project: AtomicBool,
    pub macro_active: AtomicBool,
    /// The hand is on the platter: the net clock follows it (`isScrubbing`).
    scrubbing: AtomicBool,
    /// Which macros are in effect and which nets each holds muted, for which project: one
    /// consistent record per block (`macroMirror`; the render thread is the only writer).
    pub macros: MacroMirror,
    /// The last whole record a reader got (`macroLast`): what it keeps if the writer is in the
    /// way. Main thread only; the render thread never takes this lock.
    macro_last: Mutex<MacroRead>,
    /// The marking mirrors of recently loaded projects, newest last (`mirrors`). Main thread
    /// only (the render thread writes the mirrors themselves, never this book).
    markings: Mutex<MarkingBook>,
    /// -1: no mix-out tail.
    tail_end: AtomicI64,
    pub master_mask: AtomicU32,
    /// The pan / shape / aux-FX macros running aimed at this deck (bits of `AuxMacro::bit`).
    pub aux_mask: AtomicU32,
    hp_bits: AtomicU64,
    lp_bits: AtomicU64,
    pub chain_idle: AtomicBool,
    pub generation: AtomicI32,
    pub pending_generation: AtomicI32,
    mute_words: [AtomicU64; 4],
    /// Frames this deck has rendered (the time base of fires and anchors).
    pub rendered_frames: AtomicI64,
    gr_bits: AtomicU64,
    /// Samplers loaded on the render thread.
    pub samplers_loaded: AtomicU32,
    /// The strip's peak at the last block, after the EQ and before the fader (f32 bits).
    level_bits: AtomicU32,
    /// The turntable plays (sample mode), not the runner.
    pub sample_mode: AtomicBool,
    tt_pos_bits: AtomicU64,
    tt_rate_bits: AtomicU64,
    pub tt_length: AtomicU64,
    /// The strip's output lately and the net clock's downbeats (`CaptureTap`).
    pub capture: super::capture::CaptureTap,
    /// The deck's loudness before the EQ and the fader, LUFS (f64 bits; -200 when silent):
    /// short-term over 3 s, momentary over 400 ms (`DeckRenderer.meter`, always on).
    short_term_bits: AtomicU64,
    momentary_bits: AtomicU64,
    /// Master macros fired at a sample deck's bus and not yet taken by the render thread: they
    /// count as running for the UI until it has (`LoopMasterBus.queuedMask`, `sent`, `queuedAt`).
    master_queued: AtomicU32,
    master_sent: AtomicUsize,
    master_queued_at: AtomicUsize,
    /// How many macro commands the sample bus has taken (`seenAtomic`).
    master_seen: AtomicUsize,
    /// Is the clip's FX chain in the deck's path as of the last block (`DeckRenderer.fxRunning`)?
    clip_fx_running: AtomicBool,
    /// Audio -> readers: every audible net fire, and every tick of the net clock, in order, as
    /// absolute sequence numbers, in the deck's frame time base. The render thread only writes;
    /// each reader keeps its own [`EventCursor`] (`WaveDeckSource.fires` / `anchors`, 8192 and
    /// 2048 records). Read `fires` before `anchors`: see [`DeckMirror::read_events`].
    pub fires: Arc<LatestRing<FireEvent>>,
    pub anchors: Arc<LatestRing<TickAnchor>>,
}

impl DeckMirror {
    /// Which performance tricks sound now (a bit per `TrickKind`).
    pub fn tricks_sounding(&self) -> u32 {
        self.tricks.load(Ordering::Relaxed)
    }

    pub fn new() -> DeckMirror {
        let m = DeckMirror::default();
        m.tail_end.store(-1, Ordering::Relaxed);
        m.hp_bits.store(20.0f64.to_bits(), Ordering::Relaxed);
        m.lp_bits.store(20_000.0f64.to_bits(), Ordering::Relaxed);
        m.chain_idle.store(true, Ordering::Relaxed);
        m.short_term_bits
            .store((-200.0f64).to_bits(), Ordering::Relaxed);
        m.momentary_bits
            .store((-200.0f64).to_bits(), Ordering::Relaxed);
        m
    }
    /// A cursor for a reader that subscribes now: the fires from now on, the anchors from the
    /// newest one written so far (`eventCursorNow`).
    pub fn event_cursor_now(&self) -> EventCursor {
        EventCursor::now(&self.fires, &self.anchors)
    }
    /// Append everything written since `cursor` to `fires` and `anchors` (oldest first) and move
    /// the cursor past it; how many records of each were lost, overwritten before this reader
    /// got to them (`readEvents`). Every fire returned has its anchor in this call's `anchors` or
    /// an earlier one's, unless that anchor was lost.
    pub fn read_events(
        &self,
        cursor: &mut EventCursor,
        fires: &mut Vec<FireEvent>,
        anchors: &mut Vec<TickAnchor>,
    ) -> (usize, usize) {
        read_events(&self.fires, &self.anchors, cursor, fires, anchors)
    }
    /// Is the clip's FX chain in the deck's path (`DeckRenderer.fxRunning`)?
    pub fn clip_fx_running(&self) -> bool {
        self.clip_fx_running.load(Ordering::Relaxed)
    }
    /// The deck's short-term loudness (BS.1770, the last 3 s) before the EQ and the fader, LUFS
    /// (-200 when silent) (`shortTermLUFS`).
    pub fn short_term_lufs(&self) -> f64 {
        f64::from_bits(self.short_term_bits.load(Ordering::Relaxed))
    }
    /// The momentary loudness over 400 ms (`momentaryLUFS`).
    pub fn momentary_lufs(&self) -> f64 {
        f64::from_bits(self.momentary_bits.load(Ordering::Relaxed))
    }
    /// Main thread: a master macro was sent to the sample deck's bus (`LoopMasterBus.fire`).
    pub fn note_master_fire(&self, m: MasterMacro) {
        let sent = self.master_sent.fetch_add(1, Ordering::Relaxed) + 1;
        self.master_queued.fetch_or(m.bit(), Ordering::Relaxed);
        self.master_queued_at.store(sent, Ordering::Relaxed);
    }
    /// Main thread: the sample bus was panicked (`LoopMasterBus.panic`).
    pub fn clear_master_queue(&self) {
        self.master_queued.store(0, Ordering::Relaxed);
    }
    /// The master macros running (or waiting for their bar) as of the last block: what the
    /// render thread published, with those sent but not yet taken (`runningMask`).
    pub fn running_master_mask(&self) -> u32 {
        let q = self.master_queued.load(Ordering::Relaxed);
        if q != 0
            && self.master_seen.load(Ordering::Acquire) >= self.master_queued_at.load(Ordering::Relaxed)
        {
            self.master_queued.store(0, Ordering::Relaxed);
        }
        self.master_mask.load(Ordering::Relaxed) | self.master_queued.load(Ordering::Relaxed)
    }
    /// Fraction of the current tick elapsed.
    pub fn tick_phase(&self) -> f64 {
        f64::from_bits(self.phase_bits.load(Ordering::Relaxed))
    }
    pub fn tempo(&self) -> f64 {
        f64::from_bits(self.tempo_bits.load(Ordering::Relaxed))
    }
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    pub fn tick(&self) -> i64 {
        self.tick.load(Ordering::Relaxed)
    }
    /// The tick the sounding project's mix-out tail stops on.
    pub fn tail_end_tick(&self) -> Option<i64> {
        let t = self.tail_end.load(Ordering::Relaxed);
        (t >= 0).then_some(t)
    }
    /// The master chain's HP and LP corners (Hz) at the last block, and whether it was
    /// bypassed then.
    pub fn master_filter_corners(&self) -> (f64, f64, bool) {
        (
            f64::from_bits(self.hp_bits.load(Ordering::Relaxed)),
            f64::from_bits(self.lp_bits.load(Ordering::Relaxed)),
            self.chain_idle.load(Ordering::Relaxed),
        )
    }
    /// Is net `i` of the sounding project muted (first 256 nets)?
    pub fn is_net_muted(&self, i: usize) -> bool {
        i < 256 && self.mute_words[i / 64].load(Ordering::Relaxed) & (1 << (i % 64)) != 0
    }
    pub fn is_master_active(&self, m: MasterMacro) -> bool {
        self.running_master_mask() & m.bit() != 0
    }
    /// The master-chain macros with an animation, a restore or a sub-fire pending, as of the
    /// last render block, in catalogue order (`activeMasterMacros`). Allocates its result.
    pub fn active_master_macros(&self) -> Vec<MasterMacro> {
        let m = self.running_master_mask();
        MasterMacro::ALL
            .iter()
            .copied()
            .filter(|k| m & k.bit() != 0)
            .collect()
    }
    /// The hand is on the platter (`isScrubbing`).
    pub fn is_scrubbing(&self) -> bool {
        self.scrubbing.load(Ordering::Relaxed)
    }

    /// The last whole macro record (`macroRead`): a writer in the way leaves the one before.
    pub fn macro_record(&self) -> MacroRead {
        let mut last = lock(&self.macro_last);
        if let Some(r) = self.macros.read() {
            *last = r;
        }
        *last
    }
    /// The macros in effect right now, as of the last render block (`activeMacros`): a mute
    /// macro while the nets it muted wait for their restore, the beat repeat while it runs, a
    /// tempo macro while its tempo effect lasts. Allocates its result.
    pub fn active_macros(&self) -> Vec<NetMacro> {
        self.macro_record().snapshot.active_macros()
    }
    /// `isActive(_ macro: NetMacro)`.
    pub fn is_active(&self, m: NetMacro) -> bool {
        self.macro_record().snapshot.is_active(m)
    }
    /// Is net `i` of the project the render thread runs held muted by a running macro (its
    /// restore is pending)? (`isNetHeldByMacro`; as [`is_net_muted`](Self::is_net_muted), from a
    /// per-block record.)
    pub fn is_net_held_by_macro(&self, i: usize) -> bool {
        self.macro_record().snapshot.is_held(i as i64)
    }
    /// Of `roles`, those with a net that `macro_` holds muted (its own mutes) and that is still
    /// muted, in the order given (`rolesHeld(by:among:)`). `latest` is the latest project
    /// loaded: its generation and a graph whose tables are read. Empty while the record comes
    /// from another project than that one (a hand-over waiting for its bar).
    pub fn roles_held_by<'a>(
        &self,
        latest: Option<(i32, &WaveGraph)>,
        macro_: NetMacro,
        roles: &[&'a str],
    ) -> Vec<&'a str> {
        roles_held(
            &self.macro_record(),
            latest,
            roles,
            |s, net| s.is_held_by(net, macro_),
            |i| self.is_net_muted(i),
        )
    }
    /// [`roles_held_by`](Self::roles_held_by) against the role tables the deck kept of the latest
    /// project (`Latest::role_tables`) rather than a graph.
    pub fn roles_held_by_in<'a>(
        &self,
        latest: Option<(i32, &RoleTables)>,
        macro_: NetMacro,
        roles: &[&'a str],
    ) -> Vec<&'a str> {
        roles_held_in(
            &self.macro_record(),
            latest,
            roles,
            |s, net| s.is_held_by(net, macro_),
            |i| self.is_net_muted(i),
        )
    }
    /// [`roles_held_by_macros`](Self::roles_held_by_macros) against kept role tables.
    pub fn roles_held_by_macros_in<'a>(
        &self,
        latest: Option<(i32, &RoleTables)>,
        roles: &[&'a str],
    ) -> Vec<&'a str> {
        roles_held_in(
            &self.macro_record(),
            latest,
            roles,
            |s, net| s.is_held(net),
            |i| self.is_net_muted(i),
        )
    }
    /// `rolesHeldByMacros`: [`roles_held_by`](Self::roles_held_by) for any macro.
    pub fn roles_held_by_macros<'a>(
        &self,
        latest: Option<(i32, &WaveGraph)>,
        roles: &[&'a str],
    ) -> Vec<&'a str> {
        roles_held(
            &self.macro_record(),
            latest,
            roles,
            |s, net| s.is_held(net),
            |i| self.is_net_muted(i),
        )
    }

    /// A project was loaded: keep its marking mirror and layout (`remember`), at most
    /// [`MarkingBook::KEPT`], never the one the render thread runs or the newest.
    pub fn remember_marking(&self, m: Arc<MarkingMirror>) {
        let active = self.generation.load(Ordering::Relaxed);
        lock(&self.markings).remember(m, active);
    }
    /// Copy the marking of the project that is sounding into `snap` (`readMarking(into:)`);
    /// on a deck that is not playing, the project loaded last. False if nothing is loaded.
    pub fn read_marking(&self, snap: &mut MarkingSnapshot) -> bool {
        lock(&self.markings).read_marking(
            snap,
            self.generation.load(Ordering::Relaxed),
            self.is_playing(),
            self.pending_generation.load(Ordering::Relaxed),
        )
    }
    /// The marking of a particular recent project (`readMarking(generation:into:)`).
    pub fn read_marking_of(&self, generation: i32, snap: &mut MarkingSnapshot) -> bool {
        lock(&self.markings).read_marking_of(generation, snap)
    }
    /// The layout of the project loaded last (`markingLayout`).
    pub fn marking_layout(&self) -> Option<Arc<MarkingLayout>> {
        lock(&self.markings).latest_layout()
    }
    /// The layout of a recent project by generation (`markingLayout(generation:)`).
    pub fn marking_layout_of(&self, generation: i32) -> Option<Arc<MarkingLayout>> {
        lock(&self.markings).layout(generation)
    }
    /// The turntable's unwrapped playhead (samples) and rate, as of the last block.
    pub fn turntable_position(&self) -> f64 {
        f64::from_bits(self.tt_pos_bits.load(Ordering::Relaxed))
    }
    pub fn turntable_rate(&self) -> f64 {
        f64::from_bits(self.tt_rate_bits.load(Ordering::Relaxed))
    }
    pub fn is_sample_mode(&self) -> bool {
        self.sample_mode.load(Ordering::Relaxed)
    }
    /// The strip's peak at the last block, linear (after the EQ, before the fader).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
    pub(crate) fn publish_level(&self, peak: f32) {
        self.level_bits.store(peak.to_bits(), Ordering::Relaxed);
    }
    /// How far the limiter pulled the last block down, dB.
    pub fn limiter_gain_reduction_db(&self) -> f64 {
        f64::from_bits(self.gr_bits.load(Ordering::Relaxed))
    }
    pub(crate) fn publish_mutes(&self, muted: &[u8]) {
        let mut w = [0u64; 4];
        for (i, &m) in muted.iter().take(256).enumerate() {
            if m != 0 {
                w[i / 64] |= 1 << (i % 64);
            }
        }
        for k in 0..4 {
            self.mute_words[k].store(w[k], Ordering::Relaxed);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const OP_CAPACITY: usize = 256;
const EVENT_CAPACITY: usize = 512;

/// This block's slider dispatches, in time order (`opIndex`…`opLive`).
struct OpQueue {
    n: usize,
    index: [usize; OP_CAPACITY],
    time: [f64; OP_CAPACITY],
    key: [MasterFxKey; OP_CAPACITY],
    value: [f64; OP_CAPACITY],
    /// The op is the FX unit's (a ramp over the block), not a macro's slider.
    live: [bool; OP_CAPACITY],
}

impl OpQueue {
    fn new() -> Box<OpQueue> {
        Box::new(OpQueue {
            n: 0,
            index: [0; OP_CAPACITY],
            time: [0.0; OP_CAPACITY],
            key: [MasterFxKey::HpFreq; OP_CAPACITY],
            value: [0.0; OP_CAPACITY],
            live: [false; OP_CAPACITY],
        })
    }

    /// A chain op for this block, `offset` seconds into it (`queue`).
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        frames: usize,
        sample_rate: f64,
        chain_t0: f64,
        key: MasterFxKey,
        value: f64,
        offset: f64,
        live: bool,
    ) {
        if self.n >= OP_CAPACITY {
            return;
        }
        let offset = offset.max(0.0);
        let i = self.n;
        self.index[i] = frames.min((offset * sample_rate).ceil().max(0.0) as usize);
        self.time[i] = chain_t0 + offset;
        self.key[i] = key;
        self.value[i] = value;
        self.live[i] = live;
        self.n += 1;
    }
}

/// The deck's runner with its samplers, as the brake's source: what a braked chunk pulls.
struct DeckSource<'a> {
    runner: &'a mut WaveRunner,
    samplers: &'a mut SamplerSet,
}

impl DeckSource<'_> {
    /// The runner stopped itself (the song's `stop-transport`, a mix-out tail's last tick, the
    /// end of a brake's catch-up): `WaveRunner.stop()` queues CC 123 on every sampler slot,
    /// clears the lanes' pending note-offs and resets the slots' side highpasses. The runner
    /// leaves the keys to the host, so the host does it here, when `stopped()` goes up: a held
    /// live key on a sampled voice and the net's notes still owed an end are released now, not
    /// at their own length (a pad's can be seconds). One block late at most (the runner has
    /// already flushed the block it stopped in); `Command::Stop` does the same on its own.
    #[inline]
    fn release_if_stopped(&mut self, was_stopped: bool) {
        if !was_stopped && self.runner.stopped() {
            self.samplers.all_notes_off();
            self.samplers.reset_sides();
        }
    }
}

impl BrakeSource for DeckSource<'_> {
    fn playing(&self) -> bool {
        self.runner.playing()
    }
    fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        start_frame: usize,
    ) {
        let was_stopped = self.runner.stopped();
        let Some(right) = right else {
            // (mono: the samplers have no channel to play on)
            self.runner
                .process_at(left, None, frames, start_frame as i64);
            self.runner.sample_notes.clear();
            self.runner.sample_keys.clear();
            self.runner.lane_ends.clear();
            self.release_if_stopped(was_stopped);
            return;
        };
        // the runner renders the samplers inside its block, as Swift's does: after the curve, or
        // (`.loudness`, `samplersInCurve`) before it, so the loudness gain and the energy curve
        // reach a sampled mix
        self.runner.process_with(
            left,
            Some(right),
            frames,
            start_frame as i64,
            Some(&mut *self.samplers),
        );
        self.release_if_stopped(was_stopped);
    }
    fn brake_lag(&self, sample_rate: f64) -> f64 {
        self.runner.tempo() * 4.0 / (60.0 * sample_rate)
    }
    fn brake_chunk(&mut self, _output: usize, _pulled: usize) {}
    fn catch_up(&mut self, ticks: f64) {
        let was_stopped = self.runner.stopped();
        self.runner.catch_up(ticks);
        self.release_if_stopped(was_stopped);
    }
}

/// The sample deck's turntable and the clip's FX, as the master bus's source (the vinyl brake
/// slows both together, and its release puts the turntable's playhead where it would have been:
/// `DeckRenderer`'s `BrakeSource`, with `Turntable`'s).
struct SampleSource<'a> {
    turntable: &'a mut Turntable,
    fx: &'a mut LoopFxVoice,
    commands: &'a mut VecDeque<(LoopFxParams, bool)>,
    /// Did the clip's FX chain run in this block (`blockFx`)? The limiter runs while it does.
    block_fx: &'a mut bool,
}

impl BrakeSource for SampleSource<'_> {
    /// The brake's clock runs on the deck's frames; a stopped transport cancels it instead.
    fn playing(&self) -> bool {
        true
    }
    fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        _start_frame: usize,
    ) {
        let mut right = right;
        self.turntable.render(frames, left, right.as_deref_mut());
        // `processFx`: the clip's FX over the turntable's block, only while they are in the path.
        // The commands are taken in sample mode only (a net deck leaves them for when it is a
        // sample deck again).
        while let Some((p, sounding)) = self.commands.pop_front() {
            self.fx.set(&p, sounding);
        }
        if !self.fx.running() {
            return;
        }
        *self.block_fx = true;
        // (a mono render has no right channel for the stereo chain: the net half of this deck
        // never renders mono either)
        if let Some(r) = right {
            self.fx.process(left, r, frames);
        }
    }
    /// Lag in output frames (one per output frame an unbraked turntable plays).
    fn brake_lag(&self, _sample_rate: f64) -> f64 {
        1.0
    }
    fn brake_chunk(&mut self, _output: usize, _pulled: usize) {}
    /// `ticks` output frames behind: skip on by them at the current rate.
    fn catch_up(&mut self, ticks: f64) {
        self.turntable.skip(ticks);
    }
}

/// A net deck on the render thread.
pub struct DeckCore {
    pub name: String,
    pub sample_rate: f64,
    pub runner: WaveRunner,
    pub samplers: SamplerSet,
    /// The sample deck: a file or a clip on a platter (`Deck.turntable`).
    pub turntable: Turntable,
    sample_mode: bool,
    /// A sample deck's own master chain and macros (`DeckRenderer.master`, a `LoopMasterBus`): the
    /// source is the turntable and the clip's FX, the clock the deck's frames rendered in sample
    /// mode, macros timed on the deck's bar grid by the main thread (`Engine::sample_master_fire`).
    /// At rest it is bit-transparent, so a deck without master FX renders the turntable's bytes.
    sample_bus: LoopMasterBus,
    /// The clip's FX chain and its safety limiter on a sample deck (`DeckRenderer.fx`, `fxLimiter`),
    /// fed through `clip_fx_commands`.
    clip_fx: LoopFxVoice,
    fx_limiter: LoopFxLimiter,
    clip_fx_commands: VecDeque<(LoopFxParams, bool)>,
    /// Did the clip's FX chain run in this block?
    block_fx: bool,
    /// This deck's loudness before the EQ and the fader, always on (`DeckRenderer.meter`).
    meter: LoudnessMeter,
    /// The runner's downbeats already handed to the capture tap.
    downbeat_cursor: u64,
    chain: MasterChain,
    fx: FxAutomation,
    unit: FxUnitLayer,
    /// The sends last handed to the runner: room gain, echo gain, feedback boost
    /// (`sentRoom` / `sentEcho` / `sentFeedback`).
    sent_room: f64,
    sent_echo: f64,
    sent_feedback: f64,
    brake: TurntableBrake,
    pub limiter: Option<PeakLimiter>,
    /// `vinyl-brake` brakes the deck (`WaveOptions.turntableBrake`).
    pub turntable_brake: bool,
    /// Frames the transport has run (it stops while the deck is paused or stopped).
    transport_frames: i64,
    block_transport: f64,
    block_frames: usize,
    ops: Box<OpQueue>,
    events: Vec<FxAutomationEvent>,
    rendered: i64,
    pub mirror: Arc<DeckMirror>,
}

impl DeckCore {
    /// How many clip-FX commands wait for the next block (`fxCommands`, a ring of 32).
    pub const CLIP_FX_COMMANDS: usize = 32;

    pub fn new(
        name: &str,
        sample_rate: f64,
        opts: WaveOptions,
        turntable_brake: bool,
        mirror: Arc<DeckMirror>,
    ) -> DeckCore {
        let limiter = (opts.master == Master::Limit).then(|| PeakLimiter::new(sample_rate));
        let mut runner = WaveRunner::new(sample_rate, opts);
        // the net clock's fires and anchors go to the deck's rings (`runner.fireRing = fires`)
        runner.fire_ring = Some(mirror.fires.clone());
        runner.anchor_ring = Some(mirror.anchors.clone());
        let tempo = runner.tempo();
        DeckCore {
            name: name.to_string(),
            sample_rate,
            runner,
            samplers: SamplerSet::new(sample_rate),
            turntable: Turntable::new(sample_rate),
            sample_mode: false,
            sample_bus: LoopMasterBus::new(sample_rate),
            clip_fx: LoopFxVoice::new(sample_rate),
            fx_limiter: LoopFxLimiter::new(sample_rate),
            clip_fx_commands: VecDeque::with_capacity(Self::CLIP_FX_COMMANDS),
            block_fx: false,
            meter: LoudnessMeter::new(sample_rate),
            downbeat_cursor: 0,
            chain: MasterChain::new(sample_rate),
            fx: FxAutomation::new(tempo, tempo),
            unit: FxUnitLayer::new(sample_rate),
            sent_room: 1.0,
            sent_echo: 1.0,
            sent_feedback: 0.0,
            brake: TurntableBrake::new(sample_rate),
            limiter,
            turntable_brake,
            transport_frames: 0,
            block_transport: 0.0,
            block_frames: 0,
            ops: OpQueue::new(),
            events: Vec::with_capacity(EVENT_CAPACITY),
            rendered: 0,
            mirror,
        }
    }

    /// A buffer the turntable let go of, to be freed off the render thread.
    pub fn pop_retired_buffer(&mut self) -> Option<std::sync::Arc<SampleBuffer>> {
        self.turntable.pop_retired()
    }

    /// Frames rendered so far (the time base of this deck's events).
    pub fn rendered_frames(&self) -> i64 {
        self.rendered
    }

    /// The slider dispatches the last block queued for the master chain, in time order, as
    /// (key, value the chain was asked for, the sample of the block it falls on): what the
    /// macros said once the FX unit's setting is under them. For tests; allocates.
    pub fn last_block_ops(&self) -> Vec<(MasterFxKey, f64, usize)> {
        (0..self.ops.n)
            .filter(|&i| !self.ops.live[i])
            .map(|i| (self.ops.key[i], self.ops.value[i], self.ops.index[i]))
            .collect()
    }

    /// `el._tempo` is the deck's tempo; the genre's BPM is the project's.
    fn sync_fx(&mut self) {
        self.fx.tempo = self.runner.tempo();
        self.fx.genre_bpm = self
            .runner
            .graph()
            .map(|g| g.project.tempo)
            .unwrap_or_else(|| self.runner.tempo());
    }

    /// What the automation said, during a block: slider dispatches are queued for the chain at
    /// the sample they fall on; tempo writes go to the runner at once (block resolution, as
    /// the tempo macros). The events are collected while the automation runs and handled
    /// right after; each carries whether its key was animating at its own instant, which is
    /// what `unit.automated` reads in Swift (inline, inside `fx.advance`).
    fn dispatch_events(&mut self) {
        let (frames, sr) = (self.block_frames, self.sample_rate);
        let t0 = self.chain.time_at(0);
        for i in 0..self.events.len() {
            match self.events[i] {
                FxAutomationEvent::Fx {
                    time,
                    key,
                    value,
                    animating,
                } => {
                    // the macro's slider, on the FX unit's setting (at neutral, the slider)
                    let v = self.unit.automated_at(key, value, animating);
                    self.ops
                        .push(frames, sr, t0, key, v, time - self.block_transport, false);
                }
                FxAutomationEvent::Tempo { bpm, .. } => self.runner.set_tempo(bpm),
            }
        }
        self.events.clear();
    }

    /// The turntable brake: `bars` <= 0 its default, as beats' compound takes it. A fire
    /// while one runs does nothing.
    fn fire_brake(&mut self, bars: i16) {
        let b = if bars <= 0 {
            MasterMacro::VinylBrake.default_duration()
        } else {
            bars as i32
        };
        let ticks = b as i64 * 16;
        let frames = ticks as f64 * self.sample_rate * 60.0 / (self.runner.tempo() * 4.0);
        if self.brake.start(frames.round() as usize) {
            self.runner.mark_macro_running(ticks);
        }
    }

    /// Apply one command (the render thread, at the top of a block).
    pub fn apply(&mut self, c: Command) {
        match c {
            Command::Load(p) => self.runner.load_prepared(p),
            #[cfg(feature = "sf2")]
            Command::Sampler(slot, s) => self.samplers.set_slot(slot, s),
            Command::Play => self.runner.play(),
            Command::Pause => self.runner.pause(),
            Command::Stop => {
                // A stop under the brake ends the record where the brake left it: what it held
                // must not fade back in after the cancel. (`silence` also sends every slot
                // all-sound-off, CC 120.)
                if self.brake.is_on() {
                    self.runner.silence();
                    self.samplers.silence_all();
                }
                self.runner.stop();
                // `stop` sends every slot all-notes-off (CC 123): the tails ring out, as a deck's
                // room does
                self.samplers.all_notes_off();
                self.samplers.reset_sides();
                self.brake.cancel();
                // beats: transport stop cancels and restores every animation
                let events = &mut self.events;
                self.fx.panic(&mut |e| {
                    if events.len() < EVENT_CAPACITY {
                        events.push(e)
                    }
                });
                self.dispatch_events();
            }
            Command::Panic if self.sample_mode => {
                // a sample deck's panic releases its own bus (`renderer.master?.panic()`)
                self.sample_bus.send(LoopMasterCommand::Panic);
            }
            Command::Panic => {
                self.runner.panic();
                // `panic` sends every slot all-notes-off, too
                self.samplers.all_notes_off();
                self.brake.panic();
                self.sync_fx();
                let events = &mut self.events;
                self.fx.panic(&mut |e| {
                    if events.len() < EVENT_CAPACITY {
                        events.push(e)
                    }
                });
                self.dispatch_events();
            }
            Command::Tempo(bpm) => self.runner.set_tempo(bpm),
            Command::Mute { hash, muted } => self.runner.set_mute_hash(hash, muted),
            Command::RoleMute { hash, muted } => self.runner.set_role_muted(hash, muted),
            Command::Slot(hash) => self.runner.activate_slot(hash),
            Command::Macro { m, ticks } => self.runner.fire_macro(m, ticks),
            Command::Stinger { hash, ticks } => self.runner.fire_stinger(hash, ticks),
            Command::Align(t) => self.runner.align(t),
            Command::NoteOn { note, velocity } => self.runner.note_on(note as i64, velocity as i64),
            Command::NoteOff { note } => self.runner.note_off(note as i64),
            Command::AllNotesOff => self.runner.all_notes_off(),
            Command::Chase(seconds) => self.runner.set_chase(seconds),
            Command::MasterMacro { m, bars } if self.sample_mode => {
                // the engine sends a sample deck's macro as a `SampleMaster` command, timed on
                // the deck's bar grid; this one is the net deck's, aimed at a sample deck: it
                // goes on the bus at once, at 120 (`sampleMasterFire` of a stopped deck)
                self.sample_bus.send(LoopMasterCommand::Macro {
                    id: m.raw(),
                    bars,
                    frame: 0,
                    bpm: 120.0,
                });
            }
            Command::MasterMacro { m, bars } => {
                if m == MasterMacro::VinylBrake && self.turntable_brake {
                    self.fire_brake(bars);
                    return;
                }
                self.sync_fx();
                let events = &mut self.events;
                let ticks = self
                    .fx
                    .fire(m, (bars >= 0).then_some(bars as i32), &mut |e| {
                        if events.len() < EVENT_CAPACITY {
                            events.push(e)
                        }
                    });
                self.dispatch_events();
                self.runner.mark_macro_running(ticks);
            }
            Command::MasterFx { key, value } if self.sample_mode => {
                self.sample_bus.send(LoopMasterCommand::Slider {
                    key: key.index() as u8,
                    value: value.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                });
            }
            Command::MasterFx { key, value } => self.fx.set_slider(key, value as f64),
            Command::Voice(p) => self.runner.set_voice(&p),
            // the FX unit is on whichever chain the deck plays through now (`sendFxUnit`)
            Command::FxUnit(u) if self.sample_mode => {
                self.sample_bus.send(LoopMasterCommand::FxUnit(u))
            }
            Command::FxUnit(u) => self.unit.take(u),
            Command::Freeze => {
                // the hand takes the record from the brake
                self.brake.cancel();
                self.runner.freeze();
            }
            Command::Scrub(t) => self.runner.scrub(t),
            Command::Release => self.runner.release(),
            Command::Turntable(c) => self.turntable.send(c),
            Command::ClipFx { params, sounding } => {
                // dropped with a full ring (32 waiting: never in practice)
                if self.clip_fx_commands.len() < Self::CLIP_FX_COMMANDS {
                    self.clip_fx_commands.push_back((params, sounding));
                }
            }
            Command::SampleMaster(c) => self.sample_bus.send(c),
            Command::SampleMode(on) => {
                if on == self.sample_mode {
                    return;
                }
                self.sample_mode = on;
                if on {
                    // the net falls silent where it is
                    self.runner.pause();
                    self.runner.silence();
                    self.samplers.silence_all();
                    self.brake.cancel();
                } else {
                    self.turntable.send(TurntableCommand::Transport(false));
                }
            }
        }
    }

    /// Fill `left` / `right` (the same length) with this deck's block. Real-time safe.
    /// `start_frame` is the deck frame of the first sample.
    ///
    /// A net deck renders the runner and its samplers through the brake, the master chain and the
    /// limiter ([`render_net`](Self::render_net), `WaveDeckSource.render`); a sample deck the
    /// turntable and the clip's FX through its own master bus and the fx limiter
    /// ([`render_sample`](Self::render_sample), `DeckRenderer.renderBlock`). Either way the
    /// deck's loudness meter hears the result (pre-EQ, pre-fader) and the mirrors are published.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32], start_frame: i64) {
        let frames = left.len();
        if self.sample_mode {
            self.render_sample(left, right, start_frame);
        } else {
            self.render_net(left, right, start_frame);
        }
        self.meter.push(left, Some(&*right), frames);
        self.rendered = start_frame + frames as i64;
        self.publish();
        if !self.sample_mode {
            // The commands of the next block come before its `render`, and a macro they fire is
            // anchored at the automation's `now`: Swift sets it to the block's transport time
            // before it pops them, so it has to be there already (a macro fired at the top of a
            // block starts at that block, not the one before).
            self.fx.set_now(self.transport_frames as f64 / self.sample_rate);
        }
    }

    /// `DeckRenderer.renderBlock`'s sample half: this block's hand and transport first (a jog or
    /// a stop takes the record from the brake), then the turntable and the clip's FX through the
    /// master bus, then the safety limiter while the clip's FX or the bus changed the block (or
    /// it is still coming back up): the bus engaged mid-block (a fire on its bar) is limited from
    /// that frame, so the frames before the bar stay the turntable's bytes.
    fn render_sample(&mut self, left: &mut [f32], right: &mut [f32], start_frame: i64) {
        let frames = left.len();
        self.block_fx = false;
        self.turntable.take_commands();
        if self.sample_bus.braking()
            && (self.turntable.hand_on() || !self.turntable.motor_running())
        {
            self.sample_bus.cancel_brake();
        }
        let mut src = SampleSource {
            turntable: &mut self.turntable,
            fx: &mut self.clip_fx,
            commands: &mut self.clip_fx_commands,
            block_fx: &mut self.block_fx,
        };
        self.sample_bus
            .render(&mut src, frames, left, Some(&mut *right), start_frame);
        let (engaged, from) = (self.sample_bus.engaged(), self.sample_bus.engaged_from());
        if self.block_fx || engaged || !self.fx_limiter.idle() {
            let k = if self.block_fx || !self.fx_limiter.idle() {
                0
            } else {
                from.min(frames)
            };
            if k < frames {
                self.fx_limiter
                    .process(&mut left[k..], Some(&mut right[k..]), frames - k);
            }
        }
    }

    /// `WaveDeckSource.render`.
    fn render_net(&mut self, left: &mut [f32], right: &mut [f32], start_frame: i64) {
        let frames = left.len();
        let sr = self.sample_rate;
        self.block_transport = self.transport_frames as f64 / sr;
        self.block_frames = frames;
        self.ops.n = 0;
        self.fx.set_now(self.block_transport);
        self.sync_fx();
        {
            let t0 = self.chain.time_at(0);
            let ops = &mut self.ops;
            self.unit
                .begin_block(frames as i64, start_frame, &self.fx, &mut |key, value| {
                    ops.push(frames, sr, t0, key, value, 0.0, true)
                });
        }
        // the FX unit's room, echo and feedback knobs reach the runner's sends, over this block
        let (room, echo, feedback) = (
            self.unit.room_gain(),
            self.unit.echo_gain(),
            self.unit.feedback_boost(),
        );
        if room != self.sent_room || echo != self.sent_echo || feedback != self.sent_feedback {
            self.sent_room = room;
            self.sent_echo = echo;
            self.sent_feedback = feedback;
            self.runner.set_unit_sends(room, echo, feedback, frames);
        }
        {
            let events = &mut self.events;
            let mut emit = |e: FxAutomationEvent| {
                if events.len() < EVENT_CAPACITY {
                    events.push(e)
                }
            };
            if self.runner.playing() {
                let end = self.block_transport + frames as f64 / sr;
                self.fx.advance(end, &mut emit);
                self.transport_frames += frames as i64;
            } else {
                self.fx.flush(&mut emit);
            }
        }
        self.dispatch_events();
        {
            let mut src = DeckSource {
                runner: &mut self.runner,
                samplers: &mut self.samplers,
            };
            self.brake.render(
                &mut src,
                left,
                Some(&mut *right),
                frames,
                start_frame as usize,
            );
        }
        let mut s = 0;
        for i in 0..self.ops.n {
            let k = self.ops.index[i];
            if k > s {
                self.chain.process(left, Some(&mut *right), s, k);
                s = k;
            }
            let live = self.ops.live[i].then_some(frames as f64 / sr);
            self.chain
                .apply_live(self.ops.key[i], self.ops.value[i], self.ops.time[i], live);
        }
        self.chain.process(left, Some(&mut *right), s, frames);
        self.chain.finish(frames);
        if let Some(l) = self.limiter.as_mut() {
            l.process(left, Some(&mut *right), frames);
        }
    }

    fn publish(&mut self) {
        // the marking and mutes of the project playing, as of the end of this block, into its
        // marking mirror (`runner.publishMarking(endFrame:)`; `DeckMirror::read_marking` reads it)
        self.runner.publish_marking(self.rendered);
        let m = &*self.mirror;
        let written = self.runner.downbeats_written;
        let ring = crate::wave::runner::DOWNBEAT_RING as u64;
        for i in self.downbeat_cursor.max(written.saturating_sub(ring))..written {
            let (frame, beat) = self.runner.downbeats[(i % ring) as usize];
            m.capture.downbeat(frame, beat);
        }
        self.downbeat_cursor = written;
        // the chain the deck plays through: a net deck's (`masterMaskAtomic`...), or the sample
        // deck's bus (`LoopMasterBus.maskAtomic`...)
        if self.sample_mode {
            m.master_mask
                .store(self.sample_bus.mask(), Ordering::Relaxed);
            let (hp, lp, idle) = self.sample_bus.filter_corners();
            m.hp_bits.store(hp.to_bits(), Ordering::Relaxed);
            m.lp_bits.store(lp.to_bits(), Ordering::Relaxed);
            m.chain_idle.store(idle, Ordering::Relaxed);
        } else {
            let mask = self.fx.active_mask()
                | if self.brake.is_on() {
                    MasterMacro::VinylBrake.bit()
                } else {
                    0
                };
            m.master_mask.store(mask, Ordering::Relaxed);
            let (hp, lp) = self.chain.filter_corners(0);
            m.hp_bits.store(hp.to_bits(), Ordering::Relaxed);
            m.lp_bits.store(lp.to_bits(), Ordering::Relaxed);
            m.chain_idle.store(self.chain.is_idle(0), Ordering::Relaxed);
        }
        m.master_seen
            .store(self.sample_bus.seen(), Ordering::Release);
        m.clip_fx_running
            .store(self.clip_fx.running(), Ordering::Relaxed);
        m.short_term_bits
            .store(self.meter.short_term_lufs().to_bits(), Ordering::Relaxed);
        m.momentary_bits
            .store(self.meter.momentary_lufs().to_bits(), Ordering::Relaxed);
        m.generation
            .store(self.runner.generation(), Ordering::Relaxed);
        m.pending_generation
            .store(self.runner.pending_generation(), Ordering::Relaxed);
        m.tick
            .store(self.runner.graph().map_or(0, |g| g.tick), Ordering::Relaxed);
        m.phase_bits
            .store(self.runner.tick_phase().to_bits(), Ordering::Relaxed);
        m.tempo_bits
            .store(self.runner.tempo().to_bits(), Ordering::Relaxed);
        let playing = if self.sample_mode {
            self.turntable.motor_running()
        } else {
            self.runner.playing()
        };
        m.playing.store(playing, Ordering::Relaxed);
        m.sample_mode.store(self.sample_mode, Ordering::Relaxed);
        m.tt_pos_bits
            .store(self.turntable.position().to_bits(), Ordering::Relaxed);
        m.tt_rate_bits
            .store(self.turntable.current_rate().to_bits(), Ordering::Relaxed);
        m.tt_length
            .store(self.turntable.length() as u64, Ordering::Relaxed);
        m.stopped.store(self.runner.stopped(), Ordering::Relaxed);
        m.tail_end
            .store(self.runner.tail_end_tick().unwrap_or(-1), Ordering::Relaxed);
        m.has_project
            .store(self.runner.graph().is_some(), Ordering::Relaxed);
        m.scrubbing
            .store(self.runner.is_scrubbing(), Ordering::Relaxed);
        m.macro_active
            .store(self.runner.macro_active(), Ordering::Relaxed);
        m.macros
            .publish(&self.runner.macro_snapshot(), self.runner.generation());
        m.rendered_frames.store(self.rendered, Ordering::Release);
        // (`limiterGainReductionDB`: the net deck's limiter; a sample deck has none to report)
        match &self.limiter {
            Some(l) if !self.sample_mode => m
                .gr_bits
                .store(l.gain_reduction_db().to_bits(), Ordering::Relaxed),
            _ => m.gr_bits.store(0f64.to_bits(), Ordering::Relaxed),
        }
        m.samplers_loaded
            .store(self.samplers.loaded() as u32, Ordering::Relaxed);
        if let Some(g) = self.runner.graph() {
            m.publish_mutes(&g.muted);
        }
    }
}
