//! One loudness for everything the user can play (`Audio/LevelMatch.swift`): a generated track,
//! a sample or a loop on a deck, a take of either. Each lands at [`LevelMatch::REFERENCE_LUFS`]
//! (integrated, BS.1770) at unity knobs (trim centred, EQ flat), before the channel fader:
//!
//! - net decks: the track's loudness gain (`musicality::loudness::Loudness`, calibrated to this)
//!   and the deck's `PeakLimiter` (true peaks <= -1 dBTP);
//! - sample decks: [`SampleLevel::auto_gain_db`], measured when the audio loads (derived from
//!   the audio alone, so a replay reproduces it), off per deck;
//! - takes record the deck after its EQ, so they come out at the level the deck played.
//!
//! The meter is `musicality::loudness::IntegratedLoudness` (BS.1770 with Swift's gating, the
//! crate's one: `crate::loudness` is the same under flat names); this module adds the
//! sample-deck policy on top of it.
//! [`LevelSurvey`] (`djctl levels`) is the statistics over a set of [`SampleLevel`]s: how far
//! each sample sits from its target as it is and as a sample deck plays it, and the spread. It
//! is pure (`measure`, which decodes the files, is the host's: `djctl levels`); only its `pick`
//! reads Apple Loops, and it works over any [`StockLoop`] list.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::deck::turntable::SampleBuffer;
use crate::json::{JsonObject, JsonValue};
use crate::layers::StockLoop;
use crate::musicality::loudness::{IntegratedLoudness, KWeighting};
use crate::musicality::mix_balance::MixBalance;
use crate::musicality::mix_targets::REFERENCE_LUFS;
use crate::musicality::report::round3;
use crate::wave::synth::{biquad, BUTTERWORTH_Q};

/// `LevelMatch`: the app's reference loudness and the sample auto-gain's limits.
pub struct LevelMatch;

impl LevelMatch {
    /// The app's reference, LUFS. Commercial dance masters sit at -8...-12, Apple's loops at
    /// -11...-14 for full beats and leads.
    pub const REFERENCE_LUFS: f64 = REFERENCE_LUFS;
    /// Where a loop with only part of a mix in it (a bass line, a kick, hats: one band holding
    /// most of its energy, [`SampleLevel::partial`]) is aimed, against a full mix.
    pub const PARTIAL_OFFSET_DB: f64 = -4.0;
    /// The auto-gain stays within +- this (a near-silent hat loop is not boosted to the moon)...
    pub const MAX_GAIN_DB: f64 = 12.0;
    /// ...and never lifts a sample's peak over this (dBFS): samples have no limiter, so a boost
    /// that would clip is cut back to the headroom the audio has.
    pub const SAMPLE_PEAK_CEILING_DB: f64 = -1.0;
    /// Exit M7 (ROADMAP 6.6): whole songs of every genre, synth or sampled, land within this of
    /// each other (max - min, LU)...
    pub const MAX_SPREAD_LU: f64 = 2.0;
    /// ...and a centred A->B crossfade within this of either deck alone.
    pub const CROSSFADE_TOLERANCE_LU: f64 = 1.0;

    /// `targetLUFS(genre:)`: its family's `lufs` band once measured, else the reference.
    pub fn target_lufs(genre: Option<&str>) -> f64 {
        MixBalance::target_lufs(genre)
    }
}

/// How loud a sample is and the auto-gain that puts it at the app's reference (`SampleLevel`).
/// A pure function of the audio: the same buffer always gets the same gain, so a session or
/// journal replay that loads it reproduces it without recording it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleLevel {
    /// Integrated loudness, LUFS (None: silent).
    pub lufs: Option<f64>,
    /// Sample peak, dBFS.
    pub peak_db: f64,
    /// Share of the (unweighted) energy below 150 Hz and above 4 kHz.
    pub low_share: f64,
    pub high_share: f64,
}

impl SampleLevel {
    pub fn new(lufs: Option<f64>, peak_db: f64, low_share: f64, high_share: f64) -> SampleLevel {
        SampleLevel {
            lufs,
            peak_db,
            low_share,
            high_share,
        }
    }

    /// Only part of a mix (a bass, a kick, hats): most of the energy in the lows (>= 80 % under
    /// 150 Hz, with next to nothing over 4 kHz: no hats) or the highs (>= 50 % over 4 kHz).
    pub fn partial(&self) -> bool {
        (self.low_share >= 0.8 && self.high_share < 0.02) || self.high_share >= 0.5
    }

    /// The loudness this sample is aimed at.
    pub fn target_lufs(&self) -> f64 {
        LevelMatch::REFERENCE_LUFS
            + if self.partial() {
                LevelMatch::PARTIAL_OFFSET_DB
            } else {
                0.0
            }
    }

    /// dB to play it at: toward `target_lufs`, within +-`MAX_GAIN_DB`, and no boost past the
    /// peak ceiling. 0 for silence.
    pub fn auto_gain_db(&self) -> f64 {
        let Some(lufs) = self.lufs else { return 0.0 };
        let m = LevelMatch::MAX_GAIN_DB;
        let mut g = m.min((-m).max(self.target_lufs() - lufs));
        g = g.min(0.0f64.max(LevelMatch::SAMPLE_PEAK_CEILING_DB - self.peak_db));
        libm::round(g * 100.0) / 100.0
    }

    /// `SampleLevel(buffer)`: measure `buffer` (as a loop: the filters run in from its end).
    /// Off the audio thread; tens of milliseconds for a loop in a release build.
    pub fn measure(buffer: &SampleBuffer) -> SampleLevel {
        let n = buffer.length();
        let mono = buffer.right.is_none();
        let sr = buffer.sample_rate;
        let lufs = IntegratedLoudness::integrated_count(
            &buffer.left,
            if mono { None } else { Some(buffer.right()) },
            n,
            sr,
            (sr / 2.0) as usize,
        );
        let mut peak: f32 = 0.0;
        let mut total = 0.0f64;
        let mut low = 0.0f64;
        let mut high = 0.0f64;
        let lp = biquad("lowpass", 150.0, BUTTERWORTH_Q, sr);
        let hp = biquad("highpass", 4000.0, BUTTERWORTH_Q, sr);
        let (mut l1, mut l2, mut h1, mut h2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (&a, &b) in buffer.left.iter().zip(buffer.right()) {
            peak = peak.max(a.abs().max(b.abs()));
            let x = 0.5 * (a as f64 + b as f64);
            total += x * x;
            let yl = lp[0] * x + l1;
            l1 = lp[1] * x - lp[3] * yl + l2;
            l2 = lp[2] * x - lp[4] * yl;
            low += yl * yl;
            let yh = hp[0] * x + h1;
            h1 = hp[1] * x - hp[3] * yh + h2;
            h2 = hp[2] * x - hp[4] * yh;
            high += yh * yh;
        }
        SampleLevel {
            lufs,
            peak_db: 20.0 * libm::log10((peak as f64).max(1e-10)),
            low_share: if total > 0.0 {
                (low / total).min(1.0)
            } else {
                0.0
            },
            high_share: if total > 0.0 {
                (high / total).min(1.0)
            } else {
                0.0
            },
        }
    }
}

impl SampleLevel {
    /// `SampleLevel.dictionary`: the figures as `djctl levels` prints them (`lufs` only when the
    /// sample is not silent; the shares and the peak to 3 places).
    pub fn dictionary(&self) -> JsonObject {
        let mut d = JsonObject::from_pairs([
            ("peakDB", JsonValue::Number(round3(self.peak_db))),
            ("lowShare", JsonValue::Number(round3(self.low_share))),
            ("highShare", JsonValue::Number(round3(self.high_share))),
            ("partial", JsonValue::Bool(self.partial())),
            ("targetLUFS", JsonValue::Number(self.target_lufs())),
            ("autoGainDB", JsonValue::Number(self.auto_gain_db())),
        ]);
        if let Some(lufs) = self.lufs {
            d.set("lufs", JsonValue::Number(round3(lufs)));
        }
        d
    }
}

/// One sample of a [`LevelSurvey`] (`LevelSurvey.Entry`).
#[derive(Clone, Debug, PartialEq)]
pub struct SurveyEntry {
    pub name: String,
    pub category: String,
    pub level: SampleLevel,
}

impl SurveyEntry {
    /// `Entry(name:category:level:)`.
    pub fn new(name: &str, category: &str, level: SampleLevel) -> SurveyEntry {
        SurveyEntry {
            name: name.to_string(),
            category: category.to_string(),
            level,
        }
    }

    /// LU off its target as it is (None: silent).
    pub fn error_before(&self) -> Option<f64> {
        self.level.lufs.map(|l| l - self.level.target_lufs())
    }

    /// LU off its target as a deck plays it, auto-gain on.
    pub fn error_after(&self) -> Option<f64> {
        self.error_before().map(|e| e + self.level.auto_gain_db())
    }

    /// What kept it off its target: `peak` (the boost it wanted, after the +-12 dB clamp, was cut
    /// back to the peak ceiling) or `maxGain` (the clamp itself); None when it lands within the
    /// tolerance or is silent. Decided from the gain actually applied: a sample past the clamp
    /// whose peak allowed less than the clamp is `peak`.
    pub fn limited_by(&self) -> Option<&'static str> {
        let e = self.error_after()?;
        let lufs = self.level.lufs?;
        if e.abs() <= LevelSurvey::TOLERANCE_LU {
            return None;
        }
        let m = LevelMatch::MAX_GAIN_DB;
        let want = m.min((-m).max(self.level.target_lufs() - lufs));
        let peak_cap = 0.0f64.max(LevelMatch::SAMPLE_PEAK_CEILING_DB - self.level.peak_db);
        Some(if want > peak_cap { "peak" } else { "maxGain" })
    }

    /// `Entry.dictionary`: the level's, with the name, the category when there is one and the
    /// errors (to 3 places) and what limited it.
    pub fn dictionary(&self) -> JsonObject {
        let mut d = self.level.dictionary();
        d.set("name", JsonValue::str(self.name.as_str()));
        if !self.category.is_empty() {
            d.set("category", JsonValue::str(self.category.as_str()));
        }
        if let Some(e) = self.error_before() {
            d.set("errorBeforeLU", JsonValue::Number(round3(e)));
        }
        if let Some(e) = self.error_after() {
            d.set("errorAfterLU", JsonValue::Number(round3(e)));
        }
        if let Some(l) = self.limited_by() {
            d.set("limitedBy", JsonValue::str(l));
        }
        d
    }
}

/// What the auto-gain does to a set of samples (`djctl levels`): each one's loudness as it is and
/// as a deck plays it, and the spread of both against their targets (a partial loop's is 4 LU
/// under a full mix's, so the spread is taken off each one's own target). `LevelSurvey`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LevelSurvey {
    pub entries: Vec<SurveyEntry>,
    /// Samples that could not be read: name, why.
    pub failures: Vec<(String, String)>,
}

impl LevelSurvey {
    /// A sample within this of its target counts as level matched.
    pub const TOLERANCE_LU: f64 = 1.5;

    /// `LevelSurvey(entries:failures:)`.
    pub fn new(entries: Vec<SurveyEntry>, failures: Vec<(String, String)>) -> LevelSurvey {
        LevelSurvey { entries, failures }
    }

    /// `LevelSurvey.pick`: `per_category` stock loops from each category, evenly spaced through
    /// it by path: the same pick every run for the same library. Categories and paths compare
    /// as Swift's `String <` does for ASCII (by scalar).
    pub fn pick(loops: &[StockLoop], per_category: usize) -> Vec<StockLoop> {
        let mut by_category: BTreeMap<&str, Vec<&StockLoop>> = BTreeMap::new();
        for l in loops {
            by_category.entry(l.category.as_str()).or_default().push(l);
        }
        let mut out = Vec::new();
        for (_, mut all) in by_category {
            all.sort_by(|a, b| a.path.cmp(&b.path));
            let n = per_category.min(all.len());
            for i in 0..n {
                out.push(all[i * all.len() / n.max(1)].clone());
            }
        }
        out
    }

    /// max - min of the errors (LU), as is: None with nothing measured.
    pub fn spread_before(&self) -> Option<f64> {
        range(self.entries.iter().filter_map(SurveyEntry::error_before))
    }

    /// max - min of the errors (LU), as played: None with nothing measured.
    pub fn spread_after(&self) -> Option<f64> {
        range(self.entries.iter().filter_map(SurveyEntry::error_after))
    }

    /// The samples that miss their target by more than the tolerance as played.
    pub fn exceptions(&self) -> Vec<&SurveyEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.error_after()
                    .is_some_and(|x| x.abs() > Self::TOLERANCE_LU)
            })
            .collect()
    }

    /// `LevelSurvey.summary`: measured, silent, toleranceLU, within, the exceptions, the spreads
    /// and the largest errors (to 3 places), and the failures when there are any.
    pub fn summary(&self) -> JsonObject {
        let exceptions = self.exceptions();
        let silent = self
            .entries
            .iter()
            .filter(|e| e.level.lufs.is_none())
            .count();
        let mut d = JsonObject::from_pairs([
            ("measured", JsonValue::int(self.entries.len() as i64)),
            ("silent", JsonValue::int(silent as i64)),
            ("toleranceLU", JsonValue::Number(Self::TOLERANCE_LU)),
            (
                "within",
                JsonValue::int((self.entries.len() - exceptions.len() - silent) as i64),
            ),
            (
                "exceptions",
                JsonValue::Array(
                    exceptions
                        .iter()
                        .map(|e| {
                            let mut x = JsonObject::from_pairs([
                                ("name", JsonValue::str(e.name.as_str())),
                                (
                                    "errorAfterLU",
                                    JsonValue::Number(round3(e.error_after().unwrap_or(0.0))),
                                ),
                            ]);
                            if let Some(l) = e.limited_by() {
                                x.set("limitedBy", JsonValue::str(l));
                            }
                            JsonValue::Object(x)
                        })
                        .collect(),
                ),
            ),
        ]);
        if let Some(s) = self.spread_before() {
            d.set("spreadBeforeLU", JsonValue::Number(round3(s)));
        }
        if let Some(s) = self.spread_after() {
            d.set("spreadAfterLU", JsonValue::Number(round3(s)));
        }
        let before = self.entries.iter().filter_map(SurveyEntry::error_before);
        if let Some(m) = max_abs(before) {
            d.set("maxErrorBeforeLU", JsonValue::Number(round3(m)));
        }
        let after = self.entries.iter().filter_map(SurveyEntry::error_after);
        if let Some(m) = max_abs(after) {
            d.set("maxErrorAfterLU", JsonValue::Number(round3(m)));
        }
        if !self.failures.is_empty() {
            d.set(
                "failures",
                JsonValue::Array(
                    self.failures
                        .iter()
                        .map(|(name, error)| {
                            JsonValue::Object(JsonObject::from_pairs([
                                ("name", JsonValue::str(name.as_str())),
                                ("error", JsonValue::str(error.as_str())),
                            ]))
                        })
                        .collect(),
                ),
            );
        }
        d
    }
}

/// `LevelSurvey.range`: max - min, None when empty.
fn range(x: impl Iterator<Item = f64>) -> Option<f64> {
    let (mut lo, mut hi): (Option<f64>, Option<f64>) = (None, None);
    for v in x {
        lo = Some(lo.map_or(v, |l| l.min(v)));
        hi = Some(hi.map_or(v, |h| h.max(v)));
    }
    Some(hi? - lo?)
}

/// `LevelSurvey.maxAbs`.
fn max_abs(x: impl Iterator<Item = f64>) -> Option<f64> {
    x.map(f64::abs)
        .fold(None, |m, v| Some(m.map_or(v, |m: f64| m.max(v))))
}

/// A deck's short-term loudness (BS.1770, the last 3 s) and momentary (400 ms): K-weighted
/// squares summed per 100 ms into a ring of 30 (`LoudnessMeter`). Allocation-free after `new`.
/// Swift publishes the two readings through atomics once per 100 ms; here they are fields the
/// host publishes itself.
pub struct LoudnessMeter {
    kl: KWeighting,
    kr: KWeighting,
    sub: usize,
    ring: [f64; Self::SLOTS],
    slot: usize,
    filled: usize,
    acc: f64,
    k: usize,
    short_term: f64,
    momentary: f64,
}

impl LoudnessMeter {
    const SLOTS: usize = 30;

    pub fn new(sample_rate: f64) -> LoudnessMeter {
        LoudnessMeter {
            kl: KWeighting::new(sample_rate),
            kr: KWeighting::new(sample_rate),
            sub: 1usize.max((sample_rate / 10.0) as usize),
            ring: [0.0; Self::SLOTS],
            slot: 0,
            filled: 0,
            acc: 0.0,
            k: 0,
            short_term: -200.0,
            momentary: -200.0,
        }
    }

    /// Audio thread. `right` None: the left again.
    pub fn push(&mut self, left: &[f32], right: Option<&[f32]>, frames: usize) {
        for i in 0..frames {
            let a = self.kl.process(left[i] as f64);
            let b = self.kr.process(right.unwrap_or(left)[i] as f64);
            self.acc += a * a + b * b;
            self.k += 1;
            if self.k == self.sub {
                self.ring[self.slot] = self.acc / self.sub as f64;
                self.slot = if self.slot + 1 == Self::SLOTS {
                    0
                } else {
                    self.slot + 1
                };
                self.filled = Self::SLOTS.min(self.filled + 1);
                self.acc = 0.0;
                self.k = 0;
                let mut s = 0.0;
                let mut m = 0.0;
                for j in 0..self.filled {
                    let v = self.ring[(self.slot + Self::SLOTS - 1 - j) % Self::SLOTS];
                    s += v;
                    if j < 4 {
                        m += v;
                    }
                }
                let st = s / self.filled as f64;
                let mo = m / 4usize.min(self.filled) as f64;
                self.short_term = if st > 1e-20 {
                    -0.691 + 10.0 * libm::log10(st)
                } else {
                    -200.0
                };
                self.momentary = if mo > 1e-20 {
                    -0.691 + 10.0 * libm::log10(mo)
                } else {
                    -200.0
                };
            }
        }
    }

    /// LUFS over the last 3 s as of the last 100 ms boundary (-200 when silent).
    pub fn short_term_lufs(&self) -> f64 {
        self.short_term
    }

    /// LUFS over the last 400 ms as of the last 100 ms boundary.
    pub fn momentary_lufs(&self) -> f64 {
        self.momentary
    }
}
