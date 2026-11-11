//! The planner: `WanderContext`, `Wander.plan(_:step:)` and the move builders.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::surface::DJActionKind;
use crate::deck::tempo_match;
use crate::fx::master_macro::MasterMacro;
use crate::gen::tables::BeatsTables;
use crate::json::JsonValue;
use crate::sound::is_drum_channel;
use crate::spec::TrackSpec;
use crate::voices::swift_double;
use crate::wave::macros::NetMacro;

use super::carry::{
    channel_of, project_channels, project_drum_channels, set_instrument, spec_names_kit,
    spec_palette, with_kit, with_palette, with_track_field,
};
use super::layers::{default_gain, LayerEntry, LayerLoop};
use super::rng::WanderRandom;
use super::{WanderKey, WanderLock, WanderMoveKind};

/// What one tonal (or drum-kit) channel of a deck plays, for the walk: beats' own instrument
/// lists only (`BeatsTables.instrumentSets`), so a pick is the same on every machine.
#[derive(Clone, Debug, PartialEq)]
pub struct WanderVoice {
    pub roles: Vec<String>,
    pub channel: i64,
    pub instrument: String,
    pub options: Vec<String>,
    /// The user set this role's sound (`DeckAction.setRoleVoice`): no instrument move re-picks it.
    pub overridden: bool,
    /// The palette option it plays (`TrackSound.option`), None = its instrument's own sound.
    pub option: Option<String>,
}

impl WanderVoice {
    pub fn new(roles: &[&str], channel: i64, instrument: &str, options: &[&str]) -> WanderVoice {
        WanderVoice {
            roles: roles.iter().map(|s| s.to_string()).collect(),
            channel,
            instrument: instrument.into(),
            options: options.iter().map(|s| s.to_string()).collect(),
            overridden: false,
            option: None,
        }
    }
}

/// A palette a hop may reach (`SoundPalette.neighbours` of the one playing, playable here:
/// `PaletteCard.unavailableHere[id] == nil`).
#[derive(Clone, Debug, PartialEq)]
pub struct PaletteHop {
    pub id: String,
    pub label: String,
    /// `SoundPalette.normalized(id)`: what a track writes (None for the default palette).
    pub normalized: Option<String>,
}

/// The sound palettes in play for a palette move: the one the deck plays and the playable
/// neighbours. Built by the host from the palette set and what is installed; None in a
/// [`WanderContext`] means palettes are not in play (no hop is possible).
#[derive(Clone, Debug, PartialEq)]
pub struct WanderPalettes {
    /// The label of the palette playing (`SoundPalette.playing(c.palette).label`).
    pub here_label: String,
    pub options: Vec<PaletteHop>,
}

impl WanderPalettes {
    /// `SoundPalette.playing(palette)` and its playable neighbours over `sounds`.
    pub fn from_sounds(
        set: &crate::sound::PaletteSet,
        palette: Option<&str>,
        ctx: crate::sound::Sounds,
    ) -> Option<WanderPalettes> {
        let here = set.playing(palette)?;
        let options = here
            .neighbours
            .iter()
            .filter(|id| **id != here.id)
            .filter(|id| {
                set.named(Some(id))
                    .map(|p| p.unavailable_reason(set, ctx).is_none())
                    .unwrap_or(false)
            })
            .map(|id| PaletteHop {
                id: id.clone(),
                label: set
                    .named(Some(id))
                    .map(|p| p.label.clone())
                    .unwrap_or_else(|| id.clone()),
                normalized: set.normalized(Some(id), ctx.installed),
            })
            .collect();
        Some(WanderPalettes {
            here_label: here.label.clone(),
            options,
        })
    }
}

/// Everything a move is chosen from: the deck's configuration and what the user fixed
/// (`WanderContext`).
#[derive(Clone, Debug)]
pub struct WanderContext {
    pub deck: String,
    pub spec: TrackSpec,
    pub is_loop: bool,
    pub roles: Vec<String>,
    pub roles_on: BTreeSet<String>,
    pub disabled_roles: BTreeSet<String>,
    pub stems_muted: BTreeSet<String>,
    /// `DJEngine.lockedActions`: a move whose button is locked is not taken.
    pub locked_actions: BTreeSet<String>,
    pub locks: BTreeSet<String>,
    pub voices: Vec<WanderVoice>,
    pub anchor_bpm: Option<f64>,
    pub key: Option<WanderKey>,
    /// The sound palette and drum kit the deck plays: what the palette lock carries into every
    /// new track.
    pub palette: Option<String>,
    pub kit: Option<String>,
    /// The channels the track plays on (where a carried palette is written); empty = compose
    /// the new spec to find them.
    pub channels: Vec<i64>,
    /// What Hit 1–4 fire (the curated table): the stinger move names it. Empty = "hit n".
    pub stingers: Vec<String>,
    /// Auto-mix's tempo (the outgoing deck's): a genre move only reaches genres whose tempo the
    /// incoming deck's pitch fader can match at the same beat.
    pub mix_bpm: Option<f64>,
    /// A raw-nets deck (no `TrackSpec`: `spec` is a placeholder): the walk's draws start from
    /// this (FNV-1a of the raw share's CID), and only the moves that need no spec are taken.
    pub raw_base: Option<u64>,
    /// The deck's layer list and the loops a layer move may draw from, and the bar (1-based) a
    /// move is taken in: a layer comes in from the next one.
    pub layers: Vec<LayerEntry>,
    pub layer_pool: Vec<LayerLoop>,
    pub bar: i64,
    /// Bars per move (`WanderState.pace`).
    pub pace: i64,
    /// The palettes a palette move may reach (None: no palette move).
    pub palettes: Option<WanderPalettes>,
}

impl WanderContext {
    /// The defaults of the memberwise init (`WanderContext(deck:spec:…)`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        deck: &str,
        spec: TrackSpec,
        is_loop: bool,
        roles: &[&str],
        roles_on: &[&str],
        locks: &[&str],
        voices: Vec<WanderVoice>,
    ) -> WanderContext {
        let set = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        WanderContext {
            deck: deck.into(),
            spec,
            is_loop,
            roles: roles.iter().map(|s| s.to_string()).collect(),
            roles_on: set(roles_on),
            disabled_roles: BTreeSet::new(),
            stems_muted: BTreeSet::new(),
            locked_actions: BTreeSet::new(),
            locks: set(locks),
            voices,
            anchor_bpm: None,
            key: None,
            palette: None,
            kit: None,
            channels: Vec::new(),
            stingers: Vec::new(),
            mix_bpm: None,
            raw_base: None,
            layers: Vec::new(),
            layer_pool: Vec::new(),
            bar: 0,
            pace: 8,
            palettes: None,
        }
    }

    pub fn is_raw(&self) -> bool {
        self.raw_base.is_some()
    }

    pub fn kept(&self) -> BTreeSet<String> {
        self.locks
            .iter()
            .filter_map(|l| WanderLock::kept_role(l).map(String::from))
            .collect()
    }

    pub fn locked(&self, dimension: &str) -> bool {
        self.locks.contains(dimension)
    }

    /// `locked(_ action:, _ param:)`: is the button locked (`DJEngine.lockedActions`)?
    pub fn locked_action(&self, action: DJActionKind, param: Option<&str>) -> bool {
        self.locked_actions
            .contains(&DJActionKind::lock_key(Some(&self.deck), action, param))
    }

    /// Every role the deck has is kept: no move may change the track.
    pub fn keeps_everything(&self) -> bool {
        let kept = self.kept();
        !self.roles.is_empty() && self.roles.iter().all(|r| kept.contains(r))
    }
}

/// A move as it is applied to the deck (`WanderAction`).
#[derive(Clone, Debug, PartialEq)]
pub enum WanderAction {
    Role(String, bool),
    Stem(String),
    NetMacro(NetMacro),
    MasterMacro(MasterMacro),
    Stinger(i64),
    Swap(TrackSpec),
    /// A raw-nets deck's palette hop: the same song, retargeted at the next bar, with the kept
    /// roles' sounds pinned as their own option.
    Palette(String, Vec<WanderPin>),
    /// The deck's new layer list (a layer added, swapped or taken out).
    Layers(Vec<LayerEntry>),
}

/// A kept role's sound, pinned through a palette hop (`Deck.setRoleVoice`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WanderPin {
    pub role: String,
    pub option: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WanderPlan {
    pub kind: WanderMoveKind,
    pub action: WanderAction,
    pub detail: String,
}

/// `Wander` (the namespace of static functions).
pub struct Wander;

impl Wander {
    pub const TONAL_ROLES: [&'static str; 4] = ["bass", "melody", "harmony", "arp"];
    /// The moves a raw-nets deck can take: none of them needs a `TrackSpec`.
    pub const RAW_KINDS: [WanderMoveKind; 9] = [
        WanderMoveKind::Role,
        WanderMoveKind::Stem,
        WanderMoveKind::Macro,
        WanderMoveKind::Fx,
        WanderMoveKind::Stinger,
        WanderMoveKind::Palette,
        WanderMoveKind::LayerAdd,
        WanderMoveKind::LayerSwap,
        WanderMoveKind::LayerRemove,
    ];
    /// The most layers on at once (a mix that wide is mud).
    pub const MAX_LAYERS: usize = 3;
    pub const NET_MACROS: [NetMacro; 5] = [
        NetMacro::Drop,
        NetMacro::Breakdown,
        NetMacro::BeatRepeat,
        NetMacro::SoloDrums,
        NetMacro::DoubleDrop,
    ];
    pub const MASTER_MACROS: [MasterMacro; 6] = [
        MasterMacro::SweepLp,
        MasterMacro::SweepHp,
        MasterMacro::Riser,
        MasterMacro::ReverbWash,
        MasterMacro::FilterRes,
        MasterMacro::BuildCrush,
    ];
    /// The structures a structure move picks from (beats' blueprints).
    pub const STRUCTURES: [&'static str; 7] = [
        "minimal", "standard", "extended", "ab", "drop", "build", "jam",
    ];
    /// `Deck.stems` by name, sorted: the roles each STEMS group holds.
    pub const STEMS: [(&'static str, &'static [&'static str]); 2] = [
        ("mic", &["melody"]),
        ("music", &["bass", "melody", "harmony", "arp"]),
    ];

    fn is_tonal(role: &str) -> bool {
        Self::TONAL_ROLES.contains(&role)
    }

    /// The genres a genre move may reach from `genre`: the three nearest in tempo of its family
    /// (beats' `genreFamilies`, which share structure blueprints), then any other genre within
    /// 8 % of its tempo. Sorted, so the walk is the same whatever order the tables load in.
    /// Native genres reach any genre; beats genres reach only beats genres.
    pub fn neighbours(genre: &str) -> Vec<String> {
        let t = BeatsTables::shared();
        let Some(g) = t.genres.get(genre) else {
            return Vec::new();
        };
        let family = t.family_of(genre);
        let distance = |id: &str| -> f64 {
            let bpm = t.genres.get(id).map(|x| x.bpm).unwrap_or(1.0);
            libm::fabs(libm::log(bpm / g.bpm.max(1.0)))
        };
        let mut ids: Vec<&String> = t
            .genre_ids
            .iter()
            .filter(|id| id.as_str() != genre && t.genres.contains_key(id.as_str()))
            // a beats genre walks beats' catalogue (as Swift's does); a native genre
            // (genres-native.json) walks both
            .filter(|id| t.is_native(genre) || !t.is_native(id))
            .collect();
        ids.sort();
        let mut near: Vec<&String> = ids
            .iter()
            .copied()
            .filter(|id| t.family_of(id) == family)
            .collect();
        near.sort_by(|a, b| {
            distance(a)
                .partial_cmp(&distance(b))
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.cmp(b))
        });
        near.truncate(3);
        let limit = libm::log(1.08);
        let far = ids
            .iter()
            .copied()
            .filter(|id| t.family_of(id) != family && distance(id) <= limit);
        let mut out: Vec<String> = near.into_iter().cloned().chain(far.cloned()).collect();
        out.sort();
        out
    }

    /// A spec's tempo: its `bpm` override, else its genre's.
    pub fn bpm(spec: &TrackSpec) -> f64 {
        spec.extra
            .get("bpm")
            .and_then(JsonValue::as_f64)
            .or_else(|| BeatsTables::shared().genres.get(&spec.genre).map(|g| g.bpm))
            .unwrap_or(120.0)
    }

    /// The move kinds the locks leave (before the deck's own state rules any out).
    pub fn kinds(c: &WanderContext) -> Vec<WanderMoveKind> {
        WanderMoveKind::ALL
            .iter()
            .copied()
            .filter(|&k| {
                // a raw-nets deck has no seed, genre or structure to move, and no spec to swap
                if c.is_raw() && !Self::RAW_KINDS.contains(&k) {
                    return false;
                }
                if let Some(l) = k.lock() {
                    if c.locked(l) {
                        return false;
                    }
                }
                // a palette hop changes every role's sound: the instruments lock holds them too
                if k == WanderMoveKind::Palette && c.locked("instruments") {
                    return false;
                }
                if k.swaps() && c.keeps_everything() {
                    return false;
                }
                if c.is_loop && k == WanderMoveKind::Structure {
                    return false;
                }
                true
            })
            .collect()
    }

    /// A STEMS group the walk muted wants back: its weight goes up until it is.
    pub fn weight(k: WanderMoveKind, c: &WanderContext) -> i64 {
        if k == WanderMoveKind::Stem && !c.stems_muted.is_empty() {
            return 40;
        }
        // layers come and go in a coherent way: with one on, the walk soon takes it out or swaps
        // it; with none on, one is likelier to come in
        let live = c.layers.iter().any(|l| l.end() > c.bar + 1);
        match k {
            WanderMoveKind::LayerRemove if live => 18,
            WanderMoveKind::LayerSwap if live => 9,
            WanderMoveKind::LayerAdd if !live => 9,
            _ => k.weight(),
        }
    }

    /// The move for step `step`: a weighted kind, then its parameters, all drawn from the spec
    /// and the step. A kind with nothing it may do here is struck and the draw goes on; None
    /// when no kind is left (every move locked).
    pub fn plan(c: &WanderContext, step: i64) -> Option<WanderPlan> {
        let mut rng = match c.raw_base {
            Some(b) => WanderRandom::with_base(b, step),
            None => WanderRandom::new(&c.spec, step),
        };
        Self::plan_with(c, &mut rng, Self::kinds(c))
    }

    /// The draw itself, from a stream the caller seeded, over the kinds it allows (in
    /// `WanderMoveKind` order, each with its weight): Wander's own `plan` passes every kind the
    /// locks leave; auto-mix only the new-track ones, from its own stream.
    pub fn plan_with(
        c: &WanderContext,
        rng: &mut WanderRandom,
        allowed: Vec<WanderMoveKind>,
    ) -> Option<WanderPlan> {
        let mut kinds = allowed;
        while !kinds.is_empty() {
            let total: i64 = kinds.iter().map(|&k| Self::weight(k, c)).sum();
            let mut r = rng.int(total);
            let mut k = kinds[0];
            for &x in &kinds {
                r -= Self::weight(x, c);
                if r < 0 {
                    k = x;
                    break;
                }
            }
            if let Some(p) = Self::build(k, c, rng) {
                return Some(p);
            }
            kinds.retain(|&x| x != k);
        }
        None
    }

    /// Can the walk move at all here? (False: every move is locked or impossible.)
    pub fn can_move(c: &WanderContext) -> bool {
        // trying each kind with a throwaway draw tells whether any could be built (a genre move
        // by its options alone: building one composes the new genre when instruments are pinned)
        let base = c.raw_base.unwrap_or_else(|| WanderRandom::base(&c.spec));
        Self::kinds(c).into_iter().any(|k| {
            if k == WanderMoveKind::Genre {
                return !Self::genre_options(c).is_empty();
            }
            let mut rng = WanderRandom::with_base(base, 0);
            Self::build(k, c, &mut rng).is_some()
        })
    }

    fn build(k: WanderMoveKind, c: &WanderContext, rng: &mut WanderRandom) -> Option<WanderPlan> {
        use WanderMoveKind as K;
        match k {
            K::Role => {
                let kept = c.kept();
                let eligible: Vec<&String> = c
                    .roles
                    .iter()
                    .filter(|r| {
                        !c.disabled_roles.contains(*r)
                            && !kept.contains(*r)
                            && !c.locked_action(DJActionKind::RoleMute, Some(r))
                    })
                    .collect();
                let off: Vec<&String> = eligible
                    .iter()
                    .copied()
                    .filter(|r| !c.roles_on.contains(*r))
                    .collect();
                let on: Vec<&String> = eligible
                    .iter()
                    .copied()
                    .filter(|r| c.roles_on.contains(*r))
                    .collect();
                let audible = c.roles.iter().filter(|r| c.roles_on.contains(*r)).count();
                // keep at least two parts playing: when down to two, only bring one back
                let pool: Vec<&String> = if audible <= 2 {
                    off
                } else {
                    let mut p = off.clone();
                    p.extend(off);
                    p.extend(on);
                    p
                };
                if pool.is_empty() {
                    return None;
                }
                let r = (*rng.pick(&pool)).clone();
                let turn_on = !c.roles_on.contains(&r);
                Some(WanderPlan {
                    kind: K::Role,
                    detail: format!("{r} {}", if turn_on { "on" } else { "off" }),
                    action: WanderAction::Role(r, turn_on),
                })
            }
            K::Stem => {
                let kept = c.kept();
                let stems: Vec<&str> = Self::STEMS
                    .iter()
                    .filter(|(s, group)| {
                        let present: Vec<&String> = c
                            .roles
                            .iter()
                            .filter(|r| group.contains(&r.as_str()))
                            .collect();
                        !present.is_empty()
                            && present.iter().all(|r| !kept.contains(*r))
                            && !c.locked_action(DJActionKind::StemMute, Some(s))
                    })
                    .map(|(s, _)| *s)
                    .collect();
                if stems.is_empty() {
                    return None;
                }
                if let Some(back) = stems.iter().find(|s| c.stems_muted.contains(**s)) {
                    return Some(WanderPlan {
                        kind: K::Stem,
                        action: WanderAction::Stem(back.to_string()),
                        detail: format!("STEMS {back} back"),
                    });
                }
                let s = *rng.pick(&stems);
                Some(WanderPlan {
                    kind: K::Stem,
                    action: WanderAction::Stem(s.to_string()),
                    detail: format!("STEMS {s} off"),
                })
            }
            K::Macro => {
                let ms: Vec<NetMacro> = Self::NET_MACROS
                    .iter()
                    .copied()
                    .filter(|m| !c.locked_action(DJActionKind::Macro, Some(m.id())))
                    .collect();
                if ms.is_empty() {
                    return None;
                }
                let m = *rng.pick(&ms);
                Some(WanderPlan {
                    kind: K::Macro,
                    action: WanderAction::NetMacro(m),
                    detail: m.label().to_lowercase(),
                })
            }
            K::Fx => {
                let ms: Vec<MasterMacro> = Self::MASTER_MACROS
                    .iter()
                    .copied()
                    .filter(|m| !c.locked_action(DJActionKind::Macro, Some(m.id())))
                    .collect();
                if ms.is_empty() {
                    return None;
                }
                let m = *rng.pick(&ms);
                Some(WanderPlan {
                    kind: K::Fx,
                    action: WanderAction::MasterMacro(m),
                    detail: m.label().to_lowercase(),
                })
            }
            K::Stinger => {
                let ns: Vec<i64> = (0..4)
                    .filter(|n| !c.locked_action(DJActionKind::Stinger, Some(&n.to_string())))
                    .collect();
                if ns.is_empty() {
                    return None;
                }
                let n = *rng.pick(&ns);
                let what = c
                    .stingers
                    .get(n as usize)
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default();
                Some(WanderPlan {
                    kind: K::Stinger,
                    action: WanderAction::Stinger(n),
                    detail: format!("hit {}{what}", n + 1),
                })
            }
            K::Seed => {
                let mut s = Self::nearby_seed(c, rng);
                Self::carry(c, &mut s, true, false);
                Some(WanderPlan {
                    kind: K::Seed,
                    detail: format!(
                        "seed {} → {}",
                        Self::seed_text(c.spec.seed),
                        Self::seed_text(s.seed)
                    ),
                    action: WanderAction::Swap(s),
                })
            }
            K::Instrument => {
                let kept = c.kept();
                let voices: Vec<&WanderVoice> = c
                    .voices
                    .iter()
                    .filter(|v| {
                        v.roles.iter().any(|r| Self::is_tonal(r))
                            && !v.roles.iter().any(|r| kept.contains(r))
                            && !v.overridden
                            && v.options.iter().any(|o| *o != v.instrument)
                    })
                    .collect();
                if voices.is_empty() {
                    return None;
                }
                let v = *rng.pick(&voices);
                let others: Vec<&String> =
                    v.options.iter().filter(|o| **o != v.instrument).collect();
                let pick = (*rng.pick(&others)).clone();
                let mut s = Self::nearby_seed(c, rng);
                Self::carry(c, &mut s, true, false);
                set_instrument(&mut s, &pick, v.channel);
                let role = v
                    .roles
                    .iter()
                    .find(|r| Self::is_tonal(r))
                    .unwrap_or(&v.roles[0]);
                Some(WanderPlan {
                    kind: K::Instrument,
                    detail: format!(
                        "seed {} · {role}: {} → {pick}",
                        Self::seed_text(s.seed),
                        v.instrument
                    ),
                    action: WanderAction::Swap(s),
                })
            }
            K::Structure => {
                let current = c.spec.structure.clone().unwrap_or_default();
                let options: Vec<&str> = Self::STRUCTURES
                    .iter()
                    .copied()
                    .filter(|s| *s != current)
                    .collect();
                if options.is_empty() {
                    return None;
                }
                let mut s = c.spec.clone();
                s.structure = Some((*rng.pick(&options)).to_string());
                Self::carry(c, &mut s, true, true);
                Some(WanderPlan {
                    kind: K::Structure,
                    detail: format!(
                        "{} → {}",
                        c.spec.structure.as_deref().unwrap_or("loop"),
                        s.structure.as_deref().unwrap_or("loop")
                    ),
                    action: WanderAction::Swap(s),
                })
            }
            K::Genre => {
                let options = Self::genre_options(c);
                if options.is_empty() {
                    return None;
                }
                let g = rng.pick(&options).clone();
                let mut extra = c.spec.extra.clone();
                extra.remove("bpm"); // the tempo is the genre's
                let seed = (rng.int(999_999) + 1) as f64;
                let mut s = TrackSpec::new(&g, seed, c.spec.structure.as_deref());
                s.cohesion = c.spec.cohesion.clone();
                s.extra = extra;
                Self::carry(c, &mut s, false, false);
                Some(WanderPlan {
                    kind: K::Genre,
                    detail: format!("{} → {g} · seed {}", c.spec.genre, Self::seed_text(s.seed)),
                    action: WanderAction::Swap(s),
                })
            }
            K::LayerAdd => {
                let next = c.bar + 1;
                let live: Vec<&LayerEntry> = c.layers.iter().filter(|l| l.end() > next).collect();
                if live.len() >= Self::MAX_LAYERS {
                    return None;
                }
                let taken: BTreeSet<&str> = live.iter().map(|l| l.loop_.path.as_str()).collect();
                let options: Vec<&LayerLoop> = c
                    .layer_pool
                    .iter()
                    .filter(|l| !taken.contains(l.path.as_str()))
                    .collect();
                if options.is_empty() {
                    return None;
                }
                let lp = (*rng.pick(&options)).clone();
                let e = LayerEntry::new(
                    lp.clone(),
                    next,
                    16.min(c.pace + lp.bars()),
                    default_gain(&lp.category),
                    true,
                );
                let mut list = c.layers.clone();
                list.push(e.clone());
                list.sort_by_key(|l| l.start);
                Some(WanderPlan {
                    kind: K::LayerAdd,
                    detail: format!("+ {} · bars {}–{}", lp.name, e.start, e.end() - 1),
                    action: WanderAction::Layers(list),
                })
            }
            K::LayerRemove => {
                let next = c.bar + 1;
                let options: Vec<usize> = (0..c.layers.len())
                    .filter(|&i| c.layers[i].end() > next)
                    .collect();
                if options.is_empty() {
                    return None;
                }
                let i = *rng.pick(&options);
                let mut list = c.layers.clone();
                let name = list[i].loop_.name.clone();
                if list[i].start >= next {
                    // not on yet: just never comes in
                    list.remove(i);
                } else {
                    // on: out at the next bar
                    list[i].length = (next - list[i].start).max(1);
                }
                Some(WanderPlan {
                    kind: K::LayerRemove,
                    action: WanderAction::Layers(list),
                    detail: format!("− {name}"),
                })
            }
            K::LayerSwap => {
                let next = c.bar + 1;
                let options: Vec<usize> = (0..c.layers.len())
                    .filter(|&i| c.layers[i].end() > next)
                    .collect();
                if options.is_empty() {
                    return None;
                }
                let i = *rng.pick(&options);
                let old = c.layers[i].clone();
                let busy: BTreeSet<&str> = c
                    .layers
                    .iter()
                    .filter(|l| l.end() > next)
                    .map(|l| l.loop_.path.as_str())
                    .collect();
                let same: Vec<&LayerLoop> = c
                    .layer_pool
                    .iter()
                    .filter(|l| l.category == old.loop_.category && !busy.contains(l.path.as_str()))
                    .collect();
                let any: Vec<&LayerLoop> = c
                    .layer_pool
                    .iter()
                    .filter(|l| !busy.contains(l.path.as_str()))
                    .collect();
                let pool = if same.is_empty() { any } else { same };
                if pool.is_empty() {
                    return None;
                }
                let lp = (*rng.pick(&pool)).clone();
                let mut list = c.layers.clone();
                if old.start >= next {
                    // not on yet: the other loop comes in instead
                    list[i].loop_ = lp.clone();
                    list[i].length = 8.min(list[i].length.max(1));
                } else {
                    // on: this one goes out at the next bar, the other comes in from it
                    list[i].length = (next - old.start).max(1);
                    list.push(LayerEntry::new(
                        lp.clone(),
                        next,
                        8.min((old.end() - next).max(1)),
                        old.gain_db,
                        old.key_lock,
                    ));
                }
                list.sort_by_key(|l| l.start);
                Some(WanderPlan {
                    kind: K::LayerSwap,
                    action: WanderAction::Layers(list),
                    detail: format!("{} → {}", old.loop_.name, lp.name),
                })
            }
            K::Palette => {
                let env = c.palettes.as_ref()?;
                if env.options.is_empty() {
                    return None;
                }
                let to = rng.pick(&env.options).clone();
                let detail = format!("palette {} → {}", env.here_label, to.label);
                let kept = c.kept();
                if c.is_raw() {
                    // the same song in the neighbouring palette, at the next bar
                    let pins = c
                        .voices
                        .iter()
                        .filter(|v| v.roles.iter().any(|r| kept.contains(r)) && !v.overridden)
                        .filter_map(|v| {
                            v.roles.first().map(|r| WanderPin {
                                role: r.clone(),
                                option: v.option.clone().unwrap_or_else(|| "own".into()),
                            })
                        })
                        .collect();
                    return Some(WanderPlan {
                        kind: K::Palette,
                        action: WanderAction::Palette(
                            to.normalized.clone().unwrap_or_else(|| to.id.clone()),
                            pins,
                        ),
                        detail,
                    });
                }
                let mut s = c.spec.clone();
                let channels = if c.channels.is_empty() {
                    project_channels(&s.project())
                } else {
                    c.channels.clone()
                };
                with_palette(&mut s, to.normalized.as_deref(), &channels);
                // kept roles keep the sound they play: pinned as their own option
                for v in &c.voices {
                    if v.roles.iter().any(|r| kept.contains(r)) && !v.overridden {
                        let opt = v.option.clone().unwrap_or_else(|| "own".into());
                        with_track_field(&mut s, "voice", Some(&opt), &[v.channel]);
                    }
                }
                Some(WanderPlan {
                    kind: K::Palette,
                    action: WanderAction::Swap(s),
                    detail,
                })
            }
        }
    }

    /// The genres a genre move may take from here, with the locks applied: structure keeps the
    /// family (its blueprints), tempo keeps within `TEMPO_TOLERANCE` of the anchor, key keeps
    /// the scale and the root (the genre's root pitch class, so the key holds with or without a
    /// pass that moves keys per seed).
    pub fn genre_options(c: &WanderContext) -> Vec<String> {
        let t = BeatsTables::shared();
        let Some(here) = t.genres.get(&c.spec.genre) else {
            return Vec::new();
        };
        let anchor = c.anchor_bpm.unwrap_or_else(|| Self::bpm(&c.spec));
        Self::neighbours(&c.spec.genre)
            .into_iter()
            .filter(|id| {
                let Some(g) = t.genres.get(id) else {
                    return false;
                };
                if c.locked("structure") && t.family_of(id) != t.family_of(&c.spec.genre) {
                    return false;
                }
                if c.locked("tempo")
                    && libm::fabs(g.bpm / anchor - 1.0) > WanderLock::TEMPO_TOLERANCE
                {
                    return false;
                }
                if c.locked("key") {
                    let scale = c
                        .key
                        .as_ref()
                        .map(|k| k.scale.as_str())
                        .unwrap_or(here.scale_name.as_str());
                    let root = c
                        .key
                        .as_ref()
                        .map(|k| (k.root_pc - k.offset + 12) % 12)
                        .unwrap_or(here.root_note % 12);
                    if g.scale_name != scale || g.root_note % 12 != root {
                        return false;
                    }
                }
                if let Some(mix) = c.mix_bpm {
                    let m = tempo_match(g.bpm, mix);
                    if m.clamped || m.mult != 1.0 {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    /// The same song settings a few seeds on or back (±1…7), none of this seed's own overrides.
    fn nearby_seed(c: &WanderContext, rng: &mut WanderRandom) -> TrackSpec {
        let delta = (rng.int(7) + 1) as f64 * if rng.int(2) == 0 { 1.0 } else { -1.0 };
        let mut seed = (c.spec.seed + delta).max(1.0);
        if seed == c.spec.seed {
            seed += 1.0;
        }
        let mut s = TrackSpec::new(&c.spec.genre, seed, c.spec.structure.as_deref());
        s.cohesion = c.spec.cohesion.clone();
        s.extra = c.spec.extra.clone();
        s
    }

    /// What the locks carry into a new spec: the pinned key, the kept parts' seeds (same genre
    /// only), and the instruments the user keeps (all of them under the instruments lock, else
    /// the kept roles'), so they arrive with the swap. A new genre is composed once to find which
    /// channel each kept role plays on there (`Wander.carry`).
    pub fn carry(c: &WanderContext, s: &mut TrackSpec, same_genre: bool, keep_layers: bool) {
        use crate::spec::TrackSpec as T;
        if !keep_layers {
            s.layers = super::carry::empty_layers();
        }
        // a kept part plays its line on through new seeds: its own seed, else the one it played
        // under (the track's); only kept parts' seeds go on, so a freed keep drops its seed here
        s.part_seeds.clear();
        let kept = c.kept();
        if same_genre {
            for r in &kept {
                if SEEDABLE_PARTS.contains(&r.as_str()) {
                    let seed = c.spec.part_seeds.get(r).copied().unwrap_or(c.spec.seed);
                    s.part_seeds.insert(r.clone(), seed);
                }
            }
        }
        if let Some(key) = &c.key {
            if c.locked("key") {
                s.extra
                    .set("keyOffset", JsonValue::Number(key.offset as f64));
            }
        }
        // the deck's sound — palette, kit pick, role overrides — goes with every new track (the
        // palette lock only stops palette hops)
        *s = s.carrying_sound(&c.spec);
        if (c.palette.is_some() || c.kit.is_some())
            && (spec_palette(s) != c.palette || !spec_names_kit(s, c.kit.as_deref()))
        {
            let p = if c.channels.is_empty() {
                let mut bare = T::new(&s.genre, s.seed, s.structure.as_deref());
                bare.cohesion = s.cohesion.clone();
                bare.extra = s.extra.clone();
                Some(bare.project())
            } else {
                None
            };
            if let Some(pal) = &c.palette {
                if spec_palette(s).as_deref() != Some(pal.as_str()) {
                    let ch = p
                        .as_ref()
                        .map(project_channels)
                        .unwrap_or_else(|| c.channels.clone());
                    with_palette(s, Some(pal), &ch);
                }
            }
            if let Some(kit) = &c.kit {
                if !spec_names_kit(s, Some(kit)) {
                    let ch = p.as_ref().map(project_drum_channels).unwrap_or_else(|| {
                        c.channels
                            .iter()
                            .copied()
                            .filter(|&x| is_drum_channel(x))
                            .collect()
                    });
                    with_kit(s, Some(kit), &ch);
                }
            }
        }
        // (a role the palette voices is pinned by its option, so the next seed's pick can't move
        // it; one with its own override already rides along)
        let pins: Vec<&WanderVoice> = c
            .voices
            .iter()
            .filter(|v| {
                (c.locked("instruments") || v.roles.iter().any(|r| kept.contains(r)))
                    && !v.overridden
            })
            .collect();
        if pins.is_empty() {
            return;
        }
        for v in &pins {
            if let Some(opt) = &v.option {
                with_track_field(s, "voice", Some(opt), &[v.channel]);
            }
        }
        let target = if same_genre {
            None
        } else {
            let mut bare = T::new(&s.genre, s.seed, s.structure.as_deref());
            bare.cohesion = s.cohesion.clone();
            bare.extra = s.extra.clone();
            Some(bare.project())
        };
        for v in pins.iter().filter(|v| v.option.is_none()) {
            let ch = match &target {
                Some(t) => v.roles.iter().find_map(|r| channel_of(r, t)),
                None => Some(v.channel),
            };
            if let Some(ch) = ch {
                set_instrument(s, &v.instrument, ch);
            }
        }
    }

    pub fn seed_text(s: f64) -> String {
        if s == libm::round(s) {
            format!("{}", s as i64)
        } else {
            swift_double(s)
        }
    }
}

/// `TrackSpec.seedableParts`.
pub const SEEDABLE_PARTS: [&str; 6] = ["bass", "kick", "snare", "hihat", "melody", "arp"];
