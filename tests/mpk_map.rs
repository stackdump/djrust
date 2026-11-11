//! The binding layer on the MPK Mini Mk II: a user map laid over the shipped profile, routed
//! from raw bytes with no hardware. NOTE-mode pads: bank A 36-43, bank B 44-51, channel 10.

use djrust::control::controller::Action;
use djrust::control::map::{learn, template, MapSet};
use djrust::control::profiles;
use djrust::control::registry;
use djrust::control::surface::DJActionKind;
use djrust::host::controllers::ControllerSession;

const PORT: &str = "MPK Mini Mk II";

fn session() -> (ControllerSession, MapSet) {
    let mut s = ControllerSession::new();
    assert!(s.attach(PORT));
    let set = MapSet::parse(profiles::MPK_PERFORMANCE_MAP_JSON).unwrap();
    assert_eq!(set.device, "MPK Mini Mk II");
    assert!(s.router.apply_map(&set.device, Some(set.current())));
    (s, set)
}

fn send(s: &mut ControllerSession, bytes: &[u8]) -> Vec<Action> {
    s.route(PORT, bytes, 0.0).unwrap().actions
}

fn targets(a: &[Action]) -> Vec<&Action> {
    a.iter().filter(|a| matches!(a, Action::Target { .. })).collect()
}

#[test]
fn keys_play_the_aimed_deck_live_and_transpose_when_the_mode_says() {
    use djrust::control::controller::KeyMode;
    let (mut s, _) = session();
    // live (the default): a key on the aimed deck's lead voice
    let a = send(&mut s, &[0x90, 64, 100]);
    assert!(targets(&a).is_empty(), "{a:?}");
    assert!(
        a.iter().any(|a| matches!(a, Action::KeyDown { deck, index: 64, quant: None, .. } if deck == "A")),
        "{a:?}"
    );
    let a = send(&mut s, &[0x80, 64, 0]);
    assert!(a.iter().any(|a| matches!(a, Action::KeyUp { index: 64, .. })), "{a:?}");
    // aimed at deck B by the key route
    s.host.key_to = Some("B".into());
    let a = send(&mut s, &[0x90, 60, 100]);
    assert!(a.iter().any(|a| matches!(a, Action::KeyDown { deck, .. } if deck == "B")), "{a:?}");
    s.host.key_to = None;

    // transpose mode: the keys become the transpose target on the loops, as before
    s.host.keys = KeyMode::Transpose;
    let a = send(&mut s, &[0x90, 64, 100]);
    let t = targets(&a);
    assert_eq!(t.len(), 1, "{a:?}");
    match t[0] {
        Action::Target { id, deck, index, pressed, args, .. } => {
            assert_eq!(id, "transpose");
            assert_eq!(deck.as_deref(), Some("L"));
            assert_eq!(*index, 64);
            assert!(*pressed);
            assert_eq!(args.get("quant").map(String::as_str), Some("bar"));
        }
        _ => unreachable!(),
    }
    // release arrives too
    let a = send(&mut s, &[0x80, 64, 0]);
    assert!(matches!(targets(&a)[0], Action::Target { pressed: false, .. }));

    // TD-3 mode: the app plays the TD-3 itself; the map does nothing with a key
    s.host.keys = KeyMode::Td3;
    let a = send(&mut s, &[0x90, 64, 100]);
    assert!(!a.iter().any(|a| matches!(a, Action::KeyDown { .. } | Action::Target { .. })), "{a:?}");
}

#[test]
fn a_keys_binding_can_carry_its_own_deck_and_quantise() {
    let (mut s, mut set) = session();
    let mut b = template(DJActionKind::Keys);
    b.group = "keys".into();
    b.index = None;
    b.deck = Some("B".into());
    b.args.insert("quant".into(), "bar".into());
    set.current_mut().set(b);
    s.router.apply_map(&set.device, Some(set.current()));
    let a = send(&mut s, &[0x90, 62, 80]);
    assert!(
        a.iter().any(|a| matches!(a, Action::KeyDown { deck, index: 62, quant: Some(q), .. } if deck == "B" && q == "bar")),
        "{a:?}"
    );
}

#[test]
fn the_key_mode_and_key_deck_are_bindable_targets() {
    for k in [DJActionKind::KeyMode, DJActionKind::KeyDeck, DJActionKind::VoxPage] {
        assert_eq!(DJActionKind::from_raw(k.raw()), Some(k));
        assert_eq!(registry::info(k).family, if k == DJActionKind::VoxPage { "Loops" } else { "Performance" });
    }
    let (mut s, mut set) = session();
    let mut b = template(DJActionKind::KeyMode);
    b.group = "padNotes".into();
    b.index = Some(0);
    b.args.insert("mode".into(), "toggle".into());
    set.current_mut().set(b);
    s.router.apply_map(&set.device, Some(set.current()));
    let a = send(&mut s, &[0x99, 36, 100]);
    match targets(&a)[0] {
        Action::Target { id, args, .. } => {
            assert_eq!(id, "keyMode");
            assert_eq!(args["mode"], "toggle");
        }
        _ => unreachable!(),
    }
}

#[test]
fn bank_b_pads_mute_the_aimed_decks_parts() {
    let (mut s, _) = session();
    // bank B pad 1 = note 44 on channel 10
    let a = send(&mut s, &[0x99, 44, 90]);
    assert!(
        a.iter().any(|a| matches!(a, Action::ToggleRole { role, .. } if role == "kick")),
        "{a:?}"
    );
    let a = send(&mut s, &[0x99, 51, 90]);
    assert!(a.iter().any(|a| matches!(a, Action::ToggleRole { role, .. } if role == "arp")));
}

#[test]
fn presets_switch_and_the_shipped_bindings_come_back() {
    let (mut s, mut set) = session();
    set.select("decks");
    s.router.apply_map(&set.device, Some(set.current()));
    // with the empty preset the keys are live keys again
    let a = send(&mut s, &[0x90, 64, 100]);
    assert!(targets(&a).is_empty());
    assert!(a.iter().any(|a| matches!(a, Action::KeyDown { .. })), "{a:?}");
    // and the shipped profile is intact underneath
    s.router.apply_map(&set.device, None);
    let a = send(&mut s, &[0x90, 64, 100]);
    assert!(a.iter().any(|a| matches!(a, Action::KeyDown { .. })));
    set.apply_request("next");
    assert_eq!(set.current().name, "performance");
}

#[test]
fn learn_binds_any_pad_to_any_target_and_survives_json() {
    let (mut s, mut set) = session();
    // arm "one-shot", then press bank A pad 3 (note 38)
    let mut t = template(DJActionKind::OneShot);
    t.args.insert("sample".into(), "vox/hey.wav".into());
    t.args.insert("quant".into(), "beat".into());
    let ev = s.router.surface_mut(PORT).unwrap().decode(&[0x99, 38, 100]).unwrap();
    let b = learn(&ev, &t, None);
    assert_eq!((b.group.as_str(), b.index), ("padNotes", Some(2)));
    set.current_mut().set(b);
    s.router.apply_map(&set.device, Some(set.current()));
    let a = send(&mut s, &[0x99, 38, 100]);
    match targets(&a)[0] {
        Action::Target { id, args, .. } => {
            assert_eq!(id, "oneShot");
            assert_eq!(args["sample"], "vox/hey.wav");
        }
        _ => unreachable!(),
    }
    // a knob learns a continuous target
    let ev = s.router.surface_mut(PORT).unwrap().decode(&[0xB0, 5, 64]).unwrap();
    let b = learn(&ev, &template(DJActionKind::ReverbWet), None);
    assert_eq!(b.deck.as_deref(), Some("*"));
    set.current_mut().set(b);
    // the whole set round-trips
    let again = MapSet::parse(&set.to_json().js_stringified()).unwrap();
    assert_eq!(again, set);
}

#[test]
fn every_target_is_registered_and_grouped() {
    for k in DJActionKind::ALL {
        let i = registry::info(*k);
        assert!(!i.label.is_empty() && !i.family.is_empty(), "{k:?}");
    }
    assert!(registry::families().contains(&"Loops"));
    assert!(registry::families().contains(&"Bindings"));
    assert!(registry::missing(DJActionKind::OneShot, &template(DJActionKind::OneShot)).is_some());
}

#[test]
fn maps_persist_per_device() {
    use djrust::host::stores::maps::MapStore;
    let dir = djrust::host::stores::temp_path("maps");
    let store = MapStore::new(&dir);
    let mut set = store.load_or_default("MPK Mini Mk II");
    assert_eq!(set.current().name, "performance", "ships with the MPK preset");
    set.add("jam", true);
    store.save(&set).unwrap();
    let back = store.load("MPK Mini Mk II").unwrap();
    assert_eq!(back, set);
    assert_eq!(back.current().name, "jam");
    assert!(store.load("APC mini mk2").is_none());
    let _ = std::fs::remove_dir_all(dir);
}

/// The `vocoder` preset: the keys play the vocoder's synth (a chord is every key held), the
/// knobs turn its shift, bands, attack, release, mix and cutoff (K4 stays the crossfader, for
/// the morph), bank A pads are freeze / swap / xfade / bypass / the sources / the next preset /
/// on. Through an engine: three keys down hold three notes.
#[test]
fn the_vocoder_preset_plays_chords_on_the_synth() {
    let (mut s, mut set) = session();
    set.select("vocoder");
    assert_eq!(set.current().name, "vocoder");
    s.router.apply_map(&set.device, Some(set.current()));
    let (mut e, _r) = djrust::host::deck::engine(djrust::host::deck::EngineConfig::new(48_000.0));
    e.vocoder_carrier(djrust::host::deck::vocoder::CarrierPick::Synth).unwrap();
    for n in [57u8, 60, 64] {
        let a = send(&mut s, &[0x90, n, 100]);
        match targets(&a)[..] {
            [Action::Target { id, index, pressed: true, .. }] => {
                assert_eq!((id.as_str(), *index), ("vocoderKeys", n as usize));
            }
            _ => panic!("{a:?}"),
        }
        for x in a {
            e.apply_action(x);
        }
    }
    assert_eq!(e.vocoder.notes.iter().copied().collect::<Vec<_>>(), vec![57, 60, 64]);
    let a = send(&mut s, &[0x80, 60, 0]);
    for x in a {
        e.apply_action(x);
    }
    assert_eq!(e.vocoder.notes.iter().copied().collect::<Vec<_>>(), vec![57, 64]);
    // knob 2 (CC 2... index 1) turns the shift; pad 1 freezes
    let a = send(&mut s, &[0x99, 36, 100]);
    for x in a {
        e.apply_action(x);
    }
    assert!(e.vocoder.settings.freeze, "pad 1 freezes the bands");
    // bank B pad 3 adds a second vocoder and selects it: the keys now play its synth
    for x in send(&mut s, &[0x99, 46, 100]) {
        e.apply_action(x);
    }
    assert_eq!((e.vocoder.active(), e.vocoder_selected()), (vec![0, 1], 1));
    for x in send(&mut s, &[0x90, 50, 100]) {
        e.apply_action(x);
    }
    assert_eq!(e.vocoder.instances[1].notes.iter().copied().collect::<Vec<_>>(), vec![50]);
    assert_eq!(e.vocoder.instances[0].notes.iter().copied().collect::<Vec<_>>(), vec![57, 64]);
    // bank B pad 1: back to V
    for x in send(&mut s, &[0x99, 44, 100]) {
        e.apply_action(x);
    }
    assert_eq!(e.vocoder_selected(), 0);
    // every binding in the preset names a registered target with its args
    for b in &set.current().bindings {
        assert!(registry::missing(b.action, b).is_none(), "{b:?}");
    }
    // the cycle: performance, vocoder, decks, and round
    set.select("decks");
    set.apply_request("next");
    assert_eq!(set.current().name, "performance");
    set.apply_request("next");
    assert_eq!(set.current().name, "vocoder");
}
