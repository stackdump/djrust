//! Swift golden for `.setPalette` and `.setKit` through `DJEngine.perform` (`tools/paritydump`,
//! section "performpalette": `swift/performpalette.json`). `perform_golden.rs` has `setKit` with
//! fake ids and `setRoleVoice`, but no `setPalette` and no real kit id; the recorded session text
//! and CID of a `.setPalette` were not compared. Two scenarios on a started, deterministic
//! engine at 22.05 kHz with two decks: palette hops (a hop, the same hop again, none, Studio,
//! several palettes, the Minecraft one, an unknown one, the empty id, a deck with nothing on it,
//! a sample deck, an unknown deck) and real kit ids (MuseScore's, Minecraft's, Apple's, the
//! empty one, one that is no id, and the kit pick against palette hops both ways). After each op
//! the decks' mode, palette, kit, spec payload and project are compared, and at the end the
//! recorded session's canonical text and CID. The Swift engine had an empty SoundFont folder, as
//! the Rust engine has nothing installed; the Mac's Apple GM bank is in the fixture (`gmBank`).

mod common_fixture;
mod common_track;

use common_track::Stepper;
use djrust::control::controller::{DeckMode, DeckView};
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::host::deck::PerformCtx;

const SR: f64 = 22_050.0;

fn sha(s: &str) -> String {
    djrust::cid::hex(&djrust::cid::sha256(s.as_bytes()))
}

fn opt(v: Option<String>) -> JsonValue {
    v.map_or(JsonValue::Null, JsonValue::String)
}

fn deck_row(st: &Stepper, name: &str) -> JsonValue {
    let e = &st.e;
    let d = e.deck(name).unwrap();
    let net = DeckView::mode(d) == DeckMode::Net;
    let mut o = djrust::json::JsonObject::new();
    o.set("mode", JsonValue::str(if net { "net" } else { "sample" }));
    o.set("palette", opt(e.palette(name)));
    o.set("kit", opt(e.kit(name)));
    o.set("spec", opt(d.spec.as_ref().map(|s| sha(&s.payload().canonical_js()))));
    o.set("payload", opt(d.spec.as_ref().map(|s| s.payload().js_stringified())));
    o.set("project", opt(d.project.as_ref().map(|p| sha(&p.canonical_json()))));
    JsonValue::Object(o)
}

fn compare(ctx: &str, want: &JsonValue, got: &JsonValue, diffs: &mut Vec<String>) {
    match (want, got) {
        (JsonValue::Object(w), JsonValue::Object(g)) => {
            for (k, wv) in w.iter() {
                match g.get(k) {
                    Some(gv) => compare(&format!("{ctx}.{k}"), wv, gv, diffs),
                    None => diffs.push(format!("{ctx}: missing in Rust")),
                }
            }
        }
        _ => {
            if want.js_stringified() != got.js_stringified() {
                diffs.push(format!(
                    "{ctx}: swift {} rust {}",
                    want.js_stringified(),
                    got.js_stringified()
                ));
            }
        }
    }
}

#[test]
fn palettes_and_real_kit_ids_match_swift() {
    let g = common_fixture::swift_fixture("performpalette");
    assert_eq!(g.get("sampleRate").and_then(JsonValue::as_f64), Some(SR));
    let mut diffs = Vec::new();
    let mut checked = 0usize;
    for sc in g.get("scenarios").and_then(JsonValue::as_array).unwrap() {
        let name = sc.get("name").and_then(JsonValue::as_str).unwrap();
        let ops = sc.get("ops").and_then(JsonValue::as_array).unwrap();
        let states = sc.get("states").and_then(JsonValue::as_array).unwrap();
        let mut st = Stepper::new(SR, true);
        st.e.start_session().expect("the empty engine is at rest");
        let row = |st: &Stepper, op: &str| {
            let mut o = djrust::json::JsonObject::new();
            o.set("op", JsonValue::str(op));
            o.set("A", deck_row(st, "A"));
            o.set("B", deck_row(st, "B"));
            JsonValue::Object(o)
        };
        compare(&format!("{name}[start]"), &states[0], &row(&st, "start"), &mut diffs);
        for (i, op) in ops.iter().enumerate() {
            let kind = op.get("op").and_then(JsonValue::as_str).unwrap();
            match kind {
                "act" => {
                    let a = DeckAction::from_json(op.get("a").unwrap()).expect("decodes");
                    st.e.perform(&a, PerformCtx::default());
                }
                "render" => st.step(op.get("blocks").and_then(JsonValue::as_f64).unwrap() as usize),
                other => panic!("unknown op {other}"),
            }
            let label = format!("{name}[{}:{}] {}", i + 1, kind, op.js_stringified());
            compare(&label, &states[i + 1], &row(&st, kind), &mut diffs);
            checked += 1;
        }
        let session = st.e.finish_session().expect("recording");
        let want = sc.get("session").and_then(JsonValue::as_str).unwrap();
        if session.canonical() != want {
            diffs.push(format!(
                "{name}: the recorded session differs:\n swift {want}\n rust  {}",
                session.canonical()
            ));
        }
        let want_cid = sc.get("cid").and_then(JsonValue::as_str).unwrap();
        if session.cid() != want_cid {
            diffs.push(format!("{name}: cid swift {want_cid} rust {}", session.cid()));
        }
        assert_eq!(
            session.frames,
            sc.get("frames").and_then(JsonValue::as_f64).unwrap() as i64,
            "{name}: frames"
        );
    }
    assert!(checked > 40, "{checked}");
    diffs.truncate(20);
    assert!(
        diffs.is_empty(),
        "{} differences from Swift, of {checked} op states:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}
