//! Port of `Musicality/SynthVoice.swift`: the sound-design data the runner's fixes read
//! (`.subtractive`, `.unison`, `.kickTune`, `.subOctave`, `.backbeat`'s brushes, `.hfTilt`).
//! The band-limited wavetables (`MipTables`) are `wave::mip`. The runner plays `.subtractive`,
//! `.unison`, `.subOctave`, `.kickTune`, `.hfTilt` and the brushes from this data
//! (`wave::runner`).

use alloc::collections::BTreeSet;
use alloc::string::String;

use crate::gen::tables::BeatsTables;
use crate::musicality::mix_targets::MixTargets;
use crate::voices::sampler::velocity_scale;
use crate::wave::groove::FAMILY_NAMES;
use crate::wave::synth::{pulse, VoiceSpec};

/// How a saw / square voice is shaped under `.subtractive`: a lowpass per voice whose cutoff
/// opens on the note and closes as it plays, and opens further the harder the note is struck.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterEnvelope {
    /// Where the cutoff rests, in multiples of the note's frequency (keyboard tracking).
    pub base: f64,
    /// How far a full-velocity note opens it, Hz, on top of `base`.
    pub amount: f64,
    /// Time constant of the close, seconds.
    pub seconds: f64,
    /// Resonance (Q of the two-pole state-variable lowpass).
    pub q: f64,
}

impl FilterEnvelope {
    /// A bass: dark at rest, a short bright bite on the attack.
    pub const BASS: FilterEnvelope = FilterEnvelope {
        base: 2.5,
        amount: 1400.0,
        seconds: 0.16,
        q: 1.0,
    };
    /// A lead / arp: brighter at rest, a longer open.
    pub const LEAD: FilterEnvelope = FilterEnvelope {
        base: 4.0,
        amount: 5000.0,
        seconds: 0.35,
        q: 0.8,
    };
    /// A pluck / stab / keys voice: wide open on the strike, closed fast.
    pub const PLUCK: FilterEnvelope = FilterEnvelope {
        base: 3.0,
        amount: 6000.0,
        seconds: 0.12,
        q: 0.8,
    };

    /// The envelope for a tonal synth voice of part `group`, or None for voices it leaves as
    /// they are. Parts in `plucked` take `PLUCK` whatever their voice.
    pub fn for_voice(
        v: &VoiceSpec,
        group: &str,
        plucked: &BTreeSet<String>,
    ) -> Option<FilterEnvelope> {
        if !(v.engine == "table" && (v.wave == "saw" || v.wave == "square") && v.lowpass == 0.0) {
            return None;
        }
        if group == "bass" {
            return Some(Self::BASS);
        }
        if plucked.contains(group) {
            return Some(Self::PLUCK);
        }
        // tonalVoice's pluck / stab / piano / guitar family: decayScale 0.35
        if v.decay_scale < 0.4 {
            return Some(Self::PLUCK);
        }
        Some(Self::LEAD)
    }
}

/// `.unison`: a voice played as two oscillators a few cents either side of the note.
pub struct Unison;

impl Unison {
    /// How far apart a pad's two oscillators sit (side gain each way) at full width.
    pub const WIDTH: f64 = 0.35;
    /// The pair's difference is highpassed here (second order).
    pub const SIDE_HZ: f64 = 300.0;

    /// Detune either side of the note, cents, for a tonal synth voice of part `group`; None =
    /// one oscillator. Pads (the soft table) 7, saw leads 6.
    pub fn cents(v: &VoiceSpec, group: &str) -> Option<f64> {
        if !(v.engine == "table" && v.lowpass == 0.0 && v.distortion == 0.0 && group != "bass") {
            return None;
        }
        if v.wave == "soft" && (group == "harmony" || group == "pad") {
            return Some(7.0);
        }
        if FilterEnvelope::for_voice(v, group, &BTreeSet::new()) == Some(FilterEnvelope::LEAD)
            && group == "melody"
        {
            return Some(6.0);
        }
        None
    }
}

/// Per-genre settings for the runner's sound design (`WaveOptions.voiceDesign`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct VoiceDesign {
    /// Parts whose saw / square voices take the `.pluck` filter envelope under `.subtractive`.
    pub pluck_groups: BTreeSet<String>,
    /// Scales the pad's stereo spread under `.stereo` and its unison pair's width.
    pub pad_width: f64,
    /// Where the pad's unison pair's difference is highpassed.
    pub pad_side_hz: f64,
}

impl Default for VoiceDesign {
    fn default() -> Self {
        VoiceDesign {
            pluck_groups: BTreeSet::new(),
            pad_width: 1.0,
            pad_side_hz: Unison::SIDE_HZ,
        }
    }
}

impl VoiceDesign {
    /// The arp in trance, edm and synthwave is a pluck; ambient's pads are narrower and
    /// their side starts higher.
    pub fn for_genre(name: &str) -> VoiceDesign {
        match crate::gen::tables::BeatsTables::shared().kin(name) {
            "trance" | "edm" | "synthwave" => VoiceDesign {
                pluck_groups: ["arp".into()].into_iter().collect(),
                ..Default::default()
            },
            "ambient" => VoiceDesign {
                pad_width: 0.6,
                pad_side_hz: 600.0,
                ..Default::default()
            },
            _ => VoiceDesign::default(),
        }
    }
}

/// `.kickTune`: the kick's body in the track's key.
pub struct KickTuning;

impl KickTuning {
    /// Where a kick's tail sits best: low enough to be felt, high enough for small speakers.
    pub const CENTRE_HZ: f64 = 50.0;

    /// The kick's resting frequency for a track whose key's root is `root` (a MIDI note).
    pub fn hz(root: Option<i64>) -> Option<f64> {
        let root = root?;
        let mut best: Option<f64> = None;
        for pc in [root % 12, (root + 7) % 12] {
            // the octave of this pitch class nearest `CENTRE_HZ`
            let mut f = 440.0 * libm::pow(2.0, (pc as f64 - 69.0) / 12.0);
            while f > Self::CENTRE_HZ * libm::sqrt(2.0) {
                f /= 2.0;
            }
            while f < Self::CENTRE_HZ / libm::sqrt(2.0) {
                f *= 2.0;
            }
            if best.map_or(true, |b| {
                libm::fabs(libm::log2(f / Self::CENTRE_HZ))
                    < libm::fabs(libm::log2(b / Self::CENTRE_HZ))
            }) {
                best = Some(f);
            }
        }
        best
    }
}

/// `.subOctave`: a sine an octave under each bass note (dnb, dubstep, trap).
pub struct SubOctave;

impl SubOctave {
    pub const GENRES: [&'static str; 3] = ["dnb", "dubstep", "trap"];
    /// Its level against the voice's oscillator (−6 dB).
    pub const GAIN: f64 = 0.5;
    /// The lowest sub it plays, Hz.
    pub const MIN_HZ: f64 = 30.0;

    pub fn for_genre(genre: &str) -> bool {
        Self::GENRES.contains(&genre)
    }
}

/// Brushes on the snare, for jazz under `.backbeat`.
pub struct Brush;

impl Brush {
    /// The envelope's rise and fall time constants, seconds.
    pub const ATTACK: f64 = 0.02;
    pub const DECAY: f64 = 0.07;

    pub fn for_genre(genre: &str) -> bool {
        genre == "jazz"
    }

    /// The synth brush: band-passed noise, no tonal body.
    pub fn voice() -> VoiceSpec {
        let mut v = VoiceSpec::new("snare", "noise");
        v.filter = "bandpass".into();
        v.fc = 2500.0;
        v.q = 0.6;
        v.pulse = pulse(Self::ATTACK, Self::DECAY);
        v.level = 0.7;
        v
    }

    /// A sampled kit whose bank has no Brush kit plays its own snare this much softer.
    pub fn stock_velocity() -> f64 {
        velocity_scale(-8.0)
    }
}

/// `.hfTilt` (ROADMAP 6.6): the mix's top end tilted down at the voices that carry it.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct HfTilt {
    /// The lowpass's cutoff on hat / open hat lanes, Hz.
    pub hat_hz: f64,
    /// …and on the crash.
    pub crash_hz: f64,
    /// Butterworth: no peak at the cutoff.
    pub q: f64,
}

impl HfTilt {
    pub fn new(hat_hz: f64, crash_hz: Option<f64>) -> HfTilt {
        HfTilt {
            hat_hz,
            crash_hz: crash_hz.unwrap_or(hat_hz),
            q: libm::sqrt(0.5),
        }
    }

    /// The cutoff for a drum lane of `kind`; None for what the tilt leaves alone.
    pub fn lowpass_hz(&self, kind: &str) -> Option<f64> {
        match kind {
            "hat" | "openhat" => Some(self.hat_hz),
            "crash" => Some(self.crash_hz),
            _ => None,
        }
    }

    /// The tilt a track of `genre` plays with: its family's cutoff from targets.json.
    pub fn for_genre(genre: &str) -> Option<HfTilt> {
        let i = BeatsTables::shared()
            .family_of(genre)
            .max(0)
            .min(FAMILY_NAMES.len() as i64 - 1);
        MixTargets::shared()
            .hf_tilt_hz(FAMILY_NAMES[i as usize])
            .map(|hz| HfTilt::new(hz, None))
    }
}
