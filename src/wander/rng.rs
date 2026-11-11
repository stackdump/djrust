//! The deterministic draw (`WanderRandom`): SplitMix64 seeded from the spec's share CID and
//! the step.

use crate::cid::share_cid;
use crate::spec::TrackSpec;

#[derive(Clone, Debug)]
pub struct WanderRandom {
    state: u64,
}

impl WanderRandom {
    pub fn new(spec: &TrackSpec, step: i64) -> Self {
        Self::with_base(Self::base(spec), step)
    }

    pub fn with_base(base: u64, step: i64) -> Self {
        WanderRandom {
            state: base ^ (step as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        }
    }

    /// FNV-1a of the spec's share CID: what every draw on this configuration starts from.
    pub fn base(spec: &TrackSpec) -> u64 {
        Self::base_of_cid(&share_cid(&spec.payload()))
    }

    /// The same from any share CID (a raw-nets deck's payload).
    pub fn base_of_cid(cid: &str) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in cid.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    pub fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0..<n
    pub fn int(&mut self, n: i64) -> i64 {
        if n <= 1 {
            0
        } else {
            (self.next() % n as u64) as i64
        }
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.int(xs.len() as i64) as usize]
    }
}
