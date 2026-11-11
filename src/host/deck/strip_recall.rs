//! A history and a granular freeze on every strip (beyond Swift; docs/INPUT-HISTORY.md, "Any
//! strip"): the decks, the loop row `L`, the vocoder `V` and the master `M` keep a rolling
//! stereo history of the same length as the input strips', so freeze (FRZ, `freeze` /
//! `freezeKnob` bindings), the jog aimed at a history (scrub, release, back to live), and the
//! history loop work on any of them the way they work on an input.
//!
//! [`RecallId`] names a strip with a history: an input (its own mono history, `input.rs`) or one
//! of these ([`StripId`]). The engine's history and freeze calls take either; a name resolves
//! with [`Engine::recall_id`].
//!
//! Where each strip's history is taken (and its freeze heard): a deck after its EQ and tricks,
//! before its fader (what its cue and the vocoder hear); `L` after the loop channel's fader; `V`
//! before its fader (on its own strip; as an insert the carrier's strip is the one to freeze);
//! `M` after the master fader.
//!
//! The buffers are made once, when the engine is built ([`EngineConfig::strip_history`]), and
//! nothing on the render thread allocates.
//!
//! [`EngineConfig::strip_history`]: super::engine::EngineConfig::strip_history

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use crate::fx::granular::{FreezeSettings, GrainFreeze, MAX_FREEZE_BEATS};
use crate::history::{self, History, Mode, Scrubber};

/// A strip that is not an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StripId {
    Deck(usize),
    /// The loop row's channel, `L`.
    Loops,
    /// The vocoder strip, `V`.
    Vocoder,
    /// The master, `M`.
    Master,
}

/// A strip with a history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecallId {
    Input(usize),
    Strip(StripId),
}

impl From<usize> for RecallId {
    fn from(i: usize) -> RecallId {
        RecallId::Input(i)
    }
}

impl From<StripId> for RecallId {
    fn from(s: StripId) -> RecallId {
        RecallId::Strip(s)
    }
}

/// Main thread → a strip's history and freeze (the input strips' are [`super::input::InputMsg`]).
#[derive(Clone, Copy, Debug)]
pub enum RecallMsg {
    Tempo(f64),
    HistoryBeats(f64),
    Touch,
    Scrub(f64),
    Release,
    Live,
    LoopGrab(f64),
    FreezeSet(FreezeSettings),
    Freeze { on: bool, beats: f64, at: Option<i64> },
    FreezeScrub(f64),
}

/// What a strip's history and freeze look like now, whoever's they are.
pub trait RecallView {
    /// 0 thawed, 1 freezing on the beat, 2 frozen, 3 thawing on the beat.
    fn freeze_state(&self) -> u8;
    fn freeze_position(&self) -> f32;
    fn freeze_grains(&self) -> u32;
    fn freeze_beats(&self) -> f32;
    fn history_mode(&self) -> Mode;
    fn history_lag_beats(&self) -> f32;
    fn history_loop_beats(&self) -> f32;
    fn history_filled_beats(&self) -> f32;
}

/// What a strip's history publishes for the main thread.
#[derive(Default)]
pub struct RecallMirror {
    freeze_state: AtomicU8,
    freeze_pos: AtomicU32,
    freeze_grains: AtomicU32,
    freeze_beats: AtomicU32,
    hist_mode: AtomicU8,
    hist_lag_beats: AtomicU32,
    hist_loop_beats: AtomicU32,
    hist_filled_beats: AtomicU32,
}

fn f(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

impl RecallView for RecallMirror {
    fn freeze_state(&self) -> u8 {
        self.freeze_state.load(Ordering::Relaxed)
    }
    fn freeze_position(&self) -> f32 {
        f(&self.freeze_pos)
    }
    fn freeze_grains(&self) -> u32 {
        self.freeze_grains.load(Ordering::Relaxed)
    }
    fn freeze_beats(&self) -> f32 {
        f(&self.freeze_beats)
    }
    fn history_mode(&self) -> Mode {
        Mode::of_code(self.hist_mode.load(Ordering::Relaxed))
    }
    fn history_lag_beats(&self) -> f32 {
        f(&self.hist_lag_beats)
    }
    fn history_loop_beats(&self) -> f32 {
        f(&self.hist_loop_beats)
    }
    fn history_filled_beats(&self) -> f32 {
        f(&self.hist_filled_beats)
    }
}

/// The render thread's half: a stereo history, its read heads, the freeze.
pub(crate) struct StripRecall {
    hist: [History; 2],
    scrub: [Scrubber; 2],
    beats: f64,
    bpm: f64,
    sample_rate: f64,
    freeze: GrainFreeze,
    pending: Option<(bool, f64, i64)>,
    mirror: Arc<RecallMirror>,
}

impl StripRecall {
    /// A history of `beats` (at the slowest tempo the ring is sized for) and a freeze of up to
    /// [`MAX_FREEZE_BEATS`]. Everything is allocated here, once.
    pub(crate) fn new(sample_rate: f64, beats: f64, mirror: Arc<RecallMirror>) -> StripRecall {
        let cap = history::capacity_for(beats, sample_rate);
        // a loop is at most the window long
        StripRecall {
            hist: [History::new(cap), History::new(cap)],
            scrub: [Scrubber::new(cap), Scrubber::new(cap)],
            beats,
            bpm: 120.0,
            sample_rate,
            freeze: GrainFreeze::new_stereo(
                sample_rate,
                (history::frames_per_beat(history::MIN_BPM, sample_rate) * MAX_FREEZE_BEATS)
                    as usize,
            ),
            pending: None,
            mirror,
        }
    }

    pub(crate) fn apply(&mut self, m: RecallMsg) {
        match m {
            RecallMsg::Tempo(bpm) => self.bpm = bpm.clamp(20.0, 400.0),
            RecallMsg::HistoryBeats(b) => {
                let cap = self.hist[0].capacity();
                let most = history::window_frames(b, history::MIN_BPM, self.sample_rate, cap) as f64
                    / history::frames_per_beat(history::MIN_BPM, self.sample_rate);
                self.beats = b.clamp(1.0, most.max(1.0));
            }
            RecallMsg::Touch => {
                for c in 0..2 {
                    self.scrub[c].touch(&self.hist[c]);
                }
            }
            RecallMsg::Scrub(fr) => {
                for c in 0..2 {
                    self.scrub[c].scrub(&self.hist[c], fr);
                }
            }
            RecallMsg::Release => self.scrub.iter_mut().for_each(|s| s.release()),
            RecallMsg::Live => self.scrub.iter_mut().for_each(|s| s.live()),
            RecallMsg::LoopGrab(b) => {
                for c in 0..2 {
                    let s = &mut self.scrub[c];
                    if s.mode() == Mode::Loop {
                        s.regrab_loop(&self.hist[c], b, self.bpm, self.sample_rate);
                    } else {
                        s.grab_loop(&self.hist[c], b, self.bpm, self.sample_rate);
                    }
                }
            }
            RecallMsg::FreezeSet(s) => self.freeze.configure(&s),
            RecallMsg::FreezeScrub(fr) => self.freeze.scrub(fr),
            RecallMsg::Freeze { on, beats, at } => {
                self.pending = Some((on, beats, at.unwrap_or(i64::MIN)));
            }
        }
    }

    /// Take the strip's block into the history, then play the history's head and the freeze
    /// over it (both pass the block through while live and thawed). `frame`: the renderer's
    /// frame at the block's start. Real-time safe.
    pub(crate) fn process(&mut self, l: &mut [f32], r: &mut [f32], frame: i64) {
        let n = l.len().min(r.len());
        self.hist[0].push(&l[..n]);
        self.hist[1].push(&r[..n]);
        let win = history::window_frames(self.beats, self.bpm, self.sample_rate, self.hist[0].capacity());
        self.scrub[0].process(&self.hist[0], win, self.bpm, &mut l[..n]);
        self.scrub[1].process(&self.hist[1], win, self.bpm, &mut r[..n]);
        let fpb = history::frames_per_beat(self.bpm, self.sample_rate);
        if let Some((on, beats, at)) = self.pending {
            if at < frame + n as i64 {
                self.pending = None;
                if on {
                    let late = (frame + n as i64 - at.max(frame)).clamp(0, n as i64);
                    let end = self.hist[0].written() as i64 - late;
                    let frames = (beats * fpb) as usize;
                    if self.freeze.freeze_stereo(&self.hist[0], &self.hist[1], end, frames) {
                        let held = self.freeze.frozen_frames() as f64 / fpb;
                        self.mirror
                            .freeze_beats
                            .store((held as f32).to_bits(), Ordering::Relaxed);
                    }
                } else {
                    self.freeze.thaw();
                }
            }
        }
        self.freeze.process_stereo(&mut l[..n], &mut r[..n]);
        let m = &self.mirror;
        let fs = match (self.pending, self.freeze.is_frozen()) {
            (Some((true, _, _)), _) => 1,
            (Some((false, _, _)), _) => 3,
            (None, true) => 2,
            (None, false) => 0,
        };
        m.freeze_state.store(fs, Ordering::Relaxed);
        m.freeze_pos
            .store((self.freeze.position() as f32).to_bits(), Ordering::Relaxed);
        m.freeze_grains
            .store(self.freeze.grains() as u32, Ordering::Relaxed);
        m.hist_mode.store(self.scrub[0].mode().code(), Ordering::Relaxed);
        let lag = (self.scrub[0].lag_frames(&self.hist[0]) / fpb) as f32;
        m.hist_lag_beats.store(lag.to_bits(), Ordering::Relaxed);
        let lb = self.scrub[0].loop_beats().unwrap_or(0.0) as f32;
        m.hist_loop_beats.store(lb.to_bits(), Ordering::Relaxed);
        let held = (self.hist[0].written().min(win as u64) as f64 / fpb) as f32;
        m.hist_filled_beats.store(held.to_bits(), Ordering::Relaxed);
    }
}

/// The main thread's half of a strip's history.
pub struct MainRecall {
    pub id: StripId,
    pub freeze: FreezeSettings,
    /// A freeze is asked for (it may still be waiting for its beat).
    pub frozen: bool,
    pub history_beats: f64,
    pub loop_beats: f64,
    pub(crate) last_tempo: f64,
    pub mirror: Arc<RecallMirror>,
}

impl MainRecall {
    pub(crate) fn new(id: StripId, beats: f64, mirror: Arc<RecallMirror>) -> MainRecall {
        MainRecall {
            id,
            freeze: FreezeSettings::default(),
            frozen: false,
            history_beats: beats,
            loop_beats: history::DEFAULT_LOOP_BEATS,
            last_tempo: 0.0,
            mirror,
        }
    }
}

/// The strips a history goes on, in their slot order: the decks, then `L`, `V`, `M`.
pub(crate) fn strip_ids(decks: usize) -> Vec<StripId> {
    let mut v: Vec<StripId> = (0..decks).map(StripId::Deck).collect();
    v.extend([StripId::Loops, StripId::Vocoder, StripId::Master]);
    v
}

/// A strip's slot in that order.
pub(crate) fn slot_of(id: StripId, decks: usize) -> usize {
    match id {
        StripId::Deck(i) => i,
        StripId::Loops => decks,
        StripId::Vocoder => decks + 1,
        StripId::Master => decks + 2,
    }
}

impl RecallView for super::input::InputMirror {
    fn freeze_state(&self) -> u8 {
        super::input::InputMirror::freeze_state(self)
    }
    fn freeze_position(&self) -> f32 {
        super::input::InputMirror::freeze_position(self)
    }
    fn freeze_grains(&self) -> u32 {
        super::input::InputMirror::freeze_grains(self)
    }
    fn freeze_beats(&self) -> f32 {
        super::input::InputMirror::freeze_beats(self)
    }
    fn history_mode(&self) -> Mode {
        super::input::InputMirror::history_mode(self)
    }
    fn history_lag_beats(&self) -> f32 {
        super::input::InputMirror::history_lag_beats(self)
    }
    fn history_loop_beats(&self) -> f32 {
        super::input::InputMirror::history_loop_beats(self).unwrap_or(0.0)
    }
    fn history_filled_beats(&self) -> f32 {
        super::input::InputMirror::history_filled_beats(self)
    }
}

// MARK: the main thread: one way in for every strip with a history

use super::engine::{Engine, Msg};
use super::input::InputMsg;

impl Engine {
    /// The strip a name means: an input's label (exact, else ignoring case), a deck ("A",
    /// "B"), `L` / `loops`, `V` / `vocoder`, `M` / `master`. None when it has no history.
    pub fn recall_id(&self, name: &str) -> Option<RecallId> {
        let n = name.trim();
        let id = if let Some(i) = self.input_index(n).or_else(|| {
            self.inputs
                .iter()
                .position(|x| x.settings.label.eq_ignore_ascii_case(n))
        }) {
            RecallId::Input(i)
        } else if let Some(i) = self.decks.iter().position(|d| d.name.eq_ignore_ascii_case(n)) {
            RecallId::Strip(StripId::Deck(i))
        } else {
            match n.to_ascii_lowercase().as_str() {
                "l" | "loops" | "loop" => RecallId::Strip(StripId::Loops),
                "v" | "vocoder" => RecallId::Strip(StripId::Vocoder),
                "m" | "master" | "main" => RecallId::Strip(StripId::Master),
                _ => return None,
            }
        };
        self.has_history(id).then_some(id)
    }

    /// Its name: the input's label, the deck's, `L`, `V`, `M`.
    pub fn recall_label(&self, id: RecallId) -> String {
        match id {
            RecallId::Input(i) => self
                .inputs
                .get(i)
                .map_or_else(|| format!("input {i}"), |x| x.settings.label.clone()),
            RecallId::Strip(StripId::Deck(i)) => self
                .decks
                .get(i)
                .map_or_else(|| format!("deck {i}"), |d| d.name.clone()),
            RecallId::Strip(StripId::Loops) => "L".into(),
            RecallId::Strip(StripId::Vocoder) => "V".into(),
            RecallId::Strip(StripId::Master) => "M".into(),
        }
    }

    /// The strip a control means: `name` if given (any strip with a history), else the one a
    /// deck's jog is aimed at, else the first input with a history, else none.
    pub fn recall_target(&self, name: Option<&str>) -> Option<RecallId> {
        if let Some(n) = name {
            return self.recall_id(n);
        }
        if let Some((_, i)) = &self.history_jog {
            if self.has_history(*i) {
                return Some(*i);
            }
        }
        (0..self.inputs.len())
            .find(|i| self.has_history(*i))
            .map(RecallId::Input)
    }

    /// Every strip with a history: the inputs', then the decks, `L`, `V`, `M`.
    pub fn recall_ids(&self) -> Vec<RecallId> {
        let mut v: Vec<RecallId> = (0..self.inputs.len())
            .filter(|i| self.inputs[*i].history_beats.is_some())
            .map(RecallId::Input)
            .collect();
        v.extend(self.strip_recalls.iter().map(|r| RecallId::Strip(r.id)));
        v
    }

    fn strip_slot(&self, id: StripId) -> Option<usize> {
        let k = slot_of(id, self.decks.len());
        (k < self.strip_recalls.len()).then_some(k)
    }

    pub(crate) fn main_recall(&self, id: StripId) -> Option<&MainRecall> {
        self.strip_slot(id).map(|k| &self.strip_recalls[k])
    }

    pub(crate) fn main_recall_mut(&mut self, id: StripId) -> Option<&mut MainRecall> {
        self.strip_slot(id).map(move |k| &mut self.strip_recalls[k])
    }

    /// Does this strip keep a history?
    pub fn has_history(&self, id: impl Into<RecallId>) -> bool {
        match id.into() {
            RecallId::Input(i) => self.inputs.get(i).is_some_and(|x| x.history_beats.is_some()),
            RecallId::Strip(s) => self.strip_slot(s).is_some(),
        }
    }

    /// Send a strip's history or freeze a message.
    pub(crate) fn recall_send(&mut self, id: impl Into<RecallId>, m: RecallMsg) -> bool {
        match id.into() {
            RecallId::Input(i) => {
                let im = match m {
                    RecallMsg::Tempo(b) => InputMsg::Tempo(b),
                    RecallMsg::HistoryBeats(b) => InputMsg::HistoryBeats(b),
                    RecallMsg::Touch => InputMsg::Touch,
                    RecallMsg::Scrub(f) => InputMsg::Scrub(f),
                    RecallMsg::Release => InputMsg::Release,
                    RecallMsg::Live => InputMsg::Live,
                    RecallMsg::LoopGrab(b) => InputMsg::LoopGrab(b),
                    RecallMsg::FreezeSet(s) => InputMsg::FreezeSet(s),
                    RecallMsg::Freeze { on, beats, at } => InputMsg::Freeze { on, beats, at },
                    RecallMsg::FreezeScrub(f) => InputMsg::FreezeScrub(f),
                };
                self.input_send(i, im)
            }
            RecallId::Strip(s) => match self.strip_slot(s) {
                Some(k) => self.send_msg(Msg::Recall(k, m)),
                None => false,
            },
        }
    }

    /// What a strip's history and freeze look like now.
    pub fn recall_view(&self, id: impl Into<RecallId>) -> Option<&dyn RecallView> {
        match id.into() {
            RecallId::Input(i) => self
                .inputs
                .get(i)
                .filter(|x| x.history_beats.is_some())
                .map(|x| x.mirror.as_ref() as &dyn RecallView),
            RecallId::Strip(s) => self
                .main_recall(s)
                .map(|r| r.mirror.as_ref() as &dyn RecallView),
        }
    }

    /// A strip's freeze knobs.
    pub fn freeze_settings(&self, id: impl Into<RecallId>) -> Option<FreezeSettings> {
        match id.into() {
            RecallId::Input(i) => self.inputs.get(i).map(|x| x.freeze),
            RecallId::Strip(s) => self.main_recall(s).map(|r| r.freeze),
        }
    }

    pub(crate) fn freeze_settings_mut(&mut self, id: RecallId) -> Option<&mut FreezeSettings> {
        match id {
            RecallId::Input(i) => self.inputs.get_mut(i).map(|x| &mut x.freeze),
            RecallId::Strip(s) => self.main_recall_mut(s).map(|r| &mut r.freeze),
        }
    }

    pub(crate) fn set_frozen_flag(&mut self, id: RecallId, on: bool) {
        match id {
            RecallId::Input(i) => {
                if let Some(x) = self.inputs.get_mut(i) {
                    x.frozen = on;
                }
            }
            RecallId::Strip(s) => {
                if let Some(r) = self.main_recall_mut(s) {
                    r.frozen = on;
                }
            }
        }
    }

    /// The history loop's length on a strip, beats.
    pub fn recall_loop_beats(&self, id: impl Into<RecallId>) -> f64 {
        match id.into() {
            RecallId::Input(i) => self.inputs.get(i).map_or(history::DEFAULT_LOOP_BEATS, |x| x.loop_beats),
            RecallId::Strip(s) => self
                .main_recall(s)
                .map_or(history::DEFAULT_LOOP_BEATS, |r| r.loop_beats),
        }
    }

    pub(crate) fn set_recall_loop_beats(&mut self, id: RecallId, b: f64) {
        match id {
            RecallId::Input(i) => {
                if let Some(x) = self.inputs.get_mut(i) {
                    x.loop_beats = b;
                }
            }
            RecallId::Strip(s) => {
                if let Some(r) = self.main_recall_mut(s) {
                    r.loop_beats = b;
                }
            }
        }
    }

    /// Tell every strip's history the master tempo (sends on change; from
    /// [`sync_history_tempo`](Engine::sync_history_tempo)).
    pub(crate) fn sync_strip_recall_tempo(&mut self, bpm: f64) {
        for k in 0..self.strip_recalls.len() {
            if (self.strip_recalls[k].last_tempo - bpm).abs() > 0.005 {
                self.strip_recalls[k].last_tempo = bpm;
                self.send_msg(Msg::Recall(k, RecallMsg::Tempo(bpm)));
            }
        }
    }
}
