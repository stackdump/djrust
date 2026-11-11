//! `RenderedAudio` and the WAV writer (`Session/SessionPlayer.swift`), plus the level meter
//! `SpecRender.swift` adds to `RenderedAudio`.

use alloc::vec::Vec;

/// Planar float audio (`RenderedAudio`).
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedAudio {
    pub sample_rate: f64,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl RenderedAudio {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// `RenderedAudio.levels`: peak and RMS over both channels, in dBFS.
    pub fn levels(&self) -> (f64, f64) {
        let mut peak = 0.0f32;
        let mut sum = 0.0f64;
        for ch in [&self.left, &self.right] {
            for &x in ch.iter() {
                peak = peak.max(x.abs());
                sum += x as f64 * x as f64;
            }
        }
        let n = core::cmp::max(1, self.left.len() + self.right.len());
        fn db(v: f64) -> f64 {
            if v > 0.0 {
                20.0 * libm::log10(v)
            } else {
                f64::NEG_INFINITY
            }
        }
        (db(peak as f64), db(libm::sqrt(sum / n as f64)))
    }
}

/// `WAV.encode`: 32-bit float or 16-bit PCM, interleaved stereo RIFF ([`crate::wav::wav_bytes`]).
pub fn encode_wav(audio: &RenderedAudio, float32: bool) -> Vec<u8> {
    crate::wav::wav_bytes(&audio.left, &audio.right, audio.sample_rate, float32)
}
