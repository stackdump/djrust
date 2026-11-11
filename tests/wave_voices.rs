//! `.subtractive` (band-limited saw / square tables and a filter envelope per voice), `.unison`
//! (pads and saw leads as detuned pairs) and `.sweep` (a buildup opens those filters), measured
//! on renders of one-note projects through a linear master. Swift's `SynthVoiceTests`
//! (`SynthVoiceTests`, `UnisonTests`) and `SweepTests` ported (PORTING.md's proof, form (a)); the
//! same voices held against Swift's output sample for sample are the "voices" cases of
//! `tests/runner_golden.rs`.

mod common;
use common::measure::*;
use djrust::musicality::apply;
use djrust::musicality::energy_curve::{BuildupSweep, EnergyCurve};
use djrust::musicality::fixes::Fixes;
use djrust::musicality::mix_balance::MixBalance;
use djrust::musicality::synth_voice::{FilterEnvelope, Unison, VoiceDesign};
use djrust::pflow::Project;
use djrust::wave::mip::MipTables;
use djrust::wave::synth::{tonal_voice, WAVE_TABLE_SIZE};
use djrust::wave::*;
use djrust::TrackSpec;
use std::collections::BTreeSet;

const SR: f64 = 48_000.0;

fn secs(s: f64) -> usize {
    (s * SR) as usize
}

fn linear(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        ..Default::default()
    }
}

fn render(p: &Project, fixes: Fixes, seconds: f64) -> Vec<f32> {
    render_mono(p, &linear(fixes), SR, seconds)
}

/// Harmonic `h` of `f0` against the fundamental, dB, in the window `a..b` seconds.
fn harmonic(x: &[f32], f0: f64, h: i64, a: f64, b: f64) -> f64 {
    let w = &x[secs(a)..secs(b)];
    tone_db(w, f0 * h as f64, SR) - tone_db(w, f0, SR)
}

/// SynthVoiceTests.aBassNoteHasHarmonicsThatCloseAway: a bass note (A1, 55 Hz) used to stop at
/// its 8th harmonic (440 Hz); now its upper harmonics are there on the attack, and they close
/// away as the note plays, where before the timbre never moved.
#[test]
fn a_bass_note_has_harmonics_that_close_away() {
    let f0 = midi_hz(33);
    let p = one_note("bass", 33, 100, 8, 6, "");
    let before = render(&p, Fixes::EMPTY, 1.2);
    let after = render(&p, Fixes::SUBTRACTIVE, 1.2);
    // the 12th harmonic (660 Hz) at the attack
    let h12_before = harmonic(&before, f0, 12, 0.02, 0.10);
    let h12_after = harmonic(&after, f0, 12, 0.02, 0.10);
    assert!(h12_before < -60.0, "before: h12 {h12_before} dB");
    assert!(h12_after > -40.0, "after: h12 {h12_after} dB");
    // the 8th harmonic (440 Hz), attack against 0.5 s in
    let fall_before = harmonic(&before, f0, 8, 0.02, 0.10) - harmonic(&before, f0, 8, 0.5, 0.58);
    let fall_after = harmonic(&after, f0, 8, 0.02, 0.10) - harmonic(&after, f0, 8, 0.5, 0.58);
    assert!(fall_before.abs() < 2.0, "before: h8 moved {fall_before} dB");
    assert!(fall_after > 10.0, "after: h8 fell {fall_after} dB");
}

/// SynthVoiceTests.harderNotesAreBrighter: an accent sounds like one: a lead note struck harder
/// is brighter, not only louder.
#[test]
fn harder_notes_are_brighter() {
    let f0 = midi_hz(69);
    let brightness = |v: i64, fixes: Fixes| {
        harmonic(
            &render(&one_note("melody", 69, v, 4, 6, ""), fixes, 1.2),
            f0,
            7,
            0.01,
            0.06,
        )
    };
    let flat = brightness(120, Fixes::EMPTY) - brightness(50, Fixes::EMPTY);
    let shaped = brightness(120, Fixes::SUBTRACTIVE) - brightness(50, Fixes::SUBTRACTIVE);
    assert!(flat.abs() < 0.5, "before: {flat} dB");
    assert!(
        shaped > 3.0,
        "after: the 7th harmonic {shaped} dB stronger at velocity 120 than 50"
    );
}

/// SynthVoiceTests.tablesStayUnderNyquist: every table only holds partials its highest note can
/// play under Nyquist (no aliasing), and the low tables hold far more than beats' 8.
#[test]
fn tables_stay_under_nyquist() {
    let t = MipTables::new("saw", SR);
    let size = WAVE_TABLE_SIZE;
    let partial =
        |k: usize, n: usize| cycle_harmonic(&t.samples[k * (size + 1)..k * (size + 1) + size], n);
    for k in 0..t.top.len() {
        if t.top[k] < SR / 2.0 {
            let limit = (0.5 * SR / t.top[k]) as usize;
            if limit + 1 < size / 2 {
                assert!(
                    partial(k, limit + 1) < 1e-4,
                    "table {k} holds partial {}",
                    limit + 1
                );
            }
        }
    }
    assert!(partial(0, 100) > 1e-3); // 20..40 Hz: up to 128 partials
    assert!(t.top[0] <= 40.0 && *t.top.last().unwrap() >= 20_000.0);
}

/// SynthVoiceTests.otherVoicesAreUntouched: pads (the soft table), strings (Karplus-Strong) and
/// drums play exactly as before.
#[test]
fn other_voices_are_untouched() {
    let pad = one_note("harmony", 60, 100, 8, 7, "");
    assert!(identical(
        &render(&pad, Fixes::EMPTY, 1.2),
        &render(&pad, Fixes::SUBTRACTIVE, 1.2)
    ));
    let guitar = one_note("melody", 64, 100, 4, 6, "acoustic-guitar");
    let table = || WaveOptions {
        instruments: Some(InstrumentTable::shared()),
        ..linear(Fixes::EMPTY)
    };
    let shaped = WaveOptions {
        musicality: Fixes::SUBTRACTIVE,
        ..table()
    };
    assert_eq!(
        FilterEnvelope::for_voice(
            &InstrumentTable::shared().voice(Some("acoustic-guitar"), Some("melody")),
            "melody",
            &BTreeSet::new()
        ),
        None
    );
    assert!(identical(
        &render_mono(&guitar, &table(), SR, 1.2),
        &render_mono(&guitar, &shaped, SR, 1.2)
    ));
    let kick = one_note("drums", 36, 100, 1, 10, "");
    assert!(identical(
        &render(&kick, Fixes::EMPTY, 1.2),
        &render(&kick, Fixes::SUBTRACTIVE, 1.2)
    ));
    let none = BTreeSet::new();
    assert_eq!(
        FilterEnvelope::for_voice(&tonal_voice(Some(""), Some("bass")), "bass", &none),
        Some(FilterEnvelope::BASS)
    );
    assert_eq!(
        FilterEnvelope::for_voice(&tonal_voice(Some("piano"), Some("melody")), "melody", &none),
        Some(FilterEnvelope::PLUCK)
    );
}

/// SynthVoiceTests.trancesArpIsPlucked: trance's arp takes the pluck envelope
/// (`VoiceDesign::for_genre`, through `for_genre`); a house arp, and beats' options for trance
/// (parity), keep the lead's.
#[test]
fn trances_arp_is_plucked() {
    let arp = tonal_voice(Some(""), Some("arp"));
    let shaped = WaveOptions {
        musicality: Fixes::SUBTRACTIVE,
        ..WaveOptions::extended()
    };
    let only_arp: BTreeSet<String> = ["arp".to_string()].into_iter().collect();
    for genre in ["trance", "edm", "synthwave"] {
        let design = shaped.for_genre(genre).voice_design;
        assert_eq!(design.pluck_groups, only_arp, "{genre}");
        if FilterEnvelope::for_voice(&arp, "arp", &BTreeSet::new()).is_some() {
            assert_eq!(
                FilterEnvelope::for_voice(&arp, "arp", &design.pluck_groups),
                Some(FilterEnvelope::PLUCK)
            );
        }
    }
    assert_eq!(
        shaped.for_genre("house").voice_design,
        VoiceDesign::default()
    );
    assert_eq!(
        WaveOptions::default().for_genre("trance").voice_design,
        VoiceDesign::default()
    );
    assert_ne!(
        FilterEnvelope::for_voice(&arp, "melody", &only_arp),
        Some(FilterEnvelope::PLUCK)
    );
    // a whole trance track: every shaped arp lane closes at the pluck's rate. Trance's own arp
    // instruments all have voices of their own in the instrument table (Karplus-Strong or a set
    // lowpass), which no filter envelope shapes: no seed has a shaped arp lane. So the arp plays
    // a plain saw the table doesn't know (a user's instrument pick), which `.subtractive`
    // shapes as a lead unless the arp is plucked.
    let spec = TrackSpec::new("trance", 42.0, Some("minimal"));
    let mut p = spec.project();
    let arp_ids: Vec<String> = p
        .nets
        .keys()
        .to_vec()
        .into_iter()
        .filter(|id| p.nets.get(id).unwrap().track.group.as_deref() == Some("arp"))
        .collect();
    for id in &arp_ids {
        p.nets.get_mut(id).unwrap().track.instrument = "supersaw".into();
    }
    assert!(!InstrumentTable::shared().entries.contains_key("supersaw"));
    let o = shaped.for_genre("trance");
    let w = WaveRunner::prepare(&p, 22_050.0, &o, 0);
    let mut shaped_lanes = 0;
    for id in &arp_ids {
        for lane in w.lanes_of(id) {
            let (amount, base, fall) = lane.filter_envelope();
            if !lane.is_tonal() || amount == 0.0 {
                continue;
            }
            shaped_lanes += 1;
            let pluck = FilterEnvelope::PLUCK;
            assert_eq!(base, pluck.base, "{id}");
            assert_eq!(amount, pluck.amount, "{id}");
            assert_eq!(fall, libm::exp(-1.0 / (pluck.seconds * 22_050.0)), "{id}");
        }
    }
    assert!(
        shaped_lanes > 0,
        "no shaped arp lane (arp nets {arp_ids:?})"
    );
}

// ---- unison ------------------------------------------------------------------------------

/// UnisonTests.aPadNoteIsADetunedPair: a held pad note is two oscillators 7 cents either side:
/// its 4th harmonic (C5: 2093 Hz) splits into two partials 17 Hz apart, where it was one.
#[test]
fn a_pad_note_is_a_detuned_pair() {
    let p = one_note("harmony", 72, 100, 16, 7, "");
    let h4 = midi_hz(72) * 4.0;
    let split = |x: &[f32]| {
        let w = &x[secs(0.2)..secs(1.2)];
        tone_db(w, h4 * libm::pow(2.0, 7.0 / 1200.0), SR) - tone_db(w, h4, SR)
    };
    let before = split(&render(&p, Fixes::EMPTY, 1.3));
    let after = split(&render(&p, Fixes::UNISON, 1.3));
    assert!(
        before < -10.0,
        "before: +7 cents is {before} dB against the note"
    );
    assert!(
        after > 3.0,
        "after: +7 cents is {after} dB against the note"
    );
    // equal power: as loud as one oscillator
    let level = rms_db(&render(&p, Fixes::UNISON, 1.3)) - rms_db(&render(&p, Fixes::EMPTY, 1.3));
    assert!(level.abs() < 1.5, "level moved {level} dB");
}

/// UnisonTests.whoGetsAPair (the mono half): basses and plucks keep one oscillator; saw leads
/// get the pair (the stereo half is in `wave_stereo.rs`).
#[test]
fn who_gets_a_pair() {
    let bass = one_note("bass", 33, 100, 8, 6, "");
    assert!(identical(
        &render(&bass, Fixes::EMPTY, 1.2),
        &render(&bass, Fixes::UNISON, 1.2)
    ));
    let cents = |instrument: &str, group: &str| {
        Unison::cents(&tonal_voice(Some(instrument), Some(group)), group)
    };
    assert_eq!(cents("", "melody"), Some(6.0));
    assert_eq!(cents("piano", "melody"), None);
    assert_eq!(cents("", "harmony"), Some(7.0));
    assert_eq!(cents("", "bass"), None);
    // the lanes themselves: a pad and a saw lead hold a pair, a bass does not
    let o = linear(Fixes::UNISON);
    let pad = WaveRunner::prepare(&one_note("harmony", 60, 100, 8, 7, ""), SR, &o, 0);
    let (down, up) = pad.lanes_of("harmony")[0].unison_pair().unwrap();
    assert!((down - libm::pow(2.0, -7.0 / 1200.0)).abs() < 1e-15);
    assert!((up - libm::pow(2.0, 7.0 / 1200.0)).abs() < 1e-15);
    let b = WaveRunner::prepare(&one_note("bass", 33, 100, 8, 6, ""), SR, &o, 0);
    assert!(b.lanes_of("bass")[0].unison_pair().is_none());
}

// ---- sweep -------------------------------------------------------------------------------

fn played(genre: &str) -> Project {
    let spec = TrackSpec::new(genre, 42.0, Some("standard"));
    apply(&spec.project(), &spec, Fixes::ALL)
}

/// SweepTests.sweepIsRegistered
#[test]
fn sweep_is_registered() {
    assert!(Fixes::ALL.contains(Fixes::SWEEP));
    assert_eq!(Fixes::named("sweep"), Some(Fixes::SWEEP));
    assert!(
        !WaveOptions::default().musicality.contains(Fixes::SWEEP),
        "parity stays beats-exact"
    );
}

/// SweepTests.rampProgressRunsThroughTheBuildupOnly: 0 on a buildup's first step, rising to just
/// under 1 on its last, nil in every other section.
#[test]
fn ramp_progress_runs_through_the_buildup_only() {
    let p = played("trance");
    let c = EnergyCurve::new(&p).expect("a curve");
    let b = c.ramp.iter().position(|r| *r).expect("a buildup");
    let (s0, s1) = (c.start[b], c.start[b + 1]);
    assert_eq!(c.ramp_progress(s0), Some(0.0));
    let last = c.ramp_progress(s1 - 1).unwrap();
    assert!(last > 0.9 && last < 1.0);
    assert!(c.ramp_progress((s0 + s1) / 2).unwrap() < last);
    assert_eq!(c.ramp_progress(s1), None);
    assert!(c.ramp_progress((s0 - 1).max(0)).is_none() || b == 0);
    for (k, r) in c.ramp.iter().enumerate() {
        if !*r {
            assert_eq!(c.ramp_progress(c.start[k]), None, "section {k}");
        }
    }
}

/// SweepTests.sweepShapesBaseAndQ
#[test]
fn sweep_shapes_base_and_q() {
    assert!((BuildupSweep::base(2.5, 0.0) - 1.5).abs() < 1e-12);
    assert!((BuildupSweep::base(2.5, 1.0) - 5.0).abs() < 1e-12);
    assert!(BuildupSweep::base(2.5, 0.5) > 1.5 && BuildupSweep::base(2.5, 0.5) < 5.0);
    assert_eq!(BuildupSweep::q(0.8, 0.0), 0.8);
    assert!((BuildupSweep::q(0.8, 1.0) - 2.5).abs() < 1e-12);
    assert!(
        BuildupSweep::q(0.8, 0.5) < (0.8 + 2.5) / 2.0,
        "accelerating"
    );
    assert_eq!(
        BuildupSweep::q(3.0, 1.0),
        3.0,
        "never below the voice's own"
    );
}

const SWEEP_SR: f64 = 22_050.0;

/// The song from its start, mono, synth voices, for `steps` steps.
fn render_steps(p: &Project, o: &WaveOptions, steps: i64) -> Vec<f32> {
    let mut r = WaveRunner::new(SWEEP_SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SWEEP_SR, o, 0));
    r.play();
    let mut out = vec![0f32; (steps as f64 * SWEEP_SR * 15.0 / p.tempo) as usize + 512];
    let mut off = 0;
    while off + 512 <= out.len() && !r.stopped() {
        r.process(&mut out[off..off + 512], None, 512);
        off += 512;
    }
    out
}

/// What the app plays with `.sweep` on or off, everything measured at prepare pinned so the two
/// differ by the sweep alone (this port's runner has no `.loudness` to pin).
fn sweep_options(p: &Project, sweep: bool) -> WaveOptions {
    let mut fixes = Fixes::SUBTRACTIVE | Fixes::UNISON | Fixes::ENERGY | Fixes::BALANCE;
    if sweep {
        fixes |= Fixes::SWEEP;
    }
    WaveOptions {
        master: Master::Limit,
        instruments: Some(InstrumentTable::shared()),
        musicality: fixes,
        track_balance: Some(MixBalance::trims(p, Fixes::EMPTY, None, None)),
        track_energy: EnergyCurve::new(p),
        ..Default::default()
    }
}

/// SweepTests.rendersOutsideBuildupsAreBitIdentical: everything before the first buildup renders
/// bit-identical with and without the sweep; the buildup itself is not.
#[test]
fn renders_outside_buildups_are_bit_identical() {
    let p = played("trance");
    let c = EnergyCurve::new(&p).expect("a curve");
    let b = c.ramp.iter().position(|r| *r).expect("a buildup");
    let before = c.start[b];
    assert!(before >= 64);
    let on = render_steps(&p, &sweep_options(&p, true), before + 16);
    let off = render_steps(&p, &sweep_options(&p, false), before + 16);
    let edge = (before as f64 * SWEEP_SR * 15.0 / p.tempo) as usize - 512;
    assert!(identical(&on[..edge], &off[..edge]));
    // and the buildup itself is not
    assert!(!identical(&on[edge..], &off[edge..]));
}

/// Spectral centroid (Hz) of `x[a..b]`: Hann-windowed 1024-sample frames, a naive DFT up to
/// 5.5 kHz (the bins a filter base moves), power-weighted.
fn centroid(x: &[f32], a: usize, b: usize) -> f64 {
    let size = 1024usize;
    let bins = (5_500.0 / (SWEEP_SR / size as f64)) as usize;
    let tau = 2.0 * core::f64::consts::PI;
    let window: Vec<f64> = (0..size)
        .map(|i| 0.5 - 0.5 * libm::cos(tau * i as f64 / size as f64))
        .collect();
    let cos_t: Vec<f64> = (0..size)
        .map(|i| libm::cos(tau * i as f64 / size as f64))
        .collect();
    let sin_t: Vec<f64> = (0..size)
        .map(|i| libm::sin(tau * i as f64 / size as f64))
        .collect();
    let (mut num, mut den) = (0.0, 0.0);
    let mut f = a;
    let mut frame = vec![0.0; size];
    while f + size <= b {
        for (i, v) in frame.iter_mut().enumerate() {
            *v = x[f + i] as f64 * window[i];
        }
        for k in 1..bins {
            let (mut re, mut im) = (0.0, 0.0);
            let mut j = 0;
            for v in &frame {
                re += v * cos_t[j];
                im += v * sin_t[j];
                j += k;
                if j >= size {
                    j -= size;
                }
            }
            let p = re * re + im * im;
            num += p * k as f64 * SWEEP_SR / size as f64;
            den += p;
        }
        f += size * 2;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// SweepTests.buildupBrightensItsSynthVoices: a soloed subtractive part (no `.energy` lowpass,
/// so the brightening is the sweep's): its buildup's last bar has a centroid >= 1.5x its first;
/// without the sweep it stays put. The extra resonance adds no more than 1 dB to the full mix's
/// peak.
#[test]
fn a_buildup_brightens_its_synth_voices() {
    for genre in ["trance", "techno"] {
        let p = played(genre);
        let c = EnergyCurve::new(&p).expect("a curve");
        let b = c.ramp.iter().position(|r| *r).expect("a buildup");
        let (s0, s1) = (c.start[b], c.start[b + 1]);
        assert!(s1 - s0 >= 32);
        let base = WaveOptions {
            master: Master::Limit,
            instruments: Some(InstrumentTable::shared()),
            musicality: Fixes::SUBTRACTIVE | Fixes::UNISON | Fixes::SWEEP,
            track_energy: Some(c.clone()),
            ..Default::default()
        };
        // the first part whose lanes the subtractive envelope shapes
        let mut part = None;
        for name in ["bass", "lead", "arp"] {
            let solo = MixBalance::solo(&p, name);
            let prepared = WaveRunner::prepare(&solo, SWEEP_SR, &base, 0);
            if prepared
                .lanes()
                .iter()
                .any(|l| l.filter_envelope().0 > 0.0 && l.sample_slot().is_none())
            {
                part = Some(solo);
                break;
            }
        }
        let solo = part.unwrap_or_else(|| panic!("no subtractive part in {genre}"));
        let off = WaveOptions {
            musicality: base.musicality & !Fixes::SWEEP,
            ..base.clone()
        };
        let on = render_steps(&solo, &base, s1);
        let flat = render_steps(&solo, &off, s1);
        let per_step = SWEEP_SR * 15.0 / p.tempo;
        let first = (
            (s0 as f64 * per_step) as usize,
            ((s0 + 16) as f64 * per_step) as usize,
        );
        let last = (
            ((s1 - 16) as f64 * per_step) as usize,
            (s1 as f64 * per_step) as usize,
        );
        let ratio = centroid(&on, last.0, last.1) / centroid(&on, first.0, first.1).max(1.0);
        let flat_ratio =
            centroid(&flat, last.0, last.1) / centroid(&flat, first.0, first.1).max(1.0);
        eprintln!("{genre}: centroid last/first {ratio} with the sweep, {flat_ratio} without");
        assert!(ratio >= 1.5, "{genre}: {ratio}");
        assert!(ratio > flat_ratio);
        // the full mix's peak with the extra resonance, against the same mix without the sweep
        let mix = render_steps(&p, &sweep_options(&p, true), s1 + 16);
        let stock = render_steps(&p, &sweep_options(&p, false), s1 + 16);
        let peak = mix.iter().fold(0f32, |m, v| m.max(v.abs()));
        let stock_peak = stock.iter().fold(0f32, |m, v| m.max(v.abs()));
        eprintln!(
            "{genre}: mix peak {} dBFS, {} without",
            amp_db(peak as f64),
            amp_db(stock_peak as f64)
        );
        assert!(
            amp_db(peak as f64) <= amp_db(stock_peak as f64) + 1.0,
            "{genre}: peak {} dBFS, {} without the sweep",
            amp_db(peak as f64),
            amp_db(stock_peak as f64)
        );
    }
}

// ---- kick tune ---------------------------------------------------------------------------

/// KickTuneTests.theKickRingsInTheKey: the kick's tail lands on the key: for a track in A it rings
/// at A1 (55 Hz), in C at G1 (the fifth, 49 Hz: C1 is too low, C2 too high), in D at A1 too; it was
/// 48 Hz in every key.
#[test]
fn the_kick_rings_in_the_key() {
    use djrust::musicality::synth_voice::KickTuning;
    for (root, expected) in [(57, 55.0), (60, 49.0), (65, 43.65), (62, 55.0)] {
        let mut p = one_note("drums", 36, 100, 1, 10, "");
        p.root_note = Some(root);
        let hz = KickTuning::hz(Some(root)).unwrap();
        let tail = |x: &[f32], near: f64| partial_peak(&x[secs(0.2)..secs(0.45)], near, SR);
        let before = tail(&render(&p, Fixes::EMPTY, 1.2), 48.0);
        let after = tail(&render(&p, Fixes::KICK_TUNE, 1.2), hz);
        assert!(cents(before, 48.0).abs() < 15.0, "before: {before} Hz");
        assert!(
            cents(after, hz).abs() < 15.0,
            "after: {after} Hz, tuned to {hz}"
        );
        // the root or the fifth, an octave either side of 50 Hz
        let pc = (libm::round(12.0 * libm::log2(hz / 440.0)) as i64 + 69).rem_euclid(12);
        assert!(pc == root % 12 || pc == (root + 7) % 12);
        assert!(hz > 35.0 && hz < 75.0);
        assert!(cents(hz, expected).abs() < 1.0, "{hz}");
    }
}

/// KickTuneTests.noKeyNoChange: without a key (a raw project) the kick stays as beats has it.
#[test]
fn no_key_no_change() {
    let p = one_note("drums", 36, 100, 1, 10, "");
    assert!(p.root_note.is_none());
    assert!(identical(
        &render(&p, Fixes::EMPTY, 1.2),
        &render(&p, Fixes::KICK_TUNE, 1.2)
    ));
}
