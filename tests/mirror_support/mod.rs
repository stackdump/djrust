//! A bare `WaveDeckSource` for the mirror tests: the audio side (a `WaveRunner`, the command
//! queue, the block epilogue that publishes the mirrors) and the main side (the book of
//! mirrors, the macro record, the mute words, the role tables), split the way the Swift class
//! is, so the same tests can run on one thread (`Source`) or across two (`split`).
//!
//! What it stands in for is `WaveDeckSource` (`Beats/Wave/WaveDeckSource.swift`): `render`
//! applies the queued commands, renders the block, then runs the epilogue of lines 618-631 of
//! that file (`publishMarking`, the generation atomics, `macroMirror.publish`, `publishMutes`).
//! Where Swift's `WaveProject` owns its `MarkingMirror`, the mirror here travels with the
//! project in the `Load` command and the audio side keeps it by generation.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use djrust::wave::*;
use djrust::{compose, JsonObject, Project};

pub mod stress;

pub const ROLE_NAMES: [&str; 8] = [
    "kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp",
];
pub const DRUMS: [&str; 4] = ["kick", "snare", "hihat", "clap"];

/// `project(genre, seed, structure)` of the Swift tests ("loop": no structure override).
pub fn project(genre: &str, seed: f64, structure: &str) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if structure != "loop" {
        ov.set("structure", structure.into());
    }
    compose(genre, &ov.into())
}

/// A prepared project and what the main thread keeps of it: `WaveRunner.prepare` plus the
/// `WaveProject.marking` the Swift creates in its initialiser.
pub struct Prepared {
    pub project: WaveProject,
    pub mirror: Arc<MarkingMirror>,
    pub track_serial: i32,
    pub generation: i32,
    pub start_tick: i64,
}

/// `WaveRunner.prepare(project, sampleRate:, startTick:, trackSerial:)`.
pub fn prepare(p: &Project, rate: f64, start_tick: i64, track_serial: Option<i32>) -> Prepared {
    let wp = WaveRunner::prepare(p, rate, &WaveOptions::default(), start_tick);
    let track_serial = track_serial.unwrap_or_else(next_track_serial);
    let mirror = Arc::new(MarkingMirror::new(
        wp.generation,
        track_serial,
        wp.start_tick,
        &wp.graph,
        p,
    ));
    Prepared {
        generation: wp.generation,
        start_tick: wp.start_tick,
        track_serial,
        mirror,
        project: wp,
    }
}

/// What a test keeps of a project after it was handed to the audio side.
#[derive(Clone)]
pub struct Loaded {
    pub generation: i32,
    pub track_serial: i32,
    pub start_tick: i64,
    pub mirror: Arc<MarkingMirror>,
    pub layout: Arc<MarkingLayout>,
    /// The graph as it was prepared (its initial marking, or the one rebuilt for a section
    /// launch). The running graph belongs to the audio side: `Source::graph`.
    pub graph0: Arc<WaveGraph>,
}

pub enum Cmd {
    Load(Box<WaveProject>, Arc<MarkingMirror>),
    Play,
    Pause,
    Stop,
    Tempo(f64),
    Mute(i32, bool),
    RoleMute(i32, bool),
    Macro(NetMacro, i64),
    Panic,
    Freeze,
    Scrub(f64),
    Release,
}

/// The atomics the audio thread publishes next to the mirrors.
#[derive(Default)]
pub struct Shared {
    pub active_generation: AtomicI32,
    pub pending_generation: AtomicI32,
    pub playing: AtomicBool,
    pub macro_active: AtomicBool,
    pub tick: AtomicI64,
    pub mute_words: [AtomicU64; 4],
}

pub fn publish_mutes(shared: &Shared, muted: &[u8]) {
    let mut w = [0u64; 4];
    for (i, &m) in muted.iter().take(256).enumerate() {
        if m != 0 {
            w[i / 64] |= 1 << (i % 64);
        }
    }
    for (word, v) in shared.mute_words.iter().zip(w) {
        word.store(v, Ordering::Relaxed);
    }
}

pub struct AudioSide {
    pub runner: WaveRunner,
    rx: Receiver<Cmd>,
    mirrors: Vec<(i32, Arc<MarkingMirror>)>,
    macro_mirror: Arc<MacroMirror>,
    shared: Arc<Shared>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl AudioSide {
    /// `WaveDeckSource.render(frames:left:right:startFrame:)` (the net half).
    pub fn render(&mut self, frames: usize, start_frame: i64) {
        while let Ok(c) = self.rx.try_recv() {
            self.apply(c);
        }
        let (l, r) = (&mut self.left[..frames], &mut self.right[..frames]);
        self.runner.process_at(l, Some(r), frames, start_frame);
        let generation = self.runner.generation();
        if let Some(g) = self.runner.graph() {
            if let Some((_, m)) = self.mirrors.iter().find(|(gen, _)| *gen == generation) {
                m.publish(g, start_frame + frames as i64);
            }
        }
        let s = &*self.shared;
        s.active_generation.store(generation, Ordering::Relaxed);
        s.pending_generation
            .store(self.runner.pending_generation(), Ordering::Relaxed);
        s.tick.store(
            self.runner.graph().map_or(0, |g| g.tick),
            Ordering::Relaxed,
        );
        s.playing.store(self.runner.playing(), Ordering::Relaxed);
        s.macro_active
            .store(self.runner.macro_active(), Ordering::Relaxed);
        self.macro_mirror
            .publish(&self.runner.macro_snapshot(), generation);
        if let Some(g) = self.runner.graph() {
            publish_mutes(s, &g.muted);
        }
    }

    fn apply(&mut self, c: Cmd) {
        match c {
            Cmd::Load(p, m) => {
                self.mirrors.push((p.generation, m));
                self.runner.load_prepared(*p);
            }
            Cmd::Play => self.runner.play(),
            Cmd::Pause => self.runner.pause(),
            Cmd::Stop => self.runner.stop(),
            Cmd::Tempo(b) => self.runner.set_tempo(b),
            Cmd::Mute(h, m) => self.runner.set_mute_hash(h, m),
            Cmd::RoleMute(h, m) => self.runner.set_role_muted(h, m),
            Cmd::Macro(m, t) => self.runner.fire_macro(m, t),
            Cmd::Panic => self.runner.panic(),
            Cmd::Freeze => self.runner.freeze(),
            Cmd::Scrub(t) => self.runner.scrub(t),
            Cmd::Release => self.runner.release(),
        }
    }

    /// Let go of what the runner retired (the host does this off the audio thread).
    pub fn drain_retired(&mut self) {
        while self.runner.pop_retired().is_some() {}
    }
}

pub struct MainSide {
    tx: Sender<Cmd>,
    pub sample_rate: f64,
    pub macro_mirror: Arc<MacroMirror>,
    macro_last: MacroRead,
    pub book: MarkingBook,
    /// The latest project loaded: its generation and a copy of its graph (the tables are read;
    /// the state is the audio thread's).
    latest: Option<(i32, Arc<WaveGraph>)>,
    pub shared: Arc<Shared>,
}

pub struct Source {
    pub audio: AudioSide,
    pub main: MainSide,
}

impl std::ops::Deref for Source {
    type Target = MainSide;
    fn deref(&self) -> &MainSide {
        &self.main
    }
}

impl std::ops::DerefMut for Source {
    fn deref_mut(&mut self) -> &mut MainSide {
        &mut self.main
    }
}

impl Source {
    /// `WaveDeckSource(sampleRate:)`.
    pub fn new(sample_rate: f64) -> Source {
        let (tx, rx) = channel();
        let macro_mirror = Arc::new(MacroMirror::new());
        let shared = Arc::new(Shared::default());
        Source {
            audio: AudioSide {
                runner: WaveRunner::new(sample_rate, WaveOptions::default()),
                rx,
                mirrors: Vec::new(),
                macro_mirror: macro_mirror.clone(),
                shared: shared.clone(),
                left: vec![0.0; 4096],
                right: vec![0.0; 4096],
            },
            main: MainSide {
                tx,
                sample_rate,
                macro_mirror,
                macro_last: MacroRead::default(),
                book: MarkingBook::new(),
                latest: None,
                shared,
            },
        }
    }

    pub fn split(self) -> (AudioSide, MainSide) {
        (self.audio, self.main)
    }

    pub fn render(&mut self, frames: usize, start_frame: i64) {
        self.audio.render(frames, start_frame);
    }

    /// The graph the audio side is running, as a test peeks at `prepared.graph` in Swift
    /// (single-threaded tests only).
    pub fn graph(&self) -> &WaveGraph {
        self.audio.runner.graph().expect("a project is running")
    }
}

impl MainSide {
    pub fn send(&self, c: Cmd) {
        self.tx.send(c).expect("the audio side is gone");
    }

    /// `WaveDeckSource.load(_:)`: hand a compiled project to the audio side (immediately when
    /// stopped, at the next bar when playing).
    pub fn load(&mut self, prepared: Prepared) -> Loaded {
        let graph0 = Arc::new(prepared.project.graph.clone());
        let loaded = Loaded {
            generation: prepared.generation,
            track_serial: prepared.track_serial,
            start_tick: prepared.start_tick,
            layout: prepared.mirror.layout.clone(),
            mirror: prepared.mirror.clone(),
            graph0: graph0.clone(),
        };
        self.latest = Some((prepared.generation, graph0));
        self.book
            .remember(prepared.mirror.clone(), self.active_generation());
        // A stopped deck installs the project at once: show its starting mutes before the
        // first render publishes the real ones.
        if !self.is_playing() {
            publish_mutes(&self.shared, &prepared.project.graph.muted);
        }
        self.send(Cmd::Load(Box::new(prepared.project), prepared.mirror));
        loaded
    }

    pub fn play(&self) {
        self.send(Cmd::Play);
    }
    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }
    pub fn stop(&self) {
        self.send(Cmd::Stop);
    }
    pub fn set_tempo(&self, bpm: f64) {
        self.send(Cmd::Tempo(bpm));
    }
    pub fn set_mute(&self, net_id: &str, muted: bool) {
        self.send(Cmd::Mute(str_hash(net_id), muted));
    }
    pub fn set_role_muted(&self, role: &str, muted: bool) {
        self.send(Cmd::RoleMute(str_hash(role), muted));
    }
    /// `fire(_:ticks:)`, with the macro's own default length when `ticks` is None.
    pub fn fire(&self, m: NetMacro, ticks: Option<i64>) {
        self.send(Cmd::Macro(m, ticks.unwrap_or_else(|| m.ticks(None))));
    }
    pub fn panic(&self) {
        self.send(Cmd::Panic);
    }
    pub fn freeze(&self) {
        self.send(Cmd::Freeze);
    }
    pub fn scrub(&self, ticks: f64) {
        self.send(Cmd::Scrub(ticks));
    }
    pub fn release(&self) {
        self.send(Cmd::Release);
    }

    pub fn active_generation(&self) -> i32 {
        self.shared.active_generation.load(Ordering::Relaxed)
    }
    pub fn pending_generation(&self) -> i32 {
        self.shared.pending_generation.load(Ordering::Relaxed)
    }
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }
    pub fn is_macro_active(&self) -> bool {
        self.shared.macro_active.load(Ordering::Relaxed)
    }
    pub fn tick(&self) -> i64 {
        self.shared.tick.load(Ordering::Relaxed)
    }
    pub fn latest_generation(&self) -> Option<i32> {
        self.latest.as_ref().map(|l| l.0)
    }

    // ---- the macro mirror (main thread)

    /// `WaveDeckSource.macroRead`: the last whole macro record.
    pub fn macro_read(&mut self) -> MacroRead {
        if let Some(r) = self.macro_mirror.read() {
            self.macro_last = r;
        }
        self.macro_last
    }

    pub fn active_macros(&mut self) -> Vec<NetMacro> {
        self.macro_read().snapshot.active_macros()
    }
    pub fn is_active(&mut self, m: NetMacro) -> bool {
        self.macro_read().snapshot.is_active(m)
    }
    pub fn is_net_held_by_macro(&mut self, i: i64) -> bool {
        self.macro_read().snapshot.is_held(i)
    }

    /// Is net `i` of the latest project muted right now? (Mute mirror.)
    pub fn is_net_muted(&self, i: usize) -> bool {
        i < 256 && self.shared.mute_words[i / 64].load(Ordering::Relaxed) & (1 << (i % 64)) != 0
    }

    /// Does the latest project have this role (a riff group or a plain net)?
    pub fn has_role(&self, role: &str) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|(_, g)| g.role_net_indices(str_hash(role)).is_some())
    }

    /// The roles of this song, in mixer order.
    pub fn roles(&self) -> Vec<&'static str> {
        ROLE_NAMES
            .iter()
            .copied()
            .filter(|r| self.has_role(r))
            .collect()
    }

    /// Is the role making sound right now? Reads the mute mirror and the latest project's role
    /// table (`isRoleAudible`).
    pub fn is_role_audible(&self, role: &str) -> bool {
        let Some((_, g)) = &self.latest else {
            return false;
        };
        let Some(nets) = g.role_net_indices(str_hash(role)) else {
            return false;
        };
        nets.iter().any(|&i| !self.is_net_muted(i))
    }

    pub fn roles_held(&mut self, m: NetMacro, among: &[&'static str]) -> Vec<&'static str> {
        let read = self.macro_read();
        let latest = self.latest.as_ref().map(|(g, gr)| (*g, &**gr));
        roles_held(
            &read,
            latest,
            among,
            |s, n| s.is_held_by(n, m),
            |i| self.is_net_muted(i),
        )
    }

    pub fn roles_held_by_macros(&mut self, among: &[&'static str]) -> Vec<&'static str> {
        let read = self.macro_read();
        let latest = self.latest.as_ref().map(|(g, gr)| (*g, &**gr));
        roles_held(&read, latest, among, |s, n| s.is_held(n), |i| self.is_net_muted(i))
    }

    // ---- the marking mirror (main thread)

    pub fn read_marking(&self, snap: &mut MarkingSnapshot) -> bool {
        self.book.read_marking(
            snap,
            self.active_generation(),
            self.is_playing(),
            self.pending_generation(),
        )
    }
    pub fn read_marking_of(&self, generation: i32, snap: &mut MarkingSnapshot) -> bool {
        self.book.read_marking_of(generation, snap)
    }
    pub fn marking_layout(&self) -> Option<Arc<MarkingLayout>> {
        self.book.latest_layout()
    }
    pub fn marking_layout_of(&self, generation: i32) -> Option<Arc<MarkingLayout>> {
        self.book.layout(generation)
    }
}

/// Drives a `Source` block by block and remembers where it is (`Driver` in the Swift tests).
pub struct Driver {
    pub src: Source,
    pub frame: i64,
    pub block: usize,
}

impl Driver {
    pub fn new(src: Source) -> Driver {
        Driver {
            src,
            frame: 0,
            block: 128,
        }
    }

    pub fn run(&mut self, blocks: usize) -> i64 {
        self.run_each(blocks, |_| {})
    }

    pub fn run_each(&mut self, blocks: usize, mut each: impl FnMut(&mut Source)) -> i64 {
        for _ in 0..blocks {
            self.src.render(self.block, self.frame);
            self.frame += self.block as i64;
            each(&mut self.src);
        }
        self.frame
    }
}

/// A source at `tempo` BPM (300: a tick every 50 ms, 1102.5 frames at 22.05 kHz) with the
/// project loaded and playing (`makeSource` of `MacroMirrorTests`).
pub fn make_source(p: &Project, tempo: f64, rate: f64) -> (Source, Loaded) {
    let mut src = Source::new(rate);
    let loaded = src.load(prepare(p, rate, 0, None));
    src.set_tempo(tempo);
    src.play();
    (src, loaded)
}

/// Does the snapshot hold exactly the graph's tokens and mutes, in the layout's order?
/// (`matches` of `MarkingMirrorTests`.)
pub fn matches(snap: &MarkingSnapshot, g: &WaveGraph) -> bool {
    let Some(layout) = &snap.layout else {
        return false;
    };
    if layout.net_count() != g.nets.len() {
        return false;
    }
    for (i, n) in g.nets.iter().enumerate() {
        if layout.net_ids[i] != n.id || layout.place_ids[i] != n.place_ids {
            return false;
        }
        for p in 0..n.n_places {
            if snap.tokens[layout.place_offsets[i] + p] != n.state[p] {
                return false;
            }
        }
        if snap.muted[i] != g.muted[i] {
            return false;
        }
    }
    snap.tick() == g.tick
}
