//! A loop-row clip onto deck A or B from the hub (`Engine::clip_to_deck`, the `clipToDeck`
//! target, the socket's `loops to_deck`): the engine's `play_clip`, reached by a slot, an id or
//! "the newest", recorded as the session's `clip` action.

mod common_row;

use std::collections::BTreeMap;

use common_row::*;
use djrust::control::controller::{Action, SurfaceHost};
use djrust::session::action::DeckAction;
use djrust::session::arrangement::ArrangeOp;

fn row_of(n: usize) -> (Rig, Vec<String>) {
    let mut rig = Rig::new();
    let ids = (1..=n)
        .map(|k| {
            let c = test_clip(k);
            let id = c.id.clone();
            rig.e.loops.row.clips.push(c);
            id
        })
        .collect();
    (rig, ids)
}

fn target(slot: Option<i64>, deck: &str) -> Action {
    Action::Target {
        id: "clipToDeck".into(),
        deck: Some(deck.into()),
        slot,
        index: 0,
        value: 1.0,
        pressed: true,
        args: BTreeMap::new(),
    }
}

#[test]
fn a_bound_pad_moves_a_row_clip_onto_its_deck() {
    let (mut rig, ids) = row_of(3);
    rig.e.trace = Some(Vec::new());
    SurfaceHost::perform(&mut rig.e, target(Some(1), "B"));
    assert_eq!(rig.e.deck("B").unwrap().clip_id.as_deref(), Some(ids[1].as_str()));
    // recorded as the session action, so the action log and an arrangement have it
    let traced = rig.e.trace.take().unwrap();
    assert!(
        traced.iter().any(|t| t.ops
            == [ArrangeOp::Deck(DeckAction::Clip {
                deck: "B".into(),
                id: ids[1].clone()
            })]),
        "{:?}",
        traced.iter().map(|t| &t.entry.text).collect::<Vec<_>>()
    );
    // a release does nothing
    let mut up = target(Some(0), "A");
    if let Action::Target { pressed, .. } = &mut up {
        *pressed = false;
    }
    SurfaceHost::perform(&mut rig.e, up);
    assert_eq!(rig.e.deck("A").unwrap().clip_id, None);
}

#[test]
fn by_id_by_slot_or_the_newest() {
    let (mut rig, ids) = row_of(3);
    assert_eq!(rig.e.clip_to_deck(None, None, "A"), Ok(ids[2].clone()));
    assert_eq!(rig.e.deck("A").unwrap().clip_id.as_deref(), Some(ids[2].as_str()));
    assert_eq!(rig.e.clip_to_deck(Some(&ids[0]), None, "B"), Ok(ids[0].clone()));
    assert_eq!(rig.e.deck("B").unwrap().clip_id.as_deref(), Some(ids[0].as_str()));
    assert!(rig.e.clip_to_deck(None, Some(9), "A").unwrap_err().contains("slot 9"));
    assert!(rig.e.clip_to_deck(Some("nope"), None, "A").is_err());
    assert!(rig.e.clip_to_deck(None, Some(0), "C").unwrap_err().contains("no deck"));
    // a library loop that isn't there
    assert!(rig.e.library_to_deck("nope", "A").is_err());
    let (mut empty, _) = row_of(0);
    assert_eq!(empty.e.clip_to_deck(None, None, "A"), Err("the loop row is empty".into()));
}

#[test]
fn a_clip_looping_under_the_mixer_moves_off_the_loop_channel() {
    let (mut rig, ids) = row_of(1);
    rig.e.set_looping(&ids[0], true);
    rig.run(BAR + BAR / 2);
    assert!(!rig.e.looping_clips().is_empty());
    rig.e.clip_to_deck(None, Some(0), "A").unwrap();
    assert!(rig.e.looping_clips().is_empty());
    assert_eq!(rig.e.deck("A").unwrap().clip_id.as_deref(), Some(ids[0].as_str()));
}
