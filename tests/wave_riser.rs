//! `.riser` (`Riser.swift`): a noise riser over the last two bars of each buildup into a drop,
//! mixed in by the runner. Swift's `RiserTests` ported on the runner's renders (PORTING.md's
//! proof, form (a)); the voice alone, the windows and the measures are in
//! `musicality_rests_riser.rs`, and the same renders held against Swift's output sample for
//! sample are the "riser" cases of `tests/runner_golden.rs`.
//!
//! Renders of small projects through a linear master, 120 BPM: a step is 0.125 s.

mod common;
use common::measure::*;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::riser::{RiserRise, RiserSpec};
use djrust::pflow::{NetBundle, Project, Section};
use djrust::wave::*;
use djrust::TrackSpec;

const SR: f64 = 48_000.0;
const STEP: usize = (0.125 * SR) as usize;

fn section(name: &str, steps: i64) -> Section {
    Section {
        name: name.into(),
        steps,
        phrases: Default::default(),
    }
}

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

/// Four bars of buildup into four of drop: kick on the beats, an offbeat bass. The riser window
/// is steps 32..64.
fn riser_project() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert(
        "kick",
        ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
    );
    p.nets.insert(
        "bass",
        ring("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
    );
    p.structure = vec![section("buildup", 64), section("drop", 64)];
    p
}

fn options(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        ..Default::default()
    }
}

/// Left and right of `p` from `start_tick`, for `seconds`.
fn render(p: &Project, fixes: Fixes, start_tick: i64, seconds: f64) -> (Vec<f32>, Vec<f32>) {
    let o = options(fixes);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, start_tick));
    r.play();
    let mut l = vec![0f32; (seconds * SR) as usize];
    let mut rr = l.clone();
    let mut off = 0;
    while off < l.len() {
        let n = 256.min(l.len() - off);
        r.process(&mut l[off..off + n], Some(&mut rr[off..off + n]), n);
        off += n;
    }
    (l, rr)
}

fn render12(p: &Project, fixes: Fixes) -> (Vec<f32>, Vec<f32>) {
    render(p, fixes, 0, 12.0)
}

fn minus(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

fn rms(x: &[f32], a: usize, b: usize) -> f64 {
    rms_db(&x[a..b])
}

/// RiserTests.windowsAreTheBuildupsLastTwoBars (the prepared half): a song with a buildup into a
/// drop prepares a riser with the fix on, and none without.
#[test]
fn a_project_with_a_window_prepares_a_riser() {
    let edm = TrackSpec::new("edm", 42.0, Some("extended")).project();
    assert!(WaveRunner::prepare(&edm, SR, &options(Fixes::RISER), 0)
        .riser
        .is_some());
    assert!(WaveRunner::prepare(&edm, SR, &options(Fixes::EMPTY), 0)
        .riser
        .is_none());
    // a project with no buildup into a drop has none to prepare
    assert!(
        WaveRunner::prepare(&Project::default(), SR, &options(Fixes::RISER), 0)
            .riser
            .is_none()
    );
}

/// RiserTests.highBandRises: the high band climbs through the window: by more than 9 dB, beat
/// after beat (rho >= 0.9).
#[test]
fn the_high_band_rises() {
    let p = riser_project();
    let on = render12(&p, Fixes::RISER);
    let rises = RiserRise::measure(&on.0, &on.1, SR, &p);
    assert_eq!(rises.len(), 1);
    let rise = &rises[0];
    assert!(rise.rise_db() >= 9.0, "rise {} dB", rise.rise_db());
    assert!(rise.spearman() >= 0.9, "rho {}", rise.spearman());
    // without it there is nothing in the band to rise: the kick and bass leave only numeric
    // dust above 4 kHz; every beat sits 50 dB under where the riser ends (about -18 dB)
    let off = render12(&p, Fixes::EMPTY);
    let flat = RiserRise::measure(&off.0, &off.1, SR, &p);
    let floor = rise.beats_db.last().unwrap() - 50.0;
    assert_eq!(flat.len(), 1);
    assert!(
        flat[0].beats_db.iter().all(|b| *b < floor),
        "without the riser: {:?} dB, the riser ends at {} dB",
        flat[0].beats_db,
        floor + 50.0
    );
}

/// RiserTests.silentBeforeItsWindowAndDeterministic: before the first window the render is the
/// bytes it was without the riser; two renders are identical.
#[test]
fn silent_before_its_window_and_deterministic() {
    let p = riser_project();
    let on = render12(&p, Fixes::RISER);
    let off = render12(&p, Fixes::EMPTY);
    let before = 32 * STEP;
    assert!(identical(&on.0[..before], &off.0[..before]));
    assert!(identical(&on.1[..before], &off.1[..before]));
    let again = render12(&p, Fixes::RISER);
    assert!(again.0 == on.0 && again.1 == on.1);
    assert!(!identical(&on.0, &off.0));
}

/// RiserTests.cutsAtTheDrop: it climbs to -12 dB and cuts at the drop: nothing of it a few ms
/// into the drop.
#[test]
fn it_cuts_at_the_drop() {
    let p = riser_project();
    let only = minus(&render12(&p, Fixes::RISER).0, &render12(&p, Fixes::EMPTY).0);
    let drop = 64 * STEP;
    let last = rms(&only, drop - 4 * STEP, drop);
    assert!((last - RiserSpec::TO_DB).abs() < 2.5, "last beat {last} dB");
    let first = rms(&only, 32 * STEP + STEP / 2, 36 * STEP);
    assert!((first - (-27.75)).abs() < 3.0, "first beat {first} dB");
    assert!(rms(&only, drop + (0.03 * SR) as usize, drop + 8 * STEP) < -100.0);
}

/// RiserTests.aJumpStartsInPhase: a start inside the window (a section jump) plays the riser
/// where the song has it.
#[test]
fn a_jump_starts_in_phase() {
    let p = riser_project();
    let full = minus(&render12(&p, Fixes::RISER).0, &render12(&p, Fixes::EMPTY).0);
    let late = minus(
        &render(&p, Fixes::RISER, 48, 4.0).0,
        &render(&p, Fixes::EMPTY, 48, 4.0).0,
    );
    for beat in 0..4 {
        let a = beat * 4 * STEP + STEP / 2;
        let b = (beat + 1) * 4 * STEP;
        let want = rms(&full, 48 * STEP + a, 48 * STEP + b);
        assert!((rms(&late, a, b) - want).abs() < 1.5, "beat {beat}");
    }
    // a start on the drop plays none of it
    let at_drop = minus(
        &render(&p, Fixes::RISER, 64, 2.0).0,
        &render(&p, Fixes::EMPTY, 64, 2.0).0,
    );
    assert!(rms(&at_drop, 0, at_drop.len()) < -100.0);
}

/// RiserTests.widensUnderStereo: `.stereo` gives it a side that widens through the window;
/// without, it is mono.
#[test]
fn it_widens_under_stereo() {
    let p = riser_project();
    let mono = render12(&p, Fixes::RISER);
    assert!(identical(&mono.0, &mono.1));
    let wide = render12(&p, Fixes::RISER | Fixes::STEREO);
    let plain = render12(&p, Fixes::STEREO);
    let side: Vec<f32> = minus(&wide.0, &plain.0)
        .iter()
        .zip(minus(&wide.1, &plain.1))
        .map(|(l, r)| (l - r) / 2.0)
        .collect();
    let early = rms(&side, 32 * STEP + STEP / 2, 40 * STEP);
    let late = rms(&side, 56 * STEP, 64 * STEP);
    assert!(
        early > -80.0 && late > early + 9.0,
        "side {early} -> {late} dB"
    );
}

/// RiserTests.sendsToTheRoom: `.space` sends it to the room: a tail rings on into the drop.
#[test]
fn it_sends_to_the_room() {
    let p = riser_project();
    let dry = minus(&render12(&p, Fixes::RISER).0, &render12(&p, Fixes::EMPTY).0);
    let wet = minus(
        &render12(&p, Fixes::RISER | Fixes::SPACE).0,
        &render12(&p, Fixes::SPACE).0,
    );
    let drop = 64 * STEP;
    let a = drop + (0.03 * SR) as usize;
    let b = drop + (0.3 * SR) as usize;
    assert!(rms(&dry, a, b) < -100.0);
    assert!(rms(&wet, a, b) > -60.0, "room tail {} dB", rms(&wet, a, b));
}

/// A stop and a hand on the platter silence it: the riser is keyed by the song step the clock is
/// playing, so it plays nothing stopped or scrubbed (`riserStep`). Run the same gestures with
/// and without it: inside the window the two differ, a moment after the hand lands or the deck
/// stops they are the same bits.
#[test]
fn a_stop_and_a_hand_silence_it() {
    let p = riser_project();
    let run = |fixes: Fixes| -> Vec<f32> {
        let o = options(fixes);
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare(&p, SR, &o, 0));
        r.play();
        let mut out = Vec::new();
        let mut buf = vec![0f32; 256];
        let mut blocks = |r: &mut WaveRunner, n: usize, out: &mut Vec<f32>| {
            for _ in 0..n {
                r.process(&mut buf, None, 256);
                out.extend_from_slice(&buf);
            }
        };
        // into the window (steps 32..64 = 4..8 s)
        blocks(&mut r, (5.0 * SR) as usize / 256, &mut out);
        r.freeze();
        blocks(&mut r, 300, &mut out);
        r.release();
        blocks(&mut r, 100, &mut out);
        r.stop();
        blocks(&mut r, 200, &mut out);
        out
    };
    let (on, off) = (run(Fixes::RISER), run(Fixes::EMPTY));
    let window = (5.0 * SR) as usize / 256 * 256;
    assert!(
        !identical(&on[..window], &off[..window]),
        "it sounds inside the window"
    );
    // 50 ms after the hand lands (and the riser's 3 ms glide has run out): the same bits
    let held = window + 256 * 20..window + 256 * 300;
    assert!(identical(&on[held.clone()], &off[held]), "a hand holds it");
    // and after a stop, once the notes' last 100 blocks have gone by
    let stopped = window + 256 * 400 + 256 * 20..on.len();
    assert!(
        identical(&on[stopped.clone()], &off[stopped]),
        "a stop ends it"
    );
}
