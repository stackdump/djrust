//! Shared rig for the loop-bank tests (`LoopFxSupport.swift`): a loop bank playing sine clips,
//! rendered block by block, and the measurements the FX tests make on it.

#![allow(dead_code)]

use std::sync::Arc;

use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::fx::loop_fx::LoopFxParams;
use djrust::session::types::LoopFx;
use djrust::host::deck::loops::{LoopBank, LoopMsg};

pub const SR: f64 = 48_000.0;

/// A stereo sine clip that loops seamlessly: whole periods of `hz` at 48 kHz (`fxSineBuffer`).
pub fn sine_buffer(hz: f64, amplitude: f64, seconds: f64) -> Arc<SampleBuffer> {
    let n = (SR * seconds) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| (amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / SR).sin()) as f32)
        .collect();
    Arc::new(SampleBuffer::new(l.clone(), Some(l), SR))
}

/// A loop bank with `clips` on voices 0..., each looping from frame 0 at full level, the FX of
/// each set before it starts (`FxBankRig`). `render` pulls it in 512-frame blocks, the bank's
/// own output (no channel fader).
pub struct FxBankRig {
    pub bank: LoopBank,
    pub frame: i64,
}

impl FxBankRig {
    pub fn new(clips: &[Arc<SampleBuffer>], fx: &[LoopFx]) -> FxBankRig {
        let (mut bank, _) = LoopBank::new(SR);
        for (v, b) in clips.iter().enumerate() {
            let len = b.length() as f64;
            bank.apply(LoopMsg::Voice(v, TurntableCommand::Load(b.clone())));
            bank.apply(LoopMsg::Voice(
                v,
                TurntableCommand::Loop {
                    start: 0.0,
                    end: len,
                },
            ));
            bank.apply(LoopMsg::Voice(v, TurntableCommand::Start));
            bank.apply(LoopMsg::Gain(v, 1.0));
            if v < fx.len() {
                bank.apply(LoopMsg::Fx(v, LoopFxParams::new(&fx[v], SR)));
            }
            bank.apply(LoopMsg::Start {
                v,
                frame: 0,
                fade: 0,
            });
        }
        FxBankRig { bank, frame: 0 }
    }

    pub fn set(&mut self, v: usize, fx: &LoopFx) {
        self.bank.apply(LoopMsg::Fx(v, LoopFxParams::new(fx, SR)));
    }

    /// The next `frames` frames (512 at a time), both channels.
    pub fn render(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut out_l, mut out_r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
        let mut done = 0;
        while done < frames {
            let n = 512.min(frames - done);
            self.bank.render_bank(self.frame, &mut l[..n], &mut r[..n]);
            out_l.extend_from_slice(&l[..n]);
            out_r.extend_from_slice(&r[..n]);
            self.frame += n as i64;
            done += n;
        }
        (out_l, out_r)
    }

    /// Render without keeping it.
    pub fn skip(&mut self, frames: usize) {
        let _ = self.render(frames);
    }
}

pub fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The amplitude of `hz` in `x` (a Goertzel over the whole slice; exact for whole periods).
pub fn tone_amplitude(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / SR;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    let p = s1 * s1 + s2 * s2 - c * s1 * s2;
    2.0 * p.sqrt() / x.len() as f64
}

/// Total harmonic distortion of a tone at `hz`: harmonics 2...10 against the fundamental.
pub fn thd(x: &[f32], hz: f64) -> f64 {
    let f = tone_amplitude(x, hz);
    let h: f64 = (2..=10)
        .map(|k| tone_amplitude(x, hz * k as f64))
        .map(|a| a * a)
        .sum();
    h.sqrt() / f.max(1e-12)
}

/// The largest jump from one sample to the next.
pub fn max_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |p, v| p.max(v.abs()))
}
