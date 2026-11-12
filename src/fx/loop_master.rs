//! The loops' master bus, "Loops master FX" (`Audio/LoopMasterBus.swift`): the net decks' master
//! chain on the loop channel.
//!
//! A net deck ends in beats' master inserts ([`MasterChain`]) driven by the master macros
//! ([`FxAutomation`]); the loop channel had only each clip's own after effects
//! ([`LoopFxVoice`](super::loop_fx::LoopFxVoice), per voice) and a safety limiter. This puts the
//! same chain and automation on the loop channel as one insert, after the 16 voices are summed:
//!
//! ```text
//! voices (turntable -> LoopFx -> level x fade) -> sum -> [vinyl brake] -> MasterChain -> limiter
//! ```
//!
//! The limiter stays last: distortion, crush and the pitch shifter can lift peaks, and the
//! channel's promise (never past -0.5 dBFS) holds after the chain. It runs while this bus is
//! engaged too.
//!
//! Clock: the bus's transport is the loop channel's own frame count (it never pauses: the loops
//! play whenever they are in). Macros are timed in bars of the loops' tempo: the main thread
//! sends each fire with the loop clock's BPM and the loop-channel frame it lands on (the next
//! launch line, at most a bar away; a linked fire, at once). The duration is fixed at the fire
//! from that tempo, as beats fixes a deck macro's. Feel macros drive the Feel puck as on a deck,
//! but only its filter reaches the loops: their tempo axis is dropped (the loops follow the
//! decks' tempo; a loop bus that set it would fight them).
//!
//! `vinyl-brake` is the net deck's turntable brake ([`TurntableBrake`]) with the bank as its
//! source: every voice slows down together (varispeed on the summed voices, before the chain),
//! then each voice that sounded through it skips on by the frames it fell behind and the loops
//! drop back in on the beat they would be on, over a 5 ms fade. Panic releases (5 ms), as on a
//! deck.
//!
//! At rest the bus is bit-transparent (a neutral chain is bypassed, an idle brake renders
//! straight through), so loops without master FX render the bank's old bytes.
//!
//! Real-time: everything is allocated in [`LoopMasterBus::new`]; [`LoopMasterBus::render`]
//! allocates nothing. The main thread reaches it through [`LoopMasterCommand`]s (the host's
//! command ring) and reads what it publishes ([`LoopMasterBus::mask`] and friends) from
//! atomics the host fills after each block.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use super::automation::{FxAutomation, FxAutomationEvent};
use super::brake::{BrakeSource, TurntableBrake};
use super::master_chain::MasterChain;
use super::master_macro::{MasterFxKey, MasterMacro};
use super::unit::{FxUnitCommand, FxUnitLayer};

/// Main -> audio messages for the loops' master bus (`LoopMasterCommand`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoopMasterCommand {
    /// A master macro ([`MasterMacro::raw`]) for `bars` bars (< 0: its default, 0: permanent) at
    /// the loops' tempo `bpm`, starting at loop-channel frame `frame` (a frame already past: at
    /// once).
    Macro {
        id: u8,
        bars: i16,
        frame: i64,
        bpm: f64,
    },
    /// A hand on a master FX slider ([`MasterFxKey::index`], the slider's own units).
    Slider { key: u8, value: i16 },
    /// Cancel and restore everything (pending fires too); a running brake releases.
    Panic,
    /// Let go of a latched master macro ([`MasterMacro::raw`]): its sliders go back.
    Release { id: u8 },
    /// The FX unit: a control, a reset, the kill. (No sends: the bus has no room or echo of its
    /// own.)
    FxUnit(FxUnitCommand),
}

const OP_CAPACITY: usize = 256;
const EVENT_CAPACITY: usize = 512;

/// This block's slider dispatches, in time order (`opIndex`...`opLive`).
struct OpQueue {
    n: usize,
    index: [usize; OP_CAPACITY],
    time: [f64; OP_CAPACITY],
    key: [MasterFxKey; OP_CAPACITY],
    value: [f64; OP_CAPACITY],
    /// The op is the FX unit's (a ramp over the block), not a macro's slider.
    live: [bool; OP_CAPACITY],
}

/// A fire waiting for its frame.
#[derive(Clone, Copy)]
struct Pending {
    id: u8,
    bars: i16,
    frame: i64,
    bpm: f64,
}

pub struct LoopMasterBus {
    pub sample_rate: f64,
    fx: FxAutomation,
    chain: MasterChain,
    /// The FX unit's hands on the chain.
    unit: FxUnitLayer,
    brake: TurntableBrake,
    /// The commands waiting for the next block (`commands`, an SPSC ring of 128 in Swift).
    commands: VecDeque<LoopMasterCommand>,

    /// Frames the bus has run (the loop channel's own clock).
    transport_frames: i64,
    block_transport: f64,
    block_frames: usize,

    /// Fires waiting for their frame (audio thread), in arrival order.
    pending: [Pending; Self::PENDING_CAPACITY],
    pending_count: usize,

    ops: Box<OpQueue>,
    events: Vec<FxAutomationEvent>,

    /// Macro commands taken from the ring (audio thread; published as `seen`).
    seen: usize,
    /// Did the last block change the audio (the chain was not at rest, or the brake ran)? The
    /// bank runs its limiter while it is.
    engaged: bool,
    /// The first frame of the last block the bus changed (`frames` when it didn't): a fire
    /// landing mid-block leaves the frames before it as they were, so a sample deck's limiter
    /// starts there, not at the block's start.
    engaged_from: usize,
    // what the last block leaves for the main thread
    mask: u32,
    hp_corner: f64,
    lp_corner: f64,
    idle: bool,
}

impl LoopMasterBus {
    pub const PENDING_CAPACITY: usize = 16;
    pub const COMMAND_CAPACITY: usize = 128;

    pub fn new(sample_rate: f64) -> LoopMasterBus {
        let none = Pending {
            id: 0,
            bars: 0,
            frame: 0,
            bpm: 120.0,
        };
        LoopMasterBus {
            sample_rate,
            fx: FxAutomation::new(120.0, 120.0),
            chain: MasterChain::new(sample_rate),
            unit: FxUnitLayer::new(sample_rate),
            brake: TurntableBrake::new(sample_rate),
            commands: VecDeque::with_capacity(Self::COMMAND_CAPACITY),
            transport_frames: 0,
            block_transport: 0.0,
            block_frames: 0,
            pending: [none; Self::PENDING_CAPACITY],
            pending_count: 0,
            ops: Box::new(OpQueue {
                n: 0,
                index: [0; OP_CAPACITY],
                time: [0.0; OP_CAPACITY],
                key: [MasterFxKey::HpFreq; OP_CAPACITY],
                value: [0.0; OP_CAPACITY],
                live: [false; OP_CAPACITY],
            }),
            events: Vec::with_capacity(EVENT_CAPACITY),
            seen: 0,
            engaged: false,
            engaged_from: 0,
            mask: 0,
            hp_corner: 20.0,
            lp_corner: 20_000.0,
            idle: true,
        }
    }

    // MARK: what the last block published

    /// Did the last block change the audio?
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    pub fn engaged_from(&self) -> usize {
        self.engaged_from
    }

    /// The macros running or waiting for their bar, as bits ([`MasterMacro::bit`]).
    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// Macro commands the audio thread has taken (`seenAtomic`).
    pub fn seen(&self) -> usize {
        self.seen
    }

    /// The chain's HP and LP corners (Hz) at the last block, and whether it was at rest (and the
    /// brake not running).
    pub fn filter_corners(&self) -> (f64, f64, bool) {
        (self.hp_corner, self.lp_corner, self.idle)
    }

    /// Audio thread, before `render`: drop a running brake where it is, without the catch-up (a
    /// sample deck's jog or transport stop). Waiting brake fires stay waiting.
    pub fn cancel_brake(&mut self) {
        self.brake.cancel();
    }

    /// Audio thread: is the brake running (spinning down or stopped, not yet released)?
    pub fn braking(&self) -> bool {
        self.brake.is_on()
    }

    // MARK: audio thread

    /// Queue a command for the next block. Nothing happens until [`render`](Self::render) takes
    /// it, at the top of the block, as the ring's pop does; a full queue drops it.
    pub fn send(&mut self, c: LoopMasterCommand) {
        if self.commands.len() < Self::COMMAND_CAPACITY {
            self.commands.push_back(c);
        }
    }

    /// Apply one command (the top of a block).
    fn apply(&mut self, c: LoopMasterCommand) {
        match c {
            LoopMasterCommand::Macro {
                id,
                bars,
                frame,
                bpm,
            } => {
                self.seen += 1;
                if self.pending_count >= Self::PENDING_CAPACITY {
                    return;
                }
                self.pending[self.pending_count] = Pending {
                    id,
                    bars,
                    frame,
                    bpm,
                };
                self.pending_count += 1;
            }
            LoopMasterCommand::Slider { key, value } => {
                if let Some(k) = MasterFxKey::from_index(key as usize) {
                    self.fx.set_slider(k, value as f64);
                }
            }
            LoopMasterCommand::Panic => {
                self.pending_count = 0;
                self.brake.panic();
                let events = &mut self.events;
                self.fx.panic(&mut |e| {
                    if events.len() < EVENT_CAPACITY {
                        events.push(e)
                    }
                });
                self.dispatch_events();
            }
            LoopMasterCommand::Release { id } => {
                if let Some(m) = MasterMacro::from_raw(id) {
                    let events = &mut self.events;
                    self.fx.release(m, &mut |e| {
                        if events.len() < EVENT_CAPACITY {
                            events.push(e)
                        }
                    });
                    self.dispatch_events();
                }
            }
            LoopMasterCommand::FxUnit(u) => self.unit.take(u),
        }
    }

    /// Render `frames` of `source` (the bank's summed voices) through the brake and the chain
    /// into `left` (`right`), the block starting at loop-channel frame `s`. Real-time safe.
    pub fn render<S: BrakeSource>(
        &mut self,
        source: &mut S,
        frames: usize,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        s: i64,
    ) {
        let n = frames;
        let sr = self.sample_rate;
        self.block_transport = self.transport_frames as f64 / sr;
        self.block_frames = n;
        self.ops.n = 0;
        self.fx.set_now(self.block_transport);
        let braking = self.brake.is_on();
        let idle_before = self.chain.is_idle(0);
        while let Some(c) = self.commands.pop_front() {
            self.apply(c);
        }
        {
            let t0 = self.chain.time_at(0);
            let ops = &mut self.ops;
            self.unit
                .begin_block(n as i64, s, &self.fx, &mut |key, value| {
                    push_op(ops, n, sr, t0, key, value, 0.0, true)
                });
        }
        // the fires due in this block, in frame order; the source rendered up to each brake
        let mut done = 0usize;
        let mut first_change = n;
        while let Some(i) = self.next_due(s + n as i64) {
            let k = (self.pending[i].frame - s).max(0).min(n as i64) as usize;
            let t = self.block_transport + k as f64 / sr;
            self.advance_to(t);
            self.fx.set_now(t);
            let Pending { id, bars, bpm, .. } = self.pending[i];
            self.remove_pending(i);
            let Some(m) = MasterMacro::from_raw(id) else {
                continue;
            };
            if m == MasterMacro::VinylBrake {
                if k > done {
                    self.brake.render(
                        source,
                        &mut left[done..k],
                        right.as_deref_mut().map(|r| &mut r[done..k]),
                        k - done,
                        (s + done as i64) as usize,
                    );
                    done = k;
                }
                let b = if bars <= 0 {
                    MasterMacro::VinylBrake.default_duration()
                } else {
                    bars as i32
                };
                first_change = first_change.min(k);
                self.brake
                    .start(libm::round((b * 4) as f64 * 60.0 / bpm * sr) as usize);
            } else {
                self.fx.tempo = bpm;
                self.fx.genre_bpm = bpm;
                let events = &mut self.events;
                self.fx
                    .fire(m, (bars >= 0).then_some(bars as i32), &mut |e| {
                        if events.len() < EVENT_CAPACITY {
                            events.push(e)
                        }
                    });
                self.dispatch_events();
            }
        }
        self.advance_to(self.block_transport + n as f64 / sr);
        self.transport_frames += n as i64;
        if n > done {
            self.brake.render(
                source,
                &mut left[done..n],
                right.as_deref_mut().map(|r| &mut r[done..n]),
                n - done,
                (s + done as i64) as usize,
            );
        }
        let mut from = 0usize;
        for i in 0..self.ops.n {
            let k = self.ops.index[i];
            if k > from {
                self.chain.process(left, right.as_deref_mut(), from, k);
                from = k;
            }
            let live = self.ops.live[i].then_some(n as f64 / sr);
            self.chain
                .apply_live(self.ops.key[i], self.ops.value[i], self.ops.time[i], live);
        }
        self.chain.process(left, right, from, n);
        self.chain.finish(n);
        let idle = self.chain.is_idle(0);
        self.engaged = braking || self.brake.is_on() || !idle_before || !idle || self.ops.n > 0;
        for i in 0..self.ops.n {
            first_change = first_change.min(self.ops.index[i]);
        }
        self.engaged_from = if !self.engaged {
            n
        } else if braking || !idle_before || first_change >= n {
            0
        } else {
            first_change
        };
        let mut mask = self.fx.active_mask()
            | if self.brake.is_on() {
                MasterMacro::VinylBrake.bit()
            } else {
                0
            };
        for i in 0..self.pending_count {
            mask |= 1u32 << self.pending[i].id as u32;
        }
        self.mask = mask;
        let (hp, lp) = self.chain.filter_corners(0);
        self.hp_corner = hp;
        self.lp_corner = lp;
        self.idle = idle && !self.brake.is_on();
    }

    /// `fx.advance(until:)` with the events dispatched to the chain's ops.
    fn advance_to(&mut self, t: f64) {
        let events = &mut self.events;
        self.fx.advance(t, &mut |e| {
            if events.len() < EVENT_CAPACITY {
                events.push(e)
            }
        });
        self.dispatch_events();
    }

    /// The earliest pending fire due before loop frame `end` (ties: the first sent).
    fn next_due(&self, end: i64) -> Option<usize> {
        let mut best: Option<usize> = None;
        for i in 0..self.pending_count {
            if self.pending[i].frame < end
                && best.is_none_or(|b| self.pending[i].frame < self.pending[b].frame)
            {
                best = Some(i);
            }
        }
        best
    }

    fn remove_pending(&mut self, i: usize) {
        self.pending_count -= 1;
        for j in i..self.pending_count {
            self.pending[j] = self.pending[j + 1];
        }
    }

    /// The automation's events during a block (`handle`): slider dispatches queued for the chain
    /// at their sample; tempo writes (the Feel puck's tempo axis) dropped, the loops keep the
    /// decks' tempo. The events are collected while the automation runs and handled right
    /// after, each carrying whether its key was animating at its own instant (as `DeckCore`
    /// does; `LoopMasterBus` calls `unit.automated` inline in Swift).
    fn dispatch_events(&mut self) {
        let (frames, sr) = (self.block_frames, self.sample_rate);
        let t0 = self.chain.time_at(0);
        for i in 0..self.events.len() {
            if let FxAutomationEvent::Fx {
                time,
                key,
                value,
                animating,
            } = self.events[i]
            {
                // the macro's slider, on the FX unit's setting (at neutral, the slider)
                let v = self.unit.automated_at(key, value, animating);
                push_op(
                    &mut self.ops,
                    frames,
                    sr,
                    t0,
                    key,
                    v,
                    time - self.block_transport,
                    false,
                );
            }
        }
        self.events.clear();
    }
}

/// A chain op for this block, `offset` seconds into it (`queue`).
#[allow(clippy::too_many_arguments)]
fn push_op(
    ops: &mut OpQueue,
    frames: usize,
    sample_rate: f64,
    chain_t0: f64,
    key: MasterFxKey,
    value: f64,
    offset: f64,
    live: bool,
) {
    if ops.n >= OP_CAPACITY {
        return;
    }
    let offset = offset.max(0.0);
    let i = ops.n;
    ops.index[i] = frames.min(libm::ceil(offset * sample_rate).max(0.0) as usize);
    ops.time[i] = chain_t0 + offset;
    ops.key[i] = key;
    ops.value[i] = value;
    ops.live[i] = live;
    ops.n += 1;
}
