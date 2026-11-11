//! Port of `Musicality/Translation.swift`: does a mix translate (ROADMAP 6.6)? Each part,
//! soloed, played through the curves of the devices people listen on, and folded to mono:
//! how much of it is left, K-weighted, against its own full-range level. The curves are
//! approximations of a class of device, each naming where it comes from.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::musicality::loudness::{IntegratedLoudness, KWeighting};
use crate::musicality::mix_balance::MixBalance;
use crate::wave::synth::biquad;

/// A part this far (dB) under its full-range level on a device, or in mono, is flagged.
pub const FLAG_DB: f64 = 12.0;
/// The floor a level difference is clamped to.
pub const FLOOR_DB: f64 = -100.0;

/// One device's playback curve.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: &'static str,
    /// Where the curve comes from (it is an approximation of a class of device).
    pub source: &'static str,
    pub highpass_hz: Option<f64>,
    pub highpass_order: u8,
    pub lowpass_hz: Option<f64>,
    pub lowpass_order: u8,
    /// (corner Hz, gain dB), RBJ shelf at slope 1.
    pub low_shelf: Option<(f64, f64)>,
    pub high_shelf: Option<(f64, f64)>,
    /// One speaker: (L + R) / 2 on both channels.
    pub mono: bool,
}

impl Device {
    const BASE: Device = Device {
        name: "",
        source: "",
        highpass_hz: None,
        highpass_order: 4,
        lowpass_hz: None,
        lowpass_order: 2,
        low_shelf: None,
        high_shelf: None,
        mono: false,
    };
}

/// A phone's speaker: no bass under a few hundred Hz, a dull top, one channel.
pub const PHONE: Device = Device {
    name: "phone",
    source: "ROADMAP 6.6's phone check: a phone speaker approximated as a 4th-order Butterworth highpass at 350 Hz, 2nd-order lowpass at 9 kHz, folded to mono",
    highpass_hz: Some(350.0),
    highpass_order: 4,
    lowpass_hz: Some(9000.0),
    lowpass_order: 2,
    mono: true,
    ..Device::BASE
};
/// Over-ear headphones: the bass lift of a consumer headphone target.
pub const HEADPHONES: Device = Device {
    name: "headphones",
    source: "Harman over-ear target (Olive & Welti, 2018), approximated as a +4 dB low shelf at 105 Hz and a −3 dB high shelf at 10 kHz; stereo",
    low_shelf: Some((105.0, 4.0)),
    high_shelf: Some((10000.0, -3.0)),
    ..Device::BASE
};
/// Small near-field monitors: flat, with a ported woofer's bass roll-off.
pub const MONITORS: Device = Device {
    name: "monitors",
    source: "a 5-inch ported near-field monitor, approximated as flat above a 4th-order Butterworth highpass at 50 Hz (its −3 dB point); stereo",
    highpass_hz: Some(50.0),
    highpass_order: 4,
    ..Device::BASE
};

pub const DEVICES: [Device; 3] = [PHONE, HEADPHONES, MONITORS];

/// One part through one device.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceLevel {
    /// K-weighted level against the part's own full-range level, dB (0 = nothing lost).
    pub level_db: f64,
    /// Integrated loudness through the device, LUFS (−200: silent).
    pub lufs: f64,
    pub flagged: bool,
}

/// One part's translation.
#[derive(Clone, Debug, PartialEq)]
pub struct PartReport {
    pub part: String,
    /// The part's full-range K-weighted level against the whole mix, dB.
    pub vs_mix_db: Option<f64>,
    /// Its full-range integrated loudness, LUFS.
    pub lufs: f64,
    /// Per device name.
    pub devices: BTreeMap<String, DeviceLevel>,
    /// Energy of (L + R) / 2 against the mean of L² and R², dB.
    pub mono_fold_loss_db: f64,
    /// The devices it is flagged on, and "mono" when the fold loses more than `FLAG_DB`.
    pub flags: Vec<String>,
}

/// A transposed direct form II biquad (normalised: a0 = 1).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Biquad {
    /// From `biquad(type, fc, q, sr)`'s `[b0, b1, b2, a1, a2]`.
    pub fn new(c: [f64; 5]) -> Biquad {
        Biquad {
            b0: c[0],
            b1: c[1],
            b2: c[2],
            a1: c[3],
            a2: c[4],
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// RBJ cookbook low / high shelf, shelf slope 1.
    pub fn shelf(low: bool, hz: f64, db: f64, sr: f64) -> Biquad {
        let a = libm::pow(10.0, db / 40.0);
        let w = 2.0 * core::f64::consts::PI * hz.min(sr * 0.45) / sr;
        let c = libm::cos(w);
        let alpha = libm::sin(w) / 2.0 * libm::sqrt(2.0);
        let sa = 2.0 * libm::sqrt(a) * alpha;
        let s = if low { 1.0 } else { -1.0 }; // the sign that differs between the two
        let a0 = (a + 1.0) + s * (a - 1.0) * c + sa;
        Biquad::new([
            a * ((a + 1.0) - s * (a - 1.0) * c + sa) / a0,
            s * 2.0 * a * ((a - 1.0) - s * (a + 1.0) * c) / a0,
            a * ((a + 1.0) - s * (a - 1.0) * c - sa) / a0,
            -s * 2.0 * ((a - 1.0) + s * (a + 1.0) * c) / a0,
            ((a + 1.0) + s * (a - 1.0) * c - sa) / a0,
        ])
    }

    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// The biquads of `device`, in order.
pub fn filters(device: &Device, sr: f64) -> Vec<Biquad> {
    // Butterworth: 2nd order one section at Q 0.7071, 4th order two at 0.5412 / 1.3066
    // (Swift's literals, kept digit for digit rather than FRAC_1_SQRT_2)
    #[allow(clippy::approx_constant)]
    let qs = |order: u8| -> Vec<f64> {
        if order >= 4 {
            vec![0.541_196, 1.306_563]
        } else {
            vec![0.707_107]
        }
    };
    let mut out = vec![];
    if let Some(hz) = device.highpass_hz {
        out.extend(
            qs(device.highpass_order)
                .into_iter()
                .map(|q| Biquad::new(biquad("highpass", hz, q, sr))),
        );
    }
    if let Some(hz) = device.lowpass_hz {
        out.extend(
            qs(device.lowpass_order)
                .into_iter()
                .map(|q| Biquad::new(biquad("lowpass", hz, q, sr))),
        );
    }
    if let Some((hz, db)) = device.low_shelf {
        out.push(Biquad::shelf(true, hz, db, sr));
    }
    if let Some((hz, db)) = device.high_shelf {
        out.push(Biquad::shelf(false, hz, db, sr));
    }
    out
}

/// `left` / `right` through `device`'s curve.
pub fn apply(device: &Device, left: &[f32], right: &[f32], sr: f64) -> (Vec<f32>, Vec<f32>) {
    let n = left.len().min(right.len());
    let mut l: Vec<f32> = left[..n].to_vec();
    let mut r: Vec<f32> = right[..n].to_vec();
    if device.mono {
        for i in 0..n {
            let m = (l[i] + r[i]) * 0.5;
            l[i] = m;
            r[i] = m;
        }
    }
    for f in filters(device, sr) {
        let (mut fl, mut fr) = (f, f);
        for i in 0..n {
            l[i] = fl.process(l[i] as f64) as f32;
            r[i] = fr.process(r[i] as f64) as f32;
        }
    }
    (l, r)
}

/// K-weighted mean square over both channels, dB (`FLOOR_DB` × 2 for silence).
pub fn k_level_db(left: &[f32], right: &[f32], sr: f64) -> f64 {
    let (mut kl, mut kr) = (KWeighting::new(sr), KWeighting::new(sr));
    let mut sum = 0.0;
    let n = left.len().min(right.len());
    for i in 0..n {
        let a = kl.process(left[i] as f64);
        let b = kr.process(right[i] as f64);
        sum += a * a + b * b;
    }
    if n == 0 || sum <= 0.0 {
        return 2.0 * FLOOR_DB;
    }
    10.0 * libm::log10(sum / n as f64)
}

/// Energy of (L + R) / 2 against the mean of L² and R², dB, clamped to `FLOOR_DB`.
pub fn mono_fold_loss_db(left: &[f32], right: &[f32]) -> f64 {
    let (mut mono, mut stereo) = (0.0, 0.0);
    for i in 0..left.len().min(right.len()) {
        let (l, r) = (left[i] as f64, right[i] as f64);
        mono += (l + r) * (l + r) / 4.0;
        stereo += (l * l + r * r) / 2.0;
    }
    if stereo <= 0.0 {
        return 0.0;
    }
    if mono <= 0.0 {
        return FLOOR_DB;
    }
    (10.0 * libm::log10(mono / stereo)).max(FLOOR_DB)
}

/// One part's report from its stereo render; `mix_db` the whole mix's `k_level_db`.
pub fn analyse(
    part: &str,
    left: &[f32],
    right: &[f32],
    sr: f64,
    mix_db: Option<f64>,
) -> PartReport {
    let full = k_level_db(left, right, sr);
    let mut per = BTreeMap::new();
    let mut flags = vec![];
    for d in DEVICES.iter() {
        let (l, r) = apply(d, left, right, sr);
        let level = (k_level_db(&l, &r, sr) - full).max(FLOOR_DB);
        let flagged = level < -FLAG_DB;
        per.insert(
            String::from(d.name),
            DeviceLevel {
                level_db: level,
                lufs: IntegratedLoudness::lufs(&l, &r, sr),
                flagged,
            },
        );
        if flagged {
            flags.push(String::from(d.name));
        }
    }
    let fold = mono_fold_loss_db(left, right);
    if fold < -FLAG_DB {
        flags.push("mono".into());
    }
    PartReport {
        part: part.into(),
        vs_mix_db: mix_db.map(|m| full - m),
        lufs: IntegratedLoudness::lufs(left, right, sr),
        devices: per,
        mono_fold_loss_db: fold,
        flags,
    }
}

/// Every part of `renders` (`MixBalance::stereo_solos`: "mix" plus the parts), in
/// `MixBalance::PARTS` order; the mix is the reference for `vs_mix_db`.
pub fn report(renders: &BTreeMap<String, (Vec<f32>, Vec<f32>)>, sr: f64) -> Vec<PartReport> {
    let mix = renders.get("mix").map(|(l, r)| k_level_db(l, r, sr));
    let mut names: Vec<String> = MixBalance::PARTS
        .iter()
        .filter(|p| renders.contains_key(**p))
        .map(|p| String::from(*p))
        .collect();
    let mut rest: Vec<String> = renders
        .keys()
        .filter(|k| *k != "mix" && !MixBalance::PARTS.contains(&k.as_str()))
        .cloned()
        .collect();
    rest.sort();
    names.extend(rest);
    names
        .into_iter()
        .map(|name| {
            let (l, r) = &renders[&name];
            analyse(&name, l, r, sr, mix)
        })
        .collect()
}
