//! `deck::transfer`: a track prepared and encoded by one engine, decoded and loaded by another,
//! plays the same samples as the same spec loaded inline. (`cargo test --features
//! transfer --test transfer`)
#![cfg(feature = "transfer")]

use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::TrackSpec;
use djrust::host::deck::engine::{Engine, EngineConfig, Renderer};
use djrust::host::deck::host::host_engine;
use djrust::host::deck::perform::PerformCtx;
use djrust::host::deck::track::PreparedTrack;
use djrust::host::deck::transfer::{self, TransferError};
use std::time::Instant;

/// Installs `bytes` 64 KB at a time, as a worklet would across render quanta.
fn stepped(bytes: &[u8], p: &djrust::host::deck::track::Preparer, spec: &TrackSpec) -> PreparedTrack {
    {
        let mut r = djrust::transfer::FrameReader::new(bytes.to_vec());
        let mut lens = Vec::new();
        while let Some(n) = r.peek_len() {
            lens.push(n);
            r.skip().unwrap();
        }
        let mut sorted = lens.clone();
        sorted.sort_unstable();
        eprintln!("  frames {} header {} KB, biggest {:?} KB", lens.len(), lens[0] / 1024, sorted.iter().rev().take(6).map(|n| n / 1024).collect::<Vec<_>>());
    }
    let t0 = Instant::now();
    let mut i = transfer::Install::new(bytes.to_vec(), p).unwrap();
    let (mut steps, mut worst) = (1, t0.elapsed().as_secs_f64());
    loop {
        let t = Instant::now();
        let done = i.step(64 << 10).unwrap();
        worst = worst.max(t.elapsed().as_secs_f64());
        steps += 1;
        if done {
            break;
        }
    }
    let t = Instant::now();
    let track = i.finish(p).unwrap();
    let fin = t.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "{}:{}:{:?} {} KB in {steps} steps, worst {:.2} ms, all {:.1} ms, finish {fin:.2} ms",
        spec.genre,
        spec.seed,
        spec.structure,
        bytes.len() / 1024,
        worst * 1e3,
        t0.elapsed().as_secs_f64() * 1e3
    );
    track
}

fn boot() -> (Engine, Renderer) {
    let mut cfg = EngineConfig::new(48_000.0);
    cfg.history_beats = None;
    let (mut e, r) = host_engine(cfg, false, true);
    e.set_crossfader(0.5);
    (e, r)
}

fn load(e: &mut Engine, deck: &str, spec: &TrackSpec, prepared: Option<PreparedTrack>) {
    e.perform(
        &DeckAction::Load { deck: deck.into(), source: DeckSource::Net(spec.clone()) },
        PerformCtx { prepared, ..PerformCtx::default() },
    );
    e.perform(&DeckAction::Play { deck: deck.into(), on: true }, PerformCtx::default());
}

fn render(e: &mut Engine, r: &mut Renderer, blocks: usize, out: &mut Vec<u32>) {
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    for n in 0..blocks {
        r.render_block(&mut l, &mut rr);
        out.extend(l.iter().chain(rr.iter()).map(|x| x.to_bits()));
        if n % 4 == 3 {
            e.service();
            e.service_loops();
        }
    }
}

/// Deck A plays `a`, then `b` lands on deck B while A plays on: inline vs transferred.
fn check(a: &str, b: &str) {
    let (a, b) = (TrackSpec::parse(a).unwrap(), TrackSpec::parse(b).unwrap());
    let (mut e1, mut r1) = boot();
    let mut x = Vec::new();
    load(&mut e1, "A", &a, None);
    render(&mut e1, &mut r1, 600, &mut x);
    load(&mut e1, "B", &b, None);
    render(&mut e1, &mut r1, 1500, &mut x);

    // the preparing instance (a Worker): its own engine, its decks' preparers
    let (w, _wr) = boot();
    let wire = |deck: &str, s: &TrackSpec| {
        let p = w.preparer(deck).unwrap();
        let t = PreparedTrack::compose(s, &p, 0);
        transfer::encode(t, &p).unwrap()
    };
    let (ba, bb) = (wire("A", &a), wire("B", &b));
    let (mut e2, mut r2) = boot();
    let mut y = Vec::new();
    let ta = stepped(&ba, &e2.preparer("A").unwrap(), &a);
    load(&mut e2, "A", &a, Some(ta));
    render(&mut e2, &mut r2, 600, &mut y);
    let tb = stepped(&bb, &e2.preparer("B").unwrap(), &b);
    let t = Instant::now();
    load(&mut e2, "B", &b, Some(tb));
    let lb = t.elapsed().as_secs_f64() * 1e3;
    render(&mut e2, &mut r2, 1500, &mut y);
    eprintln!("  load {lb:.1} ms");
    let peak = x.iter().map(|v| f32::from_bits(*v).abs()).fold(0f32, f32::max);
    assert!(peak > 0.01, "silent");
    assert_eq!(x.len(), y.len());
    let first = x.iter().zip(&y).position(|(p, q)| p != q);
    assert!(first.is_none(), "differs at sample {first:?}");
    // and the engine reads the same deck off it
    for d in ["A", "B"] {
        assert_eq!(e1.deck(d).unwrap().sections.len(), e2.deck(d).unwrap().sections.len());
        assert_eq!(e1.roles(d), e2.roles(d));
    }
}

#[test]
fn transferred_tracks_play_like_inline_ones() {
    check("techno:42:standard", "house:7:extended");
    check("acid:3:drop", "techno:42");
    check("ambient:5:build", "dnb:11:minimal");
}

#[test]
fn unfit_and_bad_bytes_are_refused() {
    let spec = TrackSpec::parse("techno:42:standard").unwrap();
    let (w, _) = boot();
    let p = w.preparer("A").unwrap();
    let bytes = transfer::encode(PreparedTrack::compose(&spec, &p, 0), &p).unwrap();
    let (mut e, _) = boot();
    e.set_role_disabled("A", "bass", true);
    assert!(matches!(transfer::decode(&bytes, &e.preparer("A").unwrap()), Err(TransferError::Unfit(_))));
    let (e44, _) = host_engine(EngineConfig::new(44_100.0), false, true);
    assert!(matches!(transfer::decode(&bytes, &e44.preparer("A").unwrap()), Err(TransferError::Unfit(_))));
    // a role switched off while the steps ran
    let (mut late, _) = boot();
    let i = transfer::Install::new(bytes.clone(), &late.preparer("A").unwrap()).unwrap();
    late.set_role_disabled("A", "bass", true);
    assert!(matches!(i.finish(&late.preparer("A").unwrap()), Err(TransferError::Unfit(_))));
    let (ok, _) = boot();
    assert!(matches!(transfer::decode(&bytes[..bytes.len() / 2], &ok.preparer("A").unwrap()), Err(TransferError::Bytes(_))));
}

#[test]
fn a_decoded_track_knows_its_own_numbers() {
    // the sender's generation and serial mean nothing here: what the deck reads of the
    // project (`latest`, which a relocation takes its serial from) is the receiver's
    let spec = TrackSpec::parse("techno:42:standard").unwrap();
    let (w, _) = boot();
    let p = w.preparer("A").unwrap();
    let sent = PreparedTrack::compose(&spec, &p, 0);
    let (gen, serial) = (sent.prepared.project.generation, sent.prepared.project.track_serial);
    let bytes = transfer::encode(sent, &p).unwrap();
    let (e, _) = boot();
    let t = transfer::decode(&bytes, &e.preparer("A").unwrap()).unwrap();
    let wave = &t.prepared.project;
    assert_ne!((wave.generation, wave.track_serial), (gen, serial));
    assert_eq!(t.prepared.latest.generation, wave.generation);
    assert_eq!(t.prepared.latest.track_serial, wave.track_serial);
}
