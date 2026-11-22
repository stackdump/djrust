//! A net's own pan (`Track::pan`, the track's `pan` key): where a part sits in the stereo field,
//! as measured renders on a linear master. It needs no `.stereo`; it is hard (the other channel is
//! silent) with the level trimmed by `1 / (1 + |pan|)`, so the loud channel plays what the part
//! does unpanned and a pair panned either way is the part unpanned; it replaces the role's own
//! placement; a pan macro (`set_pan_move`) wins while it lasts and lets go back to it; a part
//! with no pan is the part it was.

mod common;
use common::measure::*;
use djrust::json::JsonValue;
use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;

const SR: f64 = 48_000.0;

/// A kick on the beat and a hat on the offbeats at 120 BPM; each net's pan. Under `.stereo` the
/// hat sits left of centre (+0.3).
fn drums(kick: Option<f64>, hat: Option<f64>) -> Project {
    let mut p = Project { tempo: 120.0, ..Default::default() };
    for (id, channel, note, steps, pan) in
        [("kick", 10, 36, [0, 4, 8, 12], kick), ("hihat", 12, 42, [2, 6, 10, 14], hat)]
    {
        let mut n = NetBundle::ring(16);
        for step in steps {
            n.bind(step, note, channel, 100, 50);
        }
        n.track.channel = channel;
        n.track.group = Some(id.into());
        n.track.pan = pan;
        p.nets.insert(id, n);
    }
    p
}

/// The pad chord, held, with a pan.
fn pad_net(pan: Option<f64>) -> NetBundle {
    chord_net(pan, [60, 64, 67])
}

fn chord_net(pan: Option<f64>, notes: [i64; 3]) -> NetBundle {
    let mut c = NetBundle::ring(16);
    for (v, note) in notes.into_iter().enumerate() {
        c.bind(v as i64, note, 7, 90, 1950);
    }
    c.track.channel = 7;
    c.track.group = Some("harmony".into());
    c.track.pan = pan;
    c
}

/// Just the pad (and, when asked, a twin of it with its own net id and pan).
fn pad_only(pan: Option<f64>, twin: Option<f64>) -> Project {
    pad_pair(pan, twin, [60, 64, 67])
}

/// The pad and a twin of it that plays `notes` instead.
fn pad_pair(pan: Option<f64>, twin: Option<f64>, notes: [i64; 3]) -> Project {
    let mut p = Project { tempo: 120.0, ..Default::default() };
    p.nets.insert("harmony", pad_net(pan));
    if let Some(t) = twin {
        let mut n = chord_net(Some(t), notes);
        n.riff_group = "harmony".into();
        n.riff_variant = "r".into();
        p.nets.insert("harmony_r", n);
    }
    p
}

fn linear(fixes: Fixes) -> WaveOptions {
    WaveOptions { master: Master::Linear, musicality: fixes, ..Default::default() }
}

/// Two seconds, stereo, 256-frame blocks; `moves` is called on the runner before each block with
/// the block's start frame.
fn render(
    p: &Project,
    fixes: Fixes,
    mut moves: impl FnMut(&mut WaveRunner, usize),
) -> (Vec<f32>, Vec<f32>) {
    let o = linear(fixes);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, 0));
    r.play();
    let mut l = vec![0f32; (2.0 * SR) as usize];
    let mut rr = l.clone();
    let mut off = 0;
    while off < l.len() {
        moves(&mut r, off);
        let n = 256.min(l.len() - off);
        r.process(&mut l[off..off + n], Some(&mut rr[off..off + n]), n);
        off += n;
    }
    (l, rr)
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| *v as f64 * *v as f64).sum()
}

/// The largest difference between two channels' samples.
fn worst(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).fold(0.0, f64::max)
}

/// The mid of a stereo render: what the part plays in each channel when it is not spread.
fn mid(s: &(Vec<f32>, Vec<f32>)) -> Vec<f32> {
    s.0.iter().zip(&s.1).map(|(l, r)| 0.5 * (l + r)).collect()
}

#[test]
fn a_net_with_a_pan_plays_on_its_side_only_and_at_the_level_it_had() {
    for fixes in [Fixes::EMPTY, Fixes::STEREO] {
        let plain = render(&pad_only(None, None), fixes, |_, _| {});
        let level = mid(&plain);
        assert!(energy(&level) > 1e-3, "the pad plays ({fixes:?})");
        let left = render(&pad_only(Some(-1.0), None), fixes, |_, _| {});
        assert!(energy(&left.1) < 1e-12, "nothing on the right of a left pan ({fixes:?})");
        assert!(worst(&left.0, &level) < 1e-6, "the left plays the part's level ({fixes:?})");
        let right = render(&pad_only(Some(1.0), None), fixes, |_, _| {});
        assert!(energy(&right.0) < 1e-12, "nothing on the left of a right pan ({fixes:?})");
        assert!(worst(&right.1, &level) < 1e-6, "the right plays the part's level ({fixes:?})");
    }
}

#[test]
fn a_pair_panned_both_ways_is_the_part_unpanned() {
    let plain = render(&pad_only(None, None), Fixes::EMPTY, |_, _| {});
    let pair = render(&pad_only(Some(-1.0), Some(1.0)), Fixes::EMPTY, |_, _| {});
    assert!(worst(&pair.0, &plain.0) < 1e-6, "left");
    assert!(worst(&pair.1, &plain.1) < 1e-6, "right");
}

#[test]
fn a_half_pan_leans_without_leaving_the_middle() {
    // pan -0.5: left = mid + side with the level trimmed by 1/1.5: 2/3 on the left of what a hard
    // pan plays there... and a third as much on the right of it
    let plain = render(&pad_only(None, None), Fixes::EMPTY, |_, _| {});
    let half = render(&pad_only(Some(-0.5), None), Fixes::EMPTY, |_, _| {});
    let level = mid(&plain);
    let (l, r) = (energy(&half.0) / energy(&level), energy(&half.1) / energy(&level));
    // amplitudes: left (1 + 0.5) / 1.5 = 1, right (1 - 0.5) / 1.5 = 1/3
    assert!((l - 1.0).abs() < 1e-3, "left level {l}");
    assert!((r - 1.0 / 9.0).abs() < 1e-3, "right level {r}");
}

#[test]
fn a_pan_replaces_the_roles_own_placement() {
    // under `.stereo` a hat sits left of centre (+0.3) and a pad is spread: a net pan puts either
    // wholly where it says
    let mut hat_only = drums(None, Some(1.0));
    hat_only.nets.remove("kick");
    let hat = render(&hat_only, Fixes::STEREO, |_, _| {});
    assert!(energy(&hat.0) < 1e-12 && energy(&hat.1) > 1e-6, "the hat is all on the right");
    let mut own = drums(None, None);
    own.nets.remove("kick");
    let unplaced = render(&own, Fixes::STEREO, |_, _| {});
    assert!(energy(&unplaced.0) > energy(&unplaced.1), "(the hat's own place is left of centre)");
    let wide = render(&pad_only(None, None), Fixes::STEREO, |_, _| {});
    assert!(!identical(&wide.0, &wide.1), "the pad starts wide");
    let held = render(&pad_only(Some(1.0), None), Fixes::STEREO, |_, _| {});
    assert!(energy(&held.0) < 1e-12, "no spread leaks to the left");
}

#[test]
fn the_other_parts_stay_where_they_were() {
    for fixes in [Fixes::EMPTY, Fixes::STEREO] {
        let alone = render(&drums(None, None), fixes, |_, _| {});
        let mut both = drums(None, None);
        both.nets.insert("harmony", pad_net(Some(-1.0)));
        let with_pad = render(&both, fixes, |_, _| {});
        let pad = render(&pad_only(Some(-1.0), None), fixes, |_, _| {});
        // the right channel is the kick and the hat alone: the pad is all on the left
        assert!(energy(&alone.1) > 1e-6);
        assert!(worst(&with_pad.1, &alone.1) < 1e-6, "the pad leaked right ({fixes:?})");
        assert!(
            worst(&with_pad.0, &sum(&alone.0, &pad.0)) < 1e-6,
            "left is the rest plus the pad ({fixes:?})"
        );
    }
}

fn sum(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

#[test]
fn a_pan_macro_wins_while_it_lasts_and_lets_go_back_to_the_nets_pan() {
    let plain = render(&pad_only(None, None), Fixes::EMPTY, |_, _| {});
    let level = mid(&plain);
    let n = SR as usize;
    // the pad is panned left; for the second half a hard-right macro holds every part
    let held = render(&pad_only(Some(-1.0), None), Fixes::EMPTY, |r, at| {
        if at >= n {
            r.set_pan_move(MacroTargets::Everything, Some(1.0));
        }
    });
    assert!(energy(&held.1[..n]) < 1e-12, "left of centre before the macro");
    assert!(energy(&held.0[n + 256..]) < 1e-12, "the macro moved it right");
    assert!(worst(&held.1[n + 256..], &level[n + 256..]) < 1e-6, "at the level it had");
    // and when the macro lets go, the part is back on its own side, not in the middle
    let back = render(&pad_only(Some(-1.0), None), Fixes::EMPTY, |r, at| {
        r.set_pan_move(MacroTargets::Everything, if at < n { Some(1.0) } else { None });
    });
    assert!(energy(&back.0[..n]) < 1e-12, "right while the macro lasts");
    assert!(energy(&back.1[n + 256..]) < 1e-12, "back on the left");
    assert!(worst(&back.0[n + 256..], &level[n + 256..]) < 1e-6, "at the level it had");
}

#[test]
fn mono_collapses_a_pair_to_the_middle_and_a_pair_then_lets_go() {
    let n = SR as usize;
    let pair = render(&pad_pair(Some(-1.0), Some(1.0), [62, 65, 69]), Fixes::EMPTY, |r, at| {
        r.set_pan_move(MacroTargets::Everything, if at < n { Some(0.0) } else { None });
    });
    assert!(identical(&pair.0[..n], &pair.1[..n]), "mono is mono");
    // letting go puts the pair back on its sides, not in the middle
    assert!(energy(&pair.1[n + 256..]) > 1e-3 && !identical(&pair.0[n + 256..], &pair.1[n + 256..]));
}

#[test]
fn a_net_with_no_pan_is_the_net_it_was() {
    // the key is written only when it is set, and 0 is no pan at all
    let plain = pad_net(None).to_json();
    assert!(plain.get("track").unwrap().get("pan").is_none());
    let zero = pad_net(Some(0.0)).to_json();
    assert!(zero.get("track").unwrap().get("pan").is_none());
    assert_eq!(plain.canonical_js(), zero.canonical_js(), "the same net, the same bytes");
    let left = pad_net(Some(-0.25)).to_json();
    assert_eq!(left.get("track").unwrap().get("pan").and_then(JsonValue::as_f64), Some(-0.25));
    // and it comes back
    assert_eq!(NetBundle::from_json(&left).track.pan, Some(-0.25));
    assert_eq!(NetBundle::from_json(&plain).track.pan, None);
    let read = |s: &str| NetBundle::from_json(&JsonValue::parse(s).unwrap()).track.pan;
    assert_eq!(read(r#"{"track":{"channel":7,"pan":0}}"#), None);
    assert_eq!(read(r#"{"track":{"channel":7,"pan":7}}"#), Some(1.0), "clamped");
    assert_eq!(read(r#"{"track":{"channel":7,"pan":-7}}"#), Some(-1.0), "clamped");
    assert_eq!(read(r#"{"track":{"channel":7,"pan":"left"}}"#), None, "not a number");
    // the same render either way
    let a = render(&pad_only(None, None), Fixes::STEREO, |_, _| {});
    let b = render(&pad_only(Some(0.0), None), Fixes::STEREO, |_, _| {});
    assert_eq!(a, b);
}

#[test]
fn the_engine_reads_the_pan_from_a_raw_nets_envelope() {
    // the path a BeatScript deck takes: JSON in, no Project struct built by hand
    let ring = |pan: &str| {
        let mut places = String::new();
        let mut trans = String::new();
        let mut arcs = String::new();
        let steps = 16;
        for i in 0..steps {
            if i > 0 {
                places.push(',');
                trans.push(',');
                arcs.push(',');
            }
            places.push_str(&format!(r#""p{i}":{}"#, if i == 0 { r#"{"initial":[1]}"# } else { "{}" }));
            let midi = if i == 0 {
                r#"{"midi":{"note":69,"channel":6,"velocity":100,"durationSteps":8}}"#
            } else {
                "{}"
            };
            trans.push_str(&format!(r#""t{i}":{midi}"#));
            arcs.push_str(&format!(
                r#"{{"source":"p{i}","target":"t{i}"}},{{"source":"t{i}","target":"p{}"}}"#,
                (i + 1) % steps
            ));
        }
        format!(
            r#"{{"tempo":120,"nets":{{"bass":{{"track":{{"channel":6,"instrument":"sub-bass"{pan}}},
            "places":{{{places}}},"transitions":{{{trans}}},"arcs":[{arcs}]}}}}}}"#
        )
    };
    let o = linear(Fixes::EMPTY);
    let go = |pan: &str| {
        let json = JsonValue::parse(&ring(pan)).unwrap();
        let mut r = WaveRunner::new(SR, o.clone());
        r.load_prepared(WaveRunner::prepare_json(&json, SR, &o));
        r.play();
        let mut l = vec![0f32; SR as usize];
        let mut rr = l.clone();
        for k in (0..l.len()).step_by(256) {
            let n = 256.min(l.len() - k);
            r.process(&mut l[k..k + n], Some(&mut rr[k..k + n]), n);
        }
        (l, rr)
    };
    let (l, r) = go(r#","pan":-1"#);
    assert!(energy(&l) > 1e-3 && energy(&r) < 1e-12, "left");
    let (l, r) = go(r#","pan":1"#);
    assert!(energy(&r) > 1e-3 && energy(&l) < 1e-12, "right");
    let (l, r) = go("");
    assert!(identical(&l, &r) && energy(&l) > 1e-3, "no key, in the middle");
}
