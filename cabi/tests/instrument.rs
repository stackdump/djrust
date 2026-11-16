//! `djw_set_instrument` (feature INSTRUMENT, 2048), through the exports only, as the worklet
//! drives it:
//!
//! - neutral is idle: every net set to the instrument it already plays, after the load and
//!   again mid-song, reproduces the whole 0.2.0 golden corpus;
//! - a change before play is a load of the changed document, bit for bit;
//! - a change mid-song keeps the song where it is (the tick and the marking), and it sounds;
//! - its codes.

mod common;

use common::*;
use djrust::json::JsonValue;
use djwasm::*;

fn set_instrument(id: &str, name: &str) -> i32 {
    unsafe { djw_set_instrument(id.as_ptr(), id.len(), name.as_ptr(), name.len()) }
}

/// `(net id, track.instrument, track.channel, track.group)` of every net in `doc`.
fn tracks(doc: &str) -> Vec<(String, String, i64, String)> {
    let v = JsonValue::parse(doc).unwrap();
    let nets = v.get("nets").and_then(JsonValue::as_object).unwrap();
    nets.iter()
        .map(|(id, n)| {
            let t = n.get("track");
            let s = |k: &str| {
                t.and_then(|t| t.get(k))
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let ch = t
                .and_then(|t| t.get("channel"))
                .and_then(JsonValue::as_f64)
                .unwrap_or(1.0) as i64;
            (id.to_string(), s("instrument"), ch, s("group"))
        })
        .collect()
}

/// A melodic net (not drums, a pad or the bass) and an instrument in another voice family.
fn melodic(doc: &str) -> (String, &'static str) {
    let v = JsonValue::parse(doc).unwrap();
    let nets = v.get("nets").and_then(JsonValue::as_object).unwrap();
    for (id, inst, ch, group) in tracks(doc) {
        let role = nets
            .get(&id)
            .and_then(|n| n.get("role"))
            .and_then(JsonValue::as_str)
            .unwrap_or("music");
        // (beats' voice families: a pad is the soft voice and a bass the saw whatever their
        // instrument is named, `tonal_voice`; a melody's name picks its family)
        if role == "control"
            || (10..=15).contains(&ch)
            || matches!(group.as_str(), "harmony" | "pad" | "bass")
        {
            continue;
        }
        let to = if inst.contains("vibes") || inst.contains("bell") {
            "square-lead"
        } else {
            "vibes"
        };
        return (id, to);
    }
    panic!("no melodic net")
}

fn with_instrument(doc: &str, id: &str, name: &str) -> String {
    let mut v = JsonValue::parse(doc).unwrap();
    let JsonValue::Object(top) = &mut v else {
        panic!("not an object")
    };
    let Some(JsonValue::Object(nets)) = top.get_mut("nets") else {
        panic!("no nets")
    };
    let Some(JsonValue::Object(net)) = nets.get_mut(id) else {
        panic!("no {id}")
    };
    let Some(JsonValue::Object(track)) = net.get_mut("track") else {
        panic!("no track")
    };
    track.set("instrument", JsonValue::str(name));
    v.js_stringified()
}

const SPEC: &str = r#"{"genre":"techno","seed":31}"#;

fn doc() -> String {
    Source::Compose(SPEC.into()).doc()
}

fn render_doc(doc: &str, quanta: usize, hook: &mut dyn FnMut(usize)) -> Vec<f32> {
    assert!(djw_init(48000.0) > 0);
    assert!(call(djw_load, doc) > 0, "{}", out());
    djw_play();
    render_quanta(quanta, hook)
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

#[test]
fn the_build_reports_it() {
    let _g = lock();
    assert_ne!(djw_features() & 2048, 0);
    assert!(djw_keys() > 0);
    let k = JsonValue::parse(&out()).unwrap();
    let f = k.get("features").unwrap();
    assert_eq!(
        f.get("INSTRUMENT").and_then(JsonValue::as_f64),
        Some(2048.0)
    );
}

#[test]
fn neutral_is_idle_across_the_golden_corpus() {
    let _g = lock();
    let golden = load_golden();
    let mut bad = Vec::new();
    let accepted = std::cell::Cell::new(0usize);
    for (case, want) in &golden.cases {
        let own = tracks(&case.source.doc());
        let set_own = || {
            for (id, inst, _, _) in &own {
                if inst.is_empty() {
                    continue;
                }
                // a control net (or one without bindings) has no lanes: refused, untouched
                match set_instrument(id, inst) {
                    0 => accepted.set(accepted.get() + 1),
                    ERR_ARG => {}
                    r => panic!("{}: {id}: {r} {}", case.name, out()),
                }
            }
        };
        let got = run_full(case, &|sr| djw_init(sr), &mut || set_own(), &mut |q| {
            // a load in the script may have swapped in another document: its own instruments
            // are not ours, so only the first half is neutral by construction for every case
            if q == case.quanta / 4 && case.script.iter().all(|(_, op)| !matches!(op, Op::Load(_)))
            {
                set_own();
            }
        })
        .hashes;
        bad.extend(diff(case, want, &got));
    }
    assert!(accepted.get() > 0, "no net took its own instrument");
    assert!(
        bad.is_empty(),
        "{} streams differ from 0.2.0:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn before_play_it_is_a_load_of_the_changed_document() {
    let _g = lock();
    let d = doc();
    let (id, to) = melodic(&d);
    let got = {
        assert!(djw_init(48000.0) > 0);
        assert!(call(djw_load, &d) > 0);
        assert_eq!(set_instrument(&id, to), 0, "{}", out());
        djw_play();
        render_quanta(2000, &mut |_| {})
    };
    let want = render_doc(&with_instrument(&d, &id, to), 2000, &mut |_| {});
    let unchanged = render_doc(&d, 2000, &mut |_| {});
    assert!(
        same(&got, &want),
        "{id} on {to}: not the changed document's render"
    );
    assert!(!same(&got, &unchanged), "{id} on {to}: sounds as before");
}

#[test]
fn mid_song_it_keeps_the_song_where_it_is() {
    let _g = lock();
    let d = doc();
    let (id, to) = melodic(&d);
    let mut marking_ref = String::new();
    let mut tick_ref = 0.0;
    let reference = render_doc(&d, 1500, &mut |_| {});
    {
        djw_marking();
        marking_ref.push_str(&out());
        tick_ref += djw_tick();
    }
    let changed = render_doc(&d, 1500, &mut |q| {
        if q == 700 {
            assert_eq!(set_instrument(&id, to), 0, "{}", out());
        }
    });
    djw_marking();
    assert_eq!(out(), marking_ref, "the marking moved");
    assert_eq!(djw_tick(), tick_ref, "the tick moved");
    assert_eq!(djw_playing(), 1);
    let at = 700 * Q * 2;
    assert!(
        same(&changed[..at], &reference[..at]),
        "it changed the past"
    );
    assert!(
        !same(&changed[at..], &reference[at..]),
        "{id} on {to}: no change heard"
    );
}

#[test]
fn codes() {
    let _g = lock();
    assert!(djw_init(48000.0) > 0);
    assert_eq!(set_instrument("melody", "vibes"), ERR_NOT_READY);
    let d = doc();
    assert!(call(djw_load, &d) > 0);
    let (id, _) = melodic(&d);
    assert_eq!(set_instrument(&id, ""), ERR_ARG);
    assert_eq!(set_instrument("no-such-net", "vibes"), ERR_ARG);
    let bad = [0xffu8, 0xfe];
    assert_eq!(
        unsafe { djw_set_instrument(bad.as_ptr(), bad.len(), b"vibes".as_ptr(), 5) },
        ERR_ARG
    );
    assert!(out().contains("UTF-8"));
    // over and over, as a dropdown would: every swap frees the lanes it replaced
    for k in 0..40 {
        let to = if k % 2 == 0 { "vibes" } else { "square-lead" };
        assert_eq!(set_instrument(&id, to), 0);
    }
}
