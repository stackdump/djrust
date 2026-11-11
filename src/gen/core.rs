//! Port of `Generator/Core.swift` (beats' generator/core.js): scales, accents,
//! params, seeds, and the ring net every generator builds.

use alloc::format;
use alloc::vec::Vec;

use crate::gen::tables::ChordProg;
use crate::json::{JsonObject, JsonValue};
use crate::pflow::{Arc, MidiBinding, NetBundle, Place, Transition};

pub const MAJOR_INTERVALS: [i64; 7] = [0, 2, 4, 5, 7, 9, 11];

pub fn scale_intervals(name: &str) -> &'static [i64] {
    match name {
        "MajorScale" => &[0, 2, 4, 5, 7, 9, 11],
        "MinorScale" => &[0, 2, 3, 5, 7, 8, 10],
        "DorianScale" => &[0, 2, 3, 5, 7, 9, 10],
        "MixolydianScale" => &[0, 2, 4, 5, 7, 9, 10],
        "PhrygianScale" => &[0, 1, 3, 5, 7, 8, 10],
        "HarmonicMinScale" => &[0, 2, 3, 5, 7, 8, 11],
        "PentatonicScale" => &[0, 2, 4, 7, 9],
        "MinPentatonicScale" => &[0, 3, 5, 7, 10],
        "BluesScale" => &[0, 3, 5, 6, 7, 10],
        _ => &[0, 2, 4, 5, 7, 9, 11],
    }
}

/// `scaleNotes`: MIDI notes over `octaves` octaves, dropping anything above 127.
pub fn scale_notes(root: i64, intervals: &[i64], octaves: i64) -> Vec<i64> {
    let mut notes = Vec::new();
    for oct in 0..octaves {
        for &interval in intervals {
            let note = root + oct * 12 + interval;
            if note <= 127 {
                notes.push(note);
            }
        }
    }
    notes
}

pub fn major_scale(root: i64) -> Vec<i64> {
    scale_notes(root, &MAJOR_INTERVALS, 2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Accent {
    #[default]
    None = 0,
    Kick = 1,
    Snare = 2,
    Hihat = 3,
}

/// `accentVelocity`
pub fn accent_velocity(step: i64, _total: i64, base: i64, accent: Accent) -> i64 {
    let jitter = ((step * 7 + 13) % 11) - 5;
    match accent {
        Accent::Kick => match step % 4 {
            0 => base + 20 + jitter,
            2 => base + 8 + jitter,
            _ => base - 15 + jitter,
        },
        Accent::Snare | Accent::None => base + jitter,
        Accent::Hihat => {
            if step % 2 == 0 {
                base + 10 + jitter
            } else {
                base - 30 + jitter
            }
        }
    }
}

/// `clampVelocity`: MIDI range 1–127.
pub fn clamp_velocity(v: i64) -> i64 {
    v.clamp(1, 127)
}

/// `drumSeed`: FNV-1a over UTF-16 code units, 64-bit, then `BigInt.asIntN(53)`.
pub fn drum_seed(genre: &str) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for u in genre.encode_utf16() {
        h ^= u as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let low = h & ((1u64 << 53) - 1);
    let signed = if low >= (1u64 << 52) {
        low as i64 - (1i64 << 53)
    } else {
        low as i64
    };
    signed as f64
}

/// The loose JS params object generators take.
#[derive(Clone, Debug)]
pub struct GenParams {
    pub scale: Vec<i64>,
    pub root_note: i64,
    pub channel: i64,
    pub velocity: i64,
    pub duration: i64,
    pub density: f64,
    pub steps: Option<i64>,
    pub seed: f64,
    pub accent: Accent,
    pub chords: Option<ChordProg>,
    pub duration_variation: f64,
    pub syncopation: f64,
}

impl Default for GenParams {
    fn default() -> Self {
        GenParams {
            scale: Vec::new(),
            root_note: 60,
            channel: 1,
            velocity: 100,
            duration: 100,
            density: 0.5,
            steps: None,
            seed: 0.0,
            accent: Accent::None,
            chords: None,
            duration_variation: 0.0,
            syncopation: 0.0,
        }
    }
}

/// Layout helper shared by every ring generator (`ringLayout`).
pub fn ring_layout(n: i64) -> (f64, f64, f64) {
    let mut radius = n as f64 * 70.0 / (2.0 * core::f64::consts::PI * 0.7);
    if radius < 150.0 {
        radius = 150.0;
    }
    (radius + 80.0, radius + 80.0, radius)
}

pub(crate) fn cos(x: f64) -> f64 {
    libm::cos(x)
}
pub(crate) fn sin(x: f64) -> f64 {
    libm::sin(x)
}

impl NetBundle {
    /// The ring every generator builds: `p0..p(n-1)` (token on p0), `t0..t(n-1)`,
    /// arcs `pi → ti → p(i+1 mod n)`. Callers add bindings and the track.
    pub fn ring(n: i64) -> NetBundle {
        let mut nb = NetBundle::default();
        let (cx, cy, radius) = ring_layout(n);
        let tau = 2.0 * core::f64::consts::PI;
        for i in 0..n {
            let angle = i as f64 / n as f64 * tau;
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
        for i in 0..n {
            let t_angle = (i as f64 + 0.5) / n as f64 * tau;
            nb.transitions.insert(
                format!("t{i}"),
                Transition {
                    x: cx + radius * cos(t_angle),
                    y: cy + radius * sin(t_angle),
                    label: None,
                },
            );
            nb.arcs.push(Arc::new(format!("p{i}"), format!("t{i}")));
            nb.arcs
                .push(Arc::new(format!("t{i}"), format!("p{}", (i + 1) % n)));
        }
        nb
    }

    pub fn bind(&mut self, transition: i64, note: i64, channel: i64, velocity: i64, duration: i64) {
        self.bindings.insert(
            format!("t{transition}"),
            MidiBinding {
                note,
                channel,
                velocity,
                duration,
                duration_steps: 0,
            },
        );
    }
}

/// An object from pairs, skipping `None` values (the JS `undefined` keys).
pub fn obj(pairs: Vec<(&str, Option<JsonValue>)>) -> JsonValue {
    let mut o = JsonObject::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            o.set(k, v);
        }
    }
    o.into()
}

pub fn json_ints(v: &[i64]) -> JsonValue {
    JsonValue::Array(v.iter().map(|n| JsonValue::int(*n)).collect())
}
