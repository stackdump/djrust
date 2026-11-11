//! How the loops sound (`LoopSoundTests.swift`, the pure half): WSOLA keeps the length to the
//! ratio and the pitch of a tone, the fade names and frames, mute and solo as gains, and the
//! key-lock decision. Each test names the Swift test it ports ("(a)" in PORTING.md's parity
//! rule); the numeric stretch output is held to Swift's in `tests/loops_golden.rs`.
//!
//! The Swift tests that run the whole engine (a muted loop keeps its place, solo, a crop carries
//! the settings, the stretched copy swapping in in phase) are in `djhost/tests/deck_loop_sound.rs`.

use djrust::deck::loop_sound::*;
use djrust::deck::turntable::SampleBuffer;
use djrust::fx::time_stretch::{frame_length, resample, stretch, wsola};

/// `count` samples of a sine at `hz`, amplitude 0.5 (`sineTone`).
fn sine_tone(hz: f64, count: usize, sr: f64) -> Vec<f32> {
    (0..count)
        .map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / sr).sin()) as f32)
        .collect()
}

/// The frequency of a tone from its rising zero crossings, interpolated between samples
/// (`zeroCrossingHz`).
fn zero_crossing_hz(x: &[f32], sr: f64) -> f64 {
    let mut crossings: Vec<f64> = Vec::new();
    for i in 1..x.len() {
        let (a, b) = (x[i - 1], x[i]);
        if a < 0.0 && b >= 0.0 {
            crossings.push((i - 1) as f64 + (-a / (b - a)) as f64);
        }
    }
    if crossings.len() < 2 {
        return 0.0;
    }
    (crossings.len() - 1) as f64 / ((crossings[crossings.len() - 1] - crossings[0]) / sr)
}

// LoopSoundTests.wsolaKeepsTheLengthToTheRatio
#[test]
fn wsola_keeps_the_length_to_the_ratio() {
    let sr = 48_000.0;
    let x = sine_tone(440.0, 96_000, sr);
    for ratio in [0.8, 0.97, 1.25, 1.5] {
        let out = wsola(&x, &x, ratio, sr);
        let want = x.len() as f64 / ratio;
        assert!(
            (out.0.len() as f64 - want).abs() <= 512.0,
            "{ratio}: {} vs {want}",
            out.0.len()
        );
        assert_eq!(out.0.len(), out.1.len());
    }
}

// LoopSoundTests.wsolaKeepsTheFrequencyOfATone
#[test]
fn wsola_keeps_the_frequency_of_a_tone() {
    let sr = 48_000.0;
    // 440 periods in a second: a seamless loop, as a clip is
    let x = sine_tone(440.0, 48_000, sr);
    for ratio in [0.8, 1.25] {
        let out = wsola(&x, &x, ratio, sr).0;
        let n = out.len();
        let hz = zero_crossing_hz(&out[n / 8..7 * n / 8], sr);
        assert!((hz / 440.0 - 1.0).abs() < 0.01, "at {ratio}x: {hz} Hz");
        // and it is a tone, not a gap-filled mess: about the input's level throughout
        let peak = out[n / 8..7 * n / 8]
            .iter()
            .fold(0f32, |p, v| p.max(v.abs()));
        assert!(peak > 0.45 && peak < 0.55, "peak {peak}");
    }
}

// TimeStretch.wsola's guard: a loop shorter than two frames is resampled; the stereo image
// shares one alignment; `stretch` hands back a stereo buffer of round(count / ratio) samples
#[test]
fn a_short_loop_is_resampled_and_the_buffer_form_is_stereo() {
    let sr = 48_000.0;
    assert_eq!(frame_length(sr), 1024);
    assert_eq!(frame_length(22_050.0), 2 * 235);
    let short = sine_tone(440.0, 1500, sr); // under two frames
    let (l, r) = wsola(&short, &short, 2.0, sr);
    assert_eq!((l.len(), r.len()), (750, 750));
    assert_eq!(l, resample(&short, 750));
    // linear interpolation, wrapping at the loop's end
    let ramp = [0.0f32, 1.0, 2.0, 3.0];
    assert_eq!(resample(&ramp, 8), [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 1.5]);
    // a long left/right pair keeps its two channels apart
    let l = sine_tone(440.0, 48_000, sr);
    let r = sine_tone(660.0, 48_000, sr);
    let b = SampleBuffer::new(l.clone(), Some(r.clone()), sr);
    let s = stretch(&b, 1.25);
    assert_eq!(s.length(), 38_400);
    assert!(s.right.is_some() && s.left != s.right.clone().unwrap());
    let hz_l = zero_crossing_hz(&s.left[4800..33_600], sr);
    let hz_r = zero_crossing_hz(&s.right()[4800..33_600], sr);
    assert!((hz_l / 440.0 - 1.0).abs() < 0.01 && (hz_r / 660.0 - 1.0).abs() < 0.01);
}

// LoopSoundTests.theSoundActionsRoundTrip (the fade names)
#[test]
fn the_fade_names_round_trip() {
    for f in LoopFade::ALL {
        assert_eq!(LoopFade::from_raw(f.raw()), Some(f));
    }
    assert_eq!(
        LoopFade::ALL.map(LoopFade::label),
        ["off", "10 ms", "1 beat", "1 bar"]
    );
    assert_eq!(LoopFade::from_raw("10ms"), Some(LoopFade::Declick));
    assert_eq!(LoopFade::from_raw("x"), None);
}

// LoopSoundTests.theRowsFadeSetsHowLongALoopTakesToGoOut / aClipsOwnFadeOverridesTheRowsAndFollowsACrop
// (the frames: 120 bpm at 48 kHz, a bar = 96000, a short one only de-clicks to 480)
#[test]
fn fade_frames_follow_the_tempo_and_a_short_one_only_declicks() {
    let sr = 48_000.0;
    assert_eq!(loop_fade_frames(LoopFade::Bar, 120.0, false, sr), 96_000);
    assert_eq!(loop_fade_frames(LoopFade::Bar, 120.0, true, sr), 480);
    assert_eq!(loop_fade_frames(LoopFade::Beat, 120.0, false, sr), 24_000);
    assert_eq!(loop_fade_frames(LoopFade::Declick, 120.0, false, sr), 480);
    assert_eq!(loop_fade_frames(LoopFade::Off, 120.0, false, sr), 0);
    assert_eq!(loop_fade_frames(LoopFade::Off, 120.0, true, sr), 0);
    // no tempo: the beat is a de-click
    assert_eq!(loop_fade_frames(LoopFade::Bar, 0.0, false, sr), 4 * 480);
}

// LoopSoundTests.aMutedLoopIsSilentAndKeepsItsPlace / soloSilencesEveryLoopThatIsNotSoloed
#[test]
fn mute_and_solo_are_gains() {
    assert_eq!(audible(false, false, false, 0.7), 0.7);
    assert_eq!(audible(true, false, false, 0.7), 0.0, "muted");
    assert_eq!(audible(false, true, false, 0.7), 0.0, "soloed out");
    assert_eq!(audible(false, true, true, 0.7), 0.7, "the soloed one plays");
    assert_eq!(audible(true, true, true, 0.7), 0.0, "muted wins over solo");
}

fn state(target: f64, buffer: f64) -> KeyLockState {
    KeyLockState {
        locked: true,
        clip_bpm: 120.0,
        target_bpm: target,
        buffer_bpm: buffer,
        stretching: false,
        want: None,
        cached_bpm: None,
        now: 1000,
        sample_rate: 48_000.0,
    }
}

// LoopSoundTests.aLockedLoopAtAnotherTempoPlaysAStretchedCopyAtPitchZero and
// theRowsKeyLockCoversClipsWithoutTheirOwn (the decisions, step by step)
#[test]
fn key_lock_waits_out_the_debounce_then_renders_and_swaps() {
    // 124.8 bpm against a 120 bpm clip: first wanted, remembered
    let s = state(124.8, 120.0);
    assert_eq!(key_lock_step(&s), KeyLockStep::Want);
    // the tempo holds: still inside the debounce, then rendered
    let held = KeyLockState {
        want: Some((124.8, 1000)),
        now: 1000 + 12_000 - 1,
        ..s
    };
    assert_eq!(key_lock_step(&held), KeyLockStep::Hold);
    let due = KeyLockState {
        now: 1000 + 12_000,
        ..held
    };
    assert_eq!(key_lock_step(&due), KeyLockStep::Render);
    // a tempo that moved on is a new want
    let moved = KeyLockState {
        want: Some((126.0, 1000)),
        now: 100_000,
        ..s
    };
    assert_eq!(key_lock_step(&moved), KeyLockStep::Want);
    // a render in flight is left alone
    let busy = KeyLockState {
        stretching: true,
        ..s
    };
    assert_eq!(key_lock_step(&busy), KeyLockStep::Wait);
    // a cached copy for about that tempo is swapped in at once
    let cached = KeyLockState {
        cached_bpm: Some(124.9),
        ..s
    };
    assert_eq!(key_lock_step(&cached), KeyLockStep::UseCached);
    // the stretched copy playing at the target tempo: nothing to do
    let there = state(124.8, 124.8);
    assert_eq!(key_lock_step(&there), KeyLockStep::Settled);
}

// LoopSoundTests.theRowsKeyLockCoversClipsWithoutTheirOwn ("back near its own tempo, a locked
// loop needs no copy"; "a clip's own setting wins": unlocked swaps back to its own audio)
#[test]
fn key_lock_off_or_near_the_clips_tempo_goes_back_to_its_own_audio() {
    // locked but within the tolerance of its own tempo (+2 %: 122.4, still a stretch;
    // +0.4 %: no copy)
    let near = state(120.48, 120.0);
    assert_eq!(key_lock_step(&near), KeyLockStep::Own { swap_back: false });
    // it had a stretched copy loaded (at 124.8) and the tempo came back: swap back
    let back = state(120.48, 124.8);
    assert_eq!(key_lock_step(&back), KeyLockStep::Own { swap_back: true });
    // unlocked: its own audio, varispeed
    let unlocked = KeyLockState {
        locked: false,
        ..state(124.8, 124.8)
    };
    assert_eq!(
        key_lock_step(&unlocked),
        KeyLockStep::Own { swap_back: true }
    );
    // loopAudio: a start takes a cached copy only when locked, off tempo, and the copy fits
    assert!(use_cached_copy(true, 120.0, 124.8, Some(124.9)));
    assert!(!use_cached_copy(false, 120.0, 124.8, Some(124.9)));
    assert!(
        !use_cached_copy(true, 120.0, 120.2, Some(120.2)),
        "near its own tempo"
    );
    assert!(
        !use_cached_copy(true, 120.0, 124.8, Some(130.0)),
        "a copy for another tempo"
    );
    assert!(!use_cached_copy(true, 120.0, 124.8, None));
    // finishStretch: the copy holds the clip's beats, so its tempo follows from its length
    let bpm = stretched_bpm(120.0, 96_000, 92_307);
    assert!((bpm - 124.8).abs() < 0.01, "{bpm}");
}
