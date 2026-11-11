#![cfg(feature = "decode")]
//! A pad's one-shot args are applied: `gain` scales what plays, `choke` cuts the others in its
//! group, and a different gain or group is a different clip of the same file.

use djrust::looplib::wav_encode;
use djrust::host::deck::loop_pads::{FileLaunch, Quant};
use djrust::host::deck::loops::LoopState;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use djrust::host::stores::temp_path;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn rig() -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.set_crossfader(0.0);
    e.set_loop_volume(1.0);
    (e, r)
}

fn sine_wav(name: &str, secs: f64) -> std::path::PathBuf {
    let n = (secs * SR) as usize;
    let s: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / SR).sin() as f32 * 0.5)
        .collect();
    let dir = temp_path("oneshot-args");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, wav_encode(SR as u32, &s, Some(&s))).unwrap();
    p
}

fn launch(path: &std::path::Path, gain: f32, choke: Option<&str>) -> FileLaunch {
    FileLaunch {
        path: path.to_path_buf(),
        bars: None,
        bpm: None,
        once: true,
        quant: Quant::Now,
        gain,
        choke: choke.map(str::to_string),
        launch: true,
    }
}

fn run(e: &mut Engine, r: &mut Renderer, blocks: usize) -> Vec<f32> {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::new();
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        out.extend_from_slice(&l);
        std::thread::sleep(std::time::Duration::from_micros(300)); // let the decoder thread land
    }
    out
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn gain_scales_the_pad() {
    let wav = sine_wav("a.wav", 1.0);
    let (mut e, mut r) = rig();
    e.trigger_file(launch(&wav, 1.0, None));
    let loud = peak(&run(&mut e, &mut r, 80));
    let (mut e, mut r) = rig();
    e.trigger_file(launch(&wav, 0.25, None));
    let quiet = peak(&run(&mut e, &mut r, 80));
    assert!(loud > 0.05, "it sounds ({loud})");
    assert!((quiet / loud - 0.25).abs() < 0.05, "{quiet} vs {loud}");
}

#[test]
fn a_choke_group_cuts_the_other_pad() {
    let a = sine_wav("open.wav", 2.0);
    let b = sine_wav("closed.wav", 2.0);
    let (mut e, mut r) = rig();
    e.trigger_file(launch(&a, 1.0, Some("hat")));
    run(&mut e, &mut r, 40);
    // a one-shot reads "Stopping" while it plays out to its known end: sounding = not Off
    let sounding = |e: &Engine, n: &str| {
        e.loops
            .row
            .clips
            .iter()
            .find(|c| e.clip_extra(&c.id).from.ends_with(n))
            .map(|c| e.loop_state(&c.id) != LoopState::Off)
    };
    assert_eq!(sounding(&e, "open.wav"), Some(true));
    e.trigger_file(launch(&b, 1.0, Some("hat")));
    run(&mut e, &mut r, 60);
    assert_eq!(sounding(&e, "closed.wav"), Some(true));
    assert_eq!(sounding(&e, "open.wav"), Some(false), "cut by its group");
}

#[test]
fn no_group_means_no_choke() {
    let a = sine_wav("x.wav", 2.0);
    let b = sine_wav("y.wav", 2.0);
    let (mut e, mut r) = rig();
    e.trigger_file(launch(&a, 1.0, None));
    run(&mut e, &mut r, 40);
    e.trigger_file(launch(&b, 1.0, None));
    run(&mut e, &mut r, 60);
    let on = e.loops.row.clips.iter().filter(|c| e.loop_state(&c.id) != LoopState::Off).count();
    assert_eq!(on, 2);
}
