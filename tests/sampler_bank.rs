//! `SamplerBank` (Swift `Sampler.swift`): the slot cache. Capacity 24, a count of references on
//! each slot, `lastUsed` LRU reuse of the slots nothing references, the reasons a source is turned
//! away (`loadFailure(for:)`) and the one retry a silent tonal load gets.
//!
//! The Swift tests (`SamplerTests.aBankFullOfReferencedSlotsFallsBackToTheSynthVoice`,
//! `aFullBankEvictsTheLeastRecentlyUsedUnreferencedSlot`, `projectsHoldTheirSlotsUntilFreed`,
//! `turnedAwaySoundsAreReported`, `missingContentFallsThrough`) use General MIDI programs of
//! Apple's bank (`.gm(p)`) only as distinct keys; here the keys are the programs of a synthesised
//! SoundFont with 128 presets (`Sf2(path, p)`), which this host plays. Nothing here needs the real
//! `MuseScore_General.sf2`.
#![cfg(feature = "sf2")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use djrust::voices::SamplerSource;
use djrust::wave::runner::SamplerSlots;
use djrust::wave::{WaveOptions, WaveRunner};
use djrust::TrackSpec;
use djrust::host::deck::samplers::{HostSlots, SamplerBank, BANK_SLOTS};
use djrust::host::sampler::{check_failure, synthetic_sf2, synthetic_sf2_tuned, Check, Library};

const SR: u32 = 44_100;

/// A SoundFont folder: `MuseScore_General.sf2` (the file name the catalog's floor bank has) with
/// the 128 General MIDI programs, all a looped sine, and a percussion kit.
struct Fonts {
    dir: PathBuf,
    path: String,
}

impl Fonts {
    fn new(tag: &str) -> Fonts {
        let dir =
            std::env::temp_dir().join(format!("djhost-sampler-bank-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
        let mut presets: Vec<(&str, u16, u16)> = names
            .iter()
            .enumerate()
            .map(|(p, n)| (n.as_str(), 0u16, p as u16))
            .collect();
        presets.push(("Standard", 128, 0));
        let path = dir.join("MuseScore_General.sf2");
        std::fs::write(&path, synthetic_sf2(&presets)).unwrap();
        Fonts {
            dir,
            path: path.to_string_lossy().into_owned(),
        }
    }

    /// `.gm(p)`: program `p` of the font.
    fn gm(&self, p: usize) -> SamplerSource {
        SamplerSource::Sf2(self.path.clone(), p as i64)
    }

    fn kit(&self) -> SamplerSource {
        SamplerSource::Kit {
            path: self.path.clone(),
            bank: 128,
            program: 0,
        }
    }

    fn library(&self) -> Arc<Library> {
        Arc::new(Library::scan(&self.dir))
    }

    /// `SamplerBank(sampleRate:)`, without the part-level probes (they are held in
    /// `sampler_part_levels.rs`; each would add a few tens of ms to every load here).
    fn bank(&self) -> SamplerBank {
        let mut b = SamplerBank::with_part_cache(self.library(), SR, None);
        b.measures_part_levels = false;
        b
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write(dir: &Path, name: &str, bytes: Vec<u8>) -> String {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p.to_string_lossy().into_owned()
}

/// Take the first `BANK_SLOTS` programs, in order, keeping every reference.
fn fill(bank: &mut SamplerBank, f: &Fonts) {
    for p in 0..BANK_SLOTS {
        assert_eq!(bank.slot_for(&[f.gm(p)]), Some(p), "program {p}");
    }
}

/// SamplerTests.aBankFullOfReferencedSlotsFallsBackToTheSynthVoice.
#[test]
fn a_bank_full_of_referenced_slots_falls_back_to_the_synth_voice() {
    let f = Fonts::new("referenced");
    let mut bank = f.bank();
    fill(&mut bank, &f);
    assert_eq!(bank.slot_for(&[f.gm(100)]), None);
    assert_eq!(
        bank.slot_for(&[f.gm(3)]),
        Some(3),
        "a loaded source is shared"
    );
    assert_eq!(bank.references_of(3), 2);
    // (Swift answers nil at once: it does not look further down the list for a loaded source)
    assert_eq!(bank.slot_for(&[f.gm(101), f.gm(7)]), None);
    assert_eq!(bank.references_of(7), 1);
}

/// SamplerTests.aFullBankEvictsTheLeastRecentlyUsedUnreferencedSlot.
#[test]
fn a_full_bank_evicts_the_least_recently_used_unreferenced_slot() {
    let f = Fonts::new("lru");
    let mut bank = f.bank();
    fill(&mut bank, &f);
    // Give back 5, then 9, then 17 (taking 9 once more in between): 5 is least recently used.
    bank.release(5);
    bank.release(9);
    bank.release(17);
    assert_eq!(
        bank.slot_for(&[f.gm(9)]),
        Some(9),
        "still loaded: shared, not reloaded"
    );
    bank.release(9);
    assert_eq!(bank.slot_for(&[f.gm(100)]), Some(5));
    assert_eq!(bank.source_of(5), Some(&f.gm(100)));
    assert_eq!(bank.slot_for(&[f.gm(101)]), Some(17));
    assert_eq!(bank.slot_for(&[f.gm(102)]), Some(9));
    assert_eq!(
        bank.slot_for(&[f.gm(103)]),
        None,
        "everything left is referenced"
    );
    for p in (0..BANK_SLOTS).filter(|p| ![5, 9, 17].contains(p)) {
        assert_eq!(
            bank.source_of(p),
            Some(&f.gm(p)),
            "referenced slot {p} kept"
        );
    }
    // An evicted source comes back by loading again, into whatever slot frees up.
    bank.release(0);
    assert_eq!(bank.slot_for(&[f.gm(5)]), Some(0));
    assert_eq!(bank.source_of(0), Some(&f.gm(5)));
}

/// A slot is reused the moment it has no references, and what it played is forgotten: the old
/// source is loaded again from scratch, not found numbered.
#[test]
fn an_evicted_source_is_forgotten_with_its_slot() {
    let f = Fonts::new("forgotten");
    let mut bank = f.bank();
    fill(&mut bank, &f);
    bank.release(12);
    let before = bank.opens;
    assert_eq!(bank.slot_for(&[f.gm(100)]), Some(12));
    assert_eq!(bank.opens, before + 1);
    // program 12 is not loaded any more: with every slot referenced again, it cannot be taken
    assert_eq!(bank.slot_for(&[f.gm(12)]), None);
    bank.release(12);
    assert_eq!(
        bank.slot_for(&[f.gm(12)]),
        Some(12),
        "loaded again into the free slot"
    );
    assert_eq!(bank.opens, before + 2);
}

/// What a slot measured on the part probes goes with the sound: a kit that takes over a slot has
/// none of a melodic program's.
#[test]
fn an_eviction_clears_the_slots_part_levels() {
    let f = Fonts::new("levels");
    let mut bank = SamplerBank::with_part_cache(f.library(), SR, None);
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    let level = bank.slot_part_level_db(0, "melody");
    assert!(level.is_some(), "a program is measured on its probes");
    bank.measures_part_levels = false;
    for p in 1..BANK_SLOTS {
        assert_eq!(bank.slot_for(&[f.gm(p)]), Some(p));
    }
    bank.release(0);
    assert_eq!(bank.slot_for(&[f.kit()]), Some(0));
    assert_eq!(
        bank.slot_part_level_db(0, "melody"),
        None,
        "a kit has no probes"
    );
    assert!(bank.slot_part_levels(0).is_empty());
    assert_eq!(bank.source_of(0), Some(&f.kit()));
}

/// SamplerTests.projectsHoldTheirSlotsUntilFreed: a prepared project holds a reference on every
/// slot its sampled lanes play (the live voice's included) until it is dropped, so a churn through
/// more programs than the bank holds never takes them; dropping it gives them back.
#[test]
fn projects_hold_their_slots_until_freed() {
    let f = Fonts::new("projects");
    let lib = f.library();
    let bank = {
        let mut b = SamplerBank::with_part_cache(lib.clone(), SR, None);
        b.measures_part_levels = false;
        Arc::new(Mutex::new(b))
    };
    let host: Arc<dyn SamplerSlots> = Arc::new(HostSlots::new(bank.clone()));
    let project = TrackSpec::new("country", 7.0, Some("minimal")).project_in(lib.sounds());
    let opts = WaveOptions::extended_with(Some(host), 7.0, None);
    let prepared = WaveRunner::prepare(&project, SR as f64, &opts, 0);
    let lanes: Vec<usize> = prepared
        .lanes()
        .iter()
        .filter_map(|l| l.sampled_slot())
        .collect();
    let mut used = lanes.clone();
    used.sort();
    used.dedup();
    assert!(!used.is_empty(), "a sampled lane to hold");
    let held: Vec<(usize, Option<SamplerSource>)> = used
        .iter()
        .map(|s| (*s, bank.lock().unwrap().source_of(*s).cloned()))
        .collect();
    for s in &used {
        let n = lanes.iter().filter(|l| *l == s).count() as u32;
        assert_eq!(
            bank.lock().unwrap().references_of(*s),
            n,
            "slot {s}: one reference per lane that plays it"
        );
    }
    // Fill the bank, then churn through more programs than it holds: only the slots no project
    // references are ever reused.
    {
        let mut b = bank.lock().unwrap();
        for p in 0..64 {
            if let Some(s) = b.slot_for(&[f.gm(p)]) {
                b.release(s);
            }
        }
        for (s, source) in &held {
            assert_eq!(
                b.source_of(*s),
                source.as_ref(),
                "slot {s} kept while its project lives"
            );
            assert!(b.references_of(*s) > 0);
        }
        assert!(
            (0..BANK_SLOTS).all(|s| used.contains(&s) || b.references_of(s) == 0),
            "nothing else holds a reference"
        );
    }
    drop(prepared);
    {
        let mut b = bank.lock().unwrap();
        for s in &used {
            assert_eq!(b.references_of(*s), 0, "a freed project gave slot {s} back");
        }
        for p in 64..64 + BANK_SLOTS {
            assert!(b.slot_for(&[f.gm(p)]).is_some(), "program {p}");
        }
        assert!(
            (0..BANK_SLOTS)
                .all(|s| matches!(b.source_of(s), Some(SamplerSource::Sf2(_, p)) if *p >= 64)),
            "every slot was taken over"
        );
    }
}

/// The references a project took are the ones it gives back: one per sampled lane, the live
/// voice's included, however many lanes share a slot (the project holds the bank's handle,
/// `WaveProject::bank`, as Swift's holds `samplers`).
#[test]
fn a_project_gives_back_exactly_what_its_lanes_took() {
    let f = Fonts::new("balanced");
    let lib = f.library();
    let bank = Arc::new(Mutex::new(SamplerBank::with_part_cache(
        lib.clone(),
        SR,
        None,
    )));
    let host = Arc::new(HostSlots::new(bank.clone()));
    let project = TrackSpec::new("jazz", 3.0, Some("minimal")).project_in(lib.sounds());
    let opts = WaveOptions::extended_with(Some(host), 3.0, None);
    let p = WaveRunner::prepare(&project, SR as f64, &opts, 0);
    let total = |bank: &Mutex<SamplerBank>| -> u32 {
        let b = bank.lock().unwrap();
        (0..BANK_SLOTS).map(|s| b.references_of(s)).sum()
    };
    let lanes = p
        .lanes()
        .iter()
        .filter(|l| l.sampled_slot().is_some())
        .count() as u32;
    assert!(lanes > 1, "several lanes, which may share slots");
    assert_eq!(total(&bank), lanes);
    // a second project of the same song finds the slots loaded: the references add up
    let q = WaveRunner::prepare(&project, SR as f64, &opts, 0);
    assert_eq!(total(&bank), 2 * lanes);
    drop(p);
    assert_eq!(total(&bank), lanes, "one project's worth given back");
    drop(q);
    assert_eq!(total(&bank), 0);
}

/// Dropping a project does not wait behind a load: the references are counted apart from the
/// bank (lock-free), and the bank's lock is held across a whole load.
#[test]
fn a_project_is_freed_while_the_bank_is_busy() {
    let f = Fonts::new("busy");
    let lib = f.library();
    let bank = Arc::new(Mutex::new(SamplerBank::with_part_cache(
        lib.clone(),
        SR,
        None,
    )));
    let host = Arc::new(HostSlots::new(bank.clone()));
    let project = TrackSpec::new("country", 7.0, Some("minimal")).project_in(lib.sounds());
    let opts = WaveOptions::extended_with(Some(host), 7.0, None);
    let p = WaveRunner::prepare(&project, SR as f64, &opts, 0);
    let slot = p.lanes().iter().find_map(|l| l.sampled_slot()).unwrap();
    assert!(bank.lock().unwrap().references_of(slot) > 0);
    let busy = bank.lock().unwrap(); // a load in progress holds the bank like this
    let freed = std::thread::spawn(move || drop(p));
    let t0 = std::time::Instant::now();
    while !freed.is_finished() && t0.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        freed.is_finished(),
        "the project's drop waited for the bank's lock"
    );
    drop(busy);
    freed.join().unwrap();
    assert_eq!(bank.lock().unwrap().references_of(slot), 0);
}

/// A loaded sampler is owed to the audio thread once, whoever binds its slot and whether or not
/// the track that did plays: a second bind of the source finds it numbered and loads nothing, a
/// track dropped unused leaves it where it is, and the next take brings it. The send is a
/// property of the slot, not of the prepare that happened to load it.
#[test]
fn a_loaded_sampler_waits_for_its_one_send_however_its_slot_is_bound() {
    let f = Fonts::new("owed");
    let mut bank = f.bank();
    let outbox = bank.outbox();
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    assert_eq!(outbox.len(), 1);
    // a second prepare binds the same source (it loads nothing), and the first track is dropped
    // unused: its reference comes back
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    bank.release(0);
    assert_eq!(outbox.len(), 1, "still one sampler to send, not two and not none");
    // whoever takes first sends it, and nobody after has anything to send
    let sent = outbox.take();
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].0, &sent[0].1.source), (0, &f.gm(0)));
    assert!(outbox.is_empty() && bank.take_ready().is_empty());
    // a later bind of the slot finds it numbered, and the audio thread has it already
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    assert!(bank.take_ready().is_empty());
    // the handle and the bank are one queue
    assert_eq!(bank.slot_for(&[f.gm(1)]), Some(1));
    assert_eq!(outbox.len(), 1);
    assert_eq!(bank.take_ready().len(), 1);
    assert!(outbox.is_empty());
}

/// A sampler never sent (every track that bound it was dropped unused) goes with its slot when the
/// slot is given to another source: what is sent for it is the new source's, never the old one's
/// after it.
#[test]
fn a_sampler_never_sent_is_dropped_when_its_slot_is_taken() {
    let f = Fonts::new("stale");
    let mut bank = f.bank();
    // slot 0: a track's sampler that was never sent (the track was dropped), then the rest of
    // the bank
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    bank.release(0);
    for p in 1..BANK_SLOTS {
        assert_eq!(bank.slot_for(&[f.gm(p)]), Some(p));
    }
    // the bank is full; slot 0, the least recently used, goes to program 100
    assert_eq!(bank.slot_for(&[f.gm(100)]), Some(0));
    let ready = bank.take_ready();
    assert_eq!(ready.len(), BANK_SLOTS, "one sampler for each slot");
    let for_zero: Vec<_> = ready.iter().filter(|(slot, _)| *slot == 0).collect();
    assert_eq!(for_zero.len(), 1, "not program 0's as well");
    assert_eq!(for_zero[0].1.source, f.gm(100));
}

/// A sampler loaded for a slot that never went to the audio thread goes when the slot is taken
/// before it is sent, and the new one is the slot's.
#[test]
fn an_eviction_replaces_a_sampler_still_waiting_to_be_sent() {
    let f = Fonts::new("waiting");
    let mut bank = f.bank();
    fill(&mut bank, &f);
    bank.take_ready();
    bank.release(4);
    assert_eq!(bank.slot_for(&[f.gm(100)]), Some(4));
    bank.release(4);
    assert_eq!(bank.slot_for(&[f.gm(101)]), Some(4), "the same slot again");
    let ready = bank.take_ready();
    assert_eq!(ready.len(), 1, "one sampler for the slot, the newest");
    assert_eq!(ready[0].1.source, f.gm(101));
}

// ---- why a source is turned away -------------------------------------------------------------

/// SamplerTests.missingContentFallsThrough and `loadFailure`: a source missing from disk is
/// skipped with its reason, is not marked unloadable (installing it later works), and the next
/// source plays.
#[test]
fn a_source_not_on_disk_falls_through_and_can_load_once_installed() {
    let f = Fonts::new("missing");
    let mut bank = f.bank();
    let later = f.dir.join("Later.sf2");
    let wanted = SamplerSource::Sf2(later.to_string_lossy().into_owned(), 0);
    assert_eq!(bank.load_failure(&wanted), None, "not tried yet");
    assert_eq!(bank.slot_for(&[wanted.clone(), f.gm(25)]), Some(0));
    assert_eq!(bank.source_of(0), Some(&f.gm(25)));
    assert_eq!(bank.load_failure(&wanted), Some("not on disk".to_string()));
    // the report names it once, however often it is asked for
    bank.slot_for(std::slice::from_ref(&wanted));
    bank.slot_for(std::slice::from_ref(&wanted));
    assert_eq!(
        bank.report
            .iter()
            .filter(|l| l.ends_with(": not on disk"))
            .count(),
        1
    );
    // installed since
    write(&f.dir, "Later.sf2", synthetic_sf2(&[("Sine", 0, 0)]));
    assert_eq!(bank.slot_for(std::slice::from_ref(&wanted)), Some(1));
    assert_eq!(
        bank.load_failure(&wanted),
        None,
        "loaded: no failure to report"
    );
}

/// SamplerTests.turnedAwaySoundsAreReported: every source the bank turns away has a reason, in
/// Swift's words; one that is loaded has none.
#[test]
fn every_source_turned_away_has_a_reason_in_swifts_words() {
    let f = Fonts::new("reasons");
    let mut bank = f.bank();
    let flat = write(
        &f.dir,
        "flat.sf2",
        synthetic_sf2_tuned(&[("Sine", 0, 0)], 50),
    );
    let near = write(
        &f.dir,
        "near.sf2",
        synthetic_sf2_tuned(&[("Sine", 0, 0)], 20),
    );
    // a font whose sample is silence: zero the smpl chunk's data
    let mut mute = synthetic_sf2(&[("Silent", 0, 0), ("Silent kit", 128, 0)]);
    let at = mute.windows(4).position(|w| w == b"smpl").unwrap();
    let len = u32::from_le_bytes(mute[at + 4..at + 8].try_into().unwrap()) as usize;
    mute[at + 8..at + 8 + len].fill(0);
    let mute = write(&f.dir, "mute.sf2", mute);
    let exs = write(&f.dir, "Grand.exs", b"not an exs".to_vec());

    let cases: Vec<(SamplerSource, &str)> = vec![
        (
            SamplerSource::Exs("/nope/Missing.exs".into()),
            "not on disk",
        ),
        (
            SamplerSource::Exs(exs),
            "load failed: EXS needs AUSampler (macOS only)",
        ),
        (
            SamplerSource::Sf2(f.path.clone(), 127 + 1),
            "load failed: no preset 0:128 in the bank",
        ),
        (SamplerSource::Sf2(flat, 0), "out of tune ("),
        (SamplerSource::Sf2(mute.clone(), 0), "silent (-200.0 dB)"),
        (
            SamplerSource::Kit {
                path: mute,
                bank: 128,
                program: 0,
            },
            "a kit without a kick, snare or hat (quietest -200.0 dB)",
        ),
    ];
    for (source, why) in &cases {
        assert_eq!(
            bank.slot_for(std::slice::from_ref(source)),
            None,
            "{source}"
        );
        let got = bank.load_failure(source);
        let got = got.unwrap_or_else(|| panic!("{source}: turned away with no reason recorded"));
        assert!(got.starts_with(why), "{source}: {got:?} is not {why:?}");
    }
    // the out-of-tune text carries the measurement and the limit
    let text = bank.load_failure(&cases[3].0).unwrap();
    assert!(
        text.ends_with(" cents, limit 30)") && text.starts_with("out of tune ("),
        "{text}"
    );
    let cents: f64 = text["out of tune (".len()..text.len() - " cents, limit 30)".len()]
        .parse()
        .unwrap();
    assert!((cents.abs() - 50.0).abs() < 2.0, "{text}");
    assert!(text.contains("(+") || text.contains("(-"), "signed: {text}");
    // 20 cents is within the limit: loaded, nothing to report
    let ok = SamplerSource::Sf2(near, 0);
    assert!(bank.slot_for(std::slice::from_ref(&ok)).is_some());
    assert_eq!(bank.load_failure(&ok), None);
    // a loaded one has no failure, and a turned-away one is not asked for again
    let opens = bank.opens;
    assert_eq!(bank.slot_for(&[cases[3].0.clone()]), None);
    assert_eq!(bank.opens, opens, "unloadable: never tried again");
}

/// The wording of the check that turned a loaded sound away, for the cases the synthetic font
/// cannot make: `Sampler.swift` 551-561.
#[test]
fn the_check_failure_words() {
    let tonal = SamplerSource::Sf2("/x/Bank.sf2".into(), 5);
    let c = |rms_db, cents, held| {
        Some(Check {
            rms_db,
            cents,
            held,
        })
    };
    assert_eq!(
        check_failure(&tonal, c(-73.04, 0.0, true)),
        "silent (-73.0 dB)"
    );
    assert_eq!(
        check_failure(&tonal, c(-20.0, 12.0, false)),
        "silent 0.1 s into a held note: too short to tune"
    );
    assert_eq!(
        check_failure(&tonal, c(-20.0, 41.6, true)),
        "out of tune (+42 cents, limit 30)"
    );
    assert_eq!(
        check_failure(&tonal, c(-20.0, -41.6, true)),
        "out of tune (-42 cents, limit 30)"
    );
    // silence wins over a short note, as the order of Swift's ternary has it
    assert_eq!(
        check_failure(&tonal, c(-90.0, 0.0, false)),
        "silent (-90.0 dB)"
    );
    assert_eq!(
        check_failure(&tonal, None),
        "out of tune (? cents, limit 30)"
    );
    let kit = SamplerSource::Kit {
        path: "/x/Bank.sf2".into(),
        bank: 128,
        program: 0,
    };
    assert_eq!(
        check_failure(&kit, c(-61.25, 0.0, false)),
        "a kit without a kick, snare or hat (quietest -61.2 dB)"
    );
    assert_eq!(
        check_failure(&kit, None),
        "a kit without a kick, snare or hat (quietest -200.0 dB)"
    );
}

/// `Sampler.swift` 532-548: a tonal sound that came up silent is loaded once more before it is
/// turned away; a sound that is out of tune, or that fits, is opened once.
#[test]
fn a_silent_tonal_load_is_retried_once() {
    let f = Fonts::new("retry");
    let mut bank = f.bank();
    let mut mute = synthetic_sf2(&[("Silent", 0, 0)]);
    let at = mute.windows(4).position(|w| w == b"smpl").unwrap();
    let len = u32::from_le_bytes(mute[at + 4..at + 8].try_into().unwrap()) as usize;
    mute[at + 8..at + 8 + len].fill(0);
    let mute = SamplerSource::Sf2(write(&f.dir, "mute.sf2", mute), 0);
    let flat = SamplerSource::Sf2(
        write(
            &f.dir,
            "flat.sf2",
            synthetic_sf2_tuned(&[("Sine", 0, 0)], 50),
        ),
        0,
    );
    assert_eq!(bank.opens, 0);
    assert_eq!(bank.slot_for(&[f.gm(0)]), Some(0));
    assert_eq!(bank.opens, 1, "a sound that fits is opened once");
    assert_eq!(bank.slot_for(std::slice::from_ref(&flat)), None);
    assert_eq!(bank.opens, 2, "out of tune is not retried");
    assert_eq!(bank.slot_for(std::slice::from_ref(&mute)), None);
    assert_eq!(
        bank.opens, 4,
        "silent is opened again before it is turned away"
    );
    assert_eq!(bank.slot_for(std::slice::from_ref(&mute)), None);
    assert_eq!(bank.opens, 4, "and then never tried again");
    // the free slot the failed loads were going to take is still free
    assert_eq!(bank.loaded(), 1);
    assert_eq!(bank.slot_for(&[f.gm(1)]), Some(1));
}

/// A load that fails costs nothing already playing: a full bank of unreferenced slots keeps
/// every source when the new one will not load (Swift loads first, evicts after).
#[test]
fn a_load_that_fails_evicts_nothing() {
    let f = Fonts::new("costless");
    let mut bank = f.bank();
    fill(&mut bank, &f);
    for p in 0..BANK_SLOTS {
        bank.release(p);
    }
    let flat = SamplerSource::Sf2(
        write(
            &f.dir,
            "flat.sf2",
            synthetic_sf2_tuned(&[("Sine", 0, 0)], 50),
        ),
        0,
    );
    assert_eq!(
        bank.slot_for(&[flat.clone(), f.gm(100)]),
        Some(0),
        "falls through to the next, which evicts"
    );
    for p in 1..BANK_SLOTS {
        assert_eq!(bank.source_of(p), Some(&f.gm(p)), "slot {p} untouched");
    }
    assert_eq!(bank.source_of(0), Some(&f.gm(100)));
    assert!(bank.load_failure(&flat).is_some());
}
