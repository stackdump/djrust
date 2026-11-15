//! The C ABI of `djwasm`, called as plain Rust (the same functions the wasm module exports).
//! The JS-oracle comparison (wasm vs beats' own wave engine and generator) lives in
//! beats-bitwrap-io `scripts/test-djrust-wasm.mjs`; these tests hold the surface itself: the
//! calling convention, the error paths, determinism, and that a render equals djrust's.

use std::sync::Mutex;

use djrust::json::{JsonObject, JsonValue};
use djrust::wave::runner::{WaveOptions, WaveRunner};
use djwasm::*;

// The exports share one engine and one out buffer, so the tests run one at a time.
static LOCK: Mutex<()> = Mutex::new(());

fn out() -> String {
    unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(djw_out_ptr(), djw_out_len()))
            .unwrap()
            .to_string()
    }
}

fn call(f: unsafe extern "C" fn(*const u8, usize) -> i32, s: &str) -> i32 {
    unsafe { f(s.as_ptr(), s.len()) }
}

fn composed(spec: &str) -> JsonValue {
    assert!(call(djw_compose, spec) > 0, "{}", out());
    JsonValue::parse(&out()).unwrap()
}

fn render(doc: &str, seconds: f64) -> (Vec<f32>, Vec<f32>, i64) {
    assert_eq!(djw_init(48000.0), 4096);
    assert!(call(djw_load, doc) > 0, "{}", out());
    djw_play();
    let frames = (seconds * 48000.0) as usize;
    let (mut l, mut r, mut ticks) = (Vec::new(), Vec::new(), 0i64);
    let mut done = 0;
    while done < frames {
        let n = 128.min(frames - done);
        ticks += djw_process(n as u32) as i64;
        unsafe {
            l.extend_from_slice(std::slice::from_raw_parts(djw_left_ptr(), n));
            r.extend_from_slice(std::slice::from_raw_parts(djw_right_ptr(), n));
        }
        done += n;
    }
    (l, r, ticks)
}

#[test]
fn abi_and_info() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(djw_abi(), ABI_VERSION);
    assert!(djw_info() > 0);
    let info = JsonValue::parse(&out()).unwrap();
    assert_eq!(
        info.get("abi").and_then(JsonValue::as_f64),
        Some(ABI_VERSION as f64)
    );
}

#[test]
fn compose_is_the_engines_own_compose() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"techno","seed":42,"structure":"standard"}"#);
    let mut ov = JsonObject::new();
    ov.set("seed", JsonValue::Number(42.0));
    ov.set("structure", JsonValue::str("standard"));
    let p = djrust::compose("techno", &JsonValue::Object(ov));
    assert_eq!(
        r.get("modelCid").and_then(JsonValue::as_str),
        Some(p.model_cid().as_str())
    );
    assert_eq!(
        r.get("name").and_then(JsonValue::as_str),
        Some(p.name.as_str())
    );
    assert_eq!(
        r.get("doc").unwrap().js_stringified(),
        p.to_json().js_stringified()
    );
    assert_eq!(
        r.get("canonicalBytes").and_then(JsonValue::as_f64),
        Some(p.canonical_json().len() as f64)
    );
}

#[test]
fn model_cid_of_a_document_matches_compose() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"jazz","seed":7}"#);
    let doc = r.get("doc").unwrap().js_stringified();
    assert_eq!(call(djw_model_cid, &doc), 64);
    assert_eq!(
        out(),
        r.get("modelCid").and_then(JsonValue::as_str).unwrap()
    );
}

#[test]
fn bad_arguments_return_codes_not_traps() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(call(djw_compose, "not json"), ERR_ARG);
    assert!(out().contains("not JSON"));
    assert_eq!(call(djw_compose, "[1]"), ERR_ARG);
    assert_eq!(call(djw_model_cid, "{"), ERR_ARG);
    assert_eq!(djw_init(0.0), ERR_ARG);
    assert_eq!(djw_init(48000.0), 4096);
    assert_eq!(call(djw_load, "{}"), ERR_ARG);
    assert_eq!(call(djw_load, r#"{"nets":1}"#), ERR_ARG);
    // A bad document leaves the runner usable.
    assert_eq!(djw_process(128), 0);
}

#[test]
fn render_equals_the_engine_run_directly() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"house","seed":3,"structure":"standard"}"#);
    let doc = r.get("doc").unwrap().js_stringified();
    let (l, rr, ticks) = render(&doc, 6.0);
    assert!(ticks > 20, "{ticks} ticks");
    assert!(l.iter().any(|x| x.abs() > 0.05), "silent");

    let mut direct = WaveRunner::new(48000.0, WaveOptions::default());
    direct.load_json(&JsonValue::parse(&doc).unwrap());
    direct.play();
    let (mut dl, mut dr) = (vec![0f32; l.len()], vec![0f32; l.len()]);
    let mut off = 0;
    while off < l.len() {
        let n = 128.min(l.len() - off);
        direct.process(&mut dl[off..off + n], Some(&mut dr[off..off + n]), n);
        off += n;
    }
    assert!(
        l.iter().zip(&dl).all(|(a, b)| a.to_bits() == b.to_bits()),
        "left differs"
    );
    assert!(
        rr.iter().zip(&dr).all(|(a, b)| a.to_bits() == b.to_bits()),
        "right differs"
    );
}

#[test]
fn render_is_deterministic() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"dnb","seed":9}"#);
    let doc = r.get("doc").unwrap().js_stringified();
    let a = render(&doc, 3.0);
    let b = render(&doc, 3.0);
    assert!(a
        .0
        .iter()
        .zip(&b.0)
        .all(|(x, y)| x.to_bits() == y.to_bits()));
    assert_eq!(a.2, b.2);
}

#[test]
fn report_describes_a_tick_once() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"techno","seed":42}"#);
    let doc = r.get("doc").unwrap().js_stringified();
    assert_eq!(djw_init(48000.0), 4096);
    assert!(call(djw_load, &doc) > 0);
    djw_play();
    let mut seen = 0;
    for _ in 0..400 {
        if djw_process(128) > 0 {
            let n = djw_report();
            assert!(n > 0);
            let rep = JsonValue::parse(&out()).unwrap();
            assert_eq!(
                rep.get("tick").and_then(JsonValue::as_f64),
                Some(djw_tick())
            );
            for f in rep.get("fired").and_then(JsonValue::as_array).unwrap() {
                let midi = f.at(2).unwrap();
                assert!(
                    midi.get("note").is_some()
                        && midi.get("velocity").is_some()
                        && midi.get("channel").is_some()
                );
            }
            assert_eq!(djw_report(), 0, "taken once");
            seen += 1;
        }
    }
    assert!(seen > 5);
    assert!(djw_marking() > 2);
    assert!(out().starts_with("{\"kick\":"));
    assert!(djw_muted() > 2);
}

#[test]
fn mute_silences_a_net_and_transport_is_reported() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = composed(r#"{"genre":"techno","seed":42}"#);
    let doc = r.get("doc").unwrap().js_stringified();
    let (all, _, _) = render(&doc, 4.0);
    assert_eq!(djw_init(48000.0), 4096);
    assert!(call(djw_load, &doc) > 0);
    // Mute every net: nothing sounds.
    djw_muted();
    let muted = JsonValue::parse(&out()).unwrap();
    for (id, _) in muted.as_object().unwrap().iter() {
        assert_eq!(unsafe { djw_set_mute(id.as_ptr(), id.len(), 1) }, 0);
    }
    djw_play();
    assert_eq!(djw_playing(), 1);
    let mut peak = 0f32;
    for _ in 0..(4 * 48000 / 128) {
        djw_process(128);
        for x in unsafe { std::slice::from_raw_parts(djw_left_ptr(), 128) } {
            peak = peak.max(x.abs());
        }
    }
    assert!(peak < 1e-6, "muted peak {peak}");
    assert!(all.iter().any(|x| x.abs() > 0.05));
    djw_stop();
    assert_eq!(djw_playing(), 0);
    assert_eq!(djw_take_stopped(), 0);
}

#[test]
fn alloc_roundtrip() {
    let p = djw_alloc(10);
    assert!(!p.is_null());
    unsafe { djw_free(p, 10) };
    let z = djw_alloc(0);
    assert!(!z.is_null());
    unsafe { djw_free(z, 0) };
}
