//! Swift tests of the wave engine's options and levels that had no Rust counterpart, for
//! behaviour the engine already has (package F of the parity audit): the synth drum kit sits at
//! the kit target through the runner (`SampledKitTests.theSynthKitSetsTheKitTarget`), the
//! family's feel template is set by `forGenre` only with the flag on (`FeelTests.forGenreSetsTheTemplate`),
//! and the sampled-width table by category and group (`SampledWidthTests.theTableByCategory`).
//! Each test names the Swift test it ports.

mod common;

use common::measure::*;
use djrust::musicality::dj::{sampled_width, SampledWidth, SAMPLED_SIDE_HZ};
use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::voices::sampler as rules;
use djrust::wave::{Master, WaveOptions, WaveRunner};
use djrust::{compose, JsonObject};

const SR: f64 = 48_000.0;

/// `SampledKitSupport.kitPhraseProject`: kick, snare and hats on beats' drum channels, velocity
/// 100, one bar of 16ths looped at 120 BPM.
fn kit_phrase_project() -> Project {
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
        n
    };
    let all: Vec<i64> = (0..16).collect();
    p.nets.insert("kick", net(36, 10, &[0, 4, 8, 12]));
    p.nets.insert("snare", net(38, 11, &[4, 12]));
    p.nets.insert("hihat", net(42, 12, &all));
    p
}

// SampledKitTests.theSynthKitSetsTheKitTarget: `kitTargetRMSdB` is where beats' synth kit sits on
// the kit phrase, as a runner plays it (the options `kitOptions(nil)` builds: the instrument table
// on, no fixes, a linear master, no bank), within 1 dB over 2 s.
#[test]
fn the_synth_kit_sets_the_kit_target() {
    let o = WaveOptions {
        master: Master::Linear,
        instruments: WaveOptions::extended().instruments,
        ..Default::default()
    };
    assert!(o.instruments.is_some() && o.bank.is_none());
    let synth = render_mono(&kit_phrase_project(), &o, SR, 2.0);
    let db = rms_db(&synth);
    eprintln!("synth kit on the kit phrase: {db:.3} dB (target {})", rules::KIT_TARGET_RMS_DB);
    assert!(
        (db - rules::KIT_TARGET_RMS_DB).abs() < 1.0,
        "synth kit {db} dB, target {}",
        rules::KIT_TARGET_RMS_DB
    );
}

// FeelTests.forGenreSetsTheTemplate: `forGenre` sets the family's template only when the flag is
// on; a prepared track keeps it on its project (`sameSongOptions` hands it to a section jump).
#[test]
fn for_genre_sets_the_template_only_with_the_flag_on() {
    let mut o = WaveOptions::default();
    assert!(o.for_genre("lofi").feel.is_none());
    o.musicality = Fixes::FEEL;
    assert_eq!(o.for_genre("lofi").feel.as_ref().map(|f| f.family.as_str()), Some("chill"));
    assert_eq!(o.for_genre("techno").feel.as_ref().map(|f| f.family.as_str()), Some("dance"));
    assert!(Fixes::ALL.contains(Fixes::FEEL));
    // a prepared track (the app's options for it) keeps the template on its project
    // (`track.prepared.feelTable != nil` follows `feel` in `prepare`, and has no accessor)
    let mut ov = JsonObject::new();
    ov.set("seed", 42.0.into());
    ov.set("structure", "minimal".into());
    let p = compose("lofi", &ov.into());
    let prepared = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::for_track("lofi", 42.0), 0);
    assert_eq!(prepared.feel.as_ref().map(|f| f.family.as_str()), Some("chill"));
}

// SampledWidthTests.theTableByCategory: a sampled slot's width by its catalog category and the
// part's group: bass.* 0, keys.* and pad 1.2 over 300 Hz, guitar.* 0.8, synth.lead 0.6, the rest
// as loaded, the bass part mono whatever it plays.
#[test]
fn the_sampled_width_table_by_category() {
    let w = sampled_width;
    let mono = SampledWidth {
        width: 0.0,
        side_hz: None,
    };
    assert_eq!(w(Some("bass.electric"), "melody"), mono);
    assert_eq!(w(Some("synth.pluck"), "bass"), mono, "the bass part is mono");
    assert_eq!(w(None, "bass").width, 0.0);
    assert_eq!(
        w(Some("keys.piano"), "harmony"),
        SampledWidth {
            width: 1.2,
            side_hz: Some(SAMPLED_SIDE_HZ)
        }
    );
    assert_eq!(
        w(Some("pad"), "harmony"),
        SampledWidth {
            width: 1.2,
            side_hz: Some(300.0)
        }
    );
    assert_eq!(
        w(Some("guitar.acoustic"), "harmony"),
        SampledWidth {
            width: 0.8,
            side_hz: None
        }
    );
    assert_eq!(
        w(Some("synth.lead"), "melody"),
        SampledWidth {
            width: 0.6,
            side_hz: None
        }
    );
    assert_eq!(w(Some("strings"), "harmony"), SampledWidth::AS_LOADED);
    assert_eq!(w(None, "melody"), SampledWidth::AS_LOADED);
    assert_eq!(SAMPLED_SIDE_HZ, 300.0);
    // (Swift's `!sampledWidthSource.isEmpty`, "the table names its source", has no twin: the
    // string is not ported)
}
