//! Port of `StingerLaunch.swift`: firing a curated stinger on a net deck. The decision (what
//! each Hit does, and what it falls back to), the loop clip's id and stop frame, and the
//! bookkeeping of the stops waiting on their voices. The engine calls (`wave.fireStinger`,
//! `fireMacro`, `startLoop`) are the host's.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::pflow::Project;
use crate::sound::stinger::{AssignmentKind, StingerAssignment, SLOT_COUNT};
use crate::voices::stable_hash;

/// `Deck.stingers`: what Hit 1–4 fire on this deck's track (four fallbacks when nothing is
/// curated; empty for a sample deck: pass `None`).
pub fn deck_stingers(project: Option<&Project>) -> Vec<StingerAssignment> {
    let Some(p) = project else { return Vec::new() };
    (0..SLOT_COUNT)
        .map(|n| {
            p.stingers
                .iter()
                .find(|s| s.slot == n)
                .cloned()
                .unwrap_or_else(|| StingerAssignment::fallback(n))
        })
        .collect()
}

/// What `Deck.stinger(_:bars:)` tries, in order; the first that works ends it.
#[derive(Clone, Debug, PartialEq)]
pub enum StingerStep {
    /// `wave.fireStinger(netID, ticks:)`: always works.
    FireNet { net_id: String, ticks: i64 },
    /// `fireMacro(id:, duration:)`: false when the macro can't fire.
    FireMacro { id: String, duration: Option<i64> },
    /// `onStingerLoop(a)`: false when the loop can't play.
    LaunchLoop,
}

/// `Deck.stinger`: stinger `n` (0…3, Hit 1…4) as curated: a one-shot unmutes its stinger net
/// (`hit1`…) for its bars, a macro fires, a loop launches on the loop row. A Hit that cannot
/// fire what it was given (a loop whose audio is gone) plays its stinger net. Empty for an `n`
/// out of range.
pub fn stinger_steps(a: &StingerAssignment, n: i64, bars: i64) -> Vec<StingerStep> {
    if !(0..SLOT_COUNT as i64).contains(&n) {
        return Vec::new();
    }
    let fallback = StingerStep::FireNet {
        net_id: a.net_id(),
        ticks: bars * 16,
    };
    match a.kind {
        AssignmentKind::OneShot => alloc::vec![StingerStep::FireNet {
            net_id: a.net_id(),
            ticks: a.bars.unwrap_or(bars) * 16,
        }],
        AssignmentKind::Macro => match &a.macro_id {
            Some(id) => alloc::vec![
                StingerStep::FireMacro {
                    id: id.clone(),
                    duration: a.bars
                },
                fallback
            ],
            None => alloc::vec![fallback],
        },
        AssignmentKind::Loop => alloc::vec![StingerStep::LaunchLoop, fallback],
        AssignmentKind::Fallback => alloc::vec![fallback],
    }
}

/// The id of a stinger loop's clip when it comes from a library file: `stinger-<deck>-<hash>`
/// (`fireStingerLoop`).
pub fn stinger_clip_id(deck: &str, path: &str) -> String {
    format!("stinger-{deck}-{:x}", stable_hash(path))
}

/// The tempo a decoded stinger loop has: its beats over its length (`bpm`).
pub fn stinger_clip_bpm(beats: i64, sample_rate: f64, length: usize) -> f64 {
    beats as f64 * 60.0 * sample_rate / length as f64
}

/// `launchStingerClip`: the frame a launched stinger loop goes out at, one pass after it
/// starts at `start_frame`.
pub fn stinger_stop_frame(start_frame: i64, beats: i64, target_bpm: f64, sample_rate: f64) -> i64 {
    let pass = beats as f64 * 60.0 / target_bpm.max(1.0) * sample_rate;
    start_frame + libm::round(pass) as i64
}

/// A loop voice as `serviceStingerStops` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopVoice {
    /// The bank has the voice in (`bank.isActive(v)`).
    pub active: bool,
    /// A stop is already scheduled on it.
    pub stop_scheduled: bool,
}

/// `serviceStingerStops`: schedule each launched stinger loop's stop once its voice is in (a
/// stop sent before then would cancel the start), and forget the ones that are out. Returns
/// the `(id, frame)` stops to schedule; `stops` loses those and the ones whose voice is gone.
pub fn service_stinger_stops(
    stops: &mut BTreeMap<String, i64>,
    voice_of: &dyn Fn(&str) -> Option<StopVoice>,
) -> Vec<(String, i64)> {
    let mut schedule = Vec::new();
    let ids: Vec<String> = stops.keys().cloned().collect();
    for id in ids {
        let frame = stops[&id];
        let Some(v) = voice_of(&id) else {
            stops.remove(&id);
            continue;
        };
        if !v.active || v.stop_scheduled {
            continue;
        }
        schedule.push((id.clone(), frame));
        stops.remove(&id);
    }
    schedule
}
