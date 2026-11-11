//! The references a project holds on its sampler slots (`WaveProject.samplers` and `deinit` in
//! Swift): `SamplerSlots::slot_for` takes one for each sampled lane at prepare, and the project
//! gives one back per lane when it is dropped, however it got there: dropped unused, or let go by
//! the runner into its retire list and freed by the host (a full bank reuses only slots no
//! project references: the host's half is in `crates/djhost/tests/sampler_bank.rs`).

use djrust::pflow::{NetBundle, Project};
use djrust::voices::{SamplerSource, VoiceCatalog};
use djrust::wave::runner::SamplerSlots;
use djrust::wave::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const SR: f64 = 22_050.0;

/// A bank that numbers the first source of each list (in the order asked) and counts what is taken
/// and given back, by slot.
#[derive(Default)]
struct CountingBank {
    keys: Mutex<Vec<String>>,
    taken: Mutex<BTreeMap<usize, u32>>,
    released: Mutex<BTreeMap<usize, u32>>,
}

impl CountingBank {
    fn new() -> Arc<CountingBank> {
        Arc::new(CountingBank::default())
    }
    fn taken(&self) -> BTreeMap<usize, u32> {
        self.taken.lock().unwrap().clone()
    }
    fn released(&self) -> BTreeMap<usize, u32> {
        self.released.lock().unwrap().clone()
    }
}

impl SamplerSlots for CountingBank {
    fn instrument_sources(
        &self,
        instrument: &str,
        _role: &str,
        _seed: f64,
        _picked: bool,
    ) -> Option<Vec<SamplerSource>> {
        VoiceCatalog::bundled().category_of.get(instrument)?;
        Some(vec![SamplerSource::Sf2(
            format!("/fake/{instrument}.sf2"),
            1,
        )])
    }

    fn option_sources(&self, _option: &str, _role: &str, _seed: f64) -> Option<Vec<SamplerSource>> {
        None
    }

    fn slot_for(&self, sources: &[SamplerSource]) -> Option<usize> {
        let key = sources.first()?.to_string();
        let mut keys = self.keys.lock().unwrap();
        let slot = match keys.iter().position(|k| *k == key) {
            Some(i) => i,
            None => {
                keys.push(key);
                keys.len() - 1
            }
        };
        *self.taken.lock().unwrap().entry(slot).or_default() += 1;
        Some(slot)
    }

    fn release(&self, slot: usize) {
        *self.released.lock().unwrap().entry(slot).or_default() += 1;
    }

    fn kit_source(&self, _id: &str) -> Option<SamplerSource> {
        None
    }

    fn brush_kit(&self, _kit: &SamplerSource) -> Option<SamplerSource> {
        None
    }
}

/// A melody on a piano, a pad on a piano too (one slot, two lanes) and a bass on a bass guitar.
fn band() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let net = |group: &str, channel: i64, instrument: &str, note: i64| {
        let mut n = NetBundle::ring(16);
        n.bind(0, note, channel, 100, 100);
        n.track.channel = channel;
        n.track.group = Some(group.into());
        n.track.instrument = instrument.into();
        n
    };
    p.nets.insert("melody", net("melody", 4, "piano", 72));
    p.nets.insert("harmony", net("harmony", 7, "piano", 60));
    p.nets.insert("bass", net("bass", 6, "bass", 36));
    p
}

fn options(bank: &Arc<CountingBank>) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        bank: Some(bank.clone()),
        ..WaveOptions::extended()
    }
}

fn slots_of(p: &WaveProject) -> Vec<usize> {
    p.lanes().iter().filter_map(|l| l.sampled_slot()).collect()
}

/// Swift's `WaveProject.deinit`: one reference given back for each lane that took one, including
/// the pooled voice the live keys play, and two lanes on one slot give back two.
#[test]
fn a_dropped_project_gives_back_one_reference_per_sampled_lane() {
    let bank = CountingBank::new();
    let p = WaveRunner::prepare(&band(), SR, &options(&bank), 0);
    let slots = slots_of(&p);
    assert!(slots.len() >= 4, "three nets and the live voice: {slots:?}");
    let mut per_slot: BTreeMap<usize, u32> = BTreeMap::new();
    for s in &slots {
        *per_slot.entry(*s).or_default() += 1;
    }
    assert!(per_slot.values().any(|n| *n >= 2), "a slot two lanes share");
    assert_eq!(bank.taken(), per_slot, "one reference taken per lane");
    assert!(
        bank.released().is_empty(),
        "nothing is given back while it lives"
    );
    drop(p);
    assert_eq!(bank.released(), per_slot, "and the same given back");
}

/// A runner that lets a project go (a load over it) keeps it for the host to free; the references
/// come back when the host does, not before.
#[test]
fn a_retired_project_gives_its_references_back_when_the_host_frees_it() {
    let bank = CountingBank::new();
    let opts = options(&bank);
    let mut r = WaveRunner::new(SR, opts.clone());
    let first = WaveRunner::prepare(&band(), SR, &opts, 0);
    let per_lane = slots_of(&first).len();
    r.load_prepared(first);
    // (a load over the current one leaves it fading, its lanes ringing out; the one after that
    // pushes it out to the retire list)
    r.load_prepared(WaveRunner::prepare(&band(), SR, &opts, 0));
    assert!(r.pop_retired().is_none(), "still fading");
    r.load_prepared(WaveRunner::prepare(&band(), SR, &opts, 0));
    let old = r.pop_retired().expect("the first project is retired");
    assert!(bank.released().is_empty(), "retired, not freed");
    drop(old);
    let given: u32 = bank.released().values().sum();
    assert_eq!(given as usize, per_lane);
    assert!(r.pop_retired().is_none());
    // the two that are left give theirs back when the runner goes
    drop(r);
    let given: u32 = bank.released().values().sum();
    assert_eq!(given as usize, 3 * per_lane);
}

/// Lanes bound by name (a `SampleBinder`: no handle to the bank) take their references through the
/// host's thunk; the host gives the project the bank to hand them back to
/// (`WaveProject::hold_slots_of`). A project that has one already keeps it.
#[test]
fn a_host_gives_a_project_the_bank_its_binder_took_references_in() {
    fn bind(_instrument: &str, _part: &str) -> Option<usize> {
        Some(2)
    }
    let opts = WaveOptions {
        master: Master::Linear,
        sample_binder: Some(bind),
        ..Default::default()
    };
    let mut p = WaveRunner::prepare(&band(), SR, &opts, 0);
    let lanes = slots_of(&p).len();
    assert!(lanes >= 4);
    let bank = CountingBank::new();
    p.hold_slots_of(bank.clone());
    // a second one is ignored: the first handle is the project's
    let other = CountingBank::new();
    p.hold_slots_of(other.clone());
    drop(p);
    assert_eq!(bank.released().get(&2), Some(&(lanes as u32)));
    assert!(other.released().is_empty());
    // a project that took nothing has nothing to give back
    let none = WaveRunner::prepare(&band(), SR, &WaveOptions::default(), 0);
    let mut none = none;
    let bank = CountingBank::new();
    none.hold_slots_of(bank.clone());
    drop(none);
    assert!(bank.released().is_empty());
}
