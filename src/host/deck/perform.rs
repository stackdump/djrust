//! `Engine::perform(DeckAction)`: the one entry for everything a UI, a script or a replay does to
//! a deck. Port of `DJEngine.perform` and `DJEngine.applyAction` (`Session/SessionRecorder.swift`):
//! the single switch over the 75 ops, wrapped by the recording of the action that was done in
//! the form a replay needs.
//!
//! The controller's own [`Action`](crate::control::controller::Action)s reach the same
//! methods (`surface.rs`), so a button and a `DeckAction` cannot drift apart.
//!
//! What an op answers ([`Applied`]):
//!
//! - `Done`: it ran.
//! - `Ignored`: Swift's guard falls through, so does this (a loop in on a net deck, a macro on a
//!   sample deck, a net-only op on the loop channel). Not an error and not counted.
//! - `Deferred`: a feature a later slice ports. The action is recorded all the same, and
//!   [`Engine::unhandled`] counts it (`deckaction <op>`), as the monitor's list of what the Mac
//!   would have done. No op answers it now (auto-mix, its last, is on the decks:
//!   [`Engine::set_auto_mix`], [`Engine::auto_mix_now`], [`Engine::defer_auto_mix`]).
//!
//! **Threads.** Swift composes a loaded track and decodes a stinger in the background; here
//! `perform` composes on the calling thread, as `deterministic` replay does.

use std::sync::Arc;

use crate::automix::engine::touch_of_op;
use crate::control::macros::master_fx_slider_value;
use crate::deck::beat::{Quantize, SyncAlign};
use crate::deck::beat_grid::BeatGrid;
use crate::deck::turntable::SampleBuffer;
use crate::fx::unit::FxParam;
use crate::fx::{MasterFxKey, MasterMacro, ReverbKnob, StripKnob};
use crate::journal::event::JournalEvent;
use crate::session::action::DeckAction;
use crate::session::model::{DeckParams, DeckSource, Session, SessionError, DEMO_ID};
use crate::session::recorder::{source_descriptor, DeckSnapshot, SessionRecorder};
use crate::wave::NetMacro;

use super::engine::Engine;
use super::sample::{resample_buffer, PreparedSample};
use super::track::{PreparedTrack, TrackSource};

/// What an op did (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Done,
    Ignored(&'static str),
    Deferred(&'static str),
}

impl Applied {
    /// Did the op run, as opposed to falling through?
    pub fn ran(&self) -> bool {
        matches!(self, Applied::Done)
    }
}

/// What a live caller hands `perform` that it already has (`perform(_:prepared:buffer:)`): a
/// composed track so `load` does not build it again, or decoded audio for a `load` of a sample
/// (named by its content id in the record) or a `clipImport` / `clipRetune`.
#[derive(Default)]
pub struct PerformCtx {
    pub prepared: Option<PreparedTrack>,
    pub buffer: Option<Arc<SampleBuffer>>,
}

/// Where the always-on event log is written (`DJEngine.journal`): the engine says what
/// happened and on which engine frame; the sink stamps its own clock and sequence.
pub trait JournalSink: Send {
    fn record(&mut self, frame: i64, event: JournalEvent);
}

/// Resolves a sample's content id to audio (`DJEngine.sampleResolver`).
pub type SampleResolver = Box<dyn Fn(&str) -> Option<Arc<SampleBuffer>> + Send>;

/// The outcome of [`Engine::apply_deck_action`].
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub applied: Applied,
    /// The beat grid of a sample the action loaded (what the record names it by).
    pub grid: Option<BeatGrid>,
}

impl From<Applied> for Outcome {
    fn from(applied: Applied) -> Outcome {
        Outcome {
            applied,
            grid: None,
        }
    }
}

/// What a deck name in an action points at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Neither a deck nor the loop channel: Swift's `deck(n)?` is nil.
    Nothing,
    /// The loop channel `L`.
    Loop,
    Deck,
}

impl Engine {
    /// `DJEngine.frame`: samples rendered since the engine was built.
    pub fn frame(&self) -> i64 {
        self.loop_frames()
    }

    /// Last writer of each continuous parameter (`DJEngine.owners`: "A.volume", "crossfader",
    /// ...): a control id, or "ui". Drives soft takeover between devices.
    pub fn owners(&self) -> &std::collections::BTreeMap<String, String> {
        &self.owners
    }

    fn target(&self, deck: &str) -> Target {
        if self.index(deck).is_some() {
            Target::Deck
        } else if deck == "L" {
            Target::Loop
        } else {
            Target::Nothing
        }
    }

    pub(crate) fn is_net_deck(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.is_net())
    }

    /// Count what a later slice ports (`unhandled`, capped as [`Engine::miss`] caps it).
    pub(crate) fn miss_deferred(&mut self, op: &str) {
        if self.unhandled.len() < 256 {
            self.unhandled.push(format!("deckaction {op}"));
        }
    }

    /// `DJEngine.perform`: apply an input and record it (when a recorder or a journal is
    /// attached).
    pub fn perform(&mut self, a: &DeckAction, ctx: PerformCtx) -> Applied {
        let mut recorded = a.clone();
        // A sample loaded from a file is named by its content id, known only now.
        if let (
            DeckAction::Load {
                deck,
                source: DeckSource::Sample { title, .. },
            },
            Some(b),
        ) = (a, &ctx.buffer)
        {
            let id = b.content_id();
            self.loops.row.sample_library.insert(id.clone(), b.clone());
            recorded = DeckAction::Load {
                deck: deck.clone(),
                source: DeckSource::Sample {
                    id,
                    title: title.clone(),
                    grid: None,
                },
            };
        }
        // Outside audio on the row is named by its content id too (and the tempo it was given),
        // and a saved scene by what is in it, so a replay doesn't need the scene library.
        if matches!(
            a,
            DeckAction::ClipImport { .. } | DeckAction::SceneLaunch { clips: None, .. }
        ) {
            recorded = self.recorded_loop_action(a, ctx.buffer.as_deref());
        }
        let out = self.apply_deck_action(a, ctx);
        if let (
            Some(grid),
            DeckAction::Load {
                deck,
                source: DeckSource::Sample { id, title, .. },
            },
        ) = (out.grid, &recorded)
        {
            recorded = DeckAction::Load {
                deck: deck.clone(),
                source: DeckSource::Sample {
                    id: id.clone(),
                    title: title.clone(),
                    grid: Some(grid),
                },
            };
        }
        self.trace_deck_action(&recorded);
        self.record_action(recorded);
        out.applied
    }

    /// What `recorder?.record(recorded); journal?.record(.action(recorded))` do.
    pub(crate) fn record_action(&mut self, a: DeckAction) {
        let frame = self.frame();
        if let Some(r) = self.recorder.as_mut() {
            r.record(frame, a.clone());
        }
        if let Some(j) = self.journal.as_mut() {
            j.record(frame, JournalEvent::Action(a));
        }
    }

    /// `DJEngine.applyAction`: run an action, without recording it.
    pub fn apply_deck_action(&mut self, a: &DeckAction, mut ctx: PerformCtx) -> Outcome {
        if let (Some(t), Some(deck)) = (touch_of_op(&a.op()), a.deck()) {
            self.note_auto_mix_touch(Some(&deck), t);
        }
        if let DeckAction::Load { deck, source } = a {
            return self.load_action(deck, source, ctx);
        }
        // a track handed to an action that does not load it is not wanted: let go of it
        // (`give_back_track`)
        if let (Some(t), Some(deck)) = (ctx.prepared.take(), a.deck()) {
            self.give_back_track(&deck, t);
        }
        // the loop row's ops answer for themselves
        if self.apply_loop_action(a, ctx.buffer) {
            return Applied::Done.into();
        }
        let applied = self.apply_deck_op(a);
        if let Applied::Deferred(_) = applied {
            self.miss_deferred(&a.op());
        }
        applied.into()
    }

    /// Is `id` a macro either catalogue has (`Deck.isMacro(id:)`)?
    pub fn is_macro_id(id: &str) -> bool {
        NetMacro::from_id(id).is_some()
            || MasterMacro::from_id(id).is_some()
            || crate::fx::AuxMacro::from_id(id).is_some()
    }

    /// A hand on a master FX slider (its own units) on a deck or the loops' bus.
    pub fn set_master_fx_slider(&mut self, deck: &str, k: MasterFxKey, slider: i32) -> bool {
        if deck == "L" && self.index(deck).is_none() {
            self.set_loop_master_slider(k, slider);
            return true;
        }
        self.set_master_fx(deck, k, slider)
    }

    pub(crate) fn apply_deck_op(&mut self, a: &DeckAction) -> Applied {
        use DeckAction as A;
        match a {
            A::Play { deck, on } => match self.target(deck) {
                Target::Deck => {
                    self.set_playing(deck, *on);
                    Applied::Done
                }
                Target::Loop => Applied::Ignored("the loop channel has no transport"),
                Target::Nothing => Applied::Ignored("no such deck"),
            },
            A::Cue { deck } => self.on_deck(deck, |e| e.cue(deck)),
            A::HotCue { deck, slot } => self.on_deck(deck, |e| e.hot_cue(deck, *slot as usize)),
            A::ClearCue { deck, slot } => self.on_deck(deck, |e| e.clear_cue(deck, *slot as usize)),
            A::Loop { deck, op } => match op.as_str() {
                "in" => self.on_deck(deck, |e| e.loop_in(deck)),
                "out" => self.on_deck(deck, |e| e.loop_out(deck)),
                "halve" => self.on_deck(deck, |e| e.loop_halve(deck)),
                "double" => self.on_deck(deck, |e| e.loop_double(deck)),
                _ => Applied::Ignored("not a loop op"),
            },
            A::Param { deck, name, value } => {
                self.owners.insert(format!("{deck}.{name}"), "ui".into());
                self.apply_param(deck, name, *value)
            }
            A::Crossfader(v) => {
                self.owners.insert("crossfader".into(), "ui".into());
                self.set_crossfader(*v);
                Applied::Done
            }
            A::Master(v) => {
                self.owners.insert("master".into(), "ui".into());
                self.set_master(*v);
                Applied::Done
            }
            A::Load { .. } => unreachable!("load_action"),
            A::QueueNext { deck, spec } => self.on_deck(deck, |e| e.queue_next(deck, spec)),
            A::NextTrack { deck } => self.on_deck(deck, |e| e.skip_to_next_track(deck)),
            A::AutoAdvance { deck, on } => self.on_deck_or_loop_state(
                deck,
                |e| {
                    if let Some(d) = e.deck_mut(deck) {
                        d.auto_advance = *on;
                    }
                },
                |l| l.auto_advance = *on,
            ),
            A::Section { deck, index } => {
                let Ok(index) = usize::try_from(*index) else {
                    return Applied::Ignored("no such section");
                };
                self.on_net(deck, |e| {
                    e.launch_section(deck, index);
                })
            }
            A::Role { deck, role, on } => self.on_net(deck, |e| {
                if !e.is_role_disabled(deck, role) {
                    e.set_role(deck, role, *on);
                }
            }),
            A::RoleDisable { deck, role, on } => self.on_deck_or_loop_state(
                deck,
                |e| e.set_role_disabled(deck, role, *on),
                // `setRoleDisabled` on the loop channel's `Deck`: the set `currentParams` reads
                |l| {
                    if *on {
                        l.disabled_roles.insert(role.clone());
                    } else {
                        l.disabled_roles.remove(role);
                    }
                },
            ),
            A::Macro { deck, id } => self.apply_macro(deck, id, None),
            A::Panic { deck } => match self.target(deck) {
                Target::Nothing => Applied::Ignored("no such deck"),
                Target::Loop => {
                    self.loop_master_panic();
                    Applied::Done
                }
                Target::Deck => {
                    // net or sample, the deck's own chain: a sample deck's master bus is the
                    // `DeckCore`'s too (`Deck.panic`: `renderer.master?.panic()`)
                    self.panic(deck);
                    Applied::Done
                }
            },
            A::Stinger { deck, n } => self.on_net(deck, |e| e.stinger(deck, *n, 1)),
            A::Sync { deck, align } => match self.target(deck) {
                Target::Deck => {
                    let align = match align.as_str() {
                        "bar" => SyncAlign::Bar,
                        "tempo" => SyncAlign::TempoOnly,
                        _ => SyncAlign::Beat,
                    };
                    if let Some(other) = self.sync_partner(deck) {
                        self.sync(deck, &other, align);
                    }
                    Applied::Done
                }
                _ => Applied::Ignored("only a deck syncs"),
            },
            A::Quantize { deck, mode } => {
                let q = match mode.as_str() {
                    "beat" => Quantize::Beat,
                    "bar" => Quantize::Bar,
                    _ => Quantize::Off,
                };
                self.on_deck_or_loop_state(
                    deck,
                    |e| {
                        e.set_quantize(deck, q);
                    },
                    |l| l.quantize = q,
                )
            }
            A::Follow { deck, leader } => self.on_deck(deck, |e| {
                e.follow(deck, leader.as_deref());
            }),
            A::Grid { deck, bpm, offset } => {
                if self.is_sample_deck(deck) {
                    self.set_beat_grid(deck, BeatGrid::new(*bpm, *offset, 1.0));
                    Applied::Done
                } else {
                    Applied::Ignored("only a sample deck has a grid")
                }
            }
            A::VoiceTweak {
                deck,
                channel,
                params,
            } => self.on_net(deck, |e| {
                e.set_voice_tweak(deck, params, *channel);
            }),
            A::VoiceSynth { deck, channel, on } => self.on_net(deck, |e| {
                e.set_synth_voice(deck, *on, *channel);
            }),
            A::Lock { key, on } => {
                self.locked.retain(|k| k != key);
                if *on {
                    self.locked.push(key.clone());
                }
                Applied::Done
            }
            // on the loop channel's `Deck` the walk is only settings: it is never a net (`setWander`
            // reads no bar, a lock anchors to no spec and no key)
            A::Wander { deck, on } => self.on_deck_or_loop_state(
                deck,
                |e| e.set_wander(deck, *on),
                |l| l.wander.set_on(*on, None),
            ),
            A::WanderPace { deck, bars } => self.on_deck_or_loop_state(
                deck,
                |e| e.set_wander_pace(deck, *bars),
                |l| l.wander.set_pace(*bars),
            ),
            A::WanderLock {
                deck,
                dimension,
                on,
            } => self.on_deck_or_loop_state(
                deck,
                |e| e.set_wander_lock(deck, dimension, *on),
                |l| {
                    l.wander.set_lock_anchored(dimension, *on, None, None);
                },
            ),
            A::SetKit { deck, kit } => self.on_net(deck, |e| e.set_kit(deck, kit.as_deref())),
            A::SetPalette { deck, palette } => {
                self.on_net(deck, |e| e.set_palette(deck, palette.as_deref()))
            }
            A::SetRoleVoice { deck, role, voice } => {
                self.on_net(deck, |e| e.set_role_voice(deck, role, voice.as_deref()))
            }
            A::MixOut { deck, bars } => self.on_net(deck, |e| {
                e.mix_out(deck, *bars);
            }),
            A::MixIn { deck, bars } => self.on_net(deck, |e| {
                e.mix_in(deck, *bars);
            }),
            A::Rewind { deck } => self.on_deck(deck, |e| {
                e.rewind(deck);
            }),
            A::Focus { deck } => {
                if self.target(deck) == Target::Nothing {
                    Applied::Ignored("no such deck")
                } else {
                    self.focus = deck.clone();
                    Applied::Done
                }
            }
            A::StemMute { deck, stem, on } => self.on_net(deck, |e| {
                if e.is_stem_muted(deck, stem) != *on {
                    e.toggle_stem(deck, stem);
                }
            }),
            A::MasterFx { deck, key, value } => {
                let Some(k) = MasterFxKey::from_id(key) else {
                    return Applied::Ignored("no such master FX");
                };
                let slider = master_fx_slider_value(key, *value).unwrap_or(0) as i32;
                match self.target(deck) {
                    Target::Nothing => Applied::Ignored("no such deck"),
                    Target::Loop => {
                        self.set_loop_master_slider(k, slider);
                        Applied::Done
                    }
                    Target::Deck => {
                        self.set_master_fx(deck, k, slider);
                        Applied::Done
                    }
                }
            }
            A::Fx { deck, param, value } => {
                let Some(p) = FxParam::from_id(param) else {
                    return Applied::Ignored("no such FX control");
                };
                self.on_deck_or_loop(deck, |e| {
                    e.set_fx(deck, p, *value);
                })
            }
            A::FxReset { deck, on_beat } => {
                let at = if *on_beat {
                    self.fx_beat_frame(deck)
                } else {
                    0
                };
                self.on_deck_or_loop(deck, |e| {
                    e.fx_reset_at(deck, at);
                })
            }
            A::FxKill { deck, on, on_beat } => {
                let at = if *on_beat {
                    self.fx_beat_frame(deck)
                } else {
                    0
                };
                self.on_deck_or_loop(deck, |e| {
                    e.fx_kill_at(deck, *on, at);
                })
            }
            // `setAutoMix(on:bars:every:)`, `autoMixNow()`, `deferAutoMix(handoff:)`
            A::AutoMix { on, bars, every } => {
                self.set_auto_mix(*on, *bars, *every);
                Applied::Done
            }
            A::AutoMixNow => {
                self.auto_mix_now();
                Applied::Done
            }
            A::AutoMixDefer { handoff } => {
                self.defer_auto_mix(*handoff);
                Applied::Done
            }
            // the loop row's ops were answered by `apply_loop_action`
            _ => Applied::Ignored("not a deck op"),
        }
    }

    /// Run `f` on a real deck; the loop channel and a name that is neither do nothing.
    fn on_deck(&mut self, deck: &str, f: impl FnOnce(&mut Engine)) -> Applied {
        match self.target(deck) {
            Target::Deck => {
                f(self);
                Applied::Done
            }
            Target::Loop => Applied::Ignored("the loop channel has no such control"),
            Target::Nothing => Applied::Ignored("no such deck"),
        }
    }

    /// Run `f` on a deck, or `l` on the loop channel's own `Deck` state (`dj["L"]` is a `Deck`,
    /// so a deck-only op that sets a field `currentParams` reads back sets it on L too).
    fn on_deck_or_loop_state(
        &mut self,
        deck: &str,
        f: impl FnOnce(&mut Engine),
        l: impl FnOnce(&mut super::fxdeck::LoopChannel),
    ) -> Applied {
        match self.target(deck) {
            Target::Deck => {
                f(self);
                Applied::Done
            }
            Target::Loop => {
                l(&mut self.loop_channel);
                Applied::Done
            }
            Target::Nothing => Applied::Ignored("no such deck"),
        }
    }

    /// Run `f` on a deck that is playing a net (Swift's `guard mode == .net`).
    fn on_net(&mut self, deck: &str, f: impl FnOnce(&mut Engine)) -> Applied {
        match self.target(deck) {
            Target::Deck if self.is_net_deck(deck) => {
                f(self);
                Applied::Done
            }
            Target::Deck => Applied::Ignored("not a net deck"),
            Target::Loop => Applied::Ignored("the loop channel is not a net deck"),
            Target::Nothing => Applied::Ignored("no such deck"),
        }
    }

    /// Run `f` on a deck or on the loop channel (FX unit, master FX: every chain has one).
    fn on_deck_or_loop(&mut self, deck: &str, f: impl FnOnce(&mut Engine)) -> Applied {
        match self.target(deck) {
            Target::Nothing => Applied::Ignored("no such deck"),
            _ => {
                f(self);
                Applied::Done
            }
        }
    }

    /// `.param(name:value:)`.
    pub(crate) fn apply_param(&mut self, deck: &str, name: &str, v: f64) -> Applied {
        match self.target(deck) {
            Target::Nothing => Applied::Ignored("no such deck"),
            Target::Loop => self.loop_param(name, v),
            Target::Deck => match name {
                "volume" => {
                    self.set_volume(deck, v);
                    Applied::Done
                }
                "filter" => {
                    self.set_strip(deck, StripKnob::Filter, v);
                    Applied::Done
                }
                "gain" => {
                    self.set_gain(deck, v);
                    Applied::Done
                }
                "eqLow" => {
                    self.set_strip(deck, StripKnob::EqLow, v);
                    Applied::Done
                }
                "eqMid" => {
                    self.set_strip(deck, StripKnob::EqMid, v);
                    Applied::Done
                }
                "eqHigh" => {
                    self.set_strip(deck, StripKnob::EqHigh, v);
                    Applied::Done
                }
                "pitch" => {
                    self.set_pitch(deck, v);
                    Applied::Done
                }
                "mute" => {
                    self.set_deck_mute(deck, v > 0.5);
                    Applied::Done
                }
                "reverbWet" | "reverbSize" | "reverbTone" => {
                    self.set_reverb(deck, name, v);
                    Applied::Done
                }
                "autoGain" => {
                    self.set_auto_gain(deck, v > 0.5);
                    Applied::Done
                }
                _ => Applied::Ignored("not a deck parameter"),
            },
        }
    }

    /// `.param` aimed at the loop channel: its fader, trim, mute, EQ, filter and strip reverb are
    /// the loop bank's.
    fn loop_param(&mut self, name: &str, v: f64) -> Applied {
        match name {
            "volume" => {
                self.set_loop_volume(v);
                Applied::Done
            }
            "gain" => {
                self.set_loop_gain(v);
                Applied::Done
            }
            "mute" => {
                self.set_loop_mute(v > 0.5);
                Applied::Done
            }
            "filter" => self.loop_strip(StripKnob::Filter, v),
            "eqLow" => self.loop_strip(StripKnob::EqLow, v),
            "eqMid" => self.loop_strip(StripKnob::EqMid, v),
            "eqHigh" => self.loop_strip(StripKnob::EqHigh, v),
            "reverbWet" => {
                self.set_loop_reverb(ReverbKnob::Wet, v);
                Applied::Done
            }
            "reverbSize" => {
                self.set_loop_reverb(ReverbKnob::Size, v);
                Applied::Done
            }
            "reverbTone" => {
                self.set_loop_reverb(ReverbKnob::Tone, v);
                Applied::Done
            }
            "pitch" => {
                self.loop_channel.pitch = v;
                Applied::Ignored("the loop channel has no turntable")
            }
            // `d.autoGain = v > 0.5` on the loop channel's `Deck`: no sample to level, but the
            // flag is in its `currentParams`
            "autoGain" => {
                self.loop_channel.auto_gain = v > 0.5;
                Applied::Done
            }
            _ => Applied::Ignored("not a deck parameter"),
        }
    }

    fn loop_strip(&mut self, knob: StripKnob, v: f64) -> Applied {
        self.set_loop_strip(knob, v);
        Applied::Done
    }

    /// The `.macro` op: `Deck.fireMacro(id:duration:)` ([`Engine::fire_macro_id`]) after the
    /// guards of what it is aimed at: a net macro needs a net deck, a master macro goes on
    /// whichever chain the deck plays through; an id neither catalogue has does nothing.
    pub(crate) fn apply_macro(&mut self, deck: &str, id: &str, bars: Option<i32>) -> Applied {
        // the pan, shape and aux-FX macros (docs/AUX.md): the FX holds play on any strip, the
        // pan and shape moves need a net deck's parts
        if let Some(m) = crate::fx::AuxMacro::from_id(id) {
            let hold = matches!(m.kind(), crate::fx::aux_macro::AuxKind::Hold { .. });
            return match self.target(deck) {
                Target::Nothing => Applied::Ignored("no such deck"),
                Target::Loop if !hold => Applied::Ignored("the loop channel has no parts to move"),
                Target::Deck if !hold && !self.is_net_deck(deck) => {
                    Applied::Ignored("not a net deck")
                }
                _ => {
                    self.fire_aux_macro(deck, m, bars);
                    Applied::Done
                }
            };
        }
        let net = NetMacro::from_id(id).is_some();
        if !net && MasterMacro::from_id(id).is_none() {
            return Applied::Ignored("no such macro");
        }
        match self.target(deck) {
            Target::Nothing => Applied::Ignored("no such deck"),
            Target::Loop if net => Applied::Ignored("the loop channel is not a net deck"),
            Target::Deck if net && !self.is_net_deck(deck) => Applied::Ignored("not a net deck"),
            // A sample deck's own bus is its master bus (`fire(_: MasterMacro)`: `renderer.master`):
            // the macro lands on the next 4-beat line of the deck's grid (`sampleMasterFire`),
            // which `Engine::fire_master` works out.
            _ => {
                self.fire_macro_id(deck, id, bars.map(i64::from));
                Applied::Done
            }
        }
    }

    // MARK: loading

    /// `.load(deck, source)`: a composed track or a raw-`nets` share onto a net deck, audio or
    /// the demo loop onto the platter. `none` does nothing (it does not eject).
    ///
    /// A composed track handed in (`ctx.prepared`) that this does not load (the source is not a
    /// net, there is nothing to load) is let go of ([`Engine::give_back_track`]); its samplers
    /// wait in the deck's bank either way.
    fn load_action(&mut self, deck: &str, source: &DeckSource, mut ctx: PerformCtx) -> Outcome {
        if self.target(deck) != Target::Deck {
            return Applied::Ignored("no such deck").into();
        }
        if !matches!(source, DeckSource::Net(_) | DeckSource::RawNets(_)) {
            if let Some(t) = ctx.prepared.take() {
                self.give_back_track(deck, t);
            }
        }
        match source {
            DeckSource::None => Applied::Ignored("nothing to load").into(),
            DeckSource::Net(_) | DeckSource::RawNets(_) => {
                let track = match ctx.prepared {
                    Some(t) => Some(t),
                    None => {
                        let ts = match source {
                            DeckSource::Net(spec) => TrackSource::Net(spec.clone()),
                            DeckSource::RawNets(p) => TrackSource::RawNets(p.clone()),
                            _ => unreachable!(),
                        };
                        self.preparer(deck)
                            .and_then(|p| PreparedTrack::from_source(&ts, &p, 0))
                    }
                };
                match track {
                    Some(t) => {
                        self.load_net(deck, t);
                        Applied::Done.into()
                    }
                    None => Applied::Ignored("not a net share").into(),
                }
            }
            DeckSource::Sample { id, title, grid } => {
                if id == DEMO_ID {
                    self.load_demo(deck);
                    return Applied::Done.into();
                }
                // (the id the audio is known by: what perform named a buffer it was handed, else
                // the id the action carries)
                let handed = ctx.buffer.is_some();
                let found = ctx
                    .buffer
                    .or_else(|| self.sample_resolver.as_ref().and_then(|r| r(id)));
                let Some(mut b) = found else {
                    return Applied::Ignored("the sample is not here").into();
                };
                // Swift's turntable takes no other rate ("resample before loading"): whatever
                // reads a file converts it to the deck's, with `Deck.decode`, before it loads, and
                // the tempo is measured on that. Audio that arrives at another rate is converted
                // here the same way (`resample_buffer`, linear), before the grid is
                // read or measured; the converted audio is what the library holds under the
                // id the audio was named by, so a checkpoint's id still finds it.
                if b.sample_rate != self.sample_rate {
                    let key = if handed { b.content_id() } else { id.clone() };
                    b = Arc::new(resample_buffer(&b, self.sample_rate));
                    self.loops.row.sample_library.insert(key, b.clone());
                }
                // A recorded grid stands in for the tempo analysis; live, with none recorded,
                // the analysis runs (here, on the calling thread) and its result is recorded
                // with the load. Deterministic replay measures nothing, as Swift's does not.
                let grid = match grid {
                    Some(g) => Some(*g),
                    None if !self.deterministic => {
                        crate::deck::beat_grid::analyze(&b.left, b.right(), b.sample_rate)
                    }
                    None => None,
                };
                self.load_sample(deck, PreparedSample::with_grid(b, title, grid));
                Outcome {
                    applied: Applied::Done,
                    grid,
                }
            }
        }
    }

    // MARK: the session recorder

    /// `Deck.sourceDescriptor()`: what the deck holds, as a session can name it. A deck that is
    /// moving, or that holds something a session can't name, is refused.
    pub fn source_descriptor(&self, deck: &str) -> Result<DeckSource, SessionError> {
        let d = self.deck(deck).ok_or_else(|| SessionError::Unrecordable {
            deck: deck.to_string(),
        })?;
        let snap = if d.is_net() {
            DeckSnapshot::Net {
                playing: d.playing || d.mirror.is_playing(),
                tick: d.mirror.tick(),
                has_hot_cues: !d.hot_cues.is_empty(),
                spec: d.spec.clone(),
                raw_nets: d.track.raw_nets.clone(),
            }
        } else {
            let s = d.sample.as_ref();
            DeckSnapshot::Sample {
                playing: d.playing || d.mirror.is_playing(),
                position: d.mirror.turntable_position(),
                has_hot_cues: !d.hot_cues.is_empty(),
                looping: s.is_some_and(|s| s.looping),
                cue_point: s.map_or(0.0, |s| s.cue_point),
                buffer_id: s.map(|s| s.buffer.content_id()),
                is_demo: s.is_some_and(|s| s.is_demo),
                title: s.map(|s| s.title.clone()).unwrap_or_default(),
                grid: s.and_then(|s| s.grid),
            }
        };
        source_descriptor(deck, &snap)
    }

    /// `Deck.currentParams`: the deck's knobs as a session starts with them.
    pub fn current_params(&self, deck: &str) -> Option<DeckParams> {
        if deck == "L" && self.index(deck).is_none() {
            // the loop channel's: its strip and reverb, the pitch the bank has no DSP for, the
            // fields only its `Deck` has, set by the pokes (`LoopChannel`), its FX unit
            let l = &self.loop_channel;
            return Some(DeckParams {
                volume: self.loops.volume,
                gain: self.loops.gain,
                filter: l.strip.filter,
                eq_low: l.strip.eq_low,
                eq_mid: l.strip.eq_mid,
                eq_high: l.strip.eq_high,
                pitch: l.pitch,
                quantize: match l.quantize {
                    Quantize::Off => "off",
                    Quantize::Beat => "beat",
                    Quantize::Bar => "bar",
                }
                .to_string(),
                auto_advance: l.auto_advance,
                reverb_wet: l.reverb_wet,
                reverb_size: l.reverb_size,
                reverb_tone: l.reverb_tone,
                auto_gain: l.auto_gain,
                disabled_roles: l.disabled_roles.iter().cloned().collect(),
                wander: l.wander.settings(),
                fx_unit: l.fx_unit.clone(),
            });
        }
        let d = self.deck(deck)?;
        let mut disabled = d.disabled_roles.clone();
        disabled.sort();
        Some(DeckParams {
            volume: d.volume,
            gain: d.gain,
            filter: d.strip.filter,
            eq_low: d.strip.eq_low,
            eq_mid: d.strip.eq_mid,
            eq_high: d.strip.eq_high,
            pitch: d.pitch,
            quantize: match d.quantize {
                Quantize::Off => "off",
                Quantize::Beat => "beat",
                Quantize::Bar => "bar",
            }
            .to_string(),
            auto_advance: d.auto_advance,
            reverb_wet: d.reverb_wet,
            reverb_size: d.reverb_size,
            reverb_tone: d.reverb_tone,
            auto_gain: d.auto_gain,
            disabled_roles: disabled,
            // what a session's walk continues from: no log, no clock reading
            wander: d.walk.wander.settings(),
            fx_unit: d.fx_unit.clone(),
        })
    }

    /// `Deck.apply(_ p: DeckParams)`: a session's starting knobs (a replay's engine).
    pub fn apply_params(&mut self, deck: &str, p: &DeckParams) {
        if self.index(deck).is_none() {
            return;
        }
        self.set_volume(deck, p.volume);
        self.set_gain(deck, p.gain);
        self.set_strip(deck, StripKnob::Filter, p.filter);
        self.set_strip(deck, StripKnob::EqLow, p.eq_low);
        self.set_strip(deck, StripKnob::EqMid, p.eq_mid);
        self.set_strip(deck, StripKnob::EqHigh, p.eq_high);
        self.set_pitch(deck, p.pitch);
        self.set_quantize(
            deck,
            match p.quantize.as_str() {
                "beat" => Quantize::Beat,
                "bar" => Quantize::Bar,
                _ => Quantize::Off,
            },
        );
        if let Some(d) = self.deck_mut(deck) {
            d.auto_advance = p.auto_advance;
        }
        self.set_reverb(deck, "reverbWet", p.reverb_wet);
        self.set_reverb(deck, "reverbSize", p.reverb_size);
        self.set_reverb(deck, "reverbTone", p.reverb_tone);
        // a starting state: the gain is there from the first sample
        if let Some(d) = self.deck_mut(deck) {
            d.auto_gain = p.auto_gain;
        }
        self.apply_gain_now(deck);
        for r in &p.disabled_roles {
            self.set_role_disabled(deck, r, true);
        }
        if let Some(d) = self.deck_mut(deck) {
            d.walk.wander = p.wander.clone();
        }
        self.apply_fx_unit(deck, &p.fx_unit);
    }

    /// `SessionRecorder.start(on:)`: begin recording. The engine must be at rest: every deck
    /// stopped at its beginning with no cues or loops, holding audio or a composed track a
    /// replay can name. Row-wide settings that are not part of a session's starting state, the
    /// locked actions and the loops' FX unit are recorded first, as the actions that set them.
    pub fn start_session(&mut self) -> Result<(), SessionError> {
        let mut decks = std::collections::BTreeMap::new();
        let mut params = std::collections::BTreeMap::new();
        for d in &self.decks {
            decks.insert(d.name.clone(), self.source_descriptor(&d.name)?);
            if let Some(p) = self.current_params(&d.name) {
                params.insert(d.name.clone(), p);
            }
        }
        // a replay's decks start with no bar seen: so do these (they are at rest, at tick 0)
        for d in &mut self.decks {
            d.walk.wander.last_bar = None;
        }
        let rec = SessionRecorder::new(
            self.frame(),
            self.sample_rate,
            decks,
            params,
            self.crossfader,
            self.master,
            self.loops.gain,
            &self.auto_mix,
        );
        self.recorder = Some(rec);
        let defaults = crate::journal::replay::default_loop_settings();
        for a in self.loop_settings() {
            if !defaults.contains(&a) {
                self.record_session_only(a);
            }
        }
        let mut locked = self.locked.clone();
        locked.sort();
        for key in locked {
            self.record_session_only(DeckAction::Lock { key, on: true });
        }
        for a in self.loop_channel_fx_actions() {
            self.record_session_only(a);
        }
        Ok(())
    }

    fn record_session_only(&mut self, a: DeckAction) {
        let frame = self.frame();
        if let Some(r) = self.recorder.as_mut() {
            r.record(frame, a);
        }
    }

    /// `SessionRecorder.finish()`: stop recording and return the session.
    pub fn finish_session(&mut self) -> Option<Session> {
        let frame = self.frame();
        self.recorder.take().map(|r| r.finish(frame))
    }

    /// Does `perform` keep a session going?
    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }
}
