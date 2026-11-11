//! Auto-mix on the decks (`AutoMixEngine.swift`; `crate::automix` has the rule, the state
//! and the arithmetic, the Swift app's docs/AUTOMIX.md the design): which deck plays, the countdown to a
//! phrase boundary, composing the next track onto the idle deck with its mix-in baked in,
//! the mix-out, the start that lands the incoming deck's first tick on the outgoing deck's
//! bar line, the crossfader's steps, and letting go.
//!
//! [`Engine::service_auto_mix`] is `serviceAutoMix`: [`Engine::service`] calls it at the end
//! of every housekeeping tick (Swift: after `serviceWander`, at the same ticks live and in a
//! replay), so every derived step is a pure function of the engine's state at a tick. It
//! only uses commands that exist (a prepared project, play, tempo, align, the crossfader);
//! nothing runs on the audio thread that did not before.
//!
//! **Threads.** Live (`deterministic` off) the incoming track composes on a worker thread
//! ([`Task`]) and lands at the next tick, where Swift's detached task resumes on the main
//! actor; so does the mix-out a phrase ahead ([`MixOutAhead`]). In `deterministic` mode both
//! are inline, as a replay's.
//!
//! **Callers.** [`Engine::perform`]: `DeckAction.autoMix` -> [`Engine::set_auto_mix`],
//! `.autoMixNow` -> [`Engine::auto_mix_now`], `.autoMixDefer` -> [`Engine::defer_auto_mix`], and
//! every `DeckAction` that `autoMixTouch` maps (`crate::automix::engine::touch_of_op`) calls
//! [`Engine::note_auto_mix_touch`] *before* it is applied (`applyAction`'s first line). The
//! controller's `Action`s reach the same methods (`surface.rs`): `AutoMixTouch { deck, kind }`
//! -> `note_auto_mix_touch`, `AutoMixSet { on }` -> `set_auto_mix(on, bars, every)` with the
//! settings it has, `AutoMixNow` -> `auto_mix_now`; a press is recorded as the MIDI that made
//! it, so none of these is a `DeckAction` there. A defer the engine decided on by itself is
//! recorded as `DeckAction::AutoMixDefer` (recorder and journal, as Swift's `serviceRun` does)
//! and kept in [`AutoMixHost::recorded_defers`] for a test to read.

use std::sync::atomic::Ordering;

use crate::automix::engine::{
    arm_frames_net, arm_frames_sample, auto_mix_context, correct_start, cued_plan, fader_position,
    handoff_bar, handoff_done, incoming_align_ticks, mix_bar_index, prep_reached, x_end_for,
    AutoMixDecks, AutoMixSection, AutoMixTouch, Correction, CountdownBar, NetClock, PlayingDeck,
    RunOp,
};
use crate::automix::{AutoMix, AutoMixPhase, AutoMixRun, AutoMixState};
use crate::cid::share_cid;
use crate::deck::beat::SyncAlign;
use crate::json::JsonValue;
use crate::session::action::DeckAction;
use crate::wander::WanderContext;
use crate::wave::rests::{Entry, RestSchedule};
use crate::wave::MixOutTail;

use super::core::Command;
use super::engine::{crossfade_gain, Engine, Prepared};
use super::track::{MoveRequest, PreparedTrack, Task};

/// A start worked out but held back (`DJEngine.autoMixArmDelay`, tests).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingArm {
    pub frames: f64,
    pub countdown: i64,
}

/// The mix-out a hand-off prepares a phrase ahead (live only): handed to the outgoing deck in
/// bar H - 1 when nothing was loaded on it since. One per engine (`MixOutAhead`).
#[derive(Default)]
pub struct MixOutAhead {
    token: u64,
    task: Option<Task<Prepared>>,
}

impl MixOutAhead {
    /// Prepare `req` in the background, replacing what the slot held.
    pub fn request(&mut self, req: MoveRequest) {
        self.hold(Task::spawn(move || req.prepare()));
    }

    /// Hold a mix-out already preparing (the engine's own: [`Engine::bg_move`], which may
    /// offload it), replacing what the slot held.
    pub(crate) fn hold(&mut self, task: Task<Prepared>) {
        self.token += 1;
        self.task = Some(task);
    }

    /// The prepared project, off the slot (None when it is still preparing or was dropped).
    pub fn take(&mut self) -> Option<Prepared> {
        self.token += 1;
        self.task.take().and_then(|t| t.try_take())
    }

    /// `drop()`.
    pub fn drop_slot(&mut self) {
        let _ = self.take();
    }

    /// Is a prepare running or finished and waiting?
    pub fn is_requested(&self) -> bool {
        self.task.is_some()
    }

    /// Has the prepare finished (or is there none)?
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(Task::is_finished)
    }
}

enum Work {
    /// The incoming track composing.
    Compose(Task<PreparedTrack>),
    /// A cued track rebuilt with the mix-in.
    Cue(Task<Prepared>),
}

/// An incoming load that has not landed (the `Task.detached` of `loadIncoming`).
struct PendingIncoming {
    inc: String,
    n: i64,
    mine: u64,
    rest: Entry,
    /// `autoMixComposeStalls` was on when it started: the result is dropped.
    stalls: bool,
    work: Work,
}

/// What the engine keeps for auto-mix besides [`AutoMixState`] (the `DJEngine` fields around
/// `autoMix`).
#[derive(Default)]
pub struct AutoMixHost {
    /// `autoMixAhead`.
    pub ahead: MixOutAhead,
    /// Tests: hold a hand-off's start back this many service ticks after it is worked out, as a
    /// live start can land a block late (`autoMixArmDelay`).
    pub arm_delay: i64,
    /// `autoMixPendingArm`.
    pub pending_arm: Option<PendingArm>,
    /// Measurement only (`djctl automix --crossfade-only`): hand-offs without the mix-in /
    /// mix-out rests, the crossfader's gain law alone (`autoMixCrossfadeOnly`).
    pub crossfade_only: bool,
    /// Tests: the next live compose never lands (`autoMixComposeStalls`).
    pub compose_stalls: bool,
    /// The handoffs `autoMixDefer` was decided for by the engine, in order (each is also
    /// recorded: `recorder?.record(.autoMixDefer(handoff:))`, and the journal's): what a test
    /// reads without a recorder.
    pub recorded_defers: Vec<i64>,
    incoming: Vec<PendingIncoming>,
}

impl AutoMixDecks for Engine {
    fn bar_of(&self, deck: &str) -> Option<i64> {
        self.auto_mix_bar(deck)
    }
    fn tick_of(&self, deck: &str) -> Option<i64> {
        self.deck(deck).map(|d| d.mirror.tick())
    }
}

impl Engine {
    // MARK: the pokes

    /// `DJEngine.setAutoMix(on:bars:every:)` (`DeckAction.autoMix`): auto-mix on or off with
    /// its settings, the whole value at once. Off during a hand-off stops driving it where it
    /// is (the crossfader stays put; a mix-out or mix-in already applied plays out as by hand).
    /// Settings changed while on apply from the next hand-off (one in flight keeps its length).
    pub fn set_auto_mix(&mut self, on: bool, bars: i64, every: i64) {
        if let Some(run) = self.auto_mix.set_auto_mix(on, bars, every) {
            self.release_run(&run);
        }
        if on {
            self.service_auto_mix(); // the status reads right at once
        }
    }

    /// `DJEngine.autoMixNow()` (`DeckAction.autoMixNow`): hand off at the next phrase boundary
    /// at least [`AutoMix::NOW_PREP_BARS`] away. Ignored while a hand-off runs or auto-mix is
    /// off; resumes a paused countdown.
    pub fn auto_mix_now(&mut self) {
        self.auto_mix.auto_mix_now();
    }

    /// `DJEngine.deferAutoMix(handoff:)` (`DeckAction.autoMixDefer`): hand-off `n`'s incoming
    /// track was not composed by its mix-out point; it moves one phrase later, its plan kept.
    pub fn defer_auto_mix(&mut self, n: i64) {
        let Some(run) = self.auto_mix.defer_handoff(n) else {
            return;
        };
        self.auto_mix_host.ahead.drop_slot();
        if self.deterministic {
            return;
        }
        if run.out_is_net && self.index(&run.out).is_some() {
            self.request_mix_out_ahead(&run);
        }
        // a compose that never landed (superseded, or lost) is asked for again
        if !run.incoming_ready && self.index(&run.inc).is_some() {
            self.load_incoming(&run);
        }
    }

    /// `DJEngine.noteAutoMixTouch(_:_:)`: a poke reached `deck` (None: deck-less). Called by the
    /// `DeckAction` applier before the action and by the controllers' handlers, never by
    /// auto-mix's own moves.
    pub fn note_auto_mix_touch(&mut self, deck: Option<&str>, kind: AutoMixTouch) {
        let before = self.auto_mix.run.clone();
        let exists = deck.is_some_and(|d| self.index(d).is_some());
        let ops = self.auto_mix.note_touch(deck, kind, exists);
        let Some(run) = before else { return };
        for op in ops {
            match op {
                RunOp::CancelRun { .. } => {
                    // the start is called off: the incoming deck stops if it was started
                    if run.armed && self.deck(&run.inc).is_some_and(|d| d.playing) {
                        self.set_playing(&run.inc, false);
                    }
                    self.release_run(&run);
                }
                RunOp::EndRun => self.release_run(&run),
            }
        }
    }

    // MARK: the walk

    /// `DJEngine.serviceAutoMix()`: auto-mix's housekeeping (~100x/s, after `serviceWander`).
    pub fn service_auto_mix(&mut self) {
        self.land_incoming();
        if !self.auto_mix.settings.on {
            self.auto_mix.phase = AutoMixPhase::Off;
            return;
        }
        if self.auto_mix.run.is_some() {
            self.service_run();
        } else {
            self.service_countdown();
        }
        let started = self
            .auto_mix
            .run
            .as_ref()
            .and_then(|r| self.deck(&r.inc))
            .is_some_and(|d| d.mirror.tick() > 0);
        self.auto_mix.update_phase(started);
    }

    /// `DJEngine.autoMixBar(_:)`: the bar a deck is in on the clock auto-mix counts by: a net's
    /// song clock (`wave.tick / 16`, as Wander's), a sample deck's beat grid; None for a sample
    /// with no grid.
    pub fn auto_mix_bar(&self, deck: &str) -> Option<i64> {
        let d = self.deck(deck)?;
        if d.is_net() {
            return Some(d.mirror.tick() / 16);
        }
        let s = d.sample.as_ref()?;
        s.grid?;
        d.beat_state(self.sample_rate).map(|b| b.bar())
    }

    fn set_auto_mix_idle(&mut self, why: &str) {
        self.auto_mix.set_idle(why);
    }

    /// `autoMixContext(out:inc:)`: what the draw reads: the outgoing deck's configuration with
    /// both decks' locks and the mix's tempo.
    pub fn auto_mix_wander_context(&self, out: &str, inc: &str) -> Option<WanderContext> {
        let c = self.wander_context(out)?;
        let (o, i) = (self.deck(out)?, self.deck(inc)?);
        Some(auto_mix_context(
            c,
            o.track.seed_target.as_ref(),
            &o.walk.wander,
            &i.walk.wander,
            self.wander_key(out),
            o.effective_bpm(),
        ))
    }

    /// The sections of a net deck as the phrase arithmetic reads them (empty for a sample
    /// deck: `Deck.sections`).
    fn auto_mix_sections(&self, deck: &str) -> Vec<AutoMixSection> {
        self.deck(deck)
            .filter(|d| d.is_net())
            .map(|d| {
                d.sections
                    .iter()
                    .map(|s| AutoMixSection {
                        name: s.name.clone(),
                        start_tick: s.start_tick,
                        steps: s.steps,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Between hand-offs: which deck plays, the pause, the countdown, the prep point.
    fn service_countdown(&mut self) {
        let (Some(ai), Some(bi)) = (self.index("A"), self.index("B")) else {
            return self.set_auto_mix_idle("auto-mix needs decks A and B");
        };
        let (a_playing, b_playing) = (self.decks[ai].playing, self.decks[bi].playing);
        let ga = crossfade_gain("A", self.crossfader) * self.decks[ai].volume;
        let gb = crossfade_gain("B", self.crossfader) * self.decks[bi].volume;
        let (out, inc) = match crate::automix::engine::choose_playing(
            a_playing,
            b_playing,
            ga,
            gb,
            self.auto_mix.playing.as_deref(),
        ) {
            PlayingDeck::Nobody => {
                self.auto_mix.nobody_playing();
                return;
            }
            PlayingDeck::Both(out) => {
                // the louder by crossfader and fader is "playing": wait for the other to stop
                if self.auto_mix.playing.as_deref() != Some(out.as_str()) {
                    self.auto_mix.playing = Some(out.clone());
                }
                let other = if out == "A" { "B" } else { "A" };
                return self.set_auto_mix_idle(&format!("{other} is playing too"));
            }
            PlayingDeck::One(out) => {
                let inc = if out == "A" { "B" } else { "A" };
                (out, inc.to_string())
            }
        };
        if self.auto_mix.playing.as_deref() != Some(out.as_str()) {
            self.auto_mix.playing = Some(out.clone());
            self.auto_mix.last_bar = None;
            self.auto_mix.start_bar = None;
            self.auto_mix.handoff_bar = None;
        }
        let Some(bar) = self.auto_mix_bar(&out) else {
            return self.set_auto_mix_idle(&format!(
                "{out} plays a sample with no beat grid: cue a net by hand"
            ));
        };
        if self.auto_mix.observe_countdown(&out, bar) == CountdownBar::Quiet {
            return;
        }
        // what will come in: a deck the user cued, else a draw from the playing deck
        let inc_is_net = self.deck(&inc).is_some_and(|d| d.is_net());
        let cued = self.auto_mix.cued.as_deref() == Some(inc.as_str()) && inc_is_net;
        if self.auto_mix.cued.as_deref() == Some(inc.as_str()) && !cued {
            self.auto_mix.cued = None;
        }
        if !cued {
            let holds_sample = self
                .deck(&inc)
                .and_then(|d| d.sample.as_ref())
                .is_some_and(|s| !s.is_demo);
            if holds_sample {
                return self.set_auto_mix_idle(&format!(
                    "{inc} holds a sample: cue a net there or turn auto-mix off"
                ));
            }
            let out_d = self.deck(&out);
            let out_net = out_d.is_some_and(|d| d.is_net());
            let ctx = if out_net && out_d.is_some_and(|d| d.spec.is_some()) {
                self.auto_mix_wander_context(&out, &inc)
            } else {
                None
            };
            let Some(c) = ctx else {
                return self.set_auto_mix_idle(&if out_net {
                    format!("{out} plays a shared net: cue the next deck by hand")
                } else {
                    format!("{out} plays a sample: cue the next deck by hand")
                });
            };
            if !AutoMix::can_draw(&c) {
                return self.set_auto_mix_idle("every move is locked: cue the next deck by hand");
            }
        }
        self.auto_mix.idle = None;
        let prep = self.auto_mix.prep();
        // worked out once per countdown (a pause, Mix now, new settings or another deck clear
        // it); a bar already passed (a stall) is worked out again
        let mut h = self.auto_mix.handoff_bar.unwrap_or(-1);
        if h < bar {
            let cued_sample = !self.deck(&out).is_some_and(|d| d.is_net());
            h = handoff_bar(
                &self.auto_mix.settings,
                &self.auto_mix_sections(&out),
                self.auto_mix.start_bar,
                self.auto_mix.now_requested,
                bar,
                prep,
                cued_sample,
            );
            self.auto_mix.handoff_bar = Some(h);
        }
        if prep_reached(bar, h, prep) {
            self.begin_run(&out, &inc, h, prep, cued);
        }
    }

    // MARK: a hand-off

    /// The prep point: take the cue or draw, copy the disabled roles, hold both walks, compose
    /// the incoming track with its mix-in baked in, and (live) prepare the mix-out.
    fn begin_run(&mut self, out: &str, inc: &str, h: i64, prep: i64, cued: bool) {
        let bars = self.auto_mix.settings.bars;
        let handoffs = self.auto_mix.handoffs;
        let (plan, reuse);
        if cued {
            let Some(d) = self.deck(inc) else { return };
            let title = d
                .project
                .as_ref()
                .map_or_else(|| "—".to_string(), |p| p.name.clone());
            plan = cued_plan(handoffs, d.spec.clone(), &title);
            reuse = false;
        } else {
            let Some(c) = self.auto_mix_wander_context(out, inc) else {
                return;
            };
            let from = share_cid(&c.spec.payload());
            let kept = self.auto_mix.kept.clone().filter(|k| {
                let Some(d) = self.deck(inc) else {
                    return false;
                };
                k.from.as_deref() == Some(from.as_str())
                    && k.spec.is_some()
                    && d.spec == k.spec
                    && d.track.mix_in_end_tick == Some(bars * 16)
                    && d.mirror.tick() == 0
                    && !d.playing
            });
            if let Some(k) = kept {
                plan = k;
                reuse = true;
            } else {
                match AutoMix::plan(&c, handoffs, self.wander_key(out).as_ref()) {
                    Some(p) => {
                        plan = p;
                        reuse = false;
                    }
                    None => {
                        return self
                            .set_auto_mix_idle("every move is locked: cue the next deck by hand");
                    }
                }
            }
        }
        self.auto_mix.begin(reuse);
        // one walk: the incoming deck leaves out what the outgoing deck leaves out
        let out_roles: Vec<String> = self
            .deck(out)
            .map_or_else(Vec::new, |d| d.disabled_roles.clone());
        let inc_roles: Vec<String> = self
            .deck(inc)
            .map_or_else(Vec::new, |d| d.disabled_roles.clone());
        let mut enable: Vec<&String> = inc_roles
            .iter()
            .filter(|r| !out_roles.contains(r))
            .collect();
        enable.sort();
        for r in enable {
            self.set_role_disabled(inc, r, false);
        }
        let mut disable: Vec<&String> = out_roles
            .iter()
            .filter(|r| !inc_roles.contains(r))
            .collect();
        disable.sort();
        for r in disable {
            self.set_role_disabled(inc, r, true);
        }
        for name in [out, inc] {
            self.set_wander_held(name, true);
            self.drop_look_ahead(name);
        }
        let out_is_net = self.deck(out).is_some_and(|d| d.is_net());
        let mut run = AutoMixRun::new(plan, out, inc, h, prep, bars, out_is_net, x_end_for(out));
        run.incoming_ready = reuse;
        self.auto_mix.run = Some(run.clone());
        if !reuse {
            self.load_incoming(&run);
        }
        if !self.deterministic && run.out_is_net {
            self.request_mix_out_ahead(&run);
        }
    }

    /// Put the incoming track on the idle deck, stopped, at tick 0, its melodic lanes resting
    /// for the hand-off's bars: one prepare (`WaveOptions.extraRest`). Inline in
    /// `deterministic` replay, else off the main thread.
    fn load_incoming(&mut self, run: &AutoMixRun) {
        let inc = run.inc.as_str();
        let rest = RestSchedule::mix_in(
            0,
            if self.auto_mix_host.crossfade_only {
                0
            } else {
                run.bars
            },
        );
        let Some(i) = self.index(inc) else { return };
        self.decks[i].track.generation += 1;
        let mine = self.decks[i].track.generation;
        let n = run.plan.n;
        if !run.plan.cued() {
            if let Some(spec) = run.plan.spec.clone() {
                let Some(mut preparer) = self.preparer(inc) else {
                    return;
                };
                preparer.opts.extra_rest = Some(rest);
                if self.deterministic {
                    let track = PreparedTrack::compose(&spec, &preparer, 0);
                    self.load_net(inc, track);
                    return self.incoming_loaded(inc, n, mine, rest);
                }
                let stalls = std::mem::take(&mut self.auto_mix_host.compose_stalls); // one compose only
                let task = self.bg_compose(preparer, spec, 0);
                self.auto_mix_host.incoming.push(PendingIncoming {
                    inc: inc.to_string(),
                    n,
                    mine,
                    rest,
                    stalls,
                    work: Work::Compose(task),
                });
                return;
            }
        }
        // a cue: the track it holds, from its top, rebuilt with the mix-in
        let Some(req) = self.move_request(inc, 0, None, Some(rest)) else {
            return;
        };
        if self.deterministic {
            self.load_move(inc, req.prepare());
            return self.incoming_loaded(inc, n, mine, rest);
        }
        let task = self.bg_move(req);
        self.auto_mix_host.incoming.push(PendingIncoming {
            inc: inc.to_string(),
            n,
            mine,
            rest,
            stalls: false,
            work: Work::Cue(task),
        });
    }

    /// The composes that finished since the last tick land (Swift's `MainActor.run`).
    fn land_incoming(&mut self) {
        if self.auto_mix_host.incoming.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.auto_mix_host.incoming);
        for p in pending {
            let finished = match &p.work {
                Work::Compose(t) => t.is_finished(),
                Work::Cue(t) => t.is_finished(),
            };
            if !finished {
                self.auto_mix_host.incoming.push(p);
                continue;
            }
            let wanted = !p.stalls && self.incoming_wanted(&p.inc, p.n, p.mine);
            let Some(i) = self.index(&p.inc) else {
                continue;
            };
            match p.work {
                Work::Compose(t) => {
                    let Some(track) = t.wait_ok() else { continue };
                    if wanted {
                        self.load_net(&p.inc, track);
                        self.incoming_loaded(&p.inc, p.n, p.mine, p.rest);
                    } else {
                        self.discard(i, track);
                    }
                }
                Work::Cue(t) => {
                    let Some(prepared) = t.wait_ok() else {
                        continue;
                    };
                    if wanted {
                        self.load_move(&p.inc, prepared);
                        self.incoming_loaded(&p.inc, p.n, p.mine, p.rest);
                    } else {
                        self.discard_prepared(i, prepared);
                    }
                }
            }
        }
    }

    /// `incomingWanted`: is hand-off `n` still waiting for this compose on `inc`?
    pub fn incoming_wanted(&self, inc: &str, n: i64, mine: u64) -> bool {
        self.auto_mix.run.as_ref().map(|r| r.plan.n) == Some(n)
            && self
                .deck(inc)
                .is_some_and(|d| mine == d.track.generation && !d.playing)
    }

    /// `incomingLoaded`: the incoming track is on the deck.
    pub fn incoming_loaded(&mut self, inc: &str, n: i64, mine: u64, rest: Entry) {
        if !self.incoming_wanted(inc, n, mine) {
            return;
        }
        if let Some(d) = self.deck_mut(inc) {
            d.track.mix_in_end_tick = Some(rest.to);
        }
        if let Some(r) = self.auto_mix.run.as_mut() {
            r.incoming_ready = true;
        }
    }

    /// Live: the mix-out `mixOut(bars:)` would prepare in bar H - 1, prepared now.
    fn request_mix_out_ahead(&mut self, run: &AutoMixRun) {
        let Some((tick, tail)) = self.mix_out_point(&run.out, run.handoff_bar, run.bars) else {
            return;
        };
        let Some(req) = self.move_request(&run.out, tick, Some(tail), None) else {
            return;
        };
        let task = self.bg_move(req);
        self.auto_mix_host.ahead.hold(task);
        let generation = self
            .deck(&run.out)
            .and_then(|d| d.track.latest.as_ref().map(|l| l.generation));
        if let Some(r) = self.auto_mix.run.as_mut() {
            r.out_generation = generation;
        }
    }

    /// A hand-off from its prep point to its end.
    fn service_run(&mut self) {
        let Some(mut run) = self.auto_mix.run.clone() else {
            return;
        };
        if self.index(&run.out).is_none() || self.index(&run.inc).is_none() {
            self.auto_mix.run = None;
            return;
        }
        let (out, inc) = (run.out.clone(), run.inc.clone());
        let h = run.handoff_bar;
        // the mix-out point: bar H - 1 of the outgoing deck (the point of no return)
        if !run.mix_out_loaded && self.auto_mix_bar(&out).is_some_and(|bar| bar >= h - 1) {
            if !run.incoming_ready {
                // live only: the compose is late; the hand-off moves a phrase on, and that
                // is recorded
                self.auto_mix_host.recorded_defers.push(run.plan.n);
                self.record_action(DeckAction::AutoMixDefer {
                    handoff: run.plan.n,
                });
                self.defer_auto_mix(run.plan.n);
                return;
            }
            if run.out_is_net && !self.auto_mix_host.crossfade_only {
                if let Some((tick, tail)) = self.load_mix_out(&run) {
                    run.mix_out_tick = Some(tick);
                    run.out_handoff_tick = Some(tail.from_tick);
                    if let Some(d) = self.deck_mut(&out) {
                        d.track.auto_mixing_out = true;
                    }
                }
            }
            run.mix_out_loaded = true;
            self.auto_mix.run = Some(run.clone());
        }
        if !run.mix_out_loaded {
            return;
        }
        // the start, half a beat to a beat and a half before the hand-off bar
        if !run.armed {
            if let Some(mut pending) = self.auto_mix_host.pending_arm.take() {
                pending.countdown -= 1;
                if pending.countdown <= 0 {
                    self.start_incoming(&mut run, pending.frames);
                } else {
                    self.auto_mix_host.pending_arm = Some(pending);
                }
                self.auto_mix.run = Some(run);
                return;
            }
            let Some(frames) = self.arm_frames(&run) else {
                return;
            };
            if self.auto_mix_host.arm_delay > 0 {
                self.auto_mix_host.pending_arm = Some(PendingArm {
                    frames,
                    countdown: self.auto_mix_host.arm_delay,
                });
                return;
            }
            self.start_incoming(&mut run, frames);
            self.auto_mix.run = Some(run);
            return;
        }
        // the incoming deck's clock: tick 0 lands on the hand-off bar
        let Some((tick, playing)) = self.deck(&inc).map(|d| (d.mirror.tick(), d.playing)) else {
            return;
        };
        if !(tick > 0 || !playing) {
            return;
        }
        let k = mix_bar_index(run.bars, tick);
        if !run.corrected {
            self.correct_start(&mut run);
        }
        if run.c0.is_none() {
            run.c0 = Some(self.crossfader);
            run.last_fader = Some(self.crossfader);
        }
        // the crossfader, one step per bar (equal power: the engine's law)
        if !run.fader_taken {
            if let Some(c0) = run.c0 {
                match run.last_fader {
                    Some(last) if (self.crossfader - last).abs() > 1e-9 => run.fader_taken = true,
                    _ => {
                        let x = fader_position(c0, run.x_end, k, run.bars);
                        if self.crossfader != x {
                            self.set_crossfader(x);
                            self.owners.insert("crossfader".into(), "automix".into());
                        }
                        run.last_fader = Some(x);
                    }
                }
            }
        }
        self.auto_mix.run = Some(run.clone());
        if handoff_done(k, run.bars, playing) {
            self.finish_run(&run);
        }
    }

    /// Hand the mix-out to the outgoing deck in bar H - 1 (it takes over on H): the one
    /// prepared ahead if nothing was loaded on the deck since, else `mixOut` itself (inline in
    /// a replay; a late relocation live). Returns where the mix-out project starts.
    fn load_mix_out(&mut self, run: &AutoMixRun) -> Option<(i64, MixOutTail)> {
        let (tick, tail) = self.mix_out_point(&run.out, run.handoff_bar, run.bars)?;
        let ahead = if self.deterministic {
            None
        } else {
            self.auto_mix_host.ahead.take()
        };
        let generation = self
            .deck(&run.out)
            .and_then(|d| d.track.latest.as_ref().map(|l| l.generation));
        match ahead {
            Some(p)
                if generation == run.out_generation
                    && p.wave().start_tick == tick
                    && p.wave().tail == Some(tail) =>
            {
                self.load_move(&run.out, p);
            }
            _ => {
                self.auto_mix_host.ahead.drop_slot();
                self.mix_out(&run.out, run.bars);
            }
        }
        Some((tick, tail))
    }

    /// `renderedFrames(_:_:)`: the frames both decks have rendered, when they agree (their clocks
    /// are then from the end of one block). Swift's `renderedFrames` reads the counter acquiring;
    /// the render thread stores it releasing after the clocks it publishes.
    fn rendered_pair(&self, a: &str, b: &str) -> Option<i64> {
        let (x, y) = (self.deck(a)?, self.deck(b)?);
        let (fx, fy) = (
            x.mirror.rendered_frames.load(Ordering::Acquire),
            y.mirror.rendered_frames.load(Ordering::Acquire),
        );
        (fx == fy).then_some(fx)
    }

    /// When the start is due (from beat 3.5 of bar H - 1): the align that puts the incoming
    /// deck's first tick on the outgoing deck's hand-off bar line, worked out from both decks'
    /// clocks at the end of the same block (`renderedFrames`); None while not due (or the
    /// reading straddles a block).
    fn arm_frames(&self, run: &AutoMixRun) -> Option<f64> {
        let out = self.deck(&run.out)?;
        let inc = self.deck(&run.inc)?;
        if !inc.is_net() {
            return None;
        }
        inc.effective_bpm()?;
        // both clocks must be of one block: the decks agree on the frames rendered (`e`), and
        // still do once the outgoing deck's clock is read (a block between the reads moves `e`)
        let e = out.mirror.rendered_frames.load(Ordering::Acquire);
        if inc.mirror.rendered_frames.load(Ordering::Acquire) != e {
            return None;
        }
        let sr = self.sample_rate;
        let frames = if run.out_is_net {
            let installed = out
                .track
                .latest
                .as_ref()
                .is_some_and(|l| l.generation == out.mirror.generation.load(Ordering::Relaxed));
            arm_frames_net(
                sr,
                run,
                NetClock {
                    tick: out.mirror.tick(),
                    tick_phase: out.mirror.tick_phase(),
                    tempo: out.mirror.tempo(),
                    installed,
                },
            )?
        } else {
            let s = out.beat_state(sr)?;
            arm_frames_sample(sr, run.handoff_bar, s.beat, s.bpm)?
        };
        if frames == 0.0 {
            return Some(0.0); // missed: start now, the correction puts it in place
        }
        (self.rendered_pair(&run.out, &run.inc) == Some(e)).then_some(frames)
    }

    /// Start the incoming deck: tempo synced to the outgoing deck (and following it for the
    /// hand-off), playing, its first tick held back `frames` samples from the next block.
    fn start_incoming(&mut self, run: &mut AutoMixRun, frames: f64) {
        let (out, inc) = (run.out.clone(), run.inc.clone());
        run.prev_leader = self.deck(&inc).and_then(|d| d.leader.clone());
        let r = self.sync(&inc, &out, SyncAlign::TempoOnly);
        run.mult = r.map_or(1.0, |r| r.multiplier);
        self.set_playing(&inc, true);
        if frames >= 1.0 {
            if let Some(bpm) = self.effective_bpm(&inc) {
                if let Some(t) = incoming_align_ticks(frames, self.sample_rate, bpm) {
                    self.command(&inc, Command::Align(t));
                }
            }
        }
        if let Some(d) = self.deck_mut(&inc) {
            d.leader = Some(out);
        }
        run.armed = true;
    }

    /// In the hand-off's first bar: both decks' clocks, read at the end of one block, say how
    /// far the incoming deck's first tick missed the bar line (a live start can land a block
    /// late); one align takes it out before the crossfader brings it up. Within a sample: left
    /// alone.
    fn correct_start(&mut self, run: &mut AutoMixRun) {
        // `renderedFrames(out, inc)` is read before the clocks and again after them: a block
        // between the two readings is what the second one is for
        let before = self.rendered_pair(&run.out, &run.inc).is_some();
        let (Some(out), Some(inc)) = (self.deck(&run.out), self.deck(&run.inc)) else {
            return;
        };
        let (out_tick, out_phase, out_tempo, out_playing) = (
            out.mirror.tick(),
            out.mirror.tick_phase(),
            out.mirror.tempo(),
            out.mirror.is_playing(),
        );
        let (in_tick, in_phase) = (inc.mirror.tick(), inc.mirror.tick_phase());
        let agree = before && self.rendered_pair(&run.out, &run.inc).is_some();
        let c = correct_start(
            self.sample_rate,
            run,
            agree,
            out_tick,
            out_phase,
            out_tempo,
            out_playing,
            in_tick,
            in_phase,
        );
        match c {
            Correction::Skip | Correction::Done => run.corrected = true,
            Correction::Wait => {}
            Correction::Align(t) => {
                let inc = run.inc.clone();
                self.command(&inc, Command::Align(t));
                run.corrected = true;
            }
        }
    }

    /// The hand-off's last bar line: the crossfader at its end, the old deck stopped (its tail
    /// normally stopped it on this bar line), the walks released, the decks' roles swapped.
    fn finish_run(&mut self, run: &AutoMixRun) {
        if !run.fader_taken {
            self.set_crossfader(run.x_end);
            self.owners.insert("crossfader".into(), "automix".into());
        }
        if self.deck(&run.out).is_some_and(|d| d.playing) {
            self.set_playing(&run.out, false);
        }
        let (inc_spec, inc_playing) = self
            .deck(&run.inc)
            .map_or((None, false), |d| (d.spec.clone(), d.playing));
        self.auto_mix.finish(run, inc_spec, inc_playing);
        self.release_run(run);
    }

    /// `endRun(log:)`'s effects: let go of a hand-off where it is: the walks released, the
    /// incoming deck's leader back, the mix-out guard off (what was done stays done, as by
    /// hand).
    fn release_run(&mut self, run: &AutoMixRun) {
        self.auto_mix_host.ahead.drop_slot();
        self.auto_mix_host.pending_arm = None;
        self.set_wander_held(&run.out, false);
        if let Some(d) = self.deck_mut(&run.out) {
            d.track.auto_mixing_out = false;
        }
        let prev = run.prev_leader.clone().filter(|n| self.index(n).is_some());
        self.set_wander_held(&run.inc, false);
        if let Some(d) = self.deck_mut(&run.inc) {
            if run.armed {
                d.leader = prev;
            }
        }
    }

    /// `restoreAutoMix`: stand auto-mix where `state` says (a session's start, a journal
    /// checkpoint, a rewind): a hand-off in flight is let go first; one in `state` is taken up
    /// where it was (its walks held again; what the decks were loaded with comes from the
    /// checkpoint's sources, so a mix-in or mix-out already applied is not in them: the
    /// hand-off still ends on its bar line).
    pub fn restore_auto_mix(&mut self, state: &AutoMixState) {
        if let Some(run) = self.auto_mix.end_run() {
            self.release_run(&run);
        }
        self.auto_mix = self.auto_mix.restored(state);
        if let Some(run) = self.auto_mix.run.clone() {
            for name in [&run.out, &run.inc] {
                self.set_wander_held(name, true);
            }
            if run.mix_out_loaded && run.out_is_net {
                if let Some(d) = self.deck_mut(&run.out) {
                    d.track.auto_mixing_out = true;
                }
            }
            if run.armed && self.index(&run.out).is_some() {
                if let Some(d) = self.deck_mut(&run.inc) {
                    d.leader = Some(run.out.clone());
                }
            }
        }
    }

    // MARK: dj_state

    /// `DJEngine.autoMixJSON`: `dj_state.autoMix`.
    pub fn auto_mix_json(&self) -> JsonValue {
        self.auto_mix.dj_state_json(self)
    }

    /// `DJEngine.autoMixStatus`: one line for the mixer's foot.
    pub fn auto_mix_status(&self) -> String {
        self.auto_mix.status(self)
    }

    /// Wait for every background compose auto-mix has running (the incoming track, the
    /// mix-out ahead) and land it: what a test does where Swift polls with `eventually`.
    pub fn wait_auto_mix(&mut self) {
        for p in &self.auto_mix_host.incoming {
            match &p.work {
                Work::Compose(t) => {
                    while !t.is_finished() {
                        std::thread::yield_now();
                    }
                }
                Work::Cue(t) => {
                    while !t.is_finished() {
                        std::thread::yield_now();
                    }
                }
            }
        }
        self.land_incoming();
    }
}
