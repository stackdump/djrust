//! Port of `Musicality/MelodyShape.swift` (`.shape`, p64-shape): melodic contour by corpus,
//! leap recovery, and a peak near two-thirds of each phrase.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::gen::core::scale_intervals;
use crate::gen::tables::GenreDef;
use crate::musicality::dj::{neighbour, pad_chords};
use crate::musicality::melody_bigrams::MelodyBigrams;
use crate::musicality::util::*;
use crate::pflow::Project;
use crate::spec::TrackSpec;

/// A leap: a fourth or more.
pub const SHAPE_LEAP: i64 = 5;
/// Sixteenths within which two melody notes are neighbours.
pub const SHAPE_WINDOW: i64 = 8;
/// Where in a 4-bar phrase (sixteenths of 64) a peak that came early is moved.
pub const SHAPE_PEAK_WINDOW: (i64, i64) = (36, 51);

/// `shapeMelody`.
pub fn shape_melody(p: &mut Project, _genre: &GenreDef, _spec: &TrackSpec) {
    let key = key_pitch_classes(p);
    if key.is_empty() {
        return;
    }
    let (chords, pad_bars) = pad_chords(p);
    let root = pc(p.root_note.unwrap_or(60));
    let name = if p.scale_name.is_empty() {
        "Major"
    } else {
        p.scale_name.as_str()
    };
    let mut intervals: Vec<i64> = scale_intervals(&alloc::format!("{name}Scale"))
        .iter()
        .map(|i| pc(*i))
        .collect();
    intervals.sort();
    let bigrams: Option<MelodyBigrams> = if intervals.len() == MelodyBigrams::DEGREES {
        Some(MelodyBigrams::default_table())
    } else {
        None
    };
    let degree = |n: i64| -> Option<usize> { intervals.iter().position(|&i| i == pc(n - root)) };
    let chord = |step: i64| -> Option<&Vec<i64>> {
        if pad_bars > 0 {
            chords.get(&((step / 16) % pad_bars))
        } else {
            None
        }
    };
    let rubs = |n: i64, step: i64| -> bool {
        match chord(step) {
            None => false,
            Some(c) => c.iter().any(|x| pc(n - x) == 1 || pc(n - x) == 11),
        }
    };
    let chord_tone =
        |n: i64, step: i64| -> bool { chord(step).map_or(false, |c| c.contains(&pc(n))) };
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let steps = ring_notes(nb);
        let m = steps.len();
        if m <= 2 {
            continue;
        }
        let count = nb.transitions.len() as i64;
        let lo = steps.iter().map(|s| s.note).min().unwrap() - 2;
        let hi = steps.iter().map(|s| s.note).max().unwrap() + 2;
        let mut played: Vec<i64> = steps.iter().map(|s| s.note).collect();
        // sixteenths from note k to the next, round the cycle
        let gap = |k: usize| -> i64 {
            if k + 1 < m {
                steps[k + 1].step - steps[k].step
            } else {
                steps[0].step + count - steps[k].step
            }
        };
        let free = |n: i64, k: usize| -> bool { key.contains(&pc(n)) && !rubs(n, steps[k].step) };

        // 1. repeats and leaps onto non-chord tones become steps, by the corpus's bigrams
        for k in 1..m {
            if !(steps[k].step % 4 != 0 && gap(k - 1) <= SHAPE_WINDOW) {
                continue;
            }
            let prev = played[k - 1];
            let cur = played[k];
            let is_step = (1..=2).contains(&(cur - prev).abs());
            // a step already or a chord tone reached by a leap stays
            if !(cur == prev || (!is_step && !chord_tone(cur, steps[k].step))) {
                continue;
            }
            let next: Option<i64> = if k + 1 < m && gap(k) <= SHAPE_WINDOW {
                Some(played[k + 1])
            } else {
                None
            };
            let likelihood = |n: i64| -> f64 {
                if n == prev {
                    return 0.0;
                }
                let corpus = match (&bigrams, degree(prev), degree(n)) {
                    (Some(b), Some(from), Some(to)) => b.moves[from][to],
                    _ => next.map_or(0.5, |nx| {
                        if (nx - prev).signum() == (n - prev).signum() {
                            0.6
                        } else {
                            0.4
                        }
                    }),
                };
                // not onto the next note (a new repeat), and not a leap away from it
                let to_next = next.map_or(0, |nx| (nx - n).abs());
                corpus
                    * (if Some(n) == next { 0.25 } else { 1.0 })
                    * (if to_next >= SHAPE_LEAP { 0.5 } else { 1.0 })
            };
            let mut best: Option<(f64, i64)> = None;
            for d in [-1i64, 1, -2, 2] {
                let n = prev + d;
                if !(n >= lo && n <= hi && free(n, k)) {
                    continue;
                }
                let l = likelihood(n);
                if l > 0.0 && best.map_or(true, |b| l > b.0) {
                    best = Some((l, n));
                }
            }
            let Some(best) = best else { continue };
            if best.1 == cur {
                continue;
            }
            played[k] = best.1;
        }

        // 2. a peak near two-thirds of each 4-bar phrase
        let mut peaks: BTreeSet<usize> = BTreeSet::new();
        if count % 64 == 0 {
            let mut start = 0;
            while start < count {
                let this = start;
                start += 64;
                let block: Vec<usize> = (0..m)
                    .filter(|&i| steps[i].step >= this && steps[i].step < this + 64)
                    .collect();
                if block.len() < 2 {
                    continue;
                }
                let top = block.iter().map(|&i| played[i]).max().unwrap();
                let first = *block.iter().find(|&&i| played[i] == top).unwrap();
                if steps[first].step - this >= 35 {
                    continue; // before 0.55
                }
                let window: Vec<usize> = block
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let off = steps[i].step - this;
                        off >= SHAPE_PEAK_WINDOW.0
                            && off <= SHAPE_PEAK_WINDOW.1
                            && steps[i].step % 4 != 0
                    })
                    .collect();
                // `max(by: <)` keeps the first of equal maxima
                let mut kbest: Option<usize> = None;
                for &i in &window {
                    if kbest.map_or(true, |b| played[b] < played[i]) {
                        kbest = Some(i);
                    }
                }
                let Some(k) = kbest else { continue };
                let ceiling = hi + 1; // the ring's top + a minor third
                let raised = (top + 1..=top + 5)
                    .find(|&c| c <= ceiling && free(c, k) && chord_tone(c, steps[k].step))
                    .or_else(|| (top + 1..=top + 3).find(|&c| c <= ceiling && free(c, k)));
                let Some(raised) = raised else { continue };
                played[k] = raised;
                peaks.insert(k);
            }
        }

        // 3. a leap recovered by a step back
        for a in 0..m {
            let b = (a + 1) % m;
            let c = (a + 2) % m;
            if !(gap(a) <= SHAPE_WINDOW && gap(b) <= SHAPE_WINDOW) {
                continue;
            }
            let leap = played[b] - played[a];
            if leap.abs() < SHAPE_LEAP {
                continue;
            }
            let back = played[c] - played[b];
            if (1..=2).contains(&back.abs()) && back.signum() == -leap.signum() {
                continue;
            }
            if steps[c].step % 4 != 0 && !peaks.contains(&c) {
                if let Some(n) = neighbour(played[b], leap < 0, &key) {
                    if (n - played[b]).abs() <= 2 && n >= lo && n <= hi + 1 && free(n, c) {
                        played[c] = n;
                        continue;
                    }
                }
            }
            // the note after it is on the beat (or cannot step back): when the leap's own
            // note is off the beat, close the leap instead
            if !(steps[b].step % 4 != 0 && !peaks.contains(&b)) {
                continue;
            }
            let from = played[a];
            let to = played[c];
            let mut best: Option<i64> = None;
            for o in from - 4..=from + 4 {
                if o != from && o >= lo && o <= hi && free(o, b) && (to - o).abs() < SHAPE_LEAP {
                    if best.map_or(true, |bn| (o - played[b]).abs() < (bn - played[b]).abs()) {
                        best = Some(o);
                    }
                }
            }
            let Some(n) = best else { continue };
            played[b] = n;
        }

        for k in 0..m {
            if played[k] != steps[k].note {
                if let Some(b) = nb.bindings.get_mut(&steps[k].key) {
                    b.note = played[k];
                }
            }
        }
    }
}
