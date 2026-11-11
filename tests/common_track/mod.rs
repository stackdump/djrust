//! A mirror of `OfflineConductor.step` for the tests of the track pipeline: blocks of 128 frames
//! rendered through the real [`Renderer`], and the 100 Hz housekeeping tick (every
//! `max(1, sr / 100)` frames, before the block that finds it due) in front of the render. The
//! housekeeping is `Engine::service` (the decks' `service()`: the composes that finished, the
//! end of a song, the next track).

#![allow(dead_code)]

use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

/// `OfflineConductor.blockFrames`.
pub const BLOCK: usize = 128;

pub struct Stepper {
    pub e: Engine,
    pub r: Renderer,
    pub sr: f64,
    /// Frames rendered.
    pub frame: i64,
    next_housekeeping: i64,
    every: i64,
    pub l: Vec<f32>,
    pub rr: Vec<f32>,
    /// The loudest sample of the blocks rendered since `peak` was last read.
    peak: f32,
}

impl Stepper {
    /// Two decks, A and B. `extended`: the app's engine (the musicality pass on what it
    /// composes); off, beats' plain wave engine.
    pub fn new(sr: f64, extended: bool) -> Stepper {
        let mut c = EngineConfig::new(sr);
        c.extended = extended;
        let (e, r) = engine(c);
        Stepper {
            e,
            r,
            sr,
            frame: 0,
            next_housekeeping: 0,
            every: ((sr / 100.0) as i64).max(1),
            l: vec![0.0; BLOCK],
            rr: vec![0.0; BLOCK],
            peak: 0.0,
        }
    }

    /// One block: the housekeeping if due, then the render.
    pub fn block(&mut self) {
        if self.frame >= self.next_housekeeping {
            self.next_housekeeping = self.frame + self.every;
            self.e.service();
        }
        self.r.render_block(&mut self.l, &mut self.rr);
        for v in self.l.iter().chain(&self.rr) {
            self.peak = self.peak.max(v.abs());
        }
        self.frame += BLOCK as i64;
    }

    pub fn step(&mut self, blocks: usize) {
        for _ in 0..blocks {
            self.block();
        }
    }

    /// Render until `done` holds, at most `max_blocks` blocks; whether it did.
    pub fn until(&mut self, max_blocks: usize, mut done: impl FnMut(&mut Stepper) -> bool) -> bool {
        for _ in 0..max_blocks {
            if done(self) {
                return true;
            }
            self.block();
        }
        done(self)
    }

    /// The loudest sample since the last call.
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.peak)
    }

    /// The deck's net clock as the last block left it.
    pub fn tick(&self, deck: &str) -> i64 {
        self.e.deck(deck).unwrap().mirror.tick()
    }

    /// Render until deck `deck` has reached `tick` (a generous bound).
    pub fn run_to_tick(&mut self, deck: &str, tick: i64) {
        let d = deck.to_string();
        assert!(
            self.until(40_000, |s| s.tick(&d) >= tick),
            "deck {deck} never reached tick {tick}"
        );
    }

    /// Render until the deck plays what was loaded last: the project the audio thread runs is the
    /// one the deck loaded (generous: it takes a bar at most).
    pub fn until_installed(&mut self, deck: &str) -> bool {
        let d = deck.to_string();
        self.until(40_000, |s| {
            let dk = s.e.deck(&d).unwrap();
            let latest = dk.track.latest.as_ref().map(|l| l.generation);
            latest.is_some_and(|g| {
                dk.mirror
                    .generation
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == g
            })
        })
    }
}
