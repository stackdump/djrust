//! Live keys on a deck (docs/MPK.md, "Keys"): a key reaches the deck's live voice through the
//! controller's `keys` target, the route (mode, deck, quantise) decides where and when, and every
//! key is a `keys` live op, so an arrangement keeps it and plays it again.

mod common_track;

use common_track::Stepper;
use djrust::control::controller::{Action, KeyMode, SurfaceHost};
use djrust::json::JsonValue;
use djrust::session::arrangement::ArrangeOp;
use djrust::TrackSpec;
use djrust::host::deck::loop_pads::Quant;

const RATE: f64 = 48_000.0;

fn playing() -> Stepper {
    let mut st = Stepper::new(RATE, false);
    assert!(st.e.generate("A", &TrackSpec::new("techno", 42.0, Some("standard"))));
    assert!(st.e.generate("B", &TrackSpec::new("house", 7.0, Some("standard"))));
    st.e.play("A");
    st.e.play("B");
    assert!(st.until_installed("A"));
    assert!(st.until_installed("B"));
    st.step(400);
    st
}

fn key_down(note: usize, deck: &str, quant: Option<&str>) -> Action {
    Action::KeyDown {
        deck: deck.into(),
        index: note,
        velocity: 100,
        quant: quant.map(str::to_string),
    }
}

fn act(e: &mut djrust::host::deck::Engine, a: Action) {
    SurfaceHost::perform(e, a)
}

fn held(st: &Stepper, deck: &str) -> usize {
    st.e.deck(deck).unwrap().held_keys.len()
}

#[test]
fn a_key_holds_on_the_deck_until_it_comes_up() {
    let mut st = playing();
    act(&mut st.e, key_down(64, "A", None));
    assert_eq!(held(&st, "A"), 1);
    assert_eq!(held(&st, "B"), 0, "only the deck the key was aimed at");
    assert_eq!(st.e.keys.played, 1);
    act(&mut st.e, Action::KeyUp { deck: "A".into(), index: 64 });
    assert_eq!(held(&st, "A"), 0);
}

#[test]
fn the_key_is_heard_on_the_decks_output() {
    // the same song twice, one with a key held: the render differs, so the note reached the runner
    let (mut quiet, mut played) = (playing(), playing());
    act(&mut played.e, key_down(72, "A", None));
    let (mut a, mut b) = (0.0f64, 0.0f64);
    for _ in 0..200 {
        quiet.block();
        played.block();
        a += quiet.l.iter().map(|v| (*v as f64).abs()).sum::<f64>();
        b += played.l.iter().map(|v| (*v as f64).abs()).sum::<f64>();
    }
    assert!((a - b).abs() > 1e-3, "the held key changed nothing: {a} vs {b}");
}

#[test]
fn quantised_keys_wait_for_the_beat_and_a_quick_tap_still_sounds() {
    let mut st = playing();
    st.e.keys.quant = Quant::Beat;
    // off the beat: keep stepping until the clock is mid-beat
    let (beat, fpb) = st.e.clock_beat_now().expect("a clock");
    assert!(fpb > 0.0);
    let to_mid = (0.5 - beat.fract()).rem_euclid(1.0) * fpb;
    st.step((to_mid / common_track::BLOCK as f64) as usize);
    act(&mut st.e, key_down(60, "A", None));
    assert_eq!(held(&st, "A"), 0, "held back for the beat");
    assert_eq!(st.e.keys.pending.len(), 1);
    let due = st.e.keys.pending[0].due;
    // released at once: the release waits behind its press
    act(&mut st.e, Action::KeyUp { deck: "A".into(), index: 60 });
    assert_eq!(st.e.keys.pending.len(), 2);
    assert!(st.e.keys.pending[1].due > due, "a tap is given a gate");
    // the press lands on the beat (within a service pass), then the release
    let mut landed = None;
    for _ in 0..400 {
        st.block();
        if landed.is_none() && st.e.keys.played == 1 {
            landed = Some(st.e.loop_frames());
        }
        if st.e.keys.pending.is_empty() {
            break;
        }
    }
    let at = landed.expect("the key played");
    let late = at - due;
    assert!(
        late.abs() <= (RATE * 0.03) as i64,
        "landed {late} frames from the beat ({} ms)",
        late as f64 * 1000.0 / RATE
    );
    assert_eq!(held(&st, "A"), 0, "and let go again");
}

#[test]
fn a_bindings_own_quantise_beats_the_routes() {
    let mut st = playing();
    st.e.keys.quant = Quant::Now;
    act(&mut st.e, key_down(64, "A", Some("bar")));
    // now, unless it happens to be on the bar line
    assert!(st.e.keys.pending.len() == 1 || held(&st, "A") == 1);
    st.e.keys_off();
    assert!(st.e.keys.pending.is_empty());
    assert_eq!(held(&st, "A"), 0);
}

#[test]
fn the_route_picks_the_deck_the_mode_and_lets_go_when_it_changes() {
    let mut st = playing();
    assert_eq!(st.e.key_target(), st.e.focus);
    st.e.set_key_deck("B").unwrap();
    assert_eq!(st.e.key_target(), "B");
    assert_eq!(st.e.key_deck().as_deref(), Some("B"));
    assert!(st.e.set_key_deck("Z").is_err());
    st.e.set_key_deck("next").unwrap();
    assert_eq!(st.e.keys.deck, None, "after the last deck: the aimed one");
    st.e.set_key_deck("next").unwrap();
    assert_eq!(st.e.keys.deck.as_deref(), Some("A"));
    st.e.set_key_deck("aimed").unwrap();

    act(&mut st.e, key_down(64, "A", None));
    assert_eq!(held(&st, "A"), 1);
    assert_eq!(st.e.key_mode(), KeyMode::Live);
    st.e.set_key_mode(KeyMode::Transpose);
    assert_eq!(held(&st, "A"), 0, "switching mode lets go of a held key");
    assert_eq!(st.e.toggle_key_mode(), KeyMode::Live);
    st.e.set_key_mode(KeyMode::Td3);
    assert_eq!(st.e.toggle_key_mode(), KeyMode::Live, "the TD-3 mode goes back to live");
    let j = st.e.keys_json();
    assert_eq!(j.get("mode").and_then(JsonValue::as_str), Some("live"));
    assert_eq!(j.get("quant").and_then(JsonValue::as_str), Some("now"));
}

#[test]
fn the_key_mode_and_key_deck_targets_switch_the_route() {
    let mut st = playing();
    let target = |id: &str, k: &str, v: &str| Action::Target {
        id: id.into(),
        deck: None,
        slot: None,
        index: 0,
        value: 1.0,
        pressed: true,
        args: [(k.to_string(), v.to_string())].into(),
    };
    act(&mut st.e, target("keyMode", "mode", "toggle"));
    assert_eq!(st.e.keys.mode, KeyMode::Transpose);
    act(&mut st.e, target("keyMode", "mode", "td3"));
    assert_eq!(st.e.keys.mode, KeyMode::Td3);
    act(&mut st.e, target("keyMode", "mode", "live"));
    assert_eq!(st.e.keys.mode, KeyMode::Live);
    act(&mut st.e, target("keyDeck", "to", "B"));
    assert_eq!(st.e.keys.deck.as_deref(), Some("B"));
    act(&mut st.e, target("keyMode", "mode", "nonsense"));
    assert!(st.e.unhandled.iter().any(|u| u.contains("keyMode")), "{:?}", st.e.unhandled);
}

#[test]
fn a_sample_deck_has_no_live_voice_and_says_so() {
    let mut st = Stepper::new(RATE, false);
    assert!(st.e.load_demo("B"));
    act(&mut st.e, key_down(64, "B", None));
    assert_eq!(held(&st, "B"), 0);
    assert!(st.e.unhandled.iter().any(|u| u.contains("no live voice")), "{:?}", st.e.unhandled);
}

#[test]
fn scale_lock_can_be_turned_off_for_chromatic_leads() {
    let mut st = playing();
    // find a note the scale moves
    let moved = (48..72)
        .find(|n| st.e.scale_locked("A", *n) != *n)
        .expect("some chromatic note is off the scale");
    act(&mut st.e, key_down(moved as usize, "A", None));
    assert_eq!(st.e.deck("A").unwrap().held_keys[&moved], st.e.scale_locked("A", moved));
    act(&mut st.e, Action::KeyUp { deck: "A".into(), index: moved as usize });
    st.e.keys.scale_lock = false;
    act(&mut st.e, key_down(moved as usize, "A", None));
    assert_eq!(st.e.deck("A").unwrap().held_keys[&moved], moved);
}

#[test]
fn every_key_is_a_live_op_an_arrangement_keeps_and_plays_again() {
    let mut st = playing();
    st.e.trace = Some(Vec::new());
    act(&mut st.e, key_down(64, "A", Some("beat")));
    act(&mut st.e, Action::KeyUp { deck: "A".into(), index: 64 });
    let traced = st.e.trace.take().unwrap();
    let ops: Vec<&ArrangeOp> = traced.iter().flat_map(|t| t.ops.iter()).collect();
    assert_eq!(ops.len(), 2, "{traced:?}");
    // the press carries what it needs to land the same way
    let down = ops[0].json();
    assert_eq!(down.get("op").and_then(JsonValue::as_str), Some("keys"));
    assert_eq!(down.get("deck").and_then(JsonValue::as_str), Some("A"));
    assert_eq!(down.get("note").and_then(JsonValue::as_f64), Some(64.0));
    assert_eq!(down.get("quant").and_then(JsonValue::as_str), Some("beat"));
    assert_eq!(down.get("down"), Some(&JsonValue::Bool(true)));
    assert_eq!(ops[1].json().get("down"), Some(&JsonValue::Bool(false)));
    // and it round-trips through the arrangement's JSON
    for op in &ops {
        assert_eq!(ArrangeOp::from_json(&op.json()).as_ref(), Some(*op));
    }
    // the words for the log say the quantise
    assert!(traced[0].entry.text.contains("on the beat"), "{}", traced[0].entry.text);

    // a fresh engine takes the ops and plays the key: a press then its release
    let mut again = playing();
    again.e.keys.quant = Quant::Now;
    let press = ArrangeOp::from_json(&ops[0].json()).unwrap();
    // a press quantised to the beat is held back there too
    assert!(again.e.apply_arrange_op(&press));
    let waiting = again.e.keys.pending.len() + again.e.deck("A").unwrap().held_keys.len();
    assert_eq!(waiting, 1);
    again.step(400);
    assert!(again.e.apply_arrange_op(ops[1]));
    again.step(400);
    assert_eq!(again.e.deck("A").unwrap().held_keys.len(), 0);
    assert_eq!(again.e.keys.played, 1);
}

#[test]
fn the_keys_live_op_sets_the_route_and_refuses_nonsense() {
    let mut st = playing();
    let p = |t: &str| JsonValue::parse(t).unwrap();
    st.e.apply_live_op("keys", &p(r#"{"mode":"transpose"}"#)).unwrap();
    assert_eq!(st.e.keys.mode, KeyMode::Transpose);
    st.e.apply_live_op("keys", &p(r#"{"mode":"toggle"}"#)).unwrap();
    assert_eq!(st.e.keys.mode, KeyMode::Live);
    st.e.apply_live_op("keys", &p(r#"{"quant":"bar","deck_to":"B","scale_lock":false}"#)).unwrap();
    assert_eq!((st.e.keys.quant, st.e.keys.deck.as_deref(), st.e.keys.scale_lock), (Quant::Bar, Some("B"), false));
    st.e.apply_live_op("keys", &p(r#"{"note":"C4","deck":"A","quant":"now"}"#)).unwrap();
    assert_eq!(held(&st, "A"), 1);
    st.e.apply_live_op("keys", &p(r#"{"off":true}"#)).unwrap();
    assert_eq!(held(&st, "A"), 0);
    assert!(st.e.apply_live_op("keys", &p(r#"{"mode":"sideways"}"#)).is_err());
    assert!(st.e.apply_live_op("keys", &p(r#"{"quant":"later"}"#)).is_err());
    assert!(st.e.apply_live_op("keys", &p(r#"{}"#)).is_err());
    assert!(st.e.apply_live_op("keys", &p(r#"{"note":60,"deck":"Q"}"#)).is_err());
}
