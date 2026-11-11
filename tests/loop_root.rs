//! A loop's root is found, not assumed to be C (docs/MPK.md, "Loop root"): from the file's name
//! when it says a key, else from the audio; a hand (`loopRoot`) wins and `detect` lets it go.

use djrust::musicality::loop_key::LoopKey;
#[cfg(feature = "decode")]
use djrust::host::deck::loop_pads::FileLaunch;
use djrust::host::deck::loop_pads::Quant;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;

fn rig() -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.loops.deterministic = true;
    (e, r)
}

fn hz(n: i32) -> f64 {
    440.0 * 2f64.powf((n as f64 - 69.0) / 12.0)
}

/// A few seconds of notes with harmonics, whole loop = `bars` bars at 120 bpm.
fn line(notes: &[i32], bars: usize) -> Vec<f32> {
    let n = (bars as f64 * 2.0 * SR) as usize;
    let per = n / notes.len();
    (0..n)
        .map(|i| {
            let note = notes[(i / per).min(notes.len() - 1)];
            let t = i as f64 / SR;
            let env = (std::f64::consts::PI * (i % per) as f64 / per as f64).sin();
            let f = hz(note);
            let v: f64 = [(1.0, 1.0), (2.0, 0.5), (3.0, 0.25)]
                .iter()
                .map(|(h, a)| a * (2.0 * std::f64::consts::PI * f * h * t).sin())
                .sum();
            (0.2 * env * v) as f32
        })
        .collect()
}

fn noise(n: usize) -> Vec<f32> {
    let mut s = 0x9E3779B97F4A7C15u64;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 23) as f32 - 1.0) * 0.3
        })
        .collect()
}

fn root_of(e: &Engine, id: &str) -> (u8, bool, Option<LoopKey>, &'static str) {
    let r = e
        .loop_root_report()
        .into_iter()
        .find(|r| r.id == id)
        .expect("on the row");
    (r.root, r.manual, r.key, r.from)
}

#[test]
fn a_loop_in_a_minor_gets_a_as_its_root_and_a_drum_loop_keeps_c() {
    let (mut e, _r) = rig();
    let am = [57, 60, 64, 57, 59, 60, 62, 64, 65, 68, 69, 57];
    let tonal = line(&am, 4);
    let id = e.add_audio_clip("lead", tonal.clone(), tonal, 4, false);
    let (root, manual, key, from) = root_of(&e, &id);
    assert_eq!(root, 9, "{key:?}");
    assert!(!manual);
    let k = key.expect("a key");
    assert!(k.minor && k.tonal, "{k:?}");
    assert_eq!(from, "audio");

    let n = noise((8.0 * SR) as usize);
    let drums = e.add_audio_clip("hats", n.clone(), n, 4, false);
    let (root, _, key, _) = root_of(&e, &drums);
    assert_eq!(root, 0, "no pitch, no root");
    assert!(key.is_none_or(|k| !k.tonal), "{key:?}");
}

#[test]
fn a_key_press_now_transposes_from_the_found_root() {
    use djrust::stretch::semitones_to;
    let (mut e, _r) = rig();
    let am = [57, 60, 64, 57, 59, 60, 62, 64, 65, 68, 69, 57];
    let l = line(&am, 4);
    let id = e.add_audio_clip("lead", l.clone(), l, 4, false);
    e.launch_clip(&id, Quant::Now);
    // C (60) against a root of A: up three, the shortest way
    assert_eq!(semitones_to(e.clip_extra(&id).root, 0), 3);
}

#[test]
fn a_hand_wins_and_detect_lets_it_go() {
    let (mut e, _r) = rig();
    let am = [57, 60, 64, 57, 59, 60, 62, 64, 65, 68, 69, 57];
    let l = line(&am, 4);
    let id = e.add_audio_clip("lead", l.clone(), l, 4, false);
    e.launch_clip(&id, Quant::Now);
    e.set_loop_root(None, 2);
    let (root, manual, key, _) = root_of(&e, &id);
    assert_eq!((root, manual), (2, true));
    assert!(key.is_some(), "what was found is still known");
    // looking again does not undo the hand until asked to
    e.find_root(&id);
    assert_eq!(root_of(&e, &id).0, 2);
    let found = e.detect_loop_roots(None);
    assert_eq!(found.len(), 1);
    let (root, manual, _, _) = root_of(&e, &id);
    assert_eq!((root, manual), (9, false));
}

#[test]
fn auto_root_can_be_turned_off() {
    let (mut e, _r) = rig();
    e.set_auto_root(false);
    assert!(!e.auto_root());
    let am = [57, 60, 64, 57, 59, 60, 62, 64, 65, 68, 69, 57];
    let l = line(&am, 4);
    let id = e.add_audio_clip("lead", l.clone(), l, 4, false);
    assert_eq!(root_of(&e, &id), (0, false, None, ""));
    // asking for it is still possible
    e.detect_loop_roots(None);
    assert_eq!(root_of(&e, &id).0, 9);
}

#[cfg(feature = "decode")]
#[test]
fn a_file_name_that_says_its_key_is_believed_over_the_audio() {
    let (mut e, _r) = rig();
    let dir = std::env::temp_dir().join(format!("djrust-loop-root-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // the audio is C major; the name says F# minor: the name is the pack's word for it
    let cmaj = [60, 64, 67, 60, 62, 64, 65, 67, 69, 71, 72, 60];
    let l = line(&cmaj, 1);
    let path = dir.join("pad_120bpm_F#min_take.wav");
    std::fs::write(&path, djrust::wav::wav_bytes(&l, &l, SR, true)).unwrap();
    e.trigger_file(FileLaunch {
        path: path.clone(),
        bars: Some(1),
        bpm: None,
        once: false,
        quant: Quant::Bar,
        gain: 1.0,
        choke: None,
        launch: false,
    });
    let mut id = None;
    for _ in 0..400 {
        e.service_loops();
        if let Some(c) = e.loops.row.clips.first() {
            id = Some(c.id.clone());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let id = id.expect("the file landed on the row");
    let (root, _, key, from) = root_of(&e, &id);
    assert_eq!((root, from), (6, "name"), "{key:?}");
    assert!(key.unwrap().minor);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "decode")]
#[test]
fn a_one_shot_keeps_c_whatever_it_sings() {
    let (mut e, _r) = rig();
    let dir = std::env::temp_dir().join(format!("djrust-loop-root-os-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let l = line(&[57, 60, 64, 57], 1);
    let path = dir.join("hook_Am.wav");
    std::fs::write(&path, djrust::wav::wav_bytes(&l, &l, SR, true)).unwrap();
    e.trigger_file(FileLaunch {
        path,
        bars: None,
        bpm: None,
        once: true,
        quant: Quant::Beat,
        gain: 1.0,
        choke: None,
        launch: false,
    });
    let mut id = None;
    for _ in 0..400 {
        e.service_loops();
        if let Some(c) = e.loops.row.clips.first() {
            id = Some(c.id.clone());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let id = id.expect("landed");
    let (root, _, key, _) = root_of(&e, &id);
    assert_eq!(root, 0, "a vocal phrase is not transposed as a loop");
    assert_eq!(key.map(|k| (k.root, k.minor)), Some((9, true)), "but its key is known");
    let _ = std::fs::remove_dir_all(&dir);
}
