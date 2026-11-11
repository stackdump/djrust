//! The device side of the mixer without a device: channel-pair routing of the master and the
//! headphone cue (`CueRouting.swift` / `ChannelRouterAU`), the second-device ring, mono.

use djrust::TrackSpec;
use djrust::host::deck::output::{Interleaver, Routing};
use djrust::host::deck::{engine, EngineConfig, Renderer};
use rtrb::RingBuffer;

const SR: f64 = 48_000.0;

fn playing(cue_deck: bool) -> (djrust::host::deck::Engine, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.generate("A", &TrackSpec::new("techno", 42.0, Some("minimal")));
    e.set_pfl("A", cue_deck);
    e.set_headphone_mix(0.0);
    e.set_headphone_level(1.0);
    let (mut l, mut rr) = (vec![0f32; 256], vec![0f32; 256]);
    r.render_block(&mut l, &mut rr);
    e.play("A");
    (e, r)
}

fn rms(x: impl Iterator<Item = f32>) -> f64 {
    let (s, n) = x.fold((0.0, 0usize), |(s, n), v| (s + (v as f64).powi(2), n + 1));
    (s / n.max(1) as f64).sqrt()
}

#[test]
fn routing_knows_what_fits() {
    assert!(Routing::Stereo.fits(2));
    assert!(Routing::Pairs {
        master_pair: 0,
        cue_pair: 2
    }
    .fits(4));
    assert!(
        !Routing::Pairs {
            master_pair: 0,
            cue_pair: 2
        }
        .fits(2),
        "notEnoughChannels"
    );
    assert!(Routing::Pairs {
        master_pair: 2,
        cue_pair: 0
    }
    .fits(4));
    assert!(!Routing::Stereo.has_cue() && Routing::CueDevice.has_cue());
}

#[test]
fn a_four_channel_interface_gets_the_master_on_1_2_and_the_cue_on_3_4() {
    let routing = Routing::Pairs {
        master_pair: 0,
        cue_pair: 2,
    };
    let (mut e, mut r) = playing(true);
    e.set_volume("A", 0.0); // fader down: the master is silent, the cued deck is not
    let mut inter = Interleaver::new();
    let mut out = vec![0f32; 4 * 512];
    // settle the gain ramp
    inter.fill(&mut r, &routing, 4, &mut out, None);
    let mut master = 0.0;
    let mut cue = 0.0;
    for _ in 0..60 {
        inter.fill(&mut r, &routing, 4, &mut out, None);
        master += rms(out.chunks(4).flat_map(|f| [f[0], f[1]]));
        cue += rms(out.chunks(4).flat_map(|f| [f[2], f[3]]));
    }
    assert!(master < 1e-6, "master pair {master}");
    assert!(cue > 0.05, "cue pair {cue}");
    // fader up, cue off: the master pair sounds and the cue pair is silent
    e.set_volume("A", 1.0);
    e.set_pfl("A", false);
    inter.fill(&mut r, &routing, 4, &mut out, None);
    let (mut master, mut cue) = (0.0, 0.0);
    for _ in 0..60 {
        inter.fill(&mut r, &routing, 4, &mut out, None);
        master += rms(out.chunks(4).flat_map(|f| [f[0], f[1]]));
        cue += rms(out.chunks(4).flat_map(|f| [f[2], f[3]]));
    }
    assert!(master > 0.05, "{master}");
    assert!(cue < 1e-6, "{cue}");
}

#[test]
fn stereo_and_pair_routing_carry_the_same_master() {
    let render = |routing: Routing, channels: usize| {
        let (_e, mut r) = playing(false);
        let mut inter = Interleaver::new();
        let mut out = vec![0f32; channels * 512];
        let mut master = Vec::new();
        for _ in 0..40 {
            inter.fill(&mut r, &routing, channels, &mut out, None);
            master.extend(out.chunks(channels).flat_map(|f| [f[0], f[1]]));
        }
        master
    };
    let stereo = render(Routing::Stereo, 2);
    let pairs = render(
        Routing::Pairs {
            master_pair: 0,
            cue_pair: 2,
        },
        4,
    );
    let wide = render(Routing::Stereo, 8);
    assert!(rms(stereo.iter().copied()) > 0.05);
    assert!(stereo == pairs, "the cue does not touch the master");
    assert!(stereo == wide, "channels past the pair stay silent");
}

#[test]
fn the_master_can_sit_on_a_later_pair() {
    let routing = Routing::Pairs {
        master_pair: 2,
        cue_pair: 0,
    };
    let (_e, mut r) = playing(true);
    let mut inter = Interleaver::new();
    let mut out = vec![0f32; 4 * 512];
    for _ in 0..40 {
        inter.fill(&mut r, &routing, 4, &mut out, None);
    }
    assert!(
        rms(out.chunks(4).flat_map(|f| [f[2], f[3]])) > 0.01,
        "master on 3/4"
    );
    assert!(
        rms(out.chunks(4).flat_map(|f| [f[0], f[1]])) > 0.01,
        "cue on 1/2"
    );
}

#[test]
fn a_mono_device_gets_the_mid() {
    let (_e, mut r) = playing(false);
    let mut inter = Interleaver::new();
    let mut out = vec![0f32; 512];
    for _ in 0..40 {
        inter.fill(&mut r, &Routing::Stereo, 1, &mut out, None);
    }
    assert!(rms(out.iter().copied()) > 0.01);
}

#[test]
fn a_second_device_drains_the_cue_through_a_ring() {
    let (mut e, mut r) = playing(true);
    e.set_volume("A", 0.0);
    let (mut tx, mut rx) = RingBuffer::<f32>::new(1 << 16);
    let mut inter = Interleaver::new();
    let mut out = vec![0f32; 2 * 512];
    // before the first callback, the second device plays silence
    let mut early = vec![1f32; 2 * 64];
    Interleaver::fill_from_ring(&mut rx, 2, &mut early);
    assert!(early.iter().all(|v| *v == 0.0));
    for _ in 0..30 {
        inter.fill(&mut r, &Routing::CueDevice, 2, &mut out, Some(&mut tx));
    }
    assert!(
        rms(out.iter().copied()) < 1e-6,
        "the main device carries only the master"
    );
    let mut cue = vec![0f32; 2 * 512];
    let mut heard = 0.0;
    for _ in 0..30 {
        Interleaver::fill_from_ring(&mut rx, 2, &mut cue);
        heard += rms(cue.iter().copied());
    }
    assert!(
        heard > 0.05,
        "the cue arrived on the second device: {heard}"
    );
}
