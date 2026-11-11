//! The drums the runner voices from the notes dj's pass writes: the fill nets (`.fills`,
//! `.buildup`), the section crash (`.crash`), the open hats (`.openHats`), the closed hat that
//! chokes an open one (`.choke`), the lowpass on the noise drums (`.hfTilt`) and the brushes on
//! jazz's snare (`WaveOptions::brushes`). Swift's `MusicalityTests` (the deck halves of the fill,
//! buildup, crash and open-hat tests), `DropGapTests.closedHatChokesTheOpenHat`, `BrushTests` and
//! `SpectralSlopeTests.HFTiltTests` ported (PORTING.md's proof, form (a)); the same drums held
//! against Swift's output sample for sample are the "drums" cases of `tests/runner_golden.rs`, and
//! the allocation counters are in `crates/djhost/tests/deck_rt.rs`.

mod common;
use common::measure::*;
use djrust::musicality::apply;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::spectrum::{Spectrum, BANDS_HZ, BUMP_BANDS};
use djrust::musicality::synth_voice::{Brush, HfTilt};
use djrust::pflow::{NetBundle, Project};
use djrust::wave::synth::drum_voice;
use djrust::wave::*;
use djrust::TrackSpec;

/// `WaveOptions.extended`: the instrument table and every fix (the runner of the app, with no
/// genre).
fn extended() -> WaveOptions {
    WaveOptions {
        musicality: Fixes::ALL,
        ..WaveOptions::extended()
    }
}

/// `MusicalityTests.played`: a genre's seed-42 song as a deck plays it.
fn played(genre: &str, seed: f64) -> Project {
    let spec = TrackSpec::new(genre, seed, Some("standard"));
    apply(&spec.project(), &spec, Fixes::ALL)
}

fn lanes_where<'a>(p: &'a WaveProject, f: impl Fn(&Lane) -> bool + 'a) -> Vec<&'a Lane> {
    p.lanes().iter().filter(|l| f(l)).collect()
}

// ---- the fills, the crash and the open hats ----------------------------------------------

/// MusicalityTests.drumFillsBeforeSectionChanges (the deck's half): beats builds the fills,
/// mutes them and gives them no voice; the deck voices them.
#[test]
fn the_deck_voices_the_drum_fills() {
    for genre in ["techno", "edm", "jazz", "lofi"] {
        let p = played(genre, 42.0);
        let deck = WaveRunner::prepare(&p, 22_050.0, &extended(), 0);
        assert!(!deck.lanes_of("fill-0").is_empty(), "{genre}");
        let parity = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::default(), 0);
        assert!(parity.lanes_of("fill-0").is_empty(), "{genre}");
    }
}

/// MusicalityTests.buildupsRollIntoTheDrop (the deck's half): the deck voices the roll.
#[test]
fn the_deck_voices_the_buildup_roll() {
    let deck = WaveRunner::prepare(&played("edm", 42.0), 22_050.0, &extended(), 0);
    assert!(deck.lanes().iter().any(|l| l.net_id.starts_with("fill-")));
}

/// Each of `.fills`, `.buildup` and `.crash` alone voices the fill nets, and no other flag does.
#[test]
fn any_of_the_three_flags_voices_the_fills() {
    let p = played("edm", 42.0);
    for fix in [Fixes::FILLS, Fixes::BUILDUP, Fixes::CRASH] {
        let o = WaveOptions {
            musicality: fix,
            ..Default::default()
        };
        let deck = WaveRunner::prepare(&p, 22_050.0, &o, 0);
        assert!(!deck.lanes_of("fill-0").is_empty(), "{fix:?}");
    }
    for fix in [
        Fixes::OPEN_HATS,
        Fixes::CHOKE,
        Fixes::HF_TILT,
        Fixes::BACKBEAT,
    ] {
        let o = WaveOptions {
            musicality: fix,
            ..Default::default()
        };
        let deck = WaveRunner::prepare(&p, 22_050.0, &o, 0);
        assert!(deck.lanes_of("fill-0").is_empty(), "{fix:?}");
    }
}

/// MusicalityTests.sectionsStartWithACrash (the deck's half): a crash voice (a long bright tail,
/// not beats' 0.1 s open hat) on the fill nets; the parity path has none.
#[test]
fn the_deck_gives_the_section_crash_a_voice() {
    for genre in ["techno", "edm", "jazz", "lofi", "funk"] {
        let p = played(genre, 42.0);
        let deck = WaveRunner::prepare(&p, 22_050.0, &extended(), 0);
        let crash = lanes_where(&deck, |l| l.voice.kind == "crash");
        assert!(!crash.is_empty(), "{genre}");
        assert!(
            crash.iter().all(|l| l.net_id.starts_with("fill-")),
            "{genre}"
        );
        assert!(
            crash
                .iter()
                .all(|l| l.voice.pulse.tau.iter().cloned().fold(0.0, f64::max) > 0.5),
            "{genre}"
        );
        let parity = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::default(), 0);
        assert!(
            !parity.lanes().iter().any(|l| l.voice.kind == "crash"),
            "{genre}"
        );
    }
}

/// The crash is the section crash only (note 49 on a fill net): the open hats a fill plays
/// (note 46) and the notes of the hat net keep their own kinds.
#[test]
fn only_note_49_on_a_fill_net_is_a_crash() {
    let p = played("edm", 42.0);
    let deck = WaveRunner::prepare(&p, 22_050.0, &extended(), 0);
    for l in deck.lanes() {
        if l.voice.kind == "crash" {
            assert!(l.net_id.starts_with("fill-"));
            assert_eq!(l.kind, "crash");
        }
        if !l.net_id.starts_with("fill-") {
            assert_ne!(l.kind, "crash", "{}", l.net_id);
        }
    }
    // without `.crash` the same notes are beats' open hat
    let o = WaveOptions {
        musicality: Fixes::FILLS | Fixes::BUILDUP | Fixes::OPEN_HATS,
        ..Default::default()
    };
    let fills = WaveRunner::prepare(&p, 22_050.0, &o, 0);
    assert!(!fills.lanes().iter().any(|l| l.kind == "crash"));
    assert!(fills
        .lanes()
        .iter()
        .any(|l| l.kind == "openhat" && l.net_id.starts_with("fill-")));
}

/// MusicalityTests.offbeatHatsOpen (the deck's half): the deck gives the off-beat hats an
/// open-hat voice; `drumKind(46)` stays beats' closed hat.
#[test]
fn the_deck_gives_the_off_beat_hats_an_open_voice() {
    for genre in ["techno", "house", "edm", "trance"] {
        let p = played(genre, 42.0);
        let deck = WaveRunner::prepare(&p, 22_050.0, &extended(), 0);
        assert!(
            deck.lanes()
                .iter()
                .any(|l| l.voice.kind == "openhat" && l.net_id.starts_with("hihat")),
            "{genre}"
        );
        let parity = WaveRunner::prepare(&p, 22_050.0, &WaveOptions::default(), 0);
        assert!(
            !parity.lanes().iter().any(|l| l.voice.kind == "openhat"),
            "{genre}: beats voices 46 as a closed hat"
        );
    }
    assert_eq!(djrust::wave::drum_kind(46), "hat");
}

// ---- the choke ---------------------------------------------------------------------------

const SR: f64 = 48_000.0;
/// 120 BPM: a step is 0.125 s.
const STEP: usize = (0.125 * SR) as usize;

fn ring16(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(16);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

fn linear(fixes: Fixes) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        ..Default::default()
    }
}

fn rms(x: &[f32], a: usize, b: usize) -> f64 {
    rms_db(&x[a..b])
}

/// DropGapTests.closedHatChokesTheOpenHat: an open hat followed by a closed hat in the same net:
/// choked, the open hat's tail is gone 20 ms after the closed hat; before it, nothing changes.
/// The closed hat is played softly so what is measured after it is the open hat.
#[test]
fn a_closed_hat_chokes_the_open_hat() {
    let mut hats = NetBundle::ring(16);
    hats.bind(2, 46, 10, 110, 50);
    hats.bind(3, 42, 10, 1, 50);
    hats.track.channel = 10;
    hats.track.group = Some("drums".into());
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    p.nets.insert("hihat", hats);
    let ringing = render_mono(&p, &linear(Fixes::OPEN_HATS), SR, 1.0);
    let choked = render_mono(&p, &linear(Fixes::OPEN_HATS | Fixes::CHOKE), SR, 1.0);
    let closed = 3 * STEP + 1;
    assert!(identical(&ringing[..closed], &choked[..closed]));
    let a = closed + (0.02 * SR) as usize;
    let b = closed + (0.1 * SR) as usize;
    let tail = rms(&ringing, a, b);
    let cut = rms(&choked, a, b);
    assert!(
        cut < tail - 20.0,
        "open hat tail {tail} dB, choked {cut} dB"
    );
    // a net without an open hat is untouched
    let mut closed_only = NetBundle::ring(16);
    for s in [0, 2, 4] {
        closed_only.bind(s, 42, 10, 100, 50);
    }
    closed_only.track.channel = 10;
    let mut q = Project {
        tempo: 120.0,
        ..Default::default()
    };
    q.nets.insert("hihat", closed_only);
    assert!(identical(
        &render_mono(&q, &linear(Fixes::OPEN_HATS), SR, 1.0),
        &render_mono(&q, &linear(Fixes::OPEN_HATS | Fixes::CHOKE), SR, 1.0)
    ));
}

/// The choke joins a net's own two lanes, and only with the flag: the closed hat points at the
/// open one, which dies by a 10 ms time constant while it is choked.
#[test]
fn the_choke_joins_a_nets_hat_and_open_hat() {
    let p = {
        let mut p = Project {
            tempo: 120.0,
            ..Default::default()
        };
        p.nets.insert(
            "hihat",
            ring16("drums", 10, &[(0, 42), (2, 46), (4, 42), (6, 46)], 50),
        );
        p
    };
    let o = linear(Fixes::OPEN_HATS | Fixes::CHOKE);
    let deck = WaveRunner::prepare(&p, 48_000.0, &o, 0);
    let lanes = deck.lanes();
    let hat = lanes.iter().position(|l| l.kind == "hat").unwrap();
    let open = lanes.iter().position(|l| l.kind == "openhat").unwrap();
    assert_eq!(lanes[hat].chokes(), Some(open));
    assert_eq!(lanes[open].chokes(), None);
    assert_eq!(lanes[open].choke_mul(), libm::exp(-1.0 / (0.01 * 48_000.0)));
    assert_eq!(lanes[hat].choke_mul(), 1.0);
    let plain = WaveRunner::prepare(&p, 48_000.0, &linear(Fixes::OPEN_HATS), 0);
    assert!(plain
        .lanes()
        .iter()
        .all(|l| l.chokes().is_none() && l.choke_mul() == 1.0));
}

// ---- the tilt ----------------------------------------------------------------------------

/// SpectralSlopeTests.HFTiltTests.setByGenreOnlyUnderTheFix: the fix is in the registry,
/// `forGenre` sets the family's cutoff only with it on, and `WaveOptions()` (parity) never has
/// one.
#[test]
fn the_tilt_is_set_by_genre_only_under_the_fix() {
    assert!(Fixes::ALL.contains(Fixes::HF_TILT));
    assert_eq!(Fixes::named("hfTilt"), Some(Fixes::HF_TILT));
    assert!(WaveOptions::default().hf_tilt.is_none());
    assert!(WaveOptions::default().for_genre("techno").hf_tilt.is_none());
    let off = WaveOptions {
        musicality: Fixes::ALL & !Fixes::HF_TILT,
        ..WaveOptions::extended()
    };
    assert!(off.for_genre("techno").hf_tilt.is_none());
    use djrust::musicality::mix_targets::MixTargets;
    let family = |genre: &str| {
        let names = djrust::wave::groove::FAMILY_NAMES;
        let i = djrust::gen::tables::BeatsTables::shared()
            .family_of(genre)
            .max(0)
            .min(names.len() as i64 - 1);
        names[i as usize]
    };
    for genre in [
        "techno",
        "house",
        "edm",
        "trance",
        "dnb",
        "dubstep",
        "synthwave",
        "lofi",
        "jazz",
        "funk",
        "bossa",
        "ambient",
    ] {
        let tilt = extended().for_genre(genre).hf_tilt;
        let hz = MixTargets::shared().hf_tilt_hz(family(genre));
        assert!(tilt.is_some() && tilt.map(|t| t.hat_hz) == hz, "{genre}");
        let t = tilt.unwrap();
        assert!(t.hat_hz > 6000.0 && t.hat_hz < 16000.0, "{genre}");
    }
    let t = HfTilt::new(9000.0, None);
    assert_eq!(t.lowpass_hz("hat"), Some(9000.0));
    assert_eq!(t.lowpass_hz("openhat"), Some(9000.0));
    assert_eq!(t.lowpass_hz("crash"), Some(9000.0));
    for kind in ["kick", "snare", "clap", "tonal"] {
        assert_eq!(t.lowpass_hz(kind), None);
    }
}

/// The tilt goes on the hat, open hat and crash lanes (noise voices) under the fix and the
/// deck's tilt, at the cutoff the tilt names (at most 0.45 sr), and on nothing else.
#[test]
fn the_tilt_lowpasses_the_noise_drums_only() {
    let p = played("techno", 42.0);
    let sr = 48_000.0;
    let o = extended().for_genre("techno");
    let tilt = o.hf_tilt.unwrap();
    let deck = WaveRunner::prepare(&p, sr, &o, 0);
    let mut tilted = 0;
    for l in deck.lanes() {
        let expect = tilt.lowpass_hz(&l.kind).is_some() && l.voice.carrier == "noise";
        assert_eq!(l.tilt_on(), expect, "{} {}", l.net_id, l.kind);
        if l.tilt_on() {
            tilted += 1;
            let want = djrust::wave::synth::biquad(
                "lowpass",
                tilt.lowpass_hz(&l.kind).unwrap().min(0.45 * sr),
                tilt.q,
                sr,
            );
            assert_eq!(l.tilt(), want);
        }
    }
    assert!(tilted > 0);
    // no fix, no tilt: even with the deck's tilt set
    let mut off = o.clone();
    off.musicality = Fixes::ALL & !Fixes::HF_TILT;
    let untilted = WaveRunner::prepare(&p, sr, &off, 0);
    assert!(untilted.lanes().iter().all(|l| !l.tilt_on()));
    // and with the fix but no tilt chosen (parity genre unknown): none either
    let mut none = o.clone();
    none.hf_tilt = None;
    assert!(WaveRunner::prepare(&p, sr, &none, 0)
        .lanes()
        .iter()
        .all(|l| !l.tilt_on()));
}

fn hf_power(s: &Spectrum) -> f64 {
    BANDS_HZ
        .iter()
        .enumerate()
        .filter(|(_, hz)| BUMP_BANDS.contains(hz))
        .map(|(i, _)| libm::pow(10.0, s.ltas_db[i] / 10.0))
        .sum()
}

/// SpectralSlopeTests.HFTiltTests.lowersTheHatsTop: the soloed hats' 12.5 / 16 kHz power falls
/// with the fix on; the rest of their spectrum (their 7-10 kHz sizzle) stays.
#[test]
fn the_tilt_lowers_the_hats_top() {
    let sr = 48_000.0;
    let spec = TrackSpec::new("techno", 42.0, Some("standard"));
    let p = apply(&spec.project(), &spec, Fixes::ALL);
    let render = |without: Fixes| {
        let mut o = WaveOptions {
            musicality: Fixes::ALL & !Fixes::LOUDNESS & !without,
            master: Master::Linear,
            voicing_seed: 42.0,
            role_filter: Some(RoleFilter {
                mute: vec![],
                solo: Some(vec!["hihat".into()]),
            }),
            ..WaveOptions::extended()
        }
        .for_genre("techno");
        o.master = Master::Linear;
        let (l, r) = render_stereo(&p, &o, sr, 12.0);
        Spectrum::new(&l, &r, sr)
    };
    let on = render(Fixes::EMPTY);
    let off = render(Fixes::HF_TILT);
    assert!(on.ltas_db.iter().any(|d| *d > -150.0) && off.ltas_db.iter().any(|d| *d > -150.0));
    let (pon, poff) = (hf_power(&on), hf_power(&off));
    assert!(pon > 0.0 && poff > 0.0);
    let drop_db = 10.0 * libm::log10(poff / pon);
    eprintln!(
        "hfTilt techno hats: 12.5/16 kHz {drop_db:.1} dB down; bump {:.1} -> {:.1}",
        off.hf_bump_db, on.hf_bump_db
    );
    assert!(drop_db >= 4.0, "the top falls: {drop_db} dB");
    assert!(on.hf_bump_db < off.hf_bump_db);
}

// ---- the brushes -------------------------------------------------------------------------

/// BrushTests.snareHit: one snare hit (GM 38 on beats' snare channel) at step 0 of a 16-step
/// ring.
fn snare_hit() -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(16);
    n.bind(0, 38, 11, 100, 50);
    n.track.channel = 11;
    p.nets.insert("snare", n);
    p
}

fn brush_options(brushes: bool) -> WaveOptions {
    WaveOptions {
        master: Master::Linear,
        brushes,
        ..Default::default()
    }
}

/// RMS per `ms`-long window.
fn envelope(x: &[f32], ms: f64) -> Vec<f64> {
    let w = (ms / 1000.0 * SR) as usize;
    (0..x.len() - w)
        .step_by(w)
        .map(|a| {
            let s: f64 = x[a..a + w].iter().map(|v| *v as f64 * *v as f64).sum();
            libm::sqrt(s / w as f64)
        })
        .collect()
}

/// BrushTests.theBrushVoiceIsNoiseOnly: the brush is band-passed noise and nothing else: no tone
/// under it (beats' snare has a 180 Hz one), a slow rise and a long fall.
#[test]
fn the_brush_voice_is_noise_only() {
    let v = Brush::voice();
    assert_eq!(v.kind, "snare");
    assert_eq!(v.carrier, "noise");
    assert!(v.tone == 0.0 && v.tone_mix == 0.0);
    assert_eq!(v.filter, "bandpass");
    assert_eq!(v.pulse.alpha, vec![1.0, -1.0]);
    assert_eq!(v.pulse.tau, vec![Brush::DECAY, Brush::ATTACK]);
    let stick = drum_voice("snare", "drums");
    assert_eq!(stick.carrier, "noise+tone");
}

/// BrushTests.theBrushRisesAndSwishes: rise >= 15 ms (onset at -20 dB to the peak, 5 ms windows)
/// where the stick snare's is one window; the swish lasts 150-300 ms above -20 dB.
#[test]
fn the_brush_rises_and_swishes() {
    let shape = |brushes: bool| -> (f64, f64) {
        let x = render_mono(&snare_hit(), &brush_options(brushes), SR, 0.6);
        let env = envelope(&x, 5.0);
        let peak = env.iter().cloned().fold(0.0, f64::max);
        let at = env.iter().position(|v| *v == peak).unwrap();
        let onset = env.iter().position(|v| *v > peak * 0.1).unwrap();
        let end = env.iter().rposition(|v| *v > peak * 0.1).unwrap();
        ((at - onset) as f64 * 5.0, (end - onset + 1) as f64 * 5.0)
    };
    let brush = shape(true);
    let stick = shape(false);
    eprintln!(
        "brush rise {} ms, length {} ms; stick {stick:?}",
        brush.0, brush.1
    );
    assert!(brush.0 >= 15.0, "brush rise {} ms", brush.0);
    assert!(stick.0 <= 5.0, "stick rise {} ms", stick.0);
    assert!(
        brush.1 >= 150.0 && brush.1 <= 300.0,
        "brush length {} ms",
        brush.1
    );
}

/// BrushTests.theBrushHasNoBody: at the stick snare's 180 Hz tone (and below) the brush has
/// nothing to speak of, relative to its own level.
#[test]
fn the_brush_has_no_body() {
    let body = |brushes: bool| {
        let x = render_mono(&snare_hit(), &brush_options(brushes), SR, 0.3);
        tone_db(&x, 180.0, SR) - rms_db(&x)
    };
    let (brush, stick) = (body(true), body(false));
    eprintln!("180 Hz against the hit's RMS: brush {brush} dB, stick {stick} dB");
    assert!(stick - brush > 20.0, "brush {brush} dB vs stick {stick} dB");
}

/// BrushTests.onlyJazzUnderBackbeatPlaysBrushes: jazz plays brushes under `.backbeat`; no other
/// genre does, and the parity options never.
#[test]
fn only_jazz_under_backbeat_plays_brushes() {
    assert!(extended().for_genre("jazz").brushes);
    for g in ["lofi", "bossa", "techno", "house", "ambient", "dnb"] {
        assert!(!extended().for_genre(g).brushes, "{g}");
    }
    let mut o = extended();
    o.musicality = o.musicality & !Fixes::BACKBEAT;
    assert!(!o.for_genre("jazz").brushes);
    assert!(!WaveOptions::default().for_genre("jazz").brushes);
    assert!(!WaveOptions::default().brushes);
}

/// `SamplerInstrument.kitPhrase` as a net project at 120 BPM (one bar of 16ths, looped): kick,
/// snare and hats on beats' drum channels, velocity 100 (SampledKitSupport.kitPhraseProject).
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

/// BrushTests.otherGenresAreBitIdentical: every other genre renders bit for bit what it did;
/// jazz's snare changes.
#[test]
fn other_genres_are_bit_identical() {
    let base = WaveOptions {
        master: Master::Linear,
        musicality: Fixes::BACKBEAT,
        ..Default::default()
    };
    let before = render_mono(&kit_phrase_project(), &base, SR, 1.0);
    for g in ["lofi", "bossa", "techno", "house"] {
        let mut o = base.for_genre(g);
        o.genre = None;
        assert!(
            identical(&render_mono(&kit_phrase_project(), &o, SR, 1.0), &before),
            "{g}"
        );
    }
    let mut jazz = base.for_genre("jazz");
    jazz.genre = None;
    assert!(!identical(
        &render_mono(&kit_phrase_project(), &jazz, SR, 1.0),
        &before
    ));
}

/// The brush replaces the snare's voice on a synth lane and no other lane's (the clap and the
/// kick keep beats').
#[test]
fn the_brush_is_the_snare_lane_only() {
    let mut p = kit_phrase_project();
    let mut clap = ring16("drums", 11, &[(2, 39)], 50);
    clap.track.channel = 11;
    p.nets.insert("clap", clap);
    let deck = WaveRunner::prepare(&p, SR, &brush_options(true), 0);
    for l in deck.lanes() {
        let brushed = l.voice.pulse.tau == vec![Brush::DECAY, Brush::ATTACK];
        assert_eq!(brushed, l.kind == "snare", "{}", l.kind);
    }
}
