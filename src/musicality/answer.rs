//! Port of `Musicality/Answer.swift` (`.answer`, ROADMAP 6.4): 8-bar question and answer on
//! melody slot pairs. Runs after `.motif`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::tables::GenreDef;
use crate::musicality::dj::pad_chords;
use crate::musicality::slot_windows::{slot_windows, SlotWindow};
use crate::musicality::util::*;
use crate::pflow::Project;
use crate::spec::TrackSpec;

struct Last {
    step: i64,
    net: String,
    key: String,
    note: i64,
}

/// `answerPhrases`.
pub fn answer_phrases(p: &mut Project, _genre: &GenreDef, _spec: &TrackSpec) {
    let (chords, pad_bars) = pad_chords(p);
    let key = key_pitch_classes(p);
    if key.is_empty() {
        return;
    }
    let tonic = pc(p.root_note.unwrap_or(60));
    let windows = slot_windows(p, "melody");

    // Per ring net: its notes by step and its range.
    let mut notes: BTreeMap<String, BTreeMap<i64, (String, i64)>> = BTreeMap::new();
    let mut range: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for net in windows.keys() {
        let Some(nb) = p.nets.get(net) else { continue };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let steps = ring_notes(nb);
        let (Some(lo), Some(hi)) = (
            steps.iter().map(|s| s.note).min(),
            steps.iter().map(|s| s.note).max(),
        ) else {
            continue;
        };
        let mut m = BTreeMap::new();
        for s in &steps {
            m.entry(s.step).or_insert((s.key.clone(), s.note));
        }
        notes.insert(net.clone(), m);
        range.insert(net.clone(), (lo - 2, hi + 2));
    }
    if notes.is_empty() {
        return;
    }
    let timeline: Vec<(&String, &SlotWindow)> = windows
        .iter()
        .flat_map(|(net, ws)| ws.iter().map(move |w| (net, w)))
        .filter(|(net, w)| {
            notes.contains_key(*net) && w.ring_start.is_some() && w.ring_length.is_some()
        })
        .collect();

    // The last note struck in song steps `a ..< b`.
    let last_note = |a: i64, b: i64| -> Option<Last> {
        let mut best: Option<Last> = None;
        for (net, w) in &timeline {
            if !(w.start < b && w.end > a) {
                continue;
            }
            let ring = &notes[*net];
            let mut s = b.min(w.end) - 1;
            while s >= a.max(w.start) {
                if let Some(bst) = &best {
                    if bst.step >= s {
                        break;
                    }
                }
                if let Some(n) =
                    ring.get(&((w.ring_start.unwrap() + s - w.start) % w.ring_length.unwrap()))
                {
                    best = Some(Last {
                        step: s,
                        net: (*net).clone(),
                        key: n.0.clone(),
                        note: n.1,
                    });
                    break;
                }
                s -= 1;
            }
        }
        best
    };

    // The nearest note of pitch class `want` to `n` (a tritone at most), in `r` and the key.
    let nearest = |n: i64, want: i64, r: (i64, i64)| -> Option<i64> {
        if !key.contains(&want) {
            return None;
        }
        let mut best: Option<i64> = None;
        for d in -6..=6 {
            let c = n + d;
            if pc(c) == want
                && c >= r.0
                && c <= r.1
                && best.map_or(true, |b| (c - n).abs() < (b - n).abs())
            {
                best = Some(c);
            }
        }
        best
    };

    // What each (net, ring binding) is asked to become, in the order first asked.
    let mut asks: BTreeMap<String, BTreeMap<String, Vec<i64>>> = BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    let mut ask = |net: &str, key: &str, note: i64| {
        if asks.get(net).and_then(|m| m.get(key)).is_none() {
            order.push((net.into(), key.into()));
        }
        asks.entry(net.into())
            .or_default()
            .entry(key.into())
            .or_default()
            .push(note);
    };

    let mut section_start = 0i64;
    for s in &p.structure {
        let mut pair = section_start;
        while pair <= section_start + s.steps - 128 {
            let this = pair;
            pair += 128;
            let (Some(q), Some(a)) = (last_note(this, this + 64), last_note(this + 64, this + 128))
            else {
                continue;
            };
            if !(q.step - this >= 32 && a.step - this - 64 >= 32) {
                continue;
            }
            let q_chord: Vec<i64> = if pad_bars > 0 {
                chords
                    .get(&((q.step / 16) % pad_bars))
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let a_chord: Vec<i64> = if pad_bars > 0 {
                chords
                    .get(&((a.step / 16) % pad_bars))
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // question: the chord's fifth, else its third (the key's, without a pad)
            let root = q_chord.first().copied().unwrap_or(tonic);
            let open: Vec<i64> = [root + 7, root + 4, root + 3]
                .iter()
                .map(|x| pc(*x))
                .filter(|x| *x != tonic && (q_chord.is_empty() || q_chord.contains(x)))
                .collect();
            if open.first().copied() == Some(pc(q.note)) {
                ask(&q.net, &q.key, q.note);
            } else if let Some(n) = open
                .iter()
                .filter_map(|o| nearest(q.note, *o, range[&q.net]))
                .next()
            {
                ask(&q.net, &q.key, n);
            }

            // answer: the tonic, else the chord's root
            let n = nearest(a.note, tonic, range[&a.net]).or_else(|| {
                a_chord
                    .first()
                    .and_then(|c| nearest(a.note, *c, range[&a.net]))
            });
            if let Some(n) = n {
                ask(&a.net, &a.key, n);
            }
        }
        section_start += s.steps;
    }

    for (net, k) in &order {
        let wanted = &asks[net][k];
        let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
        for n in wanted {
            *counts.entry(*n).or_insert(0) += 1;
        }
        let top = *counts.values().max().unwrap();
        if let Some(note) = wanted.iter().find(|n| counts[*n] == top) {
            if let Some(b) = p.nets.get_mut(net).and_then(|nb| nb.bindings.get_mut(k)) {
                b.note = *note;
            }
        }
    }
}
