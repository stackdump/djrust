//! A channel vocoder and a formant (vowel) filter (beyond Swift; docs/VOCODER.md).
//!
//! Pure DSP, no I/O: the host feeds a modulator (a voice, a loop) and a carrier (the TD-3, a
//! deck, [`CarrierSynth`]) a block at a time and gets the vocoded block back.
//!
//! The vocoder is the classic channel design: the modulator and the carrier each go through a
//! bank of band-pass filters (two cascaded constant-peak biquads per band, log-spaced from
//! [`LOW_HZ`] to [`HIGH_HZ`]); an envelope follower (attack / release) on each modulator band
//! sets the gain of the matching carrier band, and the bands are summed. Moving the carrier's
//! bank up or down against the modulator's is the formant shift. Unvoiced sounds (s, t, sh)
//! have little in a tonal carrier to shape, so pre-emphasised white noise is blended into the
//! carrier ([`VocoderSettings::noise`]), more where the modulator's top bands carry more of its
//! energy than the rest. Dry / wet mixes the carrier itself back in.
//!
//! [`FormantFilter`] is three parallel band-passes at a vowel's formants (a e i o u, morphing
//! between neighbours), on whatever comes out of the vocoder, or on the carrier alone when there
//! is no modulator: a talk-box sweep on a synth line.
//!
//! Real-time safe once built: every filter is allocated for [`MAX_BANDS`] up front, and changing
//! the band count only recomputes coefficients.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::f64::consts::PI;

use crate::json::{JsonObject, JsonValue};

/// The most bands the bank holds.
pub const MAX_BANDS: usize = 32;
/// The fewest.
pub const MIN_BANDS: usize = 4;
/// The lowest band's centre, Hz.
pub const LOW_HZ: f64 = 110.0;
/// The highest band's centre, Hz (kept under 0.42 of the sample rate).
pub const HIGH_HZ: f64 = 7_600.0;
/// Voices the internal synth plays at once.
pub const SYNTH_VOICES: usize = 8;

// MARK: a biquad

/// A transposed direct form II biquad. Coefficients in f64 at design, run in f32.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// RBJ band-pass, constant 0 dB peak gain.
    fn bandpass(fc: f64, q: f64, sr: f64) -> Biquad {
        let w = 2.0 * PI * (fc / sr).clamp(1e-5, 0.49);
        let alpha = libm::sin(w) / (2.0 * q.max(0.05));
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (alpha / a0) as f32,
            b1: 0.0,
            b2: (-alpha / a0) as f32,
            a1: (-2.0 * libm::cos(w) / a0) as f32,
            a2: ((1.0 - alpha) / a0) as f32,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// New coefficients, the state kept (a sweep does not click).
    fn retune(&mut self, other: Biquad) {
        self.b0 = other.b0;
        self.b1 = other.b1;
        self.b2 = other.b2;
        self.a1 = other.a1;
        self.a2 = other.a2;
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Two cascaded band-passes: a steeper skirt, so neighbouring bands overlap less.
#[derive(Clone, Copy, Debug, Default)]
struct Band {
    a: Biquad,
    b: Biquad,
}

impl Band {
    fn new(fc: f64, q: f64, sr: f64) -> Band {
        Band {
            a: Biquad::bandpass(fc, q, sr),
            b: Biquad::bandpass(fc, q, sr),
        }
    }
    fn retune(&mut self, fc: f64, q: f64, sr: f64) {
        let f = Biquad::bandpass(fc, q, sr);
        self.a.retune(f);
        self.b.retune(f);
    }
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        self.b.tick(self.a.tick(x))
    }
    fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
    }
}

/// The centre of band `k` of `n`, Hz, log-spaced from [`LOW_HZ`] to the top (under 0.42 sr).
pub fn band_center(k: usize, n: usize, sample_rate: f64) -> f64 {
    let hi = HIGH_HZ.min(0.42 * sample_rate);
    if n <= 1 {
        return libm::sqrt(LOW_HZ * hi);
    }
    LOW_HZ * libm::pow(hi / LOW_HZ, k as f64 / (n - 1) as f64)
}

/// The Q that makes `n` log-spaced bands meet about 3 dB down.
pub fn band_q(n: usize, sample_rate: f64) -> f64 {
    let hi = HIGH_HZ.min(0.42 * sample_rate);
    let r = libm::pow(hi / LOW_HZ, 1.0 / (n.max(2) - 1) as f64);
    // bandwidth of a band whose edges sit halfway (in log) to its neighbours
    let bw = libm::sqrt(r) - 1.0 / libm::sqrt(r);
    // the cascade narrows each biquad's passband; widen to compensate
    (1.0 / bw) * 0.75
}

// MARK: settings

/// A vocoder knob, in its own units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VocoderParam {
    /// Band count, [`MIN_BANDS`]..[`MAX_BANDS`].
    Bands,
    /// The carrier bank against the modulator's, semitones (−12..12).
    Shift,
    /// Envelope attack, ms (0.5..200).
    Attack,
    /// Envelope release, ms (5..1000).
    Release,
    /// Dry (the carrier) / wet, 0..1.
    Mix,
    /// Unvoiced noise blended into the carrier, 0..1.
    Noise,
    /// The vocoder strip's fader, 0..1.
    Volume,
    /// The formant filter's vowel: 0 a, 1 e, 2 i, 3 o, 4 u, fractions morph.
    Vowel,
    /// How much of the formant filter is heard, 0 (off)..1.
    VowelMix,
    /// The internal synth's level, 0..1.
    SynthLevel,
    /// The internal synth's detune spread between osc 1 and 2, cents (0..50).
    SynthDetune,
    // the synth voice (docs/VOCODER.md, "The synth"): oscillators 1-3
    Osc1Wave,
    Osc1Oct,
    Osc1Fine,
    Osc1Level,
    Osc2Wave,
    Osc2Oct,
    Osc2Fine,
    Osc2Level,
    Osc3Wave,
    Osc3Oct,
    Osc3Fine,
    Osc3Level,
    /// The square's pulse width, 0.05..0.5.
    Pwm,
    /// Osc 2 hard-synced to osc 1, 0 / 1.
    Sync,
    /// Osc 2 into osc 1 × osc 2, 0..1.
    Ring,
    /// The filter's cutoff, Hz (30..18000).
    Cutoff,
    /// 0..1.
    Resonance,
    /// The decay envelope on the cutoff, -1..1 (×5 octaves).
    EnvAmount,
    /// The decay envelope's time, ms (5..3000).
    EnvDecay,
    /// The gate's attack and release, ms.
    SynthAttack,
    SynthRelease,
    /// Analog wobble, 0..1.
    Drift,
}

impl VocoderParam {
    pub const ALL: [VocoderParam; 33] = [
        VocoderParam::Bands,
        VocoderParam::Shift,
        VocoderParam::Attack,
        VocoderParam::Release,
        VocoderParam::Mix,
        VocoderParam::Noise,
        VocoderParam::Volume,
        VocoderParam::Vowel,
        VocoderParam::VowelMix,
        VocoderParam::SynthLevel,
        VocoderParam::SynthDetune,
        VocoderParam::Osc1Wave,
        VocoderParam::Osc1Oct,
        VocoderParam::Osc1Fine,
        VocoderParam::Osc1Level,
        VocoderParam::Osc2Wave,
        VocoderParam::Osc2Oct,
        VocoderParam::Osc2Fine,
        VocoderParam::Osc2Level,
        VocoderParam::Osc3Wave,
        VocoderParam::Osc3Oct,
        VocoderParam::Osc3Fine,
        VocoderParam::Osc3Level,
        VocoderParam::Pwm,
        VocoderParam::Sync,
        VocoderParam::Ring,
        VocoderParam::Cutoff,
        VocoderParam::Resonance,
        VocoderParam::EnvAmount,
        VocoderParam::EnvDecay,
        VocoderParam::SynthAttack,
        VocoderParam::SynthRelease,
        VocoderParam::Drift,
    ];

    /// The synth's own knobs (what a synth preset holds).
    pub const SYNTH: [VocoderParam; 24] = [
        VocoderParam::SynthLevel,
        VocoderParam::SynthDetune,
        VocoderParam::Osc1Wave,
        VocoderParam::Osc1Oct,
        VocoderParam::Osc1Fine,
        VocoderParam::Osc1Level,
        VocoderParam::Osc2Wave,
        VocoderParam::Osc2Oct,
        VocoderParam::Osc2Fine,
        VocoderParam::Osc2Level,
        VocoderParam::Osc3Wave,
        VocoderParam::Osc3Oct,
        VocoderParam::Osc3Fine,
        VocoderParam::Osc3Level,
        VocoderParam::Pwm,
        VocoderParam::Sync,
        VocoderParam::Ring,
        VocoderParam::Cutoff,
        VocoderParam::Resonance,
        VocoderParam::EnvAmount,
        VocoderParam::EnvDecay,
        VocoderParam::SynthAttack,
        VocoderParam::SynthRelease,
        VocoderParam::Drift,
    ];

    /// The oscillator (0..3) a knob belongs to, and which of its four (wave, oct, fine,
    /// level).
    fn osc_slot(self) -> Option<(usize, usize)> {
        use VocoderParam as P;
        Some(match self {
            P::Osc1Wave => (0, 0),
            P::Osc1Oct => (0, 1),
            P::Osc1Fine => (0, 2),
            P::Osc1Level => (0, 3),
            P::Osc2Wave => (1, 0),
            P::Osc2Oct => (1, 1),
            P::Osc2Fine => (1, 2),
            P::Osc2Level => (1, 3),
            P::Osc3Wave => (2, 0),
            P::Osc3Oct => (2, 1),
            P::Osc3Fine => (2, 2),
            P::Osc3Level => (2, 3),
            _ => return None,
        })
    }

    /// The name the socket, the bindings and the log use.
    pub fn name(self) -> &'static str {
        match self {
            VocoderParam::Bands => "bands",
            VocoderParam::Shift => "shift",
            VocoderParam::Attack => "attack",
            VocoderParam::Release => "release",
            VocoderParam::Mix => "mix",
            VocoderParam::Noise => "noise",
            VocoderParam::Volume => "volume",
            VocoderParam::Vowel => "vowel",
            VocoderParam::VowelMix => "vowel_mix",
            VocoderParam::SynthLevel => "synth_level",
            VocoderParam::SynthDetune => "synth_detune",
            VocoderParam::Osc1Wave => "osc1_wave",
            VocoderParam::Osc1Oct => "osc1_oct",
            VocoderParam::Osc1Fine => "osc1_fine",
            VocoderParam::Osc1Level => "osc1_level",
            VocoderParam::Osc2Wave => "osc2_wave",
            VocoderParam::Osc2Oct => "osc2_oct",
            VocoderParam::Osc2Fine => "osc2_fine",
            VocoderParam::Osc2Level => "osc2_level",
            VocoderParam::Osc3Wave => "osc3_wave",
            VocoderParam::Osc3Oct => "osc3_oct",
            VocoderParam::Osc3Fine => "osc3_fine",
            VocoderParam::Osc3Level => "osc3_level",
            VocoderParam::Pwm => "pwm",
            VocoderParam::Sync => "sync",
            VocoderParam::Ring => "ring",
            VocoderParam::Cutoff => "cutoff",
            VocoderParam::Resonance => "resonance",
            VocoderParam::EnvAmount => "env_amount",
            VocoderParam::EnvDecay => "env_decay",
            VocoderParam::SynthAttack => "synth_attack",
            VocoderParam::SynthRelease => "synth_release",
            VocoderParam::Drift => "drift",
        }
    }

    /// By name (a few aliases: `formant` for shift, `dry_wet`, `unvoiced`).
    pub fn parse(s: &str) -> Option<VocoderParam> {
        Some(
            match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "bands" | "band_count" => VocoderParam::Bands,
                "shift" | "formant" | "formant_shift" => VocoderParam::Shift,
                "attack" | "attack_ms" => VocoderParam::Attack,
                "release" | "release_ms" => VocoderParam::Release,
                "mix" | "dry_wet" | "wet" => VocoderParam::Mix,
                "noise" | "unvoiced" => VocoderParam::Noise,
                "volume" | "level" | "fader" => VocoderParam::Volume,
                "vowel" => VocoderParam::Vowel,
                "vowel_mix" | "vowel_amount" => VocoderParam::VowelMix,
                "synth_level" => VocoderParam::SynthLevel,
                "synth_detune" | "detune" | "spread" => VocoderParam::SynthDetune,
                "pwm" | "pulse_width" => VocoderParam::Pwm,
                "sync" | "hard_sync" => VocoderParam::Sync,
                "ring" | "ring_mod" => VocoderParam::Ring,
                "cutoff" | "filter" => VocoderParam::Cutoff,
                "resonance" | "res" | "q" => VocoderParam::Resonance,
                "env_amount" | "env" => VocoderParam::EnvAmount,
                "env_decay" | "decay" => VocoderParam::EnvDecay,
                "synth_attack" => VocoderParam::SynthAttack,
                "synth_release" => VocoderParam::SynthRelease,
                "drift" => VocoderParam::Drift,
                other => {
                    return VocoderParam::ALL
                        .into_iter()
                        .find(|p| p.osc_slot().is_some() && p.name() == other)
                }
            },
        )
    }

    /// The range, in its own units.
    pub fn range(self) -> (f64, f64) {
        match self {
            VocoderParam::Bands => (MIN_BANDS as f64, MAX_BANDS as f64),
            VocoderParam::Shift => (-12.0, 12.0),
            VocoderParam::Attack => (0.5, 200.0),
            VocoderParam::Release => (5.0, 1000.0),
            VocoderParam::Vowel => (0.0, 4.0),
            VocoderParam::SynthDetune => (0.0, 50.0),
            VocoderParam::Pwm => (0.05, 0.5),
            VocoderParam::Cutoff => (30.0, 18_000.0),
            VocoderParam::EnvAmount => (-1.0, 1.0),
            VocoderParam::EnvDecay => (5.0, 3000.0),
            VocoderParam::SynthAttack => (1.0, 2000.0),
            VocoderParam::SynthRelease => (5.0, 4000.0),
            p => match p.osc_slot() {
                Some((_, 0)) => (0.0, 4.0),
                Some((_, 1)) => (-3.0, 3.0),
                Some((_, 2)) => (-100.0, 100.0),
                _ => (0.0, 1.0),
            },
        }
    }

    /// Stepped: whole numbers only (bands, waves, octaves, sync).
    pub fn is_stepped(self) -> bool {
        matches!(self, VocoderParam::Bands | VocoderParam::Sync)
            || matches!(self.osc_slot(), Some((_, 0 | 1)))
    }

    /// Logarithmic travel on a knob (times, the cutoff).
    fn is_log(self) -> bool {
        matches!(
            self,
            VocoderParam::Attack
                | VocoderParam::Release
                | VocoderParam::Cutoff
                | VocoderParam::EnvDecay
                | VocoderParam::SynthAttack
                | VocoderParam::SynthRelease
        )
    }

    /// A value given as words: a vowel (a e i o u), a wave (saw, square, triangle, sine,
    /// noise), on / off for sync. None when `t` is not one of them for this knob.
    pub fn value_of_text(self, t: &str) -> Option<f64> {
        let t = t.trim().to_ascii_lowercase();
        if self == VocoderParam::Vowel {
            return ["a", "e", "i", "o", "u"].iter().position(|x| *x == t).map(|k| k as f64);
        }
        if matches!(self.osc_slot(), Some((_, 0))) {
            return super::eurosynth::Wave::parse(&t).map(|w| w.index());
        }
        if self == VocoderParam::Sync {
            return match t.as_str() {
                "on" | "true" | "yes" => Some(1.0),
                "off" | "false" | "no" => Some(0.0),
                _ => None,
            };
        }
        None
    }

    /// A knob's 0..1 travel to the param's units: the times are logarithmic, bands whole.
    pub fn from_knob(self, v: f64) -> f64 {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (lo, hi) = self.range();
        if self.is_log() {
            lo * libm::pow(hi / lo, v)
        } else if self.is_stepped() {
            libm::round(lo + v * (hi - lo))
        } else {
            lo + v * (hi - lo)
        }
    }

    /// The inverse of [`from_knob`](Self::from_knob).
    pub fn to_knob(self, x: f64) -> f64 {
        let (lo, hi) = self.range();
        let x = x.clamp(lo, hi);
        if self.is_log() {
            libm::log(x / lo) / libm::log(hi / lo)
        } else {
            (x - lo) / (hi - lo)
        }
    }

    /// Continuous knobs fold in the action log; the band count is a step.
    pub fn is_continuous(self) -> bool {
        !self.is_stepped()
    }
}

/// Everything the vocoder's knobs say. The sources are the host's.
#[derive(Clone, Debug, PartialEq)]
pub struct VocoderSettings {
    pub bands: usize,
    pub shift: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub mix: f64,
    pub noise: f64,
    pub volume: f64,
    pub vowel: f64,
    pub vowel_mix: f64,
    pub synth_level: f64,
    pub synth_detune: f64,
    /// The synth voice's other knobs.
    pub synth: super::eurosynth::SynthPatch,
    /// Bypass: the carrier plays on `V` as it is, the modulator left out (the synth as a jam
    /// synth with a modulator still picked).
    pub bypass: bool,
    /// Freeze: the band envelopes stop following the modulator and hold where they are, so the
    /// carrier keeps the shape (a held vowel) with the modulator gone.
    pub freeze: bool,
    /// The crossfader morphs the vocoder's output (carrier, vocoded, modulator, left to right)
    /// instead of crossfading the decks; `mix` is not used while it does.
    pub xfade: bool,
    /// The vocoder runs (off: silent as a strip, a bypass as an insert).
    pub on: bool,
    /// An insert: the vocoded signal replaces the carrier's own strip signal (before its
    /// fader) instead of playing on the vocoder's strip.
    pub insert: bool,
    pub mute: bool,
    pub pfl: bool,
}

impl Default for VocoderSettings {
    fn default() -> Self {
        VocoderSettings {
            bands: 16,
            shift: 0.0,
            attack_ms: 4.0,
            release_ms: 70.0,
            mix: 1.0,
            noise: 0.3,
            volume: 0.8,
            vowel: 0.0,
            vowel_mix: 0.0,
            synth_level: 0.7,
            synth_detune: 12.0,
            synth: super::eurosynth::SynthPatch::default(),
            bypass: false,
            freeze: false,
            xfade: false,
            on: false,
            insert: false,
            mute: false,
            pfl: false,
        }
    }
}

impl VocoderSettings {
    pub fn get(&self, p: VocoderParam) -> f64 {
        match p {
            VocoderParam::Bands => self.bands as f64,
            VocoderParam::Shift => self.shift,
            VocoderParam::Attack => self.attack_ms,
            VocoderParam::Release => self.release_ms,
            VocoderParam::Mix => self.mix,
            VocoderParam::Noise => self.noise,
            VocoderParam::Volume => self.volume,
            VocoderParam::Vowel => self.vowel,
            VocoderParam::VowelMix => self.vowel_mix,
            VocoderParam::SynthLevel => self.synth_level,
            VocoderParam::SynthDetune => self.synth_detune,
            VocoderParam::Pwm => self.synth.pwm,
            VocoderParam::Sync => self.synth.sync as u8 as f64,
            VocoderParam::Ring => self.synth.ring,
            VocoderParam::Cutoff => self.synth.cutoff,
            VocoderParam::Resonance => self.synth.resonance,
            VocoderParam::EnvAmount => self.synth.env_amount,
            VocoderParam::EnvDecay => self.synth.env_decay_ms,
            VocoderParam::SynthAttack => self.synth.attack_ms,
            VocoderParam::SynthRelease => self.synth.release_ms,
            VocoderParam::Drift => self.synth.drift,
            p => {
                let (o, k) = p.osc_slot().expect("an oscillator knob");
                let x = &self.synth.osc[o];
                match k {
                    0 => x.wave.index(),
                    1 => x.octave as f64,
                    2 => x.fine,
                    _ => x.level,
                }
            }
        }
    }

    /// Set `p` (clamped to its range; bands rounded). Returns the value kept, or None for a
    /// value that is not a number.
    pub fn set(&mut self, p: VocoderParam, v: f64) -> Option<f64> {
        if !v.is_finite() {
            return None;
        }
        let (lo, hi) = p.range();
        let v = v.clamp(lo, hi);
        let v = if p.is_stepped() { libm::round(v) } else { v };
        match p {
            VocoderParam::Bands => self.bands = libm::round(v) as usize,
            VocoderParam::Shift => self.shift = v,
            VocoderParam::Attack => self.attack_ms = v,
            VocoderParam::Release => self.release_ms = v,
            VocoderParam::Mix => self.mix = v,
            VocoderParam::Noise => self.noise = v,
            VocoderParam::Volume => self.volume = v,
            VocoderParam::Vowel => self.vowel = v,
            VocoderParam::VowelMix => self.vowel_mix = v,
            VocoderParam::SynthLevel => self.synth_level = v,
            VocoderParam::SynthDetune => self.synth_detune = v,
            VocoderParam::Pwm => self.synth.pwm = v,
            VocoderParam::Sync => self.synth.sync = v >= 0.5,
            VocoderParam::Ring => self.synth.ring = v,
            VocoderParam::Cutoff => self.synth.cutoff = v,
            VocoderParam::Resonance => self.synth.resonance = v,
            VocoderParam::EnvAmount => self.synth.env_amount = v,
            VocoderParam::EnvDecay => self.synth.env_decay_ms = v,
            VocoderParam::SynthAttack => self.synth.attack_ms = v,
            VocoderParam::SynthRelease => self.synth.release_ms = v,
            VocoderParam::Drift => self.synth.drift = v,
            p => {
                let (o, k) = p.osc_slot().expect("an oscillator knob");
                let x = &mut self.synth.osc[o];
                match k {
                    0 => x.wave = super::eurosynth::Wave::from_index(v),
                    1 => x.octave = v as i32,
                    2 => x.fine = v,
                    _ => x.level = v,
                }
            }
        }
        Some(self.get(p))
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for p in VocoderParam::ALL {
            let v = self.get(p);
            o.set(p.name(), JsonValue::from(libm::round(v * 1000.0) / 1000.0));
        }
        o.set("on", self.on.into());
        o.set("insert", self.insert.into());
        o.set("bypass", self.bypass.into());
        o.set("freeze", self.freeze.into());
        o.set("xfade", self.xfade.into());
        o.set("mute", self.mute.into());
        o.set("pfl", self.pfl.into());
        JsonValue::Object(o)
    }
}

// MARK: the vocoder

/// White noise, xorshift32: deterministic, so a render is repeatable.
#[derive(Clone, Copy, Debug)]
struct Noise(u32);

impl Noise {
    #[inline]
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// One-pole smoothing coefficient for a time constant of `ms`.
fn coef(ms: f64, sr: f64) -> f32 {
    let n = (ms.max(0.01) / 1000.0) * sr;
    libm::exp(-1.0 / n.max(1.0)) as f32
}

/// The channel vocoder.
pub struct Vocoder {
    sample_rate: f64,
    bands: usize,
    shift: f64,
    analysis: [Band; MAX_BANDS],
    synthesis: [Band; MAX_BANDS],
    env: [f32; MAX_BANDS],
    attack: f32,
    release: f32,
    noise_amt: f32,
    noise: Noise,
    last_noise: f32,
    /// The modulator's top quarter of bands against all of them (the "unvoiced" detector).
    sibilance: f32,
    makeup: f32,
    /// The envelopes held ([`VocoderSettings::freeze`]).
    frozen: bool,
}

impl Vocoder {
    pub fn new(sample_rate: f64, s: &VocoderSettings) -> Vocoder {
        let mut v = Vocoder {
            sample_rate,
            bands: 0,
            shift: f64::NAN,
            analysis: [Band::default(); MAX_BANDS],
            synthesis: [Band::default(); MAX_BANDS],
            env: [0.0; MAX_BANDS],
            attack: 0.0,
            release: 0.0,
            noise_amt: 0.0,
            noise: Noise(0x1234_5678),
            last_noise: 0.0,
            sibilance: 0.0,
            makeup: 1.0,
            frozen: false,
        };
        v.configure(s);
        v
    }

    /// Take the knobs. A new band count rebuilds the banks (their state starts over); a new
    /// shift retunes the carrier's bank in place. Real-time safe.
    pub fn configure(&mut self, s: &VocoderSettings) {
        let n = s.bands.clamp(MIN_BANDS, MAX_BANDS);
        let sr = self.sample_rate;
        let q = band_q(n, sr);
        if n != self.bands {
            self.bands = n;
            for k in 0..n {
                let fc = band_center(k, n, sr);
                self.analysis[k] = Band::new(fc, q, sr);
            }
            self.env = [0.0; MAX_BANDS];
            self.shift = f64::NAN; // the carrier's bank follows
                                   // the sum of N narrow bands of a broadband carrier, each scaled by its band's envelope,
                                   // falls about N^0.8 below the carrier: bring it back (measured: a -12 dBFS noise
                                   // modulator on a saw comes out near the saw's level, 4..32 bands)
            self.makeup = (10.0 * libm::pow(n as f64, 0.82)) as f32;
        }
        if s.shift != self.shift {
            let fresh = self.shift.is_nan();
            self.shift = s.shift;
            let ratio = libm::pow(2.0, s.shift / 12.0);
            for k in 0..n {
                let fc = (band_center(k, n, sr) * ratio).clamp(30.0, 0.45 * sr);
                if fresh {
                    self.synthesis[k] = Band::new(fc, q, sr);
                } else {
                    self.synthesis[k].retune(fc, q, sr);
                }
            }
        }
        self.attack = coef(s.attack_ms, sr);
        self.release = coef(s.release_ms, sr);
        self.noise_amt = s.noise.clamp(0.0, 1.0) as f32;
        self.frozen = s.freeze;
    }

    /// The band envelopes are held.
    pub fn frozen(&self) -> bool {
        self.frozen
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Each band's envelope now (the modulator's level in that band).
    pub fn envelopes(&self) -> &[f32] {
        &self.env[..self.bands]
    }

    /// The unvoiced detector, 0..1.
    pub fn sibilance(&self) -> f32 {
        self.sibilance
    }

    /// Start over: the filters and envelopes empty.
    pub fn reset(&mut self) {
        for k in 0..MAX_BANDS {
            self.analysis[k].reset();
            self.synthesis[k].reset();
        }
        self.env = [0.0; MAX_BANDS];
        self.sibilance = 0.0;
    }

    /// Vocode one block: `out[i]` = the carrier shaped by the modulator. Mono in, mono out.
    pub fn process(&mut self, modulator: &[f32], carrier: &[f32], out: &mut [f32]) {
        let n = out.len().min(modulator.len()).min(carrier.len());
        let bands = self.bands;
        let top = bands - bands / 4;
        let (att, rel) = (self.attack, self.release);
        let frozen = self.frozen;
        for i in 0..n {
            let m = modulator[i];
            let mut total = 0f32;
            let mut high = 0f32;
            // frozen: the envelopes (and the unvoiced detector) hold; the analysis bank rests
            for k in (0..bands).filter(|_| !frozen) {
                let y = self.analysis[k].tick(m).abs();
                let e = self.env[k];
                let c = if y > e { att } else { rel };
                let e = y + c * (e - y);
                self.env[k] = e;
                total += e;
                if k >= top {
                    high += e;
                }
            }
            // unvoiced: how much of the modulator sits in its top bands, smoothed
            let s = if total > 1e-6 {
                (high / total).min(1.0)
            } else {
                0.0
            };
            if !frozen {
                self.sibilance = s + rel * (self.sibilance - s);
            }
            // noise, pre-emphasised (a first difference rises 6 dB an octave), into the carrier
            let w = self.noise.next();
            let hp = 0.5 * (w - self.last_noise);
            self.last_noise = w;
            let blend = self.noise_amt * (0.35 + 1.3 * self.sibilance).min(1.0);
            let x = carrier[i] + blend * hp;
            let mut y = 0f32;
            for k in 0..bands {
                y += self.synthesis[k].tick(x) * self.env[k];
            }
            // a lone loud band can go well past the carrier: saturate softly toward ±1
            // (about -1 dB at 0.5, transparent below)
            let y = y * self.makeup;
            out[i] = y / libm::sqrtf(1.0 + y * y);
        }
    }
}

// MARK: the formant filter

/// Formant frequencies (F1, F2, F3) and gains of the vowels a e i o u, a male voice's
/// (after Peterson & Barney's averages, rounded).
pub const VOWELS: [(&str, [f64; 3], [f64; 3]); 5] = [
    ("a", [730.0, 1090.0, 2440.0], [1.0, 0.50, 0.25]),
    ("e", [530.0, 1840.0, 2480.0], [1.0, 0.40, 0.25]),
    ("i", [270.0, 2290.0, 3010.0], [1.0, 0.25, 0.20]),
    ("o", [570.0, 840.0, 2410.0], [1.0, 0.45, 0.15]),
    ("u", [300.0, 870.0, 2240.0], [1.0, 0.30, 0.10]),
];

/// The formants of vowel position `v` (0 a .. 4 u), interpolated between neighbours.
pub fn vowel_formants(v: f64) -> ([f64; 3], [f64; 3]) {
    let v = if v.is_finite() {
        v.clamp(0.0, 4.0)
    } else {
        0.0
    };
    let i = (libm::floor(v) as usize).min(3);
    let t = v - i as f64;
    let (_, fa, ga) = VOWELS[i];
    let (_, fb, gb) = VOWELS[i + 1];
    let mut f = [0.0; 3];
    let mut g = [0.0; 3];
    for k in 0..3 {
        // frequency in log, gain linear
        f[k] = fa[k] * libm::pow(fb[k] / fa[k], t);
        g[k] = ga[k] + (gb[k] - ga[k]) * t;
    }
    (f, g)
}

/// The nearest vowel's letter.
pub fn vowel_name(v: f64) -> &'static str {
    VOWELS[(libm::round(v.clamp(0.0, 4.0)) as usize).min(4)].0
}

/// Three parallel band-passes at a vowel's formants: a talk-box on whatever goes through it.
pub struct FormantFilter {
    sample_rate: f64,
    vowel: f64,
    filters: [Biquad; 3],
    gains: [f32; 3],
}

impl FormantFilter {
    pub fn new(sample_rate: f64, vowel: f64) -> FormantFilter {
        let mut f = FormantFilter {
            sample_rate,
            vowel: f64::NAN,
            filters: [Biquad::default(); 3],
            gains: [0.0; 3],
        };
        f.set_vowel(vowel);
        f
    }

    /// Move to vowel position `v` (0 a .. 4 u); the filters keep their state.
    pub fn set_vowel(&mut self, v: f64) {
        if v == self.vowel {
            return;
        }
        self.vowel = v;
        let (fs, gs) = vowel_formants(v);
        for k in 0..3 {
            // formants are narrow: Q ~ f / 80 Hz bandwidth, at least 4
            let q = (fs[k] / 90.0).max(4.0);
            self.filters[k].retune(Biquad::bandpass(fs[k], q, self.sample_rate));
            self.gains[k] = gs[k] as f32;
        }
    }

    pub fn vowel(&self) -> f64 {
        self.vowel
    }

    /// Filter `x` in place, `amount` of it (0 dry .. 1 only the formants).
    pub fn process(&mut self, x: &mut [f32], amount: f32) {
        if amount <= 0.0 {
            return;
        }
        let a = amount.min(1.0);
        for s in x.iter_mut() {
            let mut y = 0f32;
            for k in 0..3 {
                y += self.filters[k].tick(*s) * self.gains[k];
            }
            // the three narrow peaks keep a fraction of a broadband signal's power: make up ~9 dB
            *s = *s * (1.0 - a) + y * 2.8 * a;
        }
    }
}

// MARK: the internal synth carrier

/// The carrier synth: the Eurorack-style voice of [`super::eurosynth`] (its default patch is
/// the old two detuned PolyBLEP saws).
pub type CarrierSynth = super::eurosynth::EuroSynth;
pub use super::eurosynth::note_hz;

// MARK: sources, as words

/// What shapes the sound.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum ModulatorSource {
    #[default]
    None,
    /// A live input strip, by label (a mic).
    Input(String),
    /// A deck, by name, tapped live (pre-fader): whatever is loaded on it drives the vocoder.
    Deck(String),
    /// A loop playing inside the vocoder: a row clip, a library loop or a capture.
    Loop(LoopSource),
    /// [`CarrierSynth`] (its notes), as the modulator: a synth line shaping a drum loop.
    Synth,
}

/// Where a loop modulator came from.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopSource {
    /// "clip", "library" or "capture".
    pub kind: String,
    /// The clip's or the library entry's id; the input's label for a capture.
    pub id: String,
    pub title: String,
    /// Its tempo (it follows the master's), 0 when unknown (played as is).
    pub bpm: f64,
    pub beats: f64,
}

/// What gets shaped.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CarrierSource {
    #[default]
    None,
    /// An input strip, by label (the TD-3).
    Input(String),
    /// A deck, by name.
    Deck(String),
    /// [`CarrierSynth`].
    Synth,
    /// A loop playing inside the vocoder (a row clip, a library loop, a capture or a file),
    /// following the master tempo as a loop modulator does.
    Loop(LoopSource),
}

impl LoopSource {
    fn write_json(&self, o: &mut JsonObject) {
        o.set("kind", self.kind.clone().into());
        o.set("id", self.id.clone().into());
        o.set("title", self.title.clone().into());
        o.set("bpm", JsonValue::from(self.bpm));
        o.set("beats", JsonValue::from(self.beats));
    }
}

impl ModulatorSource {
    pub fn describe(&self) -> String {
        match self {
            ModulatorSource::None => "none".into(),
            ModulatorSource::Input(l) => alloc::format!("input {l}"),
            ModulatorSource::Deck(d) => alloc::format!("deck {d}"),
            ModulatorSource::Loop(s) => alloc::format!("{} {}", s.kind, s.title),
            ModulatorSource::Synth => "synth".into(),
        }
    }
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        match self {
            ModulatorSource::None => o.set("kind", "none".into()),
            ModulatorSource::Input(l) => {
                o.set("kind", "input".into());
                o.set("input", l.clone().into());
            }
            ModulatorSource::Deck(d) => {
                o.set("kind", "deck".into());
                o.set("deck", d.clone().into());
            }
            ModulatorSource::Loop(s) => s.write_json(&mut o),
            ModulatorSource::Synth => o.set("kind", "synth".into()),
        }
        JsonValue::Object(o)
    }
}

impl CarrierSource {
    pub fn describe(&self) -> String {
        match self {
            CarrierSource::None => "none".into(),
            CarrierSource::Input(l) => alloc::format!("input {l}"),
            CarrierSource::Deck(d) => alloc::format!("deck {d}"),
            CarrierSource::Synth => "synth".into(),
            CarrierSource::Loop(s) => alloc::format!("{} {}", s.kind, s.title),
        }
    }
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        match self {
            CarrierSource::None => o.set("kind", "none".into()),
            CarrierSource::Input(l) => {
                o.set("kind", "input".into());
                o.set("input", l.clone().into());
            }
            CarrierSource::Deck(d) => {
                o.set("kind", "deck".into());
                o.set("deck", d.clone().into());
            }
            CarrierSource::Synth => o.set("kind", "synth".into()),
            CarrierSource::Loop(s) => s.write_json(&mut o),
        }
        JsonValue::Object(o)
    }
}

/// The band centres of an `n`-band bank, rounded to Hz (for a view).
pub fn band_centers(n: usize, sample_rate: f64) -> Vec<f64> {
    (0..n)
        .map(|k| libm::round(band_center(k, n, sample_rate)))
        .collect()
}

/// "C4"-style name of a MIDI note.
pub fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let mut s = NAMES[(n % 12) as usize].to_string();
    s.push_str(&alloc::format!("{}", n as i32 / 12 - 1));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn sine(hz: f64, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * libm::sin(2.0 * PI * hz * i as f64 / SR) as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    fn saw(n: usize) -> Vec<f32> {
        let mut s = CarrierSynth::new(SR);
        s.note_on(45);
        let mut out = alloc::vec![0.0; n];
        s.render(&mut out);
        out
    }

    #[test]
    fn bands_are_log_spaced_and_within_nyquist() {
        for n in [MIN_BANDS, 16, MAX_BANDS] {
            let c = band_centers(n, 44_100.0);
            assert_eq!(c.len(), n);
            assert!((c[0] - LOW_HZ).abs() < 1.0);
            assert!(*c.last().unwrap() <= 0.42 * 44_100.0);
            let r0 = c[1] / c[0];
            let r1 = c[n - 1] / c[n - 2];
            assert!((r0 - r1).abs() < 0.01, "{r0} {r1}");
        }
        assert!(band_q(32, SR) > band_q(8, SR));
    }

    #[test]
    fn a_silent_modulator_silences_the_carrier() {
        let s = VocoderSettings::default();
        let mut v = Vocoder::new(SR, &VocoderSettings { noise: 0.0, ..s });
        let car = saw(9600);
        let m = alloc::vec![0.0; 9600];
        let mut out = alloc::vec![0.0; 9600];
        v.process(&m, &car, &mut out);
        assert!(rms(&out) < 1e-6, "{}", rms(&out));
    }

    #[test]
    fn the_modulators_spectrum_lands_on_the_carrier() {
        // a low tone opens the low bands; a high tone the high ones
        let s = VocoderSettings {
            noise: 0.0,
            ..VocoderSettings::default()
        };
        let n = 24_000;
        let car = saw(n);
        for (hz, low_wins) in [(200.0, true), (4000.0, false)] {
            let mut v = Vocoder::new(SR, &s);
            let m = sine(hz, n, 0.5);
            let mut out = alloc::vec![0.0; n];
            v.process(&m, &car, &mut out);
            let e = v.envelopes();
            let half = e.len() / 2;
            let lo: f32 = e[..half].iter().sum();
            let hi: f32 = e[half..].iter().sum();
            assert_eq!(lo > hi, low_wins, "{hz} Hz: lo {lo} hi {hi}");
            let level = rms(&out[n / 2..]);
            assert!(level > 0.02 && level < 1.0, "{hz} Hz: output rms {level}");
        }
    }

    #[test]
    fn output_level_is_in_the_carriers_neighbourhood() {
        // a broadband modulator (noise) at -12 dBFS on a saw: the vocoded level should be
        // within ~12 dB of the carrier's, at any band count
        let n = 48_000;
        let car = saw(n);
        let mut src = Noise(99);
        let m: Vec<f32> = (0..n).map(|_| 0.25 * src.next()).collect();
        for bands in [MIN_BANDS, 8, 16, 32] {
            let s = VocoderSettings {
                bands,
                noise: 0.0,
                ..VocoderSettings::default()
            };
            let mut v = Vocoder::new(SR, &s);
            let mut out = alloc::vec![0.0; n];
            v.process(&m, &car, &mut out);
            let ratio = rms(&out[n / 2..]) / rms(&car[n / 2..]);
            assert!(ratio > 0.3 && ratio < 2.0, "{bands} bands: ratio {ratio}");
        }
    }

    #[test]
    fn noise_fills_in_for_a_silent_carrier() {
        let n = 24_000;
        let car = alloc::vec![0.0; n];
        let m = sine(5000.0, n, 0.5);
        let mut out = alloc::vec![0.0; n];
        let mut v = Vocoder::new(
            SR,
            &VocoderSettings {
                noise: 1.0,
                ..Default::default()
            },
        );
        v.process(&m, &car, &mut out);
        assert!(rms(&out[n / 2..]) > 1e-3);
        assert!(v.sibilance() > 0.5, "{}", v.sibilance());
        let mut v = Vocoder::new(
            SR,
            &VocoderSettings {
                noise: 0.0,
                ..Default::default()
            },
        );
        v.process(&m, &car, &mut out);
        assert_eq!(rms(&out), 0.0);
    }

    #[test]
    fn shift_and_band_count_reconfigure_in_place() {
        let mut s = VocoderSettings::default();
        let mut v = Vocoder::new(SR, &s);
        assert_eq!(v.bands(), 16);
        s.bands = 99;
        v.configure(&s);
        assert_eq!(v.bands(), MAX_BANDS);
        s.shift = 7.0;
        v.configure(&s);
        let car = saw(4800);
        let m = sine(800.0, 4800, 0.5);
        let mut out = alloc::vec![0.0; 4800];
        v.process(&m, &car, &mut out);
        assert!(out.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn params_round_trip_through_names_and_knobs() {
        for p in VocoderParam::ALL {
            assert_eq!(VocoderParam::parse(p.name()), Some(p));
            for k in [0.0, 0.3, 1.0] {
                let x = p.from_knob(k);
                let (lo, hi) = p.range();
                assert!(x >= lo && x <= hi);
                if !p.is_stepped() {
                    assert!((p.to_knob(x) - k).abs() < 1e-9, "{p:?} {k}");
                }
            }
        }
        assert_eq!(VocoderParam::parse("formant"), Some(VocoderParam::Shift));
        let mut s = VocoderSettings::default();
        assert_eq!(s.set(VocoderParam::Attack, 1e9), Some(200.0));
        assert_eq!(s.set(VocoderParam::Bands, 7.6), Some(8.0));
        assert_eq!(s.set(VocoderParam::Mix, f64::NAN), None);
        let j = s.to_json();
        assert_eq!(j.get("bands").and_then(JsonValue::as_f64), Some(8.0));
        assert_eq!(j.get("on").and_then(JsonValue::as_bool), Some(false));
    }

    #[test]
    fn vowels_morph_between_neighbours() {
        let (f0, _) = vowel_formants(0.0);
        assert_eq!(f0, VOWELS[0].1);
        let (f, _) = vowel_formants(0.5);
        assert!(f[1] > VOWELS[0].1[1] && f[1] < VOWELS[1].1[1]);
        assert_eq!(vowel_name(2.2), "i");
        assert!((vowel_formants(9.0).0[1] - VOWELS[4].1[1]).abs() < 1e-9);
    }

    #[test]
    fn the_formant_filter_favours_its_formants() {
        // vowel "i": F1 270, F2 2290; a tone at 1000 Hz (between) loses against one at 270
        let n = 24_000;
        let mut f = FormantFilter::new(SR, 2.0);
        let mut on = sine(270.0, n, 0.5);
        f.process(&mut on, 1.0);
        let mut f = FormantFilter::new(SR, 2.0);
        let mut off = sine(1000.0, n, 0.5);
        f.process(&mut off, 1.0);
        assert!(rms(&on[n / 2..]) > 4.0 * rms(&off[n / 2..]));
        // amount 0 leaves it alone
        let mut x = sine(1000.0, 100, 0.5);
        let y = x.clone();
        f.process(&mut x, 0.0);
        assert_eq!(x, y);
    }

    #[test]
    fn the_synth_plays_held_notes_and_releases_cleanly() {
        let mut s = CarrierSynth::new(SR);
        let mut out = alloc::vec![0.0; 4800];
        s.render(&mut out);
        assert!(out.iter().all(|x| *x == 0.0));
        s.note_on(57);
        s.note_on(60);
        assert_eq!(s.held(), alloc::vec![57, 60]);
        s.render(&mut out);
        assert!(rms(&out) > 0.05);
        s.note_off(57);
        s.note_off(60);
        let mut tail = alloc::vec![0.0; 4800];
        s.render(&mut tail);
        // the gate ramps down over 5 ms: no jump at the start of the block
        assert!((tail[0] - out[4799]).abs() < 0.2);
        let mut quiet = alloc::vec![0.0; 480];
        s.render(&mut quiet);
        assert!(quiet.iter().all(|x| *x == 0.0));
        assert!(s.held().is_empty());
        for k in 0..20u8 {
            s.note_on(40 + k);
        }
        assert_eq!(s.held().len(), SYNTH_VOICES);
        assert_eq!(note_name(60), "C4");
        assert!((note_hz(69) - 440.0).abs() < 1e-9);
    }

    #[test]
    fn freeze_holds_the_band_envelopes() {
        use alloc::vec;
        let mut st = VocoderSettings::default();
        let mut v = Vocoder::new(SR, &st);
        let n = 4800;
        let tone: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 440.0 * i as f64 / SR).sin() as f32 * 0.5)
            .collect();
        let saw: Vec<f32> = (0..n)
            .map(|i| (2.0 * ((110.0 * i as f64 / SR) % 1.0) - 1.0) as f32 * 0.5)
            .collect();
        let mut out = vec![0f32; n];
        v.process(&tone, &saw, &mut out);
        let held: Vec<f32> = v.envelopes().to_vec();
        assert!(held.iter().any(|e| *e > 0.01));
        st.freeze = true;
        v.configure(&st);
        let silence = vec![0f32; n];
        v.process(&silence, &saw, &mut out);
        assert_eq!(v.envelopes(), &held[..], "frozen: the envelopes hold");
        let rms = (out.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt();
        assert!(rms > 0.01, "the carrier keeps the shape: {rms}");
        st.freeze = false;
        v.configure(&st);
        for _ in 0..4 {
            v.process(&silence, &saw, &mut out);
        }
        assert!(v.envelopes().iter().all(|e| *e < held.iter().cloned().fold(0.0, f32::max) * 0.1));
    }
}
