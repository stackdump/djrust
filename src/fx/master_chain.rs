//! beats' master insert chain (audio/tone-engine.js `init`, Tone 15.1.22), per deck, after the
//! wave engine and its sampled instruments (`MasterChain.swift`):
//!
//! ```text
//! HP (Tone.Filter 20 Hz, highpass, rolloff -48)  ->  Phaser (1 Hz, 3 octaves, base 350 Hz,
//! 10 stages, Q 10, wet 0)  ->  LP (Tone.Filter 20 kHz, lowpass, -48)  ->  BitCrusher (16 bits,
//! wet 0)  ->  Distortion (0, wet 0)  ->  PitchShift (0, window 0.03 s)
//! ```
//!
//! What each Tone node is, as ported (the Tone sources and the Web Audio spec / Chrome's
//! implementation are the reference):
//!
//! - Tone.Filter rolloff -48 = THREE identical cascaded BiquadFilterNodes (Filter.js:
//!   `[-12, -24, -48, -96].indexOf(rolloff) + 1`; measured in Chrome), not the four "-48 dB/oct"
//!   suggests; Q = 1 (dB for lowpass/highpass). Their frequency is a Signal (an audio-rate
//!   input), so Chrome recomputes the coefficients every sample while it ramps. Biquad:
//!   Chrome's direct form I with the RBJ coefficients of the Web Audio spec, float output.
//! - Phaser: a sine LFO per channel (right at 180 degrees) scaled to base..base*2^octaves Hz
//!   drives ten allpass biquads (Q 10) per channel, per sample; equal-power dry/wet
//!   (Tone.CrossFade = StereoPannerNode on a constant).
//! - BitCrusher: Tone's worklet, `step = 0.5^(bits-1)`, `step * floor(x/step + 0.5)`; `bits` is
//!   a k-rate parameter, read at the start of every 128-frame render quantum; equal-power wet.
//! - Distortion: a WaveShaper. Its curve has **1024** points: Distortion's setter calls
//!   `setMap(fn)` with WaveShaper's default length, not the 4096 it was built with. Linear
//!   interpolation as the spec / Chrome do it; oversample none; equal-power wet.
//! - PitchShift: two delay lines (linear interpolation, as Chrome's DelayNode) swept by
//!   sawtooth LFOs (0 and 180 degrees) between 0 and the window, crossfaded (equal power) by a
//!   triangle LFO at 90 degrees, all at `factor * 1.2 / window` Hz (`intervalToFrequencyRatio`,
//!   with Tone's `interval - 1` for downward shifts). The LFOs are Chrome's oscillators
//!   ([`super::periodic_wave`]). Its feedback path (feedback 0) is dropped.
//! - Every Tone.CrossFade (the wets, the pitch shifter's tap fade) goes through GainToAudio's
//!   1024-point curve, which leaks the wet in at -56 dB at fade 0 ([`MasterChain::crossfade`]).
//!
//! Deviations, by decision (ROADMAP 3.3):
//!
//! - A neutral chain is bypassed and bit-transparent, as beats' default ("lean") wave path is.
//!   HP, LP and the pitch shifter fade in over 5 ms when they leave their resting value and
//!   fade out over 5 ms when they settle back on it; their state starts from zero (the pitch
//!   shifter's delay lines keep the input history while bypassed). The phaser, crusher and
//!   distortion are bypassed while their wet is 0; their own 50 ms wet ramps are the fade.
//! - No compressor (beats' master ends in Tone.Compressor(-12, 3)).
//! - The phaser's LFO starts at phase 0 when the chain is made at Tone's initial 1 Hz. beats'
//!   reset value for the rate slider (0) maps to 0.1 Hz, so after a macro has restored
//!   `phaser-freq` the rate is 0.1 Hz, not 1 Hz, as in beats.
//!
//! Real-time: everything is allocated in [`MasterChain::new`]; `apply` and `process` allocate
//! nothing.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use libm::{cos, floor, pow, sin};

use super::master_macro::MasterFxKey;
use super::periodic_wave::{PeriodicWave, WaveOscillator};
use super::tone_param::ToneParam;
use crate::json::js_round;

/// One of the master chain's automated parameters, as the parity fixture names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MasterParamId {
    HpFrequency,
    LpFrequency,
    PhaserFrequency,
    PhaserWet,
    PhaserOctaves,
    CrusherWet,
    CrusherBits,
    DistortionWet,
    DistortionAmount,
    Pitch,
}

impl MasterParamId {
    pub const ALL: [MasterParamId; 10] = [
        MasterParamId::HpFrequency,
        MasterParamId::LpFrequency,
        MasterParamId::PhaserFrequency,
        MasterParamId::PhaserWet,
        MasterParamId::PhaserOctaves,
        MasterParamId::CrusherWet,
        MasterParamId::CrusherBits,
        MasterParamId::DistortionWet,
        MasterParamId::DistortionAmount,
        MasterParamId::Pitch,
    ];

    pub fn raw_value(self) -> &'static str {
        match self {
            MasterParamId::HpFrequency => "hp.frequency",
            MasterParamId::LpFrequency => "lp.frequency",
            MasterParamId::PhaserFrequency => "phaser.frequency",
            MasterParamId::PhaserWet => "phaser.wet",
            MasterParamId::PhaserOctaves => "phaser.octaves",
            MasterParamId::CrusherWet => "crusher.wet",
            MasterParamId::CrusherBits => "crusher.bits",
            MasterParamId::DistortionWet => "distortion.wet",
            MasterParamId::DistortionAmount => "distortion.amount",
            MasterParamId::Pitch => "pitch.pitch",
        }
    }

    pub fn from_raw(s: &str) -> Option<MasterParamId> {
        Self::ALL.iter().copied().find(|p| p.raw_value() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterParamOpKind {
    Cancel,
    Ramp,
    Set,
}

impl MasterParamOpKind {
    pub fn raw_value(self) -> &'static str {
        match self {
            MasterParamOpKind::Cancel => "cancel",
            MasterParamOpKind::Ramp => "ramp",
            MasterParamOpKind::Set => "set",
        }
    }
}

/// A Tone-level call one of tone-engine.js's setters made (for the parity test).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MasterParamOp {
    pub time: f64,
    pub param: MasterParamId,
    pub kind: MasterParamOpKind,
    pub a: f64,
    pub b: f64,
}

impl MasterParamOp {
    fn new(time: f64, param: MasterParamId, kind: MasterParamOpKind, a: f64, b: f64) -> Self {
        MasterParamOp {
            time,
            param,
            kind,
            a,
            b,
        }
    }
}

/// Where the Tone-level calls of one `apply` go (tests).
pub type OpLog<'a> = Option<&'a mut dyn FnMut(MasterParamOp)>;

/// The chain's parameters and tone-engine.js's master setters (`setHighpassFreq`,
/// `setLowpassFreq`, `setPhaserFreq`, `setPhaserDepth`, `setPhaserWet`, `setCrush`,
/// `setDistortion`, `setMasterPitch`), behind build.js's `applyFx`.
#[derive(Clone, Debug)]
pub struct MasterParams {
    pub hp: ToneParam,
    pub lp: ToneParam,
    pub phaser_rate: ToneParam,
    pub phaser_wet: ToneParam,
    pub crusher_wet: ToneParam,
    pub crusher_bits: ToneParam,
    pub distortion_wet: ToneParam,
    /// Plain properties (set at once).
    pub octaves: f64,
    pub distortion_amount: f64,
    pub pitch: f64,
}

impl MasterParams {
    pub fn new(sample_rate: f64) -> MasterParams {
        MasterParams {
            hp: ToneParam::new(20.0, true, sample_rate),
            lp: ToneParam::new(20000.0, true, sample_rate),
            phaser_rate: ToneParam::new(1.0, true, sample_rate),
            phaser_wet: ToneParam::new(0.0, false, sample_rate),
            crusher_wet: ToneParam::new(0.0, false, sample_rate),
            crusher_bits: ToneParam::new(16.0, false, sample_rate),
            distortion_wet: ToneParam::new(0.0, false, sample_rate),
            octaves: 3.0,
            distortion_amount: 0.0,
            pitch: 0.0,
        }
    }

    /// mixer-sliders.js `hpFreq`.
    pub fn hp_freq(v: f64) -> f64 {
        20.0 * pow(250.0, v / 100.0)
    }

    /// mixer-sliders.js `lpFreq`.
    pub fn lp_freq(v: f64) -> f64 {
        100.0 * pow(200.0, v / 100.0)
    }

    /// `applyFx(key, value)` at context time `now`. `log` (tests) sees every Tone-level call.
    pub fn apply(&mut self, key: MasterFxKey, value: i32, now: f64, log: OpLog<'_>) {
        self.apply_live(key, value as f64, now, None, log);
    }

    /// `applyFx` with a value between the slider's steps. `live` (the FX unit): a hand on a
    /// knob, ramped over `live` seconds (one block) with no cancel first, so each block's ramp
    /// starts where the last one ended (Tone's cancel-then-ramp would jump back every block);
    /// the crusher's bits are not rounded. `None` is beats' setter exactly (the macros, the
    /// faders).
    pub fn apply_live(
        &mut self,
        key: MasterFxKey,
        v: f64,
        now: f64,
        live: Option<f64>,
        mut log: OpLog<'_>,
    ) {
        use MasterParamId as P;
        use MasterParamOpKind as K;
        let mut emit = |op: MasterParamOp| {
            if let Some(l) = log.as_mut() {
                l(op);
            }
        };
        match key {
            MasterFxKey::HpFreq => {
                let f = 20.0f64.max(5000.0f64.min(Self::hp_freq(v)));
                if let Some(live) = live {
                    self.hp.ramp_to(f, live, now);
                    return;
                }
                self.hp.cancel_scheduled_values(now);
                self.hp.ramp_to(f, 0.1, now);
                emit(MasterParamOp::new(now, P::HpFrequency, K::Cancel, now, 0.0));
                emit(MasterParamOp::new(now, P::HpFrequency, K::Ramp, f, 0.1));
            }
            MasterFxKey::LpFreq => {
                let f = 100.0f64.max(20000.0f64.min(Self::lp_freq(v)));
                if let Some(live) = live {
                    self.lp.ramp_to(f, live, now);
                    return;
                }
                self.lp.cancel_scheduled_values(now);
                self.lp.ramp_to(f, 0.1, now);
                emit(MasterParamOp::new(now, P::LpFrequency, K::Cancel, now, 0.0));
                emit(MasterParamOp::new(now, P::LpFrequency, K::Ramp, f, 0.1));
            }
            MasterFxKey::PhaserFreq => {
                let rate = if v == 0.0 {
                    0.0
                } else {
                    0.1 + (v / 100.0) * 9.9
                };
                let r = 0.1f64.max(10.0f64.min(rate));
                self.phaser_rate.ramp_to(r, live.unwrap_or(0.1), now);
                emit(MasterParamOp::new(now, P::PhaserFrequency, K::Ramp, r, 0.1));
            }
            MasterFxKey::PhaserDepth => {
                self.octaves = 0.5f64.max((v / 100.0) * 6.0);
                emit(MasterParamOp::new(
                    now,
                    P::PhaserOctaves,
                    K::Set,
                    self.octaves,
                    0.0,
                ));
            }
            MasterFxKey::PhaserWet => {
                let w = 0.0f64.max(1.0f64.min(v / 100.0));
                self.phaser_wet.ramp_to(w, live.unwrap_or(0.05), now);
                emit(MasterParamOp::new(now, P::PhaserWet, K::Ramp, w, 0.05));
            }
            MasterFxKey::CrushBits => {
                let amount = v / 100.0;
                let wet = if amount <= 0.0 { 0.0 } else { 1.0 };
                self.crusher_wet.ramp_to(wet, 0.05, now);
                emit(MasterParamOp::new(now, P::CrusherWet, K::Ramp, wet, 0.05));
                if amount > 0.0 {
                    let bits = if live.is_none() {
                        1.0f64.max(js_round(16.0 - amount * 15.0))
                    } else {
                        1.0f64.max(16.0 - amount * 15.0)
                    };
                    self.crusher_bits.set_immediate(bits, now);
                    emit(MasterParamOp::new(now, P::CrusherBits, K::Set, bits, 0.0));
                }
            }
            MasterFxKey::Distortion => {
                let a = 0.0f64.max(1.0f64.min(v / 100.0));
                let wet = if a > 0.0 { 1.0 } else { 0.0 };
                self.distortion_amount = a;
                emit(MasterParamOp::new(now, P::DistortionAmount, K::Set, a, 0.0));
                self.distortion_wet.ramp_to(wet, 0.05, now);
                emit(MasterParamOp::new(
                    now,
                    P::DistortionWet,
                    K::Ramp,
                    wet,
                    0.05,
                ));
            }
            MasterFxKey::MasterPitch => {
                self.pitch = (-24.0f64).max(24.0f64.min(v));
                emit(MasterParamOp::new(now, P::Pitch, K::Set, self.pitch, 0.0));
            }
        }
    }

    /// A timeline parameter's value at `t` (what Tone's `getValueAtTime` reads).
    pub fn value(&self, id: MasterParamId, t: f64) -> f64 {
        match id {
            MasterParamId::HpFrequency => self.hp.value_at(t),
            MasterParamId::LpFrequency => self.lp.value_at(t),
            MasterParamId::PhaserFrequency => self.phaser_rate.value_at(t),
            MasterParamId::PhaserWet => self.phaser_wet.value_at(t),
            MasterParamId::CrusherWet => self.crusher_wet.value_at(t),
            MasterParamId::CrusherBits => self.crusher_bits.value_at(t),
            MasterParamId::DistortionWet => self.distortion_wet.value_at(t),
            MasterParamId::PhaserOctaves => self.octaves,
            MasterParamId::DistortionAmount => self.distortion_amount,
            MasterParamId::Pitch => self.pitch,
        }
    }

    pub fn prune(&mut self, t: f64) {
        self.hp.prune(t);
        self.lp.prune(t);
        self.phaser_rate.prune(t);
        self.phaser_wet.prune(t);
        self.crusher_wet.prune(t);
        self.crusher_bits.prune(t);
        self.distortion_wet.prune(t);
    }
}

/// The stages, for [`MasterChain::solo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Highpass,
    Phaser,
    Lowpass,
    Crusher,
    Distortion,
    Pitch,
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    const UNITY: Biquad = Biquad {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

/// A stereo (or mono) block being processed in place.
struct Block<'a> {
    l: &'a mut [f32],
    r: Option<&'a mut [f32]>,
}

impl<'a> Block<'a> {
    fn channels(&self) -> usize {
        if self.r.is_none() {
            1
        } else {
            2
        }
    }
    #[inline]
    fn get(&self, ch: usize, k: usize) -> f32 {
        if ch == 0 {
            self.l[k]
        } else {
            self.r.as_ref().unwrap()[k]
        }
    }
    #[inline]
    fn set(&mut self, ch: usize, k: usize, v: f32) {
        if ch == 0 {
            self.l[k] = v;
        } else {
            self.r.as_mut().unwrap()[k] = v;
        }
    }
    /// `r ?? l`.
    #[inline]
    fn right_or_left(&self, k: usize) -> f32 {
        match &self.r {
            Some(r) => r[k],
            None => self.l[k],
        }
    }
}

#[inline]
fn tm(frame: i64, sample_rate: f64, k: usize) -> f64 {
    (frame + k as i64) as f64 / sample_rate
}

/// The DSP of the chain, in place on a deck's block.
pub struct MasterChain {
    pub sample_rate: f64,
    params: MasterParams,
    /// Context frames processed so far: sample `k` of the block being processed is at
    /// `(frame + k) / sr` s.
    frame: i64,

    fade_step: f64,

    /// Off: every stage runs as Tone's node does, at rest too (no bypass, no fades: HP at
    /// 20 Hz, LP at 20 kHz, the pitch shifter's fixed taps at 0). For comparing with Chrome;
    /// the deck keeps it on.
    rest_bypass: bool,
    /// Tests: run only this stage.
    pub solo: Option<Stage>,

    // HP / LP: [channel][stage][x1, x2, y1, y2]
    hp_state: Vec<f64>,
    lp_state: Vec<f64>,
    hp_mix: f64,
    lp_mix: f64,
    hp_coef: Biquad,
    lp_coef: Biquad,
    hp_coef_f: f64,
    lp_coef_f: f64,

    // phaser: [channel][stage][x1, x2, y1, y2]
    ap_state: Vec<f64>,
    phaser_phase: f64,
    phaser_running: bool,

    // crusher
    bits_k: f64,
    crusher_running: bool,

    // distortion
    curve: Vec<f32>,
    curve_amount: f64,

    // pitch shift: two rings (left, right) of `ring_size`
    ring: Vec<f32>,
    ring_size: usize,
    ring_write: usize,
    saw_a: Arc<PeriodicWave>,
    saw_b: Arc<PeriodicWave>,
    fade_wave: Arc<PeriodicWave>,
    lfo_a: WaveOscillator,
    lfo_b: WaveOscillator,
    lfo_fade: WaveOscillator,
    pitch_mix: f64,
    pitch_rate: f64,
    pitch_min: f64,
    pitch_max: f64,
}

impl MasterChain {
    pub const FILTER_STAGES: usize = 3;
    pub const FILTER_Q_DB: f64 = 1.0;
    pub const PHASER_STAGES: usize = 10;
    pub const PHASER_Q: f64 = 10.0;
    pub const PHASER_BASE: f64 = 350.0;
    pub const QUANTUM: i64 = 128;
    pub const CURVE_LENGTH: usize = 1024;
    pub const WINDOW: f64 = 0.03;
    /// Peak of Chrome's full sawtooth table at 44.1 / 48 kHz (2047 partials over 4096
    /// points), which the PeriodicWave normalisation divides by: the slope of the sawtooth's
    /// ramp is 1 / this.
    pub const SAW_PEAK: f64 = 1.1784914234857347;
    /// Fade in / out of bypass.
    pub const FADE_SECONDS: f64 = 0.005;
    /// Tone.CrossFade's GainToAudio: a 1024-point WaveShaper of `|x|*2 - 1`.
    pub const GAIN_TO_AUDIO_LENGTH: usize = 1024;

    const PASS_STATE_COUNT: usize = 2 * Self::FILTER_STAGES * 4;
    const AP_STATE_COUNT: usize = 2 * Self::PHASER_STAGES * 4;

    pub fn new(sample_rate: f64) -> MasterChain {
        let mut n = 1usize;
        while n < (Self::WINDOW * sample_rate) as usize + 8 {
            n <<= 1;
        }
        let mut c = MasterChain {
            sample_rate,
            params: MasterParams::new(sample_rate),
            frame: 0,
            fade_step: 1.0 / (Self::FADE_SECONDS * sample_rate),
            rest_bypass: true,
            solo: None,
            hp_state: vec![0.0; Self::PASS_STATE_COUNT],
            lp_state: vec![0.0; Self::PASS_STATE_COUNT],
            hp_mix: 0.0,
            lp_mix: 0.0,
            hp_coef: Biquad::UNITY,
            lp_coef: Biquad::UNITY,
            hp_coef_f: -1.0,
            lp_coef_f: -1.0,
            ap_state: vec![0.0; Self::AP_STATE_COUNT],
            phaser_phase: 0.0,
            phaser_running: false,
            bits_k: 16.0,
            crusher_running: false,
            curve: vec![0.0; Self::CURVE_LENGTH],
            curve_amount: f64::NAN,
            ring: vec![0.0; 2 * n],
            ring_size: n,
            ring_write: 0,
            saw_a: PeriodicWave::sawtooth(sample_rate),
            saw_b: PeriodicWave::sawtooth180(sample_rate),
            fade_wave: PeriodicWave::triangle90(sample_rate),
            lfo_a: WaveOscillator::new(),
            lfo_b: WaveOscillator::new(),
            lfo_fade: WaveOscillator::new(),
            pitch_mix: 0.0,
            pitch_rate: 0.0,
            pitch_min: 0.03,
            pitch_max: 0.0,
        };
        c.rebuild_curve(0.0);
        c.set_pitch(0.0);
        c
    }

    pub fn params(&self) -> &MasterParams {
        &self.params
    }

    pub fn frame(&self) -> i64 {
        self.frame
    }

    pub fn rest_bypass(&self) -> bool {
        self.rest_bypass
    }

    /// `restBypass` with its `didSet`: turning it off engages the fades-free stages.
    pub fn set_rest_bypass(&mut self, on: bool) {
        self.rest_bypass = on;
        if !on {
            self.hp_mix = 1.0;
            self.lp_mix = 1.0;
            self.pitch_mix = 1.0;
        }
    }

    fn runs(&self, stage: Stage) -> bool {
        self.solo.is_none() || self.solo == Some(stage)
    }

    /// Tone.CrossFade (an equal-power StereoPannerNode on a constant): the dry and wet gains
    /// for `fade`. The fade reaches the panner through GainToAudio, a 1024-point curve of
    /// `|x|*2 - 1` that the WaveShaper interpolates: between its two points around 0 the curve
    /// does not reach -1, so at fade 0 the pan is -0.998 and the wet leaks in at -56 dB (dry
    /// 0.9999988, wet 0.00153; measured in Chrome). Exact from fade 1/1023 up. Returns
    /// `(dry, wet)`.
    #[inline]
    pub fn crossfade(fade: f64) -> (f64, f64) {
        let n = Self::GAIN_TO_AUDIO_LENGTH;
        let v = 0.5 * (fade + 1.0) * (n - 1) as f64;
        fn curve(i: usize, n: usize) -> f64 {
            let x = ((i as f64 / (n - 1) as f64) * 2.0 - 1.0).abs() * 2.0 - 1.0;
            x as f32 as f64
        }
        let pan = if v < 0.0 {
            curve(0, n)
        } else if v >= (n - 1) as f64 {
            curve(n - 1, n)
        } else {
            let i = v as usize;
            let f = v - i as f64;
            (1.0 - f) * curve(i, n) + f * curve(i + 1, n)
        };
        let x = (((pan as f32) as f64).min(1.0).max(-1.0) + 1.0) / 2.0;
        (
            cos(x * core::f64::consts::PI / 2.0),
            sin(x * core::f64::consts::PI / 2.0),
        )
    }

    /// The chain's time at sample `k` of the block being processed.
    #[inline]
    pub fn time_at(&self, k: usize) -> f64 {
        tm(self.frame, self.sample_rate, k)
    }

    /// Is every stage bypassed and settled at sample `k` (the chain leaves the audio alone)?
    pub fn is_idle(&self, k: usize) -> bool {
        let t = self.time_at(k);
        self.hp_mix == 0.0
            && self.lp_mix == 0.0
            && self.pitch_mix == 0.0
            && self.params.pitch == 0.0
            && is_at(&self.params.hp, 20.0, t)
            && is_at(&self.params.lp, 20000.0, t)
            && is_at(&self.params.phaser_wet, 0.0, t)
            && is_at(&self.params.crusher_wet, 0.0, t)
            && is_at(&self.params.distortion_wet, 0.0, t)
    }

    /// The HP / LP corners at sample `k` of the block (for the scope model).
    pub fn filter_corners(&self, k: usize) -> (f64, f64) {
        let t = self.time_at(k);
        (self.params.hp.value_at(t), self.params.lp.value_at(t))
    }

    /// `applyFx(key, value)` at context time `now` (taking effect from the next sample
    /// processed).
    pub fn apply(&mut self, key: MasterFxKey, value: i32, now: f64) {
        self.apply_live(key, value as f64, now, None);
    }

    /// `apply` with a value between the slider's steps; `live`: the FX unit's ramp over one
    /// block ([`MasterParams::apply_live`]).
    pub fn apply_live(&mut self, key: MasterFxKey, value: f64, now: f64, live: Option<f64>) {
        self.params.apply_live(key, value, now, live, None);
        match key {
            MasterFxKey::Distortion => {
                if self.params.distortion_amount != self.curve_amount {
                    let a = self.params.distortion_amount;
                    self.rebuild_curve(a);
                }
            }
            MasterFxKey::MasterPitch => {
                let p = self.params.pitch;
                self.set_pitch(p);
            }
            _ => {}
        }
    }

    /// Process samples `start..end` of the current block in place. Ops applied between two
    /// calls land at that sample. Call [`finish`](Self::finish) after the block's last segment.
    pub fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        if end <= start {
            return;
        }
        let mut b = Block { l: left, r: right };
        if self.rest_bypass && self.is_idle(start) {
            self.advance_phaser(start, end);
            self.write_history(&b, start, end);
            return;
        }
        if self.runs(Stage::Highpass) {
            self.pass_stage(false, &mut b, start, end);
        }
        if self.runs(Stage::Phaser) {
            self.phaser(&mut b, start, end);
        } else {
            self.advance_phaser(start, end);
        }
        if self.runs(Stage::Lowpass) {
            self.pass_stage(true, &mut b, start, end);
        }
        if self.runs(Stage::Crusher) {
            self.crusher(&mut b, start, end);
        }
        if self.runs(Stage::Distortion) {
            self.distortion(&mut b, start, end);
        }
        if self.runs(Stage::Pitch) {
            self.pitch_shift(&mut b, start, end);
        }
    }

    /// End of a block of `frames`: advance the clock, forget events in the past.
    pub fn finish(&mut self, frames: usize) {
        self.frame += frames as i64;
        let t = self.time_at(0);
        self.params.prune(t);
    }

    // MARK: HP / LP

    /// Chrome `Biquad::SetLowpassParams` / `SetHighpassParams` (Q in dB), normalised by a0.
    fn pass_coefficients(&self, lowpass: bool, f: f64) -> Biquad {
        let cutoff = (f / (self.sample_rate / 2.0)).min(1.0).max(0.0);
        if cutoff == 1.0 {
            return if lowpass {
                Biquad::UNITY
            } else {
                Biquad {
                    b0: 0.0,
                    ..Biquad::UNITY
                }
            };
        }
        if cutoff <= 0.0 {
            return if lowpass {
                Biquad {
                    b0: 0.0,
                    ..Biquad::UNITY
                }
            } else {
                Biquad::UNITY
            };
        }
        let resonance = pow(10.0, Self::FILTER_Q_DB / 20.0);
        let theta = core::f64::consts::PI * cutoff;
        let alpha = sin(theta) / (2.0 * resonance);
        let cosw = cos(theta);
        let beta = if lowpass {
            (1.0 - cosw) / 2.0
        } else {
            (1.0 + cosw) / 2.0
        };
        let b1 = if lowpass { 2.0 * beta } else { -2.0 * beta };
        let a0 = 1.0 + alpha;
        Biquad {
            b0: beta / a0,
            b1: b1 / a0,
            b2: beta / a0,
            a1: -2.0 * cosw / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Chrome's `Biquad::Process` (direct form I, float output) on one sample.
    #[inline]
    fn biquad(c: &Biquad, x: f64, st: &mut [f64]) -> f64 {
        let y = (c.b0 * x + c.b1 * st[0] + c.b2 * st[1] - c.a1 * st[2] - c.a2 * st[3]) as f32;
        st[1] = st[0];
        st[0] = x;
        st[3] = st[2];
        st[2] = y as f64;
        y as f64
    }

    fn flush_denormals(st: &mut [f64]) {
        for x in st.iter_mut() {
            if x.abs() < 1e-30 {
                *x = 0.0;
            }
        }
    }

    fn pass_stage(&mut self, lowpass: bool, b: &mut Block<'_>, s: usize, e: usize) {
        let frame = self.frame;
        let sr = self.sample_rate;
        let t0 = tm(frame, sr, s);
        let rest = if lowpass { 20000.0 } else { 20.0 };
        let param = if lowpass {
            self.params.lp
        } else {
            self.params.hp
        };
        let settled = param.is_settled(t0);
        let constant = param.value_at(t0);
        let target: f64 = if self.rest_bypass && settled && constant == rest {
            0.0
        } else {
            1.0
        };
        let mut mix = if lowpass { self.lp_mix } else { self.hp_mix };
        if mix == 0.0 && target == 0.0 {
            return;
        }
        let mut st = core::mem::take(if lowpass {
            &mut self.lp_state
        } else {
            &mut self.hp_state
        });
        if mix == 0.0 {
            st.iter_mut().for_each(|x| *x = 0.0); // engaging: fresh state
        }
        let mut coef = if lowpass { self.lp_coef } else { self.hp_coef };
        let mut coef_f = if lowpass {
            self.lp_coef_f
        } else {
            self.hp_coef_f
        };
        let channels = b.channels();
        for k in s..e {
            let f = if settled {
                constant
            } else {
                param.value_at(tm(frame, sr, k))
            };
            if f != coef_f {
                coef = self.pass_coefficients(lowpass, f);
                coef_f = f;
            }
            if mix < target {
                mix = target.min(mix + self.fade_step);
            } else if mix > target {
                mix = target.max(mix - self.fade_step);
            }
            for ch in 0..channels {
                let x0 = b.get(ch, k) as f64;
                let mut x = x0;
                for stage in 0..Self::FILTER_STAGES {
                    let o = (ch * Self::FILTER_STAGES + stage) * 4;
                    x = Self::biquad(&coef, x, &mut st[o..o + 4]);
                }
                let out = if mix >= 1.0 {
                    x as f32
                } else {
                    (x0 + (x - x0) * mix) as f32
                };
                b.set(ch, k, out);
            }
        }
        Self::flush_denormals(&mut st);
        if lowpass {
            self.lp_state = st;
            self.lp_mix = mix;
            self.lp_coef = coef;
            self.lp_coef_f = coef_f;
        } else {
            self.hp_state = st;
            self.hp_mix = mix;
            self.hp_coef = coef;
            self.hp_coef_f = coef_f;
        }
    }

    // MARK: phaser

    /// Chrome `Biquad::SetAllpassParams` (Q linear), normalised.
    #[inline]
    fn allpass_coefficients(&self, f: f64) -> Biquad {
        let frequency = (f / (self.sample_rate / 2.0)).min(1.0).max(0.0);
        if !(frequency > 0.0 && frequency < 1.0) {
            return Biquad::UNITY;
        }
        let w0 = core::f64::consts::PI * frequency;
        let alpha = sin(w0) / (2.0 * Self::PHASER_Q);
        let k = cos(w0);
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (1.0 - alpha) / a0,
            b1: -2.0 * k / a0,
            b2: (1.0 + alpha) / a0,
            a1: -2.0 * k / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// The LFO runs whether or not anything hears it.
    fn advance_phaser(&mut self, s: usize, e: usize) {
        let t0 = self.time_at(s);
        if self.params.phaser_rate.is_settled(t0) {
            self.phaser_phase +=
                self.params.phaser_rate.value_at(t0) * (e - s) as f64 / self.sample_rate;
        } else {
            for k in s..e {
                self.phaser_phase +=
                    self.params.phaser_rate.value_at(self.time_at(k)) / self.sample_rate;
            }
        }
        self.phaser_phase -= floor(self.phaser_phase);
    }

    fn phaser(&mut self, b: &mut Block<'_>, s: usize, e: usize) {
        let frame = self.frame;
        let sr = self.sample_rate;
        let t0 = tm(frame, sr, s);
        let wet_settled = self.params.phaser_wet.is_settled(t0);
        let wet0 = self.params.phaser_wet.value_at(t0);
        if self.rest_bypass && wet_settled && wet0 == 0.0 {
            if self.phaser_running {
                self.phaser_running = false;
                self.ap_state.iter_mut().for_each(|x| *x = 0.0);
            }
            self.advance_phaser(s, e);
            return;
        }
        self.phaser_running = true;
        let rate_settled = self.params.phaser_rate.is_settled(t0);
        let rate0 = self.params.phaser_rate.value_at(t0);
        let min_f = Self::PHASER_BASE;
        let max_f = Self::PHASER_BASE * pow(2.0, self.params.octaves);
        let channels = b.channels();
        let mut phase = self.phaser_phase;
        let mut ap = core::mem::take(&mut self.ap_state);
        for k in s..e {
            let t = tm(frame, sr, k);
            let w = if wet_settled {
                wet0
            } else {
                self.params.phaser_wet.value_at(t)
            };
            let osc = sin(2.0 * core::f64::consts::PI * phase);
            let (gd, gw) = Self::crossfade(w);
            for ch in 0..channels {
                // Tone.LFO: oscillator -> AudioToGain ((x + 1) / 2) -> Scale (x (max - min) + min)
                let f = ((if ch == 0 { osc } else { -osc }) + 1.0) / 2.0 * (max_f - min_f) + min_f;
                let c = self.allpass_coefficients(f);
                let x0 = b.get(ch, k) as f64;
                let mut x = x0;
                for stage in 0..Self::PHASER_STAGES {
                    let o = (ch * Self::PHASER_STAGES + stage) * 4;
                    x = Self::biquad(&c, x, &mut ap[o..o + 4]);
                }
                b.set(ch, k, (x0 * gd + x * gw) as f32);
            }
            phase += (if rate_settled {
                rate0
            } else {
                self.params.phaser_rate.value_at(t)
            }) / sr;
            phase -= floor(phase);
        }
        Self::flush_denormals(&mut ap);
        self.ap_state = ap;
        self.phaser_phase = phase;
    }

    // MARK: bit crusher

    fn crusher(&mut self, b: &mut Block<'_>, s: usize, e: usize) {
        let frame = self.frame;
        let sr = self.sample_rate;
        let t0 = tm(frame, sr, s);
        let wet_settled = self.params.crusher_wet.is_settled(t0);
        let wet0 = self.params.crusher_wet.value_at(t0);
        if self.rest_bypass && wet_settled && wet0 == 0.0 {
            self.crusher_running = false;
            return;
        }
        if !self.crusher_running {
            // the k-rate value of the render quantum this sample is in
            let q = (frame + s as i64) / Self::QUANTUM * Self::QUANTUM;
            self.bits_k = self.params.crusher_bits.value_at(q as f64 / sr);
            self.crusher_running = true;
        }
        let channels = b.channels();
        for k in s..e {
            if (frame + k as i64) % Self::QUANTUM == 0 {
                self.bits_k = self.params.crusher_bits.value_at(tm(frame, sr, k));
            }
            let w = if wet_settled {
                wet0
            } else {
                self.params.crusher_wet.value_at(tm(frame, sr, k))
            };
            let (gd, gw) = Self::crossfade(w);
            let step = pow(0.5, self.bits_k - 1.0);
            for ch in 0..channels {
                let x = b.get(ch, k) as f64;
                // the worklet's output is a Float32Array
                let crushed = crush_quantize(x, step);
                b.set(ch, k, (x * gd + crushed * gw) as f32);
            }
        }
    }

    // MARK: distortion

    /// Distortion's setter: `setMap` over WaveShaper's default 1024 points.
    fn rebuild_curve(&mut self, amount: f64) {
        self.curve_amount = amount;
        let k = amount * 100.0;
        let deg = core::f64::consts::PI / 180.0;
        let n = Self::CURVE_LENGTH;
        for i in 0..n {
            let x = (i as f64 / (n - 1) as f64) * 2.0 - 1.0;
            self.curve[i] = if x.abs() < 0.001 {
                0.0
            } else {
                (((3.0 + k) * x * 20.0 * deg) / (core::f64::consts::PI + k * x.abs())) as f32
            };
        }
    }

    /// The curve lookup (the spec's WaveShaper, as Chrome's `WaveShaperDSPKernel` does it).
    #[inline]
    pub fn shape(&self, input: f32) -> f64 {
        let n = Self::CURVE_LENGTH;
        let v = 0.5 * (input as f64 + 1.0) * (n - 1) as f64;
        if v < 0.0 {
            return self.curve[0] as f64;
        }
        if v >= (n - 1) as f64 {
            return self.curve[n - 1] as f64;
        }
        let i = v as usize;
        let f = v - i as f64;
        (1.0 - f) * self.curve[i] as f64 + f * self.curve[i + 1] as f64
    }

    fn distortion(&mut self, b: &mut Block<'_>, s: usize, e: usize) {
        let frame = self.frame;
        let sr = self.sample_rate;
        let t0 = tm(frame, sr, s);
        let wet_settled = self.params.distortion_wet.is_settled(t0);
        let wet0 = self.params.distortion_wet.value_at(t0);
        if self.rest_bypass && wet_settled && wet0 == 0.0 {
            return;
        }
        let channels = b.channels();
        for k in s..e {
            let w = if wet_settled {
                wet0
            } else {
                self.params.distortion_wet.value_at(tm(frame, sr, k))
            };
            let (gd, gw) = Self::crossfade(w);
            for ch in 0..channels {
                let x = b.get(ch, k);
                let y = (self.shape(x)) as f32 as f64;
                b.set(ch, k, (x as f64 * gd + y * gw) as f32);
            }
        }
    }

    // MARK: pitch shift

    /// PitchShift's `pitch` setter: which way the sawtooths sweep, and the LFOs' rate.
    fn set_pitch(&mut self, p: f64) {
        let factor;
        if p < 0.0 {
            self.pitch_min = 0.0;
            self.pitch_max = Self::WINDOW;
            factor = pow(2.0, (p - 1.0) / 12.0) + 1.0;
        } else {
            self.pitch_min = Self::WINDOW;
            self.pitch_max = 0.0;
            factor = pow(2.0, p / 12.0) - 1.0;
        }
        self.pitch_rate = factor * (1.2 / Self::WINDOW);
        self.lfo_a.set_frequency(self.pitch_rate, &self.saw_a);
        self.lfo_b.set_frequency(self.pitch_rate, &self.saw_b);
        self.lfo_fade
            .set_frequency(self.pitch_rate, &self.fade_wave);
    }

    /// Keep the history the delay lines read while the stage is bypassed.
    fn write_history(&mut self, b: &Block<'_>, s: usize, e: usize) {
        let mask = self.ring_size - 1;
        let mut w = self.ring_write;
        for k in s..e {
            self.ring[w] = b.l[k];
            self.ring[self.ring_size + w] = b.right_or_left(k);
            w = (w + 1) & mask;
        }
        self.ring_write = w;
    }

    /// Chrome's DelayNode read (the input is in the ring already): linear interpolation.
    #[inline]
    fn read_delay(base: &[f32], ring_size: usize, sample_rate: f64, w: usize, delay: f64) -> f64 {
        let size = ring_size as f64;
        let mut pos = w as f64 + size - delay.min(1.0).max(0.0) * sample_rate;
        if pos >= size {
            pos -= size;
        }
        let i1 = pos as usize;
        let i2 = (i1 + 1) & (ring_size - 1);
        let f = pos - i1 as f64;
        (1.0 - f) * base[i1] as f64 + f * base[i2] as f64
    }

    fn pitch_shift(&mut self, b: &mut Block<'_>, s: usize, e: usize) {
        let target: f64 = if self.params.pitch != 0.0 || !self.rest_bypass {
            1.0
        } else {
            0.0
        };
        if self.pitch_mix == 0.0 && target == 0.0 {
            self.write_history(b, s, e);
            return;
        }
        let ring_size = self.ring_size;
        let mask = ring_size - 1;
        let channels = b.channels();
        let sr = self.sample_rate;
        let mut mix = self.pitch_mix;
        let mut w = self.ring_write;
        let min_d = self.pitch_min;
        let max_d = self.pitch_max;
        for k in s..e {
            self.ring[w] = b.l[k];
            self.ring[ring_size + w] = b.right_or_left(k);
            // Tone.LFO: (osc + 1) / 2 -> x (max - min) + min
            let d_a = (self.lfo_a.next(&self.saw_a) + 1.0) / 2.0 * (max_d - min_d) + min_d;
            let d_b = (self.lfo_b.next(&self.saw_b) + 1.0) / 2.0 * (max_d - min_d) + min_d;
            let fade = (self.lfo_fade.next(&self.fade_wave) + 1.0) / 2.0;
            let (ga, gb) = Self::crossfade(fade);
            if mix < target {
                mix = target.min(mix + self.fade_step);
            } else if mix > target {
                mix = target.max(mix - self.fade_step);
            }
            for ch in 0..channels {
                let base = &self.ring[ch * ring_size..(ch + 1) * ring_size];
                let y = ga * Self::read_delay(base, ring_size, sr, w, d_a)
                    + gb * Self::read_delay(base, ring_size, sr, w, d_b);
                let x = b.get(ch, k) as f64;
                b.set(ch, k, (x + (y - x) * mix) as f32);
            }
            w = (w + 1) & mask;
        }
        self.ring_write = w;
        self.pitch_mix = mix;
    }
}

fn is_at(p: &ToneParam, v: f64, t: f64) -> bool {
    p.is_settled(t) && p.value_at(t) == v
}

/// The crusher's quantiser (`crushQuantize`, shared with the loops' crush): `x` quantised down
/// to a step of 2^(1 - bits), through f32 as the worklet's output is.
#[inline]
pub fn crush_quantize(x: f64, step: f64) -> f64 {
    (step * floor(x / step + 0.5)) as f32 as f64
}

/// The crusher's step for `bits` (`crushStep`).
#[inline]
pub fn crush_step(bits: f64) -> f64 {
    pow(0.5, bits - 1.0)
}
