//! Port of `Musicality/GrooveReport.swift`: how a render's hits sit against the grid
//! (ROADMAP 6.5, exit M6), from the runner's `HitRecord`s.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::wave::groove::HitRecord;

/// One lane kind's hits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrooveVoice {
    pub hits: usize,
    /// Mean count of distinct velocities at each of its bar positions (positions it hits at
    /// least twice): 1 = every hit at a position is the same strength.
    pub distinct_velocities_per_position: f64,
    /// Mean lateness, ms, and its standard deviation.
    pub mean_delay_ms: f64,
    pub sd_delay_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrooveReport {
    pub hits: usize,
    pub drum_hits: usize,
    /// `GrooveVoice.distinct_velocities_per_position` over every drum (kind, position) hit
    /// twice or more.
    pub distinct_velocities_per_position: f64,
    /// How much of the drums' timing is a fixed pattern by bar position: 1 − the variance of
    /// the delay within a (kind, position) over its total variance. 1 when nothing moves.
    pub consistency: f64,
    /// Linn's ratio, measured; None without hits on both even and odd steps.
    pub swing_ratio: Option<f64>,
    /// Mean lateness of every hit, ms, and its standard deviation.
    pub microtiming_ms: f64,
    pub microtiming_sd_ms: f64,
    pub voices: BTreeMap<String, GrooveVoice>,
}

fn mean_sd(x: &[f64]) -> (f64, f64) {
    if x.is_empty() {
        return (0.0, 0.0);
    }
    let m = x.iter().sum::<f64>() / x.len() as f64;
    let v = x.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / x.len() as f64;
    (m, libm::sqrt(v))
}

impl Default for GrooveReport {
    fn default() -> Self {
        GrooveReport {
            hits: 0,
            drum_hits: 0,
            distinct_velocities_per_position: 0.0,
            consistency: 1.0,
            swing_ratio: None,
            microtiming_ms: 0.0,
            microtiming_sd_ms: 0.0,
            voices: BTreeMap::new(),
        }
    }
}

impl GrooveReport {
    /// `bpm` is the tempo the hits were played at (a tick is 15000 / bpm ms).
    pub fn new(records: &[HitRecord], sample_rate: f64, bpm: f64) -> GrooveReport {
        let mut r = GrooveReport {
            hits: records.len(),
            ..Default::default()
        };
        if records.is_empty() || sample_rate <= 0.0 {
            return r;
        }
        let ms_per_frame = 1000.0 / sample_rate;
        let delays: Vec<f64> = records
            .iter()
            .map(|h| h.delay_frames() as f64 * ms_per_frame)
            .collect();
        (r.microtiming_ms, r.microtiming_sd_ms) = mean_sd(&delays);

        // swing: odd steps against even ones, in pairs of two sixteenths
        let (mut on, mut off) = (vec![], vec![]);
        for (h, d) in records.iter().zip(&delays) {
            if h.step() % 2 == 1 {
                off.push(*d);
            } else {
                on.push(*d);
            }
        }
        if !on.is_empty() && !off.is_empty() {
            let pair_ms = 2.0 * 15_000.0 / if bpm > 0.0 { bpm } else { 120.0 };
            r.swing_ratio = Some(0.5 + (mean_sd(&off).0 - mean_sd(&on).0) / pair_ms);
        }

        // by kind and position
        #[derive(Clone, Default)]
        struct Cell {
            delays: Vec<f64>,
            velocities: alloc::collections::BTreeSet<i64>,
        }
        let mut cells: BTreeMap<String, Vec<Cell>> = BTreeMap::new();
        for (h, d) in records.iter().zip(&delays) {
            let row = cells
                .entry(h.kind.clone())
                .or_insert_with(|| vec![Cell::default(); 16]);
            row[h.step() as usize].delays.push(*d);
            row[h.step() as usize].velocities.insert(h.velocity);
        }
        let mut drum_distinct: Vec<f64> = vec![];
        let mut drum_delays: Vec<f64> = vec![];
        let mut within = 0.0;
        for (kind, row) in &cells {
            let mut v = GrooveVoice::default();
            let mut distinct = vec![];
            let mut all = vec![];
            for c in row.iter().filter(|c| !c.delays.is_empty()) {
                all.extend(c.delays.iter().copied());
                if c.delays.len() >= 2 {
                    distinct.push(c.velocities.len() as f64);
                }
            }
            v.hits = all.len();
            v.distinct_velocities_per_position = if distinct.is_empty() {
                0.0
            } else {
                distinct.iter().sum::<f64>() / distinct.len() as f64
            };
            (v.mean_delay_ms, v.sd_delay_ms) = mean_sd(&all);
            r.voices.insert(kind.clone(), v);
            if kind == "tonal" || kind == "v" {
                continue;
            }
            r.drum_hits += all.len();
            drum_distinct.extend(distinct);
            drum_delays.extend(all);
            for c in row.iter().filter(|c| c.delays.len() >= 2) {
                let m = mean_sd(&c.delays).0;
                within += c.delays.iter().map(|d| (d - m) * (d - m)).sum::<f64>();
            }
        }
        if !drum_distinct.is_empty() {
            r.distinct_velocities_per_position =
                drum_distinct.iter().sum::<f64>() / drum_distinct.len() as f64;
        }
        let m = mean_sd(&drum_delays).0;
        let total: f64 = drum_delays.iter().map(|d| (d - m) * (d - m)).sum();
        // below (1 µs)² per hit nothing moved
        if total > 1e-6 * drum_delays.len().max(1) as f64 {
            r.consistency = (1.0 - within / total).max(0.0);
        }
        r
    }
}
