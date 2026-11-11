//! Audio inputs as mixer strips (docs/HUB.md, "Audio inputs"): the gear's line out coming back
//! in, most likely through a mic-level jack with no pad (a field setup, 2026-11-03). Pure pieces:
//! the strip's settings with defaults for that case, channel selection, a DC-blocking high-pass,
//! and the watch that holds the CLIP lamp and decides when to show the "turn the gear down" hint.

use alloc::string::String;

use crate::fx::StripSettings;

/// The trim a new input starts at: line-level gear on a mic jack is hot.
pub const DEFAULT_TRIM_DB: f64 = -12.0;
/// The trim's range.
pub const TRIM_RANGE_DB: (f64, f64) = (-24.0, 24.0);
/// A raw sample at or above this is a clip (−0.5 dBFS).
pub const CLIP_LEVEL: f32 = 0.944;
/// Raw level above this (−3 dBFS) is running hot.
pub const HOT_LEVEL: f32 = 0.708;
/// How long the CLIP lamp stays lit after a clip, seconds.
pub const CLIP_HOLD_S: f64 = 2.0;
/// Signal above this (−40 dBFS post-trim) counts as "playing" for the clean-minute rule.
pub const SIGNAL_LEVEL: f32 = 0.01;
/// The hint goes away after this long of signal with no raw clip, seconds.
pub const CLEAN_S: f64 = 60.0;
/// A clip brings the hint back for this long, seconds.
pub const HINT_AFTER_CLIP_S: f64 = 30.0;

/// Which channel of a stereo input feeds the (mono) strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputChannel {
    /// The tip of a mono jack: what a stereo plug in a mono jack delivers.
    #[default]
    Left,
    Right,
    /// Both, summed at −3 dB.
    Sum,
}

impl InputChannel {
    pub const ALL: [InputChannel; 3] = [InputChannel::Left, InputChannel::Right, InputChannel::Sum];

    pub fn label(self) -> &'static str {
        match self {
            InputChannel::Left => "L",
            InputChannel::Right => "R",
            InputChannel::Sum => "L+R",
        }
    }

    /// One frame of a stereo (or mono: `r` = `l`) input, as the strip takes it.
    pub fn select(self, l: f32, r: f32) -> f32 {
        match self {
            InputChannel::Left => l,
            InputChannel::Right => r,
            InputChannel::Sum => (l + r) * core::f32::consts::FRAC_1_SQRT_2,
        }
    }
}

/// An input strip's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct InputSettings {
    /// What is plugged in ("TD-3", "PD705").
    pub label: String,
    pub trim_db: f64,
    pub channel: InputChannel,
    pub volume: f64,
    pub mute: bool,
    pub pfl: bool,
    pub eq: StripSettings,
    /// The aux send, 0...1 (docs/AUX.md).
    pub aux: f64,
}

impl InputSettings {
    pub fn new(label: &str) -> InputSettings {
        InputSettings {
            label: label.into(),
            trim_db: DEFAULT_TRIM_DB,
            channel: InputChannel::Left,
            volume: 0.8,
            mute: false,
            pfl: false,
            eq: StripSettings::default(),
            aux: 0.0,
        }
    }
}

/// dB → linear gain.
pub fn gain_of_db(db: f64) -> f64 {
    libm::pow(10.0, db / 20.0)
}

/// The trim knob's position (0…1) for a trim in dB, and back.
pub fn trim_to_knob(db: f64) -> f64 {
    let (lo, hi) = TRIM_RANGE_DB;
    ((db - lo) / (hi - lo)).clamp(0.0, 1.0)
}
pub fn knob_to_trim(v: f64) -> f64 {
    let (lo, hi) = TRIM_RANGE_DB;
    lo + v.clamp(0.0, 1.0) * (hi - lo)
}

/// A one-pole DC blocker (`y = x − x₁ + R·y₁`), corner about `hz`: keeps a mic jack's bias and
/// thumps out of the mix and the meters. Real-time safe.
#[derive(Clone, Debug)]
pub struct DcBlock {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlock {
    pub fn new(sample_rate: f64, hz: f64) -> DcBlock {
        DcBlock {
            r: libm::exp(-2.0 * core::f64::consts::PI * hz / sample_rate) as f32,
            x1: 0.0,
            y1: 0.0,
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for x in buf.iter_mut() {
            let y = *x - self.x1 + self.r * self.y1;
            self.x1 = *x;
            // flush denormals
            self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
            *x = self.y1;
        }
    }
}

/// The CLIP lamp and the hint, from what the strip saw since the last look.
#[derive(Clone, Debug, Default)]
pub struct InputWatch {
    clip_until: f64,
    hint_until: f64,
    clean_s: f64,
    last: Option<f64>,
    /// The clean minute happened once: the standing hint is gone for good.
    settled: bool,
    pub clips: u64,
}

/// What the strip shows now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputView {
    /// The CLIP lamp (held [`CLIP_HOLD_S`]).
    pub clip: bool,
    /// Clipping *after* the trim only: turn the trim down, not the gear.
    pub trim_clip: bool,
    pub hot: bool,
    /// Show "turn the gear's output down / centre-pan the kit".
    pub hint: bool,
}

impl InputWatch {
    /// What arrived since the last call, at host time `now` (seconds): whether a raw clip
    /// happened, whether the post-trim signal clipped, the raw peak and the post-trim peak.
    pub fn update(
        &mut self,
        now: f64,
        raw_clip: bool,
        post_clip: bool,
        raw_peak: f32,
        post_peak: f32,
    ) -> InputView {
        let dt = self.last.map_or(0.0, |l| (now - l).max(0.0));
        self.last = Some(now);
        if raw_clip {
            self.clips += 1;
            self.clip_until = now + CLIP_HOLD_S;
            self.hint_until = now + HINT_AFTER_CLIP_S;
            self.clean_s = 0.0;
        } else if post_peak > SIGNAL_LEVEL {
            self.clean_s += dt;
            if self.clean_s >= CLEAN_S {
                self.settled = true;
            }
        }
        InputView {
            clip: now < self.clip_until,
            trim_clip: post_clip && !raw_clip,
            hot: raw_peak > HOT_LEVEL,
            hint: !self.settled || now < self.hint_until,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn channel_selection() {
        assert_eq!(InputChannel::Left.select(0.5, -0.5), 0.5);
        assert_eq!(InputChannel::Right.select(0.5, -0.5), -0.5);
        assert!((InputChannel::Sum.select(1.0, 1.0) - core::f32::consts::SQRT_2).abs() < 1e-6);
        assert_eq!(InputChannel::Sum.select(0.5, -0.5), 0.0);
    }

    #[test]
    fn trim_knob_round_trip_and_default() {
        assert_eq!(trim_to_knob(DEFAULT_TRIM_DB), 0.25);
        assert!((knob_to_trim(trim_to_knob(7.5)) - 7.5).abs() < 1e-12);
        assert!((gain_of_db(-12.0) - 0.251_188_643).abs() < 1e-6);
    }

    #[test]
    fn dc_block_removes_offset_and_passes_audio() {
        let sr = 48_000.0;
        let mut dc = DcBlock::new(sr, 20.0);
        // a 0.3 DC offset under a 1 kHz tone
        let n = 48_000;
        let mut buf: alloc::vec::Vec<f32> = (0..n)
            .map(|i| {
                0.3 + 0.5 * libm::sin(2.0 * core::f64::consts::PI * 1000.0 * i as f64 / sr) as f32
            })
            .collect();
        dc.process(&mut buf);
        let tail = &buf[n / 2..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        let peak = tail.iter().fold(0f32, |p, v| p.max(v.abs()));
        assert!(mean.abs() < 1e-3, "mean {mean}");
        assert!((peak - 0.5).abs() < 0.01, "the tone passes: {peak}");
        let mut silent = vec![0f32; 64];
        dc.process(&mut silent);
    }

    #[test]
    fn clip_lamp_holds_and_hint_follows_the_clean_minute() {
        let mut w = InputWatch::default();
        let v = w.update(0.0, false, false, 0.1, 0.05);
        assert!(!v.clip && v.hint, "the standing hint shows from the start");
        let v = w.update(1.0, true, true, 1.0, 0.5);
        assert!(v.clip && !v.trim_clip && v.hot);
        assert!(w.update(2.9, false, false, 0.1, 0.05).clip, "held");
        assert!(!w.update(3.1, false, false, 0.1, 0.05).clip, "and let go");
        // a clean minute of signal settles it
        let mut t = 3.1;
        while t < 70.0 {
            t += 0.5;
            w.update(t, false, false, 0.2, 0.05);
        }
        assert!(!w.update(t + 0.5, false, false, 0.2, 0.05).hint);
        // a clip brings it back for a while
        assert!(w.update(t + 1.0, true, false, 1.0, 0.3).hint);
        assert!(
            !w.update(t + 1.0 + HINT_AFTER_CLIP_S + 1.0, false, false, 0.2, 0.05)
                .hint
        );
        // silence does not count as clean
        let mut q = InputWatch::default();
        for k in 0..200 {
            q.update(k as f64, false, false, 0.0, 0.0);
        }
        assert!(q.update(201.0, false, false, 0.0, 0.0).hint);
        // over only after the trim
        assert!(
            InputWatch::default()
                .update(0.0, false, true, 0.5, 1.0)
                .trim_clip
        );
    }
}
