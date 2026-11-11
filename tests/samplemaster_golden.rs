//! Swift goldens for a sample deck's master path (`tools/paritydump/samplemaster.swift`, section
//! "samplemaster", `crates/djengine/tests/fixtures/swift/samplemaster.json`): `DeckRenderer`'s
//! sample half on a Swift `DJEngine(offline:)` against this engine's deck.
//!
//! Each case is a looping sample on deck A (its grid at 120 bpm, 48 kHz) and a script of
//! main-thread pokes run before the block they name: master macros (which land on the next 4-beat
//! line of the deck's grid), the panic, a master slider, a hand on the platter, the FX unit and a
//! clip's FX. The samples are integer arithmetic only, so both sides play the same floats. What is
//! compared:
//!
//! - **exact**: the frame and tempo each fire landed on (`sampleMasterFire`), the master macros
//!   running per block, whether the clip's FX chain is in the path;
//! - **corners**: the bus's filter corners within an ulp of a libm (`exp` / `pow`);
//! - **audio**: the sha256 of every float rendered after the first block (the first is the
//!   strip's fader ramping up from silence in this engine; Swift's is read before the strip).
//!   Where it is not the same bits (Apple's libm and the `libm` crate differ in the last bit of
//!   `sin` / `exp` / `pow` / `tanh`) the peaks, sums and raw blocks are compared by
//!   [`TOLERANCE`], relative to the case's own peak.
//!
//! The file is written on a Mac and committed; the test panics, naming the `regen.sh` command,
//! when it is missing (`common_fixture`).

mod common_fixture;

use std::sync::Arc;

use djrust::cid::sha256;
use djrust::deck::beat_grid::BeatGrid;
use djrust::deck::turntable::{SampleBuffer, TurntableCommand};
use djrust::fx::unit::FxParam;
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::json::JsonValue;
use djrust::session::types::{LoopFx, LoopFxMode};
use djrust::TrackSpec;
use djrust::host::deck::sample::PreparedSample;
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 48_000.0;
const BAR120: usize = 96_000;

/// How far a render may be from Swift's, relative to the case's own peak, where it is not the
/// same bits: the deck golden's bound (an ulp of a libm carried through the chain).
const TOLERANCE: f64 = 5e-5;
/// `sample-octave-up`: the pitch shifter's wavetables come from an FFT (vDSP in Swift, an in-crate
/// radix-2 in f64 here: PORTING.md, "vDSP bit-exactness"), which the loop slices measured at 1e-5
/// on a sine; this one is a whole saw pair through a bar of octave, measured 9.0e-5 of the peak,
/// held to 2.2x that.
const OCTAVE_TOLERANCE: f64 = 2e-4;
/// The bus's filter corners, relative: one ulp of an `exp` / `pow` is 1.1e-16.
const CORNER_TOLERANCE: f64 = 1e-12;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("samplemaster")
}

fn arr<'a>(v: &'a JsonValue, k: &str) -> &'a [JsonValue] {
    v.get(k)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {k}"))
}

fn s<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k).and_then(JsonValue::as_str).unwrap()
}

fn bits(d: f64) -> String {
    format!("{:016x}", d.to_bits())
}

fn from_bits(h: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(h, 16).unwrap())
}

/// `smSaw`: `n` frames of a saw of `period` frames at `amp`, in f32.
fn saw(period: usize, amp: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| ((i % period) as f32 / period as f32 * 2.0 - 1.0) * amp)
        .collect()
}

/// `smBuffer`.
fn buffer(kind: &str) -> Arc<SampleBuffer> {
    let n = 2 * SR as usize;
    let x: Vec<f32> = match kind {
        "clicks" => {
            let mut x = vec![0f32; BAR120];
            for k in (0..BAR120).step_by(6000) {
                x[k] = 1.0;
            }
            x
        }
        "saw" => saw(128, 0.5, n),
        _ => saw(128, 0.2, n)
            .iter()
            .zip(saw(8, 0.2, n))
            .map(|(a, b)| a + b)
            .collect(),
    };
    Arc::new(SampleBuffer::new(x.clone(), Some(x), SR))
}

fn clip_fx() -> LoopFx {
    LoopFx {
        mode: LoopFxMode::Soft,
        drive: 18.0,
        amount: 0.9,
        ..LoopFx::default()
    }
}

fn expand(runs: &JsonValue, blocks: usize) -> Vec<JsonValue> {
    let rows = runs.as_array().unwrap();
    let mut out = Vec::with_capacity(blocks);
    let mut cur = rows[0].as_array().unwrap()[1].clone();
    let mut next = 1;
    for k in 0..blocks {
        if next < rows.len() && rows[next].as_array().unwrap()[0].as_f64().unwrap() as usize == k {
            cur = rows[next].as_array().unwrap()[1].clone();
            next += 1;
        }
        out.push(cur.clone());
    }
    out
}

fn first_diff<T: PartialEq + std::fmt::Debug>(got: &[T], want: &[T]) -> Option<String> {
    got.iter()
        .zip(want)
        .position(|(a, b)| a != b)
        .map(|k| format!("block {k}: got {:?}, Swift {:?}", got[k], want[k]))
}

fn sha_of(x: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for v in x {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Got {
    out: Vec<f32>,
    fires: Vec<(i64, String)>,
    peaks: Vec<(u32, u32)>,
    sums: Vec<(f64, f64)>,
    masks: Vec<i64>,
    corners: Vec<String>,
    running: Vec<bool>,
    raw: Vec<(usize, Vec<f32>)>,
}

fn run_case(case: &JsonValue) -> Got {
    let c = case.get("case").unwrap();
    let blocks = c.get("blocks").and_then(JsonValue::as_f64).unwrap() as usize;
    let mut cfg = EngineConfig::new(SR);
    cfg.decks = vec!["A".into()];
    let (mut e, mut r) = engine(cfg);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    if c.get("netFirst").and_then(JsonValue::as_bool) == Some(true) {
        // a net on the deck with its FX unit set, then the sample over it: the setting follows the
        // deck onto its own bus (`leaveNetMode`: `syncFxUnit`). The net never renders.
        assert!(e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal"))));
        e.set_fx("A", FxParam::Filter, -0.6);
        e.set_fx("A", FxParam::Drive, 0.4);
    }
    let b = buffer(s(c, "buffer"));
    let len = b.length() as f64;
    let grid = BeatGrid::new(120.0, 0.0, 1.0);
    assert!(e.load_sample_with("A", PreparedSample::with_grid(b, "tones", Some(grid)), false));
    e.turntable("A", TurntableCommand::Loop { start: 0.0, end: len });
    if c.get("clipFx").and_then(JsonValue::as_bool) == Some(true) {
        e.set_deck_clip_fx("A", &clip_fx(), true);
    }
    e.launch("A");
    let mut script: Vec<Vec<&[JsonValue]>> = vec![Vec::new(); blocks];
    for step in arr(c, "script") {
        let row = step.as_array().unwrap();
        let at = row[0].as_f64().unwrap() as usize;
        if at < blocks {
            script[at].push(&row[1..]);
        }
    }
    let raw_at: Vec<usize> = arr(c, "raw")
        .iter()
        .map(|v| v.as_f64().unwrap() as usize)
        .collect();
    let mirror = e.deck("A").unwrap().mirror.clone();
    let mut g = Got {
        out: Vec::new(),
        fires: Vec::new(),
        peaks: Vec::new(),
        sums: Vec::new(),
        masks: Vec::new(),
        corners: Vec::new(),
        running: Vec::new(),
        raw: Vec::new(),
    };
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    for (k, ops) in script.iter().enumerate() {
        for op in ops {
            let num = |i: usize| op[i].as_f64().unwrap();
            match op[0].as_str().unwrap() {
                "fire" => {
                    let (frame, bpm) = e.sample_master_fire("A");
                    g.fires.push((frame, bits(bpm)));
                    let bars = (op.len() > 2).then(|| num(2) as i32);
                    e.fire_master("A", MasterMacro::ALL[num(1) as usize], bars);
                }
                "panic" => {
                    e.panic("A");
                }
                "slider" => {
                    e.set_master_fx("A", MasterFxKey::ALL[num(1) as usize], num(2) as i32);
                }
                "freeze" => {
                    e.turntable("A", TurntableCommand::Freeze);
                }
                "release" => {
                    e.turntable("A", TurntableCommand::Release);
                }
                "fx" => {
                    e.set_fx("A", FxParam::ALL[num(1) as usize], num(2));
                }
                "kill" => {
                    e.fx_kill("A", num(1) != 0.0);
                }
                "reset" => {
                    e.fx_reset("A");
                }
                "clipfx" => {
                    let fx = if num(1) != 0.0 { clip_fx() } else { LoopFx::default() };
                    e.set_deck_clip_fx("A", &fx, num(2) != 0.0);
                }
                "pause" => {
                    e.pause("A");
                }
                "play" => {
                    e.play("A");
                }
                other => panic!("unknown samplemaster op {other}"),
            }
        }
        r.render_block(&mut l, &mut rr);
        let (mut pl, mut pr, mut sl, mut sr) = (0f32, 0f32, 0f64, 0f64);
        for i in 0..512 {
            pl = pl.max(l[i].abs());
            pr = pr.max(rr[i].abs());
            sl += l[i] as f64;
            sr += rr[i] as f64;
        }
        if k > 0 {
            g.out.extend_from_slice(&l);
            g.out.extend_from_slice(&rr);
        }
        g.peaks.push((pl.to_bits(), pr.to_bits()));
        g.sums.push((sl, sr));
        g.masks.push(
            mirror
                .active_master_macros()
                .iter()
                .fold(0i64, |m, x| m | x.bit() as i64),
        );
        let (hp, lp, idle) = mirror.master_filter_corners();
        g.corners
            .push(format!("{}:{}:{}", bits(hp), bits(lp), idle as u8));
        g.running.push(mirror.clip_fx_running());
        if raw_at.contains(&k) {
            let mut v = l.clone();
            v.extend_from_slice(&rr);
            g.raw.push((k, v));
        }
    }
    g
}

#[test]
fn a_sample_decks_master_path_matches_swift() {
    let doc = golden();
    let cases = arr(&doc, "cases");
    assert!(!cases.is_empty());
    let mut problems: Vec<String> = Vec::new();
    let (mut identical, mut near, mut worst) = (0, 0, 0f64);
    let mut inexact: Vec<&str> = Vec::new();
    for case in cases {
        let name = s(case, "name");
        let got = run_case(case);
        let n = got.masks.len();
        // ---- exact: what the deck publishes
        let want_fires: Vec<(i64, String)> = arr(case, "fires")
            .iter()
            .map(|f| {
                let f = f.as_array().unwrap();
                (f[0].as_f64().unwrap() as i64, f[1].as_str().unwrap().to_string())
            })
            .collect();
        if got.fires != want_fires {
            problems.push(format!(
                "{name}: sampleMasterFire: got {:?}, Swift {want_fires:?}",
                got.fires
            ));
        }
        let want_masks: Vec<i64> = expand(case.get("masks").unwrap(), n)
            .iter()
            .map(|v| v.as_f64().unwrap() as i64)
            .collect();
        if let Some(d) = first_diff(&got.masks, &want_masks) {
            problems.push(format!("{name}: master macros running differ first at {d}"));
        }
        let want_running: Vec<bool> = expand(case.get("fxRunning").unwrap(), n)
            .iter()
            .map(|v| v.as_bool().unwrap())
            .collect();
        if let Some(d) = first_diff(&got.running, &want_running) {
            problems.push(format!("{name}: the clip's FX chain in the path differs first at {d}"));
        }
        let corner = |c: &str| -> (f64, f64, bool) {
            let p: Vec<&str> = c.split(':').collect();
            (from_bits(p[0]), from_bits(p[1]), p[2] == "1")
        };
        let close = |x: f64, y: f64| (x - y).abs() <= CORNER_TOLERANCE * x.abs().max(y.abs());
        let want_corners = expand(case.get("corners").unwrap(), n);
        if let Some(k) = got.corners.iter().zip(&want_corners).position(|(g, w)| {
            let (a, b) = (corner(g), corner(w.as_str().unwrap()));
            !(close(a.0, b.0) && close(a.1, b.1) && a.2 == b.2)
        }) {
            problems.push(format!(
                "{name}: filter corners differ first at block {k}: got {}, Swift {}",
                got.corners[k],
                want_corners[k].as_str().unwrap()
            ));
        }
        // ---- audio
        if sha_of(&got.out) == s(case, "hash") {
            identical += 1;
            continue;
        }
        let peak = got
            .peaks
            .iter()
            .map(|p| f32::from_bits(p.0).max(f32::from_bits(p.1)) as f64)
            .fold(0.0, f64::max)
            .max(1e-9);
        let (want_peaks, want_sums) = (arr(case, "peaks"), arr(case, "sums"));
        let mut case_worst = 0f64;
        for k in 1..n {
            let wp = want_peaks[k].as_array().unwrap();
            let ws = want_sums[k].as_array().unwrap();
            let wpl = f32::from_bits(wp[0].as_f64().unwrap() as u32) as f64;
            let wpr = f32::from_bits(wp[1].as_f64().unwrap() as u32) as f64;
            case_worst = case_worst
                .max((f32::from_bits(got.peaks[k].0) as f64 - wpl).abs() / peak)
                .max((f32::from_bits(got.peaks[k].1) as f64 - wpr).abs() / peak);
            let per = (got.sums[k].0 - from_bits(ws[0].as_str().unwrap()))
                .abs()
                .max((got.sums[k].1 - from_bits(ws[1].as_str().unwrap())).abs());
            case_worst = case_worst.max(per / (peak * 512.0));
        }
        for (k, v) in &got.raw {
            if *k == 0 {
                continue;
            }
            let want = case.get("raw").unwrap().get(&k.to_string()).unwrap();
            let want = want.as_array().unwrap();
            assert_eq!(want.len(), v.len(), "{name}: raw block {k}");
            for (g, w) in v.iter().zip(want) {
                let w = f32::from_bits(u32::from_str_radix(w.as_str().unwrap(), 16).unwrap());
                case_worst = case_worst.max((*g as f64 - w as f64).abs() / peak);
            }
        }
        worst = worst.max(case_worst);
        near += 1;
        inexact.push(name);
        let tolerance = if name == "sample-octave-up" { OCTAVE_TOLERANCE } else { TOLERANCE };
        if case_worst > tolerance {
            problems.push(format!(
                "{name}: audio is {case_worst:.3e} of its peak from Swift's (tolerance {tolerance:e}); \
                 hash {} vs {}",
                sha_of(&got.out),
                s(case, "hash")
            ));
        }
    }
    eprintln!(
        "samplemaster golden: {} cases; audio {identical} bit-identical by sha256, {near} within \
         {TOLERANCE:e} of the peak (worst {worst:.3e}; {inexact:?})",
        cases.len()
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
