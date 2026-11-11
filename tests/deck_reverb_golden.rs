//! The strip reverb (`DeckReverb`): Swift's `ReverbTests` on this engine, and a measurement of
//! Swift's AUReverb2 (`tools/paritydump/reverb.swift`, section "reverb",
//! `crates/djengine/tests/fixtures/swift/reverb.json`) that the stand-in
//! ([`djrust::fx::StripReverb`]) is held to.
//!
//! Apple's AUReverb2 cannot be matched (PORTING.md H), so what is exact is the *mapping* (every
//! value `DeckReverb.apply` gives the unit, against what the unit reads back) and what is
//! approximate is the room, within the bounds below, each with the figure it measured:
//!
//! - **bypass**: wet below 0.005 leaves the strip as it is (the master is the capture tap's
//!   bytes, as Swift's is), and LOOP record's tap hears the deck dry at every setting;
//! - **position**: the room is after the EQ and the capture tap, before the fader and the cue
//!   send, on a deck and on the loop channel (a closed fader silences the tail; the cue feed
//!   hears it);
//! - **mix**: wet is a linear crossfade (Swift's dry drops by 6.02 dB at 0.5);
//! - **level and decay**, relative to the same engine's own dry burst, for a noise burst through
//!   rooms of 0.7 s and more at `tone` 0.25 and up: the tail's level within 3.8 dB of the unit
//!   (3.1 dB hot at the 2 s room, the offsets flat over size to 0.6 dB) and its decay slope
//!   within 7 %. Over three kinds of material at a 2 s room the stand-in is +3.1 dB (noise),
//!   -0.3 dB (1 kHz) and -2.8 dB (80 Hz) against the unit: the lows come back, as the unit's do,
//!   and a burst with the right channel inverted returns nothing on both, since both sum to
//!   mono at the input;
//! - **short rooms**: size 0.25 (Swift's "Room" preset) within 4 dB over the windows 1 to 4,
//!   size 0 within 10 dB (the FDN's first window falls more slowly than the unit's);
//! - **`tone` below 0.25**: not held. The unit is not monotonic there: at 0.1 it is 5 dB under
//!   its own 0.5 and at 0 it is 15 dB OVER it. The stand-in stays audible (a floor on the loop's
//!   lowpass), 14 dB under the unit at 0 and 7 dB over it at 0.1; pinned below as a difference.
//!
//! The bursts are integer arithmetic (an LCG) or a sine, so both sides play the same floats.

mod common_fixture;

use std::sync::Arc;

use djrust::deck::turntable::SampleBuffer;
use djrust::fx::strip_reverb::{decay_seconds, is_on, reverb2_params};
use djrust::json::JsonValue;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const WINDOW: usize = 2_400;
const WINDOWS: usize = 60;
/// The silent block rendered before the burst starts.
const PRIME: usize = 4_096;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("reverb")
}

fn arr<'a>(v: &'a JsonValue, k: &str) -> &'a [JsonValue] {
    v.get(k)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {k}"))
}

fn num(v: &JsonValue, k: &str) -> f64 {
    v.get(k)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {k}"))
}

fn flag(v: &JsonValue, k: &str) -> bool {
    v.get(k)
        .and_then(JsonValue::as_bool)
        .unwrap_or_else(|| panic!("no bool {k}"))
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

/// `rvBurst(kind)`: 50 ms of material, then silence, 4 s: "noise" (an LCG in +-0.5, left =
/// right), "wide" (the same, right = -left), "low" (80 Hz at 0.5) or "mid" (1 kHz at 0.5).
fn burst(kind: &str) -> Arc<SampleBuffer> {
    let n = (4.0 * SR) as usize;
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut x: u32 = 12345;
    for k in 0..(SR * 0.05) as usize {
        match kind {
            "low" | "mid" => {
                let hz = if kind == "low" { 80.0 } else { 1000.0 };
                l[k] = (0.5 * (2.0 * std::f64::consts::PI * hz * k as f64 / SR).sin()) as f32;
                r[k] = l[k];
            }
            _ => {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                l[k] = (f64::from(x >> 8) / 16_777_216.0 - 0.5) as f32;
                r[k] = if kind == "wide" { -l[k] } else { l[k] };
            }
        }
    }
    Arc::new(SampleBuffer::new(l, Some(r), SR))
}

/// ReverbTests' engine: deck A alone, the strip at unity, the burst playing once.
fn rig(wet: f64, size: f64, tone: f64, kind: &str) -> (Engine, Renderer) {
    let mut cfg = EngineConfig::new(SR);
    cfg.decks = vec!["A".into()];
    let (mut e, mut r) = engine(cfg);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.set_master(1.0);
    assert!(e.load_sample_with("A", PreparedSample::new((*burst(kind)).clone(), "burst"), false));
    e.set_reverb("A", "reverbSize", size);
    e.set_reverb("A", "reverbTone", tone);
    e.set_reverb("A", "reverbWet", wet);
    // Swift's strip is at unity from the first frame; this one ramps its fader up over its first
    // block, so a block of silence goes by (the deck paused) before the burst starts
    let (mut l, mut rr) = (vec![0f32; PRIME], vec![0f32; PRIME]);
    r.render_block(&mut l, &mut rr);
    e.play("A");
    (e, r)
}

struct Render {
    left: Vec<f32>,
    right: Vec<f32>,
    capture_left: Vec<f32>,
}

fn render(wet: f64, size: f64, tone: f64) -> Render {
    render_of(wet, size, tone, "noise")
}

fn render_of(wet: f64, size: f64, tone: f64, kind: &str) -> Render {
    let (e, mut r) = rig(wet, size, tone, kind);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut l, mut rr) = (vec![0f32; WINDOW], vec![0f32; WINDOW]);
    for _ in 0..WINDOWS {
        r.render_block(&mut l, &mut rr);
        left.extend_from_slice(&l);
        right.extend_from_slice(&rr);
    }
    let (mut cl, mut cr) = (Vec::new(), Vec::new());
    // (the tap counts the renderer's frames, which include the priming block)
    e.deck("A")
        .unwrap()
        .mirror
        .capture
        .copy(PRIME as i64, PRIME as i64 + SR as i64, &mut cl, &mut cr);
    Render {
        left,
        right,
        capture_left: cl,
    }
}

fn windows(x: &[f32]) -> Vec<f64> {
    x.chunks(WINDOW).map(rms).collect()
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// `DeckReverb`'s mapping: `decaySeconds` and `isOn` and what the unit reads back for each
/// AUReverb2 parameter it is given (`Float`s).
#[test]
fn the_mapping_is_swifts_over_the_grid() {
    let g = golden();
    let rows = arr(&g, "params");
    assert_eq!(rows.len(), 90);
    for r in rows {
        let (wet, size, tone) = (num(r, "wet"), num(r, "size"), num(r, "tone"));
        let at = format!("wet {wet} size {size} tone {tone}");
        // `pow(32, size)` is Apple's libm in Swift: the last bit may differ
        let want = f64::from_bits(
            u64::from_str_radix(r.get("decaySeconds").and_then(JsonValue::as_str).unwrap(), 16)
                .unwrap(),
        );
        assert!((decay_seconds(size) - want).abs() <= 1e-12 * want, "{at}");
        assert_eq!(is_on(wet), flag(r, "isOn"), "{at}");
        assert_eq!(!is_on(wet), flag(r, "bypass"), "{at}: node.bypass = !isOn");
        let p = reverb2_params(wet, size, tone);
        // the AU stores Float: compare as Float, one ulp of slack for a libm `pow`
        for (name, got) in [
            ("dryWetMix", p.dry_wet_mix),
            ("gain", p.gain),
            ("minDelay", p.min_delay_time),
            ("maxDelay", p.max_delay_time),
            ("decay0", p.decay_time_at_0hz),
            ("decayNyquist", p.decay_time_at_nyquist),
        ] {
            let got = got as f32 as f64;
            let want = num(r, name);
            let ulp = f64::from(f32::EPSILON) * want.abs().max(1e-30);
            assert!((got - want).abs() <= ulp, "{at}: {name}: got {got}, Swift {want}");
        }
    }
}

/// `offIsBypassedAndBitTransparent`: wet 0 leaves the strip untouched, so the master is the
/// capture tap's bytes (the deck's EQ output), and the render equals Swift's own.
#[test]
fn off_is_bypassed_and_bit_transparent() {
    let g = golden();
    let want = arr(&g, "renders")
        .iter()
        .find(|r| num(r, "wet") == 0.0 && r.get("burst").and_then(JsonValue::as_str) == Some("noise"))
        .unwrap();
    assert!(flag(want, "masterIsCaptureFirstSecond"), "Swift's is");
    let got = render(0.0, 0.6, 0.5);
    assert!(got.capture_left.len() >= SR as usize, "{}", got.capture_left.len());
    let n = SR as usize;
    assert!(
        same_bits(&got.left[..n], &got.capture_left[..n]),
        "the master is the deck's EQ output, unchanged"
    );
}

/// `onItRingsAfterTheSoundStopsAndTakesStayDry`.
#[test]
fn on_it_rings_after_the_sound_stops_and_takes_stay_dry() {
    let got = render(0.5, 0.6, 0.5);
    let tail = (SR * 0.3) as usize..(SR * 0.6) as usize;
    assert!(rms(&got.left[tail.clone()]) > 1e-3, "{}", rms(&got.left[tail.clone()]));
    assert!(rms(&got.right[tail.clone()]) > 1e-3);
    assert_eq!(rms(&got.capture_left[tail]), 0.0, "LOOP record hears the deck dry");
}

/// The tail's mean level over the windows 2..15 (0.1 to 0.75 s), dB, and its decay slope in
/// dB per second, fitted over the windows from 2 to where it falls below -90 dB (or 30).
fn level_and_slope(w: &[f64]) -> (f64, f64) {
    let lv: f64 = (2..15).map(|k| db(w[k])).sum::<f64>() / 13.0;
    let end = (2..30).find(|&k| db(w[k]) < -90.0).unwrap_or(30).max(5);
    let xs: Vec<f64> = (2..end).map(|k| k as f64 * 0.05).collect();
    let ys: Vec<f64> = (2..end).map(|k| db(w[k])).collect();
    let n = xs.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let slope = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum::<f64>()
        / xs.iter().map(|x| (x - mx) * (x - mx)).sum::<f64>();
    (lv, slope)
}

fn swift_windows(r: &JsonValue) -> Vec<f64> {
    arr(r, "rmsLeft")
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect()
}

/// Swift's renders of the noise burst: (wet, size, tone, windows).
fn renders() -> Vec<(f64, f64, f64, Vec<f64>)> {
    arr(&golden(), "renders")
        .iter()
        .filter(|r| r.get("burst").and_then(JsonValue::as_str) == Some("noise"))
        .map(|r| (num(r, "wet"), num(r, "size"), num(r, "tone"), swift_windows(r)))
        .collect()
}

/// Swift's renders of another burst kind: (wet, windows left, windows right).
fn kind_renders(kind: &str) -> Vec<(f64, Vec<f64>, Vec<f64>)> {
    arr(&golden(), "renders")
        .iter()
        .filter(|r| r.get("burst").and_then(JsonValue::as_str) == Some(kind))
        .map(|r| {
            let right = arr(r, "rmsRight").iter().map(|v| v.as_f64().unwrap()).collect();
            (num(r, "wet"), swift_windows(r), right)
        })
        .collect()
}

/// The level and the decay of the tail, against Swift's AUReverb2, for every noise setting with
/// `tone` at 0.5 or above and a decay of 0.7 s or more (the fit's own: `RETURN_BASE x
/// T^RETURN_EXPONENT` was centred over noise, 1 kHz and 80 Hz). Measured: the level 2.5 to 3.1 dB
/// above the unit's (so the offsets, which hold the dependence on size, wet and tone, are within
/// 0.6 dB of each other), the slope within 2 dB/s (7 % at the worst, tone 1: the unit decays
/// exactly at its nominal 30 dB/s there, this a little faster). Held to 4 dB, a spread of 1.2 dB
/// and 8 %.
#[test]
fn the_tail_is_swifts_in_level_and_decay() {
    let mut offsets = Vec::new();
    for (wet, size, tone, sw) in renders() {
        if wet == 0.0 || size < 0.3 || tone < 0.5 {
            continue;
        }
        let rw = windows(&render(wet, size, tone).left);
        let (sl, ss) = level_and_slope(&sw);
        let (rl, rs) = level_and_slope(&rw);
        let at = format!("wet {wet} size {size} tone {tone}");
        assert!((rl - sl).abs() <= 4.0, "{at}: level Rust {rl:.1} dB, Swift {sl:.1} dB");
        assert!(
            (rs - ss).abs() <= 0.08 * ss.abs().max(20.0),
            "{at}: decay Rust {rs:.1} dB/s, Swift {ss:.1} dB/s"
        );
        offsets.push(rl - sl);
    }
    assert_eq!(offsets.len(), 6);
    let (lo, hi) = offsets.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    assert!(hi - lo <= 1.2, "the error moves with the setting: {lo:.1} to {hi:.1} dB");
}

/// Wet is a linear crossfade: with the room's own return still silent (the first window, before
/// the 18 ms predelay has more than started), the dry burst drops by 6.02 dB at 0.5, as Swift's
/// (-18.4 dB against -12.5).
#[test]
fn wet_is_a_linear_crossfade() {
    for (wet, size, tone, sw) in renders() {
        if wet == 0.0 || (size, tone) != (0.6, 0.5) {
            continue;
        }
        let rw = windows(&render(wet, size, tone).left);
        // (Swift's window 0 already holds some early reflections at 1; the unit is the same
        // crossfade within a dB for the two lower settings)
        if wet < 1.0 {
            assert!(
                (db(rw[0]) - db(sw[0])).abs() <= 1.0,
                "wet {wet}: dry window Rust {:.1} dB, Swift {:.1} dB",
                db(rw[0]),
                db(sw[0])
            );
        }
    }
    let dry = windows(&render(0.0, 0.6, 0.5).left)[0];
    let half = windows(&render(0.5, 0.6, 0.5).left)[0];
    assert!((db(half) - db(dry) + 6.02).abs() <= 0.8, "{} {}", db(half), db(dry));
}

/// Short rooms: size 0.25 (Swift's own "Room" preset, 0.6 s) is within 4 dB of the unit over the
/// windows 1 to 4 (measured 3.6 dB at the worst), and size 0 (0.25 s) within 10 dB over the windows
/// 2 to 4 (8.2 dB: the FDN's lines are 30 to 80 ms, so its first window falls more slowly than
/// the unit's), and both are gone (below -90 dB) by the end of the first second.
#[test]
fn short_rooms_are_close_and_gone_within_a_second() {
    for (size, from, bound) in [(0.25, 1, 4.0), (0.0, 2, 10.0)] {
        let (_, _, _, sw) = renders()
            .into_iter()
            .find(|(w, s, t, _)| (*w, *s, *t) == (0.5, size, 0.5))
            .unwrap();
        let rw = windows(&render(0.5, size, 0.5).left);
        for k in from..5 {
            assert!(
                (db(rw[k]) - db(sw[k])).abs() <= bound,
                "size {size}, window {k}: Rust {:.1} dB, Swift {:.1} dB",
                db(rw[k]),
                db(sw[k])
            );
        }
        for (who, w) in [("Swift", &sw), ("Rust", &rw)] {
            assert!(db(w[20]) < -90.0, "size {size}, {who}: {:.1}", db(w[20]));
        }
    }
}

fn tail_of(w: &[f64]) -> f64 {
    (2..15).map(|k| db(w[k])).sum::<f64>() / 13.0
}

fn swift_row(size: f64, tone: f64) -> Vec<f64> {
    renders()
        .into_iter()
        .find(|(w, s, t, _)| (*w, *s, *t) == (0.5, size, tone))
        .unwrap()
        .3
}

/// `tone` below 0.25 is where the unit stops being a lowpass in the loop: at 0.1 it is 5 dB under
/// its own 0.5 and at 0 it is 15 dB OVER it (a decay of 0.12 s at the Nyquist against 2 s at the
/// lows). The stand-in stays audible there (a floor on the loop's lowpass: an unfloored one is
/// silent below tone 0.05) and darker, and is held to the unit only loosely, as the differences
/// measured: 0.25 within 5 dB (3.8), 0.1 within 9 dB (6.8), a short dark room (size 0.25, tone 0.1)
/// within 14 dB (12.3), tone 0 below the unit by more than 8 dB (13.8) but within 8 dB of its own
/// 0.5, the decay slope within 20 % of the unit's.
#[test]
fn low_tones_are_audible_here_and_erratic_in_swift() {
    let mid = tail_of(&windows(&render(0.5, 0.6, 0.5).left));
    for (size, tone, bound) in [(0.6, 0.25, 5.0), (0.6, 0.1, 9.0), (0.25, 0.1, 14.0)] {
        let r = tail_of(&windows(&render(0.5, size, tone).left));
        let sw = tail_of(&swift_row(size, tone));
        assert!((r - sw).abs() <= bound, "size {size} tone {tone}: Rust {r:.1} dB, Swift {sw:.1} dB");
    }
    // tone 0
    let (swift_dark, swift_mid) = (level_and_slope(&swift_row(0.6, 0.0)), level_and_slope(&swift_row(0.6, 0.5)));
    assert!(swift_dark.0 > swift_mid.0 + 10.0, "Swift: {swift_dark:?} {swift_mid:?}");
    let rust_dark = level_and_slope(&windows(&render(0.5, 0.6, 0.0).left));
    assert!(rust_dark.0 > mid - 8.0, "the stand-in must not go silent: {:.1} dB against {mid:.1}", rust_dark.0);
    assert!(rust_dark.0 < swift_dark.0 - 8.0, "the known difference: {rust_dark:?} {swift_dark:?}");
    assert!((rust_dark.1 - swift_dark.1).abs() <= 0.2 * swift_dark.1.abs(), "{rust_dark:?} {swift_dark:?}");
    // and the lowest tone of all stays audible in a short room
    assert!(tail_of(&windows(&render(0.5, 0.25, 0.0).left)) > -80.0);
}

/// The lows come back as the unit's do (the unit returns an 80 Hz burst within 2 dB of a 1 kHz
/// one; the runner's send reverb highpasses its input at 200 Hz, this one does not): each kind
/// within 4 dB of the unit (measured -0.3 dB for 1 kHz, -2.8 dB for 80 Hz), and the lows within
/// 6 dB of the mids.
#[test]
fn the_lows_come_back_as_the_units_do() {
    let wet1 = |kind: &str| {
        let swift = kind_renders(kind).into_iter().find(|r| r.0 == 1.0).unwrap();
        let rust = windows(&render_of(1.0, 0.6, 0.5, kind).left);
        (tail_of(&swift.1), tail_of(&rust))
    };
    let (swift_low, rust_low) = wet1("low");
    let (swift_mid, rust_mid) = wet1("mid");
    assert!((rust_low - swift_low).abs() <= 4.0, "80 Hz: Rust {rust_low:.1} dB, Swift {swift_low:.1} dB");
    assert!((rust_mid - swift_mid).abs() <= 4.0, "1 kHz: Rust {rust_mid:.1} dB, Swift {swift_mid:.1} dB");
    assert!(swift_mid - swift_low < 6.0 && rust_mid - rust_low < 6.0, "{swift_mid} {swift_low} {rust_mid} {rust_low}");
}

/// The input is the mono sum, in the unit and here: a burst with the right channel inverted
/// leaves no room on either side.
#[test]
fn a_burst_with_the_right_channel_inverted_returns_nothing() {
    let swift = kind_renders("wide").into_iter().find(|r| r.0 == 1.0).unwrap();
    let got = render_of(1.0, 0.6, 0.5, "wide");
    let (wl, wr) = (windows(&got.left), windows(&got.right));
    for k in 2..15 {
        for (who, w) in [("Swift L", &swift.1), ("Swift R", &swift.2), ("Rust L", &wl), ("Rust R", &wr)] {
            assert!(db(w[k]) < -100.0, "{who}, window {k}: {:.1} dB", db(w[k]));
        }
    }
}

/// The room is after the EQ and the capture tap and BEFORE the fader and the cue send, as
/// Swift's graph has it (`wire`: source -> EQ -> reverb -> strip, with the cue send after the
/// reverb): a closed fader leaves no tail on the master, and the cue feed (PFL, ahead of the
/// fader) hears it.
#[test]
fn the_fader_is_after_the_room_and_the_cue_send_hears_it() {
    let tail = |wet: f64, volume: f64, pfl: bool| -> (f64, f64) {
        let (mut e, mut r) = rig(wet, 0.8, 0.5, "noise");
        e.set_volume("A", volume);
        e.set_pfl("A", pfl);
        e.set_headphone_mix(0.0); // the cue feed alone
        e.set_headphone_level(1.0);
        let (mut l, mut rr) = (vec![0f32; WINDOW], vec![0f32; WINDOW]);
        let (mut cl, mut cr) = (vec![0f32; WINDOW], vec![0f32; WINDOW]);
        let (mut master, mut cue) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            r.render_with_cue(&mut l, &mut rr, Some((&mut cl, &mut cr)));
            master.push(rms(&l));
            cue.push(rms(&cl));
        }
        let mean = |v: &[f64]| v[8..28].iter().sum::<f64>() / 20.0;
        (mean(&master), mean(&cue))
    };
    let (m_open, c_open) = tail(1.0, 1.0, false);
    assert!(m_open > 1e-4, "the room rings on the master: {m_open}");
    assert!(c_open < 1e-9, "no PFL, no cue: {c_open}");
    let (m_shut, c_shut) = tail(1.0, 0.0, true);
    assert!(m_shut < 1e-9, "a closed fader leaves no tail on the master: {m_shut}");
    assert!(c_shut > 1e-4, "the cue feed is ahead of the fader and hears the room: {c_shut}");
    let (_, c_dry) = tail(0.0, 0.0, true);
    assert!(c_dry < 1e-9, "and nothing without the room: {c_dry}");
}

/// `ReverbTests.theKnobsReachTheUnit`, through `perform` as Swift's does: the deck's three knobs
/// and the loop channel's reach the unit the render thread holds, and the strip reports them
/// back (`currentParams`).
#[test]
fn the_knobs_reach_the_unit() {
    use djrust::session::action::DeckAction;
    use djrust::host::deck::perform::{Applied, PerformCtx};
    let mut cfg = EngineConfig::new(SR);
    cfg.decks = vec!["A".into()];
    let (mut e, mut r) = engine(cfg);
    let set = |e: &mut Engine, deck: &str, name: &str, value: f64| {
        e.perform(
            &DeckAction::Param { deck: deck.into(), name: name.into(), value },
            PerformCtx::default(),
        )
    };
    assert_eq!(set(&mut e, "A", "reverbWet", 0.4), Applied::Done);
    assert_eq!(set(&mut e, "A", "reverbSize", 1.0), Applied::Done);
    assert_eq!(set(&mut e, "A", "reverbTone", 0.0), Applied::Done);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    r.render_block(&mut l, &mut rr);
    let u = r.strip_reverb("A").unwrap();
    assert!(u.is_on());
    assert_eq!((u.wet(), u.size(), u.tone()), (0.4, 1.0, 0.0));
    assert!((u.decay_seconds() - 8.0).abs() < 1e-9);
    let p = e.current_params("A").unwrap();
    assert_eq!((p.reverb_wet, p.reverb_size, p.reverb_tone), (0.4, 1.0, 0.0));
    // the loop channel's own
    assert!(!r.loop_reverb().is_on());
    assert_eq!(set(&mut e, "L", "reverbWet", 0.3), Applied::Done);
    r.render_block(&mut l, &mut rr);
    assert!(r.loop_reverb().is_on() && r.loop_reverb().wet() == 0.3);
    assert_eq!(e.loop_channel.reverb_wet, 0.3);
    // a session's starting knobs, a journal restore, and a switch-off all reach it
    let mut starting = e.current_params("A").unwrap();
    starting.reverb_wet = 0.0;
    starting.reverb_size = 0.2;
    e.apply_params("A", &starting);
    r.render_block(&mut l, &mut rr);
    let u = r.strip_reverb("A").unwrap();
    assert!(!u.is_on() && u.size() == 0.2);
}

fn blocks(e: &mut Engine, r: &mut Renderer, n: usize) -> f32 {
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let mut peak = 0f32;
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        peak = l.iter().chain(&rr).fold(peak, |p, v| p.max(v.abs()));
    }
    peak
}

/// The loop channel has a reverb after its EQ, as every `Deck` does: a clip taken out of the row
/// leaves a room ringing on L with the knob up, and nothing without; and the room is ahead of
/// the channel's fader (a closed fader leaves no tail).
#[test]
fn the_loop_channel_has_a_reverb() {
    use djrust::TrackSpec;
    let tail = |wet: f64, volume: f64| -> f32 {
        let mut cfg = EngineConfig::new(SR);
        cfg.extended = false;
        let (mut e, mut r) = engine(cfg);
        e.set_crossfader(1.0); // A out of the mix: only L is heard
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        e.play("A");
        blocks(&mut e, &mut r, 900);
        let id = e.grab_last_bars("A", 1).expect("a bar was played");
        e.set_looping(&id, true);
        e.pause("A");
        blocks(&mut e, &mut r, 400);
        assert!(blocks(&mut e, &mut r, 100) > 0.01, "L plays");
        e.set_loop_reverb(djrust::fx::ReverbKnob::Size, 0.8);
        e.set_loop_reverb(djrust::fx::ReverbKnob::Wet, wet);
        blocks(&mut e, &mut r, 20);
        e.set_looping(&id, false); // out on the next launch boundary (a bar: under 2.5 s)
        blocks(&mut e, &mut r, 300);
        e.set_loop_volume(volume);
        blocks(&mut e, &mut r, 4); // the fader's ramp
        blocks(&mut e, &mut r, 50)
    };
    let dry = tail(0.0, 0.8);
    let wet = tail(0.8, 0.8);
    let shut = tail(0.8, 0.0);
    // (A's cos(pi/2) crossfader gain leaks ~1e-17 of its tail)
    assert!(dry < 1e-6, "no reverb: the clip is gone, so is the sound: {dry}");
    // (measured 2.5e-4 against under 1e-6: the clip has been out for a second or two)
    assert!(wet > 1e-5 && wet > 100.0 * dry, "the room rings on L: {wet} against {dry}");
    assert!(shut < 1e-9, "the fader is after the room: a closed one leaves no tail: {shut}");
}

/// A journal checkpoint restores the knobs into the unit the render thread holds (the deck's
/// and the loop channel's), not only into the engine's own record of them.
#[test]
fn a_journal_restore_reaches_the_unit() {
    use djrust::fx::ReverbKnob;
    use djrust::journal::stores::JournalStoreState;
    let mk = || {
        let mut cfg = EngineConfig::new(SR);
        cfg.decks = vec!["A".into()];
        engine(cfg)
    };
    let (mut e, mut r) = mk();
    e.set_reverb("A", "reverbWet", 0.4);
    e.set_reverb("A", "reverbSize", 0.9);
    e.set_loop_reverb(ReverbKnob::Wet, 0.3);
    e.set_loop_reverb(ReverbKnob::Tone, 0.2);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    r.render_block(&mut l, &mut rr);
    let state = e.journal_state(JournalStoreState::default(), false);
    let (mut e2, mut r2) = mk();
    let warnings = e2.restore_state(&state, &|_| None, &mut |_| {
        let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
        r2.render_block(&mut l, &mut rr);
    });
    assert!(warnings.is_empty(), "{warnings:?}");
    r2.render_block(&mut l, &mut rr);
    let u = r2.strip_reverb("A").unwrap();
    assert!(u.is_on() && u.wet() == 0.4 && u.size() == 0.9, "deck A: {} {} {}", u.is_on(), u.wet(), u.size());
    let lu = r2.loop_reverb();
    assert!(lu.is_on() && lu.wet() == 0.3 && lu.tone() == 0.2, "L: {} {} {}", lu.is_on(), lu.wet(), lu.tone());
}
