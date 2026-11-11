//! The pure side of `SamplerInstrument` (`Audio/Sampler.swift`): the reference phrases a
//! sampled sound is measured on, the level it is brought to, and the rules that turn a
//! sound away. The measuring itself (play the phrase, take the RMS) is the host's
//! (`crate::host::sampler`); everything it decides from the numbers is here, so it is the same
//! on every backend.

/// One note of a reference phrase: MIDI key, onset (s), length (s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhraseNote {
    pub note: u8,
    pub at: f64,
    pub seconds: f64,
}

const fn pn(note: u8, at: f64, seconds: f64) -> PhraseNote {
    PhraseNote { note, at, seconds }
}

/// The phrase levels are measured on: a strummed C chord, then single notes (the same one the
/// synth voices are calibrated against). `SamplerInstrument.referencePhrase`.
pub const REFERENCE_PHRASE: [PhraseNote; 7] = [
    pn(48, 0.0, 0.5),
    pn(52, 0.02, 0.5),
    pn(55, 0.04, 0.5),
    pn(60, 0.06, 0.5),
    pn(64, 0.5, 0.25),
    pn(67, 0.75, 0.25),
    pn(72, 1.0, 0.5),
];
/// Seconds the reference phrase is rendered for (`Int(2 * sr)` frames).
pub const REFERENCE_SECONDS: f64 = 2.0;

/// RMS of the lead synth voice on [`REFERENCE_PHRASE`]: what every sampled instrument is
/// brought to, so a pick from any pool sits in the mix. `targetRMSdB`.
pub const TARGET_RMS_DB: f64 = -27.5;
/// Worst pitch error (cents, in whatever octave it sounds) a sound may have to be played.
/// `maxDetuneCents`.
pub const MAX_DETUNE_CENTS: f64 = 30.0;
/// Below this on the reference phrase a sound is treated as silent (missing samples).
/// `silentRMSdB`.
pub const SILENT_RMS_DB: f64 = -70.0;
/// RMS of beats' synth kit on [`kit_phrase`] at velocity 100: what a sampled kit is brought
/// to. `kitTargetRMSdB`.
pub const KIT_TARGET_RMS_DB: f64 = -15.0;
/// The kit voices that must sound for a kit to play: kick, snare, closed hat. `kitVoices`.
pub const KIT_VOICES: [u8; 3] = [36, 38, 42];
/// Seconds [`kit_phrase`] is rendered for.
pub const KIT_SECONDS: f64 = 2.25;
/// Seconds one kit voice is auditioned for (`phraseRMS([(note, 0, 0.3)], seconds: 0.3)`).
pub const KIT_VOICE_SECONDS: f64 = 0.3;

/// The phrase a drum kit is measured on: a bar at 120 BPM of four-on-the-floor kick,
/// sixteenth hats and a backbeat snare, on the GM keys the drum lanes play. `kitPhrase`.
pub fn kit_phrase() -> alloc::vec::Vec<PhraseNote> {
    let mut out = alloc::vec::Vec::new();
    for i in 0..16u32 {
        let at = i as f64 * 0.125;
        out.push(pn(42, at, 0.1));
        if i % 4 == 0 {
            out.push(pn(36, at, 0.3));
        }
        if i % 8 == 4 {
            out.push(pn(38, at, 0.3));
        }
    }
    out
}

/// 10·log10 of a mean square, floored the way `normalize` floors it (`max(x, 1e-20)`).
pub fn rms_db(sum_of_squares: f64, frames: usize) -> f64 {
    10.0 * libm::log10((sum_of_squares / frames.max(1) as f64).max(1e-20))
}

/// The gain that brings a sound measured at `rms_db` to `target_db`, limited to
/// 0.02…20 (`gain = min(20, max(0.02, 10^((target − rms)/20)))`).
pub fn gain_for(rms_db: f64, target_db: f64) -> f64 {
    libm::pow(10.0, (target_db - rms_db) / 20.0).clamp(0.02, 20.0)
}

/// `normalize`'s verdict on a tonal sound: not silent, the held A3 still sounded
/// (`level > 0`) and its pitch error within [`MAX_DETUNE_CENTS`]. With `tuning` false (a
/// local-only bank: its importer tuned it) the level alone decides.
pub fn tonal_fit(rms_db: f64, tuning: bool, level: f64, cents: f64) -> bool {
    if !tuning {
        return rms_db > SILENT_RMS_DB;
    }
    rms_db > SILENT_RMS_DB && level > 0.0 && cents.abs() <= MAX_DETUNE_CENTS
}

/// `normalizeKit`'s verdict: the phrase and the quietest of kick / snare / closed hat are
/// both audible.
pub fn kit_fit(phrase_rms_db: f64, quietest_db: f64) -> bool {
    phrase_rms_db > SILENT_RMS_DB && quietest_db > SILENT_RMS_DB
}

/// `normalizeKit`'s `quietest`: Swift starts the running minimum at 0.0 (dB) and only ever
/// lowers it, so it is `min(0, r…)`.
pub fn quietest_of(levels: impl IntoIterator<Item = f64>) -> f64 {
    levels.into_iter().fold(0.0, f64::min)
}

/// The velocity scale that moves a sampled hit by `db`: a sound bank's velocity curve is
/// about 40 dB per decade of velocity, so × 10^(dB/40). `DrumKits.velocityScale`.
pub fn velocity_scale(db: f64) -> f64 {
    libm::pow(10.0, db / 40.0)
}
