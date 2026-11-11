//! The room, its shimmer and the echo (`.space`, `.shimmer`, `.echo`) and the FX unit's gains on
//! them (`WaveRunner.setUnitSends`): the Swift suites' own tests ported (PORTING.md's proof, form
//! (a)), each named for the test it comes from (`SpaceTests`, `EchoTests`, `RoomShimmerTests`).
//! The same sends held against Swift's output sample for sample are the "sends" cases of
//! `tests/runner_golden.rs`.
//!
//! The Swift renders measure stereo (`.stereo`: left = mid + side). The tests up to the
//! "stereo halves" below read the mid, which is what the Swift tests' own `mid(...)` reads; the
//! side assertions (ping-pong alternation, a wide return, nothing changing on either channel)
//! are the last ones, now that the runner plays `.stereo`.

use djrust::musicality::echo::EchoSpec;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::space::SpaceSpec;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;

fn max_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

const SR: f64 = 48_000.0;

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(64);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

/// One short pad chord (or notes) on the first steps of an 8-second ring, played by `group`.
fn chord_once(group: &str, notes: &[i64]) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let notes: Vec<(i64, i64)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (i as i64, *n))
        .collect();
    p.nets.insert(group, ring(group, 7, &notes, 100));
    p
}

fn pad_once() -> Project {
    chord_once("harmony", &[60, 64, 67])
}

fn kick_and_bass() -> Project {
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
    p
}

fn lead_once(tempo: f64, group: &str) -> Project {
    let mut p = Project {
        tempo,
        ..Default::default()
    };
    let mut n = NetBundle::ring(64);
    n.bind(0, 72, 4, 110, 0);
    n.bindings.get_mut("t0").unwrap().duration_steps = 1;
    n.track.channel = 4;
    n.track.group = Some(group.into());
    p.nets.insert(group, n);
    p
}

fn options(fixes: Fixes, space: Option<SpaceSpec>, echo: Option<EchoSpec>) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        space,
        echo,
        ..Default::default()
    }
}

fn render(p: &Project, o: &WaveOptions, seconds: f64) -> Vec<f32> {
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, o, 0));
    r.play();
    let mut l = vec![0f32; (seconds * SR) as usize];
    let mut off = 0;
    while off < l.len() {
        let n = 256.min(l.len() - off);
        r.process(&mut l[off..off + n], None, n);
        off += n;
    }
    l
}

fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let s: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * (s / x.len() as f64).max(1e-20).log10()
}

fn secs(s: f64) -> usize {
    (s * SR) as usize
}

const SPACE: Fixes = Fixes::SPACE;
fn room() -> SpaceSpec {
    SpaceSpec::new(2.0, 0.4, 7000.0)
}

// SpaceTests.aPadLeavesATailThatDecaysAtTheRoomsRate
#[test]
fn a_pad_leaves_a_tail_that_decays_at_the_rooms_rate() {
    let dry = render(&pad_once(), &options(Fixes::EMPTY, None, None), 4.0);
    let wet = render(&pad_once(), &options(SPACE, Some(room()), None), 4.0);
    let gone = rms_db(&dry[secs(1.2)..secs(1.4)]);
    assert!(gone < -120.0);
    let early = rms_db(&wet[secs(1.2)..secs(1.4)]);
    let late = rms_db(&wet[secs(2.2)..secs(2.4)]);
    assert!(
        early > -90.0 && early > gone + 40.0,
        "tail at 1.2 s: {early} dB"
    );
    let fall = early - late;
    assert!(
        fall > 20.0 && fall < 40.0,
        "fell {fall} dB in a second (a 2 s room falls 30)"
    );
}

// SpaceTests.kickAndBassStayDry
#[test]
fn kick_and_bass_stay_dry() {
    let a = render(&kick_and_bass(), &options(Fixes::EMPTY, None, None), 4.0);
    let b = render(
        &kick_and_bass(),
        &options(SPACE, Some(SpaceSpec::for_genre("techno", false)), None),
        4.0,
    );
    assert!(a == b);
}

// SpaceTests.roomsByGenre
#[test]
fn rooms_by_genre() {
    let sec = |g: &str| SpaceSpec::for_genre(g, false).seconds;
    assert!(sec("ambient") > sec("trance"));
    assert!(sec("trance") > sec("techno"));
    assert!(sec("techno") > sec("funk"));
    assert!(
        SpaceSpec::for_genre("lofi", false).damping < SpaceSpec::for_genre("trance", false).damping
    );
    assert_eq!(SpaceSpec::send("kick", "drums"), 0.0);
    assert_eq!(SpaceSpec::send("tonal", "bass"), 0.0);
    assert!(SpaceSpec::send("tonal", "harmony") > SpaceSpec::send("snare", "drums"));
    // the project plays the room it was prepared with; none without `.space`
    let o = options(SPACE, Some(SpaceSpec::for_genre("trance", true)), None);
    let prepared = WaveRunner::prepare(&pad_once(), 22_050.0, &o, 0);
    assert_eq!(prepared.space, Some(SpaceSpec::for_genre("trance", true)));
    let plain = WaveRunner::prepare(&pad_once(), 22_050.0, &WaveOptions::default(), 0);
    assert!(plain.space.is_none());
    // `.space` with no room given plays the neutral one
    let o = options(SPACE, None, None);
    assert_eq!(
        WaveRunner::prepare(&pad_once(), 22_050.0, &o, 0).space,
        Some(SpaceSpec::room())
    );
}

// RoomShimmerTests.theReturnIsLinearInWet
#[test]
fn the_return_is_linear_in_wet() {
    let dry = render(&pad_once(), &options(Fixes::EMPTY, None, None), 4.0);
    let ret = |wet: f64| {
        let w = render(
            &pad_once(),
            &options(SPACE, Some(SpaceSpec::new(2.0, wet, 7000.0)), None),
            4.0,
        );
        let (mut num, mut den) = (0.0f64, 0.0f64);
        for i in 0..w.len() {
            let d = (w[i] - dry[i]) as f64;
            num += d * d;
            den += dry[i] as f64 * dry[i] as f64;
        }
        10.0 * (num / den).log10()
    };
    let (a, b) = (ret(0.2), ret(0.4));
    let amp_db_2 = 20.0 * 2f64.log10();
    assert!(((b - a) - amp_db_2).abs() < 0.2, "return {a} -> {b} dB");
}

// RoomShimmerTests.theShimmerIsPadOnly
#[test]
fn the_shimmer_is_pad_only() {
    let mut sp = SpaceSpec::new(2.0, 0.4, 7000.0);
    sp.shimmer = 0.3;
    let with = |p: &Project, fixes| render(p, &options(fixes, Some(sp), None), 4.0);
    let lead = chord_once("melody", &[60, 64, 67]);
    assert!(with(&lead, SPACE) == with(&lead, SPACE | Fixes::SHIMMER));
    let pad = pad_once();
    assert!(with(&pad, SPACE) != with(&pad, SPACE | Fixes::SHIMMER));
    // and the room's lanes send what they did: the lead's prepared sends are unchanged
    let o = options(SPACE | Fixes::SHIMMER, Some(sp), None);
    let prepared = WaveRunner::prepare(&lead, SR, &o, 0);
    assert!(prepared.lanes().iter().all(|l| l.shimmer_send() == 0.0));
    let prepared = WaveRunner::prepare(&pad, SR, &o, 0);
    assert!(prepared
        .lanes()
        .iter()
        .any(|l| l.shimmer_send() > 0.0 && l.shimmer_send() == l.send()));
}

// RoomShimmerTests.aZeroShimmerIsBitIdentical
#[test]
fn a_zero_shimmer_is_bit_identical() {
    let o = |fixes| options(fixes, Some(room()), None);
    let a = render(&pad_once(), &o(SPACE), 4.0);
    let b = render(&pad_once(), &o(SPACE | Fixes::SHIMMER), 4.0);
    assert!(a == b);
}

fn at(x: &[f32], t: f64) -> f64 {
    let a = secs(t);
    rms_db(&x[a..a + secs(0.06)])
}

fn trance_echo() -> EchoSpec {
    EchoSpec {
        steps: 3.0,
        feedback: 0.42,
        wet: 0.32,
        damping: 5000.0,
    }
}

// EchoTests.repeatsPingPongOnTheBeatGrid (the mid; the L/R alternation is below)
#[test]
fn repeats_come_back_on_the_beat_grid() {
    let spec = trance_echo();
    let dry = render(
        &lead_once(120.0, "melody"),
        &options(Fixes::EMPTY, None, None),
        2.0,
    );
    let full = render(
        &lead_once(120.0, "melody"),
        &options(Fixes::ECHO, None, Some(spec)),
        2.0,
    );
    // the echo alone: what the fix adds to the dry render
    let wet: Vec<f32> = full.iter().zip(&dry).map(|(a, b)| a - b).collect();
    assert!(at(&wet, 0.1) < -120.0, "nothing before the first repeat");
    assert!(
        at(&dry, 0.375) < at(&wet, 0.375) - 20.0,
        "the dry note has died by the first repeat"
    );
    let (first, second, third) = (at(&wet, 0.375), at(&wet, 0.75), at(&wet, 1.125));
    assert!(first > -60.0, "first repeat {first}");
    assert!(second < first - 3.0 && third < second - 3.0, "repeats fall");
    // between the repeats, quiet (a 60 ms window halfway between)
    assert!(at(&wet, 0.55) < first - 20.0);
}

// EchoTests.theDelayFollowsTheTempo
#[test]
fn the_delay_follows_the_tempo() {
    let spec = trance_echo();
    let dry = render(
        &lead_once(140.0, "melody"),
        &options(Fixes::EMPTY, None, None),
        2.0,
    );
    let full = render(
        &lead_once(140.0, "melody"),
        &options(Fixes::ECHO, None, Some(spec)),
        2.0,
    );
    let wet: Vec<f32> = full.iter().zip(&dry).map(|(a, b)| a - b).collect();
    let t = 3.0 * 60.0 / (140.0 * 4.0);
    assert!(
        at(&wet, t) > -60.0 && at(&wet, t - 0.07) < -120.0,
        "the repeat lands at {t} s"
    );
}

// EchoTests.onlyLeadsEcho
#[test]
fn only_leads_echo() {
    let spec = EchoSpec::for_genre("trance").unwrap();
    for group in ["bass", "harmony"] {
        let p = lead_once(120.0, group);
        let a = render(&p, &options(Fixes::EMPTY, None, None), 2.0);
        let b = render(&p, &options(Fixes::ECHO, None, Some(spec)), 2.0);
        assert!(a == b, "{group}");
    }
    assert!(EchoSpec::for_genre("jazz").is_none() && EchoSpec::for_genre("techno").is_none());
    assert!(
        EchoSpec::for_genre("ambient").unwrap().steps
            > EchoSpec::for_genre("trance").unwrap().steps
    );
    // the project plays the echo it was prepared with; none without `.echo`
    let o = options(Fixes::ECHO, None, Some(spec));
    let p = WaveRunner::prepare(&lead_once(120.0, "melody"), SR, &o, 0);
    assert_eq!(p.echo, Some(spec));
    let o = options(Fixes::ECHO, None, None);
    assert!(WaveRunner::prepare(&lead_once(120.0, "melody"), SR, &o, 0)
        .echo
        .is_none());
    let o = options(Fixes::EMPTY, None, Some(spec));
    assert!(WaveRunner::prepare(&lead_once(120.0, "melody"), SR, &o, 0)
        .echo
        .is_none());
}

/// The FX unit's gains: at 0 the room and the echo return nothing (the render is the dry
/// one, bit for bit); at 1 (their start) it is the render without the call. Golden: the
/// "sends-unit" case of `tests/runner_golden.rs` holds the ramps against Swift.
#[test]
fn the_units_sends_scale_what_the_lanes_send() {
    let pad = pad_once();
    let o = options(SPACE | Fixes::ECHO, Some(room()), Some(trance_echo()));
    let play = |sends: Option<(f64, f64, f64)>| {
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare(&pad, SR, &o, 0));
        r.play();
        if let Some((room, echo, fb)) = sends {
            r.set_unit_sends(room, echo, fb, 1);
        }
        let mut l = vec![0f32; secs(3.0)];
        let n = l.len();
        r.process(&mut l, None, n);
        l
    };
    let dry = render(&pad, &options(Fixes::EMPTY, None, None), 3.0);
    assert!(play(Some((0.0, 0.0, 0.0))) == dry, "closed sends: dry");
    assert!(
        play(Some((1.0, 1.0, 0.0))) == play(None),
        "unity sends: as sent"
    );
    let (open, plain) = (play(Some((2.0, 1.0, 0.0))), play(None));
    let ret = |x: &[f32]| {
        rms_db(
            &x[secs(1.2)..secs(1.4)]
                .iter()
                .zip(&dry[secs(1.2)..secs(1.4)])
                .map(|(a, b)| a - b)
                .collect::<Vec<_>>(),
        )
    };
    assert!(
        (ret(&open) - ret(&plain) - 20.0 * 2f64.log10()).abs() < 0.2,
        "twice the send: 6 dB more room"
    );
    // without `.space` / `.echo` there is nothing to send to
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.set_unit_sends(2.0, 2.0, 1.0, 100);
}

/// `silence` clears the room (and the shimmer): the tail stops where it is.
#[test]
fn silence_clears_the_room() {
    let mut sp = room();
    sp.shimmer = 0.3;
    let o = options(SPACE | Fixes::SHIMMER, Some(sp), None);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&pad_once(), SR, &o, 0));
    r.play();
    let mut l = vec![0f32; secs(1.0)];
    let n = l.len();
    r.process(&mut l, None, n);
    let mut tail = vec![0f32; 4800];
    r.process(&mut tail, None, 4800);
    assert!(rms_db(&tail) > -90.0, "the room rings");
    r.silence();
    r.process(&mut tail, None, 4800);
    assert!(rms_db(&tail) < -120.0, "and does not after silence()");
}

/// The sampled instruments' sends reach the room and the echo a block late, through the
/// runner's fifos, scaled by the master gain; a full fifo drops what does not fit.
#[test]
fn sampled_sends_reach_the_room_a_block_late() {
    let o = options(SPACE | Fixes::ECHO, Some(room()), Some(trance_echo()));
    let silent = Project::default();
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&silent, SR, &o, 0));
    r.play();
    let mut blk = vec![0f32; 256];
    r.process(&mut blk, None, 256);
    assert_eq!(max_abs(&blk), 0.0);
    let mut burst = vec![0f32; 256];
    burst[0] = 1.0;
    r.queue_sampler_send(&burst);
    let mut got = vec![0f32; 4800];
    r.process(&mut got, None, 4800);
    assert!(
        rms_db(&got) > -100.0,
        "the room takes it during the next block"
    );
    // the echo's fifo too
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&silent, SR, &o, 0));
    r.play();
    r.queue_sampler_echo(&burst);
    let mut got = vec![0f32; secs(0.5)];
    let n = got.len();
    r.process(&mut got, None, n);
    assert!(
        max_abs(&got[secs(0.3)..]) > 0.0,
        "and the echo's repeat comes back"
    );
    // overfull: no panic, nothing past the capacity
    r.queue_sampler_send(&vec![0.1f32; 20_000]);
    r.queue_sampler_echo(&vec![0.1f32; 20_000]);
}

/// The sampled instruments join the voices at the track gain alone, so their sends are lifted
/// by the master gain the voices go through: `queueSamplerSend` queues `samplerSend / max(0.05,
/// masterGain)`, `queueSamplerEcho` `1 / max(0.05, masterGain)`. The returns are linear in what
/// is queued: half the master gain doubles the response, and below 0.05 the lift stops rising.
/// Form (c): the factor is read from the Swift source (its tests need the AUSampler bank).
#[test]
fn sampled_sends_are_lifted_by_the_master_gain() {
    let silent = Project::default();
    let response = |gain: f64, echo: bool| {
        let o = WaveOptions {
            master_gain: gain,
            ..options(SPACE | Fixes::ECHO, Some(room()), Some(trance_echo()))
        };
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare(&silent, SR, &o, 0));
        r.play();
        let mut burst = vec![0f32; 256];
        burst[0] = 1.0;
        if echo {
            r.queue_sampler_echo(&burst);
        } else {
            r.queue_sampler_send(&burst);
        }
        let mut out = vec![0f32; secs(0.6)];
        let n = out.len();
        r.process(&mut out, None, n);
        out
    };
    for echo in [false, true] {
        let unity = response(1.0, echo);
        assert!(max_abs(&unity) > 0.0, "echo {echo}: the send is heard");
        let ratio = |gain: f64| max_abs(&response(gain, echo)) as f64 / max_abs(&unity) as f64;
        assert!(
            (ratio(0.5) - 2.0).abs() < 1e-3,
            "echo {echo}: {}",
            ratio(0.5)
        );
        assert!(
            (ratio(0.05) - 20.0).abs() < 1e-2,
            "echo {echo}: {}",
            ratio(0.05)
        );
        assert!(
            response(0.01, echo) == response(0.05, echo),
            "echo {echo}: the lift stops at a master gain of 0.05"
        );
    }
}

/// An echo that is off takes nothing from the sampled leads' fifo: what waits in it is dropped
/// (`if !echoOn { echoCount = 0 }` at the top of `process`), so the next song with an echo does
/// not start with a stale repeat. Form (c): read from the Swift source.
#[test]
fn an_echo_that_is_off_drops_what_waits_in_its_fifo() {
    let on = options(Fixes::ECHO, None, Some(trance_echo()));
    let off = options(Fixes::ECHO, None, None);
    let silent = Project::default();
    let mut r = WaveRunner::new(SR, on.clone());
    r.load_prepared(WaveRunner::prepare(&silent, SR, &off, 0));
    r.play();
    let mut burst = vec![0f32; 256];
    burst[0] = 1.0;
    r.queue_sampler_echo(&burst);
    run_seconds(&mut r, 0.1); // the echo is off: the block drops what waited
    r.stop();
    r.load_prepared(WaveRunner::prepare(&silent, SR, &on, 0));
    r.play();
    let out = run_seconds(&mut r, 1.0);
    assert_eq!(max_abs(&out), 0.0, "no stale repeat from the dry song");
}

// ---- the room and the echo against stops and the hand (FixesUnderGesturesTests) ------

/// `n` seconds of the runner in 240-frame blocks (0.9 s is a whole number of them).
fn run_seconds(r: &mut WaveRunner, seconds: f64) -> Vec<f32> {
    let mut out = Vec::new();
    let mut l = vec![0f32; 240];
    for _ in 0..secs(seconds) / 240 {
        r.process(&mut l, None, 240);
        out.extend_from_slice(&l);
    }
    out
}

// FixesUnderGesturesTests.anEchoTurnedOffDiesAwayBeforeTheNextEchoSong
#[test]
fn an_echo_turned_off_dies_away_before_the_next_echo_song() {
    let on = options(Fixes::ECHO, None, Some(trance_echo()));
    let off = options(Fixes::ECHO, None, None);
    // a silent ring: the dry songs play no note
    let mut silent = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut ring = NetBundle::ring(64);
    ring.track.channel = 4;
    ring.track.group = Some("melody".into());
    silent.nets.insert("melody", ring);
    let mut r = WaveRunner::new(SR, on.clone());
    r.load_prepared(WaveRunner::prepare(&lead_once(120.0, "melody"), SR, &on, 0));
    r.play();
    let first = run_seconds(&mut r, 0.5); // the note and its first repeat
    assert!(rms_db(&first) > -60.0, "the echo song plays");
    r.stop();
    r.load_prepared(WaveRunner::prepare(&silent, SR, &off, 0));
    r.play();
    run_seconds(&mut r, 8.0); // a dry song: 21 repeat times
    r.stop();
    r.load_prepared(WaveRunner::prepare(&silent, SR, &on, 0));
    r.play();
    let next = run_seconds(&mut r, 2.0);
    assert!(
        rms_db(&next) < -110.0,
        "stale repeats: {} dB",
        rms_db(&next)
    );
}

/// A stop lets the room ring out, as a deck's room does (no click to silence), and `silence`
/// (a stop under the brake) cuts it; a still hand on the platter holds the net clock, fires
/// nothing and lets the tails die away under it, and let go the deck plays on
/// (FixesUnderGesturesTests.aStopWithoutTheBrakeLetsTheRoomRingOut, .aStillHandLetsTheTailsRingOut).
#[test]
fn a_stop_and_a_still_hand_let_the_room_ring_out() {
    let o = options(SPACE, Some(room()), None);
    let play = || {
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare(&pad_once(), SR, &o, 0));
        r.play();
        r
    };
    // a stop
    let mut r = play();
    run_seconds(&mut r, 0.45);
    r.stop();
    let out = run_seconds(&mut r, 3.6);
    let early = rms_db(&out[secs(0.3)..secs(0.5)]);
    let late = rms_db(&out[secs(3.0)..secs(3.2)]);
    assert!(early > -80.0, "the room rings on: {early} dB");
    assert!(late < early - 10.0, "and dies away: {early} -> {late} dB");
    let mut r = play();
    run_seconds(&mut r, 0.45);
    r.stop();
    r.silence();
    let out = run_seconds(&mut r, 0.9);
    assert!(
        max_abs(&out) < 1e-5,
        "silence() leaves nothing: {}",
        max_abs(&out)
    );

    // a still hand
    let mut r = play();
    run_seconds(&mut r, 0.45);
    r.freeze();
    run_seconds(&mut r, 0.45);
    let held = r.graph().unwrap().tick as f64 + r.tick_phase();
    r.record_events = true;
    let out = run_seconds(&mut r, 2.25);
    let held_end = r.graph().unwrap().tick as f64 + r.tick_phase();
    assert_eq!(held, held_end, "the clock held");
    assert!(
        r.fires.is_empty() && r.anchors.is_empty(),
        "and fired nothing"
    );
    let (early, late) = (
        rms_db(&out[..secs(0.1)]),
        rms_db(&out[secs(2.0)..secs(2.2)]),
    );
    assert!(
        late < early,
        "the tails die away under the hand: {early} -> {late} dB"
    );
    r.release();
    run_seconds(&mut r, 0.45);
    let after = r.graph().unwrap().tick as f64 + r.tick_phase();
    assert!(after > held_end, "let go, the deck plays on");
}

// ---- the stereo halves --------------------------------------------------------------------

mod common;
use common::measure::render_stereo;

fn stereo(
    p: &Project,
    fixes: Fixes,
    space: Option<SpaceSpec>,
    echo: Option<EchoSpec>,
    secs: f64,
) -> (Vec<f32>, Vec<f32>) {
    render_stereo(p, &options(fixes, space, echo), SR, secs)
}

fn mid(x: &(Vec<f32>, Vec<f32>)) -> Vec<f32> {
    x.0.iter().zip(&x.1).map(|(l, r)| (l + r) / 2.0).collect()
}

fn side(x: &(Vec<f32>, Vec<f32>)) -> Vec<f32> {
    x.0.iter().zip(&x.1).map(|(l, r)| (l - r) / 2.0).collect()
}

/// A two-pole lowpass at `hz`, twice (fourth order).
fn lowpass(x: &[f32], hz: f64) -> Vec<f32> {
    let mut y: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    for _ in 0..2 {
        let b = djrust::wave::biquad("lowpass", hz, djrust::wave::synth::BUTTERWORTH_Q, SR);
        let (mut z1, mut z2) = (0.0, 0.0);
        for v in y.iter_mut() {
            let o = b[0] * *v + z1;
            z1 = b[1] * *v - b[3] * o + z2;
            z2 = b[2] * *v - b[4] * o;
            *v = o;
        }
    }
    y.into_iter().map(|v| v as f32).collect()
}

// SpaceTests.theReturnIsWideAboveAndMonoBelow
#[test]
fn the_return_is_wide_above_and_mono_below() {
    let tail = secs(1.2)..secs(2.0);
    let st = Fixes::STEREO | SPACE;
    let wet = stereo(&pad_once(), st, Some(room()), None, 4.0);
    let width = rms_db(&side(&wet)[tail.clone()]) - rms_db(&mid(&wet)[tail.clone()]);
    assert!(width > -8.0, "tail side/mid {width} dB");
    let low = stereo(&chord_once("harmony", &[36]), st, Some(room()), None, 4.0);
    let m = lowpass(&mid(&low), 100.0);
    let low_width = rms_db(&lowpass(&side(&low), 100.0)[tail.clone()]) - rms_db(&m[tail]);
    assert!(
        low_width < -25.0,
        "a C2 tail's side/mid below 100 Hz: {low_width} dB"
    );
}

// SpaceTests.kickAndBassStayDry (both channels)
#[test]
fn kick_and_bass_stay_dry_in_both_channels() {
    let a = stereo(&kick_and_bass(), Fixes::STEREO, None, None, 4.0);
    let b = stereo(
        &kick_and_bass(),
        Fixes::STEREO | SPACE,
        Some(SpaceSpec::for_genre("techno", false)),
        None,
        4.0,
    );
    assert_eq!(bits(&a.0), bits(&b.0));
    assert_eq!(bits(&a.1), bits(&b.1));
}

fn bits(x: &[f32]) -> Vec<u32> {
    x.iter().map(|v| v.to_bits()).collect()
}

// EchoTests.repeatsPingPongOnTheBeatGrid: the first repeat comes back on the left, the second
// on the right, the third on the left, each quieter than the last
#[test]
fn repeats_ping_pong_between_the_channels() {
    let spec = trance_echo();
    let lead = lead_once(120.0, "melody");
    let dry = stereo(&lead, Fixes::STEREO, None, Some(spec), 2.0);
    let full = stereo(&lead, Fixes::STEREO | Fixes::ECHO, None, Some(spec), 2.0);
    let sub = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x - y).collect() };
    let wet = (sub(&full.0, &dry.0), sub(&full.1, &dry.1));
    assert!(at(&wet.0, 0.1) < -120.0, "nothing before the first repeat");
    assert!(
        at(&dry.0, 0.375) < at(&wet.0, 0.375) - 20.0,
        "the dry note has died by the first repeat"
    );
    let first = (at(&wet.0, 0.375), at(&wet.1, 0.375));
    let second = (at(&wet.0, 0.75), at(&wet.1, 0.75));
    let third = (at(&wet.0, 1.125), at(&wet.1, 1.125));
    assert!(
        first.0 > -60.0 && first.0 > first.1 + 10.0,
        "first repeat L {} R {}",
        first.0,
        first.1
    );
    assert!(
        second.1 > second.0 + 10.0,
        "second repeat L {} R {}",
        second.0,
        second.1
    );
    assert!(
        third.0 > third.1 + 10.0,
        "third repeat L {} R {}",
        third.0,
        third.1
    );
    assert!(
        second.1 < first.0 - 3.0 && third.0 < second.1 - 3.0,
        "repeats fall"
    );
    // between the repeats, quiet (a 60 ms window halfway between)
    assert!(at(&wet.0, 0.55) < first.0 - 20.0);
}

// EchoTests.onlyLeadsEcho (both channels)
#[test]
fn only_leads_echo_in_both_channels() {
    let spec = EchoSpec::for_genre("trance").unwrap();
    for group in ["bass", "harmony"] {
        let lead = lead_once(120.0, group);
        let a = stereo(&lead, Fixes::STEREO, None, Some(spec), 2.0);
        let b = stereo(&lead, Fixes::STEREO | Fixes::ECHO, None, Some(spec), 2.0);
        assert_eq!(bits(&a.0), bits(&b.0), "{group}");
        assert_eq!(bits(&a.1), bits(&b.1), "{group}");
    }
}

// RoomShimmerTests.theShimmerIsPadOnly (both channels)
#[test]
fn the_shimmer_is_pad_only_in_both_channels() {
    let mut r = SpaceSpec::new(2.0, 0.4, 7000.0);
    r.shimmer = 0.3;
    let lead = chord_once("melody", &[60, 64, 67]);
    let st = Fixes::STEREO | SPACE;
    let a = stereo(&lead, st, Some(r), None, 4.0);
    let b = stereo(&lead, st | Fixes::SHIMMER, Some(r), None, 4.0);
    assert_eq!(bits(&a.0), bits(&b.0));
    assert_eq!(bits(&a.1), bits(&b.1));
    let pad = pad_once();
    let c = stereo(&pad, st, Some(r), None, 4.0);
    let d = stereo(&pad, st | Fixes::SHIMMER, Some(r), None, 4.0);
    assert_ne!(bits(&c.0), bits(&d.0));
}
