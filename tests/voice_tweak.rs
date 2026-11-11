//! `VoiceTweakTests.swift` (the engine and codec halves): the sound panel shapes a
//! channel's voice live, on the song playing, with no reload. Golden: none (dj-only).

use djrust::json::JsonValue;
use djrust::wave::*;
use djrust::TrackSpec;

const SR: f64 = 22_050.0;

fn song() -> djrust::Project {
    TrackSpec::new("techno", 42.0, Some("minimal")).project()
}

/// The MIDI channel of the first non-control net of `group`.
fn channel(p: &djrust::Project, group: &str) -> i64 {
    p.nets
        .values()
        .find(|n| n.role != "control" && n.track.group.as_deref() == Some(group))
        .map(|n| n.track.channel)
        .unwrap()
}

fn source(p: &djrust::Project) -> WaveRunner {
    let opts = WaveOptions::extended();
    let mut r = WaveRunner::new(SR, opts.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &opts, 0));
    r.set_tempo(300.0);
    r
}

fn render(r: &mut WaveRunner, blocks: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut l = vec![0f32; 256];
    for _ in 0..blocks {
        r.process(&mut l, None, 256);
        out.extend_from_slice(&l);
    }
    out
}

fn lanes(r: &WaveRunner, ch: Option<i64>) -> Vec<Vec<f64>> {
    r.project()
        .unwrap()
        .lanes()
        .iter()
        .filter(|l| ch.is_none_or(|c| l.channel == c))
        .map(|l| l.voice_state())
        .collect()
}

fn shaped() -> VoiceTweak {
    VoiceTweak {
        gain_db: Some(9.0),
        attack: Some(0.04),
        decay_scale: Some(2.0),
        max_decay: Some(3.0),
        lowpass: Some(900.0),
        distortion: Some(0.4),
        vibrato_rate: Some(6.0),
        vibrato_depth: Some(0.02),
        pitch_from: Some(3.0),
        pitch_to: Some(1.0),
        pitch_time: Some(0.15),
    }
}

fn params(ch: i64, t: &VoiceTweak) -> VoiceLaneParams {
    VoiceLaneParams::new(ch, t, false, None, SR)
}

#[test]
fn a_tweak_changes_that_channel_at_once_without_a_reload() {
    let p = song();
    let ch = channel(&p, "melody");
    let (mut a, mut b) = (source(&p), source(&p));
    a.play();
    b.play();
    assert!(
        render(&mut a, 150) == render(&mut b, 150),
        "the same song, the same audio"
    );
    let gen = b.generation();
    let tick = b.graph().unwrap().tick;
    b.set_voice(&params(ch, &shaped()));
    let ra = render(&mut a, 400);
    let rb = render(&mut b, 400);
    assert!(ra != rb, "the tweak is heard");
    let (ta, tb) = (a.graph().unwrap().tick, b.graph().unwrap().tick);
    assert!(tb > tick && tb == ta, "the song kept going: no restart");
    assert_eq!(b.generation(), gen, "the same project: no reload");
    // the lanes on that channel changed, every other lane is as it was
    assert!(lanes(&a, Some(ch)) != lanes(&b, Some(ch)));
    let others = |r: &WaveRunner| -> Vec<Vec<f64>> {
        r.project()
            .unwrap()
            .lanes()
            .iter()
            .filter(|l| l.channel != ch)
            .map(|l| l.voice_state())
            .collect()
    };
    assert_eq!(others(&a), others(&b));
    // and the swept saw reads its band-limited tables
    let swept: Vec<&Lane> = b
        .project()
        .unwrap()
        .lanes()
        .iter()
        .filter(|l| l.channel == ch && l.is_tonal() && !l.is_pluck())
        .collect();
    assert!(!swept.is_empty() && swept.iter().all(|l| l.pm_on()));
    assert!(swept.iter().filter(|l| l.has_mip()).all(|l| l.mip_on()));
}

#[test]
fn only_that_channel_is_heard_to_change() {
    // with the tweaked channel's lanes silenced on both (a section's control nets would
    // unmute muted nets), the two decks stay identical
    let p = song();
    let ch = channel(&p, "melody");
    let (mut a, mut b) = (source(&p), source(&p));
    for s in [&mut a, &mut b] {
        for lane in s.project_mut().unwrap().lanes_mut() {
            if lane.channel == ch {
                lane.set_base_level(0.0);
                lane.set_voice(&params(ch, &VoiceTweak::default()));
            }
        }
        s.play();
    }
    render(&mut a, 50);
    render(&mut b, 50);
    b.set_voice(&params(ch, &shaped()));
    let ra = render(&mut a, 300);
    assert!(ra == render(&mut b, 300));
    assert!(ra.iter().any(|&x| x != 0.0), "the rest of the song plays");
}

#[test]
fn reset_restores_the_instruments_voice_bit_exactly() {
    let p = song();
    let ch = channel(&p, "melody");
    let bass = channel(&p, "bass");
    let (mut a, mut b) = (source(&p), source(&p));
    a.play();
    b.play();
    render(&mut a, 100);
    render(&mut b, 100);
    b.set_voice(&params(ch, &shaped()));
    b.set_voice(&params(
        bass,
        &VoiceTweak {
            gain_db: Some(-6.0),
            lowpass: Some(300.0),
            ..Default::default()
        },
    ));
    render(&mut b, 100);
    assert!(lanes(&a, None) != lanes(&b, None));
    b.set_voice(&params(ch, &VoiceTweak::default()));
    b.set_voice(&params(bass, &VoiceTweak::default()));
    render(&mut b, 1);
    assert_eq!(
        lanes(&a, None),
        lanes(&b, None),
        "every lane back to its prepared values, bit for bit"
    );
    // tweaked and reset before the song starts: the very same audio
    let (mut c, mut d) = (source(&p), source(&p));
    d.set_voice(&params(ch, &shaped()));
    render(&mut d, 1);
    d.set_voice(&params(ch, &VoiceTweak::default()));
    c.play();
    d.play();
    assert!(render(&mut c, 300) == render(&mut d, 300));
}

#[test]
fn a_tweak_keeps_to_its_ranges() {
    let t = VoiceTweak::from_json(Some(
        &JsonValue::parse(
            r#"{"attack":-1,"lowpass":10,"distortion":5,"gainDB":100,"bogus":1,"pitchTime":2}"#,
        )
        .unwrap(),
    ));
    assert!(t.attack == Some(0.0005) && t.lowpass == Some(0.0));
    assert!(t.distortion == Some(1.0) && t.gain_db == Some(24.0));
    assert!(t.pitch_time == Some(2.0) && t.decay_scale.is_none());
    assert_eq!(VoiceTweak::from_json(Some(&t.to_json())), t);
    assert!(VoiceTweak::default().is_empty() && !t.is_empty());
    let mut u = VoiceTweak::default();
    u.set("lowpass", Some(30_000.0));
    assert_eq!(u.lowpass, Some(20_000.0));
    u.set("lowpass", None);
    assert!(u.is_empty());
    // only the set fields, in the panel's key order
    let j = VoiceTweak {
        lowpass: Some(1200.0),
        gain_db: Some(-4.5),
        ..Default::default()
    }
    .to_json();
    assert_eq!(j.js_stringified(), r#"{"gainDB":-4.5,"lowpass":1200}"#);
}

#[test]
fn the_voice_it_shapes() {
    let base = InstrumentTable::shared().voice(Some("laser"), Some("melody"));
    let v = VoiceTweak {
        gain_db: Some(6.0),
        lowpass: Some(2000.0),
        ..Default::default()
    }
    .applied_to(&base);
    assert!(v.lowpass == 2000.0 && v.attack == base.attack && v.pitch_from == base.pitch_from);
    assert!((v.level / base.level - 1.9953).abs() < 1e-3);
    assert_eq!(VoiceTweak::default().applied_to(&base), base);
}

#[test]
fn parameters_put_back_what_they_do_not_set() {
    let laser = InstrumentTable::shared().voice(Some("laser"), Some("melody"));
    let p = VoiceLaneParams::new(
        2,
        &VoiceTweak {
            pitch_time: Some(0.3),
            ..Default::default()
        },
        false,
        Some(&laser),
        SR,
    );
    assert!(p.has(VoiceLaneParams::PITCH) && !p.has(VoiceLaneParams::LEVEL));
    assert!(
        p.pm_on() && p.pitch_from() > p.pitch_to(),
        "a partial sweep fills in from the instrument"
    );
    assert_eq!(
        VoiceLaneParams::new(2, &VoiceTweak::default(), false, None, SR).mask(),
        0
    );
}
