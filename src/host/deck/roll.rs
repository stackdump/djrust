//! The history as a source (docs/INPUT-HISTORY.md, "Roll, slip and loops on the grid"): a roll,
//! slip mode and a loop that ends on a given frame, on any strip with a history (an input, a
//! deck, `L`, `V`, `M`). None of them is an effect of its own. They are moves of the strip's
//! history read head ([`crate::history::Scrubber`]), so while one plays, the strip runs on
//! underneath, and going back to live rejoins it exactly where it would have been.
//!
//! - [`Engine::history_loop_ending_at`]: loop N beats of a strip's history ending at a render
//!   frame (the [`Engine::loop_frames`] base), entered at the phase the loop would have had
//!   had it looped since that frame. Its [`HistoryLoopId`] lets the asker end only its own loop
//!   ([`Engine::history_live_if`]). The booth's dead-air failsafe loops `M` with it.
//! - [`Engine::history_roll`]: while held, loop the last grid line's 1/N beat (a 1/32 to a
//!   1/4 note), on the grid at once; let go, back to live.
//! - [`Engine::set_history_slip`]: a released jog goes back to live instead of playing on as a
//!   delay of where it was left.

use std::collections::BTreeMap;

use crate::history::{self, LoopAsk};
use crate::json::JsonValue;

use super::engine::Engine;
use super::strip_recall::{RecallId, RecallMsg};

/// Who asked for a strip's loop, so they can end it and only it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryLoopId(pub u32);

/// Why a loop on a strip's history was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HistoryRefused {
    /// The strip keeps no history.
    NoHistory,
    /// The history does not hold that many beats before that frame (`held_beats` it does).
    NotHeld { held_beats: f64 },
    /// The render thread did not take the message (its queue is full).
    NotSent,
}

/// One strip's hand on its history.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Hand {
    /// The roll held now, if any.
    roll: Option<u32>,
    /// The length the next roll takes, beats (0: the default).
    roll_beats: f64,
    /// A released jog goes back to live.
    slip: bool,
}

/// The engine's rolls and slips, by strip, and the next loop id.
#[derive(Debug, Default)]
pub struct HistoryHands {
    next_id: u32,
    by_strip: BTreeMap<RecallId, Hand>,
}

impl HistoryHands {
    fn id(&mut self) -> u32 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }
}

impl Engine {
    fn hand(&self, i: RecallId) -> Hand {
        self.history_hands.by_strip.get(&i).copied().unwrap_or_default()
    }

    fn hand_mut(&mut self, i: RecallId) -> &mut Hand {
        self.history_hands.by_strip.entry(i).or_default()
    }

    /// Loop `beats` of strip `i`'s history ending at render frame `end_frame` (the base of
    /// [`Engine::loop_frames`] and [`Engine::clock_beat_now`]), entering it at the phase it
    /// would have had had it looped since `end_frame`: a loop ending on a bar line stays on the
    /// bar lines. An `end_frame` still ahead starts the loop on that frame. It fades in, and
    /// later out, over `fade_frames` (0: the 64-frame declick). Refused when the strip keeps no
    /// history or does not hold that many beats before `end_frame`.
    pub fn history_loop_ending_at(
        &mut self,
        i: impl Into<RecallId>,
        beats: f64,
        end_frame: i64,
        fade_frames: u32,
    ) -> Result<HistoryLoopId, HistoryRefused> {
        let i = i.into();
        if !self.has_history(i) {
            return Err(HistoryRefused::NoHistory);
        }
        self.sync_history_tempo();
        let (_, bpm) = self.master_tempo();
        let fpb = history::frames_per_beat(bpm, self.sample_rate);
        let filled = self.recall_view(i).map_or(0.0, |v| v.history_filled_beats() as f64);
        let back = (self.loop_frames() - end_frame).max(0) as f64 / fpb;
        let held = (filled - back).max(0.0);
        if beats.is_nan() || beats <= 0.0 || held + 1e-3 < beats {
            return Err(HistoryRefused::NotHeld { held_beats: held });
        }
        let id = self.history_hands.id();
        let ask = LoopAsk {
            beats,
            end: end_frame,
            fade: if fade_frames == 0 { history::FADE_FRAMES as f32 } else { fade_frames as f32 },
            id,
        };
        if !self.recall_send(i, RecallMsg::LoopAt(ask)) {
            return Err(HistoryRefused::NotSent);
        }
        Ok(HistoryLoopId(id))
    }

    /// Back to live over `fade_frames` (0: the declick), but only while loop `id` is still the
    /// one strip `i` plays (a roll or a loop asked later is left alone). True if it was sent;
    /// the render thread makes the check.
    pub fn history_live_if(&mut self, i: impl Into<RecallId>, id: HistoryLoopId, fade_frames: u32) -> bool {
        let i = i.into();
        let fade = if fade_frames == 0 { history::FADE_FRAMES as f32 } else { fade_frames as f32 };
        self.has_history(i) && self.recall_send(i, RecallMsg::LiveIf { id: id.0, fade })
    }

    /// The id of the loop strip `i` played at the last block (None: not looping, or a loop
    /// asked without one, such as LOOP IN).
    pub fn history_loop_id(&self, i: impl Into<RecallId>) -> Option<HistoryLoopId> {
        let id = self.recall_view(i.into())?.history_loop_id();
        (id != 0).then_some(HistoryLoopId(id))
    }

    /// The length a roll on strip `i` takes, in beats.
    pub fn history_roll_beats(&self, i: impl Into<RecallId>) -> f64 {
        let b = self.hand(i.into()).roll_beats;
        if b > 0.0 {
            b
        } else {
            history::DEFAULT_ROLL_BEATS
        }
    }

    /// Set the roll length (beats, held to 1/64 .. 4); a roll held now takes it at once.
    pub fn set_history_roll_beats(&mut self, i: impl Into<RecallId>, beats: f64) -> bool {
        let i = i.into();
        if !self.has_history(i) || beats.is_nan() || beats <= 0.0 {
            return false;
        }
        let b = beats.clamp(1.0 / 64.0, 4.0);
        self.hand_mut(i).roll_beats = b;
        if self.hand(i).roll.is_some() {
            return self.quietly(|e| e.history_roll(i, true, None));
        }
        true
    }

    /// Roll strip `i` (`on`: the pad went down) or let it go. A roll loops the last `beats`
    /// (None: the strip's roll length) that ended on a grid line of that length on the master
    /// clock, entered where the grid is now, so it is on the grid at once; with no clock it
    /// loops the last `beats`. Let go, the strip is back to live, where it would have been.
    /// A roll pressed again while held re-rolls (a new length).
    pub fn history_roll(&mut self, i: impl Into<RecallId>, on: bool, beats: Option<f64>) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        if !on {
            let Some(id) = self.hand_mut(i).roll.take() else {
                return true;
            };
            let ok = self.history_live_if(i, HistoryLoopId(id), 0);
            if ok {
                self.note_history(i, "roll", Some(("on", JsonValue::Bool(false))), "roll let go".into(), false);
            }
            return ok;
        }
        let b = beats.filter(|b| *b > 0.0).unwrap_or_else(|| self.history_roll_beats(i));
        let now = self.loop_frames();
        let back = self
            .clock_beat_now()
            .map_or(0.0, |(beat, fpb)| history::frames_since_grid(beat, b, fpb));
        let end = now - libm::round(back) as i64;
        match self.history_loop_ending_at(i, b, end, 0) {
            Ok(id) => {
                self.hand_mut(i).roll = Some(id.0);
                self.note_history(
                    i,
                    "roll",
                    Some(("beats", JsonValue::Number(b))),
                    format!("roll {} beat", fraction(b)),
                    false,
                );
                true
            }
            Err(_) => false,
        }
    }

    /// Is a roll held on strip `i`?
    pub fn history_rolling(&self, i: impl Into<RecallId>) -> bool {
        self.hand(i.into()).roll.is_some()
    }

    /// Slip mode on strip `i`: a released jog goes back to live (the strip played on
    /// underneath) instead of playing on as a delay of where it was left. Rolls and history
    /// loops always rejoin; this is the jog's half.
    pub fn set_history_slip(&mut self, i: impl Into<RecallId>, on: bool) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        if self.hand(i).slip != on {
            self.hand_mut(i).slip = on;
            self.note_history(
                i,
                "slip",
                Some(("on", JsonValue::Bool(on))),
                if on { "slip on".into() } else { "slip off".into() },
                false,
            );
        }
        true
    }

    /// Is slip mode on for strip `i`?
    pub fn history_slip(&self, i: impl Into<RecallId>) -> bool {
        self.hand(i.into()).slip
    }
}

/// A beat count as a person says it: 1/4, 1/2, 1, 2.
fn fraction(b: f64) -> String {
    if b >= 1.0 || b <= 0.0 {
        return format!("{b}");
    }
    let d = libm::round(1.0 / b);
    if (1.0 / d - b).abs() < 1e-9 {
        format!("1/{d}")
    } else {
        format!("{b}")
    }
}
