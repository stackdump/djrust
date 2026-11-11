//! Port of `MinecraftEvents.swift`: gameplay on a Minecraft server fires stingers.
//! The `minecraft` MCP server's `mc_recent_events` (a ring buffer of `{at, source, event,
//! params}`, oldest first) is polled by whoever runs the loop and handed to
//! [`MinecraftEventFeed::take`], which maps each new event through `minecraft-events.json`.
//! The polling loop is ops work; this is the part that has to be right: each event fires once
//! however often the window is re-read, and a deck gets at most one stinger a bar.

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonError, JsonObject, JsonValue};

/// The bundled `minecraft-events.json` text.
pub const EVENTS_JSON: &str = include_str!("../../data/minecraft-events.json");

#[derive(Clone, Debug, PartialEq)]
pub struct MinecraftEvent {
    /// ISO-8601 with milliseconds, UTC (`new Date().toISOString()`): sorts as text.
    pub at: String,
    /// `rpc` (management-protocol notifications) or `log` (the log bridge).
    pub source: String,
    /// The RPC method (`minecraft:notification/players/joined`) or `log:chat` / `log:death` /
    /// `log:advancement`.
    pub event: String,
    pub params: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MinecraftEventError {
    Shape(String),
    Json(String),
}

impl core::fmt::Display for MinecraftEventError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MinecraftEventError::Shape(s) | MinecraftEventError::Json(s) => f.write_str(s),
        }
    }
}

impl MinecraftEvent {
    pub fn new(at: &str, event: &str) -> MinecraftEvent {
        MinecraftEvent {
            at: at.into(),
            source: "log".into(),
            event: event.into(),
            params: JsonValue::Null,
        }
    }

    /// `init?(json:)`.
    pub fn from_json(v: &JsonValue) -> Option<MinecraftEvent> {
        Some(MinecraftEvent {
            at: v.get("at")?.as_str()?.to_string(),
            source: v
                .get("source")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            event: v.get("event")?.as_str()?.to_string(),
            params: v.get("params").cloned().unwrap_or(JsonValue::Null),
        })
    }

    /// Who it happened to: the log bridge's `params.player`, or a notification's player object
    /// (`params: [{id, name}]`, or `{player: {name}}`).
    pub fn player(&self) -> Option<String> {
        let p = &self.params;
        if let Some(s) = p.get("player").and_then(JsonValue::as_str) {
            return Some(s.to_string());
        }
        if let Some(s) = p
            .get("player")
            .and_then(|x| x.get("name"))
            .and_then(JsonValue::as_str)
        {
            return Some(s.to_string());
        }
        if let Some(s) = p
            .at(0)
            .and_then(|x| x.get("name"))
            .and_then(JsonValue::as_str)
        {
            return Some(s.to_string());
        }
        p.at(0)
            .and_then(|x| x.get("player"))
            .and_then(|x| x.get("name"))
            .and_then(JsonValue::as_str)
            .map(String::from)
    }

    /// What tells two events apart at the same `at`.
    fn key(&self) -> String {
        format!("{}\u{1F}{}", self.event, self.params.canonical_js())
    }

    /// The events in `text`: the tool's own array, `{events: [...]}`, or an MCP tool result
    /// (`{content: [{type: "text", text: "[...]"}]}`, or val_call's `{content: ["[...]"]}`)
    /// whose text is that array.
    pub fn parse(text: &str) -> Result<Vec<MinecraftEvent>, MinecraftEventError> {
        let v = JsonValue::parse(text)
            .map_err(|e: JsonError| MinecraftEventError::Json(format!("{e:?}")))?;
        Self::parse_json(&v)
    }

    pub fn parse_json(v: &JsonValue) -> Result<Vec<MinecraftEvent>, MinecraftEventError> {
        if let Some(a) = v.as_array() {
            // an array of events, or a content array of text parts
            if a.iter().all(|x| x.get("event").is_some()) {
                return Ok(a.iter().filter_map(MinecraftEvent::from_json).collect());
            }
            let mut out = Vec::new();
            for part in a {
                let t = part
                    .as_str()
                    .or_else(|| part.get("text").and_then(JsonValue::as_str));
                let Some(t) = t else { continue };
                out.extend(Self::parse(t)?);
            }
            return Ok(out);
        }
        if let Some(e) = v.get("events") {
            return Self::parse_json(e);
        }
        if let Some(c) = v.get("content") {
            return Self::parse_json(c);
        }
        if let Some(r) = v.get("result") {
            return Self::parse_json(r);
        }
        Err(MinecraftEventError::Shape(
            "expected mc_recent_events' array of {at, source, event, params} (or an MCP result)"
                .into(),
        ))
    }

    /// The event as the `minecraftEvents` request carries it.
    pub fn json(&self) -> JsonValue {
        JsonObject::from_pairs([
            ("at", JsonValue::str(self.at.as_str())),
            ("source", JsonValue::str(self.source.as_str())),
            ("event", JsonValue::str(self.event.as_str())),
            ("params", self.params.clone()),
        ])
        .into()
    }
}

/// What an event plays on a deck (`DeckAction` for the ops the map names). A stinger is the
/// one the feed limits to a bar; any other op is carried as its JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum MappedAction {
    Stinger { deck: String, n: i64 },
    Other(JsonValue),
}

impl MappedAction {
    /// `DeckAction.json`: `{"op":"stinger","deck":…,"n":…}`.
    pub fn json(&self) -> JsonValue {
        match self {
            MappedAction::Stinger { deck, n } => JsonObject::from_pairs([
                ("op", JsonValue::str("stinger")),
                ("deck", JsonValue::str(deck.as_str())),
                ("n", JsonValue::int(*n)),
            ])
            .into(),
            MappedAction::Other(v) => v.clone(),
        }
    }
}

/// Which event plays what: `minecraft-events.json`'s `events`, event name → a deck action as
/// dj_perform takes it, without the deck (`{"op": "stinger", "n": 0}`).
#[derive(Clone, Debug, PartialEq)]
pub struct MinecraftEventMap {
    pub actions: BTreeMap<String, JsonValue>,
}

impl MinecraftEventMap {
    pub fn from_json(v: &JsonValue) -> MinecraftEventMap {
        let mut a = BTreeMap::new();
        if let Some(events) = v.get("events").and_then(JsonValue::as_object) {
            for (k, e) in events.iter() {
                if e.get("op").and_then(JsonValue::as_str).is_some() {
                    a.insert(k.to_string(), e.clone());
                }
            }
        }
        MinecraftEventMap { actions: a }
    }

    /// The bundled map; empty if it somehow does not parse.
    pub fn bundled() -> MinecraftEventMap {
        JsonValue::parse(EVENTS_JSON)
            .map(|v| Self::from_json(&v))
            .unwrap_or(MinecraftEventMap {
                actions: BTreeMap::new(),
            })
    }

    /// The action `e` plays on `deck`, if the map names one.
    pub fn action(&self, e: &MinecraftEvent, deck: &str) -> Option<MappedAction> {
        let a = self.actions.get(&e.event)?;
        match a.get("op")?.as_str()? {
            "stinger" => Some(MappedAction::Stinger {
                deck: deck.into(),
                n: a.get("n").and_then(JsonValue::as_i64).unwrap_or(0),
            }),
            _ => {
                let mut o = a.as_object()?.clone();
                o.set("deck", JsonValue::str(deck));
                // `DeckAction(json:)`: an op that is not an action is not mapped
                let json: JsonValue = o.into();
                crate::session::action::DeckAction::from_json(&json)?;
                Some(MappedAction::Other(json))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub event: MinecraftEvent,
    pub action: MappedAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    pub event: MinecraftEvent,
    pub reason: String,
}

/// The poller's memory: a cursor over the event stream (so a re-read window fires nothing
/// twice) and, per deck, the bar its last stinger went on (`MinecraftEventFeed`).
#[derive(Clone, Debug)]
pub struct MinecraftEventFeed {
    pub map: MinecraftEventMap,
    /// The newest `at` taken so far (None: nothing yet).
    cursor: Option<String>,
    /// The events already taken whose `at` is the cursor (several can share a millisecond).
    at_cursor: BTreeSet<String>,
    /// Per deck, the bar its last stinger fired on.
    stinger_bar: BTreeMap<String, i64>,
}

impl MinecraftEventFeed {
    pub fn new(map: MinecraftEventMap) -> MinecraftEventFeed {
        MinecraftEventFeed {
            map,
            cursor: None,
            at_cursor: BTreeSet::new(),
            stinger_bar: BTreeMap::new(),
        }
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// The events in `events` not taken before, oldest first, and what each plays on `deck`.
    /// `bar` is where the deck is now (None when it has no grid: then one stinger per call);
    /// `net` is false when the deck can't play stingers. `prime` takes the events without
    /// playing any (start the loop without replaying the buffer's backlog).
    pub fn take(
        &mut self,
        events: &[MinecraftEvent],
        deck: &str,
        bar: Option<i64>,
        net: bool,
        prime: bool,
    ) -> (Vec<Fired>, Vec<Skipped>) {
        let mut fresh: Vec<(usize, &MinecraftEvent)> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| match &self.cursor {
                None => true,
                Some(c) => e.at > *c || (e.at == *c && !self.at_cursor.contains(&e.key())),
            })
            .collect();
        fresh.sort_by(|a, b| {
            if a.1.at != b.1.at {
                a.1.at.cmp(&b.1.at)
            } else {
                a.0.cmp(&b.0)
            }
        });
        let mut fired = Vec::new();
        let mut skipped = Vec::new();
        let mut stung_this_call = false;
        for (_, e) in fresh {
            if self.cursor.as_deref() != Some(e.at.as_str()) {
                self.cursor = Some(e.at.clone());
                self.at_cursor.clear();
            }
            self.at_cursor.insert(e.key());
            if prime {
                skipped.push(Skipped {
                    event: e.clone(),
                    reason: "primed".into(),
                });
                continue;
            }
            let Some(action) = self.map.action(e, deck) else {
                skipped.push(Skipped {
                    event: e.clone(),
                    reason: "unmapped".into(),
                });
                continue;
            };
            if matches!(action, MappedAction::Stinger { .. }) {
                if !net {
                    skipped.push(Skipped {
                        event: e.clone(),
                        reason: format!("deck {deck} is not playing a net"),
                    });
                    continue;
                }
                if let Some(bar) = bar {
                    if self.stinger_bar.get(deck) == Some(&bar) {
                        skipped.push(Skipped {
                            event: e.clone(),
                            reason: format!("a stinger already fired in bar {bar}"),
                        });
                        continue;
                    }
                    self.stinger_bar.insert(deck.to_string(), bar);
                } else if stung_this_call {
                    skipped.push(Skipped {
                        event: e.clone(),
                        reason: "one stinger per poll without a beat grid".into(),
                    });
                    continue;
                }
                stung_this_call = true;
            }
            fired.push(Fired {
                event: e.clone(),
                action,
            });
        }
        (fired, skipped)
    }

    /// The `minecraftEvents` reply: what fired, what didn't and why, and the cursor.
    pub fn report(
        deck: &str,
        fired: &[Fired],
        skipped: &[Skipped],
        cursor: Option<&str>,
    ) -> JsonValue {
        fn ev(e: &MinecraftEvent) -> JsonObject {
            let mut o = JsonObject::from_pairs([
                ("at", JsonValue::str(e.at.as_str())),
                ("event", JsonValue::str(e.event.as_str())),
            ]);
            if let Some(p) = e.player() {
                o.set("player", JsonValue::String(p));
            }
            o
        }
        JsonObject::from_pairs([
            ("deck", JsonValue::str(deck)),
            (
                "fired",
                JsonValue::Array(
                    fired
                        .iter()
                        .map(|f| {
                            let mut o = ev(&f.event);
                            o.set("action", f.action.json());
                            o.into()
                        })
                        .collect(),
                ),
            ),
            (
                "skipped",
                JsonValue::Array(
                    skipped
                        .iter()
                        .map(|s| {
                            let mut o = ev(&s.event);
                            o.set("reason", JsonValue::str(s.reason.as_str()));
                            o.into()
                        })
                        .collect(),
                ),
            ),
            (
                "cursor",
                cursor.map(JsonValue::str).unwrap_or(JsonValue::Null),
            ),
        ])
        .into()
    }
}

/// `djctl minecraft events`: stdin (an `mc_recent_events` result) → the `minecraftEvents`
/// request's params.
pub fn events_command_params(
    stdin: &str,
    deck: Option<&str>,
    prime: bool,
) -> Result<JsonValue, MinecraftEventError> {
    let events = MinecraftEvent::parse(stdin)?;
    let mut o = JsonObject::from_pairs([(
        "events",
        JsonValue::Array(events.iter().map(MinecraftEvent::json).collect()),
    )]);
    if let Some(d) = deck {
        o.set("deck", JsonValue::str(d));
    }
    if prime {
        o.set("prime", JsonValue::Bool(true));
    }
    Ok(o.into())
}

/// One line per event the app fired, then the skipped count (`describe`).
pub fn describe_reply(reply: &JsonValue) -> String {
    let mut lines: Vec<String> = Vec::new();
    let deck = reply.get("deck").and_then(JsonValue::as_str).unwrap_or("?");
    let fired = reply.get("fired").and_then(JsonValue::as_array);
    for f in fired.into_iter().flatten() {
        let who = f
            .get("player")
            .and_then(JsonValue::as_str)
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        lines.push(format!(
            "{}  {}{} → {} {}",
            f.get("at").and_then(JsonValue::as_str).unwrap_or(""),
            f.get("event").and_then(JsonValue::as_str).unwrap_or(""),
            who,
            deck,
            f.get("action")
                .map(|a| a.js_stringified())
                .unwrap_or_default()
        ));
    }
    let skipped = reply
        .get("skipped")
        .and_then(JsonValue::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    lines.push(format!(
        "fired {}, skipped {}",
        fired.map(|a| a.len()).unwrap_or(0),
        skipped
    ));
    lines.join("\n")
}
