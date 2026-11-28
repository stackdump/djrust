//! The cue on a second device (booth roadmap R7): the resampler between two device clocks,
//! run on synthetic clocks with no device. Each scenario is an event-driven simulation: the
//! master callback pushes a block of a 1 kHz tone into the ring at its (drifted, jittered)
//! times, the cue callback pulls a block through the resampler at its own. Deterministic: the
//! jitter is a fixed LCG.

use djrust::host::deck::cue_resample::{
    catmull_rom, CueConfig, CueMonitor, CueResampler, CueStamp, MAX_CORRECTION,
};
use djrust::host::deck::output::{Interleaver, Routing};
use djrust::host::deck::{engine, EngineConfig};
use djrust::TrackSpec;
use rtrb::RingBuffer;

struct Side {
    /// Nominal rate, Hz, and the crystal's error, ppm.
    rate: f64,
    ppm: f64,
    block: usize,
}

struct Scenario {
    master: Side,
    cue: Side,
    seconds: f64,
    /// Callback jitter, s (each callback lands up to this late, never early).
    jitter: f64,
    /// Cue callbacks skipped in [from, to) s: the cue device stalls.
    cue_stall: Option<(f64, f64)>,
    /// Master callbacks skipped in [from, to) s: the ring runs dry.
    master_stall: Option<(f64, f64)>,
    /// Checks start here, s (the loop has locked).
    settle: f64,
}

#[derive(Debug, Default)]
struct Outcome {
    overruns: u64,
    underruns: u64,
    skips: u64,
    /// Over the checked span: the fill's worst distance from the target, frames.
    worst_fill_error: f64,
    /// The largest |second difference| of the cue output (a click is a big one).
    worst_d2: f64,
    max_target: f64,
    correction: f64,
    /// The integral term alone: the loop's drift estimate.
    drift_estimate: f64,
    /// The drift the loop should find: master / cue clock, less one.
    drift: f64,
    cue_frames: u64,
    playing_at_end: bool,
}

struct Lcg(u64);
impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn in_span(t: f64, span: Option<(f64, f64)>) -> bool {
    span.is_some_and(|(a, b)| t >= a && t < b)
}

fn run(s: &Scenario) -> Outcome {
    let fm = s.master.rate * (1.0 + s.master.ppm * 1e-6);
    let fc = s.cue.rate * (1.0 + s.cue.ppm * 1e-6);
    let (mut tx, mut rx) = RingBuffer::<f32>::new(s.master.rate as usize / 2 * 2);
    let stamp = CueStamp::new();
    let monitor = CueMonitor::new();
    let mut rs = CueResampler::new(CueConfig::new(s.master.rate, s.cue.rate));
    let mut rng = Lcg(0x5eed);

    // the tone: a phasor at 1 kHz of the master's nominal rate, amplitude 0.5
    let w = 2.0 * std::f64::consts::PI * 1000.0 / s.master.rate;
    let (cw, sw) = (w.cos(), w.sin());
    let (mut re, mut im) = (0.5f64, 0.0f64);

    let mut out = Outcome {
        drift: fm / s.master.rate / (fc / s.cue.rate) - 1.0,
        ..Default::default()
    };
    let (mut km, mut kc) = (0u64, 0u64);
    let next_m = |k: u64, j: f64| k as f64 * s.master.block as f64 / fm + j;
    let next_c = |k: u64, j: f64| k as f64 * s.cue.block as f64 / fc + j;
    let (mut jm, mut jc) = (0.0, 0.0);
    let mut buf = vec![0f32; s.cue.block * 2];
    let (mut p1, mut p2): (Option<f32>, Option<f32>) = (None, None);
    loop {
        let (tm, tc) = (next_m(km, jm), next_c(kc, jc));
        let t = tm.min(tc);
        if t >= s.seconds {
            break;
        }
        let us = (t * 1e6) as u64;
        if tm <= tc {
            if !in_span(t, s.master_stall) {
                for _ in 0..s.master.block {
                    let (l, r) = (re as f32, im as f32);
                    if tx.push(l).is_err() || tx.push(r).is_err() {
                        out.overruns += 1;
                    }
                    let n = re * cw - im * sw;
                    im = re * sw + im * cw;
                    re = n;
                }
                // keep the phasor on its circle
                let g = 0.5 / (re * re + im * im).sqrt();
                re *= g;
                im *= g;
                stamp.mark(us, s.master.block);
            }
            km += 1;
            jm = s.jitter * rng.unit();
        } else {
            if !in_span(t, s.cue_stall) {
                let lead = stamp.lead(us, s.master.rate);
                rs.fill(&mut rx, 2, &mut buf, lead, stamp.frames(), Some(&monitor));
                out.cue_frames += s.cue.block as u64;
                let checked =
                    t >= s.settle && !in_span(t, s.cue_stall) && !in_span(t, s.master_stall);
                if checked {
                    out.worst_fill_error = out
                        .worst_fill_error
                        .max((rs.fill_level() - rs.target()).abs());
                    out.max_target = out.max_target.max(rs.target());
                }
                for f in buf.chunks(2) {
                    let x = f[0];
                    if let (Some(a), Some(b)) = (p1, p2) {
                        if checked {
                            out.worst_d2 = out.worst_d2.max((x - 2.0 * a + b).abs() as f64);
                        }
                    }
                    p2 = p1;
                    p1 = Some(x);
                }
            } else {
                // a stalled callback: the next one's output is not continuous with the last
                p1 = None;
                p2 = None;
            }
            kc += 1;
            jc = s.jitter * rng.unit();
        }
    }
    out.underruns = rs.underruns();
    out.skips = rs.skips();
    out.correction = rs.correction();
    out.drift_estimate = rs.drift_estimate();
    out.playing_at_end = rs.playing();
    assert_eq!(monitor.underruns(), out.underruns);
    assert_eq!(monitor.skips(), out.skips);
    assert!((monitor.correction_ppm() - out.correction * 1e6).abs() < 0.01);
    out
}

fn side(rate: f64, ppm: f64, block: usize) -> Side {
    Side { rate, ppm, block }
}

/// The tone's largest second difference at the cue's rate: a sine's is A·ω².
fn tone_d2(cue_rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * 1000.0 / cue_rate;
    0.5 * w * w
}

fn assert_clean(o: &Outcome, master_rate: f64, cue_rate: f64, what: &str) {
    assert_eq!(o.overruns, 0, "{what}: the ring overflowed: {o:?}");
    assert_eq!(o.underruns, 0, "{what}: the ring ran dry: {o:?}");
    assert_eq!(o.skips, 0, "{what}: frames were skipped: {o:?}");
    assert!(o.playing_at_end, "{what}: {o:?}");
    // bounded latency: the loop holds the fill (master frames) within 2 ms of its target,
    // and the target is a few blocks: under 30 ms, or 45 ms with a 1024-frame cue buffer
    assert!(
        o.worst_fill_error < 0.002 * master_rate,
        "{what}: fill wandered: {o:?}"
    );
    assert!(o.max_target < 0.045 * master_rate, "{what}: latency: {o:?}");
    // the loop found the clocks' drift
    assert!(
        (o.drift_estimate - o.drift).abs() < 5e-6,
        "{what}: drift estimate {} ppm for a drift of {} ppm",
        o.drift_estimate * 1e6,
        o.drift * 1e6
    );
    // and the momentary correction is within a few tens of ppm of it (jitter; < 0.1 cent)
    assert!((o.correction - o.drift).abs() < 1e-4, "{what}: {o:?}");
    // no click anywhere: a dropout or a skip is a second difference far past the tone's own
    assert!(
        o.worst_d2 < 1.2 * tone_d2(cue_rate),
        "{what}: a click: {o:?}"
    );
}

const HOUR: f64 = 3600.0;

#[test]
fn an_hour_with_the_master_fast_and_the_cue_slow_by_200_ppm_each() {
    let s = Scenario {
        master: side(48_000.0, 200.0, 512),
        cue: side(48_000.0, -200.0, 480),
        seconds: HOUR,
        jitter: 0.001,
        cue_stall: None,
        master_stall: None,
        settle: 60.0,
    };
    let o = run(&s);
    assert!(o.cue_frames as f64 > 0.99 * HOUR * 48_000.0);
    assert_clean(&o, 48_000.0, 48_000.0, "+200/-200 ppm");
    assert!(o.max_target < 0.03 * 48_000.0, "{o:?}");
}

#[test]
fn an_hour_with_the_master_slow_and_the_cue_fast_by_200_ppm_each() {
    let s = Scenario {
        master: side(48_000.0, -200.0, 256),
        cue: side(48_000.0, 200.0, 512),
        seconds: HOUR,
        jitter: 0.001,
        cue_stall: None,
        master_stall: None,
        settle: 60.0,
    };
    assert_clean(&run(&s), 48_000.0, 48_000.0, "-200/+200 ppm");
}

#[test]
fn equal_clocks_need_no_correction() {
    let s = Scenario {
        master: side(48_000.0, 0.0, 512),
        cue: side(48_000.0, 0.0, 512),
        seconds: 300.0,
        jitter: 0.0005,
        cue_stall: None,
        master_stall: None,
        settle: 60.0,
    };
    let o = run(&s);
    assert_clean(&o, 48_000.0, 48_000.0, "0 ppm");
    assert!(o.drift_estimate.abs() < 5e-6);
}

#[test]
fn a_44_1_khz_dongle_under_a_48_khz_master() {
    let s = Scenario {
        master: side(48_000.0, 120.0, 512),
        cue: side(44_100.0, -150.0, 441),
        seconds: 600.0,
        jitter: 0.001,
        cue_stall: None,
        master_stall: None,
        settle: 60.0,
    };
    assert_clean(&run(&s), 48_000.0, 44_100.0, "48k -> 44.1k");
}

#[test]
fn a_48_khz_dongle_under_a_44_1_khz_master() {
    let s = Scenario {
        master: side(44_100.0, -90.0, 441),
        cue: side(48_000.0, 160.0, 1024),
        seconds: 600.0,
        jitter: 0.001,
        cue_stall: None,
        master_stall: None,
        settle: 60.0,
    };
    assert_clean(&run(&s), 44_100.0, 48_000.0, "44.1k -> 48k");
}

#[test]
fn a_stalled_cue_device_is_skipped_back_to_the_target_and_the_loop_holds() {
    // the headphones stop pulling for 600 ms (a USB hiccup): the half-second ring overflows,
    // then on resume the backlog is skipped once, not played as half a second of latency
    let s = Scenario {
        master: side(48_000.0, 200.0, 512),
        cue: side(48_000.0, -200.0, 512),
        seconds: 240.0,
        jitter: 0.001,
        cue_stall: Some((100.0, 100.6)),
        master_stall: None,
        settle: 60.0,
    };
    let o = run(&s);
    assert!(o.overruns > 0, "the stall filled the ring: {o:?}");
    assert_eq!(o.skips, 1, "{o:?}");
    assert_eq!(o.underruns, 0, "{o:?}");
    assert!(o.playing_at_end);
    assert!(o.worst_fill_error < 0.002 * 48_000.0, "{o:?}");
    assert!((o.drift_estimate - o.drift).abs() < 5e-6, "{o:?}");
}

#[test]
fn a_dry_ring_primes_again_with_more_margin_and_keeps_the_drift() {
    // the master stops pushing for 200 ms: one underrun, silence, then the cue comes back
    // with the target one cue block higher and the drift estimate intact
    let s = Scenario {
        master: side(48_000.0, -150.0, 512),
        cue: side(48_000.0, 150.0, 512),
        seconds: 240.0,
        jitter: 0.001,
        cue_stall: None,
        master_stall: Some((100.0, 100.2)),
        settle: 60.0,
    };
    let o = run(&s);
    assert_eq!(o.underruns, 1, "{o:?}");
    assert_eq!(o.skips, 0, "{o:?}");
    assert_eq!(o.overruns, 0, "{o:?}");
    assert!(o.playing_at_end);
    assert!((o.drift_estimate - o.drift).abs() < 5e-6, "{o:?}");
    assert!(o.max_target < 0.04 * 48_000.0, "{o:?}");
}

#[test]
fn a_broken_clock_is_clamped_not_followed() {
    // 2 % apart: past what the loop will bend pitch for; it pins at the clamp and the ring
    // drains, primes, plays, as a fixed-ratio copy would, never further from nominal
    let s = Scenario {
        master: side(48_000.0, -10_000.0, 512),
        cue: side(48_000.0, 10_000.0, 512),
        seconds: 60.0,
        jitter: 0.0,
        cue_stall: None,
        master_stall: None,
        settle: 30.0,
    };
    let o = run(&s);
    assert!(
        o.correction >= -MAX_CORRECTION - 1e-12,
        "never past the clamp: {o:?}"
    );
    assert!(
        o.correction < -0.8 * MAX_CORRECTION,
        "pinned near the clamp: {o:?}"
    );
    assert!(o.underruns > 0);
}

#[test]
fn catmull_rom_is_exact_on_lines_and_close_on_a_1_khz_tone() {
    for t in [0.0f32, 0.25, 0.5, 0.9] {
        let v = catmull_rom(1.0, 2.0, 3.0, 4.0, t);
        assert!((v - (2.0 + t)).abs() < 1e-6);
    }
    let w = 2.0 * std::f64::consts::PI * 1000.0 / 48_000.0;
    let mut worst = 0f64;
    for k in 0..48 {
        for j in 0..10 {
            let t = j as f64 / 10.0;
            let y = |i: i32| ((k as f64 + i as f64) * w).sin() as f32;
            let v = catmull_rom(y(-1), y(0), y(1), y(2), t as f32) as f64;
            worst = worst.max((v - ((k as f64 + t) * w).sin()).abs());
        }
    }
    // under -70 dB of a full-scale tone
    assert!(worst < 3.2e-4, "{worst}");
}

#[test]
fn the_mixer_cue_reaches_a_second_device_through_the_resampler() {
    // the real render path: the master device's Interleaver pushes the cued deck into the
    // ring, the second device (44.1 kHz) pulls it through the resampler
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 0.0);
    e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal")));
    e.set_pfl("A", true);
    e.set_headphone_mix(0.0);
    e.set_headphone_level(1.0);
    let (mut l, mut rr) = (vec![0f32; 256], vec![0f32; 256]);
    r.render_block(&mut l, &mut rr);
    e.play("A");

    let (mut tx, mut rx) = RingBuffer::<f32>::new(sr as usize / 2 * 2);
    let stamp = CueStamp::new();
    let mut rs = CueResampler::new(CueConfig::new(sr, 44_100.0));
    let mut inter = Interleaver::new();
    let mut master = vec![0f32; 2 * 512];
    let mut cue = vec![0f32; 2 * 441];
    let (mut tm, mut tc) = (0.0f64, 0.0f64);
    let (mut heard, mut loud_master) = (0.0f64, 0.0f64);
    while tc < 3.0 {
        if tm <= tc {
            inter.fill(&mut r, &Routing::CueDevice, 2, &mut master, Some(&mut tx));
            loud_master += master.iter().map(|v| (*v as f64).abs()).sum::<f64>();
            stamp.mark((tm * 1e6) as u64, 512);
            tm += 512.0 / sr;
        } else {
            let us = (tc * 1e6) as u64;
            rs.fill(
                &mut rx,
                2,
                &mut cue,
                stamp.lead(us, sr),
                stamp.frames(),
                None,
            );
            if tc > 1.0 {
                heard += cue.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            }
            tc += 441.0 / 44_100.0;
        }
    }
    assert!(loud_master < 1e-3, "the fader is down: {loud_master}");
    assert!(heard > 1.0, "the cue arrived on the second device: {heard}");
    assert_eq!(rs.underruns(), 0);
}
