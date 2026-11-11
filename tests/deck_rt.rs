//! The deck's render path holds to the Swift audio-thread rules, measured, not asserted
//! (`WaveRunnerRealTimeTests`, `MasterChainRealTimeTests`, `FxUnitRealTimeTests`): across
//! project swaps, macros, FX pokes, mutes and the vinyl brake, `Renderer::render_block`
//! allocates nothing and frees nothing.
//!
//! The counter is per thread (a const thread-local, so it never allocates itself). The main
//! side's work (compiling a project, sending commands) happens outside the counted windows;
//! the commands are popped and applied inside them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use djrust::fx::unit::{FxParam, FxUnitCommand};
use djrust::fx::{MasterFxKey, MasterMacro, StripKnob};
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::wave::NetMacro;
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.with(|o| o.get()) {
            FREES.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// (allocations, frees) made by `f` on this thread.
fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    (r, ALLOCS.with(|c| c.get()), FREES.with(|c| c.get()))
}

const BLOCK: usize = 128;

fn run(r: &mut Renderer, blocks: usize, l: &mut [f32], rr: &mut [f32]) {
    for _ in 0..blocks {
        r.render_block(l, rr);
    }
}

fn measured(what: &str, r: &mut Renderer, blocks: usize, l: &mut [f32], rr: &mut [f32]) {
    let (_, a, f) = count(|| run(r, blocks, l, rr));
    assert_eq!(
        (a, f),
        (0, 0),
        "{what}: render allocated {a} times and freed {f} times"
    );
}

fn deck_a(engine: &mut Engine, seed: f64) {
    let spec = TrackSpec::new("techno", seed, Some("minimal"));
    assert!(engine.generate("A", &spec));
}

#[test]
fn render_allocates_and_frees_nothing() {
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    e.set_tempo("A", 480.0); // dense ticks: many fires, control nets, section changes
    e.play("A");
    // warm-up: first touches of lazily initialised state
    run(&mut r, 400, &mut l, &mut rr);
    assert!(
        r.deck("A").unwrap().mirror.tick() > 16,
        "the run crosses many ticks"
    );

    measured("steady", &mut r, 3000, &mut l, &mut rr);

    // a second song while the first plays: compiled outside, swapped in at a bar, the old one
    // faded out and retired
    deck_a(&mut e, 43.0);
    measured("swap", &mut r, 1500, &mut l, &mut rr);
    measured("swap tail", &mut r, 3000, &mut l, &mut rr);
    let before = e.retired;
    e.drain_retired();
    assert!(
        e.retired > before,
        "the old song came back through the retire ring"
    );

    // master macros, a slider, the FX unit, net macros
    e.fire_master("A", MasterMacro::PhaserDrone, None);
    e.fire_master("A", MasterMacro::OctaveDown, None);
    e.fire_master("A", MasterMacro::BuildCrush, None);
    e.fire_master("A", MasterMacro::SweepLp, Some(1));
    e.set_master_fx("A", MasterFxKey::Distortion, 40);
    e.set_fx("A", FxParam::Filter, -0.6);
    e.set_fx("A", FxParam::PhaserMix, 0.5);
    e.set_fx("A", FxParam::Drive, 0.4);
    e.fire("A", NetMacro::Drop, None);
    e.fire("A", NetMacro::BeatRepeat, None);
    measured("macros and fx", &mut r, 1200, &mut l, &mut rr);
    e.fx_kill("A", true);
    measured("fx kill", &mut r, 100, &mut l, &mut rr);
    e.fx_reset("A");
    e.panic("A");
    measured("panic", &mut r, 400, &mut l, &mut rr);

    // mixer, mutes, stingers, tempo
    e.set_crossfader(0.3);
    e.set_volume("A", 0.7);
    e.set_gain("A", 0.6);
    e.set_strip("A", djrust::fx::StripKnob::Filter, 0.2);
    e.set_strip("A", djrust::fx::StripKnob::EqLow, 0.0);
    e.set_strip("A", djrust::fx::StripKnob::EqMid, 0.7);
    e.set_strip("A", djrust::fx::StripKnob::EqHigh, 0.9);
    e.set_pfl("A", true);
    e.set_role_muted("A", "bass", true);
    e.set_mute("A", "hihat", true);
    e.fire_stinger("A", "hit1", 16);
    e.set_tempo("A", 300.0);
    measured("mixer", &mut r, 600, &mut l, &mut rr);

    // the vinyl brake: varispeed, then the clock catches up
    e.fire_master("A", MasterMacro::VinylBrake, Some(1));
    measured("brake", &mut r, 2500, &mut l, &mut rr);

    // transport
    e.stop("A");
    measured("stop", &mut r, 100, &mut l, &mut rr);
    e.play("A");
    measured("restart", &mut r, 600, &mut l, &mut rr);

    // a hand on the platter: backwards through the history, forwards, let go
    e.jog_touch("A", true);
    e.jog_scrub("A", -1.5);
    measured("scratch back", &mut r, 400, &mut l, &mut rr);
    e.jog_scrub("A", 2.5);
    measured("scratch forward", &mut r, 400, &mut l, &mut rr);
    e.jog_touch("A", false);
    measured("release", &mut r, 400, &mut l, &mut rr);

    // the turntable: a loop loaded over the net (the net's tail retired), played, cued,
    // looped, scratched, then a second load retiring the first buffer
    e.load_demo("A");
    e.play("A");
    measured("turntable", &mut r, 400, &mut l, &mut rr);
    e.hot_cue("A", 0);
    e.loop_beats("A", 1.0);
    e.nudge("A", 0.04);
    measured("turntable loop", &mut r, 400, &mut l, &mut rr);
    e.jog_touch("A", true);
    e.jog_scrub("A", -0.7);
    measured("turntable scratch", &mut r, 300, &mut l, &mut rr);
    e.jog_touch("A", false);
    e.fire_master("A", MasterMacro::SweepLp, Some(1));
    measured("turntable fx", &mut r, 400, &mut l, &mut rr);
    e.load_demo("A");
    measured("turntable reload", &mut r, 100, &mut l, &mut rr);

    // LOOP record on the playing deck (main-thread work between blocks, not counted), the
    // take looping on the loop channel, a second loop grabbed, all out on the bar
    e.play("A");
    run(&mut r, 50, &mut l, &mut rr);
    assert!(e.loop_record("A", true, None));
    for _ in 0..1200 {
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    e.loop_record("A", false, None);
    for _ in 0..2000 {
        if !e.is_recording_loop("A") {
            break;
        }
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    assert_eq!(e.loops.row.clips.len(), 1, "the take is on the row");
    run(&mut r, 100, &mut l, &mut rr);
    measured("loop channel", &mut r, 400, &mut l, &mut rr);
    assert!(e.grab_last_bars("A", 1).is_some());
    measured("two loops", &mut r, 400, &mut l, &mut rr);
    e.set_loop_volume(0.5);
    e.stop_all_loops();
    measured("loops out", &mut r, 800, &mut l, &mut rr);
    e.service_loops();
    let id = e.loops.row.clips[0].id.clone();
    e.set_looping(&id, true);
    measured("loop back", &mut r, 800, &mut l, &mut rr);
    // the loop channel's strip: EQ, filter, mute
    e.set_loop_strip(djrust::fx::StripKnob::EqLow, 0.0);
    e.set_loop_strip(djrust::fx::StripKnob::Filter, 0.2);
    measured("loop strip", &mut r, 200, &mut l, &mut rr);
    e.set_loop_mute(true);
    measured("loop mute", &mut r, 200, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// The loop bank with everything the loop slice put on its audio path (`LoopFxRealTimeTests`,
/// `LoopMasterRealTimeTests`): clip FX changing on loops that play (mode swaps through dry,
/// smoothed drive / tone / mix, on and off, the limiter engaging and letting go), every master
/// macro fired on L with sliders, the FX unit, the vinyl brake through its release and a panic
/// in the middle of one, mute and solo, and a key-locked clip swapping to its stretched copy and
/// back. The pokes are the main thread's and sit outside the counted windows; only the renders
/// are measured.
#[test]
fn loop_fx_master_bus_and_key_lock_allocate_and_free_nothing() {
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.loops.deterministic = true; // the stretched copy renders inline (main thread, uncounted)
    assert!(e.load_demo("A"));
    e.play("A");
    run(&mut r, 50, &mut l, &mut rr);
    // two takes of the demo loop, looping on L
    assert!(e.loop_record("A", true, Some(1)));
    for _ in 0..200 {
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    e.loop_record("A", false, None);
    for _ in 0..4000 {
        if !e.is_recording_loop("A") {
            break;
        }
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    run(&mut r, 3000, &mut l, &mut rr);
    assert!(e.grab_last_bars("A", 1).is_some() || e.loops.row.clips.len() == 1);
    for _ in 0..200 {
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    let ids: Vec<String> = e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
    assert!(!ids.is_empty());
    for id in &ids {
        assert_eq!(
            e.loop_state(id),
            djrust::host::deck::loops::LoopState::On,
            "the takes loop on L"
        );
    }
    run(&mut r, 200, &mut l, &mut rr); // warm

    // the measured windows below really run the chain: FX in the path, the limiter engaged
    e.set_clip_fx(&ids[0], &LoopFx::presets()[3].1, None);
    measured("loop fx on", &mut r, 96, &mut l, &mut rr);
    assert!(
        e.loops.mirror.fx_running(0),
        "the Fuzz chain is in the path"
    );
    e.set_clip_fx(&ids[0], &LoopFx::default(), None);
    measured("loop fx off", &mut r, 96, &mut l, &mut rr);
    assert!(!e.loops.mirror.fx_running(0), "and out of it again");

    // clip FX: the presets and corners, on some clips each round
    let mut settings: Vec<LoopFx> = LoopFx::presets().into_iter().map(|(_, f)| f).collect();
    settings.extend([
        LoopFx {
            drive: 24.0,
            auto_level: false,
            ..LoopFx::default()
        }, // the limiter holds the channel
        LoopFx {
            drive: 24.0,
            mode: LoopFxMode::Fold,
            amount: 1.0,
            tone: 0.1,
            ..LoopFx::default()
        },
        LoopFx {
            mode: LoopFxMode::Crush,
            amount: 1.0,
            bits: 3.0,
            downsample: 16,
            ..LoopFx::default()
        },
        LoopFx {
            drive: 12.0,
            mode: LoopFxMode::Tube,
            amount: 0.9,
            mix: 0.5,
            trim: -6.0,
            ..LoopFx::default()
        },
        LoopFx {
            bypass: true,
            drive: 24.0,
            mode: LoopFxMode::Hard,
            ..LoopFx::default()
        },
        LoopFx::default(),
    ]);
    for round in 0..3 {
        for (k, fx) in settings.iter().enumerate() {
            for (i, id) in ids.iter().enumerate() {
                if (i + k + round) % 2 == 0 || k == settings.len() - 1 {
                    e.set_clip_fx(id, fx, None);
                }
            }
            measured("loop fx", &mut r, 96, &mut l, &mut rr);
            e.service_loops();
        }
    }
    assert!(ids.iter().all(|id| e.clip_fx(id) == LoopFx::default()));

    // mute, solo and levels
    e.set_clip_mute(&ids[0], true);
    measured("mute", &mut r, 80, &mut l, &mut rr);
    e.set_clip_solo(&ids[0], true);
    e.set_clip_mute(&ids[0], false);
    e.set_clip_gain(&ids[0], 0.4, None);
    measured("solo and level", &mut r, 80, &mut l, &mut rr);
    e.set_clip_solo(&ids[0], false);
    measured("solo off", &mut r, 80, &mut l, &mut rr);

    // every master macro on L (the next launch line, then at once), sliders, the FX unit, panics
    for round in 0..2 {
        for (i, m) in MasterMacro::ALL.iter().enumerate() {
            e.fire_loop_master(*m, if i % 3 == 0 { Some(1) } else { None }, round == 1);
            if i % 4 == 1 {
                let key = MasterFxKey::ALL[i % MasterFxKey::ALL.len()];
                e.set_loop_master_slider(key, i as i32 * 7);
            }
            if i == 5 {
                e.send_loop_fx_unit(FxUnitCommand::Set {
                    param: FxParam::Filter.index() as u8,
                    value: -0.5,
                    snap: false,
                });
            }
            measured("loop master macro", &mut r, 24, &mut l, &mut rr);
            if round == 1 && i == 0 {
                assert!(e.is_loop_master_active(*m), "the first macro runs on L");
            }
            e.service_loops();
            if i % 5 == 4 {
                e.loop_master_panic();
                measured("loop master panic", &mut r, 6, &mut l, &mut rr);
            }
        }
        // the brake through its release, and a panic in the middle of one
        e.fire_loop_master(MasterMacro::VinylBrake, Some(1), true);
        measured("loop brake", &mut r, 800, &mut l, &mut rr);
        e.service_loops();
        e.fire_loop_master(MasterMacro::VinylBrake, Some(2), true);
        measured("loop brake 2", &mut r, 120, &mut l, &mut rr);
        e.loop_master_panic();
        measured("loop brake panic", &mut r, 120, &mut l, &mut rr);
        e.service_loops();
    }
    e.loop_master_panic();
    measured("loop master settle", &mut r, 400, &mut l, &mut rr);

    // the channel's strip (EQ and DJ filter in front of its fader): every knob swept across its
    // dead zone and back under the loops, one at a time and then all together, each move a
    // coefficient change on the render thread; and the loop clock read between the blocks (the
    // main thread's, outside the counted windows) while the decks keep writing their anchors
    let knobs = [
        StripKnob::EqLow,
        StripKnob::EqMid,
        StripKnob::EqHigh,
        StripKnob::Filter,
    ];
    for (k, knob) in knobs.into_iter().enumerate() {
        for (j, v) in [0.0, 0.3, 0.5, 0.8, 1.0, 0.45, 0.2, 0.5]
            .into_iter()
            .enumerate()
        {
            e.set_loop_strip(knob, v);
            let _ = e.loop_clock(Some("a clip"), Some("A"));
            measured("loop strip", &mut r, 8 + (k + j) % 5, &mut l, &mut rr);
        }
    }
    for round in 0..40 {
        for (k, knob) in knobs.into_iter().enumerate() {
            e.set_loop_strip(knob, ((round * 7 + k * 3) % 11) as f64 / 10.0);
        }
        measured("loop strip, all four moving", &mut r, 3, &mut l, &mut rr);
    }
    for knob in knobs {
        e.set_loop_strip(knob, 0.5);
    }
    measured("loop strip at rest", &mut r, 16, &mut l, &mut rr);

    // key lock: another tempo, a stretched copy rendered (inline, uncounted) and swapped in, then
    // unlocked and swapped back
    let id = ids[0].clone();
    e.nudge("A", 0.04);
    run(&mut r, 100, &mut l, &mut rr);
    e.set_clip_key_lock(&id, true);
    for _ in 0..4000 {
        if e.is_clip_stretched(&id) {
            break;
        }
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    assert!(e.is_clip_stretched(&id), "the copy swapped in");
    measured("key lock swap", &mut r, 200, &mut l, &mut rr);
    e.set_clip_fx(&id, &settings[3], None);
    measured("key lock with fx", &mut r, 200, &mut l, &mut rr);
    e.set_clip_key_lock(&id, false);
    measured("key lock off", &mut r, 200, &mut l, &mut rr);
    assert!(!e.is_clip_stretched(&id));

    e.stop_all_loops();
    measured("loops out", &mut r, 800, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

// ---- the runner's performance surface, through a DeckCore ------------------------------

fn bind_slot_3(_instrument: &str, _part: &str) -> Option<usize> {
    Some(3)
}

fn measured_level(_slot: usize, _group: &str) -> Option<f64> {
    Some(-4.0)
}

/// A deck core playing a `genre` track with the room, its shimmer and the echo on (the sends the
/// runner applies), loaded and playing.
fn core_for(genre: &str, seed: f64, sampled: bool) -> djrust::host::deck::DeckCore {
    use djrust::musicality::echo::EchoSpec;
    use djrust::musicality::fixes::Fixes;
    use djrust::musicality::space::SpaceSpec;
    use djrust::wave::{WaveOptions, WaveRunner};
    use djrust::host::deck::{Command, DeckCore, DeckMirror};
    use std::sync::Arc;

    let sr = 48_000.0;
    let opts = WaveOptions {
        musicality: Fixes::SPACE | Fixes::SHIMMER | Fixes::ECHO | Fixes::BALANCE,
        space: Some(SpaceSpec::for_genre("trance", true)),
        echo: EchoSpec::for_genre("trance"),
        sample_binder: sampled.then_some(bind_slot_3 as djrust::wave::runner::SampleBinder),
        part_level: sampled.then_some(measured_level as djrust::wave::PartLevelFn),
        bank_rate: sr,
        ..WaveOptions::extended()
    };
    let mut ov = djrust::JsonObject::new();
    ov.set("seed", seed.into());
    ov.set("structure", "minimal".into());
    let project = djrust::compose(genre, &ov.into());
    let mut core = DeckCore::new(
        "A",
        sr,
        opts.clone(),
        false,
        Arc::new(DeckMirror::default()),
    );
    core.apply(Command::Load(WaveRunner::prepare(&project, sr, &opts, 0)));
    core.apply(Command::Tempo(480.0));
    core.apply(Command::Play);
    core
}

/// A deck core with the buffers it renders into (made outside the counted windows).
struct Rig {
    core: djrust::host::deck::DeckCore,
    l: Vec<f32>,
    r: Vec<f32>,
    frame: i64,
}

impl Rig {
    fn new(genre: &str, seed: f64, sampled: bool) -> Rig {
        Rig {
            core: core_for(genre, seed, sampled),
            l: vec![0f32; BLOCK],
            r: vec![0f32; BLOCK],
            frame: 0,
        }
    }

    fn blocks(&mut self, n: usize) {
        for _ in 0..n {
            self.core.render(&mut self.l, &mut self.r, self.frame);
            self.core.runner.sample_keys.clear(); // (the host's drain)
            self.frame += BLOCK as i64;
        }
    }

    fn measured(&mut self, what: &str, n: usize) {
        let (_, a, f) = count(|| self.blocks(n));
        assert_eq!(
            (a, f),
            (0, 0),
            "{what}: render allocated {a} times and freed {f} times"
        );
    }
}

/// Live keys (struck strings and wavetables), a held chord stolen from, the room, its shimmer
/// and the echo with the FX unit's gains moving on them, a sampled live voice's MIDI, the
/// sampled instruments' send fifos and `silence`, over thousands of blocks: the runner's
/// performance surface allocates nothing and frees nothing (`WaveDeckSourceTests
/// .pluckedStringsDoNotAllocate`, `EchoRealTimeTests`, `SpaceRealTimeTests`).
#[test]
fn live_keys_and_sends_allocate_and_free_nothing() {
    let mut rig = Rig::new("trance", 42.0, false);
    rig.blocks(400);
    assert!(rig.core.runner.graph().unwrap().tick > 16);
    rig.measured("steady, with the room and the echo", 3000);

    // live keys: a chord of more than eight (the quietest voice is taken over), released one
    // by one, then all at once
    let (_, a, f) = count(|| {
        for (i, n) in [48, 52, 55, 60, 64, 67, 72, 76, 79, 83]
            .into_iter()
            .enumerate()
        {
            rig.core.runner.note_on(n, 70 + i as i64 * 5);
            rig.blocks(20);
        }
        rig.core.runner.note_off(52);
        rig.core.runner.note_off(999);
        rig.blocks(200);
        rig.core.runner.all_notes_off();
        rig.blocks(200);
    });
    assert_eq!((a, f), (0, 0), "live keys: {a} allocations, {f} frees");

    // the FX unit's gains on the room and the echo, ramped over blocks of different lengths
    let (_, a, f) = count(|| {
        rig.core.runner.set_unit_sends(0.0, 0.0, 0.0, BLOCK);
        rig.blocks(100);
        rig.core.runner.set_unit_sends(1.8, 2.5, 0.6, 8 * BLOCK);
        rig.core.runner.note_on(72, 100);
        rig.blocks(300);
        rig.core.runner.note_off(72);
        rig.core.runner.set_unit_sends(1.0, 1.0, 0.0, BLOCK);
        rig.blocks(600);
    });
    assert_eq!((a, f), (0, 0), "unit sends: {a} allocations, {f} frees");

    // the sampled instruments' sends waiting for the room and the echo
    let burst = vec![0.25f32; BLOCK];
    let (_, a, f) = count(|| {
        for _ in 0..200 {
            rig.core.runner.queue_sampler_send(&burst);
            rig.core.runner.queue_sampler_echo(&burst);
            rig.blocks(1);
        }
        // far more than the fifos hold: what does not fit is dropped
        for _ in 0..100 {
            rig.core.runner.queue_sampler_send(&burst);
            rig.core.runner.queue_sampler_echo(&burst);
        }
        rig.blocks(400);
    });
    assert_eq!((a, f), (0, 0), "sampler sends: {a} allocations, {f} frees");

    // a hand on the platter with the room ringing, and a silence
    let (_, a, f) = count(|| {
        rig.core.runner.freeze();
        rig.core.runner.scrub(-6.0);
        rig.blocks(200);
        rig.core.runner.scrub(10.0);
        rig.core.runner.note_on(60, 90);
        rig.blocks(200);
        rig.core.runner.release();
        rig.core.runner.silence();
        rig.blocks(400);
    });
    assert_eq!(
        (a, f),
        (0, 0),
        "scratch and silence: {a} allocations, {f} frees"
    );

    // live keys strike a string too (country plays guitars)
    let mut guitar = Rig::new("country", 7.0, false);
    guitar.blocks(200);
    assert!(
        guitar
            .core
            .runner
            .project()
            .unwrap()
            .lanes()
            .iter()
            .any(|l| l.is_pluck()),
        "a plucked lane to measure"
    );
    let (_, a, f) = count(|| {
        guitar.blocks(3000);
        guitar.core.runner.note_on(52, 100);
        guitar.core.runner.note_on(40, 90);
        guitar.blocks(200);
        guitar.core.runner.all_notes_off();
        guitar.blocks(100);
    });
    assert_eq!(
        (a, f),
        (0, 0),
        "plucked live keys: {a} allocations, {f} frees"
    );
}

/// A live voice on a sampler queues MIDI, within the queue's capacity, without allocating
/// (a full queue drops the message).
#[test]
fn a_sampled_live_voice_allocates_nothing() {
    let mut rig = Rig::new("trance", 42.0, true);
    rig.blocks(400);
    assert!(
        rig.core.runner.project().unwrap().live_lane().sample_slot() == Some(3),
        "the live voice plays the sampler slot"
    );
    let (_, a, f) = count(|| {
        for k in 0..3000 {
            // (never drained inside the loop: the queue fills, and then drops)
            rig.core.runner.note_on(40 + (k % 40), 100);
            rig.core.runner.note_off(40 + ((k + 7) % 40));
        }
        rig.core.runner.all_notes_off();
        rig.blocks(200);
    });
    assert_eq!(
        (a, f),
        (0, 0),
        "sampled live keys: {a} allocations, {f} frees"
    );
}

/// A deck core playing `genre`'s track on the extended options shaped by `shape`.
fn shaped_core(
    genre: &str,
    structure: &str,
    seed: f64,
    shape: impl FnOnce(&mut djrust::wave::WaveOptions),
    tweak: impl FnOnce(&mut djrust::pflow::Project),
) -> djrust::host::deck::DeckCore {
    use djrust::wave::{WaveOptions, WaveRunner};
    use djrust::host::deck::{Command, DeckCore, DeckMirror};
    use std::sync::Arc;

    let sr = 48_000.0;
    let mut opts = WaveOptions::extended();
    shape(&mut opts);
    let mut ov = djrust::JsonObject::new();
    ov.set("seed", seed.into());
    ov.set("structure", structure.into());
    let mut project = djrust::compose(genre, &ov.into());
    tweak(&mut project);
    let mut core = DeckCore::new(
        "A",
        sr,
        opts.clone(),
        false,
        Arc::new(DeckMirror::default()),
    );
    core.apply(Command::Load(WaveRunner::prepare(&project, sr, &opts, 0)));
    core.apply(Command::Tempo(480.0));
    core.apply(Command::Play);
    core
}

/// The musicality voice shaping the runner applies per sample (`.duck`, ...): a song with each
/// on renders without allocating or freeing, through section changes (tempo 480 plays a
/// minimal song through in seconds, and the project is installed anew when it ends).
#[test]
fn the_musicality_voice_shaping_allocates_and_frees_nothing() {
    use djrust::wave::{WaveOptions, WaveProject};
    type Shape = fn(&mut WaveOptions);
    // what each shaping is, how to turn it on, and how to tell it is on in the prepared project
    // (a measurement of a feature that is not running would pass)
    type Active = fn(&WaveProject) -> bool;
    let shapes: Vec<(&str, &str, &str, Shape, Active)> = vec![
        (
            "duck",
            "techno",
            "minimal",
            |o| o.duck_depth = 0.5,
            |p| {
                p.duck_depth > 0.0
                    && p.lanes().iter().any(|l| l.ducks())
                    && p.lanes().iter().any(|l| l.keys_the_duck())
            },
        ),
        (
            // (with a right channel: the stereo pair is what the app renders)
            "energy",
            "trance",
            "minimal",
            |o| o.musicality |= djrust::musicality::fixes::Fixes::ENERGY,
            |p| p.energy.is_some(),
        ),
        (
            "subtractive, unison and sweep",
            "trance",
            "minimal",
            |o| {
                use djrust::musicality::fixes::Fixes;
                o.musicality |= Fixes::SUBTRACTIVE | Fixes::UNISON | Fixes::SWEEP | Fixes::ENERGY;
                o.voice_design =
                    djrust::musicality::synth_voice::VoiceDesign::for_genre("trance");
            },
            |p| p.lanes().iter().any(|l| l.filter_envelope().0 > 0.0),
        ),
        (
            // the sub-octave's phases are allocated at prepare
            "sub-octave",
            "dnb",
            "minimal",
            |o| {
                o.musicality |= djrust::musicality::fixes::Fixes::SUB_OCTAVE;
                o.sub_octave = true;
            },
            |p| p.lanes().iter().any(|l| l.has_sub_octave()),
        ),
        (
            // the stereo picture on top of everything the app plays a trance song with: the
            // side signal beside the mid, the wide unison pairs, the returns' side, and the
            // energy arc's wide path (`pre_s`, `tone_s`)
            "stereo",
            "trance",
            "minimal",
            |o| {
                use djrust::musicality::fixes::Fixes;
                o.musicality |= Fixes::STEREO
                    | Fixes::UNISON
                    | Fixes::SUBTRACTIVE
                    | Fixes::SPACE
                    | Fixes::SHIMMER
                    | Fixes::ECHO
                    | Fixes::ENERGY
                    | Fixes::DUCK
                    | Fixes::BALANCE;
                *o = o.for_genre("trance");
            },
            |p| {
                p.lanes().iter().any(|l| l.placement().1)
                    && p.lanes().iter().any(|l| l.placement().0 != 0.0)
                    && p.space.is_some()
                    && p.echo.is_some()
                    && p.energy.is_some()
            },
        ),
        (
            // the riser's filters and noise are made at prepare; it runs per sample in the
            // window (an edm song of 800+ steps reaches its buildup at tempo 480 in seconds)
            "riser",
            "edm",
            "extended",
            |o| {
                use djrust::musicality::fixes::Fixes;
                o.musicality |= Fixes::RISER | Fixes::STEREO | Fixes::SPACE;
                *o = o.for_genre("edm");
            },
            |p| p.riser.is_some(),
        ),
        (
            // the parts' trims are measured at prepare and play as a gain per lane
            "balance",
            "techno",
            "minimal",
            |o| o.musicality |= djrust::musicality::fixes::Fixes::BALANCE,
            |p| p.balance.is_some(),
        ),
    ];
    for (what, genre, structure, shape, active) in shapes {
        let mut rig = Rig {
            core: shaped_core(genre, structure, 42.0, shape, |_| {}),
            l: vec![0f32; BLOCK],
            r: vec![0f32; BLOCK],
            frame: 0,
        };
        rig.blocks(400);
        assert!(rig.core.runner.graph().unwrap().tick > 16, "{what}");
        assert!(active(rig.core.runner.project().unwrap()), "{what} is off");
        rig.measured(what, 4000);
    }
}

/// The drums the pass writes and the runner voices (fills, the section crash, open hats chokes by
/// the closed hat, the tilt on the noise drums, the brush) and the band the section plan moves
/// (`.sectionHarmony`, `.harmonicRhythm`: through a scratch back over a section edge too): no
/// allocation and no free on the render thread (`DropGapRealTimeTests.gapsAndChokesDoNotAllocate`,
/// `SectionTransposeRealTimeTests`).
#[test]
fn voiced_drums_and_the_section_plan_allocate_and_free_nothing() {
    use djrust::musicality::apply;
    use djrust::musicality::fixes::Fixes;
    use djrust::wave::WaveProject;
    type Active = fn(&WaveProject) -> bool;
    let drums = Fixes::FILLS
        | Fixes::BUILDUP
        | Fixes::CRASH
        | Fixes::OPEN_HATS
        | Fixes::CHOKE
        | Fixes::HF_TILT
        | Fixes::BACKBEAT;
    // (the tempo plays each song to near its end in the 4,400 blocks measured, section edges and
    // the bridge included, without the end stopping the deck)
    let cases: Vec<(&str, &str, f64, &str, f64, Fixes, Active)> = vec![
        ("voiced drums", "edm", 42.0, "extended", 960.0, drums, |p| {
            p.lanes().iter().any(|l| l.kind == "crash")
                && p.lanes().iter().any(|l| l.kind == "openhat")
                && p.lanes().iter().any(|l| l.chokes().is_some())
                && p.lanes().iter().any(|l| l.tilt_on())
                && p.lanes().iter().any(|l| l.net_id.starts_with("fill-"))
        }),
        ("brushes", "jazz", 7.0, "standard", 480.0, drums, |p| {
            p.lanes()
                .iter()
                .any(|l| l.kind == "snare" && l.voice.carrier == "noise")
        }),
        (
            "section plan",
            "bossa",
            1.0,
            "standard",
            900.0,
            Fixes::SECTION_HARMONY | Fixes::HARMONIC_RHYTHM | Fixes::GROOVE,
            |p| p.harmony.is_some() && p.lanes().iter().any(|l| l.harmony_group() >= 0),
        ),
    ];
    for (what, genre, seed, structure, tempo, fixes, active) in cases {
        let spec = TrackSpec::new(genre, seed, Some(structure));
        let mut played = apply(&spec.project(), &spec, Fixes::ALL);
        // (swung and humanized: the groove's held-back notes carry the played note)
        played.swing = 40.0;
        played.humanize = 20.0;
        let sr = 48_000.0;
        let mut opts = djrust::wave::WaveOptions {
            groove: fixes.contains(Fixes::GROOVE),
            ..djrust::wave::WaveOptions::extended()
        };
        opts.musicality = fixes;
        let opts = opts.for_genre(genre);
        let mut core = djrust::host::deck::DeckCore::new(
            "A",
            sr,
            opts.clone(),
            false,
            std::sync::Arc::new(djrust::host::deck::DeckMirror::default()),
        );
        core.apply(djrust::host::deck::Command::Load(
            djrust::wave::WaveRunner::prepare(&played, sr, &opts, 0),
        ));
        core.apply(djrust::host::deck::Command::Tempo(tempo));
        core.apply(djrust::host::deck::Command::Play);
        let mut rig = Rig {
            core,
            l: vec![0f32; BLOCK],
            r: vec![0f32; BLOCK],
            frame: 0,
        };
        rig.blocks(100);
        assert!(active(rig.core.runner.project().unwrap()), "{what} is off");
        rig.measured(what, 3000);
        // a hand back over what has played, forward again, then off
        rig.core.apply(djrust::host::deck::Command::Freeze);
        rig.core.apply(djrust::host::deck::Command::Scrub(-24.0));
        rig.measured(&format!("{what}, scrubbing back"), 400);
        rig.core.apply(djrust::host::deck::Command::Scrub(24.0));
        rig.measured(&format!("{what}, scrubbing on"), 400);
        rig.core.apply(djrust::host::deck::Command::Release);
        rig.measured(&format!("{what}, released"), 500);
        assert!(rig.core.runner.graph().unwrap().tick > 16, "{what}");
    }
}

/// Live keys on a filtered, detuned voice: the melody's instrument is one the table does not know
/// (a user's pick), so the pooled live voice is a plain saw lead, which `.subtractive` gives a
/// filter envelope and `.unison` a pair. Their strikes set the filters (and the sweep) per key.
#[test]
fn live_keys_on_a_filtered_voice_allocate_nothing() {
    use djrust::musicality::fixes::Fixes;
    let core = shaped_core(
        "trance",
        "minimal",
        42.0,
        |o| {
            o.musicality |= Fixes::SUBTRACTIVE | Fixes::UNISON | Fixes::SWEEP | Fixes::ENERGY;
        },
        |p| {
            for id in p.nets.keys().to_vec() {
                let nb = p.nets.get_mut(&id).unwrap();
                if nb.track.group.as_deref() == Some("melody") {
                    nb.track.instrument = "supersaw".into();
                }
            }
        },
    );
    let mut rig = Rig {
        core,
        l: vec![0f32; BLOCK],
        r: vec![0f32; BLOCK],
        frame: 0,
    };
    rig.blocks(400);
    let live = rig.core.runner.project().unwrap().live_lane();
    assert!(live.filter_envelope().0 > 0.0, "the live voice is filtered");
    assert!(live.unison_pair().is_some(), "and a pair");
    let (_, a, f) = count(|| {
        for (i, n) in [48, 55, 60, 64, 67, 72, 76, 79, 83].into_iter().enumerate() {
            rig.core.runner.note_on(n, 60 + i as i64 * 6);
            rig.blocks(10);
        }
        rig.core.runner.note_off(60);
        rig.blocks(100);
        rig.core.runner.all_notes_off();
        rig.blocks(100);
    });
    assert_eq!((a, f), (0, 0), "live keys: {a} allocations, {f} frees");
}

/// `WaveDeckSourceTests.voiceTweaksDoNotAllocate` (country, seed 7, minimal, tempo 480): the
/// sound panel while the song plays, every parameter and a pitch sweep switched on, then
/// switched off, then Reset, over a project prepared WITHOUT a sweep. A saw or square takes up
/// its band-limited tables from prepare (`tonalLane`: `MipTables.shared` for every wavetable
/// lane), so the sweep is a flag flip: building them in `set_voice` allocated on the audio
/// thread (and spent tens of ms working out ~1M sines).
#[test]
fn a_voice_tweak_with_a_pitch_sweep_allocates_and_frees_nothing() {
    use djrust::musicality::fixes::Fixes;
    use djrust::wave::{VoiceLaneParams, VoiceTweak};
    use djrust::host::deck::Command;
    use std::collections::BTreeSet;

    type Shape = fn(&mut djrust::wave::WaveOptions);
    let shapes: [(&str, Shape); 2] = [
        ("the extended options", |_| {}),
        ("no musicality fixes", |o| o.musicality = Fixes::EMPTY),
    ];
    for (what, shape) in shapes {
        let core = shaped_core("country", "minimal", 7.0, shape, |_| {});
        let mut rig = Rig {
            core,
            l: vec![0f32; BLOCK],
            r: vec![0f32; BLOCK],
            frame: 0,
        };
        rig.blocks(400);
        let sr = 48_000.0;
        let wavetable = |l: &djrust::wave::Lane| {
            l.is_tonal() && !l.is_pluck() && (l.voice.wave == "saw" || l.voice.wave == "square")
        };
        // every parameter, then back to nothing (made on the main thread: what `set_voice` sends)
        let full = VoiceTweak {
            gain_db: Some(6.0),
            attack: Some(0.03),
            decay_scale: Some(2.0),
            max_decay: Some(2.0),
            lowpass: Some(1500.0),
            distortion: Some(0.5),
            vibrato_rate: Some(5.0),
            vibrato_depth: Some(0.01),
            pitch_from: Some(4.0),
            pitch_to: Some(1.0),
            pitch_time: Some(0.1),
        };
        let off = VoiceTweak {
            lowpass: Some(0.0),
            pitch_time: Some(0.0),
            ..Default::default()
        };
        let (cmds, reset, swept) = {
            let project = rig.core.runner.project().unwrap();
            let tonal: BTreeSet<i64> = project
                .lanes()
                .iter()
                .filter(|l| l.is_tonal() && l.channel >= 0)
                .map(|l| l.channel)
                .collect();
            let swept: BTreeSet<i64> = project
                .lanes()
                .iter()
                .filter(|l| l.channel >= 0 && wavetable(l))
                .map(|l| l.channel)
                .collect();
            assert!(!swept.is_empty(), "{what}: a saw or square lane to sweep");
            let params = |ch: i64, t: &VoiceTweak| {
                let v = project.channel_voice(ch).map(|c| c.voice);
                VoiceLaneParams::new(ch, t, false, v.as_ref(), sr)
            };
            let mut cmds = Vec::new();
            for &ch in &tonal {
                cmds.push(params(ch, &full));
                cmds.push(params(ch, &off));
            }
            let reset: Vec<VoiceLaneParams> = tonal
                .iter()
                .map(|&ch| params(ch, &VoiceTweak::default()))
                .collect();
            (cmds, reset, swept)
        };
        let (_, a, f) = count(|| {
            for &c in &cmds {
                rig.core.apply(Command::Voice(c));
                rig.blocks(20);
            }
            for &c in &reset {
                rig.core.apply(Command::Voice(c));
            }
            rig.blocks(200);
        });
        assert_eq!(
            (a, f),
            (0, 0),
            "{what}: voice tweaks allocated {a} times and freed {f} times"
        );
        // not vacuous: the sweep does switch the band-limited tables on, on lanes that were
        // prepared without one
        let ch = *swept.iter().next().unwrap();
        let v = rig
            .core
            .runner
            .project()
            .unwrap()
            .channel_voice(ch)
            .map(|c| c.voice);
        rig.core
            .apply(Command::Voice(VoiceLaneParams::new(ch, &full, false, v.as_ref(), sr)));
        let on = rig
            .core
            .runner
            .project()
            .unwrap()
            .lanes()
            .iter()
            .filter(|l| l.channel == ch && wavetable(l))
            .all(|l| l.pm_on() && l.mip_on());
        assert!(on, "{what}: the sweep reads the band-limited tables");
    }
}

/// `counterSeesAllocations`: the hook must actually fire, or "0" means nothing.
#[test]
fn the_counter_sees_allocations() {
    let mut keep: Vec<Vec<u64>> = Vec::new();
    let (_, a, f) = count(|| {
        for i in 0..50u64 {
            keep.push(vec![i; 64]);
        }
        keep.clear();
        keep.shrink_to_fit();
    });
    assert!(a >= 50, "got {a}");
    assert!(f >= 50, "got {f}");
}

/// The beat clock and the cue buttons on both kinds of deck, while they render: a net deck's
/// cue, hot cue jump and section launches (each a compile on the main thread, swapped in on a
/// bar and the old project retired), REWIND, the tempo fader and the nudge, SYNC and FOLLOW,
/// then the sample deck's quantized cues and loops, the beat grid edits and a quantized jump.
/// The calls are the main thread's and sit outside the counted windows; only the renders are
/// measured.
#[test]
fn the_beat_clock_cues_and_relocation_allocate_and_free_nothing() {
    use djrust::deck::beat::{Quantize, SyncAlign};

    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
    assert!(e.load_demo("B"));
    e.play("A");
    e.play("B");
    e.set_tempo("A", 480.0);
    run(&mut r, 400, &mut l, &mut rr); // warm-up
    measured("steady", &mut r, 400, &mut l, &mut rr);

    // a net deck's cue: set, play on, jump back (CUE while playing pauses), play again
    e.hot_cue("A", 0);
    e.pause("A");
    run(&mut r, 3, &mut l, &mut rr);
    e.cue("A");
    e.play("A");
    measured("net cue set", &mut r, 400, &mut l, &mut rr);
    e.hot_cue("A", 0); // a relocation while playing: compiled outside, swapped in at a bar
    measured("net hot cue jump", &mut r, 1500, &mut l, &mut rr);
    e.cue("A");
    measured("net cue return", &mut r, 400, &mut l, &mut rr);
    e.play("A");
    assert!(e.launch_section("A", 2));
    measured("section launch", &mut r, 1500, &mut l, &mut rr);
    assert!(e.step_section("A", 1));
    measured("section step", &mut r, 1500, &mut l, &mut rr);
    assert!(e.rewind("A"));
    measured("rewind", &mut r, 1500, &mut l, &mut rr);
    assert!(e.relocate("A", 40, None, None));
    measured("relocate", &mut r, 1500, &mut l, &mut rr);
    e.drain_retired();

    // the tempo fader and the nudge on both, SYNC and FOLLOW
    e.set_pitch("A", 0.5);
    e.nudge("A", -0.03);
    e.set_pitch("B", -0.25);
    e.nudge("B", 0.02);
    measured("fader and nudge", &mut r, 600, &mut l, &mut rr);
    assert!(e.sync("B", "A", SyncAlign::Bar).is_some());
    measured("bar sync", &mut r, 600, &mut l, &mut rr);
    assert!(e.sync("A", "B", SyncAlign::Beat).is_some());
    measured("beat sync", &mut r, 600, &mut l, &mut rr);
    e.follow("B", Some("A"));
    e.set_pitch("A", -0.3);
    for _ in 0..30 {
        run(&mut r, 1, &mut l, &mut rr);
        e.service_beat();
    }
    measured("follow", &mut r, 600, &mut l, &mut rr);
    e.follow("B", None);

    // the sample deck, quantized
    e.set_quantize("B", Quantize::Beat);
    run(&mut r, 100, &mut l, &mut rr);
    e.hot_cue("B", 1);
    e.cue("B");
    e.loop_in("B");
    run(&mut r, 150, &mut l, &mut rr);
    e.loop_out("B");
    measured("quantized loop", &mut r, 600, &mut l, &mut rr);
    e.loop_halve("B");
    e.loop_double("B");
    e.set_quantize("B", Quantize::Bar);
    e.loop_beats("B", 2.0);
    measured("loop halve, double, beats", &mut r, 600, &mut l, &mut rr);
    e.exit_loop("B");
    e.hot_cue("B", 1); // a quantized jump keeps the phase
    measured("quantized jump", &mut r, 400, &mut l, &mut rr);
    e.set_bpm("B", 100.0);
    e.set_downbeat_here("B");
    measured("grid edit", &mut r, 400, &mut l, &mut rr);
    assert!(e.rewind("B"));
    measured("sample rewind", &mut r, 200, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// Render `blocks` blocks, each one counted on its own, the main thread's housekeeping between
/// them (outside the window): the loop row is serviced as the app does, ~every frame.
fn pump(what: &str, e: &mut Engine, r: &mut Renderer, blocks: usize, l: &mut [f32], rr: &mut [f32]) {
    for _ in 0..blocks {
        measured(what, r, 1, l, rr);
        e.service_loops();
    }
}

/// The loop row's engine side (`LoopLaunchTests`, `LoopRecordTests`): a start and a stop waiting
/// on a launch line while the deck's tempo moves under them (retimed), a scene switching clips
/// in and out on one boundary, a level change on its frame, a crop and a sequence taking a
/// looping clip's place (the voice carries on in phase), a clip moved onto deck B and back, the
/// loop channel's mute and trim. The row's edits are the main thread's and sit outside the
/// counted windows; only the renders are measured.
#[test]
fn the_loop_row_launches_scenes_and_swaps_allocate_and_free_nothing() {
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.set_volume("B", 1.0);
    let mut spec = TrackSpec::new("techno", 1.0, Some("minimal"));
    spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
    assert!(e.generate("A", &spec));
    e.play("A");
    run(&mut r, 750 + 40, &mut l, &mut rr);
    // two one-bar takes, looping on L
    for _ in 0..2 {
        assert!(e.loop_record("A", true, Some(1)));
        for _ in 0..60 {
            run(&mut r, 1, &mut l, &mut rr);
            e.service_loops();
        }
        e.loop_record("A", false, None);
        let n = e.loops.row.clips.len();
        for _ in 0..4000 {
            if e.loops.row.clips.len() > n {
                break;
            }
            run(&mut r, 1, &mut l, &mut rr);
            e.service_loops();
        }
        run(&mut r, 100, &mut l, &mut rr);
    }
    assert_eq!(e.loops.row.clips.len(), 2, "two takes");
    let (one, two) = (e.loops.row.clips[0].id.clone(), e.loops.row.clips[1].id.clone());
    pump("two loops", &mut e, &mut r, 800, &mut l, &mut rr);

    // a stop and a start waiting on a 2-bar line while A's tempo moves under them
    e.set_loop_quantum(8);
    e.set_looping(&one, false);
    e.set_looping(&two, false);
    pump("waiting stops", &mut e, &mut r, 100, &mut l, &mut rr);
    e.nudge("A", 0.04);
    pump("retimed stops", &mut e, &mut r, 2400, &mut l, &mut rr);
    assert!(e.looping_clips().is_empty());
    e.set_looping(&one, true);
    pump("waiting start", &mut e, &mut r, 100, &mut l, &mut rr);
    e.nudge("A", -0.03);
    pump("retimed start", &mut e, &mut r, 2400, &mut l, &mut rr);
    assert_eq!(e.looping_clips(), [one.clone()]);

    // a scene: two in at its level, the other out, on one boundary; a level on its frame
    e.set_loop_quantum(4);
    e.set_looping(&two, true);
    pump("second loop", &mut e, &mut r, 1600, &mut l, &mut rr);
    e.save_scene("both");
    e.set_clip_gain(&one, 0.5, None);
    e.set_clip_fx(&two, &LoopFx::presets()[3].1, None);
    e.save_scene("quiet");
    e.set_clip_gain(&one, 1.0, None);
    e.set_clip_fx(&two, &LoopFx::default(), None);
    e.launch_scene("quiet");
    pump("scene launch", &mut e, &mut r, 1600, &mut l, &mut rr);
    e.set_clip_gain(&one, 0.25, Some(e.loop_frames() + 6000));
    pump("level on its frame", &mut e, &mut r, 200, &mut l, &mut rr);
    e.launch_scene_clips(&[djrust::session::types::SceneClip {
        clip_id: one.clone(),
        gain: 1.0,
        fx: None,
    }]);
    pump("scene takes the other out", &mut e, &mut r, 1600, &mut l, &mut rr);
    assert_eq!(e.looping_clips(), [one.clone()]);

    // a crop and a sequence take a looping clip's place
    let cropped = e.crop_clip(&one, 0, 2).expect("a crop");
    pump("crop swap", &mut e, &mut r, 800, &mut l, &mut rr);
    assert_eq!(e.loop_state(&cropped), djrust::host::deck::loops::LoopState::On);
    e.drain_retired();
    let seq = e
        .make_sequence(
            &[
                djrust::session::types::SequenceStep::new(&cropped, 2),
                djrust::session::types::SequenceStep::new(&two, 1),
            ],
            None,
        )
        .expect("a sequence");
    e.set_looping(&seq, true);
    pump("sequence in", &mut e, &mut r, 1600, &mut l, &mut rr);
    let seq2 = e
        .make_sequence(
            &[djrust::session::types::SequenceStep::new(&two, 2)],
            Some(&seq),
        )
        .expect("an edited sequence");
    pump("sequence edit", &mut e, &mut r, 1600, &mut l, &mut rr);
    assert_eq!(e.loop_state(&seq2), djrust::host::deck::loops::LoopState::On);
    e.drain_retired();

    // a clip on deck B (a sample deck with its own loop and grid) and back on L
    e.play_clip(&two, "B");
    pump("clip on a deck", &mut e, &mut r, 1200, &mut l, &mut rr);
    assert_eq!(e.deck("B").unwrap().clip_id.as_deref(), Some(two.as_str()));
    e.play_clip(&two, "L");
    pump("clip back on L", &mut e, &mut r, 1600, &mut l, &mut rr);

    // the loop channel's strip
    e.set_loop_mute(true);
    pump("loop channel muted", &mut e, &mut r, 200, &mut l, &mut rr);
    e.set_loop_mute(false);
    e.set_loop_gain(0.4);
    e.set_loop_volume(0.6);
    pump("loop channel trim", &mut e, &mut r, 200, &mut l, &mut rr);
    e.remove_clip(&two);
    pump("a clip off the row", &mut e, &mut r, 400, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// `WaveDeckSourceTests.renderDoesNotAllocate`, the gestures the other tests here did not
/// cover, through the real command ring (they are popped and applied inside the counted
/// windows): live keys (on, off, all off), `align` forwards and back, a `chase`, a scratch with
/// it, every master macro fired at once and re-fired, permanent fires (`bars` 0), the
/// sliders by hand, and a key, a macro and a stop in one window. Both the parity options and the
/// app's (the limiter).
#[test]
fn keys_align_chase_every_macro_and_permanent_fires_allocate_nothing() {
    use djrust::host::deck::Command;
    for extended in [false, true] {
        let mut c = EngineConfig::new(48_000.0);
        c.extended = extended;
        let (mut e, mut r) = engine(c);
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        deck_a(&mut e, 42.0);
        e.set_tempo("A", 480.0);
        e.play("A");
        run(&mut r, 200, &mut l, &mut rr);
        let what = |s: &str| format!("{s} (extended: {extended})");

        // the performance surface, sent at once (main thread), applied inside the window
        e.fire("A", NetMacro::Drop, None);
        e.fire("A", NetMacro::BeatRepeat, None);
        e.fire("A", NetMacro::HalfTime, None);
        e.command("A", Command::note_on(60, 100));
        e.command("A", Command::note_on(64, 90));
        e.command("A", Command::note_off(60));
        e.command("A", Command::AllNotesOff);
        e.command("A", Command::Align(3.4));
        e.command("A", Command::Align(-2.25));
        measured(&what("keys and align"), &mut r, 1500, &mut l, &mut rr);
        e.panic("A");
        e.fire("A", NetMacro::Cut, None);
        e.command("A", Command::note_on(67, 80));
        measured(
            &what("panic, cut, a held key"),
            &mut r,
            1000,
            &mut l,
            &mut rr,
        );

        // scratch with a chase, forward over ticks and back over them
        e.command("A", Command::Freeze);
        e.command("A", Command::Chase(0.01));
        e.command("A", Command::Scrub(9.0));
        measured(&what("scratch forward"), &mut r, 150, &mut l, &mut rr);
        e.command("A", Command::note_on(62, 100));
        e.command("A", Command::Scrub(-14.0));
        measured(&what("scratch back, a key"), &mut r, 150, &mut l, &mut rr);
        e.command("A", Command::Release);
        e.command("A", Command::AllNotesOff);
        measured(&what("release"), &mut r, 200, &mut l, &mut rr);

        // every master macro at once (overlapping: every slider and the feel tempo move), then
        // re-fired mid-flight with other lengths
        for m in MasterMacro::ALL {
            e.fire_master("A", m, None);
        }
        measured(&what("every master macro"), &mut r, 300, &mut l, &mut rr);
        e.fire_master("A", MasterMacro::SweepLp, Some(8));
        e.fire_master("A", MasterMacro::PhaserDrone, Some(8));
        e.fire_master("A", MasterMacro::OctaveUp, Some(4));
        e.fire_master("A", MasterMacro::FeelBuild, Some(8));
        measured(
            &what("macros of other lengths"),
            &mut r,
            37,
            &mut l,
            &mut rr,
        );
        e.fire_master("A", MasterMacro::SweepLp, Some(8)); // re-fire: keeps the first start
        e.fire_master("A", MasterMacro::BuildCrush, Some(8));
        e.fire_master("A", MasterMacro::VinylBrake, None);
        measured(
            &what("re-fires and the brake"),
            &mut r,
            250,
            &mut l,
            &mut rr,
        );
        e.panic("A");
        measured(&what("panic mid-ramp"), &mut r, 5, &mut l, &mut rr);

        // permanent fires, and the sliders by hand
        for m in [
            MasterMacro::FilterRes,
            MasterMacro::PitchBend,
            MasterMacro::FeelEuphoric,
            MasterMacro::Riser,
            MasterMacro::ReverbWash,
        ] {
            e.fire_master("A", m, Some(0));
        }
        e.set_master_fx("A", MasterFxKey::CrushBits, 100);
        e.set_master_fx("A", MasterFxKey::Distortion, 100);
        e.set_master_fx("A", MasterFxKey::MasterPitch, -12);
        measured(&what("permanent fires"), &mut r, 400, &mut l, &mut rr);
        e.fire_master("A", MasterMacro::FeelReset, Some(16));
        e.fire_master("A", MasterMacro::SweepHp, Some(8));
        e.fire_master("A", MasterMacro::PhaserDrone, Some(8));
        measured(&what("the full durations"), &mut r, 1500, &mut l, &mut rr);

        // a key, a macro and a stop in one window; then play again
        e.command("A", Command::note_on(60, 100));
        e.fire("A", NetMacro::Drop, None);
        e.stop("A");
        measured(
            &what("key, macro and stop at once"),
            &mut r,
            50,
            &mut l,
            &mut rr,
        );
        e.play("A");
        measured(&what("play"), &mut r, 100, &mut l, &mut rr);
        e.drain_retired();
        assert_eq!(e.dropped, 0, "the command ring had room for all of it");
        assert_eq!(r.retire_overflow, 0);
    }
}

/// `FxUnitRealTimeTests.knobsMovingOnEveryTargetDoNotAllocate`, the net deck's part (B: a trance
/// song with a room and an echo): every FX unit knob moving, the three sends among them, macros on
/// top, kills and resets, and a panic, with the commands applied inside the counted windows.
#[test]
fn the_fx_unit_knobs_on_a_net_deck_with_a_room_and_echo_allocate_nothing() {
    use djrust::fx::unit::FxParam;
    use djrust::host::deck::Command;
    let mut rig = Rig::new("trance", 11.0, false);
    rig.blocks(400);
    let params = FxParam::ALL;
    let (mut allocs, mut frees) = (0, 0);
    for step in 0..240usize {
        // (the knobs are the main thread's; here the commands and the blocks are counted together)
        let p = params[(step + 4) % params.len()];
        let (lo, hi) = p.range();
        let phase = ((step * 7 + 13) % 40) as f64 / 39.0;
        let (_, a, f) = count(|| {
            rig.core.apply(Command::FxUnit(FxUnitCommand::Set {
                param: p.index() as u8,
                value: lo + phase * (hi - lo),
                snap: false,
            }));
            if step % 30 == 3 {
                rig.core.apply(Command::MasterMacro {
                    m: MasterMacro::SweepLp,
                    bars: -1,
                });
            }
            if step % 40 == 20 {
                rig.core
                    .apply(Command::FxUnit(FxUnitCommand::Kill { on: true, frame: 0 }));
            }
            if step % 40 == 30 {
                rig.core.apply(Command::FxUnit(FxUnitCommand::Kill {
                    on: false,
                    frame: 0,
                }));
            }
            if step % 50 == 25 {
                rig.core
                    .apply(Command::FxUnit(FxUnitCommand::Reset { frame: 0 }));
            }
            if step % 60 == 59 {
                rig.core.apply(Command::Panic);
            }
            rig.blocks(2);
        });
        allocs += a;
        frees += f;
    }
    let (_, a, f) = count(|| {
        rig.core
            .apply(Command::FxUnit(FxUnitCommand::Reset { frame: 0 }));
        rig.blocks(100);
    });
    assert_eq!(
        (allocs + a, frees + f),
        (0, 0),
        "the FX unit's knobs: {allocs} allocations, {frees} frees"
    );
    // the sends really were on the runner's path: a deck without them renders differently
    assert!(rig.core.runner.project().unwrap().space.is_some());
}

/// The track pipeline (`DeckGenerator.swift`) on a deck that is playing: a swap from the seed
/// wheel, a generate that starts mid-song, a look-ahead taken, the next track handed over, a
/// relocation with a tail, a mix-in and a mix-out. The compose, the install and `service()` run
/// on the main thread, outside the counted windows; the render path pops the command and swaps
/// the project in at a bar, and allocates and frees nothing.
#[test]
fn the_track_pipeline_costs_the_render_path_nothing() {
    use djrust::deck::slots::{LookAheadKey, SwapStart};
    use djrust::wave::MixOutTail;

    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    e.set_tempo("A", 480.0);
    e.play("A");
    run(&mut r, 400, &mut l, &mut rr);

    // the seed wheel: a song comes in at the bar after the swap
    assert!(e.step_seed("A", 1).is_some());
    measured("step_seed swap", &mut r, 1500, &mut l, &mut rr);
    e.drain_retired();

    // a generate that starts where the clock will be, and a restart of a late one
    let tick = e.deck("A").unwrap().mirror.tick();
    let spec = TrackSpec::new("house", 7.0, Some("minimal"));
    assert!(e.generate_at("A", &spec, SwapStart::Tick(tick + 16)));
    measured("generate at a tick", &mut r, 1500, &mut l, &mut rr);
    let late = TrackSpec::new("dnb", 9.0, Some("minimal"));
    assert!(e.generate_at("A", &late, SwapStart::Tick(1)));
    measured(
        "a late generate, prepared again",
        &mut r,
        1500,
        &mut l,
        &mut rr,
    );
    e.drain_retired();

    // a look-ahead composed on a worker and taken by its move
    let spec = TrackSpec::new("techno", 11.0, Some("minimal"));
    let tick = e.deck("A").unwrap().mirror.tick();
    let key = LookAheadKey {
        step: 1,
        spec: spec.clone(),
        start: SwapStart::Tick(tick + 32),
        role_filter: None,
    };
    assert!(e.request_look_ahead("A", key.clone()));
    e.wait_look_ahead("A");
    let taken = e.take_look_ahead("A", &key).expect("the key matches");
    e.swap_with_ahead("A", &spec, key.start, taken);
    measured("look-ahead swap", &mut r, 1500, &mut l, &mut rr);

    // the next track, handed over while the deck plays
    e.queue_next("A", &TrackSpec::new("lofi", 3.0, Some("minimal")));
    e.skip_to_next_track("A");
    measured("next track", &mut r, 1500, &mut l, &mut rr);
    e.drain_retired();

    // the sound panel: a tweak made live (the command ring), then carried onto the next
    // project of the song and onto another song's (`hand_over` re-applies the deck's voice
    // settings on the main thread, before the project goes to the audio thread)
    let melody = e
        .deck("A")
        .unwrap()
        .project
        .as_ref()
        .unwrap()
        .nets
        .iter()
        .find(|(_, nb)| nb.role != "control" && nb.track.group.as_deref() == Some("melody"))
        .map(|(_, nb)| nb.track.channel)
        .expect("a melody channel");
    let tweak = djrust::wave::VoiceTweak {
        gain_db: Some(-3.0),
        lowpass: Some(900.0),
        vibrato_rate: Some(5.0),
        vibrato_depth: Some(0.01),
        ..Default::default()
    };
    assert!(e.set_voice_tweak("A", &tweak, melody));
    measured("a voice tweak", &mut r, 400, &mut l, &mut rr);
    assert!(e.jump_to_section("A", 1));
    measured(
        "the tweak carried onto a jump",
        &mut r,
        1500,
        &mut l,
        &mut rr,
    );
    assert_eq!(e.voice_tweak("A", melody), tweak);
    assert!(e.set_voice_tweak("A", &djrust::wave::VoiceTweak::default(), melody));
    measured("the tweak reset", &mut r, 400, &mut l, &mut rr);
    e.drain_retired();

    // relocations with a tail and a rest, a mix-in and a mix-out
    assert!(e.relocate("A", 16, Some(MixOutTail::new(64, 2)), None));
    measured("relocate with a tail", &mut r, 1500, &mut l, &mut rr);
    assert!(e.jump_to_section("A", 1));
    measured(
        "section jump keeping the tail",
        &mut r,
        1500,
        &mut l,
        &mut rr,
    );
    deck_a(&mut e, 44.0);
    assert!(e.mix_in("A", 1).is_some());
    measured("mix in", &mut r, 1500, &mut l, &mut rr);
    assert!(e.mix_out("A", 1).is_some());
    measured("mix out", &mut r, 1500, &mut l, &mut rr);
    measured("mix out tail to its stop", &mut r, 3000, &mut l, &mut rr);
    e.drain_retired();

    // the housekeeping between blocks
    e.play("A");
    for _ in 0..200 {
        measured("service between blocks", &mut r, 4, &mut l, &mut rr);
        e.service();
    }
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

fn go(
    e: &mut Engine,
    what: &str,
    a: djrust::session::action::DeckAction,
    r: &mut Renderer,
    blocks: usize,
) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.perform(&a, djrust::host::deck::PerformCtx::default());
    measured(what, r, blocks, &mut l, &mut rr);
    e.drain_retired();
}

#[test]
fn the_deck_action_applier_costs_the_render_path_nothing() {
    use djrust::session::action::DeckAction as A;
    use djrust::session::model::DeckSource;
    use djrust::wave::VoiceTweak;

    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    e.set_tempo("A", 480.0);
    e.play("A");
    run(&mut r, 400, &mut l, &mut rr);
    let d = |s: &str| s.to_string();

    // the strip, the mixer, the lock and the focus
    for (name, v) in [
        ("volume", 0.7),
        ("gain", 0.6),
        ("filter", 0.3),
        ("eqLow", 0.2),
        ("eqMid", 0.4),
        ("eqHigh", 0.9),
        ("pitch", 0.25),
        ("mute", 1.0),
        ("mute", 0.0),
        ("reverbWet", 0.4),
        ("autoGain", 1.0),
    ] {
        go(
            &mut e,
            name,
            A::Param {
                deck: d("A"),
                name: d(name),
                value: v,
            },
            &mut r,
            40,
        );
    }
    go(&mut e, "crossfader", A::Crossfader(0.1), &mut r, 40);
    go(&mut e, "master", A::Master(0.9), &mut r, 40);
    go(
        &mut e,
        "lock",
        A::Lock {
            key: d("A.play"),
            on: true,
        },
        &mut r,
        40,
    );
    go(&mut e, "focus", A::Focus { deck: d("L") }, &mut r, 40);
    // the FX unit, quantised to the beat, on a deck and on the loops' bus
    go(
        &mut e,
        "fx",
        A::Fx {
            deck: d("A"),
            param: d("crush"),
            value: 0.5,
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "fx kill",
        A::FxKill {
            deck: d("A"),
            on: true,
            on_beat: true,
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "fx reset",
        A::FxReset {
            deck: d("A"),
            on_beat: true,
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "fx on L",
        A::Fx {
            deck: d("L"),
            param: d("lp"),
            value: 0.4,
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "master fx",
        A::MasterFx {
            deck: d("A"),
            key: d("lowpass"),
            value: 0.5,
        },
        &mut r,
        200,
    );
    // roles, stems, macros, Hits, panic
    go(
        &mut e,
        "role",
        A::Role {
            deck: d("A"),
            role: d("kick"),
            on: false,
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "stem",
        A::StemMute {
            deck: d("A"),
            stem: d("music"),
            on: true,
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "stem back",
        A::StemMute {
            deck: d("A"),
            stem: d("music"),
            on: false,
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "disable",
        A::RoleDisable {
            deck: d("A"),
            role: d("hihat"),
            on: true,
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "enable",
        A::RoleDisable {
            deck: d("A"),
            role: d("hihat"),
            on: false,
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "net macro",
        A::Macro {
            deck: d("A"),
            id: d("drop"),
        },
        &mut r,
        300,
    );
    go(
        &mut e,
        "master macro",
        A::Macro {
            deck: d("A"),
            id: d("sweep-lp"),
        },
        &mut r,
        300,
    );
    go(
        &mut e,
        "hit",
        A::Stinger { deck: d("A"), n: 1 },
        &mut r,
        200,
    );
    go(&mut e, "panic", A::Panic { deck: d("A") }, &mut r, 200);
    // the sound panel, live keys
    let ch = e.role_voices("A")[0].channel;
    go(
        &mut e,
        "voice tweak",
        A::VoiceTweak {
            deck: d("A"),
            channel: ch,
            params: VoiceTweak {
                gain_db: Some(3.0),
                lowpass: Some(900.0),
                ..Default::default()
            },
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "voice synth",
        A::VoiceSynth {
            deck: d("A"),
            channel: ch,
            on: true,
        },
        &mut r,
        100,
    );
    e.key_down("A", 61, 100);
    measured("key down", &mut r, 100, &mut l, &mut rr);
    e.key_up("A", 61);
    measured("key up", &mut r, 100, &mut l, &mut rr);
    // the song rebuilt from the next bar with another kit / role voice, a section, a mix
    go(
        &mut e,
        "kit",
        A::SetKit {
            deck: d("A"),
            kit: Some(d("kit-x")),
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "kit back",
        A::SetKit {
            deck: d("A"),
            kit: None,
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "role voice",
        A::SetRoleVoice {
            deck: d("A"),
            role: d("melody"),
            voice: Some(d("synth")),
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "section",
        A::Section {
            deck: d("A"),
            index: 1,
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "mix in",
        A::MixIn {
            deck: d("A"),
            bars: 1,
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "mix out",
        A::MixOut {
            deck: d("A"),
            bars: 1,
        },
        &mut r,
        1500,
    );
    // a track loaded, the next one queued and taken, sync / follow
    let song = TrackSpec::new("house", 5.0, Some("minimal"));
    go(
        &mut e,
        "load",
        A::Load {
            deck: d("A"),
            source: DeckSource::Net(song.clone()),
        },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "queue next",
        A::QueueNext {
            deck: d("A"),
            spec: song.next_version(),
        },
        &mut r,
        100,
    );
    go(
        &mut e,
        "next track",
        A::NextTrack { deck: d("A") },
        &mut r,
        1500,
    );
    go(
        &mut e,
        "demo on B",
        A::Load {
            deck: d("B"),
            source: DeckSource::Sample {
                id: d("demo-loop"),
                title: d("demo loop"),
                grid: None,
            },
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "play B",
        A::Play {
            deck: d("B"),
            on: true,
        },
        &mut r,
        200,
    );
    go(
        &mut e,
        "sync",
        A::Sync {
            deck: d("B"),
            align: d("bar"),
        },
        &mut r,
        300,
    );
    go(
        &mut e,
        "follow",
        A::Follow {
            deck: d("B"),
            leader: Some(d("A")),
        },
        &mut r,
        300,
    );
    go(
        &mut e,
        "hot cue",
        A::HotCue {
            deck: d("B"),
            slot: 0,
        },
        &mut r,
        100,
    );
    go(&mut e, "cue", A::Cue { deck: d("B") }, &mut r, 100);
    for _ in 0..100 {
        measured("service between blocks", &mut r, 4, &mut l, &mut rr);
        e.service();
    }
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// Wander on a deck that is playing (`Wander.swift`'s `Deck` extension): a walk at a tempo that
/// takes a move every couple of seconds: roles, STEMS, macros, stingers and swaps, composed inline
/// at the move (replay) and then through the look-ahead on a worker thread. (A composed deck's
/// palette hop is a swap like the others. A raw deck's hop, which `Engine::set_palette` makes by
/// retargeting, and a layer move are not asserted here: a retarget reaches the audio thread as
/// the same `Load` a relocation sends, which `the_track_pipeline_costs_the_render_path_nothing`
/// measures, and a layer move is state only.) The planning, the composing and the pokes run on
/// the main thread, outside the counted windows (`Engine::service` between blocks); the render
/// path pops the commands and swaps the projects in at a bar, and allocates and frees nothing.
#[test]
fn a_walk_on_a_deck_costs_the_render_path_nothing() {
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    e.set_tempo("A", 480.0); // a bar is half a second
    e.set_wander_pace("A", 4);
    e.set_wander("A", true);
    // the songs end well inside a dozen phrases: the next version carries the deck on
    e.deck_mut("A").unwrap().auto_advance = true;
    e.play("A");
    run(&mut r, 400, &mut l, &mut rr);

    // composed inline at each move (replay): a dozen phrases
    for what in ["a walk, replay"] {
        let mut blocks = 0;
        while e.deck("A").unwrap().walk.wander.log.len() < 12 && blocks < 40_000 {
            measured(what, &mut r, 1, &mut l, &mut rr);
            e.service();
            blocks += 1;
        }
    }
    let log = e.deck("A").unwrap().walk.wander.log.clone();
    assert!(log.len() >= 12, "{} moves", log.len());
    assert!(
        log.iter().any(|m| m.spec.is_some() && m.cid.is_some()),
        "a swap among the moves: {:?}",
        log.iter().map(|m| m.kind.raw()).collect::<Vec<_>>()
    );
    e.drain_retired();

    // through the look-ahead: composed a bar early on a worker, taken by its move
    e.deterministic = false;
    let before = e.deck("A").unwrap().walk.wander.log.len();
    let mut blocks = 0;
    while e.deck("A").unwrap().walk.wander.log.len() < before + 8 && blocks < 60_000 {
        measured("a walk, look-ahead", &mut r, 1, &mut l, &mut rr);
        e.service();
        blocks += 1;
    }
    assert!(e.deck("A").unwrap().walk.wander.log.len() >= before + 8);
    e.wait_composes("A");
    measured("the last swap lands", &mut r, 1500, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// `AutoMixRealTimeTests.aHandOffAllocatesNothing`: auto-mix adds nothing to the audio thread, it
/// works through commands that exist (a prepared project, play, tempo, align, the crossfader).
/// Measured over a whole hand-off: the incoming compose landing on the idle deck, the mix-out
/// swap, the start, the (late, so corrected) align, the crossfader steps, the tail's stop, with
/// the main thread's housekeeping (`Engine::service`, which runs auto-mix) outside the counted
/// windows, as the app's hub runs it.
#[test]
fn a_hand_off_allocates_and_frees_nothing() {
    use djrust::automix::AutoMixPhase;
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    assert!(e.generate("A", &TrackSpec::new("dnb", 42.0, None)));
    assert!(e.load_demo("B"));
    e.auto_mix_host.arm_delay = 1; // a late start, so the correction's align is measured too
    e.set_auto_mix(true, 4, 0);
    e.play("A");
    e.service();
    run(&mut r, 40 * 4, &mut l, &mut rr); // warm up
    e.auto_mix_now();
    let (mut allocs, mut frees, mut blocks) = (0, 0, 0);
    let mut mixed = false;
    while blocks < 40_000 {
        e.service();
        if e.auto_mix.phase == AutoMixPhase::Mixing {
            mixed = true;
        }
        if mixed && e.auto_mix.run.is_none() {
            break;
        }
        let (_, a, f) = count(|| run(&mut r, 4, &mut l, &mut rr));
        allocs += a;
        frees += f;
        blocks += 4;
    }
    assert!(mixed && e.auto_mix.log.len() == 1, "a whole hand-off ran");
    assert_eq!(allocs, 0, "the render path allocated {allocs} times");
    assert_eq!(frees, 0, "and freed {frees} times");
    // (the tail's stop and the old deck's retired project come after, still on the render path)
    measured("after the hand-off", &mut r, 3000, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// `JournalRealTimeTests.journalingWhileRenderingDoesNotAllocateOnTheRenderThread`: the journal
/// is written from the main side, every input through every path, and none of it runs inside the
/// render: with a journal attached and thousands of lines written between the blocks, rendering
/// allocates and frees nothing.
#[test]
fn journaling_while_rendering_costs_the_render_path_nothing() {
    use djrust::journal::event::JournalEvent;
    use djrust::journal::stores::JournalStoreState;
    use djrust::session::action::DeckAction;
    use djrust::host::deck::PerformCtx;
    use djrust::host::journal::{read, Journal};
    const APC: &str = "APC mini mk2 Control";
    let dir = std::env::temp_dir().join(format!("deck-rt-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut e, mut r) = engine(EngineConfig::new(48_000.0));
    let mut spec = TrackSpec::new("techno", 1.0, None);
    spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
    assert!(e.generate("A", &spec));
    e.play("A");
    e.load_demo("B");
    e.play("B");
    let journal = Journal::attach(&mut e, &dir, 1_760_000_000_000, JournalStoreState::default())
        .unwrap();
    let mut rig = djrust::host::deck::Rig::new(e);
    assert!(rig.attach(APC));
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    run(&mut r, 200, &mut l, &mut rr); // warm up
    let (mut allocs, mut frees) = (0, 0);
    for round in 0..20 {
        // the main side: inputs through every path, journaled (outside the measured region)
        for i in 0..200 {
            let v = ((round * 200 + i) % 100) as f64 / 100.0;
            rig.engine
                .perform(&DeckAction::Crossfader(v), PerformCtx::default());
            rig.route(APC, &[0xB0, 48, (i % 128) as u8], 1.0);
            journal.record(rig.engine.frame(), JournalEvent::Mark(format!("load {i}")));
        }
        // ...while the render thread renders
        let (_, a, f) = count(|| run(&mut r, 50, &mut l, &mut rr));
        allocs += a;
        frees += f;
        rig.engine.drain_retired();
    }
    assert_eq!(
        (allocs, frees),
        (0, 0),
        "the render path allocated {allocs} times and freed {frees} times"
    );
    assert_eq!(rig.engine.dropped, 0);
    journal.close(&mut rig.engine);
    let c = read(&journal.path()).unwrap();
    assert_eq!(c.entries.len(), 1 + 20 * 200 * 3, "every line written");
    assert!(!c.truncated && c.skipped.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `JournalRewindRealTimeTests.resettingWhileRenderingDoesNotAllocateOnTheRenderThread`: a reset
/// is main-thread work: the row's voices fade out and are reloaded, the decks emptied and loaded
/// again, and every buffer and project they let go of comes back through the retire rings to be
/// freed on the main thread. Rendering through resets back and forth between two busy states
/// allocates and frees nothing.
#[test]
fn resetting_while_rendering_costs_the_render_path_nothing() {
    use djrust::deck::clips::LoopClip;
    use djrust::deck::turntable::SampleBuffer;
    use djrust::deck::waveform::WaveformOverview;
    use djrust::journal::state::{ClipCheckpoint, DeckCheckpoint, JournalState};
    use djrust::session::model::{DeckSource, DEMO_ID};
    use std::sync::Arc;
    let sr = 48_000.0;
    let mut spec = TrackSpec::new("techno", 1.0, None);
    spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
    let clips: Vec<LoopClip> = (1..=3)
        .map(|n| {
            let l: Vec<f32> = (0..96_000)
                .map(|i| n as f32 * 0.01 + (i % 100) as f32 * 1e-4)
                .collect();
            let b = SampleBuffer::new(l, None, sr);
            let w = WaveformOverview::new(&b, 40);
            LoopClip::from_bars(
                &b.content_id(),
                &format!("fx loop {n}"),
                "A",
                1,
                120.0,
                0.0,
                Arc::new(b),
                w,
                None,
            )
        })
        .collect();
    let lookup = |id: &str| clips.iter().find(|c| c.id == id).cloned();
    // (as the engine names the demo loop it holds, and its master as the mixer's Float: Swift's own
    // test compares the final state with a hand-built one that says "demo", no grid, no row
    // settings and a master of 0.8, which no engine reports)
    let demo = DeckSource::Sample {
        id: DEMO_ID.into(),
        title: "demo loop".into(),
        grid: Some(djrust::deck::beat_grid::BeatGrid::new(120.0, 0.0, 1.0)),
    };
    let checkpoint = |source: DeckSource, leader: Option<&str>| {
        let mut d = DeckCheckpoint::new(source);
        d.playing = true;
        d.leader = leader.map(String::from);
        d
    };
    let clip_cp = |c: &LoopClip, looping: bool| {
        let mut k = ClipCheckpoint::new(&c.id, &c.title);
        k.looping = looping;
        k
    };
    // one: the demo on A, the net on B, both playing, clip 1 looping, clip 2 on the row
    let mut one = JournalState::new(sr);
    one.decks.insert("A".into(), checkpoint(demo.clone(), None));
    one.decks
        .insert("B".into(), checkpoint(DeckSource::Net(spec.clone()), None));
    one.clips = vec![clip_cp(&clips[0], true), clip_cp(&clips[1], false)];
    one.loop_settings = djrust::journal::replay::default_loop_settings();
    one.master = 0.8f32 as f64;
    // two: swapped over, clips 2 and 3 looping, clip 1 removed
    let mut two = JournalState::new(sr);
    two.decks
        .insert("A".into(), checkpoint(DeckSource::Net(spec.clone()), None));
    two.decks.insert("B".into(), checkpoint(demo, Some("A")));
    two.crossfader = 0.2;
    let mut c2 = clip_cp(&clips[1], true);
    c2.gain = 0.5;
    let mut c3 = clip_cp(&clips[2], true);
    c3.muted = true;
    two.clips = vec![c2, c3];
    two.removed = vec![(clips[0].id.clone(), 0)];
    two.locked = vec!["A.volume".into()];
    two.loop_settings = djrust::journal::replay::default_loop_settings();
    two.master = 0.8f32 as f64;

    let (mut e, mut r) = engine(EngineConfig::new(sr));
    e.deterministic = true; // compiles inline, on this (main) thread
    let mut l = vec![0f32; 512];
    let mut rr = vec![0f32; 512];
    e.reset_to(&one, &lookup, &mut |_| r.drain_commands());
    for _ in 0..100 {
        // warm up
        run(&mut r, 2, &mut l, &mut rr);
        e.service_loops();
        e.service();
    }
    let (mut allocs, mut frees) = (0, 0);
    for round in 0..12 {
        // the main side: the reset (outside the measured region)
        let state = if round % 2 == 0 { &two } else { &one };
        // (the render thread takes the commands between the reset's phases too, as `pump`:
        // what it applies there is counted with the rest)
        let warnings = e.reset_to(state, &lookup, &mut |_| {
            let (_, a, f) = count(|| r.drain_commands());
            allocs += a;
            frees += f;
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        // ...then the render thread renders through the swap, fades and loads
        for _ in 0..40 {
            let (_, a, f) = count(|| run(&mut r, 2, &mut l, &mut rr));
            allocs += a;
            frees += f;
            e.service_loops();
            e.service();
        }
    }
    assert_eq!(
        (allocs, frees),
        (0, 0),
        "the render path allocated {allocs} times and freed {frees} times"
    );
    assert_eq!(e.dropped, 0);
    assert_eq!(r.retire_overflow, 0);
    let now = e.journal_state(Default::default(), false);
    assert!(
        now == one,
        "the last reset stood:\n{}\n{}",
        now.json().canonical_js(),
        one.json().canonical_js()
    );
}

/// `AutoMixRealTimeTests.aHandOffAllocatesNothing` and `JournalRealTimeTests` for a rewind: a
/// reset to a checkpoint taken while a hand-off was under way (`DJEngine.restoreAutoMix` stands
/// the run up, its walks held and the leader link back) and to an empty one (which lets go of
/// it) costs the render path nothing, and the restored hand-off still ends, on the main side's
/// housekeeping, through the same commands a live one uses.
#[test]
fn resetting_to_a_mid_hand_off_checkpoint_costs_the_render_path_nothing() {
    use djrust::automix::AutoMixPhase;
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    e.deterministic = true; // compiles inline, on this (main) thread
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let empty = e.journal_state(Default::default(), false);
    assert!(e.generate("A", &TrackSpec::new("dnb", 42.0, None)));
    assert!(e.load_demo("B"));
    e.set_auto_mix(true, 4, 0);
    e.play("A");
    e.service();
    run(&mut r, 40 * 4, &mut l, &mut rr); // warm up
    e.auto_mix_now();
    // the live hand-off, to the point where B is well into its mix-in: the checkpoint
    let mut blocks = 0;
    while !(e.auto_mix.phase == AutoMixPhase::Mixing && e.deck("B").unwrap().mirror.tick() > 20) {
        e.service();
        run(&mut r, 4, &mut l, &mut rr);
        blocks += 4;
        assert!(blocks < 40_000, "the hand-off never came");
    }
    let mid = e.journal_state(Default::default(), true);
    assert!(mid.auto_mix.run.is_some(), "a hand-off is under way");
    let (mut allocs, mut frees) = (0, 0);
    // the reset (outside the measured region), the render thread taking the commands between its
    // phases as `pump` does: what it applies there is counted with the rest
    let reset = |e: &mut Engine,
                 r: &mut Renderer,
                 state: &djrust::journal::state::JournalState,
                 tally: &mut (u64, u64)| {
        let warnings = e.reset_to(state, &|_| None, &mut |_| {
            let (_, a, f) = count(|| r.drain_commands());
            tally.0 += a;
            tally.1 += f;
        });
        assert!(warnings.is_empty(), "{warnings:?}");
    };
    // the engine back and forth between the checkpoint and nothing, rendering between
    for round in 0..8 {
        let state = if round % 2 == 0 { &empty } else { &mid };
        let mut tally = (0, 0);
        reset(&mut e, &mut r, state, &mut tally);
        allocs += tally.0;
        frees += tally.1;
        assert_eq!(e.auto_mix.run.is_some(), round % 2 == 1, "round {round}");
        for _ in 0..30 {
            let (_, a, f) = count(|| run(&mut r, 2, &mut l, &mut rr));
            allocs += a;
            frees += f;
            e.service_loops();
            e.service();
        }
    }
    // (the last reset stood the hand-off: it goes on to its end)
    assert!(e.auto_mix.run.is_some());
    let before = e.auto_mix.log.len();
    let mut blocks = 0;
    while e.auto_mix.run.is_some() && blocks < 40_000 {
        e.service();
        let (_, a, f) = count(|| run(&mut r, 4, &mut l, &mut rr));
        allocs += a;
        frees += f;
        blocks += 4;
    }
    assert_eq!(e.auto_mix.log.len(), before + 1, "the hand-off ended");
    assert_eq!(
        (allocs, frees),
        (0, 0),
        "the render path allocated {allocs} times and freed {frees} times"
    );
    measured("after the hand-off", &mut r, 3000, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(e.dropped, 0);
    assert_eq!(r.retire_overflow, 0);
}

// SampleDeckMasterRealTimeTests: every master macro fired on a playing sample deck (sweeps, holds,
// feel, pitch, the vinyl brake and its release), stacked fires, sliders, panics, a jog mid-brake,
// the FX unit and a clip's FX chain allocate and free nothing on the render thread. (The pokes are
// the main thread's; only the renders are measured.)
#[test]
fn a_sample_decks_master_path_allocates_and_frees_nothing() {
    use djrust::deck::turntable::TurntableCommand;
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    e.set_crossfader(0.5);
    assert!(e.load_demo("A"));
    assert!(e.load_demo("B"));
    e.play("A");
    e.play("B");
    run(&mut r, 200, &mut l, &mut rr); // spun up, and warm
    let keys = MasterFxKey::ALL;
    for round in 0..2 {
        for (i, m) in MasterMacro::ALL.iter().copied().enumerate() {
            let deck = if i % 2 == 0 { "A" } else { "B" };
            if round == 0 {
                e.fire_macro_id(deck, m.id(), None); // on the deck's next bar
            } else {
                e.fire_master(deck, m, (i % 3 == 0).then_some(1));
            }
            if i % 4 == 1 {
                e.set_master_fx(deck, keys[i % keys.len()], (i * 7) as i32);
            }
            measured("a master macro", &mut r, 40, &mut l, &mut rr);
            if i % 5 == 4 {
                e.panic(deck);
                measured("a panic", &mut r, 8, &mut l, &mut rr);
            }
        }
        // let the stacked fires land and run out (a bar at 120 bpm is 188 blocks of 512)
        measured("the fires land", &mut r, 400, &mut l, &mut rr);
        e.panic("A");
        e.panic("B");
        measured("panic all", &mut r, 40, &mut l, &mut rr);
    }
    // a whole brake and its release on A (a bar plus the wait for the downbeat), one panicked
    // mid-way on B, then one cancelled by a jog on A
    e.fire_master("A", MasterMacro::VinylBrake, Some(1));
    e.fire_master("B", MasterMacro::VinylBrake, Some(2));
    measured("brakes", &mut r, 300, &mut l, &mut rr);
    e.panic("B");
    measured("a panicked brake", &mut r, 200, &mut l, &mut rr);
    e.fire_master("A", MasterMacro::VinylBrake, Some(1));
    measured("a brake again", &mut r, 250, &mut l, &mut rr);
    e.turntable("A", TurntableCommand::Freeze);
    measured("a jog mid-brake", &mut r, 20, &mut l, &mut rr);
    e.turntable("A", TurntableCommand::Release);
    measured("released", &mut r, 40, &mut l, &mut rr);
    // the FX unit on the bus, and a clip's FX chain with its limiter (changed while sounding)
    e.set_fx("A", FxParam::Filter, -0.6);
    e.set_fx("A", FxParam::PhaserMix, 0.5);
    e.set_fx("A", FxParam::Drive, 0.4);
    measured("the fx unit on a sample deck", &mut r, 120, &mut l, &mut rr);
    e.fx_kill("A", true);
    measured("fx kill", &mut r, 40, &mut l, &mut rr);
    e.fx_reset("A");
    let fx = LoopFx {
        mode: LoopFxMode::Soft,
        drive: 18.0,
        amount: 0.9,
        ..LoopFx::default()
    };
    e.set_deck_clip_fx("A", &fx, true);
    measured("a clip's fx at the load", &mut r, 120, &mut l, &mut rr);
    e.set_deck_clip_fx("A", &LoopFx::default(), false);
    measured("the clip's fx out", &mut r, 120, &mut l, &mut rr);
    e.set_deck_clip_fx("A", &fx, false);
    measured("the clip's fx in again", &mut r, 120, &mut l, &mut rr);
    assert_eq!(e.dropped, 0);
    assert_eq!(r.retire_overflow, 0);
}

// The marking mirror on the render path: every block publishes the running project's marking
// (tokens, mutes, tick) into its mirror, across a swap at the bar, mutes, a macro and a pause,
// while the main thread reads it into a snapshot sized once. The reads are main-thread work and
// outside the counted windows (`mirrors_rt.rs` measures a read on its own).
#[test]
fn the_marking_mirror_costs_the_render_path_nothing() {
    use djrust::wave::MarkingSnapshot;
    let sr = 22_050.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
    e.set_tempo("A", 480.0); // dense ticks: the marking changes nearly every block
    e.play("A");
    let mut snap = MarkingSnapshot::new();
    assert!(e.read_marking("A", &mut snap)); // sized here, once
    run(&mut r, 400, &mut l, &mut rr); // warm
    let before = snap.header.sequence;
    measured("publishing", &mut r, 1500, &mut l, &mut rr);
    assert!(e.read_marking("A", &mut snap));
    assert!(snap.header.sequence > before + 100, "a published state per tick");
    // a second song waits for its bar (its mirror is remembered, the old one keeps publishing)
    assert!(e.generate("A", &TrackSpec::new("house", 4.0, Some("standard"))));
    measured("a swap at the bar", &mut r, 1200, &mut l, &mut rr);
    e.set_mute("A", "kick", true);
    e.fire("A", NetMacro::Drop, None);
    measured("mutes and a macro", &mut r, 600, &mut l, &mut rr);
    e.pause("A");
    measured("paused", &mut r, 100, &mut l, &mut rr);
    e.play("A");
    measured("playing again", &mut r, 400, &mut l, &mut rr);
    assert!(e.read_marking("A", &mut snap));
    assert_eq!(snap.generation(), e.marking_layout("A").unwrap().generation);
    // reading into a sized snapshot allocates nothing, on this thread either
    let ((), a, f) = count(|| {
        for _ in 0..100 {
            assert!(e.read_marking("A", &mut snap));
        }
    });
    assert_eq!((a, f), (0, 0), "a read of the marking allocated or freed");
    e.drain_retired();
    assert_eq!(e.dropped, 0);
}

// The deck's fire and anchor rings (`LatestRing`, `WaveDeckSource.fires` / `anchors`): the render
// thread writes a record per fire and per tick into them as it plays, across songs one after
// another, a swap, a macro and a pause, wrapping the 2048-anchor ring more than once, with a
// reader draining after the counted windows; nothing is allocated or freed on the render thread.
#[test]
fn the_event_rings_cost_the_render_path_nothing() {
    use djrust::wave::EventCursor;
    let sr = 8_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal"))));
    e.set_tempo("A", 3000.0); // 40 frames a tick: a song is a few hundred blocks
    e.play("A");
    run(&mut r, 50, &mut l, &mut rr); // warm
    let m = e.deck("A").unwrap().mirror.clone();
    let mut cursor = e.event_cursor_now("A");
    let before = (m.fires.written(), m.anchors.written());
    // songs one after another (each stops at its end): the anchor ring wraps more than once
    for seed in 0..6 {
        measured("a song", &mut r, 20, &mut l, &mut rr);
        // the loop clock takes the newest of the last four anchors (a main-thread read of the
        // ring the render thread writes; counted blocks follow it)
        let clock = e.loop_clock(Some("a clip"), Some("A"));
        assert_eq!(clock.ref_deck.as_deref(), Some("A"), "song {seed}");
        assert!(
            clock.origin_frame <= e.frame() && e.frame() - clock.origin_frame <= 2 * 128,
            "song {seed}: origin {} at frame {}",
            clock.origin_frame,
            e.frame()
        );
        measured("a song, on", &mut r, 380, &mut l, &mut rr);
        assert!(e.generate("A", &TrackSpec::new("techno", 50.0 + seed as f64, Some("minimal"))));
        e.set_tempo("A", 3000.0);
        e.play("A");
    }
    let (written, anchors) = (m.fires.written() - before.0, m.anchors.written() - before.1);
    assert!(
        written > 200 && anchors > 2048,
        "{written} fires, {anchors} anchors: the ring wrapped"
    );
    let (mut f, mut a) = (Vec::new(), Vec::new());
    let (lost_f, lost_a) = e.read_events("A", &mut cursor, &mut f, &mut a);
    assert_eq!(f.len() + lost_f, written, "every fire is read or counted lost");
    // (a cursor made now starts at the newest anchor written, so the first fire has its tick)
    assert_eq!(a.len() + lost_a, anchors + 1);
    assert!(lost_a > 0, "a reader a ring behind is told how many it missed");
    e.drain_retired();
    assert!(e.generate("A", &TrackSpec::new("house", 4.0, Some("standard"))));
    measured("a swap at the bar", &mut r, 1200, &mut l, &mut rr);
    e.fire("A", NetMacro::Drop, None);
    measured("a macro", &mut r, 600, &mut l, &mut rr);
    e.pause("A");
    measured("paused", &mut r, 100, &mut l, &mut rr);
    e.play("A");
    measured("playing again", &mut r, 400, &mut l, &mut rr);
    let mut fresh = EventCursor::new();
    f.clear();
    a.clear();
    e.read_events("A", &mut fresh, &mut f, &mut a);
    assert!(
        a.windows(2).any(|w| w[1].tick != w[0].tick + 1),
        "the swap and the pause break the count"
    );
    e.drain_retired();
    assert_eq!(e.dropped, 0);
}

// Resetting a deck (`DJEngine.resetDeck` -> `Deck.eject`) that holds a clip with FX and an FX unit
// setting, over and over, with a net loaded on it in between: what the render thread applies of it
// (the clip's FX off, the platter leaving, the unit moving to the net's chain and back to the sample
// bus) allocates and frees nothing, and the retired buffers and projects come back to be freed here.
#[test]
fn resetting_a_clip_deck_with_fx_costs_the_render_path_nothing() {
    use djrust::deck::turntable::SampleBuffer;
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.decks = vec!["A".into()];
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    e.set_crossfader(0.0);
    let tone: Vec<f32> = (0..96_000)
        .map(|i| (0.5 * (std::f64::consts::TAU * 375.0 * i as f64 / sr).sin()) as f32)
        .collect();
    let id = e
        .import_clip(
            SampleBuffer::new(tone.clone(), Some(tone), sr),
            "tone",
            Some(60.0),
            2,
            None,
        )
        .expect("a clip");
    let fx = LoopFx {
        mode: LoopFxMode::Soft,
        drive: 18.0,
        amount: 0.9,
        ..LoopFx::default()
    };
    e.set_clip_fx(&id, &fx, None);
    run(&mut r, 20, &mut l, &mut rr); // warm
    for round in 0..4 {
        e.play_clip(&id, "A"); // the clip's FX come with it
        e.launch("A");
        measured("the clip with its fx", &mut r, 60, &mut l, &mut rr);
        e.set_fx("A", FxParam::Filter, -0.5); // on the sample deck's bus
        e.set_fx("A", FxParam::Drive, 0.3);
        measured("the fx unit on the bus", &mut r, 40, &mut l, &mut rr);
        e.reset_deck("A"); // clip FX off, the platter leaves, the unit to the net's chain
        measured("after the reset", &mut r, 40, &mut l, &mut rr);
        e.drain_retired();
        if round % 2 == 0 {
            // a net where the clip was (`loadNet`: the unit moves to its chain)
            deck_a(&mut e, 40.0 + round as f64);
            e.set_tempo("A", 600.0);
            e.play("A");
            measured("a net on it", &mut r, 120, &mut l, &mut rr);
            e.reset_deck("A");
            measured("the net ejected", &mut r, 40, &mut l, &mut rr);
            e.drain_retired();
        }
    }
    assert_eq!(e.dropped, 0);
    assert_eq!(r.retire_overflow, 0);
}

/// `StingerLaunch`: a Hit that launches a loop (`fireStingerLoop`, `launchStingerClip`,
/// `serviceStingerStops`) starts a voice on the loop bank through the existing command ring and
/// schedules its stop once it is in; the render path pays nothing for it. Every window below is
/// a render with the Hit's loop waiting for its bar, coming in, going out after its one pass,
/// fired again over a sounding one, fired and stopped before it is in, and with a Hit that falls
/// back to its net (a clip that is not on the row) in the same bar.
#[test]
fn a_hit_that_launches_a_loop_allocates_and_frees_nothing() {
    use djrust::deck::loops::LoopState;
    use djrust::sound::stinger::{AssignedLoop, AssignmentKind, StingerAssignment};
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    let mut spec = TrackSpec::new("techno", 1.0, Some("minimal"));
    spec.extra.set("bpm", djrust::json::JsonValue::Number(120.0));
    assert!(e.generate("A", &spec));
    e.play("A");
    run(&mut r, 750 + 40, &mut l, &mut rr);
    // a one-bar take, then off the row's loop
    assert!(e.loop_record("A", true, Some(1)));
    for _ in 0..60 {
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    e.loop_record("A", false, None);
    for _ in 0..4000 {
        if !e.loops.row.clips.is_empty() {
            break;
        }
        run(&mut r, 1, &mut l, &mut rr);
        e.service_loops();
    }
    let id = e.loops.row.clips[0].id.clone();
    pump("the take", &mut e, &mut r, 100, &mut l, &mut rr);
    e.stop_all_loops();
    pump("the take out", &mut e, &mut r, 1600, &mut l, &mut rr);
    assert!(e.looping_clips().is_empty());
    let loop_hit = |slot: usize, clip: &str| StingerAssignment {
        slot,
        kind: AssignmentKind::Loop,
        candidate: Some("test-loop".into()),
        label: "Test loop".into(),
        option: None,
        macro_id: None,
        bars: None,
        loop_: Some(AssignedLoop {
            path: None,
            clip_id: Some(clip.into()),
            name: "Test loop".into(),
            beats: 4,
            bpm: 120.0,
        }),
        local_only: false,
    };
    e.deck_mut("A").unwrap().project.as_mut().unwrap().stingers = vec![
        StingerAssignment::fallback(0),
        loop_hit(1, &id),
        loop_hit(2, "not-on-the-row"),
    ];

    // Hit 2 waits for A's bar, comes in from its top, goes out after one pass
    e.stinger("A", 1, 1);
    assert_eq!(e.loop_state(&id), LoopState::Starting);
    assert_eq!(e.stinger_stops().len(), 1, "its stop waits for the voice");
    pump("a Hit's loop waiting", &mut e, &mut r, 400, &mut l, &mut rr);
    e.stinger("A", 1, 1); // over one on its way in: left as it is
    pump("a Hit's loop coming in", &mut e, &mut r, 800, &mut l, &mut rr);
    assert!(e.stinger_stops().is_empty(), "the stop is scheduled once it is in");
    assert!(e.loop_stop_frame(&id).is_some());
    e.stinger("A", 1, 1); // over one on its way out: left as it is
    e.stinger("A", 2, 1); // a clip that is not on the row: the stinger net
    pump("a Hit's loop sounding", &mut e, &mut r, 700, &mut l, &mut rr);
    pump("a Hit's loop out", &mut e, &mut r, 300, &mut l, &mut rr);
    assert_eq!(e.loop_state(&id), LoopState::Off);
    assert!(e.looping_clips().is_empty());

    // fired and stopped before it is in: it never starts, and its pending stop is forgotten
    e.stinger("A", 1, 1);
    e.set_looping(&id, false);
    pump("a cancelled Hit", &mut e, &mut r, 20, &mut l, &mut rr);
    assert!(e.stinger_stops().is_empty());
    assert_eq!(e.loop_state(&id), LoopState::Off);
    pump("after the cancelled Hit", &mut e, &mut r, 1600, &mut l, &mut rr);
    e.drain_retired();
    assert_eq!(r.retire_overflow, 0);
    assert_eq!(e.dropped, 0);
}

/// Input strips render without allocating: steady, through underruns, skips and every control.
#[test]
fn input_strips_allocate_and_free_nothing() {
    let sr = 48_000.0;
    let mut c = EngineConfig::new(sr);
    c.inputs = vec!["TD-3".into(), "PD705".into()];
    let (mut e, mut r) = engine(c);
    let (mut feed, _) = e.take_input_feed(0).unwrap();
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut phase = 0f32;
    let mut push = |feed: &mut rtrb::Producer<f32>, n: usize| {
        for _ in 0..n {
            phase += 0.05;
            let _ = feed.push(0.5 * phase.sin());
        }
    };
    push(&mut feed, 4096);
    run(&mut r, 50, &mut l, &mut rr);
    // steady: the producer keeps up block for block (its pushes are outside the count)
    for _ in 0..500 {
        push(&mut feed, BLOCK);
        measured("input steady", &mut r, 1, &mut l, &mut rr);
    }
    e.set_input_trim(0, 6.0);
    e.set_input_strip(0, djrust::fx::StripKnob::EqLow, 0.0);
    e.set_input_mute(1, true);
    e.set_input_pfl(0, true);
    push(&mut feed, BLOCK);
    measured("input controls", &mut r, 1, &mut l, &mut rr);
    measured("input underrun", &mut r, 20, &mut l, &mut rr);
    push(&mut feed, 20_000);
    measured("input skip", &mut r, 5, &mut l, &mut rr);
}

/// The strip reverb (`DeckReverb`, after the EQ on a deck and on the loop channel): its knobs
/// moving every few blocks (wet across its switch-on line, so the room is cleared on the way
/// off, size and tone retuning the loop) cost the render path nothing.
#[test]
fn the_strip_reverb_costs_the_render_path_nothing() {
    use djrust::fx::ReverbKnob;
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    e.set_tempo("A", 480.0);
    e.play("A");
    run(&mut r, 400, &mut l, &mut rr); // warm-up
    for (k, wet) in [0.0, 0.004, 0.3, 1.0, 0.0, 0.5, 0.005, 0.8, 0.0].into_iter().enumerate() {
        e.set_reverb("A", "reverbWet", wet);
        e.set_reverb("A", "reverbSize", (k % 4) as f64 / 3.0);
        e.set_reverb("A", "reverbTone", ((k * 3) % 5) as f64 / 4.0);
        e.set_loop_reverb(ReverbKnob::Wet, wet);
        e.set_loop_reverb(ReverbKnob::Size, 1.0 - (k % 4) as f64 / 3.0);
        measured("reverb knobs", &mut r, 40, &mut l, &mut rr);
    }
    e.set_reverb("A", "reverbWet", 1.0);
    e.set_reverb("A", "reverbSize", 1.0);
    for round in 0..60 {
        e.set_reverb("A", "reverbSize", (round % 7) as f64 / 6.0);
        e.set_reverb("A", "reverbTone", (round % 5) as f64 / 4.0);
        measured("reverb sweeping", &mut r, 3, &mut l, &mut rr);
    }
    assert!(r.strip_reverb("A").unwrap().is_on());
}

/// The aux bus (docs/AUX.md): the sends, the return, the six sliders moving every few blocks, the
/// bus waking and going to sleep, and every one of the eleven macros starting on its bar, holding
/// and ending (the sliders, the send and the parts' pan and decay put back) cost the render path
/// nothing.
#[test]
fn the_aux_bus_and_its_macros_allocate_and_free_nothing() {
    use djrust::fx::{AuxFxKey, AuxMacro};
    let sr = 48_000.0;
    let (mut e, mut r) = engine(EngineConfig::new(sr));
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    deck_a(&mut e, 42.0);
    // a bar is 2 s at the deck's 120 BPM: 750 blocks of 128. A macro of one bar starts within a
    // bar and ends a bar later
    e.set_tempo("A", 480.0);
    e.play("A");
    run(&mut r, 400, &mut l, &mut rr); // warm-up
    e.set_aux_send("A", 0.5);
    e.set_aux_send("L", 0.3);
    measured("aux steady", &mut r, 100, &mut l, &mut rr);
    for (k, key) in AuxFxKey::ALL.into_iter().cycle().take(18).enumerate() {
        e.set_aux_fx(key, (k * 11 % 90) as f64);
        e.set_aux_return((k % 5) as f64 / 4.0);
        e.set_aux_mute(k % 7 == 0);
        measured("aux knobs", &mut r, 6, &mut l, &mut rr);
    }
    e.set_aux_mute(false);
    for m in AuxMacro::ALL {
        assert!(e.fire_aux_macro("A", m, Some(1)), "{}", m.id());
        measured(m.id(), &mut r, 1600, &mut l, &mut rr);
        assert_eq!(e.aux_macros_running() & m.bit(), 0, "{} is over", m.id());
    }
    // a fire stacked on a fire, then a panic in the middle of them
    assert!(e.fire_aux_macro("A", AuxMacro::PingPong, Some(2)));
    assert!(e.fire_aux_macro("A", AuxMacro::Tighten, Some(2)));
    assert!(e.fire_aux_macro("A", AuxMacro::DelayThrow, Some(2)));
    assert!(e.fire_aux_macro("L", AuxMacro::Cathedral, Some(2)));
    measured("stacked", &mut r, 230, &mut l, &mut rr);
    e.panic("A");
    e.cancel_aux_macros(None);
    measured("panic", &mut r, 30, &mut l, &mut rr);
    assert_eq!(e.aux_macros_running(), 0);
    // the sends down: the tails ring out and the bus goes to sleep
    e.set_aux_send("A", 0.0);
    e.set_aux_send("L", 0.0);
    measured("tails", &mut r, 300, &mut l, &mut rr);
}
