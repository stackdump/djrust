//! Offline loudness measurement as `SessionMix.report` reads it: ITU-R BS.1770-4 integrated
//! loudness (with K-weighting), EBU Tech 3342 loudness range and BS.1770-4 Annex 2 true peak,
//! under the free-function names this crate's mix report uses.
//!
//! Nothing is implemented here. There is one K-weighting, one gating and one range
//! ([`crate::musicality::loudness`]: `KWeighting`, `IntegratedLoudness`, ports of
//! `Musicality/Loudness.swift` and `IntegratedLoudness.swift`) and one true peak
//! ([`crate::fx::true_peak`], port of `Musicality/TruePeak.swift`, which the limiter runs too);
//! these are the same functions under flat names. `tests/loudness_fold.rs` holds each name to
//! the implementation behind it bit for bit.

use crate::musicality::loudness::IntegratedLoudness;

pub use crate::fx::true_peak::{
    dbtp, flat_coefficients, interpolated_peak, peak as true_peak, Detector as TruePeakDetector,
    DELAY as TP_DELAY, TAPS as TP_TAPS,
};
pub use crate::musicality::loudness::KWeighting;

/// `IntegratedLoudness.lufs`: integrated loudness of a stereo render, LUFS; −200 for silence.
pub fn lufs(left: &[f32], right: &[f32], sample_rate: f64) -> f64 {
    IntegratedLoudness::lufs(left, right, sample_rate)
}

/// `IntegratedLoudness.integrated(left:right:sampleRate:)`: LUFS, or None for silence.
/// `right` None = mono, played on both channels of a stereo deck, so counted twice.
pub fn integrated(left: &[f32], right: Option<&[f32]>, sample_rate: f64) -> Option<f64> {
    IntegratedLoudness::integrated(left, right, sample_rate)
}

/// The same over `n` frames of raw channels. Something shorter than one block is measured as
/// one block. `prime_frames`: run the filters over that many frames from the end first (a
/// loop, which plays round).
pub fn integrated_frames(
    left: &[f32],
    right: Option<&[f32]>,
    n: usize,
    sr: f64,
    prime_frames: usize,
) -> Option<f64> {
    IntegratedLoudness::integrated_count(left, right, n, sr, prime_frames)
}

/// `IntegratedLoudness.range`: EBU Tech 3342 loudness range, LU. 0 for silence or anything
/// shorter than one block.
pub fn range(left: &[f32], right: Option<&[f32]>, sr: f64) -> f64 {
    IntegratedLoudness::range(left, right, sr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const SR: f64 = 48_000.0;

    fn sine(hz: f64, amplitude: f64, seconds: f64, phase: f64) -> Vec<f32> {
        (0..(seconds * SR) as usize)
            .map(|i| {
                (amplitude * libm::sin(2.0 * core::f64::consts::PI * hz * i as f64 / SR + phase))
                    as f32
            })
            .collect()
    }

    fn db(x: f64) -> f64 {
        libm::pow(10.0, x / 20.0)
    }

    // LoudnessRangeTests.steadySineHasNoRange
    #[test]
    fn steady_sine_has_no_range() {
        let x = sine(1000.0, db(-20.0), 20.0, 0.0);
        let lra = range(&x, Some(&x), SR);
        assert!((0.0..0.2).contains(&lra), "lra {lra}");
    }

    // LoudnessRangeTests.twoLevelsTenDBApartReadTenLU
    #[test]
    fn two_levels_ten_db_apart_read_ten_lu() {
        let mut x = sine(1000.0, db(-20.0), 20.0, 0.0);
        x.extend(sine(1000.0, db(-30.0), 20.0, 0.0));
        let lra = range(&x, Some(&x), SR);
        assert!((lra - 10.0).abs() <= 1.0, "lra {lra}");
    }

    // LoudnessRangeTests.silenceAndShortRenderHaveNoRange
    #[test]
    fn silence_and_short_render_have_no_range() {
        let quiet = vec![0.0f32; (5.0 * SR) as usize];
        assert_eq!(range(&quiet, None, SR), 0.0);
        let short = sine(1000.0, 0.5, 2.0, 0.0);
        assert_eq!(range(&short, None, SR), 0.0);
    }

    // LoudnessRangeTests.quarterRateSineAt45DegreesPeaksBetweenSamples
    #[test]
    fn quarter_rate_sine_at_45_degrees_peaks_between_samples() {
        let x = sine(SR / 4.0, 1.0, 1.0, core::f64::consts::PI / 4.0);
        let sample_peak =
            20.0 * libm::log10(x.iter().map(|v| v.abs()).fold(0.0f32, f32::max) as f64);
        let tp = dbtp(&x, None);
        assert!(
            (tp - sample_peak - 3.0).abs() <= 0.3,
            "tp {tp} sample peak {sample_peak}"
        );
    }

    // LoudnessRangeTests.fullScale997HzReadsZeroDBTP
    #[test]
    fn full_scale_997_hz_reads_zero_dbtp() {
        let x = sine(997.0, 1.0, 1.0, 0.0);
        assert!(dbtp(&x, Some(&x)).abs() <= 0.2);
    }

    // LoudnessRangeTests.truePeakTakesTheLouderChannel
    #[test]
    fn true_peak_takes_the_louder_channel() {
        let l = sine(997.0, db(-12.0), 0.5, 0.0);
        let r = sine(997.0, db(-6.0), 0.5, 0.0);
        assert!((dbtp(&l, Some(&r)) + 6.0).abs() <= 0.2);
        assert_eq!(dbtp(&[0.0, 0.0, 0.0], None), -200.0);
    }

    // LoudnessRangeTests.detectorStreamsWithoutGrowingAndResets
    #[test]
    fn detector_streams_without_growing_and_resets() {
        let mut d = TruePeakDetector::new();
        for v in sine(997.0, 0.5, 0.1, 0.0) {
            d.process(v as f64);
        }
        assert!((20.0 * libm::log10(d.peak) - 20.0 * libm::log10(0.5)).abs() <= 0.2);
        d.reset();
        assert_eq!(d.peak, 0.0);
        assert_eq!(d.process(0.0), 0.0);
    }

    /// A 997 Hz sine at -20 dBFS peak reads about -20 LUFS on both channels (this K-weighting
    /// reads it 0.26 LU low, as the port's arithmetic is the Swift's; not a Swift literal).
    #[test]
    fn a_997_hz_sine_reads_its_level_in_lufs() {
        let x = sine(997.0, db(-20.0), 10.0, 0.0);
        let l = lufs(&x, &x, SR);
        assert!((l - (-20.0)).abs() < 0.5, "lufs {l}");
        assert_eq!(lufs(&vec![0.0; 48_000], &vec![0.0; 48_000], SR), -200.0);
    }
}
