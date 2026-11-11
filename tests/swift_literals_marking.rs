//! The marking mirror through the real wiring: `WaveProject::marking` and
//! `WaveRunner::publish_marking` (package F of the parity audit). The djengine mirror tests all
//! go through `mirror_support`, which builds its own `MarkingMirror` and publishes it with
//! `m.publish(g, ..)`, so none of them reads the project's own mirror or calls the runner's hook
//! (the real path is held in `djhost`: `deck_golden.rs`, `deck_feeds.rs`, `deck_rt.rs`). These
//! carry the Swift tests that matter for that wiring: the mirror follows the graph block by block
//! (`MarkingMirrorTests`), a swap and a stop (`MarkingMirrorTests`, `MarkingBeforeTheFirstRenderTests`)
//! and the preempted-reader stress with `midCopyHook` set on the project's own mirror
//! (`MarkingMirrorThreadTests.aReaderNeverSeesAMixOfTwoStates(preempted: true)`).

mod mirror_support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use djrust::wave::net::str_hash;
use djrust::wave::*;
use mirror_support::project;

/// `mirror_support::stress::Gate` with a way out: it keeps a writer and its reader in step
/// whatever the scheduler does (the writer is never more than `AHEAD` records ahead of the
/// newest the reader has seen whole), and a reader that gives up lets the writer go, so a
/// broken publish fails the test where the original would wait for ever.
#[derive(Default)]
struct Gate {
    seen: AtomicI64,
    abort: AtomicBool,
}

impl Gate {
    const AHEAD: i64 = 4;

    fn reader_saw(&self, n: i64) {
        self.seen.fetch_max(n, Ordering::Release);
    }

    /// False if the reader gave up.
    fn writer_wait(&self, n: i64) -> bool {
        while n - Self::AHEAD > self.seen.load(Ordering::Acquire) {
            if self.abort.load(Ordering::Acquire) {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }
}

const SR: f64 = 8000.0;
const BLOCK: usize = 128;

fn prepare(genre: &str, seed: f64) -> WaveProject {
    WaveRunner::prepare(&project(genre, seed, "loop"), SR, &WaveOptions::default(), 0)
}

/// A runner playing `wp` at `tempo` (its marking mirror stays with the caller by `Arc`).
fn runner_for(wp: WaveProject, tempo: f64) -> WaveRunner {
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.load_prepared(wp);
    r.set_tempo(tempo);
    r.play();
    r
}

/// One block as `WaveDeckSource.render` ends it: the runner, then its marking published.
fn block(r: &mut WaveRunner, frame: &mut i64) {
    let (mut l, mut rr) = ([0f32; BLOCK], [0f32; BLOCK]);
    r.process_at(&mut l, Some(&mut rr), BLOCK, *frame);
    *frame += BLOCK as i64;
    r.publish_marking(*frame);
}

/// The running graph's marking as `read_into` lays it out: tokens net after net, mutes.
fn state_of(r: &WaveRunner) -> (Vec<f64>, Vec<u8>, i64) {
    let g = r.graph().expect("a project is running");
    let tokens = g
        .nets
        .iter()
        .flat_map(|n| n.state[..n.n_places].iter().copied())
        .collect();
    (tokens, g.muted[..g.nets.len()].to_vec(), g.tick)
}

// MarkingMirrorTests / MarkingLayoutIdentityTests, through `prepare`: the project owns the mirror
// that describes it: its generation, its song's serial, the tick it was seeded at.
#[test]
fn a_prepared_project_owns_the_marking_that_describes_it() {
    let a = prepare("techno", 42.0);
    let b = prepare("house", 7.0);
    for wp in [&a, &b] {
        let l = &wp.marking.layout;
        assert_eq!(l.generation, wp.generation);
        assert_eq!(l.track_serial, wp.track_serial);
        assert_eq!(l.start_tick, wp.start_tick);
        assert_eq!(l.net_count(), wp.graph.nets.len());
    }
    assert!(b.generation > a.generation, "generations increase with each prepare");
    assert_ne!(a.track_serial, b.track_serial, "a new composition is a new song");
    // another start of the same song (a section launch, a relocation) keeps the song's serial
    let again = WaveRunner::prepare_serial(
        &project("techno", 42.0, "loop"),
        SR,
        &WaveOptions::default(),
        16,
        Some(a.track_serial),
    );
    assert_eq!(again.track_serial, a.track_serial);
    assert_eq!(again.marking.layout.track_serial, a.track_serial);
    assert_ne!(again.generation, a.generation);
    assert_eq!(again.marking.layout.start_tick, 16);
}

// MarkingMirrorTests (the mirror follows the graph): after every block the runner publishes, the
// project's own mirror reads back the graph's tokens, mutes and tick; the frame is the end of the
// block that last changed it.
#[test]
fn the_runner_publishes_the_graph_into_the_projects_own_mirror() {
    let wp = prepare("techno", 42.0);
    let mirror = wp.marking.clone();
    let mut r = runner_for(wp, 480.0);
    let mut snap = MarkingSnapshot::new();
    let mut frame = 0i64;
    let (mut changes, mut last_seq) = (0, 0);
    for b in 0..400 {
        if b == 100 {
            r.set_role_muted(str_hash("bass"), true);
        }
        block(&mut r, &mut frame);
        assert!(mirror.read_into(&mut snap));
        let (tokens, muted, tick) = state_of(&r);
        assert_eq!(snap.tokens, tokens, "block {b}");
        assert_eq!(snap.muted, muted, "block {b}");
        assert_eq!(snap.header.tick, tick, "block {b}");
        assert!(snap.header.frame <= frame, "published at the end of a block already rendered");
        if snap.header.sequence != last_seq {
            changes += 1;
            assert!(snap.header.sequence > last_seq);
            last_seq = snap.header.sequence;
        }
    }
    assert!(changes > 100, "the marking moved with the clock: {changes} publishes");
    assert!(snap.muted.contains(&1), "the bass role was muted in it");
}

// MarkingMirrorTests (a swap): a queued project takes over at a bar, and from then on the runner
// publishes into the new project's mirror and leaves the old one where it was.
#[test]
fn a_swap_moves_the_publishing_to_the_new_projects_mirror() {
    let first = prepare("techno", 42.0);
    let second = prepare("house", 7.0);
    let (m1, m2) = (first.marking.clone(), second.marking.clone());
    let mut r = runner_for(first, 480.0);
    let mut frame = 0i64;
    for _ in 0..50 {
        block(&mut r, &mut frame);
    }
    r.load_prepared(second); // queued behind the playing one
    let mut snap = MarkingSnapshot::new();
    let seq_before = {
        assert!(m1.read_into(&mut snap));
        snap.header.sequence
    };
    let mut swapped_at = None;
    for b in 0..600 {
        block(&mut r, &mut frame);
        if swapped_at.is_none() && r.generation() == m2.layout.generation {
            swapped_at = Some(b);
        }
    }
    assert!(swapped_at.is_some(), "the swap landed at a bar");
    assert!(m1.read_into(&mut snap));
    let seq_old = snap.header.sequence;
    assert!(seq_old > seq_before, "the old project kept publishing until its bar");
    for _ in 0..50 {
        block(&mut r, &mut frame);
    }
    assert!(m1.read_into(&mut snap));
    assert_eq!(snap.header.sequence, seq_old, "and then no more");
    assert!(m2.read_into(&mut snap));
    assert_eq!(snap.generation(), m2.layout.generation);
    assert!(snap.header.sequence > 1, "the new one is being published");
    let (tokens, _, tick) = state_of(&r);
    assert_eq!((snap.tokens, snap.header.tick), (tokens, tick));
}

// MarkingMirrorTests (stop): a stop resets the marking in place; the next publish is the seed.
#[test]
fn a_stop_resets_the_marking_to_the_seed() {
    let wp = prepare("techno", 42.0);
    let mirror = wp.marking.clone();
    let mut r = runner_for(wp, 480.0);
    let mut frame = 0i64;
    for _ in 0..80 {
        block(&mut r, &mut frame);
    }
    let mut snap = MarkingSnapshot::new();
    assert!(mirror.read_into(&mut snap));
    let moved = snap.header.tick;
    assert!(moved > 0, "it ran: tick {moved}");
    r.stop();
    block(&mut r, &mut frame);
    assert!(mirror.read_into(&mut snap));
    assert_eq!(snap.header.tick, 0, "back to the seed");
    let (tokens, muted, _) = state_of(&r);
    assert_eq!((snap.tokens, snap.muted), (tokens, muted));
}

/// A checksum of one marking: its tokens (bits), its mutes.
fn checksum(tokens: &[f64], muted: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut eat = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(0x100000001b3);
    };
    for t in tokens {
        eat(t.to_bits());
    }
    for m in muted {
        eat(*m as u64 + 1);
    }
    h
}

// MarkingMirrorThreadTests.aReaderNeverSeesAMixOfTwoStates(preempted: true), on the project's own
// mirror (`midCopyHook` set through `Arc::get_mut` before the project is shared) and the real
// runner as the writer: a reader that is held up halfway through each third copy, while the
// runner laps the buffer, never gets a mix of two states. Each state the runner publishes is
// checksummed first, by the frame it ends at; a snapshot must carry the checksum of its own frame.
#[test]
fn a_preempted_reader_never_sees_a_mix_of_two_states_from_the_runner() {
    let mut wp = prepare("techno", 42.0);
    let calls = AtomicUsize::new(0);
    Arc::get_mut(&mut wp.marking)
        .expect("the project's mirror is not shared yet")
        .set_mid_copy_hook(move || {
            if (calls.fetch_add(1, Ordering::Relaxed) + 1) % 3 == 0 {
                // a spin holds the reader for 200 us, as Swift's sleep does, without a macOS timer's
                // millisecond slop
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_micros(200) {
                    std::hint::spin_loop();
                }
            }
        });
    let mirror = wp.marking.clone();
    let sums: Arc<Mutex<HashMap<i64, u64>>> = Arc::default();
    let done = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gate::default());
    const BLOCKS: i64 = 3000;
    let writer = {
        let (sums, done, gate) = (sums.clone(), done.clone(), gate.clone());
        std::thread::spawn(move || {
            let mut r = runner_for(wp, 3000.0);
            let mut frame = 0i64;
            let (mut l, mut rr) = ([0f32; BLOCK], [0f32; BLOCK]);
            for i in 1..=BLOCKS {
                if !gate.writer_wait(i) {
                    break; // the reader gave up
                }
                r.process_at(&mut l, Some(&mut rr), BLOCK, frame);
                frame += BLOCK as i64;
                let (tokens, muted, _) = state_of(&r);
                // the checksum is in the map before the state can be read
                sums.lock().unwrap().insert(frame, checksum(&tokens, &muted));
                r.publish_marking(frame);
            }
            done.store(true, Ordering::Release);
            r
        })
    };
    let mut snap = MarkingSnapshot::new();
    let (mut reads, mut torn, mut backwards, mut changes) = (0, 0, 0, 0);
    let (mut last_seq, mut last_frame) = (0i64, -1i64);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let finished = done.load(Ordering::Acquire);
        if Instant::now() > deadline {
            gate.abort.store(true, Ordering::Release);
            panic!("no end in 30 s: {reads} reads, last sequence {last_seq}");
        }
        if !mirror.read_into(&mut snap) {
            if finished {
                break;
            }
            continue;
        }
        reads += 1;
        let h = snap.header;
        gate.reader_saw(h.frame / BLOCK as i64); // the writer's own count of the record
        if h.sequence < last_seq {
            backwards += 1;
        }
        last_seq = h.sequence;
        if h.frame != last_frame {
            changes += 1;
        }
        last_frame = h.frame;
        if h.tick > 0 {
            match sums.lock().unwrap().get(&h.frame) {
                Some(&want) if want == checksum(&snap.tokens, &snap.muted) => {}
                _ => torn += 1,
            }
        }
        if finished {
            break; // this read came after the writer was done: it is the last state
        }
    }
    drop(writer.join().unwrap());
    assert_eq!(torn, 0, "{torn} snapshots mixed two states");
    assert_eq!(backwards, 0);
    assert!(reads > 10 && changes > 5, "the reader really saw the runner move: {reads} reads, {changes} changes");
    assert!(mirror.retries() > 0, "the runner really did lap the reader's copy: {} retries", mirror.retries());
}
