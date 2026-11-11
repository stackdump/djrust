//! Port of `AudioMetrics` and `OfflineRender` (`Musicality/OfflineRender.swift`): level and
//! spectral balance of a stereo render, the arrangement's level contour, and the pre-drop /
//! band-stop dips and riser rises measured from the audio. `OfflineRender` here renders a
//! `Project` through this port's `WaveRunner` (Swift renders through a deck's
//! `WaveDeckSource`, which adds samplers and the master chain).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::dj::{drop_gaps, DROP_GAP_STEPS};
use crate::musicality::rest_gestures::{band_stop_genre, band_stops, BAND_STOP_STEPS};
use crate::musicality::riser::RiserRise;
use crate::musicality::spectrum::Spectrum;
use crate::pflow::Project;
use crate::wave::runner::{WaveOptions, WaveRunner};
use crate::wave::synth::{biquad, BUTTERWORTH_Q};

/// A track rendered offline, stereo.
#[derive(Clone, Debug)]
pub struct OfflineRender {
    pub sample_rate: f64,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl OfflineRender {
    /// Render `project` with `opts` from the top for up to `seconds` (or until the song stops
    /// itself).
    pub fn new(
        project: &Project,
        opts: &WaveOptions,
        sample_rate: f64,
        seconds: f64,
    ) -> OfflineRender {
        let total = (seconds * sample_rate) as usize;
        let mut left = vec![0.0f32; total];
        let mut right = vec![0.0f32; total];
        let mut runner = WaveRunner::new(sample_rate, opts.clone());
        runner.load(project);
        runner.play();
        let block = 512;
        let mut done = 0;
        while done < total {
            let n = block.min(total - done);
            runner.process_at(
                &mut left[done..done + n],
                Some(&mut right[done..done + n]),
                n,
                done as i64,
            );
            done += n;
            if runner.stopped() {
                break;
            }
        }
        left.truncate(done);
        right.truncate(done);
        OfflineRender {
            sample_rate,
            left,
            right,
        }
    }

    pub fn metrics(&self) -> AudioMetrics {
        AudioMetrics::new(&self.left, &self.right, self.sample_rate)
    }

    /// RMS (dB) of each section of `project` that the render holds whole, in song order.
    pub fn section_levels(&self, project: &Project) -> Vec<(String, f64)> {
        let tempo = if project.tempo > 0.0 {
            project.tempo
        } else {
            120.0
        };
        let per_step = self.sample_rate * 60.0 / (tempo * 4.0);
        let mut out = vec![];
        let mut step = 0;
        for s in &project.structure {
            let a = (step as f64 * per_step) as usize;
            let b = ((step + s.steps) as f64 * per_step) as usize;
            step += s.steps;
            if !(b <= self.left.len() && b > a) {
                break;
            }
            let mut sum = 0.0;
            for i in a..b {
                let m = (self.left[i] as f64 + self.right[i] as f64) / 2.0;
                sum += m * m;
            }
            out.push((
                s.name.clone(),
                10.0 * libm::log10((sum / (b - a) as f64).max(1e-20)),
            ));
        }
        out
    }

    /// For each drop gap of `project` the render holds, the level of the beat before the drop
    /// against the beat before that, below 200 Hz, dB.
    pub fn pre_drop_dips(&self, project: &Project) -> Vec<f64> {
        self.low_dips(project, &drop_gaps(project), DROP_GAP_STEPS)
    }

    /// For each band stop of `project`, the same; empty outside the song family.
    pub fn band_stop_dips(&self, project: &Project, genre: &str) -> Vec<f64> {
        if !band_stop_genre(genre) {
            return vec![];
        }
        self.low_dips(project, &band_stops(project), BAND_STOP_STEPS)
    }

    /// Below 200 Hz, the level of `beat` steps from each of `starts` against the `beat` steps
    /// before it, dB; starts the render does not hold both sides of are left out.
    pub fn low_dips(&self, project: &Project, starts: &[i64], beat: i64) -> Vec<f64> {
        if starts.is_empty() {
            return vec![];
        }
        let tempo = if project.tempo > 0.0 {
            project.tempo
        } else {
            120.0
        };
        let per_step = self.sample_rate * 60.0 / (tempo * 4.0);
        let mut low: Vec<f64> = (0..self.left.len())
            .map(|i| (self.left[i] as f64 + self.right[i] as f64) / 2.0)
            .collect();
        for _ in 0..2 {
            let b = biquad("lowpass", 200.0, BUTTERWORTH_Q, self.sample_rate);
            let (mut z1, mut z2) = (0.0, 0.0);
            for v in low.iter_mut() {
                let x = *v;
                let y = b[0] * x + z1;
                z1 = b[1] * x - b[3] * y + z2;
                z2 = b[2] * x - b[4] * y;
                *v = y;
            }
        }
        let level = |a: usize, b: usize| -> f64 {
            let s: f64 = low[a..b].iter().map(|v| v * v).sum();
            10.0 * libm::log10((s / (b - a).max(1) as f64).max(1e-20))
        };
        let mut out = vec![];
        for g in starts {
            let a = ((g - beat) as f64 * per_step) as i64;
            let b = (*g as f64 * per_step) as usize;
            let c = ((g + beat) as f64 * per_step) as usize;
            if a < 0 || c > low.len() {
                continue;
            }
            out.push(level(b, c) - level(a as usize, b));
        }
        out
    }

    /// How the high band climbs through each riser window of `project` the render holds whole.
    pub fn riser_rises(&self, project: &Project) -> Vec<RiserRise> {
        RiserRise::measure(&self.left, &self.right, self.sample_rate, project)
    }

    /// The riser windows' mean rise and mean Spearman ρ; None without a window.
    pub fn riser_summary(&self, project: &Project) -> Option<(f64, f64)> {
        let rises = self.riser_rises(project);
        if rises.is_empty() {
            return None;
        }
        let n = rises.len() as f64;
        Some((
            rises.iter().map(|r| r.rise_db()).sum::<f64>() / n,
            rises.iter().map(|r| r.spearman()).sum::<f64>() / n,
        ))
    }

    /// 16-bit PCM WAV, stereo ([`crate::wav::wav_bytes`]).
    pub fn wav(&self) -> Vec<u8> {
        crate::wav::wav_bytes(&self.left, &self.right, self.sample_rate, false)
    }
}

/// Level and spectral balance of a stereo render.
#[derive(Clone, Debug)]
pub struct AudioMetrics {
    pub peak_db: f64,
    pub rms_db: f64,
    /// Peak over RMS, dB.
    pub crest_db: f64,
    /// Samples at or over full scale (|x| ≥ 0.999), either channel.
    pub clipped: usize,
    /// Share of the energy in each band (sums to 1): sub < 60 Hz, low 60–250, lowMid 250–2k,
    /// high 2k–6k, air > 6k.
    pub bands: BTreeMap<String, f64>,
    /// RMS of 400 ms windows, dB, 10th and 95th percentile.
    pub short_term_p10_db: f64,
    pub short_term_p95_db: f64,
    /// Side over mid energy, dB (−200 for mono).
    pub side_db: f64,
    /// The same below 150 Hz.
    pub low_side_db: f64,
    pub spectrum: Spectrum,
}

impl AudioMetrics {
    pub const BAND_EDGES: [(&'static str, f64, f64); 5] = [
        ("sub", 20.0, 60.0),
        ("low", 60.0, 250.0),
        ("lowMid", 250.0, 2000.0),
        ("high", 2000.0, 6000.0),
        ("air", 6000.0, 20000.0),
    ];

    pub fn new(left: &[f32], right: &[f32], sr: f64) -> AudioMetrics {
        let mut m = AudioMetrics {
            peak_db: -200.0,
            rms_db: -200.0,
            crest_db: 0.0,
            clipped: 0,
            bands: BTreeMap::new(),
            short_term_p10_db: -200.0,
            short_term_p95_db: -200.0,
            side_db: -200.0,
            low_side_db: -200.0,
            spectrum: Spectrum::new(&[], &[], 0.0),
        };
        let n = left.len().min(right.len());
        if n == 0 {
            return m;
        }
        let mut peak = 0.0f32;
        let mut sum = 0.0;
        let mut mono = vec![0.0f64; n];
        for i in 0..n {
            let (a, b) = (left[i].abs(), right[i].abs());
            peak = peak.max(a.max(b));
            if a >= 0.999 || b >= 0.999 {
                m.clipped += 1;
            }
            sum += left[i] as f64 * left[i] as f64 + right[i] as f64 * right[i] as f64;
            mono[i] = (left[i] as f64 + right[i] as f64) / 2.0;
        }
        m.peak_db = 20.0 * libm::log10((peak as f64).max(1e-10));
        m.rms_db = 10.0 * libm::log10((sum / (2 * n) as f64).max(1e-20));
        m.crest_db = m.peak_db - m.rms_db;

        let win = (0.4 * sr) as usize;
        let mut st: Vec<f64> = vec![];
        let mut i = 0;
        while win > 0 && i + win <= n {
            let s: f64 = mono[i..i + win].iter().map(|v| v * v).sum();
            st.push(10.0 * libm::log10((s / win as f64).max(1e-20)));
            i += win / 2;
        }
        // Ignore digital silence in the percentiles.
        st.retain(|v| *v > -90.0);
        st.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if !st.is_empty() {
            m.short_term_p10_db = st[st.len() / 10];
            m.short_term_p95_db = st[(st.len() - 1).min(st.len() * 95 / 100)];
        }

        // Band energies: fourth-order bandpasses.
        let mut energy: BTreeMap<String, f64> = BTreeMap::new();
        for (name, lo, hi) in Self::BAND_EDGES {
            let mut x = mono.clone();
            for _ in 0..2 {
                filter(&mut x, &biquad("highpass", lo, BUTTERWORTH_Q, sr));
            }
            if hi < sr * 0.45 {
                for _ in 0..2 {
                    filter(&mut x, &biquad("lowpass", hi, BUTTERWORTH_Q, sr));
                }
            }
            energy.insert(name.into(), x.iter().map(|v| v * v).sum());
        }
        let total: f64 = energy.values().sum();
        if total > 0.0 {
            m.bands = energy.into_iter().map(|(k, v)| (k, v / total)).collect();
        }

        let side: Vec<f64> = (0..n)
            .map(|i| (left[i] as f64 - right[i] as f64) / 2.0)
            .collect();
        let ratio = |s: &[f64], mid: &[f64]| -> f64 {
            let es: f64 = s.iter().map(|v| v * v).sum();
            let em: f64 = mid.iter().map(|v| v * v).sum();
            if es > 0.0 && em > 0.0 {
                10.0 * libm::log10(es / em)
            } else {
                -200.0
            }
        };
        m.side_db = ratio(&side, &mono);
        let mut low_s = side;
        let mut low_m = mono;
        for _ in 0..2 {
            filter(&mut low_s, &biquad("lowpass", 150.0, BUTTERWORTH_Q, sr));
            filter(&mut low_m, &biquad("lowpass", 150.0, BUTTERWORTH_Q, sr));
        }
        m.low_side_db = ratio(&low_s, &low_m);
        m.spectrum = Spectrum::new(left, right, sr);
        m
    }
}

fn filter(x: &mut [f64], b: &[f64; 5]) {
    let (mut z1, mut z2) = (0.0, 0.0);
    for v in x.iter_mut() {
        let input = *v;
        let y = b[0] * input + z1;
        z1 = b[1] * input - b[3] * y + z2;
        z2 = b[2] * input - b[4] * y;
        *v = y;
    }
}

/// How many melodic parts sound in one section of a track (`OfflineRender.layersPerSection`):
/// what `.layerBudget` caps, measured from the audio rather than read from the nets.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionLayers {
    pub name: String,
    /// The parts (`LAYER_PARTS`) at or above `LAYER_FLOOR_DB` against the mix there.
    pub parts: Vec<String>,
    /// Each part's level against the section's mix, dB (−200 for silence).
    pub levels_db: BTreeMap<String, f64>,
}

impl SectionLayers {
    pub fn layers(&self) -> usize {
        self.parts.len()
    }
}

/// The melodic parts a layer count looks at (`MixBalance`'s names).
pub const LAYER_PARTS: [&str; 4] = ["bass", "pad", "lead", "arp"];
/// A part counts as a layer in a section when its solo is at least this loud against the mix.
pub const LAYER_FLOOR_DB: f64 = -40.0;

/// `OfflineRender.layersPerSection`: each section of `played` (what a deck plays): which
/// melodic parts sound in it. Every part is rendered alone (`MixBalance::solo`) through the
/// whole song with `opts` (its rest schedule included: `opts.rests`, else the one `opts`' fixes
/// give the song) through a linear master at one gain, and its level
/// in each section measured against the mix there, leaving out each section's first bar.
/// Sections past `seconds` are left out.
pub fn layers_per_section(
    played: &Project,
    opts: &WaveOptions,
    sample_rate: f64,
    seconds: Option<f64>,
) -> Vec<SectionLayers> {
    use crate::musicality::mix_balance::MixBalance;
    use crate::musicality::note_log::length;
    use crate::musicality::rest_gestures::build_rests;
    use crate::wave::runner::Master;
    let mut o = opts.clone();
    o.bank = None;
    o.master = Master::Linear;
    // one gain for the mix and every solo (`prepare` would measure each one's own under
    // `.loudness`), and the rest schedule of the song played, not of each solo (`prepare` builds
    // one per project): `RestSchedule.build(played, fixes:)`, no genre
    o.track_gain = Some(1.0);
    if o.rests.is_none() {
        o.rests = Some(build_rests(played, o.musicality, None));
    }
    let tempo = if played.tempo > 0.0 {
        played.tempo
    } else {
        120.0
    };
    let per_step = sample_rate * 60.0 / (tempo * 4.0);
    let mut frames = (length(played) as f64 * per_step) as usize;
    if let Some(s) = seconds {
        frames = frames.min((s * sample_rate) as usize);
    }
    // the sections the render holds whole, as (name, first frame, end frame)
    let mut spans: Vec<(String, usize, usize)> = vec![];
    let mut step = 0i64;
    for s in &played.structure {
        let skip = 16.min(s.steps / 4);
        let a = ((step + skip) as f64 * per_step) as usize;
        let b = ((step + s.steps) as f64 * per_step) as usize;
        step += s.steps;
        if b > frames {
            break;
        }
        if b > a {
            spans.push((s.name.clone(), a, b));
        }
    }
    let levels = |p: &Project| -> Vec<f64> {
        let mut runner = WaveRunner::new(sample_rate, o.clone());
        runner.load_prepared(WaveRunner::prepare(p, sample_rate, &o, 0));
        runner.play();
        let mut x = vec![0.0f32; frames];
        let mut n = 0;
        while n < x.len() {
            let m = 512.min(x.len() - n);
            runner.process(&mut x[n..n + m], None, m);
            n += m;
            if runner.stopped() {
                break;
            }
        }
        spans
            .iter()
            .map(|(_, a, b)| {
                let sum: f64 = x[*a..*b].iter().map(|v| *v as f64 * *v as f64).sum();
                10.0 * libm::log10((sum / (b - a) as f64).max(1e-20))
            })
            .collect()
    };
    let mix = levels(played);
    let mut solo: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for name in LAYER_PARTS {
        solo.insert(name, levels(&MixBalance::solo(played, name)));
    }
    spans
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| {
            let mut rel: BTreeMap<String, f64> = BTreeMap::new();
            for part in LAYER_PARTS {
                let l = solo[part][i];
                rel.insert(
                    part.into(),
                    if l <= -150.0 {
                        -200.0
                    } else {
                        (l - mix[i]).max(-200.0)
                    },
                );
            }
            SectionLayers {
                name: name.clone(),
                parts: LAYER_PARTS
                    .iter()
                    .filter(|p| rel[**p] >= LAYER_FLOOR_DB)
                    .map(|p| String::from(*p))
                    .collect(),
                levels_db: rel,
            }
        })
        .collect()
}
