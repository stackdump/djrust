//! The pokes on a net deck that Wander's moves are made of, by the names Swift gives them:
//! `setRole`, `toggleStem`, `fire(_:duration:)`, `stinger(_:bars:)`, `setPalette`,
//! `setRoleVoice`, `setLayers` (`DeckPerform.swift`, `StingerLaunch.swift`, `DeckKits.swift`,
//! `PaletteSound.swift`, `DeckLayers.swift`).
//!
//! A sound poke (`setPalette`, `setRoleVoice`) edits the deck's project and the source it came
//! from (the spec's or the raw share's `tracks` override) and then rebuilds the same song from
//! where it plays, at the next bar ([`Engine::retarget`], over [`Engine::relocate`]).

use crate::deck::perform::{next_swap_tick, StemsMuted};
use crate::deck::stinger::{deck_stingers, stinger_steps, StingerStep};
use crate::fx::MasterMacro;
use crate::layers::LayerEntry;
use crate::pflow::{Project, StingerAssignment};
use crate::session::model::DeckSource;
use crate::sound::stinger::StingerContent;
use crate::sound::PaletteSet;
use crate::wander::deck::{
    curate_project_stingers, palette_channels, project_palette, resolved_project_stingers,
    set_project_palette, set_project_role_voice,
};
use crate::wave::NetMacro;

use super::engine::Engine;
use super::track::TrackSource;

/// Why a sampled palette is greyed in a build without the `sf2` feature: the SoundFont folder
/// (MuseScore_General.sf2, the imported Minecraft bank) is never read.
pub const NO_SAMPLER: &str = "this build has no SoundFont sampler (rebuild with --features sf2)";

/// `Deck.resetLayers(from:)`: a new track is on the deck: its layer list is its own
/// `loopLayers` (a raw share's), else empty, and the key to pick layers in is the raw share's
/// `djKey`.
pub(crate) fn layers_of(source: &TrackSource) -> (Vec<LayerEntry>, Option<String>) {
    match source {
        TrackSource::Net(_) => (Vec::new(), None),
        TrackSource::RawNets(payload) => {
            crate::layers::reset_layers(&DeckSource::RawNets(payload.clone()))
        }
    }
}

impl Engine {
    // MARK: roles and STEMS

    /// `Deck.setRole(_:on:)`: a role audible or muted (net decks only).
    pub fn set_role(&mut self, deck: &str, role: &str, on: bool) -> bool {
        if !self.deck(deck).is_some_and(|d| d.is_net()) {
            return false;
        }
        self.set_role_muted(deck, role, !on)
    }

    /// `Deck.isStemMuted`: the STEMS group `stem` is muted (its button pressed, not pressed
    /// again).
    pub fn is_stem_muted(&self, deck: &str, stem: &str) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.is_net() && d.walk.stems_muted.is_muted(stem))
    }

    /// `Deck.toggleStem`: the STEMS button: mute the group, or bring back what it silenced.
    /// Song sections mute and unmute roles too, so "silent now" is not "muted by the button":
    /// the group remembers which roles were playing when it muted them and brings back only
    /// those (and none another muted group still holds).
    pub fn toggle_stem(&mut self, deck: &str, stem: &str) {
        let Some(d) = self.deck(deck) else { return };
        if !d.is_net() {
            return;
        }
        let roles = self.roles(deck);
        let mut muted: StemsMuted = d.walk.stems_muted.clone();
        let calls = muted.toggle(stem, &roles, &|r| self.is_role_audible(deck, r));
        if let Some(d) = self.deck_mut(deck) {
            d.walk.stems_muted = muted;
        }
        for (role, on) in calls {
            self.set_role(deck, &role, on);
        }
    }

    // MARK: macros and stingers

    /// `Deck.fire(_ macro: NetMacro, duration:)`: `duration` is in the macro's own unit (bars
    /// for most, ticks for the rest); None = its default.
    pub fn fire_duration(&mut self, deck: &str, m: NetMacro, duration: Option<i64>) -> bool {
        if !self.deck(deck).is_some_and(|d| d.is_net()) {
            return false;
        }
        self.fire(deck, m, Some(m.ticks(duration)))
    }

    /// `Deck.fireMacro(id:duration:)`: fire a catalog macro by its beats id, whichever kind it
    /// is. False for an id neither catalogue has. A master macro aimed at the loop channel `L`
    /// goes to the loops' master bus (`Deck.fire(_: MasterMacro)` with `bank != nil`).
    pub fn fire_macro_id(&mut self, deck: &str, id: &str, duration: Option<i64>) -> bool {
        if let Some(m) = crate::fx::AuxMacro::from_id(id) {
            return self.fire_aux_macro(deck, m, duration.map(|d| d as i32));
        }
        if let Some(m) = NetMacro::from_id(id) {
            self.fire_duration(deck, m, duration);
        } else if let Some(m) = MasterMacro::from_id(id) {
            let bars = duration.map(|d| d as i32);
            if deck == "L" && self.index(deck).is_none() {
                self.fire_loop_master(m, bars, false);
            } else {
                self.fire_master(deck, m, bars);
            }
        } else {
            return false;
        }
        true
    }

    /// `Deck.stingers`: what Hit 1-4 fire on this deck's track (four fallbacks when nothing is
    /// curated; empty off a net).
    pub fn stingers(&self, deck: &str) -> Vec<StingerAssignment> {
        match self.deck(deck) {
            Some(d) if d.is_net() => deck_stingers(d.project.as_ref()),
            _ => Vec::new(),
        }
    }

    /// `Deck.stinger(_:bars:)`: stinger `n` (0-3, Hit 1-4) as curated: a one-shot unmutes its
    /// stinger net for its bars, a macro fires, a loop launches on the loop row
    /// ([`fire_stinger_loop`](Self::fire_stinger_loop)). A Hit that cannot fire what it was
    /// given (a loop whose audio is gone) plays its stinger net.
    pub fn stinger(&mut self, deck: &str, n: i64, bars: i64) {
        let hits = self.stingers(deck);
        let Some(a) = usize::try_from(n).ok().and_then(|n| hits.get(n)) else {
            return;
        };
        for step in stinger_steps(a, n, bars) {
            match step {
                StingerStep::FireNet { net_id, ticks } => {
                    self.fire_stinger(deck, &net_id, ticks);
                    return;
                }
                StingerStep::FireMacro { id, duration } => {
                    if self.fire_macro_id(deck, &id, duration) {
                        return;
                    }
                }
                StingerStep::LaunchLoop => {
                    if self.fire_stinger_loop(a, deck) {
                        return;
                    }
                }
            }
        }
    }

    /// The Hits for `project` on this deck's track, resolved again from its genre, palette,
    /// seed, key and tempo (`Deck.curateStingers`); a track with no genre (a raw-`nets` share)
    /// keeps none.
    pub fn curate_stingers(&self, deck: &str, project: &mut Project) {
        let Some(d) = self.deck(deck) else { return };
        let (genre, seed) = (d.spec.as_ref().map(|s| s.genre.clone()), d.seed);
        self.with_sounds(|ctx| {
            let set = PaletteSet::shared();
            match genre {
                None => curate_project_stingers(project, Vec::new(), seed, set, ctx),
                Some(g) => {
                    let content = StingerContent::here(set, ctx, Vec::new());
                    let hits = resolved_project_stingers(project, set, &g, seed, &content);
                    curate_project_stingers(project, hits, seed, set, ctx)
                }
            }
        });
    }

    // MARK: the sound pokes

    /// `Deck.netSource`: the net the deck is playing, as a descriptor.
    pub fn net_source(&self, deck: &str) -> Option<DeckSource> {
        self.deck(deck)?.net_source()
    }

    /// `Deck.retarget(_:_:)`: replace the deck's project with `project` (the same song with
    /// other sound choices) and its source with `edit(source)`, then rebuild at the next bar
    /// from where it plays.
    pub fn retarget(
        &mut self,
        deck: &str,
        project: Project,
        edit: impl FnOnce(&DeckSource) -> Option<DeckSource>,
    ) {
        let next = self.net_source(deck).and_then(|s| edit(&s));
        let Some(d) = self.deck_mut(deck) else { return };
        match next {
            Some(DeckSource::Net(spec)) => d.spec = Some(spec),
            Some(DeckSource::RawNets(payload)) => d.track.raw_nets = Some(payload),
            _ => {}
        }
        d.project = Some(project);
        let tick = d.mirror.tick();
        let at = if d.mirror.is_playing() {
            next_swap_tick(tick)
        } else {
            tick
        };
        self.relocate(deck, at, None, None);
    }

    /// `Deck.setPalette` (`DeckAction.setPalette`; None = none): play the track from `palette`:
    /// the same song from the next bar. A palette not playable here leaves every role as it
    /// was voiced. The default palette (Studio) is written as none. Role overrides and kit
    /// picks stay, and the palette goes with every new track the deck makes.
    pub fn set_palette(&mut self, deck: &str, palette: Option<&str>) {
        let Some(d) = self.deck(deck) else { return };
        if !d.is_net() {
            return;
        }
        let Some(mut project) = d.project.clone() else {
            return;
        };
        let id = self.with_sounds(|ctx| PaletteSet::shared().normalized(palette, ctx.installed));
        if project_palette(&project) == id {
            return;
        }
        // a composed track's every role channel (as `TrackSpec.withPalette` writes it), so a
        // palette set here and one chosen before Generate name the same configuration
        let channels = palette_channels(&project, d.spec.as_ref().map(|s| s.genre.as_str()));
        if channels.is_empty() {
            return;
        }
        set_project_palette(&mut project, id.as_deref());
        // the Hits are the palette's too: resolved again
        self.curate_stingers(deck, &mut project);
        self.retarget(deck, project, |src| {
            src.with_palette(id.as_deref(), &channels)
        });
    }

    /// The deck's sound chooser (`SoundChoices`): every shipped palette, the one it plays lit,
    /// greyed with its reason when it can't play here. Empty for a deck with no net track.
    pub fn palette_cards(&self, deck: &str) -> Vec<crate::sound::PaletteCard> {
        let Some(d) = self.deck(deck).filter(|d| d.is_net() && d.project.is_some()) else {
            return Vec::new();
        };
        let current = d.project.as_ref().and_then(project_palette);
        #[allow(unused_mut)]
        let mut cards = self.with_sounds(|ctx| {
            crate::sound::PaletteCard::cards(PaletteSet::shared(), current.as_deref(), ctx)
        });
        // a build without the sampler never scans the SoundFont folder: every sampled palette
        // is greyed whatever is installed, and the reason says so (not "not imported yet")
        #[cfg(not(feature = "sf2"))]
        if self.installed.is_none() {
            for c in &mut cards {
                if let Some(why) = &mut c.unavailable {
                    *why = format!("{NO_SAMPLER}; {why}");
                }
            }
        }
        cards
    }

    /// Step the deck's palette `delta` along the playable ones (the chooser's arrows,
    /// `soundPrev` / `soundNext`): [`set_palette`](Self::set_palette), so the same song from
    /// the next bar and carried into new tracks. False when nothing changed.
    pub fn step_palette(&mut self, deck: &str, delta: i64) -> bool {
        let cards = self.palette_cards(deck);
        let Some(id) = crate::sound::PaletteCard::step(&cards, delta) else {
            return false;
        };
        let before = self.palette(deck);
        self.set_palette(deck, Some(&id));
        self.palette(deck) != before
    }

    /// `AppModel.copyDeck`: load the net track playing on `from` onto `to` (palette, kit and
    /// instrument choices included), from its start, as a `load` (so it records). False when
    /// `from` has no net track or `to` is no deck. [`copy_deck`](Engine::copy_deck) is the
    /// hub's: on the next bar, in sync, where the source is (docs/COPY-DECK.md).
    pub fn copy_deck_from_start(&mut self, from: &str, to: &str) -> bool {
        let Some(source) = self.net_source(from) else {
            return false;
        };
        if from == to || self.deck(to).is_none() {
            return false;
        }
        let a = crate::session::action::DeckAction::Load {
            deck: to.to_string(),
            source,
        };
        matches!(
            self.perform(&a, super::perform::PerformCtx::default()),
            super::perform::Applied::Done
        )
    }

    /// `Deck.setRoleVoice` (`DeckAction.setRoleVoice`; None = back to the palette's pick): play
    /// `role` with `voice`: the same song from the next bar, the choice in the source's
    /// `tracks` override so it records, stars and shares, and carried into every new track
    /// until cleared. Clearing also drops an instrument (or, for the drums, a kit) the user
    /// picked for the role the older way.
    pub fn set_role_voice(&mut self, deck: &str, role: &str, voice: Option<&str>) {
        let Some(d) = self.deck(deck) else { return };
        if !d.is_net() {
            return;
        }
        let Some(mut project) = d.project.clone() else {
            return;
        };
        let voice = voice.filter(|v| !v.is_empty());
        let seed = d.seed;
        let channels = self.with_sounds(|ctx| {
            set_project_role_voice(&mut project, role, voice, seed, PaletteSet::shared(), ctx)
        });
        let Some(channels) = channels else { return };
        self.retarget(deck, project, |src| {
            let x = src
                .with_track_field("voice", voice, &channels)
                .and_then(|x| x.with_track_field("instrument", None, &channels));
            if role == "kit" && voice.is_none() {
                x.and_then(|x| x.with_track_field("kit", None, &channels))
            } else {
                x
            }
        });
    }

    // MARK: layers

    /// `Deck.setLayers` (a Wander move): replace the layer list.
    pub fn set_layers(&mut self, deck: &str, entries: Vec<LayerEntry>) {
        if let Some(d) = self.deck_mut(deck) {
            d.walk.layer_entries = entries;
        }
    }

    /// `Deck.layerKey`: the key to pick layers in: the track's own (`djKey`), else a composed
    /// deck's genre scale and root, else unknown.
    pub fn layer_key(&self, deck: &str) -> Option<String> {
        let d = self.deck(deck)?;
        let wk = self.wander_key(deck);
        crate::layers::layer_key(
            d.walk.layer_key_name.as_deref(),
            wk.as_ref().map(|k| (k.scale.as_str(), k.root_pc)),
        )
    }
}
