//! Port of `Beats/ShareCodec.swift` (share/codec.js's byte helpers): base64url for
//! strings and JSON, and gzip <-> base64url.
//!
//! gzip is a single member — deflate, no name, mtime 0, XFL 0, OS 0xFF — with the
//! RFC 1952 header and CRC-32 / ISIZE trailer written here around raw deflate, exactly
//! the framing the Swift side writes. The deflate stream itself comes from
//! miniz_oxide; it is a valid stream but not promised to be the same bytes Apple's
//! Compression framework emits (see PORTING.md). Nothing hashes gzip bytes: share
//! CIDs are computed over the canonical JSON, so any conforming stream round-trips.

use alloc::string::String;
use alloc::vec::Vec;

use crate::json::JsonValue;

/// Largest inflated size accepted (a share envelope is a few KB; this only bounds abuse).
pub const MAX_INFLATED_BYTES: usize = 16 << 20;

/// Deflate level. Apple's `COMPRESSION_ZLIB` is zlib level 5.
const LEVEL: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    BadBase64,
    BadGzip,
}

impl core::fmt::Display for CodecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            CodecError::BadBase64 => "bad base64url",
            CodecError::BadGzip => "bad gzip",
        })
    }
}

#[cfg(feature = "std")]
impl std::error::Error for CodecError {}

const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `btoa(bin).replace(+ -> -, / -> _).replace(=+$, '')`
pub fn base64_url(bytes: &[u8]) -> String {
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let chars = chunk.len() + 1;
        for k in 0..chars {
            out.push(URL[((n >> (18 - 6 * k)) & 63) as usize] as char);
        }
    }
    out
}

/// The decode half shared by `b64urlDecode` / `b64UrlToGunzip`: pad, map to the
/// standard alphabet, then `atob` (forgiving-base64: ASCII whitespace ignored, bad
/// characters or a length of 1 mod 4 fail). None where JS throws.
pub fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    let pad = (4 - s.encode_utf16().count() % 4) % 4;
    let mut b: Vec<u8> = s
        .bytes()
        .map(|c| match c {
            b'-' => b'+',
            b'_' => b'/',
            c => c,
        })
        .chain(core::iter::repeat_n(b'=', pad))
        .filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r'))
        .collect();
    if b.len() % 4 == 0 {
        if b.last() == Some(&b'=') {
            b.pop();
        }
        if b.last() == Some(&b'=') {
            b.pop();
        }
    }
    if b.len() % 4 == 1 {
        return None;
    }
    let mut table = [-1i32; 256];
    for (i, &c) in STD_ALPHABET.iter().enumerate() {
        table[c as usize] = i as i32;
    }
    let mut out = Vec::with_capacity(b.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in b {
        let v = table[c as usize];
        if v < 0 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `b64urlEncode(obj)`: `JSON.stringify` -> UTF-8 -> base64url.
pub fn b64url_encode(value: &JsonValue) -> String {
    base64_url(value.js_stringified().as_bytes())
}

/// `b64urlDecode(str)`: None on any failure (the JS returns null).
pub fn b64url_decode(s: &str) -> Option<JsonValue> {
    let bytes = base64_url_decode(s)?;
    JsonValue::parse(&String::from_utf8_lossy(&bytes)).ok()
}

/// `gzipToB64Url(str)`
pub fn gzip_to_b64url(text: &str) -> String {
    base64_url(&gzip(text.as_bytes()))
}

/// `b64UrlToGunzip(str)`: errors where the JS rejects.
pub fn b64url_to_gunzip(s: &str) -> Result<String, CodecError> {
    let bytes = base64_url_decode(s).ok_or(CodecError::BadBase64)?;
    let out = gunzip(&bytes)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// A single-member gzip stream (deflate, no name, mtime 0, OS unknown).
pub fn gzip(input: &[u8]) -> Vec<u8> {
    let mut out = alloc::vec![0x1F, 0x8B, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xFF];
    if input.is_empty() {
        out.extend_from_slice(&[0x03, 0x00]); // an empty final fixed-Huffman block
    } else {
        out.extend(miniz_oxide::deflate::compress_to_vec(input, LEVEL));
    }
    out.extend_from_slice(&crc32(input).to_le_bytes());
    out.extend_from_slice(&(input.len() as u32).to_le_bytes());
    out
}

/// Inflate one gzip member; verifies CRC-32 and ISIZE.
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>, CodecError> {
    let bad = CodecError::BadGzip;
    if data.len() < 18 || data[0] != 0x1F || data[1] != 0x8B || data[2] != 0x08 {
        return Err(bad);
    }
    let flags = data[3];
    let mut i = 10usize;
    if flags & 4 != 0 {
        if i + 2 > data.len() {
            return Err(bad);
        }
        i += 2 + data[i] as usize + ((data[i + 1] as usize) << 8);
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            let from = i.min(data.len());
            let z = data[from..].iter().position(|&b| b == 0).ok_or(bad)?;
            i = from + z + 1;
        }
    }
    if flags & 2 != 0 {
        i += 2;
    }
    let end = data.len() - 8;
    if i > end {
        return Err(bad);
    }
    let le32 = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let crc = le32(end);
    let size = le32(end + 4) as usize;
    if size > MAX_INFLATED_BYTES {
        return Err(bad);
    }
    let out = if size == 0 {
        Vec::new()
    } else {
        miniz_oxide::inflate::decompress_to_vec_with_limit(&data[i..end], size).map_err(|_| bad)?
    };
    if out.len() != size || crc32(&out) != crc {
        return Err(bad);
    }
    Ok(out)
}

/// CRC-32 (IEEE, as gzip).
pub fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[n] = c;
            n += 1;
        }
        t
    };
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}
