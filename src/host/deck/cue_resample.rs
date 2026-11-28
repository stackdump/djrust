//! The headphone cue on a second device whose clock is not the master's (booth roadmap R7):
//! the cue ring between the two device callbacks ([`Routing::CueDevice`]) is drained through
//! an adaptive resampler that keeps the ring's fill level, and so the cue's latency, where it
//! is set.
//!
//! Two devices run at two crystals. Even at the same nominal rate they differ by tens to
//! hundreds of ppm, so a fixed 1:1 copy slowly empties the ring (dropouts) or fills it (the
//! cue falls behind, then frames are dropped). Here:
//!
//! - the master callback stamps each push ([`CueStamp`]): how many frames, and when;
//! - the cue callback measures the fill as if the master had pushed continuously (the frames
//!   in the ring less what the last push put in early, [`CueStamp::lead`]). Without that the
//!   fill is a saw of one master block, and two block clocks beating against each other
//!   would wobble the ratio;
//! - a one-pole low-pass smooths that measure, and a PI controller on its error from the
//!   target sets the ratio: `nominal × (1 + c)`, `c` clamped to ±[`MAX_CORRECTION`]. In
//!   steady state the error is zero and `c` is the clocks' relative drift;
//! - the frames are read at that ratio through 4-point cubic (Catmull-Rom) interpolation.
//!
//! The nominal ratio is the master's rate over the cue device's, so a 44.1 kHz dongle under
//! a 48 kHz master works too (a fixed 0.91875 plus the drift).
//!
//! The target is learned from the blocks seen: one master push plus one cue pull plus a
//! margin, never less than [`CueConfig::min_target_s`]. An underrun (the ring ran dry
//! mid-buffer) plays the rest of the buffer as silence, raises the target by one cue block
//! (up to [`CueConfig::max_target_s`]) and primes again. A fill far past the target (the
//! cue device stalled while the master kept pushing) is skipped down to the target, so the
//! cue's latency stays bounded. The drift estimate (the integrator) survives both.
//!
//! No device and no clock: the callbacks pass the times in, so the tests run two synthetic
//! clocks for a simulated hour ([`crate::host::audio`] wires it to cpal).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rtrb::Consumer;

/// The largest correction to the nominal ratio, either way: ±0.5 %. Real crystals are within
/// ±100 ppm of nominal; this leaves room for a bad one and keeps a fault from bending pitch
/// further than a few cents.
pub const MAX_CORRECTION: f64 = 0.005;

/// The control loop's tuning. Every field has a sensible default ([`CueConfig::new`]).
#[derive(Clone, Debug, PartialEq)]
pub struct CueConfig {
    /// The master's rate, Hz: the rate of the frames in the ring.
    pub in_rate: f64,
    /// The cue device's rate, Hz.
    pub out_rate: f64,
    /// The loop's natural frequency, Hz (critically damped): how fast it locks and how much
    /// callback jitter reaches the ratio. 0.05 Hz locks in about 15 s.
    pub loop_hz: f64,
    /// The fill measure's low-pass time constant, s.
    pub smooth_s: f64,
    /// The least target fill, s, whatever the blocks.
    pub min_target_s: f64,
    /// The most the target grows to after underruns, s.
    pub max_target_s: f64,
}

impl CueConfig {
    pub fn new(in_rate: f64, out_rate: f64) -> CueConfig {
        CueConfig {
            in_rate,
            out_rate,
            loop_hz: 0.05,
            smooth_s: 0.2,
            min_target_s: 0.005,
            max_target_s: 0.1,
        }
    }
}

/// The master callback's side: how many frames its last push put in the ring, and when.
/// One atomic word (microseconds in the high 44 bits, frames in the low 20), so the cue
/// callback never reads a torn pair.
#[derive(Debug, Default)]
pub struct CueStamp(AtomicU64);

const FRAME_BITS: u32 = 20;
const FRAME_MASK: u64 = (1 << FRAME_BITS) - 1;

impl CueStamp {
    pub fn new() -> CueStamp {
        CueStamp::default()
    }

    /// The master callback pushed `frames` at `now_us` (any epoch both callbacks share).
    pub fn mark(&self, now_us: u64, frames: usize) {
        let f = (frames as u64).min(FRAME_MASK);
        self.0.store((now_us << FRAME_BITS) | f, Ordering::Release);
    }

    /// The frames the last push put in the ring ahead of a continuous producer at `now_us`:
    /// all of them at the push, none once a block's time at `in_rate` has passed. 0 before
    /// the first push.
    pub fn lead(&self, now_us: u64, in_rate: f64) -> f64 {
        let w = self.0.load(Ordering::Acquire);
        let (at, frames) = (w >> FRAME_BITS, (w & FRAME_MASK) as f64);
        let since = now_us.saturating_sub(at) as f64 * 1e-6 * in_rate;
        (frames - since).max(0.0)
    }

    /// The frames of the last push.
    pub fn frames(&self) -> usize {
        (self.0.load(Ordering::Acquire) & FRAME_MASK) as usize
    }
}

/// What the resampler reports, for the host's status line and its health check. Written by
/// the cue callback, read anywhere.
#[derive(Debug, Default)]
pub struct CueMonitor {
    underruns: AtomicU64,
    skips: AtomicU64,
    /// The smoothed fill, frames of the master's rate.
    fill: AtomicU32,
    target: AtomicU32,
    /// The correction, parts per billion, biased by 2^31 so it fits a u32.
    correction_ppb: AtomicU32,
    locked: AtomicU32,
}

const PPB_BIAS: i64 = 1 << 31;

impl CueMonitor {
    pub fn new() -> CueMonitor {
        CueMonitor::default()
    }
    /// Buffers in which the ring ran dry.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    /// Times a fill far past the target was skipped down to it.
    pub fn skips(&self) -> u64 {
        self.skips.load(Ordering::Relaxed)
    }
    /// The ring's smoothed fill and the target, in master frames.
    pub fn fill_frames(&self) -> (u32, u32) {
        (
            self.fill.load(Ordering::Relaxed),
            self.target.load(Ordering::Relaxed),
        )
    }
    /// The correction to the nominal ratio, ppm (the clocks' drift once locked).
    pub fn correction_ppm(&self) -> f64 {
        (self.correction_ppb.load(Ordering::Relaxed) as i64 - PPB_BIAS) as f64 / 1000.0
    }
    /// Playing (false while priming, before the first buffer and after an underrun).
    pub fn playing(&self) -> bool {
        self.locked.load(Ordering::Relaxed) != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Silence until the ring holds the target.
    Priming,
    Playing,
}

/// The cue device's side of the ring: see the module doc.
pub struct CueResampler {
    cfg: CueConfig,
    nominal: f64,
    kp: f64,
    ki: f64,
    /// The integrator, seconds·seconds (its `ki` multiple is the drift estimate).
    integ: f64,
    correction: f64,
    /// The smoothed fill, frames; None until the first measure.
    smooth: Option<f64>,
    target: f64,
    /// Extra target from underruns, frames.
    extra: f64,
    /// The largest master push and cue pull seen, frames.
    max_push: f64,
    max_pull: f64,
    /// The last four input frames; the read point is `frac` past `hist[1]`.
    hist: [[f32; 2]; 4],
    frac: f64,
    state: State,
    underruns: u64,
    skips: u64,
}

/// Catmull-Rom through `y[0..4]` at `t` in [0, 1) past `y[1]`.
#[inline]
pub fn catmull_rom(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
    let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c = -0.5 * y0 + 0.5 * y2;
    ((a * t + b) * t + c) * t + y1
}

impl CueResampler {
    pub fn new(cfg: CueConfig) -> CueResampler {
        let w = 2.0 * std::f64::consts::PI * cfg.loop_hz;
        // error e (s of latency): de/dt = drift - c, c = kp e + ki ∫e: s² + kp s + ki,
        // critically damped at w
        let (kp, ki) = (2.0 * w, w * w);
        let nominal = cfg.in_rate / cfg.out_rate;
        let target = cfg.min_target_s * cfg.in_rate;
        CueResampler {
            nominal,
            kp,
            ki,
            integ: 0.0,
            correction: 0.0,
            smooth: None,
            target,
            extra: 0.0,
            max_push: 0.0,
            max_pull: 0.0,
            hist: [[0.0; 2]; 4],
            frac: 0.0,
            state: State::Priming,
            underruns: 0,
            skips: 0,
            cfg,
        }
    }

    /// The input frames read per output frame now.
    pub fn ratio(&self) -> f64 {
        self.nominal * (1.0 + self.correction)
    }
    /// The correction to the nominal ratio (a fraction: 1e-6 is 1 ppm).
    pub fn correction(&self) -> f64 {
        self.correction
    }
    /// The loop's estimate of the clocks' drift (its integral term, a fraction): the
    /// correction with the momentary error taken out.
    pub fn drift_estimate(&self) -> f64 {
        self.ki * self.integ
    }
    /// The fill the loop holds, master frames.
    pub fn target(&self) -> f64 {
        self.target
    }
    /// The smoothed fill, master frames.
    pub fn fill_level(&self) -> f64 {
        self.smooth.unwrap_or(0.0)
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }
    pub fn skips(&self) -> u64 {
        self.skips
    }
    pub fn playing(&self) -> bool {
        self.state == State::Playing
    }

    fn retarget(&mut self) {
        let lo = self.cfg.min_target_s * self.cfg.in_rate;
        let hi = self.cfg.max_target_s * self.cfg.in_rate;
        // one push may land just after a pull; a quarter of both again for callback jitter
        let blocks = 1.25 * (self.max_push + self.max_pull * self.nominal) + 4.0;
        self.target = (blocks + self.extra).clamp(lo, hi.max(lo));
    }

    /// Fill one cue buffer (`channels` interleaved; the cue on the first two, or summed on a
    /// mono device, the rest silent) from `ring` (stereo interleaved, master frames). `lead`
    /// is [`CueStamp::lead`] now and `push` the master's last block ([`CueStamp::frames`]),
    /// both 0 when there is no stamp (the loop then sees a saw and works, but wobbles more).
    pub fn fill(
        &mut self,
        ring: &mut Consumer<f32>,
        channels: usize,
        out: &mut [f32],
        lead: f64,
        push: usize,
        monitor: Option<&CueMonitor>,
    ) {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        out.fill(0.0);
        if frames == 0 {
            return;
        }
        self.max_push = self.max_push.max(push as f64);
        self.max_pull = self.max_pull.max(frames as f64);
        self.retarget();

        let avail = (ring.slots() / 2) as f64;
        let fill = (avail - lead).max(0.0);
        let dt = frames as f64 / self.cfg.out_rate;

        if self.state == State::Priming {
            if fill < self.target {
                self.report(monitor);
                return;
            }
            self.skip_to(ring, fill, self.target);
            // the read point starts one frame in: hist = [x0, x0, x1, x2] after the loads
            for _ in 0..3 {
                if !self.load(ring) {
                    break;
                }
            }
            self.hist[0] = self.hist[1];
            self.frac = 0.0;
            self.smooth = Some(self.target);
            self.state = State::Playing;
        } else {
            let excess = self.target.max(0.05 * self.cfg.in_rate);
            if fill > self.target + excess {
                self.skip_to(ring, fill, self.target);
                self.skips += 1;
                self.smooth = Some(self.target);
            } else {
                let a = dt / (self.cfg.smooth_s + dt);
                let s = self.smooth.map_or(fill, |s| s + a * (fill - s));
                self.smooth = Some(s);
                let e = (s - self.target) / self.cfg.in_rate;
                let c = self.kp * e + self.ki * (self.integ + e * dt);
                if c.abs() < MAX_CORRECTION {
                    self.integ += e * dt;
                    self.correction = c;
                } else {
                    // saturated: hold the integrator (no wind-up)
                    self.correction = c.clamp(-MAX_CORRECTION, MAX_CORRECTION);
                }
            }
        }

        let step = self.ratio();
        for i in 0..frames {
            let t = self.frac as f32;
            let h = &self.hist;
            let l = catmull_rom(h[0][0], h[1][0], h[2][0], h[3][0], t);
            let r = catmull_rom(h[0][1], h[1][1], h[2][1], h[3][1], t);
            let f = &mut out[i * channels..(i + 1) * channels];
            if channels == 1 {
                f[0] = 0.5 * (l + r);
            } else {
                f[0] = l;
                f[1] = r;
            }
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                if !self.load(ring) {
                    // dry: the rest of this buffer is silence, and wait for the target again
                    self.underruns += 1;
                    self.extra = (self.extra + frames as f64 * self.nominal)
                        .min(self.cfg.max_target_s * self.cfg.in_rate);
                    self.state = State::Priming;
                    self.frac = 0.0;
                    self.report(monitor);
                    return;
                }
            }
        }
        self.report(monitor);
    }

    /// Shift one frame from the ring into the history; false when the ring is dry.
    fn load(&mut self, ring: &mut Consumer<f32>) -> bool {
        if ring.slots() < 2 {
            return false;
        }
        let l = ring.pop().unwrap_or(0.0);
        let r = ring.pop().unwrap_or(0.0);
        self.hist.copy_within(1..4, 0);
        self.hist[3] = [l, r];
        true
    }

    /// Drop the frames past `target` (keeping whole frames).
    fn skip_to(&mut self, ring: &mut Consumer<f32>, fill: f64, target: f64) {
        let drop = (fill - target).floor().max(0.0) as usize;
        let n = (drop * 2).min(ring.slots() / 2 * 2);
        if n > 0 {
            if let Ok(chunk) = ring.read_chunk(n) {
                chunk.commit_all();
            }
        }
    }

    fn report(&self, monitor: Option<&CueMonitor>) {
        let Some(m) = monitor else { return };
        m.underruns.store(self.underruns, Ordering::Relaxed);
        m.skips.store(self.skips, Ordering::Relaxed);
        m.fill.store(self.fill_level() as u32, Ordering::Relaxed);
        m.target.store(self.target as u32, Ordering::Relaxed);
        let ppb = (self.correction * 1e9).round() as i64 + PPB_BIAS;
        m.correction_ppb
            .store(ppb.clamp(0, u32::MAX as i64) as u32, Ordering::Relaxed);
        m.locked
            .store((self.state == State::Playing) as u32, Ordering::Relaxed);
    }
}
