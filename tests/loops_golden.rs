//! Swift goldens for the loop channel's DSP (`tools/paritydump/loops.swift`, section "loops"):
//! the clip FX chain and its parameters, the safety limiter, WSOLA, and `SampleLevel`. The
//! bank and the master bus are compared in `djhost/tests/loops_golden.rs` from the same file.
//!
//! This is parity form (b) of the port rules: Swift rendered a fixed source through each
//! setting, and Rust renders the same bits and compares per-block peak and RMS, a float32 hash
//! of the whole render and a decimated waveform. Rust has to be bit-identical wherever it can
//! be; where it is not, the tolerance below is what was measured and says why (Apple's libm and
//! the `libm` crate differ in the last bit of `tanhf`, `pow`, `tan`; vDSP_conv sums in another
//! order than a plain loop).

mod common;
mod loops_support;

use common::*;
use djrust::deck::level_match::{LevelMatch, SampleLevel};
use djrust::deck::turntable::SampleBuffer;
use djrust::fx::loop_fx::{halfband_taps, LoopFxLimiter, LoopFxParams, LoopFxVoice};
use djrust::fx::time_stretch;
use djrust::json::JsonValue;
use djrust::session::types::LoopFx;
use loops_support::*;

const SR: f64 = 48_000.0;

fn golden() -> JsonValue {
    fixture("loops")
}

fn fx_of(v: &JsonValue) -> LoopFx {
    LoopFx::from_json(Some(v))
}

/// The cases whose render may differ from Swift's in the last bit, and by how much (waveform,
/// per-block peak, per-block RMS), measured: only the `soft` shaper, whose `tanhf` is Apple's
/// libm in Swift. At most 3e-8 on the waveform and 6e-8 on a block peak, 1 ulp of an f32 near
/// 0.5, and 2e-9 on an RMS; the bound is 4x that. Every other case must be bit-identical.
const SOFT_CASES: [&str; 5] = [
    "fuzz",
    "loud",
    "soft-24-full",
    "script-smooth",
    "script-on-off",
];
const SOFT_TOLERANCE: (f64, f64, f64) = (1.2e-7, 2.4e-7, 1e-8);

/// WSOLA's cases (the long ones) are not bit-identical: they differ by 1 ulp at most (measured
/// 9e-8 on the waveform, 6e-8 on a block peak, 5e-9 on an RMS; the bound is 2x that). The
/// candidates are the window's `cosf` (Apple's) and `vDSP_conv`'s summation order against a
/// plain loop; neither was isolated. The offset the correlation picks is the same in every case
/// (a different one would move whole frames, an error of the order of the signal).
const STRETCH_TOLERANCE: (f64, f64, f64) = (2e-7, 1.2e-7, 1e-8);

// LoopFxParams: every field of the targets the audio thread uses, for a grid of settings
#[test]
fn loop_fx_params_match_swifts_bit_for_bit() {
    let g = golden();
    let fx = g.get("fx").unwrap();
    let mut d = Diffs::default();
    let mut libm_off = 0;
    for p in rows(fx, "params") {
        let settings = fx_of(p.get("fx").unwrap());
        let got = LoopFxParams::new(&settings, SR);
        let tag = settings.summary();
        d.eq(
            format!("{tag} active"),
            got.active,
            p.get("active").unwrap().as_bool().unwrap(),
        );
        d.eq(format!("{tag} mode"), got.mode as i64, i(p, "mode"));
        d.eq(format!("{tag} hold"), got.hold as i64, i(p, "hold"));
        for (name, v) in [
            ("drive", got.drive),
            ("amount", got.amount),
            ("pre", got.pre),
            ("norm", got.norm),
            ("step", got.step),
            ("toneG", got.tone_g),
            ("toneWet", got.tone_wet),
            ("mix", got.mix),
            ("trim", got.trim),
            ("comp", got.comp),
            ("auto", got.auto),
        ] {
            if bits32(v) != s(p, name) {
                libm_off += 1;
            }
            d.eq(format!("{tag} {name}"), bits32(v), s(p, name).to_string());
        }
    }
    eprintln!("params that differ in the last bit: {libm_off}");
    d.finish("LoopFxParams");
}

// LoopFX presets, summaries and the value type's isActive
#[test]
fn presets_and_summaries_match_swifts() {
    let g = golden();
    let fx = g.get("fx").unwrap();
    let mut d = Diffs::default();
    let presets = LoopFx::presets();
    let want = rows(fx, "presets");
    d.eq("preset count", presets.len(), want.len());
    for ((name, f), w) in presets.iter().zip(want) {
        d.eq(
            format!("{name} name"),
            name.to_string(),
            s(w, "name").to_string(),
        );
        d.eq(
            format!("{name} fx"),
            f.json().js_stringified(),
            w.get("fx").unwrap().js_stringified(),
        );
        d.eq(
            format!("{name} summary"),
            f.summary(),
            s(w, "summary").to_string(),
        );
        d.eq(
            format!("{name} active"),
            f.is_active(),
            w.get("active").unwrap().as_bool().unwrap(),
        );
        d.eq(
            format!("{name} presetName"),
            f.preset_name().unwrap_or("").to_string(),
            s(w, "presetName").to_string(),
        );
    }
    for w in rows(fx, "summaries") {
        let f = fx_of(w.get("fx").unwrap());
        d.eq(f.summary(), f.summary(), s(w, "summary").to_string());
        d.eq(
            format!("{} active", f.summary()),
            f.is_active(),
            w.get("active").unwrap().as_bool().unwrap(),
        );
    }
    d.finish("LoopFX presets");
}

/// Run `script` (block, setting) over the source in 512-frame blocks, as the dump does.
fn run_voice(script: &[(usize, LoopFx)]) -> (Vec<f32>, Vec<f32>, Vec<bool>) {
    let (mut l, mut r) = loop_source(32_768, 0);
    let mut voice = LoopFxVoice::new(SR);
    let mut running = Vec::new();
    let (mut block, mut offset) = (0usize, 0usize);
    while offset < l.len() {
        let n = 512.min(l.len() - offset);
        for (b, fx) in script {
            if *b == block {
                voice.set(&LoopFxParams::new(fx, SR), block > 0);
            }
        }
        voice.process(&mut l[offset..offset + n], &mut r[offset..offset + n], n);
        running.push(voice.running());
        offset += n;
        block += 1;
    }
    (l, r, running)
}

// LoopFxVoice: the chain on a fixed source, settings changed while it sounds
#[test]
fn the_clip_fx_chain_renders_what_swifts_does() {
    let g = golden();
    let cases = rows(g.get("fx").unwrap(), "cases");
    assert!(cases.len() >= 17);
    let mut worst = Measured::default();
    let mut inexact = Vec::new();
    for c in cases {
        let name = s(c, "name");
        let script: Vec<(usize, LoopFx)> = rows(c, "script")
            .iter()
            .map(|st| (i(st, "block") as usize, fx_of(st.get("fx").unwrap())))
            .collect();
        let (l, r, running) = run_voice(&script);
        let want_running: Vec<bool> = rows(c, "running")
            .iter()
            .map(|b| b.as_bool().unwrap())
            .collect();
        assert_eq!(
            running, want_running,
            "{name}: when the chain is in the path"
        );
        let m = compare(c, &l, &r);
        assert!(m.frames_match, "{name}");
        eprintln!(
            "fx {name}: exact {} wave {:.3e} peak {:.3e} rms {:.3e}",
            m.exact, m.max_wave, m.max_peak, m.max_rms
        );
        if !m.exact {
            inexact.push(name.to_string());
            let tol = SOFT_TOLERANCE;
            assert!(
                SOFT_CASES.contains(&name),
                "{name}: expected bit-identical, off by {m:?}"
            );
            assert!(
                m.max_wave <= tol.0 && m.max_peak <= tol.1 && m.max_rms <= tol.2,
                "{name}: not within the measured tolerance {tol:?}: {m:?}"
            );
        }
        worst.max_wave = worst.max_wave.max(m.max_wave);
    }
    eprintln!(
        "fx: inexact {inexact:?}, worst waveform error {:.3e}",
        worst.max_wave
    );
}

// The halfband's 24 side taps sum to exactly 0.5 (unity at DC both ways), are symmetric about the
// centre tap, and alternate in sign from the centre out (a sinc under a Kaiser window)
#[test]
fn the_halfband_has_unity_dc_gain() {
    let t = halfband_taps();
    let sum: f64 = t.iter().map(|&v| v as f64).sum();
    assert!((sum - 0.5).abs() < 1e-7, "{sum}");
    for k in 0..12 {
        assert_eq!(t[k], t[23 - k], "tap {k}");
    }
    assert!(
        t[11] > 0.0 && t[10] < 0.0 && t[9] > 0.0 && t[12] > 0.0,
        "{:?}",
        &t[8..16]
    );
    assert!(t[11].abs() > t[10].abs() && t[10].abs() > t[9].abs());
}

// LoopFxLimiter: instant attack, 120 ms release, mono and stereo
#[test]
fn the_limiter_matches_swifts() {
    let g = golden();
    let lim = g.get("limiter").unwrap();
    assert_eq!(f(lim, "ceiling"), LoopFxLimiter::CEILING as f64);
    let (l0, r0) = loop_source(24_576, 1);
    for c in rows(lim, "cases") {
        let gain = f(c, "gain") as f32;
        let mut l: Vec<f32> = l0.iter().map(|v| v * gain).collect();
        let mut r: Vec<f32> = r0.iter().map(|v| v * gain).collect();
        let mut limiter = LoopFxLimiter::new(SR);
        let mut gains = Vec::new();
        let mut offset = 0;
        while offset < l.len() {
            let n = 512.min(l.len() - offset);
            limiter.process(
                &mut l[offset..offset + n],
                Some(&mut r[offset..offset + n]),
                n,
            );
            gains.push(limiter.gain());
            offset += n;
        }
        let m = compare(c, &l, &r);
        let want_gains = nums(c, "gains");
        let gain_err = gains
            .iter()
            .zip(&want_gains)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        let mut mono: Vec<f32> = l0.iter().map(|v| v * gain).collect();
        let mut lim2 = LoopFxLimiter::new(SR);
        let n = mono.len();
        lim2.process(&mut mono, None, n);
        eprintln!(
            "limiter x{gain}: exact {} gain err {gain_err:.3e} mono {} (gain err {:.3e})",
            m.exact,
            hash_floats(&mono) == s(c, "monoHash"),
            (lim2.gain() - f(c, "monoGain")).abs()
        );
        assert!(m.exact, "x{gain}: {m:?}");
        assert!(gain_err == 0.0, "x{gain}");
        assert_eq!(hash_floats(&mono), s(c, "monoHash"), "x{gain} mono");
        assert_eq!(limiter.idle(), c.get("idle").unwrap().as_bool().unwrap());
    }
}

// WSOLA at several ratios (time_stretch)
#[test]
fn wsola_matches_swifts() {
    let g = golden();
    let st = g.get("stretch").unwrap();
    assert_eq!(time_stretch::frame_length(SR) as i64, i(st, "frameLength"));
    let mut worst = 0f64;
    let mut exact = 0;
    for c in rows(st, "cases") {
        let name = s(c, "name");
        let ratio = f(c, "ratio");
        let n = i(c, "inFrames") as usize;
        let (l, r) = match name {
            n_ if n_.starts_with("source-") => {
                let (l, r) = loop_source(n, 0);
                (l, r)
            }
            n_ if n_.starts_with("other-") => loop_source(n, 1),
            n_ if n_.starts_with("mono-") => {
                let (l, _) = loop_source(n, 0);
                (l.clone(), l)
            }
            n_ if n_.starts_with("tone-") => {
                let t = rotation_tone(n);
                (t.clone(), t)
            }
            n_ if n_.starts_with("short-") => {
                let (l, r) = loop_source(24_000, 0);
                (l[..n].to_vec(), r[..n].to_vec())
            }
            _ => panic!("unknown case {name}"),
        };
        let (ol, or) = time_stretch::wsola(&l, &r, ratio, SR);
        assert_eq!(ol.len() as i64, i(c, "outFrames"), "{name}: length");
        let m = compare(c, &ol, &or);
        eprintln!(
            "stretch {name}: exact {} wave {:.3e} peak {:.3e} rms {:.3e}",
            m.exact, m.max_wave, m.max_peak, m.max_rms
        );
        if m.exact {
            exact += 1;
        } else {
            worst = worst.max(m.max_wave);
            let tol = STRETCH_TOLERANCE;
            assert!(
                !name.starts_with("short-"),
                "{name}: the resampled fallback has no vDSP in it, expected bit-identical: {m:?}"
            );
            assert!(
                m.max_wave <= tol.0 && m.max_peak <= tol.1 && m.max_rms <= tol.2,
                "{name}: not within the measured tolerance {tol:?}: {m:?}"
            );
        }
    }
    eprintln!("stretch: {exact} bit-identical; worst waveform error elsewhere {worst:.3e}");
}

/// `dumpStretch`'s rotation tone.
fn rotation_tone(n: usize) -> Vec<f32> {
    let (mut x, mut y) = (0f32, 0.5f32);
    (0..n)
        .map(|_| {
            let nx = 0.998_795_46f32 * x - 0.049_067_674f32 * y;
            let ny = 0.049_067_674f32 * x + 0.998_795_46f32 * y;
            x = nx;
            y = ny;
            y
        })
        .collect()
}

/// `dumpLevel`'s buffers.
fn level_buffer(name: &str, frames: usize) -> SampleBuffer {
    let scaled = |src: &(Vec<f32>, Vec<f32>), g: f32| {
        SampleBuffer::new(
            src.0.iter().map(|v| v * g).collect(),
            Some(src.1.iter().map(|v| v * g).collect()),
            SR,
        )
    };
    let a = loop_source(96_000, 0);
    match name {
        "full-1.0" => scaled(&a, 1.0),
        "full-0.1" => scaled(&a, 0.1),
        "full-0.02" => scaled(&a, 0.02),
        "full-0.001" => scaled(&a, 0.001),
        "full-2.0" => scaled(&a, 2.0),
        "other" => scaled(&loop_source(72_000, 1), 0.3),
        "mono" => SampleBuffer::new(a.0.iter().map(|v| v * 0.2).collect(), None, SR),
        "spiky" => {
            let mut l: Vec<f32> = a.0.iter().map(|v| v * 0.01).collect();
            l[48_000] = 0.95;
            SampleBuffer::new(l.clone(), Some(l), SR)
        }
        "bass" => {
            let l: Vec<f32> = (0..96_000)
                .map(|i| ((i % 960) as i64 - 480) as f32 / 4800.0)
                .collect();
            SampleBuffer::new(l.clone(), Some(l), SR)
        }
        "hats" => {
            let noise = lcg_noise(777, 96_000);
            let l: Vec<f32> = (0..96_000)
                .map(|i| (if i % 2 == 0 { 1.0f32 } else { -1.0 }) * noise[i].abs() * 0.05)
                .collect();
            SampleBuffer::new(l.clone(), Some(l), SR)
        }
        "silence" => {
            let l = vec![0f32; 48_000];
            SampleBuffer::new(l, None, SR)
        }
        "short" => scaled(&(a.0[..2000].to_vec(), a.1[..2000].to_vec()), 0.3),
        _ => panic!("unknown case {name} ({frames})"),
    }
}

// SampleLevel: loudness, peak, band shares, partial, target and auto-gain
#[test]
fn sample_level_matches_swifts() {
    let g = golden();
    let lv = g.get("level").unwrap();
    assert_eq!(f(lv, "referenceLUFS"), LevelMatch::REFERENCE_LUFS);
    assert_eq!(f(lv, "partialOffsetDB"), LevelMatch::PARTIAL_OFFSET_DB);
    assert_eq!(f(lv, "maxGainDB"), LevelMatch::MAX_GAIN_DB);
    assert_eq!(
        f(lv, "samplePeakCeilingDB"),
        LevelMatch::SAMPLE_PEAK_CEILING_DB
    );
    assert_eq!(
        f(lv, "targetLUFSTechno"),
        LevelMatch::target_lufs(Some("techno"))
    );
    let mut d = Diffs::default();
    let mut worst = 0f64;
    for c in rows(lv, "cases") {
        let name = s(c, "name");
        let b = level_buffer(name, i(c, "frames") as usize);
        assert_eq!(b.length() as i64, i(c, "frames"), "{name}");
        let got = SampleLevel::measure(&b);
        let tag = |k: &str| format!("{name} {k}");
        match c.get("lufs").and_then(JsonValue::as_f64) {
            Some(want) => {
                let lufs = got.lufs.expect("a loudness");
                worst = worst.max((lufs - want).abs());
                d.eq(tag("lufs"), lufs.to_bits(), want.to_bits());
            }
            None => d.eq(tag("lufs"), got.lufs.is_none(), true),
        }
        for (k, v) in [
            ("peakDB", got.peak_db),
            ("lowShare", got.low_share),
            ("highShare", got.high_share),
            ("targetLUFS", got.target_lufs()),
            ("autoGainDB", got.auto_gain_db()),
        ] {
            let want = f(c, k);
            worst = worst.max((v - want).abs());
            d.eq(tag(k), v.to_bits(), want.to_bits());
        }
        d.eq(
            tag("partial"),
            got.partial(),
            c.get("partial").unwrap().as_bool().unwrap(),
        );
    }
    eprintln!("level: worst numeric difference {worst:.3e}");
    d.finish("SampleLevel");
}
