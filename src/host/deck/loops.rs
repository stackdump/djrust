//! LOOP record and the loop channel "L" (`LoopRecorder.swift`, `Audio/LoopBank.swift`).
//!
//! Hold a deck's LOOP and its sound is taken a bar at a time; let go and the bars become a clip
//! on the row, looping on the loop channel under the mixer. Pressing snaps back to the downbeat
//! of the bar already playing (the history is in the deck's [`CaptureTap`]); releasing records
//! on to the next downbeat, unless the release came within a sixteenth of one, which ends the
//! take there. A net deck's downbeats are the runner's (exact frames); a sample deck's come from
//! its beat grid. A tap (let go within a quarter second) with a length named records exactly
//! that many bars. [`Engine::grab_last_bars`] takes bars already played.
//!
//! The render side is [`LoopBank`]: [`LOOP_VOICES`] turntables, each sounding only while its
//! voice is on, started and stopped on exact frames with a short fade, each through its clip's
//! after effects (`LoopFxVoice`), summed, through the loops' master bus (`LoopMasterBus`: the
//! net decks' master chain and macros, and the vinyl brake) and the channel's safety limiter,
//! then into the master after the decks at the channel's level (fader x trim 1/sqrt 2,
//! `DJEngine.loopChannelGain`):
//!
//! ```text
//! voice: turntable -> clip FX -> level x fade \
//!                                              > sum -> [brake] -> master chain -> limiter -> fader
//! voice: turntable -> clip FX -> level x fade /
//! ```
//!
//! The main thread keeps the row and the voices' bookkeeping, and moves takes on in
//! [`Engine::service_loops`]. How the loops sound lives beside it (`LoopSound.swift`): mute and
//! solo as gains, the row's and a clip's own fade, key lock (stretched copies rendered on a
//! worker thread, swapped in in phase), and the loops' master FX (`LoopMasterFx.swift`: a fire
//! lands on the next launch line, at the loop clock's tempo).
//!
//! The row's clips are `crate::deck::clips::LoopClip`s on a `crate::deck::row::ClipRow`
//! (ids are `SampleBuffer::content_id`, sha256); the voices, launch scheduling, tempo follow and
//! the countdown are here, the row's edits in [`super::clips`], scenes and the row's settings in
//! [`super::launch`], the `DeckAction` glue in [`super::loop_actions`], the library on disk in
//! `stores::loops`. The retune render (`ClipRetune.render`) is [`super::retune`].
//!
//! Beyond Swift (docs/HUB.md, docs/INPUT-HISTORY.md): clips captured from an input strip
//! ([`super::input_capture`]), and the pads in [`super::loop_pads`]: keys that transpose the
//! loops (a pitch-shifted copy swapped in on the grid, or tape speed), file pads and one-shots
//! with a gain and a choke group. What those add to a clip is kept beside the row
//! ([`super::loop_pads::ClipExtra`]), not on `djengine`'s `LoopClip`.
//!
//! [`CaptureTap`]: super::capture::CaptureTap

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use crate::control::controller::DeckView;
use crate::deck::beat::BeatState;
use crate::deck::clips::LoopFade;
use crate::deck::clock::{
    anchor_origin, retime_waiting, select_clock, ClockDeck, ClockVoice, LoopClock, RetimeMove,
    RetimeVoice, WaitingBoundary,
};
use crate::deck::loop_sound::{self, KeyLockState, KeyLockStep};
use crate::deck::loops::{channel_level, update_dominant_deck, DominantCandidate, LoopFollow};
use crate::deck::row::ClipRow;
use crate::deck::turntable::{SampleBuffer, Turntable, TurntableCommand};
use crate::deck::waveform::WaveformOverview;
use crate::fx::brake::BrakeSource;
use crate::fx::loop_fx::{LoopFxLimiter, LoopFxParams, LoopFxVoice};
use crate::fx::loop_master::{LoopMasterBus, LoopMasterCommand};
use crate::fx::unit::FxUnitCommand;
use crate::fx::{
    time_stretch, MasterFxKey, MasterMacro, ReverbKnob, StripEq, StripKnob, StripReverb,
};
use crate::json::JsonObject;
use crate::session::types::LoopFx;
use crate::wave::{TickAnchor, TickAnchorFlags};

use super::engine::{crossfade_gain, trim_for_gain, Engine, MainDeck, Msg, MAX_BLOCK};

pub use crate::deck::clips::LoopClip;
pub use crate::deck::loops::LoopState;

/// Clips that can loop at once.
pub const LOOP_VOICES: usize = 16;
/// The loop channel's trim: the centred crossfader's gain on A and B, which L doesn't take.
pub const LOOP_TRIM: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// How far ahead of the render head a start or stop is scheduled at the least (~43 ms).
pub const LOOP_LEAD: i64 = crate::deck::clock::LOOP_LEAD;
/// The loop channel's knob in a new engine: trim 1/sqrt 2 (`DJEngine.loopChannelGain`).
pub const LOOP_CHANNEL_GAIN: f64 = 0.25;
/// A new channel's fader (`Deck.volume`'s default).
pub const LOOP_VOLUME: f64 = 0.8;
/// A take's longest hold, bars (the capture holds ~22 s; a take copies out as it goes).
const MAX_TAKE_BARS: i64 = 64;

// MARK: render side

/// Main thread to the loop bank (`LoopBankCommand`, plus the turntables' and the master bus's).
pub enum LoopMsg {
    /// To voice `v`'s turntable.
    Voice(usize, TurntableCommand),
    /// Start voice `v` at frame `frame` (already past: at once), fading in over `fade` frames.
    /// On a voice already sounding it cancels a stop and brings a fade-out back up.
    Start { v: usize, frame: i64, fade: i64 },
    /// [`LoopMsg::Start`], and the voice goes out again at `end` (a one-shot; beyond Swift).
    StartOnce {
        v: usize,
        frame: i64,
        fade: i64,
        end: i64,
    },
    /// Stop voice `v` at frame `frame`, falling to 0 over `fade` frames from there.
    Stop { v: usize, frame: i64, fade: i64 },
    /// At the block holding `frame`, hand voice `v` another take of the same loop (the same
    /// length, so it stays in phase): a transposed copy, at turntable pitch `pitch` (beyond
    /// Swift: `loop_pads`).
    SwapAt {
        v: usize,
        frame: i64,
        buffer: Arc<SampleBuffer>,
        pitch: f64,
    },
    /// Voice `v`'s level (its gain x mute x solo), ramped to over ~5 ms.
    Gain(usize, f32),
    /// Voice `v`'s level from frame `frame` (a scene's level for a clip already sounding,
    /// changed on the launch boundary), ramped to like `Gain`. A plain `Gain` cancels it.
    GainAt(usize, f32, i64),
    /// Voice `v`'s after effects: smoothed and crossfaded while it sounds, at once while it is
    /// silent. A plain `Fx` cancels a pending `FxAt`.
    Fx(usize, LoopFxParams),
    /// Voice `v`'s after effects from frame `frame` (a scene's FX for a clip already sounding),
    /// taken like `Fx`.
    FxAt(usize, LoopFxParams, i64),
    /// The loops' master bus (`LoopMasterBus`).
    Master(LoopMasterCommand),
    /// The channel fader, 0...1.
    Volume(f64),
    /// The channel's trim, from its knob (`Deck.trim`): `2^((gain - 0.5) * 2)`.
    Trim(f64),
    /// The channel's mute (`Deck.mute`): the strip's volume is 0.
    Mute(bool),
    /// A knob of the channel's strip (`loopChannel.filter` / `eqLow` / `eqMid` / `eqHigh`), 0...1:
    /// the EQ and DJ filter every `Deck` has, in front of its fader.
    Strip(StripKnob, f64),
    /// A knob of the channel's reverb (`loopChannel.reverb.wet / size / tone`), 0...1: after the
    /// EQ, before the fader, as on every `Deck`.
    Reverb(ReverbKnob, f64),
}

/// What the loop bank publishes once per block.
pub struct LoopMirror {
    /// Frames the mixer has rendered (the loops' and the decks' time base).
    pub rendered: AtomicI64,
    active: AtomicU64,
    pos: [AtomicU64; LOOP_VOICES],
    len: [AtomicU64; LOOP_VOICES],
    level: AtomicU32,
    /// Voices whose FX chain is in the signal path.
    fx_running: AtomicU32,
    /// The master bus: the macros running or waiting, the macro commands it has taken, the
    /// chain's filter corners (f64 bits) and whether it was at rest.
    master_mask: AtomicU32,
    master_seen: AtomicUsize,
    hp_bits: AtomicU64,
    lp_bits: AtomicU64,
    master_idle: AtomicBool,
    limiter_idle: AtomicBool,
}

impl LoopMirror {
    fn new() -> LoopMirror {
        LoopMirror {
            rendered: AtomicI64::new(0),
            active: AtomicU64::new(0),
            pos: std::array::from_fn(|_| AtomicU64::new(0)),
            len: std::array::from_fn(|_| AtomicU64::new(0)),
            level: AtomicU32::new(0),
            fx_running: AtomicU32::new(0),
            master_mask: AtomicU32::new(0),
            master_seen: AtomicUsize::new(0),
            hp_bits: AtomicU64::new(20.0f64.to_bits()),
            lp_bits: AtomicU64::new(20_000.0f64.to_bits()),
            master_idle: AtomicBool::new(true),
            limiter_idle: AtomicBool::new(true),
        }
    }
    /// Is voice `v` sounding (a fade-out still is)?
    pub fn is_active(&self, v: usize) -> bool {
        self.active.load(Ordering::Relaxed) & (1 << v) != 0
    }
    /// Voice `v`'s unwrapped playhead and loaded length (samples), as of the last block.
    pub fn playhead(&self, v: usize) -> (f64, usize) {
        (
            f64::from_bits(self.pos[v].load(Ordering::Relaxed)),
            self.len[v].load(Ordering::Relaxed) as usize,
        )
    }
    /// The channel's peak at the last block, after its fader.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    /// Is voice `v`'s FX chain in the signal path (`LoopFxVoice.running`) as of the last block?
    pub fn fx_running(&self, v: usize) -> bool {
        self.fx_running.load(Ordering::Relaxed) & (1 << v) != 0
    }
    /// Has the channel's safety limiter let go (gain back at 1) as of the last block?
    pub fn limiter_idle(&self) -> bool {
        self.limiter_idle.load(Ordering::Relaxed)
    }
}

/// The voices of the bank: each a turntable through its FX and its level x envelope, summed.
/// The brake's source (`LoopBank: BrakeSource`): the vinyl brake slows the summed voices, its
/// lag counted in frames per voice that sounded through it, and each such voice skips on by its
/// own lag at the release, so a loop lands where an unbraked one would be.
struct VoiceMix {
    voices: Vec<Turntable>,
    /// Each voice's FX insert (after its playback, before its level x envelope).
    fx: Vec<LoopFxVoice>,
    /// Frames each voice fell behind under the bus's vinyl brake (made up at its release).
    brake_lag: [f64; LOOP_VOICES],
    /// The voices' render state for this block (`process` may run several times in one block).
    block_bits: u64,
    block_fx: bool,
    active: [bool; LOOP_VOICES],
    start_at: [i64; LOOP_VOICES],
    start_fade: [i64; LOOP_VOICES],
    stop_at: [i64; LOOP_VOICES],
    stop_fade: [i64; LOOP_VOICES],
    /// The level: where it is, where it is going, by how much per sample, and for how many more
    /// samples (it lands on the target exactly when that runs out).
    level: [f32; LOOP_VOICES],
    level_target: [f32; LOOP_VOICES],
    level_step: [f32; LOOP_VOICES],
    level_left: [i64; LOOP_VOICES],
    /// A level change waiting for its frame (`GainAt`): the level, and the frame (-1: none).
    pending_gains: [f32; LOOP_VOICES],
    gain_frames: [i64; LOOP_VOICES],
    /// FX waiting for their frame (`FxAt`): the settings, and the frame (-1: none).
    pending_fx: [LoopFxParams; LOOP_VOICES],
    fx_frames: [i64; LOOP_VOICES],
    /// The fade envelope, 0...1, its step per sample (> 0 fading in, < 0 fading out) and the
    /// samples to go.
    envelope: [f32; LOOP_VOICES],
    envelope_step: [f32; LOOP_VOICES],
    envelope_left: [i64; LOOP_VOICES],
    /// Frames a level change takes: ~5 ms.
    ramp_frames: i64,
    /// A swap waiting for its block (`SwapAt`): the frame, the audio, the pitch.
    swap: Vec<Option<(i64, Arc<SampleBuffer>, f64)>>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

/// The loop channel's audio: several clips looping at once, each on its own voice, summed.
/// Allocates nothing once built.
pub struct LoopBank {
    mix: VoiceMix,
    /// The loops' master bus ("Loops master FX", `LoopMasterBus`): the net decks' master chain
    /// and macros on the summed voices, before the limiter.
    master: LoopMasterBus,
    /// The channel's safety limiter (only while some voice has FX on: `LoopFx.swift`).
    limiter: LoopFxLimiter,
    /// The channel strip's EQ and DJ filter (`Deck.filterNode`), between the limiter and the fader.
    /// Every band bypassed (the knobs at rest) leaves the block as it is.
    eq: StripEq,
    /// The channel strip's reverb (`loopChannel.reverb`), between the EQ and the fader. Off, the
    /// block is left as it is.
    reverb: StripReverb,
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    volume: f64,
    trim: f64,
    mute: bool,
    last_gain: f64,
    mirror: Arc<LoopMirror>,
}

impl LoopBank {
    pub fn new(sample_rate: f64) -> (LoopBank, Arc<LoopMirror>) {
        let mirror = Arc::new(LoopMirror::new());
        let bank = LoopBank {
            mix: VoiceMix {
                voices: (0..LOOP_VOICES)
                    .map(|_| Turntable::new(sample_rate))
                    .collect(),
                fx: (0..LOOP_VOICES)
                    .map(|_| LoopFxVoice::new(sample_rate))
                    .collect(),
                brake_lag: [0.0; LOOP_VOICES],
                block_bits: 0,
                block_fx: false,
                active: [false; LOOP_VOICES],
                start_at: [-1; LOOP_VOICES],
                start_fade: [0; LOOP_VOICES],
                stop_at: [-1; LOOP_VOICES],
                stop_fade: [0; LOOP_VOICES],
                level: [1.0; LOOP_VOICES],
                level_target: [1.0; LOOP_VOICES],
                level_step: [0.0; LOOP_VOICES],
                level_left: [0; LOOP_VOICES],
                pending_gains: [1.0; LOOP_VOICES],
                gain_frames: [-1; LOOP_VOICES],
                pending_fx: [LoopFxParams::default(); LOOP_VOICES],
                fx_frames: [-1; LOOP_VOICES],
                envelope: [1.0; LOOP_VOICES],
                envelope_step: [0.0; LOOP_VOICES],
                envelope_left: [0; LOOP_VOICES],
                ramp_frames: ((sample_rate * 0.005) as i64).max(1),
                swap: (0..LOOP_VOICES).map(|_| None).collect(),
                scratch_l: vec![0.0; MAX_BLOCK],
                scratch_r: vec![0.0; MAX_BLOCK],
            },
            master: LoopMasterBus::new(sample_rate),
            limiter: LoopFxLimiter::new(sample_rate),
            eq: StripEq::new(sample_rate),
            reverb: StripReverb::new(sample_rate),
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            volume: LOOP_VOLUME,
            trim: LOOP_TRIM,
            mute: false,
            last_gain: LOOP_VOLUME * LOOP_TRIM,
            mirror: mirror.clone(),
        };
        (bank, mirror)
    }

    /// Frames a level change takes: ~5 ms.
    pub fn ramp_frames(&self) -> i64 {
        self.mix.ramp_frames
    }

    /// Voice `v`'s turntable (tests, measurement).
    pub fn voice(&self, v: usize) -> &Turntable {
        &self.mix.voices[v]
    }

    /// Is voice `v`'s FX chain in the signal path?
    pub fn fx_running(&self, v: usize) -> bool {
        self.mix.fx[v].running()
    }

    /// The channel's safety limiter.
    pub fn limiter(&self) -> &LoopFxLimiter {
        &self.limiter
    }

    /// The loops' master bus.
    pub fn master(&self) -> &LoopMasterBus {
        &self.master
    }

    /// Is voice `v` sounding as of the last block? A voice fading out still is.
    pub fn is_active(&self, v: usize) -> bool {
        self.mix.block_bits & (1 << v) != 0
    }

    pub fn apply(&mut self, m: LoopMsg) {
        let x = &mut self.mix;
        match m {
            LoopMsg::Voice(v, c) => {
                if let Some(t) = x.voices.get_mut(v) {
                    t.send(c);
                }
            }
            LoopMsg::Start { v, frame, fade } if v < LOOP_VOICES => {
                x.stop_at[v] = -1;
                if !x.active[v] {
                    x.start_at[v] = frame;
                    x.start_fade[v] = fade;
                } else if x.envelope_step[v] < 0.0 || x.envelope[v] < 1.0 {
                    // on its way out: back up
                    x.ramp(v, 1.0, fade.max(x.ramp_frames));
                }
            }
            LoopMsg::StartOnce {
                v,
                frame,
                fade,
                end,
            } if v < LOOP_VOICES => {
                self.apply(LoopMsg::Start { v, frame, fade });
                let x = &mut self.mix;
                x.stop_at[v] = end;
                x.stop_fade[v] = fade;
            }
            LoopMsg::SwapAt {
                v,
                frame,
                buffer,
                pitch,
            } if v < LOOP_VOICES => {
                // one waiting already goes in now, so it retires through the turntable (freed
                // off this thread) and not by being dropped here
                if let Some((_, b, p)) = x.swap[v].take() {
                    x.voices[v].send(TurntableCommand::Swap(b, p));
                }
                x.swap[v] = Some((frame, buffer, pitch));
            }
            LoopMsg::Stop { v, frame, fade } if v < LOOP_VOICES => {
                x.start_at[v] = -1;
                if x.active[v] {
                    x.stop_at[v] = frame;
                    x.stop_fade[v] = fade;
                }
            }
            LoopMsg::Gain(v, g) if v < LOOP_VOICES => {
                x.gain_frames[v] = -1;
                x.set_level(v, g);
            }
            LoopMsg::GainAt(v, g, f) if v < LOOP_VOICES => {
                x.pending_gains[v] = g;
                x.gain_frames[v] = f;
            }
            LoopMsg::Fx(v, p) if v < LOOP_VOICES => {
                x.fx_frames[v] = -1;
                let sounding = x.active[v];
                x.fx[v].set(&p, sounding);
            }
            LoopMsg::FxAt(v, p, f) if v < LOOP_VOICES => {
                x.pending_fx[v] = p;
                x.fx_frames[v] = f;
            }
            LoopMsg::Master(c) => self.master.send(c),
            LoopMsg::Volume(v) => self.volume = v.clamp(0.0, 1.0),
            LoopMsg::Trim(t) => self.trim = t,
            LoopMsg::Mute(m) => self.mute = m,
            LoopMsg::Strip(k, v) => self.eq.set(k, v),
            LoopMsg::Reverb(k, v) => self.reverb.set(k, v),
            _ => {}
        }
    }

    /// The channel strip's reverb (tests, measurement).
    pub fn reverb(&self) -> &StripReverb {
        &self.reverb
    }

    /// Buffers a voice let go of (to the retire ring).
    pub(crate) fn pop_retired(&mut self) -> Option<Arc<SampleBuffer>> {
        self.mix.voices.iter_mut().find_map(|t| t.pop_retired())
    }

    /// Render the bank's frames `s..s + n` into `l` / `r` (`LoopBank.render`, which replaces
    /// them): the voices summed, the loops' master bus, the limiter. Without the channel's
    /// fader and trim. Real-time safe.
    pub fn render_bank(&mut self, s: i64, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        self.mix.block_fx = false;
        self.master.render(&mut self.mix, n, l, Some(r), s);
        if self.mix.block_fx || self.master.engaged() || !self.limiter.idle() {
            self.limiter.process(l, Some(r), n);
        }
    }

    /// Add the loops' frames `s..s + n` into `out_l` / `out_r` at the channel's level.
    pub(crate) fn render(&mut self, s: i64, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        self.mix.block_fx = false;
        self.master.render(
            &mut self.mix,
            n,
            &mut self.l[..n],
            Some(&mut self.r[..n]),
            s,
        );
        if self.mix.block_fx || self.master.engaged() || !self.limiter.idle() {
            self.limiter
                .process(&mut self.l[..n], Some(&mut self.r[..n]), n);
        }
        // the strip's EQ and DJ filter, then its reverb, then its volume
        self.eq.process(&mut self.l[..n], &mut self.r[..n]);
        self.reverb.process(&mut self.l[..n], &mut self.r[..n]);
        // the strip's volume, `applyGains`: 0 when muted, else trim x fader (no crossfader on L)
        let target = if self.mute {
            0.0
        } else {
            self.trim * self.volume
        };
        let (g0, step) = (self.last_gain, (target - self.last_gain) / n.max(1) as f64);
        self.last_gain = target;
        let mut peak = 0f32;
        for i in 0..n {
            let g = (g0 + step * (i + 1) as f64) as f32;
            let (l, r) = (g * self.l[i], g * self.r[i]);
            out_l[i] += l;
            out_r[i] += r;
            peak = peak.max(l.abs()).max(r.abs());
        }
        let m = &*self.mirror;
        let x = &self.mix;
        m.active.store(x.block_bits, Ordering::Relaxed);
        let mut fx_bits = 0u32;
        for v in 0..LOOP_VOICES {
            m.pos[v].store(x.voices[v].position().to_bits(), Ordering::Relaxed);
            m.len[v].store(x.voices[v].length() as u64, Ordering::Relaxed);
            if x.fx[v].running() {
                fx_bits |= 1 << v;
            }
        }
        m.fx_running.store(fx_bits, Ordering::Relaxed);
        m.level.store(peak.to_bits(), Ordering::Relaxed);
        m.master_mask.store(self.master.mask(), Ordering::Relaxed);
        m.master_seen.store(self.master.seen(), Ordering::Release);
        let (hp, lp, idle) = self.master.filter_corners();
        m.hp_bits.store(hp.to_bits(), Ordering::Relaxed);
        m.lp_bits.store(lp.to_bits(), Ordering::Relaxed);
        m.master_idle.store(idle, Ordering::Relaxed);
        m.limiter_idle.store(self.limiter.idle(), Ordering::Relaxed);
        m.rendered.store(s + n as i64, Ordering::Relaxed);
    }
}

/// What `loopClock` reads of a net deck's clock: the newest of the last four tick anchors the
/// deck's render thread wrote (`wave.anchors`, from `written - 4`) that has a tick of 1 or more,
/// as [`anchor_origin`] gives it: an exact frame and beat where the deck's `beatState` is the
/// last block's estimate. A sample deck has none (`ref.mode == .net`).
fn newest_anchor(d: &MainDeck) -> Option<(i64, f64)> {
    if !d.is_net() {
        return None;
    }
    let ring = &d.mirror.anchors;
    let none = TickAnchor {
        frame: 0,
        tick: 0,
        generation: 0,
        tempo: 0.0,
        flags: TickAnchorFlags::default(),
    };
    let mut last = [none; 4];
    let (n, _lost) = ring.read(ring.written().saturating_sub(4), &mut last);
    last[..n]
        .iter()
        .rev()
        .find(|a| a.tick >= 1)
        .map(|a| anchor_origin(a.frame, a.tick as i64))
}

impl VoiceMix {
    /// The voices' frames `s..s + n` summed into `left` / `right` (replacing them): the dry side
    /// of the master bus (several calls a block while its brake runs, each the next source
    /// frames) (`mixVoices`).
    fn mix_voices(&mut self, n: usize, left: &mut [f32], right: &mut [f32], s: i64) {
        left[..n].fill(0.0);
        right[..n].fill(0.0);
        let end = s + n as i64;
        for v in 0..LOOP_VOICES {
            if self.swap[v].as_ref().is_some_and(|w| w.0 < end) {
                if let Some((_, b, p)) = self.swap[v].take() {
                    self.voices[v].send(TurntableCommand::Swap(b, p));
                }
            }
        }
        let mut bits = 0u64;
        let mut any_fx = false;
        for v in 0..LOOP_VOICES {
            let mut from = 0usize;
            // a voice that is not sounding takes a level change due in this block at once
            if !self.active[v] && self.gain_frames[v] >= 0 && self.gain_frames[v] < end {
                self.apply_pending_gain(v);
            }
            if !self.active[v] && self.fx_frames[v] >= 0 && self.fx_frames[v] < end {
                self.apply_pending_fx(v);
            }
            if !self.active[v] {
                if self.start_at[v] < 0 || self.start_at[v] >= end {
                    continue;
                }
                from = (self.start_at[v] - s).max(0) as usize;
                self.start_at[v] = -1;
                self.active[v] = true;
                let f = self.start_fade[v];
                self.envelope[v] = if f > 0 { 0.0 } else { 1.0 };
                self.envelope_step[v] = if f > 0 { 1.0 / f as f32 } else { 0.0 };
                self.envelope_left[v] = f;
            }
            let mut to = n;
            let mut stops = false;
            if self.stop_at[v] >= 0 && self.stop_at[v] < end {
                let at = ((self.stop_at[v] - s).max(0) as usize).max(from);
                self.stop_at[v] = -1;
                if self.stop_fade[v] > 0 {
                    // sound on to the stop, then fall from wherever the envelope is to 0
                    stops = self.mix_across(v, from, at, s, left, right);
                    self.ramp(v, 0.0, self.stop_fade[v]);
                    from = if stops { n } else { at };
                } else {
                    to = at;
                    stops = true;
                }
            }
            if from < to && self.mix_across(v, from, to, s, left, right) {
                stops = true;
            }
            if stops {
                self.active[v] = false;
                self.envelope_step[v] = 0.0;
                self.envelope_left[v] = 0;
            } else {
                bits |= 1 << v;
            }
            if self.fx[v].running() {
                any_fx = true;
            }
        }
        self.block_bits = bits;
        if any_fx {
            self.block_fx = true;
        }
    }

    fn apply_pending_fx(&mut self, v: usize) {
        let (p, sounding) = (self.pending_fx[v], self.active[v]);
        self.fx[v].set(&p, sounding);
        self.fx_frames[v] = -1;
    }

    /// `mixAcrossGain`, with an `FxAt` due inside `from..to` (block starting at frame `s`)
    /// taking effect on its frame: the part before it through the old FX, the rest through the
    /// new.
    fn mix_across(
        &mut self,
        v: usize,
        from: usize,
        to: usize,
        s: i64,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let f = self.fx_frames[v];
        if !(f >= 0 && f - s < to as i64) {
            return self.mix_across_gain(v, from, to, s, left, right);
        }
        let k = (f - s).max(from as i64).min(to as i64) as usize;
        if self.mix_across_gain(v, from, k, s, left, right) {
            return true;
        }
        self.apply_pending_fx(v);
        self.mix_across_gain(v, k, to, s, left, right)
    }

    /// Head voice `v`'s level for `g`: over the ramp while it sounds, at once while it is silent
    /// (nothing to ramp from) or already there.
    fn set_level(&mut self, v: usize, g: f32) {
        self.level_target[v] = g;
        if self.active[v] && g != self.level[v] {
            self.level_step[v] = (g - self.level[v]) / self.ramp_frames as f32;
            self.level_left[v] = self.ramp_frames;
        } else {
            self.level[v] = g;
            self.level_step[v] = 0.0;
            self.level_left[v] = 0;
        }
    }

    fn apply_pending_gain(&mut self, v: usize) {
        let g = self.pending_gains[v];
        self.set_level(v, g);
        self.gain_frames[v] = -1;
    }

    /// `mix`, with a `GainAt` due inside `from..to` (block starting at frame `s`) taking effect
    /// on its frame: the part before it at the old level, the ramp from there.
    fn mix_across_gain(
        &mut self,
        v: usize,
        from: usize,
        to: usize,
        s: i64,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let g = self.gain_frames[v];
        if !(g >= 0 && g - s < to as i64) {
            return self.mix(v, from, to, left, right);
        }
        let k = (g - s).max(from as i64).min(to as i64) as usize;
        if self.mix(v, from, k, left, right) {
            return true;
        }
        self.apply_pending_gain(v);
        self.mix(v, k, to, left, right)
    }

    /// Move voice `v`'s envelope from where it is to `target` (0 or 1) over `frames` samples.
    fn ramp(&mut self, v: usize, target: f32, frames: i64) {
        let f = frames.max(1);
        let mut step = (target - self.envelope[v]) / f as f32;
        if step == 0.0 {
            step = if target > 0.0 {
                f32::EPSILON
            } else {
                -f32::EPSILON
            };
        }
        self.envelope_step[v] = step;
        self.envelope_left[v] = f;
    }

    /// Add voice `v`'s frames `from..to` to the output at its level x envelope, moving both on
    /// per sample. True if a fade-out reached 0 on the way (the rest is not added).
    fn mix(
        &mut self,
        v: usize,
        from: usize,
        to: usize,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let k = to - from;
        let (sl, sr) = (&mut self.scratch_l[..k], &mut self.scratch_r[..k]);
        self.voices[v].render(k, sl, Some(&mut *sr));
        self.fx[v].process(sl, sr, k);
        let (mut lv, ls, mut ln, lt) = (
            self.level[v],
            self.level_step[v],
            self.level_left[v],
            self.level_target[v],
        );
        let (mut e, mut es, mut en) = (
            self.envelope[v],
            self.envelope_step[v],
            self.envelope_left[v],
        );
        let mut ended = false;
        for j in 0..k {
            let g = lv * e;
            left[from + j] += sl[j] * g;
            right[from + j] += sr[j] * g;
            if ln > 0 {
                ln -= 1;
                lv = if ln == 0 { lt } else { lv + ls };
            }
            if en > 0 {
                en -= 1;
                e += es;
                if en == 0 {
                    // landed: exactly on 1, or on 0 and out
                    e = if es > 0.0 { 1.0 } else { 0.0 };
                    ended = es < 0.0;
                    es = 0.0;
                    if ended {
                        break;
                    }
                }
            }
        }
        self.level[v] = lv;
        self.level_left[v] = ln;
        self.envelope[v] = e;
        self.envelope_step[v] = es;
        self.envelope_left[v] = en;
        ended
    }
}

impl BrakeSource for VoiceMix {
    fn playing(&self) -> bool {
        true
    }
    fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        start_frame: usize,
    ) {
        if let Some(right) = right {
            self.mix_voices(frames, left, right, start_frame as i64);
        }
    }
    fn brake_lag(&self, _sample_rate: f64) -> f64 {
        1.0
    }
    fn brake_chunk(&mut self, output: usize, pulled: usize) {
        let lag = output as f64 - pulled as f64;
        for v in 0..LOOP_VOICES {
            if self.active[v] {
                self.brake_lag[v] += lag;
            }
        }
    }
    fn catch_up(&mut self, _ticks: f64) {
        for v in 0..LOOP_VOICES {
            if self.brake_lag[v] > 0.0 {
                if self.active[v] {
                    self.voices[v].skip(self.brake_lag[v]);
                }
                self.brake_lag[v] = 0.0;
            }
        }
    }
}

// MARK: main thread

#[derive(Clone, Default)]
pub(crate) struct LoopVoice {
    pub(crate) clip: Option<String>,
    /// The deck whose tempo it follows.
    pub(crate) reference: Option<String>,
    /// Its turntable's speed: `target_bpm / buffer_bpm`.
    pub(crate) ratio: f64,
    /// The tempo the loaded audio plays at at speed 1: the clip's own, or a stretched copy's.
    pub(crate) buffer_bpm: f64,
    /// The tempo it is to play at: its reference deck's, else the one it started at.
    pub(crate) target_bpm: f64,
    /// Set once it is told to stop (it goes out at this frame).
    pub(crate) stop_frame: Option<i64>,
    /// The frame it was told to come in at.
    pub(crate) start_frame: Option<i64>,
    /// When its pending start or stop was scheduled (the countdown's full length).
    pub(crate) scheduled_at: Option<i64>,
    /// A level change waiting for a frame (a scene's, on its boundary): the clip's level until
    /// then and the frame.
    gain_until: Option<(f64, i64)>,
    /// Samples in the audio it was started with (a stretched copy's, perhaps).
    pub(crate) buffer_length: f64,
    /// The boundary its pending start or stop waits for (`LoopVoice.boundary`).
    pub(crate) boundary: Option<WaitingBoundary>,
    /// Key lock: the tempo a stretch is wanted for and the frame it was first wanted at (it is
    /// rendered once that has held for `STRETCH_DEBOUNCE`), and the render in flight.
    stretch_want: Option<(f64, i64)>,
    stretching: Option<u64>,
}

/// A stretched copy a worker finished: the render's token, the clip, the audio.
type Stretched = (u64, String, Arc<SampleBuffer>);

/// A take in progress on one deck.
pub(crate) struct LoopTake {
    pub(crate) start: i64,
    pub(crate) start_beat: f64,
    pub(crate) pressed: i64,
    pub(crate) fixed_bars: Option<i64>,
    pub(crate) tapped: bool,
    pub(crate) bar_frames: f64,
    pub(crate) down_cursor: u64,
    pub(crate) downs: Vec<(i64, f64)>,
    pub(crate) released: Option<i64>,
    pub(crate) end: Option<i64>,
    pub(crate) bars: i64,
    pub(crate) cursor: i64,
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    /// The deck as LOOP went down (`LoopProvenance.capture`), and the sample it was playing.
    pub(crate) provenance: JsonObject,
    pub(crate) sample: Option<Arc<SampleBuffer>>,
}

/// The loop row and channel as the main thread keeps them.
pub struct LoopRow {
    /// The clips, the removed ones, their levels and settings, the sample library, the scenes
    /// (`DJEngine.clips` and friends): `crate::deck::row`.
    pub row: ClipRow,
    pub(crate) voices: Vec<LoopVoice>,
    pub(crate) takes: BTreeMap<String, LoopTake>,
    /// Launched stinger loops, by clip id, and the frame each is to go out at, scheduled once its
    /// voice is in (`DJEngine.stingerStops`, `serviceStingerStops`).
    pub(crate) stinger_stops: BTreeMap<String, i64>,
    /// The channel fader (`loopChannel.volume`).
    pub volume: f64,
    /// The channel's trim knob, 0...1 (`loopChannel.gain`; trim `2^((gain - 0.5) * 2)`).
    pub gain: f64,
    /// The channel's mute.
    pub mute: bool,
    /// What loops start and stop on, beats (a bar).
    pub quantum: i64,
    /// Bars a quick tap of LOOP records (None: a tap is a short hold).
    pub tap_take_bars: Option<i64>,
    /// Whose tempo the loops follow (`loopFollow`): the louder deck by default.
    pub follow: LoopFollow,
    /// The deck the loops follow under `Louder` (`dominantDeck`).
    pub(crate) dominant_deck: RefCell<Option<String>>,
    pub mirror: Arc<LoopMirror>,
    /// How loops come in and go out, the row's (`loopFade`); a clip's own is in `row.settings`.
    loop_fade: LoopFade,
    /// Key lock, the row's (`loopKeyLock`), and the stretched copies made so far: the tempo
    /// they play at and their audio (`stretchCache`).
    loop_key_lock: bool,
    pub(crate) stretch_cache: BTreeMap<String, (f64, Arc<SampleBuffer>)>,
    stretch_token: u64,
    stretch_tx: Sender<Stretched>,
    stretch_rx: Receiver<Stretched>,
    /// Render stretched copies inline, not on a worker (`DJEngine.deterministic`: a replay must
    /// be the same every time).
    pub deterministic: bool,
    /// The loops' master FX follow the decks' (`loopFxLink`).
    fx_link: bool,
    /// Master fires sent but maybe not yet seen by the audio thread (they count as running for
    /// the UI until it has applied the last of them), and the last value each master FX slider
    /// was set to by hand.
    master_queued_mask: Cell<u32>,
    master_sent: usize,
    master_queued_at: usize,
    master_sliders: BTreeMap<u8, i32>,
    /// What provenance says built it (`BuildInfo.current.build`: "dev" unbundled).
    pub build: String,
    /// The hub's per-clip extras and background work (transpose, file pads, one-shots).
    pub(crate) pads: super::loop_pads::Pads,
}

impl LoopRow {
    pub(crate) fn new(mirror: Arc<LoopMirror>) -> LoopRow {
        let (stretch_tx, stretch_rx) = channel();
        LoopRow {
            row: ClipRow::new(),
            voices: vec![LoopVoice::default(); LOOP_VOICES],
            takes: BTreeMap::new(),
            stinger_stops: BTreeMap::new(),
            volume: LOOP_VOLUME,
            gain: LOOP_CHANNEL_GAIN,
            mute: false,
            quantum: crate::deck::loops::DEFAULT_LOOP_QUANTUM,
            tap_take_bars: Some(crate::deck::loops::DEFAULT_TAP_TAKE_BARS),
            follow: LoopFollow::Louder,
            dominant_deck: RefCell::new(None),
            mirror,
            loop_fade: LoopFade::Declick,
            loop_key_lock: false,
            stretch_cache: BTreeMap::new(),
            stretch_token: 0,
            stretch_tx,
            stretch_rx,
            deterministic: false,
            fx_link: false,
            master_queued_mask: Cell::new(0),
            master_sent: 0,
            master_queued_at: 0,
            master_sliders: BTreeMap::new(),
            build: "dev".to_string(),
            pads: super::loop_pads::Pads::new(),
        }
    }

    /// The deck the loops follow under `Louder` as of the last update.
    pub fn dominant_deck(&self) -> Option<String> {
        self.dominant_deck.borrow().clone()
    }
}

/// How `start_loop` is asked (`startLoop(_:atOnce:at:fromTop:prefer:)`).
#[derive(Clone, Copy, Debug, Default)]
pub struct StartOpts<'a> {
    /// Start now, in phase, instead of waiting for the next launch boundary (a fresh take, which
    /// ends on a downbeat that has just gone by).
    pub at_once: bool,
    /// Start at that engine frame (a scene, whose clips all switch on one boundary).
    pub at: Option<i64>,
    /// From the clip's first beat, not in phase (a curated stinger's loop).
    pub from_top: bool,
    /// Line it up with that deck first.
    pub prefer: Option<&'a str>,
    /// Launch on a boundary of this many beats instead of the clip's own (a pad's `quant: beat`;
    /// beyond Swift).
    pub every: Option<i64>,
}

impl Engine {
    pub(crate) fn loop_send(&mut self, m: LoopMsg) -> bool {
        self.send_msg(Msg::Loop(m))
    }

    /// The frame the mixer has rendered to.
    pub fn loop_frames(&self) -> i64 {
        self.loops.mirror.rendered.load(Ordering::Relaxed)
    }

    /// Where `deck` is on its beat clock (`Deck.beatState`): None when it has no clock.
    pub fn beat_state(&self, deck: &str) -> Option<BeatState> {
        let d = self.deck(deck)?;
        if let Some(s) = &d.sample {
            let grid = s.grid?;
            let rate = d.mirror.turntable_rate();
            let bpm = grid.bpm
                * if rate.abs() > 1e-6 {
                    rate
                } else {
                    1.0 + d.nudge
                };
            let len = s.buffer.length() as f64;
            return Some(BeatState::of_sample(
                bpm,
                &grid,
                d.mirror.turntable_position(),
                len,
                self.sample_rate,
            ));
        }
        if !d.mirror.has_project.load(Ordering::Relaxed) {
            return None;
        }
        Some(BeatState::of_net(
            d.mirror.tempo(),
            d.mirror.tick(),
            d.mirror.tick_phase(),
        ))
    }

    fn deck_now(&self, deck: &str) -> i64 {
        self.deck(deck)
            .map_or(0, |d| d.mirror.rendered_frames.load(Ordering::Relaxed))
    }

    pub(crate) fn deck_playing(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.mirror.is_playing())
    }

    fn is_net(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.sample.is_none())
    }

    // MARK: LOOP record

    /// Is LOOP held (or its last bar still finishing) on `deck`?
    pub fn is_recording_loop(&self, deck: &str) -> bool {
        self.loops.takes.contains_key(deck)
    }

    /// The take on `deck`: bars so far and whether it was let go.
    pub fn loop_take_progress(&self, deck: &str) -> Option<(i64, bool)> {
        let t = self.loops.takes.get(deck)?;
        let now = t.released.unwrap_or_else(|| self.deck_now(deck));
        let bars = if t.end.is_some() {
            t.bars
        } else if t.tapped {
            t.fixed_bars.unwrap_or(1)
        } else {
            (((now - t.start) as f64 / t.bar_frames).ceil() as i64).max(1)
        };
        Some((bars, t.released.is_some()))
    }

    /// Keep each deck's view of its take current (`DeckView::loop_take`: released or not).
    pub(crate) fn sync_takes(&mut self) {
        for i in 0..self.decks.len() {
            let t = self
                .loops
                .takes
                .get(&self.decks[i].name)
                .map(|t| t.released.is_some());
            self.decks[i].loop_take = t;
        }
    }

    fn loop_tap_frames(&self) -> i64 {
        (0.25 * self.sample_rate).round() as i64
    }

    /// The audio a deck's sample take names by content: its loaded sample, unless it is the
    /// demo loop (`d.mode == .sample && !d.isDemo ? d.loaded : nil`).
    fn take_sample(&self, deck: &str) -> Option<Arc<SampleBuffer>> {
        let s = self.deck(deck)?.sample.as_ref()?;
        (!s.is_demo()).then(|| s.buffer.clone())
    }

    /// LOOP pressed (`on`) or let go on `deck` (`DJEngine.loopRecord`). Pressing does nothing
    /// on a deck that is not playing or has no beat clock, or while a take on it is still
    /// finishing. With `bars`, a release within a quarter second makes it a take of exactly
    /// that many bars. Returns whether the press started (or the release ended) a take.
    pub fn loop_record(&mut self, deck: &str, on: bool, bars: Option<i64>) -> bool {
        if self.deck(deck).is_none() {
            return false;
        }
        let now = self.deck_now(deck);
        if !on {
            let tap = self.loop_tap_frames();
            let Some(t) = self.loops.takes.get_mut(deck) else {
                return false;
            };
            if t.released.is_some() {
                return false;
            }
            t.released = Some(now);
            t.tapped = t.fixed_bars.is_some() && now - t.pressed <= tap;
            self.sync_takes();
            self.service_loops();
            return true;
        }
        if self.loops.takes.contains_key(deck) || !self.deck_playing(deck) {
            return false;
        }
        let Some(s) = self.beat_state(deck).filter(|s| s.bpm > 0.0) else {
            return false;
        };
        let bar_frames = 4.0 * 60.0 / s.bpm * self.sample_rate;
        let mut start = now - (s.bar_phase() * bar_frames).round() as i64;
        let mut start_beat = (s.bar() * 4) as f64;
        let d = self.deck(deck).unwrap();
        let mut cursor = d.mirror.capture.downbeats_written();
        if self.is_net(deck) {
            let mut c = cursor.saturating_sub(16);
            if let Some(&(f, b)) = d
                .mirror
                .capture
                .downbeats(&mut c)
                .iter()
                .rev()
                .find(|(f, _)| *f <= now)
            {
                start = f;
                start_beat = b as f64;
            }
            cursor = c;
        }
        let provenance = self.capture_provenance(deck, start_beat);
        let sample = self.take_sample(deck);
        self.loops.takes.insert(
            deck.to_string(),
            LoopTake {
                start,
                start_beat,
                pressed: now,
                fixed_bars: bars.map(|b| b.clamp(1, MAX_TAKE_BARS)),
                tapped: false,
                bar_frames,
                down_cursor: cursor,
                downs: Vec::new(),
                released: None,
                end: None,
                bars: 0,
                cursor: start,
                left: Vec::new(),
                right: Vec::new(),
                provenance,
                sample,
            },
        );
        self.sync_takes();
        true
    }

    /// Move every take on (copy what the decks captured, find the end of released ones, turn
    /// finished ones into clips) and keep the loops at their decks' tempo. Main thread; call it
    /// often (each frame).
    pub fn service_loops(&mut self) {
        let names: Vec<String> = self.loops.takes.keys().cloned().collect();
        for name in names {
            let Some(d) = self.deck(&name) else {
                self.loops.takes.remove(&name);
                continue;
            };
            let mirror = d.mirror.clone();
            let net = d.sample.is_none();
            let playing = mirror.is_playing();
            let t = self.loops.takes.get_mut(&name).unwrap();
            if t.end.is_none() && net {
                let start = t.start;
                let new = mirror.capture.downbeats(&mut t.down_cursor);
                t.downs.extend(
                    new.into_iter()
                        .filter(|(f, _)| *f > start)
                        .map(|(f, b)| (f, b as f64)),
                );
                let k = t.downs.len().saturating_sub(8);
                t.downs.drain(..k);
            }
            if t.end.is_none() {
                if let Some(r) = t.released {
                    find_end(t, net && playing, r);
                }
            }
            // a hold past the longest take ends on its own
            if t.end.is_none() && t.released.is_none() {
                let now = mirror.rendered_frames.load(Ordering::Relaxed);
                if (now - t.start) as f64 > MAX_TAKE_BARS as f64 * t.bar_frames {
                    t.released = Some(now);
                }
            }
            let to = t.end.unwrap_or(i64::MAX);
            t.cursor = mirror.capture.copy(t.cursor, to, &mut t.left, &mut t.right);
            if let Some(end) = t.end {
                if t.cursor >= end {
                    let t = self.loops.takes.remove(&name).unwrap();
                    self.finish_take(&name, t);
                }
            }
        }
        self.sync_takes();
        // captures from the input strips (beyond Swift: `input_capture`)
        self.service_input_takes();
        // and from the buses, V and M (`loop_source`)
        self.service_bus_takes();
        self.service_voices();
    }

    fn finish_take(&mut self, deck: &str, t: LoopTake) {
        let Some(end) = t.end else { return };
        let n = (end - t.start) as usize;
        if n == 0 || t.left.len() < n || t.right.len() < n {
            return;
        }
        let (mut l, mut r) = (t.left, t.right);
        l.truncate(n);
        r.truncate(n);
        self.add_take(deck, t.bars, t.start_beat, l, r, t.provenance, t.sample);
    }

    /// A finished take's audio onto the row, looping under the mixer at once, in phase (it
    /// ends on a downbeat that has just gone by). Returns its id (`addTake`).
    #[allow(clippy::too_many_arguments)]
    fn add_take(
        &mut self,
        deck: &str,
        bars: i64,
        start_beat: f64,
        left: Vec<f32>,
        right: Vec<f32>,
        provenance: JsonObject,
        sample: Option<Arc<SampleBuffer>>,
    ) -> String {
        let buffer = SampleBuffer::new(left, Some(right), self.sample_rate);
        let id = buffer.content_id();
        let bpm = bars as f64 * 4.0 * 60.0 * self.sample_rate / buffer.length() as f64;
        self.loops.row.loop_count += 1;
        let waveform = WaveformOverview::new(&buffer, crate::deck::clips::WAVEFORM_COLUMNS);
        let provenance = crate::deck::provenance::finish(
            provenance,
            bars,
            bpm,
            sample.as_ref().map(|b| b.content_id()).as_deref(),
        );
        let clip = LoopClip::from_bars(
            &id,
            &format!("{deck} loop {}", self.loops.row.loop_count),
            deck,
            bars,
            bpm,
            start_beat,
            Arc::new(buffer),
            waveform,
            Some(provenance),
        );
        self.loops
            .row
            .sample_library
            .insert(id.clone(), clip.buffer.clone());
        if self.clip(&id).is_none() {
            self.loops.row.clips.push(clip);
        }
        self.start_loop(&id, true);
        id
    }

    /// Take the last `bars` whole bars `deck` has already played, ending on the latest
    /// downbeat (`grabLastBars`). None on a deck that is not playing or has no beat clock, or
    /// when the capture does not reach back that far. The provenance says `"grabbed": true`.
    pub fn grab_last_bars(&mut self, deck: &str, bars: i64) -> Option<String> {
        if bars <= 0 || !self.deck_playing(deck) {
            return None;
        }
        let s = self.beat_state(deck).filter(|s| s.bpm > 0.0)?;
        let d = self.deck(deck)?;
        let (oldest, captured) = d.mirror.capture.span()?;
        let now = self.deck_now(deck).min(captured);
        let bar_frames = 4.0 * 60.0 / s.bpm * self.sample_rate;
        let mut end = self.deck_now(deck) - (s.bar_phase() * bar_frames).round() as i64;
        let mut end_beat = (s.bar() * 4) as f64;
        let mut start = end - (bars as f64 * bar_frames).round() as i64;
        if d.sample.is_none() {
            let mut c = 0;
            let downs: Vec<(i64, i64)> = d
                .mirror
                .capture
                .downbeats(&mut c)
                .into_iter()
                .filter(|(f, _)| *f <= now)
                .collect();
            let &(last_f, last_b) = downs.last()?;
            end = last_f;
            end_beat = last_b as f64;
            let first = end_beat - (4 * bars) as f64;
            start = downs
                .iter()
                .rev()
                .find(|(f, b)| (*b as f64 - first).abs() < 0.5 && *f < last_f)
                .map_or(end - (bars as f64 * bar_frames).round() as i64, |d| d.0);
        }
        if start < oldest || start >= end || end > captured {
            return None;
        }
        let (mut l, mut r) = (Vec::new(), Vec::new());
        if d.mirror.capture.copy(start, end, &mut l, &mut r) != end || l.len() as i64 != end - start
        {
            return None;
        }
        let start_beat = end_beat - (4 * bars) as f64;
        let mut provenance = self.capture_provenance(deck, start_beat);
        provenance.set("grabbed", crate::json::JsonValue::Bool(true));
        let sample = self.take_sample(deck);
        Some(self.add_take(deck, bars, start_beat, l, r, provenance, sample))
    }

    // MARK: the row

    /// A clip on the row (`clip(_:)`; its fallback to a curated stinger's stock loop,
    /// `stingerClips`, is Apple Loops: n/a, see [`Engine::fire_stinger_loop`]).
    pub fn clip(&self, id: &str) -> Option<&LoopClip> {
        self.loops.row.clip(id)
    }

    /// The bank voice clip `id` holds (tests, measurement).
    pub fn voice_index(&self, id: &str) -> Option<usize> {
        self.voice_of(id)
    }

    pub(crate) fn voice_of(&self, id: &str) -> Option<usize> {
        self.loops
            .voices
            .iter()
            .position(|v| v.clip.as_deref() == Some(id))
    }

    pub fn loop_state(&self, id: &str) -> LoopState {
        let Some(v) = self.voice_of(id) else {
            return LoopState::Off;
        };
        let on = self.loops.mirror.is_active(v);
        match (self.loops.voices[v].stop_frame.is_some(), on) {
            (true, true) => LoopState::Stopping,
            (true, false) => LoopState::Off,
            (false, true) => LoopState::On,
            (false, false) => LoopState::Starting,
        }
    }

    /// The clips looping (or about to) under the mixer, in voice order (`loopingClips`).
    pub fn looping_clips(&self) -> Vec<String> {
        self.loops
            .voices
            .iter()
            .filter(|v| v.stop_frame.is_none())
            .filter_map(|v| v.clip.clone())
            .collect()
    }

    /// The frame clip `id`'s voice was told to stop at (it goes out then, over its fade).
    pub fn loop_stop_frame(&self, id: &str) -> Option<i64> {
        self.loops.voices[self.voice_of(id)?].stop_frame
    }

    /// The tempo clip `id`'s voice is to play at: its reference deck's, else the one it started
    /// at (`LoopVoice.targetBPM`).
    pub fn loop_target_bpm(&self, id: &str) -> Option<f64> {
        Some(self.loops.voices[self.voice_of(id)?].target_bpm)
    }

    /// The deck clip `id`'s voice follows the tempo of now (`LoopVoice.ref`).
    pub fn loop_reference(&self, id: &str) -> Option<String> {
        self.loops.voices[self.voice_of(id)?].reference.clone()
    }

    /// The tempo of the clock loops line up with now (`loopClock(for: nil).bpm`): a playing
    /// deck's, else a loop already going, else None.
    pub fn loop_clock_bpm(&self) -> Option<f64> {
        self.loop_clock(None, None).bpm
    }

    /// How far through its loop clip `id` is (0...1), while it sounds.
    pub fn loop_playhead(&self, id: &str) -> Option<f64> {
        let v = self.voice_of(id)?;
        if !self.loops.mirror.is_active(v) {
            return None;
        }
        let (p, len) = self.loops.mirror.playhead(v);
        (len > 0).then(|| p.rem_euclid(len as f64) / len as f64)
    }

    /// How many times clip `id` has gone round since its voice started, as of the last block
    /// (unwrapped: 2.25 is a quarter into the third pass), while it sounds. Loop routing
    /// follows it (`crate::deck::loop_route::Follow`).
    pub fn loop_position(&self, id: &str) -> Option<f64> {
        let v = self.voice_of(id)?;
        if !self.loops.mirror.is_active(v) {
            return None;
        }
        let (p, len) = self.loops.mirror.playhead(v);
        (len > 0).then(|| p / len as f64)
    }

    /// Seconds until clip `id`'s voice stops, when it has been told to (None: it isn't
    /// stopping).
    pub fn loop_stops_in(&self, id: &str) -> Option<f64> {
        let at = self.loop_stop_frame(id)?;
        let now = self.loops.mirror.rendered.load(Ordering::Relaxed);
        Some((at - now).max(0) as f64 / self.sample_rate)
    }

    /// Play only (beyond Swift; the socket's `loops play`): start clip `id` on the next launch
    /// boundary, or keep it in when it is on its way out. A clip already in, or about to be,
    /// is left alone. True if it was told to start.
    pub fn loop_play(&mut self, id: &str) -> bool {
        if !matches!(self.loop_state(id), LoopState::Off | LoopState::Stopping) || self.clip(id).is_none() {
            return false;
        }
        self.start_loop(id, false);
        true
    }

    /// Stop only (beyond Swift; the socket's `loops stop`): stop clip `id` on the next launch
    /// boundary. A clip that is off stays off, and one already stopping keeps the stop it has
    /// (a second stop does not push it to a later boundary). True if it was told to stop.
    pub fn loop_stop(&mut self, id: &str) -> bool {
        if !matches!(self.loop_state(id), LoopState::On | LoopState::Starting) {
            return false;
        }
        self.stop_loop(id, false, None);
        true
    }

    /// Start or stop clip `id` looping under the mixer, on the next launch boundary (a bar
    /// unless `loop_quantum` says otherwise). Starting a clip that is on its way out keeps it in.
    pub fn set_looping(&mut self, id: &str, on: bool) {
        if on {
            self.start_loop(id, false)
        } else {
            self.stop_loop(id, false, None)
        }
    }

    /// Every loop out, on the next launch boundary.
    pub fn stop_all_loops(&mut self) {
        for id in self.looping_clips() {
            self.stop_loop(&id, false, None);
        }
        self.note_live(
            "loops:stop".into(),
            "stop all loops".into(),
            false,
            "loops",
            super::vocoder::live_params([("action", "stop_all".into())]),
        );
    }

    // MARK: the channel strip

    /// The loop channel's fader (`loopChannel.volume`).
    pub fn set_loop_volume(&mut self, v: f64) {
        self.loops.volume = v.clamp(0.0, 1.0);
        self.loop_send(LoopMsg::Volume(self.loops.volume));
    }

    /// The loop channel's trim knob, 0...1 (`loopChannel.gain`): trim `2^((gain - 0.5) * 2)`.
    pub fn set_loop_gain(&mut self, knob: f64) {
        self.loops.gain = knob.clamp(0.0, 1.0);
        self.loop_send(LoopMsg::Trim(trim_for_gain(self.loops.gain)));
    }

    /// The loop channel's mute (`loopChannel.mute`).
    pub fn set_loop_mute(&mut self, on: bool) {
        self.loops.mute = on;
        self.loop_send(LoopMsg::Mute(on));
    }

    /// A knob of the channel's strip (`loopChannel.filter`, `.eqLow`, `.eqMid`, `.eqHigh`): kept
    /// as it was set (`currentParams` and a session name it so) and heard through the channel's EQ
    /// and DJ filter, in front of its fader. At rest (0.5) the strip is bit-transparent.
    pub fn set_loop_strip(&mut self, knob: StripKnob, v: f64) {
        let s = &mut self.loop_channel.strip;
        match knob {
            StripKnob::Filter => s.filter = v,
            StripKnob::EqLow => s.eq_low = v,
            StripKnob::EqMid => s.eq_mid = v,
            StripKnob::EqHigh => s.eq_high = v,
        }
        self.loop_send(LoopMsg::Strip(knob, v));
    }

    /// A knob of the channel's reverb (`loopChannel.reverb.wet / size / tone`): kept as it was
    /// set (`currentParams` and a session name it so) and heard after the channel's EQ, in front of
    /// its fader. Wet below 0.005 leaves the channel as it is.
    pub fn set_loop_reverb(&mut self, knob: ReverbKnob, v: f64) {
        // kept as it was set (Swift keeps the raw value); the unit clamps its own
        let c = &mut self.loop_channel;
        match knob {
            ReverbKnob::Wet => c.reverb_wet = v,
            ReverbKnob::Size => c.reverb_size = v,
            ReverbKnob::Tone => c.reverb_tone = v,
        }
        self.loop_send(LoopMsg::Reverb(knob, v));
    }

    /// The channel's level in the master as the strip sets it: trim x fader, 0 when muted (no
    /// crossfader on L).
    pub fn loop_channel_level(&self) -> f64 {
        channel_level(
            self.loops.mute,
            trim_for_gain(self.loops.gain),
            self.loops.volume,
            1.0,
        )
    }

    // MARK: the clock

    /// Decks as `loopClock` sees them.
    fn clock_decks(&self) -> Vec<ClockDeck> {
        self.decks
            .iter()
            .map(|d| ClockDeck {
                name: d.name.clone(),
                playing: d.mirror.is_playing(),
                clip_id: d.clip_id.clone(),
                beat_state: self
                    .beat_state(&d.name)
                    .filter(|s| s.bpm > 0.0)
                    .map(|s| (s.bpm, s.beat)),
                rendered_frames: d.mirror.rendered_frames.load(Ordering::Relaxed),
                anchor: newest_anchor(d),
            })
            .collect()
    }

    /// What a loop lines up with (`loopClock(for:prefer:)`): the deck it was taken from if that
    /// plays, else another playing deck, else a loop already going under the mixer, else
    /// nothing. `prefer`: that deck first; following the louder deck, that one first.
    pub fn loop_clock(&self, clip: Option<&str>, prefer: Option<&str>) -> LoopClock {
        let c = clip.and_then(|id| self.clip(id));
        let first: Option<String> = match prefer {
            Some(p) => Some(p.to_string()),
            None if self.follow_for(clip) == LoopFollow::Louder => self.update_dominant_deck(),
            None => c.map(|c| c.source.clone()),
        };
        let voices: Vec<ClockVoice> = self
            .loops
            .voices
            .iter()
            .enumerate()
            .map(|(v, voice)| {
                let (position, length) = self.loops.mirror.playhead(v);
                let other = voice.clip.as_deref().and_then(|id| self.clip(id));
                ClockVoice {
                    active: self.loops.mirror.is_active(v),
                    stopping: voice.stop_frame.is_some(),
                    clip_id: voice.clip.clone(),
                    clip_beats: other.map(|c| c.beats),
                    clip_length: other.map_or(0, |c| c.buffer.length()),
                    length: length as f64,
                    position,
                    target_bpm: voice.target_bpm,
                }
            })
            .collect();
        select_clock(
            clip,
            c.map(|c| c.source.as_str()),
            first.as_deref(),
            &self.clock_decks(),
            &voices,
            self.loop_frames(),
            self.sample_rate,
        )
    }

    // MARK: launch

    /// Start clip `id` looping: `at_once`, now in phase; else on the next launch boundary.
    pub fn start_loop(&mut self, id: &str, at_once: bool) {
        self.start_loop_with(
            id,
            StartOpts {
                at_once,
                ..StartOpts::default()
            },
        )
    }

    /// `startLoop(_:atOnce:at:fromTop:prefer:)`. A clip whose voice is on its way out keeps
    /// in; a free voice takes the clip, loaded and seeked to the phase the clock gives it, and
    /// comes in at the frame (`at` for a scene's clips, on the row's boundary and not the
    /// clip's own; the clip's own boundary and fade otherwise).
    pub fn start_loop_with(&mut self, id: &str, o: StartOpts<'_>) {
        let Some(clip) = self.clip(id) else { return };
        let (beats, source, start_beat, clip_bpm, fx, own) = (
            clip.beats as f64,
            clip.source.clone(),
            clip.start_beat,
            clip.bpm,
            clip.fx.clone(),
            clip.buffer.clone(),
        );
        if let Some(v) = self.voice_of(id) {
            if self.loops.voices[v].stop_frame.is_none() {
                return; // already in
            }
            self.loops.voices[v].stop_frame = None;
            self.loops.voices[v].boundary = None;
            // cancels the stop (or brings a fade-out back up)
            let fade = self.loop_fade_frames(Some(id), self.loops.voices[v].target_bpm, false);
            self.loop_send(LoopMsg::Start { v, frame: 0, fade });
            return;
        }
        let Some(v) = self.loops.voices.iter().position(|v| v.clip.is_none()) else {
            return;
        };
        let clock = self.loop_clock(Some(id), o.prefer);
        let earliest = self.loop_frames() + LOOP_LEAD;
        // its own boundary, but a scene's clips all switch on the row's
        let every = if let Some(e) = o.every.filter(|e| *e > 0) {
            e
        } else if o.at.is_none() {
            self.quantum_for(Some(id))
        } else {
            self.loops.quantum
        };
        // the hub's extras: a one-shot, a gain, a transposition, a capture snapped to the clock
        let x = self.loops.pads.extra(id);
        let at = o.at.map(|f| f.max(earliest)).unwrap_or(if o.at_once {
            earliest
        } else {
            clock.next_boundary(earliest, every)
        });
        let offset = if x.locked || clock.ref_deck.as_deref() == Some(source.as_str()) {
            start_beat
        } else {
            0.0
        };
        let mut b = (clock.beat(at) - offset) % beats;
        if b < 0.0 {
            b += beats;
        }
        if o.from_top || x.once {
            b = 0.0;
        }
        // a one-shot keeps its own speed; a loop follows the clock
        let target = if x.once {
            clip_bpm
        } else {
            clock.bpm.unwrap_or(clip_bpm)
        };
        // key-locked at another tempo: a stretched copy made before, if there is one for it; a
        // transposed clip plays its pitch-shifted copy
        let (buffer, buffer_bpm) = match self.loops.pads.shifted_copy(id, x.shift) {
            Some(b) => (b, clip_bpm),
            None => self.loop_audio(id, &own, clip_bpm, target),
        };
        let ratio = target / buffer_bpm;
        let tape = crate::stretch::ratio(x.tape);
        let len = buffer.length() as f64;
        let params = LoopFxParams::new(&fx, self.sample_rate);
        let mut cmds = vec![TurntableCommand::Load(buffer)];
        cmds.push(if x.once {
            TurntableCommand::LoopOff
        } else {
            TurntableCommand::Loop {
                start: 0.0,
                end: len,
            }
        });
        cmds.push(TurntableCommand::Pitch(ratio * tape - 1.0));
        cmds.push(TurntableCommand::Seek(b * len / beats));
        cmds.push(TurntableCommand::Start);
        for c in cmds {
            self.loop_send(LoopMsg::Voice(v, c));
        }
        let level = (self.audible_clip_gain(id) as f32) * x.gain;
        self.loop_send(LoopMsg::Gain(v, level));
        self.loop_send(LoopMsg::Fx(v, params));
        let fade = self.loop_fade_frames(Some(id), target, o.at_once);
        let stop_frame = if x.once {
            // out again when it has played through
            let end = at + (len / (ratio * tape)).ceil() as i64;
            self.loop_send(LoopMsg::StartOnce {
                v,
                frame: at,
                fade,
                end,
            });
            Some(end)
        } else {
            self.loop_send(LoopMsg::Start { v, frame: at, fade });
            // the master phase a capture was snapped to only counts for its first start
            self.loops.pads.unlock(id);
            None
        };
        self.loops.voices[v] = LoopVoice {
            clip: Some(id.to_string()),
            reference: if x.once { None } else { clock.ref_deck.clone() },
            ratio,
            buffer_bpm,
            target_bpm: target,
            stop_frame,
            start_frame: Some(at),
            scheduled_at: Some(self.loop_frames()),
            gain_until: None,
            buffer_length: len,
            boundary: if o.at_once || x.once {
                None
            } else {
                clock.waiting_boundary(every)
            },
            stretch_want: None,
            stretching: None,
        };
    }

    /// `stopLoop(_:now:at:)`: `now`, at once on a short fade; else on the next launch boundary
    /// of the clip's own (`at`: that engine frame, a scene launch, on the row's).
    pub fn stop_loop(&mut self, id: &str, now: bool, at: Option<i64>) {
        let Some(v) = self.voice_of(id) else { return };
        if !self.loops.mirror.is_active(v) && self.loops.voices[v].stop_frame.is_none() {
            // not in yet: never let it start
            self.loop_send(LoopMsg::Stop {
                v,
                frame: 0,
                fade: 0,
            });
            self.loops.voices[v].stop_frame = Some(0);
            return;
        }
        let earliest = self.loop_frames() + if now { 0 } else { LOOP_LEAD };
        let clock = self.loop_clock(Some(id), None);
        let every = if at.is_none() {
            self.quantum_for(Some(id))
        } else {
            self.loops.quantum
        };
        let frame = if now {
            earliest
        } else {
            at.map(|f| f.max(earliest))
                .unwrap_or_else(|| clock.next_boundary(earliest, every))
        };
        let fade = self.loop_fade_frames(Some(id), self.loops.voices[v].target_bpm, now);
        self.loop_send(LoopMsg::Stop { v, frame, fade });
        let scheduled = self.loop_frames();
        let voice = &mut self.loops.voices[v];
        voice.stop_frame = Some(frame);
        voice.scheduled_at = Some(scheduled);
        voice.boundary = if now {
            None
        } else {
            clock.waiting_boundary(every)
        };
    }

    // MARK: tempo follow

    /// The deck the loops follow under `Louder` (`updateDominantDeck`): the louder playing deck
    /// with a tempo. It only changes hands when another deck is clearly louder (3 dB: a
    /// crossfade does not flap it back and forth across the middle), or when it stops or goes
    /// silent.
    pub fn update_dominant_deck(&self) -> Option<String> {
        let mut names: Vec<&MainDeck> = self.decks.iter().collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        let candidates: Vec<DominantCandidate> = names
            .into_iter()
            .map(|d| DominantCandidate {
                name: d.name.clone(),
                playing_with_tempo: d.mirror.is_playing() && self.beat_state(&d.name).is_some(),
                level: channel_level(
                    d.mute,
                    trim_for_gain(d.gain),
                    d.volume,
                    crossfade_gain(&d.name, self.crossfader),
                ),
            })
            .collect();
        let current = self.loops.dominant_deck.borrow().clone();
        let next = update_dominant_deck(&candidates, current.as_deref());
        *self.loops.dominant_deck.borrow_mut() = next.clone();
        next
    }

    /// Free voices whose loop is out; keep each loop at its reference deck's tempo, a waiting
    /// start or stop on its deck's boundary, and key-locked ones at their pitch
    /// (`serviceLoops`).
    pub(crate) fn service_voices(&mut self) {
        self.service_pads();
        let now = self.loop_frames();
        self.take_stretched();
        // once per pass, whatever each loop follows: per voice it could hand over mid-pass
        let louder = if self.any_loop_follows_louder() {
            self.update_dominant_deck()
        } else {
            None
        };
        self.service_stinger_stops();
        for v in 0..LOOP_VOICES {
            let Some(id) = self.loops.voices[v].clip.clone() else {
                continue;
            };
            let active = self.loops.mirror.is_active(v);
            if let Some(stop) = self.loops.voices[v].stop_frame {
                if now >= stop && !active {
                    self.loops.voices[v] = LoopVoice::default();
                } else {
                    self.retime_waiting(v, now);
                }
                continue;
            }
            self.retime_waiting(v, now);
            // moved back from a deck: the deck hands over when the loop comes in, on the downbeat
            if active {
                let holding: Vec<String> = self
                    .decks
                    .iter()
                    .filter(|d| d.clip_id.as_deref() == Some(id.as_str()) && d.mirror.is_playing())
                    .map(|d| d.name.clone())
                    .collect();
                for d in holding {
                    self.pause(&d);
                }
            }
            let Some(clip) = self.clip(&id) else { continue };
            let source = clip.source.clone();
            let x = self.loops.pads.extra(&id);
            if x.once {
                continue; // a one-shot plays at its own speed and goes out on its own
            }
            // following the louder deck, a playing loop takes its tempo from whichever that is
            // now; following its own deck, back to that one when it plays (each clip by its
            // own follow)
            let follows = self.follow_for(Some(&id));
            let clip_of = |d: &str| self.deck(d).and_then(|d| d.clip_id.clone());
            if follows == LoopFollow::Louder
                && active
                && louder
                    .as_deref()
                    .is_some_and(|l| clip_of(l).as_deref() != Some(id.as_str()))
            {
                self.loops.voices[v].reference = louder.clone();
            } else if follows == LoopFollow::Source
                && active
                && self.deck_playing(&source)
                && clip_of(&source).as_deref() != Some(id.as_str())
            {
                self.loops.voices[v].reference = Some(source);
            }
            if let Some(r) = self.loops.voices[v].reference.clone() {
                if self.deck_playing(&r) {
                    if let Some(bpm) = self.deck(&r).and_then(DeckView::effective_bpm) {
                        self.loops.voices[v].target_bpm = bpm;
                    }
                }
            }
            // a transposed clip plays its own pitch-shifted copy, not a key-locked stretch
            if x.shift == 0 && x.tape == 0 {
                self.service_key_lock(v, now);
            }
            let ratio = self.loops.voices[v].target_bpm / self.loops.voices[v].buffer_bpm;
            if (ratio - self.loops.voices[v].ratio).abs() > 1e-6 {
                self.loops.voices[v].ratio = ratio;
                let tape = crate::stretch::ratio(x.tape);
                self.loop_send(LoopMsg::Voice(
                    v,
                    TurntableCommand::Pitch(ratio * tape - 1.0),
                ));
            }
        }
    }

    /// A start or stop still waiting for its boundary follows the reference deck to it
    /// (`retimeWaiting`): once the deck's tempo is not the one the frame was worked out at,
    /// each pass moves the frame to the deck's launch line nearest it, and a waiting start is
    /// seeked to the clip's phase there. The decision is `crate::deck::clock::retime_waiting`.
    fn retime_waiting(&mut self, v: usize, now: i64) {
        let voice = &self.loops.voices[v];
        if voice.boundary.is_none() {
            return;
        }
        let Some(id) = voice.clip.clone() else { return };
        let rv = RetimeVoice {
            boundary: voice.boundary.clone(),
            stop_frame: voice.stop_frame,
            start_frame: voice.start_frame,
            buffer_length: voice.buffer_length,
            active: self.loops.mirror.is_active(v),
        };
        let target = voice.target_bpm;
        let Some(clip) = self.clip(&id) else { return };
        let clock = self.loop_clock(Some(&id), None);
        let Some(r) = retime_waiting(&rv, clip, &clock, now) else {
            return;
        };
        if let Some(b) = self.loops.voices[v].boundary.as_mut() {
            b.follow = r.follow;
        }
        let fade = self.loop_fade_frames(Some(&id), target, false);
        match r.moved {
            Some(RetimeMove::Stop { at }) => {
                self.loop_send(LoopMsg::Stop { v, frame: at, fade });
                self.loops.voices[v].stop_frame = Some(at);
            }
            Some(RetimeMove::Start { at, seek }) => {
                self.loop_send(LoopMsg::Voice(v, TurntableCommand::Seek(seek)));
                self.loop_send(LoopMsg::Start { v, frame: at, fade });
                self.loops.voices[v].start_frame = Some(at);
            }
            None => {}
        }
    }
}

/// A released take's end (`findEnd`).
fn find_end(t: &mut LoopTake, net_playing: bool, r: i64) {
    if t.tapped {
        if let Some(n) = t.fixed_bars {
            if net_playing {
                let target = t.start_beat + (4 * n) as f64;
                if let Some(&(f, _)) = t.downs.iter().find(|(_, b)| *b > target - 0.5) {
                    t.end = Some(f);
                    t.bars = n;
                }
                return; // that downbeat has not been played yet
            }
            t.bars = n;
            t.end = Some(t.start + (n as f64 * t.bar_frames).round() as i64);
            return;
        }
    }
    let tolerance = t.bar_frames / 16.0;
    if net_playing {
        if let Some(&(f, b)) = t.downs.iter().rev().find(|(f, _)| *f <= r) {
            if (r - f) as f64 <= tolerance {
                t.end = Some(f);
                t.bars = (((b - t.start_beat) / 4.0).round() as i64).max(1);
                return;
            }
        }
        if let Some(&(f, b)) = t.downs.iter().find(|(f, _)| *f > r) {
            t.end = Some(f);
            t.bars = (((b - t.start_beat) / 4.0).round() as i64).max(1);
        }
        return; // the next downbeat has not been played yet
    }
    // a sample deck, or a net deck that stopped: whole bars at the pressed tempo
    let elapsed = (r - t.start) as f64;
    t.bars = (((elapsed - tolerance) / t.bar_frames).ceil() as i64).max(1);
    t.end = Some(t.start + (t.bars as f64 * t.bar_frames).round() as i64);
}

// MARK: how the loops sound (LoopSound.swift)

impl Engine {
    // mute and solo

    pub fn is_clip_muted(&self, id: &str) -> bool {
        self.loops.row.settings.is_muted(id)
    }

    pub fn is_clip_soloed(&self, id: &str) -> bool {
        self.loops.row.settings.is_soloed(id)
    }

    /// Mute (`on`) a clip under the mixer: it plays on, silent, and comes back in phase.
    pub fn set_clip_mute(&mut self, id: &str, on: bool) {
        self.loops.row.settings.set_mute(id, on);
        self.push_clip_levels();
    }

    /// Solo a clip: while any clip is soloed, only soloed ones are heard under the mixer.
    pub fn set_clip_solo(&mut self, id: &str, on: bool) {
        self.loops.row.settings.set_solo(id, on);
        self.push_clip_levels();
    }

    /// A clip's level under the mixer, 0...1 (1 by default) (`clipGain`).
    pub fn clip_gain(&self, id: &str) -> f64 {
        self.loops.row.clip_gain(id)
    }

    /// Set a clip's level. `at`: change it at that frame instead of at once (a scene launch, on
    /// its boundary) (`setClipGain`).
    pub fn set_clip_gain(&mut self, id: &str, g: f64, at: Option<i64>) {
        let before = self.clip_gain(id);
        self.loops.row.set_clip_gain(id, g);
        let Some(v) = self.voice_of(id) else { return };
        let level = self.audible_clip_gain(id) as f32;
        if let Some(at) = at {
            // remembered, so a mute or solo before then keeps the change on its frame
            let was = self.loops.voices[v].gain_until.map_or(before, |w| w.0);
            self.loops.voices[v].gain_until = Some((was, at));
            self.loop_send(LoopMsg::GainAt(v, level, at));
        } else {
            self.loops.voices[v].gain_until = None;
            self.loop_send(LoopMsg::Gain(v, level));
        }
    }

    /// What a clip's voice plays at: its level, or 0 when muted or soloed out.
    pub fn audible_clip_gain(&self, id: &str) -> f64 {
        self.audible(id, self.clip_gain(id))
    }

    /// Level `gain` for clip `id` as its voice plays it: 0 when muted or soloed out.
    fn audible(&self, id: &str, gain: f64) -> f64 {
        self.loops.row.settings.audible(id, gain)
    }

    /// Every voice's level, again (a mute or solo changes others' too). The bank ramps to it. A
    /// voice whose level is waiting for a scene's boundary (`gain_until`) keeps waiting: the
    /// level it has until then, and the scene's from then, both with the new mute and solo (a
    /// plain level change on its own would cancel the wait).
    pub(crate) fn push_clip_levels(&mut self) {
        let now = self.loop_frames();
        for v in 0..LOOP_VOICES {
            let Some(id) = self.loops.voices[v].clip.clone() else {
                continue;
            };
            match self.loops.voices[v].gain_until {
                Some((gain, frame)) if frame > now => {
                    let before = self.audible(&id, gain) as f32;
                    let after = self.audible_clip_gain(&id) as f32;
                    self.loop_send(LoopMsg::Gain(v, before));
                    self.loop_send(LoopMsg::GainAt(v, after, frame));
                }
                _ => {
                    self.loops.voices[v].gain_until = None;
                    let level = self.audible_clip_gain(&id) as f32;
                    self.loop_send(LoopMsg::Gain(v, level));
                }
            }
        }
    }

    // fades

    /// How loops come in and go out from now on (clips with a fade of their own keep it).
    pub fn set_loop_fade(&mut self, fade: LoopFade) {
        self.loops.loop_fade = fade;
    }

    /// The row's fade.
    pub fn loop_fade(&self) -> LoopFade {
        self.loops.loop_fade
    }

    /// Clip `id`'s own fade, if it has one (None: it follows the row's).
    pub fn clip_fade(&self, id: &str) -> Option<LoopFade> {
        self.loops.row.settings.fade(id)
    }

    /// The fade clip `id` comes in and goes out with: its own, else the row's.
    pub fn loop_fade_for(&self, id: Option<&str>) -> LoopFade {
        self.loops.row.settings.fade_for(self.loops.loop_fade, id)
    }

    /// Give clip `id` a fade of its own; None: it follows the row's again. A start or stop
    /// already waiting keeps the fade it was given.
    pub fn set_clip_fade(&mut self, id: &str, fade: Option<LoopFade>) {
        self.loops.row.settings.set_fade(id, fade);
    }

    /// The fade in frames for clip `id` (its own fade, else the row's) playing at `bpm`;
    /// `short`: a start or stop that is not on a downbeat (a fresh take, a clip moved to a
    /// deck), which only de-clicks (`loopFadeFrames(for:bpm:short:)`).
    pub fn loop_fade_frames(&self, id: Option<&str>, bpm: f64, short: bool) -> i64 {
        loop_sound::loop_fade_frames(self.loop_fade_for(id), bpm, short, self.sample_rate)
    }

    // key lock

    /// Does clip `id` keep its pitch at another tempo? Its own setting, else the row's.
    pub fn is_key_locked(&self, id: &str) -> bool {
        self.loops
            .row
            .settings
            .is_key_locked(id, self.loops.loop_key_lock)
    }

    pub fn set_clip_key_lock(&mut self, id: &str, on: bool) {
        self.loops.row.settings.set_key_lock(id, on);
        self.service_voices(); // Swift's `serviceLoops()`: the voices, not the takes
    }

    /// The row's key lock, for clips not set one way or the other themselves.
    pub fn set_loop_key_lock(&mut self, on: bool) {
        self.loops.loop_key_lock = on;
        self.service_voices();
    }

    pub fn loop_key_lock(&self) -> bool {
        self.loops.loop_key_lock
    }

    /// Is clip `id` playing a time-stretched copy under the mixer (key lock at work)?
    pub fn is_clip_stretched(&self, id: &str) -> bool {
        match (self.voice_of(id), self.clip(id)) {
            (Some(v), Some(c)) => self.loops.voices[v].buffer_bpm != c.bpm,
            _ => false,
        }
    }

    /// Is a stretched copy of clip `id` being rendered?
    pub fn is_clip_stretching(&self, id: &str) -> bool {
        self.loops
            .voices
            .iter()
            .any(|v| v.clip.as_deref() == Some(id) && v.stretching.is_some())
    }

    /// The audio a clip starts with at `bpm`: a stretched copy already made for about that
    /// tempo when it is key-locked, else its own. With the tempo that audio plays at
    /// (`loopAudio(_:at:)`).
    fn loop_audio(
        &self,
        id: &str,
        own: &Arc<SampleBuffer>,
        clip_bpm: f64,
        bpm: f64,
    ) -> (Arc<SampleBuffer>, f64) {
        let cached = self.loops.stretch_cache.get(id);
        if loop_sound::use_cached_copy(self.is_key_locked(id), clip_bpm, bpm, cached.map(|c| c.0)) {
            if let Some((b, buf)) = cached {
                return (buf.clone(), *b);
            }
        }
        (own.clone(), clip_bpm)
    }

    /// Key lock for voice `v` (from `service_voices`, its target tempo up to date): swap back to
    /// the clip's own audio when it is not wanted, else to a stretched copy at the target tempo
    /// (a cached one, or one rendered once the tempo has held for `STRETCH_DEBOUNCE`)
    /// (`serviceKeyLock`).
    fn service_key_lock(&mut self, v: usize, now: i64) {
        let Some(id) = self.loops.voices[v].clip.clone() else {
            return;
        };
        let Some(clip) = self.clip(&id) else { return };
        let voice = &self.loops.voices[v];
        let state = KeyLockState {
            locked: self.is_key_locked(&id),
            clip_bpm: clip.bpm,
            target_bpm: voice.target_bpm,
            buffer_bpm: voice.buffer_bpm,
            stretching: voice.stretching.is_some(),
            want: voice.stretch_want,
            cached_bpm: self.loops.stretch_cache.get(&id).map(|c| c.0),
            now,
            sample_rate: self.sample_rate,
        };
        let (own, clip_bpm, target) = (clip.buffer.clone(), clip.bpm, voice.target_bpm);
        match loop_sound::key_lock_step(&state) {
            KeyLockStep::Own { swap_back } => {
                self.loops.voices[v].stretch_want = None;
                if swap_back {
                    self.swap_voice(v, own, clip_bpm);
                }
            }
            KeyLockStep::Settled => self.loops.voices[v].stretch_want = None,
            KeyLockStep::Wait | KeyLockStep::Hold => {}
            KeyLockStep::UseCached => {
                if let Some((bpm, buf)) = self.loops.stretch_cache.get(&id).cloned() {
                    self.swap_voice(v, buf, bpm);
                }
            }
            KeyLockStep::Render => self.render_stretch(v, &id, target),
            KeyLockStep::Want => self.loops.voices[v].stretch_want = Some((target, now)),
        }
    }

    /// Render clip's audio stretched to `bpm`, off the main thread (inline when
    /// `deterministic`, as a replay must be), then swap it into voice `v` if that voice still
    /// wants it (`renderStretch`).
    fn render_stretch(&mut self, v: usize, id: &str, bpm: f64) {
        let Some(clip) = self.clip(id) else { return };
        let (ratio, source) = (bpm / clip.bpm, clip.buffer.clone());
        self.loops.stretch_token += 1;
        let token = self.loops.stretch_token;
        self.loops.voices[v].stretching = Some(token);
        self.loops.voices[v].stretch_want = None;
        let id = id.to_string();
        if self.loops.deterministic {
            let out = Arc::new(time_stretch::stretch(&source, ratio));
            return self.finish_stretch(token, &id, out);
        }
        let tx = self.loops.stretch_tx.clone();
        std::thread::spawn(move || {
            let out = Arc::new(time_stretch::stretch(&source, ratio));
            let _ = tx.send((token, id, out));
        });
    }

    /// Take in the copies the worker has finished (the main-actor hop in Swift).
    fn take_stretched(&mut self) {
        while let Ok((token, id, buffer)) = self.loops.stretch_rx.try_recv() {
            self.finish_stretch(token, &id, buffer);
        }
    }

    fn finish_stretch(&mut self, token: u64, id: &str, buffer: Arc<SampleBuffer>) {
        let Some(c) = self.clip(id) else { return };
        // the copy holds the clip's beats: its tempo follows from its length
        let bpm = loop_sound::stretched_bpm(c.bpm, c.buffer.length(), buffer.length());
        let clip_bpm = c.bpm;
        self.loops
            .stretch_cache
            .insert(id.to_string(), (bpm, buffer.clone()));
        let Some(v) = self
            .loops
            .voices
            .iter()
            .position(|x| x.clip.as_deref() == Some(id) && x.stretching == Some(token))
        else {
            return;
        };
        self.loops.voices[v].stretching = None;
        let off_tempo = (self.loops.voices[v].target_bpm / clip_bpm - 1.0).abs()
            > loop_sound::KEY_LOCK_TOLERANCE;
        if self.loops.voices[v].stop_frame.is_some() || !self.is_key_locked(id) || !off_tempo {
            return;
        }
        self.swap_voice(v, buffer, bpm);
    }

    /// What voice `v` was handed by a scheduled swap (`loop_pads`' transpositions): the audio's
    /// tempo, its length, the speed.
    pub(crate) fn set_voice_buffer(&mut self, v: usize, bpm: f64, len: f64, ratio: f64) {
        let voice = &mut self.loops.voices[v];
        voice.buffer_bpm = bpm;
        voice.buffer_length = len;
        voice.ratio = ratio;
    }

    /// Voice `v` plays `buffer` (the same beats, at `bpm`) from the next block, in phase.
    fn swap_voice(&mut self, v: usize, buffer: Arc<SampleBuffer>, bpm: f64) {
        let ratio = self.loops.voices[v].target_bpm / bpm;
        let len = buffer.length() as f64;
        self.loop_send(LoopMsg::Voice(
            v,
            TurntableCommand::Swap(buffer, ratio - 1.0),
        ));
        let voice = &mut self.loops.voices[v];
        voice.buffer_bpm = bpm;
        voice.buffer_length = len;
        voice.ratio = ratio;
    }

    // bookkeeping

    /// Drop clip `id`'s stretched copy (the audio it was made from is gone).
    pub(crate) fn drop_stretch(&mut self, id: &str) {
        self.loops.stretch_cache.remove(id);
    }

    /// Every clip some voice holds, once each (`Set(loopVoices.compactMap(\.clipID))`).
    pub(crate) fn looping_voice_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .loops
            .voices
            .iter()
            .filter_map(|v| v.clip.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// A clip left the row: forget how it sounded (`forgetClipSound`).
    pub fn forget_clip_sound(&mut self, id: &str) {
        self.loops.stretch_cache.remove(id);
        if self.loops.row.settings.forget(id) {
            self.push_clip_levels();
        }
    }

    /// `new` takes `old`'s place on the row (a crop, an edited sequence): it sounds the same way
    /// (`carryClipSound`).
    pub fn carry_clip_sound(&mut self, old: &str, new: &str) {
        self.loops.row.settings.carry(old, new);
        self.loops.stretch_cache.remove(old);
    }
}

// MARK: clip FX (LoopFxEdits.swift)

impl Engine {
    /// Clip `id`'s FX, on the row or removed (`clipFx`: `clip(id)?.fx ?? removedClips...`); off
    /// for an unknown clip. A removed clip keeps its FX, so they come back with `restore_clip`.
    pub fn clip_fx(&self, id: &str) -> LoopFx {
        self.clip(id)
            .or_else(|| {
                self.loops
                    .row
                    .removed
                    .iter()
                    .find(|r| r.clip.id == id)
                    .map(|r| &r.clip)
            })
            .map(|c| c.fx.clone())
            .unwrap_or_default()
    }

    /// Set clip `id`'s FX (clamped into range), on the row and among the removed clips
    /// (`editClip`: a removed clip's FX change too, and come back with `restore_clip`). A voice
    /// playing it changes over smoothly. `at`: the loop voice playing it changes at that frame
    /// instead of at once (a scene launch, on its boundary). A deck playing the clip changes at
    /// once, whatever `at` says (`Deck.setClipFx`).
    pub fn set_clip_fx(&mut self, id: &str, fx: &LoopFx, at: Option<i64>) {
        let f = fx.clamped();
        self.loops.row.edit_clip(id, |c| c.fx = f.clone());
        let holding: Vec<String> = self
            .decks
            .iter()
            .filter(|d| d.clip_id.as_deref() == Some(id))
            .map(|d| d.name.clone())
            .collect();
        for d in holding {
            self.set_deck_clip_fx(&d, &f, false);
        }
        let Some(v) = self.voice_of(id) else { return };
        let p = LoopFxParams::new(&f, self.sample_rate);
        match at {
            Some(frame) => self.loop_send(LoopMsg::FxAt(v, p, frame)),
            None => self.loop_send(LoopMsg::Fx(v, p)),
        };
    }
}

impl Engine {
    /// `Deck.setClipFx(_:now:)`: play the deck's clip through `fx` (the loop row's chain, on this
    /// deck). `now`: start clean at them (a clip being loaded); otherwise a playing deck changes
    /// over smoothly. Sample mode only: the commands wait while the deck plays a net.
    pub fn set_deck_clip_fx(&mut self, deck: &str, fx: &LoopFx, now: bool) {
        let p = LoopFxParams::new(fx, self.sample_rate);
        let Some(d) = self.deck_mut(deck) else { return };
        if !(p.active || d.clip_fx_on) {
            return;
        }
        d.clip_fx_on = p.active;
        let sounding = !now && d.playing;
        self.command(
            deck,
            super::core::Command::ClipFx {
                params: p,
                sounding,
            },
        );
    }

    /// `Deck.clipID = nil`: the clip's stay ends, and its FX go with it
    /// (`didSet { if clipID == nil { setClipFx(.off, now: true) } }`).
    pub(crate) fn clear_deck_clip(&mut self, deck: &str) {
        if let Some(d) = self.deck_mut(deck) {
            d.clip_id = None;
        }
        self.set_deck_clip_fx(deck, &LoopFx::default(), true);
    }
}

// MARK: the loops' master FX (LoopMasterFx.swift)

impl Engine {
    /// Beats a fire on the loops waits for: the launch quantum, at most a bar.
    pub fn loop_master_quantum(&self) -> i64 {
        self.loops.quantum.clamp(1, 4)
    }

    /// The loop-bus fire frame for a fire now (`fireLoopMaster`): for the UI's countdown and
    /// tests.
    pub fn loop_master_fire_frame(&self) -> i64 {
        self.loop_clock(None, None)
            .next_boundary(self.loop_frames() + LOOP_LEAD, self.loop_master_quantum())
    }

    /// Fire master macro `m` on the loops for `bars` bars (None: its default; 0: permanent), on
    /// the next launch line (`at_once`: the next block), at the loop clock's tempo
    /// (`fireLoopMaster`).
    pub fn fire_loop_master(&mut self, m: MasterMacro, bars: Option<i32>, at_once: bool) {
        let clock = self.loop_clock(None, None);
        let frame = if at_once {
            0
        } else {
            clock.next_boundary(self.loop_frames() + LOOP_LEAD, self.loop_master_quantum())
        };
        let bpm = clock.bpm.unwrap_or(120.0);
        self.loop_send(LoopMsg::Master(LoopMasterCommand::Macro {
            id: m.raw(),
            bars: bars.map_or(-1, |b| b.clamp(i16::MIN as i32, i16::MAX as i32) as i16),
            frame,
            bpm: if bpm.is_finite() && bpm > 0.0 {
                bpm
            } else {
                120.0
            },
        }));
        let l = &mut self.loops;
        l.master_sent += 1;
        l.master_queued_mask
            .set(l.master_queued_mask.get() | m.bit());
        l.master_queued_at = l.master_sent;
    }

    /// A hand on a master FX slider (a fader aimed at the loops).
    pub fn set_loop_master_slider(&mut self, key: MasterFxKey, value: i32) {
        self.loops.master_sliders.insert(key.index() as u8, value);
        self.loop_send(LoopMsg::Master(LoopMasterCommand::Slider {
            key: key.index() as u8,
            value: value.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        }));
    }

    /// The last value each master FX slider was set to by hand.
    pub fn loop_master_slider_set(&self, key: MasterFxKey) -> Option<i32> {
        self.loops.master_sliders.get(&(key.index() as u8)).copied()
    }

    /// Release everything: animations cancelled and restored, waiting fires dropped, a brake out
    /// (`LoopMasterBus.panic`).
    pub fn loop_master_panic(&mut self) {
        self.loop_send(LoopMsg::Master(LoopMasterCommand::Panic));
        self.loops.master_queued_mask.set(0);
    }

    /// The FX unit's hands on the loops' chain (`sendFxUnit`).
    pub fn send_loop_fx_unit(&mut self, c: FxUnitCommand) {
        self.loop_send(LoopMsg::Master(LoopMasterCommand::FxUnit(c)));
    }

    /// A deck's master macro (None: its panic), passed on to the loops while they are linked
    /// (`linkedLoopMaster`).
    pub fn linked_loop_master(&mut self, m: Option<MasterMacro>, bars: Option<i32>) {
        if !self.loops.fx_link {
            return;
        }
        match m {
            Some(m) => self.fire_loop_master(m, bars, true),
            None => self.loop_master_panic(),
        }
    }

    /// Link the loops' master FX to the decks' (`loopFxLink`).
    pub fn set_loop_fx_link(&mut self, on: bool) {
        self.loops.fx_link = on;
    }

    pub fn loop_fx_link(&self) -> bool {
        self.loops.fx_link
    }

    /// The master macros running (or waiting for their bar) as of the last block, as bits.
    pub(crate) fn loop_master_running_mask(&self) -> u32 {
        let l = &self.loops;
        // once the audio thread has taken the fires (pending or running), its mask speaks for them
        if l.master_queued_mask.get() != 0
            && l.mirror.master_seen.load(Ordering::Acquire) >= l.master_queued_at
        {
            l.master_queued_mask.set(0);
        }
        l.mirror.master_mask.load(Ordering::Relaxed) | l.master_queued_mask.get()
    }

    pub fn is_loop_master_active(&self, m: MasterMacro) -> bool {
        self.loop_master_running_mask() & m.bit() != 0
    }

    /// The master macros running (or waiting for their bar) as of the last block.
    pub fn active_loop_master_macros(&self) -> Vec<MasterMacro> {
        let mask = self.loop_master_running_mask();
        MasterMacro::ALL
            .iter()
            .copied()
            .filter(|m| mask & m.bit() != 0)
            .collect()
    }

    /// The loops' chain HP and LP corners (Hz) at the last block, and whether it was at rest.
    pub fn loop_master_filter_corners(&self) -> (f64, f64, bool) {
        let m = &self.loops.mirror;
        (
            f64::from_bits(m.hp_bits.load(Ordering::Relaxed)),
            f64::from_bits(m.lp_bits.load(Ordering::Relaxed)),
            m.master_idle.load(Ordering::Relaxed),
        )
    }
}
