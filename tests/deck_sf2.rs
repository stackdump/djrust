//! A deck whose tonal lanes play SoundFont presets (feature `sf2`): the voice catalog's
//! fallback ranking picks the General MIDI program per instrument and part, the sampler loads
//! through rustysynth and the deck renders it inside its block, then through the master
//! chain. No SoundFont is installed here, so the bank is synthesised (a looped sine on every
//! GM program); a real bank runs when `$DJ_SOUNDFONTS` points at one.
#![cfg(feature = "sf2")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use djrust::fx::MasterMacro;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer};
use djrust::host::sampler::{synthetic_sf2, Library};

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

fn count<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
    ALLOCS.with(|c| c.set(0));
    FREES.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    let r = f();
    ON.with(|o| o.set(false));
    (r, ALLOCS.with(|c| c.get()), FREES.with(|c| c.get()))
}

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn bank_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("djhost-deck-sf2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    std::fs::write(d.join("MuseScore_General.sf2"), synthetic_sf2(&presets)).unwrap();
    d
}

fn render(r: &mut Renderer, blocks: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut ol, mut or) = (Vec::new(), Vec::new());
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        ol.extend_from_slice(&l);
        or.extend_from_slice(&rr);
    }
    (ol, or)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn with_bank(dir: &std::path::Path) -> (djrust::host::deck::Engine, Renderer) {
    let lib = Arc::new(Library::scan(dir));
    let mut c = EngineConfig::new(SR);
    c.library = Some(lib);
    c.extended = false;
    engine(c)
}

#[test]
fn tonal_lanes_play_soundfont_programs() {
    let dir = bank_dir("play");
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));

    let (mut e, mut r) = with_bank(&dir);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.generate("A", &spec));
    e.set_tempo("A", 240.0);
    e.play("A");
    let (sampled, _) = render(&mut r, 200);
    assert!(
        e.deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed)
            > 0,
        "a SoundFont program is loaded"
    );
    assert!(rms(&sampled) > 0.005, "{}", rms(&sampled));

    // the same song on the synth voices
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e2, mut r2) = engine(c);
    e2.set_crossfader(0.0);
    e2.set_volume("A", 1.0);
    e2.generate("A", &spec);
    e2.set_tempo("A", 240.0);
    e2.play("A");
    let (synth, _) = render(&mut r2, 200);
    assert_eq!(
        e2.deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed),
        0
    );
    assert!(sampled != synth, "the sampled lanes sound different");

    // deterministic
    let (mut e3, mut r3) = with_bank(&dir);
    e3.set_crossfader(0.0);
    e3.set_volume("A", 1.0);
    e3.generate("A", &spec);
    e3.set_tempo("A", 240.0);
    e3.play("A");
    let (again, _) = render(&mut r3, 200);
    assert!(sampled == again, "same bank, same spec, same bytes");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `sampledInstrumentsDoNotAllocate`: the samplers render inside the deck's callback, then the
/// master chain, with no allocation and no free once warm.
#[test]
fn sampled_decks_do_not_allocate() {
    let dir = bank_dir("rt");
    let (mut e, mut r) = with_bank(&dir);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.generate("A", &TrackSpec::new("country", 7.0, Some("minimal")));
    e.set_tempo("A", 480.0);
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for _ in 0..200 {
        r.render_block(&mut l, &mut rr);
    }
    assert!(
        e.deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed)
            > 0
    );
    let (_, a, f) = count(|| {
        for _ in 0..1000 {
            r.render_block(&mut l, &mut rr);
        }
    });
    eprintln!("sampled steady: {a} allocations, {f} frees in 1000 blocks");
    e.fire_master("A", MasterMacro::PhaserDrone, None);
    e.fire_master("A", MasterMacro::OctaveDown, None);
    let (_, a2, f2) = count(|| {
        for _ in 0..400 {
            r.render_block(&mut l, &mut rr);
        }
    });
    eprintln!("sampled + master macros: {a2} allocations, {f2} frees in 400 blocks");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((a, f, a2, f2), (0, 0, 0, 0));
}

/// A bank that cannot fit a category leaves the lane on its synth voice (no sound is lost).
#[test]
fn an_empty_bank_leaves_every_lane_on_its_synth_voice() {
    let dir = std::env::temp_dir().join(format!("djhost-deck-sf2-empty-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let (mut e, mut r) = with_bank(&dir);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.generate("A", &TrackSpec::new("country", 7.0, Some("minimal")));
    e.play("A");
    let (out, _) = render(&mut r, 100);
    assert_eq!(
        e.deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed),
        0
    );
    assert!(rms(&out) > 0.005);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `TrackSpec.finish` asks what is installed: an engine with a library resolves a palette's
/// General MIDI options against its SoundFont folder, one without plays the composed
/// instruments (nothing installed).
#[test]
fn a_palette_resolves_against_the_engines_library() {
    use djrust::sound::project_sound_net;
    let dir = bank_dir("palette");
    let lib = Library::scan(&dir);
    let spec =
        TrackSpec::new("house", 11.0, Some("standard")).with_palette(Some("jazz"), &lib.installed);
    let melody = |e: &djrust::host::deck::Engine| {
        let p = e.deck("A").unwrap().project.clone().unwrap();
        project_sound_net(&p, "melody")
            .unwrap()
            .track
            .sound
            .option
            .clone()
    };
    let (mut e, _r) = with_bank(&dir);
    assert!(e.generate("A", &spec));
    let picked = melody(&e);
    assert!(
        ["gm:66", "gm:65", "gm:59", "gm:26"].contains(&picked.as_deref().unwrap_or("")),
        "{picked:?}"
    );
    assert_eq!(
        e.project_of(&spec).canonical_json(),
        spec.project_in(lib.sounds()).canonical_json()
    );

    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut bare, _r2) = engine(c);
    assert!(bare.generate("A", &spec));
    assert_eq!(melody(&bare), None);
}

/// A track composed ahead and dropped unused (a cancelled next track, a superseded generate, a
/// look-ahead nobody took) bound its samplers to slots the bank has numbered since: the next track
/// that binds the same instruments finds the slots already there and would send nothing, so the
/// dropped track leaves its samplers in the bank, which sends them with the next project the deck
/// is handed (`Outbox`).
#[test]
fn a_track_dropped_unused_leaves_its_samplers_to_be_sent() {
    let dir = bank_dir("discard");
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    let (mut e, mut r) = with_bank(&dir);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.queue_next("A", &spec);
    assert!(e.next_ready("A").is_some());
    e.cancel_next("A");
    assert!(e.generate("A", &spec));
    e.set_tempo("A", 240.0);
    e.play("A");
    let (sampled, _) = render(&mut r, 200);
    assert!(
        e.deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed)
            > 0,
        "the samplers the dropped track bound were sent with the next one"
    );
    assert!(rms(&sampled) > 0.005, "{}", rms(&sampled));
}

/// How many samplers the audio thread holds for deck A, after `blocks` more blocks.
fn loaded(e: &djrust::host::deck::Engine, r: &mut Renderer, blocks: usize) -> u32 {
    render(r, blocks);
    e.deck("A")
        .unwrap()
        .mirror
        .samplers_loaded
        .load(Ordering::Relaxed)
}

/// A swap composed past its tick is prepared again from the next bar (`PreparedTrack.restarted`).
/// The first compose bound the samplers (they wait in the bank to be sent); the restart finds
/// their slots numbered already and binds nothing, so the track that loads must bring the first
/// one's: the same swap composed on time and one composed late leave the audio thread the same
/// samplers.
#[test]
fn a_late_swap_still_sends_the_samplers_its_first_compose_bound() {
    use djrust::deck::slots::SwapStart;
    let first = TrackSpec::new("country", 7.0, Some("minimal"));
    let second = TrackSpec::new("jazz", 3.0, Some("minimal"));
    let run = |tag: &str, start: SwapStart| {
        let dir = bank_dir(tag);
        let (mut e, mut r) = with_bank(&dir);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        e.set_tempo("A", 240.0);
        assert!(e.generate("A", &first));
        e.play("A");
        let one = loaded(&e, &mut r, 300);
        assert!(
            e.deck("A").unwrap().mirror.tick() > 1,
            "the clock is past tick 1"
        );
        assert!(e.generate_at("A", &second, start));
        let two = loaded(&e, &mut r, 600);
        let _ = std::fs::remove_dir_all(&dir);
        (one, two)
    };
    let (one, on_time) = run("late-top", SwapStart::Top);
    assert!(
        on_time > one,
        "the second song brings samplers of its own ({one} -> {on_time})"
    );
    // tick 1 has passed by the time the compose lands: prepared again from the next bar
    let (_, late) = run("late-tick", SwapStart::Tick(1));
    assert_eq!(
        late, on_time,
        "the restart found the first compose's slots numbered already"
    );
}

/// A compose nobody waits for any more (a next track cancelled, a look-ahead dropped, finished
/// or not) made a track that is dropped unused: its samplers stay in the bank, not with it
/// (`Engine::service` drops the track of a compose that was still running when it lands,
/// `wait_composes` waits for it).
#[test]
fn a_compose_let_go_of_leaves_its_samplers_to_be_sent() {
    use djrust::deck::slots::{LookAheadKey, SwapStart};
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    let key = LookAheadKey {
        step: 1,
        spec: spec.clone(),
        start: SwapStart::Tick(64),
        role_filter: None,
    };
    for (what, cancel, running) in [
        ("a next track cancelled after it composed", true, false),
        ("a look-ahead dropped after it composed", false, false),
        ("a next track cancelled while it composed", true, true),
        ("a look-ahead dropped while it composed", false, true),
    ] {
        let dir = bank_dir("orphan");
        let (mut e, mut r) = with_bank(&dir);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        e.deterministic = false;
        // it composes on a worker, binding the bank's slots; nobody polls it (no `service`) ...
        if cancel {
            e.queue_next("A", &spec);
        } else {
            assert!(e.request_look_ahead("A", key.clone()));
        }
        while !running && !e.deck("A").unwrap().track.composes_finished() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // ... and it is let go of
        if cancel {
            e.cancel_next("A");
        } else {
            e.drop_look_ahead("A");
        }
        if running {
            e.wait_composes("A");
        } else {
            e.service();
        }
        assert!(e.generate("A", &spec));
        e.set_tempo("A", 240.0);
        e.play("A");
        assert!(
            loaded(&e, &mut r, 200) > 0,
            "{what}: the samplers it bound were sent with the next track"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// What a host that composes with `PreparedTrack::from_source` and then decides against the load
/// does with the track: it drops it, or hands it back (`Engine::give_back_track`, which
/// `ComposeJobs` does for a superseded compose and `perform` for a load it turns down). Either way
/// the samplers its worker loaded are not lost with it: a prepare takes none, they wait in the
/// bank until the deck is handed a project (`SamplerBank::take_ready`), so the next track that
/// binds the slots the bank numbered already still sends them.
fn composed_unloaded(e: &djrust::host::deck::Engine, spec: &TrackSpec) -> djrust::host::deck::PreparedTrack {
    use djrust::host::deck::{PreparedTrack, TrackSource};
    let preparer = e.preparer("A").unwrap();
    PreparedTrack::from_source(&TrackSource::Net(spec.clone()), &preparer, 0).unwrap()
}

/// The next track `A` plays: does it bring samplers to the audio thread?
fn next_track_sends_samplers(
    e: &mut djrust::host::deck::Engine,
    r: &mut Renderer,
    spec: &TrackSpec,
) -> u32 {
    assert!(e.generate("A", spec));
    e.set_tempo("A", 240.0);
    e.play("A");
    loaded(e, r, 200)
}

#[test]
fn a_track_nobody_loads_leaves_its_samplers_to_be_sent_handed_back_or_not() {
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    for (what, hand_back, want_sent) in [
        ("dropped plain", false, true),
        ("handed back to the bank", true, true),
    ] {
        let dir = bank_dir("handback");
        let (mut e, mut r) = with_bank(&dir);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        let track = composed_unloaded(&e, &spec);
        if hand_back {
            e.give_back_track("A", track);
        } else {
            drop(track);
        }
        let sent = next_track_sends_samplers(&mut e, &mut r, &spec);
        assert_eq!(sent > 0, want_sent, "{what}: {sent} samplers sent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A load `perform` turns down (nothing to load; an action that is no load at all) was handed a
/// composed track: it goes back to the bank, not with the `PerformCtx` that drops.
#[test]
fn a_load_perform_turns_down_gives_the_composed_track_back() {
    use djrust::session::action::DeckAction;
    use djrust::session::model::DeckSource;
    use djrust::host::deck::{Applied, PerformCtx};
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    let load_nothing = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::None,
    };
    let play = DeckAction::Play {
        deck: "A".into(),
        on: false,
    };
    let demo = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Sample {
            id: djrust::session::model::DEMO_ID.into(),
            title: "demo loop".into(),
            grid: None,
        },
    };
    for (what, action, ignored) in [
        ("a load of nothing", load_nothing, true),
        ("an action that loads nothing", play, false),
        ("a load of audio", demo, false),
    ] {
        let dir = bank_dir("perform-back");
        let (mut e, mut r) = with_bank(&dir);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        let track = composed_unloaded(&e, &spec);
        let applied = e.perform(
            &action,
            PerformCtx {
                prepared: Some(track),
                ..PerformCtx::default()
            },
        );
        assert_eq!(
            matches!(applied, Applied::Ignored(_)),
            ignored,
            "{what}: {applied:?}"
        );
        let sent = next_track_sends_samplers(&mut e, &mut r, &spec);
        assert!(sent > 0, "{what}: the track's samplers were lost with it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A compose a newer ask superseded (the window's Compose, Random and Next on a deck that is
/// still composing) is handed back, and the newer track still plays every instrument: the
/// older one's samplers wait in the bank, so the newer one's hand-over sends them. The older ask
/// may be finished when the newer arrives or still running.
#[test]
fn a_superseded_compose_job_leaves_its_samplers_to_be_sent() {
    use djrust::host::deck::{ComposeJobs, Landed};
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    for (what, older_finished) in [
        ("the older compose had finished", true),
        ("the older compose was still running", false),
    ] {
        let dir = bank_dir("job-superseded");
        let (mut e, mut r) = with_bank(&dir);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        let mut jobs = ComposeJobs::new();
        assert!(jobs.ask(&e, "A", spec.clone()));
        if older_finished {
            while !jobs.is_ready("A") {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        // the same song again, as Random with the same seed would ask, and then a third: the
        // newest replaces the second
        assert!(jobs.ask(&e, "A", TrackSpec::new("jazz", 1.0, Some("minimal"))));
        assert!(jobs.ask(&e, "A", spec.clone()));
        let mut ended = Vec::new();
        for _ in 0..3_000 {
            ended.extend(jobs.poll(&mut e));
            if !jobs.is_composing("A") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            ended.iter().map(|l| &l.landed).collect::<Vec<_>>(),
            vec![
                &Landed::Superseded,
                &Landed::Loaded(djrust::host::deck::Applied::Done)
            ],
            "{what}: the first was given back and only the newest loaded"
        );
        e.set_tempo("A", 240.0);
        e.play("A");
        let (sampled, _) = render(&mut r, 200);
        let sent = e
            .deck("A")
            .unwrap()
            .mirror
            .samplers_loaded
            .load(Ordering::Relaxed);
        assert!(sent > 0, "{what}: samplers sent with the newest track");
        assert!(rms(&sampled) > 0.005, "{what}: {}", rms(&sampled));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
