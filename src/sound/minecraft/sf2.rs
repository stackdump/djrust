//! `SF2Writer` (`Audio/MinecraftBank.swift`): a minimal SoundFont 2 writer, mono 16-bit
//! samples and one instrument per preset. The bytes are a pure function of the calls, so the
//! same assets build the same bank.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

use super::import::SAMPLE_RATE;

/// `SF2Writer.Zone`: one key range of a preset's instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub keys: RangeInclusive<i64>,
    pub sample: usize,
    /// Every key plays the sample at its own pitch (scaleTuning 0).
    pub fixed_pitch: bool,
    pub root_key: i64,
    /// Swift's `loop`: `sampleModes` loops continuously.
    pub looped: bool,
    pub release_seconds: f64,
    /// Zone attenuation, dB (`initialAttenuation`, written when > 0).
    pub attenuation_db: f64,
}

impl Zone {
    /// `Zone(keys:sample:fixedPitch:rootKey:loop:releaseSeconds:)`, attenuation 0.
    pub fn new(
        keys: RangeInclusive<i64>,
        sample: usize,
        fixed_pitch: bool,
        root_key: i64,
        looped: bool,
        release_seconds: f64,
    ) -> Zone {
        Zone {
            keys,
            sample,
            fixed_pitch,
            root_key,
            looped,
            release_seconds,
            attenuation_db: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sf2Writer {
    name: String,
    smpl: Vec<u8>,
    shdr: Vec<u8>,
    sample_count: usize,
    presets: Vec<(String, i64, Vec<Zone>)>,
}

fn le16(d: &mut Vec<u8>, v: u16) {
    d.extend_from_slice(&v.to_le_bytes());
}
fn le32(d: &mut Vec<u8>, v: u32) {
    d.extend_from_slice(&v.to_le_bytes());
}
/// `Data.appendFixed`: the name's first `n - 1` bytes, zero-padded to `n`.
fn fixed(d: &mut Vec<u8>, s: &str, n: usize) {
    let b = s.as_bytes();
    let b = &b[..b.len().min(n - 1)];
    d.extend_from_slice(b);
    d.resize(d.len() + (n - b.len()), 0);
}

fn chunk(id: &str, body: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(body.len() + 9);
    d.extend_from_slice(id.as_bytes());
    le32(&mut d, body.len() as u32);
    d.extend_from_slice(body);
    if body.len() % 2 == 1 {
        d.push(0);
    }
    d
}

fn list(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 4);
    b.extend_from_slice(kind.as_bytes());
    b.extend_from_slice(body);
    chunk("LIST", &b)
}

fn gen(op: u16, amount: u16) -> [u8; 4] {
    let a = op.to_le_bytes();
    let b = amount.to_le_bytes();
    [a[0], a[1], b[0], b[1]]
}

/// `signed`: `UInt16(bitPattern: Int16(clamping: v))`.
fn signed(v: i64) -> u16 {
    (v.clamp(i16::MIN as i64, i16::MAX as i64) as i16) as u16
}

impl Sf2Writer {
    /// `SF2Writer(name:)`.
    pub fn new(name: &str) -> Sf2Writer {
        Sf2Writer {
            name: name.into(),
            smpl: Vec::new(),
            shdr: Vec::new(),
            sample_count: 0,
            presets: Vec::new(),
        }
    }

    /// `addSample`: a sample, 16-bit, followed by the 46 zero frames the spec asks for; returns
    /// its index. `loop_` is in frames from the sample's start.
    pub fn add_sample(
        &mut self,
        name: &str,
        pcm: &[f32],
        root: i64,
        correction: i64,
        loop_: Option<Range<usize>>,
    ) -> usize {
        let start = (self.smpl.len() / 2) as u32;
        for &x in pcm {
            // max(-32767, min(32767, rounded)) with Swift's min / max, which differ from
            // f32::clamp only for a NaN (32767 here)
            let r = libm::roundf(x * 32767.0);
            let m = if r < 32767.0 { r } else { 32767.0 };
            let v = (if m >= -32767.0 { m } else { -32767.0 }) as i16;
            self.smpl.extend_from_slice(&v.to_le_bytes());
        }
        let end = (self.smpl.len() / 2) as u32;
        self.smpl.resize(self.smpl.len() + 46 * 2, 0);
        let ls = loop_.as_ref().map_or(start, |r| start + r.start as u32);
        let le = loop_
            .as_ref()
            .map_or(start.max(end.saturating_sub(1)), |r| start + r.end as u32);
        let mut h = Vec::with_capacity(46);
        fixed(&mut h, name, 20);
        for v in [start, end, ls, le, SAMPLE_RATE as u32] {
            le32(&mut h, v);
        }
        h.push(root.clamp(0, 255) as u8);
        h.push(correction.clamp(-128, 127) as i8 as u8);
        le16(&mut h, 0); // sample link
        le16(&mut h, 1); // mono
        self.shdr.extend_from_slice(&h);
        self.sample_count += 1;
        self.sample_count - 1
    }

    /// `addPreset`.
    pub fn add_preset(&mut self, name: &str, program: i64, zones: Vec<Zone>) {
        self.presets.push((name.into(), program, zones));
    }

    /// `data()`: the RIFF `sfbk` file.
    pub fn data(&self) -> Vec<u8> {
        let mut inst = Vec::new();
        let mut ibag = Vec::new();
        let mut igen = Vec::new();
        let mut phdr = Vec::new();
        let mut pbag = Vec::new();
        let mut pgen = Vec::new();
        let mut bag_index = 0usize;
        let mut gen_index = 0usize;
        for (i, (name, program, zones)) in self.presets.iter().enumerate() {
            fixed(&mut inst, name, 20);
            le16(&mut inst, bag_index as u16);
            for z in zones {
                le16(&mut ibag, gen_index as u16);
                le16(&mut ibag, 0);
                bag_index += 1;
                let mut g: Vec<u8> = Vec::new();
                // keyRange
                g.extend_from_slice(&gen(
                    43,
                    (*z.keys.start() as u16) | ((*z.keys.end() as u16) << 8),
                ));
                let release = libm::round(1200.0 * libm::log2(z.release_seconds.max(0.001))) as i64;
                g.extend_from_slice(&gen(38, signed(release))); // releaseVolEnv (timecents)
                if z.looped {
                    g.extend_from_slice(&gen(54, 1)); // sampleModes: loop continuously
                }
                if z.fixed_pitch {
                    g.extend_from_slice(&gen(56, 0)); // scaleTuning 0: every key at the root's pitch
                }
                if z.attenuation_db > 0.0 {
                    // initialAttenuation, centibels
                    let cb = libm::round(z.attenuation_db * 10.0).min(1440.0);
                    g.extend_from_slice(&gen(48, cb as u16));
                }
                g.extend_from_slice(&gen(58, z.root_key as u16)); // overridingRootKey
                g.extend_from_slice(&gen(53, z.sample as u16)); // sampleID (last)
                gen_index += g.len() / 4;
                igen.extend_from_slice(&g);
            }
            fixed(&mut phdr, name, 20);
            le16(&mut phdr, *program as u16);
            le16(&mut phdr, 0); // bank
            le16(&mut phdr, i as u16); // preset bag index
            for _ in 0..3 {
                le32(&mut phdr, 0);
            }
            le16(&mut pbag, i as u16);
            le16(&mut pbag, 0);
            pgen.extend_from_slice(&gen(41, i as u16)); // instrument
        }
        fixed(&mut inst, "EOI", 20);
        le16(&mut inst, bag_index as u16);
        le16(&mut ibag, gen_index as u16);
        le16(&mut ibag, 0);
        igen.extend_from_slice(&gen(0, 0));
        fixed(&mut phdr, "EOP", 20);
        le16(&mut phdr, 0);
        le16(&mut phdr, 0);
        le16(&mut phdr, self.presets.len() as u16);
        for _ in 0..3 {
            le32(&mut phdr, 0);
        }
        le16(&mut pbag, self.presets.len() as u16);
        le16(&mut pbag, 0);
        pgen.extend_from_slice(&gen(0, 0));
        let mut shdr = self.shdr.clone();
        fixed(&mut shdr, "EOS", 20);
        shdr.resize(shdr.len() + 26, 0);
        let mut ifil = Vec::new();
        le16(&mut ifil, 2);
        le16(&mut ifil, 1);
        let mut inam = Vec::from(self.name.as_bytes());
        inam.push(0);
        let mut info_body = chunk("ifil", &ifil);
        info_body.extend(chunk("isng", b"EMU8000\0"));
        info_body.extend(chunk("INAM", &inam));
        let info = list("INFO", &info_body);
        let sdta = list("sdta", &chunk("smpl", &self.smpl));
        let mut pdta_body = chunk("phdr", &phdr);
        pdta_body.extend(chunk("pbag", &pbag));
        pdta_body.extend(chunk("pmod", &[0u8; 10]));
        pdta_body.extend(chunk("pgen", &pgen));
        pdta_body.extend(chunk("inst", &inst));
        pdta_body.extend(chunk("ibag", &ibag));
        pdta_body.extend(chunk("imod", &[0u8; 10]));
        pdta_body.extend(chunk("igen", &igen));
        pdta_body.extend(chunk("shdr", &shdr));
        let pdta = list("pdta", &pdta_body);
        let mut body = Vec::from(&b"sfbk"[..]);
        body.extend(info);
        body.extend(sdta);
        body.extend(pdta);
        let mut out = Vec::from(&b"RIFF"[..]);
        le32(&mut out, body.len() as u32);
        out.extend(body);
        out
    }
}
