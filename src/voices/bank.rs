//! A sound bank's preset list without its samples (`BankPresets` in `Audio/DrumKits.swift`):
//! the `phdr` chunk of a SoundFont 2, the instrument headers of a DLS. A 200 MB bank costs a
//! few kB of reads, so the SF2 walk is written against [`ReadAt`] and hosts hand it a file.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A preset of a sound bank: its name, bank (128 = percussion) and program (`BankPreset`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankPreset {
    pub name: String,
    pub bank: i64,
    pub program: i64,
}

/// Random access to a bank's bytes (a file, or a slice).
pub trait ReadAt {
    /// Up to `len` bytes at `offset`; fewer (or none) past the end.
    fn read_at(&self, offset: u64, len: usize) -> Option<Vec<u8>>;
}

impl ReadAt for [u8] {
    fn read_at(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let start = usize::try_from(offset).ok()?;
        if start > self.len() {
            return None;
        }
        let end = start.saturating_add(len).min(self.len());
        Some(self[start..end].to_vec())
    }
}

impl ReadAt for Vec<u8> {
    fn read_at(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        self.as_slice().read_at(offset, len)
    }
}

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
/// A NUL-terminated, space-trimmed string in a fixed field (`Data.cString`).
fn c_string(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).trim().to_string()
}

/// The presets in a `phdr` chunk body (38-byte records, the terminal `EOP` left out).
pub fn phdr_presets(body: &[u8]) -> Vec<BankPreset> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 38 <= body.len() {
        let name = c_string(&body[i..i + 20]);
        let program = u16le(body, i + 20) as i64;
        let bank = u16le(body, i + 22) as i64;
        i += 38;
        if i + 38 > body.len() {
            break; // the last record is EOP
        }
        out.push(BankPreset {
            name,
            bank,
            program,
        });
    }
    out
}

/// A SoundFont's presets (`pdta`/`phdr`). `BankPresets.sf2`.
pub fn sf2_presets(f: &(impl ReadAt + ?Sized)) -> Vec<BankPreset> {
    let Some(riff) = f.read_at(0, 12) else {
        return Vec::new();
    };
    if riff.len() != 12 || &riff[..4] != b"RIFF" || &riff[8..12] != b"sfbk" {
        return Vec::new();
    }
    let end = u32le(&riff, 4) as u64 + 8;
    let mut at: u64 = 12;
    while at + 12 <= end {
        let Some(head) = f.read_at(at, 12) else { break };
        if head.len() != 12 {
            break;
        }
        let size = u32le(&head, 4) as u64;
        if &head[..4] == b"LIST" && &head[8..12] == b"pdta" {
            let mut sub = at + 12;
            let list_end = at + 8 + size;
            while sub + 8 <= list_end {
                let Some(ch) = f.read_at(sub, 8) else { break };
                if ch.len() != 8 {
                    break;
                }
                let n = u32le(&ch, 4) as u64;
                if &ch[..4] == b"phdr" {
                    return match f.read_at(sub + 8, n as usize) {
                        Some(body) => phdr_presets(&body),
                        None => Vec::new(),
                    };
                }
                sub += 8 + n + (n & 1);
            }
            return Vec::new();
        }
        at += 8 + size + (size & 1);
    }
    Vec::new()
}

/// A DLS bank's instruments (`lins` / `ins ` / `insh` + `INAM`): a drum instrument
/// (`F_INSTRUMENT_DRUMS`) as bank 128, a melodic one in its CC0 bank. `BankPresets.dls`.
/// (Apple's `gs_instruments.dls` is the only DLS in play and exists only on a Mac; the
/// parser is pure, so it is tested here against a synthesised bank.)
pub fn dls_presets(data: &[u8]) -> Vec<BankPreset> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"DLS " {
        return Vec::new();
    }
    let mut out = Vec::new();
    // visit(id, list type, body offset, size)
    type Visit<'a> = dyn FnMut(&[u8], Option<&[u8]>, usize, usize) + 'a;
    fn walk(data: &[u8], from: usize, to: usize, visit: &mut Visit) {
        let mut at = from;
        while at + 8 <= to {
            let id = &data[at..at + 4];
            let size = u32le(data, at + 4) as usize;
            let body = at + 8;
            if body + size > data.len() {
                return;
            }
            let ty = if id == b"LIST" && size >= 4 {
                Some(&data[body..body + 4])
            } else {
                None
            };
            visit(id, ty, body, size);
            at = body + size + (size & 1);
        }
    }
    walk(data, 12, data.len(), &mut |id, ty, body, size| {
        if id != b"LIST" || ty != Some(b"lins") {
            return;
        }
        let mut insts: Vec<(usize, usize)> = Vec::new();
        walk(data, body + 4, body + size, &mut |id, ty, ib, isz| {
            if id == b"LIST" && ty == Some(b"ins ") {
                insts.push((ib, isz));
            }
        });
        for (ib, isz) in insts {
            let mut name = String::new();
            let mut bank: i64 = -1;
            let mut program = 0i64;
            let mut infos: Vec<(usize, usize)> = Vec::new();
            walk(data, ib + 4, ib + isz, &mut |id, ty, b, sz| {
                if id == b"insh" && sz >= 12 {
                    let ul_bank = u32le(data, b + 4);
                    program = (u32le(data, b + 8) & 0x7F) as i64;
                    bank = if ul_bank & 0x8000_0000 != 0 {
                        128
                    } else {
                        ((ul_bank >> 8) & 0x7F) as i64
                    };
                } else if id == b"LIST" && ty == Some(b"INFO") {
                    infos.push((b, sz));
                }
            });
            for (b, sz) in infos {
                walk(data, b + 4, b + sz, &mut |id, _, nb, nsz| {
                    if id == b"INAM" {
                        name = c_string(&data[nb..nb + nsz]);
                    }
                });
            }
            if bank >= 0 {
                out.push(BankPreset {
                    name,
                    bank,
                    program,
                });
            }
        }
    });
    out
}
