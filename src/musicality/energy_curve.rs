//! Port of `Musicality/EnergyCurve.swift`: the arrangement's energy arc, as the deck plays it
//! (`DJMusicality.Fixes.energy`), the buildup sweep (`.sweep`) and the sweep filter.
//!
//! beats' cohesion-v2 composer gives every section an `energy` and a `filterOpen` and writes
//! them into a `feel-curve` net as `set-feel` controls. dj plays `energy` as the section's
//! level (the loudest section at unity) and `filterOpen` as a lowpass on the tonal parts, a
//! buildup sweeping both up into the section after it. Built from the project off the audio
//! thread; the runner only reads it.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::musicality::loudness::Loudness;
use crate::pflow::Project;

/// Sections that build into the next one.
pub const RAMP_SECTIONS: [&str; 2] = ["buildup", "pre-chorus"];

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct EnergyCurve {
    /// First step of each section, ascending.
    pub start: Vec<i64>,
    /// Each section's gain (linear).
    pub gain: Vec<f64>,
    /// The tonal lowpass cutoff (Hz).
    pub cutoff: Vec<f64>,
    /// The section ramps from its own values to the next section's (a buildup).
    pub ramp: Vec<bool>,
}

impl EnergyCurve {
    /// dB under the loudest section for a section with no energy at all.
    pub const SPAN_DB: f64 = 8.0;
    /// A section the composer already made sparse is brought up towards its level by at most
    /// this much, never pushed further down than `SPAN_DB`.
    pub const MAX_BOOST_DB: f64 = 4.0;
    /// Cutoff of a fully open section, and the octaves it falls at `filterOpen` 0.
    pub const OPEN_HZ: f64 = 18_000.0;
    pub const CLOSED_OCTAVES: f64 = 5.5;

    /// The curve the project's `feel-curve` net describes, or None (a loop, a project without
    /// one, or one whose sections all have the same energy and filter). A section's level is
    /// set against what it already plays: each section is probed (`Loudness::measure`) and
    /// given the gain that puts it `SPAN_DB × (1 − energy)` under the most energetic one.
    pub fn new(project: &Project) -> Option<EnergyCurve> {
        if project.structure.len() <= 1 {
            return None;
        }
        let feel = project.nets.get("feel-curve")?;
        let mut points: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        for (t, c) in feel.control_bindings.iter() {
            if c.action != "set-feel" {
                continue;
            }
            let Some(step) = t.strip_prefix("ft").and_then(|s| s.parse::<i64>().ok()) else {
                continue;
            };
            let params = c.macro_params.as_ref();
            let (Some(x), Some(y)) = (
                params.and_then(|m| m.get("x")).and_then(|v| v.as_f64()),
                params.and_then(|m| m.get("y")).and_then(|v| v.as_f64()),
            ) else {
                continue;
            };
            points.insert(step, (x, y));
        }
        // the composer marks each section name once, at its first start
        let mut by_name: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        let mut first_start: BTreeMap<String, i64> = BTreeMap::new();
        let mut at = 0;
        for s in &project.structure {
            if !first_start.contains_key(&s.name) {
                first_start.insert(s.name.clone(), at);
                if let Some(p) = points.get(&at) {
                    by_name.insert(s.name.clone(), *p);
                }
            }
            at += s.steps;
        }
        let x_max = by_name.values().map(|p| p.0).fold(None, max_opt)?;
        let y_max = by_name.values().map(|p| p.1).fold(None, max_opt)?;
        if !(x_max > 0.0 && y_max > 0.0) {
            return None;
        }
        let distinct = |f: fn(&(f64, f64)) -> f64| {
            let mut v: Vec<f64> = by_name.values().map(f).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v.dedup();
            v.len() > 1
        };
        if !(distinct(|p| p.0) || distinct(|p| p.1)) {
            return None;
        }
        // `max(by: x <)` over a Dictionary: ties break by iteration order, which is
        // arbitrary in Swift; here the first by name
        let mut peak: Option<(&String, f64)> = None;
        for (n, p) in &by_name {
            if peak.map_or(true, |b| b.1 < p.0) {
                peak = Some((n, p.0));
            }
        }
        let peak = peak?.0.clone();
        let reference = Loudness::measure(project, Some(first_start[&peak]), Loudness::PROBE_RATE)?;
        let mut section_db: BTreeMap<String, f64> = BTreeMap::new();
        for (name, p) in &by_name {
            let target = -Self::SPAN_DB * (1.0 - (p.0 / x_max).min(1.0));
            if *name == peak {
                section_db.insert(name.clone(), 0.0);
                continue;
            }
            let Some(level) =
                Loudness::measure(project, Some(first_start[name]), Loudness::PROBE_RATE)
            else {
                section_db.insert(name.clone(), target);
                continue;
            };
            section_db.insert(
                name.clone(),
                (target - (level - reference))
                    .min(Self::MAX_BOOST_DB)
                    .max(-Self::SPAN_DB),
            );
        }
        let mut start = Vec::new();
        let mut gain = Vec::new();
        let mut cutoff = Vec::new();
        let mut ramp = Vec::new();
        at = 0;
        for s in &project.structure {
            let y = by_name.get(&s.name).map_or(y_max, |p| p.1);
            start.push(at);
            gain.push(libm::pow(
                10.0,
                section_db.get(&s.name).copied().unwrap_or(0.0) / 20.0,
            ));
            cutoff.push(
                Self::OPEN_HZ
                    * libm::pow(2.0, -Self::CLOSED_OCTAVES * (1.0 - (y / y_max).min(1.0))),
            );
            ramp.push(RAMP_SECTIONS.contains(&s.name.as_str()));
            at += s.steps;
        }
        if let Some(l) = ramp.last_mut() {
            *l = false;
        }
        Some(EnergyCurve {
            start,
            gain,
            cutoff,
            ramp,
        })
    }

    /// Gain and cutoff at `step` (the song's sixteenth). Allocation-free.
    pub fn at(&self, step: i64) -> (f64, f64) {
        let mut k = 0;
        while k + 1 < self.start.len() && self.start[k + 1] <= step {
            k += 1;
        }
        if !self.ramp[k] {
            return (self.gain[k], self.cutoff[k]);
        }
        let len = (self.start[k + 1] - self.start[k]) as f64;
        // accelerating, as a riser does: most of the sweep in the section's last bars
        let x = ((step - self.start[k]) as f64 / len).max(0.0).min(1.0);
        let w = x * x;
        (
            self.gain[k] + (self.gain[k + 1] - self.gain[k]) * w,
            self.cutoff[k] * libm::pow(self.cutoff[k + 1] / self.cutoff[k], w),
        )
    }

    /// How far through a ramp section (a buildup) `step` is: 0 on its first step, rising
    /// linearly towards 1 at the next section; None outside one.
    pub fn ramp_progress(&self, step: i64) -> Option<f64> {
        let mut k = 0;
        while k + 1 < self.start.len() && self.start[k + 1] <= step {
            k += 1;
        }
        if !(self.ramp[k] && step >= self.start[k] && k + 1 < self.start.len()) {
            return None;
        }
        Some((step - self.start[k]) as f64 / (self.start[k + 1] - self.start[k]) as f64)
    }
}

fn max_opt(m: Option<f64>, v: f64) -> Option<f64> {
    Some(m.map_or(v, |m| if v > m { v } else { m }))
}

/// `.sweep`: the resonance sweep of a buildup. A subtractive voice struck in a ramp section
/// has its filter's resting point scaled from `BASE_FROM` at the section's start to `BASE_TO`
/// at its end, and its Q raised towards `Q_TO` (accelerating).
pub struct BuildupSweep;

impl BuildupSweep {
    pub const BASE_FROM: f64 = 0.6;
    pub const BASE_TO: f64 = 2.0;
    pub const Q_TO: f64 = 2.5;

    /// The filter base at ramp progress `x` (0…1) for a voice whose own base is `stock`.
    pub fn base(stock: f64, x: f64) -> f64 {
        stock * Self::BASE_FROM * libm::pow(Self::BASE_TO / Self::BASE_FROM, x.max(0.0).min(1.0))
    }

    /// The filter Q at ramp progress `x` for a voice whose own Q is `stock` (never below it).
    pub fn q(stock: f64, x: f64) -> f64 {
        let x = x.max(0.0).min(1.0);
        stock + (stock.max(Self::Q_TO) - stock) * x * x
    }
}

/// A two-pole lowpass (Q 0.707) that can be retuned every sample without clicks: the
/// trapezoidal state-variable filter.
#[derive(Clone, Debug)]
pub struct SweepFilter {
    ic1: f64,
    ic2: f64,
    a1: f64,
    a2: f64,
    a3: f64,
}

impl Default for SweepFilter {
    fn default() -> Self {
        SweepFilter {
            ic1: 0.0,
            ic2: 0.0,
            a1: 1.0,
            a2: 0.0,
            a3: 0.0,
        }
    }
}

impl SweepFilter {
    pub fn tune(&mut self, hz: f64, sr: f64) {
        let g = libm::tan(core::f64::consts::PI * hz.min(sr * 0.45) / sr);
        let k = libm::sqrt(2.0);
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    pub fn process(&mut self, x: f64) -> f64 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v2
    }
}

/// The sampled instruments' bus made mono below `hz` (`.stereo`; Swift's `SideHighpass`): EXS
/// instruments carry stereo samples, upright and electric basses included, so their low end
/// came out of both sides differently. The side signal goes through a fourth-order highpass
/// (two trapezoidal state-variable stages, Q 0.707); the mid is untouched. The host that
/// renders the sampled instruments runs it over their block (`WaveProject::side_slots` says
/// which slots).
#[derive(Clone, Debug)]
pub struct SideHighpass {
    s1: (f64, f64),
    s2: (f64, f64),
    a1: f64,
    a2: f64,
    a3: f64,
    k: f64,
}

impl SideHighpass {
    pub fn new(hz: f64, sr: f64) -> SideHighpass {
        let k = libm::sqrt(2.0);
        let g = libm::tan(core::f64::consts::PI * hz.min(sr * 0.45) / sr);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        SideHighpass {
            s1: (0.0, 0.0),
            s2: (0.0, 0.0),
            a1,
            a2,
            a3: g * a2,
            k,
        }
    }

    #[inline]
    fn stage(x: f64, s: &mut (f64, f64), a1: f64, a2: f64, a3: f64, k: f64) -> f64 {
        let v3 = x - s.1;
        let v1 = a1 * s.0 + a2 * v3;
        let v2 = s.1 + a2 * s.0 + a3 * v3;
        s.0 = 2.0 * v1 - s.0;
        s.1 = 2.0 * v2 - s.1;
        x - k * v1 - v2
    }

    /// One stereo sample, in place.
    #[inline]
    pub fn process(&mut self, l: &mut f32, r: &mut f32) {
        let m = (*l as f64 + *r as f64) / 2.0;
        let x = Self::stage(
            (*l as f64 - *r as f64) / 2.0,
            &mut self.s1,
            self.a1,
            self.a2,
            self.a3,
            self.k,
        );
        let s = Self::stage(x, &mut self.s2, self.a1, self.a2, self.a3, self.k);
        *l = (m + s) as f32;
        *r = (m - s) as f32;
    }

    pub fn reset(&mut self) {
        self.s1 = (0.0, 0.0);
        self.s2 = (0.0, 0.0);
    }
}
