//! The `Project` half of `DeckKits.swift` and the sound panel's `ChannelVoice`. Held to the
//! Swift source (no oracle dumps `Project.drumChannels` on its own; `swift/perform.json` has the
//! deck-level result of every poke): a composed track's drum channels are the kick, snare, hats
//! and clap channels, a kit or a palette goes on the nets it names and on no other.

use djrust::pflow::Project;
use djrust::spec::TrackSpec;
use djrust::wave::{WaveOptions, WaveRunner};

fn project(genre: &str, seed: f64) -> Project {
    TrackSpec::new(genre, seed, None).project()
}

#[test]
fn drum_channels_are_the_drum_nets_channels_in_order() {
    let p = project("techno", 1.0);
    let chans = p.drum_channels();
    assert!(!chans.is_empty());
    assert!(chans.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
    assert!(chans.iter().all(|c| djrust::sound::is_drum_channel(*c)));
    // the channels that play include every drum channel and nothing from a control net
    let all = p.channels();
    assert!(chans.iter().all(|c| all.contains(c)));
}

#[test]
fn a_kit_goes_on_every_drum_net_and_reads_back() {
    let mut p = project("techno", 1.0);
    assert_eq!(p.drum_kit(), None);
    p.set_drum_kit(Some("kit-x"));
    assert_eq!(p.drum_kit().as_deref(), Some("kit-x"));
    for id in p.nets.keys() {
        let nb = p.nets.get(id).unwrap();
        let drum = djrust::sound::is_drum_channel(nb.track.channel);
        assert_eq!(nb.track.kit.is_some(), drum, "{id}");
    }
    p.set_drum_kit(None);
    assert_eq!(p.drum_kit(), None);
}

#[test]
fn a_palette_goes_on_every_net() {
    let mut p = project("house", 5.0);
    assert_eq!(p.palette(), None);
    p.set_palette(Some("minecraft@32"));
    assert_eq!(p.palette().as_deref(), Some("minecraft@32"));
    assert!(p
        .nets
        .keys()
        .iter()
        .all(|id| p.nets.get(id).unwrap().track.palette.as_deref() == Some("minecraft@32")));
}

#[test]
fn a_prepared_project_knows_what_each_channel_plays() {
    let p = project("techno", 1.0);
    let w = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::extended(), 0);
    let voices = w.channel_voices();
    // one entry per channel a lane plays on; the drum channels are not tonal
    for ch in p.drum_channels() {
        let v = voices.get(&ch).expect("a drum channel has a lane");
        assert!(!v.tonal);
        assert_eq!(w.channel_voice(ch).as_ref(), Some(v));
    }
    assert!(voices.values().any(|v| v.tonal));
    assert!(w.channel_voice(99).is_none());
}
