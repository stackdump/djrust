//! Audio files to a sample deck (`Deck.decode`): decode the first audio track, keep at most two
//! channels, and resample to the engine's rate. Swift uses AVAudioFile and AVAudioConverter;
//! here it is symphonia (feature `decode`) and a linear-interpolation resampler, which is
//! enough for scratching and cueing but softer in the top octave than Apple's converter.

use std::path::Path;

use crate::deck::turntable::SampleBuffer;

use crate::host::HostError;

/// The file's name without its extension: what the deck shows (`Deck.load(contentsOf:)`).
pub fn title_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into())
}

/// A RIFF WAVE file's audio (`WAV.encode`'s float32 and 16-bit PCM, mono or stereo; more
/// channels keep the first two), at its own rate: the library's own files, readable without
/// the `decode` feature. None for anything else (another codec, a damaged header).
pub fn read_wav(bytes: &[u8]) -> Option<(SampleBuffer, f64)> {
    let u16le = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32le = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut fmt: Option<(u16, usize, f64, usize)> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32le(at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            let tag = u16le(body)?;
            let channels = u16le(body + 2)? as usize;
            let rate = u32le(body + 4)? as f64;
            let bits = u16le(body + 14)? as usize;
            fmt = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = fmt?;
            let data = bytes.get(body..(body + size).min(bytes.len()))?;
            let bytes_per = bits / 8;
            let frame = channels.checked_mul(bytes_per)?;
            let float = tag == 3 && bits == 32;
            let pcm16 = tag == 1 && bits == 16;
            if channels == 0 || frame == 0 || !(float || pcm16) {
                return None;
            }
            let n = data.len() / frame;
            let sample = |i: usize, c: usize| -> f32 {
                let o = i * frame + c * bytes_per;
                if float {
                    f32::from_bits(u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]))
                } else {
                    let v = i16::from_le_bytes([data[o], data[o + 1]]);
                    // `WAV.encode` scales negative samples by 32768 and the rest by 32767
                    if v < 0 {
                        v as f32 / 32768.0
                    } else {
                        v as f32 / 32767.0
                    }
                }
            };
            if n == 0 {
                return None;
            }
            let left: Vec<f32> = (0..n).map(|i| sample(i, 0)).collect();
            let right = (channels > 1).then(|| (0..n).map(|i| sample(i, 1)).collect());
            return Some((SampleBuffer::new(left, right, rate), rate));
        }
        at = body + size + (size & 1);
    }
    None
}

/// A WAV the library wrote, or any file `decode_file` can read (feature `decode`), at
/// `sample_rate` (linearly resampled where the file's rate differs: `Deck.decode`'s
/// AVAudioConverter is better in the top octave).
pub fn decode_audio(path: &Path, sample_rate: f64) -> Result<SampleBuffer, HostError> {
    let bytes = std::fs::read(path)
        .map_err(|e| HostError::Backend(format!("{}: {e}", path.display())))?;
    let Some((b, rate)) = read_wav(&bytes) else {
        return decode_file(path, sample_rate);
    };
    if (rate - sample_rate).abs() <= 0.5 {
        return Ok(SampleBuffer::new(b.left, b.right, sample_rate));
    }
    Ok(SampleBuffer::new(
        resample(&b.left, rate, sample_rate),
        b.right.as_deref().map(|r| resample(r, rate, sample_rate)),
        sample_rate,
    ))
}

/// An audio file as it is stored: the first (and second) channel at the file's own rate.
#[cfg(feature = "decode")]
pub struct Native {
    pub left: Vec<f32>,
    /// The second channel; none for a mono file.
    pub right: Option<Vec<f32>>,
    pub rate: Option<f64>,
}

/// Decode `path` at `sample_rate`.
#[cfg(feature = "decode")]
pub fn decode_file(path: &Path, sample_rate: f64) -> Result<SampleBuffer, HostError> {
    let n = decode_native(path, false)?;
    let from = n.rate.unwrap_or(sample_rate);
    let (left, right) = if (from - sample_rate).abs() > 0.5 {
        (
            resample(&n.left, from, sample_rate),
            n.right.map(|r| resample(&r, from, sample_rate)),
        )
    } else {
        (n.left, n.right)
    };
    Ok(SampleBuffer::new(left, right, sample_rate))
}

/// Decode `path` at the file's own rate, no resampling. `gapless` trims the encoder's delay and
/// padding where the container says how much (Ogg Vorbis' granule positions, an MP3's LAME
/// tag, an MP4's edit list), as AVAudioFile does; [`decode_file`] keeps them (a sample deck's
/// audio starts a little late for some files).
#[cfg(feature = "decode")]
pub fn decode_native(path: &Path, gapless: bool) -> Result<Native, HostError> {
    use symphonia::core::audio::SampleBuffer as Interleaved;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_VORBIS};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let bad = |e: String| HostError::Backend(format!("{}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(|e| bad(e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: gapless,
                ..FormatOptions::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| bad(format!("not an audio file I can read ({e})")))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| bad("no audio track".into()))?;
    let track_id = track.id;
    let mut source_rate = track.codec_params.sample_rate.map(f64::from);
    let vorbis_frames = if gapless && track.codec_params.codec == CODEC_TYPE_VORBIS {
        track.codec_params.n_frames
    } else {
        None
    };
    // CAF (Apple Loops): an AAC stream's cookie is an `esds`, the decoder wants the
    // AudioSpecificConfig inside it; the packet table says how much lead-in and padding to cut
    let caf = (format_name_is_caf(path)).then(|| crate::host::caf::read_info_file(path)).flatten();
    let mut params = track.codec_params.clone();
    if let Some(c) = &caf {
        if params.codec == symphonia::core::codecs::CODEC_TYPE_AAC {
            if let Some(asc) = c.cookie.as_deref().and_then(crate::host::caf::audio_specific_config) {
                params.extra_data = Some(asc.into_boxed_slice());
            }
        }
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| bad(e.to_string()))?;

    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut stereo = false;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(bad(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue, // a damaged frame: skip it
            Err(e) => return Err(bad(e.to_string())),
        };
        let spec = *decoded.spec();
        source_rate.get_or_insert(spec.rate as f64);
        let channels = spec.channels.count().max(1);
        stereo |= channels > 1;
        let mut buf = Interleaved::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            left.push(frame[0]);
            right.push(if channels > 1 { frame[1] } else { frame[0] });
        }
    }
    // Ogg Vorbis, gapless: the stream is `n` frames long by its last granule position. Where
    // symphonia could not tell the encoder's lead-in from its padding (a file of one audio
    // page, which is its first and its last), more than `n` frames were decoded and the
    // excess is the lead-in, cut at the start as the spec has it and AVFoundation does. And one
    // frame more in every case: AVFoundation's decode starts one frame after where the spec
    // puts it (measured on the game's 44.1 kHz sounds, which then agree to the sample).
    if let Some(n) = vorbis_frames {
        let n = n as usize;
        if n > 0 && left.len() >= n {
            let drop = (left.len() - n + 1).min(left.len() - 1);
            left.drain(..drop);
            right.drain(..drop);
        }
    }
    // CAF: cut the encoder's priming frames and keep the packet table's valid frames, as
    // AVAudioFile does, whether or not `gapless` (a loop must be exactly its length)
    if let Some(c) = &caf {
        let prime = (c.priming_frames.max(0) as usize).min(left.len());
        if prime > 0 && left.len() > prime {
            left.drain(..prime);
            right.drain(..prime);
        }
        if let Some(v) = c.valid_frames.filter(|v| *v > 0) {
            let v = v as usize;
            left.truncate(v);
            right.truncate(v);
        }
    }
    if left.is_empty() {
        return Err(bad("decoded to nothing".into()));
    }
    Ok(Native {
        left,
        right: stereo.then_some(right),
        rate: source_rate,
    })
}

/// A `.caf` by its name (the probe's own answer is not exposed past the demuxer).
#[cfg(feature = "decode")]
fn format_name_is_caf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("caf"))
        || std::fs::File::open(path)
            .and_then(|mut f| {
                let mut b = [0u8; 4];
                std::io::Read::read_exact(&mut f, &mut b).map(|_| &b == b"caff")
            })
            .unwrap_or(false)
}

#[cfg(not(feature = "decode"))]
pub fn decode_file(_: &Path, _: f64) -> Result<SampleBuffer, HostError> {
    Err(HostError::Disabled("decode"))
}

/// Linear interpolation from `from` Hz to `to` Hz.
pub fn resample(x: &[f32], from: f64, to: f64) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let n = ((x.len() as f64) * to / from).round().max(1.0) as usize;
    let step = from / to;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let i0 = (p as usize).min(x.len() - 1);
            let i1 = (i0 + 1).min(x.len() - 1);
            let f = (p - i0 as f64) as f32;
            x[i0] + (x[i1] - x[i0]) * f
        })
        .collect()
}
