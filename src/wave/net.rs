//! Port of `Beats/Wave/WaveNet.swift` (beats' `wave-engine/net.js`): the Petri-net
//! executor compiled to flat arrays.
//!
//! Semantics are the sequencer worker's `_advanceOneTick` with `deterministicLoop` on:
//! nets in project order, enabled transitions in key order, one winner per contested
//! input place chosen by `deterministicRand(tick, strHash(place))`, controls applied
//! immediately so a mute fired by an earlier net affects later nets in the same tick.
//! Not ported (same as the JS): macros (`fire-macro`), loop/seek, drift - reported, not
//! applied. The live-performance half (role mutes, macros, history) is in `macros.rs`
//! and `history.rs`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::pflow::{Arc, NetBundle, Project};
use crate::rng::Mulberry32;

/// `ControlAction` (`A_NONE`…`A_OTHER` in net.js).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum ControlAction {
    None = 0,
    Mute = 1,
    Unmute = 2,
    Toggle = 3,
    MuteNote = 4,
    UnmuteNote = 5,
    ToggleNote = 6,
    Slot = 7,
    Stop = 8,
    Other = 9,
}

impl ControlAction {
    pub fn from_name(name: &str) -> ControlAction {
        match name {
            "mute-track" => ControlAction::Mute,
            "unmute-track" => ControlAction::Unmute,
            "toggle-track" => ControlAction::Toggle,
            "mute-note" => ControlAction::MuteNote,
            "unmute-note" => ControlAction::UnmuteNote,
            "toggle-note" => ControlAction::ToggleNote,
            "activate-slot" => ControlAction::Slot,
            "stop-transport" => ControlAction::Stop,
            _ => ControlAction::Other,
        }
    }
}

/// `deterministicRand(tick, salt)` - identical to the sequencer worker.
pub fn deterministic_rand(tick: i64, salt: i32) -> f64 {
    let seed = (tick as i32).wrapping_add(salt);
    Mulberry32::from_i32(seed).next()
}

/// `strHash`: `h = imul(31, h) + charCode | 0` over UTF-16 code units.
pub fn str_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for u in s.encode_utf16() {
        h = 31i32.wrapping_mul(h).wrapping_add(u as i32);
    }
    h
}

/// `CompiledNet`: one net as flat arrays.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct CompiledNet {
    pub id: String,
    pub index: usize,
    /// Not on the wire: `graph.project.nets[id]` again on arrival (`WaveBuild::step`).
    #[cfg_attr(feature = "transfer", serde(skip))]
    pub bundle: NetBundle,
    pub place_ids: Vec<String>,
    pub trans_ids: Vec<String>,
    pub n_places: usize,
    pub n_trans: usize,

    pub(crate) in_start: Vec<usize>,
    pub(crate) in_place: Vec<i64>,
    pub(crate) in_w: Vec<f64>,
    pub(crate) in_inhib: Vec<bool>,
    pub(crate) out_start: Vec<usize>,
    pub(crate) out_place: Vec<i64>,
    pub(crate) out_w: Vec<f64>,
    pub(crate) place_salt: Vec<i32>,
    pub(crate) initial: Vec<f64>,
    pub state: Vec<f64>,

    pub midi: Vec<bool>,
    pub note: Vec<i64>,
    pub vel: Vec<i64>,
    pub dur_ms: Vec<f64>,
    pub dur_steps: Vec<f64>,
    /// The syllable a formant voice sings on each MIDI-bound transition
    /// ([`crate::fx::formant::Syllable::code`]; 0: none).
    #[cfg_attr(feature = "transfer", serde(default))]
    pub syllable: Vec<u16>,
    /// MIDI channel of every MIDI-bound transition: the binding's, else the track's (what
    /// picks its voice). 0 for transitions without a binding.
    pub channel: Vec<u8>,
    pub(crate) action: Vec<ControlAction>,
    pub(crate) action_note: Vec<i64>,
    pub(crate) action_target: Vec<String>,
    pub(crate) action_target_idx: Vec<i64>,
    pub group: String,
    pub(crate) group_idx: i64,
    /// Lane index of every MIDI-bound transition (filled by the runner); -1 otherwise.
    pub lane: Vec<i64>,

    // scratch, sized once
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    enabled: Vec<usize>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    blocked: Vec<bool>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    consumers: Vec<usize>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    place_count: Vec<i64>,

    // Seek tables (`WaveGraph::fast_forward`), built once with the net: the transitions with
    // an input arc (inhibitor or not) from place p are `cons_t[cons_start[p]..cons_start[p + 1]]`,
    // and `has_action` is whether any transition carries a control action that moves the
    // graph's state outside the net (mutes, slots, the stop; not `Other`, such as `fire-macro`
    // or `set-feel`, which only lists its firing in `controls`).
    cons_start: Vec<u32>,
    cons_t: Vec<u32>,
    /// `ff_chain[t]` = 1 + the place `t` moves one token into, when `t` is a silent link of a
    /// chain (no MIDI binding, no control action; exactly one input arc, weight 1, from a place
    /// no other transition reads; exactly one output arc, weight 1, to another place), else 0.
    /// A seek walks a token down a run of them at once (`ff_chain_run`): a song's structure
    /// net is one such chain per tick, broken only by its section actions.
    ff_chain: Vec<u32>,
    has_action: bool,
    // seek scratch, sized once: `ff_bits` bit t = `is_enabled(t)` on the current state, and
    // `ff_diff` bit p = the state of p is not bit for bit its initial marking (`ff_ndiff` bits set)
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    ff_bits: Vec<u64>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    ff_diff: Vec<u64>,
    ff_ndiff: usize,
    ff_count: usize,
    ff_dead: bool,
    /// `enabled[..ff_count]` is the firing set of tick `ff_at + 1`, which fires an action
    ff_pending: bool,
    ff_multi: bool,
    /// ticks this net has been stepped through
    ff_at: i64,
    ff_most: usize,
    /// the most transitions with a control action (any, `Other` too) fired on one tick
    ff_most_ctl: usize,
    /// words `ff_lo..ff_hi` of `ff_bits` hold every set bit
    ff_lo: usize,
    ff_hi: usize,
    // What stepping alone from the initial marking does, surveyed once when the graph is
    // built (`ff_survey`): `ff_bits0` (words `ff_lo0..ff_hi0`) is the initial enabled set, and
    // `ff_fate` is FATE_OPEN, FATE_DEAD (nothing enabled after `ff_len` ticks, on marking
    // `ff_end`) or FATE_CYCLE (back on the initial marking bit for bit after `ff_len` ticks,
    // the tick never having decided anything), with the most MIDI-bound / control-action
    // transitions fired on one tick on the way.
    ff_bits0: Vec<u64>,
    ff_lo0: usize,
    ff_hi0: usize,
    ff_fate: u8,
    ff_len: i64,
    ff_end: Vec<f64>,
    ff_fate_most: usize,
    ff_fate_most_ctl: usize,
}

fn weight_sum(w: &[f64]) -> f64 {
    if w.is_empty() {
        1.0
    } else {
        w.iter().sum()
    }
}

impl CompiledNet {
    fn new(id: &str, nb: &NetBundle, index: usize) -> CompiledNet {
        let place_ids: Vec<String> = nb.places.keys().to_vec();
        let trans_ids: Vec<String> = nb.transitions.keys().to_vec();
        let n_places = place_ids.len();
        let n_trans = trans_ids.len();
        let mut p_idx: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, p) in place_ids.iter().enumerate() {
            p_idx.entry(p.as_str()).or_insert(i);
        }

        // arcs by target / source, in document order
        let mut input_arcs: BTreeMap<&str, Vec<&Arc>> = BTreeMap::new();
        let mut output_arcs: BTreeMap<&str, Vec<&Arc>> = BTreeMap::new();
        for arc in &nb.arcs {
            input_arcs.entry(arc.target.as_str()).or_default().push(arc);
            output_arcs
                .entry(arc.source.as_str())
                .or_default()
                .push(arc);
        }

        let mut in_start = vec![0usize; n_trans + 1];
        let mut out_start = vec![0usize; n_trans + 1];
        let mut ins: Vec<(i64, f64, bool)> = Vec::new();
        let mut outs: Vec<(i64, f64)> = Vec::new();
        for (i, t) in trans_ids.iter().enumerate() {
            in_start[i] = ins.len();
            for ca in input_arcs.get(t.as_str()).into_iter().flatten() {
                let p = p_idx.get(ca.source.as_str()).map_or(-1, |&v| v as i64);
                ins.push((p, weight_sum(&ca.weight), ca.inhibit));
            }
            out_start[i] = outs.len();
            for ca in output_arcs.get(t.as_str()).into_iter().flatten() {
                let p = p_idx.get(ca.target.as_str()).map_or(-1, |&v| v as i64);
                outs.push((p, weight_sum(&ca.weight)));
            }
        }
        in_start[n_trans] = ins.len();
        out_start[n_trans] = outs.len();

        // nb.resetState(): the marking is the sum of each place's initial.
        let initial: Vec<f64> = place_ids
            .iter()
            .map(|p| nb.places.get(p).unwrap().initial.iter().sum())
            .collect();

        let mut midi = vec![false; n_trans];
        let mut note = vec![0i64; n_trans];
        let mut vel = vec![0i64; n_trans];
        let mut dur_ms = vec![0.0; n_trans];
        let mut dur_steps = vec![0.0; n_trans];
        let mut syllable = vec![0u16; n_trans];
        let mut channel = vec![0u8; n_trans];
        let mut action = vec![ControlAction::None; n_trans];
        let mut action_note = vec![0i64; n_trans];
        let mut action_target: Vec<String> = Vec::with_capacity(n_trans);
        for (i, t) in trans_ids.iter().enumerate() {
            if let Some(b) = nb.bindings.get(t) {
                midi[i] = true;
                note[i] = b.note as i16 as i64;
                vel[i] = b.velocity as i16 as i64;
                dur_ms[i] = b.duration as f64;
                dur_steps[i] = b.duration_steps as f64;
                syllable[i] = b.syllable;
                let ch = if b.channel != 0 {
                    b.channel
                } else {
                    nb.track.channel
                };
                channel[i] = ch.clamp(0, 255) as u8;
            }
            if let Some(c) = nb.control_bindings.get(t) {
                action_target.push(c.target_net.clone());
                action[i] = ControlAction::from_name(&c.action);
                action_note[i] = c.target_note;
            } else {
                action_target.push(String::new());
            }
        }

        // places → consuming transitions, each transition once per place
        let mut cons_start = vec![0u32; n_places + 1];
        for t in 0..n_trans {
            for a in in_start[t]..in_start[t + 1] {
                let p = ins[a].0;
                if p >= 0 && !ins[in_start[t]..a].iter().any(|b| b.0 == p) {
                    cons_start[p as usize + 1] += 1;
                }
            }
        }
        for p in 0..n_places {
            cons_start[p + 1] += cons_start[p];
        }
        let mut cons_t = vec![0u32; cons_start[n_places] as usize];
        let mut fill: Vec<u32> = cons_start[..n_places].to_vec();
        for t in 0..n_trans {
            for a in in_start[t]..in_start[t + 1] {
                let p = ins[a].0;
                if p >= 0 && !ins[in_start[t]..a].iter().any(|b| b.0 == p) {
                    cons_t[fill[p as usize] as usize] = t as u32;
                    fill[p as usize] += 1;
                }
            }
        }
        let has_action = action
            .iter()
            .any(|&a| a != ControlAction::None && a != ControlAction::Other);
        let one = 1f64.to_bits();
        let ff_chain: Vec<u32> = (0..n_trans)
            .map(|t| {
                if midi[t]
                    || action[t] != ControlAction::None
                    || in_start[t + 1] - in_start[t] != 1
                    || out_start[t + 1] - out_start[t] != 1
                {
                    return 0;
                }
                let (p, w, inhib) = ins[in_start[t]];
                let (q, wo) = outs[out_start[t]];
                if inhib || p < 0 || q < 0 || p == q || w.to_bits() != one || wo.to_bits() != one {
                    return 0;
                }
                if cons_start[p as usize + 1] - cons_start[p as usize] != 1 {
                    return 0;
                }
                q as u32 + 1
            })
            .collect();

        CompiledNet {
            id: id.into(),
            index,
            bundle: nb.clone(),
            n_places,
            n_trans,
            in_start,
            in_place: ins.iter().map(|a| a.0).collect(),
            in_w: ins.iter().map(|a| a.1).collect(),
            in_inhib: ins.iter().map(|a| a.2).collect(),
            out_start,
            out_place: outs.iter().map(|a| a.0).collect(),
            out_w: outs.iter().map(|a| a.1).collect(),
            place_salt: place_ids.iter().map(|p| str_hash(p)).collect(),
            state: initial.clone(),
            initial,
            midi,
            note,
            vel,
            dur_ms,
            dur_steps,
            syllable,
            channel,
            action,
            action_note,
            action_target,
            action_target_idx: vec![-1; n_trans],
            group: nb.riff_group.clone(),
            group_idx: -1,
            lane: vec![-1; n_trans],
            enabled: vec![0; n_trans],
            blocked: vec![false; n_trans],
            consumers: vec![0; ins.len().max(1)],
            place_count: vec![0; n_places],
            cons_start,
            cons_t,
            ff_chain,
            has_action,
            ff_bits: vec![0; n_trans.div_ceil(64)],
            ff_diff: vec![0; n_places.div_ceil(64)],
            ff_ndiff: 0,
            ff_count: 0,
            ff_dead: false,
            ff_pending: false,
            ff_multi: false,
            ff_at: 0,
            ff_most: 0,
            ff_most_ctl: 0,
            ff_lo: 0,
            ff_hi: 0,
            ff_bits0: Vec::new(),
            ff_lo0: 0,
            ff_hi0: 0,
            ff_fate: FATE_OPEN,
            ff_len: 0,
            ff_end: Vec::new(),
            ff_fate_most: 0,
            ff_fate_most_ctl: 0,
            place_ids,
            trans_ids,
        }
    }

    /// `mat[p][t] = Σ weight(t→p) − Σ weight(p→t)`, inhibitor arcs excluded.
    pub fn incidence_matrix(&self) -> Vec<Vec<f64>> {
        let mut mat = vec![vec![0.0; self.n_trans]; self.n_places];
        for t in 0..self.n_trans {
            for a in self.in_start[t]..self.in_start[t + 1] {
                if !self.in_inhib[a] && self.in_place[a] >= 0 {
                    mat[self.in_place[a] as usize][t] -= self.in_w[a];
                }
            }
            for a in self.out_start[t]..self.out_start[t + 1] {
                if self.out_place[a] >= 0 {
                    mat[self.out_place[a] as usize][t] += self.out_w[a];
                }
            }
        }
        mat
    }

    fn is_enabled(&self, t: usize) -> bool {
        let end = self.in_start[t + 1];
        if self.in_start[t] == end {
            return false;
        }
        for a in self.in_start[t]..end {
            let tok = if self.in_place[a] >= 0 {
                self.state[self.in_place[a] as usize]
            } else {
                0.0
            };
            if self.in_inhib[a] {
                if tok >= self.in_w[a] {
                    return false;
                }
            } else if tok < self.in_w[a] {
                return false;
            }
        }
        true
    }

    fn fire(&mut self, t: usize) {
        for a in self.in_start[t]..self.in_start[t + 1] {
            let p = self.in_place[a];
            if !self.in_inhib[a] && p >= 0 {
                let p = p as usize;
                self.state[p] -= self.in_w[a];
                if self.state[p] < 0.0 {
                    self.state[p] = 0.0;
                }
            }
        }
        for a in self.out_start[t]..self.out_start[t + 1] {
            let p = self.out_place[a];
            if p >= 0 {
                self.state[p as usize] += self.out_w[a];
            }
        }
    }

    /// Returns the number of survivors written back into `enabled`.
    fn resolve_conflicts(&mut self, count: usize, tick: i64) -> usize {
        for c in self.place_count.iter_mut() {
            *c = 0;
        }
        let mut contested = false;
        for i in 0..count {
            let t = self.enabled[i];
            for a in self.in_start[t]..self.in_start[t + 1] {
                if !self.in_inhib[a] && self.in_place[a] >= 0 {
                    let p = self.in_place[a] as usize;
                    self.place_count[p] += 1;
                    if self.place_count[p] > 1 {
                        contested = true;
                    }
                }
            }
        }
        if !contested {
            return count;
        }
        for b in self.blocked.iter_mut() {
            *b = false;
        }
        for p in 0..self.n_places {
            if self.place_count[p] < 2 {
                continue;
            }
            let mut c = 0;
            for i in 0..count {
                let t = self.enabled[i];
                for a in self.in_start[t]..self.in_start[t + 1] {
                    if !self.in_inhib[a] && self.in_place[a] == p as i64 {
                        self.consumers[c] = t;
                        c += 1;
                    }
                }
            }
            let r = deterministic_rand(tick, self.place_salt[p]);
            let winner = self.consumers[libm::floor(r * c as f64) as usize];
            for i in 0..c {
                if self.consumers[i] != winner {
                    self.blocked[self.consumers[i]] = true;
                }
            }
        }
        let mut w = 0;
        for i in 0..count {
            if !self.blocked[self.enabled[i]] {
                self.enabled[w] = self.enabled[i];
                w += 1;
            }
        }
        w
    }

    pub(crate) fn unfire(&mut self, t: usize) {
        for a in self.out_start[t]..self.out_start[t + 1] {
            let p = self.out_place[a];
            if p >= 0 {
                self.state[p as usize] -= self.out_w[a];
            }
        }
        for a in self.in_start[t]..self.in_start[t + 1] {
            let p = self.in_place[a];
            if !self.in_inhib[a] && p >= 0 {
                self.state[p as usize] += self.in_w[a];
            }
        }
    }
}

/// `isStingerID`: `hit1`…
pub(crate) fn is_stinger_id(id: &str) -> bool {
    id.starts_with("hit") && id.len() > 3 && id[3..].bytes().all(|b| b.is_ascii_digit())
}

/// The role tables of a graph (`roleTargets` / `groupNets`): what a role name's hash names, a riff
/// group's nets or one plain net. Copied off a project's graph when it is loaded
/// ([`WaveGraph::role_tables`]), so the main thread can answer `rolesHeld` and `isRoleAudible`
/// for `wave.latest` after the project itself has gone to the audio thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct RoleTables {
    pub(crate) role_targets: BTreeMap<i32, i64>,
    pub(crate) group_nets: Vec<Vec<usize>>,
}

impl RoleTables {
    /// The nets (indices into the graph's `nets`) a role names, `None` for a role the song does
    /// not have (`WaveGraph::role_net_indices`).
    pub fn role_net_indices(&self, hash: i32) -> Option<Vec<usize>> {
        let &t = self.role_targets.get(&hash)?;
        Some(if t >= 0 {
            self.group_nets[t as usize].clone()
        } else {
            vec![(-t - 2) as usize]
        })
    }

    /// The same, without allocating: a group's nets, or the one plain net.
    pub(crate) fn nets_of(&self, hash: i32) -> Option<RoleNets<'_>> {
        let &t = self.role_targets.get(&hash)?;
        Some(if t >= 0 {
            RoleNets::Group(&self.group_nets[t as usize])
        } else {
            RoleNets::Net((-t - 2) as usize)
        })
    }
}

/// What a role is: a riff group's nets or one plain net (`roleTargets`' two kinds).
pub(crate) enum RoleNets<'a> {
    Group(&'a [usize]),
    Net(usize),
}

/// `WaveGraph`: the compiled project, mute tables and the per-tick event buffers.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct WaveGraph {
    pub project: Project,
    pub nets: Vec<CompiledNet>,
    pub by_id: BTreeMap<String, usize>,
    /// strHash(netId) → net index (first net wins on a hash collision): lets the audio
    /// thread address a net without a string.
    pub by_hash: BTreeMap<i32, usize>,
    pub groups: Vec<String>,
    pub tempo: f64,
    pub tick: i64,
    pub muted: Vec<u8>,
    pub muted_notes: Vec<u8>,
    pub muted_groups: Vec<u8>,
    pub stop_requested: bool,
    /// Events of the last tick, for the UI: (net, transition) pairs flattened. Fixed capacity.
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub fired: Vec<i32>,
    pub fired_count: usize,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub controls: Vec<i32>,
    pub control_count: usize,
    pub overflow: i64,

    // Live-performance state (macros.rs, history.rs).
    pub(crate) is_drum_net: Vec<bool>,
    pub(crate) is_control_net: Vec<bool>,
    pub(crate) is_kick_net: Vec<bool>,
    pub(crate) is_stinger_net: Vec<bool>,
    /// Role name hash → group index (>= 0) or `-(net index) - 2` for a plain net.
    pub(crate) role_targets: BTreeMap<i32, i64>,
    pub(crate) group_nets: Vec<Vec<usize>>,
    pub(crate) active_slot: Vec<i64>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) timer_remaining: Vec<i32>,
    /// 0 free, 1 unmute, 2 mute, 3 delayed mute macro
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) timer_kind: Vec<u8>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) timer_net: Vec<i32>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) timer_arg: Vec<i32>,
    pub timer_overflow: i64,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) h_fires: Vec<i32>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) h_changes: Vec<i32>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) h_fire_end: Vec<i32>,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::zeros"))]
    pub(crate) h_change_end: Vec<i32>,
    pub(crate) h_ticks: usize,
    pub(crate) h_fire_count: usize,
    pub(crate) h_change_count: usize,
    pub(crate) recording: bool,
    pub(crate) fire_need_cache: i64,
    pub(crate) repeat_remaining: i64,
    pub(crate) repeat_elapsed: i64,
    pub(crate) repeat_step: i64,
    pub(crate) repeat_burst: i64,
}

impl WaveGraph {
    pub const TIMER_CAPACITY: usize = 512;
    pub const HISTORY_TICKS: usize = 512;
    pub(crate) const FIRE_CAPACITY: usize = 1 << 17;
    pub(crate) const CHANGE_CAPACITY: usize = 6 << 14;

    /// `compileProject(json)`: the project is round-tripped through `parseProject` in the
    /// JS, so a composed `Project` goes through its own JSON form here for the same
    /// rounding.
    pub fn new(project: &Project) -> WaveGraph {
        WaveGraph::from_parsed(Project::from_json(&project.to_json()))
    }

    pub fn from_json(json: &crate::json::JsonValue) -> WaveGraph {
        WaveGraph::from_parsed(Project::from_json(json))
    }

    fn from_parsed(proj: Project) -> WaveGraph {
        let ids: Vec<String> = proj.nets.keys().to_vec();
        let mut nets: Vec<CompiledNet> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| CompiledNet::new(id, proj.nets.get(id).unwrap(), i))
            .collect();
        let mut by_id = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            by_id.insert(id.clone(), i);
        }
        let mut by_hash = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            by_hash.entry(str_hash(id)).or_insert(i);
        }
        let mut groups: Vec<String> = Vec::new();
        for n in &nets {
            if !n.group.is_empty() && !groups.contains(&n.group) {
                groups.push(n.group.clone());
            }
        }
        for n in nets.iter_mut() {
            n.group_idx = if n.group.is_empty() {
                -1
            } else {
                groups.iter().position(|g| *g == n.group).unwrap() as i64
            };
            for t in 0..n.n_trans {
                let tgt = &n.action_target[t];
                if !tgt.is_empty() {
                    if let Some(&i) = by_id.get(tgt) {
                        n.action_target_idx[t] = i as i64;
                    }
                }
            }
        }
        let tempo = if proj.tempo > 0.0 { proj.tempo } else { 120.0 };
        let is_control_net: Vec<bool> = nets.iter().map(|n| n.bundle.role == "control").collect();
        let is_drum_net: Vec<bool> = nets
            .iter()
            .map(|n| super::synth::is_drum_channel(n.bundle.track.channel))
            .collect();
        let is_stinger_net: Vec<bool> = nets.iter().map(|n| is_stinger_id(&n.id)).collect();
        let is_kick_net: Vec<bool> = nets
            .iter()
            .map(|n| n.bundle.riff_group == "kick" || n.id == "kick" || n.id.starts_with("kick:"))
            .collect();
        let mut role_targets: BTreeMap<i32, i64> = BTreeMap::new();
        let mut group_nets: Vec<Vec<usize>> = vec![Vec::new(); groups.len()];
        for n in &nets {
            if n.bundle.role != "control" && n.group_idx >= 0 {
                group_nets[n.group_idx as usize].push(n.index);
            }
        }
        for (gi, name) in groups.iter().enumerate() {
            role_targets.insert(str_hash(name), gi as i64);
        }
        for n in &nets {
            if n.bundle.role != "control" && n.group_idx < 0 {
                // a plain net is addressed by its id, unless a group already claimed that name
                role_targets
                    .entry(str_hash(&n.id))
                    .or_insert(-(n.index as i64) - 2);
            }
        }
        let n_nets = nets.len();
        let n_groups = groups.len();
        let mut g = WaveGraph {
            project: proj,
            nets,
            by_id,
            by_hash,
            groups,
            tempo,
            tick: 0,
            muted: vec![0; n_nets],
            muted_notes: vec![0; n_nets * 128],
            muted_groups: vec![0; n_groups.max(1)],
            stop_requested: false,
            fired: vec![0; 8192],
            fired_count: 0,
            controls: vec![0; 1024],
            control_count: 0,
            overflow: 0,
            is_drum_net,
            is_control_net,
            is_kick_net,
            is_stinger_net,
            role_targets,
            group_nets,
            active_slot: vec![-1; n_groups],
            timer_remaining: vec![0; Self::TIMER_CAPACITY],
            timer_kind: vec![0; Self::TIMER_CAPACITY],
            timer_net: vec![0; Self::TIMER_CAPACITY],
            timer_arg: vec![0; Self::TIMER_CAPACITY],
            timer_overflow: 0,
            h_fires: vec![0; Self::FIRE_CAPACITY],
            h_changes: vec![0; Self::CHANGE_CAPACITY],
            h_fire_end: vec![0; Self::HISTORY_TICKS],
            h_change_end: vec![0; Self::HISTORY_TICKS],
            h_ticks: 0,
            h_fire_count: 0,
            h_change_count: 0,
            recording: true,
            fire_need_cache: -1,
            repeat_remaining: 0,
            repeat_elapsed: 0,
            repeat_step: 2,
            repeat_burst: 1,
        };
        g.reset();
        let mut budget = SURVEY_BUDGET;
        for n in g.nets.iter_mut() {
            n.ff_survey(&mut budget);
        }
        g
    }

    pub fn reset(&mut self) {
        self.tick = 0;
        self.stop_requested = false;
        for m in self.muted.iter_mut() {
            *m = 0;
        }
        for m in self.muted_notes.iter_mut() {
            *m = 0;
        }
        for m in self.muted_groups.iter_mut() {
            *m = 0;
        }
        // In place (reset runs on the audio thread via stop()).
        for n in self.nets.iter_mut() {
            for p in 0..n.n_places {
                n.state[p] = n.initial[p];
            }
        }
        for i in 0..self.project.initial_mutes.len() {
            if let Some(&n) = self.by_id.get(&self.project.initial_mutes[i]) {
                self.muted[n] = 1;
            }
        }
        self.clear_macros();
        self.clear_history();
        for gi in 0..self.group_nets.len() {
            self.active_slot[gi] = self.group_nets[gi]
                .iter()
                .copied()
                .find(|&i| self.muted[i] == 0)
                .map_or(-1, |i| i as i64);
        }
    }

    fn apply_control(&mut self, ni: usize, t: usize) {
        let tgt = self.nets[ni].action_target_idx[t];
        let action = self.nets[ni].action[t];
        let note = (self.nets[ni].action_note[t] & 127) as usize;
        match action {
            ControlAction::Mute => {
                if tgt >= 0 {
                    self.set_muted(tgt as usize, 1);
                }
            }
            ControlAction::Unmute => {
                if tgt >= 0 {
                    self.set_muted(tgt as usize, 0);
                }
            }
            ControlAction::Toggle => {
                if tgt >= 0 {
                    let v = self.muted[tgt as usize] ^ 1;
                    self.set_muted(tgt as usize, v);
                }
            }
            ControlAction::MuteNote => {
                if tgt >= 0 {
                    self.muted_notes[tgt as usize * 128 + note] = 1;
                }
            }
            ControlAction::UnmuteNote => {
                if tgt >= 0 {
                    self.muted_notes[tgt as usize * 128 + note] = 0;
                }
            }
            ControlAction::ToggleNote => {
                if tgt >= 0 {
                    self.muted_notes[tgt as usize * 128 + note] ^= 1;
                }
            }
            ControlAction::Slot => {
                if tgt >= 0 {
                    let tgt = tgt as usize;
                    let gi = self.nets[tgt].group_idx;
                    if gi >= 0 {
                        for o in 0..self.nets.len() {
                            if self.nets[o].group_idx == gi && o != tgt {
                                self.set_muted(o, 1);
                            }
                        }
                        if self.muted_groups[gi as usize] == 0 {
                            self.set_muted(tgt, 0);
                        }
                        let prev = self.active_slot[gi as usize];
                        self.log_change(2, gi, prev, 0, 0, 0);
                        self.active_slot[gi as usize] = tgt as i64;
                    }
                }
            }
            ControlAction::Stop => self.stop_requested = true,
            ControlAction::None | ControlAction::Other => {}
        }
        if self.control_count + 2 <= self.controls.len() {
            self.controls[self.control_count] = ni as i32;
            self.controls[self.control_count + 1] = t as i32;
            self.control_count += 2;
        } else {
            self.overflow += 1;
        }
    }

    /// Advance every net one tick, ignoring the notes.
    pub fn advance(&mut self) {
        self.advance_with(|_, _| {});
    }

    /// Advance every net one tick. `on_note(net, t)` is called for each fired transition
    /// whose MIDI binding is audible (net un-muted, note un-muted), in firing order.
    pub fn advance_with<F: FnMut(&CompiledNet, usize)>(&mut self, mut on_note: F) {
        self.tick += 1;
        self.fired_count = 0;
        self.control_count = 0;
        self.ensure_history_room();
        for ni in 0..self.nets.len() {
            let tick = self.tick;
            let mut count = 0;
            {
                let n = &mut self.nets[ni];
                for t in 0..n.n_trans {
                    if n.is_enabled(t) {
                        n.enabled[count] = t;
                        count += 1;
                    }
                }
                if count > 1 {
                    count = n.resolve_conflicts(count, tick);
                }
            }
            for i in 0..count {
                let t = self.nets[ni].enabled[i];
                self.nets[ni].fire(t);
                self.log_fire(ni, t);
                if self.nets[ni].action[t] != ControlAction::None {
                    self.apply_control(ni, t);
                }
                let n = &self.nets[ni];
                if n.midi[t]
                    && self.muted[ni] == 0
                    && self.muted_notes[ni * 128 + (n.note[t] & 127) as usize] == 0
                {
                    if self.fired_count + 2 <= self.fired.len() {
                        self.fired[self.fired_count] = ni as i32;
                        self.fired[self.fired_count + 1] = t as i32;
                        self.fired_count += 2;
                    } else {
                        self.overflow += 1;
                    }
                    on_note(&self.nets[ni], t);
                }
            }
        }
        self.service_macros();
        self.end_tick_record();
    }

    /// Plain marking `{netId: {place: tokens}}` - allocates, so only for a graph no
    /// other thread is running.
    pub fn marking(&self) -> BTreeMap<String, BTreeMap<String, f64>> {
        let mut out = BTreeMap::new();
        for n in &self.nets {
            let mut m = BTreeMap::new();
            for p in 0..n.n_places {
                m.insert(n.place_ids[p].clone(), n.state[p]);
            }
            out.insert(n.id.clone(), m);
        }
        out
    }

    /// [`fast_forward`](Self::fast_forward) without ticking every net `target` times.
    ///
    /// What a net fires depends on its own marking alone (and on the tick, but only when it
    /// has a conflict to resolve), and firing moves only its own marking: nets never feed each
    /// other. Only control actions reach outside a net (mutes, slots, the stop). So each net is
    /// stepped on its own, and the control actions are applied in the replay's order:
    ///
    /// - A net with a control action that moves the graph (a mute, a slot, the stop; not
    ///   `Other`, which only lists its firing) steps alone up to the tick before its next
    ///   action fires, and the pending actions of all such nets are applied earliest tick
    ///   first, nets in index order within a tick, exactly as the replay interleaves them. A
    ///   `stop-transport` ends the seek on its tick.
    /// - Every other net steps alone to the tick before the last. One that comes back to its
    ///   initial marking bit for bit, never having had two transitions enabled at once,
    ///   skips its whole cycles; one with nothing enabled stops there (nothing can enable it
    ///   again).
    /// - A step keeps an enabled-transition bitset that is re-checked only where a firing
    ///   touched a place, and a run of one transition that stays the only one enabled (a
    ///   countdown) is applied at once when its arithmetic is exact (`ff_run`).
    ///
    /// The last tick is a real [`advance`](Self::advance), so its events are the replay's.
    /// Returns false, with the graph half-moved (the replay must follow, and it resets), when a
    /// tick could have overflowed `controls` or `fired` (bounded by the sum over nets of the
    /// most each fired on one tick): the replay would have counted that in `overflow`, and this
    /// path does not count it. Allocates nothing.
    pub(crate) fn fast_forward_jump(&mut self, target: i64) -> bool {
        self.reset();
        self.recording = false;
        for n in self.nets.iter_mut() {
            n.ff_start();
        }
        // the control nets: each to its first action (or the tick before the last)
        let mut last = target;
        for n in self.nets.iter_mut() {
            if n.has_action {
                n.ff_advance(last - 1, true, false);
            }
        }
        loop {
            let mut e = i64::MAX;
            for n in &self.nets {
                if n.ff_pending && n.ff_at + 1 < e {
                    e = n.ff_at + 1;
                }
            }
            if e == i64::MAX {
                break;
            }
            let stop = self.nets.iter().any(|n| {
                n.ff_pending
                    && n.ff_at + 1 == e
                    && n.enabled[..n.ff_count]
                        .iter()
                        .any(|&t| n.action[t] == ControlAction::Stop)
            });
            if stop {
                // the stop's tick is the last: a real advance fires it
                last = e;
                break;
            }
            self.tick = e;
            self.fired_count = 0;
            self.control_count = 0;
            for ni in 0..self.nets.len() {
                if !(self.nets[ni].ff_pending && self.nets[ni].ff_at + 1 == e) {
                    continue;
                }
                let count = self.nets[ni].ff_count;
                let (mut midi, mut ctl) = (0, 0);
                for i in 0..count {
                    let t = self.nets[ni].enabled[i];
                    self.nets[ni].fire(t);
                    if self.nets[ni].action[t] != ControlAction::None {
                        self.apply_control(ni, t);
                        ctl += 1;
                    }
                    if self.nets[ni].midi[t] {
                        midi += 1;
                    }
                }
                let n = &mut self.nets[ni];
                n.ff_touched(count);
                n.ff_most = n.ff_most.max(midi);
                n.ff_most_ctl = n.ff_most_ctl.max(ctl);
                n.ff_at = e;
                n.ff_pending = false;
                n.ff_advance(last - 1, true, false);
            }
        }
        // everything to the tick before the last
        let steps = last - 1;
        let (mut most, mut most_ctl) = (0usize, 0usize);
        for n in self.nets.iter_mut() {
            if !n.has_action {
                n.ff_alone(steps);
            } else if n.ff_at > steps {
                // stepped past a stop that came earlier (dead or not: one that died after the
                // stop's tick still holds a marking the replay never reached): again from the
                // start, silently (its actions up to the stop are applied already, and none
                // came after them)
                let seen = (n.ff_most, n.ff_most_ctl);
                n.state.copy_from_slice(&n.initial);
                n.ff_start();
                n.ff_advance(steps, false, true);
                n.ff_most = n.ff_most.max(seen.0);
                n.ff_most_ctl = n.ff_most_ctl.max(seen.1);
            }
            debug_assert!(n.ff_dead || n.ff_at == steps, "{}: at {} of {steps}", n.id, n.ff_at);
            most += n.ff_most;
            most_ctl += n.ff_most_ctl;
        }
        if 2 * most > self.fired.len() || 2 * most_ctl > self.controls.len() {
            return false;
        }
        self.tick = steps;
        self.advance();
        self.stop_requested = false;
        self.recording = true;
        self.clear_history();
        true
    }
}

// MARK: seek stepping (`WaveGraph::fast_forward_jump`)

/// A run is applied at once only while every number in it is a small whole number (no
/// rounding anywhere): the run length per batch, the weights and the markings it moves.
const RUN_MAX: i64 = 1 << 20;
const RUN_WEIGHT_MAX: f64 = (1u64 << 24) as f64;
const RUN_STATE_MAX: f64 = (1u64 << 40) as f64;

/// `CompiledNet::ff_fate`. The survey looks at most `SURVEY_MAX` ticks ahead, in at most
/// `SURVEY_STEPS` steps (a run is one step) per net and `SURVEY_BUDGET` per graph, so building
/// a graph of nets that never settle costs a bounded few milliseconds; such a net is left
/// open, and a seek steps it.
const FATE_OPEN: u8 = 0;
const FATE_DEAD: u8 = 1;
const FATE_CYCLE: u8 = 2;
const SURVEY_MAX: i64 = 1 << 20;
const SURVEY_STEPS: i64 = 1 << 14;
const SURVEY_BUDGET: i64 = 1 << 17;

/// `v` as a whole number when it is one, bit for bit (so not -0.0, NaN or a fraction).
fn whole(v: f64, max: f64) -> Option<i64> {
    if v >= 0.0 && v <= max && (v as i64 as f64).to_bits() == v.to_bits() {
        Some(v as i64)
    } else {
        None
    }
}

fn floor_div(x: i64, y: i64) -> i64 {
    x.div_euclid(y) // y > 0
}

fn ceil_div(x: i64, y: i64) -> i64 {
    -(-x).div_euclid(y) // y > 0
}

impl CompiledNet {
    /// Seek scratch for the current marking, which is the initial one (right after `reset`).
    fn ff_start(&mut self) {
        for w in self.ff_bits.iter_mut() {
            *w = 0;
        }
        for w in self.ff_diff.iter_mut() {
            *w = 0;
        }
        self.ff_ndiff = 0;
        self.ff_count = 0;
        self.ff_dead = false;
        self.ff_pending = false;
        self.ff_multi = false;
        self.ff_at = 0;
        self.ff_most = 0;
        self.ff_most_ctl = 0;
        self.ff_bits.copy_from_slice(&self.ff_bits0);
        self.ff_lo = self.ff_lo0;
        self.ff_hi = self.ff_hi0;
    }

    /// Fill the survey fields (see the struct). Runs once, on the initial marking, as the graph
    /// is built (so it may allocate: `ff_end`), and leaves the marking as it found it.
    fn ff_survey(&mut self, budget: &mut i64) {
        let words = self.ff_bits.len();
        self.ff_bits0 = vec![0; words];
        self.ff_lo0 = words;
        self.ff_hi0 = 0;
        for t in 0..self.n_trans {
            if self.is_enabled(t) {
                self.ff_bits0[t >> 6] |= 1u64 << (t & 63);
                self.ff_lo0 = self.ff_lo0.min(t >> 6);
                self.ff_hi0 = self.ff_hi0.max((t >> 6) + 1);
            }
        }
        self.ff_fate = FATE_OPEN;
        if self.has_action {
            return; // stepped to each action at a seek, never skipped
        }
        self.ff_start();
        let mut steps = 0;
        while self.ff_at < SURVEY_MAX && steps < SURVEY_STEPS && *budget > 0 {
            steps += 1;
            *budget -= 1;
            let (count, pre) = self.ff_collect(self.ff_at + 1);
            if pre > 1 {
                // the tick may decide (every enabled transition can even lose its conflict
                // on this tick and win on the next): open
                break;
            }
            if count == 0 {
                self.ff_fate = FATE_DEAD;
                self.ff_len = self.ff_at;
                self.ff_end = self.state.clone();
                break;
            }
            let t = self.enabled[0];
            let m = self.ff_run(t, SURVEY_MAX - self.ff_at);
            if m >= 2 {
                self.ff_batch(t, m);
            } else {
                self.fire(t);
                self.ff_touched(1);
            }
            if self.midi[t] {
                self.ff_most = 1;
            }
            if self.action[t] != ControlAction::None {
                self.ff_most_ctl = 1;
            }
            self.ff_at += m;
            if self.ff_ndiff == 0 {
                // (after a run too: any tick count that lands on the initial marking repeats)
                self.ff_fate = FATE_CYCLE;
                self.ff_len = self.ff_at;
                break;
            }
        }
        self.ff_fate_most = self.ff_most;
        self.ff_fate_most_ctl = self.ff_most_ctl;
        self.state.copy_from_slice(&self.initial);
        self.ff_start();
    }

    /// Step this net alone (no control action of it moves the graph) `steps` ticks from its
    /// initial marking, using its survey.
    fn ff_alone(&mut self, steps: i64) {
        self.ff_start();
        match self.ff_fate {
            FATE_DEAD if steps >= self.ff_len => {
                self.state.copy_from_slice(&self.ff_end);
                for w in self.ff_bits.iter_mut() {
                    *w = 0;
                }
                (self.ff_lo, self.ff_hi) = (self.ff_bits.len(), 0);
                self.ff_dead = true;
            }
            FATE_CYCLE => {
                // whole cycles change nothing; the tick decides nothing on the way
                self.ff_advance(steps % self.ff_len, false, false);
                self.ff_at = steps;
            }
            FATE_DEAD => self.ff_advance(steps, false, false),
            _ => self.ff_advance(steps, false, true),
        }
        if self.ff_fate != FATE_OPEN {
            self.ff_most = self.ff_most.max(self.ff_fate_most);
            self.ff_most_ctl = self.ff_most_ctl.max(self.ff_fate_most_ctl);
        }
    }

    fn ff_set(&mut self, t: usize) {
        let w = t >> 6;
        self.ff_bits[w] |= 1u64 << (t & 63);
        self.ff_lo = self.ff_lo.min(w);
        self.ff_hi = self.ff_hi.max(w + 1);
    }

    /// What `advance_with` fires in this net at `tick`: the enabled transitions in index order,
    /// conflicts resolved, left in `enabled[..count]`. Returns (count, how many were enabled
    /// before the conflicts were resolved); more than one enabled sets `ff_multi`, the only
    /// case in which the tick can decide anything.
    fn ff_collect(&mut self, tick: i64) -> (usize, usize) {
        let mut count = 0;
        let (mut lo, mut hi) = (self.ff_hi, self.ff_lo);
        for w in self.ff_lo..self.ff_hi {
            let mut b = self.ff_bits[w];
            if b != 0 {
                lo = lo.min(w);
                hi = w + 1;
            }
            while b != 0 {
                self.enabled[count] = (w << 6) | b.trailing_zeros() as usize;
                count += 1;
                b &= b - 1;
            }
        }
        if count == 0 {
            (self.ff_lo, self.ff_hi) = (self.ff_bits.len(), 0);
        } else {
            (self.ff_lo, self.ff_hi) = (lo, hi);
        }
        let pre = count;
        if count > 1 {
            self.ff_multi = true;
            count = self.resolve_conflicts(count, tick);
        }
        (count, pre)
    }

    /// After `enabled[..count]` fired: refresh the places they touched and the transitions
    /// that consume from those places.
    fn ff_touched(&mut self, count: usize) {
        for i in 0..count {
            let t = self.enabled[i];
            for a in self.in_start[t]..self.in_start[t + 1] {
                if !self.in_inhib[a] && self.in_place[a] >= 0 {
                    self.ff_place_changed(self.in_place[a] as usize);
                }
            }
            for a in self.out_start[t]..self.out_start[t + 1] {
                if self.out_place[a] >= 0 {
                    self.ff_place_changed(self.out_place[a] as usize);
                }
            }
        }
    }

    fn ff_place_changed(&mut self, p: usize) {
        let differs = self.state[p].to_bits() != self.initial[p].to_bits();
        let (w, bit) = (p >> 6, 1u64 << (p & 63));
        if differs != (self.ff_diff[w] & bit != 0) {
            self.ff_diff[w] ^= bit;
            if differs {
                self.ff_ndiff += 1;
            } else {
                self.ff_ndiff -= 1;
            }
        }
        for c in self.cons_start[p]..self.cons_start[p + 1] {
            let t = self.cons_t[c as usize] as usize;
            if self.is_enabled(t) {
                self.ff_set(t);
            } else {
                self.ff_bits[t >> 6] &= !(1u64 << (t & 63));
            }
        }
    }

    /// The marking change of one firing of `t` on place `p` (0 for a place `t` leaves alone).
    fn ff_delta(&self, t: usize, p: i64) -> i64 {
        let mut d = 0i64;
        for a in self.in_start[t]..self.in_start[t + 1] {
            if !self.in_inhib[a] && self.in_place[a] == p {
                d -= self.in_w[a] as i64;
            }
        }
        for a in self.out_start[t]..self.out_start[t + 1] {
            if self.out_place[a] == p {
                d += self.out_w[a] as i64;
            }
        }
        d
    }

    /// `{ j >= 0 : u is enabled after j more firings of t }`, an interval `lo..=hi` (empty
    /// when lo > hi), for a `t` that `ff_run` has checked: every place `t` moves holds a
    /// whole number and moves by a whole number per firing.
    fn ff_window(&self, u: usize, t: usize) -> (i64, i64) {
        let (mut lo, mut hi) = (0i64, i64::MAX);
        if self.in_start[u] == self.in_start[u + 1] {
            return (1, 0);
        }
        for a in self.in_start[u]..self.in_start[u + 1] {
            let q = self.in_place[a];
            let w = self.in_w[a];
            let inhib = self.in_inhib[a];
            let tok = if q >= 0 { self.state[q as usize] } else { 0.0 };
            let d = if q >= 0 { self.ff_delta(t, q) } else { 0 };
            if d == 0 || !w.is_finite() || w.abs() > RUN_STATE_MAX {
                // the same answer for every j: `is_enabled`'s own comparison
                let ok = if inhib { !(tok >= w) } else { !(tok < w) };
                if !ok {
                    return (1, 0);
                }
                continue;
            }
            // tok + j·d against ceil(w): a whole marking is >= w iff it is >= ceil(w)
            let a0 = tok as i64;
            let wc = libm::ceil(w) as i64;
            if !inhib {
                // a0 + j·d >= wc
                if d > 0 {
                    lo = lo.max(ceil_div(wc - a0, d));
                } else {
                    hi = hi.min(floor_div(a0 - wc, -d));
                }
            } else {
                // a0 + j·d <= wc - 1
                if d > 0 {
                    hi = hi.min(floor_div(wc - 1 - a0, d));
                } else {
                    lo = lo.max(ceil_div(a0 - wc + 1, -d));
                }
            }
        }
        (lo.max(0), hi)
    }

    /// How many ticks in a row, from now and at most `cap`, `t` is the only transition
    /// enabled (it is now). 1 unless firing `t` that many times is plain whole-number
    /// arithmetic, bit for bit what firing it one tick at a time gives: `t`'s weights and the
    /// markings it moves are small whole numbers, and it takes from each place once (so the
    /// floor at 0 in `fire` never engages while it is enabled).
    fn ff_run(&self, t: usize, cap: i64) -> i64 {
        let cap = cap.min(RUN_MAX);
        let (i0, i1, o0, o1) = (
            self.in_start[t],
            self.in_start[t + 1],
            self.out_start[t],
            self.out_start[t + 1],
        );
        if cap < 2 || i1 - i0 + o1 - o0 > 16 {
            return 1;
        }
        // the common case first, cheaply: one firing takes what t needs from a place (a ring's
        // or a chain's token moving on). Only a shortcut to the answer the windows give.
        for a in i0..i1 {
            let p = self.in_place[a];
            if self.in_inhib[a] || p < 0 {
                continue;
            }
            let mut after = self.state[p as usize] - self.in_w[a];
            for o in o0..o1 {
                if self.out_place[o] == p {
                    after += self.out_w[o];
                }
            }
            if after < self.in_w[a] {
                return 1;
            }
        }
        let mut moves = false;
        for a in i0..i1 {
            let p = self.in_place[a];
            if self.in_inhib[a] || p < 0 {
                continue;
            }
            if whole(self.in_w[a], RUN_WEIGHT_MAX).is_none()
                || whole(self.state[p as usize], RUN_STATE_MAX).is_none()
                || (i0..a).any(|b| !self.in_inhib[b] && self.in_place[b] == p)
            {
                return 1;
            }
        }
        for a in o0..o1 {
            let p = self.out_place[a];
            if p < 0 {
                continue;
            }
            if whole(self.out_w[a], RUN_WEIGHT_MAX).is_none()
                || whole(self.state[p as usize], RUN_STATE_MAX).is_none()
            {
                return 1;
            }
        }
        for a in i0..i1 {
            if self.in_place[a] >= 0 && self.ff_delta(t, self.in_place[a]) != 0 {
                moves = true;
            }
        }
        for a in o0..o1 {
            if self.out_place[a] >= 0 && self.ff_delta(t, self.out_place[a]) != 0 {
                moves = true;
            }
        }
        if !moves {
            return cap; // firing t changes nothing: it fires every tick, alone, from here on
        }
        // t stays enabled for j in 0..=hi; any other transition joins at its first j >= 1
        let (_, hi_t) = self.ff_window(t, t);
        if hi_t < 1 {
            return 1;
        }
        let mut m = cap.min(hi_t.saturating_add(1));
        let check = |p: i64, m: &mut i64| {
            if p < 0 || self.ff_delta(t, p) == 0 {
                return;
            }
            let p = p as usize;
            for c in self.cons_start[p]..self.cons_start[p + 1] {
                let u = self.cons_t[c as usize] as usize;
                if u == t {
                    continue;
                }
                let (lo, hi) = self.ff_window(u, t);
                let first = lo.max(1);
                if first <= hi {
                    *m = (*m).min(first);
                }
            }
        };
        for a in i0..i1 {
            if !self.in_inhib[a] {
                check(self.in_place[a], &mut m);
            }
        }
        for a in o0..o1 {
            check(self.out_place[a], &mut m);
        }
        m.max(1)
    }

    /// `t` is the only transition enabled, and a chain link (`ff_chain`) holding one token
    /// (exactly 1.0) on its input: walk that token down the chain, one link per tick, for at
    /// most `cap` ticks, and return how many it moved (0: not a chain here; nothing changed).
    ///
    /// Bit for bit what firing tick by tick does. Each link reads only its input place, which
    /// nothing else reads, and writes only its output place, so firing it disables it and
    /// enables the next link alone (the output's single reader): one transition enabled per
    /// tick, nothing for the tick to decide, nothing else in the net touched. A place is entered
    /// only while it holds +0.0, so every move is 1.0 - 1.0 = 0.0 and 0.0 + 1.0 = 1.0 exactly.
    /// The walk stops at a link that is not one (an action, a MIDI binding, a branch, a merge
    /// with another reader), at a place that already holds something, or back on its start (a
    /// ring: the cycle skip takes over from there).
    fn ff_chain_run(&mut self, t: usize, cap: i64) -> i64 {
        if self.ff_chain[t] == 0 || cap < 1 {
            return 0;
        }
        let p0 = self.in_place[self.in_start[t]] as usize;
        if self.state[p0].to_bits() != 1f64.to_bits() {
            return 0;
        }
        let zero = 0f64.to_bits();
        self.state[p0] = 0.0; // the token is on its way
        let (mut cur, mut at, mut m) = (t, p0, 0i64);
        while m < cap {
            let code = self.ff_chain[cur];
            if code == 0 {
                break; // `cur` fires the ordinary way
            }
            let q = (code - 1) as usize;
            if self.state[q].to_bits() != zero {
                break;
            }
            m += 1; // `cur` fires: the token is on q
            at = q;
            if q == p0 {
                break;
            }
            let (c0, c1) = (self.cons_start[q] as usize, self.cons_start[q + 1] as usize);
            if c1 - c0 != 1 {
                break; // no reader (it rests here) or several (they decide)
            }
            cur = self.cons_t[c0] as usize;
        }
        self.state[at] = 1.0;
        if m == 0 {
            return 0;
        }
        self.ff_place_changed(p0);
        if at != p0 {
            self.ff_place_changed(at);
        }
        m
    }

    /// Fire `t` `m` times at once (`ff_run` said that is exact), then refresh as after a
    /// firing.
    fn ff_batch(&mut self, t: usize, m: i64) {
        for a in self.in_start[t]..self.in_start[t + 1] {
            let p = self.in_place[a];
            if !self.in_inhib[a] && p >= 0 {
                let p = p as usize;
                self.state[p] = (self.state[p] as i64 - m * self.in_w[a] as i64) as f64;
            }
        }
        for a in self.out_start[t]..self.out_start[t + 1] {
            let p = self.out_place[a];
            if p >= 0 {
                let p = p as usize;
                self.state[p] = (self.state[p] as i64 + m * self.out_w[a] as i64) as f64;
            }
        }
        self.enabled[0] = t;
        self.ff_touched(1);
    }

    /// Step this net alone from tick `ff_at` up to `bound`, tick numbers running from 1 as the
    /// replay's do. With `stop_at_action`, stop short on the tick before one that fires a
    /// control action, its firing set left in `enabled[..ff_count]` and `ff_pending` set. With
    /// `cycles`, a net back on its initial marking (never two transitions enabled at once)
    /// skips the whole cycles left. Keeps `ff_most`, the most MIDI-bound transitions fired on
    /// one tick (`ff_most_ctl`: with any control action), and sets `ff_dead` when nothing is
    /// enabled.
    fn ff_advance(&mut self, bound: i64, stop_at_action: bool, mut cycles: bool) {
        while self.ff_at < bound {
            let (count, pre) = self.ff_collect(self.ff_at + 1);
            if count == 0 && pre > 0 {
                // every enabled transition lost a conflict on this tick: nothing fires, the
                // marking stands, and the next tick's draw may pick a winner (the replay
                // moves on the same way)
                self.ff_at += 1;
                continue;
            }
            if count == 0 {
                self.ff_dead = true; // only this net's own firing could change that
                return;
            }
            if stop_at_action
                && self.enabled[..count]
                    .iter()
                    .any(|&t| self.action[t] != ControlAction::None)
            {
                self.ff_count = count;
                self.ff_pending = true;
                return;
            }
            if pre == 1 {
                let t = self.enabled[0];
                let c = self.ff_chain_run(t, bound - self.ff_at);
                if c > 0 {
                    // (chain links are silent: no MIDI, no action to count)
                    self.ff_at += c;
                } else {
                    let m = self.ff_run(t, bound - self.ff_at);
                    if m >= 2 {
                        self.ff_batch(t, m);
                    } else {
                        self.fire(t);
                        self.ff_touched(1);
                    }
                    if self.midi[t] {
                        self.ff_most = self.ff_most.max(1);
                    }
                    if self.action[t] != ControlAction::None {
                        self.ff_most_ctl = self.ff_most_ctl.max(1);
                    }
                    self.ff_at += m;
                }
            } else {
                let (mut midi, mut ctl) = (0, 0);
                for i in 0..count {
                    let t = self.enabled[i];
                    self.fire(t);
                    if self.midi[t] {
                        midi += 1;
                    }
                    if self.action[t] != ControlAction::None {
                        ctl += 1;
                    }
                }
                self.ff_touched(count);
                self.ff_most = self.ff_most.max(midi);
                self.ff_most_ctl = self.ff_most_ctl.max(ctl);
                self.ff_at += 1;
            }
            if cycles && !self.ff_multi && self.ff_ndiff == 0 {
                // Back on the initial marking bit for bit after ff_at ticks, each with one
                // transition at most enabled (so the tick decided nothing): every later cycle
                // repeats this one, and only the remainder is left to step.
                let p = self.ff_at;
                self.ff_at += (bound - self.ff_at) / p * p;
                cycles = false;
            }
        }
    }
}

/// `pInvariants`: basis of `{ y : yᵀC = 0 }` by Gaussian elimination on Cᵀ
/// (nTrans×nPlaces, row-major).
pub fn p_invariants(mat: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n_places = mat.len();
    let n_trans = if n_places > 0 { mat[0].len() } else { 0 };
    let mut a = vec![0.0f64; n_trans * n_places];
    for t in 0..n_trans {
        for p in 0..n_places {
            a[t * n_places + p] = mat[p][t];
        }
    }
    let mut pivots: Vec<usize> = Vec::new();
    let mut r = 0usize;
    let mut c = 0usize;
    while c < n_places && r < n_trans {
        let cc = c;
        c += 1;
        let mut best = r;
        for i in (r + 1)..n_trans {
            if libm::fabs(a[i * n_places + cc]) > libm::fabs(a[best * n_places + cc]) {
                best = i;
            }
        }
        if libm::fabs(a[best * n_places + cc]) < 1e-12 {
            continue;
        }
        if best != r {
            for j in 0..n_places {
                a.swap(r * n_places + j, best * n_places + j);
            }
        }
        let piv = a[r * n_places + cc];
        for j in 0..n_places {
            a[r * n_places + j] /= piv;
        }
        for i in 0..n_trans {
            if i == r || a[i * n_places + cc] == 0.0 {
                continue;
            }
            let f = a[i * n_places + cc];
            for j in 0..n_places {
                a[i * n_places + j] -= f * a[r * n_places + j];
            }
        }
        pivots.push(cc);
        r += 1;
    }
    let mut is_pivot = vec![false; n_places];
    for &c in &pivots {
        is_pivot[c] = true;
    }
    let mut out = Vec::new();
    for fc in 0..n_places {
        if is_pivot[fc] {
            continue;
        }
        let mut y = vec![0.0; n_places];
        y[fc] = 1.0;
        for (row, &pc) in pivots.iter().enumerate() {
            y[pc] = -a[row * n_places + fc];
        }
        out.push(y);
    }
    out
}

#[cfg(test)]
mod fast_forward_tests {
    //! `fast_forward` (control nets ticked, the rest stepped alone and cycle-skipped) against
    //! `fast_forward_replay` (every net ticked): every field the replay leaves defined, and the
    //! ticks that follow.
    use super::*;
    use crate::json::{JsonObject, JsonValue};
    use alloc::format;
    use alloc::string::ToString;

    #[derive(PartialEq, Debug)]
    struct Snap {
        tick: i64,
        states: Vec<Vec<u64>>,
        muted: Vec<u8>,
        muted_notes: Vec<u8>,
        muted_groups: Vec<u8>,
        active_slot: Vec<i64>,
        stop_requested: bool,
        fired: Vec<i32>,
        controls: Vec<i32>,
        overflow: i64,
        timers: (Vec<i32>, Vec<u8>, Vec<i32>, Vec<i32>, i64),
        history: (usize, usize, usize, bool, i64),
        repeat: (i64, i64, i64, i64),
    }

    fn snap(g: &WaveGraph) -> Snap {
        Snap {
            tick: g.tick,
            states: g
                .nets
                .iter()
                .map(|n| n.state.iter().map(|v| v.to_bits()).collect())
                .collect(),
            muted: g.muted.clone(),
            muted_notes: g.muted_notes.clone(),
            muted_groups: g.muted_groups.clone(),
            active_slot: g.active_slot.clone(),
            stop_requested: g.stop_requested,
            fired: g.fired[..g.fired_count].to_vec(),
            controls: g.controls[..g.control_count].to_vec(),
            overflow: g.overflow,
            timers: (
                g.timer_remaining.clone(),
                g.timer_kind.clone(),
                g.timer_net.clone(),
                g.timer_arg.clone(),
                g.timer_overflow,
            ),
            history: (
                g.h_ticks,
                g.h_fire_count,
                g.h_change_count,
                g.recording,
                g.fire_need_cache,
            ),
            repeat: (
                g.repeat_remaining,
                g.repeat_elapsed,
                g.repeat_step,
                g.repeat_burst,
            ),
        }
    }

    /// Both ways from a graph that has already played (dirty scratch, a mute, a pending
    /// macro timer), then 40 more ticks each.
    fn check(name: &str, g0: &WaveGraph, targets: &[i64]) {
        check_with(name, g0, targets, true)
    }

    fn check_with(name: &str, g0: &WaveGraph, targets: &[i64], jumps: bool) {
        let mut played = g0.clone();
        for _ in 0..23 {
            played.advance();
        }
        if !played.nets.is_empty() {
            played.set_muted(0, 1);
        }
        for &t in targets {
            let mut a = played.clone();
            let mut b = played.clone();
            if t > 0 {
                // (a seek to tick 1 is the last tick alone: a real advance, which counts its own overflow)
                let want = jumps || t == 1;
                assert_eq!(played.clone().fast_forward_jump(t), want, "{name}: which path at {t}");
            }
            a.fast_forward(t);
            b.fast_forward_replay(t);
            assert_eq!(snap(&a), snap(&b), "{name}: fast_forward({t})");
            for i in 0..40 {
                a.advance();
                b.advance();
                assert_eq!(snap(&a), snap(&b), "{name}: fast_forward({t}) then tick {i}");
            }
            // and a second seek on the same graph (scratch from the first)
            a.fast_forward(t / 2 + 1);
            b.fast_forward_replay(t / 2 + 1);
            assert_eq!(snap(&a), snap(&b), "{name}: second seek after {t}");
        }
    }

    fn composed(genre: &str, seed: f64, structure: &str) -> WaveGraph {
        let mut ov = JsonObject::new();
        ov.set("seed", seed.into());
        if structure != "loop" {
            ov.set("structure", structure.into());
        }
        WaveGraph::new(&crate::compose(genre, &ov.into()))
    }

    const TARGETS: &[i64] = &[0, 1, 2, 3, 15, 16, 17, 63, 64, 65, 100, 257, 1000, 3000, 7777];

    #[test]
    fn composed_tracks_land_where_the_replay_does() {
        let genres = [
            "ambient", "blues", "bossa", "country", "dnb", "dubstep", "edm", "funk", "garage",
            "house", "jazz", "lofi", "metal", "reggae", "speedcore", "synthwave", "techno",
            "trance", "trap",
        ];
        let structures = ["loop", "ab", "drop", "build", "jam", "minimal", "standard", "extended"];
        for (i, genre) in genres.iter().enumerate() {
            for (j, s) in structures.iter().enumerate() {
                // every genre with two structures, every structure with several genres
                if (i + j) % 4 != 0 {
                    continue;
                }
                let g = composed(genre, (i * 7 + j) as f64 + 1.0, s);
                check(&format!("{genre}:{s}"), &g, TARGETS);
            }
        }
        check("techno:42:standard", &composed("techno", 42.0, "standard"), TARGETS);
        check("techno:42:extended", &composed("techno", 42.0, "extended"), TARGETS);
    }

    fn net(places: &[(&str, f64)], trans: &[(&str, &str)], arcs: &[(&str, &str, f64, bool)]) -> JsonValue {
        let mut ps = JsonObject::new();
        for (id, init) in places {
            let mut p = JsonObject::new();
            p.set("initial", JsonValue::Array(alloc::vec![JsonValue::Number(*init)]));
            ps.set(id.to_string(), p.into());
        }
        let mut ts = JsonObject::new();
        for (id, kind) in trans {
            let mut t = JsonObject::new();
            if let Some(rest) = kind.strip_prefix("midi:") {
                let mut m = JsonObject::new();
                m.set("note", JsonValue::Number(rest.parse::<f64>().unwrap()));
                m.set("velocity", JsonValue::Number(100.0));
                m.set("duration", JsonValue::Number(100.0));
                t.set("midi", m.into());
            } else if !kind.is_empty() {
                let mut parts = kind.split(':');
                let mut c = JsonObject::new();
                c.set("action", parts.next().unwrap().into());
                if let Some(target) = parts.next() {
                    c.set("targetNet", target.into());
                }
                if let Some(note) = parts.next() {
                    c.set("targetNote", JsonValue::Number(note.parse::<f64>().unwrap()));
                }
                t.set("control", c.into());
            }
            ts.set(id.to_string(), t.into());
        }
        let mut az = Vec::new();
        for (s, t, w, inh) in arcs {
            let mut a = JsonObject::new();
            a.set("source", (*s).into());
            a.set("target", (*t).into());
            a.set("weight", JsonValue::Array(alloc::vec![JsonValue::Number(*w)]));
            if *inh {
                a.set("inhibit", true.into());
            }
            az.push(a.into());
        }
        let mut n = JsonObject::new();
        n.set("places", ps.into());
        n.set("transitions", ts.into());
        n.set("arcs", JsonValue::Array(az));
        n.into()
    }

    fn ring(n: usize, hits: usize, token: &str) -> JsonValue {
        let ps: Vec<(String, f64)> = (0..n).map(|i| (format!("p{i}"), if i == 0 { 1.0 } else { 0.0 })).collect();
        let ts: Vec<(String, String)> = (0..n)
            .map(|i| (format!("t{i}"), if i % hits == 0 { format!("midi:{}", 36 + i) } else { String::new() }))
            .collect();
        let mut arcs = Vec::new();
        for i in 0..n {
            arcs.push((format!("p{i}"), format!("t{i}"), 1.0, false));
            arcs.push((format!("t{i}"), format!("p{}", (i + 1) % n), 1.0, false));
        }
        let _ = token;
        net(
            &ps.iter().map(|(a, b)| (a.as_str(), *b)).collect::<Vec<_>>(),
            &ts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
            &arcs.iter().map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, *d)).collect::<Vec<_>>(),
        )
    }

    fn graph(nets: &[(&str, JsonValue, Option<&str>)], mutes: &[&str]) -> WaveGraph {
        let mut ns = JsonObject::new();
        for (id, n, group) in nets {
            let mut n = n.clone();
            if let Some(g) = group {
                if let JsonValue::Object(o) = &mut n {
                    let mut tr = JsonObject::new();
                    tr.set("channel", JsonValue::Number(1.0));
                    o.set("riffGroup", (*g).into());
                    o.set("track", tr.into());
                }
            }
            ns.set(id.to_string(), n);
        }
        let mut d = JsonObject::new();
        d.set("nets", ns.into());
        d.set("tempo", JsonValue::Number(120.0));
        d.set(
            "initialMutes",
            JsonValue::Array(mutes.iter().map(|m| (*m).into()).collect()),
        );
        WaveGraph::from_json(&d.into())
    }

    #[test]
    fn hand_built_nets_land_where_the_replay_does() {
        // a contested place (two consumers, the tick decides), weighted arcs, an inhibitor,
        // an arc from a place the net does not have, a transient before a cycle, a dead end
        let contest = net(
            &[("a", 1.0), ("b", 0.0), ("c", 0.0)],
            &[("x", "midi:60"), ("y", "midi:62"), ("back1", ""), ("back2", "")],
            &[
                ("a", "x", 1.0, false),
                ("a", "y", 1.0, false),
                ("x", "b", 1.0, false),
                ("y", "c", 1.0, false),
                ("b", "back1", 1.0, false),
                ("c", "back2", 1.0, false),
                ("back1", "a", 1.0, false),
                ("back2", "a", 1.0, false),
            ],
        );
        // a contest whose branches differ in length: the marking comes back to the initial one
        // after 2 or 3 ticks as the tick decides, so no cycle may be skipped
        let lopsided = net(
            &[("a", 1.0), ("b", 0.0)],
            &[("short", "midi:64"), ("long", "midi:65"), ("home", "")],
            &[
                ("a", "short", 1.0, false),
                ("a", "long", 1.0, false),
                ("short", "a", 1.0, false),
                ("long", "b", 1.0, false),
                ("b", "home", 1.0, false),
                ("home", "a", 1.0, false),
            ],
        );
        let weighted = net(
            &[("a", 5.0), ("b", 0.0), ("gate", 0.0)],
            &[("take", "midi:40"), ("give", ""), ("inhib", "midi:41"), ("ghost", "")],
            &[
                ("a", "take", 2.0, false),
                ("take", "b", 3.0, false),
                ("b", "give", 4.0, false),
                ("give", "a", 1.5, false),
                ("gate", "inhib", 1.0, true),
                ("b", "inhib", 1.0, false),
                ("inhib", "gate", 0.5, false),
                ("nowhere", "ghost", 0.0, false),
            ],
        );
        let lead_in = net(
            &[("s0", 1.0), ("s1", 0.0), ("r0", 0.0), ("r1", 0.0), ("r2", 0.0)],
            &[("i0", ""), ("i1", "midi:50"), ("c0", "midi:51"), ("c1", ""), ("c2", "midi:52")],
            &[
                ("s0", "i0", 1.0, false),
                ("i0", "s1", 1.0, false),
                ("s1", "i1", 1.0, false),
                ("i1", "r0", 1.0, false),
                ("r0", "c0", 1.0, false),
                ("c0", "r1", 1.0, false),
                ("r1", "c1", 1.0, false),
                ("c1", "r2", 1.0, false),
                ("r2", "c2", 1.0, false),
                ("c2", "r0", 1.0, false),
            ],
        );
        let parallel = net(
            &[("a", 1.0), ("b", 1.0)],
            &[("ta", "midi:70"), ("tb", "midi:71")],
            &[("a", "ta", 1.0, false), ("ta", "a", 1.0, false), ("b", "tb", 1.0, false), ("tb", "b", 1.0, false)],
        );
        let dead_end = net(
            &[("a", 3.0), ("b", 0.0)],
            &[("t", "midi:30")],
            &[("a", "t", 1.0, false), ("t", "b", 1.0, false)],
        );
        // control nets: a cyclic one that toggles and mutes notes, a chain with slots and a stop,
        // and a music ring that carries a control action itself
        let toggler = net(
            &[("q0", 1.0), ("q1", 0.0), ("q2", 0.0)],
            &[("k0", "toggle-track:bass"), ("k1", "mute-note:lead:62"), ("k2", "toggle-note:lead:60")],
            &[
                ("q0", "k0", 1.0, false),
                ("k0", "q1", 1.0, false),
                ("q1", "k1", 1.0, false),
                ("k1", "q2", 1.0, false),
                ("q2", "k2", 1.0, false),
                ("k2", "q0", 1.0, false),
            ],
        );
        let chain = |stop_at: usize| {
            let n = 400;
            let ps: Vec<(String, f64)> = (0..=n).map(|i| (format!("c{i}"), if i == 0 { 1.0 } else { 0.0 })).collect();
            let ts: Vec<(String, String)> = (0..n)
                .map(|i| {
                    let k = if i == stop_at {
                        "stop-transport".to_string()
                    } else if i % 97 == 5 {
                        "activate-slot:pad-b".to_string()
                    } else if i % 97 == 50 {
                        "activate-slot:pad-a".to_string()
                    } else if i % 61 == 3 {
                        "unmute-track:bass".to_string()
                    } else if i % 61 == 30 {
                        "mute-track:bass".to_string()
                    } else if i % 53 == 7 {
                        "fire-macro".to_string()
                    } else {
                        String::new()
                    };
                    (format!("u{i}"), k)
                })
                .collect();
            let mut arcs = Vec::new();
            for i in 0..n {
                arcs.push((format!("c{i}"), format!("u{i}"), 1.0, false));
                arcs.push((format!("u{i}"), format!("c{}", i + 1), 1.0, false));
            }
            net(
                &ps.iter().map(|(a, b)| (a.as_str(), *b)).collect::<Vec<_>>(),
                &ts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
                &arcs.iter().map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, *d)).collect::<Vec<_>>(),
            )
        };
        let mut acting_ring = ring(8, 2, "x");
        if let JsonValue::Object(o) = &mut acting_ring {
            if let Some(JsonValue::Object(ts)) = o.get_mut("transitions") {
                if let Some(JsonValue::Object(t3)) = ts.get_mut("t3") {
                    let mut c = JsonObject::new();
                    c.set("action", "toggle-track".into());
                    c.set("targetNet", "hat".into());
                    t3.set("control", c.into());
                }
            }
        }
        for (name, stop) in [("no stop", 10_000usize), ("stop at 311", 311), ("stop at 0", 0)] {
            let g = graph(
                &[
                    ("kick", ring(16, 4, "k"), None),
                    ("bass", ring(64, 3, "b"), None),
                    ("lead", contest.clone(), None),
                    ("lopsided", lopsided.clone(), None),
                    ("weighted", weighted.clone(), None),
                    ("lead-in", lead_in.clone(), None),
                    ("parallel", parallel.clone(), None),
                    ("dead", dead_end.clone(), None),
                    ("toggler", toggler.clone(), None),
                    ("struct", chain(stop), None),
                    ("pad-a", ring(12, 1, "a"), Some("pad")),
                    ("pad-b", ring(12, 5, "b"), Some("pad")),
                    ("hat", acting_ring.clone(), None),
                    ("hat2", ring(5, 1, "h"), None),
                ],
                &["bass", "pad-b"],
            );
            check(name, &g, &[0, 1, 2, 5, 6, 7, 64, 99, 311, 312, 399, 400, 401, 999, 4000]);
        }
    }

    #[test]
    fn runs_land_where_the_replay_does() {
        // struct-style countdowns (a drain and an inhibited control), a run cut short by a
        // place that grows to another transition's weight, a run cut by an inhibitor, runs
        // longer than one batch, weights that are not whole (no batching), and a self-loop
        // that changes nothing
        let countdown = |first: f64, second: f64, act1: &str, act2: &str| {
            net(
                &[("d0", first), ("g0", 1.0), ("d1", second), ("g1", 0.0), ("done", 0.0)],
                &[("drain0", ""), ("ctl0", act1), ("drain1", "midi:33"), ("ctl1", act2)],
                &[
                    ("d0", "drain0", 1.0, false),
                    ("g0", "drain0", 1.0, false),
                    ("drain0", "g0", 1.0, false),
                    ("d0", "ctl0", 1.0, true),
                    ("g0", "ctl0", 1.0, false),
                    ("ctl0", "g1", 1.0, false),
                    ("d1", "drain1", 1.0, false),
                    ("g1", "drain1", 1.0, false),
                    ("drain1", "g1", 1.0, false),
                    ("d1", "ctl1", 1.0, true),
                    ("g1", "ctl1", 1.0, false),
                    ("ctl1", "done", 1.0, false),
                ],
            )
        };
        let grow = net(
            &[("src", 1.0), ("g", 0.0), ("h", 0.0)],
            &[("s", ""), ("u", "midi:44"), ("back", "")],
            &[
                ("src", "s", 1.0, false),
                ("s", "src", 1.0, false),
                ("s", "g", 2.0, false),
                ("g", "u", 51.0, false),
                ("u", "h", 1.0, false),
                ("h", "back", 1.0, false),
            ],
        );
        let cut = net(
            &[("a", 10.0), ("b", 0.0), ("gate", 1.0), ("z", 0.0)],
            &[("t", "midi:45"), ("u", ""), ("w", "")],
            &[
                ("a", "t", 1.0, false),
                ("t", "b", 1.0, false),
                ("a", "u", 4.0, true),
                ("gate", "u", 1.0, false),
                ("u", "z", 1.0, false),
                ("b", "w", 3.5, false),
                ("w", "a", 2.0, false),
            ],
        );
        let halves = net(
            &[("d", 40.0), ("g", 1.0), ("x", 0.0)],
            &[("drain", "midi:46"), ("end", "")],
            &[
                ("d", "drain", 0.5, false),
                ("g", "drain", 1.0, false),
                ("drain", "g", 1.0, false),
                ("d", "end", 0.25, true),
                ("g", "end", 1.0, false),
                ("end", "x", 1.0, false),
            ],
        );
        let idle = net(&[("a", 1.0)], &[("t", "")], &[("a", "t", 1.0, false), ("t", "a", 1.0, false)]);
        // a run that limits itself (an inhibitor on the place it fills, then nothing is
        // enabled), and one that another transition joins after a single firing
        let self_limit = net(
            &[("src", 1.0), ("c", 0.0), ("out", 0.0)],
            &[("t", "midi:47"), ("v", "")],
            &[
                ("src", "t", 1.0, false),
                ("t", "src", 1.0, false),
                ("t", "c", 1.0, false),
                ("c", "t", 5.0, true),
                ("c", "v", 7.0, false),
                ("v", "out", 1.0, false),
            ],
        );
        // (u's firing switches s off, so a run of s that went one tick too far would show)
        let grow49 = net(
            &[("src", 1.0), ("g", 49.0), ("off", 0.0)],
            &[("s", ""), ("u", "midi:48")],
            &[
                ("src", "s", 1.0, false),
                ("s", "src", 1.0, false),
                ("s", "g", 2.0, false),
                ("off", "s", 1.0, true),
                ("g", "u", 51.0, false),
                ("u", "off", 1.0, false),
            ],
        );
        let g = graph(
            &[
                ("kick", ring(16, 4, "k"), None),
                ("bass", ring(7, 2, "b"), None),
                ("self-limit", self_limit.clone(), None),
                ("grow49", grow49.clone(), None),
                ("s-a", countdown(37.0, 13.0, "toggle-track:kick", "mute-track:bass"), None),
                ("s-b", countdown(64.0, 1000.0, "unmute-track:bass", "stop-transport"), None),
                ("s-long", countdown(1_500_000.0, 1.0, "toggle-track:bass", "toggle-track:kick"), None),
                ("long", countdown(2_500_000.0, 3.0, "", ""), None),
                ("grow", grow.clone(), None),
                ("cut", cut.clone(), None),
                ("halves", halves.clone(), None),
                ("idle", idle.clone(), None),
            ],
            &["kick"],
        );
        check("runs", &g, &[0, 1, 2, 25, 26, 27, 28, 37, 38, 39, 51, 64, 65, 66, 100, 1065, 1066, 1067, 5000]);
        // no stop: the long countdowns run past one batch
        let g = graph(
            &[
                ("kick", ring(16, 4, "k"), None),
                ("s-a", countdown(37.0, 13.0, "toggle-track:kick", "mute-track:kick"), None),
                ("s-long", countdown(1_500_000.0, 1.0, "toggle-track:kick", "toggle-track:kick"), None),
                ("long", countdown(2_500_000.0, 3.0, "", ""), None),
                ("grow", grow, None),
                ("cut", cut, None),
                ("halves", halves, None),
                ("idle", idle, None),
            ],
            &[],
        );
        check("long runs", &g, &[1_048_577, 1_500_001, 1_500_002, 2_500_003]);
    }

    #[test]
    fn mutually_blocked_ticks_and_late_dying_action_nets_land_where_the_replay_does() {
        // Two transitions that both take from the same two places: each place draws its own
        // winner, so on a tick where the draws differ both lose and nothing fires, and the
        // next tick's draws may pick one. A seek that called that net dead froze it on a
        // marking the playback never reaches.
        let blocked = net(
            &[("a", 1.0), ("b", 1.0), ("c", 0.0), ("d", 0.0)],
            &[("x", "midi:60"), ("y", "midi:61"), ("back1", ""), ("back2", "")],
            &[
                ("a", "x", 1.0, false),
                ("b", "x", 1.0, false),
                ("a", "y", 1.0, false),
                ("b", "y", 1.0, false),
                ("x", "c", 1.0, false),
                ("y", "d", 1.0, false),
                ("c", "back1", 1.0, false),
                ("d", "back2", 1.0, false),
                ("back1", "a", 1.0, false),
                ("back1", "b", 1.0, false),
                ("back2", "a", 1.0, false),
                ("back2", "b", 1.0, false),
            ],
        );
        // the same shape with no way back: until one of them wins, nothing fires on a tick
        // and the net is not dead (two are still enabled)
        let blocked_once = net(
            &[("a", 1.0), ("b", 1.0), ("c", 0.0), ("d", 0.0)],
            &[("x", "midi:62"), ("y", "midi:63")],
            &[
                ("a", "x", 1.0, false),
                ("b", "x", 1.0, false),
                ("a", "y", 1.0, false),
                ("b", "y", 1.0, false),
                ("x", "c", 1.0, false),
                ("y", "d", 1.0, false),
            ],
        );
        let all: Vec<i64> = (0..70).chain([99, 100, 127, 128, 333, 1000, 5000]).collect();
        let g = graph(
            &[("kick", ring(16, 4, "k"), None), ("blocked", blocked.clone(), None), ("once", blocked_once, None)],
            &[],
        );
        check("mutually blocked", &g, &all);
        // the seek fuzzer's smallest case: a duplicate arc and a half weight
        let doc = r#"{"tempo":120,"nets":{"n0":{"role":"music","track":{"channel":1},"riffGroup":"g0","places":{"p0":{"initial":[1]},"p1":{"initial":[2.5]},"p2":{"initial":[0]},"p3":{"initial":[1]},"p4":{"initial":[1]}},"transitions":{"t0":{"midi":{"note":61,"velocity":80,"duration":100}},"t1":{"midi":{"note":62,"velocity":80,"duration":100}}},"arcs":[{"source":"p0","target":"t0","weight":[1]},{"source":"t0","target":"p1","weight":[1]},{"source":"p1","target":"t1","weight":[1]},{"source":"t1","target":"p2","weight":[1]},{"source":"p1","target":"t0","weight":[1],"inhibit":false},{"source":"t0","target":"p3","weight":[1]},{"source":"p3","target":"t0","weight":[0.5],"inhibit":false},{"source":"t1","target":"p0","weight":[1]},{"source":"p1","target":"t0","weight":[1],"inhibit":false},{"source":"p0","target":"t1","weight":[1],"inhibit":false},{"source":"t0","target":"p2","weight":[2]}]}}}"#;
        let g = WaveGraph::from_json(&JsonValue::parse(doc).expect("case parses"));
        check("fuzz case 1", &g, &all);

        // An action net that acts on tick 1, then runs a silent tail and dies on tick 12,
        // past the stop-transport another net fires on tick 4: the replay stops on tick 4,
        // so that net must be rewound to tick 3 although it is dead by then.
        let late_dier = {
            let n = 12;
            let ps: Vec<(String, f64)> = (0..=n).map(|i| (format!("c{i}"), if i == 0 { 1.0 } else { 0.0 })).collect();
            let ts: Vec<(String, String)> = (0..n)
                .map(|i| (format!("u{i}"), if i == 0 { "mute-track:kick".to_string() } else { String::new() }))
                .collect();
            let mut arcs = Vec::new();
            for i in 0..n {
                arcs.push((format!("c{i}"), format!("u{i}"), 1.0, false));
                arcs.push((format!("u{i}"), format!("c{}", i + 1), 1.0, false));
            }
            net(
                &ps.iter().map(|(a, b)| (a.as_str(), *b)).collect::<Vec<_>>(),
                &ts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
                &arcs.iter().map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, *d)).collect::<Vec<_>>(),
            )
        };
        let stopper = net(
            &[("s0", 1.0), ("s1", 0.0), ("s2", 0.0), ("s3", 0.0), ("s4", 0.0)],
            &[("v0", ""), ("v1", ""), ("v2", ""), ("v3", "stop-transport")],
            &[
                ("s0", "v0", 1.0, false),
                ("v0", "s1", 1.0, false),
                ("s1", "v1", 1.0, false),
                ("v1", "s2", 1.0, false),
                ("s2", "v2", 1.0, false),
                ("v2", "s3", 1.0, false),
                ("s3", "v3", 1.0, false),
                ("v3", "s4", 1.0, false),
            ],
        );
        let g = graph(
            &[("kick", ring(16, 4, "k"), None), ("late", late_dier, None), ("stop", stopper, None), ("blocked", blocked, None)],
            &[],
        );
        check("late dier", &g, &all);
        // (check plays 23 ticks first; from a fresh graph too)
        for &t in &all {
            let mut a = g.clone();
            let mut b = g.clone();
            a.fast_forward(t);
            b.fast_forward_replay(t);
            assert_eq!(snap(&a), snap(&b), "late dier, fresh: fast_forward({t})");
        }
    }

    #[test]
    fn a_tick_that_could_overflow_takes_the_replay() {
        // 3000 one-place self-loops that all fire every tick: more MIDI fires than `fired`
        // holds, so the replay counts overflows and fast_forward must take the replay too
        let n = 3000;
        let ps: Vec<(String, f64)> = (0..n).map(|i| (format!("p{i}"), 1.0)).collect();
        let ts: Vec<(String, String)> = (0..n).map(|i| (format!("t{i}"), format!("midi:{}", i % 100))).collect();
        let mut arcs = Vec::new();
        for i in 0..n {
            arcs.push((format!("p{i}"), format!("t{i}"), 1.0, false));
            arcs.push((format!("t{i}"), format!("p{i}"), 1.0, false));
        }
        let big = net(
            &ps.iter().map(|(a, b)| (a.as_str(), *b)).collect::<Vec<_>>(),
            &ts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
            &arcs.iter().map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, *d)).collect::<Vec<_>>(),
        );
        let g = graph(&[("a", big.clone(), None), ("b", big, None)], &[]);
        let mut a = g.clone();
        a.fast_forward(20);
        assert!(a.overflow > 0, "the test net overflows `fired`");
        check_with("overflow", &g, &[1, 2, 20], false);
    }
}
