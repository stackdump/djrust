//! Helpers shared by the passes: what `DJMusicality.swift` keeps as `static func`s under
//! `// MARK: shared`, plus the ring-net accessors that every pass repeats.

use alloc::string::String;
use alloc::vec::Vec;

use crate::pflow::{NetBundle, Project};
use crate::wave::synth::is_drum_channel;

/// `Int(t.dropFirst())` for a binding key `t<i>`.
pub fn t_index(key: &str) -> Option<i64> {
    key.strip_prefix('t')?.parse::<i64>().ok()
}

/// A tonal note bound on a ring's step: the binding key, the step, the MIDI note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepNote {
    pub key: String,
    pub step: i64,
    pub note: i64,
}

/// `DJMusicality.part`: the part a net belongs to, its mixer group else its riff group.
pub fn part(nb: &NetBundle) -> &str {
    match nb.track.group.as_deref() {
        Some(g) if !g.is_empty() => g,
        _ => &nb.riff_group,
    }
}

/// `DJMusicality.notes(_:group:)`: every tonal note of a part.
pub fn notes(p: &Project, group: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for nb in p.nets.values() {
        if nb.role != "control" && part(nb) == group {
            for b in nb.bindings.values() {
                if !is_drum_channel(b.channel) {
                    out.push(b.note);
                }
            }
        }
    }
    out
}

/// `DJMusicality.median`: the upper middle of the sorted notes.
pub fn median(xs: &[i64]) -> Option<i64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort();
    Some(v[v.len() / 2])
}

/// `DJMusicality.transpose`: move a part's tonal notes by `shift` semitones (clamped 0…127).
pub fn transpose(p: &mut Project, group: &str, shift: i64) {
    if shift == 0 {
        return;
    }
    let ids: Vec<String> = p.nets.keys().to_vec();
    for id in ids {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || part(nb) != group {
            continue;
        }
        let keys: Vec<String> = nb.bindings.keys().to_vec();
        for t in keys {
            let b = nb.bindings.get_mut(&t).unwrap();
            if is_drum_channel(b.channel) {
                continue;
            }
            b.note = (b.note + shift).clamp(0, 127);
        }
    }
}

/// `DJMusicality.isBarRing`: a ring net of whole bars (one place per transition).
pub fn is_bar_ring(nb: &NetBundle) -> bool {
    nb.role != "control"
        && nb.transitions.len() % 16 == 0
        && nb.places.len() == nb.transitions.len()
}

/// `DJMusicality.ringNotes`: the tonal notes bound on a ring's steps, in step order.
pub fn ring_notes(nb: &NetBundle) -> Vec<StepNote> {
    let mut out = Vec::new();
    for (t, b) in nb.bindings.iter() {
        if t.starts_with('t') && !is_drum_channel(b.channel) {
            if let Some(i) = t_index(t) {
                out.push(StepNote {
                    key: t.into(),
                    step: i,
                    note: b.note,
                });
            }
        }
    }
    out.sort_by_key(|s| s.step);
    out
}

/// `DJMusicality.pc`: pitch class of any integer.
pub fn pc(n: i64) -> i64 {
    ((n % 12) + 12) % 12
}

/// `DJMusicality.keyPitchClasses`: the project's key as pitch classes.
pub fn key_pitch_classes(p: &Project) -> alloc::collections::BTreeSet<i64> {
    let root = p.root_note.unwrap_or(60);
    let name = if p.scale_name.is_empty() {
        "Major"
    } else {
        p.scale_name.as_str()
    };
    let mut full = String::from(name);
    full.push_str("Scale");
    crate::gen::core::scale_intervals(&full)
        .iter()
        .map(|i| (root + i) % 12)
        .collect()
}
