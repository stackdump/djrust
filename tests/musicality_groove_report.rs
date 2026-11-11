//! Port of Swift `GrooveReportTests`: `WaveOptions::record_hits` (`onHit`) and `GrooveReport`
//! (ROADMAP 6.5, exit M6): the hits the runner plays, sample-exact, and what the groove did
//! to them, measured.

use djrust::musicality::groove_report::GrooveReport;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::groove::HitRecord;
use djrust::wave::runner::{WaveOptions, WaveRunner};
use djrust::TrackSpec;

const SR: f64 = 48_000.0;

/// A closed hat on every sixteenth of a 16-step bar, velocity 100.
fn sixteenths(swing: f64, humanize: f64, tempo: f64) -> Project {
    let mut nb = NetBundle::ring(16);
    for step in 0..16 {
        nb.bind(step, 42, 12, 100, 50);
    }
    nb.track.channel = 12;
    let mut p = Project::default();
    p.tempo = tempo;
    p.swing = swing;
    p.humanize = humanize;
    p.nets.insert("hat", nb);
    p
}

/// Render `bars` 4/4 bars of `p` in 256-frame blocks; the hits when `record`.
fn render(
    p: &Project,
    opts: WaveOptions,
    groove: bool,
    bars: i64,
    record: bool,
) -> (Vec<f32>, Vec<HitRecord>) {
    let mut o = opts;
    o.groove = groove;
    o.record_hits = record;
    let mut r = WaveRunner::new(SR, o);
    r.load(p);
    r.play();
    let tempo = if p.tempo > 0.0 { p.tempo } else { 120.0 };
    let mut out = vec![0.0f32; (bars as f64 * 4.0 * 60.0 / tempo * SR) as usize];
    let mut off = 0;
    while off < out.len() {
        let n = 256.min(out.len() - off);
        r.process_at(&mut out[off..off + n], None, n, off as i64);
        off += n;
    }
    (out, r.hits.clone())
}

fn record(step: i64, bar: i64, delay_ms: f64, velocity: i64, kind: &str) -> HitRecord {
    let tick = bar * 16 + step + 1;
    let grid = ((tick - 1) as f64 * 0.125 * SR) as i64;
    HitRecord {
        frame: grid + (delay_ms / 1000.0 * SR) as i64,
        grid_frame: grid,
        tick,
        net: "hat".into(),
        note: 42,
        channel: 12,
        kind: kind.into(),
        velocity,
    }
}

/// The record says what played where: every tick in order, the off-beat sixteenths late by
/// swing% × half a sixteenth, the beats on the grid.
#[test]
fn records_are_sample_exact() {
    let (_, hits) = render(
        &sixteenths(50.0, 0.0, 120.0),
        WaveOptions::default(),
        true,
        4,
        true,
    );
    assert!(hits.len() >= 60, "{} hits", hits.len());
    let tick_frames = 0.125 * SR;
    for h in &hits {
        assert!(h.net == "hat" && h.note == 42 && h.channel == 12 && h.kind == "hat");
        assert_eq!(h.velocity, 100);
        assert!(h.is_drum());
        let late_ms = h.delay_frames() as f64 / SR * 1000.0;
        assert!(
            (late_ms - if h.step() % 2 == 1 { 31.25 } else { 0.0 }).abs() < 0.05,
            "tick {}: {late_ms} ms",
            h.tick
        );
    }
    let ticks: Vec<i64> = hits.iter().map(|h| h.tick).collect();
    let mut sorted = ticks.clone();
    sorted.sort();
    assert_eq!(ticks, sorted);
    let first = &hits[0];
    for h in &hits {
        let want = first.grid_frame as f64 + (h.tick - first.tick) as f64 * tick_frames;
        assert!(
            (h.grid_frame as f64 - want).abs() <= 1.0,
            "tick {} at {}",
            h.tick,
            h.grid_frame
        );
    }
}

/// Measured swing matches the theory within 0.5 %, at two tempi.
#[test]
fn measured_swing_matches_theory() {
    for swing in [10.0, 30.0, 50.0, 66.0, 90.0] {
        for tempo in [92.0, 128.0] {
            let (_, hits) = render(
                &sixteenths(swing, 0.0, tempo),
                WaveOptions::default(),
                true,
                4,
                true,
            );
            let report = GrooveReport::new(&hits, SR, tempo);
            let theory = 0.5 + swing / 400.0;
            let measured = report.swing_ratio.expect("swing ratio");
            assert!(
                (measured - theory).abs() / theory < 0.005,
                "swing {swing} @{tempo}: {measured}"
            );
            assert!(report.consistency > 0.999);
            assert_eq!(report.distinct_velocities_per_position, 1.0);
        }
    }
}

/// Without the groove every hit sits on its tick: straight, nothing late.
#[test]
fn straight_is_on_the_grid() {
    let (_, hits) = render(
        &sixteenths(50.0, 40.0, 120.0),
        WaveOptions::default(),
        false,
        4,
        true,
    );
    let report = GrooveReport::new(&hits, SR, 120.0);
    assert!(report.hits > 0);
    assert!(hits.iter().all(|h| h.delay_frames() == 0));
    assert_eq!(report.swing_ratio, Some(0.5));
    assert_eq!(report.microtiming_ms, 0.0);
    assert_eq!(report.consistency, 1.0);
}

/// Humanize moves the velocities and the timing from bar to bar.
#[test]
fn humanize_shows_in_the_report() {
    let (_, hits) = render(
        &sixteenths(0.0, 60.0, 120.0),
        WaveOptions::default(),
        true,
        8,
        true,
    );
    let report = GrooveReport::new(&hits, SR, 120.0);
    assert!(report.distinct_velocities_per_position > 2.0, "{report:?}");
    assert!(report.consistency < 0.9, "{report:?}");
    assert!(
        report.microtiming_ms > 1.0 && report.microtiming_sd_ms > 1.0,
        "{report:?}"
    );
    assert_eq!(report.voices["hat"].hits, report.hits);
}

/// The hook observes: a render with it is bit-identical to one without.
#[test]
fn the_hook_does_not_change_the_render() {
    let mut spec = TrackSpec::new("jazz", 42.0, Some("minimal")).project();
    spec.swing = 60.0;
    spec.humanize = 40.0;
    let (hooked, hits) = render(&spec, WaveOptions::extended(), true, 8, true);
    let (plain, _) = render(&spec, WaveOptions::extended(), true, 8, false);
    assert!(hits.len() > 100);
    assert!(hooked == plain);
    assert!(hits.iter().any(|h| h.kind == "tonal"));
}

/// The report's arithmetic on hand-made hits.
#[test]
fn report_arithmetic() {
    let mut hits = vec![];
    for bar in 0..2 {
        for step in 0..16 {
            hits.push(record(
                step,
                bar,
                if step % 2 == 1 { 20.0 } else { 0.0 },
                if step == 0 { 90 + 20 * bar } else { 100 },
                "hat",
            ));
        }
    }
    hits.push(record(3, 0, 5.0, 64, "tonal"));
    let r = GrooveReport::new(&hits, SR, 120.0);
    assert!(r.hits == 33 && r.drum_hits == 32);
    assert!((r.distinct_velocities_per_position - 17.0 / 16.0).abs() < 1e-9);
    assert!((r.consistency - 1.0).abs() < 1e-9);
    let off_mean = (16.0 * 20.0 + 5.0) / 17.0;
    assert!((r.swing_ratio.unwrap() - (0.5 + off_mean / 250.0)).abs() < 1e-4);
    assert_eq!(r.voices["tonal"].hits, 1);
    assert!((r.voices["hat"].mean_delay_ms - 10.0).abs() < 0.01);
    assert!(GrooveReport::new(&[], SR, 120.0).swing_ratio.is_none());
}
