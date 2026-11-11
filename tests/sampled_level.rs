//! `LevelMatchTests`' sampled half (Swift suite, form (a)): a track on the deck's own sampled
//! instruments, whose part levels are measured at load (`SamplerPartLevels`). The loudness probe
//! stands each sampled part in where its slot sits (the slot's measured level plus its
//! `.balance` gain, never the table's average); a slot that skipped the probes takes the palette
//! average; two cold renders of one spec pick the same gains and play the same samples. The sample
//! half of the suite is `deck_level.rs`.
//!
//! On the real MuseScore_General.sf2 (`make soundfonts`, `$DJ_SOUNDFONTS`): they say SKIPPED and
//! pass without it, as `deck_kit_levels.rs` does. The survey tests of the Swift suite
//! (`DJ_LEVEL_SURVEY=1`: `sampledPartsAgainstTheirSynthVoices`, `wholeSongsAgainstTheReference`,
//! `sampledSongsSpread`) are measurements that assert nothing but "something played"; the
//! first is `deck_kit_levels::sampled_songs_sit_at_their_synth_twins_level`'s question, the
//! last `sampled_songs_spread_stays_near_swifts_two_lu`'s.
#![cfg(feature = "sf2")]

use std::sync::{Arc, Mutex};

use djrust::deck::level_match::LevelMatch;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::loudness::{IntegratedLoudness, Loudness};
use djrust::musicality::mix_balance::{MixBalance, SampledBalance};
use djrust::wave::Lane;
use djrust::TrackSpec;
use djrust::host::deck::samplers::SamplerBank;
use djrust::host::deck::{engine, EngineConfig, PreparedTrack, Preparer};
use djrust::host::sampler::{sound_font_dir, Library};

const RATE: f64 = 24_000.0;

fn real_bank() -> Option<std::path::PathBuf> {
    let dir = sound_font_dir();
    if dir.join("MuseScore_General.sf2").exists() {
        Some(dir)
    } else {
        eprintln!(
            "SKIPPED: no MuseScore_General.sf2 in {} (make soundfonts)",
            dir.display()
        );
        None
    }
}

/// A deck's preparer on a bank of its own with a cold part-level cache.
fn cold(dir: &std::path::Path, measures: bool) -> (Preparer, Arc<Mutex<SamplerBank>>) {
    let mut c = EngineConfig::new(RATE);
    c.library = Some(Arc::new(Library::scan(dir)));
    let (e, _r) = engine(c);
    let p = e.preparer("A").unwrap();
    let b = p.sampler_bank().unwrap();
    let mut fresh = SamplerBank::with_part_cache(Arc::new(Library::scan(dir)), RATE as u32, None);
    fresh.measures_part_levels = measures;
    *b.lock().unwrap() = fresh;
    (p, b)
}

fn sampled_lanes(p: &djrust::wave::WaveProject) -> Vec<(&Lane, String)> {
    p.lanes()
        .iter()
        .filter_map(|l| {
            let g = p.sampled_groups.get(&l.net_id)?;
            (l.kind == "tonal" && l.sample_slot().is_some()).then(|| (l, g.clone()))
        })
        .collect()
}

// LevelMatchTests.aSampledTrackPlaysAtTheReference, lofi / jazz / bossa: the probe's stand-ins
// are the slots' measured levels (with the slot gain), never the table; the song lands near the
// reference.
//
// Swift holds the song within 1 LU of `LevelMatch.referenceLUFS`. **Here jazz reads 0.9 LU under
// it, lofi 1.9 and bossa 2.2** (-12.9, -13.9, -14.2 LUFS at seed 4242 on MuseScore), from 2.4, 3.0
// and 3.4 LU under before the samplers joined the master curve (`samplersInCurve`: the measured
// loudness gain and the energy curve now reach them). The stand-ins are exactly where the Swift
// literal puts them, and the parts sit where their synth voices do
// (`deck_kit_levels::sampled_songs_sit_at_their_synth_twins_level`); the two that are still past
// 1 LU are the ones whose instrument plays the song quieter than its part-level probe said (lofi's
// piano lead: see `sampled_balance.rs`), so the gain measured with the stand-in at the probe's
// level leaves them under. Held at 2.4 LU and named.
#[test]
fn a_sampled_track_plays_near_the_reference() {
    let Some(dir) = real_bank() else { return };
    for genre in ["lofi", "jazz", "bossa"] {
        let spec = TrackSpec::new(genre, 4242.0, Some("standard"));
        let (p, bank) = cold(&dir, true);
        let t = PreparedTrack::compose(&spec, &p, 0);
        let w = t.prepared.wave();
        let sampled = sampled_lanes(w);
        assert!(!sampled.is_empty(), "{genre}: nothing played on a sampler");
        for (lane, group) in &sampled {
            let slot = lane.sample_slot().unwrap();
            assert!(
                bank.lock().unwrap().slot_part_level_db(slot, group).is_some(),
                "{genre}: {} measured",
                lane.net_id
            );
            // (against the net's own synth voice, which the probe plays: `ownPartLevelDB`)
            let level = w.sampled_part_db.get(&lane.net_id).copied();
            assert!(level.is_some());
            let gain_db = 20.0 * (w.slot_gain(slot) as f64).log10();
            let trim = if lane.net_id.starts_with("fill-") {
                0.0
            } else {
                MixBalance::part_of_kind("tonal", group)
                    .and_then(|part| w.balance.as_ref().and_then(|b| b.get(part).copied()))
                    .unwrap_or(0.0)
            };
            let want = SampledBalance::probe_level(trim, level, group, gain_db);
            let stand = w.probe_levels[&lane.net_id];
            // (`slotGain` is stored as f32, the render's gain stage, so `gain_db` read back from
            // it carries f32 rounding: the stand-in matches to that)
            assert!(
                (stand - want).abs() <= 1e-6 * want.abs().max(1e-3),
                "{genre} {}: {stand} vs {want}",
                lane.net_id
            );
        }

        // and played whole, as a deck plays it
        let mut c = EngineConfig::new(RATE);
        c.library = Some(Arc::new(Library::scan(&dir)));
        let (mut e, mut r) = engine(c);
        e.set_crossfader(0.0);
        e.set_volume("A", 1.0);
        assert!(e.generate("A", &spec), "{genre}");
        let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
        r.render_block(&mut l, &mut rr);
        e.play("A");
        let (mut left, mut right) = (Vec::new(), Vec::new());
        while left.len() < (400.0 * RATE) as usize {
            r.render_block(&mut l, &mut rr);
            left.extend_from_slice(&l);
            right.extend_from_slice(&rr);
            if e.deck("A").unwrap().mirror.stopped.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
        }
        let lufs = IntegratedLoudness::integrated(&left, Some(&right), RATE).expect("plays");
        println!(
            "{genre} sampled: {lufs:.1} LUFS (reference {}, {:+.1} LU)",
            LevelMatch::REFERENCE_LUFS,
            lufs - LevelMatch::REFERENCE_LUFS
        );
        assert!(
            (lufs - LevelMatch::REFERENCE_LUFS).abs() <= 2.4,
            "{genre} sampled: {lufs} LUFS"
        );
    }
}

// LevelMatchTests.anUnmeasuredSlotTakesThePaletteAverage: a slot whose bank skipped the
// part-level probes has no level of its own: the probe falls back to the palette's average
// (`Loudness.sampledPartDB`) for that part.
#[test]
fn an_unmeasured_slot_takes_the_palette_average() {
    let Some(dir) = real_bank() else { return };
    let (p, _bank) = cold(&dir, false);
    let mut p = p;
    p.opts.musicality = Fixes::ALL & !Fixes::BALANCE;
    let t = PreparedTrack::compose(&TrackSpec::new("lofi", 4242.0, Some("minimal")), &p, 0);
    let w = t.prepared.wave();
    assert!(!w.sampled_groups.is_empty(), "a sampled part");
    for (net, group) in &w.sampled_groups {
        assert_eq!(
            w.probe_levels[net],
            Loudness::sampled_part_level(group, None),
            "{net}"
        );
    }
}

// LevelMatchTests.twoColdCacheRendersOfOneSpecMatch: the gain comes only from what prepare has:
// it waits for each slot's load (and its part levels), so two decks rendering one spec from a
// cold cache pick the same loudness gain and slot gains, and play the same bytes (SoundFont
// programs render the same every time).
#[test]
fn two_cold_cache_renders_of_one_spec_match() {
    let Some(dir) = real_bank() else { return };
    let spec = TrackSpec::new("lofi", 4242.0, Some("standard"));
    let one = render(&dir, &spec, 30.0);
    let two = render(&dir, &spec, 30.0);
    assert!(one.slots > 0, "a sampled part");
    assert_eq!(one.gain, two.gain);
    assert_eq!(one.probe, two.probe);
    assert_eq!(one.levels, two.levels, "the slots' measured levels");
    assert_eq!(one.slot_gains, two.slot_gains);
    assert!(one.left == two.left && one.right == two.right, "the same bytes");
}

struct Played {
    slots: usize,
    gain: f64,
    probe: Vec<(String, f64)>,
    levels: Vec<Vec<(String, f64)>>,
    slot_gains: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
}

fn render(dir: &std::path::Path, spec: &TrackSpec, seconds: f64) -> Played {
    let (p, bank) = cold(dir, true);
    let t = PreparedTrack::compose(spec, &p, 0);
    let w = t.prepared.wave();
    let slots: std::collections::BTreeSet<usize> = sampled_lanes(w)
        .iter()
        .map(|(l, _)| l.sample_slot().unwrap())
        .collect();
    let levels = slots
        .iter()
        .map(|s| {
            bank.lock()
                .unwrap()
                .slot_part_levels(*s)
                .into_iter()
                .collect()
        })
        .collect();
    let slot_gains = slots.iter().map(|s| w.slot_gain(*s)).collect();
    let probe = w.probe_levels.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let gain = w.gain;
    // the deck plays it: the samplers handed to a core, the master and the limiter as the app has them
    let djrust::host::deck::PreparedTrack { prepared, opts, .. } = t;
    let mut core = djrust::host::deck::DeckCore::new(
        "x",
        RATE,
        opts,
        true,
        Arc::new(djrust::host::deck::DeckMirror::new()),
    );
    for (slot, s) in p.take_samplers() {
        core.apply(djrust::host::deck::Command::Sampler(slot, s));
    }
    core.apply(djrust::host::deck::Command::Load(prepared.project));
    core.apply(djrust::host::deck::Command::Play);
    let (mut l, mut r) = (vec![0f32; 512], vec![0f32; 512]);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut frame = 0i64;
    while left.len() < (seconds * RATE) as usize {
        core.render(&mut l, &mut r, frame);
        frame += 512;
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
    }
    Played {
        slots: slots.len(),
        gain,
        probe,
        levels,
        slot_gains,
        left,
        right,
    }
}
