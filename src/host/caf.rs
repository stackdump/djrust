//! Apple's Core Audio Format (`.caf`), as far as a loop browser and the decoder need it:
//! the stream's description, the packet table's gapless counts (priming and remainder
//! frames), the magic cookie, and the strings Apple Loops carry (`info`, and the Apple Loops
//! `uuid` chunk: category, subcategory, key, beat count, time signature, genre, descriptors).
//! Pure std; only chunk headers and the small chunks are read, the audio is skipped over.
//!
//! The audio itself is decoded by symphonia's CAF demuxer ([`crate::host::decode`]), with two fixes
//! made from what this module reads: an AAC stream's cookie is an MPEG-4 `esds` descriptor,
//! which symphonia hands to its AAC decoder as is (it wants the AudioSpecificConfig inside:
//! [`audio_specific_config`]), and the packet table's priming and remainder frames are cut, as
//! AVAudioFile does, so a loop is exactly its length and loops seamlessly.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The Apple Loops metadata chunk's UUID (`29819273-b5bf-4aef-b78d-62d1ef90bb2c`).
pub const APPLE_LOOPS_UUID: [u8; 16] = [
    0x29, 0x81, 0x92, 0x73, 0xb5, 0xbf, 0x4a, 0xef, 0xb7, 0x8d, 0x62, 0xd1, 0xef, 0x90, 0xbb, 0x2c,
];

/// What a CAF file says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CafInfo {
    pub sample_rate: f64,
    /// The format's four-character code (`aac `, `lpcm`, `alac`...).
    pub format: String,
    pub channels: u32,
    pub frames_per_packet: u32,
    pub bytes_per_packet: u32,
    /// The audio chunk's size in bytes (without its edit count); None for an open-ended one.
    pub data_bytes: Option<u64>,
    /// From the packet table: frames that are audio, and the encoder's lead-in and padding.
    pub valid_frames: Option<i64>,
    pub priming_frames: i64,
    pub remainder_frames: i64,
    /// The magic cookie (`kuki`), as stored.
    pub cookie: Option<Vec<u8>>,
    /// The `info` chunk's strings, then the Apple Loops chunk's, in file order.
    pub meta: Vec<(String, String)>,
}

impl CafInfo {
    /// Frames of audio: the packet table's valid frames, else what a constant-size format's
    /// data holds.
    pub fn frames(&self) -> Option<i64> {
        if let Some(v) = self.valid_frames {
            return Some(v);
        }
        let bytes = self.data_bytes?;
        (self.bytes_per_packet > 0 && self.frames_per_packet > 0).then(|| {
            (bytes / self.bytes_per_packet as u64) as i64 * self.frames_per_packet as i64
        })
    }

    /// Length in seconds.
    pub fn seconds(&self) -> Option<f64> {
        let f = self.frames()?;
        (self.sample_rate > 0.0 && f > 0).then(|| f as f64 / self.sample_rate)
    }

    /// A metadata value by key (the Apple Loops chunk's wins over `info`'s).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    /// The loop's length in beats (`beat count`).
    pub fn beats(&self) -> Option<f64> {
        self.get("beat count")
            .and_then(|b| b.trim().parse::<f64>().ok())
            .filter(|b| *b > 0.0)
    }

    /// Its tempo: beats over its length.
    pub fn tempo(&self) -> Option<f64> {
        let (b, s) = (self.beats()?, self.seconds()?);
        let bpm = b * 60.0 / s;
        (20.0..=400.0).contains(&bpm).then(|| (bpm * 10.0).round() / 10.0)
    }

    /// Its key, e.g. "Ab minor" (`key signature` and `key type`).
    pub fn key(&self) -> Option<String> {
        let k = self.get("key signature")?;
        Some(match self.get("key type") {
            Some(t) => format!("{k} {t}"),
            None => k.to_string(),
        })
    }
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_i64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// NUL-separated key / value pairs (an `info` chunk after its count, the Apple Loops chunk
/// after its UUID and count).
fn strings(body: &[u8], out: &mut Vec<(String, String)>) {
    let parts: Vec<&[u8]> = body.split(|b| *b == 0).collect();
    let mut i = 0;
    while i + 1 < parts.len() {
        let k = String::from_utf8_lossy(parts[i]).trim().to_string();
        let v = String::from_utf8_lossy(parts[i + 1]).trim().to_string();
        if !k.is_empty() {
            out.push((k, v));
        }
        i += 2;
    }
}

/// Chunks bigger than this are skipped rather than read (the audio, overviews).
const SMALL: i64 = 1 << 16;

/// Read a CAF file's description and metadata from `r`, skipping its audio. None if it is not
/// a CAF file or has no description.
pub fn read_info<R: Read + Seek>(r: &mut R) -> Option<CafInfo> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head).ok()?;
    if &head[0..4] != b"caff" {
        return None;
    }
    let mut info = CafInfo::default();
    let mut have_desc = false;
    let mut loops_meta = Vec::new();
    loop {
        let mut ch = [0u8; 12];
        if r.read_exact(&mut ch).is_err() {
            break;
        }
        let kind = [ch[0], ch[1], ch[2], ch[3]];
        let size = i64::from_be_bytes(ch[4..12].try_into().ok()?);
        if &kind == b"data" {
            if size < 0 {
                // open-ended: the audio runs to the end of the file, nothing follows it
                let here = r.stream_position().ok()?;
                let end = r.seek(SeekFrom::End(0)).ok()?;
                info.data_bytes = Some(end.saturating_sub(here + 4));
                break;
            }
            info.data_bytes = Some((size as u64).saturating_sub(4));
            r.seek(SeekFrom::Current(size)).ok()?;
            continue;
        }
        if size < 0 {
            break;
        }
        let wanted = matches!(&kind, b"desc" | b"pakt" | b"kuki" | b"info" | b"uuid");
        if !wanted || size > SMALL {
            r.seek(SeekFrom::Current(size)).ok()?;
            continue;
        }
        let mut body = vec![0u8; size as usize];
        if r.read_exact(&mut body).is_err() {
            break;
        }
        match &kind {
            b"desc" => {
                info.sample_rate = f64::from_bits(u64::from_be_bytes(body.get(0..8)?.try_into().ok()?));
                info.format = String::from_utf8_lossy(body.get(8..12)?).to_string();
                info.bytes_per_packet = be_u32(&body, 16)?;
                info.frames_per_packet = be_u32(&body, 20)?;
                info.channels = be_u32(&body, 24)?;
                have_desc = true;
            }
            b"pakt" => {
                info.valid_frames = be_i64(&body, 8).filter(|v| *v >= 0);
                info.priming_frames = be_u32(&body, 16).map_or(0, |v| v as i32 as i64).max(0);
                info.remainder_frames = be_u32(&body, 20).map_or(0, |v| v as i32 as i64).max(0);
            }
            b"kuki" => info.cookie = Some(body),
            b"info" => {
                if body.len() >= 4 {
                    strings(&body[4..], &mut info.meta);
                }
            }
            b"uuid" if body.len() >= 20 && body[0..16] == APPLE_LOOPS_UUID => {
                strings(&body[20..], &mut loops_meta);
            }
            _ => {}
        }
    }
    info.meta.extend(loops_meta);
    have_desc.then_some(info)
}

/// [`read_info`] of a file.
pub fn read_info_file(path: &Path) -> Option<CafInfo> {
    let f = std::fs::File::open(path).ok()?;
    read_info(&mut std::io::BufReader::with_capacity(4096, f))
}

/// An MPEG-4 descriptor's length (up to four 7-bit groups) at `at`: (length, bytes read).
fn descriptor_len(b: &[u8], mut at: usize) -> Option<(usize, usize)> {
    let start = at;
    let mut n = 0usize;
    for _ in 0..4 {
        let x = *b.get(at)?;
        at += 1;
        n = (n << 7) | (x & 0x7f) as usize;
        if x & 0x80 == 0 {
            break;
        }
    }
    Some((n, at - start))
}

/// The AudioSpecificConfig inside an `esds` cookie (ES_Descriptor 0x03 → DecoderConfig
/// 0x04 → DecoderSpecificInfo 0x05). A cookie that is already an AudioSpecificConfig (it does
/// not start with 0x03) is returned as is.
pub fn audio_specific_config(cookie: &[u8]) -> Option<Vec<u8>> {
    // some writers put the full `esds` atom (version + flags) before the descriptor
    let b = if cookie.len() > 4 && cookie[0..4] == [0, 0, 0, 0] && cookie.get(4) == Some(&0x03) {
        &cookie[4..]
    } else {
        cookie
    };
    if b.first() != Some(&0x03) {
        return (!b.is_empty()).then(|| b.to_vec());
    }
    let mut at = 1;
    let (_, k) = descriptor_len(b, at)?;
    at += k;
    let flags = *b.get(at + 2)?;
    at += 3; // ES_ID, flags
    if flags & 0x80 != 0 {
        at += 2; // dependsOn_ES_ID
    }
    if flags & 0x40 != 0 {
        at += 1 + *b.get(at)? as usize; // URL
    }
    if flags & 0x20 != 0 {
        at += 2; // OCR_ES_Id
    }
    if *b.get(at)? != 0x04 {
        return None;
    }
    at += 1;
    let (_, k) = descriptor_len(b, at)?;
    at += k + 13; // objectType, streamType, bufferSize, maxBitrate, avgBitrate
    if *b.get(at)? != 0x05 {
        return None;
    }
    at += 1;
    let (n, k) = descriptor_len(b, at)?;
    at += k;
    let asc = b.get(at..at + n)?;
    (!asc.is_empty()).then(|| asc.to_vec())
}

#[cfg(test)]
pub(crate) use tests::caf_bytes as test_caf;

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = kind.to_vec();
        v.extend_from_slice(&(body.len() as i64).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    /// A CAF of 16-bit stereo PCM with an `info` chunk and an Apple Loops chunk, as written by
    /// [`test_caf`].
    pub(crate) fn caf_bytes(frames: usize, rate: f64, loops_meta: &[(&str, &str)]) -> Vec<u8> {
        let mut v = b"caff\x00\x01\x00\x00".to_vec();
        let mut desc = rate.to_bits().to_be_bytes().to_vec();
        desc.extend_from_slice(b"lpcm");
        desc.extend_from_slice(&2u32.to_be_bytes()); // little-endian flag
        desc.extend_from_slice(&4u32.to_be_bytes()); // bytes per packet
        desc.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
        desc.extend_from_slice(&2u32.to_be_bytes()); // channels
        desc.extend_from_slice(&16u32.to_be_bytes()); // bits
        v.extend(chunk(b"desc", &desc));
        let mut data = vec![0u8; 4];
        for i in 0..frames {
            let s = ((i as f32 * 0.05).sin() * 12000.0) as i16;
            data.extend_from_slice(&s.to_le_bytes());
            data.extend_from_slice(&(-s).to_le_bytes());
        }
        v.extend(chunk(b"data", &data));
        let mut inf = 1u32.to_be_bytes().to_vec();
        inf.extend_from_slice(b"genre\0Test\0");
        v.extend(chunk(b"info", &inf));
        let mut u = APPLE_LOOPS_UUID.to_vec();
        u.extend_from_slice(&(loops_meta.len() as u32).to_be_bytes());
        for (k, val) in loops_meta {
            u.extend_from_slice(k.as_bytes());
            u.push(0);
            u.extend_from_slice(val.as_bytes());
            u.push(0);
        }
        v.extend(chunk(b"uuid", &u));
        v
    }

    #[test]
    fn reads_the_description_and_apple_loops_metadata() {
        let b = caf_bytes(
            44100,
            44100.0,
            &[
                ("category", "Vocals"),
                ("key signature", "Ab"),
                ("key type", "minor"),
                ("beat count", "2"),
                ("genre", "House"),
            ],
        );
        let i = read_info(&mut std::io::Cursor::new(b)).unwrap();
        assert_eq!(i.format, "lpcm");
        assert_eq!((i.channels, i.sample_rate), (2, 44100.0));
        assert_eq!(i.frames(), Some(44100));
        assert_eq!(i.seconds(), Some(1.0));
        assert_eq!(i.get("category"), Some("Vocals"));
        assert_eq!(i.get("genre"), Some("House"), "the loops chunk wins over info");
        assert_eq!(i.key().as_deref(), Some("Ab minor"));
        assert_eq!(i.beats(), Some(2.0));
        assert_eq!(i.tempo(), Some(120.0));
    }

    /// A PCM CAF decodes through symphonia's demuxer to exactly its frames.
    #[cfg(feature = "decode")]
    #[test]
    fn a_pcm_caf_decodes() {
        let d = std::env::temp_dir().join(format!("djhost-caf-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("t.caf");
        std::fs::write(&p, caf_bytes(1000, 48000.0, &[("beat count", "1")])).unwrap();
        let n = crate::host::decode::decode_native(&p, false).unwrap();
        assert_eq!(n.left.len(), 1000);
        assert_eq!(n.rate, Some(48000.0));
        let r = n.right.unwrap();
        assert!((n.left[10] + r[10]).abs() < 1e-4, "right is the left inverted");
        assert!((n.left[10] - (0.5f32).sin() * 12000.0 / 32768.0).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The installed Apple Loops (read in place, never copied), where this machine has them:
    /// AAC in CAF decodes, trimmed to the packet table's valid frames.
    #[cfg(feature = "decode")]
    #[test]
    fn installed_apple_loops_decode_where_present() {
        let root = std::path::Path::new("/Library/Audio/Apple Loops/Apple");
        let Some(file) = std::fs::read_dir(root).ok().and_then(|mut packs| {
            packs.find_map(|p| {
                std::fs::read_dir(p.ok()?.path()).ok()?.flatten().map(|f| f.path()).find(|f| {
                    f.extension().is_some_and(|e| e == "caf")
                        && std::fs::metadata(f).is_ok_and(|m| m.len() < 400_000)
                })
            })
        }) else {
            eprintln!("no Apple Loops installed: skipped");
            return;
        };
        let info = read_info_file(&file).unwrap();
        let n = crate::host::decode::decode_native(&file, false).unwrap();
        assert_eq!(Some(n.left.len() as i64), info.frames(), "{}", file.display());
        assert!(n.left.iter().any(|v| v.abs() > 1e-3), "not silent");
    }

    #[test]
    fn not_a_caf_is_none() {
        assert!(read_info(&mut std::io::Cursor::new(b"RIFF....WAVE".to_vec())).is_none());
    }

    #[test]
    fn the_esds_cookie_yields_its_audio_specific_config() {
        // ES_Descriptor > DecoderConfigDescriptor (AAC LC) > DecoderSpecificInfo 12 10, the
        // shape Apple writes for 44.1 kHz stereo AAC-LC
        let cookie = [
            0x03, 0x80, 0x80, 0x80, 0x22, 0x00, 0x00, 0x00, 0x04, 0x80, 0x80, 0x80, 0x14, 0x40,
            0x15, 0x00, 0x18, 0x00, 0x00, 0x05, 0x79, 0x80, 0x00, 0x03, 0xe8, 0x00, 0x05, 0x80,
            0x80, 0x80, 0x02, 0x12, 0x10, 0x06, 0x80, 0x80, 0x80, 0x01, 0x02,
        ];
        assert_eq!(audio_specific_config(&cookie), Some(vec![0x12, 0x10]));
        assert_eq!(audio_specific_config(&[0x12, 0x10]), Some(vec![0x12, 0x10]));
    }
}

