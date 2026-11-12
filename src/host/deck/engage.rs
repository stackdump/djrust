//! Engaging an effect: one way to fire anything in [`crate::fx::catalogue`], whichever family
//! it comes from (a trick, a net macro, a master macro, an aux macro).
//!
//! - **Starts on the bar.** Every engagement lands on the aimed deck's next bar (at once when the
//!   deck is not playing). Tricks, aux macros and a sample deck's master macros are sent with the
//!   exact frame; a net macro, and a master macro on a net deck, are started by the service tick
//!   when the bar is a few milliseconds off ([`Engine::sync_trick_tempo`], about every 10 ms).
//! - **Lasts [`Engagement`].** `Bars(n)` runs `n` whole bars and ends itself; `Latch` stays on
//!   until [`Engine::unlatch`] (on the next bar) or PANIC; `Momentary` is a held pad: on at the
//!   bar after the press, off at once on [`Engine::release`]. A transition is a one-shot and is
//!   only ever `Momentary`: it runs its own length and needs no release.
//! - **Continuous controls are not engagements.** A knob or slider (`trick_knob`,
//!   `set_master_fx`, `set_aux_fx`) acts at once, whatever the beat.
//!
//! [`Engine::fx_state`] says what is engaged on a deck, for lighting pads.

use crate::fx::catalogue::{self, Entry, Family};
use crate::fx::tricks::TrickKind;
use crate::wave::macros::MacroUnit;

use super::engine::Engine;
use super::tricks::TrickQuant;

/// How long an effect stays on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engagement {
    /// While the pad is held (and, for a transition, its own length).
    Momentary,
    /// Whole bars, then it ends by itself. 1 to 4 in the panel.
    Bars(u8),
    /// Until let go.
    Latch,
}

/// One engaged effect, for a pad's light.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FxState {
    pub id: String,
    pub latched: bool,
    /// Asked for, and waiting for its bar.
    pub pending: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Engaged {
    pub latched: bool,
    pub start: i64,
    pub end: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct FxJob {
    pub due: i64,
    pub id: String,
    pub what: Job,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Job {
    /// Start a net macro, or a master macro on a net deck, `ticks`/`bars` as given.
    Start(Engagement),
    /// End a trick at its length.
    TrickOff(TrickKind),
    /// End a latch (on the bar).
    Release,
}

/// Net macro ticks for a latch: it holds until it is let go (16 ticks a bar).
const LATCH_TICKS: i64 = 16_000_000;
/// How far ahead of its frame the service tick sends an exact-frame message (a trick's end): the
/// render thread holds it for the frame.
fn exact_lead(sample_rate: f64) -> i64 {
    (sample_rate * 0.06) as i64
}

/// How far ahead of its bar the service tick starts a net macro (or a master macro on a net
/// deck), which the render thread applies at once: a little over one service period (about
/// 10 ms) plus a block, so it lands within about 20 ms of the bar, never after it by a tick.
fn start_lead(sample_rate: f64) -> i64 {
    (sample_rate * 0.02) as i64
}

impl Engine {
    fn deck_frames(&self, deck: &str) -> i64 {
        self.deck(deck)
            .map(|d| d.mirror.rendered_frames.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Check a request against the catalogue: the id is offered, a transition is momentary, a
    /// sweep has no latch, and a song macro has a net deck to land on.
    fn engage_check(&self, deck: &str, id: &str, e: Engagement) -> Result<&'static Entry, String> {
        let entry = catalogue::find(id).ok_or_else(|| format!("no effect {id:?}"))?;
        self.index(deck).ok_or_else(|| format!("no deck {deck:?}"))?;
        match e {
            Engagement::Momentary => {}
            Engagement::Bars(n) if !(1..=64).contains(&n) => {
                return Err(format!("{n} bars: use 1 to 4"));
            }
            Engagement::Bars(_) if entry.transition() => {
                return Err(format!("{id} is a one-shot: momentary only"));
            }
            Engagement::Bars(_) => {}
            Engagement::Latch if !entry.latchable() => {
                return Err(format!("{id} cannot latch ({})", entry.category.label()));
            }
            Engagement::Latch => {}
        }
        if entry.needs_net() && !self.deck(deck).is_some_and(|d| d.is_net()) {
            return Err(format!("{id} needs a net deck"));
        }
        Ok(entry)
    }

    /// Engage `id` on `deck`: on the deck's next bar, for `e`. See the module's notes.
    pub fn engage(&mut self, deck: &str, id: &str, e: Engagement) -> Result<(), String> {
        let entry = self.engage_check(deck, id, e)?;
        let i = self.index(deck).ok_or("no such deck")?;
        let now = self.deck_frames(deck);
        let bar = self.deck_grid_frame(deck, 4.0);
        let start = bar.unwrap_or(now);
        let fpb = self.deck_fpb(deck);
        let end = match e {
            Engagement::Bars(n) => Some(start + (n as f64 * 4.0 * fpb).round() as i64),
            _ => None,
        };
        // a net macro asked again replaces the run before it; the others do by themselves
        self.decks[i].tricks.jobs.retain(|j| j.id != id);
        match entry.family {
            Family::Trick(k) => {
                let q = bar.map_or(TrickQuant::Now, TrickQuant::Frame);
                self.trick(deck, k, true, None, q)?;
                if let Some(end) = end {
                    self.decks[i].tricks.jobs.push(FxJob { due: end, id: id.into(), what: Job::TrickOff(k) });
                }
            }
            Family::Aux(m) => {
                let bars = match e {
                    Engagement::Bars(n) => Some(n as i32),
                    _ => Some(0),
                };
                if !self.fire_macro_traced(deck, m.id(), bars) {
                    return Err(format!("{id} did not fire on {deck}"));
                }
            }
            Family::Net(_) => {
                if bar.is_some() {
                    self.decks[i].tricks.jobs.push(FxJob { due: start, id: id.into(), what: Job::Start(e) });
                } else {
                    self.start_net_or_master(deck, entry, e)?;
                }
            }
            Family::Master(_) => {
                if bar.is_some() && !self.is_sample_deck(deck) {
                    self.decks[i].tricks.jobs.push(FxJob { due: start, id: id.into(), what: Job::Start(e) });
                } else {
                    self.start_net_or_master(deck, entry, e)?;
                }
            }
        }
        let latched = matches!(e, Engagement::Latch | Engagement::Momentary) && !entry.transition();
        self.decks[i].tricks.engaged.insert(
            id.to_string(),
            Engaged {
                latched,
                start,
                // a held trick (tape stop) is on until the pad lifts; a song one-shot lights for
                // a bar
                end: end.or_else(|| {
                    (entry.transition() && !matches!(entry.family, Family::Trick(_)))
                        .then(|| start + (4.0 * fpb) as i64)
                }),
            },
        );
        Ok(())
    }

    /// Start a net macro, or a master macro, now (its bar has come, or the deck does not play).
    fn start_net_or_master(&mut self, deck: &str, entry: &Entry, e: Engagement) -> Result<(), String> {
        let held = matches!(e, Engagement::Latch | Engagement::Momentary) && !entry.transition();
        let ok = match entry.family {
            Family::Net(m) => match e {
                Engagement::Bars(n) => self.fire_macro_traced(deck, m.id(), Some(n as i32)),
                _ if entry.transition() => self.fire_macro_traced(deck, m.id(), None),
                _ => {
                    let ticks = if m.unit() == MacroUnit::Bar { LATCH_TICKS } else { m.ticks(None) };
                    self.fire(deck, m, Some(ticks))
                }
            },
            Family::Master(m) => {
                let bars = match e {
                    Engagement::Bars(n) => Some(n as i32),
                    _ if held => Some(0),
                    _ => None,
                };
                self.fire_macro_traced(deck, m.id(), bars)
            }
            _ => false,
        };
        if ok { Ok(()) } else { Err(format!("{} did not fire on {deck}", entry.id)) }
    }

    /// Let go of a held (or latched) effect at once: a pad lifted. A one-shot is left to run.
    pub fn release(&mut self, deck: &str, id: &str) -> Result<(), String> {
        let entry = catalogue::find(id).ok_or_else(|| format!("no effect {id:?}"))?;
        let i = self.index(deck).ok_or_else(|| format!("no deck {deck:?}"))?;
        self.decks[i].tricks.jobs.retain(|j| j.id != id);
        if entry.transition() && !matches!(entry.family, Family::Trick(_)) {
            // a song one-shot (cut, double drop) runs its own length
            return Ok(());
        }
        self.decks[i].tricks.engaged.remove(id);
        match entry.family {
            Family::Trick(k) => self.trick(deck, k, false, None, TrickQuant::Now)?,
            Family::Aux(m) => {
                self.release_aux_macro(deck, m);
            }
            Family::Net(_) => {
                self.command(deck, super::core::Command::MacroEnd);
            }
            Family::Master(m) => {
                self.command(deck, super::core::Command::MasterRelease(m));
            }
        }
        Ok(())
    }

    /// Let go of a latch on the next bar: the pad pressed again.
    pub fn unlatch(&mut self, deck: &str, id: &str) -> Result<(), String> {
        let entry = catalogue::find(id).ok_or_else(|| format!("no effect {id:?}"))?;
        let i = self.index(deck).ok_or_else(|| format!("no deck {deck:?}"))?;
        let Some(bar) = self.deck_grid_frame(deck, 4.0) else {
            return self.release(deck, id);
        };
        self.decks[i].tricks.jobs.retain(|j| j.id != id);
        if let Family::Trick(k) = entry.family {
            self.trick(deck, k, false, None, TrickQuant::Frame(bar))?;
            if let Some(g) = self.decks[i].tricks.engaged.get_mut(id) {
                g.latched = false;
                g.end = Some(bar);
            }
        } else {
            self.decks[i].tricks.jobs.push(FxJob { due: bar, id: id.into(), what: Job::Release });
            if let Some(g) = self.decks[i].tricks.engaged.get_mut(id) {
                g.end = Some(bar);
            }
        }
        Ok(())
    }

    /// PANIC for the engagements: everything engaged on the deck is let go at once (a latch
    /// included), and what waited for its bar is dropped.
    pub fn clear_engagements(&mut self, deck: &str) {
        let ids: Vec<String> = self
            .deck(deck)
            .map(|d| d.tricks.engaged.keys().cloned().collect())
            .unwrap_or_default();
        for id in ids {
            let _ = self.release(deck, &id);
        }
        if let Some(d) = self.deck_mut(deck) {
            d.tricks.engaged.clear();
            d.tricks.jobs.clear();
        }
    }

    /// What is engaged on `deck`, for the pads' lights.
    pub fn fx_state(&self, deck: &str) -> Vec<FxState> {
        let Some(d) = self.deck(deck) else { return Vec::new() };
        let now = self.deck_frames(deck);
        d.tricks
            .engaged
            .iter()
            .map(|(id, g)| FxState { id: id.clone(), latched: g.latched, pending: now < g.start })
            .collect()
    }

    /// The service tick's half: start what is due, end what has run its bars.
    pub(crate) fn service_engagements(&mut self) {
        let exact = exact_lead(self.sample_rate);
        let early = start_lead(self.sample_rate);
        for i in 0..self.decks.len() {
            if self.decks[i].tricks.jobs.is_empty() && self.decks[i].tricks.engaged.is_empty() {
                continue;
            }
            let name = self.decks[i].name.clone();
            let now = self.deck_frames(&name);
            let mut due = Vec::new();
            self.decks[i].tricks.jobs.retain(|j| {
                let lead = if matches!(j.what, Job::Start(_)) { early } else { exact };
                if now + lead >= j.due {
                    due.push(j.clone());
                    false
                } else {
                    true
                }
            });
            for j in due {
                match j.what {
                    Job::TrickOff(k) => {
                        let _ = self.trick(&name, k, false, None, TrickQuant::Frame(j.due));
                    }
                    Job::Start(e) => {
                        if let Some(entry) = catalogue::find(&j.id) {
                            let _ = self.start_net_or_master(&name, entry, e);
                        }
                    }
                    Job::Release => {
                        let _ = self.release(&name, &j.id);
                    }
                }
            }
            // an engagement that has run its length is over; so is a trick that is no longer on
            // (a riser has run its bars)
            let on: Vec<TrickKind> = self.decks[i].tricks.on.keys().copied().collect();
            self.decks[i].tricks.engaged.retain(|id, g| {
                if g.end.is_some_and(|end| now >= end) {
                    return false;
                }
                match catalogue::find(id).map(|x| x.family) {
                    Some(Family::Trick(k)) => on.contains(&k) || now < g.start,
                    _ => true,
                }
            });
        }
    }
}
