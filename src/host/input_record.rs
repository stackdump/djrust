//! Recording the inputs of a set to files (beyond Swift; docs/HEADLESS.md, "Recording the
//! inputs"): opt-in (`djapp --record-inputs DIR`), one mono WAV per input strip for the whole
//! run, of exactly what the strip was fed (before the DC block and the trim, a block the strip
//! did not play as silence), so an offline render of the set's arrangement can feed the strip
//! the same audio and a history scrub or a capture to loop takes what it took live.
//!
//! The render thread copies each block into a ring made with the strip
//! ([`InputMirror::recording`](crate::host::deck::input::InputMirror)); a thread per strip drains
//! the ring into the file, patching the WAV header about once a second so a crash leaves a
//! readable file. The arrangement's `start.inputs[].file` / `file_start` say which file and
//! which sample of it falls on beat 0 ([`Engine::arrange_start`]).
//!
//! Disk use: 4 bytes a sample, about 10 MB a minute per input at 44.1 kHz.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::Consumer;

use crate::host::deck::Engine;

/// The recorders of a run's input strips.
pub struct InputRecorder {
    writers: Vec<Writer>,
}

struct Writer {
    label: String,
    path: PathBuf,
    stop: Sender<()>,
    join: Option<JoinHandle<io::Result<u64>>>,
}

impl InputRecorder {
    /// Start recording every input strip of `engine` into `dir` (made if missing), as
    /// `<stamp>-<label>.wav`. Each strip's `record_file` is set, so the next
    /// [`Engine::arrange_start`] names it.
    pub fn start(engine: &mut Engine, dir: &Path, stamp: &str) -> io::Result<InputRecorder> {
        std::fs::create_dir_all(dir)?;
        let rate = engine.sample_rate as u32;
        let mut writers = Vec::new();
        for i in 0..engine.inputs.len() {
            let Some(rx) = engine.take_input_record_rx(i) else {
                continue;
            };
            let label = engine.inputs[i].settings.label.clone();
            let safe: String = label
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            let path = dir.join(format!("{stamp}-{safe}.wav"));
            let file = File::create(&path)?;
            let (stop, stop_rx) = mpsc::channel();
            let join = std::thread::Builder::new()
                .name(format!("rec-{safe}"))
                .spawn(move || drain(file, rx, rate, stop_rx))?;
            engine.inputs[i].record_file = Some(path.display().to_string());
            engine.set_input_recording(i, true);
            writers.push(Writer {
                label,
                path,
                stop,
                join: Some(join),
            });
        }
        Ok(InputRecorder { writers })
    }

    /// The files being written, by strip label.
    pub fn files(&self) -> Vec<(String, PathBuf)> {
        self.writers
            .iter()
            .map(|w| (w.label.clone(), w.path.clone()))
            .collect()
    }

    /// Stop recording and finish the files. Returns the samples written per strip.
    pub fn finish(&mut self, engine: &mut Engine) -> Vec<(String, u64)> {
        for i in 0..engine.inputs.len() {
            engine.set_input_recording(i, false);
            engine.inputs[i].record_file = None;
        }
        self.writers
            .iter_mut()
            .map(|w| {
                let _ = w.stop.send(());
                let n = w
                    .join
                    .take()
                    .and_then(|j| j.join().ok())
                    .and_then(Result::ok)
                    .unwrap_or(0);
                (w.label.clone(), n)
            })
            .collect()
    }
}

impl Drop for InputRecorder {
    fn drop(&mut self) {
        for w in &mut self.writers {
            let _ = w.stop.send(());
            if let Some(j) = w.join.take() {
                let _ = j.join();
            }
        }
    }
}

const HEADER: u64 = 44;

fn header(rate: u32, samples: u64) -> [u8; HEADER as usize] {
    let data = (samples * 4).min(u32::MAX as u64 - 36) as u32;
    let mut h = [0u8; HEADER as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&32u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// The writer thread: drain the ring into `file` until told to stop, then once more.
fn drain(mut file: File, mut rx: Consumer<f32>, rate: u32, stop: Receiver<()>) -> io::Result<u64> {
    file.write_all(&header(rate, 0))?;
    let mut written = 0u64;
    let mut patched = Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
    loop {
        let done = match stop.recv_timeout(Duration::from_millis(20)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        let n = rx.slots();
        if n > 0 {
            if let Ok(chunk) = rx.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                buf.clear();
                for x in a.iter().chain(b) {
                    buf.extend_from_slice(&x.to_le_bytes());
                }
                chunk.commit_all();
                file.write_all(&buf)?;
                written += n as u64;
            }
        }
        if done || patched.elapsed() >= Duration::from_secs(1) {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header(rate, written))?;
            file.seek(SeekFrom::Start(HEADER + written * 4))?;
            patched = Instant::now();
        }
        if done {
            file.flush()?;
            return Ok(written);
        }
    }
}

/// A recording, or any WAV (PCM 16 / 24 / 32-bit or IEEE float, any channels) as a mono mix,
/// from sample `from` of it on, and its sample rate. The offline render feeds this to a strip.
pub fn read_recording(path: &Path, from: u64) -> io::Result<(Vec<f32>, u32)> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {m}", path.display()));
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let (mut fmt, mut data): (Option<(u16, u16, u32, u16)>, Option<&[u8]>) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        // a file still being written (or cut short) has a data size beyond its end
        let end = body.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " if end - body >= 16 => {
                let b = &bytes[body..end];
                fmt = Some((
                    u16::from_le_bytes([b[0], b[1]]),
                    u16::from_le_bytes([b[2], b[3]]),
                    u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                    u16::from_le_bytes([b[14], b[15]]),
                ));
            }
            b"data" => {
                data = Some(&bytes[body..end]);
                break;
            }
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let (format, channels, rate, bits) = fmt.ok_or_else(|| bad("no fmt chunk"))?;
    let data = data.ok_or_else(|| bad("no data chunk"))?;
    let ch = channels.max(1) as usize;
    let width = (bits / 8) as usize;
    if !matches!((format, bits), (3, 32) | (1, 16) | (1, 24) | (1, 32)) {
        return Err(bad("PCM 16 / 24 / 32 or float 32 only"));
    }
    let frame = width * ch;
    let sample = |b: &[u8]| -> f32 {
        match (format, bits) {
            (3, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (1, 16) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        }
    };
    let out = data
        .chunks_exact(frame)
        .skip(from as usize)
        .map(|f| f.chunks_exact(width).map(sample).sum::<f32>() / ch as f32)
        .collect();
    Ok((out, rate))
}
