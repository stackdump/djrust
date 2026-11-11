//! Swift tests of the session model that had no Rust counterpart (package F of the parity
//! audit): the auto-mix action's decode defaults, canonical text and summaries
//! (`AutoMixRuleTests.theActionsRoundTripThroughJSON`). `session.rs` holds the clamp of bars 7 and
//! every 3; `sessions.json` holds the four actions among the 102 forms.

use djrust::journal::event::action_summary;
use djrust::json::JsonValue;
use djrust::session::action::{DeckAction, JSON_OPS};

// AutoMixRuleTests.theActionsRoundTripThroughJSON
#[test]
fn the_auto_mix_actions_round_trip_through_json() {
    let actions = [
        DeckAction::AutoMix {
            on: true,
            bars: 8,
            every: 0,
        },
        DeckAction::AutoMix {
            on: false,
            bars: 32,
            every: 16,
        },
        DeckAction::AutoMixNow,
        DeckAction::AutoMixDefer { handoff: 3 },
    ];
    for a in &actions {
        let j = a.json();
        assert_eq!(DeckAction::from_json(&j).as_ref(), Some(a), "{}", j.canonical_js());
        assert!(JSON_OPS.contains(&j.get("op").unwrap().as_str().unwrap()));
    }
    assert_eq!(
        actions[0].json().canonical_js(),
        r#"{"bars":8,"every":0,"on":true,"op":"autoMix"}"#
    );
    let decode = |text: &str| DeckAction::from_json(&JsonValue::parse(text).unwrap());
    // decode clamps to the offered lengths and phrases, with the defaults
    assert_eq!(
        decode(r#"{"op":"autoMix","on":true,"bars":7,"every":5}"#),
        Some(DeckAction::AutoMix {
            on: true,
            bars: 8,
            every: 0
        })
    );
    assert_eq!(
        decode(r#"{"op":"autoMix"}"#),
        Some(DeckAction::AutoMix {
            on: false,
            bars: 8,
            every: 0
        })
    );
    assert_eq!(decode(r#"{"op":"autoMixDefer"}"#), None, "a defer names its hand-off");
    assert_eq!(action_summary(&actions[0]), "auto-mix on · 8 bars · to the outro");
    assert_eq!(action_summary(&DeckAction::AutoMixNow), "mix now");
    assert_eq!(
        action_summary(&DeckAction::AutoMixDefer { handoff: 3 }),
        "auto-mix deferred #3"
    );
}
