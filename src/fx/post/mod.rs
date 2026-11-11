//! Post-processing ("master") stage for offline renders and, optionally, djapp's live output
//! (docs/POST.md). Beyond Swift: beats-btw-dj has no such stage.
//!
//! - [`chain`]: the chain as data ([`PostChain`], [`Stage`]), the presets, JSON, and the
//!   ffmpeg graph each one stands for
//! - [`dsp`]: the streaming stages (RBJ biquads, a downward expander, an STFT spectral denoise
//!   with minimum-statistics noise tracking, a short-term-LUFS leveler, a true-peak limiter),
//!   real-time safe
//! - [`offline`]: a chain over a whole file, the loudness stage in two passes (measure, then
//!   apply, like ffmpeg's `loudnorm`), and the before / after measurements (LUFS, LRA, true
//!   peak, noise floor)
//! - [`live`]: a chain on live audio ([`LivePost`])
//!
//! Pure Rust and `no_std`: nothing here needs ffmpeg.

pub mod chain;
pub mod dsp;
pub mod live;
pub mod offline;

pub use chain::{LoudnessMode, PostChain, Stage, DEFAULT_PRESET, PRESETS};
pub use live::{LiveMeter, LivePost};
pub use offline::{measure, process, Measurement, PostReport};

#[cfg(test)]
mod tests {
    use super::dsp::{Biquad, BiquadKind, Denoise, Fft};
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const SR: f64 = 44_100.0;

    /// A deterministic LCG's white noise at `db` dBFS RMS.
    fn noise(n: usize, db: f64, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let a = libm::pow(10.0, db / 20.0) * libm::sqrt(3.0);
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let u = ((s >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0;
                (u * a) as f32
            })
            .collect()
    }

    fn sine(n: usize, hz: f64, db: f64) -> Vec<f32> {
        let a = libm::pow(10.0, db / 20.0) * libm::sqrt(2.0);
        (0..n)
            .map(|i| (a * libm::sin(2.0 * core::f64::consts::PI * hz * i as f64 / SR)) as f32)
            .collect()
    }

    fn rms_db(x: &[f32]) -> f64 {
        let s: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum();
        10.0 * libm::log10(s / x.len() as f64)
    }

    #[test]
    fn fft_round_trips() {
        let f = Fft::new(64);
        let mut re: Vec<f32> = (0..64).map(|i| libm::sin(i as f64 * 0.3) as f32).collect();
        let orig = re.clone();
        let mut im = vec![0.0f32; 64];
        f.process(&mut re, &mut im, false);
        f.process(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&orig) {
            assert!((a / 64.0 - b).abs() < 1e-5);
        }
    }

    #[test]
    fn every_preset_parses_and_round_trips_as_json() {
        for name in PostChain::preset_names() {
            let c = PostChain::preset(name).unwrap();
            assert_eq!(c.name.as_deref(), Some(name));
            let back = PostChain::from_json(c.json().get("stages").unwrap()).unwrap();
            assert_eq!(back.stages, c.stages, "{name}");
        }
        let m = PostChain::preset("clean-medium").unwrap();
        assert_eq!(
            m.ffmpeg(),
            "highpass=f=25,afftdn=nf=-50:nr=14:tn=1,loudnorm=I=-14:TP=-1:LRA=11"
        );
        assert!(PostChain::preset("nope").is_none());
        assert_eq!(PostChain::preset("medium").unwrap().name.as_deref(), Some("clean-medium"));
        let custom = PostChain::from_json(
            &crate::json::JsonValue::parse(
                r#"["master",{"stage":"highshelf","freq":8000,"gain_db":-3}]"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(custom.stages.len(), 2);
        assert!(PostChain::from_json(&crate::json::JsonValue::str("[{\"stage\":\"warp\"}]")).is_err());
        assert_eq!(PostChain::step_preset("master", 1), "none");
        assert_eq!(PostChain::step_preset("none", -1), "master");
    }

    #[test]
    fn highpass_cuts_rumble_and_passes_the_mids() {
        let mut lo = sine(44_100, 10.0, -20.0);
        let mut lo2 = lo.clone();
        let mut mid = sine(44_100, 1000.0, -20.0);
        let mut mid2 = mid.clone();
        Biquad::new(BiquadKind::Highpass, 25.0, 0.0, core::f64::consts::FRAC_1_SQRT_2, SR).process(&mut lo, &mut lo2);
        Biquad::new(BiquadKind::Highpass, 25.0, 0.0, core::f64::consts::FRAC_1_SQRT_2, SR).process(&mut mid, &mut mid2);
        assert!(rms_db(&lo[22_050..]) < -34.0, "{}", rms_db(&lo[22_050..]));
        assert!((rms_db(&mid[22_050..]) + 20.0).abs() < 0.1);
    }

    #[test]
    fn denoise_turns_white_noise_down_and_keeps_a_loud_tone() {
        let n = 8 * 44_100;
        let mut l = noise(n, -50.0, 1);
        let mut r = noise(n, -50.0, 2);
        let mut d = Denoise::new(-50.0, 14.0, true, 12.0, chain::NOISE_OFFSET_DB, 2048, SR);
        d.process(&mut l, &mut r);
        let tail = &l[4 * 44_100..];
        assert!(rms_db(tail) < -56.0, "noise after {}", rms_db(tail));
        // the tracked estimate settles near the real floor
        assert!((d.noise_db() + 50.0).abs() < 3.0, "estimate {}", d.noise_db());
        // a tone 30 dB over the noise keeps its level
        let tone = sine(n, 1000.0, -20.0);
        let nz = noise(n, -50.0, 3);
        let mut l: Vec<f32> = tone.iter().zip(&nz).map(|(a, b)| a + b).collect();
        let mut r = l.clone();
        let mut d = Denoise::new(-50.0, 14.0, true, 12.0, chain::NOISE_OFFSET_DB, 2048, SR);
        d.process(&mut l, &mut r);
        assert!((rms_db(&l[4 * 44_100..]) + 20.0).abs() < 0.5, "{}", rms_db(&l[4 * 44_100..]));
    }

    #[test]
    fn offline_master_lands_on_target_and_ceiling() {
        // quiet music-ish: a tone with an envelope, plus noise
        let n = 20 * 44_100;
        let nz = noise(n, -60.0, 4);
        let mut l: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / SR;
                let env = 0.3 + 0.7 * (0.5 + 0.5 * libm::sin(t * 1.3));
                (env * 0.1 * libm::sin(2.0 * core::f64::consts::PI * 220.0 * t)) as f32 + nz[i]
            })
            .collect();
        let mut r = l.clone();
        let rep = process(&mut l, &mut r, SR, &PostChain::preset("clean-medium").unwrap());
        let after = rep.after.lufs.unwrap();
        assert!((after + 14.0).abs() < 0.3, "after {after}");
        assert!(rep.after.true_peak_db <= -0.9, "tp {}", rep.after.true_peak_db);
        assert_eq!(l.len(), n, "latency is taken off");
        // master alone, linear: nothing but one gain
        let mut l2 = sine(n, 440.0, -30.0);
        let mut r2 = l2.clone();
        let rep = process(&mut l2, &mut r2, SR, &PostChain::preset("master").unwrap());
        assert_eq!(rep.loudness[0].mode, LoudnessMode::Linear);
        assert!((rep.after.lufs.unwrap() + 14.0).abs() < 0.1);
    }

    #[test]
    fn live_leveler_brings_a_quiet_feed_up_and_holds_the_ceiling() {
        let c = PostChain::preset("master").unwrap();
        let mut p = LivePost::new(&c, SR);
        let n = 30 * 44_100;
        let mut l = sine(n, 440.0, -30.0);
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            p.process(a, b);
        }
        let m = measure(&l[20 * 44_100..], &r[20 * 44_100..], SR);
        assert!((m.lufs.unwrap() + 14.0).abs() < 1.0, "{:?}", m.lufs);
        assert!(m.true_peak_db <= -0.9);
        assert!(p.meter().gain_db > 12.0, "{:?}", p.meter());
    }

    #[test]
    fn expander_turns_down_what_is_far_under_the_program() {
        let n = 10 * 44_100;
        let loud = sine(n, 440.0, -15.0);
        let quiet = noise(n, -60.0, 9);
        let mut l: Vec<f32> = loud[..n / 2].iter().chain(&quiet[n / 2..]).copied().collect();
        let mut r = l.clone();
        let mut e = super::dsp::Expander::new(None, 30.0, 2.0, 20.0, 250.0, 24.0, 9.0, SR, Some(-15.0));
        e.process(&mut l, &mut r);
        assert!((rms_db(&l[44_100..n / 2]) + 15.0).abs() < 0.2);
        assert!(rms_db(&l[n / 2 + 44_100..]) < -70.0, "{}", rms_db(&l[n / 2 + 44_100..]));
    }

    /// The live chain's cost: `cargo test -p djengine --release live_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_cost() {
        extern crate std;
        let sr = 48_000.0;
        let n = 60 * 48_000;
        for name in ["clean-medium", "expander", "master"] {
            let mut p = LivePost::new(&PostChain::preset(name).unwrap(), sr);
            let mut l = noise(n, -20.0, 5);
            let mut r = noise(n, -20.0, 6);
            let t = std::time::Instant::now();
            for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
                p.process(a, b);
            }
            let took = t.elapsed().as_secs_f64();
            std::println!(
                "{name}: 60 s in {:.3} s = {:.2} % of a core, latency {} frames",
                took,
                took / 60.0 * 100.0,
                p.latency()
            );
        }
    }
}
