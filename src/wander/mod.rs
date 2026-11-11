//! Wander: the theme's random walk made autonomous (port of `Wander.swift`).
//!
//! A net deck with wander on takes one move in configuration space per phrase until the
//! performer grabs the browser wheel. Each move is a pure function of the deck's configuration
//! (its [`TrackSpec`], roles, locks) and the step index, never the wall clock, so a recorded
//! session replays the same walk: the moves are derived, not recorded; only the pokes that set
//! wander up are.
//!
//! This module is the pure half: the lock vocabulary, the move kinds and their weights, the
//! walk's [`WanderState`] and its session codec, the SplitMix64 draw ([`WanderRandom`]) and the
//! planner ([`Wander::plan`]). The deck half of `Wander.swift` needs a live deck: what a deck
//! tells the planner (`wanderContext` / `wanderVoices` / `wanderKey`) is [`deck`], as functions
//! of explicit inputs, and `Deck.advanceWander`, `takeWanderMove`, the look-ahead slot and the
//! moves are `crate::host::deck::wander`. What of it is a function of explicit inputs and lives
//! here: [`bar_countdown`],
//! [`is_move_bar`], [`WanderState::advance`] (the bar bookkeeping), [`WanderState::set_on`],
//! [`WanderState::set_lock_anchored`], [`WanderState::fill_anchors`], [`WanderState::idle_reason`],
//! [`WanderState::deck_json`] and [`Wander::played_key`].
//!
//! Dependencies the Swift planner reaches into the app for are inputs here: the sound
//! palettes a hop may reach ([`WanderPalettes`]), the loops a layer move may draw from
//! (`layer_pool`) and the deck's channels. They are plain fields of [`WanderContext`].

pub mod carry;
pub mod deck;
pub mod layers;
mod plan;
mod rng;

pub use layers::{default_gain, LayerEntry, LayerLoop};
pub use plan::{
    PaletteHop, Wander, WanderAction, WanderContext, WanderPalettes, WanderPin, WanderPlan,
    WanderVoice, SEEDABLE_PARTS,
};
pub use rng::WanderRandom;

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};
use crate::spec::TrackSpec;

/// What the walk must not change on a deck (`WanderLock`). Each removes move kinds or
/// constrains them; per-role keeps are `keep:<role>`.
pub struct WanderLock;

impl WanderLock {
    /// The dimensions, in the order the UI shows their chips.
    pub const DIMENSIONS: [&'static str; 9] = [
        "genre",
        "tempo",
        "key",
        "structure",
        "instruments",
        "arrangement",
        "fx",
        "palette",
        "layers",
    ];
    pub const KEEP_PREFIX: &'static str = "keep:";
    /// How far (either way, as a fraction) a tempo-locked walk may move from the tempo the lock
    /// was set at: a genre hop beyond it is never taken.
    pub const TEMPO_TOLERANCE: f64 = 0.06;

    /// The lock that keeps `role` ("keep the bass").
    pub fn keep(role: &str) -> String {
        alloc::format!("{}{}", Self::KEEP_PREFIX, role)
    }

    /// The role a `keep:` lock names, None for any other.
    pub fn kept_role(dimension: &str) -> Option<&str> {
        dimension.strip_prefix(Self::KEEP_PREFIX)
    }

    pub fn is_valid(dimension: &str) -> bool {
        Self::DIMENSIONS.contains(&dimension)
            || !Self::kept_role(dimension).unwrap_or("").is_empty()
    }

    /// "keep bass", "Genre".
    pub fn label(dimension: &str) -> String {
        if let Some(r) = Self::kept_role(dimension) {
            return alloc::format!("keep {r}");
        }
        let mut cs = dimension.chars();
        match cs.next() {
            Some(c) => c.to_uppercase().chain(cs).collect(),
            None => String::new(),
        }
    }
}

/// The kinds of move, from the small ones taken most often to the rare big ones
/// (`WanderMoveKind`, in declaration order: the draw walks this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WanderMoveKind {
    /// Mute or unmute one role (never a disabled, kept or locked one).
    Role,
    /// A STEMS group (music / mic) off, or back on.
    Stem,
    /// A net macro over the phrase's last bar.
    Macro,
    /// A master-chain FX macro from the phrase end.
    Fx,
    /// A ring stinger.
    Stinger,
    /// The next seed nearby (±1…7).
    Seed,
    /// A nearby seed with another instrument on one tonal role.
    Instrument,
    /// The same seed in another structure (songs only).
    Structure,
    /// A neighbouring genre, a new seed.
    Genre,
    /// The same song in a neighbouring sound palette.
    Palette,
    /// A library sound comes in over the track as a parallel loop.
    LayerAdd,
    /// A looping layer is swapped for another loop of its kind.
    LayerSwap,
    /// A looping layer goes out at the next bar.
    LayerRemove,
}

impl WanderMoveKind {
    /// `CaseIterable.allCases`.
    pub const ALL: [WanderMoveKind; 13] = [
        WanderMoveKind::Role,
        WanderMoveKind::Stem,
        WanderMoveKind::Macro,
        WanderMoveKind::Fx,
        WanderMoveKind::Stinger,
        WanderMoveKind::Seed,
        WanderMoveKind::Instrument,
        WanderMoveKind::Structure,
        WanderMoveKind::Genre,
        WanderMoveKind::Palette,
        WanderMoveKind::LayerAdd,
        WanderMoveKind::LayerSwap,
        WanderMoveKind::LayerRemove,
    ];

    /// `rawValue`.
    pub fn raw(self) -> &'static str {
        use WanderMoveKind::*;
        match self {
            Role => "role",
            Stem => "stem",
            Macro => "macro",
            Fx => "fx",
            Stinger => "stinger",
            Seed => "seed",
            Instrument => "instrument",
            Structure => "structure",
            Genre => "genre",
            Palette => "palette",
            LayerAdd => "layerAdd",
            LayerSwap => "layerSwap",
            LayerRemove => "layerRemove",
        }
    }

    pub fn from_raw(s: &str) -> Option<WanderMoveKind> {
        Self::ALL.iter().copied().find(|k| k.raw() == s)
    }

    /// Out of 100. Tuned for a walk you can follow: about two moves in three change the mix of
    /// the track playing, one in three moves to another track, one in twenty to another genre.
    pub fn weight(self) -> i64 {
        use WanderMoveKind::*;
        match self {
            Role => 28,
            Stem => 6,
            Macro => 13,
            Fx => 8,
            Stinger => 5,
            Seed => 20,
            Instrument => 9,
            Structure => 4,
            Genre => 5,
            Palette => 2,
            LayerAdd => 6,
            LayerSwap => 4,
            LayerRemove => 3,
        }
    }

    /// Moves that put another track on the deck (swapped in at the next bar).
    pub fn swaps(self) -> bool {
        use WanderMoveKind::*;
        matches!(self, Seed | Instrument | Structure | Genre | Palette)
    }

    /// The lock dimension that removes this kind (None: none removes it outright).
    pub fn lock(self) -> Option<&'static str> {
        use WanderMoveKind::*;
        match self {
            Role | Stem => Some("arrangement"),
            Macro | Fx | Stinger => Some("fx"),
            Instrument => Some("instruments"),
            Structure => Some("structure"),
            Genre => Some("genre"),
            Palette => Some("palette"),
            LayerAdd | LayerSwap | LayerRemove => Some("layers"),
            Seed => None,
        }
    }
}

/// The key a key-locked walk keeps: the genre's scale and the root pitch class the track played
/// in when the lock was set, and its offset from the genre's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WanderKey {
    pub scale: String,
    pub root_pc: i64,
    /// Semitones from the genre's root (−5…+6): what the spec pins as `extra["keyOffset"]`.
    pub offset: i64,
}

impl WanderKey {
    /// `WanderKey.json`.
    pub fn json(&self) -> JsonValue {
        JsonObject::from_pairs([
            ("scale", JsonValue::str(self.scale.as_str())),
            ("rootPC", JsonValue::int(self.root_pc)),
            ("offset", JsonValue::int(self.offset)),
        ])
        .into()
    }

    /// `WanderKey(json:)`.
    pub fn from_json(v: Option<&JsonValue>) -> Option<WanderKey> {
        let v = v?;
        Some(WanderKey {
            scale: v.get("scale")?.as_str()?.to_string(),
            root_pc: v.get("rootPC")?.as_i64()?,
            offset: v.get("offset").and_then(JsonValue::as_i64).unwrap_or(0),
        })
    }
}

/// One move of the walk: the discovery trail (`WanderMove`).
#[derive(Clone, Debug, PartialEq)]
pub struct WanderMove {
    pub step: i64,
    /// The bar (1-based, on the deck's clock) it was taken in: the last of a phrase.
    pub bar: i64,
    pub kind: WanderMoveKind,
    /// What it did: "bass off", "drop", "seed 41 → 44", "techno → house · seed 512".
    pub detail: String,
    /// The configuration it put on the deck (a swap), or the one it played on.
    pub spec: Option<TrackSpec>,
    /// The share CID of `spec` for a swap (None for a move that kept the track).
    pub cid: Option<String>,
}

impl WanderMove {
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("step", JsonValue::int(self.step)),
            ("bar", JsonValue::int(self.bar)),
            ("kind", JsonValue::str(self.kind.raw())),
            ("move", JsonValue::str(self.detail.as_str())),
        ]);
        if let Some(cid) = &self.cid {
            o.set("cid", JsonValue::str(cid.as_str()));
        }
        if let Some(spec) = &self.spec {
            o.set("genre", JsonValue::str(spec.genre.as_str()));
            o.set("seed", JsonValue::Number(spec.seed));
        }
        o.into()
    }
}

/// A deck's walk: on or off, its pace, where it has got to, its locks (`WanderState`).
#[derive(Clone, Debug, PartialEq)]
pub struct WanderState {
    pub on: bool,
    /// Bars per move.
    pub pace: i64,
    /// Moves taken so far on this deck: with the spec, what the next one is drawn from.
    pub step: i64,
    pub locks: BTreeSet<String>,
    /// The tempo the tempo lock holds the walk to (set with the lock).
    pub anchor_bpm: Option<f64>,
    /// The key the key lock holds (set with the lock).
    pub key: Option<WanderKey>,
    /// The browser wheel turned on this deck: the walk waits for a quiet phrase.
    pub paused: bool,
    /// Bars since the wheel last turned, while paused.
    pub quiet_bars: i64,
    /// Newest last, at most [`WanderState::LOG_LIMIT`].
    pub log: Vec<WanderMove>,
    /// The bar the deck's clock was last seen in (None: not yet).
    pub last_bar: Option<i64>,
}

impl Default for WanderState {
    fn default() -> Self {
        WanderState {
            on: false,
            pace: Self::DEFAULT_PACE,
            step: 0,
            locks: BTreeSet::new(),
            anchor_bpm: None,
            key: None,
            paused: false,
            quiet_bars: 0,
            log: Vec::new(),
            last_bar: None,
        }
    }
}

impl WanderState {
    pub const PACES: [i64; 4] = [4, 8, 16, 32];
    pub const DEFAULT_PACE: i64 = 16;
    /// Moves the log keeps (newest last).
    pub const LOG_LIMIT: usize = 48;

    /// The roles a `keep:` lock names.
    pub fn kept(&self) -> BTreeSet<String> {
        self.locks
            .iter()
            .filter_map(|l| WanderLock::kept_role(l).map(String::from))
            .collect()
    }

    pub fn is_locked(&self, dimension: &str) -> bool {
        self.locks.contains(dimension)
    }

    /// The part a session's starting state keeps (no log, no clock reading).
    pub fn settings(&self) -> WanderState {
        let mut s = self.clone();
        s.log.clear();
        s.last_bar = None;
        s
    }

    /// `Deck.setWanderPace`: 4, 8, 16 or 32; anything else is ignored.
    pub fn set_pace(&mut self, bars: i64) {
        if Self::PACES.contains(&bars) {
            self.pace = bars;
        }
    }

    /// `Deck.stepWanderPace`: one pace longer (clockwise) or shorter per click, stopping at 4
    /// and 32.
    pub fn step_pace(&mut self, delta: i64) {
        let i = Self::PACES
            .iter()
            .position(|&p| p == self.pace)
            .unwrap_or(2) as i64;
        let j = (i + delta.signum()).clamp(0, Self::PACES.len() as i64 - 1);
        self.pace = Self::PACES[j as usize];
    }

    /// `Deck.setWander`: the walk on or off. On, it waits for the next phrase end: the bar the
    /// deck's clock is in now (`bar` = `tick / 16` of a net deck, None for a sample deck) does
    /// not count as a fresh one.
    pub fn set_on(&mut self, on: bool, bar: Option<i64>) {
        self.on = on;
        self.paused = false;
        self.quiet_bars = 0;
        self.last_bar = bar;
    }

    /// `Deck.setWanderLock` with its anchors: the tempo and key locks hold the tempo and key
    /// the deck plays now (`spec_bpm` = `Wander::bpm(spec)` of its spec, `played_key` =
    /// [`Wander::played_key`]); freeing them lets go. False for a dimension that is no lock.
    pub fn set_lock_anchored(
        &mut self,
        dimension: &str,
        on: bool,
        spec_bpm: Option<f64>,
        played_key: Option<WanderKey>,
    ) -> bool {
        if !self.set_lock(dimension, on) {
            return false;
        }
        match dimension {
            "tempo" => self.anchor_bpm = if on { spec_bpm } else { None },
            "key" => self.key = if on { played_key } else { None },
            _ => {}
        }
        true
    }

    /// The anchors `Deck.advanceWander` fills in at a move: a tempo or key lock restored at
    /// launch (no track then) holds what plays at its first move.
    pub fn fill_anchors(&mut self, spec_bpm: Option<f64>, played_key: Option<WanderKey>) {
        if self.is_locked("tempo") && self.anchor_bpm.is_none() {
            self.anchor_bpm = spec_bpm;
        }
        if self.is_locked("key") && self.key.is_none() {
            self.key = played_key;
        }
    }

    /// `Deck.wanderIdleReason`: why the walk is standing still, if it is. `net`: the deck
    /// plays a net; `ctx`: its context (None: nothing composed to walk from, with
    /// [`Wander::can_move`] already given the palettes the deck's host builds).
    pub fn idle_reason(&self, net: bool, ctx: Option<&WanderContext>) -> Option<&'static str> {
        if !self.on {
            return None;
        }
        if !net {
            return Some("a sample deck: wander walks live nets");
        }
        let Some(c) = ctx else {
            return Some("nothing composed to walk from");
        };
        if Wander::can_move(c) {
            None
        } else {
            Some("every move is locked: nothing left to change")
        }
    }

    /// `Deck.wanderJSON`, the JSON `dj_state` shows for the walk. `countdown`:
    /// [`WanderState::countdown`] of a net deck; `idle`: [`WanderState::idle_reason`].
    pub fn deck_json(&self, countdown: Option<i64>, idle: Option<&str>) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("on", JsonValue::Bool(self.on)),
            ("pace", JsonValue::int(self.pace)),
            ("paused", JsonValue::Bool(self.paused)),
            ("step", JsonValue::int(self.step)),
            (
                "locks",
                JsonValue::Array(
                    self.locks
                        .iter()
                        .map(|l| JsonValue::str(l.as_str()))
                        .collect(),
                ),
            ),
            (
                "log",
                JsonValue::Array(self.log.iter().rev().take(12).map(|m| m.json()).collect()),
            ),
        ]);
        if let Some(n) = countdown {
            o.set(
                if self.paused {
                    "resumesInBars"
                } else {
                    "nextMoveInBars"
                },
                JsonValue::int(n),
            );
        }
        if let Some(why) = idle {
            o.set("idle", JsonValue::str(why));
        }
        if let Some(a) = self.anchor_bpm {
            o.set("tempoAnchor", JsonValue::Number(a));
        }
        o.into()
    }

    /// `Deck.setWanderLock` (the state half: the tempo and key anchors are set by the caller
    /// from the deck, `anchor_bpm` = `Wander::bpm(spec)`, `key` = the deck's key; see
    /// [`set_lock_anchored`](Self::set_lock_anchored) for both at once).
    pub fn set_lock(&mut self, dimension: &str, on: bool) -> bool {
        if !WanderLock::is_valid(dimension) {
            return false;
        }
        if on {
            self.locks.insert(dimension.to_string());
        } else {
            self.locks.remove(dimension);
        }
        true
    }

    /// `Deck.grabWander`: the browser wheel turned.
    pub fn grab(&mut self) {
        if self.on {
            self.paused = true;
            self.quiet_bars = 0;
        }
    }

    /// The paused / quiet-bar half of `Deck.advanceWander`: the deck's clock is in bar `bar`
    /// (0-based). Returns the move bar when a move is due (the last bar of a phrase) and
    /// `look_ahead` when the bar before is reached; `None`/`false` otherwise. The caller takes
    /// the move (`Wander::plan`) and calls [`WanderState::took_move`].
    pub fn advance(&mut self, bar: i64, held: bool) -> Advance {
        if !self.on || self.last_bar == Some(bar) {
            return Advance::Nothing;
        }
        self.last_bar = Some(bar);
        // auto-mix holds the walk through a hand-off: no move, no look-ahead, and the held
        // bars are not quiet bars
        if held {
            return Advance::Held;
        }
        if self.paused {
            self.quiet_bars += 1;
            if self.quiet_bars >= self.pace {
                self.paused = false;
                self.quiet_bars = 0;
            }
            return Advance::Nothing;
        }
        if (bar + 2) % self.pace == 0 {
            return Advance::LookAhead(bar + 1);
        }
        if (bar + 1) % self.pace == 0 {
            return Advance::Move(bar);
        }
        Advance::Nothing
    }

    /// Record a move taken: the step counts on and the log keeps its limit.
    pub fn took_move(&mut self, m: WanderMove) {
        self.step += 1;
        self.log.push(m);
        if self.log.len() > Self::LOG_LIMIT {
            let extra = self.log.len() - Self::LOG_LIMIT;
            self.log.drain(0..extra);
        }
    }

    /// `Deck.wanderCountdown`: bars until the walk's next move (or, paused, until it resumes)
    /// given the deck's tick; None when it is off.
    pub fn countdown(&self, tick: i64) -> Option<i64> {
        if !self.on {
            return None;
        }
        if self.paused {
            return Some((self.pace - self.quiet_bars).max(1));
        }
        Some(bar_countdown(tick, self.pace))
    }

    /// A session's starting wander settings (`WanderState.json`).
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("on", JsonValue::Bool(self.on)),
            ("pace", JsonValue::int(self.pace)),
            ("step", JsonValue::int(self.step)),
            (
                "locks",
                JsonValue::Array(
                    self.locks
                        .iter()
                        .map(|l| JsonValue::str(l.as_str()))
                        .collect(),
                ),
            ),
        ]);
        if let Some(a) = self.anchor_bpm {
            o.set("anchorBPM", JsonValue::Number(a));
        }
        if let Some(k) = &self.key {
            o.set("key", k.json());
        }
        if self.paused {
            o.set("paused", JsonValue::Bool(true));
            o.set("quietBars", JsonValue::int(self.quiet_bars));
        }
        o.into()
    }

    /// `WanderState(json:)`.
    pub fn from_json(v: &JsonValue) -> WanderState {
        let mut s = WanderState::default();
        s.on = v.get("on").and_then(JsonValue::as_bool).unwrap_or(false);
        s.pace = v
            .get("pace")
            .and_then(JsonValue::as_i64)
            .filter(|p| Self::PACES.contains(p))
            .unwrap_or(Self::DEFAULT_PACE);
        s.step = v.get("step").and_then(JsonValue::as_i64).unwrap_or(0);
        s.locks = v
            .get("locks")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(JsonValue::as_str)
                    .filter(|l| WanderLock::is_valid(l))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        s.anchor_bpm = v.get("anchorBPM").and_then(JsonValue::as_f64);
        s.key = WanderKey::from_json(v.get("key"));
        s.paused = v
            .get("paused")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        s.quiet_bars = v.get("quietBars").and_then(JsonValue::as_i64).unwrap_or(0);
        s
    }
}

/// What the walk does with a bar reading (`Deck.advanceWander`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    Nothing,
    /// Auto-mix holds the walk: no move and no look-ahead this bar.
    Held,
    /// The bar before the move at this bar: plan it read-only and compose a swap ahead.
    LookAhead(i64),
    /// Take the move now (`bar` is the phrase's last bar, 0-based).
    Move(i64),
}

/// `Deck.wanderCountdown` for a running walk: bars until the move (a full `pace` on the move
/// bar itself).
pub fn bar_countdown(tick: i64, pace: i64) -> i64 {
    let into = (tick / 16) % pace;
    let n = pace - 1 - into;
    if n == 0 {
        pace
    } else {
        n
    }
}

/// Is bar `bar` (0-based) the last of a phrase of `pace` bars?
pub fn is_move_bar(bar: i64, pace: i64) -> bool {
    (bar + 1) % pace == 0
}

impl Wander {
    /// `TrackSpec.pinnedKeyOffset` for the key a lock holds.
    pub fn pinned(spec: &TrackSpec) -> Option<i64> {
        spec.pinned_key_offset()
    }

    /// `Deck.wanderKey`: the key the track plays in now, the genre's scale and the played
    /// project's root (`root_note`; a pass that moves keys moves it; None: the genre's own).
    /// None for a genre the tables do not know.
    pub fn played_key(genre: &str, root_note: Option<i64>) -> Option<WanderKey> {
        let g = crate::gen::tables::BeatsTables::shared()
            .genres
            .get(genre)?;
        let root = root_note.unwrap_or(g.root_note);
        let mut offset = ((root - g.root_note) % 12 + 12) % 12;
        if offset > 6 {
            offset -= 12;
        }
        Some(WanderKey {
            scale: g.scale_name.clone(),
            root_pc: ((g.root_note + offset) % 12 + 12) % 12,
            offset,
        })
    }
}
