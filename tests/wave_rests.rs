//! `RestScheduleTests.swift` and `MixOutTailTests.swift`: the runner's rests and the
//! mix-out tail. Small projects through a linear master, 120 BPM at 48 kHz: a step is
//! 6000 frames, so step k ticks at frame k × 6000 exactly. Golden: none (dj-only).

use djrust::gen::structure::build_control_bundle;
use djrust::pflow::{ControlBinding, NetBundle, Section};
use djrust::wave::rests::{Class, Cursor, Entry};
use djrust::wave::*;
use djrust::Project;
use std::collections::BTreeMap;

const SR: f64 = 48_000.0;
const STEP: usize = (0.125 * SR) as usize;

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for &(step, note) in notes {
        n.bind(step, note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

fn rms_db(x: &[f32], a: usize, b: usize) -> f64 {
    let x = &x[a..b];
    let s: f64 = x.iter().map(|&v| v as f64 * v as f64).sum();
    10.0 * (s / x.len() as f64).max(1e-20).log10()
}

/// Four bars of buildup into four of drop, kick on the beats, an offbeat bass.
fn rest_project(kick: bool) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    if kick {
        p.nets.insert(
            "kick",
            ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
        );
    }
    p.nets.insert(
        "bass",
        ring("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
    );
    p.structure = vec![
        Section {
            name: "buildup".into(),
            steps: 64,
            phrases: Default::default(),
        },
        Section {
            name: "drop".into(),
            steps: 64,
            phrases: Default::default(),
        },
    ];
    p
}

/// Eight bars of kick, snare, bass and a melody, ending in beats' `struct-stop`.
fn tail_project(melody: bool) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert(
        "kick",
        ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
    );
    p.nets
        .insert("snare", ring("drums", 10, &[(4, 38), (12, 38)], 60));
    p.nets.insert(
        "bass",
        ring("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
    );
    if melody {
        p.nets.insert(
            "melody",
            ring(
                "melody",
                1,
                &[(0, 69), (3, 69), (6, 69), (8, 69), (11, 69), (14, 69)],
                100,
            ),
        );
    }
    p.structure = vec![
        Section {
            name: "verse".into(),
            steps: 64,
            phrases: Default::default(),
        },
        Section {
            name: "outro".into(),
            steps: 64,
            phrases: Default::default(),
        },
    ];
    let mut ctrl = BTreeMap::new();
    ctrl.insert(127, ControlBinding::new("stop-transport", ""));
    p.nets.insert(
        "struct-stop",
        build_control_bundle(128, &ctrl, true, "p", "t"),
    );
    p
}

fn options(rests: Option<RestSchedule>, tail: Option<MixOutTail>) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        rests,
        tail,
        ..Default::default()
    }
}

/// `seconds` of the runner from `play()`, and the block (frames) in which it stopped.
fn play(
    p: &Project,
    o: &WaveOptions,
    start_tick: i64,
    seconds: f64,
) -> (Vec<f32>, Option<std::ops::Range<usize>>) {
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, o, start_tick));
    r.play();
    let mut out = vec![0f32; (seconds * SR) as usize];
    let mut stop = None;
    let mut off = 0;
    while off < out.len() {
        let n = 256.min(out.len() - off);
        r.process(&mut out[off..off + n], None, n);
        if stop.is_none() && r.stopped() {
            stop = Some(off..off + n);
        }
        off += n;
    }
    (out, stop)
}

// ---- RestScheduleTests -------------------------------------------------------

#[test]
fn merging_makes_disjoint_stretches() {
    let s = RestSchedule::merging(&[
        Entry::new(8, 16, Class::KICK),
        Entry::new(0, 12, Class::BASS),
        Entry::new(20, 20, Class::DRUMS),
        Entry::new(30, 40, Class::NONE),
        Entry::new(40, 44, Class::MELODY),
    ]);
    assert_eq!(
        s.entries,
        vec![
            Entry::new(0, 8, Class::BASS),
            Entry::new(8, 12, Class::BASS.union(Class::KICK)),
            Entry::new(12, 16, Class::KICK),
            Entry::new(40, 44, Class::MELODY),
        ]
    );
    assert!(RestSchedule::merging(&[]).is_empty() && RestSchedule::empty().is_empty());
}

#[test]
fn lane_classes() {
    assert_eq!(Class::of("kick", "drums", "kick"), Class::KICK);
    assert_eq!(Class::of("snare", "drums", "snare"), Class::DRUMS);
    assert_eq!(Class::of("openhat", "drums", "hihat"), Class::DRUMS);
    assert!(Class::of("crash", "drums", "fill-1").is_empty());
    assert_eq!(Class::of("kick", "drums", "fill-2"), Class::FILL);
    assert_eq!(Class::of("tonal", "bass", "bass"), Class::BASS);
    assert_eq!(Class::of("tonal", "harmony", "harmony"), Class::HARMONY);
    assert_eq!(Class::of("tonal", "melody", "melody-0"), Class::MELODY);
    assert_eq!(Class::of("tonal", "arp", "arp"), Class::ARP);
    assert_eq!(Class::of("tonal", "lead2", "x"), Class::TONAL);
    assert!(Class::RHYTHM.is_disjoint(Class::MELODIC));
}

/// The cursor reads what a scan reads: walking forward, after a jump back, after a reset,
/// and starting anywhere.
#[test]
fn cursor_matches_a_scan() {
    let s = RestSchedule::merging(&[
        Entry::new(4, 8, Class::KICK),
        Entry::new(6, 10, Class::BASS),
        Entry::new(60, 64, Class::RHYTHM),
        Entry::new(64, 80, Class::MELODY),
    ]);
    let mut c = Cursor::new();
    for t in 0..100 {
        assert_eq!(c.mask(t, &s), s.mask(t), "step {t}");
    }
    for t in [70, 5, 9, 62, 0, 99, 63, 64] {
        assert_eq!(c.mask(t, &s), s.mask(t), "jump to {t}");
    }
    c.reset();
    for t in 50..90 {
        assert_eq!(c.mask(t, &s), s.mask(t), "step {t}");
    }
    assert!(Cursor::new().mask(5, &RestSchedule::empty()).is_empty());
}

/// A gesture rests only its classes: the kick out for a bar, the bass playing on, and the
/// render before it untouched.
#[test]
fn a_schedule_rests_its_classes_only() {
    let s = RestSchedule::merging(&[Entry::new(16, 32, Class::KICK)]);
    let (both, _) = play(&rest_project(true), &options(Some(s), None), 0, 6.0);
    let (plain, _) = play(&rest_project(true), &options(None, None), 0, 6.0);
    let (bass_only, _) = play(&rest_project(false), &options(None, None), 0, 6.0);
    assert!(both[..16 * STEP] == plain[..16 * STEP]);
    // in the bar the kick is out (past the last kick's tail) only the bass plays
    let (a, b) = (16 * STEP + STEP / 2, 32 * STEP);
    assert!((rms_db(&both, a, b) - rms_db(&bass_only, a, b)).abs() < 0.5);
    assert!(rms_db(&plain, a, b) > rms_db(&both, a, b) + 1.0);
}

/// Starting past the top (a section launch) and stopping then playing again (the tick goes
/// back to 0) both find the gap where it is.
#[test]
fn cursor_follows_starts_and_stops() {
    let p = rest_project(true);
    let gap = RestSchedule::merging(&[Entry::new(60, 64, Class::RHYTHM)]);
    let o = options(Some(gap), None);
    let (gapped, _) = play(&p, &o, 0, 10.0);
    // a start at step 48 plays what the full render plays from 48, on the same tick grid
    let (late, _) = play(&p, &o, 48, 4.0);
    let (plain_late, _) = play(&p, &options(None, None), 48, 4.0);
    let g = 12 * STEP;
    assert!(
        rms_db(&late, g + STEP / 4, g + 4 * STEP)
            < rms_db(&plain_late, g + STEP / 4, g + 4 * STEP) - 20.0
    );
    // play past the gap, stop, play again from the top: the second pass rests too
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &o, 0));
    r.play();
    let mut buf = vec![0f32; (10.0 * SR) as usize];
    for c in buf.chunks_mut(256) {
        let n = c.len();
        r.process(c, None, n);
    }
    r.stop();
    r.play();
    let mut again = vec![0f32; (10.0 * SR) as usize];
    for c in again.chunks_mut(256) {
        let n = c.len();
        r.process(c, None, n);
    }
    let gp = 60 * STEP;
    let in_gap = rms_db(&again, gp + STEP / 4, gp + 4 * STEP);
    assert!((in_gap - rms_db(&gapped, gp + STEP / 4, gp + 4 * STEP)).abs() < 1.0);
    assert!(in_gap < rms_db(&again, gp - 4 * STEP, gp) - 20.0);
}

// ---- MixOutTailTests ---------------------------------------------------------

/// The tail's rest is the melodic classes, from its first tick to its stop; on a schedule
/// it merges with what is there.
#[test]
fn tail_rests_the_melodic_classes() {
    let t = MixOutTail::new(64, 4);
    assert!(
        t.end_tick() == 128 && MixOutTail::new(32, MixOutTail::DEFAULT_BARS).end_tick() == 32 + 256
    );
    assert_eq!(t.entry(), Entry::new(64, 128, Class::MELODIC));
    assert!(Class::MELODIC.is_disjoint(Class::RHYTHM));
    let gap = RestSchedule::merging(&[Entry::new(60, 68, Class::RHYTHM)]);
    let s = gap.with_tail(&t);
    assert!(s.mask(59).is_empty() && s.mask(60) == Class::RHYTHM);
    assert!(s.mask(64) == Class::RHYTHM.union(Class::MELODIC) && s.mask(70) == Class::MELODIC);
    assert!(s.mask(128).is_empty());
    assert_eq!(
        RestSchedule::mix_in(32, 2),
        Entry::new(32, 64, Class::MELODIC)
    );
}

/// No tail: nothing changes. The project has no tail, its rests are the song's, and it
/// stops where beats' stop-transport stops it (on step 127's tick).
#[test]
fn no_tail_ends_with_the_song() {
    let p = tail_project(true);
    let o = options(None, None);
    let prepared = WaveRunner::prepare(&p, SR, &o, 0);
    assert!(prepared.tail.is_none() && prepared.rests == prepared.song_rests);
    assert!(WaveOptions::default().tail.is_none());
    let (_, stop) = play(&p, &o, 0, 18.0);
    let s = stop.expect("the song ends");
    assert!(
        s.contains(&(127 * STEP)),
        "stopped in {s:?}, the song's end is frame {}",
        127 * STEP
    );
}

/// With a tail from the outro for 16 bars: the render is the song's until the outro, then
/// the drums-forward lanes alone (as the project without its lead), on past the song's
/// end, and the deck stops on the tail's last bar line.
#[test]
fn tail_plays_past_the_song_and_stops_on_its_bar_line() {
    let p = tail_project(true);
    let tail = MixOutTail::new(64, 16);
    let (with_tail, stop) = play(&p, &options(None, Some(tail)), 0, 42.0);
    let (song, _) = play(&p, &options(None, None), 0, 42.0);
    let (no_lead, _) = play(&tail_project(false), &options(None, None), 0, 42.0);
    let s = stop.expect("the tail ends");
    let end = tail.end_tick() as usize * STEP;
    assert!(s.contains(&end), "stopped in {s:?}, the bar line is {end}");
    assert!(
        with_tail[..64 * STEP] == song[..64 * STEP],
        "untouched before the tail"
    );
    // in the tail (past the lead's last note) only kick, snare and bass play
    let (a, b) = (64 * STEP + STEP, 96 * STEP);
    assert!((rms_db(&with_tail, a, b) - rms_db(&no_lead, a, b)).abs() < 0.5);
    let residual = |x: &[f32]| {
        let d: Vec<f32> = (a..b).map(|i| x[i] - no_lead[i]).collect();
        rms_db(&d, 0, d.len())
    };
    assert!(
        residual(&with_tail) < -80.0,
        "the tail plays no lead: {} dB",
        residual(&with_tail)
    );
    assert!(
        residual(&song) > -40.0,
        "the song does: {} dB",
        residual(&song)
    );
    // past the song's own end the rhythm plays on, as loud as earlier in the tail
    let c = 200 * STEP;
    assert!((rms_db(&with_tail, c, c + 32 * STEP) - rms_db(&with_tail, a, b)).abs() < 1.0);
    // and it is silent once the last voices have died away
    let quiet = end + SR as usize;
    assert!(rms_db(&with_tail, quiet, quiet + SR as usize / 2) < -80.0);
}

/// A start inside the tail (a section launch) stops on the same bar line, in song ticks.
#[test]
fn a_late_start_stops_on_the_same_bar_line() {
    let p = tail_project(true);
    let tail = MixOutTail::new(64, 4);
    let (out, stop) = play(&p, &options(None, Some(tail)), 96, 6.0);
    let s = stop.expect("the tail ends");
    let end = (tail.end_tick() - 96) as usize * STEP;
    assert!(s.contains(&end), "stopped in {s:?}, the bar line is {end}");
    assert!(
        rms_db(&out, STEP, 16 * STEP) > -60.0,
        "the rhythm plays up to it"
    );
}

/// `align` replays whole ticks silently, and a mix-out tail takes the song's stop-transport
/// request back (`songEnds`), so the alignment goes on past the song's end to the tail's bar
/// line; with no tail it stops where the song does. (WaveRunner.align in Swift calls
/// `songEnds` after every silent advance.)
#[test]
fn align_goes_on_past_the_songs_end_under_a_tail() {
    let p = tail_project(true);
    let aligned = |o: &WaveOptions| {
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare(&p, SR, o, 0));
        r.align(200.0);
        r.graph().unwrap().tick
    };
    let song_end = aligned(&options(None, None));
    assert!(
        song_end < 200,
        "the song stops the alignment at tick {song_end}"
    );
    let tail = aligned(&options(None, Some(MixOutTail::new(64, 16))));
    assert_eq!(tail, 200, "a tail plays on past the song's end");
}
