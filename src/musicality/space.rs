//! Port of `Musicality/Space.swift` (`.space`, `.shimmer`): the room a deck plays in. A
//! send reverb as a mixing desk has: each lane sends some of itself, the return comes back
//! in stereo. `SpaceSpec` is data; `Shimmer` and `FdnReverb` are the runner-owned DSP blocks
//! (buffers allocated at construction, `tune` / `process` allocate nothing).

use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::mix_targets::MixTargets;

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct SpaceSpec {
    /// Reverb time (RT60), seconds.
    pub seconds: f64,
    /// Return level: what the reverb adds, as a gain on its normalised output.
    pub wet: f64,
    /// Lowpass in the reverb's loop, Hz.
    pub damping: f64,
    /// The pad shimmer's level (the pads' octave-up path into the room); 0 = none.
    pub shimmer: f64,
}

impl SpaceSpec {
    pub fn new(seconds: f64, wet: f64, damping: f64) -> SpaceSpec {
        SpaceSpec {
            seconds,
            wet,
            damping,
            shimmer: 0.0,
        }
    }

    /// `SpaceSpec.room`: a neutral medium room (`targets.json`'s `space.default`).
    pub fn room() -> SpaceSpec {
        MixTargets::shared().default_room()
    }

    /// `forGenre(_:shimmer:)`: the room for a genre.
    pub fn for_genre(name: &str, shimmer: bool) -> SpaceSpec {
        let mut s = MixTargets::shared().room(name);
        if shimmer {
            s.shimmer = MixTargets::shared().shimmer(name);
        }
        s
    }

    /// `send(kind:group:)`: how much of itself a lane sends to the room.
    pub fn send(kind: &str, group: &str) -> f64 {
        MixTargets::shared()
            .send(kind, group)
            .unwrap_or_else(|| Self::rule_of_thumb_send(kind, group))
    }

    /// The sends before `targets.json` held them: what a key missing from the table gets.
    pub fn rule_of_thumb_send(kind: &str, group: &str) -> f64 {
        match kind {
            "tonal" => match group {
                "harmony" | "pad" => 0.5,
                "arp" => 0.4,
                "bass" => 0.0,
                _ => 0.3,
            },
            "snare" => 0.22,
            "clap" => 0.3,
            "crash" => 0.25,
            "openhat" => 0.1,
            "hat" => 0.05,
            _ => 0.0,
        }
    }

    /// What the sampled instruments that send render as one bus send.
    pub fn sampler_send() -> f64 {
        MixTargets::shared().sampler_send().unwrap_or(0.35)
    }

    /// Which lanes the shimmer takes: synth pads.
    pub fn shimmers(kind: &str, group: &str) -> bool {
        kind == "tonal" && (group == "harmony" || group == "pad")
    }
}

/// The pad shimmer: the pads' room send, an octave up, at a low level, into the room. An
/// octave-up pitch shifter with feedback and a lowpass on its output.
pub struct Shimmer {
    buf: Vec<f32>,
    window: usize,
    w: usize,
    d: usize,
    last: f64,
    lp: f64,
    lp_a: f64,
    level: f64,
}

impl Shimmer {
    pub const WINDOW_MS: f64 = 60.0;
    pub const FEEDBACK: f64 = 0.35;
    pub const LOWPASS_HZ: f64 = 6000.0;

    pub fn new(sample_rate: f64) -> Shimmer {
        let window = ((Self::WINDOW_MS / 1000.0 * sample_rate) as usize).max(4);
        Shimmer {
            buf: vec![0.0; window + 1],
            window,
            w: 0,
            d: window,
            last: 0.0,
            lp: 0.0,
            lp_a: 1.0
                - libm::exp(
                    -2.0 * core::f64::consts::PI * Self::LOWPASS_HZ.min(sample_rate * 0.45)
                        / sample_rate,
                ),
            level: 0.0,
        }
    }

    pub fn level(&self) -> f64 {
        self.level
    }

    pub fn is_on(&self) -> bool {
        self.level > 0.0
    }

    /// Set its level (the room's `shimmer`; None / 0 = off).
    pub fn tune(&mut self, spec: Option<&SpaceSpec>) {
        self.level = spec.map_or(0.0, |s| s.shimmer).max(0.0);
    }

    /// Clear the delay line.
    pub fn reset(&mut self) {
        for x in self.buf.iter_mut() {
            *x = 0.0;
        }
        self.last = 0.0;
        self.lp = 0.0;
    }

    /// One sample of the pads' send in, one sample out for the room's input.
    pub fn process(&mut self, x: f64) -> f64 {
        let n = self.buf.len();
        self.buf[self.w] = (x + Self::FEEDBACK * self.last) as f32;
        let half = self.window / 2;
        let d2 = if self.d > half {
            self.d - half
        } else {
            self.d + half
        };
        let r1 = if self.w >= self.d {
            self.w - self.d
        } else {
            self.w + n - self.d
        };
        let r2 = if self.w >= d2 {
            self.w - d2
        } else {
            self.w + n - d2
        };
        let s = libm::sin(core::f64::consts::PI * self.d as f64 / self.window as f64);
        let g1 = s * s;
        let y = g1 * self.buf[r1] as f64 + (1.0 - g1) * self.buf[r2] as f64;
        self.last = y;
        self.lp += self.lp_a * (y - self.lp);
        self.w = if self.w + 1 == n { 0 } else { self.w + 1 };
        self.d = if self.d == 1 { self.window } else { self.d - 1 };
        self.lp * self.level
    }
}

/// The side highpass is `energy_curve`'s, the one `SideHighpass` of EnergyCurve.swift that
/// `FdnReverb` and the sampled slots both use.
pub use super::energy_curve::SideHighpass;

/// A stereo feedback-delay-network reverb: eight delay lines of mutually prime lengths,
/// mixed through a Hadamard matrix, each with a lowpass and the gain that makes it decay by
/// 60 dB in the room's time.
pub struct FdnReverb {
    sample_rate: f64,
    buf: Vec<f32>,
    stride: usize,
    len: [usize; 8],
    pos: [usize; 8],
    gain: [f64; 8],
    lp: [f64; 8],
    t: [f64; 8],
    damp_a: f64,
    pre: Vec<f32>,
    pre_pos: usize,
    hp_a: f64,
    /// Is the input highpass in the path? (The runner's room has it; the strip reverb does not.)
    hp_on: bool,
    hp_x: (f64, f64),
    hp_y: (f64, f64),
    side_hp: SideHighpass,
    /// Output gain: the spec's `wet`, normalised so a longer room is not simply louder.
    out_gain: f64,
    /// The FX unit's send gain on the input (1 = the genre's sends), ramped linearly.
    send: f64,
    send_target: f64,
    send_step: f64,
    send_left: usize,
}

impl FdnReverb {
    pub const LINE_MS: [f64; 8] = [29.7, 37.1, 41.1, 43.7, 53.0, 59.9, 67.7, 79.3];
    pub const PREDELAY_MS: f64 = 18.0;
    pub const HIGHPASS_HZ: f64 = 200.0;
    /// The return's side is highpassed higher.
    pub const SIDE_HZ: f64 = 300.0;
    /// Brings the normalised output up to `wet` = 1 meaning a return about as loud as what
    /// was sent.
    pub const MAKEUP: f64 = 12.0;

    pub fn new(sample_rate: f64) -> FdnReverb {
        Self::with_input_highpass(sample_rate, true)
    }

    /// The room with or without its input highpass ([`Self::HIGHPASS_HZ`], two poles). The
    /// runner's send reverb keeps it (`new`); a channel's reverb takes the whole signal, the
    /// lows too, as Apple's AUReverb2 does (`fx::strip_reverb`).
    pub fn with_input_highpass(sample_rate: f64, input_highpass: bool) -> FdnReverb {
        let sr = sample_rate;
        let max_ms = Self::LINE_MS.iter().copied().fold(0.0, f64::max);
        let stride = libm::ceil(max_ms / 1000.0 * sr) as usize + 1;
        let mut len = [1usize; 8];
        for i in 0..8 {
            len[i] = stride.min(((libm::round(Self::LINE_MS[i] / 1000.0 * sr)) as usize).max(1));
        }
        let rc = 1.0 / (2.0 * core::f64::consts::PI * Self::HIGHPASS_HZ);
        FdnReverb {
            sample_rate: sr,
            buf: vec![0.0; 8 * stride],
            stride,
            len,
            pos: [0; 8],
            gain: [0.0; 8],
            lp: [0.0; 8],
            t: [0.0; 8],
            damp_a: 1.0,
            pre: vec![0.0; ((Self::PREDELAY_MS / 1000.0 * sr) as usize).max(1)],
            pre_pos: 0,
            hp_a: rc / (rc + 1.0 / sr),
            hp_on: input_highpass,
            hp_x: (0.0, 0.0),
            hp_y: (0.0, 0.0),
            side_hp: SideHighpass::new(Self::SIDE_HZ, sr),
            out_gain: 0.0,
            send: 1.0,
            send_target: 1.0,
            send_step: 0.0,
            send_left: 0,
        }
    }

    pub fn out_gain(&self) -> f64 {
        self.out_gain
    }

    /// Set the room.
    pub fn tune(&mut self, spec: Option<&SpaceSpec>) {
        let Some(spec) = spec.filter(|s| s.wet > 0.0 && s.seconds > 0.0) else {
            self.out_gain = 0.0;
            return;
        };
        let mut mean = 0.0;
        for i in 0..8 {
            self.gain[i] = libm::pow(
                10.0,
                -3.0 * self.len[i] as f64 / (spec.seconds * self.sample_rate),
            );
            mean += self.gain[i] / 8.0;
        }
        self.damp_a = 1.0
            - libm::exp(
                -2.0 * core::f64::consts::PI * spec.damping.min(self.sample_rate * 0.45)
                    / self.sample_rate,
            );
        self.out_gain = Self::MAKEUP * spec.wet * libm::sqrt(1.0 - mean * mean);
    }

    /// Change only the decay, to `seconds` (beyond Swift: the reverb-freeze trick holds the room
    /// near-forever without the return's level following, as [`tune`](Self::tune) would make it).
    pub fn hold(&mut self, seconds: f64) {
        if !(seconds > 0.0) {
            return;
        }
        for i in 0..8 {
            self.gain[i] = libm::pow(10.0, -3.0 * self.len[i] as f64 / (seconds * self.sample_rate));
        }
    }

    /// The loop's low-pass at `hz`, or open (None: no damping at all, for a freeze). Beyond Swift.
    pub fn set_damping(&mut self, hz: Option<f64>) {
        self.damp_a = match hz {
            None => 1.0,
            Some(hz) => {
                1.0 - libm::exp(
                    -2.0 * core::f64::consts::PI * hz.min(self.sample_rate * 0.45)
                        / self.sample_rate,
                )
            }
        };
    }

    /// The FX unit's room send, reached linearly over the next `frames` samples.
    pub fn set_send(&mut self, g: f64, frames: usize) {
        self.send_target = g;
        self.send_left = frames.max(1);
        self.send_step = (g - self.send) / self.send_left as f64;
    }

    /// Clear the room (the tail stops).
    pub fn reset(&mut self) {
        for x in self.buf.iter_mut() {
            *x = 0.0;
        }
        for x in self.pre.iter_mut() {
            *x = 0.0;
        }
        self.lp = [0.0; 8];
        self.hp_x = (0.0, 0.0);
        self.hp_y = (0.0, 0.0);
        self.side_hp.reset();
    }

    /// One sample in (the send bus), one stereo sample out (the return).
    pub fn process(&mut self, x: f64) -> (f64, f64) {
        let mut x = x;
        if self.send_left > 0 {
            self.send_left -= 1;
            self.send = if self.send_left == 0 {
                self.send_target
            } else {
                self.send + self.send_step
            };
        }
        if self.send != 1.0 {
            x *= self.send;
        }
        let hp = if self.hp_on {
            let h1 = self.hp_a * (self.hp_y.0 + x - self.hp_x.0);
            self.hp_x.0 = x;
            self.hp_y.0 = h1;
            let hp = self.hp_a * (self.hp_y.1 + h1 - self.hp_x.1);
            self.hp_x.1 = h1;
            self.hp_y.1 = hp;
            hp
        } else {
            x
        };
        let d = self.pre[self.pre_pos] as f64;
        self.pre[self.pre_pos] = hp as f32;
        self.pre_pos = if self.pre_pos + 1 == self.pre.len() {
            0
        } else {
            self.pre_pos + 1
        };
        let mut l = 0.0;
        let mut r = 0.0;
        for i in 0..8 {
            let y = self.buf[i * self.stride + self.pos[i]] as f64;
            self.lp[i] += self.damp_a * (y - self.lp[i]);
            self.t[i] = self.lp[i];
            if i % 2 == 0 {
                l += self.lp[i];
            } else {
                r += self.lp[i];
            }
        }
        // 8-point fast Walsh-Hadamard transform, scaled to be orthonormal
        let mut h = 1;
        while h < 8 {
            let mut i = 0;
            while i < 8 {
                for j in i..(i + h) {
                    let a = self.t[j];
                    let b = self.t[j + h];
                    self.t[j] = a + b;
                    self.t[j + h] = a - b;
                }
                i += 2 * h;
            }
            h *= 2;
        }
        let norm = 1.0 / libm::sqrt(8.0);
        let inject = 0.35 * d;
        for i in 0..8 {
            let v = self.gain[i] * self.t[i] * norm + if i % 2 == 0 { inject } else { -inject };
            self.buf[i * self.stride + self.pos[i]] = v as f32;
            self.pos[i] = if self.pos[i] + 1 == self.len[i] {
                0
            } else {
                self.pos[i] + 1
            };
        }
        let mut ol = (0.5 * l * self.out_gain) as f32;
        let mut or = (0.5 * r * self.out_gain) as f32;
        self.side_hp.process(&mut ol, &mut or);
        (ol as f64, or as f64)
    }
}
