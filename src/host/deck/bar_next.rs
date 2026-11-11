//! Auto-next every N bars (beyond Swift; docs/AUTO-NEXT.md): a deck steps to its next seed by
//! itself after a set number of bars, as if the browser wheel had been turned one click.
//!
//! Where it comes from: beats.bitwrap.io's Auto-DJ **Regen** (`public/lib/macros/runtime.js`
//! `autoDjTick`, the `.pn-autodj-regen` select in `public/lib/ui/build.js`): "regenerate the
//! whole track every N bars", off / 8 / 16 / ... / 1024, the next track pre-loaded one bar early
//! and handed over on the N-bar line. The Swift app has only the end-of-song kind
//! (`Deck.autoAdvance`, ported as [`MainDeck::auto_advance`](super::engine::MainDeck)), which
//! stays as it is beside this one.
//!
//! How it lands: the bars are the deck's own bar lines (`tick / 16`), the grid the master clock
//! is locked to when the deck leads or is synced. At the line that starts the last bar of the
//! run, [`Engine::step_seed`] is called with +1 exactly as the browser wheel calls it (the same
//! spec, seed + 1, composed and swapped in at the next bar, a loop from its top; a wandering
//! deck's walk is grabbed), so the compose has a whole bar to finish and the new seed comes in
//! on the N-bar line. The count restarts there. A new seed from anywhere else (the wheel,
//! Compose, Regenerate, Wander) restarts the count from the bar it comes in on. A stopped deck
//! does not count; a sample deck or one with no composed net waits.
//!
//! Every setting change and every step is traced (the action log): a step with the `swap` op
//! the wheel's step records, so an arrangement replays the seeds exactly; settings in words.

use std::collections::BTreeMap;

use crate::control::action_log::Entry;
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::trace::Traced;

/// The bar counts offered (the window's menu, a binding's up / down).
pub const PRESETS: [u32; 5] = [4, 8, 16, 32, 64];
/// The bar count a deck starts with.
pub const DEFAULT_BARS: u32 = 16;
/// The longest run that can be set (beats' Regen went to 1024).
pub const MAX_BARS: u32 = 1024;

/// One deck's auto-next-every-N-bars.
#[derive(Clone, Debug, PartialEq)]
pub struct BarNext {
    pub on: bool,
    /// Bars between steps, 1..=MAX_BARS.
    pub bars: u32,
    /// Whole bars played of this run; -1 while the bar in progress does not count yet (turned
    /// on mid-bar, or a new seed asked for that comes in at the next line).
    done: i64,
    /// The deck's bar at the last look.
    last_bar: Option<i64>,
    /// The step for this run was made; the count restarts at the next bar line.
    fired: bool,
    /// The seed the deck was on or heading to at the last look (a change not ours restarts
    /// the count).
    seen_seed: Option<f64>,
    /// Steps made since the app started.
    pub steps: u64,
}

impl Default for BarNext {
    fn default() -> Self {
        BarNext {
            on: false,
            bars: DEFAULT_BARS,
            done: -1,
            last_bar: None,
            fired: false,
            seen_seed: None,
            steps: 0,
        }
    }
}

impl BarNext {
    /// Bars left before the next seed comes in, counting the one in progress (N .. 1).
    pub fn bars_left(&self) -> u32 {
        let done = self.done.max(0) as u32;
        self.bars.saturating_sub(done).max(1)
    }

    fn restart(&mut self, mid_bar: bool) {
        self.done = if mid_bar { -1 } else { 0 };
        self.fired = false;
    }
}

/// `n` clamped to what can be set.
pub fn clamp_bars(n: i64) -> u32 {
    n.clamp(1, MAX_BARS as i64) as u32
}

/// The preset after (`dir` > 0) or before `bars`; past either end it stays there.
pub fn step_preset(bars: u32, dir: i64) -> u32 {
    if dir > 0 {
        PRESETS
            .iter()
            .copied()
            .find(|&p| p > bars)
            .unwrap_or(bars.max(PRESETS[4]))
    } else {
        PRESETS
            .iter()
            .rev()
            .copied()
            .find(|&p| p < bars)
            .unwrap_or(bars.min(PRESETS[0]))
    }
}

impl Engine {
    /// The deck's seed now, or the one it is composing to.
    fn bar_next_seed(&self, i: usize) -> Option<f64> {
        let d = &self.decks[i];
        d.track
            .seed_target
            .as_ref()
            .or(d.spec.as_ref())
            .map(|s| s.seed)
    }

    /// Turn auto-next-every-N-bars on or off and / or set its bar count (`None`: unchanged).
    /// Turning it on (or changing the count while on) starts a fresh run from the next bar
    /// line. Traced in words.
    pub fn set_bar_next(
        &mut self,
        deck: &str,
        on: Option<bool>,
        bars: Option<i64>,
    ) -> Result<(), String> {
        let i = self
            .index(deck)
            .ok_or_else(|| format!("no deck {deck:?} (auto-next is on A or B)"))?;
        let seed = self.bar_next_seed(i);
        let d = &mut self.decks[i];
        let before = (d.bar_next.on, d.bar_next.bars);
        if let Some(n) = bars {
            d.bar_next.bars = clamp_bars(n);
        }
        if let Some(on) = on {
            d.bar_next.on = on;
        }
        let after = (d.bar_next.on, d.bar_next.bars);
        if after == before {
            return Ok(());
        }
        let playing = d.playing;
        let bar = d.mirror.tick().div_euclid(16);
        let b = &mut d.bar_next;
        b.restart(playing);
        b.last_bar = Some(bar);
        b.seen_seed = seed;
        let text = if b.on {
            format!("Auto-next {deck} every {} bars", b.bars)
        } else if before.0 {
            format!("Auto-next {deck} off")
        } else {
            format!("Auto-next {deck}: {} bars (off)", b.bars)
        };
        let key = format!("autonext:{deck}");
        let name = self.decks[i].name.clone();
        let (on_now, bars_now) = after;
        self.bar_next_trace(key, text, Vec::new(), None);
        // The setting is words only in an arrangement (its replay steps by the swaps the steps
        // record), but a journal replays the engine, which counts the bars itself: it needs
        // the setting, as it stands now.
        self.journal_live(
            "autonext",
            super::vocoder::live_params([
                ("deck", name.into()),
                ("on", on_now.into()),
                ("bars", crate::json::JsonValue::int(bars_now as i64)),
            ]),
        );
        Ok(())
    }

    /// Whether `deck` steps every N bars.
    pub fn bar_next_on(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.bar_next.on)
    }

    /// Bars left on `deck` before its next seed (counting the bar in progress), while it is
    /// on and counting (playing a composed net). None otherwise.
    pub fn bar_next_left(&self, deck: &str) -> Option<u32> {
        let d = self.deck(deck)?;
        (d.bar_next.on && d.playing && d.is_net() && d.spec.is_some())
            .then(|| d.bar_next.bars_left())
    }

    /// Why `deck`'s auto-next is not counting, if it is not.
    fn bar_next_idle(&self, i: usize) -> Option<&'static str> {
        let d = &self.decks[i];
        if !d.bar_next.on {
            Some("off")
        } else if !d.is_net() {
            Some("not a composed net (a sample deck)")
        } else if d.spec.is_none() {
            Some("no composed net (nothing to step from)")
        } else if !d.playing {
            Some("stopped")
        } else {
            None
        }
    }

    /// Every deck's auto-next: on, bars, bars left, steps, why idle; and the presets.
    pub fn bar_next_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for (i, d) in self.decks.iter().enumerate() {
            let b = &d.bar_next;
            let idle = self.bar_next_idle(i);
            let mut x = JsonObject::new();
            x.set("on", b.on.into());
            x.set("bars", JsonValue::int(b.bars as i64));
            x.set(
                "bars_left",
                if idle.is_none() {
                    JsonValue::int(b.bars_left() as i64)
                } else {
                    JsonValue::Null
                },
            );
            x.set("pending", (b.fired && idle.is_none()).into());
            x.set("steps", JsonValue::int(b.steps as i64));
            x.set("idle", idle.map_or(JsonValue::Null, |s| s.into()));
            o.set(d.name.clone(), JsonValue::Object(x));
        }
        o.set(
            "presets",
            JsonValue::Array(PRESETS.iter().map(|&p| JsonValue::int(p as i64)).collect()),
        );
        JsonValue::Object(o)
    }

    /// Count the bars on every deck that has auto-next on and make the step on the line that
    /// starts a run's last bar (part of [`service`](Self::service)).
    pub(crate) fn service_bar_next(&mut self) {
        for i in 0..self.decks.len() {
            if !self.decks[i].bar_next.on {
                continue;
            }
            let seed = self.bar_next_seed(i);
            let d = &mut self.decks[i];
            let bar = d.mirror.tick().div_euclid(16);
            let counting = d.playing && d.is_net() && d.spec.is_some();
            let b = &mut d.bar_next;
            let last = b.last_bar.replace(bar);
            if !counting {
                // a stopped deck starts its run over when it plays again
                b.restart(false);
                b.seen_seed = seed;
                continue;
            }
            if b.seen_seed != seed {
                // a new seed from somewhere else (the wheel, Compose, Wander): it comes in at
                // the next line, and the run counts from there
                b.seen_seed = seed;
                b.restart(true);
                continue;
            }
            // bar lines crossed since the last look: forward, or a jump back (a loop's wrap,
            // a swap to the top) which is a line too
            let crossed = match last {
                Some(l) if bar > l => bar - l,
                Some(l) if bar < l => 1,
                _ => 0,
            };
            if crossed > 0 {
                if b.fired {
                    // the step came in on this line: a new run
                    b.fired = false;
                    b.done = crossed - 1;
                } else {
                    b.done += crossed;
                }
            }
            if b.fired || b.done < 0 || b.done < b.bars as i64 - 1 {
                continue;
            }
            // the last bar of the run has begun: step now, so it comes in on the next line
            let n = b.bars;
            let name = d.name.clone();
            let from = seed;
            let Some(to) = self.step_seed(&name, 1) else {
                continue;
            };
            self.grab_wander(&name);
            let seed_now = self.bar_next_seed(i);
            let b = &mut self.decks[i].bar_next;
            b.fired = true;
            b.steps += 1;
            b.seen_seed = seed_now;
            let text = format!(
                "Auto-next {name} ({n} bars) +1 → seed {:.0}{}",
                to.seed,
                from.map_or(String::new(), |f| format!(" (from {f:.0})"))
            );
            let ops = self.swap_op(&name).into_iter().collect();
            self.bar_next_trace(format!("autonext:{name}:step"), text, ops, Some(1));
        }
    }

    fn bar_next_trace(
        &mut self,
        key: String,
        text: String,
        ops: Vec<crate::session::arrangement::ArrangeOp>,
        step: Option<i64>,
    ) {
        if !self.tracing() {
            return;
        }
        // a step is the engine's own move; a setting is whoever asked
        let source = if step.is_some() {
            "auto-next".to_string()
        } else if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(Traced {
            entry: Entry {
                source,
                key,
                text,
                step: None,
                fold: false,
            },
            ops,
        });
    }

    /// An `autoNextBars` binding: `op` toggle (the default) / on / off / up / down (the next
    /// preset, 4 8 16 32 64) / set (with `bars`); `bars` alone with toggle / on sets it too.
    /// Acts on press only.
    pub(crate) fn bar_next_target(
        &mut self,
        deck: &str,
        args: &BTreeMap<String, String>,
        pressed: bool,
    ) -> Result<(), String> {
        if !pressed {
            return Ok(());
        }
        let bars = args
            .get("bars")
            .map(|s| {
                s.trim()
                    .parse::<i64>()
                    .map_err(|_| format!("bars {s:?} is not a whole number"))
            })
            .transpose()?;
        let cur = self
            .deck(deck)
            .map(|d| d.bar_next.clone())
            .ok_or_else(|| format!("no deck {deck:?}"))?;
        match args.get("op").map(String::as_str).unwrap_or("toggle") {
            "toggle" => self.set_bar_next(deck, Some(!cur.on), bars),
            "on" => self.set_bar_next(deck, Some(true), bars),
            "off" => self.set_bar_next(deck, Some(false), None),
            "up" => self.set_bar_next(deck, None, Some(step_preset(cur.bars, 1) as i64)),
            "down" => self.set_bar_next(deck, None, Some(step_preset(cur.bars, -1) as i64)),
            "set" => self.set_bar_next(deck, None, Some(bars.ok_or("op set needs bars")?)),
            o => Err(format!(
                "no auto-next op {o:?}; one of toggle, on, off, up, down, set"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_step_and_clamp() {
        assert_eq!(step_preset(16, 1), 32);
        assert_eq!(step_preset(16, -1), 8);
        assert_eq!(step_preset(64, 1), 64);
        assert_eq!(step_preset(4, -1), 4);
        assert_eq!(step_preset(12, 1), 16);
        assert_eq!(step_preset(12, -1), 8);
        assert_eq!(step_preset(128, 1), 128);
        assert_eq!(step_preset(2, -1), 2);
        assert_eq!(clamp_bars(0), 1);
        assert_eq!(clamp_bars(5000), MAX_BARS);
    }

    #[test]
    fn bars_left_counts_the_bar_in_progress() {
        let mut b = BarNext {
            bars: 8,
            ..Default::default()
        };
        assert_eq!(b.bars_left(), 8);
        b.done = 0;
        assert_eq!(b.bars_left(), 8);
        b.done = 7;
        assert_eq!(b.bars_left(), 1);
    }
}
