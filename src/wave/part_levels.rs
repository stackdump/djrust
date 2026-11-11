//! How loud a part plays on its own synth voice, the reference a sampled instrument's part
//! level is taken against: `SamplerPartLevels` (the probes and the group's plain voice,
//! `Audio/Sampler.swift`) and `ownSynthPartDB` / `ownPartLevelDB` (the lane's own voice,
//! `WaveRunner.swift`).
//!
//! A sampler measures itself on these probes when it loads (`SamplerInstrument
//! .measurePartLevels`: the instrument against the group's plain synth voice). The runner
//! then moves that level to be against the lane's *own* voice, which the instrument table
//! shapes (`dark-pad` is 7.7 dB over the plain harmony voice), so a sampled part lands where
//! its own trimmed synth voice would. The synth side is here, pure and deterministic; the
//! measuring of a sampler is the host's (the AUSampler lifecycle is Apple-only, see
//! PORTING.md), and reaches the runner through `WaveOptions::part_level`.
//!
//! Swift keeps the two reference levels in process-wide tables behind a mutex; the engine has
//! no lock, so each call renders its probe (a second of audio at most) and the runner asks
//! once per sampled lane while a project is prepared.

use alloc::vec::Vec;

use super::runner::{audition_lane, own_lane, Lane};
use super::synth::{tonal_voice, VoiceSpec};
use crate::musicality::loudness::KWeighting;

/// The probes (`SamplerPartLevels`).
pub struct SamplerPartLevels;

impl SamplerPartLevels {
    /// The parts measured.
    pub const GROUPS: [&'static str; 4] = ["bass", "harmony", "melody", "arp"];
    /// Passes per probe, averaged by the sampler: round-robin guitars and modulated patches
    /// vary between plays.
    pub const PASSES: usize = 3;
    /// Bumped when a probe changes, so cached levels are measured again. (2: the pad chord
    /// held a bar, not two held half a bar each.)
    pub const VERSION: i64 = 2;

    /// `probeGroup(for:)`: the probe group for a part's group: a pad plays like harmony,
    /// anything else without a probe of its own like melody (as `Loudness::sampled_part_level`
    /// falls back).
    pub fn probe_group(group: &str) -> &'static str {
        if group == "pad" {
            return "harmony";
        }
        Self::GROUPS
            .iter()
            .copied()
            .find(|g| *g == group)
            .unwrap_or("melody")
    }

    /// `probe(_:)`: the probe for `group` (one of `GROUPS`) as `(notes, seconds)`, each note
    /// `(midi note, start s, length s)`: a pad chord held a bar at 120 BPM, bass eighths, a lead
    /// line of mixed lengths, a sixteenth-note arpeggio.
    pub fn probe(group: &str) -> (Vec<(i64, f64, f64)>, f64) {
        match group {
            "harmony" => (
                alloc::vec![(48, 0.0, 1.9), (52, 0.0, 1.9), (55, 0.0, 1.9)],
                2.0,
            ),
            "bass" => (
                [36, 36, 43, 41]
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (*n, i as f64 * 0.25, 0.2))
                    .collect(),
                1.0,
            ),
            "arp" => (
                [60, 64, 67, 72, 67, 64, 60, 64]
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (*n, i as f64 * 0.125, 0.1))
                    .collect(),
                1.0,
            ),
            _ => (
                alloc::vec![
                    (72, 0.0, 0.375),
                    (74, 0.375, 0.125),
                    (76, 0.5, 0.25),
                    (79, 0.75, 0.25)
                ],
                1.0,
            ),
        }
    }

    /// `synthLevelDB(group:sampleRate:)`: the K-weighted level (dB) of `group`'s plain synth
    /// voice (`tonalVoice(instrument: nil, group:)`) on its probe at `sample_rate`.
    pub fn synth_level_db(group: &str, sample_rate: f64) -> f64 {
        let (notes, seconds) = Self::probe(group);
        let a = audition_lane(
            own_lane(&tonal_voice(None, Some(group)), sample_rate),
            &notes,
            sample_rate,
            seconds,
            100,
        );
        k_weighted_db(&a, sample_rate)
    }
}

/// 10 log10 of the mean square of `a` after the K-weighting (the meter the probes read).
fn k_weighted_db(a: &[f32], sample_rate: f64) -> f64 {
    let mut k = KWeighting::new(sample_rate);
    let mut sum = 0.0;
    for x in a {
        let y = k.process(*x as f64);
        sum += y * y;
    }
    10.0 * libm::log10((sum / a.len().max(1) as f64).max(1e-20))
}

/// `ownSynthPartDB`: what a tonal lane's own synth voice measures on its group's part probe
/// (K-weighted dB) at `sample_rate`, shaped as `buildLanes` shapes it. `shape` is that
/// shaping applied to the fresh six-voice lane (`.subtractive`, `.unison`, `.subOctave`); the
/// runner passes none until it applies them.
///
/// A slot's measured part level is the sampler against the group's plain voice, but the part
/// it stands in for plays the instrument's own voice: `dark-pad` from the instrument table is
/// 7.7 dB over the plain harmony voice, `sub-bass` 6.2 over the plain bass, and unison /
/// filter envelopes move them further. The slot's gain and the loudness probe's stand-in take
/// the sampler against this level ([`own_part_level_db`]).
pub fn own_synth_part_db(
    voice: &VoiceSpec,
    group: &str,
    sample_rate: f64,
    shape: &dyn Fn(&mut Lane),
) -> f64 {
    let (notes, seconds) = SamplerPartLevels::probe(SamplerPartLevels::probe_group(group));
    let mut lane = own_lane(voice, sample_rate);
    shape(&mut lane);
    let a = audition_lane(lane, &notes, sample_rate, seconds, 100);
    k_weighted_db(&a, sample_rate)
}

/// `ownPartLevelDB`: a slot's measured part level (`SamplerBank.slotPartLevelDB`: the sampler
/// against the group's plain synth voice) moved to be against the lane's own synth voice
/// ([`own_synth_part_db`]); None stays None.
pub fn own_part_level_db(
    measured: Option<f64>,
    voice: &VoiceSpec,
    group: &str,
    bank_rate: f64,
    shape: &dyn Fn(&mut Lane),
) -> Option<f64> {
    let measured = measured?;
    let plain = SamplerPartLevels::synth_level_db(SamplerPartLevels::probe_group(group), bank_rate);
    let own = own_synth_part_db(voice, group, bank_rate, shape);
    Some(measured + plain - own)
}
