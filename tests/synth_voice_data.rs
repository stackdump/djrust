//! `Musicality/SynthVoice.swift`'s sound-design data, held to the Swift source's literals (the
//! values its comments and `SynthVoiceTests` / `StereoTests` / `BrushTests` rely on): the filter
//! envelopes, who gets a unison pair and how wide, the per-genre voice design, the sub-octave,
//! the brush and the HF tilt. The runner plays them (`wave_voices.rs`, `wave_suboctave.rs`,
//! `wave_stereo.rs`, `wave_drums.rs`); this file pins the data they read.

use djrust::musicality::synth_voice::{
    Brush, FilterEnvelope, HfTilt, KickTuning, SubOctave, Unison, VoiceDesign,
};
use djrust::wave::synth::{tonal_voice, VoiceSpec};
use djrust::wave::InstrumentTable;
use std::collections::BTreeSet;

fn voice(instrument: &str, group: &str) -> VoiceSpec {
    tonal_voice(Some(instrument), Some(group))
}

#[test]
fn the_filter_envelopes() {
    assert_eq!(
        (FilterEnvelope::BASS.base, FilterEnvelope::BASS.amount),
        (2.5, 1400.0)
    );
    assert_eq!(
        (FilterEnvelope::BASS.seconds, FilterEnvelope::BASS.q),
        (0.16, 1.0)
    );
    assert_eq!(
        FilterEnvelope::LEAD,
        FilterEnvelope {
            base: 4.0,
            amount: 5000.0,
            seconds: 0.35,
            q: 0.8
        }
    );
    assert_eq!(
        FilterEnvelope::PLUCK,
        FilterEnvelope {
            base: 3.0,
            amount: 6000.0,
            seconds: 0.12,
            q: 0.8
        }
    );
}

/// `FilterEnvelope.forVoice`: only a table saw or square with no lowpass of its own; a bass is
/// `.bass`, a part in `plucked` (and a decayScale under 0.4) `.pluck`, anything else `.lead`.
#[test]
fn who_gets_a_filter_envelope() {
    let none = BTreeSet::new();
    let mut saw = VoiceSpec::new("lead", "dc");
    saw.engine = "table".into();
    saw.wave = "saw".into();
    saw.decay_scale = 1.0;
    assert_eq!(
        FilterEnvelope::for_voice(&saw, "bass", &none),
        Some(FilterEnvelope::BASS)
    );
    assert_eq!(
        FilterEnvelope::for_voice(&saw, "melody", &none),
        Some(FilterEnvelope::LEAD)
    );
    let arp: BTreeSet<String> = ["arp".to_string()].into_iter().collect();
    assert_eq!(
        FilterEnvelope::for_voice(&saw, "arp", &arp),
        Some(FilterEnvelope::PLUCK)
    );
    assert_eq!(
        FilterEnvelope::for_voice(&saw, "arp", &none),
        Some(FilterEnvelope::LEAD)
    );
    // a short decay is a pluck / stab / keys voice
    saw.decay_scale = 0.35;
    assert_eq!(
        FilterEnvelope::for_voice(&saw, "melody", &none),
        Some(FilterEnvelope::PLUCK)
    );
    // a square too; a sine, a soft table, a string and a voice with its own lowpass: none
    let mut square = saw.clone();
    square.wave = "square".into();
    assert!(FilterEnvelope::for_voice(&square, "melody", &none).is_some());
    for wave in ["sine", "soft", "triangle"] {
        let mut v = saw.clone();
        v.wave = wave.into();
        assert!(
            FilterEnvelope::for_voice(&v, "melody", &none).is_none(),
            "{wave}"
        );
    }
    let mut filtered = saw.clone();
    filtered.lowpass = 2000.0;
    assert!(FilterEnvelope::for_voice(&filtered, "melody", &none).is_none());
    let guitar = InstrumentTable::shared().voice(Some("acoustic-guitar"), Some("melody"));
    assert_eq!(guitar.engine, "pluck");
    assert!(FilterEnvelope::for_voice(&guitar, "melody", &none).is_none());
}

#[test]
fn the_unison_pairs() {
    assert_eq!((Unison::WIDTH, Unison::SIDE_HZ), (0.35, 300.0));
    let pad = voice("dark-pad", "harmony");
    assert_eq!(Unison::cents(&pad, "harmony"), Some(7.0));
    assert_eq!(Unison::cents(&pad, "pad"), Some(7.0));
    assert_eq!(Unison::cents(&pad, "bass"), None);
    assert_eq!(
        Unison::cents(&voice("supersaw", "melody"), "melody"),
        Some(6.0)
    );
    // basses stay single, as do strings and a lead with a lowpass or a drive
    assert_eq!(Unison::cents(&voice("sub-bass", "bass"), "bass"), None);
    assert_eq!(
        Unison::cents(&voice("acoustic-guitar", "melody"), "melody"),
        None
    );
}

#[test]
fn the_voice_design_by_genre() {
    let plain = VoiceDesign::default();
    assert!(plain.pluck_groups.is_empty());
    assert_eq!((plain.pad_width, plain.pad_side_hz), (1.0, Unison::SIDE_HZ));
    for g in ["trance", "edm", "synthwave"] {
        let d = VoiceDesign::for_genre(g);
        assert_eq!(
            d.pluck_groups,
            ["arp".to_string()].into_iter().collect(),
            "{g}"
        );
        assert_eq!((d.pad_width, d.pad_side_hz), (1.0, Unison::SIDE_HZ), "{g}");
    }
    let ambient = VoiceDesign::for_genre("ambient");
    assert!(ambient.pluck_groups.is_empty());
    assert_eq!((ambient.pad_width, ambient.pad_side_hz), (0.6, 600.0));
    assert!(ambient.pad_width < 1.0 && ambient.pad_side_hz > Unison::SIDE_HZ);
    for g in ["techno", "house", "dnb", "jazz", "unknown"] {
        assert_eq!(VoiceDesign::for_genre(g), plain, "{g}");
    }
}

#[test]
fn the_sub_octave_the_brush_and_the_tilt() {
    assert_eq!((SubOctave::GAIN, SubOctave::MIN_HZ), (0.5, 30.0));
    for g in ["dnb", "dubstep", "trap"] {
        assert!(SubOctave::for_genre(g), "{g}");
    }
    for g in ["techno", "house", "lofi", "jazz", ""] {
        assert!(!SubOctave::for_genre(g), "{g}");
    }
    assert!(Brush::for_genre("jazz") && !Brush::for_genre("lofi") && !Brush::for_genre("bossa"));
    assert_eq!((Brush::ATTACK, Brush::DECAY), (0.02, 0.07));
    // -8 dB as a velocity: 10^(-8/40)
    assert_eq!(Brush::stock_velocity(), libm::pow(10.0, -8.0 / 40.0));
    let b = Brush::voice();
    assert_eq!((b.fc, b.q, b.level), (2500.0, 0.6, 0.7));
    let t = HfTilt::new(9000.0, None);
    assert_eq!(
        (t.hat_hz, t.crash_hz, t.q),
        (9000.0, 9000.0, libm::sqrt(0.5))
    );
    assert_eq!(
        HfTilt::new(9000.0, Some(7000.0)).lowpass_hz("crash"),
        Some(7000.0)
    );
    assert_eq!(
        HfTilt::new(9000.0, Some(7000.0)).lowpass_hz("hat"),
        Some(9000.0)
    );
}

#[test]
fn the_kick_rests_on_the_keys_root_or_fifth() {
    assert_eq!(KickTuning::CENTRE_HZ, 50.0);
    assert_eq!(KickTuning::hz(None), None);
    // the key's root or fifth, whichever octave is nearest 50 Hz, between about 37 and 73 Hz
    for root in 0..128 {
        let hz = KickTuning::hz(Some(root)).unwrap();
        assert!(hz > 35.0 && hz < 75.0, "{root}: {hz}");
    }
}
