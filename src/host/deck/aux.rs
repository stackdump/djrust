//! The aux send/return bus on the mixer, and the macros that drive it (docs/HUB.md M4; docs/AUX.md).
//!
//! Render thread ([`AuxRender`], owned by the [`Renderer`](super::engine::Renderer)): the sends of
//! every strip (A, B, L, each input), post-fader, summed on a stereo bus; the bus
//! ([`crate::fx::AuxBus`]); the macros in flight; the return added to the master. Main thread
//! (`impl Engine`): the knobs, kept, sent, traced as `aux` live ops (an arrangement replays
//! them), and the macros, started on the aimed deck's next bar.
//!
//! **Macros.** [`AuxMacro`] has eleven. A pan or shape macro holds the aimed deck's parts at the
//! pattern's pan or decay for its bars (`WaveRunner::set_pan_move` / `set_decay_move`); an FX
//! macro (delay throw, dub delay, cathedral) jumps the bus's sliders and raises the aimed strip's
//! send, holds, and fades back. Each block the renderer works out where every running macro is
//! from the frame it started on, so it needs no timer and offline renders match live ones.
//! Starting on the bar is the main thread's: it works out the frame
//! ([`Engine::sample_master_fire`]) and the beat length and sends both.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::fx::aux_bus::{AuxBus, AuxFxKey, AuxSettings};
use crate::fx::aux_macro::{hold_at, AuxKind, AuxMacro, THROW_SEND};
use crate::json::JsonValue;
use crate::session::arrangement::ArrangeOp;
use crate::wave::MacroTargets;

use super::engine::{Engine, Strip, MAX_BLOCK};

/// Macros in flight at once (a fire beyond it is refused).
const MAX_ACTIVE: usize = 16;

/// What a send belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxTarget {
    /// A deck strip, by its index in the mixer.
    Deck(usize),
    /// The loop channel L.
    Loop,
    /// An input strip, by index.
    Input(usize),
}

/// Main thread to render thread.
#[derive(Clone, Copy, Debug)]
pub enum AuxMsg {
    Send(AuxTarget, f64),
    Level(f64),
    Mute(bool),
    Fx(AuxFxKey, f64),
    /// Start a macro at render frame `start`, `bars` long at `beat_frames` frames a beat, aimed
    /// at `target`.
    Fire {
        m: AuxMacro,
        target: AuxTarget,
        start: i64,
        beat_frames: f64,
        bars: i32,
    },
    /// Stop the macros aimed at a target (all of them with None) and put everything back.
    Cancel(Option<AuxTarget>),
    /// Stop one macro on a target (a latch let go) and put back what it moved.
    CancelOne(AuxMacro, AuxTarget),
}

/// How many bars a latched aux macro is fired for: it holds until it is let go.
pub const LATCH_BARS: i32 = 1_000_000;

/// What the render thread tells the others.
#[derive(Default)]
pub struct AuxMirror {
    /// Bits of [`AuxMacro::bit`]: the macros running (or waiting for their bar).
    pub mask: AtomicU32,
    /// The return's peak at the last block (f32 bits).
    pub peak: AtomicU32,
    /// The macros running aimed at the loop channel.
    pub loop_mask: AtomicU32,
}

impl AuxMirror {
    pub fn running(&self) -> u32 {
        self.mask.load(Ordering::Relaxed)
    }

    pub fn is_running(&self, m: AuxMacro) -> bool {
        self.running() & m.bit() != 0
    }

    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    pub fn loop_mask(&self) -> u32 {
        self.loop_mask.load(Ordering::Relaxed)
    }
}

/// One strip's send as the render thread has it.
#[derive(Clone, Copy, Default)]
struct Send {
    /// The knob.
    base: f64,
    /// What a running FX macro raises it to (0: nothing).
    boost: f64,
    /// Where the ramp over the last block ended.
    last: f64,
}

impl Send {
    fn target(&self) -> f64 {
        self.base.max(self.boost)
    }
}

#[derive(Clone, Copy)]
struct Active {
    m: AuxMacro,
    target: AuxTarget,
    start: i64,
    /// Frames the macro lasts.
    len: i64,
    beat_frames: f64,
    /// The sliders as they were when it started, for its ops (an FX hold).
    starts: [f64; 3],
}

/// The render thread's half.
pub(super) struct AuxRender {
    pub(super) bus: AuxBus,
    pub(super) l: Vec<f32>,
    pub(super) r: Vec<f32>,
    /// Was anything sent in this block?
    pub(super) sent: bool,
    decks: Vec<Send>,
    looped: Send,
    inputs: Vec<Send>,
    active: Vec<Active>,
    /// Which targets a pan / decay macro has hold of now, to let go when it ends.
    pan_held: Vec<Option<MacroTargets>>,
    decay_held: Vec<Option<MacroTargets>>,
    pub(super) mirror: Arc<AuxMirror>,
}

impl AuxRender {
    pub(super) fn new(sample_rate: f64, decks: usize, inputs: usize, mirror: Arc<AuxMirror>) -> AuxRender {
        AuxRender {
            bus: AuxBus::new(sample_rate),
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            sent: false,
            decks: vec![Send::default(); decks],
            looped: Send::default(),
            inputs: vec![Send::default(); inputs],
            active: Vec::with_capacity(MAX_ACTIVE),
            pan_held: vec![None; decks],
            decay_held: vec![None; decks],
            mirror,
        }
    }

    fn send_mut(&mut self, t: AuxTarget) -> Option<&mut Send> {
        match t {
            AuxTarget::Deck(i) => self.decks.get_mut(i),
            AuxTarget::Loop => Some(&mut self.looped),
            AuxTarget::Input(i) => self.inputs.get_mut(i),
        }
    }

    pub(super) fn apply(&mut self, m: AuxMsg, strips: &mut [Strip]) {
        match m {
            AuxMsg::Send(t, v) => {
                if let Some(s) = self.send_mut(t) {
                    s.base = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                }
            }
            AuxMsg::Level(v) => self.bus.set_level(v),
            AuxMsg::Mute(on) => self.bus.set_mute(on),
            AuxMsg::Fx(k, v) => self.bus.set(k, v),
            AuxMsg::Fire { m, target, start, beat_frames, bars } => {
                if self.active.len() >= MAX_ACTIVE || !(beat_frames > 0.0) {
                    return;
                }
                let len = (bars.max(1) as f64 * 4.0 * beat_frames) as i64;
                let mut starts = [0.0; 3];
                if let AuxKind::Hold { ops, .. } = m.kind() {
                    for (n, (k, _)) in ops.iter().enumerate().take(3) {
                        // a macro fired again while one holds the slider starts from where the
                        // first started, so the pair does not strand the peak
                        starts[n] = self
                            .active
                            .iter()
                            .find_map(|a| match a.m.kind() {
                                AuxKind::Hold { ops: o2, .. } => o2
                                    .iter()
                                    .position(|(k2, _)| k2 == k)
                                    .map(|p| a.starts[p.min(2)]),
                                _ => None,
                            })
                            .unwrap_or_else(|| self.bus.settings.get(*k));
                    }
                }
                // a pan or shape macro again on the same parts replaces the one before it
                if !matches!(m.kind(), AuxKind::Hold { .. }) {
                    let same = |a: &Active| {
                        a.target == target
                            && std::mem::discriminant(&a.m.kind()) == std::mem::discriminant(&m.kind())
                    };
                    self.active.retain(|a| !same(a));
                }
                self.active.push(Active { m, target, start, len, beat_frames, starts });
            }
            AuxMsg::CancelOne(m, target) => {
                let mut k = self.active.len();
                while k > 0 {
                    k -= 1;
                    if self.active[k].m == m && self.active[k].target == target {
                        let a = self.active.remove(k);
                        self.end(&a);
                    }
                }
                self.let_go(strips);
            }
            AuxMsg::Cancel(t) => {
                // in place: this runs on the render thread
                let mut k = self.active.len();
                while k > 0 {
                    k -= 1;
                    if t.is_none_or(|t| self.active[k].target == t) {
                        let a = self.active.remove(k);
                        self.end(&a);
                    }
                }
                self.let_go(strips);
            }
        }
    }

    /// Put back what a macro moved on the bus: its sliders at their starts.
    fn end(&mut self, a: &Active) {
        if let AuxKind::Hold { ops, .. } = a.m.kind() {
            for (n, (k, _)) in ops.iter().enumerate().take(3) {
                // only if no later macro still holds the slider
                let held = self.active.iter().any(|b| {
                    !(b.m == a.m && b.target == a.target && b.start == a.start)
                        && matches!(b.m.kind(), AuxKind::Hold { ops: o, .. } if o.iter().any(|(k2, _)| k2 == k))
                });
                if !held {
                    self.bus.set(*k, a.starts[n]);
                }
            }
        }
    }

    /// Strips whose pan or decay no macro holds any more are let go.
    fn let_go(&mut self, strips: &mut [Strip]) {
        for (i, s) in strips.iter_mut().enumerate() {
            let pan = self.holds(i, true);
            if self.pan_held[i].is_some() && pan.is_none() {
                if let Some(w) = self.pan_held[i].take() {
                    s.core.runner.set_pan_move(w, None);
                }
            }
            let dec = self.holds(i, false);
            if self.decay_held[i].is_some() && dec.is_none() {
                if let Some(w) = self.decay_held[i].take() {
                    s.core.runner.set_decay_move(w, None);
                }
            }
        }
    }

    fn holds(&self, deck: usize, pan: bool) -> Option<usize> {
        self.active.iter().rposition(|a| {
            a.target == AuxTarget::Deck(deck)
                && match a.m.kind() {
                    AuxKind::Pan(_) => pan,
                    AuxKind::Decay(_) => !pan,
                    AuxKind::Hold { .. } => false,
                }
        })
    }

    /// Work out every running macro at the block that starts at `frame`: move the bus's sliders,
    /// the sends' boosts and the parts' pan and decay; end the ones that are over.
    pub(super) fn begin(&mut self, frame: i64, n: usize, strips: &mut [Strip]) {
        self.l[..n].fill(0.0);
        self.r[..n].fill(0.0);
        self.sent = false;
        for s in self.decks.iter_mut().chain(self.inputs.iter_mut()) {
            s.boost = 0.0;
        }
        self.looped.boost = 0.0;
        if self.active.is_empty() {
            self.mirror.mask.store(0, Ordering::Relaxed);
            self.mirror.loop_mask.store(0, Ordering::Relaxed);
            for s in strips.iter() {
                s.core.mirror.aux_mask.store(0, Ordering::Relaxed);
            }
            return;
        }
        // the ones that are over (removing in place: nothing allocates here)
        let mut k = self.active.len();
        while k > 0 {
            k -= 1;
            if frame >= self.active[k].start + self.active[k].len {
                let a = self.active.remove(k);
                self.end(&a);
            }
        }
        let mut mask = 0u32;
        let mut loop_mask = 0u32;
        for s in strips.iter() {
            s.core.mirror.aux_mask.store(0, Ordering::Relaxed);
        }
        for k in 0..self.active.len() {
            let a = self.active[k];
            mask |= a.m.bit();
            match a.target {
                AuxTarget::Deck(i) => {
                    if let Some(s) = strips.get(i) {
                        s.core.mirror.aux_mask.fetch_or(a.m.bit(), Ordering::Relaxed);
                    }
                }
                AuxTarget::Loop => loop_mask |= a.m.bit(),
                AuxTarget::Input(_) => {}
            }
            if frame < a.start {
                continue;
            }
            let beats = (frame - a.start) as f64 / a.beat_frames;
            let dur_beats = a.len as f64 / a.beat_frames;
            match a.m.kind() {
                AuxKind::Hold { ops, tail_frac } => {
                    for (i, (key, to)) in ops.iter().enumerate().take(3) {
                        let v = hold_at(a.starts[i], *to, beats, dur_beats, tail_frac);
                        self.bus.set(*key, v);
                    }
                    // the aimed strip goes into the effect, on the same envelope
                    let throw = hold_at(0.0, THROW_SEND, beats, dur_beats, tail_frac);
                    if let Some(s) = self.send_mut(a.target) {
                        s.boost = s.boost.max(throw);
                    }
                }
                AuxKind::Pan(_) | AuxKind::Decay(_) => {}
            }
        }
        self.mirror.mask.store(mask, Ordering::Relaxed);
        self.mirror.loop_mask.store(loop_mask, Ordering::Relaxed);
        // pan and decay: the latest macro that holds a strip's parts sets them
        for i in 0..strips.len().min(self.decks.len()) {
            if let Some(k) = self.holds(i, true) {
                let a = self.active[k];
                if frame >= a.start {
                    let beats = (frame - a.start) as f64 / a.beat_frames;
                    if let Some(p) = a.m.pan_at(beats) {
                        if let Some(prev) = self.pan_held[i].filter(|w| *w != a.m.targets()) {
                            strips[i].core.runner.set_pan_move(prev, None);
                        }
                        strips[i].core.runner.set_pan_move(a.m.targets(), Some(p));
                        self.pan_held[i] = Some(a.m.targets());
                    }
                }
            }
            if let Some(k) = self.holds(i, false) {
                let a = self.active[k];
                if frame >= a.start {
                    let beats = (frame - a.start) as f64 / a.beat_frames;
                    if let Some(d) = a.m.decay_at(beats) {
                        if let Some(prev) = self.decay_held[i].filter(|w| *w != a.m.targets()) {
                            strips[i].core.runner.set_decay_move(prev, None);
                        }
                        strips[i].core.runner.set_decay_move(a.m.targets(), Some(d));
                        self.decay_held[i] = Some(a.m.targets());
                    }
                }
            }
        }
        self.let_go(strips);
    }

    /// Add a strip's post-fader block `src` (times its gain ramp `g0 -> g0 + step n`) to the bus
    /// at the strip's send, ramped over the block.
    pub(super) fn add(
        &mut self,
        t: AuxTarget,
        n: usize,
        src_l: &[f32],
        src_r: &[f32],
        g0: f64,
        step: f64,
    ) {
        let Some(s) = self.send_mut(t) else {
            return;
        };
        let target = s.target();
        let from = s.last;
        s.last = target;
        if from == 0.0 && target == 0.0 {
            return;
        }
        let ds = (target - from) / n.max(1) as f64;
        for i in 0..n {
            let g = (g0 + step * (i + 1) as f64) * (from + ds * (i + 1) as f64);
            self.l[i] += (g * src_l[i] as f64) as f32;
            self.r[i] += (g * src_r[i] as f64) as f32;
        }
        self.sent = true;
    }

    /// An input strip's send over the block, as `(from, to)`; updates the ramp.
    pub(super) fn input_ramp(&mut self, k: usize) -> (f64, f64) {
        match self.inputs.get_mut(k) {
            Some(s) => {
                let r = (s.last, s.target());
                s.last = r.1;
                r
            }
            None => (0.0, 0.0),
        }
    }

    /// Run the bus and add its return to the master block.
    pub(super) fn finish(&mut self, n: usize, out_l: &mut [f32], out_r: &mut [f32]) {
        let peak = self
            .bus
            .process(&self.l[..n], &self.r[..n], &mut out_l[..n], &mut out_r[..n], self.sent);
        self.mirror.peak.store(peak.to_bits(), Ordering::Relaxed);
    }
}

// MARK: the main thread's half

/// The stems and the mixer name the bus.
pub const AUX_NAME: &str = "aux";

impl Engine {
    /// A strip by the name a control uses: a deck (`A`), the loop channel (`L`) or an input's
    /// label.
    pub fn aux_target(&self, name: &str) -> Option<AuxTarget> {
        if name == "L" && self.index("L").is_none() {
            return Some(AuxTarget::Loop);
        }
        if let Some(i) = self.index(name) {
            return Some(AuxTarget::Deck(i));
        }
        self.inputs
            .iter()
            .position(|x| x.settings.label.eq_ignore_ascii_case(name))
            .map(AuxTarget::Input)
    }

    /// The name of a target, as a control and the log say it.
    pub fn aux_target_name(&self, t: AuxTarget) -> String {
        match t {
            AuxTarget::Deck(i) => self.decks[i].name.clone(),
            AuxTarget::Loop => "L".into(),
            AuxTarget::Input(i) => self.inputs[i].settings.label.clone(),
        }
    }

    /// A strip's aux send, 0...1.
    pub fn aux_send(&self, strip: &str) -> Option<f64> {
        Some(match self.aux_target(strip)? {
            AuxTarget::Deck(i) => self.decks[i].aux,
            AuxTarget::Loop => self.loop_channel.aux,
            AuxTarget::Input(i) => self.inputs[i].settings.aux,
        })
    }

    /// Set a strip's aux send (0...1), traced as an `aux` live op.
    pub fn set_aux_send(&mut self, strip: &str, v: f64) -> bool {
        let Some(t) = self.aux_target(strip) else {
            return false;
        };
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match t {
            AuxTarget::Deck(i) => self.decks[i].aux = v,
            AuxTarget::Loop => self.loop_channel.aux = v,
            AuxTarget::Input(i) => self.inputs[i].settings.aux = v,
        }
        let name = self.aux_target_name(t);
        self.aux_trace(
            format!("aux:{name}:send"),
            format!("{name} aux send {v:.2}"),
            true,
            [("strip", JsonValue::str(&name)), ("send", JsonValue::Number(v))],
        );
        self.aux_send_msg(AuxMsg::Send(t, v))
    }

    pub fn set_aux_return(&mut self, v: f64) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        self.aux.level = v;
        self.aux_trace(
            "aux:return".into(),
            format!("FX return {v:.2}"),
            true,
            [("return", JsonValue::Number(v))],
        );
        self.aux_send_msg(AuxMsg::Level(v));
    }

    pub fn set_aux_mute(&mut self, on: bool) {
        self.aux.mute = on;
        self.aux_trace(
            "aux:mute".into(),
            format!("FX return {}", if on { "muted" } else { "unmuted" }),
            false,
            [("mute", on.into())],
        );
        self.aux_send_msg(AuxMsg::Mute(on));
    }

    /// One of the bus's six sliders, in its own units (see [`AuxFxKey`]).
    pub fn set_aux_fx(&mut self, key: AuxFxKey, value: f64) {
        self.aux.set(key, value);
        let v = self.aux.get(key);
        self.aux_trace(
            format!("aux:{}", key.id()),
            format!("{} {v:.0}", key.label()),
            true,
            [(key.id(), JsonValue::Number(v))],
        );
        self.aux_send_msg(AuxMsg::Fx(key, v));
    }

    /// Reset the six sliders to beats' defaults.
    pub fn reset_aux_fx(&mut self) {
        for k in AuxFxKey::ALL {
            self.set_aux_fx(k, k.default_value() as f64);
        }
    }

    pub fn aux_settings(&self) -> AuxSettings {
        self.aux
    }

    /// The macros running now (bits of [`AuxMacro::bit`]).
    pub fn aux_macros_running(&self) -> u32 {
        self.aux_mirror.running()
    }

    /// The return's peak at the last block.
    pub fn aux_peak(&self) -> f32 {
        self.aux_mirror.peak()
    }

    pub(crate) fn aux_send_msg(&mut self, m: AuxMsg) -> bool {
        self.send(super::engine::Msg::Aux(m))
    }

    /// Fire one of the eleven macros at `deck` (a deck, or `L`) for `bars` bars (None: its
    /// default), on the deck's next bar; at once when it is not playing. A pan or shape macro
    /// needs a net deck.
    pub fn fire_aux_macro(&mut self, deck: &str, m: AuxMacro, bars: Option<i32>) -> bool {
        let Some(target) = self.aux_target(deck) else {
            return false;
        };
        if let AuxTarget::Input(_) = target {
            if !matches!(m.kind(), AuxKind::Hold { .. }) {
                return false;
            }
        }
        let parts = !matches!(m.kind(), AuxKind::Hold { .. });
        if parts && !matches!(target, AuxTarget::Deck(i) if self.decks[i].is_net()) {
            return false;
        }
        let (start, bpm) = match target {
            AuxTarget::Deck(_) => self.sample_master_fire(deck),
            _ => (0, self.master_tempo().1),
        };
        let bpm = if bpm.is_finite() && bpm > 0.0 { bpm } else { 120.0 };
        // 0 is permanent (a latch): it holds until [`Engine::release_aux_macro`]
        let bars = match bars {
            Some(0) => LATCH_BARS,
            Some(b) if b > 0 => b,
            _ => m.default_duration(),
        };
        self.aux_send_msg(AuxMsg::Fire {
            m,
            target,
            start,
            beat_frames: 60.0 / bpm * self.sample_rate,
            bars,
        })
    }

    /// Fire any macro of the catalogue (a net, master or aux one) for `bars` bars, as `perform`
    /// guards it, and put it in the action trace as an `aux` live op (`macro`, `deck`, `bars`) so
    /// an arrangement replays it with its length. (A macro fired through `perform` is a `macro`
    /// action already and is logged by that: `DeckAction::Macro` has no bars, as Swift's.)
    pub fn fire_macro_traced(&mut self, deck: &str, id: &str, bars: Option<i32>) -> bool {
        let ok = match AuxMacro::from_id(id) {
            Some(m) => {
                let hold = matches!(m.kind(), AuxKind::Hold { .. });
                let target_ok = match self.aux_target(deck) {
                    Some(AuxTarget::Deck(i)) => hold || self.decks[i].is_net(),
                    Some(AuxTarget::Loop) => hold,
                    _ => false,
                };
                target_ok && self.fire_aux_macro(deck, m, bars)
            }
            None => matches!(self.apply_macro(deck, id, bars), super::perform::Applied::Done),
        };
        if !ok {
            return false;
        }
        let words = match bars {
            Some(b) if b > 0 => format!("{id} on {deck} ({b} bars)"),
            Some(0) => format!("{id} on {deck} (latched)"),
            _ => format!("{id} on {deck}"),
        };
        let mut params = vec![("macro", JsonValue::str(id)), ("deck", JsonValue::str(deck))];
        if let Some(b) = bars.filter(|b| *b >= 0) {
            params.push(("bars", JsonValue::Number(b as f64)));
        }
        let (key, text) = (format!("aux:macro:{id}"), words);
        self.aux_trace_vec(key, text, false, params);
        true
    }

    /// Let go of one aux macro on `deck` (a latch): what it moved is put back.
    pub fn release_aux_macro(&mut self, deck: &str, m: AuxMacro) -> bool {
        match self.aux_target(deck) {
            Some(t) => self.aux_send_msg(AuxMsg::CancelOne(m, t)),
            None => false,
        }
    }

    /// Put every running aux macro, or a strip's, back.
    pub fn cancel_aux_macros(&mut self, strip: Option<&str>) -> bool {
        let t = match strip {
            Some(s) => match self.aux_target(s) {
                Some(t) => Some(t),
                None => return false,
            },
            None => None,
        };
        self.aux_send_msg(AuxMsg::Cancel(t))
    }

    /// The aux knobs in the action trace: words, and the `aux` live op that sets them again.
    fn aux_trace<const N: usize>(
        &mut self,
        key: String,
        text: String,
        fold: bool,
        params: [(&str, JsonValue); N],
    ) {
        self.aux_trace_vec(key, text, fold, params.to_vec());
    }

    fn aux_trace_vec(&mut self, key: String, text: String, fold: bool, params: Vec<(&str, JsonValue)>) {
        if self.trace.is_none() {
            return;
        }
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(super::trace::Traced {
            entry: crate::control::action_log::Entry { source: via, key, text, step: None, fold },
            ops: vec![ArrangeOp::Live {
                method: "aux".into(),
                params: JsonValue::Object(crate::json::JsonObject::from_pairs(params)),
            }],
        });
    }

    /// The `aux` live op, as djapp's socket method: `strip` + `send` (a strip's send), `return`,
    /// `mute`, any of the bus's sliders by beats' id (`reverb-wet`, `delay-time`…), `reset`.
    pub(crate) fn live_aux(&mut self, p: &JsonValue) -> Result<(), String> {
        let num = |k: &str| match p.get(k)? {
            JsonValue::Number(n) => Some(*n),
            JsonValue::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        if p.get("reset").and_then(JsonValue::as_bool) == Some(true) {
            self.reset_aux_fx();
        }
        if let Some(strip) = p.get("strip").and_then(JsonValue::as_str) {
            let v = num("send").ok_or("aux: a strip needs a send")?;
            if !self.set_aux_send(strip, v) {
                return Err(format!("aux: no strip {strip:?}"));
            }
        }
        if let Some(id) = p.get("macro").and_then(JsonValue::as_str) {
            if !Engine::is_macro_id(id) {
                return Err(format!("aux: no macro {id:?}"));
            }
            let deck = p
                .get("deck")
                .and_then(JsonValue::as_str)
                .map(str::to_uppercase)
                .unwrap_or_else(|| self.focus.clone());
            // (untraced here: a replay does not log what it plays; the log's own is the live op)
            let ok = match AuxMacro::from_id(id) {
                Some(m) => self.fire_aux_macro(&deck, m, num("bars").map(|b| b as i32)),
                None => matches!(
                    self.apply_macro(&deck, id, num("bars").map(|b| b as i32)),
                    super::perform::Applied::Done
                ),
            };
            if !ok {
                return Err(format!("aux: {id} cannot be fired at {deck:?}"));
            }
        }
        if let Some(v) = num("return") {
            self.set_aux_return(v);
        }
        if let Some(b) = p.get("mute").and_then(JsonValue::as_bool) {
            self.set_aux_mute(b);
        }
        for k in AuxFxKey::ALL {
            if let Some(v) = num(k.id()) {
                self.set_aux_fx(k, v);
            }
        }
        Ok(())
    }

    /// The aux's state when an arrangement starts, as `aux` live ops: each strip's send, the
    /// return and the sliders that differ from a fresh bus's.
    pub fn aux_setup_ops(&self) -> Vec<ArrangeOp> {
        let mut out = Vec::new();
        let mut sends = Vec::new();
        for d in &self.decks {
            sends.push((d.name.clone(), d.aux));
        }
        sends.push(("L".to_string(), self.loop_channel.aux));
        for i in &self.inputs {
            sends.push((i.settings.label.clone(), i.settings.aux));
        }
        for (name, v) in sends {
            if v != 0.0 {
                out.push(ArrangeOp::Live {
                    method: "aux".into(),
                    params: super::vocoder::live_params([
                        ("strip", JsonValue::str(&name)),
                        ("send", JsonValue::Number(v)),
                    ]),
                });
            }
        }
        let fresh = AuxSettings::default();
        let mut o = crate::json::JsonObject::new();
        if self.aux.level != fresh.level {
            o.set("return", JsonValue::Number(self.aux.level));
        }
        if self.aux.mute {
            o.set("mute", true.into());
        }
        for k in AuxFxKey::ALL {
            if self.aux.get(k) != fresh.get(k) {
                o.set(k.id(), JsonValue::Number(self.aux.get(k)));
            }
        }
        if o.len() > 0 {
            out.push(ArrangeOp::Live { method: "aux".into(), params: JsonValue::Object(o) });
        }
        out
    }
}
