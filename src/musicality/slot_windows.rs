//! Port of `Musicality/SlotWindows.swift`: which slot net of a role plays which steps of
//! which section.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::musicality::dj::phrase_lengths;
use crate::musicality::note_log;
use crate::pflow::{NetBundle, Project};
use crate::wave::net::ControlAction;

/// `Structure.swift`'s `boundaryStagger`.
use crate::gen::structure::BOUNDARY_STAGGER;

/// `SlotWindow`: a stretch of one section during which one slot net of a role plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotWindow {
    /// Index into `Project.structure`.
    pub section: usize,
    pub section_name: String,
    /// Index into the section's `phrases[role]`: the phrase `start` falls in.
    pub phrase: usize,
    /// Song steps (sixteenths from 0), `end` exclusive.
    pub start: i64,
    pub end: i64,
    /// The step of the net's ring cycle it plays at `start`; None for a net that isn't a
    /// one-token ring.
    pub ring_start: Option<i64>,
    /// The ring's length in steps, None with `ring_start`.
    pub ring_length: Option<i64>,
}

/// `DJMusicality.slotWindows(_:role:)`: windows of the nets whose `riffGroup` is `role`,
/// keyed by net.
pub fn slot_windows(project: &Project, role: &str) -> BTreeMap<String, Vec<SlotWindow>> {
    let nets: Vec<(String, &NetBundle)> = project
        .nets
        .iter()
        .filter(|(_, nb)| nb.riff_group == role && nb.role != "control")
        .map(|(k, nb)| (k.into(), nb))
        .collect();
    windows(project, role, &nets)
}

/// `DJMusicality.netWindows`: the windows of one net that isn't a slot net, keyed by its id.
pub fn net_windows(project: &Project, id: &str) -> Vec<SlotWindow> {
    let Some(nb) = project.nets.get(id) else {
        return Vec::new();
    };
    if nb.role == "control" {
        return Vec::new();
    }
    windows(project, id, &[(id.into(), nb)])
        .remove(id)
        .unwrap_or_default()
}

fn windows(
    project: &Project,
    role: &str,
    nets: &[(String, &NetBundle)],
) -> BTreeMap<String, Vec<SlotWindow>> {
    if nets.is_empty() || project.structure.is_empty() {
        return BTreeMap::new();
    }
    let ids: BTreeSet<&str> = nets.iter().map(|(k, _)| k.as_str()).collect();
    let rings: BTreeMap<&str, (i64, i64)> = nets
        .iter()
        .filter_map(|(id, nb)| ring_shape(nb).map(|r| (id.as_str(), r)))
        .collect();

    // The role's changes, as (step from which they hold, net playing or None).
    let order: BTreeMap<&str, usize> = project
        .nets
        .keys()
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let muted: BTreeSet<&str> = project.initial_mutes.iter().map(|s| s.as_str()).collect();
    let initial: Option<String> = nets
        .iter()
        .find(|(k, _)| !muted.contains(k.as_str()))
        .map(|(k, _)| k.clone());
    let mut playing = initial.clone();
    let mut changes: Vec<(i64, Option<String>)> = Vec::new();
    let part = part_only(project, &ids);
    let (_, controls) = note_log::capture_all(&part, Some(note_log::length(project)));
    for c in controls.iter().filter(|c| ids.contains(c.target.as_str())) {
        match c.action {
            ControlAction::Slot | ControlAction::Unmute => playing = Some(c.target.clone()),
            ControlAction::Mute => {
                if playing.as_deref() == Some(c.target.as_str()) {
                    playing = None;
                }
            }
            ControlAction::Toggle => {
                playing = if playing.as_deref() == Some(c.target.as_str()) {
                    None
                } else {
                    Some(c.target.clone())
                };
            }
            _ => continue,
        }
        let step = note_log::PartChange::acting_step(
            c.tick,
            order.get(c.net.as_str()).copied().unwrap_or(usize::MAX),
            order.get(c.target.as_str()).copied(),
        );
        changes.push((step, playing.clone()));
    }

    let mut out: BTreeMap<String, Vec<SlotWindow>> = BTreeMap::new();
    let mut section_start = 0i64;
    let mut k = 0usize; // next change
    let mut current = initial;
    for (si, s) in project.structure.iter().enumerate() {
        let end = section_start + s.steps;
        let count = (s.phrases.get(role).map_or(1, |p| p.len() as i64)).max(1);
        let inside: Vec<i64> = changes
            .iter()
            .map(|c| c.0)
            .filter(|&c| c > section_start && c < end)
            .collect();
        let aligned = phrase_starts(section_start, &phrase_lengths(s.steps, count));
        let even = phrase_starts(section_start, &alloc::vec![s.steps / count; count as usize]);
        // A change sits on a grid when it's at most the boundary stagger after a phrase start.
        let fits = |grid: &[i64]| -> bool {
            inside.iter().all(|&c| {
                grid.iter()
                    .any(|&g| c - g >= 0 && c - g <= BOUNDARY_STAGGER + 1)
            })
        };
        let grid = if fits(&aligned) || !fits(&even) {
            aligned
        } else {
            even
        };
        let mut cuts: BTreeSet<i64> = grid
            .iter()
            .copied()
            .filter(|&g| g >= section_start && g < end)
            .collect();
        cuts.insert(section_start);
        for c in &inside {
            cuts.insert(*c);
        }
        let starts: Vec<i64> = cuts.into_iter().collect();
        for (i, &a) in starts.iter().enumerate() {
            while k < changes.len() && changes[k].0 <= a {
                current = changes[k].1.clone();
                k += 1;
            }
            let Some(net) = current.clone() else { continue };
            let b = if i + 1 < starts.len() {
                starts[i + 1]
            } else {
                end
            };
            let phrase = grid.iter().rposition(|&g| g <= a).unwrap_or(0);
            let ring = rings.get(net.as_str()).map(|&(at, n)| (at + a) % n);
            let w = SlotWindow {
                section: si,
                section_name: s.name.clone(),
                phrase,
                start: a,
                end: b,
                ring_start: ring,
                ring_length: rings.get(net.as_str()).map(|r| r.1),
            };
            // A change inside a phrase that keeps the same net (a re-sent slot) joins its window.
            let list = out.entry(net).or_default();
            if let Some(last) = list.last_mut() {
                if last.end == a && last.section == si && last.phrase == phrase {
                    last.end = b;
                    continue;
                }
            }
            list.push(w);
        }
        section_start = end;
    }
    out
}

/// `partOnly`: `project` with only the control nets that act on the nets `ids`.
pub fn part_only(project: &Project, ids: &BTreeSet<&str>) -> Project {
    let mut p = project.clone();
    p.nets = crate::pflow::OrderedMap::new();
    for (id, nb) in project.nets.iter() {
        if nb
            .control_bindings
            .values()
            .any(|c| ids.contains(c.target_net.as_str()))
        {
            p.nets.insert(id, nb.clone());
        }
    }
    p.initial_mutes = Vec::new();
    p
}

fn phrase_starts(start: i64, lengths: &[i64]) -> Vec<i64> {
    let mut t = start;
    lengths
        .iter()
        .map(|l| {
            let r = t;
            t += l;
            r
        })
        .collect()
}

/// `ringShape`: a one-token ring of places `p0`…`pN-1`: where its token starts and N.
fn ring_shape(nb: &NetBundle) -> Option<(i64, i64)> {
    let mut at: Option<i64> = None;
    let mut count = 0i64;
    for (id, place) in nb.places.iter() {
        let i = id.strip_prefix('p')?.parse::<i64>().ok()?;
        count = count.max(i + 1);
        if place.initial.first().copied().unwrap_or(0.0) > 0.0 {
            if at.is_some() {
                return None;
            }
            at = Some(i);
        }
    }
    let at = at?;
    if count > 0 && count == nb.places.len() as i64 {
        Some((at, count))
    } else {
        None
    }
}
