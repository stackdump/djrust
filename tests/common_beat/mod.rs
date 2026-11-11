//! A mirror of `OfflineConductor.step` for the tests that hold a deck to the Swift one: blocks
//! of 128 frames, the 100 Hz housekeeping tick (every `sr / 100` frames, before the block that
//! finds it due) in front of the render. The decks are rendered through the real
//! [`Renderer`](djrust::host::deck::Renderer); the housekeeping is the beat clock's
//! ([`Engine::service_beat`](djrust::host::deck::Engine::service_beat)).

#![allow(dead_code)]

use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

pub const SR: f64 = 48_000.0;
/// `OfflineConductor.blockFrames`.
pub const BLOCK: usize = 128;

pub struct Stepper {
    pub e: Engine,
    pub r: Renderer,
    /// Frames rendered.
    pub frame: i64,
    next_housekeeping: i64,
    every: i64,
    l: Vec<f32>,
    rr: Vec<f32>,
}

impl Stepper {
    /// Two decks, A and B, the plain wave engine (no musicality pass: ticks do not depend on it).
    pub fn new() -> Stepper {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (e, r) = engine(c);
        Stepper {
            e,
            r,
            frame: 0,
            next_housekeeping: 0,
            every: ((SR / 100.0) as i64).max(1),
            l: vec![0.0; BLOCK],
            rr: vec![0.0; BLOCK],
        }
    }

    /// One block: the housekeeping if due, then the render.
    pub fn block(&mut self) {
        if self.frame >= self.next_housekeeping {
            self.next_housekeeping = self.frame + self.every;
            self.e.service_beat();
        }
        self.r.render_block(&mut self.l, &mut self.rr);
        self.frame += BLOCK as i64;
    }

    pub fn step(&mut self, blocks: usize) {
        for _ in 0..blocks {
            self.block();
        }
    }
}
