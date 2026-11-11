//! Copy a deck onto the other (docs/COPY-DECK.md), beyond Swift's `copyDeck` (which loads
//! the net from its start): the copy lands on the source's next bar, in sync, where the source
//! is in its song.
//!
//! - A net deck: its source (the spec, or the raw-`nets` payload) carries the genre, seed,
//!   structure and every sound choice (palette, kit, role voices, instruments) as the deck's
//!   `tracks` overrides, so composing it again on the target is the same song with the same
//!   sound. It is composed on a worker starting at the source's next bar line; a playing target
//!   is first synced to the source (tempo and bar phase) so its own next bar is that bar line,
//!   and swaps there; a stopped one is put on that tick and started when the source crosses it,
//!   then lined up by the bar. The source's disabled roles go first (they shape what compiles),
//!   its role mutes once the copy plays.
//! - A sample deck (a file, a clip moved onto it): the same audio and grid, at the same place,
//!   at the source's tempo and bar phase, from its next bar (at once without a grid).
//!
//! The copy is one line in the action log ("copy A → B ...") and its landing another.

use crate::deck::perform::next_swap_tick;
use crate::deck::turntable::TurntableCommand;
use crate::session::model::DeckSource;
use crate::deck::beat::SyncAlign;

use super::engine::Engine;
use super::sample::PreparedSample;
use super::track::{PreparedTrack, Task, TrackSource};

/// A copy on its way.
pub struct CopyJob {
    pub from: String,
    pub to: String,
    stage: Stage,
    /// Words for the log and the socket.
    pub what: String,
    /// Service passes so far: a copy that has not landed in [`GIVE_UP`] is dropped.
    age: u32,
}

/// Passes (about a minute of frames) before a copy that never landed is given up.
const GIVE_UP: u32 = 4000;

enum Stage {
    /// Composing; it lands at `at` (the source's bar line) at the earliest.
    Compose { task: Task<Option<PreparedTrack>>, at: i64 },
    /// Loaded on the stopped target at `at`: started when the source crosses it.
    StartAt { at: i64 },
    /// Swapping on the target's next bar: the role mutes follow once it plays the copy.
    Mutes { roles: Vec<(String, bool)>, at: i64 },
    /// A sample: lands when the source's beat passes `bar_beat` (None: now).
    Sample { prepared: Option<PreparedSample>, bar_beat: Option<f64> },
}

/// The tick a bar line starts on: the bar after the one `tick` is in.
fn next_bar_line(tick: i64) -> i64 {
    next_swap_tick(tick) + 1
}

impl Engine {
    /// Copy deck `from` onto deck `to`. Returns what it will do; the copy lands later
    /// ([`service_copies`](Engine::service_copies), every frame).
    pub fn copy_deck(&mut self, from: &str, to: &str) -> Result<String, String> {
        if from == to {
            return Err(format!("copy {from} onto itself"));
        }
        let src = self.deck(from).ok_or_else(|| format!("no deck {from:?}"))?;
        if self.deck(to).is_none() || to == "L" {
            return Err(format!("no deck {to:?}"));
        }
        if let Some(s) = &src.sample {
            let prepared = PreparedSample::with_grid(s.buffer.clone(), &s.title, s.grid);
            let what = format!("copy {from} → {to}: sample {}", s.title);
            let playing = src.playing;
            let bar_beat = if playing {
                src.beat_state(self.sample_rate)
                    .map(|b| (b.beat / 4.0).floor() * 4.0 + 4.0)
            } else {
                None
            };
            // a copy already on its way to `to` gives way to this one
            self.copies.retain(|c| c.to != to);
            self.copies.push(CopyJob {
                from: from.into(),
                to: to.into(),
                stage: Stage::Sample { prepared: Some(prepared), bar_beat },
                what: what.clone(),
                age: 0,
            });
            self.trace_push_text(&format!("{what}{}", if bar_beat.is_some() { ", on the next bar" } else { "" }));
            self.service_copies();
            return Ok(what);
        }
        let source = src.net_source().ok_or_else(|| format!("deck {from} has nothing on it"))?;
        let ts = match &source {
            DeckSource::Net(spec) => TrackSource::Net(spec.clone()),
            DeckSource::RawNets(p) => TrackSource::RawNets(p.clone()),
            _ => return Err(format!("deck {from} has nothing to copy")),
        };
        let label = match &source {
            DeckSource::Net(s) => format!("{} {}", s.genre, s.seed),
            _ => "raw nets".to_string(),
        };
        let playing = src.mirror.is_playing();
        let tick = src.mirror.tick();
        let at = if playing { next_bar_line(tick) } else { tick.max(0) };
        // the source's disabled roles are in the target's role filter before it compiles
        let theirs = src.disabled_roles.clone();
        let mine = self.deck(to).map(|d| d.disabled_roles.clone()).unwrap_or_default();
        for r in &mine {
            if !theirs.contains(r) {
                self.set_role_disabled(to, r, false);
            }
        }
        for r in &theirs {
            self.set_role_disabled(to, r, true);
        }
        let preparer = self.preparer(to).ok_or_else(|| format!("deck {to} cannot take a net"))?;
        let task = if self.deterministic {
            Task::run(|| PreparedTrack::from_source(&ts, &preparer, at))
        } else {
            self.bg_from_source(preparer, ts, at)
        };
        let what = format!("copy {from} → {to}: {label}");
        self.copies.retain(|c| c.to != to);
        self.copies.push(CopyJob {
            from: from.into(),
            to: to.into(),
            stage: Stage::Compose { task, at },
            what: what.clone(),
            age: 0,
        });
        self.trace_push_text(&format!(
            "{what}{}",
            if playing { ", on the next bar in sync" } else { "" }
        ));
        self.service_copies();
        Ok(what)
    }

    /// Is a copy's track still composing (its worker, or the host's composer, has not answered)?
    pub fn copy_composing(&self) -> bool {
        self.copies
            .iter()
            .any(|c| matches!(&c.stage, Stage::Compose { task, .. } if !task.is_finished()))
    }

    /// The copies still on their way (`(from, to, what)`).
    pub fn pending_copies(&self) -> Vec<(String, String, String)> {
        self.copies
            .iter()
            .map(|c| (c.from.clone(), c.to.clone(), c.what.clone()))
            .collect()
    }

    /// Move every copy on: land what is composed when its bar comes, start a stopped target
    /// on the bar line, set the role mutes once the copy plays.
    pub fn service_copies(&mut self) {
        if self.copies.is_empty() {
            return;
        }
        // the landing lines come from the copy, not whoever drives the engine this frame
        let before = std::mem::replace(&mut self.trace_via, "copy".into());
        self.service_copies_now();
        self.trace_via = before;
    }

    fn service_copies_now(&mut self) {
        let jobs = std::mem::take(&mut self.copies);
        for mut job in jobs {
            job.age += 1;
            if job.age > GIVE_UP {
                self.trace_push_text(&format!("{}: given up (it never landed)", job.what));
                continue;
            }
            if self.step_copy(&mut job) {
                self.copies.push(job);
            }
        }
    }

    /// One step of a copy. True while it has more to do.
    fn step_copy(&mut self, job: &mut CopyJob) -> bool {
        let (from, to) = (job.from.clone(), job.to.clone());
        let Some(src) = self.deck(&from) else { return false };
        let src_playing = src.playing || src.mirror.is_playing();
        let src_tick = src.mirror.tick();
        let sr = self.sample_rate;
        match &mut job.stage {
            Stage::Compose { task, at } => {
                if !task.is_finished() {
                    return true;
                }
                let Some(Some(mut track)) = task.try_take() else {
                    self.trace_push_text(&format!("{}: the copy failed to compose", job.what));
                    return false;
                };
                let at0 = *at;
                let to_playing = self.deck(&to).is_some_and(|d| d.mirror.is_playing());
                let mut land = at0;
                if src_playing && src_tick >= at0 {
                    // composed after its bar: the bar after the source's tick now
                    land = next_bar_line(src_tick);
                    if let Some(p) = self.preparer(&to) {
                        track = track.restarted(&p, land);
                    }
                }
                let roles: Vec<(String, bool)> = self
                    .deck(&from)
                    .map(|d| d.roles.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| {
                        let on = self.is_role_audible(&from, &r);
                        (r, on)
                    })
                    .collect();
                if to_playing && src_playing {
                    // its bars on the source's, then the copy from the source's bar line, swapped
                    // in on the target's next bar (the same moment now)
                    self.sync(&to, &from, SyncAlign::Bar);
                    self.load_net(&to, track);
                    let land = next_bar_line(src_tick);
                    self.relocate(&to, land, None, None);
                    job.stage = Stage::Mutes { roles, at: land };
                    return true;
                }
                // a stopped target switches at once: cued on the bar line, started on it
                if to_playing {
                    self.pause(&to);
                }
                self.load_net(&to, track);
                self.sync(&to, &from, SyncAlign::TempoOnly);
                if !src_playing {
                    let t = src_tick.max(0);
                    if t > 0 {
                        self.relocate(&to, t, None, None);
                    }
                    job.stage = Stage::Mutes { roles, at: t };
                    return true;
                }
                self.relocate(&to, land, None, None);
                job.stage = Stage::StartAt { at: land };
                true
            }
            Stage::StartAt { at } => {
                if !src_playing {
                    // the source stopped before its bar: the copy stays cued on the bar line
                    self.trace_push_text(&format!("{}: cued (the source stopped)", job.what));
                    return false;
                }
                let to_tick = self.deck(&to).map_or(0, |d| d.mirror.tick());
                if to_tick != *at {
                    // the cue has not landed yet; if the bar went by meanwhile, the next one
                    if src_tick >= *at {
                        *at = next_bar_line(src_tick);
                        let a = *at;
                        self.relocate(&to, a, None, None);
                    }
                    return true;
                }
                if src_tick < *at {
                    return true;
                }
                let at0 = *at;
                self.play(&to);
                self.sync(&to, &from, SyncAlign::Bar);
                let roles = self.role_states(&from);
                self.trace_push_text(&format!("{}: landed in sync", job.what));
                job.stage = Stage::Mutes { roles, at: at0 };
                true
            }
            Stage::Mutes { roles, at } => {
                let Some(d) = self.deck(&to) else { return false };
                // the copy is on the target once it has the song's roles and has reached its tick
                // (the tick within a few bars of `at`: an older song's count is elsewhere)
                let t = d.mirror.tick();
                if d.roles.is_empty() || t < *at || t >= *at + 64 {
                    return true;
                }
                let roles = std::mem::take(roles);
                // the copy brought its song's own tempo: the source's again, on its bars
                let align = if src_playing { SyncAlign::Bar } else { SyncAlign::TempoOnly };
                self.sync(&to, &from, align);
                for (r, on) in roles {
                    if !self.is_role_disabled(&to, &r) {
                        self.set_role(&to, &r, on);
                    }
                }
                self.trace_push_text(&format!("{}: playing", job.what));
                false
            }
            Stage::Sample { prepared, bar_beat } => {
                if let Some(bb) = *bar_beat {
                    let now = self.deck(&from).and_then(|d| d.beat_state(sr)).map(|b| b.beat);
                    match now {
                        Some(b) if b < bb && src_playing => return true,
                        _ => {}
                    }
                }
                let Some(p) = prepared.take() else { return false };
                let pos = self.sample_position(&from).unwrap_or(0.0);
                let pitch = self.deck(&from).map_or(0.0, |d| d.pitch);
                self.load_sample(&to, p);
                self.quietly(|e| e.seek(&to, pos));
                self.set_pitch(&to, pitch);
                if src_playing {
                    self.launch(&to);
                    self.set_playing(&to, true);
                    self.sync(&to, &from, SyncAlign::Bar);
                }
                // its loop, if it loops
                let lp = self.deck(&from).and_then(|d| d.sample.as_ref()).map(|s| (s.loop_start, s.loop_end, s.looping));
                if let Some((Some(a), Some(b), true)) = lp {
                    self.turntable(&to, TurntableCommand::Loop { start: a, end: b });
                    if let Some(s) = self.deck_mut(&to).and_then(|d| d.sample.as_mut()) {
                        s.loop_start = Some(a);
                        s.loop_end = Some(b);
                        s.looping = true;
                    }
                }
                self.trace_push_text(&format!("{}: landed", job.what));
                false
            }
        }
    }

    fn role_states(&self, deck: &str) -> Vec<(String, bool)> {
        self.deck(deck)
            .map(|d| d.roles.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let on = self.is_role_audible(deck, &r);
                (r, on)
            })
            .collect()
    }
}
