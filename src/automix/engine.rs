//! The arithmetic of `AutoMixEngine.swift`, as functions of explicit inputs.
//!
//! The engine's `serviceAutoMix` runs on the main thread at the controller hub's housekeeping
//! ticks, reading decks (`wave.tick`, `effectiveBPM`, `renderedFrames`) and calling `load`,
//! `setPlaying` and `align` on them. None of that is here. What is: which poke touches
//! auto-mix how, the state's transitions for each, the countdown's bookkeeping, the hand-off
//! bar, the start's align frames, the crossfader's steps and the start's correction. A host
//! reads its decks, calls these, and applies what they return.

use alloc::string::String;
use alloc::vec::Vec;

use super::{
    AutoMix, AutoMixHandoff, AutoMixPhase, AutoMixPlan, AutoMixRun, AutoMixSettings, AutoMixState,
};
use crate::json::{JsonObject, JsonValue};
use crate::spec::TrackSpec;
use crate::wander::{Wander, WanderContext, WanderKey, WanderState};

/// What a poke does to auto-mix (`AutoMixTouch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoMixTouch {
    /// Playing the mix (knobs, mutes, macros, FX, keys): never pauses it.
    Expression,
    /// Steering a deck (cue, loops, sync, sections, sound changes, the jog): pauses it.
    Steer,
    /// The transport (play / stop): pauses it; during a hand-off, ends it.
    Transport,
    /// What a deck holds changed (a load, Generate, next track): on the idle deck between
    /// hand-offs it is a cue; otherwise as `Transport`.
    Load,
}

/// What `DeckAction.autoMixTouch` names a poke by: the Swift cases it maps, by their op name
/// (`DeckAction`'s JSON `op`). None for actions about neither deck (the loop row, the mixer,
/// auto-mix's own) and for the engine's own records (`grid`, `autoMixDefer`).
pub fn touch_of_op(op: &str) -> Option<AutoMixTouch> {
    match op {
        "play" => Some(AutoMixTouch::Transport),
        "load" | "clip" | "nextTrack" => Some(AutoMixTouch::Load),
        "cue" | "hotCue" | "loop" | "section" | "sync" | "follow" | "rewind" | "setKit"
        | "setPalette" | "setRoleVoice" | "mixOut" | "mixIn" => Some(AutoMixTouch::Steer),
        _ => None,
    }
}

/// What a touch asks the host to do to the run in flight, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum RunOp {
    /// `cancelRun(keepPlan:)`: stop the armed incoming deck if it plays, keep the plan.
    CancelRun { keep_plan: bool },
    /// `endRun(log: false)`.
    EndRun,
}

/// A deck of the phrase arithmetic: where its sections start (`Deck.sections`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoMixSection {
    pub name: String,
    pub start_tick: i64,
    pub steps: i64,
}

/// Which deck is "playing" (`serviceCountdown`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayingDeck {
    /// Neither plays: "start a deck".
    Nobody,
    /// Both play: the louder by crossfader and fader is the playing one (the winner's name);
    /// wait for the other to stop.
    Both(String),
    /// Exactly one plays.
    One(String),
}

/// `serviceCountdown`'s choice of the playing deck from the transports and the strips' gains
/// (`crossfadeGain × volume`): equal gains keep the previous one (default "A").
pub fn choose_playing(
    a_playing: bool,
    b_playing: bool,
    gain_a: f64,
    gain_b: f64,
    previous: Option<&str>,
) -> PlayingDeck {
    match (a_playing, b_playing) {
        (false, false) => PlayingDeck::Nobody,
        (true, false) => PlayingDeck::One("A".into()),
        (false, true) => PlayingDeck::One("B".into()),
        (true, true) => {
            let winner = if gain_a == gain_b {
                match previous {
                    Some("B") => "B",
                    _ => "A",
                }
            } else if gain_a > gain_b {
                "A"
            } else {
                "B"
            };
            PlayingDeck::Both(winner.into())
        }
    }
}

/// A phrase boundary: a multiple of 8 bars, or a section's first bar (`isAutoMixPhrase`).
pub fn is_phrase(sections: &[AutoMixSection], bar: i64) -> bool {
    bar % AutoMix::PHRASE_BARS == 0 || sections.iter().any(|s| s.start_tick == bar * 16)
}

/// The first phrase boundary at or after `bar` (`autoMixPhrase`).
pub fn phrase_from(sections: &[AutoMixSection], bar: i64) -> i64 {
    let mut b = bar.max(0);
    while !is_phrase(sections, b) {
        b += 1;
    }
    b
}

/// The next hand-off bar on the outgoing deck's clock, at least `prep` + 1 bars after `bar`
/// (`autoMixHandoffBar`): a song's outro (`every` 0; without one, the last phrase boundary that
/// leaves the hand-off's bars before the end), else the first phrase boundary `every` phrases
/// after the deck came in (a loop: [`AutoMix::LOOP_PHRASES`]). Mix now, and a cue under a
/// sample deck: the first phrase boundary from there.
pub fn handoff_bar(
    settings: &AutoMixSettings,
    sections: &[AutoMixSection],
    start_bar: Option<i64>,
    now_requested: bool,
    bar: i64,
    prep: i64,
    cued_sample: bool,
) -> i64 {
    let earliest = bar + prep + 1;
    if now_requested || cued_sample {
        return phrase_from(sections, earliest);
    }
    if settings.every == 0 && !sections.is_empty() {
        if let Some(outro) = sections.iter().rev().find(|s| s.name == "outro") {
            if outro.start_tick / 16 >= earliest {
                return outro.start_tick / 16;
            }
        }
        let end = sections.last().map(|s| s.start_tick + s.steps).unwrap_or(0) / 16;
        let latest = end - settings.bars;
        if latest >= earliest {
            let mut b = latest;
            while b >= earliest && !is_phrase(sections, b) {
                b -= 1;
            }
            if b >= earliest {
                return b;
            }
        }
        return phrase_from(sections, earliest);
    }
    let every = if settings.every == 0 {
        AutoMix::LOOP_PHRASES
    } else {
        settings.every
    };
    phrase_from(
        sections,
        earliest.max(start_bar.unwrap_or(bar) + every * AutoMix::PHRASE_BARS),
    )
}

/// The prep point is reached (`bar >= h - prep`).
pub fn prep_reached(bar: i64, h: i64, prep: i64) -> bool {
    bar >= h - prep
}

/// What `serviceCountdown`'s bar bookkeeping decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountdownBar {
    /// Paused and not yet quiet for a phrase: nothing to count down.
    Quiet,
    /// Counting down (the countdown's hand-off bar is worked out from here).
    Counting,
}

impl AutoMixState {
    // MARK: the pokes

    /// `setAutoMix(on:bars:every:)`, the state half. Off during a hand-off lets go of it where
    /// it is (the returned run: the host releases its decks). Settings changed while on apply
    /// from the next hand-off.
    pub fn set_auto_mix(&mut self, on: bool, bars: i64, every: i64) -> Option<AutoMixRun> {
        let was = self.settings;
        self.settings = AutoMixSettings::new(on, bars, every);
        if !on {
            let ended = self.end_run();
            self.kept = None;
            self.cued = None;
            self.paused = false;
            self.quiet_bars = 0;
            self.playing = None;
            self.last_bar = None;
            self.start_bar = None;
            self.handoff_bar = None;
            self.now_requested = false;
            self.idle = None;
            self.phase = AutoMixPhase::Off;
            return ended;
        }
        if !was.on {
            self.playing = None;
            self.last_bar = None;
            self.start_bar = None;
            self.cued = None;
            self.kept = None;
        }
        self.handoff_bar = None; // worked out again with the new settings
        None
    }

    /// `autoMixNow()`: hand off at the next phrase boundary at least `NOW_PREP_BARS` away.
    /// Ignored while a hand-off runs or auto-mix is off; resumes a paused countdown.
    pub fn auto_mix_now(&mut self) {
        if !self.settings.on || self.run.is_some() {
            return;
        }
        self.now_requested = true;
        self.paused = false;
        self.quiet_bars = 0;
        self.handoff_bar = None;
    }

    /// `deferAutoMix(handoff:)`: hand-off `n`'s incoming track was not composed by its mix-out
    /// point; it moves one phrase later, its plan kept. Returns the run to re-request for.
    pub fn defer_handoff(&mut self, n: i64) -> Option<AutoMixRun> {
        let run = self.run.as_mut()?;
        if run.plan.n != n || run.mix_out_loaded {
            return None;
        }
        run.handoff_bar += AutoMix::PHRASE_BARS;
        Some(run.clone())
    }

    /// `noteAutoMixTouch(_:_:)`: a poke reached `deck` (None: deck-less). `deck_exists`: the
    /// engine has that deck. Returns what to do to the run in flight, in order.
    pub fn note_touch(
        &mut self,
        deck: Option<&str>,
        kind: AutoMixTouch,
        deck_exists: bool,
    ) -> Vec<RunOp> {
        let mut ops = Vec::new();
        if !self.settings.on || kind == AutoMixTouch::Expression {
            return ops;
        }
        // only decks A and B; with nothing playing yet there is nothing to pause (the first
        // play starts the set)
        if let Some(d) = deck {
            if d != "A" && d != "B" {
                return ops;
            }
        }
        if let Some(run) = self.run.clone() {
            if !run.mix_out_loaded {
                // before the point of no return (the mix-out, a bar before the hand-off): a
                // load on the incoming deck replaces what was prepared (a cue), anything else
                // cancels the start and keeps the plan for later
                let cue = kind == AutoMixTouch::Load && deck == Some(run.inc.as_str());
                self.cancel_run(!cue);
                ops.push(RunOp::CancelRun { keep_plan: !cue });
                if cue {
                    self.cued = Some(run.inc.clone());
                }
                self.pause();
            } else if kind == AutoMixTouch::Transport || kind == AutoMixTouch::Load {
                // from the mix-out on: the transport or a load on either deck ends it,
                // nothing undone
                self.end_run();
                ops.push(RunOp::EndRun);
                self.pause();
            }
            return ops;
        }
        if kind == AutoMixTouch::Load {
            if let Some(d) = deck {
                if Some(d) != self.playing.as_deref() && deck_exists {
                    self.cued = Some(d.into());
                    return ops;
                }
            }
        }
        if self.playing.is_none() {
            return ops;
        }
        self.pause();
        ops
    }

    fn pause(&mut self) {
        self.paused = true;
        self.quiet_bars = 0;
        self.handoff_bar = None;
        self.now_requested = false;
    }

    /// `endRun(log:)`, the state half: the run in flight is dropped (the host releases the
    /// decks' holds and the incoming deck's leader).
    pub fn end_run(&mut self) -> Option<AutoMixRun> {
        self.run.take()
    }

    /// `cancelRun(keepPlan:)`, the state half: a steering poke before the mix-out point calls
    /// the start off. The plan is kept for the next prep point unless `keep_plan` is false.
    pub fn cancel_run(&mut self, keep_plan: bool) -> Option<AutoMixRun> {
        let run = self.run.clone()?;
        self.kept = if keep_plan && !run.plan.cued() && run.incoming_ready {
            Some(run.plan.clone())
        } else {
            None
        };
        self.end_run()
    }

    // MARK: the countdown

    /// `serviceCountdown`'s bookkeeping once exactly one deck plays and has a bar reading:
    /// a changed playing deck resets the countdown, the bar clock advances, a pause counts
    /// quiet bars, and after [`AutoMix::QUIET_BARS`] of them the countdown resumes.
    pub fn observe_countdown(&mut self, playing: &str, bar: i64) -> CountdownBar {
        if self.playing.as_deref() != Some(playing) {
            self.playing = Some(playing.into());
            self.last_bar = None;
            self.start_bar = None;
            self.handoff_bar = None;
        }
        let new_bar = self.last_bar.is_some() && Some(bar) != self.last_bar;
        self.last_bar = Some(bar);
        if self.start_bar.is_none() {
            self.start_bar = Some(bar);
        }
        if self.paused {
            if new_bar {
                self.quiet_bars += 1;
            }
            if self.quiet_bars < AutoMix::QUIET_BARS {
                self.idle = None;
                self.handoff_bar = None;
                return CountdownBar::Quiet;
            }
            self.paused = false;
            self.quiet_bars = 0;
            self.start_bar = Some(bar);
        }
        CountdownBar::Counting
    }

    /// Nothing plays: `serviceCountdown` forgets the clock and says why it is idle.
    pub fn nobody_playing(&mut self) {
        self.playing = None;
        self.last_bar = None;
        self.start_bar = None;
        self.set_idle("start a deck");
    }

    /// `setAutoMixIdle`.
    pub fn set_idle(&mut self, why: &str) {
        self.idle = Some(why.into());
        self.handoff_bar = None;
    }

    /// The prep distance: Mix now uses the shorter one.
    pub fn prep(&self) -> i64 {
        if self.now_requested {
            AutoMix::NOW_PREP_BARS
        } else {
            AutoMix::PREP_BARS
        }
    }

    /// `beginRun`'s bookkeeping once a plan is chosen: the counter moves unless the plan is a
    /// kept one reused, and the cue, kept plan and Mix-now request are spent.
    pub fn begin(&mut self, reuse: bool) {
        if !reuse {
            self.handoffs += 1;
        }
        self.kept = None;
        self.cued = None;
        self.now_requested = false;
    }

    /// `finishRun`'s bookkeeping: the hand-off logged, the incoming deck is the playing one and
    /// came fully in on its bar `bars`.
    pub fn finish(
        &mut self,
        run: &AutoMixRun,
        spec_if_cued: Option<crate::spec::TrackSpec>,
        incoming_playing: bool,
    ) {
        let entry = AutoMixHandoff {
            n: run.plan.n,
            from: run.out.clone(),
            to: run.inc.clone(),
            bar: run.handoff_bar,
            kind: run.plan.kind.clone(),
            move_: run.plan.detail.clone(),
            spec: run.plan.spec.clone().or(spec_if_cued),
        };
        self.end_run();
        self.log.push(entry);
        if self.log.len() > Self::LOG_LIMIT {
            let extra = self.log.len() - Self::LOG_LIMIT;
            self.log.drain(0..extra);
        }
        self.playing = if incoming_playing {
            Some(run.inc.clone())
        } else {
            None
        };
        self.start_bar = Some(run.bars);
        self.last_bar = None;
        self.handoff_bar = None;
    }

    /// `updateAutoMixPhase`: `inc_started` = the incoming deck's transport has ticked.
    pub fn update_phase(&mut self, inc_started: bool) {
        self.phase = if !self.settings.on {
            AutoMixPhase::Off
        } else if let Some(run) = &self.run {
            let started = inc_started && run.armed;
            if !run.armed {
                AutoMixPhase::Preparing
            } else if started {
                AutoMixPhase::Mixing
            } else {
                AutoMixPhase::Armed
            }
        } else if self.idle.is_some() {
            AutoMixPhase::Idle
        } else if self.paused {
            AutoMixPhase::Paused
        } else {
            AutoMixPhase::Playing
        };
    }

    /// `restoreAutoMix`: stand where `state` says (a session's start, a checkpoint, a rewind),
    /// keeping this state's log. The phase is `preparing` with a run in `state`, else
    /// `playing` when on.
    pub fn restored(&self, state: &AutoMixState) -> AutoMixState {
        let mut s = state.clone();
        s.log = self.log.clone();
        s.phase = if s.settings.on {
            if s.run.is_some() {
                AutoMixPhase::Preparing
            } else {
                AutoMixPhase::Playing
            }
        } else {
            AutoMixPhase::Off
        };
        s
    }
}

// MARK: the start

/// The outgoing net deck's clock as the start reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetClock {
    pub tick: i64,
    /// How far into the tick (0…1).
    pub tick_phase: f64,
    pub tempo: f64,
    /// The generation playing is the latest loaded (`activeGeneration == latest.generation`).
    pub installed: bool,
}

/// When the start is due (from beat 3.5 of bar H − 1) for a net outgoing deck: the output
/// frames to hold the incoming deck's first tick back so it lands on the outgoing deck's
/// hand-off bar line (`armFrames`, the net branch, after the rendered-frames agreement checks).
/// `Some(0.0)` = the line was missed, start now (the correction puts it in place); None while
/// not due.
pub fn arm_frames_net(sample_rate: f64, run: &AutoMixRun, out: NetClock) -> Option<f64> {
    let h = run.handoff_bar;
    let t = out.tick;
    // the step that plays on the hand-off bar line: the mix-out's first (an outro jumped to)
    let target = run.out_handoff_tick.unwrap_or(h * 16);
    let late = if out.installed {
        t >= target
    } else {
        t >= h * 16
    };
    if !(late || t >= h * 16 - 2) {
        return None;
    }
    if late {
        return Some(0.0); // missed: start now, the correction puts it in place
    }
    if out.tempo <= 0.0 {
        return None;
    }
    let spt = sample_rate * 60.0 / (out.tempo * 4.0);
    let p = out.tick_phase * spt;
    // doTicks to the one that plays step 16H: a swap still pending takes a tick of its own
    let pending = !out.installed;
    let mut j = target - t;
    if pending {
        if let Some(c) = run.mix_out_tick {
            j = (h * 16 - 1 - t) + 1 + (target - c);
        }
    }
    let frames = libm::ceil((j + 1) as f64 * spt - p - 1.0);
    (frames >= 1.0).then_some(frames)
}

/// The same for a sample outgoing deck with a beat grid: `beat` is its position in beats,
/// `bpm` its running tempo (`armFrames`, the sample branch).
pub fn arm_frames_sample(sample_rate: f64, handoff_bar: i64, beat: f64, bpm: f64) -> Option<f64> {
    if bpm <= 0.0 {
        return None;
    }
    let beats_left = (handoff_bar * 4) as f64 - beat;
    if beats_left > 1.5 {
        return None;
    }
    if beats_left <= 0.0 {
        return Some(0.0);
    }
    let frames = libm::round(beats_left * sample_rate * 60.0 / bpm);
    (frames >= 1.0).then_some(frames)
}

/// `startIncoming`'s align: the incoming deck's first tick held back `frames` samples from the
/// next block, in ticks at its own tempo (`wave.align(ticks:)`); None for `frames < 1`.
pub fn incoming_align_ticks(frames: f64, sample_rate: f64, incoming_bpm: f64) -> Option<f64> {
    if frames < 1.0 {
        return None;
    }
    let spt = sample_rate * 60.0 / (incoming_bpm * 4.0);
    Some(-(frames - 0.5) / spt)
}

/// What `correctStart` decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    /// Nothing to correct (a sample outgoing deck, or a tempo multiplier other than 1): the
    /// run is marked corrected.
    Skip,
    /// Not readable yet (the decks' blocks disagree, the incoming has not ticked): try again.
    Wait,
    /// Within a sample: left alone, the run is marked corrected.
    Done,
    /// One `align(ticks:)` takes the error out, then the run is marked corrected.
    Align(f64),
}

/// `correctStart`: both decks' clocks, read at the end of one block, say how far the incoming
/// deck's first tick missed the bar line.
#[allow(clippy::too_many_arguments)]
pub fn correct_start(
    sample_rate: f64,
    run: &AutoMixRun,
    blocks_agree: bool,
    out_tick: i64,
    out_tick_phase: f64,
    out_tempo: f64,
    out_playing: bool,
    in_tick: i64,
    in_tick_phase: f64,
) -> Correction {
    if !run.out_is_net || run.mult != 1.0 {
        return Correction::Skip;
    }
    let h = run.out_handoff_tick.unwrap_or(run.handoff_bar * 16);
    if !(blocks_agree && in_tick >= 1 && out_tick > h && out_playing) {
        return Correction::Wait;
    }
    let pos_out = (out_tick - 1) as f64 + out_tick_phase - h as f64;
    let pos_in = (in_tick - 1) as f64 + in_tick_phase;
    let spt = if out_tempo > 0.0 {
        sample_rate * 60.0 / (out_tempo * 4.0)
    } else {
        0.0
    };
    let err = pos_in - pos_out;
    if libm::fabs(err * spt) >= 1.0 {
        Correction::Align(-err)
    } else {
        Correction::Done
    }
}

/// The incoming deck's bar of the hand-off, `k` (0-based, capped at `bars`) from its tick
/// (`serviceRun`): 0 until it ticks.
pub fn mix_bar_index(run_bars: i64, in_tick: i64) -> i64 {
    if in_tick > 0 {
        run_bars.min((in_tick - 1) / 16)
    } else {
        0
    }
}

/// The crossfader, one step per bar, linear in position (equal power is the engine's law):
/// `c0 + (xEnd − c0) · k / bars`.
pub fn fader_position(c0: f64, x_end: f64, k: i64, bars: i64) -> f64 {
    c0 + (x_end - c0) * k as f64 / bars.max(1) as f64
}

/// The hand-off is over: the incoming deck has played all its bars, or stopped.
pub fn handoff_done(k: i64, bars: i64, incoming_playing: bool) -> bool {
    k >= bars || !incoming_playing
}

/// The crossfader value the outgoing deck's side ends on: A out → deck B up (1), else 0
/// (`xEnd: out.name == "A" ? 1 : 0`).
pub fn x_end_for(out_name: &str) -> f64 {
    if out_name == "A" {
        1.0
    } else {
        0.0
    }
}

/// A plan for a cued deck: what it holds, as is (`AutoMixPlan(kind: "cued")`).
pub fn cued_plan(n: i64, spec: Option<crate::spec::TrackSpec>, title: &str) -> AutoMixPlan {
    AutoMixPlan {
        n,
        spec,
        kind: "cued".into(),
        detail: alloc::format!("cued: {title}"),
        from: None,
    }
}

// MARK: the draw's context

/// `DJEngine.autoMixContext(out:inc:)`: what the draw reads (§3.1 of the Swift app's docs/AUTOMIX.md): the
/// outgoing deck's configuration (`c`, its `wanderContext`) — where it is heading if a swap is
/// still composing (`out_seed_target`) — with both decks' locks and the mix's tempo. A pure
/// merge: the host builds `c` from the outgoing deck and reads the rest off the two decks.
///
/// `out_key` is the outgoing deck's `wanderKey` (the key it plays in now), the key a key lock
/// that was never given one holds; `out_effective_bpm` its tempo after the fader.
pub fn auto_mix_context(
    mut c: WanderContext,
    out_seed_target: Option<&TrackSpec>,
    out_wander: &WanderState,
    inc_wander: &WanderState,
    out_key: Option<WanderKey>,
    out_effective_bpm: Option<f64>,
) -> WanderContext {
    if let Some(s) = out_seed_target {
        c.spec = s.clone();
    }
    c.locks = out_wander.locks.union(&inc_wander.locks).cloned().collect();
    c.anchor_bpm = out_wander.anchor_bpm.or(inc_wander.anchor_bpm);
    if c.locked("tempo") && c.anchor_bpm.is_none() {
        c.anchor_bpm = Some(Wander::bpm(&c.spec));
    }
    c.key = out_wander.key.clone().or_else(|| inc_wander.key.clone());
    if c.locked("key") && c.key.is_none() {
        c.key = out_key;
    }
    c.mix_bpm = out_effective_bpm;
    c
}

// MARK: dj_state

/// What `autoMixJSON` and `autoMixStatus` read off the decks: a deck's bar on the clock
/// auto-mix counts by (`autoMixBar`; None when the deck is missing or has no bar) and its net
/// clock's tick.
pub trait AutoMixDecks {
    /// `autoMixBar(deck)`.
    fn bar_of(&self, deck: &str) -> Option<i64>;
    /// `deck.wave.tick`; None when there is no such deck.
    fn tick_of(&self, deck: &str) -> Option<i64>;
}

impl AutoMixState {
    /// `DJEngine.autoMixJSON`: `dj_state.autoMix`.
    pub fn dj_state_json(&self, decks: &dyn AutoMixDecks) -> JsonValue {
        let s = self;
        let mut o = JsonObject::from_pairs([
            ("on", JsonValue::Bool(s.settings.on)),
            ("bars", JsonValue::int(s.settings.bars)),
            ("every", JsonValue::int(s.settings.every)),
            ("phase", JsonValue::str(s.phase.raw())),
            ("handoffs", JsonValue::int(s.handoffs)),
            ("paused", JsonValue::Bool(s.paused)),
            ("fader", JsonValue::str(s.fader())),
            (
                "log",
                JsonValue::Array(s.log.iter().rev().take(12).map(|h| h.json()).collect()),
            ),
        ]);
        o.set(
            "playing",
            s.playing.as_deref().map_or(JsonValue::Null, JsonValue::str),
        );
        o.set(
            "incoming",
            s.incoming().map_or(JsonValue::Null, JsonValue::str),
        );
        o.set(
            "idle",
            s.idle.as_deref().map_or(JsonValue::Null, JsonValue::str),
        );
        if let Some(next) = s.next() {
            o.set("next", next.json());
        }
        if let Some(run) = &s.run {
            o.set("handoffBar", JsonValue::int(run.handoff_bar));
            if let Some(bar) = decks.bar_of(&run.out) {
                if bar < run.handoff_bar {
                    o.set("handoffInBars", JsonValue::int(run.handoff_bar - bar));
                }
            }
            if let Some(t) = decks.tick_of(&run.inc) {
                if t > 0 {
                    o.set("mixBar", JsonValue::int(run.bars.min((t - 1) / 16 + 1)));
                }
            }
        } else if let (Some(h), Some(p)) = (s.handoff_bar, s.playing.as_deref()) {
            if let Some(bar) = decks.bar_of(p) {
                o.set("handoffBar", JsonValue::int(h));
                o.set("handoffInBars", JsonValue::int((h - bar).max(0)));
            }
        }
        if s.paused {
            o.set(
                "resumesInBars",
                JsonValue::int((AutoMix::QUIET_BARS - s.quiet_bars).max(1)),
            );
        }
        o.into()
    }

    /// `DJEngine.autoMixStatus`: one line for the mixer's foot: "A playing · → B in 23 bars",
    /// "mixing A → B · bar 3/8".
    pub fn status(&self, decks: &dyn AutoMixDecks) -> String {
        let s = self;
        if !s.settings.on {
            return "auto-mix off".into();
        }
        if let Some(why) = &s.idle {
            return alloc::format!("waiting: {why}");
        }
        if s.paused {
            return alloc::format!(
                "paused · resumes after {} quiet bars ({} to go)",
                AutoMix::QUIET_BARS,
                (AutoMix::QUIET_BARS - s.quiet_bars).max(1)
            );
        }
        if let Some(run) = &s.run {
            let what = match &run.plan.spec {
                Some(sp) => alloc::format!("{} · seed {}", sp.genre, Wander::seed_text(sp.seed)),
                None => "cued".into(),
            };
            return match s.phase {
                AutoMixPhase::Mixing => {
                    let bar = decks
                        .tick_of(&run.inc)
                        .map_or(1, |t| ((t - 1) / 16 + 1).max(1));
                    alloc::format!(
                        "mixing {} → {} · bar {}/{}",
                        run.out,
                        run.inc,
                        run.bars.min(bar),
                        run.bars
                    )
                }
                AutoMixPhase::Armed => alloc::format!("{}: {} · starting", run.inc, what),
                _ => {
                    let ready = if run.incoming_ready {
                        "ready"
                    } else {
                        "composing"
                    };
                    let left = decks.bar_of(&run.out).map_or(String::new(), |b| {
                        alloc::format!(" · in {} bars", (run.handoff_bar - b).max(0))
                    });
                    alloc::format!(
                        "{}: {} ({}) · {}{}",
                        run.inc,
                        what,
                        run.plan.kind,
                        ready,
                        left
                    )
                }
            };
        }
        if let (Some(p), Some(h)) = (s.playing.as_deref(), s.handoff_bar) {
            if let Some(bar) = decks.bar_of(p) {
                let other = if p == "A" { "B" } else { "A" };
                return alloc::format!("{p} playing · → {other} in {} bars", (h - bar).max(0));
            }
        }
        "auto-mix on".into()
    }
}
