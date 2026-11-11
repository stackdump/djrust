//! How long booting an engine takes, and how much it allocates
//! (`cargo run --release --example boot_profile`).
use djrust::host::deck::engine::EngineConfig;
use djrust::host::deck::host::host_engine;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static BYTES: AtomicUsize = AtomicUsize::new(0);
static BIG: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
static TRACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        if l.size() >= 256 << 10 && TRACK.swap(false, Ordering::Relaxed) {
            BIG.lock().unwrap().push(l.size());
            TRACK.store(true, Ordering::Relaxed);
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}
#[global_allocator]
static A: Counting = Counting;

fn main() {
    for i in 0..4 {
        let mut cfg = EngineConfig::new(48_000.0);
        cfg.history_beats = None;
        let b0 = BYTES.load(Ordering::Relaxed);
        TRACK.store(i == 1, Ordering::Relaxed);
        let t = Instant::now();
        let (e, r) = host_engine(cfg, false, true);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        TRACK.store(false, Ordering::Relaxed);
        let mb = (BYTES.load(Ordering::Relaxed) - b0) as f64 / 1048576.0;
        drop((e, r));
        eprintln!("boot {ms:.1} ms, allocated {mb:.1} MB");
    }
    let mut big = BIG.lock().unwrap().clone();
    big.sort_unstable();
    let mut counts: std::collections::BTreeMap<usize, usize> = Default::default();
    for b in big {
        *counts.entry(b).or_default() += 1;
    }
    for (size, n) in counts.iter().rev() {
        eprintln!("  {n} x {} KB", size / 1024);
    }
}
