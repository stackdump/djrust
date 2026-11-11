//! Port of `Musicality/Backbeat.swift` and `BeatStyle.swift` (`DJMusicality.Fixes.backbeat`):
//! kick and snare in chill / jazz verse and chorus.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::clamp_velocity;
use crate::musicality::note_log::NoteEvent;
use crate::pflow::{ControlBinding, MidiBinding, NetBundle, OrderedMap, Project};
use crate::wave::groove::Voice;
use crate::wave::synth::is_drum_channel;

/// `beatStyle(genre:)`: the sections where a genre's style has a beat. None when the style
/// has none anywhere (ambient) or the genre is not in the catalogue.
pub fn beat_style(genre: &str) -> Option<&'static [&'static str]> {
    match crate::gen::tables::BeatsTables::shared().kin(genre) {
        "techno" | "house" | "edm" | "trance" | "dnb" | "dubstep" | "speedcore" | "garage"
        | "trap" | "lofi" => Some(&["verse", "pre-chorus", "chorus", "buildup", "drop"]),
        "country" | "blues" | "funk" | "reggae" | "synthwave" | "metal" | "jazz" | "bossa" => {
            Some(&["verse", "pre-chorus", "chorus", "drop"])
        }
        _ => None,
    }
}

/// `hasBeat`: whether a stretch of the note log has a beat: at least half its bars carry
/// both a kick (35, 36) and a snare or clap (37–40). Fill chains don't count.
pub fn has_beat(log: &[NoteEvent], start: i64, steps: i64) -> bool {
    let bars = (steps / 16).max(1);
    let mut kick = alloc::collections::BTreeSet::new();
    let mut snare = alloc::collections::BTreeSet::new();
    for e in log {
        if !(e.is_drum() && !e.net.starts_with("fill-") && e.tick > start) {
            continue;
        }
        if e.tick > start + steps {
            continue;
        }
        let bar = (e.tick - start - 1) / 16;
        match Voice::from_note(e.note) {
            Some(Voice::Kick) => {
                kick.insert(bar);
            }
            Some(Voice::Snare) => {
                snare.insert(bar);
            }
            _ => {}
        }
    }
    2 * kick.intersection(&snare).count() as i64 >= bars
}

/// `BackbeatFigure`: a drum part's figure, as shares of the part's loudest composed hit:
/// `bar` by sixteenth in a 16-step bar, `beat` by sixteenth in one beat.
#[derive(Clone, Debug)]
pub struct BackbeatFigure {
    pub bar: Vec<(i64, f64)>,
    pub beat: Vec<(i64, f64)>,
}

/// `BackbeatStyle`: the chill family's kick and snare.
#[derive(Clone, Debug)]
pub struct BackbeatStyle {
    pub kick: BackbeatFigure,
    pub snare: BackbeatFigure,
}

impl BackbeatStyle {
    pub fn figure(&self, role: &str) -> &BackbeatFigure {
        if role == "kick" {
            &self.kick
        } else {
            &self.snare
        }
    }
}

/// `backbeatStyle(genre:)`: the figures by genre; None outside the chill / jazz family.
pub fn backbeat_style(genre: &str) -> Option<BackbeatStyle> {
    let f = |bar: &[(i64, f64)], beat: &[(i64, f64)]| BackbeatFigure {
        bar: bar.to_vec(),
        beat: beat.to_vec(),
    };
    match genre {
        // boom-bap: kick on 1, the a of 2 and the and of 3; the snare's crack on 2 and 4
        "lofi" => Some(BackbeatStyle {
            kick: f(&[(0, 1.0), (7, 0.55), (10, 0.85)], &[(0, 0.9)]),
            snare: f(&[(4, 1.0), (12, 1.0)], &[(0, 0.8)]),
        }),
        // a feathered kick under a soft 2 and 4; brushes on a ring of whole beats
        "jazz" => Some(BackbeatStyle {
            kick: f(&[(0, 0.45), (4, 0.45), (8, 0.45), (12, 0.45)], &[(0, 0.45)]),
            snare: f(&[(4, 0.55), (12, 0.55)], &[(0, 0.4)]),
        }),
        // the surdo's dotted quarter and eighth, leaning on 3; the rim plays the clave
        "bossa" => Some(BackbeatStyle {
            kick: f(&[(0, 0.8), (6, 0.55), (8, 1.0), (14, 0.55)], &[(0, 0.8)]),
            snare: f(
                &[(0, 0.9), (3, 0.75), (6, 0.85), (10, 0.9), (13, 0.75)],
                &[(0, 0.8)],
            ),
        }),
        _ => None,
    }
}

/// The parts `.backbeat` brings in.
pub const BACKBEAT_ROLES: [&str; 2] = ["kick", "snare"];

/// `playBackbeat`.
pub fn play_backbeat(p: &mut Project, genre: &str) {
    backbeat(p, genre);
}

/// `backbeat`: returns the parts it left alone and why.
pub fn backbeat(p: &mut Project, genre: &str) -> BTreeMap<String, String> {
    let mut skipped: BTreeMap<String, String> = BTreeMap::new();
    let (Some(style), Some(beat)) = (backbeat_style(genre), beat_style(genre)) else {
        return skipped;
    };
    if p.structure.is_empty() {
        return skipped;
    }
    let mut starts: Vec<i64> = Vec::new();
    let mut total = 0;
    for s in &p.structure {
        starts.push(total);
        total += s.steps;
    }
    for role in BACKBEAT_ROLES {
        if p.nets.values().any(|nb| nb.riff_group == role) {
            skipped.insert(
                role.into(),
                format!("slotted: struct-{role} activates a slot per phrase"),
            );
            continue;
        }
        let Some(net) = p.nets.get(role) else {
            skipped.insert(role.into(), format!("no {role} net"));
            continue;
        };
        let Some(figure) = backbeat_bindings(net, style.figure(role)) else {
            skipped.insert(role.into(), "not a ring of whole beats".into());
            continue;
        };
        let control_id = format!("struct-{role}");
        let stages = p
            .nets
            .get(&control_id)
            .and_then(|c| structure_stages(c, role));
        let (Some(control), Some(stages)) = (p.nets.get(&control_id), stages) else {
            skipped.insert(
                role.into(),
                format!("struct-{role} is not a chain of mutes and unmutes"),
            );
            continue;
        };

        // Where the composer had the part, where the style wants it, and the edges between.
        let mut composed: Vec<bool> = Vec::new();
        for (start, section) in starts.iter().zip(p.structure.iter()) {
            let mut on = !p.initial_mutes.iter().any(|m| m == role);
            for s in &stages {
                if s.0 <= start + section.steps / 2 {
                    on = s.1;
                }
            }
            composed.push(on);
        }
        let wanted: Vec<bool> = (0..p.structure.len())
            .map(|i| composed[i] || beat.contains(&p.structure[i].name.as_str()))
            .collect();
        let mut edges: Vec<(i64, bool)> = Vec::new();
        for k in 1..wanted.len() {
            if wanted[k] != wanted[k - 1] {
                edges.push((starts[k], wanted[k]));
            }
        }
        let wanted_at =
            |step: i64| -> bool { wanted[starts.iter().rposition(|&s| s <= step).unwrap_or(0)] };
        let action = |unmute: bool| -> ControlBinding {
            ControlBinding::new(if unmute { "unmute-track" } else { "mute-track" }, role)
        };

        let mut q = p.clone();
        let mut ctl = control.clone();
        if stages.len() >= edges.len() {
            // Retime the chain's own stages onto the edges.
            let mut prev = 0i64;
            let mut last = wanted[0];
            for i in 0..stages.len() {
                let tick = if i < edges.len() {
                    edges[i].0
                } else {
                    prev + 1
                };
                let unmute = if i < edges.len() { edges[i].1 } else { last };
                if let Some(pl) = ctl.places.get_mut(&format!("p_delay_{i}")) {
                    pl.initial = alloc::vec![(tick - prev - 1) as f64];
                }
                ctl.control_bindings
                    .insert(format!("t_ctrl_{i}"), action(unmute));
                prev = tick;
                last = unmute;
            }
        } else {
            // Each stage says what the plan says at its tick; the edges ride on fill chains.
            for (i, s) in stages.iter().enumerate() {
                ctl.control_bindings
                    .insert(format!("t_ctrl_{i}"), action(wanted_at(s.0)));
            }
            let mut chains: Vec<(String, usize)> = q
                .nets
                .iter()
                .filter(|(k, nb)| k.starts_with("fill-") && is_sink_chain(nb))
                .map(|(k, nb)| (k.into(), nb.transitions.len()))
                .collect();
            chains.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let mut carried = true;
            for e in &edges {
                let t = format!("t{}", e.0 - 1);
                let id = chains.iter().find(|c| {
                    let nb = q.nets.get(&c.0).unwrap();
                    nb.transitions.contains_key(&t) && !nb.control_bindings.contains_key(&t)
                });
                let Some(id) = id else {
                    carried = false;
                    break;
                };
                let id = id.0.clone();
                q.nets
                    .get_mut(&id)
                    .unwrap()
                    .control_bindings
                    .insert(t, action(e.1));
            }
            if !carried {
                skipped.insert(
                    role.into(),
                    "no stage or fill chain left to carry a section edge".into(),
                );
                continue;
            }
        }
        q.nets.insert(control_id, ctl);
        if wanted[0] {
            q.initial_mutes.retain(|m| m != role);
        } else if !q.initial_mutes.iter().any(|m| m == role) {
            q.initial_mutes.push(role.into());
        }
        q.nets.get_mut(role).unwrap().bindings = figure;
        *p = q;
    }
    skipped
}

/// `structureStages`: the stages of an accumulator control net: the tick each fires on and
/// whether it unmutes `role`. `[]` for the empty chain the composer builds for a part that
/// never changes; None for anything else.
pub fn structure_stages(nb: &NetBundle, role: &str) -> Option<Vec<(i64, bool)>> {
    if nb.role != "control" {
        return None;
    }
    if !nb.places.contains_key("p_delay_0") {
        return if nb.control_bindings.is_empty() {
            Some(Vec::new())
        } else {
            None
        };
    }
    let mut out: Vec<(i64, bool)> = Vec::new();
    let mut tick = 0i64;
    while let Some(gap) = nb
        .places
        .get(&format!("p_delay_{}", out.len()))
        .and_then(|p| p.initial.first())
    {
        let c = nb.control_bindings.get(&format!("t_ctrl_{}", out.len()))?;
        if c.target_net != role || !(c.action == "mute-track" || c.action == "unmute-track") {
            return None;
        }
        tick += *gap as i64 + 1;
        out.push((tick, c.action == "unmute-track"));
    }
    Some(out)
}

/// `isSinkChain`: a chain `p0 → t0 → p1 → … → tN-1 → pN` (`drumFillNet`).
pub fn is_sink_chain(nb: &NetBundle) -> bool {
    let n = nb.transitions.len();
    n > 0
        && nb.places.len() == n + 1
        && nb.places.get("p0").and_then(|p| p.initial.first()) == Some(&1.0)
        && nb.transitions.contains_key(&format!("t{}", n - 1))
}

/// `backbeatBindings`: `figure` on a one-token ring of drum hits, from where its token
/// starts. None when the net is no such ring or holds no whole beat.
pub fn backbeat_bindings(
    nb: &NetBundle,
    figure: &BackbeatFigure,
) -> Option<OrderedMap<MidiBinding>> {
    let n = nb.transitions.len();
    if n < 4 || nb.places.len() != n {
        return None;
    }
    if !(0..n).all(|i| {
        nb.places.contains_key(&format!("p{i}")) && nb.transitions.contains_key(&format!("t{i}"))
    }) {
        return None;
    }
    let at = (0..n).find(|i| {
        nb.places
            .get(&format!("p{i}"))
            .and_then(|p| p.initial.first())
            .copied()
            .unwrap_or(0.0)
            > 0.0
    })?;
    let mut peak: Option<MidiBinding> = None;
    for b in nb.bindings.values().filter(|b| is_drum_channel(b.channel)) {
        if peak.map_or(true, |p| p.velocity < b.velocity) {
            peak = Some(*b);
        }
    }
    let peak = peak?;
    let (period, cell) = if n % 16 == 0 {
        (16usize, &figure.bar)
    } else {
        (4usize, &figure.beat)
    };
    if n % period != 0 {
        return None;
    }
    let mut share: Vec<Option<f64>> = alloc::vec![None; n];
    for k in 0..(n / period) {
        for &(pos, s) in cell {
            if pos >= 0 && (pos as usize) < period {
                share[(at + k * period + pos as usize) % n] = Some(s);
            }
        }
    }
    let mut out: OrderedMap<MidiBinding> = OrderedMap::new();
    for i in 0..n {
        let Some(s) = share[i] else { continue };
        let mut b = peak;
        b.velocity = clamp_velocity(libm::round(peak.velocity as f64 * s) as i64);
        out.insert(format!("t{i}"), b);
    }
    Some(out)
}
