//! Port of `LatestRing.swift`: a ring of plain values where the producer never waits and never
//! fails: when it is full the oldest values are overwritten. For history that is sampled, not
//! consumed (the net clock's fires and tick anchors for a scope), where a late reader prefers
//! "the newest data and a count of what it missed" to back-pressure on the audio thread.
//!
//! Every value has an absolute sequence number: the first ever written is 0. A consumer keeps its
//! own cursor and pulls by sequence number ([`EventCursor`] for a deck's two streams); nothing it
//! does affects the producer.
//!
//! Threading contract: exactly one producer thread ([`LatestRing::write`]); any number of
//! consumers ([`LatestRing::read`], [`LatestRing::written`]), each with its own cursor. Neither
//! side allocates or locks after [`LatestRing::new`] (a consumer's [`LatestRing::drain`] allocates
//! only if its `Vec` has to grow, on its own thread).
//!
//! Protocol (a seqlock over the ring): the producer first publishes `claimed` ("everything below
//! `claimed - capacity` may be overwritten from now on"), then writes the values, then publishes
//! `written` ("everything below this is complete"). A reader copies up to `written`, and
//! afterwards re-reads `claimed`: any copied value older than `claimed - capacity` may have been
//! overwritten while it was being copied, so that prefix is thrown away and counted as lost.
//! Nothing torn is ever returned.
//!
//! The slots are plain memory under the seqlock, read and written with volatile accesses (the
//! textbook seqlock for a `Copy` payload; Swift's slots are the same). A reader that overlaps the
//! producer on a slot may copy bytes mid-write, and says so by the claim moving: those bytes are
//! never handed out, and `T` has no invalid bit patterns a half-copied value could break (it is
//! `Copy` plain data, and the copy is discarded before anything reads it as a `T`).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::{fence, AtomicUsize, Ordering};

use super::fire::{FireEvent, TickAnchor};

/// See the module comment.
pub struct LatestRing<T: Copy> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    /// Producer-owned: sequence numbers started.
    claimed: AtomicUsize,
    /// Producer-owned: sequence numbers completed.
    written: AtomicUsize,
}

// SAFETY: the producer is one thread and the readers only copy slots under the seqlock above;
// `T: Copy` plain data, `Send` because it crosses threads by value.
unsafe impl<T: Copy + Send> Sync for LatestRing<T> {}
unsafe impl<T: Copy + Send> Send for LatestRing<T> {}

impl<T: Copy> LatestRing<T> {
    /// `capacity` is rounded up to a power of two (at least 2).
    pub fn new(capacity: usize) -> LatestRing<T> {
        let mut n = 2;
        while n < capacity {
            n <<= 1;
        }
        LatestRing {
            slots: (0..n)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
            mask: n - 1,
            claimed: AtomicUsize::new(0),
            written: AtomicUsize::new(0),
        }
    }

    /// How many values the ring holds.
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Total values completed so far: the sequence number the next write will get. Readable from
    /// any thread.
    pub fn written(&self) -> usize {
        self.written.load(Ordering::Acquire)
    }

    /// Producer side. Never blocks; overwrites the oldest value when full.
    pub fn write(&self, value: T) {
        let w = self.written.load(Ordering::Relaxed);
        self.claimed.store(w + 1, Ordering::Relaxed);
        fence(Ordering::Release); // the claim is visible before the slot changes
        // SAFETY: the one producer owns slot `w & mask` until `written` moves past it.
        unsafe { ptr::write_volatile(self.slots[w & self.mask].get(), MaybeUninit::new(value)) };
        self.written.store(w + 1, Ordering::Release);
    }

    /// Consumer side. Copies the values from sequence number `from` on into `out` (as many as are
    /// complete and fit). Returns `(count, lost)`: the number copied (into `out[..count]`) and how
    /// many values were skipped before the first copied one because they were already
    /// overwritten: the consumer was more than a capacity behind, or the producer lapped it during
    /// the copy (the torn prefix is dropped). The first value returned has sequence number
    /// `from + lost`; advance the cursor by `lost + count`.
    pub fn read(&self, from: usize, out: &mut [T]) -> (usize, usize) {
        self.read_hooked(from, out, || {})
    }

    /// [`read`](Self::read) with a hook in the middle of the copy: a test's reader that gets
    /// preempted there while the producer laps it (`midCopyHook`).
    pub fn read_hooked(
        &self,
        from: usize,
        out: &mut [T],
        mid_copy: impl FnOnce(),
    ) -> (usize, usize) {
        // SAFETY: `MaybeUninit<T>` has `T`'s layout and every `T` is a valid `MaybeUninit<T>`;
        // what is written into `out` are copies of slots, and only the part `read_raw` keeps is
        // ever looked at as a `T`.
        let raw = unsafe { &mut *(out as *mut [T] as *mut [MaybeUninit<T>]) };
        self.read_raw(from, raw, mid_copy)
    }

    fn read_raw(
        &self,
        from: usize,
        out: &mut [MaybeUninit<T>],
        mid_copy: impl FnOnce(),
    ) -> (usize, usize) {
        if out.is_empty() {
            return (0, 0);
        }
        let w = self.written.load(Ordering::Acquire);
        let mut start = from;
        let mut lost = 0;
        if start >= w {
            return (0, 0);
        }
        let cap = self.capacity();
        if w > cap && start < w - cap {
            // fell behind by a whole lap
            lost = w - cap - start;
            start = w - cap;
        }
        let n = (w - start).min(out.len());
        let s = start & self.mask;
        let first = n.min(cap - s);
        self.copy(s, &mut out[..first]);
        mid_copy();
        if n > first {
            self.copy(0, &mut out[first..n]);
        }
        fence(Ordering::Acquire); // the copy is done before we look at the claim
        let claimed = self.claimed.load(Ordering::Relaxed);
        let safe_from = claimed.saturating_sub(cap);
        if safe_from > start {
            // the producer lapped part of what we just copied
            let torn = n.min(safe_from - start);
            lost += torn;
            let kept = n - torn;
            if kept > 0 {
                out.copy_within(torn..n, 0);
            }
            return (kept, lost);
        }
        (n, lost)
    }

    fn copy(&self, from_slot: usize, out: &mut [MaybeUninit<T>]) {
        for (k, o) in out.iter_mut().enumerate() {
            // SAFETY: a slot being overwritten is copied and then discarded by `read_raw` when
            // the claim says so; the copy is of `MaybeUninit` bytes, not looked at as a `T` here.
            *o = unsafe { ptr::read_volatile(self.slots[from_slot + k].get()) };
        }
    }

    /// Consumer side. Appends everything from `cursor` on, as of this call, to `out` and moves
    /// `cursor` past it. Returns how many values were lost (already overwritten when the reader
    /// got to them; skipped, never repeated). Allocates only if `out` has to grow, so it is for a
    /// consumer's own thread, never the producer's.
    pub fn drain(&self, cursor: &mut usize, out: &mut Vec<T>) -> usize {
        let end = self.written();
        let mut lost = 0;
        let mut chunk = [MaybeUninit::<T>::uninit(); 128];
        while *cursor < end {
            let (count, l) = self.read_raw(*cursor, &mut chunk, || {});
            if count == 0 && l == 0 {
                break;
            }
            out.extend(
                chunk[..count]
                    .iter()
                    // SAFETY: `read_raw` keeps only values below `written`: complete `T`s.
                    .map(|v| unsafe { v.assume_init() }),
            );
            lost += l;
            *cursor += l + count;
        }
        lost
    }
}

/// How many fires a deck's ring holds (`WaveDeckSource.init(fireCapacity:)`): minutes of an
/// ordinary song, many seconds of the densest, against a reader that polls at 30...60 Hz.
pub const FIRE_CAPACITY: usize = 8192;
/// How many tick anchors it holds (`anchorCapacity`).
pub const ANCHOR_CAPACITY: usize = 2048;

impl Default for LatestRing<FireEvent> {
    fn default() -> Self {
        LatestRing::new(FIRE_CAPACITY)
    }
}

impl Default for LatestRing<TickAnchor> {
    fn default() -> Self {
        LatestRing::new(ANCHOR_CAPACITY)
    }
}

/// A reader's place in a deck's two event streams: the next fire and the next anchor it has not
/// seen. Every reader (a deck's feed, the master's feed, a timeline) has its own, so readers
/// never disturb one another or the audio thread (`WaveDeckSource.EventCursor`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventCursor {
    pub fires: usize,
    pub anchors: usize,
}

impl EventCursor {
    /// `EventCursor()`: the oldest records still held.
    pub fn new() -> EventCursor {
        EventCursor::default()
    }

    /// A cursor for a reader that subscribes now: it sees the fires from now on, and the anchors
    /// from the newest one written so far (so the first anchor it reads places it on the net
    /// clock at once, and the fires of a tick that is half written still find their anchor)
    /// (`eventCursorNow`).
    pub fn now(fires: &LatestRing<FireEvent>, anchors: &LatestRing<TickAnchor>) -> EventCursor {
        let a = anchors.written(); // anchors first: a tick's anchor is written before its fires
        let f = fires.written();
        EventCursor {
            fires: f,
            anchors: a.saturating_sub(1),
        }
    }
}

/// Append everything written since `cursor` to `fires_out` and `anchors_out` (oldest first) and
/// move the cursor past it. Returns how many records of each stream were lost: overwritten before
/// this reader got to them, because it was a whole ring behind.
///
/// The fires are read first and the anchors second, and the audio thread writes a tick's anchor
/// before its fires, so every fire returned has its anchor (same `generation` and `tick`) in this
/// call's `anchors_out` or an earlier one's, unless that anchor was lost. Records are stamped in
/// the deck's frame time base and carry their project `generation` (resolve it with the deck's
/// `marking_layout_of`: which net, which lane, which song) (`readEvents`).
pub fn read_events(
    fires: &LatestRing<FireEvent>,
    anchors: &LatestRing<TickAnchor>,
    cursor: &mut EventCursor,
    fires_out: &mut Vec<FireEvent>,
    anchors_out: &mut Vec<TickAnchor>,
) -> (usize, usize) {
    let f = fires.drain(&mut cursor.fires, fires_out);
    let a = anchors.drain(&mut cursor.anchors, anchors_out);
    (f, a)
}
