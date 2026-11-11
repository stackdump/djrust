//! `LatestRing` against the Swift suite's own literals: a port of `LatestRingTests`
//! (`Tests/DJCoreTests/RingTests.swift`) and its two-thread stress (`RingStress.swift`,
//! `stressLatest`). Parity form (a): each test names the Swift test whose numbers it asserts.
//!
//! Not ported, because the Rust ring has no such writers (the deck writes single records, one a
//! fire and one a tick anchor): `write(contentsOf:)` and `write(repeating:count:)`, so
//! `batchWritesWrapAndKeepSequenceNumbers` and `floatHistoryRoundTripsExactly` have no Rust twin,
//! and the stress writes singles only. A negative cursor (`from: -4`) cannot be asked of a
//! `usize`.

use djrust::wave::LatestRing;

/// `readAll`: the values a read returns and how many it reports lost.
fn read_all(r: &LatestRing<i64>, from: usize, max: usize) -> (Vec<i64>, usize) {
    let mut buf = vec![-1i64; max];
    let (count, lost) = r.read(from, &mut buf);
    buf.truncate(count);
    (buf, lost)
}

fn range(a: i64, b: i64) -> Vec<i64> {
    (a..b).collect()
}

// LatestRingTests.capacityRoundsUpToAPowerOfTwo
#[test]
fn capacity_rounds_up_to_a_power_of_two() {
    assert_eq!(LatestRing::<i64>::new(8).capacity(), 8);
    assert_eq!(LatestRing::<i64>::new(5).capacity(), 8);
    assert_eq!(LatestRing::<i64>::new(1).capacity(), 2);
    assert_eq!(LatestRing::<f32>::new(1 << 16).capacity(), 1 << 16);
}

// LatestRingTests.readsByAbsoluteSequenceNumber
#[test]
fn reads_by_absolute_sequence_number() {
    let r = LatestRing::<i64>::new(8);
    assert_eq!(r.written(), 0);
    assert_eq!(read_all(&r, 0, 1000).0, Vec::<i64>::new());
    for i in 0..5 {
        r.write(i * 10);
    }
    assert_eq!(r.written(), 5);
    let all = read_all(&r, 0, 1000);
    assert_eq!(all, (vec![0, 10, 20, 30, 40], 0));
    assert_eq!(read_all(&r, 3, 1000).0, vec![30, 40], "any cursor, not only the oldest");
    assert_eq!(read_all(&r, 5, 1000).0, Vec::<i64>::new(), "nothing new yet");
    assert_eq!(read_all(&r, 99, 1000), (vec![], 0), "a future cursor");
    assert_eq!(read_all(&r, 0, 1000).0, all.0, "reading does not consume");
}

// LatestRingTests.overwritesTheOldestWhenFullAndReportsWhatWasLost
#[test]
fn overwrites_the_oldest_when_full_and_reports_what_was_lost() {
    let r = LatestRing::<i64>::new(8);
    for i in 0..20 {
        r.write(i);
    }
    assert_eq!(r.written(), 20);
    assert_eq!(read_all(&r, 0, 1000), (range(12, 20), 12), "12 overwritten, newest 8 kept");
    assert_eq!(
        read_all(&r, 12, 1000),
        (range(12, 20), 0),
        "exactly one capacity behind: lossless"
    );
    assert_eq!(read_all(&r, 11, 1000), (range(12, 20), 1));
    assert_eq!(read_all(&r, 15, 1000), (vec![15, 16, 17, 18, 19], 0));
}

// LatestRingTests.aSmallOutputBufferPagesThroughTheHistory
#[test]
fn a_small_output_buffer_pages_through_the_history() {
    let r = LatestRing::<i64>::new(16);
    for i in 0..13 {
        r.write(i);
    }
    let mut cursor = 0;
    let mut seen = Vec::new();
    loop {
        let (values, lost) = read_all(&r, cursor, 5);
        if values.is_empty() {
            break;
        }
        cursor += lost + values.len();
        seen.extend(values);
    }
    assert_eq!(seen, range(0, 13));
    assert_eq!(cursor, 13);
}

// LatestRingTests.aReaderLappedInTheMiddleOfItsCopyDropsTheTornPrefix
#[test]
fn a_reader_lapped_in_the_middle_of_its_copy_drops_the_torn_prefix() {
    // The "preemption" happens between the two halves of a wrapped copy, and the producer writes
    // a bit more than a lap meanwhile.
    let r = LatestRing::<i64>::new(8);
    for i in 0..12 {
        r.write(i); // sequence 4...11 are held; 4...7 sit in slots 4...7
    }
    let mut buf = vec![-1i64; 1000];
    let (count, lost) = r.read_hooked(4, &mut buf, || {
        for i in 12..16 {
            r.write(i); // overwrites slots 4...7, already copied
        }
    });
    // the first half was copied whole but could have been torn: conservatively lost
    assert_eq!(
        (lost, &buf[..count]),
        (4, &[8, 9, 10, 11][..]),
        "lost {lost} values {:?}",
        &buf[..count]
    );

    for i in 16..24 {
        r.write(i); // now 16...23 held, slots 0...7
    }
    let (count, lost) = r.read_hooked(18, &mut buf, || {
        for i in 24..32 {
            r.write(i); // a whole lap: every slot, including the unread half
        }
    });
    assert!(count == 0 && lost == 6, "nothing of the copy is trustworthy: {count}, {lost}");
    // and the ring itself is intact afterwards
    assert_eq!(read_all(&r, 24, 1000).0, range(24, 32));
}

// LatestRingTests.drainAppendsEverythingFromACursorAndAdvancesIt
#[test]
fn drain_appends_everything_from_a_cursor_and_advances_it() {
    let r = LatestRing::<i64>::new(512);
    for i in 0..300 {
        r.write(i); // more than one drain chunk
    }
    let mut out = Vec::new();
    let mut cursor = 40;
    assert_eq!(r.drain(&mut cursor, &mut out), 0);
    assert_eq!((out.clone(), cursor), (range(40, 300), 300));
    assert_eq!(r.drain(&mut cursor, &mut out), 0);
    assert_eq!(out.len(), 260, "nothing new");
    r.write(300);
    assert_eq!(r.drain(&mut cursor, &mut out), 0);
    assert_eq!((out.last().copied(), cursor), (Some(300), 301));
    // a reader that fell a lap behind skips ahead and says how much it missed
    for i in 301..1500 {
        r.write(i);
    }
    assert_eq!(r.drain(&mut cursor, &mut out), 1500 - 512 - 301);
    assert_eq!(&out[out.len() - 512..], &range(988, 1500)[..]);
    assert_eq!(cursor, 1500);
    // independent readers do not disturb one another
    let mut other = 1490;
    let mut theirs = Vec::new();
    r.drain(&mut other, &mut theirs);
    assert_eq!(theirs, range(1490, 1500));
}

/// `StressPayload`: a multi-word value whose words must always agree, so a torn read shows up as
/// a mismatch.
#[derive(Clone, Copy)]
struct StressPayload {
    seq: i64,
    inverse: i64,
    triple: i64,
    tag: i64,
}

impl StressPayload {
    fn new(n: i64) -> StressPayload {
        StressPayload {
            seq: n,
            inverse: !n,
            triple: n.wrapping_mul(3),
            tag: 0x5EED,
        }
    }
    fn is_consistent(&self) -> bool {
        self.inverse == !self.seq && self.triple == self.seq.wrapping_mul(3) && self.tag == 0x5EED
    }
}

#[derive(Default)]
struct Report {
    written: usize,
    received: usize,
    lost: usize,
    corrupt: usize,
    misplaced: usize,
    lost_reads: usize,
}

/// `stressLatest` (singles only): the producer thread writes `total` consecutive payloads as fast
/// as it can into a deliberately small ring while this thread pulls by sequence number. Whatever
/// a read returns must be untorn and exactly the sequence numbers the cursor arithmetic promises,
/// and every sequence number is either received or reported lost.
fn stress(total: usize, capacity: usize) -> Report {
    use std::sync::atomic::{AtomicBool, Ordering};
    let ring = LatestRing::<StressPayload>::new(capacity);
    let done = AtomicBool::new(false);
    let mut report = Report::default();
    std::thread::scope(|s| {
        s.spawn(|| {
            for n in 0..total {
                ring.write(StressPayload::new(n as i64));
            }
            done.store(true, Ordering::Release);
        });
        let mut cursor = 0usize;
        let mut out = [StressPayload::new(0); 53];
        loop {
            let finished_now = done.load(Ordering::Acquire);
            let (count, lost) = ring.read(cursor, &mut out);
            if lost > 0 {
                report.lost_reads += 1;
            }
            for (i, p) in out[..count].iter().enumerate() {
                if !p.is_consistent() {
                    report.corrupt += 1;
                }
                if p.seq as usize != cursor + lost + i {
                    report.misplaced += 1;
                }
            }
            report.received += count;
            report.lost += lost;
            cursor += lost + count;
            if finished_now && count == 0 && lost == 0 && cursor >= ring.written() {
                break;
            }
        }
    });
    report.written = ring.written();
    report
}

// LatestRingTests.aFastProducerNeverCorruptsASlowReader
#[test]
fn a_fast_producer_never_corrupts_a_slow_reader() {
    // 64 slots, a producer that never waits, a reader that copies 53 at a time: laps are
    // routine. Every value returned must be whole and exactly where the cursor says it is.
    let report = stress(3_000_000, 64);
    assert_eq!(report.written, 3_000_000);
    assert_eq!(report.corrupt, 0, "a torn value was returned");
    assert_eq!(report.misplaced, 0, "a value came back with the wrong sequence number");
    assert!(report.lost_reads > 0, "the reader really was lapped, or this proves nothing");
    assert_eq!(
        report.received + report.lost,
        report.written,
        "every sequence number accounted for"
    );
    assert!(report.received > 0);
}

// LatestRingTests.aReaderThatKeepsUpLosesNothing
#[test]
fn a_reader_that_keeps_up_loses_nothing() {
    // The same stress with a ring bigger than the whole run (Swift's is 1 << 22, 128 MB of
    // payloads: 1 << 18 holds all 200 000 too), so the reader cannot be a lap behind.
    let report = stress(200_000, 1 << 18);
    assert!(report.corrupt == 0 && report.misplaced == 0);
    assert!(report.lost == 0 && report.received == 200_000);
}
