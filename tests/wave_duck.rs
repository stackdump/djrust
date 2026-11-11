//! `.duck`: bass and pad dip on each kick and swell back (`WaveOptions::duck_depth`), measured on
//! renders through a linear master, so a mix minus its kick-alone render is exactly the pad as
//! it played. Swift's `DuckTests` ported (PORTING.md's proof, form (a)); the same renders held
//! against Swift's output sample for sample are the "duck" cases of `tests/runner_golden.rs`.

use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;

const SR: f64 = 48_000.0;

/// A four-on-the-floor kick and a pad chord held for the bar, at 120 BPM (a beat is 0.5 s).
fn kick_and_pad(pad: bool, kick: bool) -> Project {
    let mut k = NetBundle::ring(16);
    for step in [0, 4, 8, 12] {
        k.bind(step, 36, 10, 100, 50);
    }
    k.track.channel = 10;
    k.track.group = Some("drums".into());
    let mut h = NetBundle::ring(16);
    for (v, note) in [60, 64, 67].into_iter().enumerate() {
        h.bind(v as i64, note, 7, 90, 1950);
    }
    h.track.channel = 7;
    h.track.group = Some("harmony".into());
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    if kick {
        p.nets.insert("kick", k);
    }
    if pad {
        p.nets.insert("harmony", h);
    }
    p
}

fn render(p: &Project, depth: f64) -> Vec<f32> {
    let o = WaveOptions {
        master: Master::Linear,
        duck_depth: depth,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, 0));
    r.play();
    let mut out = vec![0f32; (4.0 * SR) as usize];
    let mut off = 0;
    while off < out.len() {
        let n = 256.min(out.len() - off);
        r.process(&mut out[off..off + n], None, n);
        off += n;
    }
    out
}

fn rms(x: &[f32], a: usize, b: usize) -> f64 {
    let s: f64 = x[a..b].iter().map(|v| *v as f64 * *v as f64).sum();
    libm::sqrt(s / (b - a) as f64)
}

fn amp_db(x: f64) -> f64 {
    20.0 * libm::log10(x.max(1e-12))
}

fn minus(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// DuckTests.padDipsOnTheKickAndRecovers: just after each kick the pad is about `depth` down;
/// just before the next it is back.
#[test]
fn the_pad_dips_on_the_kick_and_recovers() {
    let kick_alone = render(&kick_and_pad(false, true), 0.5);
    let ducked = render(&kick_and_pad(true, true), 0.5);
    let plain = render(&kick_and_pad(true, true), 0.0);
    let pad = minus(&ducked, &kick_alone);
    let pad_plain = minus(&plain, &kick_alone);
    let beat = (0.5 * SR) as usize;
    for b in 1..7 {
        let k = b * beat + 1; // the kick lands on the beat
        let early = amp_db(rms(&pad, k + 480, k + 1440) / rms(&pad_plain, k + 480, k + 1440));
        let late = amp_db(
            rms(&pad, k + beat - 2400, k + beat - 480)
                / rms(&pad_plain, k + beat - 2400, k + beat - 480),
        );
        assert!(early < -4.0, "beat {b}: {early} dB 10-30 ms after the kick");
        assert!(late > -0.5, "beat {b}: {late} dB just before the next kick");
    }
}

/// DuckTests.noDepthIsBitIdenticalAndTheKickIsUntouched: depth 0 (beats' engine, and genres that
/// don't pump) renders exactly as before; the kick itself is never ducked.
#[test]
fn no_depth_is_bit_identical_and_the_kick_is_untouched() {
    let a = render(&kick_and_pad(true, true), 0.0);
    let o = WaveOptions {
        master: Master::Linear,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, o);
    r.load(&kick_and_pad(true, true));
    r.play();
    let mut b = vec![0f32; a.len()];
    let mut off = 0;
    while off < b.len() {
        let n = 256.min(b.len() - off);
        r.process(&mut b[off..off + n], None, n);
        off += n;
    }
    assert_eq!(
        a.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        b.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    let ducked_kick = render(&kick_and_pad(false, true), 0.5);
    let plain_kick = render(&kick_and_pad(false, true), 0.0);
    assert_eq!(
        ducked_kick.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        plain_kick.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
}

/// The envelope the runner hands the host for its sampled slots (`duckBuf`): the dip's depth
/// after a kick, back to 1 before the next, and 1 the whole block for a project without a depth.
#[test]
fn the_gains_for_the_samplers_follow_the_dip() {
    let o = WaveOptions {
        master: Master::Linear,
        duck_depth: 0.5,
        ..Default::default()
    };
    let p = kick_and_pad(true, true);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &o, 0));
    r.play();
    let mut buf = vec![0f32; 256];
    let (mut lowest, mut recovered) = (1f32, false);
    for b in 0..400 {
        r.process(&mut buf, None, 256);
        let g = r.duck_gains(256);
        assert_eq!(g.len(), 256);
        lowest = g.iter().fold(lowest, |m, x| m.min(*x));
        // half a beat after a kick (0.25 s): back near 1
        if b > 90 && g.iter().all(|x| *x > 0.99) {
            recovered = true;
        }
    }
    assert!((0.49..0.56).contains(&lowest), "deepest dip {lowest}");
    assert!(recovered, "the gain never came back to unity");
    // no depth: unity throughout
    let plain = WaveOptions {
        master: Master::Linear,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, plain.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &plain, 0));
    r.play();
    for _ in 0..100 {
        r.process(&mut buf, None, 256);
        assert!(r.duck_gains(256).iter().all(|x| *x == 1.0));
    }
}

/// Only the bass and harmony groups duck, and only a kick keys it (`buildLanes`: `ducks`,
/// `isKick`); the project reports its depth.
#[test]
fn only_bass_and_pad_duck() {
    let o = WaveOptions {
        duck_depth: 0.4,
        ..Default::default()
    };
    let mut p = kick_and_pad(true, true);
    let mut bass = NetBundle::ring(16);
    bass.bind(0, 36, 6, 100, 100);
    bass.track.channel = 6;
    bass.track.group = Some("bass".into());
    p.nets.insert("bass", bass);
    let mut lead = NetBundle::ring(16);
    lead.bind(0, 72, 4, 100, 100);
    lead.track.channel = 4;
    lead.track.group = Some("melody".into());
    p.nets.insert("melody", lead);
    let w = WaveRunner::prepare(&p, SR, &o, 0);
    assert_eq!(w.duck_depth, 0.4);
    for (net, ducks) in [
        ("bass", true),
        ("harmony", true),
        ("melody", false),
        ("kick", false),
    ] {
        let lanes = w.lanes_of(net);
        assert_eq!(lanes.len(), 1, "{net}");
        assert_eq!(lanes[0].ducks(), ducks, "{net}");
    }
    assert!(w.lanes_of("kick")[0].keys_the_duck());
    assert!(!w.lanes_of("bass")[0].keys_the_duck());
    // off: nothing ducks, nothing keys it
    let off = WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0);
    assert_eq!(off.duck_depth, 0.0);
    assert!(off.lanes().iter().all(|l| !l.ducks() && !l.keys_the_duck()));
}

/// DuckTests.depthByGenre (`PreparedTrack`'s half): a deck's track gets its genre's depth, and
/// only with the fix on (`WaveOptions.forGenre`). The depths themselves are in
/// `musicality_dsp.rs`.
#[test]
fn a_decks_track_gets_its_genres_depth() {
    use djrust::musicality::fixes::Fixes;
    let with = WaveOptions {
        musicality: Fixes::DUCK,
        ..Default::default()
    };
    assert_eq!(with.for_genre("house").duck_depth, 0.5);
    assert_eq!(with.for_genre("techno").duck_depth, 0.5);
    assert_eq!(with.for_genre("jazz").duck_depth, 0.0);
    assert_eq!(WaveOptions::default().for_genre("house").duck_depth, 0.0);
}

/// `WaveOptions.extended.forGenre(genre)` with the track's seed (`PreparedTrack`'s options):
/// every fix, and each setting a genre picks.
#[test]
fn a_track_plays_with_every_fix_and_its_genres_settings() {
    use djrust::musicality::fixes::Fixes;
    let o = WaveOptions::for_track("techno", 42.0);
    assert_eq!(o.musicality, Fixes::ALL);
    // `.all` holds `.groove` and `.drift` (PreparedTrack plays the swing and the 1/f humanize);
    // the runner reads them off `musicality`, as Swift does
    assert!(o.musicality.contains(Fixes::GROOVE) && o.musicality.contains(Fixes::DRIFT));
    assert!(o.musicality.contains(Fixes::RISER)); // RiserTests.flagIsRegistered
    assert_eq!(o.master, Master::Limit);
    assert!(o.instruments.is_some());
    assert_eq!(o.voicing_seed, 42.0);
    assert_eq!(o.genre.as_deref(), Some("techno"));
    assert_eq!(o.duck_depth, 0.5);
    assert!(o.space.is_some());
    assert!(!o.sub_octave);
    // a genre's own: dnb's sub, trance's plucked arp, an echo where the genre has one
    assert!(WaveOptions::for_track("dnb", 1.0).sub_octave);
    assert!(WaveOptions::for_track("trance", 1.0)
        .voice_design
        .pluck_groups
        .contains("arp"));
    assert!(WaveOptions::for_track("trance", 1.0).echo.is_some());
    assert!(WaveOptions::for_track("jazz", 1.0).echo.is_none());
    // the beats-parity options hold none of it
    assert!(WaveOptions::default().musicality.is_empty());
}

// ---- the duck under gestures (FixesUnderGesturesTests) --------------------------------------

/// `seconds` of `p` at duck `depth`, mono, in 240-frame blocks; `setup` runs before play and
/// `at(runner, frame)` before each block.
fn render_with(
    p: &Project,
    depth: f64,
    seconds: f64,
    setup: impl FnOnce(&mut WaveRunner),
    mut at: impl FnMut(&mut WaveRunner, usize),
) -> Vec<f32> {
    let o = WaveOptions {
        master: Master::Linear,
        duck_depth: depth,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, 0));
    setup(&mut r);
    r.play();
    let mut out = vec![0f32; (seconds * SR) as usize];
    let mut off = 0;
    while off < out.len() {
        at(&mut r, off);
        let n = 240.min(out.len() - off);
        r.process(&mut out[off..off + n], None, n);
        off += n;
    }
    out
}

fn rms_db(x: &[f32]) -> f64 {
    10.0 * libm::log10(
        (x.iter().map(|v| *v as f64 * *v as f64).sum::<f64>() / x.len() as f64).max(1e-20),
    )
}

/// FixesUnderGesturesTests.aMutedKickDucksNothing: the duck follows the kick you hear: with the
/// kick's role muted the pad plays exactly as it does with no kick at all.
#[test]
fn a_muted_kick_ducks_nothing() {
    let muted = render_with(
        &kick_and_pad(true, true),
        0.5,
        4.0,
        |r| r.set_mute("kick", true),
        |_, _| {},
    );
    let pad_alone = render_with(&kick_and_pad(true, false), 0.5, 4.0, |_| {}, |_, _| {});
    assert!(rms_db(&pad_alone) > -60.0, "the pad plays");
    let worst = muted
        .iter()
        .zip(&pad_alone)
        .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst < 1e-5, "{worst}");
}

/// FixesUnderGesturesTests.aBreakdownLetsThePadSwellBack: a breakdown (the drums muted by the
/// macro) lets the pad swell back to its undipped level within a beat of the last kick, and stay
/// there.
#[test]
fn a_breakdown_lets_the_pad_swell_back() {
    use djrust::wave::NetMacro;
    let fire = (0.9 * SR) as usize;
    let broken = render_with(
        &kick_and_pad(true, true),
        0.5,
        4.0,
        |_| {},
        |r, off| {
            if off == fire {
                r.fire_macro(NetMacro::Breakdown, 64);
            }
        },
    );
    let plain = render_with(&kick_and_pad(true, false), 0.0, 4.0, |_| {}, |_, _| {});
    let secs = |s: f64| (s * SR) as usize;
    for (a, b) in [(1.6, 2.4), (2.4, 3.2), (3.2, 3.9)] {
        let x = rms_db(&broken[secs(a)..secs(b)]);
        let y = rms_db(&plain[secs(a)..secs(b)]);
        assert!((x - y).abs() < 0.2, "{a}-{b} s: {x} vs {y} dB");
    }
}
