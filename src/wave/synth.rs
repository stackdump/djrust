//! Port of `Beats/Wave/WaveSynth.swift` (beats' `wave-engine/synth.js`): pulse shapes,
//! voice specs, wavetables, noise seeds and biquad coefficients. Pure functions; the
//! runner realises them per sample.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::json::JsonValue;

pub const WAVE_TABLE_SIZE: usize = 2048;
/// `PPQ`: ticks per beat, fixed in the sequencer worker.
pub const PPQ: f64 = 4.0;

/// `pulse(attack, decay)`'s result: `P(t) = Σ α_e e^{-t/τ_e}`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Pulse {
    pub alpha: Vec<f64>,
    pub tau: Vec<f64>,
}

/// `pulse(attack, decay)`: two exponentials (no onset click), or one if attack ≥ decay.
pub fn pulse(attack: f64, decay: f64) -> Pulse {
    if !(attack > 0.0) || attack >= decay {
        return Pulse {
            alpha: vec![1.0],
            tau: vec![decay],
        };
    }
    Pulse {
        alpha: vec![1.0, -1.0],
        tau: vec![decay, attack],
    }
}

/// A voice spec, the union of the drum and tonal shapes synth.js returns, plus the
/// fields beyond beats' wave engine (`InstrumentTable`; their defaults leave a voice
/// exactly as beats has it).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct VoiceSpec {
    /// kick | snare | clap | hat | openhat | crash | tonal
    pub kind: String,
    /// sweep | noise | noise+tone | table | dc
    pub carrier: String,
    pub f1: f64,
    pub f0: f64,
    pub sweep_tau: f64,
    pub filter: String,
    pub fc: f64,
    pub q: f64,
    pub tone: f64,
    pub tone_mix: f64,
    pub pulse: Pulse,
    pub level: f64,
    // tonal
    pub wave: String,
    pub attack: f64,
    pub decay_scale: f64,
    pub max_decay: f64,
    // beyond beats
    /// "table" (wavetable) | "pluck" (Karplus-Strong string, as Tone's PluckSynth).
    pub engine: String,
    /// pluck: noise burst length in periods of the note.
    pub attack_noise: f64,
    /// pluck: lowpass cutoff inside the string loop, Hz.
    pub dampening: f64,
    /// pluck: loop feedback, 0…1.
    pub resonance: f64,
    /// Pitch vibrato: rate in Hz, depth as a fraction of the frequency.
    pub vibrato_rate: f64,
    pub vibrato_depth: f64,
    /// Post lowpass on the voice sum, Hz (0 = off).
    pub lowpass: f64,
    /// Tone.Distortion amount after the lowpass (0 = off).
    pub distortion: f64,
    /// Per-note pitch envelope (laser zaps): the note starts at `pitch_from` × its
    /// frequency and glides exponentially to `pitch_to` × over `pitch_time` seconds.
    /// `pitch_time` 0 = off.
    pub pitch_from: f64,
    pub pitch_to: f64,
    pub pitch_time: f64,
}

impl VoiceSpec {
    pub fn new(kind: &str, carrier: &str) -> VoiceSpec {
        VoiceSpec {
            kind: kind.into(),
            carrier: carrier.into(),
            f1: 0.0,
            f0: 0.0,
            sweep_tau: 0.0,
            filter: String::new(),
            fc: 0.0,
            q: 0.0,
            tone: 0.0,
            tone_mix: 0.0,
            pulse: Pulse {
                alpha: vec![1.0],
                tau: vec![0.1],
            },
            level: 0.0,
            wave: "saw".into(),
            attack: 0.0,
            decay_scale: 0.0,
            max_decay: 0.0,
            engine: "table".into(),
            attack_noise: 1.0,
            dampening: 4000.0,
            resonance: 0.9,
            vibrato_rate: 0.0,
            vibrato_depth: 0.0,
            lowpass: 0.0,
            distortion: 0.0,
            pitch_from: 1.0,
            pitch_to: 1.0,
            pitch_time: 0.0,
        }
    }

    /// Decode the JSON shape synth.js emits (tests and voice overrides): `VoiceSpec(json:)`.
    pub fn from_json(v: &JsonValue) -> VoiceSpec {
        let s = |k: &str, d: &str| v.get(k).and_then(JsonValue::as_str).unwrap_or(d).into();
        let n = |k: &str, d: f64| v.get(k).and_then(JsonValue::as_f64).unwrap_or(d);
        let mut out = VoiceSpec::new(
            v.get("kind").and_then(JsonValue::as_str).unwrap_or("tonal"),
            v.get("carrier").and_then(JsonValue::as_str).unwrap_or("dc"),
        );
        out.f1 = n("f1", 0.0);
        out.f0 = n("f0", 0.0);
        out.sweep_tau = n("sweepTau", 0.0);
        out.filter = s("filter", "");
        out.fc = n("fc", 0.0);
        out.q = n("q", 0.0);
        out.tone = n("tone", 0.0);
        out.tone_mix = n("toneMix", 0.0);
        out.apply_extended(v);
        if let Some(p) = v.get("pulse") {
            let list = |k: &str| -> Vec<f64> {
                p.get(k)
                    .and_then(JsonValue::as_array)
                    .map(|a| a.iter().filter_map(JsonValue::as_f64).collect())
                    .unwrap_or_default()
            };
            out.pulse = Pulse {
                alpha: list("alpha"),
                tau: list("tau"),
            };
        }
        out.level = n("level", 0.0);
        out.wave = s("wave", "saw");
        out.attack = n("attack", 0.0);
        out.decay_scale = n("decayScale", 0.0);
        out.max_decay = n("maxDecay", 0.0);
        out
    }

    /// `VoiceSpec.overlay`: set the fields `v` names, leave the rest.
    pub fn overlay(&mut self, v: &JsonValue) {
        if let Some(x) = v.get("level").and_then(JsonValue::as_f64) {
            self.level = x;
        }
        if let Some(x) = v.get("wave").and_then(JsonValue::as_str) {
            self.wave = x.into();
        }
        if let Some(x) = v.get("attack").and_then(JsonValue::as_f64) {
            self.attack = x;
        }
        if let Some(x) = v.get("decayScale").and_then(JsonValue::as_f64) {
            self.decay_scale = x;
        }
        if let Some(x) = v.get("maxDecay").and_then(JsonValue::as_f64) {
            self.max_decay = x;
        }
        self.apply_extended(v);
    }

    fn apply_extended(&mut self, v: &JsonValue) {
        let n = |k: &str| v.get(k).and_then(JsonValue::as_f64);
        if let Some(x) = v.get("engine").and_then(JsonValue::as_str) {
            self.engine = x.into();
        }
        if let Some(x) = n("attackNoise") {
            self.attack_noise = x;
        }
        if let Some(x) = n("dampening") {
            self.dampening = x;
        }
        if let Some(x) = n("resonance") {
            self.resonance = x;
        }
        if let Some(x) = n("vibratoRate") {
            self.vibrato_rate = x;
        }
        if let Some(x) = n("vibratoDepth") {
            self.vibrato_depth = x;
        }
        if let Some(x) = n("lowpass") {
            self.lowpass = x;
        }
        if let Some(x) = n("distortion") {
            self.distortion = x;
        }
        if let Some(x) = n("pitchFrom") {
            self.pitch_from = x;
        }
        if let Some(x) = n("pitchTo") {
            self.pitch_to = x;
        }
        if let Some(x) = n("pitchTime") {
            self.pitch_time = x;
        }
    }
}

struct Kit {
    kick_decay: f64,
    kick_octaves: f64,
    snare_decay: f64,
    hihat_decay: f64,
}

/// Mirrors tone-engine.js::_synthDrumKit's per-kit constants.
fn kit(name: &str) -> Option<Kit> {
    let k = |kick_decay, kick_octaves, snare_decay, hihat_decay| Kit {
        kick_decay,
        kick_octaves,
        snare_decay,
        hihat_decay,
    };
    Some(match name {
        "drums" => k(0.30, 6.0, 0.15, 0.05),
        "drums-breakbeat" => k(0.20, 4.0, 0.20, 0.08),
        "drums-cr78" => k(0.25, 3.0, 0.12, 0.04),
        "drums-v8" => k(0.35, 8.0, 0.18, 0.06),
        "drums-808" => k(0.60, 8.0, 0.20, 0.04),
        "drums-lofi" => k(0.25, 4.0, 0.15, 0.05),
        _ => return None,
    })
}

/// `isDrumChannel`
pub fn is_drum_channel(ch: i64) -> bool {
    (10..=15).contains(&ch)
}

/// Same note → drum-role dispatch as `_synthDrumKit`.
pub fn drum_kind(note: i64) -> &'static str {
    match note {
        36 | 35 => "kick",
        38 | 40 | 37 => "snare",
        39 => "clap",
        42..=46 => "hat",
        49 | 57 => "openhat",
        _ => "kick",
    }
}

/// `isSynthDrumKit`: one of beats' synth drum kits (`drums`, `drums-808`, …).
pub fn is_synth_drum_kit(name: &str) -> bool {
    kit(name).is_some()
}

/// beats' synth drum kits, by name (sorted).
pub fn synth_drum_kits() -> Vec<&'static str> {
    vec![
        "drums",
        "drums-808",
        "drums-breakbeat",
        "drums-cr78",
        "drums-lofi",
        "drums-v8",
    ]
}

/// `drumVoice(kind, instrument)`
pub fn drum_voice(kind: &str, instrument: &str) -> VoiceSpec {
    let k = kit(instrument).unwrap_or_else(|| kit("drums").unwrap());
    match kind {
        "kick" => {
            let mut v = VoiceSpec::new("kick", "sweep");
            v.f1 = 48.0;
            v.f0 = 48.0 * k.kick_octaves;
            v.sweep_tau = 0.03;
            v.pulse = pulse(0.001, k.kick_decay / 2.5);
            v.level = 0.9;
            v
        }
        "snare" => {
            let mut v = VoiceSpec::new("snare", "noise+tone");
            v.filter = "bandpass".into();
            v.fc = 3000.0;
            v.q = 0.9;
            v.tone = 180.0;
            v.tone_mix = 0.35;
            v.pulse = pulse(0.001, k.snare_decay / 2.5);
            v.level = 0.6;
            v
        }
        "clap" => {
            let mut v = VoiceSpec::new("clap", "noise");
            v.filter = "bandpass".into();
            v.fc = 1400.0;
            v.q = 1.2;
            v.pulse = pulse(0.002, 0.06);
            v.level = 0.6;
            v
        }
        "crash" => {
            // Not in beats (its drum map plays note 49 as the open hat): dj's section
            // crash (`DJMusicality.Fixes.crash`). Bright noise with a cymbal's long tail.
            let mut v = VoiceSpec::new("crash", "noise");
            v.filter = "highpass".into();
            v.fc = 4200.0;
            v.q = 0.5;
            v.pulse = pulse(0.002, 0.7);
            v.level = 0.2;
            v
        }
        "openhat" => {
            let mut v = VoiceSpec::new("openhat", "noise");
            v.filter = "highpass".into();
            v.fc = 7000.0;
            v.q = 0.7;
            v.pulse = pulse(0.001, k.hihat_decay * 2.5);
            v.level = 0.28;
            v
        }
        _ => {
            let mut v = VoiceSpec::new("hat", "noise");
            v.filter = "highpass".into();
            v.fc = 7000.0;
            v.q = 0.7;
            v.pulse = pulse(0.001, k.hihat_decay / 2.0);
            v.level = 0.32;
            v
        }
    }
}

fn matches(name: &str, parts: &[&str]) -> bool {
    parts.iter().any(|p| name.contains(p))
}

/// `tonalVoice(instrument, group)`: instrument families are matched by name, as the JS
/// regexes do.
pub fn tonal_voice(instrument: Option<&str>, group: Option<&str>) -> VoiceSpec {
    let name = instrument.unwrap_or("");
    let mut wave = "saw";
    let mut attack = 0.003;
    let mut decay_scale = 0.5;
    let mut level = 0.22;
    let mut max_decay = 1.5;
    if matches(name, &["pad", "strings", "choir", "organ"])
        || group == Some("harmony")
        || group == Some("pad")
    {
        wave = "soft";
        attack = 0.06;
        decay_scale = 0.8;
        level = 0.12;
        max_decay = 3.0;
    } else if matches(name, &["sub", "808"]) {
        wave = "sine";
        level = 0.42;
    } else if matches(name, &["bass", "reese", "acid"]) || group == Some("bass") {
        wave = "saw";
        level = 0.26;
    } else if matches(
        name,
        &[
            "bell",
            "vibes",
            "marimba",
            "kalimba",
            "music-box",
            "steel",
            "glass",
        ],
    ) {
        wave = "bell";
        attack = 0.001;
        decay_scale = 1.2;
        level = 0.2;
    } else if matches(name, &["square", "chiptune", "pwm", "clav"]) {
        wave = "square";
        level = 0.16;
    } else if matches(name, &["pluck", "stab", "piano", "guitar", "harpsi"]) {
        wave = "saw";
        attack = 0.002;
        decay_scale = 0.35;
        level = 0.2;
    }
    let mut v = VoiceSpec::new("tonal", "table");
    v.wave = wave.into();
    v.attack = attack;
    v.decay_scale = decay_scale;
    v.max_decay = max_decay;
    v.level = level;
    v
}

fn partials(wave: &str) -> Vec<f64> {
    match wave {
        "saw" => vec![
            1.0,
            1.0 / 2.0,
            1.0 / 3.0,
            1.0 / 4.0,
            1.0 / 5.0,
            1.0 / 6.0,
            1.0 / 7.0,
            1.0 / 8.0,
        ],
        "square" => vec![1.0, 0.0, 1.0 / 3.0, 0.0, 1.0 / 5.0, 0.0, 1.0 / 7.0],
        "soft" => vec![1.0, 0.5, 0.25, 0.12],
        "bell" => vec![1.0, 0.0, 0.45, 0.0, 0.0, 0.25, 0.0, 0.0, 0.12],
        _ => vec![1.0], // sine, and the fallback
    }
}

/// Normalised additive wavetable (`WAVE_TABLE_SIZE` + 1 guard sample), stored as float32.
pub fn build_table(wave: &str) -> Vec<f32> {
    let amps = partials(wave);
    let mut t = vec![0.0f64; WAVE_TABLE_SIZE];
    let mut peak = 0.0f64;
    for (i, slot) in t.iter_mut().enumerate() {
        let mut s = 0.0;
        for (k, a) in amps.iter().enumerate() {
            if *a != 0.0 {
                s += a * libm::sin(
                    2.0 * core::f64::consts::PI * (k + 1) as f64 * i as f64
                        / WAVE_TABLE_SIZE as f64,
                );
            }
        }
        // The JS stores into a Float32Array then normalises the stored (rounded) values.
        *slot = (s as f32) as f64;
        if libm::fabs(s) > peak {
            peak = libm::fabs(s);
        }
    }
    let mut out = vec![0.0f32; WAVE_TABLE_SIZE + 1];
    for i in 0..WAVE_TABLE_SIZE {
        out[i] = (t[i] / peak) as f32;
    }
    out[WAVE_TABLE_SIZE] = out[0];
    out
}

/// `noiseSeed(i)`: seeded xorshift32 seed for lane `i`.
pub fn noise_seed(i: i64) -> i32 {
    let s = (0x9E37_79B9u32 as i32) ^ ((i + 1) as i32).wrapping_mul(0x85EB_CA6Bu32 as i32);
    if s == 0 {
        1
    } else {
        s
    }
}

/// The Q Swift passes for one Butterworth section: its literal `0.7071`, not
/// `FRAC_1_SQRT_2`, so the coefficients match Swift's bit for bit.
#[allow(clippy::approx_constant)]
pub const BUTTERWORTH_Q: f64 = 0.7071;

/// `biquad(type, fc, q, sr)`: RBJ coefficients `[b0, b1, b2, a1, a2]` (normalised by a0).
pub fn biquad(kind: &str, fc: f64, q: f64, sr: f64) -> [f64; 5] {
    let fcm = if fc < sr * 0.45 { fc } else { sr * 0.45 };
    let w = 2.0 * core::f64::consts::PI * fcm / sr;
    let cw = libm::cos(w);
    let sw = libm::sin(w);
    let alpha = sw / (2.0 * q);
    let (b0, b1, b2) = match kind {
        "highpass" => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0),
        "lowpass" => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0),
        _ => (alpha, 0.0, -alpha),
    };
    let a0 = 1.0 + alpha;
    [
        b0 / a0,
        b1 / a0,
        b2 / a0,
        (-2.0 * cw) / a0,
        (1.0 - alpha) / a0,
    ]
}

/// Tone.Distortion's waveshaping curve (`amount` 0…1): `toneDistortion`.
#[inline]
pub fn tone_distortion(x: f64, amount: f64) -> f64 {
    let k = amount * 100.0;
    (3.0 + k) * x * 20.0 * (core::f64::consts::PI / 180.0)
        / (core::f64::consts::PI + k * libm::fabs(x))
}
