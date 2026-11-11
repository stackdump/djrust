//! The master chain against Chrome rendering beats' own Tone nodes (mirrors the Swift
//! `MasterChainChromeTests`), plus the closed forms of `MasterChainTests` / `ToneParamTests`.
//!
//! Oracle for the Chrome half: Chrome rendering beats' own master nodes (real Tone 15.1.22,
//! built as tone-engine.js builds them, moved by its setters) in an OfflineAudioContext, one
//! node at a time: beats-btw-dj `scripts/master-chain-chrome-reference.html` ->
//! `fixtures/js/master-chain-chrome.json`. The chain runs here with its rest bypass off (Tone's
//! nodes run at rest too) and only the stage under test, on the same input, with the same setter
//! calls at the same render quanta.

use djrust::fx::{MasterChain, MasterFxKey, MasterParams, Stage, ToneParam};
use djrust::json::JsonValue;

const SR: f64 = 48_000.0;

fn fixture() -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/js/master-chain-chrome.json",
        env!("CARGO_MANIFEST_DIR")
    );
    JsonValue::parse(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn base64(s: &str) -> Vec<u8> {
    let mut out = vec![];
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

fn decode_float32(s: &str) -> Vec<f32> {
    base64(s)
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// The page's input (two tones and LCG noise per channel), as Float32.
fn input(n: usize, sr: f64) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut seed: u32 = 12345;
    let tau = 2.0 * std::f64::consts::PI;
    for i in 0..n {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let noise = (seed as f64 / 4_294_967_296.0) * 2.0 - 1.0;
        let t = i as f64 / sr;
        l[i] =
            (0.3 * (tau * 220.0 * t).sin() + 0.2 * (tau * 1375.0 * t).sin() + 0.1 * noise) as f32;
        r[i] =
            (0.3 * (tau * 330.0 * t).sin() + 0.15 * (tau * 2750.0 * t).sin() + 0.1 * noise) as f32;
    }
    (l, r)
}

fn stage(name: &str) -> Stage {
    match name {
        "lowpass" => Stage::Lowpass,
        "highpass" => Stage::Highpass,
        "phaser" => Stage::Phaser,
        "crusher" => Stage::Crusher,
        "distortion" => Stage::Distortion,
        _ => Stage::Pitch,
    }
}

/// The chain's output for a scenario: 128-frame quanta, each op applied before its quantum.
fn render_scenario(sc: &JsonValue, n: usize, sr: f64) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = input(n, sr);
    let mut chain = MasterChain::new(sr);
    chain.set_rest_bypass(false);
    chain.solo = Some(stage(sc.get("name").unwrap().as_str().unwrap()));
    let ops: Vec<(usize, MasterFxKey, i32)> = sc
        .get("ops")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o.at(0).unwrap().as_f64().unwrap() as usize,
                MasterFxKey::from_id(o.at(1).unwrap().as_str().unwrap()).unwrap(),
                o.at(2).unwrap().as_f64().unwrap() as i32,
            )
        })
        .collect();
    let mut start = 0;
    while start < n {
        let m = 128.min(n - start);
        for &(f, key, v) in &ops {
            if f == start {
                let t = chain.time_at(0);
                chain.apply(key, v, t);
            }
        }
        chain.process(&mut l[start..], Some(&mut r[start..]), 0, m);
        chain.finish(m);
        start += m;
    }
    (l, r)
}

/// Magnitude spectrum (Hann window, every 4th bin up to Nyquist) of a stretch.
fn spectrum(x: &[f32]) -> Vec<f64> {
    let n = x.len();
    let tau = 2.0 * std::f64::consts::PI;
    (1..n / 2)
        .step_by(4)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, v) in x.iter().enumerate() {
                let h = 0.5 - 0.5 * (tau * i as f64 / n as f64).cos();
                let a = tau * k as f64 * i as f64 / n as f64;
                re += *v as f64 * h * a.cos();
                im -= *v as f64 * h * a.sin();
            }
            (re * re + im * im).sqrt()
        })
        .collect()
}

/// Normalised correlation of two spectra (1: the same shape).
fn similarity(a: &[f64], b: &[f64]) -> f64 {
    let ab: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let aa: f64 = a.iter().map(|x| x * x).sum();
    let bb: f64 = b.iter().map(|x| x * x).sum();
    ab / (aa * bb).sqrt()
}

/// RMS of each 128-frame stretch.
fn envelope(x: &[f32]) -> Vec<f64> {
    x.chunks(128)
        .map(|s| (s.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / s.len() as f64).sqrt())
        .collect()
}

/// Worst |Rust - Chrome| over the windows, per scenario. The biquads, the crusher and the
/// distortion (with their wet ramps and Tone's -56 dB leak at wet 0) sample for sample within
/// 1e-4. The phaser and the pitch shifter (per-sample allpass coefficients from an LFO, delay
/// lines swept by Chrome's band-limited wavetables) within 2e-3, their 128-frame RMS envelopes
/// within 0.1 % and their spectra alike (correlation > 0.9999).
#[test]
fn matches_chrome() {
    let f = fixture();
    let sr = f.get("sampleRate").unwrap().as_f64().unwrap();
    let n = f.get("frames").unwrap().as_f64().unwrap() as usize;
    for name in [
        "lowpass",
        "highpass",
        "crusher",
        "distortion",
        "phaser",
        "pitch",
    ] {
        let sc = f
            .get("scenarios")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s.get("name").and_then(JsonValue::as_str) == Some(name))
            .unwrap();
        let (l, r) = render_scenario(sc, n, sr);
        let (mut worst, mut worst_env, mut least_similar) = (0.0f64, 0.0f64, 1.0f64);
        let mut report = vec![];
        for w in sc.get("windows").unwrap().as_array().unwrap() {
            let s = w.get("start").unwrap().as_f64().unwrap() as usize;
            for (mine, theirs) in [
                (&l, decode_float32(w.get("left").unwrap().as_str().unwrap())),
                (
                    &r,
                    decode_float32(w.get("right").unwrap().as_str().unwrap()),
                ),
            ] {
                let a = &mine[s..s + theirs.len()];
                let d = a
                    .iter()
                    .zip(&theirs)
                    .fold(0.0f64, |m, (x, y)| m.max((*x as f64 - *y as f64).abs()));
                let (ea, eb) = (envelope(a), envelope(&theirs));
                let de = ea
                    .iter()
                    .zip(&eb)
                    .fold(0.0f64, |m, (x, y)| m.max((x - y).abs() / y.max(0.05)));
                worst = worst.max(d);
                worst_env = worst_env.max(de);
                let sim = if name == "pitch" || name == "phaser" {
                    similarity(&spectrum(a), &spectrum(&theirs))
                } else {
                    1.0
                };
                least_similar = least_similar.min(sim);
                report.push(format!("{s}: {d} env {de} spectrum {sim}"));
            }
        }
        eprintln!("chrome {name}: max |d| {worst}, envelope {worst_env}, spectrum {least_similar}");
        match name {
            "pitch" | "phaser" => {
                assert!(worst < 2e-3, "{name}: {worst}: {report:?}");
                assert!(
                    worst_env < 1e-3 && least_similar > 0.9999,
                    "{name}: {report:?}"
                );
            }
            _ => assert!(worst < 1e-4, "{name}: {worst}: {report:?}"),
        }
    }
}

// ---- closed forms (MasterChainTests, ToneParamTests) ----

/// Run `seconds` of `signal(i)` (mono) through `chain`.
fn run(chain: &mut MasterChain, seconds: f64, mut signal: impl FnMut(usize) -> f32) -> Vec<f32> {
    let n = (seconds * SR) as usize;
    let mut out = vec![0f32; n];
    let mut buf = vec![0f32; 128];
    let mut i = 0;
    while i < n {
        let m = 128.min(n - i);
        for k in 0..m {
            buf[k] = signal(i + k);
        }
        chain.process(&mut buf, None, 0, m);
        chain.finish(m);
        out[i..i + m].copy_from_slice(&buf[..m]);
        i += m;
    }
    out
}

fn stereo(
    chain: &mut MasterChain,
    seconds: f64,
    signal: impl Fn(usize) -> (f32, f32),
) -> (Vec<f32>, Vec<f32>) {
    let n = (seconds * SR) as usize;
    let (mut ol, mut or) = (vec![0f32; n], vec![0f32; n]);
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    let mut i = 0;
    while i < n {
        let m = 128.min(n - i);
        for k in 0..m {
            (l[k], r[k]) = signal(i + k);
        }
        chain.process(&mut l, Some(&mut r), 0, m);
        chain.finish(m);
        ol[i..i + m].copy_from_slice(&l[..m]);
        or[i..i + m].copy_from_slice(&r[..m]);
        i += m;
    }
    (ol, or)
}

fn sine(f: f64, amp: f64) -> impl Fn(usize) -> f32 {
    move |i| (amp * (2.0 * std::f64::consts::PI * f * i as f64 / SR).sin()) as f32
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

#[test]
fn tone_exponential_ramp_is_web_audios() {
    let mut p = ToneParam::new(20000.0, true, SR);
    p.cancel_scheduled_values(1.0);
    p.ramp_to(500.0, 0.1, 1.0);
    assert_eq!(p.value_at(0.5), 20000.0);
    assert_eq!(p.value_at(1.0), 20000.0);
    // V0 (V1/V0)^((t - T0) / (T1 - T0))
    for f in [0.1, 0.25, 0.5, 0.9] {
        let want = 20000.0 * (500.0f64 / 20000.0).powf(f);
        assert!((p.value_at(1.0 + 0.1 * f) - want).abs() <= 1e-9 * want);
    }
    assert_eq!(p.value_at(1.1), 500.0);
    assert_eq!(p.value_at(3.0), 500.0);
    assert!(p.is_settled(1.1) && !p.is_settled(1.05));
}

#[test]
fn tone_linear_ramp_starts_from_min_output_not_zero() {
    // Param.js setRampPoint: a ramp point of exactly 0 becomes 1e-7
    let mut w = ToneParam::new(0.0, false, SR);
    w.ramp_to(1.0, 0.05, 2.0);
    assert_eq!(w.value_at(2.0), 1e-7);
    assert!((w.value_at(2.025) - (1e-7 + (1.0 - 1e-7) * 0.5)).abs() < 1e-15);
    w.ramp_to(0.0, 0.05, 3.0);
    assert_eq!(w.value_at(3.05), 0.0, "a linear ramp TO 0 ends at 0");
}

#[test]
fn tone_cancel_then_ramp_jumps_back_to_the_cut_ramps_start() {
    // tone-engine.js setLowpassFreq: cancelScheduledValues(now) then rampTo(f, 0.1). Cut 50 ms
    // into a 20 kHz -> 1 kHz ramp, Tone's timeline (and the native param) restart from 20 kHz.
    let mut p = ToneParam::new(20000.0, true, SR);
    p.cancel_scheduled_values(1.0);
    p.ramp_to(1000.0, 0.1, 1.0);
    let mid = p.value_at(1.05);
    assert!((mid - 20000.0 * 0.05f64.powf(0.5)).abs() < 1e-6);
    p.cancel_scheduled_values(1.05);
    p.ramp_to(200.0, 0.1, 1.05);
    assert_eq!(p.value_at(1.05), 20000.0);
    assert!((p.value_at(1.1) - 20000.0 * 0.01f64.powf(0.5)).abs() < 1e-6);
}

#[test]
fn tone_ramp_without_cancel_holds_the_current_value() {
    // phaser frequency: rampTo with no cancel -> cancelAndHold at the current value
    let mut p = ToneParam::new(1.0, true, SR);
    p.ramp_to(4.0, 0.1, 1.0);
    let at = p.value_at(1.05);
    p.ramp_to(0.1, 0.1, 1.05);
    assert!((p.value_at(1.05) - at).abs() < 1e-12);
    assert!((p.value_at(1.1) - at * (0.1 / at).powf(0.5)).abs() < 1e-12);
}

#[test]
fn tone_pruning_keeps_what_the_future_needs() {
    let mut p = ToneParam::new(20000.0, true, SR);
    let mut t = 1.0;
    for i in 0..500 {
        p.cancel_scheduled_values(t);
        p.ramp_to(if i % 2 == 0 { 500.0 } else { 5000.0 }, 0.1, t);
        let before = p.value_at(t + 0.05);
        p.prune(t + 0.05);
        assert_eq!(p.value_at(t + 0.05), before);
        assert!(p.count() <= 6);
        t += 0.12;
    }
}

#[test]
fn a_neutral_chain_is_bit_transparent() {
    let mut chain = MasterChain::new(SR);
    let gen = |seed: &mut u64| {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((((*seed >> 11) as i64) % 2_000_000) as f32) / 1_000_000.0
    };
    let mut s1 = 7u64;
    let mut s2 = 7u64;
    let copy: Vec<f32> = (0..(0.5 * SR) as usize).map(|_| gen(&mut s2)).collect();
    assert!(chain.is_idle(0));
    let out = run(&mut chain, 0.5, |_| gen(&mut s1));
    assert_eq!(
        out.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        copy.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
}

/// |H(f)|^2 of a Web Audio lowpass/highpass biquad (Q in dB).
fn pass_mag2(lowpass: bool, fc: f64, q_db: f64, f: f64) -> f64 {
    let w0 = 2.0 * std::f64::consts::PI * fc / SR;
    let cw = w0.cos();
    let alpha = w0.sin() / (2.0 * 10f64.powf(q_db / 20.0));
    let (b0, b1) = if lowpass {
        ((1.0 - cw) / 2.0, 1.0 - cw)
    } else {
        ((1.0 + cw) / 2.0, -(1.0 + cw))
    };
    let (a0, a1, a2) = (1.0 + alpha, -2.0 * cw, 1.0 - alpha);
    let w = 2.0 * std::f64::consts::PI * f / SR;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let nr = b0 + b1 * c1 + b0 * c2;
    let ni = -(b1 * s1 + b0 * s2);
    let dr = a0 + a1 * c1 + a2 * c2;
    let di = -(a1 * s1 + a2 * s2);
    (nr * nr + ni * ni) / (dr * dr + di * di)
}

#[test]
fn filters_match_the_web_audio_response_cubed() {
    // Tone.Filter rolloff -48 = 3 cascaded Web Audio biquads, Q 1 dB: the steady-state gain on
    // a sine is |H(f)|^3
    for (key, slider, lowpass) in [
        (MasterFxKey::LpFreq, 50, true),
        (MasterFxKey::HpFreq, 60, false),
    ] {
        let fc = if lowpass {
            MasterParams::lp_freq(slider as f64)
        } else {
            MasterParams::hp_freq(slider as f64)
        };
        for f in [200.0, 700.0, 1414.0, 3000.0, 6000.0] {
            let mut chain = MasterChain::new(SR);
            chain.apply(key, slider, 0.0);
            let y = run(&mut chain, 0.6, sine(f, 0.5));
            let x: Vec<f32> = (0..y.len()).map(|i| sine(f, 0.5)(i)).collect();
            let a = (0.4 * SR) as usize;
            let got = rms(&y[a..]) / rms(&x[a..]);
            let want = pass_mag2(lowpass, fc, 1.0, f).powf(MasterChain::FILTER_STAGES as f64 / 2.0);
            assert!(
                (got - want).abs() <= 2e-3 * want.max(1e-3),
                "{key:?} {slider} at {f} Hz: {got} vs {want}"
            );
        }
    }
}

#[test]
fn distortion_curve_is_tones_over_1024_points() {
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::Distortion, 20, 0.0);
    let k = 20.0;
    let deg = std::f64::consts::PI / 180.0;
    let tone = |x: f64| -> f64 {
        if x.abs() < 0.001 {
            0.0
        } else {
            (((3.0 + k) * x * 20.0 * deg) / (std::f64::consts::PI + k * x.abs())) as f32 as f64
        }
    };
    // grid points are the curve, in between is linear interpolation, beyond +-1 the ends
    for i in [0, 1, 300, 511, 512, 700, 1022, 1023] {
        let x = i as f64 / 1023.0 * 2.0 - 1.0;
        assert!((chain.shape(x as f32) - tone(x)).abs() < 1e-6, "point {i}");
    }
    let i = 800;
    let xa = i as f64 / 1023.0 * 2.0 - 1.0;
    let xb = (i + 1) as f64 / 1023.0 * 2.0 - 1.0;
    let mid = ((xa + xb) / 2.0) as f32;
    assert!((chain.shape(mid) - (tone(xa) + tone(xb)) / 2.0).abs() < 1e-6);
    assert_eq!(chain.shape(1.5), tone(1.0));
    assert_eq!(chain.shape(-3.0), tone(-1.0));
    // fully wet once the 50 ms ramp is over: y = curve(x)
    let y = run(&mut chain, 0.2, sine(330.0, 0.8));
    for n in [(0.1 * SR) as usize, (0.15 * SR) as usize + 17] {
        assert!((y[n] as f64 - chain.shape(sine(330.0, 0.8)(n))).abs() < 1e-6);
    }
}

#[test]
fn crusher_quantises_to_the_bit_step() {
    // slider 80 -> amount 0.8 -> bits max(1, round(16 - 12)) = 4 -> step 1/8
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::CrushBits, 80, 0.0);
    let y = run(&mut chain, 0.2, sine(97.0, 0.9));
    let step = 0.5f64.powf(3.0);
    for n in ((0.1 * SR) as usize..y.len()).step_by(37) {
        let x = sine(97.0, 0.9)(n) as f64;
        let want = step * (x / step + 0.5).floor();
        assert!(
            (y[n] as f64 - want).abs() < 1e-6,
            "sample {n}: {} vs {want}",
            y[n]
        );
    }
}

#[test]
fn wet_is_equal_power() {
    // crusher at 1 bit (step 1): a small signal crushes to 0, so the output is the dry part,
    // x*cos(w*pi/2), along the wet's linear ramp 1e-7 -> 1 over 50 ms
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::CrushBits, 100, 0.0); // bits max(1, round(16 - 15)) = 1
    let y = run(&mut chain, 0.06, |_| 0.25);
    for n in [0usize, 300, 1200, 1800, 2399] {
        let t = n as f64 / SR;
        let w = 1e-7 + (1.0 - 1e-7) * (t / 0.05);
        assert!(
            (y[n] as f64 - 0.25 * (w * std::f64::consts::PI / 2.0).cos()).abs() < 1e-6,
            "sample {n}"
        );
    }
    assert!(y[2500].abs() < 1e-6);
}

#[test]
fn phaser_allpasses_keep_the_level() {
    // fully wet (slider 100: wet 1), the phaser is ten allpasses: the level of a tone is kept
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::PhaserWet, 100, 0.0);
    chain.apply(MasterFxKey::PhaserDepth, 100, 0.0);
    let (l, r) = stereo(&mut chain, 1.0, |n| {
        (sine(523.0, 0.5)(n), sine(523.0, 0.5)(n))
    });
    let a = (0.2 * SR) as usize;
    let refv = rms(&(0..SR as usize)
        .map(|i| sine(523.0, 0.5)(i))
        .collect::<Vec<_>>()[a..]);
    assert!((rms(&l[a..]) / refv - 1.0).abs() < 0.01);
    assert!((rms(&r[a..]) / refv - 1.0).abs() < 0.01);
    // the two channels' LFOs are half a cycle apart: the outputs differ
    assert!(l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 0.01));
}

#[test]
fn pitch_shift_moves_a_sine_by_tones_ratio() {
    // A tap reads x(t - d(t)) with d a sawtooth: the tone comes out at 440 (1 - d') during each
    // grain, and as the two taps' grains repeat at the LFO rate W the spectrum is lines at
    // 440 + 2kW. The strongest line is the one nearest the grain frequency. d' = -+1.2 factor /
    // sawPeak (Chrome's table normalisation), so this pins both the rate and the slope.
    for p in [12.0f64, 7.0, -12.0] {
        let factor = if p < 0.0 {
            2f64.powf((p - 1.0) / 12.0) + 1.0
        } else {
            2f64.powf(p / 12.0) - 1.0
        };
        let omega = factor * 1.2 / MasterChain::WINDOW;
        let slope = (if p < 0.0 { 1.0 } else { -1.0 }) * 1.2 * factor / MasterChain::SAW_PEAK;
        let grain = 440.0 * (1.0 - slope); // signed
        let mut chain = MasterChain::new(SR);
        chain.apply(MasterFxKey::MasterPitch, p as i32, 0.0);
        let full = run(&mut chain, 1.0, sine(440.0, 0.5));
        let y = &full[(0.1 * SR) as usize..];
        let level = |f: f64| -> f64 {
            let (mut re, mut im) = (0.0, 0.0);
            let w = 2.0 * std::f64::consts::PI * f.abs() / SR;
            for (i, v) in y.iter().enumerate() {
                let h = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / y.len() as f64).cos();
                re += *v as f64 * h * (w * i as f64).cos();
                im -= *v as f64 * h * (w * i as f64).sin();
            }
            (re * re + im * im).sqrt()
        };
        let mut lines = vec![];
        for k in -12i32..=12 {
            let f = 440.0 + 2.0 * k as f64 * omega;
            if f.abs() > 60.0 && f.abs() < 1500.0 {
                lines.push(f);
            }
        }
        let levels: Vec<f64> = lines.iter().map(|&f| level(f)).collect();
        let nearest = *lines
            .iter()
            .min_by(|a, b| (*a - grain).abs().partial_cmp(&(*b - grain).abs()).unwrap())
            .unwrap();
        let li = (0..levels.len())
            .max_by(|&a, &b| levels[a].partial_cmp(&levels[b]).unwrap())
            .unwrap();
        assert_eq!(
            lines[li],
            nearest,
            "pitch {p}: loudest line {} Hz, want {} (grain {})",
            lines[li].abs(),
            nearest.abs(),
            grain.abs()
        );
        assert!(level(nearest + omega) < 0.2 * level(nearest), "pitch {p}");
    }
}

#[test]
fn stages_fade_in_and_out_of_bypass() {
    // HP engages over 5 ms (no step at the first sample), then settles back to bypass:
    // bit-transparent again once its 100 ms ramp home and the 5 ms fade are over
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::HpFreq, 50, 0.0);
    let y1 = run(&mut chain, 0.3, sine(1000.0, 0.5));
    assert!((y1[0] - sine(1000.0, 0.5)(0)).abs() < 1e-6);
    assert!(!chain.is_idle(0));
    let t = chain.time_at(0);
    chain.apply(MasterFxKey::HpFreq, 0, t);
    let off = (0.3 * SR) as usize;
    run(&mut chain, 0.2, |i| sine(1000.0, 0.5)(i + off));
    assert!(chain.is_idle(0));
    let off = (0.5 * SR) as usize;
    let x: Vec<f32> = (0..4800).map(|i| sine(1000.0, 0.5)(i + off)).collect();
    let y = run(&mut chain, 0.1, |i| sine(1000.0, 0.5)(i + off));
    assert_eq!(
        y.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        x.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}

#[test]
fn silence_decays_to_exact_zero() {
    // an impulse through every stage, then a long silence: no denormal tail survives
    let mut chain = MasterChain::new(SR);
    for (k, v) in [
        (MasterFxKey::HpFreq, 40),
        (MasterFxKey::LpFreq, 30),
        (MasterFxKey::PhaserWet, 100),
        (MasterFxKey::MasterPitch, 5),
    ] {
        chain.apply(k, v, 0.0);
    }
    let y = run(&mut chain, 4.0, |i| if i == 100 { 1.0 } else { 0.0 });
    assert!(!y.iter().any(|v| v.is_subnormal()));
    assert!(y[y.len() - 4800..].iter().all(|v| *v == 0.0));
}
