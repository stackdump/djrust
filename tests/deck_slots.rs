//! `SwapStart`, `NextTrackSlot` and `LookAheadSlot` (`DeckGenerator.swift`): the compose
//! bookkeeping of a deck as plain state a host drives.
//!
//! Golden: `swift/spec.json` (`swapStarts`, `slots`), written by driving the real Swift
//! objects: the tick arithmetic over a grid, and the observable state of each slot after a
//! scripted run (the composes there are real `PreparedTrack`s; here a "track" is its spec and
//! the test plays the host that composes and hands the result back). The Swift suite has no
//! unit test of the slots themselves: `DeckNetTests.nextTrackIsComposedInTheBackgroundThenAdvanced`,
//! `DeckNetTests.nextProviderKeepsOneComing` and `WanderSwapTimingTests` exercise them through
//! a deck, and their expectations are the literals in `swift_suite_literals` below.

mod common;
use common::*;
use djrust::deck::slots::{LookAheadKey, LookAheadSlot, NextTrackSlot, SwapStart};
use djrust::json::{JsonObject, JsonValue};
use djrust::wave::RoleFilter;
use djrust::TrackSpec;

fn spec_json(s: Option<&TrackSpec>) -> JsonValue {
    s.map(TrackSpec::payload).unwrap_or(JsonValue::Null)
}

#[test]
fn swap_starts_match_swift() {
    let doc = fixture("spec");
    let rows = rows(&doc, "swapStarts");
    let nows: Vec<i64> = rows
        .iter()
        .find_map(|r| r.get("nows"))
        .unwrap()
        .ints()
        .unwrap();
    let mut d = Diffs::default();
    for r in rows.iter().filter(|r| r.get("start").is_some()) {
        let name = s(r, "start");
        let start = match name.split_once(':') {
            None if name == "top" => SwapStart::Top,
            None => SwapStart::NextBar,
            Some((_, t)) => SwapStart::Tick(t.parse().unwrap()),
        };
        let ticks = r.get("ticks").unwrap().ints().unwrap();
        let passed: Vec<bool> = r
            .get("passed")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_bool().unwrap())
            .collect();
        for (k, now) in nows.iter().enumerate() {
            d.eq(format!("{name} tick({now})"), start.tick(*now), ticks[k]);
            d.eq(
                format!("{name} passed({now})"),
                start.passed(*now),
                passed[k],
            );
        }
    }
    d.finish("SwapStart");
}

/// One `look(...)` line of the Swift trace for a `NextTrackSlot`.
fn next_step(
    op: &str,
    slot: &NextTrackSlot<TrackSpec>,
    took: Option<Option<&TrackSpec>>,
) -> String {
    let mut o = JsonObject::from_pairs([
        ("op", JsonValue::str(op)),
        ("ready", spec_json(slot.ready_spec())),
        ("pending", spec_json(slot.pending_spec())),
        ("busy", JsonValue::Bool(slot.is_busy())),
    ]);
    if let Some(t) = took {
        o.set("took", spec_json(t));
    }
    JsonValue::Object(o).js_stringified()
}

#[test]
fn the_next_track_slot_goes_as_swifts_does() {
    let doc = fixture("spec");
    let trace = rows(&doc, "slots")
        .iter()
        .find(|r| s(r, "slot") == "next")
        .unwrap();
    let want: Vec<String> = rows(trace, "steps")
        .iter()
        .map(JsonValue::js_stringified)
        .collect();
    let a = TrackSpec::new("techno", 7.0, None);
    let b = TrackSpec::new("house", 8.0, None);
    let c = TrackSpec::new("dnb", 9.0, None);
    let mut slot: NextTrackSlot<TrackSpec> = NextTrackSlot::new();
    let mut got = vec![next_step("new", &slot, None)];

    slot.request_sync(a.clone(), a.clone());
    got.push(next_step("requestSync a", &slot, None));
    slot.request_sync(b.clone(), b.clone());
    got.push(next_step("requestSync b", &slot, None));
    let took = slot.take();
    got.push(next_step("take", &slot, Some(took.as_ref())));
    let none = slot.take();
    got.push(next_step("take again", &slot, Some(none.as_ref())));
    slot.request_sync(a.clone(), a.clone());
    slot.cancel();
    got.push(next_step("requestSync a; cancel", &slot, None));

    // the latest request wins; its compose lands, the earlier ones are refused
    let (ta, _) = slot.request(a.clone());
    let (tb, _) = slot.request(b.clone());
    let (tc, _) = slot.request(c.clone());
    got.push(next_step("request a, b, c", &slot, None));
    assert_eq!(slot.complete(ta, a.clone()), Err(a.clone()));
    assert_eq!(slot.complete(tb, b.clone()), Err(b.clone()));
    assert_eq!(slot.complete(tc, c.clone()), Ok(()));
    got.push(next_step("request a, b, c: composed", &slot, None));
    slot.cancel();
    got.push(next_step("cancel", &slot, None));

    // a request made while a track is ready drops it
    slot.request_sync(a.clone(), a.clone());
    let (tb, dropped) = slot.request(b.clone());
    assert_eq!(dropped, Some(a.clone()), "the ready track is handed back");
    got.push(next_step("ready a; request b", &slot, None));
    assert_eq!(slot.complete(tb, b.clone()), Ok(()));
    got.push(next_step("composed b", &slot, None));

    assert_eq!(got.len(), want.len());
    let mut d = Diffs::default();
    for (k, (g, w)) in got.iter().zip(&want).enumerate() {
        d.eq(format!("step {k}"), g.clone(), w.clone());
    }
    d.finish("NextTrackSlot");
}

fn start_name(s: SwapStart) -> String {
    match s {
        SwapStart::Top => "top".into(),
        SwapStart::NextBar => "nextBar".into(),
        SwapStart::Tick(t) => format!("tick({t})"),
    }
}

fn look_step(
    op: &str,
    slot: &LookAheadSlot<TrackSpec>,
    taken: Option<(bool, Option<bool>, Option<&TrackSpec>)>,
) -> String {
    let key = match slot.key() {
        None => JsonValue::Null,
        Some(k) => JsonValue::Object(JsonObject::from_pairs([
            ("step", JsonValue::int(k.step)),
            ("spec", k.spec.payload()),
            ("start", JsonValue::str(start_name(k.start))),
        ])),
    };
    let mut o = JsonObject::from_pairs([
        ("op", JsonValue::str(op)),
        ("key", key),
        ("task", JsonValue::Bool(slot.task().is_some())),
        ("ready", JsonValue::Bool(slot.is_ready())),
    ]);
    if let Some((was_taken, was_ready, track)) = taken {
        o.set("taken", JsonValue::Bool(was_taken));
        if let Some(r) = was_ready {
            o.set("wasReady", JsonValue::Bool(r));
        }
        if was_taken {
            o.set("track", spec_json(track));
        }
    }
    JsonValue::Object(o).js_stringified()
}

#[test]
fn the_look_ahead_slot_goes_as_swifts_does() {
    let doc = fixture("spec");
    let trace = rows(&doc, "slots")
        .iter()
        .find(|r| s(r, "slot") == "lookAhead")
        .unwrap();
    let want: Vec<String> = rows(trace, "steps")
        .iter()
        .map(JsonValue::js_stringified)
        .collect();
    let a = TrackSpec::new("techno", 7.0, None);
    let b = TrackSpec::new("house", 8.0, None);
    let k1 = LookAheadKey {
        step: 3,
        spec: a.clone(),
        start: SwapStart::Tick(64),
        role_filter: None,
    };
    let k2 = LookAheadKey {
        step: 4,
        spec: b.clone(),
        start: SwapStart::Top,
        role_filter: Some(RoleFilter {
            mute: vec!["hats".into()],
            solo: None,
        }),
    };
    let mut slot: LookAheadSlot<TrackSpec> = LookAheadSlot::new();
    let mut got = vec![look_step("new", &slot, None)];

    let r = slot.request(k1.clone());
    assert_eq!(r.start_tick, 64, "composed starting at the key's tick");
    got.push(look_step("request k1", &slot, None));
    assert_eq!(slot.complete(r.token, a.clone()), Ok(()));
    got.push(look_step("request k1: composed", &slot, None));
    let t = slot.take(&k2);
    got.push(look_step("take k2", &slot, Some((t.is_some(), None, None))));
    assert!(t.is_none());

    let r = slot.request(k1.clone());
    slot.complete(r.token, a.clone()).unwrap();
    let t = slot.take(&k1).expect("the look-ahead for its own key");
    got.push(look_step(
        "request k1; composed; take k1",
        &slot,
        Some((true, Some(t.ready.is_some()), Some(&a))),
    ));
    assert_eq!(t.ready, Some(a.clone()));
    // the compose the move took no longer belongs to the slot
    assert_eq!(slot.complete(t.token, a.clone()), Err(a.clone()));

    let r1 = slot.request(k1.clone());
    let r2 = slot.request(k2.clone());
    assert_eq!(r2.start_tick, 0, "`.top` starts at 0");
    got.push(look_step("request k1, k2", &slot, None));
    assert_eq!(slot.complete(r1.token, a.clone()), Err(a.clone()));
    assert_eq!(slot.complete(r2.token, b.clone()), Ok(()));
    let t = slot.take(&k1);
    got.push(look_step(
        "composed k2; take k1",
        &slot,
        Some((t.is_some(), None, None)),
    ));
    assert!(t.is_none(), "another key: nothing, and the slot is empty");

    let r = slot.request(k2.clone());
    let t = slot.take(&k2).expect("taken before it finished");
    assert!(
        t.ready.is_none() && t.token == r.token,
        "wait for the compose"
    );
    got.push(look_step(
        "request k2; take k2 at once",
        &slot,
        Some((true, None, Some(&b))),
    ));

    let r = slot.request(k1.clone());
    slot.complete(r.token, a.clone()).unwrap();
    assert_eq!(slot.drop_held(), Some(a.clone()));
    got.push(look_step("composed k1; drop", &slot, None));

    assert_eq!(got.len(), want.len());
    let mut d = Diffs::default();
    for (k, (g, w)) in got.iter().zip(&want).enumerate() {
        d.eq(format!("step {k}"), g.clone(), w.clone());
    }
    d.finish("LookAheadSlot");
}

#[test]
fn look_ahead_keys_compare_as_swifts_do() {
    let doc = fixture("spec");
    let want: Vec<bool> = rows(&doc, "slots")
        .iter()
        .find(|r| s(r, "slot") == "lookAhead")
        .unwrap()
        .get("keysEqual")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_bool().unwrap())
        .collect();
    let a = TrackSpec::new("techno", 7.0, None);
    let b = TrackSpec::new("house", 8.0, None);
    let key = |step, spec: &TrackSpec, start, f: Option<RoleFilter>| LookAheadKey {
        step,
        spec: spec.clone(),
        start,
        role_filter: f,
    };
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let f1 = key(
        1,
        &a,
        SwapStart::Top,
        Some(RoleFilter {
            mute: names(&["a", "b"]),
            solo: None,
        }),
    );
    let f2 = key(
        1,
        &a,
        SwapStart::Top,
        Some(RoleFilter {
            mute: names(&["b", "a"]),
            solo: None,
        }),
    );
    let f3 = key(
        1,
        &a,
        SwapStart::Top,
        Some(RoleFilter {
            mute: names(&["a", "b"]),
            solo: Some(vec![]),
        }),
    );
    let f4 = key(1, &a, SwapStart::Top, None);
    let f5 = key(1, &a, SwapStart::NextBar, None);
    let f6 = key(2, &a, SwapStart::Top, None);
    let f7 = key(1, &b, SwapStart::Top, None);
    let got = vec![
        f1 == f2,
        f1 == f3,
        f1 == f4,
        f4 == f4,
        f4 == f5,
        f4 == f6,
        f4 == f7,
    ];
    assert_eq!(got, want);
}

/// The expectations the Swift suite puts on these through a deck.
#[test]
fn swift_suite_literals() {
    // DeckNetTests.nextTrackIsComposedInTheBackgroundThenAdvanced: nothing queued is nothing
    // ready and nothing to advance to; a queued spec is ready once composed and is taken once
    let mut next: NextTrackSlot<u32> = NextTrackSlot::new();
    assert!(next.ready_spec().is_none() && next.take().is_none() && !next.is_busy());
    let spec = TrackSpec::new("techno", 5.0, Some("minimal"));
    let (token, _) = next.request(spec.clone());
    assert!(next.pending_spec() == Some(&spec) && next.ready_spec().is_none() && next.is_busy());
    assert_eq!(next.complete(token, 1), Ok(()));
    assert_eq!(next.ready_spec(), Some(&spec));
    assert_eq!(next.take(), Some(1));
    assert!(next.ready_spec().is_none(), "taken");
    // DeckNetTests.nextProviderKeepsOneComing: busy while composing and while ready, so the
    // deck asks for no second one
    let (token, _) = next.request(spec.clone());
    assert!(next.is_busy());
    next.complete(token, 2).unwrap();
    assert!(next.is_busy(), "ready counts as busy");

    // WanderSwapTimingTests.aSwapComposedPastItsBoundaryStartsAtTheNextBar and
    // anOnTimeSwapStillStartsAtItsBoundary: a swap planned for the phrase boundary at tick 32 and composed
    // when the deck is at tick 40 has been passed, one planned for 48 has not; the bar after
    // tick 40 is 48 (`generate` re-prepares such a track to start there)
    assert!(SwapStart::Tick(32).passed(40));
    assert!(!SwapStart::Tick(48).passed(40));
    assert_eq!(SwapStart::NextBar.tick(40), 48);
    assert_eq!(SwapStart::Top.tick(40), 0);
    // `wanderSwapStart(bar:)` is `.tick((bar + 1) * 16)`, or `.top` on a loop; bar 3 -> 64
    assert_eq!(SwapStart::Tick((3 + 1) * 16), SwapStart::Tick(64));
    // a tick of 0 is the top, never "passed"
    assert!(!SwapStart::Tick(0).passed(100));
}
