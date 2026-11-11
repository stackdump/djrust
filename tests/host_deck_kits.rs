//! A deck whose drum lanes play a sampled kit (feature `sf2`): the track's `Track.kit` is
//! resolved against the engine's library, the bank numbers the kit's slot, the runner reports
//! each hit as the kit's GM key and the sampler renders it inside the deck's block; a closed hat
//! ends the open hat's note on the sampler (`.choke`), the Brush kit plays jazz's snare, and the
//! render thread allocates nothing. The bank is synthesised (a looped sine on every key of a
//! "percussion" preset), as in `deck_sf2.rs`.
#![cfg(feature = "sf2")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
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
const KIT: &str = "MuseScore_General.sf2#128:0";

fn bank_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("djhost-deck-kits-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let mut presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    presets.push(("Standard", 128, 0));
    presets.push(("Brush", 128, 40));
    std::fs::write(d.join("MuseScore_General.sf2"), synthetic_sf2(&presets)).unwrap();
    d
}

fn with_bank(dir: &std::path::Path) -> (Engine, Renderer) {
    let lib = Arc::new(Library::scan(dir));
    let mut c = EngineConfig::new(SR);
    c.library = Some(lib);
    engine(c)
}

/// SampledKitSupport.kitPhraseProject, with hats and open hats between the beats.
fn kit_project(kit: Option<&str>) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let net = |notes: &[(i64, i64)], channel: i64| {
        let mut n = NetBundle::ring(16);
        for (s, note) in notes {
            n.bind(*s, *note, channel, 100, 50);
        }
        n.track.channel = channel;
        n.track.kit = kit.map(String::from);
        n
    };
    p.nets
        .insert("kick", net(&[(0, 36), (4, 36), (8, 36), (12, 36)], 10));
    p.nets.insert("snare", net(&[(4, 38), (12, 38)], 11));
    let mut hats = vec![];
    for s in 0..16 {
        hats.push((s, if s % 4 == 2 { 46 } else { 42 }));
    }
    p.nets.insert("hihat", net(&hats, 12));
    p
}

fn play(e: &mut Engine, p: &Project, tweak: impl FnOnce(&mut djrust::wave::WaveOptions)) -> bool {
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    let Some(prepared) = e.prepare_with("A", p, 0, 1.0, tweak) else {
        return false;
    };
    let ok = e.load_prepared("A", prepared);
    e.set_tempo("A", 480.0);
    e.play("A");
    ok
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

fn loaded(e: &Engine) -> u32 {
    e.deck("A")
        .unwrap()
        .mirror
        .samplers_loaded
        .load(Ordering::Relaxed)
}

/// A track that picked a kit plays its drums on the sampler: the kit's slot is loaded, the render
/// differs from the synth kit's, and a track without the pick loads nothing.
#[test]
fn a_picked_kit_plays_on_the_sampler() {
    let dir = bank_dir("play");
    let (mut e, mut r) = with_bank(&dir);
    assert!(play(&mut e, &kit_project(Some(KIT)), |_| {}));
    let kit = render(&mut r, 150);
    assert!(loaded(&e) > 0, "the kit's slot is loaded");
    let rms =
        |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    assert!(rms(&kit) > 0.001, "{}", rms(&kit));

    let (mut e2, mut r2) = with_bank(&dir);
    assert!(play(&mut e2, &kit_project(None), |_| {}));
    let synth = render(&mut r2, 150);
    assert_eq!(loaded(&e2), 0, "no pick, no kit");
    assert!(kit != synth, "the sampled drums sound different");

    // deterministic: the same bank and project, the same bytes
    let (mut e3, mut r3) = with_bank(&dir);
    assert!(play(&mut e3, &kit_project(Some(KIT)), |_| {}));
    assert!(render(&mut r3, 150) == kit);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A kit no installed bank names leaves beats' synth kit.
#[test]
fn a_kit_the_bank_does_not_have_leaves_the_synth_kit() {
    let dir = bank_dir("missing");
    let (mut e, mut r) = with_bank(&dir);
    assert!(play(
        &mut e,
        &kit_project(Some("Nowhere.sf2#128:0")),
        |_| {}
    ));
    let missing = render(&mut r, 100);
    assert_eq!(loaded(&e), 0);
    let (mut e2, mut r2) = with_bank(&dir);
    assert!(play(&mut e2, &kit_project(None), |_| {}));
    assert!(render(&mut r2, 100) == missing);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Jazz's snare on the bank's Brush kit (a second slot), and the choke between the kit's hats,
/// render without an allocation or a free on the render thread.
#[test]
fn a_sampled_kit_with_brushes_and_chokes_allocates_nothing() {
    let dir = bank_dir("rt");
    let (mut e, mut r) = with_bank(&dir);
    assert!(play(&mut e, &kit_project(Some(KIT)), |o| {
        o.musicality |= Fixes::OPEN_HATS | Fixes::CHOKE;
        o.brushes = true;
    }));
    let mut warm = render(&mut r, 100);
    assert!(loaded(&e) >= 2, "the kit and the Brush kit: {}", loaded(&e));
    warm.clear();
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (_, a, f) = count(|| {
        for _ in 0..1500 {
            r.render_block(&mut l, &mut rr);
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((a, f), (0, 0), "{a} allocations, {f} frees in 1500 blocks");
}
