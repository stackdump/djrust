//! CUE, the hot cues, the beat jump and the loop buttons, on a sample deck and on a net deck
//! (`DJEngine.swift`'s `Deck.cue`, `hotCue`, `clearCue`, `loopIn`, `loopOut`, `loopHalve`,
//! `loopDouble`; `DeckBeat.swift`'s `loopBeats`).
//!
//! A sample deck's cue points are positions on the platter, in samples, **unwrapped** (the
//! turntable's own position, which runs past the end of the audio as it loops, and which a
//! loop region is also in), snapped to the beat grid when the deck's quantize says so. A net
//! deck's are ticks on the net clock: setting one reads the tick, jumping to one relocates the
//! net there ([`Engine::relocate`]).
//!
//! Beyond Swift, where they do nothing on a net deck: the loop buttons loop a net deck too. Its
//! loop is a window of song steps (a beat is 4) that the render thread's clock wraps in, going
//! back through the net's history at the window's end so every pass plays what the first did
//! ([`NetLoopOp`], `WaveRunner::net_loop_op`). The window snaps to the deck's quantize: LOOP IN
//! and OUT to the nearest step, beat or bar, a loop of N beats to the beat (or bar) at or
//! before the playhead, as on a sample deck. A loop leaves off where the playhead is: the deck
//! plays on through the window's end. Anything that hands the deck another project (a jump, a
//! cue, a new track, a sound change) ends the loop first. Also beyond Swift: the beat jump
//! ([`Engine::beat_jump`]).

use crate::deck::beat::{loop_beats, Quantize};
use crate::deck::turntable::TurntableCommand;
use crate::wave::{NetLoopOp, NET_LOOP_MAX};

use super::core::Command;
use super::engine::Engine;

/// The line a net deck's loop button snaps to, in steps, for the deck's quantize: `off` when it
/// is off.
fn net_line(q: Quantize, off: i64) -> i64 {
    match q {
        Quantize::Off => off,
        Quantize::Beat => 4,
        Quantize::Bar => 16,
    }
}

impl Engine {
    /// CUE (`Deck.cue`). A sample deck: while playing, back to the cue point and pause; while
    /// paused, set the cue point at the playhead (snapped). A net deck: playing, pause and
    /// relocate to the cue tick; paused, set the cue tick here.
    pub fn cue(&mut self, deck: &str) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
        if d.is_net() {
            if d.playing {
                let at = d.net_cue_tick as i64;
                self.set_playing(deck, false);
                self.relocate(deck, at, None, None);
            } else {
                let tick = d.mirror.tick() as f64;
                if let Some(d) = self.deck_mut(deck) {
                    d.net_cue_tick = tick;
                }
            }
            return;
        }
        let Some(s) = &d.sample else { return };
        if d.playing {
            let at = s.cue_point;
            self.set_playing(deck, false);
            self.turntable(deck, TurntableCommand::Seek(at));
        } else {
            let at = d.snapped(d.mirror.turntable_position(), sr);
            if let Some(s) = self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
                s.cue_point = at;
            }
        }
    }

    /// Jump to the cue in `slot`, or set one at the playhead if the slot is empty
    /// (`Deck.hotCue`). A net deck's cue is the tick; the jump replays the control nets to it
    /// and swaps at the next bar. A sample deck's jump, quantized and playing, keeps the beat
    /// phase: it lands as far past the cue as the playhead was past the grid line.
    pub fn hot_cue(&mut self, deck: &str, slot: usize) {
        let sr = self.sample_rate;
        let slot = slot as i64;
        let Some(d) = self.deck(deck) else { return };
        let existing = d.hot_cues.get(&slot).copied();
        if d.is_net() {
            if let Some(at) = existing {
                self.relocate(deck, at as i64, None, None);
            } else if d
                .mirror
                .has_project
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let tick = d.mirror.tick() as f64;
                self.set_hot_cue(deck, slot, tick);
            }
            return;
        }
        let Some(_) = &d.sample else { return };
        let position = d.mirror.turntable_position();
        if let Some(at) = existing {
            let held = if d.quantize != crate::deck::beat::Quantize::Off && d.playing {
                position - d.snapped(position, sr)
            } else {
                0.0
            };
            self.turntable(deck, TurntableCommand::Seek(at + held));
        } else {
            let at = d.snapped(position, sr);
            self.set_hot_cue(deck, slot, at);
        }
    }

    fn set_hot_cue(&mut self, deck: &str, slot: i64, at: f64) {
        if let Some(d) = self.deck_mut(deck) {
            d.hot_cues.insert(slot, at);
        }
    }

    /// `Deck.clearCue`.
    pub fn clear_cue(&mut self, deck: &str, slot: usize) {
        if let Some(d) = self.deck_mut(deck) {
            d.hot_cues.remove(&(slot as i64));
        }
    }

    // MARK: the beat jump (beyond Swift)

    /// Move the deck `beats` beats along its music (negative: back), keeping its place in the
    /// beat: a jump of whole beats lands as far into a beat as the deck was.
    ///
    /// A sample deck moves its platter by its grid's beat, from wherever the platter is when the
    /// move lands (a loop that holds moves with it); one with no grid is refused. A net deck
    /// relocates by `4 x beats` steps ([`Engine::relocate`], which ends a loop): at once when
    /// it is not playing; playing, on the next bar line, as every relocation lands, to the step
    /// the song would have played on that line moved by the beats. A net deck moves in whole
    /// steps (quarter beats), not before its song's top and not past its end.
    pub fn beat_jump(&mut self, deck: &str, beats: f64) -> Result<(), String> {
        if !beats.is_finite() {
            return Err(format!("beat jump: {beats} is no number of beats"));
        }
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else {
            return Err(format!("no deck {deck:?}"));
        };
        if d.is_net() {
            let steps = beats * 4.0;
            if steps.fract() != 0.0 {
                return Err(format!(
                    "deck {deck}: a net deck jumps in whole steps (quarter beats), not {beats} beats"
                ));
            }
            if steps == 0.0 {
                return Ok(());
            }
            let tick = d.mirror.tick();
            // counted from the step the deck plays next: stopped, the one it starts on; playing,
            // the next bar line's, which the relocation's swap replaces
            let from = if d.playing {
                (tick.div_euclid(16) + 1) * 16
            } else {
                tick
            };
            let to = (from + steps as i64).max(0);
            let length = d
                .project
                .as_ref()
                .map_or(0, crate::deck::swap::song_length);
            if length > 0 && to >= length {
                return Err(format!(
                    "deck {deck}: {beats} beats from here is past the end of the song"
                ));
            }
            return if self.relocate(deck, to, None, None) {
                Ok(())
            } else {
                Err(format!("deck {deck}: the net could not be moved"))
            };
        }
        let Some(s) = &d.sample else {
            return Err(format!("deck {deck}: nothing loaded to jump in"));
        };
        let Some(g) = s.grid else {
            return Err(format!("deck {deck}: no beat grid to jump by"));
        };
        let by = beats * g.samples_per_beat(sr);
        if let (true, Some(start), Some(end)) = (s.looping, s.loop_start, s.loop_end) {
            self.set_loop(deck, start + by, end + by);
        }
        self.turntable(deck, TurntableCommand::SeekBy(by));
        Ok(())
    }

    // MARK: loops

    /// Can the deck loop on its beat: a net deck, or a sample deck with a beat grid.
    pub fn can_loop(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| {
            d.is_net() || d.sample.as_ref().is_some_and(|s| s.grid.is_some())
        })
    }

    /// The window a net deck's loop holds, `(start, end)` in song steps (a beat is 4), as of the
    /// last block the render thread finished; None off a net deck or with no loop.
    pub fn net_loop(&self, deck: &str) -> Option<(i64, i64)> {
        self.deck(deck).filter(|d| d.is_net())?.mirror.net_loop()
    }

    fn net_loop_op(&mut self, deck: &str, op: NetLoopOp) {
        self.command(deck, Command::NetLoop(op));
    }

    /// LOOP IN: mark the loop start at the playhead (snapped), leaving any active loop
    /// (`Deck.loopIn`).
    pub fn loop_in(&mut self, deck: &str) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
        if d.is_net() {
            let unit = net_line(d.quantize, 1);
            return self.net_loop_op(deck, NetLoopOp::In { unit });
        }
        let Some(s) = &d.sample else { return };
        let looping = s.looping;
        let at = d.snapped(d.mirror.turntable_position(), sr);
        if looping {
            self.exit_loop(deck);
        }
        if let Some(s) = self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            s.loop_start = Some(at);
            s.loop_end = None;
        }
    }

    /// LOOP OUT: close the loop at the playhead (snapped) and start looping; while looping,
    /// exit it (`Deck.loopOut`).
    pub fn loop_out(&mut self, deck: &str) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
        if d.is_net() {
            let unit = net_line(d.quantize, 1);
            return self.net_loop_op(deck, NetLoopOp::Out { unit });
        }
        let Some(s) = &d.sample else { return };
        if s.looping {
            return self.exit_loop(deck);
        }
        let Some(start) = s.loop_start else { return };
        let end = d.snapped(d.mirror.turntable_position(), sr);
        if end > start + 1.0 {
            self.set_loop(deck, start, end);
        }
    }

    /// `Deck.loopHalve`: the active loop to its first half (not under 64 samples; on a net
    /// deck, not under a step).
    pub fn loop_halve(&mut self, deck: &str) {
        if self.is_net_deck(deck) {
            return self.net_loop_op(deck, NetLoopOp::Halve);
        }
        if let Some((s, e)) = self.active_loop(deck) {
            if e - s > 64.0 {
                self.set_loop(deck, s, s + (e - s) / 2.0);
            }
        }
    }

    /// `Deck.loopDouble` (on a net deck, not over 64 beats).
    pub fn loop_double(&mut self, deck: &str) {
        if self.is_net_deck(deck) {
            return self.net_loop_op(deck, NetLoopOp::Double);
        }
        if let Some((s, e)) = self.active_loop(deck) {
            self.set_loop(deck, s, s + (e - s) * 2.0);
        }
    }

    /// A loop `beats` beats long, from the beat (or bar, quantizing to bars) at or before the
    /// playhead (`Deck.loopBeats`; the window's auto-loop). Nothing on a sample deck without a
    /// grid; a net deck takes from a step (¼ beat) to 64 beats.
    pub fn loop_beats(&mut self, deck: &str, beats: f64) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
        let name = d.name.clone();
        if d.is_net() {
            let len = beats * 4.0;
            if !(len >= 1.0 && len <= NET_LOOP_MAX as f64) {
                return;
            }
            let unit = net_line(d.quantize, 4);
            self.net_loop_op(
                deck,
                NetLoopOp::Beats {
                    len: len.round() as i64,
                    unit,
                },
            );
        } else {
            let Some(g) = d.sample.as_ref().and_then(|s| s.grid) else {
                return;
            };
            let (start, end) =
                loop_beats(beats, d.mirror.turntable_position(), d.quantize, &g, sr);
            self.set_loop(deck, start, end);
        }
        self.note_live(
            format!("loop_beats:{name}"),
            format!("{name} auto-loop {beats} beats"),
            false,
            "loop_beats",
            super::vocoder::live_params([("deck", name.into()), ("beats", crate::json::JsonValue::Number(beats))]),
        );
    }

    fn active_loop(&self, deck: &str) -> Option<(f64, f64)> {
        let s = self.deck(deck)?.sample.as_ref()?;
        if !s.looping {
            return None;
        }
        Some((s.loop_start?, s.loop_end?))
    }

    fn set_loop(&mut self, deck: &str, start: f64, end: f64) {
        if let Some(s) = self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            s.loop_start = Some(start);
            s.loop_end = Some(end);
            s.looping = true;
        }
        self.turntable(deck, TurntableCommand::Loop { start, end });
    }

    /// Leave the loop: the deck plays on from where it is. On a net deck that is through the
    /// window's end into the rest of the song, as on a sample deck: nothing jumps, so nothing
    /// is out of time.
    pub fn exit_loop(&mut self, deck: &str) {
        if self.is_net_deck(deck) {
            return self.net_loop_op(deck, NetLoopOp::Exit);
        }
        if let Some(s) = self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            s.looping = false;
        }
        self.turntable(deck, TurntableCommand::LoopOff);
    }
}
