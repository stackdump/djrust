//! The FX unit on the loops' master bus (`FxUnitTests.FxUnitBusTests`), which PORTING.md's
//! section B listed as not ported as tests (package F of the parity audit): a unit at its neutral
//! values is bit-transparent, a reset or a kill brings the bus back bit-identical, the HP corner
//! glides to the knob and holds, and a macro rides on the unit and returns to it. `fx_unit.rs`
//! holds the layer (glide, coalescing, reset frame) and `deck_fx_unit.rs` the unit on a deck; this
//! is the bus the loops go through, on `FxBankRig` (`common_loops`: two clips on a loop bank, the
//! bank's own output, 512-frame blocks).

mod common_loops;

use common_loops::*;
use djrust::fx::loop_master::LoopMasterCommand;
use djrust::fx::master_chain::MasterParams;
use djrust::fx::unit::{FxParam, FxUnitCommand};
use djrust::fx::MasterMacro;
use djrust::host::deck::loops::LoopMsg;

/// `FxUnitTests.tones`: a low and a high sine, whole periods.
fn tones() -> Vec<std::sync::Arc<djrust::deck::turntable::SampleBuffer>> {
    vec![sine_buffer(375.0, 0.15, 1.0), sine_buffer(6000.0, 0.15, 1.0)]
}

/// `FxUnitTests.set`: a control of the unit, to `v`, glided or `snap`ped.
fn set(rig: &mut FxBankRig, p: FxParam, v: f64, snap: bool) {
    unit(
        rig,
        FxUnitCommand::Set {
            param: p.index() as u8,
            value: v,
            snap,
        },
    );
}

fn unit(rig: &mut FxBankRig, c: FxUnitCommand) {
    rig.bank.apply(LoopMsg::Master(LoopMasterCommand::FxUnit(c)));
}

fn corners(rig: &FxBankRig) -> (f64, f64, bool) {
    rig.bank.master().filter_corners()
}

// FxUnitBusTests.aNeutralUnitIsBitTransparent: a unit set to its neutral values (never moved)
// sends nothing to the chain: the same bytes.
#[test]
fn a_neutral_unit_is_bit_transparent() {
    let dry = FxBankRig::new(&tones(), &[]).render(48_000);
    let mut rig = FxBankRig::new(&tones(), &[]);
    for p in FxParam::ALL {
        set(&mut rig, p, p.neutral(), false);
    }
    unit(&mut rig, FxUnitCommand::Kill { on: false, frame: 0 });
    let wet = rig.render(48_000);
    assert!(wet.0 == dry.0 && wet.1 == dry.1, "the same bytes");
    assert!(corners(&rig).2, "bypassed");
}

// FxUnitBusTests.afterAResetOrAKillTheBusIsBitIdenticalAgain: every control moved, then reset
// (and, again, killed): once the glides and the stages' fades are done the bus is back to bypass
// and the bytes are the dry ones.
#[test]
fn after_a_reset_or_a_kill_the_bus_is_bit_identical_again() {
    for kill in [false, true] {
        let dry = FxBankRig::new(&tones(), &[]).render(4 * 48_000);
        let mut rig = FxBankRig::new(&tones(), &[]);
        for (p, v) in [
            (FxParam::Filter, -0.4),
            (FxParam::Hp, 0.3),
            (FxParam::PhaserMix, 0.6),
            (FxParam::PhaserDepth, 0.8),
            (FxParam::PhaserRate, 0.3),
            (FxParam::Crush, 0.5),
            (FxParam::Drive, 0.4),
            (FxParam::Pitch, -5.0),
        ] {
            set(&mut rig, p, v, false);
        }
        let mut wet = rig.render(48_000);
        assert!(
            wet.0[24_000..48_000] != dry.0[24_000..48_000],
            "the unit is heard"
        );
        assert!(!corners(&rig).2);
        if kill {
            unit(&mut rig, FxUnitCommand::Kill { on: true, frame: 0 });
        } else {
            unit(&mut rig, FxUnitCommand::Reset { frame: 0 });
        }
        let rest = rig.render(3 * 48_000);
        wet.0.extend_from_slice(&rest.0);
        wet.1.extend_from_slice(&rest.1);
        let from = 2 * 48_000;
        assert!(wet.0[from..] == dry.0[from..], "kill {kill}");
        assert!(wet.1[from..] == dry.1[from..]);
        assert!(corners(&rig).2);
        assert!(rig.bank.limiter().idle());
    }
}

// FxUnitBusTests.theCornerGlidesThereAndHolds: the HP corner glides to where the knob is (no
// jump at the first block), then holds, and the low tone is cut while the high one passes.
#[test]
fn the_corner_glides_there_and_holds() {
    let mut rig = FxBankRig::new(&tones(), &[]);
    set(&mut rig, FxParam::Hp, 0.6, false);
    let target = MasterParams::hp_freq(60.0);
    rig.skip(512);
    let first = corners(&rig).0;
    assert!(first > 20.0 && first < target * 0.8, "part of the way after a block: {first}");
    let mut last = first;
    for _ in 0..40 {
        rig.skip(512);
        let c = corners(&rig).0;
        assert!(c >= last - 1e-9, "never back");
        last = c;
    }
    assert!((last - target).abs() / target < 1e-3, "{last} Hz vs {target}");
    // the low tone is cut, the high one passes
    let w = rig.render(4608 * 2);
    let mut dry = FxBankRig::new(&tones(), &[]);
    dry.skip((rig.frame - 4608 * 2) as usize);
    let d = dry.render(4608 * 2);
    let low = tone_amplitude(&w.0[0..4608], 375.0);
    let low_dry = tone_amplitude(&d.0[0..4608], 375.0);
    assert!(db(low / low_dry) < -12.0, "375 Hz under a 550 Hz HP: {} dB", db(low / low_dry));
}

// FxUnitBusTests.aMacroReturnsToTheUnitsSetting: a macro rides on top of the unit and returns to
// it: sweep-hp from the unit's HP up and back to the unit's corner, not to 20 Hz.
#[test]
fn a_macro_returns_to_the_units_setting() {
    let mut rig = FxBankRig::new(&tones(), &[]);
    set(&mut rig, FxParam::Hp, 0.3, true);
    set(&mut rig, FxParam::Lp, 0.7, true);
    rig.skip(4096);
    let base = MasterParams::hp_freq(30.0);
    let lp_base = MasterParams::lp_freq(70.0);
    assert!((corners(&rig).0 - base).abs() < 1e-6);
    for m in [MasterMacro::SweepHp, MasterMacro::SweepLp] {
        // one bar = 1 s
        rig.bank.apply(LoopMsg::Master(LoopMasterCommand::Macro {
            id: m.raw(),
            bars: 1,
            frame: 0,
            bpm: 240.0,
        }));
    }
    let (mut peak_hp, mut lowest_lp) = (0.0f64, 1e9f64);
    while rig.frame < 4096 + 48_000 {
        rig.skip(512);
        let (hp, lp, _) = corners(&rig);
        peak_hp = peak_hp.max(hp);
        lowest_lp = lowest_lp.min(lp);
        assert!(hp >= base - 1e-6, "never under the unit's HP");
        assert!(lp <= lp_base + 1e-6, "never over the unit's LP");
    }
    assert!(peak_hp > 1000.0, "the sweep went up: {peak_hp}");
    assert!(lowest_lp < 300.0, "and the LP down: {lowest_lp}");
    rig.skip(48_000);
    assert_eq!(rig.bank.master().mask(), 0);
    let (hp, lp, _) = corners(&rig);
    assert!((hp - base).abs() < 1e-6, "back on the unit's HP");
    assert!((lp - lp_base).abs() < 1e-6, "and its LP");
}
