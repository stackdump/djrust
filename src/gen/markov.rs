//! Port of `Generator/Markov.swift` (generator/markov.js).

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::gen::core::{clamp_velocity, json_ints, obj, GenParams, MAJOR_INTERVALS};
use crate::gen::euclidean::gen_track;
use crate::gen::tables::ChordProg;
use crate::json::{floor, JsonValue};
use crate::pflow::NetBundle;
use crate::rng::{Mulberry32, RandomSource};

pub type ToneSet = BTreeSet<i64>;

/// `composeSequence`: scale-degree indices, -1 for rests.
pub fn compose_sequence<R: RandomSource>(
    steps: i64,
    scale_len: i64,
    chord_tones: &ToneSet,
    is_bass: bool,
    density: f64,
    rng: &mut R,
) -> Vec<i64> {
    let mut seq = alloc::vec![0i64; steps as usize];
    let mut current = 0;
    seq[0] = current;
    for i in 1..steps.max(1) {
        let beat_pos = i % 4;
        current = if is_bass {
            compose_bass_step(current, beat_pos, scale_len, rng)
        } else {
            compose_melody_step(current, beat_pos, scale_len, chord_tones, rng)
        };
        seq[i as usize] = current;
    }

    let mut note_count = floor(density * steps as f64) as i64;
    if note_count < 1 {
        note_count = 1;
    }
    if note_count >= steps {
        return seq;
    }
    let rests_needed = steps - note_count;
    let mut candidates: Vec<(i64, i64)> = (0..steps)
        .map(|i| {
            let s = if i == 0 {
                200
            } else {
                match i % 4 {
                    0 => 100,
                    2 => 50,
                    _ => 0,
                }
            };
            (i, s)
        })
        .collect();
    // The JS insertion-sorts ascending by score: a stable sort.
    candidates.sort_by_key(|c| c.1);
    for &(idx, _) in candidates.iter().take(rests_needed as usize) {
        if idx != 0 {
            seq[idx as usize] = -1;
        }
    }
    seq
}

pub fn compose_melody_step<R: RandomSource>(
    current: i64,
    beat_pos: i64,
    scale_len: i64,
    chord_tones: &ToneSet,
    rng: &mut R,
) -> i64 {
    match beat_pos {
        0 => nearest_chord_tone(current, scale_len, chord_tones),
        2 => {
            if rng.float64() < 0.7 {
                nearest_chord_tone(current, scale_len, chord_tones)
            } else {
                stepwise(current, scale_len, 1, rng)
            }
        }
        _ => {
            let r = rng.float64();
            if r < 0.60 {
                stepwise(current, scale_len, 1, rng)
            } else if r < 0.85 {
                stepwise(current, scale_len, 2, rng)
            } else {
                current
            }
        }
    }
}

fn compose_bass_step<R: RandomSource>(
    current: i64,
    beat_pos: i64,
    scale_len: i64,
    rng: &mut R,
) -> i64 {
    match beat_pos {
        0 => 0,
        2 => {
            if scale_len > 4 && rng.float64() < 0.6 {
                4
            } else {
                0
            }
        }
        _ => {
            if rng.float64() < 0.5 {
                current
            } else {
                stepwise(current, scale_len, 1, rng)
            }
        }
    }
}

fn nearest_chord_tone(current: i64, scale_len: i64, chord_tones: &ToneSet) -> i64 {
    if chord_tones.contains(&current) {
        return current;
    }
    let mut dist = 1;
    while dist < scale_len {
        let up = current + dist;
        let down = current - dist;
        if up < scale_len && chord_tones.contains(&up) {
            return up;
        }
        if down >= 0 && chord_tones.contains(&down) {
            return down;
        }
        dist += 1;
    }
    0
}

fn stepwise<R: RandomSource>(current: i64, scale_len: i64, interval: i64, rng: &mut R) -> i64 {
    if rng.float64() < 0.5 {
        let mut next = current + interval;
        if next >= scale_len {
            next = current - interval;
        }
        if next < 0 {
            next = 0;
        }
        return next;
    }
    let mut next = current - interval;
    if next < 0 {
        next = current + interval;
    }
    if next >= scale_len {
        next = scale_len - 1;
    }
    next
}

pub fn compute_velocity(step: i64, deg: i64, base_vel: i64, chord_tones: &ToneSet) -> i64 {
    let mut vel = base_vel;
    match step % 4 {
        0 => vel += 15,
        2 => vel += 5,
        _ => vel -= 10,
    }
    if chord_tones.contains(&deg) {
        vel += 5;
    }
    vel
}

pub fn compute_duration<R: RandomSource>(
    deg: i64,
    base_dur: i64,
    variation: f64,
    chord_tones: &ToneSet,
    rng: &mut R,
) -> i64 {
    if variation <= 0.0 {
        return base_dur;
    }
    let mut dur = base_dur as f64;
    if chord_tones.contains(&deg) {
        dur = floor(dur * (1.2 + rng.float64() * 0.8 * variation));
    } else {
        dur = floor(dur * (0.3 + rng.float64() * 0.5 * variation));
    }
    if dur < 20.0 {
        dur = 20.0;
    }
    dur as i64
}

fn apply_syncopation<R: RandomSource>(seq: &mut [i64], probability: f64, rng: &mut R) {
    let mut i = 2;
    while i < seq.len() {
        if i % 2 == 0 && seq[i] >= 0 && seq[i - 1] < 0 && rng.float64() < probability {
            seq[i - 1] = seq[i];
            seq[i] = -1;
        }
        i += 1;
    }
}

/// Chord tones (scale-degree indices below `n`), defaulting to root/3rd/5th.
pub fn chord_tone_set(chords: Option<&ChordProg>, n: i64) -> ToneSet {
    let mut set = ToneSet::new();
    for chord in chords.map(|c| c.chords.as_slice()).unwrap_or(&[]) {
        for &t in &chord.tones {
            if t < n {
                set.insert(t);
            }
        }
    }
    if set.is_empty() {
        for deg in [0, 2, 4] {
            if deg < n {
                set.insert(deg);
            }
        }
    }
    set
}

/// The generator-params object markov / walking / call-response nets carry.
pub(crate) fn melodic_params_json(p: &GenParams, notes: &[i64], syncopation: bool) -> JsonValue {
    let mut pairs = alloc::vec![
        ("scale", Some(json_ints(notes))),
        ("rootNote", Some(JsonValue::int(p.root_note))),
        ("chords", p.chords.as_ref().map(ChordProg::json)),
        ("seed", Some(JsonValue::Number(p.seed))),
        ("duration", Some(JsonValue::int(p.duration))),
        ("density", Some(JsonValue::Number(p.density))),
        (
            "durationVariation",
            Some(JsonValue::Number(p.duration_variation))
        ),
    ];
    if syncopation {
        pairs.push(("syncopation", Some(JsonValue::Number(p.syncopation))));
    }
    obj(pairs)
}

/// `markovMelody(params)`
pub fn markov_melody(p: &GenParams) -> NetBundle {
    let mut notes = p.scale.clone();
    if notes.is_empty() {
        for oct in 0..2 {
            for interval in MAJOR_INTERVALS {
                let n = p.root_note + oct * 12 + interval;
                if n <= 127 {
                    notes.push(n);
                }
            }
        }
    }
    let mut n = notes.len() as i64;
    if n > 12 {
        n = 12;
        notes.truncate(12);
    }
    let mut steps = p.steps.unwrap_or(16);
    if steps <= 0 {
        steps = 16;
    }
    let mut rng = Mulberry32::new(p.seed);
    let tones = chord_tone_set(p.chords.as_ref(), n);
    let is_bass = p.root_note < 48;

    let mut seq = compose_sequence(steps, n, &tones, is_bass, p.density, &mut rng);
    if p.syncopation > 0.0 {
        apply_syncopation(&mut seq, p.syncopation, &mut rng);
    }

    let mut nb = NetBundle::ring(steps);
    for i in 0..steps {
        let deg = seq[i as usize];
        if deg < 0 {
            continue;
        }
        let vel = compute_velocity(i, deg, p.velocity, &tones);
        let dur = compute_duration(deg, p.duration, p.duration_variation, &tones, &mut rng);
        nb.bind(i, notes[deg as usize], p.channel, clamp_velocity(vel), dur);
    }
    let beats = seq.iter().filter(|d| **d >= 0).count() as i64;
    nb.track = gen_track(
        p,
        "markov",
        steps,
        beats,
        None,
        None,
        melodic_params_json(p, &notes, true),
    );
    nb
}
