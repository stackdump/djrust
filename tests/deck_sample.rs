//! Sample decks: the deck's turntable plays a loop or a file (`Deck`'s sample mode), with the
//! CUE button, hot cues, loops, the platter and the controllers on it.

use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer, Rig};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn render(r: &mut Renderer, blocks: usize) -> f64 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut e = 0.0;
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        e += l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    (e / (blocks * BLOCK) as f64).sqrt()
}

fn demo_deck() -> (Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.load_demo("A"));
    render(&mut r, 1);
    (e, r)
}

fn pos(e: &Engine) -> f64 {
    e.sample_position("A").unwrap()
}

#[test]
fn the_demo_loop_plays_on_the_turntable() {
    let (mut e, mut r) = demo_deck();
    let d = e.deck("A").unwrap();
    assert!(d.mirror.is_sample_mode());
    assert!(!d.mirror.is_playing());
    assert_eq!(render(&mut r, 10), 0.0, "stopped: silent");
    e.play("A");
    let level = render(&mut r, 100);
    assert!(level > 0.01, "plays: {level}");
    assert!(e.deck("A").unwrap().mirror.is_playing());
    // the motor spins up in ~45 ms, then runs at 1x: ~100 blocks played
    let p = pos(&e);
    assert!(p > 90.0 * BLOCK as f64 && p < 101.0 * BLOCK as f64, "{p}");
    // the tempo fader runs it faster
    e.nudge("A", 0.08);
    let before = pos(&e);
    render(&mut r, 100);
    let ran = pos(&e) - before;
    assert!((ran / (100.0 * BLOCK as f64) - 1.08).abs() < 0.01, "{ran}");
}

#[test]
fn cue_sets_while_paused_and_returns_while_playing() {
    let (mut e, mut r) = demo_deck();
    e.play("A");
    render(&mut r, 50);
    e.pause("A");
    render(&mut r, 20); // the motor brakes
    let here = pos(&e);
    e.cue("A");
    assert_eq!(
        e.deck("A").unwrap().sample.as_ref().unwrap().cue_point,
        here
    );
    e.play("A");
    render(&mut r, 60);
    assert!(pos(&e) > here + 1000.0);
    e.cue("A");
    render(&mut r, 1);
    assert!(
        !e.deck("A").unwrap().mirror.is_playing(),
        "CUE while playing pauses"
    );
    render(&mut r, 20);
    // the seek lands on the cue point, and the motor (as in Swift) coasts ~45 ms past it
    let past = pos(&e) - here;
    assert!(
        (0.0..0.06 * SR).contains(&past),
        "and goes back: {} vs {here}",
        pos(&e)
    );
}

#[test]
fn hot_cues_set_then_jump() {
    let (mut e, mut r) = demo_deck();
    e.play("A");
    render(&mut r, 40);
    e.hot_cue("A", 2);
    let at = e.deck("A").unwrap().hot_cues[&2];
    render(&mut r, 80);
    assert!(pos(&e) > at + 30_000.0);
    e.hot_cue("A", 2);
    render(&mut r, 1);
    assert!(
        (pos(&e) - at).abs() < 2.0 * BLOCK as f64,
        "{} vs {at}",
        pos(&e)
    );
    e.clear_cue("A", 2);
    assert!(!e.deck("A").unwrap().hot_cues.contains_key(&2));
}

#[test]
fn a_loop_holds_the_playhead() {
    let (mut e, mut r) = demo_deck();
    e.play("A");
    render(&mut r, 30);
    e.loop_in("A");
    let start = pos(&e);
    render(&mut r, 20);
    e.loop_out("A");
    let end = start + 20.0 * BLOCK as f64;
    for _ in 0..50 {
        render(&mut r, 7);
        let p = pos(&e);
        assert!(
            p >= start - BLOCK as f64 && p < end + BLOCK as f64,
            "{start} <= {p} < {end}"
        );
    }
    e.loop_halve("A");
    assert!(e.deck("A").unwrap().sample.as_ref().unwrap().looping);
    e.loop_out("A"); // while looping: exit
    render(&mut r, 100);
    assert!(pos(&e) > end + 10_000.0, "plays on past the loop");
}

const INPULSE: &str = "DJControl Inpulse 200 Mk3";

/// The Inpulse on a sample deck: the platter scratches the turntable, the transport and cue
/// pads land on it.
#[test]
fn the_inpulse_scratches_a_sample_deck() {
    let (e, mut r) = demo_deck();
    let mut rig = Rig::new(e);
    assert!(rig.attach(INPULSE));
    rig.engine.play("A");
    render(&mut r, 150);
    let before = pos(&rig.engine);
    let mut t = 1.0;
    let mut step = |rig: &mut Rig, r: &mut Renderer, bytes: &[u8]| {
        t += 0.002;
        rig.route(INPULSE, bytes, t);
        rig.tick(t);
        render(r, 1);
    };
    step(&mut rig, &mut r, &[0x91, 8, 127]); // touch
    for _ in 0..124 {
        step(&mut rig, &mut r, &[0xB1, 10, 0x7F]); // half a turn back: 0.9 s
    }
    render(&mut r, 60);
    let back = pos(&rig.engine);
    // the touch stops the record where it was; half a turn back is 0.9 s of it
    let held = before - 0.9 * SR;
    assert!(
        (back - held).abs() < 0.05 * SR,
        "{before} -> {back}, want ~{held}"
    );
    render(&mut r, 50);
    assert!(
        (pos(&rig.engine) - back).abs() < 1.0,
        "a held record stays put"
    );
    step(&mut rig, &mut r, &[0x91, 8, 0]); // let go
    render(&mut r, 100);
    assert!(pos(&rig.engine) > back + 40_000.0, "plays on");
    assert_eq!(rig.engine.dropped, 0);
}

#[test]
fn composing_takes_the_deck_back_from_the_platter() {
    let (mut e, mut r) = demo_deck();
    e.play("A");
    render(&mut r, 20);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
    render(&mut r, 1);
    let d = e.deck("A").unwrap();
    assert!(d.sample.is_none() && !d.mirror.is_sample_mode());
    e.play("A");
    assert!(render(&mut r, 200) > 0.001, "the net plays");
    assert!(e.load_sample("A", PreparedSample::demo(SR)));
    render(&mut r, 1);
    assert!(e.deck("A").unwrap().mirror.is_sample_mode());
    // the old demo buffer comes back to the main thread to be freed
    e.drain_retired();
}

// (it was refused once: Swift's turntable cannot hold another rate, and the engine converts instead,
// see "audio at another sample rate" below)
#[test]
fn audio_at_another_rate_is_converted_not_refused() {
    let (mut e, _r) = demo_deck();
    assert!(e.load_sample("A", PreparedSample::demo(44_100.0)));
    let held = e.deck("A").unwrap().sample.as_ref().unwrap();
    assert_eq!(held.buffer.sample_rate, SR);
    assert_eq!(
        held.buffer.length(),
        (2.0 * 4.0 * 60.0 / 120.0 * SR) as usize
    );
}

#[cfg(feature = "decode")]
#[test]
fn a_wav_file_decodes_and_resamples() {
    // 0.5 s of a 440 Hz sine, 16-bit stereo at 24 kHz
    let (rate, n) = (24_000u32, 12_000usize);
    let mut data = Vec::new();
    for i in 0..n {
        let v = ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 16_000.0) as i16;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&(v / 2).to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    let dir = std::env::temp_dir().join(format!("djhost-decode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.wav");
    std::fs::write(&path, wav).unwrap();
    let b = djrust::host::decode::decode_file(&path, SR).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(b.length(), 24_000, "0.5 s at 48 kHz");
    assert!(b.right.is_some(), "stereo kept");
    let peak_l = b.left.iter().fold(0f32, |m, v| m.max(v.abs()));
    let peak_r = b.right().iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!((peak_l - 16_000.0 / 32_768.0).abs() < 0.01, "{peak_l}");
    assert!((peak_r - peak_l / 2.0).abs() < 0.01, "{peak_r}");
    assert_eq!(djrust::host::decode::title_of(&path), "tone");
}

// ---- audio at another sample rate ------------------------------------------------------------
//
// Swift's `Turntable.load` takes no other rate (`precondition(buffer.sampleRate == sampleRate,
// "resample before loading")`): `Deck.decode` converts a file to the engine's rate, and the tempo
// is measured on that. Audio that reaches the engine at another rate is converted the same way
// when it loads (`decode::resample`, the converter the file path uses, so a file and a buffer give
// the same audio), instead of being refused. Form (a) from the Swift source: no Swift test loads
// a buffer at the wrong rate (it would trap); the tests of `Deck.decode`'s own resampling are
// `LoopLibraryTests`' (`loop_library.rs`).

use djrust::deck::beat_grid::{analyze, BeatGrid};
use djrust::deck::turntable::SampleBuffer;
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::host::deck::{Applied, PerformCtx};
use std::sync::Arc;

/// `seconds` of a 440 Hz tone at `rate`, mono or stereo (the right a quarter the level).
fn tone(rate: f64, seconds: f64, stereo: bool) -> SampleBuffer {
    let n = (rate * seconds) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate).sin() as f32 * 0.5)
        .collect();
    let r = stereo.then(|| l.iter().map(|v| v * 0.25).collect());
    SampleBuffer::new(l, r, rate)
}

/// Zero crossings per second of `x` at `rate`, rising edges.
fn hz(x: &[f32], rate: f64) -> f64 {
    let n = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    n as f64 / (x.len() as f64 / rate)
}

fn render_left(r: &mut Renderer, blocks: usize) -> Vec<f32> {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::new();
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        out.extend_from_slice(&l);
    }
    out
}

#[test]
fn a_prepared_sample_at_another_rate_loads_converted_and_plays_in_tune() {
    for (rate, stereo) in [(44_100.0, false), (96_000.0, true), (22_050.0, true)] {
        let (mut e, mut r) = {
            let mut c = EngineConfig::new(SR);
            c.extended = false;
            engine(c)
        };
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        let src = tone(rate, 4.0, stereo);
        let grid = BeatGrid::new(120.0, 4_410.0, 0.8);
        let prepared = PreparedSample::with_grid(Arc::new(src.clone()), "tone", Some(grid));
        assert!(e.load_sample("A", prepared), "{rate}: it loads");
        let held = e.deck("A").unwrap().sample.as_ref().unwrap();
        assert_eq!(held.buffer.sample_rate, SR, "{rate}");
        // exactly what the decode path makes of it
        let want = djrust::host::decode::resample(&src.left, rate, SR);
        assert_eq!(held.buffer.left, want, "{rate}: the file path's converter");
        assert_eq!(
            held.buffer.length(),
            (src.length() as f64 * SR / rate).round() as usize
        );
        assert_eq!(held.buffer.right.is_some(), stereo);
        if stereo {
            assert_eq!(
                held.buffer.right(),
                djrust::host::decode::resample(src.right(), rate, SR)
            );
        }
        // the grid is counted in the buffer's samples: its offset moved with them, its tempo did not
        let g = held.grid.unwrap();
        assert_eq!((g.bpm, g.confidence), (120.0, 0.8));
        assert_eq!(g.offset, 4_410.0 * SR / rate, "{rate}");
        // and it sounds at pitch: a tone left at its own rate would play 440 * rate / SR
        e.play("A");
        render_left(&mut r, 20); // the motor spins up
        let heard = hz(&render_left(&mut r, 100), SR);
        assert!((heard - 440.0).abs() < 8.0, "{rate}: {heard} Hz");
    }
}

#[test]
fn a_load_action_converts_audio_at_another_rate_and_measures_the_tempo_on_what_is_loaded() {
    let click_track = |rate: f64| {
        // a click every half second (120 bpm): 8 s
        let n = (rate * 8.0) as usize;
        let mut l = vec![0f32; n];
        for k in 0..16 {
            let at = (rate * 0.5 * k as f64) as usize + 100;
            for j in 0..((rate * 0.02) as usize).min(n - at) {
                l[at + j] = 0.9 * (1.0 - j as f32 / (rate * 0.02) as f32);
            }
        }
        SampleBuffer::new(l, None, rate)
    };
    let src = Arc::new(click_track(44_100.0));
    let load = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Sample {
            id: String::new(),
            title: "clicks".into(),
            grid: None,
        },
    };
    // not deterministic: the tempo is measured, here on the converted audio
    let (mut e, _r) = {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        engine(c)
    };
    e.deterministic = false;
    let applied = e.perform(
        &load,
        PerformCtx {
            prepared: None,
            buffer: Some(src.clone()),
        },
    );
    assert_eq!(applied, Applied::Done);
    let held = e.deck("A").unwrap().sample.as_ref().unwrap().buffer.clone();
    assert_eq!((held.sample_rate, held.length()), (SR, 8 * 48_000));
    let measured = analyze(&held.left, held.right(), SR).expect("a tempo");
    assert!((measured.bpm - 120.0).abs() < 1.5, "{}", measured.bpm);
    assert_eq!(
        e.deck("A").unwrap().sample.as_ref().unwrap().grid,
        Some(measured)
    );

    // a recorded grid is in the loaded samples already (Swift's grid is of the decoded buffer):
    // it is taken as it is, not moved again
    let recorded = BeatGrid::new(120.0, 2_400.0, 1.0);
    let (mut e2, _r2) = {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        engine(c)
    };
    e2.perform(
        &DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Sample {
                id: String::new(),
                title: "clicks".into(),
                grid: Some(recorded),
            },
        },
        PerformCtx {
            prepared: None,
            buffer: Some(src),
        },
    );
    assert_eq!(
        e2.deck("A").unwrap().sample.as_ref().unwrap().grid,
        Some(recorded)
    );

    // and by name, through the host's resolver: the same
    let by_name = Arc::new(click_track(32_000.0));
    let (mut e3, _r3) = {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        engine(c)
    };
    let held_out = by_name.clone();
    e3.sample_resolver = Some(Box::new(move |id| {
        (id == "clicks-32k").then(|| held_out.clone())
    }));
    let applied = e3.perform(
        &DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Sample {
                id: "clicks-32k".into(),
                title: "clicks".into(),
                grid: None,
            },
        },
        PerformCtx::default(),
    );
    assert_eq!(applied, Applied::Done);
    let held = e3
        .deck("A")
        .unwrap()
        .sample
        .as_ref()
        .unwrap()
        .buffer
        .clone();
    assert_eq!(held.sample_rate, SR);
    assert_eq!(
        held.left,
        djrust::host::decode::resample(&by_name.left, 32_000.0, SR)
    );
    assert_eq!(
        e3.sample_id(&held),
        "clicks-32k",
        "known by the name it was asked for"
    );
}
