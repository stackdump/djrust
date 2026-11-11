//! What the live hosts (`djapp`, `djctl play`) set on a fresh engine, and the defaults the engine
//! itself starts from (package A1).
//!
//! - `host_engine` (`djrust::host::deck::host`): `deterministic` off for a host with real audio, so a
//!   seed-wheel click, the APC's regenerate, Wander's swaps and its look-ahead, auto-mix's loads
//!   and the next track compose on a worker and `Rig::tick` lands them; sample auto-gain on
//!   (`AppModel.applyAutoGainPreference`, ahead of the first load).
//! - The headphone cue starts cue-only at level 0.7 (`DJEngine._headphoneMix`, `_headphoneLevel`):
//!   `CueOutputTests.theMasterMixIsOnTheFirstPairOnly`, `theHeadphoneMixBlendsInTheMaster`,
//!   `CueControlTests.apcFaderFiveLeavesTheHeadphoneMix`.
//! - A scan's result (`Rig::hotplug`): a replugged controller is painted again
//!   (`ControllerHub.rescan` -> `router.keepOnly`).
//! - A folder holding only the Minecraft bank is a library the engine can use
//!   (`Library::has_banks`; `MinecraftBank.isInstalled`).
//!
//! The Swift suites hold the engine's behaviour once these are set (`deck_generate.rs`,
//! `deck_wander.rs`, `deck_automix.rs`, `deck_level.rs`, `deck_output.rs`); this file holds the
//! wiring: what a host gets from the constructor it calls.

use std::sync::{Arc, Mutex};

use djrust::deck::level_match::LevelMatch;
use djrust::deck::turntable::SampleBuffer;
use djrust::journal::event::JournalEvent;
use djrust::mixer::MixerState;
use djrust::musicality::loudness::IntegratedLoudness;
use djrust::session::action::DeckAction;
use djrust::TrackSpec;
use djrust::host::deck::output::{Interleaver, Routing};
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, host_engine, Engine, EngineConfig, JournalSink, Renderer, Rig};

const SR: f64 = 22_050.0;
const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";

fn config(sr: f64) -> EngineConfig {
    let mut c = EngineConfig::new(sr);
    c.extended = false;
    c
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

// MARK: the headphone cue's starting positions (CueOutputTests, CueControlTests)

/// Four output channels as `CueOutputTests.make()` has them: the master on 1/2, the cue on 3/4,
/// both decks on the demo loop. `frames` of it, de-interleaved.
fn four_channels(
    mix: Option<f64>,
    play: impl FnOnce(&mut Engine),
    frames: usize,
) -> (Engine, Vec<Vec<f32>>) {
    let (mut e, mut r) = engine(config(SR));
    assert!(e.load_demo("A") && e.load_demo("B"));
    if let Some(m) = mix {
        e.set_headphone_mix(m);
    }
    play(&mut e);
    let routing = Routing::Pairs {
        master_pair: 0,
        cue_pair: 2,
    };
    let mut inter = Interleaver::new();
    let mut chans = vec![Vec::new(); 4];
    let mut out = vec![0f32; 4 * 512];
    while chans[0].len() < frames {
        inter.fill(&mut r, &routing, 4, &mut out, None);
        for f in out.chunks(4) {
            for (c, v) in chans.iter_mut().zip(f) {
                c.push(*v);
            }
        }
    }
    (e, chans)
}

fn play_a(e: &mut Engine) {
    e.play("A");
}

// CueOutputTests.theMasterMixIsOnTheFirstPairOnly
#[test]
fn the_master_mix_is_on_the_first_pair_only() {
    let (_e, c) = four_channels(None, play_a, 22_050); // no PFL anywhere
    let tail = 4000..22_050;
    assert!(rms(&c[0][tail.clone()]) > 0.01 && rms(&c[1][tail.clone()]) > 0.01);
    assert!(
        rms(&c[2][tail.clone()]) < 1e-5 && rms(&c[3][tail.clone()]) < 1e-5,
        "nothing cued, headphone mix = cue only: {} {}",
        rms(&c[2][tail.clone()]),
        rms(&c[3][tail.clone()])
    );
}

// CueOutputTests.theHeadphoneMixBlendsInTheMaster
#[test]
fn the_headphone_mix_blends_in_the_master() {
    let (e, c) = four_channels(Some(1.0), play_a, 22_050); // all master
    let tail = 4000..22_050;
    assert!(
        rms(&c[2][tail.clone()]) > 0.01,
        "the master reaches the headphones"
    );
    // same signal, scaled by the headphone level (Swift reads `dj.headphoneLevel`: its 0.7)
    let ratio = rms(&c[2][tail.clone()]) / rms(&c[0][tail]);
    assert_eq!(e.headphone_level, 0.7);
    assert!((ratio - 0.7).abs() < 0.02, "ratio {ratio}");
}

// CueControlTests.apcFaderFiveLeavesTheHeadphoneMix (`#expect(dj.headphoneMix == 0)` at the start;
// the Rust APC profile does bind fader 5 to the mix, PORTING.md's divergence row, so the other
// half does not port), and the level's start (`DJEngine._headphoneLevel`)
#[test]
fn a_new_engine_starts_the_headphones_cue_only_at_point_seven() {
    let (e, _r) = engine(config(SR));
    assert_eq!(e.headphone_mix, 0.0);
    assert_eq!(e.headphone_level, 0.7);
    // the mixer document and the engine agree on every starting position (the document's own
    // defaults are "the engine's starting positions"): the cue pair, the loop fader and the rest
    assert_eq!(e.mixer_state(), MixerState::new(&["A", "B"]));
    let m = MixerState::from_json(&djrust::json::JsonValue::parse("{}").unwrap()).unwrap();
    assert_eq!((m.headphone_mix, m.headphone_level), (0.0, 0.7));
}

// MARK: host_engine

#[test]
fn a_live_host_composes_off_the_control_thread_and_a_replay_keeps_it_inline() {
    let (live, _r) = host_engine(config(SR), true, true);
    assert!(
        !live.deterministic,
        "Swift's app: composes finish on workers"
    );
    let (headless, _r) = host_engine(config(SR), false, true);
    assert!(headless.deterministic, "--screenshot, --device null, tests");
    let (plain, _r) = engine(config(SR));
    assert!(
        plain.deterministic,
        "the constructor's own default is unchanged"
    );
}

#[test]
fn a_host_turns_every_decks_auto_gain_on_before_anything_loads() {
    let (e, _r) = host_engine(config(SR), true, true);
    assert!(e.auto_gain("A") && e.auto_gain("B"));
    let (e, _r) = host_engine(config(SR), false, false);
    assert!(!e.auto_gain("A") && !e.auto_gain("B"), "--no-auto-gain");
    let (e, _r) = engine(config(SR));
    assert!(!e.auto_gain("A"), "DJCore's default is off");
}

struct Collect(Arc<Mutex<Vec<JournalEvent>>>);

impl JournalSink for Collect {
    fn record(&mut self, _frame: i64, event: JournalEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// `applyAutoGainPreference` goes through `perform(.param)`, so a journal and a session carry
/// it, deck by deck in name order; a deck that has it already is not poked again.
#[test]
fn the_preference_is_a_recorded_param_for_each_deck_that_differs() {
    let (mut e, _r) = engine(config(SR));
    let log = Arc::new(Mutex::new(Vec::new()));
    e.journal = Some(Box::new(Collect(log.clone())));
    e.apply_auto_gain_preference(true);
    let param = |deck: &str| {
        JournalEvent::Action(DeckAction::Param {
            deck: deck.into(),
            name: "autoGain".into(),
            value: 1.0,
        })
    };
    assert_eq!(*log.lock().unwrap(), vec![param("A"), param("B")]);
    e.apply_auto_gain_preference(true);
    assert_eq!(log.lock().unwrap().len(), 2, "already on: nothing to do");
    e.set_auto_gain("B", false);
    e.apply_auto_gain_preference(true);
    assert_eq!(log.lock().unwrap().len(), 3);
    assert_eq!(log.lock().unwrap()[2], param("B"));
}

fn tones(amplitude: f32, seconds: f64, sr: f64) -> SampleBuffer {
    let n = (seconds * sr) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let s: f64 = [100.0, 1000.0, 5000.0]
                .iter()
                .map(|f| (2.0 * std::f64::consts::PI * f * i as f64 / sr).sin())
                .sum();
            amplitude * s as f32
        })
        .collect();
    SampleBuffer::new(l.clone(), Some(l), sr)
}

// LevelMatchTests.aSampleLoadsAtTheReference, on an engine built the way the hosts build it: no
// explicit `set_auto_gain`, the file loads levelled
#[test]
fn a_file_loaded_on_a_hosts_deck_plays_at_the_reference() {
    let sr = 48_000.0;
    let (mut e, mut r) = host_engine(config(sr), false, true);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.set_master(1.0);
    let quiet = tones(0.01, 4.0, sr); // about -30 LUFS, peak -30 dBFS: room to lift it
    assert!(e.load_sample("A", PreparedSample::new(quiet, "quiet")));
    let level = e.sample_level("A").expect("a sample was loaded");
    assert_eq!(e.auto_gain_db("A"), level.auto_gain_db());
    assert!(e.auto_gain_db("A") > 0.0, "levelled, not as recorded");
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let (mut ol, mut or) = (Vec::new(), Vec::new());
    for _ in 0..(3.0 * sr / 512.0).ceil() as usize {
        r.render_block(&mut l, &mut rr);
        ol.extend_from_slice(&l);
        or.extend_from_slice(&rr);
    }
    let measured = IntegratedLoudness::integrated(&ol[24_000..], Some(&or[24_000..]), sr).unwrap();
    let expected = LevelMatch::REFERENCE_LUFS.min(level.lufs.unwrap() + LevelMatch::MAX_GAIN_DB);
    assert!((measured - expected).abs() < 0.5, "{measured} LUFS");
}

// MARK: a live host's controllers (the wheel, regenerate) do not wait for a compose

struct Booth {
    rig: Rig,
    r: Renderer,
    now: f64,
}

impl Booth {
    fn new(live: bool) -> Booth {
        let mut c = EngineConfig::new(48_000.0);
        c.extended = false;
        let (mut e, r) = host_engine(c, live, true);
        e.load_demo("B");
        assert!(e.generate("A", &TrackSpec::new("techno", 20.0, Some("standard"))));
        e.play("A");
        let mut rig = Rig::new(e);
        assert!(rig.attach(INPULSE) && rig.attach(APC));
        let mut b = Booth { rig, r, now: 0.0 };
        b.render(2);
        b
    }

    fn render(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
        for _ in 0..blocks {
            self.r.render_block(&mut l, &mut rr);
        }
    }

    fn send(&mut self, port: &str, bytes: [u8; 3]) {
        self.now += 0.01;
        self.rig.route(port, &bytes, self.now).unwrap();
    }

    fn seed(&self) -> f64 {
        self.rig
            .engine
            .deck("A")
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .seed
    }

    fn target(&self) -> Option<f64> {
        self.rig
            .engine
            .deck("A")
            .unwrap()
            .track
            .seed_target
            .as_ref()
            .map(|s| s.seed)
    }

    /// `Rig::tick` (the hosts' loop) until `done`, a compose on a worker at a time.
    fn tick_until(&mut self, what: &str, done: impl Fn(&Booth) -> bool) {
        for _ in 0..3_000 {
            self.now += 0.01;
            self.rig.tick(self.now);
            self.render(1);
            if done(self) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("{what}: never landed");
    }
}

const CW: [u8; 3] = [0xB0, 0x01, 0x01];

// BrowserSeedTests.aFastTurnCountsEveryClickWhileItComposes, through the router of a host
// engine: the click returns with the compose on a worker (`seedTarget` set, the spec as it was),
// `Rig::tick` lands it, and five clicks in one frame end five seeds on
#[test]
fn a_live_hosts_wheel_click_returns_at_once_and_the_tick_lands_it() {
    let mut b = Booth::new(true);
    assert_eq!(b.seed(), 20.0);
    for _ in 0..5 {
        b.send(INPULSE, CW);
    }
    assert_eq!(
        b.seed(),
        20.0,
        "the compose is not done: the route came back"
    );
    assert_eq!(b.target(), Some(25.0), "the target counts every click");
    b.tick_until("five clicks", |b| b.seed() == 25.0);
    assert!(b.target().is_none());
}

/// The differential: the same clicks on a host that keeps composes inline have landed when
/// `route` returns, which is what blocked the MIDI and UI thread.
#[test]
fn a_headless_hosts_wheel_click_composes_inline() {
    let mut b = Booth::new(false);
    b.send(INPULSE, CW);
    assert_eq!(b.seed(), 21.0);
    assert!(b.target().is_none());
}

/// The APC's scene button 2 (`regenerate`): the deck's next version composes on a worker and
/// the tick lands it.
#[test]
fn a_live_hosts_regenerate_is_a_background_compose() {
    let mut b = Booth::new(true);
    let before = b.rig.engine.deck("A").unwrap().spec.clone().unwrap();
    let want = before.next_version();
    // the scene buttons act on the aimed deck, A here
    b.rig
        .router
        .press(APC, "scene", 1, false, 0.5, &mut b.rig.engine);
    assert_eq!(
        b.rig.engine.deck("A").unwrap().spec.as_ref(),
        Some(&before),
        "not loaded yet"
    );
    assert!(
        b.rig.engine.deck("A").unwrap().track.is_composing(),
        "composing"
    );
    b.tick_until("regenerate", |b| {
        b.rig.engine.deck("A").unwrap().spec.as_ref() == Some(&want)
    });
}

/// Wander's look-ahead runs in a host engine: a pace-4 swap is composed a bar before its move
/// (`Engine::deterministic` made `look_ahead_wander` a no-op in both live hosts).
#[test]
fn a_live_hosts_wander_composes_its_swap_a_bar_ahead() {
    let mut b = Booth::new(true);
    b.rig.engine.set_wander("A", true);
    b.rig.engine.set_wander_pace("A", 4);
    let mut asked = false;
    for step in 0..20_000 {
        b.now += 0.01;
        b.rig.tick(b.now);
        b.render(1);
        if b.rig.engine.look_ahead_key("A").is_some() {
            asked = true;
            break;
        }
        if step % 50 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    assert!(asked, "a swap was composed ahead of its move");
}

/// The same walk on a headless host never asks.
#[test]
fn a_headless_hosts_wander_makes_no_look_ahead() {
    let mut b = Booth::new(false);
    b.rig.engine.set_wander("A", true);
    b.rig.engine.set_wander_pace("A", 4);
    for _ in 0..6_000 {
        b.now += 0.01;
        b.rig.tick(b.now);
        b.render(1);
        assert!(b.rig.engine.look_ahead_key("A").is_none());
    }
}

// MARK: auto-mix and the next track on a host engine, stepped by the host's loop (`Rig::tick`)

/// Two decks as `deck_automix.rs` has them (A plays a composed net, B holds the demo loop) on a
/// host engine, stepped as a host steps it: the audio in 128-frame blocks, and `Rig::tick` about
/// every 16 ms (the window's frame; `djctl play` ticks every 10 ms) in place of the offline
/// conductor's `Engine::service`.
struct Set {
    rig: Rig,
    r: Renderer,
    frame: i64,
}

const SET_RATE: f64 = 22_050.0;
const BLOCK: usize = 128;
/// Blocks between two `Rig::tick`s: 3 x 128 frames at 22.05 kHz is 17 ms.
const TICK_EVERY: i64 = 3;

impl Set {
    fn new(live: bool, a: &TrackSpec) -> Set {
        let mut c = EngineConfig::new(SET_RATE);
        c.extended = true;
        let (mut e, r) = host_engine(c, live, true);
        let track = djrust::host::deck::PreparedTrack::compose(a, &e.preparer("A").unwrap(), 0);
        assert!(e.load_net("A", track));
        assert!(e.load_demo("B"));
        let mut s = Set {
            rig: Rig::new(e),
            r,
            frame: 0,
        };
        for d in ["A", "B"] {
            s.r.deck_mut(d).unwrap().runner.record_events = true;
        }
        s
    }

    fn step(&mut self) {
        if (self.frame / BLOCK as i64) % TICK_EVERY == 0 {
            self.rig.tick(self.frame as f64 / SET_RATE);
        }
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.r.render_block(&mut l, &mut rr);
        self.frame += BLOCK as i64;
    }

    fn run_blocks(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    fn run_until(&mut self, mut done: impl FnMut(&Set) -> bool) -> bool {
        let mut n = 0;
        while !done(self) && n < 40_000 {
            self.step();
            n += 1;
        }
        done(self)
    }

    fn perform(&mut self, a: DeckAction) {
        let applied = self
            .rig
            .engine
            .perform(&a, djrust::host::deck::PerformCtx::default());
        assert!(applied.ran(), "{a:?}: {applied:?}");
    }

    /// `deck_automix.rs`'s `started`: auto-mix on with `bars`, play A, Mix now.
    fn started(&mut self, bars: i64) {
        self.perform(DeckAction::AutoMix {
            on: true,
            bars,
            every: 0,
        });
        self.perform(DeckAction::Play {
            deck: "A".into(),
            on: true,
        });
        self.run_blocks(4);
        self.perform(DeckAction::AutoMixNow);
        self.run_blocks(4);
    }

    /// The frame `deck` played step `step` of the project it was running then.
    fn frame_of(&self, deck: &str, step: i64) -> Option<i64> {
        use djrust::wave::TickAnchorFlags;
        self.r
            .deck(deck)?
            .runner
            .anchors
            .iter()
            .rev()
            .find(|a| a.tick as i64 == step + 1 && !a.flags.contains(TickAnchorFlags::SWAP))
            .map(|a| a.frame)
    }
}

// deck_automix `a_live_hand_off_lands_on_its_bar_line`, on a host engine and stepped through
// `Rig::tick`: the incoming composes on a worker, the mix-out is prepared a phrase ahead, and the
// hand-off still lands on A's bar 8 with the mix-out that was prepared ahead
#[test]
fn a_live_hosts_auto_mix_hand_off_lands_on_its_bar_line() {
    use djrust::wave::rests::MixOutTail;
    let mut s = Set::new(true, &TrackSpec::new("dnb", 42.0, None));
    assert!(!s.rig.engine.deterministic);
    s.started(4);
    assert!(s.run_until(|s| s.rig.engine.auto_mix.run.is_some()));
    s.rig.engine.wait_auto_mix();
    assert!(s.rig.engine.auto_mix.run.as_ref().unwrap().incoming_ready);
    assert!(
        s.rig.engine.auto_mix_host.ahead.is_requested(),
        "the mix-out is prepared a phrase ahead, on a worker"
    );
    while !s.rig.engine.auto_mix_host.ahead.is_finished() {
        std::thread::yield_now();
    }
    assert!(s.run_until(|s| s.rig.engine.auto_mix.log.len() == 1));
    s.run_blocks(200);
    assert!(s.rig.engine.auto_mix_host.recorded_defers.is_empty());
    let b_start = s.frame_of("B", 0).unwrap();
    let a_h = s.frame_of("A", 8 * 16).unwrap();
    assert!(
        (b_start - a_h).abs() <= 1,
        "B starts on A's bar 8: {b_start} vs {a_h}"
    );
    assert_eq!(
        s.rig
            .engine
            .deck("A")
            .unwrap()
            .track
            .latest
            .as_ref()
            .unwrap()
            .tail,
        Some(MixOutTail::new(8 * 16, 4)),
        "the mix-out prepared ahead is the one that plays"
    );
    let e = &s.rig.engine;
    assert!(e.deck("B").unwrap().playing && !e.deck("A").unwrap().playing);
}

/// The differential: the same set on a host that keeps composes inline never prepares the
/// mix-out ahead, so what the test above shows is the live path.
#[test]
fn a_headless_hosts_auto_mix_prepares_nothing_ahead() {
    let mut s = Set::new(false, &TrackSpec::new("dnb", 42.0, None));
    assert!(s.rig.engine.deterministic);
    s.started(4);
    assert!(s.run_until(|s| s.rig.engine.auto_mix.run.is_some()));
    assert!(!s.rig.engine.auto_mix_host.ahead.is_requested());
    assert!(s.run_until(|s| s.rig.engine.auto_mix.log.len() == 1));
    assert!(!s.rig.engine.auto_mix_host.ahead.is_requested());
}

// DeckNetTests.aSongThatEndsHandsOverToThePrecomposedNext, on a host engine: auto-next composes
// the next version on a worker when the deck plays (`next_composing`, not ready), `Rig::tick`
// lands it, and the song that ends hands over to it
#[test]
fn a_live_hosts_auto_next_composes_on_a_worker_and_the_tick_hands_over() {
    let first = TrackSpec::new("ambient", 3.0, Some("minimal"));
    let want = first.next_version();
    let mut s = Set::new(true, &first);
    {
        let e = &mut s.rig.engine;
        e.deck_mut("A").unwrap().auto_advance = true;
        e.set_tempo("A", 3000.0); // ~5 ms per tick: the 320-tick song ends in ~1.6 s
        e.play("A");
    }
    s.rig.tick(0.0);
    let e = &s.rig.engine;
    assert!(e.next_ready("A").is_none(), "the compose is on a worker");
    assert_eq!(e.next_composing("A"), Some(&want));
    // ticks (no audio: the song does not end yet) land it
    for _ in 0..3_000 {
        if s.rig.engine.next_ready("A").is_some() {
            break;
        }
        s.frame += 1;
        s.rig.tick(s.frame as f64 / SET_RATE);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        s.rig.engine.next_ready("A"),
        Some(&want),
        "Rig::tick landed it"
    );
    assert!(
        s.run_until(|s| s.rig.engine.deck("A").unwrap().spec.as_ref() == Some(&want)),
        "the song ended and the tick started the next track"
    );
    assert!(s.rig.engine.deck("A").unwrap().playing);
}

/// The differential: a host that keeps composes inline has the next version ready at once.
#[test]
fn a_headless_hosts_auto_next_is_ready_at_once() {
    let first = TrackSpec::new("ambient", 3.0, Some("minimal"));
    let mut s = Set::new(false, &first);
    s.rig.engine.deck_mut("A").unwrap().auto_advance = true;
    s.rig.engine.play("A");
    s.rig.tick(0.0);
    assert_eq!(s.rig.engine.next_ready("A"), Some(&first.next_version()));
}

// MARK: hot-plug (ControllerHub.rescan)

/// A scan that lost the APC and found it again: the Rig forgets it and paints it afresh (an APC
/// comes up dark); one that only found a port the profiles do not own attaches nothing.
#[test]
fn a_replugged_controller_is_attached_and_painted_again_by_the_scan() {
    let (e, _r) = engine(config(48_000.0));
    let mut rig = Rig::new(e);
    let first = rig.hotplug(&[], &[APC.to_string(), "Some Synth".to_string()]);
    assert_eq!(first, vec![APC.to_string()]);
    let painted = |rig: &mut Rig, now: f64| {
        rig.led_frames(now)
            .iter()
            .map(|(_, f)| f.len())
            .sum::<usize>()
    };
    let n = painted(&mut rig, 0.0);
    assert!(n >= 80, "every pad and button: {n}");
    assert_eq!(painted(&mut rig, 0.01), 0, "nothing changed");
    // unplugged and plugged back in between two scans: gone in one, added in the next
    assert!(rig.hotplug(&[APC.to_string()], &[]).is_empty());
    assert_eq!(rig.hotplug(&[], &[APC.to_string()]), vec![APC.to_string()]);
    assert_eq!(
        painted(&mut rig, 0.02),
        n,
        "it came up dark: every LED again"
    );
    // a scan that reports it gone and found in one go (the host's one call) paints it too
    assert_eq!(painted(&mut rig, 0.03), 0);
    assert_eq!(
        rig.hotplug(&[APC.to_string()], &[APC.to_string()]),
        vec![APC.to_string()]
    );
    assert_eq!(painted(&mut rig, 0.04), n);
    // and without the detach (what djctl play did: attach alone) nothing is repainted
    assert_eq!(painted(&mut rig, 0.05), 0);
    assert_eq!(rig.hotplug(&[], &[APC.to_string()]), vec![APC.to_string()]);
    assert_eq!(
        painted(&mut rig, 0.06),
        0,
        "the old controller state was kept"
    );
}

// MARK: a library with only the Minecraft bank

#[cfg(feature = "sf2")]
mod minecraft_only {
    use super::*;
    use djrust::host::sampler::Library;

    fn folder(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("djhost-live-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn install_minecraft_bank(d: &std::path::Path) {
        std::fs::write(d.join("minecraft-local.sf2"), b"RIFF").unwrap();
        let side = djrust::sound::minecraft::Sidecar::new(
            "32",
            Default::default(),
            vec!["harp2".into()],
            2,
        );
        std::fs::write(d.join("minecraft-local.json"), side.file_text()).unwrap();
    }

    /// `MinecraftBank.isInstalled` reads the folder, so a folder with only that bank makes the
    /// Minecraft palette playable: the engine is handed the library and records
    /// `minecraft@<asset index>`. With no library it has no installed sounds, and the palette
    /// does not resolve.
    #[test]
    fn a_folder_with_only_the_minecraft_bank_is_a_library_the_engine_uses() {
        let d = folder("mc-only");
        install_minecraft_bank(&d);
        let lib = Library::scan(&d);
        assert!(lib.sound_fonts().is_empty(), "no general SoundFont");
        assert!(lib.has_banks(), "but a bank");
        assert!(lib.installed.minecraft_installed());

        let mut c = config(48_000.0);
        c.library = Some(Arc::new(lib));
        let (mut e, _r) = engine(c);
        assert!(e.generate("A", &TrackSpec::new("house", 3.0, Some("minimal"))));
        assert!(e.with_sounds(|s| s.installed.minecraft_installed()));
        e.set_palette("A", Some("minecraft"));
        assert_eq!(
            e.deck("A").unwrap().spec.as_ref().and_then(|s| s.palette()),
            Some("minecraft@32+b2".to_string())
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The gate both hosts share: the library goes to the engine whenever a bank is there, and
    /// the counts the hosts print say how many are general SoundFonts and how many local-only.
    #[test]
    fn for_host_hands_over_the_library_whenever_a_bank_is_there() {
        let d = folder("for-host");
        let none = Library::scan(&d).for_host();
        assert!(none.library.is_none() && none.sound_fonts == 0 && none.local_only == 0);
        install_minecraft_bank(&d);
        let mc = Library::scan(&d).for_host();
        assert!(
            mc.library.is_some(),
            "the Minecraft bank alone is a library"
        );
        assert_eq!((mc.sound_fonts, mc.local_only), (0, 1));
        std::fs::write(d.join("MuseScore_General.sf2"), b"RIFF").unwrap();
        let both = Library::scan(&d).for_host();
        assert!(both.library.is_some());
        assert_eq!((both.sound_fonts, both.local_only), (1, 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_folder_and_a_folder_of_other_files_have_no_banks() {
        let d = folder("none");
        assert!(!Library::scan(&d).has_banks());
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        assert!(!Library::scan(&d).has_banks());
        std::fs::write(d.join("MuseScore_General.SF2"), b"RIFF").unwrap();
        assert!(Library::scan(&d).has_banks(), "any case");
        let _ = std::fs::remove_dir_all(&d);
    }
}
