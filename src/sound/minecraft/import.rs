//! `MinecraftImport` (`Audio/MinecraftBank.swift`): builds `minecraft-local.sf2` and its
//! sidecar from the installed assets. Each sound is decoded (mono, 44.1 kHz, peak-normalised:
//! the host's job, see [`peak_normalise`]), pitched sounds are measured and tuned in the sample
//! header, steady tones (flute, didgeridoo, bit, trumpets) get a crossfade loop so a pad or a
//! held note can sustain, plucked ones stay one-shots. Each tone is also stored an octave down
//! and an octave up ([`octave_shift`], band-limited, loop points scaled) and played from the
//! nearest of the three ([`octave_zones`]), so no key stretches the recording more than half an
//! octave.
//!
//! [`build`] is the whole of `MinecraftImport.run` up to the bytes: it takes the assets, the
//! file system they are read through and a decode function, and returns the SF2 and the sidecar.
//! Writing them out is the host's (`crate::host::minecraft::run`).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Range, RangeInclusive};
use once_cell::race::OnceBox;

use super::assets::{AssetFs, MinecraftAssets, Unavailable};
use super::sf2::{Sf2Writer, Zone};
use super::{bank_programs, kit_zones, Kind, Sidecar, FORMAT_VERSION, KIT_PROGRAM};
use crate::voices::pitch::pitch_class_error;

/// `MinecraftImport.sampleRate`: every sample in the bank.
pub const SAMPLE_RATE: f64 = 44_100.0;

/// `MinecraftImport.Report`, without the path (the host knows where it wrote).
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub asset_index: String,
    pub programs: Vec<ReportProgram>,
    pub skipped: Vec<String>,
    pub bytes: usize,
}

/// One built program of the report: `(name, program, cents, loop)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportProgram {
    pub name: String,
    pub program: i64,
    pub cents: f64,
    /// Swift's `loop`: a crossfade loop holds it.
    pub looped: bool,
}

/// What [`build`] returns: the bank's bytes, its sidecar and the report.
#[derive(Clone, Debug)]
pub struct Built {
    pub sf2: Vec<u8>,
    pub sidecar: Sidecar,
    pub report: Report,
}

/// Why an import stopped (the errors `MinecraftImport.run` lets out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// A kit sound was not there ([`MinecraftAssets::file`]).
    Unavailable(Unavailable),
    /// The host could not decode a sound.
    Decode(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Unavailable(u) => write!(f, "{u}"),
            ImportError::Decode(d) => write!(f, "{d}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ImportError {}

impl From<Unavailable> for ImportError {
    fn from(u: Unavailable) -> Self {
        ImportError::Unavailable(u)
    }
}

/// `MinecraftImport.run` without the file writes: build the bank from `assets`. `decode` turns
/// an object's path (as [`MinecraftAssets::file`] returns it) into a sound as mono float at
/// [`SAMPLE_RATE`], already through [`peak_normalise`] (`MinecraftImport.decode`).
///
/// A program the index lacks, or whose object is not downloaded, is skipped with the reason; a
/// kit sound that is missing, or a sound that does not decode, stops the import.
pub fn build(
    assets: &MinecraftAssets,
    fs: &dyn AssetFs,
    decode: &mut dyn FnMut(&str) -> Result<Vec<f32>, String>,
) -> Result<Built, ImportError> {
    let index: i64 = assets.index_id.parse().unwrap_or(0);
    let mut writer = Sf2Writer::new("minecraft-local");
    let mut sha1: BTreeMap<String, String> = BTreeMap::new();
    let mut built: Vec<ReportProgram> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for p in bank_programs() {
        if index < p.min_index {
            skipped.push(format!(
                "{} (asset index {} < {})",
                p.name, index, p.min_index
            ));
            continue;
        }
        let sound = p
            .event
            .as_ref()
            .and_then(|e| assets.sounds(e).into_iter().next())
            .unwrap_or_else(|| p.sound.clone());
        let file = match assets.file(fs, &sound) {
            Ok(f) => f,
            Err(e) => {
                skipped.push(format!("{}: {}", p.name, e));
                continue;
            }
        };
        let mut pcm = decode(&file.0).map_err(ImportError::Decode)?;
        let mut cents = 0.0;
        let mut root = p.root;
        let mut loop_: Option<Range<usize>> = None;
        if p.kind == Kind::Tone {
            (root, cents) = measure_root(&pcm, p.root);
            if p.looped {
                loop_ = crossfade_loop(&mut pcm, root, cents);
            }
        }
        let release = if p.kind == Kind::Fx {
            8.0
        } else if p.looped {
            0.15
        } else {
            1.2
        };
        let mut zones: Vec<Zone> = Vec::new();
        if p.kind == Kind::Tone {
            for z in octave_zones(root) {
                let shifted = octave_shift(&pcm, loop_.clone(), z.shift);
                let suffix = if z.shift == 0 {
                    ""
                } else if z.shift < 0 {
                    "-1"
                } else {
                    "+1"
                };
                let zone_root = root + 12 * z.shift;
                let sample = writer.add_sample(
                    &format!("{}{}", p.name, suffix),
                    &shifted.0,
                    zone_root,
                    -(libm::round(cents) as i64),
                    shifted.1.clone(),
                );
                zones.push(Zone::new(
                    z.keys,
                    sample,
                    false,
                    zone_root,
                    shifted.1.is_some(),
                    release,
                ));
            }
        } else {
            let sample = writer.add_sample(&p.name, &pcm, root, 0, None);
            zones.push(Zone::new(0..=127, sample, true, root, false, release));
        }
        writer.add_preset(&format!("mc {}", p.name), p.program, zones);
        sha1.insert(p.name.clone(), file.1);
        built.push(ReportProgram {
            name: p.name.clone(),
            program: p.program,
            cents,
            looped: loop_.is_some(),
        });
    }
    // the kit's sounds, each hit's energy brought to its level against the kick by cutting
    // (all are peak-normalised: only the louder ones can move)
    let zones_of_kit = kit_zones();
    struct KitSound {
        sound: &'static str,
        name: &'static str,
        pcm: Vec<f32>,
        level: f64,
    }
    let mut kit: Vec<KitSound> = Vec::new();
    for z in zones_of_kit.iter() {
        if kit.iter().any(|k| k.sound == z.sound) {
            continue;
        }
        let file = assets.file(fs, z.sound)?;
        let pcm = decode(&file.0).map_err(ImportError::Decode)?;
        kit.push(KitSound {
            sound: z.sound,
            name: z.name,
            pcm,
            level: z.level,
        });
        sha1.insert(format!("kit:{}", z.name), file.1);
    }
    // the kick lands where the quietest sound, at full scale, allows
    let mut kick_at = f64::INFINITY;
    for k in &kit {
        let v = hit_energy(&k.pcm) - k.level;
        if v < kick_at {
            kick_at = v;
        }
    }
    let mut kit_samples: BTreeMap<&'static str, usize> = BTreeMap::new();
    for k in kit.iter_mut() {
        let v = kick_at + k.level - hit_energy(&k.pcm);
        let cut = if v < 0.0 { v } else { 0.0 };
        let g = libm::pow(10.0, cut / 20.0) as f32;
        for x in k.pcm.iter_mut() {
            *x *= g;
        }
        let sample = writer.add_sample(&format!("kit {}", k.name), &k.pcm, 60, 0, None);
        kit_samples.insert(k.sound, sample);
    }
    let mut kit_zone_list: Vec<Zone> = Vec::new();
    for z in zones_of_kit.iter() {
        kit_zone_list.push(Zone::new(
            z.keys.clone(),
            kit_samples[z.sound],
            true,
            60,
            false,
            2.0,
        ));
        let key = format!("kit:{}", z.name);
        if !sha1.contains_key(&key) {
            let first = kit
                .iter()
                .find(|k| k.sound == z.sound)
                .map(|k| k.name)
                .unwrap_or_default();
            if let Some(h) = sha1.get(&format!("kit:{first}")).cloned() {
                sha1.insert(key, h);
            }
        }
    }
    writer.add_preset("Minecraft Kit", KIT_PROGRAM, kit_zone_list);
    let sf2 = writer.data();
    let sidecar = Sidecar::new(
        assets.index_id.clone(),
        sha1,
        built.iter().map(|b| b.name.clone()).collect(),
        FORMAT_VERSION,
    );
    let report = Report {
        asset_index: assets.index_id.clone(),
        programs: built,
        skipped,
        bytes: sf2.len(),
    };
    Ok(Built {
        sf2,
        sidecar,
        report,
    })
}

/// The tail of `MinecraftImport.decode`: peak-normalise to -1 dBFS (0.891). The rest of it
/// (reading the file, mixing to mono, converting to [`SAMPLE_RATE`]) is the host's.
pub fn peak_normalise(pcm: &mut [f32]) {
    let mut peak = 0.0f32;
    for &x in pcm.iter() {
        let a = x.abs();
        if a >= peak {
            peak = a;
        }
    }
    if peak > 0.0 {
        let g = 0.891f32 / peak;
        for x in pcm.iter_mut() {
            *x *= g;
        }
    }
}

/// The rate conversion `MinecraftImport.decode` leaves to AVAudioConverter, as a windowed-sinc
/// lowpass-and-interpolate: `x` at `from` Hz to `to` Hz, the passband flat to 0.47 of the
/// lower rate (20.7 kHz for 44.1 kHz) and everything above its Nyquist more than 80 dB down
/// (a Kaiser window, beta 9, 96 output-samples half width), so nothing folds back. The output
/// has `round(len * to / from)` frames and sample `j` sits at input time `j * from / to`.
///
/// Not Apple's converter (that one is not published and has its own delay): the sounds
/// differ from Swift's by the two filters' difference, measured in `djhost/tests` and on the
/// game's own sounds, where it is the 48 kHz ones this applies to. Linear interpolation, the
/// deck's resampler, would fold the top octave back at -30 dB.
pub fn resample(x: &[f32], from: f64, to: f64) -> Vec<f32> {
    if x.is_empty() || from == to {
        return x.to_vec();
    }
    const BETA: f64 = 9.0;
    const HALF_OUT: f64 = 96.0;
    const PHASES: usize = 256;
    let fc = 0.47 * if to < from { to / from } else { 1.0 };
    let half = HALF_OUT * (from / to).max(1.0);
    let i0 = |v: f64| {
        let mut sum = 1.0f64;
        let mut term = 1.0f64;
        let mut k = 1.0f64;
        while term > 1e-12 * sum {
            term *= (v / (2.0 * k)) * (v / (2.0 * k));
            sum += term;
            k += 1.0;
        }
        sum
    };
    let norm = i0(BETA);
    // the kernel at 1/PHASES of an input sample, interpolated linearly between entries
    let len = (2.0 * half * PHASES as f64) as usize + 2;
    let table: Vec<f64> = (0..len)
        .map(|k| {
            let u = k as f64 / PHASES as f64 - half;
            let r = u / half;
            if r.abs() >= 1.0 {
                return 0.0;
            }
            let arg = core::f64::consts::PI * 2.0 * fc * u;
            let sinc = if u == 0.0 {
                2.0 * fc
            } else {
                libm::sin(arg) / (core::f64::consts::PI * u)
            };
            sinc * i0(BETA * libm::sqrt(1.0 - r * r)) / norm
        })
        .collect();
    let n = x.len();
    let count = libm::round(n as f64 * to / from) as usize;
    let step = from / to;
    let mut out = Vec::with_capacity(count);
    for j in 0..count {
        let t = j as f64 * step;
        let lo = libm::ceil(t - half) as i64;
        let hi = libm::floor(t + half) as i64;
        let mut acc = 0.0f64;
        for i in lo.max(0)..=hi.min(n as i64 - 1) {
            let pos = (t - i as f64 + half) * PHASES as f64;
            let k = pos as usize;
            let f = pos - k as f64;
            acc += x[i as usize] as f64 * (table[k] + (table[k + 1] - table[k]) * f);
        }
        out.push(acc as f32);
    }
    out
}

/// One key zone of a tone program (`octaveZones`' tuple): the keys and the octave shift of the
/// recording they play (-1, 0, +1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OctaveZone {
    pub keys: RangeInclusive<i64>,
    pub shift: i64,
}

/// `octaveZones(root:)`: a tone program's key zones, the recording an octave down below
/// `root - 6`, as recorded from `root - 6` to `root + 6`, an octave up above. No key within 18
/// semitones of the root plays a sample more than six semitones from that sample's root.
/// Together they cover 0...127 without overlap.
pub fn octave_zones(root: i64) -> Vec<OctaveZone> {
    let low = (root - 6).clamp(0, 127);
    let high = (root + 6).max(low).min(127);
    let mut out = Vec::new();
    if low > 0 {
        out.push(OctaveZone {
            keys: 0..=(low - 1),
            shift: -1,
        });
    }
    out.push(OctaveZone {
        keys: low..=high,
        shift: 0,
    });
    if high < 127 {
        out.push(OctaveZone {
            keys: (high + 1)..=127,
            shift: 1,
        });
    }
    out
}

/// `octaveFilter`: the lowpass both octave shifts run through, a Kaiser-windowed sinc (255
/// taps, beta 6.5, cut at 0.24 fs) that is down more than 60 dB from 0.25 fs (11.025 kHz), the
/// octave-up sample's new Nyquist (nothing above it folds back), and that also removes the
/// images zero-stuffing leaves in the octave-down one. Unity gain at DC.
pub fn octave_filter() -> &'static [f64] {
    static SHARED: OnceBox<Vec<f64>> = OnceBox::new();
    SHARED.get_or_init(|| {
        let taps = 255usize;
        let beta = 6.5f64;
        let fc = 0.24f64;
        fn i0(x: f64) -> f64 {
            let mut sum = 1.0f64;
            let mut term = 1.0f64;
            let mut k = 1.0f64;
            while term > 1e-12 * sum {
                term *= (x / (2.0 * k)) * (x / (2.0 * k));
                sum += term;
                k += 1.0;
            }
            sum
        }
        let m = (taps - 1) as f64 / 2.0;
        let mut h: Vec<f64> = (0..taps)
            .map(|i| {
                let t = i as f64 - m;
                let sinc = if t == 0.0 {
                    2.0 * fc
                } else {
                    libm::sin(2.0 * core::f64::consts::PI * fc * t) / (core::f64::consts::PI * t)
                };
                let r = t / m;
                sinc * i0(beta * libm::sqrt(1.0 - r * r)) / i0(beta)
            })
            .collect();
        let mut dc = 0.0f64;
        for &x in &h {
            dc += x;
        }
        for x in h.iter_mut() {
            *x /= dc;
        }
        alloc::boxed::Box::new(h)
    })
}

/// `octaveShift(_:loop:octaves:)`: `pcm` (and its loop) moved by `octaves` (-1, 0, +1) at the
/// same sample rate: an octave up keeps every other sample of the lowpassed sound (half as
/// long), an octave down zero-stuffs and lowpasses it (twice as long). Loop points scale with
/// it (an octave up needs an even loop start, which [`crossfade_loop`] gives). Pure: the same
/// input gives the same bytes.
pub fn octave_shift(
    pcm: &[f32],
    loop_: Option<Range<usize>>,
    octaves: i64,
) -> (Vec<f32>, Option<Range<usize>>) {
    let h = octave_filter();
    let c = ((h.len() - 1) / 2) as i64;
    let n = pcm.len() as i64;
    match octaves {
        1 => {
            let count = (n + 1) / 2;
            let mut out = alloc::vec![0.0f32; count as usize];
            for j in 0..count {
                let mid = 2 * j;
                let lo = (mid - c).max(0);
                let hi = (n - 1).min(mid + c);
                let mut acc = 0.0f64;
                if lo <= hi {
                    for i in lo..=hi {
                        acc += h[(mid - i + c) as usize] * pcm[i as usize] as f64;
                    }
                }
                out[j as usize] = acc as f32;
            }
            let l = loop_.and_then(|r| {
                let s = r.start / 2;
                let e = s + libm::round((r.end - r.start) as f64 / 2.0) as usize;
                if (e as i64) + 8 < count && e > s {
                    Some(s..e)
                } else {
                    None
                }
            });
            (out, l)
        }
        -1 => {
            let count = 2 * n;
            let mut out = alloc::vec![0.0f32; count as usize];
            let hn = h.len() as i64;
            for m in 0..count {
                // taps k = m + c - 2i in 0..<h.count
                let i_lo = ((m + c - (hn - 1) + 1) / 2).max(0);
                let i_hi = (n - 1).min((m + c) / 2);
                let mut acc = 0.0f64;
                if i_lo <= i_hi {
                    for i in i_lo..=i_hi {
                        acc += h[(m + c - 2 * i) as usize] * pcm[i as usize] as f64;
                    }
                }
                out[m as usize] = (2.0 * acc) as f32;
            }
            (out, loop_.map(|r| (2 * r.start)..(2 * r.end)))
        }
        _ => (pcm.to_vec(), loop_),
    }
}

/// `hitEnergy`: a hit's energy, dB (its whole length).
pub fn hit_energy(pcm: &[f32]) -> f64 {
    let mut sum = 0.0f64;
    for &x in pcm {
        sum += x as f64 * x as f64;
    }
    let floor = 1e-20f64;
    10.0 * libm::log10(if floor >= sum { floor } else { sum })
}

/// `measureRoot(_:near:)`: the key a recording sounds at, near its nominal root (a semitone
/// either way: an oxidized copper trumpet is not quite an F#), and how many cents off that key
/// it is.
pub fn measure_root(pcm: &[f32], near: i64) -> (i64, f64) {
    let skip = ((0.02 * SAMPLE_RATE) as usize).min(pcm.len());
    let probe = &pcm[skip..];
    let probe = &probe[..probe.len().min((0.3 * SAMPLE_RATE) as usize)];
    let mut best = (near, 0.0f64);
    let mut found = false;
    for root in [near, near - 1, near + 1] {
        let (cents, _octave, level) = pitch_class_error(probe, root, SAMPLE_RATE);
        if level <= 0.0 {
            continue;
        }
        if !found || cents.abs() < best.1.abs() {
            best = (root, cents);
            found = true;
        }
    }
    best
}

/// `crossfadeLoop(_:root:cents:)`: a loop over whole periods of the note in the steady middle
/// of the sound, its end crossfaded into what precedes its start (30 ms) so the jump back is
/// seamless. The loop start is even (the octave-up copy halves it exactly).
pub fn crossfade_loop(pcm: &mut [f32], root: i64, cents: f64) -> Option<Range<usize>> {
    let f0 = 440.0 * libm::pow(2.0, (root as f64 - 69.0) / 12.0) * libm::pow(2.0, cents / 1200.0);
    let period = SAMPLE_RATE / f0;
    let n = pcm.len() as i64;
    let start = (((n as f64) * 0.3) as i64) & !1;
    let ratio = libm::round(0.12 * SAMPLE_RATE / period);
    let periods = if ratio >= 1.0 { ratio } else { 1.0 };
    let length = libm::round(periods * period) as i64;
    let end = start + length;
    let fade = ((0.03 * SAMPLE_RATE) as i64).min(length / 2).min(start);
    if !(end + 8 < n && fade > 8) {
        return None;
    }
    for i in 0..fade {
        let t = i as f32 / fade as f32;
        let a = (end - fade + i) as usize;
        let b = (start - fade + i) as usize;
        pcm[a] = pcm[a] * (1.0 - t) + pcm[b] * t;
    }
    Some(start as usize..end as usize)
}
