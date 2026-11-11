//! The strip EQ and DJ filter (`Deck.filterNode`): knob mappings and the band responses.

use djrust::fx::strip::{eq_decibels, filter_corners, Coefficients, StripEq, StripKnob};

const SR: f64 = 48_000.0;

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn eq_knob_maps_like_deck_eq_decibels() {
    assert_eq!(eq_decibels(0.5), 0.0);
    assert_eq!(eq_decibels(1.0), 6.0);
    assert_eq!(eq_decibels(0.0), -24.0);
    assert_eq!(eq_decibels(0.25), -12.0);
}

#[test]
fn filter_knob_sweeps_lp_down_and_hp_up() {
    assert_eq!(filter_corners(0.5), (None, None));
    assert_eq!(filter_corners(0.45), (None, None));
    let (lp, hp) = filter_corners(0.0);
    assert!(near(lp.unwrap(), 200.0, 1e-6) && hp.is_none());
    let (lp, hp) = filter_corners(1.0);
    assert!(lp.is_none() && near(hp.unwrap(), 5_000.0, 1e-6));
}

#[test]
fn flat_strip_is_bypassed_and_leaves_audio_alone() {
    let mut s = StripEq::new(SR);
    s.set(StripKnob::EqLow, 0.505);
    s.set(StripKnob::Filter, 0.52);
    assert!(s.is_flat());
    let mut l: Vec<f32> = (0..64).map(|i| (i as f32 * 0.1).sin()).collect();
    let mut r = l.clone();
    let want = l.clone();
    s.process(&mut l, &mut r);
    assert_eq!(l, want);
    assert_eq!(r, want);
}

#[test]
fn bands_hit_their_gains() {
    let mut s = StripEq::new(SR);
    s.set(StripKnob::EqLow, 1.0);
    assert!(
        near(s.response_db(20.0), 6.0, 0.05),
        "{}",
        s.response_db(20.0)
    );
    assert!(near(s.response_db(15_000.0), 0.0, 0.1));
    s.set(StripKnob::EqLow, 0.0);
    assert!(
        near(s.response_db(20.0), -24.0, 0.3),
        "{}",
        s.response_db(20.0)
    );
    s.set(StripKnob::EqLow, 0.5);

    s.set(StripKnob::EqMid, 0.0);
    assert!(
        near(s.response_db(1_000.0), -24.0, 1e-6),
        "{}",
        s.response_db(1_000.0)
    );
    assert!(near(s.response_db(30.0), 0.0, 0.2));
    s.set(StripKnob::EqMid, 0.5);

    s.set(StripKnob::EqHigh, 1.0);
    assert!(near(s.response_db(18_000.0), 6.0, 0.2));
    assert!(
        near(s.response_db(4_000.0), 3.0, 1e-6),
        "corner = half the gain"
    );
    assert!(near(s.response_db(30.0), 0.0, 0.05));
}

#[test]
fn filter_corner_is_minus_3_db() {
    let mut s = StripEq::new(SR);
    s.set(StripKnob::Filter, 0.0);
    assert!(near(s.response_db(200.0), -3.0103, 1e-3));
    assert!(s.response_db(2_000.0) < -35.0);
    s.set(StripKnob::Filter, 1.0);
    assert!(near(s.response_db(5_000.0), -3.0103, 1e-3));
    assert!(s.response_db(500.0) < -35.0);
}

#[test]
fn processing_matches_the_response() {
    // a 1 kHz tone through a −24 dB mid cut comes out 24 dB down
    let mut s = StripEq::new(SR);
    s.set(StripKnob::EqMid, 0.0);
    let n = 48_000;
    let mut l: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / SR).sin() as f32)
        .collect();
    let mut r = l.clone();
    s.process(&mut l, &mut r);
    let rms =
        |v: &[f32]| (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    let db = 20.0 * (rms(&l[n / 2..]) / (0.5f64).sqrt()).log10();
    assert!(near(db, -24.0, 0.2), "{db}");
    assert_eq!(l, r);
}

#[test]
fn coefficients_unity_is_flat() {
    assert_eq!(Coefficients::UNITY.magnitude2(1234.0, SR), 1.0);
}
