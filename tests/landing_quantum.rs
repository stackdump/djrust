//! The quantum a track lands in, on the browser's engine (`djrust::wasm::Dj`, driven here as the
//! worklet drives it: `begin_install`, then `install_step` and `render` a quantum at a time, `tick`
//! every fourth): no quantum does much of the allocator's work, the landing included, and freeing
//! what a landing replaced a few pieces at a time plays the same samples as freeing it at once.
//! (`cargo test --release --features wasm --test landing_quantum -- --nocapture` prints the times.)
//!
//! The gate counts allocator calls, not milliseconds: a call count is the same on every machine
//! and under any load, and it is what a landing cost on the audio thread (the frees of the track
//! it replaced: thousands of calls, 20-26 ms in the browser on a desktop core). The times are
//! printed beside it.
#![cfg(feature = "wasm")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use djrust::TrackSpec;
use djrust::host::deck::engine::{Engine, EngineConfig, Renderer};
use djrust::host::deck::host::host_engine;
use djrust::host::deck::perform::PerformCtx;
use djrust::host::deck::track::PreparedTrack;
use djrust::host::deck::transfer;
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::wasm::{Dj, DjComposer};

/// Counts this thread's allocator calls (alloc, dealloc, realloc): each test counts its own.
struct Counting;

thread_local! {
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = CALLS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let _ = CALLS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let _ = CALLS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = CALLS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn calls() -> u64 {
    CALLS.with(|c| c.get())
}

const SR: f64 = 48_000.0;
/// The page's install budget a quantum (engine-worklet.js `DEFAULT_BUDGET`).
const BUDGET: usize = 12 << 10;
/// The most allocator calls any one quantum's `install_step` or `tick` may make: about 0.1-0.2 ms
/// natively, ~1 ms in the browser on a slow core. At 2aeffaf (the landing freeing and copying
/// at once) the quantum a track landed in over another made up to ~153 thousand calls (10 ms
/// native) and the `tick` after it up to ~199 thousand. Now (gir, 2026-11-28) a section's landing
/// makes ~330 (a DJ compose's ~690), a tick ~100 (~500), and a quantum freeing what a landing
/// replaced ~1.4 thousand at most (two pieces of a net's places or transitions).
const MOST_CALLS: u64 = 2_000;

/// The most a quantum taking a genre track's frames in may make. A frame is one compiled net or
/// one lane, taken in whole; the biggest nets of a DJ compose are ~14-21 thousand calls (as at
/// 2aeffaf: this bound only catches a regression). BeatScript's sections (raw-`nets` shares)
/// have small frames and are held to [`MOST_CALLS`].
const MOST_FRAME_CALLS: u64 = 24_000;

/// What one run measured: per kind of quantum, the most allocator calls and the most time.
#[derive(Default, Debug)]
struct Worst {
    landing_calls: u64,
    landing_ms: f64,
    step_calls: u64,
    step_ms: f64,
    tick_calls: u64,
    tick_ms: f64,
    landings: usize,
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, u64, f64) {
    let (c, t) = (calls(), Instant::now());
    let x = f();
    (x, calls() - c, t.elapsed().as_secs_f64() * 1e3)
}

/// Lands each of `tracks` (deck, bytes) on the playing `dj` as the worklet does, playing `quanta`
/// render quanta after each lands, and returns what the worst quanta cost.
fn land_all(dj: &mut Dj, tracks: Vec<(&str, Vec<u8>)>, quanta: usize) -> Worst {
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    let mut w = Worst::default();
    let mut q = 0usize;
    for (deck, bytes) in tracks {
        dj.begin_install(deck, bytes).unwrap();
        let mut after = 0usize;
        let mut landed = false;
        while !landed || after < quanta || dj.installing() {
            if dj.installing() {
                let (out, c, ms) = timed(|| dj.install_step(BUDGET));
                match out {
                    Some(a) if a.contains("\"deck\"") => {
                        assert!(a.contains("Done"), "{a}");
                        landed = true;
                        w.landings += 1;
                        w.landing_calls = w.landing_calls.max(c);
                        w.landing_ms = w.landing_ms.max(ms);
                        dj.set_playing(deck, true).unwrap();
                    }
                    _ => {
                        w.step_calls = w.step_calls.max(c);
                        w.step_ms = w.step_ms.max(ms);
                    }
                }
            }
            dj.render(&mut l, &mut r);
            if q % 4 == 0 {
                let (_, c, ms) = timed(|| dj.tick());
                w.tick_calls = w.tick_calls.max(c);
                w.tick_ms = w.tick_ms.max(ms);
            }
            q += 1;
            if landed {
                after += 1;
            }
            assert!(q < 200_000, "never landed");
        }
    }
    w
}

fn share(g: &str, seed: u32) -> String {
    format!("{{\"genre\":\"{g}\",\"seed\":{seed}}}")
}

/// A BeatScript program's sections: raw-`nets` shares landing one after another on both decks,
/// each over the share already there. No quantum does much: the landing, the steps, the ticks
/// and the quanta freeing what each landing replaced.
#[test]
fn a_section_landing_over_another_is_a_small_quantum() {
    let mut comp = DjComposer::new(SR);
    let mut dj = Dj::new(SR);
    let mut tracks = Vec::new();
    for (n, (g, s)) in [("techno", 42), ("house", 7), ("dnb", 11), ("acid", 3), ("techno", 9), ("house", 8), ("techno", 42), ("house", 7)].into_iter().enumerate() {
        let deck = if n % 2 == 0 { "A" } else { "B" };
        tracks.push((deck, comp.prepare_share(deck, &share(g, s), "[]").unwrap()));
    }
    let w = land_all(&mut dj, tracks, 1500);
    eprintln!("shares: {w:?}");
    assert_eq!(w.landings, 8);
    assert!(w.landing_calls <= MOST_CALLS, "the landing quantum made {} allocator calls ({:.2} ms)", w.landing_calls, w.landing_ms);
    assert!(w.step_calls <= MOST_CALLS, "an install_step quantum made {} allocator calls ({:.2} ms)", w.step_calls, w.step_ms);
    assert!(w.tick_calls <= MOST_CALLS, "a tick made {} allocator calls ({:.2} ms)", w.tick_calls, w.tick_ms);
}

/// DJ composes (genre tracks) and shares landing over each other, either kind over either: the
/// landing and the ticks stay small, and freeing what was replaced is spread (in the steps).
#[test]
fn a_track_landing_over_another_of_either_kind_is_a_small_quantum() {
    let mut comp = DjComposer::new(SR);
    let mut dj = Dj::new(SR);
    let mut tracks = Vec::new();
    for (n, (g, s)) in [("techno", 42), ("house", 7), ("dnb", 11), ("acid", 3), ("techno", 9), ("house", 8)].into_iter().enumerate() {
        let deck = if n % 2 == 0 { "A" } else { "B" };
        let bytes = if n % 3 == 2 {
            comp.prepare_share(deck, &share(g, s), "[]").unwrap()
        } else {
            comp.prepare(deck, g, s as f64, "standard", "[]").unwrap()
        };
        tracks.push((deck, bytes));
    }
    // and the same tracks again over themselves: every landing from here replaces one
    let again = tracks.clone();
    tracks.extend(again);
    let w = land_all(&mut dj, tracks, 1500);
    eprintln!("mixed: {w:?}");
    assert_eq!(w.landings, 12);
    assert!(w.landing_calls <= MOST_CALLS, "the landing quantum made {} allocator calls ({:.2} ms)", w.landing_calls, w.landing_ms);
    assert!(w.tick_calls <= MOST_CALLS, "a tick made {} allocator calls ({:.2} ms)", w.tick_calls, w.tick_ms);
    assert!(w.step_calls <= MOST_FRAME_CALLS, "an install_step quantum made {} allocator calls ({:.2} ms)", w.step_calls, w.step_ms);
}

fn boot() -> (Engine, Renderer) {
    let mut cfg = EngineConfig::new(SR);
    cfg.history_beats = None;
    let (mut e, r) = host_engine(cfg, false, true);
    e.set_crossfader(0.5);
    (e, r)
}

/// Tracks and shares landing over each other on both decks, playing, as 2aeffaf landed them
/// (`new_path` false: `perform(.load)`, what each replaced freed at once) or as now
/// (`perform_load_prepared`, freed a few pieces a quantum): every sample.
fn played(new_path: bool) -> Vec<u32> {
    let (w, _wr) = boot();
    let (mut e, mut r) = boot();
    e.set_free_in_pieces(new_path);
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    let mut out = Vec::new();
    let order = ["techno:42:standard", "raw:house:7", "acid:3:drop", "raw:dnb:11", "raw:techno:9", "house:7:extended", "raw:techno:42", "dnb:11:minimal"];
    for (n, s) in order.iter().enumerate() {
        let deck = if n % 2 == 0 { "A" } else { "B" };
        let p = w.preparer(deck).unwrap();
        let (track, source) = match s.strip_prefix("raw:") {
            Some(rest) => {
                let spec = TrackSpec::parse(rest).unwrap();
                let DeckSource::RawNets(payload) = djrust::host::beats::source_of(&share(&spec.genre, spec.seed as u32)).unwrap() else { unreachable!() };
                let src = djrust::host::deck::track::TrackSource::RawNets(payload.clone());
                (PreparedTrack::from_source(&src, &p, 0).unwrap(), DeckSource::RawNets(payload))
            }
            None => {
                let spec = TrackSpec::parse(s).unwrap();
                (PreparedTrack::compose(&spec, &p, 0), DeckSource::Net(spec))
            }
        };
        let bytes = transfer::encode(track, &p).unwrap();
        let track = transfer::decode(&bytes, &e.preparer(deck).unwrap()).unwrap();
        if new_path {
            e.perform_load_prepared(deck, track);
        } else {
            e.perform(&DeckAction::Load { deck: deck.into(), source }, PerformCtx { prepared: Some(track), ..PerformCtx::default() });
        }
        e.perform(&DeckAction::Play { deck: deck.into(), on: true }, PerformCtx::default());
        for q in 0..1200 {
            if new_path {
                e.free_step(2);
            }
            r.render_block(&mut l, &mut rr);
            out.extend(l.iter().chain(rr.iter()).map(|x| x.to_bits()));
            if q % 4 == 3 {
                e.service();
                e.service_loops();
            }
        }
        // and the deck reads the same off it
        let d = e.deck(deck).unwrap();
        out.extend([d.sections.len() as u32, e.roles(deck).len() as u32, d.seed.to_bits() as u32]);
    }
    if new_path {
        assert!(e.retired > 0, "nothing was let go of");
        while !e.free_step(2) {}
    }
    out
}

#[test]
fn freeing_in_pieces_plays_the_same_samples() {
    let (at_once, in_pieces) = (played(false), played(true));
    let peak = at_once.iter().map(|v| f32::from_bits(*v).abs()).fold(0f32, f32::max);
    assert!(peak > 0.01, "silent");
    assert_eq!(at_once.len(), in_pieces.len());
    let first = at_once.iter().zip(&in_pieces).position(|(a, b)| a != b);
    assert!(first.is_none(), "differs at sample {first:?}");
}

/// A profile, not a gate (`-- --ignored --nocapture`): where each landing's allocator calls go
/// (the header, each step, `finish`, the load, the ticks, each piece freed after it, by type).
#[test]
#[ignore]
fn profile_where_a_landing_spends() {
    let (w, _wr) = boot();
    let (mut e, mut r) = boot();
    e.set_free_in_pieces(true);
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    for (n, s) in ["techno:42:standard", "house:7:extended", "techno:42:standard", "house:7:extended", "raw:techno:42", "raw:house:7", "raw:techno:42", "raw:house:7"].iter().enumerate() {
        let deck = if n % 2 == 0 { "A" } else { "B" };
        let p = w.preparer(deck).unwrap();
        let (track, spec) = if let Some(rest) = s.strip_prefix("raw:") {
            let spec = TrackSpec::parse(rest).unwrap();
            let text = format!("{{\"genre\":\"{}\",\"seed\":{}}}", spec.genre, spec.seed);
            let src = match djrust::host::beats::source_of(&text).unwrap() {
                DeckSource::RawNets(p) => djrust::host::deck::track::TrackSource::RawNets(p),
                _ => unreachable!(),
            };
            (PreparedTrack::from_source(&src, &p, 0).unwrap(), spec)
        } else {
            let spec = TrackSpec::parse(s).unwrap();
            (PreparedTrack::compose(&spec, &p, 0), spec)
        };
        let _ = spec; // (the genre track's spec, or the share's)
        let bytes = transfer::encode(track, &p).unwrap();
        let pr = e.preparer(deck).unwrap();
        let (mut i, c_new, _) = timed(|| transfer::Install::new(bytes.clone(), &pr).unwrap());
        let mut steps = Vec::new();
        loop {
            let (done, c, _) = timed(|| i.step(BUDGET).unwrap());
            steps.push(c);
            if done {
                break;
            }
        }
        let (track, c_fin, _) = timed(|| i.finish(&pr).unwrap());
        let (_, c_load, _) = timed(|| e.perform_load_prepared(deck, track));
        e.perform(&DeckAction::Play { deck: deck.into(), on: true }, PerformCtx::default());
        let mut frees = Vec::new();
        let mut svc = 0;
        let mut by_name: std::collections::BTreeMap<&str, (u64, u64)> = Default::default();
        let mut q = 0;
        while q < 1500 || e.freeing() > 0 {
            q += 1;
            if e.freeing() > 0 {
                let name = e.next_free_name().unwrap_or("?");
                let (_, c, _) = timed(|| e.free_step(1));
                frees.push((c, name));
                let b = by_name.entry(name).or_default();
                b.0 += 1;
                b.1 += c;
            }
            r.render_block(&mut l, &mut rr);
            if q % 4 == 3 {
                let (_, c, _) = timed(|| { e.service(); e.service_loops(); });
                svc = svc.max(c);
            }
        }
        let short = |n: &str| n.rsplit("::").next().unwrap_or(n).to_string();
        let names: Vec<String> = by_name.iter().map(|(n, (k, c))| format!("{} x{k} {c}", short(n))).collect();
        eprintln!("  freed by kind (steps, calls): {}", names.join(", "));
        let mut top_steps: Vec<(u64, usize)> = steps.iter().copied().enumerate().map(|(k, c)| (c, k)).collect();
        top_steps.sort_unstable_by(|a, b| b.cmp(a));
        let mut top_frees = frees.clone();
        top_frees.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!(
            "{s} {} KB: new {c_new} last {} finish {c_fin} load {c_load} service {svc} | steps {} top {:?} | frees {} top (calls, piece) {:?}",
            bytes.len() / 1024,
            steps.last().unwrap(),
            steps.len(),
            &top_steps[..5.min(top_steps.len())],
            frees.len(),
            &top_frees[..6.min(top_frees.len())]
        );
    }
}
