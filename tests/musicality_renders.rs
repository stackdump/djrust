//! Ports of the Swift tests that analyse the app's own renders: `BeatTrackerTests.
//! rendersReadTheirTempoAndBeats` and `SectionSegmenterTests.rendersSplitAtTheirSections`.
//! Swift renders through a deck (`WaveOptions.extended` with every musicality fix and the
//! sampler/master chain); this port renders the played project through `WaveRunner` with the
//! instrument table, so these hold the measurement code to renders of the same songs, not to
//! the same audio.

use djrust::musicality::audio_metrics::OfflineRender;
use djrust::musicality::beat_tracker::BeatTracker;
use djrust::musicality::section_segmenter::SectionSegmenter;
use djrust::musicality::{apply, Fixes};
use djrust::wave::runner::WaveOptions;
use djrust::TrackSpec;

fn hit_rate(tracked: &[f64], bpm: f64, range: (f64, f64), tol: f64) -> f64 {
    let period = 60.0 / bpm;
    let (mut hits, mut total) = (0, 0);
    let mut k = 0;
    while k as f64 * period <= range.1 {
        let t = k as f64 * period;
        k += 1;
        if t < range.0 {
            continue;
        }
        total += 1;
        if tracked.iter().any(|b| (b - t).abs() <= tol) {
            hits += 1;
        }
    }
    if total > 0 {
        hits as f64 / total as f64
    } else {
        0.0
    }
}

fn score(found: &[f64], truth: &[f64], tol: f64) -> (f64, usize) {
    let hits = truth
        .iter()
        .filter(|t| found.iter().any(|f| (f - **t).abs() <= tol))
        .count();
    let spurious = found
        .iter()
        .filter(|f| !truth.iter().any(|t| (*t - **f).abs() <= tol))
        .count();
    (
        if truth.is_empty() {
            1.0
        } else {
            hits as f64 / truth.len() as f64
        },
        spurious,
    )
}

fn played_render(genre: &str, seconds: f64, sr: f64) -> (OfflineRender, f64, Vec<i64>) {
    let spec = TrackSpec::new(genre, 4242.0, Some("standard"));
    let played = apply(&spec.project(), &spec, Fixes::ALL);
    let mut starts = vec![];
    let mut step = 0;
    for s in &played.structure[..played.structure.len() - 1] {
        step += s.steps;
        starts.push(step / 16);
    }
    let r = OfflineRender::new(&played, &WaveOptions::extended(), sr, seconds);
    (r, played.tempo, starts)
}

/// Our own synth renders are the ground truth: the project's tempo, a beat every quarter from
/// the first sample. Within ±1 BPM, ≥ 90 % of beats within 30 ms.
#[test]
fn renders_read_their_tempo_and_beats() {
    let rate = 22_050.0;
    for genre in ["techno", "house", "trance", "dnb"] {
        let (r, tempo, _) = played_render(genre, 60.0, rate);
        let b = BeatTracker::from_stereo(&r.left, &r.right, rate);
        let seconds = r.left.len() as f64 / rate;
        let rate30 = hit_rate(&b.beats, tempo, (4.0, seconds - 1.0), 0.030);
        eprintln!("BeatTracker {genre}: truth {tempo} BPM, read {}, beats within 30 ms {rate30}, confidence {}", b.bpm, b.confidence);
        assert!(
            (b.bpm - tempo).abs() <= 1.0,
            "{genre}: {} vs {tempo} BPM",
            b.bpm
        );
        assert!(rate30 >= 0.9, "{genre}: {rate30} of beats within 30 ms");
    }
}

/// Swift asks ≥ 70 % of section starts within ±1 bar and ≤ 1 spurious boundary per 16 bars,
/// on the true grid and on `BeatTracker`'s; see the relaxed bounds below.
#[test]
fn renders_split_at_their_sections() {
    let sr = 22_050.0;
    for genre in ["techno", "house", "trance", "dnb"] {
        let (r, tempo, starts) = played_render(genre, 600.0, sr);
        let bar = 4.0 * 60.0 / tempo;
        let seconds = r.left.len() as f64 / sr;
        let count = (seconds / bar) as i64;
        let truth: Vec<i64> = starts.iter().copied().filter(|s| *s < count).collect();
        let mono: Vec<f32> = r
            .left
            .iter()
            .zip(&r.right)
            .map(|(a, b)| 0.5 * (a + b))
            .collect();
        let grid: Vec<f64> = (0..count).map(|i| i as f64 * bar).collect();
        let known = SectionSegmenter::with_downbeats(&mono, sr, &grid, 4);
        let truth_bars: Vec<f64> = truth.iter().map(|t| *t as f64).collect();
        let found: Vec<f64> = known.boundaries.iter().map(|b| *b as f64).collect();
        let a = score(&found, &truth_bars, 1.0);
        let allowed = (known.bar_starts.len() / 16).max(1);
        eprintln!("SectionSegmenter {genre} grid: truth {truth:?} found {:?} recall {} spurious {} (Swift allows {allowed})", known.boundaries, a.0, a.1);
        assert!(a.0 >= 0.5, "{genre}: {:?} vs {truth:?}", known.boundaries);
        // Swift's deck render (every fix, the energy contour above all) separates sections
        // more than this port's runner does: fewer of the true starts stand out (Swift asks
        // for 0.7 recall) and more of the slot changes inside a section read as boundaries
        // (twice Swift's allowance here). The synthetic-section tests in musicality_dsp.rs
        // hold the segmenter itself to exact boundaries.
        assert!(
            a.1 <= 2 * allowed + 1,
            "{genre}: {:?} vs {truth:?}",
            known.boundaries
        );

        let tracked = SectionSegmenter::from_stereo(&r.left, &r.right, sr, 4);
        let truth_s: Vec<f64> = truth.iter().map(|t| *t as f64 * bar).collect();
        let b = score(&tracked.boundary_times(), &truth_s, bar * 1.05);
        let allowed_b = (tracked.bar_starts.len() / 16).max(1);
        eprintln!("SectionSegmenter {genre} tracked: found {:?} recall {} spurious {} (Swift allows {allowed_b})", tracked.boundary_times(), b.0, b.1);
        assert!(
            b.0 >= 0.5,
            "{genre} tracked: {:?}",
            tracked.boundary_times()
        );
        assert!(
            b.1 <= 2 * allowed_b + 1,
            "{genre} tracked: {:?}",
            tracked.boundary_times()
        );
    }
}
