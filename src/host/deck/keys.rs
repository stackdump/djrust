//! Live keys on a deck (docs/MPK.md, "Keys"): where a controller's piano keys go, and when.
//!
//! A deck's *live voice* is the runner's pooled lead lane (`<live>`: `WaveRunner::note_on`,
//! eight voices, the deck's melody sound), which is what a key plays. [`Engine::key_down`] already
//! put a note on it, scale-locked; this adds what a player needs around that:
//!
//! * the **route**: one switch for the whole hub, [`KeyMode`] (play a deck / transpose the loops
//!   / play the TD-3), which deck (A, B, or the aimed one) and how it lands;
//! * **quantise**: `now`, `beat` or `bar`. A key pressed off the grid is held and played on the
//!   next beat (or bar) of the hub clock, the one the loop pads line up with; its release comes
//!   after at least a quarter beat, so a tap still sounds. The release of a key that was not held
//!   back is never held back;
//! * **replay**: a live key is a `keys` live op (`ArrangeOp::Live`), so a jam's arrangement keeps
//!   it, and `render_arrangement` plays it. The MIDI that made it is also in the journal.
//!
//! Quantised keys land within one service pass (about a UI frame) of the grid, not on the exact
//! sample: the commands to a deck are taken at the top of a block anyway, and a timestamped
//! `Command` would mean splitting the runner's block.

use crate::control::controller::KeyMode;
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::loop_pads::Quant;

/// A key held back for the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingKey {
    /// Loop-clock frame it plays on.
    pub due: i64,
    pub deck: String,
    pub note: i64,
    pub velocity: i64,
    pub down: bool,
}

/// Where the keys go.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyState {
    pub mode: KeyMode,
    /// The deck the keys play; None: the aimed one.
    pub deck: Option<String>,
    /// How a key press lands unless the binding says.
    pub quant: Quant,
    /// Snap keys to the song's scale (`Deck.scaleLocked`).
    pub scale_lock: bool,
    pub pending: Vec<PendingKey>,
    /// Keys played, for the status.
    pub played: u64,
}

impl Default for KeyState {
    fn default() -> Self {
        KeyState {
            mode: KeyMode::Live,
            deck: None,
            quant: Quant::Now,
            scale_lock: true,
            pending: Vec::new(),
            played: 0,
        }
    }
}

impl Quant {
    pub fn name(self) -> &'static str {
        match self {
            Quant::Now => "now",
            Quant::Beat => "beat",
            Quant::Bar => "bar",
        }
    }
}

/// How long a released key is held back at least after the note it follows, in beats.
const MIN_GATE_BEATS: f64 = 0.25;
/// Beats in a bar, for `bar` (the hub's grid is 4/4).
const BEATS_PER_BAR: i64 = 4;

impl Engine {
    /// The deck a key plays: the route's, else the aimed one.
    pub fn key_target(&self) -> String {
        self.keys
            .deck
            .clone()
            .filter(|d| self.deck(d).is_some())
            .unwrap_or_else(|| self.focus.clone())
    }

    /// Switch what the keys do. Held notes are let go, pending ones dropped, so nothing hangs.
    pub fn set_key_mode(&mut self, mode: KeyMode) {
        if self.keys.mode != mode {
            self.keys_off();
            self.keys.mode = mode;
        }
    }

    /// Live <-> transpose (the TD-3 mode goes back to live).
    pub fn toggle_key_mode(&mut self) -> KeyMode {
        let next = match self.keys.mode {
            KeyMode::Live => KeyMode::Transpose,
            KeyMode::Transpose | KeyMode::Td3 => KeyMode::Live,
        };
        self.set_key_mode(next);
        next
    }

    /// Aim the keys at a deck: "A", "B", "aimed" (or "*", ""), or "next" (the deck after the
    /// current one, the aimed one after the last).
    pub fn set_key_deck(&mut self, to: &str) -> Result<(), String> {
        let t = to.trim();
        let new = match t.to_ascii_lowercase().as_str() {
            "" | "*" | "aimed" | "focus" | "none" => None,
            "next" | "other" => {
                let names = self.deck_names_sorted();
                let cur = self.keys.deck.clone();
                match cur.and_then(|c| names.iter().position(|n| *n == c)) {
                    None => names.first().cloned(),
                    Some(i) => names.get(i + 1).cloned(),
                }
            }
            _ => {
                let name = t.to_ascii_uppercase();
                if self.deck(&name).is_none() {
                    return Err(format!(
                        "no deck {t:?} (A, B, aimed or next; decks: {})",
                        self.deck_names_sorted().join(", ")
                    ));
                }
                Some(name)
            }
        };
        if new != self.keys.deck {
            self.keys_off();
            self.keys.deck = new;
        }
        Ok(())
    }

    fn deck_names_sorted(&self) -> Vec<String> {
        let mut v: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        v.sort();
        v
    }

    /// The frame (loop clock) the next beat or bar falls on, None when keys should play at once
    /// (`now`, or no clock to quantise to).
    fn key_due(&self, q: Quant) -> Option<i64> {
        let every = match q {
            Quant::Now => return None,
            Quant::Beat => 1,
            Quant::Bar => BEATS_PER_BAR,
        };
        let clock = self.hub_clock();
        if clock.frames_per_beat.is_nan() || clock.frames_per_beat <= 0.0 {
            return None;
        }
        let now = self.loop_frames();
        let due = clock.next_boundary(now, every);
        (due > now).then_some(due)
    }

    /// A key on `deck`, from a controller, the socket or a replay. `quant`: "now" / "beat" /
    /// "bar", else the route's.
    pub fn live_key(&mut self, deck: &str, note: i64, velocity: i64, down: bool, quant: Option<&str>) {
        if !self.is_net_deck(deck) {
            if down {
                self.unhandled_push(format!("keys (deck {deck} has no live voice: it plays a sample)"));
            }
            return;
        }
        let q = Quant::parse(quant, self.keys.quant);
        if down {
            match self.key_due(q) {
                None => self.play_key(deck, note, velocity, true),
                Some(due) => self.keys.pending.push(PendingKey {
                    due,
                    deck: deck.to_string(),
                    note,
                    velocity,
                    down: true,
                }),
            }
            return;
        }
        // a release: after its own held-back press, if it has one
        let held_back = self
            .keys
            .pending
            .iter()
            .rev()
            .find(|p| p.down && p.deck == deck && p.note == note)
            .map(|p| p.due);
        match held_back {
            None => self.play_key(deck, note, 0, false),
            Some(due) => {
                let gate = (self.hub_clock().frames_per_beat * MIN_GATE_BEATS).max(1.0) as i64;
                self.keys.pending.push(PendingKey {
                    due: due + gate,
                    deck: deck.to_string(),
                    note,
                    velocity: 0,
                    down: false,
                });
            }
        }
    }

    fn play_key(&mut self, deck: &str, note: i64, velocity: i64, down: bool) {
        if down {
            self.key_down(deck, note, velocity);
            self.keys.played += 1;
        } else {
            self.key_up(deck, note);
        }
    }

    /// Play the held-back keys whose frame has come. Call it every service pass.
    pub fn service_keys(&mut self) {
        if self.keys.pending.is_empty() {
            return;
        }
        let now = self.loop_frames();
        // half a UI pass early: the command is taken at the next block, so this centres the
        // error on the grid instead of leaving it all late
        let lead = (self.sample_rate * 0.008) as i64;
        while !self.keys.pending.is_empty() {
            // in order: a release never passes its press
            if self.keys.pending[0].due - lead > now {
                break;
            }
            let k = self.keys.pending.remove(0);
            self.play_key(&k.deck, k.note, k.velocity, k.down);
        }
    }

    /// Let go of everything: pending keys dropped, held ones released.
    pub fn keys_off(&mut self) {
        self.keys.pending.clear();
        let decks: Vec<String> = self
            .decks
            .iter()
            .filter(|d| !d.held_keys.is_empty())
            .map(|d| d.name.clone())
            .collect();
        for d in decks {
            let held: Vec<i64> = self.deck(&d).map(|x| x.held_keys.keys().copied().collect()).unwrap_or_default();
            for n in held {
                self.key_up(&d, n);
            }
        }
    }

    fn unhandled_push(&mut self, what: String) {
        if self.unhandled.len() < 256 {
            self.unhandled.push(what);
        }
    }

    /// The route and its queue, for the socket's `keys status`.
    pub fn keys_json(&self) -> JsonValue {
        let held: Vec<JsonValue> = self
            .decks
            .iter()
            .flat_map(|d| {
                d.held_keys
                    .values()
                    .map(|n| JsonValue::int(*n))
                    .collect::<Vec<_>>()
            })
            .collect();
        JsonValue::Object(JsonObject::from_pairs([
            ("mode", JsonValue::str(self.keys.mode.raw())),
            (
                "deck",
                JsonValue::str(self.keys.deck.clone().unwrap_or_else(|| "aimed".into())),
            ),
            ("target", JsonValue::str(self.key_target())),
            ("quant", JsonValue::str(self.keys.quant.name())),
            ("scaleLock", JsonValue::Bool(self.keys.scale_lock)),
            ("held", JsonValue::Array(held)),
            ("pending", JsonValue::int(self.keys.pending.len() as i64)),
            ("played", JsonValue::int(self.keys.played as i64)),
        ]))
    }

    /// The `keys` live op (docs/ACTION-LOG.md): `{deck?, note, velocity?, down?, quant?}`; or
    /// `{mode}` / `{quant}` / `{deck_to}` to set the route; `{off: true}` lets go of everything.
    pub fn live_keys(&mut self, p: &JsonValue) -> Result<(), String> {
        let text = |k: &str| match p.get(k) {
            Some(JsonValue::String(s)) => Some(s.clone()),
            Some(JsonValue::Null) | None => None,
            Some(o) => Some(o.js_stringified()),
        };
        let flag = |k: &str| match p.get(k) {
            Some(JsonValue::Bool(b)) => Some(*b),
            Some(JsonValue::Number(n)) => Some(*n != 0.0),
            Some(JsonValue::String(s)) => match s.as_str() {
                "on" | "true" | "1" => Some(true),
                "off" | "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        };
        let mut did = false;
        if let Some(m) = text("mode") {
            let mode = if m == "toggle" {
                self.toggle_key_mode();
                None
            } else {
                Some(KeyMode::parse(&m).ok_or_else(|| format!("keys mode {m:?}: live, transpose, td3 or toggle"))?)
            };
            if let Some(mode) = mode {
                self.set_key_mode(mode);
            }
            did = true;
        }
        if let Some(q) = text("quant") {
            if !matches!(q.as_str(), "now" | "beat" | "bar") && p.get("note").is_none() {
                return Err(format!("keys quant {q:?}: now, beat or bar"));
            }
            if p.get("note").is_none() {
                self.keys.quant = Quant::parse(Some(q.as_str()), self.keys.quant);
                did = true;
            }
        }
        if let Some(d) = text("deck_to") {
            self.set_key_deck(&d)?;
            did = true;
        }
        if let Some(on) = flag("scale_lock") {
            self.keys.scale_lock = on;
            did = true;
        }
        if flag("off") == Some(true) {
            self.keys_off();
            did = true;
        }
        if let Some(n) = p.get("note").and_then(note_number) {
            let deck = text("deck").map(|d| d.to_ascii_uppercase()).unwrap_or_else(|| self.key_target());
            if self.deck(&deck).is_none() {
                return Err(format!("keys: no deck {deck:?}"));
            }
            let vel = p.get("velocity").and_then(JsonValue::as_f64).unwrap_or(100.0) as i64;
            let down = flag("down").unwrap_or(true);
            self.live_key(&deck, n, vel, down, text("quant").as_deref());
            did = true;
        }
        if did {
            Ok(())
        } else {
            Err("keys: needs note, mode, quant, deck_to, scale_lock or off".into())
        }
    }
}

fn note_number(v: &JsonValue) -> Option<i64> {
    super::live_ops::note_of(v).map(i64::from)
}
