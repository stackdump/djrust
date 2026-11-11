//! The strip reverb's pure half (`fx::strip_reverb`, `Audio/DeckReverb.swift`): the knob mapping
//! as Swift's `ReverbTests` and `DeckReverb.swift` state it, and the stand-in room's behaviour.
//! The room is held to Swift's AUReverb2 measurement in `djhost/tests/deck_reverb_golden.rs`.

use djrust::fx::strip_reverb::{
    decay_seconds, is_on, reverb2_params, ReverbKnob, StripReverb, RAMP_FRAMES, WET_ON,
};

const SR: f64 = 48_000.0;

/// `ReverbTests.theKnobsReachTheUnit`: wet 0.4 is 40 % in the unit, size 1 is a decay of 8 s, and
/// tone 0 is dark: the highs die first (under a second at the Nyquist).
#[test]
fn the_knobs_map_as_the_swift_tests_say() {
    let p = reverb2_params(0.4, 1.0, 0.0);
    assert!((p.dry_wet_mix - 40.0).abs() < 1e-3);
    assert!((p.decay_time_at_0hz - 8.0).abs() < 1e-3);
    assert!(p.decay_time_at_nyquist < 1.0, "dark: the highs die first");
}

/// `DeckReverb.swift`: decay `0.25 x 32^size`, 0.25 ... 8 s; on from wet 0.005; gain 0; the
/// early reflections spread out with size.
#[test]
fn the_documented_ranges() {
    assert!((decay_seconds(0.0) - 0.25).abs() < 1e-12);
    assert!((decay_seconds(1.0) - 8.0).abs() < 1e-12);
    assert!((decay_seconds(0.5) - 0.25 * 32f64.sqrt()).abs() < 1e-12);
    assert_eq!(decay_seconds(-3.0), 0.25, "clamped");
    assert_eq!(decay_seconds(9.0), 8.0, "clamped");
    assert!(!is_on(0.0) && !is_on(0.004_999) && is_on(WET_ON) && is_on(1.0));
    let (lo, hi) = (reverb2_params(0.5, 0.0, 0.5), reverb2_params(0.5, 1.0, 0.5));
    assert_eq!((lo.min_delay_time, lo.max_delay_time), (0.004, 0.03));
    assert!((hi.min_delay_time - 0.016).abs() < 1e-12 && (hi.max_delay_time - 0.15).abs() < 1e-12);
    assert_eq!(lo.gain, 0.0);
    // tone: the decay at the Nyquist is decay x (0.06 + 0.94 tone)
    let t = reverb2_params(0.5, 0.6, 0.25);
    assert!((t.decay_time_at_nyquist - t.decay_time_at_0hz * (0.06 + 0.94 * 0.25)).abs() < 1e-12);
    // a new unit: off, size and tone at 0.5
    let r = StripReverb::new(SR);
    assert_eq!((r.wet(), r.size(), r.tone(), r.is_on()), (0.0, 0.5, 0.5, false));
}

fn burst() -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0f32; 4 * SR as usize];
    let mut x: u32 = 12345;
    for v in l.iter_mut().take((SR * 0.05) as usize) {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *v = (f64::from(x >> 8) / 16_777_216.0 - 0.5) as f32;
    }
    (l.clone(), l)
}

fn unit(wet: f64, size: f64, tone: f64) -> StripReverb {
    let mut r = StripReverb::new(SR);
    r.set(ReverbKnob::Size, size);
    r.set(ReverbKnob::Tone, tone);
    r.set(ReverbKnob::Wet, wet);
    r
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Off (wet below 0.005) is bypassed: the block is left bit for bit, and nothing is kept.
#[test]
fn off_is_bypassed_and_leaves_the_block_as_it_is() {
    let (l0, r0) = burst();
    for wet in [0.0, 0.004] {
        let mut r = unit(wet, 0.6, 0.5);
        assert!(!r.is_on());
        let (mut l, mut rr) = (l0.clone(), r0.clone());
        r.process(&mut l, &mut rr);
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(rr.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}

/// On, it rings after the sound has stopped; a tail does not come back with the next switch-on
/// (the mix ramps in from the dry, so the first blocks hold nothing of it either).
#[test]
fn it_rings_and_switching_off_clears_the_room() {
    let (l0, r0) = burst();
    let mut r = unit(0.5, 0.6, 0.5);
    let (mut l, mut rr) = (l0.clone(), r0.clone());
    r.process(&mut l, &mut rr);
    let tail = (SR * 0.3) as usize..(SR * 0.6) as usize;
    assert!(rms(&l[tail.clone()]) > 1e-4 && rms(&rr[tail.clone()]) > 1e-4);
    // off (the AU's bypass stops it), then on again with nothing playing: silence, exactly
    r.set(ReverbKnob::Wet, 0.0);
    assert!(!r.is_on());
    r.set(ReverbKnob::Wet, 0.5);
    let (mut l, mut rr) = (vec![0f32; 4800], vec![0f32; 4800]);
    r.process(&mut l, &mut rr);
    // (nothing but the 1e-20 guard that keeps a tail out of the subnormals)
    assert!(l.iter().chain(&rr).all(|v| v.abs() < 1e-15), "no tail came back");
}

/// Wet is a linear crossfade: half-wet halves the dry (before the room has returned anything,
/// inside its 18 ms predelay, and once the mix has ramped there).
#[test]
fn wet_crossfades_linearly() {
    let (l0, r0) = burst();
    let from = RAMP_FRAMES + 1;
    let upto = (SR * 0.017) as usize; // inside the 18 ms predelay
    assert!(from + 100 < upto);
    for wet in [0.25, 0.5, 1.0] {
        let mut r = unit(wet, 0.6, 0.5);
        let (mut l, mut rr) = (l0.clone(), r0.clone());
        r.process(&mut l, &mut rr);
        for i in from..upto {
            let want = (1.0 - wet) * l0[i] as f64;
            assert!((l[i] as f64 - want).abs() < 1e-6, "wet {wet}, frame {i}");
        }
    }
}

/// A change of wet is ramped (a step in the mix clicks on a held note): switched on under a
/// steady tone the first frame is the dry one, and no step is larger than the ramp's share.
#[test]
fn a_wet_change_is_ramped() {
    let sine: Vec<f32> = (0..2400)
        .map(|i| (0.5 * (2.0 * std::f64::consts::PI * 100.0 * i as f64 / SR).sin()) as f32)
        .collect();
    let mut r = StripReverb::new(SR);
    r.set(ReverbKnob::Wet, 0.55);
    let (mut l, mut rr) = (sine.clone(), sine.clone());
    r.process(&mut l, &mut rr);
    assert_eq!(l[0], sine[0] * (1.0 - 1.0 / RAMP_FRAMES as f32), "starts from the dry mix");
    let step = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(step < 0.04, "the largest step between samples is {step}");
    // and a jump while on: 0.005 -> 0.55 is ramped too
    let mut r = unit(0.005, 0.6, 0.5);
    let (mut l, mut rr) = (sine.clone(), sine.clone());
    r.process(&mut l, &mut rr);
    r.set(ReverbKnob::Wet, 0.55);
    let (mut l, mut rr) = (sine.clone(), sine.clone());
    r.process(&mut l, &mut rr);
    let jump = (l[0] - sine[0]).abs();
    assert!(jump < 0.01, "the first frame after the jump moved {jump}");
}

/// The tail never reaches the subnormal floats (a guard far below hearing keeps it out): the
/// longest room, left to ring for minutes, writes none.
#[test]
fn the_tail_stays_out_of_the_subnormals() {
    let (l0, r0) = burst();
    let mut r = unit(1.0, 1.0, 1.0);
    let (mut l, mut rr) = (l0, r0);
    r.process(&mut l, &mut rr);
    let mut subnormal = 0usize;
    let (mut a, mut b) = (vec![0f32; 48_000], vec![0f32; 48_000]);
    for _ in 0..240 {
        a.iter_mut().for_each(|v| *v = 0.0);
        b.iter_mut().for_each(|v| *v = 0.0);
        r.process(&mut a, &mut b);
        subnormal += a.iter().chain(&b).filter(|v| **v != 0.0 && v.abs() < f32::MIN_POSITIVE).count();
    }
    assert_eq!(subnormal, 0, "subnormal samples in four minutes of tail");
}

/// A low tone is never silent: the loop's lowpass is floored (`MIN_HF_GAIN`), so a room of
/// 0.6 s at tone 0.1 is within 25 dB of the same room at 0.5, where an unfloored one is some 50
/// dB down and one at tone 0.02 inaudible.
#[test]
fn a_low_tone_is_never_silent() {
    let (l0, r0) = burst();
    let tail = |size: f64, tone: f64| {
        let mut r = unit(1.0, size, tone);
        let (mut l, mut rr) = (l0.clone(), r0.clone());
        r.process(&mut l, &mut rr);
        20.0 * rms(&l[(SR * 0.1) as usize..(SR * 0.75) as usize]).max(1e-12).log10()
    };
    for size in [0.0, 0.25, 0.6] {
        let mid = tail(size, 0.5);
        for tone in [0.0, 0.02, 0.1] {
            let t = tail(size, tone);
            assert!(t > mid - 25.0, "size {size} tone {tone}: {t:.1} dB against {mid:.1} dB at 0.5");
        }
    }
}

/// Jumping `tone` to 0 over a ringing tail does not freeze a DC offset in the loop (the
/// lowpass corner stays above a few hundred hertz): the tail's mean stays near zero.
#[test]
fn a_jump_to_dark_leaves_no_dc() {
    let (l0, r0) = burst();
    let mut r = unit(1.0, 0.0, 1.0);
    let (mut l, mut rr) = (l0, r0);
    r.process(&mut l[..4800], &mut rr[..4800]);
    r.set(ReverbKnob::Tone, 0.0);
    let (mut a, mut b) = (vec![0f32; 48_000], vec![0f32; 48_000]);
    r.process(&mut a, &mut b);
    let tail = &a[24_000..];
    let mean = tail.iter().map(|v| *v as f64).sum::<f64>() / tail.len() as f64;
    assert!(mean.abs() < 1e-3, "mean {mean}");
}

/// The room decays as the knob says: the tail falls at the rate that makes 60 dB in about the
/// decay time (fitted over its first 20 dB, within 35 % of the nominal: the FDN's lines are 30 to
/// 80 ms), and a longer `size` is longer.
#[test]
fn the_decay_follows_the_size_knob() {
    let (l0, r0) = burst();
    let mut last = 0.0;
    for size in [0.3, 0.6, 1.0] {
        let mut r = unit(1.0, size, 1.0);
        let (mut l, mut rr) = (l0.clone(), r0.clone());
        r.process(&mut l, &mut rr);
        // dB per second, from the 0.15 s mark to where the tail is 20 dB down
        let win = 2400;
        let level = |k: usize| 20.0 * rms(&l[k * win..(k + 1) * win]).max(1e-12).log10();
        let (k0, top) = (3, level(3));
        let k1 = (k0..79).find(|&k| level(k) < top - 20.0).unwrap();
        let slope = (top - level(k1)) / ((k1 - k0) as f64 * 0.05);
        let rt60 = 60.0 / slope;
        let nominal = decay_seconds(size);
        assert!(
            (rt60 / nominal - 1.0).abs() < 0.35,
            "size {size}: RT60 {rt60:.2} s, nominal {nominal:.2} s"
        );
        assert!(rt60 > last);
        last = rt60;
    }
}

/// A darker room has less top: the tail's first difference (a crude highpass) is a smaller part
/// of it at tone 0 than at tone 1.
#[test]
fn tone_darkens_the_tail() {
    let (l0, r0) = burst();
    let brightness = |tone: f64| {
        let mut r = unit(1.0, 0.6, tone);
        let (mut l, mut rr) = (l0.clone(), r0.clone());
        r.process(&mut l, &mut rr);
        let t = &l[(SR * 0.2) as usize..(SR * 1.0) as usize];
        let d: Vec<f32> = t.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(t)
    };
    assert!(brightness(0.0) < 0.5 * brightness(1.0), "{} {}", brightness(0.0), brightness(1.0));
}

/// Every setting stays finite and bounded under loud input, and the knobs clamp.
#[test]
fn it_stays_finite_over_the_grid() {
    let n = 12_000;
    let loud: Vec<f32> = (0..n).map(|i| if (i / 40) % 2 == 0 { 1.0 } else { -1.0 }).collect();
    for wet in [0.005, 0.3, 1.0] {
        for size in [0.0, 0.5, 1.0] {
            for tone in [0.0, 0.5, 1.0] {
                let mut r = unit(wet, size, tone);
                let (mut l, mut rr) = (loud.clone(), loud.clone());
                for _ in 0..8 {
                    r.process(&mut l, &mut rr);
                }
                assert!(
                    l.iter().chain(&rr).all(|v| v.is_finite() && v.abs() < 50.0),
                    "wet {wet} size {size} tone {tone}"
                );
            }
        }
    }
    let mut r = StripReverb::new(SR);
    r.set(ReverbKnob::Wet, 7.0);
    r.set(ReverbKnob::Size, -1.0);
    r.set(ReverbKnob::Tone, f64::NAN);
    assert_eq!((r.wet(), r.size(), r.tone()), (1.0, 0.0, 0.0));
    // (the engine keeps the raw value, as Swift's `Deck.reverb` does; the unit clamps its own)
}
