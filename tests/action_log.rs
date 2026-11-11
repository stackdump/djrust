//! The action log's source (docs/ACTION-LOG.md): controller MIDI through the router comes out of
//! the engine's trace as words ("Browser A +1 → seed 43", "APC pad 3: solo-drums macro on A")
//! and as session ops, which make an arrangement that survives JSON and plays back on another
//! engine to the same tracks and moves.

use djrust::control::action_log::{ActionLog, LineKind};
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::arrangement::{ArrangeEvent, ArrangeOp, Arrangement};
use djrust::session::model::DeckSource;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer, Rig};

const SR: f64 = 48_000.0;
const INPULSE: &str = "DJControl Inpulse 200 Mk3";
const APC: &str = "APC mini mk2 Control";

fn new_rig() -> (Rig, Renderer) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, r) = engine(c);
    e.generate("A", &TrackSpec::new("techno", 42.0, None));
    e.generate("B", &TrackSpec::new("house", 7.0, None));
    e.play("A");
    let mut rig = Rig::new(e);
    rig.engine.trace = Some(Vec::new());
    assert!(rig.attach(INPULSE));
    assert!(rig.attach(APC));
    (rig, r)
}

fn seed_target(rig: &Rig, deck: &str) -> f64 {
    let d = rig.engine.deck(deck).unwrap();
    d.track.seed_target.as_ref().or(d.spec.as_ref()).unwrap().seed
}

#[test]
fn controller_moves_are_logged_in_words_and_as_session_ops_that_replay() {
    let (mut rig, _r) = new_rig();
    let mut t = 0.0;
    let mut send = |rig: &mut Rig, port: &str, m: &[u8]| {
        t += 0.25;
        rig.route(port, m, t);
        t
    };
    // the wheel on A: forward 2, back 1
    send(&mut rig, INPULSE, &[0xB0, 0x01, 0x01]);
    send(&mut rig, INPULSE, &[0xB0, 0x01, 0x01]);
    send(&mut rig, INPULSE, &[0xB0, 0x01, 0x7F]);
    // an APC pad: the third macro of the grid on the aimed deck
    send(&mut rig, APC, &[0x90, 0x02, 0x7F]);
    send(&mut rig, APC, &[0x90, 0x02, 0x00]);
    // B armed with its SHIFT, then the wheel turns B
    send(&mut rig, INPULSE, &[0x92, 0x04, 0x7F]);
    send(&mut rig, INPULSE, &[0x92, 0x04, 0x00]);
    send(&mut rig, INPULSE, &[0xB0, 0x01, 0x01]);

    let traced = rig.engine.trace.take().unwrap();
    let texts: Vec<&str> = traced.iter().map(|x| x.entry.text.as_str()).collect();
    assert_eq!(texts[0], "Browser A +1 → seed 43", "{texts:?}");
    assert_eq!(texts[1], "Browser A +1 → seed 44");
    assert_eq!(texts[2], "Browser A -1 → seed 43");
    assert!(
        texts.contains(&"APC pad 3: solo-drums macro on A"),
        "{texts:?}"
    );
    assert_eq!(*texts.last().unwrap(), "Browser B +1 → seed 8");
    assert_eq!(traced[0].entry.source, "Inpulse browser");
    // the ops: swaps to the seeds, the macro as its session action
    let macro_op = traced
        .iter()
        .find(|x| x.entry.text.starts_with("APC pad 3"))
        .unwrap();
    assert_eq!(
        macro_op.ops,
        [ArrangeOp::Deck(DeckAction::Macro {
            deck: "A".into(),
            id: "solo-drums".into(),
        })]
    );
    assert_eq!(seed_target(&rig, "A"), 43.0);
    assert_eq!(seed_target(&rig, "B"), 8.0);

    // the log, one beat apart at 120: the pattern of the wheel's moves
    let mut log = ActionLog::new();
    let mut events = Vec::new();
    for (k, x) in traced.iter().enumerate() {
        let beat = k as f64;
        log.record(beat * 0.5, 120.0, Some(beat), x.entry.clone());
        for op in &x.ops {
            events.push(ArrangeEvent {
                at: beat,
                bpm: 120.0,
                op: op.clone(),
                via: x.entry.source.clone(),
                text: x.entry.text.clone(),
            });
        }
    }
    let actions: Vec<_> = log
        .lines()
        .iter()
        .filter(|l| l.kind == LineKind::Action)
        .collect();
    assert_eq!(actions[2].pattern, "forward 2, back 1");
    assert_eq!(actions[1].since, Some(1.0));
    assert!(log.lines().iter().any(|l| l.kind == LineKind::Bar && l.text == "bar 2"));

    // the arrangement through JSON, played on a fresh engine: the same tracks, the same macro
    let decks = [
        ("A".to_string(), DeckSource::Net(TrackSpec::new("techno", 42.0, None))),
        ("B".to_string(), DeckSource::Net(TrackSpec::new("house", 7.0, None))),
    ]
    .into_iter()
    .collect();
    let arr = Arrangement::new(120.0, decks, events);
    let text = arr.json().js_stringified();
    let (back, bad) = Arrangement::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
    assert_eq!(bad, 0);
    assert_eq!(back, arr);
    assert!(text.contains(r#""op":"swap""#) && text.contains(r#""op":"macro""#));

    let (mut fresh, _r2) = new_rig();
    fresh.engine.trace = Some(Vec::new());
    for e in &back.events {
        assert!(fresh.engine.apply_arrange_op(&e.op), "{:?}", e.op);
    }
    assert_eq!(seed_target(&fresh, "A"), 43.0);
    assert_eq!(seed_target(&fresh, "B"), 8.0);
    // played back, the macro went through `perform` and is traced as the session action
    let replayed = fresh.engine.trace.take().unwrap();
    assert!(
        replayed
            .iter()
            .any(|x| x.ops == [ArrangeOp::Deck(DeckAction::Macro {
                deck: "A".into(),
                id: "solo-drums".into()
            })]),
        "{:?}",
        replayed.iter().map(|x| &x.entry.text).collect::<Vec<_>>()
    );
}

#[test]
fn a_passage_of_the_jam_starts_on_the_seed_the_deck_had_reached() {
    let (mut rig, _r) = new_rig();
    for (k, m) in [[0xB0u8, 0x01, 0x01], [0xB0, 0x01, 0x01], [0xB0, 0x01, 0x01]]
        .iter()
        .enumerate()
    {
        rig.route(INPULSE, m, k as f64);
    }
    let traced = rig.engine.trace.take().unwrap();
    // one move a bar: bars 1, 2, 3
    let events: Vec<ArrangeEvent> = traced
        .iter()
        .enumerate()
        .flat_map(|(k, x)| {
            x.ops.iter().map(move |op| ArrangeEvent {
                at: k as f64 * 4.0,
                bpm: 120.0,
                op: op.clone(),
                via: String::new(),
                text: String::new(),
            })
        })
        .collect();
    let arr = Arrangement::new(120.0, Default::default(), events);
    let p = arr.passage(3, 3);
    assert_eq!(p.events.len(), 1);
    assert_eq!(
        p.decks.get("A"),
        Some(&DeckSource::Net(TrackSpec::new("techno", 44.0, None)))
    );
}
