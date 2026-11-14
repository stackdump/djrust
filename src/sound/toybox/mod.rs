//! The Toy Box bank (`toybox.sf2`): an original SoundFont of small, toy-like voices — a plucked
//! harp, a thumpy bass, bells, chimes, mallets, a reed drone, a chip square, a flute, a twangy
//! string, a little horn, a tiny kit and a few effects — in the spirit of block-game note
//! blocks.
//!
//! It is an independent bank: every voice is synthesized here from first principles (additive
//! partials, FM, filtered noise and envelopes; [`synth`]), and nothing in it is derived from any
//! game's audio — no recording is read, resampled, traced or measured to make it. Being our
//! own, it is an ordinary publishable bank (unlike `minecraft-local.sf2`, which is built from
//! the user's own game files and never leaves their machine).
//!
//! [`build`] makes the whole file, the same bytes on every platform (`libm`, fixed noise
//! seeds, no clock), so a host can write it into its SoundFont folder on first run instead of
//! shipping it. Bump [`VERSION`] whenever the sound changes.

pub mod synth;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

use super::minecraft::import::{hit_energy, octave_zones};
use super::sf2::{Sf2Writer, Zone};
use synth::{peak, rms_db, Rendered, SR};

/// The bank's file name in a SoundFont folder.
pub const FILE_NAME: &str = "toybox.sf2";
/// The bank's name (the SoundFont's `INAM`).
pub const BANK_NAME: &str = "Toy Box";
/// The sound's version: bumped whenever [`build`]'s output changes.
pub const VERSION: u32 = 1;
/// Every sample's rate, Hz.
pub const SAMPLE_RATE: u32 = SR as u32;
/// The kit preset: fixed-pitch zones on the GM drum keys (melodic bank 0).
pub const KIT_PROGRAM: i64 = 127;
/// The kit preset's name (a melodic preset that is a kit by its name, as drum kits are found).
pub const KIT_NAME: &str = "Toy Box Kit";

/// The kit's id (`toybox.sf2#0:127`).
pub fn kit_id() -> String {
    format!("{FILE_NAME}#0:{KIT_PROGRAM}")
}

/// What a program is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToyKind {
    /// Pitched: MIDI key `n` plays pitch `n`.
    Tone,
    /// Fixed pitch, played as rendered on any key.
    Fx,
}

/// One program of the bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToyProgram {
    pub name: &'static str,
    pub program: i64,
    pub kind: ToyKind,
    /// The key the middle zone's sample sounds at (60 for an effect).
    pub root: i64,
    /// A crossfade-free loop sustains it while the key is held.
    pub looped: bool,
    pub description: &'static str,
}

const fn tone(
    name: &'static str,
    program: i64,
    root: i64,
    looped: bool,
    description: &'static str,
) -> ToyProgram {
    ToyProgram {
        name,
        program,
        kind: ToyKind::Tone,
        root,
        looped,
        description,
    }
}

const fn fx(name: &'static str, program: i64, description: &'static str) -> ToyProgram {
    ToyProgram {
        name,
        program,
        kind: ToyKind::Fx,
        root: 60,
        looped: false,
        description,
    }
}

static PROGRAMS: [ToyProgram; 19] = [
    tone("pluck-harp", 0, 69, false, "A short plucked harp string."),
    tone(
        "thump-bass",
        1,
        45,
        false,
        "A round, thumpy bass with a felt hit.",
    ),
    tone(
        "reed-drone",
        2,
        45,
        true,
        "A buzzing reed drone with a slowly moving mouth.",
    ),
    tone(
        "nylon-pick",
        3,
        57,
        false,
        "A soft nylon-string pick with a wooden body.",
    ),
    tone(
        "chip-square",
        4,
        69,
        true,
        "A 25 % pulse, like a game console's sound chip.",
    ),
    tone(
        "twang",
        5,
        69,
        false,
        "A bright, nasal, quickly fading twang.",
    ),
    tone("glass-tine", 6, 69, false, "A glassy electric-piano tine."),
    tone(
        "steel-mallet",
        7,
        69,
        false,
        "A metal bar struck with a mallet.",
    ),
    tone(
        "reed-flute",
        8,
        81,
        true,
        "A breathy little flute with a gentle vibrato.",
    ),
    tone("clank-bell", 9, 81, false, "A short, clanky hand bell."),
    tone("bright-bell", 10, 93, false, "A bright ringing bell."),
    tone(
        "wood-mallet",
        11,
        81,
        false,
        "A dry wooden bar, like a xylophone.",
    ),
    tone(
        "ice-chimes",
        12,
        93,
        false,
        "Glassy chimes, a bar and two smaller ones after it.",
    ),
    tone(
        "toy-horn",
        13,
        57,
        true,
        "A little brass horn that speaks low first.",
    ),
    fx("rise-sweep", 32, "A rising whoosh over three octaves."),
    fx("sparkle", 33, "A falling scatter of tiny high bells."),
    fx("thud", 34, "A heavy, dull thud."),
    fx("coin-blip", 35, "A quick two-note pickup blip."),
    fx("horn-call", 36, "A short horn call that swells up a fifth."),
];

/// Every program at its fixed number (the kit, at [`KIT_PROGRAM`], is separate).
pub fn programs() -> &'static [ToyProgram] {
    &PROGRAMS
}

/// A program's number by name.
pub fn program_named(name: &str) -> Option<i64> {
    PROGRAMS.iter().find(|p| p.name == name).map(|p| p.program)
}

/// One zone of the kit: GM keys → a sound played at its own pitch. `level` is a hit's energy
/// against the kick's, dB (a mixed kit's balance: snare a little under, hats well under).
#[derive(Clone, Debug, PartialEq)]
pub struct ToyKitZone {
    pub name: &'static str,
    pub keys: RangeInclusive<i64>,
    pub level: f64,
}

/// The kit's zones.
pub fn kit_zones() -> [ToyKitZone; 5] {
    [
        ToyKitZone {
            name: "kick",
            keys: 35..=36,
            level: 0.0,
        },
        ToyKitZone {
            name: "snare",
            keys: 37..=41,
            level: -3.0,
        },
        ToyKitZone {
            name: "hat",
            keys: 42..=45,
            level: -13.0,
        },
        ToyKitZone {
            name: "open-hat",
            keys: 46..=48,
            level: -10.0,
        },
        ToyKitZone {
            name: "crash",
            keys: 49..=57,
            level: -9.0,
        },
    ]
}

/// One sample of the bank and the zone that plays it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToySample {
    /// The sample's name in the bank (`pluck-harp-1`, `kit snare`).
    pub name: String,
    /// The program whose zone plays it ([`KIT_PROGRAM`] for the kit).
    pub program: i64,
    pub keys: RangeInclusive<i64>,
    /// The key the sample sounds at (60 for effects and kit sounds).
    pub root: i64,
    /// Every key plays it at its own pitch.
    pub fixed_pitch: bool,
    /// Mono at [`SAMPLE_RATE`], within ±1.
    pub pcm: Vec<f32>,
    /// Its loop, frames, end exclusive.
    pub loop_: Option<Range<usize>>,
    /// The zone's release, seconds.
    pub release: f64,
}

/// A program's loudness, dBFS RMS over its middle zone's first half second, is brought down to
/// this when it would be louder (peaks are already at -1 dBFS); quieter, peakier voices stay
/// where their peak puts them. A host's sampler level-matches on load besides.
const TARGET_RMS_DB: f64 = -16.0;
const PEAK: f64 = 0.891;

fn release_of(p: &ToyProgram) -> f64 {
    match (p.kind, p.name) {
        (ToyKind::Fx, _) => 8.0,
        (_, "reed-drone") => 0.35,
        (_, "chip-square") => 0.05,
        (_, "reed-flute") => 0.18,
        (_, "toy-horn") => 0.15,
        _ => 1.0,
    }
}

/// The gain that brings a program to -1 dBFS peak and at most [`TARGET_RMS_DB`].
fn level(all: &[&[f64]], probe: &[f64]) -> f64 {
    let top = all.iter().fold(0.0f64, |m, x| m.max(peak(x)));
    if top <= 0.0 {
        return 1.0;
    }
    let mut g = PEAK / top;
    let window = &probe[..probe.len().min((0.5 * SR) as usize)];
    let scaled = rms_db(window) + 20.0 * libm::log10(g);
    if scaled > TARGET_RMS_DB {
        g *= libm::pow(10.0, (TARGET_RMS_DB - scaled) / 20.0);
    }
    g
}

fn to_f32(x: &[f64], g: f64) -> Vec<f32> {
    x.iter().map(|v| (v * g) as f32).collect()
}

/// Every sample of the bank, in the order [`build`] writes them: each tonal program in three
/// octave zones (each rendered at its own root, not stretched from one), each effect once, then
/// the kit's sounds.
pub fn samples() -> Vec<ToySample> {
    let mut out = Vec::new();
    for p in PROGRAMS.iter() {
        let release = release_of(p);
        match p.kind {
            ToyKind::Tone => {
                let zones = octave_zones(p.root);
                let rendered: Vec<(RangeInclusive<i64>, i64, i64, Rendered)> = zones
                    .into_iter()
                    .map(|z| {
                        let key = p.root + 12 * z.shift;
                        // a zone an octave lower rings about 30 % longer
                        let stretch = libm::pow(2.0, -0.35 * z.shift as f64);
                        let r = synth::tone(p.name, synth::midi_hz(key as f64), stretch)
                            .expect("every tonal program has a voice");
                        (z.keys, z.shift, key, r)
                    })
                    .collect();
                let all: Vec<&[f64]> = rendered.iter().map(|r| r.3.pcm.as_slice()).collect();
                let mid = rendered
                    .iter()
                    .find(|r| r.1 == 0)
                    .map(|r| r.3.pcm.as_slice())
                    .unwrap_or(all[0]);
                let g = level(&all, mid);
                for (keys, shift, key, r) in rendered.iter() {
                    let suffix = match shift {
                        s if *s < 0 => "-1",
                        s if *s > 0 => "+1",
                        _ => "",
                    };
                    out.push(ToySample {
                        name: format!("{}{}", p.name, suffix),
                        program: p.program,
                        keys: keys.clone(),
                        root: *key,
                        fixed_pitch: false,
                        pcm: to_f32(&r.pcm, g),
                        loop_: r.loop_.clone(),
                        release,
                    });
                }
            }
            ToyKind::Fx => {
                let x = synth::fx(p.name).expect("every effect has a voice");
                let g = level(&[&x], &x);
                out.push(ToySample {
                    name: String::from(p.name),
                    program: p.program,
                    keys: 0..=127,
                    root: 60,
                    fixed_pitch: true,
                    pcm: to_f32(&x, g),
                    loop_: None,
                    release,
                });
            }
        }
    }
    // the kit: each sound peak-normalised, then cut so a hit's energy sits at its level
    // against the kick's (only the louder ones move)
    let zones = kit_zones();
    let mut kit: Vec<(Vec<f64>, f64)> = zones
        .iter()
        .map(|z| {
            let mut x = synth::kit(z.name).expect("every kit zone has a sound");
            let g = PEAK / peak(&x);
            for v in x.iter_mut() {
                *v *= g;
            }
            let e = hit_energy(&to_f32(&x, 1.0));
            (x, e)
        })
        .collect();
    let kick_at = kit
        .iter()
        .zip(zones.iter())
        .map(|((_, e), z)| e - z.level)
        .fold(f64::INFINITY, f64::min);
    for ((x, e), z) in kit.iter_mut().zip(zones.iter()) {
        let cut = (kick_at + z.level - *e).min(0.0);
        let g = libm::pow(10.0, cut / 20.0);
        out.push(ToySample {
            name: format!("kit {}", z.name),
            program: KIT_PROGRAM,
            keys: z.keys.clone(),
            root: 60,
            fixed_pitch: true,
            pcm: to_f32(x, g),
            loop_: None,
            release: 2.0,
        });
    }
    out
}

/// The preset name a program is written under (`toy pluck-harp`).
pub fn preset_name(program: &ToyProgram) -> String {
    format!("toy {}", program.name)
}

/// The whole bank: every program of [`programs`] at its number, and [`KIT_NAME`] at
/// [`KIT_PROGRAM`]. Deterministic: the same bytes on every platform and every call.
pub fn build() -> Vec<u8> {
    let mut writer = Sf2Writer::with_sample_rate(BANK_NAME, SAMPLE_RATE);
    let all = samples();
    let mut zones_of: Vec<(i64, Vec<Zone>)> = Vec::new();
    for s in &all {
        let id = writer.add_sample(&s.name, &s.pcm, s.root, 0, s.loop_.clone());
        let zone = Zone::new(
            s.keys.clone(),
            id,
            s.fixed_pitch,
            s.root,
            s.loop_.is_some(),
            s.release,
        );
        match zones_of.iter_mut().find(|(p, _)| *p == s.program) {
            Some((_, z)) => z.push(zone),
            None => zones_of.push((s.program, alloc::vec![zone])),
        }
    }
    for (program, zones) in zones_of {
        let name = PROGRAMS
            .iter()
            .find(|p| p.program == program)
            .map(preset_name)
            .unwrap_or_else(|| String::from(KIT_NAME));
        writer.add_preset(&name, program, zones);
    }
    writer.data()
}
