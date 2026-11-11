//! What a deck played lately, for the main thread (`CaptureTap.swift`): the strip's output
//! after its EQ (before the fader), in a ring of about 22 s at 48 kHz, keyed by deck frame,
//! and the net clock's downbeats (deck frame, beat) as they were played.
//!
//! The render thread writes; the main thread copies out of it (LOOP record takes, grabbing the
//! last bars, the scrolling waveform). Samples are stored as relaxed atomics, so a copy racing
//! the writer never reads torn memory; [`CaptureTap::copy`] stays a block's margin behind the
//! oldest frame still held, so what it copies is what was played.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

/// Frames held: 2^20, ~21.8 s at 48 kHz.
pub const CAPTURE_FRAMES: usize = 1 << 20;
/// How far behind the oldest frame a copy keeps (the writer may be inside its next block).
const MARGIN: i64 = 8192;
/// Downbeats remembered.
pub const DOWNBEAT_SLOTS: usize = 64;

pub struct CaptureTap {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    /// The deck frame after the last one written; -1: nothing yet.
    end: AtomicI64,
    /// The first frame of the run being written (a jump in frames starts a new one).
    start: AtomicI64,
    down_frames: [AtomicI64; DOWNBEAT_SLOTS],
    down_beats: [AtomicI64; DOWNBEAT_SLOTS],
    down_written: AtomicU64,
}

impl Default for CaptureTap {
    fn default() -> Self {
        CaptureTap::new()
    }
}

impl CaptureTap {
    pub fn new() -> CaptureTap {
        let ring = || (0..CAPTURE_FRAMES).map(|_| AtomicU32::new(0)).collect();
        CaptureTap {
            left: ring(),
            right: ring(),
            end: AtomicI64::new(-1),
            start: AtomicI64::new(-1),
            down_frames: std::array::from_fn(|_| AtomicI64::new(0)),
            down_beats: std::array::from_fn(|_| AtomicI64::new(0)),
            down_written: AtomicU64::new(0),
        }
    }

    /// Render thread: the block at deck frame `frame`. Real-time safe.
    pub fn write(&self, frame: i64, left: &[f32], right: &[f32]) {
        let mask = CAPTURE_FRAMES - 1;
        if self.end.load(Ordering::Relaxed) != frame {
            self.start.store(frame, Ordering::Relaxed);
        }
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            let k = (frame as usize).wrapping_add(i) & mask;
            self.left[k].store(l.to_bits(), Ordering::Relaxed);
            self.right[k].store(r.to_bits(), Ordering::Relaxed);
        }
        self.end.store(
            frame + left.len().min(right.len()) as i64,
            Ordering::Release,
        );
    }

    /// Render thread: a downbeat of the net clock at deck frame `frame`, beat `beat`.
    pub fn downbeat(&self, frame: i64, beat: i64) {
        let n = self.down_written.load(Ordering::Relaxed);
        let k = (n as usize) % DOWNBEAT_SLOTS;
        self.down_frames[k].store(frame, Ordering::Relaxed);
        self.down_beats[k].store(beat, Ordering::Relaxed);
        self.down_written.store(n + 1, Ordering::Release);
    }

    /// The oldest frame a copy can still read, and the frame after the newest (None: nothing
    /// written yet).
    pub fn span(&self) -> Option<(i64, i64)> {
        let end = self.end.load(Ordering::Acquire);
        if end < 0 {
            return None;
        }
        let oldest = (end - CAPTURE_FRAMES as i64 + MARGIN).max(self.start.load(Ordering::Relaxed));
        Some((oldest.min(end), end))
    }

    /// Main thread: append frames `from..to` (as far as written) to `left` / `right`. Frames
    /// the ring no longer holds (overwritten, or before the deck first rendered) come back as
    /// silence, so a slow reader gets a take of the right length with a hole, never a shifted
    /// one (`CaptureTap.copy`). Returns the frame after the last one copied: `from` when there is
    /// nothing to copy (nothing written, or `from` is at or past `min(to, end)`).
    pub fn copy(&self, from: i64, to: i64, left: &mut Vec<f32>, right: &mut Vec<f32>) -> i64 {
        let Some((oldest, end)) = self.span() else {
            return from;
        };
        let stop = to.min(end);
        if from >= stop {
            return from;
        }
        let mut cursor = from;
        if cursor < oldest {
            // lost to the ring, or before the first frame written: silence of the same length
            let n = (oldest.min(stop) - cursor) as usize;
            left.resize(left.len() + n, 0.0);
            right.resize(right.len() + n, 0.0);
            cursor += n as i64;
        }
        let mask = CAPTURE_FRAMES - 1;
        for f in cursor..stop {
            let k = f as usize & mask;
            left.push(f32::from_bits(self.left[k].load(Ordering::Relaxed)));
            right.push(f32::from_bits(self.right[k].load(Ordering::Relaxed)));
        }
        stop
    }

    /// Main thread: the downbeats written since `cursor` (moving it on): (deck frame, beat).
    /// Ones the ring has already dropped are skipped.
    pub fn downbeats(&self, cursor: &mut u64) -> Vec<(i64, i64)> {
        let n = self.down_written.load(Ordering::Acquire);
        let first = (*cursor).max(n.saturating_sub(DOWNBEAT_SLOTS as u64 - 1));
        let out = (first..n)
            .map(|i| {
                let k = (i as usize) % DOWNBEAT_SLOTS;
                (
                    self.down_frames[k].load(Ordering::Relaxed),
                    self.down_beats[k].load(Ordering::Relaxed),
                )
            })
            .collect();
        *cursor = n;
        out
    }

    /// How many downbeats were ever written (a cursor that reads only new ones).
    pub fn downbeats_written(&self) -> u64 {
        self.down_written.load(Ordering::Acquire)
    }
}

/// What the mix buses played lately, keyed by render frame, for grabbing and recording them to
/// the loop row ([`Engine::source_grab`](super::engine::Engine::source_grab), docs/LOOPS.md
/// "Sources"): the vocoder strip `V` after its fader and before it, and the master output `M`
/// after the master fader. Downbeats are not used (they come from the master clock).
pub struct BusTaps {
    /// `V` after its fader (as an insert, `V` has no fader of its own: the insert's output).
    pub vocoder: CaptureTap,
    /// `V` before its fader.
    pub vocoder_pre: CaptureTap,
    /// `M`, the master output after the master fader.
    pub master: CaptureTap,
    /// `L`, the loop row after its channel's fader.
    pub loops: CaptureTap,
}

impl Default for BusTaps {
    fn default() -> Self {
        BusTaps {
            vocoder: CaptureTap::new(),
            vocoder_pre: CaptureTap::new(),
            master: CaptureTap::new(),
            loops: CaptureTap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write frames `a..b` of a counter ramp (left = frame, right = -frame) in `block`-sized
    /// pieces, as the render thread does. Values stay under 2^24, so f32 holds them exactly.
    fn write_ramp(tap: &CaptureTap, a: i64, b: i64, block: i64) {
        let mut f = a;
        while f < b {
            let n = block.min(b - f);
            let l: Vec<f32> = (f..f + n).map(|i| i as f32).collect();
            let r: Vec<f32> = l.iter().map(|v| -v).collect();
            tap.write(f, &l, &r);
            f += n;
        }
    }

    /// `CaptureTap.copy`: "Frames already overwritten come back as silence, so a slow reader
    /// gets a take of the right length with a hole, never a shifted one." Swift has no test of
    /// it (the contract is the doc comment), so these are Rust-only.
    #[test]
    fn a_reader_behind_the_ring_still_gets_the_length_it_asked_for() {
        let tap = CaptureTap::new();
        let end = (CAPTURE_FRAMES as f64 * 2.5) as i64; // two and a half laps
        write_ramp(&tap, 0, end, 4096);
        let (oldest, span_end) = tap.span().unwrap();
        assert_eq!(span_end, end);
        assert_eq!(oldest, end - CAPTURE_FRAMES as i64 + MARGIN, "the ring's tail, less the margin");
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let next = tap.copy(0, end, &mut l, &mut r);
        assert_eq!(next, end, "the cursor reaches the end");
        assert_eq!(l.len() as i64, end, "exactly to - from frames, not what survived");
        assert_eq!(r.len() as i64, end);
        let hole = oldest as usize;
        assert!(
            l[..hole].iter().all(|v| *v == 0.0) && r[..hole].iter().all(|v| *v == 0.0),
            "head is silence"
        );
        for f in (hole..end as usize).step_by(997).chain([end as usize - 1]) {
            assert_eq!((l[f], r[f]), (f as f32, -(f as f32)), "frame {f} is where it was played");
        }
    }

    #[test]
    fn frames_before_the_first_one_written_are_silence_in_place() {
        let tap = CaptureTap::new();
        write_ramp(&tap, 1000, 5000, 512);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        assert_eq!(tap.copy(0, 5000, &mut l, &mut r), 5000);
        assert_eq!((l.len(), r.len()), (5000, 5000));
        assert!(l[..1000].iter().all(|v| *v == 0.0));
        assert_eq!((l[1000], l[4999], r[1000]), (1000.0, 4999.0, -1000.0));
        // a range that ends before the first frame is all silence, and the cursor moves to its end
        let (mut l, mut r) = (Vec::new(), Vec::new());
        assert_eq!(tap.copy(100, 300, &mut l, &mut r), 300);
        assert!(l.len() == 200 && l.iter().chain(&r).all(|v| *v == 0.0));
    }

    #[test]
    fn a_range_the_ring_holds_is_what_was_written() {
        let tap = CaptureTap::new();
        let end = CAPTURE_FRAMES as i64 + 30_000; // one lap and a bit
        write_ramp(&tap, 0, end, 4096);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let from = end - 20_000;
        assert_eq!(tap.copy(from, end + 5_000, &mut l, &mut r), end, "as far as written");
        assert_eq!(l.len(), 20_000);
        assert!(l.iter().zip(from..end).all(|(v, f)| *v == f as f32));
        assert!(r.iter().zip(from..end).all(|(v, f)| *v == -(f as f32)));
    }

    #[test]
    fn nothing_to_copy_returns_the_cursor_and_appends_nothing() {
        let tap = CaptureTap::new();
        let (mut l, mut r) = (Vec::new(), Vec::new());
        assert_eq!(tap.copy(10, 20, &mut l, &mut r), 10, "nothing written yet");
        write_ramp(&tap, 0, 1000, 256);
        assert_eq!(tap.copy(500, 500, &mut l, &mut r), 500, "an empty range");
        assert_eq!(tap.copy(900, 400, &mut l, &mut r), 900, "a backwards range");
        assert_eq!(tap.copy(1000, 2000, &mut l, &mut r), 1000, "nothing is written at `from` yet");
        assert!(l.is_empty() && r.is_empty());
    }
}
