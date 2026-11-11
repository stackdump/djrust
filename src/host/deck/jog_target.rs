//! Where a deck's jog wheel goes when `--history-jog` aims it at a history (beyond Swift;
//! docs/INPUT-HISTORY.md, "The jog follows the deck").
//!
//! The aim (`Engine::history_jog`) is only a *candidate*. The wheel follows what is on the deck:
//!
//! - **The deck holds a track or a loop**: the platter scratches the deck, as on any deck, and
//!   CUE / LOOP IN / LOOP OUT / ÷2 / ×2 are the deck's own.
//! - **The deck is empty**, or **a modifier is held**: the platter scrubs the history, and those
//!   buttons are the history's (back to live, loop beats of it).
//!
//! The modifiers are the controller's **SHIFT** (the Inpulse's hardware SHIFT layer: while it is
//! held the platter's messages arrive on the shift channel, so the router passes the flag
//! through [`RouterHost::jog_shift`](crate::control::router::RouterHost::jog_shift)), and any
//! control bound to **`historyJog`** (a hold: press = on, release = off), from a profile's
//! bindings or a user map.
//!
//! A touch latches its target: the wheel goes where it went at touch-down until it is let go,
//! so pressing or releasing SHIFT mid-scratch, or a loop landing on the deck, never splits one
//! gesture across two clocks. Each change of target is logged in the action trace in words.

use super::engine::Engine;
use super::strip_recall::RecallId;

/// What a deck's jog wheel drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JogTarget {
    /// The deck itself (its track or loop).
    Deck,
    /// A strip's history.
    History(RecallId),
}

/// Why the wheel goes where it does (the trace's words, the deck's indicator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JogWhy {
    /// No history aim on this deck: the wheel is the deck's, as always.
    NoAim,
    /// The deck holds something: the wheel follows it.
    Loaded,
    /// The deck is empty: the wheel scrubs the history.
    Empty,
    /// SHIFT is held on the controller.
    Shift,
    /// A control bound to `historyJog` is held.
    Hold,
}

impl JogWhy {
    pub fn words(self) -> &'static str {
        match self {
            JogWhy::NoAim => "no history aim",
            JogWhy::Loaded => "deck loaded",
            JogWhy::Empty => "deck empty",
            JogWhy::Shift => "SHIFT held",
            JogWhy::Hold => "historyJog held",
        }
    }
}

/// The SHIFT layer's coarse seek on the wheel (`JogAdapter::new`'s shift multiplier): a scrub
/// that went to a history because SHIFT was held is taken back to 1x, so the history scratches
/// as it did before the modifier existed.
const SHIFT_MULTIPLIER: f64 = crate::control::jog::SHIFT_MULTIPLIER;

/// The aimed wheel's state, on the engine.
#[derive(Clone, Debug, Default)]
pub struct JogFollow {
    /// Decks with a `historyJog` control held ("" = whichever deck is aimed).
    held: Vec<String>,
    /// A touch in progress: the deck, where it went at touch-down and why.
    latch: Vec<(String, JogTarget, JogWhy)>,
    /// The last target logged (the aimed deck's), to log only the switches.
    shown: Option<(String, JogTarget)>,
}

impl Engine {
    /// The history a deck's jog is aimed at (`--history-jog`), whatever the deck holds now.
    pub fn history_jog_aim(&self, deck: &str) -> Option<RecallId> {
        self.history_jog
            .as_ref()
            .filter(|(d, _)| d.eq_ignore_ascii_case(deck))
            .map(|(_, i)| *i)
    }

    /// The deck holds a track (a net) or a loop / file (a sample on the platter).
    pub fn deck_loaded(&self, deck: &str) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.sample.is_some() || d.project.is_some())
    }

    fn jog_hold_on(&self, deck: &str) -> bool {
        self.jog_follow
            .held
            .iter()
            .any(|d| d.is_empty() || d.eq_ignore_ascii_case(deck))
    }

    /// Where the wheel would go on a touch now, with SHIFT held or not.
    pub fn jog_target_with(&self, deck: &str, shift: bool) -> (JogTarget, JogWhy) {
        let Some(i) = self.history_jog_aim(deck) else {
            return (JogTarget::Deck, JogWhy::NoAim);
        };
        if shift {
            (JogTarget::History(i), JogWhy::Shift)
        } else if self.jog_hold_on(deck) {
            (JogTarget::History(i), JogWhy::Hold)
        } else if !self.deck_loaded(deck) {
            (JogTarget::History(i), JogWhy::Empty)
        } else {
            (JogTarget::Deck, JogWhy::Loaded)
        }
    }

    /// Where the deck's wheel goes now: a touch's latch while one is held, else the rule
    /// (SHIFT is only known on the wheel's own messages, so it counts from touch-down).
    pub fn jog_target(&self, deck: &str) -> (JogTarget, JogWhy) {
        if let Some((_, t, why)) = self.jog_follow.latch.iter().find(|(d, ..)| d == deck) {
            return (*t, *why);
        }
        self.jog_target_with(deck, false)
    }

    /// The history the deck's wheel and its CUE / LOOP buttons drive now: the aim, while the
    /// deck is empty or a modifier holds it there. None: they are the deck's own.
    pub fn history_aimed(&self, deck: &str) -> Option<RecallId> {
        match self.jog_target(deck).0 {
            JogTarget::History(i) => Some(i),
            JogTarget::Deck => None,
        }
    }

    /// A `historyJog` control pressed or let go (`deck`: that deck's wheel; None: the aimed one).
    pub fn history_jog_hold(&mut self, deck: Option<&str>, on: bool) {
        let d = deck.map(str::to_uppercase).unwrap_or_default();
        self.jog_follow.held.retain(|x| *x != d);
        if on {
            self.jog_follow.held.push(d);
        }
        self.note_jog_targets();
    }

    /// Touch-down: decide and latch where this gesture goes.
    pub(crate) fn jog_latch(&mut self, deck: &str, shift: bool) -> JogTarget {
        let (t, why) = self.jog_target_with(deck, shift);
        self.jog_follow.latch.retain(|(d, ..)| d != deck);
        self.jog_follow.latch.push((deck.to_string(), t, why));
        self.note_jog_targets();
        t
    }

    /// Let go: the latch ends (after the release went where the touch did).
    pub(crate) fn jog_unlatch(&mut self, deck: &str) {
        self.jog_follow.latch.retain(|(d, ..)| d != deck);
        self.note_jog_targets();
    }

    /// A scrub's platter seconds for its target: SHIFT's coarse seek undone on a history the
    /// modifier sent it to.
    pub(crate) fn jog_scrub_seconds(&self, deck: &str, seconds: f64, shift: bool) -> f64 {
        match self.jog_target(deck) {
            (JogTarget::History(_), _) if shift => seconds / SHIFT_MULTIPLIER,
            _ => seconds,
        }
    }

    /// Log the aimed deck's target when it has changed (a load, an eject, a modifier, a touch).
    /// Called on every change of the inputs to the rule and from `service_jogs`.
    pub fn note_jog_targets(&mut self) {
        let Some((deck, _)) = self.history_jog.clone() else {
            self.jog_follow.shown = None;
            return;
        };
        let (t, why) = self.jog_target(&deck);
        let now = Some((deck.clone(), t));
        if self.jog_follow.shown == now {
            return;
        }
        let first = self.jog_follow.shown.is_none();
        self.jog_follow.shown = now;
        let what = self.jog_target_words(&deck, t);
        let verb = if first { "is on" } else { "switched to" };
        self.trace_push_text(&format!("jog {deck} {verb} {what} ({})", why.words()));
    }

    /// The deck's indicator: `LOOP`, `TRACK`, `DECK` or `<label> HIST`.
    pub fn jog_target_words(&self, deck: &str, t: JogTarget) -> String {
        match t {
            JogTarget::History(i) => format!("{} HIST", self.recall_label(i)),
            JogTarget::Deck => match self.deck(deck) {
                Some(d) if d.sample.is_some() => "LOOP".into(),
                Some(d) if d.project.is_some() => "TRACK".into(),
                _ => "DECK".into(),
            },
        }
    }
}
