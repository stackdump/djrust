//! The sampler bank as the runner binds against it at prepare ([`SamplerSlots`], Swift's
//! `SamplerBank` as `buildLanes` uses it): tonal lanes through `bindSampler` (the track's
//! `picked`, the palette's option, the ranked fallback) and drum lanes through `drumKitSource` /
//! `bindKit` (the kit's GM keys, its holds, the kick's trim as a velocity, jazz's Brush kit, a
//! closed hat ending the open hat's ring). Swift's `SampledKitTests` and `BrushTests` cases that
//! do not need an installed sound bank (those play MuseScore_General or Apple's GM bank, which
//! the Rust host loads in `djhost`), against a bank that only numbers what it is asked for.
//! (PORTING.md's form (a); the host's half is in `crates/djhost/tests/deck_sf2.rs`.)

mod common;
use common::measure::*;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::synth_voice::Brush;
use djrust::pflow::{NetBundle, Project};
use djrust::voices::drumkits::{self, gm_note};
use djrust::voices::sampler::velocity_scale;
use djrust::voices::{SamplerSource, VoiceCatalog};
use djrust::wave::runner::SamplerSlots;
use djrust::wave::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;

/// A bank with no sounds of its own: it numbers the first source of each list it is asked for
/// (in the order asked) and remembers every list.
#[derive(Default)]
struct FakeBank {
    keys: Mutex<Vec<String>>,
    asked: Mutex<Vec<Vec<SamplerSource>>>,
    instruments: Mutex<Vec<(String, String, bool)>>,
    /// Sources whose name has this in it do not load.
    refuse: Option<&'static str>,
    brush: bool,
}

impl FakeBank {
    fn new() -> Arc<FakeBank> {
        Arc::new(FakeBank::default())
    }
    fn asked(&self) -> Vec<Vec<SamplerSource>> {
        self.asked.lock().unwrap().clone()
    }
    fn slot_of(&self, s: &SamplerSource) -> Option<usize> {
        self.keys
            .lock()
            .unwrap()
            .iter()
            .position(|k| *k == s.to_string())
    }
}

impl SamplerSlots for FakeBank {
    fn instrument_sources(
        &self,
        instrument: &str,
        role: &str,
        _seed: f64,
        picked: bool,
    ) -> Option<Vec<SamplerSource>> {
        self.instruments
            .lock()
            .unwrap()
            .push((instrument.into(), role.into(), picked));
        // the bundled catalog knows which instruments have a sampled category
        VoiceCatalog::bundled().category_of.get(instrument)?;
        Some(vec![
            SamplerSource::Sf2(format!("/fake/{instrument}.sf2"), 1),
            SamplerSource::Sf2("/fake/floor.sf2".into(), 1),
        ])
    }

    fn option_sources(&self, option: &str, _role: &str, _seed: f64) -> Option<Vec<SamplerSource>> {
        let c = option.strip_prefix("cat:")?;
        Some(vec![SamplerSource::Sf2(
            format!("/fake/palette-{c}.sf2"),
            7,
        )])
    }

    fn slot_for(&self, sources: &[SamplerSource]) -> Option<usize> {
        self.asked.lock().unwrap().push(sources.to_vec());
        let first = sources
            .iter()
            .find(|s| self.refuse.is_none_or(|r| !s.to_string().contains(r)))?;
        let mut keys = self.keys.lock().unwrap();
        let key = first.to_string();
        Some(match keys.iter().position(|k| *k == key) {
            Some(i) => i,
            None => {
                keys.push(key);
                keys.len() - 1
            }
        })
    }

    fn kit_source(&self, id: &str) -> Option<SamplerSource> {
        let (file, bank, program) = drumkits::parse(id)?;
        (file != "nowhere.sf2").then(|| SamplerSource::Kit {
            path: format!("/fake/{file}"),
            bank,
            program,
        })
    }

    fn brush_kit(&self, kit: &SamplerSource) -> Option<SamplerSource> {
        let SamplerSource::Kit { path, .. } = kit else {
            return None;
        };
        self.brush.then(|| SamplerSource::Kit {
            path: path.clone(),
            bank: 128,
            program: 40,
        })
    }
}

fn with_bank(bank: &Arc<FakeBank>, fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        bank: Some(bank.clone()),
        ..WaveOptions::extended()
    }
}

const KIT: &str = "MuseScore_General.sf2#128:0";

/// SampledKitSupport.kitPhraseProject: kick, snare and hats on beats' drum channels, velocity 100,
/// one bar of 16ths looped at 120 BPM; `kit` is the share's `tracks` override on every drum net.
fn kit_phrase(kit: Option<&str>) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let net = |note: i64, channel: i64, steps: &[i64]| {
        let mut n = NetBundle::ring(16);
        for s in steps {
            n.bind(*s, note, channel, 100, 50);
        }
        n.track.channel = channel;
        n.track.kit = kit.map(String::from);
        n
    };
    let all: Vec<i64> = (0..16).collect();
    p.nets.insert("kick", net(36, 10, &[0, 4, 8, 12]));
    p.nets.insert("snare", net(38, 11, &[4, 12]));
    p.nets.insert("hihat", net(42, 12, &all));
    p
}

fn lane<'a>(p: &'a WaveProject, kind: &str) -> &'a Lane {
    p.lanes()
        .iter()
        .find(|l| l.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} lane"))
}

// ---- the kits ----------------------------------------------------------------------------

/// SampledKitTests.drumLanesPlayTheirGMKeys.
#[test]
fn drum_lanes_play_their_gm_keys() {
    let keys: Vec<i64> = ["kick", "snare", "clap", "hat", "openhat", "crash"]
        .iter()
        .map(|k| gm_note(k))
        .collect();
    assert_eq!(keys, [36, 38, 39, 42, 46, 49]);
    // every kind beats' drum map makes comes back to its own key
    for note in [35, 36, 37, 38, 39, 40, 42, 44, 46] {
        let k = gm_note(drum_kind(note));
        assert_eq!(drum_kind(k), drum_kind(note), "{note}");
    }
    assert_eq!(drumkits::hold_ms("crash"), 2500.0);
    assert_eq!(drumkits::hold_ms("openhat"), 600.0);
    assert_eq!(drumkits::hold_ms("kick"), 400.0);
}

/// SampledKitTests.theParityPathKeepsTheSynthKit: the parity path never samples drums: without
/// the deck's bank and the instrument table a kit pick renders beats' synth kit, bit for bit.
#[test]
fn the_parity_path_keeps_the_synth_kit() {
    let linear = WaveOptions {
        master: Master::Linear,
        ..Default::default()
    };
    let plain = render_mono(&kit_phrase(None), &linear, SR, 1.0);
    let picked = render_mono(&kit_phrase(Some(KIT)), &linear, SR, 1.0);
    assert!(identical(&plain, &picked));
    // the instrument table without a bank
    let x = WaveOptions {
        master: Master::Linear,
        ..WaveOptions::extended()
    };
    assert!(identical(
        &render_mono(&kit_phrase(None), &x, SR, 1.0),
        &render_mono(&kit_phrase(Some(KIT)), &x, SR, 1.0)
    ));
    // a bank without the instrument table: the kit is not asked for either
    let bank = FakeBank::new();
    let o = WaveOptions {
        master: Master::Linear,
        bank: Some(bank.clone()),
        ..Default::default()
    };
    let prepared = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    assert!(prepared.lanes().iter().all(|l| l.sample_slot().is_none()));
    assert!(bank.asked().is_empty());
}

/// A track's drums on a kit: every drum lane on the kit's slot, on its kind's GM key, held as long
/// as the kit holds it, at full velocity; the notes it plays are reported for the host's sampler
/// and the synth kit is silent.
#[test]
fn a_kit_takes_the_drum_lanes() {
    let bank = FakeBank::new();
    let o = with_bank(&bank, Fixes::EMPTY);
    let p = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    let kit = bank.kit_source(KIT).unwrap();
    let slot = bank.slot_of(&kit).expect("the kit loads");
    for (kind, note) in [("kick", 36), ("snare", 38), ("hat", 42)] {
        let l = lane(&p, kind);
        assert_eq!(l.sample_slot(), Some(slot), "{kind}");
        assert_eq!(l.kit(), (note, 400.0, 1.0), "{kind}");
    }
    // one slot, the kit asked for once per lane
    assert!(bank.asked().iter().all(|a| *a == vec![kit.clone()]));
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(p);
    r.play();
    let mut out = vec![0f32; 256];
    let mut notes = Vec::new();
    for _ in 0..400 {
        r.process(&mut out, None, 256);
        assert!(out.iter().all(|x| *x == 0.0), "the synth kit is silent");
        notes.append(&mut r.sample_notes);
    }
    assert!(!notes.is_empty());
    assert!(notes.iter().all(|n| n.slot == slot && n.dur_ms == 400.0));
    let at = |note: i64| notes.iter().filter(|n| n.note == note).count();
    assert!(at(36) > 0 && at(38) > 0 && at(42) > 0);
    assert!(notes.iter().all(|n| [36, 38, 42].contains(&n.note)));
    // velocity 100 of a binding is what a full-velocity kit plays
    assert!(notes.iter().all(|n| n.velocity == 100));
}

/// `drumKitSource`: the first drum-channel net's `Track.kit`, else the palette's
/// (`sound.kit`); control nets and nets on a melodic channel do not count; an id no bank names
/// leaves the synth kit.
#[test]
fn the_kit_is_the_first_drum_nets() {
    let bank = FakeBank::new();
    let o = with_bank(&bank, Fixes::EMPTY);
    // a palette's kit, no pick
    let mut p = kit_phrase(None);
    p.nets.get_mut("kick").unwrap().track.sound.kit = Some(KIT.into());
    let prepared = WaveRunner::prepare(&p, SR, &o, 0);
    assert!(lane(&prepared, "kick").sample_slot().is_some());
    // a kit on a net whose channel is melodic is not a drum kit
    let mut q = kit_phrase(None);
    let mut tonal = NetBundle::ring(16);
    tonal.bind(0, 60, 4, 100, 50);
    tonal.track.channel = 4;
    tonal.track.kit = Some(KIT.into());
    q.nets.insert("lead", tonal);
    let prepared = WaveRunner::prepare(&q, SR, &o, 0);
    assert!(prepared.lanes().iter().all(|l| l.sample_slot().is_none()));
    // the pick wins over the palette's
    let mut both = kit_phrase(Some("Other.sf2#128:8"));
    both.nets.get_mut("kick").unwrap().track.sound.kit = Some(KIT.into());
    let bank2 = FakeBank::new();
    WaveRunner::prepare(&both, SR, &with_bank(&bank2, Fixes::EMPTY), 0);
    assert!(bank2
        .asked()
        .iter()
        .all(|a| a[0].to_string().contains("Other.sf2")));
    // a kit no installed bank names
    let nowhere = kit_phrase(Some("nowhere.sf2#128:0"));
    let prepared = WaveRunner::prepare(&nowhere, SR, &o, 0);
    assert!(prepared.lanes().iter().all(|l| l.sample_slot().is_none()));
}

/// A kit that does not load keeps beats' synth lane (`bindKit` returns false).
#[test]
fn a_kit_that_does_not_load_keeps_the_synth_kit() {
    let bank = Arc::new(FakeBank {
        refuse: Some("kit:"),
        ..Default::default()
    });
    let o = with_bank(&bank, Fixes::EMPTY);
    let p = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    assert!(p.lanes().iter().all(|l| l.sample_slot().is_none()));
    assert!(identical(
        &render_mono(&kit_phrase(Some(KIT)), &o, SR, 0.5),
        &render_mono(&kit_phrase(None), &o, SR, 0.5)
    ));
}

/// `.balance`: the kit was matched to the synth kit before its trims; the kick's trim moves the
/// whole kit as a velocity (`DrumKits.velocityScale`), and a hit's velocity is the binding's
/// times it, at most full.
#[test]
fn the_kicks_trim_moves_the_whole_kit() {
    let bank = FakeBank::new();
    let mut o = with_bank(&bank, Fixes::BALANCE);
    let trims: BTreeMap<String, f64> = [("kick".to_string(), -6.0), ("snare".to_string(), 3.0)]
        .into_iter()
        .collect();
    o.track_balance = Some(trims);
    let p = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    for kind in ["kick", "snare", "hat"] {
        assert_eq!(lane(&p, kind).kit().2, velocity_scale(-6.0), "{kind}");
    }
    let mut r = WaveRunner::new(SR, o);
    r.load_prepared(p);
    r.play();
    let mut out = vec![0f32; 256];
    for _ in 0..40 {
        r.process(&mut out, None, 256);
    }
    let v: Vec<i64> = r.sample_notes.iter().map(|n| n.velocity).collect();
    assert!(!v.is_empty());
    // 100/127 x 10^(-6/40) = 0.558 -> 71
    assert!(v.iter().all(|x| *x == 71), "{v:?}");
    // a boost clips at full velocity
    let mut loud = with_bank(&bank, Fixes::BALANCE);
    loud.track_balance = Some([("kick".to_string(), 12.0)].into_iter().collect());
    let mut r = WaveRunner::new(SR, loud.clone());
    r.load_prepared(WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &loud, 0));
    r.play();
    for _ in 0..40 {
        r.process(&mut out, None, 256);
    }
    assert!(r
        .sample_notes
        .iter()
        .all(|n| n.velocity <= 127 && n.velocity >= 100));
    // no trims: untouched
    let plain = WaveRunner::prepare(
        &kit_phrase(Some(KIT)),
        SR,
        &with_bank(&bank, Fixes::EMPTY),
        0,
    );
    assert!(plain.lanes().iter().all(|l| l.kit().2 == 1.0));
}

/// BrushTests.aSampledSnarePlaysTheBrushKit: on a sampled kit the snare lane plays the bank's
/// Brush kit (another slot than the kit's), at full velocity; the other lanes keep the kit. A bank
/// without one plays the kit's own snare, softer (`Brush.stockVelocity`).
#[test]
fn a_sampled_snare_plays_the_brush_kit() {
    let bank = Arc::new(FakeBank {
        brush: true,
        ..Default::default()
    });
    let mut o = with_bank(&bank, Fixes::EMPTY);
    let plain = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    o.brushes = true;
    let brushed = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
    let (snare, kick) = (lane(&brushed, "snare"), lane(&brushed, "kick"));
    assert!(snare.sample_slot().is_some());
    assert_ne!(snare.sample_slot(), kick.sample_slot());
    assert_eq!(snare.kit().2, 1.0);
    assert_eq!(
        lane(&plain, "snare").sample_slot(),
        lane(&plain, "kick").sample_slot()
    );
    assert_eq!(kick.sample_slot(), lane(&plain, "kick").sample_slot());
    // the snare still plays the snare key, on the Brush kit
    assert_eq!(snare.kit().0, 38);
    // no Brush kit in the bank: the kit's own snare, softer; and a bank that cannot load the
    // Brush kit falls back the same way
    for bank in [
        FakeBank::new(),
        Arc::new(FakeBank {
            brush: true,
            refuse: Some("#128:40"),
            ..Default::default()
        }),
    ] {
        let mut o = with_bank(&bank, Fixes::EMPTY);
        o.brushes = true;
        let p = WaveRunner::prepare(&kit_phrase(Some(KIT)), SR, &o, 0);
        assert_eq!(
            lane(&p, "snare").sample_slot(),
            lane(&p, "kick").sample_slot()
        );
        assert_eq!(lane(&p, "snare").kit().2, Brush::stock_velocity());
        assert_eq!(lane(&p, "kick").kit().2, 1.0);
    }
}

/// `.choke` on a sampled kit: a closed hat sounding ends the open hat's note on the sampler (a
/// raw note-off for the open hat's key), once and only while it is held; a hat that follows
/// long after the open hat's hold, or no open hat in the net, sends nothing; a key struck again
/// while held is ended first.
#[test]
fn a_closed_hat_ends_the_kits_open_hat() {
    let bank = FakeBank::new();
    let o = with_bank(&bank, Fixes::OPEN_HATS | Fixes::CHOKE);
    let mut hats = NetBundle::ring(32);
    hats.bind(2, 46, 10, 110, 50);
    hats.bind(3, 42, 10, 90, 50);
    // far from an open hat (its hold is 600 ms; 3 steps are 375 ms at 120 BPM)
    hats.bind(20, 42, 10, 90, 50);
    hats.bind(24, 46, 10, 110, 50);
    hats.bind(28, 46, 10, 110, 50);
    hats.track.channel = 10;
    hats.track.kit = Some(KIT.into());
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert("hihat", hats);
    let prepared = WaveRunner::prepare(&p, SR, &o, 0);
    let (hat, open) = (lane(&prepared, "hat"), lane(&prepared, "openhat"));
    assert_eq!(
        hat.chokes(),
        prepared.lanes().iter().position(|l| l.kind == "openhat")
    );
    assert_eq!(hat.sample_slot(), open.sample_slot());
    assert_eq!(open.kit(), (46, 600.0, 1.0));
    let mut r = WaveRunner::new(SR, o);
    r.load_prepared(prepared);
    r.play();
    let mut out = vec![0f32; 256];
    let (mut keys, mut notes) = (Vec::new(), Vec::new());
    for b in 0..1000 {
        r.process_at(&mut out, None, 256, b * 256);
        keys.append(&mut r.sample_keys);
        notes.append(&mut r.sample_notes);
    }
    let step = (0.125 * SR) as i64;
    // the closed hat on step 3 ends the open hat of step 2
    let offs: Vec<_> = keys.iter().filter(|k| k.status == 0x80).collect();
    assert_eq!(offs.len(), 3, "{keys:?}");
    assert!(offs.iter().all(|k| k.data1 == 46 && k.data2 == 0));
    assert!((offs[0].frame - 3 * step).abs() <= 2, "{}", offs[0].frame);
    // the closed hat on step 20 finds no open hat held; the open hat struck again on step 28
    // while held from step 24 (500 ms < 600 ms) is ended first
    assert!((offs[1].frame - 28 * step).abs() <= 2, "{}", offs[1].frame);
    // (the ring comes round: step 34 opens the hat again and step 35 closes it)
    assert!((offs[2].frame - 35 * step).abs() <= 2, "{}", offs[2].frame);
    assert!(notes.iter().any(|n| n.note == 46) && notes.iter().any(|n| n.note == 42));
}

// ---- bindSampler -------------------------------------------------------------------------

fn lead_project(instrument: &str, group: Option<&str>) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(16);
    n.bind(0, 60, 4, 100, 100);
    n.track.channel = 4;
    n.track.group = group.map(String::from);
    n.track.instrument = instrument.into();
    p.nets.insert("melody", n);
    p
}

fn tonal_instrument() -> String {
    // an instrument the bundled catalog sorts into a sampled category
    VoiceCatalog::bundled()
        .category_of
        .keys()
        .find(|k| !k.is_empty())
        .cloned()
        .expect("a catalog instrument")
}

/// `bindSampler`: the instrument's sounds, ranked; the lane plays the first the bank loads, and
/// the bank is asked with the whole list.
#[test]
fn a_tonal_lane_binds_through_the_instruments_sounds() {
    let inst = tonal_instrument();
    let bank = FakeBank::new();
    let o = with_bank(&bank, Fixes::EMPTY);
    let p = WaveRunner::prepare(&lead_project(&inst, Some("melody")), SR, &o, 0);
    let l = &p.lanes_of("melody")[0];
    assert_eq!(l.sample_slot(), Some(0));
    let first = bank.asked()[0].clone();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0], SamplerSource::Sf2(format!("/fake/{inst}.sf2"), 1));
    // an instrument the catalog has no category for plays its synth voice
    let none = WaveRunner::prepare(
        &lead_project("no-such-instrument", Some("melody")),
        SR,
        &o,
        0,
    );
    assert!(none.lanes_of("melody")[0].sample_slot().is_none());
    // a bank that cannot load any of them leaves the synth voice
    let refusing = Arc::new(FakeBank {
        refuse: Some("sf2:"),
        ..Default::default()
    });
    let none = WaveRunner::prepare(
        &lead_project(&inst, Some("melody")),
        SR,
        &with_bank(&refusing, Fixes::EMPTY),
        0,
    );
    assert!(none.lanes_of("melody")[0].sample_slot().is_none());
}

/// `bindSampler`: the role is the lane's group, else its net's riff group, else "melody" for an
/// empty one; the track's seed ranks the sounds.
#[test]
fn the_role_is_the_lanes_group() {
    let inst = tonal_instrument();
    let bank = FakeBank::new();
    let o = WaveOptions {
        voicing_seed: 9.0,
        ..with_bank(&bank, Fixes::EMPTY)
    };
    WaveRunner::prepare(&lead_project(&inst, Some("arp")), SR, &o, 0);
    WaveRunner::prepare(&lead_project(&inst, None), SR, &o, 0);
    let asked = bank.instruments.lock().unwrap().clone();
    // (the live voice asks too, as the melody)
    assert!(
        asked.contains(&(inst.clone(), "arp".into(), false)),
        "{asked:?}"
    );
    assert!(
        asked.contains(&(inst.clone(), "melody".into(), false)),
        "{asked:?}"
    );
}

/// `bindSampler`'s `option` (`TrackSound.option`, what the palette or the user chose for the
/// role): a synth voice plays no sample; a sampled option's sounds come first, then the
/// instrument's own as the fallback; the user's `picked` instrument ignores the palette.
#[test]
fn a_palette_option_comes_first_and_a_pick_ignores_it() {
    let inst = tonal_instrument();
    let make = |picked: bool, option: Option<&str>| {
        let mut p = lead_project(&inst, Some("melody"));
        let t = &mut p.nets.get_mut("melody").unwrap().track;
        t.picked = picked;
        t.sound.option = option.map(String::from);
        p
    };
    let run = |p: &Project| {
        let bank = FakeBank::new();
        let prepared = WaveRunner::prepare(p, SR, &with_bank(&bank, Fixes::EMPTY), 0);
        let slot = prepared.lanes_of("melody")[0].sample_slot();
        let instruments = bank.instruments.lock().unwrap().clone();
        (slot, bank.asked(), instruments)
    };
    // an option that plays a synth voice: no sample, and the bank is not asked for the net
    for option in ["synth", "synth:warm-pad"] {
        let (slot, asked, _) = run(&make(false, Some(option)));
        assert!(slot.is_none(), "{option}");
        // (only the live keys' voice, which has no option of its own, asks)
        assert!(
            asked.iter().all(|a| !a[0].to_string().contains("palette")),
            "{option}"
        );
    }
    // a sampled option: its sounds, then the instrument's
    let (slot, asked, _) = run(&make(false, Some("cat:pad")));
    assert_eq!(slot, Some(0));
    assert_eq!(
        asked[0],
        vec![
            SamplerSource::Sf2("/fake/palette-pad.sf2".into(), 7),
            SamplerSource::Sf2(format!("/fake/{inst}.sf2"), 1),
            SamplerSource::Sf2("/fake/floor.sf2".into(), 1),
        ]
    );
    // an option the palette has no sounds for: the instrument's own
    let (_, asked, _) = run(&make(false, Some("own")));
    assert_eq!(
        asked[0][0],
        SamplerSource::Sf2(format!("/fake/{inst}.sf2"), 1)
    );
    // the user's pick: the palette does not apply (a synth option included), and the catalog is
    // asked for the pick by name
    for option in ["cat:pad", "synth"] {
        let (slot, asked, instruments) = run(&make(true, Some(option)));
        assert_eq!(slot, Some(0), "{option}");
        assert_eq!(
            asked[0][0],
            SamplerSource::Sf2(format!("/fake/{inst}.sf2"), 1)
        );
        assert!(instruments.iter().any(|i| i.0 == inst && i.2), "{option}");
    }
}

/// `bindSampler` needs the instrument table too (`opts.instruments != nil`): a bank on the
/// parity options binds nothing.
#[test]
fn a_bank_without_the_instrument_table_binds_nothing() {
    let inst = tonal_instrument();
    let bank = FakeBank::new();
    let o = WaveOptions {
        master: Master::Linear,
        bank: Some(bank.clone()),
        ..Default::default()
    };
    let p = WaveRunner::prepare(&lead_project(&inst, Some("melody")), SR, &o, 0);
    assert!(p.lanes().iter().all(|l| l.sample_slot().is_none()));
    assert!(bank.asked().is_empty());
}

/// The live keys' voice binds to the melody net's instrument and palette option.
#[test]
fn the_live_voice_follows_the_melodys_option() {
    let inst = tonal_instrument();
    let bank = FakeBank::new();
    let mut p = lead_project(&inst, Some("melody"));
    p.nets.get_mut("melody").unwrap().track.sound.option = Some("cat:pad".into());
    let prepared = WaveRunner::prepare(&p, SR, &with_bank(&bank, Fixes::EMPTY), 0);
    assert_eq!(prepared.live_lane().sample_slot(), Some(0));
    // asked twice (the net and the live voice), both with the palette's sounds first
    let asked = bank.asked();
    assert_eq!(asked.len(), 2);
    assert!(asked
        .iter()
        .all(|a| a[0] == SamplerSource::Sf2("/fake/palette-pad.sf2".into(), 7)));
    p.nets.get_mut("melody").unwrap().track.sound.option = Some("synth".into());
    let b2 = FakeBank::new();
    let prepared = WaveRunner::prepare(&p, SR, &with_bank(&b2, Fixes::EMPTY), 0);
    assert!(prepared.live_lane().sample_slot().is_none());
    assert!(b2.asked().is_empty());
}

// ---- the sound panel on a sampled channel ------------------------------------------------

/// `WaveProject.channelVoice` / `voiceParams` / `applyVoice`: what the sound panel reads and
/// writes (`WaveDeckSource.setVoice`): a channel's instrument and its own synth voice, the tweak
/// made concrete for it, and the lanes of that channel (only) taking it before the hand-over.
#[test]
fn the_panel_reads_a_channels_voice_and_sets_its_lanes() {
    use djrust::wave::voice_tweak::{VoiceSetting, VoiceTweak};
    let inst = tonal_instrument();
    let mut p = lead_project(&inst, Some("melody"));
    let mut drums = NetBundle::ring(16);
    drums.bind(0, 36, 10, 100, 50);
    drums.track.channel = 10;
    p.nets.insert("kick", drums);
    let mut prepared = WaveRunner::prepare(&p, SR, &WaveOptions::extended(), 0);
    let lead = prepared.channel_voice(4).expect("the lead's channel");
    assert_eq!(lead.instrument, inst);
    assert!(lead.tonal && !lead.sampled);
    assert_eq!(lead.pluck, lead.voice.engine == "pluck");
    assert_eq!(lead.voice, prepared.lanes_of("melody")[0].voice);
    let kick = prepared.channel_voice(10).expect("the drums' channel");
    assert!(!kick.tonal && !kick.pluck);
    assert!(prepared.channel_voice(9).is_none());

    let tweak = VoiceTweak {
        gain_db: Some(6.0),
        lowpass: Some(900.0),
        pitch_from: Some(2.0),
        pitch_to: Some(1.0),
        pitch_time: Some(0.1),
        ..Default::default()
    };
    let setting = VoiceSetting {
        tweak,
        use_synth: false,
        instrument: inst.clone(),
    };
    let params = prepared.voice_params(4, &setting, SR);
    assert!(params.mask() != 0);
    // (the instrument's own voice fills what the tweak sets only in part)
    let own = VoiceLaneParams::new(4, &tweak, false, Some(&lead.voice), SR);
    assert_eq!(params.mask(), own.mask());
    assert_eq!(params.pitch_from(), own.pitch_from());
    assert_eq!(params.pitch_to(), own.pitch_to());
    // an unknown channel is still a valid (voiceless) set of parameters
    assert_eq!(prepared.voice_params(9, &setting, SR).mask(), params.mask());

    let before: Vec<_> = prepared.lanes().iter().map(|l| l.voice_state()).collect();
    prepared.apply_voice(&params);
    for (b, l) in before.iter().zip(prepared.lanes()) {
        assert_eq!(*b != l.voice_state(), l.channel == 4, "{}", l.net_id);
    }
    // the default setting puts every value back
    let reset = prepared.voice_params(
        4,
        &VoiceSetting {
            tweak: VoiceTweak::default(),
            use_synth: false,
            instrument: inst,
        },
        SR,
    );
    prepared.apply_voice(&reset);
    let after: Vec<_> = prepared.lanes().iter().map(|l| l.voice_state()).collect();
    assert_eq!(before, after);
}

/// `Lane.setVoice` on a sampled lane: "Use synth voice" plays the lane's own synth voice, and
/// taking it off puts the sample back (the synth voices stopped first); the slot it was bound to
/// stays `sampled_slot`. Through the runner, the lane's own held notes end and no others
/// (`applyVoice`: a note-off for each of the lane's pending notes): the host is asked to end
/// the lane's, not sent all notes off for the slot.
#[test]
fn a_sampled_lane_goes_to_its_synth_voice_and_back() {
    use djrust::wave::voice_tweak::{VoiceSetting, VoiceTweak};
    let inst = tonal_instrument();
    let bank = FakeBank::new();
    let o = with_bank(&bank, Fixes::EMPTY);
    let p = lead_project(&inst, Some("melody"));
    let mut r = WaveRunner::new(SR, o.clone());
    let mut prepared = WaveRunner::prepare(&p, SR, &o, 0);
    assert!(prepared.channel_voice(4).unwrap().sampled);
    let setting = |synth: bool| VoiceSetting {
        tweak: VoiceTweak::default(),
        use_synth: synth,
        instrument: inst.clone(),
    };
    let synth = prepared.voice_params(4, &setting(true), SR);
    let sample = prepared.voice_params(4, &setting(false), SR);
    // before the hand-over, on the project
    prepared.apply_voice(&synth);
    let lane = &prepared.lanes_of("melody")[0];
    assert_eq!((lane.sample_slot(), lane.sampled_slot()), (None, Some(0)));
    prepared.apply_voice(&sample);
    let lane = &prepared.lanes_of("melody")[0];
    assert_eq!(
        (lane.sample_slot(), lane.sampled_slot()),
        (Some(0), Some(0))
    );
    // on the runner: the switch ends the lane's notes
    r.load_prepared(prepared);
    let uid = r.project().unwrap().lanes_of("melody")[0].uid();
    assert_ne!(uid, 0, "a lane of a prepared project is named");
    r.set_voice(&sample);
    assert!(r.sample_keys.is_empty() && r.lane_ends.is_empty());
    r.set_voice(&synth);
    // (not CC 123 for the slot: that would cut a live key and the slot's other lanes)
    assert!(r.sample_keys.is_empty(), "{:?}", r.sample_keys);
    let ends = r.lane_ends.clone();
    r.lane_ends.clear();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].lane, uid);
    // now on its synth voice: struck notes sound, none is reported for the sampler
    r.play();
    let mut out = vec![0f32; 256];
    for _ in 0..200 {
        r.process(&mut out, None, 256);
    }
    assert!(
        r.sample_notes.iter().all(|n| n.slot != 0),
        "{:?}",
        r.sample_notes
    );
    r.set_voice(&sample);
    assert_eq!(
        r.project().unwrap().lanes_of("melody")[0].sample_slot(),
        Some(0)
    );
}
