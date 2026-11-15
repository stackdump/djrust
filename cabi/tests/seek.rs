//! `djw_seek(t)`: the net is where a fresh graph fast-forwarded to `t` is (beats' worker
//! `fastForwardTo`), and what plays from there is what that fresh graph plays.

mod common;

use common::*;
use djrust::json::{JsonObject, JsonValue};
use djrust::wave::runner::{WaveOptions, WaveRunner};
use djwasm::*;

const SR: f64 = 48000.0;
const AFTER: usize = 900; // quanta rendered after the seek (2.4 s)

/// The oracle: a fresh runner, `fast_forward_replay(t)` on its graph (every net ticked `t`
/// times; `djw_seek` takes `fast_forward`, which skips cycles), then play. Also the tick it
/// reached (short of `t` past a song's `stop-transport`).
fn fresh(doc: &str, t: i64) -> (Vec<f32>, String, f64) {
    let mut r = WaveRunner::new(SR, WaveOptions::default());
    r.load_json(&JsonValue::parse(doc).unwrap());
    r.graph_mut().unwrap().fast_forward_replay(t);
    let marking = marking_of(&r);
    let reached = r.graph().unwrap().tick as f64;
    r.play();
    let mut lr = Vec::new();
    let (mut l, mut rr) = ([0f32; Q], [0f32; Q]);
    for _ in 0..AFTER {
        r.process(&mut l, Some(&mut rr), Q);
        for i in 0..Q {
            lr.push(l[i]);
            lr.push(rr[i]);
        }
    }
    (lr, marking, reached)
}

/// `djw_marking`'s text, built from a runner directly.
fn marking_of(r: &WaveRunner) -> String {
    let g = r.graph().unwrap();
    let mut top = JsonObject::new();
    for n in &g.nets {
        let mut m = JsonObject::new();
        for p in 0..n.n_places {
            m.set(n.place_ids[p].clone(), JsonValue::Number(n.state[p]));
        }
        top.set(n.id.clone(), JsonValue::Object(m));
    }
    JsonValue::Object(top).js_stringified()
}

fn module_marking() -> String {
    assert!(djw_marking() > 2);
    out()
}

fn docs() -> Vec<String> {
    [
        r#"{"genre":"techno","seed":42}"#,
        r#"{"genre":"jazz","seed":7,"structure":"standard"}"#,
    ]
    .iter()
    .map(|s| Source::Compose(s.to_string()).doc())
    .collect()
}

#[test]
fn seek_while_stopped_is_a_fresh_fast_forward() {
    let _g = lock();
    for doc in docs() {
        for t in [0i64, 1, 37, 200, 1000, 3000] {
            let (want, want_marking, reached) = fresh(&doc, t);
            assert_eq!(djw_init(SR), 4096);
            assert!(call(djw_load, &doc) > 0);
            assert_eq!(djw_seek(t as f64), reached);
            assert_eq!(djw_tick(), reached);
            assert_eq!(module_marking(), want_marking, "marking at {t}");
            djw_play();
            let got = render_quanta(AFTER, &mut |_| {});
            assert!(
                first_diff(&want, &got).is_none(),
                "t={t}: {:?}",
                first_diff(&want, &got)
            );
        }
    }
}

/// Seeking a playing track: the marking is the fresh graph's, the transport keeps playing, the
/// frame clock restarts, and the result is deterministic. The audio is NOT a fresh runner's:
/// `stop` + `silence` clear the voices' envelopes and filter memories, but not the per-lane
/// oscillator phases, noise generators and voice rotation (WaveRunner has no in-place full reset;
/// only a new install has fresh lanes). Measured: it differs from the first sample after the
/// seek. The test records that and holds the rest.
#[test]
fn seek_while_playing_lands_on_the_fresh_marking_deterministically() {
    let _g = lock();
    for doc in docs() {
        for (played, t) in [(500usize, 37i64), (900, 200), (300, 1000), (1200, 5)] {
            let (fresh_audio, want_marking, reached) = fresh(&doc, t);
            let mut runs = Vec::new();
            for _ in 0..2 {
                assert_eq!(djw_init(SR), 4096);
                assert!(call(djw_load, &doc) > 0);
                djw_play();
                render_quanta(played, &mut |_| {});
                assert!(djw_frame() > 0.0);
                assert_eq!(djw_seek(t as f64), reached);
                assert_eq!(djw_frame(), 0.0, "the frame clock restarts");
                assert_eq!(djw_playing(), 1, "still playing");
                assert_eq!(module_marking(), want_marking, "marking at {t}");
                runs.push(render_quanta(AFTER, &mut |_| {}));
            }
            assert!(
                first_diff(&runs[0], &runs[1]).is_none(),
                "seek is not deterministic"
            );
            if let Some((i, max)) = first_diff(&fresh_audio, &runs[0]) {
                eprintln!(
                    "played {played} quanta, seek {t}: audio vs a fresh runner differs from frame {} (max |d| {max})",
                    i / 2
                );
            }
        }
    }
}

#[test]
fn the_next_report_is_the_tick_after_the_seek() {
    let _g = lock();
    let doc = &docs()[0];
    assert_eq!(djw_init(SR), 4096);
    assert!(call(djw_load, doc) > 0);
    djw_play();
    render_quanta(400, &mut |_| {});
    assert_eq!(djw_seek(64.0), 64.0);
    assert_eq!(djw_report(), 0, "nothing pending after a seek");
    loop {
        if djw_process(Q as u32) > 0 {
            assert!(djw_report() > 0);
            let r = JsonValue::parse(&out()).unwrap();
            assert_eq!(r.get("tick").and_then(JsonValue::as_f64), Some(65.0));
            break;
        }
    }
}

#[test]
fn bad_seeks_are_codes() {
    let _g = lock();
    assert_eq!(djw_init(SR), 4096);
    assert_eq!(djw_seek(10.0), ERR_NOT_READY as f64);
    assert!(call(djw_load, &docs()[0]) > 0);
    assert_eq!(djw_seek(-1.0), ERR_ARG as f64);
    assert_eq!(djw_seek(f64::NAN), ERR_ARG as f64);
    assert_eq!(djw_seek(f64::INFINITY), ERR_ARG as f64);
    assert_eq!(djw_seek(3.0), 3.0);
}

/// Measured, not gated here (beats' djrust-cpu gates the wasm cost at 2.0 ms).
#[test]
fn seek_cost_at_tick_3000() {
    let _g = lock();
    let doc =
        Source::Compose(r#"{"genre":"techno","seed":42,"structure":"extended"}"#.into()).doc();
    assert_eq!(djw_init(SR), 4096);
    assert!(call(djw_load, &doc) > 0);
    djw_play();
    render_quanta(10, &mut |_| {});
    let t0 = std::time::Instant::now();
    let reached = djw_seek(3000.0);
    let dt = t0.elapsed();
    eprintln!(
        "seek to 3000 on techno:42:extended ({} nets): {:.3} ms native, reached {reached}",
        net_ids(&doc).len(),
        dt.as_secs_f64() * 1e3
    );
    assert!(reached > 0.0);
}
