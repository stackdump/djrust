//! The deck half of `Wander.swift` (lines 766-1041) that is a function of explicit inputs: what
//! a deck tells the planner about itself (`Deck.wanderVoices`, `wanderKey`, `wanderContext`),
//! where a swap starts (`wanderSwapStart`) and the JSON `dj_state` shows (`wanderJSON`).
//!
//! The calls that touch a live deck (`advanceWander`, `takeWanderMove`, the look-ahead slot, the
//! pokes a move makes) are `crate::host::deck::wander`; they gather these inputs from the deck and
//! apply what [`Wander::plan`] decides.
//!
//! Also here, as free functions on a [`Project`], the project edits a sound poke makes
//! (`Project.palette`, `setPalette`, `drumKit`, `curateStingers`, `resolvedStingers` and the
//! project half of `Deck.setRoleVoice`): the palette hop of a raw-`nets` deck is built on them.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::carry::project_channels;
use super::{Wander, WanderContext, WanderKey, WanderPalettes, WanderState, WanderVoice};
use crate::cid::share_cid;
use crate::deck::slots::SwapStart;
use crate::gen::tables::BeatsTables;
use crate::json::JsonValue;
use crate::layers::{LayerEntry, LayerLoop};
use crate::pflow::{Project, StingerAssignment};
use crate::sound::stinger::{self, AssignmentKind, Key, StingerContent};
use crate::sound::{
    apply_project_sound, channels_for_role, is_drum_channel, PaletteSet, SoundNet, Sounds,
};
use crate::spec::TrackSpec;
use crate::wander::WanderRandom;

/// `Deck.wanderVoices`: the tonal (and drum-kit) channels of the net with beats' instrument
/// lists, in role order. `roles` are the deck's (`Deck.roles`), `genre` the spec's ("" without
/// one). The net a role plays on is the first non-control one, by id, whose riff group or id is
/// the role; roles that share a channel are one voice.
pub fn wander_voices(project: &Project, roles: &[String], genre: &str) -> Vec<WanderVoice> {
    let sets = &BeatsTables::shared().instrument_sets;
    let mut ids: Vec<&str> = project.nets.keys().iter().map(String::as_str).collect();
    ids.sort_unstable();
    let mut out: Vec<WanderVoice> = Vec::new();
    for role in roles {
        let net = ids.iter().find_map(|id| {
            let nb = project.nets.get(id)?;
            (nb.role != "control" && (nb.riff_group == *role || *id == role.as_str())).then_some(nb)
        });
        let Some(net) = net else { continue };
        if let Some(v) = out.iter_mut().find(|v| v.channel == net.track.channel) {
            v.roles.push(role.clone());
            continue;
        }
        let mut options: Vec<String> = BeatsTables::shared()
            .instrument_set(genre)
            .and_then(|m| m.get(role))
            .cloned()
            .unwrap_or_default();
        for (id, set) in sets.iter() {
            if id == genre {
                continue;
            }
            for i in set.get(role).map(Vec::as_slice).unwrap_or(&[]) {
                if !options.contains(i) {
                    options.push(i.clone());
                }
            }
        }
        // a palette's synth voice is not an instrument the user chose: the composer's is
        let t = &net.track;
        out.push(WanderVoice {
            roles: alloc::vec![role.clone()],
            channel: t.channel,
            instrument: if t.picked {
                t.instrument.clone()
            } else {
                t.sound
                    .composed
                    .clone()
                    .unwrap_or_else(|| t.instrument.clone())
            },
            options,
            overridden: t.sound.override_.is_some(),
            option: if t.picked {
                None
            } else {
                t.sound.option.clone()
            },
        });
    }
    out
}

/// `Deck.wanderKey`: the key the track plays in now. `project` is the one on the deck.
pub fn wander_key(genre: &str, project: Option<&Project>) -> Option<WanderKey> {
    Wander::played_key(genre, project.and_then(|p| p.root_note))
}

/// `Deck.wanderSwapStart(bar:)`: where the swap of the move at `bar` starts: the next phrase's
/// first bar, the transport still running (a loop starts at its top).
pub fn wander_swap_start(is_net_loop: bool, bar: i64) -> SwapStart {
    if is_net_loop {
        SwapStart::Top
    } else {
        SwapStart::Tick((bar + 1) * 16)
    }
}

/// Everything `Deck.wanderContext(lockedActions:)` reads off a deck.
pub struct WanderDeckInputs<'a> {
    pub deck: &'a str,
    /// `Deck.currentSpec`.
    pub spec: Option<&'a TrackSpec>,
    /// `BeatsCID.shareCID` of `Deck.currentRawNets`, when the deck holds a raw-`nets` share
    /// and no spec.
    pub raw_cid: Option<String>,
    /// `Deck.isNetLoop`.
    pub is_loop: bool,
    /// `Deck.roles`.
    pub roles: Vec<String>,
    /// The `roles` that are audible (`isRoleOn`).
    pub roles_on: BTreeSet<String>,
    pub disabled_roles: BTreeSet<String>,
    /// The keys of `Deck.stemsMuted`.
    pub stems_muted: BTreeSet<String>,
    /// `DJEngine.lockedActions`.
    pub locked_actions: BTreeSet<String>,
    pub wander: &'a WanderState,
    /// `Deck.netProject`.
    pub project: &'a Project,
    pub layers: Vec<LayerEntry>,
    pub layer_pool: Vec<LayerLoop>,
    /// `Deck.stingers`, one label each: `"stinger"` for a Hit that fell back.
    pub stingers: Vec<String>,
    /// The palettes a hop may reach, built by the host from what is installed.
    pub palettes: Option<WanderPalettes>,
}

/// `Deck.wanderContext(lockedActions:)`: None unless the deck holds a spec or a raw-`nets`
/// share. A raw deck walks on the moves that need no spec: its spec is a placeholder and its
/// draws start from its CID.
pub fn wander_context(i: WanderDeckInputs<'_>) -> Option<WanderContext> {
    let (spec, raw_base) = if let Some(s) = i.spec {
        (s.clone(), None)
    } else if let Some(cid) = &i.raw_cid {
        (
            TrackSpec::new("", 0.0, None),
            Some(WanderRandom::base_of_cid(cid)),
        )
    } else {
        return None;
    };
    let genre = i.spec.map(|s| s.genre.as_str()).unwrap_or("");
    let voices = wander_voices(i.project, &i.roles, genre);
    let mut c = WanderContext::new(i.deck, spec, i.is_loop, &[], &[], &[], voices);
    c.roles = i.roles;
    c.roles_on = i.roles_on;
    c.disabled_roles = i.disabled_roles;
    c.stems_muted = i.stems_muted;
    c.locked_actions = i.locked_actions;
    c.locks = i.wander.locks.clone();
    c.anchor_bpm = i.wander.anchor_bpm;
    c.key = i.wander.key.clone();
    c.palette = project_palette(i.project);
    c.kit = project_kit(i.project);
    c.channels = project_channels(i.project);
    c.stingers = i.stingers;
    c.raw_base = raw_base;
    c.layers = i.layers;
    c.layer_pool = i.layer_pool;
    c.pace = i.wander.pace;
    c.palettes = i.palettes;
    Some(c)
}

/// The label a Hit goes by in a context (`kind == .fallback ? "stinger" : label`).
pub fn stinger_labels(hits: &[StingerAssignment]) -> Vec<String> {
    hits.iter()
        .map(|a| {
            if a.kind == AssignmentKind::Fallback {
                "stinger".to_string()
            } else {
                a.label.clone()
            }
        })
        .collect()
}

/// `BeatsCID.shareCID` of a raw-`nets` payload (what a raw deck's walk is seeded from).
pub fn raw_cid(payload: &JsonValue) -> String {
    share_cid(payload)
}

// MARK: the project's sound fields

/// `Project.palette`: the palette the track plays from (the first playing net's).
pub fn project_palette(p: &Project) -> Option<String> {
    crate::deck::swap::project_palette(p)
}

/// `Project.drumKit`: the kit the drums play (`Track.kit` of the first drum net), None = the
/// synth kit.
pub fn project_kit(p: &Project) -> Option<String> {
    for id in p.nets.keys() {
        let Some(nb) = p.nets.get(id) else { continue };
        if nb.role == "control" || !is_drum_channel(nb.track.channel) {
            continue;
        }
        return nb.track.kit.clone();
    }
    None
}

/// `Project.setPalette`: every net (control nets too) plays from `palette`.
pub fn set_project_palette(p: &mut Project, palette: Option<&str>) {
    let ids: Vec<String> = p.nets.keys().to_vec();
    for id in ids {
        if let Some(nb) = p.nets.get_mut(&id) {
            nb.track.palette = palette.map(String::from);
        }
    }
}

/// `Project.channels(soundRole:)`: the channels whose nets play as `role`.
pub fn project_role_channels(p: &Project, role: &str) -> Vec<i64> {
    channels_for_role(&SoundNet::from_project(p, None), role)
}

/// `Project.curateStingers(_:seed:)`: play `assignments` on the project's Hits (they are kept in
/// `stingers`, each one-shot's option goes on its stinger net as `TrackSound.curated`), and
/// resolve the sounds again.
pub fn curate_project_stingers(
    p: &mut Project,
    assignments: Vec<StingerAssignment>,
    seed: f64,
    set: &PaletteSet,
    ctx: Sounds,
) {
    for a in &assignments {
        if let Some(nb) = p.nets.get_mut(&a.net_id()) {
            nb.track.sound.curated = if a.kind == AssignmentKind::OneShot {
                a.option.clone()
            } else {
                None
            };
        }
    }
    p.stingers = assignments;
    apply_project_sound(p, seed, set, ctx);
}

/// `Project.resolvedStingers(genre:seed:installed:)`: the project's Hits resolved for `genre`
/// from its palette, key and tempo.
pub fn resolved_project_stingers(
    p: &Project,
    set: &PaletteSet,
    genre: &str,
    seed: f64,
    installed: &StingerContent,
) -> Vec<StingerAssignment> {
    let key = p.root_note.map(|r| Key::from_scale(r, &p.scale_name));
    stinger::resolve(
        set,
        genre,
        project_palette(p).as_deref(),
        seed,
        installed,
        (p.tempo > 0.0).then_some(p.tempo),
        key.as_ref(),
    )
}

/// The project half of `Deck.setRoleVoice`: `role` plays `voice` (None: back to the palette's
/// pick). The override goes on every net of the role's channels; an instrument the user picked
/// the older way is dropped (the composer's comes back), and so is a kit pick when the drums go
/// back to the palette's. Returns the role's channels, and None when there is nothing to
/// change (no channel, or no net's choice moves); the project's sounds are then resolved again
/// (`applySound(seed:)`).
pub fn set_project_role_voice(
    p: &mut Project,
    role: &str,
    voice: Option<&str>,
    seed: f64,
    set: &PaletteSet,
    ctx: Sounds,
) -> Option<Vec<i64>> {
    let voice = voice.filter(|v| !v.is_empty());
    let channels = project_role_channels(p, role);
    if channels.is_empty() {
        return None;
    }
    let mut changed = false;
    let ids: Vec<String> = p.nets.keys().to_vec();
    for id in ids {
        let Some(nb) = p.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" || !channels.contains(&nb.track.channel) {
            continue;
        }
        if nb.track.sound.override_.as_deref() != voice {
            changed = true;
        }
        nb.track.sound.override_ = voice.map(String::from);
        if nb.track.picked {
            nb.track.picked = false;
            if let Some(c) = nb.track.sound.composed.clone() {
                nb.track.instrument = c;
            }
            changed = true;
        }
        if role == "kit" && voice.is_none() && nb.track.kit.is_some() {
            nb.track.kit = None;
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    apply_project_sound(p, seed, set, ctx);
    Some(channels)
}

/// The channels a palette is written to on a composed deck: the project's own and every role
/// channel of the genre (`TrackSpec.soundChannels`), so a palette set here and one chosen
/// before Generate name the same configuration.
pub fn palette_channels(p: &Project, genre: Option<&str>) -> Vec<i64> {
    let mut channels = project_channels(p);
    if let Some(g) = genre {
        for c in crate::sound::sound_channels(g) {
            if !channels.contains(&c) {
                channels.push(c);
            }
        }
        channels.sort_unstable();
    }
    channels
}
