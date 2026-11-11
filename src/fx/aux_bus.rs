//! The aux send/return bus (docs/HUB.md M4; beyond Swift, which has no aux).
//!
//! Every strip (A, B, L, each input) has an **aux send** knob, post-fader. The sends sum on one
//! stereo bus, which goes through a room (the runner's FDN reverb) and a ping-pong echo, side by
//! side, and comes back to the master on the mixer's **FX return** fader. The six effect knobs are
//! beats' own master-FX sliders (lib/ui/build.js `data-fx`: `reverb-size`, `reverb-damp`,
//! `reverb-wet`, `delay-time`, `delay-feedback`, `delay-wet`), on the same ranges and defaults, so
//! the macros that move them in beats (`delay-throw`, `dub-delay`, `cathedral`, see
//! [`super::aux_macro`]) move them here.
//!
//! Where beats' sliders mean the Tone reverb and delay inserted on the master, here they shape the
//! aux effects, and what a strip gives them is its send. All sends start at 0, so a mix that never
//! touches the aux is untouched, bit for bit: the bus is skipped while nothing is sent and its
//! tails have died away.
//!
//! The map from a slider to the DSP (`build.js`'s `toneEngine.set…`):
//!
//! | slider | range (default) | here |
//! |---|---|---|
//! | `reverb-size` | 0…100 (50) | the room's decay, `0.25 × 32^(v/100)` s as the strip reverb's size |
//! | `reverb-damp` | 0…100 (30) | the loop's lowpass, `10000 − v/100 × 9800` Hz |
//! | `reverb-wet` | 0…100 (20) | the return level of the room, `v/100` (1 = about what was sent) |
//! | `delay-time` | 1…100 (25) | the echo's time, `v/100` s per side |
//! | `delay-feedback` | 0…90 (25) | the echo's feedback, `v/100` |
//! | `delay-wet` | 0…100 (15) | the return level of the echo, `v/100` |
//!
//! Pure and allocation-free after `new` (the delay lines are made there for the longest echo), so
//! it runs on the render thread.

use alloc::vec;
use alloc::vec::Vec;
use libm::{exp, floor, pow};

use super::strip_reverb::{decay_seconds, RETURN_BASE, RETURN_EXPONENT};
use crate::musicality::space::{FdnReverb, SpaceSpec};

/// The FX return's fader at the start. The sends are 0, so it matters once one is raised.
pub const RETURN_LEVEL: f64 = 0.8;

/// A send knob's value for "all the way up". The knob is 0…1 and is a plain gain.
pub const SEND_MAX: f64 = 1.0;

/// The longest echo (a slider at 100 is a second per side).
pub const MAX_DELAY_SECONDS: f64 = 1.0;

/// How long a setting takes to be reached, so a moved knob or a macro's jump does not click
/// (5 ms: the gains) and a changed echo time glides like tape (80 ms).
const GAIN_SMOOTH_SECONDS: f64 = 0.005;
const TIME_SMOOTH_SECONDS: f64 = 0.080;

/// How long the bus keeps running after the last send went quiet: the longest tail it can make
/// (a 8 s room), plus a second of echo.
const TAIL_SECONDS: f64 = 12.0;

/// One of the six aux effect knobs (beats' master-FX slider ids).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AuxFxKey {
    ReverbSize,
    ReverbDamp,
    ReverbWet,
    DelayTime,
    DelayFeedback,
    DelayWet,
}

impl AuxFxKey {
    pub const ALL: [AuxFxKey; 6] = [
        AuxFxKey::ReverbSize,
        AuxFxKey::ReverbDamp,
        AuxFxKey::ReverbWet,
        AuxFxKey::DelayTime,
        AuxFxKey::DelayFeedback,
        AuxFxKey::DelayWet,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Option<AuxFxKey> {
        Self::ALL.get(i).copied()
    }

    /// beats' `data-fx`.
    pub fn id(self) -> &'static str {
        match self {
            AuxFxKey::ReverbSize => "reverb-size",
            AuxFxKey::ReverbDamp => "reverb-damp",
            AuxFxKey::ReverbWet => "reverb-wet",
            AuxFxKey::DelayTime => "delay-time",
            AuxFxKey::DelayFeedback => "delay-feedback",
            AuxFxKey::DelayWet => "delay-wet",
        }
    }

    pub fn from_id(id: &str) -> Option<AuxFxKey> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            AuxFxKey::ReverbSize => "Room size",
            AuxFxKey::ReverbDamp => "Room damp",
            AuxFxKey::ReverbWet => "Room return",
            AuxFxKey::DelayTime => "Echo time",
            AuxFxKey::DelayFeedback => "Echo feedback",
            AuxFxKey::DelayWet => "Echo return",
        }
    }

    /// The slider's `min` (build.js).
    pub fn min(self) -> i32 {
        if self == AuxFxKey::DelayTime {
            1
        } else {
            0
        }
    }

    /// The slider's `max` (build.js).
    pub fn max(self) -> i32 {
        if self == AuxFxKey::DelayFeedback {
            90
        } else {
            100
        }
    }

    /// `data-default`.
    pub fn default_value(self) -> i32 {
        match self {
            AuxFxKey::ReverbSize => 50,
            AuxFxKey::ReverbDamp => 30,
            AuxFxKey::ReverbWet => 20,
            AuxFxKey::DelayTime => 25,
            AuxFxKey::DelayFeedback => 25,
            AuxFxKey::DelayWet => 15,
        }
    }

    /// A slider position, 0…1 of its travel, in the slider's own units (rounded).
    pub fn slider_value(self, position: f64) -> i32 {
        let (min, max) = (self.min() as f64, self.max() as f64);
        let p = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.0 };
        libm::round(min + p * (max - min)) as i32
    }

    /// A slider value as a position 0…1 of its travel.
    pub fn position(self, v: f64) -> f64 {
        let (min, max) = (self.min() as f64, self.max() as f64);
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    }

    /// A value clamped to the slider's range (a NaN is the default).
    pub fn sanitize(self, v: f64) -> f64 {
        if !v.is_finite() {
            return self.default_value() as f64;
        }
        v.clamp(self.min() as f64, self.max() as f64)
    }
}

/// The bus's settings: the return fader, its mute and the six sliders (in their own units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuxSettings {
    /// The FX return's fader, 0…1.
    pub level: f64,
    pub mute: bool,
    pub fx: [f64; 6],
}

impl Default for AuxSettings {
    fn default() -> Self {
        let mut fx = [0.0; 6];
        for k in AuxFxKey::ALL {
            fx[k.index()] = k.default_value() as f64;
        }
        AuxSettings { level: RETURN_LEVEL, mute: false, fx }
    }
}

impl AuxSettings {
    pub fn get(&self, k: AuxFxKey) -> f64 {
        self.fx[k.index()]
    }

    pub fn set(&mut self, k: AuxFxKey, v: f64) {
        self.fx[k.index()] = k.sanitize(v);
    }
}

/// One-pole smoothing coefficient for a time constant.
fn smooth_coeff(seconds: f64, sample_rate: f64) -> f64 {
    1.0 - exp(-1.0 / (seconds * sample_rate))
}

/// The aux effects bus.
pub struct AuxBus {
    sample_rate: f64,
    pub settings: AuxSettings,
    room: FdnReverb,
    /// The room as last tuned (size, damp), so a block with the same knobs does not retune.
    tuned: (f64, f64),
    /// The echo's lines, one per side, `len` long.
    line_l: Vec<f32>,
    line_r: Vec<f32>,
    write: usize,
    /// Smoothed: the room's return, the echo's return, the feedback, the echo time (frames), and
    /// the return fader (with the mute).
    room_gain: f64,
    echo_gain: f64,
    feedback: f64,
    time: f64,
    fader: f64,
    a_gain: f64,
    a_time: f64,
    /// Frames since the bus last heard a send, to put it to sleep when its tails are over.
    quiet_frames: u64,
    tail_frames: u64,
    guard: f64,
}

impl AuxBus {
    pub fn new(sample_rate: f64) -> AuxBus {
        let len = floor(MAX_DELAY_SECONDS * sample_rate) as usize + 2;
        let s = AuxSettings::default();
        let mut bus = AuxBus {
            sample_rate,
            settings: s,
            room: FdnReverb::new(sample_rate),
            tuned: (f64::NAN, f64::NAN),
            line_l: vec![0.0; len],
            line_r: vec![0.0; len],
            write: 0,
            room_gain: 0.0,
            echo_gain: 0.0,
            feedback: 0.0,
            time: 0.0,
            fader: 0.0,
            a_gain: smooth_coeff(GAIN_SMOOTH_SECONDS, sample_rate),
            a_time: smooth_coeff(TIME_SMOOTH_SECONDS, sample_rate),
            quiet_frames: u64::MAX / 2,
            tail_frames: floor(TAIL_SECONDS * sample_rate) as u64,
            guard: 1e-20,
        };
        bus.time = bus.target_time();
        bus.retune();
        bus
    }

    pub fn set(&mut self, k: AuxFxKey, v: f64) {
        self.settings.set(k, v);
    }

    pub fn set_level(&mut self, v: f64) {
        self.settings.level = if v.is_finite() { v.clamp(0.0, 1.0) } else { RETURN_LEVEL };
    }

    pub fn set_mute(&mut self, on: bool) {
        self.settings.mute = on;
    }

    /// The decay of the room in seconds, for the slider as it is.
    pub fn decay_seconds(&self) -> f64 {
        decay_seconds(self.settings.get(AuxFxKey::ReverbSize) / 100.0)
    }

    fn target_time(&self) -> f64 {
        (self.settings.get(AuxFxKey::DelayTime) / 100.0 * self.sample_rate)
            .clamp(1.0, (self.line_l.len() - 2) as f64)
    }

    /// The room for the sliders: its decay, and the damping in Hz as beats' Tone reverb has it.
    fn retune(&mut self) {
        let size = self.settings.get(AuxFxKey::ReverbSize);
        let damp = self.settings.get(AuxFxKey::ReverbDamp);
        if self.tuned == (size, damp) {
            return;
        }
        self.tuned = (size, damp);
        let seconds = decay_seconds(size / 100.0);
        let damping = 10_000.0 - damp / 100.0 * 9_800.0;
        // wet 1 = the return as loud as the send, whatever the room's length (the strip
        // reverb's fit, `RETURN_BASE × T^RETURN_EXPONENT`, times its own `wet`, which is the
        // slider here)
        let spec = SpaceSpec {
            seconds,
            wet: RETURN_BASE * pow(seconds, RETURN_EXPONENT),
            damping,
            shimmer: 0.0,
        };
        self.room.tune(Some(&spec));
    }

    /// Could the bus still be making sound? False once nothing has been sent for a full tail:
    /// the render skips it until the next send.
    pub fn is_awake(&self) -> bool {
        self.quiet_frames <= self.tail_frames
    }

    /// Clear the room and the echo (a panic, or a stop).
    pub fn reset(&mut self) {
        self.room.reset();
        self.line_l.iter_mut().for_each(|x| *x = 0.0);
        self.line_r.iter_mut().for_each(|x| *x = 0.0);
        self.quiet_frames = u64::MAX / 2;
    }

    /// Run the bus over a block: `send_l` / `send_r` are the summed sends in, the **return** is
    /// added to `out_l` / `out_r` (through the return fader). `sent` says whether any strip had a
    /// send up in this block. Real-time safe. Returns the return's peak.
    pub fn process(
        &mut self,
        send_l: &[f32],
        send_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        sent: bool,
    ) -> f32 {
        let n = send_l.len().min(send_r.len()).min(out_l.len()).min(out_r.len());
        if sent {
            self.quiet_frames = 0;
        } else if self.quiet_frames > self.tail_frames {
            return 0.0;
        } else {
            self.quiet_frames += n as u64;
        }
        self.retune();
        let s = &self.settings;
        let room_t = s.get(AuxFxKey::ReverbWet) / 100.0;
        let echo_t = s.get(AuxFxKey::DelayWet) / 100.0;
        let fb_t = s.get(AuxFxKey::DelayFeedback) / 100.0;
        let fader_t = if s.mute { 0.0 } else { s.level };
        let time_t = self.target_time();
        let len = self.line_l.len();
        let mut peak = 0f32;
        for i in 0..n {
            self.room_gain += self.a_gain * (room_t - self.room_gain);
            self.echo_gain += self.a_gain * (echo_t - self.echo_gain);
            self.feedback += self.a_gain * (fb_t - self.feedback);
            self.fader += self.a_gain * (fader_t - self.fader);
            self.time += self.a_time * (time_t - self.time);
            let (xl, xr) = (send_l[i] as f64, send_r[i] as f64);
            // the room hears the mono sum
            self.guard = -self.guard;
            let (rl, rr) = self.room.process(0.5 * (xl + xr) + self.guard);
            // the echo: a delay on each side, the feedback crossing over (ping-pong), the mono
            // sum entering the left line
            let back = self.time;
            let at = self.write as f64 - back;
            let at = if at < 0.0 { at + len as f64 } else { at };
            let i0 = floor(at) as usize % len;
            let i1 = (i0 + 1) % len;
            let fr = at - floor(at);
            let dl = self.line_l[i0] as f64 * (1.0 - fr) + self.line_l[i1] as f64 * fr;
            let dr = self.line_r[i0] as f64 * (1.0 - fr) + self.line_r[i1] as f64 * fr;
            self.line_l[self.write] = (0.5 * (xl + xr) + self.feedback * dr + self.guard) as f32;
            self.line_r[self.write] = (self.feedback * dl + self.guard) as f32;
            self.write = if self.write + 1 == len { 0 } else { self.write + 1 };
            let g = self.fader;
            let l = g * (self.room_gain * rl + self.echo_gain * dl);
            let r = g * (self.room_gain * rr + self.echo_gain * dr);
            out_l[i] += l as f32;
            out_r[i] += r as f32;
            peak = peak.max((l as f32).abs()).max((r as f32).abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn run(bus: &mut AuxBus, input: &[f32], blocks: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut ol, mut or) = (Vec::new(), Vec::new());
        let n = input.len();
        for b in 0..blocks {
            let mut l = vec![0f32; n];
            let mut r = vec![0f32; n];
            let zeros = vec![0f32; n];
            let send: &[f32] = if b == 0 { input } else { &zeros };
            bus.process(send, send, &mut l, &mut r, b == 0);
            ol.extend(l);
            or.extend(r);
        }
        (ol, or)
    }

    fn click(n: usize) -> Vec<f32> {
        let mut v = vec![0f32; n];
        v[0] = 1.0;
        v
    }

    #[test]
    fn the_sliders_have_beats_ranges_and_defaults() {
        let table: Vec<(&str, i32, i32, i32)> = AuxFxKey::ALL
            .iter()
            .map(|k| (k.id(), k.min(), k.max(), k.default_value()))
            .collect();
        assert_eq!(
            table,
            vec![
                ("reverb-size", 0, 100, 50),
                ("reverb-damp", 0, 100, 30),
                ("reverb-wet", 0, 100, 20),
                ("delay-time", 1, 100, 25),
                ("delay-feedback", 0, 90, 25),
                ("delay-wet", 0, 100, 15),
            ]
        );
        for k in AuxFxKey::ALL {
            assert_eq!(AuxFxKey::from_id(k.id()), Some(k));
            assert_eq!(AuxFxKey::from_index(k.index()), Some(k));
        }
        assert_eq!(AuxFxKey::DelayFeedback.slider_value(1.0), 90);
        assert_eq!(AuxFxKey::DelayTime.slider_value(0.0), 1);
        assert_eq!(AuxFxKey::ReverbWet.sanitize(f64::NAN), 20.0);
        assert_eq!(AuxFxKey::DelayFeedback.sanitize(500.0), 90.0);
    }

    #[test]
    fn nothing_sent_makes_no_sound_and_the_bus_sleeps() {
        let mut bus = AuxBus::new(SR);
        assert!(!bus.is_awake());
        let z = vec![0f32; 256];
        let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
        assert_eq!(bus.process(&z, &z, &mut l, &mut r, false), 0.0);
        assert!(l.iter().chain(&r).all(|x| *x == 0.0));
    }

    #[test]
    fn a_click_comes_back_as_an_echo_at_the_delay_time_and_a_tail() {
        let mut bus = AuxBus::new(SR);
        bus.set(AuxFxKey::DelayTime, 25.0);
        bus.set(AuxFxKey::DelayWet, 100.0);
        bus.set(AuxFxKey::ReverbWet, 0.0);
        bus.set_level(1.0);
        // let the gains arrive
        let _ = run(&mut bus, &vec![0f32; 4800], 1);
        let (l, r) = run(&mut bus, &click(4800), 20);
        let at = (0.25 * SR) as usize;
        let near = |v: &[f32], c: usize| v[c - 40..c + 40].iter().fold(0f32, |p, x| p.max(x.abs()));
        // the first echo is on the left, the second (feedback 0.25) on the right a delay later
        assert!(near(&l, at) > 0.3, "left echo {}", near(&l, at));
        assert!(near(&r, 2 * at) > 0.05, "right echo {}", near(&r, 2 * at));
        assert!(near(&r, at) < 0.01, "right at the first echo");
        assert!(bus.is_awake());
    }

    #[test]
    fn a_room_rings_and_the_return_fader_scales_it() {
        let mut bus = AuxBus::new(SR);
        bus.set(AuxFxKey::DelayWet, 0.0);
        bus.set(AuxFxKey::ReverbWet, 100.0);
        bus.set_level(1.0);
        let _ = run(&mut bus, &vec![0f32; 4800], 1);
        let (l1, _) = run(&mut bus, &click(4800), 10);
        let rms = |v: &[f32]| (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let loud = rms(&l1);
        assert!(loud > 1e-4, "a room returned nothing: {loud}");
        let mut quiet = AuxBus::new(SR);
        quiet.set(AuxFxKey::DelayWet, 0.0);
        quiet.set(AuxFxKey::ReverbWet, 100.0);
        quiet.set_level(0.5);
        let _ = run(&mut quiet, &vec![0f32; 4800], 1);
        let (l2, _) = run(&mut quiet, &click(4800), 10);
        let ratio = rms(&l2) / loud;
        assert!((ratio - 0.5).abs() < 0.02, "fader ratio {ratio}");
        quiet.set_mute(true);
        let (l3, _) = run(&mut quiet, &click(4800), 5);
        assert!(l3.iter().skip(2400).all(|x| x.abs() < 1e-4), "mute leaves a tail");
    }

    #[test]
    fn a_bigger_room_rings_longer_and_a_darker_one_has_less_top() {
        let tail = |size: f64| {
            let mut b = AuxBus::new(SR);
            b.set(AuxFxKey::DelayWet, 0.0);
            b.set(AuxFxKey::ReverbWet, 100.0);
            b.set(AuxFxKey::ReverbSize, size);
            b.set_level(1.0);
            let _ = run(&mut b, &vec![0f32; 4800], 1);
            let (l, _) = run(&mut b, &click(4800), 40);
            // the last second of 40 blocks of 0.1 s
            let t = &l[l.len() - 48_000..];
            (t.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / t.len() as f64).sqrt()
        };
        assert!(tail(100.0) > 5.0 * tail(20.0));
        assert!((AuxBus::new(SR).decay_seconds() - 0.25 * pow(32.0, 0.5)).abs() < 1e-9);
    }

    #[test]
    fn it_is_deterministic_and_does_not_depend_on_the_block_size() {
        let input: Vec<f32> = (0..9600).map(|i| ((i * 7919 % 2003) as f32 / 1001.5 - 1.0) * 0.3).collect();
        let go = |block: usize| {
            let mut b = AuxBus::new(SR);
            b.set(AuxFxKey::DelayWet, 70.0);
            b.set(AuxFxKey::ReverbWet, 60.0);
            let (mut l, mut r) = (Vec::new(), Vec::new());
            let mut at = 0;
            while at < 24_000 {
                let n = block.min(24_000 - at);
                let mut x = vec![0f32; n];
                for (k, v) in x.iter_mut().enumerate() {
                    *v = *input.get(at + k).unwrap_or(&0.0);
                }
                let (mut ol, mut or) = (vec![0f32; n], vec![0f32; n]);
                b.process(&x, &x, &mut ol, &mut or, at < 9600);
                l.extend(ol);
                r.extend(or);
                at += n;
            }
            (l, r)
        };
        let (a, b) = (go(128), go(1000));
        assert_eq!(a, b);
        assert_eq!(go(128), a);
    }
}
