//! Ports of Swift `LayerBudgetTests`, `BandStopTests`, `KickOutTests`, `DropGapTests` and
//! `RiserTests`. The planners (`build_rests`, the riser windows) are held to the Swift
//! expectations exactly; the renders go through this port's `WaveRunner` with the planned
//! `RestSchedule` as `WaveOptions::rests` (Swift passes `musicality` flags and builds the
//! same schedule inside `prepare`, which now does: `wave_prepare.rs`). Not ported:
//! `OfflineRender.layersPerSection`. The choke / open-hat render is in `wave_drums.rs`. The
//! `.duck` interplay is below; the riser's renders are in `wave_riser.rs`; the allocation
//! counters of all of it are in `crates/djhost/tests/deck_rt.rs`.

use djrust::gen::tables::BeatsTables;
use djrust::musicality::dj::{drop_gaps, DROP_GAP_STEPS};
use djrust::musicality::mix_balance::MixBalance;
use djrust::musicality::rest_gestures::*;
use djrust::musicality::riser::{riser_windows, RiserRise, RiserSpec, RiserVoice};
use djrust::musicality::Fixes;
use djrust::pflow::{NetBundle, Project, Section};
use djrust::wave::rests::{Class, Entry, RestSchedule};
use djrust::wave::runner::{Master, WaveOptions, WaveRunner};
use djrust::TrackSpec;

fn section(name: &str, steps: i64) -> Section {
    Section {
        name: name.into(),
        steps,
        phrases: Default::default(),
    }
}

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (s, note) in notes {
        n.bind(*s, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

fn render(
    p: &Project,
    sr: f64,
    rests: Option<RestSchedule>,
    seconds: f64,
    start_tick: i64,
) -> Vec<f32> {
    let mut o = WaveOptions::default();
    o.master = Master::Linear;
    o.rests = rests;
    let mut r = WaveRunner::new(sr, o.clone());
    r.load_prepared(WaveRunner::prepare(p, sr, &o, start_tick));
    r.play();
    let mut out = vec![0.0f32; (seconds * sr) as usize];
    let mut off = 0;
    while off < out.len() {
        let n = 256.min(out.len() - off);
        r.process(&mut out[off..off + n], None, n);
        off += n;
    }
    out
}

fn rms_db(x: &[f32], a: usize, b: usize) -> f64 {
    let s = &x[a..b];
    if s.is_empty() {
        return -200.0;
    }
    let sum: f64 = s.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * (sum / s.len() as f64).max(1e-20).log10()
}

// MARK: layer budget

fn layered() -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    let r = |g: &str, ch: i64, note: i64| {
        ring(g, ch, &[(0, note), (4, note), (8, note), (12, note)], 200)
    };
    let mut with_riff = |name: &str, g: &str, ch: i64, note: i64| {
        let mut n = r(g, ch, note);
        n.riff_group = g.into();
        p.nets.insert(name, n);
    };
    with_riff("bass", "bass", 6, 33);
    with_riff("melody", "melody", 1, 72);
    with_riff("harmony", "harmony", 2, 60);
    with_riff("arp", "arp", 3, 67);
    p.structure = vec![section("intro", 128), section("drop", 128)];
    p
}

#[test]
fn priority_is_bass_melody_harmony_arp() {
    assert_eq!(
        layer_class_priority(),
        vec![Class::BASS, Class::MELODY, Class::HARMONY, Class::ARP]
    );
    assert_eq!(layer_group("lead"), "melody");
    assert_eq!(layer_group("chords"), "harmony");
}

#[test]
fn flag_is_registered_and_off_for_parity() {
    assert!(Fixes::ALL.contains(Fixes::LAYER_BUDGET));
    assert_eq!(Fixes::named("layerBudget"), Some(Fixes::LAYER_BUDGET));
    assert!(build_rests(&layered(), Fixes::EMPTY, None).is_empty());
}

/// Each section keeps its first `cap` parts in priority order and rests the rest over the
/// whole section.
#[test]
fn budgets_keep_the_priority_parts() {
    let p = layered();
    let b = layer_budgets(&p);
    assert_eq!(b.iter().map(|x| x.cap).collect::<Vec<_>>(), vec![1, 3]);
    assert!(b
        .iter()
        .all(|x| x.playing == vec![Class::BASS, Class::MELODY, Class::HARMONY, Class::ARP]));
    assert_eq!(b[0].kept(), vec![Class::BASS]);
    assert_eq!(
        b[0].rested(),
        vec![Class::MELODY, Class::HARMONY, Class::ARP]
    );
    assert_eq!(
        b[1].kept(),
        vec![Class::BASS, Class::MELODY, Class::HARMONY]
    );
    assert_eq!(b[1].rested(), vec![Class::ARP]);
    assert_eq!(
        layer_budget_rests(&p),
        vec![
            Entry::new(
                0,
                128,
                Class::MELODY.union(Class::HARMONY).union(Class::ARP)
            ),
            Entry::new(128, 256, Class::ARP),
        ]
    );
    assert_eq!(build_rests(&p, Fixes::LAYER_BUDGET, None).entries.len(), 2);
    assert!(build_rests(&p, Fixes::EMPTY, None).is_empty());
    // within budget: nothing rests
    let mut small = p.clone();
    small.nets.remove("harmony");
    small.nets.remove("arp");
    small.structure = vec![section("drop", 128)];
    assert!(layer_budget_rests(&small).is_empty());
}

/// Without the flag beats' drop stacks four melodic parts (edm 4242); with it every section
/// over its budget rests its lowest-priority parts there, and none within it does.
#[test]
fn composed_track_is_capped() {
    let p = TrackSpec::new("edm", 4242.0, Some("standard")).project();
    let budgets = layer_budgets(&p);
    assert!(budgets.iter().any(|b| b.playing.len() > b.cap));
    let drop = p.structure.iter().position(|s| s.name == "drop").unwrap();
    assert!(budgets[drop].playing.len() == 4 && budgets[drop].rested() == vec![Class::ARP]);
    let s = build_rests(&p, Fixes::LAYER_BUDGET, None);
    let mut start = 0;
    for (i, sec) in p.structure.iter().enumerate() {
        let mask = s.mask(start + sec.steps / 2);
        let mut want = Class::NONE;
        for c in budgets[i].rested() {
            want = want.union(c);
        }
        assert_eq!(mask, want, "{}", sec.name);
        assert!(budgets[i].playing.len() - budgets[i].rested().len() <= budgets[i].cap);
        start += sec.steps;
    }
}

/// A rested part is silent (< −80 dB) where it rests and plays where it doesn't.
#[test]
fn rested_solo_is_silent() {
    let sr = 24_000.0;
    let step = (0.125 * sr) as usize;
    let p = layered();
    let rests = build_rests(&p, Fixes::LAYER_BUDGET, None);
    let melody = render(
        &MixBalance::solo(&p, "lead"),
        sr,
        Some(rests.clone()),
        32.0,
        0,
    );
    let arp = render(
        &MixBalance::solo(&p, "arp"),
        sr,
        Some(rests.clone()),
        32.0,
        0,
    );
    let bass = render(&MixBalance::solo(&p, "bass"), sr, Some(rests), 32.0, 0);
    let intro = (step * 4, step * 128);
    let drop = (step * 132, step * 256);
    assert!(rms_db(&melody, intro.0, intro.1) < -80.0);
    assert!(rms_db(&melody, drop.0, drop.1) > -60.0);
    assert!(rms_db(&arp, intro.0, intro.1) < -80.0 && rms_db(&arp, drop.0, drop.1) < -80.0);
    assert!(rms_db(&bass, intro.0, intro.1) > -60.0 && rms_db(&bass, drop.0, drop.1) > -60.0);
    // without the flag the arp plays the drop
    let free = render(&MixBalance::solo(&p, "arp"), sr, None, 32.0, 0);
    assert!(rms_db(&free, drop.0, drop.1) > -60.0);
}

// MARK: band stop

fn lead(steps: &[i64]) -> NetBundle {
    let n: Vec<(i64, i64)> = steps.iter().map(|s| (*s, 72)).collect();
    ring("melody", 1, &n, 100)
}

fn verse_and_chorus(band: bool, melody: Option<NetBundle>) -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    if band {
        p.nets.insert(
            "kick",
            ring("drums", 10, &[0, 4, 8, 12].map(|s| (s, 36)), 50),
        );
        p.nets.insert(
            "bass",
            ring("bass", 6, &[2, 6, 10, 14].map(|s| (s, 33)), 100),
        );
        p.nets.insert(
            "harmony",
            ring("harmony", 7, &[0, 4, 8, 12].map(|s| (s, 48)), 150),
        );
        p.nets
            .insert("fill-1", ring("drums", 10, &[13, 15].map(|s| (s, 38)), 50));
    }
    if let Some(m) = melody {
        p.nets.insert("melody", m);
    }
    p.structure = vec![section("verse", 64), section("chorus", 64)];
    p
}

/// A stop is the last beat before each chorus, except the song's first section and a chorus
/// after a buildup / pre-chorus (the drop gap's).
#[test]
fn stops_sit_before_choruses() {
    let mut p = Project::default();
    p.nets.insert("melody", lead(&[12, 13, 14, 15]));
    p.structure = vec![
        section("chorus", 64),
        section("verse", 64),
        section("chorus", 64),
        section("pre-chorus", 64),
        section("chorus", 64),
        section("bridge", 32),
        section("chorus", 64),
    ];
    assert_eq!(band_stops(&p), vec![124, 348]);
    assert_eq!(drop_gaps(&p), vec![252]);
}

/// No lead note in the beat, no stop.
#[test]
fn silent_lead_skips_the_stop() {
    assert!(band_stops(&verse_and_chorus(true, Some(lead(&[0, 4])))).is_empty());
    assert!(band_stops(&verse_and_chorus(true, None)).is_empty());
    let mut muted = verse_and_chorus(true, Some(lead(&[12, 13, 14, 15])));
    muted.initial_mutes = vec!["melody".into()];
    assert!(band_stops(&muted).is_empty(), "a muted lead plays nothing");
    assert_eq!(
        band_stops(&verse_and_chorus(true, Some(lead(&[12, 13, 14, 15])))),
        vec![60]
    );
}

/// Only the song family stops; an unknown genre (raw nets, parity) does not.
#[test]
fn song_family_only() {
    let p = verse_and_chorus(true, Some(lead(&[12, 13, 14, 15])));
    for g in ["country", "blues", "funk", "reggae", "synthwave", "metal"] {
        assert!(band_stop_genre(g), "{g}");
        assert_eq!(
            build_rests(&p, Fixes::BAND_STOP, Some(g)).entries,
            vec![Entry::new(60, 64, BAND_STOP_MASK)]
        );
    }
    for g in [
        "edm", "techno", "house", "trance", "dnb", "jazz", "bossa", "lofi", "ambient",
    ] {
        assert!(!band_stop_genre(g), "{g}");
        assert!(build_rests(&p, Fixes::BAND_STOP, Some(g)).is_empty());
    }
    assert!(build_rests(&p, Fixes::BAND_STOP, None).is_empty());
    assert!(BAND_STOP_MASK.is_disjoint(Class::MELODY));
}

/// The dance genres are unchanged with `.bandStop` on.
#[test]
fn edm_unchanged() {
    for structure in ["standard", "extended", "ab"] {
        let edm = TrackSpec::new("edm", 42.0, Some(structure)).project();
        assert_eq!(
            build_rests(&edm, Fixes::ALL, Some("edm")),
            build_rests(&edm, Fixes::ALL & !Fixes::BAND_STOP, Some("edm"))
        );
    }
}

/// In the stop the band is ≥ 15 dB under the beat before, the lead plays on as it would
/// alone, and the chorus's downbeat comes back; the bars before are untouched.
#[test]
fn band_rests_lead_plays() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let p = verse_and_chorus(true, Some(lead(&[12, 13, 14, 15])));
    let rests = build_rests(&p, Fixes::BAND_STOP, Some("funk"));
    let plain = render(&p, sr, None, 10.0, 0);
    let stopped = render(&p, sr, Some(rests.clone()), 10.0, 0);
    let alone = render(
        &verse_and_chorus(false, Some(lead(&[12, 13, 14, 15]))),
        sr,
        Some(rests),
        10.0,
        0,
    );
    let stop = 60 * step;
    let (wa, wb) = (stop + step / 4, stop + 4 * step);
    let before = rms_db(&stopped, stop - 4 * step, stop);
    let band: Vec<f32> = (wa..wb).map(|i| stopped[i] - alone[i]).collect();
    let rest = rms_db(&band, 0, band.len());
    assert!(
        rest < before - 15.0,
        "band in the stop {rest} dB against the beat before at {before} dB"
    );
    let lead_db = rms_db(&alone, wa, wb);
    assert!(lead_db > -60.0, "the lead plays in the stop ({lead_db} dB)");
    assert!((rms_db(&stopped, wa, wb) - lead_db).abs() < 1.0);
    assert!(rms_db(&plain, wa, wb) > before - 3.0);
    let chorus = 64 * step;
    let downbeat = rms_db(&stopped, chorus, chorus + step);
    let earlier = rms_db(&stopped, 56 * step, 57 * step);
    assert!(
        (downbeat - earlier).abs() < 1.0,
        "chorus downbeat {downbeat} dB, bar 4's {earlier} dB"
    );
    assert!(plain[..stop] == stopped[..stop]);
}

// MARK: kick out

fn buildup_project(kick: bool, snare: bool, bass: bool) -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    if kick {
        p.nets.insert(
            "kick",
            ring("drums", 10, &[0, 4, 8, 12].map(|s| (s, 36)), 50),
        );
    }
    if snare {
        let n: Vec<(i64, i64)> = (0..16).step_by(2).map(|s| (s, 38)).collect();
        p.nets.insert("snare", ring("drums", 10, &n, 40));
    }
    if bass {
        p.nets.insert(
            "bass",
            ring("bass", 6, &[2, 6, 10, 14].map(|s| (s, 33)), 100),
        );
    }
    p.structure = vec![section("buildup", 64), section("drop", 64)];
    p
}

/// Each buildup of two bars or more into a drop: the kick, for its last bar.
#[test]
fn rests_the_last_bar_of_each_buildup_into_a_drop() {
    let rests = |s: &[(&str, i64)]| -> Vec<Entry> {
        let mut p = Project::default();
        p.structure = s.iter().map(|(n, st)| section(n, *st)).collect();
        kick_out_rests(&p)
    };
    assert_eq!(
        rests(&[
            ("intro", 64),
            ("buildup", 64),
            ("drop", 128),
            ("buildup", 32),
            ("drop", 64)
        ]),
        vec![
            Entry::new(112, 128, Class::KICK),
            Entry::new(272, 288, Class::KICK)
        ]
    );
    assert!(rests(&[("buildup", 64), ("chorus", 64)]).is_empty());
    assert!(rests(&[("buildup", 16), ("drop", 64)]).is_empty());
    assert!(rests(&[("intro", 64), ("buildup", 64)]).is_empty());
    assert!(rests(&[("pre-chorus", 64), ("drop", 64)]).is_empty());

    let edm = TrackSpec::new("edm", 42.0, Some("extended")).project();
    let built = kick_out_rests(&edm);
    assert!(!built.is_empty());
    assert!(built
        .iter()
        .all(|e| e.to - e.from == KICK_OUT_STEPS && e.mask == Class::KICK));
    assert_eq!(
        build_rests(&edm, Fixes::KICK_OUT, None),
        RestSchedule::merging(&built)
    );
}

/// Genres outside the dance family get no kick out, whatever their structure.
#[test]
fn non_dance_genres_are_unchanged() {
    let genres: Vec<&String> = BeatsTables::shared()
        .genres
        .keys()
        .filter(|g| BeatsTables::shared().family_of(g) != 0)
        .collect();
    assert!(!genres.is_empty());
    for g in genres {
        for structure in [
            "minimal", "standard", "extended", "build", "drop", "ab", "jam",
        ] {
            let p = TrackSpec::new(g, 42.0, Some(structure)).project();
            assert!(kick_out_rests(&p).is_empty(), "{g} {structure}");
            assert_eq!(
                build_rests(&p, Fixes::ALL, None),
                build_rests(&p, Fixes::ALL & !Fixes::KICK_OUT, None),
                "{g} {structure}"
            );
        }
    }
}

/// Solo, the kick is silent for the last bar of the buildup and back on the drop.
#[test]
fn kick_is_out_for_the_last_buildup_bar() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let kick_only = buildup_project(true, false, false);
    let rests = build_rests(&kick_only, Fixes::KICK_OUT, None);
    let out = render(&kick_only, sr, Some(rests), 10.0, 0);
    let plain = render(&kick_only, sr, None, 10.0, 0);
    assert!(out[..48 * step] == plain[..48 * step]);
    let last = rms_db(&out, 48 * step + step / 2, 64 * step);
    assert!(last < -60.0, "kick in the last buildup bar: {last} dB");
    assert!(rms_db(&plain, 48 * step + step / 2, 64 * step) > -40.0);
    let drop = rms_db(&out, 64 * step, 68 * step);
    assert!(
        (drop - rms_db(&plain, 64 * step, 68 * step)).abs() < 0.5,
        "drop: {drop} dB"
    );
}

/// The snare roll plays through.
#[test]
fn snare_roll_plays_on() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let snare = buildup_project(false, true, false);
    let rests = build_rests(&snare, Fixes::KICK_OUT, None);
    assert!(render(&snare, sr, Some(rests), 10.0, 0) == render(&snare, sr, None, 10.0, 0));
    let full = buildup_project(true, true, true);
    let mix = render(
        &full,
        sr,
        Some(build_rests(&full, Fixes::KICK_OUT, None)),
        10.0,
        0,
    );
    let no_kick = render(&buildup_project(false, true, true), sr, None, 10.0, 0);
    let (a, b) = (48 * step + step / 2, 64 * step);
    assert!((rms_db(&mix, a, b) - rms_db(&no_kick, a, b)).abs() < 0.5);
    assert!(rms_db(&no_kick, a, b) > -40.0);
}

/// `.duck` follows the hits that play (KickOutTests.duckFollowsTheKickOut): with the kick out the
/// bass stops dipping, so in the last buildup bar the ducked mix is the unducked one.
#[test]
fn duck_follows_the_kick_out() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let p = buildup_project(true, false, true);
    let rests = build_rests(&p, Fixes::KICK_OUT, None);
    let render_duck = |depth: f64| -> Vec<f32> {
        let o = WaveOptions {
            master: Master::Linear,
            rests: Some(rests.clone()),
            duck_depth: depth,
            ..Default::default()
        };
        let mut r = WaveRunner::new(sr, o.clone());
        r.load_prepared(WaveRunner::prepare(&p, sr, &o, 0));
        r.play();
        let mut out = vec![0.0f32; (10.0 * sr) as usize];
        let mut off = 0;
        while off < out.len() {
            let n = 256.min(out.len() - off);
            r.process(&mut out[off..off + n], None, n);
            off += n;
        }
        out
    };
    let ducked = render_duck(0.5);
    let flat = render_duck(0.0);
    // a bar earlier the kick still pumps the bass
    assert!(ducked[32 * step..48 * step] != flat[32 * step..48 * step]);
    // past the last kick's release (step 44: e^-9 of the dip left by step 50)
    let (a, b) = (50 * step, 64 * step);
    assert!((rms_db(&ducked, a, b) - rms_db(&flat, a, b)).abs() < 0.05);
    assert!(rms_db(&flat, a, b) > -40.0);
}

// MARK: drop gap

fn buildup_and_drop(rhythm: bool, pad: bool) -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    if rhythm {
        p.nets.insert(
            "kick",
            ring("drums", 10, &[0, 4, 8, 12].map(|s| (s, 36)), 50),
        );
        p.nets.insert(
            "bass",
            ring("bass", 6, &[2, 6, 10, 14].map(|s| (s, 33)), 100),
        );
    }
    if pad {
        p.nets.insert(
            "harmony",
            ring("harmony", 7, &[(0, 60), (1, 64), (2, 67)], 1950),
        );
    }
    p.structure = vec![section("buildup", 64), section("drop", 64)];
    p
}

/// The gap is the last beat of each buildup that lands on a drop (or a pre-chorus on a
/// chorus), nowhere else.
#[test]
fn gaps_sit_before_drops() {
    let mut p = Project::default();
    p.structure = vec![
        section("intro", 64),
        section("buildup", 128),
        section("drop", 128),
        section("breakdown", 64),
        section("drop", 64),
        section("pre-chorus", 64),
        section("chorus", 64),
    ];
    assert_eq!(drop_gaps(&p), vec![188, 508]);
    let edm = TrackSpec::new("edm", 42.0, Some("standard")).project();
    let (mut start, mut drops) = (0, vec![]);
    for (k, s) in edm.structure.iter().enumerate() {
        if k > 0 && s.name == "drop" && edm.structure[k - 1].name == "buildup" {
            drops.push(start);
        }
        start += s.steps;
    }
    assert!(!drops.is_empty());
    assert_eq!(
        drop_gaps(&edm),
        drops.iter().map(|d| d - DROP_GAP_STEPS).collect::<Vec<_>>()
    );
}

/// In the gap the kick and bass stop; the drop's downbeat plays as loud as a downbeat before
/// the gap; bars before are untouched.
#[test]
fn rhythm_section_rests_for_the_last_beat() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let p = buildup_and_drop(true, false);
    let plain = render(&p, sr, None, 10.0, 0);
    let gapped = render(
        &p,
        sr,
        Some(build_rests(&p, Fixes::DROP_GAP, None)),
        10.0,
        0,
    );
    let gap = 60 * step;
    let before = rms_db(&gapped, gap - 4 * step, gap);
    let in_gap = rms_db(&gapped, gap + step / 4, gap + 4 * step);
    assert!(
        in_gap < before - 20.0,
        "gap {in_gap} dB against the beat before at {before} dB"
    );
    assert!(rms_db(&plain, gap + step / 4, gap + 4 * step) > before - 3.0);
    let drop = 64 * step;
    let downbeat = rms_db(&gapped, drop, drop + step);
    let earlier = rms_db(&gapped, 56 * step, 57 * step);
    assert!(
        (downbeat - earlier).abs() < 1.0,
        "drop downbeat {downbeat} dB, bar 4's {earlier} dB"
    );
    assert!(plain[..gap] == gapped[..gap]);
}

/// Pads play through the gap.
#[test]
fn pads_play_through() {
    let sr = 48_000.0;
    let p = buildup_and_drop(false, true);
    assert!(
        render(&p, sr, None, 10.0, 0)
            == render(
                &p,
                sr,
                Some(build_rests(&p, Fixes::DROP_GAP, None)),
                10.0,
                0
            )
    );
}

// MARK: riser

fn riser_project(sections: Option<Vec<Section>>) -> Project {
    let mut p = Project::default();
    p.tempo = 120.0;
    p.nets.insert(
        "kick",
        ring("drums", 10, &[0, 4, 8, 12].map(|s| (s, 36)), 50),
    );
    p.nets.insert(
        "bass",
        ring("bass", 6, &[2, 6, 10, 14].map(|s| (s, 33)), 100),
    );
    p.structure = sections.unwrap_or_else(|| vec![section("buildup", 64), section("drop", 64)]);
    p
}

#[test]
fn riser_flag_is_registered() {
    assert!(Fixes::ALL.contains(Fixes::RISER));
    assert_eq!(Fixes::named("riser"), Some(Fixes::RISER));
}

/// The last two bars of each buildup that lands on a drop or chorus; a short buildup whole.
#[test]
fn windows_are_the_buildups_last_two_bars() {
    assert_eq!(riser_windows(&riser_project(None)), vec![(32, 64)]);
    let short = riser_project(Some(vec![
        section("intro", 32),
        section("buildup", 16),
        section("chorus", 64),
        section("buildup", 64),
        section("breakdown", 32),
        section("verse", 32),
        section("drop", 32),
    ]));
    assert_eq!(riser_windows(&short), vec![(32, 48)]);
    assert!(riser_windows(&Project::default()).is_empty());
    let edm = TrackSpec::new("edm", 42.0, Some("extended")).project();
    assert!(!riser_windows(&edm).is_empty());
}

/// The sweep's ends are the spec's: 300 Hz at −30 dB to 8 kHz at −12 dB.
#[test]
fn sweep_ends() {
    let a = RiserSpec::at(0.0);
    let b = RiserSpec::at(1.0);
    assert!((a.0 - 300.0).abs() < 1e-9 && (a.1 + 30.0).abs() < 1e-9);
    assert!((b.0 - 8000.0).abs() < 1e-6 && (b.1 + 12.0).abs() < 1e-9);
    let m = RiserSpec::at(0.5).0;
    assert!(m > 1500.0 && m < 1600.0);
}

/// Spearman's ρ: 1 rising, −1 falling, ties by their mean rank.
#[test]
fn spearman() {
    assert!((RiserRise::spearman_of(&[1.0, 2.0, 3.0, 4.0]) - 1.0).abs() < 1e-12);
    assert!((RiserRise::spearman_of(&[4.0, 3.0, 2.0, 1.0]) + 1.0).abs() < 1e-12);
    assert!(RiserRise::spearman_of(&[1.0, 3.0, 2.0, 4.0]) > 0.7);
    assert_eq!(RiserRise::spearman_of(&[2.0, 2.0]), 0.0);
}

/// The riser voice alone, over a hand-run window (the runner's renders of it are in
/// `wave_riser.rs`):
/// silent before the window, deterministic, high band rising by > 9 dB beat after beat,
/// ending near −12 dB, cut at the drop.
#[test]
fn voice_climbs_cuts_and_is_deterministic() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let p = riser_project(None);
    let windows = riser_windows(&p);
    let run = || -> (Vec<f32>, Vec<f32>) {
        let mut v = RiserVoice::new(&windows, sr, true, false).unwrap();
        let (mut l, mut r) = (vec![], vec![]);
        for i in 0..(10.0 * sr) as usize {
            let pos = i as f64 / step as f64;
            if v.render(Some(pos)) {
                l.push((v.mid + v.side) as f32);
                r.push((v.mid - v.side) as f32);
            } else {
                l.push(0.0);
                r.push(0.0);
            }
        }
        (l, r)
    };
    let (l, r) = run();
    let (l2, r2) = run();
    assert!(l == l2 && r == r2, "deterministic");
    assert!(
        l[..32 * step].iter().all(|x| *x == 0.0),
        "silent before its window"
    );
    let last = rms_db(&l, 64 * step - 4 * step, 64 * step);
    assert!((last - RiserSpec::TO_DB).abs() < 3.5, "last beat {last} dB");
    let first = rms_db(&l, 32 * step + step / 2, 36 * step);
    assert!(first < last - 9.0, "first beat {first}");
    assert!(
        rms_db(&l, 64 * step + (0.03 * sr) as usize, 72 * step) < -100.0,
        "cut at the drop"
    );
    let rises = RiserRise::measure(&l, &r, sr, &p);
    assert_eq!(rises.len(), 1);
    assert!(rises[0].rise_db() >= 9.0, "rise {} dB", rises[0].rise_db());
    assert!(rises[0].spearman() >= 0.9, "ρ {}", rises[0].spearman());
    // the side widens through the window (stereo)
    let side: Vec<f32> = l.iter().zip(&r).map(|(a, b)| (a - b) / 2.0).collect();
    let early = rms_db(&side, 32 * step + step / 2, 40 * step);
    let late = rms_db(&side, 56 * step, 64 * step);
    assert!(
        early > -80.0 && late > early + 9.0,
        "side {early} → {late} dB"
    );
}

/// A start inside the window (a section jump) plays the riser where the song has it: the
/// voice is keyed by song position, not by when it was started.
#[test]
fn a_jump_starts_in_phase() {
    let sr = 48_000.0;
    let step = (0.125 * sr) as usize;
    let windows = vec![(32i64, 64i64)];
    let level = |from_step: f64, a: f64, b: f64| -> f64 {
        let mut v = RiserVoice::new(&windows, sr, false, false).unwrap();
        let (mut sum, mut n) = (0.0f64, 0usize);
        for i in 0..(4.0 * sr) as usize {
            let pos = from_step + i as f64 / step as f64;
            if v.render(Some(pos)) {
                let t = i as f64 / step as f64;
                if t >= a && t < b {
                    sum += v.mid * v.mid;
                    n += 1;
                }
            }
        }
        10.0 * (sum / n.max(1) as f64).max(1e-20).log10()
    };
    for beat in 0..4 {
        let (a, b) = (beat as f64 * 4.0 + 0.5, (beat + 1) as f64 * 4.0);
        let full = level(32.0, 16.0 + a, 16.0 + b);
        let late = level(48.0, a, b);
        assert!((full - late).abs() < 1.5, "beat {beat}: {full} vs {late}");
    }
    // a start on the drop plays none of it
    let mut v = RiserVoice::new(&windows, sr, false, false).unwrap();
    assert!((0..sr as usize).all(|i| !v.render(Some(64.0 + i as f64 / step as f64))));
}

// MARK: layers per section, groove report

/// `djctl render --layers`'s count: four parts a section without the flag, the budget with it.
#[test]
fn layers_per_section_counts_the_cap() {
    use djrust::musicality::audio_metrics::layers_per_section;
    let sr = 24_000.0;
    let p = layered();
    let mut o = WaveOptions::default();
    let without = layers_per_section(&p, &o, sr, None);
    assert_eq!(
        without.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["intro", "drop"]
    );
    assert_eq!(
        without.iter().map(|s| s.layers()).collect::<Vec<_>>(),
        vec![4, 4]
    );
    o.rests = Some(build_rests(&p, Fixes::LAYER_BUDGET, None));
    let with = layers_per_section(&p, &o, sr, None);
    assert_eq!(
        with.iter().map(|s| s.layers()).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(with[0].parts, vec!["bass"]);
    assert_eq!(with[1].parts, vec!["bass", "pad", "lead"]);
    assert!(with[1].levels_db["arp"] <= -150.0);
    let short = layers_per_section(&p, &o, sr, Some(20.0));
    assert_eq!(
        short.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["intro"]
    );
}
