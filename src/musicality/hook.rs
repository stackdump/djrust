//! Port of `Musicality/Hook.swift` (`.hook`, p64-hook): one seeded chord-tone hook in every
//! melodic section.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::tables::GenreDef;
use crate::gen::techniques::{build_hook, hook_mode, HookCell};
use crate::gen::theme::motif_mode;
use crate::musicality::dj::pad_chords;
use crate::musicality::slot_windows::slot_windows;
use crate::musicality::util::*;
use crate::pflow::Project;
use crate::spec::TrackSpec;

/// `placeHook`. `triads`: the pad's chords before `.color` (None: the pad as it is now).
pub fn place_hook(
    p: &mut Project,
    genre: &GenreDef,
    spec: &TrackSpec,
    triads: Option<&(BTreeMap<i64, Vec<i64>>, i64)>,
) {
    let ov = spec.overrides();
    if ov.get("hook").and_then(|v| v.as_bool()) == Some(true) {
        return;
    }
    let (chords, pad_bars) = match triads {
        Some(t) => t.clone(),
        None => pad_chords(p),
    };
    let key = key_pitch_classes(p);
    if pad_bars <= 0 || key.is_empty() {
        return;
    }
    let scale: Vec<i64> = (0..=127).filter(|n| key.contains(&pc(*n))).collect();
    let tonic = pc(p.root_note.unwrap_or(60));
    let density = ov
        .get("melodyDensity")
        .and_then(|v| v.as_f64())
        .unwrap_or(genre.melody_density);
    let hook = build_hook(&spec.genre, spec.seed, 1, density);

    let mut windows: Vec<(String, crate::musicality::slot_windows::SlotWindow)> = Vec::new();
    for (net, ws) in slot_windows(p, "melody") {
        for w in ws {
            windows.push((net.clone(), w));
        }
    }
    windows.sort_by(|a, b| (a.1.start, &a.0).cmp(&(b.1.start, &b.0)));
    let mut written: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for (id, w) in &windows {
        let Some(nb) = p.nets.get_mut(id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let Some(ring_start) = w.ring_start else {
            continue;
        };
        if ring_start % 16 != 0 || w.end - w.start < 16 {
            continue;
        }
        let n = nb.transitions.len() as i64;
        let steps = ring_notes(nb);
        let (Some(lo), Some(first)) = (steps.iter().map(|s| s.note).min(), steps.first()) else {
            continue;
        };
        let Some(template) = nb.bindings.get(&first.key).copied() else {
            continue;
        };
        let mut base = lo - 2;
        while pc(base) != tonic {
            base += 1;
        }

        let mode = hook_mode(&w.section_name);
        let reshapes = mode == motif_mode::FRAGMENT || mode == motif_mode::AUGMENTED;
        let mut bars: Vec<(i64, i64)> =
            alloc::vec![(0, if reshapes { motif_mode::PLAY } else { mode })];
        if reshapes && w.end - w.start >= 48 {
            bars.push((32, mode));
        }
        for (offset, m) in bars {
            let start = (ring_start + offset) % n;
            if written.get(id).map_or(false, |s| s.contains(&(start / 16))) {
                continue;
            }
            let chord = chords
                .get(&(((w.start + offset) / 16) % pad_bars))
                .cloned()
                .unwrap_or_default();
            let notes = hook_bar(&hook, m, base, &chord, &key, &scale);
            if notes.len() < 2 {
                continue;
            }
            // the bar's own velocity and length where it plays, else the ring's
            let in_bar: Vec<&StepNote> = steps
                .iter()
                .filter(|s| s.step >= start && s.step < start + 16)
                .collect();
            let like = in_bar
                .first()
                .and_then(|s| nb.bindings.get(&s.key).copied())
                .unwrap_or(template);
            for s in &in_bar {
                nb.bindings.remove(&s.key);
            }
            for (step, note) in &notes {
                let mut b = like;
                b.note = *note;
                nb.bindings.insert(format!("t{}", start + step), b);
            }
            written.entry(id.clone()).or_default().insert(start / 16);
        }
    }
}

/// `hookBar`: one bar of `hook` as `mode` plays it: (sixteenth, MIDI note).
pub fn hook_bar(
    hook: &HookCell,
    mode: i64,
    base: i64,
    chord: &[i64],
    key: &BTreeSet<i64>,
    scale: &[i64],
) -> Vec<(i64, i64)> {
    let Some(root) = scale.iter().position(|&n| n == base) else {
        return Vec::new();
    };
    let root = root as i64;
    let mut cell: Vec<(i64, i64)> = Vec::new();
    for h in 0..hook.length().min(16) {
        if !hook.mask[h] {
            continue;
        }
        let mut t = hook.target[h];
        if mode == motif_mode::TRANSPOSED {
            t += 2;
        }
        if mode == motif_mode::INVERTED {
            t = 9 - t;
        }
        let h = h as i64;
        match mode {
            m if m == motif_mode::FRAGMENT => {
                if h < 8 {
                    cell.push((h, t));
                }
            }
            m if m == motif_mode::AUGMENTED => {
                if h < 8 {
                    cell.push((2 * h, t));
                }
            }
            _ => cell.push((h, t)),
        }
    }
    // the chord's tones that are in the key
    let in_key: Vec<i64> = chord.iter().copied().filter(|c| key.contains(c)).collect();
    let tones: Vec<i64> = if in_key.is_empty() {
        key.iter().copied().collect()
    } else {
        in_key
    };
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (k, c) in cell.iter().enumerate() {
        let idx = (root + c.1).max(0).min(scale.len() as i64 - 1);
        let raw = scale[idx as usize];
        let options: Vec<i64> = (-6..=6)
            .map(|d| raw + d)
            .filter(|n| (0..=127).contains(n) && tones.contains(&pc(*n)))
            .collect();
        let prev = out.last().map(|o| o.1);
        let sign = if k > 0 {
            (c.1 - cell[k - 1].1).signum()
        } else {
            0
        };
        let keep: Vec<i64> = options
            .iter()
            .copied()
            .filter(|o| prev.map_or(true, |p| (o - p).signum() == sign))
            .collect();
        let pool = if keep.is_empty() { &options } else { &keep };
        let mut note: Option<i64> = None;
        for &o in pool {
            if note.map_or(true, |b| ((o - raw).abs(), o) < ((b - raw).abs(), b)) {
                note = Some(o);
            }
        }
        out.push((c.0, note.unwrap_or(raw)));
    }
    out
}
