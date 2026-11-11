//! Port of `Musicality/HarmonicCadence.swift` (`DJMusicality.Fixes.harmonicCadence`): the
//! pad cycle's last bar resolves into its first.

use alloc::collections::BTreeSet;
use alloc::vec;

use crate::gen::core::scale_intervals;
use crate::gen::tables::{BeatsTables, GenreDef};
use crate::gen::techniques::gcd;
use crate::musicality::dj::pad_chords;
use crate::musicality::track_variety::{reharmonize, seven_note_key};
use crate::musicality::util::pc;
use crate::pflow::Project;
use crate::spec::TrackSpec;

/// The semitones a bar's root rises into the next bar's that count as a cadence: a fourth
/// (V → I) or a whole step (♭VII → i, IV → V).
pub const CADENCE_RISES: [i64; 2] = [5, 2];

/// `cadencesFromFlatSeven`: dance-family genres in a minor mode resolve ♭VII → i.
pub fn cadences_from_flat_seven(genre: &GenreDef) -> bool {
    BeatsTables::shared().family_of(&genre.name) == 0
        && ["MinorScale", "DorianScale", "PhrygianScale"].contains(&genre.scale_name.as_str())
}

/// `resolvePadCycle`.
pub fn resolve_pad_cycle(p: &mut Project, genre: &GenreDef, _spec: &TrackSpec) {
    let _ = scale_intervals;
    if p.cohesion.as_deref() != Some("v2") {
        return;
    }
    let Some(key) = seven_note_key(p) else {
        return;
    };
    let (chords, pad_bars) = pad_chords(p);
    if pad_bars < 2 {
        return;
    }
    // each bar's root degree as it is
    let mut roots = vec![0i64; pad_bars as usize];
    for bar in 0..pad_bars {
        if let Some(r) = chords.get(&bar).and_then(|c| c.first()) {
            if let Some(d) = key.1.iter().position(|x| x == r) {
                roots[bar as usize] = d as i64;
            }
        }
    }
    // the pad bars that end a 4-bar line of the song (the ring loops from bar 0)
    let span = pad_bars * 4 / gcd(pad_bars, 4);
    let ends: BTreeSet<i64> = (3..span).step_by(4).map(|x| x % pad_bars).collect();
    let flat_seven_first = cadences_from_flat_seven(genre);
    let mut changed = false;
    for &bar in &ends {
        let next = (bar + 1) % pad_bars;
        // a bar that both ends a line and starts one (odd cycles) is left as it is
        if ends.contains(&next) {
            continue;
        }
        let (Some(&last), Some(&first)) = (
            chords.get(&bar).and_then(|c| c.first()),
            chords.get(&next).and_then(|c| c.first()),
        ) else {
            continue;
        };
        if !key.1.contains(&last) {
            continue;
        }
        let Some(d0) = key.1.iter().position(|x| *x == first) else {
            continue;
        };
        let d0 = d0 as i64;
        if CADENCE_RISES.contains(&pc(first - last)) {
            continue;
        }
        // (degree, the semitones it must rise into the next bar's root)
        let mut candidates = [((d0 + 4) % 7, 5i64), ((d0 + 6) % 7, 2i64)];
        if flat_seven_first {
            candidates.reverse();
        }
        let Some(target) = candidates
            .iter()
            .find(|c| pc(first - key.1[c.0 as usize]) == c.1)
        else {
            continue;
        };
        roots[bar as usize] = target.0;
        changed = true;
    }
    if changed {
        reharmonize(p, &roots);
    }
}
