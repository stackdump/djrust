//! The `minecraftEvents` host method on a real (offline) engine (`AppControl.controlMinecraftEvents`,
//! `MinecraftEvents.swift`): gameplay fires Hits, each event once however often the window is
//! re-read, a stinger at most once a bar, on the request's deck or the focus deck.
//!
//! The events are a real `mc_recent_events` window (`mc-recent-events.json`). The pure half
//! (parse, map, feed, report) is held by `djengine/tests/deck_minecraft.rs` against the Swift
//! suite's literals; no Swift test drives the handler itself (`DJViewsTests` has none), so
//! these are Rust tests of what the Swift handler says: a mapped stinger is `Deck.stinger`, so a
//! Hit assigned a loop launches it on the loop row (`StingerLaunch`).

mod common_row;

use common_row::*;
use djrust::deck::loops::LoopState;
use djrust::deck::minecraft::{MinecraftEventFeed, MinecraftEventMap};
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::StingerAssignment;
use djrust::sound::stinger::{AssignedLoop, AssignmentKind};

fn window() -> JsonValue {
    let path = format!(
        "{}/tests/fixtures/js/mc-recent-events.json",
        env!("CARGO_MANIFEST_DIR")
    );
    JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn request(events: JsonValue, deck: Option<&str>, prime: bool) -> JsonValue {
    let mut o = JsonObject::from_pairs([("events", events)]);
    if let Some(d) = deck {
        o.set("deck", JsonValue::str(d));
    }
    if prime {
        o.set("prime", JsonValue::Bool(true));
    }
    o.into()
}

fn feed() -> MinecraftEventFeed {
    MinecraftEventFeed::new(MinecraftEventMap::bundled())
}

fn names(reply: &JsonValue, key: &str) -> Vec<String> {
    reply
        .get(key)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.get("event").unwrap().as_str().unwrap().to_string())
        .collect()
}

fn reasons(reply: &JsonValue) -> Vec<String> {
    reply
        .get("skipped")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.get("reason").unwrap().as_str().unwrap().to_string())
        .collect()
}

/// The Hit nets' indices in deck A's graph, as the Swift golden recorded them.
fn hit_nets() -> Vec<usize> {
    let path = format!(
        "{}/tests/fixtures/swift/stinger.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let g = JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    g.get("hits").unwrap().get("netsA").unwrap().as_array().unwrap().iter()
        .map(|v| v.as_i64().unwrap() as usize)
        .collect()
}

fn ringing(rig: &Rig) -> Vec<bool> {
    let m = &rig.e.deck("A").unwrap().mirror;
    hit_nets().iter().map(|&i| !m.is_net_muted(i)).collect()
}

/// Deck A at 120 bpm playing, every Hit a fallback (its stinger net).
fn rig() -> Rig {
    let mut rig = Rig::new();
    rig.net_deck(120.0);
    rig.e.deck_mut("A").unwrap().project.as_mut().unwrap().stingers = Vec::new();
    rig.run_to(2 * BAR + 1_024);
    rig
}

#[test]
fn a_window_fires_its_first_stinger_and_each_event_once() {
    let mut rig = rig();
    let mut feed = feed();
    assert_eq!(ringing(&rig), [false; 4]);
    let reply = rig.e.minecraft_events(&mut feed, &request(window(), Some("A"), false)).unwrap();
    assert_eq!(reply.get("deck").and_then(JsonValue::as_str), Some("A"));
    // oldest first: the chat is unmapped, the join is Hit 3, the rest wait for the next bar
    assert_eq!(names(&reply, "fired"), ["minecraft:notification/players/joined"]);
    assert_eq!(
        reasons(&reply),
        [
            "unmapped".to_string(),
            "a stinger already fired in bar 2".into(),
            "a stinger already fired in bar 2".into(),
            "a stinger already fired in bar 2".into()
        ]
    );
    assert_eq!(reply.get("cursor").and_then(JsonValue::as_str), Some("2026-10-18T16:16:23.845Z"));
    let action = reply.get("fired").unwrap().as_array().unwrap()[0].get("action").unwrap();
    assert_eq!(action.js_stringified(), r#"{"op":"stinger","deck":"A","n":2}"#);
    // the Hit sounds: Hit 3's net rings
    rig.run(4_096);
    assert_eq!(ringing(&rig), [false, false, true, false]);
    // the same window again fires nothing, and says so
    let again = rig.e.minecraft_events(&mut feed, &request(window(), Some("A"), false)).unwrap();
    assert!(names(&again, "fired").is_empty() && reasons(&again).is_empty());
}

#[test]
fn a_stinger_a_bar_per_deck_and_priming_takes_the_backlog_without_playing_it() {
    let mut rig = rig();
    let mut feed = feed();
    let primed = rig.e.minecraft_events(&mut feed, &request(window(), None, true)).unwrap();
    assert!(names(&primed, "fired").is_empty());
    assert_eq!(reasons(&primed), vec!["primed"; 5]);
    rig.run(4_096);
    assert_eq!(ringing(&rig), [false; 4], "nothing played");
    // a new death in a new bar plays Hit 1 on the focus deck (A)
    rig.run_to(4 * BAR);
    let one = JsonValue::parse(r#"[{"at":"2026-10-18T16:30:00.000Z","source":"log","event":"log:death","params":{"player":"x"}}]"#).unwrap();
    let reply = rig.e.minecraft_events(&mut feed, &request(one, None, false)).unwrap();
    assert_eq!(reply.get("deck").and_then(JsonValue::as_str), Some("A"));
    assert_eq!(names(&reply, "fired"), ["log:death"]);
    assert_eq!(reply.get("fired").unwrap().as_array().unwrap()[0].get("player").and_then(JsonValue::as_str), Some("x"));
    rig.run(4_096);
    assert_eq!(ringing(&rig), [true, false, false, false]);
}

#[test]
fn a_mapped_stinger_launches_a_hits_loop_on_the_row() {
    let mut rig = rig();
    let x = test_clip(1);
    rig.e.restore_clips(std::slice::from_ref(&x));
    rig.e.deck_mut("A").unwrap().project.as_mut().unwrap().stingers = vec![
        StingerAssignment::fallback(0),
        StingerAssignment {
            slot: 1,
            kind: AssignmentKind::Loop,
            candidate: Some("test-loop".into()),
            label: "Test loop".into(),
            option: None,
            macro_id: None,
            bars: None,
            loop_: Some(AssignedLoop {
                path: None,
                clip_id: Some(x.id.clone()),
                name: "Test loop".into(),
                beats: 4,
                bpm: 120.0,
            }),
            local_only: false,
        },
    ];
    // an advancement is Hit 2
    let adv = JsonValue::parse(r#"[{"at":"2026-10-18T16:30:00.000Z","source":"log","event":"log:advancement","params":{"player":"x"}}]"#).unwrap();
    let mut feed = feed();
    let reply = rig.e.minecraft_events(&mut feed, &request(adv, Some("A"), false)).unwrap();
    assert_eq!(names(&reply, "fired"), ["log:advancement"]);
    assert_eq!(rig.e.loop_state(&x.id), LoopState::Starting, "the Hit's loop is on its way in");
    assert_eq!(ringing(&rig), [false; 4], "the loop, not the stinger net");
}

#[test]
fn a_sample_deck_consumes_the_event_with_the_reason() {
    let mut rig = rig();
    assert!(rig.e.load_demo("B"));
    let mut feed = feed();
    let one = JsonValue::parse(r#"[{"at":"2026-10-18T16:30:00.000Z","source":"log","event":"log:death","params":{}}]"#).unwrap();
    let reply = rig.e.minecraft_events(&mut feed, &request(one, Some("B"), false)).unwrap();
    assert!(names(&reply, "fired").is_empty());
    assert_eq!(reasons(&reply), ["deck B is not playing a net"]);
}

#[test]
fn bad_requests_say_what_is_wrong() {
    let mut rig = rig();
    let mut feed = feed();
    let none = JsonValue::Object(JsonObject::new());
    assert_eq!(
        rig.e.minecraft_events(&mut feed, &none).unwrap_err(),
        "`events` must be mc_recent_events' array (or its result)"
    );
    let bad = request(JsonValue::parse(r#"{"x":1}"#).unwrap(), None, false);
    assert!(rig.e.minecraft_events(&mut feed, &bad).unwrap_err().starts_with("expected mc_recent_events"));
    let nodeck = request(window(), Some("Z"), false);
    assert_eq!(
        rig.e.minecraft_events(&mut feed, &nodeck).unwrap_err(),
        "no deck \"Z\" (decks: A, B)"
    );
    // a refused request takes nothing: the window still fires
    let ok = rig.e.minecraft_events(&mut feed, &request(window(), Some("A"), false)).unwrap();
    assert_eq!(names(&ok, "fired").len(), 1);
}
