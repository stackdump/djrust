//! Port of `Turntable.swift`: a turntable reads a looped buffer at a variable, signed rate with
//! linear interpolation, updated per sample (the scrub-worklet of the JS lab).
//!
//! ```text
//! play   rate eases toward (playing ? (1 + pitch)(1 + nudge) : 0): motor spin-up / brake
//! scrub  rate = (target - pos) / chaseTau: the platter follows the hand
//! ```
//!
//! The model is single-owner: commands are queued with [`Turntable::send`] and taken at the
//! start of [`Turntable::render`], as the audio thread takes them from Swift's SPSC ring. The
//! rings themselves (`SPSCRing`, the retire ring and the atomic position / rate / length
//! readout) are the host's: it owns a `Turntable` on its audio thread and publishes the
//! readout fields it reads after each render. Buffers the render let go of come back through
//! [`Turntable::drain_retired`] to be freed off the audio thread.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Decoded audio (`SampleBuffer`): `right` is None for mono (the same samples as `left`).
#[derive(Clone, Debug, PartialEq)]
pub struct SampleBuffer {
    pub left: Vec<f32>,
    pub right: Option<Vec<f32>>,
    pub sample_rate: f64,
}

impl SampleBuffer {
    /// `init(left:right:sampleRate:)`. Panics on empty audio or mismatched channels, as the
    /// Swift preconditions do.
    pub fn new(left: Vec<f32>, right: Option<Vec<f32>>, sample_rate: f64) -> SampleBuffer {
        assert!(!left.is_empty(), "empty audio");
        if let Some(r) = &right {
            assert_eq!(r.len(), left.len(), "channel lengths differ");
        }
        SampleBuffer {
            left,
            right,
            sample_rate,
        }
    }

    pub fn length(&self) -> usize {
        self.left.len()
    }

    /// The right channel (the left again for mono).
    pub fn right(&self) -> &[f32] {
        self.right.as_deref().unwrap_or(&self.left)
    }

    pub fn duration(&self) -> f64 {
        self.length() as f64 / self.sample_rate
    }
}

/// Messages from the main thread to the audio thread (`TurntableCommand`).
#[derive(Clone, Debug)]
pub enum TurntableCommand {
    Load(Arc<SampleBuffer>),
    /// Replace the audio with another take of the same loop at another length, in phase: the
    /// playhead and the loop region scale with the length, and a playing turntable runs at its
    /// new speed at once (no motor ease). `pitch` is its tempo fader for the new audio.
    Swap(Arc<SampleBuffer>, f64),
    Transport(bool),
    /// Play at full speed at once, with no motor spin-up: a clip launched in time.
    Start,
    /// Hand on the platter: switch to scrub mode at the current position.
    Freeze,
    /// Move the scrub target by this many samples (negative = backwards).
    Scrub(f64),
    /// Hand off: back to motor-driven play.
    Release,
    /// Run at (1 + ratio)x while playing. Momentary (jog ring), on top of pitch.
    Nudge(f64),
    /// Tempo fader: run at (1 + ratio)x while playing.
    Pitch(f64),
    /// Jump the playhead (and scrub target) to this sample.
    Seek(f64),
    /// While playing, wrap the playhead from `end` back to `start` (unwrapped samples).
    Loop {
        start: f64,
        end: f64,
    },
    LoopOff,
    /// Seconds the platter takes to catch up with the hand.
    Chase(f64),
    /// Output gain (linear): the deck's auto-gain. Glides there over about 3 ms unless `now`.
    Gain(f32, bool),
}

#[derive(Debug)]
pub struct Turntable {
    pub sample_rate: f64,
    commands: VecDeque<TurntableCommand>,
    retired: Vec<Arc<SampleBuffer>>,
    buffer: Option<Arc<SampleBuffer>>,
    pos: f64,
    rate: f64,
    target: f64,
    scrubbing: bool,
    motor_on: bool,
    nudge: f64,
    pitch: f64,
    loop_start: f64,
    loop_end: f64,
    looping: bool,
    chase_tau: f64,
    rate_smooth: f64,
    motor_smooth: f64,
    max_rate: f64,
    gain: f32,
    gain_target: f32,
    gain_smooth: f32,
    // the readout the host publishes after a render (Swift: three atomics)
    position: f64,
    current_rate: f64,
    length: usize,
}

impl Turntable {
    pub fn new(sample_rate: f64) -> Turntable {
        Turntable {
            sample_rate,
            // preallocated as Swift's rings are (1024 commands, 64 retired buffers), so a host
            // that drains them every block never allocates on its audio thread
            commands: VecDeque::with_capacity(1024),
            retired: Vec::with_capacity(64),
            buffer: None,
            pos: 0.0,
            rate: 0.0,
            target: 0.0,
            scrubbing: false,
            motor_on: false,
            nudge: 0.0,
            pitch: 0.0,
            loop_start: 0.0,
            loop_end: 0.0,
            looping: false,
            chase_tau: 0.035 * sample_rate,
            rate_smooth: 1.0 - libm::exp(-1.0 / (0.004 * sample_rate)),
            motor_smooth: 1.0 - libm::exp(-1.0 / (0.045 * sample_rate)),
            max_rate: 12.0,
            gain: 1.0,
            gain_target: 1.0,
            gain_smooth: (1.0 - libm::exp(-1.0 / (0.003 * sample_rate))) as f32,
            position: 0.0,
            current_rate: 0.0,
            length: 0,
        }
    }

    // MARK: main-thread side

    pub fn send(&mut self, c: TurntableCommand) {
        self.commands.push_back(c);
    }

    /// Hand `buffer` over. It must match `sample_rate` (resample before loading).
    pub fn load(&mut self, buffer: Arc<SampleBuffer>) {
        assert_eq!(
            buffer.sample_rate, self.sample_rate,
            "resample before loading"
        );
        self.send(TurntableCommand::Load(buffer));
    }

    /// Hand `buffer` over in place of the loaded one, in phase (`TurntableCommand::Swap`).
    pub fn swap(&mut self, buffer: Arc<SampleBuffer>, pitch: f64) {
        assert_eq!(
            buffer.sample_rate, self.sample_rate,
            "resample before loading"
        );
        self.send(TurntableCommand::Swap(buffer, pitch));
    }

    /// Free buffers the render has swapped out: they are returned to be dropped by the caller
    /// (off the audio thread).
    pub fn drain_retired(&mut self) -> Vec<Arc<SampleBuffer>> {
        core::mem::take(&mut self.retired)
    }

    /// One buffer the render let go of (keeps the retire list's capacity, unlike
    /// [`drain_retired`](Self::drain_retired)), for a host draining on its audio thread.
    pub fn pop_retired(&mut self) -> Option<Arc<SampleBuffer>> {
        self.retired.pop()
    }

    /// Unwrapped playhead in samples (can go negative or past the end), as of the last render.
    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn current_rate(&self) -> f64 {
        self.current_rate
    }

    /// Loaded length in samples, 0 when empty.
    pub fn length(&self) -> usize {
        self.length
    }

    // MARK: audio-thread side

    /// Fill `frames` samples of `left` (and `right` if given).
    pub fn render(&mut self, frames: usize, left: &mut [f32], mut right: Option<&mut [f32]>) {
        self.take_commands();
        let Some(buffer) = self.buffer.clone() else {
            left[..frames].fill(0.0);
            if let Some(r) = right.as_deref_mut() {
                r[..frames].fill(0.0);
            }
            return;
        };
        let b = &*buffer;
        let len = b.length() as f64;
        let bl = &b.left;
        let br = b.right();
        for i in 0..frames {
            if self.scrubbing {
                let want = ((self.target - self.pos) / self.chase_tau)
                    .max(-self.max_rate)
                    .min(self.max_rate);
                self.rate += (want - self.rate) * self.rate_smooth;
            } else {
                let want = if self.motor_on {
                    (1.0 + self.pitch) * (1.0 + self.nudge)
                } else {
                    0.0
                };
                self.rate += (want - self.rate) * self.motor_smooth;
            }
            self.pos += self.rate;
            if self.looping && !self.scrubbing {
                let l = self.loop_end - self.loop_start;
                if self.pos >= self.loop_end {
                    self.pos -= l;
                } else if self.pos < self.loop_start && self.rate < 0.0 {
                    self.pos += l;
                }
            }

            let mut p = libm::fmod(self.pos, len);
            if p < 0.0 {
                p += len;
            }
            let i0 = (p as usize).min(b.length() - 1);
            let i1 = if i0 + 1 == b.length() { 0 } else { i0 + 1 };
            let f = (p - i0 as f64) as f32;
            left[i] = bl[i0] + (bl[i1] - bl[i0]) * f;
            if let Some(r) = right.as_deref_mut() {
                r[i] = br[i0] + (br[i1] - br[i0]) * f;
            }
        }
        // the auto-gain: nothing at unity, so a deck without one renders the audio's own samples
        if self.gain != 1.0 || self.gain_target != 1.0 {
            for i in 0..frames {
                if self.gain != self.gain_target {
                    self.gain += (self.gain_target - self.gain) * self.gain_smooth;
                    if (self.gain - self.gain_target).abs() < 1e-6 {
                        self.gain = self.gain_target;
                    }
                }
                left[i] *= self.gain;
                if let Some(r) = right.as_deref_mut() {
                    r[i] *= self.gain;
                }
            }
        }
        self.position = self.pos;
        self.current_rate = self.rate;
        // with the playhead, not when a swap is applied mid-render
        self.length = self.buffer.as_ref().map(|b| b.length()).unwrap_or(0);
    }

    /// Apply the commands waiting now (`render` does it first anyway), so a caller can see this
    /// block's hand / transport state before it renders.
    pub fn take_commands(&mut self) {
        while let Some(c) = self.commands.pop_front() {
            self.apply(c);
        }
    }

    /// Is a hand on the platter (scrub mode), as of the last command applied?
    pub fn hand_on(&self) -> bool {
        self.scrubbing
    }

    /// Is the motor on (transport playing), as of the last command applied?
    pub fn motor_running(&self) -> bool {
        self.motor_on
    }

    /// Move the playhead on by `frames` output frames at the current rate, wrapped inside the
    /// loop region (a loop voice catching up after the loops' vinyl brake, as an unbraked
    /// voice would have played on). Nothing else changes.
    pub fn skip(&mut self, frames: f64) {
        if self.buffer.is_none() || self.scrubbing || frames <= 0.0 {
            return;
        }
        self.pos += self.rate * frames;
        if self.looping {
            let len = self.loop_end - self.loop_start;
            if len > 0.0 && self.pos >= self.loop_end {
                self.pos = self.loop_start + libm::fmod(self.pos - self.loop_start, len);
            }
        }
        self.position = self.pos;
    }

    fn apply(&mut self, c: TurntableCommand) {
        use TurntableCommand::*;
        match c {
            Load(b) => {
                if let Some(old) = self.buffer.take() {
                    self.retired.push(old);
                }
                self.length = b.length();
                self.buffer = Some(b);
                self.pos = 0.0;
                self.target = 0.0;
                self.looping = false; // a loop region belongs to the audio it was set on
            }
            Swap(b, new_pitch) => {
                self.pitch = new_pitch;
                let new_length = b.length() as f64;
                if let Some(old) = self.buffer.take() {
                    let old_length = old.length() as f64;
                    let scale = new_length / old_length;
                    let mut q = libm::fmod(self.pos, old_length);
                    if q < 0.0 {
                        q += old_length;
                    }
                    self.pos = q * scale;
                    self.target = self.pos;
                    if self.looping {
                        self.loop_start *= scale;
                        self.loop_end = (self.loop_start + 1.0).max(self.loop_end * scale);
                    }
                    self.retired.push(old);
                } else {
                    self.pos = 0.0;
                    self.target = 0.0;
                }
                self.buffer = Some(b); // its length is published with the playhead
                if self.motor_on && !self.scrubbing {
                    self.rate = (1.0 + self.pitch) * (1.0 + self.nudge);
                }
            }
            Transport(on) => self.motor_on = on,
            Start => {
                self.motor_on = true;
                self.scrubbing = false;
                self.rate = (1.0 + self.pitch) * (1.0 + self.nudge);
            }
            Freeze => {
                self.scrubbing = true;
                self.target = self.pos;
            }
            Scrub(s) => self.target += s,
            Release => self.scrubbing = false,
            Nudge(r) => self.nudge = r,
            Pitch(r) => self.pitch = r,
            Seek(s) => {
                self.pos = s;
                self.target = s;
            }
            Loop { start, end } => {
                self.loop_start = start;
                self.loop_end = end.max(start + 1.0);
                self.looping = true;
            }
            LoopOff => self.looping = false,
            Chase(seconds) => self.chase_tau = seconds.max(0.002) * self.sample_rate,
            Gain(g, now) => {
                self.gain_target = g;
                if now {
                    self.gain = g;
                }
            }
        }
    }
}
