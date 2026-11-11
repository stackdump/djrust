//! Swift goldens for the loop bank (`tools/paritydump/loops.swift`, section "loops", `bank`): a
//! `LoopBank` (voices, clip FX, the master bus with its macros and the vinyl brake, the
//! limiter) rendered in 512-frame blocks, compared per block (peak, RMS), by a float32 hash of
//! the whole render and by a decimated waveform. The pure DSP is compared in
//! `djengine/tests/loops_golden.rs` from the same file.
//!
//! Parity form (b) of the port rules. Where the render is not bit-identical the tolerance below
//! is what was measured, and says why.

mod common_fixture;
mod common_loops;
#[path = "loops_support/mod.rs"]
mod loops_support;

use common_loops::{FxBankRig, SR};
use djrust::deck::turntable::SampleBuffer;
use djrust::fx::loop_fx::LoopFxParams;
use djrust::fx::loop_master::LoopMasterCommand;
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::json::JsonValue;
use djrust::session::types::LoopFx;
use djrust::host::deck::loops::LoopMsg;
use loops_support::*;
use std::sync::Arc;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("loops")
}

const BAR: usize = 24_000; // one bar at 480 bpm, 48 kHz

fn buffer(variant: usize, frames: usize) -> Arc<SampleBuffer> {
    let (l, r) = loop_source(frames, variant);
    Arc::new(SampleBuffer::new(l, Some(r), SR))
}

fn two() -> Vec<Arc<SampleBuffer>> {
    vec![buffer(0, 48_000), buffer(1, 36_000)]
}

fn fire(rig: &mut FxBankRig, m: MasterMacro, bars: i16, frame: i64, bpm: f64) {
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Macro {
        id: m.raw(),
        bars,
        frame,
        bpm,
    }));
}

fn slider(rig: &mut FxBankRig, key: MasterFxKey, value: i16) {
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Slider {
        key: key.index() as u8,
        value,
    }));
}

fn set_fx(rig: &mut FxBankRig, v: usize, fx: &LoopFx) {
    rig.bank.apply(LoopMsg::Fx(v, LoopFxParams::new(fx, SR)));
}

/// `renderBank`: `frames` frames in 512-frame blocks; `at(block, rig)` runs before a block.
fn render(
    rig: &mut FxBankRig,
    frames: usize,
    mut at: impl FnMut(usize, &mut FxBankRig),
) -> (Vec<f32>, Vec<f32>) {
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    let (mut done, mut block) = (0, 0);
    while done < frames {
        let n = 512.min(frames - done);
        at(block, rig);
        rig.bank.render_bank(done as i64, &mut l[..n], &mut r[..n]);
        out_l.extend_from_slice(&l[..n]);
        out_r.extend_from_slice(&r[..n]);
        done += n;
        block += 1;
    }
    (out_l, out_r)
}

fn never(_: usize, _: &mut FxBankRig) {}

/// What the bank's cases may differ by: `(waveform, per-block peak, per-block RMS)`, or None for
/// bit-identical. Measured:
///
/// - `fx` runs the Fuzz preset, a `soft` shaper whose `tanhf` is Apple's libm in Swift: RMS
///   1.1e-9 off, the waveform and peaks within the decimation's sight identical. The bound is 4x
///   the f32 ulp at 0.5 (as in `djengine/tests/loops_golden.rs`).
/// - `octave-up` runs the master chain's pitch shifter, whose LFO wavetables are built by an FFT
///   (vDSP in Swift, an in-crate radix-2 FFT in f64 here: PORTING.md section H): 1.1e-5 on the
///   waveform, 7.9e-6 on a block peak, 3.7e-7 on an RMS. The bound is 3x that.
///
/// Everything else (the voices, the clip FX but soft, the sweep, the vinyl brake with its voices
/// skipping on at the release, the panic, the sliders, the limiter, FX and level changes landing
/// on a frame) is bit-identical.
fn tolerance(name: &str) -> Option<(f64, f64, f64)> {
    match name {
        "fx" => Some((1.2e-7, 2.4e-7, 1e-8)),
        "octave-up" => Some((3.5e-5, 2.5e-5, 1.2e-6)),
        _ => None,
    }
}

#[test]
fn the_bank_renders_what_swifts_does() {
    let g = golden();
    let bank = g.get("bank").unwrap();
    let cases = bank.get("cases").and_then(JsonValue::as_array).unwrap();
    let p = LoopFx::presets();
    let mut ran = Vec::new();

    let mut run = |name: &str, out: (Vec<f32>, Vec<f32>)| {
        let c = by_name(cases, name);
        let m = compare(c, &out.0, &out.1);
        eprintln!(
            "bank {name}: exact {} wave {:.3e} peak {:.3e} rms {:.3e}",
            m.exact, m.max_wave, m.max_peak, m.max_rms
        );
        assert!(m.frames_match, "{name}: length");
        match tolerance(name) {
            None => assert!(m.exact, "{name}: expected bit-identical, off by {m:?}"),
            Some(tol) => assert!(
                m.exact || (m.max_wave <= tol.0 && m.max_peak <= tol.1 && m.max_rms <= tol.2),
                "{name}: not within the measured tolerance {tol:?}: {m:?}"
            ),
        }
        ran.push(name.to_string());
        m.exact
    };

    run(
        "dry",
        render(&mut FxBankRig::new(&two(), &[]), 4 * BAR, never),
    );
    {
        let mut rig = FxBankRig::new(&two(), &[p[3].1.clone(), p[4].1.clone()]);
        let out = render(&mut rig, 3 * BAR, |b, rig| {
            if b == 60 {
                set_fx(rig, 0, &p[2].1);
            }
            if b == 100 {
                set_fx(rig, 1, &LoopFx::default());
            }
        });
        run("fx", out);
    }
    {
        let clips: Vec<_> = (0..6).map(|k| buffer(k % 2, 48_000 - 1000 * k)).collect();
        let loud = LoopFx {
            drive: 24.0,
            auto_level: false,
            ..LoopFx::default()
        };
        let mut rig = FxBankRig::new(&clips, &vec![loud; 6]);
        run("limiter", render(&mut rig, 2 * BAR, never));
    }
    {
        let mut rig = FxBankRig::new(&two(), &[]);
        fire(&mut rig, MasterMacro::SweepLp, 1, 0, 480.0);
        run("sweep-lp", render(&mut rig, 3 * BAR, never));
    }
    {
        let mut rig = FxBankRig::new(&two(), &[]);
        fire(&mut rig, MasterMacro::VinylBrake, 1, 4096, 480.0);
        run("vinyl-brake", render(&mut rig, 4 * BAR, never));
    }
    {
        // a voice that comes in and goes out under the brake
        let mut rig = FxBankRig::new(&two(), &[]);
        fire(&mut rig, MasterMacro::VinylBrake, 2, 1000, 480.0);
        let out = render(&mut rig, 5 * BAR, |b, rig| {
            if b == 20 {
                rig.bank.apply(LoopMsg::Stop {
                    v: 1,
                    frame: 12_000,
                    fade: 480,
                });
            }
            if b == 50 {
                rig.bank.apply(LoopMsg::Start {
                    v: 1,
                    frame: 30_000,
                    fade: 480,
                });
            }
        });
        run("vinyl-brake-stop", out);
    }
    {
        let mut rig = FxBankRig::new(&two(), &[]);
        fire(&mut rig, MasterMacro::OctaveUp, 1, 0, 480.0);
        run("octave-up", render(&mut rig, 3 * BAR, never));
    }
    {
        let mut rig = FxBankRig::new(&two(), &[]);
        fire(&mut rig, MasterMacro::SweepLp, 4, 0, 480.0);
        fire(&mut rig, MasterMacro::PhaserDrone, 4, 0, 480.0);
        let out = render(&mut rig, 3 * BAR, |b, rig| {
            if b == 40 {
                rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Panic));
            }
        });
        run("panic", out);
    }
    {
        let mut rig = FxBankRig::new(&two(), &[]);
        slider(&mut rig, MasterFxKey::Distortion, 80);
        slider(&mut rig, MasterFxKey::CrushBits, 55);
        slider(&mut rig, MasterFxKey::HpFreq, 30);
        run("sliders", render(&mut rig, 2 * BAR, never));
    }
    {
        // a macro landing inside a block, and a second one queued behind it
        let mut rig = FxBankRig::new(&two(), &[p[1].1.clone()]);
        fire(&mut rig, MasterMacro::SweepHp, 1, 5000, 480.0);
        fire(&mut rig, MasterMacro::BuildCrush, 1, 9000, 480.0);
        run("two-fires", render(&mut rig, 3 * BAR, never));
    }
    {
        // FX and level changes that land on a frame inside a block (`FxAt`, `GainAt`): the part
        // before the frame through the old chain, the rest through the new; two changes in one
        // block; one inside a fade out; a silent voice that takes both at once and comes in
        // through them
        let mut rig = FxBankRig::new(&two(), &[]);
        let fx_at = |rig: &mut FxBankRig, v: usize, fx: &LoopFx, frame: i64| {
            rig.bank
                .apply(LoopMsg::FxAt(v, LoopFxParams::new(fx, SR), frame));
        };
        let out = render(&mut rig, 3 * BAR, |b, rig| {
            if b == 10 {
                fx_at(rig, 0, &p[2].1, 6000);
            }
            if b == 14 {
                fx_at(rig, 1, &p[4].1, 7800);
                rig.bank.apply(LoopMsg::GainAt(1, 0.4, 7800));
            }
            if b == 30 {
                fx_at(rig, 0, &LoopFx::default(), 15_700);
                rig.bank.apply(LoopMsg::GainAt(0, 0.6, 15_900));
            }
            if b == 40 {
                rig.bank.apply(LoopMsg::Stop {
                    v: 1,
                    frame: 20_700,
                    fade: 1200,
                });
            }
            if b == 41 {
                fx_at(rig, 1, &p[1].1, 21_200);
            }
            if b == 60 {
                fx_at(rig, 1, &p[2].1, 33_000);
                rig.bank.apply(LoopMsg::GainAt(1, 0.7, 33_500));
                rig.bank.apply(LoopMsg::Start {
                    v: 1,
                    frame: 36_000,
                    fade: 480,
                });
            }
        });
        run("at-frame", out);
    }
    assert_eq!(
        ran.len(),
        cases.len(),
        "every case Swift wrote was compared: {ran:?}"
    );
}
