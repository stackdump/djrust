//! The pooled live-keys voice (`WaveRunner.noteOn` / `noteOff` / `allNotesOff`): the Swift
//! suites' own tests ported (PORTING.md's proof, form (a)), each named for the test it comes
//! from (`WaveLiveTests.WaveRunnerLiveTests`), plus the voice's own contract. The same keys held
//! against Swift's output sample for sample are the "live" cases of `tests/runner_golden.rs`.

use djrust::pflow::Project;
use djrust::wave::*;
use djrust::{compose, JsonObject};

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        ov.set("structure", s.into());
    }
    compose(genre, &ov.into())
}

fn max_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

const SR: f64 = 8000.0;

fn runner(p: &Project) -> WaveRunner {
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.load(p);
    r.set_tempo(120.0);
    r.play();
    r
}

fn run(r: &mut WaveRunner, blocks: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut l = vec![0f32; 128];
    for _ in 0..blocks {
        r.process(&mut l, None, 128);
        out.extend_from_slice(&l);
    }
    out
}

// WaveRunnerLiveTests.liveKeysSoundWhileHeldAndRingOutAfter
#[test]
fn live_keys_sound_while_held_and_ring_out_after() {
    let mut r = runner(&project("techno", 42.0, None));
    for n in [
        "melody", "bass", "harmony", "kick", "snare", "hihat", "hit1", "hit2", "hit3", "hit4",
        "arp", "clap",
    ] {
        r.set_mute_hash(str_hash(n), true);
    }
    run(&mut r, 40);
    let quiet = max_abs(&run(&mut r, 20));
    r.note_on(60, 110);
    let held = run(&mut r, 40);
    let held_peak = max_abs(&held[held.len() - 2000..]);
    assert!(
        held_peak > 0.02,
        "a held key sustains (peak {held_peak}, quiet {quiet})"
    );
    r.note_off(60);
    run(&mut r, 60);
    let after = max_abs(&run(&mut r, 20));
    assert!(after < held_peak * 0.2, "released keys die away");
}

/// The live voice is the project's last lane, shaped like the melody net's instrument, with
/// eight voices; it belongs to no net, no channel and no rest class.
#[test]
fn the_live_voice_is_the_last_lane() {
    let p = project("techno", 42.0, None);
    let r = runner(&p);
    let proj = r.project().unwrap();
    let live = proj.live_lane();
    assert_eq!(live.net_id, "<live>");
    assert_eq!(live.channel, -1);
    assert!(live.is_tonal() && !live.active);
    assert!(std::ptr::eq(live, proj.lanes().last().unwrap()));
    assert!(
        proj.lanes_of("<live>").is_empty(),
        "no net renders through it"
    );
    // the melody net's instrument, in the melody group
    let melody = p.nets.get("melody").unwrap();
    let want = tonal_voice(Some(&melody.track.instrument), Some("melody"));
    assert_eq!(live.voice.wave, want.wave);
    assert_eq!(live.voice.level, want.level);
}

/// Each press takes the quietest voice that is not held; with all eight held the nine-key
/// chord steals the quietest held one. Held keys are the ones that sound until `note_off`.
#[test]
fn keys_take_free_voices_first_then_steal() {
    let mut r = runner(&project("techno", 42.0, None));
    for n in [60, 62, 64, 65, 67, 69, 71, 72] {
        r.note_on(n, 100);
        run(&mut r, 2);
    }
    let mut held = r.project().unwrap().live_lane().held_notes();
    held.sort();
    assert_eq!(
        held,
        vec![60, 62, 64, 65, 67, 69, 71, 72],
        "eight voices, eight keys"
    );
    // a ninth: every voice is held, so one is taken over
    r.note_on(74, 100);
    let mut held = r.project().unwrap().live_lane().held_notes();
    held.sort();
    assert_eq!(held.len(), 8);
    assert!(held.contains(&74));
    // releasing a key that is not down changes nothing; releasing one that is lets go of it
    r.note_off(99);
    assert_eq!(r.project().unwrap().live_lane().held_notes().len(), 8);
    r.note_off(74);
    assert!(!r.project().unwrap().live_lane().held_notes().contains(&74));
    // a voice already let go is the one the next press takes before any held one
    let before = r.project().unwrap().live_lane().held_notes().len();
    r.note_on(50, 100);
    assert_eq!(
        r.project().unwrap().live_lane().held_notes().len(),
        before + 1
    );
    r.all_notes_off();
    assert!(r.project().unwrap().live_lane().held_notes().is_empty());
}

/// A key takes a voice that is not held before any held one, even a quiet held one beside a
/// louder voice still ringing out: `noteOn` adds 1000 to a held voice's level (Swift:
/// `abs(sd + sa) + (hold ? 1000 : 0)`), then takes the quietest. Form (c): read from the Swift
/// source (no Swift test pins it; the "live" goldens never press a key over a ringing release).
#[test]
fn a_released_voice_is_taken_before_a_quiet_held_one() {
    let mut r = runner(&project("techno", 42.0, None));
    // eight voices in use: one soft key (velocity 10), seven loud ones
    r.note_on(60, 10);
    run(&mut r, 2);
    for n in [62, 64, 65, 67, 69, 71, 72] {
        r.note_on(n, 127);
        run(&mut r, 2);
    }
    run(&mut r, 40);
    // one loud key lets go and rings out on its 0.3 s release: at 0.16 s it is still about six
    // times louder than the soft key that is held
    r.note_off(72);
    run(&mut r, 10);
    r.note_on(50, 100);
    let mut held = r.project().unwrap().live_lane().held_notes();
    held.sort();
    assert_eq!(
        held,
        vec![50, 60, 62, 64, 65, 67, 69, 71],
        "the released voice was taken, the soft held one was not"
    );
}

/// Without a project the keys do nothing (`guard let proj = current`).
#[test]
fn keys_without_a_project_do_nothing() {
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.note_on(60, 100);
    r.note_off(60);
    r.all_notes_off();
    let mut l = vec![0f32; 128];
    r.process(&mut l, None, 128);
    assert_eq!(max_abs(&l), 0.0);
}

/// Velocity is clamped to 1...127: 0 still sounds (as the quietest), 300 is as loud as 127.
#[test]
fn velocity_is_clamped() {
    let peak_of = |v: i64| {
        let mut r = runner(&project("jazz", 7.0, None));
        for n in r
            .graph()
            .unwrap()
            .nets
            .iter()
            .map(|n| n.id.clone())
            .collect::<Vec<_>>()
        {
            r.set_mute(&n, true);
        }
        r.note_on(69, v);
        max_abs(&run(&mut r, 20))
    };
    assert!(peak_of(0) > 0.0 && peak_of(0) == peak_of(1));
    assert_eq!(peak_of(300), peak_of(127));
    assert!(peak_of(127) > peak_of(1));
}

fn bind_slot_5(_instrument: &str, _part: &str) -> Option<usize> {
    Some(5)
}

/// A live voice on a sampler plays no synth voice: the keys go to the host as the MIDI
/// `queue(_:_:_:_:)` sends the bank (note on, note off, all notes off), at the frame of the
/// block they were pressed in. Golden: none (the Swift bank is Apple's AUSampler).
#[test]
fn a_sampled_live_voice_sends_midi_to_its_slot() {
    let p = project("techno", 42.0, None);
    let opts = WaveOptions {
        master: Master::Linear,
        sample_binder: Some(bind_slot_5),
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, opts.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &opts, 0));
    r.play();
    let mut l = vec![0f32; 128];
    r.process_at(&mut l, None, 128, 1000);
    r.note_on(60, 200); // velocity clamps to 127
    r.note_on(64, 0); // ...and to 1
    r.note_off(60);
    r.all_notes_off();
    let keys = std::mem::take(&mut r.sample_keys);
    let k = |status, d1, d2| SampleKey {
        slot: 5,
        status,
        data1: d1,
        data2: d2,
        frame: 1000,
    };
    assert_eq!(
        keys,
        vec![
            k(0x90, 60, 127),
            k(0x90, 64, 1),
            k(0x80, 60, 0),
            k(0xB0, 123, 0)
        ]
    );
    // the queue never grows on the audio thread: a host that took its buffer away (rather than
    // `clear()`ing it) loses the key, it does not make the runner allocate
    r.note_on(61, 100);
    assert!(r.sample_keys.is_empty() && r.sample_keys.capacity() == 0);
    assert!(
        !r.project().unwrap().live_lane().active,
        "the synth voice stays silent"
    );
    // a synth live voice reports nothing
    let mut plain = runner(&p);
    plain.note_on(60, 100);
    assert!(plain.sample_keys.is_empty());
}
