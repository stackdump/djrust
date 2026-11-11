//! Port of `Musicality/BassMotif.swift` (`.bassMotif`, ROADMAP 6.4): bass bar 2 echoes
//! bar 1's rhythm.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec::Vec;

use crate::gen::tables::{BeatsTables, GenreDef};
use crate::musicality::dj::{pad_chords, BASS_CEILING, BASS_FLOOR};
use crate::musicality::util::*;
use crate::pflow::{MidiBinding, Project};
use crate::spec::TrackSpec;

/// The sixteenth of a bar from which a note bar 1 does not play counts as bar 2's pickup.
pub const BASS_PICKUP_FROM: i64 = 12;

/// `restateBassRhythm`.
pub fn restate_bass_rhythm(p: &mut Project, genre: &GenreDef, _spec: &TrackSpec) {
    let groove = BeatsTables::shared()
        .theme_table(&genre.name)
        .map_or(0, |t| t.groove);
    if groove == 5 || groove == 6 {
        return;
    }
    let key = key_pitch_classes(p);
    if key.is_empty() {
        return;
    }
    let scale: Vec<i64> = (0..=127).filter(|n| key.contains(&pc(*n))).collect();
    let (chords, pad_bars) = pad_chords(p);
    // the bass's room, as `varyBassFigure` reads it
    let mut ceiling = notes(p, "bass")
        .into_iter()
        .max()
        .unwrap_or(BASS_CEILING)
        .max(BASS_CEILING);
    if let Some(pad) = notes(p, "harmony").into_iter().min() {
        ceiling = ceiling.min(pad - 1);
    }
    let room = (BASS_FLOOR, BASS_FLOOR.max(ceiling));
    let in_room = |n: i64| n >= room.0 && n <= room.1;
    let fit = |n: i64| -> Option<i64> {
        if in_room(n) {
            Some(n)
        } else if in_room(n + 12) {
            Some(n + 12)
        } else if in_room(n - 12) {
            Some(n - 12)
        } else {
            None
        }
    };
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "bass" {
            continue;
        }
        let count = nb.transitions.len() as i64;
        let block = if count % 64 == 0 { 64 } else { 32 };
        if count % block != 0 {
            continue;
        }
        let steps = ring_notes(nb);
        let mut start = 0;
        while start < count {
            let this = start;
            start += block;
            let first: Vec<&StepNote> = steps
                .iter()
                .filter(|s| s.step >= this && s.step < this + 16)
                .collect();
            let second: Vec<&StepNote> = steps
                .iter()
                .filter(|s| s.step >= this + 16 && s.step < this + 32)
                .collect();
            if first.len() < 2 {
                continue;
            }
            let Some(opening) = second.first() else {
                continue;
            };
            if !first.iter().all(|s| key.contains(&pc(s.note))) {
                continue;
            }
            let rhythm1: Vec<i64> = first.iter().map(|s| s.step % 16).collect();
            let r1: BTreeSet<i64> = rhythm1.iter().copied().collect();
            let r2: BTreeSet<i64> = second.iter().map(|s| s.step % 16).collect();
            if r1 == r2 {
                continue;
            }
            // bar 2's root: its chord's root nearest where it began, in key and in the room
            let mut root2 = opening.note;
            if pad_bars > 0 {
                if let Some(&r) = chords
                    .get(&(((this + 16) / 16) % pad_bars))
                    .and_then(|c| c.first())
                {
                    if pc(root2) != r && key.contains(&r) {
                        let mut near: Option<i64> = None;
                        for d in -6..=6 {
                            let c = opening.note + d;
                            if pc(c) == r
                                && in_room(c)
                                && near.map_or(true, |b| {
                                    (c - opening.note).abs() < (b - opening.note).abs()
                                })
                            {
                                near = Some(c);
                            }
                        }
                        if let Some(n) = near {
                            root2 = n;
                        }
                    }
                }
            }
            let (Some(i1), Some(i2)) = (
                scale.iter().position(|&n| n == first[0].note),
                scale.iter().position(|&n| n == root2),
            ) else {
                continue;
            };
            let (i1, i2) = (i1 as i64, i2 as i64);
            // `n` moved `k` scale steps, then into the room
            let moved = |n: i64, k: i64| -> Option<i64> {
                let i = scale.iter().position(|&x| x == n)? as i64;
                let j = i + k;
                if j < 0 || j >= scale.len() as i64 {
                    return None;
                }
                fit(scale[j as usize])
            };
            let mut copy: Vec<(i64, MidiBinding)> = Vec::new();
            for s in &first {
                let Some(n) = moved(s.note, i2 - i1) else {
                    break;
                };
                let Some(mut b) = nb.bindings.get(&s.key).copied() else {
                    break;
                };
                b.note = n;
                copy.push((s.step + 16, b));
            }
            if copy.len() != first.len() {
                continue;
            }
            // bar 2's pickup: last-beat notes after bar 1's last, kept; bar 1 plays them too
            let last1 = *rhythm1.iter().max().unwrap();
            let pickups: Vec<&&StepNote> = second
                .iter()
                .filter(|s| s.step % 16 >= BASS_PICKUP_FROM && s.step % 16 > last1)
                .collect();
            let mut lead: Vec<(i64, MidiBinding)> = Vec::new();
            for s in &pickups {
                let Some(n) = moved(s.note, i1 - i2) else {
                    break;
                };
                let Some(mut b) = nb.bindings.get(&s.key).copied() else {
                    break;
                };
                if !nb.transitions.contains_key(&format!("t{}", s.step - 16)) {
                    break;
                }
                b.note = n;
                lead.push((s.step - 16, b));
            }
            if lead.len() != pickups.len() {
                continue;
            }
            let kept: BTreeSet<&str> = pickups.iter().map(|s| s.key.as_str()).collect();
            for s in &second {
                if !kept.contains(s.key.as_str()) {
                    nb.bindings.remove(&s.key);
                }
            }
            for c in &copy {
                nb.bindings.insert(format!("t{}", c.0), c.1);
            }
            for l in &lead {
                nb.bindings.insert(format!("t{}", l.0), l.1);
            }
        }
    }
}
