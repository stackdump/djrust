//! `GrooveTests`, `FeelTests`, `DriftTests` and `GrooveTemplateTests` (Swift): the project's
//! swing and humanize (`.groove`), the family's groove template (`.feel`) and humanize as
//! seeded 1/f noise (`.drift`), measured from the hits the runner plays and from the
//! rendered audio. Golden: none (dj-only; the Swift suites' own measurements).

use djrust::gen::tables::BeatsTables;
use djrust::musicality::fixes::Fixes;
use djrust::pflow::NetBundle;
use djrust::wave::groove::{
    Drift, Grid, GrooveTemplate, Swing, Voice, FAMILY_NAMES, MAX_OFFSET_MS,
};
use djrust::wave::*;
use djrust::Project;
use std::collections::{BTreeMap, BTreeSet};

const SR: f64 = 48_000.0;

fn lofi() -> &'static GrooveTemplate {
    GrooveTemplate::for_genre("lofi").unwrap()
}

/// One note on every sixteenth of a 16-step bar: a closed hat (drum channel 12) or a
/// tonal C.
fn sixteenths(drum: bool, tempo: f64, velocity: i64, swing: f64, humanize: f64) -> Project {
    let mut nb = NetBundle::ring(16);
    let channel = if drum { 12 } else { 2 };
    for step in 0..16 {
        nb.bind(step, if drum { 42 } else { 60 }, channel, velocity, 50);
    }
    nb.track.channel = channel;
    let mut p = Project {
        tempo,
        swing,
        humanize,
        ..Default::default()
    };
    p.nets.insert(if drum { "hat" } else { "keys" }, nb);
    p
}

struct Flags {
    groove: bool,
    feel: Option<GrooveTemplate>,
    drift: bool,
    seed: f64,
}

fn flags(groove: bool, feel: Option<&GrooveTemplate>, drift: bool) -> Flags {
    Flags {
        groove,
        feel: feel.cloned(),
        drift,
        seed: 42.0,
    }
}

fn options(f: &Flags) -> WaveOptions {
    WaveOptions {
        groove: f.groove,
        // a template plays only with `.feel` in the musicality (`feel: opts.musicality.contains(
        // .feel) ? opts.feel : nil`), so a test that wants one asks for the flag with it
        musicality: if f.feel.is_some() {
            Fixes::FEEL
        } else {
            Fixes::EMPTY
        },
        feel: f.feel.clone(),
        drift: f.drift,
        voicing_seed: f.seed,
        record_hits: true,
        ..Default::default()
    }
}

fn run_blocks(r: &mut WaveRunner, frames: usize) -> Vec<f32> {
    let mut out = vec![0f32; frames];
    let mut off = 0;
    while off < frames {
        let n = 256.min(frames - off);
        r.process_at(&mut out[off..off + n], None, n, off as i64);
        off += n;
    }
    out
}

/// `bars` bars of `p` in 256-frame blocks, and the hits it played.
fn render(p: &Project, f: &Flags, bars: usize) -> (Vec<f32>, Vec<HitRecord>) {
    let o = options(f);
    let mut r = WaveRunner::new(SR, o);
    r.load(p);
    r.play();
    let audio = run_blocks(&mut r, (bars as f64 * 4.0 * 60.0 / p.tempo * SR) as usize);
    (audio, std::mem::take(&mut r.hits))
}

fn late_ms(h: &HitRecord) -> f64 {
    h.delay_frames() as f64 / SR * 1000.0
}

// ---- GrooveTests -------------------------------------------------------------

fn hats(swing: f64, humanize: f64) -> Project {
    let mut nb = NetBundle::ring(16);
    for step in [1, 4] {
        nb.bind(step, 42, 12, 100, 50);
    }
    nb.track.channel = 12;
    let mut p = Project {
        tempo: 120.0, // a sixteenth is 125 ms
        swing,
        humanize,
        ..Default::default()
    };
    p.nets.insert("hat", nb);
    p
}

/// Sample index of every onset: the first sample over the threshold after 20 ms of quiet.
fn onsets(x: &[f32]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut quiet = usize::MAX / 2;
    for (i, v) in x.iter().enumerate() {
        if v.abs() > 0.01 {
            if quiet > (0.02 * SR) as usize {
                out.push(i);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}

fn render_groove(p: &Project, groove: bool) -> Vec<f32> {
    let mut r = WaveRunner::new(
        SR,
        WaveOptions {
            groove,
            ..Default::default()
        },
    );
    r.load(p);
    r.play();
    run_blocks(&mut r, (4.0 * 2.0 * SR) as usize)
}

/// Without the fix (beats' wave engine) both hats sit on the grid; with it the off-beat one
/// is late by swing% × half a sixteenth and the on-beat one stays put.
#[test]
fn swing_delays_the_off_beat_sixteenths() {
    let tick = 0.125 * SR;
    let straight = onsets(&render_groove(&hats(50.0, 0.0), false));
    let swung = onsets(&render_groove(&hats(50.0, 0.0), true));
    assert!(
        straight.len() == 8 && swung.len() == 8,
        "{straight:?} {swung:?}"
    );
    for (a, b) in straight.iter().zip(&swung) {
        let off_beat = ((*a as f64 / tick).round() as i64) % 16 == 1;
        let late = (*b as f64 - *a as f64) / SR * 1000.0;
        assert!(
            (late - if off_beat { 31.25 } else { 0.0 }).abs() < 1.0,
            "onset {a}: {late} ms late"
        );
    }
}

/// Humanize moves every hit a little (never early), differently from bar to bar, and the
/// same way on every render (a replay plays the same groove).
#[test]
fn humanize_jitters_deterministically() {
    let tick = 0.125 * SR;
    let a = onsets(&render_groove(&hats(0.0, 40.0), true));
    let b = onsets(&render_groove(&hats(0.0, 40.0), true));
    assert_eq!(a, b);
    assert_eq!(a.len(), 8);
    let offsets: Vec<f64> = a
        .iter()
        .map(|&o| o as f64 - (o as f64 / tick).floor() * tick)
        .collect();
    let distinct: BTreeSet<i64> = offsets.iter().map(|o| (o / SR * 1000.0) as i64).collect();
    assert!(distinct.len() > 2, "the hits move: {offsets:?}");
    for o in offsets {
        assert!(
            o / SR * 1000.0 < 0.4 * 30.0 + 1.0,
            "within humanize's reach: {o}"
        );
    }
}

/// A project with neither (most of beats' techno / trance / edm) plays exactly as before.
#[test]
fn no_swing_no_humanize_is_bit_identical() {
    assert!(render_groove(&hats(0.0, 0.0), true) == render_groove(&hats(0.0, 0.0), false));
}

// ---- FeelTests ---------------------------------------------------------------

fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    let (mx, my) = (x.iter().sum::<f64>() / n, y.iter().sum::<f64>() / n);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
        syy += (b - my) * (b - my);
    }
    sxy / (sxx * syy).sqrt()
}

/// GrooveReport's swing ratio: the mean delay of the odd steps against the even ones.
fn measured_swing(hits: &[HitRecord], bpm: f64) -> Option<f64> {
    let (mut on, mut off) = (Vec::new(), Vec::new());
    for h in hits {
        if h.step() % 2 == 1 {
            off.push(late_ms(h))
        } else {
            on.push(late_ms(h))
        }
    }
    if on.is_empty() || off.is_empty() {
        return None;
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    Some(0.5 + (mean(&off) - mean(&on)) / (2.0 * 15_000.0 / bpm))
}

/// A tonal lane gets the swing only: the measured ratio is the table's at three tempi
/// (within 2 %), the off-beat sixteenths (2r − 1) sixteenths late, the rest on the grid.
#[test]
fn swing_follows_the_table() {
    for bpm in [95.0, 105.0, 130.0] {
        let hits = render(
            &sixteenths(false, bpm, 80, 0.0, 0.0),
            &flags(false, Some(lofi()), false),
            4,
        )
        .1;
        assert!(hits.len() >= 60, "{} hits", hits.len());
        let want = lofi().swing.ratio(bpm);
        assert!(want > 0.52, "the chill line swings at {bpm}: {want}");
        let got = measured_swing(&hits, bpm).unwrap();
        assert!(
            (got - want).abs() / want < 0.02,
            "{bpm} bpm: {got} against {want}"
        );
        let sixteenth_ms = 15_000.0 / bpm;
        for h in &hits {
            let expected = if h.step() % 2 == 1 {
                (2.0 * want - 1.0) * sixteenth_ms
            } else {
                0.0
            };
            assert!(
                (late_ms(h) - expected).abs() < 0.05,
                "tick {}: {} ms",
                h.tick,
                late_ms(h)
            );
            assert_eq!(h.velocity, 80, "tonal velocities stay as bound");
        }
    }
}

/// The jazz family swings eighths: only the off-beat eighth of each beat moves.
#[test]
fn jazz_swings_eighths() {
    let jazz = GrooveTemplate::for_genre("bossa").unwrap();
    assert_eq!(jazz.swing.grid, Grid::Eighth);
    let bpm = 200.0;
    let hits = render(
        &sixteenths(false, bpm, 80, 0.0, 0.0),
        &flags(false, Some(jazz), false),
        4,
    )
    .1;
    assert!(hits.len() >= 60);
    let want = jazz.swing.delay_ms(bpm);
    assert!(want > 1.0);
    for h in &hits {
        assert!(
            (late_ms(h) - if h.step() % 4 == 2 { want } else { 0.0 }).abs() < 0.05,
            "tick {}",
            h.tick
        );
    }
}

/// A drum plays its template offset plus the swing at each bar step, never early.
#[test]
fn drums_play_the_template_offsets() {
    let bpm = 105.0;
    let hits = render(
        &sixteenths(true, bpm, 80, 0.0, 0.0),
        &flags(false, Some(lofi()), false),
        4,
    )
    .1;
    assert!(hits.len() >= 60);
    let swing = lofi().swing.delay_ms(bpm);
    for h in &hits {
        assert!(h.delay_frames() >= 0, "tick {} early", h.tick);
        let step = h.step() as usize;
        let expected =
            lofi().offset_ms(Voice::Hihat, step) + if step % 2 == 1 { swing } else { 0.0 };
        assert!(
            (late_ms(h) - expected).abs() < 0.05,
            "tick {}: {} vs {expected}",
            h.tick,
            late_ms(h)
        );
    }
}

/// Velocities follow the template's accents (correlation > 0.9 over the bar's 16 steps).
#[test]
fn velocities_follow_the_template() {
    let hits = render(
        &sixteenths(true, 100.0, 80, 0.0, 0.0),
        &flags(false, Some(lofi()), false),
        4,
    )
    .1;
    assert!(hits.len() >= 60);
    let (mut played, mut count) = ([0.0; 16], [0.0; 16]);
    for h in &hits {
        played[h.step() as usize] += h.velocity as f64;
        count[h.step() as usize] += 1.0;
    }
    assert!(!count.contains(&0.0));
    let mean: Vec<f64> = played.iter().zip(&count).map(|(a, b)| a / b).collect();
    let template: Vec<f64> = (0..16).map(|s| lofi().velocity(Voice::Hihat, s)).collect();
    let distinct: BTreeSet<u64> = template.iter().map(|x| x.to_bits()).collect();
    assert!(distinct.len() > 4, "the template accents");
    let r = pearson(&mean, &template);
    assert!(r > 0.9, "correlation {r}: {mean:?}");
}

/// `bars` bars of `p` under `fixes` with `feel` in the options, like `FeelTests.render`
/// (`fixes:` and `feel:`): the template is the options' own, the flag is the fixes'.
fn render_fixes(p: &Project, fixes: Fixes, feel: Option<&GrooveTemplate>, bars: usize) -> Vec<f32> {
    let o = WaveOptions {
        musicality: fixes,
        feel: feel.cloned(),
        voicing_seed: 42.0,
        record_hits: true,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, o);
    r.load(p);
    r.play();
    run_blocks(&mut r, (bars as f64 * 4.0 * 60.0 / p.tempo * SR) as usize)
}

/// FeelTests.withoutTheFlagIsBitIdentical: without `.feel` the template is never read: a render
/// with `feel` set but the flag off is bit-identical to one without it (beats' engine, and
/// `.groove` alone, beats' swing); with the flag the hats move; the flag on without a template
/// plays as without the flag.
#[test]
fn without_the_flag_the_render_is_bit_identical() {
    for (fixes, swing) in [
        (Fixes::EMPTY, 0.0),
        (Fixes::GROOVE, 0.0),
        (Fixes::GROOVE, 40.0),
    ] {
        let p = sixteenths(true, 110.0, 80, swing, swing / 2.0);
        let a = render_fixes(&p, fixes, Some(lofi()), 4);
        let b = render_fixes(&p, fixes, None, 4);
        assert!(a == b, "{fixes:?} swing {swing}");
    }
    // with it, the hats move
    let p = sixteenths(true, 110.0, 80, 0.0, 0.0);
    assert!(
        render_fixes(&p, Fixes::FEEL, Some(lofi()), 4) != render_fixes(&p, Fixes::EMPTY, None, 4)
    );
    // and the flag on without a template plays as without the flag
    assert!(render_fixes(&p, Fixes::FEEL, None, 4) == render_fixes(&p, Fixes::EMPTY, None, 4));
}

/// The prepared project keeps the template only under the flag (`WaveProject.feel`, which
/// `sameSongOptions` hands on to a section jump): a template in the options of a prepare without
/// `.feel` is not on the project.
#[test]
fn the_project_holds_the_template_only_under_the_flag() {
    let p = sixteenths(true, 110.0, 80, 0.0, 0.0);
    let prepare = |fixes: Fixes| {
        let o = WaveOptions {
            musicality: fixes,
            feel: Some(lofi().clone()),
            ..Default::default()
        };
        WaveRunner::prepare(&p, SR, &o, 0).feel.map(|f| f.family)
    };
    assert_eq!(prepare(Fixes::EMPTY), None);
    assert_eq!(prepare(Fixes::GROOVE), None);
    assert_eq!(prepare(Fixes::FEEL), Some("chill".to_string()));
}

// ---- DriftTests --------------------------------------------------------------

fn drift_hats(tempo: f64, humanize: f64, swing: f64) -> Project {
    sixteenths(true, tempo, 80, swing, humanize)
}

/// Welch's estimate of the noise's power spectrum: the mean periodogram of `segments`
/// Hann-windowed segments of `n` ticks; bin k is k / n cycles a tick.
fn spectrum(key: u64, n: usize, segments: usize) -> Vec<f64> {
    let mut power = vec![0.0; n / 2];
    let window: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect();
    for s in 0..segments {
        let mut x: Vec<f64> = (0..n)
            .map(|i| Drift::pink(key, 3, 0, (s * n + i) as i64))
            .collect();
        let m = x.iter().sum::<f64>() / n as f64;
        for i in 0..n {
            x[i] = (x[i] - m) * window[i];
        }
        for k in 1..n / 2 {
            let (mut re, mut im) = (0.0, 0.0);
            for i in 0..n {
                let a = 2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
                re += x[i] * a.cos();
                im -= x[i] * a.sin();
            }
            power[k] += (re * re + im * im) / segments as f64;
        }
    }
    power
}

/// The noise is bounded, a pure function of (key, net, stream, tick) - read in any order
/// it gives the same values - and the key, the net and the stream each change it.
#[test]
fn noise_is_bounded_deterministic_and_seekable() {
    let key = Drift::key(42.0);
    assert_eq!(key, Drift::key(42.0));
    let forward: Vec<f64> = (0..2048).map(|t| Drift::pink(key, 1, 0, t)).collect();
    assert!(forward.iter().all(|&v| (-1.0..=1.0).contains(&v)));
    let distinct: BTreeSet<u64> = forward.iter().map(|x| x.to_bits()).collect();
    assert!(distinct.len() > 1000, "it moves");
    for t in (0..=2047).rev().step_by(37) {
        assert_eq!(Drift::pink(key, 1, 0, t), forward[t as usize], "tick {t}");
    }
    let other: Vec<f64> = (0..2048)
        .map(|t| Drift::pink(Drift::key(43.0), 1, 0, t))
        .collect();
    let net2: Vec<f64> = (0..2048).map(|t| Drift::pink(key, 2, 0, t)).collect();
    let timing: Vec<f64> = (0..2048).map(|t| Drift::pink(key, 1, 1, t)).collect();
    assert!(other != forward && net2 != forward && timing != forward);
    let sd = (forward.iter().map(|x| x * x).sum::<f64>() / forward.len() as f64).sqrt();
    assert!(sd > 0.2 && sd < 0.5, "SD {sd}");
}

/// 1/f: the power falls about 10 dB a decade over the octaves the sum covers.
#[test]
fn noise_is_pink() {
    let n = 512;
    let power = spectrum(Drift::key(7.0), n, 24);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for k in 2..=(n / 4) {
        xs.push((k as f64 / n as f64).log10());
        ys.push(10.0 * power[k].log10());
    }
    let mx = xs.iter().sum::<f64>() / xs.len() as f64;
    let my = ys.iter().sum::<f64>() / ys.len() as f64;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (x, y) in xs.iter().zip(&ys) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
    }
    let slope = sxy / sxx;
    assert!(slope < -7.0 && slope > -13.0, "slope {slope} dB/decade");
}

/// GrooveReport's distinct velocities per position over the drums, and its consistency.
fn distinct_velocities(hits: &[HitRecord]) -> f64 {
    let mut cells: BTreeMap<(&str, i64), (usize, BTreeSet<i64>)> = BTreeMap::new();
    for h in hits.iter().filter(|h| h.is_drum()) {
        let c = cells.entry((h.kind.as_str(), h.step())).or_default();
        c.0 += 1;
        c.1.insert(h.velocity);
    }
    let d: Vec<f64> = cells
        .values()
        .filter(|c| c.0 >= 2)
        .map(|c| c.1.len() as f64)
        .collect();
    if d.is_empty() {
        0.0
    } else {
        d.iter().sum::<f64>() / d.len() as f64
    }
}

fn consistency(hits: &[HitRecord]) -> f64 {
    let drums: Vec<&HitRecord> = hits.iter().filter(|h| h.is_drum()).collect();
    let delays: Vec<f64> = drums.iter().map(|h| late_ms(h)).collect();
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let m = mean(&delays);
    let total: f64 = delays.iter().map(|d| (d - m) * (d - m)).sum();
    let mut cells: BTreeMap<(&str, i64), Vec<f64>> = BTreeMap::new();
    for (h, d) in drums.iter().zip(&delays) {
        cells
            .entry((h.kind.as_str(), h.step()))
            .or_default()
            .push(*d);
    }
    let within: f64 = cells
        .values()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let cm = mean(c);
            c.iter().map(|d| (d - cm) * (d - cm)).sum::<f64>()
        })
        .sum();
    if total > 1e-6 * (delays.len().max(1)) as f64 {
        (1.0 - within / total).max(0.0)
    } else {
        1.0
    }
}

/// On top of `.feel`: each hat is its template offset plus swing plus 0…3 ms × h late (never
/// early), velocities vary at each bar position, and the timing stays mostly the pattern.
#[test]
fn drift_on_top_of_feel() {
    for genre in ["house", "lofi", "funk"] {
        let g = &BeatsTables::shared().genres[genre];
        let template = GrooveTemplate::for_genre(genre).unwrap();
        let p = drift_hats(g.bpm as f64, g.humanize as f64, 0.0);
        let h = g.humanize as f64 / 100.0;
        let hits = render(&p, &flags(false, Some(template), true), 16).1;
        assert!(hits.len() >= 200, "{} hits", hits.len());
        let base = render(&p, &flags(false, Some(template), false), 16).1;
        assert_eq!(base.len(), hits.len());
        for (a, b) in hits.iter().zip(&base) {
            assert!(a.delay_frames() >= 0, "tick {} early", a.tick);
            let extra = late_ms(a) - late_ms(b);
            assert!(
                extra > -0.05 && extra < Drift::LATE_SPAN_MS * h + 0.05,
                "tick {}: +{extra} ms",
                a.tick
            );
            assert!(
                (a.velocity - b.velocity).abs() <= (Drift::VELOCITY_SPAN * h).round() as i64 + 1
            );
        }
        assert!(distinct_velocities(&hits) >= 2.0, "{genre}");
        let c = consistency(&hits);
        assert!(c > 0.9 && c < 1.0, "{genre}: {c}");
        // without drift the template plays one velocity a position
        assert_eq!(distinct_velocities(&base), 1.0);
    }
}

/// With `.groove` alone it replaces the white jitter: the swing as beats has it, then the
/// drift's few late ms - the off-beats no longer jitter ±30 ms × h.
#[test]
fn drift_replaces_the_white_jitter() {
    let p = drift_hats(100.0, 40.0, 30.0);
    let hits = render(&p, &flags(true, None, true), 16).1;
    assert!(hits.len() >= 200);
    let tick_ms = 15_000.0 / 100.0;
    for hit in &hits {
        let swing = if hit.step() % 2 == 1 {
            0.3 * tick_ms * 0.5
        } else {
            0.0
        };
        let extra = late_ms(hit) - swing;
        assert!(
            extra > -0.05 && extra < Drift::LATE_SPAN_MS * 0.4 + 0.05,
            "tick {}: {extra}",
            hit.tick
        );
    }
    assert!(distinct_velocities(&hits) > 1.0);
    // `.drift` on its own humanizes too (no swing without `.groove`)
    let alone = render(&p, &flags(false, None, true), 16).1;
    assert!(alone
        .iter()
        .all(|h| late_ms(h) < Drift::LATE_SPAN_MS * 0.4 + 0.05));
    assert!(
        alone
            .iter()
            .map(|h| h.velocity)
            .collect::<BTreeSet<_>>()
            .len()
            > 1
    );
}

/// The same seed plays the same hits twice; another seed other ones.
#[test]
fn drift_is_deterministic_by_seed() {
    let p = drift_hats(120.0, 30.0, 0.0);
    let mut f = flags(false, Some(lofi()), true);
    let a = render(&p, &f, 16);
    let b = render(&p, &f, 16);
    f.seed = 43.0;
    let c = render(&p, &f, 16);
    assert!(a.1 == b.1 && a.0 == b.0);
    let va: Vec<i64> = a.1.iter().map(|h| h.velocity).collect();
    let vc: Vec<i64> = c.1.iter().map(|h| h.velocity).collect();
    assert!(va != vc);
}

/// Seekable: a project prepared from tick 64 (a section launch) plays the hits the whole
/// song plays there - the noise is a function of the song's tick.
#[test]
fn seek_plays_the_same_drift() {
    let p = drift_hats(120.0, 40.0, 0.0);
    let f = flags(false, None, true);
    let whole = render(&p, &f, 16).1;
    let o = options(&f);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &o, 64));
    r.play();
    let mut buf = vec![0f32; 256];
    for i in 0..2400 {
        r.process_at(&mut buf, None, 256, i * 256);
    }
    let seeked = std::mem::take(&mut r.hits);
    assert!(seeked.len() >= 64);
    let mut by_tick: BTreeMap<i64, &HitRecord> = BTreeMap::new();
    for h in &whole {
        by_tick.entry(h.tick).or_insert(h);
    }
    let mut compared = 0;
    for hit in &seeked {
        let Some(w) = by_tick.get(&hit.tick) else {
            continue;
        };
        assert_eq!(hit.velocity, w.velocity, "tick {}", hit.tick);
        assert!(
            (hit.delay_frames() - w.delay_frames()).abs() <= 1,
            "tick {}",
            hit.tick
        );
        compared += 1;
    }
    assert!(compared >= 64, "{compared} ticks in common");
}

/// Off, nothing reads the seed: `.feel` and `.groove` renders are bit-identical across
/// seeds, and with no humanize there is nothing to drift.
#[test]
fn drift_off_is_untouched() {
    let p = drift_hats(110.0, 30.0, 20.0);
    for (groove, feel) in [(true, None), (false, Some(lofi())), (false, None)] {
        let mut f = flags(groove, feel, false);
        f.seed = 1.0;
        let a = render(&p, &f, 4).0;
        f.seed = 2.0;
        let b = render(&p, &f, 4).0;
        assert!(a == b, "groove {groove}");
    }
    let still = drift_hats(110.0, 0.0, 0.0);
    assert!(
        render(&still, &flags(false, None, true), 4).0
            == render(&still, &flags(false, None, false), 4).0
    );
}

// ---- GrooveTemplateTests -----------------------------------------------------

#[test]
fn every_family_and_genre_has_a_template() {
    let all = GrooveTemplate::all();
    assert_eq!(
        all.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        FAMILY_NAMES.iter().copied().collect()
    );
    for (i, name) in FAMILY_NAMES.iter().enumerate() {
        assert_eq!(
            GrooveTemplate::family_index(i as i64).unwrap().family,
            *name
        );
    }
    let t = BeatsTables::shared();
    assert!(t.genre_ids.len() >= 19);
    for g in t.genre_ids.iter().map(String::as_str).chain(["wrapped"]) {
        let tpl = GrooveTemplate::for_genre(g).unwrap_or_else(|| panic!("{g}"));
        assert_eq!(tpl.index, t.family_of(g), "{g}");
    }
    assert_eq!(GrooveTemplate::for_genre("techno").unwrap().family, "dance");
    assert_eq!(GrooveTemplate::for_genre("funk").unwrap().family, "song");
    assert_eq!(GrooveTemplate::for_genre("bossa").unwrap().family, "jazz");
    assert_eq!(GrooveTemplate::for_genre("lofi").unwrap().family, "chill");
}

#[test]
fn template_values_are_in_range() {
    for t in GrooveTemplate::all().values() {
        assert!(t.files > 0 && !t.styles.is_empty(), "{}", t.family);
        for v in Voice::ALL {
            let (off, vel, hits) = (
                &t.offset_ms[v as usize],
                &t.velocity[v as usize],
                &t.hits[v as usize],
            );
            assert!(off.len() == 16 && vel.len() == 16 && hits.len() == 16);
            assert!(
                off.iter().all(|x| (0.0..=MAX_OFFSET_MS).contains(x)),
                "{}.{}",
                t.family,
                v.name()
            );
            assert!(
                vel.iter().all(|x| (0.25..=1.75).contains(x)),
                "{}.{}",
                t.family,
                v.name()
            );
            // Thin cells are neutral.
            for s in 0..16 {
                if hits[s] < 20 {
                    assert!(
                        off[s] == 0.0 && vel[s] == 1.0,
                        "{}.{}[{s}]",
                        t.family,
                        v.name()
                    );
                }
            }
        }
        // A drummer's hats are not flat: more than one velocity across the bar.
        let hat: BTreeSet<u64> = t.velocity[Voice::Hihat as usize]
            .iter()
            .map(|x| x.to_bits())
            .collect();
        assert!(hat.len() > 1, "{}", t.family);
        // A backbeat (2 and 4) louder than the snare's average, except where it comps (jazz).
        let snare = &t.velocity[Voice::Snare as usize];
        if t.family != "jazz" {
            assert!(snare[4] > 1.2 && snare[12] > 1.2, "{}", t.family);
        }
        // Swing within Linn's straight … triplet at every tempo.
        let mut bpm = 40.0;
        while bpm <= 300.0 {
            let r = t.swing.ratio(bpm);
            assert!((0.5..=0.66).contains(&r), "{} {bpm}", t.family);
            bpm += 5.0;
        }
    }
    assert_eq!(GrooveTemplate::all()["jazz"].swing.grid, Grid::Eighth);
    assert_eq!(GrooveTemplate::all()["song"].swing.grid, Grid::Sixteenth);
}

#[test]
fn provenance_and_voices() {
    let p = GrooveTemplate::provenance();
    assert!(p.license.starts_with("CC BY 4.0") && p.min_hits == 20);
    assert!(p.url.ends_with("groove-v1.0.0-midionly.zip") && p.sha256.len() == 64);
    assert_eq!(Voice::from_note(36), Some(Voice::Kick));
    assert_eq!(Voice::from_note(38), Some(Voice::Snare));
    assert_eq!(Voice::from_note(39), Some(Voice::Snare));
    assert_eq!(Voice::from_note(42), Some(Voice::Hihat));
    assert_eq!(Voice::from_note(46), Some(Voice::OpenHat));
    assert_eq!(Voice::from_note(51), Some(Voice::Ride));
    assert_eq!(Voice::from_note(49), None);
    assert_eq!(
        Voice::ALL.map(Voice::name),
        ["kick", "snare", "hihat", "openHat", "ride"]
    );
}

#[test]
fn swing_is_tempo_scaled_and_clamped() {
    let s = Swing {
        grid: Grid::Sixteenth,
        intercept: 0.3,
        per_bpm: 0.003,
        bpm_range: (80.0, 140.0),
    };
    assert!((s.ratio(100.0) - 0.6).abs() < 1e-9);
    assert!((s.ratio(60.0) - 0.54).abs() < 1e-9); // bpm clamps to 80
    assert_eq!(s.ratio(200.0), 0.66); // 140 -> 0.72, held at triplet
                                      // 120 bpm: a sixteenth is 125 ms, the pair 250 ms; ratio 0.66 -> off-beat 40 ms late.
    assert!((s.delay_ms(120.0) - 0.16 * 250.0).abs() < 1e-9);
    let e = Swing {
        grid: Grid::Eighth,
        intercept: 0.6,
        per_bpm: 0.0,
        bpm_range: (60.0, 200.0),
    };
    assert!((e.delay_ms(120.0) - 0.1 * 500.0).abs() < 1e-9);
}

#[test]
fn prepared_folds_swing_into_the_off_beats() {
    for t in GrooveTemplate::all().values() {
        let (bpm, sr) = (110.0, 48_000.0);
        let p = t.prepared(bpm, sr);
        assert!(p.delay_samples.len() == 5 * 16 && p.velocity_scale.len() == 5 * 16);
        assert_eq!(p.swing_ratio, t.swing.ratio(bpm));
        let swing_samples = t.swing.delay_ms(bpm) / 1000.0 * sr;
        let pair = t.swing.pair_steps() as usize;
        for v in Voice::ALL {
            for step in 0..32 {
                let s = step % 16;
                let swung = s % pair == pair / 2;
                let want =
                    t.offset_ms(v, s) / 1000.0 * sr + if swung { swing_samples } else { 0.0 };
                assert!((p.delay(v, step) - want).abs() < 1e-9);
                assert_eq!(p.velocity(v, step), t.velocity(v, s));
            }
        }
        assert_eq!(
            p.delay(Voice::Kick, 0),
            t.offset_ms(Voice::Kick, 0) / 1000.0 * sr
        );
    }
}
