//! Port of Swift `TranslationTests`: the device curves and the mono fold, against signals
//! whose fate is known.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use djrust::musicality::translation::*;

const SR: f64 = 48_000.0;
const SECONDS: f64 = 2.0;

fn sine(hz: f64, amp: f64) -> Vec<f32> {
    (0..(SECONDS * SR) as usize)
        .map(|i| (amp * (2.0 * PI * hz * i as f64 / SR).sin()) as f32)
        .collect()
}

/// A band-limited sawtooth (every harmonic under Nyquist, at 1/n).
fn saw(hz: f64, amp: f64) -> Vec<f32> {
    let harmonics = (SR / 2.0 / hz) as usize;
    (0..(SECONDS * SR) as usize)
        .map(|i| {
            let t = i as f64 / SR;
            let mut y = 0.0;
            for n in 1..=harmonics {
                y += (2.0 * PI * hz * n as f64 * t).sin() / n as f64;
            }
            (amp * y * 2.0 / PI) as f32
        })
        .collect()
}

#[test]
fn sub_bass_is_flagged_on_the_phone() {
    let bass = sine(45.0, 0.25);
    let r = analyse("bass", &bass, &bass, SR, None);
    let phone = &r.devices["phone"];
    assert!(phone.flagged);
    assert!(phone.level_db < -FLAG_DB);
    assert!(r.flags.contains(&"phone".to_string()));
    assert!(!r.flags.contains(&"mono".to_string()));
    assert!(!r.devices["headphones"].flagged);
}

#[test]
fn saw_on_the_same_note_keeps_its_harmonics_on_the_phone() {
    // A1 = 55 Hz: the fundamental goes, the harmonics above 350 Hz carry it
    let bass = saw(55.0, 0.2);
    let r = analyse("bass", &bass, &bass, SR, None);
    assert!(!r.devices["phone"].flagged);
    assert!(r.devices["phone"].level_db > -FLAG_DB);
    assert!(r.flags.is_empty());
}

#[test]
fn opposite_polarity_channels_are_flagged_in_mono() {
    let l = sine(440.0, 0.25);
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    let rep = analyse("pad", &l, &r, SR, None);
    assert!(rep.mono_fold_loss_db <= -FLAG_DB);
    assert!(rep.flags.contains(&"mono".to_string()));
    // the phone folds to mono too
    assert!(rep.flags.contains(&"phone".to_string()));
    // headphones and monitors are stereo: nothing lost there
    assert!(!rep.devices["headphones"].flagged);
    assert!(!rep.devices["monitors"].flagged);
}

#[test]
fn centred_part_folds_without_loss() {
    let x = sine(440.0, 0.25);
    assert!(mono_fold_loss_db(&x, &x).abs() < 0.01);
    let rep = analyse("lead", &x, &x, SR, None);
    assert!(rep.flags.is_empty());
    // 440 Hz is inside every curve's passband: within 1.5 dB everywhere
    for d in DEVICES.iter() {
        assert!(rep.devices[d.name].level_db.abs() < 1.5, "{}", d.name);
    }
}

#[test]
fn headphones_lift_the_bass() {
    let x = sine(40.0, 0.25);
    let rep = analyse("bass", &x, &x, SR, None);
    assert!(rep.devices["headphones"].level_db > 2.5);
}

#[test]
fn every_device_names_its_source() {
    assert_eq!(
        DEVICES.iter().map(|d| d.name).collect::<Vec<_>>(),
        vec!["phone", "headphones", "monitors"]
    );
    for d in DEVICES.iter() {
        assert!(!d.source.is_empty());
    }
}

#[test]
fn report_orders_parts_and_measures_against_the_mix() {
    let bass = sine(45.0, 0.25);
    let lead = sine(880.0, 0.125);
    let mix: Vec<f32> = bass.iter().zip(&lead).map(|(a, b)| a + b).collect();
    let mut renders: BTreeMap<String, (Vec<f32>, Vec<f32>)> = BTreeMap::new();
    renders.insert("mix".into(), (mix.clone(), mix));
    renders.insert("lead".into(), (lead.clone(), lead));
    renders.insert("bass".into(), (bass.clone(), bass));
    let parts = report(&renders, SR);
    assert_eq!(
        parts.iter().map(|p| p.part.as_str()).collect::<Vec<_>>(),
        vec!["bass", "lead"]
    );
    for p in &parts {
        assert!(p.vs_mix_db.unwrap() < 0.0);
    }
    assert_eq!(parts[0].flags, vec!["phone".to_string()]);
    assert!(parts[1].flags.is_empty());
}
