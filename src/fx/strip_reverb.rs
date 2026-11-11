//! A channel's reverb: `Audio/DeckReverb.swift`.
//!
//! Swift puts Apple's AUReverb2 between a deck's EQ and its fader (and the cue send), so the
//! headphones hear it too; LOOP record listens at the EQ's output, before it. Three knobs, 0...1:
//!
//! - `wet`: the dry/wet mix. Below 0.005 the unit is bypassed, bit-transparent and free.
//! - `size`: the decay of the lows, `0.25 x 32^size` s (0.25...8 s), with the early reflections
//!   spreading out as it grows.
//! - `tone`: how long the highs ring against the lows, 0 dark, 1 bright (the decay at the
//!   Nyquist is `decay x (0.06 + 0.94 x tone)`).
//!
//! The AU cannot be matched (PORTING.md H: it is Apple's), so the *mapping* is ported exactly
//! ([`decay_seconds`], [`is_on`], [`reverb2_params`]: every value `DeckReverb.apply` hands the
//! unit, held to what the unit reads back in `swift/reverb.json`) and the room itself is a
//! stand-in (PORTING.md open decision 1, taken by the parity run on 2026-11-04 and open to
//! reversal): the stereo feedback-delay network of `musicality::space` (`FdnReverb`, the room
//! the runner's send reverb uses), tuned from those same AU parameters. Its level and decay are
//! compared with Swift's measured AUReverb2 in `djhost/tests/deck_reverb_golden.rs` (a noise
//! burst over six settings of size, wet and tone; shorter rooms, three kinds of material, the
//! low tones), each within a stated tolerance in dB, not bit for bit.
//!
//! The mapping from the AU's two decay times to the FDN: the lows decay by 60 dB in
//! `decay_at_0hz` seconds (the FDN's per-line gain), and the one-pole lowpass in the loop is
//! chosen so that the highs at the Nyquist lose the extra per-pass gain that makes them decay in
//! `decay_at_nyquist` seconds instead, never below [`MIN_HF_GAIN`] a pass: below that the loop's
//! lowpass would swallow the room (and, jumped to live, freeze its DC). `min_delay` /
//! `max_delay` (the AU's early-reflection spread) have no counterpart: the FDN's line lengths
//! are fixed.
//!
//! What was measured of the unit (`swift/reverb.json`) and is matched: the lows are returned in
//! full (an 80 Hz burst comes back within 2 dB of a 1 kHz one, so the room has no input
//! highpass here, unlike the runner's send reverb), the input is the mono sum (a burst with the
//! right channel inverted returns nothing), wet is a linear crossfade. What is not: below `tone`
//! 0.25 the unit is not monotonic (5 dB under its own 0.5 at 0.1, 15 dB over it at 0) while the
//! stand-in only gets darker (14 dB under the unit at 0, 7 dB over it at 0.1), and over three
//! kinds of material the level is within 3 dB, not the 0.3 dB a noise burst alone allowed
//! (noise +3.1 dB, 1 kHz -0.3 dB, 80 Hz -2.8 dB at a 2 s room); see `deck_reverb_golden.rs`.
//!
//! [`StripReverb::process`] allocates nothing (it runs on the render thread); `new` allocates
//! the room's delay lines. A wet change is ramped over [`RAMP_FRAMES`] (a step in the mix would
//! click on a held note), and a guard of 1e-20 keeps the tail out of the subnormal range.

use libm::{log, log10, pow};

use crate::musicality::space::{FdnReverb, SpaceSpec};

/// The wet level at which the unit switches on (`DeckReverb.isOn`).
pub const WET_ON: f64 = 0.005;

/// The wet mix moves to a new setting over this many frames (5 ms at 48 kHz).
pub const RAMP_FRAMES: usize = 256;

/// The least gain per pass the loop's lowpass may give the Nyquist (-20 dB): the AU's decay at
/// the Nyquist can be a twentieth of the lows', which per pass of the FDN is a corner near
/// 15 Hz and a silent room.
pub const MIN_HF_GAIN: f64 = 0.1;

/// Added to the room's input, alternating in sign, so that a decaying tail never reaches the
/// subnormal floats (which cost a CPU dearly on some hosts): 1e-20 is 400 dB down.
const DENORMAL_GUARD: f64 = 1e-20;

/// `DeckReverb.decaySeconds`: the decay of the lows, seconds.
pub fn decay_seconds(size: f64) -> f64 {
    0.25 * pow(32.0, size.clamp(0.0, 1.0))
}

/// `DeckReverb.isOn`: is it doing anything?
pub fn is_on(wet: f64) -> bool {
    wet >= WET_ON
}

/// What `DeckReverb.apply` gives AUReverb2: its parameter values before the unit rounds them to
/// `Float`. `dry_wet_mix` is a percentage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reverb2Params {
    pub dry_wet_mix: f64,
    pub gain: f64,
    pub min_delay_time: f64,
    pub max_delay_time: f64,
    pub decay_time_at_0hz: f64,
    pub decay_time_at_nyquist: f64,
}

/// `DeckReverb.apply`'s parameter mapping.
pub fn reverb2_params(wet: f64, size: f64, tone: f64) -> Reverb2Params {
    let decay = decay_seconds(size);
    let s = size.clamp(0.0, 1.0);
    Reverb2Params {
        dry_wet_mix: wet.clamp(0.0, 1.0) * 100.0,
        gain: 0.0,
        min_delay_time: 0.004 + 0.012 * s,
        max_delay_time: 0.03 + 0.12 * s,
        decay_time_at_0hz: decay,
        decay_time_at_nyquist: decay * (0.06 + 0.94 * tone.clamp(0.0, 1.0)),
    }
}

/// A knob of the unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReverbKnob {
    Wet,
    Size,
    Tone,
}

/// The return's level as a function of the decay of the lows, `RETURN_BASE x T^RETURN_EXPONENT`
/// (a gain on `FdnReverb`'s own `wet`). The FDN normalises its output by `sqrt(1 - g^2)`, which
/// falls 3 dB per doubling of the decay, so a long room is no louder than a short one; AUReverb2
/// does not. Fitted to `swift/reverb.json` (50 ms bursts at wet 1, the tail's mean level over
/// 0.1 to 0.75 s): the exponent to the way the unit's level climbs with the decay (size 0.3, 0.6,
/// 1: decays 0.71, 2, 8 s) over a noise burst, the base to centre the error over three kinds of
/// material, noise, 1 kHz and 80 Hz, which the stand-in returns with a different spectral
/// balance than the unit (noise 3 dB hot, 1 kHz level, 80 Hz 3 dB cold at a 2 s decay).
pub const RETURN_BASE: f64 = 0.093;
pub const RETURN_EXPONENT: f64 = 0.287;

/// One channel's reverb, stereo.
pub struct StripReverb {
    sample_rate: f64,
    wet: f64,
    size: f64,
    tone: f64,
    room: FdnReverb,
    /// Is the room running? Off, a block is left as it is and the room holds nothing.
    on: bool,
    /// The mix as it is now, on its way to `wet` ([`RAMP_FRAMES`]); 0 while off.
    mix: f64,
    guard: f64,
}

impl StripReverb {
    /// Off (`wet` 0), `size` and `tone` at 0.5: a new `DeckReverb`.
    pub fn new(sample_rate: f64) -> StripReverb {
        StripReverb {
            sample_rate,
            wet: 0.0,
            size: 0.5,
            tone: 0.5,
            room: FdnReverb::with_input_highpass(sample_rate, false),
            on: false,
            mix: 0.0,
            guard: DENORMAL_GUARD,
        }
    }

    pub fn wet(&self) -> f64 {
        self.wet
    }

    pub fn size(&self) -> f64 {
        self.size
    }

    pub fn tone(&self) -> f64 {
        self.tone
    }

    /// `DeckReverb.isOn`.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The decay of the lows, seconds (`DeckReverb.decaySeconds`).
    pub fn decay_seconds(&self) -> f64 {
        decay_seconds(self.size)
    }

    /// Move a knob (clamped to 0...1). Switching off clears the room: `node.bypass` stops the AU,
    /// and what it held does not come back with the next switch-on.
    pub fn set(&mut self, knob: ReverbKnob, v: f64) {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        match knob {
            ReverbKnob::Wet => self.wet = v,
            ReverbKnob::Size => self.size = v,
            ReverbKnob::Tone => self.tone = v,
        }
        let on = is_on(self.wet);
        if self.on && !on {
            self.room.reset();
            self.mix = 0.0;
        }
        self.on = on;
        if on {
            self.tune();
        }
    }

    /// The room for the knobs: lows decay in `decay_time_at_0hz`, and the loop's lowpass is the
    /// one whose gain at the Nyquist makes the highs decay in `decay_time_at_nyquist`.
    fn tune(&mut self) {
        let p = reverb2_params(self.wet, self.size, self.tone);
        let sr = self.sample_rate;
        // the mean line length, seconds: what one pass of the loop is
        let mean_line = FdnReverb::LINE_MS.iter().sum::<f64>() / FdnReverb::LINE_MS.len() as f64 / 1000.0;
        // extra loss per pass at the Nyquist, as a gain: 10^(-3 L (1/T_hf - 1/T_lf))
        let extra = -3.0 * mean_line * (1.0 / p.decay_time_at_nyquist - 1.0 / p.decay_time_at_0hz);
        let g_nyq = pow(10.0, extra).clamp(MIN_HF_GAIN, 1.0);
        // a one-pole lowpass `y += a (x - y)` has |H(Nyquist)| = a / (2 - a)
        let a = 2.0 * g_nyq / (1.0 + g_nyq);
        let damping = if a >= 0.999_999 {
            sr
        } else {
            -sr / (2.0 * core::f64::consts::PI) * log(1.0 - a)
        };
        let spec = SpaceSpec {
            seconds: p.decay_time_at_0hz,
            wet: RETURN_BASE * pow(p.decay_time_at_0hz, RETURN_EXPONENT),
            damping,
            shimmer: 0.0,
        };
        self.room.tune(Some(&spec));
    }

    /// In place, both channels the same length. Real-time safe. Off, the block is untouched.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.on {
            return;
        }
        let (target, step) = (self.wet, 1.0 / RAMP_FRAMES as f64);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.mix != target {
                self.mix = if (target - self.mix).abs() <= step {
                    target
                } else {
                    self.mix + step.copysign(target - self.mix)
                };
            }
            self.guard = -self.guard;
            let x = (*l as f64 + *r as f64) * 0.5 + self.guard;
            let (rl, rr) = self.room.process(x);
            let w = self.mix;
            *l = ((1.0 - w) * *l as f64 + w * rl) as f32;
            *r = ((1.0 - w) * *r as f64 + w * rr) as f32;
        }
    }

    /// The room's return level in dB for a unit send, as `tune` left it (tests).
    pub fn return_db(&self) -> f64 {
        20.0 * log10(self.room.out_gain().max(1e-12))
    }
}
