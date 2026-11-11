//! `SamplerPartLevelTests` (Swift suite, form (a)): each sampled instrument's part levels,
//! measured at load (`SamplerPartLevels`, `Sf2Sampler::measure_part_levels`, kept by the
//! `SamplerBank`). Swift runs them on General MIDI programs of Apple's bank, which every Mac has
//! and which render the same every time; the SoundFont here is, in turn,
//!
//! - a synthetic one (`synthetic_sf2`: a looped sine under 128 programs), always: the logic is
//!   held without the real bank, so these run in CI;
//! - the real MuseScore_General.sf2 (the app's folder or `$DJ_SOUNDFONTS`, `make soundfonts`),
//!   when it is there: the programs of Swift's list, `[0, 33, 48, 80, 89]`. They say SKIPPED and
//!   pass without it, as `deck_kit_levels.rs` does.
//!
//! The load-time bounds of Swift (150 ms added by the probes, one second for six lanes) are the
//! app's acceptance and only hold for an optimised build; the test profile here is `opt-level
//! = 2`, so they run and the figures they measure are printed.
#![cfg(feature = "sf2")]

use std::sync::Arc;
use std::time::Instant;

use djrust::voices::SamplerSource;
use djrust::wave::SamplerPartLevels;
use djrust::host::deck::samplers::SamplerBank;
use djrust::host::sampler::{
    part_level_cache_key, sound_font_dir, synthetic_sf2, Library, PartLevelCache, Sf2Sampler,
};

const SR: u32 = 48_000;
/// Piano, fingered bass, strings, square lead, a pad.
const PROGRAMS: [i64; 5] = [0, 33, 48, 80, 89];

/// A folder holding a bank, and what to call the bank's path in a source.
struct Bank {
    dir: std::path::PathBuf,
    file: String,
    name: &'static str,
}

impl Bank {
    fn source(&self, program: i64) -> SamplerSource {
        SamplerSource::Sf2(self.file.clone(), program)
    }

    fn library(&self) -> Arc<Library> {
        Arc::new(Library::scan(&self.dir))
    }

    fn cold(&self) -> SamplerBank {
        SamplerBank::with_part_cache(self.library(), SR, None)
    }
}

fn synthetic(tag: &str) -> Bank {
    let d = std::env::temp_dir().join(format!("djhost-partlevels-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    std::fs::write(d.join("MuseScore_General.sf2"), synthetic_sf2(&presets)).unwrap();
    Bank {
        file: d.join("MuseScore_General.sf2").to_string_lossy().into_owned(),
        dir: d,
        name: "synthetic",
    }
}

fn real() -> Option<Bank> {
    let dir = sound_font_dir();
    let file = dir.join("MuseScore_General.sf2");
    if !file.exists() {
        eprintln!(
            "SKIPPED: no MuseScore_General.sf2 in {} (make soundfonts)",
            dir.display()
        );
        return None;
    }
    Some(Bank {
        file: file.to_string_lossy().into_owned(),
        dir,
        name: "MuseScore",
    })
}

fn banks(tag: &str) -> Vec<Bank> {
    let mut v = vec![synthetic(tag)];
    v.extend(real());
    v
}

fn groups() -> std::collections::BTreeSet<String> {
    SamplerPartLevels::GROUPS.iter().map(|g| g.to_string()).collect()
}

// SamplerPartLevelTests.everyPartIsMeasuredAndFinite
#[test]
fn every_part_is_measured_and_finite() {
    for b in banks("finite") {
        let mut bank = b.cold();
        for p in PROGRAMS {
            let slot = bank.slot_for(&[b.source(p)]).expect("the program loads");
            let levels = bank.slot_part_levels(slot);
            let keys: std::collections::BTreeSet<String> = levels.keys().cloned().collect();
            assert_eq!(keys, groups(), "{} program {p}: {levels:?}", b.name);
            for (g, l) in &levels {
                assert!(l.is_finite() && l.abs() < 60.0, "{} {p} {g}: {l} dB", b.name);
            }
            println!(
                "{} program {p}: {:?}",
                b.name,
                levels.iter().map(|(g, l)| format!("{g} {l:+.1}")).collect::<Vec<_>>()
            );
        }
    }
}

// SamplerPartLevelTests.measurementsAreDeterministic
#[test]
fn measurements_are_deterministic() {
    for b in banks("determinism") {
        for p in PROGRAMS {
            let (mut a, mut c) = (b.cold(), b.cold());
            let sa = a.slot_for(&[b.source(p)]).unwrap();
            let sc = c.slot_for(&[b.source(p)]).unwrap();
            for g in SamplerPartLevels::GROUPS {
                let la = a.slot_part_level_db(sa, g).expect("measured");
                let lc = c.slot_part_level_db(sc, g).expect("measured");
                assert!((la - lc).abs() <= 0.1, "{} {p} {g}: {la} vs {lc}", b.name);
            }
        }
    }
}

// SamplerPartLevelTests.groupsWithoutAProbeTakeTheNearest
#[test]
fn groups_without_a_probe_take_the_nearest() {
    for b in banks("nearest") {
        let mut bank = b.cold();
        let slot = bank.slot_for(&[b.source(48)]).unwrap();
        assert_eq!(
            bank.slot_part_level_db(slot, "pad"),
            bank.slot_part_level_db(slot, "harmony")
        );
        assert_eq!(
            bank.slot_part_level_db(slot, "lead"),
            bank.slot_part_level_db(slot, "melody")
        );
        assert!(bank.slot_part_level_db(slot, "pad").is_some());
        assert_eq!(bank.slot_part_level_db(slot + 1, "melody"), None, "an empty slot");
        assert_eq!(bank.slot_part_level_db(usize::MAX, "melody"), None);
    }
}

// SamplerPartLevelTests.kitsAndSkippedProbesMeasureNothing
#[test]
fn kits_and_skipped_probes_measure_nothing() {
    for b in banks("skipped") {
        let mut bank = b.cold();
        bank.measures_part_levels = false;
        let slot = bank.slot_for(&[b.source(0)]).unwrap();
        assert!(bank.slot_part_levels(slot).is_empty());
        let kit = SamplerSource::Kit {
            path: "/x.sf2".into(),
            bank: 128,
            program: 0,
        };
        assert_eq!(part_level_cache_key(&kit, SR), None);
    }
}

// SamplerPartLevelTests.theCacheRoundTrips: warm loads read the cache and land on the same
// levels; the file outlives the bank.
#[test]
fn the_cache_round_trips() {
    for b in banks("cache") {
        let file = b.dir.join(format!("part-levels-{}.json", b.name));
        let _ = std::fs::remove_file(&file);
        let cache = Arc::new(PartLevelCache::new(Some(file.clone())));
        let mut cold = SamplerBank::with_part_cache(b.library(), SR, Some(cache.clone()));
        let s1 = cold.slot_for(&[b.source(33)]).unwrap();
        let first = cold.slot_part_levels(s1);
        let key = part_level_cache_key(&b.source(33), SR).expect("a key");
        assert!(cache.levels(&key).is_some());
        let reread = Arc::new(PartLevelCache::new(Some(file.clone())));
        assert_eq!(reread.levels(&key), cache.levels(&key));
        let mut warm = SamplerBank::with_part_cache(b.library(), SR, Some(reread));
        let s2 = warm.slot_for(&[b.source(33)]).unwrap();
        for g in SamplerPartLevels::GROUPS {
            let (x, y) = (first[g], warm.slot_part_level_db(s2, g).unwrap());
            assert!((x - y).abs() <= 0.1, "{} {g}: {x} vs {y}", b.name);
        }
        assert_ne!(part_level_cache_key(&b.source(33), 44_100), Some(key), "keyed by rate");
        // Swift's key, field for field: tag, file, size, whole seconds of its mtime, the
        // rate and the probes' version
        let key = part_level_cache_key(&b.source(33), SR).unwrap();
        let parts: Vec<&str> = key.split('|').collect();
        assert_eq!(parts.len(), 6, "{key}");
        assert_eq!(parts[0], "sf2#33");
        assert_eq!(parts[1], b.file);
        assert_eq!(
            parts[2].parse::<u64>().unwrap(),
            std::fs::metadata(&b.file).unwrap().len()
        );
        assert_eq!((parts[4], parts[5]), ("48000", "v2"));
        let _ = std::fs::remove_file(&file);
    }
}

// SamplerPartLevelTests.probesAddLittleToALoad: the probes add at most 150 ms to a load (cold
// cache; median of five).
#[test]
fn probes_add_little_to_a_load() {
    for b in banks("probes") {
        let seconds = |probes: bool, program: i64| {
            let mut bank = b.cold();
            bank.measures_part_levels = probes;
            let t = Instant::now();
            let _ = bank.slot_for(&[b.source(program)]);
            t.elapsed().as_secs_f64()
        };
        for g in SamplerPartLevels::GROUPS {
            let _ = djrust::host::sampler::synth_level_db(g, SR as f64);
        }
        let mut added: Vec<f64> = PROGRAMS
            .iter()
            .map(|&p| seconds(true, p) - seconds(false, p))
            .collect();
        println!(
            "{}: probes add {:?} ms",
            b.name,
            added.iter().map(|s| (s * 1000.0).round()).collect::<Vec<_>>()
        );
        added.sort_by(f64::total_cmp);
        let median = added[added.len() / 2];
        assert!(median <= 0.15, "{}: median {} ms", b.name, median * 1000.0);
    }
}

// SamplerPartLevelTests.sixSampledLanesAddUnderASecond: a track with six sampled lanes adds at
// most a second to its prepare, cold cache (the synth reference included).
#[test]
fn six_sampled_lanes_add_under_a_second() {
    for b in banks("six") {
        let mut insts: Vec<Sf2Sampler> = Vec::new();
        for p in [0, 25, 33, 48, 73, 89] {
            let mut i = Sf2Sampler::open(&b.source(p), 44_100).unwrap();
            assert!(i.normalize(true));
            insts.push(i);
        }
        let t = Instant::now();
        for i in &mut insts {
            i.measure_part_levels(None);
        }
        let seconds = t.elapsed().as_secs_f64();
        println!("{}: six lanes: {:.0} ms", b.name, seconds * 1000.0);
        assert!(seconds <= 1.0, "{}: {seconds} s", b.name);
        for i in &insts {
            assert_eq!(i.part_level_db.len(), SamplerPartLevels::GROUPS.len());
        }
    }
}
