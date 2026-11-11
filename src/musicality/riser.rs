//! Port of `Musicality/Riser.swift` (`.riser`, p62-riser): a noise riser over the last two
//! bars of each buildup into a drop. A runner-owned stinger-class lane: seeded xorshift noise
//! through a state-variable lowpass swept 300 Hz → 8 kHz while its level climbs −30 → −12 dB,
//! cut at the drop.

use alloc::vec::Vec;

use crate::pflow::Project;
use crate::wave::synth::{biquad, BUTTERWORTH_Q};

/// How long a riser lasts: the last two bars of the buildup, in sixteenths.
pub const RISER_STEPS: i64 = 32;
const RISER_FROM: [&str; 1] = ["buildup"];
const RISER_INTO: [&str; 2] = ["drop", "chorus"];

/// `riserWindows`: song steps `from ..< to`, `to` the first step of the drop (or chorus) a
/// buildup lands on. Ascending, disjoint.
pub fn riser_windows(p: &Project) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut t = 0;
    for (k, s) in p.structure.iter().enumerate() {
        if k > 0
            && RISER_INTO.contains(&s.name.as_str())
            && RISER_FROM.contains(&p.structure[k - 1].name.as_str())
        {
            let len = RISER_STEPS.min(p.structure[k - 1].steps);
            if len > 0 {
                out.push((t - len, t));
            }
        }
        t += s.steps;
    }
    out
}

/// `RiserSpec`: the riser's sweep.
pub struct RiserSpec;

impl RiserSpec {
    pub const FROM_HZ: f64 = 300.0;
    pub const TO_HZ: f64 = 8000.0;
    pub const FROM_DB: f64 = -30.0;
    pub const TO_DB: f64 = -12.0;
    /// Its share of the room send (`.space`).
    pub const SEND: f64 = 0.3;
    /// Side over mid at the start and at the end of the window (`.stereo`).
    pub const FROM_WIDTH: f64 = 0.2;
    pub const TO_WIDTH: f64 = 0.8;
    /// Time constant of the level's glide (ms).
    pub const GLIDE_MS: f64 = 3.0;

    /// Cutoff (Hz) and level (dB) at `x` (0…1) through the window.
    pub fn at(x: f64) -> (f64, f64) {
        let x = x.clamp(0.0, 1.0);
        (
            Self::FROM_HZ * libm::pow(Self::TO_HZ / Self::FROM_HZ, x),
            Self::FROM_DB + (Self::TO_DB - Self::FROM_DB) * x,
        )
    }
}

/// The riser lane of one project: built at prepare, rendered one sample at a time.
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct RiserVoice {
    starts: Vec<i64>,
    ends: Vec<i64>,
    sample_rate: f64,
    stereo: bool,
    pub send: f64,
    inv_sr: f64,
    glide: f64,
    ln_sweep: f64,
    index: usize,
    gain: f64,
    rng_mid: u32,
    rng_side: u32,
    m1: f64,
    m2: f64,
    s1: f64,
    s2: f64,
    /// What the last `render` produced: mid, side (0 without `.stereo`).
    pub mid: f64,
    pub side: f64,
}

impl RiserVoice {
    pub fn new(
        windows: &[(i64, i64)],
        sample_rate: f64,
        stereo: bool,
        space: bool,
    ) -> Option<RiserVoice> {
        if windows.is_empty() || sample_rate <= 0.0 {
            return None;
        }
        Some(RiserVoice {
            starts: windows.iter().map(|w| w.0).collect(),
            ends: windows.iter().map(|w| w.1).collect(),
            sample_rate,
            stereo,
            send: if space { RiserSpec::SEND } else { 0.0 },
            inv_sr: 1.0 / sample_rate,
            glide: 1.0 - libm::exp(-1.0 / (RiserSpec::GLIDE_MS / 1000.0 * sample_rate)),
            ln_sweep: libm::log(RiserSpec::TO_HZ / RiserSpec::FROM_HZ),
            index: 0,
            gain: 0.0,
            rng_mid: 1,
            rng_side: 1,
            m1: 0.0,
            m2: 0.0,
            s1: 0.0,
            s2: 0.0,
            mid: 0.0,
            side: 0.0,
        })
    }

    /// Back to silence, from the top (a stop, an install).
    pub fn reset(&mut self) {
        self.index = 0;
        self.gain = 0.0;
        self.m1 = 0.0;
        self.m2 = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.mid = 0.0;
        self.side = 0.0;
    }

    /// One sample at song position `pos` (steps, fractional; None: not playing). Returns
    /// false, touching nothing, while it is silent.
    pub fn render(&mut self, pos: Option<f64>) -> bool {
        let mut target = 0.0;
        let mut x = 0.0;
        if let Some(pos) = pos {
            let n = self.starts.len();
            if self.index > n || (self.index > 0 && self.ends[self.index - 1] as f64 > pos) {
                self.index = 0;
            }
            while self.index < n && self.ends[self.index] as f64 <= pos {
                self.index += 1;
            }
            if self.index < n && self.starts[self.index] as f64 <= pos {
                let a = self.starts[self.index] as f64;
                x = (pos - a) / (self.ends[self.index] as f64 - a);
                target = libm::pow(
                    10.0,
                    (RiserSpec::FROM_DB + (RiserSpec::TO_DB - RiserSpec::FROM_DB) * x) / 20.0,
                );
                if self.gain == 0.0 {
                    self.enter(self.starts[self.index]);
                }
            }
        }
        if self.gain == 0.0 && target == 0.0 {
            return false;
        }
        self.gain += (target - self.gain) * self.glide;
        if target == 0.0 && self.gain < 1e-6 {
            self.reset();
            return false;
        }
        let hz = RiserSpec::FROM_HZ * libm::exp(self.ln_sweep * x.clamp(0.0, 1.0));
        // trapezoidal SVF lowpass, Butterworth (k = √2)
        let g = libm::tan(core::f64::consts::PI * hz * self.inv_sr);
        let k = libm::sqrt(2.0);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        // white uniform noise (variance 1/3) through a Butterworth lowpass keeps 1.11 fc /
        // (sr / 2) of its power: scaled back so `gain` is the riser's RMS
        let norm = self.gain * libm::sqrt(3.0 * 0.5 * self.sample_rate / (1.11 * hz));
        let n_mid = Self::noise(&mut self.rng_mid);
        self.mid = norm * Self::lowpass(n_mid, &mut self.m1, &mut self.m2, a1, a2, a3);
        if self.stereo {
            let w = RiserSpec::FROM_WIDTH + (RiserSpec::TO_WIDTH - RiserSpec::FROM_WIDTH) * x;
            let n_side = Self::noise(&mut self.rng_side);
            self.side = w * norm * Self::lowpass(n_side, &mut self.s1, &mut self.s2, a1, a2, a3);
        }
        true
    }

    /// A window starts from silence: fresh filters, and noise seeded by the window.
    fn enter(&mut self, start: i64) {
        self.m1 = 0.0;
        self.m2 = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.rng_mid = ((start as i128 * 2_654_435_761i128) as u32) | 1;
        self.rng_side = (self.rng_mid ^ 0x9E37_79B9) | 1;
    }

    fn noise(x: &mut u32) -> f64 {
        *x ^= *x << 13;
        *x ^= *x >> 17;
        *x ^= *x << 5;
        (*x as i32) as f64 / 2_147_483_648.0
    }

    fn lowpass(v: f64, ic1: &mut f64, ic2: &mut f64, a1: f64, a2: f64, a3: f64) -> f64 {
        let v3 = v - *ic2;
        let v1 = a1 * *ic1 + a2 * v3;
        let v2 = *ic2 + a2 * *ic1 + a3 * v3;
        *ic1 = 2.0 * v1 - *ic1;
        *ic2 = 2.0 * v2 - *ic2;
        v2
    }
}

/// `RiserRise`: how a render's high band climbs through each riser window.
#[derive(Clone, Debug, PartialEq)]
pub struct RiserRise {
    /// The window, in song steps.
    pub window: (i64, i64),
    /// High-band (≥ `HIGH_HZ`) RMS of each beat of the window, dB.
    pub beats_db: Vec<f64>,
}

impl RiserRise {
    pub const HIGH_HZ: f64 = 4000.0;

    /// Last beat over the first, dB.
    pub fn rise_db(&self) -> f64 {
        self.beats_db.last().copied().unwrap_or(0.0) - self.beats_db.first().copied().unwrap_or(0.0)
    }

    /// Spearman rank correlation of the beats' levels with time.
    pub fn spearman(&self) -> f64 {
        Self::spearman_of(&self.beats_db)
    }

    /// Each riser window of `project` that the mono-folded render `left` / `right` holds whole.
    pub fn measure(
        left: &[f32],
        right: &[f32],
        sample_rate: f64,
        project: &Project,
    ) -> Vec<RiserRise> {
        let windows = riser_windows(project);
        let n = left.len().min(right.len());
        if windows.is_empty() || n == 0 {
            return Vec::new();
        }
        let tempo = if project.tempo > 0.0 {
            project.tempo
        } else {
            120.0
        };
        let per_step = sample_rate * 60.0 / (tempo * 4.0);
        let mut hi: Vec<f64> = (0..n)
            .map(|i| (left[i] as f64 + right[i] as f64) / 2.0)
            .collect();
        for _ in 0..2 {
            let b = biquad("highpass", Self::HIGH_HZ, BUTTERWORTH_Q, sample_rate);
            let (mut z1, mut z2) = (0.0, 0.0);
            for v in hi.iter_mut() {
                let x = *v;
                let y = b[0] * x + z1;
                z1 = b[1] * x - b[3] * y + z2;
                z2 = b[2] * x - b[4] * y;
                *v = y;
            }
        }
        let mut out = Vec::new();
        for w in windows {
            if (w.1 as f64 * per_step) as usize > n {
                continue;
            }
            let mut beats = Vec::new();
            let mut s = w.0;
            while s + 4 <= w.1 {
                let a = (s as f64 * per_step) as usize;
                let b = ((s + 4) as f64 * per_step) as usize;
                let mut e = 0.0;
                for i in a..b {
                    e += hi[i] * hi[i];
                }
                beats.push(10.0 * libm::log10((e / (b - a).max(1) as f64).max(1e-20)));
                s += 4;
            }
            if beats.len() >= 2 {
                out.push(RiserRise {
                    window: w,
                    beats_db: beats,
                });
            }
        }
        out
    }

    /// Spearman's ρ of `ys` against their index (ties ranked by their mean).
    pub fn spearman_of(ys: &[f64]) -> f64 {
        let n = ys.len();
        if n < 2 {
            return 0.0;
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            ys[a]
                .partial_cmp(&ys[b])
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        let mut rank = alloc::vec![0.0; n];
        let mut i = 0;
        while i < n {
            let mut j = i;
            while j + 1 < n && ys[order[j + 1]] == ys[order[i]] {
                j += 1;
            }
            for k in i..=j {
                rank[order[k]] = (i + j) as f64 / 2.0;
            }
            i = j + 1;
        }
        let mean = (n - 1) as f64 / 2.0;
        let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
        for k in 0..n {
            let a = k as f64 - mean;
            let b = rank[k] - mean;
            num += a * b;
            dx += a * a;
            dy += b * b;
        }
        if dx > 0.0 && dy > 0.0 {
            num / libm::sqrt(dx * dy)
        } else {
            0.0
        }
    }
}
