//! `.subOctave`: a sine an octave under the synth basses of dnb, dubstep and trap
//! (`WaveOptions::sub_octave`), measured on mono renders of one-note projects through a linear
//! master, 120 BPM. Swift's `SubOctaveTests` ported (PORTING.md's proof, form (a)); the same
//! voices held against Swift's output sample for sample are the "suboctave" cases of
//! `tests/runner_golden.rs`.

mod common;
use common::measure::*;
use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;
use djrust::TrackSpec;

const SR: f64 = 48_000.0;

fn bass_note(note: i64, steps: i64, group: &str) -> Project {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(64);
    n.bind(0, note, 6, 100, 0);
    n.bindings.get_mut("t0").unwrap().duration_steps = steps;
    n.track.channel = 6;
    n.track.group = Some(group.into());
    p.nets.insert(group, n);
    p
}

fn render(p: &Project, fixes: Fixes, sub: bool) -> Vec<f32> {
    let o = WaveOptions {
        master: Master::Linear,
        musicality: fixes,
        sub_octave: sub,
        ..Default::default()
    };
    render_mono(p, &o, SR, 1.0)
}

fn window(x: &[f32], a: f64, b: f64) -> &[f32] {
    &x[(a * SR) as usize..(b * SR) as usize]
}

/// SubOctaveTests.theGenresThatPlayIt: on for dnb, dubstep and trap when the deck's fixes hold
/// it; off for every other genre, and never on the parity path.
#[test]
fn the_genres_that_play_it() {
    let extended = WaveOptions {
        musicality: Fixes::ALL,
        ..WaveOptions::extended()
    };
    for g in ["dnb", "dubstep", "trap"] {
        assert!(extended.for_genre(g).sub_octave, "{g}");
        assert!(
            !WaveOptions::default().for_genre(g).sub_octave,
            "{g} parity"
        );
    }
    for g in ["techno", "house", "trance", "ambient", "jazz"] {
        assert!(!extended.for_genre(g).sub_octave, "{g}");
    }
    assert!(Fixes::ALL.contains(Fixes::SUB_OCTAVE));
    assert_eq!(Fixes::named("subOctave"), Some(Fixes::SUB_OCTAVE));
    assert!(!WaveOptions::default().sub_octave);
}

/// SubOctaveTests.aBassNoteGetsItsSubOctave: a bass on A2 (110 Hz) gains a sine on A1 (55 Hz),
/// in tune, about 6 dB under its fundamental; the note itself is unchanged.
#[test]
fn a_bass_note_gets_its_sub_octave() {
    let f0 = midi_hz(45);
    let p = bass_note(45, 8, "bass");
    let before = render(&p, Fixes::SUB_OCTAVE, false);
    let after = render(&p, Fixes::SUB_OCTAVE, true);
    let w = |x: &[f32]| window(x, 0.05, 0.45).to_vec();
    let sub_before = tone_db(&w(&before), f0 / 2.0, SR);
    let sub_after = tone_db(&w(&after), f0 / 2.0, SR);
    assert!(
        sub_after - sub_before > 20.0,
        "sub {sub_before} -> {sub_after} dB"
    );
    let pitch = partial_peak(&w(&after), f0 / 2.0, SR);
    assert!(cents(pitch, f0 / 2.0).abs() < 5.0, "sub at {pitch} Hz");
    let fund = tone_db(&w(&after), f0, SR);
    assert!((fund - tone_db(&w(&before), f0, SR)).abs() < 0.5);
    // -6 dB under the voice's oscillator: beats' table's fundamental is its own level
    assert!(
        fund - sub_after > -1.0 && fund - sub_after < 12.0,
        "fund {fund}, sub {sub_after}"
    );
}

/// SubOctaveTests.noSubUnderThirtyHertz: a note whose sub would sit under 30 Hz (A1, 55 Hz: a
/// 27.5 Hz sub) plays without one.
#[test]
fn no_sub_under_thirty_hertz() {
    let p = bass_note(33, 8, "bass");
    assert!(identical(
        &render(&p, Fixes::SUB_OCTAVE, false),
        &render(&p, Fixes::SUB_OCTAVE, true)
    ));
    // B1 (61.7 Hz) has one
    let q = bass_note(35, 8, "bass");
    assert!(!identical(
        &render(&q, Fixes::SUB_OCTAVE, false),
        &render(&q, Fixes::SUB_OCTAVE, true)
    ));
}

/// SubOctaveTests.onlyTheBass: a melody note is untouched, and so is everything without the fix.
#[test]
fn only_the_bass() {
    let lead = bass_note(57, 8, "melody");
    assert!(identical(
        &render(&lead, Fixes::SUB_OCTAVE, false),
        &render(&lead, Fixes::SUB_OCTAVE, true)
    ));
    let bass = bass_note(45, 8, "bass");
    assert!(identical(
        &render(&bass, Fixes::EMPTY, false),
        &render(&bass, Fixes::EMPTY, true)
    ));
}

/// SubOctaveTests.dnbsBassHasOneTechnosDoesNot: a dnb track's synth bass lanes get a sub; a
/// techno track's don't (the options a deck plays a track with: the extended ones, by genre).
#[test]
fn dnbs_bass_has_one_technos_does_not() {
    let deck = WaveOptions {
        musicality: Fixes::SUB_OCTAVE,
        ..WaveOptions::extended()
    };
    let prepare = |genre: &str| {
        let o = deck.for_genre(genre);
        let spec = TrackSpec::new(genre, 42.0, Some("minimal"));
        (
            o.sub_octave,
            WaveRunner::prepare(&spec.project(), SR, &o, 0),
        )
    };
    let (on, dnb) = prepare("dnb");
    assert!(on);
    assert!(dnb.lanes().iter().any(|l| l.has_sub_octave()));
    for lane in dnb.lanes().iter().filter(|l| l.has_sub_octave()) {
        assert!(lane.is_tonal() && !lane.is_pluck());
    }
    let (on, techno) = prepare("techno");
    assert!(!on);
    assert!(!techno.lanes().iter().any(|l| l.has_sub_octave()));
}
