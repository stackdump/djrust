//! The runner's arrangement gestures as rests (`RestSchedule.build` and the planners it
//! calls): `.dropGap`, `.layerBudget` (`LayerBudget.swift`), `.bandStop` (`BandStop.swift`)
//! and `.kickOut` (`KickOut.swift`). Pure functions of the played project.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::arrange::ArrangeTables;
use crate::gen::techniques::{auto_layer_budget, LAYER_PRIORITY};
use crate::musicality::dj::{drop_gaps, DROP_GAP_STEPS, RISING_SECTIONS};
use crate::musicality::fixes::Fixes;
use crate::musicality::note_log;
use crate::musicality::slot_windows::{net_windows, slot_windows};
use crate::musicality::util::part;
use crate::pflow::Project;
use crate::wave::net::is_stinger_id;
use crate::wave::rests::{Class, Entry, RestSchedule};
use crate::wave::synth::is_drum_channel;

/// `RestSchedule.build(_:fixes:genre:)`: the schedule `fixes` give `project`.
pub fn build_rests(project: &Project, fixes: Fixes, genre: Option<&str>) -> RestSchedule {
    let mut gestures: Vec<Entry> = Vec::new();
    if fixes.contains(Fixes::DROP_GAP) {
        for s in drop_gaps(project) {
            gestures.push(Entry::new(s, s + DROP_GAP_STEPS, Class::RHYTHM));
        }
    }
    if fixes.contains(Fixes::LAYER_BUDGET) {
        gestures.extend(layer_budget_rests(project));
    }
    if fixes.contains(Fixes::BAND_STOP) {
        gestures.extend(band_stop_rests(project, genre));
    }
    if fixes.contains(Fixes::KICK_OUT) {
        gestures.extend(kick_out_rests(project));
    }
    RestSchedule::merging(&gestures)
}

// MARK: layer budget

/// One section's budget: the melodic parts that play in it, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerBudget {
    pub cap: usize,
    pub playing: Vec<Class>,
}

impl LayerBudget {
    pub fn kept(&self) -> Vec<Class> {
        self.playing.iter().take(self.cap).copied().collect()
    }
    pub fn rested(&self) -> Vec<Class> {
        self.playing.iter().skip(self.cap).copied().collect()
    }
}

/// The group the composer gives a melodic role (`groupFor` in Composer.swift).
pub fn layer_group(role: &str) -> &str {
    match role {
        "melody" | "lead" => "melody",
        "pad" | "chord" | "chords" | "harmony" => "harmony",
        other => other,
    }
}

/// The melodic parts in the order a budget keeps them: bass, melody, harmony, arp.
pub fn layer_class_priority() -> Vec<Class> {
    let mut out: Vec<Class> = Vec::new();
    for role in LAYER_PRIORITY {
        let c = Class::of("tonal", layer_group(role), role);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// `layerBudgetRests`: per section, the first `autoLayerBudget[name]` melodic parts kept, the
/// rest resting over the whole section.
pub fn layer_budget_rests(project: &Project) -> Vec<Entry> {
    let budgets = layer_budgets(project);
    let mut out = Vec::new();
    let mut start = 0;
    for (i, s) in project.structure.iter().enumerate() {
        let rested = budgets[i].rested();
        if !rested.is_empty() && s.steps > 0 {
            let mut mask = Class::NONE;
            for c in rested {
                mask = mask.union(c);
            }
            out.push(Entry::new(start, start + s.steps, mask));
        }
        start += s.steps;
    }
    out
}

/// `layerBudgets`: each section's budget and which of the melodic parts play in it.
pub fn layer_budgets(project: &Project) -> Vec<LayerBudget> {
    let priority = layer_class_priority();
    let mut roles: BTreeMap<String, Class> = BTreeMap::new();
    let mut single: BTreeMap<String, Class> = BTreeMap::new();
    for (id, nb) in project.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        if nb.track.group.as_deref() == Some("stinger")
            || is_stinger_id(id)
            || id.starts_with("fill-")
        {
            continue;
        }
        let c = Class::of("tonal", part(nb), id);
        if !priority.contains(&c) {
            continue;
        }
        if nb.riff_group.is_empty() {
            single.insert(id.into(), c);
        } else {
            roles.insert(nb.riff_group.clone(), c);
        }
    }
    let mut playing = alloc::vec![Class::NONE; project.structure.len()];
    for (role, c) in &roles {
        for windows in slot_windows(project, role).values() {
            for w in windows {
                if w.section < playing.len() {
                    playing[w.section] = playing[w.section].union(*c);
                }
            }
        }
    }
    for (id, c) in &single {
        for w in net_windows(project, id) {
            if w.section < playing.len() {
                playing[w.section] = playing[w.section].union(*c);
            }
        }
    }
    project
        .structure
        .iter()
        .enumerate()
        .map(|(i, s)| LayerBudget {
            cap: auto_layer_budget(&s.name) as usize,
            playing: priority
                .iter()
                .copied()
                .filter(|c| !playing[i].is_disjoint(*c))
                .collect(),
        })
        .collect()
}

// MARK: band stop

/// How long the band stops before a chorus (`.bandStop`): a beat, in sixteenths.
pub const BAND_STOP_STEPS: i64 = 4;

/// What stops: every part but the lead, fill chains' drums included.
pub const BAND_STOP_MASK: Class = Class(
    Class::KICK.0
        | Class::DRUMS.0
        | Class::BASS.0
        | Class::HARMONY.0
        | Class::ARP.0
        | Class::TONAL.0
        | Class::FILL.0,
);

/// `bandStopGenre`: beats' song family.
pub fn band_stop_genre(genre: &str) -> bool {
    ArrangeTables::family(genre) == Some("song")
}

/// `bandStopRests`: None-genre or a genre outside the song family gives no rests.
pub fn band_stop_rests(project: &Project, genre: Option<&str>) -> Vec<Entry> {
    let Some(genre) = genre.filter(|g| band_stop_genre(g)) else {
        return Vec::new();
    };
    let _ = genre;
    band_stops(project)
        .into_iter()
        .map(|s| Entry::new(s, s + BAND_STOP_STEPS, BAND_STOP_MASK))
        .collect()
}

/// `bandStops`: first step of each band stop, ascending.
pub fn band_stops(p: &Project) -> Vec<i64> {
    let mut candidates: Vec<i64> = Vec::new();
    let mut t = 0;
    for (k, s) in p.structure.iter().enumerate() {
        if k > 0
            && t >= BAND_STOP_STEPS
            && s.name == "chorus"
            && !RISING_SECTIONS.contains(&p.structure[k - 1].name.as_str())
        {
            candidates.push(t - BAND_STOP_STEPS);
        }
        t += s.steps;
    }
    let Some(&last) = candidates.last() else {
        return Vec::new();
    };
    let onsets = lead_onsets(p, last + BAND_STOP_STEPS);
    candidates
        .into_iter()
        .filter(|c| onsets.iter().any(|&o| o >= *c && o < *c + BAND_STOP_STEPS))
        .collect()
}

/// `leadOnsets`: the song steps on which the lead starts an audible note in the first
/// `ticks` steps, ascending.
pub fn lead_onsets(p: &Project, ticks: i64) -> Vec<i64> {
    let mut lead: BTreeSet<String> = BTreeSet::new();
    for (id, nb) in p.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        let group = nb.track.group.as_deref().unwrap_or(&nb.riff_group);
        if Class::of("tonal", group, id) == Class::MELODY
            && nb.bindings.values().any(|b| !is_drum_channel(b.channel))
        {
            lead.insert(id.into());
        }
    }
    if lead.is_empty() {
        return Vec::new();
    }
    let ids: BTreeSet<&str> = lead.iter().map(|s| s.as_str()).collect();
    let mut only = crate::musicality::slot_windows::part_only(p, &ids);
    for (id, nb) in p.nets.iter() {
        if lead.contains(id) {
            only.nets.insert(id, nb.clone());
        }
    }
    only.initial_mutes = p
        .initial_mutes
        .iter()
        .filter(|m| lead.contains(*m))
        .cloned()
        .collect();
    let mut out: Vec<i64> = Vec::new();
    for e in note_log::capture(&only, Some(ticks)) {
        if lead.contains(&e.net) && !e.is_drum() {
            // a note at tick n (1-based: after the fire) plays song step n - 1
            if out.last() != Some(&(e.tick - 1)) {
                out.push(e.tick - 1);
            }
        }
    }
    out
}

// MARK: kick out

/// How long the kick sits out at the end of a buildup (`.kickOut`): a bar, in sixteenths.
pub const KICK_OUT_STEPS: i64 = 16;

/// `kickOutRests`: every buildup of at least two bars that leads straight into a drop loses
/// its kick for its last `KICK_OUT_STEPS`.
pub fn kick_out_rests(project: &Project) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut t = 0;
    let structure = &project.structure;
    for (k, s) in structure.iter().enumerate() {
        let end = t + s.steps;
        if s.name == "buildup"
            && s.steps >= 2 * KICK_OUT_STEPS
            && k + 1 < structure.len()
            && structure[k + 1].name == "drop"
        {
            out.push(Entry::new(end - KICK_OUT_STEPS, end, Class::KICK));
        }
        t = end;
    }
    out
}
