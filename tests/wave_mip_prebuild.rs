//! Every saw / square wavetable lane is prepared with its band-limited tables
//! (`WaveRunner.tonalLane`: "every wavetable lane can sweep (the sound panel turns it on live) ...
//! for a saw or square the band-limited tables a swept note crosses octaves on (shared, built
//! once per rate)"), so a live pitch sweep (`Lane::set_voice`, on the audio thread) is a flag
//! flip. Held by `WaveDeckSourceTests.voiceTweaksDoNotAllocate` in Swift (a counting allocator:
//! `djhost/tests/deck_rt.rs` has the port); these are the prepare-time halves.

use std::sync::Arc;

use djrust::musicality::fixes::Fixes;
use djrust::wave::mip::MipTables;
use djrust::wave::{Lane, VoiceLaneParams, VoiceTweak, WaveOptions, WaveRunner, WAVE_TABLE_SIZE};
use djrust::TrackSpec;

const SR: f64 = 22_050.0;

fn wavetable(l: &Lane) -> bool {
    l.is_tonal() && !l.is_pluck() && (l.voice.wave == "saw" || l.voice.wave == "square")
}

fn prepared(genre: &str, seed: f64, opts: &WaveOptions) -> djrust::wave::WaveProject {
    let p = TrackSpec::new(genre, seed, Some("minimal")).project();
    WaveRunner::prepare(&p, SR, opts, 0)
}

/// Swift: every non-pluck saw / square lane has `mip`, `mipTop` and `mipBase` at prepare; a
/// sine, a triangle and a string have none. `mipOn` stays off until something asks for the
/// tables (a sweep in the voice, `.subtractive`'s filter envelope).
#[test]
fn every_saw_and_square_lane_has_its_band_limited_tables_after_prepare() {
    let table = WaveOptions::extended();
    let beats = WaveOptions::default();
    let no_fixes = WaveOptions {
        musicality: Fixes::EMPTY,
        ..WaveOptions::extended()
    };
    let (mut with, mut without) = (0, 0);
    for opts in [&table, &beats, &no_fixes] {
        for (genre, seed) in [("country", 7.0), ("techno", 42.0), ("lofi", 3.0), ("trance", 9.0)] {
            let wp = prepared(genre, seed, opts);
            for l in wp.lanes().iter().filter(|l| l.is_tonal()) {
                let ctx = format!("{genre} {seed} lane {} ({})", l.net_id, l.voice.wave);
                if wavetable(l) {
                    with += 1;
                    assert_eq!(
                        l.mip_len(),
                        11 * (WAVE_TABLE_SIZE + 1),
                        "{ctx}: eleven tables of one cycle and a guard point"
                    );
                    // on only when its voice sweeps or its filter envelope opens
                    let sweeps = l.voice.pitch_time > 0.0 && l.voice.pitch_from != l.voice.pitch_to;
                    let filtered = l.filter_envelope().0 > 0.0;
                    assert_eq!(l.mip_on(), sweeps || filtered, "{ctx}: mip_on");
                } else {
                    without += 1;
                    assert!(!l.has_mip(), "{ctx}: no band-limited tables");
                    assert!(!l.mip_on(), "{ctx}: and none in use");
                }
            }
        }
    }
    assert!(with > 10 && without > 10, "{with} wavetable lanes, {without} others");
}

/// A saw lane prepared without a sweep (and without `.subtractive`) switches a sweep on from
/// the sound panel and reads the tables it was prepared with: the same bits as a lane prepared
/// with the sweep in its voice would, since the tables are the same.
#[test]
fn a_sweep_switched_on_live_reads_the_prepared_tables() {
    let opts = WaveOptions {
        musicality: Fixes::EMPTY,
        ..WaveOptions::extended()
    };
    let mut wp = prepared("country", 7.0, &opts);
    let ch = wp
        .lanes()
        .iter()
        .find(|l| wavetable(l) && l.channel >= 0 && !l.mip_on())
        .map(|l| l.channel)
        .expect("a saw or square lane that is not swept");
    let before: Vec<usize> = wp.lanes().iter().map(|l| l.mip_len()).collect();
    let v = wp.channel_voice(ch).map(|c| c.voice);
    let t = VoiceTweak {
        pitch_from: Some(4.0),
        pitch_to: Some(1.0),
        pitch_time: Some(0.1),
        ..Default::default()
    };
    wp.apply_voice(&VoiceLaneParams::new(ch, &t, false, v.as_ref(), SR));
    for l in wp.lanes().iter().filter(|l| l.channel == ch && wavetable(l)) {
        assert!(l.pm_on() && l.mip_on(), "{}: swept, on its tables", l.net_id);
    }
    // nothing was built to do it
    let after: Vec<usize> = wp.lanes().iter().map(|l| l.mip_len()).collect();
    assert_eq!(before, after);
    // and off again
    wp.apply_voice(&VoiceLaneParams::new(
        ch,
        &VoiceTweak::default(),
        false,
        v.as_ref(),
        SR,
    ));
    for l in wp.lanes().iter().filter(|l| l.channel == ch && wavetable(l)) {
        assert!(!l.pm_on() && !l.mip_on(), "{}: back to the instrument", l.net_id);
        assert!(l.has_mip());
    }
}

/// `MipTables.shared`: one build per wave and rate (the key is `"\(wave)@\(Int(sampleRate))"`),
/// the same arrays for every caller, and the bits of a fresh build.
#[test]
fn the_tables_are_built_once_per_wave_and_rate_and_shared() {
    let a = MipTables::shared("saw", 31_000.0);
    let b = MipTables::shared("saw", 31_000.0);
    assert!(Arc::ptr_eq(&a.samples, &b.samples) && Arc::ptr_eq(&a.top, &b.top));
    // the rate is cut to an integer for the key, as Swift's `Int(sampleRate)`
    let c = MipTables::shared("saw", 31_000.9);
    assert!(Arc::ptr_eq(&a.samples, &c.samples));
    let square = MipTables::shared("square", 31_000.0);
    let other_rate = MipTables::shared("saw", 31_001.0);
    assert!(!Arc::ptr_eq(&a.samples, &square.samples));
    assert!(!Arc::ptr_eq(&a.samples, &other_rate.samples));
    assert_ne!(&a.samples[..], &square.samples[..]);
    // the same bits as a build of its own
    let fresh = MipTables::new("saw", 31_000.0);
    assert_eq!(&a.samples[..], &fresh.samples[..]);
    assert_eq!(&a.top[..], &fresh.top[..]);
    assert_eq!(a.samples.len(), 11 * (WAVE_TABLE_SIZE + 1));
    assert_eq!(a.top.len(), 11);
}
