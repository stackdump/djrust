//! Port of `Musicality/DJMusicality.swift`: dj's pass over a composed track before a deck
//! plays it. `compose()` stays beats' byte for byte; these passes run after it, on the
//! played project. They rewrite MIDI bindings (note, velocity, length) and initial
//! markings, never places, transitions or arcs. A pure function of (project, spec).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gen::core::clamp_velocity;
use crate::gen::tables::{BeatsTables, GenreDef};
use crate::gen::theme::hash_str;
use crate::gen::variety::tension_for_variant;
use crate::json::js_to_int32;
use crate::musicality::energy_curve::RAMP_SECTIONS;
use crate::musicality::note_log;
use crate::musicality::slot_windows::slot_windows;
use crate::musicality::util::*;
use crate::pflow::{MidiBinding, NetBundle, OrderedMap, Project};
use crate::rng::Mulberry32;
use crate::spec::TrackSpec;
use crate::wave::synth::{drum_kind, is_drum_channel};

fn round_i(d: f64) -> i64 {
    js_round_half_away(d)
}

/// Swift `.rounded()` (half away from zero).
pub(crate) fn js_round_half_away(d: f64) -> i64 {
    libm::round(d) as i64
}

// MARK: key

/// The roles `expandVariants` builds melodic slot variants for.
pub const MELODIC_ROLES: [&str; 3] = ["bass", "melody", "arp"];

fn nets_keys(p: &Project) -> Vec<String> {
    p.nets.keys().to_vec()
}

/// `keepVariantsInKey`: with the genre's tension curve, a B / C slot of a melodic role was
/// composed on `rootNote + registerShift`; every note of it is the home-key note moved by
/// that shift. Move it back.
pub fn keep_variants_in_key(p: &mut Project, genre: &GenreDef, spec: &TrackSpec) {
    let tension_curve = spec
        .extra
        .get("tension-curve")
        .and_then(|v| v.as_bool())
        .unwrap_or(genre.tension_curve);
    if !tension_curve {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || !MELODIC_ROLES.contains(&nb.riff_group.as_str()) {
            continue;
        }
        let shift = tension_for_variant(&nb.riff_variant).register_shift;
        if shift == 0 {
            continue;
        }
        for t in nb.bindings.keys().to_vec() {
            let b = nb.bindings.get_mut(&t).unwrap();
            if is_drum_channel(b.channel) {
                continue;
            }
            b.note = (b.note - shift).clamp(0, 127);
        }
    }
}

// MARK: timing

/// `fixStructureTiming`: a delay of `gap - 1` on every stage of a structure accumulator
/// fires each change on the last sixteenth before its step.
pub fn fix_structure_timing(p: &mut Project) {
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role != "control"
            || !nb.places.contains_key("p_delay_0")
            || !nb.places.contains_key("p_gate_0")
        {
            continue;
        }
        let mut i = 0;
        loop {
            let name = format!("p_delay_{i}");
            let Some(place) = nb.places.get_mut(&name) else {
                break;
            };
            if let Some(&gap) = place.initial.first() {
                if gap >= 1.0 {
                    place.initial = vec![gap - 1.0];
                }
            }
            i += 1;
        }
    }
}

// MARK: register

/// The bass's median note goes no higher than this (A2).
pub const BASS_CEILING: i64 = 45;
/// …and no lower than this (E1).
pub const BASS_FLOOR: i64 = 28;
/// A pad's median goes no lower than this (F3).
pub const PAD_FLOOR: i64 = 53;

/// `separateRegisters`: move whole parts by octaves so the bass sits under the pad and the
/// melody and arp above it.
pub fn separate_registers(p: &mut Project) {
    let bass = notes(p, "bass");
    let mut bass_shift = 0;
    if let Some(m) = median(&bass) {
        while m + bass_shift > BASS_CEILING {
            bass_shift -= 12;
        }
        while m + bass_shift < BASS_FLOOR {
            bass_shift += 12;
        }
    }
    transpose(p, "bass", bass_shift);
    let bass_top = bass.iter().copied().max().unwrap_or(0) + bass_shift;

    let pad = notes(p, "harmony");
    let mut pad_shift = 0;
    if let (Some(&low), Some(m)) = (pad.iter().min(), median(&pad)) {
        while low + pad_shift <= bass_top || m + pad_shift < PAD_FLOOR {
            pad_shift += 12;
        }
    }
    transpose(p, "harmony", pad_shift);
    let pad_median = median(&pad).map(|m| m + pad_shift);

    for lead in ["melody", "arp"] {
        let ns = notes(p, lead);
        let mut shift = 0;
        if let (Some(m), Some(floor)) = (median(&ns), pad_median) {
            while m + shift <= floor && ns.iter().copied().max().unwrap_or(0) + shift + 12 <= 96 {
                shift += 12;
            }
        }
        transpose(p, lead, shift);
    }
}

// MARK: bass line

#[derive(Clone)]
struct Slot {
    key: String,
    step: i64,
    note: i64,
}

/// `leadBass`: in each bass ring, move every bar that plays by octaves to the placement
/// with the least motion from bar to bar around the cycle.
pub fn lead_bass(p: &mut Project) {
    let Some(mut top) = notes(p, "bass").into_iter().max() else {
        return;
    };
    top = top.max(BASS_CEILING);
    if let Some(pad) = notes(p, "harmony").into_iter().min() {
        top = top.min(pad - 1);
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || part(nb) != "bass"
            || nb.transitions.len() % 16 != 0
            || nb.places.len() != nb.transitions.len()
        {
            continue;
        }
        let mut bars: Vec<Vec<Slot>> = vec![Vec::new(); nb.transitions.len() / 16];
        for (t, b) in nb.bindings.iter() {
            if t.starts_with('t') && !is_drum_channel(b.channel) {
                if let Some(i) = t_index(t) {
                    bars[(i / 16) as usize].push(Slot {
                        key: t.into(),
                        step: i,
                        note: b.note,
                    });
                }
            }
        }
        let played: Vec<usize> = (0..bars.len()).filter(|&i| !bars[i].is_empty()).collect();
        if played.len() <= 1 {
            continue;
        }
        for &b in &played {
            bars[b].sort_by_key(|s| s.step);
        }
        let options: Vec<Vec<i64>> = played
            .iter()
            .map(|&b| {
                let ns: Vec<i64> = bars[b].iter().map(|s| s.note).collect();
                let fit: Vec<i64> = [0i64, -12, 12, -24, 24]
                    .into_iter()
                    .filter(|s| ns.iter().all(|n| n + s >= BASS_FLOOR && n + s <= top))
                    .collect();
                if fit.is_empty() {
                    vec![0]
                } else {
                    fit
                }
            })
            .collect();
        // the step from bar i placed at u to the next played bar placed at v; an octave
        // moved costs a little, so ties keep the composed placement
        let cost = |i: usize, u: i64, j: usize, v: i64| -> i64 {
            10 * (bars[played[i]].last().unwrap().note + u - (bars[played[j]][0].note + v)).abs()
                + v.abs() / 12
        };
        let mut best: Option<(i64, Vec<usize>)> = None;
        for first in 0..options[0].len() {
            let unreachable = i64::MAX / 4;
            let mut acc = vec![unreachable; options[0].len()];
            acc[first] = options[0][first].abs() / 12;
            let mut back: Vec<Vec<usize>> = vec![(0..acc.len()).collect()];
            for k in 1..played.len() {
                let mut next = vec![unreachable; options[k].len()];
                let mut from = vec![0usize; options[k].len()];
                for (j, &v) in options[k].iter().enumerate() {
                    for (i, &u) in options[k - 1].iter().enumerate() {
                        if acc[i] >= unreachable {
                            continue;
                        }
                        let x = acc[i] + cost(k - 1, u, k, v);
                        if x < next[j] {
                            next[j] = x;
                            from[j] = i;
                        }
                    }
                }
                acc = next;
                back.push(from);
            }
            let last = played.len() - 1;
            for (j, &v) in options[last].iter().enumerate() {
                if acc[j] >= unreachable {
                    continue;
                }
                let total =
                    acc[j] + cost(last, v, 0, options[0][first]) - options[0][first].abs() / 12;
                if best.as_ref().map_or(true, |(c, _)| total < *c) {
                    let mut path = vec![j];
                    for k in (1..=last).rev() {
                        let prev = *path.last().unwrap();
                        path.push(back[k][prev]);
                    }
                    path.reverse();
                    best = Some((total, path));
                }
            }
        }
        let Some((_, path)) = best else {
            continue;
        };
        for (k, &b) in played.iter().enumerate() {
            let s = options[k][path[k]];
            if s == 0 {
                continue;
            }
            for x in &bars[b] {
                if let Some(bd) = nb.bindings.get_mut(&x.key) {
                    bd.note = x.note + s;
                }
            }
        }
    }
    // Each ring is placed on its own, so a section's slot can land an octave from the others:
    // a ring whose median sits a fifth or more from the part's moves an octave toward it,
    // where all its notes still fit.
    let Some(part_median) = median(&notes(p, "bass")) else {
        return;
    };
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "bass" {
            continue;
        }
        let ring = ring_notes(nb);
        let ns: Vec<i64> = ring.iter().map(|s| s.note).collect();
        let Some(m) = median(&ns) else {
            continue;
        };
        if (m - part_median).abs() < 7 {
            continue;
        }
        let s = if m > part_median { -12 } else { 12 };
        if !ring
            .iter()
            .all(|x| x.note + s >= BASS_FLOOR && x.note + s <= top)
        {
            continue;
        }
        for x in &ring {
            if let Some(bd) = nb.bindings.get_mut(&x.key) {
                bd.note = x.note + s;
            }
        }
    }
}

// MARK: dynamics

/// Velocity offset by position in the bar (sixteenths).
pub const METER_ACCENT: [i64; 16] = [10, -8, -3, -8, 3, -8, -3, -8, 6, -8, -3, -8, 3, -8, -3, -8];
/// Extra for the highest note of each melody bar.
pub const PEAK_ACCENT: i64 = 5;

/// `accentByMeter`: accent the tonal parts' ring nets by meter.
pub fn accent_by_meter(p: &mut Project) {
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || !["bass", "melody", "arp", "harmony"].contains(&part(nb))
            || nb.transitions.len() % 16 != 0
            || nb.places.len() != nb.transitions.len()
        {
            continue;
        }
        let mut steps: Vec<Slot> = Vec::new();
        for t in nb.bindings.keys().to_vec() {
            let Some(i) = t_index(&t) else { continue };
            let Some(b) = nb.bindings.get(&t) else {
                continue;
            };
            if is_drum_channel(b.channel) {
                continue;
            }
            steps.push(Slot {
                key: t,
                step: i,
                note: b.note,
            });
        }
        let mut peak: BTreeMap<i64, i64> = BTreeMap::new();
        if part(nb) == "melody" {
            for s in &steps {
                let e = peak.entry(s.step / 16).or_insert(0);
                *e = (*e).max(s.note);
            }
        }
        for s in &steps {
            let mut b = *nb.bindings.get(&s.key).unwrap();
            let mut v = b.velocity + METER_ACCENT[(s.step % 16) as usize];
            if peak.get(&(s.step / 16)) == Some(&s.note) {
                v += PEAK_ACCENT;
            }
            b.velocity = clamp_velocity(v);
            nb.bindings.insert(s.key.clone(), b);
        }
    }
}

// MARK: kick and bass

/// `bassLockedToFourOnTheFloor`: dance family, groove template 0, kick on every beat.
pub fn bass_locked_to_four_on_the_floor(genre: &GenreDef) -> bool {
    let t = BeatsTables::shared();
    t.family_of(&genre.name) == 0
        && t.theme_table(&genre.name).map(|x| x.groove) == Some(0)
        && genre.kick.len() >= 3
        && genre.kick[0] == 4
        && genre.kick[1] == 16
        && genre.kick[2] == 0
}

/// `moveBassOffTheKick`: every bass note on a beat moves to that beat's off-beat eighth.
pub fn move_bass_off_the_kick(p: &mut Project, genre: &GenreDef) {
    if !bass_locked_to_four_on_the_floor(genre) {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || part(nb) != "bass"
            || nb.transitions.len() % 16 != 0
            || nb.places.len() != nb.transitions.len()
        {
            continue;
        }
        let n = nb.transitions.len() as i64;
        let mut moved: OrderedMap<MidiBinding> = OrderedMap::new();
        for t in nb.bindings.keys().to_vec() {
            let Some(b) = nb.bindings.get(&t).copied() else {
                continue;
            };
            let Some(i) = t_index(&t) else { continue };
            let target = if i % 4 == 0 {
                format!("t{}", (i + 2) % n)
            } else {
                t.clone()
            };
            if !moved.contains_key(&target) {
                moved.insert(target, b);
            }
        }
        nb.bindings = moved;
    }
}

// MARK: phrases

/// `phraseLengths`: phrase lengths for a section of `steps` split into `count` phrases.
pub fn phrase_lengths(steps: i64, count: i64) -> Vec<i64> {
    let even = |c: i64| vec![steps / c.max(1); c.max(1) as usize];
    if !(count > 1 && (steps % count == 0 || steps % 32 == 0)) {
        return even(count);
    }
    if steps % count == 0 && ((steps / count) % 64 == 0 || steps / count <= 32) {
        return vec![steps / count; count as usize];
    }
    if !(steps % 32 == 0 && steps / 32 >= count) {
        return vec![steps / count; count as usize];
    }
    let mut units = steps / 32;
    let mut out = Vec::new();
    for left in (1..=count).rev() {
        let mut p = 1;
        while p * 2 <= units - (left - 1) {
            p *= 2;
        }
        if left == 1 {
            p = units;
        }
        out.push(p * 32);
        units -= p;
    }
    out
}

/// `alignPhrases`: move the phrase-start events of the role control nets from the even split
/// onto `phraseLengths`.
pub fn align_phrases(p: &mut Project) {
    struct Sec {
        start: i64,
        steps: i64,
        count: i64,
    }
    let mut sections: Vec<Sec> = Vec::new();
    let mut t = 0;
    for s in &p.structure {
        let count = s
            .phrases
            .values()
            .map(|v| v.len() as i64)
            .max()
            .unwrap_or(1);
        sections.push(Sec {
            start: t,
            steps: s.steps,
            count,
        });
        t += s.steps;
    }
    if sections.is_empty() {
        return;
    }
    let remap = |tick: i64| -> Option<i64> {
        let Some(s) = sections
            .iter()
            .find(|s| tick > s.start && tick < s.start + s.steps)
        else {
            return Some(tick);
        };
        let even = s.steps / s.count.max(1);
        let offset = tick - s.start;
        if !(even > 0 && offset % even == 0) {
            return None;
        }
        let lengths = phrase_lengths(s.steps, s.count);
        Some(s.start + lengths.iter().take((offset / even) as usize).sum::<i64>())
    };
    for id in nets_keys(p) {
        if !id.starts_with("struct-") {
            continue;
        }
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role != "control" || !nb.places.contains_key("p_delay_0") {
            continue;
        }
        let mut gaps: Vec<f64> = Vec::new();
        while let Some(d) = nb
            .places
            .get(&format!("p_delay_{}", gaps.len()))
            .and_then(|pl| pl.initial.first())
        {
            gaps.push(*d);
        }
        let mut ticks = Vec::new();
        let mut acc = 0i64;
        for g in &gaps {
            acc += *g as i64;
            ticks.push(acc);
        }
        let moved: Vec<Option<i64>> = ticks.iter().map(|&t| remap(t)).collect();
        if moved.iter().any(|m| m.is_none()) {
            continue;
        }
        let new: Vec<i64> = moved.into_iter().map(|m| m.unwrap()).collect();
        let increasing = new.windows(2).all(|w| w[0] < w[1]);
        if !(increasing && new.first().map_or(false, |&f| f > 0)) {
            continue;
        }
        let mut prev = 0;
        for (i, &tick) in new.iter().enumerate() {
            nb.places.get_mut(&format!("p_delay_{i}")).unwrap().initial =
                vec![(tick - prev) as f64];
            prev = tick;
        }
    }
}

// MARK: fills

/// `fillVelocities`: velocity of a fill's first and last hit.
pub fn fill_velocities(genre: &GenreDef) -> (i64, i64) {
    if BeatsTables::shared().family_of(&genre.name) == 0 {
        (50, 115)
    } else {
        (40, 95)
    }
}

fn first_drum_binding(nb: &NetBundle) -> Option<MidiBinding> {
    nb.bindings
        .values()
        .find(|b| is_drum_channel(b.channel))
        .copied()
}

/// `playFills`: every `fill-N` net that ends before the song does plays its steps as one
/// snare roll, unmuted.
pub fn play_fills(p: &mut Project, genre: &GenreDef) {
    let total: i64 = p.structure.iter().map(|s| s.steps).sum();
    let (from, to) = fill_velocities(genre);
    let mut sounding: BTreeSet<String> = BTreeSet::new();
    for id in nets_keys(p) {
        if !id.starts_with("fill-") {
            continue;
        }
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        let mut steps: Vec<i64> = nb
            .bindings
            .keys()
            .iter()
            .filter(|k| k.starts_with('t'))
            .filter_map(|k| t_index(k))
            .collect();
        steps.sort();
        let Some(&last) = steps.last() else { continue };
        if last + 1 >= total {
            continue;
        }
        for (k, &i) in steps.iter().enumerate() {
            let key = format!("t{i}");
            let Some(b) = nb.bindings.get_mut(&key) else {
                continue;
            };
            if !is_drum_channel(b.channel) {
                continue;
            }
            let x = if steps.len() > 1 {
                k as f64 / (steps.len() - 1) as f64
            } else {
                1.0
            };
            b.note = 38;
            b.velocity = clamp_velocity(round_i(from as f64 + (to - from) as f64 * x));
        }
        sounding.insert(id);
    }
    p.initial_mutes.retain(|m| !sounding.contains(m));
}

// MARK: buildup

/// `rollSpacing`: sixteenths between roll hits, by bars left in the buildup.
pub fn roll_spacing(bars_left: i64) -> i64 {
    match bars_left {
        1..=2 => 1,
        3..=4 => 2,
        _ => 4,
    }
}

/// `rollIntoDrops`: each `buildup` section followed by another gets a snare roll that doubles
/// its rate over its last eight bars, on the fill net that ends on its last tick.
pub fn roll_into_drops(p: &mut Project, genre: &GenreDef) {
    let mut ends: BTreeMap<i64, i64> = BTreeMap::new();
    let mut t = 0;
    for (k, s) in p.structure.iter().enumerate() {
        t += s.steps;
        if s.name == "buildup" && k + 1 < p.structure.len() {
            ends.insert(t, s.steps);
        }
    }
    if ends.is_empty() {
        return;
    }
    let (_, peak) = fill_velocities(genre);
    let soft = 36;
    let mut sounding: BTreeSet<String> = BTreeSet::new();
    for id in nets_keys(p) {
        if !id.starts_with("fill-") {
            continue;
        }
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        let Some(&steps) = ends.get(&(nb.transitions.len() as i64)) else {
            continue;
        };
        let Some(template) = first_drum_binding(nb) else {
            continue;
        };
        let boundary = nb.transitions.len() as i64;
        let length = steps.min(128);
        let mut hits: Vec<i64> = Vec::new();
        let mut s = boundary - length;
        while s < boundary {
            hits.push(s);
            s += roll_spacing((boundary - s + 15) / 16);
        }
        let mut bound: OrderedMap<MidiBinding> = OrderedMap::new();
        for (k, &step) in hits.iter().enumerate() {
            let mut b = template;
            b.note = 38;
            let x = if hits.len() > 1 {
                k as f64 / (hits.len() - 1) as f64
            } else {
                1.0
            };
            b.velocity = clamp_velocity(round_i(soft as f64 + (peak - soft) as f64 * x * x));
            bound.insert(format!("t{step}"), b);
        }
        nb.bindings = bound;
        sounding.insert(id);
    }
    p.initial_mutes.retain(|m| !sounding.contains(m));
}

// MARK: crash

/// The note a crash is bound to (GM crash cymbal 1).
pub const CRASH_NOTE: i64 = 49;
/// Sections that start with the full band, where the crash is loudest.
pub const PEAK_SECTIONS: [&str; 2] = ["drop", "chorus"];

/// `crashOnSections`: each section start after the first gets a crash on its downbeat, on
/// the fill net whose chain runs through it.
pub fn crash_on_sections(p: &mut Project, genre: &GenreDef) {
    if p.structure.len() <= 1 {
        return;
    }
    let mut starts: BTreeMap<i64, String> = BTreeMap::new();
    let mut t = 0;
    for s in &p.structure {
        if t > 0 {
            starts.insert(t, s.name.clone());
        }
        t += s.steps;
    }
    let dance = BeatsTables::shared().family_of(&genre.name) == 0;
    let mut fill_nets: Vec<(String, i64)> = Vec::new();
    for id in nets_keys(p) {
        if id.starts_with("fill-") {
            if let Some(nb) = p.nets.get(&id) {
                fill_nets.push((id.clone(), nb.transitions.len() as i64));
            }
        }
    }
    let mut sounding: BTreeSet<String> = BTreeSet::new();
    for (tick, name) in &starts {
        // the shortest fill chain that reaches the downbeat
        let host =
            fill_nets
                .iter()
                .filter(|f| f.1 > *tick)
                .fold(None::<&(String, i64)>, |best, f| match best {
                    Some(b) if b.1 <= f.1 => Some(b),
                    _ => Some(f),
                });
        let Some(host) = host else { continue };
        let muted = p.initial_mutes.contains(&host.0);
        let Some(nb) = p.nets.get_mut(&host.0) else {
            continue;
        };
        let Some(template) = first_drum_binding(nb) else {
            continue;
        };
        if muted {
            nb.bindings = OrderedMap::new(); // its toms have no voice; keep only crashes
        }
        let mut b = template;
        b.note = CRASH_NOTE;
        b.velocity = if PEAK_SECTIONS.contains(&name.as_str()) {
            if dance {
                118
            } else {
                96
            }
        } else if dance {
            92
        } else {
            76
        };
        nb.bindings.insert(format!("t{tick}"), b);
        sounding.insert(host.0.clone());
    }
    p.initial_mutes.retain(|m| !sounding.contains(m));
}

// MARK: voicing

/// `motion`: semitones the voices move from chord `a` to chord `b`, lowest paired with lowest.
pub fn motion(a: &[i64], b: &[i64]) -> i64 {
    let mut x = a.to_vec();
    let mut y = b.to_vec();
    x.sort();
    y.sort();
    let mut d = 12 * (x.len() as i64 - y.len() as i64).abs();
    for (u, v) in x.iter().zip(y.iter()) {
        d += (u - v).abs();
    }
    d
}

/// `voiceLeadPads`: re-voice each pad net's chords by octaves so the cycle of chords moves
/// least, every note kept inside the pad's range.
pub fn voice_lead_pads(p: &mut Project) {
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || part(nb) != "harmony" {
            continue;
        }
        let mut steps: Vec<Slot> = Vec::new();
        for t in nb.bindings.keys().to_vec() {
            let Some(i) = t_index(&t) else { continue };
            let Some(b) = nb.bindings.get(&t) else {
                continue;
            };
            if is_drum_channel(b.channel) {
                continue;
            }
            steps.push(Slot {
                key: t,
                step: i,
                note: b.note,
            });
        }
        steps.sort_by_key(|s| s.step);
        let mut chords: Vec<Vec<Slot>> = Vec::new();
        for s in &steps {
            let extend = chords
                .last()
                .and_then(|c| c.last())
                .map_or(false, |l| s.step == l.step + 1);
            if extend {
                chords.last_mut().unwrap().push(s.clone());
            } else {
                chords.push(vec![s.clone()]);
            }
        }
        if chords.len() <= 1 {
            continue;
        }
        let (Some(lo), Some(top)) = (
            steps.iter().map(|s| s.note).min(),
            steps.iter().map(|s| s.note).max(),
        ) else {
            continue;
        };
        let hi = top.max(lo + 12);
        // every octave placement of each chord's notes inside the range, no two a semitone
        // or less apart
        let candidates: Vec<Vec<Vec<i64>>> = chords
            .iter()
            .map(|chord| {
                let mut out: Vec<Vec<i64>> = Vec::new();
                fn place(
                    chord: &[Slot],
                    i: usize,
                    acc: &mut Vec<i64>,
                    lo: i64,
                    hi: i64,
                    out: &mut Vec<Vec<i64>>,
                ) {
                    if i == chord.len() {
                        out.push(acc.clone());
                        return;
                    }
                    for k in -2i64..=2 {
                        let n = chord[i].note + 12 * k;
                        if n >= lo && n <= hi && !acc.iter().any(|a| (a - n).abs() <= 1) {
                            acc.push(n);
                            place(chord, i + 1, acc, lo, hi, out);
                            acc.pop();
                        }
                    }
                }
                place(chord, 0, &mut Vec::new(), lo, hi, &mut out);
                if out.is_empty() {
                    vec![chord.iter().map(|s| s.note).collect()]
                } else {
                    out
                }
            })
            .collect();
        // distance from the composed voicing, to break ties
        let drift = |c: usize, v: &[i64]| -> i64 {
            chords[c]
                .iter()
                .zip(v.iter())
                .map(|(s, n)| (s.note - n).abs())
                .sum()
        };
        let mut best: Option<(i64, Vec<usize>)> = None;
        let unreachable = i64::MAX / 4;
        for first in 0..candidates[0].len() {
            let mut cost = vec![unreachable; candidates[0].len()];
            cost[first] = drift(0, &candidates[0][first]);
            let mut back: Vec<Vec<usize>> = vec![(0..cost.len()).collect()];
            for c in 1..chords.len() {
                let mut next = vec![unreachable; candidates[c].len()];
                let mut from = vec![0usize; candidates[c].len()];
                for (j, v) in candidates[c].iter().enumerate() {
                    for (i, u) in candidates[c - 1].iter().enumerate() {
                        if cost[i] >= unreachable {
                            continue;
                        }
                        let x = cost[i] + 100 * motion(u, v) + drift(c, v);
                        if x < next[j] {
                            next[j] = x;
                            from[j] = i;
                        }
                    }
                }
                cost = next;
                back.push(from);
            }
            let lastc = chords.len() - 1;
            for (j, v) in candidates[lastc].iter().enumerate() {
                if cost[j] >= unreachable {
                    continue;
                }
                let total = cost[j] + 100 * motion(v, &candidates[0][first]);
                if best.as_ref().map_or(true, |(c, _)| total < *c) {
                    let mut path = vec![j];
                    for c in (1..=lastc).rev() {
                        let prev = *path.last().unwrap();
                        path.push(back[c][prev]);
                    }
                    path.reverse();
                    best = Some((total, path));
                }
            }
        }
        let Some((_, path)) = best else { continue };
        for (c, chord) in chords.iter().enumerate() {
            for (s, n) in chord.iter().zip(candidates[c][path[c]].iter()) {
                if let Some(b) = nb.bindings.get_mut(&s.key) {
                    b.note = *n;
                }
            }
        }
    }
}

// MARK: open hats

/// The note an open hat is bound to (GM open hi-hat).
pub const OPEN_HAT_NOTE: i64 = 46;

/// `fourOnTheFloor`: dance-family genres with a kick on every beat.
pub fn four_on_the_floor(genre: &GenreDef) -> bool {
    BeatsTables::shared().family_of(&genre.name) == 0
        && genre.kick.len() >= 3
        && genre.kick[0] == 4
        && genre.kick[1] == 16
        && genre.kick[2] == 0
}

/// `openOffbeatHats`: the hats on the off-beat eighth become open hats.
pub fn open_offbeat_hats(p: &mut Project, genre: &GenreDef) {
    if !four_on_the_floor(genre) {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || !(nb.riff_group == "hihat" || id == "hihat")
            || nb.transitions.len() % 4 != 0
        {
            continue;
        }
        for t in nb.bindings.keys().to_vec() {
            let b = nb.bindings.get_mut(&t).unwrap();
            if !(is_drum_channel(b.channel) && drum_kind(b.note) == "hat") {
                continue;
            }
            let Some(i) = t_index(&t) else { continue };
            if i % 4 != 2 {
                continue;
            }
            b.note = OPEN_HAT_NOTE;
        }
    }
}

// MARK: drop gap

/// How long the rhythm section stops before a drop (`.dropGap`): a beat, in sixteenths.
pub const DROP_GAP_STEPS: i64 = 4;
pub const RISING_SECTIONS: [&str; 2] = ["buildup", "pre-chorus"];
pub const PAYOFF_SECTIONS: [&str; 2] = ["drop", "chorus"];

/// `dropGaps`: first step of each gap, ascending.
pub fn drop_gaps(p: &Project) -> Vec<i64> {
    let mut out = Vec::new();
    let mut t = 0;
    for (k, s) in p.structure.iter().enumerate() {
        if k > 0
            && t >= DROP_GAP_STEPS
            && PAYOFF_SECTIONS.contains(&s.name.as_str())
            && RISING_SECTIONS.contains(&p.structure[k - 1].name.as_str())
        {
            out.push(t - DROP_GAP_STEPS);
        }
        t += s.steps;
    }
    out
}

// MARK: ducking

/// `duckDepth`: how far bass and pad dip on a kick (`.duck`).
pub fn duck_depth(name: &str) -> f64 {
    let t = BeatsTables::shared();
    let Some(genre) = t.genres.get(name) else {
        return 0.0;
    };
    if t.kin(name) == "synthwave" {
        return 0.4;
    }
    if t.family_of(name) != 0 {
        return 0.0;
    }
    let four =
        genre.kick.len() >= 3 && genre.kick[0] == 4 && genre.kick[1] == 16 && genre.kick[2] == 0;
    if four {
        0.5
    } else {
        0.35
    }
}

// MARK: contour

/// `neighbour`: the nearest note of the key above (`up`) or below `note`, at most a minor
/// third away.
pub fn neighbour(note: i64, up: bool, key: &BTreeSet<i64>) -> Option<i64> {
    for d in 1..=3 {
        let n = if up { note + d } else { note - d };
        if key.contains(&(((n % 12) + 12) % 12)) {
            return Some(n);
        }
    }
    None
}

/// `stepRepeatedNotes`: an off-beat note repeating the pitch before it moves a scale step.
pub fn step_repeated_notes(p: &mut Project) {
    let key = key_pitch_classes(p);
    if key.is_empty() {
        return;
    }
    // the pad's pitch classes in each bar of its cycle
    let mut chord_at: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    let mut pad_bars = 0i64;
    for nb in p.nets.values() {
        if nb.role != "control" && part(nb) == "harmony" {
            pad_bars = pad_bars.max(nb.transitions.len() as i64 / 16);
            for (t, b) in nb.bindings.iter() {
                if t.starts_with('t') && !is_drum_channel(b.channel) {
                    if let Some(i) = t_index(t) {
                        chord_at.entry(i / 16).or_default().insert(b.note % 12);
                    }
                }
            }
        }
    }
    let rubs = |note: i64, step: i64| -> bool {
        if pad_bars <= 0 {
            return false;
        }
        let Some(chord) = chord_at.get(&((step / 16) % pad_bars)) else {
            return false;
        };
        chord.iter().any(|c| {
            let d = ((note - c) % 12 + 12) % 12;
            d == 1 || d == 11
        })
    };
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || part(nb) != "melody"
            || nb.transitions.len() % 16 != 0
            || nb.places.len() != nb.transitions.len()
        {
            continue;
        }
        let mut steps: Vec<Slot> = Vec::new();
        for t in nb.bindings.keys().to_vec() {
            let Some(i) = t_index(&t) else { continue };
            let Some(b) = nb.bindings.get(&t) else {
                continue;
            };
            if is_drum_channel(b.channel) {
                continue;
            }
            steps.push(Slot {
                key: t,
                step: i,
                note: b.note,
            });
        }
        steps.sort_by_key(|s| s.step);
        if steps.len() <= 2 {
            continue;
        }
        let lo = steps.iter().map(|s| s.note).min().unwrap() - 2;
        let hi = steps.iter().map(|s| s.note).max().unwrap() + 2;
        let mut played: Vec<i64> = steps.iter().map(|s| s.note).collect();
        for k in 1..steps.len() {
            if steps[k].step % 4 == 0 {
                continue;
            }
            if !(played[k] == played[k - 1] && steps[k].step - steps[k - 1].step <= 4) {
                continue;
            }
            let next = if k + 1 < steps.len() {
                steps[k + 1].note
            } else {
                steps[0].note
            };
            let up = if next == played[k] {
                true
            } else {
                next > played[k]
            };
            let options: Vec<i64> = [up, !up]
                .into_iter()
                .filter_map(|u| neighbour(played[k], u, &key))
                .collect();
            let Some(n) = options
                .into_iter()
                .find(|&n| n >= lo && n <= hi && !rubs(n, steps[k].step))
            else {
                continue;
            };
            played[k] = n;
            if let Some(b) = nb.bindings.get_mut(&steps[k].key) {
                b.note = n;
            }
        }
    }
}

// MARK: colour

/// `chordColor`: the interval each pad chord gains in `genre`, by preference.
pub fn chord_color(genre: &str) -> Option<[i64; 2]> {
    match crate::gen::tables::BeatsTables::shared().kin(genre) {
        "jazz" | "bossa" | "house" | "garage" | "funk" => Some([10, 11]),
        _ => None,
    }
}

/// `ColorTone`: one tone `colorChords` added.
#[derive(Clone, Debug)]
pub struct ColorTone {
    pub net: String,
    pub step: i64,
    pub chord_steps: Vec<i64>,
    pub root: i64,
    pub candidates: Vec<i64>,
    pub lo: i64,
    pub hi: i64,
}

/// `colorChords`: give each pad chord one more tone where the style has it.
pub fn color_chords(p: &mut Project, genre: &GenreDef) -> Vec<ColorTone> {
    let Some(intervals) = chord_color(&genre.name) else {
        return Vec::new();
    };
    let key = key_pitch_classes(p);
    if key.len() != 7 {
        return Vec::new(); // a seventh is a diatonic scale's
    }
    let mut added = Vec::new();
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || part(nb) != "harmony" {
            continue;
        }
        let mut steps: Vec<(i64, i64)> = Vec::new();
        for (t, b) in nb.bindings.iter() {
            if t.starts_with('t') && !is_drum_channel(b.channel) {
                if let Some(i) = t_index(t) {
                    steps.push((i, b.note));
                }
            }
        }
        steps.sort_by_key(|s| s.0);
        let (Some(lo), Some(top)) = (
            steps.iter().map(|s| s.1).min(),
            steps.iter().map(|s| s.1).max(),
        ) else {
            continue;
        };
        let hi = top.max(lo + 12);
        let mut chords: Vec<Vec<(i64, i64)>> = Vec::new();
        for s in &steps {
            let extend = chords
                .last()
                .and_then(|c| c.last())
                .map_or(false, |l| s.0 == l.0 + 1);
            if extend {
                chords.last_mut().unwrap().push(*s);
            } else {
                chords.push(vec![*s]);
            }
        }
        for chord in &chords {
            let next = chord.last().unwrap().0 + 1;
            let nkey = format!("t{next}");
            if !(next < nb.transitions.len() as i64
                && next / 16 == chord[0].0 / 16
                && nb.transitions.contains_key(&nkey)
                && !nb.bindings.contains_key(&nkey))
            {
                continue;
            }
            let Some(last) = nb
                .bindings
                .get(&format!("t{}", chord.last().unwrap().0))
                .copied()
            else {
                continue;
            };
            let pcs: BTreeSet<i64> = chord.iter().map(|c| c.1 % 12).collect();
            let candidates: Vec<i64> = intervals
                .iter()
                .map(|i| (chord[0].1 + i) % 12)
                .filter(|c| key.contains(c) && !pcs.contains(c))
                .collect();
            let Some(&pc) = candidates.first() else {
                continue;
            };
            // the octave nearest the chord's middle, inside the pad's range, more than a
            // semitone from its notes
            let mid: i64 = chord.iter().map(|c| c.1).sum::<i64>() / chord.len() as i64;
            let below = mid - ((mid - pc) % 12 + 12) % 12;
            let mut note: Option<i64> = None;
            for n in [below, below + 12, below - 12, below + 24, below - 24] {
                let clear = n >= lo && n <= hi && !chord.iter().any(|c| (c.1 - n).abs() <= 1);
                if clear && note.map_or(true, |m| (n - mid).abs() < (m - mid).abs()) {
                    note = Some(n);
                }
            }
            let Some(note) = note else { continue };
            let mut b = last;
            b.note = note;
            nb.bindings.insert(nkey, b);
            added.push(ColorTone {
                net: id.clone(),
                step: next,
                chord_steps: chord.iter().map(|c| c.0).collect(),
                root: chord[0].1 % 12,
                candidates,
                lo,
                hi,
            });
        }
    }
    added
}

/// See `unrubColor`: about one rubbing note in twelve is let through.
pub const COLOR_RUB_SHARE: f64 = 0.085;

/// `unrubColor`: a colour tone that sits a semitone from the other parts' notes in its span,
/// or a major seventh voiced under a root, moves to the next candidate that does neither.
pub fn unrub_color(p: &mut Project, tones: &[ColorTone]) {
    let wanted: BTreeSet<&str> = tones.iter().map(|t| t.net.as_str()).collect();
    let mut spans: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    struct Other {
        start: i64,
        end: i64,
        pc: i64,
        group: String,
    }
    let mut others: Vec<Other> = Vec::new();
    let mut g = crate::wave::net::WaveGraph::new(p);
    let tempo = if g.tempo > 0.0 { g.tempo } else { 120.0 };
    let ms_per_step = 60000.0 / (tempo * 4.0);
    let group: Vec<String> = g
        .nets
        .iter()
        .map(|n| note_log::group_name(&n.bundle, &n.id))
        .collect();
    let mut group_of: BTreeMap<String, String> = BTreeMap::new();
    for (n, name) in g.nets.iter().zip(group.iter()) {
        group_of.insert(n.id.clone(), name.clone());
    }
    for _ in 0..note_log::length(p) {
        let tick = g.tick + 1;
        {
            let spans = &mut spans;
            let others = &mut others;
            let wanted = &wanted;
            let group = &group;
            g.advance_with(|n, t| {
                if is_drum_channel(n.channel[t] as i64) {
                    return;
                }
                let steps = if n.dur_steps[t] > 0.0 {
                    n.dur_steps[t]
                } else {
                    n.dur_ms[t] / ms_per_step
                };
                let end = tick + (libm::ceil(steps) as i64).max(1);
                if wanted.contains(n.id.as_str()) {
                    spans
                        .entry(format!("{}/{}", n.id, n.trans_ids[t]))
                        .or_default()
                        .push((tick, end));
                } else {
                    others.push(Other {
                        start: tick,
                        end,
                        pc: n.note[t] % 12,
                        group: group[n.index].clone(),
                    });
                }
            });
        }
        if g.stop_requested {
            break;
        }
    }
    others.sort_by_key(|o| o.start);
    let heard = |s: (i64, i64), from: &str| -> Vec<i64> {
        let mut pcs = Vec::new();
        for o in &others {
            if o.start >= s.1 {
                break;
            }
            if o.end > s.0 && o.group != from {
                pcs.push(o.pc);
            }
        }
        pcs
    };
    for tone in tones {
        let key = format!("t{}", tone.step);
        let Some(nb) = p.nets.get_mut(&tone.net) else {
            continue;
        };
        let Some(b) = nb.bindings.get(&key).copied() else {
            continue;
        };
        let mut against: Vec<i64> = Vec::new();
        let own = group_of.get(&tone.net).cloned().unwrap_or_default();
        if let Some(ss) = spans.get(&format!("{}/{}", tone.net, key)) {
            for s in ss {
                against.extend(heard(*s, &own));
            }
        }
        let chord: Vec<i64> = tone
            .chord_steps
            .iter()
            .filter_map(|s| nb.bindings.get(&format!("t{s}")).map(|b| b.note))
            .collect();
        let rubs = |pc: i64| -> bool {
            let n = against
                .iter()
                .filter(|d| [1, 11].contains(&(((pc - **d) % 12 + 12) % 12)))
                .count();
            n > 0 && n as f64 > COLOR_RUB_SHARE * against.len() as f64
        };
        // a major seventh just under a voiced root: the root's octave, a semitone up
        let under_root = |n: i64| -> bool {
            (n - tone.root + 12 * 11) % 12 == 11
                && chord
                    .iter()
                    .any(|c| c % 12 == tone.root && [1, 13].contains(&(c - n)))
        };
        if Some(b.note % 12) == tone.candidates.first().copied()
            && !rubs(b.note % 12)
            && !under_root(b.note)
        {
            continue;
        }
        let mut pick: Option<i64> = None;
        for &pc in &tone.candidates {
            if rubs(pc) {
                continue;
            }
            let below = b.note - ((b.note - pc) % 12 + 12) % 12;
            for n in [below, below + 12, below - 12, below + 24, below - 24] {
                let clear = n >= tone.lo
                    && n <= tone.hi
                    && !chord.iter().any(|c| (c - n).abs() <= 1)
                    && !under_root(n);
                if clear && pick.map_or(true, |p| (n - b.note).abs() < (p - b.note).abs()) {
                    pick = Some(n);
                }
            }
            if pick.is_some() {
                break;
            }
        }
        match pick {
            Some(n) => {
                let mut moved = b;
                moved.note = n;
                nb.bindings.insert(key, moved);
            }
            None => {
                nb.bindings.remove(&key);
            }
        }
    }
}

// MARK: arpeggio

/// `padChords`: the pad's chord in each bar of its cycle, as pitch classes in the order it
/// strikes them (root first), and the cycle's length in bars; empty without a pad.
pub fn pad_chords(p: &Project) -> (BTreeMap<i64, Vec<i64>>, i64) {
    let mut bars = 0i64;
    let mut struck: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for nb in p.nets.values() {
        if nb.role != "control" && part(nb) == "harmony" {
            bars = bars.max(nb.transitions.len() as i64 / 16);
            for (t, b) in nb.bindings.iter() {
                if t.starts_with('t') && !is_drum_channel(b.channel) {
                    if let Some(i) = t_index(t) {
                        struck.entry(i / 16).or_default().push((i, b.note % 12));
                    }
                }
            }
        }
    }
    let mut chords = BTreeMap::new();
    for (bar, mut s) in struck {
        s.sort_by_key(|x| x.0);
        let mut pcs: Vec<i64> = Vec::new();
        for x in s {
            if !pcs.contains(&x.1) {
                pcs.push(x.1);
            }
        }
        chords.insert(bar, pcs);
    }
    (chords, bars)
}

/// Which of the five chord tones each sixteenth of a bar plays (beats-like figure).
pub const ARP_PATTERN: [i64; 16] = [0, 1, 2, 3, 1, 2, 3, 4, 2, 3, 4, 3, 2, 1, 2, 1];

/// `ArpFigure`: which tone of a `span`-tone ladder each sixteenth plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArpFigure {
    pub shape: i64,
    pub span: i64,
    pub start: i64,
    pub rate: i64,
    pub phase: i64,
    pub octaves: bool,
}

impl ArpFigure {
    pub const SHAPES: usize = 6;

    /// The ladder index for sixteenth `s` (0…15).
    pub fn tone(&self, s: i64) -> i64 {
        let i = s / self.rate / (if self.octaves { 2 } else { 1 }) + self.phase;
        let n = self.span;
        let x = match self.shape {
            1 => n - 1 - i % n,
            2 => {
                let period = (2 * n - 2).max(1);
                let k = i % period;
                if k < n {
                    k
                } else {
                    period - k
                }
            }
            3 => (i / 2 + (i % 2) * 2) % n,
            4 => {
                if i % 2 == 0 {
                    0
                } else {
                    1 + (i / 2) % (n - 1).max(1)
                }
            }
            5 => (n - 1).min(ARP_PATTERN[s as usize]),
            _ => i % n,
        };
        (x + self.start) % n
    }

    /// Semitones above the ladder tone for sixteenth `s`.
    pub fn octave(&self, s: i64) -> i64 {
        if self.octaves && (s / self.rate) % 2 == 1 {
            12
        } else {
            0
        }
    }

    /// `ArpFigure.seeded(_:variant:genre:)`.
    pub fn seeded(seed: f64, variant: &str, genre: Option<&str>) -> ArpFigure {
        let u = js_to_int32(seed) as u32;
        let mut rng = Mulberry32::from_i32((u ^ hash_str(&format!("dj-arp/{variant}"))) as i32);
        match genre.map(|g| crate::gen::tables::BeatsTables::shared().kin(g)) {
            Some("trance") => {
                let span = 4 + rng.next_int(3) as i64;
                let phase = rng.next_int((2 * span - 2) as usize) as i64;
                return ArpFigure {
                    shape: 2,
                    span,
                    start: 0,
                    rate: if variant == "C" { 2 } else { 1 },
                    phase,
                    octaves: false,
                };
            }
            Some("synthwave") => {
                let shape = [0, 2, 1][rng.next_int(3)];
                let span = 3 + rng.next_int(3) as i64;
                let start = if shape == 2 {
                    0
                } else {
                    rng.next_int(span as usize) as i64
                };
                let phase = if shape == 2 {
                    rng.next_int((2 * span - 2) as usize) as i64
                } else {
                    0
                };
                return ArpFigure {
                    shape,
                    span,
                    start,
                    rate: 2,
                    phase,
                    octaves: true,
                };
            }
            Some("edm") => {
                let shape = if rng.next_int(2) == 0 { 0 } else { 3 };
                let span = 4 + rng.next_int(3) as i64;
                let start = rng.next_int(span as usize) as i64;
                return ArpFigure {
                    shape,
                    span,
                    start,
                    rate: if variant == "C" { 2 } else { 1 },
                    phase: 0,
                    octaves: false,
                };
            }
            _ => {}
        }
        let shape = rng.next_int(Self::SHAPES) as i64;
        let span = 4 + rng.next_int(3) as i64;
        let start = rng.next_int(span as usize) as i64;
        let mut rate = if rng.next_int(3) == 2 { 2 } else { 1 };
        if variant == "C" {
            rate = 2;
        }
        ArpFigure {
            shape,
            span,
            start,
            rate,
            phase: 0,
            octaves: false,
        }
    }
}

/// `arpeggiate`: every arp ring whose length is a whole pad cycle is rewritten bar by bar
/// to play the bar's pad chord as a ladder of chord tones in the slot's `ArpFigure`.
/// With `rates` (`.arpDensity`: net → ring bar → steps between notes) a bar listed there
/// plays at that rate instead of the figure's.
pub fn arpeggiate(
    p: &mut Project,
    seed: f64,
    genre: Option<&str>,
    rates: Option<&BTreeMap<String, BTreeMap<i64, i64>>>,
) {
    let (chords, pad_bars) = pad_chords(p);
    if pad_bars <= 0 {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control"
            || part(nb) != "arp"
            || nb.transitions.len() as i64 % (16 * pad_bars) != 0
            || nb.places.len() != nb.transitions.len()
        {
            continue;
        }
        let mut steps: Vec<(i64, MidiBinding)> = Vec::new();
        for (t, b) in nb.bindings.iter() {
            if t.starts_with('t') && !is_drum_channel(b.channel) {
                if let Some(i) = t_index(t) {
                    steps.push((i, *b));
                }
            }
        }
        let notes_v: Vec<i64> = steps.iter().map(|s| s.1.note).collect();
        let (Some(m), Some(template)) = (median(&notes_v), steps.first().map(|s| s.1)) else {
            continue;
        };
        let low = m - 5;
        let figure = ArpFigure::seeded(seed, &nb.riff_variant, genre);
        let mut bound: OrderedMap<MidiBinding> = OrderedMap::new();
        for bar in 0..(nb.transitions.len() as i64 / 16) {
            let in_bar: Vec<&(i64, MidiBinding)> =
                steps.iter().filter(|s| s.0 / 16 == bar).collect();
            let Some(chord) = chords.get(&(bar % pad_bars)).filter(|c| !c.is_empty()) else {
                continue;
            };
            let Some(velocity) = in_bar.iter().map(|s| s.1.velocity).max() else {
                continue;
            };
            // the chord's tones from `low` up, `span` of them
            let mut ladder: Vec<i64> = Vec::new();
            let mut n = low;
            while (ladder.len() as i64) < figure.span && n < low + 48 {
                if chord.contains(&(((n % 12) + 12) % 12)) {
                    ladder.push(n);
                }
                n += 1;
            }
            if ladder.len() as i64 != figure.span {
                continue;
            }
            let rate = rates
                .and_then(|r| r.get(&id))
                .and_then(|r| r.get(&bar))
                .copied()
                .unwrap_or(figure.rate);
            let mut s = 0;
            while s < 16 {
                let mut b = template;
                b.note = (ladder[figure.tone(s) as usize] + figure.octave(s)).clamp(0, 127);
                b.velocity = velocity;
                b.duration = 0;
                b.duration_steps = 1;
                bound.insert(format!("t{}", bar * 16 + s), b);
                s += rate;
            }
        }
        if bound.keys().is_empty() {
            continue;
        }
        nb.bindings = bound;
    }
}

// MARK: arp density

pub const ARP_DENSITY_DIRECTION: i64 = 1;
/// Steps between arp notes at each density level: quarters, eighths, sixteenths.
pub const ARP_DENSITY_LEVELS: [i64; 3] = [4, 2, 1];
/// Melody onsets in a bar from which it counts as busy.
pub const ARP_BUSY_ONSETS: i64 = 5;
/// Section energy from which the arp plays eighths, and sixteenths, where no melody plays.
pub const ARP_ENERGY_EIGHTHS: f64 = 0.45;
pub const ARP_ENERGY_SIXTEENTHS: f64 = 0.75;

/// `arpDensityRates`: `.arpDensity`'s rate for every bar of every arp slot ring that plays.
pub fn arp_density_rates(
    p: &Project,
    seed: f64,
    genre: Option<&str>,
) -> BTreeMap<String, BTreeMap<i64, i64>> {
    let melody = melody_onsets_by_bar(p);
    let energy = section_energy(p);
    let mut section_start: Vec<i64> = Vec::new();
    let mut at = 0;
    for s in &p.structure {
        section_start.push(at);
        at += s.steps;
    }
    let top = ARP_DENSITY_LEVELS.len() as i64 - 1;
    let mut out: BTreeMap<String, BTreeMap<i64, i64>> = BTreeMap::new();
    for (id, windows) in slot_windows(p, "arp") {
        let Some(nb) = p.nets.get(&id) else { continue };
        if nb.transitions.len() < 16 {
            continue;
        }
        let figure = ArpFigure::seeded(seed, &nb.riff_variant, genre);
        let base = ARP_DENSITY_LEVELS
            .iter()
            .position(|&r| r == figure.rate)
            .map_or(top, |i| i as i64);
        let length = nb.transitions.len() as i64;
        let mut levels: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for w in &windows {
            let ring_length = w.ring_length.unwrap_or(length);
            let mut step = w.start;
            while step < w.end {
                let bar = step / 16;
                let mut level: i64;
                if let Some(&n) = melody.get(&bar) {
                    level = if n == 0 {
                        0
                    } else if n < ARP_BUSY_ONSETS {
                        1
                    } else {
                        2
                    };
                    if ARP_DENSITY_DIRECTION < 0 {
                        level = top - level;
                    }
                } else if let Some(e) = energy
                    .as_ref()
                    .and_then(|e| e.get(w.section).copied().flatten())
                {
                    level = if e >= ARP_ENERGY_SIXTEENTHS {
                        2
                    } else if e >= ARP_ENERGY_EIGHTHS {
                        1
                    } else {
                        0
                    };
                } else {
                    level = base;
                }
                if (step - section_start[w.section]) / 16 % 4 == 3 {
                    level += 1;
                }
                if RAMP_SECTIONS.contains(&w.section_name.as_str()) {
                    level += 1;
                }
                let ring = (w.ring_start.unwrap_or(w.start) + step - w.start) % ring_length;
                levels.entry(ring / 16).or_default().push(level.min(top));
                step += 16;
            }
        }
        let rates: BTreeMap<i64, i64> = levels
            .into_iter()
            .map(|(bar, ls)| {
                let mean = ls.iter().sum::<i64>() as f64 / ls.len() as f64;
                (bar, ARP_DENSITY_LEVELS[round_i(mean) as usize])
            })
            .collect();
        out.insert(id, rates);
    }
    out
}

/// `melodyOnsetsByBar`: melody onsets in each song bar a melody slot plays in.
pub fn melody_onsets_by_bar(p: &Project) -> BTreeMap<i64, i64> {
    let mut out: BTreeMap<i64, i64> = BTreeMap::new();
    for (id, windows) in slot_windows(p, "melody") {
        let Some(nb) = p.nets.get(&id) else { continue };
        if nb.transitions.is_empty() {
            continue;
        }
        let onsets: BTreeSet<i64> = ring_notes(nb).iter().map(|s| s.step).collect();
        for w in &windows {
            let length = w.ring_length.unwrap_or(nb.transitions.len() as i64);
            let mut step = w.start;
            while step < w.end {
                let ring = (w.ring_start.unwrap_or(w.start) + step - w.start) % length;
                let n = onsets
                    .iter()
                    .filter(|&&o| (o - ring + length) % length < 16)
                    .count() as i64;
                *out.entry(step / 16).or_insert(0) += n;
                step += 16;
            }
        }
    }
    out
}

/// `sectionEnergy`: each section's energy over the most energetic section's, by structure
/// index (None for a section it doesn't mark); None without a feel curve.
pub fn section_energy(p: &Project) -> Option<Vec<Option<f64>>> {
    if p.structure.len() <= 1 {
        return None;
    }
    let feel = p.nets.get("feel-curve")?;
    let mut x: BTreeMap<i64, f64> = BTreeMap::new();
    for (t, c) in feel.control_bindings.iter() {
        if c.action != "set-feel" {
            continue;
        }
        let Some(rest) = t.strip_prefix("ft") else {
            continue;
        };
        let Ok(step) = rest.parse::<i64>() else {
            continue;
        };
        let Some(v) = c
            .macro_params
            .as_ref()
            .and_then(|m| m.get("x"))
            .and_then(|v| v.as_f64())
        else {
            continue;
        };
        x.insert(step, v);
    }
    let mut by_name: BTreeMap<String, f64> = BTreeMap::new();
    let mut at = 0;
    for s in &p.structure {
        if !by_name.contains_key(&s.name) {
            if let Some(v) = x.get(&at) {
                by_name.insert(s.name.clone(), *v);
            }
        }
        at += s.steps;
    }
    let peak = by_name.values().copied().fold(None, |m: Option<f64>, v| {
        Some(m.map_or(v, |m| if v > m { v } else { m }))
    })?;
    if peak <= 0.0 {
        return None;
    }
    Some(
        p.structure
            .iter()
            .map(|s| by_name.get(&s.name).map(|v| (v / peak).min(1.0)))
            .collect(),
    )
}

// MARK: legato

/// The longest a melody note is held (sixteenths): a beat.
pub const LEGATO_MAX: i64 = 4;

/// `holdMelodyNotes`: every note of a melody ring lasts until the next note of the ring.
pub fn hold_melody_notes(p: &mut Project) {
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let steps = ring_notes(nb);
        let n = nb.transitions.len() as i64;
        for (k, s) in steps.iter().enumerate() {
            let next = if k + 1 < steps.len() {
                steps[k + 1].step
            } else {
                steps[0].step + n
            };
            let len = (next - s.step).min(LEGATO_MAX);
            let Some(b) = nb.bindings.get_mut(&s.key) else {
                continue;
            };
            if b.duration_steps >= len {
                continue;
            }
            b.duration_steps = len;
            b.duration = 0;
        }
    }
}

// MARK: cadence

/// Sixteenths of rest after a note that end its phrase: half a bar.
pub const PHRASE_REST: i64 = 8;

/// `resolvePhraseEnds`: the last note of each melody phrase lands on a tone of the bar's
/// chord. `triads`: the pad's chords before `.color`.
pub fn resolve_phrase_ends(p: &mut Project, triads: Option<&(BTreeMap<i64, Vec<i64>>, i64)>) {
    let (chords, pad_bars) = match triads {
        Some(t) => t.clone(),
        None => pad_chords(p),
    };
    if pad_bars <= 0 {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let steps = ring_notes(nb);
        if steps.len() <= 1 {
            continue;
        }
        let n = nb.transitions.len() as i64;
        let lo = steps.iter().map(|s| s.note).min().unwrap() - 2;
        let hi = steps.iter().map(|s| s.note).max().unwrap() + 2;
        for (k, s) in steps.iter().enumerate() {
            let next = if k + 1 < steps.len() {
                steps[k + 1].step
            } else {
                steps[0].step + n
            };
            let end = next - s.step >= PHRASE_REST || (n >= 32 && next / 32 > s.step / 32);
            if !end {
                continue;
            }
            let Some(chord) = chords.get(&((s.step / 16) % pad_bars)) else {
                continue;
            };
            if chord.is_empty() || chord.contains(&pc(s.note)) {
                continue;
            }
            let before = steps[(k + steps.len() - 1) % steps.len()].note;
            let mut best: Option<(i64, i64)> = None;
            for d in -5..=5 {
                if d == 0 {
                    continue;
                }
                let c = s.note + d;
                if c < lo || c > hi || !chord.contains(&pc(c)) {
                    continue;
                }
                // two points a semitone; the root, a fall and a new pitch each earn one back
                let mut score = 2 * d.abs();
                if pc(c) == chord[0] {
                    score -= 1;
                }
                if d < 0 {
                    score -= 1;
                }
                if c == before {
                    score += 3;
                }
                if best.map_or(true, |(bs, _)| score < bs) {
                    best = Some((score, c));
                }
            }
            let Some((_, note)) = best else { continue };
            if let Some(b) = nb.bindings.get_mut(&s.key) {
                b.note = note;
            }
        }
    }
}

// MARK: approach

/// `approachChordChanges`: the bass leads into each chord change.
pub fn approach_chord_changes(p: &mut Project, genre: &GenreDef) {
    if genre.name == "jazz" || bass_locked_to_four_on_the_floor(genre) {
        return;
    }
    let key = key_pitch_classes(p);
    if key.is_empty() {
        return;
    }
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "bass" {
            continue;
        }
        let count = nb.transitions.len() / 16;
        if count <= 1 {
            continue;
        }
        let mut bars: Vec<Vec<StepNote>> = vec![Vec::new(); count];
        for s in ring_notes(nb) {
            bars[(s.step / 16) as usize].push(s);
        }
        for b in 0..count {
            if bars[b].len() < 3 {
                continue;
            }
            let (Some(target), Some(last)) = (bars[(b + 1) % count].first(), bars[b].last()) else {
                continue;
            };
            if pc(target.note) == pc(bars[b][0].note)
                || last.step % 16 < 8
                || (last.note - target.note).abs() <= 2
            {
                continue;
            }
            let Some(near) = neighbour(target.note, last.note > target.note, &key) else {
                continue;
            };
            // from above where the step below would be under E1
            let c = if near >= BASS_FLOOR {
                Some(near)
            } else {
                neighbour(target.note, true, &key)
            };
            let Some(c) = c else { continue };
            if (c - target.note).abs() > 2 {
                continue;
            }
            if let Some(bd) = nb.bindings.get_mut(&last.key) {
                bd.note = c;
            }
        }
    }
}

// MARK: motif

/// `diatonic`: `note` moved `k` steps along the key (`scale`: every MIDI note in the key,
/// ascending); None when the note is not in the key or the move leaves the keyboard.
pub fn diatonic(note: i64, k: i64, scale: &[i64]) -> Option<i64> {
    let i = scale.iter().position(|&n| n == note)? as i64;
    let j = i + k;
    if j < 0 || j >= scale.len() as i64 {
        return None;
    }
    Some(scale[j as usize])
}

/// `stateIdeasTwice`: bar 2 of each 4-bar melody phrase replays bar 1's rhythm and shape
/// moved along the scale onto bar 2's chord.
pub fn state_ideas_twice(p: &mut Project, triads: Option<&(BTreeMap<i64, Vec<i64>>, i64)>) {
    let (chords, pad_bars) = match triads {
        Some(t) => t.clone(),
        None => pad_chords(p),
    };
    let key = key_pitch_classes(p);
    if pad_bars <= 0 || key.is_empty() {
        return;
    }
    let scale: Vec<i64> = (0..=127).filter(|n| key.contains(&pc(*n))).collect();
    for id in nets_keys(p) {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !is_bar_ring(nb) || part(nb) != "melody" {
            continue;
        }
        let count = nb.transitions.len() as i64;
        let block = if count % 64 == 0 { 64 } else { 32 };
        if count % block != 0 {
            continue;
        }
        let steps = ring_notes(nb);
        let (Some(lo), Some(hi)) = (
            steps.iter().map(|s| s.note).min(),
            steps.iter().map(|s| s.note).max(),
        ) else {
            continue;
        };
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
            let Some(chord) = chords.get(&(((this + 16) / 16) % pad_bars)) else {
                continue;
            };
            if chord.is_empty() {
                continue;
            }
            // the moves that land bar 1's first note on bar 2's chord, nearest where bar 2
            // began first; the first whose copy stays in range is taken
            let mut moves: Vec<(i64, i64)> = Vec::new();
            for k in -7..=7 {
                let Some(n) = diatonic(first[0].note, k, &scale) else {
                    continue;
                };
                if !chord.contains(&pc(n)) {
                    continue;
                }
                moves.push((k, (n - opening.note).abs()));
            }
            moves.sort_by(|a, b| (a.1, a.0.abs()).cmp(&(b.1, b.0.abs())));
            // the sixteenth after each note of bar 1 (the next note, or bar 2's first)
            let next: Vec<i64> = (0..first.len())
                .map(|j| {
                    if j + 1 < first.len() {
                        first[j + 1].step
                    } else {
                        this + 16
                    }
                })
                .collect();
            // A note that has to be a chord tone goes to the nearest one that keeps the
            // line's direction from the note before, else the nearest.
            let snap = |n: i64, prev: Option<i64>, sign: i64, avoid: Option<i64>| -> i64 {
                let options: Vec<i64> = (-6..=6)
                    .map(|d| n + d)
                    .filter(|o| chord.contains(&pc(*o)) && Some(pc(*o)) != avoid)
                    .collect();
                let keep: Vec<i64> = options
                    .iter()
                    .copied()
                    .filter(|o| prev.map_or(true, |p| (o - p).signum() == sign))
                    .collect();
                let pool = if keep.is_empty() { &options } else { &keep };
                // Swift's `min(by:)` keeps the first of equal elements
                let mut best: Option<i64> = None;
                for &o in pool {
                    if best.map_or(true, |b| (o - n).abs() < (b - n).abs()) {
                        best = Some(o);
                    }
                }
                best.unwrap_or(n)
            };
            let mut chosen: Option<(Vec<(i64, MidiBinding)>, i64)> = None;
            for &(k, d) in &moves {
                let mut copy: Vec<(i64, MidiBinding)> = Vec::new();
                let mut turns = 0i64;
                for (j, s) in first.iter().enumerate() {
                    let Some(mut n) = diatonic(s.note, k, &scale) else {
                        break;
                    };
                    let Some(mut b) = nb.bindings.get(&s.key).copied() else {
                        break;
                    };
                    let step = s.step + 16;
                    let prev = copy.last().map(|c| c.1.note);
                    let sign = if j > 0 {
                        (s.note - first[j - 1].note).signum()
                    } else {
                        0
                    };
                    if j == first.len() - 1 && (pc(n) == chord[0] || !chord.contains(&pc(n))) {
                        n = snap(n, prev, sign, Some(chord[0]));
                    } else if (step % 4 == 0 || next[j] - s.step >= PHRASE_REST)
                        && !chord.contains(&pc(n))
                    {
                        n = snap(n, prev, sign, None);
                    }
                    if let Some(prev) = prev {
                        if (n - prev).signum() != sign {
                            turns += 1;
                        }
                    }
                    b.note = n;
                    copy.push((step, b));
                }
                if copy.len() != first.len()
                    || !copy
                        .iter()
                        .all(|c| c.1.note >= lo - 3 && c.1.note <= hi + 7)
                {
                    continue;
                }
                // the shape first, then nearness to where bar 2 began
                let cost = 100 * turns + d;
                if chosen.as_ref().map_or(true, |c| cost < c.1) {
                    chosen = Some((copy, cost));
                }
            }
            let Some((chosen, _)) = chosen else { continue };
            for s in &second {
                nb.bindings.remove(&s.key);
            }
            for (step, b) in chosen {
                nb.bindings.insert(format!("t{step}"), b);
            }
        }
    }
}

// MARK: stereo

/// `stereoPlacement`: where a lane sits (`.stereo`): `pan` from −1 (left) to 1 (right), as
/// the side gain of a mid/side pan, and `spread`; from `targets.json`'s `stereo`.
pub fn stereo_placement(kind: &str, group: &str) -> (f64, f64) {
    crate::musicality::mix_targets::MixTargets::shared().placement(kind, group)
}

/// `SampledWidth`: how wide a sampled instrument plays (`.stereo`): its slot's side signal
/// scaled by `width` (0 = mono, 1 = as the samples have it), after a side highpass at
/// `side_hz` when it has one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledWidth {
    pub width: f64,
    pub side_hz: Option<f64>,
}

impl SampledWidth {
    pub const AS_LOADED: SampledWidth = SampledWidth {
        width: 1.0,
        side_hz: None,
    };
}

/// The highpass on a widened slot's side signal.
pub const SAMPLED_SIDE_HZ: f64 = 300.0;

/// `sampledWidth(category:group:)`: a sampled slot's width by its `VoiceCatalog` category.
pub fn sampled_width(category: Option<&str>, group: &str) -> SampledWidth {
    if group == "bass" {
        return SampledWidth {
            width: 0.0,
            side_hz: None,
        };
    }
    let Some(c) = category else {
        return SampledWidth::AS_LOADED;
    };
    if c.starts_with("bass.") {
        return SampledWidth {
            width: 0.0,
            side_hz: None,
        };
    }
    if c.starts_with("keys.") || c == "pad" {
        return SampledWidth {
            width: 1.2,
            side_hz: Some(SAMPLED_SIDE_HZ),
        };
    }
    if c.starts_with("guitar.") {
        return SampledWidth {
            width: 0.8,
            side_hz: None,
        };
    }
    if c == "synth.lead" {
        return SampledWidth {
            width: 0.6,
            side_hz: None,
        };
    }
    SampledWidth::AS_LOADED
}
