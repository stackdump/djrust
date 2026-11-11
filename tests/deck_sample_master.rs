//! A sample deck's master path (`DeckRenderer`'s sample half): the turntable and the clip's FX
//! through the deck's own master bus (a `LoopMasterBus`, bit-transparent at rest) and a separate
//! safety limiter, not the net deck's chain. Master macros fired at it land on the next 4-beat line
//! of the deck's grid (`Deck.sampleMasterFire`), the vinyl brake slows the turntable and the clip's
//! FX together and its release skips the playhead on by the frames it fell behind
//! (`Turntable.catchUp`), a hand on the platter cancels it, and a clip moved onto the deck keeps
//! its FX.
//!
//! Parity form (a): each test names the Swift test whose facts it asserts
//! (`MasterFxDeckTests`, sample half: lines 209-390; `LoopFxDeckTests` for the clip's chain). The
//! Swift engine's output for the same calls is in `samplemaster_golden.rs` (form b).

use std::sync::Arc;

use djrust::deck::beat_grid::BeatGrid;
use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::fx::unit::{FxParam, FxUnitSettings};
use djrust::fx::{MasterChain, MasterFxKey, MasterMacro};
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BAR120: usize = 96_000; // one bar at 120 bpm, 48 kHz
const CLICK_STEP: usize = 6000; // sixteenths at 120 bpm

fn sine(hz: f64, amplitude: f64, seconds: f64) -> Vec<f32> {
    let n = (SR * seconds) as usize;
    (0..n)
        .map(|i| (amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / SR).sin()) as f32)
        .collect()
}

fn buffer(x: Vec<f32>) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer::new(x.clone(), Some(x), SR))
}

/// A low (375 Hz) and a bright (6 kHz) sine together, 2 s of whole periods (`twoTones`).
fn two_tones() -> Arc<SampleBuffer> {
    let lo = sine(375.0, 0.2, 2.0);
    let hi = sine(6000.0, 0.2, 2.0);
    buffer(lo.iter().zip(&hi).map(|(a, b)| a + b).collect())
}

/// One bar of a click on every sixteenth (a single full-scale sample), silence between.
fn click_track() -> Arc<SampleBuffer> {
    let mut x = vec![0f32; BAR120];
    for k in (0..BAR120).step_by(CLICK_STEP) {
        x[k] = 1.0;
    }
    buffer(x)
}

/// The frames in `range` where a click starts: above 0.1 after at least 1000 quiet frames.
fn onsets(x: &[f32], range: std::ops::Range<usize>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut last = i64::MIN / 2;
    for i in range {
        if i < x.len() && x[i].abs() > 0.1 {
            if i as i64 - last > 1000 {
                out.push(i);
            }
            last = i as i64;
        }
    }
    out
}

fn tone_amplitude(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / SR;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    let p = s1 * s1 + s2 * s2 - c * s1 * s2;
    2.0 * p.sqrt() / x.len() as f64
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// `fundamental` of `AudioMeasure.swift`: the first strong peak of the normalised
/// autocorrelation over 40 Hz...2 kHz, refined by a parabola.
fn fundamental(x: &[f32], sr: f64) -> f64 {
    let v: Vec<f64> = x.iter().map(|&s| s as f64).collect();
    let min_lag = (sr / 2000.0) as usize;
    let max_lag = ((sr / 40.0) as usize).min(v.len() / 2);
    let mut r = vec![0.0f64; max_lag + 2];
    for lag in (min_lag - 1)..=(max_lag + 1) {
        let (mut s, mut ea, mut eb) = (0.0, 0.0, 0.0);
        for i in 0..v.len() - lag {
            s += v[i] * v[i + lag];
            ea += v[i] * v[i];
            eb += v[i + lag] * v[i + lag];
        }
        r[lag] = s / (ea * eb).sqrt().max(1e-30);
    }
    let best = r[min_lag..=max_lag].iter().cloned().fold(f64::MIN, f64::max);
    let mut lag = min_lag;
    while lag < max_lag && r[lag] < 0.9 * best {
        lag += 1;
    }
    while lag < max_lag && r[lag + 1] > r[lag] {
        lag += 1;
    }
    let (a, b, c) = (r[lag - 1], r[lag], r[lag + 1]);
    let p = (a - c) / (2.0 * (a - 2.0 * b + c));
    sr / (lag as f64 + if p.is_finite() { p } else { 0.0 })
}

fn cents(f: f64, reference: f64) -> f64 {
    1200.0 * (f / reference).log2()
}

/// A sample deck A at 48 kHz playing `buffer` as a loop from frame 0 at full speed, its grid at
/// 120 bpm (offset 0), on an engine whose mixer is flat (`sampleDeck`; the Swift test pulls the
/// deck's `renderBlock`, here the strip is a unity pass: no EQ, the fader up, the crossfader on A).
struct Rig {
    e: Engine,
    r: Renderer,
}

impl Rig {
    fn new(b: Arc<SampleBuffer>) -> Rig {
        let mut c = EngineConfig::new(SR);
        c.decks = vec!["A".into()];
        let (mut e, r) = engine(c);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        let grid = BeatGrid::new(120.0, 0.0, 1.0);
        let len = b.length() as f64;
        assert!(e.load_sample_with("A", PreparedSample::with_grid(b, "tones", Some(grid)), false));
        e.turntable("A", TurntableCommand::Loop { start: 0.0, end: len });
        e.launch("A");
        Rig { e, r }
    }

    fn mirror(&self) -> Arc<djrust::host::deck::DeckMirror> {
        self.e.deck("A").unwrap().mirror.clone()
    }

    fn active(&self, m: MasterMacro) -> bool {
        self.mirror().is_master_active(m)
    }

    fn active_macros(&self) -> Vec<MasterMacro> {
        self.mirror().active_master_macros()
    }
}

/// `frames` (a multiple of 512) of the deck, both channels; `at(rig, f)` runs before the block at
/// frame `f` (the main thread's pokes).
fn render(
    rig: &mut Rig,
    frames: usize,
    mut at: impl FnMut(&mut Rig, usize),
) -> (Vec<f32>, Vec<f32>) {
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    let mut f = 0;
    while f < frames {
        at(rig, f);
        rig.r.render_block(&mut l, &mut r);
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&r);
        f += 512;
    }
    (out_l, out_r)
}

fn nothing(_: &mut Rig, _: usize) {}

// MasterFxDeckTests.aSampleDeckWithoutMacrosIsBitIdentical
#[test]
fn a_sample_deck_without_macros_is_bit_identical() {
    let b = two_tones();
    let mut rig = Rig::new(b.clone());
    let out = render(&mut rig, 2 * BAR120, nothing);
    // the bare turntable, as the deck played before it had a master bus
    let mut tt = djrust::deck::turntable::Turntable::new(SR);
    tt.load(b.clone());
    tt.send(TurntableCommand::Gain(1.0, true));
    tt.send(TurntableCommand::Loop {
        start: 0.0,
        end: b.length() as f64,
    });
    tt.send(TurntableCommand::Start);
    let (mut ref_l, mut ref_r) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    for _ in 0..(2 * BAR120 / 512) {
        tt.render(512, &mut l, Some(&mut r));
        ref_l.extend_from_slice(&l);
        ref_r.extend_from_slice(&r);
    }
    // (the first block is the strip's fader ramping up from silence: the bytes are the
    // turntable's from the next one)
    assert!(out.0[512..] == ref_l[512..], "left");
    assert!(out.1[512..] == ref_r[512..], "right");
    let (_, _, bypassed) = rig.mirror().master_filter_corners();
    assert!(bypassed && rig.active_macros().is_empty());
}

// MasterFxDeckTests.aSampleDeckSweepsItsLowpass
#[test]
fn a_sample_deck_sweeps_its_lowpass() {
    let mut dry_rig = Rig::new(two_tones());
    let dry = render(&mut dry_rig, 4 * BAR120, nothing);
    let mut rig = Rig::new(two_tones());
    let mut fire = -1i64;
    let wet = render(&mut rig, 4 * BAR120, |rig, f| {
        if f == 512 {
            // fired after the first block (the turntable has its sample): it waits for the deck's
            // next downbeat, bar 2
            fire = rig.e.sample_master_fire("A").0;
            rig.e.fire_master("A", MasterMacro::SweepLp, Some(1));
        }
        if f == BAR120 / 2 / 512 * 512 {
            assert_eq!(rig.active_macros(), vec![MasterMacro::SweepLp], "waiting counts");
        }
    });
    assert!((fire - BAR120 as i64).abs() <= 1, "{fire}");
    let fire = fire as usize;
    assert!(wet.0[512..fire] == dry.0[512..fire], "nothing before its bar");
    // 0.8 s into the bar, the LP is half way down; 4608 frames = whole periods of both tones
    let w = (fire + 38_000)..(fire + 38_000 + 4608);
    let hf = db(tone_amplitude(&wet.0[w.clone()], 6000.0) / tone_amplitude(&dry.0[w.clone()], 6000.0));
    let lf = db(tone_amplitude(&wet.0[w.clone()], 375.0) / tone_amplitude(&dry.0[w], 375.0));
    assert!(hf < -20.0, "the 6 kHz tone is swept out: {hf} dB");
    assert!(hf < lf - 15.0, "far more than the 375 Hz one ({lf} dB)");
    // a bar and a bit later, all of it is back, and the bytes are the turntable's again
    let after = (fire + BAR120 + 24_000)..(fire + BAR120 + 24_000 + 4608);
    let back = tone_amplitude(&wet.0[after.clone()], 6000.0);
    assert!(db(back / tone_amplitude(&dry.0[after], 6000.0)).abs() < 0.1);
    assert!(wet.0[fire + 2 * BAR120..] == dry.0[fire + 2 * BAR120..]);
    assert!(wet.1[fire + 2 * BAR120..] == dry.1[fire + 2 * BAR120..]);
    assert!(rig.active_macros().is_empty());
    assert!(rig.mirror().master_filter_corners().2);
}

// MasterFxDeckTests.octaveUpShiftsASampleDeck
#[test]
fn octave_up_shifts_a_sample_deck() {
    // 220 whole periods a second: a seamless loop
    let mut rig = Rig::new(buffer(sine(220.0, 0.3, 1.0)));
    let w = (BAR120 + 24_000)..(BAR120 + 24_000 + 8192);
    let mut active = false;
    let out = render(&mut rig, 3 * BAR120, |rig, f| {
        if f == 512 {
            rig.e.fire_master("A", MasterMacro::OctaveUp, Some(2)); // lands on bar 2
        }
        if f == w.start / 512 * 512 {
            active = rig.active(MasterMacro::OctaveUp);
        }
    });
    let before = fundamental(&out.0[20_000..20_000 + 8192], SR);
    assert!(cents(before, 220.0).abs() < 10.0, "dry: {before} Hz");
    // The shift is the net decks' chain's: beats' Tone.PitchShift, whose Chrome sawtooths sweep the
    // delay a little faster than 1 + factor (an "octave" is ~1277 cents up at 48 kHz). Measured
    // against the same chain on the same sine, not against an exact octave.
    let mut chain = MasterChain::new(SR);
    chain.apply(MasterFxKey::MasterPitch, 12, 0.0);
    let mut sine2 = sine(220.0, 0.3, 2.0);
    for chunk in sine2.chunks_mut(512) {
        let m = chunk.len();
        chain.process(chunk, None, 0, m);
        chain.finish(m);
    }
    let want = fundamental(&sine2[24_000..24_000 + 8192], SR);
    assert!(
        (cents(want, 220.0) - 1200.0).abs() < 100.0,
        "the chain's octave: {} cents",
        cents(want, 220.0)
    );
    let up = fundamental(&out.0[w], SR);
    assert!(
        cents(up, want).abs() < 10.0,
        "the chain's octave up: {} vs {} cents",
        cents(up, 220.0),
        cents(want, 220.0)
    );
    assert!(active, "running while it sounds");
}

// MasterFxDeckTests.aSampleDeckBrakesAndLandsOnTheBeat
#[test]
fn a_sample_deck_brakes_and_lands_on_the_beat() {
    let clicks = click_track();
    let mut dry_rig = Rig::new(clicks.clone());
    let dry = render(&mut dry_rig, 4 * BAR120, nothing);
    let mut rig = Rig::new(clicks);
    let mut fire = -1i64;
    let wet = render(&mut rig, 4 * BAR120, |rig, f| {
        if f == 512 {
            fire = rig.e.sample_master_fire("A").0; // bar 2
            rig.e.fire_master("A", MasterMacro::VinylBrake, Some(1));
        }
        if f == 236 * 512 {
            // 24 832 frames into the spin-down
            assert!(rig.active(MasterMacro::VinylBrake));
        }
    });
    assert!((fire - BAR120 as i64).abs() <= 1, "{fire}");
    let fire = fire as usize;
    assert!(wet.0[512..fire] == dry.0[512..fire], "nothing before its bar");
    // the spin-down (3/4 of the bar): each sixteenth comes later than the last, the rate falling
    let spin = onsets(&wet.0, fire..fire + 3 * BAR120 / 4);
    assert!(spin.len() >= 5, "{spin:?}");
    let gaps: Vec<usize> = spin.windows(2).map(|w| w[1] - w[0]).collect();
    for w in gaps.windows(2) {
        assert!(w[1] > w[0], "slowing: {gaps:?}");
    }
    assert!(*gaps.last().unwrap() > 3 * CLICK_STEP / 2, "well below full speed: {gaps:?}");
    // released a bar after the fire, over 5 ms: the next click is on the unbraked grid
    let release = fire + BAR120;
    let after = onsets(&wet.0, release + 240..release + BAR120);
    let grid = onsets(&dry.0, release + 240..release + BAR120);
    let (first, want) = (after[0], grid[0]);
    assert!(
        (first as i64 - want as i64).abs() <= 48,
        "within 1 ms of the grid: {first} vs {want}"
    );
    assert_eq!(after.len(), grid.len());
    assert!(rig.active_macros().is_empty());
}

// MasterFxDeckTests.panicReleasesABrakingSampleDeck
#[test]
fn panic_releases_a_braking_sample_deck() {
    let clicks = click_track();
    let mut dry_rig = Rig::new(clicks.clone());
    let dry = render(&mut dry_rig, 4 * BAR120, nothing);
    let mut rig = Rig::new(clicks);
    let mut fire = -1i64;
    let panic_at = 228 * 512; // under a quarter of a bar into a 2-bar brake (blocks of 512)
    let wet = render(&mut rig, 4 * BAR120, |rig, f| {
        if f == 512 {
            fire = rig.e.sample_master_fire("A").0;
            rig.e.fire_master("A", MasterMacro::VinylBrake, Some(2));
        }
        if f == panic_at {
            assert!(rig.active(MasterMacro::VinylBrake));
            rig.e.panic("A");
        }
    });
    assert!((fire - BAR120 as i64).abs() <= 1, "{fire}");
    assert!(rig.active_macros().is_empty());
    // 5 ms out, 5 ms back in: then the clicks are the unbraked deck's, long before the 2 bars
    let from = panic_at + 2 * 240 + 512;
    let after = onsets(&wet.0, from..from + BAR120);
    let grid = onsets(&dry.0, from..from + BAR120);
    assert!(!after.is_empty() && after.len() == grid.len(), "{after:?} vs {grid:?}");
    for (w, d) in after.iter().zip(&grid) {
        assert!((*w as i64 - *d as i64).abs() <= 48, "{w} vs {d}");
    }
}

// MasterFxDeckTests.aJogCancelsASampleDecksBrake
#[test]
fn a_jog_cancels_a_sample_decks_brake() {
    let mut rig = Rig::new(click_track());
    render(&mut rig, BAR120 + 20 * 512, |rig, f| {
        if f == 512 {
            rig.e.fire_master("A", MasterMacro::VinylBrake, Some(1));
        }
    });
    assert!(rig.active(MasterMacro::VinylBrake));
    rig.e.turntable("A", TurntableCommand::Freeze); // a hand on the platter: the jog wins
    render(&mut rig, 512, nothing);
    assert!(!rig.active(MasterMacro::VinylBrake));
    rig.e.turntable("A", TurntableCommand::Release);
}

// MasterFxDeckTests.aSampleDeckSliderAndPanicReachItsBus
#[test]
fn a_sample_deck_slider_and_panic_reach_its_bus() {
    let mut rig = Rig::new(two_tones());
    render(&mut rig, 8 * 512, |rig, f| {
        if f == 0 {
            rig.e.fire_master("A", MasterMacro::SweepHp, Some(4));
            rig.e.set_master_fx("A", MasterFxKey::LpFreq, 40);
        }
    });
    assert_eq!(
        rig.e.deck("A").unwrap().master_fx_set.get(&(MasterFxKey::LpFreq.index() as u8)),
        Some(&40)
    );
    assert!(rig.active(MasterMacro::SweepHp));
    rig.e.panic("A");
    render(&mut rig, 512, nothing);
    assert!(rig.active_macros().is_empty());
}

// LoopMasterBus.runningMask: "Fires sent but maybe not yet seen by the audio thread (they count as
// running for the UI until it has applied the last of them)"; `panic()` drops them
// (`queuedMask = 0`)
#[test]
fn a_fire_the_audio_thread_has_not_taken_yet_counts_as_running() {
    let mut rig = Rig::new(two_tones());
    render(&mut rig, 4 * 512, nothing);
    assert!(rig.active_macros().is_empty());
    rig.e.fire_master("A", MasterMacro::SweepLp, Some(1));
    assert!(rig.active(MasterMacro::SweepLp), "queued: running as far as the UI can tell");
    assert_eq!(rig.active_macros(), vec![MasterMacro::SweepLp]);
    assert!(!rig.active(MasterMacro::SweepHp), "only the one fired");
    // a block later the bus has it, waiting for its bar: its own mask says the same
    render(&mut rig, 512, nothing);
    assert_eq!(rig.active_macros(), vec![MasterMacro::SweepLp], "waiting counts");
    // a panic empties the queue at once, and the bus at its next block
    rig.e.fire_master("A", MasterMacro::SweepHp, Some(1));
    assert!(rig.active(MasterMacro::SweepHp));
    rig.e.panic("A");
    assert!(!rig.active(MasterMacro::SweepHp), "a panic empties the queue");
    render(&mut rig, 512, nothing);
    assert!(rig.active_macros().is_empty());
}

// MasterFxDeckTests.aSampleDeckSweepsItsLowpass, the firing frame: a stopped deck fires at once
// (the next block), a playing one on the next 4-beat line, counting beats in the deck's own
// numbering (`sampleMasterFire`)
#[test]
fn a_master_fire_lands_on_the_next_four_beat_line_of_the_decks_grid() {
    let mut rig = Rig::new(two_tones());
    render(&mut rig, 512, nothing);
    // playing at 120 bpm from the top: beat 0 was frame 0, the next line is bar 2 (4 beats = 1 bar)
    let (frame, bpm) = rig.e.sample_master_fire("A");
    assert_eq!(bpm, 120.0);
    assert!((frame - BAR120 as i64).abs() <= 1, "{frame}");
    // a little later in the bar: still bar 2
    render(&mut rig, 20 * 512, nothing);
    let (frame, _) = rig.e.sample_master_fire("A");
    assert!((frame - BAR120 as i64).abs() <= 1, "{frame}");
    // past it: bar 3
    render(&mut rig, BAR120, nothing);
    let (frame, _) = rig.e.sample_master_fire("A");
    assert!((frame - 2 * BAR120 as i64).abs() <= 1, "{frame}");
    // paused: at once, at the deck's tempo
    rig.e.pause("A");
    assert_eq!(rig.e.sample_master_fire("A"), (0, 120.0));
    // a deck with no grid: at once, at 120
    let mut c = EngineConfig::new(SR);
    c.decks = vec!["A".into()];
    let (mut bare, _r) = engine(c);
    assert!(bare.load_sample_with("A", PreparedSample::with_grid(two_tones(), "no grid", None), false));
    bare.launch("A");
    assert_eq!(bare.sample_master_fire("A"), (0, 120.0));
}

/// An engine whose deck A is empty, with one clip on the loop row (2 s of a 375 Hz sine at 60 bpm:
/// 2 beats) whose FX are `fx`; the clip's id.
fn clip_rig(fx: &LoopFx) -> (Engine, Renderer, String) {
    let mut c = EngineConfig::new(SR);
    c.decks = vec!["A".into()];
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    let b = sine(375.0, 0.5, 2.0);
    let id = e
        .import_clip(
            SampleBuffer::new(b.clone(), Some(b), SR),
            "tone",
            Some(60.0),
            2,
            None,
        )
        .expect("a clip");
    e.set_clip_fx(&id, fx, None);
    (e, r, id)
}

fn blocks(r: &mut Renderer, n: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&rr);
    }
    (out_l, out_r)
}

// LoopFxDeckTests.aClipMovedOntoADeckKeepsItsFx / anythingElseLoadedThereIsDry: a clip moved
// onto a deck keeps its after effects (the deck has a chain of its own, set at the load and
// changed with the clip's, behind a safety limiter); anything else loaded there plays dry
#[test]
fn a_clip_on_a_deck_keeps_its_fx_and_a_load_over_it_ends_them() {
    let fx = LoopFx {
        mode: LoopFxMode::Soft,
        drive: 18.0,
        amount: 0.9,
        ..LoopFx::default()
    };
    let (mut dry_e, mut dry_r, dry_id) = clip_rig(&LoopFx::default());
    let (mut e, mut r, id) = clip_rig(&fx);
    dry_e.play_clip(&dry_id, "A");
    e.play_clip(&id, "A");
    assert!(e.deck("A").unwrap().clip_fx_on, "its FX come with it");
    assert!(!dry_e.deck("A").unwrap().clip_fx_on);
    dry_e.launch("A");
    e.launch("A");
    let dry = blocks(&mut dry_r, 40);
    let wet = blocks(&mut r, 40);
    assert!(wet.0[8 * 512..] != dry.0[8 * 512..], "the chain is in the path");
    // the limiter holds the channel's promise (-0.5 dBFS) with the FX on
    let peak = wet.0.iter().chain(&wet.1).fold(0f32, |p, v| p.max(v.abs()));
    assert!(peak <= 0.944 + 1e-6, "peak {peak}");
    assert!(dry.0.iter().fold(0f32, |p, v| p.max(v.abs())) > 0.49, "the dry clip is as loud");
    // a change of the clip's FX reaches the deck playing it at once; off, the chain fades out
    e.set_clip_fx(&id, &LoopFx::default(), None);
    assert!(!e.deck("A").unwrap().clip_fx_on);
    blocks(&mut dry_r, 60);
    blocks(&mut r, 60);
    let (dry_tail, wet_tail) = (blocks(&mut dry_r, 8), blocks(&mut r, 8));
    assert!(wet_tail.0 == dry_tail.0 && wet_tail.1 == dry_tail.1, "dry again, to the byte");
    // a load over a clip with FX on ends them (`clipID = nil`): the same sample on both, dry
    e.set_clip_fx(&id, &fx, None);
    assert!(e.deck("A").unwrap().clip_fx_on);
    blocks(&mut r, 8);
    let other = SampleBuffer::new(sine(440.0, 0.4, 1.0), None, SR);
    for en in [&mut e, &mut dry_e] {
        let g = BeatGrid::new(120.0, 0.0, 1.0);
        let p = PreparedSample::with_grid(Arc::new(other.clone()), "other", Some(g));
        assert!(en.load_sample_with("A", p, false));
        en.launch("A");
    }
    assert!(!e.deck("A").unwrap().clip_fx_on, "anything else loaded there plays dry");
    blocks(&mut dry_r, 40);
    blocks(&mut r, 40);
    let (dry_other, wet_other) = (blocks(&mut dry_r, 8), blocks(&mut r, 8));
    assert!(wet_other.0 == dry_other.0 && wet_other.1 == dry_other.1);
}

/// 300 blocks of 128 of deck A playing a techno net at 22.05 kHz, however it came to be loaded.
fn net_deck(prepare: impl FnOnce(&mut Engine)) -> Vec<f32> {
    let (mut e, mut r) = engine(EngineConfig::new(22_050.0));
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    prepare(&mut e);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal"))));
    e.set_tempo("A", 300.0);
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    let mut out = Vec::new();
    for _ in 0..300 {
        r.render_block(&mut l, &mut rr);
        out.extend_from_slice(&l);
    }
    out
}

// FxUnitTests.FxUnitDeckTests.theSettingFollowsTheDeckAcrossModes: "the deck keeps its setting
// through a mode switch and hands it to the chain it plays through" (`loadNet`:
// `syncFxUnit()  // the FX unit moves to the net's chain`). A net loaded over a sample deck whose
// FX unit was set plays exactly as one given the same setting at once (`applyFxUnit`: the same
// `syncFxUnit`) before it played.
#[test]
fn the_fx_unit_follows_a_sample_deck_onto_a_net() {
    let mut unit = FxUnitSettings::new();
    unit.set(FxParam::Filter, -0.6);
    unit.set(FxParam::Drive, 0.4);
    let dry = net_deck(|_| {});
    let direct = net_deck(|e| e.apply_fx_unit("A", &unit));
    assert!(direct != dry, "the unit is audible on the net, or this proves nothing");
    let followed = net_deck(|e| {
        // the unit is set while the deck is a sample deck: it goes to the sample deck's own bus
        assert!(e.load_demo("A"));
        e.set_fx("A", FxParam::Filter, -0.6);
        e.set_fx("A", FxParam::Drive, 0.4);
    });
    assert!(followed == direct, "the net plays through the unit the sample deck had");
}
