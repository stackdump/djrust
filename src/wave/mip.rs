//! Port of `MipTables` (`Musicality/SynthVoice.swift`): band-limited wavetables, one per
//! octave of fundamental (a mip map). A pitch sweep on a saw or square (`laser`) steps
//! down through them as the note falls, so it stays under Nyquist: as many partials as
//! fit for the highest note each table plays (up to 128), where beats' tables have 8 for
//! every note. Flattened: table `k` starts at `k × (WAVE_TABLE_SIZE + 1)` and serves
//! fundamentals up to `top[k]`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use once_cell::race::OnceBox;

use super::synth::{build_table, WAVE_TABLE_SIZE};

/// The band-limited tables of one wave at one rate. The arrays are shared (Swift's are
/// copy-on-write values of one process-wide cache): every lane of a wave holds the same
/// ~90 KB, and cloning a `MipTables` copies two pointers.
#[derive(Clone, Debug)]
pub struct MipTables {
    pub samples: Arc<[f32]>,
    pub top: Arc<[f64]>,
}

/// One entry of the process-wide cache: a lock-free, append-only list (`OnceBox` per link, so
/// the engine keeps its no locks, no threads rule and still builds without `std`).
struct Shared {
    wave: String,
    rate: i64,
    tables: MipTables,
    next: OnceBox<Shared>,
}

static SHARED: OnceBox<Shared> = OnceBox::new();

/// The fundamentals the tables start from, doubling per table, up to 20 kHz.
const LOWEST: f64 = 20.0;
const COUNT: usize = 11;
const MAX_PARTIALS: usize = 128;

impl MipTables {
    /// The tables for `wave` ("saw" or "square") at `sr`, built once per process
    /// (`MipTables.shared`: "off the audio thread: prepare calls this"). The key is the wave
    /// and the rate cut to an integer, as Swift's `"\(wave)@\(Int(sampleRate))"`. A build that
    /// two threads race to make is made twice and one copy dropped; neither waits for the other.
    pub fn shared(wave: &str, sr: f64) -> MipTables {
        let rate = sr as i64;
        let mut slot: &'static OnceBox<Shared> = &SHARED;
        while let Some(e) = slot.get() {
            if e.rate == rate && e.wave == wave {
                return e.tables.clone();
            }
            slot = &e.next;
        }
        let built = MipTables::new(wave, sr);
        loop {
            let e = slot.get_or_init(|| {
                Box::new(Shared {
                    wave: String::from(wave),
                    rate,
                    tables: built.clone(),
                    next: OnceBox::new(),
                })
            });
            if e.rate == rate && e.wave == wave {
                return e.tables.clone();
            }
            slot = &e.next;
        }
    }

    /// The tables for `wave` ("saw" or "square") at `sample_rate` (`MipTables.init`).
    pub fn new(wave: &str, sr: f64) -> MipTables {
        let size = WAVE_TABLE_SIZE;
        let tau = 2.0 * core::f64::consts::PI;
        // Every table's fundamental as strong as in beats' table for this wave (normalised
        // by its own peak): the richer tables add harmonics, the note's weight stays.
        let beats = build_table(wave);
        let mut re = 0.0;
        let mut im = 0.0;
        for i in 0..size {
            re += beats[i] as f64 * libm::cos(tau * i as f64 / size as f64);
            im += beats[i] as f64 * libm::sin(tau * i as f64 / size as f64);
        }
        let fundamental = 2.0 * libm::sqrt(re * re + im * im) / size as f64;
        let mut out = vec![0.0f32; COUNT * (size + 1)];
        let mut tops = Vec::new();
        let mut sines = vec![0.0f64; size];
        for k in 0..COUNT {
            let top = LOWEST * libm::pow(2.0, (k + 1) as f64);
            tops.push(top);
            let partials = (MAX_PARTIALS.min((0.45 * sr / top) as usize)).max(1);
            let mut t = vec![0.0f64; size];
            for n in 1..=partials {
                let a = if wave == "square" {
                    if n % 2 == 1 {
                        1.0 / n as f64
                    } else {
                        0.0
                    }
                } else {
                    1.0 / n as f64
                };
                if a == 0.0 {
                    continue;
                }
                for i in 0..size {
                    sines[i] = libm::sin(tau * n as f64 * i as f64 / size as f64);
                }
                for i in 0..size {
                    t[i] += a * sines[i];
                }
            }
            // the fundamental's amplitude is 1 (a = 1 for n = 1) before scaling
            let base = k * (size + 1);
            for i in 0..size {
                out[base + i] = (t[i] * fundamental) as f32;
            }
            out[base + size] = out[base];
        }
        MipTables {
            samples: Arc::from(out),
            top: Arc::from(tops),
        }
    }
}
