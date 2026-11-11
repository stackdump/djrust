//! The project model, port of `Beats/PFlow.swift` (beats' public/lib/pflow.js:
//! parse, projectToJSON, durations, model CID).
//!
//! As in Swift, `recomputeLayout` and the runtime half of a net (arc index, enabling,
//! firing — the wave engine) are not here. Coordinates never reach `to_json` or the
//! CID. String sorts are UTF-16 code-unit order (JS `<`), mirrored from the JS.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cid;
use crate::json::{array_index, js_round, utf16_cmp, JsonObject, JsonValue};
/// What one Hit fires (`StingerAssignment` in `StingerCuration.swift`): the one type, which
/// `sound::stinger` resolves and `Project.stingers` holds.
pub use crate::sound::stinger::StingerAssignment;

fn js_round_int(d: f64) -> i64 {
    let r = js_round(d);
    if !r.is_finite() {
        return 0;
    }
    r.clamp(-9e18, 9e18) as i64
}

/// A string-keyed map that iterates the way a JS object does (integer keys first).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct OrderedMap<V> {
    keys: Vec<String>,
    storage: BTreeMap<String, V>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            keys: Vec::new(),
            storage: BTreeMap::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, key: &str) -> Option<&V> {
        self.storage.get(key)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.storage.get_mut(key)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.storage.contains_key(key)
    }
    pub fn insert(&mut self, key: impl Into<String>, value: V) {
        let key = key.into();
        if self.storage.insert(key.clone(), value).is_none() {
            let is_index = array_index(&key).is_some();
            self.keys.push(key);
            if is_index {
                let mut ints: Vec<String> = self
                    .keys
                    .iter()
                    .filter(|k| array_index(k).is_some())
                    .cloned()
                    .collect();
                ints.sort_by_key(|k| array_index(k).unwrap());
                let rest = self
                    .keys
                    .iter()
                    .filter(|k| array_index(k).is_none())
                    .cloned();
                self.keys = ints.into_iter().chain(rest).collect();
            }
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let v = self.storage.remove(key)?;
        self.keys.retain(|k| k != key);
        Some(v)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.keys
            .iter()
            .map(move |k| (k.as_str(), &self.storage[k]))
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.keys.iter().map(move |k| &self.storage[k])
    }
}

// ---------------------------------------------------------------------------
// Model

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Place {
    pub initial: Vec<f64>,
    pub x: f64,
    pub y: f64,
    pub label: Option<String>,
}

impl Default for Place {
    fn default() -> Self {
        Place {
            initial: alloc::vec![0.0],
            x: 0.0,
            y: 0.0,
            label: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Transition {
    pub x: f64,
    pub y: f64,
    pub label: Option<String>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Arc {
    pub source: String,
    pub target: String,
    pub weight: Vec<f64>,
    pub inhibit: bool,
}

impl Arc {
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Arc {
            source: source.into(),
            target: target.into(),
            weight: alloc::vec![1.0],
            inhibit: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct MidiBinding {
    pub note: i64,
    pub channel: i64,
    pub velocity: i64,
    /// Legacy milliseconds.
    pub duration: i64,
    /// Sixteenth steps; canonical when > 0.
    pub duration_steps: i64,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct ControlBinding {
    pub action: String,
    pub target_net: String,
    pub target_note: i64,
    pub macro_name: Option<String>,
    pub macro_bars: Option<f64>,
    pub macro_params: Option<JsonValue>,
}

impl ControlBinding {
    pub fn new(action: &str, target: &str) -> Self {
        ControlBinding {
            action: action.to_string(),
            target_net: target.to_string(),
            target_note: 0,
            macro_name: None,
            macro_bars: None,
            macro_params: None,
        }
    }
}

/// What a net plays, as its palette and the user's choices resolve it (`TrackSound` in
/// `PaletteSound.swift`). dj-only: never encoded, so the canonical text and CID stay beats'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct TrackSound {
    /// The user's own option for this net's role (the `tracks` override `voice`).
    pub override_: Option<String>,
    /// The palette option it plays (`SoundPalette` grammar).
    pub option: Option<String>,
    /// A drum net's kit from the palette.
    pub kit: Option<String>,
    /// The composer's instrument, kept when a pick or a palette's synth voice replaced it.
    pub composed: Option<String>,
    /// A stinger net's curated one-shot.
    pub curated: Option<String>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Track {
    pub channel: i64,
    pub default_velocity: i64,
    pub instrument: String,
    /// The instrument was chosen by the user (a share `tracks` override), not the composer.
    /// dj-only, never encoded.
    pub picked: bool,
    /// The drum kit this net's drum lanes play (a `tracks` override's `kit`); None = beats'
    /// synth kit. dj-only, never encoded.
    pub kit: Option<String>,
    /// The sound palette this net's role plays from (a `tracks` override's `palette`).
    /// dj-only, never encoded.
    pub palette: Option<String>,
    /// What the palette (or the user's role override) resolved this net to play. dj-only.
    pub sound: TrackSound,
    pub instrument_set: Vec<JsonValue>,
    pub mix: Option<JsonValue>,
    pub group: Option<String>,
    pub generator: Option<String>,
    pub ring_size: Option<f64>,
    pub beats: Option<f64>,
    pub rotation: Option<f64>,
    pub note: Option<f64>,
    pub generator_params: Option<JsonValue>,
}

impl Default for Track {
    fn default() -> Self {
        Track {
            channel: 1,
            default_velocity: 100,
            instrument: String::new(),
            picked: false,
            kit: None,
            palette: None,
            sound: TrackSound::default(),
            instrument_set: Vec::new(),
            mix: None,
            group: None,
            generator: None,
            ring_size: None,
            beats: None,
            rotation: None,
            note: None,
            generator_params: None,
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct NetBundle {
    pub places: OrderedMap<Place>,
    pub transitions: OrderedMap<Transition>,
    pub arcs: Vec<Arc>,
    pub track: Track,
    pub role: String,
    pub riff_group: String,
    pub riff_variant: String,
    pub bindings: OrderedMap<MidiBinding>,
    pub control_bindings: OrderedMap<ControlBinding>,
    /// For a net from an unparsed wire document: the raw ms `stamp_duration_steps`
    /// stamps from, per binding. None for parsed nets. Not part of the model.
    pub wire_duration_ms: Option<BTreeMap<String, f64>>,
}

impl Default for NetBundle {
    fn default() -> Self {
        NetBundle {
            places: OrderedMap::new(),
            transitions: OrderedMap::new(),
            arcs: Vec::new(),
            track: Track::default(),
            role: "music".into(),
            riff_group: String::new(),
            riff_variant: String::new(),
            bindings: OrderedMap::new(),
            control_bindings: OrderedMap::new(),
            wire_duration_ms: None,
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Section {
    pub name: String,
    pub steps: i64,
    pub phrases: OrderedMap<Vec<String>>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Project {
    pub name: String,
    pub tempo: f64,
    pub swing: f64,
    pub humanize: f64,
    pub root_note: Option<i64>,
    pub scale_name: String,
    pub bars: Option<i64>,
    pub nets: OrderedMap<NetBundle>,
    pub structure: Vec<Section>,
    pub initial_mutes: Vec<String>,
    /// Set by the composer when the cohesion-v2 engine ran; not part of the model CID.
    pub cohesion: Option<String>,
    /// What the deck's Hit 1–4 fire (`curateStingers`), empty = each Hit's stinger net as
    /// composed. dj-only, never encoded.
    pub stingers: Vec<StingerAssignment>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            name: "Untitled".into(),
            tempo: 120.0,
            swing: 0.0,
            humanize: 0.0,
            root_note: None,
            scale_name: String::new(),
            bars: None,
            nets: OrderedMap::new(),
            structure: Vec::new(),
            initial_mutes: Vec::new(),
            cohesion: None,
            stingers: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing

fn get_string(m: Option<&JsonValue>, key: &str, def: &str) -> String {
    m.and_then(|m| m.get(key))
        .and_then(JsonValue::as_str)
        .unwrap_or(def)
        .to_string()
}
fn get_float(m: Option<&JsonValue>, key: &str, def: f64) -> f64 {
    m.and_then(|m| m.get(key))
        .and_then(JsonValue::as_f64)
        .unwrap_or(def)
}
fn get_int(m: Option<&JsonValue>, key: &str, def: i64) -> i64 {
    match m.and_then(|m| m.get(key)).and_then(JsonValue::as_f64) {
        Some(d) => js_round_int(d),
        None => def,
    }
}
fn get_float_array(m: Option<&JsonValue>, key: &str, def: &[f64]) -> Vec<f64> {
    match m.and_then(|m| m.get(key)).and_then(JsonValue::as_array) {
        Some(a) => a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect(),
        None => def.to_vec(),
    }
}

impl NetBundle {
    /// `parseNetBundle`
    pub fn from_json(data: &JsonValue) -> NetBundle {
        let d = Some(data);
        let mut nb = NetBundle {
            role: get_string(d, "role", "music"),
            riff_group: get_string(d, "riffGroup", ""),
            riff_variant: get_string(d, "riffVariant", ""),
            ..Default::default()
        };
        let t = data.get("track");
        nb.track.channel = get_int(t, "channel", 1);
        nb.track.default_velocity = get_int(t, "defaultVelocity", 100);
        nb.track.instrument = get_string(t, "instrument", "");
        nb.track.instrument_set = t
            .and_then(|t| t.get("instrumentSet"))
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(mix) = t.and_then(|t| t.get("mix")) {
            if mix.is_truthy() {
                nb.track.mix = Some(mix.clone());
            }
        }
        if let Some(g) = t.and_then(|t| t.get("group")).and_then(JsonValue::as_str) {
            if !g.is_empty() {
                nb.track.group = Some(g.to_string());
            }
        }
        if let Some(g) = t
            .and_then(|t| t.get("generator"))
            .and_then(JsonValue::as_str)
        {
            nb.track.generator = Some(g.to_string());
        }
        let num = |k: &str| t.and_then(|t| t.get(k)).and_then(JsonValue::as_f64);
        nb.track.ring_size = num("ringSize");
        nb.track.beats = num("beats");
        nb.track.rotation = num("rotation");
        nb.track.note = num("note");
        if let Some(gp) = t.and_then(|t| t.get("generatorParams")) {
            if gp.is_object_like() {
                nb.track.generator_params = Some(gp.clone());
            }
        }

        if let Some(places) = data.get("places").and_then(JsonValue::as_object) {
            for (id, p) in places.iter() {
                let p = Some(p);
                let label = get_string(p, "label", "");
                nb.places.insert(
                    id,
                    Place {
                        initial: get_float_array(p, "initial", &[0.0]),
                        x: get_float(p, "x", 0.0),
                        y: get_float(p, "y", 0.0),
                        label: (!label.is_empty()).then_some(label),
                    },
                );
            }
        }

        if let Some(transitions) = data.get("transitions").and_then(JsonValue::as_object) {
            for (id, td) in transitions.iter() {
                let tdo = Some(td);
                let label = get_string(tdo, "label", "");
                nb.transitions.insert(
                    id,
                    Transition {
                        x: get_float(tdo, "x", 0.0),
                        y: get_float(tdo, "y", 0.0),
                        label: (!label.is_empty()).then_some(label),
                    },
                );
                if let Some(m) = td.get("midi") {
                    if m.is_object_like() {
                        let b = nb.midi(m);
                        nb.bindings.insert(id, b);
                    }
                }
                if let Some(c) = td.get("control") {
                    if c.is_object_like() {
                        let co = Some(c);
                        let mut cb = ControlBinding {
                            action: get_string(co, "action", "toggle-track"),
                            target_net: get_string(co, "targetNet", ""),
                            target_note: get_int(co, "targetNote", 0),
                            macro_name: None,
                            macro_bars: None,
                            macro_params: None,
                        };
                        let m = get_string(co, "macro", "");
                        if !m.is_empty() {
                            cb.macro_name = Some(m);
                        }
                        let bars = get_float(co, "macroBars", 0.0);
                        if bars != 0.0 {
                            cb.macro_bars = Some(bars);
                        }
                        if let Some(mp) = c.get("macroParams") {
                            if mp.is_object_like() {
                                cb.macro_params = Some(mp.clone());
                            }
                        }
                        nb.control_bindings.insert(id, cb);
                    }
                }
            }
        }

        for a in data
            .get("arcs")
            .and_then(JsonValue::as_array)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
        {
            let ao = Some(a);
            nb.arcs.push(Arc {
                source: get_string(ao, "source", ""),
                target: get_string(ao, "target", ""),
                weight: get_float_array(ao, "weight", &[1.0]),
                inhibit: a
                    .get("inhibit")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false),
            });
        }

        // Re-parse fallback: a previously parsed bundle carries bindings as siblings.
        if let Some(b) = data.get("bindings").and_then(JsonValue::as_object) {
            for (id, m) in b.iter() {
                if !nb.bindings.contains_key(id) && m.is_truthy() && m.is_object_like() {
                    let mb = nb.midi(m);
                    nb.bindings.insert(id, mb);
                }
            }
        }
        if let Some(cbs) = data.get("controlBindings").and_then(JsonValue::as_object) {
            for (id, c) in cbs.iter() {
                if !nb.control_bindings.contains_key(id) && c.is_truthy() && c.is_object_like() {
                    let co = Some(c);
                    let mut cb = ControlBinding {
                        action: get_string(co, "action", "toggle-track"),
                        target_net: get_string(co, "targetNet", ""),
                        target_note: get_int(co, "targetNote", 0),
                        macro_name: None,
                        macro_bars: None,
                        macro_params: None,
                    };
                    if c.get("macro").is_some_and(JsonValue::is_truthy) {
                        cb.macro_name = Some(get_string(co, "macro", ""));
                    }
                    if c.get("macroBars").is_some_and(JsonValue::is_truthy) {
                        cb.macro_bars = Some(get_float(co, "macroBars", 0.0));
                    }
                    if let Some(mp) = c.get("macroParams") {
                        if mp.is_object_like() {
                            cb.macro_params = Some(mp.clone());
                        }
                    }
                    nb.control_bindings.insert(id, cb);
                }
            }
        }
        nb
    }

    fn midi(&self, m: &JsonValue) -> MidiBinding {
        let m = Some(m);
        MidiBinding {
            note: get_int(m, "note", 60),
            channel: get_int(m, "channel", self.track.channel),
            velocity: get_int(m, "velocity", self.track.default_velocity),
            duration: get_int(m, "duration", 100),
            duration_steps: get_int(m, "durationSteps", 0),
        }
    }
}

impl Project {
    /// `parseProject`
    pub fn from_json(data: &JsonValue) -> Project {
        let d = Some(data);
        let mut p = Project {
            name: get_string(d, "name", "Untitled"),
            tempo: get_float(d, "tempo", 120.0),
            swing: get_float(d, "swing", 0.0),
            humanize: get_float(d, "humanize", 0.0),
            scale_name: get_string(d, "scaleName", ""),
            ..Default::default()
        };
        if data.get("rootNote").is_some_and(|r| *r != JsonValue::Null) {
            let v = js_round_int(get_float(d, "rootNote", -1.0));
            p.root_note = (v >= 0).then_some(v);
        }
        if data.get("bars").is_some_and(|b| *b != JsonValue::Null) {
            p.bars = Some(js_round_int(get_float(d, "bars", 0.0)).max(0));
        }
        if let Some(nets) = data.get("nets").and_then(JsonValue::as_object) {
            for (id, n) in nets.iter() {
                if n.is_object_like() {
                    p.nets.insert(id, NetBundle::from_json(n));
                }
            }
        }
        for s in data
            .get("structure")
            .and_then(JsonValue::as_array)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
        {
            if s.as_object().is_none() {
                continue;
            }
            let so = Some(s);
            let mut sec = Section {
                name: get_string(so, "name", ""),
                steps: js_round_int(get_float(so, "steps", 0.0)),
                phrases: OrderedMap::new(),
            };
            if let Some(ph) = s.get("phrases").and_then(JsonValue::as_object) {
                for (role, arr) in ph.iter() {
                    if let Some(a) = arr.strings() {
                        sec.phrases.insert(role, a);
                    }
                }
            }
            p.structure.push(sec);
        }
        p.initial_mutes = data
            .get("initialMutes")
            .and_then(JsonValue::strings)
            .unwrap_or_default();
        p
    }
}

/// JS `v > 0` for a raw JSON value (ToNumber on strings and booleans).
fn js_greater_than_zero(v: Option<&JsonValue>) -> bool {
    match v {
        Some(JsonValue::Number(d)) => *d > 0.0,
        Some(JsonValue::Bool(b)) => *b,
        Some(JsonValue::String(s)) => {
            let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            let n = if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            };
            n > 0.0
        }
        _ => false,
    }
}

impl Project {
    /// `Project(wireJSON:)`: a project built from an unparsed wire document (a share's raw
    /// `nets`, as the worker's `parseProject` sees it), remembering for each MIDI binding the
    /// raw ms the JS stamp would use: it only touches `transitions[].midi` (or sibling
    /// `bindings`) whose JSON carries a finite positive `duration` and no positive
    /// `durationSteps`, and it stamps from the raw (possibly fractional) ms, before the
    /// parse rounds it.
    pub fn from_wire_json(doc: &JsonValue) -> Project {
        let mut p = Project::from_json(doc);
        let Some(nets) = doc.get("nets").and_then(JsonValue::as_object) else {
            return p;
        };
        for (id, nd) in nets.iter() {
            let Some(nb) = p.nets.get_mut(id) else {
                continue;
            };
            fn stampable(m: &JsonValue) -> Option<f64> {
                if js_greater_than_zero(m.get("durationSteps")) {
                    return None;
                }
                let d = m.get("duration").and_then(JsonValue::as_f64)?;
                (d.is_finite() && d > 0.0).then_some(d)
            }
            let mut ms: BTreeMap<String, f64> = BTreeMap::new();
            let mut from_midi: alloc::collections::BTreeSet<String> = Default::default();
            if let Some(ts) = nd.get("transitions").and_then(JsonValue::as_object) {
                for (tid, t) in ts.iter() {
                    let Some(m) = t.get("midi").filter(|m| m.is_object_like()) else {
                        continue;
                    };
                    from_midi.insert(tid.to_string());
                    if let Some(d) = stampable(m) {
                        ms.insert(tid.to_string(), d);
                    }
                }
            }
            if let Some(bs) = nd.get("bindings").and_then(JsonValue::as_object) {
                for (bid, m) in bs.iter() {
                    if from_midi.contains(bid) {
                        continue;
                    }
                    if let Some(d) = stampable(m) {
                        ms.insert(bid.to_string(), d);
                    }
                }
            }
            nb.wire_duration_ms = Some(ms);
        }
        p
    }
}

// ---------------------------------------------------------------------------
// Serialization (projectToJSON)

fn strs(v: &[String]) -> JsonValue {
    JsonValue::Array(v.iter().map(|s| JsonValue::String(s.clone())).collect())
}

fn nums(v: &[f64]) -> JsonValue {
    JsonValue::Array(v.iter().map(|d| JsonValue::Number(*d)).collect())
}

impl Project {
    /// `projectToJSON`: key order and omission rules follow the JS exactly.
    pub fn to_json(&self) -> JsonValue {
        let mut r = JsonObject::new();
        r.set("name", self.name.as_str().into());
        r.set("tempo", self.tempo.into());
        r.set("nets", JsonValue::Null); // position fixed; filled below
        if self.swing > 0.0 {
            r.set("swing", self.swing.into());
        }
        if self.humanize > 0.0 {
            r.set("humanize", self.humanize.into());
        }
        if let Some(root) = self.root_note {
            r.set("rootNote", root.into());
        }
        if !self.scale_name.is_empty() {
            r.set("scaleName", self.scale_name.as_str().into());
        }
        if let Some(bars) = self.bars {
            if bars > 0 {
                r.set("bars", bars.into());
            }
        }
        let mut nets = JsonObject::new();
        for (id, nb) in self.nets.iter() {
            nets.set(id, nb.to_json());
        }
        r.set("nets", nets.into());
        if !self.initial_mutes.is_empty() {
            r.set("initialMutes", strs(&self.initial_mutes));
        }
        if !self.structure.is_empty() {
            let secs = self
                .structure
                .iter()
                .map(|sec| {
                    let mut s = JsonObject::new();
                    s.set("name", sec.name.as_str().into());
                    s.set("steps", sec.steps.into());
                    if !sec.phrases.is_empty() {
                        let mut ph = JsonObject::new();
                        for (k, v) in sec.phrases.iter() {
                            ph.set(k, strs(v));
                        }
                        s.set("phrases", ph.into());
                    }
                    s.into()
                })
                .collect();
            r.set("structure", JsonValue::Array(secs));
        }
        r.into()
    }
}

impl NetBundle {
    pub fn to_json(&self) -> JsonValue {
        let t = &self.track;
        let mut tm = JsonObject::new();
        tm.set("channel", t.channel.into());
        if t.default_velocity != 100 {
            tm.set("defaultVelocity", t.default_velocity.into());
        }
        if !t.instrument.is_empty() {
            tm.set("instrument", t.instrument.as_str().into());
        }
        if !t.instrument_set.is_empty() {
            tm.set("instrumentSet", JsonValue::Array(t.instrument_set.clone()));
        }
        if let Some(mix) = &t.mix {
            tm.set("mix", mix.clone());
        }
        if let Some(g) = t.generator.as_deref().filter(|g| !g.is_empty()) {
            tm.set("generator", g.into());
        }
        if let Some(v) = t.ring_size {
            tm.set("ringSize", v.into());
        }
        if let Some(v) = t.beats {
            tm.set("beats", v.into());
        }
        if let Some(v) = t.rotation.filter(|v| *v != 0.0) {
            tm.set("rotation", v.into());
        }
        if let Some(v) = t.note {
            tm.set("note", v.into());
        }
        if let Some(gp) = &t.generator_params {
            tm.set("generatorParams", gp.clone());
        }
        if let Some(g) = &t.group {
            tm.set("group", g.as_str().into());
        }

        let mut r = JsonObject::new();
        r.set("track", tm.into());
        if !self.role.is_empty() && self.role != "music" {
            r.set("role", self.role.as_str().into());
        }
        if !self.riff_group.is_empty() {
            r.set("riffGroup", self.riff_group.as_str().into());
        }
        if !self.riff_variant.is_empty() {
            r.set("riffVariant", self.riff_variant.as_str().into());
        }

        let mut pl = JsonObject::new();
        for (label, place) in self.places.iter() {
            let mut p = JsonObject::new();
            if place.initial.iter().sum::<f64>() > 0.0 {
                p.set("initial", nums(&place.initial));
            }
            if let Some(l) = &place.label {
                p.set("label", l.as_str().into());
            }
            pl.set(label, p.into());
        }
        r.set("places", pl.into());

        let mut tr = JsonObject::new();
        for (label, trans) in self.transitions.iter() {
            let mut o = JsonObject::new();
            if let Some(l) = &trans.label {
                o.set("label", l.as_str().into());
            }
            if let Some(m) = self.bindings.get(label) {
                let mut midi = JsonObject::new();
                midi.set("note", m.note.into());
                if m.channel != t.channel {
                    midi.set("channel", m.channel.into());
                }
                if m.velocity != t.default_velocity {
                    midi.set("velocity", m.velocity.into());
                }
                if m.duration_steps > 0 {
                    midi.set("durationSteps", m.duration_steps.into());
                } else if m.duration != 100 {
                    midi.set("duration", m.duration.into());
                }
                o.set("midi", midi.into());
            }
            if let Some(c) = self.control_bindings.get(label) {
                let mut cm = JsonObject::new();
                cm.set("action", c.action.as_str().into());
                if !c.target_net.is_empty() {
                    cm.set("targetNet", c.target_net.as_str().into());
                }
                if c.target_note > 0 {
                    cm.set("targetNote", c.target_note.into());
                }
                if let Some(m) = c.macro_name.as_deref().filter(|m| !m.is_empty()) {
                    cm.set("macro", m.into());
                }
                if let Some(b) = c.macro_bars.filter(|b| *b != 0.0) {
                    cm.set("macroBars", b.into());
                }
                if let Some(mp) = &c.macro_params {
                    cm.set("macroParams", mp.clone());
                }
                o.set("control", cm.into());
            }
            tr.set(label, o.into());
        }
        r.set("transitions", tr.into());

        let arcs = self
            .arcs
            .iter()
            .map(|arc| {
                let mut a = JsonObject::new();
                a.set("source", arc.source.as_str().into());
                a.set("target", arc.target.as_str().into());
                if !(arc.weight.len() == 1 && arc.weight[0] == 1.0) {
                    a.set("weight", nums(&arc.weight));
                }
                if arc.inhibit {
                    a.set("inhibit", true.into());
                }
                a.into()
            })
            .collect();
        r.set("arcs", JsonValue::Array(arcs));
        r.into()
    }

    /// `normalizeProjectForCID`'s per-net half.
    pub fn normalized_for_cid(&self) -> JsonValue {
        let mut n = JsonObject::new();
        if !self.role.is_empty() && self.role != "music" {
            n.set("role", self.role.as_str().into());
        }
        if !self.riff_group.is_empty() {
            n.set("riffGroup", self.riff_group.as_str().into());
        }
        let mut pl = JsonObject::new();
        let mut ids: Vec<&String> = self.places.keys().iter().collect();
        ids.sort_by(|a, b| utf16_cmp(a, b));
        for id in ids {
            let mut p = JsonObject::new();
            p.set("initial", nums(&self.places.get(id).unwrap().initial));
            pl.set(id.as_str(), p.into());
        }
        n.set("places", pl.into());

        let mut tr = JsonObject::new();
        let mut ids: Vec<&String> = self.transitions.keys().iter().collect();
        ids.sort_by(|a, b| utf16_cmp(a, b));
        for id in ids {
            let mut o = JsonObject::new();
            if let Some(m) = self.bindings.get(id) {
                let mut midi = JsonObject::new();
                midi.set("note", m.note.into());
                midi.set("velocity", m.velocity.into());
                midi.set("channel", m.channel.into());
                if m.duration_steps > 0 {
                    midi.set("durationSteps", m.duration_steps.into());
                } else {
                    midi.set("duration", m.duration.into());
                }
                o.set("midi", midi.into());
            }
            if let Some(c) = self.control_bindings.get(id) {
                let mut ctrl = JsonObject::new();
                ctrl.set("action", c.action.as_str().into());
                if !c.target_net.is_empty() {
                    ctrl.set("targetNet", c.target_net.as_str().into());
                }
                o.set("control", ctrl.into());
            }
            tr.set(id.as_str(), o.into());
        }
        n.set("transitions", tr.into());

        // Stable sort by (source, target): equal keys keep document order.
        let mut sorted: Vec<&Arc> = self.arcs.iter().collect();
        sorted.sort_by(|a, b| {
            utf16_cmp(&a.source, &b.source).then_with(|| utf16_cmp(&a.target, &b.target))
        });
        let arcs = sorted
            .into_iter()
            .map(|arc| {
                let mut a = JsonObject::new();
                a.set("source", arc.source.as_str().into());
                a.set("target", arc.target.as_str().into());
                a.set("weight", nums(&arc.weight));
                if arc.inhibit {
                    a.set("inhibit", true.into());
                }
                a.into()
            })
            .collect();
        n.set("arcs", JsonValue::Array(arcs));
        n.into()
    }
}

impl Project {
    /// `normalizeProjectForCID`
    pub fn normalized_for_cid(&self) -> JsonValue {
        let mut r = JsonObject::new();
        r.set("name", self.name.as_str().into());
        r.set("tempo", self.tempo.into());
        if self.swing > 0.0 {
            r.set("swing", self.swing.into());
        }
        if self.humanize > 0.0 {
            r.set("humanize", self.humanize.into());
        }
        let mut n = JsonObject::new();
        let mut ids: Vec<&String> = self.nets.keys().iter().collect();
        ids.sort_by(|a, b| utf16_cmp(a, b));
        for id in ids {
            n.set(id.as_str(), self.nets.get(id).unwrap().normalized_for_cid());
        }
        r.set("nets", n.into());
        r.into()
    }

    /// `canonicalProjectJSON`: the exact bytes the model CID hashes.
    pub fn canonical_json(&self) -> String {
        self.normalized_for_cid().canonical_go()
    }

    /// Hex sha256 of `canonical_json` (internal/pflow/cid.go `Project.CID`).
    pub fn model_cid(&self) -> String {
        cid::model_cid(&self.canonical_json())
    }

    /// `stampDurationSteps`: idempotent; stamps at the authored tempo.
    pub fn stamp_duration_steps(&mut self) {
        let bpm = if self.tempo > 0.0 { self.tempo } else { 120.0 };
        let ids: Vec<String> = self.nets.keys().to_vec();
        for id in ids {
            let nb = self.nets.get_mut(&id).unwrap();
            let bids: Vec<String> = nb.bindings.keys().to_vec();
            for bid in bids {
                let mut m = *nb.bindings.get(&bid).unwrap();
                if m.duration_steps > 0 {
                    continue;
                }
                if let Some(wire) = &nb.wire_duration_ms {
                    let Some(&ms) = wire.get(&bid) else { continue };
                    m.duration_steps = duration_steps_from_ms(ms, bpm);
                } else {
                    if m.duration <= 0 {
                        continue;
                    }
                    m.duration_steps = duration_steps_from_ms(m.duration as f64, bpm);
                }
                nb.bindings.insert(bid, m);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Beat-relative durations (internal/pflow/duration.go)

/// `durationStepsFromMs`
pub fn duration_steps_from_ms(ms: f64, bpm: f64) -> i64 {
    if !(ms > 0.0 && bpm > 0.0) {
        return 0;
    }
    let steps = js_round(ms * bpm * 4.0 / 60000.0);
    if steps < 1.0 {
        1
    } else {
        steps as i64
    }
}

/// `resolveDurationMs` for a binding's raw fields (None = absent / not finite).
pub fn resolve_duration_ms(duration_steps: Option<f64>, duration: Option<f64>, bpm: f64) -> f64 {
    if let Some(s) = duration_steps {
        if s > 0.0 && bpm > 0.0 {
            return js_round(s * 60000.0 / (bpm * 4.0));
        }
    }
    match duration {
        Some(d) if d.is_finite() => d,
        _ => 100.0,
    }
}

impl MidiBinding {
    pub fn resolved_duration_ms(&self, bpm: f64) -> f64 {
        resolve_duration_ms(
            Some(self.duration_steps as f64),
            Some(self.duration as f64),
            bpm,
        )
    }
}
