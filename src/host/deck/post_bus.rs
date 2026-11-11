//! The post chain on the live master (docs/POST.md, "Live"): the same chain as the offline
//! `--post` presets, on the master bus after the aux return and **before the master level**,
//! so the master fader still turns the finished sound down (a leveler after it would undo the
//! fader). Off by default: with it off the master is bit-for-bit what it was.
//!
//! The loudness stage runs live as the leveler (short-term LUFS AGC + true-peak limiter); the
//! denoise runs at a 1024-point FFT (21 ms at 48 kHz, plus the limiter's 1.75 ms) so the booth
//! hears it late by that much only while a denoise stage is on. A change of chain crossfades over [`FADE_SECONDS`] from the
//! old one (or the dry signal) to the new.
//!
//! Threading: the main thread builds a [`LivePost`] (allocates) and sends it boxed; the render
//! thread swaps it in and hands the old one back through a retire ring (it never frees). The
//! meters come back through [`PostMirror`]'s atomics.
//!
//! As a live op (`{"op": "post", …}`, the socket method's params): `on` (bool), `toggle`,
//! `preset` (a name), `chain` (a chain, JSON), `step` (±1: the next / previous preset). Traced
//! into the action log and the journal, so an arrangement or a journal replays it, offline too.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::fx::post::{LivePost, PostChain, DEFAULT_PRESET};
use crate::json::{JsonObject, JsonValue};
use crate::session::arrangement::ArrangeOp;
use rtrb::Producer;

use super::engine::{Engine, Msg, MAX_BLOCK};

/// How long a chain change crossfades.
pub const FADE_SECONDS: f64 = 0.05;

/// What the render thread reports.
#[derive(Default)]
pub struct PostMirror {
    pub active: AtomicBool,
    gain_db: AtomicU32,
    short_term: AtomicU32,
    limiter_db: AtomicU32,
    noise_db: AtomicU32,
    latency: AtomicU32,
}

fn load(a: &AtomicU32) -> f64 {
    f32::from_bits(a.load(Ordering::Relaxed)) as f64
}

impl PostMirror {
    pub fn gain_db(&self) -> f64 {
        load(&self.gain_db)
    }
    pub fn short_term_lufs(&self) -> f64 {
        load(&self.short_term)
    }
    pub fn limiter_db(&self) -> f64 {
        load(&self.limiter_db)
    }
    /// NaN without a denoise stage.
    pub fn noise_db(&self) -> f64 {
        load(&self.noise_db)
    }
    pub fn latency_frames(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

/// The render thread's half.
pub(crate) struct PostRender {
    cur: Option<Box<LivePost>>,
    /// Fading out: the old chain (None inside = the dry signal), and how far along.
    old: Option<Option<Box<LivePost>>>,
    at: usize,
    fade: usize,
    l: Vec<f32>,
    r: Vec<f32>,
    retire: Producer<Box<LivePost>>,
    mirror: Arc<PostMirror>,
    pub(crate) overflow: u64,
}

impl PostRender {
    pub(crate) fn new(sr: f64, retire: Producer<Box<LivePost>>, mirror: Arc<PostMirror>) -> PostRender {
        mirror.noise_db.store(f32::NAN.to_bits(), Ordering::Relaxed);
        PostRender {
            cur: None,
            old: None,
            at: 0,
            fade: ((FADE_SECONDS * sr) as usize).max(1),
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            retire,
            mirror,
            overflow: 0,
        }
    }

    fn retire(&mut self, p: Option<Box<LivePost>>) {
        if let Some(p) = p {
            if let Err(rtrb::PushError::Full(p)) = self.retire.push(p) {
                self.overflow += 1;
                drop(p);
            }
        }
    }

    /// A new chain (None: off). Render thread.
    pub(crate) fn apply(&mut self, next: Option<Box<LivePost>>) {
        if let Some(prev) = self.old.take() {
            self.retire(prev);
        }
        let was = self.cur.take();
        self.old = Some(was);
        self.cur = next;
        self.at = 0;
        self.mirror.active.store(self.cur.is_some(), Ordering::Relaxed);
        self.mirror.latency.store(
            self.cur.as_ref().map_or(0, |p| p.latency() as u32),
            Ordering::Relaxed,
        );
    }

    /// Run the chain over the master's `n` frames. Real-time safe.
    pub(crate) fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len()).min(MAX_BLOCK);
        if self.old.is_none() && self.cur.is_none() {
            return;
        }
        let fading = self.old.is_some();
        if fading {
            self.l[..n].copy_from_slice(&out_l[..n]);
            self.r[..n].copy_from_slice(&out_r[..n]);
            if let Some(Some(p)) = self.old.as_mut() {
                p.process(&mut self.l[..n], &mut self.r[..n]);
            }
        }
        if let Some(p) = self.cur.as_mut() {
            p.process(&mut out_l[..n], &mut out_r[..n]);
            let m = p.meter();
            self.mirror.gain_db.store((m.gain_db as f32).to_bits(), Ordering::Relaxed);
            self.mirror
                .short_term
                .store((m.short_term_lufs as f32).to_bits(), Ordering::Relaxed);
            self.mirror
                .limiter_db
                .store((m.limiter_db as f32).to_bits(), Ordering::Relaxed);
            self.mirror.noise_db.store(
                (m.noise_db.unwrap_or(f64::NAN) as f32).to_bits(),
                Ordering::Relaxed,
            );
        }
        if fading {
            for i in 0..n {
                let t = ((self.at + i) as f32 / self.fade as f32).min(1.0);
                out_l[i] = self.l[i] * (1.0 - t) + out_l[i] * t;
                out_r[i] = self.r[i] * (1.0 - t) + out_r[i] * t;
            }
            self.at += n;
            if self.at >= self.fade {
                if let Some(prev) = self.old.take() {
                    self.retire(prev);
                }
            }
        }
    }
}

/// The main thread's view: whether the chain is on and which one.
#[derive(Clone, Debug, PartialEq)]
pub struct PostState {
    pub on: bool,
    /// The chain the post runs when on (a preset or a custom one); kept while off, so a toggle
    /// brings the same one back.
    pub chain: PostChain,
}

impl Default for PostState {
    fn default() -> PostState {
        PostState {
            on: false,
            chain: PostChain::preset(DEFAULT_PRESET).expect("the default preset exists"),
        }
    }
}

impl PostState {
    /// The live op that makes this state again.
    pub fn op_params(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([("on", self.on.into())]);
        match PostChain::preset(self.chain.label()).filter(|p| p.stages == self.chain.stages) {
            Some(_) => o.set("preset", JsonValue::str(self.chain.label())),
            None => o.set(
                "chain",
                self.chain.json().get("stages").cloned().unwrap_or(JsonValue::Null),
            ),
        }
        JsonValue::Object(o)
    }
}

impl Engine {
    /// Turn the live post on or off and/or change its chain; traced as a `post` live op.
    pub fn set_post(&mut self, on: bool, chain: Option<PostChain>) {
        if let Some(c) = chain {
            self.post.chain = c;
        }
        self.post.on = on;
        let next = (on && !self.post.chain.is_empty())
            .then(|| Box::new(LivePost::new(&self.post.chain, self.sample_rate)));
        self.send(Msg::Post(next));
        let words = if on {
            format!("post {} on", self.post.chain.label())
        } else {
            format!("post off ({})", self.post.chain.label())
        };
        let params = self.post.op_params();
        self.note_resolved("post".into(), words, true, "post", params);
    }

    /// Step the preset (±1), keeping on / off.
    pub fn step_post_preset(&mut self, dir: i32) -> String {
        let next = PostChain::step_preset(self.post.chain.label(), dir);
        let on = self.post.on;
        self.set_post(on, PostChain::preset(next));
        next.to_string()
    }

    /// The `post` live op, as djapp's socket method takes it: `on`, `toggle`, `preset`, `chain`,
    /// `step`.
    pub(crate) fn live_post(&mut self, p: &JsonValue) -> Result<(), String> {
        let chain = match (p.get("chain"), p.get("preset")) {
            (Some(c), _) if !matches!(c, JsonValue::Null) => Some(PostChain::from_json(c)?),
            (_, Some(JsonValue::String(name))) => Some(
                PostChain::preset(name).ok_or_else(|| format!("post: no preset {name:?}"))?,
            ),
            _ => None,
        };
        if let Some(d) = super::live_ops::num(p, "step") {
            if chain.is_some() {
                return Err("post: step or preset / chain, not both".into());
            }
            self.step_post_preset(if d < 0.0 { -1 } else { 1 });
            return Ok(());
        }
        let on = if super::live_ops::flag(p, "toggle") == Some(true) {
            !self.post.on
        } else {
            match super::live_ops::flag(p, "on") {
                Some(v) => v,
                // naming a chain without `on` turns it on
                None => chain.is_some() || self.post.on,
            }
        };
        // a preset named "none" is off
        if chain.as_ref().is_some_and(PostChain::is_empty) && on {
            self.set_post(false, chain);
            return Ok(());
        }
        self.set_post(on, chain);
        Ok(())
    }

    /// The post as JSON: on, the chain, the meters (`gain_db`: the leveler's gain now,
    /// `short_term_lufs`: what it hears, `limiter_db`, `noise_dbfs`: the denoise's estimate),
    /// the latency it adds.
    pub fn post_json(&self) -> JsonValue {
        let m = &self.post_mirror;
        let r = |x: f64| {
            if x.is_finite() {
                JsonValue::Number((x * 10.0).round() / 10.0)
            } else {
                JsonValue::Null
            }
        };
        let live = m.active.load(Ordering::Relaxed);
        let mut o = JsonObject::from_pairs([
            ("on", self.post.on.into()),
            ("preset", JsonValue::str(self.post.chain.label())),
            ("chain", self.post.chain.json()),
            ("running", live.into()),
        ]);
        if live {
            o.set("gain_db", r(m.gain_db()));
            o.set("short_term_lufs", r(m.short_term_lufs()));
            o.set("limiter_db", r(m.limiter_db()));
            o.set("noise_dbfs", r(m.noise_db()));
            o.set(
                "latency_ms",
                r(m.latency_frames() as f64 / self.sample_rate * 1000.0),
            );
        }
        o.set(
            "presets",
            JsonValue::Array(PostChain::preset_names().into_iter().map(JsonValue::str).collect()),
        );
        JsonValue::Object(o)
    }

    /// The live op that sets the post as it is now, for an arrangement's `start` (nothing when
    /// it is off and on the default chain).
    pub fn post_setup_ops(&self) -> Vec<ArrangeOp> {
        if self.post == PostState::default() {
            return Vec::new();
        }
        vec![ArrangeOp::Live {
            method: "post".into(),
            params: self.post.op_params(),
        }]
    }
}
