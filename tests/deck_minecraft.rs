//! Gameplay → stingers (`MinecraftEvents.swift`), ported from the second half of
//! `MinecraftTests.swift` with its literals. The events are a real `mc_recent_events` window
//! (`fixtures/js/mc-recent-events.json`, copied from the Swift repo's test fixtures).

use djrust::deck::minecraft::*;
use djrust::json::{JsonObject, JsonValue};

fn fixture() -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/js/mc-recent-events.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn map() -> MinecraftEventMap {
    MinecraftEventMap::bundled()
}

fn at(s: &str, e: &str) -> MinecraftEvent {
    MinecraftEvent::new(s, e)
}

fn stinger(deck: &str, n: i64) -> MappedAction {
    MappedAction::Stinger {
        deck: deck.into(),
        n,
    }
}

#[test]
fn recent_events_parse_from_the_tool_and_its_result() {
    let text = fixture();
    let events = MinecraftEvent::parse(&text).unwrap();
    assert_eq!(
        events.iter().map(|e| e.event.as_str()).collect::<Vec<_>>(),
        [
            "log:chat",
            "minecraft:notification/players/joined",
            "log:advancement",
            "log:death",
            "log:death"
        ]
    );
    assert!(events
        .iter()
        .all(|e| e.player().as_deref() == Some("stackdump")));
    assert_eq!(events[1].source, "rpc");
    // the same array inside an MCP result (val_call's strings, or text parts)
    let wrapped = JsonValue::Object(JsonObject::from_pairs([(
        "content",
        JsonValue::Array(vec![JsonValue::str(text.as_str())]),
    )]));
    assert_eq!(MinecraftEvent::parse_json(&wrapped).unwrap(), events);
    let parts = JsonValue::Object(JsonObject::from_pairs([(
        "content",
        JsonValue::Array(vec![JsonValue::Object(JsonObject::from_pairs([
            ("type", JsonValue::str("text")),
            ("text", JsonValue::str(text.as_str())),
        ]))]),
    )]));
    assert_eq!(MinecraftEvent::parse_json(&parts).unwrap(), events);
    // an `{events: […]}` object and a `{result: …}` wrapper
    let obj = format!("{{\"events\":{text}}}");
    assert_eq!(MinecraftEvent::parse(&obj).unwrap(), events);
    assert!(MinecraftEvent::parse("[]").unwrap().is_empty());
    assert!(matches!(
        MinecraftEvent::parse(r#"{"x":1}"#),
        Err(MinecraftEventError::Shape(_))
    ));
}

#[test]
fn events_map_to_stingers() {
    let map = map();
    assert_eq!(
        map.action(&at("t", "log:death"), "A"),
        Some(stinger("A", 0))
    );
    assert_eq!(
        map.action(&at("t", "log:advancement"), "B"),
        Some(stinger("B", 1))
    );
    assert_eq!(
        map.action(&at("t", "minecraft:notification/players/joined"), "A"),
        Some(stinger("A", 2))
    );
    assert_eq!(map.action(&at("t", "log:chat"), "A"), None);
    assert_eq!(map.actions.len(), 3);
}

#[test]
fn a_map_from_json_overrides() {
    let over = MinecraftEventMap::from_json(
        &JsonValue::parse(r#"{"events":{"log:chat":{"op":"stinger","n":3}}}"#).unwrap(),
    );
    assert_eq!(
        over.action(&at("t", "log:chat"), "A"),
        Some(stinger("A", 3))
    );
    assert_eq!(over.action(&at("t", "log:death"), "A"), None);
    // an entry without an op is dropped
    let bad =
        MinecraftEventMap::from_json(&JsonValue::parse(r#"{"events":{"x":{"n":1}}}"#).unwrap());
    assert!(bad.actions.is_empty());
}

/// `DeckAction(json:)` is nil for an op that is not an action, and the event is then unmapped.
#[test]
fn an_op_that_is_not_an_action_maps_to_nothing() {
    let m = MinecraftEventMap::from_json(
        &JsonValue::parse(r#"{"events":{"log:chat":{"op":"nope"},"log:death":{"op":"panic"}}}"#).unwrap(),
    );
    assert_eq!(m.action(&at("t", "log:chat"), "A"), None);
    assert!(matches!(m.action(&at("t", "log:death"), "A"), Some(MappedAction::Other(_))));
}

/// A re-read window fires nothing twice (events sharing a millisecond included), and a deck
/// takes one stinger a bar.
#[test]
fn a_repeated_window_fires_each_event_once() {
    let events = MinecraftEvent::parse(&fixture()).unwrap();
    let mut feed = MinecraftEventFeed::new(map());
    let first = feed.take(&events[..3], "A", Some(0), true, false);
    assert_eq!(
        first.0.iter().map(|f| f.action.clone()).collect::<Vec<_>>(),
        [stinger("A", 2)]
    );
    assert_eq!(
        first
            .1
            .iter()
            .map(|s| s.reason.as_str())
            .collect::<Vec<_>>(),
        ["unmapped", "a stinger already fired in bar 0"]
    );
    assert_eq!(feed.cursor(), Some("2026-10-18T16:16:22.845Z"));
    // the next poll overlaps: only the new death at the cursor's millisecond and the one after
    let second = feed.take(&events, "A", Some(1), true, false);
    assert_eq!(
        second
            .0
            .iter()
            .map(|f| f.action.clone())
            .collect::<Vec<_>>(),
        [stinger("A", 0)]
    );
    assert_eq!(second.0[0].event.at, "2026-10-18T16:16:22.845Z");
    assert_eq!(
        second
            .1
            .iter()
            .map(|s| s.reason.as_str())
            .collect::<Vec<_>>(),
        ["a stinger already fired in bar 1"]
    );
    // the same window again: nothing new
    let third = feed.take(&events, "A", Some(2), true, false);
    assert!(third.0.is_empty() && third.1.is_empty());
    // the bar limit is per deck
    let mut feed_b = MinecraftEventFeed::new(map());
    let deaths: Vec<MinecraftEvent> = events
        .iter()
        .filter(|e| e.event == "log:death")
        .cloned()
        .collect();
    let b = feed_b.take(&deaths, "B", Some(7), true, false);
    assert_eq!(b.0.len(), 1);
    feed_b.take(&[], "A", Some(7), true, false);
    let later = at("2026-10-18T16:17:00.000Z", "log:death");
    assert_eq!(
        feed_b.take(&[later], "A", Some(7), true, false).0.len(),
        1,
        "deck A's bar 7 is free"
    );
}

#[test]
fn priming_takes_the_backlog_without_playing_it() {
    let events = MinecraftEvent::parse(&fixture()).unwrap();
    let mut feed = MinecraftEventFeed::new(map());
    let primed = feed.take(&events, "A", Some(0), true, true);
    assert!(primed.0.is_empty() && primed.1.len() == events.len());
    assert!(feed.take(&events, "A", Some(1), true, false).0.is_empty());
    // a sample deck can't take stingers: the event is consumed with the reason
    let next = at("2026-10-18T16:20:00.000Z", "log:advancement");
    let s = feed.take(&[next], "A", Some(2), false, false);
    assert!(s.0.is_empty() && s.1[0].reason.contains("net"));
    // without a beat grid: one stinger per poll
    let two = [
        at("2026-10-18T16:21:00.000Z", "log:death"),
        at("2026-10-18T16:21:01.000Z", "log:death"),
    ];
    assert_eq!(feed.take(&two, "A", None, true, false).0.len(), 1);
}

/// `djctl minecraft events`: stdin → the request's params, the reply → lines.
#[test]
fn the_command_builds_the_request() {
    let p = events_command_params(&fixture(), Some("B"), false).unwrap();
    assert_eq!(p.get("deck").and_then(JsonValue::as_str), Some("B"));
    assert_eq!(
        p.get("events")
            .and_then(JsonValue::as_array)
            .map(|a| a.len()),
        Some(5)
    );
    assert!(p.get("prime").is_none());
    let back = MinecraftEvent::parse_json(p.get("events").unwrap()).unwrap();
    assert_eq!(back, MinecraftEvent::parse(&fixture()).unwrap());
    let q = events_command_params("[]", None, true).unwrap();
    assert!(q.get("deck").is_none() && q.get("prime").and_then(JsonValue::as_bool) == Some(true));
    let mut ev = at("t", "log:death");
    ev.params = JsonValue::Object(JsonObject::from_pairs([("player", JsonValue::str("x"))]));
    let reply = MinecraftEventFeed::report(
        "A",
        &[Fired {
            event: ev,
            action: stinger("A", 0),
        }],
        &[],
        Some("t"),
    );
    let text = describe_reply(&reply);
    assert!(text.contains("log:death (x) → A"));
    assert!(text.ends_with("fired 1, skipped 0"));
    // the action prints as DeckAction.json: op first
    assert!(
        text.contains(r#"{"op":"stinger","deck":"A","n":0}"#),
        "{text}"
    );
}
