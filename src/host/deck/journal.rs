//! The engine's half of the journal (`Journal/JournalState.swift`, `JournalRewind.swift`,
//! `Notes.swift`): what a checkpoint reads off the engine ([`Engine::journal_state`]), how a
//! fresh engine is stood where one says ([`Engine::restore_state`]), and how a running engine is
//! put back to one in place ([`Engine::reset_to`]). The values and the codec are
//! `crate::journal`; the file is [`crate::host::journal`]; the stepping loop is
//! [`crate::host::replay`].
//!
//! Nothing here is an input: a restore and a reset are applied with
//! [`Engine::apply_deck_action`] and the deck calls themselves, so nothing is recorded.
//!
//! **The command ring.** Every deck call is a message on a 512-deep ring that the render thread
//! empties at the top of each block, and a full ring drops the rest silently
//! ([`Engine::dropped`]). A restore makes a great many calls at once, so the callers hand in a
//! `pump` that lets the render thread take what is queued
//! ([`Renderer::drain_commands`](super::Renderer::drain_commands)); it runs between the phases
//! below. (Swift's calls are immediate: there is nothing to wait for.)
//!
//! **Auto-mix.** `JournalState.autoMix` is the engine's [`AutoMixState`]
//! ([`Engine::capture_auto_mix`]: the settings, the hand-off counter, the countdown and a hand-off
//! in flight, the form `AutoMixState::checkpoint_json` keeps) and is stood back through
//! [`Engine::restore_auto_mix`] ([`Engine::stand_auto_mix`]): a restore takes up the hand-off
//! where it was, a reset lets go of the one running and puts auto-mix back to its default.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::deck::clips::LoopClip;
use crate::deck::turntable::{SampleBuffer, TurntableCommand};
use crate::fx::ReverbKnob;
use crate::journal::event::JournalEvent;
use crate::journal::state::{DeckCheckpoint, JournalState};
use crate::journal::stores::{favorite_cid, JournalStoreState, NotePosition, NoteTarget};
use crate::session::action::DeckAction;
use crate::session::model::{DeckParams, DeckSource, DEMO_ID};
use crate::session::types::AutoMixState;

use super::core::Command;
use super::engine::Engine;
use super::perform::PerformCtx;

/// What [`Engine::restore_staged`] leaves to its caller (`JournalState.StagedRestore`): the
/// net decks' playheads it did not seek (asked to defer them), and the decks to start once they
/// are where they belong.
#[derive(Clone, Debug, Default)]
pub struct StagedRestore {
    pub warnings: Vec<String>,
    /// `(deck, tick)`: a net deck's checkpoint position, to relocate to.
    pub net_seeks: Vec<(String, i64)>,
    pub to_play: Vec<String>,
}

impl StagedRestore {
    /// `StagedRestore.play`: start the decks that were playing.
    pub fn play(&self, e: &mut Engine) {
        for d in &self.to_play {
            e.set_playing(d, true);
        }
    }
}

impl Engine {
    // MARK: what a deck holds, as a journal names it

    /// `dj.sampleResolver?(id)` after `dj.sampleLibrary[id]`: audio by content id, from the
    /// library of what was loaded or imported this run, then the host's resolver.
    pub fn resolve_sample(&self, id: &str) -> Option<Arc<SampleBuffer>> {
        self.loops
            .row
            .sample_library
            .get(id)
            .cloned()
            .or_else(|| self.sample_resolver.as_ref().and_then(|r| r(id)))
    }

    /// `DJEngine.sampleID`: the id a loaded buffer is known by: the key the sample library holds
    /// it under (the smallest, if several), else its content id.
    pub fn sample_id(&self, buffer: &Arc<SampleBuffer>) -> String {
        self.loops
            .row
            .sample_library
            .iter()
            .find(|(_, b)| Arc::ptr_eq(b, buffer))
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| buffer.content_id())
    }

    /// `DJEngine.journalSource`: what a deck holds, as a journal names it. Like
    /// [`Engine::source_descriptor`], but at any moment: a deck playing mid-track is still named
    /// by its source.
    pub fn journal_source(&self, deck: &str) -> DeckSource {
        let Some(d) = self.deck(deck) else {
            return DeckSource::None;
        };
        if let Some(s) = &d.sample {
            let id = if s.is_demo {
                DEMO_ID.to_string()
            } else {
                d.clip_id
                    .clone()
                    .unwrap_or_else(|| self.sample_id(&s.buffer))
            };
            return DeckSource::Sample {
                id,
                title: s.title.clone(),
                grid: s.grid,
            };
        }
        d.net_source().unwrap_or(DeckSource::None)
    }

    /// `Deck.journalPosition`: where the deck is, as a checkpoint keeps it: the sample frame
    /// (sample deck, as the platter counts them) or the net tick (net deck); None with nothing
    /// loaded.
    pub fn journal_position(&self, deck: &str) -> Option<f64> {
        let d = self.deck(deck)?;
        if d.sample.is_some() {
            Some(d.mirror.turntable_position())
        } else if d.project.is_some() {
            Some(d.mirror.tick() as f64)
        } else {
            None
        }
    }

    /// `Deck.seek(journalPosition:)`: put the deck at a checkpoint's `position`. A net deck is
    /// relocated to the tick (inline in `deterministic` mode, else it lands like a section
    /// launch).
    pub fn seek_journal_position(&mut self, deck: &str, p: f64) {
        if self.is_sample_deck(deck) {
            self.quietly(|e| e.seek(deck, p));
        } else if self.is_net_deck(deck) {
            self.relocate(deck, p.round() as i64, None, None);
        }
    }

    // MARK: notes

    /// `DJEngine.noteTarget`: what a note made on `deck` now is about, and the title it plays
    /// under: the deck's net track, the clip on it, or its sample. None for an empty deck.
    pub fn note_target(&self, deck: &str) -> Option<(NoteTarget, String)> {
        let d = self.deck(deck)?;
        if let Some(s) = &d.sample {
            if let Some(id) = &d.clip_id {
                return Some((NoteTarget::Clip { id: id.clone() }, s.title.clone()));
            }
            let id = if s.is_demo {
                DEMO_ID.to_string()
            } else {
                self.sample_id(&s.buffer)
            };
            return Some((
                NoteTarget::Sample {
                    id,
                    file: s.source_file.clone(),
                },
                s.title.clone(),
            ));
        }
        if !d.is_net() {
            return None;
        }
        let cid = favorite_cid(&d.net_source()?)?;
        let title = d.project.as_ref().map(|p| p.name.clone())?;
        Some((NoteTarget::Track { cid }, title))
    }

    /// `DJEngine.notePosition`: where `deck` is now within its track, as a note position: a
    /// net's bar and beat from its tick (a sixteenth), a sample's from its beat grid (bar 1
    /// with no grid).
    pub fn note_position(&self, deck: &str) -> Option<NotePosition> {
        let d = self.deck(deck)?;
        if let Some(s) = &d.sample {
            let state = d.beat_state(self.sample_rate);
            let len = s.buffer.length() as f64;
            let mut p = d.mirror.turntable_position() % len;
            if p < 0.0 {
                p += len;
            }
            return Some(NotePosition::new(
                state.as_ref().map_or(0, |s| s.bar()) + 1,
                state.as_ref().map_or(0, |s| s.beat_in_bar()) + 1,
                Some(p / self.sample_rate),
            ));
        }
        if !d.is_net() {
            return None;
        }
        let tick = d.mirror.tick();
        Some(NotePosition::new(
            tick / 16 + 1,
            (tick % 16) / 4 + 1,
            Some(tick as f64 * 15.0 / d.base_tempo.max(1.0)),
        ))
    }

    /// `DJEngine.journalSampleFile`: a sample deck's file, for the journal: which content id came
    /// from which path, so a replay can find the audio (`JournalEvent.sampleFile`). The deck
    /// remembers the path too (`Deck.sourceFile`: a note on it names the file).
    pub fn journal_sample_file(&mut self, deck: &str, path: &str) {
        let Some(d) = self.deck_mut(deck) else { return };
        let Some(s) = d.sample.as_mut().filter(|s| !s.is_demo) else {
            return;
        };
        s.source_file = Some(path.to_string());
        let buffer = s.buffer.clone();
        if self.journal.is_some() {
            let id = self.sample_id(&buffer);
            let frame = self.frame();
            if let Some(j) = self.journal.as_mut() {
                j.record(
                    frame,
                    JournalEvent::SampleFile {
                        id,
                        path: path.to_string(),
                    },
                );
            }
        }
    }

    // MARK: capture

    /// `JournalState.autoMix`'s source (`dj.autoMix`): auto-mix's state as it is now. A
    /// checkpoint keeps what `AutoMixState::checkpoint_json` names of it (the log and the phase
    /// are not part of one).
    pub fn capture_auto_mix(&self) -> AutoMixState {
        self.auto_mix.clone()
    }

    /// `DJEngine.restoreAutoMix`: stand auto-mix where `state` says (a session's start, a
    /// journal restore, a reset's default): [`Engine::restore_auto_mix`].
    pub fn stand_auto_mix(&mut self, state: &AutoMixState) {
        self.restore_auto_mix(state);
    }

    /// `JournalState(engine:stores:playheads:)`: the engine's state now, with the stores'
    /// contents. `playheads` also keeps where each deck is (`DeckCheckpoint.position`); off by
    /// default, because states compare by their JSON and a replay's playheads are not the live
    /// run's to the frame. Loop clips' phases are never kept.
    pub fn journal_state(&self, stores: JournalStoreState, playheads: bool) -> JournalState {
        let mut s = JournalState::new(self.sample_rate);
        for d in &self.decks {
            s.decks.insert(
                d.name.clone(),
                DeckCheckpoint {
                    source: self.journal_source(&d.name),
                    params: self.current_params(&d.name).unwrap_or_default(),
                    playing: d.playing,
                    leader: d.leader.clone(),
                    clip: d.clip_id.clone(),
                    voices: if d.is_net() {
                        d.voice_settings.clone()
                    } else {
                        BTreeMap::new()
                    },
                    position: if playheads {
                        self.journal_position(&d.name)
                    } else {
                        None
                    },
                },
            );
        }
        s.loop_channel = self.current_params("L").unwrap_or_default();
        s.loop_mute = self.loops.mute;
        s.crossfader = self.crossfader;
        s.master = self.master;
        s.loop_settings = self.loop_settings();
        s.clips = self.clip_checkpoints();
        s.scenes = self.scenes().to_vec();
        s.removed = self.removed_checkpoint();
        let mut locked = self.locked.clone();
        locked.sort();
        s.locked = locked;
        s.stores = stores;
        s.auto_mix = self.capture_auto_mix();
        s
    }

    // MARK: restore

    /// `JournalState.restore(into:clip:)`: stand a fresh engine (with these decks) where `state`
    /// was. `find` finds a clip's audio by id (the loop library: the row, then `removed/`); a clip
    /// it can't find is left out and named in the returned warnings. The deck sources' samples
    /// come from the sample library, then [`Engine::sample_resolver`]. A deck with a `position`
    /// is put there after its load and params, before it plays; decks without one, and every
    /// loop clip, start from their beginnings. Nothing is recorded.
    pub fn restore_state(
        &mut self,
        state: &JournalState,
        find: &dyn Fn(&str) -> Option<LoopClip>,
        pump: &mut dyn FnMut(&mut Engine),
    ) -> Vec<String> {
        let staged = self.restore_staged(state, find, false, pump);
        staged.play(self);
        staged.warnings
    }

    /// `JournalState.restore(into:clip:deferNetSeeks:)`: [`restore_state`](Self::restore_state)
    /// up to the decks' transports: everything is in place but no deck has been started.
    /// `defer_net_seeks`: a net deck's `position` is handed back instead of relocated to.
    pub fn restore_staged(
        &mut self,
        state: &JournalState,
        find: &dyn Fn(&str) -> Option<LoopClip>,
        defer_net_seeks: bool,
        pump: &mut dyn FnMut(&mut Engine),
    ) -> StagedRestore {
        let mut staged = StagedRestore::default();
        // the row's settings, the clips and what is on them, the scenes, the removed
        staged.warnings.extend(self.restore_clip_row(
            &state.loop_settings,
            &state.clips,
            &state.scenes,
            &state.removed,
            find,
        ));
        pump(self);
        for (name, d) in &state.decks {
            if self.index(name).is_none() {
                continue;
            }
            match d.clip.as_deref().filter(|id| self.clip(id).is_some()) {
                Some(id) => self.play_clip(id, name),
                None => {
                    let ctx = PerformCtx {
                        prepared: None,
                        buffer: match &d.source {
                            DeckSource::Sample { id, .. } if id != DEMO_ID => {
                                self.resolve_sample(id)
                            }
                            _ => None,
                        },
                    };
                    self.apply_deck_action(
                        &DeckAction::Load {
                            deck: name.clone(),
                            source: d.source.clone(),
                        },
                        ctx,
                    );
                }
            }
            self.apply_params(name, &d.params);
            for (ch, v) in &d.voices {
                if !v.tweak.is_empty() {
                    self.set_voice_tweak(name, &v.tweak, *ch);
                }
                if v.use_synth {
                    self.set_synth_voice(name, true, *ch);
                }
            }
            if let Some(p) = d.position {
                if defer_net_seeks && self.is_net_deck(name) {
                    staged.net_seeks.push((name.clone(), p.round() as i64));
                } else {
                    self.seek_journal_position(name, p);
                }
            }
            pump(self);
        }
        // followers, once every deck is loaded and set (`dj[name].follow(dj.decks[leader])`)
        for (name, d) in &state.decks {
            if let Some(l) = &d.leader {
                self.follow(name, Some(l));
            }
        }
        let lc = &state.loop_channel;
        self.set_loop_volume(lc.volume);
        self.set_loop_gain(lc.gain); // the run's trim, not the engine's default
        self.set_loop_reverb(ReverbKnob::Wet, lc.reverb_wet);
        self.set_loop_reverb(ReverbKnob::Size, lc.reverb_size);
        self.set_loop_reverb(ReverbKnob::Tone, lc.reverb_tone);
        self.apply_fx_unit("L", &lc.fx_unit);
        self.set_loop_mute(state.loop_mute);
        self.set_crossfader(state.crossfader);
        self.set_master(state.master);
        self.locked = state.locked.clone();
        self.stand_auto_mix(&state.auto_mix);
        // the loops that were going come back, in at once
        self.restore_checkpoint_loops(&state.clips);
        pump(self);
        staged.to_play = state
            .decks
            .iter()
            .filter(|(n, d)| d.playing && self.index(n).is_some())
            .map(|(n, _)| n.clone())
            .collect();
        staged
    }

    // MARK: reset

    /// `Deck.eject`: take what is on the deck off it, stopped, as a fresh deck has nothing
    /// ([`Engine::journal_source`] says `none`). The turntable keeps its old buffer, silent,
    /// until the next load retires it through the ring; a net deck's project stays installed
    /// but paused.
    ///
    /// What the deck keeps beside that is what Swift keeps: the marking mirrors and role tables
    /// of the projects it held (`wave.latest`, `mirrors`: [`Engine::read_marking`] and
    /// [`Engine::marking_layout`] still answer), the fires and tick anchors rings and their
    /// frame counts, the sample bus and what it runs. What `eject` does end is the clip's stay
    /// and its FX with it (`clipID = nil`, whose `didSet` is `setClipFx(.off, now: true)`) and,
    /// leaving the platter, the FX unit's place (`syncFxUnit`). The deck is no net any more
    /// (`mode == .sample`), which `MainDeck::is_net` says by the project that is gone: so
    /// [`Engine::macro_activity`] is idle, though the paused runner still holds its macros.
    pub fn eject(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        let net = self.decks[i].is_net();
        let had_sample = self.decks[i].sample.is_some();
        if net {
            // leaveNetMode: the runner pauses
            self.command(deck, Command::Pause);
        }
        if had_sample {
            // the deck is a net deck's again (a later composed track must find the core there);
            // leaving sample mode stops the platter
            self.command(deck, Command::SampleMode(false));
            // (Swift's deck stays on the sample bus, where the unit already is, and syncs it to
            // the net's chain when a net loads; this one is on the net's chain from now, which
            // kept whatever it last had)
            self.sync_fx_unit(deck);
        }
        self.turntable(deck, TurntableCommand::Transport(false));
        self.turntable(deck, TurntableCommand::LoopOff);
        let d = &mut self.decks[i];
        d.playing = false;
        d.project = None;
        d.sections.clear();
        d.roles.clear();
        d.ring_roles.clear();
        d.ring_shapes.get_mut().clear();
        d.sample = None;
        d.spec = None;
        d.track.raw_nets = None;
        d.track.fixes = None;
        d.track.pass_applied = false;
        d.track.seed_target = None;
        d.hot_cues.clear();
        // `clipID = nil`: the clip's stay ends, and its FX with it
        self.clear_deck_clip(deck);
    }

    /// `DJEngine.resetDeck`: a deck as a fresh engine has it: stopped, empty, following no one,
    /// no voice tweaks, disabled roles or next track, default knobs; anything still composing
    /// for it superseded.
    pub fn reset_deck(&mut self, deck: &str) {
        let Some(i) = self.index(deck) else { return };
        self.decks[i].track.generation += 1;
        self.cancel_next(deck);
        self.decks[i].track.awaiting_next = false;
        self.decks[i].track.starting_next = false;
        self.follow(deck, None);
        self.eject(deck);
        self.decks[i].voice_settings.clear();
        let mut disabled = self.decks[i].disabled_roles.clone();
        disabled.sort();
        for r in disabled {
            self.set_role_disabled(deck, &r, false);
        }
        self.apply_params(deck, &DeckParams::default());
        self.set_wander(deck, false);
    }

    /// `DJEngine.reset(to:clip:)`: reset this engine to `state`, as
    /// [`restore_state`](Self::restore_state) stands a fresh one there. `find` finds a clip's audio
    /// by id (the loop library); a clip it can't find is left out and named in the returned
    /// warnings. Nothing is recorded: a reset is not an input.
    ///
    /// Not `deterministic`, composes and jumps already on their way are superseded (each deck's
    /// generation moves on, so whatever they finish lands nowhere), and a net deck's `position`
    /// is awaited as a jump before the decks start. In `deterministic` mode it all happens
    /// inline.
    pub fn reset_to(
        &mut self,
        state: &JournalState,
        find: &dyn Fn(&str) -> Option<LoopClip>,
        pump: &mut dyn FnMut(&mut Engine),
    ) -> Vec<String> {
        // a hand-off in flight lets go of its decks first
        self.stand_auto_mix(&AutoMixState::default());
        self.clear_loop_row();
        self.clear_scenes();
        let mut names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        names.sort();
        for n in &names {
            self.reset_deck(n);
        }
        self.locked.clear();
        self.owners.clear();
        self.set_crossfader(0.5);
        self.set_master(0.8);
        self.set_loop_mute(false);
        self.reset_loop_settings();
        pump(self);
        let defer = !self.deterministic;
        let staged = self.restore_staged(state, find, defer, pump);
        for (name, tick) in &staged.net_seeks {
            self.relocate(name, *tick, None, None);
            if let Some(i) = self.index(name) {
                self.settle(i, true);
            }
        }
        staged.play(self);
        staged.warnings
    }
}
