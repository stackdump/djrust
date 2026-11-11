//! The read-back views a control surface uses (`djrust::host::deck::control_json`): the socket's
//! shortcuts become the `DeckAction`s they name, go through `Engine::perform`, and the views
//! show what they changed. Not a Swift port: djapp's socket (docs/CONTROL.md) is beyond Swift.

mod common_row;

use common_row::*;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::host::deck::control_json::{
    action_for, auto_mix_op, loop_op, wander_op, AUTO_MIX_OPS, LOOP_OPS, WANDER_OPS,
};
use djrust::host::deck::perform::{Applied, PerformCtx};

fn shortcut(rig: &mut Rig, action: &str, op: &str, params: &str) -> Applied {
    let p = JsonValue::parse(params).unwrap();
    let a = action_for(action, op, &p);
    let act = DeckAction::from_json(&a).unwrap_or_else(|| panic!("not an action: {a:?}"));
    rig.e.perform(&act, PerformCtx::default())
}

fn loop_shortcut(rig: &mut Rig, action: &str, params: &str) -> Applied {
    let op = loop_op(action).unwrap();
    shortcut(rig, action, op, params)
}

fn get<'a>(v: &'a JsonValue, path: &[&str]) -> &'a JsonValue {
    path.iter().fold(v, |v, k| v.get(k).unwrap_or_else(|| panic!("no {k} in {v:?}")))
}

fn clip<'a>(row: &'a JsonValue, id: &str) -> &'a JsonValue {
    match row.get("clips") {
        Some(JsonValue::Array(cs)) => cs
            .iter()
            .find(|c| c.get("id").and_then(JsonValue::as_str) == Some(id))
            .expect("the clip is listed"),
        _ => panic!("no clips"),
    }
}

#[test]
fn every_shortcut_names_a_deck_action_op() {
    for (action, op) in LOOP_OPS.iter().chain(&WANDER_OPS).chain(&AUTO_MIX_OPS) {
        assert!(!action.is_empty() && !op.is_empty());
        // the op is one the session codec knows (with the fields it insists on)
        let probe = JsonValue::parse(
            r#"{"deck":"A","id":"x","name":"n","bars":4,"dimension":"key","handoff":1,"fx":{}}"#,
        )
        .unwrap();
        assert!(
            DeckAction::from_json(&action_for(action, op, &probe)).is_some(),
            "{action} -> {op}"
        );
    }
    let mut names: Vec<&str> = LOOP_OPS.iter().map(|x| x.0).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), LOOP_OPS.len());
    assert_eq!(wander_op("pace"), Some("wanderPace"));
    assert_eq!(auto_mix_op("now"), Some("autoMixNow"));
    assert_eq!(loop_op("to_deck"), None, "clips onto A/B are not this table's");
}

#[test]
fn on_and_off_force_the_switch_and_action_is_dropped() {
    let p = JsonValue::parse(r#"{"action":"off","deck":"A","on":true}"#).unwrap();
    let a = action_for("off", "wander", &p);
    assert_eq!(a.get("on"), Some(&JsonValue::Bool(false)));
    assert_eq!(a.get("op").and_then(JsonValue::as_str), Some("wander"));
    assert!(a.get("action").is_none());
}

#[test]
fn the_loop_row_view_shows_what_the_shortcuts_changed() {
    let mut rig = Rig::new();
    let (one, two) = rig.two_takes();
    rig.settle();

    let row = rig.e.loop_row_json();
    assert_eq!(get(&row, &["row", "key_lock"]), &JsonValue::Bool(rig.e.loop_key_lock()));
    let c = clip(&row, &one);
    assert_eq!(c.get("quantum"), Some(&JsonValue::Null), "follows the row");
    assert_eq!(c.get("key_lock"), Some(&JsonValue::Null));
    assert_eq!(c.get("steps"), Some(&JsonValue::Null), "a take, not a sequence");

    assert!(!matches!(
        loop_shortcut(&mut rig, "quantize", &format!(r#"{{"id":"{one}","beats":16}}"#)),
        Applied::Ignored(_)
    ));
    loop_shortcut(&mut rig, "key_lock", &format!(r#"{{"id":"{one}","on":true}}"#));
    loop_shortcut(&mut rig, "row_quantize", r#"{"beats":1}"#);
    loop_shortcut(&mut rig, "fx", &format!(r#"{{"id":"{two}","fx":{{"drive":6}}}}"#));
    loop_shortcut(&mut rig, "rename", &format!(r#"{{"id":"{two}","title":"riff"}}"#));
    loop_shortcut(&mut rig, "scene_save", r#"{"name":"intro"}"#);

    let row = rig.e.loop_row_json();
    let c = clip(&row, &one);
    assert_eq!(c.get("quantum"), Some(&JsonValue::int(16)));
    assert_eq!(c.get("key_lock"), Some(&JsonValue::Bool(true)));
    assert_eq!(get(&row, &["row", "quantum"]), &JsonValue::int(1));
    let c2 = clip(&row, &two);
    assert_eq!(c2.get("title").and_then(JsonValue::as_str), Some("riff"));
    assert_eq!(c2.get("fx"), Some(&rig.e.clip_fx(&two).json()));
    match row.get("scenes") {
        Some(JsonValue::Array(s)) => {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].get("name").and_then(JsonValue::as_str), Some("intro"));
        }
        other => panic!("{other:?}"),
    }

    // a crop takes the clip's place (cropClip) with its settings; removing one lists it as
    // removed, restore brings it back
    loop_shortcut(&mut rig, "crop", &format!(r#"{{"id":"{one}","from":0,"to":2}}"#));
    let row = rig.e.loop_row_json();
    let first = match row.get("clips") {
        Some(JsonValue::Array(c)) => {
            assert_eq!(c.len(), 2, "in place");
            c[0].clone()
        }
        other => panic!("{other:?}"),
    };
    assert_ne!(first.get("id").and_then(JsonValue::as_str), Some(one.as_str()));
    assert_eq!(first.get("beats"), Some(&JsonValue::int(2)));
    assert_eq!(first.get("quantum"), Some(&JsonValue::int(16)), "settings carried");
    rig.e.remove_clip(&two);
    let row = rig.e.loop_row_json();
    match row.get("removed") {
        Some(JsonValue::Array(r)) => {
            assert_eq!(r[0].get("id").and_then(JsonValue::as_str), Some(two.as_str()))
        }
        other => panic!("{other:?}"),
    }
    loop_shortcut(&mut rig, "restore", &format!(r#"{{"id":"{two}"}}"#));
    clip(&rig.e.loop_row_json(), &two);
}

#[test]
fn the_deck_and_walk_views_follow_their_actions() {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    let s = rig.e.deck_sound_json("A");
    for k in ["kit", "palette", "voices", "macros", "wander", "auto_gain", "auto_gain_db"] {
        assert!(s.get(k).is_some(), "{k}");
    }
    assert_eq!(rig.e.deck_sound_json("Z"), JsonValue::Null);

    shortcut(&mut rig, "on", "wander", r#"{"deck":"A"}"#);
    shortcut(&mut rig, "pace", "wanderPace", r#"{"deck":"A","bars":16}"#);
    let w = rig.e.wander_all_json();
    assert_eq!(get(&w, &["A", "on"]), &JsonValue::Bool(true));
    assert_eq!(get(&w, &["A", "pace"]), &JsonValue::int(16));
    assert_eq!(get(&rig.e.deck_sound_json("A"), &["wander", "on"]), &JsonValue::Bool(true));
    shortcut(&mut rig, "off", "wander", r#"{"deck":"A"}"#);
    assert_eq!(
        get(&rig.e.wander_all_json(), &["A", "on"]),
        &JsonValue::Bool(false)
    );

    shortcut(&mut rig, "set", "autoMix", r#"{"on":true,"bars":16}"#);
    let m = rig.e.auto_mix_view_json();
    assert!(m.get("status").and_then(JsonValue::as_str).is_some());
    assert_eq!(m.get("state"), Some(&rig.e.auto_mix_json()));
    shortcut(&mut rig, "off", "autoMix", r#"{}"#);
}
