//! The `inserts` live op: the strip's insert chain driven as the booth's registry drives it
//! (`effect` + `on`, `param` + `value`), so a host with only live ops (the browser's `Dj`)
//! plays the same filter, resonance and all.

use djrust::fx::insert::{InsertKind, InsertParam};
use djrust::host::deck::{engine, EngineConfig};
use djrust::json::JsonValue;
use djrust::session::arrangement::LIVE_METHODS;

fn v(s: &str) -> JsonValue {
    JsonValue::parse(s).unwrap()
}

#[test]
fn a_switch_and_a_knob_reach_the_chain() {
    let (mut e, _r) = engine(EngineConfig::new(44_100.0));
    e.apply_live_op("inserts", &v(r#"{"target":"A","effect":"filter","on":true}"#)).unwrap();
    e.apply_live_op("inserts", &v(r#"{"target":"A","param":"filter.sweep","value":0.2}"#)).unwrap();
    e.apply_live_op("inserts", &v(r#"{"target":"A","param":"filter.res","value":0.4}"#)).unwrap();
    let s = e.insert_settings("A").unwrap();
    assert!(s.is_on(InsertKind::Filter));
    assert!((s.get(InsertParam::FilterSweep) - 0.2).abs() < 1e-9);
    assert!((s.get(InsertParam::FilterRes) - 0.4).abs() < 1e-9);
    // the master, by either spelling; both at once in one op
    e.apply_live_op("inserts", &v(r#"{"target":"Master","effect":"filter","on":true,"param":"filter.res","value":0.9}"#)).unwrap();
    let m = e.insert_settings("master").unwrap();
    assert!(m.is_on(InsertKind::Filter));
    assert!((m.get(InsertParam::FilterRes) - 0.9).abs() < 1e-9);
    // a knob clamps to 0..1, as set_insert_param does
    e.apply_live_op("inserts", &v(r#"{"target":"B","param":"filter.res","value":3}"#)).unwrap();
    assert_eq!(e.insert_settings("B").unwrap().get(InsertParam::FilterRes), 1.0);
}

#[test]
fn what_it_refuses_it_says() {
    let (mut e, _r) = engine(EngineConfig::new(44_100.0));
    for (p, why) in [
        (r#"{"param":"filter.res","value":0.4}"#, "target"),
        (r#"{"target":"Z","param":"filter.res","value":0.4}"#, "no strip"),
        (r#"{"target":"A","param":"filter.q","value":0.4}"#, "no knob"),
        (r#"{"target":"A","param":"filter.res"}"#, "value"),
        (r#"{"target":"A","effect":"phaser","on":true}"#, "no effect"),
        (r#"{"target":"A","effect":"filter"}"#, "on"),
        (r#"{"target":"A"}"#, "effect + on"),
    ] {
        let err = e.apply_live_op("inserts", &v(p)).unwrap_err();
        assert!(err.contains(why), "{p}: {err}");
    }
}

#[test]
fn an_arrangement_may_carry_it() {
    assert!(LIVE_METHODS.contains(&"inserts"));
}
