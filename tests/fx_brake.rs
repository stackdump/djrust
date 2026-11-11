//! dj's vinyl brake (`TurntableBrake`), mirroring the Swift `TurntableBrakeTests` on a synthetic
//! source in place of a `WaveDeckSource` (no wave engine in this crate yet): the pitch and the
//! source clock fall with r(t) = 1 - t / (0.75 D), the record is silent while stopped, the clock
//! is put back on at the release, a panic releases in 5 ms, a cancel does not put the clock on.
//! No Swift oracle here: the expectations are the Swift tests' own closed forms.

use djrust::fx::brake::{BrakeSource, TurntableBrake};

const RATE: f64 = 22_050.0;
const BLOCK: usize = 128;
const F0: f64 = 441.0; // 50 samples a cycle at RATE

/// A sine per *source* frame; counts the frames it was asked for.
struct SineSource {
    playing: bool,
    pulled: usize,
    caught_up: Vec<f64>,
}

impl SineSource {
    fn new() -> Self {
        SineSource {
            playing: true,
            pulled: 0,
            caught_up: vec![],
        }
    }
}

impl BrakeSource for SineSource {
    fn playing(&self) -> bool {
        self.playing
    }
    fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        _start: usize,
    ) {
        for i in 0..frames {
            left[i] = (0.5
                * (2.0 * std::f64::consts::PI * F0 * (self.pulled + i) as f64 / RATE).sin())
                as f32;
        }
        if let Some(r) = right {
            r[..frames].copy_from_slice(&left[..frames]);
        }
        self.pulled += frames;
    }
    fn brake_lag(&self, _sr: f64) -> f64 {
        0.01
    }
    fn brake_chunk(&mut self, _output: usize, _pulled: usize) {}
    fn catch_up(&mut self, ticks: f64) {
        self.caught_up.push(ticks);
    }
}

fn render(
    brake: &mut TurntableBrake,
    src: &mut SineSource,
    blocks: usize,
    mut at: impl FnMut(usize, &mut TurntableBrake, &mut SineSource),
) -> Vec<f32> {
    let mut out = vec![];
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for b in 0..blocks {
        at(b, brake, src);
        brake.render(src, &mut l, Some(&mut r), BLOCK, b * BLOCK);
        out.extend_from_slice(&l);
    }
    out
}

/// Upward zero crossings per frame over `x[range]` (first to last crossing, interpolated).
fn crossing_rate(x: &[f32], lo: usize, hi: usize) -> f64 {
    let (mut first, mut last, mut count) = (-1.0, -1.0, 0);
    for i in lo + 1..hi {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f64 + (-x[i - 1] as f64) / (x[i] as f64 - x[i - 1] as f64);
            if first < 0.0 {
                first = t;
            }
            last = t;
            count += 1;
        }
    }
    if count > 1 {
        (count - 1) as f64 / (last - first)
    } else {
        0.0
    }
}

fn max_step(x: &[f32], lo: usize, hi: usize) -> f32 {
    (lo + 1..hi).fold(0.0, |m, i| m.max((x[i] - x[i - 1]).abs()))
}

const FIRE_BLOCK: usize = 40;
const BAR: usize = (RATE * 2.0) as usize; // one bar at 120 BPM

#[test]
fn pitch_and_clock_fall_with_the_rate() {
    let mut brake = TurntableBrake::new(RATE);
    let mut src = SineSource::new();
    let stop = BAR as f64 * TurntableBrake::SPIN_DOWN;
    let fire = FIRE_BLOCK * BLOCK;
    let out = render(
        &mut brake,
        &mut src,
        FIRE_BLOCK + BAR / BLOCK + 20,
        |b, br, _| {
            if b == FIRE_BLOCK {
                assert!(br.start(BAR));
            }
        },
    );
    let f0 = crossing_rate(&out, fire - 4000, fire);
    assert!((f0 - F0 / RATE).abs() < 1e-4, "{f0}");
    for frac in [0.1, 0.5, 0.9] {
        let c = fire + (frac * stop) as usize;
        let f = crossing_rate(&out, c - 750, c + 750);
        let want = 1.0 - frac;
        assert!(
            (f / f0 - want).abs() < 0.01,
            "pitch at {frac}: {} of f0, r = {want}",
            f / f0
        );
    }
    assert!(!brake.is_on(), "released");
    // the source ran ahead of the brake's clock by exactly the pulled frames: its clock fell
    // behind the unbraked one by the deficit, the catch-up the release asked for
    assert_eq!(src.caught_up.len(), 1);
    // deficit = (frames the brake took - frames it pulled) x lag; the brake took BAR frames
    // and pulled ~ stop / 2 (+ the 3 look-ahead): (BAR - stop/2) x 0.01
    let want = (BAR as f64 - stop / 2.0) * 0.01;
    assert!(
        (src.caught_up[0] - want).abs() < 0.2,
        "deficit {} vs {want}",
        src.caught_up[0]
    );
}

#[test]
fn silent_when_stopped_and_back_after() {
    let mut brake = TurntableBrake::new(RATE);
    let mut dry_brake = TurntableBrake::new(RATE);
    let (mut src, mut reference) = (SineSource::new(), SineSource::new());
    let fire = FIRE_BLOCK * BLOCK;
    let release = fire + BAR;
    let stop = BAR as f64 * TurntableBrake::SPIN_DOWN;
    let mut stopped_pulled = None;
    let blocks = release / BLOCK + 400;
    let out = render(&mut brake, &mut src, blocks, |b, br, s| {
        if b == FIRE_BLOCK {
            br.start(BAR);
        }
        if b == (fire + stop as usize) / BLOCK + 2 {
            stopped_pulled = Some(s.pulled);
        }
        if b == release / BLOCK - 2 {
            assert_eq!(
                Some(s.pulled),
                stopped_pulled,
                "the clock stands still while the record is stopped"
            );
        }
    });
    let reference_out = render(&mut dry_brake, &mut reference, blocks, |_, _, _| {});
    // nothing before the fire changes
    assert_eq!(out[..fire], reference_out[..fire]);
    // the stop: exact silence from r = 0 to the release
    let silent = fire + stop.ceil() as usize;
    assert!(
        out[silent..release].iter().all(|v| *v == 0.0),
        "silent while stopped"
    );
    assert!(
        out[fire + 100..fire + 2000].iter().any(|v| *v != 0.0),
        "spinning down plays"
    );
    // no click: the steps into and out of the brake are no bigger than the music's own
    let music = max_step(&reference_out, fire, reference_out.len());
    assert!(max_step(&out, fire - 50, fire + 500) <= music * 1.05);
    assert!(max_step(&out, release - 50, release + 500) <= music * 1.05);
    // and the fade-in starts from silence
    assert!(out[release].abs() < 0.01);
    assert!(!brake.is_on());
    assert_eq!(src.caught_up.len(), 1);
}

#[test]
fn two_bars_take_twice_as_long() {
    let mut brake = TurntableBrake::new(RATE);
    let mut src = SineSource::new();
    let fire = FIRE_BLOCK * BLOCK;
    let mut halfway = false;
    render(
        &mut brake,
        &mut src,
        FIRE_BLOCK + 2 * BAR / BLOCK + 50,
        |b, br, _| {
            if b == FIRE_BLOCK {
                br.start(2 * BAR);
            }
            if b == (fire + BAR) / BLOCK {
                halfway = br.is_on();
            }
        },
    );
    assert!(halfway);
    assert!(!brake.is_on());
}

#[test]
fn a_second_start_while_braking_is_refused() {
    let mut brake = TurntableBrake::new(RATE);
    assert!(brake.start(1000));
    assert!(!brake.start(5000));
    assert_eq!(brake.release_frame(), 1000);
}

#[test]
fn panic_releases_at_once_in_time() {
    let mut brake = TurntableBrake::new(RATE);
    let mut src = SineSource::new();
    let panic_block = FIRE_BLOCK + 100;
    let fade = (0.005 * RATE).round() as usize;
    let mut on_after = true;
    let out = render(&mut brake, &mut src, panic_block + 300, |b, br, _| {
        if b == FIRE_BLOCK {
            br.start(BAR);
        }
        if b == panic_block {
            br.panic();
        }
        if b == panic_block + (fade + BLOCK - 1) / BLOCK {
            on_after = br.is_on();
        }
    });
    assert!(!on_after, "released within 5 ms (and the block it ends in)");
    assert_eq!(src.caught_up.len(), 1, "the clock is put on at the release");
    let at = panic_block * BLOCK;
    assert!(
        max_step(&out, at - 50, at + 2 * fade + 200)
            <= 0.5 * 2.0 * std::f32::consts::PI * (F0 / RATE) as f32 * 1.05,
        "no click"
    );
}

#[test]
fn cancel_drops_the_brake_without_putting_the_clock_on() {
    let mut brake = TurntableBrake::new(RATE);
    let mut src = SineSource::new();
    render(&mut brake, &mut src, 200, |b, br, _| {
        if b == 10 {
            br.start(BAR);
        }
        if b == 60 {
            br.cancel();
        }
    });
    assert!(!brake.is_on());
    assert!(src.caught_up.is_empty());
    // the next brake starts afresh
    assert!(brake.start(1000));
    assert_eq!(brake.elapsed(), 0);
    assert_eq!(brake.deficit_ticks(), 0.0);
}

#[test]
fn a_paused_source_does_not_age_the_brake() {
    let mut brake = TurntableBrake::new(RATE);
    let mut src = SineSource::new();
    render(&mut brake, &mut src, 60, |b, br, s| {
        if b == 10 {
            br.start(BAR);
        }
        if b == 20 {
            s.playing = false;
        }
    });
    let e = brake.elapsed();
    assert_eq!(e, 10 * BLOCK);
    assert!(brake.is_on(), "waits for the clock to run again");
}

#[test]
fn replays_to_the_byte() {
    let run = || {
        let mut brake = TurntableBrake::new(RATE);
        let mut src = SineSource::new();
        render(&mut brake, &mut src, 900, |b, br, _| match b {
            20 => {
                br.start(BAR);
            }
            300 => {
                br.start(2 * BAR);
            }
            420 => br.panic(),
            500 => {
                br.start(BAR);
            }
            _ => {}
        })
    };
    let a = run();
    assert_eq!(
        a.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        run().iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    assert!(a.iter().all(|x| x.is_finite()));
}
