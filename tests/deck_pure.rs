//! The deck-level pure logic: tempo rules and the BPM analyzer (`DeckBeatTests`,
//! `BeatGridTests`), the turntable (`TurntableTests`), waveform peaks (`WaveformTests`), the
//! section timeline and timeline dots (`SectionTimelineTests`, `TimelineDotsTests`), ring
//! snapshots (`NetSnapshotTests`), the loop row's rules (`LoopLaunchTests`, `LoopRetuneTests`)
//! and stinger launch. No Swift on this box: every expectation is a Swift test literal or a
//! value worked out from the Swift source (marked where it is).

use std::collections::BTreeMap;
use std::sync::Arc;

use djrust::deck::beat::*;
use djrust::deck::beat_grid::{analyze, BeatGrid};
use djrust::deck::demo_loop;
use djrust::deck::loops::*;
use djrust::deck::snapshot::*;
use djrust::deck::stinger::*;
use djrust::deck::timeline::*;
use djrust::deck::turntable::*;
use djrust::deck::waveform::*;
use djrust::json::JsonValue;
use djrust::pflow::NetBundle;
use djrust::session::types::LoopFx;
use djrust::sound::stinger::{resolve, AssignmentKind, StingerAssignment, StingerContent};
use djrust::sound::PaletteSet;
use djrust::TrackSpec;

// MARK: BeatGridTests

/// A synthetic track: a kick on every beat, quieter hats on the offbeats; `lead` seconds of
/// silence first.
fn track(bpm: f64, beats: usize, lead: f64, sr: f64) -> Vec<f32> {
    let spb = sr * 60.0 / bpm;
    let n = (lead * sr + beats as f64 * spb) as usize;
    let mut l = vec![0.0f32; n];
    let mut seed: u32 = 12345;
    let mut noise = move || -> f32 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    for b in 0..beats {
        let start = (lead * sr + b as f64 * spb) as usize;
        for i in 0..(0.12 * sr) as usize {
            if start + i >= n {
                break;
            }
            let t = i as f64 / sr;
            l[start + i] += ((2.0 * std::f64::consts::PI * (50.0 + 120.0 * (-t * 40.0).exp()) * t)
                .sin()
                * (-t * 18.0).exp()) as f32
                * 0.9;
        }
        let hat = start + (spb / 2.0) as usize;
        for i in 0..(0.04 * sr) as usize {
            if hat + i >= n {
                break;
            }
            l[hat + i] += noise() * (-(i as f64) / sr * 90.0).exp() as f32 * 0.3;
        }
    }
    l
}

fn phase_error(got: f64, want: f64, spb: f64) -> f64 {
    let mut d = (got - want) % spb;
    if d > spb / 2.0 {
        d -= spb;
    }
    if d < -spb / 2.0 {
        d += spb;
    }
    d.abs()
}

#[test]
fn the_analyzer_finds_tempo_and_phase() {
    let sr = 22_050.0;
    let lead = 0.1;
    for bpm in [90.0, 100.0, 120.0, 128.0, 140.0, 150.0, 174.0] {
        let l = track(bpm, 48, lead, sr);
        let grid = analyze(&l, &l, sr).unwrap_or_else(|| panic!("{bpm}: no grid"));
        assert!((grid.bpm - bpm).abs() < 0.5, "{bpm}: got {}", grid.bpm);
        let spb = sr * 60.0 / bpm;
        assert!(
            phase_error(grid.offset, lead * sr, spb) < 0.012 * sr,
            "{bpm}: offset {} vs {}",
            grid.offset,
            lead * sr
        );
    }
}

#[test]
fn the_demo_loop_is_120() {
    let sr = 22_050.0;
    let one = demo_loop::make(sr);
    let mut l = Vec::new();
    for _ in 0..4 {
        l.extend_from_slice(&one.left);
    }
    let grid = analyze(&l, &l, sr).unwrap();
    assert!((grid.bpm - demo_loop::BPM).abs() < 1.0, "got {}", grid.bpm);
    assert!(phase_error(grid.offset, 0.0, sr * 60.0 / 120.0) < 0.015 * sr);
}

#[test]
fn silence_and_noise_give_nothing_useful() {
    let z = vec![0.0f32; 100_000];
    assert!(analyze(&z, &z, 22_050.0).is_none());
    let short = vec![0.1f32; 200];
    assert!(analyze(&short, &short, 22_050.0).is_none());
}

#[test]
fn the_analyzer_fills_in_the_grid_of_a_click_track() {
    // DeckBeatTests.theAnalyzerFillsInTheGridOfALoadedBuffer
    let sr = 48_000.0;
    let spb = sr * 60.0 / 132.0;
    let mut l = vec![0.0f32; (spb * 40.0) as usize];
    for b in 0..40 {
        let s = (b as f64 * spb) as usize;
        for i in 0..(0.08 * sr) as usize {
            if s + i < l.len() {
                let t = i as f64 / sr;
                l[s + i] =
                    ((2.0 * std::f64::consts::PI * 60.0 * t).sin() * (-t * 25.0).exp()) as f32;
            }
        }
    }
    let g = analyze(&l, &l, sr).unwrap();
    assert!((g.bpm - 132.0).abs() < 0.5, "{}", g.bpm);
}

#[test]
fn the_demo_loop_is_deterministic() {
    let a = demo_loop::make(8000.0);
    let b = demo_loop::make(8000.0);
    assert_eq!(a.length(), 8000 * 4);
    assert_eq!(a, b);
    assert!(
        a.left.iter().any(|x| x.abs() > 0.1),
        "the kick on beat 1 is loud"
    );
}

// MARK: DeckBeatTests (the arithmetic)

#[test]
fn sync_matches_tempo_within_the_fader_range() {
    let m = tempo_match(120.0, 128.0);
    assert!(!m.clamped && m.mult == 1.0);
    let r = sync_to(120.0, 128.0, None, None, SyncAlign::TempoOnly);
    assert!(!r.tempo_clamped && r.multiplier == 1.0);
    assert!((effective_bpm(120.0, r.pitch, 0.0) - 128.0).abs() < 1e-9);
    assert!((r.pitch - (128.0 / 120.0 - 1.0) / PITCH_RANGE).abs() < 1e-9);
}

#[test]
fn sync_clamps_when_the_tempi_are_too_far_apart() {
    let r = sync_to(120.0, 150.0, None, None, SyncAlign::TempoOnly);
    assert!(r.tempo_clamped && (r.pitch - 1.0).abs() < 1e-9);
    assert!((effective_bpm(120.0, r.pitch, 0.0) - 120.0 * 1.08).abs() < 1e-9);
}

#[test]
fn sync_picks_half_or_double_time_when_that_fits() {
    let r = sync_to(70.0, 140.0, None, None, SyncAlign::TempoOnly);
    assert!(
        r.multiplier == 0.5 && !r.tempo_clamped && r.pitch == 0.0,
        "{r:?}"
    );
    assert!((effective_bpm(70.0, r.pitch, 0.0) - 70.0).abs() < 1e-9);
}

fn wrapped(d: f64, period: f64) -> f64 {
    d - (d / period).round() * period
}

#[test]
fn sync_aligns_the_bar_or_the_beat() {
    // two net decks at the same tempo: b is 1.3 beats ahead of a (mod a bar)
    let me = BeatState {
        bpm: 128.0,
        beat: 9.3,
    };
    let you = BeatState {
        bpm: 128.0,
        beat: 8.0,
    };
    let r = sync_to(128.0, 128.0, Some(me), Some(you), SyncAlign::Bar);
    // err = 1.3 → wrapped to [-2, 2] → 1.3: move back 1.3 beats
    assert!((r.shifted_beats + 1.3).abs() < 1e-9, "{r:?}");
    assert!(wrapped(me.beat + r.shifted_beats - you.beat, 4.0).abs() < 1e-9);
    let rb = sync_to(128.0, 128.0, Some(me), Some(you), SyncAlign::Beat);
    assert!((rb.shifted_beats + 0.3).abs() < 1e-9, "{rb:?}");
    // a half-time deck moves in its own beats
    let half = sync_to(
        70.0,
        140.0,
        Some(BeatState {
            bpm: 70.0,
            beat: 3.0,
        }),
        Some(BeatState {
            bpm: 140.0,
            beat: 6.0,
        }),
        SyncAlign::Beat,
    );
    // mine = 3 · 0.5 = 1.5, err = 1.5 − 6 = −4.5, rounds away from zero (−5) → 0.5;
    // shifted = −0.5 / 0.5 = −1 beat of its own
    assert_eq!(half.multiplier, 0.5);
    assert!((half.shifted_beats + 1.0).abs() < 1e-9, "{half:?}");
}

#[test]
fn the_beat_state_reads_the_clock() {
    // netBeatPositionTracksTheClock: tick t, a quarter into it
    assert_eq!(BeatState::of_net(120.0, 0, 0.5).beat, 0.0);
    let s = BeatState::of_net(120.0, 9, 0.0);
    assert_eq!(s.beat, 2.0);
    assert!(s.beat_in_bar() == 2 && s.bar() == 0);
    let s = BeatState::of_net(120.0, 22, 0.0);
    assert!(
        s.bar() == 1 && s.beat_in_bar() == 1 && (s.bar_phase() - 0.3125).abs() < 1e-12,
        "{s:?}"
    );
    // a sample deck on the grid: beat = (p − offset) / samplesPerBeat
    let g = BeatGrid::new(120.0, 100.0, 1.0);
    let spb = g.samples_per_beat(1000.0);
    assert_eq!(spb, 500.0);
    let s = BeatState::of_sample(120.0, &g, 100.0 + 2.5 * spb, 1.0e6, 1000.0);
    assert!((s.beat - 2.5).abs() < 1e-12);
    // a negative playhead wraps
    let s = BeatState::of_sample(120.0, &g, -400.0, 10_000.0, 1000.0);
    assert!((s.beat - (9600.0 - 100.0) / 500.0).abs() < 1e-12);
}

#[test]
fn quantize_snaps_cues_and_loops_to_the_grid() {
    // QuantizeTests: the demo at 120 bpm, beat at sample 0 → 0.5 s per beat
    let sr = 48_000.0;
    let g = BeatGrid::new(120.0, 0.0, 1.0);
    let spb = sr * 0.5;
    // off: set where we are
    assert_eq!(
        snapped(spb * 3.0 + 4000.0, Quantize::Off, Some(&g), sr),
        spb * 3.0 + 4000.0
    );
    // beat: snapped to the nearest beat
    assert_eq!(
        snapped(spb * 3.0 + 4000.0, Quantize::Beat, Some(&g), sr),
        spb * 3.0
    );
    // bar: nearest bar line (4 beats)
    assert_eq!(snapped(spb * 5.2, Quantize::Bar, Some(&g), sr), spb * 4.0);
    assert_eq!(snapped(123.0, Quantize::Beat, None, sr), 123.0);
    // loopBeats: starts on the nearest beat at or before, exactly `beats` long
    let (s, e) = loop_beats(4.0, spb * 1.2 + 500.0, Quantize::Beat, &g, sr);
    assert!(s == spb && e == spb * 5.0);
    // loopHalve / double keep the grid: the end is on a beat line either way
    assert_eq!(
        snapped(spb * 4.0 + 9000.0, Quantize::Beat, Some(&g), sr),
        spb * 4.0
    );
}

// MARK: TurntableTests

fn ramp(n: usize) -> Arc<SampleBuffer> {
    Arc::new(SampleBuffer::new(
        (0..n).map(|i| i as f32).collect(),
        None,
        1000.0,
    ))
}

fn run(tt: &mut Turntable, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames];
    tt.render(frames, &mut out, None);
    out
}

#[test]
fn a_turntable_is_silent_when_empty() {
    let mut tt = Turntable::new(1000.0);
    assert!(run(&mut tt, 16).iter().all(|x| *x == 0.0));
}

#[test]
fn the_motor_spins_up_to_unity_rate() {
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(100_000));
    tt.send(TurntableCommand::Transport(true));
    run(&mut tt, 2000); // 2 s >> 45 ms motor time constant
    assert!((tt.current_rate() - 1.0).abs() < 1e-6);
    // Lost ground during spin-up is about one time constant (45 samples).
    assert!(
        (tt.position() - (2000.0 - 45.0)).abs() < 2.0,
        "{}",
        tt.position()
    );
}

#[test]
fn nudge_changes_rate() {
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(100_000));
    tt.send(TurntableCommand::Transport(true));
    tt.send(TurntableCommand::Nudge(0.1));
    run(&mut tt, 2000);
    assert!((tt.current_rate() - 1.1).abs() < 1e-6);
}

#[test]
fn scrub_there_and_back_returns_to_start() {
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(100_000));
    tt.send(TurntableCommand::Transport(true));
    run(&mut tt, 500);
    let start = tt.position(); // freeze applies before the next sample, so this is the anchor
    tt.send(TurntableCommand::Freeze);
    for _ in 0..20 {
        tt.send(TurntableCommand::Scrub(7.0));
    }
    run(&mut tt, 50);
    for _ in 0..20 {
        tt.send(TurntableCommand::Scrub(-7.0));
    }
    run(&mut tt, 3000);
    assert!((tt.position() - start).abs() < 0.5);
    assert!(tt.current_rate().abs() < 1e-3);
}

#[test]
fn output_is_the_interpolated_buffer() {
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(100_000));
    tt.send(TurntableCommand::Transport(true));
    let out = run(&mut tt, 1500);
    // Once at speed, output tracks position: consecutive samples differ by ~1.
    assert!(((out[1499] - out[1498]) - 1.0).abs() < 1e-3);
}

#[test]
fn it_wraps_backwards_past_zero() {
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(1000));
    tt.send(TurntableCommand::Freeze);
    tt.send(TurntableCommand::Scrub(-100.0));
    let out = run(&mut tt, 2000);
    assert!(tt.position() < -99.0);
    assert!((out[1999] - 900.0).abs() < 1.0); // -100 wraps to 900
}

#[test]
fn a_swapped_buffer_is_handed_back_to_be_freed_off_the_audio_thread() {
    let mut tt = Turntable::new(1000.0);
    let first = ramp(10);
    tt.load(first.clone());
    run(&mut tt, 1);
    assert_eq!(Arc::strong_count(&first), 2, "held by the turntable");
    tt.load(ramp(10));
    run(&mut tt, 1); // the render swaps and retires
    let retired = tt.drain_retired();
    assert_eq!(retired.len(), 1);
    drop(retired);
    assert_eq!(Arc::strong_count(&first), 1);
}

#[test]
fn a_loop_wraps_the_playhead() {
    let mut tt = Turntable::new(1000.0);
    tt.load(Arc::new(SampleBuffer::new(
        (0..100_000).map(|i| i as f32).collect(),
        None,
        1000.0,
    )));
    tt.send(TurntableCommand::Transport(true));
    tt.send(TurntableCommand::Loop {
        start: 100.0,
        end: 300.0,
    });
    run(&mut tt, 5000);
    assert!(tt.position() >= 100.0 && tt.position() < 300.0);
    tt.send(TurntableCommand::LoopOff);
    run(&mut tt, 1000);
    assert!(tt.position() > 1000.0);
}

#[test]
fn a_swap_keeps_the_phase_and_scales_the_loop() {
    // worked from `apply(.swap)`: the playhead and the loop region scale with the length
    let mut tt = Turntable::new(1000.0);
    tt.load(ramp(1000));
    tt.send(TurntableCommand::Seek(250.0));
    tt.send(TurntableCommand::Loop {
        start: 100.0,
        end: 300.0,
    });
    run(&mut tt, 1);
    tt.swap(ramp(2000), 0.0);
    run(&mut tt, 1);
    assert_eq!(tt.length(), 2000);
    assert_eq!(tt.position(), 500.0, "250 of 1000 is 500 of 2000");
    // gain: nothing at unity, a glide otherwise, `now` jumps
    let mut g = Turntable::new(1000.0);
    g.load(Arc::new(SampleBuffer::new(vec![1.0; 100], None, 1000.0)));
    g.send(TurntableCommand::Gain(0.5, true));
    let out = run(&mut g, 4);
    assert_eq!(out, [0.5; 4]);
    // skip moves a stopped-motor deck nowhere, a running one on at its rate
    g.send(TurntableCommand::Start);
    run(&mut g, 1);
    let p = g.position();
    g.skip(10.0);
    assert!((g.position() - p - 10.0 * 1.0).abs() < 1e-9);
}

// MARK: WaveformTests

fn buf(f: impl Fn(usize) -> f32, n: usize) -> SampleBuffer {
    SampleBuffer::new((0..n).map(f).collect(), None, 1000.0)
}

#[test]
fn peaks_of_known_signals() {
    // 4 columns over 4000 samples: loud, silent, quiet-negative, loud-both-signs
    let b = buf(
        |i| match i / 1000 {
            0 => 0.8,
            1 => 0.0,
            2 => -0.3,
            _ => {
                if i % 2 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
        },
        4000,
    );
    let w = WaveformOverview::new(&b, 4);
    assert!(w.columns == 4 && w.peaks.len() == 8);
    assert_eq!(w.peak(0), (0.8, 0.8));
    assert_eq!(w.peak(1), (0.0, 0.0));
    assert_eq!(w.peak(2), (-0.3, -0.3));
    assert_eq!(w.peak(3), (-1.0, 1.0));
}

#[test]
fn stereo_is_mixed_to_mono() {
    let b = SampleBuffer::new(vec![1.0; 100], Some(vec![-1.0; 100]), 1000.0);
    assert_eq!(WaveformOverview::new(&b, 2).peak(0), (0.0, 0.0));
}

#[test]
fn more_columns_than_samples_still_covers_the_track() {
    let b = buf(|i| i as f32 / 10.0, 10);
    let w = WaveformOverview::new(&b, 40);
    assert_eq!(w.peaks.len(), 80);
    assert_eq!(w.peak(39).1, 0.9);
    assert!((0..40).all(|c| w.peak(c).0 <= w.peak(c).1));
}

#[test]
fn windows_zoom_into_the_samples() {
    let b = buf(|i| (i % 1000) as f32 / 1000.0, 4000);
    let win = WaveformOverview::window(&b, 1000, 2000, 2);
    assert!(win[0] == 0.0 && (win[1] - 0.499).abs() < 1e-6);
    assert!((win[3] - 0.999).abs() < 1e-6);
    // out of range clamps instead of crashing
    let edge = WaveformOverview::window(&b, -500, 99_999, 4);
    assert_eq!(edge.len(), 8);
    assert_eq!(WaveformOverview::window(&b, 10, 10, 3), vec![0.0f32; 6]);
}

#[test]
fn the_overlay_places_playhead_bars_cues_and_loop() {
    // DeckOverlayTests: the demo (8 beats at 120 bpm: two bars, offset 0), playhead at 0.75
    let sr = 22_050.0;
    let demo = demo_loop::make(sr);
    let len = demo.length();
    let grid = BeatGrid::new(120.0, 0.0, 1.0);
    let mut cues = BTreeMap::new();
    cues.insert(1, len as f64 * 0.25);
    let o = WaveformOverlay::of_sample_deck(
        len,
        len as f64 * 0.75,
        Some(len as f64 * 0.25),
        Some(len as f64 * 0.75),
        true,
        &cues,
        Some(&grid),
        sr,
    )
    .unwrap();
    assert!((o.playhead - 0.75).abs() < 1e-3);
    assert_eq!(o.bar_lines, [0.0, 0.5]);
    assert!((o.cues[&1] - 0.25).abs() < 1e-3);
    let (a, b) = o.loop_range.unwrap();
    assert!((a - 0.25).abs() < 1e-3 && (b - 0.75).abs() < 1e-3 && o.loop_active);
    assert!(WaveformOverlay::of_sample_deck(0, 0.0, None, None, false, &cues, None, sr).is_none());
    // a grid too dense to read draws no bar lines
    let dense = BeatGrid::new(400.0, 0.0, 1.0);
    let o2 =
        WaveformOverlay::of_sample_deck(len * 400, 0.0, None, None, false, &cues, Some(&dense), sr)
            .unwrap();
    assert!(o2.bar_lines.is_empty());
}

// MARK: SectionTimelineTests

#[test]
fn clicks_land_on_the_right_section() {
    let t = SectionTimeline {
        segments: vec![
            Segment::new("intro", 0.0, 0.2, false),
            Segment::new("drop", 0.2, 0.5, true),
            Segment::new("outro", 0.7, 0.3, false),
        ],
        playhead: 0.3,
        total_ticks: 100,
    };
    assert_eq!(t.index_at_fraction(0.0), Some(0));
    assert!(t.index_at_fraction(0.1999) == Some(0) && t.index_at_fraction(0.2) == Some(1));
    assert!(t.index_at_fraction(0.69) == Some(1) && t.index_at_fraction(0.7) == Some(2));
    assert!(t.index_at_fraction(1.5) == Some(2) && t.index_at_fraction(-3.0) == Some(0));
    let empty = SectionTimeline {
        segments: vec![],
        playhead: 0.0,
        total_ticks: 0,
    };
    assert_eq!(empty.index_at_fraction(0.5), None);
}

#[test]
fn hue_is_stable_per_name() {
    assert_eq!(SectionTimeline::hue("drop"), SectionTimeline::hue("drop"));
    assert_ne!(
        SectionTimeline::hue("drop"),
        SectionTimeline::hue("breakdown")
    );
    assert!((0.0..1.0).contains(&SectionTimeline::hue("verse")));
    // FNV-1a/32 of "drop" mod 360 (worked from the source: h = 2166136261, ^byte, ·16777619)
    let mut h: u32 = 2_166_136_261;
    for b in b"drop" {
        h = (h ^ *b as u32).wrapping_mul(16_777_619);
    }
    assert_eq!(SectionTimeline::hue("drop"), (h % 360) as f64 / 360.0);
}

#[test]
fn a_composed_song_lays_out_contiguous_segments_that_follow_the_clock() {
    // segmentsCoverTheSongAndFollowTheClock, off a composed project's structure
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let secs = SectionInfo::sections_of(&spec.project());
    assert!(!secs.is_empty());
    let t0 = SectionTimeline::of_sections(&secs, 0, 0.0).unwrap();
    assert!(t0.segments.len() == secs.len() && t0.segments[0].start == 0.0);
    assert!((t0.segments.iter().map(|s| s.width).sum::<f64>() - 1.0).abs() < 1e-9);
    for w in t0.segments.windows(2) {
        assert!(
            (w[1].start - (w[0].start + w[0].width)).abs() < 1e-9,
            "contiguous"
        );
    }
    assert!(t0.segments[0].is_current && t0.playhead == 0.0);
    let mid = t0.total_ticks / 2;
    let t1 = SectionTimeline::of_sections(&secs, mid, 0.5).unwrap();
    assert!(t1.playhead > 0.0 && t1.playhead < 1.0);
    assert_eq!(t1.segments.iter().filter(|s| s.is_current).count(), 1);
    let cur = t1.segments.iter().position(|s| s.is_current).unwrap();
    assert!(
        t1.segments[cur].start <= t1.playhead
            && t1.playhead < t1.segments[cur].start + t1.segments[cur].width + 1e-9
    );
    // a loop has no sections
    assert!(SectionTimeline::of_sections(
        &SectionInfo::sections_of(&TrackSpec::new("techno", 42.0, None).project()),
        0,
        0.0
    )
    .is_none());
}

#[test]
fn segments_label_their_phrases_and_colour_their_names() {
    let mut s = Segment::new("verse", 0.0, 0.5, true);
    assert!(!s.has_phrases() && s.phrase_label().is_none() && s.dividers().is_empty());
    s.phrase_count = 4;
    s.phrase_pattern = ["A", "B", "A", "B"].iter().map(|x| x.to_string()).collect();
    assert_eq!(s.phrase_label().as_deref(), Some("ABAB"));
    assert_eq!(s.dividers(), [0.25, 0.5, 0.75]);
    use djrust::deck::timeline::label_layout::run;
    // the letters go only if the whole run fits with air
    let r = run(100.0, 30.0, Some(20.0));
    assert_eq!((r.name_x, r.letters_x), (23.0, Some(57.0)));
    let tight = run(50.0, 30.0, Some(20.0));
    assert_eq!((tight.name_x, tight.letters_x), (10.0, None));
    // beats' five names keep beats' colours, dimmed when resting
    let c = SectionTimeline::color("chorus", true);
    assert_eq!(c, Rgb::hex(0xe94560));
    let dim = SectionTimeline::color("chorus", false);
    assert!((dim.r - c.r * 0.45 / 0.95).abs() < 1e-12);
    // any other name takes its hash hue
    let d = SectionTimeline::color("drop", true);
    assert_eq!(
        d,
        SectionTimeline::hsb(SectionTimeline::hue("drop"), 0.55, 0.95)
    );
    assert_eq!(
        SectionTimeline::hsb(0.0, 0.0, 1.0),
        Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0
        }
    );
}

// MARK: TimelineDotsMathTests

fn red() -> Rgb {
    Rgb::hex(0xe94560)
}

fn dots(ages_ms: &[f64], now: f64) -> TimelineDots {
    let mut h = TimelineDots::new();
    let mut ages = ages_ms.to_vec();
    ages.sort_by(|a, b| b.partial_cmp(a).unwrap());
    for a in ages {
        h.add(TimelineDot {
            time: now - a / 1000.0,
            color: red(),
        });
    }
    h
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_window_is_at_least_two_seconds_and_never_more_than_four_bars() {
    assert_eq!(TimelineDots::new().window_ms(5.0, 120.0), Some(2000.0));
    assert_eq!(geometry::max_window_ms(300.0), 800.0);
    assert_eq!(dots(&[5000.0], 100.0).window_ms(100.0, 300.0), Some(2000.0));
    assert_eq!(geometry::max_window_ms(120.0), 2000.0);
    assert_eq!(dots(&[1000.0], 100.0).window_ms(100.0, 120.0), Some(2000.0));
    assert_eq!(geometry::max_window_ms(60.0), 4000.0);
    assert_eq!(dots(&[1000.0], 100.0).window_ms(100.0, 60.0), Some(2000.0));
    assert_eq!(
        dots(&[3000.0, 10.0], 100.0).window_ms(100.0, 60.0),
        Some(3500.0)
    );
    assert_eq!(dots(&[5000.0], 100.0).window_ms(100.0, 60.0), Some(4000.0));
    assert_eq!(geometry::max_window_ms(20.0), 4000.0);
    assert_eq!(geometry::max_window_ms(0.0), 4000.0);
}

#[test]
fn a_tempo_that_is_not_a_number_draws_nothing() {
    let h = dots(&[100.0], 100.0);
    assert_eq!(h.window_ms(100.0, f64::NAN), None);
    assert!(h.layout(100.0, 400.0, f64::NAN).is_empty());
    let mut p = h.clone();
    p.prune(1000.0, f64::NAN);
    assert_eq!(p.count(), 1);
}

#[test]
fn position_alpha_and_streak_follow_the_age() {
    let h = dots(&[2000.0, 1850.0, 1700.0, 1000.0, 0.0], 100.0);
    let l = h.layout(100.0, 400.0, 120.0);
    assert_eq!(l.len(), 5);
    let x: Vec<f64> = l.iter().map(|d| d.x).collect();
    let a: Vec<f64> = l.iter().map(|d| d.alpha).collect();
    assert!(
        near(x[0], 0.0)
            && near(x[1], 30.0)
            && near(x[2], 60.0)
            && near(x[3], 200.0)
            && near(x[4], 400.0)
    );
    assert!(near(a[0], 0.0) && near(a[1], 0.35) && near(a[2], 0.7) && near(a[3], 0.7));
    assert!(near(a[4], 0.7));
    assert!(l.iter().all(|d| d.streak_length == 20.0));
    assert!(l.iter().all(|d| d.color == red()));
    assert!(near(h.layout(100.0, 100.0, 120.0)[0].streak_length, 6.0));
}

#[test]
fn the_fade_is_linear_from_eighty_five_percent() {
    assert!(near(TimelineDots::alpha_at_fraction(0.0), 0.7));
    assert!(near(TimelineDots::alpha_at_fraction(0.8499), 0.7));
    assert!(near(TimelineDots::alpha_at_fraction(0.85), 0.7));
    assert!(near(TimelineDots::alpha_at_fraction(0.925), 0.35));
    assert!(near(
        TimelineDots::alpha_at_fraction(0.95),
        0.7 * (1.0 - 0.1 / 0.15)
    ));
    assert!(near(TimelineDots::alpha_at_fraction(1.0), 0.0));
}

#[test]
fn dots_older_than_the_window_are_not_drawn_and_are_pruned() {
    let mut h = dots(&[2500.0, 2001.0, 2000.0, 500.0], 100.0);
    assert_eq!(
        h.layout(100.0, 400.0, 120.0).len(),
        2,
        "2000 is in, 2001 is out"
    );
    h.prune(100.0, 120.0);
    assert_eq!(h.count(), 2);
    assert!(near(h.dots()[0].time, 100.0 - 2.0) && near(h.dots()[1].time, 100.0 - 0.5));
}

#[test]
fn the_strip_is_as_tall_as_the_dots_need_and_no_taller() {
    use djrust::deck::timeline::geometry as g;
    assert_eq!(g::STRIP_HEIGHT, 12.0);
    assert!(g::HEAD_Y - g::HEAD_RADIUS == 2.0 && g::HEAD_Y + g::HEAD_RADIUS == 10.0);
    assert!(g::STREAK_Y >= 0.0 && g::STREAK_Y + g::STREAK_HEIGHT <= g::STRIP_HEIGHT);
    assert!(g::HEAD_Y - g::HEAD_RADIUS >= 0.0 && g::HEAD_Y + g::HEAD_RADIUS <= g::STRIP_HEIGHT);
}

#[test]
fn the_history_keeps_two_hundred_dots_and_the_constants_are_pinned() {
    use djrust::deck::timeline::geometry as g;
    let mut h = TimelineDots::new();
    for i in 0..250 {
        h.add(TimelineDot {
            time: i as f64,
            color: red(),
        });
    }
    assert_eq!(h.count(), 200);
    assert!(h.dots().first().unwrap().time == 50.0 && h.dots().last().unwrap().time == 249.0);
    h.remove_all();
    assert!(h.is_empty());
    assert!(g::CAP == 200 && g::MIN_WINDOW_MS == 2000.0);
    assert!(g::WINDOW_PAD_MS == 500.0 && g::FADE_START == 0.85);
    assert!(g::BASE_ALPHA == 0.7 && g::STREAK_MAX == 20.0 && g::STREAK_PER_WIDTH == 120.0);
    assert!(
        g::STRIP_TOP == 24.0
            && g::STREAK_Y + g::STRIP_TOP == 27.0
            && g::HEAD_Y + g::STRIP_TOP == 30.0
    );
    assert!(g::STREAK_HEIGHT == 6.0 && g::HEAD_RADIUS == 4.0);
}

// MARK: NetSnapshotTests

#[test]
fn only_single_token_rings_qualify() {
    let nb = NetBundle::ring(8);
    assert_eq!(RingShape::of(&nb).map(|s| s.size), Some(8));
    let mut inhibited = NetBundle::ring(8);
    inhibited.arcs[3].inhibit = true;
    assert!(RingShape::of(&inhibited).is_none());
    let mut two = NetBundle::ring(8);
    two.places.get_mut("p3").unwrap().initial = vec![1.0];
    assert!(RingShape::of(&two).is_none());
    assert!(RingShape::of(&NetBundle::default()).is_none());
}

#[test]
fn rings_carry_their_hits_and_roles() {
    let p = TrackSpec::new("techno", 42.0, None).project();
    let kick = RingShape::of(p.nets.get("kick").unwrap()).unwrap();
    assert!(
        kick.size == 16 && kick.hits.iter().filter(|h| **h).count() == 4,
        "techno kick: 4 on the floor"
    );
    assert_eq!(
        RingShape::of(p.nets.get("harmony").unwrap()).unwrap().size,
        64
    );
    // velocities where there are hits, zero elsewhere
    assert!(kick
        .hits
        .iter()
        .zip(&kick.velocities)
        .all(|(h, v)| if *h { *v > 0 } else { *v == 0 }));
    // the token sits on tick mod n, the flash on the step before
    let v = kick.view("kick", "kick", 0, 0.0, false);
    assert!(v.token == 0 && v.last_fired == -1 && !v.muted);
    let v = kick.view("kick", "kick", 35, 0.25, true);
    assert!(v.token == 3 && v.last_fired == 2 && v.muted && v.phase == 0.25);
    // stingers are rings too
    assert!(RingShape::of(p.nets.get("hit1").unwrap()).is_some());
}

// MARK: loops

#[test]
fn launch_boundaries_are_labelled_and_validated() {
    assert_eq!(
        (quantum_label(1), quantum_label(4)),
        ("beat".to_string(), "bar".to_string())
    );
    assert_eq!(
        (quantum_label(8), quantum_label(32)),
        ("2 bars".to_string(), "8 bars".to_string())
    );
    assert_eq!(quantum_label(3), "3 beats");
    assert_eq!(valid_loop_quantum(16), Some(16));
    assert_eq!(valid_loop_quantum(5), None);
    assert_eq!(valid_clip_quantum(None), Some(None));
    assert_eq!(valid_clip_quantum(Some(9)), None);
    assert_eq!(valid_tap_take_bars(Some(64)), Some(Some(64)));
    assert_eq!(valid_tap_take_bars(Some(65)), None);
    assert_eq!(valid_tap_take_bars(None), Some(None));
    let mut own = BTreeMap::new();
    own.insert("c1".to_string(), 16);
    assert_eq!(quantum_for(&own, 4, Some("c1")), 16);
    assert_eq!(quantum_for(&own, 4, Some("c2")), 4);
    assert_eq!(quantum_for(&own, 4, None), 4);
}

#[test]
fn scenes_are_named_renamed_overwritten_deleted_and_follow_a_crop() {
    let mut sc = Scenes::default();
    assert_eq!(sc.next_name(), "Scene 1");
    let row = vec![
        ("a".to_string(), 1.0, None),
        (
            "b".to_string(),
            0.5,
            Some(LoopFx {
                drive: 6.0,
                ..LoopFx::default()
            }),
        ),
        ("c".to_string(), 1.0, None),
    ];
    let looping = vec!["a".to_string(), "b".to_string()];
    let s = sc.save("Scene 1", &row, &looping).unwrap();
    assert_eq!(
        s.clips
            .iter()
            .map(|c| c.clip_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(s.clips[1].fx.is_some() && s.clips[0].fx.is_none());
    assert!(sc.save("", &row, &looping).is_none());
    sc.save("Scene 2", &row, &["c".to_string()]);
    // overwriting keeps one scene of the name
    sc.save("Scene 1", &row, &["c".to_string()]);
    assert_eq!(sc.list.len(), 2);
    assert_eq!(sc.scene("Scene 1").unwrap().clips.len(), 1);
    assert_eq!(sc.next_name(), "Scene 3", "Scene 2 is taken");
    sc.delete("Scene 2");
    assert_eq!(sc.next_name(), "Scene 2");
}

#[test]
fn scene_json_carries_fx_only_when_set() {
    let plain = LoopScene {
        name: "Scene 1".into(),
        clips: vec![SceneClip::new("a")],
    };
    assert_eq!(
        plain.json().js_stringified(),
        r#"{"name":"Scene 1","clips":[{"clip":"a","gain":1}]}"#
    );
    let mut with = plain.clone();
    with.clips[0].fx = Some(LoopFx {
        drive: 6.0,
        ..LoopFx::default()
    });
    with.clips[0].gain = 0.5;
    assert_eq!(LoopScene::from_json(&with.json()), Some(with.clone()));
    assert_eq!(LoopScene::from_json(&plain.json()), Some(plain));
    assert!(
        LoopScene::from_json(&JsonValue::parse(r#"{"name":"","clips":[]}"#).unwrap()).is_none()
    );
}

#[test]
fn renaming_restoring_and_replacing_clips() {
    let mut sc = Scenes::default();
    let row = vec![("a".to_string(), 1.0, None)];
    sc.save("Scene 1", &row, &["a".to_string()]);
    sc.save("Scene 2", &row, &["a".to_string()]);
    sc.rename("Scene 1", "Scene 2"); // taken: refused
    sc.rename("Scene 1", "");
    assert!(sc.scene("Scene 1").is_some());
    sc.rename("Scene 1", "Drop");
    assert!(sc.scene("Drop").is_some() && sc.scene("Scene 1").is_none());
    sc.replace_clip("a", "a2");
    assert_eq!(sc.scene("Drop").unwrap().clips[0].clip_id, "a2");
    let saved = sc.list.clone();
    sc.clear();
    assert!(sc.list.is_empty());
    sc.restore(&saved);
    sc.restore(&saved);
    assert_eq!(sc.list.len(), 2, "a name already taken is skipped");
}

#[test]
fn a_scene_starts_exactly_its_clips_and_stops_the_others_on_one_boundary() {
    let wanted = vec![
        SceneClip {
            clip_id: "a".into(),
            gain: 0.8,
            fx: None,
        },
        SceneClip {
            clip_id: "b".into(),
            gain: 0.6,
            fx: Some(LoopFx::default()),
        },
        SceneClip::new("gone"),
    ];
    let exists = |id: &str| id != "gone";
    let looping = vec!["a".to_string(), "x".to_string()];
    let state = |id: &str| match id {
        "a" => LoopState::On,
        "x" => LoopState::Stopping,
        _ => LoopState::Off,
    };
    let ops = plan_scene_launch(&wanted, &exists, &looping, &state);
    assert_eq!(
        ops,
        [
            SceneOp::Stop { id: "x".into() },
            SceneOp::SetGain {
                id: "a".into(),
                gain: 0.8,
                at_boundary: true
            },
            SceneOp::Start { id: "a".into() },
            SceneOp::SetGain {
                id: "b".into(),
                gain: 0.6,
                at_boundary: false
            },
            SceneOp::SetFx {
                id: "b".into(),
                fx: LoopFx::default(),
                at_boundary: false
            },
            SceneOp::Start { id: "b".into() },
        ]
    );
}

#[test]
fn the_countdown_counts_beats_to_the_switch() {
    // 48 000 Hz, 120 bpm: 24 000 frames a beat; scheduled at frame 0, switching at 96 000
    let (left, total) = loop_countdown(96_000, 24_000, Some(0), 24_000.0, 120.0, 48_000.0);
    assert_eq!((left, total), (3.0, 4.0));
    // no clock: the clip's own tempo
    let (left, _) = loop_countdown(48_000, 0, None, 0.0, 120.0, 48_000.0);
    assert_eq!(left, 2.0);
    // already past: zero, and the total is never less than what is left
    let (left, total) = loop_countdown(1000, 5000, Some(0), 24_000.0, 120.0, 48_000.0);
    assert!(left == 0.0 && total > 0.0);
}

#[test]
fn the_loops_follow_the_louder_deck_with_a_three_db_margin() {
    let d = |name: &str, level: f64| DominantCandidate {
        name: name.into(),
        playing_with_tempo: true,
        level,
    };
    assert_eq!(channel_level(true, 1.0, 1.0, 1.0), 0.0);
    assert_eq!(channel_level(false, 0.5, 0.8, 0.5), 0.2);
    // the louder one
    assert_eq!(
        update_dominant_deck(&[d("A", 0.5), d("B", 0.9)], None).as_deref(),
        Some("B")
    );
    // the current one stays until another is 3 dB louder
    let margin = dominance_margin();
    assert!((margin - 1.4125375446227544).abs() < 1e-12);
    assert_eq!(
        update_dominant_deck(&[d("A", 0.6), d("B", 0.6 * 1.40)], Some("A")).as_deref(),
        Some("A")
    );
    assert_eq!(
        update_dominant_deck(&[d("A", 0.6), d("B", 0.6 * 1.42)], Some("A")).as_deref(),
        Some("B")
    );
    // ties keep the first; silence is nobody; a stopped current hands over
    assert_eq!(
        update_dominant_deck(&[d("A", 0.5), d("B", 0.5)], None).as_deref(),
        Some("A")
    );
    assert_eq!(
        update_dominant_deck(&[d("A", 0.0), d("B", 0.0)], None),
        None
    );
    let mut stopped = d("A", 0.9);
    stopped.playing_with_tempo = false;
    assert_eq!(
        update_dominant_deck(&[stopped, d("B", 0.2)], Some("A")).as_deref(),
        Some("B")
    );
    // a clip's own follow beats the row's
    let mut own = BTreeMap::new();
    own.insert("c".to_string(), LoopFollow::Source);
    assert_eq!(
        follow_for(&own, LoopFollow::Louder, Some("c")),
        LoopFollow::Source
    );
    assert_eq!(
        follow_for(&own, LoopFollow::Louder, Some("d")),
        LoopFollow::Louder
    );
    assert!(any_loop_follows_louder(
        LoopFollow::Louder,
        &BTreeMap::new()
    ));
    assert!(!any_loop_follows_louder(LoopFollow::Source, &own));
    assert_eq!(LoopFollow::from_raw("louder"), Some(LoopFollow::Louder));
}

#[test]
fn a_clip_is_retuned_through_crops_and_retunes_but_never_a_sequence() {
    let nested = JsonValue::parse(
        r#"{"op":"crop","parent":{"op":"retune","parent":{"mode":"net","source":{"kind":"net"}}}}"#,
    )
    .unwrap();
    assert_eq!(
        take_provenance(Some(&nested))
            .unwrap()
            .get("mode")
            .and_then(JsonValue::as_str),
        Some("net")
    );
    let sample = JsonValue::parse(r#"{"mode":"sample"}"#).unwrap();
    assert_eq!(take_provenance(Some(&sample)), Err(Refusal::NotANet));
    let seq = JsonValue::parse(r#"{"op":"sequence","steps":[]}"#).unwrap();
    assert_eq!(take_provenance(Some(&seq)), Err(Refusal::Sequence));
    let odd = JsonValue::parse(r#"{"op":"mystery"}"#).unwrap();
    assert_eq!(take_provenance(Some(&odd)), Err(Refusal::NoProvenance));
    assert_eq!(take_provenance(None), Err(Refusal::NoProvenance));
    let cut = JsonValue::parse(r#"{"op":"crop"}"#).unwrap();
    assert_eq!(
        take_provenance(Some(&cut)),
        Err(Refusal::NoProvenance),
        "a crop with no parent"
    );
    assert!(Refusal::Macros(vec!["beatRepeat".into(), "drop".into()])
        .message()
        .starts_with("Taken with beatRepeat, drop running"));
    assert_eq!(
        Refusal::Sequence.message(),
        "A sequence: retune the loops it is made of."
    );
}

#[test]
fn a_retune_render_starts_a_preroll_before_the_take_and_stops_far_in() {
    // start beat 8 → tick 32, 4 beats → 16 ticks more; the render starts 32 ticks earlier (0)
    assert_eq!(retune_ticks(8.0, 4), Ok((32, 48, 0)));
    assert_eq!(retune_ticks(32.0, 8), Ok((128, 160, 96)));
    assert_eq!(retune_ticks(-1.0, 4), Err(Refusal::TooFar));
    assert_eq!(retune_ticks(2040.0, 16), Err(Refusal::TooFar));
    // the frame limit: (end + preroll + 64) ticks at the take's tempo
    assert_eq!(
        retune_frame_limit(48, 120.0, 48_000.0),
        ((48 + 32 + 64) as f64 * 6000.0) as i64
    );
    // the cut: the step that begins at `frame` is tick − 1; swap anchors don't count
    let anchors = [(33, 1000, false), (34, 1100, true), (49, 5000, false)];
    assert_eq!(
        retune_cut(&anchors, 32, 48, None, None),
        (Some(1000), Some(5000))
    );
    assert_eq!(
        retune_cut(&anchors[2..], 32, 48, None, None),
        (None, None),
        "the end needs a start"
    );
}

// MARK: stinger launch

fn assignment(kind: AssignmentKind) -> StingerAssignment {
    let mut a = StingerAssignment::fallback(1);
    a.kind = kind;
    a
}

#[test]
fn a_hit_fires_what_it_was_curated_and_falls_back_to_its_net() {
    // deck.stingers: four slots, fallbacks where nothing is curated, none for a sample deck
    let mut p = TrackSpec::new("techno", 42.0, None).project();
    let all = deck_stingers(Some(&p));
    assert_eq!(all.len(), 4);
    assert!(all
        .iter()
        .enumerate()
        .all(|(i, a)| a.slot == i && a.kind == AssignmentKind::Fallback));
    p.stingers = vec![assignment(AssignmentKind::OneShot)];
    let all = deck_stingers(Some(&p));
    assert_eq!(all[1].kind, AssignmentKind::OneShot);
    assert!(deck_stingers(None).is_empty());

    let net = |bars: i64| StingerStep::FireNet {
        net_id: "hit2".into(),
        ticks: bars * 16,
    };
    // a one-shot unmutes its net for its bars (its own, else the call's)
    assert_eq!(
        stinger_steps(&assignment(AssignmentKind::OneShot), 1, 2),
        [net(2)]
    );
    let mut own = assignment(AssignmentKind::OneShot);
    own.bars = Some(4);
    assert_eq!(stinger_steps(&own, 1, 2), [net(4)]);
    // a macro fires through the macro path, the net behind it
    let mut m = assignment(AssignmentKind::Macro);
    m.macro_id = Some("riser".into());
    m.bars = Some(2);
    assert_eq!(
        stinger_steps(&m, 1, 1),
        [
            StingerStep::FireMacro {
                id: "riser".into(),
                duration: Some(2)
            },
            net(1)
        ]
    );
    // a loop launches on the loop row, the net behind it
    assert_eq!(
        stinger_steps(&assignment(AssignmentKind::Loop), 1, 1),
        [StingerStep::LaunchLoop, net(1)]
    );
    assert_eq!(
        stinger_steps(&assignment(AssignmentKind::Fallback), 1, 3),
        [net(3)]
    );
    // out of range: nothing
    assert!(stinger_steps(&assignment(AssignmentKind::Fallback), 4, 1).is_empty());
}

// (a) from the Swift source (`PreparedTrack.init` hands `StingerCuration.resolve`'s list to
// `Project.curateStingers`, which keeps it as `Project.stingers`; `Deck.stingers` reads it back):
// there is one `StingerAssignment`, so a project holds what the resolver returned as it came,
// and a deck's Hits fire it. (Two types here once, `pflow`'s and `sound::stinger`'s, which a host
// would have had to convert between.)
#[test]
fn a_project_holds_the_resolved_hits_and_the_deck_fires_them() {
    let content = StingerContent::everything();
    for (genre, seed) in [("techno", 42.0), ("lofi", 7.0), ("dubstep", 123.0)] {
        let hits = resolve(PaletteSet::shared(), genre, None, seed, &content, None, None);
        assert_eq!(hits.len(), 4);
        let mut p = TrackSpec::new(genre, seed, None).project();
        p.stingers = hits.clone();
        assert_eq!(deck_stingers(Some(&p)), hits, "{genre}");
        for (n, a) in hits.iter().enumerate() {
            let steps = stinger_steps(a, n as i64, 1);
            // the stinger net is always the last resort
            assert_eq!(
                steps.last(),
                Some(&StingerStep::FireNet {
                    net_id: format!("hit{}", n + 1),
                    ticks: if a.kind == AssignmentKind::OneShot {
                        a.bars.unwrap_or(1) * 16
                    } else {
                        16
                    },
                }),
                "{genre} hit {n}"
            );
            match a.kind {
                AssignmentKind::Macro => assert!(
                    matches!(&steps[0], StingerStep::FireMacro { id, duration }
                        if Some(id) == a.macro_id.as_ref() && *duration == a.bars),
                    "{genre} hit {n}: {steps:?}"
                ),
                AssignmentKind::Loop => assert_eq!(steps[0], StingerStep::LaunchLoop),
                AssignmentKind::OneShot | AssignmentKind::Fallback => assert_eq!(steps.len(), 1),
            }
        }
    }
}

#[test]
fn a_stinger_loop_has_a_stable_clip_id_and_goes_out_after_one_pass() {
    let id = stinger_clip_id("A", "/lib/Riser.caf");
    assert_eq!(id, stinger_clip_id("A", "/lib/Riser.caf"));
    assert!(id.starts_with("stinger-A-") && id != stinger_clip_id("B", "/lib/Riser.caf"));
    assert_eq!(
        id,
        format!(
            "stinger-A-{:x}",
            djrust::voices::stable_hash("/lib/Riser.caf")
        )
    );
    // 8 beats at 120 bpm: 4 s = 192 000 frames at 48 kHz after the start
    assert_eq!(stinger_stop_frame(10_000, 8, 120.0, 48_000.0), 202_000);
    // a decoded loop's tempo is its beats over its length
    assert_eq!(stinger_clip_bpm(8, 48_000.0, 192_000), 120.0);
    // the stops wait for their voice to be in, then go; a gone voice is forgotten
    let mut stops = BTreeMap::new();
    stops.insert("a".to_string(), 100);
    stops.insert("b".to_string(), 200);
    stops.insert("c".to_string(), 300);
    stops.insert("d".to_string(), 400);
    let voice = |id: &str| match id {
        "a" => Some(StopVoice {
            active: true,
            stop_scheduled: false,
        }),
        "b" => Some(StopVoice {
            active: false,
            stop_scheduled: false,
        }),
        "c" => Some(StopVoice {
            active: true,
            stop_scheduled: true,
        }),
        _ => None,
    };
    assert_eq!(
        service_stinger_stops(&mut stops, &voice),
        [("a".to_string(), 100)]
    );
    assert_eq!(
        stops.keys().map(String::as_str).collect::<Vec<_>>(),
        ["b", "c"]
    );
}
