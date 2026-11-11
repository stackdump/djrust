//! `deck::offload`: the composes an engine starts on its own, offloaded to another instance
//! (`answer_offload`, as a Worker runs it) and taken back in steps, land as the same composes on
//! worker threads do: the same samples, bit for bit. (`cargo test --features transfer
//! --test offload`)
#![cfg(feature = "transfer")]

use djrust::deck::slots::{LookAheadKey, SwapStart};
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::TrackSpec;
use djrust::host::deck::engine::{Engine, EngineConfig, Renderer};
use djrust::host::deck::host::host_engine;
use djrust::host::deck::perform::PerformCtx;
use djrust::host::deck::transfer;

/// A live engine: composes on worker threads, or (`offload`) handed to the host.
fn boot(offload: bool) -> (Engine, Renderer) {
    let mut cfg = EngineConfig::new(48_000.0);
    cfg.history_beats = None;
    let (mut e, r) = host_engine(cfg, true, true);
    e.set_crossfader(0.5);
    if offload {
        e.set_offload(true);
    }
    (e, r)
}

/// The host and its composer: every request answered by another instance and taken in 64 KB
/// at a time.
fn pump(e: &mut Engine) -> (usize, usize) {
    let (mut asked, mut steps) = (0, 0);
    while let Some((id, bytes)) = e.take_offload() {
        asked += 1;
        let answer = transfer::answer_offload(&bytes).map_err(|x| x.to_string());
        e.offload_answer(id, answer).unwrap();
    }
    while e.offload_busy() {
        steps += 1;
        if let Some(l) = e.offload_step(64 << 10) {
            assert_eq!(l.error, None, "{l:?}");
        }
    }
    (asked, steps)
}

/// Before a service: every compose asked so far is in, as if its worker took no time. Threads:
/// waited for; offloaded: carried to the composer and back. Either way it lands on this service.
fn settle(e: &mut Engine) -> usize {
    if e.offloading() {
        let asked = pump(e).0;
        assert_eq!(e.offload_pending(), 0);
        return asked;
    }
    while e.copy_composing() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for d in ["A", "B"] {
        e.wait_composes(d);
    }
    0
}

/// Land what an action started (and what landing it starts in turn).
fn land(e: &mut Engine) -> usize {
    let mut asked = 0;
    for _ in 0..4 {
        asked += settle(e);
        e.service();
        e.service_copies();
    }
    asked
}

fn render(e: &mut Engine, r: &mut Renderer, blocks: usize, out: &mut Vec<u32>) -> usize {
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    let mut asked = 0;
    for n in 0..blocks {
        r.render_block(&mut l, &mut rr);
        out.extend(l.iter().chain(rr.iter()).map(|x| x.to_bits()));
        if n % 4 == 3 {
            asked += settle(e);
            e.service();
            e.service_loops();
            // (a host's frame, as djapp's: the engine's `service` leaves copies to it)
            e.service_copies();
        }
    }
    asked
}

/// The spec on a deck, as text.
fn on(e: &Engine, deck: &str) -> String {
    let s = e.deck(deck).unwrap().spec.clone().expect("a composed track");
    format!("{}:{}:{}", s.genre, s.seed, s.structure.unwrap_or_default())
}

fn spec(s: &str) -> TrackSpec {
    TrackSpec::parse(s).unwrap()
}

/// One session, the same on either engine: returns its samples and how many composes went out.
fn session(offload: bool) -> (Vec<u32>, usize) {
    let (mut e, mut r) = boot(offload);
    let mut out = Vec::new();
    let mut asked = 0;
    // (a host's own load: inline on both, it is not one of the engine's)
    for (d, s) in [("A", "techno:42:standard"), ("B", "house:7:loop")] {
        e.perform(&DeckAction::Load { deck: d.into(), source: DeckSource::Net(spec(s)) }, PerformCtx::default());
        e.perform(&DeckAction::Play { deck: d.into(), on: true }, PerformCtx::default());
    }
    asked += render(&mut e, &mut r, 400, &mut out);

    // a relocation (a seek, a section launch)
    assert!(e.relocate("A", 256, None, None));
    asked += land(&mut e);
    asked += render(&mut e, &mut r, 600, &mut out);

    // the next version, composed ahead and skipped to (auto-advance, the media controls' next)
    e.queue_next("A", &spec("techno:43:standard"));
    asked += land(&mut e);
    e.skip_to_next_track("A");
    asked += land(&mut e);
    assert_eq!(on(&e, "A"), "techno:43:standard");
    asked += render(&mut e, &mut r, 600, &mut out);

    // a swap at a tick the deck has passed by the time it lands: composed, then restarted
    let now = e.deck("B").unwrap().mirror.tick();
    assert!(now > 0);
    e.begin_generate("B", &spec("acid:3:drop"), SwapStart::Tick(now));
    asked += land(&mut e);
    assert_eq!(on(&e, "B"), "acid:3:drop");
    asked += render(&mut e, &mut r, 600, &mut out);

    // Wander's look-ahead, taken by its swap
    let key = LookAheadKey { step: 1, spec: spec("dnb:11:minimal"), start: SwapStart::Top, role_filter: None };
    assert!(e.request_look_ahead("A", key.clone()));
    asked += land(&mut e);
    let ahead = e.take_look_ahead("A", &key).expect("the look-ahead is held");
    e.swap_with_ahead("A", &key.spec, SwapStart::Top, ahead);
    asked += land(&mut e);
    assert_eq!(on(&e, "A"), "dnb:11:minimal");
    asked += render(&mut e, &mut r, 600, &mut out);

    // a deck copy (from a source, at the source's bar line)
    e.copy_deck("A", "B").unwrap();
    asked += land(&mut e);
    asked += render(&mut e, &mut r, 900, &mut out);
    assert!(e.pending_copies().is_empty(), "the copy never landed");
    assert_eq!(on(&e, "B"), "dnb:11:minimal");
    (out, asked)
}

#[test]
fn offloaded_composes_land_as_threads_do() {
    let (threads, none) = session(false);
    assert_eq!(none, 0);
    let (offloaded, asked) = session(true);
    // relocate, next, generate + its restart, look-ahead, copy (and the copy's relocation)
    assert!(asked >= 6, "only {asked} composes offloaded");
    let peak = threads.iter().map(|v| f32::from_bits(*v).abs()).fold(0f32, f32::max);
    assert!(peak > 0.01, "silent");
    assert_eq!(threads.len(), offloaded.len());
    let first = threads.iter().zip(&offloaded).position(|(p, q)| p != q);
    assert!(first.is_none(), "differs at sample {first:?} of {}", threads.len());
}

#[test]
fn a_dropped_compose_is_not_sent_and_off_fails_the_rest() {
    let (mut e, _r) = boot(true);
    e.perform(&DeckAction::Load { deck: "A".into(), source: DeckSource::Net(spec("techno:42:standard")) }, PerformCtx::default());
    // two asks for the next track: the first is superseded before the host takes it
    e.queue_next("A", &spec("techno:43:standard"));
    e.queue_next("A", &spec("techno:44:standard"));
    e.service();
    let mut taken = Vec::new();
    while let Some(r) = e.take_offload() {
        taken.push(r);
    }
    assert!(!taken.is_empty());
    // an answer of the wrong kind is refused, and that compose fails
    let (id, _) = taken[0].clone();
    assert!(e.offload_answer(id, Ok(vec![1, 2, 3])).is_err());
    assert!(e.offload_answer(id, Ok(Vec::new())).is_err(), "answered once already");
    // off: what is still out fails, and the engine composes as it did before
    e.set_offload(false);
    assert!(!e.offloading());
    assert_eq!(e.offload_pending(), 0);
    assert!(e.take_offload().is_none());
}

#[test]
fn an_answer_for_a_replaced_engine_matches_nothing_in_the_next() {
    // (a host swaps a restored engine in while the composer still works for the old one)
    let ask = |e: &mut Engine| {
        e.perform(&DeckAction::Load { deck: "A".into(), source: DeckSource::Net(spec("techno:42:loop")) }, PerformCtx::default());
        e.queue_next("A", &spec("techno:43:loop"));
        e.take_offload().expect("a request").0
    };
    let (mut old, _) = boot(true);
    let (mut new, _) = boot(true);
    let (a, b) = (ask(&mut old), ask(&mut new));
    assert_ne!(a, b);
    assert!(old.offload_waiting(a) && !new.offload_waiting(a));
}
