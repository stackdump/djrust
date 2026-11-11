//! Port of `Waveform.swift`: min / max peaks of a buffer, one pair per display column
//! (channels mixed to mono), and where a deck's markers sit along it.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use super::beat_grid::BeatGrid;
use super::turntable::SampleBuffer;

/// The whole-track strip of a deck (`WaveformOverview`). Pure data.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformOverview {
    pub columns: usize,
    /// `2 * columns` floats: min0, max0, min1, max1, …
    pub peaks: Vec<f32>,
    pub length: usize,
    pub sample_rate: f64,
}

impl WaveformOverview {
    pub fn new(buffer: &SampleBuffer, columns: usize) -> WaveformOverview {
        let n = columns.max(1);
        WaveformOverview {
            columns: n,
            length: buffer.length(),
            sample_rate: buffer.sample_rate,
            peaks: compute(buffer, 0, buffer.length(), n),
        }
    }

    pub fn peak(&self, column: usize) -> (f32, f32) {
        (self.peaks[column * 2], self.peaks[column * 2 + 1])
    }

    /// Peaks of the window `[start, end)` (in samples, clamped) at `columns` resolution: the
    /// zoomed view around the playhead. Computed straight from the samples.
    pub fn window(buffer: &SampleBuffer, start: i64, end: i64, columns: usize) -> Vec<f32> {
        let len = buffer.length() as i64;
        let s = start.clamp(0, len);
        let e = end.min(len).max(s);
        compute(buffer, s as usize, e as usize, columns.max(1))
    }
}

fn compute(buffer: &SampleBuffer, from: usize, to: usize, columns: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; columns * 2];
    let count = to - from;
    if count == 0 {
        return out;
    }
    let mono = buffer.right.is_none();
    let left = &buffer.left;
    let right = buffer.right();
    for c in 0..columns {
        let a = from + c * count / columns;
        let mut b = from + (c + 1) * count / columns;
        if b <= a {
            b = to.min(a + 1);
        }
        let mut lo = f32::MAX;
        let mut hi = -f32::MAX;
        for i in a..b {
            let v = if mono {
                left[i]
            } else {
                (left[i] + right[i]) * 0.5
            };
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        if lo > hi {
            lo = 0.0;
            hi = 0.0;
        }
        out[c * 2] = lo;
        out[c * 2 + 1] = hi;
    }
    out
}

/// Where a deck's markers sit along its waveform, as fractions (0…1) of the track
/// (`WaveformOverlay`).
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformOverlay {
    pub playhead: f64,
    /// The active (or armed) loop.
    pub loop_range: Option<(f64, f64)>,
    pub loop_active: bool,
    pub cues: BTreeMap<i64, f64>,
    /// Bar lines from the beat grid (every 4 beats); empty without a grid or if too dense to
    /// read.
    pub bar_lines: Vec<f64>,
}

impl WaveformOverlay {
    /// `Deck.waveformOverlay` for a sample deck: `position` is the turntable's unwrapped
    /// playhead, `length` the loaded length in samples.
    pub fn of_sample_deck(
        length: usize,
        position: f64,
        loop_start: Option<f64>,
        loop_end: Option<f64>,
        looping: bool,
        hot_cues: &BTreeMap<i64, f64>,
        grid: Option<&BeatGrid>,
        sample_rate: f64,
    ) -> Option<WaveformOverlay> {
        if length == 0 {
            return None;
        }
        let len = length as f64;
        let mut p = libm::fmod(position, len);
        if p < 0.0 {
            p += len;
        }
        let mut loop_range = None;
        if let (Some(s), Some(e)) = (loop_start, loop_end) {
            if e > s {
                loop_range = Some((
                    0.0f64.max(1.0f64.min(s / len)),
                    0.0f64.max(1.0f64.min(e / len)),
                ));
            }
        }
        let mut bars = Vec::new();
        if let Some(g) = grid {
            let bar = g.samples_per_beat(sample_rate) * 4.0;
            if bar > 0.0 && len / bar <= 400.0 {
                let mut x = g.offset;
                while x < len {
                    bars.push(x / len);
                    x += bar;
                }
            }
        }
        Some(WaveformOverlay {
            playhead: p / len,
            loop_range,
            loop_active: looping,
            cues: hot_cues
                .iter()
                .map(|(k, v)| (*k, 0.0f64.max(1.0f64.min(*v / len))))
                .collect(),
            bar_lines: bars,
        })
    }
}
