//! `.energy`: the composer's per-section energy and filter, which beats' wave engine drops,
//! played as a level and a lowpass on the tonal voices (`EnergyCurve`, applied by
//! `WaveRunner`), measured on renders. Swift's `EnergyTests` ported (PORTING.md's proof, form
//! (a)); the same renders held against Swift's output sample for sample are the "energy" cases
//! of `tests/runner_golden.rs`.
//!
//! Swift renders through `WaveOptions.extended` (every fix, `.loudness` among them, whose
//! gain it divides out); this runner plays `.energy` alone beside the instrument table and the
//! genre's room, so the contour is what the arc does and there is no gain to take out.

use djrust::musicality::apply;
use djrust::musicality::echo::EchoSpec;
use djrust::musicality::energy_curve::EnergyCurve;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::space::SpaceSpec;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;
use djrust::TrackSpec;

const SR: f64 = 22_050.0;

fn played(genre: &str, structure: Option<&str>) -> Project {
    let spec = TrackSpec::new(genre, 42.0, structure);
    apply(&spec.project(), &spec, Fixes::ALL)
}

fn amp_db(x: f64) -> f64 {
    20.0 * libm::log10(x.max(1e-10))
}

/// The whole song, mono, synth voices, with or without the energy arc, in the genre's room.
fn render_song(p: &Project, energy: bool, genre: &str) -> Vec<f32> {
    let o = WaveOptions {
        master: Master::Limit,
        instruments: Some(InstrumentTable::shared()),
        musicality: if energy {
            Fixes::ENERGY | Fixes::SPACE
        } else {
            Fixes::SPACE
        },
        space: Some(SpaceSpec::for_genre(genre, false)),
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, 0));
    r.play();
    let steps: i64 = p.structure.iter().map(|s| s.steps).sum();
    let mut out = vec![0f32; (steps as f64 * SR * 15.0 / p.tempo) as usize + 512];
    let mut off = 0;
    while off + 512 <= out.len() && !r.stopped() {
        r.process(&mut out[off..off + 512], None, 512);
        off += 512;
    }
    out
}

/// RMS (dB) of each section.
fn section_db(x: &[f32], p: &Project) -> Vec<(String, f64)> {
    let per_step = SR * 15.0 / p.tempo;
    let mut at = 0i64;
    p.structure
        .iter()
        .map(|s| {
            let a = (at as f64 * per_step) as usize;
            let b = x.len().min(((at + s.steps) as f64 * per_step) as usize);
            at += s.steps;
            let sum: f64 = x[a..b].iter().map(|v| *v as f64 * *v as f64).sum();
            (
                s.name.clone(),
                amp_db(libm::sqrt((sum / (b - a).max(1) as f64).max(1e-20))),
            )
        })
        .collect()
}

/// EnergyTests.curveFollowsTheComposedArc: the most energetic section at unity, an intro and a
/// breakdown under it and darker, a buildup sweeping up into the drop.
#[test]
fn the_curve_follows_the_composed_arc() {
    let p = played("trance", Some("standard"));
    let c = EnergyCurve::new(&p).expect("a curve");
    let names: Vec<&str> = p.structure.iter().map(|s| s.name.as_str()).collect();
    let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
    let (drop, intro, breakdown, buildup) =
        (at("drop"), at("intro"), at("breakdown"), at("buildup"));
    assert_eq!(c.gain[drop], 1.0);
    assert!(
        amp_db(c.gain[intro]) < -3.0,
        "intro {} dB",
        amp_db(c.gain[intro])
    );
    assert!(c.cutoff[intro] < 3000.0 && c.cutoff[breakdown] < 3000.0 && c.cutoff[drop] > 15000.0);
    assert!(c.ramp[buildup] && !c.ramp[drop]);
    // the buildup starts at its own values and arrives at the drop's
    let (s0, s1) = (c.start[buildup], c.start[buildup + 1]);
    assert_eq!(c.at(s0).1, c.cutoff[buildup]);
    assert!(c.at(s1 - 1).1 > 0.8 * c.cutoff[buildup + 1]);
    assert!(c.at((s0 + s1) / 2).1 < c.at(s1 - 8).1);
    // every occurrence of a section name plays alike (the composer marks only the first)
    for (i, n) in names.iter().enumerate() {
        let first = names.iter().position(|x| x == n).unwrap();
        assert!(
            c.gain[i] == c.gain[first] && c.cutoff[i] == c.cutoff[first],
            "{n} #{i}"
        );
    }
}

/// EnergyTests.sectionsHaveALevelContour: rendered, the section the curve marks loudest plays as
/// before, the others come down, the intro by several dB, and sections that played at one level
/// now have a contour.
#[test]
fn sections_have_a_level_contour() {
    for genre in ["techno", "lofi"] {
        let p = played(genre, Some("standard"));
        let curve = EnergyCurve::new(&p).expect("a curve");
        let flat = section_db(&render_song(&p, false, genre), &p);
        let arc = section_db(&render_song(&p, true, genre), &p);
        let depth = |s: &[(String, f64)]| {
            let top = s.iter().map(|x| x.1).fold(f64::MIN, f64::max);
            s.iter().map(|x| top - x.1).sum::<f64>() / s.len() as f64
        };
        eprintln!(
            "energy {genre}: depth {:.2} -> {:.2} dB",
            depth(&flat),
            depth(&arc)
        );
        for (i, g) in curve.gain.iter().enumerate() {
            eprintln!(
                "  {:10} gain {g:.3} cutoff {:7.0}  flat {:7.2}  arc {:7.2}",
                arc[i].0, curve.cutoff[i], flat[i].1, arc[i].1
            );
        }
        if genre == "techno" {
            assert!(depth(&flat) < 1.5, "{genre} was flat: {flat:?}");
            assert!(
                depth(&arc) > depth(&flat) + 1.5,
                "{genre}: {flat:?} -> {arc:?}"
            );
        }
        for (i, g) in curve.gain.iter().enumerate() {
            if *g >= 0.999 {
                assert!(
                    (arc[i].1 - flat[i].1).abs() < 0.75,
                    "{genre} {} keeps its level",
                    arc[i].0
                );
            } else if *g <= 0.8 {
                assert!(
                    arc[i].1 < flat[i].1 - 1.0,
                    "{genre} {} at {g}: {flat:?} -> {arc:?}",
                    arc[i].0
                );
            }
        }
        let intro = arc.iter().position(|x| x.0 == "intro").unwrap();
        let loudest = (0..flat.len())
            .max_by(|a, b| arc[*a].1.partial_cmp(&arc[*b].1).unwrap())
            .unwrap();
        // the intro comes down, and sits several dB under the peak
        assert!(arc[intro].1 < flat[intro].1 - 2.0);
        assert!(arc[intro].1 < arc[loudest].1 - 3.0);
    }
}

/// EnergyTests.loopsAndParityAreUntouched: a loop has no arc; a project without the flag renders
/// exactly as before.
#[test]
fn loops_and_parity_are_untouched() {
    assert!(EnergyCurve::new(&played("techno", Some("standard"))).is_some());
    assert!(EnergyCurve::new(&played("techno", None)).is_none());
    let p = played("techno", Some("minimal"));
    let off = WaveOptions {
        musicality: Fixes::ALL & !Fixes::ENERGY & !Fixes::SWEEP,
        ..Default::default()
    };
    assert!(WaveRunner::prepare(&p, SR, &off, 0).energy.is_none());
    let on = WaveOptions {
        musicality: Fixes::ENERGY,
        ..Default::default()
    };
    assert!(WaveRunner::prepare(&p, SR, &on, 0).energy.is_some());
    // `.sweep` reads the curve's ramps, so it builds the curve too (and `.energy` stays off)
    let sweep = WaveOptions {
        musicality: Fixes::SWEEP,
        ..Default::default()
    };
    assert!(WaveRunner::prepare(&p, SR, &sweep, 0).energy.is_some());
    // another start of the same song brings the playing project's curve along
    let mut again = on.clone();
    again.track_energy = WaveRunner::prepare(&p, SR, &on, 0).energy;
    assert_eq!(
        WaveRunner::prepare(&p, SR, &again, 0).energy,
        WaveRunner::prepare(&p, SR, &on, 0).energy
    );
}

// ---- the curve applied, on projects small enough to read ---------------------------------

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

fn kick_and_lead() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert(
        "kick",
        ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
    );
    p.nets.insert(
        "melody",
        ring("melody", 4, &[(0, 84), (2, 88), (6, 91)], 200),
    );
    p
}

fn only(p: &Project, keep: &str) -> Project {
    let mut q = p.clone();
    let others: Vec<String> = q
        .nets
        .keys()
        .to_vec()
        .into_iter()
        .filter(|k| k != keep)
        .collect();
    for k in others {
        q.nets.remove(&k);
    }
    q
}

fn arc(gain: f64, cutoff: f64) -> EnergyCurve {
    EnergyCurve {
        start: vec![0],
        gain: vec![gain],
        cutoff: vec![cutoff],
        ramp: vec![false],
    }
}

fn render(p: &Project, o: &WaveOptions, blocks: usize, block: usize) -> Vec<f32> {
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, o, 0));
    r.play();
    let mut out = vec![0f32; blocks * block];
    for b in 0..blocks {
        r.process(&mut out[b * block..(b + 1) * block], None, block);
    }
    out
}

fn energy_options(curve: EnergyCurve) -> WaveOptions {
    WaveOptions {
        master: Master::Limit,
        musicality: Fixes::ENERGY,
        track_energy: Some(curve),
        ..Default::default()
    }
}

fn rms(x: &[f32]) -> f64 {
    libm::sqrt(x.iter().map(|v| *v as f64 * *v as f64).sum::<f64>() / x.len() as f64)
}

/// The section's gain scales the whole mix, and the lowpass touches the tonal voices only: a
/// kick alone plays at exactly the gain (to a float), a high lead falls with the cutoff.
#[test]
fn the_gain_scales_the_mix_and_the_lowpass_leaves_the_drums_alone() {
    let p = kick_and_lead();
    // (the app's master: with beats' "linear" the energy path would still apply the master gain,
    // 0.8, as Swift's `finishEnergy` does, where the plain path does not)
    let plain = WaveOptions {
        master: Master::Limit,
        ..Default::default()
    };
    // glide: the gain is primed to the section's at the first block, so it holds from the start
    let kick = only(&p, "kick");
    let dry = render(&kick, &plain, 100, 256);
    let half = render(&kick, &energy_options(arc(0.5, 200.0)), 100, 256);
    for (a, b) in dry.iter().zip(&half) {
        assert!(
            (*b as f64 - 0.5 * *a as f64).abs() < 1e-6 * (1.0 + a.abs() as f64),
            "{a} {b}"
        );
    }
    // a bright lead at C6 and up: a 300 Hz lowpass takes its level well down
    let lead = only(&p, "melody");
    let bright = render(&lead, &plain, 100, 256);
    let dark = render(&lead, &energy_options(arc(1.0, 300.0)), 100, 256);
    let drop_db = amp_db(rms(&dark) / rms(&bright));
    assert!(drop_db < -12.0, "lowpassed at 300 Hz: {drop_db} dB");
    // open: 18 kHz leaves it near unchanged (the filter's own phase aside)
    let open = render(&lead, &energy_options(arc(1.0, 18_000.0)), 100, 256);
    assert!(amp_db(rms(&open) / rms(&bright)).abs() < 0.5);
}

/// Without `.energy` in the options the curve is carried but not played: the render is the
/// plain one, bit for bit (`.sweep` builds the curve and leaves the level alone).
#[test]
fn a_curve_without_the_fix_changes_nothing() {
    let p = kick_and_lead();
    let plain = WaveOptions {
        master: Master::Limit,
        ..Default::default()
    };
    let mut carried = energy_options(arc(0.5, 300.0));
    carried.musicality = Fixes::SWEEP;
    let a = render(&p, &plain, 100, 256);
    let b = render(&p, &carried, 100, 256);
    assert_eq!(
        a.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        b.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    // ... and a block longer than the runner's buffers plays flat, as Swift's
    let long = render(
        &p,
        &energy_options(arc(0.5, 300.0)),
        1,
        djrust::wave::runner::MAX_BLOCK + 1,
    );
    let flat = render(&p, &plain, 1, djrust::wave::runner::MAX_BLOCK + 1);
    assert_eq!(
        long.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        flat.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
}

/// The echo's return goes into the tonal side (so the section's lowpass reaches it), not beside
/// the drums: well after the note has gone, a lead's repeats are dark under a 300 Hz cutoff and
/// bright under an open one. (Mixed in with the drums they would be as loud either way.)
#[test]
fn the_returns_go_through_the_lowpass() {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(64);
    n.bind(0, 84, 4, 110, 0);
    n.bindings.get_mut("t0").unwrap().duration_steps = 1;
    n.track.channel = 4;
    n.track.group = Some("melody".into());
    p.nets.insert("melody", n);
    let echo = |cutoff: f64| {
        let mut o = energy_options(arc(1.0, cutoff));
        o.musicality = Fixes::ENERGY | Fixes::ECHO;
        o.echo = Some(EchoSpec {
            steps: 3.0,
            feedback: 0.5,
            wet: 0.5,
            damping: 9000.0,
        });
        o
    };
    // 0.45 s on: the note (one step, 0.125 s) is long gone; what sounds is its repeats
    let tail = |x: &[f32]| rms(&x[(0.45 * SR) as usize..(2.0 * SR) as usize]);
    let open = tail(&render(&p, &echo(18_000.0), 200, 256));
    let dark = tail(&render(&p, &echo(300.0), 200, 256));
    assert!(open > 1e-4, "the echo repeats: {open}");
    assert!(
        amp_db(dark / open) < -12.0,
        "repeats under the lowpass: {} dB",
        amp_db(dark / open)
    );
}
