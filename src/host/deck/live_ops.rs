//! Live ops (beyond Swift; docs/ACTION-LOG.md): the arrangement moves that are no `DeckAction`
//! ([`ArrangeOp::Live`](crate::session::arrangement::ArrangeOp::Live)), applied to the
//! engine with the params of djapp's socket method of the same name, so an arrangement written
//! by hand (or recorded) plays the same live (`djapp_log play`) and offline
//! ([`render_arrangement`](super::offline::render_arrangement)).
//!
//! | method | params |
//! |---|---|
//! | `vocoder` | `instance` (0.., default 0: which vocoder; naming one adds it; `remove: true` takes it away), `modulator` (none, synth, deck:A, A, clip:ID, library:ID, capture:LABEL:BEATS, input:LABEL), `carrier` (the same), `swap` (true: the two change roles, after any picks), any knob by name (`mix`, `bands`, …, `vowel` also a letter), `knob` + `value` (0..1 travel), `on` / `insert` / `mute` / `pfl`, `notes` (numbers or names: `["A2","C3","E3"]`), `note` + `down` |
//! | `trick` | `deck`, `kind`, `action` on / off / toggle / knob (`value`), `quant`, the kind's args (`div`, `rise`, `amount`, `bars`, `source`, `beats`, `feedback`, `res`) |
//! | `keys` | `deck`, `note` (number or name), `velocity`, `down` (default true), `quant` (now / beat / bar): a key on the deck's live voice; or `mode` (live / transpose / td3 / toggle), `quant`, `deck_to` (A / B / aimed / next), `scale_lock`, `off` to set the route (docs/MPK.md) |
//! | `post` | `on`, `toggle`, `preset` (a name), `chain` (stages, JSON), `step` (±1): the live post chain on the master (docs/POST.md) |
//! | `library` | `action` load / play / stop, `id`: a capture-library loop onto the row, looping from the next downbeat, or stopped |
//!
//! Each is one match arm: a new method is a name in `LIVE_METHODS` and an arm here.

use crate::fx::tricks::TrickKind;
use crate::fx::vocoder::VocoderParam;
use crate::json::JsonValue;

use super::engine::Engine;
use super::tricks::{trick_args, TrickQuant};
use super::vocoder::{CarrierPick, ModulatorPick, VocoderSwitch};

pub(super) fn text(v: &JsonValue) -> String {
    match v {
        JsonValue::String(s) => s.clone(),
        other => other.js_stringified(),
    }
}

pub(super) fn num(p: &JsonValue, k: &str) -> Option<f64> {
    match p.get(k)? {
        JsonValue::Number(n) => Some(*n),
        JsonValue::String(s) => s.trim().parse().ok(),
        JsonValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

pub(super) fn flag(p: &JsonValue, k: &str) -> Option<bool> {
    match p.get(k)? {
        JsonValue::Bool(b) => Some(*b),
        JsonValue::Number(n) => Some(*n != 0.0),
        JsonValue::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "1" | "yes" => Some(true),
            "off" | "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A MIDI note: a number, or a name (`A2`, `C#4`, `Eb3`; C4 = 60).
pub fn note_of(v: &JsonValue) -> Option<u8> {
    if let Some(n) = v.as_f64() {
        return (0.0..=127.0).contains(&n).then_some(n.round() as u8);
    }
    let t = v.as_str()?.trim();
    if let Ok(n) = t.parse::<f64>() {
        return (0.0..=127.0).contains(&n).then_some(n.round() as u8);
    }
    let mut c = t.chars();
    let base: i32 = match c.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = c.collect();
    let (acc, oct) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, &rest[..]),
    };
    let o: i32 = oct.parse().ok()?;
    let n = (o + 1) * 12 + base + acc;
    (0..=127).contains(&n).then_some(n as u8)
}

impl Engine {
    /// Apply one live op (`method` with its socket params). Err says what was refused.
    pub fn apply_live_op(&mut self, method: &str, p: &JsonValue) -> Result<(), String> {
        match method {
            "vocoder" => self.live_vocoder(p),
            "trick" => self.live_trick(p),
            "library" => self.live_library(p),
            "freeze" => self.live_freeze(p),
            "input" => self.live_input(p),
            "aux" => self.live_aux(p),
            "post" => self.live_post(p),
            "keys" => self.live_keys(p),
            "cue" => self.live_cue(p),
            "nudge" => self.live_nudge(p),
            "jog" => self.live_jog(p),
            "history" => self.live_history(p),
            "capture" => self.live_capture(p),
            "loops" => self.live_loops(p),
            "source" => self.live_source(p),
            "pitch_range" => self.live_pitch_range(p),
            "loop_beats" => self.live_loop_beats(p),
            "seek" => self.live_seek(p),
            "autonext" => self.live_autonext(p),
            "transport" => self.live_transport(p),
            "target" => self.live_target(p),
            o => Err(format!("no live op {o:?}")),
        }
    }

    pub(super) fn input_named(&self, label: &str) -> Result<usize, String> {
        let l = label.trim();
        if let Ok(k) = l.parse::<usize>() {
            if k < self.inputs.len() {
                return Ok(k);
            }
        }
        self.inputs
            .iter()
            .position(|x| x.settings.label.eq_ignore_ascii_case(l))
            .ok_or_else(|| format!("no input strip {l:?}"))
    }

    fn live_mod_pick(&self, spec: &str) -> Result<ModulatorPick, String> {
        let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
        Ok(match kind.trim().to_lowercase().as_str() {
            "none" | "off" | "" => ModulatorPick::None,
            "synth" | "internal" => ModulatorPick::Synth,
            "deck" => ModulatorPick::Deck(rest.trim().to_uppercase()),
            "clip" => ModulatorPick::Clip(rest.to_string()),
            "library" => ModulatorPick::Library(rest.to_string()),
            "input" => ModulatorPick::Input(self.input_named(rest)?),
            // the last BEATS of an input's history (the strip's loop length without)
            "capture" => {
                let (label, beats) = match rest.split_once(':') {
                    Some((l, b)) => (l, b.trim().parse::<f64>().ok()),
                    None => (rest, None),
                };
                ModulatorPick::Capture(self.input_named(label)?, beats)
            }
            _ if rest.is_empty() && self.index(&spec.trim().to_uppercase()).is_some() => {
                ModulatorPick::Deck(spec.trim().to_uppercase())
            }
            _ => {
                return Err(format!(
                    "modulator {spec:?}: none, synth, deck:A, clip:ID, library:ID, capture:LABEL:BEATS or input:LABEL"
                ))
            }
        })
    }

    fn live_car_pick(&self, spec: &str) -> Result<CarrierPick, String> {
        let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
        Ok(match kind.trim().to_lowercase().as_str() {
            "none" | "off" | "" => CarrierPick::None,
            "synth" | "internal" => CarrierPick::Synth,
            "deck" => CarrierPick::Deck(rest.trim().to_uppercase()),
            "input" => CarrierPick::Input(self.input_named(rest)?),
            "clip" => CarrierPick::Clip(rest.to_string()),
            "library" => CarrierPick::Library(rest.to_string()),
            "capture" => {
                let (label, beats) = match rest.split_once(':') {
                    Some((l, b)) => (l, b.trim().parse::<f64>().ok()),
                    None => (rest, None),
                };
                CarrierPick::Capture(self.input_named(label)?, beats)
            }
            _ if rest.is_empty() && self.index(&spec.trim().to_uppercase()).is_some() => {
                CarrierPick::Deck(spec.trim().to_uppercase())
            }
            _ => {
                return Err(format!(
                    "carrier {spec:?}: none, synth, deck:A, clip:ID, library:ID, capture:LABEL:BEATS or input:LABEL"
                ))
            }
        })
    }

    /// `vocoder`: the sources first (so `on` in the same op turns on what they picked), then
    /// the knobs, the switches and the synth's notes, as djapp's socket method.
    ///
    /// `instance` (0.., default 0) names the vocoder; naming one not in use adds it, and
    /// `remove: true` takes it away again.
    fn live_vocoder(&mut self, p: &JsonValue) -> Result<(), String> {
        let k = num(p, "instance").map_or(0, |x| x.max(0.0) as usize);
        if flag(p, "remove") == Some(true) {
            return self.vocoder_remove(k);
        }
        let was = self.vocoder_enter(k)?;
        let r = self.live_vocoder_on(p);
        self.vocoder_leave(was);
        r
    }

    fn live_vocoder_on(&mut self, p: &JsonValue) -> Result<(), String> {
        if let Some(spec) = p.get("modulator").map(text) {
            let pick = self.live_mod_pick(&spec)?;
            self.vocoder_modulator(pick)?;
        }
        if let Some(spec) = p.get("carrier").map(text) {
            let pick = self.live_car_pick(&spec)?;
            self.vocoder_carrier(pick)?;
        }
        if flag(p, "swap") == Some(true) {
            self.vocoder_swap()?;
        }
        for param in VocoderParam::ALL {
            let v = match p.get(param.name()) {
                Some(JsonValue::String(t)) if param == VocoderParam::Vowel => {
                    match ["a", "e", "i", "o", "u"].iter().position(|x| t.eq_ignore_ascii_case(x)) {
                        Some(k) => Some(k as f64),
                        None => num(p, param.name()),
                    }
                }
                Some(_) => num(p, param.name()),
                None => None,
            };
            if let Some(v) = v {
                self.vocoder_set(param, v)
                    .ok_or_else(|| format!("{}: not a number", param.name()))?;
            }
        }
        if let Some(name) = p.get("knob").and_then(JsonValue::as_str) {
            let param = VocoderParam::parse(name).ok_or_else(|| format!("no knob {name:?}"))?;
            let v = num(p, "value").ok_or("knob: no value")?;
            self.vocoder_knob(param, v);
        }
        for name in ["on", "insert", "mute", "pfl", "bypass", "freeze", "xfade"] {
            if let Some(v) = flag(p, name) {
                self.vocoder_switch(VocoderSwitch::parse(name).expect("a switch"), v);
            }
        }
        if let Some(JsonValue::Array(ns)) = p.get("notes") {
            let notes = ns
                .iter()
                .map(|n| note_of(n).ok_or_else(|| format!("not a note: {}", n.js_stringified())))
                .collect::<Result<Vec<u8>, String>>()?;
            self.vocoder_set_notes(&notes);
        }
        if let Some(n) = p.get("note") {
            let n = note_of(n).ok_or_else(|| format!("not a note: {}", n.js_stringified()))?;
            self.vocoder_note(n, flag(p, "down").unwrap_or(true));
        }
        Ok(())
    }

    /// `trick`: fire (on / off / toggle) or turn the knob of one, as djapp's socket method.
    fn live_trick(&mut self, p: &JsonValue) -> Result<(), String> {
        let deck = p
            .get("deck")
            .and_then(JsonValue::as_str)
            .map(str::to_uppercase)
            .unwrap_or_else(|| self.focus.clone());
        if self.index(&deck).is_none() {
            return Err(format!("no deck {deck:?}"));
        }
        let name = p.get("kind").and_then(JsonValue::as_str).ok_or("trick: no kind")?;
        let kind = TrickKind::parse(name).ok_or_else(|| format!("no trick {name:?}"))?;
        let action = p.get("action").and_then(JsonValue::as_str).unwrap_or("on");
        if action == "knob" {
            let v = num(p, "value").ok_or("trick knob: no value")?;
            return self.trick_knob(&deck, kind, v).map(|_| ());
        }
        let on = match action {
            "on" => true,
            "off" => false,
            "toggle" => !self.trick_on(&deck, kind),
            o => return Err(format!("trick action {o:?}: on, off, toggle or knob")),
        };
        let get = |k: &str| p.get(k).map(text);
        let given = ["div", "rise", "amount", "bars", "source", "beats", "feedback", "res"]
            .iter()
            .any(|k| p.get(k).is_some());
        let base = self
            .deck(&deck)
            .and_then(|d| d.tricks.on.get(&kind).copied())
            .unwrap_or_else(|| kind.default_args());
        let names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        let args = given.then(|| trick_args(base, &get, &|d| names.iter().position(|n| n == d)));
        // a grid by name, or as a number of beats (what a recorded move carries)
        let quant = TrickQuant::parse(p.get("quant").map(text).as_deref());
        self.trick(&deck, kind, on, args, quant)
    }

    /// `input`: an input strip's knobs, as djapp's socket method: `trim_db`, `volume`, `mute`,
    /// `pfl`, `channel` (L, R, LR), `latency_ms`, `filter`, `low`, `mid`, `high`.
    fn live_input(&mut self, p: &JsonValue) -> Result<(), String> {
        use crate::fx::StripKnob;
        let label = p.get("input").map(text).ok_or("input: no input")?;
        let k = self.input_named(&label)?;
        if let Some(v) = num(p, "trim_db") {
            self.set_input_trim(k, v);
        }
        if let Some(v) = num(p, "volume") {
            self.set_input_volume(k, v);
        }
        if let Some(v) = flag(p, "mute") {
            self.set_input_mute(k, v);
        }
        if let Some(v) = flag(p, "pfl") {
            self.set_input_pfl(k, v);
        }
        if let Some(c) = p.get("channel").map(text) {
            use crate::input::InputChannel;
            let c = match c.to_uppercase().as_str() {
                "L" | "LEFT" => InputChannel::Left,
                "R" | "RIGHT" => InputChannel::Right,
                "LR" | "SUM" | "MIX" => InputChannel::Sum,
                o => return Err(format!("channel {o:?}: L, R or LR")),
            };
            self.set_input_channel(k, c);
        }
        if let Some(v) = num(p, "latency_ms") {
            self.set_input_latency(k, v);
        }
        for (name, knob) in [
            ("filter", StripKnob::Filter),
            ("low", StripKnob::EqLow),
            ("mid", StripKnob::EqMid),
            ("high", StripKnob::EqHigh),
        ] {
            if let Some(v) = num(p, name) {
                self.set_input_strip(k, knob, v);
            }
        }
        Ok(())
    }

    /// `freeze`: the granular freeze on input `input` (a label or index), as djapp's socket
    /// method: `quantize`, any knob by name in its units (before the freeze, so `beats` in the
    /// same move is what freezes), `knob` + `value` (0..1 travel), `action` on / off / toggle
    /// (`now`: not on the beat) or scrub (`frames` of the frozen audio, or `by` beats at the
    /// master tempo).
    fn live_freeze(&mut self, p: &JsonValue) -> Result<(), String> {
        use crate::fx::granular::FreezeParam;
        let label = p.get("input").map(text).ok_or("freeze: no input")?;
        let k = self.input_named(&label)?;
        if !self.has_history(k) {
            return Err(format!("input {label} keeps no history"));
        }
        if let Some(q) = flag(p, "quantize") {
            self.freeze_quantize(k, q);
        }
        let action = p.get("action").and_then(JsonValue::as_str).unwrap_or("set");
        for param in FreezeParam::ALL {
            // an on / off move's `beats` is the length it froze: set it with the freeze
            if param == FreezeParam::Beats && matches!(action, "on" | "off" | "toggle") {
                continue;
            }
            if let Some(v) = num(p, param.name()) {
                self.freeze_set(k, param, v)
                    .ok_or_else(|| format!("{}: not a number", param.name()))?;
            }
        }
        if let Some(name) = p.get("knob").and_then(JsonValue::as_str) {
            let param = FreezeParam::parse(name).ok_or_else(|| format!("no knob {name:?}"))?;
            self.freeze_knob(k, param, num(p, "value").ok_or("knob: no value")?);
        }
        let quantize = flag(p, "now").map(|n| !n);
        // the length it froze, when it is not the length set already (a set would restart the
        // grains for nothing)
        let beats = num(p, "beats").filter(|b| *b != self.inputs[k].freeze.beats);
        match action {
            "set" | "status" => {}
            "on" => {
                self.input_freeze(k, true, beats, quantize);
            }
            "off" => {
                self.input_freeze(k, false, beats, quantize);
            }
            "toggle" => {
                let on = !self.is_frozen(k);
                self.input_freeze(k, on, beats, quantize);
            }
            "scrub" => {
                let frames = match num(p, "frames") {
                    Some(f) => f,
                    None => {
                        let by = num(p, "by").ok_or("scrub: frames or by")?;
                        by * 60.0 / self.master_tempo().1 * self.sample_rate
                    }
                };
                self.freeze_scrub(k, frames);
            }
            o => {
                return Err(format!(
                    "freeze action {o:?}: set, on, off, toggle or scrub"
                ))
            }
        }
        Ok(())
    }

    /// `library`: a capture-library loop onto the row (`load`), looping from the next downbeat
    /// (`play`, loading it first if it is not on the row), or stopped (`stop`).
    fn live_library(&mut self, p: &JsonValue) -> Result<(), String> {
        let id = p.get("id").and_then(JsonValue::as_str).ok_or("library: no id")?.to_string();
        let action = p.get("action").and_then(JsonValue::as_str).unwrap_or("load");
        let ensure = |e: &mut Engine| -> Result<(), String> {
            if e.clip(&id).is_none() && !e.quietly(|e| e.library_load(&id)) {
                return Err(format!("no library loop {id:?}"));
            }
            Ok(())
        };
        match action {
            "load" => {
                // the op is the move: its own load is noted (once) by `library_load`
                if self.clip(&id).is_none() && !self.library_load(&id) {
                    return Err(format!("no library loop {id:?}"));
                }
                Ok(())
            }
            "save" => {
                if self.library_save(&id) {
                    Ok(())
                } else {
                    Err(format!("no loop {id:?} on the row to save (or no library)"))
                }
            }
            "play" => {
                ensure(self)?;
                self.set_looping(&id, true);
                Ok(())
            }
            "stop" => {
                self.set_looping(&id, false);
                Ok(())
            }
            o => Err(format!("library action {o:?}: load, play, stop or save")),
        }
    }
}
