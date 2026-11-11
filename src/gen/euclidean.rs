//! Port of `Generator/Euclidean.swift` (generator/euclidean.js and riffs.js).

use alloc::vec::Vec;

use crate::gen::core::{accent_velocity, clamp_velocity, json_ints, obj, GenParams};
use crate::gen::markov::markov_melody;
use crate::gen::tables::DrumVariant;
use crate::json::{floor, JsonValue};
use crate::pflow::{NetBundle, Track};
use crate::rng::Mulberry32;

/// Bjorklund: K pulses spread over N steps as evenly as possible (0/1 array).
pub fn bjorklund(k: i64, n: i64) -> Vec<i64> {
    if k >= n {
        return alloc::vec![1; n.max(0) as usize];
    }
    if k == 0 {
        return alloc::vec![0; n as usize];
    }
    let mut groups: Vec<Vec<i64>> = (0..n)
        .map(|i| {
            if i < k {
                alloc::vec![1]
            } else {
                alloc::vec![0]
            }
        })
        .collect();
    let mut kk = k as usize;
    loop {
        let remainder = groups.len() - kk;
        if remainder <= 1 {
            break;
        }
        let merges = kk.min(remainder);
        let mut next: Vec<Vec<i64>> = Vec::new();
        for i in 0..merges {
            let mut g = groups[i].clone();
            g.extend_from_slice(&groups[groups.len() - 1 - i]);
            next.push(g);
        }
        for g in groups.iter().take(groups.len() - merges).skip(merges) {
            next.push(g.clone());
        }
        groups = next;
        kk = merges;
        if groups.len() - kk <= 1 {
            break;
        }
    }
    groups.into_iter().flatten().collect()
}

/// Rotation as euclidean/ghost hihat apply it: `pattern[(i + rot) % n]`.
pub fn rotated(pattern: &[i64], rotation: i64) -> Vec<i64> {
    if rotation == 0 {
        return pattern.to_vec();
    }
    let n = pattern.len() as i64;
    let mut rot = rotation % n;
    if rot < 0 {
        rot += n;
    }
    (0..n).map(|i| pattern[((i + rot) % n) as usize]).collect()
}

/// A generator's track: no instrument yet, the generator's own fields.
pub(crate) fn gen_track(
    p: &GenParams,
    generator: &str,
    ring_size: i64,
    beats: i64,
    rotation: Option<f64>,
    note: Option<f64>,
    generator_params: JsonValue,
) -> Track {
    Track {
        channel: p.channel,
        default_velocity: p.velocity,
        generator: Some(generator.into()),
        ring_size: Some(ring_size as f64),
        beats: Some(beats as f64),
        rotation,
        note,
        generator_params: Some(generator_params),
        ..Track::default()
    }
}

/// `euclidean(k, n, rotation, note, params)`
pub fn euclidean(k: i64, n: i64, rotation: i64, note: i64, p: &GenParams) -> NetBundle {
    let pattern = rotated(&bjorklund(k, n), rotation);
    let mut nb = NetBundle::ring(n);
    for i in 0..n {
        if pattern[i as usize] == 1 {
            let vel = accent_velocity(i, n, p.velocity, p.accent);
            nb.bind(i, note, p.channel, clamp_velocity(vel), p.duration);
        }
    }
    nb.track = gen_track(
        p,
        "euclidean",
        n,
        k,
        Some(rotation as f64),
        Some(note as f64),
        obj(alloc::vec![
            ("duration", Some(JsonValue::int(p.duration))),
            ("seed", Some(JsonValue::Number(p.seed))),
            ("accent", Some(JsonValue::int(p.accent as i64))),
        ]),
    );
    nb
}

/// `euclideanMelodic(notes, steps, seed, params)`: every step bound, cycling `notes`.
pub fn euclidean_melodic(notes: &[i64], steps: i64, seed: f64, p: &GenParams) -> NetBundle {
    let notes: Vec<i64> = if notes.is_empty() {
        alloc::vec![60]
    } else {
        notes.to_vec()
    };
    let mut nb = NetBundle::ring(steps);
    for i in 0..steps {
        nb.bind(
            i,
            notes[i as usize % notes.len()],
            p.channel,
            p.velocity,
            p.duration,
        );
    }
    nb.track = gen_track(
        p,
        "euclidean-melodic",
        steps,
        steps,
        None,
        None,
        obj(alloc::vec![
            ("scale", Some(json_ints(&notes))),
            ("seed", Some(JsonValue::Number(seed))),
            ("duration", Some(JsonValue::int(p.duration))),
        ]),
    );
    nb
}

/// `drumRiff(variant, hits, steps, rotation, note, params, style)`
pub fn drum_riff(
    variant: &str,
    hits: i64,
    steps: i64,
    rotation: i64,
    note: i64,
    params: &GenParams,
    style: Option<&DrumVariant>,
) -> NetBundle {
    let mut p = params.clone();
    let mut hits = hits;
    let mut rotation = rotation;
    match style {
        Some(style) if variant != "A" => {
            hits += style.hits_add;
            if let Some(mul) = style.hits_mul {
                if mul != 0.0 && mul != 1.0 {
                    hits = floor(hits as f64 * mul) as i64;
                }
            }
            rotation = (rotation + style.rotation_add) % steps;
            hits = hits.max(1).min(steps);
        }
        _ => match variant {
            "B" => {
                p.velocity = (p.velocity + 15).min(127);
                p.duration = floor(p.duration as f64 * 1.4) as i64;
            }
            "C" => {
                p.velocity = (p.velocity - 20).max(40);
                p.duration = floor(p.duration as f64 * 0.6) as i64;
            }
            _ => {}
        },
    }
    euclidean(hits, steps, rotation, note, &p)
}

/// `melodyRiff(variant, params)`
pub fn melody_riff(variant: &str, params: &GenParams) -> NetBundle {
    let mut p = params.clone();
    let mut rng = Mulberry32::new(p.seed);
    match variant {
        "B" => {
            p.density = (p.density * 1.4).min(1.0);
            p.seed = p.seed + 1000.0 + floor(rng.next() * 1000.0);
        }
        "C" => {
            p.density *= 0.5;
            p.duration = floor(p.duration as f64 * 1.5) as i64;
            p.seed = p.seed + 2000.0 + floor(rng.next() * 1000.0);
        }
        _ => {}
    }
    markov_melody(&p)
}
