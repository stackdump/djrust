//! Groove: `GrooveTemplate` (`Musicality/GrooveTemplate.swift`), the runner's `FeelTable`
//! and `Drift` (`Beats/Wave/WaveRunner.swift`), and the hit records `onHit` reports
//! (`Musicality/GrooveReport.swift`'s `HitRecord`).
//!
//! A family's groove template (ROADMAP 6.5): for each drum voice and each sixteenth of a
//! 4/4 bar, how late a human drummer plays it (ms) and how hard (a velocity scale around
//! 1), plus a swing ratio that depends on tempo. Distilled from the Groove MIDI Dataset
//! (CC BY 4.0) by beats-btw-dj's `scripts/groove-templates.py` into `data/grooves.json`
//! (the same file as the Swift `Resources/beats/grooves.json`).
//!
//! Offsets are never negative: the runner can hold a note back, not play it before its
//! tick. A position with fewer than `minHits` hits in the dataset is neutral (0 ms, ×1).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use crate::gen::tables::BeatsTables;
use crate::json::JsonValue;

pub const GROOVES_JSON: &str = include_str!("../../data/grooves.json");

/// The drum voices a template has, in the file's order. MIDI notes fold onto them the way
/// the dataset's kit (Roland TD-11) does, plus beats' clap (39) as a snare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice {
    Kick = 0,
    Snare,
    Hihat,
    OpenHat,
    Ride,
}

impl Voice {
    pub const ALL: [Voice; 5] = [
        Voice::Kick,
        Voice::Snare,
        Voice::Hihat,
        Voice::OpenHat,
        Voice::Ride,
    ];

    pub fn name(self) -> &'static str {
        ["kick", "snare", "hihat", "openHat", "ride"][self as usize]
    }

    pub fn from_note(note: i64) -> Option<Voice> {
        match note {
            35 | 36 => Some(Voice::Kick),
            37..=40 => Some(Voice::Snare),
            22 | 42 | 44 => Some(Voice::Hihat),
            26 | 46 => Some(Voice::OpenHat),
            51 | 53 | 59 => Some(Voice::Ride),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum Grid {
    Sixteenth,
    Eighth,
}

/// Linn's swing ratio: where the off-beat member of a pair starts, as a share of the pair
/// (0.50 straight … 0.66 triplet). The pair is two sixteenths or two eighths (the jazz
/// family).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Swing {
    pub grid: Grid,
    pub intercept: f64,
    pub per_bpm: f64,
    /// The tempi the line was fitted over; outside it the ratio holds at the edge.
    pub bpm_range: (f64, f64),
}

impl Swing {
    pub const STRAIGHT: f64 = 0.50;
    pub const TRIPLET: f64 = 0.66;

    /// `intercept + perBpm × bpm`, bpm clamped to `bpm_range`, ratio to straight … triplet.
    pub fn ratio(&self, bpm: f64) -> f64 {
        let b = bpm.max(self.bpm_range.0).min(self.bpm_range.1);
        (self.intercept + self.per_bpm * b)
            .max(Self::STRAIGHT)
            .min(Self::TRIPLET)
    }

    /// Sixteenths in a pair.
    pub fn pair_steps(&self) -> i64 {
        if self.grid == Grid::Eighth {
            4
        } else {
            2
        }
    }

    /// The off-beat member's delay in ms at `bpm`: (ratio − ½) of the pair.
    pub fn delay_ms(&self, bpm: f64) -> f64 {
        let sixteenth_ms = 15_000.0 / bpm;
        (self.ratio(bpm) - Self::STRAIGHT) * self.pair_steps() as f64 * sixteenth_ms
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct GrooveTemplate {
    pub family: String,
    pub index: i64,
    /// The dataset styles averaged into it, and how many files they had.
    pub styles: Vec<String>,
    pub files: i64,
    pub swing: Swing,
    /// `[voice][step]`, ms late, 0 … `MAX_OFFSET_MS`.
    pub offset_ms: Vec<Vec<f64>>,
    /// `[voice][step]`, mean velocity there / the voice's mean velocity.
    pub velocity: Vec<Vec<f64>>,
    /// `[voice][step]`, the dataset's hit count behind each cell.
    pub hits: Vec<Vec<i64>>,
}

/// beats' structure.js family order (`BeatsTables.family(of:)`).
pub const FAMILY_NAMES: [&str; 4] = ["dance", "song", "jazz", "chill"];
pub const STEPS: usize = 16;
pub const MAX_OFFSET_MS: f64 = 15.0;

/// `GrooveTemplate.Parsed.provenance`
#[derive(Clone, Debug)]
pub struct Provenance {
    pub url: String,
    pub sha256: String,
    pub license: String,
    pub min_hits: i64,
}

static SHARED: OnceBox<(Provenance, BTreeMap<String, GrooveTemplate>)> = OnceBox::new();

impl GrooveTemplate {
    pub fn offset_ms(&self, voice: Voice, step: usize) -> f64 {
        self.offset_ms[voice as usize][step & (STEPS - 1)]
    }

    pub fn velocity(&self, voice: Voice, step: usize) -> f64 {
        self.velocity[voice as usize][step & (STEPS - 1)]
    }

    fn table() -> &'static (Provenance, BTreeMap<String, GrooveTemplate>) {
        SHARED.get_or_init(|| {
            Box::new(
                GrooveTemplate::parse(
                    &JsonValue::parse(GROOVES_JSON).expect("grooves.json parses"),
                )
                .expect("bundled grooves.json is valid"),
            )
        })
    }

    /// Every family's template, by name.
    pub fn all() -> &'static BTreeMap<String, GrooveTemplate> {
        &Self::table().1
    }

    pub fn provenance() -> &'static Provenance {
        &Self::table().0
    }

    /// The template for a family (`BeatsTables.family(of:)`'s index).
    pub fn family_index(index: i64) -> Option<&'static GrooveTemplate> {
        Self::all().values().find(|t| t.index == index)
    }

    /// The template a genre plays with: its beats family's (an unknown genre is beats'
    /// default family, song).
    pub fn for_genre(genre: &str) -> Option<&'static GrooveTemplate> {
        Self::family_index(BeatsTables::shared().family_of(genre))
    }

    /// `GrooveTemplate.parse`: the file's families, validated.
    pub fn parse(f: &JsonValue) -> Result<(Provenance, BTreeMap<String, GrooveTemplate>), String> {
        let bad = |why: &str| alloc::format!("grooves.json: {why}");
        let p = f.get("provenance").ok_or_else(|| bad("provenance"))?;
        let provenance = Provenance {
            url: p
                .get("url")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| bad("url"))?
                .into(),
            sha256: p
                .get("sha256")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| bad("sha256"))?
                .into(),
            license: p
                .get("license")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| bad("license"))?
                .into(),
            min_hits: p
                .get("minHits")
                .and_then(JsonValue::as_i64)
                .ok_or_else(|| bad("minHits"))?,
        };
        let voices = f
            .get("voices")
            .and_then(JsonValue::strings)
            .ok_or_else(|| bad("voices"))?;
        let want: Vec<String> = Voice::ALL.iter().map(|v| v.name().into()).collect();
        if voices != want {
            return Err(bad("voices"));
        }
        let fams = f
            .get("families")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| bad("families"))?;
        let mut out = BTreeMap::new();
        for (name, fam) in fams.iter() {
            let sw = fam
                .get("swing")
                .ok_or_else(|| bad(&alloc::format!("{name} swing")))?;
            let grid = match sw.get("grid").and_then(JsonValue::as_str) {
                Some("16th") => Grid::Sixteenth,
                Some("8th") => Grid::Eighth,
                _ => return Err(bad(&alloc::format!("{name} swing"))),
            };
            let range = sw
                .get("bpmRange")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| bad("bpmRange"))?;
            if range.len() != 2 {
                return Err(bad(&alloc::format!("{name} swing")));
            }
            let (lo, hi) = (
                range[0].as_f64().unwrap_or(0.0),
                range[1].as_f64().unwrap_or(0.0),
            );
            if lo > hi {
                return Err(bad(&alloc::format!("{name} swing")));
            }
            let nums = |v: &JsonValue| -> Option<Vec<f64>> {
                v.as_array()?.iter().map(JsonValue::as_f64).collect()
            };
            let (mut off, mut vel, mut hits) = (Vec::new(), Vec::new(), Vec::new());
            for v in Voice::ALL {
                let c = fam
                    .get("voices")
                    .and_then(|x| x.get(v.name()))
                    .ok_or_else(|| bad(&alloc::format!("{name}.{}", v.name())))?;
                let o = c.get("offsetMs").and_then(nums);
                let ve = c.get("velocity").and_then(nums);
                let h = c.get("hits").and_then(nums);
                match (o, ve, h) {
                    (Some(o), Some(ve), Some(h))
                        if o.len() == STEPS && ve.len() == STEPS && h.len() == STEPS =>
                    {
                        off.push(o);
                        vel.push(ve);
                        hits.push(h.into_iter().map(|x| x as i64).collect());
                    }
                    _ => return Err(bad(&alloc::format!("{name}.{}", v.name()))),
                }
            }
            out.insert(
                String::from(name),
                GrooveTemplate {
                    family: name.into(),
                    index: fam.get("index").and_then(JsonValue::as_i64).unwrap_or(0),
                    styles: fam
                        .get("styles")
                        .and_then(JsonValue::strings)
                        .unwrap_or_default(),
                    files: fam.get("files").and_then(JsonValue::as_i64).unwrap_or(0),
                    swing: Swing {
                        grid,
                        intercept: sw
                            .get("intercept")
                            .and_then(JsonValue::as_f64)
                            .unwrap_or(0.0),
                        per_bpm: sw.get("perBpm").and_then(JsonValue::as_f64).unwrap_or(0.0),
                        bpm_range: (lo, hi),
                    },
                    offset_ms: off,
                    velocity: vel,
                    hits,
                },
            );
        }
        Ok((provenance, out))
    }

    /// A template at one tempo and sample rate, flattened into fixed arrays
    /// (`voice × 16 + step`) with the swing folded into each off-beat step's delay.
    pub fn prepared(&self, bpm: f64, sample_rate: f64) -> Prepared {
        let swing_ms = self.swing.delay_ms(bpm);
        let pair = self.swing.pair_steps() as usize;
        let half = pair / 2;
        let mut delay = alloc::vec![0.0; Voice::ALL.len() * STEPS];
        let mut vel = alloc::vec![1.0; delay.len()];
        for v in Voice::ALL {
            for s in 0..STEPS {
                let ms =
                    self.offset_ms[v as usize][s] + if s % pair == half { swing_ms } else { 0.0 };
                delay[v as usize * STEPS + s] = ms / 1000.0 * sample_rate;
                vel[v as usize * STEPS + s] = self.velocity[v as usize][s];
            }
        }
        Prepared {
            bpm,
            sample_rate,
            swing_ratio: self.swing.ratio(bpm),
            delay_samples: delay,
            velocity_scale: vel,
        }
    }
}

/// `GrooveTemplate.Prepared`
#[derive(Clone, Debug)]
pub struct Prepared {
    pub bpm: f64,
    pub sample_rate: f64,
    /// The swing ratio at `bpm`.
    pub swing_ratio: f64,
    /// Samples late, microtiming + swing.
    pub delay_samples: Vec<f64>,
    pub velocity_scale: Vec<f64>,
}

impl Prepared {
    fn cell(voice: Voice, step: usize) -> usize {
        voice as usize * STEPS + (step & (STEPS - 1))
    }
    pub fn delay(&self, voice: Voice, step: usize) -> f64 {
        self.delay_samples[Self::cell(voice, step)]
    }
    pub fn velocity(&self, voice: Voice, step: usize) -> f64 {
        self.velocity_scale[Self::cell(voice, step)]
    }
}

/// `FeelTable`: what the runner reads of a template (`.feel`), the swing worked out at the
/// tempo playing.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct FeelTable {
    pub(crate) offset_ms: Vec<f64>,
    pub(crate) velocity: Vec<f64>,
    pub(crate) swing: Swing,
}

impl FeelTable {
    pub fn new(t: &GrooveTemplate) -> FeelTable {
        let mut off = alloc::vec![0.0; Voice::ALL.len() * STEPS];
        let mut vel = alloc::vec![1.0; off.len()];
        for v in Voice::ALL {
            for s in 0..STEPS {
                off[Self::cell(v, s)] = t.offset_ms(v, s).max(0.0);
                vel[Self::cell(v, s)] = t.velocity(v, s);
            }
        }
        FeelTable {
            offset_ms: off,
            velocity: vel,
            swing: t.swing,
        }
    }

    pub(crate) fn cell(voice: Voice, step: usize) -> usize {
        voice as usize * STEPS + (step & (STEPS - 1))
    }

    /// The swing delay (ms) of bar step `step` at `bpm`: (2r − 1) sixteenths on the
    /// off-beat sixteenth of each pair (twice that on the off-beat eighth of the jazz
    /// family's), 0 elsewhere.
    pub(crate) fn swing_ms(&self, step: usize, bpm: f64) -> f64 {
        let pair = self.swing.pair_steps() as usize;
        if step % pair != pair / 2 || !(bpm > 0.0) {
            return 0.0;
        }
        self.swing.delay_ms(bpm)
    }
}

/// `.drift` (ROADMAP 6.5): humanize as seeded 1/f noise. Voss-McCartney, stateless and
/// random-access: row k of `OCTAVES` holds a hash of (key, net, stream, k, its own
/// counter), the counter stepping on the ticks whose trailing-zero count is k, plus a
/// white row that moves every tick. Their sum is pink between 1/2^(octaves+1) and 1/2
/// cycles a tick, so a drummer's hands drift over bars instead of jittering hit to hit; and
/// any tick's value is computed directly, so a seek, a section jump or a replay plays the
/// same noise there.
pub struct Drift;

impl Drift {
    /// Rows of the sum: the slowest moves every 2^octaves ticks (16 bars of sixteenths).
    pub const OCTAVES: i64 = 8;
    /// The largest velocity swing, × humanize (h = humanize% / 100).
    pub const VELOCITY_SPAN: f64 = 12.0;
    /// The largest lateness, ms, × h (never early).
    pub const LATE_SPAN_MS: f64 = 3.0;

    /// The sum of `OCTAVES + 1` uniform rows divided by this has an SD of 1/3; clamped
    /// to ±1.
    fn scale() -> f64 {
        3.0 * libm::sqrt((Self::OCTAVES + 1) as f64 / 3.0)
    }

    /// The key for a track's seed (`WaveOptions.voicingSeed`).
    pub fn key(seed: f64) -> u64 {
        Self::mix(seed.to_bits() ^ 0x6472_6966_745f_6b79)
    }

    /// The noise in [-1, 1] at `tick` (≥ 0) for `net`'s `stream` (0 velocity, 1 timing).
    pub fn pink(key: u64, net: i64, stream: i64, tick: i64) -> f64 {
        let n = net.wrapping_mul(2).wrapping_add(stream);
        let base = Self::mix(key ^ (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut sum = Self::uniform(base, Self::OCTAVES, tick);
        for k in 0..Self::OCTAVES {
            sum += Self::uniform(base, k, tick.wrapping_add(1 << k) >> (k + 1));
        }
        let v = sum / Self::scale();
        if v < -1.0 {
            -1.0
        } else if v > 1.0 {
            1.0
        } else {
            v
        }
    }

    /// The velocity offset, ± `VELOCITY_SPAN` × h, rounded.
    pub fn velocity(key: u64, net: i64, tick: i64, humanize: f64) -> i64 {
        libm::round(Self::pink(key, net, 0, tick) * Self::VELOCITY_SPAN * humanize) as i64
    }

    /// How late, ms: 0…`LATE_SPAN_MS` × h.
    pub fn late_ms(key: u64, net: i64, tick: i64, humanize: f64) -> f64 {
        (Self::pink(key, net, 1, tick) + 1.0) / 2.0 * Self::LATE_SPAN_MS * humanize
    }

    /// Uniform in [-1, 1).
    fn uniform(base: u64, row: i64, index: i64) -> f64 {
        let x = Self::mix(
            base ^ ((row.wrapping_add(1)) as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
                ^ (index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9),
        );
        (x >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// splitmix64's finaliser.
    fn mix(z0: u64) -> u64 {
        let mut z = z0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// beats' browser timing hash: a hash of (tick, net, transition), not a random draw, so a
/// replay plays the same groove (`grooveHash`).
pub(crate) fn groove_hash(a: i64, b: i64, c: i64) -> u32 {
    let mut x = (a.wrapping_mul(73_856_093)
        ^ b.wrapping_mul(19_349_663)
        ^ c.wrapping_mul(83_492_791)) as u32;
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// `HitRecord`: one onset the runner played (`onHit`, offline measurement).
#[derive(Clone, Debug, PartialEq)]
pub struct HitRecord {
    /// The sample it plays on, counted like the render's frames (`process_at`'s start).
    pub frame: i64,
    /// The sample of its tick: where it would have played straight. `frame - grid_frame`
    /// is how late the groove played it.
    pub grid_frame: i64,
    /// The graph's tick after the fire (1-based); a tick is a sixteenth.
    pub tick: i64,
    pub net: String,
    pub note: i64,
    /// The lane's MIDI channel (-1 for none).
    pub channel: i64,
    /// The lane's kind (`kick`, `snare`, `clap`, `hat`, `openhat`, `crash`, `tonal`, `v`).
    pub kind: String,
    /// The velocity played (after humanize).
    pub velocity: i64,
}

impl HitRecord {
    /// The sixteenth of its bar, 0…15.
    pub fn step(&self) -> i64 {
        ((self.tick - 1) % 16 + 16) % 16
    }
    pub fn delay_frames(&self) -> i64 {
        self.frame - self.grid_frame
    }
    /// A drum lane (not a tonal one or a voice override).
    pub fn is_drum(&self) -> bool {
        self.kind != "tonal" && self.kind != "v"
    }
}
