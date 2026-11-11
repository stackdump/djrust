//! Ports of `SectionTimeline.swift` and `TimelineDots.swift`: a song's sections laid out along
//! a bar as fractions of its length, and the rolling timeline dots (SPEC mode 23:
//! `vizSpawnParticle` / `vizDrawTimeline`). Numbers only; the views draw what these return.

use alloc::string::String;
use alloc::vec::Vec;

use crate::pflow::Project;

/// `Deck.SectionInfo`: one section of a song with its start tick.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct SectionInfo {
    pub name: String,
    pub start_tick: i64,
    pub steps: i64,
    /// How many phrases the section is cut into: the longest phrase list of any role (1 when no
    /// role has one).
    pub phrase_count: i64,
    /// The phrase letters of the section's first role ("A", "B", "A", "C"); empty when the
    /// section has no phrase data.
    pub phrase_pattern: Vec<String>,
}

impl SectionInfo {
    /// `SectionInfo.sections(of:)`: the sections of a project's structure, in order, with
    /// their start ticks.
    pub fn sections_of(project: &Project) -> Vec<SectionInfo> {
        let mut start = 0;
        let mut out = Vec::new();
        for sec in &project.structure {
            let lists: Vec<&Vec<String>> = sec.phrases.values().collect();
            out.push(SectionInfo {
                name: sec.name.clone(),
                start_tick: start,
                steps: sec.steps,
                phrase_count: lists.iter().map(|l| l.len() as i64).max().unwrap_or(1),
                phrase_pattern: lists.first().map(|l| (*l).clone()).unwrap_or_default(),
            });
            start += sec.steps;
        }
        out
    }
}

/// An RGB colour with components 0…1 (`ScopeColor`'s r, g, b).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Rgb {
    /// `ScopeColor(hex: "#rrggbb")`.
    pub fn hex(v: u32) -> Rgb {
        Rgb {
            r: ((v >> 16) & 255) as f64 / 255.0,
            g: ((v >> 8) & 255) as f64 / 255.0,
            b: (v & 255) as f64 / 255.0,
        }
    }
}

/// One block of the strip (`SectionTimeline.Segment`).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub name: String,
    pub start: f64,
    pub width: f64,
    pub is_current: bool,
    /// How many phrases the section is cut into (1: not shown).
    pub phrase_count: i64,
    /// The phrase letters of the first role, e.g. ["A", "B", "A", "B"].
    pub phrase_pattern: Vec<String>,
}

impl Segment {
    pub fn new(name: &str, start: f64, width: f64, is_current: bool) -> Segment {
        Segment {
            name: name.into(),
            start,
            width,
            is_current,
            phrase_count: 1,
            phrase_pattern: Vec::new(),
        }
    }

    /// `renderTimeline`: phrase marks appear only when the section has more than one phrase.
    pub fn has_phrases(&self) -> bool {
        self.phrase_count > 1
    }

    /// The text printed after the name: the phrase letters run together ("ABAB"); None when
    /// the section shows no phrases or has no letters.
    pub fn phrase_label(&self) -> Option<String> {
        (self.has_phrases() && !self.phrase_pattern.is_empty())
            .then(|| self.phrase_pattern.concat())
    }

    /// Where the phrase dividers fall, as fractions of the block's own width.
    pub fn dividers(&self) -> Vec<f64> {
        if self.has_phrases() {
            (1..self.phrase_count)
                .map(|p| p as f64 / self.phrase_count as f64)
                .collect()
        } else {
            Vec::new()
        }
    }
}

/// Where a block's label goes (`SectionTimeline.LabelLayout`): the name and the phrase letters
/// are one run, centred in the block; the letters go only if the whole run fits with a little
/// air, else the name stands alone.
pub mod label_layout {
    /// `margin-left: 4px` of the phrase letters.
    pub const PHRASE_GAP: f64 = 4.0;
    /// Air kept on each side of a run that carries the letters.
    pub const MARGIN: f64 = 2.0;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Run {
        /// Left edge of the name.
        pub name_x: f64,
        /// Left edge of the letters; None when they are dropped.
        pub letters_x: Option<f64>,
    }

    pub fn run(block_width: f64, name_width: f64, letters_width: Option<f64>) -> Run {
        if let Some(lw) = letters_width {
            let whole = name_width + PHRASE_GAP + lw;
            if whole <= block_width - 2.0 * MARGIN {
                let x = (block_width - whole) / 2.0;
                return Run {
                    name_x: x,
                    letters_x: Some(x + name_width + PHRASE_GAP),
                };
            }
        }
        Run {
            name_x: (block_width - name_width) / 2.0,
            letters_x: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SectionTimeline {
    pub segments: Vec<Segment>,
    /// 0…1 along the song (clamped; 1 once it has ended).
    pub playhead: f64,
    pub total_ticks: i64,
}

impl SectionTimeline {
    /// `Deck.sectionTimeline`: the section bar for a song deck (None for a loop or a sample
    /// deck) at the net clock's `tick` and `tick_phase`.
    pub fn of_sections(
        secs: &[SectionInfo],
        tick: i64,
        tick_phase: f64,
    ) -> Option<SectionTimeline> {
        let total: i64 = secs.iter().map(|s| s.steps).sum();
        if total <= 0 {
            return None;
        }
        let current = secs
            .iter()
            .rposition(|s| s.start_tick <= tick && tick < s.start_tick + s.steps);
        let segments = secs
            .iter()
            .enumerate()
            .map(|(i, s)| Segment {
                name: s.name.clone(),
                start: s.start_tick as f64 / total as f64,
                width: s.steps as f64 / total as f64,
                is_current: Some(i) == current,
                phrase_count: s.phrase_count,
                phrase_pattern: s.phrase_pattern.clone(),
            })
            .collect();
        Some(SectionTimeline {
            segments,
            playhead: 1.0f64.min(0.0f64.max((tick as f64 + tick_phase) / total as f64)),
            total_ticks: total,
        })
    }

    /// The section a click at fraction `f` lands on.
    pub fn index_at_fraction(&self, f: f64) -> Option<usize> {
        if self.segments.is_empty() {
            return None;
        }
        let x = f.max(0.0).min(1.0);
        Some(
            self.segments
                .iter()
                .rposition(|s| s.start <= x + 1e-12)
                .unwrap_or(0),
        )
    }

    /// The section whose name colours it: stable across songs (a drop is always the same hue).
    pub fn hue(name: &str) -> f64 {
        let mut h: u32 = 2_166_136_261;
        for u in name.bytes() {
            h = (h ^ u as u32).wrapping_mul(16_777_619);
        }
        (h % 360) as f64 / 360.0
    }

    /// The names beats' structure strip colours (canvas.js `renderTimeline`).
    pub const BEATS_NAMES: [&'static str; 5] = ["intro", "verse", "chorus", "bridge", "outro"];
    /// Saturation of the name-hash colours, and the brightness of a block that is / is not the
    /// section being played.
    pub const HASH_SATURATION: f64 = 0.55;
    pub const CURRENT_BRIGHTNESS: f64 = 0.95;
    pub const RESTING_BRIGHTNESS: f64 = 0.45;

    /// canvas.js section colours: intro `#4a90d9`, verse `#2ecc71`, chorus `#e94560`, bridge
    /// `#9b59b6`, outro `#f5a623`, anything else `#888` (`ScopeColors.section`).
    pub fn beats_color(name: &str) -> Rgb {
        Rgb::hex(match name {
            "intro" => 0x4a90d9,
            "verse" => 0x2ecc71,
            "chorus" => 0xe94560,
            "bridge" => 0x9b59b6,
            "outro" => 0xf5a623,
            _ => 0x888888,
        })
    }

    /// A section's colour. The five names beats colours keep beats' colours (the current block
    /// at full strength, the others dimmed by the same ratio the strip always dimmed by); any
    /// other name keeps the stable per-name hue it had.
    pub fn color(name: &str, is_current: bool) -> Rgb {
        if Self::BEATS_NAMES.contains(&name) {
            let c = Self::beats_color(name);
            let k = if is_current {
                1.0
            } else {
                Self::RESTING_BRIGHTNESS / Self::CURRENT_BRIGHTNESS
            };
            return Rgb {
                r: c.r * k,
                g: c.g * k,
                b: c.b * k,
            };
        }
        Self::hsb(
            Self::hue(name),
            Self::HASH_SATURATION,
            if is_current {
                Self::CURRENT_BRIGHTNESS
            } else {
                Self::RESTING_BRIGHTNESS
            },
        )
    }

    /// HSB to RGB (the colour model `Color(hue:saturation:brightness:)` uses).
    pub fn hsb(h: f64, s: f64, v: f64) -> Rgb {
        let i = (h * 6.0) as i64 % 6;
        let f = h * 6.0 - (h * 6.0) as i64 as f64;
        let p = v * (1.0 - s);
        let q = v * (1.0 - f * s);
        let t = v * (1.0 - (1.0 - f) * s);
        let (r, g, b) = match i {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        Rgb { r, g, b }
    }
}

// MARK: timeline dots

/// One fire as the strip remembers it (`TimelineDot`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineDot {
    /// Audio seconds at which the note sounds.
    pub time: f64,
    pub color: Rgb,
}

/// A dot as drawn: a streak behind a head, at `x`, at `alpha` (`TimelineDotDraw`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineDotDraw {
    pub x: f64,
    pub alpha: f64,
    /// Length of the gradient streak that trails the head to its left.
    pub streak_length: f64,
    pub color: Rgb,
}

/// The constants of `vizDrawTimeline` (`TimelineGeometry`).
pub mod geometry {
    /// `_vizHistory.length > 200` → `shift()`.
    pub const CAP: usize = 200;
    /// `Math.max(2000, …)`: the window is never shorter than 2 s.
    pub const MIN_WINDOW_MS: f64 = 2000.0;
    /// `elapsed + 500`.
    pub const WINDOW_PAD_MS: f64 = 500.0;
    /// Four bars: `(240 / Math.max(60, tempo)) * 1000` ms.
    pub fn max_window_ms(tempo: f64) -> f64 {
        240.0 / tempo.max(60.0) * 1000.0
    }
    /// `pct < 0.85 ? 0.7 : …`: the dot holds 0.7 until 85 % of the window, then fades linearly.
    pub const BASE_ALPHA: f64 = 0.7;
    pub const FADE_START: f64 = 0.85;
    /// `Math.min(20, (w / windowMs) * 120)`.
    pub const STREAK_MAX: f64 = 20.0;
    pub const STREAK_PER_WIDTH: f64 = 120.0;
    pub const STRIP_TOP: f64 = 24.0;
    pub const STREAK_Y: f64 = 27.0 - STRIP_TOP;
    pub const STREAK_HEIGHT: f64 = 6.0;
    pub const HEAD_Y: f64 = 30.0 - STRIP_TOP;
    pub const HEAD_RADIUS: f64 = 4.0;
    /// The head's circle (2...10) with two points of air above and below.
    pub const STRIP_HEIGHT: f64 = 12.0;
}

/// The history behind the strip: up to 200 dots, oldest first (`TimelineDots`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TimelineDots {
    dots: Vec<TimelineDot>,
}

impl TimelineDots {
    pub fn new() -> TimelineDots {
        TimelineDots::default()
    }

    pub fn dots(&self) -> &[TimelineDot] {
        &self.dots
    }

    pub fn is_empty(&self) -> bool {
        self.dots.is_empty()
    }

    pub fn count(&self) -> usize {
        self.dots.len()
    }

    /// `vizSpawnParticle`: push, and drop the oldest past 200.
    pub fn add(&mut self, dot: TimelineDot) {
        self.dots.push(dot);
        if self.dots.len() > geometry::CAP {
            self.dots.remove(0);
        }
    }

    pub fn remove_all(&mut self) {
        self.dots.clear();
    }

    /// The window in ms: `max(2000, min(maxWindowMs, elapsed + 500))`, `elapsed` being the age
    /// of the oldest dot (0 for none). None when the tempo is not a finite number: the JS
    /// bails out there.
    pub fn window_ms(&self, now: f64, tempo: f64) -> Option<f64> {
        // JS `Math.max(60, NaN)` is NaN; Rust's `max` would answer 60, so say it outright
        if tempo.is_nan() {
            return None;
        }
        let elapsed = self
            .dots
            .first()
            .map(|d| (now - d.time) * 1000.0)
            .unwrap_or(0.0);
        let w = geometry::MIN_WINDOW_MS
            .max(geometry::max_window_ms(tempo).min(elapsed + geometry::WINDOW_PAD_MS));
        (w.is_finite() && w > 0.0).then_some(w)
    }

    /// `alpha` of a dot at `pct = age / window`.
    pub fn alpha_at_fraction(pct: f64) -> f64 {
        if pct < geometry::FADE_START {
            geometry::BASE_ALPHA
        } else {
            geometry::BASE_ALPHA
                * (1.0 - (pct - geometry::FADE_START) / (1.0 - geometry::FADE_START))
        }
    }

    /// The dots of one frame, oldest first, for a strip `width` wide at audio time `now`. A dot
    /// older than the window is skipped; so is one whose position is not finite.
    pub fn layout(&self, now: f64, width: f64, tempo: f64) -> Vec<TimelineDotDraw> {
        let Some(window) = self.window_ms(now, tempo) else {
            return Vec::new();
        };
        let streak = geometry::STREAK_MAX.min(width / window * geometry::STREAK_PER_WIDTH);
        let mut out = Vec::with_capacity(self.dots.len());
        for d in &self.dots {
            let age = (now - d.time) * 1000.0;
            if age > window {
                continue;
            }
            let x = width - age / window * width;
            if !x.is_finite() {
                continue;
            }
            out.push(TimelineDotDraw {
                x,
                alpha: Self::alpha_at_fraction(age / window),
                streak_length: streak,
                color: d.color,
            });
        }
        out
    }

    /// Drop the dots older than the window (after a draw, as `vizDrawTimeline` does).
    pub fn prune(&mut self, now: f64, tempo: f64) {
        let Some(window) = self.window_ms(now, tempo) else {
            return;
        };
        let mut n = 0;
        while n < self.dots.len() && (now - self.dots[n].time) * 1000.0 > window {
            n += 1;
        }
        if n > 0 {
            self.dots.drain(0..n);
        }
    }
}
