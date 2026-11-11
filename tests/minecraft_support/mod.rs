//! Shared by the Minecraft tests (`tests/minecraft.rs`, `tests/minecraft_golden.rs`, and
//! `djhost/tests/minecraft_import.rs`, which includes this file by path): the integer-only
//! synthesiser `tools/paritydump/minecraft.swift` makes its sounds with, the WAV writer, the
//! synthetic assets tree both sides build, and an in-memory file system for it.
//!
//! No Minecraft asset is used or needed anywhere: every sound is a triangle wave or an LCG's
//! noise under a linear envelope, in integers, so Swift and Rust agree on every sample without a
//! libm in between (the golden records each sound's Int16 hash, checked first).

#![allow(dead_code)]

use std::collections::BTreeMap;

use djrust::cid::{hex, sha256};
use djrust::json::JsonValue;
use djrust::sound::minecraft::assets::{sha1_hex, AssetFs};

/// `McSynth`: "tri" is a triangle at `hz`, "noise" an LCG's top 16 bits; "pluck" falls linearly
/// to silence over `decay` frames, "hold" rises over `attack` frames and stays.
#[derive(Clone, Debug)]
pub struct Synth {
    pub wave: String,
    pub hz: f64,
    pub seed: u32,
    pub frames: usize,
    pub env: String,
    pub decay: usize,
    pub attack: usize,
}

impl Synth {
    pub fn tri(hz: f64, frames: usize) -> Synth {
        Synth {
            wave: "tri".into(),
            hz,
            seed: 1,
            frames,
            env: "pluck".into(),
            decay: frames,
            attack: 0,
        }
    }
    pub fn tri_decay(hz: f64, frames: usize, decay: usize) -> Synth {
        Synth {
            decay,
            ..Synth::tri(hz, frames)
        }
    }
    pub fn tri_hold(hz: f64, frames: usize, attack: usize) -> Synth {
        Synth {
            env: "hold".into(),
            attack,
            ..Synth::tri(hz, frames)
        }
    }
    pub fn noise(seed: u32, frames: usize, decay: usize) -> Synth {
        Synth {
            wave: "noise".into(),
            hz: 0.0,
            seed,
            frames,
            env: "pluck".into(),
            decay,
            attack: 0,
        }
    }

    pub fn from_json(v: &JsonValue) -> Synth {
        let n = |k: &str| v.get(k).and_then(JsonValue::as_f64).unwrap();
        Synth {
            wave: v.get("wave").and_then(JsonValue::as_str).unwrap().into(),
            hz: n("hz"),
            seed: n("seed") as u32,
            frames: n("frames") as usize,
            env: v.get("env").and_then(JsonValue::as_str).unwrap().into(),
            decay: n("decay") as usize,
            attack: n("attack") as usize,
        }
    }

    pub fn int16(&self) -> Vec<i16> {
        let inc = (self.hz / 44_100.0 * 4_294_967_296.0).round() as u32;
        let mut x = self.seed;
        (0..self.frames)
            .map(|k| {
                let v: i64 = if self.wave == "tri" {
                    let phase = (k as u64).wrapping_mul(inc as u64) as u32;
                    let a = (phase >> 16) as i64;
                    2 * (if a < 32_768 { a } else { 65_535 - a }) - 32_768
                } else {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((x as i32) >> 16) as i64
                };
                let (num, den) = if self.env == "pluck" {
                    ((self.decay as i64 - k as i64).max(0), self.decay as i64)
                } else {
                    ((k as i64).min(self.attack as i64), self.attack as i64)
                };
                (v * num * 4 / (5 * den)) as i16
            })
            .collect()
    }
}

/// `mcFloats`: Int16 / 32768, exact in f32.
pub fn floats(x: &[i16]) -> Vec<f32> {
    x.iter().map(|&v| v as f32 / 32_768.0).collect()
}

/// A 16-bit PCM WAV of interleaved `frames`.
pub fn wav(frames: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data = frames.len() as u32 * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    b.extend_from_slice(&(channels * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for f in frames {
        b.extend_from_slice(&f.to_le_bytes());
    }
    b
}

/// The mono 44.1 kHz WAV `mcWav` writes.
pub fn mono_wav(frames: &[i16]) -> Vec<u8> {
    wav(frames, 1, 44_100)
}

/// What `MinecraftImport.decode` makes of a 16-bit mono 44.1 kHz WAV, for the tests that decode
/// without a decoder: the samples as floats, peak-normalised.
pub fn decode_mono_wav(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[36..40] != b"data" {
        return Err("not a 16-bit WAV from mono_wav".into());
    }
    let mut pcm: Vec<f32> = bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32_768.0)
        .collect();
    djrust::sound::minecraft::import::peak_normalise(&mut pcm);
    Ok(pcm)
}

pub fn bytes_sha(b: &[u8]) -> String {
    hex(&sha256(b))
}

pub fn int16_sha(x: &[i16]) -> String {
    let mut b = Vec::with_capacity(x.len() * 2);
    for v in x {
        b.extend_from_slice(&v.to_le_bytes());
    }
    bytes_sha(&b)
}

pub fn floats_sha(x: &[f32]) -> String {
    let mut b = Vec::with_capacity(x.len() * 4);
    for v in x {
        b.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    bytes_sha(&b)
}

pub fn bits32(f: f32) -> String {
    format!("{:08x}", f.to_bits())
}

// MARK: the synthetic assets tree

/// One sound of the tree (`McSound`).
pub struct TreeSound {
    pub sound: String,
    pub synth: Synth,
    pub on_disk: bool,
    pub corrupt: bool,
}

/// The sounds of the golden's `sounds`.
pub fn tree_sounds(golden: &JsonValue) -> Vec<TreeSound> {
    golden
        .get("sounds")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|x| TreeSound {
            sound: x.get("sound").and_then(JsonValue::as_str).unwrap().into(),
            synth: Synth::from_json(x.get("synth").unwrap()),
            on_disk: x.get("onDisk").and_then(JsonValue::as_bool).unwrap(),
            corrupt: x.get("corrupt").and_then(JsonValue::as_bool).unwrap(),
        })
        .collect()
}

/// A corrupt object holds other bytes under the genuine one's name.
fn corrupt_bytes() -> Vec<u8> {
    mono_wav(&Synth::noise(99, 300, 300).int16())
}

/// Every file of the tree, as (path relative to the assets root, bytes): the three index files,
/// the sounds.json object and the objects that are on disk.
pub fn tree_files(golden: &JsonValue) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let indexes = golden
        .get("indexes")
        .and_then(JsonValue::as_object)
        .unwrap();
    for id in ["5", "29", "30"] {
        files.push((
            format!("indexes/{id}.json"),
            indexes
                .get(id)
                .and_then(JsonValue::as_str)
                .unwrap()
                .as_bytes()
                .to_vec(),
        ));
    }
    let sj = golden
        .get("soundsJSON")
        .and_then(JsonValue::as_str)
        .unwrap();
    files.push((object_rel(&sha1_hex(sj.as_bytes())), sj.as_bytes().to_vec()));
    for x in tree_sounds(golden) {
        if !x.on_disk {
            continue;
        }
        let genuine = mono_wav(&x.synth.int16());
        let name = sha1_hex(&genuine);
        files.push((
            object_rel(&name),
            if x.corrupt { corrupt_bytes() } else { genuine },
        ));
    }
    files
}

pub fn object_rel(hash: &str) -> String {
    format!("objects/{}/{}", &hash[..2], hash)
}

/// An in-memory [`AssetFs`]: file paths to bytes; a path is a folder when something lies under
/// it.
#[derive(Default)]
pub struct MemFs(pub BTreeMap<String, Vec<u8>>);

impl MemFs {
    pub fn with(root: &str, files: &[(String, Vec<u8>)]) -> MemFs {
        let mut m = BTreeMap::new();
        for (rel, bytes) in files {
            m.insert(format!("{root}/{rel}"), bytes.clone());
        }
        MemFs(m)
    }
    /// A folder that exists but is empty (marked by a zero-length placeholder).
    pub fn folder(&mut self, path: &str) {
        self.0.insert(format!("{path}/.keep"), Vec::new());
    }
}

impl AssetFs for MemFs {
    fn exists(&self, path: &str) -> bool {
        let p = path.trim_end_matches('/');
        let under = format!("{p}/");
        self.0.contains_key(p) || self.0.keys().any(|k| k.starts_with(&under))
    }
    fn list(&self, dir: &str) -> Option<Vec<String>> {
        let under = format!("{}/", dir.trim_end_matches('/'));
        let mut names: Vec<String> = self
            .0
            .keys()
            .filter_map(|k| k.strip_prefix(&under))
            .filter_map(|rest| rest.split('/').next())
            .filter(|n| *n != ".keep")
            .map(String::from)
            .collect();
        names.dedup();
        if names.is_empty() && !self.exists(dir) {
            None
        } else {
            Some(names)
        }
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

/// `hex` bytes of a golden string.
pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|k| u8::from_str_radix(&s[k..k + 2], 16).unwrap())
        .collect()
}

/// `mcLayout`: every chunk of a RIFF file as (path, size, sha of leaf bodies).
pub fn layout(d: &[u8]) -> Vec<(String, usize, Option<String>)> {
    fn u32le(d: &[u8], at: usize) -> usize {
        u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]) as usize
    }
    fn walk(
        d: &[u8],
        start: usize,
        end: usize,
        path: &str,
        rows: &mut Vec<(String, usize, Option<String>)>,
    ) {
        let mut p = start;
        while p + 8 <= end {
            let id = String::from_utf8_lossy(&d[p..p + 4]).into_owned();
            let size = u32le(d, p + 4);
            let body = p + 8;
            if id == "LIST" || id == "RIFF" {
                let kind = String::from_utf8_lossy(&d[body..body + 4]).into_owned();
                let here = format!("{path}{id}:{kind}");
                rows.push((here.clone(), size, None));
                walk(d, body + 4, body + size, &format!("{here}/"), rows);
            } else {
                rows.push((
                    format!("{path}{id}"),
                    size,
                    Some(bytes_sha(&d[body..body + size])),
                ));
            }
            p = body + size + (size % 2);
        }
    }
    let mut rows = Vec::new();
    walk(d, 0, d.len(), "", &mut rows);
    rows
}

/// A small assets tree of its own (not the golden's): `sounds` are (sound path, the object's
/// bytes); the index `id` lists them, `sounds.json` names no events (every program uses its
/// default sound path).
pub fn custom_tree(id: &str, sounds: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    use djrust::json::JsonObject;
    let mut files = Vec::new();
    let sj = b"{}".to_vec();
    let entry = |bytes: &[u8]| {
        JsonValue::Object(JsonObject::from_pairs([
            ("hash", JsonValue::str(sha1_hex(bytes))),
            ("size", JsonValue::int(bytes.len() as i64)),
        ]))
    };
    let mut objects = JsonObject::new();
    objects.set("minecraft/sounds.json", entry(&sj));
    files.push((object_rel(&sha1_hex(&sj)), sj));
    for (sound, bytes) in sounds {
        objects.set(format!("minecraft/sounds/{sound}.ogg"), entry(bytes));
        files.push((object_rel(&sha1_hex(bytes)), bytes.clone()));
    }
    let index = JsonValue::Object(JsonObject::from_pairs([(
        "objects",
        JsonValue::Object(objects),
    )]));
    files.push((
        format!("indexes/{id}.json"),
        index.js_stringified().into_bytes(),
    ));
    files
}
