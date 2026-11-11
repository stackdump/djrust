//! Ports of `Musicality/Loudness.swift`, `IntegratedLoudness.swift` and `TruePeak.swift`:
//! the per-track gain (`.loudness`), ITU-R BS.1770-4 integrated loudness and EBU Tech 3342
//! loudness range, and BS.1770-4 Annex 2 true peak.
//!
//! This is the crate's one K-weighting and one integrated-loudness core ([`KWeighting`],
//! [`IntegratedLoudness`]); [`crate::loudness`] is the same under flat function names. The
//! true peak is [`crate::fx::true_peak`]'s (the limiter's): [`TruePeak`] and
//! [`TruePeakDetector`] here are Swift-shaped names for it.
//!
//! `Loudness::measure` plays the probe on `WaveRunner` (`Master::Linear`, the instrument table,
//! the voice fixes the deck plays, the deck's sub-octave and tilt, the sampled parts' levels),
//! as Swift's does; `WaveRunner::prepare` runs `gain` under `.loudness`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::fx::true_peak;
use crate::musicality::fixes::Fixes;
use crate::musicality::mix_balance::MixBalance;
use crate::musicality::mix_targets::{MixTargets, REFERENCE_LUFS};
use crate::musicality::synth_voice::HfTilt;
use crate::pflow::Project;
use crate::wave::instruments::InstrumentTable;
use crate::wave::runner::{Master, WaveOptions, WaveRunner};
use crate::wave::synth::biquad;

/// ITU-R BS.1770's K-weighting, from its analog prototypes so it works at any rate: a high
/// shelf (+4 dB above ~1.7 kHz) then a second-order highpass at 38 Hz.
#[derive(Clone, Debug)]
pub struct KWeighting {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    c0: f64,
    c1: f64,
    c2: f64,
    d1: f64,
    d2: f64,
    s1: f64,
    s2: f64,
    h1: f64,
    h2: f64,
}

impl KWeighting {
    pub fn new(sample_rate: f64) -> KWeighting {
        let sr = sample_rate;
        // RBJ high shelf, f0 1681.97 Hz, +3.9998 dB, Q 0.7072
        let a = libm::pow(10.0, 3.99984 / 40.0);
        let w = 2.0 * core::f64::consts::PI * 1681.974 / sr;
        let alpha = libm::sin(w) / (2.0 * 0.7071752);
        let c = libm::cos(w);
        let sa = 2.0 * libm::sqrt(a) * alpha;
        let a0 = (a + 1.0) - (a - 1.0) * c + sa;
        let hp = biquad("highpass", 38.13, 0.5003, sr);
        KWeighting {
            b0: a * ((a + 1.0) + (a - 1.0) * c + sa) / a0,
            b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * c) / a0,
            b2: a * ((a + 1.0) + (a - 1.0) * c - sa) / a0,
            a1: 2.0 * ((a - 1.0) - (a + 1.0) * c) / a0,
            a2: ((a + 1.0) - (a - 1.0) * c - sa) / a0,
            c0: hp[0],
            c1: hp[1],
            c2: hp[2],
            d1: hp[3],
            d2: hp[4],
            s1: 0.0,
            s2: 0.0,
            h1: 0.0,
            h2: 0.0,
        }
    }

    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        let z = self.c0 * y + self.h1;
        self.h1 = self.c1 * y - self.d1 * z + self.h2;
        self.h2 = self.c2 * y - self.d2 * z;
        z
    }
}

/// Per-track gain so every track plays at about the same loudness (`.loudness`).
pub struct Loudness;

impl Loudness {
    /// The fixes that change how the voices themselves sound (and so how loud they are): the
    /// probe plays those of the deck's, so the gain is measured on the voices the deck plays
    /// (`voiceFixes`).
    pub const VOICE_FIXES: Fixes =
        Fixes(Fixes::SUBTRACTIVE.0 | Fixes::UNISON.0 | Fixes::SUB_OCTAVE.0 | Fixes::HF_TILT.0);

    /// Probe length and rate: two bars of the loudest section, at a low rate.
    pub const PROBE_BARS: i64 = 2;
    pub const PROBE_RATE: f64 = 8_000.0;

    /// `targetDB`: loudness the busiest section is brought to, K-weighted mean square of the
    /// summed voices after the master gain.
    pub fn target_db() -> f64 {
        REFERENCE_LUFS + MixTargets::shared().probe_offset_db()
    }

    /// `targetDB(genre:)`: its family's integrated target plus the same probe offset.
    pub fn target_db_for(genre: Option<&str>) -> f64 {
        MixBalance::target_lufs(genre) + MixTargets::shared().probe_offset_db()
    }

    /// The gain stays within `loudness.gainRange` (min, max), linear.
    pub fn range() -> (f64, f64) {
        MixTargets::shared().gain_range()
    }

    /// How much quieter a part plays on a sampled instrument than on its synth voice (dB, by
    /// group): the fallback for a slot with no measured part level of its own.
    pub fn sampled_part_db(group: &str) -> f64 {
        match group {
            "bass" => -4.1,
            "harmony" => -5.0,
            _ => -1.7,
        }
    }

    /// The linear level the probe plays a sampled `group` part's synth voice at.
    pub fn sampled_part_level(group: &str, measured_db: Option<f64>) -> f64 {
        libm::pow(
            10.0,
            measured_db.unwrap_or_else(|| Self::sampled_part_db(group)) / 20.0,
        )
    }

    /// `gain(for:fixes:balance:sampled:subOctave:hfTilt:genre:sectionGain:)`: the gain for
    /// `project` (1 when it plays nothing), measured with the `VOICE_FIXES` among `fixes` on.
    /// `balance`: the parts' trims to play them with (`MixBalance::trims`). `sampled`: net id -> a
    /// level the probe plays that net's part at (the parts the deck plays on samplers, at each
    /// slot's measured part level and gain: `WaveRunner::prepare`). `sub_octave`: the deck's
    /// `WaveOptions::sub_octave` (the genre's), so the probe hears the sub the deck plays.
    /// `hf_tilt`: the deck's tilt, likewise its hats. `genre`: the deck's, for its family's
    /// target (`target_db_for`). `section_gain`: the linear gain the deck plays the probed section
    /// at (`.energy`'s `EnergyCurve` at `busiest_start`; 1 without it or where the busiest
    /// section is the curve's peak).
    #[allow(clippy::too_many_arguments)]
    pub fn gain(
        project: &Project,
        fixes: Fixes,
        balance: Option<&BTreeMap<String, f64>>,
        sampled: &BTreeMap<String, f64>,
        sub_octave: bool,
        hf_tilt: Option<HfTilt>,
        genre: Option<&str>,
        section_gain: f64,
    ) -> f64 {
        let Some(measured) = Self::measure_fixed(
            project,
            None,
            fixes,
            balance,
            sampled,
            sub_octave,
            hf_tilt,
            Self::PROBE_RATE,
        ) else {
            return 1.0;
        };
        let db = measured + 20.0 * libm::log10(section_gain.max(1e-6));
        let g = libm::pow(10.0, (Self::target_db_for(genre) - db) / 20.0);
        let (lo, hi) = Self::range();
        g.max(lo).min(hi)
    }

    /// `measure(_:start:fixes:balance:sampled:subOctave:hfTilt:sampleRate:)`: [`measure`]
    /// (Self::measure) with the voices `fixes` shape (`VOICE_FIXES` among them), the parts'
    /// trims `balance`, the sampled parts' stand-in levels and the deck's sub-octave and tilt.
    #[allow(clippy::too_many_arguments)]
    pub fn measure_fixed(
        project: &Project,
        start: Option<i64>,
        fixes: Fixes,
        balance: Option<&BTreeMap<String, f64>>,
        sampled: &BTreeMap<String, f64>,
        sub_octave: bool,
        hf_tilt: Option<HfTilt>,
        probe_rate: f64,
    ) -> Option<f64> {
        let mut o = WaveOptions {
            musicality: fixes & Self::VOICE_FIXES,
            probe_levels: sampled.clone(),
            sub_octave,
            hf_tilt,
            ..WaveOptions::default()
        };
        if let Some(b) = balance {
            o.musicality |= Fixes::BALANCE;
            o.track_balance = Some(b.clone());
        }
        Self::measure_with(project, start, probe_rate, o)
    }

    /// `measure`: K-weighted mean square (dB) of the voices, times the master gain, over the
    /// probe: two bars from `start` (a step), else from the busiest section's start.
    pub fn measure(project: &Project, start: Option<i64>, probe_rate: f64) -> Option<f64> {
        Self::measure_with(project, start, probe_rate, WaveOptions::default())
    }

    /// `measure` with extra runner options (the instrument table and the linear master are
    /// set here, as in Swift).
    pub fn measure_with(
        project: &Project,
        start: Option<i64>,
        probe_rate: f64,
        mut o: WaveOptions,
    ) -> Option<f64> {
        let start = start.unwrap_or_else(|| Self::busiest_start(project));
        o.instruments = Some(InstrumentTable::shared());
        o.master = Master::Linear;
        let mut runner = WaveRunner::new(probe_rate, o.clone());
        runner.load_prepared(WaveRunner::prepare(project, probe_rate, &o, start));
        runner.play();
        let tempo = if project.tempo > 0.0 {
            project.tempo
        } else {
            120.0
        };
        let frames = (Self::PROBE_BARS as f64 * 4.0 * 60.0 / tempo * probe_rate) as usize;
        let mut buf = vec![0.0f32; 512];
        let mut hp = KWeighting::new(probe_rate);
        let mut sum = 0.0;
        let mut n = 0usize;
        while n < frames {
            let k = 512.min(frames - n);
            runner.process(&mut buf[..k], None, k);
            for i in 0..k {
                let y = hp.process(buf[i] as f64 * o.master_gain);
                sum += y * y;
            }
            n += k;
            if runner.stopped() {
                break;
            }
        }
        if n == 0 || sum <= 0.0 {
            return None;
        }
        Some(10.0 * libm::log10(sum / n as f64))
    }

    /// `busiestStart`: where the full arrangement plays, by the section names beats'
    /// blueprints use: the first drop, else chorus, solo, verse, buildup; else the longest
    /// section (0 for a loop).
    pub fn busiest_start(project: &Project) -> i64 {
        if project.structure.is_empty() {
            return 0;
        }
        let mut starts: Vec<(&str, i64, i64)> = Vec::new();
        let mut t = 0;
        for s in &project.structure {
            starts.push((s.name.as_str(), t, s.steps));
            t += s.steps;
        }
        for name in ["drop", "chorus", "solo", "verse", "buildup"] {
            if let Some(s) = starts.iter().find(|s| s.0 == name) {
                return s.1;
            }
        }
        // `max(by: <)` keeps the first of equal maxima
        let mut best = starts[0];
        for s in &starts[1..] {
            if best.2 < s.2 {
                best = *s;
            }
        }
        best.1
    }
}

/// ITU-R BS.1770-4 integrated loudness, in LUFS, and EBU Tech 3342 loudness range.
pub struct IntegratedLoudness;

impl IntegratedLoudness {
    /// Integrated loudness of a stereo render, LUFS; −200 for silence.
    pub fn lufs(left: &[f32], right: &[f32], sample_rate: f64) -> f64 {
        Self::integrated(left, Some(right), sample_rate).unwrap_or(-200.0)
    }

    /// Integrated loudness, LUFS, or None for silence. `right` None = mono, played on both
    /// channels of a stereo deck, so counted twice.
    pub fn integrated(left: &[f32], right: Option<&[f32]>, sample_rate: f64) -> Option<f64> {
        let n = match right {
            Some(r) => left.len().min(r.len()),
            None => left.len(),
        };
        Self::integrated_count(left, right, n, sample_rate, 0)
    }

    /// The same over `n` frames of raw channels. Something shorter than one block is
    /// measured as one block. `prime_frames`: run the filters over that many frames from the
    /// end first (a loop, which plays round).
    pub fn integrated_count(
        left: &[f32],
        right: Option<&[f32]>,
        n: usize,
        sr: f64,
        prime_frames: usize,
    ) -> Option<f64> {
        if n == 0 {
            return None;
        }
        let sub = ((sr / 10.0) as usize).max(1);
        let (subs, acc) = Self::sub_blocks(left, right, n, sr, prime_frames);
        let blocks: Vec<f64> = if subs.len() < 4 {
            // shorter than a block: the whole thing is one
            vec![(subs.iter().sum::<f64>() + acc) / n as f64]
        } else {
            (3..subs.len())
                .map(|j| (subs[j - 3] + subs[j - 2] + subs[j - 1] + subs[j]) / (4 * sub) as f64)
                .collect()
        };
        let loudness = |ms: f64| -0.691 + 10.0 * libm::log10(ms);
        let absolute: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&b| b > 0.0 && loudness(b) > -70.0)
            .collect();
        if absolute.is_empty() {
            return None;
        }
        let relative = loudness(absolute.iter().sum::<f64>() / absolute.len() as f64) - 10.0;
        let gated: Vec<f64> = absolute
            .iter()
            .copied()
            .filter(|&b| loudness(b) > relative)
            .collect();
        if gated.is_empty() {
            return None;
        }
        Some(loudness(gated.iter().sum::<f64>() / gated.len() as f64))
    }

    /// EBU Tech 3342 loudness range, LU. 0 for silence or anything shorter than one block.
    pub fn range(left: &[f32], right: Option<&[f32]>, sr: f64) -> f64 {
        let n = match right {
            Some(r) => left.len().min(r.len()),
            None => left.len(),
        };
        Self::range_count(left, right, n, sr)
    }

    pub fn range_count(left: &[f32], right: Option<&[f32]>, n: usize, sr: f64) -> f64 {
        if n == 0 {
            return 0.0;
        }
        let sub = ((sr / 10.0) as usize).max(1);
        let (subs, _) = Self::sub_blocks(left, right, n, sr, 0);
        if subs.len() < 30 {
            return 0.0;
        }
        let mut window: f64 = subs[0..30].iter().sum();
        let mut blocks: Vec<f64> = vec![window / (30 * sub) as f64];
        for j in 30..subs.len() {
            window += subs[j] - subs[j - 30];
            blocks.push(window.max(0.0) / (30 * sub) as f64);
        }
        let loudness = |ms: f64| -0.691 + 10.0 * libm::log10(ms);
        let absolute: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&b| b > 0.0 && loudness(b) > -70.0)
            .collect();
        if absolute.is_empty() {
            return 0.0;
        }
        let relative = loudness(absolute.iter().sum::<f64>() / absolute.len() as f64) - 20.0;
        let mut gated: Vec<f64> = absolute
            .iter()
            .map(|&b| loudness(b))
            .filter(|&l| l > relative)
            .collect();
        gated.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        if gated.len() <= 1 {
            return 0.0;
        }
        // nearest rank, as Tech 3342's reference code
        let percentile = |p: f64| -> f64 {
            let i = libm::round(p * (gated.len() - 1) as f64) as i64;
            gated[(gated.len() as i64 - 1).min(i.max(0)) as usize]
        };
        percentile(0.95) - percentile(0.10)
    }

    /// 100 ms sums of both channels' K-weighted squares, and what is left over past the last.
    fn sub_blocks(
        left: &[f32],
        right: Option<&[f32]>,
        n: usize,
        sr: f64,
        prime_frames: usize,
    ) -> (Vec<f64>, f64) {
        let sub = ((sr / 10.0) as usize).max(1);
        let mut kl = KWeighting::new(sr);
        let mut kr = KWeighting::new(sr);
        let r = right.unwrap_or(left);
        let prime = prime_frames.min(n);
        for i in (n - prime)..n {
            kl.process(left[i] as f64);
            kr.process(r[i] as f64);
        }
        let mut subs: Vec<f64> = Vec::with_capacity(n / sub + 1);
        let mut acc = 0.0;
        let mut k = 0;
        for i in 0..n {
            let a = kl.process(left[i] as f64);
            let b = kr.process(r[i] as f64);
            acc += a * a + b * b;
            k += 1;
            if k == sub {
                subs.push(acc);
                acc = 0.0;
                k = 0;
            }
        }
        (subs, acc)
    }
}

/// ITU-R BS.1770-4 Annex 2 true peak: each channel oversampled 4× through the annex's
/// 48-tap polyphase FIR (four phases of 12), the largest magnitude of what comes out.
///
/// Swift's `TruePeak` namespace over the one implementation, [`crate::fx::true_peak`] (the
/// limiter runs the same FIR): the associated items here are that module's.
pub struct TruePeak;

impl TruePeak {
    /// Taps per phase.
    pub const TAPS: usize = true_peak::TAPS;
    /// Samples the oversampled output lags the input: the FIR's centre.
    pub const DELAY: usize = true_peak::DELAY;

    /// The annex's coefficients, phase by phase.
    pub const PHASES: [[f64; true_peak::TAPS]; 4] = true_peak::PHASES;

    /// `flatCoefficients`: `PHASES`, phase-major (4 × `TAPS`).
    pub fn flat_coefficients() -> Vec<f64> {
        true_peak::flat_coefficients()
    }

    /// `interpolatedPeak`: the largest magnitude of the four oversampled outputs for one
    /// input: `history[k]` the input `k` samples ago (`TAPS` of them, newest first).
    pub fn interpolated_peak(coefficients: &[f64], history: &[f64]) -> f64 {
        true_peak::interpolated_peak(coefficients, history)
    }

    /// True peak over both channels, linear (`right` None = mono). The FIR's tail is
    /// flushed, so a peak in the last samples counts.
    pub fn peak(left: &[f32], right: Option<&[f32]>) -> f64 {
        true_peak::peak(left, right)
    }

    /// True peak in dBTP; −200 for silence.
    pub fn dbtp(left: &[f32], right: Option<&[f32]>) -> f64 {
        true_peak::dbtp(left, right)
    }
}

/// `TruePeak.Detector`: one channel's 4× oversampled peak, fed a sample at a time.
pub use crate::fx::true_peak::Detector as TruePeakDetector;
