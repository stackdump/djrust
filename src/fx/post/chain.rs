//! The post-processing chain as data (docs/POST.md): an ordered list of [`Stage`]s with their
//! params, the named presets, JSON in and out, and the ffmpeg filter graph each one stands for
//! (so a chain can be A/B'd against the ffmpeg command it replaced).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};

/// How the loudness stage reaches its target (ffmpeg `loudnorm`'s two modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoudnessMode {
    /// Linear when one gain meets the target without crossing the true-peak ceiling and the
    /// input's range is within `lra`; dynamic otherwise (what `loudnorm` does).
    Auto,
    /// One gain for the whole file (then the true-peak limiter if the gain pushes peaks over).
    Linear,
    /// Short-term loudness levelled toward the target (3 s windows, smoothed), then the limiter.
    Dynamic,
}

impl LoudnessMode {
    pub fn id(self) -> &'static str {
        match self {
            LoudnessMode::Auto => "auto",
            LoudnessMode::Linear => "linear",
            LoudnessMode::Dynamic => "dynamic",
        }
    }

    pub fn parse(s: &str) -> Option<LoudnessMode> {
        Some(match s {
            "auto" => LoudnessMode::Auto,
            "linear" => LoudnessMode::Linear,
            "dynamic" => LoudnessMode::Dynamic,
            _ => return None,
        })
    }
}

/// One stage of a chain.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// 2nd-order (12 dB/oct) RBJ highpass: rumble and DC.
    Highpass { freq: f64, q: f64 },
    Lowpass { freq: f64, q: f64 },
    HighShelf { freq: f64, gain_db: f64, q: f64 },
    LowShelf { freq: f64, gain_db: f64, q: f64 },
    /// Peaking EQ band.
    Peak { freq: f64, gain_db: f64, q: f64 },
    /// STFT spectral denoise (Wiener gain, decision-directed SNR) against a noise profile that
    /// starts as white noise at `noise_floor_db` dBFS and, with `track`, follows the measured
    /// floor (minimum statistics, per bin) within ±`track_range_db` of it. `reduction_db` is
    /// the most any bin is turned down. `fft_size`: None = 2048 offline, 1024 live (latency).
    Denoise {
        noise_floor_db: f64,
        reduction_db: f64,
        track: bool,
        track_range_db: f64,
        /// The noise estimate the gain works against, moved by this much (dB): calibrated so
        /// `noise_floor_db` / `reduction_db` turn the hiss down about as far as ffmpeg's
        /// `afftdn` with the same `nf` / `nr` does (docs/POST.md).
        noise_offset_db: f64,
        fft_size: Option<usize>,
    },
    /// Downward expander / gate: below the threshold the level falls `ratio - 1` dB per dB, to
    /// at most `range_db` down. The threshold is `threshold_db` dBFS if set, else
    /// `below_program_db` under the program level it tracks (so it works at any render level).
    Expander {
        threshold_db: Option<f64>,
        below_program_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        range_db: f64,
        knee_db: f64,
    },
    Gain { db: f64 },
    /// Loudness to `target_lufs` with a true-peak ceiling. Offline: two passes (measure, then
    /// apply), like `loudnorm`. Live: a short-term-LUFS leveler (AGC) and a true-peak limiter.
    Loudness {
        target_lufs: f64,
        true_peak_db: f64,
        lra: f64,
        mode: LoudnessMode,
    },
    /// A lookahead true-peak limiter (BS.1770 4x oversampled detection) at `ceiling_db` dBTP.
    Limiter { ceiling_db: f64 },
}

/// [`Stage::Denoise`]'s default `noise_offset_db`.
pub const NOISE_OFFSET_DB: f64 = -5.0;

const Q_BUTTER: f64 = core::f64::consts::FRAC_1_SQRT_2;

impl Stage {
    pub fn kind(&self) -> &'static str {
        match self {
            Stage::Highpass { .. } => "highpass",
            Stage::Lowpass { .. } => "lowpass",
            Stage::HighShelf { .. } => "highshelf",
            Stage::LowShelf { .. } => "lowshelf",
            Stage::Peak { .. } => "peak",
            Stage::Denoise { .. } => "denoise",
            Stage::Expander { .. } => "expander",
            Stage::Gain { .. } => "gain",
            Stage::Loudness { .. } => "loudness",
            Stage::Limiter { .. } => "limiter",
        }
    }

    pub fn highpass(freq: f64) -> Stage {
        Stage::Highpass { freq, q: Q_BUTTER }
    }

    pub fn denoise(noise_floor_db: f64, reduction_db: f64) -> Stage {
        Stage::Denoise {
            noise_floor_db,
            reduction_db,
            track: true,
            track_range_db: 12.0,
            noise_offset_db: NOISE_OFFSET_DB,
            fft_size: None,
        }
    }

    pub fn expander() -> Stage {
        Stage::Expander {
            threshold_db: None,
            below_program_db: 30.0,
            ratio: 2.0,
            attack_ms: 20.0,
            release_ms: 250.0,
            range_db: 24.0,
            knee_db: 9.0,
        }
    }

    pub fn loudness() -> Stage {
        Stage::Loudness {
            target_lufs: -14.0,
            true_peak_db: -1.0,
            lra: 11.0,
            mode: LoudnessMode::Auto,
        }
    }

    /// Parse `{"stage": KIND, …params}`; a param left out takes the default.
    pub fn from_json(v: &JsonValue) -> Result<Stage, String> {
        let kind = v
            .get("stage")
            .or_else(|| v.get("type"))
            .and_then(JsonValue::as_str)
            .ok_or("a stage is {\"stage\": KIND, …params}")?;
        let num = |k: &str, d: f64| -> Result<f64, String> {
            match v.get(k) {
                None | Some(JsonValue::Null) => Ok(d),
                Some(x) => x
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| format!("{kind}.{k}: a number")),
            }
        };
        let opt = |k: &str| -> Result<Option<f64>, String> {
            match v.get(k) {
                None | Some(JsonValue::Null) => Ok(None),
                Some(x) => x
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .map(Some)
                    .ok_or_else(|| format!("{kind}.{k}: a number")),
            }
        };
        let freq = |d: f64| -> Result<f64, String> {
            let f = num("freq", num("f", d)?)?;
            if f <= 0.0 {
                return Err(format!("{kind}.freq: above 0 Hz"));
            }
            Ok(f)
        };
        let q = |d: f64| -> Result<f64, String> {
            let q = num("q", d)?;
            if q <= 0.0 {
                return Err(format!("{kind}.q: above 0"));
            }
            Ok(q)
        };
        let gain = |d: f64| num("gain_db", num("gain", num("g", d)?)?);
        let s = match kind {
            "highpass" | "hp" => Stage::Highpass {
                freq: freq(25.0)?,
                q: q(Q_BUTTER)?,
            },
            "lowpass" | "lp" => Stage::Lowpass {
                freq: freq(18_000.0)?,
                q: q(Q_BUTTER)?,
            },
            "highshelf" | "treble" => Stage::HighShelf {
                freq: freq(9_000.0)?,
                gain_db: gain(-5.0)?,
                q: q(Q_BUTTER)?,
            },
            "lowshelf" | "bass" => Stage::LowShelf {
                freq: freq(100.0)?,
                gain_db: gain(0.0)?,
                q: q(Q_BUTTER)?,
            },
            "peak" | "eq" | "equalizer" => Stage::Peak {
                freq: freq(1_000.0)?,
                gain_db: gain(0.0)?,
                q: q(1.0)?,
            },
            "denoise" | "afftdn" => {
                let fft = match opt("fft_size")? {
                    None => None,
                    Some(n) => {
                        let n = n as usize;
                        if !(256..=16_384).contains(&n) || !n.is_power_of_two() {
                            return Err("denoise.fft_size: a power of two, 256..16384".into());
                        }
                        Some(n)
                    }
                };
                Stage::Denoise {
                    noise_floor_db: num("noise_floor_db", num("nf", -50.0)?)?.clamp(-100.0, -10.0),
                    reduction_db: num("reduction_db", num("nr", 14.0)?)?.clamp(0.0, 60.0),
                    track: match v.get("track").or_else(|| v.get("tn")) {
                        None => true,
                        Some(JsonValue::Bool(b)) => *b,
                        Some(JsonValue::Number(n)) => *n != 0.0,
                        _ => return Err("denoise.track: true or false".into()),
                    },
                    track_range_db: num("track_range_db", 12.0)?.clamp(0.0, 40.0),
                    noise_offset_db: num("noise_offset_db", NOISE_OFFSET_DB)?.clamp(-30.0, 30.0),
                    fft_size: fft,
                }
            }
            "expander" | "gate" | "agate" => {
                let ratio = num("ratio", 2.0)?;
                if ratio < 1.0 {
                    return Err("expander.ratio: 1 or more".into());
                }
                Stage::Expander {
                    threshold_db: opt("threshold_db")?,
                    below_program_db: num("below_program_db", 30.0)?.abs(),
                    ratio,
                    attack_ms: num("attack_ms", 20.0)?.max(0.1),
                    release_ms: num("release_ms", 250.0)?.max(1.0),
                    range_db: num("range_db", 24.0)?.abs(),
                    knee_db: num("knee_db", 9.0)?.max(0.0),
                }
            }
            "gain" | "volume" => Stage::Gain {
                db: num("db", num("gain_db", 0.0)?)?.clamp(-60.0, 40.0),
            },
            "loudness" | "loudnorm" => {
                let mode = match v.get("mode").and_then(JsonValue::as_str) {
                    None => LoudnessMode::Auto,
                    Some(m) => LoudnessMode::parse(m)
                        .ok_or("loudness.mode: auto, linear or dynamic")?,
                };
                Stage::Loudness {
                    target_lufs: num("target_lufs", num("i", num("I", -14.0)?)?)?.clamp(-70.0, -5.0),
                    true_peak_db: num("true_peak_db", num("tp", num("TP", -1.0)?)?)?.clamp(-9.0, 0.0),
                    lra: num("lra", num("LRA", 11.0)?)?.clamp(1.0, 50.0),
                    mode,
                }
            }
            "limiter" | "alimiter" => Stage::Limiter {
                ceiling_db: num("ceiling_db", num("tp", -1.0)?)?.clamp(-12.0, 0.0),
            },
            other => {
                return Err(format!(
                    "no stage {other:?} (highpass, lowpass, highshelf, lowshelf, peak, denoise, expander, gain, loudness, limiter)"
                ))
            }
        };
        Ok(s)
    }

    pub fn json(&self) -> JsonValue {
        let n = JsonValue::Number;
        let mut o = JsonObject::from_pairs([("stage", JsonValue::str(self.kind()))]);
        match self {
            Stage::Highpass { freq, q } | Stage::Lowpass { freq, q } => {
                o.set("freq", n(*freq));
                o.set("q", n(*q));
            }
            Stage::HighShelf { freq, gain_db, q }
            | Stage::LowShelf { freq, gain_db, q }
            | Stage::Peak { freq, gain_db, q } => {
                o.set("freq", n(*freq));
                o.set("gain_db", n(*gain_db));
                o.set("q", n(*q));
            }
            Stage::Denoise {
                noise_floor_db,
                reduction_db,
                track,
                track_range_db,
                noise_offset_db,
                fft_size,
            } => {
                o.set("noise_floor_db", n(*noise_floor_db));
                o.set("noise_offset_db", n(*noise_offset_db));
                o.set("reduction_db", n(*reduction_db));
                o.set("track", (*track).into());
                o.set("track_range_db", n(*track_range_db));
                if let Some(f) = fft_size {
                    o.set("fft_size", JsonValue::int(*f as i64));
                }
            }
            Stage::Expander {
                threshold_db,
                below_program_db,
                ratio,
                attack_ms,
                release_ms,
                range_db,
                knee_db,
            } => {
                match threshold_db {
                    Some(t) => o.set("threshold_db", n(*t)),
                    None => o.set("below_program_db", n(*below_program_db)),
                }
                o.set("ratio", n(*ratio));
                o.set("attack_ms", n(*attack_ms));
                o.set("release_ms", n(*release_ms));
                o.set("range_db", n(*range_db));
                o.set("knee_db", n(*knee_db));
            }
            Stage::Gain { db } => o.set("db", n(*db)),
            Stage::Loudness {
                target_lufs,
                true_peak_db,
                lra,
                mode,
            } => {
                o.set("target_lufs", n(*target_lufs));
                o.set("true_peak_db", n(*true_peak_db));
                o.set("lra", n(*lra));
                o.set("mode", JsonValue::str(mode.id()));
            }
            Stage::Limiter { ceiling_db } => o.set("ceiling_db", n(*ceiling_db)),
        }
        JsonValue::Object(o)
    }

    /// The ffmpeg filter this stage stands for (approximately: the DSP is this crate's own).
    pub fn ffmpeg(&self) -> String {
        match self {
            Stage::Highpass { freq, .. } => format!("highpass=f={}", g(*freq)),
            Stage::Lowpass { freq, .. } => format!("lowpass=f={}", g(*freq)),
            Stage::HighShelf { freq, gain_db, .. } => {
                format!("highshelf=f={}:g={}", g(*freq), g(*gain_db))
            }
            Stage::LowShelf { freq, gain_db, .. } => {
                format!("lowshelf=f={}:g={}", g(*freq), g(*gain_db))
            }
            Stage::Peak { freq, gain_db, q } => {
                format!("equalizer=f={}:t=q:w={}:g={}", g(*freq), g(*q), g(*gain_db))
            }
            Stage::Denoise {
                noise_floor_db,
                reduction_db,
                track,
                ..
            } => format!(
                "afftdn=nf={}:nr={}:tn={}",
                g(*noise_floor_db),
                g(*reduction_db),
                *track as u8
            ),
            Stage::Expander { ratio, range_db, .. } => format!(
                "agate=ratio={}:range={}",
                g(*ratio),
                g(round3(libm::pow(10.0, -range_db / 20.0)))
            ),
            Stage::Gain { db } => format!("volume={}dB", g(*db)),
            Stage::Loudness {
                target_lufs,
                true_peak_db,
                lra,
                ..
            } => format!(
                "loudnorm=I={}:TP={}:LRA={}",
                g(*target_lufs),
                g(*true_peak_db),
                g(*lra)
            ),
            Stage::Limiter { ceiling_db } => format!(
                "alimiter=limit={}:level=false",
                g(round3(libm::pow(10.0, ceiling_db / 20.0)))
            ),
        }
    }

    /// The samples this stage delays its output by when it runs live / streaming.
    pub fn latency(&self, sample_rate: f64, live: bool) -> usize {
        match self {
            Stage::Denoise { fft_size, .. } => super::dsp::denoise_fft_size(*fft_size, sample_rate, live),
            Stage::Loudness { .. } | Stage::Limiter { .. } => {
                super::dsp::TpLimiter::new(sample_rate, -1.0).latency()
            }
            _ => 0,
        }
    }
}

fn round3(x: f64) -> f64 {
    libm::round(x * 1000.0) / 1000.0
}

/// A number as ffmpeg's option parser takes it (no trailing `.0`).
fn g(x: f64) -> String {
    if x == libm::trunc(x) && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// An ordered list of stages, maybe named after the preset it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct PostChain {
    pub name: Option<String>,
    pub stages: Vec<Stage>,
}

/// The presets, in the order they are listed, with what each is for.
pub const PRESETS: [(&str, &str); 7] = [
    ("none", "nothing: the render as it is"),
    (
        "clean-medium",
        "the default: rumble cut, medium FFT denoise for line-input hiss (TD-3 / PD705), -14 LUFS / -1 dBTP",
    ),
    ("clean-light", "rumble cut, light FFT denoise, -14 LUFS / -1 dBTP"),
    ("clean-strong", "rumble cut, strong FFT denoise (can smear cymbals), -14 LUFS / -1 dBTP"),
    (
        "expander",
        "rumble cut, a soft downward expander (ratio 2) 30 dB under the program, -14 LUFS / -1 dBTP",
    ),
    ("treble-shelf", "rumble cut, -5 dB high shelf at 9 kHz, -14 LUFS / -1 dBTP"),
    ("master", "loudness only: -14 LUFS / -1 dBTP"),
];

/// The preset a bare `--post` (or `post` with no preset) means.
pub const DEFAULT_PRESET: &str = "clean-medium";

impl PostChain {
    pub fn empty() -> PostChain {
        PostChain {
            name: Some("none".into()),
            stages: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// A named preset ([`PRESETS`]).
    pub fn preset(name: &str) -> Option<PostChain> {
        let hp = || Stage::highpass(25.0);
        let stages = match name {
            "none" | "off" => Vec::new(),
            "clean-medium" | "clean" | "medium" | "default" => {
                vec![hp(), Stage::denoise(-50.0, 14.0), Stage::loudness()]
            }
            "clean-light" | "light" => vec![hp(), Stage::denoise(-55.0, 8.0), Stage::loudness()],
            "clean-strong" | "strong" => {
                vec![hp(), Stage::denoise(-45.0, 24.0), Stage::loudness()]
            }
            "expander" => vec![hp(), Stage::expander(), Stage::loudness()],
            "treble-shelf" | "shelf" => vec![
                hp(),
                Stage::HighShelf {
                    freq: 9_000.0,
                    gain_db: -5.0,
                    q: Q_BUTTER,
                },
                Stage::loudness(),
            ],
            "master" | "loudness" => vec![Stage::loudness()],
            _ => return None,
        };
        let canonical = PRESETS
            .iter()
            .map(|p| p.0)
            .find(|p| {
                *p == name
                    || matches!(
                        (name, *p),
                        ("off", "none")
                            | ("clean" | "medium" | "default", "clean-medium")
                            | ("light", "clean-light")
                            | ("strong", "clean-strong")
                            | ("shelf", "treble-shelf")
                            | ("loudness", "master")
                    )
            })
            .unwrap_or(name);
        Some(PostChain {
            name: Some(canonical.to_string()),
            stages,
        })
    }

    /// The preset names, in order.
    pub fn preset_names() -> Vec<&'static str> {
        PRESETS.iter().map(|p| p.0).collect()
    }

    /// The next (or previous) preset after `name`, wrapping: what a "preset" button steps.
    pub fn step_preset(name: &str, dir: i32) -> &'static str {
        let names = Self::preset_names();
        let at = names.iter().position(|n| *n == name).unwrap_or(0) as i32;
        let k = (at + dir).rem_euclid(names.len() as i32) as usize;
        names[k]
    }

    /// A chain from JSON: a preset's name (`"clean-medium"`), an array of stages and preset
    /// names (a preset in the array contributes its stages), `{"stages": [...]}` or
    /// `{"preset": NAME}`.
    pub fn from_json(v: &JsonValue) -> Result<PostChain, String> {
        match v {
            JsonValue::String(s) => {
                let t = s.trim();
                if t.starts_with('[') || t.starts_with('{') {
                    let parsed = JsonValue::parse(t).map_err(|e| format!("post chain: {e:?}"))?;
                    return Self::from_json(&parsed);
                }
                Self::preset(t).ok_or_else(|| unknown_preset(t))
            }
            JsonValue::Array(items) => {
                let mut stages = Vec::new();
                for x in items {
                    match x {
                        JsonValue::String(s) => {
                            let p = Self::preset(s).ok_or_else(|| unknown_preset(s))?;
                            stages.extend(p.stages);
                        }
                        other => stages.push(Stage::from_json(other)?),
                    }
                }
                Ok(PostChain { name: None, stages })
            }
            JsonValue::Object(o) => {
                if let Some(p) = o.get("preset").and_then(JsonValue::as_str) {
                    return Self::preset(p).ok_or_else(|| unknown_preset(p));
                }
                if let Some(s) = o.get("stages") {
                    let mut c = Self::from_json(s)?;
                    c.name = o.get("name").and_then(JsonValue::as_str).map(String::from);
                    return Ok(c);
                }
                if o.get("stage").is_some() {
                    return Ok(PostChain {
                        name: None,
                        stages: vec![Stage::from_json(v)?],
                    });
                }
                Err("post chain: a preset name, an array of stages, or {\"stages\": [...]}".into())
            }
            _ => Err("post chain: a preset name, an array of stages, or {\"stages\": [...]}".into()),
        }
    }

    /// What a request's `post` / `post_chain` mean together: `post_chain` (JSON) wins over
    /// `post` (a preset name, or a chain itself); neither is None.
    pub fn from_params(post: Option<&JsonValue>, chain: Option<&JsonValue>) -> Result<Option<PostChain>, String> {
        if let Some(c) = chain.filter(|c| !matches!(c, JsonValue::Null)) {
            return Self::from_json(c).map(Some);
        }
        match post {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Bool(false)) => Ok(None),
            Some(JsonValue::Bool(true)) => Ok(Self::preset(DEFAULT_PRESET)),
            Some(v) => Self::from_json(v).map(Some),
        }
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        if let Some(n) = &self.name {
            o.set("name", JsonValue::str(n.clone()));
        }
        o.set(
            "stages",
            JsonValue::Array(self.stages.iter().map(Stage::json).collect()),
        );
        o.set("ffmpeg", JsonValue::str(self.ffmpeg()));
        JsonValue::Object(o)
    }

    /// The ffmpeg `-af` graph the chain stands for (`anull` when empty).
    pub fn ffmpeg(&self) -> String {
        if self.stages.is_empty() {
            return "anull".into();
        }
        self.stages
            .iter()
            .map(Stage::ffmpeg)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Its name, or `custom`.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("custom")
    }

    /// The preset list as JSON: name, what it is for, its stages and its ffmpeg graph.
    pub fn presets_json() -> JsonValue {
        JsonValue::Array(
            PRESETS
                .iter()
                .map(|(name, about)| {
                    let c = Self::preset(name).expect("a listed preset exists");
                    let mut o = JsonObject::from_pairs([
                        ("name", JsonValue::str(*name)),
                        ("about", JsonValue::str(*about)),
                        ("default", (*name == DEFAULT_PRESET).into()),
                    ]);
                    o.set(
                        "stages",
                        JsonValue::Array(c.stages.iter().map(Stage::json).collect()),
                    );
                    o.set("ffmpeg", JsonValue::str(c.ffmpeg()));
                    JsonValue::Object(o)
                })
                .collect(),
        )
    }
}

fn unknown_preset(name: &str) -> String {
    format!(
        "no post preset {name:?} ({})",
        PRESETS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
    )
}
