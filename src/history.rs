//! A rolling history of an input, measured in beats, and a vinyl-style read head over it
//! (docs/INPUT-HISTORY.md).
//!
//! [`History`] is a mono ring with a fixed capacity, so the render path never allocates. How
//! much of it counts as "the history" is a *window in beats* ([`window_frames`]): the same
//! 32 beats are more frames at 90 BPM than at 150. [`Scrubber`] is the read head. It sits
//! between the live signal and the strip:
//!
//! - [`Mode::Live`]: the live signal passes untouched.
//! - [`Mode::Touch`]: the platter is held. The head chases the wheel's commanded position, so
//!   the sound follows the hand and is silent when the hand is still.
//! - [`Mode::Play`]: the platter is let go. The head plays at 1x and keeps its lag, which is a
//!   delay of whatever was scrubbed to.
//! - [`Mode::Loop`]: a section of whole beats copied out of the ring into a loop buffer. The
//!   loop's speed follows the tempo (`bpm / bpm_at_grab`), so it stays on the grid.
//!
//! Positions are absolute frame counts since the ring started (the newest frame is
//! `written - 1`), so a head does not move when the ring wraps. All maths is `f64`, and every
//! buffer is allocated in [`History::new`] / [`Scrubber::new`].

use alloc::vec;
use alloc::vec::Vec;

/// The slowest tempo the ring is sized for: below it the window gets shorter than its beats.
pub const MIN_BPM: f64 = 60.0;
/// The most beats a ring is sized for.
pub const MAX_BEATS: f64 = 64.0;
/// The default window, in beats.
pub const DEFAULT_BEATS: f64 = 64.0;
/// Loop lengths the buttons step through, in beats.
pub const LOOP_BEATS: [f64; 7] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// The default loop length.
pub const DEFAULT_LOOP_BEATS: f64 = 4.0;
/// Frames the head takes to fade in or out against the live signal.
const FADE_FRAMES: f64 = 64.0;
/// A held platter closes this fraction of its distance to the wheel each block.
const CHASE: f64 = 0.5;
/// Slack frames kept so the interpolator never reads a sample the ring just overwrote.
const GUARD: usize = 4;

/// Frames in one beat at `bpm`.
pub fn frames_per_beat(bpm: f64, sample_rate: f64) -> f64 {
    sample_rate * 60.0 / bpm.max(1.0)
}

/// Frames a ring needs to hold `beats` at the slowest tempo (`MIN_BPM`).
pub fn capacity_for(beats: f64, sample_rate: f64) -> usize {
    libm::ceil(frames_per_beat(MIN_BPM, sample_rate) * beats.max(1.0)) as usize + GUARD
}

/// How many frames `beats` span at `bpm`, held to what a ring of `capacity` can give.
pub fn window_frames(beats: f64, bpm: f64, sample_rate: f64, capacity: usize) -> usize {
    let w = libm::floor(frames_per_beat(bpm, sample_rate) * beats.max(0.0)) as usize;
    w.min(capacity.saturating_sub(GUARD))
}

/// The next loop length up (`dir > 0`) or down in [`LOOP_BEATS`].
pub fn step_loop_beats(current: f64, dir: i32) -> f64 {
    let i = LOOP_BEATS
        .iter()
        .position(|b| *b >= current - 1e-9)
        .unwrap_or(LOOP_BEATS.len() - 1);
    let j = (i as i32 + dir.signum()).clamp(0, LOOP_BEATS.len() as i32 - 1);
    LOOP_BEATS[j as usize]
}

/// The mono ring.
pub struct History {
    buf: Vec<f32>,
    /// Frames pushed since the start.
    written: u64,
    /// Where the next frame goes (`written % capacity`, kept without a division).
    head: usize,
}

impl History {
    pub fn new(capacity: usize) -> History {
        History {
            buf: vec![0.0; capacity.max(GUARD + 1)],
            written: 0,
            head: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Frames pushed since the start.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// The absolute index of the oldest frame still held.
    pub fn oldest(&self) -> u64 {
        self.written.saturating_sub((self.buf.len() - GUARD) as u64)
    }

    /// The absolute position of the newest frame.
    pub fn newest(&self) -> f64 {
        self.written.saturating_sub(1) as f64
    }

    pub fn push(&mut self, block: &[f32]) {
        let cap = self.buf.len();
        for &x in block {
            self.buf[self.head] = x;
            self.head += 1;
            if self.head == cap {
                self.head = 0;
            }
        }
        self.written += block.len() as u64;
    }

    /// The frame at absolute index `abs` (0 when it is not held).
    pub fn get(&self, abs: i64) -> f32 {
        if abs < self.oldest() as i64 || abs >= self.written as i64 {
            return 0.0;
        }
        self.buf[(abs as u64 % self.buf.len() as u64) as usize]
    }

    /// Linear interpolation at absolute position `pos`.
    pub fn read(&self, pos: f64) -> f32 {
        let i = libm::floor(pos);
        let f = (pos - i) as f32;
        let a = self.get(i as i64);
        let b = if (i as i64 + 1) >= self.written as i64 {
            a
        } else {
            self.get(i as i64 + 1)
        };
        a + (b - a) * f
    }
}

/// What the read head is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Touch,
    Play,
    Loop,
}

impl Mode {
    pub fn code(self) -> u8 {
        match self {
            Mode::Live => 0,
            Mode::Touch => 1,
            Mode::Play => 2,
            Mode::Loop => 3,
        }
    }
    pub fn of_code(c: u8) -> Mode {
        match c {
            1 => Mode::Touch,
            2 => Mode::Play,
            3 => Mode::Loop,
            _ => Mode::Live,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Live => "LIVE",
            Mode::Touch => "SCRUB",
            Mode::Play => "DELAY",
            Mode::Loop => "LOOP",
        }
    }
}

/// The read head over a [`History`].
pub struct Scrubber {
    mode: Mode,
    /// The head, as an absolute position (Touch, Play).
    pos: f64,
    /// Where the wheel has put it (Touch).
    target: f64,
    /// How much of the output is the head rather than the live signal, 0...1.
    weight: f64,
    loop_buf: Vec<f32>,
    loop_len: usize,
    loop_pos: f64,
    loop_bpm: f64,
    loop_beats: f64,
    /// The absolute index one past the loop's last frame (a re-grab keeps the end).
    loop_end: i64,
}

impl Scrubber {
    /// `loop_capacity` frames is the longest loop.
    pub fn new(loop_capacity: usize) -> Scrubber {
        Scrubber {
            mode: Mode::Live,
            pos: 0.0,
            target: 0.0,
            weight: 0.0,
            loop_buf: vec![0.0; loop_capacity.max(2)],
            loop_len: 0,
            loop_pos: 0.0,
            loop_bpm: 120.0,
            loop_beats: DEFAULT_LOOP_BEATS,
            loop_end: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The loop's length in beats while looping.
    pub fn loop_beats(&self) -> Option<f64> {
        (self.mode == Mode::Loop).then_some(self.loop_beats)
    }

    /// Frames the head is behind the newest frame (Touch, Play; 0 otherwise).
    pub fn lag_frames(&self, hist: &History) -> f64 {
        match self.mode {
            Mode::Touch | Mode::Play => (hist.newest() - self.pos).max(0.0),
            _ => 0.0,
        }
    }

    /// The platter is touched: the live signal gives way to the head, from where it was.
    pub fn touch(&mut self, hist: &History) {
        match self.mode {
            Mode::Touch => {}
            Mode::Play => {
                self.mode = Mode::Touch;
                self.target = self.pos;
            }
            Mode::Live | Mode::Loop => {
                self.pos = hist.newest();
                self.target = self.pos;
                self.mode = Mode::Touch;
            }
        }
    }

    /// The wheel turned by `frames` (negative: back in time). Touches first if it was not.
    pub fn scrub(&mut self, hist: &History, frames: f64) {
        if self.mode != Mode::Touch {
            self.touch(hist);
        }
        self.target += frames;
    }

    /// The platter is let go: the head plays on from where the wheel left it.
    pub fn release(&mut self) {
        if self.mode == Mode::Touch {
            self.pos = self.target;
            self.mode = Mode::Play;
        }
    }

    /// Back to the live signal (a short crossfade).
    pub fn live(&mut self) {
        self.mode = Mode::Live;
    }

    /// Loop `beats` whole beats ending at the head (or at the newest frame when live), at
    /// `bpm`. The copy is a memcpy of at most the loop buffer, once. Returns false when
    /// nothing has been recorded yet.
    pub fn grab_loop(
        &mut self,
        hist: &History,
        beats: f64,
        bpm: f64,
        sample_rate: f64,
    ) -> bool {
        let end = match self.mode {
            Mode::Touch | Mode::Play => libm::floor(self.pos) as i64 + 1,
            Mode::Loop => self.loop_end,
            Mode::Live => hist.written() as i64,
        };
        self.grab_loop_ending_at(hist, end, beats, bpm, sample_rate)
    }

    /// As [`Scrubber::grab_loop`] with a new length, keeping the loop's end (halve / double).
    pub fn regrab_loop(
        &mut self,
        hist: &History,
        beats: f64,
        bpm: f64,
        sample_rate: f64,
    ) -> bool {
        if self.mode != Mode::Loop {
            return false;
        }
        self.grab_loop_ending_at(hist, self.loop_end, beats, bpm, sample_rate)
    }

    fn grab_loop_ending_at(
        &mut self,
        hist: &History,
        end: i64,
        beats: f64,
        bpm: f64,
        sample_rate: f64,
    ) -> bool {
        let want = libm::round(frames_per_beat(bpm, sample_rate) * beats) as usize;
        let held = (end - hist.oldest() as i64).max(0) as usize;
        let len = want.min(self.loop_buf.len()).min(held);
        if len < 2 {
            return false;
        }
        let start = end - len as i64;
        for k in 0..len {
            self.loop_buf[k] = hist.get(start + k as i64);
        }
        self.loop_len = len;
        self.loop_pos = 0.0;
        self.loop_bpm = bpm.max(1.0);
        self.loop_beats = beats;
        self.loop_end = end;
        self.mode = Mode::Loop;
        true
    }

    fn loop_at(&self, pos: f64) -> f32 {
        let i = libm::floor(pos) as usize;
        let f = (pos - libm::floor(pos)) as f32;
        let a = self.loop_buf[i % self.loop_len];
        let b = self.loop_buf[(i + 1) % self.loop_len];
        a + (b - a) * f
    }

    /// Replace `block` (the live signal, already pushed into `hist`) with what the head reads,
    /// crossfading at the edges. `window` is [`window_frames`] now; `bpm` is the master tempo.
    pub fn process(&mut self, hist: &History, window: usize, bpm: f64, block: &mut [f32]) {
        let want = if self.mode == Mode::Live { 0.0 } else { 1.0 };
        if self.weight <= 0.0 && want == 0.0 {
            return;
        }
        let n = block.len();
        let newest = hist.newest();
        let lowest = (newest - window as f64).max(hist.oldest() as f64);
        if matches!(self.mode, Mode::Touch | Mode::Play | Mode::Live) {
            self.pos = self.pos.clamp(lowest, newest);
            self.target = self.target.clamp(lowest, newest);
        }
        if self.mode == Mode::Play && self.pos >= newest {
            // caught up with live
            self.mode = Mode::Live;
        }
        let want = if self.mode == Mode::Live { 0.0 } else { 1.0 };
        let step = 1.0 / FADE_FRAMES;
        // a held platter: how far it moves this block
        let v = if self.mode == Mode::Touch {
            let d = self.target - self.pos;
            if libm::fabs(d) < 0.5 {
                self.pos = self.target;
                0.0
            } else {
                d * CHASE / n.max(1) as f64
            }
        } else {
            0.0
        };
        let rate = if self.loop_bpm > 0.0 { bpm / self.loop_bpm } else { 1.0 };
        for x in block.iter_mut() {
            let s = match self.mode {
                Mode::Touch => {
                    let s = if v == 0.0 { 0.0 } else { hist.read(self.pos) };
                    self.pos += v;
                    s
                }
                // Live, fading out, keeps playing at 1x like Play
                Mode::Play | Mode::Live => {
                    let s = hist.read(self.pos);
                    self.pos = (self.pos + 1.0).min(newest + 1.0);
                    s
                }
                Mode::Loop => {
                    let s = self.loop_at(self.loop_pos);
                    self.loop_pos += rate;
                    let len = self.loop_len as f64;
                    if self.loop_pos >= len {
                        self.loop_pos -= len * libm::floor(self.loop_pos / len);
                    }
                    s
                }
            };
            if self.weight < want {
                self.weight = (self.weight + step).min(want);
            } else if self.weight > want {
                self.weight = (self.weight - step).max(want);
            }
            *x = *x * (1.0 - self.weight as f32) + s * self.weight as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// A ramp: frame k holds k / 1e6, so a position reads back as its own index.
    fn ramp(h: &mut History, from: usize, n: usize) {
        let v: Vec<f32> = (from..from + n).map(|k| k as f32 / 1e6).collect();
        h.push(&v);
    }

    #[test]
    fn the_window_is_beats_not_seconds() {
        let cap = capacity_for(MAX_BEATS, SR);
        assert_eq!(window_frames(32.0, 120.0, SR, cap), 32 * 24_000);
        assert_eq!(window_frames(32.0, 60.0, SR, cap), 32 * 48_000);
        // faster tempo: fewer frames for the same beats
        assert_eq!(window_frames(32.0, 150.0, SR, cap), 32 * 19_200);
        // slower than the ring is sized for: held to the capacity
        assert!(window_frames(64.0, 30.0, SR, cap) <= cap);
    }

    #[test]
    fn the_ring_wraps_and_reads_back_absolute_positions() {
        let mut h = History::new(1000);
        ramp(&mut h, 0, 2500);
        assert_eq!(h.written(), 2500);
        assert_eq!(h.oldest(), 2500 - (1000 - GUARD) as u64);
        assert!((h.get(2499) - 0.002499).abs() < 1e-9);
        assert_eq!(h.get(100), 0.0, "overwritten frames read as silence");
        assert!((h.read(2000.5) - 0.0020005).abs() < 1e-8);
    }

    #[test]
    fn loop_lengths_step() {
        assert_eq!(step_loop_beats(4.0, 1), 8.0);
        assert_eq!(step_loop_beats(4.0, -1), 2.0);
        assert_eq!(step_loop_beats(0.5, -1), 0.5);
        assert_eq!(step_loop_beats(32.0, 1), 32.0);
    }

    #[test]
    fn live_passes_the_signal_untouched() {
        let mut h = History::new(4000);
        let mut s = Scrubber::new(4000);
        let mut b = [0.25f32; 64];
        h.push(&b);
        s.process(&h, 3000, 120.0, &mut b);
        assert!(b.iter().all(|x| *x == 0.25));
    }

    #[test]
    fn a_scrub_back_lands_on_the_beat() {
        let mut h = History::new(capacity_for(MAX_BEATS, SR));
        let mut s = Scrubber::new(capacity_for(MAX_BEATS, SR));
        let win = window_frames(32.0, 120.0, SR, h.capacity());
        for b in 0..600 {
            ramp(&mut h, b * 256, 256);
        }
        let fpb = frames_per_beat(120.0, SR);
        s.touch(&h);
        s.scrub(&h, -4.0 * fpb);
        // settle: the head chases the wheel
        for k in 0..40 {
            let mut blk = [0.0f32; 256];
            ramp(&mut h, (600 + k) * 256, 256);
            s.process(&h, win, 120.0, &mut blk);
        }
        let want = 4.0 * fpb; // plus the time the platter was held: the live signal moves on
        let lag = s.lag_frames(&h);
        let held_for = 40.0 * 256.0;
        assert!(
            (lag - (want + held_for)).abs() < 1.0,
            "lag {lag} vs {} beats back + {held_for} held",
            want / fpb
        );
        assert_eq!(s.mode(), Mode::Touch);
    }

    #[test]
    fn a_released_platter_plays_a_delay_of_the_scrubbed_beats() {
        let cap = capacity_for(MAX_BEATS, SR);
        let mut h = History::new(cap);
        let mut s = Scrubber::new(cap);
        let win = window_frames(32.0, 120.0, SR, cap);
        let mut at = 0usize;
        for _ in 0..400 {
            ramp(&mut h, at, 256);
            at += 256;
        }
        let fpb = frames_per_beat(120.0, SR);
        s.touch(&h);
        s.scrub(&h, -2.0 * fpb);
        for _ in 0..30 {
            let mut blk = [0.0f32; 256];
            ramp(&mut h, at, 256);
            at += 256;
            s.process(&h, win, 120.0, &mut blk);
        }
        s.release();
        let lag0 = s.lag_frames(&h);
        let mut blk = [0.0f32; 256];
        for _ in 0..50 {
            ramp(&mut h, at, 256);
            at += 256;
            blk = [0.0; 256];
            s.process(&h, win, 120.0, &mut blk);
        }
        assert_eq!(s.mode(), Mode::Play);
        assert!((s.lag_frames(&h) - lag0).abs() < 1.0, "the delay holds");
        // and the output is the ramp `lag0` frames ago
        let last = at - 1;
        let want = (last as f64 - lag0) as f32 / 1e6;
        assert!((blk[255] - want).abs() < 1e-5, "{} vs {want}", blk[255]);
    }

    #[test]
    fn live_brings_it_back_with_a_fade() {
        let cap = capacity_for(8.0, SR);
        let mut h = History::new(cap);
        let mut s = Scrubber::new(cap);
        let mut at = 0usize;
        for _ in 0..100 {
            ramp(&mut h, at, 256);
            at += 256;
        }
        s.touch(&h);
        s.scrub(&h, -10_000.0);
        s.release();
        let mut b = [1.0f32; 256];
        ramp(&mut h, at, 256);
        at += 256;
        s.process(&h, cap, 120.0, &mut b);
        s.live();
        assert_eq!(s.mode(), Mode::Live);
        for _ in 0..3 {
            b = [1.0; 256];
            ramp(&mut h, at, 256);
            at += 256;
            s.process(&h, cap, 120.0, &mut b);
        }
        assert!(b.iter().all(|x| *x == 1.0), "fully live again");
    }

    #[test]
    fn a_loop_repeats_whole_beats_and_follows_the_tempo() {
        let cap = capacity_for(MAX_BEATS, SR);
        let mut h = History::new(cap);
        let mut s = Scrubber::new(cap);
        let mut at = 0usize;
        for _ in 0..400 {
            ramp(&mut h, at, 256);
            at += 256;
        }
        let fpb = frames_per_beat(120.0, SR);
        assert!(s.grab_loop(&h, 1.0, 120.0, SR));
        assert_eq!(s.mode(), Mode::Loop);
        assert_eq!(s.loop_beats(), Some(1.0));
        let len = fpb as usize; // 24 000
        // after the fade-in, one loop later the output repeats exactly
        let mut out = vec![];
        for _ in 0..(3 * len / 256 + 2) {
            ramp(&mut h, at, 256);
            at += 256;
            let mut b = [0.0f32; 256];
            s.process(&h, cap, 120.0, &mut b);
            out.extend_from_slice(&b);
        }
        for k in 1000..1100 {
            assert!((out[k] - out[k + len]).abs() < 1e-6, "k={k}");
            assert!((out[k] - out[k + 2 * len]).abs() < 1e-6);
        }
        // the loop is the last beat: frame 0 of the loop is `len` before the end
        let start = (400 * 256 - len) as f32 / 1e6;
        assert!((out[2000] - (start + 2000.0 / 1e6)).abs() < 1e-5);
        // double the tempo: the loop runs twice as fast, so it repeats at half the length
        let mut out2 = vec![];
        for _ in 0..(2 * len / 256) {
            ramp(&mut h, at, 256);
            at += 256;
            let mut b = [0.0f32; 256];
            s.process(&h, cap, 240.0, &mut b);
            out2.extend_from_slice(&b);
        }
        let half = len / 2;
        // find a phase-matched pair: the period is len/2 at 2x
        let k0 = 5000;
        assert!((out2[k0] - out2[k0 + half]).abs() < 1e-3);
    }

    #[test]
    fn a_loop_can_be_halved_keeping_its_end() {
        let cap = capacity_for(MAX_BEATS, SR);
        let mut h = History::new(cap);
        let mut s = Scrubber::new(cap);
        for b in 0..400 {
            ramp(&mut h, b * 256, 256);
        }
        assert!(s.grab_loop(&h, 4.0, 120.0, SR));
        assert!(s.regrab_loop(&h, 2.0, 120.0, SR));
        assert_eq!(s.loop_beats(), Some(2.0));
        assert!(!{
            let mut t = Scrubber::new(10);
            t.regrab_loop(&h, 2.0, 120.0, SR)
        });
    }

    #[test]
    fn nothing_recorded_means_no_loop() {
        let h = History::new(1000);
        let mut s = Scrubber::new(1000);
        assert!(!s.grab_loop(&h, 4.0, 120.0, SR));
        assert_eq!(s.mode(), Mode::Live);
    }

    #[test]
    fn a_shrinking_window_pulls_the_head_in() {
        let cap = capacity_for(MAX_BEATS, SR);
        let mut h = History::new(cap);
        let mut s = Scrubber::new(cap);
        for b in 0..2000 {
            ramp(&mut h, b * 256, 256);
        }
        let fpb = frames_per_beat(120.0, SR);
        s.touch(&h);
        s.scrub(&h, -30.0 * fpb);
        s.release();
        // tempo rises to 240: 8 beats is 12 000 frames
        let win = window_frames(8.0, 240.0, SR, cap);
        ramp(&mut h, 2000 * 256, 256);
        let mut b = [0.0f32; 256];
        s.process(&h, win, 240.0, &mut b);
        assert!(s.lag_frames(&h) <= win as f64 + 256.0);
    }
}
