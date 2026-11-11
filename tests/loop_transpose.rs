//! Keys transpose a playing loop: the pitch moves, the length and the bar grid do not; a
//! one-shot plays once and frees its voice.

use djrust::musicality::pitch_measure::fundamental;
use djrust::stretch::ratio;
use djrust::host::deck::loop_pads::Quant;
use djrust::host::deck::loops::LoopState;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn sine(hz: f64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / SR).sin() as f32 * 0.4)
        .collect()
}

/// Render `n` blocks, return the left channel.
fn run(e: &mut Engine, r: &mut Renderer, n: usize) -> Vec<f32> {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::with_capacity(n * BLOCK);
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        out.extend_from_slice(&l);
    }
    out
}

fn rig() -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_loop_volume(1.0);
    // these tests press keys against a root of C; the test tone is not in a key worth finding
    e.set_auto_root(false);
    (e, r)
}

fn pitch_of(out: &[f32]) -> f64 {
    // loop channel gain is constant; measure a steady stretch
    fundamental(&out[out.len() - 8192..], SR).expect("pitched")
}

#[test]
fn a_key_transposes_a_playing_loop_and_it_stays_on_the_grid() {
    let (mut e, mut r) = rig();
    // 2 bars at 120 bpm = 4 s; 240 Hz is whole cycles
    let n = (4.0 * SR) as usize;
    let id = e.add_audio_clip("sine", sine(240.0, n), sine(240.0, n), 2, false);
    e.launch_clip(&id, Quant::Now);
    let out = run(&mut e, &mut r, 120);
    assert_eq!(e.loop_state(&id), LoopState::On);
    let base = pitch_of(&out);
    assert!((base / 240.0 - 1.0).abs() < 0.02, "{base}");
    let phase_before = e.loop_playhead(&id).unwrap();

    // E (64) against a root of C: +4 semitones, on the next beat; wait for the copy
    e.transpose_loops(None, 64, Quant::Beat, false);
    let mut out = Vec::new();
    for _ in 0..200 {
        out = run(&mut e, &mut r, 40);
        std::thread::sleep(std::time::Duration::from_millis(20));
        if e.clip_extra(&id).shift == 4 {
            break;
        }
    }
    assert_eq!(e.clip_extra(&id).shift, 4, "the shifted copy landed");
    out.extend(run(&mut e, &mut r, 100));
    let got = pitch_of(&out);
    assert!((got / (240.0 * ratio(4)) - 1.0).abs() < 0.03, "{got}");
    // same length, so the playhead still moves through the same loop
    assert!(e.loop_playhead(&id).is_some());
    assert_ne!(e.loop_playhead(&id), Some(phase_before));

    // back to the root
    e.transpose_loops(None, 60, Quant::Now, false);
    let mut out = Vec::new();
    for _ in 0..50 {
        out = run(&mut e, &mut r, 100);
        if e.clip_extra(&id).shift == 0 {
            break;
        }
    }
    assert_eq!(e.clip_extra(&id).shift, 0);
    out.extend(run(&mut e, &mut r, 100));
    assert!((pitch_of(&out) / 240.0 - 1.0).abs() < 0.02);
}

#[test]
fn tape_mode_follows_the_pitch_with_the_speed() {
    let (mut e, mut r) = rig();
    let n = (4.0 * SR) as usize;
    let id = e.add_audio_clip("sine", sine(240.0, n), sine(240.0, n), 2, false);
    e.launch_clip(&id, Quant::Now);
    run(&mut e, &mut r, 40);
    e.transpose_loops(None, 67, Quant::Now, true); // G: -5, nearest way round
    let out = run(&mut e, &mut r, 160);
    let got = pitch_of(&out);
    assert!((got / (240.0 * ratio(-5)) - 1.0).abs() < 0.03, "{got}");
}

#[test]
fn a_one_shot_plays_once_and_frees_its_voice() {
    let (mut e, mut r) = rig();
    let n = (0.25 * SR) as usize;
    let id = e.add_audio_clip("hit", sine(440.0, n), sine(440.0, n), 1, true);
    e.launch_clip(&id, Quant::Now);
    let first = run(&mut e, &mut r, 20);
    assert!(first.iter().any(|v| v.abs() > 0.05), "it sounds");
    // 0.25 s = 47 blocks; well past that it is silent and the voice is free
    run(&mut e, &mut r, 120);
    let tail = run(&mut e, &mut r, 20);
    assert!(tail.iter().all(|v| v.abs() < 1e-4), "no second pass");
    assert_eq!(e.loop_state(&id), LoopState::Off);
    // and it can be hit again
    e.launch_clip(&id, Quant::Now);
    assert!(run(&mut e, &mut r, 20).iter().any(|v| v.abs() > 0.05));
}
