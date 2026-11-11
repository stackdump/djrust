//! The beat clock on a deck, `DeckBeat.swift`'s engine half: the tempo before and after the
//! fader, where the deck is on its beat grid, `sync` and `follow`, the quantize setting, the
//! sample deck's grid, and the housekeeping the clock needs (`Deck.service`'s share).
//!
//! The rules are `crate::deck::beat`'s (`tempo_match`, `sync_to`, `snapped`, `loop_beats`);
//! this file holds the state they read and the deck calls they end in.
//!
//! Two inputs set a deck's tempo, as in Swift: the tempo fader ([`MainDeck::pitch`], -1...1 x
//! 8 %, which stays through a new track) and the momentary nudge ([`MainDeck::net_nudge`], the
//! jog ring and the joystick). A net deck's clock runs at `base x (1 + pitch x 0.08) x
//! (1 + net_nudge)`, set again behind every load (`applyNetTempo`).
//!
//! Beyond Swift (docs/HUB.md M1): the fader's travel is the deck's [`MainDeck::pitch_range`],
//! 8 % unless the mixer widens it (16 %, 50 %); at 8 % every formula here is Swift's.

use crate::deck::beat::{
    effective_bpm_in, snapped, sync_to_in, tempo_match_in, BeatState, Quantize, SyncAlign,
    SyncResult,
};
use crate::deck::beat_grid::BeatGrid;
use crate::deck::turntable::TurntableCommand;

use super::core::Command;
use super::engine::{Engine, MainDeck};

impl MainDeck {
    /// `Deck.mode == .net` with a project on it. A deck with nothing loaded is a sample deck in
    /// Swift (its `mode` starts at `.sample`), so none of the net calls act on it.
    pub fn is_net(&self) -> bool {
        self.sample.is_none() && self.project.is_some()
    }

    /// `Deck.baseBPM`: the tempo before the fader: the project's own on a net, the grid's on a
    /// sample deck.
    pub fn base_bpm(&self) -> Option<f64> {
        if let Some(s) = &self.sample {
            return s.grid.map(|g| g.bpm);
        }
        self.project.as_ref().map(|_| self.base_tempo)
    }

    /// `Deck.effectiveBPM`: base x tempo fader x nudge.
    pub fn effective_bpm(&self) -> Option<f64> {
        self.base_bpm()
            .map(|b| effective_bpm_in(b, self.pitch, self.net_nudge, self.pitch_range))
    }

    /// `Deck.beatState`: where the deck is on its beat grid. A net deck reads the net clock
    /// (the tick and how far into it), a sample deck the platter against its grid. The tempo in
    /// it is the effective one, not the turntable's instantaneous rate.
    pub fn beat_state(&self, sample_rate: f64) -> Option<BeatState> {
        let bpm = self.effective_bpm()?;
        if let Some(s) = &self.sample {
            let g = s.grid?;
            let length = self
                .mirror
                .tt_length
                .load(std::sync::atomic::Ordering::Relaxed);
            if length == 0 {
                return None;
            }
            return Some(BeatState::of_sample(
                bpm,
                &g,
                self.mirror.turntable_position(),
                length as f64,
                sample_rate,
            ));
        }
        Some(BeatState::of_net(
            bpm,
            self.mirror.tick(),
            self.mirror.tick_phase(),
        ))
    }

    /// `Deck.snapped`: a sample position on this deck's grid, as its quantize setting says.
    pub fn snapped(&self, position: f64, sample_rate: f64) -> f64 {
        let grid = self.sample.as_ref().and_then(|s| s.grid);
        snapped(position, self.quantize, grid.as_ref(), sample_rate)
    }
}

impl Engine {
    /// `Deck.applyNetTempo`: the net clock at project BPM x tempo fader x nudge. Nothing on a
    /// deck that is not playing a net. The deck's `nudge` (the whole ratio) follows either way.
    pub(crate) fn apply_net_tempo(&mut self, deck: &str) {
        let Some(d) = self.deck_mut(deck) else { return };
        d.nudge = (1.0 + d.pitch * d.pitch_range) * (1.0 + d.net_nudge) - 1.0;
        if !d.is_net() {
            return;
        }
        // half / double time (docs/TRICKS.md): the clock runs at a multiple of the deck's tempo;
        // the deck's own tempo (`effective_bpm`, sync, the master's) stays what it is
        let bpm = effective_bpm_in(d.base_tempo, d.pitch, d.net_nudge, d.pitch_range)
            * d.tricks.time_scale();
        self.command(deck, Command::Tempo(bpm));
    }

    /// The tempo fader, -1...1 (`Deck.pitch`). The platter takes it at once, a net's clock too.
    pub fn set_pitch(&mut self, deck: &str, pitch: f64) -> bool {
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        d.pitch = pitch;
        let range = d.pitch_range;
        let scale = d.tricks.time_scale();
        let ok = self.turntable(deck, TurntableCommand::Pitch((1.0 + pitch * range) * scale - 1.0));
        self.apply_net_tempo(deck);
        ok
    }

    /// `Deck.baseBPM`.
    pub fn base_bpm(&self, deck: &str) -> Option<f64> {
        self.deck(deck)?.base_bpm()
    }

    /// `Deck.effectiveBPM`.
    pub fn effective_bpm(&self, deck: &str) -> Option<f64> {
        self.deck(deck)?.effective_bpm()
    }

    /// The other deck `sync` lines up with: the first by name that is not `deck`
    /// (`dj.decks.values.sorted { $0.name < $1.name }.first { $0 !== deck }`).
    pub fn sync_partner(&self, deck: &str) -> Option<String> {
        let mut names: Vec<&str> = self.decks.iter().map(|d| d.name.as_str()).collect();
        names.sort_unstable();
        names.into_iter().find(|n| *n != deck).map(String::from)
    }

    /// `Deck.sync(to:align:)`: match `deck`'s tempo (and optionally its beat or bar phase) to
    /// `other`'s. None when `deck` has no base tempo, or `other` has no tempo or no beat state
    /// yet (a sample deck whose first block has not rendered).
    pub fn sync(&mut self, deck: &str, other: &str, align: SyncAlign) -> Option<SyncResult> {
        let sr = self.sample_rate;
        let me = self.deck(deck)?;
        let you = self.deck(other)?;
        let base = me.base_bpm()?;
        let target = you.effective_bpm()?;
        let their = you.beat_state(sr)?;
        // the beat positions do not depend on the fader, so `me` read before the change is the
        // one Swift reads after it
        let mine = me.beat_state(sr);
        let r = sync_to_in(base, target, mine, Some(their), align, me.pitch_range);
        // pitch = (ratio - 1) / range; netNudge = 0; applyNetTempo
        self.set_pitch(deck, r.pitch);
        if let Some(d) = self.deck_mut(deck) {
            d.net_nudge = 0.0;
        }
        self.apply_net_tempo(deck);
        if align != SyncAlign::TempoOnly && mine.is_some() {
            self.move_beats(deck, r.shifted_beats);
        }
        Some(r)
    }

    /// A controller's SYNC button: `deck` onto the other deck, by the bar or the beat.
    pub fn sync_to_partner(&mut self, deck: &str, bar: bool) -> Option<SyncResult> {
        let other = self.sync_partner(deck)?;
        self.sync(
            deck,
            &other,
            if bar { SyncAlign::Bar } else { SyncAlign::Beat },
        )
    }

    /// `Deck.move(beats:)`: along the beat grid (positive is forward in the music): a net's
    /// clock is aligned by that many beats, a platter seeks.
    pub fn move_beats(&mut self, deck: &str, beats: f64) -> bool {
        if beats == 0.0 {
            return false;
        }
        let sr = self.sample_rate;
        let Some(d) = self.deck(deck) else {
            return false;
        };
        let Some(s) = &d.sample else {
            return d.is_net() && self.command(deck, Command::Align(beats * 4.0));
        };
        let Some(g) = s.grid else { return false };
        let at = d.mirror.turntable_position() + beats * g.samples_per_beat(sr);
        self.turntable(deck, TurntableCommand::Seek(at))
    }

    /// `Deck.follow`: keep `deck`'s tempo locked to `leader`'s (tempo only: the phase stays
    /// where it is) and re-match it as the leader moves ([`service_beat`](Self::service_beat)).
    /// `None` lets go; the deck stays at the last tempo.
    pub fn follow(&mut self, deck: &str, leader: Option<&str>) -> bool {
        if self.index(deck).is_none() {
            return false;
        }
        // `decks[leader]`: a name that is no deck is nil, which lets go
        let leader = leader.filter(|l| self.index(l).is_some());
        if let Some(d) = self.deck_mut(deck) {
            d.leader = leader.map(String::from);
        }
        if let Some(l) = leader {
            self.sync(deck, l, SyncAlign::TempoOnly);
        }
        true
    }

    /// `Deck.rewind`: to the start of the sample, or tick 0 of the net (a loop too).
    pub fn rewind(&mut self, deck: &str) -> bool {
        let Some(d) = self.deck(deck) else {
            return false;
        };
        if d.sample.is_some() {
            return self.turntable(deck, TurntableCommand::Seek(0.0));
        }
        self.relocate(deck, 0, None, None)
    }

    // MARK: quantize

    /// `Deck.quantize = q`: cue points and loops snap to the grid of a sample deck.
    pub fn set_quantize(&mut self, deck: &str, q: Quantize) -> bool {
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        d.quantize = q;
        true
    }

    /// A controller's QUANTIZE button: off, beat, bar, off...
    pub fn cycle_quantize(&mut self, deck: &str) -> bool {
        let Some(q) = self.deck(deck).map(|d| d.quantize) else {
            return false;
        };
        let next = match q {
            Quantize::Off => Quantize::Beat,
            Quantize::Beat => Quantize::Bar,
            Quantize::Bar => Quantize::Off,
        };
        self.set_quantize(deck, next)
    }

    // MARK: the sample deck's grid

    /// `Deck.setBeatGrid`: put a grid in place directly (a session replay, a stored analysis).
    /// A sample deck only.
    pub fn set_beat_grid(&mut self, deck: &str, grid: BeatGrid) -> bool {
        match self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            Some(s) => {
                s.grid = Some(grid);
                true
            }
            None => false,
        }
    }

    /// `Deck.setBPM`: declare the tempo by hand (or after a tap), keeping the grid's offset. The
    /// new grid is journaled and recorded as a `grid` action (`Deck.onGrid`).
    pub fn set_bpm(&mut self, deck: &str, bpm: f64) -> bool {
        let grid = match self.deck_mut(deck).and_then(|d| d.sample.as_mut()) {
            Some(s) if bpm > 0.0 => {
                let g = BeatGrid::new(bpm, s.grid.map_or(0.0, |g| g.offset), 1.0);
                s.grid = Some(g);
                g
            }
            _ => return false,
        };
        self.record_grid(deck, grid);
        true
    }

    /// `DJEngine.init`'s `deck.onGrid`: a grid the deck changed by hand lands in the session
    /// recorder and the journal as a `grid` action.
    fn record_grid(&mut self, deck: &str, g: BeatGrid) {
        self.record_action(crate::session::action::DeckAction::Grid {
            deck: deck.to_string(),
            bpm: g.bpm,
            offset: g.offset,
        });
    }

    /// `Deck.setDownbeatHere`: the playhead is on a downbeat; the grid's origin moves here.
    pub fn set_downbeat_here(&mut self, deck: &str) -> bool {
        let sr = self.sample_rate;
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        let length = d
            .mirror
            .tt_length
            .load(std::sync::atomic::Ordering::Relaxed) as f64;
        let position = d.mirror.turntable_position();
        let Some(s) = d.sample.as_mut() else {
            return false;
        };
        let Some(g) = s.grid else { return false };
        if length <= 0.0 {
            return false;
        }
        let spb = g.samples_per_beat(sr);
        let mut p = position % length;
        if p < 0.0 {
            p += length;
        }
        let moved = BeatGrid::new(g.bpm, p % spb, 1.0);
        s.grid = Some(moved);
        self.record_grid(deck, moved);
        true
    }

    // MARK: housekeeping

    /// The beat clock's share of `Deck.service` (the hub calls it about every 10 ms; `Rig::tick`
    /// does): a deck that follows a leader re-matches its tempo when the leader's has moved, and
    /// a net whose song has ended is no longer playing.
    ///
    /// Not here: the rest of `service` (auto-advance, keeping the next track composed, landing
    /// the composes that finished) is [`Engine::service`](Self::service), the track pipeline's,
    /// which does this in Swift's order and is what the rig calls.
    pub fn service_beat(&mut self) {
        for i in 0..self.decks.len() {
            self.follow_leader(i);
            let d = &self.decks[i];
            // `playing && !wave.isPlaying && wave.hasEnded`
            if d.is_net()
                && d.playing
                && !d.mirror.is_playing()
                && d.mirror.stopped.load(std::sync::atomic::Ordering::Relaxed)
            {
                self.decks[i].playing = false;
            }
        }
    }

    /// `Deck.service`'s leader tracking: re-match the tempo to the deck being followed when it
    /// has moved (the same multiplier rule as sync).
    pub(crate) fn follow_leader(&mut self, i: usize) {
        let d = &self.decks[i];
        let name = d.name.clone();
        if let Some(leader) = d.leader.clone() {
            let target = self.deck(&leader).and_then(MainDeck::effective_bpm);
            if let (Some(target), Some(base)) = (target, self.decks[i].base_bpm()) {
                let range = self.decks[i].pitch_range;
                let p = (tempo_match_in(base, target, range).ratio - 1.0) / range;
                if (p - self.decks[i].pitch).abs() > 1e-6 {
                    self.set_pitch(&name, p);
                }
            }
        }
    }
}
