//! Input strips offline (docs/HUB.md, "Audio inputs"): the gear's audio from a ring into the mix
//! through the DC blocker, the trim (−12 dB by default) and the fader; clips raise the lamp;
//! mute, underruns and drift skips behave.

use djrust::input::DEFAULT_TRIM_DB;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn rig() -> (Engine, Renderer, rtrb::Producer<f32>) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    c.inputs = vec!["TD-3".into()];
    let (mut e, r) = engine(c);
    let (feed, _) = e.take_input_feed(0).expect("a ring to fill");
    assert!(e.take_input_feed(0).is_none(), "only once");
    e.set_crossfader(0.5);
    (e, r, feed)
}

/// Push `n` frames of a 440 Hz sine at `amp` with a DC `offset`.
fn tone(feed: &mut rtrb::Producer<f32>, start: usize, n: usize, amp: f32, offset: f32) {
    for k in start..start + n {
        let t = k as f32 / SR as f32;
        let _ = feed.push(offset + amp * (2.0 * std::f32::consts::PI * 440.0 * t).sin());
    }
}

/// Render `blocks` blocks, keeping the ring fed; the output's peak over the last half.
fn play(
    e: &mut Engine,
    r: &mut Renderer,
    feed: &mut rtrb::Producer<f32>,
    blocks: usize,
    amp: f32,
    offset: f32,
) -> f32 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut at = 0;
    tone(feed, at, 2048, amp, offset);
    at += 2048;
    let mut peak = 0f32;
    for b in 0..blocks {
        tone(feed, at, BLOCK, amp, offset);
        at += BLOCK;
        r.render_block(&mut l, &mut rr);
        if b > blocks / 2 {
            peak = l.iter().fold(peak, |p, v| p.max(v.abs()));
        }
    }
    let _ = e;
    peak
}

#[test]
fn the_tone_comes_through_at_the_trim_and_fader() {
    let (mut e, mut r, mut feed) = rig();
    assert_eq!(e.inputs[0].settings.trim_db, DEFAULT_TRIM_DB);
    e.set_input_volume(0, 1.0);
    let peak = play(&mut e, &mut r, &mut feed, 400, 0.5, 0.3);
    // 0.5 x 10^(-12/20) = 0.126, the 0.3 DC offset removed
    assert!((peak - 0.1256).abs() < 0.01, "peak {peak}");
    let m = &e.inputs[0].mirror;
    assert!(m.live.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(m.underruns.load(std::sync::atomic::Ordering::Relaxed), 0);
    // no clip at 0.8 raw
    let v = e.watch_inputs(1.0);
    assert!(!v[0].clip && v[0].hint);
}

#[test]
fn a_hot_input_lights_the_clip_lamp_and_mute_silences() {
    let (mut e, mut r, mut feed) = rig();
    play(&mut e, &mut r, &mut feed, 100, 1.2, 0.0); // a line level the mic jack can't take
    let v = e.watch_inputs(1.0);
    assert!(v[0].clip && v[0].hot, "{v:?}");
    assert!(e.watch_inputs(2.0)[0].clip, "held");
    assert!(!e.watch_inputs(3.5)[0].clip, "let go after 2 s");
    e.set_input_mute(0, true);
    assert!(play(&mut e, &mut r, &mut feed, 100, 0.5, 0.0) < 1e-6);
}

#[test]
fn underruns_and_drift_are_counted() {
    let (mut e, mut r, mut feed) = rig();
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    tone(&mut feed, 0, 2048, 0.5, 0.0); // under three cushions: no skip yet
    for _ in 0..40 {
        r.render_block(&mut l, &mut rr); // nothing more pushed: it runs dry
    }
    let m = e.inputs[0].mirror.clone();
    assert!(m.underruns.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    tone(&mut feed, 0, 20_000, 0.5, 0.0); // far ahead: drop back to the cushion
    r.render_block(&mut l, &mut rr);
    assert_eq!(m.skips.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(m.fill.load(std::sync::atomic::Ordering::Relaxed) >= 20_000);
    r.render_block(&mut l, &mut rr);
    assert!(m.fill.load(std::sync::atomic::Ordering::Relaxed) < 2_000);
    let _ = &mut e;
}

/// An output whose callback is longer than the strip's cushion (Android's AAudio may hand
/// 2048+ frames at once): a feeder that keeps the ring a block plus a little ahead plays
/// without a gap, and the strip says how big the block is.
#[test]
fn a_long_output_block_plays_without_dropouts() {
    let (mut e, mut r, mut feed) = rig();
    e.set_input_volume(0, 1.0);
    let n = 2176;
    let (mut l, mut rr) = (vec![0f32; n], vec![0f32; n]);
    let m = e.inputs[0].mirror.clone();
    let mut at = 0;
    let mut quiet = 0;
    for b in 0..60 {
        // the feeder: up to one block (as soon as the strip has said how big) plus 512
        let want = (m.block.load(std::sync::atomic::Ordering::Relaxed) as usize).max(1024) + 512;
        let have = feed.buffer().capacity() - feed.slots();
        if want > have {
            tone(&mut feed, at, want - have, 0.5, 0.0);
            at += want - have;
        }
        r.render_block(&mut l, &mut rr);
        if b > 4 {
            // no silent stretch inside a block (the sine's zero crossings are single samples)
            quiet += l.windows(8).filter(|w| w.iter().all(|v| v.abs() < 1e-6)).count();
        }
    }
    assert_eq!(m.block.load(std::sync::atomic::Ordering::Relaxed) as usize, n);
    assert_eq!(m.skips.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(m.underruns.load(std::sync::atomic::Ordering::Relaxed) <= 1, "only the first block");
    assert_eq!(quiet, 0, "gaps in the sound");
    let _ = &mut e;
}
