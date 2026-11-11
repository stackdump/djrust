//! `.sectionHarmony` / `.harmonicRhythm` in the runner (`WaveRunner.prepare` builds the section
//! plan, every tonal lane plays its notes through it, the `FireEvent` carries the moved note):
//! Swift's `SectionTransposeTests` and the one `HarmonicRhythmTests` case that reads
//! `prepare().harmony` (PORTING.md's form (a)). The same plan held against Swift's output sample
//! for sample is the "harmony" cases of `tests/runner_golden.rs`; the allocation counters are in
//! `crates/djhost/tests/deck_rt.rs`.

mod common;
use common::measure::identical;
use djrust::gen::tables::BeatsTables;
use djrust::gen::theme::build_track_theme;
use djrust::musicality::note_log::capture_all_with;
use djrust::musicality::section_harmony::SectionHarmony;
use djrust::musicality::section_melody::section_melody_shifts;
use djrust::musicality::{apply, Fixes};
use djrust::pflow::Project;
use djrust::wave::synth::is_drum_channel;
use djrust::wave::*;
use djrust::TrackSpec;
use std::collections::BTreeSet;
use std::sync::OnceLock;

const SR: f64 = 22_050.0;
/// A tick at 600 bpm (551.25 frames).
const FRAMES_PER_TICK: f64 = SR * 60.0 / (600.0 * 4.0);

/// SectionMelodyTests' `BridgeSong`: the first standard / extended song (genres sorted, seeds
/// 1...6, seed 42 first) with a bridge, a plan and a melody net to normalise.
struct BridgeSong {
    spec: TrackSpec,
    composed: Project,
}

impl BridgeSong {
    fn played(&self, fixes: Fixes) -> Project {
        apply(&self.composed, &self.spec, fixes)
    }

    fn first() -> &'static BridgeSong {
        static CELL: OnceLock<Option<BridgeSong>> = OnceLock::new();
        CELL.get_or_init(|| {
            for structure in ["standard", "extended"] {
                for genre in BeatsTables::shared().genres.keys() {
                    let mut seeds = vec![42.0];
                    seeds.extend((1..=6).map(|s| s as f64));
                    for seed in seeds {
                        let spec = TrackSpec::new(genre, seed, Some(structure));
                        let composed = spec.project();
                        if !(composed.structure.iter().any(|s| s.name == "bridge")
                            && composed.cohesion.as_deref() == Some("v2")
                            && SectionHarmony::new(&composed).is_some())
                        {
                            continue;
                        }
                        let early = apply(
                            &composed,
                            &spec,
                            Fixes::KEY | Fixes::PHRASES | Fixes::TIMING,
                        );
                        let theme = build_track_theme(genre, seed);
                        if section_melody_shifts(&early, &theme)
                            .iter()
                            .any(|s| s.1 == 5)
                        {
                            return Some(BridgeSong { spec, composed });
                        }
                    }
                }
            }
            None
        })
        .as_ref()
        .expect("a bridge song")
    }
}

fn opts(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        musicality: fixes,
        ..Default::default()
    }
}

/// `runner(_:_:startTick:)`: a runner playing `p` with `fixes` from `start_tick`, at 600 bpm.
fn runner(p: &Project, fixes: Fixes, start_tick: i64) -> WaveRunner {
    let o = opts(fixes);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, start_tick));
    r.set_tempo(600.0);
    r.play();
    r
}

/// `blocks` blocks of 128 frames.
fn render(r: &mut WaveRunner, blocks: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * 128);
    let mut l = [0f32; 128];
    for _ in 0..blocks {
        r.process(&mut l, None, 128);
        out.extend_from_slice(&l);
    }
    out
}

/// The first moved section (`degrees != 0`) of a song's plan.
fn first_moved(h: &SectionHarmony) -> usize {
    h.degrees
        .iter()
        .position(|d| *d != 0)
        .expect("a moved section")
}

/// SectionTransposeTests.lanesGetTheirGroup: without the flag there is no plan and no lane is
/// mapped; with it, the tonal lanes of the plan's groups are, and drums and the live keys never
/// are.
#[test]
fn lanes_get_their_group() {
    let played = BridgeSong::first().played(Fixes::ALL);
    let plain = WaveRunner::prepare(&played, SR, &WaveOptions::default(), 0);
    assert!(plain.harmony.is_none());
    assert!(plain.lanes().iter().all(|l| l.harmony_group() == -1));
    let mapped = WaveRunner::prepare(&played, SR, &opts(Fixes::SECTION_HARMONY), 0);
    let h = mapped.harmony.clone().expect("a plan");
    assert_eq!(Some(h.clone()), SectionHarmony::new(&played));
    assert_eq!(mapped.live_lane().harmony_group(), -1);
    for lane in mapped.lanes().iter().filter(|l| !l.is_tonal()) {
        assert_eq!(lane.harmony_group(), -1);
    }
    let groups: BTreeSet<i64> = mapped
        .lanes()
        .iter()
        .map(|l| l.harmony_group())
        .filter(|g| *g >= 0)
        .collect();
    assert!(!groups.is_empty());
    assert!(groups.iter().all(|g| (*g as usize) < h.groups.len()));
}

/// SectionTransposeTests.firedNotesAreTheMappedLog: across the first moved section's start, the
/// runner fires exactly the notes `NoteLog` captures through the same plan (the `FireEvent` note
/// is the mapped one), and some of them differ from the bound notes.
#[test]
fn fired_notes_are_the_mapped_log() {
    let played = BridgeSong::first().played(Fixes::ALL);
    let h = SectionHarmony::new(&played).unwrap();
    let k = first_moved(&h);
    let from = (h.start[k] - 32).max(0);
    let ticks = 96;
    let mut r = runner(&played, Fixes::SECTION_HARMONY, from);
    r.record_events = true;
    render(&mut r, (ticks as f64 * FRAMES_PER_TICK / 128.0) as usize);
    let fires = std::mem::take(&mut r.fires);
    let last = fires.iter().map(|f| f.tick as i64).max().unwrap_or(0);
    assert!(last > h.start[k] + 16);

    let proj = r.project().unwrap();
    let voiced: BTreeSet<&str> = proj.lanes().iter().map(|l| l.net_id.as_str()).collect();
    let mut got: Vec<(i64, String, i64)> = fires
        .iter()
        .filter(|f| !f.flags.contains(FireFlags::DRUM) && !is_drum_channel(f.channel as i64))
        .map(|f| {
            (
                f.tick as i64,
                proj.graph.nets[f.net as usize].id.clone(),
                f.note as i64,
            )
        })
        .collect();
    let expected = |harmony: Option<&SectionHarmony>| -> Vec<(i64, String, i64)> {
        capture_all_with(&played, Some(last), harmony)
            .0
            .into_iter()
            .filter(|e| e.tick > from && !e.is_drum() && voiced.contains(e.net.as_str()))
            .map(|e| (e.tick, e.net, e.note))
            .collect()
    };
    let mut want = expected(Some(&h));
    assert!(!got.is_empty());
    got.sort();
    want.sort();
    assert_eq!(got, want);
    let moved: BTreeSet<_> = want.iter().cloned().collect();
    let bound: BTreeSet<_> = expected(None).into_iter().collect();
    assert_ne!(moved, bound, "the plan moves some note");
}

/// SectionTransposeTests.homeSectionsAndLoopsRenderAsBound: home sections play the bound notes:
/// up to the first moved section the flag renders the same samples as no flag, and a loop (no
/// plan) renders the same throughout.
#[test]
fn home_sections_and_loops_render_as_bound() {
    let song = BridgeSong::first();
    let played = song.played(Fixes::ALL);
    let h = SectionHarmony::new(&played).unwrap();
    let k = first_moved(&h);
    let from = (h.start[k] - 48).max(0);
    let blocks = ((h.start[k] - from - 1) as f64 * FRAMES_PER_TICK / 128.0) as usize;
    let on = render(&mut runner(&played, Fixes::SECTION_HARMONY, from), blocks);
    let off = render(&mut runner(&played, Fixes::EMPTY, from), blocks);
    assert!(identical(&on, &off));
    assert!(on.iter().any(|x| *x != 0.0));

    let loop_spec = TrackSpec::new(&song.spec.genre, song.spec.seed, None);
    let lp = apply(&loop_spec.project(), &loop_spec, Fixes::ALL);
    assert!(SectionHarmony::new(&lp).is_none());
    assert!(identical(
        &render(&mut runner(&lp, Fixes::SECTION_HARMONY, 0), 400),
        &render(&mut runner(&lp, Fixes::EMPTY, 0), 400)
    ));
}

/// SectionTransposeTests.liveKeysAreNotMapped: live keys play what is pressed, whatever the
/// section.
#[test]
fn live_keys_are_not_mapped() {
    let played = BridgeSong::first().played(Fixes::ALL);
    let h = SectionHarmony::new(&played).unwrap();
    let k = first_moved(&h);
    let mut r = runner(&played, Fixes::SECTION_HARMONY, h.start[k]);
    assert_eq!(r.project().unwrap().live_lane().harmony_group(), -1);
    // nothing but the key: every net muted
    let ids: Vec<String> = r
        .graph()
        .unwrap()
        .nets
        .iter()
        .map(|n| n.id.clone())
        .collect();
    for id in ids {
        r.set_mute(&id, true);
    }
    render(&mut r, 8);
    r.note_on(69, 100);
    render(&mut r, 2);
    assert!(r.project().unwrap().live_lane().held_notes().contains(&69));
    r.note_off(69);
}

/// The first played `standard` / `extended` project of HarmonicRhythmTests' genres and seeds
/// with a rhythm plan.
fn rhythm_song() -> Project {
    let both = Fixes::SECTION_HARMONY | Fixes::HARMONIC_RHYTHM;
    for structure in ["standard", "extended"] {
        for genre in [
            "trance",
            "techno",
            "house",
            "edm",
            "synthwave",
            "lofi",
            "country",
            "funk",
            "metal",
            "reggae",
        ] {
            for seed in [42.0, 7.0, 1234.0] {
                let spec = TrackSpec::new(genre, seed, Some(structure));
                let p = apply(&spec.project(), &spec, Fixes::ALL);
                if SectionHarmony::with_fixes(&p, both).is_some() {
                    return p;
                }
            }
        }
    }
    panic!("a song with a rhythm plan");
}

/// HarmonicRhythmTests.prepareBuildsThePlanUnderEitherFlag: `WaveRunner.prepare` builds the
/// plan under either flag.
#[test]
fn prepare_builds_the_plan_under_either_flag() {
    let p = rhythm_song();
    assert!(WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0)
        .harmony
        .is_none());
    let rhythm = WaveRunner::prepare(&p, SR, &opts(Fixes::HARMONIC_RHYTHM), 0);
    assert_eq!(
        rhythm.harmony,
        SectionHarmony::with_fixes(&p, Fixes::HARMONIC_RHYTHM)
    );
    assert!(rhythm.harmony.is_some());
    assert!(rhythm.lanes().iter().any(|l| l.harmony_group() >= 0));
}
