//! The generator's random sources, port of `Beats/JSRandom.swift` and the `LCG` in
//! `Generator/Core.swift`. Both reproduce the JS streams bit for bit (`|0`,
//! `Math.imul`, `>>>`, and the LCG's double arithmetic).

use crate::json::{floor, js_to_int32};

/// mulberry32 as beats' `createRng` (generator/core.js) writes it.
#[derive(Clone, Debug)]
pub struct Mulberry32 {
    s: i32,
}

impl Mulberry32 {
    /// `seed | 0`: pass the JS number; it is reduced with ToInt32.
    pub fn new(seed: f64) -> Self {
        Mulberry32 {
            s: js_to_int32(seed),
        }
    }

    pub fn from_i32(seed: i32) -> Self {
        Mulberry32 { s: seed }
    }

    /// `rng.next()`: a double in [0, 1). (Named after the JS; not an `Iterator`.)
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f64 {
        self.s = self.s.wrapping_add(0x6D2B_79F5);
        let s = self.s;
        let mut t = (s ^ ((s as u32) >> 15) as i32).wrapping_mul(1 | s);
        t = (t.wrapping_add((t ^ ((t as u32) >> 7) as i32).wrapping_mul(61 | t))) ^ t;
        let r = (t ^ ((t as u32) >> 14) as i32) as u32;
        r as f64 / 4_294_967_296.0
    }

    /// `Math.floor(next() * max)`
    pub fn next_int(&mut self, max: usize) -> usize {
        floor(self.next() * max as f64) as usize
    }

    /// `Math.floor(next() * 2147483647)`
    pub fn next_int63(&mut self) -> i64 {
        floor(self.next() * 2_147_483_647.0) as i64
    }
}

/// The "simple LCG" variety.js uses for ghost notes / walking bass / call-response.
/// All arithmetic is done in doubles then ToInt32'd, exactly as JS does, so large
/// seeds lose precision in the same places.
#[derive(Clone, Debug)]
pub struct Lcg {
    state: f64,
}

impl Lcg {
    pub fn new(seed: f64) -> Self {
        Lcg { state: seed }
    }
    pub fn float64(&mut self) -> f64 {
        let t = self.state * 1_664_525.0 + 1_013_904_223.0;
        self.state = (js_to_int32(t) & 0x7fff_ffff) as f64;
        self.state / 2_147_483_648.0
    }
    pub fn intn(&mut self, n: i64) -> i64 {
        floor(self.float64() * n as f64) as i64
    }
}

/// What the shared markov helpers draw from: either source.
pub trait RandomSource {
    fn float64(&mut self) -> f64;
}

impl RandomSource for Mulberry32 {
    fn float64(&mut self) -> f64 {
        self.next()
    }
}

impl RandomSource for Lcg {
    fn float64(&mut self) -> f64 {
        Lcg::float64(self)
    }
}
