//! `PeakLimiter` / `TruePeak` against the literals of Swift `TruePeakLimiterTests`.

use djrust::fx::limiter::{GainReductionStats, PeakLimiter};
use djrust::fx::true_peak;
use djrust::wave::{Master, WaveOptions, WaveRunner};
use djrust::TrackSpec;

const SR: f64 = 48_000.0;

fn run(lim: &mut PeakLimiter, l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    let mut done = 0;
    while done < n {
        let k = 512.min(n - done);
        lim.process(&mut l[done..done + k], Some(&mut r[done..done + k]), k);
        done += k;
    }
}

#[test]
fn inter_sample_peaks_are_held_under_the_ceiling() {
    let n = 48_000;
    // samples at ±0.85 (-1.4 dBFS), true peak 1.2 (+1.6 dBTP)
    let x: Vec<f32> = (0..n)
        .map(|i| {
            (1.2 * libm::sin(core::f64::consts::PI / 2.0 * i as f64 + core::f64::consts::PI / 4.0))
                as f32
        })
        .collect();
    assert!(true_peak::dbtp(&x, None) > 1.0);
    let sample_ceiling = libm::pow(10.0, PeakLimiter::CEILING_DB / 20.0) as f32;
    assert!(x.iter().fold(0f32, |m, v| m.max(v.abs())) < sample_ceiling);
    let mut lim = PeakLimiter::new(SR);
    let (mut l, mut r) = (x.clone(), x.clone());
    run(&mut lim, &mut l, &mut r);
    let tp = true_peak::dbtp(&l, Some(&r));
    assert!(tp <= PeakLimiter::CEILING_DB, "output {tp} dBTP");
    assert!(
        tp > PeakLimiter::CEILING_DB - 0.5,
        "held {tp} dBTP: no deeper than it needs"
    );
    let st = lim.statistics(&[0.5, 0.95, 0.99]);
    assert!(st.true_peak_dbtp <= PeakLimiter::CEILING_DB);
    assert!(
        (st.true_peak_dbtp - tp).abs() < 0.05,
        "meter {}, offline {tp}",
        st.true_peak_dbtp
    );
    assert!(lim.gain_reduction_db() > 2.0);
}

#[test]
fn hot_material_stays_under_the_ceiling() {
    for drive_db in [0.0, 6.0, 12.0] {
        let n = 96_000;
        let gain = libm::pow(10.0, drive_db / 20.0);
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut noise = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 1) as i64 % 2_000_000) as f64 / 1_000_000.0 - 1.0
        };
        let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
        for i in 0..n {
            let t = i as f64 / SR;
            let bass = libm::tanh(4.0 * libm::sin(2.0 * core::f64::consts::PI * 55.0 * t)) * 0.6;
            let click = if i % 6000 < 3 {
                if i % 2 == 0 {
                    1.0
                } else {
                    -1.0
                }
            } else {
                0.0
            };
            let burst = if (i / 4800) % 4 == 1 {
                noise() * 0.7
            } else {
                0.0
            };
            let hat = libm::sin(2.0 * core::f64::consts::PI * 11_900.0 * t)
                * if i % 3000 < 400 { 0.5 } else { 0.0 };
            l[i] = (gain * (bass + click + burst + hat)) as f32;
            r[i] = (gain * (bass - click + 0.5 * burst - hat)) as f32;
        }
        let mut lim = PeakLimiter::new(SR);
        run(&mut lim, &mut l, &mut r);
        let tp = true_peak::dbtp(&l, Some(&r));
        assert!(
            tp <= PeakLimiter::CEILING_DB,
            "drive {drive_db} dB: {tp} dBTP"
        );
        assert!(lim.statistics(&[0.5]).true_peak_dbtp <= PeakLimiter::CEILING_DB);
    }
}

#[test]
fn the_delay_is_fixed_and_small() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut lim = PeakLimiter::new(rate);
        let window = libm::round(PeakLimiter::LOOKAHEAD_SECONDS * rate) as usize;
        assert_eq!(lim.lookahead, window + 2 * true_peak::DELAY);
        assert!(
            lim.latency_seconds() < 0.002,
            "{rate}: {} s",
            lim.latency_seconds()
        );
        let mut l = vec![0f32; 1024];
        l[100] = 0.25;
        let mut r = l.clone();
        run(&mut lim, &mut l, &mut r);
        assert_eq!(l.iter().position(|&v| v != 0.0), Some(100 + lim.lookahead));
        assert_eq!(l[100 + lim.lookahead], 0.25);
        assert_eq!(lim.statistics(&[0.5]).limited_share, 0.0);
    }
}

#[test]
fn the_gain_moves_smoothly() {
    let n = 24_000;
    let mut l: Vec<f32> = (0..n)
        .map(|i| (0.25 * libm::sin(2.0 * core::f64::consts::PI * 1000.0 * i as f64 / SR)) as f32)
        .collect();
    for v in &mut l[12_000..12_048] {
        *v *= 16.0; // a 1 ms burst, +12 dB over the ceiling
    }
    let reference = l.clone();
    let mut r = l.clone();
    let mut lim = PeakLimiter::new(SR);
    run(&mut lim, &mut l, &mut r);
    let (mut biggest, mut last) = (0f64, 1f64);
    for i in lim.lookahead..n {
        let x = reference[i - lim.lookahead] as f64;
        if x.abs() <= 0.05 {
            continue;
        }
        let g = l[i] as f64 / x;
        if i > lim.lookahead {
            biggest = biggest.max((g - last).abs());
        }
        last = g;
    }
    assert!(biggest < 0.2, "largest gain step {biggest}");
    assert!(true_peak::dbtp(&l, Some(&r)) <= PeakLimiter::CEILING_DB);
}

#[test]
fn the_report_carries_true_peak_and_p95() {
    let mut lim = PeakLimiter::new(SR);
    let mut l: Vec<f32> = (0..4800)
        .map(|i| (2.0 * libm::sin(i as f64 * 0.1)) as f32)
        .collect();
    let mut r = l.clone();
    run(&mut lim, &mut l, &mut r);
    let st = lim.statistics(&[0.5, 0.95, 0.99]);
    assert!(st.true_peak_dbtp <= PeakLimiter::CEILING_DB);
    assert_eq!(st.percentiles_db.len(), 3);
    lim.reset_statistics();
    assert_eq!(
        lim.statistics(&[0.5, 0.95, 0.99]),
        GainReductionStats::default()
    );
}

/// `everyGenreReadsUnderTheCeiling`, synth voices only, 24 kHz and a shorter song: true peak
/// <= -1 dBTP after the limiter, and it works rarely (P95 GR <= 2 dB).
#[test]
fn songs_on_the_app_path_read_under_the_ceiling() {
    for genre in ["techno", "house", "dnb", "jazz", "ambient", "trap"] {
        let rate = 24_000.0;
        let spec = TrackSpec::new(genre, 4242.0, Some("standard"));
        let project = spec.project();
        let mut opts = WaveOptions::extended();
        assert_eq!(opts.master, Master::Limit);
        opts.master_gain = WaveOptions::extended().master_gain;
        let mut runner = WaveRunner::new(rate, opts);
        runner.load(&project);
        runner.play();
        let mut lim = PeakLimiter::new(rate);
        let frames = (rate * 60.0) as usize;
        let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
        let mut done = 0;
        while done < frames {
            let k = 512.min(frames - done);
            runner.process(&mut l[done..done + k], Some(&mut r[done..done + k]), k);
            lim.process(&mut l[done..done + k], Some(&mut r[done..done + k]), k);
            done += k;
        }
        let st = lim.statistics(&[0.5, 0.95, 0.99]);
        assert!(
            st.true_peak_dbtp <= PeakLimiter::CEILING_DB,
            "{genre}: {} dBTP",
            st.true_peak_dbtp
        );
        assert!(
            st.percentiles_db[1] <= 2.0,
            "{genre}: GR P95 {} dB",
            st.percentiles_db[1]
        );
        let tp = true_peak::dbtp(&l, Some(&r));
        assert!(
            tp <= PeakLimiter::CEILING_DB + 0.01,
            "{genre}: rendered {tp} dBTP"
        );
    }
}
