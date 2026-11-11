//! Port of `Generator/Variety.swift` (generator/variety.js): ghost-note hihat,
//! walking bass, call/response, modal interchange, drum fills, chorus.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec::Vec;

use crate::gen::core::{
    accent_velocity, clamp_velocity, cos, major_scale, obj, ring_layout, sin, GenParams,
};
use crate::gen::euclidean::{bjorklund, gen_track, rotated};
use crate::gen::markov::{
    chord_tone_set, compose_melody_step, compose_sequence, compute_duration, compute_velocity,
    markov_melody, melodic_params_json,
};
use crate::gen::tables::{ChordDegree, ChordProg, GenreDef};
use crate::json::{floor, JsonValue};
use crate::pflow::{Arc, NetBundle, Place, Project, Track, Transition};
use crate::rng::{Lcg, Mulberry32};

pub struct Tension {
    pub density_mul: f64,
    pub velocity_add: i64,
    pub register_shift: i64,
}

pub fn tension_for_variant(variant: &str) -> Tension {
    match variant {
        "B" => Tension {
            density_mul: 1.3,
            velocity_add: 10,
            register_shift: 2,
        },
        "C" => Tension {
            density_mul: 0.6,
            velocity_add: -10,
            register_shift: -3,
        },
        _ => Tension {
            density_mul: 1.0,
            velocity_add: 0,
            register_shift: 0,
        },
    }
}

pub fn ghost_note_hihat(
    hits: i64,
    steps: i64,
    rotation: i64,
    note: i64,
    p: &GenParams,
    ghost_density: f64,
) -> NetBundle {
    let pattern = rotated(&bjorklund(hits, steps), rotation);
    let mut lcg = Lcg::new(p.seed);
    let mut nb = NetBundle::ring(steps);
    for i in 0..steps {
        if pattern[i as usize] == 1 {
            let vel = accent_velocity(i, steps, p.velocity, p.accent);
            nb.bind(i, note, p.channel, clamp_velocity(vel), p.duration);
        } else if lcg.float64() < ghost_density {
            let ghost_vel = 30 + lcg.intn(20);
            nb.bind(i, note, p.channel, clamp_velocity(ghost_vel), p.duration);
        }
    }
    nb.track = gen_track(
        p,
        "euclidean-ghost",
        steps,
        hits,
        Some(rotation as f64),
        Some(note as f64),
        obj(alloc::vec![
            ("duration", Some(JsonValue::int(p.duration))),
            ("seed", Some(JsonValue::Number(p.seed))),
            ("accent", Some(JsonValue::int(p.accent as i64))),
            ("ghostDensity", Some(JsonValue::Number(ghost_density))),
        ]),
    );
    nb
}

pub fn walking_bass_line(p: &GenParams) -> NetBundle {
    let notes = if p.scale.is_empty() {
        major_scale(p.root_note)
    } else {
        p.scale.clone()
    };
    let mut steps = p.steps.unwrap_or(16);
    if steps <= 0 {
        steps = 16;
    }
    let su = steps as usize;
    let mut lcg = Lcg::new(p.seed);

    let mut chord_roots = alloc::vec![p.root_note; su];
    if let Some(prog) = p.chords.as_ref().filter(|c| !c.chords.is_empty()) {
        let mut steps_per = prog.steps_per;
        if steps_per <= 0 {
            steps_per = 4;
        }
        for i in 0..steps {
            let chord_idx = ((i / steps_per) % prog.chords.len() as i64) as usize;
            let root_deg = prog.chords[chord_idx].root;
            chord_roots[i as usize] = if root_deg < notes.len() as i64 {
                notes[root_deg as usize]
            } else {
                p.root_note
            };
        }
    }

    let mut seq = alloc::vec![0i64; su];
    seq[0] = chord_roots[0];
    for i in 1..steps.max(1) {
        let iu = i as usize;
        let beat_pos = i % 4;
        let current_note = seq[iu - 1];
        let target_root = chord_roots[iu];
        if beat_pos == 0 {
            seq[iu] = target_root;
        } else {
            let next_strong_beat = (i / 4 + 1) * 4;
            let mut next_target = target_root;
            if next_strong_beat < steps {
                next_target = chord_roots[next_strong_beat as usize];
            }
            let diff = next_target - current_note;
            if diff == 0 {
                seq[iu] = if lcg.float64() < 0.5 {
                    current_note + 1
                } else {
                    current_note - 1
                };
            } else {
                let steps_left = 4 - beat_pos;
                let mut step_size = diff / steps_left; // Math.trunc
                if step_size == 0 {
                    step_size = if diff > 0 { 1 } else { -1 };
                }
                if lcg.float64() < 0.3 {
                    seq[iu] = if diff > 0 {
                        current_note + 1
                    } else {
                        current_note - 1
                    };
                } else {
                    seq[iu] = current_note + step_size;
                }
            }
        }
        seq[iu] = seq[iu].clamp(24, 72);
    }

    let mut note_count = floor(p.density * steps as f64) as i64;
    if note_count < 1 {
        note_count = 1;
    }
    let mut rests = BTreeSet::new();
    if note_count < steps {
        let rests_needed = steps - note_count;
        for _ in 0..rests_needed {
            let mut best_idx = -1;
            let mut best_score = 1000;
            for j in 1..steps.max(1) {
                if rests.contains(&j) {
                    continue;
                }
                let score = match j % 4 {
                    0 => 100,
                    2 => 50,
                    _ => 0,
                };
                if score < best_score {
                    best_score = score;
                    best_idx = j;
                }
            }
            if best_idx >= 0 {
                rests.insert(best_idx);
            }
        }
    }

    let mut nb = NetBundle::ring(steps);
    for i in 0..steps {
        if rests.contains(&i) {
            continue;
        }
        let mut dur = p.duration;
        if p.duration_variation > 0.0 {
            dur = floor(dur as f64 * (0.8 + lcg.float64() * 0.4 * p.duration_variation)) as i64;
            if dur < 20 {
                dur = 20;
            }
        }
        let mut vel = p.velocity;
        match i % 4 {
            0 => vel += 15,
            2 => vel += 5,
            _ => vel -= 10,
        }
        nb.bind(i, seq[i as usize], p.channel, clamp_velocity(vel), dur);
    }
    nb.track = gen_track(
        p,
        "walking-bass",
        steps,
        steps - rests.len() as i64,
        None,
        None,
        melodic_params_json(p, &notes, false),
    );
    nb
}

pub fn call_response_melody(p: &GenParams) -> NetBundle {
    let mut notes = if p.scale.is_empty() {
        major_scale(p.root_note)
    } else {
        p.scale.clone()
    };
    let mut n = notes.len() as i64;
    if n > 12 {
        n = 12;
        notes.truncate(12);
    }
    let mut rng = Lcg::new(p.seed);
    let tones = chord_tone_set(p.chords.as_ref(), n);

    let mut total_steps = p.steps.unwrap_or(0);
    if total_steps < 4 {
        total_steps = 32;
    }
    let half = total_steps / 2;
    let resolve_at = (floor(half as f64 * 0.75) as i64).max(1);

    let call_seq = compose_sequence(half, n, &tones, false, p.density, &mut rng);
    let mut response = alloc::vec![0i64; half as usize];
    let mut current = call_seq[0];
    for i in 0..half {
        let iu = i as usize;
        if call_seq[iu] < 0 {
            response[iu] = -1;
            continue;
        }
        if i >= resolve_at {
            if current > 0 {
                current -= 1;
            } else if current < 0 {
                current += 1;
            }
            response[iu] = current;
        } else {
            current = compose_melody_step(current, i % 4, n, &tones, &mut rng);
            response[iu] = current;
        }
    }
    let last = half as usize - 1;
    if response[last] != -1 {
        response[last] = 0;
    }

    let mut seq: Vec<i64> = alloc::vec![-1; total_steps as usize];
    seq[..half as usize].copy_from_slice(&call_seq);
    seq[half as usize..2 * half as usize].copy_from_slice(&response);

    let mut nb = NetBundle::ring(total_steps);
    for i in 0..total_steps {
        let deg = seq[i as usize];
        if deg < 0 {
            continue;
        }
        let vel = compute_velocity(i % 16, deg, p.velocity, &tones);
        let dur = compute_duration(deg, p.duration, p.duration_variation, &tones, &mut rng);
        nb.bind(i, notes[deg as usize], p.channel, clamp_velocity(vel), dur);
    }
    let beats = seq.iter().filter(|d| **d >= 0).count() as i64;
    nb.track = gen_track(
        p,
        "call-response",
        total_steps,
        beats,
        None,
        None,
        melodic_params_json(p, &notes, false),
    );
    nb
}

pub fn apply_modal_interchange(
    prog: &ChordProg,
    scale: &[i64],
    probability: f64,
    rng: &mut Mulberry32,
) -> ChordProg {
    if probability <= 0.0 {
        return prog.clone();
    }
    let is_major = scale.len() >= 3 && scale[2] - scale[0] == 4;
    let mut result = prog.clone();
    for i in 1..result.chords.len() {
        if rng.next() < probability {
            let chord = &result.chords[i];
            let mut tones = chord.tones.clone();
            if tones.len() >= 2 {
                if is_major {
                    if tones[1] > 0 {
                        tones[1] -= 1;
                    }
                } else {
                    tones[1] += 1;
                }
            }
            result.chords[i] = ChordDegree {
                root: chord.root,
                tones,
            };
        }
    }
    result
}

pub fn drum_fill_net(boundary_step: i64, fill_length: i64) -> NetBundle {
    let fill_length = if fill_length < 2 { 4 } else { fill_length };
    const FILL_NOTES: [i64; 6] = [38, 45, 48, 43, 38, 48];
    let total = boundary_step.max(1);
    let mut nb = NetBundle::default();
    let (cx, cy, radius) = ring_layout(total + 1);
    let tau = 2.0 * core::f64::consts::PI;
    for i in 0..=total {
        let angle = i as f64 / (total + 1) as f64 * tau;
        nb.places.insert(
            format!("p{i}"),
            Place {
                initial: alloc::vec![if i == 0 { 1.0 } else { 0.0 }],
                x: cx + radius * 0.7 * cos(angle),
                y: cy + radius * 0.7 * sin(angle),
                label: None,
            },
        );
    }
    for i in 0..total {
        let angle = (i as f64 + 0.5) / (total + 1) as f64 * tau;
        nb.transitions.insert(
            format!("t{i}"),
            Transition {
                x: cx + radius * cos(angle),
                y: cy + radius * sin(angle),
                label: None,
            },
        );
        nb.arcs.push(Arc::new(format!("p{i}"), format!("t{i}")));
        nb.arcs
            .push(Arc::new(format!("t{i}"), format!("p{}", i + 1)));
        let fill_start = total - fill_length;
        if i >= fill_start {
            let fill_pos = i - fill_start;
            let vel = (60 + fill_pos * 50 / fill_length).min(127);
            nb.bind(
                i,
                FILL_NOTES[fill_pos as usize % FILL_NOTES.len()],
                10,
                clamp_velocity(vel),
                40,
            );
        }
    }
    nb.track = Track {
        channel: 10,
        ..Track::default()
    };
    nb.role = "control".into();
    nb
}

/// `chorus(proj, genre, rng)`: adds a `harmony` net a fifth above the melody.
pub fn add_chorus(proj: &mut Project, genre: &GenreDef, rng: &mut Mulberry32) {
    let melody_root = genre.root_note + 12 + 7;
    let p = GenParams {
        scale: genre.scale(melody_root),
        root_note: melody_root,
        channel: 7,
        velocity: 75,
        duration: genre.melody_duration,
        density: genre.melody_density * 0.8,
        seed: rng.next_int63() as f64,
        steps: Some(16),
        ..GenParams::default()
    };
    proj.nets.insert("harmony", markov_melody(&p));
}
