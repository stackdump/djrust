//! Per-loop after effects on the loop bank (`LoopFxTests.swift`, the audio half): off is exact,
//! drive is the dB it says, every shaper adds harmonics and stays bounded, crush quantises to
//! its bits, changes are smoothed, and the loop channel's limiter holds the peak. Each test
//! names the Swift test it ports (the "(a)" form of PORTING.md's parity rule: the Swift suite's
//! own numbers).
//!
//! The Swift tests that go through `DJEngine` are not here: `thePokeSetsTheClipAndItsVoice` is
//! `the_poke_sets_the_clip_and_its_voice` in `deck_loop_row.rs` (with the removed clip's FX);
//! the rest (sequences, the library, the journal checkpoint, a clip on deck A / B, the session
//! replay) are held under other names where `PORTING.md` says, or not yet.

mod common_loops;

use common_loops::*;
use djrust::fx::loop_fx::LoopFxLimiter;
use djrust::session::types::{LoopFx, LoopFxMode};

const HZ: f64 = 375.0; // 128 samples a period at 48 kHz

fn fx(drive: f64, mode: LoopFxMode, amount: f64) -> LoopFx {
    LoopFx {
        drive,
        mode,
        amount,
        ..LoopFx::default()
    }
}

// LoopFxTests.noFxIsBitIdenticalToNoChain
#[test]
fn no_fx_is_bit_identical_to_no_chain() {
    let clips = [sine_buffer(375.0, 0.25, 1.0), sine_buffer(500.0, 0.3, 1.0)];
    let plain = FxBankRig::new(&clips, &[]).render(48_000);
    // the default value, and settings that are bypassed or do nothing, never enter the chain
    let off = [
        LoopFx::default(),
        LoopFx {
            bypass: true,
            drive: 24.0,
            mode: LoopFxMode::Fold,
            amount: 1.0,
            tone: 0.2,
            trim: 6.0,
            ..LoopFx::default()
        },
        LoopFx {
            mode: LoopFxMode::Soft,
            amount: 0.0,
            ..LoopFx::default()
        },
        LoopFx {
            drive: 12.0,
            mix: 0.0,
            ..LoopFx::default()
        },
    ];
    for f in off {
        assert!(!f.is_active(), "{f:?}");
        let out = FxBankRig::new(&clips, &[f.clone(), f.clone()]).render(48_000);
        assert!(out.0 == plain.0 && out.1 == plain.1, "{f:?}");
    }
}

// LoopFxTests.switchedOffItIsExactAgainAfterTheFade
#[test]
fn switched_off_it_is_exact_again_after_the_fade() {
    let clips = [sine_buffer(375.0, 0.25, 1.0)];
    let plain = FxBankRig::new(&clips, &[]).render(48_000);
    let mut rig = FxBankRig::new(&clips, &[]);
    rig.skip(4096);
    rig.set(0, &fx(6.0, LoopFxMode::Soft, 0.7));
    rig.skip(8192);
    rig.set(0, &LoopFx::default());
    rig.skip(4096); // the 10 ms fade out, and the limiter (never engaged) idle
    let from = rig.frame as usize;
    assert_eq!(from, 16_384);
    let after = rig.render(48_000 - from);
    assert!(plain.0[from..] == after.0[..]);
    assert!(!rig.bank.fx_running(0));
}

// LoopFxTests.driveIsTheGainItSaysAndAutoLevelHoldsTheLevel
#[test]
fn drive_is_the_gain_it_says_and_auto_level_holds_the_level() {
    let clip = sine_buffer(375.0, 0.05, 1.0);
    let dry = rms(&FxBankRig::new(std::slice::from_ref(&clip), &[])
        .render(24_576)
        .0);
    for drive in [6.0, 12.0, 24.0] {
        let loud = FxBankRig::new(
            std::slice::from_ref(&clip),
            &[LoopFx {
                drive,
                auto_level: false,
                ..LoopFx::default()
            }],
        )
        .render(24_576);
        assert!(
            (db(rms(&loud.0) / dry) - drive).abs() < 0.05,
            "+{drive} dB: {}",
            db(rms(&loud.0) / dry)
        );
        let level = FxBankRig::new(
            std::slice::from_ref(&clip),
            &[LoopFx {
                drive,
                ..LoopFx::default()
            }],
        )
        .render(24_576);
        assert!(
            db(rms(&level.0) / dry).abs() < 1.0,
            "auto-level at +{drive} dB"
        );
    }
}

// LoopFxTests.autoLevelKeepsADistortedLoopNearItsLevel
#[test]
fn auto_level_keeps_a_distorted_loop_near_its_level() {
    let clip = sine_buffer(375.0, 0.2, 1.0);
    let dry = rms(&FxBankRig::new(std::slice::from_ref(&clip), &[])
        .render(48_000)
        .0);
    for mode in [
        LoopFxMode::Soft,
        LoopFxMode::Hard,
        LoopFxMode::Tube,
        LoopFxMode::Fold,
    ] {
        let mut rig = FxBankRig::new(std::slice::from_ref(&clip), &[fx(12.0, mode, 0.8)]);
        rig.skip(48_000); // the followers settle (~0.3 s)
        let out = rig.render(24_576);
        let d = db(rms(&out.0) / dry);
        assert!(d.abs() < 1.5, "{mode:?}: {d} dB");
    }
}

// LoopFxTests.everyShaperAddsHarmonicsAndIsBounded
#[test]
fn every_shaper_adds_harmonics_and_is_bounded() {
    let clip = sine_buffer(375.0, 0.25, 1.0);
    let dry = FxBankRig::new(std::slice::from_ref(&clip), &[]).render(24_576);
    let clean = thd(&dry.0[4096..], HZ);
    assert!(clean < 1e-4, "the dry sine is clean: {clean}");
    for mode in [
        LoopFxMode::Soft,
        LoopFxMode::Hard,
        LoopFxMode::Tube,
        LoopFxMode::Fold,
        LoopFxMode::Crush,
    ] {
        for drive in [0.0, 24.0] {
            let f = LoopFx {
                drive,
                mode,
                amount: 0.8,
                bits: 4.0,
                ..LoopFx::default()
            };
            let out = FxBankRig::new(std::slice::from_ref(&clip), &[f]).render(24_576);
            let d = thd(&out.0[4096..], HZ);
            assert!(d > 0.02, "{mode:?} +{drive} dB adds harmonics: THD {d}");
            assert!(
                out.0.iter().chain(&out.1).all(|v| v.is_finite()),
                "{mode:?}"
            );
            let p = peak(&out.0);
            assert!(
                p <= LoopFxLimiter::CEILING + 1e-6,
                "{mode:?} +{drive} dB peak {p}"
            );
        }
    }
}

// LoopFxTests.tubeIsAsymmetricAndTheOthersAreNot
#[test]
fn tube_is_asymmetric_and_the_others_are_not() {
    // even harmonics come from an asymmetric curve: tube's second harmonic, soft's none
    let clip = sine_buffer(375.0, 0.25, 1.0);
    let second = |mode: LoopFxMode| {
        let out = FxBankRig::new(std::slice::from_ref(&clip), &[fx(6.0, mode, 0.7)]).render(24_576);
        let tail = &out.0[8192..];
        tone_amplitude(tail, 2.0 * HZ) / tone_amplitude(tail, HZ)
    };
    let (tube, soft, hard) = (
        second(LoopFxMode::Tube),
        second(LoopFxMode::Soft),
        second(LoopFxMode::Hard),
    );
    assert!(tube > 0.01, "{tube}");
    assert!(soft < 1e-3, "{soft}");
    assert!(hard < 1e-3, "{hard}");
}

// LoopFxTests.crushQuantisesToItsBitsAndHoldsItsSamples
#[test]
fn crush_quantises_to_its_bits_and_holds_its_samples() {
    let clip = sine_buffer(375.0, 0.4, 1.0);
    for bits in [2.0, 4.0, 6.0] {
        let f = LoopFx {
            mode: LoopFxMode::Crush,
            amount: 1.0,
            bits,
            auto_level: false,
            ..LoopFx::default()
        };
        let out = FxBankRig::new(std::slice::from_ref(&clip), &[f]).render(8192);
        let step = djrust::fx::master_chain::crush_step(bits) as f32;
        assert!(
            out.0.iter().all(|&v| (v / step).round() * step == v),
            "every sample on the {bits}-bit grid"
        );
        let mut levels: Vec<u32> = out.0.iter().map(|v| v.to_bits()).collect();
        levels.sort_unstable();
        levels.dedup();
        assert!(
            levels.len() <= (2.0 * 0.4 / step as f64) as usize + 2,
            "and only those levels: {}",
            levels.len()
        );
    }
    // /4: each value held for four samples, from the voice's first
    let held = FxBankRig::new(
        &[clip],
        &[LoopFx {
            mode: LoopFxMode::Crush,
            amount: 1.0,
            bits: 12.0,
            downsample: 4,
            auto_level: false,
            ..LoopFx::default()
        }],
    )
    .render(8192)
    .0;
    let changes: Vec<usize> = (1..held.len())
        .filter(|&i| held[i] != held[i - 1])
        .collect();
    assert!(
        !changes.is_empty() && changes.iter().all(|c| c % 4 == 0),
        "{:?}",
        &changes[..changes.len().min(8)]
    );
}

// LoopFxTests.parameterChangesAreSmoothed
#[test]
fn parameter_changes_are_smoothed() {
    // each change mid-loop: the samples while it moves never jump further from one to the next
    // than the steadier of the settings before and after it (an unsmoothed jump to +24 dB at a
    // peak of this sine would be ~0.75 in one sample)
    let clip = sine_buffer(375.0, 0.05, 1.0);
    let base = LoopFx {
        auto_level: false,
        ..LoopFx::default()
    };
    let mut rig = FxBankRig::new(&[clip], std::slice::from_ref(&base));
    rig.skip(4096);
    let mut before = max_step(&rig.render(2048).0);
    let d24 = LoopFx {
        drive: 24.0,
        ..base.clone()
    };
    let fold = LoopFx {
        mode: LoopFxMode::Fold,
        amount: 0.3,
        ..d24.clone()
    };
    let settings = [
        d24.clone(),
        base.clone(),
        d24.clone(),
        LoopFx {
            mode: LoopFxMode::Soft,
            amount: 0.3,
            ..d24.clone()
        },
        fold.clone(),
        LoopFx {
            tone: 0.0,
            ..fold.clone()
        },
        LoopFx {
            tone: 0.0,
            mix: 0.2,
            ..fold.clone()
        },
        LoopFx {
            tone: 0.0,
            mix: 0.2,
            trim: -12.0,
            ..fold.clone()
        },
        LoopFx::default(),
    ];
    for f in settings {
        rig.set(0, &f);
        let moving = max_step(&rig.render(4096).0);
        let after = max_step(&rig.render(2048).0);
        assert!(
            moving <= 1.1 * before.max(after) + 0.002,
            "{}: {moving} vs {before} -> {after}",
            f.summary()
        );
        before = after;
    }
}

// LoopFxTests.theLimiterHoldsTheChannelUnderFullScale
#[test]
fn the_limiter_holds_the_channel_under_full_scale() {
    // six loops that sum under full scale dry, all pushed +24 dB
    let clips: Vec<_> = (0..6)
        .map(|k| sine_buffer(375.0 + 125.0 * k as f64, 0.1, 1.0))
        .collect();
    let f = LoopFx {
        drive: 24.0,
        auto_level: false,
        ..LoopFx::default()
    };
    let mut rig = FxBankRig::new(&clips, &vec![f; clips.len()]);
    let out = rig.render(96_000);
    let p = peak(&out.0).max(peak(&out.1));
    assert!(p <= LoopFxLimiter::CEILING + 1e-6, "{p}");
    assert!(p < 1.0, "{p}");
    assert!(p > 0.9, "and it is loud: {p}");
    assert!(out.0.iter().all(|v| v.is_finite()));
    // once the FX are off and the gain has come back up, it lets go
    for v in 0..clips.len() {
        rig.set(v, &LoopFx::default());
    }
    rig.skip(96_000);
    assert!(rig.bank.limiter().idle());
}

// LoopFxTests (presets, LoopFX.summary, isActive: the value type)
#[test]
fn presets_and_summaries_read_as_swift_does() {
    let p = LoopFx::presets();
    assert_eq!(
        p.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        ["Off", "Warm drive", "Crunch", "Fuzz", "Crush", "Loud"]
    );
    assert_eq!(p[0].1, LoopFx::default());
    assert_eq!(p[1].1.preset_name(), Some("Warm drive"));
    assert_eq!(LoopFx::default().summary(), "off");
    assert_eq!(
        LoopFx {
            bypass: true,
            drive: 6.0,
            ..LoopFx::default()
        }
        .summary(),
        "bypassed"
    );
    assert_eq!(
        p[1].1.summary(),
        "drive +6 dB \u{b7} tube 35% \u{b7} tone 80%"
    );
    assert_eq!(
        p[4].1.summary(),
        "crush 6 bit \u{f7}4 \u{b7} tone 85% \u{b7} mix 80%"
    );
    assert_eq!(
        p[5].1.summary(),
        "drive +9 dB \u{b7} soft 15% \u{b7} auto-level off"
    );
    assert_eq!(
        LoopFx {
            trim: -3.5,
            ..LoopFx::default()
        }
        .summary(),
        "trim -3.5 dB"
    );
    assert!(p.iter().skip(1).all(|(_, f)| f.is_active()));
}
