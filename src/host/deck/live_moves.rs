//! The window-side live ops (beyond Swift; docs/ACTION-LOG.md): the moves a hand makes that are
//! neither a `DeckAction` nor a controller's MIDI, as live ops an arrangement plays and a journal
//! replays ([`ArrangeOp::Live`](crate::session::arrangement::ArrangeOp::Live),
//! [`JournalEvent::Live`](crate::journal::event::JournalEvent::Live)).
//!
//! | method | params |
//! |---|---|
//! | `cue` | `deck` + `pfl` (a deck's headphone cue), `input` + `pfl` (a strip's), `mix`, `level` (the phones) |
//! | `nudge` | `deck`, `ratio` (the momentary tempo bend, as `Engine::nudge`) |
//! | `jog` | `deck` + `touch` (the platter held / let go), `deck` + `scrub` (seconds of platter time) |
//! | `history` | as the socket's: `strip` (an input's label, A, B, L, V, M) + `action`: `scrub` (`beats`), `live`, `loop` (`beats`), `loop_step` (`dir`), `beats` (the window kept), `jog` (`deck` A / B / off: aim that deck's wheel at the strip) |
//! | `capture` | as the socket's: `input` + `action` `grab` (`beats`), `start`, `end` |
//! | `library` | (see `live_ops`) `action` `load` / `play` / `stop` / `save`, `id` |
//! | `source` | `bus` (`V`, `M`) + `action` `grab` (`bars`) / `start` (`bars`) / `end`: a capture from a bus |
//! | `loops` | `action: stop_all`: every loop out |
//! | `pitch_range` | `deck`, `range` |
//! | `loop_beats` | `deck`, `beats` (a sample deck's auto-loop) |
//! | `seek` | `deck`, `sample` |
//! | `autonext` | `deck`, `on`, `bars` |
//! | `transport` | as the socket's: `action` `start` / `stop` / `lock` (`deck`) / `unlock` / `free_bpm` (`bpm`), and `running` (`on`: the flag alone, the auto transport's) |
//! | `target` | a controller binding's target press, as its `Action::Target` (`id`, `deck`, `slot`, `index`, `value`, `pressed`, `args`): the ones that are routing or gear (`midiOut`, `loopRoute`…) reach the hub, which only a running app has |

use std::collections::BTreeMap;

use crate::control::controller::{Action, SurfaceHost};
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::live_ops::{flag, num, text};
use super::strip_recall::RecallId;
use super::vocoder::live_params;

/// The labels of the input strips a live op names (`input`, `strip`, a vocoder source
/// `input:LABEL` / `capture:LABEL:BEATS`), pushed onto `out` if new: the strips an offline engine
/// replaying it needs.
pub fn input_labels(method: &str, p: &JsonValue, out: &mut Vec<String>) {
    let mut add = |l: &str| {
        let l = l.trim();
        if !l.is_empty() && !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    };
    match method {
        "input" | "freeze" | "capture" => {
            if let Some(l) = p.get("input").and_then(JsonValue::as_str) {
                add(l);
            }
        }
        "cue" => {
            if let Some(l) = p.get("input").and_then(JsonValue::as_str) {
                add(l);
            }
        }
        "history" => {
            // a deck, L, V, M have a history of their own; anything else is an input's label
            if let Some(l) = p.get("strip").and_then(JsonValue::as_str) {
                let u = l.trim().to_ascii_uppercase();
                if !matches!(u.as_str(), "A" | "B" | "C" | "D" | "L" | "V" | "M") {
                    add(l);
                }
            }
        }
        "vocoder" => {
            for k in ["modulator", "carrier"] {
                if let Some(spec) = p.get(k).and_then(JsonValue::as_str) {
                    if let Some(rest) = spec
                        .strip_prefix("input:")
                        .or_else(|| spec.strip_prefix("capture:"))
                    {
                        add(rest.split(':').next().unwrap_or(rest));
                    }
                }
            }
        }
        _ => {}
    }
}

fn deck_of(p: &JsonValue) -> Result<String, String> {
    p.get("deck")
        .map(text)
        .map(|d| d.trim().to_uppercase())
        .filter(|d| !d.is_empty())
        .ok_or_else(|| "no deck".to_string())
}

fn obj(pairs: impl IntoIterator<Item = (&'static str, JsonValue)>) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(pairs))
}

impl Engine {
    /// Run `f` without noting what it does as live ops: a move made inside another one that is
    /// recorded as a whole (a restore, a `DeckAction`'s own library load).
    pub(crate) fn quietly<R>(&mut self, f: impl FnOnce(&mut Engine) -> R) -> R {
        let outer = (std::mem::replace(&mut self.in_surface, true), std::mem::replace(&mut self.quiet, true));
        let r = f(self);
        (self.in_surface, self.quiet) = outer;
        r
    }

    fn recall_named(&self, p: &JsonValue) -> Result<RecallId, String> {
        let name = p
            .get("strip")
            .or_else(|| p.get("input"))
            .map(text)
            .ok_or("no strip")?;
        self.recall_id(&name)
            .ok_or_else(|| format!("no strip {name:?} that keeps a history"))
    }

    pub(crate) fn live_cue(&mut self, p: &JsonValue) -> Result<(), String> {
        if let Some(v) = num(p, "mix") {
            self.set_headphone_mix(v);
        }
        if let Some(v) = num(p, "level") {
            self.set_headphone_level(v);
        }
        if let Some(on) = flag(p, "pfl") {
            if p.get("input").is_some() {
                let k = self.input_named(&p.get("input").map(text).unwrap_or_default())?;
                self.set_input_pfl(k, on);
            } else {
                let d = deck_of(p)?;
                if !self.set_pfl(&d, on) {
                    return Err(format!("no deck {d:?}"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn live_nudge(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        let r = num(p, "ratio").ok_or("nudge: no ratio")?;
        if self.nudge_by_hand(&d, r) {
            Ok(())
        } else {
            Err(format!("no deck {d:?}"))
        }
    }

    pub(crate) fn live_jog(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        if self.index(&d).is_none() {
            return Err(format!("no deck {d:?}"));
        }
        if let Some(down) = flag(p, "touch") {
            self.jog_touch_shifted(&d, down, flag(p, "shift").unwrap_or(false));
        }
        if let Some(s) = num(p, "scrub") {
            self.jog_scrub(&d, s);
        }
        Ok(())
    }

    pub(crate) fn live_history(&mut self, p: &JsonValue) -> Result<(), String> {
        let action = p.get("action").and_then(JsonValue::as_str).unwrap_or("live");
        if action == "jog" {
            // `deck` A / B aims that deck's wheel at `strip`; `off` (or no strip) gives it back
            let d = deck_of(p)?;
            let aim = match p.get("strip").map(text).filter(|s| !s.is_empty()) {
                Some(_) if d != "OFF" && d != "NONE" => Some(self.recall_named(p)?),
                _ => None,
            };
            self.set_history_jog(aim.as_ref().map(|i| (d.as_str(), *i)));
            return Ok(());
        }
        let i = self.recall_named(p)?;
        match action {
            "scrub" => {
                self.input_touch(i);
                self.input_scrub_beats(i, num(p, "beats").ok_or("scrub: no beats")?);
            }
            "live" => {
                self.input_live(i);
            }
            "loop" => {
                let b = num(p, "beats").unwrap_or_else(|| self.recall_loop_beats(i));
                self.input_loop(i, b);
            }
            "loop_step" => {
                self.input_loop_step(i, num(p, "dir").unwrap_or(1.0).signum() as i32);
            }
            "beats" => match i {
                RecallId::Input(k) => {
                    self.set_input_history_beats(k, num(p, "beats").ok_or("beats: no beats")?);
                }
                _ => return Err("beats: an input's history only".into()),
            },
            o => {
                return Err(format!(
                    "history action {o:?}: scrub, live, loop, loop_step, beats or jog"
                ))
            }
        }
        Ok(())
    }

    pub(crate) fn live_capture(&mut self, p: &JsonValue) -> Result<(), String> {
        let label = p.get("input").map(text).ok_or("capture: no input")?;
        let k = self.input_named(&label)?;
        let ok = match p.get("action").and_then(JsonValue::as_str).unwrap_or("grab") {
            "grab" => self.capture_grab(k, num(p, "beats")).is_some(),
            "start" => self.capture_start(k),
            "end" => self.capture_end(k, false),
            o => return Err(format!("capture action {o:?}: grab, start or end")),
        };
        if ok {
            Ok(())
        } else {
            Err(format!("capture on {label}: nothing recorded to take, or one already running"))
        }
    }

    /// `source`: a capture from a bus (`V`, `M`, as `bus`): `grab` (`bars`), `start` (`bars`), `end`.
    pub(crate) fn live_source(&mut self, p: &JsonValue) -> Result<(), String> {
        let name = p.get("bus").map(text).ok_or("source: no bus")?;
        let src = self
            .loop_source(&name, false)
            .ok_or_else(|| format!("no source {name:?}"))?;
        let bars = num(p, "bars").map(|b| b.round() as i64);
        let ok = match p.get("action").and_then(JsonValue::as_str).unwrap_or("grab") {
            "grab" => self.source_grab(&src, bars.unwrap_or(4)).is_some(),
            "start" => self.source_record(&src, true, bars),
            "end" => self.source_record(&src, false, None),
            o => return Err(format!("source action {o:?}: grab, start or end")),
        };
        if ok {
            Ok(())
        } else {
            Err(format!("source {name}: nothing to take (not that much played yet, or a take is running)"))
        }
    }

    pub(crate) fn live_loops(&mut self, p: &JsonValue) -> Result<(), String> {
        match p.get("action").and_then(JsonValue::as_str).unwrap_or("stop_all") {
            "stop_all" => {
                self.stop_all_loops();
                Ok(())
            }
            o => Err(format!("loops action {o:?}: stop_all")),
        }
    }

    pub(crate) fn live_pitch_range(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        if self.set_pitch_range(&d, num(p, "range").ok_or("pitch_range: no range")?) {
            Ok(())
        } else {
            Err(format!("no deck {d:?}"))
        }
    }

    pub(crate) fn live_loop_beats(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        if self.index(&d).is_none() {
            return Err(format!("no deck {d:?}"));
        }
        self.loop_beats(&d, num(p, "beats").ok_or("loop_beats: no beats")?);
        Ok(())
    }

    pub(crate) fn live_seek(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        if self.seek(&d, num(p, "sample").ok_or("seek: no sample")?) {
            Ok(())
        } else {
            Err(format!("{d:?} is no sample deck"))
        }
    }

    pub(crate) fn live_autonext(&mut self, p: &JsonValue) -> Result<(), String> {
        let d = deck_of(p)?;
        self.set_bar_next(&d, flag(p, "on"), num(p, "bars").map(|b| b.round() as i64))
    }

    /// `transport`, as the socket's: `start`, `stop`, `lock` (`deck`), `unlock`, `free_bpm`
    /// (`bpm`); and `running` (`on`), the flag alone (the auto transport's).
    pub(crate) fn live_transport(&mut self, p: &JsonValue) -> Result<(), String> {
        match p.get("action").and_then(JsonValue::as_str).unwrap_or("start") {
            "start" => self.transport_start(),
            "stop" => self.transport_stop(),
            "running" => self.set_transport_running(flag(p, "on").unwrap_or(true)),
            "unlock" => self.set_tempo_lock(None),
            "lock" => {
                let d = deck_of(p)?;
                if self.index(&d).is_none() {
                    return Err(format!("no deck {d:?}"));
                }
                self.set_tempo_lock(Some(&d));
            }
            "free_bpm" => self.set_free_bpm(num(p, "bpm").ok_or("free_bpm: no bpm")?),
            o => {
                return Err(format!(
                    "transport action {o:?}: start, stop, running, lock, unlock or free_bpm"
                ))
            }
        }
        Ok(())
    }

    /// `target`: a binding's target press again, as the router hands it to the engine.
    pub(crate) fn live_target(&mut self, p: &JsonValue) -> Result<(), String> {
        let id = p.get("id").map(text).ok_or("target: no id")?;
        let mut args = BTreeMap::new();
        if let Some(o) = p.get("args").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                args.insert(k.to_string(), text(v));
            }
        }
        SurfaceHost::perform(
            self,
            Action::Target {
                id,
                deck: p.get("deck").map(text).filter(|d| !d.is_empty()),
                slot: num(p, "slot").map(|s| s as i64),
                index: num(p, "index").unwrap_or(0.0).max(0.0) as usize,
                value: num(p, "value").unwrap_or(1.0),
                pressed: flag(p, "pressed").unwrap_or(true),
                args,
            },
        );
        Ok(())
    }

    // MARK: noting the hands (the recording half)

    pub(crate) fn note_cue_deck(&mut self, deck: &str, on: bool) {
        self.note_live(
            format!("pfl:{deck}"),
            format!("cue (PFL) {deck} {}", if on { "on" } else { "off" }),
            false,
            "cue",
            live_params([("deck", deck.into()), ("pfl", on.into())]),
        );
    }

    pub(crate) fn note_phones(&mut self, which: &'static str, v: f64) {
        self.note_live(
            format!("hp{which}"),
            format!("Cue {which} {v:.2}"),
            true,
            "cue",
            obj([(which, JsonValue::Number(v))]),
        );
    }

    pub(crate) fn note_history(&mut self, i: RecallId, action: &str, extra: Option<(&'static str, JsonValue)>, words: String, fold: bool) {
        let label = self.recall_label(i);
        let mut o = JsonObject::from_pairs([
            ("strip", JsonValue::str(&label)),
            ("action", JsonValue::str(action)),
        ]);
        if let Some((k, v)) = extra {
            o.set(k, v);
        }
        self.note_resolved(
            format!("history:{label}:{action}"),
            format!("{label} history {words}"),
            fold,
            "history",
            JsonValue::Object(o),
        );
    }

    pub(crate) fn note_capture(&mut self, input: usize, action: &str, beats: Option<i64>) {
        let label = self.inputs[input].settings.label.clone();
        let mut o = JsonObject::from_pairs([
            ("input", JsonValue::str(&label)),
            ("action", JsonValue::str(action)),
        ]);
        let words = match (action, beats) {
            ("grab", Some(b)) => {
                o.set("beats", JsonValue::int(b));
                format!("grab {b} beats of {label}")
            }
            ("start", _) => format!("capture {label} started"),
            _ => format!("capture {label} ended"),
        };
        self.note_resolved(format!("capture:{label}"), words, false, "capture", JsonValue::Object(o));
    }
}
