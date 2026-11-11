//! A deck's channel-strip EQ and one-knob DJ filter (`Deck.filterNode` in `DJEngine.swift`,
//! an `AVAudioUnitEQ` with five bands).
//!
//! The bands, in Swift's order and with what `ModelEngineFx.swift` measured them to be:
//!
//! - 0 low-pass, 1 high-pass: 2nd-order Butterworth (RBJ, Q = 1/√2), the `filter` knob
//! - 2 low shelf at 250 Hz, 4 high shelf at 4 kHz: RBJ shelves with slope S = 1
//! - 3 mid peak at 1 kHz: the RBJ peaking filter, bandwidth 1.5 octaves
//!
//! Knobs are 0…1. EQ: 0.5 is flat (the band is bypassed within 0.01 of it), 1 is +6 dB and 0
//! is −24 dB (`Deck.eqDecibels`). Filter: 0.45…0.55 is flat; below sweeps the low-pass from
//! 20 kHz down to 200 Hz, above sweeps the high-pass from 20 Hz up to 5 kHz.
//!
//! [`StripEq::process`] allocates nothing (it runs on the render thread).

use core::f64::consts::{LN_2, PI, SQRT_2};
use libm::{cos, pow, sin, sinh, sqrt};

/// `Deck.eqDecibels`: an EQ knob's gain.
pub fn eq_decibels(v: f64) -> f64 {
    if v < 0.5 {
        -24.0 * (0.5 - v) / 0.5
    } else {
        6.0 * (v - 0.5) / 0.5
    }
}

/// The filter knob's corners: `(low-pass, high-pass)`, at most one of them on.
pub fn filter_corners(v: f64) -> (Option<f64>, Option<f64>) {
    if v < 0.45 {
        let t = (0.45 - v) / 0.45;
        (Some(20_000.0 * pow(200.0 / 20_000.0, t)), None)
    } else if v > 0.55 {
        let t = (v - 0.55) / 0.45;
        (None, Some(20.0 * pow(5_000.0 / 20.0, t)))
    } else {
        (None, None)
    }
}

/// Whether an EQ knob leaves its band bypassed.
pub fn eq_is_flat(v: f64) -> bool {
    (v - 0.5).abs() < 0.01
}

/// One biquad, normalised by a0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefficients {
    pub const UNITY: Coefficients = Coefficients {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coefficients {
        Coefficients {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Butterworth low-pass (`pass = false`: high-pass). The corner is clamped inside Nyquist.
    pub fn pass(low: bool, f: f64, sr: f64) -> Coefficients {
        let f = f.clamp(1.0, sr * 0.499);
        let w = 2.0 * PI * f / sr;
        let (c, s) = (cos(w), sin(w));
        let alpha = s / SQRT_2; // s / (2Q), Q = 1/√2
        if low {
            let b = (1.0 - c) / 2.0;
            Self::norm(b, 1.0 - c, b, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
        } else {
            let b = (1.0 + c) / 2.0;
            Self::norm(b, -(1.0 + c), b, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
        }
    }

    /// RBJ peaking filter, `bw` in octaves.
    pub fn peak(f: f64, db: f64, bw: f64, sr: f64) -> Coefficients {
        let a = pow(10.0, db / 40.0);
        let w = 2.0 * PI * f / sr;
        let (c, s) = (cos(w), sin(w));
        let alpha = s * sinh(LN_2 / 2.0 * bw * w / s);
        Self::norm(
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        )
    }

    /// RBJ shelf with slope 1 (`low = false`: the high shelf).
    pub fn shelf(low: bool, f: f64, db: f64, sr: f64) -> Coefficients {
        let a = pow(10.0, db / 40.0);
        let w = 2.0 * PI * f / sr;
        let (c, s) = (cos(w), sin(w));
        let k = 2.0 * sqrt(a) * (s / SQRT_2);
        let (ap, am) = (a + 1.0, a - 1.0);
        if low {
            Self::norm(
                a * (ap - am * c + k),
                2.0 * a * (am - ap * c),
                a * (ap - am * c - k),
                ap + am * c + k,
                -2.0 * (am + ap * c),
                ap + am * c - k,
            )
        } else {
            Self::norm(
                a * (ap + am * c + k),
                -2.0 * a * (am + ap * c),
                a * (ap + am * c - k),
                ap - am * c + k,
                2.0 * (am - ap * c),
                ap - am * c - k,
            )
        }
    }

    /// |H(f)|² at `f`.
    pub fn magnitude2(&self, f: f64, sr: f64) -> f64 {
        let w = 2.0 * PI * f / sr;
        let (c1, s1, c2, s2) = (cos(w), sin(w), cos(2.0 * w), sin(2.0 * w));
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let d = dr * dr + di * di;
        if d > 0.0 {
            (nr * nr + ni * ni) / d
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

#[derive(Clone, Copy)]
struct Band {
    on: bool,
    c: Coefficients,
    s: [State; 2],
}

impl Band {
    const OFF: Band = Band {
        on: false,
        c: Coefficients::UNITY,
        s: [State { z1: 0.0, z2: 0.0 }; 2],
    };

    fn set(&mut self, c: Option<Coefficients>) {
        match c {
            Some(c) => {
                if !self.on {
                    self.s = [State::default(); 2];
                }
                self.on = true;
                self.c = c;
            }
            None => self.on = false,
        }
    }

    #[inline]
    fn run(&mut self, ch: usize, x: f64) -> f64 {
        // transposed direct form II
        let c = &self.c;
        let s = &mut self.s[ch];
        let y = c.b0 * x + s.z1;
        s.z1 = c.b1 * x - c.a1 * y + s.z2;
        s.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

/// The strip's knob positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripSettings {
    pub filter: f64,
    pub eq_low: f64,
    pub eq_mid: f64,
    pub eq_high: f64,
}

impl Default for StripSettings {
    fn default() -> Self {
        StripSettings {
            filter: 0.5,
            eq_low: 0.5,
            eq_mid: 0.5,
            eq_high: 0.5,
        }
    }
}

/// Which strip knob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripKnob {
    Filter,
    EqLow,
    EqMid,
    EqHigh,
}

pub const LOW_SHELF_HZ: f64 = 250.0;
pub const MID_PEAK_HZ: f64 = 1_000.0;
pub const HIGH_SHELF_HZ: f64 = 4_000.0;
pub const MID_BANDWIDTH: f64 = 1.5;

/// The five-band strip on one deck, stereo.
pub struct StripEq {
    pub sample_rate: f64,
    settings: StripSettings,
    bands: [Band; 5],
}

impl StripEq {
    pub fn new(sample_rate: f64) -> StripEq {
        StripEq {
            sample_rate,
            settings: StripSettings::default(),
            bands: [Band::OFF; 5],
        }
    }

    pub fn settings(&self) -> StripSettings {
        self.settings
    }

    /// Every band bypassed: [`process`](Self::process) leaves the block as it is.
    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|b| !b.on)
    }

    pub fn set(&mut self, knob: StripKnob, v: f64) {
        let v = v.clamp(0.0, 1.0);
        let sr = self.sample_rate;
        let eq =
            |v: f64, mk: &dyn Fn(f64) -> Coefficients| (!eq_is_flat(v)).then(|| mk(eq_decibels(v)));
        match knob {
            StripKnob::Filter => {
                self.settings.filter = v;
                let (lp, hp) = filter_corners(v);
                self.bands[0].set(lp.map(|f| Coefficients::pass(true, f, sr)));
                self.bands[1].set(hp.map(|f| Coefficients::pass(false, f, sr)));
            }
            StripKnob::EqLow => {
                self.settings.eq_low = v;
                let c = eq(v, &|db| Coefficients::shelf(true, LOW_SHELF_HZ, db, sr));
                self.bands[2].set(c);
            }
            StripKnob::EqMid => {
                self.settings.eq_mid = v;
                let c = eq(v, &|db| {
                    Coefficients::peak(MID_PEAK_HZ, db, MID_BANDWIDTH, sr)
                });
                self.bands[3].set(c);
            }
            StripKnob::EqHigh => {
                self.settings.eq_high = v;
                let c = eq(v, &|db| Coefficients::shelf(false, HIGH_SHELF_HZ, db, sr));
                self.bands[4].set(c);
            }
        }
    }

    pub fn set_all(&mut self, s: StripSettings) {
        self.set(StripKnob::Filter, s.filter);
        self.set(StripKnob::EqLow, s.eq_low);
        self.set(StripKnob::EqMid, s.eq_mid);
        self.set(StripKnob::EqHigh, s.eq_high);
    }

    /// The strip's response at `f`, dB (bypassed bands count as flat).
    pub fn response_db(&self, f: f64) -> f64 {
        let m2: f64 = self
            .bands
            .iter()
            .filter(|b| b.on)
            .map(|b| b.c.magnitude2(f, self.sample_rate))
            .product();
        10.0 * libm::log10(m2.max(1e-30))
    }

    /// In place, both channels the same length. Real-time safe.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for b in self.bands.iter_mut().filter(|b| b.on) {
            for x in left.iter_mut() {
                *x = b.run(0, *x as f64) as f32;
            }
            for x in right.iter_mut() {
                *x = b.run(1, *x as f64) as f32;
            }
        }
    }
}
