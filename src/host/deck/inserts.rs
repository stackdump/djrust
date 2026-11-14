//! Insert effects on the mixer's strips and the master ([`crate::fx::insert`]): a deck ("A",
//! "B", …), an input strip ("input:<label>") or the master ("master") each may carry one chain
//! of filter -> crush -> delay -> reverb.
//!
//! Where each sits: a deck's after its EQ, tricks and strip reverb, before its fader, cue send
//! and aux send (the capture tap and LOOP record still hear the deck dry); an input strip's
//! before its fader and cue; the master's after the aux return and before the post chain and
//! the master level (an offline render's stems do not include it).
//!
//! The delay follows a tempo: a deck's chain its own deck's BPM; an input strip's and the
//! master's the lead deck's (the playing deck loudest in the mix, by fader and crossfader).
//!
//! Threading: a chain is built on the main thread the first time its target is touched
//! ([`InsertChain::new`] allocates) and sent boxed; the render thread keeps it for good (it is
//! never replaced, so the render thread never frees one). After that only the deltas travel:
//! a switch or a knob, a few bytes on the command ring. Nothing is built for a target never
//! touched, so a host that never uses inserts pays nothing.

use std::collections::BTreeMap;

use crate::fx::insert::{InsertChain, InsertKind, InsertParam, InsertSettings};
use crate::json::{JsonObject, JsonValue};

use super::engine::{Engine, Msg};

/// The master's target name.
pub const MASTER: &str = "master";
/// An input strip's target name is this followed by its label.
pub const INPUT_PREFIX: &str = "input:";

/// Where a chain sits, as the render thread numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertSlot {
    Deck(usize),
    Input(usize),
    Master,
}

/// Main -> render.
pub enum InsertMsg {
    /// The slot's chain (once; built on the main thread).
    Install(Box<InsertChain>),
    On(InsertKind, bool),
    Set(InsertParam, f64),
}

/// The render thread's chains, one optional slot per strip (made at construction: installing
/// fills a slot, it never grows a vector).
pub(crate) struct InsertRender {
    decks: Vec<Option<Box<InsertChain>>>,
    inputs: Vec<Option<Box<InsertChain>>>,
    master: Option<Box<InsertChain>>,
    /// The lead deck's tempo, for the inputs' and the master's delays.
    pub(crate) lead_bpm: f64,
}

impl InsertRender {
    pub(crate) fn new(decks: usize, inputs: usize) -> InsertRender {
        InsertRender {
            decks: (0..decks).map(|_| None).collect(),
            inputs: (0..inputs).map(|_| None).collect(),
            master: None,
            lead_bpm: 120.0,
        }
    }

    fn slot(&mut self, s: InsertSlot) -> Option<&mut Option<Box<InsertChain>>> {
        match s {
            InsertSlot::Deck(i) => self.decks.get_mut(i),
            InsertSlot::Input(i) => self.inputs.get_mut(i),
            InsertSlot::Master => Some(&mut self.master),
        }
    }

    /// Real-time safe. An `Install` into a slot that has a chain is refused by the main thread
    /// ([`InsertState::installed`]) before it is sent, so nothing is freed here.
    pub(crate) fn apply(&mut self, s: InsertSlot, m: InsertMsg) {
        let Some(slot) = self.slot(s) else { return };
        match m {
            InsertMsg::Install(c) => {
                if slot.is_none() {
                    *slot = Some(c);
                } else {
                    // never sent (see above); leak rather than free on the render thread
                    core::mem::forget(c);
                }
            }
            InsertMsg::On(k, on) => {
                if let Some(c) = slot.as_mut() {
                    c.set_on(k, on);
                }
            }
            InsertMsg::Set(p, v) => {
                if let Some(c) = slot.as_mut() {
                    c.set(p, v);
                }
            }
        }
    }

    /// Run slot `s`'s chain over a block, with the delay at `bpm` (None: the lead deck's).
    #[inline]
    pub(crate) fn process(&mut self, s: InsertSlot, bpm: Option<f64>, l: &mut [f32], r: &mut [f32]) {
        let lead = self.lead_bpm;
        let Some(Some(c)) = self.slot(s) else { return };
        c.set_bpm(bpm.unwrap_or(lead));
        if c.is_active() {
            c.process(l, r);
        }
    }
}

/// The main thread's half: each target's settings, and which slots have a chain.
#[derive(Default, Debug)]
pub struct InsertState {
    pub settings: BTreeMap<String, InsertSettings>,
    installed: Vec<String>,
}

impl Engine {
    /// Every target an insert chain may sit on: the decks, `input:<label>` per input strip,
    /// and `master`.
    pub fn insert_targets(&self) -> Vec<String> {
        self.decks
            .iter()
            .map(|d| d.name.clone())
            .chain(self.inputs.iter().map(|i| format!("{INPUT_PREFIX}{}", i.settings.label)))
            .chain(core::iter::once(MASTER.to_string()))
            .collect()
    }

    fn insert_slot(&self, target: &str) -> Option<InsertSlot> {
        if target == MASTER {
            return Some(InsertSlot::Master);
        }
        if let Some(label) = target.strip_prefix(INPUT_PREFIX) {
            return self.input_index(label).map(InsertSlot::Input);
        }
        self.index(target).map(InsertSlot::Deck)
    }

    /// `target`'s settings (the defaults, all off, for one never touched; None: no such
    /// target).
    pub fn insert_settings(&self, target: &str) -> Option<InsertSettings> {
        self.insert_slot(target)?;
        Some(self.inserts.settings.get(target).cloned().unwrap_or_default())
    }

    /// Make sure `target`'s chain is on the render thread, built to its settings.
    fn insert_ensure(&mut self, target: &str, slot: InsertSlot) -> bool {
        if self.inserts.installed.iter().any(|t| t == target) {
            return true;
        }
        let s = self.inserts.settings.get(target).cloned().unwrap_or_default();
        let mut chain = Box::new(InsertChain::new(self.sample_rate));
        chain.apply(&s, true);
        if self.send(Msg::Insert(slot, InsertMsg::Install(chain))) {
            self.inserts.installed.push(target.to_string());
            true
        } else {
            false
        }
    }

    /// Switch one effect of `target`'s chain on or off. False: no such target, or the ring
    /// was full (the setting is kept either way for a known target).
    pub fn set_insert_on(&mut self, target: &str, kind: InsertKind, on: bool) -> bool {
        let Some(slot) = self.insert_slot(target) else { return false };
        self.inserts.settings.entry(target.to_string()).or_default().set_on(kind, on);
        let fresh = !self.inserts.installed.iter().any(|t| t == target);
        if fresh {
            // a chain's first touch: it is built with the setting already in it
            if !on {
                return true;
            }
            return self.insert_ensure(target, slot);
        }
        self.send(Msg::Insert(slot, InsertMsg::On(kind, on)))
    }

    /// Move one knob of `target`'s chain (0...1).
    pub fn set_insert_param(&mut self, target: &str, param: InsertParam, v: f64) -> bool {
        let Some(slot) = self.insert_slot(target) else { return false };
        let s = self.inserts.settings.entry(target.to_string()).or_default();
        s.set(param, v);
        let v = s.get(param);
        if !self.inserts.installed.iter().any(|t| t == target) {
            // nothing sounds until a switch is on; the knob is kept for when one is
            return true;
        }
        self.send(Msg::Insert(slot, InsertMsg::Set(param, v)))
    }

    /// Set every switch and knob of `target` (a restore): the knobs glide there.
    pub fn apply_insert_settings(&mut self, target: &str, s: &InsertSettings) -> bool {
        let Some(slot) = self.insert_slot(target) else { return false };
        let installed = self.inserts.installed.iter().any(|t| t == target);
        self.inserts.settings.insert(target.to_string(), s.clone());
        if !installed {
            return !s.any_on() || self.insert_ensure(target, slot);
        }
        let mut ok = true;
        for p in InsertParam::ALL {
            ok &= self.send(Msg::Insert(slot, InsertMsg::Set(p, s.get(p))));
        }
        for k in InsertKind::ALL {
            ok &= self.send(Msg::Insert(slot, InsertMsg::On(k, s.is_on(k))));
        }
        ok
    }

    /// Every target's switches off (the tails ring out).
    pub fn inserts_off(&mut self) {
        for t in self.insert_targets() {
            for k in InsertKind::ALL {
                if self.inserts.settings.get(&t).is_some_and(|s| s.is_on(k)) {
                    self.set_insert_on(&t, k, false);
                }
            }
        }
    }

    /// Every target's settings that were touched, as `{"A": {…}, "master": {…}}`.
    pub fn inserts_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for (t, s) in &self.inserts.settings {
            o.set(t.clone(), s.json());
        }
        JsonValue::Object(o)
    }

    /// Restore what [`inserts_json`](Self::inserts_json) wrote; targets this engine does not
    /// have are skipped. Returns the targets restored.
    pub fn restore_inserts_json(&mut self, v: &JsonValue) -> Vec<String> {
        let Some(o) = v.as_object() else { return Vec::new() };
        let mut done = Vec::new();
        for t in self.insert_targets() {
            if let Some(x) = o.get(&t) {
                let s = InsertSettings::from_json(Some(x));
                if self.apply_insert_settings(&t, &s) {
                    done.push(t);
                }
            }
        }
        done
    }
}
