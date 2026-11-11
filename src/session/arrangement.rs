//! An arrangement (beyond Swift; docs/ACTION-LOG.md): a performance's moves as a beat-stamped
//! timeline of session-JSON actions, made to be cut into passages, edited and played back as a
//! track of its own.
//!
//! Why not a [`Session`](super::model::Session): a session stamps each event with the engine
//! frame it arrived on and keeps controller input as raw MIDI, from a snapshot of every deck at
//! rest. It is exact for re-rendering a whole set, but a frame offset says nothing musical: a
//! passage can't be lifted out and re-placed, the tempo can't change under it, and MIDI bytes
//! mean nothing without the controller map of the day. An arrangement keeps the same
//! [`DeckAction`] JSON (so anything the engine performs can be in it, and an edited one plays
//! through `Engine::perform` unchanged), and puts round each one:
//!
//! - `at`: where, in **beats** from the arrangement's start (bar = ⌊at/4⌋+1, beat = at mod 4 + 1;
//!   both are written beside it for reading, `at` is what counts when read back);
//! - `bpm`: the master tempo then;
//! - `via` and `text`: which control did it and what it did in words (metadata, the log's view).
//!
//! One op is the arrangement's own, beside the 75 `DeckAction` ops (which stay as Swift has
//! them): **`swap`** `{deck, source, start: "bar" | "top"}`: the deck re-composed to a new track
//! and handed over on its next bar (from the top for a loop): what the browser wheel's seed step
//! does. A `load` restarts a deck at once, which is not the same move.
//!
//! The others are **live ops**, the moves that are no `DeckAction` because Swift has no such
//! part (the vocoder, the performance tricks, the capture library): `{"op": METHOD, …params}`
//! with the params of djapp's socket method of that name ([`LIVE_METHODS`]), applied by
//! `Engine::apply_live_op`. Kept generic ([`ArrangeOp::Live`]) so another method is one more
//! name here and one more arm there.
//!
//! `decks` holds what each deck held at the start, so a passage plays from the right tracks:
//! [`Arrangement::passage`] carries forward the last `load` / `swap` before the cut.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::action::DeckAction;
use super::model::DeckSource;
use crate::json::{JsonObject, JsonValue};

/// The schema tag an arrangement file carries.
pub const SCHEMA: &str = "djrust.arrangement/1";

/// The socket methods an arrangement can carry as live ops ([`ArrangeOp::Live`]).
pub const LIVE_METHODS: [&str; 21] = [
    "vocoder",
    "trick",
    "library",
    "freeze",
    "input",
    "cue",
    "nudge",
    "jog",
    "history",
    "capture",
    "loops",
    "source",
    "pitch_range",
    "loop_beats",
    "seek",
    "autonext",
    "transport",
    "target",
    "aux",
    "keys",
    "post",
];

/// When a `swap` hands over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStart {
    /// On the deck's next bar, carrying on where the song is.
    Bar,
    /// On the next bar, from the new track's top (a loop).
    Top,
}

/// One move of an arrangement.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrangeOp {
    /// Any session action, as the engine performs it.
    Deck(DeckAction),
    /// The deck re-composed to `source` and handed over on its next bar.
    Swap {
        deck: String,
        source: DeckSource,
        start: SwapStart,
    },
    /// A live op: one of [`LIVE_METHODS`] with its socket params (`op` taken out).
    Live { method: String, params: JsonValue },
}

impl ArrangeOp {
    pub fn json(&self) -> JsonValue {
        match self {
            ArrangeOp::Deck(a) => a.json(),
            ArrangeOp::Swap {
                deck,
                source,
                start,
            } => JsonValue::Object(JsonObject::from_pairs([
                ("op", JsonValue::str("swap")),
                ("deck", JsonValue::str(deck)),
                ("source", source.json()),
                (
                    "start",
                    JsonValue::str(match start {
                        SwapStart::Bar => "bar",
                        SwapStart::Top => "top",
                    }),
                ),
            ])),
            ArrangeOp::Live { method, params } => {
                let mut o = JsonObject::from_pairs([("op", JsonValue::str(method))]);
                if let Some(p) = params.as_object() {
                    for (k, x) in p.iter() {
                        if k != "op" {
                            o.set(k, x.clone());
                        }
                    }
                }
                JsonValue::Object(o)
            }
        }
    }

    pub fn from_json(v: &JsonValue) -> Option<ArrangeOp> {
        if v.get("op").and_then(JsonValue::as_str) == Some("swap") {
            let deck = v.get("deck")?.as_str()?.to_string();
            let source = DeckSource::from_json(v.get("source")?)?;
            let start = match v.get("start").and_then(JsonValue::as_str) {
                Some("top") => SwapStart::Top,
                _ => SwapStart::Bar,
            };
            return Some(ArrangeOp::Swap {
                deck,
                source,
                start,
            });
        }
        if let Some(m) = v
            .get("op")
            .and_then(JsonValue::as_str)
            .filter(|m| LIVE_METHODS.contains(m))
        {
            let mut params = JsonObject::new();
            for (k, x) in v.as_object()?.iter() {
                if k != "op" {
                    params.set(k, x.clone());
                }
            }
            return Some(ArrangeOp::Live {
                method: m.to_string(),
                params: JsonValue::Object(params),
            });
        }
        DeckAction::from_json(v).map(ArrangeOp::Deck)
    }

    /// The deck it acts on, if one.
    pub fn deck(&self) -> Option<String> {
        match self {
            ArrangeOp::Deck(a) => a.deck(),
            ArrangeOp::Swap { deck, .. } => Some(deck.clone()),
            ArrangeOp::Live { params, .. } => {
                params.get("deck").and_then(JsonValue::as_str).map(str::to_string)
            }
        }
    }

    /// `op` as written.
    pub fn op(&self) -> String {
        match self {
            ArrangeOp::Deck(a) => a.op(),
            ArrangeOp::Swap { .. } => "swap".into(),
            ArrangeOp::Live { method, .. } => method.clone(),
        }
    }
}

/// One stamped move.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangeEvent {
    /// Beats from the arrangement's start.
    pub at: f64,
    /// The master tempo when it was made.
    pub bpm: f64,
    pub op: ArrangeOp,
    /// The control that did it ("Inpulse browser", "APC pad 3", "window", "socket").
    pub via: String,
    /// What it did, in words.
    pub text: String,
}

/// The bar (1-based) and beat in it (1-based, fractional) of a position `at` in beats.
pub fn bar_beat(at: f64) -> (i64, f64) {
    let bar = libm::floor(at / 4.0);
    (bar as i64 + 1, at - bar * 4.0 + 1.0)
}

impl ArrangeEvent {
    pub fn json(&self) -> JsonValue {
        let (bar, beat) = bar_beat(self.at);
        let mut o = JsonObject::from_pairs([
            ("at", JsonValue::Number(self.at)),
            ("bar", JsonValue::int(bar)),
            ("beat", JsonValue::Number(beat)),
            ("bpm", JsonValue::Number(self.bpm)),
            ("action", self.op.json()),
        ]);
        if !self.via.is_empty() {
            o.set("via", JsonValue::str(&self.via));
        }
        if !self.text.is_empty() {
            o.set("text", JsonValue::str(&self.text));
        }
        JsonValue::Object(o)
    }

    /// `at`, else `bar` and `beat` (both 1-based), places it.
    pub fn from_json(v: &JsonValue) -> Option<ArrangeEvent> {
        let at = match v.get("at").and_then(JsonValue::as_f64) {
            Some(a) => a,
            None => {
                let bar = v.get("bar")?.as_f64()?;
                let beat = v.get("beat").and_then(JsonValue::as_f64).unwrap_or(1.0);
                (bar - 1.0) * 4.0 + (beat - 1.0)
            }
        };
        Some(ArrangeEvent {
            at,
            bpm: v.get("bpm").and_then(JsonValue::as_f64).unwrap_or(0.0),
            op: ArrangeOp::from_json(v.get("action")?)?,
            via: v
                .get("via")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            text: v
                .get("text")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
        })
    }
}

/// A beat-stamped timeline of moves, with what the decks held at its start.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Arrangement {
    /// The tempo it was played at, at its start (playback may take another).
    pub bpm: f64,
    /// What each deck held at the start (a deck missing here plays what it holds).
    pub decks: BTreeMap<String, DeckSource>,
    /// In order of `at`.
    pub events: Vec<ArrangeEvent>,
    /// The rest of the engine at beat 0: mixer, knobs, playing, inputs, files, setup moves
    /// ([`ArrangeStart`](super::arrange_start::ArrangeStart); written only when not empty).
    pub start: super::arrange_start::ArrangeStart,
}

impl Arrangement {
    /// Events sorted by `at` (stable: same-beat moves keep their order).
    pub fn new(bpm: f64, decks: BTreeMap<String, DeckSource>, mut events: Vec<ArrangeEvent>) -> Self {
        events.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(core::cmp::Ordering::Equal));
        Arrangement {
            bpm,
            decks,
            events,
            start: Default::default(),
        }
    }

    /// Length in beats: to the end of the bar of its last move.
    pub fn beats(&self) -> f64 {
        self.events
            .last()
            .map_or(0.0, |e| (libm::floor(e.at / 4.0) + 1.0) * 4.0)
    }

    /// Seconds from beat 0 to beat `at`, at the arrangement's own tempo: it starts at `bpm`, and
    /// each move's `bpm` (the master tempo when it was made, after it) holds until the next
    /// one, so a set whose tempo moved lands every move where it fell. An arrangement whose
    /// moves all carry its `bpm` (or none) is `at * 60 / bpm`, exactly. A move with no tempo
    /// (hand-written) keeps the one before it.
    pub fn seconds_at(&self, at: f64) -> f64 {
        let base = if self.bpm > 0.0 { self.bpm } else { 120.0 };
        let at = at.max(0.0);
        if self.events.iter().all(|e| !(e.bpm > 0.0) || e.bpm == base) {
            return at * 60.0 / base;
        }
        let (mut secs, mut t, mut bpm) = (0.0, 0.0, base);
        for e in &self.events {
            if e.at >= at {
                break;
            }
            if e.at > t {
                secs += (e.at - t) * 60.0 / bpm;
                t = e.at;
            }
            if e.bpm > 0.0 {
                bpm = e.bpm;
            }
        }
        secs + (at - t) * 60.0 / bpm
    }

    pub fn json(&self) -> JsonValue {
        let mut decks = JsonObject::new();
        for (k, v) in &self.decks {
            decks.set(k, v.json());
        }
        let mut o = JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("bpm", JsonValue::Number(self.bpm)),
            ("decks", JsonValue::Object(decks)),
        ]);
        if !self.start.is_empty() {
            o.set("start", self.start.json());
        }
        o.set(
            "events",
            JsonValue::Array(self.events.iter().map(ArrangeEvent::json).collect()),
        );
        JsonValue::Object(o)
    }

    /// Read one back. Events that don't parse are skipped and counted in the second value.
    pub fn from_json(v: &JsonValue) -> Option<(Arrangement, usize)> {
        if v.get("schema").and_then(JsonValue::as_str) != Some(SCHEMA) {
            return None;
        }
        let mut decks = BTreeMap::new();
        if let Some(JsonValue::Object(o)) = v.get("decks") {
            for k in o.keys() {
                if let Some(s) = o.get(k).and_then(DeckSource::from_json) {
                    decks.insert(k.to_string(), s);
                }
            }
        }
        let mut bad = 0;
        let mut events = Vec::new();
        if let Some(JsonValue::Array(a)) = v.get("events") {
            for e in a {
                match ArrangeEvent::from_json(e) {
                    Some(e) => events.push(e),
                    None => bad += 1,
                }
            }
        }
        let bpm = v.get("bpm").and_then(JsonValue::as_f64).unwrap_or(120.0);
        let mut a = Arrangement::new(bpm, decks, events);
        if let Some(s) = v.get("start") {
            a.start = super::arrange_start::ArrangeStart::from_json(s);
        }
        Some((a, bad))
    }

    /// Bars `from..=to` (1-based) as an arrangement of their own: the moves in them, moved to
    /// start at beat 0, and each deck starting on what it held at the cut (the last `load` or
    /// `swap` before it, else what the whole arrangement started with).
    pub fn passage(&self, from: i64, to: i64) -> Arrangement {
        let start = ((from.max(1) - 1) * 4) as f64;
        let end = (to.max(from) * 4) as f64;
        let mut decks = self.decks.clone();
        let mut bpm = self.bpm;
        for e in self.events.iter().filter(|e| e.at < start) {
            if e.bpm > 0.0 {
                bpm = e.bpm;
            }
            match &e.op {
                ArrangeOp::Swap { deck, source, .. } => {
                    decks.insert(deck.clone(), source.clone());
                }
                ArrangeOp::Deck(DeckAction::Load { deck, source }) => {
                    decks.insert(deck.clone(), source.clone());
                }
                _ => {}
            }
        }
        let events = self
            .events
            .iter()
            .filter(|e| e.at >= start && e.at < end)
            .map(|e| ArrangeEvent {
                at: e.at - start,
                ..e.clone()
            })
            .collect();
        // the start's mixer, files and setup carry over (the knobs moved before the cut are not
        // folded in: the passage starts as the whole did, on the decks it had reached)
        let mut p = Arrangement::new(bpm, decks, events);
        p.start = self.start.clone();
        if let Some(pl) = p.start.playing.as_mut() {
            // the decks the whole had started by the cut play from the passage's top
            for e in self.events.iter().filter(|e| e.at < start) {
                if let ArrangeOp::Deck(DeckAction::Play { deck, on }) = &e.op {
                    pl.retain(|d| d != deck);
                    if *on {
                        pl.push(deck.clone());
                    }
                }
            }
            pl.sort();
        }
        p
    }

    /// The moves due as playback goes from beat `from` (exclusive) to `to` (inclusive):
    /// what a player hands to the engine each pass. A first call with `from` < 0 takes beat 0.
    pub fn due(&self, from: f64, to: f64) -> Vec<&ArrangeEvent> {
        self.events
            .iter()
            .filter(|e| e.at > from && e.at <= to)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::TrackSpec;

    fn ev(at: f64, op: ArrangeOp) -> ArrangeEvent {
        ArrangeEvent {
            at,
            bpm: 124.0,
            op,
            via: "Inpulse browser".into(),
            text: "x".into(),
        }
    }

    fn swap(deck: &str, seed: f64) -> ArrangeOp {
        ArrangeOp::Swap {
            deck: deck.into(),
            source: DeckSource::Net(TrackSpec::new("techno", seed, None)),
            start: SwapStart::Top,
        }
    }

    fn mac(deck: &str) -> ArrangeOp {
        ArrangeOp::Deck(DeckAction::Macro {
            deck: deck.into(),
            id: "filter-sweep".into(),
        })
    }

    #[test]
    fn positions_read_as_bars_and_beats() {
        assert_eq!(bar_beat(0.0), (1, 1.0));
        assert_eq!(bar_beat(146.5), (37, 3.5));
    }

    #[test]
    fn an_arrangement_round_trips_through_json() {
        let a = Arrangement::new(
            124.0,
            BTreeMap::from([("A".to_string(), DeckSource::Net(TrackSpec::new("techno", 4212.0, None)))]),
            Vec::from([ev(5.0, mac("B")), ev(4.0, swap("A", 4213.0))]),
        );
        assert_eq!(a.events[0].at, 4.0, "sorted");
        let j = a.json();
        assert_eq!(j.get("schema").and_then(JsonValue::as_str), Some(SCHEMA));
        let e0 = &j.get("events").unwrap();
        let JsonValue::Array(e0) = e0 else { panic!() };
        assert_eq!(e0[0].get("bar").and_then(JsonValue::as_f64), Some(2.0));
        assert_eq!(e0[0].get("action").unwrap().get("op").and_then(JsonValue::as_str), Some("swap"));
        assert_eq!(e0[1].get("action").unwrap().get("op").and_then(JsonValue::as_str), Some("macro"));
        let (b, bad) = Arrangement::from_json(&JsonValue::parse(&j.js_stringified()).unwrap()).unwrap();
        assert_eq!(bad, 0);
        assert_eq!(b.events.len(), 2);
        assert_eq!(b.events[0].op.op(), "swap");
        assert_eq!(b.events[1].op, mac("B"));
        assert_eq!(b.events[1].text, "x");
    }

    #[test]
    fn a_live_op_keeps_its_method_and_params() {
        let v = JsonValue::parse(r#"{"op":"trick","deck":"A","kind":"riser","action":"on","bars":4}"#).unwrap();
        let op = ArrangeOp::from_json(&v).unwrap();
        let ArrangeOp::Live { method, params } = &op else { panic!("{op:?}") };
        assert_eq!(method, "trick");
        assert!(params.get("op").is_none());
        assert_eq!(params.get("bars").and_then(JsonValue::as_f64), Some(4.0));
        assert_eq!(op.deck().as_deref(), Some("A"));
        assert_eq!(op.op(), "trick");
        let back = ArrangeOp::from_json(&op.json()).unwrap();
        assert_eq!(back, op);
        assert_eq!(op.json().get("op").and_then(JsonValue::as_str), Some("trick"));
        // an op that is neither a DeckAction nor a live method is still refused
        assert!(ArrangeOp::from_json(&JsonValue::parse(r#"{"op":"nope"}"#).unwrap()).is_none());
    }

    #[test]
    fn an_edited_event_can_be_placed_by_bar_and_beat() {
        let v = JsonValue::parse(
            r#"{"bar": 3, "beat": 2, "action": {"op": "macro", "deck": "B", "macro": "filter-sweep"}}"#,
        )
        .unwrap();
        let e = ArrangeEvent::from_json(&v).unwrap();
        assert_eq!(e.at, 9.0);
        assert_eq!(e.op, mac("B"));
        // unknown ops are skipped and counted, not fatal
        let doc = JsonValue::parse(&alloc::format!(
            r#"{{"schema":"{SCHEMA}","bpm":120,"events":[{{"at":1,"action":{{"op":"nope"}}}},{{"at":2,"action":{{"op":"panic","deck":"A"}}}}]}}"#
        ))
        .unwrap();
        let (a, bad) = Arrangement::from_json(&doc).unwrap();
        assert_eq!((a.events.len(), bad), (1, 1));
    }

    #[test]
    fn a_passage_starts_on_the_tracks_the_decks_held_at_the_cut() {
        let a = Arrangement::new(
            120.0,
            BTreeMap::new(),
            Vec::from([
                ev(1.0, swap("A", 1.0)),
                ev(6.0, swap("A", 2.0)),
                ev(9.0, mac("A")),
                ev(13.0, swap("B", 7.0)),
                ev(20.0, mac("B")),
            ]),
        );
        // bars 3..=4 (beats 8..16)
        let p = a.passage(3, 4);
        assert_eq!(p.events.len(), 2);
        assert_eq!(p.events[0].at, 1.0);
        assert_eq!(p.events[1].at, 5.0);
        assert_eq!(
            p.decks.get("A"),
            Some(&DeckSource::Net(TrackSpec::new("techno", 2.0, None)))
        );
        assert!(p.decks.get("B").is_none());
        assert_eq!(p.bpm, 124.0);
        assert_eq!(p.beats(), 8.0);
        // playback hands out each move once
        let mut got = 0;
        let mut last = -1.0;
        for k in 1..=40 {
            let now = k as f64 * 0.25;
            got += p.due(last, now).len();
            last = now;
        }
        assert_eq!(got, 2);
    }

    #[test]
    fn seconds_follow_the_tempo_each_move_was_made_at() {
        let mk = |at: f64, bpm: f64| ArrangeEvent {
            at,
            bpm,
            op: ArrangeOp::Deck(DeckAction::Master(0.5)),
            via: String::new(),
            text: String::new(),
        };
        // constant tempo: exactly at * 60 / bpm
        let a = Arrangement::new(120.0, BTreeMap::new(), alloc::vec![mk(1.0, 120.0), mk(9.0, 120.0)]);
        assert_eq!(a.seconds_at(7.0), 7.0 * 60.0 / 120.0);
        // 120 for the first 4 beats (2 s), then 60 (1 s a beat) from the move at beat 4 on,
        // then 240 from the one at beat 8
        let a = Arrangement::new(120.0, BTreeMap::new(), alloc::vec![mk(4.0, 60.0), mk(8.0, 240.0)]);
        assert_eq!(a.seconds_at(0.0), 0.0);
        assert_eq!(a.seconds_at(4.0), 2.0);
        assert_eq!(a.seconds_at(6.0), 4.0);
        assert_eq!(a.seconds_at(8.0), 6.0);
        assert_eq!(a.seconds_at(12.0), 7.0);
        // a hand-written move with no tempo keeps the one before it
        let a = Arrangement::new(120.0, BTreeMap::new(), alloc::vec![mk(4.0, 60.0), mk(8.0, 0.0)]);
        assert_eq!(a.seconds_at(12.0), 2.0 + 4.0 + 4.0);
    }
}
