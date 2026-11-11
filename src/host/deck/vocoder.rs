//! The vocoder strip (beyond Swift; docs/VOCODER.md): [`crate::fx::vocoder`] in the mix.
//!
//! A modulator (a live input strip such as a mic, or a loop playing inside the vocoder: a
//! loop-row clip, a library loop, or a capture from an input's history) shapes a carrier (an
//! input strip such as the TD-3, a deck, or the internal [`CarrierSynth`]). The vocoder taps
//! its sources **pre-fader**: a mic strip can sit muted or pulled down and still drive it, and
//! the TD-3's own fader still sets what is heard of it dry.
//!
//! Two ways in the mix:
//!
//! - **Strip** (the default): the vocoded signal plays on its own strip, `V`, with a fader,
//!   mute and PFL, summed into the master after the input strips.
//! - **Insert**: the vocoded signal (dry / wet against the carrier) replaces the carrier's own
//!   strip signal before that strip's fader, cue and loop record; `V`'s fader is then unused.
//!   A synth carrier has no strip of its own, so it always plays on `V`.
//!
//! With no modulator the vocoder passes the carrier through, so the formant (vowel) filter can
//! be used alone. Off, it is silent as a strip and a bypass as an insert.
//!
//! Every move goes into the action trace in words (no session op, like the input strips).
//!
//! **Instances.** There are up to [`MAX_VOCODERS`] vocoders, each with its own sources, knobs,
//! synth, freeze, meter and routing; instance 0 is the strip `V`, the others `V2`, `V3`, … All
//! are allocated when the engine is made, so adding one allocates nothing on the render thread,
//! and one that is off costs a branch. The `vocoder_*` calls act on the *selected* instance
//! ([`VocoderBank::selected`], the window's tab, what the bindings move);
//! [`Engine::with_vocoder`] runs them on another. Instances render in order, so an insert on a
//! strip is what a later instance tapping that strip hears. Only instance 0 feeds `V`'s
//! history, its loop-grab taps and its recall; every instance's output is in the `vocoder` stem.

use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::deck::turntable::SampleBuffer;
use crate::fx::vocoder::{
    band_centers, note_name, vowel_name, CarrierSource, CarrierSynth, FormantFilter, LoopSource,
    ModulatorSource, Vocoder, VocoderParam, VocoderSettings, MAX_BANDS,
};
use crate::json::{JsonObject, JsonValue};

use super::engine::{Engine, Msg, MAX_BLOCK};
use super::trace::Traced;

/// How many vocoders can run at once (instance 0 is always there).
pub const MAX_VOCODERS: usize = 4;

/// An instance's strip name: `V`, `V2`, `V3`, …
pub fn vocoder_label(k: usize) -> String {
    if k == 0 {
        "V".into()
    } else {
        format!("V{}", k + 1)
    }
}

/// Which signal the render thread reads as the modulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModTap {
    None,
    Input(usize),
    Deck(usize),
    Loop,
    /// The internal synth, rendered as the modulator.
    Synth,
}

/// Which signal it reads as the carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarTap {
    None,
    Input(usize),
    Deck(usize),
    Synth,
    /// The carrier's own loop.
    Loop,
}

/// Main thread → the vocoder strip.
pub enum VocoderMsg {
    Settings(VocoderSettings),
    Modulator(ModTap),
    Carrier(CarTap),
    /// The loop the modulator plays (None: none), restarted from its top.
    Loop(Option<Arc<SampleBuffer>>),
    /// Frames of the loop per output frame (its tempo against the master's, and its rate).
    LoopRate(f64),
    /// The loop the carrier plays (None: none), restarted from its top.
    CarrierLoop(Option<Arc<SampleBuffer>>),
    /// The carrier loop's rate, as [`VocoderMsg::LoopRate`].
    CarrierLoopRate(f64),
    NoteOn(u8),
    NoteOff(u8),
    AllOff,
}

/// What the strip publishes for the main thread.
pub struct VocoderMirror {
    modulator: AtomicU32,
    carrier: AtomicU32,
    level: AtomicU32,
    sibilance: AtomicU32,
    loop_pos: AtomicU32,
    car_loop_pos: AtomicU32,
    env: [AtomicU32; MAX_BANDS],
    /// Each band's envelope peak, held and falling about 20 dB a second.
    peak: [AtomicU32; MAX_BANDS],
}

impl Default for VocoderMirror {
    fn default() -> Self {
        VocoderMirror {
            modulator: AtomicU32::new(0),
            carrier: AtomicU32::new(0),
            level: AtomicU32::new(0),
            sibilance: AtomicU32::new(0),
            loop_pos: AtomicU32::new(0),
            car_loop_pos: AtomicU32::new(0),
            env: std::array::from_fn(|_| AtomicU32::new(0)),
            peak: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

fn put(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}
fn get(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

impl VocoderMirror {
    /// Peaks at the last block: (modulator, carrier, the vocoder's output after its fader or,
    /// as an insert, before the carrier's).
    pub fn levels(&self) -> (f32, f32, f32) {
        (get(&self.modulator), get(&self.carrier), get(&self.level))
    }
    /// Each band's envelope (the first `n`).
    pub fn envelopes(&self, n: usize) -> Vec<f32> {
        self.env[..n.min(MAX_BANDS)].iter().map(get).collect()
    }
    /// Each band's held peak (the first `n`), falling about 20 dB a second.
    pub fn peaks(&self, n: usize) -> Vec<f32> {
        self.peak[..n.min(MAX_BANDS)].iter().map(get).collect()
    }
    /// The unvoiced detector, 0..1.
    pub fn sibilance(&self) -> f32 {
        get(&self.sibilance)
    }
    /// Where the loop modulator is, 0..1 of its length.
    pub fn loop_position(&self) -> f32 {
        get(&self.loop_pos)
    }
    /// Where the carrier's loop is, 0..1 of its length.
    pub fn carrier_loop_position(&self) -> f32 {
        get(&self.car_loop_pos)
    }
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |p, v| p.max(v.abs()))
}

/// A loop played inside the vocoder (either side's): looped, linear interpolation, at a rate.
struct LoopPlayer {
    buf: Option<Arc<SampleBuffer>>,
    pos: f64,
    rate: f64,
}

impl LoopPlayer {
    fn new() -> LoopPlayer {
        LoopPlayer {
            buf: None,
            pos: 0.0,
            rate: 1.0,
        }
    }

    /// A new loop from its top; returns the one let go of.
    fn load(&mut self, b: Option<Arc<SampleBuffer>>) -> Option<Arc<SampleBuffer>> {
        self.pos = 0.0;
        std::mem::replace(&mut self.buf, b)
    }

    fn set_rate(&mut self, r: f64) {
        if r.is_finite() && r > 0.0 {
            self.rate = r.clamp(0.125, 8.0);
        }
    }

    /// The next `l.len()` frames into `l` / `r` (a mono loop in both); silence with none.
    /// Returns where it is, 0..1 of its length.
    fn render(&mut self, l: &mut [f32], r: &mut [f32]) -> f32 {
        let Some(b) = &self.buf else {
            l.fill(0.0);
            r.fill(0.0);
            return 0.0;
        };
        let len = b.left.len();
        if len == 0 {
            l.fill(0.0);
            r.fill(0.0);
            return 0.0;
        }
        let right = b.right.as_deref().unwrap_or(&b.left);
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let i = self.pos as usize % len;
            let j = (i + 1) % len;
            let t = (self.pos - self.pos.floor()) as f32;
            *x = b.left[i] + t * (b.left[j] - b.left[i]);
            *y = right[i] + t * (right[j] - right[i]);
            self.pos += self.rate;
            if self.pos >= len as f64 {
                self.pos -= len as f64;
            }
        }
        (self.pos / len as f64) as f32
    }
}

/// The render thread's half.
pub(crate) struct VocoderStrip {
    settings: VocoderSettings,
    vocoder: Vocoder,
    formant: FormantFilter,
    synth: CarrierSynth,
    pub(crate) modt: ModTap,
    pub(crate) cart: CarTap,
    mod_loop: LoopPlayer,
    car_loop: LoopPlayer,
    m: Vec<f32>,
    /// Scratch: the right half of a stereo modulator loop.
    m2: Vec<f32>,
    c: Vec<f32>,
    w: Vec<f32>,
    pub(crate) l: Vec<f32>,
    pub(crate) r: Vec<f32>,
    /// What `V` added to the master in the last block, after its fader (as an insert: the
    /// insert's output; off: silence), for the bus tap.
    pub(crate) post_l: Vec<f32>,
    pub(crate) post_r: Vec<f32>,
    last_gain: f64,
    /// Each band's held peak, and how much it falls per frame.
    peaks: [f32; MAX_BANDS],
    peak_fall: f32,
    mirror: Arc<VocoderMirror>,
    /// What the last pass made: nothing, the strip's own signal, or an insert (already in the
    /// carrier's buffer).
    pub(crate) made: Made,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Made {
    Nothing,
    Strip,
    Insert,
}

impl VocoderStrip {
    pub(crate) fn new(sample_rate: f64, mirror: Arc<VocoderMirror>) -> VocoderStrip {
        let settings = VocoderSettings::default();
        VocoderStrip {
            vocoder: Vocoder::new(sample_rate, &settings),
            formant: FormantFilter::new(sample_rate, settings.vowel),
            synth: CarrierSynth::new(sample_rate),
            settings,
            modt: ModTap::None,
            cart: CarTap::None,
            mod_loop: LoopPlayer::new(),
            car_loop: LoopPlayer::new(),
            m: vec![0.0; MAX_BLOCK],
            m2: vec![0.0; MAX_BLOCK],
            c: vec![0.0; MAX_BLOCK],
            w: vec![0.0; MAX_BLOCK],
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            post_l: vec![0.0; MAX_BLOCK],
            post_r: vec![0.0; MAX_BLOCK],
            last_gain: 0.0,
            peaks: [0.0; MAX_BANDS],
            // -20 dB a second
            peak_fall: (0.1f64.powf(1.0 / sample_rate)) as f32,
            mirror,
            made: Made::Nothing,
        }
    }

    /// Apply a message; returns a loop buffer it let go of (for the retire ring).
    pub(crate) fn apply(&mut self, m: VocoderMsg) -> Option<Arc<SampleBuffer>> {
        match m {
            VocoderMsg::Settings(s) => {
                self.vocoder.configure(&s);
                self.formant.set_vowel(s.vowel);
                self.synth.set_level(s.synth_level);
                if s.synth != self.settings.synth || s.synth_detune != self.settings.synth_detune {
                    self.synth.set_patch(&s.synth);
                    self.synth.set_detune(s.synth_detune);
                }
                if s.on && !self.settings.on {
                    self.vocoder.reset();
                }
                self.settings = s;
            }
            VocoderMsg::Modulator(t) => self.modt = t,
            VocoderMsg::Carrier(t) => self.cart = t,
            VocoderMsg::Loop(b) => return self.mod_loop.load(b),
            VocoderMsg::LoopRate(r) => self.mod_loop.set_rate(r),
            VocoderMsg::CarrierLoop(b) => return self.car_loop.load(b),
            VocoderMsg::CarrierLoopRate(r) => self.car_loop.set_rate(r),
            VocoderMsg::NoteOn(n) => self.synth.note_on(n),
            VocoderMsg::NoteOff(n) => self.synth.note_off(n),
            VocoderMsg::AllOff => self.synth.all_off(),
        }
        None
    }

    /// Vocode a block, first half: the modulator read (copied out, so one strip can be both). None when the vocoder
    /// is off (nothing more to do); else whether there is a modulator.
    pub(crate) fn begin(&mut self, n: usize, modulator: Option<(&[f32], &[f32])>) -> Option<bool> {
        let n = n.min(MAX_BLOCK);
        self.made = Made::Nothing;
        if !self.settings.on {
            put(&self.mirror.level, 0.0);
            return None;
        }
        Some(self.load_modulator(n, modulator))
    }

    /// The modulator, mono, into `self.m`. False when there is none.
    fn load_modulator(&mut self, n: usize, modulator: Option<(&[f32], &[f32])>) -> bool {
        match (self.modt, modulator) {
            (ModTap::Input(_) | ModTap::Deck(_), Some((l, r))) => {
                for i in 0..n {
                    self.m[i] = 0.5 * (l[i] + r[i]);
                }
                true
            }
            (ModTap::Loop, _) => {
                let at = self.mod_loop.render(&mut self.m[..n], &mut self.m2[..n]);
                for i in 0..n {
                    self.m[i] = 0.5 * (self.m[i] + self.m2[i]);
                }
                put(&self.mirror.loop_pos, at);
                self.mod_loop.buf.is_some()
            }
            (ModTap::Synth, _) => {
                self.m[..n].fill(0.0);
                self.synth.render(&mut self.m[..n]);
                true
            }
            _ => {
                self.m[..n].fill(0.0);
                false
            }
        }
    }

    /// The second half: the carrier read, vocoded, and (as an insert) written back.
    pub(crate) fn finish(
        &mut self,
        n: usize,
        has_mod: bool,
        carrier: Option<(&mut [f32], &mut [f32])>,
        crossfader: f64,
    ) {
        let n = n.min(MAX_BLOCK);
        // the carrier: stereo dry into l / r, mono into c
        let mut carrier = carrier;
        match (&mut carrier, self.cart) {
            (Some((cl, cr)), CarTap::Input(_) | CarTap::Deck(_)) => {
                self.l[..n].copy_from_slice(&cl[..n]);
                self.r[..n].copy_from_slice(&cr[..n]);
            }
            (_, CarTap::Synth) => {
                self.l[..n].fill(0.0);
                self.synth.render(&mut self.l[..n]);
                self.r[..n].copy_from_slice(&self.l[..n]);
            }
            (_, CarTap::Loop) => {
                let at = self.car_loop.render(&mut self.l[..n], &mut self.r[..n]);
                put(&self.mirror.car_loop_pos, at);
            }
            _ => {
                self.l[..n].fill(0.0);
                self.r[..n].fill(0.0);
            }
        }
        for i in 0..n {
            self.c[i] = 0.5 * (self.l[i] + self.r[i]);
        }
        put(&self.mirror.modulator, peak(&self.m[..n]));
        put(&self.mirror.carrier, peak(&self.c[..n]));
        // bypassed: the carrier as it is, the modulator left out (a jam synth on V); frozen: the
        // held bands shape it with or without a modulator
        let has_mod = (has_mod || self.settings.freeze) && !self.settings.bypass;
        // wet: vocoded, or the carrier as is with no modulator (the vowel filter alone)
        if has_mod {
            self.vocoder
                .process(&self.m[..n], &self.c[..n], &mut self.w[..n]);
        } else {
            self.w[..n].copy_from_slice(&self.c[..n]);
        }
        let s = &self.settings;
        self.formant.process(&mut self.w[..n], s.vowel_mix as f32);
        if s.xfade {
            // the crossfader's morph: the carrier (left), vocoded (centre), the modulator (right)
            let t = crossfader.clamp(0.0, 1.0) as f32;
            let a = (2.0 * t).min(1.0);
            let b = (2.0 * t - 1.0).max(0.0);
            let (car, wet, m) = (1.0 - a, a - b, b);
            for i in 0..n {
                let x = wet * self.w[i] + m * self.m[i];
                self.l[i] = car * self.l[i] + x;
                self.r[i] = car * self.r[i] + x;
            }
        } else {
            let wet = if has_mod { s.mix as f32 } else { 1.0 };
            let dry = 1.0 - wet;
            for i in 0..n {
                self.l[i] = dry * self.l[i] + wet * self.w[i];
                self.r[i] = dry * self.r[i] + wet * self.w[i];
            }
        }
        let fall = self.peak_fall.powi(n as i32);
        for (k, e) in self.vocoder.envelopes().iter().enumerate() {
            put(&self.mirror.env[k], *e);
            self.peaks[k] = e.max(self.peaks[k] * fall);
            put(&self.mirror.peak[k], self.peaks[k]);
        }
        put(&self.mirror.sibilance, self.vocoder.sibilance());
        match carrier {
            Some((cl, cr))
                if s.insert && matches!(self.cart, CarTap::Input(_) | CarTap::Deck(_)) =>
            {
                cl[..n].copy_from_slice(&self.l[..n]);
                cr[..n].copy_from_slice(&self.r[..n]);
                put(
                    &self.mirror.level,
                    peak(&self.l[..n]).max(peak(&self.r[..n])),
                );
                self.made = Made::Insert;
            }
            _ => self.made = Made::Strip,
        }
    }

    /// The crossfader morphs V, so the decks are left out of it.
    pub(crate) fn owns_crossfader(&self) -> bool {
        self.settings.on && self.settings.xfade
    }

    /// Add the strip, through its fader, into the master (and the cue when PFL is on).
    pub(crate) fn mix(
        &mut self,
        n: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
        cue: Option<(&mut [f32], &mut [f32])>,
    ) {
        let target = if self.made == Made::Strip && !self.settings.mute {
            self.settings.volume
        } else {
            0.0
        };
        if self.made != Made::Strip && self.last_gain == 0.0 {
            if self.made == Made::Nothing {
                put(&self.mirror.level, 0.0);
                self.post_l[..n].fill(0.0);
                self.post_r[..n].fill(0.0);
            } else {
                self.post_l[..n].copy_from_slice(&self.l[..n]);
                self.post_r[..n].copy_from_slice(&self.r[..n]);
            }
            return;
        }
        if self.made == Made::Insert {
            self.post_l[..n].copy_from_slice(&self.l[..n]);
            self.post_r[..n].copy_from_slice(&self.r[..n]);
        }
        if self.made != Made::Strip {
            // fading out of strip mode: the last block's signal is gone, ramp the gain only
            self.l[..n].fill(0.0);
            self.r[..n].fill(0.0);
        }
        let (g0, step) = (self.last_gain, (target - self.last_gain) / n.max(1) as f64);
        self.last_gain = target;
        let mut p = 0f32;
        for i in 0..n {
            let g = (g0 + step * (i + 1) as f64) as f32;
            let (a, b) = (g * self.l[i], g * self.r[i]);
            out_l[i] += a;
            out_r[i] += b;
            p = p.max(a.abs()).max(b.abs());
            if self.made != Made::Insert {
                self.post_l[i] = a;
                self.post_r[i] = b;
            }
        }
        if let (Some((cl, cr)), true) = (cue, self.settings.pfl && self.made == Made::Strip) {
            for i in 0..n {
                cl[i] += self.l[i];
                cr[i] += self.r[i];
            }
        }
        if self.made == Made::Strip {
            put(&self.mirror.level, p);
        }
    }
}

// MARK: the main thread's half

/// What the main thread keeps about the vocoder.
pub struct MainVocoder {
    pub settings: VocoderSettings,
    pub modulator: ModulatorSource,
    pub carrier: CarrierSource,
    /// The synth's notes held (the keys, or a chord set by hand).
    pub notes: BTreeSet<u8>,
    pub mirror: Arc<VocoderMirror>,
    /// The modulator's and the carrier's loops: their audio (kept for a swap) and tempo.
    mod_loop: LoopSync,
    car_loop: LoopSync,
    /// The synth preset loaded last (None: none, or a synth knob moved since).
    pub preset: Option<String>,
    /// Shown and in use. Instance 0 always is; the others from an add (or any call naming
    /// them) to a remove.
    pub active: bool,
}

impl MainVocoder {
    /// Either side plays the internal synth ([`CarrierSynth`]): only then do its chord, voice,
    /// oscillators, presets and level/detune knobs do anything, so the window shows them only
    /// then. A swap keeps it (the synth just changes side).
    pub fn uses_synth(&self) -> bool {
        self.modulator == ModulatorSource::Synth || self.carrier == CarrierSource::Synth
    }
    pub(crate) fn new(mirror: Arc<VocoderMirror>) -> MainVocoder {
        MainVocoder {
            settings: VocoderSettings::default(),
            modulator: ModulatorSource::None,
            carrier: CarrierSource::None,
            notes: BTreeSet::new(),
            mirror,
            mod_loop: LoopSync::default(),
            car_loop: LoopSync::default(),
            preset: None,
            active: false,
        }
    }
}

/// Every vocoder instance, main thread's half. Derefs to the selected one, so
/// `engine.vocoder.settings` is the selected instance's (instance 0 unless another is picked).
pub struct VocoderBank {
    /// [`MAX_VOCODERS`] of them, made up front; `active` says which are in use.
    pub instances: Vec<MainVocoder>,
    /// The instance the `vocoder_*` calls act on.
    at: usize,
    /// The synth's presets, shared by every instance: the user's and where they are kept.
    pub presets: super::synth_presets::SynthPresets,
}

impl VocoderBank {
    pub(crate) fn new(mirrors: Vec<Arc<VocoderMirror>>) -> VocoderBank {
        let mut instances: Vec<MainVocoder> = mirrors.into_iter().map(MainVocoder::new).collect();
        instances[0].active = true;
        VocoderBank {
            instances,
            at: 0,
            presets: Default::default(),
        }
    }
    /// The instance the `vocoder_*` calls act on.
    pub fn selected(&self) -> usize {
        self.at
    }
    /// The instances in use, in order.
    pub fn active(&self) -> Vec<usize> {
        (0..self.instances.len())
            .filter(|k| self.instances[*k].active)
            .collect()
    }
}

impl Deref for VocoderBank {
    type Target = MainVocoder;
    fn deref(&self) -> &MainVocoder {
        &self.instances[self.at]
    }
}

impl DerefMut for VocoderBank {
    fn deref_mut(&mut self) -> &mut MainVocoder {
        &mut self.instances[self.at]
    }
}

/// A loop one side of the vocoder plays: its audio and how to keep it at the master tempo.
#[derive(Default)]
struct LoopSync {
    buffer: Option<Arc<SampleBuffer>>,
    /// Its tempo (0: played at its own rate).
    bpm: f64,
    /// Its sample rate against the engine's.
    rate_sr: f64,
    last_rate: f64,
}

/// A loop for either side, as a caller names it.
enum LoopPick {
    Clip(String),
    Library(String),
    Capture(usize, Option<f64>),
    Loaded(LoopSource, Arc<SampleBuffer>),
}

/// A modulator to pick, as a caller names it.
#[derive(Clone, Debug, PartialEq)]
pub enum ModulatorPick {
    None,
    /// An input strip by index.
    Input(usize),
    /// A deck by name, live (pre-fader): whatever is loaded on it.
    Deck(String),
    /// A loop-row clip by id.
    Clip(String),
    /// A library loop by id.
    Library(String),
    /// The last `beats` (None: the strip's loop length) of input `i`'s history.
    Capture(usize, Option<f64>),
    /// An audio file, decoded (the browser's "to the vocoder", docs/BROWSER.md): `id` is its
    /// path, `bpm` 0 when unknown (played as is).
    File {
        buffer: Arc<SampleBuffer>,
        id: String,
        title: String,
        bpm: f64,
        beats: f64,
    },
    /// The internal synth (its notes) as the modulator. Not while it is the carrier.
    Synth,
    /// A loop already in hand, as it was (what a swap hands over).
    Loaded(LoopSource, Arc<SampleBuffer>),
}

/// A carrier to pick.
#[derive(Clone, Debug, PartialEq)]
pub enum CarrierPick {
    None,
    Input(usize),
    Deck(String),
    Synth,
    /// A loop-row clip by id, played in the vocoder at the master tempo.
    Clip(String),
    /// A library loop by id.
    Library(String),
    /// The last `beats` of input `i`'s history.
    Capture(usize, Option<f64>),
    /// A loop already in hand (a browser file, or what a swap hands over).
    Loaded(LoopSource, Arc<SampleBuffer>),
}

/// The vocoder's on / off switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VocoderSwitch {
    On,
    Insert,
    Mute,
    Pfl,
    /// The carrier straight onto V, the modulator left out (the synth as a jam synth).
    Bypass,
    /// The band envelopes held: the carrier keeps the shape with the modulator gone.
    Freeze,
    /// The crossfader morphs V (carrier, vocoded, modulator) instead of crossfading the decks.
    Xfade,
}

impl VocoderSwitch {
    pub fn parse(s: &str) -> Option<VocoderSwitch> {
        Some(match s {
            "on" | "enabled" => VocoderSwitch::On,
            "insert" => VocoderSwitch::Insert,
            "mute" => VocoderSwitch::Mute,
            "pfl" | "cue" => VocoderSwitch::Pfl,
            "bypass" | "jam" => VocoderSwitch::Bypass,
            "freeze" | "hold" => VocoderSwitch::Freeze,
            "xfade" | "crossfader" | "morph" => VocoderSwitch::Xfade,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            VocoderSwitch::On => "on",
            VocoderSwitch::Insert => "insert",
            VocoderSwitch::Mute => "mute",
            VocoderSwitch::Pfl => "pfl",
            VocoderSwitch::Bypass => "bypass",
            VocoderSwitch::Freeze => "freeze",
            VocoderSwitch::Xfade => "xfade",
        }
    }
}

fn fmt_value(p: VocoderParam, v: f64) -> String {
    match p {
        VocoderParam::Bands => format!("{v:.0}"),
        VocoderParam::Shift => format!("{v:+.1} st"),
        VocoderParam::Attack | VocoderParam::Release => format!("{v:.1} ms"),
        VocoderParam::Vowel => format!("{v:.2} ({})", vowel_name(v)),
        VocoderParam::SynthDetune => format!("{v:.0} ct"),
        VocoderParam::Cutoff => format!("{v:.0} Hz"),
        VocoderParam::EnvDecay | VocoderParam::SynthAttack | VocoderParam::SynthRelease => {
            format!("{v:.0} ms")
        }
        VocoderParam::EnvAmount => format!("{v:+.2}"),
        VocoderParam::Sync => (if v >= 0.5 { "on" } else { "off" }).to_string(),
        VocoderParam::Osc1Wave | VocoderParam::Osc2Wave | VocoderParam::Osc3Wave => {
            crate::fx::eurosynth::Wave::from_index(v).name().to_string()
        }
        VocoderParam::Osc1Oct | VocoderParam::Osc2Oct | VocoderParam::Osc3Oct => {
            format!("{v:+.0} oct")
        }
        VocoderParam::Osc1Fine | VocoderParam::Osc2Fine | VocoderParam::Osc3Fine => {
            format!("{v:+.0} ct")
        }
        _ => format!("{v:.2}"),
    }
}

impl Engine {
    pub(crate) fn vocoder_send(&mut self, m: VocoderMsg) -> bool {
        let k = self.vocoder.at;
        self.send_msg(Msg::Vocoder(k, m))
    }

    /// The selected instance (the window's tab; what the bindings move).
    pub fn vocoder_selected(&self) -> usize {
        self.vocoder.at
    }

    /// Make instance `k` the one the `vocoder_*` calls act on, adding it when it is not in use.
    /// Returns the one acted on before (for [`vocoder_leave`](Self::vocoder_leave)).
    pub fn vocoder_enter(&mut self, k: usize) -> Result<usize, String> {
        if k >= MAX_VOCODERS {
            return Err(format!(
                "no vocoder instance {k}: 0 to {} (at most {MAX_VOCODERS})",
                MAX_VOCODERS - 1
            ));
        }
        let was = self.vocoder.at;
        self.vocoder.instances[k].active = true;
        self.vocoder.at = k;
        Ok(was)
    }

    /// Back to the instance acted on before [`vocoder_enter`](Self::vocoder_enter) (instance 0
    /// if that one was removed meanwhile).
    pub fn vocoder_leave(&mut self, was: usize) {
        self.vocoder.at = if self.vocoder.instances.get(was).is_some_and(|v| v.active) {
            was
        } else {
            0
        };
    }

    /// Run `f` on vocoder instance `k` (adding it when it is not in use), then go back to the
    /// selected one.
    pub fn with_vocoder<R>(
        &mut self,
        k: usize,
        f: impl FnOnce(&mut Engine) -> R,
    ) -> Result<R, String> {
        let was = self.vocoder_enter(k)?;
        let r = f(self);
        self.vocoder_leave(was);
        Ok(r)
    }

    /// A binding's `instance` arg (empty or absent: the selected vocoder): run `f` on it.
    pub(crate) fn on_bound_vocoder<R>(
        &mut self,
        instance: Option<&str>,
        f: impl FnOnce(&mut Engine) -> R,
    ) -> Result<R, String> {
        match instance.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(f(self)),
            Some(t) => {
                let k = t
                    .parse::<usize>()
                    .map_err(|_| format!("instance {t:?}: a number, 0.."))?;
                self.with_vocoder(k, f)
            }
        }
    }

    /// Select instance `k` (the window's tab, the bindings' target), adding it when it is not in
    /// use. No sound changes, so no line in the trace.
    pub fn vocoder_select(&mut self, k: usize) -> Result<(), String> {
        self.vocoder_enter(k).map(|_| ())
    }

    /// Step the selection through the instances in use.
    pub fn vocoder_select_step(&mut self, dir: i32) {
        let act = self.vocoder.active();
        let now = act.iter().position(|k| *k == self.vocoder.at);
        let k = step_index(now, act.len(), dir);
        self.vocoder.at = act[k];
    }

    /// Add an instance (the first not in use) and select it. Err when all are in use.
    pub fn vocoder_add(&mut self) -> Result<usize, String> {
        let k = (0..MAX_VOCODERS)
            .find(|k| !self.vocoder.instances[*k].active)
            .ok_or_else(|| format!("all {MAX_VOCODERS} vocoders are in use"))?;
        self.vocoder_select(k)?;
        self.vocoder_trace(
            "add",
            "vocoder added".to_string(),
            false,
            live_params([]),
        );
        Ok(k)
    }

    /// Remove instance `k` (not 0): off, its sources let go, its notes released, its knobs back
    /// to the defaults. The selection moves to instance 0 if it was on `k`.
    pub fn vocoder_remove(&mut self, k: usize) -> Result<(), String> {
        if k == 0 {
            return Err("vocoder V (instance 0) is always there".into());
        }
        if k >= MAX_VOCODERS || !self.vocoder.instances[k].active {
            return Err(format!("vocoder instance {k} is not in use"));
        }
        let was = self.vocoder_enter(k)?;
        self.set_modulator(ModulatorPick::None)?;
        self.set_carrier(CarrierPick::None)?;
        self.vocoder.notes.clear();
        self.vocoder_send(VocoderMsg::AllOff);
        self.vocoder.settings = VocoderSettings::default();
        self.vocoder.preset = None;
        let s = self.vocoder.settings.clone();
        self.vocoder_send(VocoderMsg::Settings(s));
        self.vocoder_trace(
            "remove",
            "vocoder removed".to_string(),
            false,
            live_params([("remove", true.into())]),
        );
        self.vocoder.instances[k].active = false;
        self.vocoder_leave(was);
        Ok(())
    }

    /// A vocoder move in the action trace: its words and, for an arrangement, the `vocoder`
    /// live op that makes it again (`params`: the socket method's). `fold`: a knob.
    fn vocoder_trace(&mut self, key: &str, text: String, fold: bool, params: JsonValue) {
        if !self.tracing() {
            return;
        }
        // another instance than V: its name in the words and the key, its index in the op
        let k = self.vocoder.at;
        let (key, text, params) = if k == 0 {
            (key.to_string(), text, params)
        } else {
            let label = vocoder_label(k);
            let params = match params {
                JsonValue::Object(mut o) => {
                    o.set("instance", JsonValue::int(k as i64));
                    JsonValue::Object(o)
                }
                other => other,
            };
            (
                format!("{label}:{key}"),
                text.replacen("vocoder", &format!("vocoder {label}"), 1),
                params,
            )
        };
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(Traced {
            entry: crate::control::action_log::Entry {
                source: via,
                key: format!("vocoder:{key}"),
                text,
                step: None,
                fold,
            },
            // `params` null: words only (a preset saved or deleted changes no sound)
            ops: (!matches!(params, JsonValue::Null))
                .then(|| crate::session::arrangement::ArrangeOp::Live {
                    method: "vocoder".into(),
                    params,
                })
                .into_iter()
                .collect(),
        });
    }

    /// A line in the action trace, not folded, and no op.
    pub(crate) fn vocoder_trace_text(&mut self, key: &str, text: String) {
        self.vocoder_trace(key, text, false, JsonValue::Null);
    }

    /// A line in the action trace, with the `vocoder` live op that sets every synth knob as it
    /// is now (a preset loaded: an arrangement plays the knobs, whatever the preset is called
    /// by then).
    pub(crate) fn vocoder_trace_synth(&mut self, key: &str, text: String) {
        let mut o = crate::json::JsonObject::new();
        for p in VocoderParam::SYNTH {
            o.set(p.name(), JsonValue::Number(self.vocoder.settings.get(p)));
        }
        self.vocoder_trace(key, text, false, JsonValue::Object(o));
    }

    /// Set a vocoder knob in its own units (clamped). Returns the value kept.
    pub fn vocoder_set(&mut self, p: VocoderParam, v: f64) -> Option<f64> {
        let kept = self.vocoder.settings.set(p, v)?;
        if VocoderParam::SYNTH.contains(&p) {
            self.vocoder.preset = None;
        }
        let s = self.vocoder.settings.clone();
        self.vocoder_send(VocoderMsg::Settings(s));
        self.vocoder_trace(
            p.name(),
            format!("vocoder {} {}", p.name(), fmt_value(p, kept)),
            p.is_continuous(),
            live_params([(p.name(), JsonValue::Number(kept))]),
        );
        Some(kept)
    }

    /// Set a vocoder knob from a control's 0..1 travel.
    pub fn vocoder_knob(&mut self, p: VocoderParam, v: f64) -> Option<f64> {
        self.vocoder_set(p, p.from_knob(v))
    }

    pub fn vocoder_switch(&mut self, w: VocoderSwitch, on: bool) {
        let s = &mut self.vocoder.settings;
        match w {
            VocoderSwitch::On => s.on = on,
            VocoderSwitch::Insert => s.insert = on,
            VocoderSwitch::Mute => s.mute = on,
            VocoderSwitch::Pfl => s.pfl = on,
            VocoderSwitch::Bypass => s.bypass = on,
            VocoderSwitch::Freeze => s.freeze = on,
            VocoderSwitch::Xfade => s.xfade = on,
        }
        let s = s.clone();
        self.vocoder_send(VocoderMsg::Settings(s));
        let what = match (w, on) {
            (VocoderSwitch::On, true) => format!(
                "vocoder on: {} → {}",
                self.vocoder.modulator.describe(),
                self.vocoder.carrier.describe()
            ),
            (VocoderSwitch::Insert, true) => {
                format!(
                    "vocoder as an insert on {}",
                    self.vocoder.carrier.describe()
                )
            }
            (VocoderSwitch::Insert, false) => "vocoder on its own strip".to_string(),
            (VocoderSwitch::Bypass, true) => format!(
                "vocoder bypassed: {} straight onto V",
                self.vocoder.carrier.describe()
            ),
            (VocoderSwitch::Freeze, true) => "vocoder freeze: the bands held".to_string(),
            (VocoderSwitch::Xfade, true) => format!(
                "vocoder on the crossfader: {} ← vocoder → {}",
                self.vocoder.carrier.describe(),
                self.vocoder.modulator.describe()
            ),
            (VocoderSwitch::Xfade, false) => "the crossfader back on the decks".to_string(),
            _ => format!("vocoder {} {}", w.name(), if on { "on" } else { "off" }),
        };
        self.vocoder_trace(w.name(), what, false, live_params([(w.name(), on.into())]));
    }

    pub fn vocoder_switch_value(&self, w: VocoderSwitch) -> bool {
        let s = &self.vocoder.settings;
        match w {
            VocoderSwitch::On => s.on,
            VocoderSwitch::Insert => s.insert,
            VocoderSwitch::Mute => s.mute,
            VocoderSwitch::Pfl => s.pfl,
            VocoderSwitch::Bypass => s.bypass,
            VocoderSwitch::Freeze => s.freeze,
            VocoderSwitch::Xfade => s.xfade,
        }
    }

    /// A loop for either side: its words and its audio.
    fn resolve_loop(&mut self, pick: LoopPick) -> Result<(LoopSource, Arc<SampleBuffer>), String> {
        Ok(match pick {
            LoopPick::Clip(id) => {
                let c = self
                    .loops
                    .row
                    .clips
                    .iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| format!("no clip {id:?} on the row"))?;
                let src = LoopSource {
                    kind: "clip".into(),
                    id: c.id.clone(),
                    title: c.title.clone(),
                    bpm: c.bpm,
                    beats: c.beats as f64,
                };
                (src, c.buffer.clone())
            }
            LoopPick::Library(id) => {
                let lib = self
                    .capture_library
                    .as_ref()
                    .ok_or("there is no loop library")?;
                let saved = lib
                    .load(&id, self.sample_rate)
                    .ok_or_else(|| format!("no library loop {id:?}"))?;
                let m = saved.meta;
                let src = LoopSource {
                    kind: "library".into(),
                    id: m.id,
                    title: m.title,
                    bpm: m.bpm,
                    beats: m.beats as f64,
                };
                (src, Arc::new(saved.buffer))
            }
            LoopPick::Capture(i, beats) => {
                let (label, n_beats, _, audio) =
                    self.grab_input_audio(i, beats).ok_or_else(|| {
                        format!("input {i} has no recording, or not that many beats yet")
                    })?;
                let (_, bpm) = self.master_tempo();
                let src = LoopSource {
                    kind: "capture".into(),
                    id: label.clone(),
                    title: format!("{label} last {n_beats} beats"),
                    bpm,
                    beats: n_beats as f64,
                };
                (src, Arc::new(SampleBuffer::new(audio, None, self.sample_rate)))
            }
            LoopPick::Loaded(src, buf) => (src, buf),
        })
    }

    fn loop_sync(&self, src: &LoopSource, buf: &Arc<SampleBuffer>) -> LoopSync {
        LoopSync {
            buffer: Some(buf.clone()),
            bpm: src.bpm,
            rate_sr: buf.sample_rate / self.sample_rate,
            last_rate: 0.0,
        }
    }

    /// Pick the modulator. A loop is copied into the vocoder and plays from its top, following
    /// the master tempo. Only the modulator: the carrier is never touched.
    pub fn vocoder_modulator(&mut self, pick: ModulatorPick) -> Result<(), String> {
        let spec = self.modulator_spec(&pick);
        self.set_modulator(pick)?;
        let text = format!("vocoder modulator: {}", self.vocoder.modulator.describe());
        self.vocoder_trace(
            "modulator",
            text,
            false,
            live_params([("modulator", spec.into())]),
        );
        Ok(())
    }

    /// [`vocoder_modulator`](Self::vocoder_modulator) with no line in the trace.
    fn set_modulator(&mut self, pick: ModulatorPick) -> Result<(), String> {
        let lp = |l: LoopPick| Some(l);
        let (src, tap, lpick) = match pick {
            ModulatorPick::None => (ModulatorSource::None, ModTap::None, None),
            ModulatorPick::Input(i) => {
                let label = self
                    .inputs
                    .get(i)
                    .map(|x| x.settings.label.clone())
                    .ok_or_else(|| format!("no input strip {i}"))?;
                (ModulatorSource::Input(label), ModTap::Input(i), None)
            }
            ModulatorPick::Deck(d) => {
                let i = self.index(&d).ok_or_else(|| format!("no deck {d:?}"))?;
                if self.vocoder.carrier == CarrierSource::Deck(d.clone()) {
                    return Err(format!(
                        "deck {d} is the carrier: a deck cannot shape itself (pick another carrier first)"
                    ));
                }
                (ModulatorSource::Deck(d), ModTap::Deck(i), None)
            }
            ModulatorPick::Synth => {
                if self.vocoder.carrier == CarrierSource::Synth {
                    return Err(
                        "the synth is the carrier: one synth cannot shape itself (pick another carrier first)"
                            .into(),
                    );
                }
                (ModulatorSource::Synth, ModTap::Synth, None)
            }
            ModulatorPick::Clip(id) => (ModulatorSource::None, ModTap::Loop, lp(LoopPick::Clip(id))),
            ModulatorPick::Library(id) => {
                (ModulatorSource::None, ModTap::Loop, lp(LoopPick::Library(id)))
            }
            ModulatorPick::Capture(i, b) => {
                (ModulatorSource::None, ModTap::Loop, lp(LoopPick::Capture(i, b)))
            }
            ModulatorPick::File {
                buffer,
                id,
                title,
                bpm,
                beats,
            } => {
                let src = LoopSource {
                    kind: "file".into(),
                    id,
                    title,
                    bpm,
                    beats,
                };
                (ModulatorSource::None, ModTap::Loop, lp(LoopPick::Loaded(src, buffer)))
            }
            ModulatorPick::Loaded(src, buf) => {
                (ModulatorSource::None, ModTap::Loop, lp(LoopPick::Loaded(src, buf)))
            }
        };
        let (src, buf) = match lpick {
            Some(l) => {
                let (ls, buf) = self.resolve_loop(l)?;
                self.vocoder.mod_loop = self.loop_sync(&ls, &buf);
                (ModulatorSource::Loop(ls), Some(buf))
            }
            None => {
                self.vocoder.mod_loop = LoopSync::default();
                (src, None)
            }
        };
        let is_loop = tap == ModTap::Loop;
        if is_loop || matches!(self.vocoder.modulator, ModulatorSource::Loop(_)) {
            self.vocoder_send(VocoderMsg::Loop(buf));
        }
        self.vocoder_send(VocoderMsg::Modulator(tap));
        self.vocoder.modulator = src;
        self.sync_vocoder_tempo();
        Ok(())
    }

    /// Pick the carrier. Only the carrier: the modulator is never touched.
    pub fn vocoder_carrier(&mut self, pick: CarrierPick) -> Result<(), String> {
        let spec = self.carrier_spec(&pick);
        self.set_carrier(pick)?;
        let text = format!("vocoder carrier: {}", self.vocoder.carrier.describe());
        self.vocoder_trace(
            "carrier",
            text,
            false,
            live_params([("carrier", spec.into())]),
        );
        Ok(())
    }

    /// [`vocoder_carrier`](Self::vocoder_carrier) with no line in the trace.
    fn set_carrier(&mut self, pick: CarrierPick) -> Result<(), String> {
        let (src, tap, lpick) = match pick {
            CarrierPick::None => (CarrierSource::None, CarTap::None, None),
            CarrierPick::Input(i) => {
                let label = self
                    .inputs
                    .get(i)
                    .map(|x| x.settings.label.clone())
                    .ok_or_else(|| format!("no input strip {i}"))?;
                (CarrierSource::Input(label), CarTap::Input(i), None)
            }
            CarrierPick::Deck(d) => {
                let i = self.index(&d).ok_or_else(|| format!("no deck {d:?}"))?;
                if self.vocoder.modulator == ModulatorSource::Deck(d.clone()) {
                    return Err(format!(
                        "deck {d} is the modulator: a deck cannot shape itself (pick another modulator first)"
                    ));
                }
                (CarrierSource::Deck(d), CarTap::Deck(i), None)
            }
            CarrierPick::Synth => {
                if self.vocoder.modulator == ModulatorSource::Synth {
                    return Err(
                        "the synth is the modulator: one synth cannot shape itself (pick another modulator first)"
                            .into(),
                    );
                }
                (CarrierSource::Synth, CarTap::Synth, None)
            }
            CarrierPick::Clip(id) => (CarrierSource::None, CarTap::Loop, Some(LoopPick::Clip(id))),
            CarrierPick::Library(id) => {
                (CarrierSource::None, CarTap::Loop, Some(LoopPick::Library(id)))
            }
            CarrierPick::Capture(i, b) => {
                (CarrierSource::None, CarTap::Loop, Some(LoopPick::Capture(i, b)))
            }
            CarrierPick::Loaded(src, buf) => {
                (CarrierSource::None, CarTap::Loop, Some(LoopPick::Loaded(src, buf)))
            }
        };
        let (src, buf) = match lpick {
            Some(l) => {
                let (ls, buf) = self.resolve_loop(l)?;
                self.vocoder.car_loop = self.loop_sync(&ls, &buf);
                (CarrierSource::Loop(ls), Some(buf))
            }
            None => {
                self.vocoder.car_loop = LoopSync::default();
                (src, None)
            }
        };
        if tap == CarTap::Loop || matches!(self.vocoder.carrier, CarrierSource::Loop(_)) {
            self.vocoder_send(VocoderMsg::CarrierLoop(buf));
        }
        self.vocoder_send(VocoderMsg::Carrier(tap));
        self.vocoder.carrier = src;
        self.sync_vocoder_tempo();
        Ok(())
    }

    /// Swap the roles: the carrier becomes the modulator and the modulator the carrier (a drum
    /// loop shaping the synth turns into the synth shaping the drum loop). A loop keeps its
    /// audio (a capture is not grabbed again). One line in the trace; the live op is `swap`.
    pub fn vocoder_swap(&mut self) -> Result<(), String> {
        let as_car: CarrierPick = match self.vocoder.modulator.clone() {
            ModulatorSource::None => CarrierPick::None,
            ModulatorSource::Input(l) => CarrierPick::Input(
                self.input_index(&l)
                    .ok_or_else(|| format!("no input strip {l:?}"))?,
            ),
            ModulatorSource::Deck(d) => CarrierPick::Deck(d),
            ModulatorSource::Synth => CarrierPick::Synth,
            ModulatorSource::Loop(src) => {
                let buf = self.vocoder.mod_loop.buffer.clone().ok_or("the loop has no audio")?;
                CarrierPick::Loaded(src, buf)
            }
        };
        let as_mod: ModulatorPick = match self.vocoder.carrier.clone() {
            CarrierSource::None => ModulatorPick::None,
            CarrierSource::Input(l) => ModulatorPick::Input(
                self.input_index(&l)
                    .ok_or_else(|| format!("no input strip {l:?}"))?,
            ),
            CarrierSource::Deck(d) => ModulatorPick::Deck(d),
            CarrierSource::Synth => ModulatorPick::Synth,
            CarrierSource::Loop(src) => {
                let buf = self.vocoder.car_loop.buffer.clone().ok_or("the loop has no audio")?;
                ModulatorPick::Loaded(src, buf)
            }
        };
        // the carrier let go first, so a deck or the synth is never on both sides
        self.set_carrier(CarrierPick::None)?;
        self.set_modulator(as_mod)?;
        self.set_carrier(as_car)?;
        let text = format!(
            "vocoder swap: {} → {}",
            self.vocoder.modulator.describe(),
            self.vocoder.carrier.describe()
        );
        self.vocoder_trace("swap", text, false, live_params([("swap", true.into())]));
        Ok(())
    }

    /// Step the modulator through none, each input strip, each deck (not the carrier's), and
    /// the loop row's clips.
    pub fn vocoder_cycle_modulator(&mut self, dir: i32) -> Result<(), String> {
        let mut picks = vec![ModulatorPick::None];
        picks.extend((0..self.inputs.len()).map(ModulatorPick::Input));
        let car = self.vocoder.carrier.clone();
        picks.extend(
            self.decks
                .iter()
                .filter(|d| car != CarrierSource::Deck(d.name.clone()))
                .map(|d| ModulatorPick::Deck(d.name.clone())),
        );
        if car != CarrierSource::Synth {
            picks.push(ModulatorPick::Synth);
        }
        picks.extend(
            self.loops
                .row
                .clips
                .iter()
                .map(|c| ModulatorPick::Clip(c.id.clone())),
        );
        let now = match &self.vocoder.modulator {
            ModulatorSource::None => Some(0),
            ModulatorSource::Input(l) => self
                .input_index(l)
                .and_then(|i| picks.iter().position(|p| *p == ModulatorPick::Input(i))),
            ModulatorSource::Deck(d) => picks
                .iter()
                .position(|p| *p == ModulatorPick::Deck(d.clone())),
            ModulatorSource::Synth => picks.iter().position(|p| *p == ModulatorPick::Synth),
            ModulatorSource::Loop(s) => picks
                .iter()
                .position(|p| *p == ModulatorPick::Clip(s.id.clone())),
        };
        let k = step_index(now, picks.len(), dir);
        self.vocoder_modulator(picks[k].clone())
    }

    /// Step the carrier through none, each input strip, each deck (not the modulator's) and
    /// the synth.
    pub fn vocoder_cycle_carrier(&mut self, dir: i32) -> Result<(), String> {
        let mut picks = vec![CarrierPick::None];
        picks.extend((0..self.inputs.len()).map(CarrierPick::Input));
        let m = self.vocoder.modulator.clone();
        picks.extend(
            self.decks
                .iter()
                .filter(|d| m != ModulatorSource::Deck(d.name.clone()))
                .map(|d| CarrierPick::Deck(d.name.clone())),
        );
        if m != ModulatorSource::Synth {
            picks.push(CarrierPick::Synth);
        }
        picks.extend(
            self.loops
                .row
                .clips
                .iter()
                .map(|c| CarrierPick::Clip(c.id.clone())),
        );
        let now = match &self.vocoder.carrier {
            CarrierSource::None => Some(0),
            CarrierSource::Input(l) => self
                .input_index(l)
                .and_then(|i| picks.iter().position(|p| *p == CarrierPick::Input(i))),
            CarrierSource::Deck(d) => picks
                .iter()
                .position(|p| *p == CarrierPick::Deck(d.clone())),
            CarrierSource::Synth => picks.iter().position(|p| *p == CarrierPick::Synth),
            CarrierSource::Loop(s) => picks
                .iter()
                .position(|p| *p == CarrierPick::Clip(s.id.clone())),
        };
        let k = step_index(now, picks.len(), dir);
        self.vocoder_carrier(picks[k].clone())
    }

    /// A synth note on or off (a key).
    pub fn vocoder_note(&mut self, note: u8, on: bool) {
        let note = note.min(127);
        if on {
            self.vocoder.notes.insert(note);
            self.vocoder_send(VocoderMsg::NoteOn(note));
        } else {
            self.vocoder.notes.remove(&note);
            self.vocoder_send(VocoderMsg::NoteOff(note));
        }
        let held = self.vocoder_notes_text();
        self.vocoder_trace(
            "keys",
            format!("vocoder keys: {held}"),
            true,
            live_params([("note", JsonValue::int(note as i64)), ("down", on.into())]),
        );
    }

    /// Hold exactly these synth notes (a chord; empty: release them all).
    pub fn vocoder_set_notes(&mut self, notes: &[u8]) {
        self.vocoder.notes.clear();
        self.vocoder_send(VocoderMsg::AllOff);
        for &n in notes.iter().take(crate::fx::vocoder::SYNTH_VOICES) {
            let n = n.min(127);
            self.vocoder.notes.insert(n);
            self.vocoder_send(VocoderMsg::NoteOn(n));
        }
        let held = self.vocoder_notes_text();
        let held_notes: Vec<JsonValue> = self
            .vocoder
            .notes
            .iter()
            .map(|n| JsonValue::int(*n as i64))
            .collect();
        self.vocoder_trace(
            "notes",
            format!("vocoder synth chord: {held}"),
            false,
            live_params([("notes", JsonValue::Array(held_notes))]),
        );
    }

    /// A modulator pick as the socket's `modulator` names it (the inverse of djapp's
    /// `vocoder_mod_pick`): what a `vocoder` live op carries.
    fn modulator_spec(&self, pick: &ModulatorPick) -> String {
        let label = |i: usize| {
            self.inputs
                .get(i)
                .map_or_else(|| i.to_string(), |x| x.settings.label.clone())
        };
        match pick {
            ModulatorPick::None => "none".into(),
            ModulatorPick::Input(i) => format!("input:{}", label(*i)),
            ModulatorPick::Deck(d) => format!("deck:{d}"),
            ModulatorPick::Clip(id) => format!("clip:{id}"),
            ModulatorPick::Library(id) => format!("library:{id}"),
            ModulatorPick::Capture(i, None) => format!("capture:{}", label(*i)),
            ModulatorPick::Capture(i, Some(b)) => format!("capture:{}:{b}", label(*i)),
            // a browser file (docs/BROWSER.md): its path, which the socket does not take back
            ModulatorPick::File { id, .. } => format!("file:{id}"),
            ModulatorPick::Synth => "synth".into(),
            ModulatorPick::Loaded(src, _) => loop_spec(src),
        }
    }

    /// A carrier pick as the socket's `carrier` names it.
    fn carrier_spec(&self, pick: &CarrierPick) -> String {
        match pick {
            CarrierPick::None => "none".into(),
            CarrierPick::Synth => "synth".into(),
            CarrierPick::Input(i) => format!(
                "input:{}",
                self.inputs
                    .get(*i)
                    .map_or_else(|| i.to_string(), |x| x.settings.label.clone())
            ),
            CarrierPick::Deck(d) => format!("deck:{d}"),
            CarrierPick::Clip(id) => format!("clip:{id}"),
            CarrierPick::Library(id) => format!("library:{id}"),
            CarrierPick::Capture(i, None) => format!(
                "capture:{}",
                self.inputs
                    .get(*i)
                    .map_or_else(|| i.to_string(), |x| x.settings.label.clone())
            ),
            CarrierPick::Capture(i, Some(b)) => format!(
                "capture:{}:{b}",
                self.inputs
                    .get(*i)
                    .map_or_else(|| i.to_string(), |x| x.settings.label.clone())
            ),
            CarrierPick::Loaded(src, _) => loop_spec(src),
        }
    }

    fn vocoder_notes_text(&self) -> String {
        notes_text(&self.vocoder)
    }

    /// Keep the loop modulator at the master tempo (sends on change; [`sync_history_tempo`]
    /// calls it about every 10 ms).
    ///
    /// [`sync_history_tempo`]: Engine::sync_history_tempo
    pub fn sync_vocoder_tempo(&mut self) {
        let at = self.vocoder.at;
        for k in 0..MAX_VOCODERS {
            if self.vocoder.instances[k].active {
                self.vocoder.at = k;
                self.sync_vocoder_tempo_one();
            }
        }
        self.vocoder.at = at;
    }

    fn sync_vocoder_tempo_one(&mut self) {
        let (_, bpm) = self.master_tempo();
        let rate = |l: &LoopSync| {
            let tempo = if l.bpm > 0.0 { bpm / l.bpm } else { 1.0 };
            tempo * l.rate_sr
        };
        if matches!(self.vocoder.modulator, ModulatorSource::Loop(_)) {
            let r = rate(&self.vocoder.mod_loop);
            if (r - self.vocoder.mod_loop.last_rate).abs() > 1e-4 {
                self.vocoder.mod_loop.last_rate = r;
                self.vocoder_send(VocoderMsg::LoopRate(r));
            }
        }
        if matches!(self.vocoder.carrier, CarrierSource::Loop(_)) {
            let r = rate(&self.vocoder.car_loop);
            if (r - self.vocoder.car_loop.last_rate).abs() > 1e-4 {
                self.vocoder.car_loop.last_rate = r;
                self.vocoder_send(VocoderMsg::CarrierLoopRate(r));
            }
        }
    }

    /// The selected vocoder as JSON: its knobs, sources, notes, levels and bands, and what it
    /// can pick.
    pub fn vocoder_json(&self) -> JsonValue {
        self.vocoder_json_of(self.vocoder.at)
    }

    /// Every instance in use, and which is selected: `{"selected", "max", "instances": [...]}`,
    /// each instance as [`vocoder_json_of`](Self::vocoder_json_of).
    pub fn vocoders_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("selected", JsonValue::int(self.vocoder.at as i64));
        o.set("max", JsonValue::int(MAX_VOCODERS as i64));
        o.set(
            "instances",
            JsonValue::Array(
                self.vocoder
                    .active()
                    .into_iter()
                    .map(|k| self.vocoder_json_of(k))
                    .collect(),
            ),
        );
        JsonValue::Object(o)
    }

    /// Instance `k` as JSON (`instance` and `label` first).
    pub fn vocoder_json_of(&self, k: usize) -> JsonValue {
        let v = &self.vocoder.instances[k.min(MAX_VOCODERS - 1)];
        let (m, c, l) = v.mirror.levels();
        let r3 = |x: f32| JsonValue::from((x as f64 * 1000.0).round() / 1000.0);
        let mut o = JsonObject::new();
        o.set("instance", JsonValue::int(k as i64));
        o.set("label", vocoder_label(k).into());
        o.set("settings", v.settings.to_json());
        o.set("modulator", v.modulator.to_json());
        o.set("carrier", v.carrier.to_json());
        o.set(
            "notes",
            JsonValue::Array(v.notes.iter().map(|n| JsonValue::int(*n as i64)).collect()),
        );
        o.set("note_names", notes_text(v).into());
        // the synth voice in words, and its presets
        let mut sy = JsonObject::new();
        for (k, x) in v.settings.synth.osc.iter().enumerate() {
            sy.set(&format!("osc{}_wave", k + 1), x.wave.name().into());
        }
        sy.set(
            "preset",
            v.preset.clone().map_or(JsonValue::Null, JsonValue::from),
        );
        sy.set(
            "presets",
            JsonValue::Array(
                self.synth_preset_names()
                    .into_iter()
                    .map(|(n, user)| {
                        let mut p = JsonObject::new();
                        p.set("name", n.into());
                        p.set("user", user.into());
                        JsonValue::Object(p)
                    })
                    .collect(),
            ),
        );
        o.set("synth", JsonValue::Object(sy));
        let mut lv = JsonObject::new();
        lv.set("modulator", r3(m));
        lv.set("carrier", r3(c));
        lv.set("output", r3(l));
        lv.set("sibilance", r3(v.mirror.sibilance()));
        o.set("levels", JsonValue::Object(lv));
        let n = v.settings.bands;
        let shift = 2f64.powf(v.settings.shift / 12.0);
        let peaks = v.mirror.peaks(n);
        o.set(
            "bands",
            JsonValue::Array(
                band_centers(n, self.sample_rate)
                    .into_iter()
                    .zip(v.mirror.envelopes(n))
                    .zip(peaks)
                    .map(|((hz, e), pk)| {
                        let mut b = JsonObject::new();
                        b.set("hz", JsonValue::from(hz));
                        // where the shift moves the carrier's band
                        b.set("carrier_hz", JsonValue::from((hz * shift).round()));
                        b.set("env", r3(e));
                        b.set("peak", r3(pk));
                        b.set("db", JsonValue::from(band_db(e)));
                        JsonValue::Object(b)
                    })
                    .collect(),
            ),
        );
        o.set("frozen", v.settings.freeze.into());
        if matches!(v.modulator, ModulatorSource::Loop(_)) {
            o.set("loop_position", r3(v.mirror.loop_position()));
        }
        if matches!(v.carrier, CarrierSource::Loop(_)) {
            o.set("carrier_loop_position", r3(v.mirror.carrier_loop_position()));
        }
        o.set("vowel_name", vowel_name(v.settings.vowel).into());
        let insert_ok = matches!(v.carrier, CarrierSource::Input(_) | CarrierSource::Deck(_));
        o.set(
            "routing",
            (if !v.settings.on {
                "off"
            } else if v.settings.insert && insert_ok {
                "insert"
            } else {
                "strip"
            })
            .into(),
        );
        let mut ch = JsonObject::new();
        ch.set(
            "inputs",
            JsonValue::Array(
                self.inputs
                    .iter()
                    .map(|x| x.settings.label.clone().into())
                    .collect(),
            ),
        );
        ch.set(
            "decks",
            JsonValue::Array(self.decks.iter().map(|d| d.name.clone().into()).collect()),
        );
        ch.set(
            "clips",
            JsonValue::Array(
                self.loops
                    .row
                    .clips
                    .iter()
                    .map(|c| c.id.clone().into())
                    .collect(),
            ),
        );
        ch.set(
            "params",
            JsonValue::Array(VocoderParam::ALL.iter().map(|p| p.name().into()).collect()),
        );
        o.set("choices", JsonValue::Object(ch));
        JsonValue::Object(o)
    }
}

impl Engine {
    /// A `vocoderSwitch` press: `op` is on / insert / mute / pfl (toggles), modulator /
    /// carrier (step the source; `_prev` steps back), synth (the synth as the carrier), capture
    /// (the last `beats` of `input` as the modulator loop).
    pub fn vocoder_target(
        &mut self,
        op: &str,
        input: Option<&str>,
        beats: Option<&str>,
    ) -> Result<(), String> {
        self.vocoder_target_with(op, input, beats, None)
    }

    /// [`vocoder_target`](Self::vocoder_target) with a preset's name: `op` preset loads it,
    /// preset_next / preset_prev step through them.
    pub fn vocoder_target_with(
        &mut self,
        op: &str,
        input: Option<&str>,
        beats: Option<&str>,
        preset: Option<&str>,
    ) -> Result<(), String> {
        match op {
            "select" => {
                let k = input
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .ok_or("select: which instance (0..)")?;
                return self.vocoder_select(k);
            }
            "select_next" | "select_prev" => {
                self.vocoder_select_step(if op == "select_prev" { -1 } else { 1 });
                return Ok(());
            }
            "add" => return self.vocoder_add().map(|_| ()),
            "remove" => {
                let k = self.vocoder.at;
                return self.vocoder_remove(k);
            }
            _ => {}
        }
        if let Some(w) = VocoderSwitch::parse(op) {
            let on = !self.vocoder_switch_value(w);
            self.vocoder_switch(w, on);
            return Ok(());
        }
        match op {
            "modulator" => self.vocoder_cycle_modulator(1),
            "modulator_prev" => self.vocoder_cycle_modulator(-1),
            "carrier" => self.vocoder_cycle_carrier(1),
            "carrier_prev" => self.vocoder_cycle_carrier(-1),
            "synth" => self.vocoder_carrier(CarrierPick::Synth),
            "synth_mod" => self.vocoder_modulator(ModulatorPick::Synth),
            "swap" => self.vocoder_swap(),
            "preset" => match preset.filter(|p| !p.is_empty()) {
                Some(p) => self.synth_preset_load(p),
                None => self.synth_preset_step(1),
            },
            "preset_next" => self.synth_preset_step(1),
            "preset_prev" => self.synth_preset_step(-1),
            "capture" => {
                let i = self
                    .capture_input(input.filter(|s| !s.is_empty()))
                    .ok_or("no input strip with a history")?;
                let b = beats.and_then(|b| b.trim().parse::<f64>().ok());
                self.vocoder_modulator(ModulatorPick::Capture(i, b))
            }
            o => Err(format!("no op {o:?}")),
        }
    }
}

fn notes_text(v: &MainVocoder) -> String {
    if v.notes.is_empty() {
        return "none".into();
    }
    v.notes
        .iter()
        .map(|n| note_name(*n))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A band envelope in dB (one decimal), floored at -60: what the window's meter draws.
pub fn band_db(e: f32) -> f64 {
    let db = 20.0 * (e.max(1e-6) as f64).log10();
    (db.max(-60.0) * 10.0).round() / 10.0
}

/// A loop as the socket names it: `clip:ID`, `library:ID`, `capture:LABEL:BEATS`, `file:PATH`.
fn loop_spec(src: &LoopSource) -> String {
    match src.kind.as_str() {
        "capture" => format!("capture:{}:{}", src.id, src.beats),
        k => format!("{k}:{}", src.id),
    }
}

fn step_index(now: Option<usize>, len: usize, dir: i32) -> usize {
    let len = len.max(1) as i64;
    let at = now.map_or(0, |k| k as i64);
    let d = if dir < 0 { -1 } else { 1 };
    (at + d).rem_euclid(len) as usize
}

/// A live op's params object from pairs.
pub(crate) fn live_params<const N: usize>(pairs: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(crate::json::JsonObject::from_pairs(pairs))
}

impl Engine {
    /// The vocoder as it is now, as the `vocoder` live ops that set it up again on a fresh
    /// engine (an arrangement's `start.setup`, docs/HEADLESS.md): the carrier, the modulator,
    /// every knob, the synth's held notes, then the switches. A capture modulator (audio grabbed
    /// from an input's past) has no past to grab at beat 0 and is left out. Empty when the
    /// vocoder is as a fresh engine has it.
    ///
    /// Each instance in use has its own op; another than instance 0 carries its `instance` and
    /// is there even as a fresh one has it (so it is added again).
    pub fn vocoder_setup_ops(&self) -> Vec<crate::session::arrangement::ArrangeOp> {
        self.vocoder
            .active()
            .into_iter()
            .filter_map(|k| self.vocoder_setup_op(k))
            .collect()
    }

    fn vocoder_setup_op(&self, k: usize) -> Option<crate::session::arrangement::ArrangeOp> {
        use crate::fx::vocoder::{CarrierSource, ModulatorSource, VocoderSettings};
        use crate::session::arrangement::ArrangeOp;
        let v = &self.vocoder.instances[k];
        let s = &v.settings;
        let fresh = VocoderSettings::default();
        let mut o = crate::json::JsonObject::new();
        match &v.carrier {
            CarrierSource::None => {}
            CarrierSource::Synth => o.set("carrier", "synth".into()),
            CarrierSource::Input(l) => o.set("carrier", format!("input:{l}").into()),
            CarrierSource::Deck(d) => o.set("carrier", format!("deck:{d}").into()),
            CarrierSource::Loop(l) if l.kind == "clip" || l.kind == "library" => {
                o.set("carrier", format!("{}:{}", l.kind, l.id).into())
            }
            CarrierSource::Loop(_) => {}
        }
        match &v.modulator {
            ModulatorSource::None => {}
            ModulatorSource::Input(l) => o.set("modulator", format!("input:{l}").into()),
            ModulatorSource::Deck(d) => o.set("modulator", format!("deck:{d}").into()),
            ModulatorSource::Synth => o.set("modulator", "synth".into()),
            ModulatorSource::Loop(l) if l.kind == "clip" || l.kind == "library" => {
                o.set("modulator", format!("{}:{}", l.kind, l.id).into())
            }
            ModulatorSource::Loop(_) => {}
        }
        for p in crate::fx::vocoder::VocoderParam::ALL {
            if s.get(p) != fresh.get(p) {
                o.set(p.name(), JsonValue::Number(s.get(p)));
            }
        }
        if !v.notes.is_empty() {
            o.set(
                "notes",
                JsonValue::Array(v.notes.iter().map(|n| JsonValue::int(*n as i64)).collect()),
            );
        }
        for (name, on, was) in [
            ("bypass", s.bypass, fresh.bypass),
            ("freeze", s.freeze, fresh.freeze),
            ("xfade", s.xfade, fresh.xfade),
            ("insert", s.insert, fresh.insert),
            ("mute", s.mute, fresh.mute),
            ("pfl", s.pfl, fresh.pfl),
            ("on", s.on, fresh.on),
        ] {
            if on != was {
                o.set(name, on.into());
            }
        }
        if o.is_empty() && k == 0 {
            return None;
        }
        if k > 0 {
            o.set("instance", JsonValue::int(k as i64));
        }
        Some(ArrangeOp::Live {
            method: "vocoder".into(),
            params: JsonValue::Object(o),
        })
    }
}
