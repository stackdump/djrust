//! Captured loops as files (docs/INPUT-HISTORY.md, "Capture to loop"): the maths that snaps a
//! piece of a live input to whole beats at the app tempo, the metadata that sits beside a loop
//! (`<id>.json`), and a small WAV codec for its audio (`<id>.wav`).
//!
//! Pure: no files here. `crate::host::stores::loops` reads and writes them. The WAV is 32-bit float
//! (mono or stereo) so a loop saved and loaded again is bit-identical; the decoder also takes
//! 16/24-bit PCM, so a file dropped into the folder by hand loads too.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};

/// The schema tag in a loop's metadata.
pub const LOOP_SCHEMA: &str = "djrust/loop/v1";
/// The longest capture, in beats (the history ring is sized for this many).
pub const MAX_CAPTURE_BEATS: i64 = 64;

// MARK: quantisation

/// A length asked for in beats, snapped to whole beats (1 ... [`MAX_CAPTURE_BEATS`]).
pub fn quantize_beats(want: f64) -> i64 {
    if !want.is_finite() {
        return 1;
    }
    (libm::round(want) as i64).clamp(1, MAX_CAPTURE_BEATS)
}

/// Frames in `beats` whole beats at `bpm`.
pub fn capture_frames(beats: i64, bpm: f64, sample_rate: f64) -> usize {
    libm::round(beats as f64 * 60.0 / bpm.max(1.0) * sample_rate).max(1.0) as usize
}

/// Frames from the last multiple of `quantum` beats up to `beat` (>= 0, < one quantum).
pub fn frames_since_boundary(beat: f64, quantum: i64, frames_per_beat: f64) -> i64 {
    let q = quantum.max(1) as f64;
    let into = beat - libm::floor(beat / q) * q;
    libm::round(into * frames_per_beat) as i64
}

/// Frames from `beat` ahead to the next multiple of `quantum` beats (0 when on one).
pub fn frames_to_boundary(beat: f64, quantum: i64, frames_per_beat: f64) -> i64 {
    let since = frames_since_boundary(beat, quantum, frames_per_beat);
    if since == 0 {
        0
    } else {
        libm::round(quantum.max(1) as f64 * frames_per_beat) as i64 - since
    }
}

/// The master beat a loop captured up to a boundary starts on: `beat` snapped back to a multiple
/// of `quantum`, less the loop's length.
pub fn start_beat_of(beat: f64, quantum: i64, beats: i64) -> f64 {
    let q = quantum.max(1) as f64;
    libm::floor(beat / q) * q - beats as f64
}

/// Whole bars a held live capture of `elapsed` frames rounds up to (at least one), forgiving a
/// sixteenth of a bar of overshoot as LOOP record on a deck does (`findEnd`).
pub fn held_bars(elapsed: i64, bar_frames: f64) -> i64 {
    let tolerance = bar_frames / 16.0;
    (libm::ceil((elapsed as f64 - tolerance) / bar_frames) as i64).clamp(1, MAX_CAPTURE_BEATS / 4)
}

// MARK: metadata

/// What a saved loop remembers beside its audio.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopMeta {
    /// A hash of its audio (also the file stem).
    pub id: String,
    pub title: String,
    /// The tempo it was captured at.
    pub bpm: f64,
    /// Whole beats long.
    pub beats: i64,
    pub sample_rate: f64,
    pub frames: usize,
    pub channels: u8,
    /// Where it came from: the label of the input, or the deck it was taken from.
    pub source: String,
    /// "input" or "deck".
    pub kind: String,
    /// Unix seconds, 0 when unknown.
    pub created: i64,
    /// Whether it comes back onto the loop row at the next start.
    pub row: bool,
    pub from: String,
}

impl LoopMeta {
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("schema", JsonValue::str(LOOP_SCHEMA));
        o.set("id", JsonValue::str(self.id.clone()));
        o.set("title", JsonValue::str(self.title.clone()));
        o.set("bpm", JsonValue::Number(self.bpm));
        o.set("beats", JsonValue::int(self.beats));
        o.set("sampleRate", JsonValue::Number(self.sample_rate));
        o.set("frames", JsonValue::int(self.frames as i64));
        o.set("channels", JsonValue::int(self.channels as i64));
        o.set("source", JsonValue::str(self.source.clone()));
        o.set("kind", JsonValue::str(self.kind.clone()));
        o.set("created", JsonValue::int(self.created));
        o.set("row", JsonValue::Bool(self.row));
        o.set("from", JsonValue::str(self.from.clone()));
        JsonValue::Object(o)
    }

    /// None unless it names an id, a tempo, a length in beats and a rate.
    pub fn from_json(v: &JsonValue) -> Option<LoopMeta> {
        let s = |k: &str| v.get(k).and_then(JsonValue::as_str).map(str::to_string);
        let id = s("id").filter(|i| !i.is_empty())?;
        let bpm = v.get("bpm")?.as_f64().filter(|b| *b > 0.0)?;
        let beats = v.get("beats")?.as_i64().filter(|b| *b > 0)?;
        let sample_rate = v.get("sampleRate")?.as_f64().filter(|r| *r > 0.0)?;
        Some(LoopMeta {
            title: s("title").unwrap_or_else(|| id.clone()),
            id,
            bpm,
            beats,
            sample_rate,
            frames: v.get("frames").and_then(JsonValue::as_i64).unwrap_or(0).max(0) as usize,
            channels: v.get("channels").and_then(JsonValue::as_i64).unwrap_or(1).clamp(1, 2) as u8,
            source: s("source").unwrap_or_default(),
            kind: s("kind").unwrap_or_else(|| "input".to_string()),
            created: v.get("created").and_then(JsonValue::as_i64).unwrap_or(0),
            row: v.get("row").and_then(JsonValue::as_bool).unwrap_or(true),
            from: s("from").unwrap_or_default(),
        })
    }
}

// MARK: WAV

/// A decoded WAV: one channel, or two.
#[derive(Clone, Debug, PartialEq)]
pub struct Wav {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    /// None: mono.
    pub right: Option<Vec<f32>>,
}

/// RIFF/WAVE bytes, 32-bit float. `right: None` writes a mono file.
pub fn wav_encode(sample_rate: u32, left: &[f32], right: Option<&[f32]>) -> Vec<u8> {
    let channels: u16 = if right.is_some() { 2 } else { 1 };
    let data_len = (left.len() * channels as usize * 4) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&sample_rate.to_le_bytes());
    b.extend_from_slice(&(sample_rate * channels as u32 * 4).to_le_bytes());
    b.extend_from_slice(&(channels * 4).to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..left.len() {
        b.extend_from_slice(&left[i].to_le_bytes());
        if let Some(r) = right {
            b.extend_from_slice(&r[i].to_le_bytes());
        }
    }
    b
}

/// Parse a WAV: PCM 16 / 24 bit or float 32 bit, one or two channels. None for anything else
/// (more channels, compressed, truncated).
pub fn wav_decode(bytes: &[u8]) -> Option<Wav> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |p: usize| bytes.get(p..p + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |p: usize| {
        bytes
            .get(p..p + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let (mut fmt, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut pos = 12;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32_at(pos + 4)? as usize;
        let body = pos + 8;
        let end = body.checked_add(len)?.min(bytes.len());
        if id == b"fmt " {
            fmt = u16_at(body)?;
            channels = u16_at(body + 2)?;
            rate = u32_at(body + 4)?;
            bits = u16_at(body + 14)?;
        } else if id == b"data" {
            data = Some(&bytes[body..end]);
            break;
        }
        pos = body + len + (len & 1);
    }
    let data = data?;
    if !(1..=2).contains(&channels) || rate == 0 {
        return None;
    }
    let width = (bits / 8) as usize;
    let frame = width * channels as usize;
    let one = |s: &[u8]| -> Option<f32> {
        match (fmt, bits) {
            (3, 32) => Some(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
            (1, 16) => Some(i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0),
            (1, 24) => {
                let v = (i32::from_le_bytes([0, s[0], s[1], s[2]])) >> 8;
                Some(v as f32 / 8_388_608.0)
            }
            _ => None,
        }
    };
    if !matches!((fmt, bits), (3, 32) | (1, 16) | (1, 24)) {
        return None;
    }
    let n = data.len() / frame;
    let mut left = Vec::with_capacity(n);
    let mut right = (channels == 2).then(|| Vec::with_capacity(n));
    for f in 0..n {
        let at = f * frame;
        left.push(one(&data[at..at + width])?);
        if let Some(r) = right.as_mut() {
            r.push(one(&data[at + width..at + 2 * width])?);
        }
    }
    Some(Wav {
        sample_rate: rate,
        left,
        right,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_snap_to_whole_beats_and_stay_in_range() {
        assert_eq!(quantize_beats(3.6), 4);
        assert_eq!(quantize_beats(0.4), 1);
        assert_eq!(quantize_beats(0.5), 1);
        assert_eq!(quantize_beats(1000.0), MAX_CAPTURE_BEATS);
        assert_eq!(quantize_beats(f64::NAN), 1);
    }

    #[test]
    fn frames_follow_the_tempo() {
        assert_eq!(capture_frames(4, 120.0, 48_000.0), 96_000);
        assert_eq!(capture_frames(4, 100.0, 48_000.0), 115_200);
        assert_eq!(capture_frames(1, 90.0, 48_000.0), 32_000);
    }

    #[test]
    fn boundaries_are_counted_in_frames() {
        let fpb = 24_000.0;
        // 0.25 beat past a bar line
        assert_eq!(frames_since_boundary(8.25, 4, fpb), 6_000);
        assert_eq!(frames_to_boundary(8.25, 4, fpb), 4 * 24_000 - 6_000);
        // on one
        assert_eq!(frames_since_boundary(8.0, 4, fpb), 0);
        assert_eq!(frames_to_boundary(8.0, 4, fpb), 0);
        // a beat quantum
        assert_eq!(frames_since_boundary(5.5, 1, fpb), 12_000);
        assert_eq!(frames_to_boundary(5.5, 1, fpb), 12_000);
        // beats can be negative (a clock that started behind zero)
        assert_eq!(frames_since_boundary(-0.5, 1, fpb), 12_000);
    }

    #[test]
    fn a_loop_ending_on_a_boundary_starts_beats_before_it() {
        assert_eq!(start_beat_of(9.3, 4, 4), 4.0);
        assert_eq!(start_beat_of(8.0, 4, 8), 0.0);
        assert_eq!(start_beat_of(5.9, 1, 2), 3.0);
    }

    #[test]
    fn a_hold_rounds_up_to_whole_bars_with_a_sixteenth_of_grace() {
        let bar = 96_000.0;
        assert_eq!(held_bars(10_000, bar), 1);
        assert_eq!(held_bars(96_000, bar), 1);
        assert_eq!(held_bars(96_000 + 5_000, bar), 1); // within a sixteenth
        assert_eq!(held_bars(96_000 + 7_000, bar), 2);
        assert_eq!(held_bars(3 * 96_000, bar), 3);
        assert_eq!(held_bars(10_000_000, bar), MAX_CAPTURE_BEATS / 4);
    }

    fn meta() -> LoopMeta {
        LoopMeta {
            id: "00ab".into(),
            title: "TD-3 loop 1".into(),
            bpm: 123.5,
            beats: 8,
            sample_rate: 48_000.0,
            frames: 1234,
            channels: 1,
            source: "TD-3".into(),
            kind: "input".into(),
            created: 1_788_000_000,
            row: false,
            from: String::new(),
        }
    }

    #[test]
    fn metadata_round_trips_through_json_text() {
        let m = meta();
        let text = m.to_json().canonical_js();
        let back = LoopMeta::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn metadata_without_a_tempo_or_length_is_refused() {
        let mut j = meta().to_json();
        if let JsonValue::Object(o) = &mut j {
            o.remove("bpm");
        }
        assert!(LoopMeta::from_json(&j).is_none());
        assert!(LoopMeta::from_json(&JsonValue::Null).is_none());
    }

    #[test]
    fn wav_float_round_trips_bit_for_bit() {
        let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.37).sin() * 0.9).collect();
        let r: Vec<f32> = l.iter().map(|x| -x * 0.5).collect();
        let stereo = wav_decode(&wav_encode(44_100, &l, Some(&r))).unwrap();
        assert_eq!((stereo.sample_rate, &stereo.left, stereo.right.as_ref()), (44_100, &l, Some(&r)));
        let mono = wav_decode(&wav_encode(48_000, &l, None)).unwrap();
        assert_eq!((mono.sample_rate, mono.left, mono.right), (48_000, l, None));
    }

    #[test]
    fn wav_decodes_16_bit_pcm_and_skips_other_chunks() {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        b.extend_from_slice(b"LIST\x03\0\0\0abc\0"); // odd-length chunk, padded
        b.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0");
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&[2, 0, 16, 0]);
        b.extend_from_slice(b"data\x06\0\0\0");
        for s in [0i16, 16384, -32768] {
            b.extend_from_slice(&s.to_le_bytes());
        }
        let w = wav_decode(&b).unwrap();
        assert_eq!(w.sample_rate, 8000);
        assert_eq!(w.left, [0.0, 0.5, -1.0]);
    }

    #[test]
    fn wav_garbage_is_none() {
        assert!(wav_decode(b"").is_none());
        assert!(wav_decode(b"RIFF....WAVEfmt ").is_none());
        let mut b = wav_encode(48_000, &[0.0; 4], None);
        b[20] = 2; // ADPCM
        assert!(wav_decode(&b).is_none());
    }
}
