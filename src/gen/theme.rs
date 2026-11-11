//! Port of `Generator/Theme.swift` (generator/theme.js, cohesion v2, and groove.js).

use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::{clamp_velocity, obj, GenParams};
use crate::gen::euclidean::{bjorklund, gen_track};
use crate::gen::tables::{BeatsTables, ChordDegree, GenreDef, SectionProfile};
use crate::json::{floor, js_to_int32, JsonValue};
use crate::pflow::{NetBundle, OrderedMap};
use crate::rng::Mulberry32;

pub mod motif_mode {
    pub const IGNORE: i64 = 0;
    pub const PLAY: i64 = 1;
    pub const TRANSPOSED: i64 = 2;
    pub const FRAGMENT: i64 = 3;
    pub const AUGMENTED: i64 = 4;
    pub const INVERTED: i64 = 5;
    pub const LAYERED: i64 = 6;
}

pub mod groove {
    pub const FOUR_ON_FLOOR: i64 = 0;
    pub const OFFBEAT: i64 = 1;
    pub const SIDECHAINED: i64 = 2;
    pub const SYNCO_POCKET: i64 = 3;
    pub const BREAKBEAT: i64 = 4;
    pub const WALKING: i64 = 5;
    pub const BOSSA: i64 = 6;
}

#[derive(Clone, Debug)]
pub struct ChordPlan {
    pub steps_per_chord: i64,
    pub chords: Vec<ChordDegree>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MotifCell {
    pub degrees: Vec<i64>,
    pub mask: Vec<bool>,
    pub contour: i64,
}

#[derive(Clone, Debug)]
pub struct TrackTheme {
    pub motif: MotifCell,
    pub plan: ChordPlan,
    /// Harmonic offset per section name.
    pub harmonic: Vec<(&'static str, i64)>,
    pub groove: i64,
    pub energy: OrderedMap<SectionProfile>,
}

impl TrackTheme {
    pub fn harmonic_offset(&self, section: &str) -> i64 {
        self.harmonic
            .iter()
            .find(|(k, _)| *k == section)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    }
}

pub fn chord_at(plan: &ChordPlan, step: i64) -> ChordDegree {
    if plan.chords.is_empty() {
        return ChordDegree {
            root: 0,
            tones: alloc::vec![0, 2, 4],
        };
    }
    let idx = (step / plan.steps_per_chord) as usize % plan.chords.len();
    plan.chords[idx].clone()
}

pub fn cycle_steps(plan: &ChordPlan) -> i64 {
    plan.chords.len() as i64 * plan.steps_per_chord
}

pub fn clamp_degree(d: i64) -> i64 {
    let mut d = d;
    while d < 0 {
        d += 7;
    }
    while d >= 7 {
        d -= 7;
    }
    d
}

/// FNV-32 over UTF-16 code units (`hashStr`).
pub fn hash_str(s: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for u in s.encode_utf16() {
        h = (h ^ u as u32).wrapping_mul(16_777_619);
    }
    h
}

pub fn cohesion_genre_supported(genre: &str) -> bool {
    BeatsTables::shared()
        .theme_table(genre)
        .is_some_and(|t| t.supported)
}

/// `buildTrackTheme(genreName, seed)`: a pure function of its inputs.
pub fn build_track_theme(genre: &str, seed: f64) -> TrackTheme {
    let tables = BeatsTables::shared();
    let u = js_to_int32(seed) as u32;
    let mut prefixed = String::from("theme/");
    prefixed.push_str(genre);
    let mut rng = Mulberry32::from_i32((u ^ hash_str(&prefixed)) as i32);

    let mut plan = ChordPlan {
        steps_per_chord: 16,
        chords: Vec::new(),
    };
    match tables
        .genres
        .get(genre)
        .and_then(|g| g.theory.as_ref())
        .map(|t| &t.chord_progs)
    {
        Some(progs) if !progs.is_empty() => {
            plan.chords = progs[rng.next_int(progs.len())].chords.clone();
        }
        _ => {
            let _ = rng.next_int(1);
            plan.chords = alloc::vec![
                ChordDegree {
                    root: 0,
                    tones: alloc::vec![0, 2, 4]
                },
                ChordDegree {
                    root: 5,
                    tones: alloc::vec![5, 0, 2]
                },
                ChordDegree {
                    root: 3,
                    tones: alloc::vec![3, 5, 0]
                },
                ChordDegree {
                    root: 6,
                    tones: alloc::vec![6, 1, 3]
                },
            ];
        }
    }
    let motif = generate_motif(&mut rng, 7, &plan);
    let harmonic = alloc::vec![
        ("intro", 0),
        ("verse", 0),
        ("buildup", 0),
        ("drop", 0),
        ("breakdown", 0),
        ("bridge", 5),
        ("chorus", 0),
        ("outro", 0),
        ("solo", 4),
    ];
    let table = tables.theme_table(genre);
    TrackTheme {
        motif,
        plan,
        harmonic,
        groove: table.map(|t| t.groove).unwrap_or(0),
        energy: table.map(|t| t.energy.clone()).unwrap_or_default(),
    }
}

fn generate_motif(rng: &mut Mulberry32, scale_len: i64, plan: &ChordPlan) -> MotifCell {
    const BARS: usize = 4;
    const BAR_STEPS: usize = 16;
    let motif_len = BARS * BAR_STEPS;
    let mut degrees = alloc::vec![0i64; motif_len];
    let mut mask = alloc::vec![false; motif_len];

    for b in 0..BARS {
        let hits = 6 + rng.next_int(3);
        let base = b * BAR_STEPS;
        let mut have = 0;
        let mut i = 0;
        while i < BAR_STEPS {
            mask[base + i] = true;
            have += 1;
            i += 4;
        }
        while have < hits {
            let j = rng.next_int(BAR_STEPS);
            if !mask[base + j] {
                mask[base + j] = true;
                have += 1;
            }
        }
    }

    let mut cur: i64 = 0;
    for i in 0..motif_len {
        if !mask[i] {
            degrees[i] = -1;
            continue;
        }
        let bar = (i / BAR_STEPS) as i64;
        let chord = chord_at(plan, bar * plan.steps_per_chord);
        if i % 4 == 0 {
            cur = nearest_tone_of(cur, scale_len, &chord.tones);
        } else {
            let delta = rng.next_int(5) as i64 - 2;
            cur = (cur + delta).max(0).min(scale_len - 1);
        }
        degrees[i] = cur;
    }

    if let Some(li) = last_active_in(&degrees, BAR_STEPS, 2 * BAR_STEPS) {
        let c = chord_at(plan, plan.steps_per_chord);
        if c.tones.len() > 1 {
            degrees[li] = clamp_degree(c.tones[1]);
        }
    }
    if let Some(li) = last_active_in(&degrees, 3 * BAR_STEPS, 4 * BAR_STEPS) {
        degrees[li] = 0;
    }

    let active: Vec<i64> = degrees.iter().copied().filter(|d| *d >= 0).collect();
    let contour = match (active.first(), active.last()) {
        (Some(f), Some(l)) if l > f => 1,
        (Some(f), Some(l)) if l < f => -1,
        _ => 0,
    };
    MotifCell {
        degrees,
        mask,
        contour,
    }
}

fn nearest_tone_of(cur: i64, scale_len: i64, tones: &[i64]) -> i64 {
    if tones.contains(&cur) {
        return cur;
    }
    let mut dist = 1;
    while dist < scale_len {
        if let Some(&t) = tones.iter().find(|&&t| t == cur + dist && t < scale_len) {
            return t;
        }
        if let Some(&t) = tones.iter().find(|&&t| t == cur - dist && t >= 0) {
            return t;
        }
        dist += 1;
    }
    tones.first().map(|&t| clamp_degree(t)).unwrap_or(0)
}

fn last_active_in(degrees: &[i64], from: usize, to: usize) -> Option<usize> {
    (from..to).rev().find(|&i| degrees[i] >= 0)
}

/// `renderMotif(cell, mode, harmonicOffset)`
pub fn render_motif(cell: &MotifCell, mode: i64, harmonic_offset: i64) -> MotifCell {
    use motif_mode::*;
    match mode {
        IGNORE => MotifCell {
            degrees: Vec::new(),
            mask: Vec::new(),
            contour: 0,
        },
        PLAY | LAYERED | TRANSPOSED => {
            let mut out = cell.clone();
            if mode == TRANSPOSED || harmonic_offset != 0 {
                for d in out.degrees.iter_mut().filter(|d| **d >= 0) {
                    *d = clamp_degree(*d + harmonic_offset);
                }
            }
            out
        }
        FRAGMENT => {
            let mut out = cell.clone();
            let half = out.degrees.len() / 2;
            for i in half..out.degrees.len() {
                out.degrees[i] = -1;
                out.mask[i] = false;
            }
            out
        }
        AUGMENTED => {
            let dl = cell.degrees.len() * 2;
            let mut out = MotifCell {
                degrees: alloc::vec![0; dl],
                mask: alloc::vec![false; dl],
                contour: cell.contour,
            };
            for i in 0..cell.degrees.len() {
                out.degrees[i * 2] = cell.degrees[i];
                out.mask[i * 2] = cell.mask[i];
                out.degrees[i * 2 + 1] = -1;
                out.mask[i * 2 + 1] = false;
            }
            out
        }
        INVERTED => {
            let mut out = cell.clone();
            for d in out.degrees.iter_mut().filter(|d| **d >= 0) {
                *d = clamp_degree((7 - 1) - *d + harmonic_offset);
            }
            out.contour = -cell.contour;
            out
        }
        _ => cell.clone(),
    }
}

/// `degreeToMidi`
pub fn degree_to_midi(degree: i64, scale: &[i64]) -> i64 {
    if scale.is_empty() {
        return 60;
    }
    if degree < 0 {
        return 0;
    }
    let n = scale.len() as i64;
    if degree < n {
        return scale[degree as usize];
    }
    scale[(degree % n) as usize] + 12 * (degree / n)
}

// ---------------------------------------------------------------------------
// Net builders

fn finish_track(nb: &mut NetBundle, p: &GenParams, generator: &str, ring_size: i64, beats: i64) {
    nb.track = gen_track(
        p,
        generator,
        ring_size,
        beats,
        None,
        None,
        obj(alloc::vec![
            ("duration", Some(JsonValue::int(p.duration))),
            ("seed", Some(JsonValue::Number(p.seed))),
        ]),
    );
}

pub fn motif_net(cell: &MotifCell, scale: &[i64], p: &GenParams) -> NetBundle {
    let (degrees, mask) = if cell.degrees.is_empty() {
        (alloc::vec![-1i64; 16], alloc::vec![false; 16])
    } else {
        (cell.degrees.clone(), cell.mask.clone())
    };
    let n = degrees.len() as i64;
    let mut nb = NetBundle::ring(n);
    for i in 0..n as usize {
        if mask[i] && degrees[i] >= 0 {
            nb.bind(
                i as i64,
                degree_to_midi(degrees[i], scale),
                p.channel,
                clamp_velocity(p.velocity),
                p.duration,
            );
        }
    }
    let beats = mask.iter().filter(|m| **m).count() as i64;
    finish_track(&mut nb, p, "motif", n, beats);
    nb
}

pub fn noted_ring(notes: &[i64], mask: &[bool], p: &GenParams) -> NetBundle {
    noted_ring_dur(notes, mask, None, p)
}

fn noted_ring_dur(notes: &[i64], mask: &[bool], durs: Option<&[i64]>, p: &GenParams) -> NetBundle {
    let (m, nt): (Vec<bool>, Vec<i64>) = if mask.is_empty() {
        (alloc::vec![false; 16], alloc::vec![0; 16])
    } else {
        (mask.to_vec(), notes.to_vec())
    };
    let n = m.len();
    let mut nb = NetBundle::ring(n as i64);
    for i in 0..n {
        if m[i] && i < nt.len() {
            let mut dur = p.duration;
            if let Some(d) = durs.and_then(|d| d.get(i)) {
                if *d > 0 {
                    dur = *d;
                }
            }
            nb.bind(i as i64, nt[i], p.channel, clamp_velocity(p.velocity), dur);
        }
    }
    let beats = m.iter().filter(|x| **x).count() as i64;
    finish_track(&mut nb, p, "noted-ring", n as i64, beats);
    nb
}

pub fn chord_pad_net(plan: &ChordPlan, scale: &[i64], p: &GenParams) -> NetBundle {
    let mut n = cycle_steps(plan);
    if n == 0 {
        n = 64;
    }
    let mut nb = NetBundle::ring(n);
    for (c, chord) in plan.chords.iter().enumerate() {
        let base = c as i64 * plan.steps_per_chord;
        for (v, &tone) in chord.tones.iter().take(3).enumerate() {
            let step = base + v as i64;
            if step >= n {
                break;
            }
            nb.bind(
                step,
                degree_to_midi(clamp_degree(tone), scale),
                p.channel,
                clamp_velocity(p.velocity),
                p.duration,
            );
        }
    }
    finish_track(&mut nb, p, "chord-pad", n, plan.chords.len() as i64 * 3);
    nb
}

pub fn chord_bass_ring(
    plan: &ChordPlan,
    bar_mask: &[bool],
    scale: &[i64],
    p: &GenParams,
    register_shift: i64,
) -> NetBundle {
    let bm: Vec<bool> = if bar_mask.is_empty() {
        alloc::vec![false; 16]
    } else {
        bar_mask.to_vec()
    };
    let bar_steps = bm.len();
    let bars = if plan.chords.is_empty() {
        4
    } else {
        plan.chords.len()
    };
    let n = bars * bar_steps;
    let mut mask = alloc::vec![false; n];
    let mut notes = alloc::vec![0i64; n];
    for b in 0..bars {
        let chord = chord_at(plan, b as i64 * plan.steps_per_chord);
        let root = degree_to_midi(clamp_degree(chord.root), scale) + register_shift;
        for i in 0..bar_steps {
            mask[b * bar_steps + i] = bm[i];
            notes[b * bar_steps + i] = root;
        }
    }
    noted_ring_dur(&notes, &mask, None, p)
}

pub fn chord_walking_bass_ring(
    plan: &ChordPlan,
    scale: &[i64],
    p: &GenParams,
    register_shift: i64,
) -> NetBundle {
    let bar_steps = if plan.steps_per_chord <= 0 {
        16
    } else {
        plan.steps_per_chord
    };
    let bars = if plan.chords.is_empty() {
        4
    } else {
        plan.chords.len() as i64
    };
    let beats = 4;
    let stride = (bar_steps / beats).max(1);
    let n = (bars * bar_steps) as usize;
    let mut mask = alloc::vec![false; n];
    let mut notes = alloc::vec![0i64; n];
    for b in 0..bars {
        let chord = chord_at(plan, b * plan.steps_per_chord);
        let next = chord_at(plan, ((b + 1) % bars) * plan.steps_per_chord);
        for beat in 0..beats {
            let step = beat * stride;
            if step >= bar_steps {
                break;
            }
            let idx = (b * bar_steps + step) as usize;
            mask[idx] = true;
            notes[idx] = walking_bass_note(beat, &chord, &next, scale, register_shift);
        }
    }
    noted_ring_dur(&notes, &mask, None, p)
}

fn walking_bass_note(
    beat: i64,
    chord: &ChordDegree,
    next: &ChordDegree,
    scale: &[i64],
    shift: i64,
) -> i64 {
    let root = degree_to_midi(clamp_degree(chord.root), scale) + shift;
    match beat {
        1 if chord.tones.len() > 1 => degree_to_midi(clamp_degree(chord.tones[1]), scale) + shift,
        2 if chord.tones.len() > 2 => degree_to_midi(clamp_degree(chord.tones[2]), scale) + shift,
        3 => {
            let next_root = degree_to_midi(clamp_degree(next.root), scale) + shift;
            if next_root < root {
                next_root + 1
            } else {
                next_root - 1
            }
        }
        _ => root,
    }
}

pub fn chord_bossa_bass_ring(
    plan: &ChordPlan,
    scale: &[i64],
    p: &GenParams,
    register_shift: i64,
    bpm: f64,
) -> NetBundle {
    let bar_steps = if plan.steps_per_chord <= 0 {
        16
    } else {
        plan.steps_per_chord
    };
    let bars = if plan.chords.is_empty() {
        4
    } else {
        plan.chords.len() as i64
    };
    let mut fifth_step = bar_steps * 3 / 8;
    if fifth_step <= 0 || fifth_step >= bar_steps {
        fifth_step = bar_steps / 2;
    }
    let mut bar_ms = 2000;
    if bpm > 0.0 {
        bar_ms = floor(4.0 * 60000.0 / bpm * 0.95) as i64;
    }
    let root_dur = bar_ms * 3 / 8;
    let fifth_dur = bar_ms / 4;
    let n = (bars * bar_steps) as usize;
    let mut mask = alloc::vec![false; n];
    let mut notes = alloc::vec![0i64; n];
    let mut durs = alloc::vec![0i64; n];
    for b in 0..bars {
        let chord = chord_at(plan, b * plan.steps_per_chord);
        let root = degree_to_midi(clamp_degree(chord.root), scale) + register_shift;
        let mut fifth = root;
        if chord.tones.len() > 2 {
            fifth = degree_to_midi(clamp_degree(chord.tones[2]), scale) + register_shift;
        }
        while fifth >= root {
            fifth -= 12;
        }
        let base = (b * bar_steps) as usize;
        mask[base] = true;
        notes[base] = root;
        durs[base] = root_dur;
        let f = base + fifth_step as usize;
        mask[f] = true;
        notes[f] = fifth;
        durs[f] = fifth_dur;
    }
    noted_ring_dur(&notes, &mask, Some(&durs), p)
}

/// `kickHitMask(genre)`
pub fn kick_hit_mask(genre: &GenreDef) -> Vec<bool> {
    let k = genre.kick.first().copied().unwrap_or(4);
    let n = genre.kick.get(1).copied().unwrap_or(16);
    let rot = genre.kick.get(2).copied().unwrap_or(0);
    let pattern = bjorklund(k, n);
    let r = ((rot % n) + n) % n;
    (0..n)
        .map(|i| pattern[((i + r) % n) as usize] == 1)
        .collect()
}

/// `grooveLock(kickMask, template, rng)`. composer.js passes its rng *object* where
/// grooveLock tests `typeof rng === 'function'`, so the breakbeat rotation is always
/// 0 and no RNG draw happens; that behaviour is the contract.
pub fn groove_lock(kick_mask: &[bool], template: i64) -> Vec<bool> {
    use groove::*;
    let n = kick_mask.len();
    let ni = n as i64;
    let mut out = alloc::vec![false; n];
    match template {
        OFFBEAT => {
            for i in 0..ni {
                if kick_mask[i as usize] {
                    out[(((i - 1) % ni + ni) % ni) as usize] = true;
                }
            }
        }
        SIDECHAINED => {
            for i in 0..n {
                out[i] = !kick_mask[i];
            }
        }
        SYNCO_POCKET => {
            out = kick_mask.to_vec();
            let (off1, off2) = if n == 8 { (3, 7) } else { (6, 14) };
            if off1 < n {
                out[off1] = true;
            }
            if off2 < n {
                out[off2] = true;
            }
        }
        BREAKBEAT => {
            let hits = (n * 3 / 8).max(1);
            for i in 0..hits {
                out[(i * n / hits) % n] = true;
            }
        }
        _ => out = kick_mask.to_vec(), // FOUR_ON_FLOOR and unknown
    }
    out
}

// ---------------------------------------------------------------------------
// Native genres' bass figures (no beats counterpart: genres-native.json)

/// Italo / synth-pop's octave-bouncing bass: eighths alternating the chord root and its
/// octave, a bar per chord.
pub fn octave_bounce_bass_ring(plan: &ChordPlan, scale: &[i64], p: &GenParams) -> NetBundle {
    let bars = plan.chords.len().max(1);
    let n = bars * 16;
    let mut nb = NetBundle::ring(n as i64);
    for b in 0..bars {
        let chord = chord_at(plan, b as i64 * plan.steps_per_chord);
        let root = degree_to_midi(clamp_degree(chord.root), scale);
        for e in 0..8 {
            let note = if e % 2 == 0 { root } else { root + 12 };
            let vel = if e % 2 == 0 { p.velocity } else { p.velocity - 12 };
            nb.bind(
                (b * 16 + e * 2) as i64,
                note,
                p.channel,
                clamp_velocity(vel),
                p.duration,
            );
        }
    }
    finish_track(&mut nb, p, "octave-bass", n as i64, (bars * 8) as i64);
    nb
}

/// Acid house's 303-style line: a seeded one-bar sixteenth pattern (root-heavy, with the
/// octave, fifth, minor third and flat seventh), accents louder and slides held into the
/// next step, transposed to each bar's chord root. Its own RNG (`p.seed`), so the
/// composer's draws are untouched.
pub fn acid_bass_ring(plan: &ChordPlan, scale: &[i64], p: &GenParams) -> NetBundle {
    let mut rng = Mulberry32::new(p.seed);
    // (step on, semitones over the root, accent, slide)
    let mut pattern: Vec<(bool, i64, bool, bool)> = Vec::with_capacity(16);
    let intervals = [0, 0, 0, 12, 12, 7, 3, 10, -2];
    for i in 0..16 {
        let on = i == 0 || rng.next() < p.density;
        let semis = intervals[rng.next_int(intervals.len())];
        let accent = rng.next() < 0.3;
        let slide = rng.next() < 0.2;
        pattern.push((on, semis, accent, slide));
    }
    let bars = plan.chords.len().max(1);
    let n = bars * 16;
    let mut nb = NetBundle::ring(n as i64);
    let mut beats = 0;
    for b in 0..bars {
        let chord = chord_at(plan, b as i64 * plan.steps_per_chord);
        let root = degree_to_midi(clamp_degree(chord.root), scale);
        for (i, &(on, semis, accent, slide)) in pattern.iter().enumerate() {
            if !on {
                continue;
            }
            let vel = if accent { p.velocity + 25 } else { p.velocity - 10 };
            let dur = if slide { p.duration * 9 / 4 } else { p.duration };
            nb.bind(
                (b * 16 + i) as i64,
                root + semis,
                p.channel,
                clamp_velocity(vel),
                dur,
            );
            beats += 1;
        }
    }
    finish_track(&mut nb, p, "acid-bass", n as i64, beats);
    nb
}
