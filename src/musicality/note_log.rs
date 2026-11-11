//! Port of `Musicality/NoteLog.swift`: the notes a project plays, from simulating its nets
//! tick by tick on the wave engine's graph (no audio).

use alloc::string::String;
use alloc::vec::Vec;

use crate::musicality::section_harmony::SectionHarmony;
use crate::pflow::{NetBundle, Project};
use crate::wave::net::{CompiledNet, ControlAction, WaveGraph};
use crate::wave::synth::is_drum_channel;

/// `NoteEvent`: one audible note of a net, as the wave engine's clock fires it.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteEvent {
    /// The graph's tick after the fire (1-based: the first tick of the song is 1).
    pub tick: i64,
    pub net: String,
    /// The net's mixer group (`drums`, `bass`, …), else its riff group, else its id.
    pub group: String,
    pub note: i64,
    pub velocity: i64,
    pub channel: i64,
    /// Length in sixteenths (from `durationSteps`, else the ms length at the project's tempo).
    pub steps: f64,
}

impl NoteEvent {
    pub fn is_drum(&self) -> bool {
        is_drum_channel(self.channel)
    }
}

/// `PartChange`: a control net muting, unmuting or switching a part.
#[derive(Clone, Debug, PartialEq)]
pub struct PartChange {
    pub tick: i64,
    pub net: String,
    pub action: ControlAction,
    pub target: String,
    /// For a slot switch onto a ring net: where the ring's token sits after the tick.
    pub phase: Option<i64>,
    /// The first song step the change acts on (`actingStep`).
    pub step: i64,
}

impl PartChange {
    /// `PartChange.actingStep(tick:control:target:)`: tick − 1 when the control net comes
    /// before its target in the graph's order, else `tick`.
    pub fn acting_step(tick: i64, control: usize, target: Option<usize>) -> i64 {
        if let Some(target) = target {
            if control < target {
                return tick - 1;
            }
        }
        tick
    }
}

/// `NoteLog.length`: the song's length in ticks.
pub fn length(project: &Project) -> i64 {
    let s: i64 = project.structure.iter().map(|s| s.steps).sum();
    if s > 0 {
        s
    } else {
        project.bars.unwrap_or(4).max(1) * 16
    }
}

/// `NoteLog.groupName`: mixer group, else riff group, else the id.
pub fn group_name(nb: &NetBundle, id: &str) -> String {
    match nb.track.group.as_deref() {
        Some(g) if !g.is_empty() => g.into(),
        _ if !nb.riff_group.is_empty() => nb.riff_group.clone(),
        _ => id.into(),
    }
}

/// `NoteLog.capture`: every note `project` plays in its first `ticks` ticks.
pub fn capture(project: &Project, ticks: Option<i64>) -> Vec<NoteEvent> {
    capture_all(project, ticks).0
}

/// `NoteLog.captureAll`: the notes and the part changes of the first `ticks`.
pub fn capture_all(project: &Project, ticks: Option<i64>) -> (Vec<NoteEvent>, Vec<PartChange>) {
    capture_all_with(project, ticks, None)
}

/// `NoteLog.captureAll(_:ticks:harmony:)`: with a section plan, tonal notes are mapped
/// through it, as the runner plays them under `.sectionHarmony`.
pub fn capture_all_with(
    project: &Project,
    ticks: Option<i64>,
    harmony: Option<&SectionHarmony>,
) -> (Vec<NoteEvent>, Vec<PartChange>) {
    run(
        &mut WaveGraph::new(project),
        ticks.unwrap_or_else(|| length(project)),
        harmony,
    )
}

/// `NoteLog.run`.
pub fn run(
    g: &mut WaveGraph,
    ticks: i64,
    harmony: Option<&SectionHarmony>,
) -> (Vec<NoteEvent>, Vec<PartChange>) {
    let mut out: Vec<NoteEvent> = Vec::new();
    let mut controls: Vec<PartChange> = Vec::new();
    let tempo = if g.tempo > 0.0 { g.tempo } else { 120.0 };
    let ms_per_step = 60000.0 / (tempo * 4.0);
    let group: Vec<String> = g
        .nets
        .iter()
        .map(|n| group_name(&n.bundle, &n.id))
        .collect();
    let harmony_group: Vec<Option<usize>> = group
        .iter()
        .map(|name| harmony.and_then(|h| h.group_index(name)))
        .collect();
    for _ in 0..ticks {
        let tick = g.tick + 1;
        {
            let out = &mut out;
            let group = &group;
            let harmony_group = &harmony_group;
            g.advance_with(|n: &CompiledNet, t: usize| {
                let steps = if n.dur_steps[t] > 0.0 {
                    n.dur_steps[t]
                } else {
                    n.dur_ms[t] / ms_per_step
                };
                let channel = n.channel[t] as i64;
                let mut note = n.note[t];
                if let (Some(h), Some(gi)) = (harmony, harmony_group[n.index]) {
                    if !is_drum_channel(channel) {
                        note = h.note(note, gi, tick - 1);
                    }
                }
                out.push(NoteEvent {
                    tick,
                    net: n.id.clone(),
                    group: group[n.index].clone(),
                    note,
                    velocity: n.vel[t],
                    channel,
                    steps,
                });
            });
        }
        let mut i = 0;
        while i < g.control_count {
            let n = &g.nets[g.controls[i] as usize];
            let t = g.controls[i + 1] as usize;
            let a = n.action[t];
            i += 2;
            if !matches!(
                a,
                ControlAction::Mute
                    | ControlAction::Unmute
                    | ControlAction::Toggle
                    | ControlAction::Slot
            ) {
                continue;
            }
            let target = n.action_target[t].clone();
            let by_id = g.by_id.get(&target).copied();
            let mut phase = None;
            if a == ControlAction::Slot {
                if let Some(ti) = by_id {
                    phase = ring_position(&g.nets[ti]);
                }
            }
            controls.push(PartChange {
                tick: g.tick,
                net: n.id.clone(),
                action: a,
                target,
                phase,
                step: PartChange::acting_step(g.tick, n.index, by_id),
            });
        }
        if g.stop_requested {
            break;
        }
    }
    (out, controls)
}

/// `NoteLog.ringPosition`: the index of the marked `p<i>` place of a ring net, else None.
pub fn ring_position(n: &CompiledNet) -> Option<i64> {
    let mut at: Option<i64> = None;
    for (i, id) in n.place_ids.iter().enumerate() {
        if n.state[i] > 0.0 {
            let k = id.strip_prefix('p').and_then(|s| s.parse::<i64>().ok());
            match (at, k) {
                (None, Some(k)) => at = Some(k),
                _ => return None,
            }
        }
    }
    at
}
