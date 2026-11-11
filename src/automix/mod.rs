//! Auto-mix: Wander across two decks (port of `AutoMix.swift`; the Swift app's docs/AUTOMIX.md is the design).
//!
//! The rule ([`AutoMix::plan`]), the settings, plans, the hand-off log, the hand-off in flight
//! ([`AutoMixRun`]) and the state with its session / checkpoint codecs live here. The engine's
//! arithmetic (`AutoMixEngine.swift`: touches, the countdown's hand-off bar, the start's align
//! frames, the crossfader's steps, the correction) is in [`engine`], as functions of explicit
//! inputs: the `Deck` / `DJEngine` wiring that feeds them is the host's.

pub mod engine;
pub mod measure;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cid::share_cid;
use crate::gen::tables::BeatsTables;
use crate::json::{JsonObject, JsonValue};
use crate::spec::TrackSpec;
use crate::wander::{Wander, WanderContext, WanderKey, WanderMoveKind, WanderRandom};

/// The poke's value (`AutoMixSettings`): on or off, the hand-off length, how long a track plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoMixSettings {
    pub on: bool,
    pub bars: i64,
    pub every: i64,
}

impl AutoMixSettings {
    /// Hand-off lengths in bars.
    pub const LENGTHS: [i64; 4] = [4, 8, 16, 32];
    /// Phrases (8 bars) a track plays before the next hand-off; 0 = a song to its outro (a
    /// loop: [`AutoMix::LOOP_PHRASES`]).
    pub const EVERIES: [i64; 4] = [0, 4, 8, 16];
    pub const DEFAULT_BARS: i64 = 8;

    /// `init(on:bars:every:)`: values outside the offered lists read as the defaults.
    pub fn new(on: bool, bars: i64, every: i64) -> AutoMixSettings {
        AutoMixSettings {
            on,
            bars: if Self::LENGTHS.contains(&bars) {
                bars
            } else {
                Self::DEFAULT_BARS
            },
            every: if Self::EVERIES.contains(&every) {
                every
            } else {
                0
            },
        }
    }

    /// Nothing set but the defaults (off, 8 bars, to the outro): a session leaves it out.
    pub fn is_default(&self) -> bool {
        *self == AutoMixSettings::default()
    }

    /// "8 bars · to the outro", "16 bars · every 8 phrases".
    pub fn label(&self) -> String {
        alloc::format!(
            "{} bars · {}",
            self.bars,
            if self.every == 0 {
                "to the outro".to_string()
            } else {
                alloc::format!("every {} phrases", self.every)
            }
        )
    }
}

impl Default for AutoMixSettings {
    fn default() -> Self {
        AutoMixSettings {
            on: false,
            bars: Self::DEFAULT_BARS,
            every: 0,
        }
    }
}

/// Where auto-mix is (`AutoMixPhase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoMixPhase {
    Off,
    /// On, but it can't act (`AutoMixState.idle` says why).
    Idle,
    /// Counting down to the next hand-off.
    Playing,
    /// A steering poke paused it: it resumes after a phrase with no input.
    Paused,
    /// The next track is on (or on its way to) the idle deck, stopped.
    Preparing,
    /// The incoming deck is started, its first bar still to come.
    Armed,
    /// The hand-off is running.
    Mixing,
}

impl AutoMixPhase {
    pub fn raw(self) -> &'static str {
        match self {
            AutoMixPhase::Off => "off",
            AutoMixPhase::Idle => "idle",
            AutoMixPhase::Playing => "playing",
            AutoMixPhase::Paused => "paused",
            AutoMixPhase::Preparing => "preparing",
            AutoMixPhase::Armed => "armed",
            AutoMixPhase::Mixing => "mixing",
        }
    }
}

/// `DeckSourceDescriptor.net(spec).json`: `{kind: "net", share, cid[, partSeeds]}`.
pub fn net_descriptor_json(spec: &TrackSpec) -> JsonValue {
    let payload = spec.payload();
    let mut o = JsonObject::from_pairs([
        ("kind", JsonValue::str("net")),
        ("share", payload.clone()),
        ("cid", JsonValue::String(share_cid(&payload))),
    ]);
    if let Some(seeds) = spec.part_seeds_json() {
        o.set("partSeeds", seeds);
    }
    o.into()
}

/// `DeckSourceDescriptor(json:)` for a net.
pub fn net_descriptor_from_json(v: &JsonValue) -> Option<TrackSpec> {
    if v.get("kind")?.as_str()? != "net" {
        return None;
    }
    let spec = TrackSpec::from_payload(v.get("share")?)?;
    Some(spec.with_part_seeds(v.get("partSeeds")))
}

/// The configuration hand-off `n` brings in (`AutoMixPlan`).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMixPlan {
    pub n: i64,
    /// What the incoming deck will play; for a cued deck, what it holds (None: a shared net).
    pub spec: Option<TrackSpec>,
    /// seed | instrument | genre (Wander's move kinds), or cued (loaded by hand).
    pub kind: String,
    /// "seed 41 → 44 · key A", "techno → house · seed 512", "cued: house #12".
    pub detail: String,
    /// The outgoing configuration's share CID it was drawn from (None for a cue).
    pub from: Option<String>,
}

impl AutoMixPlan {
    pub fn cued(&self) -> bool {
        self.kind == "cued"
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("n", JsonValue::int(self.n)),
            ("kind", JsonValue::str(self.kind.as_str())),
            ("move", JsonValue::str(self.detail.as_str())),
        ]);
        if let Some(spec) = &self.spec {
            o.set("genre", JsonValue::str(spec.genre.as_str()));
            o.set("seed", JsonValue::Number(spec.seed));
            o.set("cid", JsonValue::String(share_cid(&spec.payload())));
            o.set("spec", net_descriptor_json(spec));
        }
        if let Some(from) = &self.from {
            o.set("from", JsonValue::str(from.as_str()));
        }
        o.into()
    }

    pub fn from_json(v: Option<&JsonValue>) -> Option<AutoMixPlan> {
        let v = v?;
        Some(AutoMixPlan {
            n: v.get("n")?.as_i64()?,
            kind: v.get("kind")?.as_str()?.to_string(),
            spec: v.get("spec").and_then(net_descriptor_from_json),
            detail: v
                .get("move")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            from: v.get("from").and_then(JsonValue::as_str).map(String::from),
        })
    }
}

/// One finished hand-off: the log row (newest first in `dj_state`) (`AutoMixHandoff`).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMixHandoff {
    pub n: i64,
    pub from: String,
    pub to: String,
    /// The bar of the outgoing deck it started on (0-based, its clock).
    pub bar: i64,
    pub kind: String,
    pub move_: String,
    pub spec: Option<TrackSpec>,
}

impl AutoMixHandoff {
    pub fn cid(&self) -> Option<String> {
        self.spec.as_ref().map(|s| share_cid(&s.payload()))
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("n", JsonValue::int(self.n)),
            ("from", JsonValue::str(self.from.as_str())),
            ("to", JsonValue::str(self.to.as_str())),
            ("bar", JsonValue::int(self.bar)),
            ("kind", JsonValue::str(self.kind.as_str())),
            ("move", JsonValue::str(self.move_.as_str())),
        ]);
        if let Some(spec) = &self.spec {
            o.set("genre", JsonValue::str(spec.genre.as_str()));
            o.set("seed", JsonValue::Number(spec.seed));
        }
        if let Some(cid) = self.cid() {
            o.set("cid", JsonValue::String(cid));
        }
        o.into()
    }
}

/// A hand-off in flight: everything from its prep point to its end (`AutoMixRun`).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMixRun {
    pub plan: AutoMixPlan,
    pub out: String,
    pub inc: String,
    /// The hand-off bar on the outgoing deck's clock, and how many bars it was prepared ahead.
    pub handoff_bar: i64,
    pub prep: i64,
    pub bars: i64,
    /// The outgoing deck plays a net (else a sample with a beat grid: no mix-out).
    pub out_is_net: bool,
    pub incoming_ready: bool,
    /// The outgoing project's generation when the mix-out was prepared ahead.
    pub out_generation: Option<i32>,
    /// Where the mix-out project starts; set when it is loaded.
    pub mix_out_tick: Option<i64>,
    /// The outgoing deck's song tick on the hand-off bar line.
    pub out_handoff_tick: Option<i64>,
    pub mix_out_loaded: bool,
    pub armed: bool,
    pub corrected: bool,
    /// The crossfader at the hand-off bar, where it goes, and whether the user took it.
    pub c0: Option<f64>,
    pub x_end: f64,
    pub fader_taken: bool,
    pub last_fader: Option<f64>,
    /// The incoming deck's leader before the hand-off (restored at its end).
    pub prev_leader: Option<String>,
    /// The incoming deck's tempo multiplier against the outgoing (1, or a cued deck's ½ / 2).
    pub mult: f64,
}

impl AutoMixRun {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: AutoMixPlan,
        out: &str,
        inc: &str,
        handoff_bar: i64,
        prep: i64,
        bars: i64,
        out_is_net: bool,
        x_end: f64,
    ) -> AutoMixRun {
        AutoMixRun {
            plan,
            out: out.into(),
            inc: inc.into(),
            handoff_bar,
            prep,
            bars,
            out_is_net,
            incoming_ready: false,
            out_generation: None,
            mix_out_tick: None,
            out_handoff_tick: None,
            mix_out_loaded: false,
            armed: false,
            corrected: false,
            c0: None,
            x_end,
            fader_taken: false,
            last_fader: None,
            prev_leader: None,
            mult: 1.0,
        }
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("plan", self.plan.json()),
            ("out", JsonValue::str(self.out.as_str())),
            ("in", JsonValue::str(self.inc.as_str())),
            ("handoffBar", JsonValue::int(self.handoff_bar)),
            ("prep", JsonValue::int(self.prep)),
            ("bars", JsonValue::int(self.bars)),
            ("outIsNet", JsonValue::Bool(self.out_is_net)),
            ("ready", JsonValue::Bool(self.incoming_ready)),
            ("mixOutLoaded", JsonValue::Bool(self.mix_out_loaded)),
            ("armed", JsonValue::Bool(self.armed)),
            ("corrected", JsonValue::Bool(self.corrected)),
            ("xEnd", JsonValue::Number(self.x_end)),
            ("faderTaken", JsonValue::Bool(self.fader_taken)),
            ("mult", JsonValue::Number(self.mult)),
        ]);
        if let Some(t) = self.mix_out_tick {
            o.set("mixOutTick", JsonValue::int(t));
        }
        if let Some(t) = self.out_handoff_tick {
            o.set("outHandoffTick", JsonValue::int(t));
        }
        if let Some(c) = self.c0 {
            o.set("c0", JsonValue::Number(c));
        }
        if let Some(l) = self.last_fader {
            o.set("lastFader", JsonValue::Number(l));
        }
        if let Some(l) = &self.prev_leader {
            o.set("prevLeader", JsonValue::str(l.as_str()));
        }
        o.into()
    }

    pub fn from_json(v: Option<&JsonValue>) -> Option<AutoMixRun> {
        let v = v?;
        let plan = AutoMixPlan::from_json(v.get("plan"))?;
        let mut r = AutoMixRun::new(
            plan,
            v.get("out")?.as_str()?,
            v.get("in")?.as_str()?,
            v.get("handoffBar")?.as_i64()?,
            v.get("prep")
                .and_then(JsonValue::as_i64)
                .unwrap_or(AutoMix::PREP_BARS),
            v.get("bars")
                .and_then(JsonValue::as_i64)
                .unwrap_or(AutoMixSettings::DEFAULT_BARS),
            v.get("outIsNet")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
            v.get("xEnd").and_then(JsonValue::as_f64).unwrap_or(1.0),
        );
        let b = |k: &str| v.get(k).and_then(JsonValue::as_bool).unwrap_or(false);
        r.incoming_ready = b("ready");
        r.mix_out_tick = v.get("mixOutTick").and_then(JsonValue::as_i64);
        r.out_handoff_tick = v.get("outHandoffTick").and_then(JsonValue::as_i64);
        r.mix_out_loaded = b("mixOutLoaded");
        r.armed = b("armed");
        r.corrected = b("corrected");
        r.c0 = v.get("c0").and_then(JsonValue::as_f64);
        r.fader_taken = b("faderTaken");
        r.last_fader = v.get("lastFader").and_then(JsonValue::as_f64);
        r.prev_leader = v
            .get("prevLeader")
            .and_then(JsonValue::as_str)
            .map(String::from);
        r.mult = v.get("mult").and_then(JsonValue::as_f64).unwrap_or(1.0);
        Some(r)
    }
}

/// Auto-mix's state on the engine (`AutoMixState`): the settings (the poke's value), the
/// hand-off counter, and where the walk is.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMixState {
    pub settings: AutoMixSettings,
    /// Hand-offs drawn so far: hand-off n draws from (the outgoing spec's CID, n).
    pub handoffs: i64,
    pub phase: AutoMixPhase,
    /// The deck playing (the next hand-off's outgoing deck), once one is.
    pub playing: Option<String>,
    /// Why it can't act, when it can't.
    pub idle: Option<String>,
    /// A steering poke paused it; `quiet_bars` of the playing deck pass with no input to resume.
    pub paused: bool,
    pub quiet_bars: i64,
    /// The playing deck's bar when last seen (None: not yet).
    pub last_bar: Option<i64>,
    /// The bar (playing deck's clock) `every` counts from.
    pub start_bar: Option<i64>,
    /// The next hand-off bar, while counting down.
    pub handoff_bar: Option<i64>,
    /// Mix now: the next phrase boundary at least `AutoMix.NOW_PREP_BARS` away.
    pub now_requested: bool,
    /// The idle deck the user loaded by hand since the last hand-off: the next incoming, as-is.
    pub cued: Option<String>,
    /// A start cancelled by a steering poke: its plan, kept.
    pub kept: Option<AutoMixPlan>,
    pub run: Option<AutoMixRun>,
    pub log: Vec<AutoMixHandoff>,
}

impl Default for AutoMixState {
    fn default() -> Self {
        AutoMixState {
            settings: AutoMixSettings::default(),
            handoffs: 0,
            phase: AutoMixPhase::Off,
            playing: None,
            idle: None,
            paused: false,
            quiet_bars: 0,
            last_bar: None,
            start_bar: None,
            handoff_bar: None,
            now_requested: false,
            cued: None,
            kept: None,
            run: None,
            log: Vec::new(),
        }
    }
}

impl AutoMixState {
    /// Rows the log keeps (newest last).
    pub const LOG_LIMIT: usize = 48;

    /// The next hand-off's plan, from its prep point (or a kept one).
    pub fn next(&self) -> Option<&AutoMixPlan> {
        self.run.as_ref().map(|r| &r.plan).or(self.kept.as_ref())
    }

    /// The deck coming in during a hand-off.
    pub fn incoming(&self) -> Option<&str> {
        self.run.as_ref().map(|r| r.inc.as_str())
    }

    /// Who moves the crossfader in this hand-off: "auto", or "user" once taken.
    pub fn fader(&self) -> &'static str {
        if self.run.as_ref().map(|r| r.fader_taken) == Some(true) {
            "user"
        } else {
            "auto"
        }
    }

    /// The session's starting value (`Session.mixer.autoMix`): settings and counter.
    pub fn session_json(&self) -> JsonValue {
        JsonObject::from_pairs([
            ("on", JsonValue::Bool(self.settings.on)),
            ("bars", JsonValue::int(self.settings.bars)),
            ("every", JsonValue::int(self.settings.every)),
            ("handoffs", JsonValue::int(self.handoffs)),
        ])
        .into()
    }

    /// A session leaves it out when it is the default (older sessions keep their text and CID).
    pub fn is_session_default(&self) -> bool {
        self.settings.is_default() && self.handoffs == 0
    }

    /// A journal checkpoint's value: the session's, plus a hand-off in flight and the
    /// countdown.
    pub fn checkpoint_json(&self) -> JsonValue {
        let mut o = match self.session_json() {
            JsonValue::Object(o) => o,
            _ => JsonObject::new(),
        };
        if let Some(p) = &self.playing {
            o.set("playing", JsonValue::str(p.as_str()));
        }
        if self.paused {
            o.set("paused", JsonValue::Bool(true));
            o.set("quietBars", JsonValue::int(self.quiet_bars));
        }
        if let Some(b) = self.start_bar {
            o.set("startBar", JsonValue::int(b));
        }
        if self.now_requested {
            o.set("now", JsonValue::Bool(true));
        }
        if let Some(c) = &self.cued {
            o.set("cued", JsonValue::str(c.as_str()));
        }
        if let Some(k) = &self.kept {
            o.set("kept", k.json());
        }
        if let Some(r) = &self.run {
            o.set("run", r.json());
        }
        o.into()
    }

    /// A session's starting value: settings and counter only.
    pub fn with_start(settings: AutoMixSettings, handoffs: i64) -> AutoMixState {
        AutoMixState {
            settings,
            handoffs: handoffs.max(0),
            ..Default::default()
        }
    }

    /// A session's `mixer.autoMix` read back (missing: the default).
    pub fn from_start_json(v: Option<&JsonValue>) -> AutoMixState {
        let full = AutoMixState::from_json(v);
        AutoMixState::with_start(full.settings, full.handoffs)
    }

    /// The part a session starts from (`Session.autoMix`).
    pub fn start(&self) -> AutoMixState {
        AutoMixState::with_start(self.settings, self.handoffs)
    }

    /// Read either form back (unknown or missing fields: the defaults).
    pub fn from_json(v: Option<&JsonValue>) -> AutoMixState {
        let mut s = AutoMixState::default();
        let Some(v) = v else { return s };
        s.settings = AutoMixSettings::new(
            v.get("on").and_then(JsonValue::as_bool).unwrap_or(false),
            v.get("bars")
                .and_then(JsonValue::as_i64)
                .unwrap_or(AutoMixSettings::DEFAULT_BARS),
            v.get("every").and_then(JsonValue::as_i64).unwrap_or(0),
        );
        s.handoffs = v
            .get("handoffs")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
            .max(0);
        s.playing = v
            .get("playing")
            .and_then(JsonValue::as_str)
            .map(String::from);
        s.paused = v
            .get("paused")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        s.quiet_bars = v.get("quietBars").and_then(JsonValue::as_i64).unwrap_or(0);
        s.start_bar = v.get("startBar").and_then(JsonValue::as_i64);
        s.now_requested = v.get("now").and_then(JsonValue::as_bool).unwrap_or(false);
        s.cued = v.get("cued").and_then(JsonValue::as_str).map(String::from);
        s.kept = AutoMixPlan::from_json(v.get("kept"));
        s.run = AutoMixRun::from_json(v.get("run"));
        s
    }
}

/// `AutoMix` (the namespace of the rule and its constants).
pub struct AutoMix;

impl AutoMix {
    /// A phrase: hand-offs start on a multiple of 8 bars of the playing deck (or a section
    /// start).
    pub const PHRASE_BARS: i64 = 8;
    /// How far ahead of the hand-off the next track is drawn and composed.
    pub const PREP_BARS: i64 = 8;
    /// Mix now: the next phrase boundary at least this many bars away.
    pub const NOW_PREP_BARS: i64 = 4;
    /// Quiet bars (a phrase) after a steering poke before it resumes.
    pub const QUIET_BARS: i64 = 8;
    /// A loop (no outro) plays this many phrases when `every` is 0 (64 bars).
    pub const LOOP_PHRASES: i64 = 8;
    /// Keeps hand-off n from drawing what Wander's step n draws on the same configuration.
    pub const SALT: u64 = 0xA17C_0DE5_1A7E_F00D;
    /// The move kinds that bring a new track.
    pub const KINDS: [WanderMoveKind; 3] = [
        WanderMoveKind::Seed,
        WanderMoveKind::Instrument,
        WanderMoveKind::Genre,
    ];
    /// The largest dip (LU) of the short-term loudness during a hand-off below the quieter of
    /// the two tracks alone, from `djctl automix` (see [`AutoMix::MAX_DIP_SOURCE`]).
    pub const MAX_DIP_LU: f64 = 24.0;
    pub const MAX_DIP_SOURCE: &'static str = "djctl automix matrix 2026-10-19 (techno house trance dnb lofi jazz × seeds 42, 7; bars 8, every 4; 48 kHz synth voices), n=12, worst lofi 7 −23.64 LU (lofi's drums and bass alone are near silent: −39 LUFS short-term); the worst outside lofi trance 42 −9.40";
    pub const PITCH_NAMES: [&'static str; 12] = [
        "C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B",
    ];

    /// Hand-off `n`'s incoming configuration over the outgoing deck's (`c`, with both decks'
    /// locks and `c.mix_bpm`): Wander's seed / instrument / genre moves drawn from (the spec's
    /// CID ^ `SALT`, n), then the key pinned compatible ([`pin_key`](Self::pin_key)) from the
    /// same stream. A pure function; None when every new-track move is locked.
    pub fn plan(c: &WanderContext, n: i64, key: Option<&WanderKey>) -> Option<AutoMixPlan> {
        let mut rng = WanderRandom::with_base(WanderRandom::base(&c.spec) ^ Self::SALT, n);
        let allowed: Vec<WanderMoveKind> = Wander::kinds(c)
            .into_iter()
            .filter(|k| Self::KINDS.contains(k))
            .collect();
        let p = Wander::plan_with(c, &mut rng, allowed)?;
        let crate::wander::WanderAction::Swap(mut spec) = p.action else {
            return None;
        };
        let mut detail = p.detail;
        if !c.locked("key") {
            if let Some(k) = Self::pin_key(&mut spec, key, &mut rng) {
                detail.push_str(" · ");
                detail.push_str(&k);
            }
        }
        Some(AutoMixPlan {
            n,
            spec: Some(spec),
            kind: p.kind.raw().to_string(),
            detail,
            from: Some(share_cid(&c.spec.payload())),
        })
    }

    /// Can a hand-off be drawn here at all (some new-track move left)?
    pub fn can_draw(c: &WanderContext) -> bool {
        Wander::kinds(c).into_iter().any(|k| {
            Self::KINDS.contains(&k)
                && (k != WanderMoveKind::Genre || !Wander::genre_options(c).is_empty())
        })
    }

    /// The incoming key, compatible with the outgoing `key`: when the incoming genre plays the
    /// same scale, its root is pinned (`extra["keyOffset"]`, as the key lock pins it) to the
    /// outgoing root or a fifth either way, drawn 2 : 1 : 1. Another scale keeps `.variety`'s
    /// own key (an offset carried from the outgoing spec is dropped). Returns the key's label
    /// when it pinned one.
    pub fn pin_key(
        spec: &mut TrackSpec,
        key: Option<&WanderKey>,
        rng: &mut WanderRandom,
    ) -> Option<String> {
        let g = BeatsTables::shared().genres.get(&spec.genre);
        let (Some(key), Some(g)) = (key, g) else {
            spec.extra.remove("keyOffset");
            return None;
        };
        if g.scale_name != key.scale {
            spec.extra.remove("keyOffset");
            return None;
        }
        let shift = [0, 0, 7, 5][rng.int(4) as usize]; // same root, same root, a fifth up, a fifth down
        let pc = (key.root_pc + shift) % 12;
        let mut offset = ((pc - g.root_note % 12) % 12 + 12) % 12;
        if offset > 6 {
            offset -= 12;
        }
        spec.extra
            .set("keyOffset", JsonValue::Number(offset as f64));
        Some(alloc::format!("key {}", Self::PITCH_NAMES[pc as usize]))
    }
}
