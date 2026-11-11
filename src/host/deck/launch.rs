//! How loops come in and go out under the mixer (`LoopLaunch.swift`, `LoopFollow.swift`): the
//! launch boundary (a beat, a bar, a 2-, 4- or 8-bar phrase of the reference deck), a clip's own
//! boundary and tempo follow, the countdown to a waiting start or stop, the row's settings as
//! the actions that set them, and scenes: saved sets of loops (with their levels and FX) that
//! switch in together on one boundary.
//!
//! The decisions are `crate::deck::loops` ([`valid_loop_quantum`], [`plan_scene_launch`],
//! [`loop_countdown`], [`LoopFollow`]) and `crate::deck::clips::ClipSettings`; this is the
//! engine half that reads the voices and the decks.

use crate::deck::loops::{
    loop_countdown, plan_scene_launch, valid_clip_quantum, valid_loop_quantum,
    valid_tap_take_bars, LoopFollow, LoopScene, SceneClip, SceneOp,
};
use crate::deck::stinger::{service_stinger_stops, stinger_stop_frame, StopVoice};
use crate::session::action::DeckAction;
use crate::sound::stinger::StingerAssignment;

use super::engine::Engine;
use super::loops::{StartOpts, LOOP_LEAD};

impl Engine {
    // MARK: launch boundary

    /// What loops start and stop on, in beats (`loopQuantum`).
    pub fn loop_quantum(&self) -> i64 {
        self.loops.quantum
    }

    /// What loops start and stop on, in beats (any of `LAUNCH_QUANTA`; anything else is
    /// refused). Pending starts and stops keep the boundary they were given.
    pub fn set_loop_quantum(&mut self, beats: i64) {
        if let Some(q) = valid_loop_quantum(beats) {
            self.loops.quantum = q;
        }
    }

    /// Clip `id`'s own launch boundary in beats, if it has one (None: it follows the row's).
    pub fn clip_quantum(&self, id: &str) -> Option<i64> {
        self.loops.row.settings.quantum(id)
    }

    /// What clip `id` starts and stops on, in beats: its own boundary, else the row's. Scenes
    /// switch on the row's whatever their clips say (`launchScene`).
    pub fn quantum_for(&self, id: Option<&str>) -> i64 {
        self.loops.row.settings.quantum_for(self.loops.quantum, id)
    }

    /// Give clip `id` a launch boundary of its own (any of `LAUNCH_QUANTA`; anything else is
    /// refused); None: it follows the row's again. Pending starts and stops keep theirs.
    pub fn set_clip_quantum(&mut self, id: &str, beats: Option<i64>) {
        if valid_clip_quantum(beats).is_some() {
            self.loops.row.settings.set_quantum(id, beats);
        }
    }

    /// How many bars a quick tap of a controller's LOOP records (1...64; None: none, a tap is a
    /// short hold). Anything else is refused.
    pub fn set_tap_take_bars(&mut self, bars: Option<i64>) {
        if let Some(b) = valid_tap_take_bars(bars) {
            self.loops.tap_take_bars = b;
        }
    }

    // MARK: tempo follow

    /// Whose tempo the loops follow (`loopFollow`).
    pub fn loop_follow(&self) -> LoopFollow {
        self.loops.follow
    }

    /// Follow the louder deck, or each loop's own. Clips with a follow of their own keep it.
    pub fn set_loop_follow(&mut self, mode: LoopFollow) {
        self.loops.follow = mode;
        if mode == LoopFollow::Louder {
            self.update_dominant_deck();
        }
    }

    /// Clip `id`'s own tempo follow, if it has one (None: it follows the row's).
    pub fn clip_follow(&self, id: &str) -> Option<LoopFollow> {
        self.loops.row.settings.follow(id)
    }

    /// Whose tempo clip `id` follows: its own choice, else the row's (`follow(for:)`).
    pub fn follow_for(&self, id: Option<&str>) -> LoopFollow {
        self.loops.row.settings.follow_for(self.loops.follow, id)
    }

    /// Give clip `id` a tempo follow of its own; None: it follows the row's again.
    pub fn set_clip_follow(&mut self, id: &str, mode: Option<LoopFollow>) {
        self.loops.row.settings.set_follow(id, mode);
        if mode == Some(LoopFollow::Louder) {
            self.update_dominant_deck();
        }
    }

    /// Whether anything on the row follows the louder deck: the row, or a clip of its own. Only
    /// then is the dominant deck worth keeping up to date.
    pub(crate) fn any_loop_follows_louder(&self) -> bool {
        self.loops.row.settings.any_follows_louder(self.loops.follow)
    }

    // MARK: the row's settings

    /// The row-wide settings, as the actions that set them: what a session recording has to say
    /// first when they are not the defaults (`loopSettings`), since a replay starts from a
    /// fresh engine.
    pub fn loop_settings(&self) -> Vec<DeckAction> {
        let mut v = vec![
            DeckAction::LoopQuantize {
                beats: self.loops.quantum,
            },
            DeckAction::LoopFade {
                mode: self.loop_fade().raw().to_string(),
            },
            DeckAction::LoopKeyLock {
                on: self.loop_key_lock(),
            },
            DeckAction::TapBars {
                bars: self.loops.tap_take_bars,
            },
            DeckAction::LoopFollow {
                mode: self.loops.follow.raw().to_string(),
            },
        ];
        if self.loop_fx_link() {
            v.push(DeckAction::LoopFxLink { on: true });
        }
        v
    }

    /// The row-wide settings back to a fresh engine's (`defaultLoopSettings`, FX link off).
    pub fn reset_loop_settings(&mut self) {
        for a in crate::journal::replay::default_loop_settings() {
            self.apply_loop_action(&a, None);
        }
        self.set_loop_fx_link(false);
    }

    // MARK: countdown

    /// The engine frame a waiting clip switches at: comes in (`Starting`) or goes out
    /// (`Stopping`). None when it is not waiting (`loopSwitchFrame`).
    pub fn loop_switch_frame(&self, id: &str) -> Option<i64> {
        use crate::deck::loops::LoopState;
        let v = self.voice_of(id)?;
        match self.loop_state(id) {
            LoopState::Starting => self.loops.voices[v].start_frame,
            LoopState::Stopping => self.loops.voices[v].stop_frame,
            _ => None,
        }
    }

    /// How long a waiting clip has to go, in beats of the clock it lines up with (the clip's own
    /// tempo with none), and how long it was given in all (the countdown's full turn)
    /// (`loopCountdown`).
    pub fn loop_countdown(&self, id: &str) -> Option<(f64, f64)> {
        let at = self.loop_switch_frame(id)?;
        let c = self.clip(id)?;
        let v = self.voice_of(id)?;
        let clock = self.loop_clock(Some(id), None);
        Some(loop_countdown(
            at,
            self.loop_frames(),
            self.loops.voices[v].scheduled_at,
            clock.frames_per_beat,
            c.bpm,
            self.sample_rate,
        ))
    }

    // MARK: scenes

    pub fn scene(&self, name: &str) -> Option<&LoopScene> {
        self.loops.row.scenes.scene(name)
    }

    /// The scenes, in order.
    pub fn scenes(&self) -> &[LoopScene] {
        &self.loops.row.scenes.list
    }

    /// The name a new scene gets: "Scene n", the first n not taken.
    pub fn next_scene_name(&self) -> String {
        self.loops.row.scenes.next_name()
    }

    /// Save the loops playing now (and those on their way in), in row order, with their levels
    /// and FX (those that have any on), as scene `name`: a new one on the end, or the one
    /// already called that, overwritten.
    pub fn save_scene(&mut self, name: &str) -> Option<LoopScene> {
        let looping = self.looping_clips();
        self.loops.row.save_scene(name, &looping)
    }

    /// Launch scene `name` (`launchScene(_:)`). Returns the boundary it switches on; an
    /// unknown scene does nothing.
    pub fn launch_scene(&mut self, name: &str) -> Option<i64> {
        let clips = self.scene(name)?.clips.clone();
        Some(self.launch_scene_clips(&clips))
    }

    /// Bring exactly `wanted` in at their levels (and FX, those that carry them) and take every
    /// other loop out, all at one frame: the next launch boundary (the row's, not any clip's
    /// own) of the clock the first of them lines up with. A clip already in stays in (its level
    /// and FX change on the boundary); clips no longer on the row are left out. Returns the
    /// boundary (`launchScene(clips:)`).
    pub fn launch_scene_clips(&mut self, wanted: &[SceneClip]) -> i64 {
        let looping = self.looping_clips();
        let lead = wanted
            .iter()
            .find(|c| self.clip(&c.clip_id).is_some())
            .map(|c| c.clip_id.clone())
            .or_else(|| looping.first().filter(|i| self.clip(i).is_some()).cloned());
        let at = self
            .loop_clock(lead.as_deref(), None)
            .next_boundary(self.loop_frames() + LOOP_LEAD, self.loops.quantum);
        let ops = plan_scene_launch(
            wanted,
            &|id| self.clip(id).is_some(),
            &looping,
            &|id| self.loop_state(id),
        );
        for op in ops {
            match op {
                SceneOp::Stop { id } => self.stop_loop(&id, false, Some(at)),
                SceneOp::SetGain {
                    id,
                    gain,
                    at_boundary,
                } => self.set_clip_gain(&id, gain, at_boundary.then_some(at)),
                SceneOp::SetFx {
                    id,
                    fx,
                    at_boundary,
                } => self.set_clip_fx(&id, &fx, at_boundary.then_some(at)),
                SceneOp::Start { id } => self.start_loop_with(
                    &id,
                    StartOpts {
                        at: Some(at),
                        ..StartOpts::default()
                    },
                ),
            }
        }
        at
    }

    /// Rename scene `name` to `to` (refused if `to` is empty or taken).
    pub fn rename_scene(&mut self, name: &str, to: &str) {
        self.loops.row.scenes.rename(name, to);
    }

    pub fn delete_scene(&mut self, name: &str) {
        self.loops.row.scenes.delete(name);
    }

    /// Every scene gone (a reset).
    pub fn clear_scenes(&mut self) {
        self.loops.row.scenes.clear();
    }

    /// Put saved scenes back (after any already there; a name already taken is skipped).
    pub fn restore_scenes(&mut self, restored: &[LoopScene]) {
        self.loops.row.scenes.restore(restored);
    }
}

// MARK: a Hit's loop (`StingerLaunch.swift`)

impl Engine {
    /// `DJEngine.fireStingerLoop`: launch a Hit's loop on the loop row, from its top, on the next
    /// launch boundary of `deck`'s time, for one pass. False when it can't play (the Hit names
    /// no loop, or a clip that is not on the row): the deck then plays its stinger net.
    ///
    /// A Hit's loop is a library clip (`loop.clipID`) or a stock loop (`loop.path`, an Apple
    /// Loop). The stock loop is n/a off the Mac: Swift decodes the file into `stingerClips` and
    /// launches it from there; a deterministic engine whose decode fails (no such file) answers
    /// false, which is what this answers for it, so the Hit plays its net (a live Swift engine
    /// decodes on a worker and answers true at once). `DJEngine.clip(_:)`'s fallback to
    /// `stingerClips` has nothing to fall back to, and `loopingClips`' exclusion of them nothing
    /// to exclude. The engine always has its loop bank (`loopChannel.bank != nil`).
    pub(crate) fn fire_stinger_loop(&mut self, a: &StingerAssignment, deck: &str) -> bool {
        let Some(id) = a.loop_.as_ref().and_then(|l| l.clip_id.as_deref()) else {
            return false;
        };
        if self.clip(id).is_none() {
            return false;
        }
        self.launch_stinger_clip(id, deck);
        true
    }

    /// `DJEngine.launchStingerClip`: start a stinger clip from its top and have it go out after
    /// one pass (its stop is scheduled by [`service_stinger_stops`](Self::service_stinger_stops)
    /// once its voice is in). One already sounding (or on its way in or out) is left as it is.
    fn launch_stinger_clip(&mut self, id: &str, deck: &str) {
        let Some(beats) = self.clip(id).map(|c| c.beats) else {
            return;
        };
        if self.voice_of(id).is_some() {
            return;
        }
        self.start_loop_with(
            id,
            StartOpts {
                from_top: true,
                prefer: Some(deck),
                ..StartOpts::default()
            },
        );
        let Some(v) = self.voice_of(id) else { return };
        let Some(start) = self.loops.voices[v].start_frame else {
            return;
        };
        let target = self.loops.voices[v].target_bpm;
        let stop = stinger_stop_frame(start, beats, target, self.sample_rate);
        self.loops.stinger_stops.insert(id.to_string(), stop);
    }

    /// The launched stinger loops that wait for their voice to come in, with the frame each is
    /// to go out at (`stingerStops`; tests, measurement).
    pub fn stinger_stops(&self) -> &std::collections::BTreeMap<String, i64> {
        &self.loops.stinger_stops
    }

    /// `DJEngine.serviceStingerStops`: schedule each launched stinger loop's stop once its voice
    /// is in (a stop sent before then would cancel the start), and forget the ones that are out.
    /// `service_voices` calls it first, as Swift's `serviceLoops` does.
    pub(crate) fn service_stinger_stops(&mut self) {
        if self.loops.stinger_stops.is_empty() {
            return;
        }
        let mut stops = std::mem::take(&mut self.loops.stinger_stops);
        let schedule = service_stinger_stops(&mut stops, &|id| {
            let v = self.voice_of(id)?;
            Some(StopVoice {
                active: self.loops.mirror.is_active(v),
                stop_scheduled: self.loops.voices[v].stop_frame.is_some(),
            })
        });
        self.loops.stinger_stops = stops;
        for (id, frame) in schedule {
            self.stop_loop(&id, false, Some(frame));
        }
    }
}
