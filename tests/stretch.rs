use djrust::musicality::pitch_measure::fundamental;
use djrust::stretch::*;

const SR: f64 = 44100.0;

fn sine(hz: f64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * hz * i as f64 / SR).sin() as f32 * 0.5)
        .collect()
}

#[test]
fn shifts_keep_length_and_land_on_pitch() {
    let x = sine(220.0, 88200);
    for s in [-5, -1, 3, 7, 12] {
        let y = pitch_shift(&[&x], s, false);
        assert_eq!(y[0].len(), x.len());
        let mid = &y[0][20000..28192];
        let f = fundamental(mid, SR).expect("pitched");
        let want = 220.0 * ratio(s);
        assert!((f / want - 1.0).abs() < 0.02, "shift {s}: {f} vs {want}");
    }
}

#[test]
fn zero_is_identity_and_circular_keeps_length() {
    // 441 Hz over 44100 frames is whole cycles, and so is its octave: the wrap is clean
    let x = sine(441.0, 44100);
    assert_eq!(pitch_shift(&[&x], 0, true)[0], x);
    let y = pitch_shift(&[&x, &x], 12, true);
    assert_eq!(y.len(), 2);
    assert_eq!(y[0].len(), x.len());
    assert_eq!(y[0], y[1], "channels share offsets");
    let jump = (y[0][0] - y[0][y[0].len() - 1]).abs();
    assert!(jump < 0.15, "seam jump {jump}");
}

#[test]
fn time_fit_changes_length_not_pitch() {
    let x = sine(440.0, 88200);
    let y = time_fit(&[&x], 66150);
    assert_eq!(y[0].len(), 66150);
    let f = fundamental(&y[0][20000..28192], SR).unwrap();
    assert!((f / 440.0 - 1.0).abs() < 0.02, "{f}");
}

#[test]
fn nearest_way_round() {
    assert_eq!(semitones_to(0, 60), 0);
    assert_eq!(semitones_to(0, 67), -5);
    assert_eq!(semitones_to(0, 66), -6);
    assert_eq!(semitones_to(9, 60), 3);
    assert_eq!(pitch_class(440.0).unwrap().0, 9);
}
