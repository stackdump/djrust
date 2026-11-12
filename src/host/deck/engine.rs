//! The deck mixer: `DJEngine.swift`'s channel strips and crossfader, the headphone cue
//! (`CueRouting.swift`), and the two halves of the threading contract.
//!
//! [`engine`] returns a pair:
//!
//! - [`Renderer`] goes to the audio callback (or is called by hand for offline renders). It
//!   owns every deck's [`DeckCore`], pops [`Msg`]s off an SPSC ring at the top of each block,
//!   and hands whatever the decks let go of back through a retire ring.
//! - [`Engine`] stays on the main thread: it compiles projects off the audio thread
//!   (`WaveRunner::prepare`), numbers their samplers, sends commands and reads the per-deck
//!   mirrors.
//!
//! Channel strip gain = trim x volume x crossfader (A: cos, B: sin, others 1), as
//! `DJEngine.applyGains`; the change is ramped across the block. The headphone feed is
//! `(1 - mix) x (sum of cued decks, pre-fader) + mix x master`, x level (`applyHeadphoneMix`).
//! Each strip runs the deck through its EQ and DJ filter (`Deck.filterNode`,
//! [`crate::fx::strip`]) before the fader and the cue send, as Swift's graph does.
//! A deck is a net deck or, with audio on its platter, a sample deck ([`sample`](super::sample));
//! the beat clock, the cue buttons and relocation work on both ([`beat`](super::beat),
//! [`cues`](super::cues), [`relocate`](super::relocate)). The loop channel `L` is the loop bank
//! beside the decks ([`loops`](super::loops)): its own fader, trim, mute, EQ and DJ filter
//! (`crate::fx::strip`, as a deck's strip), master bus and FX unit, summed into the master
//! after the strips, and addressed like a deck by the controllers and the `DeckAction`s. The
//! input strips are [`input`](super::input); the transport, sync and the mixer as one document
//! are [`mixer`](super::mixer). The strip's reverb (`DeckReverb`) sits after the EQ and the
//! capture tap, before the fader and the cue send, as in Swift's graph; Swift's unit is Apple's
//! AUReverb2, here an FDN room tuned from the same parameters ([`StripReverb`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
#[cfg(feature = "sf2")]
use std::sync::Mutex;

use crate::deck::beat::Quantize;
use crate::deck::snapshot::RingShape;
use crate::deck::turntable::{SampleBuffer, TurntableCommand};
use crate::fx::unit::FxUnitSettings;
use crate::fx::{
    LoopMasterCommand, MasterFxKey, MasterMacro, ReverbKnob, StripEq, StripKnob, StripReverb,
    StripSettings,
};
use crate::pflow::Project;
use crate::session::recorder::SessionRecorder;
use crate::spec::TrackSpec;
use crate::wave::net::str_hash;
use crate::wave::{NetMacro, VoiceLaneParams, VoiceSetting, WaveOptions, WaveProject};
use rtrb::{Consumer, Producer, RingBuffer};

use super::core::{Command, DeckCore, DeckMirror};
use super::fxdeck::LoopChannel;
use super::loops::{LoopBank, LoopMsg, LoopRow, LOOP_LEAD};
use super::perform::{JournalSink, SampleResolver};
#[cfg(feature = "sf2")]
use super::samplers::SamplerBank;
use super::samplers::MAX_CHUNK;

/// Most frames the mixer renders in one pass (longer buffers are chunked).
pub const MAX_BLOCK: usize = MAX_CHUNK;
const COMMAND_CAPACITY: usize = 512;
const RETIRE_CAPACITY: usize = 32;

/// Main thread to render thread.
///
/// `Deck(_, Command)` is far larger than the rest, and boxing it would make the render thread
/// free what the main thread allocated: the ring holds the messages by value on purpose.
#[allow(clippy::large_enum_variant)]
pub enum Msg {
    Deck(usize, Command),
    Crossfader(f64),
    Master(f64),
    Volume(usize, f64),
    /// The channel gain knob 0...1 (trim `2^((gain - 0.5) * 2)`).
    Gain(usize, f64),
    Mute(usize, bool),
    Pfl(usize, bool),
    HeadphoneMix(f64),
    HeadphoneLevel(f64),
    /// A strip knob (EQ, filter) 0...1.
    Strip(usize, StripKnob, f64),
    /// A knob of the strip's reverb (`Deck.reverb.wet / size / tone`) 0...1.
    Reverb(usize, ReverbKnob, f64),
    /// The loop channel.
    Loop(LoopMsg),
    /// An input strip.
    Input(usize, super::input::InputMsg),
    /// A vocoder instance (its index, below [`super::vocoder::MAX_VOCODERS`]).
    Vocoder(usize, super::vocoder::VocoderMsg),
    /// A deck's performance tricks.
    Trick(usize, super::tricks::TrickMsg),
    /// A strip's history and freeze, by its slot (`strip_recall::slot_of`).
    Recall(usize, super::strip_recall::RecallMsg),
    /// The browser's audition on the cue.
    Audition(super::audition::AuditionMsg),
    /// The aux bus: a strip's send, the return, the sliders, a macro (docs/AUX.md).
    Aux(super::aux::AuxMsg),
    /// The live post chain on the master (None: off), built on the main thread; the one it
    /// replaces goes back through the post retire ring (docs/POST.md).
    Post(Option<Box<crate::fx::post::LivePost>>),
}

/// `Deck.trim`.
pub fn trim_for_gain(gain: f64) -> f64 {
    2f64.powf((gain - 0.5) * 2.0)
}

/// `DJEngine.crossfadeGain`.
pub fn crossfade_gain(deck: &str, crossfader: f64) -> f64 {
    match deck {
        "A" => (crossfader * std::f64::consts::FRAC_PI_2).cos(),
        "B" => (crossfader * std::f64::consts::FRAC_PI_2).sin(),
        _ => 1.0,
    }
}

pub(super) struct Strip {
    pub(super) core: DeckCore,
    eq: StripEq,
    tricks: super::tricks::TrickChain,
    /// The strip's reverb, after the EQ and the capture tap, before the fader and the cue send
    /// (`DeckReverb`).
    reverb: StripReverb,
    l: Vec<f32>,
    r: Vec<f32>,
    volume: f64,
    trim: f64,
    mute: bool,
    pfl: bool,
    last_gain: f64,
}

/// The render thread's half.
pub struct Renderer {
    pub sample_rate: f64,
    rx: Consumer<Msg>,
    retire_tx: Producer<WaveProject>,
    retire_buffers_tx: Producer<Arc<SampleBuffer>>,
    /// Samplers a deck's slots replaced (the bank evicted what they played): freed by the main
    /// thread, as the projects are.
    #[cfg(feature = "sf2")]
    retire_samplers_tx: Producer<crate::host::sampler::Sf2Sampler>,
    strips: Vec<Strip>,
    crossfader: f64,
    master: f64,
    hp_mix: f64,
    hp_level: f64,
    cue_l: Vec<f32>,
    cue_r: Vec<f32>,
    /// Frames rendered through this renderer (every deck's time base).
    frames: i64,
    /// Projects the retire ring had no room for, dropped on the render thread.
    pub retire_overflow: u64,
    master_level: Arc<AtomicU32>,
    loops: LoopBank,
    inputs: Vec<super::input::InputStrip>,
    /// Every vocoder instance ([`super::vocoder::MAX_VOCODERS`], made up front); 0 is `V`.
    vocoders: Vec<super::vocoder::VocoderStrip>,
    /// A pump's source deck, mono, for the block.
    side: Vec<f32>,
    /// Silence, for a tap with nothing to write.
    zeros: Vec<f32>,
    /// Each strip's history and freeze (the decks, `L`, `V`, `M`; empty without them).
    recalls: Vec<super::strip_recall::StripRecall>,
    /// The loop row's block, on its own while its history takes it.
    loop_l: Vec<f32>,
    loop_r: Vec<f32>,
    /// `V` and `M` lately (loop grab / record from a bus).
    bus: Arc<super::capture::BusTaps>,
    /// Each strip's share of the master, kept for an offline render's stems
    /// ([`enable_stems`](Renderer::enable_stems)). None live: nothing is kept.
    stems: Option<Stems>,
    /// The browser's audition, on the cue only.
    audition: super::audition::AuditionVoice,
    /// The aux bus, the sends into it and the macros in flight (docs/AUX.md).
    aux: super::aux::AuxRender,
    /// The post chain on the master, before the master level; off by default (docs/POST.md).
    post: super::post_bus::PostRender,
}

/// What each part of the mixer added to the master, block by block, after the master level:
/// the decks (post-fader, after the crossfader; a deck the vocoder replaces as its carrier
/// carries the vocoded signal), the loop row, each input strip and the vocoder's own output. The
/// stems add up to the master mix (to float rounding). Only an offline render keeps them
/// ([`Renderer::enable_stems`]): they grow by every block rendered.
#[derive(Clone, Debug, Default)]
pub struct Stems {
    /// `A`, `B`, …, `loops`, `input:<index>`, `vocoder`, in the mixer's order.
    pub names: Vec<String>,
    pub l: Vec<Vec<f32>>,
    pub r: Vec<Vec<f32>>,
}

impl Stems {
    fn push(&mut self, k: usize, l: &[f32], r: &[f32]) {
        self.l[k].extend_from_slice(l);
        self.r[k].extend_from_slice(r);
    }

    /// Add what `out` gained since `before` as stem `k`.
    fn push_diff(&mut self, k: usize, before: (&[f32], &[f32]), out: (&[f32], &[f32])) {
        self.l[k].extend(out.0.iter().zip(before.0).map(|(a, b)| a - b));
        self.r[k].extend(out.1.iter().zip(before.1).map(|(a, b)| a - b));
    }

    /// The last `n` frames of every stem times `g` (the master level).
    fn scale_tail(&mut self, n: usize, g: f32) {
        for v in self.l.iter_mut().chain(self.r.iter_mut()) {
            let at = v.len() - n;
            for x in &mut v[at..] {
                *x *= g;
            }
        }
    }
}

impl Renderer {
    /// Every compiled project the decks' runners hold, taken out (a renderer being freed a
    /// piece at a time: [`Teardown`](super::teardown::Teardown)). The decks play nothing after.
    pub fn take_projects(&mut self) -> Vec<WaveProject> {
        self.strips.iter_mut().flat_map(|s| s.core.runner.take_projects()).collect()
    }

    /// Render `out_l.len()` frames of the master mix. Real-time safe.
    pub fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.render_with_cue(out_l, out_r, None);
    }

    /// The master mix and, if asked, the headphone feed.
    pub fn render_with_cue(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        mut cue: Option<(&mut [f32], &mut [f32])>,
    ) {
        let total = out_l.len().min(out_r.len());
        let mut at = 0;
        while at < total {
            let n = (total - at).min(MAX_BLOCK);
            let cue_part = cue
                .as_mut()
                .map(|(l, r)| (&mut l[at..at + n], &mut r[at..at + n]));
            self.render_chunk(&mut out_l[at..at + n], &mut out_r[at..at + n], cue_part);
            at += n;
        }
    }

    fn apply(&mut self, m: Msg) {
        match m {
            Msg::Deck(i, c) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.core.apply(c);
                }
            }
            Msg::Crossfader(v) => self.crossfader = v.clamp(0.0, 1.0),
            Msg::Master(v) => self.master = v.clamp(0.0, 1.0),
            Msg::Volume(i, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.volume = v.clamp(0.0, 1.0);
                }
            }
            Msg::Gain(i, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.trim = trim_for_gain(v.clamp(0.0, 1.0));
                }
            }
            Msg::Mute(i, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.mute = v;
                }
            }
            Msg::Pfl(i, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.pfl = v;
                }
            }
            Msg::HeadphoneMix(v) => self.hp_mix = v.clamp(0.0, 1.0),
            Msg::HeadphoneLevel(v) => self.hp_level = v.clamp(0.0, 1.0),
            Msg::Strip(i, k, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.eq.set(k, v);
                }
            }
            Msg::Reverb(i, k, v) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.reverb.set(k, v);
                }
            }
            Msg::Loop(m) => self.loops.apply(m),
            Msg::Input(i, m) => {
                if let Some(s) = self.inputs.get_mut(i) {
                    s.apply(m);
                }
            }
            Msg::Trick(i, m) => {
                if let Some(s) = self.strips.get_mut(i) {
                    s.tricks.apply(m);
                }
            }
            Msg::Recall(k, m) => {
                if let Some(r) = self.recalls.get_mut(k) {
                    r.apply(m);
                }
            }
            Msg::Aux(m) => self.aux.apply(m, &mut self.strips),
            Msg::Post(p) => self.post.apply(p),
            Msg::Audition(m) => {
                if let Some(b) = self.audition.apply(m) {
                    if let Err(rtrb::PushError::Full(b)) = self.retire_buffers_tx.push(b) {
                        self.retire_overflow += 1;
                        drop(b);
                    }
                }
            }
            Msg::Vocoder(k, m) => {
                let Some(v) = self.vocoders.get_mut(k) else {
                    return;
                };
                if let Some(b) = v.apply(m) {
                    if let Err(rtrb::PushError::Full(b)) = self.retire_buffers_tx.push(b) {
                        self.retire_overflow += 1;
                        drop(b);
                    }
                }
            }
        }
    }

    fn render_chunk(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        cue: Option<(&mut [f32], &mut [f32])>,
    ) {
        let n = out_l.len();
        while let Ok(m) = self.rx.pop() {
            self.apply(m);
        }
        for v in out_l.iter_mut() {
            *v = 0.0;
        }
        for v in out_r.iter_mut() {
            *v = 0.0;
        }
        let want_cue = cue.is_some();
        // the aux macros where this block finds them, and the bus's send block cleared
        self.aux.begin(self.frames, n, &mut self.strips);
        if want_cue {
            for v in self.cue_l[..n].iter_mut() {
                *v = 0.0;
            }
            for v in self.cue_r[..n].iter_mut() {
                *v = 0.0;
            }
        }
        // the crossfader on the vocoder (docs/VOCODER.md): the decks play as at no crossfader
        let xf_decks = !self.vocoders.iter().any(|v| v.owns_crossfader());
        let xf = self.crossfader;
        // every strip's pre-fader signal first, so the vocoder can read its sources and, as an
        // insert, replace its carrier's before that strip's fader, cue and loop record
        for s in self.strips.iter_mut() {
            s.core.render(&mut s.l[..n], &mut s.r[..n], self.frames);
            s.eq.process(&mut s.l[..n], &mut s.r[..n]);
        }
        // the tricks, once every deck has its block: a pump listens to another deck's (after that
        // deck's own tricks when it comes earlier in the mixer, before them when it comes later)
        for k in 0..self.strips.len() {
            let side = match self.strips[k].tricks.side_source() {
                Some(j) if j < self.strips.len() => {
                    let src = &self.strips[j];
                    for i in 0..n {
                        self.side[i] = 0.5 * (src.l[i] + src.r[i]);
                    }
                    true
                }
                _ => false,
            };
            let s = &mut self.strips[k];
            let side = side.then_some(&self.side[..n]);
            s.tricks
                .process(&mut s.l[..n], &mut s.r[..n], self.frames, side);
            s.core
                .mirror
                .tricks
                .store(s.tricks.sounding(), Ordering::Relaxed);
            // its history and freeze, before the vocoder, the cue and the fader hear it
            if let Some(r) = self.recalls.get_mut(k) {
                r.process(&mut s.l[..n], &mut s.r[..n], self.frames);
            }
        }
        for s in self.inputs.iter_mut() {
            s.pull(n, self.frames);
        }
        for k in 0..self.vocoders.len() {
            self.vocoder_pass(n, k);
        }
        let nd = self.strips.len();
        if let Some(r) = self.recalls.get_mut(nd + 1) {
            // V before its fader (silence while it makes nothing, so the history runs on)
            let v = &mut self.vocoders[0];
            if v.made == super::vocoder::Made::Nothing {
                v.l[..n].fill(0.0);
                v.r[..n].fill(0.0);
            }
            r.process(&mut v.l[..n], &mut v.r[..n], self.frames);
        }
        {
            let v = &self.vocoders[0];
            let (l, r) = if v.made == super::vocoder::Made::Nothing {
                (&self.zeros[..n], &self.zeros[..n])
            } else {
                (&v.l[..n], &v.r[..n])
            };
            self.bus.vocoder_pre.write(self.frames, l, r);
        }
        let mut strip_k = 0usize;
        for s in self.strips.iter_mut() {
            s.core
                .mirror
                .capture
                .write(self.frames, &s.l[..n], &s.r[..n]);
            // the strip's reverb: LOOP record has just heard the deck dry, the fader and the cue
            // send hear the room (Swift's source -> EQ -> reverb -> strip, with the tap on the EQ)
            s.reverb.process(&mut s.l[..n], &mut s.r[..n]);
            let peak = s.l[..n]
                .iter()
                .chain(&s.r[..n])
                .fold(0f32, |p, v| p.max(v.abs()));
            s.core.mirror.publish_level(peak);
            while let Some(p) = s.core.runner.pop_retired() {
                if let Err(rtrb::PushError::Full(p)) = self.retire_tx.push(p) {
                    self.retire_overflow += 1;
                    drop(p);
                }
            }
            while let Some(b) = s.core.pop_retired_buffer() {
                if let Err(rtrb::PushError::Full(b)) = self.retire_buffers_tx.push(b) {
                    self.retire_overflow += 1;
                    drop(b);
                }
            }
            #[cfg(feature = "sf2")]
            while let Some(x) = s.core.samplers.pop_retired() {
                if let Err(rtrb::PushError::Full(x)) = self.retire_samplers_tx.push(x) {
                    self.retire_overflow += 1;
                    drop(x);
                }
            }
            let target = if s.mute {
                0.0
            } else {
                s.trim * s.volume * if xf_decks { crossfade_gain(&s.core.name, xf) } else { 1.0 }
            };
            let (g0, g1) = (s.last_gain, target);
            s.last_gain = target;
            let step = (g1 - g0) / n as f64;
            for i in 0..n {
                let g = (g0 + step * (i + 1) as f64) as f32;
                out_l[i] += g * s.l[i];
                out_r[i] += g * s.r[i];
            }
            // the aux send is post-fader (and post-crossfader, and silent when muted)
            self.aux
                .add(super::aux::AuxTarget::Deck(strip_k), n, &s.l[..n], &s.r[..n], g0, step);
            if let Some(st) = self.stems.as_mut() {
                let k = strip_k;
                let gl: Vec<f32> = (0..n)
                    .map(|i| (g0 + step * (i + 1) as f64) as f32 * s.l[i])
                    .collect();
                let gr: Vec<f32> = (0..n)
                    .map(|i| (g0 + step * (i + 1) as f64) as f32 * s.r[i])
                    .collect();
                st.push(k, &gl, &gr);
            }
            strip_k += 1;
            if want_cue && s.pfl {
                for i in 0..n {
                    self.cue_l[i] += s.l[i];
                    self.cue_r[i] += s.r[i];
                }
            }
        }
        let mut before = self
            .stems
            .as_ref()
            .map(|_| (out_l[..n].to_vec(), out_r[..n].to_vec()));
        let ns = self.strips.len();
        // L on its own: through its history (when strips keep one), into its tap, then the mix
        self.loop_l[..n].fill(0.0);
        self.loop_r[..n].fill(0.0);
        self.loops
            .render(self.frames, &mut self.loop_l[..n], &mut self.loop_r[..n]);
        if let Some(r) = self.recalls.get_mut(nd) {
            r.process(&mut self.loop_l[..n], &mut self.loop_r[..n], self.frames);
        }
        self.bus
            .loops
            .write(self.frames, &self.loop_l[..n], &self.loop_r[..n]);
        for i in 0..n {
            out_l[i] += self.loop_l[i];
            out_r[i] += self.loop_r[i];
        }
        self.aux.add(
            super::aux::AuxTarget::Loop,
            n,
            &self.loop_l[..n],
            &self.loop_r[..n],
            1.0,
            0.0,
        );
        if let (Some(st), Some((bl, br))) = (self.stems.as_mut(), before.as_mut()) {
            st.push_diff(ns, (bl, br), (&out_l[..n], &out_r[..n]));
            bl.copy_from_slice(&out_l[..n]);
            br.copy_from_slice(&out_r[..n]);
        }
        while let Some(b) = self.loops.pop_retired() {
            if let Err(rtrb::PushError::Full(b)) = self.retire_buffers_tx.push(b) {
                self.retire_overflow += 1;
                drop(b);
            }
        }
        for (k, s) in self.inputs.iter_mut().enumerate() {
            let ramp = self.aux.input_ramp(k);
            s.mix(
                &mut out_l[..n],
                &mut out_r[..n],
                Some((&mut self.aux.l[..n], &mut self.aux.r[..n], ramp)),
            );
            if ramp.0 != 0.0 || ramp.1 != 0.0 {
                self.aux.sent = true;
            }
            if let (Some(st), Some((bl, br))) = (self.stems.as_mut(), before.as_mut()) {
                st.push_diff(ns + 1 + k, (bl, br), (&out_l[..n], &out_r[..n]));
                bl.copy_from_slice(&out_l[..n]);
                br.copy_from_slice(&out_r[..n]);
            }
            if want_cue && s.pfl && s.pulled == n {
                for i in 0..n {
                    self.cue_l[i] += s.l[i];
                    self.cue_r[i] += s.r[i];
                }
            }
        }
        for v in self.vocoders.iter_mut() {
            let cue_v = if want_cue {
                Some((&mut self.cue_l[..n], &mut self.cue_r[..n]))
            } else {
                None
            };
            v.mix(n, &mut out_l[..n], &mut out_r[..n], cue_v);
        }
        if let (Some(st), Some((bl, br))) = (self.stems.as_mut(), before.as_mut()) {
            let k = st.names.len() - 2;
            st.push_diff(k, (bl, br), (&out_l[..n], &out_r[..n]));
            bl.copy_from_slice(&out_l[..n]);
            br.copy_from_slice(&out_r[..n]);
        }
        // the aux return joins the master, before the master level
        self.aux.finish(n, out_l, out_r);
        if let (Some(st), Some((bl, br))) = (self.stems.as_mut(), before.as_ref()) {
            let k = st.names.len() - 1;
            st.push_diff(k, (bl, br), (&out_l[..n], &out_r[..n]));
        }
        // the browser's audition: the cue only, never the master
        self.audition.mix(
            n,
            want_cue.then(|| (&mut self.cue_l[..n], &mut self.cue_r[..n])),
        );
        self.bus.vocoder.write(
            self.frames,
            &self.vocoders[0].post_l[..n],
            &self.vocoders[0].post_r[..n],
        );
        // the post chain (off: nothing), before the master level so the fader still turns the
        // finished sound down
        self.post.process(&mut out_l[..n], &mut out_r[..n]);
        let m = self.master as f32;
        for i in 0..n {
            out_l[i] *= m;
            out_r[i] *= m;
        }
        if let Some(r) = self.recalls.get_mut(nd + 2) {
            r.process(&mut out_l[..n], &mut out_r[..n], self.frames);
        }
        let mut peak = 0f32;
        for i in 0..n {
            peak = peak.max(out_l[i].abs()).max(out_r[i].abs());
        }
        self.master_level.store(peak.to_bits(), Ordering::Relaxed);
        if let Some(st) = self.stems.as_mut() {
            st.scale_tail(n, m);
        }
        self.bus.master.write(self.frames, &out_l[..n], &out_r[..n]);
        if let Some((cl, cr)) = cue {
            let (mix, level) = (self.hp_mix as f32, self.hp_level as f32);
            for i in 0..n {
                cl[i] = level * ((1.0 - mix) * self.cue_l[i] + mix * out_l[i]);
                cr[i] = level * ((1.0 - mix) * self.cue_r[i] + mix * out_r[i]);
            }
        }
        self.frames += n as i64;
    }

    /// The vocoder between the strips' pre-fader signals and their faders: its sources are
    /// tapped there and, as an insert, its output written over its carrier's. Instance `k`.
    fn vocoder_pass(&mut self, n: usize, k: usize) {
        use super::vocoder::{CarTap, ModTap};
        let v = &mut self.vocoders[k];
        let modulator = match v.modt {
            ModTap::Input(i) => self.inputs.get(i).map(|s| (&s.l[..n], &s.r[..n])),
            // a deck's signal before its fader (after its EQ and tricks), as the carrier taps it
            ModTap::Deck(i) => self.strips.get(i).map(|s| (&s.l[..n], &s.r[..n])),
            _ => None,
        };
        // the modulator is copied out first, so one strip can be both
        let Some(has_mod) = v.begin(n, modulator) else {
            return;
        };
        let carrier = match v.cart {
            CarTap::Input(i) => self
                .inputs
                .get_mut(i)
                .map(|s| (&mut s.l[..n], &mut s.r[..n])),
            CarTap::Deck(i) => self
                .strips
                .get_mut(i)
                .map(|s| (&mut s.l[..n], &mut s.r[..n])),
            _ => None,
        };
        v.finish(n, has_mod, carrier, self.crossfader);
    }

    /// Keep each strip's share of the master from the next block on ([`Stems`]): for an offline
    /// render only (the stems grow by every block, and allocate on the render thread).
    pub fn enable_stems(&mut self) {
        let mut names: Vec<String> = self.strips.iter().map(|s| s.core.name.clone()).collect();
        names.push("loops".into());
        names.extend((0..self.inputs.len()).map(|k| format!("input:{k}")));
        names.push("vocoder".into());
        names.push(super::aux::AUX_NAME.into());
        let n = names.len();
        self.stems = Some(Stems {
            names,
            l: vec![Vec::new(); n],
            r: vec![Vec::new(); n],
        });
    }

    /// The stems kept so far, emptied (they keep being kept); None when not enabled.
    pub fn take_stems(&mut self) -> Option<Stems> {
        let st = self.stems.as_mut()?;
        let n = st.names.len();
        Some(Stems {
            names: st.names.clone(),
            l: std::mem::replace(&mut st.l, vec![Vec::new(); n]),
            r: std::mem::replace(&mut st.r, vec![Vec::new(); n]),
        })
    }

    /// Frames rendered so far (every deck's time base; [`Engine::frame`] reads the same count).
    pub fn frames(&self) -> i64 {
        self.frames
    }

    /// Apply every command waiting on the ring now, without rendering and without moving time:
    /// what `render_block` does at its top. For a host that is not rendering yet (the offline
    /// conductor between the phases of a restore, which can queue more than the ring holds). Not
    /// for the audio callback.
    pub fn drain_commands(&mut self) {
        while let Ok(m) = self.rx.pop() {
            self.apply(m);
        }
    }

    /// A deck strip's reverb as the render thread holds it (tests, measurement).
    pub fn strip_reverb(&self, name: &str) -> Option<&StripReverb> {
        self.strips
            .iter()
            .find(|s| s.core.name == name)
            .map(|s| &s.reverb)
    }

    /// The loop channel's reverb as the render thread holds it (tests, measurement).
    pub fn loop_reverb(&self) -> &StripReverb {
        self.loops.reverb()
    }

    /// The deck cores (tests, measurement).
    pub fn deck(&self, name: &str) -> Option<&DeckCore> {
        self.strips
            .iter()
            .find(|s| s.core.name == name)
            .map(|s| &s.core)
    }

    /// [`deck`](Self::deck), mutably: a test sets `runner.record_events` to read the fires and
    /// anchors a deck plays (`OfflineConductor`'s anchors). Only between blocks.
    pub fn deck_mut(&mut self, name: &str) -> Option<&mut DeckCore> {
        self.strips
            .iter_mut()
            .find(|s| s.core.name == name)
            .map(|s| &mut s.core)
    }
}

/// What the main thread keeps about a deck.
pub struct MainDeck {
    pub name: String,
    pub index: usize,
    pub mirror: Arc<DeckMirror>,
    pub(crate) opts: WaveOptions,
    /// The project last loaded (the one a queued swap will make current).
    pub project: Option<Project>,
    /// The spec it was composed from, if it was (`Regenerate`, the seed wheel).
    pub spec: Option<TrackSpec>,
    pub seed: f64,
    pub sections: Vec<SectionInfo>,
    /// Role name -> net indices, in `ROLE_ORDER`, for the roles the song has.
    pub(crate) roles: Vec<(String, Vec<usize>)>,
    /// Roles the performer disabled: left out of every project loaded from now on (they are in
    /// the deck's role filter, `opts.role_filter`, too: `Deck.setRoleDisabled`).
    pub disabled_roles: Vec<String>,
    pub fx_unit: FxUnitSettings,
    pub fx_killed: bool,
    pub master_fx_set: BTreeMap<u8, i32>,
    pub volume: f64,
    pub gain: f64,
    /// The strip's EQ and filter knobs.
    pub strip: StripSettings,
    pub pfl: bool,
    /// The aux send, 0...1 (docs/AUX.md).
    pub aux: f64,
    /// The tempo fader's travel (0.08 = ±8 %, Swift's `Deck.pitchRange`; wider is the hub's).
    pub pitch_range: f64,
    pub auto_advance: bool,
    /// Sample loads are levelled to the reference (`Deck.autoGain`; `Engine::set_auto_gain`).
    pub auto_gain: bool,
    /// The project's own tempo (a nudge is relative to it).
    pub base_tempo: f64,
    /// The deck's whole tempo ratio over its base, less one: the fader and the ring together
    /// (`(1 + pitch x 0.08)(1 + net_nudge) - 1`). Kept in step by [`Engine::set_pitch`] and
    /// [`Engine::nudge`] for readers that want the one number (the loop bank's stopped-deck
    /// tempo, the app's slider); Swift has the two apart, and so do [`pitch`](Self::pitch) and
    /// [`net_nudge`](Self::net_nudge), the fields to set.
    pub nudge: f64,
    /// The tempo fader, -1...1, +-`PITCH_RANGE` (`Deck.pitch`). It stays through a new track.
    pub pitch: f64,
    /// The momentary nudge: the jog ring, the joystick (`Deck.netNudge`).
    pub net_nudge: f64,
    /// Snap cues and loops to the beat grid on a sample deck (`Deck.quantize`).
    pub quantize: Quantize,
    /// The deck whose tempo this one tracks (`Deck.leader`; [`Engine::follow`]).
    pub leader: Option<String>,
    /// The main thread's view of the transport (`Deck.playing`): set the moment a play or a
    /// pause is sent, where the mirror's `playing` follows a block later. A song that ends
    /// clears it ([`Engine::service_beat`]).
    pub playing: bool,
    /// Hot cues by slot (`Deck.hotCues`): samples on a sample deck, ticks on a net deck. A new
    /// track clears them.
    pub hot_cues: BTreeMap<i64, f64>,
    /// Where the CUE button returns a net deck to, in ticks (`Deck.netCueTick`).
    pub net_cue_tick: f64,
    /// What is on the platter when the deck is a sample deck (None: a net deck).
    pub sample: Option<super::sample::SampleDeck>,
    /// The roles of the song a ring snapshot shows, with their candidate nets (id and index in
    /// the compiled graph): `ROLE_ORDER` and the four hits (`Deck.netSnapshot`).
    pub(crate) ring_roles: Vec<(String, Vec<(usize, String)>)>,
    /// Ring shapes by net id, filled as a snapshot asks for them (`Deck.ringShapes`).
    pub(crate) ring_shapes: std::cell::RefCell<BTreeMap<String, RingShape>>,
    /// The loop-row clip this deck holds, when it holds one (`Deck.clipID`; `Engine::play_clip`).
    pub clip_id: Option<String>,
    /// Whether the deck's clip FX chain was last told to be on (`Deck.clipFxOn`, main thread's
    /// view).
    pub clip_fx_on: bool,
    /// The strip's mute (`Deck.mute`).
    pub mute: bool,
    /// The strip reverb's knobs (`Deck.reverb`): kept, recorded, and heard through
    /// [`StripReverb`] (a stand-in for Apple's AUReverb2, see `crate::fx::strip_reverb`).
    pub reverb_wet: f64,
    pub reverb_size: f64,
    pub reverb_tone: f64,
    /// The sound panel's settings by channel (`WaveDeckSource.voiceSettings`), kept through the
    /// same song's recompiles and dropped for another.
    pub voice_settings: BTreeMap<i64, VoiceSetting>,
    /// The live keys held: the note pressed -> the scale-locked note that sounds
    /// (`Deck.heldKeys`).
    pub held_keys: BTreeMap<i64, i64>,
    /// The LOOP take in progress on this deck: Some(released) (`DeckView::loop_take`).
    pub loop_take: Option<bool>,
    /// The track pipeline's state: the source, the generation, the next track and the
    /// look-ahead slots, what is composing (`DeckGenerator.swift`).
    pub track: super::track::TrackState,
    /// The deck's walk and the things a walk moves: Wander's state, the STEMS mutes, the layer
    /// list (`Wander.swift`, `DeckPerform.swift`, `DeckLayers.swift`).
    pub walk: super::wander::DeckWalk,
    /// The performance tricks asked on (docs/TRICKS.md).
    pub tricks: super::tricks::DeckTricks,
    /// Auto-next every N bars (beyond Swift: beats' Auto-DJ Regen; `bar_next.rs`).
    pub bar_next: super::bar_next::BarNext,
    #[cfg(feature = "sf2")]
    pub(crate) bank: Option<Arc<Mutex<SamplerBank>>>,
    /// The bank's samplers that wait for the audio thread, taken without the bank's lock (a load
    /// holds it) when the deck is handed a project. The bank's own handle: a bank swapped in whole
    /// behind `bank` would leave it on the old one.
    #[cfg(feature = "sf2")]
    pub(crate) outbox: Option<super::samplers::Outbox>,
}

impl MainDeck {
    /// `Deck.waveOptions`, without the track's seed: how this deck compiles nets (the instrument
    /// table, every musicality fix and the deck's role filter on an extended engine; beats'
    /// parity options on the other).
    pub fn wave_options(&self) -> &WaveOptions {
        &self.opts
    }
}

pub use crate::deck::perform::ROLE_ORDER;
pub use crate::deck::timeline::SectionInfo;

/// A project compiled off the audio thread. The samplers it bound are not in it: they wait in
/// the deck's bank until the deck is handed a project ([`SamplerBank::take_ready`],
/// [`Outbox`](super::samplers::Outbox)).
pub struct Prepared {
    pub project: WaveProject,
    pub(crate) source: Project,
    pub(crate) roles: Vec<(String, Vec<usize>)>,
    pub(crate) ring_roles: Vec<(String, Vec<(usize, String)>)>,
    pub(crate) sections: Vec<SectionInfo>,
    /// What the deck will know of the project once it is loaded (`wave.latest`), read off it
    /// before it goes to the audio thread.
    pub latest: crate::deck::swap::Latest,
}

impl Prepared {
    /// The nets and roles of the compiled song (`WaveProject::graph`).
    pub fn wave(&self) -> &WaveProject {
        &self.project
    }
}

/// Settings of [`engine`].
#[derive(Clone)]
pub struct EngineConfig {
    pub sample_rate: f64,
    /// Deck names in mixer order (`["A", "B"]`).
    pub decks: Vec<String>,
    /// Play the way the app does: `WaveOptions::extended()` (instrument table, limiter,
    /// turntable brake, every musicality fix: the pass on what the deck composes and the runner's
    /// own). Off: beats' wave engine exactly (`tanh` master, no limiter, no fixes).
    pub extended: bool,
    /// The decks' options as given (`djctl render`'s report plays a subset of the fixes and
    /// records the hits): instead of what `extended` picks.
    pub options: Option<WaveOptions>,
    /// Input strips, by label ("TD-3", "PD705"): each gets a ring a capture stream fills.
    pub inputs: Vec<String>,
    /// Every input keeps a history of this many beats (None: no history). docs/INPUT-HISTORY.md.
    pub history_beats: Option<f64>,
    /// The most the window can be widened to, in beats; sizes the rings.
    pub history_max_beats: f64,
    /// Every other strip (the decks, `L`, `V`, `M`) keeps a stereo history of `history_beats`
    /// too, with a freeze (docs/INPUT-HISTORY.md, "Any strip"). Off by default: about 30 MB a
    /// strip, made once here.
    pub strip_history: bool,
    #[cfg(feature = "sf2")]
    pub library: Option<Arc<crate::host::sampler::Library>>,
}

impl EngineConfig {
    pub fn new(sample_rate: f64) -> EngineConfig {
        EngineConfig {
            sample_rate,
            decks: vec!["A".into(), "B".into()],
            extended: true,
            options: None,
            inputs: Vec::new(),
            history_beats: Some(crate::history::DEFAULT_BEATS),
            history_max_beats: crate::history::MAX_BEATS,
            strip_history: false,
            #[cfg(feature = "sf2")]
            library: None,
        }
    }
}

/// The main thread's half.
pub struct Engine {
    pub sample_rate: f64,
    tx: Producer<Msg>,
    retire_rx: Consumer<WaveProject>,
    retire_buffers_rx: Consumer<Arc<SampleBuffer>>,
    retire_post_rx: Consumer<Box<crate::fx::post::LivePost>>,
    /// The live post chain: on / off and which chain (docs/POST.md).
    pub post: super::post_bus::PostState,
    pub post_mirror: Arc<super::post_bus::PostMirror>,
    #[cfg(feature = "sf2")]
    retire_samplers_rx: Consumer<crate::host::sampler::Sf2Sampler>,
    pub decks: Vec<MainDeck>,
    pub focus: String,
    pub(crate) owners: BTreeMap<String, String>,
    pub crossfader: f64,
    pub master: f64,
    pub headphone_mix: f64,
    pub headphone_level: f64,
    /// Commands the ring had no room for.
    pub dropped: u64,
    /// Locked action keys (`DJEngine.lockedActions`).
    pub locked: Vec<String>,
    /// What the surfaces asked for that this host does not do (kept for the monitor).
    pub unhandled: Vec<String>,
    /// Where the controllers' piano keys go (docs/MPK.md).
    pub keys: super::keys::KeyState,
    /// The vocal pads: the folder, the page, the defaults (docs/MPK.md).
    pub vox: super::vox::VoxState,
    /// Registry targets the hub owns, not the engine (a binding preset switch): drained by
    /// the app each frame.
    pub hub_requests: Vec<crate::control::controller::Action>,
    /// The action trace (docs/ACTION-LOG.md): each hand on the engine in words and session ops,
    /// while `Some`. The app drains it.
    pub trace: Option<Vec<super::trace::Traced>>,
    /// Who the next actions come from ("Inpulse browser", "window", "socket"): set by whoever
    /// drives the engine, read by the trace.
    pub trace_via: String,
    /// True while a controller's [`Action`](crate::control::controller::Action) is applied
    /// (`SurfaceHost::perform`): the journal has that move as the MIDI that made it, so a live
    /// op it traces is not journalled again, and the moves that note themselves
    /// ([`Engine::note_live`]) leave it to the action's own words and ops.
    pub(crate) in_surface: bool,
    /// Inside a move that is recorded as a whole ([`Engine::quietly`]): nothing it calls notes
    /// itself, not even the moves that note what only the engine knows.
    pub(crate) quiet: bool,
    /// How many entries the controller action being described traced itself.
    pub(crate) surface_traced: usize,
    /// Projects freed off the render thread.
    pub retired: u64,
    pub(crate) jogs: Vec<super::surface::JogShim>,
    master_level: Arc<AtomicU32>,
    /// The loop row and channel (`LoopRecorder.swift`).
    pub loops: LoopRow,
    /// What is installed here, which a composed track's palette resolves its sounds against
    /// (`TrackSpec.finish`); without one nothing is (`Sounds::nothing`).
    #[cfg(feature = "sf2")]
    pub library: Option<Arc<crate::host::sampler::Library>>,
    /// `DJEngine.deterministic`, replay mode: whatever normally finishes on a background
    /// thread (a swap, a relocation, the next track's compose) completes inline, so the same
    /// inputs give the same audio every time. **On by default, unlike Swift's**: this engine
    /// has always composed and prepared on the calling thread, and a host that wants Swift's
    /// live behavior (the control thread free while a swap composes) turns it off and calls
    /// [`Engine::service`] about every 10 ms, which lands what finished. The two live hosts
    /// (`djapp`, `djctl play`, with real audio) do: they build the engine with
    /// [`host_engine`](super::host::host_engine); replay, render, serve, `--screenshot` and
    /// every test leave it on.
    pub deterministic: bool,
    /// What is installed here, when the host says so (a test: a machine with nothing, or with
    /// everything); else the scanned `library`, else nothing.
    pub installed: Option<Arc<super::wander::InstalledSounds>>,
    /// The composes the engine starts on its own, offloaded to the host (feature `transfer`,
    /// [`Engine::set_offload`]); None: worker threads, or inline when `deterministic`.
    #[cfg(feature = "transfer")]
    pub(crate) offload: Option<super::offload::Offload>,
    /// Records the performance when set (`DJEngine.recorder`; [`Engine::start_session`]).
    pub recorder: Option<SessionRecorder>,
    /// The always-on event log (`DJEngine.journal`): every input the recorder sees, and more.
    pub journal: Option<Box<dyn JournalSink>>,
    /// Resolves a sample's content id to audio (`DJEngine.sampleResolver`: replays, and
    /// `perform(.load)`). Audio loaded through `perform` is kept by content id in
    /// `loops.row.sample_library` (`DJEngine.sampleLibrary`).
    pub sample_resolver: Option<SampleResolver>,
    /// What pokes aimed at the loop channel `L` leave behind (its FX unit, strip knobs).
    pub loop_channel: LoopChannel,
    /// The two-deck autopilot (`DJEngine.autoMix`): its settings, the hand-off counter, where
    /// it is ([`Engine::service_auto_mix`]).
    pub auto_mix: crate::automix::AutoMixState,
    /// What auto-mix keeps besides its state: the mix-out prepared ahead, the test hooks, the
    /// composes in flight.
    pub auto_mix_host: super::automix::AutoMixHost,
    /// The master transport: tempo source and START / STOP.
    pub transport: crate::mixer::Transport,
    pub(crate) tap: crate::mixer::TapTempo,
    /// The input strips (the gear's audio coming back in).
    pub inputs: Vec<super::input::MainInput>,
    /// The deck whose jog wheel can scrub an input's history, and the input (`--history-jog`).
    /// The wheel follows what the deck holds: it scrubs the history only while the deck is
    /// empty or a modifier is held (docs/INPUT-HISTORY.md, [`super::jog_target`]).
    pub history_jog: Option<(String, super::strip_recall::RecallId)>,
    /// Where the aimed deck's wheel goes now: the modifiers held, a touch's latch, the last
    /// target logged.
    pub jog_follow: super::jog_target::JogFollow,
    /// Where captured loops persist (None: they live for the session).
    pub capture_library: Option<crate::host::stores::captures::CaptureLibrary>,
    pub(crate) input_takes: Vec<super::input_capture::InputTake>,
    /// The vocoder instances (docs/VOCODER.md); derefs to the selected one.
    pub vocoder: super::vocoder::VocoderBank,
    /// The TD-3 tricks: accent, slide, the cutoff ride (docs/TRICKS.md).
    pub gear_tricks: super::tricks::GearTricks,
    /// `V` and `M` lately, for loop grab / record from a bus (docs/LOOPS.md "Sources").
    pub(crate) bus: Arc<super::capture::BusTaps>,
    pub(crate) bus_takes: Vec<super::loop_source::BusTake>,
    /// Every strip's history and freeze, main thread's half (empty without them).
    pub strip_recalls: Vec<super::strip_recall::MainRecall>,
    /// The browser's audition on the headphone cue (docs/BROWSER.md).
    pub audition: super::audition::Audition,
    /// Deck copies on their way (docs/COPY-DECK.md).
    pub(crate) copies: Vec<super::copy::CopyJob>,
    /// The aux bus's return fader, mute and sliders (docs/AUX.md); the sends are on the strips.
    pub aux: crate::fx::aux_bus::AuxSettings,
    /// What the render thread says about it.
    pub aux_mirror: Arc<super::aux::AuxMirror>,
}

/// Build a mixer: the main-thread [`Engine`] and the render-thread [`Renderer`].
pub fn engine(config: EngineConfig) -> (Engine, Renderer) {
    let (tx, rx) = RingBuffer::<Msg>::new(COMMAND_CAPACITY);
    let (retire_tx, retire_rx) = RingBuffer::<WaveProject>::new(RETIRE_CAPACITY);
    let (retire_buffers_tx, retire_buffers_rx) =
        RingBuffer::<Arc<SampleBuffer>>::new(RETIRE_CAPACITY);
    let (retire_post_tx, retire_post_rx) =
        RingBuffer::<Box<crate::fx::post::LivePost>>::new(16);
    let post_mirror = Arc::new(super::post_bus::PostMirror::default());
    #[cfg(feature = "sf2")]
    let (retire_samplers_tx, retire_samplers_rx) =
        RingBuffer::<crate::host::sampler::Sf2Sampler>::new(RETIRE_CAPACITY);
    let mut strips = Vec::new();
    let mut decks = Vec::new();
    for (i, name) in config.decks.iter().enumerate() {
        let opts = match &config.options {
            Some(o) => o.clone(),
            None if config.extended => WaveOptions::extended(),
            None => WaveOptions::default(),
        };
        let mirror = Arc::new(DeckMirror::new());
        let core = DeckCore::new(
            name,
            config.sample_rate,
            opts.clone(),
            opts.turntable_brake,
            mirror.clone(),
        );
        strips.push(Strip {
            core,
            eq: StripEq::new(config.sample_rate),
            tricks: super::tricks::TrickChain::new(config.sample_rate),
            reverb: StripReverb::new(config.sample_rate),
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            volume: 0.8,
            trim: 1.0,
            mute: false,
            pfl: false,
            last_gain: 0.0,
        });
        #[cfg(feature = "sf2")]
        let bank = config.library.as_ref().map(|lib| {
            Arc::new(Mutex::new(SamplerBank::new(
                lib.clone(),
                config.sample_rate as u32,
            )))
        });
        #[cfg(feature = "sf2")]
        let outbox = bank.as_ref().map(|b| b.lock().unwrap().outbox());
        decks.push(MainDeck {
            name: name.clone(),
            index: i,
            mirror,
            opts,
            project: None,
            spec: None,
            seed: 0.0,
            sections: Vec::new(),
            roles: Vec::new(),
            disabled_roles: Vec::new(),
            fx_unit: FxUnitSettings::new(),
            fx_killed: false,
            master_fx_set: BTreeMap::new(),
            volume: 0.8,
            gain: 0.5,
            strip: StripSettings::default(),
            pfl: false,
            aux: 0.0,
            pitch_range: crate::deck::beat::PITCH_RANGE,
            auto_advance: false,
            auto_gain: false,
            base_tempo: 120.0,
            nudge: 0.0,
            pitch: 0.0,
            net_nudge: 0.0,
            quantize: Quantize::Off,
            leader: None,
            playing: false,
            hot_cues: BTreeMap::new(),
            net_cue_tick: 0.0,
            sample: None,
            ring_roles: Vec::new(),
            ring_shapes: std::cell::RefCell::new(BTreeMap::new()),
            clip_id: None,
            clip_fx_on: false,
            mute: false,
            reverb_wet: 0.0,
            reverb_size: 0.5,
            reverb_tone: 0.5,
            voice_settings: BTreeMap::new(),
            held_keys: BTreeMap::new(),
            loop_take: None,
            track: Default::default(),
            walk: Default::default(),
            tricks: Default::default(),
            bar_next: Default::default(),
            #[cfg(feature = "sf2")]
            bank,
            #[cfg(feature = "sf2")]
            outbox,
        });
    }
    let master_level = Arc::new(AtomicU32::new(0));
    let focus = decks.first().map(|d| d.name.clone()).unwrap_or_default();
    let (loop_bank, loop_mirror) = LoopBank::new(config.sample_rate);
    let mut input_strips = Vec::new();
    let mut main_inputs = Vec::new();
    for label in &config.inputs {
        // a second of audio: the cushion is ~20 ms, the rest absorbs a stalled UI
        let (tx, rx) = RingBuffer::<f32>::new(config.sample_rate as usize);
        let mirror = Arc::new(super::input::InputMirror::default());
        input_strips.push(super::input::InputStrip::new(
            config.sample_rate,
            rx,
            mirror.clone(),
            config.history_beats.map(|b| (b, config.history_max_beats)),
        ));
        main_inputs.push(super::input::MainInput::new(
            label,
            tx,
            mirror,
            config.history_beats,
        ));
    }
    let vocoder_mirrors: Vec<Arc<super::vocoder::VocoderMirror>> = (0..super::vocoder::MAX_VOCODERS)
        .map(|_| Arc::new(super::vocoder::VocoderMirror::default()))
        .collect();
    let bus = Arc::new(super::capture::BusTaps::default());
    // every strip's history and freeze, made once (docs/INPUT-HISTORY.md, "Any strip")
    let (mut recalls, mut main_recalls) = (Vec::new(), Vec::new());
    if let (true, Some(beats)) = (config.strip_history, config.history_beats) {
        for id in super::strip_recall::strip_ids(config.decks.len()) {
            let m = Arc::new(super::strip_recall::RecallMirror::default());
            recalls.push(super::strip_recall::StripRecall::new(config.sample_rate, beats, m.clone()));
            main_recalls.push(super::strip_recall::MainRecall::new(id, beats, m));
        }
    }
    let audition_mirror = Arc::new(super::audition::AuditionMirror::default());
    let aux_mirror = Arc::new(super::aux::AuxMirror::default());
    let renderer = Renderer {
        sample_rate: config.sample_rate,
        rx,
        retire_tx,
        retire_buffers_tx,
        #[cfg(feature = "sf2")]
        retire_samplers_tx,
        strips,
        crossfader: 0.5,
        master: 1.0,
        hp_mix: crate::mixer::HEADPHONE_MIX,
        hp_level: crate::mixer::HEADPHONE_LEVEL,
        cue_l: vec![0.0; MAX_BLOCK],
        cue_r: vec![0.0; MAX_BLOCK],
        frames: 0,
        retire_overflow: 0,
        master_level: master_level.clone(),
        loops: loop_bank,
        inputs: input_strips,
        vocoders: vocoder_mirrors
            .iter()
            .map(|m| super::vocoder::VocoderStrip::new(config.sample_rate, m.clone()))
            .collect(),
        side: vec![0.0; MAX_BLOCK],
        zeros: vec![0.0; MAX_BLOCK],
        bus: bus.clone(),
        stems: None,
        recalls,
        loop_l: vec![0.0; MAX_BLOCK],
        loop_r: vec![0.0; MAX_BLOCK],
        audition: super::audition::AuditionVoice::new(config.sample_rate, audition_mirror.clone()),
        aux: super::aux::AuxRender::new(
            config.sample_rate,
            config.decks.len(),
            config.inputs.len(),
            aux_mirror.clone(),
        ),
        post: super::post_bus::PostRender::new(config.sample_rate, retire_post_tx, post_mirror.clone()),
    };
    let engine = Engine {
        sample_rate: config.sample_rate,
        tx,
        retire_rx,
        retire_buffers_rx,
        retire_post_rx,
        post: Default::default(),
        post_mirror,
        #[cfg(feature = "sf2")]
        retire_samplers_rx,
        decks,
        focus,
        owners: BTreeMap::new(),
        crossfader: 0.5,
        master: 1.0,
        headphone_mix: crate::mixer::HEADPHONE_MIX,
        headphone_level: crate::mixer::HEADPHONE_LEVEL,
        dropped: 0,
        locked: Vec::new(),
        unhandled: Vec::new(),
        keys: Default::default(),
        vox: Default::default(),
        hub_requests: Vec::new(),
        trace: None,
        trace_via: String::new(),
        retired: 0,
        jogs: Vec::new(),
        master_level,
        loops: LoopRow::new(loop_mirror),
        #[cfg(feature = "sf2")]
        library: config.library.clone(),
        deterministic: true,
        installed: None,
        #[cfg(feature = "transfer")]
        offload: None,
        recorder: None,
        journal: None,
        in_surface: false,
        quiet: false,
        surface_traced: 0,
        sample_resolver: None,
        loop_channel: LoopChannel::default(),
        auto_mix: Default::default(),
        auto_mix_host: Default::default(),
        transport: crate::mixer::Transport::default(),
        tap: crate::mixer::TapTempo::default(),
        inputs: main_inputs,
        history_jog: None,
        jog_follow: Default::default(),
        capture_library: None,
        input_takes: Vec::new(),
        vocoder: super::vocoder::VocoderBank::new(vocoder_mirrors),
        gear_tricks: Default::default(),
        bus,
        bus_takes: Vec::new(),
        strip_recalls: main_recalls,
        audition: super::audition::Audition::new(audition_mirror),
        copies: Vec::new(),
        aux: Default::default(),
        aux_mirror,
    };
    (engine, renderer)
}

impl Engine {
    /// The master and deck bus taps (what a stream server or a recorder reads from).
    #[doc(hidden)]
    pub fn bus_taps(&self) -> Arc<super::capture::BusTaps> {
        self.bus.clone()
    }

    pub fn index(&self, deck: &str) -> Option<usize> {
        self.decks.iter().position(|d| d.name == deck)
    }

    pub fn deck(&self, deck: &str) -> Option<&MainDeck> {
        self.decks.iter().find(|d| d.name == deck)
    }

    pub fn deck_mut(&mut self, deck: &str) -> Option<&mut MainDeck> {
        self.decks.iter_mut().find(|d| d.name == deck)
    }

    pub(crate) fn send_msg(&mut self, m: Msg) -> bool {
        self.send(m)
    }

    pub(crate) fn send(&mut self, m: Msg) -> bool {
        self.drain_retired();
        if self.tx.push(m).is_err() {
            self.dropped += 1;
            return false;
        }
        true
    }

    /// Send a command to a deck.
    pub fn command(&mut self, deck: &str, c: Command) -> bool {
        match self.index(deck) {
            Some(i) => self.send(Msg::Deck(i, c)),
            None => false,
        }
    }

    /// Free projects the render thread has let go of (`drainRetired`).
    pub fn drain_retired(&mut self) {
        while let Ok(p) = self.retire_rx.pop() {
            self.retired += 1;
            drop(p);
        }
        while let Ok(b) = self.retire_buffers_rx.pop() {
            self.retired += 1;
            drop(b);
        }
        while let Ok(p) = self.retire_post_rx.pop() {
            self.retired += 1;
            drop(p);
        }
        #[cfg(feature = "sf2")]
        while let Ok(x) = self.retire_samplers_rx.pop() {
            self.retired += 1;
            drop(x);
        }
    }

    // MARK: compile and load

    /// Compile `project` for `deck` off the render thread, from `start_tick` (a section
    /// launch replays the control nets silently to there), binding sampled instruments
    /// through the deck's bank. Any thread (the bank is behind a mutex).
    pub fn prepare(
        &self,
        deck: &str,
        project: &Project,
        start_tick: i64,
        seed: f64,
    ) -> Option<Prepared> {
        self.prepare_with(deck, project, start_tick, seed, |_| {})
    }

    /// [`prepare`](Self::prepare) with the deck's options adjusted first: `tweak` sees them
    /// after the seed and the disabled roles are in (a relocation's mix-out tail and rest,
    /// `movedOptions`).
    pub fn prepare_with(
        &self,
        deck: &str,
        project: &Project,
        start_tick: i64,
        seed: f64,
        tweak: impl FnOnce(&mut WaveOptions),
    ) -> Option<Prepared> {
        let preparer = self.preparer(deck)?;
        let mut opts = preparer.options(seed);
        tweak(&mut opts);
        Some(preparer.prepare_opts(project, start_tick, opts, None))
    }

    /// Hand a compiled project to the deck: immediately when stopped, at the next bar when
    /// playing (`WaveDeckSource.load`). A new track (`Deck.loadNet`): the hot cues go, the
    /// tempo fader and the ring's nudge stay.
    pub fn load_prepared(&mut self, deck: &str, p: Prepared) -> bool {
        self.hand_over(deck, p, true)
    }

    /// `load_prepared` for a new track, or (`new_track` false) for the same song from another
    /// tick (`Deck.relocate`, which hands the runner a project but leaves the deck's cues).
    /// Either way the tempo is set again behind the load: the runner takes the project's own
    /// when the project goes in, and the fader and the nudge are the deck's (`applyNetTempo`).
    pub(crate) fn hand_over(&mut self, deck: &str, p: Prepared, new_track: bool) -> bool {
        self.hand_over_with(deck, p, new_track, false)
    }

    /// [`hand_over`](Self::hand_over); `keep_voices`: the new track keeps the sound panel's
    /// settings (`loadNet(_:project:keepVoices:)`: the same song with another instrument
    /// picked). A new track without it starts from the instruments' own voices; either way the
    /// settings that stay are set on the project before it goes to the audio thread
    /// (`WaveDeckSource.load`), and a channel whose instrument changed loses its own.
    pub(crate) fn hand_over_with(
        &mut self,
        deck: &str,
        mut p: Prepared,
        new_track: bool,
        keep_voices: bool,
    ) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        if new_track && !keep_voices {
            self.decks[i].voice_settings.clear();
        }
        let sr = self.sample_rate;
        super::net::reapply_voice_settings(&mut self.decks[i].voice_settings, &mut p, sr);
        // every sampler the bank has loaded and the deck does not have yet, not only the ones this
        // project bound: a track prepared beside it (composed ahead, a swap, a restart) found the
        // sources they share numbered and loaded nothing, and if it is the one that goes in first
        // its lanes need them. Sending one early costs nothing: a slot is given to another source
        // only when no project references it, so no project that is live plays the sampler it
        // replaces on the audio thread.
        #[cfg(feature = "sf2")]
        if let Some(outbox) = self.decks[i].outbox.clone() {
            for (slot, s) in outbox.take() {
                self.send(Msg::Deck(i, Command::Sampler(slot, s)));
            }
        }
        let tempo = p.source.tempo;
        if self.decks[i].sample.take().is_some() {
            // a composed track takes the deck back from the platter, and the FX unit moves to the
            // net's chain
            self.send(Msg::Deck(i, Command::SampleMode(false)));
            self.sync_fx_unit(deck);
        }
        // a stopped deck installs the project at once: show its starting mutes before the first
        // render publishes the real ones (`WaveDeckSource.load`)
        if !self.decks[i].mirror.is_playing() {
            self.decks[i].mirror.publish_mutes(&p.project.graph.muted);
        }
        let marking = p.project.marking.clone();
        let ok = self.send(Msg::Deck(i, Command::Load(p.project)));
        if ok {
            // the project's marking mirror and layout are kept for the readers (`remember`)
            self.decks[i].mirror.remember_marking(marking);
            let d = &mut self.decks[i];
            d.project = Some(p.source);
            d.roles = p.roles;
            d.ring_roles = p.ring_roles;
            d.sections = p.sections;
            d.track.latest = Some(p.latest);
            // `netBaseBPM = project.tempo > 0 ? project.tempo : 120`
            d.base_tempo = if tempo > 0.0 { tempo } else { 120.0 };
            if new_track {
                d.ring_shapes.get_mut().clear();
                d.hot_cues.clear();
                d.walk.stems_muted = Default::default(); // STEMS mutes are the track's
            }
            // anything else loading ends a clip's stay (`Deck.clipID = nil`), and its FX with it
            self.clear_deck_clip(deck);
            self.apply_net_tempo(deck);
            // a net loaded onto a deck that is playing plays (`loadNet`: `if playing {
            // wave.play() }`); a relocation (`wave.load` and `applyNetTempo` only) does not start
            // a runner that is not playing, as when the song has just ended and `service_beat`
            // has not yet cleared `playing`
            if new_track && self.decks[i].playing {
                self.send(Msg::Deck(i, Command::Play));
            }
        }
        ok
    }

    /// Compile and load a project from its start (`Deck.loadNet(_ project:)`): a hand-built net
    /// has no spec, no raw-`nets` payload and no pass; the deck's own fixes are its.
    pub fn load(&mut self, deck: &str, project: &Project, seed: f64) -> bool {
        match self.prepare(deck, project, 0, seed) {
            Some(p) => {
                if let Some(d) = self.deck_mut(deck) {
                    d.seed = seed;
                }
                let ok = self.load_prepared(deck, p);
                if let Some(d) = self.deck_mut(deck).filter(|_| ok) {
                    d.spec = None;
                    d.track.raw_nets = None;
                    d.track.fixes = Some(d.opts.musicality);
                    d.track.pass_applied = false;
                    d.track.seed_target = None;
                    d.track.mix_in_end_tick = None;
                }
                ok
            }
            None => false,
        }
    }

    /// `TrackSpec.project()` with the sounds this engine's library has installed (the
    /// palettes' picks), or none when it has no library.
    pub fn project_of(&self, spec: &TrackSpec) -> Project {
        if let Some(i) = &self.installed {
            return spec.project_in(i.sounds());
        }
        #[cfg(feature = "sf2")]
        if let Some(lib) = &self.library {
            return spec.project_in(lib.sounds());
        }
        spec.project()
    }

    /// Compose a spec and load it from its first tick (`Deck.generate`, `SwapStart.top`); the
    /// deck remembers the spec. Blocks until it is loaded; [`Engine::generate_at`] starts
    /// elsewhere, [`Engine::begin_generate`] does not wait.
    pub fn generate(&mut self, deck: &str, spec: &TrackSpec) -> bool {
        self.generate_at(deck, spec, crate::deck::slots::SwapStart::Top)
    }

    /// [`generate`](Self::generate) for a stem (`djctl render --mute / --solo`): the deck's role
    /// filter is `filter` (`Deck.roleFilter`, what `RenderRequest.applied(to:)` sets on the
    /// options), so every net the track compiles to leaves the roles out, and everything else is
    /// as the whole track: the loudness, the balance trims, the energy and the rests are measured
    /// on the whole project, and `Deck::project` stays whole. `None` leaves the deck's filter as
    /// it is.
    pub fn generate_filtered(
        &mut self,
        deck: &str,
        spec: &TrackSpec,
        filter: Option<&crate::wave::RoleFilter>,
    ) -> bool {
        if let (Some(filter), Some(d)) = (filter, self.deck_mut(deck)) {
            d.opts.role_filter = Some(filter.clone()).filter(|f| !f.is_empty());
        }
        self.generate(deck, spec)
    }

    /// `Deck.jumpToSection`: the same project, rebuilt at the start of section `index`, swapped
    /// in at the next bar ([`Engine::relocate`], which prepares it with the options the song
    /// plays with).
    pub fn jump_to_section(&mut self, deck: &str, index: usize) -> bool {
        let Some(at) = self
            .deck(deck)
            .and_then(|d| d.sections.get(index).map(|s| s.start_tick))
        else {
            return false;
        };
        self.relocate(deck, at, None, None)
    }

    /// `Deck.jumpToTick`: the deck's own project from `tick` ([`Engine::relocate`]). (`project`
    /// and `seed` are the deck's already; they stay in the signature for the callers that
    /// pass them.)
    pub fn jump_to_tick(&mut self, deck: &str, _project: &Project, tick: i64, _seed: f64) -> bool {
        self.relocate(deck, tick, None, None)
    }

    // MARK: transport and performance

    /// `Deck.setPlaying(true)`.
    pub fn play(&mut self, deck: &str) -> bool {
        self.set_playing(deck, true)
    }
    /// `Deck.setPlaying(false)`.
    pub fn pause(&mut self, deck: &str) -> bool {
        self.set_playing(deck, false)
    }
    /// `Deck.setPlaying`: the deck's `playing` flag, and the runner's or the turntable's
    /// transport.
    pub fn set_playing(&mut self, deck: &str, on: bool) -> bool {
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        d.playing = on;
        if d.sample.is_some() {
            return self.turntable(deck, TurntableCommand::Transport(on));
        }
        self.command(deck, if on { Command::Play } else { Command::Pause })
    }
    /// `Deck.launch`: start a sample at full speed this block (no motor spin-up), so it lands
    /// where it was put; a net deck plays as usual.
    pub fn launch(&mut self, deck: &str) -> bool {
        if !self.is_sample_deck(deck) {
            return self.set_playing(deck, true);
        }
        if let Some(d) = self.deck_mut(deck) {
            d.playing = true;
        }
        self.turntable(deck, TurntableCommand::Start)
    }
    pub fn stop(&mut self, deck: &str) -> bool {
        if let Some(d) = self.deck_mut(deck) {
            d.playing = false;
        }
        self.command(deck, Command::Stop)
    }
    pub fn panic(&mut self, deck: &str) -> bool {
        let ok = self.command(deck, Command::Panic);
        if ok {
            // a sample deck's own bus: what was waiting for its bar is dropped
            // (`LoopMasterBus.panic`)
            if let Some(d) = self.deck(deck).filter(|d| d.sample.is_some()) {
                d.mirror.clear_master_queue();
            }
            // `Deck.panic` ends in `onMasterMacro?(nil, nil)`: the loops release with it while linked
            self.linked_loop_master(None, None);
            // the pan, shape and aux macros aimed at it end with it, everything put back
            self.cancel_aux_macros(Some(deck));
            self.clear_engagements(deck);
        }
        ok
    }
    pub fn set_tempo(&mut self, deck: &str, bpm: f64) -> bool {
        self.command(deck, Command::Tempo(bpm))
    }
    pub fn set_mute(&mut self, deck: &str, net_id: &str, muted: bool) -> bool {
        self.command(
            deck,
            Command::Mute {
                hash: str_hash(net_id),
                muted,
            },
        )
    }
    pub fn set_role_muted(&mut self, deck: &str, role: &str, muted: bool) -> bool {
        self.command(
            deck,
            Command::RoleMute {
                hash: str_hash(role),
                muted,
            },
        )
    }
    pub fn activate_slot(&mut self, deck: &str, net_id: &str) -> bool {
        self.command(deck, Command::Slot(str_hash(net_id)))
    }
    pub fn fire(&mut self, deck: &str, m: NetMacro, ticks: Option<i64>) -> bool {
        self.command(
            deck,
            Command::Macro {
                m,
                ticks: ticks.unwrap_or_else(|| m.ticks(None)),
            },
        )
    }
    pub fn fire_stinger(&mut self, deck: &str, net_id: &str, ticks: i64) -> bool {
        self.command(
            deck,
            Command::Stinger {
                hash: str_hash(net_id),
                ticks,
            },
        )
    }
    /// Where a master macro fired now lands on a sample deck (`sampleMasterFire`): the first
    /// downbeat of its grid at least [`LOOP_LEAD`] frames ahead (the command must reach the audio
    /// thread first), counting a downbeat every 4 beats, at the deck's tempo. Stopped, or with no
    /// grid: at once (the next block), at its tempo or 120.
    pub fn sample_master_fire(&self, deck: &str) -> (i64, f64) {
        let Some(d) = self.deck(deck) else {
            return (0, 120.0);
        };
        let bpm = d.effective_bpm().unwrap_or(120.0);
        let state = d.beat_state(self.sample_rate).filter(|s| s.bpm > 0.0);
        let Some(s) = state.filter(|_| d.playing) else {
            return (0, bpm);
        };
        let now = d.mirror.rendered_frames.load(Ordering::Acquire);
        let clock = crate::deck::clock::LoopClock {
            ref_deck: Some(deck.to_string()),
            origin_frame: now,
            origin_beat: s.beat,
            frames_per_beat: 60.0 / s.bpm * self.sample_rate,
            bpm: Some(s.bpm),
        };
        (clock.next_boundary(now + LOOP_LEAD, 4), s.bpm)
    }

    /// Fire a master-chain macro for `bars` bars (None: its default; 0: permanent). On a sample
    /// deck it goes to the deck's own bus, on the deck's next bar at its tempo (the vinyl brake
    /// too: it slows the turntable and lands back on the deck's grid): `fire(_: MasterMacro)`.
    pub fn fire_master(&mut self, deck: &str, m: MasterMacro, bars: Option<i32>) -> bool {
        let sent = bars.map_or(-1, |b| b.clamp(-1, i16::MAX as i32) as i16);
        let on_platter = self.deck(deck).is_some_and(|d| d.sample.is_some());
        let ok = if on_platter {
            let (frame, bpm) = self.sample_master_fire(deck);
            let ok = self.command(
                deck,
                Command::SampleMaster(LoopMasterCommand::Macro {
                    id: m.raw(),
                    bars: sent,
                    frame,
                    bpm: if bpm.is_finite() && bpm > 0.0 { bpm } else { 120.0 },
                }),
            );
            if let Some(d) = self.deck(deck).filter(|_| ok) {
                d.mirror.note_master_fire(m);
            }
            ok
        } else {
            self.command(deck, Command::MasterMacro { m, bars: sent })
        };
        if ok {
            // `Deck.fire(_: MasterMacro)` ends in `onMasterMacro?(macro, duration)`: the loops
            // take it too while they are linked (`loopFxLink`)
            self.linked_loop_master(Some(m), bars);
        }
        ok
    }
    /// Move one of the master FX sliders by hand (its own units).
    pub fn set_master_fx(&mut self, deck: &str, key: MasterFxKey, value: i32) -> bool {
        if let Some(d) = self.deck_mut(deck) {
            d.master_fx_set.insert(key.index() as u8, value);
        }
        self.command(deck, Command::MasterFx { key, value })
    }
    pub fn set_voice(&mut self, deck: &str, p: VoiceLaneParams) -> bool {
        self.command(deck, Command::Voice(p))
    }
    /// Toggle a role (`Deck.toggleRole`): a disabled role stays off.
    pub fn toggle_role(&mut self, deck: &str, role: &str) -> bool {
        let Some(d) = self.deck(deck) else {
            return false;
        };
        if d.disabled_roles.iter().any(|r| r == role) {
            return false;
        }
        let on = self.is_role_audible(deck, role);
        self.set_role_muted(deck, role, on)
    }

    /// `Deck.setRoleDisabled`: disable a role (left out of every project loaded from now on, and
    /// muted at once in the one playing) or enable it again (it plays again now: only the mute
    /// comes off, and the notes that were left out of the song come back with the next compile,
    /// as in Swift; `service` keeps a disabled role muted if a section brings it back).
    pub fn set_role_disabled(&mut self, deck: &str, role: &str, disabled: bool) {
        let Some(d) = self.deck_mut(deck) else { return };
        d.disabled_roles.retain(|r| r != role);
        if disabled {
            d.disabled_roles.push(role.to_string());
        }
        // a disabled role is left out of every net the deck compiles from now on: it is in the
        // deck's role filter (`Deck.setRoleDisabled`)
        d.opts.role_filter =
            crate::deck::perform::set_role_disabled(d.opts.role_filter.take(), role, disabled);
        if d.is_net() {
            self.set_role_muted(deck, role, disabled);
        }
    }

    pub fn is_role_disabled(&self, deck: &str, role: &str) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.disabled_roles.iter().any(|r| r == role))
    }

    /// Keep disabled roles muted in the songs playing: a section of the current song can bring
    /// one back before the next compile (`Deck.enforceDisabledRoles`).
    pub fn enforce_disabled_roles(&mut self) {
        let mut back = Vec::new();
        for d in &self.decks {
            for r in &d.disabled_roles {
                if self.is_role_audible(&d.name, r) {
                    back.push((d.name.clone(), r.clone()));
                }
            }
        }
        for (deck, role) in back {
            self.set_role_muted(&deck, &role, true);
        }
    }

    /// The momentary nudge, as a ratio of the tempo (`Deck.nudge`): the jog ring's bend, the
    /// joystick. On top of the tempo fader, not in place of it; a sample deck's platter bends
    /// with it, a net deck's clock runs at base x fader x (1 + ratio).
    pub fn nudge(&mut self, deck: &str, ratio: f64) -> bool {
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        d.net_nudge = ratio;
        let ok = self.turntable(deck, TurntableCommand::Nudge(ratio));
        self.apply_net_tempo(deck);
        ok
    }

    /// [`nudge`](Self::nudge) by a hand that is no controller (the socket, a playback): noted as
    /// a `nudge` live op. A controller's nudge is described by its action, and the jog ring's
    /// bend is MIDI the journal already has.
    pub fn nudge_by_hand(&mut self, deck: &str, ratio: f64) -> bool {
        let ok = self.nudge(deck, ratio);
        if ok {
            let name = deck.to_uppercase();
            self.note_live(
                format!("nudge:{name}"),
                format!("nudge {name} {ratio:+.3}"),
                true,
                "nudge",
                super::vocoder::live_params([
                    ("deck", name.clone().into()),
                    ("ratio", crate::json::JsonValue::Number(ratio)),
                ]),
            );
        }
        ok
    }

    // MARK: mixer

    pub fn set_crossfader(&mut self, v: f64) {
        self.crossfader = v.clamp(0.0, 1.0);
        self.send(Msg::Crossfader(v));
    }
    /// `DJEngine.master`: Swift keeps it in the main mixer node's `outputVolume`, a `Float`, so what
    /// it reads back (and a checkpoint or a recorded session names) is the value rounded to one.
    pub fn set_master(&mut self, v: f64) {
        self.master = v.clamp(0.0, 1.0) as f32 as f64;
        self.send(Msg::Master(v));
    }
    pub fn set_volume(&mut self, deck: &str, v: f64) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        self.decks[i].volume = v.clamp(0.0, 1.0);
        self.send(Msg::Volume(i, v))
    }
    pub fn set_gain(&mut self, deck: &str, v: f64) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        self.decks[i].gain = v.clamp(0.0, 1.0);
        self.send(Msg::Gain(i, v))
    }
    /// A strip knob: `Deck.filter`, `.eqLow`, `.eqMid`, `.eqHigh` (0...1, 0.5 flat).
    pub fn set_strip(&mut self, deck: &str, knob: StripKnob, v: f64) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        let v = v.clamp(0.0, 1.0);
        let s = &mut self.decks[i].strip;
        match knob {
            StripKnob::Filter => s.filter = v,
            StripKnob::EqLow => s.eq_low = v,
            StripKnob::EqMid => s.eq_mid = v,
            StripKnob::EqHigh => s.eq_high = v,
        }
        self.send(Msg::Strip(i, knob, v))
    }
    /// The strip reverb's three knobs (`Deck.reverb.wet / size / tone`): kept, and heard
    /// through the deck's [`StripReverb`](crate::fx::StripReverb), after its EQ (PORTING.md
    /// open decision 1: Apple's AUReverb2 cannot be matched, a stand-in plays).
    pub fn set_reverb(&mut self, deck: &str, name: &str, v: f64) {
        let knob = match name {
            "reverbWet" => ReverbKnob::Wet,
            "reverbSize" => ReverbKnob::Size,
            _ => ReverbKnob::Tone,
        };
        // kept as it was set, as Swift keeps `Deck.reverb`'s raw values; the unit clamps its own
        let Some(i) = self.index(deck) else {
            return;
        };
        let d = &mut self.decks[i];
        match knob {
            ReverbKnob::Wet => d.reverb_wet = v,
            ReverbKnob::Size => d.reverb_size = v,
            ReverbKnob::Tone => d.reverb_tone = v,
        }
        self.send(Msg::Reverb(i, knob, v));
    }
    pub fn set_deck_mute(&mut self, deck: &str, v: bool) -> bool {
        match self.index(deck) {
            Some(i) => {
                self.decks[i].mute = v;
                self.send(Msg::Mute(i, v))
            }
            None => false,
        }
    }
    pub fn set_pfl(&mut self, deck: &str, v: bool) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        self.decks[i].pfl = v;
        let name = self.decks[i].name.clone();
        self.note_cue_deck(&name, v);
        self.send(Msg::Pfl(i, v))
    }
    pub fn set_headphone_mix(&mut self, v: f64) {
        self.headphone_mix = v.clamp(0.0, 1.0);
        self.note_phones("mix", self.headphone_mix);
        self.send(Msg::HeadphoneMix(v));
    }
    pub fn set_headphone_level(&mut self, v: f64) {
        self.headphone_level = v.clamp(0.0, 1.0);
        self.note_phones("level", self.headphone_level);
        self.send(Msg::HeadphoneLevel(v));
    }

    // MARK: reads (any thread's mirrors, this thread's tables)

    /// The master mix's peak at the last block, linear.
    pub fn master_level(&self) -> f32 {
        f32::from_bits(self.master_level.load(Ordering::Relaxed))
    }

    /// The roles this deck's song has, in mixer order (`Deck.roles`).
    pub fn roles(&self, deck: &str) -> Vec<String> {
        self.deck(deck)
            .map(|d| d.roles.iter().map(|(r, _)| r.clone()).collect())
            .unwrap_or_default()
    }

    /// Is the role making sound right now? Any key of the latest song's role table (a riff group
    /// or a plain net id: `hit1`..`hit4`, a riff variant) with a net that is not muted, from the
    /// mute mirror, so it is main-thread safe (`WaveDeckSource.isRoleAudible` through
    /// `Deck.isRoleOn`, which is `mode == .net && ...`). `Deck.roles`, the mixer's rows, is only
    /// the eight `roleOrder` roles; this answers for every role a session or a journal can name
    /// (`toggleRole`, `enforceDisabledRoles`).
    pub fn is_role_audible(&self, deck: &str, role: &str) -> bool {
        let Some(d) = self.deck(deck).filter(|d| d.is_net()) else {
            return false;
        };
        let Some(latest) = d.track.latest.as_ref() else {
            return false;
        };
        latest
            .role_tables
            .role_net_indices(crate::wave::str_hash(role))
            .is_some_and(|ix| ix.iter().any(|&i| !d.mirror.is_net_muted(i)))
    }

    /// Which of the latest project's nets play on a sampler, and from what: net id -> what the
    /// slot plays, as `SamplerSource::to_string` ("kit:MuseScore_General.sf2#128:0",
    /// "sf2:MuseScore_General.sf2#30", "gm:25"; "?" for a slot the bank has no record of). The
    /// rest play synth voices, and so does a channel the sound panel put on its synth voice (the
    /// lane's slot is -1 in Swift while it does). Main thread (`WaveDeckSource.sampledNets`,
    /// `dj_state`'s `sampled`, `djctl`'s `audio.sampled`).
    pub fn sampled_nets(&self, deck: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(d) = self.deck(deck) else {
            return out;
        };
        let Some(latest) = d.track.latest.as_ref() else {
            return out;
        };
        for (net, channel, slot) in &latest.sampled {
            if d.voice_settings.get(channel).is_some_and(|s| s.use_synth) {
                continue;
            }
            #[cfg(feature = "sf2")]
            let what = d
                .bank
                .as_ref()
                .and_then(|b| {
                    b.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .source_of(*slot)
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "?".to_string());
            #[cfg(not(feature = "sf2"))]
            let what = {
                let _ = slot;
                "?".to_string()
            };
            out.insert(net.clone(), what);
        }
        out
    }

    /// The section the net clock is in (`currentSectionIndex`).
    pub fn current_section(&self, deck: &str) -> Option<usize> {
        let d = self.deck(deck)?;
        let t = d.mirror.tick.load(Ordering::Relaxed);
        d.sections
            .iter()
            .rposition(|s| s.start_tick <= t && t < s.start_tick + s.steps)
    }
}
