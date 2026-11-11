//! Port of `Musicality/TrackVariety.swift` (`DJMusicality.Fixes.variety`): what makes two
//! seeds of a genre two different songs. Seeded from the track's seed, a key offset, a
//! progression, a bass figure and drum detail, all inside the genre.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gen::core::scale_intervals;
use crate::gen::tables::{BeatsTables, GenreDef};
use crate::gen::theme::{clamp_degree, hash_str, kick_hit_mask};
use crate::json::js_to_int32;
use crate::musicality::dj::*;
use crate::musicality::util::*;
use crate::pflow::{MidiBinding, NetBundle, OrderedMap, Project};
use crate::rng::Mulberry32;
use crate::spec::TrackSpec;
use crate::wave::synth::is_drum_channel;

/// `varietyRandom`: the pass's random source for one decision.
pub fn variety_random(name: &str, genre: &str, seed: f64) -> Mulberry32 {
    let u = js_to_int32(seed) as u32;
    Mulberry32::from_i32((u ^ hash_str(&format!("dj-variety/{name}/{genre}"))) as i32)
}

/// `Mulberry32.shuffle`: `xs` in a seeded order (Fisher–Yates on this generator's draws).
pub fn shuffle<T: Clone>(rng: &mut Mulberry32, xs: &[T]) -> Vec<T> {
    let mut out = xs.to_vec();
    let mut i = out.len() as i64 - 1;
    while i > 0 {
        let j = rng.next_int((i + 1) as usize);
        out.swap(i as usize, j);
        i -= 1;
    }
    out
}

impl TrackSpec {
    /// The parts `partSeeds` can seed on their own.
    pub const SEEDABLE_PARTS: [&'static str; 6] =
        ["bass", "kick", "snare", "hihat", "melody", "arp"];

    /// `TrackSpec.seed(forPart:)`: the seed `part`'s seeded decisions are drawn from.
    pub fn seed_for_part(&self, part: &str) -> f64 {
        self.part_seeds.get(part).copied().unwrap_or(self.seed)
    }
}

/// The key offsets a track may move by (semitones): −5…6.
pub const KEY_OFFSETS: [i64; 12] = [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6];

/// `progressionTable`: diatonic progressions by scale, the scale degree (0-based) of each
/// bar's chord root.
pub fn progression_table(scale: &str) -> &'static [&'static [i64]] {
    match scale {
        "MinorScale" => &[
            &[0, 5, 2, 6],
            &[0, 6, 5, 6],
            &[0, 5, 3, 4],
            &[5, 6, 0, 0],
            &[0, 3, 5, 6],
            &[0, 2, 6, 5],
            &[0, 0, 5, 6],
            &[0, 3, 0, 4],
        ],
        "MajorScale" => &[
            &[0, 4, 5, 3],
            &[0, 3, 4, 3],
            &[0, 5, 3, 4],
            &[5, 3, 0, 4],
            &[0, 3, 0, 4],
            &[0, 1, 3, 4],
            &[0, 2, 3, 4],
        ],
        "DorianScale" => &[
            &[0, 3, 0, 3],
            &[0, 6, 3, 0],
            &[0, 4, 3, 0],
            &[3, 6, 0, 0],
            &[0, 2, 3, 0],
            &[1, 4, 0, 0],
        ],
        "MixolydianScale" => &[
            &[0, 6, 3, 0],
            &[0, 3, 6, 3],
            &[0, 4, 3, 0],
            &[0, 6, 0, 3],
            &[0, 5, 6, 0],
        ],
        "PhrygianScale" => &[
            &[0, 1, 0, 6],
            &[0, 5, 1, 0],
            &[0, 6, 5, 1],
            &[0, 2, 1, 0],
            &[0, 3, 1, 0],
        ],
        "MinPentatonicScale" => &[&[0, 3, 2, 4], &[0, 2, 3, 0], &[0, 4, 3, 2]],
        "PentatonicScale" => &[&[0, 3, 4, 2], &[0, 2, 3, 1]],
        _ => &[],
    }
}

/// The scale sizes `reharmonize` moves chords in: seven-note keys and pentatonic ones.
pub fn progression_key_size(n: usize) -> bool {
    n == 5 || n == 7
}

/// `progressions(for:)`: beats' (as roots) then dj's table, without repeats.
pub fn progressions(genre: &GenreDef) -> Vec<Vec<i64>> {
    let size = scale_intervals(&genre.scale_name).len();
    if !progression_key_size(size) {
        return Vec::new();
    }
    let mut out: Vec<Vec<i64>> = Vec::new();
    if let Some(theory) = &genre.theory {
        for prog in &theory.chord_progs {
            let roots: Vec<i64> = prog
                .chords
                .iter()
                .map(|c| clamp_degree(c.root) % size as i64)
                .collect();
            if !roots.is_empty() && !out.contains(&roots) {
                out.push(roots);
            }
        }
    }
    for roots in progression_table(&genre.scale_name) {
        let r = roots.to_vec();
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// `varyHarmony`: the pass's first half, key then progression.
pub fn vary_harmony(p: &mut Project, genre: &GenreDef, spec: &TrackSpec) {
    let mut key_rng = variety_random("key", &genre.name, spec.seed);
    let drawn = KEY_OFFSETS[key_rng.next_int(KEY_OFFSETS.len())];
    // a key-locked walk pins the key it holds (`TrackSpec.pinnedKeyOffset`)
    transpose_track(p, spec.pinned_key_offset().unwrap_or(drawn));
    if p.cohesion.as_deref() != Some("v2") {
        return;
    }
    let mut prog_rng = variety_random("progression", &genre.name, spec.seed);
    let pool = progressions(genre);
    if !pool.is_empty() {
        let i = prog_rng.next_int(pool.len());
        reharmonize(p, &pool[i]);
    }
}

/// `varyFigures`: the second half, the bass figure and the drum detail.
pub fn vary_figures(p: &mut Project, genre: &GenreDef, spec: &TrackSpec) {
    if p.cohesion.as_deref() == Some("v2") {
        let mut bass_rng = variety_random("bass", &genre.name, spec.seed_for_part("bass"));
        vary_bass_figure(p, genre, &mut bass_rng);
    }
    vary_drums(p, genre, spec.seed, &spec.part_seeds);
}

/// `transplantSeededMelody`: a seeded melody plays the melody its own seed composes. The
/// donor is composed here each time (the Swift side keeps a small cache; a host can).
pub fn transplant_seeded_melody(p: &mut Project, spec: &TrackSpec) {
    let Some(&seed) = spec.part_seeds.get("melody") else {
        return;
    };
    if seed == spec.seed {
        return;
    }
    let mut donor_spec = spec.clone();
    donor_spec.seed = seed;
    donor_spec.part_seeds = BTreeMap::new();
    let donor = donor_spec.project();
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || part(nb) != "melody" {
            continue;
        }
        let Some(from) = donor.nets.get(&id) else {
            continue;
        };
        if from.transitions.keys() != nb.transitions.keys() {
            continue;
        }
        nb.bindings = from.bindings.clone();
    }
}

/// `transposeTrack`: every tonal note of the track `offset` semitones away, and the root.
pub fn transpose_track(p: &mut Project, offset: i64) {
    if offset == 0 {
        return;
    }
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" {
            continue;
        }
        for t in nb.bindings.keys().to_vec() {
            let b = nb.bindings.get_mut(&t).unwrap();
            if is_drum_channel(b.channel) {
                continue;
            }
            b.note = (b.note + offset).clamp(0, 127);
        }
    }
    p.root_note = Some(p.root_note.unwrap_or(60) + offset);
}

/// `scaleKey`: the key's notes from MIDI 0 to 127, and its pitch classes from the tonic up.
pub fn scale_key(p: &Project) -> (Vec<i64>, Vec<i64>) {
    let root = p.root_note.unwrap_or(60);
    let name = if p.scale_name.is_empty() {
        "Major"
    } else {
        p.scale_name.as_str()
    };
    let degrees: Vec<i64> = scale_intervals(&format!("{name}Scale"))
        .iter()
        .map(|i| pc(root + i))
        .collect();
    let key: BTreeSet<i64> = degrees.iter().copied().collect();
    (
        (0..=127).filter(|n| key.contains(&pc(*n))).collect(),
        degrees,
    )
}

/// `sevenNoteKey`: `scaleKey`, None unless the key has seven notes.
pub fn seven_note_key(p: &Project) -> Option<(Vec<i64>, Vec<i64>)> {
    let key = scale_key(p);
    if key.1.len() == 7 {
        Some(key)
    } else {
        None
    }
}

/// `reharmonize`: the pad chord of each bar moved along the key so its root is the
/// progression's; the bass with it; then melody and arp notes snapped to the new chords.
pub fn reharmonize(p: &mut Project, roots: &[i64]) {
    let (chords, pad_bars) = pad_chords(p);
    let (scale, degrees) = scale_key(p);
    let size = degrees.len() as i64;
    if pad_bars <= 0 || roots.is_empty() || !progression_key_size(size as usize) {
        return;
    }
    // the scale steps each bar of the pad cycle moves, and the semitones its root moves
    let mut steps = vec![0i64; pad_bars as usize];
    let mut semis = vec![0i64; pad_bars as usize];
    for bar in 0..pad_bars {
        let Some(&root) = chords.get(&bar).and_then(|c| c.first()) else {
            continue;
        };
        let Some(from) = degrees.iter().position(|&d| d == root) else {
            continue;
        };
        let from = from as i64;
        let mut k = ((roots[(bar as usize) % roots.len()] - from) % size + size) % size;
        if k > size / 2 {
            k -= size;
        }
        steps[bar as usize] = k;
        let to = degrees[((from + k + size) % size) as usize];
        let mut s = ((to - root) % 12 + 12) % 12;
        if k < 0 && s > 0 {
            s -= 12;
        }
        semis[bar as usize] = s;
    }
    if !steps.iter().any(|&s| s != 0) {
        return;
    }
    let mv = |n: i64, bar: i64| -> i64 {
        let b = (bar % pad_bars) as usize;
        diatonic(n, steps[b], &scale).unwrap_or((n + semis[b]).clamp(0, 127))
    };
    let in_key: BTreeSet<i64> = degrees.iter().copied().collect();
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" {
            continue;
        }
        let group: String = part(nb).into();
        if group != "harmony" && group != "bass" {
            continue;
        }
        let before = ring_notes(nb);
        for s in &before {
            if let Some(b) = nb.bindings.get_mut(&s.key) {
                b.note = mv(s.note, s.step / 16);
            }
        }
        // A chromatic bass note steps into the next note as it did, a semitone from it on
        // the same side, now that the next note has moved.
        if group == "bass" && before.len() > 1 {
            for (j, s) in before.iter().enumerate() {
                if in_key.contains(&pc(s.note)) {
                    continue;
                }
                let next = &before[(j + 1) % before.len()];
                if !in_key.contains(&pc(next.note)) {
                    continue;
                }
                let Some(moved) = nb.bindings.get(&next.key).map(|b| b.note) else {
                    continue;
                };
                let side = if s.note < next.note { -1 } else { 1 };
                if let Some(b) = nb.bindings.get_mut(&s.key) {
                    b.note = moved + side;
                }
            }
        }
    }
    let (moved, _) = pad_chords(p);
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if !(is_bar_ring(nb) && (part(nb) == "melody" || part(nb) == "arp")) {
            continue;
        }
        let line = ring_notes(nb);
        for (j, s) in line.iter().enumerate() {
            let bar = (s.step / 16) % pad_bars;
            let Some(chord) = moved.get(&bar).filter(|c| !c.is_empty()) else {
                continue;
            };
            if chord.contains(&pc(s.note)) {
                continue;
            }
            let mut around: Vec<i64> = Vec::new();
            if j > 0 {
                around.push(line[j - 1].note);
            }
            if j + 1 < line.len() {
                around.push(line[j + 1].note);
            }
            let old = chords.get(&bar).cloned().unwrap_or_default();
            if s.step % 4 == 0 {
                // a beat note: the nearest chord tone that does not repeat the note before
                // or after, else the nearest
                if let Some(b) = nb.bindings.get_mut(&s.key) {
                    b.note = nearest_tone(s.note, chord, &around);
                }
            } else if rubs(s.note, chord) && !rubs(s.note, &old) {
                // a passing note that now sits a semitone from the new chord steps along the
                // key to the nearest note that does not
                for k in [-1i64, 1, -2, 2] {
                    let Some(n) = diatonic(s.note, k, &scale) else {
                        continue;
                    };
                    if rubs(n, chord) || around.contains(&n) {
                        continue;
                    }
                    if let Some(b) = nb.bindings.get_mut(&s.key) {
                        b.note = n;
                    }
                    break;
                }
            }
        }
    }
}

/// `rubs`: whether `n` is a semitone from a tone of `chord` (pitch classes).
pub fn rubs(n: i64, chord: &[i64]) -> bool {
    chord.iter().any(|&c| pc(c - n) == 1 || pc(n - c) == 1)
}

/// `nearestTone`: the note nearest `n` whose pitch class is in `chord` and that is none of
/// `avoiding` (a tie goes down), within a fifth; else the nearest chord tone.
pub fn nearest_tone(n: i64, chord: &[i64], avoiding: &[i64]) -> i64 {
    let mut nearest: Option<i64> = None;
    for d in 0..=7 {
        for c in [n - d, n + d] {
            if c >= 0 && c <= 127 && chord.contains(&pc(c)) {
                if nearest.is_none() {
                    nearest = Some(c);
                }
                if !avoiding.contains(&c) {
                    return c;
                }
            }
        }
    }
    nearest.unwrap_or(n)
}

/// `rootRingBars`: a bass ring that plays one note per bar, by bar; None when some bar plays
/// two pitches.
fn root_ring_bars(nb: &NetBundle) -> Option<Vec<Vec<StepNote>>> {
    if !is_bar_ring(nb) {
        return None;
    }
    let mut bars: Vec<Vec<StepNote>> = vec![Vec::new(); nb.transitions.len() / 16];
    for s in ring_notes(nb) {
        let i = (s.step / 16) as usize;
        bars[i].push(s);
    }
    for bar in &bars {
        let set: BTreeSet<i64> = bar.iter().map(|s| s.note).collect();
        if set.len() > 1 {
            return None;
        }
    }
    if bars.iter().any(|b| !b.is_empty()) {
        Some(bars)
    } else {
        None
    }
}

/// `varyBassFigure`: a seeded figure for a root-per-bar bass.
pub fn vary_bass_figure(p: &mut Project, genre: &GenreDef, rng: &mut Mulberry32) {
    // Groove.fourOnFloor = 0, walking = 5, bossa = 6
    let groove = BeatsTables::shared()
        .theme_table(&genre.name)
        .map_or(0, |t| t.groove);
    if groove == 5 || groove == 6 {
        return;
    }
    let rhythm = rng.next_int(3);
    let line = rng.next_int(4);
    if rhythm == 0 && line == 0 {
        return;
    }
    let kick = kick_hit_mask(genre);
    let pickup: i64 = if bass_locked_to_four_on_the_floor(genre) {
        15
    } else {
        14
    };
    let key = seven_note_key(p);
    let pcs = key_pitch_classes(p);
    // the bass's room: up to A2 or its highest note, and under the pad
    let mut ceiling = notes(p, "bass")
        .into_iter()
        .max()
        .unwrap_or(BASS_CEILING)
        .max(BASS_CEILING);
    if let Some(pad) = notes(p, "harmony").into_iter().min() {
        ceiling = ceiling.min(pad - 1);
    }
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || part(nb) != "bass" {
            continue;
        }
        let Some(bars) = root_ring_bars(nb) else {
            continue;
        };
        for (bi, bar) in bars.iter().enumerate() {
            if bar.is_empty() {
                continue;
            }
            let bi = bi as i64;
            let root = bar[0].note;
            let mut hits: Vec<StepNote> = bar.clone();
            let pickup_key = format!("t{}", bi * 16 + pickup);
            if rhythm == 1
                && bi % 2 == 1
                && hits.last().unwrap().step % 16 < pickup
                && !(kick.len() == 16 && kick[pickup as usize])
                && nb.bindings.contains_key(&bar[0].key)
                && nb.transitions.contains_key(&pickup_key)
            {
                let template = *nb.bindings.get(&bar[0].key).unwrap();
                nb.bindings.insert(pickup_key.clone(), template);
                hits.push(StepNote {
                    key: pickup_key,
                    step: bi * 16 + pickup,
                    note: root,
                });
            } else if rhythm == 2 && hits.len() >= 6 {
                for (j, h) in hits.iter().enumerate() {
                    if j % 2 == 1 {
                        nb.bindings.remove(&h.key);
                    }
                }
                hits = hits
                    .into_iter()
                    .enumerate()
                    .filter(|(o, _)| o % 2 == 0)
                    .map(|(_, h)| h)
                    .collect();
            }
            // the line: the bar's first and last notes stay the root as composed; a figure
            // note goes up where it fits under `ceiling`, else down where it stays above
            // E1, else stays the root
            let count = hits.len();
            for (j, h) in hits.iter().enumerate() {
                if !(j > 0 && j + 1 < count) {
                    continue;
                }
                let mut note = root;
                if (line == 1 && j % 2 == 1) || (line == 2 && h.step % 16 >= 8) {
                    // the chord's fifth along the key
                    let up = key
                        .as_ref()
                        .and_then(|k| diatonic(root, 4, &k.0))
                        .unwrap_or(root + 7);
                    let down = key
                        .as_ref()
                        .and_then(|k| diatonic(root, -3, &k.0))
                        .unwrap_or(root - 5);
                    if pcs.contains(&pc(up)) && up <= ceiling {
                        note = up;
                    } else if pcs.contains(&pc(down)) && down >= BASS_FLOOR {
                        note = down;
                    }
                } else if line == 3 && j == count / 2 {
                    // one octave jump in the middle of the bar
                    if root + 12 <= ceiling {
                        note = root + 12;
                    } else if root - 12 >= BASS_FLOOR {
                        note = root - 12;
                    }
                }
                if let Some(b) = nb.bindings.get_mut(&h.key) {
                    b.note = note.clamp(0, 127);
                }
            }
        }
    }
}

/// `drumRole`: the drum part a net plays (`kick`, `snare`, `hihat`), else None.
pub fn drum_role(nb: &NetBundle, id: &str) -> Option<&'static str> {
    if nb.role == "control"
        || nb.bindings.keys().is_empty()
        || !nb.bindings.values().all(|b| is_drum_channel(b.channel))
        || nb.places.len() != nb.transitions.len()
    {
        return None;
    }
    let role = if nb.riff_group.is_empty() {
        id
    } else {
        nb.riff_group.as_str()
    };
    ["kick", "snare", "hihat"].into_iter().find(|r| *r == role)
}

/// `varyDrums`: seeded detail on the genre's drum patterns, the same choice in every slot
/// of a part. A part in `part_seeds` (`kick`, `snare`, `hihat`) draws from its own seed.
pub fn vary_drums(
    p: &mut Project,
    genre: &GenreDef,
    seed: f64,
    part_seeds: &BTreeMap<String, f64>,
) {
    let hats = variety_random(
        "hihat",
        &genre.name,
        part_seeds.get("hihat").copied().unwrap_or(seed),
    );
    let snares = variety_random(
        "snare",
        &genre.name,
        part_seeds.get("snare").copied().unwrap_or(seed),
    );
    let kicks = variety_random(
        "kick",
        &genre.name,
        part_seeds.get("kick").copied().unwrap_or(seed),
    );
    let steady = four_on_the_floor(genre);
    for id in p.nets.keys().to_vec() {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        let Some(role) = drum_role(nb, &id) else {
            continue;
        };
        let n = nb.transitions.len();
        if n < 8 {
            continue;
        }
        let Some(template) = nb.bindings.values().next().copied() else {
            continue;
        };
        let mut hit = vec![false; n];
        for t in nb.bindings.keys() {
            if let Some(i) = t_index(t) {
                if t.starts_with('t') && (i as usize) < n {
                    hit[i as usize] = true;
                }
            }
        }
        let free = |hit: &[bool], nb: &NetBundle, i: usize| -> bool {
            i > 0
                && i < n
                && !hit[i]
                && !hit[(i + 1) % n]
                && !hit[(i + n - 1) % n]
                && nb.transitions.contains_key(&format!("t{i}"))
        };
        let ghost = |hit: &mut Vec<bool>, nb: &mut NetBundle, i: usize, share: f64| {
            let mut b = template;
            b.velocity = ((template.velocity as f64 * share) as i64).max(1);
            nb.bindings.insert(format!("t{i}"), b);
            hit[i] = true;
        };
        match role {
            "hihat" => {
                let mut rng = hats.clone();
                match rng.next_int(4) {
                    1 => {
                        let cand: Vec<usize> = (0..n).filter(|&i| i % 2 == 1 && !hit[i]).collect();
                        for i in shuffle(&mut rng, &cand).into_iter().take(n / 8) {
                            ghost(&mut hit, nb, i, 0.45);
                        }
                    }
                    2 => {
                        let odd: Vec<usize> = (0..n).filter(|&i| i % 2 == 1 && hit[i]).collect();
                        if odd.len() < hit.iter().filter(|&&h| h).count() {
                            for i in odd {
                                nb.bindings.remove(&format!("t{i}"));
                            }
                        }
                    }
                    3 => {
                        let mut moved: OrderedMap<MidiBinding> = OrderedMap::new();
                        for (t, b) in nb.bindings.iter() {
                            let Some(i) = t_index(t) else { continue };
                            if !t.starts_with('t') {
                                continue;
                            }
                            moved.insert(format!("t{}", (i as usize + 2) % n), *b);
                        }
                        if moved.keys().iter().all(|k| nb.transitions.contains_key(k)) {
                            nb.bindings = moved;
                        }
                    }
                    _ => {}
                }
            }
            "snare" => {
                let mut rng = snares.clone();
                let count = rng.next_int(3);
                let cand: Vec<usize> = (0..n).filter(|&i| free(&hit, nb, i)).collect();
                let spots = shuffle(&mut rng, &cand);
                for i in spots.into_iter().take(count) {
                    ghost(&mut hit, nb, i, 0.35);
                }
            }
            "kick" => {
                if steady {
                    continue;
                }
                let mut rng = kicks.clone();
                if rng.next_int(2) == 1 {
                    let cand: Vec<usize> = (0..n)
                        .filter(|&i| i % 2 == 0 && free(&hit, nb, i))
                        .collect();
                    if let Some(&i) = shuffle(&mut rng, &cand).first() {
                        ghost(&mut hit, nb, i, 0.6);
                    }
                }
            }
            _ => {}
        }
    }
}
