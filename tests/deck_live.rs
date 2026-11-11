//! `DeckCore`, the render and command side of `WaveDeckSource`: the live keys and the chase
//! command, the scrubbing flag, the per-block macro record and the master macros it publishes,
//! and the marking book the deck keeps. Each test names the Swift test it ports (parity form (a)
//! of the port rules: the Swift suites' own numbers) or says what it holds instead.
//!
//! A deck is driven the way `WaveDeckSource` is: commands applied at the top of a block, then
//! the block rendered. The sample rate and block size are the Swift tests'.

use std::sync::Arc;

use djrust::fx::MasterMacro;
use djrust::pflow::Project;
use djrust::wave::net::str_hash;
use djrust::wave::{
    MarkingMirror, MarkingSnapshot, NetMacro, WaveGraph, WaveOptions, WaveRunner,
};
use djrust::{compose, JsonObject};
use djrust::host::deck::{Command, DeckCore, DeckMirror};

fn project(genre: &str, seed: f64, structure: Option<&str>) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    if let Some(s) = structure {
        ov.set("structure", s.into());
    }
    compose(genre, &ov.into())
}

/// A deck and the frame it is at.
struct Rig {
    core: DeckCore,
    mirror: Arc<DeckMirror>,
    sr: f64,
    block: usize,
    frame: i64,
}

impl Rig {
    fn new(sr: f64, block: usize, opts: WaveOptions) -> Rig {
        let mirror = Arc::new(DeckMirror::new());
        let core = DeckCore::new("A", sr, opts, false, mirror.clone());
        Rig {
            core,
            mirror,
            sr,
            block,
            frame: 0,
        }
    }

    /// Prepare `p` (as `WaveRunner.prepare` does) and hand it over, tempo and all, playing.
    fn start(&mut self, p: &Project, opts: &WaveOptions, tempo: f64) {
        let prepared = WaveRunner::prepare(p, self.sr, opts, 0);
        self.send(Command::Load(prepared));
        self.send(Command::Tempo(tempo));
        self.send(Command::Play);
    }

    fn send(&mut self, c: Command) {
        self.core.apply(c);
    }

    /// `blocks` blocks, the left channel.
    fn run(&mut self, blocks: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0f32; self.block], vec![0f32; self.block]);
        let mut out = Vec::with_capacity(blocks * self.block);
        for _ in 0..blocks {
            self.core.render(&mut l, &mut r, self.frame);
            self.frame += self.block as i64;
            out.extend_from_slice(&l);
        }
        out
    }
}

fn max_abs(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Everything muted but the live voice (`WaveLiveTests.liveKeysSoundWhileHeldAndRingOutAfter`).
fn mute_the_song(r: &mut Rig) {
    for n in [
        "melody", "bass", "harmony", "kick", "snare", "hihat", "hit1", "hit2", "hit3", "hit4",
        "arp", "clap",
    ] {
        r.send(Command::Mute {
            hash: str_hash(n),
            muted: true,
        });
    }
}

// WaveLiveTests.liveKeysSoundWhileHeldAndRingOutAfter, through the deck's commands
#[test]
fn live_keys_sound_while_held_and_ring_out_after() {
    let mut r = Rig::new(8000.0, 128, WaveOptions::default());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::default(),
        120.0,
    );
    mute_the_song(&mut r);
    r.run(40);
    let quiet = max_abs(&r.run(20));
    r.send(Command::note_on(60, 110));
    let held = r.run(40);
    let held_peak = max_abs(&held[held.len() - 2000..]);
    assert!(
        held_peak > 0.02,
        "a held key sustains (peak {held_peak}, quiet {quiet})"
    );
    r.send(Command::note_off(60));
    r.run(60);
    let after = max_abs(&r.run(20));
    assert!(after < held_peak * 0.2, "released keys die away");
}

// WaveDeckSource.allNotesOff: every held key comes up and rings out
#[test]
fn all_notes_off_releases_every_held_key() {
    let mut r = Rig::new(8000.0, 128, WaveOptions::default());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::default(),
        120.0,
    );
    mute_the_song(&mut r);
    r.run(60);
    for k in [60, 64, 67] {
        r.send(Command::note_on(k, 100));
    }
    let held = r.run(40);
    let held_peak = max_abs(&held[held.len() - 2000..]);
    assert!(held_peak > 0.02, "the chord sustains ({held_peak})");
    r.send(Command::AllNotesOff);
    r.run(80);
    let after = max_abs(&r.run(20));
    assert!(
        after < held_peak * 0.2,
        "all keys came up: {after} vs {held_peak}"
    );
}

/// Held keys are not touched by a stop or a panic, only by an all-notes-off (the Swift
/// `stop()` / `panic()` never release the live voice, so neither does this port).
#[test]
fn a_panic_does_not_release_the_live_voice() {
    let mut r = Rig::new(8000.0, 128, WaveOptions::default());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::default(),
        120.0,
    );
    mute_the_song(&mut r);
    r.run(60);
    r.send(Command::note_on(60, 110));
    r.run(20);
    r.send(Command::Panic);
    let after_panic = r.run(40);
    assert!(
        max_abs(&after_panic[after_panic.len() - 2000..]) > 0.02,
        "the key is still held"
    );
}

/// The deck's keys are the runner's keys: the same gestures at the same block boundaries
/// against a bare runner come out byte for byte (the parity path, the chain bypassed).
#[test]
fn live_keys_through_the_deck_are_the_runners_exact_bytes() {
    let sr = 22_050.0;
    let p = project("techno", 42.0, None);
    let mut r = Rig::new(sr, 128, WaveOptions::default());
    r.start(&p, &WaveOptions::default(), 300.0);

    let mut bare = WaveRunner::new(sr, WaveOptions::default());
    bare.load(&p);
    bare.set_tempo(300.0);
    bare.play();

    let (mut want, mut got) = (Vec::new(), Vec::new());
    let (mut wr, mut gr) = (vec![0f32; 128], vec![0f32; 128]);
    let mut frame = 0i64;
    for b in 0..120 {
        match b {
            20 => {
                r.send(Command::note_on(60, 110));
                bare.note_on(60, 110);
                r.send(Command::note_on(64, 90));
                bare.note_on(64, 90);
            }
            50 => {
                r.send(Command::note_off(60));
                bare.note_off(60);
            }
            80 => {
                r.send(Command::AllNotesOff);
                bare.all_notes_off();
            }
            _ => {}
        }
        bare.process_at(&mut wr, Some(&mut gr), 128, frame);
        want.extend_from_slice(&wr);
        got.extend(r.run(1));
        frame += 128;
    }
    assert!(got == want, "the deck and the bare runner differ");
    assert!(max_abs(&got) > 0.05, "something sounded");
}

// Int16(clamping:) / UInt8(clamping:) of WaveDeckSource.noteOn / noteOff
#[test]
fn the_key_commands_clamp_like_swift() {
    match Command::note_on(100_000, 300) {
        Command::NoteOn { note, velocity } => assert_eq!((note, velocity), (i16::MAX, 255)),
        _ => unreachable!(),
    }
    match Command::note_on(-100_000, -3) {
        Command::NoteOn { note, velocity } => assert_eq!((note, velocity), (i16::MIN, 0)),
        _ => unreachable!(),
    }
    match Command::note_on(61, 110) {
        Command::NoteOn { note, velocity } => assert_eq!((note, velocity), (61, 110)),
        _ => unreachable!(),
    }
    match Command::note_off(-5) {
        Command::NoteOff { note } => assert_eq!(note, -5),
        _ => unreachable!(),
    }
}

// FireEventTests.scratchingTheNetClockMakesNoEventsButTheClockResumesAfterward: the part of it
// that is the deck's (the chase command, the clock following the hand, back on the tempo clock
// at the release); the fire and anchor rings are not ported to the live deck
#[test]
fn the_chase_command_sets_how_fast_the_clock_follows_the_hand() {
    let sr = 22_050.0;
    let tick_after = |chase: Option<f64>, blocks: usize| {
        let mut r = Rig::new(sr, 128, WaveOptions::default());
        r.start(
            &project("techno", 42.0, None),
            &WaveOptions::default(),
            300.0,
        );
        r.run(100);
        let before = r.mirror.tick();
        r.send(Command::Freeze);
        r.send(Command::Scrub(12.0));
        if let Some(s) = chase {
            r.send(Command::Chase(s));
        }
        r.run(blocks);
        (before, r.mirror.tick(), r)
    };
    let (before, quick, mut r) = tick_after(Some(0.01), 150);
    assert!(quick > before + 6, "the scratch moved the net clock");
    // a short chase gets there sooner than the default 35 ms
    let (_, slow, _) = tick_after(None, 6);
    let (_, fast, _) = tick_after(Some(0.002), 6);
    assert!(
        fast > slow,
        "a chase of 2 ms is ahead of 35 ms: {fast} vs {slow}"
    );
    // backwards: un-firing
    r.send(Command::Scrub(-20.0));
    r.run(150);
    assert!(r.mirror.tick() < quick, "back over the ticks");
    let at_release = r.mirror.tick();
    r.send(Command::Release);
    r.run(200);
    assert!(r.mirror.tick() > at_release, "back on the tick clock");
}

// WaveDeckSource.isScrubbing: published once per block
#[test]
fn the_scrubbing_flag_follows_the_hand() {
    let mut r = Rig::new(22_050.0, 128, WaveOptions::default());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::default(),
        240.0,
    );
    r.run(20);
    assert!(!r.mirror.is_scrubbing());
    r.send(Command::Freeze);
    assert!(!r.mirror.is_scrubbing(), "published by the next block");
    r.run(1);
    assert!(r.mirror.is_scrubbing());
    r.send(Command::Scrub(4.0));
    r.run(30);
    assert!(r.mirror.is_scrubbing());
    r.send(Command::Release);
    r.run(1);
    assert!(!r.mirror.is_scrubbing());
    // a hand cancelled by freeze on a deck with no project is not a hand
    let mut empty = Rig::new(22_050.0, 128, WaveOptions::default());
    empty.send(Command::Freeze);
    empty.run(2);
    assert!(!empty.mirror.is_scrubbing());
}

// MacroMirrorTests.aMuteMacroIsInEffectUntilItsRestore, on the deck's record: the macros in
// effect, the nets they hold, the roles held
#[test]
fn the_macro_record_follows_a_mute_macro() {
    let sr = 22_050.0;
    let p = project("techno", 42.0, None); // ("loop" in the Swift helper: no structure)
    let mut r = Rig::new(sr, 128, WaveOptions::default());
    // the project's role tables, from a second compile (the loaded one belongs to the deck)
    let tables = WaveRunner::prepare(&p, sr, &WaveOptions::default(), 0);
    let loaded = WaveRunner::prepare(&p, sr, &WaveOptions::default(), 0);
    let generation = loaded.generation;
    let graph: &WaveGraph = &tables.graph;
    r.send(Command::Load(loaded));
    r.send(Command::Tempo(300.0));
    r.send(Command::Play);
    r.run(50);
    let roles = [
        "kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp",
    ];
    let m = &r.mirror;
    assert!(m.active_macros().is_empty() && !m.is_active(NetMacro::Drop));
    assert!(m
        .roles_held_by_macros(Some((generation, graph)), &roles)
        .is_empty());
    let sounding: Vec<&str> = roles
        .iter()
        .copied()
        .filter(|role| {
            let nets = net_indices(graph, role);
            nets.iter().any(|&n| !m.is_net_muted(n))
        })
        .collect();
    r.send(Command::Macro {
        m: NetMacro::Drop,
        ticks: 16,
    });
    r.run(3);
    let m = &r.mirror;
    assert_eq!(m.active_macros(), [NetMacro::Drop]);
    assert!(m.is_active(NetMacro::Drop) && !m.is_active(NetMacro::Cut));
    assert!(m.macro_active.load(std::sync::atomic::Ordering::Relaxed));
    let held = m.roles_held_by_macros(Some((generation, graph)), &roles);
    let drums = ["kick", "snare", "hihat", "clap"];
    let want: Vec<&str> = sounding
        .iter()
        .copied()
        .filter(|r| !drums.contains(r))
        .collect();
    assert_eq!(held, want, "the sounding non-drum roles are the drop's");
    assert!(
        held.len() >= 3,
        "techno has bass, melody, harmony...: {held:?}"
    );
    assert_eq!(
        m.roles_held_by(Some((generation, graph)), NetMacro::Drop, &roles),
        held
    );
    assert!(m
        .roles_held_by(Some((generation, graph)), NetMacro::Breakdown, &roles)
        .is_empty());
    // a project the deck is not running (another generation) is not chased through its tables
    assert!(m
        .roles_held_by_macros(Some((generation + 1, graph)), &roles)
        .is_empty());
    assert!(m.roles_held_by_macros(None, &roles).is_empty());
    // the nets named by the record are the held ones
    let held_nets: Vec<usize> = (0..graph.nets.len())
        .filter(|&i| m.is_net_held_by_macro(i))
        .collect();
    assert!(!held_nets.is_empty() && held_nets.iter().all(|&i| m.is_net_muted(i)));
    r.run(100);
    assert!(r.mirror.is_active(NetMacro::Drop), "still held at 600 ms");
    r.run(60);
    let m = &r.mirror;
    assert!(m.active_macros().is_empty(), "restored on the 16th tick");
    assert!(m
        .roles_held_by_macros(Some((generation, graph)), &roles)
        .is_empty());
    assert!(!(0..graph.nets.len()).any(|i| m.is_net_held_by_macro(i)));
}

fn net_indices(g: &WaveGraph, role: &str) -> Vec<usize> {
    g.role_net_indices(str_hash(role)).unwrap_or_default()
}

// SurfaceNetTests.apcPadsAreBeatsMacroGridInGroupColours: after the drop pad and 4096 frames,
// the deck's active macros contain .drop (the LED half of it is the controller's, not the deck's)
#[test]
fn a_drop_is_among_the_active_macros_4096_frames_later() {
    let mut r = Rig::new(22_050.0, 512, WaveOptions::default());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::default(),
        120.0,
    );
    r.run(4);
    r.send(Command::Macro {
        m: NetMacro::Drop,
        ticks: NetMacro::Drop.ticks(None),
    });
    r.run(8); // 4096 frames
    assert!(r.mirror.active_macros().contains(&NetMacro::Drop));
}

// WaveDeckSource.activeMasterMacros / isActive(MasterMacro)
#[test]
fn the_active_master_macros_are_listed_in_catalogue_order() {
    let mut r = Rig::new(22_050.0, 128, WaveOptions::extended());
    r.start(
        &project("techno", 42.0, None),
        &WaveOptions::extended(),
        120.0,
    );
    r.run(10);
    assert!(r.mirror.active_master_macros().is_empty());
    r.send(Command::MasterMacro {
        m: MasterMacro::SweepLp,
        bars: 1,
    });
    r.send(Command::MasterMacro {
        m: MasterMacro::PhaserDrone,
        bars: 1,
    });
    r.run(4);
    let active = r.mirror.active_master_macros();
    assert!(r.mirror.is_master_active(MasterMacro::SweepLp));
    assert!(r.mirror.is_master_active(MasterMacro::PhaserDrone));
    let pos = |m: MasterMacro| active.iter().position(|a| *a == m).unwrap();
    assert!(
        pos(MasterMacro::SweepLp).min(pos(MasterMacro::PhaserDrone)) == 0 && active.len() == 2,
        "{active:?}"
    );
    assert_eq!(
        active,
        MasterMacro::ALL
            .iter()
            .copied()
            .filter(|m| active.contains(m))
            .collect::<Vec<_>>(),
        "in the order of the catalogue"
    );
    r.send(Command::Panic);
    r.run(4);
    assert!(r.mirror.active_master_macros().is_empty());
}

// WaveDeckSource.readMarking / markingLayout over the mirrors it kept (`remember`); the render
// thread's half (`publishMarking`) is the track pipeline's, so this holds the main-thread book
// the deck owns: which project it reports, and that a newer load wins on a stopped deck
#[test]
fn the_deck_keeps_the_marking_mirrors_of_the_projects_it_loaded() {
    let sr = 22_050.0;
    let mut r = Rig::new(sr, 128, WaveOptions::default());
    let mut snap = MarkingSnapshot::new();
    assert!(!r.mirror.read_marking(&mut snap), "nothing loaded");
    assert!(r.mirror.marking_layout().is_none());

    let load = |r: &mut Rig, genre: &str, seed: f64| {
        let p = project(genre, seed, None);
        let wp = WaveRunner::prepare(&p, sr, &WaveOptions::default(), 0);
        let m = Arc::new(MarkingMirror::new(
            wp.generation,
            1,
            wp.start_tick,
            &wp.graph,
            &p,
        ));
        let generation = wp.generation;
        r.mirror.remember_marking(m);
        r.send(Command::Load(wp));
        generation
    };
    let a = load(&mut r, "techno", 42.0);
    // not rendered yet: the stopped deck reports the project loaded last
    assert!(r.mirror.read_marking(&mut snap));
    assert_eq!(snap.generation(), a);
    assert_eq!(r.mirror.marking_layout().unwrap().generation, a);
    r.run(2);
    assert!(r.mirror.read_marking(&mut snap));
    assert_eq!(snap.generation(), a);
    // a second song loaded while the first is installed, not playing: the newer is reported
    let b = load(&mut r, "dnb", 3.0);
    assert!(r.mirror.read_marking(&mut snap));
    assert_eq!(
        snap.generation(),
        b,
        "the newest, installed at the next block"
    );
    assert_eq!(r.mirror.marking_layout().unwrap().generation, b);
    assert_eq!(r.mirror.marking_layout_of(a).unwrap().generation, a);
    assert!(r.mirror.marking_layout_of(a + 1000).is_none());
    assert!(r.mirror.read_marking_of(a, &mut snap) && snap.generation() == a);
    assert!(!r.mirror.read_marking_of(a + 1000, &mut snap));
}

// A master macro fired in the commands of a block is anchored at that block's transport time
// (Swift: `fx.setNow(blockTransport)` before it pops the commands). Fired at block 60 of a
// techno 42 minimal deck at 48 kHz, 128-frame blocks, 480 BPM (the golden's `master-macros`),
// the sweep's low-pass first moves at block 105, 45 blocks later; a deck whose automation clock
// was last set a block earlier started it at 104 and ended it a block early.
#[test]
fn a_master_macro_is_anchored_at_the_block_it_is_fired_in() {
    let sr = 48_000.0;
    let opts = WaveOptions::extended();
    let mut r = Rig::new(sr, 128, opts.clone());
    r.start(&project("techno", 42.0, Some("minimal")), &opts, 480.0);
    let lp_open = |r: &Rig| {
        let (_, lp, idle) = r.mirror.master_filter_corners();
        idle || lp == 20_000.0
    };
    r.run(60);
    r.send(Command::MasterMacro {
        m: MasterMacro::SweepLp,
        bars: 1,
    });
    let mut first_move = None;
    for b in 60..140 {
        r.run(1);
        if first_move.is_none() && !lp_open(&r) {
            first_move = Some(b);
        }
    }
    assert_eq!(
        first_move,
        Some(105),
        "the sweep starts 45 blocks after its fire"
    );
    // it is over (and the mask clear) the block Swift reports, 187 blocks after the fire's own
    r.run(300);
    assert!(r.mirror.active_master_macros().is_empty());
}
