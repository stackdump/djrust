//! The deck's bank over a whole engine (feature `sf2`): a long set keeps getting sampled sounds
//! after 24 distinct sources (Wander changes instruments all the time), the projects' references
//! come back when the render thread hands them over and the main thread frees them, and an
//! eviction on a loader thread while the deck renders costs the render thread nothing
//! (`WaveDeckSourceTests.swift` 185-235, the eviction half of `sampledInstrumentsDoNotAllocate`).
//! The sounds are a synthesised SoundFont with the 128 General MIDI programs, so no bank is
//! needed on the box.
#![cfg(feature = "sf2")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use djrust::voices::SamplerSource;
use djrust::TrackSpec;
use djrust::host::deck::samplers::{SamplerBank, BANK_SLOTS};
use djrust::host::deck::{engine, Command, Engine, EngineConfig, Renderer};
use djrust::host::sampler::{synthetic_sf2, Library};

struct Counting;
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.with(|o| o.get()) {
            FREES.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static A: Counting = Counting;

/// (allocations, frees) made by `f` on this thread.
fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    (r, ALLOCS.with(|c| c.get()), FREES.with(|c| c.get()))
}

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

struct Dir {
    dir: std::path::PathBuf,
    font: String,
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A SoundFont folder with the floor bank, `MuseScore_General.sf2`: 128 programs, all a sine.
fn bank_dir(tag: &str) -> Dir {
    let dir = std::env::temp_dir().join(format!(
        "djhost-deck-sampler-bank-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    let font = dir.join("MuseScore_General.sf2");
    std::fs::write(&font, synthetic_sf2(&presets)).unwrap();
    Dir {
        dir,
        font: font.to_string_lossy().into_owned(),
    }
}

fn gm(d: &Dir, p: usize) -> SamplerSource {
    SamplerSource::Sf2(d.font.clone(), p as i64)
}

/// A deck-A engine over the folder's library; `extended`: the instrument table and the deck's whole
/// bank (`WaveOptions::bank`), else binding by name alone (`SampleBinder`).
fn with_bank(d: &Dir, extended: bool) -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.library = Some(Arc::new(Library::scan(&d.dir)));
    c.extended = extended;
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    (e, r)
}

/// Compose and load `spec` on deck A and start it, fast (a load takes the song's own tempo).
fn start(e: &mut Engine, spec: &TrackSpec) {
    assert!(e.generate("A", spec));
    e.set_tempo("A", 240.0);
    e.play("A");
}

fn bank_of(e: &Engine) -> Arc<Mutex<SamplerBank>> {
    e.preparer("A").unwrap().sampler_bank().unwrap()
}

fn render(r: &mut Renderer, blocks: usize) -> Vec<f32> {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::new();
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        out.extend_from_slice(&l);
    }
    out
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// References held on every slot of the bank, in all.
fn total(bank: &Mutex<SamplerBank>) -> u32 {
    let b = bank.lock().unwrap();
    (0..BANK_SLOTS).map(|s| b.references_of(s)).sum()
}

/// A bank that has numbered 24 sources nobody plays any more still gives the next track its
/// sampled sounds: the least recently used slots are reused, as Swift's bank does. Without that
/// (the first 24 sources held forever) every later song stays on its synth voices.
#[test]
fn a_bank_that_has_filled_still_gives_the_next_track_its_sampled_sounds() {
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    for extended in [false, true] {
        let d = bank_dir("filled");
        // the same song on a fresh bank, and on synth voices alone
        let (mut e, mut r) = with_bank(&d, extended);
        start(&mut e, &spec);
        let fresh = render(&mut r, 200);
        let mut c = EngineConfig::new(SR);
        c.extended = extended;
        let (mut bare, mut rb) = engine(c);
        bare.set_crossfader(0.0);
        bare.set_volume("A", 1.0);
        start(&mut bare, &spec);
        let synth = render(&mut rb, 200);
        assert!(
            fresh != synth,
            "extended {extended}: the sampled lanes sound different"
        );

        // a deck whose bank has filled with other programs, each given back: nobody plays them
        let (mut e2, mut r2) = with_bank(&d, extended);
        {
            let bank = bank_of(&e2);
            let mut b = bank.lock().unwrap();
            for p in 100..100 + BANK_SLOTS {
                let s = b.slot_for(&[gm(&d, p)]).expect("the bank has room");
                b.release(s);
            }
            assert_eq!(b.loaded(), BANK_SLOTS);
        }
        start(&mut e2, &spec);
        let full = render(&mut r2, 200);
        assert!(
            full != synth,
            "extended {extended}: the track still plays sampled sounds on a bank that has filled"
        );
        let (a, b) = (rms(&full), rms(&fresh));
        assert!(
            (a - b).abs() < 0.02 * b,
            "extended {extended}: as loud as on a fresh bank ({a} against {b})"
        );
    }
}

/// A project's references come back when the render thread hands it over and the main thread
/// frees it, and when a track is dropped unused: both ways a deck binds lanes (`WaveOptions::bank`
/// and the by-name `SampleBinder`, whose project is given the bank's handle by the preparer).
#[test]
fn a_deck_gives_its_references_back_as_projects_are_freed() {
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    for extended in [false, true] {
        let d = bank_dir("refs");
        let (mut e, mut r) = with_bank(&d, extended);
        let bank = bank_of(&e);
        assert_eq!(total(&bank), 0);
        assert!(e.generate("A", &spec));
        let one = total(&bank);
        assert!(one > 0, "extended {extended}: the track holds its slots");
        // the same song again: its project finds the slots loaded and takes its own references
        // while the first still plays
        assert!(e.generate("A", &spec));
        assert_eq!(total(&bank), 2 * one, "extended {extended}");
        // the render thread lets the first go (the retire ring), the main thread frees it
        render(&mut r, 4);
        e.drain_retired();
        assert_eq!(
            total(&bank),
            one,
            "extended {extended}: the retired project gave its slots back"
        );
        // a track composed ahead and cancelled is dropped unused, and gives them back too
        e.queue_next("A", &spec);
        assert!(e.next_ready("A").is_some());
        assert_eq!(total(&bank), 2 * one, "extended {extended}");
        e.cancel_next("A");
        assert_eq!(
            total(&bank),
            one,
            "extended {extended}: the dropped track gave its slots back"
        );
    }
}

/// `WaveDeckSourceTests` 185-235: a full bank of unreferenced programs (each renders every block),
/// and one more loaded on another thread while the deck renders. The sampler it evicts is replaced
/// on the audio thread by a command and the old one is freed on the main thread: the render
/// thread allocates nothing and frees nothing, the slot is never one the playing project
/// references, and the track goes on sounding.
#[test]
fn an_eviction_on_a_loader_thread_costs_the_render_thread_nothing() {
    let d = bank_dir("evict-rt");
    let (mut e, mut r) = with_bank(&d, true);
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    start(&mut e, &spec);
    render(&mut r, 100);
    let bank = bank_of(&e);
    let used: BTreeSet<usize> = {
        let b = bank.lock().unwrap();
        (0..BANK_SLOTS)
            .filter(|s| b.references_of(*s) > 0)
            .collect()
    };
    assert!(!used.is_empty(), "a sampled lane to hold");
    // fill the bank with unreferenced programs and put their samplers on the audio thread
    let (ready, fresh_program) = {
        let mut b = bank.lock().unwrap();
        let mut p = 0;
        while b.loaded() < BANK_SLOTS && p < 128 {
            if let Some(s) = b.slot_for(&[gm(&d, p)]) {
                b.release(s);
            }
            p += 1;
        }
        assert_eq!(b.loaded(), BANK_SLOTS, "a full bank");
        // a pitched program that no slot plays
        let fresh = (0..128)
            .rev()
            .find(|p| (0..BANK_SLOTS).all(|s| b.source_of(s) != Some(&gm(&d, *p))))
            .unwrap();
        (b.take_ready(), fresh)
    };
    for (slot, sampler) in ready {
        assert!(e.command("A", Command::Sampler(slot, sampler)));
    }
    render(&mut r, 20);
    e.drain_retired();
    let retired_before = e.retired;

    // the loader: one more program, evicting the least recently used unreferenced slot, while
    // this thread renders
    let theirs = bank.clone();
    let font = d.font.clone();
    let loader = std::thread::spawn(move || {
        let mut b = theirs.lock().unwrap();
        let slot = b.slot_for(&[SamplerSource::Sf2(font, fresh_program as i64)]);
        (slot, b.take_ready())
    });
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let (_, a1, f1) = count(|| {
        while !loader.is_finished() && std::time::Instant::now() < deadline {
            r.render_block(&mut l, &mut rr);
        }
        for _ in 0..50 {
            r.render_block(&mut l, &mut rr);
        }
    });
    assert!(loader.is_finished(), "the load missed the deadline");
    let (slot, mut ready) = loader.join().unwrap();
    let slot = slot.expect("the load evicted a slot");
    assert!(
        !used.contains(&slot),
        "never one the playing project references ({slot})"
    );
    {
        let b = bank.lock().unwrap();
        assert_eq!(b.source_of(slot), Some(&gm(&d, fresh_program)));
    }
    assert_eq!(ready.len(), 1, "the one sampler loaded for the slot");
    let (sent_to, sampler) = ready.pop().unwrap();
    assert_eq!(sent_to, slot);
    // the sampler goes to the audio thread; the one it replaces comes back to the main thread
    assert!(e.command("A", Command::Sampler(slot, sampler)));
    let (out, a2, f2) = count(|| {
        let mut peak = 0f32;
        for _ in 0..100 {
            r.render_block(&mut l, &mut rr);
            peak = l.iter().chain(rr.iter()).fold(peak, |m, v| m.max(v.abs()));
        }
        peak
    });
    assert_eq!((a1, f1), (0, 0), "rendering while the loader evicted");
    assert_eq!((a2, f2), (0, 0), "rendering through the replacement");
    assert!(out > 0.005, "the track goes on sounding ({out})");
    e.drain_retired();
    assert!(
        e.retired > retired_before,
        "the replaced sampler was freed by the main thread ({} -> {})",
        retired_before,
        e.retired
    );
    bank.lock().unwrap().release(slot);
}

/// The slots a project holds, each with what the audio thread plays there against what the bank
/// says the slot is for. Returns how many slots that is.
fn slots_agree(r: &Renderer, bank: &Mutex<SamplerBank>, what: &str) -> usize {
    let b = bank.lock().unwrap();
    let core = r.deck("A").unwrap();
    let mut held = 0;
    for slot in (0..BANK_SLOTS).filter(|s| b.references_of(*s) > 0) {
        held += 1;
        assert_eq!(
            core.samplers.source_of(slot),
            b.source_of(slot),
            "{what}: the audio thread plays what the bank says slot {slot} is"
        );
    }
    held
}

/// A loaded sampler goes to the audio thread with the first track handed over that plays its slot,
/// not with the first track that was prepared. A track composed ahead (`queue_next`, a Wander
/// swap, a restart) finds the sources it shares with a track prepared a moment later numbered
/// already; whichever of them is handed over first has to bring the samplers, or its lanes play
/// what the slot held before: nothing on a bank that is filling, another instrument's sound once
/// it has filled and the slot has been given to a new source. Swift has no hand-off (the bank owns
/// the instrument), so this is where the port can differ.
#[test]
fn the_first_track_handed_over_brings_the_samplers_of_every_track_prepared_before_it() {
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    for extended in [false, true] {
        for full in [false, true] {
            let what = format!("extended {extended}, a bank that has filled {full}");
            let d = bank_dir("handoff");
            let (mut e, mut r) = with_bank(&d, extended);
            let bank = bank_of(&e);
            if full {
                // 24 sources nobody plays, their samplers on the audio thread: every slot the next
                // tracks bind is given a new source and holds the old one's sound until its own
                // sampler arrives
                let ready = {
                    let mut b = bank.lock().unwrap();
                    for p in 100..100 + BANK_SLOTS {
                        let s = b.slot_for(&[gm(&d, p)]).expect("the bank has room");
                        b.release(s);
                    }
                    b.take_ready()
                };
                assert_eq!(ready.len(), BANK_SLOTS);
                for (slot, sampler) in ready {
                    assert!(e.command("A", Command::Sampler(slot, sampler)));
                }
                r.drain_commands();
            }
            // composed ahead: binds the song's slots and loads what they play
            e.queue_next("A", &spec);
            assert!(e.next_ready("A").is_some(), "{what}");
            // the same song, prepared after it and handed over first: its sources are numbered
            assert!(e.generate("A", &spec));
            r.drain_commands();
            let held = slots_agree(&r, &bank, &what);
            assert!(held > 0, "{what}: the track holds sampled slots");
            // and the track that was composed ahead, when it is the one that goes in
            if e.next_ready("A").is_some() {
                assert!(e.advance_to_next("A"), "{what}");
                r.drain_commands();
                slots_agree(&r, &bank, &what);
            }
            // what the audio thread plays is the sampled sound, not the synth voice or silence
            e.set_tempo("A", 240.0);
            e.play("A");
            let heard = render(&mut r, 200);
            assert!(rms(&heard) > 0.001, "{what}: the track sounds");
        }
    }
}
