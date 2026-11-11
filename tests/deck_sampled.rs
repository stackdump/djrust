//! The deck's side of the sampled instruments (feature `sf2`): what `WaveRunner.flushSamplers`
//! and `renderScratch` do with a slot in Swift, held here against a reference built by hand.
//!
//! The instrument is a synthesised SoundFont (a looped sine, `synthetic_sf2`), rendered twice:
//! once inside the `SamplerSet` under test and once by a reference sampler fed the same MIDI.
//! Everything around the slot is exact arithmetic on the reference's samples (`raw * duck`,
//! `raw * gain`, `0.5 * (l + r)`), so the assertions are `==` on f32s: that is the form (a)
//! check of the port rules. The names of the Swift tests the behaviour comes from are on each
//! test. AUSampler's own rendering is not matched (Apple-only, PORTING.md section H), and the
//! synthetic font is mono, so a slot's width and side highpass are held to the Swift source and
//! to the properties below, not to an audio golden.
#![cfg(feature = "sf2")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use djrust::fx::MasterMacro;
use djrust::musicality::echo::EchoSpec;
use djrust::musicality::energy_curve::{EnergyCurve, SideHighpass, SweepFilter};
use djrust::musicality::fixes::Fixes;
use djrust::musicality::space::SpaceSpec;
use djrust::pflow::{NetBundle, Project};
use djrust::voices::SamplerSource;
use djrust::wave::net::str_hash;
use djrust::wave::runner::{SampleKey, SampleNote};
use djrust::wave::{Master, WaveOptions, WaveRunner};
use djrust::host::deck::samplers::SamplerSet;
use djrust::host::deck::{Command, DeckCore, DeckMirror};
use djrust::host::sampler::{synthetic_sf2, Sf2Sampler};
use std::collections::BTreeMap;

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

const SR: f64 = 22_050.0;
const BLOCK: usize = 256;

/// The synthetic SoundFont, written once per process (the tests run in parallel).
fn font(_tag: &str) -> String {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("djhost-deck-sampled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sine.sf2");
        std::fs::write(&path, synthetic_sf2(&[("Sine", 0, 0)])).unwrap();
        path.to_string_lossy().into_owned()
    })
    .clone()
}

fn sampler(path: &str) -> Sf2Sampler {
    Sf2Sampler::open(&SamplerSource::Sf2(path.into(), 0), SR as u32).unwrap()
}

/// Every tonal lane (and the live voice) plays slot 0.
fn bind_zero(_instrument: &str, _part: &str) -> Option<usize> {
    Some(0)
}

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

/// A kick on the beats and an offbeat bass (RiserTests' / DuckTests' shape), 120 BPM.
fn kick_bass() -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
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

/// A one-bar lead (the group `melody`) and nothing else.
fn lead() -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    p.nets.insert("melody", ring("melody", 4, &[(0, 72)], 100));
    p
}

fn base_options() -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        sample_binder: Some(bind_zero),
        ..Default::default()
    }
}

/// A runner with the project loaded and every role named in `mute` muted, and a sampler set with
/// slot 0 loaded, and a twin of that sampler to render the reference with.
struct Bench {
    runner: WaveRunner,
    set: SamplerSet,
    reference: Sf2Sampler,
    frame: i64,
}

impl Bench {
    fn new(p: &Project, opts: WaveOptions, mute: &[&str], play: bool) -> Bench {
        let path = font("bench");
        let mut runner = WaveRunner::new(SR, opts.clone());
        runner.load_prepared(WaveRunner::prepare(p, SR, &opts, 0));
        for role in mute {
            runner.set_role_muted(str_hash(role), true);
        }
        runner.set_tempo(p.tempo);
        if play {
            runner.play();
        }
        let mut set = SamplerSet::new(SR);
        set.set_slot(0, sampler(&path));
        Bench {
            runner,
            set,
            reference: sampler(&path),
            frame: 0,
        }
    }

    /// One block: the runner, then the samplers. Returns what the runner made alone, what the
    /// samplers added, and the reference sampler's raw block.
    fn block(&mut self) -> Block {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.set.begin_block(&self.runner, BLOCK);
        self.runner
            .process_at(&mut l, Some(&mut r), BLOCK, self.frame);
        let duck = self.runner.duck_gains(BLOCK).to_vec();
        let (before_l, before_r) = (l.clone(), r.clone());
        self.set
            .flush(&mut self.runner, self.frame, &mut l, &mut r, BLOCK);
        let (mut raw_l, mut raw_r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.reference.render_into(&mut raw_l, &mut raw_r);
        self.frame += BLOCK as i64;
        Block {
            before_l,
            before_r,
            l,
            r,
            raw_l,
            raw_r,
            duck,
        }
    }

    /// One block the way a deck plays it: the samplers rendered inside the runner's block
    /// (`process_with`), which with `.loudness` puts them in the master curve. Returns the block
    /// and the reference sampler's raw block.
    fn block_with_samplers(&mut self) -> CurveBlock {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.runner
            .process_with(&mut l, Some(&mut r), BLOCK, self.frame, Some(&mut self.set));
        let (mut raw_l, mut raw_r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.reference.render_into(&mut raw_l, &mut raw_r);
        self.frame += BLOCK as i64;
        CurveBlock { l, r, raw_l, raw_r }
    }

    fn key(&mut self, status: u8, d1: u8, d2: u8) {
        self.runner.sample_keys.push(SampleKey {
            slot: 0,
            status,
            data1: d1,
            data2: d2,
            frame: self.frame,
        });
    }

    /// A key down at the start of the next block, on the set and on the reference.
    fn key_on(&mut self, note: u8, velocity: u8) {
        self.key(0x90, note, velocity);
        self.reference.note_on(note, velocity);
    }

    fn note(&mut self, note: i64, dur_ms: f64, frame: i64) {
        self.runner.sample_notes.push(SampleNote {
            slot: 0,
            note,
            velocity: 100,
            dur_ms,
            frame,
            lane: 0,
        });
    }
}

struct CurveBlock {
    l: Vec<f32>,
    r: Vec<f32>,
    raw_l: Vec<f32>,
    raw_r: Vec<f32>,
}

struct Block {
    before_l: Vec<f32>,
    before_r: Vec<f32>,
    l: Vec<f32>,
    r: Vec<f32>,
    raw_l: Vec<f32>,
    raw_r: Vec<f32>,
    duck: Vec<f32>,
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// The amplitude of the sine at `hz` in `x` (a correlation with it, the Goertzel bin): how much
/// of a pitch there is in a stretch of the deck's output.
fn tone(x: &[f32], hz: f64) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        let a = 2.0 * std::f64::consts::PI * hz * i as f64 / SR;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

/// The pitch of a MIDI key on the synthetic font (a sine at middle C, key 60, repitched).
fn key_hz(key: i32) -> f64 {
    261.625_565 * 2f64.powf((key - 60) as f64 / 12.0)
}

// WaveRunner.renderScratch's duck: a sampled bass dips on the kick by the runner's own duck
// gain of each frame (DuckTests.sampledSlotsDuckToo)
#[test]
fn a_sampled_bass_slot_ducks_with_the_runners_gain_of_each_frame() {
    let mut opts = base_options();
    opts.duck_depth = 0.6;
    let mut b = Bench::new(&kick_bass(), opts, &["bass"], true);
    assert_ne!(
        b.runner.project().unwrap().duck_slots() & 1,
        0,
        "slot 0 ducks"
    );
    b.key_on(45, 110);
    let mut dipped = false;
    for i in 0..40 {
        let k = b.block();
        for n in 0..BLOCK {
            let want_l = k.before_l[n] + k.raw_l[n] * k.duck[n];
            let want_r = k.before_r[n] + k.raw_r[n] * k.duck[n];
            assert_eq!((k.l[n], k.r[n]), (want_l, want_r), "block {i} frame {n}");
        }
        dipped |= k.duck.iter().any(|&g| g < 0.9) && peak(&k.raw_l) > 0.01;
    }
    assert!(dipped, "the kick dipped a sounding slot");
}

// With no duck depth the slot is plain: the block plus the slot, exactly
#[test]
fn a_slot_of_a_project_without_a_duck_is_added_as_it_is() {
    let mut b = Bench::new(&kick_bass(), base_options(), &["bass"], true);
    b.key_on(45, 110);
    for _ in 0..8 {
        let k = b.block();
        for n in 0..BLOCK {
            assert_eq!(k.l[n], k.before_l[n] + k.raw_l[n]);
            assert_eq!(k.r[n], k.before_r[n] + k.raw_r[n]);
        }
    }
}

// WaveRunner.process reads the slot sets (duck, gain, sends, width) from the project playing at
// the TOP of the block (hand-built reference; no Swift test names it): a queued project that
// takes over at a bar in the middle of a block leaves the old project's sets in force for that
// block, and the next block is the new project's
#[test]
fn a_swap_in_the_middle_of_a_block_keeps_the_old_projects_slot_sets_for_that_block() {
    let mut ducking = base_options();
    ducking.duck_depth = 0.6;
    let mut b = Bench::new(&kick_bass(), ducking, &["bass"], true);
    assert_ne!(
        b.runner.project().unwrap().duck_slots() & 1,
        0,
        "the playing project ducks slot 0"
    );
    b.key_on(45, 110);
    for _ in 0..10 {
        b.block();
    }
    // a lead with no duck, whose one note falls half a bar after the swap, queued for the bar
    let lead_late = {
        let mut p = Project::default();
        p.tempo = 120.0;
        p.nets.insert("melody", ring("melody", 4, &[(8, 72)], 100));
        p
    };
    let next = WaveRunner::prepare(&lead_late, SR, &base_options(), 0);
    assert_eq!(next.duck_slots(), 0, "the queued project does not duck");
    b.runner.load_prepared(next);
    let old = b.runner.generation();
    let mut swap = None;
    for _ in 0..400 {
        let k = b.block();
        if b.runner.generation() != old {
            swap = Some(k);
            break;
        }
    }
    let k = swap.expect("the queued project took over at the bar");
    assert!(
        k.duck.iter().any(|&g| g < 1.0) && peak(&k.raw_l) > 0.01,
        "the old project's duck is still dipping a sounding slot"
    );
    for n in 0..BLOCK {
        let want_l = k.before_l[n] + k.raw_l[n] * k.duck[n];
        let want_r = k.before_r[n] + k.raw_r[n] * k.duck[n];
        assert_eq!((k.l[n], k.r[n]), (want_l, want_r), "swap block frame {n}");
    }
    let k = b.block();
    for n in 0..BLOCK {
        assert_eq!(k.l[n], k.before_l[n] + k.raw_l[n], "next block frame {n}");
        assert_eq!(k.r[n], k.before_r[n] + k.raw_r[n], "next block frame {n}");
    }
}

// SampledBalanceTests: the slot's gain (.balance) scales what it adds
#[test]
fn a_slots_balance_gain_scales_what_it_adds() {
    let mut opts = base_options();
    opts.musicality = Fixes::BALANCE;
    let mut trims = BTreeMap::new();
    trims.insert("bass".to_string(), 3.0);
    opts.track_balance = Some(trims);
    let mut b = Bench::new(&kick_bass(), opts, &["bass", "kick"], false);
    let gain = b.runner.project().unwrap().slot_gain(0);
    assert!(
        (gain - 1.0).abs() > 1e-4 && b.runner.project().unwrap().gain_slots() & 1 != 0,
        "slot 0 has a gain ({gain})"
    );
    b.key_on(45, 110);
    for _ in 0..6 {
        let k = b.block();
        for n in 0..BLOCK {
            assert_eq!(k.l[n], k.before_l[n] + k.raw_l[n] * gain);
            assert_eq!(k.r[n], k.before_r[n] + k.raw_r[n] * gain);
        }
    }
}

// SpaceTests.sampledInstrumentsSendToTheRoom: the slots that send put their mono mix, after
// gain and duck, in the room's input for the next block
#[test]
fn a_sending_slot_feeds_the_room_the_mono_mix_of_the_block() {
    let mut opts = base_options();
    opts.musicality = Fixes::SPACE;
    opts.space = Some(SpaceSpec::room());
    let mut b = Bench::new(&lead(), opts, &["melody"], false);
    assert_ne!(
        b.runner.project().unwrap().send_slots() & 1,
        0,
        "slot 0 sends"
    );
    b.key_on(60, 110);
    let k = b.block();
    assert!(peak(&k.raw_l) > 0.01, "the key sounds");
    let sent = b.set.room_send();
    assert_eq!(sent.len(), BLOCK);
    for n in 0..BLOCK {
        assert_eq!(sent[n], 0.5 * (k.raw_l[n] + k.raw_r[n]), "frame {n}");
    }
    // ...and the room returns it after its pre-delay, from a runner with nothing else playing
    let mut heard = 0f32;
    for _ in 0..60 {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        b.runner.process_at(&mut l, Some(&mut r), BLOCK, b.frame);
        b.frame += BLOCK as i64;
        heard = heard.max(peak(&l));
    }
    assert!(
        heard > 1e-4,
        "the room rings with what the slot sent ({heard})"
    );
    // a twin that was never sent anything is silent
    let mut twin = WaveRunner::new(SR, {
        let mut o = base_options();
        o.musicality = Fixes::SPACE;
        o.space = Some(SpaceSpec::room());
        o
    });
    let o2 = {
        let mut o = base_options();
        o.musicality = Fixes::SPACE;
        o.space = Some(SpaceSpec::room());
        o
    };
    twin.load_prepared(WaveRunner::prepare(&lead(), SR, &o2, 0));
    twin.set_role_muted(str_hash("melody"), true);
    let (mut tl, mut tr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    twin.process_at(&mut tl, Some(&mut tr), BLOCK, b.frame);
    assert_eq!(peak(&tl), 0.0);
}

// EchoTests.sampledLeadsEcho: a slot's echo send is its mono mix times the slot's send
#[test]
fn a_sampled_lead_feeds_the_echo_its_mono_mix_times_its_send() {
    let mut opts = base_options();
    opts.musicality = Fixes::ECHO;
    opts.echo = EchoSpec::for_genre("trance");
    let mut b = Bench::new(&lead(), opts, &["melody"], false);
    let p = b.runner.project().unwrap();
    assert!(p.echo.is_some() && p.echo_slots() & 1 != 0, "slot 0 echoes");
    let send = p.slot_echo(0);
    assert!(send > 0.0);
    b.key_on(72, 110);
    let k = b.block();
    let echoed = b.set.echo_send();
    for n in 0..BLOCK {
        let mono = 0.5 * (k.raw_l[n] + k.raw_r[n]);
        assert_eq!(echoed[n], mono * send, "frame {n}");
    }
    // the repeats come back after the echo's delay, from a runner that plays nothing itself
    let mut heard = 0f32;
    for _ in 0..200 {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        b.runner.process_at(&mut l, Some(&mut r), BLOCK, b.frame);
        b.frame += BLOCK as i64;
        heard = heard.max(peak(&l));
    }
    assert!(
        heard > 1e-4,
        "the echo repeats what the slot sent ({heard})"
    );
}

// samplerNote: a net note lasts at least 120 ms, so a one-step note still speaks
#[test]
fn a_net_note_sounds_for_at_least_120_ms() {
    let last_nonzero = |dur_ms: f64| {
        let mut b = Bench::new(&lead(), base_options(), &["melody"], false);
        b.note(60, dur_ms, 0);
        let mut last = 0usize;
        for blk in 0..40 {
            let k = b.block();
            for (n, v) in k.l.iter().enumerate() {
                if v.abs() > 1e-4 {
                    last = blk * BLOCK + n;
                }
            }
        }
        last
    };
    let short = last_nonzero(10.0);
    let floor = last_nonzero(120.0);
    let none = last_nonzero(0.0);
    let long = last_nonzero(300.0);
    let frames = |ms: f64| (ms / 1000.0 * SR) as usize;
    assert_eq!(short, floor, "10 ms is held for 120");
    assert_eq!(none, floor, "no length at all is 120 too");
    assert!(
        (frames(120.0)..frames(120.0) + frames(15.0)).contains(&floor),
        "ends at 120 ms plus the release ({floor})"
    );
    assert!(long > frames(300.0) && long < frames(330.0), "{long}");
}

// samplerNote: a note struck again while it is held is ended first, and the first one's end
// no longer cuts the second
#[test]
fn a_note_struck_again_while_held_is_ended_first() {
    let mut b = Bench::new(&lead(), base_options(), &["melody"], false);
    let at = |ms: f64| (ms / 1000.0 * SR) as i64;
    b.note(60, 500.0, 0);
    let mut loud_between = 0f32;
    for blk in 0..60 {
        if (blk * BLOCK) as i64 <= at(100.0) && at(100.0) < ((blk + 1) * BLOCK) as i64 {
            b.note(60, 500.0, at(100.0));
        }
        let k = b.block();
        for (n, v) in k.l.iter().enumerate() {
            let t = blk * BLOCK + n;
            if t > at(520.0) as usize && t < at(580.0) as usize {
                loud_between = loud_between.max(v.abs());
            }
        }
    }
    assert!(
        loud_between > 0.01,
        "the second strike still sounds past the first one's end ({loud_between})"
    );
}

// endSamplerNotes (a sampled kit lane's choke): a note-off the runner sends for a net note that is
// still held ends that hold, and the note's own end, owed at 120 ms, is never sent: the lane's
// `offNote`s are cleared. A key struck after the choke is not cut by it.
#[test]
fn a_choke_ends_the_hold_and_no_second_note_off_follows() {
    let mut b = Bench::new(&lead(), base_options(), &["melody"], false);
    let at = |ms: f64| (ms / 1000.0 * SR) as usize;
    b.note(60, 120.0, 0);
    let mut late = 0f32;
    for blk in 0..60 {
        let t = blk * BLOCK;
        if t <= at(30.0) && at(30.0) < t + BLOCK {
            b.key(0x80, 60, 0);
        }
        if t <= at(60.0) && at(60.0) < t + BLOCK {
            b.key(0x90, 60, 100);
        }
        let k = b.block();
        for (n, v) in k.l.iter().enumerate() {
            if (at(200.0)..at(300.0)).contains(&(t + n)) {
                late = late.max(v.abs());
            }
        }
    }
    assert!(
        late > 0.01,
        "the key struck after the choke still holds past the net note's end ({late})"
    );
}

// queue order: the live keys' MIDI keeps the order it was pressed in at one sample
#[test]
fn the_live_keys_midi_keeps_its_order() {
    let tail = |msgs: &[(u8, u8, u8)]| {
        let mut b = Bench::new(&lead(), base_options(), &["melody"], false);
        for &(s, d1, d2) in msgs {
            b.key(s, d1, d2);
        }
        let mut peak_late = 0f32;
        for blk in 0..12 {
            let k = b.block();
            if blk >= 6 {
                peak_late = peak_late.max(peak(&k.l));
            }
        }
        peak_late
    };
    assert!(
        tail(&[(0x80, 60, 0), (0x90, 60, 100)]) > 0.01,
        "off, then on: sounds"
    );
    assert!(
        tail(&[(0x90, 60, 100), (0x80, 60, 0)]) < 1e-4,
        "on, then off: released"
    );
    assert!(
        tail(&[(0x90, 60, 100), (0xB0, 123, 0)]) < 1e-4,
        "all notes off releases it"
    );
    assert!(
        tail(&[(0x90, 60, 100), (0xB0, 120, 0)]) < 1e-4,
        "all sound off silences it"
    );
    assert!(
        tail(&[(0xB0, 123, 0), (0x90, 60, 100)]) > 0.01,
        "a key after the all-off"
    );
}

/// A deck with a sampler on slot 0 that plays the live voice on it.
fn sampled_deck(opts: WaveOptions) -> (DeckCore, Arc<DeckMirror>) {
    let path = font("deck");
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", SR, opts.clone(), false, mirror.clone());
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(&lead(), SR, &opts, 0)));
    core.apply(Command::Tempo(120.0));
    core.apply(Command::Play);
    core.apply(Command::RoleMute {
        hash: str_hash("melody"),
        muted: true,
    });
    (core, mirror)
}

fn run(core: &mut DeckCore, frame: &mut i64, blocks: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut out = Vec::new();
    for _ in 0..blocks {
        core.render(&mut l, &mut r, *frame);
        *frame += BLOCK as i64;
        out.extend_from_slice(&l);
    }
    out
}

// WaveDeckSourceTests.sampledInstrumentsDoNotAllocate's keys, audibly: a live key on a sampled
// voice reaches the slot, holds until its note-off, and a panic or a stop ends it
#[test]
fn live_keys_on_a_sampled_voice_hold_and_a_stop_or_panic_ends_them() {
    for end in ["off", "all", "panic", "stop"] {
        let (mut core, mirror) = sampled_deck(base_options());
        assert_eq!(
            mirror.samplers_loaded.load(Ordering::Relaxed),
            0,
            "not published yet"
        );
        let mut frame = 0;
        run(&mut core, &mut frame, 4);
        assert_eq!(mirror.samplers_loaded.load(Ordering::Relaxed), 1);
        core.apply(Command::note_on(60, 110));
        let held = run(&mut core, &mut frame, 20);
        assert!(
            peak(&held[held.len() - 1000..]) > 0.01,
            "{end}: the key sounds"
        );
        let lasting = run(&mut core, &mut frame, 20);
        assert!(
            peak(&lasting) > 0.01,
            "{end}: and goes on sounding while held"
        );
        core.apply(match end {
            "off" => Command::note_off(60),
            "all" => Command::AllNotesOff,
            "panic" => Command::Panic,
            _ => Command::Stop,
        });
        let after = run(&mut core, &mut frame, 4);
        assert!(
            peak(&after[after.len() - 256..]) < 1e-4,
            "{end}: the note was released ({})",
            peak(&after[after.len() - 256..])
        );
    }
}

/// A stop under the vinyl brake silences the samplers outright (CC 120), a plain stop only
/// releases them (CC 123): the synthetic font's release is 1 ms, so both end here, but the
/// brake's path must not leave a held note.
#[test]
fn a_stop_under_the_brake_silences_the_samplers() {
    let mut opts = base_options();
    opts.master = Master::Limit;
    let path = font("brake");
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", SR, opts.clone(), true, mirror);
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(&lead(), SR, &opts, 0)));
    core.apply(Command::Play);
    core.apply(Command::RoleMute {
        hash: str_hash("melody"),
        muted: true,
    });
    let mut frame = 0;
    run(&mut core, &mut frame, 4);
    core.apply(Command::note_on(60, 110));
    core.apply(Command::MasterMacro {
        m: MasterMacro::VinylBrake,
        bars: 2,
    });
    let braked = run(&mut core, &mut frame, 20);
    assert!(peak(&braked) > 0.001, "the key sounds under the brake");
    core.apply(Command::Stop);
    let after = run(&mut core, &mut frame, 8);
    assert!(
        peak(&after[after.len() - 256..]) < 1e-4,
        "silent after the stop"
    );
}

/// A one-bar lead whose note (struck on step 12) holds 2 s on slot 0, under a 128-step structure
/// that ends in beats' `stop-transport` on its last step.
fn lead_to_the_end() -> Project {
    use djrust::gen::structure::build_control_bundle;
    use djrust::pflow::{ControlBinding, Section};
    let mut p = Project::default();
    p.tempo = 120.0;
    p.nets.insert("melody", ring("melody", 4, &[(12, 72)], 2000));
    p.structure = ["verse", "outro"]
        .iter()
        .map(|n| Section {
            name: (*n).into(),
            steps: 64,
            phrases: Default::default(),
        })
        .collect();
    let mut ctrl = BTreeMap::new();
    ctrl.insert(127, ControlBinding::new("stop-transport", ""));
    p.nets.insert(
        "struct-stop",
        build_control_bundle(128, &ctrl, true, "p", "t"),
    );
    p
}

/// `WaveRunner.stop()` is also what the runner calls on itself, at the song's `stop-transport`
/// and at a mix-out tail's last tick: it queues CC 123 on every sampler slot and clears the
/// lanes' pending note-offs (`samplersAllOff`, `clearOffs`). Here the notes' ends are the
/// host's, so the deck releases the keys when the runner stops itself: a sampled note that
/// still has seconds to hold is released within a block or two of the stop, not at its own
/// length (the synthetic font's release is 1 ms, so a released key is silent).
/// No Swift test names it (AUSampler); the Swift source is `WaveRunner.swift` 1600-1617, 1823-1854,
/// 2857-2860.
#[test]
fn a_song_that_stops_itself_releases_its_sampled_notes() {
    use djrust::wave::MixOutTail;
    for end in ["stop-transport", "mix-out tail"] {
        let mut opts = base_options();
        if end == "mix-out tail" {
            // the rests start after the first strike (tick 12) and the deck stops at tick 20
            opts.tail = Some(MixOutTail::new(20, 0));
        }
        let path = font("selfstop");
        let mirror = Arc::new(DeckMirror::new());
        let mut core = DeckCore::new("A", SR, opts.clone(), false, mirror.clone());
        core.apply(Command::Sampler(0, sampler(&path)));
        core.apply(Command::Load(WaveRunner::prepare(
            &lead_to_the_end(),
            SR,
            &opts,
            0,
        )));
        core.apply(Command::Tempo(480.0));
        core.apply(Command::Play);
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        let mut frame = 0;
        let mut blocks = Vec::new();
        for _ in 0..600 {
            core.render(&mut l, &mut r, frame);
            frame += BLOCK as i64;
            blocks.push(peak(&l).max(peak(&r)));
            if mirror.stopped.load(Ordering::Relaxed) {
                break;
            }
        }
        assert!(
            mirror.stopped.load(Ordering::Relaxed),
            "{end}: the runner stopped itself"
        );
        let at = blocks.len() - 1;
        assert!(
            blocks[at.saturating_sub(2)..].iter().any(|&p| p > 0.01),
            "{end}: the 2 s note is sounding as the deck stops ({:?})",
            &blocks[at.saturating_sub(4)..]
        );
        let after = run(&mut core, &mut frame, 3);
        assert!(
            peak(&after[after.len() - BLOCK..]) < 1e-4,
            "{end}: released by the stop, not 1.7 s later ({})",
            peak(&after[after.len() - BLOCK..])
        );
        assert!(
            core.samplers.owed_notes().is_empty(),
            "{end}: and no note-off is still owed"
        );
    }
}

/// The same self-stop, found by the vinyl brake's release instead of by a rendered block:
/// `TurntableBrake` puts the runner's clock on by the ticks the braked deck lagged
/// (`WaveRunner.catchUp`), and the song's `stop-transport` falls inside that jump. The runner
/// stops itself there (`WaveRunner.swift` 2857-2860), in `catch_up`, not in `process`, so it is
/// `DeckSource::catch_up` that releases the sampled notes. The brake fires at tick 100 of a
/// 128-tick song with a 2-bar duration (32 ticks): it spins the net on 12 ticks and the release
/// catches up the other 20, across tick 127.
#[test]
fn a_stop_found_by_the_brakes_catch_up_releases_the_sampled_notes() {
    let mut opts = base_options();
    opts.master = Master::Limit;
    let path = font("catchup");
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", SR, opts.clone(), true, mirror.clone());
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(
        &lead_to_the_end(),
        SR,
        &opts,
        0,
    )));
    core.apply(Command::Tempo(480.0));
    core.apply(Command::Play);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut frame = 0;
    let mut fired = false;
    // the tick at the end of the last block that was not the stopping one, and what it owed
    let (mut last_tick, mut owed_then) = (0, Vec::new());
    for _ in 0..800 {
        if !fired && mirror.tick.load(Ordering::Relaxed) >= 100 {
            core.apply(Command::MasterMacro {
                m: MasterMacro::VinylBrake,
                bars: 2,
            });
            fired = true;
        }
        core.render(&mut l, &mut r, frame);
        frame += BLOCK as i64;
        if mirror.stopped.load(Ordering::Relaxed) {
            break;
        }
        last_tick = mirror.tick.load(Ordering::Relaxed);
        owed_then = core.samplers.owed_notes();
    }
    assert!(fired, "the brake was fired");
    assert!(
        mirror.stopped.load(Ordering::Relaxed),
        "the runner stopped itself"
    );
    // the block before it ended on tick 112, and a block is a third of a tick: the stop is the
    // release's jump across tick 127, found in `catch_up` and not in a rendered block
    assert!(
        (100..120).contains(&last_tick),
        "the brake held the net's clock short of the stop ({last_tick})"
    );
    assert!(
        !owed_then.is_empty(),
        "the 2 s notes were still held when the release found the stop"
    );
    // the hook is the only thing that ends them: the notes' own ends are more than a second off
    let after = run(&mut core, &mut frame, 3);
    assert!(
        peak(&after[after.len() - BLOCK..]) < 1e-4,
        "released by the stop that catch_up found ({})",
        peak(&after[after.len() - BLOCK..])
    );
    assert!(
        core.samplers.owed_notes().is_empty(),
        "and no note-off is still owed"
    );
}

/// A lead (channel 4, the key 84) and a bass (channel 6, the key 36), each striking a note that
/// holds 2 s, both on sampler slot 0 (`bind_zero`).
fn two_held_lanes() -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    p.nets.insert("melody", ring("melody", 4, &[(0, 84)], 2000));
    p.nets.insert("bass", ring("bass", 6, &[(0, 36)], 2000));
    p
}

fn to_synth(channel: i64, on: bool) -> Command {
    use djrust::wave::{VoiceLaneParams, VoiceTweak};
    Command::Voice(VoiceLaneParams::new(
        channel,
        &VoiceTweak::default(),
        on,
        None,
        SR,
    ))
}

/// `WaveRunner.applyVoice`: "use synth voice" on a sampled channel queues a note-off for each of
/// the notes THAT LANE holds and clears its queue (`lane.offNote`, `offCount = 0`). Another
/// lane's notes on the same slot are left to hold to their ends. (Swift's tests drive the switch
/// only for allocation; the scope is the Swift source, 1679-1689.)
#[test]
fn use_synth_voice_ends_only_that_lanes_notes() {
    let opts = base_options();
    let path = font("synthscope");
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", SR, opts.clone(), false, mirror);
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(
        &two_held_lanes(),
        SR,
        &opts,
        0,
    )));
    core.apply(Command::Tempo(120.0));
    core.apply(Command::Play);
    let mut frame = 0;
    let held = run(&mut core, &mut frame, 4);
    let mut owed = core.samplers.owed_notes();
    owed.sort();
    assert_eq!(owed, vec![(0, 36), (0, 84)], "both lanes hold their note");
    // (the notes are on different keys, so each lane has a pitch of its own to listen for)
    let (lead_hz, bass_hz) = (key_hz(84), key_hz(36));
    let (lead_held, bass_held) = (tone(&held[BLOCK..], lead_hz), tone(&held[BLOCK..], bass_hz));
    assert!(
        lead_held > 0.02 && bass_held > 0.02,
        "both notes sound before the switch: lead {lead_held}, bass {bass_held}"
    );
    core.apply(to_synth(4, true));
    run(&mut core, &mut frame, 1);
    assert_eq!(
        core.samplers.owed_notes(),
        vec![(0, 36)],
        "the lead's note was ended, the bass's is still held"
    );
    // the lead's note-off reached the sampler: its pitch is gone from what the deck plays, while
    // the bass goes on sounding (it was not cut by an all-notes-off)
    let after = run(&mut core, &mut frame, 8);
    let (lead_after, bass_after) = (tone(&after, lead_hz), tone(&after, bass_hz));
    assert!(
        lead_after < 0.02 * lead_held,
        "the lead's sampled note was released, not left to ring its 2 s ({lead_after} of {lead_held})"
    );
    assert!(
        bass_after > 0.5 * bass_held,
        "the bass note rings on ({bass_after} of {bass_held})"
    );
}

/// The same switch with one lane only, and nothing but the sound to go by: a channel that goes
/// to its synth voice while its sampled note has seconds to run is silent within two blocks
/// (the synthetic font's release is 1 ms), where without the switch the note is still ringing.
#[test]
fn use_synth_voice_silences_the_sampled_note_its_lane_holds() {
    let mut one = Project {
        tempo: 120.0,
        ..Default::default()
    };
    one.nets
        .insert("melody", ring("melody", 4, &[(0, 72)], 2000));
    for switch in [false, true] {
        let opts = base_options();
        let path = font("synthsilence");
        let mut core = DeckCore::new("A", SR, opts.clone(), false, Arc::new(DeckMirror::new()));
        core.apply(Command::Sampler(0, sampler(&path)));
        core.apply(Command::Load(WaveRunner::prepare(&one, SR, &opts, 0)));
        core.apply(Command::Tempo(120.0));
        core.apply(Command::Play);
        let mut frame = 0;
        let held = run(&mut core, &mut frame, 6);
        assert!(
            peak(&held[held.len() - BLOCK..]) > 0.01,
            "the lane's 2 s note sounds"
        );
        if switch {
            core.apply(to_synth(4, true));
        }
        let after = run(&mut core, &mut frame, 2);
        let last = peak(&after[BLOCK..]);
        if switch {
            assert!(
                last < 1e-4,
                "the lane's note was ended with the switch ({last})"
            );
            assert!(core.samplers.owed_notes().is_empty());
        } else {
            assert!(
                last > 0.01,
                "without the switch the note rings on, so the silence above is the end's ({last})"
            );
            assert_eq!(core.samplers.owed_notes(), vec![(0, 72)]);
        }
    }
}

/// The lane-scoped end costs the audio thread nothing (the queue and the pending list are
/// preallocated): a channel to its synth voice and back, with notes held, allocates and frees
/// nothing.
#[test]
fn ending_a_lanes_notes_allocates_and_frees_nothing() {
    let opts = base_options();
    let path = font("synthrt");
    let mut core = DeckCore::new("A", SR, opts.clone(), false, Arc::new(DeckMirror::new()));
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(
        &two_held_lanes(),
        SR,
        &opts,
        0,
    )));
    core.apply(Command::Tempo(120.0));
    core.apply(Command::Play);
    let mut frame = 0;
    run(&mut core, &mut frame, 40);
    let (l, r) = (to_synth(4, true), to_synth(4, false));
    let (mut bl, mut br) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (_, a, f) = count(|| {
        for c in [l, r] {
            core.apply(c);
            for _ in 0..60 {
                core.render(&mut bl, &mut br, frame);
                frame += BLOCK as i64;
            }
        }
    });
    assert_eq!((a, f), (0, 0), "{a} allocations, {f} frees");
}

/// Swift keeps the note-offs per lane (`Lane.offNote` / `offAt`): a note struck again ends the
/// held one only on the SAME lane (`samplerNote`: off, then on). Another lane that strikes the
/// same key on the shared slot (all lanes of one source) holds its own note to its end.
#[test]
fn a_note_struck_by_another_lane_does_not_end_the_held_one() {
    let mut p = Project::default();
    p.tempo = 120.0;
    p.nets.insert("melody", ring("melody", 4, &[(0, 60)], 2000));
    p.nets.insert("bass", ring("bass", 6, &[(1, 60)], 2000));
    let opts = base_options();
    let path = font("twolanes");
    let mut core = DeckCore::new("A", SR, opts.clone(), false, Arc::new(DeckMirror::new()));
    core.apply(Command::Sampler(0, sampler(&path)));
    core.apply(Command::Load(WaveRunner::prepare(&p, SR, &opts, 0)));
    core.apply(Command::Tempo(120.0));
    core.apply(Command::Play);
    let mut frame = 0;
    run(&mut core, &mut frame, 20); // 232 ms: both lanes have struck (step 1 falls at 125 ms)
    assert_eq!(
        core.samplers.owed_notes(),
        vec![(0, 60), (0, 60)],
        "each lane owes the end of its own note"
    );
}

/// ... and a live key held on the same slot (the live voice plays the melody's sampler) plays on:
/// the switch is not CC 123 for the slot.
#[test]
fn a_held_live_key_plays_on_when_a_channel_goes_to_its_synth_voice() {
    let (mut core, _mirror) = sampled_deck(base_options());
    let mut frame = 0;
    run(&mut core, &mut frame, 4);
    core.apply(Command::note_on(60, 110));
    let held = run(&mut core, &mut frame, 20);
    assert!(peak(&held[held.len() - 1000..]) > 0.01, "the key sounds");
    core.apply(to_synth(4, true));
    let after = run(&mut core, &mut frame, 20);
    assert!(
        peak(&after[after.len() - 1000..]) > 0.01,
        "the live key still sounds after the melody channel took its synth voice"
    );
    core.apply(to_synth(4, false));
    core.apply(Command::note_off(60));
    let end = run(&mut core, &mut frame, 4);
    assert!(peak(&end[end.len() - BLOCK..]) < 1e-4, "its note-off ends it");
}

/// A kick, a bass and a lead on one sampler slot (the three groups that duck, send and echo).
fn band() -> Project {
    let mut p = kick_bass();
    p.nets
        .insert("melody", ring("melody", 4, &[(0, 72), (8, 75)], 100));
    p.nets.insert(
        "harmony",
        ring("harmony", 7, &[(0, 60), (1, 64), (2, 67)], 1900),
    );
    p
}

/// `WaveDeckSourceTests.sampledInstrumentsDoNotAllocate`, with the sampled slots' own path on:
/// the kick's duck, the balance gain, the room, the echo, the stereo width, the live keys' MIDI,
/// a stop and a panic (all-notes-off on every slot), a swap, the master chain, over thousands of
/// blocks. Nothing is allocated or freed on the render thread.
#[test]
fn sampled_slots_with_every_send_allocate_and_free_nothing() {
    sampled_slots_cost_the_render_path_nothing(
        Fixes::SPACE | Fixes::SHIMMER | Fixes::ECHO | Fixes::BALANCE | Fixes::STEREO,
    );
}

/// The same, in the master curve (`samplersInCurve`): `.loudness` and the section energy curve on
/// top, so the slots render into the runner's scratch pair and join the voices before the curve,
/// through the energy lowpass and gain, with the mono lows under `.stereo`.
#[test]
fn sampled_slots_in_the_curve_allocate_and_free_nothing() {
    sampled_slots_cost_the_render_path_nothing(
        Fixes::LOUDNESS
            | Fixes::ENERGY
            | Fixes::SPACE
            | Fixes::SHIMMER
            | Fixes::ECHO
            | Fixes::BALANCE
            | Fixes::STEREO,
    );
}

fn sampled_slots_cost_the_render_path_nothing(fixes: Fixes) {
    let mut opts = base_options();
    opts.musicality = fixes;
    if fixes.contains(Fixes::ENERGY) {
        // (a project with sections has a curve of its own; this one has none to measure)
        opts.track_energy = Some(EnergyCurve {
            start: vec![0, 16, 48],
            gain: vec![0.6, 1.0, 0.8],
            cutoff: vec![2_000.0, 18_000.0, 9_000.0],
            ramp: vec![false, true, false],
        });
    }
    opts.space = Some(SpaceSpec::room());
    opts.echo = EchoSpec::for_genre("trance");
    opts.duck_depth = 0.5;
    opts.master = Master::Limit;
    let path = font("rt");
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", SR, opts.clone(), false, mirror.clone());
    core.apply(Command::Sampler(0, sampler(&path)));
    let prepared = WaveRunner::prepare(&band(), SR, &opts, 0);
    {
        let p = &prepared;
        assert!(
            p.duck_slots() & 1 != 0 && p.send_slots() & 1 != 0,
            "slot 0 ducks and sends"
        );
        assert!(p.echo_slots() & 1 != 0, "and echoes");
    }
    core.apply(Command::Load(prepared));
    core.apply(Command::Tempo(240.0));
    core.apply(Command::Play);
    let next = WaveRunner::prepare(&band(), SR, &opts, 0);
    let mut frame = 0i64;
    run(&mut core, &mut frame, 400);
    assert_eq!(mirror.samplers_loaded.load(Ordering::Relaxed), 1);
    // (the commands are the main thread's: applied here, uncounted; the renders are measured)
    let (mut allocs, mut frees) = (0, 0);
    let mut bl = vec![0f32; BLOCK];
    let mut br = vec![0f32; BLOCK];
    let mut measure = |core: &mut DeckCore, cmds: Vec<Command>, blocks: usize| {
        for c in cmds {
            core.apply(c);
        }
        let (_, a, f) = count(|| {
            for _ in 0..blocks {
                core.render(&mut bl, &mut br, frame);
                frame += BLOCK as i64;
            }
        });
        allocs += a;
        frees += f;
    };
    measure(&mut core, vec![], 2000);
    measure(
        &mut core,
        vec![
            Command::note_on(52, 100),
            Command::note_on(40, 90),
            Command::note_off(52),
        ],
        200,
    );
    measure(&mut core, vec![Command::AllNotesOff, Command::Panic], 100);
    measure(&mut core, vec![Command::note_on(64, 100)], 100);
    measure(&mut core, vec![Command::Stop], 50);
    measure(
        &mut core,
        vec![Command::Play, Command::note_on(60, 110)],
        300,
    );
    measure(
        &mut core,
        vec![
            Command::MasterMacro {
                m: MasterMacro::PhaserDrone,
                bars: -1,
            },
            Command::MasterMacro {
                m: MasterMacro::OctaveDown,
                bars: -1,
            },
            Command::Load(next),
        ],
        1500,
    );
    assert_eq!(
        (allocs, frees),
        (0, 0),
        "sampled slots ({fixes:?}): {allocs} allocations, {frees} frees"
    );
    if fixes.contains(Fixes::LOUDNESS) {
        assert!(
            mirror.samplers_loaded.load(Ordering::Relaxed) == 1,
            "the slot was in the render"
        );
    }
}

// ---- samplersInCurve ---------------------------------------------------------------------------
//
// With `.loudness` Swift's runner does not add its samplers to the finished block: they render
// into a scratch pair that joins the voices *before* the master curve, scaled by the track gain
// alone (`WaveRunner.process`'s `inCurve` branch and `finishEnergy`'s `samplersIn`; the voices are
// scaled by the master gain as well). So the measured loudness gain and the section's energy gain
// and lowpass reach a sampled mix, and the master curve limits the sum. The synthetic font has no
// AUSampler golden to be held to; these are hand-built references with exact f32 equality, form
// (a) of the port rules, from the Swift source.

fn curve_options(master: Master, track_gain: f64) -> WaveOptions {
    let mut o = base_options();
    o.musicality = Fixes::LOUDNESS;
    o.master = master;
    o.track_gain = Some(track_gain);
    o
}

/// The voices alone, a block at a time: a runner of its own with no host, on `Master::Limit`, where
/// the output is the voices' sum at the master and track gain as a float (`preL`) on both sides.
struct Voices {
    bench: Bench,
}

impl Voices {
    fn new(p: &Project, opts: &WaveOptions, mute: &[&str], play: bool) -> Voices {
        let mut o = opts.clone();
        o.master = Master::Limit;
        Voices {
            bench: Bench::new(p, o, mute, play),
        }
    }

    fn block(&mut self) -> (Vec<f32>, Vec<f32>) {
        let b = &mut self.bench;
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        b.runner.process_at(&mut l, Some(&mut r), BLOCK, b.frame);
        b.frame += BLOCK as i64;
        (l, r)
    }
}

// The sum, as `process` makes it: `(preL + s) + trackGain * smpL`, through the curve (linear: as
// it is; soft: tanh). The voices are a synth kick (which sounds), the slot a live key; under
// `.stereo` the voices are left out (their mid and side are not the twin's to give) and what is
// held is the slot's lows made mono first.
#[test]
fn with_loudness_the_samplers_join_the_voices_before_the_curve_at_the_track_gain() {
    for (master, fixes, kicks) in [
        (Master::Limit, Fixes::LOUDNESS, true),
        (Master::Linear, Fixes::LOUDNESS, true),
        (Master::Soft, Fixes::LOUDNESS, true),
        (Master::Limit, Fixes::LOUDNESS | Fixes::STEREO, false),
    ] {
        let mut opts = curve_options(master, 0.5);
        opts.musicality = fixes;
        let (project, mute) = if kicks {
            (kick_bass(), vec!["bass"])
        } else {
            (lead(), vec!["melody"])
        };
        let mut b = Bench::new(&project, opts.clone(), &mute, true);
        let mut twin = Voices::new(&project, &opts, &mute, true);
        b.key_on(45, 110);
        let tg = 0.5f32;
        let wide = fixes.contains(Fixes::STEREO);
        let mut lows = SideHighpass::new(150.0, SR);
        let (mut voiced, mut sampled, mut differs) = (false, false, false);
        for i in 0..14 {
            let k = b.block_with_samplers();
            let (pre_l, pre_r) = twin.block();
            voiced |= peak(&pre_l) > 0.01;
            sampled |= peak(&k.raw_l) > 0.01;
            // under `.stereo` the slots' lows are made mono first (`monoSamplerLows`, 150 Hz)
            let (mut smp_l, mut smp_r) = (k.raw_l.clone(), k.raw_r.clone());
            if wide {
                for n in 0..BLOCK {
                    lows.process(&mut smp_l[n], &mut smp_r[n]);
                }
            }
            for n in 0..BLOCK {
                let (a, c) = (
                    (pre_l[n] + 0.0) + tg * smp_l[n],
                    (pre_r[n] - 0.0) + tg * smp_r[n],
                );
                let (want_l, want_r) = if master == Master::Soft {
                    ((a as f64).tanh() as f32, (c as f64).tanh() as f32)
                } else {
                    (a, c)
                };
                // (soft: `libm::tanh` against the platform's, which can sit an ulp apart)
                let tol = if master == Master::Soft { 2e-7 } else { 0.0 };
                assert!(
                    (k.l[n] - want_l).abs() <= tol && (k.r[n] - want_r).abs() <= tol,
                    "{master:?} {fixes:?} block {i} frame {n}: {} {} vs {want_l} {want_r}",
                    k.l[n],
                    k.r[n]
                );
                differs |= k.l[n] != pre_l[n] + k.raw_l[n] && k.raw_l[n] != 0.0;
            }
        }
        assert!(voiced || !kicks, "{master:?}: the kick sounds");
        assert!(sampled, "{master:?}: the key sounds");
        assert!(
            differs,
            "{master:?}: at the track gain, not as the plain sum the slot adds after the curve"
        );
    }
}

// Without `.loudness` nothing changes: the slot is added to the finished block at unity, whatever
// the track gain (`flushSamplers` after the curve).
#[test]
fn without_loudness_the_samplers_are_added_after_the_curve_as_before() {
    let mut opts = base_options();
    opts.master = Master::Limit;
    opts.track_gain = Some(0.5);
    let mut b = Bench::new(&kick_bass(), opts.clone(), &["bass"], true);
    let mut twin = Voices::new(&kick_bass(), &opts, &["bass"], true);
    b.key_on(45, 110);
    let mut sampled = false;
    for i in 0..8 {
        let k = b.block_with_samplers();
        let (tl, tr) = twin.block();
        sampled |= peak(&k.raw_l) > 0.01;
        for n in 0..BLOCK {
            assert_eq!(k.l[n], tl[n] + k.raw_l[n], "block {i} frame {n}");
            assert_eq!(k.r[n], tr[n] + k.raw_r[n], "block {i} frame {n}");
        }
    }
    assert!(sampled, "the key sounds");
}

// `finishEnergy`'s `samplersIn`: the slot joins the tonal voices before the section's lowpass and
// gain, `(dry + lowpass(tone + trackGain * smp)) * g`, so the energy curve reaches a sampled mix.
// A runner that is not playing stays on step 0 of the curve, whose gain and cutoff are then
// constant, so the reference is a lowpass of its own run over the same samples.
#[test]
fn with_loudness_the_energy_curve_gains_and_lowpasses_the_samplers_too() {
    let curve = EnergyCurve {
        start: vec![0, 256],
        gain: vec![0.5, 1.0],
        cutoff: vec![1_500.0, 18_000.0],
        ramp: vec![false, false],
    };
    let (gain, cutoff) = curve.at(0);
    assert_eq!((gain, cutoff), (0.5, 1_500.0));
    let mut opts = curve_options(Master::Limit, 0.5);
    opts.musicality = Fixes::LOUDNESS | Fixes::ENERGY;
    opts.track_energy = Some(curve);
    let mut b = Bench::new(&lead(), opts, &["melody"], false);
    assert!(b.runner.project().unwrap().energy.is_some());
    b.key_on(96, 110);
    let (mut lp_l, mut lp_r) = (SweepFilter::default(), SweepFilter::default());
    let (mut loud, mut dulled) = (0f32, false);
    for i in 0..12 {
        let k = b.block_with_samplers();
        lp_l.tune(cutoff, SR);
        lp_r.tune(cutoff, SR);
        for n in 0..BLOCK {
            let sl = (0.5f32 * k.raw_l[n]) as f64;
            let sr = (0.5f32 * k.raw_r[n]) as f64;
            let want_l = ((0.0f64 + lp_l.process(0.0 + sl)) * gain) as f32;
            let want_r = ((0.0f64 + lp_r.process(0.0 + sr)) * gain) as f32;
            assert_eq!((k.l[n], k.r[n]), (want_l, want_r), "block {i} frame {n}");
        }
        loud = loud.max(peak(&k.raw_l));
        // the key is an octave and a half over the 1.5 kHz cutoff: what comes out is under the
        // gain and trim alone
        dulled |= peak(&k.l) < 0.5 * 0.5 * peak(&k.raw_l) * 0.9 && peak(&k.raw_l) > 0.01;
    }
    assert!(loud > 0.01, "the key sounds");
    assert!(dulled, "the section's lowpass took the highs off the slot");
}

// The deck renders its samplers inside the runner's block too: one core with `.loudness` at a
// track gain of a half and one without, the same key, nothing else sounding: the first is the
// second's slot at the track gain, exactly (`DeckCore::render` through the brake and the chain).
#[test]
fn a_deck_plays_its_samplers_in_the_curve() {
    let render = |loudness: bool| -> Vec<f32> {
        let mut opts = base_options();
        opts.master = Master::Linear;
        opts.track_gain = Some(0.5);
        if loudness {
            opts.musicality = Fixes::LOUDNESS;
        }
        let (mut core, _) = sampled_deck(opts);
        let mut frame = 0;
        run(&mut core, &mut frame, 2);
        core.apply(Command::note_on(60, 110));
        run(&mut core, &mut frame, 24)
    };
    let (plain, curved) = (render(false), render(true));
    assert!(peak(&plain) > 0.01, "the key sounds");
    for (i, (p, c)) in plain.iter().zip(&curved).enumerate() {
        assert_eq!(*c, 0.5 * *p, "frame {i}");
    }
}

// ---- eviction, the audio thread's half ----------------------------------------------------

/// A second SoundFont (two presets), for a slot to be given to another source.
fn other_font() -> String {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("djhost-deck-sampled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.sf2");
        std::fs::write(&path, synthetic_sf2(&[("Sine", 0, 0), ("Sine 2", 0, 1)])).unwrap();
        path.to_string_lossy().into_owned()
    })
    .clone()
}

/// `SamplerBank.evict` / the publish after it, on the audio thread's side: a slot that another
/// source takes over (the bank had no reference on it) is cut, its sampler goes to the retire
/// list for the main thread to free, and the new one plays in its place.
#[test]
fn a_slot_another_source_takes_over_is_cut_and_its_sampler_retired() {
    let path = font("takeover");
    let (mut core, _m) = sampled_deck(base_options());
    let mut frame = 0;
    run(&mut core, &mut frame, 4);
    core.apply(Command::note_on(60, 110));
    let held = run(&mut core, &mut frame, 20);
    assert!(peak(&held[held.len() - 1000..]) > 0.01, "the key sounds");
    let incoming = Sf2Sampler::open(&SamplerSource::Sf2(other_font(), 1), SR as u32).unwrap();
    core.apply(Command::Sampler(0, incoming));
    // the old sampler's held key went with it (no note-off was sent: the new one never heard it)
    let after = run(&mut core, &mut frame, 4);
    assert!(
        peak(&after[after.len() - 256..]) < 1e-4,
        "the old sampler's note is cut ({})",
        peak(&after[after.len() - 256..])
    );
    let old = core
        .samplers
        .pop_retired()
        .expect("the replaced sampler is retired");
    assert_eq!(old.source, SamplerSource::Sf2(path, 0));
    assert!(core.samplers.pop_retired().is_none());
    assert_eq!(core.samplers.loaded(), 1);
    // the slot plays its new sampler
    core.apply(Command::note_on(60, 110));
    let again = run(&mut core, &mut frame, 20);
    assert!(
        peak(&again[again.len() - 1000..]) > 0.01,
        "the new sampler sounds"
    );
}

/// The same source sent again (its slot was given up and the same source loaded into it again
/// before anything else was sent there) leaves the sampler that is playing alone, and the one that
/// came is retired, not dropped on the audio thread.
#[test]
fn the_same_source_sent_again_leaves_the_playing_sampler_alone() {
    let path = font("again");
    let (mut core, _m) = sampled_deck(base_options());
    let mut frame = 0;
    run(&mut core, &mut frame, 4);
    core.apply(Command::note_on(60, 110));
    run(&mut core, &mut frame, 20);
    core.apply(Command::Sampler(0, sampler(&path)));
    let still = run(&mut core, &mut frame, 10);
    assert!(peak(&still) > 0.01, "the held key goes on sounding");
    let spare = core
        .samplers
        .pop_retired()
        .expect("the new copy is retired");
    assert_eq!(spare.source, SamplerSource::Sf2(path, 0));
    assert!(core.samplers.pop_retired().is_none());
    assert_eq!(core.samplers.loaded(), 1);
}

/// `WaveDeckSourceTests.sampledInstrumentsDoNotAllocate`'s eviction half: a slot taken over while
/// the deck renders (the replacement command, and a copy of the playing source, which is retired
/// too) costs the render thread no allocation and no free; the old samplers wait for the main
/// thread.
#[test]
fn replacing_a_slot_while_rendering_allocates_and_frees_nothing() {
    let path = font("evict-rt");
    let (mut core, _m) = sampled_deck(base_options());
    let mut frame = 0;
    run(&mut core, &mut frame, 4);
    core.apply(Command::note_on(60, 110));
    run(&mut core, &mut frame, 20);
    let other = Sf2Sampler::open(&SamplerSource::Sf2(other_font(), 1), SR as u32).unwrap();
    let copy = Sf2Sampler::open(&SamplerSource::Sf2(other_font(), 1), SR as u32).unwrap();
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (_, allocs, frees) = count(|| {
        core.apply(Command::Sampler(0, other));
        for _ in 0..30 {
            core.render(&mut l, &mut r, frame);
            frame += BLOCK as i64;
        }
        core.apply(Command::Sampler(0, copy));
        for _ in 0..30 {
            core.render(&mut l, &mut r, frame);
            frame += BLOCK as i64;
        }
    });
    assert_eq!(
        (allocs, frees),
        (0, 0),
        "the render thread allocated or freed"
    );
    let first = core.samplers.pop_retired().expect("the copy");
    let second = core.samplers.pop_retired().expect("the replaced sampler");
    assert_eq!(first.source, SamplerSource::Sf2(other_font(), 1));
    assert_eq!(second.source, SamplerSource::Sf2(path, 0));
    assert_eq!(core.samplers.retire_overflow, 0);
}
