//! The default path is djwasm 0.2.0, byte for byte.
//!
//! `fixtures/golden-0.2.0.json` was written by this file's `write` mode from the UNCHANGED ABI-1
//! module (cabi @ fa73dc3, djwasm 0.2.0, before any 0.3.0 source existed), natively:
//!
//!     DJW_GOLDEN_WRITE=1 cargo test -p djwasm --test golden -- --nocapture
//!
//! Every later build must reproduce every stream of every case (PCM, ticks per quantum, the
//! report, marking and muted texts, the self-stop flag) through the ABI-1 call sequence alone:
//! `djw_init`, `djw_load`, `djw_play`, `djw_process(128)`, ... No new export is called here;
//! `idle.rs` is the same corpus with every new knob sent at its neutral value.
//!
//! The canonical gate is beats' `make test-djrust-golden` on the wasm; this is its native twin.
//! Whether the native hashes equal the pinned wasm's is recorded in the fixture (`"wasm"`).

mod common;

use common::*;
use djrust::json::{JsonObject, JsonValue};

#[test]
fn default_path_reproduces_golden_0_2_0() {
    let _g = lock();
    if std::env::var_os("DJW_GOLDEN_WRITE").is_some() {
        write();
        return;
    }
    let golden = load_golden();
    assert!(golden.cases.len() > 150, "corpus too small");
    let mut bad = Vec::new();
    for (case, want) in &golden.cases {
        let got = run(case).hashes;
        bad.extend(diff(case, want, &got));
    }
    assert!(
        bad.is_empty(),
        "{} streams differ from 0.2.0:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The corpus the fixture holds is the one `common::corpus()` builds (so idle.rs and the
/// fixture cannot drift apart).
#[test]
fn fixture_is_the_corpus() {
    let golden = load_golden();
    let want: Vec<String> = corpus()
        .iter()
        .map(|c| c.to_json().js_stringified())
        .collect();
    let got: Vec<String> = golden
        .cases
        .iter()
        .map(|(c, _)| c.to_json().js_stringified())
        .collect();
    assert_eq!(want, got);
}

fn write() {
    let mut cases = Vec::new();
    for case in corpus() {
        let doc = case.source.doc();
        for (_, op) in &case.script {
            if let Op::Mute(id, _) = op {
                assert!(net_ids(&doc).contains(id), "{}: no net {id}", case.name);
            }
        }
        let h = run(&case).hashes;
        let mut c = match case.to_json() {
            JsonValue::Object(o) => o,
            _ => unreachable!(),
        };
        let mut ho = JsonObject::new();
        for k in Hashes::KEYS {
            ho.set(k, JsonValue::str(h.get(k).to_string()));
        }
        c.set("hashes", JsonValue::Object(ho));
        cases.push(JsonValue::Object(c));
        eprintln!("{}", case.name);
    }
    let mut top = JsonObject::new();
    top.set(
        "engine",
        JsonValue::str(format!(
            "djwasm {} (ABI {}), native {} build of cabi @ fa73dc3, unchanged",
            env!("CARGO_PKG_VERSION"),
            djwasm::ABI_VERSION,
            std::env::consts::ARCH
        )),
    );
    top.set(
        "recipe",
        JsonValue::str(
            "per case: djw_init(sampleRate); djw_load(doc); djw_play(); then per quantum q: the \
             script's calls at q, djw_process(128), djw_report() when it returned >0, \
             djw_take_stopped(), and when q%32==31 djw_marking()+djw_muted(); marking+muted once \
             more at the end. pcm = sha256(interleaved LE f32 L,R); ticks = sha256(i32 LE per \
             quantum); report/marking/muted = sha256(each text + '\\n'); stopped = sha256(one \
             byte per quantum). compose sources go through djw_compose and load its doc \
             (js_stringified).",
        ),
    );
    top.set("cases", JsonValue::Array(cases));
    std::fs::create_dir_all(fixtures()).unwrap();
    std::fs::write(
        golden_path(),
        JsonValue::Object(top).js_stringified() + "\n",
    )
    .unwrap();
}
