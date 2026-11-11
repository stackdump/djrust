//! The vocoder strip, offline (docs/VOCODER.md): a mic shaping the TD-3 on its own strip or as
//! an insert, a captured loop shaping the internal synth, the binding targets, the action
//! trace, and no allocation on the render thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::fx::vocoder::{CarrierSource, ModulatorSource, VocoderParam};
use djrust::host::deck::vocoder::{CarrierPick, ModulatorPick, VocoderSwitch};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

struct Counting;
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
}
#[global_allocator]
static A: Counting = Counting;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const MIC: usize = 0;
const TD3: usize = 1;

struct Rig {
    e: Engine,
    r: Renderer,
    mic: rtrb::Producer<f32>,
    td3: rtrb::Producer<f32>,
    t: usize,
    /// The mic speaks (a 300 Hz tone with a 3 kHz edge); false: silence.
    talking: bool,
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.inputs = vec!["MIC".into(), "TD-3".into()];
        let (mut e, r) = engine(c);
        let (mic, _) = e.take_input_feed(MIC).unwrap();
        let (td3, _) = e.take_input_feed(TD3).unwrap();
        for i in [MIC, TD3] {
            e.set_input_trim(i, 0.0);
            e.set_input_volume(i, 1.0);
        }
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        Rig {
            e,
            r,
            mic,
            td3,
            t: 0,
            talking: true,
            out: Vec::new(),
        }
    }

    fn run(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.out.clear();
        for _ in 0..blocks {
            for k in 0..BLOCK {
                let t = (self.t + k) as f64 / SR;
                let voice = if self.talking {
                    0.4 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()
                        + 0.1 * (2.0 * std::f64::consts::PI * 3000.0 * t).sin()
                } else {
                    0.0
                };
                // a 110 Hz saw on the TD-3
                let saw = 0.5 * (2.0 * ((110.0 * t) % 1.0) - 1.0);
                let _ = self.mic.push(voice as f32);
                let _ = self.td3.push(saw as f32);
            }
            self.t += BLOCK;
            self.r.render_block(&mut l, &mut rr);
            self.e.sync_history_tempo();
            self.out.extend_from_slice(&l);
        }
    }

    /// RMS of the last run's second half.
    fn level(&self) -> f32 {
        let x = &self.out[self.out.len() / 2..];
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }
}

#[test]
fn off_it_adds_nothing() {
    let mut a = Rig::new();
    a.run(200);
    let mut b = Rig::new();
    b.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    b.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    b.run(200);
    assert_eq!(a.out, b.out);
}

#[test]
fn a_mic_shapes_the_td3_on_the_vocoder_strip() {
    let mut g = Rig::new();
    // both sources' own faders down: the vocoder taps them pre-fader
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.run(200);
    let talking = g.level();
    assert!(talking > 0.02, "vocoded {talking}");
    let (m, c, out) = g.e.vocoder.mirror.levels();
    assert!(m > 0.1 && c > 0.1 && out > 0.01, "{m} {c} {out}");
    assert!(g.e.vocoder.mirror.envelopes(16).iter().any(|e| *e > 0.01));
    // the mic goes quiet: so does the vocoder (the noise blend has nothing to open it)
    g.talking = false;
    g.run(200);
    assert!(g.level() < talking * 0.05, "{} vs {talking}", g.level());
    // the strip's mute
    g.talking = true;
    g.e.vocoder_switch(VocoderSwitch::Mute, true);
    g.run(200);
    assert!(g.level() < 1e-4);
}

#[test]
fn as_an_insert_it_replaces_the_carriers_strip_signal() {
    let mut g = Rig::new();
    g.e.set_input_volume(MIC, 0.0);
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_switch(VocoderSwitch::Insert, true);
    g.e.vocoder_set(VocoderParam::Volume, 0.0); // V's fader plays no part in an insert
    g.run(200);
    let wet = g.level();
    assert!(wet > 0.02, "{wet}");
    // the TD-3's own fader now sets the vocoded level
    g.e.set_input_volume(TD3, 0.0);
    g.run(200);
    assert!(g.level() < 1e-4);
    // dry / wet 0 is the TD-3 as it was
    g.e.set_input_volume(TD3, 1.0);
    g.e.vocoder_set(VocoderParam::Mix, 0.0);
    g.run(200);
    let dry = g.level();
    let mut plain = Rig::new();
    plain.e.set_input_volume(MIC, 0.0);
    plain.run(200);
    assert!(
        (dry - plain.level()).abs() < 1e-3,
        "{dry} vs {}",
        plain.level()
    );
}

#[test]
fn a_deck_can_be_the_carrier_as_an_insert() {
    let mut g = Rig::new();
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    assert!(g
        .e
        .generate("A", &djrust::TrackSpec::new("techno", 7.0, None)));
    g.e.set_volume("A", 1.0);
    g.e.set_crossfader(0.0);
    g.e.play("A");
    g.run(400);
    let dry = g.level();
    assert!(dry > 0.01, "the deck plays: {dry}");
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Deck("A".into())).unwrap();
    g.e.vocoder_switch(VocoderSwitch::Insert, true);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.talking = false;
    g.e.vocoder_set(VocoderParam::Noise, 0.0);
    g.run(400);
    assert!(
        g.level() < dry * 0.1,
        "a silent mic closes the deck: {} vs {dry}",
        g.level()
    );
    assert!(g.e.vocoder_carrier(CarrierPick::Deck("Z".into())).is_err());
}

#[test]
fn a_captured_loop_shapes_the_synth() {
    let mut g = Rig::new();
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    g.run(2 * 24_000 / BLOCK + 4); // two beats of history at 120
    g.e.vocoder_modulator(ModulatorPick::Capture(MIC, Some(1.0)))
        .unwrap();
    match &g.e.vocoder.modulator {
        ModulatorSource::Loop(l) => {
            assert_eq!((l.kind.as_str(), l.beats), ("capture", 1.0));
            assert!((l.bpm - 120.0).abs() < 1e-6);
        }
        o => panic!("{o:?}"),
    }
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Synth);
    g.e.vocoder_set_notes(&[45, 52, 57]);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    // the mic goes quiet: the loop keeps talking
    g.talking = false;
    g.run(200);
    assert!(g.level() > 0.01, "{}", g.level());
    // as an insert a synth carrier still plays on V (it has no strip of its own)
    g.e.vocoder_switch(VocoderSwitch::Insert, true);
    g.run(50);
    assert!(g.level() > 0.01);
    // no notes and no noise blend: silence (the noise alone still whispers the loop)
    let with_notes = g.level();
    g.e.vocoder_set_notes(&[]);
    g.run(200);
    let noise_only = g.level();
    assert!(noise_only < with_notes, "{noise_only} vs {with_notes}");
    g.e.vocoder_set(VocoderParam::Noise, 0.0);
    g.run(200);
    assert!(g.level() < 1e-3, "{}", g.level());
    let j = g.e.vocoder_json();
    assert_eq!(j.get("routing").and_then(|v| v.as_str()), Some("strip"));
    assert!(j.get("loop_position").is_some());
}

#[test]
fn the_formant_filter_works_alone_on_the_carrier() {
    let mut g = Rig::new();
    g.e.set_input_volume(TD3, 0.0);
    g.e.set_input_volume(MIC, 0.0);
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.run(100);
    let through = g.level();
    assert!(through > 0.1, "no modulator passes the carrier: {through}");
    g.e.vocoder_set(VocoderParam::VowelMix, 1.0);
    g.e.vocoder_set(VocoderParam::Vowel, 2.0);
    g.run(100);
    let vowel = g.level();
    assert!(
        vowel > 0.005 && (vowel - through).abs() > 0.01,
        "{vowel} vs {through}"
    );
}

fn target(id: &str, value: f64, index: usize, pressed: bool, args: &[(&str, &str)]) -> Action {
    Action::Target {
        id: id.into(),
        deck: None,
        slot: None,
        index,
        value,
        pressed,
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn bindings_drive_it_and_the_trace_says_so() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "APC".into();
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "on")]),
    );
    assert!(g.e.vocoder.settings.on);
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "carrier")]),
    );
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Input("MIC".into()));
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "carrier_prev")]),
    );
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "carrier_prev")]),
    );
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Synth);
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "modulator")]),
    );
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Input("MIC".into()));
    // a knob, twice: one folded line
    SurfaceHost::perform(
        &mut g.e,
        target("vocoder", 0.25, 0, false, &[("param", "shift")]),
    );
    SurfaceHost::perform(
        &mut g.e,
        target("vocoder", 0.75, 0, false, &[("param", "shift")]),
    );
    assert!((g.e.vocoder.settings.shift - 6.0).abs() < 1e-9);
    // keys, transposed
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderKeys", 1.0, 48, true, &[("transpose", "12")]),
    );
    assert_eq!(
        g.e.vocoder.notes.iter().copied().collect::<Vec<_>>(),
        vec![60]
    );
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderKeys", 0.0, 48, false, &[("transpose", "12")]),
    );
    assert!(g.e.vocoder.notes.is_empty());
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "nope")]),
    );
    assert!(g.e.unhandled.last().unwrap().contains("vocoderSwitch"));

    let t = g.e.trace.take().unwrap();
    let texts: Vec<&str> = t.iter().map(|x| x.entry.text.as_str()).collect();
    // each move is also the `vocoder` live op that makes it again (an arrangement keeps it)
    assert!(
        t.iter()
            .all(|x| x.ops.len() == 1 && x.ops[0].op() == "vocoder"),
        "a vocoder op each"
    );
    assert!(
        t.iter().all(|x| x.entry.key.starts_with("vocoder:")),
        "{texts:?}"
    );
    assert!(t.iter().all(|x| x.entry.source == "APC"));
    assert!(
        texts.iter().any(|s| s.starts_with("vocoder on")),
        "{texts:?}"
    );
    assert!(texts.contains(&"vocoder carrier: synth"), "{texts:?}");
    assert!(texts.contains(&"vocoder shift -6.0 st") && texts.contains(&"vocoder shift +6.0 st"));
    assert!(t
        .iter()
        .filter(|x| x.entry.key == "vocoder:shift")
        .all(|x| x.entry.fold));
    assert!(texts.contains(&"vocoder keys: C4"), "{texts:?}");
}

#[test]
fn the_render_thread_does_not_allocate() {
    let mut g = Rig::new();
    g.run(2 * 24_000 / BLOCK + 4);
    g.e.vocoder_modulator(ModulatorPick::Capture(MIC, Some(1.0)))
        .unwrap();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_set_notes(&[57, 60, 64]);
    g.e.vocoder_set(VocoderParam::Bands, 32.0);
    g.e.vocoder_set(VocoderParam::VowelMix, 0.5);
    // the whole synth voice in use: filter and envelope, sync, ring, drift, a noise osc 3
    g.e.synth_preset_load("sync lead").unwrap();
    g.e.vocoder_set(VocoderParam::Ring, 0.5);
    g.e.vocoder_set(VocoderParam::Osc3Wave, 4.0);
    g.e.vocoder_set(VocoderParam::Osc3Level, 0.3);
    g.e.vocoder_set(VocoderParam::Drift, 1.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_switch(VocoderSwitch::Pfl, true);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut cl, mut cr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    // the messages land on the first block; count the ones after
    g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..50 {
        g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0);
    assert!(cl.iter().any(|x| x.abs() > 1e-4), "PFL puts V in the cue");
    // swapped (the synth the modulator, the loop the carrier), frozen, on the crossfader
    g.e.vocoder_swap().unwrap();
    g.e.vocoder_switch(VocoderSwitch::Freeze, true);
    g.e.vocoder_switch(VocoderSwitch::Xfade, true);
    g.e.set_crossfader(0.7);
    g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..50 {
        g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0, "swapped, frozen, morphing");
}

/// A deck as the modulator (`deck:A`): tapped live before its fader, so whatever is loaded on
/// it drives the vocoder, the deck's fader down or not, and a new track on it carries on.
#[test]
fn a_deck_drives_the_vocoder_live() {
    let mut g = Rig::new();
    g.talking = false;
    g.e.set_volume("A", 0.0); // pre-fader: the deck need not be heard
    assert!(g.e.generate("A", &djrust::TrackSpec::new("techno", 42.0, None)));
    g.e.play("A");
    g.e.vocoder_modulator(ModulatorPick::Deck("A".into())).unwrap();
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Deck("A".into()));
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_set_notes(&[45, 52, 57]);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.run(400);
    let playing = g.level();
    let (m, _, _) = g.e.vocoder.mirror.levels();
    assert!(m > 0.01, "the deck reaches the modulator: {m}");
    assert!(playing > 0.005, "vocoded {playing}");
    // swap what is on the deck: the modulator is still the deck, and it still drives it
    assert!(g.e.generate("A", &djrust::TrackSpec::new("house", 7.0, None)));
    g.e.play("A");
    g.run(400);
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Deck("A".into()));
    assert!(g.level() > 0.005, "after the swap {}", g.level());
    // the deck stops: its signal leaves the modulator (only the tail of its effects is left)
    g.e.pause("A");
    g.run(800);
    let (after, _, _) = g.e.vocoder.mirror.levels();
    assert!(after < m * 0.25, "modulator {after} vs {m} playing");
    let j = g.e.vocoder_json();
    let src = j.get("modulator").unwrap();
    assert_eq!(src.get("kind").and_then(|k| k.as_str()), Some("deck"));
    assert_eq!(src.get("deck").and_then(|k| k.as_str()), Some("A"));
}

/// A deck cannot be both the carrier and the modulator: either way round, the second pick is
/// refused and nothing changes.
#[test]
fn the_same_deck_cannot_be_carrier_and_modulator() {
    let mut g = Rig::new();
    g.e.vocoder_carrier(CarrierPick::Deck("A".into())).unwrap();
    assert!(g.e.vocoder_modulator(ModulatorPick::Deck("A".into())).is_err());
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::None);
    g.e.vocoder_modulator(ModulatorPick::Deck("B".into())).unwrap();
    assert!(g.e.vocoder_carrier(CarrierPick::Deck("B".into())).is_err());
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Deck("A".into()));
    // the cycles step over the other side's deck
    for _ in 0..12 {
        g.e.vocoder_cycle_carrier(1).unwrap();
        assert_ne!(g.e.vocoder.carrier, CarrierSource::Deck("B".into()));
        assert_eq!(g.e.vocoder.modulator, ModulatorSource::Deck("B".into()));
    }
    g.e.vocoder_carrier(CarrierPick::Deck("A".into())).unwrap();
    let mut seen_b = false;
    for _ in 0..12 {
        g.e.vocoder_cycle_modulator(1).unwrap();
        assert_ne!(g.e.vocoder.modulator, ModulatorSource::Deck("A".into()));
        seen_b |= g.e.vocoder.modulator == ModulatorSource::Deck("B".into());
        assert_eq!(g.e.vocoder.carrier, CarrierSource::Deck("A".into()));
    }
    assert!(seen_b, "the modulator cycle takes in the decks");
}

/// Switching the carrier, every way it can be switched, never changes the modulator (seen live:
/// carrier = synth turned the modulator into input TD-3).
#[test]
fn switching_the_carrier_never_changes_the_modulator() {
    let mut g = Rig::new();
    g.run(50);
    let mods = [
        ModulatorPick::None,
        ModulatorPick::Input(MIC),
        ModulatorPick::Deck("A".into()),
    ];
    for m in mods {
        g.e.vocoder_modulator(m.clone()).unwrap();
        let want = g.e.vocoder.modulator.clone();
        let picks = [
            CarrierPick::Synth,
            CarrierPick::Input(TD3),
            CarrierPick::Deck("B".into()),
            CarrierPick::None,
            CarrierPick::Synth,
        ];
        for c in picks {
            g.e.vocoder_carrier(c).unwrap();
            assert_eq!(g.e.vocoder.modulator, want, "{m:?}");
        }
        for op in ["synth", "carrier", "carrier_prev", "carrier", "on", "insert", "insert", "on"] {
            g.e.vocoder_target(op, None, None).unwrap();
            assert_eq!(g.e.vocoder.modulator, want, "{m:?} after {op}");
        }
        g.e.vocoder_cycle_carrier(-1).unwrap();
        g.run(20);
        assert_eq!(g.e.vocoder.modulator, want);
    }
}

/// The synth as a jam synth on V: with no modulator it plays straight; with one picked,
/// bypass leaves the modulator out so it still plays straight (a silent mic would close it).
#[test]
fn the_synth_jams_on_v_without_a_modulator_and_with_bypass() {
    let mut g = Rig::new();
    g.talking = false;
    g.e.set_input_volume(TD3, 0.0); // only V in the master
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.e.vocoder_note(57, true);
    g.run(200);
    let alone = g.level();
    assert!(alone > 0.05, "no modulator: the synth on V {alone}");
    // a modulator (a silent mic) shuts the vocoded synth
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.run(200);
    assert!(g.level() < alone * 0.1, "vocoded by silence {}", g.level());
    // bypass: straight again, the modulator still picked
    g.e.trace = Some(Vec::new());
    g.e.vocoder_switch(VocoderSwitch::Bypass, true);
    g.run(200);
    assert!((g.level() - alone).abs() < alone * 0.1, "{} vs {alone}", g.level());
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Input("MIC".into()));
    let t = g.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "vocoder bypassed: synth straight onto V");
    g.e.vocoder_switch(VocoderSwitch::Bypass, false);
    g.run(200);
    assert!(g.level() < alone * 0.1);
}

/// Every synth knob is a vocoder param: bindable as `vocoder` (param = its name), set over
/// the socket's knobs, and logged in its units; the keys play it.
#[test]
fn the_synth_knobs_bind_play_and_log() {
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    g.e.trace_via = "MPK".into();
    // K2 on the cutoff, K3 on osc 2's wave, the APC fader on the resonance
    SurfaceHost::perform(&mut g.e, target("vocoder", 0.5, 0, false, &[("param", "cutoff")]));
    SurfaceHost::perform(&mut g.e, target("vocoder", 0.5, 1, false, &[("param", "osc2_wave")]));
    SurfaceHost::perform(&mut g.e, target("vocoder", 0.8, 2, false, &[("param", "resonance")]));
    SurfaceHost::perform(&mut g.e, target("vocoder", 1.0, 3, false, &[("param", "sync")]));
    let s = &g.e.vocoder.settings;
    assert!((s.synth.cutoff - VocoderParam::Cutoff.from_knob(0.5)).abs() < 1e-6);
    assert_eq!(s.synth.osc[1].wave, djrust::fx::eurosynth::Wave::Triangle);
    assert!((s.synth.resonance - 0.8).abs() < 1e-9);
    assert!(s.synth.sync);
    let lines: Vec<String> = g.e.trace.take().unwrap().into_iter().map(|t| t.entry.text).collect();
    assert!(lines[0].starts_with("vocoder cutoff ") && lines[0].ends_with(" Hz"), "{lines:?}");
    assert_eq!(lines[1], "vocoder osc2_wave triangle");
    assert_eq!(lines[2], "vocoder resonance 0.80");
    assert_eq!(lines[3], "vocoder sync on");
    // the keys (vocoderKeys on the MPK) play it on V
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    SurfaceHost::perform(&mut g.e, target("vocoderKeys", 1.0, 45, true, &[]));
    g.run(200);
    assert!(g.level() > 0.02, "{}", g.level());
    // the bypass and the presets from a pad
    SurfaceHost::perform(&mut g.e, target("vocoderSwitch", 1.0, 0, true, &[("op", "bypass")]));
    assert!(g.e.vocoder.settings.bypass);
    SurfaceHost::perform(
        &mut g.e,
        target("vocoderSwitch", 1.0, 0, true, &[("op", "preset"), ("preset", "acid")]),
    );
    assert_eq!(g.e.vocoder.preset.as_deref(), Some("acid"));
    SurfaceHost::perform(&mut g.e, target("vocoderSwitch", 1.0, 0, true, &[("op", "preset_next")]));
    assert_eq!(g.e.vocoder.preset.as_deref(), Some("pad"));
}

/// Presets: a factory one sets every synth knob; the user's are saved to the file, read back
/// by another engine, and cannot take a factory name; a knob moved after a load is no longer
/// that preset.
#[test]
fn synth_presets_save_and_recall() {
    let dir = std::env::temp_dir().join(format!("djrust-synth-presets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("synth-presets.json");
    let mut g = Rig::new();
    assert_eq!(g.e.set_synth_preset_file(Some(file.clone())), 0);
    g.e.trace = Some(Vec::new());
    g.e.synth_preset_load("Acid").unwrap();
    let s = g.e.vocoder.settings.clone();
    assert_eq!(s.synth.cutoff, 380.0);
    assert_eq!(s.synth.resonance, 0.8);
    assert_eq!(s.synth.osc[1].level, 0.0);
    assert_eq!(s.synth_detune, 12.0, "left out: the default");
    assert_eq!(g.e.vocoder.preset.as_deref(), Some("acid"));
    // a tweak, saved as the user's own
    g.e.vocoder_set(VocoderParam::Cutoff, 520.0);
    assert_eq!(g.e.vocoder.preset, None);
    g.e.vocoder_set(VocoderParam::Osc3Wave, 1.0);
    g.e.vocoder_set(VocoderParam::Osc3Level, 0.4);
    g.e.synth_preset_save("my acid").unwrap();
    assert!(g.e.synth_preset_save("pad").is_err(), "a factory name");
    assert!(file.exists());
    let lines: Vec<String> = g.e.trace.take().unwrap().into_iter().map(|t| t.entry.text).collect();
    assert_eq!(lines.first().unwrap(), "vocoder synth preset acid");
    assert_eq!(lines.last().unwrap(), "vocoder synth preset saved: my acid");
    // another engine reads it back and recalls it
    let mut h = Rig::new();
    assert_eq!(h.e.set_synth_preset_file(Some(file.clone())), 1);
    assert!(h.e.synth_preset_names().contains(&("my acid".to_string(), true)));
    h.e.synth_preset_load("my acid").unwrap();
    let t = &h.e.vocoder.settings;
    assert_eq!(t.synth.cutoff, 520.0);
    assert_eq!(t.synth.osc[2].wave, djrust::fx::eurosynth::Wave::Square);
    assert_eq!(t.synth.osc[2].level, 0.4);
    assert_eq!(t.synth.resonance, 0.8);
    assert!(h.e.synth_preset_load("nope").is_err());
    h.e.synth_preset_delete("my acid").unwrap();
    assert_eq!(Rig::new().e.set_synth_preset_file(Some(file)), 0);
    // the state shows them
    let j = h.e.vocoder_json();
    let sy = j.get("synth").unwrap();
    assert_eq!(sy.get("osc3_wave").and_then(|w| w.as_str()), Some("square"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A one-bar drum loop at 120: a decaying noise burst on each beat (a kick-ish thump and a
/// hat-ish hiss), so its band envelopes pulse.
fn drum_loop() -> (djrust::fx::vocoder::LoopSource, std::sync::Arc<djrust::deck::turntable::SampleBuffer>) {
    let beat = (SR * 0.5) as usize;
    let mut x = vec![0f32; beat * 4];
    let mut seed = 0x9e37_79b9u32;
    for (i, v) in x.iter_mut().enumerate() {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
        let t = (i % beat) as f32 / SR as f32;
        let thump = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * (-t * 30.0).exp();
        *v = 0.6 * thump + 0.3 * noise * (-t * 60.0).exp();
    }
    let src = djrust::fx::vocoder::LoopSource {
        kind: "file".into(),
        id: "drums.wav".into(),
        title: "drums".into(),
        bpm: 120.0,
        beats: 4.0,
    };
    let buf = djrust::deck::turntable::SampleBuffer::new(x, None, SR);
    (src, std::sync::Arc::new(buf))
}

/// The envelope of `out` in 10 ms windows: how much it pulses (max over min, in the second
/// half), and its level.
fn pulse(out: &[f32]) -> f32 {
    let x = &out[out.len() / 2..];
    let w = (SR * 0.01) as usize;
    let rms: Vec<f32> = x
        .chunks(w)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let hi = rms.iter().cloned().fold(0f32, f32::max);
    let lo = rms.iter().cloned().fold(f32::MAX, f32::min);
    hi / lo.max(1e-6)
}

/// "Talking drums": a drum loop as the modulator and the synth as the carrier — the chord
/// pulses with the drums. Swapped, the synth's chord shapes the drum loop instead, and swapping
/// again puts everything back, the loop's audio kept (no new pick of it).
#[test]
fn talking_drums_and_the_swap() {
    let mut g = Rig::new();
    g.talking = false;
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    let (src, buf) = drum_loop();
    g.e.vocoder_modulator(ModulatorPick::Loaded(src, buf)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_set_notes(&[45, 52, 57, 60]);
    g.e.vocoder_set(VocoderParam::Noise, 0.0);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.run(800);
    let talking = g.level();
    let p = pulse(&g.out);
    assert!(talking > 0.005, "vocoded {talking}");
    assert!(p > 4.0, "the chord pulses with the drums: {p}");
    g.e.trace = Some(Vec::new());
    g.e.vocoder_swap().unwrap();
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Synth);
    match &g.e.vocoder.carrier {
        CarrierSource::Loop(l) => assert_eq!((l.kind.as_str(), l.id.as_str()), ("file", "drums.wav")),
        o => panic!("{o:?}"),
    }
    let t = g.e.trace.take().unwrap();
    assert_eq!(t.len(), 1, "one line for a swap");
    assert_eq!(t[0].entry.text, "vocoder swap: synth → file drums");
    g.run(800);
    assert!(g.level() > 0.005, "the synth shapes the drums: {}", g.level());
    let j = g.e.vocoder_json();
    assert_eq!(
        j.get("modulator").and_then(|m| m.get("kind")).and_then(|k| k.as_str()),
        Some("synth")
    );
    assert!(j.get("carrier_loop_position").is_some());
    // and back
    g.e.vocoder_swap().unwrap();
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Synth);
    assert!(matches!(g.e.vocoder.modulator, ModulatorSource::Loop(_)));
    g.run(800);
    assert!((g.level() - talking).abs() < talking * 0.3, "{} vs {talking}", g.level());
}

/// One synth: it is the carrier or the modulator, never both, as a deck.
#[test]
fn the_synth_is_on_one_side_only() {
    let mut g = Rig::new();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    assert!(g.e.vocoder_modulator(ModulatorPick::Synth).is_err());
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_modulator(ModulatorPick::Synth).unwrap();
    assert!(g.e.vocoder_carrier(CarrierPick::Synth).is_err());
    for _ in 0..12 {
        g.e.vocoder_cycle_carrier(1).unwrap();
        assert_ne!(g.e.vocoder.carrier, CarrierSource::Synth);
    }
    // input ↔ synth swaps fine
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_swap().unwrap();
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Synth);
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Input("TD-3".into()));
}

/// The live op plays a swap and the new source kinds (synth modulator, loop carrier).
#[test]
fn the_live_op_swaps_and_takes_every_source_on_either_side() {
    use djrust::json::JsonValue;
    let mut g = Rig::new();
    let op = |s: &str| JsonValue::parse(s).unwrap();
    g.e.apply_live_op("vocoder", &op(r#"{"carrier":"input:TD-3","modulator":"synth"}"#))
        .unwrap();
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Synth);
    g.e.apply_live_op("vocoder", &op(r#"{"swap":true}"#)).unwrap();
    assert_eq!(g.e.vocoder.carrier, CarrierSource::Synth);
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Input("TD-3".into()));
    g.run(2 * 24_000 / BLOCK + 4);
    g.e.apply_live_op("vocoder", &op(r#"{"carrier":"capture:MIC:1"}"#))
        .unwrap();
    assert!(matches!(&g.e.vocoder.carrier, CarrierSource::Loop(l) if l.kind == "capture"));
    assert!(g.e.apply_live_op("vocoder", &op(r#"{"carrier":"bogus:1"}"#)).is_err());
}

/// Freeze holds the bands: the mic stops and the TD-3 keeps its shape; the window's meter and
/// `state` say frozen; unfrozen, silence closes it again.
#[test]
fn freeze_holds_the_bands_with_the_mic_gone() {
    let mut g = Rig::new();
    g.e.set_input_volume(TD3, 0.0);
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_set(VocoderParam::Noise, 0.0);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.run(200);
    let talking = g.level();
    assert!(talking > 0.01);
    g.e.trace = Some(Vec::new());
    g.e.vocoder_switch(VocoderSwitch::Freeze, true);
    let t = g.e.trace.take().unwrap();
    assert_eq!(t[0].entry.text, "vocoder freeze: the bands held");
    g.run(10);
    let held = g.e.vocoder.mirror.envelopes(16);
    g.talking = false;
    g.run(200);
    assert_eq!(g.e.vocoder.mirror.envelopes(16), held, "the envelopes hold");
    assert!((g.level() - talking).abs() < talking * 0.2, "{} vs {talking}", g.level());
    // the modulator unpicked: still held
    g.e.vocoder_modulator(ModulatorPick::None).unwrap();
    g.run(100);
    assert!(g.level() > talking * 0.8, "frozen with no modulator: {}", g.level());
    let j = g.e.vocoder_json();
    assert_eq!(j.get("frozen").and_then(|v| v.as_bool()), Some(true));
    // unfrozen with the silent mic back: it closes
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_switch(VocoderSwitch::Freeze, false);
    g.run(200);
    assert!(g.level() < talking * 0.1, "{}", g.level());
}

/// The crossfader as the morph: left the carrier alone, centre the vocoded signal, right the
/// modulator alone; the decks leave the crossfader while it is assigned.
#[test]
fn the_crossfader_morphs_carrier_vocoder_modulator() {
    let mut g = Rig::new();
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_set(VocoderParam::Noise, 0.0);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.run(100);
    let vocoded = g.out.clone();
    g.e.vocoder_switch(VocoderSwitch::Xfade, true);
    // centre: the vocoded signal, as with mix 1 (the crossfader starts at the centre)
    g.e.set_crossfader(0.5);
    g.run(100);
    let centre = g.level();
    // what each source is alone, dry on its own strip
    let dry = |strip: usize| {
        let mut d = Rig::new();
        d.e.set_input_volume(if strip == MIC { TD3 } else { MIC }, 0.0);
        d.run(100);
        d.level()
    };
    let (td3, mic) = (dry(TD3), dry(MIC));
    // left: the TD-3's saw itself
    g.e.set_crossfader(0.0);
    g.run(100);
    let left = g.level();
    assert!((left - td3).abs() < td3 * 0.1, "the carrier alone: {left} vs {td3}");
    // right: the mic itself
    g.e.set_crossfader(1.0);
    g.run(100);
    let right = g.level();
    assert!((right - mic).abs() < mic * 0.1, "the modulator alone: {right} vs {mic}");
    assert!(centre > 0.01 && (centre - left).abs() > 0.02, "centre {centre} vs left {left}");
    let _ = vocoded;
    // a deck: the crossfader at 1 would silence A, but assigned to the vocoder it does not
    let mut d = Rig::new();
    d.talking = false;
    d.e.set_input_volume(TD3, 0.0);
    d.e.set_input_volume(MIC, 0.0);
    assert!(d.e.generate("A", &djrust::TrackSpec::new("techno", 42.0, None)));
    d.e.play("A");
    d.e.set_crossfader(1.0);
    d.run(300);
    let gone = d.level();
    d.e.vocoder_switch(VocoderSwitch::On, true);
    d.e.vocoder_switch(VocoderSwitch::Xfade, true);
    d.run(300);
    assert!(d.level() > gone * 4.0 + 0.005, "deck A back: {} vs {gone}", d.level());
}

/// The meter: every band has its centre, the carrier's centre after the shift, its envelope,
/// a held peak at least the envelope, and dB.
#[test]
fn the_band_meter_reads_out_in_state() {
    let mut g = Rig::new();
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_set(VocoderParam::Bands, 8.0);
    g.e.vocoder_set(VocoderParam::Shift, 12.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.run(100);
    let j = g.e.vocoder_json();
    let bands = match j.get("bands") {
        Some(djrust::json::JsonValue::Array(b)) => b.clone(),
        o => panic!("{o:?}"),
    };
    assert_eq!(bands.len(), 8);
    let n = |b: &djrust::json::JsonValue, k: &str| b.get(k).and_then(|v| v.as_f64()).unwrap();
    for b in &bands {
        assert!((n(b, "carrier_hz") - 2.0 * n(b, "hz")).abs() <= 1.0);
        assert!(n(b, "peak") >= n(b, "env"));
        assert!(n(b, "db") >= -60.0 && n(b, "db") <= 0.0);
    }
    assert!(bands.iter().any(|b| n(b, "db") > -40.0), "the mic is in some band");
    // the mic stops: the envelopes fall faster than the held peaks
    g.talking = false;
    g.run(40);
    let j = g.e.vocoder_json();
    if let Some(djrust::json::JsonValue::Array(b)) = j.get("bands") {
        assert!(b.iter().any(|b| n(b, "peak") > 2.0 * n(b, "env")));
    }
}

// MARK: instances (docs/VOCODER.md, "Instances")

/// Two vocoders at once, each with its own pair: V is the mic shaping the TD-3, V2 the TD-3
/// shaping its own synth. Each keeps its own sources, knobs, notes and meter; the master has
/// both, and removing V2 leaves V as it was.
#[test]
fn two_instances_run_different_pairs_at_once() {
    use djrust::host::deck::vocoder::MAX_VOCODERS;
    // V alone, for reference
    let mut solo = Rig::new();
    solo.e.set_input_volume(MIC, 0.0);
    solo.e.set_input_volume(TD3, 0.0);
    solo.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    solo.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    solo.e.vocoder_switch(VocoderSwitch::On, true);
    solo.e.vocoder_set(VocoderParam::Volume, 1.0);
    solo.run(200);

    let mut g = Rig::new();
    g.e.set_input_volume(MIC, 0.0);
    g.e.set_input_volume(TD3, 0.0);
    assert_eq!(g.e.vocoder.active(), vec![0]);
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    // V2: added and selected, so the same calls now act on it
    assert_eq!(g.e.vocoder_add().unwrap(), 1);
    assert_eq!(g.e.vocoder_selected(), 1);
    g.e.vocoder_modulator(ModulatorPick::Input(TD3)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    g.e.vocoder_set_notes(&[57, 64]);
    g.e.vocoder_set(VocoderParam::Bands, 8.0);
    g.e.vocoder_set(VocoderParam::Volume, 1.0);
    g.e.vocoder_switch(VocoderSwitch::On, true);
    g.run(200);
    let both = g.level();

    let v0 = &g.e.vocoder.instances[0];
    let v1 = &g.e.vocoder.instances[1];
    assert_eq!(v0.modulator, ModulatorSource::Input("MIC".into()));
    assert_eq!(v0.carrier, CarrierSource::Input("TD-3".into()));
    assert!(v0.notes.is_empty() && v0.settings.bands == 16);
    assert_eq!(v1.modulator, ModulatorSource::Input("TD-3".into()));
    assert_eq!(v1.carrier, CarrierSource::Synth);
    assert_eq!(v1.settings.bands, 8);
    let (m0, c0, o0) = v0.mirror.levels();
    let (m1, c1, o1) = v1.mirror.levels();
    assert!(m0 > 0.1 && c0 > 0.1 && o0 > 0.01, "V {m0} {c0} {o0}");
    assert!(m1 > 0.1 && c1 > 0.01 && o1 > 0.01, "V2 {m1} {c1} {o1}");
    // each its own modulator: the mic's tone, and the TD-3's saw
    assert!((m0 - m1).abs() > 0.01, "{m0} vs {m1}");
    assert!(v1.mirror.envelopes(8).iter().any(|e| *e > 0.01));
    // V still makes what it made alone; the master has V2 on top
    let v_alone = solo.e.vocoder.instances[0].mirror.levels().2;
    assert!((o0 - v_alone).abs() < 1e-3, "V {o0} alone {v_alone}");
    assert!(both > solo.level() * 1.05, "both {both} V alone {}", solo.level());
    // the socket's view
    let js = g.e.vocoders_json();
    assert_eq!(js.get("selected").and_then(|x| x.as_f64()), Some(1.0));
    assert_eq!(js.get("max").and_then(|x| x.as_f64()), Some(MAX_VOCODERS as f64));
    let insts = js.get("instances").and_then(|x| x.as_array()).unwrap();
    assert_eq!(insts.len(), 2);
    assert_eq!(insts[1].get("label").and_then(|x| x.as_str()), Some("V2"));

    // V2 away: off, sources and notes gone, the selection back on V
    g.e.vocoder_remove(1).unwrap();
    assert_eq!(g.e.vocoder.active(), vec![0]);
    assert_eq!(g.e.vocoder_selected(), 0);
    assert!(g.e.vocoder.instances[1].notes.is_empty());
    assert_eq!(g.e.vocoder.instances[1].carrier, CarrierSource::None);
    assert!(g.e.vocoder_remove(0).is_err(), "V stays");
    g.run(200);
    assert!((g.level() - solo.level()).abs() < solo.level() * 0.05);
    // the cap
    for _ in 1..MAX_VOCODERS {
        g.e.vocoder_add().unwrap();
    }
    assert!(g.e.vocoder_add().is_err());
    assert!(g.e.with_vocoder(MAX_VOCODERS, |_| ()).is_err());
}

/// Every instance on at once, each with a different pair, a synth on each: still no allocation
/// on the render thread; the instances not in use cost a branch.
#[test]
fn every_instance_at_once_does_not_allocate() {
    use djrust::host::deck::vocoder::MAX_VOCODERS;
    let mut g = Rig::new();
    g.run(2 * 24_000 / BLOCK + 4);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut cl, mut cr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    // allocations over 50 blocks, and whether the cue had sound
    let mut count = |g: &mut Rig| {
        g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
        ALLOCS.with(|c| c.set(0));
        ON.with(|o| o.set(true));
        for b in 0..50 {
            // the mic and the TD-3 keep sounding (a ring push allocates nothing)
            for k in 0..BLOCK {
                let t = ((b * BLOCK + k) as f32 / SR as f32) * 300.0;
                let _ = g.mic.push(0.4 * (2.0 * std::f32::consts::PI * t).sin());
                let _ = g.td3.push(0.5 * (2.0 * ((t / 3.0) % 1.0) - 1.0));
            }
            g.r.render_with_cue(&mut l, &mut r, Some((&mut cl, &mut cr)));
        }
        ON.with(|o| o.set(false));
        let cued = cl.iter().any(|x| x.abs() > 1e-4);
        (ALLOCS.with(|c| c.get()), cued)
    };
    // only V in use, off: the baseline
    assert_eq!(count(&mut g).0, 0, "one, off");
    for k in 0..MAX_VOCODERS {
        g.e.with_vocoder(k, |e| {
            match k % 4 {
                0 => {
                    e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
                    e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
                }
                1 => {
                    e.vocoder_modulator(ModulatorPick::Input(TD3)).unwrap();
                    e.vocoder_carrier(CarrierPick::Synth).unwrap();
                }
                2 => {
                    e.vocoder_modulator(ModulatorPick::Capture(MIC, Some(1.0))).unwrap();
                    e.vocoder_carrier(CarrierPick::Synth).unwrap();
                }
                _ => {
                    e.vocoder_modulator(ModulatorPick::Synth).unwrap();
                    e.vocoder_carrier(CarrierPick::Input(MIC)).unwrap();
                    e.vocoder_switch(VocoderSwitch::Insert, true);
                    e.vocoder_switch(VocoderSwitch::Xfade, true);
                }
            }
            e.vocoder_set_notes(&[45, 52, 57]);
            e.vocoder_set(VocoderParam::Bands, 32.0);
            e.vocoder_set(VocoderParam::VowelMix, 0.5);
            e.vocoder_switch(VocoderSwitch::Pfl, true);
            e.vocoder_switch(VocoderSwitch::On, true);
        })
        .unwrap();
    }
    assert_eq!(g.e.vocoder.active().len(), MAX_VOCODERS);
    g.e.set_crossfader(0.3);
    let (n, cued) = count(&mut g);
    assert_eq!(n, 0, "every instance on");
    assert!(cued, "PFL puts them in the cue");
    for k in 0..MAX_VOCODERS {
        let (_, _, o) = g.e.vocoder.instances[k].mirror.levels();
        assert!(o > 1e-4, "instance {k} makes sound: {o}");
    }
    // V3 frozen (its bands hold what they had), two removed mid-flight: still none
    g.e.with_vocoder(2, |e| e.vocoder_switch(VocoderSwitch::Freeze, true)).unwrap();
    g.e.vocoder_remove(1).unwrap();
    g.e.vocoder_remove(3).unwrap();
    assert_eq!(count(&mut g).0, 0, "two removed");
}

/// Back-compat: with only V, the live op, the setup ops and the trace are exactly as before (no
/// `instance` anywhere). Another instance carries its `instance`, its name in the words, and
/// its setup op plays it back on a fresh engine.
#[test]
fn instance_zero_is_unchanged_and_others_round_trip() {
    use djrust::json::JsonValue;
    use djrust::session::arrangement::ArrangeOp;
    let op = |s: &str| JsonValue::parse(s).unwrap();
    let mut g = Rig::new();
    g.e.trace = Some(Vec::new());
    // an old op: instance 0
    g.e.apply_live_op("vocoder", &op(r#"{"carrier":"synth","notes":[57],"on":true}"#))
        .unwrap();
    assert_eq!(g.e.vocoder.instances[0].carrier, CarrierSource::Synth);
    assert_eq!(g.e.vocoder.active(), vec![0]);
    let setup = g.e.vocoder_setup_ops();
    assert_eq!(setup.len(), 1);
    let ArrangeOp::Live { params, .. } = &setup[0] else { panic!() };
    assert!(params.get("instance").is_none(), "{}", params.js_stringified());
    let traced = g.e.trace.take().unwrap();
    for t in &traced {
        assert!(t.entry.key.starts_with("vocoder:"), "{}", t.entry.key);
        assert!(!t.entry.text.contains("V2"));
        for o in &t.ops {
            let ArrangeOp::Live { params, .. } = o else { continue };
            assert!(params.get("instance").is_none());
        }
    }

    // a second instance, by op: naming it adds it
    g.e.trace = Some(Vec::new());
    g.e.apply_live_op(
        "vocoder",
        &op(r#"{"instance":1,"carrier":"input:TD-3","modulator":"input:MIC","shift":3,"on":true}"#),
    )
    .unwrap();
    assert_eq!(g.e.vocoder.active(), vec![0, 1]);
    assert_eq!(g.e.vocoder_selected(), 0, "an op does not move the selection");
    assert_eq!(g.e.vocoder.instances[1].carrier, CarrierSource::Input("TD-3".into()));
    assert_eq!(g.e.vocoder.instances[0].carrier, CarrierSource::Synth, "V untouched");
    let traced = g.e.trace.take().unwrap();
    assert!(!traced.is_empty());
    for t in &traced {
        assert!(t.entry.key.starts_with("vocoder:V2:"), "{}", t.entry.key);
        assert!(t.entry.text.starts_with("vocoder V2"), "{}", t.entry.text);
        for o in &t.ops {
            let ArrangeOp::Live { params, .. } = o else { continue };
            assert_eq!(params.get("instance").and_then(|x| x.as_f64()), Some(1.0));
        }
    }
    // a fresh engine from the setup ops has both, as they were
    let setup = g.e.vocoder_setup_ops();
    assert_eq!(setup.len(), 2);
    let mut f = Rig::new();
    for o in &setup {
        let ArrangeOp::Live { method, params } = o else { panic!() };
        f.e.apply_live_op(method, params).unwrap();
    }
    assert_eq!(f.e.vocoder.active(), vec![0, 1]);
    for k in 0..2 {
        let (a, b) = (&g.e.vocoder.instances[k], &f.e.vocoder.instances[k]);
        assert_eq!(a.carrier, b.carrier, "instance {k}");
        assert_eq!(a.modulator, b.modulator, "instance {k}");
        assert_eq!(a.settings, b.settings, "instance {k}");
        assert_eq!(a.notes, b.notes, "instance {k}");
    }
    // and the remove op
    f.e.apply_live_op("vocoder", &op(r#"{"instance":1,"remove":true}"#)).unwrap();
    assert_eq!(f.e.vocoder.active(), vec![0]);
}

/// Bindings move the selected instance, or the one their `instance` arg names; the select / add
/// / remove ops manage them.
#[test]
fn bindings_target_the_selected_instance_or_their_own() {
    let mut g = Rig::new();
    let press = |e: &mut djrust::host::deck::Engine, args: &[(&str, &str)]| {
        SurfaceHost::perform(e, target("vocoderSwitch", 1.0, 0, true, args));
    };
    press(&mut g.e, &[("op", "add")]);
    assert_eq!((g.e.vocoder.active(), g.e.vocoder_selected()), (vec![0, 1], 1));
    // the selected one: V2
    SurfaceHost::perform(&mut g.e, target("vocoder", 1.0, 0, false, &[("param", "shift")]));
    SurfaceHost::perform(&mut g.e, target("vocoderKeys", 1.0, 60, true, &[]));
    assert!((g.e.vocoder.instances[1].settings.shift - 12.0).abs() < 1e-9);
    assert_eq!(g.e.vocoder.instances[0].settings.shift, 0.0);
    assert!(g.e.vocoder.instances[1].notes.contains(&60));
    // an `instance` arg: V, the selection stays
    SurfaceHost::perform(
        &mut g.e,
        target("vocoder", 0.0, 0, false, &[("param", "shift"), ("instance", "0")]),
    );
    press(&mut g.e, &[("op", "freeze"), ("instance", "0")]);
    assert!((g.e.vocoder.instances[0].settings.shift + 12.0).abs() < 1e-9);
    assert!(g.e.vocoder.instances[0].settings.freeze);
    assert!(!g.e.vocoder.instances[1].settings.freeze);
    assert_eq!(g.e.vocoder_selected(), 1);
    // select_next wraps over the ones in use; select picks one
    press(&mut g.e, &[("op", "select_next")]);
    assert_eq!(g.e.vocoder_selected(), 0);
    press(&mut g.e, &[("op", "select"), ("instance", "1")]);
    assert_eq!(g.e.vocoder_selected(), 1);
    press(&mut g.e, &[("op", "remove")]);
    assert_eq!((g.e.vocoder.active(), g.e.vocoder_selected()), (vec![0], 0));
}

/// `uses_synth` (what the window shows the synth's controls on): either side the internal
/// synth, per instance, and a swap keeps it.
#[test]
fn uses_synth_follows_either_side_per_instance_and_survives_a_swap() {
    let mut g = Rig::new();
    assert!(!g.e.vocoder.uses_synth(), "none / none");
    g.e.vocoder_modulator(ModulatorPick::Input(MIC)).unwrap();
    g.e.vocoder_carrier(CarrierPick::Input(TD3)).unwrap();
    assert!(!g.e.vocoder.uses_synth(), "input / input");
    g.e.vocoder_carrier(CarrierPick::Synth).unwrap();
    assert!(g.e.vocoder.uses_synth(), "synth carrier");
    g.e.vocoder_swap().unwrap();
    assert_eq!(g.e.vocoder.modulator, ModulatorSource::Synth);
    assert!(g.e.vocoder.uses_synth(), "synth modulator, after the swap");
    g.e.vocoder_swap().unwrap();
    assert!(g.e.vocoder.uses_synth(), "and back");
    // V2: its own sources, V's synth does not count for it
    assert_eq!(g.e.vocoder_add().unwrap(), 1);
    assert!(!g.e.vocoder.uses_synth(), "V2 none / none");
    assert!(g.e.vocoder.instances[0].uses_synth());
    g.e.vocoder_modulator(ModulatorPick::Input(TD3)).unwrap();
    g.e.vocoder_swap().unwrap();
    assert!(!g.e.vocoder.uses_synth(), "V2 input swapped");
    // V lets the synth go: neither uses it
    g.e.vocoder_select(0).unwrap();
    g.e.vocoder_carrier(CarrierPick::None).unwrap();
    assert!(!g.e.vocoder.uses_synth());
    g.e.vocoder_modulator(ModulatorPick::Synth).unwrap();
    assert!(g.e.vocoder.uses_synth(), "synth modulator alone");
}
