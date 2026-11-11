//! `OfflineConductor` (`Session/SessionPlayer.swift`): steps an offline engine through time the
//! way the live app does, in fixed blocks. Apply the inputs due, run the 100 Hz housekeeping
//! (the controller router's jog-ring decay, every deck's `service`, the loop takes, Wander),
//! render a block. Both [`render_session`](crate::host::replay::render_session) and the tests that
//! perform a set use it, so a live run and its replay take exactly the same steps.
//!
//! The pieces Swift keeps in the engine (`AVAudioEngine`'s manual rendering) are two here: the
//! [`Engine`] the main thread drives (inside a [`Rig`], with the controller router) and the
//! [`Renderer`] that renders a block. The conductor owns both.
//!
//! The command ring holds 512 messages and a full ring drops the rest ([`Engine::dropped`]): a
//! restore, which makes a great many calls between blocks, [`pump`](OfflineConductor::pump)s the
//! ring empty between its phases. A conductor that has dropped a command says so
//! ([`OfflineConductor::dropped`]).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::deck::turntable::SampleBuffer;

use crate::deck::clips::LoopClip;
use crate::journal::state::JournalState;
use crate::session::action::DeckAction;
use crate::session::model::{SessionEvent, SessionEventKind};
use crate::session::player::{Conductor, BLOCK_FRAMES};

use super::engine::{Engine, Renderer};
use super::perform::PerformCtx;
use super::rig::Rig;

/// `OfflineConductor`.
pub struct OfflineConductor {
    /// The engine and the controller router (`dj` and `router`).
    pub rig: Rig,
    pub renderer: Renderer,
    /// `housekeepingEvery`: `max(1, Int(sampleRate / 100))` frames.
    every: i64,
    /// `nextHousekeeping`.
    next_housekeeping: i64,
    /// The ports the router was attached to (a replay attaches one on its first message).
    attached: BTreeSet<String>,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl OfflineConductor {
    /// `OfflineConductor.blockFrames`.
    pub const BLOCK_FRAMES: usize = BLOCK_FRAMES;

    pub fn new(engine: Engine, renderer: Renderer) -> OfflineConductor {
        let sr = engine.sample_rate;
        OfflineConductor {
            rig: Rig::new(engine),
            renderer,
            every: ((sr / 100.0) as i64).max(1),
            next_housekeeping: 0,
            attached: BTreeSet::new(),
            l: vec![0.0; Self::BLOCK_FRAMES],
            r: vec![0.0; Self::BLOCK_FRAMES],
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.rig.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.rig.engine
    }

    /// `OfflineConductor.housekeepingEvery`: the frames between two housekeeping ticks (the 100 Hz
    /// poll: `max(1, Int(sampleRate / 100))`).
    pub fn housekeeping_every(&self) -> i64 {
        self.every
    }

    /// Engine time in seconds, what the router sees as "now" (`OfflineConductor.now`).
    pub fn now(&self) -> f64 {
        self.rig.engine.frame() as f64 / self.rig.engine.sample_rate
    }

    /// Commands the ring had no room for (nothing a replay does may lose one).
    pub fn dropped(&self) -> u64 {
        self.rig.engine.dropped
    }

    /// Hand the render thread what the engine has queued, without rendering: the host's
    /// `drain` between the phases of a restore ([`Renderer::drain_commands`]).
    pub fn pump(&mut self) {
        self.renderer.drain_commands();
    }

    /// `JournalState.restore(into:clip:)` into this engine, the ring pumped between its phases
    /// ([`Engine::restore_state`]). Nothing is recorded; the warnings name what had no audio.
    pub fn restore_state(
        &mut self,
        state: &JournalState,
        find: &dyn Fn(&str) -> Option<LoopClip>,
    ) -> Vec<String> {
        let OfflineConductor { rig, renderer, .. } = self;
        rig.engine
            .restore_state(state, find, &mut |_| renderer.drain_commands())
    }

    /// `DJEngine.reset(to:clip:)` on this engine, the ring pumped between its phases
    /// ([`Engine::reset_to`]).
    pub fn reset_to(
        &mut self,
        state: &JournalState,
        find: &dyn Fn(&str) -> Option<LoopClip>,
    ) -> Vec<String> {
        let OfflineConductor { rig, renderer, .. } = self;
        rig.engine
            .reset_to(state, find, &mut |_| renderer.drain_commands())
    }

    /// `OfflineConductor.step(inputs:)`: `inputs` runs first (apply whatever is due at the
    /// engine's frame), then the housekeeping tick if due, then the render. The block's left and
    /// right samples stay valid until the next step.
    pub fn step_with(&mut self, inputs: impl FnOnce(&mut OfflineConductor)) -> (&[f32], &[f32]) {
        inputs(self);
        let frame = self.rig.engine.frame();
        if frame >= self.next_housekeeping {
            self.next_housekeeping = frame + self.every;
            let now = self.now();
            // router.tick, the jog rings, every deck's `service` (and Wander's), the loop takes
            self.rig.tick(now);
        }
        self.renderer.render_block(&mut self.l, &mut self.r);
        (&self.l, &self.r)
    }

    /// `n` frames (at most [`BLOCK_FRAMES`](Self::BLOCK_FRAMES)) with no inputs: a short step
    /// to land the next input on its exact frame. The housekeeping tick as [`step_with`].
    ///
    /// [`step_with`]: OfflineConductor::step_with
    pub fn step_frames(&mut self, n: usize) -> (&[f32], &[f32]) {
        let n = n.clamp(1, Self::BLOCK_FRAMES);
        let frame = self.rig.engine.frame();
        if frame >= self.next_housekeeping {
            self.next_housekeeping = frame + self.every;
            let now = self.now();
            self.rig.tick(now);
        }
        self.renderer.render_block(&mut self.l[..n], &mut self.r[..n]);
        (&self.l[..n], &self.r[..n])
    }

    /// One block with no inputs.
    pub fn step_block(&mut self) -> (&[f32], &[f32]) {
        self.step_with(|_| {})
    }

    /// Render until the engine has reached `frame` (whole blocks).
    pub fn run_to(&mut self, frame: i64) {
        while self.rig.engine.frame() < frame {
            self.step_block();
        }
    }

    /// Route one raw message the way a replay does: the router is attached to the port on its
    /// first message (even where no profile owns it), then the message is routed at the engine's
    /// time (`SessionPlayer.apply(.midi)`, `Journal.apply(.midi)`).
    pub fn route_midi(&mut self, port: &str, bytes: &[u8]) {
        if !self.attached.contains(port) {
            self.rig.attach(port);
            self.attached.insert(port.to_string());
        }
        let now = self.now();
        self.rig.route(port, bytes, now);
        // (the rig keeps what it routed for a recorder stand-in: a replay has no use for it)
        self.rig.recorded.clear();
    }

    /// `dj.applyAction(a)` for a replay: nothing is recorded. A `clipImport`'s audio is looked up
    /// in the clip library, then through the engine's resolver (`buffer ?? sampleLibrary[id] ??
    /// sampleResolver?(id)`); a sample `load` finds its own through the resolver.
    pub fn apply_action(&mut self, a: &DeckAction) {
        let mut ctx = PerformCtx::default();
        if let DeckAction::ClipImport { id, .. } = a {
            ctx.buffer = self.rig.engine.resolve_sample(id);
        }
        self.rig.engine.apply_deck_action(a, ctx);
    }
}

impl Conductor for OfflineConductor {
    fn frame(&self) -> i64 {
        self.rig.engine.frame()
    }

    /// `SessionPlayer.apply`: hardware MIDI to the router, an action to `applyAction`.
    fn apply(&mut self, event: &SessionEvent) {
        match &event.kind {
            SessionEventKind::Midi { port, bytes } => self.route_midi(port, bytes),
            SessionEventKind::Action(a) => self.apply_action(a),
        }
    }

    fn step(&mut self) -> (Vec<f32>, Vec<f32>) {
        let (l, r) = self.step_block();
        (l.to_vec(), r.to_vec())
    }
}

// MARK: an arrangement, offline

/// How [`render_arrangement`] renders.
pub struct ArrangeRenderOptions {
    pub sample_rate: f64,
    /// The capture library `library` live ops and `library:ID` modulators read (None: the one
    /// the arrangement's `start.loops_dir` names, if any).
    pub library: Option<crate::host::stores::captures::CaptureLibrary>,
    /// Beats rendered after the last move (a tape stop's silence, a tail). Ignored with `beats`.
    pub tail_beats: f64,
    /// Render exactly this many beats instead.
    pub beats: Option<f64>,
    /// Resolves a sample deck's (or a clip import's) content id to its audio.
    pub samples: Option<crate::host::replay::SampleLookup>,
    /// Audio fed into input strips from beat 0, by label (its mono mix; a strip not named in
    /// `start.inputs` is made). An input with no feed is silent.
    pub inputs: Vec<(String, Arc<SampleBuffer>)>,
    /// Keep a WAV per mixer strip ([`ArrangeRender::stems`]).
    pub stems: bool,
    /// The app's path (`EngineConfig::extended`), as djapp plays; off: beats' parity engine.
    pub extended: bool,
    /// SoundFont voices, as djapp loads them (`sf2` builds).
    #[cfg(feature = "sf2")]
    pub voices: Option<Arc<crate::host::sampler::Library>>,
}

impl Default for ArrangeRenderOptions {
    fn default() -> Self {
        ArrangeRenderOptions {
            sample_rate: 44_100.0,
            library: None,
            tail_beats: 4.0,
            beats: None,
            samples: None,
            inputs: Vec::new(),
            stems: false,
            extended: true,
            #[cfg(feature = "sf2")]
            voices: None,
        }
    }
}

/// What [`render_arrangement`] made.
pub struct ArrangeRender {
    pub audio: crate::session::wav::RenderedAudio,
    /// The moves the engine refused, with their beat: a live op's reason, or a swap it could
    /// not make.
    pub refused: Vec<(f64, String)>,
    /// Commands the ring had no room for.
    pub dropped: u64,
    /// With `stems`: each strip's share of the master, by name (`A`, `B`, `loops`, an input's
    /// label, `vocoder`); they add up to `audio`.
    pub stems: Vec<(String, crate::session::wav::RenderedAudio)>,
}

/// The offline engine for `arr`: its decks (and A and B), the input strips its `start` names
/// and `opts` feeds, deterministic, not yet stood on its start.
pub fn arrangement_conductor(
    arr: &crate::session::arrangement::Arrangement,
    opts: &ArrangeRenderOptions,
) -> OfflineConductor {
    let mut decks: Vec<String> = arr.decks.keys().cloned().collect();
    for d in ["A", "B"] {
        if !decks.iter().any(|x| x == d) {
            decks.push(d.to_string());
        }
    }
    decks.sort();
    let mut labels: Vec<String> = arr.start.inputs.iter().map(|i| i.label.clone()).collect();
    for (l, _) in &opts.inputs {
        if !labels.contains(l) {
            labels.push(l.clone());
        }
    }
    let mut config = super::engine::EngineConfig::new(opts.sample_rate);
    config.decks = decks;
    config.extended = opts.extended;
    config.inputs = labels;
    config.history_max_beats = crate::history::MAX_BEATS;
    #[cfg(feature = "sf2")]
    {
        config.library = opts.voices.clone();
    }
    let (mut e, r) = super::engine::engine(config);
    e.deterministic = true;
    let mut c = OfflineConductor::new(e, r);
    for inp in &arr.start.inputs {
        if let (Some(k), Some(b)) = (c.rig.engine.input_index(&inp.label), inp.history_beats) {
            if c.rig.engine.inputs[k].history_beats != Some(b) {
                c.rig.engine.set_input_history_beats(k, b);
            }
        }
    }
    c
}

/// Stand `c` on `arr`'s start: the capture library (and the loop row from it), each deck
/// loaded with what it starts on, the knobs, the mixer, the setup moves, the decks that play
/// from beat 0. Returns the setup moves refused.
pub fn stand_on_start(
    c: &mut OfflineConductor,
    arr: &crate::session::arrangement::Arrangement,
    library: Option<crate::host::stores::captures::CaptureLibrary>,
    samples: Option<crate::host::replay::SampleLookup>,
) -> Vec<(f64, String)> {
    use crate::session::arrangement::ArrangeOp;
    let st = &arr.start;
    let library = library.or_else(|| {
        st.loops_dir
            .as_ref()
            .map(crate::host::stores::captures::CaptureLibrary::new)
    });
    let e = &mut c.rig.engine;
    if let Some(f) = samples {
        e.sample_resolver = Some(Box::new(move |id| f(id)));
    }
    let has_library = library.is_some();
    e.set_loop_library(library);
    if st.loop_row && has_library {
        e.library_load_row();
    }
    for (name, source) in &arr.decks {
        let mut ctx = PerformCtx::default();
        if let crate::session::model::DeckSource::Sample { id, .. } = source {
            ctx.buffer = e.resolve_sample(id);
        }
        e.apply_deck_action(
            &DeckAction::Load {
                deck: name.clone(),
                source: source.clone(),
            },
            ctx,
        );
    }
    c.pump();
    let e = &mut c.rig.engine;
    for (name, p) in &st.params {
        if name != "L" {
            e.apply_params(name, p);
        }
    }
    if let Some(v) = st.crossfader {
        e.set_crossfader(v);
    }
    if let Some(v) = st.master {
        e.set_master(v);
    }
    if let Some(v) = st.loop_gain {
        e.set_loop_gain(v);
    }
    let mut refused = Vec::new();
    for op in &st.setup {
        match op {
            ArrangeOp::Live { method, params } => {
                if let Err(why) = e.apply_live_op(method, params) {
                    refused.push((0.0, format!("start {method}: {why}")));
                }
            }
            op => {
                if !e.apply_arrange_op(op) {
                    refused.push((0.0, format!("start {} refused", op.op())));
                }
            }
        }
    }
    // the decks playing at beat 0: as recorded; else each one unless its first `play` turns it
    // on (a deck the arrangement starts itself was stopped)
    let names: Vec<String> = e.decks.iter().map(|d| d.name.clone()).collect();
    for d in names {
        let plays = match &st.playing {
            Some(p) => p.contains(&d),
            None => {
                let first = arr.events.iter().find_map(|ev| match &ev.op {
                    ArrangeOp::Deck(DeckAction::Play { deck, on }) if *deck == d => Some(*on),
                    _ => None,
                });
                match first {
                    Some(on) => !on,
                    None => arr.decks.contains_key(&d),
                }
            }
        };
        if plays {
            e.play(&d);
        }
    }
    c.pump();
    refused
}

/// An arrangement (`djrust.arrangement/1`) rendered offline, faster than real time and the same
/// every time (docs/HEADLESS.md): the engine djapp plays on ([`arrangement_conductor`]), stood
/// on the arrangement's `start` ([`stand_on_start`]), then every move handed to the engine on the
/// exact frame of its beat (a block is cut short there), at the arrangement's tempo, as
/// `djapp_log play` hands them out live, there on the window's next pass (a `swap` through `swap_to`, a live op through [`Engine::apply_live_op`], the rest
/// through `perform`). Beat 0 is frame 0. Renders to the last move plus `tail_beats`.
pub fn render_arrangement(
    arr: &crate::session::arrangement::Arrangement,
    opts: ArrangeRenderOptions,
) -> ArrangeRender {
    use crate::session::arrangement::ArrangeOp;
    use crate::session::wav::RenderedAudio;

    let sr = opts.sample_rate;
    let mut c = arrangement_conductor(arr, &opts);
    let ArrangeRenderOptions {
        library,
        samples,
        inputs,
        stems,
        tail_beats,
        beats,
        ..
    } = opts;
    let mut refused = stand_on_start(&mut c, arr, library, samples);
    // input feeds: the strip plays once its ring holds its target (20 ms, at least 1024
    // samples); keep it that far ahead, a block at a time
    let target = ((sr * 0.02) as usize).max(1024);
    let mut feeds: Vec<(rtrb::Producer<f32>, Vec<f32>, usize)> = Vec::new();
    // what each strip is fed: audio given (`opts.inputs`), else the file the set recorded
    // (`start.inputs[].file`, from beat 0; docs/HEADLESS.md, "Recording the inputs")
    let mut monos: Vec<(String, Vec<f32>)> = inputs
        .iter()
        .map(|(label, buf)| {
            let mono: Vec<f32> = buf
                .left
                .iter()
                .zip(buf.right())
                .map(|(a, b)| 0.5 * (a + b))
                .collect();
            (label.clone(), mono)
        })
        .collect();
    for inp in &arr.start.inputs {
        let Some(file) = inp.file.as_deref() else {
            continue;
        };
        if monos.iter().any(|(l, _)| *l == inp.label) {
            continue;
        }
        match crate::host::input_record::read_recording(std::path::Path::new(file), inp.file_start) {
            Ok((m, rate)) if rate as f64 == sr => monos.push((inp.label.clone(), m)),
            Ok((_, rate)) => refused.push((
                0.0,
                format!("input {}: {file} is {rate} Hz, the render {sr} Hz", inp.label),
            )),
            Err(e) => refused.push((0.0, format!("input {}: {e}", inp.label))),
        }
    }
    for (label, mono) in monos {
        let Some(k) = c.rig.engine.input_index(&label) else {
            continue;
        };
        let Some((tx, _)) = c.rig.engine.take_input_feed(k) else {
            continue;
        };
        feeds.push((tx, mono, 0));
    }
    let feed = |feeds: &mut Vec<(rtrb::Producer<f32>, Vec<f32>, usize)>, n: usize| {
        for (tx, mono, at) in feeds.iter_mut() {
            for _ in 0..n {
                let x = mono.get(*at).copied().unwrap_or(0.0);
                if tx.push(x).is_err() {
                    break;
                }
                *at += 1;
            }
        }
    };
    feed(&mut feeds, target);
    if stems {
        c.renderer.enable_stems();
    }
    let beats = beats.unwrap_or(arr.beats() + tail_beats.max(0.0));
    // each move lands at its beat at the tempo the arrangement was playing then
    // (`Arrangement::seconds_at`), not at one tempo for the whole set
    let want = (arr.seconds_at(beats) * sr).round().max(0.0) as usize;
    let mut left: Vec<f32> = Vec::with_capacity(want);
    let mut right: Vec<f32> = Vec::with_capacity(want);
    // each move on its own frame: the engine renders up to it, then takes it
    let frame_of = |at: f64| (arr.seconds_at(at) * sr).round() as i64;
    let mut next = 0;
    c.rig.engine.trace_via = "playback".into();
    while left.len() < want {
        let frame = c.rig.engine.frame();
        while next < arr.events.len() && frame_of(arr.events[next].at) <= frame {
            let ev = &arr.events[next];
            next += 1;
            let e = &mut c.rig.engine;
            match &ev.op {
                ArrangeOp::Live { method, params } => {
                    if let Err(why) = e.apply_live_op(method, params) {
                        refused.push((ev.at, format!("{method}: {why}")));
                    }
                    // a bound target that reaches the gear hub (`midiOut`, `loopRoute`…) needs
                    // the running app: an offline engine has no hub to take it
                    for a in std::mem::take(&mut e.hub_requests) {
                        if let crate::control::controller::Action::Target { id, .. } = a {
                            refused.push((ev.at, format!("target {id}: needs the running app (gear)")));
                        }
                    }
                }
                op => {
                    if !e.apply_arrange_op(op) {
                        refused.push((ev.at, format!("{} refused", op.op())));
                    }
                }
            }
        }
        let mut n = (want - left.len()).min(OfflineConductor::BLOCK_FRAMES);
        if let Some(ev) = arr.events.get(next) {
            n = n.min((frame_of(ev.at) - frame).max(1) as usize);
        }
        feed(&mut feeds, n);
        let (l, r) = c.step_frames(n);
        left.extend_from_slice(l);
        right.extend_from_slice(r);
    }
    left.truncate(want);
    right.truncate(want);
    let mut stem_audio = Vec::new();
    if let Some(st) = c.renderer.take_stems() {
        let labels: Vec<String> = c
            .rig
            .engine
            .inputs
            .iter()
            .map(|i| i.settings.label.clone())
            .collect();
        for (k, name) in st.names.iter().enumerate() {
            let name = match name
                .strip_prefix("input:")
                .and_then(|i| i.parse::<usize>().ok())
            {
                Some(i) => labels.get(i).cloned().unwrap_or_else(|| name.clone()),
                None => name.clone(),
            };
            let mut l = st.l[k].clone();
            let mut r = st.r[k].clone();
            l.truncate(want);
            r.truncate(want);
            stem_audio.push((
                name,
                RenderedAudio {
                    sample_rate: sr,
                    left: l,
                    right: r,
                },
            ));
        }
    }
    ArrangeRender {
        audio: RenderedAudio {
            sample_rate: sr,
            left,
            right,
        },
        refused,
        dropped: c.dropped(),
        stems: stem_audio,
    }
}
