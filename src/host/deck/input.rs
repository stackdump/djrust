//! Input strips on the render thread (docs/HUB.md, "Audio inputs"): the gear's audio from a
//! cpal input stream, through a lock-free ring, into the mix like a deck's strip.
//!
//! Each strip: ring → DC blocker → trim → EQ / filter → fader (ramped) → master, and the cue
//! when PFL is on. The capture callback writes the channel-selected mono samples into the ring
//! ([`crate::host::audio::Input`]); the strip reads a block at a time. Clock drift between the input
//! and output devices is handled the simple way: the strip waits until the ring holds
//! [`target`](InputStrip) samples before it plays, skips ahead when the ring runs more than three
//! targets full, and plays silence (counted) when it runs dry. Both devices must run at the same
//! nominal rate (the host opens the input at the output's rate); a resampler is future work.
//! Real-time safe: every buffer is allocated up front.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use crate::fx::{StripEq, StripKnob};
use crate::history::{self, History, Mode, Scrubber};
use crate::input::{gain_of_db, DcBlock, CLIP_LEVEL, DEFAULT_TRIM_DB};
use rtrb::Consumer;

use super::engine::MAX_BLOCK;
use super::strip_recall::{RecallId, RecallMsg};

/// Main thread → an input strip.
pub enum InputMsg {
    TrimDb(f64),
    Volume(f64),
    Mute(bool),
    Pfl(bool),
    Strip(StripKnob, f64),
    /// The master tempo, for the history's window and the loop's speed.
    Tempo(f64),
    /// The history window, in beats.
    HistoryBeats(f64),
    /// The platter is touched / turned by this many frames / let go (docs/INPUT-HISTORY.md).
    Touch,
    Scrub(f64),
    Release,
    /// Back to the live signal.
    Live,
    /// Loop this many beats, ending at the head (or now).
    LoopGrab(f64),
    /// The granular freeze's knobs.
    FreezeSet(crate::fx::granular::FreezeSettings),
    /// Freeze (the last `beats`) or thaw, at render frame `at` (None: now).
    Freeze { on: bool, beats: f64, at: Option<i64> },
    /// Move the freeze position by this many frames of the frozen audio.
    FreezeScrub(f64),
}

/// What the strip recorded, for the main thread (capture to loop, docs/INPUT-HISTORY.md): the
/// same mono signal as its history ring, in relaxed atomics so a copy racing the writer never
/// reads torn memory. Frames are counted from the first one recorded; [`InputTap::copy`] stays a
/// block's margin behind the oldest frame still held.
pub struct InputTap {
    ring: Box<[AtomicU32]>,
    /// Frames written so far.
    end: AtomicI64,
}

/// How far behind the oldest held frame a copy keeps (the writer may be inside its next block).
const TAP_MARGIN: i64 = 4096;

impl InputTap {
    pub fn new(capacity: usize) -> InputTap {
        InputTap {
            ring: (0..capacity.max(2 * TAP_MARGIN as usize))
                .map(|_| AtomicU32::new(0))
                .collect(),
            end: AtomicI64::new(0),
        }
    }
    /// Render thread: append a block. Real-time safe.
    pub(crate) fn write(&self, block: &[f32]) {
        let end = self.end.load(Ordering::Relaxed);
        let cap = self.ring.len();
        for (i, x) in block.iter().enumerate() {
            self.ring[(end as usize + i) % cap].store(x.to_bits(), Ordering::Relaxed);
        }
        self.end.store(end + block.len() as i64, Ordering::Release);
    }
    /// Frames recorded so far (the frame after the newest).
    pub fn end(&self) -> i64 {
        self.end.load(Ordering::Acquire)
    }
    /// The oldest frame a copy can still read.
    pub fn oldest(&self) -> i64 {
        (self.end() - self.ring.len() as i64 + TAP_MARGIN).max(0)
    }
    /// Main thread: append frames `from..to` (as far as recorded and still held) to `out`.
    /// Returns the next frame to copy (`from` raised to the oldest held frame).
    pub fn copy(&self, from: i64, to: i64, out: &mut Vec<f32>) -> i64 {
        let (oldest, end) = (self.oldest(), self.end());
        let from = from.max(oldest);
        let to = to.min(end);
        let cap = self.ring.len();
        for f in from..to {
            out.push(f32::from_bits(self.ring[f as usize % cap].load(Ordering::Relaxed)));
        }
        to.max(from)
    }
}

/// What an input strip publishes for the main thread.
#[derive(Default)]
pub struct InputMirror {
    raw_peak: AtomicU32,
    post_peak: AtomicU32,
    /// The fader's output, for the strip's meter.
    level: AtomicU32,
    raw_clip: AtomicBool,
    post_clip: AtomicBool,
    pub underruns: AtomicU64,
    pub skips: AtomicU64,
    /// Samples in the ring at the last block.
    pub fill: AtomicU32,
    /// The capture callback has delivered samples.
    pub live: AtomicBool,
    /// The history head's [`Mode`] code.
    hist_mode: AtomicU8,
    /// How far behind live the head is, in beats (f32 bits).
    hist_lag_beats: AtomicU32,
    /// The loop's length in beats while looping (f32 bits, 0 otherwise).
    hist_loop_beats: AtomicU32,
    /// Beats of history recorded, up to the window (f32 bits).
    hist_filled_beats: AtomicU32,
    /// The strip's recording, for capture to loop (set once, when the strip is built with a
    /// history).
    pub tap: std::sync::OnceLock<InputTap>,
    /// Keep a raw copy of what the strip is fed, for [`crate::host::input_record`]: the main thread
    /// turns it on, the render thread copies each block (before the DC block and the trim)
    /// into a ring the recorder's thread drains.
    pub recording: AtomicBool,
    /// The engine frame of the first sample copied (valid once `rec_started`).
    rec_frame: AtomicI64,
    rec_started: AtomicBool,
    /// Samples copied (offered to the ring) so far.
    rec_count: AtomicU64,
    /// Samples the recorder's ring had no room for (a stalled disk).
    pub rec_dropped: AtomicU64,
    /// The read end of the recording ring, until the recorder takes it.
    rec_rx: std::sync::Mutex<Option<Consumer<f32>>>,
    /// The freeze: 0 thawed, 1 freezing on the beat, 2 frozen, 3 thawing on the beat.
    freeze_state: AtomicU8,
    /// The freeze position, 0..1 (f32 bits).
    freeze_pos: AtomicU32,
    /// Grains sounding.
    freeze_grains: AtomicU32,
    /// Beats actually frozen (f32 bits).
    freeze_beats: AtomicU32,
}

/// The recording ring's size, in samples (about 24 seconds at 44.1 kHz: a stalled disk has that
/// long before a block is dropped).
const REC_RING: usize = 1 << 20;

/// Render thread: copy a block into the recording ring while [`InputMirror::recording`] is on.
/// Real-time safe (a push on a ring made up front).
fn record_block(rec: &mut rtrb::Producer<f32>, m: &InputMirror, frame: i64, block: &[f32]) {
    if !m.recording.load(Ordering::Relaxed) {
        return;
    }
    if !m.rec_started.load(Ordering::Relaxed) {
        m.rec_frame.store(frame, Ordering::Relaxed);
        m.rec_started.store(true, Ordering::Release);
    }
    if rec.slots() < block.len() {
        m.rec_dropped.fetch_add(block.len() as u64, Ordering::Relaxed);
    } else {
        for x in block {
            let _ = rec.push(*x);
        }
    }
    m.rec_count.fetch_add(block.len() as u64, Ordering::Release);
}

fn max_into(a: &AtomicU32, v: f32) {
    let mut old = a.load(Ordering::Relaxed);
    while v > f32::from_bits(old) {
        match a.compare_exchange_weak(old, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(now) => old = now,
        }
    }
}

impl InputMirror {
    /// Peaks and clips since the last take (raw, post-trim), and reset them.
    pub fn take(&self) -> (f32, f32, bool, bool) {
        (
            f32::from_bits(self.raw_peak.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.post_peak.swap(0, Ordering::Relaxed)),
            self.raw_clip.swap(false, Ordering::Relaxed),
            self.post_clip.swap(false, Ordering::Relaxed),
        )
    }
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    /// What the history head is doing.
    pub fn history_mode(&self) -> Mode {
        Mode::of_code(self.hist_mode.load(Ordering::Relaxed))
    }
    /// Beats behind live (scrubbing or delayed), 0 otherwise.
    pub fn history_lag_beats(&self) -> f32 {
        f32::from_bits(self.hist_lag_beats.load(Ordering::Relaxed))
    }
    /// The loop's length in beats, if looping.
    pub fn history_loop_beats(&self) -> Option<f32> {
        let b = f32::from_bits(self.hist_loop_beats.load(Ordering::Relaxed));
        (b > 0.0).then_some(b)
    }
    /// Beats of history held (at most the window).
    pub fn history_filled_beats(&self) -> f32 {
        f32::from_bits(self.hist_filled_beats.load(Ordering::Relaxed))
    }
    /// The freeze: 0 thawed, 1 freezing on the beat, 2 frozen, 3 thawing on the beat.
    pub fn freeze_state(&self) -> u8 {
        self.freeze_state.load(Ordering::Relaxed)
    }
    /// Where the grains start, 0..1 of the frozen beats.
    pub fn freeze_position(&self) -> f32 {
        f32::from_bits(self.freeze_pos.load(Ordering::Relaxed))
    }
    pub fn freeze_grains(&self) -> u32 {
        self.freeze_grains.load(Ordering::Relaxed)
    }
    /// The beats held by the freeze (0 before the first).
    pub fn freeze_beats(&self) -> f32 {
        f32::from_bits(self.freeze_beats.load(Ordering::Relaxed))
    }
}

/// The history behind a strip: the ring, its read head and the window in beats.
pub(crate) struct Recall {
    hist: History,
    scrub: Scrubber,
    beats: f64,
    bpm: f64,
    sample_rate: f64,
    freeze: crate::fx::granular::GrainFreeze,
    /// A freeze or a thaw waiting for its frame: (on, beats, at).
    pending: Option<(bool, f64, i64)>,
}

pub(crate) struct InputStrip {
    rx: Consumer<f32>,
    /// The recording ring's write end (see [`InputMirror::recording`]).
    rec: rtrb::Producer<f32>,
    dc: DcBlock,
    trim: f32,
    eq: StripEq,
    volume: f64,
    mute: bool,
    pub(crate) pfl: bool,
    last_gain: f64,
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    target: usize,
    primed: bool,
    mirror: Arc<InputMirror>,
    recall: Option<Recall>,
    /// Frames the last [`pull`](Self::pull) filled, and whether it played (primed).
    pub(crate) pulled: usize,
    played: bool,
}

impl InputStrip {
    /// `history`: (the window now, the most it can grow to), in beats. None: no history.
    pub(crate) fn new(
        sample_rate: f64,
        rx: Consumer<f32>,
        mirror: Arc<InputMirror>,
        history: Option<(f64, f64)>,
    ) -> InputStrip {
        let recall = history.map(|(beats, max)| {
            let cap = history::capacity_for(max.max(beats), sample_rate);
            let _ = mirror.tap.set(InputTap::new(cap));
            Recall {
                hist: History::new(cap),
                scrub: Scrubber::new(cap),
                beats: beats.clamp(1.0, history::MAX_BEATS.max(max)),
                bpm: 120.0,
                sample_rate,
                freeze: crate::fx::granular::GrainFreeze::new(
                    sample_rate,
                    (history::frames_per_beat(history::MIN_BPM, sample_rate)
                        * crate::fx::granular::MAX_FREEZE_BEATS) as usize,
                ),
                pending: None,
            }
        });
        let (rec, rec_rx) = rtrb::RingBuffer::<f32>::new(REC_RING);
        if let Ok(mut slot) = mirror.rec_rx.lock() {
            *slot = Some(rec_rx);
        }
        InputStrip {
            rx,
            rec,
            dc: DcBlock::new(sample_rate, 20.0),
            trim: gain_of_db(DEFAULT_TRIM_DB) as f32,
            eq: StripEq::new(sample_rate),
            volume: 0.8,
            mute: false,
            pfl: false,
            last_gain: 0.0,
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            target: ((sample_rate * 0.02) as usize).max(1024),
            primed: false,
            mirror,
            recall,
            pulled: 0,
            played: false,
        }
    }

    pub(crate) fn apply(&mut self, m: InputMsg) {
        match m {
            InputMsg::TrimDb(db) => self.trim = gain_of_db(db) as f32,
            InputMsg::Volume(v) => self.volume = v.clamp(0.0, 1.0),
            InputMsg::Mute(on) => self.mute = on,
            InputMsg::Pfl(on) => self.pfl = on,
            InputMsg::Strip(k, v) => self.eq.set(k, v),
            m => {
                let Some(r) = self.recall.as_mut() else { return };
                match m {
                    InputMsg::Tempo(bpm) => r.bpm = bpm.clamp(20.0, 400.0),
                    InputMsg::HistoryBeats(b) => {
                        let cap = r.hist.capacity();
                        let most = history::window_frames(b, history::MIN_BPM, r.sample_rate, cap)
                            as f64
                            / history::frames_per_beat(history::MIN_BPM, r.sample_rate);
                        r.beats = b.clamp(1.0, most.max(1.0));
                    }
                    InputMsg::Touch => r.scrub.touch(&r.hist),
                    InputMsg::Scrub(f) => r.scrub.scrub(&r.hist, f),
                    InputMsg::Release => r.scrub.release(),
                    InputMsg::Live => r.scrub.live(),
                    InputMsg::FreezeSet(s) => r.freeze.configure(&s),
                    InputMsg::FreezeScrub(f) => r.freeze.scrub(f),
                    InputMsg::Freeze { on, beats, at } => {
                        // `at` None: the next block (its frame is not known here)
                        r.pending = Some((on, beats, at.unwrap_or(i64::MIN)));
                    }
                    InputMsg::LoopGrab(b) => {
                        if r.scrub.mode() == Mode::Loop {
                            r.scrub.regrab_loop(&r.hist, b, r.bpm, r.sample_rate);
                        } else {
                            r.scrub.grab_loop(&r.hist, b, r.bpm, r.sample_rate);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Fill `self.l` / `self.r` with the strip's next `n` frames (pre-fader) and add the faded
    /// signal into `out_l` / `out_r`. Real-time safe.
    #[allow(dead_code)]
    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.pull(out_l.len(), 0);
        self.mix(out_l, out_r, None);
    }

    /// Fill `self.l` / `self.r` with the strip's next `n` frames, pre-fader (after the trim,
    /// the history and the EQ): what the cue hears and what an insert (the vocoder) may
    /// replace before [`mix`](Self::mix). Real-time safe.
    /// `frame` is the renderer's frame count at the block's start (a quantised freeze waits
    /// for its frame).
    pub(crate) fn pull(&mut self, n: usize, frame: i64) {
        let n = n.min(MAX_BLOCK);
        self.pulled = n;
        self.played = false;
        let l = &mut self.l[..n];
        l.fill(0.0);
        let mut avail = self.rx.slots();
        self.mirror.fill.store(avail as u32, Ordering::Relaxed);
        if avail > 0 {
            self.mirror.live.store(true, Ordering::Relaxed);
        }
        if !self.primed {
            if avail < self.target {
                self.r[..n].fill(0.0);
                // the recording keeps the strip's timeline: a block it did not play is silence
                record_block(&mut self.rec, &self.mirror, frame, &self.l[..n]);
                return;
            }
            self.primed = true;
        }
        if avail > self.target * 3 {
            // the input runs ahead of the output: drop back to the target
            let skip = avail - self.target;
            if let Ok(c) = self.rx.read_chunk(skip) {
                c.commit_all();
            }
            avail = self.target;
            self.mirror.skips.fetch_add(1, Ordering::Relaxed);
        }
        let take = avail.min(n);
        if let Ok(c) = self.rx.read_chunk(take) {
            let (a, b) = c.as_slices();
            l[..a.len()].copy_from_slice(a);
            l[a.len()..a.len() + b.len()].copy_from_slice(b);
            c.commit_all();
        }
        if take < n {
            self.mirror.underruns.fetch_add(1, Ordering::Relaxed);
            self.primed = false; // build the cushion up again
        }
        record_block(&mut self.rec, &self.mirror, frame, l);
        let raw = l.iter().fold(0f32, |p, v| p.max(v.abs()));
        max_into(&self.mirror.raw_peak, raw);
        if raw >= CLIP_LEVEL {
            self.mirror.raw_clip.store(true, Ordering::Relaxed);
        }
        self.dc.process(l);
        let t = self.trim;
        for x in l.iter_mut() {
            *x *= t;
        }
        let post = l.iter().fold(0f32, |p, v| p.max(v.abs()));
        max_into(&self.mirror.post_peak, post);
        if post >= 1.0 {
            self.mirror.post_clip.store(true, Ordering::Relaxed);
        }
        if let Some(rc) = self.recall.as_mut() {
            // the history is what the strip would have played: after the DC block and the trim
            rc.hist.push(&self.l[..n]);
            if let Some(t) = self.mirror.tap.get() {
                t.write(&self.l[..n]);
            }
            let win = history::window_frames(rc.beats, rc.bpm, rc.sample_rate, rc.hist.capacity());
            rc.scrub.process(&rc.hist, win, rc.bpm, &mut self.l[..n]);
            let fpb = history::frames_per_beat(rc.bpm, rc.sample_rate);
            // the freeze on the beat: the frames frozen end on the beat's own frame, which this
            // block has just pushed into the history
            if let Some((on, beats, at)) = rc.pending {
                if at < frame + n as i64 {
                    rc.pending = None;
                    if on {
                        let late = (frame + n as i64 - at.max(frame)).clamp(0, n as i64);
                        let end = rc.hist.written() as i64 - late;
                        let frames =
                            (beats * history::frames_per_beat(rc.bpm, rc.sample_rate)) as usize;
                        if rc.freeze.freeze(&rc.hist, end, frames) {
                            let held = rc.freeze.frozen_frames() as f64
                                / history::frames_per_beat(rc.bpm, rc.sample_rate);
                            self.mirror
                                .freeze_beats
                                .store((held as f32).to_bits(), Ordering::Relaxed);
                        }
                    } else {
                        rc.freeze.thaw();
                    }
                }
            }
            rc.freeze.process(&mut self.l[..n]);
            let m = &self.mirror;
            let fs = match (rc.pending, rc.freeze.is_frozen()) {
                (Some((true, _, _)), _) => 1,
                (Some((false, _, _)), _) => 3,
                (None, true) => 2,
                (None, false) => 0,
            };
            m.freeze_state.store(fs, Ordering::Relaxed);
            m.freeze_pos
                .store((rc.freeze.position() as f32).to_bits(), Ordering::Relaxed);
            m.freeze_grains
                .store(rc.freeze.grains() as u32, Ordering::Relaxed);
            m.hist_mode.store(rc.scrub.mode().code(), Ordering::Relaxed);
            let lag = (rc.scrub.lag_frames(&rc.hist) / fpb) as f32;
            m.hist_lag_beats.store(lag.to_bits(), Ordering::Relaxed);
            let lb = rc.scrub.loop_beats().unwrap_or(0.0) as f32;
            m.hist_loop_beats.store(lb.to_bits(), Ordering::Relaxed);
            let held = (rc.hist.written().min(win as u64) as f64 / fpb) as f32;
            m.hist_filled_beats.store(held.to_bits(), Ordering::Relaxed);
        }
        let r = &mut self.r[..n];
        r.copy_from_slice(&self.l[..n]);
        self.eq.process(&mut self.l[..n], &mut self.r[..n]);
        self.played = true;
    }

    /// Add the pulled block, through the fader (ramped), into `out_l` / `out_r`. Nothing until
    /// the ring has its cushion.
    pub(crate) fn mix(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        aux: Option<(&mut [f32], &mut [f32], (f64, f64))>,
    ) {
        if !self.played {
            return;
        }
        let mut aux = aux;
        let n = self.pulled.min(out_l.len()).min(out_r.len());
        let target = if self.mute { 0.0 } else { self.volume };
        let (g0, step) = (self.last_gain, (target - self.last_gain) / n.max(1) as f64);
        self.last_gain = target;
        let mut peak = 0f32;
        for i in 0..n {
            let g = (g0 + step * (i + 1) as f64) as f32;
            let (a, b) = (g * self.l[i], g * self.r[i]);
            out_l[i] += a;
            out_r[i] += b;
            peak = peak.max(a.abs()).max(b.abs());
            // the aux send is post-fader, ramped over the block
            if let Some((al, ar, (s0, s1))) = aux.as_mut() {
                let s = *s0 + (*s1 - *s0) * (i + 1) as f64 / n as f64;
                al[i] += (a as f64 * s) as f32;
                ar[i] += (b as f64 * s) as f32;
            }
        }
        self.mirror.level.store(peak.to_bits(), Ordering::Relaxed);
    }
}

// MARK: the main thread's half

use crate::input::{InputChannel, InputSettings, InputView, InputWatch};
use crate::json::JsonValue;

use super::engine::{Engine, Msg};

/// What the main thread keeps about an input strip.
pub struct MainInput {
    pub settings: InputSettings,
    pub mirror: Arc<InputMirror>,
    /// The ring's write end, until a capture stream takes it ([`Engine::take_input_feed`]).
    pub(crate) feed: Option<rtrb::Producer<f32>>,
    /// The channel selection the capture callback reads (0 L, 1 R, 2 L+R).
    pub channel: Arc<AtomicU8>,
    pub watch: InputWatch,
    /// The device feeding it, once one is attached.
    pub device: Option<String>,
    /// The history window now, in beats (None: this strip keeps no history).
    pub history_beats: Option<f64>,
    /// The loop length the LOOP button grabs, in beats.
    pub loop_beats: f64,
    /// How late this input's audio reaches the app, ms (the interface's and the gear's own
    /// latency). Captures read this much later in the recording so a loop lands on the beat.
    pub latency_ms: f64,
    /// The tempo last sent to the strip.
    pub(crate) last_tempo: f64,
    /// The granular freeze's knobs (docs/INPUT-HISTORY.md, "Freeze").
    pub freeze: crate::fx::granular::FreezeSettings,
    /// A freeze is asked for (it may still be waiting for its beat).
    pub frozen: bool,
    /// The WAV [`crate::host::input_record`] writes this strip's audio to, while it does.
    pub record_file: Option<String>,
}

impl MainInput {
    pub(crate) fn new(
        label: &str,
        feed: rtrb::Producer<f32>,
        mirror: Arc<InputMirror>,
        history_beats: Option<f64>,
    ) -> MainInput {
        MainInput {
            settings: InputSettings::new(label),
            mirror,
            feed: Some(feed),
            channel: Arc::new(AtomicU8::new(0)),
            watch: InputWatch::default(),
            device: None,
            history_beats,
            loop_beats: history::DEFAULT_LOOP_BEATS,
            latency_ms: 0.0,
            last_tempo: 0.0,
            freeze: Default::default(),
            frozen: false,
            record_file: None,
        }
    }
}

/// The capture callback's channel code for a selection.
pub fn channel_code(c: InputChannel) -> u8 {
    match c {
        InputChannel::Left => 0,
        InputChannel::Right => 1,
        InputChannel::Sum => 2,
    }
}

/// The selection a code stands for.
pub fn channel_of(code: u8) -> InputChannel {
    match code {
        1 => InputChannel::Right,
        2 => InputChannel::Sum,
        _ => InputChannel::Left,
    }
}

impl Engine {
    /// The read end of input `i`'s recording ring, once: the recorder's thread keeps it.
    pub fn take_input_record_rx(&mut self, i: usize) -> Option<Consumer<f32>> {
        self.inputs.get(i)?.mirror.rec_rx.lock().ok()?.take()
    }

    /// Start (or stop) the render thread copying input `i`'s raw feed into its recording ring.
    pub fn set_input_recording(&self, i: usize, on: bool) -> bool {
        match self.inputs.get(i) {
            Some(x) => {
                x.mirror.recording.store(on, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// The engine frame of the first sample copied for input `i`'s recording (None: not yet).
    pub fn input_record_frame(&self, i: usize) -> Option<i64> {
        let m = &self.inputs.get(i)?.mirror;
        m.rec_started
            .load(Ordering::Acquire)
            .then(|| m.rec_frame.load(Ordering::Relaxed))
    }

    /// Samples copied so far for input `i`'s recording, and samples its ring had no room for.
    pub fn input_record_counts(&self, i: usize) -> (u64, u64) {
        self.inputs.get(i).map_or((0, 0), |x| {
            (
                x.mirror.rec_count.load(Ordering::Acquire),
                x.mirror.rec_dropped.load(Ordering::Relaxed),
            )
        })
    }

    /// The input strip labelled `label`.
    pub fn input_index(&self, label: &str) -> Option<usize> {
        self.inputs.iter().position(|i| i.settings.label == label)
    }

    /// Hand input `i`'s ring to a capture stream (once): the samples it pushes play on the strip.
    pub fn take_input_feed(&mut self, i: usize) -> Option<(rtrb::Producer<f32>, Arc<AtomicU8>)> {
        let inp = self.inputs.get_mut(i)?;
        let feed = inp.feed.take()?;
        Some((feed, inp.channel.clone()))
    }

    pub(crate) fn input_send(&mut self, i: usize, m: InputMsg) -> bool {
        i < self.inputs.len() && self.send_msg(Msg::Input(i, m))
    }

    /// Set input `i`'s latency offset for captures, ms (-200..500; negative: it arrives early).
    pub fn set_input_latency(&mut self, i: usize, ms: f64) -> bool {
        match self.inputs.get_mut(i) {
            Some(inp) if ms.is_finite() => {
                inp.latency_ms = ms.clamp(-200.0, 500.0);
                let v = inp.latency_ms;
                self.input_trace(i, "latency_ms", format!("latency {v:.0} ms"), true, JsonValue::Number(v));
                true
            }
            _ => false,
        }
    }
    /// The latency offset of input `i` in frames.
    pub fn input_latency_frames(&self, i: usize) -> i64 {
        self.inputs
            .get(i)
            .map_or(0, |x| (x.latency_ms / 1000.0 * self.sample_rate).round() as i64)
    }

    pub fn set_input_trim(&mut self, i: usize, db: f64) -> bool {
        let (lo, hi) = crate::input::TRIM_RANGE_DB;
        let db = db.clamp(lo, hi);
        match self.inputs.get_mut(i) {
            Some(inp) => inp.settings.trim_db = db,
            None => return false,
        }
        self.input_trace(i, "trim_db", format!("trim {db:+.1} dB"), true, JsonValue::Number(db));
        self.input_send(i, InputMsg::TrimDb(db))
    }
    pub fn set_input_volume(&mut self, i: usize, v: f64) -> bool {
        let v = v.clamp(0.0, 1.0);
        match self.inputs.get_mut(i) {
            Some(inp) => inp.settings.volume = v,
            None => return false,
        }
        self.input_trace(i, "volume", format!("volume {v:.2}"), true, JsonValue::Number(v));
        self.input_send(i, InputMsg::Volume(v))
    }
    pub fn set_input_mute(&mut self, i: usize, on: bool) -> bool {
        match self.inputs.get_mut(i) {
            Some(inp) => inp.settings.mute = on,
            None => return false,
        }
        let words = if on { "muted" } else { "unmuted" };
        self.input_trace(i, "mute", words.into(), false, on.into());
        self.input_send(i, InputMsg::Mute(on))
    }
    pub fn set_input_pfl(&mut self, i: usize, on: bool) -> bool {
        match self.inputs.get_mut(i) {
            Some(inp) => inp.settings.pfl = on,
            None => return false,
        }
        self.input_trace(i, "pfl", format!("cue (PFL) {}", if on { "on" } else { "off" }), false, on.into());
        self.input_send(i, InputMsg::Pfl(on))
    }
    pub fn set_input_strip(&mut self, i: usize, knob: StripKnob, v: f64) -> bool {
        let v = v.clamp(0.0, 1.0);
        match self.inputs.get_mut(i) {
            Some(inp) => {
                let s = &mut inp.settings.eq;
                match knob {
                    StripKnob::Filter => s.filter = v,
                    StripKnob::EqLow => s.eq_low = v,
                    StripKnob::EqMid => s.eq_mid = v,
                    StripKnob::EqHigh => s.eq_high = v,
                }
            }
            None => return false,
        }
        let name = match knob {
            StripKnob::Filter => "filter",
            StripKnob::EqLow => "low",
            StripKnob::EqMid => "mid",
            StripKnob::EqHigh => "high",
        };
        self.input_trace(i, name, format!("{name} {v:.2}"), true, JsonValue::Number(v));
        self.input_send(i, InputMsg::Strip(knob, v))
    }

    /// An input strip's knob in the action trace: words, and the `input` live op that sets it
    /// again (`{"op": "input", "input": LABEL, key: value}`), so an arrangement keeps it.
    fn input_trace(&mut self, i: usize, key: &str, text: String, fold: bool, value: JsonValue) {
        if !self.tracing() {
            return;
        }
        let label = self.inputs[i].settings.label.clone();
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(super::trace::Traced {
            entry: crate::control::action_log::Entry {
                source: via,
                key: format!("input:{label}:{key}"),
                text: format!("{label} {text}"),
                step: None,
                fold,
            },
            ops: vec![crate::session::arrangement::ArrangeOp::Live {
                method: "input".into(),
                params: super::vocoder::live_params([
                    ("input", JsonValue::str(&label)),
                    (key, value),
                ]),
            }],
        });
    }

    /// Each input strip's knobs that differ from a fresh strip's, as `input` live ops (an
    /// arrangement's `start.setup`).
    pub fn input_setup_ops(&self) -> Vec<crate::session::arrangement::ArrangeOp> {
        let mut out = Vec::new();
        for inp in &self.inputs {
            let s = &inp.settings;
            let fresh = InputSettings::new(&s.label);
            let mut o = crate::json::JsonObject::from_pairs([("input", JsonValue::str(&s.label))]);
            if s.trim_db != fresh.trim_db {
                o.set("trim_db", JsonValue::Number(s.trim_db));
            }
            if s.volume != fresh.volume {
                o.set("volume", JsonValue::Number(s.volume));
            }
            if s.mute != fresh.mute {
                o.set("mute", s.mute.into());
            }
            if s.pfl != fresh.pfl {
                o.set("pfl", s.pfl.into());
            }
            if s.channel != fresh.channel {
                o.set(
                    "channel",
                    JsonValue::str(match s.channel {
                        InputChannel::Left => "L",
                        InputChannel::Right => "R",
                        InputChannel::Sum => "LR",
                    }),
                );
            }
            if inp.latency_ms != 0.0 {
                o.set("latency_ms", JsonValue::Number(inp.latency_ms));
            }
            for (k, v, f) in [
                ("filter", s.eq.filter, fresh.eq.filter),
                ("low", s.eq.eq_low, fresh.eq.eq_low),
                ("mid", s.eq.eq_mid, fresh.eq.eq_mid),
                ("high", s.eq.eq_high, fresh.eq.eq_high),
            ] {
                if v != f {
                    o.set(k, JsonValue::Number(v));
                }
            }
            if o.len() > 1 {
                out.push(crate::session::arrangement::ArrangeOp::Live {
                    method: "input".into(),
                    params: JsonValue::Object(o),
                });
            }
        }
        out
    }
    /// Which channel of the device feeds the strip (read by the capture callback directly).
    pub fn set_input_channel(&mut self, i: usize, c: InputChannel) -> bool {
        let Some(inp) = self.inputs.get_mut(i) else {
            return false;
        };
        inp.settings.channel = c;
        inp.channel.store(channel_code(c), Ordering::Relaxed);
        let name = match c {
            InputChannel::Left => "L",
            InputChannel::Right => "R",
            InputChannel::Sum => "LR",
        };
        self.input_trace(i, "channel", format!("channel {name}"), false, JsonValue::str(name));
        true
    }

    // MARK: history (docs/INPUT-HISTORY.md)

    /// Resize input `i`'s history window, in beats.
    pub fn set_input_history_beats(&mut self, i: usize, beats: f64) -> bool {
        let Some(inp) = self.inputs.get_mut(i) else {
            return false;
        };
        if inp.history_beats.is_none() {
            return false;
        }
        let b = beats.clamp(1.0, history::MAX_BEATS);
        inp.history_beats = Some(b);
        self.note_history(
            RecallId::Input(i),
            "beats",
            Some(("beats", crate::json::JsonValue::Number(b))),
            format!("keeps {b} beats"),
            false,
        );
        self.input_send(i, InputMsg::HistoryBeats(b))
    }
    /// The platter on strip `i` (an input, or any strip with a history) is touched: its live
    /// signal gives way to the history.
    pub fn input_touch(&mut self, i: impl Into<RecallId>) -> bool {
        let i = i.into();
        if self.is_frozen(i) {
            return true; // the wheel moves the freeze position; touching holds nothing
        }
        self.has_history(i) && self.recall_send(i, RecallMsg::Touch)
    }
    /// The platter turned by `seconds` of platter time (negative: back in time).
    pub fn input_scrub(&mut self, i: impl Into<RecallId>, seconds: f64) -> bool {
        let i = i.into();
        let sr = self.sample_rate;
        if self.is_frozen(i) {
            return self.freeze_scrub(i, seconds * sr);
        }
        self.has_history(i) && self.recall_send(i, RecallMsg::Scrub(seconds * sr))
    }
    /// Scrub by whole or fractional beats at the master tempo (negative: back).
    pub fn input_scrub_beats(&mut self, i: impl Into<RecallId>, beats: f64) -> bool {
        let i = i.into();
        let (_, bpm) = self.master_tempo();
        let ok = self.input_scrub(i, beats * 60.0 / bpm);
        if ok {
            self.note_history(
                i,
                "scrub",
                Some(("beats", crate::json::JsonValue::Number(beats))),
                format!("scrub {beats:+.2} beats"),
                true,
            );
        }
        ok
    }
    /// The platter is let go: the history plays on, a delay of where it was left.
    pub fn input_release(&mut self, i: impl Into<RecallId>) -> bool {
        let i = i.into();
        if self.is_frozen(i) {
            return true;
        }
        self.has_history(i) && self.recall_send(i, RecallMsg::Release)
    }
    /// Back to the live signal.
    pub fn input_live(&mut self, i: impl Into<RecallId>) -> bool {
        let i = i.into();
        let ok = self.has_history(i) && self.recall_send(i, RecallMsg::Live);
        if ok {
            self.note_history(i, "live", None, "back to live".into(), false);
        }
        ok
    }
    /// Loop the last `beats` (or the beats ending at the head if it is scrubbed back).
    pub fn input_loop(&mut self, i: impl Into<RecallId>, beats: f64) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        self.set_recall_loop_beats(i, beats);
        self.sync_history_tempo();
        self.note_history(
            i,
            "loop",
            Some(("beats", crate::json::JsonValue::Number(beats))),
            format!("loop {beats} beats"),
            false,
        );
        self.recall_send(i, RecallMsg::LoopGrab(beats))
    }
    /// Halve (`dir < 0`) or double the loop length; re-grabs if looping.
    pub fn input_loop_step(&mut self, i: impl Into<RecallId>, dir: i32) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        let b = history::step_loop_beats(self.recall_loop_beats(i), dir);
        self.set_recall_loop_beats(i, b);
        self.note_history(
            i,
            "loop_step",
            Some(("dir", crate::json::JsonValue::int(dir.signum() as i64))),
            format!("loop {} → {b} beats", if dir < 0 { "halved" } else { "doubled" }),
            false,
        );
        if self.recall_view(i).is_some_and(|v| v.history_mode() == Mode::Loop) {
            return self.recall_send(i, RecallMsg::LoopGrab(b));
        }
        true
    }

    /// Snap the history the deck's jog is aimed at back to live (beyond Swift: `historySnap`):
    /// the head to now, wherever the wheel left it. With no deck, every aimed history. True if
    /// one was snapped.
    pub fn history_snap(&mut self, deck: Option<&str>) -> bool {
        let Some((d, id)) = self.history_jog.clone() else {
            return false;
        };
        if deck.is_some_and(|x| !x.eq_ignore_ascii_case(&d)) {
            return false;
        }
        let ok = self.quietly(|e| e.input_live(id));
        if ok {
            let label = self.recall_label(id);
            // the `live` op of the history it was aimed at makes the same move again
            let op = crate::session::arrangement::ArrangeOp::Live {
                method: "history".into(),
                params: super::vocoder::live_params([
                    ("strip", label.clone().into()),
                    ("action", "live".into()),
                ]),
            };
            self.trace_push_text_ops(&format!("history {label} snapped to live ({d}'s jog)"), vec![op]);
        }
        ok
    }

    /// Aim a deck's jog wheel at a strip's history (an input, another deck, `L`, `V`, `M`; None:
    /// the wheel drives its deck again). The wheel follows the deck (`jog_target`): it goes to
    /// the history while the deck is empty or SHIFT / a `historyJog` control is held, and then
    /// that deck's CUE is back to live, LOOP IN grabs a loop of the strip's loop length, LOOP OUT
    /// ends it, and HALVE / DOUBLE change its length; frozen, the wheel moves the freeze
    /// position. With a track or loop on the deck they are the deck's own.
    pub fn set_history_jog(&mut self, aim: Option<(&str, RecallId)>) {
        self.history_jog = aim
            .filter(|(d, i)| self.index(d).is_some() && self.has_history(*i))
            .map(|(d, i)| (d.to_string(), i));
        let aimed = self.history_jog.clone();
        let (deck, strip) = match &aimed {
            Some((d, i)) => (d.clone(), self.recall_label(*i)),
            None => (String::new(), String::new()),
        };
        if self.tracing() {
            self.note_resolved(
                "history:aim".into(),
                if aimed.is_some() {
                    format!("{deck}'s jog aimed at {strip}")
                } else {
                    "jog back on its deck".into()
                },
                false,
                "history",
                super::vocoder::live_params([
                    ("action", "jog".into()),
                    ("deck", if deck.is_empty() { "off".into() } else { deck.into() }),
                    ("strip", strip.into()),
                ]),
            );
        }
        self.note_jog_targets();
    }

    /// Tell every history strip the master tempo (call about every 10 ms; sends on change).
    pub fn sync_history_tempo(&mut self) {
        self.sync_vocoder_tempo();
        self.sync_trick_tempo();
        if !self.inputs.iter().any(|x| x.history_beats.is_some()) && self.strip_recalls.is_empty() {
            return;
        }
        let (_, bpm) = self.master_tempo();
        self.sync_strip_recall_tempo(bpm);
        for i in 0..self.inputs.len() {
            if self.inputs[i].history_beats.is_some()
                && (self.inputs[i].last_tempo - bpm).abs() > 0.005
            {
                self.inputs[i].last_tempo = bpm;
                self.input_send(i, InputMsg::Tempo(bpm));
            }
        }
    }

    /// Each input's lamp and hint now, from what its strip saw since the last call (`now` in
    /// seconds on any monotonic clock). Call once a frame.
    pub fn watch_inputs(&mut self, now: f64) -> Vec<InputView> {
        self.inputs
            .iter_mut()
            .map(|inp| {
                let (raw, post, rc, pc) = inp.mirror.take();
                inp.watch.update(now, rc, pc, raw, post)
            })
            .collect()
    }
}
