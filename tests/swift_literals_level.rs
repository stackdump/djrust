//! `SampleAutoGainTests.aTenMinuteSampleMeasuresInUnderHalfASecond`, which had no Rust
//! counterpart (package F of the parity audit): a whole long track measures (BS.1770 integrated,
//! peak, band shares) in under half a second, so a load measures it there and then, and a second
//! read is the cached level. Swift runs it in optimized builds only (`.enabled(if: optimized)`);
//! the same here: the test profile is optimized (opt-level 2) but keeps debug assertions, which
//! slow the measure, so it runs in a build without them
//! (`CARGO_PROFILE_TEST_DEBUG_ASSERTIONS=false cargo test -p djhost --test swift_literals_level`,
//! or `--release`) and is ignored otherwise.

use std::sync::Arc;
use std::time::Instant;

use djrust::deck::turntable::SampleBuffer;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, EngineConfig};

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing is only meaningful without debug assertions (see the module doc)"
)]
fn a_ten_minute_sample_measures_in_under_half_a_second() {
    let sr = 48_000.0;
    let n = (600.0 * sr) as usize;
    // the Swift LCG: s = s * 1664525 + 1013904223; l = Int32(bitPattern: s) / Int32.max * 0.2
    let mut s: u32 = 1;
    let mut l = vec![0f32; n];
    let mut r = vec![0f32; n];
    for i in 0..n {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        l[i] = (s as i32 as f32) / (i32::MAX as f32) * 0.2;
        r[i] = l[i] * 0.9;
    }
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, _r) = engine(c);
    // (the grid is given: finding one in ten minutes of noise is not what is timed)
    let buffer = Arc::new(SampleBuffer::new(l, Some(r), sr));
    assert!(e.load_sample("A", PreparedSample::with_grid(buffer, "ten minutes", None)));
    let t = Instant::now();
    let level = e.sample_level("A").expect("a sample is loaded");
    let took = t.elapsed();
    eprintln!("ten minutes measured in {took:?}");
    assert!(took.as_millis() < 500, "{took:?}");
    assert!(level.lufs.is_some());
    // and cached: a second read is free
    let t = Instant::now();
    let again = e.sample_level("A").unwrap();
    assert!(t.elapsed().as_millis() < 5, "{:?}", t.elapsed());
    assert_eq!(again, level);
}
