//! Moving a net deck's clock: `Deck.relocate(toTick:tail:rest:)` and what a controller does
//! with it (`launchSection`, `stepSection`, the CUE and hot cue jumps, REWIND)
//! (`DeckPerform.swift`).
//!
//! A relocation compiles the *same* project again from the tick (the control nets replayed
//! silently to there) and hands it to the runner, which swaps it in at the next bar of a deck
//! that plays, at once on one that does not. Nothing is composed: it is the song, from
//! somewhere else. The deck keeps its cues, its tempo fader and its roles; the swap takes
//! the song's own tempo with it, so the tempo is set again behind it ([`Engine::hand_over`]).
//!
//! What a relocation prepares with is `Deck.sameSongOptions` (`crate::deck::swap`): the genre's
//! settings, the loudness, energy and duck the song was measured with, the tail it already plays
//! and its song serial, plus the mix-out tail and the extra rest the caller brings (`movedOptions`).
//! Inline in `deterministic` mode, else on a worker thread ([`MoveRequest`]), landing in
//! [`Engine::service`] unless a newer request superseded it.
//!
//! `mixOut` and `mixIn` are relocations with a tail or a rest ([`Engine::mix_out`],
//! [`Engine::mix_in`]).

use crate::deck::perform::{
    is_mixing_in, launch_section_tick, mix_in as perform_mix_in, mix_out as perform_mix_out,
    mix_out_point, step_section_tick,
};
use crate::wave::rests::Entry;
use crate::wave::MixOutTail;

use super::engine::Engine;

impl Engine {
    /// `Deck.relocate(toTick:tail:rest:)`: move the net to a tick (the swap lands on the next
    /// bar). `tail` and `rest`: the project from there on plays that mix-out tail, or that
    /// extra rest on top of the song's (`mixOut`, `mixIn`). Prepared on the calling thread in
    /// `deterministic` mode (the default here), else on a worker (see
    /// [`move_request`](Self::move_request)); false on a deck that is not playing a net.
    pub fn relocate(
        &mut self,
        deck: &str,
        tick: i64,
        tail: Option<MixOutTail>,
        rest: Option<Entry>,
    ) -> bool {
        let Some(req) = self.move_request(deck, tick, tail, rest) else {
            return false;
        };
        if self.deterministic {
            let p = req.prepare();
            self.load_move(deck, p)
        } else {
            self.begin_move(deck, req)
        }
    }

    // MARK: mix out / mix in

    /// `Deck.mixOutPoint(atBar:bars:)`: where `mix_out` performed in bar `h - 1` of a playing
    /// deck starts (the tick the relocation prepares from) and the tail it plays: the outro if
    /// it is still ahead, else the bar line `h`. What auto-mix prepares a phrase early.
    pub fn mix_out_point(&self, deck: &str, h: i64, bars: i64) -> Option<(i64, MixOutTail)> {
        let d = self.deck(deck)?;
        Some(mix_out_point(&d.sections, h, bars))
    }

    /// `Deck.mixOut(bars:)` (`DeckAction.mixOut`): at the next bar the deck jumps to the
    /// song's outro (or, already in it or with none, plays on from where it is) and from that
    /// bar line plays only its drums-forward lanes for `bars` bars, then stops on a bar line
    /// (`MixOutTail`; the room rings out). Section jumps, kit and palette changes keep the
    /// tail (`sameSongOptions`); a new track drops it. The relocation is part of the action,
    /// never recorded on its own. Returns the tail, None on a deck not playing a net.
    pub fn mix_out(&mut self, deck: &str, bars: i64) -> Option<MixOutTail> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        let (tick, tail) =
            perform_mix_out(&d.sections, d.mirror.is_playing(), d.mirror.tick(), bars);
        self.relocate(deck, tick, Some(tail), None);
        Some(tail)
    }

    /// `Deck.mixIn(bars:)` (`DeckAction.mixIn`): bring the deck in drums-forward: from the next
    /// bar its melodic lanes (the mix-out tail's mask) rest for `bars` bars and come back on
    /// the bar line after, the song carrying on where it is. The rest joins the song's
    /// schedule (in song ticks: a jump back across that stretch rests it again). Returns it,
    /// None on a deck not playing a net.
    pub fn mix_in(&mut self, deck: &str, bars: i64) -> Option<Entry> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        let (tick, rest) = perform_mix_in(d.mirror.is_playing(), d.mirror.tick(), bars)?;
        self.relocate(deck, tick, None, Some(rest));
        self.deck_mut(deck)?.track.mix_in_end_tick = Some(rest.to);
        Some(rest)
    }

    /// `Deck.isMixingOut`: a mix-out tail plays on the sounding project (it stops on its bar
    /// line).
    pub fn is_mixing_out(&self, deck: &str) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.is_net() && d.mirror.tail_end_tick().is_some())
    }

    /// `Deck.isMixingIn`: a mix-in's rest is still to come or playing.
    pub fn is_mixing_in(&self, deck: &str) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.is_net() && is_mixing_in(d.track.mix_in_end_tick, d.mirror.tick()))
    }

    /// `Deck.launchSection`: the net to the start of section `index`.
    pub fn launch_section(&mut self, deck: &str, index: usize) -> bool {
        let Some(at) = self
            .deck(deck)
            .and_then(|d| launch_section_tick(&d.sections, index))
        else {
            return false;
        };
        self.relocate(deck, at, None, None)
    }

    /// `Deck.stepSection`: `delta` sections from the one the clock is in, clamped to the song.
    pub fn step_section(&mut self, deck: &str, delta: i64) -> bool {
        let Some(d) = self.deck(deck) else {
            return false;
        };
        let Some(at) = step_section_tick(&d.sections, d.mirror.tick(), delta) else {
            return false;
        };
        self.relocate(deck, at, None, None)
    }
}
