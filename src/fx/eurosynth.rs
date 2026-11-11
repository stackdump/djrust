//! The vocoder's synth carrier as a small Eurorack-style voice (beyond Swift; docs/VOCODER.md,
//! "The synth"): up to [`SYNTH_VOICES`](super::vocoder::SYNTH_VOICES) notes, each three
//! oscillators into a resonant low-pass with a decay envelope, through an attack / release gate.
//!
//! - **Oscillators** 1-3: a wave each ([`Wave`]: saw, square with pulse width, triangle, sine,
//!   noise), octave, fine tune and level. Osc 1 and 2 are spread apart by the detune (osc 1 up
//!   half of it, osc 2 down half); osc 2 can be **hard-synced** to osc 1 (its phase reset each
//!   time osc 1 wraps) and **ring-modulated** by it (`ring` crossfades osc 2 into osc 1 × osc 2).
//! - **Filter**: a resonant state-variable low-pass (Simper's trapezoidal SVF) per voice:
//!   cutoff, resonance, and the decay envelope's amount (±5 octaves). Wide open, with no
//!   resonance and no envelope, it is out of the signal.
//! - **Drift**: slow wobble of each oscillator's pitch (up to ±15 cents) and of the cutoff, at
//!   rates of their own per voice, as analog oscillators wander.
//!
//! The default [`SynthPatch`] is the carrier this replaced: two saws 12 cents apart, the
//! filter out, 5 ms gate ramps. Real-time safe: no allocation, the pitch and filter
//! coefficients made every [`CONTROL_FRAMES`] frames.

use alloc::vec::Vec;

/// Frames between pitch / filter updates (drift, the envelope's sweep).
pub const CONTROL_FRAMES: u32 = 32;
/// Most a drift of 1 moves an oscillator, cents.
pub const DRIFT_CENTS: f64 = 15.0;
/// The cutoff at or above which (with no resonance and no envelope) the filter is left out, Hz.
pub const CUTOFF_OPEN: f64 = 17_999.0;

/// An oscillator's wave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Saw,
    /// A pulse, its width from [`SynthPatch::pwm`] (0.5: a square).
    Square,
    Triangle,
    Sine,
    Noise,
}

impl Wave {
    pub const ALL: [Wave; 5] = [Wave::Saw, Wave::Square, Wave::Triangle, Wave::Sine, Wave::Noise];

    pub fn from_index(v: f64) -> Wave {
        let k = libm::round(v.clamp(0.0, 4.0)) as usize;
        Wave::ALL[k.min(4)]
    }
    pub fn index(self) -> f64 {
        Wave::ALL.iter().position(|w| *w == self).unwrap_or(0) as f64
    }
    pub fn name(self) -> &'static str {
        match self {
            Wave::Saw => "saw",
            Wave::Square => "square",
            Wave::Triangle => "triangle",
            Wave::Sine => "sine",
            Wave::Noise => "noise",
        }
    }
    pub fn parse(s: &str) -> Option<Wave> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "saw" | "sawtooth" | "ramp" => Wave::Saw,
            "square" | "pulse" | "pwm" | "sq" => Wave::Square,
            "triangle" | "tri" => Wave::Triangle,
            "sine" | "sin" => Wave::Sine,
            "noise" | "white" => Wave::Noise,
            _ => return None,
        })
    }
}

/// One oscillator's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OscPatch {
    pub wave: Wave,
    /// Octaves, -3..3.
    pub octave: i32,
    /// Cents, -100..100.
    pub fine: f64,
    /// 0..1.
    pub level: f64,
}

/// Everything the synth's knobs say, but its level and the detune spread (those are the
/// vocoder's `synth_level` / `synth_detune`, kept from before).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SynthPatch {
    pub osc: [OscPatch; 3],
    /// The square's pulse width, 0.05..0.5.
    pub pwm: f64,
    /// Osc 2 hard-synced to osc 1.
    pub sync: bool,
    /// Osc 2 crossfaded into osc 1 × osc 2, 0..1.
    pub ring: f64,
    /// Hz, 30..18000.
    pub cutoff: f64,
    /// 0..1 (1: close to self-oscillation).
    pub resonance: f64,
    /// The decay envelope's pull on the cutoff, -1..1 (×5 octaves).
    pub env_amount: f64,
    /// The decay envelope's time, ms.
    pub env_decay_ms: f64,
    /// The gate's attack and release, ms.
    pub attack_ms: f64,
    pub release_ms: f64,
    /// 0..1.
    pub drift: f64,
}

impl Default for SynthPatch {
    fn default() -> Self {
        let osc = |wave, octave, level| OscPatch {
            wave,
            octave,
            fine: 0.0,
            level,
        };
        SynthPatch {
            osc: [
                osc(Wave::Saw, 0, 1.0),
                osc(Wave::Saw, 0, 1.0),
                osc(Wave::Sine, -1, 0.0),
            ],
            pwm: 0.5,
            sync: false,
            ring: 0.0,
            cutoff: 18_000.0,
            resonance: 0.0,
            env_amount: 0.0,
            env_decay_ms: 400.0,
            attack_ms: 5.0,
            release_ms: 5.0,
            drift: 0.0,
        }
    }
}

impl SynthPatch {
    /// The filter is in the signal.
    pub fn filter_on(&self) -> bool {
        self.cutoff < CUTOFF_OPEN || self.resonance > 0.0 || self.env_amount != 0.0
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: Option<u8>,
    /// Releasing: the gate ramps to 0, then the voice is free.
    held: bool,
    gate: f32,
    phase: [f64; 3],
    inc: [f64; 3],
    /// The filter's state, and its coefficients (a1, a2, a3).
    ic1: f32,
    ic2: f32,
    coef: (f32, f32, f32),
    /// The decay envelope, 1 at a note's start.
    fenv: f64,
    /// Frames to the next control update.
    ctr: u32,
    /// Drift LFO phases: the three oscillators and the cutoff.
    drift_ph: [f64; 4],
}

/// The synth itself.
pub struct EuroSynth {
    sample_rate: f64,
    voices: [Voice; super::vocoder::SYNTH_VOICES],
    patch: SynthPatch,
    spread_cents: f64,
    level: f32,
    attack_step: f32,
    release_step: f32,
    env_coef: f64,
    rng: u32,
}

#[inline]
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// A MIDI note's frequency.
pub fn note_hz(note: u8) -> f64 {
    440.0 * libm::pow(2.0, (note as f64 - 69.0) / 12.0)
}

impl EuroSynth {
    pub fn new(sample_rate: f64) -> EuroSynth {
        let mut s = EuroSynth {
            sample_rate,
            voices: [Voice::default(); super::vocoder::SYNTH_VOICES],
            patch: SynthPatch::default(),
            spread_cents: 12.0,
            level: 0.7,
            attack_step: 1.0,
            release_step: 1.0,
            env_coef: 0.0,
            rng: 0x2545_f491,
        };
        s.set_patch(&SynthPatch::default());
        s
    }

    pub fn set_level(&mut self, v: f64) {
        self.level = v.clamp(0.0, 1.0) as f32;
    }

    /// The spread between osc 1 and 2, cents (0..50).
    pub fn set_detune(&mut self, cents: f64) {
        self.spread_cents = cents.clamp(0.0, 50.0);
        self.retune_all();
    }

    pub fn set_patch(&mut self, p: &SynthPatch) {
        self.patch = *p;
        let sr = self.sample_rate;
        let step = |ms: f64| (1.0 / (ms.max(0.5) * 0.001 * sr)).min(1.0) as f32;
        self.attack_step = step(p.attack_ms);
        self.release_step = step(p.release_ms);
        self.env_coef = libm::exp(-1.0 / (p.env_decay_ms.max(1.0) * 0.001 * sr));
        self.retune_all();
    }

    pub fn patch(&self) -> &SynthPatch {
        &self.patch
    }

    fn retune_all(&mut self) {
        for k in 0..self.voices.len() {
            if self.voices[k].note.is_some() {
                self.update(k);
            }
        }
    }

    /// Voice `k`'s pitches and filter, with the drift where it is now.
    fn update(&mut self, k: usize) {
        let p = self.patch;
        let sr = self.sample_rate;
        let v = &mut self.voices[k];
        let Some(note) = v.note else { return };
        let hz = note_hz(note);
        let spread = [self.spread_cents * 0.5, -self.spread_cents * 0.5, 0.0];
        for o in 0..3 {
            let drift = if p.drift > 0.0 {
                p.drift * DRIFT_CENTS * libm::sin(core::f64::consts::TAU * v.drift_ph[o])
            } else {
                0.0
            };
            let cents = p.osc[o].octave as f64 * 1200.0 + p.osc[o].fine + spread[o] + drift;
            v.inc[o] = (hz * libm::pow(2.0, cents / 1200.0) / sr).min(0.49);
        }
        if p.filter_on() {
            let wobble = if p.drift > 0.0 {
                p.drift * 0.3 * libm::sin(core::f64::consts::TAU * v.drift_ph[3])
            } else {
                0.0
            };
            let octaves = p.env_amount * 5.0 * v.fenv + wobble;
            let fc = (p.cutoff * libm::pow(2.0, octaves)).clamp(20.0, 0.45 * sr);
            let g = libm::tan(core::f64::consts::PI * fc / sr);
            let kq = 2.0 - 1.95 * p.resonance.clamp(0.0, 1.0);
            let a1 = 1.0 / (1.0 + g * (g + kq));
            let a2 = g * a1;
            let a3 = g * a2;
            v.coef = (a1 as f32, a2 as f32, a3 as f32);
        }
        // the drift LFOs: slow, a rate of their own per voice and oscillator
        if p.drift > 0.0 {
            let dt = CONTROL_FRAMES as f64 / sr;
            for (o, ph) in v.drift_ph.iter_mut().enumerate() {
                let rate = 0.09 + 0.07 * o as f64 + 0.031 * k as f64;
                let p = *ph + rate * dt;
                *ph = p - libm::trunc(p); // `fract`, which needs std
            }
        }
        v.ctr = CONTROL_FRAMES;
    }

    /// Start `note` (again, if it is sounding). With every voice busy the quietest releasing
    /// one is taken, else the first.
    pub fn note_on(&mut self, note: u8) {
        let note = note.min(127);
        let slot = self
            .voices
            .iter()
            .position(|v| v.note == Some(note))
            .or_else(|| self.voices.iter().position(|v| v.note.is_none()))
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.held)
                    .min_by(|a, b| a.1.gate.total_cmp(&b.1.gate))
                    .map(|(i, _)| i)
            })
            .unwrap_or(0);
        let v = &mut self.voices[slot];
        if v.note != Some(note) {
            v.phase = [0.0, 0.37, 0.71];
            v.ic1 = 0.0;
            v.ic2 = 0.0;
            if v.note.is_some() {
                v.gate = 0.0;
            }
            // each voice wanders from its own place
            v.drift_ph = [
                0.11 * slot as f64,
                0.5 + 0.13 * slot as f64,
                0.25 + 0.17 * slot as f64,
                0.75 + 0.19 * slot as f64,
            ]
            .map(|x: f64| x - libm::trunc(x)); // `fract`, which needs std
        }
        v.note = Some(note);
        v.held = true;
        v.fenv = 1.0;
        self.update(slot);
    }

    pub fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut() {
            if v.note == Some(note) {
                v.held = false;
            }
        }
    }

    /// Release every note.
    pub fn all_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
        }
    }

    /// The notes held now.
    pub fn held(&self) -> Vec<u8> {
        self.voices
            .iter()
            .filter(|v| v.held)
            .filter_map(|v| v.note)
            .collect()
    }

    /// Notes still sounding (held, or in their release).
    pub fn sounding(&self) -> usize {
        self.voices.iter().filter(|v| v.note.is_some()).count()
    }

    #[inline]
    fn noise(rng: &mut u32) -> f64 {
        let mut x = *rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *rng = x;
        (x as f64 / u32::MAX as f64) * 2.0 - 1.0
    }

    #[inline]
    fn osc(wave: Wave, p: f64, dt: f64, pw: f64, rng: &mut u32) -> f64 {
        match wave {
            Wave::Saw => 2.0 * p - 1.0 - poly_blep(p, dt),
            Wave::Square => {
                let mut v = if p < pw { 1.0 } else { -1.0 };
                v += poly_blep(p, dt);
                let q = p - pw;
                v -= poly_blep(if q < 0.0 { q + 1.0 } else { q }, dt);
                v
            }
            Wave::Triangle => 1.0 - 4.0 * libm::fabs(p - 0.5),
            Wave::Sine => libm::sin(core::f64::consts::TAU * p),
            Wave::Noise => Self::noise(rng),
        }
    }

    /// Add the next `out.len()` frames into `out` (mono).
    pub fn render(&mut self, out: &mut [f32]) {
        let p = self.patch;
        let filter = p.filter_on();
        let pw = p.pwm.clamp(0.05, 0.5);
        let ring = p.ring.clamp(0.0, 1.0);
        let lv = [p.osc[0].level, p.osc[1].level, p.osc[2].level];
        // a chord stacks up: scale by the voices sounding so 1 note and 4 sit close
        let scale = self.level * 0.5 / libm::sqrtf(self.sounding().max(1) as f32);
        let (att, rel) = (self.attack_step, self.release_step);
        let env_coef = self.env_coef;
        for k in 0..self.voices.len() {
            if self.voices[k].note.is_none() {
                continue;
            }
            for o in out.iter_mut() {
                if self.voices[k].ctr == 0 {
                    self.update(k);
                }
                let rng = &mut self.rng;
                let v = &mut self.voices[k];
                v.ctr -= 1;
                let target = if v.held { 1.0 } else { 0.0 };
                if v.gate < target {
                    v.gate = (v.gate + att).min(1.0);
                } else if v.gate > target {
                    v.gate = (v.gate - rel).max(0.0);
                }
                v.fenv *= env_coef;
                let s1 = Self::osc(p.osc[0].wave, v.phase[0], v.inc[0], pw, rng);
                let mut s2 = Self::osc(p.osc[1].wave, v.phase[1], v.inc[1], pw, rng);
                s2 = (1.0 - ring) * s2 + ring * s1 * s2;
                let s3 = if lv[2] > 0.0 {
                    Self::osc(p.osc[2].wave, v.phase[2], v.inc[2], pw, rng)
                } else {
                    0.0
                };
                let mut x = (lv[0] * s1 + lv[1] * s2 + lv[2] * s3) as f32;
                if filter {
                    let (a1, a2, a3) = v.coef;
                    let v3 = x - v.ic2;
                    let v1 = a1 * v.ic1 + a2 * v3;
                    let v2 = v.ic2 + a2 * v.ic1 + a3 * v3;
                    v.ic1 = 2.0 * v1 - v.ic1;
                    v.ic2 = 2.0 * v2 - v.ic2;
                    x = v2;
                }
                *o += x * v.gate * scale;
                v.phase[0] += v.inc[0];
                let wrapped = v.phase[0] >= 1.0;
                if wrapped {
                    v.phase[0] -= 1.0;
                }
                if p.sync && wrapped {
                    // osc 2 starts its cycle again with osc 1's
                    v.phase[1] = v.phase[0] * v.inc[1] / v.inc[0].max(1e-12);
                } else {
                    v.phase[1] += v.inc[1];
                }
                if v.phase[1] >= 1.0 {
                    v.phase[1] -= libm::floor(v.phase[1]);
                }
                v.phase[2] += v.inc[2];
                if v.phase[2] >= 1.0 {
                    v.phase[2] -= 1.0;
                }
            }
            let v = &mut self.voices[k];
            if !v.held && v.gate <= 0.0 {
                v.note = None;
            }
        }
    }
}

// MARK: presets

/// The factory presets: (name, the synth's knobs by name and value). A preset sets every synth
/// knob; those it leaves out take the default patch's value.
pub const FACTORY_PRESETS: &[(&str, &[(&str, f64)])] = &[
    ("classic", &[]),
    (
        "acid",
        &[
            ("osc2_level", 0.0),
            ("cutoff", 380.0),
            ("resonance", 0.8),
            ("env_amount", 0.7),
            ("env_decay", 180.0),
            ("synth_release", 60.0),
            ("drift", 0.1),
        ],
    ),
    (
        "pad",
        &[
            ("synth_detune", 26.0),
            ("osc3_wave", 2.0),
            ("osc3_level", 0.5),
            ("cutoff", 2400.0),
            ("resonance", 0.2),
            ("synth_attack", 350.0),
            ("synth_release", 1400.0),
            ("drift", 0.45),
        ],
    ),
    (
        "sync lead",
        &[
            ("osc1_wave", 1.0),
            ("pwm", 0.3),
            ("osc2_oct", 1.0),
            ("osc2_level", 0.8),
            ("sync", 1.0),
            ("cutoff", 6000.0),
            ("resonance", 0.3),
            ("env_amount", 0.4),
            ("env_decay", 300.0),
            ("synth_release", 150.0),
            ("drift", 0.15),
        ],
    ),
    (
        "ring bell",
        &[
            ("osc1_wave", 3.0),
            ("osc2_wave", 3.0),
            ("osc2_oct", 1.0),
            ("osc2_fine", 30.0),
            ("ring", 1.0),
            ("synth_detune", 0.0),
            ("synth_attack", 2.0),
            ("synth_release", 900.0),
        ],
    ),
    (
        "noise sweep",
        &[
            ("osc1_wave", 4.0),
            ("osc2_level", 0.0),
            ("cutoff", 700.0),
            ("resonance", 0.85),
            ("env_amount", 0.8),
            ("env_decay", 800.0),
            ("synth_release", 400.0),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn render(s: &mut EuroSynth, n: usize) -> Vec<f32> {
        let mut out = alloc::vec![0f32; n];
        for c in out.chunks_mut(256) {
            s.render(c);
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    /// Crossings of zero upward: the fundamental's cycles.
    fn cycles(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    #[test]
    fn every_wave_sounds_at_its_pitch() {
        for w in Wave::ALL {
            let mut s = EuroSynth::new(SR);
            let mut p = SynthPatch::default();
            p.osc[0].wave = w;
            p.osc[1].level = 0.0;
            s.set_patch(&p);
            s.set_detune(0.0);
            s.note_on(69); // 440 Hz
            let x = render(&mut s, 48_000);
            assert!(rms(&x[4800..]) > 0.05, "{w:?} {}", rms(&x));
            if w != Wave::Noise {
                let c = cycles(&x[4800..]) as f64 / 0.9;
                assert!((c - 440.0).abs() < 6.0, "{w:?} {c}");
            }
        }
    }

    #[test]
    fn octave_and_fine_move_the_pitch() {
        let mut s = EuroSynth::new(SR);
        let mut p = SynthPatch::default();
        p.osc[0].wave = Wave::Sine;
        p.osc[0].octave = -1;
        p.osc[0].fine = 100.0; // a semitone up from A3
        p.osc[1].level = 0.0;
        s.set_patch(&p);
        s.set_detune(0.0);
        s.note_on(69);
        let x = render(&mut s, 48_000);
        let c = cycles(&x[4800..]) as f64 / 0.9;
        assert!((c - 233.08).abs() < 3.0, "{c}");
    }

    #[test]
    fn the_filter_takes_the_top_off_and_its_envelope_opens_it() {
        let bright = {
            let mut s = EuroSynth::new(SR);
            s.note_on(45);
            let x = render(&mut s, 24_000);
            hf(&x[2400..])
        };
        let mut s = EuroSynth::new(SR);
        let mut p = SynthPatch::default();
        p.cutoff = 300.0;
        s.set_patch(&p);
        s.note_on(45);
        let x = render(&mut s, 24_000);
        let dark = hf(&x[2400..]);
        assert!(dark < bright * 0.2, "{dark} vs {bright}");
        // the envelope: bright at the start of a note, darker as it decays
        p.env_amount = 1.0;
        p.env_decay_ms = 100.0;
        s.set_patch(&p);
        s.all_off();
        render(&mut s, 4800);
        s.note_on(45);
        let x = render(&mut s, 48_000);
        let (start, end) = (hf(&x[240..2640]), hf(&x[40_000..]));
        assert!(start > end * 3.0, "{start} vs {end}");
    }

    /// The high frequencies: the first difference's level.
    fn hf(x: &[f32]) -> f32 {
        let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d)
    }

    #[test]
    fn resonance_rings_at_the_cutoff() {
        // a sine at the cutoff: the filter's gain there is 1/k (k = 2 open, ~0.15 resonant)
        let level = |res: f64| {
            let mut s = EuroSynth::new(SR);
            let mut p = SynthPatch::default();
            p.osc[0].wave = Wave::Sine;
            p.osc[1].level = 0.0;
            p.cutoff = note_hz(83);
            p.resonance = res;
            s.set_patch(&p);
            s.set_detune(0.0);
            s.note_on(83);
            let x = render(&mut s, 24_000);
            rms(&x[2400..])
        };
        let (flat, ringing) = (level(0.0), level(0.95));
        assert!(ringing > flat * 5.0, "{ringing} vs {flat}");
    }

    #[test]
    fn sync_and_ring_change_the_sound() {
        let take = |f: &dyn Fn(&mut SynthPatch)| {
            let mut s = EuroSynth::new(SR);
            let mut p = SynthPatch::default();
            p.osc[0].level = 0.0;
            p.osc[1].octave = 1;
            p.osc[1].fine = 37.0;
            f(&mut p);
            s.set_patch(&p);
            s.note_on(45);
            render(&mut s, 9600)
        };
        let plain = take(&|_| {});
        let synced = take(&|p| p.sync = true);
        let ringed = take(&|p| p.ring = 1.0);
        let diff = |a: &[f32], b: &[f32]| rms(&a.iter().zip(b).map(|(x, y)| x - y).collect::<Vec<_>>());
        assert!(diff(&plain, &synced) > 0.02);
        assert!(diff(&plain, &ringed) > 0.02);
        // synced, osc 2 repeats with osc 1's period: 110 Hz cycles of the same shape
        let period = (SR / 110.0).round() as usize;
        let a = &synced[4800..4800 + 400];
        let b = &synced[4800 + period..4800 + period + 400];
        assert!(diff(a, b) < 0.05, "synced osc 2 follows osc 1");
    }

    #[test]
    fn drift_wobbles_the_pitch() {
        let take = |drift: f64| {
            let mut s = EuroSynth::new(SR);
            let mut p = SynthPatch::default();
            p.drift = drift;
            s.set_patch(&p);
            s.note_on(57);
            render(&mut s, 96_000)
        };
        let (a, b) = (take(0.0), take(1.0));
        assert_ne!(a, b);
        assert_eq!(take(1.0), b, "deterministic");
    }

    #[test]
    fn attack_and_release_shape_the_gate() {
        let mut s = EuroSynth::new(SR);
        let mut p = SynthPatch::default();
        p.attack_ms = 500.0;
        p.release_ms = 500.0;
        s.set_patch(&p);
        s.note_on(57);
        let x = render(&mut s, 48_000);
        assert!(rms(&x[..2400]) < rms(&x[36_000..]) * 0.3);
        s.note_off(57);
        let y = render(&mut s, 12_000);
        assert!(rms(&y[..1200]) > 0.01, "still ringing out");
        assert_eq!(s.sounding(), 1);
        render(&mut s, 24_000);
        assert_eq!(s.sounding(), 0);
    }
}

#[cfg(test)]
mod param_tests {
    use crate::fx::vocoder::{VocoderParam, VocoderSettings};

    #[test]
    fn every_synth_knob_has_a_name_a_range_and_a_round_trip() {
        let mut s = VocoderSettings::default();
        for p in VocoderParam::SYNTH {
            assert_eq!(VocoderParam::parse(p.name()), Some(p), "{}", p.name());
            assert!(VocoderParam::ALL.contains(&p));
            let (lo, hi) = p.range();
            for t in [0.0, 0.25, 0.5, 1.0] {
                let v = p.from_knob(t);
                assert!(v >= lo && v <= hi, "{} {v}", p.name());
                let kept = s.set(p, v).unwrap();
                assert!((p.to_knob(kept) - t).abs() < 0.2 || p.is_stepped(), "{}", p.name());
            }
        }
        // waves, octaves and sync are steps
        assert!(VocoderParam::Osc2Wave.is_stepped() && VocoderParam::Osc3Oct.is_stepped());
        assert!(!VocoderParam::Cutoff.is_stepped() && VocoderParam::Cutoff.is_continuous());
        assert_eq!(s.set(VocoderParam::Osc1Oct, 1.4), Some(1.0));
        // values in words
        assert_eq!(VocoderParam::Osc1Wave.value_of_text("square"), Some(1.0));
        assert_eq!(VocoderParam::Osc3Wave.value_of_text("Noise"), Some(4.0));
        assert_eq!(VocoderParam::Sync.value_of_text("on"), Some(1.0));
        assert_eq!(VocoderParam::Vowel.value_of_text("o"), Some(3.0));
        assert_eq!(VocoderParam::Cutoff.value_of_text("loud"), None);
        // aliases
        assert_eq!(VocoderParam::parse("res"), Some(VocoderParam::Resonance));
        assert_eq!(VocoderParam::parse("spread"), Some(VocoderParam::SynthDetune));
        // the cutoff knob is logarithmic: its middle is near 735 Hz, not 9 kHz
        let mid = VocoderParam::Cutoff.from_knob(0.5);
        assert!(mid > 600.0 && mid < 900.0, "{mid}");
    }

    #[test]
    fn the_default_patch_is_the_old_carrier() {
        let s = VocoderSettings::default();
        assert!(!s.synth.filter_on());
        assert_eq!(s.synth_detune, 12.0);
        assert_eq!(s.synth.osc[2].level, 0.0);
        assert!(!s.bypass);
    }

    #[test]
    fn factory_presets_name_real_knobs() {
        for (name, knobs) in super::FACTORY_PRESETS {
            for (k, v) in knobs.iter() {
                let p = VocoderParam::parse(k).unwrap_or_else(|| panic!("{name}: {k}"));
                assert!(VocoderParam::SYNTH.contains(&p), "{name}: {k}");
                let (lo, hi) = p.range();
                assert!(*v >= lo && *v <= hi, "{name}: {k} {v}");
            }
        }
    }
}
