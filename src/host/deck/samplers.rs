//! Sampled instruments inside a deck (`SamplerBank` in `Audio/Sampler.swift`, as far as it
//! ports, and the sampled half of `WaveRunner`: `samplerNote`, `flushSamplers`, `renderScratch`).
//!
//! Two halves, like the Swift bank:
//!
//! - [`SamplerBank`] (main thread, feature `sf2`): picks a sound for a `(instrument, part)`
//!   by the voice catalog's fallback ranking, opens it (rustysynth), runs the load-time checks
//!   and numbers it. The wave runner asks for it through [`HostSlots`] (its `WaveOptions::bank`:
//!   the track's picks and palette option, the drum kit) or [`bind_thunk`] (by name alone) while
//!   a project is being prepared; the loaded samplers are handed to the audio thread in a
//!   command. A slot that loads measures how loud each part plays on it against the synth voice
//!   (`SamplerPartLevels`), which the runner reads back through the same binding
//!   ([`part_level_thunk`]) to set the slot's `.balance` gain and the loudness probe's stand-in.
//! - [`SamplerSet`] (audio thread): the loaded samplers, rendered by hand inside the deck's
//!   callback so MIDI is sample-exact and offline rendering works. Swift renders its bank inside
//!   `WaveRunner.process`; the Rust runner only reports what the sampled lanes and the live keys
//!   asked for ([`SampleNote`]s with their lengths, [`SampleKey`]s as raw MIDI) and
//!   [`SamplerSet::flush`] plays it right after the block: the net notes end after at least
//!   120 ms (a note struck again while held is ended first), and each slot is added into the
//!   block as the project says (the kick's duck, the `.balance` gain, the stereo width and side
//!   highpass of a single slot) with its sends to the room and the echo handed back to the runner.
//!   Every buffer is allocated up front.
//!
//! `samplersInCurve` (`.loudness`: the samplers join the voices before the master curve) is the
//! runner's: [`WaveRunner::process_with`] takes the set as a [`SamplerHost`] and calls
//! [`SamplerSet::flush`] where Swift's `process` calls `flushSamplers`, into a scratch pair when
//! the slots belong in the curve, into the finished block when they do not.
//!
//! The bank's slots are a cache, as Swift's are: [`BANK_SLOTS`] of them, each with a count of the
//! references taken on it (`slot_for` takes one, a project gives it back when it drops), and a
//! full bank reuses the least recently used slot nothing references, so a long set (Wander
//! changes instruments all the time) keeps getting sampled sounds. What is **not** ported is the
//! half of Swift's eviction that guards AUSampler: its two-phase `active` / `epoch` handshake
//! with the audio thread and the process-wide lifecycle lock. Here the new sampler goes to the
//! audio thread in a command, ahead of the project that plays it, and the one it replaces goes
//! back to the main thread through a retire ring ([`SamplerSet::set_slot`]), so nothing
//! waits on the audio thread and the audio thread never frees; a slot with no references is one
//! no project can reach, which is all the epoch protocol was there to establish.

use crate::musicality::dj::SAMPLED_SIDE_HZ;
use crate::musicality::energy_curve::SideHighpass;
use crate::wave::runner::SamplerHost;
use crate::wave::runner::{LaneEnd, SampleKey, SampleNote, MAX_BLOCK as RUNNER_BLOCK};
use crate::wave::WaveRunner;

#[cfg(feature = "sf2")]
use crate::host::sampler::Sf2Sampler;

/// Slots a deck's bank holds (`SamplerBank.capacity`).
pub const BANK_SLOTS: usize = 24;
/// Longest chunk a sampler renders in one call (the brake's chunk is at most 4092).
pub const MAX_CHUNK: usize = 8192;
const EVENT_CAPACITY: usize = 4096;
const PENDING_CAPACITY: usize = 1024;
/// How long a slot keeps rendering after its last note ends (a release and room tail).
#[cfg(feature = "sf2")]
const RING_OUT_SECONDS: f64 = 6.0;
/// The shortest a net note sounds, ms: one-step notes still speak (`samplerNote`).
const MIN_NOTE_MS: f64 = 120.0;

/// A note-off the set owes a slot: what `Lane.offNote` / `offAt` keep in Swift.
#[derive(Clone, Copy)]
struct PendingOff {
    slot: u8,
    key: u8,
    due: i64,
    /// The lane that struck the note (`SampleNote::lane`): Swift's queue is the lane's own.
    lane: u64,
}

/// A MIDI message for a slot: the note and the three channel messages the runner sends.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "sf2"), allow(dead_code))]
enum Kind {
    On,
    Off,
    /// CC 123: every key released, the tails ring out.
    AllOff,
    /// CC 120: all sound off.
    SoundOff,
}

/// One message at a sample of the block; `seq` is the order it was queued in (Swift's one queue
/// keeps that order for messages at the same sample).
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "sf2"), allow(dead_code))]
struct Ev {
    at: usize,
    seq: u32,
    kind: Kind,
    slot: u8,
    key: u8,
    vel: u8,
}

#[cfg(feature = "sf2")]
struct Slot {
    sampler: Sf2Sampler,
    held: u32,
    /// Absolute frame until which the slot is rendered (notes held: forever).
    active_until: i64,
}

/// What the project asks of the sampled slots in a block (`duckingSlots`, `sendingSlots`,
/// `gainedSlots`, `echoingSlots`, `widenedSlots`, `sidedSlots` and their per-slot values),
/// copied from the project playing at the top of the block.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "sf2"), allow(dead_code))]
struct SlotMix {
    ducking: u64,
    sending: u64,
    gained: u64,
    echoing: u64,
    widened: u64,
    sided: u64,
    gain: [f32; 64],
    echo: [f32; 64],
    width: [f32; 64],
}

impl SlotMix {
    const NONE: SlotMix = SlotMix {
        ducking: 0,
        sending: 0,
        gained: 0,
        echoing: 0,
        widened: 0,
        sided: 0,
        gain: [1.0; 64],
        echo: [0.0; 64],
        width: [1.0; 64],
    };

    /// The gates of `WaveRunner.process`: each set of slots exists only when the project has it
    /// and the block fits the scratch (`frames` at most [`RUNNER_BLOCK`]).
    #[cfg(feature = "sf2")]
    fn of(runner: &WaveRunner, frames: usize) -> SlotMix {
        let mut m = SlotMix::NONE;
        let Some(p) = runner.project().filter(|_| frames <= RUNNER_BLOCK) else {
            return m;
        };
        if p.duck_depth > 0.0 {
            m.ducking = p.duck_slots();
        }
        m.sending = p.send_slots();
        m.gained = p.gain_slots();
        // an echo that is off sends nothing (`echoFx.isOn`)
        if p.echo.is_some_and(|e| e.wet > 0.0) {
            m.echoing = p.echo_slots();
        }
        m.widened = p.width_slots();
        m.sided = p.side_slots();
        let mut b = m.echoing;
        while b != 0 {
            let i = b.trailing_zeros() as usize;
            b &= b - 1;
            m.echo[i] = p.slot_echo(i);
        }
        let mut b = m.widened;
        while b != 0 {
            let i = b.trailing_zeros() as usize;
            b &= b - 1;
            m.width[i] = p.slot_width(i);
        }
        let mut b = m.gained;
        while b != 0 {
            let i = b.trailing_zeros() as usize;
            b &= b - 1;
            m.gain[i] = p.slot_gain(i);
        }
        m
    }
}

/// The loaded samplers of one deck, on the audio thread.
///
/// Port of the sampled half of `WaveRunner`: `samplerNote`'s note-offs, `flushSamplers`,
/// `renderScratch` and the sampler send fifos. Swift renders the bank inside `process`; here
/// the runner reports its sampled notes and the live keys' MIDI ([`SampleNote`], [`SampleKey`])
/// and [`flush`](Self::flush) plays them right after the block, with the project's duck, gain,
/// width, side highpass and sends.
pub struct SamplerSet {
    sample_rate: f64,
    #[cfg(feature = "sf2")]
    slots: Vec<Option<Slot>>,
    pending: Vec<PendingOff>,
    events: Vec<Ev>,
    seq: u32,
    /// Samples rendered so far: the time base of the note-offs (`clock[clkSamples]`).
    clock: i64,
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    mix: SlotMix,
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    scratch_l: Vec<f32>,
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    scratch_r: Vec<f32>,
    /// The scratch pair a group (or one slot) of slots sums into (`duckL` / `duckR`).
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    group_l: Vec<f32>,
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    group_r: Vec<f32>,
    /// The runner's duck gain on each frame of the block (`duckBuf`), copied.
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    duck: Vec<f32>,
    /// The mono mix of the slots that send to the room, and the echo's (`sendBuf`, `echoBuf`).
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    send: Vec<f32>,
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    echo: Vec<f32>,
    /// Each slot's side highpass (`slotSide`).
    #[cfg_attr(not(feature = "sf2"), allow(dead_code))]
    side: Vec<SideHighpass>,
    /// Frames of the last block flushed.
    last_frames: usize,
    /// Notes that did not fit the event list (never in practice).
    pub dropped: u64,
    /// Samplers a slot let go of, waiting for [`pop_retired`](Self::pop_retired): the audio
    /// thread never frees one.
    #[cfg(feature = "sf2")]
    retired: Vec<Sf2Sampler>,
    /// Samplers the retire list had no room for, dropped on the audio thread (never in practice:
    /// the host drains the list every block).
    pub retire_overflow: u64,
}

impl SamplerSet {
    pub fn new(sample_rate: f64) -> SamplerSet {
        SamplerSet {
            sample_rate,
            #[cfg(feature = "sf2")]
            slots: (0..BANK_SLOTS).map(|_| None).collect(),
            pending: Vec::with_capacity(PENDING_CAPACITY),
            events: Vec::with_capacity(EVENT_CAPACITY),
            seq: 0,
            clock: 0,
            mix: SlotMix::NONE,
            scratch_l: vec![0.0; MAX_CHUNK],
            scratch_r: vec![0.0; MAX_CHUNK],
            group_l: vec![0.0; MAX_CHUNK],
            group_r: vec![0.0; MAX_CHUNK],
            duck: vec![1.0; RUNNER_BLOCK],
            send: vec![0.0; MAX_CHUNK],
            echo: vec![0.0; MAX_CHUNK],
            side: (0..64)
                .map(|_| SideHighpass::new(SAMPLED_SIDE_HZ, sample_rate))
                .collect(),
            last_frames: 0,
            dropped: 0,
            #[cfg(feature = "sf2")]
            retired: Vec::with_capacity(2 * BANK_SLOTS),
            retire_overflow: 0,
        }
    }

    /// The net notes still owed an end, as (slot, key) (`Lane.offNote` of every lane, for
    /// tests): a note ends at its length (at least 120 ms), at a retrigger, a choke, a stop or
    /// when its lane goes to its synth voice.
    pub fn owed_notes(&self) -> Vec<(usize, u8)> {
        self.pending
            .iter()
            .map(|p| (p.slot as usize, p.key))
            .collect()
    }

    /// The mono mix of the slots that send to the room in the last block, after their gain
    /// and duck (`sendBuf`; what [`WaveRunner::queue_sampler_send`] was given).
    pub fn room_send(&self) -> &[f32] {
        &self.send[..self.last_frames]
    }

    /// The same for the echo (`echoBuf`: each slot's mono times its echo send).
    pub fn echo_send(&self) -> &[f32] {
        &self.echo[..self.last_frames]
    }

    /// Put a loaded sampler in `slot` (`Command::Sampler`; `publish` in Swift's `slot(for:)`).
    ///
    /// A slot that holds another source's sampler is an eviction: the bank gave the slot to the
    /// source being loaded, and no project that plays it is left (it had no references). The old
    /// sound is cut (its notes and the note-offs owed it forgotten) and handed to the retire list
    /// ([`pop_retired`](Self::pop_retired)), so the render thread never frees it. The same
    /// source sent again (its slot was given up and the same source loaded into it again before
    /// anything else was sent there) leaves the playing sampler as it is. Real-time safe.
    #[cfg(feature = "sf2")]
    pub fn set_slot(&mut self, slot: usize, sampler: Sf2Sampler) {
        let playing_it = matches!(
            self.slots.get(slot),
            Some(Some(s)) if s.sampler.source == sampler.source
                && s.sampler.sample_rate() == sampler.sample_rate()
        );
        if playing_it {
            return self.retire(sampler);
        }
        let Some(cell) = self.slots.get_mut(slot) else {
            return self.retire(sampler);
        };
        let old = cell.replace(Slot {
            sampler,
            held: 0,
            active_until: 0,
        });
        if let Some(old) = old {
            self.pending.retain(|p| p.slot as usize != slot);
            self.retire(old.sampler);
        }
    }

    /// What the sampler in `slot` plays: the audio thread's side of
    /// [`SamplerBank::source_of`], which says what the slot is for. They agree once every sampler
    /// the bank has loaded has been sent.
    #[cfg(feature = "sf2")]
    pub fn source_of(&self, slot: usize) -> Option<&crate::voices::SamplerSource> {
        self.slots.get(slot)?.as_ref().map(|s| &s.sampler.source)
    }

    /// Keep `s` for the main thread to free (a push within the list's capacity makes no
    /// allocation), or drop it here when the list is full.
    #[cfg(feature = "sf2")]
    fn retire(&mut self, s: Sf2Sampler) {
        if self.retired.len() < self.retired.capacity() {
            self.retired.push(s);
        } else {
            self.retire_overflow += 1;
        }
    }

    /// A sampler a slot let go of, for the main thread to free: the host calls this where it
    /// collects the projects a deck retires (`drainRetired`). Real-time safe.
    #[cfg(feature = "sf2")]
    pub fn pop_retired(&mut self) -> Option<Sf2Sampler> {
        self.retired.pop()
    }

    /// How many slots hold a sampler.
    pub fn loaded(&self) -> usize {
        #[cfg(feature = "sf2")]
        {
            self.slots.iter().filter(|s| s.is_some()).count()
        }
        #[cfg(not(feature = "sf2"))]
        {
            0
        }
    }

    /// Every sampler silent, tails included (`runner.silence()`: CC 120 on every slot, a stop
    /// under the brake).
    pub fn silence_all(&mut self) {
        self.pending.clear();
        #[cfg(feature = "sf2")]
        for s in self.slots.iter_mut().flatten() {
            s.sampler.silence();
            s.held = 0;
            s.active_until = 0;
        }
    }

    /// Every key of every sampler released and the notes still owed an end forgotten
    /// (`samplersAllOff`: CC 123 on every slot, from `stop` and `panic`). The tails ring out.
    pub fn all_notes_off(&mut self) {
        self.pending.clear();
        #[cfg(feature = "sf2")]
        {
            let until = self.clock + (RING_OUT_SECONDS * self.sample_rate) as i64;
            for s in self.slots.iter_mut().flatten() {
                s.sampler.all_notes_off();
                s.held = 0;
                s.active_until = s.active_until.min(until);
            }
        }
    }

    /// The slots' side highpasses cleared (`stop` resets `slotSide`).
    pub fn reset_sides(&mut self) {
        for h in self.side.iter_mut() {
            h.reset();
        }
    }

    fn push(&mut self, at: usize, kind: Kind, slot: u8, key: u8, vel: u8) {
        if self.events.len() >= EVENT_CAPACITY {
            self.dropped += 1;
            return;
        }
        self.events.push(Ev {
            at,
            seq: self.seq,
            kind,
            slot,
            key,
            vel,
        });
        self.seq = self.seq.wrapping_add(1);
    }

    /// A net note on a sampled lane: note-on now, note-off after its length (at least 120 ms,
    /// so one-step notes still speak). A note struck again while held is ended first
    /// (`samplerNote`).
    fn net_note(&mut self, n: SampleNote, start: i64, frames: usize) {
        if n.slot >= BANK_SLOTS {
            self.dropped += 1;
            return;
        }
        let at = (n.frame - start).clamp(0, frames.max(1) as i64 - 1) as usize;
        let (slot, key) = (n.slot as u8, n.note.clamp(0, 127) as u8);
        let on = self.clock + at as i64;
        // (the same lane's: another lane on the slot holds its own note of that key to its end)
        if let Some(k) = self
            .pending
            .iter()
            .position(|p| p.lane == n.lane && p.slot == slot && p.key == key && p.due >= on)
        {
            self.pending.swap_remove(k);
            self.push(at, Kind::Off, slot, key, 0);
        }
        self.push(at, Kind::On, slot, key, n.velocity.clamp(1, 127) as u8);
        if self.pending.len() >= PENDING_CAPACITY {
            // (Swift's lane has no room to remember the end either: the note holds on)
            self.dropped += 1;
            return;
        }
        let ms = if n.dur_ms > 0.0 {
            n.dur_ms
        } else {
            MIN_NOTE_MS
        }
        .max(MIN_NOTE_MS);
        // the end falls on the first sample at or after `on + ms`
        let due = on + (ms / 1000.0 * self.sample_rate).ceil() as i64;
        self.pending.push(PendingOff {
            slot,
            key,
            due,
            lane: n.lane,
        });
    }

    /// A lane's notes ended now (`applyVoice`: a note-off for each of the lane's pending notes,
    /// then its queue cleared): the lane that goes to its synth voice stops what it holds on the
    /// sampler, and nothing else does.
    fn end_lane(&mut self, e: LaneEnd, start: i64, frames: usize) {
        let at = (e.frame - start).clamp(0, frames.max(1) as i64 - 1) as usize;
        let mut k = 0;
        while k < self.pending.len() {
            if self.pending[k].lane == e.lane {
                let p = self.pending.swap_remove(k);
                self.push(at, Kind::Off, p.slot, p.key, 0);
            } else {
                k += 1;
            }
        }
    }

    /// A raw MIDI message of the live keys, or of a sampled kit lane's choke
    /// (`queue(slot, status, d1, d2)`).
    fn key_message(&mut self, k: SampleKey, start: i64, frames: usize) {
        if k.slot >= BANK_SLOTS {
            self.dropped += 1;
            return;
        }
        let at = (k.frame - start).clamp(0, frames.max(1) as i64 - 1) as usize;
        let kind = match (k.status & 0xF0, k.data1) {
            (0x90, _) => Kind::On,
            (0x80, _) => Kind::Off,
            (0xB0, 123) => Kind::AllOff,
            (0xB0, 120) => Kind::SoundOff,
            _ => return,
        };
        if kind == Kind::Off {
            // a note-off for a key the set still owes an end ends that hold here: a sampled kit
            // lane's choke or re-strike (`endSamplerNotes` clears the lane's `offNote`s), so no
            // second note-off follows
            self.pending
                .retain(|p| !(p.slot == k.slot as u8 && p.key == k.data1));
        }
        self.push(at, kind, k.slot as u8, k.data1, k.data2);
    }

    /// The top of a block, before the runner processes it: which sets of slots the project that
    /// starts the block asks for, and each one's value (the gates at the top of
    /// `WaveRunner.process`). Swift reads them there, so a queued project that takes over at a
    /// bar in the middle of the block does not change them until the next block: the old
    /// project's slots are still ducked, trimmed and sent as it said. Call it with the `frames`
    /// the runner is given, then [`flush`](Self::flush) after the runner. Real-time safe.
    pub fn begin_block(&mut self, runner: &WaveRunner, frames: usize) {
        #[cfg(feature = "sf2")]
        {
            self.mix = SlotMix::of(runner, frames);
        }
        #[cfg(not(feature = "sf2"))]
        let _ = (runner, frames);
    }

    /// Play what the runner reported for the block it just rendered into `left` / `right`
    /// (`flushSamplers`): the live keys' MIDI in the order pressed, then the net's notes and the
    /// note-offs that fall due, each at its sample; the slots add into the block, ducked on
    /// the kick, trimmed, widened and sent to the room and the echo as the project said at
    /// [`begin_block`](Self::begin_block). `start` is the `start_frame` the runner got.
    /// Real-time safe.
    pub fn flush(
        &mut self,
        runner: &mut WaveRunner,
        start: i64,
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        self.events.clear();
        self.seq = 0;
        // (a lane's notes end before any key of the block that follows it: a live key struck on
        // the same note comes after the end, as it does in Swift's queue)
        for e in runner.lane_ends.drain(..) {
            self.end_lane(e, start, frames);
        }
        for k in runner.sample_keys.drain(..) {
            self.key_message(k, start, frames);
        }
        for n in runner.sample_notes.drain(..) {
            self.net_note(n, start, frames);
        }
        // the note-offs due in this block
        let end = self.clock + frames as i64;
        let mut k = 0;
        while k < self.pending.len() {
            let p = self.pending[k];
            if p.due < end {
                self.push(
                    (p.due - self.clock).max(0) as usize,
                    Kind::Off,
                    p.slot,
                    p.key,
                    0,
                );
                self.pending.swap_remove(k);
            } else {
                k += 1;
            }
        }
        self.events.sort_unstable_by_key(|e| (e.at, e.seq));
        self.last_frames = frames.min(MAX_CHUNK);
        #[cfg(feature = "sf2")]
        self.render(runner, left, right, frames);
        #[cfg(not(feature = "sf2"))]
        let _ = (left, right);
        self.clock = end;
    }

    #[cfg(feature = "sf2")]
    fn render(
        &mut self,
        runner: &mut WaveRunner,
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        if self.mix.ducking != 0 {
            let d = runner.duck_gains(frames);
            self.duck[..d.len()].copy_from_slice(d);
        }
        let live = self.live();
        let m = self.mix;
        // the slots that send (to the room, or to the echo) run on a cleared buffer, whether
        // or not one of them sounds
        let single = live & (m.gained | m.echoing | m.widened);
        let sending = live & m.sending;
        let echoing = single & m.echoing;
        if sending != 0 {
            self.send[..frames].fill(0.0);
        }
        if echoing != 0 {
            self.echo[..frames].fill(0.0);
        }
        let mut pos = 0;
        for i in 0..self.events.len() {
            let e = self.events[i];
            if e.at > pos {
                self.render_segment(left, right, pos, e.at);
                pos = e.at;
            }
            self.apply(e, self.clock + e.at as i64);
        }
        if pos < frames {
            self.render_segment(left, right, pos, frames);
        }
        if sending != 0 {
            runner.queue_sampler_send(&self.send[..frames]);
        }
        if echoing != 0 {
            runner.queue_sampler_echo(&self.echo[..frames]);
        }
    }

    /// The loaded slots, bit i = slot i (`bank.beginBlock()`).
    #[cfg(feature = "sf2")]
    fn live(&self) -> u64 {
        let mut live = 0;
        for (i, s) in self.slots.iter().enumerate() {
            if s.is_some() {
                live |= 1u64 << i;
            }
        }
        live
    }

    #[cfg(feature = "sf2")]
    fn apply(&mut self, e: Ev, abs: i64) {
        let ring = (RING_OUT_SECONDS * self.sample_rate) as i64;
        let Some(Some(s)) = self.slots.get_mut(e.slot as usize) else {
            return;
        };
        match e.kind {
            Kind::On => {
                s.sampler.note_on(e.key, e.vel);
                s.held += 1;
                s.active_until = i64::MAX;
            }
            Kind::Off => {
                s.sampler.note_off(e.key);
                s.held = s.held.saturating_sub(1);
                if s.held == 0 {
                    s.active_until = abs + ring;
                }
            }
            Kind::AllOff => {
                s.sampler.all_notes_off();
                s.held = 0;
                s.active_until = abs + ring;
            }
            Kind::SoundOff => {
                s.sampler.silence();
                s.held = 0;
                s.active_until = 0;
            }
        }
    }

    /// Render `slot` for `n` samples from absolute sample `abs` into the slot scratch, times the
    /// sampler's own level. False (and nothing rendered) when the slot is empty or has rung out.
    #[cfg(feature = "sf2")]
    fn render_slot(&mut self, slot: usize, abs: i64, n: usize) -> bool {
        let Some(Some(s)) = self.slots.get_mut(slot) else {
            return false;
        };
        if abs >= s.active_until {
            return false;
        }
        let (sl, sr_) = (&mut self.scratch_l[..n], &mut self.scratch_r[..n]);
        s.sampler.render_into(sl, sr_);
        let g = s.sampler.gain as f32;
        for v in sl.iter_mut().chain(sr_.iter_mut()) {
            *v *= g;
        }
        true
    }

    /// Samples `from..to` of the block, in chunks the scratch holds (`renderAll` and its
    /// groups). The slots with a gain, an echo send or a width go one at a time (each with its
    /// own duck and sends); ducked and sending slots by group through the scratch pair; the
    /// rest straight into the block.
    #[cfg(feature = "sf2")]
    fn render_segment(&mut self, left: &mut [f32], right: &mut [f32], from: usize, to: usize) {
        let m = self.mix;
        let live = self.live();
        let single = live & (m.gained | m.echoing | m.widened);
        let ducked = live & m.ducking & !single;
        let sent = live & m.sending & !single;
        let plain = live & !ducked & !sent & !single;
        let mut a = from;
        while a < to {
            let n = (to - a).min(MAX_CHUNK);
            let abs = self.clock + a as i64;
            let mut bits = plain;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if self.render_slot(i, abs, n) {
                    for k in 0..n {
                        left[a + k] += self.scratch_l[k];
                        right[a + k] += self.scratch_r[k];
                    }
                }
            }
            for group in 0..3 {
                let mask = match group {
                    0 => ducked & !sent,
                    1 => sent & !ducked,
                    _ => ducked & sent,
                };
                if mask != 0 {
                    self.add_slots(
                        mask,
                        left,
                        right,
                        a,
                        n,
                        abs,
                        1.0,
                        group != 1,
                        group != 0,
                        0.0,
                    );
                }
            }
            let mut bits = single;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let bit = 1u64 << i;
                let widened = m.widened & bit != 0;
                self.add_slot_alone(
                    i,
                    left,
                    right,
                    a,
                    n,
                    abs,
                    if m.gained & bit != 0 { m.gain[i] } else { 1.0 },
                    m.ducking & bit != 0,
                    m.sending & bit != 0,
                    if m.echoing & bit != 0 { m.echo[i] } else { 0.0 },
                    if widened { m.width[i] } else { 1.0 },
                    widened && m.sided & bit != 0,
                );
            }
            a += n;
        }
    }

    /// `renderScratch` for a group: `mask`'s slots summed into the cleared scratch pair, then
    /// added to the block at `gain`, ducked as asked, into the room's send as asked and, `echo` >
    /// 0, that much into the echo's.
    #[cfg(feature = "sf2")]
    #[allow(clippy::too_many_arguments)]
    fn add_slots(
        &mut self,
        mask: u64,
        left: &mut [f32],
        right: &mut [f32],
        a: usize,
        n: usize,
        abs: i64,
        gain: f32,
        ducks: bool,
        sends: bool,
        echo: f32,
    ) {
        self.group_l[..n].fill(0.0);
        self.group_r[..n].fill(0.0);
        let mut any = false;
        let mut bits = mask;
        while bits != 0 {
            let i = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if self.render_slot(i, abs, n) {
                any = true;
                for k in 0..n {
                    self.group_l[k] += self.scratch_l[k];
                    self.group_r[k] += self.scratch_r[k];
                }
            }
        }
        if any {
            self.add_group(left, right, a, n, gain, ducks, sends, echo, 1.0, None);
        }
    }

    /// `renderScratch` for one slot, which can play at a stereo `width` (its side scaled), its side
    /// highpassed first when `sided`.
    #[cfg(feature = "sf2")]
    #[allow(clippy::too_many_arguments)]
    fn add_slot_alone(
        &mut self,
        slot: usize,
        left: &mut [f32],
        right: &mut [f32],
        a: usize,
        n: usize,
        abs: i64,
        gain: f32,
        ducks: bool,
        sends: bool,
        echo: f32,
        width: f32,
        sided: bool,
    ) {
        if !self.render_slot(slot, abs, n) {
            return;
        }
        self.group_l[..n].copy_from_slice(&self.scratch_l[..n]);
        self.group_r[..n].copy_from_slice(&self.scratch_r[..n]);
        self.add_group(
            left,
            right,
            a,
            n,
            gain,
            ducks,
            sends,
            echo,
            width,
            sided.then_some(slot),
        );
    }

    /// The tail of `renderScratch`: the scratch pair (side highpassed, widened) times `gain` and
    /// the duck, added to the block and into the sends.
    #[cfg(feature = "sf2")]
    #[allow(clippy::too_many_arguments)]
    fn add_group(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        a: usize,
        n: usize,
        gain: f32,
        ducks: bool,
        sends: bool,
        echo: f32,
        width: f32,
        side: Option<usize>,
    ) {
        if side.is_some() || width != 1.0 {
            for k in 0..n {
                if let Some(h) = side {
                    self.side[h].process(&mut self.group_l[k], &mut self.group_r[k]);
                }
                let m = 0.5 * (self.group_l[k] + self.group_r[k]);
                let s = 0.5 * (self.group_l[k] - self.group_r[k]) * width;
                self.group_l[k] = m + s;
                self.group_r[k] = m - s;
            }
        }
        for k in 0..n {
            let g: f32 = if ducks { gain * self.duck[a + k] } else { gain };
            let l = self.group_l[k] * g;
            let r = self.group_r[k] * g;
            left[a + k] += l;
            right[a + k] += r;
            let mono = 0.5 * (l + r);
            if sends {
                self.send[a + k] += mono;
            }
            if echo > 0.0 {
                self.echo[a + k] += mono * echo;
            }
        }
    }
}

/// The runner renders the set inside its block ([`WaveRunner::process_with`]).
impl SamplerHost for SamplerSet {
    fn begin_block(&mut self, runner: &WaveRunner, frames: usize) {
        SamplerSet::begin_block(self, runner, frames);
    }
    fn flush(
        &mut self,
        runner: &mut WaveRunner,
        start: i64,
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        SamplerSet::flush(self, runner, start, left, right, frames);
    }
}

// MARK: main thread

#[cfg(feature = "sf2")]
mod bank {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::BANK_SLOTS;
    use crate::host::sampler::{load_checked, source_available, Library, PartLevelCache, Sf2Sampler};
    use crate::sound::{sources_for_option, PaletteSet};
    use crate::voices::{drumkits, SamplerSource};
    use crate::wave::runner::SamplerSlots;
    use crate::wave::SamplerPartLevels;

    /// The references taken on a bank's slots (`SamplerBank.table.refs`), shared by handle and
    /// lock-free: a project that is freed gives its references back without waiting behind a load
    /// (which holds the bank itself for as long as it takes to read a font and measure it), and
    /// without a lock at all, so even a project the render thread has to drop (the retire ring was
    /// full) takes none. Only the loader raises a count, under the bank's lock; the loader that
    /// picks a slot with no references therefore finds it still has none when it takes it.
    #[derive(Clone)]
    pub struct SlotRefs(Arc<[AtomicU32; BANK_SLOTS]>);

    impl SlotRefs {
        fn new() -> SlotRefs {
            SlotRefs(Arc::new(std::array::from_fn(|_| AtomicU32::new(0))))
        }

        fn acquire(&self, slot: usize) {
            self.0[slot].fetch_add(1, Ordering::AcqRel);
        }

        /// `SamplerBank.release(_:)`: give back one reference `slot_for` took. A slot released
        /// more often than taken is a bug in the caller: a debug build stops there (Swift's
        /// `assert`), a release build keeps the count at zero.
        pub fn release(&self, slot: usize) {
            let Some(n) = self.0.get(slot) else { return };
            let mut now = n.load(Ordering::Acquire);
            loop {
                if now == 0 {
                    debug_assert!(false, "sampler slot {slot} released more than taken");
                    return;
                }
                match n.compare_exchange_weak(now, now - 1, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => return,
                    Err(seen) => now = seen,
                }
            }
        }

        /// References held on `slot`.
        pub fn count(&self, slot: usize) -> u32 {
            self.0.get(slot).map_or(0, |n| n.load(Ordering::Acquire))
        }

        fn snapshot(&self) -> [u32; BANK_SLOTS] {
            std::array::from_fn(|i| self.0[i].load(Ordering::Acquire))
        }
    }

    /// The samplers a bank has loaded that the audio thread does not have yet.
    ///
    /// Swift's bank owns its instruments and the audio thread plays them where they are; here a
    /// loaded sampler has to be sent to the deck's [`SamplerSet`](super::SamplerSet), and whether
    /// it has been is a property of the slot, not of the prepare that happened to load it: a
    /// track composed ahead and a track prepared a moment later share the sources they bind (the
    /// second finds them numbered and loads nothing), and whichever is handed over first has to
    /// bring the samplers or its lanes play what the slot held before. So a sampler waits here,
    /// not in a prepared track, until the deck is handed a project (any project: see
    /// [`Engine::hand_over_with`](crate::host::deck::Engine)) and drains the lot.
    ///
    /// Shared by handle with its own lock, so a hand-over on the main thread never waits behind a
    /// load (which holds the bank itself for as long as it takes to read a font and measure it).
    /// The loader fills it and evicts from it under the bank's lock, taking this one briefly
    /// inside it; nothing takes the bank's while holding this one.
    #[derive(Clone, Default)]
    pub struct Outbox(Arc<Mutex<Vec<(usize, Sf2Sampler)>>>);

    impl Outbox {
        fn queue(&self) -> std::sync::MutexGuard<'_, Vec<(usize, Sf2Sampler)>> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// A sampler just loaded for `slot` (the bank has numbered it already, after
        /// [`forget`](Self::forget)ting whatever was loaded for the slot before).
        fn put(&self, slot: usize, sampler: Sf2Sampler) {
            self.queue().push((slot, sampler));
        }

        /// `slot` has been given to another source: what was loaded for it and never sent goes.
        fn forget(&self, slot: usize) {
            self.queue().retain(|(i, _)| *i != slot);
        }

        /// Every sampler loaded and not sent yet, which the caller now owes the audio thread.
        pub fn take(&self) -> Vec<(usize, Sf2Sampler)> {
            std::mem::take(&mut *self.queue())
        }

        /// How many samplers wait.
        pub fn len(&self) -> usize {
            self.queue().len()
        }

        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }
    }

    /// The deck's sampled instruments, on the main thread (`SamplerBank`).
    ///
    /// [`BANK_SLOTS`] slots, loaded off the audio thread. `slot_for` takes one reference on the
    /// slot it returns and [`release`](Self::release) gives it back; a `WaveProject` holds one per
    /// sampled lane until it is freed, so a slot no live project (current, pending or fading)
    /// plays has none. A full bank reuses the least recently used slot with no references
    /// (`freeSlot`, `evict`), and answers None, leaving the lane on its synth voice, when every
    /// slot is referenced.
    pub struct SamplerBank {
        library: Arc<Library>,
        sample_rate: u32,
        /// The seed of the track being prepared (picks the pool's first sound).
        pub seed: f64,
        /// What each slot plays, by number (`table.sources`); None for a slot never filled.
        sources: Vec<Option<SamplerSource>>,
        /// Where each loaded source is (`bySource`).
        by_source: BTreeMap<SamplerSource, usize>,
        /// Sources that loaded and were turned away, or would not load: never tried again
        /// (`unloadable`). A source missing from disk is not here, so installing it later works.
        unloadable: BTreeSet<SamplerSource>,
        /// Why each source that was turned away was (`failures`), read by
        /// [`load_failure`](Self::load_failure).
        failures: BTreeMap<SamplerSource, String>,
        /// Slots ever filled: the next never-used slot is this one (`filled`).
        filled: usize,
        /// When each slot was last taken, in `uses` (`lastUsed`: the LRU order).
        last_used: [u64; BANK_SLOTS],
        uses: u64,
        refs: SlotRefs,
        /// Loaded and not yet sent to the audio thread ([`Outbox`]).
        outbox: Outbox,
        /// What each slot measured on the part probes (`SamplerBank.partLevels`: by probe group,
        /// empty for a kit or a bank that skipped them), by slot.
        part_levels: Vec<BTreeMap<String, f64>>,
        /// Where measured levels are kept between loads (None: measured on every load).
        part_cache: Option<Arc<PartLevelCache>>,
        /// Off: loads skip the part-level probes (tests timing them) (`measuresPartLevels`).
        pub measures_part_levels: bool,
        /// Why sources were skipped, newest last, as [`load_failure`](Self::load_failure) words
        /// it (plus `bank full`, which Swift answers without a reason).
        pub report: Vec<String>,
        /// How many times a sampler was opened: a silent tonal load is opened once more before it
        /// is turned away, so such a source counts twice.
        pub opens: u64,
    }

    impl SamplerBank {
        /// A bank over `library` that keeps its part levels in the process's cache
        /// ([`PartLevelCache::shared`]).
        pub fn new(library: Arc<Library>, sample_rate: u32) -> SamplerBank {
            SamplerBank::with_part_cache(library, sample_rate, Some(PartLevelCache::shared()))
        }

        /// [`new`](Self::new) with the cache the part levels go through (`init(sampleRate:
        /// partLevelCache:)`; None: measured on every load).
        pub fn with_part_cache(
            library: Arc<Library>,
            sample_rate: u32,
            part_cache: Option<Arc<PartLevelCache>>,
        ) -> SamplerBank {
            SamplerBank {
                library,
                sample_rate,
                seed: 0.0,
                sources: vec![None; BANK_SLOTS],
                by_source: BTreeMap::new(),
                unloadable: BTreeSet::new(),
                failures: BTreeMap::new(),
                filled: 0,
                last_used: [0; BANK_SLOTS],
                uses: 0,
                refs: SlotRefs::new(),
                outbox: Outbox::default(),
                part_levels: vec![BTreeMap::new(); BANK_SLOTS],
                part_cache,
                measures_part_levels: true,
                report: Vec::new(),
                opens: 0,
            }
        }

        /// The rate the bank loads and measures at (`SamplerBank.sampleRate`).
        pub fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        /// The slot that plays `instrument` as `part`: the first source of its ranked
        /// fallback list that is loaded already or loads and passes its checks. None leaves
        /// the lane on its synth voice (the category keeps one, nothing fits, or every slot of
        /// a full bank is referenced).
        pub fn bind(&mut self, instrument: &str, part: &str) -> Option<usize> {
            let ranking = self
                .library
                .catalog
                .sources(instrument, part, self.seed, false)?;
            self.slot_for(&ranking)
        }

        /// `SamplerBank.slot(for:)`: the slot of the first of `sources` (already ranked) that is
        /// loaded or loads and passes its checks, loading (and level-normalising) it if needed,
        /// evicting an unreferenced slot when the bank is full, with one reference taken on it
        /// for the caller to [`release`](Self::release). None when none loads or every slot is
        /// referenced (the caller falls back to a synth voice).
        pub fn slot_for(&mut self, sources: &[SamplerSource]) -> Option<usize> {
            for s in sources {
                if let Some(&i) = self.by_source.get(s) {
                    self.acquire(i);
                    return Some(i);
                }
                if self.unloadable.contains(s) {
                    continue;
                }
                if !source_available(s) {
                    self.fail(s, "not on disk".into());
                    continue;
                }
                let Some(target) = self.free_slot() else {
                    // (Swift answers nil here without trying the sources after this one)
                    self.note(format!("{s}: bank full"));
                    return None;
                };
                // Load first, so a source that fails to load costs nothing already playing.
                let mut inst = match load_checked(s, self.sample_rate, &mut self.opens) {
                    Ok(inst) => inst,
                    Err(why) => {
                        self.unloadable.insert(s.clone());
                        self.fail(s, why);
                        continue;
                    }
                };
                if !s.is_kit() && self.measures_part_levels {
                    inst.measure_part_levels(self.part_cache.as_deref());
                }
                if target < self.filled {
                    self.evict(target);
                } else {
                    self.filled += 1;
                }
                self.sources[target] = Some(s.clone());
                self.by_source.insert(s.clone(), target);
                self.part_levels[target] = inst.part_level_db.clone();
                self.outbox.put(target, inst);
                self.acquire(target);
                return Some(target);
            }
            None
        }

        /// Why `slot_for` turned `source` away: not on disk, failed to load (with the error),
        /// silent, out of tune, or a kit without a kick, snare or hat. None if it has not been
        /// tried or is loaded now (`loadFailure(for:)`).
        pub fn load_failure(&self, source: &SamplerSource) -> Option<String> {
            if self.by_source.contains_key(source) {
                return None;
            }
            self.failures.get(source).cloned()
        }

        fn fail(&mut self, source: &SamplerSource, why: String) {
            self.note(format!("{source}: {why}"));
            self.failures.insert(source.clone(), why);
        }

        /// A line for `report`, unless it repeats the last one (a source asked for on every
        /// prepare is said once).
        fn note(&mut self, line: String) {
            if self.report.last() != Some(&line) {
                self.report.push(line);
            }
        }

        /// A reference taken on slot `i` and the slot marked used now (`acquire`).
        fn acquire(&mut self, i: usize) {
            self.refs.acquire(i);
            self.uses += 1;
            self.last_used[i] = self.uses;
        }

        /// A never-used slot, else the least recently used one nothing references (`freeSlot`).
        fn free_slot(&self) -> Option<usize> {
            if self.filled < BANK_SLOTS {
                return Some(self.filled);
            }
            let refs = self.refs.snapshot();
            let mut best: Option<usize> = None;
            for i in (0..BANK_SLOTS).filter(|i| refs[*i] == 0) {
                if best.is_none_or(|b| self.last_used[i] < self.last_used[b]) {
                    best = Some(i);
                }
            }
            best
        }

        /// Slot `i`, unreferenced, is given up (`evict`): what it played is forgotten, and a
        /// sampler loaded for it that never went to the audio thread (every track that bound it
        /// was dropped unused) goes too. The sampler that is on the audio thread is replaced by
        /// the one that is loaded for the slot next
        /// ([`SamplerSet::set_slot`](super::SamplerSet::set_slot)).
        fn evict(&mut self, i: usize) {
            if let Some(old) = self.sources[i].take() {
                self.by_source.remove(&old);
            }
            self.part_levels[i].clear();
            self.outbox.forget(i);
        }

        /// Give back a reference `slot_for` took (`release(_:)`). Off the audio thread.
        pub fn release(&self, slot: usize) {
            self.refs.release(slot);
        }

        /// The handle a project that plays slots of this bank gives its references back
        /// through, which does not wait behind a load ([`HostSlots`] holds one).
        pub fn refs(&self) -> SlotRefs {
            self.refs.clone()
        }

        /// References held on `slot` (`references(of:)`).
        pub fn references_of(&self, slot: usize) -> u32 {
            self.refs.count(slot)
        }

        /// How loud `group`'s part plays on `slot`'s instrument against its synth voice (dB,
        /// K-weighted, measured at load: [`Sf2Sampler::measure_part_levels`]). Groups without a
        /// probe of their own take the nearest ([`SamplerPartLevels::probe_group`]). None for an
        /// empty slot, a kit, or a bank that skipped the probes (`slotPartLevelDB(_:group:)`).
        pub fn slot_part_level_db(&self, slot: usize, group: &str) -> Option<f64> {
            self.part_levels
                .get(slot)?
                .get(SamplerPartLevels::probe_group(group))
                .copied()
        }

        /// Every part level measured for `slot` (by probe group; empty when none were)
        /// (`slotPartLevelDB(_:)`).
        pub fn slot_part_levels(&self, slot: usize) -> BTreeMap<String, f64> {
            self.part_levels.get(slot).cloned().unwrap_or_default()
        }

        /// What `slot` plays, for display and tests (`source(of:)`).
        pub fn source_of(&self, slot: usize) -> Option<&SamplerSource> {
            self.sources.get(slot)?.as_ref()
        }

        /// Every sampler loaded and not yet sent, for a host that plays a prepare on a core of its
        /// own (an offline render, a test): it must send them all, since the bank counts them as
        /// sent. A deck's engine sends them itself when it is handed a project; a prepare never
        /// takes them, so a track dropped unused loses none and tracks prepared side by side
        /// share them ([`Outbox`]).
        pub fn take_ready(&mut self) -> Vec<(usize, Sf2Sampler)> {
            self.outbox.take()
        }

        /// The handle the samplers waiting to be sent are taken through, which does not wait
        /// behind a load ([`Engine`](crate::host::deck::Engine) keeps one per deck).
        pub fn outbox(&self) -> Outbox {
            self.outbox.clone()
        }

        /// How many slots are numbered.
        pub fn loaded(&self) -> usize {
            self.filled
        }
    }

    /// A deck's bank as the runner asks of it while it prepares a project
    /// ([`WaveOptions::bank`](crate::wave::WaveOptions::bank)): the ranking from the library's
    /// catalogs and palettes, the numbering of the slots in the bank behind its lock, the kits
    /// among the installed banks, and the references a freed project gives back
    /// ([`SlotRefs`], which needs no lock on the bank).
    pub struct HostSlots {
        bank: Arc<Mutex<SamplerBank>>,
        refs: SlotRefs,
    }

    impl HostSlots {
        pub fn new(bank: Arc<Mutex<SamplerBank>>) -> HostSlots {
            let refs = bank.lock().unwrap_or_else(PoisonError::into_inner).refs();
            HostSlots { bank, refs }
        }

        fn library(&self) -> Option<Arc<Library>> {
            Some(self.bank.lock().ok()?.library.clone())
        }
    }

    impl SamplerSlots for HostSlots {
        fn instrument_sources(
            &self,
            instrument: &str,
            role: &str,
            seed: f64,
            picked: bool,
        ) -> Option<Vec<SamplerSource>> {
            self.library()?
                .catalog
                .sources(instrument, role, seed, picked)
        }

        fn option_sources(
            &self,
            option: &str,
            role: &str,
            seed: f64,
        ) -> Option<Vec<SamplerSource>> {
            let lib = self.library()?;
            sources_for_option(PaletteSet::shared(), option, role, seed, lib.sounds())
        }

        fn slot_for(&self, sources: &[SamplerSource]) -> Option<usize> {
            self.bank.lock().ok()?.slot_for(sources)
        }

        fn release(&self, slot: usize) {
            self.refs.release(slot);
        }

        fn kit_source(&self, id: &str) -> Option<SamplerSource> {
            let lib = self.library()?;
            let i = &lib.installed;
            drumkits::source_for(id, &i.dir, &i.files, i.gm_bank)
        }

        fn brush_kit(&self, kit: &SamplerSource) -> Option<SamplerSource> {
            let SamplerSource::Kit { path, .. } = kit else {
                return None;
            };
            let lib = self.library()?;
            drumkits::brush_kit(
                kit,
                lib.presets.get(crate::voices::last_component(path))?,
            )
        }
    }

    thread_local! {
        static BINDING: RefCell<Option<Arc<Mutex<SamplerBank>>>> = const { RefCell::new(None) };
    }

    /// Make `bank` what [`bind_thunk`] asks, on this thread, until the guard drops. The
    /// runner's `SampleBinder` is a plain `fn` pointer, so the bank comes through here.
    pub struct Binding;

    impl Binding {
        pub fn set(bank: Arc<Mutex<SamplerBank>>) -> Binding {
            BINDING.with(|b| *b.borrow_mut() = Some(bank));
            Binding
        }
    }

    impl Drop for Binding {
        fn drop(&mut self) {
            BINDING.with(|b| *b.borrow_mut() = None);
        }
    }

    /// The `WaveOptions::part_level` of a deck with a bank: what a slot measured on a part's probe
    /// (`SamplerBank.slotPartLevelDB`), asked of the bank [`Binding`] set.
    pub fn part_level_thunk(slot: usize, group: &str) -> Option<f64> {
        BINDING.with(|b| {
            let bank = b.borrow().clone()?;
            let bank = bank.lock().ok()?;
            bank.slot_part_level_db(slot, group)
        })
    }

    /// The `WaveOptions::sample_binder` of a deck with a bank.
    pub fn bind_thunk(instrument: &str, part: &str) -> Option<usize> {
        BINDING.with(|b| {
            let bank = b.borrow().clone()?;
            let mut bank = bank.lock().ok()?;
            bank.bind(instrument, part)
        })
    }
}

#[cfg(feature = "sf2")]
pub use bank::{bind_thunk, part_level_thunk, Binding, HostSlots, Outbox, SamplerBank, SlotRefs};

/// The arithmetic of `renderScratch`'s tail (`add_group`) on numbers that are exact in `f32`. The
/// synthetic SoundFont the integration tests play is mono, so the stereo width and the side
/// highpass of a single slot cannot be heard through a sampler there; these hold them to the
/// Swift source (no Swift test names them): `m = 0.5 (l + r)`, `s = 0.5 (l - r) * width`,
/// `l = m + s`, `r = m - s`, then the gain (times the duck where it ducks), then the sends as
/// the mono mean of what was added.
#[cfg(all(test, feature = "sf2"))]
mod tests {
    use super::*;

    const SR: f64 = 22_050.0;

    fn set_with(l: &[f32], r: &[f32]) -> SamplerSet {
        let mut set = SamplerSet::new(SR);
        set.group_l[..l.len()].copy_from_slice(l);
        set.group_r[..r.len()].copy_from_slice(r);
        set
    }

    #[test]
    fn a_wider_slot_scales_its_side_and_sends_the_mono_mean() {
        let mut set = set_with(&[1.0, 0.0, 0.5, -1.0], &[0.0, 1.0, 0.5, 1.0]);
        let (mut l, mut r) = ([10.0f32; 4], [20.0f32; 4]);
        set.add_group(&mut l, &mut r, 0, 4, 1.0, false, true, 0.5, 2.0, None);
        // (l, r) -> (m + s, m - s) with s doubled: (1.5, -0.5), (-0.5, 1.5), (0.5, 0.5), (-2, 2)
        assert_eq!(l, [11.5, 9.5, 10.5, 8.0]);
        assert_eq!(r, [19.5, 21.5, 20.5, 22.0]);
        assert_eq!(set.send[..4], [0.5, 0.5, 0.5, 0.0]);
        assert_eq!(set.echo[..4], [0.25, 0.25, 0.25, 0.0]);
        // at width 1 and no side highpass the pair is added as it is
        let mut set = set_with(&[1.0, 0.0, 0.5, -1.0], &[0.0, 1.0, 0.5, 1.0]);
        let (mut l, mut r) = ([0.0f32; 4], [0.0f32; 4]);
        set.add_group(&mut l, &mut r, 0, 4, 1.0, false, false, 0.0, 1.0, None);
        assert_eq!((l, r), ([1.0, 0.0, 0.5, -1.0], [0.0, 1.0, 0.5, 1.0]));
        assert_eq!(set.send[..4], [0.0; 4], "nothing sent unless asked");
        assert_eq!(set.echo[..4], [0.0; 4]);
        // and a narrower one folds toward the mid: width 0 is mono
        let mut set = set_with(&[1.0, 0.0], &[0.0, 1.0]);
        let (mut l, mut r) = ([0.0f32; 2], [0.0f32; 2]);
        set.add_group(&mut l, &mut r, 0, 2, 1.0, false, false, 0.0, 0.0, None);
        assert_eq!((l, r), ([0.5, 0.5], [0.5, 0.5]));
    }

    #[test]
    fn the_gain_and_the_duck_scale_what_is_added_and_what_is_sent() {
        let mut set = set_with(&[1.0, 0.0, 0.5, -1.0], &[0.0, 1.0, 0.5, 1.0]);
        set.duck[..4].copy_from_slice(&[1.0, 0.5, 0.25, 0.0]);
        let (mut l, mut r) = ([0.0f32; 4], [0.0f32; 4]);
        // gain 0.5, ducked: g = 0.5 * duck = 0.5, 0.25, 0.125, 0
        set.add_group(&mut l, &mut r, 0, 4, 0.5, true, true, 0.0, 1.0, None);
        assert_eq!(l, [0.5, 0.0, 0.0625, 0.0]);
        assert_eq!(r, [0.0, 0.25, 0.0625, 0.0]);
        assert_eq!(set.send[..4], [0.25, 0.125, 0.0625, 0.0]);
        // not ducking: the gain alone
        let mut set = set_with(&[1.0, 0.0, 0.5, -1.0], &[0.0, 1.0, 0.5, 1.0]);
        set.duck[..4].copy_from_slice(&[1.0, 0.5, 0.25, 0.0]);
        let (mut l, mut r) = ([0.0f32; 4], [0.0f32; 4]);
        set.add_group(&mut l, &mut r, 0, 4, 0.5, false, false, 0.0, 1.0, None);
        assert_eq!(l, [0.5, 0.0, 0.25, -0.5]);
        // a group in the middle of a block adds there and nowhere else
        let mut set = set_with(&[1.0, 1.0], &[1.0, 1.0]);
        let (mut l, mut r) = ([0.0f32; 6], [0.0f32; 6]);
        set.add_group(&mut l, &mut r, 3, 2, 1.0, false, true, 0.0, 1.0, None);
        assert_eq!(l, [0.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
        assert_eq!(set.send[..6], [0.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_sided_slot_loses_the_side_below_its_corner_and_stop_resets_the_filters() {
        // a pure side at DC (l = 1, r = -1 forever): the 300 Hz side highpass takes it away, and
        // the mid (0) is untouched; without the highpass it stays
        let n = 2048;
        let dc_l = vec![1.0f32; n];
        let dc_r = vec![-1.0f32; n];
        let mut set = set_with(&dc_l, &dc_r);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        set.add_group(&mut l, &mut r, 0, n, 1.0, false, false, 0.0, 1.0, Some(3));
        assert!(l[0].abs() > 0.1, "the first samples pass ({})", l[0]);
        assert!(
            l[n - 1].abs() < 1e-3 && r[n - 1].abs() < 1e-3,
            "the DC side is gone ({}, {})",
            l[n - 1],
            r[n - 1]
        );
        let mut set = set_with(&dc_l, &dc_r);
        let (mut l2, mut r2) = (vec![0.0f32; n], vec![0.0f32; n]);
        set.add_group(&mut l2, &mut r2, 0, n, 1.0, false, false, 0.0, 1.0, None);
        assert_eq!((l2[n - 1], r2[n - 1]), (1.0, -1.0));
        // each slot has its own filter, and `stop` (`reset_sides`) clears them all
        let first = |set: &mut SamplerSet, slot: usize| {
            set.group_l[..2].copy_from_slice(&[1.0, 1.0]);
            set.group_r[..2].copy_from_slice(&[-1.0, -1.0]);
            let (mut l, mut r) = ([0.0f32; 2], [0.0f32; 2]);
            set.add_group(
                &mut l,
                &mut r,
                0,
                2,
                1.0,
                false,
                false,
                0.0,
                1.0,
                Some(slot),
            );
            l
        };
        let mut set = SamplerSet::new(SR);
        let fresh = first(&mut set, 3);
        assert_ne!(first(&mut set, 3), fresh, "slot 3 has state now");
        assert_eq!(first(&mut set, 4), fresh, "slot 4 is another filter");
        set.reset_sides();
        assert_eq!(first(&mut set, 3), fresh, "reset");
        assert_eq!(first(&mut set, 4), fresh, "reset");
    }
}
