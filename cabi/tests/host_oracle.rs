//! The host Engine (djrust `host`, std) as a native differential oracle for the module's net
//! macros. Never shipped: this is the only place the host is linked (a dev-dependency).
//!
//! Parity config, as measured by the host-ABI probe: `EngineConfig.extended = false`, auto-gain
//! off, strip volume 1, crossfader to A, history off, the doc's nets in id order (the host loads
//! RawNets in id order; noise lanes are seeded by lane index). The strip fader ramps up from 0
//! over the first block, so the comparison starts at sample 128 (block 1).
//!
//! For each of the nine net macros x {default duration, 2} x {techno:42, dnb:3}: djwasm's
//! `djw_fire_macro` before quantum q against host `beats::fire_macro` before the same quantum.
//! Both reach `WaveRunner::fire_macro` at the start of block q (the host through its command
//! ring, drained at the top of `render_block`). L/R must be equal bit for bit from sample 128,
//! and the module's `djw_macro_mask` per quantum must equal the host deck's published macro
//! record (`DeckMirror::macro_record().snapshot.mask`).
//!
//! Two macros play beats' page, not the DJ app, on purpose (beats' `test-djwasm-030`
//! mechanism parity holds the module to the page for them):
//! - `double-drop` ignores its duration for its steps, as the page's compound does: the drop
//!   is always the drop macro's 1 bar. So the module's `double-drop` at any duration is held to
//!   the host's at its default (1 bar).
//! - `tape-stop` sweeps in whole bpm, as the page's `setTempoTransient` does. The oracle turns
//!   that off (`oracle_tempo_sweep_rounding(false)`) for the comparison, so the rest of the
//!   port is still held to the host bit for bit; `page_semantics_differ_from_the_host_only_where_meant`
//!   checks both exceptions themselves.

mod common;

use common::*;
use djrust::host::beats;
use djrust::host::deck::engine::{Engine, EngineConfig, Renderer};
use djrust::host::deck::host::host_engine;
use djrust::host::deck::perform::PerformCtx;
use djrust::session::action::DeckAction;
use djrust::wave::NetMacro;
use djwasm::*;

const SR: f64 = 48000.0;
const QUANTA: usize = 1500; // 4 s
const FIRE_AT: usize = 300;

struct Host {
    e: Engine,
    r: Renderer,
    quanta: usize,
}

impl Host {
    fn new(doc: &str) -> Host {
        let mut cfg = EngineConfig::new(SR);
        cfg.history_beats = None;
        cfg.extended = false;
        let (mut e, r) = host_engine(cfg, false, false);
        e.set_crossfader(0.0);
        let src = beats::source_of(doc).unwrap();
        beats::load(&mut e, "A", src);
        assert!(
            e.deck("A").is_some_and(|d| d.project.is_some()),
            "host loaded nothing"
        );
        beats::perform(&mut e, "A", r#"{"op":"param","name":"volume","value":1}"#).unwrap();
        e.perform(
            &DeckAction::Play {
                deck: "A".into(),
                on: true,
            },
            PerformCtx::default(),
        );
        Host { e, r, quanta: 0 }
    }

    fn block(&mut self, lr: &mut Vec<f32>) {
        let (mut l, mut r) = ([0f32; Q], [0f32; Q]);
        self.r.render_block(&mut l, &mut r);
        self.quanta += 1;
        if self.quanta % beats::TICK_EVERY == 0 {
            self.e.service();
            self.e.service_loops();
        }
        for i in 0..Q {
            lr.push(l[i]);
            lr.push(r[i]);
        }
    }

    fn mask(&self) -> u64 {
        self.e
            .deck("A")
            .unwrap()
            .mirror
            .macro_record()
            .snapshot
            .mask as u64
    }
}

fn host_render(doc: &str, fire: Option<(&str, i32)>) -> (Vec<f32>, Vec<u64>) {
    let mut h = Host::new(doc);
    let (mut lr, mut masks) = (Vec::new(), Vec::new());
    for q in 0..QUANTA {
        if q == FIRE_AT {
            if let Some((id, bars)) = fire {
                assert_eq!(beats::fire_macro(&mut h.e, "A", id, bars).unwrap(), "done");
            }
        }
        h.block(&mut lr);
        masks.push(h.mask());
    }
    (lr, masks)
}

fn module_render(doc: &str, fire_: Option<(&str, i32)>) -> (Vec<f32>, Vec<u64>) {
    module_render_with(doc, fire_, false)
}

fn module_render_with(doc: &str, fire_: Option<(&str, i32)>, page_tempo: bool) -> (Vec<f32>, Vec<u64>) {
    assert_eq!(djw_init(SR), 4096);
    oracle_tempo_sweep_rounding(page_tempo);
    assert!(call(djw_load, doc) > 0, "{}", out());
    djw_play();
    let mut masks = Vec::new();
    let mut lr = Vec::new();
    for q in 0..QUANTA {
        if q == FIRE_AT {
            if let Some((id, d)) = fire_ {
                assert_eq!(fire(id, d), 0, "{id}: {}", out());
            }
        }
        lr.extend(render_quanta(1, &mut |_| {}));
        masks.push(djw_macro_mask());
    }
    (lr, masks)
}

fn compare(label: &str, a: &[f32], b: &[f32]) -> Result<(), String> {
    assert_eq!(a.len(), b.len());
    let from = 2 * Q; // interleaved: sample 128 on
    match first_diff(&a[from..], &b[from..]) {
        None => Ok(()),
        Some((i, max)) => Err(format!(
            "{label}: differs from sample {} (frame {}), max |d| {max}",
            from + i,
            (from + i) / 2
        )),
    }
}

fn doc(spec: &str) -> String {
    id_sorted(&Source::Compose(spec.into()).doc())
}

#[test]
fn baseline_without_macros_is_bit_equal_from_sample_128() {
    let _g = lock();
    for spec in [
        r#"{"genre":"techno","seed":42}"#,
        r#"{"genre":"dnb","seed":3}"#,
    ] {
        let d = doc(spec);
        let (h, hm) = host_render(&d, None);
        let (m, mm) = module_render(&d, None);
        compare(spec, &h, &m).unwrap();
        assert!(hm.iter().all(|&x| x == 0) && mm.iter().all(|&x| x == 0));
        assert!(m.iter().any(|x| x.abs() > 0.05), "silent");
    }
}

#[test]
fn every_net_macro_matches_the_host() {
    let _g = lock();
    let mut bad = Vec::new();
    let mut cases = 0;
    for spec in [
        r#"{"genre":"techno","seed":42}"#,
        r#"{"genre":"dnb","seed":3}"#,
    ] {
        let d = doc(spec);
        let (plain, _) = module_render(&d, None);
        for m in NetMacro::ALL {
            for (module_d, host_bars) in [(-1, 0), (2, 2)] {
                // (the page's double-drop: the drop is 1 bar at any duration)
                let host_bars = if m == NetMacro::DoubleDrop { 0 } else { host_bars };
                let label = format!("{spec} {} d={module_d}", m.id());
                let (h, hm) = host_render(&d, Some((m.id(), host_bars)));
                let (x, xm) = module_render(&d, Some((m.id(), module_d)));
                cases += 1;
                if let Err(e) = compare(&label, &h, &x) {
                    bad.push(e);
                }
                if hm != xm {
                    let q = hm.iter().zip(&xm).position(|(a, b)| a != b).unwrap();
                    bad.push(format!(
                        "{label}: mask differs at quantum {q}: host {} module {}",
                        hm[q], xm[q]
                    ));
                }
                // the fire did something the oracle saw too (not a vacuous equality)
                assert!(
                    xm[FIRE_AT..].iter().any(|&b| b & m.bit() as u64 != 0)
                        || first_diff(&x, &plain).is_some(),
                    "{label}: the macro changed nothing"
                );
            }
        }
    }
    assert_eq!(cases, 36);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The module's default tape-stop (whole bpm) against the exact sweep: the same mask, the same
/// audio up to the fire, different audio during the sweep; and a double-drop at 2 bars renders
/// as at 1 bar with the same mask (its steps ignore the duration).
#[test]
fn page_semantics_differ_from_the_host_only_where_meant() {
    let _g = lock();
    let d = doc(r#"{"genre":"techno","seed":42}"#);
    let (exact, em) = module_render_with(&d, Some(("tape-stop", -1)), false);
    let (page, pm) = module_render_with(&d, Some(("tape-stop", -1)), true);
    assert_eq!(em, pm, "tape-stop: the mask does not depend on the rounding");
    let fire = 2 * Q * FIRE_AT;
    assert!(first_diff(&exact[..fire], &page[..fire]).is_none(), "tape-stop: differs before the fire");
    assert!(first_diff(&exact[fire..], &page[fire..]).is_some(), "tape-stop: the rounding changed nothing");
    let (two, tm) = module_render_with(&d, Some(("double-drop", 2)), true);
    let (one, om) = module_render_with(&d, Some(("double-drop", 1)), true);
    assert!(first_diff(&two, &one).is_none(), "double-drop: 2 bars renders unlike 1 bar");
    assert_eq!(tm, om, "double-drop: the mask depends on the duration");
}
