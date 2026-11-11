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
