//! The audio measures the Swift suites share (`Tests/DJCoreTests/AudioMeasure.swift`), for the
//! tests that read a render rather than compare it with a fixture.

#![allow(dead_code)]

use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;

/// 120 BPM ring of 64 steps with one note: the project `SynthVoiceTests.oneNote` builds.
pub fn one_note(
    group: &str,
    note: i64,
    velocity: i64,
    steps: i64,
    channel: i64,
    instrument: &str,
) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(64);
    n.bind(0, note, channel, velocity, 0);
    n.bindings.get_mut("t0").unwrap().duration_steps = steps;
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n.track.instrument = instrument.into();
    p.nets.insert(group, n);
    p
}

pub fn midi_hz(note: i64) -> f64 {
    440.0 * libm::pow(2.0, (note as f64 - 69.0) / 12.0)
}

pub fn amp_db(x: f64) -> f64 {
    20.0 * libm::log10(x)
}

/// `rmsDB`: mean-square level of `x`, dB.
pub fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let s: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * libm::log10((s / x.len() as f64).max(1e-20))
}

/// `toneDB`: level of the component at `hz` in `x` (Hann-windowed DFT at that one frequency), dB.
pub fn tone_db(x: &[f32], hz: f64, sr: f64) -> f64 {
    let n = x.len();
    let k = 2.0 * core::f64::consts::PI * hz / sr;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * libm::cos(2.0 * core::f64::consts::PI * i as f64 / (n - 1) as f64);
        let s = *v as f64 * w;
        re += s * libm::cos(k * i as f64);
        im -= s * libm::sin(k * i as f64);
    }
    10.0 * libm::log10(((re * re + im * im) / (n * n) as f64).max(1e-30))
}

/// `cycleHarmonic`: amplitude of harmonic `n` of one cycle `x` (a wavetable), normalised.
pub fn cycle_harmonic(x: &[f32], n: usize) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let a = 2.0 * core::f64::consts::PI * (n * i) as f64 / x.len() as f64;
        re += *v as f64 * libm::cos(a);
        im += *v as f64 * libm::sin(a);
    }
    libm::sqrt(re * re + im * im) / x.len() as f64
}

/// `sameSamples == .identical`: the same bits throughout.
pub fn identical(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Correlation of two channels over `a..b`: 1 for a mono or panned source.
pub fn correlation(l: &[f32], r: &[f32], a: usize, b: usize) -> f64 {
    let (mut lr, mut ll, mut rr) = (0.0, 0.0, 0.0);
    for i in a..b {
        let (x, y) = (l[i] as f64, r[i] as f64);
        lr += x * y;
        ll += x * x;
        rr += y * y;
    }
    lr / libm::sqrt(ll * rr).max(1e-30)
}

/// A render of `p` through `o`, `seconds` long in 256-frame blocks, mono.
pub fn render_mono(p: &Project, o: &WaveOptions, sr: f64, seconds: f64) -> Vec<f32> {
    let mut r = WaveRunner::new(sr, o.clone());
    r.load_prepared(WaveRunner::prepare(p, sr, o, 0));
    r.play();
    let mut out = vec![0f32; (seconds * sr) as usize];
    let mut off = 0;
    while off < out.len() {
        let n = 256.min(out.len() - off);
        r.process(&mut out[off..off + n], None, n);
        off += n;
    }
    out
}

/// The same, stereo (left, right).
pub fn render_stereo(p: &Project, o: &WaveOptions, sr: f64, seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let mut r = WaveRunner::new(sr, o.clone());
    r.load_prepared(WaveRunner::prepare(p, sr, o, 0));
    r.play();
    let mut l = vec![0f32; (seconds * sr) as usize];
    let mut rr = l.clone();
    let mut off = 0;
    while off < l.len() {
        let n = 256.min(l.len() - off);
        r.process(&mut l[off..off + n], Some(&mut rr[off..off + n]), n);
        off += n;
    }
    (l, rr)
}

/// `cents`: `f` against `reference`, in cents.
pub fn cents(f: f64, reference: f64) -> f64 {
    1200.0 * libm::log2(f / reference)
}

/// `partialPeak`: frequency of the strongest partial within +-100 cents of `near` (Hann-windowed
/// DFT scanned in 0.5-cent steps).
pub fn partial_peak(x: &[f32], near: f64, sr: f64) -> f64 {
    let n = x.len();
    let tau = 2.0 * core::f64::consts::PI;
    let w: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * libm::cos(tau * i as f64 / (n - 1) as f64))
        .collect();
    let (mut best, mut best_mag) = (near, -1.0);
    let mut c = -100.0;
    while c <= 100.0 {
        let f = near * libm::pow(2.0, c / 1200.0);
        let k = tau * f / sr;
        let (mut re, mut im) = (0.0, 0.0);
        for i in 0..n {
            let s = x[i] as f64 * w[i];
            re += s * libm::cos(k * i as f64);
            im -= s * libm::sin(k * i as f64);
        }
        let mag = re * re + im * im;
        if mag > best_mag {
            best_mag = mag;
            best = f;
        }
        c += 0.5;
    }
    best
}
