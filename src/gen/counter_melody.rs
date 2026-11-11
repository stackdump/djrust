//! Port of `Generator/CounterMelody.swift` (generator/countermelody.js): the rule-based
//! note kernel behind the `counterMelody` arrange directive. Rule-based only (answer /
//! harmony / shadow).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::arrange::ArrangeError;
use crate::json::{floor, js_number_string, utf16_cmp};
use crate::pflow::{MidiBinding, NetBundle, Project, Track};
use crate::rng::Mulberry32;

/// PPQ matches sequencer-worker.js.
pub const COUNTER_PPQ: i64 = 4;

/// fnv32a over the UTF-16 code units of the concatenated parts (JS `charCodeAt`).
pub fn counter_seed(parts: &[String]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for part in parts {
        for u in part.encode_utf16() {
            h = (h ^ u as u32).wrapping_mul(0x0100_0193);
        }
    }
    h
}

#[derive(Clone, Copy, Debug)]
pub struct SourceNote {
    pub tick: i64,
    pub note: i64,
    pub velocity: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CounterNote {
    pub start_tick: i64,
    pub note: i64,
    pub velocity: i64,
    pub duration: i64,
}

struct Input {
    place: usize,
    weight: f64,
    inhibit: bool,
}

struct Output {
    place: usize,
    weight: f64,
}

/// The runtime half of `NetBundle` (arc index, `resetState`, `isEnabled`, `fire`) for the
/// transitions that carry a MIDI binding, with places densely indexed. Same semantics as
/// the JS: a transition with no input arcs is never enabled; tokens never go below zero.
struct CounterNetRuntime {
    initial: Vec<f64>,
    state: Vec<f64>,
    labels: Vec<String>,
    bindings: Vec<MidiBinding>,
    inputs: Vec<Option<Vec<Input>>>,
    outputs: Vec<Vec<Output>>,
}

impl CounterNetRuntime {
    fn new(nb: &NetBundle) -> Self {
        let mut index: BTreeMap<&str, usize> = BTreeMap::new();
        let mut initial: Vec<f64> = Vec::new();
        for (id, p) in nb.places.iter() {
            let n = index.len();
            index.entry(id).or_insert(n);
            initial.push(p.initial.iter().sum());
        }
        for a in &nb.arcs {
            let n = index.len();
            index.entry(a.source.as_str()).or_insert(n);
            let n = index.len();
            index.entry(a.target.as_str()).or_insert(n);
        }
        while initial.len() < index.len() {
            initial.push(0.0);
        }
        fn weight_sum(w: &[f64]) -> f64 {
            if w.is_empty() {
                1.0
            } else {
                w.iter().sum()
            }
        }

        // Object.keys(bindings).sort()
        let mut labels: Vec<String> = nb.bindings.keys().to_vec();
        labels.sort_by(|a, b| utf16_cmp(a, b));
        let mut bindings = Vec::new();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for label in &labels {
            bindings.push(*nb.bindings.get(label).unwrap());
            let mut ins = Vec::new();
            let mut outs = Vec::new();
            for a in &nb.arcs {
                if a.target == *label {
                    ins.push(Input {
                        place: index[a.source.as_str()],
                        weight: weight_sum(&a.weight),
                        inhibit: a.inhibit,
                    });
                }
                if a.source == *label {
                    outs.push(Output {
                        place: index[a.target.as_str()],
                        weight: weight_sum(&a.weight),
                    });
                }
            }
            inputs.push(if ins.is_empty() { None } else { Some(ins) });
            outputs.push(outs);
        }
        CounterNetRuntime {
            state: initial.clone(),
            initial,
            labels,
            bindings,
            inputs,
            outputs,
        }
    }

    fn reset(&mut self) {
        self.state.copy_from_slice(&self.initial);
    }

    fn is_enabled(&self, li: usize) -> bool {
        let Some(ins) = &self.inputs[li] else {
            return false;
        };
        for ca in ins {
            let tokens = self.state[ca.place];
            if ca.inhibit {
                if tokens >= ca.weight {
                    return false;
                }
            } else if tokens < ca.weight {
                return false;
            }
        }
        true
    }

    fn fire(&mut self, li: usize) {
        if let Some(ins) = &self.inputs[li] {
            for ca in ins.iter().filter(|c| !c.inhibit) {
                self.state[ca.place] -= ca.weight;
                if self.state[ca.place] < 0.0 {
                    self.state[ca.place] = 0.0;
                }
            }
        }
        for o in &self.outputs[li] {
            self.state[o.place] += o.weight;
        }
    }
}

pub struct Simulated {
    pub hits: Vec<bool>,
    pub pitches: Vec<i64>,
    pub notes: Vec<SourceNote>,
}

/// `simulateMusicNotes(proj, totalTicks, scopeNetID)`. `plain` lists the nets that exist in
/// JS as unparsed objects (no `resetState`): reaching one throws, exactly as the JS does.
pub fn simulate_music_notes(
    proj: &Project,
    total_ticks: i64,
    scope: Option<&str>,
    plain: &BTreeSet<String>,
) -> Result<Simulated, ArrangeError> {
    let total = total_ticks.max(0) as usize;
    let mut hits = alloc::vec![false; total];
    let mut pitch_set = BTreeSet::new();
    let mut notes = Vec::new();
    let mut ids: Vec<&String> = proj.nets.keys().iter().collect();
    ids.sort_by(|a, b| utf16_cmp(a, b));
    for id in ids {
        if let Some(scope) = scope {
            if id != scope {
                continue;
            }
        }
        let Some(nb) = proj.nets.get(id) else {
            continue;
        };
        if nb.role == "control" {
            continue;
        }
        if plain.contains(id) {
            return Err(ArrangeError::NetNotParsed(id.clone()));
        }
        let mut rt = CounterNetRuntime::new(nb);
        rt.reset();
        for (tick, hit) in hits.iter_mut().enumerate() {
            let mut fired = false;
            for li in 0..rt.labels.len() {
                if !rt.is_enabled(li) {
                    continue;
                }
                rt.fire(li);
                fired = true;
                let b = rt.bindings[li];
                if b.note > 0 {
                    *hit = true;
                    pitch_set.insert(b.note);
                    let mut vel = b.velocity;
                    if vel <= 0 {
                        vel = 90;
                    }
                    notes.push(SourceNote {
                        tick: tick as i64,
                        note: b.note,
                        velocity: vel,
                    });
                }
                break;
            }
            if !fired {
                rt.reset();
            }
        }
    }
    Ok(Simulated {
        hits,
        pitches: pitch_set.into_iter().collect(),
        notes,
    })
}

fn clamp_pitch(p: i64) -> i64 {
    let mut pitch = p;
    while pitch < 24 {
        pitch += 12;
    }
    while pitch > 108 {
        pitch -= 12;
    }
    pitch
}

pub fn answer_mode(
    hits: &[bool],
    pitches: &[i64],
    register: &str,
    density: f64,
    rng: &mut Mulberry32,
) -> Vec<CounterNote> {
    let transpose = if register == "below" { -12 } else { 12 };
    let mut notes = Vec::new();
    let total = hits.len();
    let mut t = 0;
    while t < total {
        if hits[t] {
            t += 1;
            continue;
        }
        let run_start = t;
        while t < total && !hits[t] {
            t += 1;
        }
        let run_len = t - run_start;
        if run_len < 2 {
            continue;
        }
        if rng.next() > density {
            continue;
        }
        let base_pitch =
            clamp_pitch(pitches[floor(rng.next() * pitches.len() as f64) as usize] + transpose);
        let start_tick = (run_start + run_len / 2) as i64;
        let mut dur = (run_len / 2) as i64;
        if dur > 8 {
            dur = 8;
        }
        if dur < 1 {
            dur = 1;
        }
        notes.push(CounterNote {
            start_tick,
            note: base_pitch,
            velocity: 90,
            duration: dur,
        });
    }
    notes
}

pub fn harmony_mode(
    src: &[SourceNote],
    register: &str,
    density: f64,
    rng: &mut Mulberry32,
) -> Vec<CounterNote> {
    let interval = if register == "below" { -3 } else { 4 };
    let mut out = Vec::new();
    for n in src {
        if rng.next() > density {
            continue;
        }
        out.push(CounterNote {
            start_tick: n.tick,
            note: clamp_pitch(n.note + interval),
            velocity: n.velocity,
            duration: 4,
        });
    }
    out
}

pub fn shadow_mode(
    src: &[SourceNote],
    total_ticks: i64,
    register: &str,
    density: f64,
    rng: &mut Mulberry32,
) -> Vec<CounterNote> {
    let transpose = if register == "below" { -12 } else { 12 };
    let mut out = Vec::new();
    for n in src {
        if rng.next() > density {
            continue;
        }
        let echo = n.tick + 1;
        if echo >= total_ticks {
            continue;
        }
        let mut velocity = n.velocity / 2;
        if velocity < 30 {
            velocity = 30;
        }
        out.push(CounterNote {
            start_tick: echo,
            note: clamp_pitch(n.note + transpose),
            velocity,
            duration: 3,
        });
    }
    out
}

/// `generateCounterMelody`. `None`: no source material; empty: the mode found nothing.
#[allow(clippy::too_many_arguments)]
pub fn generate_counter_melody(
    proj: &Project,
    mode: Option<&str>,
    raw_density: f64,
    register: Option<&str>,
    seed: u32,
    raw_ticks: i64,
    source_net_id: Option<&str>,
    plain: &BTreeSet<String>,
) -> Result<Option<Vec<CounterNote>>, ArrangeError> {
    let register = register.unwrap_or("above");
    let mut density = raw_density;
    if density.is_nan() || density <= 0.0 {
        density = 0.5;
    }
    if density > 1.0 {
        density = 1.0;
    }
    let total_ticks = if raw_ticks < 4 { 4 } else { raw_ticks };
    let sim = simulate_music_notes(proj, total_ticks, source_net_id, plain)?;
    if sim.pitches.is_empty() {
        return Ok(None);
    }
    let mut rng = Mulberry32::new(seed as f64);
    Ok(Some(match mode {
        Some("harmony") => harmony_mode(&sim.notes, register, density, &mut rng),
        Some("shadow") => shadow_mode(&sim.notes, total_ticks, register, density, &mut rng),
        _ => answer_mode(&sim.hits, &sim.pitches, register, density, &mut rng),
    }))
}

/// `buildMusicNet(notes, opts)` in its parsed form.
pub fn build_counter_melody_net(
    notes: &[CounterNote],
    total_ticks: i64,
    channel: i64,
    instrument: &str,
    ms_per_tick: f64,
) -> Option<NetBundle> {
    if total_ticks <= 0 {
        return None;
    }
    // The JS writes all-zero coordinates; the worker's parse then recomputes the layout,
    // which for this single cycle is exactly the ring layout. (Coordinates are not in the
    // model.)
    let mut nb = NetBundle::ring(total_ticks);
    nb.role = "music".into();
    nb.track = Track {
        channel,
        default_velocity: 90,
        instrument: instrument.into(),
        instrument_set: Vec::new(),
        group: Some("harmony".into()),
        ..Default::default()
    };
    for n in notes {
        if n.start_tick < 0 || n.start_tick >= total_ticks {
            continue;
        }
        let mut dur_ms = floor(n.duration as f64 * ms_per_tick) as i64;
        if dur_ms < 1 {
            dur_ms = 1;
        }
        nb.bind(n.start_tick, n.note, channel, n.velocity, dur_ms);
    }
    Some(nb)
}

/// JS `Number.prototype.toFixed(6)`: the exact binary value rounded half up (not the
/// half-even a C `printf` does on exact ties such as 0.0078125).
pub fn js_to_fixed6(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.abs() >= 1e21 {
        return js_number_string(x);
    }
    let neg = x < 0.0;
    let bits = x.abs().to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    // |x| = m * 2^e exactly
    let (m, e) = if exp == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp - 1075)
    };
    // scaled = |x| * 10^6, rounded half up, as an integer
    let scaled: u128 = if e >= 0 {
        ((m as u128) << e as u32) * 1_000_000
    } else {
        let n = m as u128 * 1_000_000;
        let sh = (-e) as u32;
        if sh >= 127 {
            0
        } else {
            let q = n >> sh;
            let r = n & ((1u128 << sh) - 1);
            let half = 1u128 << (sh - 1);
            if r >= half {
                q + 1
            } else {
                q
            }
        }
    };
    let int_part = scaled / 1_000_000;
    let frac_part = scaled % 1_000_000;
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    use core::fmt::Write;
    let _ = write!(s, "{int_part}.{frac_part:06}");
    s
}
