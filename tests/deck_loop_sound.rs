//! How the loops under the mixer sound (`LoopSoundTests.swift`): the bank's fades and ramps on
//! the audio thread, mute and solo as gains, the row's fade and a clip's own, and key lock (a
//! stretched copy swapped in in phase). Each test names the Swift test it ports ("(a)" in
//! PORTING.md's parity rule). The pure halves (WSOLA, the key-lock decision) are in
//! `djengine/tests/loop_sound.rs`.
//!
//! Not here: a crop carrying the settings (crops are another slice; `carry_clip_sound` and
//! `forget_clip_sound` are tested directly), the poke round trips (`DeckAction` glue), and the
//! recorded session's start (`SessionRecorder`).

mod common_loops;

use std::sync::Arc;

use common_loops::SR;
use djrust::deck::loop_sound::LoopFade;
use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::TrackSpec;
use djrust::host::deck::loops::{LoopBank, LoopMsg, LoopState};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

// MARK: the bank: fades and ramps on the audio thread

/// A loop bank with one voice holding a constant 1.0 (so its output is its gain), playing
/// (`dcBank`).
fn dc_bank(fade: i64, at: i64) -> LoopBank {
    let (mut bank, _) = LoopBank::new(SR);
    let b = Arc::new(SampleBuffer::new(vec![1.0; 4800], None, SR));
    bank.apply(LoopMsg::Voice(0, TurntableCommand::Load(b)));
    bank.apply(LoopMsg::Voice(
        0,
        TurntableCommand::Loop {
            start: 0.0,
            end: 4800.0,
        },
    ));
    bank.apply(LoopMsg::Voice(0, TurntableCommand::Start));
    bank.apply(LoopMsg::Gain(0, 1.0));
    bank.apply(LoopMsg::Start {
        v: 0,
        frame: at,
        fade,
    });
    bank
}

/// Render `frames` of `bank` from frame `start`: the left channel.
fn render(bank: &mut LoopBank, frames: usize, start: i64) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
    bank.render_bank(start, &mut l, &mut r);
    l
}

// LoopSoundTests.aStartRampsUpFromZero
#[test]
fn a_start_ramps_up_from_zero() {
    let mut bank = dc_bank(480, 100);
    let out = render(&mut bank, 1024, 0);
    assert!(
        out[0..100].iter().all(|&v| v == 0.0),
        "silent before its frame"
    );
    assert!(out[100] == 0.0, "the first sample is at 0");
    for k in [1usize, 120, 240, 479] {
        assert!(
            (out[100 + k] - k as f32 / 480.0).abs() < 1e-4,
            "{k} in: {}",
            out[100 + k]
        );
    }
    assert!(
        out[100..580].windows(2).all(|w| w[0] <= w[1]),
        "rises, never steps back"
    );
    assert!(
        out[580..].iter().all(|&v| (v - 1.0).abs() < 1e-6),
        "then holds at its level"
    );
}

// LoopSoundTests.withTheFadeOffAStartIsAtOnce
#[test]
fn with_the_fade_off_a_start_is_at_once() {
    let out = render(&mut dc_bank(0, 100), 512, 0);
    assert!(out[99] == 0.0 && out[100] == 1.0);
}

// LoopSoundTests.aFadedStopEndsAtZeroAfterTheFade
#[test]
fn a_faded_stop_ends_at_zero_after_the_fade() {
    let mut bank = dc_bank(0, 0);
    render(&mut bank, 512, 0);
    bank.apply(LoopMsg::Stop {
        v: 0,
        frame: 1000,
        fade: 2400,
    });
    let out = render(&mut bank, 4096, 512);
    let at = 1000 - 512;
    assert!(
        (out[at - 1] - 1.0).abs() < 1e-6 && (out[at] - 1.0).abs() < 1e-6,
        "full until the stop"
    );
    assert!(
        (out[at + 1200] - 0.5).abs() < 1e-3,
        "half way down half way through"
    );
    assert!(
        out[at..at + 2400]
            .iter()
            .zip(&out[at + 1..])
            .all(|(a, b)| a >= b),
        "falls only"
    );
    assert!(
        out[at + 2400..].iter().all(|&v| v == 0.0),
        "silent from the fade's end"
    );
    assert!(!bank.is_active(0), "and then the voice is off");
}

// LoopSoundTests.aStopWithoutAFadeCutsAndAFadingVoiceStaysOnUntilItEnds
#[test]
fn a_stop_without_a_fade_cuts_and_a_fading_voice_stays_on_until_it_ends() {
    let mut bank = dc_bank(0, 0);
    render(&mut bank, 512, 0);
    bank.apply(LoopMsg::Stop {
        v: 0,
        frame: 600,
        fade: 4000,
    });
    render(&mut bank, 512, 512);
    assert!(bank.is_active(0), "still fading");
    // started again while fading out: it comes back up instead of going
    bank.apply(LoopMsg::Start {
        v: 0,
        frame: 0,
        fade: 480,
    });
    let out = render(&mut bank, 2048, 1024);
    assert!(bank.is_active(0) && (out[out.len() - 1] - 1.0).abs() < 1e-6);
    bank.apply(LoopMsg::Stop {
        v: 0,
        frame: 3100,
        fade: 0,
    });
    let cut = render(&mut bank, 512, 3072);
    assert!(cut[27] == 1.0 && cut[28..].iter().all(|&v| v == 0.0) && !bank.is_active(0));
}

// LoopSoundTests.aLevelChangeRampsInsteadOfStepping
#[test]
fn a_level_change_ramps_instead_of_stepping() {
    let mut bank = dc_bank(0, 0);
    render(&mut bank, 512, 0);
    bank.apply(LoopMsg::Gain(0, 0.5));
    let out = render(&mut bank, 512, 512);
    let ramp = bank.ramp_frames() as usize;
    assert_eq!(ramp, 240, "5 ms");
    assert!(
        out[0] > 0.99 && (out[ramp / 2] - 0.75).abs() < 0.01 && (out[ramp + 10] - 0.5).abs() < 1e-6
    );
    let steps = out[0..ramp]
        .iter()
        .zip(&out[1..])
        .map(|(a, b)| (a - b).abs());
    let max = steps.fold(0f32, f32::max);
    assert!(max < 0.01, "no zipper step: {max}");
}

// A level change waiting for its frame (a scene's, LoopBankCommand.gainAt): the old level until
// then, the ramp from it; a plain gain cancels the wait
#[test]
fn a_level_change_can_wait_for_its_frame() {
    let mut bank = dc_bank(0, 0);
    render(&mut bank, 512, 0);
    bank.apply(LoopMsg::GainAt(0, 0.5, 700));
    let out = render(&mut bank, 512, 512);
    let at = 700 - 512;
    assert!(
        out[..at].iter().all(|&v| (v - 1.0).abs() < 1e-6),
        "the old level until its frame"
    );
    assert!((out[at + 120] - 0.75).abs() < 0.01 && (out[at + 250] - 0.5).abs() < 1e-6);
    // cancelled by a plain gain
    let mut bank = dc_bank(0, 0);
    render(&mut bank, 512, 0);
    bank.apply(LoopMsg::GainAt(0, 0.5, 5000));
    bank.apply(LoopMsg::Gain(0, 1.0));
    let out = render(&mut bank, 6000, 512);
    assert!(out.iter().all(|&v| (v - 1.0).abs() < 1e-6));
}

// MARK: the engine

const BLOCK: usize = 512;

fn blocks(e: &mut Engine, r: &mut Renderer, n: usize) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
    }
}

fn run_frames(e: &mut Engine, r: &mut Renderer, frames: i64) {
    blocks(e, r, ((frames as f64) / BLOCK as f64).ceil() as usize);
}

fn run_to(e: &mut Engine, r: &mut Renderer, frame: i64) {
    while e.loop_frames() < frame {
        blocks(e, r, 1);
    }
}

/// The loop channel's peak over a stretch of blocks.
fn loop_peak(e: &mut Engine, r: &mut Renderer, frames: i64) -> f32 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut peak = 0f32;
    let mut done = 0;
    while done < frames {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        peak = peak.max(e.loops.mirror.level());
        done += BLOCK as i64;
    }
    peak
}

struct Rig {
    e: Engine,
    r: Renderer,
    bpm: f64,
}

impl Rig {
    /// Deck A plays a techno net, only the loops on the master; one bar of it is taken and
    /// loops under the mixer (`SoundRig.oneTake`).
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (mut e, mut r) = engine(c);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        e.set_master(1.0);
        assert!(e.generate("A", &TrackSpec::new("techno", 1.0, None)));
        e.play("A");
        blocks(&mut e, &mut r, 4);
        let bpm = e.deck("A").unwrap().mirror.tempo();
        Rig { e, r, bpm }
    }

    fn bar(&self) -> i64 {
        (4.0 * 60.0 / self.bpm * SR).round() as i64
    }

    /// A one-bar take, in and sounding.
    fn take(&mut self) -> String {
        let bar = self.bar();
        let at = self.e.loop_frames();
        run_to(&mut self.e, &mut self.r, at + bar + 100);
        let count = self.e.loops.row.clips.len();
        assert!(self.e.loop_record("A", true, Some(1)));
        blocks(&mut self.e, &mut self.r, 2);
        self.e.loop_record("A", false, None);
        for _ in 0..4000 {
            if self.e.loops.row.clips.len() > count {
                break;
            }
            blocks(&mut self.e, &mut self.r, 1);
        }
        let id = self.e.loops.row.clips.last().expect("a take").id.clone();
        run_frames(&mut self.e, &mut self.r, 4096); // in
        id
    }

    /// How far through its bar clip `id`'s voice is, against deck A's bar: the error, 0...0.5.
    fn phase_error(&self, id: &str) -> f64 {
        let bars = self.e.clip(id).unwrap().bars() as f64;
        let phase = (self.e.loop_playhead(id).unwrap() * bars).rem_euclid(1.0);
        let a = self.e.beat_state("A").unwrap().bar_phase();
        let err = (phase - a).abs();
        err.min(1.0 - err)
    }
}

// LoopSoundTests.aMutedLoopIsSilentAndKeepsItsPlace
#[test]
fn a_muted_loop_is_silent_and_keeps_its_place() {
    let mut rig = Rig::new();
    let id = rig.take();
    let bar = rig.bar();
    assert!(loop_peak(&mut rig.e, &mut rig.r, 4096) > 0.001, "it plays");
    rig.e.set_clip_mute(&id, true);
    assert!(rig.e.is_clip_muted(&id) && rig.e.audible_clip_gain(&id) == 0.0);
    let before = rig.e.loops.mirror.playhead(0).0;
    run_frames(&mut rig.e, &mut rig.r, bar / 2);
    assert!(
        loop_peak(&mut rig.e, &mut rig.r, bar / 4) == 0.0,
        "a muted voice adds silence"
    );
    assert_eq!(
        rig.e.loop_state(&id),
        LoopState::On,
        "but it is still looping"
    );
    assert!(
        rig.e.loops.mirror.playhead(0).0 != before,
        "and its playhead moved on"
    );
    rig.e.set_clip_mute(&id, false);
    run_frames(&mut rig.e, &mut rig.r, bar / 4);
    assert!(loop_peak(&mut rig.e, &mut rig.r, 4096) > 0.001, "back");
    assert!(
        rig.phase_error(&id) < 0.01,
        "in time with deck A, as if it had never stopped"
    );
}

// LoopSoundTests.soloSilencesEveryLoopThatIsNotSoloed
#[test]
fn solo_silences_every_loop_that_is_not_soloed() {
    let mut rig = Rig::new();
    let one = rig.take();
    let two = rig.take();
    let bar = rig.bar();
    assert_ne!(one, two);
    rig.e.set_clip_solo(&two, true);
    assert!(rig.e.audible_clip_gain(&one) == 0.0 && rig.e.audible_clip_gain(&two) == 1.0);
    run_frames(&mut rig.e, &mut rig.r, bar / 4);
    assert!(
        loop_peak(&mut rig.e, &mut rig.r, 4096) > 0.001,
        "the soloed one plays"
    );
    // with the soloed one muted too, nothing is left: the other is soloed out
    rig.e.set_clip_mute(&two, true);
    run_frames(&mut rig.e, &mut rig.r, bar / 4);
    assert_eq!(loop_peak(&mut rig.e, &mut rig.r, 4096), 0.0);
    rig.e.set_clip_solo(&two, false);
    run_frames(&mut rig.e, &mut rig.r, bar / 4);
    // (two stays muted: the first is heard again)
    assert!(
        loop_peak(&mut rig.e, &mut rig.r, 4096) > 0.001,
        "solo off: the first is heard again"
    );
    assert_eq!(rig.e.loop_state(&one), LoopState::On);
    assert_eq!(rig.e.loop_state(&two), LoopState::On);
}

// LoopSoundTests.muteSoloAndKeyLockFollowACrop (carryClipSound / forgetClipSound; the crop itself
// is another slice)
#[test]
fn mute_solo_fade_and_key_lock_follow_a_clip_that_takes_anothers_place() {
    let mut rig = Rig::new();
    let one = rig.take();
    rig.e.set_clip_mute(&one, true);
    rig.e.set_clip_solo(&one, true);
    rig.e.set_clip_key_lock(&one, true);
    rig.e.set_clip_fade(&one, Some(LoopFade::Bar));
    rig.e.carry_clip_sound(&one, "cropped");
    assert!(rig.e.is_clip_muted("cropped") && rig.e.is_clip_soloed("cropped"));
    assert!(rig.e.is_key_locked("cropped"));
    assert_eq!(rig.e.clip_fade("cropped"), Some(LoopFade::Bar));
    assert!(!rig.e.is_clip_muted(&one) && !rig.e.is_clip_soloed(&one));
    assert_eq!(rig.e.clip_fade(&one), None);
    // removed from the row, its solo is forgotten (and the levels pushed again)
    rig.e.forget_clip_sound("cropped");
    assert!(!rig.e.is_clip_soloed("cropped") && !rig.e.is_clip_muted("cropped"));
}

// LoopSoundTests.theRowsFadeSetsHowLongALoopTakesToGoOut
#[test]
fn the_rows_fade_sets_how_long_a_loop_takes_to_go_out() {
    let mut rig = Rig::new();
    assert_eq!(rig.e.loop_fade(), LoopFade::Declick);
    let id = rig.take();
    let (bar, bpm) = (rig.bar(), rig.bpm);
    rig.e.set_loop_fade(LoopFade::Bar);
    assert_eq!(rig.e.loop_fade(), LoopFade::Bar);
    let bar_frames = (60.0 / bpm * SR) as i64 * 4;
    assert_eq!(rig.e.loop_fade_frames(None, bpm, false), bar_frames);
    assert_eq!(rig.e.loop_fade_frames(None, bpm, true), 480);
    rig.e.set_looping(&id, false);
    let stop = rig.e.loop_stop_frame(&id).expect("told to stop");
    run_to(&mut rig.e, &mut rig.r, stop + bar / 2);
    assert_eq!(
        rig.e.loop_state(&id),
        LoopState::Stopping,
        "half way through its bar-long fade"
    );
    run_to(&mut rig.e, &mut rig.r, stop + bar + 4096);
    assert_eq!(rig.e.loop_state(&id), LoopState::Off);
    assert_eq!(loop_peak(&mut rig.e, &mut rig.r, 4096), 0.0);
}

// LoopSoundTests.aClipsOwnFadeOverridesTheRowsAndFollowsACrop
#[test]
fn a_clips_own_fade_overrides_the_rows() {
    let mut rig = Rig::new();
    let id = rig.take();
    let bpm = rig.bpm;
    assert_eq!(rig.e.loop_fade_for(Some(&id)), LoopFade::Declick);
    assert_eq!(rig.e.clip_fade(&id), None);
    rig.e.set_clip_fade(&id, Some(LoopFade::Bar));
    rig.e.set_loop_fade(LoopFade::Off);
    assert_eq!(rig.e.clip_fade(&id), Some(LoopFade::Bar));
    assert_eq!(rig.e.loop_fade(), LoopFade::Off);
    let bar_frames = (60.0 / bpm * SR) as i64 * 4;
    assert_eq!(
        rig.e.loop_fade_frames(Some(&id), bpm, false),
        bar_frames,
        "its own"
    );
    assert_eq!(rig.e.loop_fade_frames(Some(&id), bpm, true), 480);
    assert_eq!(
        rig.e.loop_fade_frames(Some("other"), bpm, false),
        0,
        "the row's"
    );
    assert_eq!(rig.e.loop_fade_frames(None, bpm, false), 0);
    // null: the row's again
    rig.e.set_clip_fade(&id, None);
    assert_eq!(rig.e.clip_fade(&id), None);
    assert_eq!(rig.e.loop_fade_for(Some(&id)), LoopFade::Off);
}

// LoopSoundTests.aLockedLoopAtAnotherTempoPlaysAStretchedCopyAtPitchZero
#[test]
fn a_locked_loop_at_another_tempo_plays_a_stretched_copy_in_phase() {
    let mut rig = Rig::new();
    rig.e.loops.deterministic = true; // the stretch renders inline
    let id = rig.take();
    let bar = rig.bar();
    let len = rig.e.clip(&id).unwrap().buffer.length() as f64;
    rig.e.nudge("A", 0.04); // +4 %
    run_frames(&mut rig.e, &mut rig.r, bar / 4);
    assert!(!rig.e.is_clip_stretched(&id), "unlocked: varispeed");
    assert_eq!(rig.e.loops.mirror.playhead(0).1 as f64, len);
    rig.e.set_clip_key_lock(&id, true);
    assert!(rig.e.is_key_locked(&id));
    // the tempo holds for the debounce, the copy renders, then swaps in
    for _ in 0..2000 {
        if rig.e.is_clip_stretched(&id) {
            break;
        }
        blocks(&mut rig.e, &mut rig.r, 1);
    }
    assert!(rig.e.is_clip_stretched(&id));
    run_frames(&mut rig.e, &mut rig.r, 2048);
    let want = len / 1.04;
    let got = rig.e.loops.mirror.playhead(0).1 as f64;
    assert!(
        (got - want).abs() <= 1.0,
        "the stretched copy: {got} vs {want}"
    );
    assert!(rig.phase_error(&id) < 0.01, "swapped in phase");
    assert!(loop_peak(&mut rig.e, &mut rig.r, 4096) > 0.001);

    // unlocked again: back to its own audio, varispeed
    rig.e.set_clip_key_lock(&id, false);
    run_frames(&mut rig.e, &mut rig.r, 2048);
    assert!(!rig.e.is_clip_stretched(&id));
    assert_eq!(rig.e.loops.mirror.playhead(0).1 as f64, len);
    assert!(rig.phase_error(&id) < 0.01);
}

// LoopSoundTests.aLockedLoopAtAnotherTempoPlaysAStretchedCopyAtPitchZero, off the main thread: the
// copy is rendered on a worker and taken in by a later pass
#[test]
fn the_stretch_renders_on_a_worker_and_is_taken_in_later() {
    let mut rig = Rig::new();
    let id = rig.take();
    rig.e.nudge("A", 0.04);
    rig.e.set_clip_key_lock(&id, true);
    let mut seen_stretching = false;
    for _ in 0..4000 {
        if rig.e.is_clip_stretched(&id) {
            break;
        }
        blocks(&mut rig.e, &mut rig.r, 1);
        if rig.e.is_clip_stretching(&id) {
            seen_stretching = true;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    assert!(rig.e.is_clip_stretched(&id));
    assert!(
        seen_stretching,
        "a render was in flight before it swapped in"
    );
}

// LoopSoundTests.theRowsKeyLockCoversClipsWithoutTheirOwn
#[test]
fn the_rows_key_lock_covers_clips_without_their_own() {
    let mut rig = Rig::new();
    rig.e.loops.deterministic = true; // the stretch renders inline, as in a replay
    let id = rig.take();
    rig.e.set_loop_key_lock(true);
    assert!(rig.e.loop_key_lock() && rig.e.is_key_locked(&id));
    rig.e.nudge("A", -0.04); // -4 %
    let debounce = (djrust::deck::loop_sound::STRETCH_DEBOUNCE * SR) as i64;
    run_frames(&mut rig.e, &mut rig.r, debounce + 8192);
    assert!(rig.e.is_clip_stretched(&id));
    // a clip's own setting wins over the row's
    rig.e.set_clip_key_lock(&id, false);
    run_frames(&mut rig.e, &mut rig.r, 2048);
    assert!(!rig.e.is_clip_stretched(&id));
    // back near its own tempo, a locked loop needs no copy
    rig.e.set_clip_key_lock(&id, true);
    rig.e.nudge("A", 0.0);
    run_frames(&mut rig.e, &mut rig.r, debounce + 8192);
    assert!(!rig.e.is_clip_stretched(&id));
}

// LoopSoundTests.theLoopClockKeepsAVoicesTempoThroughASwap
#[test]
fn the_loop_clock_keeps_a_voices_tempo_through_a_swap() {
    let mut rig = Rig::new();
    rig.e.loops.deterministic = true;
    let id = rig.take();
    let base = rig.bpm;
    rig.e.nudge("A", 0.05); // +5 %
    rig.e.set_clip_key_lock(&id, true);
    let debounce = (djrust::deck::loop_sound::STRETCH_DEBOUNCE * SR) as i64;
    run_frames(&mut rig.e, &mut rig.r, debounce + 8192);
    assert!(rig.e.is_clip_stretched(&id));
    // nothing plays: the loop is the clock, at the tempo it was left at
    rig.e.pause("A");
    run_frames(&mut rig.e, &mut rig.r, 4096);
    let want = base * 1.05;
    assert!((rig.e.loop_target_bpm(&id).unwrap() - want).abs() < 0.01);
    assert!((rig.e.loop_clock_bpm().unwrap() - want).abs() < 0.05);
    // unlock: the voice swaps back to its own audio at speed 1.05, from the next block. The
    // clock keeps saying the same tempo
    rig.e.set_clip_key_lock(&id, false);
    assert!(
        (rig.e.loop_clock_bpm().unwrap() - want).abs() < 0.05,
        "before the swap"
    );
    run_frames(&mut rig.e, &mut rig.r, 2048);
    assert!(
        (rig.e.loop_clock_bpm().unwrap() - want).abs() < 0.05,
        "and after it"
    );
}

// a clip's level: muted or soloed out, the gain remembered
#[test]
fn a_clips_level_is_remembered_and_muted_or_soloed_out() {
    let mut rig = Rig::new();
    let id = rig.take();
    assert_eq!(rig.e.clip_gain(&id), 1.0);
    rig.e.set_clip_gain(&id, 0.5, None);
    assert_eq!(rig.e.audible_clip_gain(&id), 0.5);
    rig.e.set_clip_mute(&id, true);
    assert_eq!(rig.e.audible_clip_gain(&id), 0.0);
    assert_eq!(rig.e.clip_gain(&id), 0.5, "mute keeps the level");
    rig.e.set_clip_mute(&id, false);
    assert_eq!(rig.e.audible_clip_gain(&id), 0.5);
}
