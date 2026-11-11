//! Two-thread stress for the marking mirror (`MarkingStress.swift`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

/// Keeps a stress writer and its reader in step whatever the scheduler does. Left alone, a
/// writer that gets a time slice while the reader is descheduled (a loaded machine) publishes
/// the whole run before the reader looks, and the reader sees two states. The writer may not be
/// more than `AHEAD` records ahead of the newest record the reader has seen whole, so the two
/// always overlap (the writer is only ever held back between two records, never inside one),
/// and nothing else about the protocol is touched. (The Swift tests rely on the OS to
/// interleave them.)
#[derive(Default)]
pub struct Gate(AtomicI64);

impl Gate {
    /// How many records the writer may be ahead of what the reader has seen.
    pub const AHEAD: i64 = 4;

    /// The reader got record number `n` whole.
    pub fn reader_saw(&self, n: i64) {
        self.0.fetch_max(n, Ordering::Release);
    }

    /// The writer is about to publish record number `n`: wait until the reader is close enough.
    pub fn writer_wait(&self, n: i64) {
        while n - Self::AHEAD > self.0.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }
}

#[derive(Default, Debug)]
pub struct MirrorStressReport {
    pub reads: usize,
    pub failed_reads: usize,
    /// A snapshot whose tokens/mutes/frame disagree with its own tick.
    pub torn: usize,
    /// A later read that returned an older sequence.
    pub backwards: usize,
    pub published: usize,
    pub changes_seen: usize,
    pub retried: i64,
}

/// A writer thread publishes `publishes` states in which every token equals the tick, every
/// mute flag equals the tick's parity and the frame equals the tick; a reader on this thread
/// checks every snapshot it can get for exactly that. Any mix of two states fails the check.
/// `preempt_reader`: the reader is held up halfway through each copy, so the writer laps it.
pub fn stress_mirror(publishes: i64, preempt_reader: bool) -> MirrorStressReport {
    let p = project("techno", 42.0, "loop");
    let wp = WaveRunner::prepare(&p, 8000.0, &WaveOptions::default(), 0);
    let mut mirror = MarkingMirror::new(wp.generation, next_track_serial(), 0, &wp.graph, &p);
    // A reader that is descheduled halfway through its copy while the writer laps the buffer.
    if preempt_reader {
        let calls = AtomicUsize::new(0);
        mirror.set_mid_copy_hook(move || {
            if (calls.fetch_add(1, Ordering::Relaxed) + 1) % 3 == 0 {
                // Swift sleeps 200 us here. A sleep on macOS lasts several milliseconds (timer
                // coalescing), which would make this one test take ten seconds; a spin holds the
                // reader for the same 200 us and keeps it mid-copy just the same.
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_micros(200) {
                    std::hint::spin_loop();
                }
            }
        });
    }
    let mirror = Arc::new(mirror);
    let done = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gate::default());
    let writer = {
        let (mirror, done, gate) = (mirror.clone(), done.clone(), gate.clone());
        std::thread::spawn(move || {
            let mut g = wp.graph;
            let mut count = 0;
            for i in 1..=publishes {
                gate.writer_wait(i);
                for n in g.nets.iter_mut() {
                    for p in 0..n.n_places {
                        n.state[p] = i as f64;
                    }
                }
                for k in 0..g.muted.len() {
                    g.muted[k] = (i & 1) as u8;
                }
                g.tick = i;
                if mirror.publish(&g, i) {
                    count += 1;
                }
            }
            done.store(true, Ordering::Release);
            count
        })
    };
    let mut report = MirrorStressReport::default();
    let mut snap = MarkingSnapshot::new();
    let (mut last_seq, mut last_tick) = (0i64, -1i64);
    loop {
        let finished = done.load(Ordering::Acquire);
        if !mirror.read_into(&mut snap) {
            report.failed_reads += 1;
            if finished {
                break;
            }
            continue;
        }
        report.reads += 1;
        let h = snap.header;
        gate.reader_saw(h.tick);
        if h.sequence < last_seq {
            report.backwards += 1;
        }
        last_seq = h.sequence;
        if h.tick != last_tick {
            report.changes_seen += 1;
        }
        last_tick = h.tick;
        if h.tick > 0 {
            // tick 0 is the seed: the project's real initial marking, not uniform
            let v = h.tick as f64;
            if h.frame != h.tick
                || !snap.tokens.iter().all(|&t| t == v)
                || !snap.muted.iter().all(|&m| m == (h.tick & 1) as u8)
            {
                report.torn += 1;
            }
        }
        if finished && h.tick == publishes {
            break;
        }
    }
    report.published = writer.join().unwrap();
    report.retried = mirror.retries();
    report
}

#[derive(Default, Debug)]
pub struct EngineMirrorReport {
    pub reads: usize,
    /// Distinct generations observed.
    pub swaps_seen: usize,
    /// Snapshot arrays not the size of its own layout, or generation != layout's.
    pub mismatched: usize,
    /// Sequence or tick went back within one generation.
    pub backwards: usize,
    pub unknown_layouts: usize,
    pub frames_rendered: i64,
}

/// The audio thread is a real thread running `render` in a loop on a net deck; this thread
/// loads one prepared project after another (so the audio thread swaps at bar boundaries) and
/// reads the mirror as fast as it can. Every snapshot must be sized like its own layout.
pub fn stress_engine_mirror(projects: Vec<Prepared>, rate: f64) -> EngineMirrorReport {
    let total = projects.len();
    let generations: Vec<i32> = projects.iter().map(|p| p.generation).collect();
    let mut queue: VecDeque<Prepared> = projects.into();
    let mut src = Source::new(rate);
    src.load(queue.pop_front().unwrap());
    src.set_tempo(3000.0);
    src.play();
    let (mut audio, mut main) = src.split();
    let stop = Arc::new(AtomicBool::new(false));
    // frames rendered so far, for the main loop to wait on: a loaded machine can start the
    // render thread later than the reads finish, and then nothing would have been measured
    let progress = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let renderer = {
        let stop = stop.clone();
        let progress = progress.clone();
        std::thread::spawn(move || {
            let mut f = 0i64;
            while !stop.load(Ordering::Acquire) {
                audio.render(128, f);
                f += 128;
                progress.store(f, Ordering::Release);
                audio.drain_retired();
            }
            f
        })
    };
    let started = Instant::now() + Duration::from_secs(120);
    while progress.load(Ordering::Acquire) == 0 && Instant::now() < started {
        std::thread::yield_now();
    }
    let mut report = EngineMirrorReport::default();
    let mut snap = MarkingSnapshot::new();
    let mut last_seq: BTreeMap<i32, i64> = BTreeMap::new();
    let mut last_tick: BTreeMap<i32, i64> = BTreeMap::new();
    let mut seen: BTreeSet<i32> = BTreeSet::new();
    let mut next = 1; // the next project to hand over
    let mut after_last = 0;
    let mut on_current = 0; // reads since the current project was first seen
    let deadline = Instant::now() + Duration::from_secs(120);
    // Paced by progress, not by the clock: the next project is handed over only once a snapshot
    // of the previous one has been read, so a loaded machine only makes this slower, and every
    // project is guaranteed to be observed while it runs.
    while after_last < 300 && Instant::now() < deadline {
        if seen.contains(&generations[next - 1]) {
            on_current += 1;
        }
        if next < total && on_current >= 200 {
            main.load(queue.pop_front().unwrap());
            next += 1;
            on_current = 0;
        }
        if next == total && on_current >= 200 {
            after_last += 1;
        }
        if !main.read_marking(&mut snap) {
            continue;
        }
        let Some(layout) = snap.layout.clone() else {
            continue;
        };
        report.reads += 1;
        let h = snap.header;
        if h.generation != layout.generation
            || snap.tokens.len() != layout.place_count()
            || snap.muted.len() != layout.net_count()
        {
            report.mismatched += 1;
        }
        if main.marking_layout_of(h.generation).is_none() {
            report.unknown_layouts += 1;
        }
        if last_seq.get(&h.generation).is_some_and(|&s| h.sequence < s) {
            report.backwards += 1;
        }
        if last_tick.get(&h.generation).is_some_and(|&t| h.tick < t) {
            report.backwards += 1;
        }
        last_seq.insert(h.generation, h.sequence);
        last_tick.insert(h.generation, h.tick);
        seen.insert(h.generation);
    }
    stop.store(true, Ordering::Release);
    // Join the render thread: it hands `frames` back only as it leaves its loop.
    report.frames_rendered = renderer.join().unwrap();
    report.swaps_seen = seen.len();
    report
}
