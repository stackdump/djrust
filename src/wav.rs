//! RIFF/WAVE bytes for stereo audio: the one encoder (`WAV.encode` in
//! `Session/SessionPlayer.swift`). `session::wav::encode_wav`, `LoopClip::wav_data`, the
//! `djctl` renders and `musicality`'s `RenderedAudio::wav` all go through it.

use alloc::vec::Vec;

/// `WAV.encode`: interleaved stereo RIFF, 32-bit float (`float32`) or 16-bit PCM. 44 header
/// bytes, then `4 * channels` (float) or `2 * channels` (PCM) bytes a frame. PCM clamps to
/// -1...1 and scales negative samples by 32768 and the rest by 32767, rounding half away from
/// zero (as beats' `encodeWav` does). `sample_rate` goes in the header truncated to a `u32`
/// (`UInt32(audio.sampleRate)`). `right` is read for `left.len()` frames.
pub fn wav_bytes(left: &[f32], right: &[f32], sample_rate: f64, float32: bool) -> Vec<u8> {
    let ch = 2usize;
    let bytes_per = if float32 { 4 } else { 2 };
    let n = left.len();
    let rate = sample_rate as u32;
    let mut d: Vec<u8> = Vec::with_capacity(44 + n * ch * bytes_per);
    fn u32le(d: &mut Vec<u8>, v: u32) {
        d.extend(v.to_le_bytes());
    }
    fn u16le(d: &mut Vec<u8>, v: u16) {
        d.extend(v.to_le_bytes());
    }
    d.extend(b"RIFF");
    u32le(&mut d, (36 + n * ch * bytes_per) as u32);
    d.extend(b"WAVEfmt ");
    u32le(&mut d, 16);
    u16le(&mut d, if float32 { 3 } else { 1 });
    u16le(&mut d, ch as u16);
    u32le(&mut d, rate);
    u32le(&mut d, rate.wrapping_mul((ch * bytes_per) as u32));
    u16le(&mut d, (ch * bytes_per) as u16);
    u16le(&mut d, (bytes_per * 8) as u16);
    d.extend(b"data");
    u32le(&mut d, (n * ch * bytes_per) as u32);
    for i in 0..n {
        for s in [left[i], right[i]] {
            if float32 {
                u32le(&mut d, s.to_bits());
            } else {
                let c = s.clamp(-1.0, 1.0);
                let v = if c < 0.0 {
                    libm::roundf(c * 32768.0)
                } else {
                    libm::roundf(c * 32767.0)
                };
                u16le(&mut d, (v as i16) as u16);
            }
        }
    }
    d
}
