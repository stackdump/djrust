//! The formant voice in the engine (`fx::formant`, the `"formant"` wave engine, the `vox`
//! compose option, a binding's `"vowel"`). Held here:
//!
//! - `vox` off is the beats path: a document without a vowel keeps its bytes and CID.
//! - `vox` on adds the `vox` net (and its structure net) and changes no other net.
//! - A `"vowel"` on a binding survives a document round trip and reaches the lane.
//! - A `voice-*` instrument renders through formant lanes, deterministically, and its vowel
//!   moves live.

use djrust::fx::formant::{self, Syllable};
use djrust::gen::vox::{auto_style, VoxStyle, VOX_NET};
use djrust::pflow::Project;
use djrust::wave::*;
use djrust::{compose, JsonObject, JsonValue};

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn ov(seed: f64, extra: &[(&str, JsonValue)]) -> JsonValue {
    let mut o = JsonObject::new();
    o.set("seed", seed.into());
    for (k, v) in extra {
        o.set(*k, v.clone());
    }
    o.into()
}

fn runner(p: &Project) -> WaveRunner {
    let opts = WaveOptions::extended();
    let mut r = WaveRunner::new(SR, opts.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &opts, 0));
    r.play();
    r
}

fn blocks(r: &mut WaveRunner, n: usize) -> Vec<f32> {
    let mut out = vec![0f32; 2 * n * BLOCK];
    for b in 0..n {
        let (l, rest) = out[2 * b * BLOCK..].split_at_mut(BLOCK);
        r.process(l, Some(&mut rest[..BLOCK]), BLOCK);
    }
    out
}

#[test]
fn vox_adds_one_part_and_moves_nothing_else() {
    for (genre, style, structure) in [
        ("trance", "choir", ""),
        ("house", "robot", ""),
        ("synthwave", "choir", "standard"),
        ("techno", "robot", "standard"),
        ("lofi", "choir", ""),
    ] {
        let mut extra = vec![];
        if !structure.is_empty() {
            extra.push(("structure", JsonValue::from(structure)));
        }
        let plain = compose(genre, &ov(5.0, &extra));
        extra.push(("vox", JsonValue::from(style)));
        let sung = compose(genre, &ov(5.0, &extra));
        let vox = sung
            .nets
            .get(VOX_NET)
            .unwrap_or_else(|| panic!("{genre}: no vox net"));
        assert!(!vox.bindings.is_empty());
        assert!(
            vox.bindings.values().all(|b| b.syllable != 0),
            "{genre}: every note is sung"
        );
        assert!(vox.track.instrument.starts_with("voice-"));
        for (id, nb) in plain.nets.iter() {
            let other = sung
                .nets
                .get(id)
                .unwrap_or_else(|| panic!("{genre}: {id} gone"));
            assert_eq!(
                nb.to_json().canonical_js(),
                other.to_json().canonical_js(),
                "{genre}: {id} changed with vox on"
            );
        }
        for id in sung.nets.keys() {
            assert!(
                plain.nets.get(id).is_some() || id == VOX_NET || id == "struct-vox",
                "{genre}: unexpected net {id}"
            );
        }
        assert_eq!(plain.tempo, sung.tempo);
    }
}

#[test]
fn vox_auto_picks_by_genre() {
    assert_eq!(auto_style("trance"), Some(VoxStyle::Choir));
    assert_eq!(auto_style("techno"), Some(VoxStyle::Robot));
    assert_eq!(auto_style("italo"), Some(VoxStyle::Choir)); // like synthwave
    assert_eq!(auto_style("jazz"), None);
    let p = compose("jazz", &ov(1.0, &[("vox", "auto".into())]));
    assert!(p.nets.get(VOX_NET).is_none());
    let p = compose("ambient", &ov(1.0, &[("vox", "auto".into())]));
    assert!(p.nets.get(VOX_NET).is_some());
}

#[test]
fn a_vowel_survives_the_document_and_leaves_plain_notes_alone() {
    let p = compose("trance", &ov(9.0, &[("vox", "robot".into())]));
    let text = p.to_json().canonical_js();
    assert!(text.contains("\"vowel\""));
    let back = Project::from_json(&JsonValue::parse(&text).unwrap());
    assert_eq!(back.to_json().canonical_js(), text);
    assert_eq!(back.model_cid(), p.model_cid());
    let vox = back.nets.get(VOX_NET).unwrap();
    let texts: Vec<String> = vox
        .bindings
        .values()
        .filter_map(|b| b.vowel_text())
        .collect();
    assert_eq!(texts.len(), vox.bindings.len());
    // no vowel anywhere else: the other nets' canonical text has none
    let plain = compose("trance", &ov(9.0, &[]));
    assert!(!plain.to_json().canonical_js().contains("\"vowel\""));
    assert!(!plain.canonical_json().contains("vowel"));
    // a vowel that does not parse is no vowel
    let doc = r#"{"tempo":120,"nets":{"n":{"role":"music","track":{"channel":1},"places":{"p":{"initial":[1]}},"transitions":{"t":{"midi":{"note":60,"vowel":"zz"}}},"arcs":[{"source":"p","target":"t","weight":[1]},{"source":"t","target":"p","weight":[1]}]}}}"#;
    let p = Project::from_json(&JsonValue::parse(doc).unwrap());
    assert_eq!(
        p.nets.get("n").unwrap().bindings.get("t").unwrap().syllable,
        0
    );
}

fn sung_project(instrument: &str) -> Project {
    let mut p = compose("trance", &ov(3.0, &[("vox", "choir".into())]));
    p.nets.get_mut(VOX_NET).unwrap().track.instrument = instrument.into();
    // only the vox part sounds
    let ids: Vec<String> = p.nets.keys().to_vec();
    for id in ids {
        let nb = p.nets.get_mut(&id).unwrap();
        if id != VOX_NET && nb.role != "control" {
            nb.bindings = Default::default();
        }
    }
    p
}

#[test]
fn every_voice_instrument_renders_through_formant_lanes() {
    for name in formant::instrument_names() {
        let p = sung_project(name);
        let mut r = runner(&p);
        let lanes = r.project().unwrap().lanes_of(VOX_NET);
        assert!(
            !lanes.is_empty() && lanes.iter().all(|l| l.is_formant()),
            "{name}"
        );
        let out = blocks(&mut r, (4.0 * SR) as usize / BLOCK);
        assert!(out.iter().all(|x| x.is_finite()), "{name}");
        let rms = (out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 1e-3, "{name}: silent ({rms})");
        assert!(out.iter().all(|x| x.abs() < 4.0), "{name}: runaway");
    }
}

#[test]
fn renders_are_deterministic() {
    let p = sung_project("voice-robot-choir");
    let a = blocks(&mut runner(&p), 1500);
    let b = blocks(&mut runner(&p), 1500);
    assert!(a.iter().any(|x| x.abs() > 1e-3));
    assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
}

#[test]
fn the_vowel_moves_live() {
    let p = sung_project("voice-choir-aah");
    let mut r = runner(&p);
    blocks(&mut r, 200);
    assert!(r.vowels().iter().any(|(id, _)| id == VOX_NET));
    let took = r.set_vowel(Some(VOX_NET), 2.0);
    assert!(took >= 1);
    assert!(r
        .vowels()
        .iter()
        .filter(|(id, _)| id == VOX_NET)
        .all(|(_, v)| *v == 2.0));
    assert_eq!(r.set_vowel(Some("no-such-net"), 1.0), 0);
    // a plain lane is not a voice
    let mut plain = runner(&compose("techno", &ov(1.0, &[])));
    assert_eq!(plain.set_vowel(None, 1.0), 0);
}

#[test]
fn a_parity_options_render_ignores_the_voice_table() {
    // WaveOptions::default (beats' own path) does not consult the instrument table: a
    // voice-* instrument plays its name's family voice there, as beats would.
    let p = sung_project("voice-choir-aah");
    let opts = WaveOptions::default();
    let prepared = WaveRunner::prepare(&p, SR, &opts, 0);
    assert!(prepared.lanes_of(VOX_NET).iter().all(|l| !l.is_formant()));
    let s = Syllable::parse("da").unwrap();
    assert_eq!(Syllable::from_code(s.code()), Some(s));
}

#[test]
fn the_instruments_override_voices_the_part() {
    let mut inst = JsonObject::new();
    inst.set("vox", "voice-talkbox".into());
    let p = compose(
        "house",
        &ov(
            2.0,
            &[("vox", "robot".into()), ("instruments", inst.into())],
        ),
    );
    assert_eq!(
        p.nets.get(VOX_NET).unwrap().track.instrument,
        "voice-talkbox"
    );
}

fn level_db(instrument: &str) -> f64 {
    let p = sung_project(instrument);
    let mut r = runner(&p);
    let out = blocks(&mut r, (4.0 * SR) as usize / BLOCK);
    10.0 * (out.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / out.len() as f64).log10()
}

#[test]
fn the_voices_play_at_about_the_same_level() {
    // The track's loudness gain is set by a probe. At the probe's 8 kHz the resonators above 4 kHz (the soprano's F4 and
    // F5) and the energy above it are not what the voice plays at 48 kHz: the probe heard `voice-soprano` 19 dB louder than it
    // sounds, so its gain came out 19 dB low (and one /ee/ in a line took 13 dB off the whole line). A track with a voice is now
    // probed at 24 kHz, and the two parallel-formant presets, which summed 13 and 18 dB under the rest, are made up.
    let l: Vec<(&str, f64)> = formant::instrument_names().iter().map(|n| (*n, level_db(n))).collect();
    let top = l.iter().map(|x| x.1).fold(f64::MIN, f64::max);
    for (n, x) in &l {
        assert!(top - x < 8.0, "{n} is {:.1} dB under the loudest voice: {l:?}", top - x);
    }
}
