//! Port of `Musicality/Echo.swift` (`.echo`): a tempo-synced ping-pong echo on the lead and
//! arp where the style has one.

use alloc::vec;
use alloc::vec::Vec;

use crate::fx::unit::FxParam;

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct EchoSpec {
    /// Delay time in sixteenths (3 = a dotted eighth), followed as the tempo moves.
    pub steps: f64,
    /// How much of each repeat comes back.
    pub feedback: f64,
    /// Return level.
    pub wet: f64,
    /// Lowpass inside the loop, Hz.
    pub damping: f64,
}

impl EchoSpec {
    /// The repeats go on into the room (`.space`) this much.
    pub const ROOM_SEND: f64 = 0.3;

    /// `forGenre`: the echo for a genre, or None where the style is dry.
    pub fn for_genre(name: &str) -> Option<EchoSpec> {
        let e = |steps, feedback, wet, damping| EchoSpec {
            steps,
            feedback,
            wet,
            damping,
        };
        match crate::gen::tables::BeatsTables::shared().kin(name) {
            "trance" => Some(e(3.0, 0.42, 0.32, 5000.0)),
            "synthwave" => Some(e(3.0, 0.38, 0.3, 4000.0)),
            "edm" => Some(e(3.0, 0.35, 0.26, 6000.0)),
            "dubstep" | "dnb" | "trap" => Some(e(3.0, 0.35, 0.24, 4500.0)),
            "ambient" => Some(e(6.0, 0.5, 0.3, 3500.0)),
            "lofi" => Some(e(2.0, 0.3, 0.22, 2500.0)),
            _ => None,
        }
    }

    /// `send(kind:group:)`: the lead most, the arp less, nothing else.
    pub fn send(kind: &str, group: &str) -> f64 {
        if kind != "tonal" {
            return 0.0;
        }
        match group {
            "melody" => 0.4,
            "arp" => 0.22,
            _ => 0.0,
        }
    }
}

/// A stereo ping-pong delay: the send goes into the left line, each line feeds the other, so
/// repeats alternate left, right, left.
pub struct PingPong {
    sample_rate: f64,
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    delay: f64,
    target: f64,
    glide: f64,
    feedback: f64,
    wet: f64,
    damp_a: f64,
    lp_l: f64,
    lp_r: f64,
    hp_a: f64,
    hp_x: f64,
    hp_y: f64,
    spec: Option<EchoSpec>,
    send: f64,
    send_target: f64,
    send_step: f64,
    boost: f64,
    boost_target: f64,
    boost_step: f64,
    unit_left: usize,
}

impl PingPong {
    pub const MAX_SECONDS: f64 = 2.5;
    pub const HIGHPASS_HZ: f64 = 250.0;

    pub fn new(sample_rate: f64) -> PingPong {
        let n = (Self::MAX_SECONDS * sample_rate) as usize + 4;
        let rc = 1.0 / (2.0 * core::f64::consts::PI * Self::HIGHPASS_HZ);
        PingPong {
            sample_rate,
            left: vec![0.0; n],
            right: vec![0.0; n],
            pos: 0,
            delay: 0.0,
            target: 0.0,
            glide: 1.0 - libm::exp(-1.0 / (0.05 * sample_rate)),
            feedback: 0.0,
            wet: 0.0,
            damp_a: 1.0,
            lp_l: 0.0,
            lp_r: 0.0,
            hp_a: rc / (rc + 1.0 / sample_rate),
            hp_x: 0.0,
            hp_y: 0.0,
            spec: None,
            send: 1.0,
            send_target: 1.0,
            send_step: 0.0,
            boost: 0.0,
            boost_target: 0.0,
            boost_step: 0.0,
            unit_left: 0,
        }
    }

    pub fn spec(&self) -> Option<EchoSpec> {
        self.spec
    }

    pub fn is_on(&self) -> bool {
        self.wet > 0.0
    }

    /// Set the echo (None: silent).
    pub fn tune(&mut self, spec: Option<EchoSpec>, bpm: f64) {
        self.spec = spec;
        let Some(spec) = spec else {
            self.wet = 0.0;
            return;
        };
        self.feedback = spec.feedback;
        self.wet = spec.wet;
        self.damp_a = 1.0
            - libm::exp(
                -2.0 * core::f64::consts::PI * spec.damping.min(self.sample_rate * 0.45)
                    / self.sample_rate,
            );
        self.set_tempo(bpm);
        self.delay = self.target;
    }

    /// Follow the tempo (the delay glides there over ~50 ms).
    pub fn set_tempo(&mut self, bpm: f64) {
        let Some(spec) = self.spec else { return };
        if bpm <= 0.0 {
            return;
        }
        let seconds = spec.steps * 60.0 / (bpm * 4.0);
        self.target = (self.left.len() as f64 - 3.0).min((seconds * self.sample_rate).max(1.0));
    }

    /// The FX unit's echo send and feedback boost, reached linearly over the next `frames`.
    pub fn set_send(&mut self, g: f64, feedback_boost: f64, frames: usize) {
        self.send_target = g;
        self.boost_target = feedback_boost;
        self.unit_left = frames.max(1);
        self.send_step = (g - self.send) / self.unit_left as f64;
        self.boost_step = (feedback_boost - self.boost) / self.unit_left as f64;
    }

    /// One sample in (the send), one stereo sample out (the repeats).
    pub fn process(&mut self, x: f64) -> (f64, f64) {
        let mut x = x;
        if self.unit_left > 0 {
            self.unit_left -= 1;
            if self.unit_left == 0 {
                self.send = self.send_target;
                self.boost = self.boost_target;
            } else {
                self.send += self.send_step;
                self.boost += self.boost_step;
            }
        }
        if self.send != 1.0 {
            x *= self.send;
        }
        let feedback = if self.boost == 0.0 {
            self.feedback
        } else {
            self.feedback + (FxParam::MAX_FEEDBACK - self.feedback) * self.boost
        };
        self.delay += (self.target - self.delay) * self.glide;
        let hp = self.hp_a * (self.hp_y + x - self.hp_x);
        self.hp_x = x;
        self.hp_y = hp;
        let n = self.left.len();
        let mut rp = self.pos as f64 - self.delay;
        if rp < 0.0 {
            rp += n as f64;
        }
        // a read a hair behind the write head wraps to exactly `count`: fold it back
        if rp >= n as f64 {
            rp -= n as f64;
        }
        let i0 = rp as usize;
        let fr = rp - i0 as f64;
        let i1 = if i0 + 1 == n { 0 } else { i0 + 1 };
        let l = self.left[i0] as f64 + (self.left[i1] as f64 - self.left[i0] as f64) * fr;
        let r = self.right[i0] as f64 + (self.right[i1] as f64 - self.right[i0] as f64) * fr;
        self.lp_l += self.damp_a * (l - self.lp_l);
        self.lp_r += self.damp_a * (r - self.lp_r);
        self.left[self.pos] = (hp + feedback * self.lp_r) as f32;
        self.right[self.pos] = (feedback * self.lp_l) as f32;
        self.pos = if self.pos + 1 == n { 0 } else { self.pos + 1 };
        (self.lp_l * self.wet, self.lp_r * self.wet)
    }
}
