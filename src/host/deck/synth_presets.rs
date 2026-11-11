//! The synth carrier's presets (beyond Swift; docs/VOCODER.md, "The synth"): the factory ones
//! in [`crate::fx::eurosynth::FACTORY_PRESETS`] and the user's, kept in a JSON file when the
//! host gives one ([`Engine::set_synth_preset_file`]).
//!
//! A preset holds every synth knob ([`VocoderParam::SYNTH`]): loading one sets them all at
//! once (one message to the render thread, one line in the action log); a knob a factory
//! preset leaves out takes the default patch's value. A user preset cannot take a factory
//! preset's name.
//!
//! The file: `{"presets": {"name": {"cutoff": 380, "resonance": 0.8, ...}, ...}}`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::fx::eurosynth::FACTORY_PRESETS;
use crate::fx::vocoder::{VocoderParam, VocoderSettings};
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::vocoder::VocoderMsg;

/// The user's presets and where they are kept.
#[derive(Clone, Debug, Default)]
pub struct SynthPresets {
    pub file: Option<PathBuf>,
    /// Name → (knob, value), in [`VocoderParam::SYNTH`] order.
    pub user: BTreeMap<String, Vec<(VocoderParam, f64)>>,
}

fn factory(name: &str) -> Option<Vec<(VocoderParam, f64)>> {
    let (_, knobs) = FACTORY_PRESETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))?;
    // the default patch, then what the preset says
    let mut s = VocoderSettings::default();
    for (k, v) in knobs.iter() {
        if let Some(p) = VocoderParam::parse(k) {
            s.set(p, *v);
        }
    }
    Some(VocoderParam::SYNTH.iter().map(|p| (*p, s.get(*p))).collect())
}

impl Engine {
    /// Keep user presets in `file` (None: for the session only), reading what is there.
    /// Returns how many it read.
    pub fn set_synth_preset_file(&mut self, file: Option<PathBuf>) -> usize {
        let mut user = BTreeMap::new();
        if let Some(f) = &file {
            if let Ok(text) = std::fs::read_to_string(f) {
                if let Some(JsonValue::Object(ps)) = JsonValue::parse(&text)
                    .ok()
                    .and_then(|j| j.get("presets").cloned())
                {
                    for (name, knobs) in ps.iter() {
                        let Some(o) = knobs.as_object() else { continue };
                        let mut s = VocoderSettings::default();
                        for (k, v) in o.iter() {
                            if let (Some(p), Some(x)) = (VocoderParam::parse(k), v.as_f64()) {
                                s.set(p, x);
                            }
                        }
                        user.insert(
                            name.to_string(),
                            VocoderParam::SYNTH.iter().map(|p| (*p, s.get(*p))).collect(),
                        );
                    }
                }
            }
        }
        let n = user.len();
        self.vocoder.presets.user = user;
        self.vocoder.presets.file = file;
        n
    }

    /// Every preset's name, factory ones first: (name, is it the user's).
    pub fn synth_preset_names(&self) -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = FACTORY_PRESETS
            .iter()
            .map(|(n, _)| (n.to_string(), false))
            .collect();
        v.extend(self.vocoder.presets.user.keys().map(|n| (n.clone(), true)));
        v
    }

    fn synth_preset_knobs(&self, name: &str) -> Option<Vec<(VocoderParam, f64)>> {
        factory(name).or_else(|| {
            let p = &self.vocoder.presets.user;
            p.get(name).cloned().or_else(|| {
                p.iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(name))
                    .map(|(_, k)| k.clone())
            })
        })
    }

    /// Load preset `name` (factory or the user's, any case): every synth knob at once.
    pub fn synth_preset_load(&mut self, name: &str) -> Result<(), String> {
        let knobs = self.synth_preset_knobs(name).ok_or_else(|| {
            let all: Vec<String> = self.synth_preset_names().into_iter().map(|(n, _)| n).collect();
            format!("no synth preset {name:?}; presets are {}", all.join(", "))
        })?;
        for (p, v) in knobs {
            self.vocoder.settings.set(p, v);
        }
        let s = self.vocoder.settings.clone();
        self.vocoder_send(VocoderMsg::Settings(s));
        let shown = self
            .synth_preset_names()
            .into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or(name.to_string(), |(n, _)| n);
        self.vocoder.preset = Some(shown.clone());
        self.vocoder_trace_synth("preset", format!("vocoder synth preset {shown}"));
        Ok(())
    }

    /// Save the synth's knobs now as the user's preset `name` (and to the file, if any).
    pub fn synth_preset_save(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a preset needs a name".into());
        }
        if factory(name).is_some() {
            return Err(format!("{name:?} is a factory preset: pick another name"));
        }
        let s = &self.vocoder.settings;
        let knobs = VocoderParam::SYNTH.iter().map(|p| (*p, s.get(*p))).collect();
        self.vocoder.presets.user.insert(name.to_string(), knobs);
        self.vocoder.preset = Some(name.to_string());
        self.write_synth_presets()?;
        self.vocoder_trace_text("preset_save", format!("vocoder synth preset saved: {name}"));
        Ok(())
    }

    /// Forget the user's preset `name`.
    pub fn synth_preset_delete(&mut self, name: &str) -> Result<(), String> {
        if self.vocoder.presets.user.remove(name).is_none() {
            return Err(format!("no user preset {name:?}"));
        }
        if self.vocoder.preset.as_deref() == Some(name) {
            self.vocoder.preset = None;
        }
        self.write_synth_presets()?;
        self.vocoder_trace_text("preset_delete", format!("vocoder synth preset deleted: {name}"));
        Ok(())
    }

    /// Step through the presets (`dir` ±1) from the one loaded last.
    pub fn synth_preset_step(&mut self, dir: i32) -> Result<(), String> {
        let names: Vec<String> = self.synth_preset_names().into_iter().map(|(n, _)| n).collect();
        let at = self
            .vocoder
            .preset
            .as_ref()
            .and_then(|c| names.iter().position(|n| n == c));
        let len = names.len() as i64;
        let k = match at {
            Some(i) => (i as i64 + if dir < 0 { -1 } else { 1 }).rem_euclid(len),
            None if dir < 0 => len - 1,
            None => 0,
        };
        self.synth_preset_load(&names[k as usize].clone())
    }

    fn write_synth_presets(&self) -> Result<(), String> {
        let Some(f) = &self.vocoder.presets.file else {
            return Ok(());
        };
        let mut ps = JsonObject::new();
        for (name, knobs) in &self.vocoder.presets.user {
            let mut o = JsonObject::new();
            for (p, v) in knobs {
                o.set(p.name(), JsonValue::from(libm_round3(*v)));
            }
            ps.set(name, JsonValue::Object(o));
        }
        let mut top = JsonObject::new();
        top.set("presets", JsonValue::Object(ps));
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = f.with_extension("json.tmp");
        std::fs::write(&tmp, JsonValue::Object(top).js_stringified())
            .and_then(|_| std::fs::rename(&tmp, f))
            .map_err(|e| format!("writing {}: {e}", f.display()))
    }
}

fn libm_round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
