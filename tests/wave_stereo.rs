//! `.stereo`: parts placed in the stereo field by a mid/side pan in the runner, measured on
//! renders (linear master, so left + right is exactly twice the mono mix). Swift's
//! `StereoTests`, `SampledWidthTests` and `UnisonTests` (the stereo half) ported (PORTING.md's
//! proof, form (a)); the same renders held against Swift's output sample for sample are the
//! "stereo" cases of `tests/runner_golden.rs`.
//!
//! Swift's whole-track checks run the deck (`WaveDeckSource`, its limiter after the runner):
//! these run the runner into `fx::limiter::PeakLimiter`, which is what the deck puts there.

mod common;
use common::measure::*;
use djrust::fx::limiter::PeakLimiter;
use djrust::musicality::apply;
use djrust::musicality::audio_metrics::OfflineRender;
use djrust::musicality::dj::{sampled_width, SAMPLED_SIDE_HZ};
use djrust::musicality::energy_curve::SideHighpass;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::synth_voice::Unison;
use djrust::pflow::{NetBundle, Project};
use djrust::voices::VoiceCatalog;
use djrust::wave::*;
use djrust::TrackSpec;

const SR: f64 = 48_000.0;

/// A four-on-the-floor kick, off-beat hats and a held pad chord at 120 BPM.
fn kit_and_pad(kick: bool, hats: bool, pad: bool) -> Project {
    let mut k = NetBundle::ring(16);
    for step in [0, 4, 8, 12] {
        k.bind(step, 36, 10, 100, 50);
    }
    k.track.channel = 10;
    k.track.group = Some("drums".into());
    let mut h = NetBundle::ring(16);
    for step in [2, 6, 10, 14] {
        h.bind(step, 42, 10, 90, 50);
    }
    h.track.channel = 10;
    h.track.group = Some("drums".into());
    let mut c = NetBundle::ring(16);
    for (v, note) in [60, 64, 67].into_iter().enumerate() {
        c.bind(v as i64, note, 7, 90, 1950);
    }
    c.track.channel = 7;
    c.track.group = Some("harmony".into());
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    if kick {
        p.nets.insert("kick", k);
    }
    if hats {
        p.nets.insert("hihat", h);
    }
    if pad {
        p.nets.insert("harmony", c);
    }
    p
}

fn linear(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        ..Default::default()
    }
}

/// Two seconds, stereo (or mono with `right: false`), linear master.
fn render(p: &Project, fixes: Fixes, right: bool) -> (Vec<f32>, Vec<f32>) {
    if right {
        render_stereo(p, &linear(fixes), SR, 2.0)
    } else {
        (render_mono(p, &linear(fixes), SR, 2.0), Vec::new())
    }
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| *v as f64 * *v as f64).sum()
}

/// StereoTests.partsSitWhereTheyBelong: the kick stays dead centre, hats lean one way, the pad's
/// notes spread both ways; the mono sum is the mono mix.
#[test]
fn parts_sit_where_they_belong() {
    let kick = render(&kit_and_pad(true, false, false), Fixes::STEREO, true);
    assert!(identical(&kick.0, &kick.1), "kick in the middle");
    let hats = render(&kit_and_pad(false, true, false), Fixes::STEREO, true);
    assert!(energy(&hats.0) > 2.0 * energy(&hats.1), "hats to a side");
    let pad = render(&kit_and_pad(false, false, true), Fixes::STEREO, true);
    let side: Vec<f32> = pad
        .0
        .iter()
        .zip(&pad.1)
        .map(|(l, r)| (l - r) / 2.0)
        .collect();
    let mid: Vec<f32> = pad
        .0
        .iter()
        .zip(&pad.1)
        .map(|(l, r)| (l + r) / 2.0)
        .collect();
    assert!(
        amp_db(libm::sqrt(energy(&side) / energy(&mid))) > -20.0,
        "the pad is wide"
    );
    let all = render(&kit_and_pad(true, true, true), Fixes::STEREO, true);
    let mono = render(&kit_and_pad(true, true, true), Fixes::STEREO, false).0;
    let mut worst = 0.0f64;
    for (i, m) in mono.iter().enumerate() {
        worst = worst.max(((all.0[i] as f64 + all.1[i] as f64) / 2.0 - *m as f64).abs());
    }
    assert!(worst < 1e-6, "mono sum = mono mix (worst {worst})");
}

/// StereoTests.offIsMono: without `.stereo` both channels carry the same signal, exactly as
/// before.
#[test]
fn off_is_mono() {
    let a = render(&kit_and_pad(true, true, true), Fixes::EMPTY, true);
    assert!(identical(&a.0, &a.1));
    assert!(identical(
        &a.0,
        &render(&kit_and_pad(true, true, true), Fixes::EMPTY, false).0
    ));
}

/// StereoTests.samplerLowsGoMono: the sampler bus's side signal loses its lows (a sine in one
/// channel only: its side is more than 20 dB down at 60 Hz, within 1 dB at 2 kHz) and its mid
/// is untouched.
#[test]
fn sampler_lows_go_mono() {
    for (hz, lo, hi) in [(60.0, -200.0, -20.0), (2000.0, -1.0, 1.0)] {
        let mut f = SideHighpass::new(150.0, SR);
        let (mut s_in, mut s_out, mut worst_mid) = (0.0, 0.0, 0.0f64);
        for i in 0..SR as usize {
            let mut l = (0.5 * libm::sin(2.0 * core::f64::consts::PI * hz * i as f64 / SR)) as f32;
            let mut r = 0.0f32;
            let mid = (l as f64 + r as f64) / 2.0;
            let side = (l as f64 - r as f64) / 2.0;
            f.process(&mut l, &mut r);
            if i <= (SR / 4.0) as usize {
                continue; // settled
            }
            s_in += side * side;
            let out = (l as f64 - r as f64) / 2.0;
            s_out += out * out;
            worst_mid = worst_mid.max(((l as f64 + r as f64) / 2.0 - mid).abs());
        }
        let db = 10.0 * libm::log10(s_out / s_in);
        assert!(db > lo && db < hi, "{hz} Hz: side {db} dB");
        assert!(worst_mid < 1e-6);
    }
}

/// The deck's output for `genre` seed 42: twelve seconds of the minimal track through the
/// runner (the deck's options, by genre) and the peak limiter the deck ends in.
fn deck_metrics(
    genre: &str,
    without: Fixes,
) -> (djrust::musicality::audio_metrics::AudioMetrics, usize) {
    let sr = 48_000.0;
    let spec = TrackSpec::new(genre, 42.0, Some("minimal"));
    let played = apply(&spec.project(), &spec, Fixes::ALL);
    let o = WaveOptions {
        instruments: Some(InstrumentTable::shared()),
        musicality: RUNNER_FIXES & !without,
        ..WaveOptions::extended()
    }
    .for_genre(genre);
    let mut r = OfflineRender::new(&played, &o, sr, 12.0);
    let mut lim = PeakLimiter::new(sr);
    let n = r.left.len();
    let mut off = 0;
    while off < n {
        let m = 512.min(n - off);
        lim.process(
            &mut r.left[off..off + m],
            Some(&mut r.right[off..off + m]),
            m,
        );
        off += m;
    }
    let clipped = r
        .left
        .iter()
        .chain(&r.right)
        .filter(|v| v.abs() >= 1.0)
        .count();
    (r.metrics(), clipped)
}

/// The fixes this runner plays (see `wave_balance.rs`).
const RUNNER_FIXES: Fixes = Fixes(
    Fixes::SPACE.0
        | Fixes::SHIMMER.0
        | Fixes::ECHO.0
        | Fixes::DUCK.0
        | Fixes::ENERGY.0
        | Fixes::SWEEP.0
        | Fixes::SUBTRACTIVE.0
        | Fixes::UNISON.0
        | Fixes::SUB_OCTAVE.0
        | Fixes::BALANCE.0
        | Fixes::STEREO.0,
);

/// StereoTests.aTrackIsWideAboveAndMonoBelow: a whole track through the deck's options: a wide
/// mix (side 5-25 dB under mid) whose low end is mono (side more than 30 dB under mid below
/// 150 Hz), pads reaching down to C3 included (lofi, jazz).
#[test]
fn a_track_is_wide_above_and_mono_below() {
    for genre in ["trance", "lofi", "jazz"] {
        let (m, clipped) = deck_metrics(genre, Fixes::LAYER_BUDGET);
        eprintln!(
            "{genre}: side {:.1} dB, low side {:.1} dB",
            m.side_db, m.low_side_db
        );
        assert!(
            m.side_db > -25.0 && m.side_db < -5.0,
            "{genre}: side {} dB",
            m.side_db
        );
        assert!(
            m.low_side_db < -30.0,
            "{genre}: low side {} dB",
            m.low_side_db
        );
        assert_eq!(clipped, 0);
    }
}

/// StereoTests.ambientPadsKeepTheirLowsMono: ambient's pads sit low and long: narrower
/// (`VoiceDesign::pad_width`) and with the unison pair's side highpassed higher (`pad_side_hz`),
/// the low end stays mono (side 35 dB or more under mid below 150 Hz, where full width left it
/// at -26) while the mix is still wide.
#[test]
fn ambient_pads_keep_their_lows_mono() {
    let design = WaveOptions {
        musicality: RUNNER_FIXES,
        ..WaveOptions::extended()
    }
    .for_genre("ambient")
    .voice_design;
    assert!(design.pad_width < 1.0 && design.pad_side_hz > Unison::SIDE_HZ);
    let (m, clipped) = deck_metrics("ambient", Fixes::EMPTY);
    eprintln!(
        "ambient: side {:.1} dB, low side {:.1} dB",
        m.side_db, m.low_side_db
    );
    assert!(
        m.low_side_db <= -35.0,
        "ambient: low side {} dB",
        m.low_side_db
    );
    assert!(m.side_db >= -20.0, "ambient: side {} dB", m.side_db);
    assert_eq!(clipped, 0);
}

// ---- unison, wide -------------------------------------------------------------------------

/// UnisonTests.aPadNoteIsWideAndFoldsToMono: with `.stereo` the pair spreads: a held pad note was
/// one point in the field (left and right perfectly correlated), now it is wide; it folds to mono
/// without cancelling, and a low pad note stays in the middle.
#[test]
fn a_pad_note_is_wide_and_folds_to_mono() {
    let stereo = |p: &Project, fixes: Fixes| render_stereo(p, &linear(fixes), SR, 1.5);
    let high = one_note("harmony", 72, 100, 16, 7, "");
    let before = stereo(&high, Fixes::STEREO);
    let wide = stereo(&high, Fixes::STEREO | Fixes::UNISON);
    let (a, b) = ((0.2 * SR) as usize, (1.2 * SR) as usize);
    let c_before = correlation(&before.0, &before.1, a, b);
    let c_after = correlation(&wide.0, &wide.1, a, b);
    assert!(c_before > 0.999, "before: L/R correlation {c_before}");
    assert!(c_after < 0.9, "after: L/R correlation {c_after}");
    let mono: Vec<f32> = wide
        .0
        .iter()
        .zip(&wide.1)
        .map(|(l, r)| (l + r) / 2.0)
        .collect();
    let stereo_power = (rms_db(&wide.0) + rms_db(&wide.1)) / 2.0;
    assert!(
        rms_db(&mono) > stereo_power - 1.5,
        "the mono fold lost level"
    );
    let low = one_note("harmony", 48, 100, 16, 7, "");
    let l = stereo(&low, Fixes::STEREO | Fixes::UNISON);
    let low_corr = correlation(&l.0, &l.1, a, b);
    assert!(
        low_corr > 0.999,
        "a C3 pad note: L/R correlation {low_corr}"
    );
}

/// UnisonTests.whoGetsAPair (the stereo half): saw leads get the pair, mono (they are filtered).
#[test]
fn a_filtered_lead_pair_stays_mono() {
    let lead = one_note("melody", 69, 100, 8, 6, "");
    let s = render_stereo(
        &lead,
        &linear(Fixes::STEREO | Fixes::UNISON | Fixes::SUBTRACTIVE),
        SR,
        1.5,
    );
    assert!(identical(&s.0, &s.1));
}

// ---- the lanes' placement and the sampled slots' widths ----------------------------------

/// `buildLanes`: a lane's pan is its placement's, a tonal lane's voices spread either side where
/// the placement has a spread and nothing on the voice sum is in the way.
#[test]
fn lanes_carry_their_placement() {
    let p = kit_and_pad(true, true, true);
    let w = WaveRunner::prepare(&p, SR, &linear(Fixes::STEREO), 0);
    let (kick_pan, kick_spread) = w.lanes_of("kick")[0].placement();
    assert_eq!((kick_pan, kick_spread), (0.0, false));
    let (hat_pan, hat_spread) = w.lanes_of("hihat")[0].placement();
    assert!(hat_pan != 0.0 && !hat_spread);
    let (_, pad_spread) = w.lanes_of("harmony")[0].placement();
    assert!(pad_spread, "the pad's voices spread");
    // off: everything in the middle
    let off = WaveRunner::prepare(&p, SR, &linear(Fixes::EMPTY), 0);
    assert!(off.lanes().iter().all(|l| l.placement() == (0.0, false)));
    // a unison pair is wide only under `.stereo`, and only unfiltered
    let pad = one_note("harmony", 60, 100, 8, 7, "");
    let on = WaveRunner::prepare(&pad, SR, &linear(Fixes::UNISON | Fixes::STEREO), 0);
    assert!(on.lanes_of("harmony")[0].unison_wide());
    let flat = WaveRunner::prepare(&pad, SR, &linear(Fixes::UNISON), 0);
    assert!(!flat.lanes_of("harmony")[0].unison_wide());
}

fn bind_slot_3(_instrument: &str, _part: &str) -> Option<usize> {
    Some(3)
}

/// SampledWidthTests.slotsCarryTheirWidth: a prepared track's sampled slots carry their
/// instrument's width and side highpass (the host reads them); none without `.stereo`.
#[test]
fn slots_carry_their_width() {
    let spec = TrackSpec::new("country", 7.0, Some("minimal"));
    let p = spec.project();
    let o = |fixes: Fixes| WaveOptions {
        musicality: fixes,
        instruments: Some(InstrumentTable::shared()),
        sample_binder: Some(bind_slot_3 as runner::SampleBinder),
        ..WaveOptions::extended()
    };
    let w = WaveRunner::prepare(&p, SR, &o(Fixes::STEREO), 0);
    // every tonal lane binds slot 3; the first lane that plays it names its width
    let first = w
        .lanes()
        .iter()
        .find(|l| l.is_tonal() && l.sample_slot() == Some(3) && l.net_id != "<live>")
        .expect("a sampled lane");
    let group = &w.sampled_groups[&first.net_id];
    let category = VoiceCatalog::bundled()
        .category_of
        .get(&first.instrument)
        .cloned();
    let want = sampled_width(category.as_deref(), group);
    assert_eq!(w.slot_width(3), want.width as f32);
    assert_eq!(w.side_slots() >> 3 & 1, want.side_hz.is_some() as u64);
    assert_eq!(
        w.width_slots() >> 3 & 1,
        ((want.width - 1.0).abs() > 1e-4 || want.side_hz.is_some()) as u64
    );
    let _ = SAMPLED_SIDE_HZ;
    let flat = WaveRunner::prepare(&p, SR, &o(Fixes::EMPTY), 0);
    assert_eq!((flat.width_slots(), flat.side_slots()), (0, 0));
    // no bank (no binder): no widths either
    let bare = WaveRunner::prepare(
        &p,
        SR,
        &WaveOptions {
            sample_binder: None,
            ..o(Fixes::STEREO)
        },
        0,
    );
    assert_eq!((bare.width_slots(), bare.side_slots()), (0, 0));
}
