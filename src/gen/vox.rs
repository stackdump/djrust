//! `vox`: a sung part for the formant voice (`fx::formant`), an opt-in `compose` override
//! beyond beats.
//!
//! - `"choir"`: the chord plan as a held, stacked choir, one vowel per chord (aah, ooh, …),
//!   on a `voice-choir-*` instrument.
//! - `"robot"`: a chant: syllables (`ro`, `bo`, `da`, …) on the chords' tones in a quarter /
//!   off-beat rhythm, on a `voice-robot*` instrument.
//! - `"auto"`: what the genre suits (trance, synthwave, ambient: choir; house, techno, edm,
//!   electro, italo: robot; the rest none).
//!
//! Like the other techniques it never draws from compose's rng (its own Mulberry32 on the seed
//! and the genre), and it is added after every other net is composed, so switching it on
//! changes nothing but the `vox` net and the structure net that mutes it. The notes are on the
//! chord plan cohesion v2 builds (the theme's own when there is one).

use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::GenParams;
use crate::gen::tables::{BeatsTables, GenreDef};
use crate::gen::theme::{
    build_track_theme, chord_at, clamp_degree, cycle_steps, degree_to_midi, hash_str, ChordPlan,
    TrackTheme,
};
use crate::json::{js_to_int32, JsonValue};
use crate::pflow::NetBundle;
use crate::rng::Mulberry32;

/// The net's id, and the MIDI channel it plays on (free in every genre: drums are 10-15,
/// melody 4, arp 5, bass 6, pad 7).
pub const VOX_NET: &str = "vox";
pub const VOX_CHANNEL: i64 = 8;

/// The kind of part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoxStyle {
    Choir,
    Robot,
}

impl VoxStyle {
    pub fn name(self) -> &'static str {
        match self {
            VoxStyle::Choir => "choir",
            VoxStyle::Robot => "robot",
        }
    }

    /// What the `vox` override asks for in `genre` (None: off, or auto in a genre with none).
    pub fn from_override(v: Option<&JsonValue>, genre: &str) -> Option<VoxStyle> {
        match v?.as_str()? {
            "choir" => Some(VoxStyle::Choir),
            "robot" => Some(VoxStyle::Robot),
            "auto" => auto_style(genre),
            _ => None,
        }
    }

    /// The instruments the part may be voiced with, the first the default.
    pub fn instruments(self) -> &'static [&'static str] {
        match self {
            VoxStyle::Choir => &[
                "voice-choir-aah",
                "voice-choir-ooh",
                "voice-soprano",
                "voice-bass-choir",
            ],
            VoxStyle::Robot => &[
                "voice-robot-choir",
                "voice-robot",
                "voice-android",
                "voice-talkbox",
            ],
        }
    }
}

/// `vox: "auto"`: the part a genre (or the genre a native one is like) suits.
pub fn auto_style(genre: &str) -> Option<VoxStyle> {
    match BeatsTables::shared().kin(genre) {
        "trance" | "synthwave" | "ambient" => Some(VoxStyle::Choir),
        "house" | "techno" | "edm" | "garage" => Some(VoxStyle::Robot),
        _ => None,
    }
}

/// The vowels a choir moves through, one per chord.
const CHOIR_VOWELS: [&[&str]; 3] = [
    &["ah", "oo", "ah", "o"],
    &["oo", "ah", "oo", "e"],
    &["ah", "ah", "oo", "oo"],
];
/// The robot's syllables.
const ROBOT_SYLLABLES: [&str; 12] = [
    "ro", "bo", "da", "di", "mo", "to", "ka", "wa", "yo", "ne", "lu", "ha",
];
/// Robot rhythms over a bar of 16 steps (true: a syllable).
const ROBOT_RHYTHMS: [[bool; 16]; 3] = [
    // quarters
    [
        true, false, false, false, true, false, false, false, true, false, false, false, true,
        false, false, false,
    ],
    // off-beats
    [
        false, false, true, false, false, false, true, false, false, false, true, false, false,
        false, true, false,
    ],
    // a three-step chant
    [
        true, false, false, true, false, false, true, false, false, false, true, false, true,
        false, false, false,
    ],
];

/// The `vox` net for `genre` at `seed`, on `theme`'s chord plan (or the one cohesion v2 would
/// build).
pub fn vox_net(
    style: VoxStyle,
    genre_name: &str,
    genre: &GenreDef,
    seed: f64,
    bpm: f64,
    theme: Option<&TrackTheme>,
) -> NetBundle {
    let plan: ChordPlan = match theme {
        Some(t) => t.plan.clone(),
        None => build_track_theme(genre_name, seed).plan,
    };
    let mut rng = Mulberry32::new(
        ((js_to_int32(seed) as u32) ^ hash_str(&alloc::format!("vox/{genre_name}"))) as f64,
    );
    let scale = genre.scale(genre.root_note + 12);
    let mut n = cycle_steps(&plan);
    if n == 0 {
        n = 64;
    }
    let step_ms = 60000.0 / bpm.max(1.0) / 4.0;
    let mut nb = NetBundle::ring(n);
    match style {
        VoxStyle::Choir => {
            let vowels = CHOIR_VOWELS[rng.next_int(CHOIR_VOWELS.len())];
            let held = ((plan.steps_per_chord.max(4) as f64) * step_ms * 0.95) as i64;
            for (c, chord) in plan.chords.iter().enumerate() {
                let base = c as i64 * plan.steps_per_chord;
                let vowel = vowels[c % vowels.len()];
                for (v, &tone) in chord.tones.iter().take(3).enumerate() {
                    let step = base + v as i64;
                    if step >= n {
                        break;
                    }
                    let note = degree_to_midi(clamp_degree(tone), &scale);
                    nb.bind_sung(step, note, VOX_CHANNEL, 72, held, vowel);
                }
            }
        }
        VoxStyle::Robot => {
            let rhythm = ROBOT_RHYTHMS[rng.next_int(ROBOT_RHYTHMS.len())];
            // a four-syllable word, repeated: a chant, not a random babble
            let word: Vec<&str> = (0..4)
                .map(|_| ROBOT_SYLLABLES[rng.next_int(ROBOT_SYLLABLES.len())])
                .collect();
            let mut sung = 0usize;
            for step in 0..n {
                if !rhythm[(step % 16) as usize] {
                    continue;
                }
                let chord = chord_at(&plan, step);
                let tones = if chord.tones.is_empty() {
                    alloc::vec![chord.root]
                } else {
                    chord.tones.clone()
                };
                let tone = tones[(sung / 2) % tones.len().min(3)];
                let note = degree_to_midi(clamp_degree(tone), &scale);
                let dur = (step_ms * 1.8) as i64;
                nb.bind_sung(step, note, VOX_CHANNEL, 84, dur, word[sung % word.len()]);
                sung += 1;
            }
        }
    }
    let p = GenParams {
        channel: VOX_CHANNEL,
        ..GenParams::default()
    };
    nb.track.channel = p.channel;
    let set = style.instruments();
    let pick = set[rng.next_int(2)];
    nb.track.instrument = pick.into();
    nb.track.instrument_set = set
        .iter()
        .map(|s| JsonValue::String(String::from(*s)))
        .collect();
    nb.track.group = Some(
        match style {
            VoxStyle::Choir => "harmony",
            VoxStyle::Robot => "melody",
        }
        .into(),
    );
    nb
}

/// The sections a vox part sings in: where the melody or the harmony plays, not the intro or
/// the outro (every melodic section when that leaves none).
pub fn vox_sections(names: &[(String, bool)]) -> Vec<bool> {
    let pick: Vec<bool> = names
        .iter()
        .map(|(n, melodic)| *melodic && !matches!(n.as_str(), "intro" | "outro"))
        .collect();
    if pick.iter().any(|b| *b) {
        pick
    } else {
        names.iter().map(|(_, m)| *m).collect()
    }
}
