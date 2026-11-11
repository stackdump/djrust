//! `WaveDeckSource.sampledNets` (Sampler.swift:910-920, `Engine::sampled_nets`): which of the
//! latest project's nets play on a sampler, and from what (net id -> the slot's source, as
//! `SamplerSource.description`). Feature `sf2`; the bank is synthesised (a looped sine on every
//! GM program and a drum kit), as in `deck_sf2.rs` / `deck_kits.rs`.
//!
//! Ports `SampledKitTests` (the kit pick shows up in `sampledNets`, and goes away with it:
//! SampledKitTests.swift:170-194). `MinecraftTests`' palette case (:235-262) needs the Minecraft
//! bank imported from a game install, which is not here.
#![cfg(feature = "sf2")]

use std::sync::Arc;

use djrust::session::action::DeckAction;
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, PerformCtx, Renderer};
use djrust::host::sampler::{synthetic_sf2, Library};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const FILE: &str = "MuseScore_General.sf2";
const KIT: &str = "MuseScore_General.sf2#128:0";

fn bank_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("djhost-sampled-nets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let names: Vec<String> = (0..128).map(|p| format!("GM {p}")).collect();
    let mut presets: Vec<(&str, u16, u16)> = names
        .iter()
        .enumerate()
        .map(|(p, n)| (n.as_str(), 0u16, p as u16))
        .collect();
    presets.push(("Standard", 128, 0));
    presets.push(("Brush", 128, 40));
    std::fs::write(d.join(FILE), synthetic_sf2(&presets)).unwrap();
    d
}

fn with_bank(dir: &std::path::Path) -> (Engine, Renderer) {
    let lib = Arc::new(Library::scan(dir));
    let mut c = EngineConfig::new(SR);
    c.library = Some(lib);
    engine(c)
}

fn render(r: &mut Renderer, blocks: usize) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
    }
}

fn started(e: &mut Engine, r: &mut Renderer) {
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    let spec = TrackSpec::new("house", 11.0, Some("standard"));
    assert!(e.generate("A", &spec));
    e.set_tempo("A", 240.0);
    e.play("A");
    render(r, 100);
}

/// SampledKitTests.aKitPickPlaysTheDrumsOnTheSampler (:170-194): before a kit pick no `kick` net
/// is sampled and no source is a kit; after `setKit` a `kick` net plays a `kit:` source; cleared,
/// no kit is left.
#[test]
fn a_kit_pick_shows_in_the_sampled_nets_and_goes_with_it() {
    let dir = bank_dir("kit");
    let (mut e, mut r) = with_bank(&dir);
    started(&mut e, &mut r);
    let before = e.sampled_nets("A");
    assert!(
        !before.keys().any(|k| k.starts_with("kick")),
        "no kick before the pick: {before:?}"
    );
    assert!(!before.values().any(|v| v.starts_with("kit:")), "{before:?}");
    let tick = e.deck("A").unwrap().mirror.tick();
    e.set_kit("A", Some(KIT));
    render(&mut r, 200);
    assert!(e.deck("A").unwrap().mirror.tick() > tick, "the song carried on");
    let kit = e.sampled_nets("A");
    assert!(kit.keys().any(|k| k.starts_with("kick")), "{kit:?}");
    let kick = kit.iter().find(|(k, _)| k.starts_with("kick")).unwrap();
    assert_eq!(kick.1, "kit:MuseScore_General.sf2#128:0", "{kit:?}");
    assert!(kit.values().any(|v| v.starts_with("kit:")), "{kit:?}");
    e.set_kit("A", None);
    render(&mut r, 200);
    let after = e.sampled_nets("A");
    assert!(
        !after.values().any(|v| v.starts_with("kit:")),
        "no kit after the clear: {after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tonal net on a bank program: the source reads `sf2:<file>#<program>` (MinecraftTests' bass
/// line, over the synthetic bank), and a channel the sound panel puts on its synth voice drops
/// out of the table while it is there (Swift's lane slot is -1), and comes back with the sample.
#[test]
fn a_bass_net_names_its_program_and_a_synth_voice_leaves_the_table() {
    let dir = bank_dir("bass");
    let (mut e, mut r) = with_bank(&dir);
    started(&mut e, &mut r);
    let nets = e.sampled_nets("A");
    let (bass, source) = nets
        .iter()
        .find(|(k, _)| k.starts_with("bass"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .unwrap_or_else(|| panic!("a sampled bass: {nets:?}"));
    let basses = nets.keys().filter(|k| k.starts_with("bass")).count();
    assert!(
        source.starts_with(&format!("sf2:{FILE}#")),
        "{bass} plays {source}"
    );
    // every entry is a source the bank holds, none is the unknown-slot "?"
    assert!(nets.values().all(|v| v != "?"), "{nets:?}");
    // the bass channel to its synth voice: it leaves, the others stay
    let ch = e
        .deck("A")
        .unwrap()
        .project
        .as_ref()
        .unwrap()
        .nets
        .get(&bass)
        .map(|n| n.track.channel)
        .unwrap();
    e.perform(
        &DeckAction::VoiceSynth {
            deck: "A".into(),
            channel: ch,
            on: true,
        },
        PerformCtx::default(),
    );
    render(&mut r, 20);
    let synth = e.sampled_nets("A");
    assert!(!synth.keys().any(|k| k.starts_with("bass")), "{synth:?}");
    assert_eq!(
        synth.len() + basses,
        nets.len(),
        "only the bass channel's nets left: {synth:?}"
    );
    e.perform(
        &DeckAction::VoiceSynth {
            deck: "A".into(),
            channel: ch,
            on: false,
        },
        PerformCtx::default(),
    );
    assert_eq!(e.sampled_nets("A"), nets, "and back with the sample");
    let _ = std::fs::remove_dir_all(&dir);
}

/// No bank, nothing sampled: the table is empty (Swift's `[:]`), and a deck with nothing loaded
/// has none.
#[test]
fn without_a_bank_nothing_is_sampled() {
    let (mut e, mut r) = engine(EngineConfig::new(SR));
    assert!(e.sampled_nets("A").is_empty());
    started(&mut e, &mut r);
    assert!(e.sampled_nets("A").is_empty());
    assert!(e.sampled_nets("nowhere").is_empty());
}
