//! The browser's audition (docs/BROWSER.md): a file played on the headphone cue only, never
//! the master, so a loop can be heard before it goes anywhere. One voice: a new audition
//! replaces the one playing. It loops (a loop is judged going round) until stopped, at the
//! file's own tempo, at the headphone level the cue already has (the CUE MIX knob still blends
//! in the master). Where the output has no cue (no headphone pair or device) nothing is heard:
//! [`Audition::cue_routed`] says so.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::deck::turntable::SampleBuffer;

use super::engine::{Engine, Msg};

/// Main thread → the audition voice.
pub enum AuditionMsg {
    /// Play this from its top (looping), replacing what plays.
    Play(Arc<SampleBuffer>, f32),
    Stop,
    Gain(f32),
}

/// What the voice publishes.
#[derive(Default)]
pub struct AuditionMirror {
    playing: AtomicBool,
    /// The playhead as a fraction of the file.
    position: AtomicU32,
    /// The last block rendered a cue feed.
    cue: AtomicBool,
}

/// The render thread's voice.
pub struct AuditionVoice {
    buffer: Option<Arc<SampleBuffer>>,
    pos: f64,
    rate: f64,
    gain: f32,
    /// Ramp in from silence over this many frames (no click on start).
    fade: f32,
    mirror: Arc<AuditionMirror>,
    sample_rate: f64,
}

/// Frames an audition fades in over.
const FADE_FRAMES: f32 = 256.0;

impl AuditionVoice {
    pub fn new(sample_rate: f64, mirror: Arc<AuditionMirror>) -> AuditionVoice {
        AuditionVoice {
            buffer: None,
            pos: 0.0,
            rate: 1.0,
            gain: 1.0,
            fade: 0.0,
            mirror,
            sample_rate,
        }
    }

    /// Apply a message; returns the buffer it let go of, for the retire ring.
    pub(crate) fn apply(&mut self, m: AuditionMsg) -> Option<Arc<SampleBuffer>> {
        match m {
            AuditionMsg::Play(b, gain) => {
                self.rate = if self.sample_rate > 0.0 { b.sample_rate / self.sample_rate } else { 1.0 };
                self.pos = 0.0;
                self.fade = 0.0;
                self.gain = gain;
                self.mirror.playing.store(true, Ordering::Relaxed);
                self.buffer.replace(b)
            }
            AuditionMsg::Stop => {
                self.mirror.playing.store(false, Ordering::Relaxed);
                self.mirror.position.store(0f32.to_bits(), Ordering::Relaxed);
                self.buffer.take()
            }
            AuditionMsg::Gain(g) => {
                self.gain = g;
                None
            }
        }
    }

    /// Add the audition to the cue feed (`cue` None: there is no cue output this block).
    pub(crate) fn mix(&mut self, n: usize, cue: Option<(&mut [f32], &mut [f32])>) {
        self.mirror.cue.store(cue.is_some(), Ordering::Relaxed);
        let Some(b) = &self.buffer else { return };
        let len = b.left.len();
        if len == 0 {
            return;
        }
        let right = b.right.as_deref().unwrap_or(&b.left);
        match cue {
            Some((cl, cr)) => {
                for i in 0..n {
                    let p = self.pos;
                    let i0 = p as usize % len;
                    let i1 = (i0 + 1) % len;
                    let f = (p - p.floor()) as f32;
                    let g = self.gain * (self.fade / FADE_FRAMES).min(1.0);
                    cl[i] += g * (b.left[i0] + (b.left[i1] - b.left[i0]) * f);
                    cr[i] += g * (right[i0] + (right[i1] - right[i0]) * f);
                    self.fade += 1.0;
                    self.pos = (p + self.rate) % len as f64;
                }
            }
            // keep time with no cue, so it is where it would be when the phones come back
            None => self.pos = (self.pos + self.rate * n as f64) % len as f64,
        }
        self.mirror
            .position
            .store(((self.pos / len as f64) as f32).to_bits(), Ordering::Relaxed);
    }
}

/// The main thread's half: what is auditioning.
pub struct Audition {
    pub mirror: Arc<AuditionMirror>,
    /// What plays (a title and the file it came from).
    pub title: Option<String>,
    pub path: Option<String>,
    pub gain: f32,
}

impl Audition {
    pub fn new(mirror: Arc<AuditionMirror>) -> Audition {
        Audition {
            mirror,
            title: None,
            path: None,
            gain: 1.0,
        }
    }

    pub fn playing(&self) -> bool {
        self.mirror.playing.load(Ordering::Relaxed)
    }

    /// Where it is, 0...1.
    pub fn position(&self) -> f32 {
        f32::from_bits(self.mirror.position.load(Ordering::Relaxed))
    }

    /// Did the last block have a headphone feed to play into?
    pub fn cue_routed(&self) -> bool {
        self.mirror.cue.load(Ordering::Relaxed)
    }
}

impl Engine {
    /// Audition `buffer` on the headphone cue (looping, replacing any audition).
    pub fn audition_play(&mut self, buffer: SampleBuffer, title: &str, path: Option<&str>) -> bool {
        let g = self.audition.gain;
        let ok = self.send_audition(AuditionMsg::Play(Arc::new(buffer), g));
        if ok {
            self.audition.title = Some(title.to_string());
            self.audition.path = path.map(str::to_string);
        }
        ok
    }

    /// Stop the audition.
    pub fn audition_stop(&mut self) -> bool {
        self.audition.title = None;
        self.audition.path = None;
        self.send_audition(AuditionMsg::Stop)
    }

    /// The audition's level, 0...1 (on top of the headphone level).
    pub fn set_audition_gain(&mut self, g: f64) {
        self.audition.gain = g.clamp(0.0, 1.0) as f32;
        let g = self.audition.gain;
        self.send_audition(AuditionMsg::Gain(g));
    }

    fn send_audition(&mut self, m: AuditionMsg) -> bool {
        self.send_msg(Msg::Audition(m))
    }
}
