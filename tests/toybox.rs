//! The Toy Box bank (`sound::toybox`): deterministic bytes, a SoundFont the crate's own reader
//! lists in full, every tonal sample in tune with its declared root (measured here on our own
//! synthesized audio), seamless loops, and the palette that plays it whenever `toybox.sf2` is
//! in the SoundFont folder — an ordinary, publishable bank.

use djrust::session::publish::LocalOnly;
use djrust::sound::toybox::{self, ToyKind};
use djrust::sound::{Installed, PaletteSet, Sounds};
use djrust::voices::bank::sf2_presets;
use djrust::voices::{SamplerSource, VoiceCatalog};
use sha2::{Digest, Sha256};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The bank's sha256, pinned: a change to any voice changes it (bump `toybox::VERSION` then).
const SHA256: &str = "8edfdf1964204187a2422221b4208215d5a47bef0e2c706466aaaabfed677436";

#[test]
fn build_is_deterministic_and_pinned() {
    let a = toybox::build();
    let b = toybox::build();
    assert_eq!(a, b);
    assert!(a.len() < 3_000_000, "{} bytes", a.len());
    let h = hex(&Sha256::digest(&a));
    assert_eq!(
        h,
        SHA256,
        "toybox.sf2 changed (version {})",
        toybox::VERSION
    );
}

#[test]
fn the_reader_lists_every_program_and_the_kit() {
    let bytes = toybox::build();
    let presets = sf2_presets(&bytes);
    assert_eq!(presets.len(), toybox::programs().len() + 1);
    for p in toybox::programs() {
        let found = presets
            .iter()
            .find(|q| q.program == p.program && q.bank == 0)
            .unwrap_or_else(|| panic!("{} missing", p.name));
        assert_eq!(found.name, toybox::preset_name(p));
    }
    let kit = presets
        .iter()
        .find(|q| q.program == toybox::KIT_PROGRAM)
        .unwrap();
    assert_eq!(kit.name, toybox::KIT_NAME);
    assert!(djrust::voices::drumkits::is_kit_name(&kit.name));
    let kits = djrust::voices::drumkits::scan(&[("/sf/toybox.sf2".into(), presets.clone())]);
    assert_eq!(
        kits.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
        vec![toybox::kit_id()]
    );
}

#[test]
fn names_are_our_own() {
    let banned = [
        "harp2",
        "bassattack",
        "didgeridoo",
        "pling",
        "bit",
        "icechime",
        "xylobone",
        "iron_xylophone",
        "cow_bell",
        "goat_horn",
        "amethyst",
        "anvil",
        "orb",
        "minecraft",
    ];
    for p in toybox::programs() {
        for b in banned {
            assert_ne!(p.name, b);
            assert!(!p.name.split('-').any(|w| w == b), "{}", p.name);
            assert!(!p.description.to_lowercase().contains("minecraft"));
        }
    }
}

/// The frequency of the strongest component within ±80 cents of `hz` in `x` (Hann window), by
/// a 1-cent scan refined in 0.05-cent steps.
fn peak_near(x: &[f32], hz: f64, rate: f64) -> (f64, f64) {
    let n = x.len();
    let w: Vec<f64> = (0..n)
        .map(|i| {
            x[i] as f64
                * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())
        })
        .collect();
    let mag = |f: f64| {
        let k = 2.0 * std::f64::consts::PI * f / rate;
        let (cr, ci) = (k.cos(), -k.sin());
        let (mut pr, mut pi, mut re, mut im) = (1.0f64, 0.0f64, 0.0, 0.0);
        for v in &w {
            re += v * pr;
            im += v * pi;
            let t = pr * cr - pi * ci;
            pi = pr * ci + pi * cr;
            pr = t;
        }
        re * re + im * im
    };
    let at = |c: f64| hz * 2f64.powf(c / 1200.0);
    let mut best = (0.0, -1.0);
    let mut c = -80.0;
    while c <= 80.0 {
        let m = mag(at(c));
        if m > best.1 {
            best = (c, m);
        }
        c += 1.0;
    }
    let centre = best.0;
    let mut c = centre - 1.5;
    while c <= centre + 1.5 {
        let m = mag(at(c));
        if m > best.1 {
            best = (c, m);
        }
        c += 0.05;
    }
    best
}

#[test]
fn every_tonal_sample_sounds_at_its_root() {
    let rate = toybox::SAMPLE_RATE as f64;
    for s in toybox::samples() {
        if s.fixed_pitch {
            continue;
        }
        let hz = 440.0 * 2f64.powf((s.root as f64 - 69.0) / 12.0);
        // a steady stretch: the loop when it has one, else 30 ms in for up to 0.4 s
        let window: &[f32] = match &s.loop_ {
            Some(l) => &s.pcm[l.clone()],
            None => {
                let a = (0.03 * rate) as usize;
                &s.pcm[a..(a + (0.4 * rate) as usize).min(s.pcm.len())]
            }
        };
        let (cents, _) = peak_near(window, hz, rate);
        assert!(
            cents.abs() <= 3.0,
            "{} (root {}) is {cents:.2} cents off",
            s.name,
            s.root
        );
    }
}

#[test]
fn samples_are_clean() {
    for s in toybox::samples() {
        let peak = s.pcm.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05 && peak <= 0.9, "{} peak {peak}", s.name);
        // starts from silence, and a one-shot ends in it (no clicks)
        assert!(s.pcm[0].abs() < 1e-3, "{} starts at {}", s.name, s.pcm[0]);
        match &s.loop_ {
            None => assert!(s.pcm.last().unwrap().abs() < 1e-3, "{} tail", s.name),
            Some(l) => {
                // the loop is seamless: the frame after its end is the frame at its start
                let jump = (s.pcm[l.end] - s.pcm[l.start]).abs();
                assert!(jump < 2e-3, "{} loop seam {jump}", s.name);
                assert!(l.end + 8 <= s.pcm.len());
                assert_eq!(l.start % 2, 0);
            }
        }
    }
    // every program and the kit, each tonal program in three zones covering 0...127
    for p in toybox::programs() {
        let zones: Vec<_> = toybox::samples()
            .into_iter()
            .filter(|s| s.program == p.program)
            .collect();
        match p.kind {
            ToyKind::Tone => {
                assert_eq!(zones.len(), 3, "{}", p.name);
                assert_eq!(*zones[0].keys.start(), 0);
                assert_eq!(*zones[2].keys.end(), 127);
                assert_eq!(zones.iter().all(|z| z.loop_.is_some()), p.looped);
            }
            ToyKind::Fx => assert_eq!(zones.len(), 1),
        }
    }
}

#[test]
fn the_toybox_palette_plays_from_the_folder_and_publishes() {
    let set = PaletteSet::shared();
    let toy = set.named(Some("toybox")).expect("the toybox palette");
    assert_eq!(toy.label, "Toy Box");
    let catalog = VoiceCatalog::bundled();

    // not installed: unavailable, saying why
    let none = Installed::nothing("/sf");
    let ctx = Sounds {
        catalog: &catalog,
        installed: &none,
    };
    assert!(toy
        .unavailable_reason(set, ctx)
        .unwrap()
        .contains("toybox.sf2"));

    // the bank alone in the folder: playable, not a General MIDI SoundFont
    let inst = Installed {
        files: vec![toybox::FILE_NAME.into()],
        ..Installed::nothing("/sf")
    };
    assert!(inst.sound_fonts().is_empty());
    let ctx = Sounds {
        catalog: &catalog,
        installed: &inst,
    };
    assert_eq!(toy.unavailable_reason(set, ctx), None);
    assert_eq!(set.kit_for(Some("toybox"), ctx), Some(toybox::kit_id()));
    let src = toy.sources("melody", 7.0, set, ctx).unwrap();
    let lead = [
        "pluck-harp",
        "glass-tine",
        "chip-square",
        "reed-flute",
        "twang",
        "toy-horn",
    ];
    assert_eq!(src.len(), lead.len());
    for s in &src {
        let SamplerSource::Sf2(path, prog) = s else {
            panic!("{s:?}")
        };
        assert_eq!(path, "/sf/toybox.sf2");
        assert!(lead.iter().any(|n| toybox::program_named(n) == Some(*prog)));
        assert!(!s.is_local_only() && s.is_named_only());
    }
    let label = djrust::sound::option_label(set, "bank:toybox.lead", "melody", 7.0, ctx);
    assert!(lead.contains(&label.as_str()), "{label}");

    // ours: never refused for publishing
    assert!(!toy.is_local_only(set));
    assert!(!LocalOnly::bundled().palette_is_local_only("toybox"));
}
