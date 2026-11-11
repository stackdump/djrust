//! CUE, the hot cues and the loop buttons, on a sample deck and on a net deck
//! (`DJEngine.swift`'s `Deck.cue`, `hotCue`, `clearCue`, `loopIn`, `loopOut`, `loopHalve`,
//! `loopDouble`; `DeckBeat.swift`'s `loopBeats`).
//!
//! A sample deck's cue points are positions on the platter, in samples, **unwrapped** (the
//! turntable's own position, which runs past the end of the audio as it loops, and which a
//! loop region is also in), snapped to the beat grid when the deck's quantize says so. A net
//! deck's are ticks on the net clock: setting one reads the tick, jumping to one relocates the
//! net there ([`Engine::relocate`]). The loop buttons do nothing on a net deck.

use crate::deck::beat::loop_beats;
use crate::deck::turntable::TurntableCommand;

use super::engine::Engine;

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

    /// LOOP IN: mark the loop start at the playhead (snapped), leaving any active loop
    /// (`Deck.loopIn`).
    pub fn loop_in(&mut self, deck: &str) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
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

    /// `Deck.loopHalve`: the active loop to its first half (not under 64 samples).
    pub fn loop_halve(&mut self, deck: &str) {
        if let Some((s, e)) = self.active_loop(deck) {
            if e - s > 64.0 {
                self.set_loop(deck, s, s + (e - s) / 2.0);
            }
        }
    }

    /// `Deck.loopDouble`.
    pub fn loop_double(&mut self, deck: &str) {
        if let Some((s, e)) = self.active_loop(deck) {
            self.set_loop(deck, s, s + (e - s) * 2.0);
        }
    }

    /// A loop `beats` beats long, from the beat (or bar, quantizing to bars) at or before the
    /// playhead (`Deck.loopBeats`; the window's auto-loop). Nothing without a grid.
    pub fn loop_beats(&mut self, deck: &str, beats: f64) {
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else { return };
        let Some(g) = d.sample.as_ref().and_then(|s| s.grid) else {
            return;
        };
        let (start, end) = loop_beats(beats, d.mirror.turntable_position(), d.quantize, &g, sr);
        let name = d.name.clone();
        self.set_loop(deck, start, end);
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

    pub fn exit_loop(&mut self, deck: &str) {
        if let Some(s) = self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            s.looping = false;
        }
        self.turntable(deck, TurntableCommand::LoopOff);
    }
}
