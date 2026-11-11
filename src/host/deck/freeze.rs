//! Granular freeze on any strip with a history (docs/INPUT-HISTORY.md, "Freeze" and "Any
//! strip"): an input, a deck, `L`, `V` or `M`. The main thread's half of
//! [`crate::fx::granular`].
//!
//! [`Engine::input_freeze`] freezes the last N beats of an input's history into a grain cloud,
//! or thaws it; with `quantize` on (the default) both land on the next beat of the master clock
//! (at once when no clock runs). While a strip is frozen, the moves that would scrub its
//! history (a jog wheel aimed at it with `--history-jog`, a `historyScrub` binding, the
//! socket's `history scrub`) move the freeze position instead. Every move is traced in words.

use crate::fx::granular::FreezeParam;
use crate::history;
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::strip_recall::{RecallId, RecallMsg};
use super::trace::Traced;
use super::vocoder::live_params;

/// The fewest frames between a quantised freeze being asked and its beat: the message must
/// reach the render thread first, so a beat closer than this waits for the next one.
const LEAD_FRAMES: f64 = 512.0;

impl Engine {
    /// Strip `i` (an input, or any strip with a history) asked to be frozen (it may be waiting
    /// for its beat).
    pub fn is_frozen(&self, i: impl Into<RecallId>) -> bool {
        match i.into() {
            RecallId::Input(k) => self.inputs.get(k).is_some_and(|x| x.frozen),
            RecallId::Strip(s) => self.main_recall(s).is_some_and(|r| r.frozen),
        }
    }

    /// A freeze move in the action trace: its words and the `freeze` live op that makes it
    /// again (`params`, the strip named in it here: an input's label, A, B, L, V, M).
    fn freeze_trace(&mut self, i: RecallId, key: &str, text: String, fold: bool, params: JsonValue) {
        if !self.tracing() {
            return;
        }
        let label = self.recall_label(i);
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(Traced {
            entry: crate::control::action_log::Entry {
                source: via,
                key: format!("freeze:{label}:{key}"),
                text: format!("{label} {text}"),
                step: None,
                fold,
            },
            ops: vec![crate::session::arrangement::ArrangeOp::Live {
                method: "freeze".into(),
                params: {
                    let mut o =
                        crate::json::JsonObject::from_pairs([("input", JsonValue::str(&label))]);
                    if let Some(p) = params.as_object() {
                        for (k, v) in p.iter() {
                            o.set(k, v.clone());
                        }
                    }
                    JsonValue::Object(o)
                },
            }],
        });
    }

    /// The render frame of the next beat of the master clock, at least [`LEAD_FRAMES`] ahead;
    /// None when no clock runs (freeze at once).
    fn next_beat_frame(&self) -> Option<i64> {
        let (beat, fpb) = self.clock_beat_now()?;
        if !(fpb > 0.0) || !beat.is_finite() {
            return None;
        }
        let mut ahead = (beat.ceil() - beat) * fpb;
        if ahead < LEAD_FRAMES {
            ahead += fpb;
        }
        Some(self.loop_frames() + ahead.round() as i64)
    }

    /// Freeze strip `i` (an input, a deck, `L`, `V`, `M`: its last `beats`, None: the freeze's
    /// own setting) or thaw it. With `quantize` (None: the freeze's setting) it lands on the
    /// next beat. False when the strip keeps no history.
    pub fn input_freeze(
        &mut self,
        i: impl Into<RecallId>,
        on: bool,
        beats: Option<f64>,
        quantize: Option<bool>,
    ) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        if let Some(b) = beats {
            if let Some(f) = self.freeze_settings_mut(i) {
                f.set(FreezeParam::Beats, b);
            }
            let s = self.freeze_settings(i).unwrap_or_default();
            self.recall_send(i, RecallMsg::FreezeSet(s));
        }
        let s = self.freeze_settings(i).unwrap_or_default();
        let q = quantize.unwrap_or(s.quantize);
        let at = if q { self.next_beat_frame() } else { None };
        self.sync_history_tempo();
        let ok = self.recall_send(
            i,
            RecallMsg::Freeze {
                on,
                beats: s.beats,
                at,
            },
        );
        if ok {
            self.set_frozen_flag(i, on);
            let when = if at.is_some() { " on the beat" } else { "" };
            let text = if on {
                format!(
                    "freeze {} beat{}{when}",
                    fmt_beats(s.beats),
                    if s.beats == 1.0 { "" } else { "s" }
                )
            } else {
                format!("thaw{when}")
            };
            self.freeze_trace(
                i,
                "on",
                text,
                false,
                live_params([
                    ("action", if on { "on" } else { "off" }.into()),
                    ("beats", JsonValue::Number(s.beats)),
                    ("now", (!q).into()),
                ]),
            );
        }
        ok
    }

    /// Set a freeze knob in its own units. Returns the value kept.
    pub fn freeze_set(&mut self, i: impl Into<RecallId>, p: FreezeParam, v: f64) -> Option<f64> {
        let i = i.into();
        if !self.has_history(i) {
            return None;
        }
        let kept = self.freeze_settings_mut(i)?.set(p, v)?;
        let s = self.freeze_settings(i)?;
        self.recall_send(i, RecallMsg::FreezeSet(s));
        let text = match p {
            FreezeParam::Beats => format!("freeze length {} beats", fmt_beats(kept)),
            FreezeParam::Size => format!("grain size {kept:.0} ms"),
            FreezeParam::Density => format!("grain density {kept:.0}/s"),
            FreezeParam::Jitter => format!("pitch jitter ±{kept:.1} st"),
            _ => format!("freeze {} {kept:.2}", p.name()),
        };
        self.freeze_trace(
            i,
            p.name(),
            text,
            true,
            live_params([(p.name(), JsonValue::Number(kept))]),
        );
        Some(kept)
    }

    /// Set a freeze knob from a control's 0..1 travel.
    pub fn freeze_knob(&mut self, i: impl Into<RecallId>, p: FreezeParam, v: f64) -> Option<f64> {
        self.freeze_set(i, p, p.from_knob(v))
    }

    /// Quantise freezing and thawing to the beat, or not.
    pub fn freeze_quantize(&mut self, i: impl Into<RecallId>, on: bool) -> bool {
        let i = i.into();
        if !self.has_history(i) {
            return false;
        }
        if let Some(f) = self.freeze_settings_mut(i) {
            f.quantize = on;
        }
        let s = self.freeze_settings(i).unwrap_or_default();
        self.recall_send(i, RecallMsg::FreezeSet(s));
        self.freeze_trace(
            i,
            "quantize",
            format!("freeze quantise {}", if on { "on" } else { "off" }),
            false,
            live_params([("quantize", on.into())]),
        );
        true
    }

    /// Move the freeze position by `frames` of the frozen audio (a jog wheel, a scrub).
    pub fn freeze_scrub(&mut self, i: impl Into<RecallId>, frames: f64) -> bool {
        let i = i.into();
        if !self.has_history(i) || !frames.is_finite() {
            return false;
        }
        let ok = self.recall_send(i, RecallMsg::FreezeScrub(frames));
        // the main thread's copy follows the mirror (the render thread wraps it)
        let held = self.recall_view(i).map_or(0.0, |v| v.freeze_beats() as f64);
        let len = held * history::frames_per_beat(self.master_tempo().1, self.sample_rate);
        if let (true, Some(f)) = (len > 0.0, self.freeze_settings_mut(i)) {
            let p = f.position + frames / len;
            f.position = p - p.floor();
        }
        let pos = self.freeze_settings(i).map_or(0.0, |f| f.position);
        self.freeze_trace(
            i,
            "position",
            format!("freeze position {pos:.2}"),
            true,
            live_params([
                ("action", "scrub".into()),
                ("frames", JsonValue::Number(frames)),
            ]),
        );
        ok
    }

    /// Strip `i`'s freeze as JSON: the knobs, whether frozen (and whether waiting for the
    /// beat), the position, the grains sounding, the beats held.
    pub fn freeze_json(&self, i: impl Into<RecallId>) -> JsonValue {
        let i = i.into();
        let (Some(m), Some(settings)) = (self.recall_view(i), self.freeze_settings(i)) else {
            if let RecallId::Input(k) = i {
                if let Some(inp) = self.inputs.get(k) {
                    // an input with no history: its knobs, and that it has none
                    let mut o = JsonObject::new();
                    o.set("strip", inp.settings.label.clone().into());
                    o.set("has_history", false.into());
                    o.set("settings", inp.freeze.to_json());
                    return JsonValue::Object(o);
                }
            }
            return JsonValue::Null;
        };
        let mut o = JsonObject::new();
        o.set("strip", self.recall_label(i).into());
        o.set("asked", self.is_frozen(i).into());
        o.set(
            "state",
            match m.freeze_state() {
                1 => "freezing",
                2 => "frozen",
                3 => "thawing",
                _ => "off",
            }
            .into(),
        );
        o.set("settings", settings.to_json());
        let r3 = |x: f64| JsonValue::from((x * 1000.0).round() / 1000.0);
        o.set("position", r3(m.freeze_position() as f64));
        o.set("grains", JsonValue::int(m.freeze_grains() as i64));
        o.set("held_beats", r3(m.freeze_beats() as f64));
        o.set("has_history", true.into());
        JsonValue::Object(o)
    }

    /// A `freeze` / `freezeKnob` binding: `op` toggle (default) / on / off / hold (frozen while
    /// held) / quantize (toggles); a knob's `param`.
    pub(crate) fn freeze_target(
        &mut self,
        id: &str,
        input: Option<&str>,
        args: &std::collections::BTreeMap<String, String>,
        value: f64,
        pressed: bool,
    ) -> Result<(), String> {
        let i = self
            .recall_target(input.filter(|s| !s.is_empty()))
            .ok_or("no strip with a history")?;
        let arg = |k: &str| args.get(k).map(String::as_str);
        if id == "freezeKnob" {
            let name = arg("param").filter(|p| !p.is_empty()).unwrap_or("position");
            let p = FreezeParam::parse(name).ok_or_else(|| format!("no knob {name:?}"))?;
            self.freeze_knob(i, p, value);
            return Ok(());
        }
        let beats = arg("beats").and_then(|b| b.trim().parse::<f64>().ok());
        let quant = match arg("quant") {
            Some("now") | Some("off") => Some(false),
            Some("beat") | Some("on") => Some(true),
            _ => None,
        };
        let op = arg("op").unwrap_or("toggle");
        match (op, pressed) {
            ("hold", p) => {
                self.input_freeze(i, p, beats, quant);
            }
            (_, false) => {}
            ("on", true) => {
                self.input_freeze(i, true, beats, quant);
            }
            ("off", true) => {
                self.input_freeze(i, false, beats, quant);
            }
            ("quantize", true) => {
                let q = !self.freeze_settings(i).is_some_and(|f| f.quantize);
                self.freeze_quantize(i, q);
            }
            ("toggle", true) => {
                let on = !self.is_frozen(i);
                self.input_freeze(i, on, beats, quant);
            }
            (o, true) => return Err(format!("no op {o:?}")),
        }
        Ok(())
    }
}

fn fmt_beats(b: f64) -> String {
    if (b - b.round()).abs() < 1e-9 {
        format!("{b:.0}")
    } else {
        format!("{b}")
    }
}
