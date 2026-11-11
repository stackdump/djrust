//! dj's `vinyl-brake` on a net deck (`TurntableBrake.swift`; `WaveOptions.turntableBrake`, on in
//! `.extended`): the deck's platter spins down to a stop, then the record drops back in where
//! it would have been.
//!
//! beats' catalog makes vinyl-brake a compound of tape-stop (0 ms) and octave-down (60 ms), but
//! beats' `runCompound` skips tempo-sweep steps, so all it plays is an octave-down hold. dj
//! keeps that on the parity path ([`super::automation`]) and plays a real brake here
//! (GO-DIVERGENCE.md §5).
//!
//! Varispeed on the whole deck: while braking, the runner renders its net in *source time* at
//! rate r(t) <= 1. Each output frame moves a read head r source frames on; the runner renders
//! just the source frames the head reaches (at most one per output frame), and a 4-point cubic
//! (Catmull-Rom) reads between them. The net's tick clock counts source frames, so tempo and
//! pitch fall together for everything the runner plays. The master chain runs after this, on
//! the output (real time).
//!
//! The curve, from the frame the macro is applied: the macro's duration D (bars x 16 ticks at
//! the tempo when fired; default 1 bar) is three quarters spin-down and a quarter stopped. The
//! spin-down is a constant deceleration, r = 1 - t / (0.75 D), with the level following r over
//! its last tenth. Then silence until D.
//!
//! Release (at D, or 5 ms after a panic, faded down over them): the runner's tick clock is put
//! on by the ticks a deck that never braked would have run since the fire
//! ([`BrakeSource::catch_up`]), so the deck lands on the beat and bar it would be on, and comes
//! in over a 5 ms fade. A transport stop or a hand on the platter (freeze) cancels the brake
//! where it is.
//!
//! Real-time: buffers are allocated in [`TurntableBrake::new`]; nothing on the render path
//! allocates. A chunk takes at most [`TurntableBrake::MAX_CHUNK`] output frames.

use alloc::vec;
use alloc::vec::Vec;

/// What a [`TurntableBrake`] slows down: a net deck's runner, the loop channel's bank, or a
/// sample deck's turntable. Audio thread only; nothing here may allocate.
pub trait BrakeSource {
    /// Is its clock running (the brake's elapsed time only counts while it is)?
    fn playing(&self) -> bool;
    /// Render `frames` source frames into `left` (`right`), stamping events from `start_frame`.
    fn process(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        start_frame: usize,
    );
    /// How far an unbraked source runs ahead per output frame, in the units `catch_up` takes
    /// (the runner: ticks at its tempo; the bank: frames). Read before each braked chunk.
    fn brake_lag(&self, sample_rate: f64) -> f64;
    /// A braked chunk played `output` frames from `pulled` source frames (while running).
    fn brake_chunk(&mut self, output: usize, pulled: usize);
    /// The release: put the clock where an unbraked source would be, `ticks` (lag units) on.
    fn catch_up(&mut self, ticks: f64);
}

pub struct TurntableBrake {
    pub sample_rate: f64,
    pub fade_frames: usize,
    src_l: Vec<f32>,
    src_r: Vec<f32>,

    /// Braking (or stopped, waiting for the release).
    is_on: bool,
    /// Output frames since the fire, counted while the transport runs.
    elapsed: usize,
    /// Frames from the fire to the stop (r = 0) and to the release.
    stop_frames: f64,
    release_frame: usize,
    /// Ticks a deck that never braked would be ahead of this one's source clock.
    deficit_ticks: f64,
    // The read head: between window samples 1 and 2, `phase` of the way.
    phase: f64,
    l: [f32; 4],
    r: [f32; 4],
    /// Window samples still to pull before the first output (the onset's look-ahead).
    fill: usize,
    /// > 0: a panic is releasing the brake, the output fading out over these frames.
    panic_left: usize,
    /// > 0: the direct output fading in after a release.
    fade_in_left: usize,
    // The last frame the direct path played (before the master chain): the window's first sample.
    last_l: f32,
    last_r: f32,
}

impl TurntableBrake {
    /// Output frames per braked chunk: with the onset's three look-ahead pulls the runner is
    /// asked for at most 4095 source frames (its own scratch and the samplers' hold 4096).
    pub const MAX_CHUNK: usize = 4092;
    /// The share of the duration spent spinning down (the rest is the stop).
    pub const SPIN_DOWN: f64 = 0.75;
    /// Below this rate the level follows the rate down to 0.
    pub const TAIL_RATE: f64 = 0.1;

    pub fn new(sample_rate: f64) -> TurntableBrake {
        TurntableBrake {
            sample_rate,
            fade_frames: 1usize.max(libm::round(0.005 * sample_rate) as usize),
            src_l: vec![0.0; Self::MAX_CHUNK + 4],
            src_r: vec![0.0; Self::MAX_CHUNK + 4],
            is_on: false,
            elapsed: 0,
            stop_frames: 1.0,
            release_frame: 0,
            deficit_ticks: 0.0,
            phase: 0.0,
            l: [0.0; 4],
            r: [0.0; 4],
            fill: 0,
            panic_left: 0,
            fade_in_left: 0,
            last_l: 0.0,
            last_r: 0.0,
        }
    }

    pub fn is_on(&self) -> bool {
        self.is_on
    }
    pub fn elapsed(&self) -> usize {
        self.elapsed
    }
    pub fn stop_frames(&self) -> f64 {
        self.stop_frames
    }
    pub fn release_frame(&self) -> usize {
        self.release_frame
    }
    pub fn deficit_ticks(&self) -> f64 {
        self.deficit_ticks
    }

    /// The rate `e` output frames after the fire (1 -> 0, then 0).
    #[inline]
    pub fn rate(&self, e: usize) -> f64 {
        (1.0 - e as f64 / self.stop_frames).max(0.0)
    }

    /// The level at rate `r`.
    #[inline]
    pub fn level(r: f64) -> f64 {
        (r / Self::TAIL_RATE).min(1.0)
    }

    /// Start a brake lasting `duration_frames` (spin-down and stop) from the next frame. False
    /// (nothing changes) while one is already running.
    pub fn start(&mut self, duration_frames: usize) -> bool {
        if self.is_on {
            return false;
        }
        self.is_on = true;
        self.elapsed = 0;
        self.release_frame = 1usize.max(duration_frames);
        self.stop_frames = 1.0f64.max(Self::SPIN_DOWN * self.release_frame as f64);
        self.deficit_ticks = 0.0;
        self.phase = 0.0;
        self.l[0] = self.last_l;
        self.r[0] = self.last_r;
        self.fill = 3;
        self.panic_left = 0;
        self.fade_in_left = 0;
        true
    }

    /// Release now: fade out over 5 ms, then drop back in time.
    pub fn panic(&mut self) {
        if self.is_on && self.panic_left == 0 {
            self.panic_left = self.fade_frames;
        }
    }

    /// Drop the brake where it is, without putting the clock on (a transport stop, a scratch).
    pub fn cancel(&mut self) {
        if !self.is_on {
            return;
        }
        self.is_on = false;
        self.panic_left = 0;
        self.fade_in_left = self.fade_frames;
    }

    /// Render `frames` of the deck's runner into `left` (`right`), braked while a brake runs.
    /// Real-time safe. `start_frame` stamps the runner's events (braked chunks stamp theirs
    /// from the chunk's first output frame).
    pub fn render<S: BrakeSource>(
        &mut self,
        runner: &mut S,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        frames: usize,
        start_frame: usize,
    ) {
        let mut off = 0;
        while off < frames {
            if !self.is_on {
                let n = frames - off;
                runner.process(
                    &mut left[off..off + n],
                    right.as_deref_mut().map(|r| &mut r[off..off + n]),
                    n,
                    start_frame + off,
                );
                self.fade_in(
                    &mut left[off..off + n],
                    right.as_deref_mut().map(|r| &mut r[off..off + n]),
                    n,
                );
                break;
            }
            let playing = runner.playing();
            let panicking = self.panic_left > 0;
            let mut n = (frames - off).min(Self::MAX_CHUNK);
            if panicking {
                n = n.min(self.panic_left);
            } else if playing {
                n = n.min(self.release_frame.saturating_sub(self.elapsed));
            }
            if n > 0 {
                self.braked(
                    runner,
                    &mut left[off..off + n],
                    right.as_deref_mut().map(|r| &mut r[off..off + n]),
                    n,
                    start_frame + off,
                    playing,
                );
                off += n;
            }
            if (panicking && self.panic_left == 0)
                || (!panicking && playing && self.elapsed >= self.release_frame)
            {
                self.is_on = false;
                self.panic_left = 0;
                runner.catch_up(self.deficit_ticks);
                self.fade_in_left = self.fade_frames;
            }
        }
        if frames > 0 {
            self.last_l = left[frames - 1];
            self.last_r = right.as_ref().map(|r| r[frames - 1]).unwrap_or(self.last_l);
        }
    }

    fn fade_in(&mut self, left: &mut [f32], mut right: Option<&mut [f32]>, n: usize) {
        let mut i = 0;
        while i < n && self.fade_in_left > 0 {
            let g = (self.fade_frames - self.fade_in_left) as f32 / self.fade_frames as f32;
            left[i] *= g;
            if let Some(r) = right.as_deref_mut() {
                r[i] *= g;
            }
            self.fade_in_left -= 1;
            i += 1;
        }
    }

    #[inline]
    fn cubic(x0: f32, x1: f32, x2: f32, x3: f32, t: f32) -> f32 {
        let c1 = 0.5 * (x2 - x0);
        let c2 = x0 - 2.5 * x1 + 2.0 * x2 - 0.5 * x3;
        let c3 = 0.5 * (x3 - x0) + 1.5 * (x1 - x2);
        ((c3 * t + c2) * t + c1) * t + x1
    }

    #[inline]
    fn push(&mut self, k: usize) {
        self.l = [self.l[1], self.l[2], self.l[3], self.src_l[k]];
        self.r = [self.r[1], self.r[2], self.r[3], self.src_r[k]];
    }

    /// `n` braked output frames.
    fn braked<S: BrakeSource>(
        &mut self,
        runner: &mut S,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        n: usize,
        start_frame: usize,
        playing: bool,
    ) {
        // how many source frames the head reaches (the same arithmetic as the loop below)
        let mut pulls = self.fill;
        let mut ph = self.phase;
        let mut e = self.elapsed;
        for _ in 0..n {
            ph += self.rate(e);
            if ph >= 1.0 {
                ph -= 1.0;
                pulls += 1;
            } // r <= 1 and ph < 1: at most one a frame
            if playing {
                e += 1;
            }
        }
        let ticks_per_frame = runner.brake_lag(self.sample_rate); // before a bar's hand-over
        let stereo = right.is_some();
        if pulls > 0 {
            runner.process(
                &mut self.src_l[..pulls],
                if stereo {
                    Some(&mut self.src_r[..pulls])
                } else {
                    None
                },
                pulls,
                start_frame,
            );
            if !stereo {
                let (l, r) = (&self.src_l, &mut self.src_r);
                r[..pulls].copy_from_slice(&l[..pulls]);
            }
        }
        if playing {
            // the shadow deck ran n frames, this one's clock `pulls`
            self.deficit_ticks += (n as f64 - pulls as f64) * ticks_per_frame;
            runner.brake_chunk(n, pulls);
        }
        let mut k = 0;
        while self.fill > 0 {
            self.push(k);
            k += 1;
            self.fill -= 1;
        }
        for j in 0..n {
            let rt = self.rate(self.elapsed);
            let mut g = Self::level(rt);
            if self.panic_left > 0 {
                g *= self.panic_left as f64 / self.fade_frames as f64;
                self.panic_left -= 1;
            }
            let t = self.phase as f32;
            left[j] = g as f32 * Self::cubic(self.l[0], self.l[1], self.l[2], self.l[3], t);
            if let Some(r) = right.as_deref_mut() {
                r[j] = g as f32 * Self::cubic(self.r[0], self.r[1], self.r[2], self.r[3], t);
            }
            self.phase += rt;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.push(k);
                k += 1;
            }
            if playing {
                self.elapsed += 1;
            }
        }
    }
}
