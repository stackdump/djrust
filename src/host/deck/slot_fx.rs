//! Effects on a strip with no deck of its own: an input strip (the phone's Wheel) or the master.
//!
//! The catalogue's audio tricks (a trick that only works the sound: stutter, pump, the filter,
//! delay, wash, reverb freeze, riser, tape stop, spin-back) play there through a
//! [`TrickChain`](super::tricks::TrickChain) of the slot's own, fired the way a deck's are
//! ([`Engine::engage`]): on the next bar, for 1 to 4 bars, latched, or (a transition) held.
//! Everything else in the catalogue works a deck's track (half / double time its tempo, the net
//! macros its parts, the master and aux macros its own chain and sends) and is refused here,
//! saying so.
//!
//! **The bar** is the lead deck's: the deck playing loudest through its fader (the same deck the
//! insert chains' tempo-synced delay follows). With no deck playing, an effect starts at once at
//! the master tempo.
//!
//! **Where they sit** (the render thread, [`super::engine::Renderer`]):
//!
//! - an input strip: pulled from its ring → **its tricks** → (the vocoder taps it here, before
//!   its fader) → its insert chain → its fader → the mix. As a deck: tricks before the vocoder's
//!   tap and the inserts.
//! - the master: the strips summed (with the vocoder's and the aux return) → **its tricks** →
//!   the master's insert chain → the post chain → the master level.

use super::engage::{Engaged, Engagement, FxJob, FxState, Job};
use super::engine::{Engine, Msg};
use super::tricks::{DeckTricks, TrickMsg};
use crate::fx::catalogue::{self, Entry, Family};
use crate::fx::tricks::{TrickKind, TrickSource};

/// A strip with no deck: an input strip by its index, or the master.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrickSlot {
    Input(usize),
    Master,
}

/// Can `kind` play on a slot: it works the sound only (not a deck's tempo, not gear over MIDI).
pub fn slot_kind(kind: TrickKind) -> bool {
    kind.time_scale().is_none() && !kind.is_gear()
}

/// Can the catalogue's `id` play on a slot (see [`slot_kind`]).
pub fn slot_entry(id: &str) -> bool {
    catalogue::find(id).is_some_and(|x| matches!(x.family, Family::Trick(k) if slot_kind(k)))
}

impl Engine {
    fn slot_check(&self, slot: TrickSlot) -> Result<(), String> {
        match slot {
            TrickSlot::Input(k) if k >= self.inputs.len() => Err(format!("no input strip {k}")),
            _ => Ok(()),
        }
    }

    fn slot_tricks(&mut self, slot: TrickSlot) -> &mut DeckTricks {
        self.slot_tricks.entry(slot).or_default()
    }

    /// The deck a slot's bar follows: the one playing loudest through its fader (volume and
    /// crossfader), None when none plays.
    pub fn slot_lead(&self) -> Option<String> {
        let xf = self.crossfader;
        self.decks
            .iter()
            .filter(|d| d.playing)
            .map(|d| (d.volume * super::engine::crossfade_gain(&d.name, xf), &d.name))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, n)| n.clone())
    }

    /// The render frame of the lead deck's next multiple of `q` beats (None: at once) and the
    /// frames per beat a slot's tricks run at.
    fn slot_clock(&self, q: f64) -> (Option<i64>, f64) {
        match self.slot_lead() {
            Some(d) => (self.deck_grid_frame(&d, q), self.deck_fpb(&d)),
            None => (None, 60.0 * self.sample_rate / self.master_tempo().1.max(1.0)),
        }
    }

    /// Fire `kind` on `slot`, on or off, at `at` (None: the next block).
    fn slot_trick(&mut self, slot: TrickSlot, kind: TrickKind, on: bool, at: Option<i64>) -> Result<(), String> {
        let (_, fpb) = self.slot_clock(4.0);
        let lead = self.slot_lead().and_then(|d| self.index(&d));
        let t = self.slot_tricks(slot);
        let mut args = t.on.get(&kind).copied().unwrap_or_else(|| kind.default_args());
        if kind == TrickKind::Pump {
            // auto: the lead deck's kick while it plays, else the grid
            args.source = match (args.source, lead) {
                (TrickSource::Auto, Some(k)) => TrickSource::Deck(k as u8),
                (TrickSource::Auto, None) => TrickSource::Grid,
                (s, _) => s,
            };
        }
        if on {
            t.on.insert(kind, args);
        } else {
            t.on.remove(&kind);
        }
        t.last_fpb = fpb;
        if !self.send_msg(Msg::SlotTrick(slot, TrickMsg::Fire { kind, on, at, args, fpb })) {
            return Err("the render queue is full".into());
        }
        Ok(())
    }

    fn slot_entry_check(&self, slot: TrickSlot, id: &str, e: Engagement) -> Result<(&'static Entry, TrickKind), String> {
        self.slot_check(slot)?;
        let entry = catalogue::find(id).ok_or_else(|| format!("no effect {id:?}"))?;
        let kind = match entry.family {
            Family::Trick(k) if slot_kind(k) => k,
            _ => return Err(format!("{} plays on a deck: it works the deck's track, not only its sound", entry.label)),
        };
        match e {
            Engagement::Bars(n) if !(1..=64).contains(&n) => return Err(format!("{n} bars: use 1 to 4")),
            Engagement::Bars(_) if entry.transition() => return Err(format!("{id} is a one-shot: momentary only")),
            Engagement::Latch if !entry.latchable() => {
                return Err(format!("{id} cannot latch ({})", entry.category.label()));
            }
            _ => {}
        }
        Ok((entry, kind))
    }

    /// Engage `id` on `slot`: on the lead deck's next bar, for `e` (see the module's notes).
    pub fn engage_slot(&mut self, slot: TrickSlot, id: &str, e: Engagement) -> Result<(), String> {
        let (_, kind) = self.slot_entry_check(slot, id, e)?;
        let (bar, fpb) = self.slot_clock(4.0);
        let now = self.frame();
        let start = bar.unwrap_or(now);
        let end = match e {
            Engagement::Bars(n) => Some(start + (n as f64 * 4.0 * fpb).round() as i64),
            _ => None,
        };
        self.slot_trick(slot, kind, true, bar)?;
        let t = self.slot_tricks(slot);
        t.jobs.retain(|j| j.id != id);
        if let Some(end) = end {
            t.jobs.push(FxJob { due: end, id: id.into(), what: Job::TrickOff(kind) });
        }
        if kind == TrickKind::Riser {
            let args = t.on.get(&kind).copied().unwrap_or_else(|| kind.default_args());
            t.riser_end = Some(start + (args.bars * 4.0 * fpb).round() as i64);
        }
        let latched = matches!(e, Engagement::Latch | Engagement::Momentary) && !catalogue::find(id).is_some_and(|x| x.transition());
        t.engaged.insert(id.to_string(), Engaged { latched, start, end });
        Ok(())
    }

    /// A held (or latched) effect let go at once: a pad lifted.
    pub fn release_slot(&mut self, slot: TrickSlot, id: &str) -> Result<(), String> {
        self.slot_check(slot)?;
        let Some(Family::Trick(kind)) = catalogue::find(id).map(|x| x.family) else {
            return Err(format!("no effect {id:?} here"));
        };
        let t = self.slot_tricks(slot);
        t.jobs.retain(|j| j.id != id);
        t.engaged.remove(id);
        self.slot_trick(slot, kind, false, None)
    }

    /// A latch let go on the lead deck's next bar: the pad pressed again.
    pub fn unlatch_slot(&mut self, slot: TrickSlot, id: &str) -> Result<(), String> {
        self.slot_check(slot)?;
        let Some(Family::Trick(kind)) = catalogue::find(id).map(|x| x.family) else {
            return Err(format!("no effect {id:?} here"));
        };
        let (bar, _) = self.slot_clock(4.0);
        let Some(bar) = bar else { return self.release_slot(slot, id) };
        self.slot_trick(slot, kind, false, Some(bar))?;
        let t = self.slot_tricks(slot);
        t.jobs.retain(|j| j.id != id);
        if let Some(g) = t.engaged.get_mut(id) {
            g.latched = false;
            g.end = Some(bar);
        }
        Ok(())
    }

    /// The filter's knob on `slot` under a hand: 0 low-pass … 0.5 open … 1 high-pass, at once.
    pub fn slot_knob(&mut self, slot: TrickSlot, v: f64) -> Result<(), String> {
        self.slot_check(slot)?;
        let pos = (v.clamp(0.0, 1.0) - 0.5) * 2.0;
        self.slot_tricks(slot).knob = pos;
        if !self.send_msg(Msg::SlotTrick(slot, TrickMsg::Knob(TrickKind::Filter, pos))) {
            return Err("the render queue is full".into());
        }
        Ok(())
    }

    /// PANIC on `slot`: everything engaged let go at once, what waited dropped.
    pub fn clear_slot_engagements(&mut self, slot: TrickSlot) {
        let kinds: Vec<TrickKind> = self.slot_tricks(slot).on.keys().copied().collect();
        for k in kinds {
            let _ = self.slot_trick(slot, k, false, None);
        }
        let t = self.slot_tricks(slot);
        t.engaged.clear();
        t.jobs.clear();
        t.riser_end = None;
        let _ = self.slot_knob(slot, 0.5);
    }

    /// What is engaged on `slot`, for the pads' lights.
    pub fn fx_state_slot(&self, slot: TrickSlot) -> Vec<FxState> {
        let now = self.frame();
        self.slot_tricks
            .get(&slot)
            .map(|t| {
                t.engaged
                    .iter()
                    .map(|(id, g)| FxState { id: id.clone(), latched: g.latched, pending: now < g.start })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The service tick's half for the slots: end what has run its bars, follow the lead's tempo.
    pub(crate) fn service_slot_engagements(&mut self) {
        if self.slot_tricks.values().all(|t| t.jobs.is_empty() && t.engaged.is_empty() && t.on.is_empty()) {
            return;
        }
        let now = self.frame();
        let lead = (self.sample_rate * 0.06) as i64;
        let (_, fpb) = self.slot_clock(4.0);
        let slots: Vec<TrickSlot> = self.slot_tricks.keys().copied().collect();
        for slot in slots {
            let t = self.slot_tricks(slot);
            let mut due = Vec::new();
            t.jobs.retain(|j| {
                if now + lead >= j.due {
                    due.push(j.clone());
                    false
                } else {
                    true
                }
            });
            if t.riser_end.is_some_and(|end| now >= end) {
                t.riser_end = None;
                t.on.remove(&TrickKind::Riser);
            }
            let tempo = (fpb - t.last_fpb).abs() > 0.5 && !t.on.is_empty();
            for j in due {
                if let Job::TrickOff(k) = j.what {
                    let _ = self.slot_trick(slot, k, false, Some(j.due));
                }
            }
            if tempo {
                self.slot_tricks(slot).last_fpb = fpb;
                self.send_msg(Msg::SlotTrick(slot, TrickMsg::Tempo(fpb)));
            }
            let t = self.slot_tricks(slot);
            let on: Vec<TrickKind> = t.on.keys().copied().collect();
            t.engaged.retain(|id, g| {
                if g.end.is_some_and(|end| now >= end) {
                    return false;
                }
                match catalogue::find(id).map(|x| x.family) {
                    Some(Family::Trick(k)) => on.contains(&k) || now < g.start,
                    _ => false,
                }
            });
        }
    }
}
