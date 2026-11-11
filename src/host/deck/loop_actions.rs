//! The loop row's `DeckAction`s on the engine (`DJEngine.applyAction`'s loop cases, and the
//! recorded forms `SessionRecorder.perform` writes): one method per op, so the later wave's
//! `Engine::perform` calls [`Engine::apply_loop_action`] for every action it does not own and
//! stops at the first that answers true.
//!
//! The recorded form differs from the action in two cases: a saved scene is recorded with the
//! clips in it (a replay needs no scene library), and outside audio on the row is recorded by
//! its content id and the tempo it was given ([`Engine::recorded_loop_action`]).

use std::sync::Arc;

use crate::deck::clips::{import_tempo, LoopClip, LoopFade};
use crate::deck::loops::{LoopFollow, LoopScene};
use crate::journal::state::ClipCheckpoint;
use crate::deck::turntable::SampleBuffer;
use crate::session::action::DeckAction;

use super::engine::Engine;

impl Engine {
    /// Do the loop row's action `a`; false if it is not one of them (nothing is done). `buffer`
    /// is the audio an action carries when it came with some (`clipImport` from a file,
    /// `clipRetune` rendered off the main thread; without one it renders here).
    pub fn apply_loop_action(&mut self, a: &DeckAction, buffer: Option<Arc<SampleBuffer>>) -> bool {
        use DeckAction as A;
        match a {
            A::LoopRecord { deck, on, bars } => {
                self.loop_record(deck, *on, *bars);
            }
            A::GrabBars { deck, bars } => {
                self.grab_last_bars(deck, *bars);
            }
            A::Clip { deck, id } => self.play_clip(id, deck),
            A::ClipLoop { id, on } => self.set_looping(id, *on),
            A::ClipGain { id, value } => self.set_clip_gain(id, *value, None),
            A::ClipCrop { id, from, to } => {
                self.crop_clip(id, *from, *to);
            }
            A::ClipRetune { id, voices } => {
                let b = buffer.map(|b| (*b).clone());
                self.retune_clip(id, voices, b);
            }
            A::ClipSequence { steps, replacing } => {
                self.make_sequence(steps, replacing.as_deref());
            }
            A::ClipRemove { id } => self.remove_clip(id),
            A::ClipRestore { id, at } => {
                self.restore_clip(id, *at);
            }
            A::ClipForget { id } => self.forget_removed_clip(id),
            A::ClipRename { id, title } => self.rename_clip(id, title),
            A::ClipColor { id, color } => self.set_clip_color(id, color.as_deref()),
            A::ClipMove { id, before } => self.move_clip(id, before.as_deref()),
            A::ClipImport {
                id,
                title,
                bpm,
                beats,
                provenance,
            } => {
                // `buffer ?? sampleLibrary[id] ?? sampleResolver?(id)` (SessionRecorder.swift)
                let found = buffer.or_else(|| self.resolve_sample(id));
                if let Some(b) = found {
                    self.import_clip(
                        (*b).clone(),
                        title,
                        (*bpm > 0.0).then_some(*bpm),
                        *beats,
                        provenance.clone(),
                    );
                }
            }
            A::LoopQuantize { beats } => self.set_loop_quantum(*beats),
            A::LoopFollow { mode } => {
                if let Some(m) = LoopFollow::from_raw(mode) {
                    self.set_loop_follow(m);
                }
            }
            A::LoopFade { mode } => {
                if let Some(f) = LoopFade::from_raw(mode) {
                    self.set_loop_fade(f);
                }
            }
            A::ClipQuantize { id, beats } => self.set_clip_quantum(id, *beats),
            A::ClipFade { id, mode } => match mode {
                Some(m) => {
                    if let Some(f) = LoopFade::from_raw(m) {
                        self.set_clip_fade(id, Some(f));
                    }
                }
                None => self.set_clip_fade(id, None),
            },
            A::ClipFollow { id, mode } => match mode {
                Some(m) => {
                    if let Some(f) = LoopFollow::from_raw(m) {
                        self.set_clip_follow(id, Some(f));
                    }
                }
                None => self.set_clip_follow(id, None),
            },
            A::TapBars { bars } => self.set_tap_take_bars(*bars),
            A::SceneSave { name } => {
                self.save_scene(name);
            }
            A::SceneLaunch { name, clips } => match clips {
                Some(c) => {
                    self.launch_scene_clips(c);
                }
                None => {
                    self.launch_scene(name);
                }
            },
            A::SceneRename { name, to } => self.rename_scene(name, to),
            A::SceneDelete { name } => self.delete_scene(name),
            A::ClipMute { id, on } => self.set_clip_mute(id, *on),
            A::ClipSolo { id, on } => self.set_clip_solo(id, *on),
            A::ClipKeyLock { id, on } => self.set_clip_key_lock(id, *on),
            A::LoopKeyLock { on } => self.set_loop_key_lock(*on),
            A::ClipFx { id, fx } => self.set_clip_fx(id, fx, None),
            A::LoopFxLink { on } => self.set_loop_fx_link(*on),
            _ => return false,
        }
        true
    }

    /// The form of `a` a session records (`SessionRecorder.perform`): a saved scene launched
    /// by name is recorded with its clips, outside audio on the row by its content id and
    /// tempo (the audio joins the sample library, so the recorded id names it). Any other
    /// action is recorded as it is.
    pub fn recorded_loop_action(
        &mut self,
        a: &DeckAction,
        buffer: Option<&SampleBuffer>,
    ) -> DeckAction {
        match a {
            DeckAction::ClipImport {
                title,
                bpm,
                beats,
                provenance,
                ..
            } => {
                let Some(b) = buffer else { return a.clone() };
                let id = b.content_id();
                self.loops
                    .row
                    .sample_library
                    .insert(id.clone(), Arc::new((*b).clone()));
                DeckAction::ClipImport {
                    id,
                    title: title.clone(),
                    bpm: if *bpm > 0.0 { *bpm } else { import_tempo(b, *beats) },
                    beats: *beats,
                    provenance: provenance.clone(),
                }
            }
            DeckAction::SceneLaunch { name, clips: None } => match self.scene(name) {
                Some(s) => DeckAction::SceneLaunch {
                    name: name.clone(),
                    clips: Some(s.clips.clone()),
                },
                None => a.clone(),
            },
            _ => a.clone(),
        }
    }
}

// MARK: the journal's half (JournalState.swift, JournalReplay.swift)

impl Engine {
    /// The clips on the row as a journal checkpoint holds them (`JournalState(engine:).clips`):
    /// each with its level, mute, solo, key lock, launch settings, FX and whether it is looping
    /// (or about to). A loop's phase is never kept.
    pub fn clip_checkpoints(&self) -> Vec<ClipCheckpoint> {
        let looping = self.looping_clips();
        self.loops
            .row
            .clips
            .iter()
            .map(|c| ClipCheckpoint {
                id: c.id.clone(),
                title: c.title.clone(),
                color: c.color.clone(),
                gain: self.clip_gain(&c.id),
                muted: self.is_clip_muted(&c.id),
                soloed: self.is_clip_soloed(&c.id),
                key_lock: self.loops.row.settings.key_locks.get(&c.id).copied(),
                quantum: self.clip_quantum(&c.id),
                fade: self.clip_fade(&c.id).map(|f| f.raw().to_string()),
                follow: self.clip_follow(&c.id).map(|f| f.raw().to_string()),
                looping: looping.contains(&c.id),
                fx: c.fx.clone(),
            })
            .collect()
    }

    /// The clips taken off the row this run, newest first, with the place each had
    /// (`JournalState.removed`).
    pub fn removed_checkpoint(&self) -> Vec<(String, i64)> {
        self.loops
            .row
            .removed
            .iter()
            .map(|r| (r.clip.id.clone(), r.index))
            .collect()
    }

    /// The row half of `JournalState.restore(into:)`: the row's settings, the clips (audio from
    /// `find`, the library's) with their levels and settings, the scenes, the removed clips
    /// (oldest first, so the newest ends up first). What has no audio is skipped and said in the
    /// warnings. The decks' own clip (`DeckCheckpoint.clip`) is [`Engine::play_clip`] after
    /// this, and the loops that were going come back with
    /// [`restore_checkpoint_loops`](Self::restore_checkpoint_loops) once the decks are loaded.
    pub fn restore_clip_row(
        &mut self,
        loop_settings: &[DeckAction],
        clips: &[ClipCheckpoint],
        scenes: &[LoopScene],
        removed: &[(String, i64)],
        find: &dyn Fn(&str) -> Option<LoopClip>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for a in loop_settings {
            self.apply_loop_action(a, None);
        }
        let mut found = Vec::new();
        for c in clips {
            match find(&c.id) {
                Some(lc) => found.push(lc),
                None => warnings.push(format!("clip {} has no audio", c.id)),
            }
        }
        self.restore_clips(&found);
        for c in clips {
            let Some(on_row) = self.clip(&c.id) else { continue };
            if on_row.title != c.title {
                self.rename_clip(&c.id, &c.title);
            }
            self.set_clip_color(&c.id, c.color.as_deref());
            if c.gain != 1.0 {
                self.set_clip_gain(&c.id, c.gain, None);
            }
            if c.muted {
                self.set_clip_mute(&c.id, true);
            }
            if c.soloed {
                self.set_clip_solo(&c.id, true);
            }
            if let Some(k) = c.key_lock {
                self.set_clip_key_lock(&c.id, k);
            }
            if let Some(q) = c.quantum {
                self.set_clip_quantum(&c.id, Some(q));
            }
            if let Some(f) = c.fade.as_deref().and_then(LoopFade::from_raw) {
                self.set_clip_fade(&c.id, Some(f));
            }
            if let Some(f) = c.follow.as_deref().and_then(LoopFollow::from_raw) {
                self.set_clip_follow(&c.id, Some(f));
            }
            if self.clip_fx(&c.id) != c.fx {
                self.set_clip_fx(&c.id, &c.fx, None); // the run's, not the library's
            }
        }
        self.restore_scenes(scenes);
        for (id, index) in removed.iter().rev() {
            match find(id) {
                Some(lc) => self.remember_removed_clip(lc, *index),
                None => warnings.push(format!("removed clip {id} has no audio")),
            }
        }
        warnings
    }

    /// The loops that were going at the checkpoint come back, in at once (the end of
    /// `restore(into:)`).
    pub fn restore_checkpoint_loops(&mut self, clips: &[ClipCheckpoint]) {
        for c in clips {
            if c.looping && self.clip(&c.id).is_some() {
                self.start_loop(&c.id, true);
            }
        }
    }

    /// `Journal.apply` for a loop action: a clip this replay didn't make (a live take that
    /// rendered differently) comes from the library (`find`), so the moves on it still land;
    /// an import whose audio is nowhere is warned of. False if `a` is not a loop action.
    pub fn replay_loop_action(
        &mut self,
        a: &DeckAction,
        find: &dyn Fn(&str) -> Option<LoopClip>,
        warnings: &mut Vec<String>,
    ) -> bool {
        for id in crate::journal::event::clip_ids(a) {
            let removed = self.loops.row.removed.iter().any(|r| r.clip.id == id);
            if self.clip(&id).is_none() && !removed {
                if let Some(c) = find(&id) {
                    self.restore_clips(&[c]);
                }
            }
        }
        if let DeckAction::ClipImport { id, title, .. } = a {
            if self.clip(id).is_none() && !self.loops.row.sample_library.contains_key(id) {
                let what = if title.is_empty() { id } else { title };
                warnings.push(format!("imported clip {what} has no audio"));
            }
        }
        self.apply_loop_action(a, None)
    }

    /// `.libraryRestore(id, index)`: a clip the library had taken off the row in an earlier run
    /// is remembered as removed, so a replayed restore finds it.
    pub fn replay_library_restore(
        &mut self,
        id: &str,
        index: i64,
        find: &dyn Fn(&str) -> Option<LoopClip>,
        warnings: &mut Vec<String>,
    ) {
        match find(id) {
            Some(c) => self.remember_removed_clip(c, index),
            None => warnings.push(format!("library clip {id} has no audio")),
        }
    }
}
