//! Wander on a live deck (`Wander.swift`, the `Deck` and `DJEngine` extensions): the walk's
//! state, the context a deck tells the planner, the look-ahead, and the moves it takes.
//!
//! The planner, the bar arithmetic and the JSON are `crate::wander` and
//! `crate::wander::deck`; this file gathers a deck's state for them, applies what
//! [`Wander::plan`] decides and keeps the look-ahead slot. A move reaches the audio thread the
//! way a hand on a control does (a role mute, a macro, a stinger: a command; a swap or a
//! palette hop: a prepared project through the deck's `Load`), so nothing here runs on it.
//!
//! The housekeeping is [`Engine::service_wander`], called from the end of [`Engine::service`]
//! (Swift's hub calls `serviceWander` right after the decks' own `service` and the loop takes,
//! then `serviceAutoMix`; nothing the walk reads or writes is the loop row's, so it does not
//! matter that the loop takes come after it here).
//!
//! Deliberate differences from Swift: none in what a move does. A look-ahead is composed on a
//! worker thread ([`Engine::request_look_ahead`]) and polled in `service`, as every compose
//! here is; with [`Engine::deterministic`] no look-ahead is made and a swap composes inline at
//! the move, exactly as Swift's replay does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::cid::share_cid;
use crate::deck::perform::StemsMuted;
use crate::deck::slots::{LookAheadKey, SwapStart};
use crate::deck::stinger::deck_stingers;
use crate::layers::{LayerEntry, LayerLoop};
use crate::sound::{Installed, PaletteSet, Sounds};
use crate::voices::{CatalogSound, VoiceCatalog};
use crate::wander::deck::{
    project_palette, stinger_labels, wander_context, wander_key, wander_swap_start, wander_voices,
    WanderDeckInputs,
};
use crate::wander::{
    Advance, Wander, WanderAction, WanderContext, WanderKey, WanderLock, WanderMove,
    WanderPalettes, WanderState, WanderVoice,
};
use crate::wave::RoleFilter;
use crate::JsonValue;

use super::engine::Engine;
use super::track::TakenAhead;

/// What a deck keeps for its walk and the things a walk moves (the `Deck` fields of
/// `Wander.swift`, `DeckPerform.swift` and `DeckLayers.swift`).
#[derive(Default)]
pub struct DeckWalk {
    /// The deck's walk: on/off, pace, step, locks, the log of moves (`Deck.wander`).
    pub wander: WanderState,
    /// Auto-mix holds the walk (a hand-off this deck is in, from its prep point to its end):
    /// derived, never recorded; not `wander.paused`, which is the user's (`Deck.wanderHeld`).
    pub held: bool,
    /// STEMS groups this deck has muted, each with the roles that were playing when it was.
    /// Cleared when a track loads (`Deck.stemsMuted`).
    pub stems_muted: StemsMuted,
    /// The library sounds this deck is bringing in over its track: a loop, the bar it comes in
    /// on, how long it stays. A raw track's `loopLayers` fill it at load; Wander adds, swaps and
    /// removes entries as it walks (`Deck.layerEntries`).
    pub layer_entries: Vec<LayerEntry>,
    /// Moves on every new track (`Deck.layerEpoch`).
    pub layer_epoch: i64,
    /// The loops Wander may draw layer moves from for this track (set by the host); empty = no
    /// layer moves (`Deck.layerPool`).
    pub layer_pool: Vec<LayerLoop>,
    /// The key the track is in ("A minor") when its author said so (`Deck.layerKeyName`).
    pub layer_key_name: Option<String>,
}

/// What is installed on this host, as the pure sound code asks about it
/// ([`Sounds`]): the catalog and the folders' content. A host with a SoundFont folder scans it
/// ([`crate::host::sampler::Library`]); a test says what a machine has.
pub struct InstalledSounds {
    catalog: VoiceCatalog,
    installed: Installed,
}

impl InstalledSounds {
    /// A host with this installed (the catalog knows its SoundFonts).
    pub fn new(installed: Installed) -> Arc<InstalledSounds> {
        let catalog = VoiceCatalog::with_installed(Default::default(), installed.sound_fonts());
        Arc::new(InstalledSounds { catalog, installed })
    }

    /// [`new`](Self::new) on a host that also names sampled instruments of its own (the
    /// catalog's `sounds`, by category: what a Mac's Logic / GarageBand folders hold).
    pub fn with_catalog(
        installed: Installed,
        sounds: BTreeMap<String, Vec<CatalogSound>>,
    ) -> Arc<InstalledSounds> {
        let catalog = VoiceCatalog::with_installed(sounds, installed.sound_fonts());
        Arc::new(InstalledSounds { catalog, installed })
    }

    pub fn sounds(&self) -> Sounds<'_> {
        Sounds {
            catalog: &self.catalog,
            installed: &self.installed,
        }
    }
}

impl Engine {
    /// Run `f` with what is installed here: [`Engine::installed`] if the host said, else the
    /// scanned library (feature `sf2`), else nothing (`Sounds::nothing`).
    pub fn with_sounds<R>(&self, f: impl FnOnce(Sounds<'_>) -> R) -> R {
        if let Some(i) = &self.installed {
            return f(i.sounds());
        }
        #[cfg(feature = "sf2")]
        if let Some(lib) = &self.library {
            return f(lib.sounds());
        }
        f(Sounds::nothing())
    }

    /// The palettes a hop from `palette` can reach here (`SoundPalette.playing(palette)` and its
    /// playable neighbours), built from what is installed. Without it a palette move never
    /// happens.
    pub fn wander_palettes(&self, palette: Option<&str>) -> Option<WanderPalettes> {
        self.with_sounds(|ctx| WanderPalettes::from_sounds(PaletteSet::shared(), palette, ctx))
    }

    /// `Deck.roleFilter`: the roles the deck leaves out of every net it compiles. One value, the
    /// deck's options' (`set_role_disabled` and `generate_filtered` both write it), so the key a
    /// look-ahead is composed under is the filter every other compose on the deck takes.
    pub fn role_filter(&self, deck: &str) -> Option<RoleFilter> {
        self.deck(deck)?.opts.role_filter.clone()
    }

    // MARK: what the deck tells the planner

    /// `Deck.wanderVoices`: the tonal (and drum-kit) channels of the net with beats' instrument
    /// lists, in role order. Empty unless the deck holds a net.
    pub fn wander_voices(&self, deck: &str) -> Vec<WanderVoice> {
        let Some(d) = self.deck(deck) else {
            return Vec::new();
        };
        match (&d.project, d.is_net()) {
            (Some(p), true) => wander_voices(
                p,
                &self.roles(deck),
                d.spec.as_ref().map_or("", |s| s.genre.as_str()),
            ),
            _ => Vec::new(),
        }
    }

    /// `Deck.wanderKey`: the key the track plays in now.
    pub fn wander_key(&self, deck: &str) -> Option<WanderKey> {
        let d = self.deck(deck)?;
        wander_key(&d.spec.as_ref()?.genre, d.project.as_ref())
    }

    /// `Deck.wanderContext(lockedActions:)`: None unless the deck plays a net it knows: a spec,
    /// or a raw-`nets` share (which walks on the moves that need no spec).
    pub fn wander_context(&self, deck: &str) -> Option<WanderContext> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        let project = d.project.as_ref()?;
        let roles = self.roles(deck);
        let roles_on = roles
            .iter()
            .filter(|r| self.is_role_audible(deck, r))
            .cloned()
            .collect();
        let palette = project_palette(project);
        wander_context(WanderDeckInputs {
            deck: &d.name,
            spec: d.spec.as_ref(),
            raw_cid: d
                .spec
                .is_none()
                .then(|| d.track.raw_nets.as_ref().map(share_cid))
                .flatten(),
            is_loop: d.is_net_loop(),
            roles,
            roles_on,
            disabled_roles: d.disabled_roles.iter().cloned().collect(),
            stems_muted: d.walk.stems_muted.groups.keys().cloned().collect(),
            locked_actions: self.locked.iter().cloned().collect::<BTreeSet<_>>(),
            wander: &d.walk.wander,
            project,
            layers: d.walk.layer_entries.clone(),
            layer_pool: d.walk.layer_pool.clone(),
            stingers: stinger_labels(&deck_stingers(Some(project))),
            palettes: self.wander_palettes(palette.as_deref()),
        })
    }

    // MARK: the walk's controls

    /// `Deck.setWander` (`DeckAction.wander`): the walk on or off. On, it waits for the next
    /// phrase end; the bar it is in now does not count as a fresh one.
    pub fn set_wander(&mut self, deck: &str, on: bool) {
        let Some(d) = self.deck_mut(deck) else { return };
        let bar = d.is_net().then(|| d.mirror.tick() / 16);
        d.walk.wander.set_on(on, bar);
        if !on {
            self.drop_look_ahead(deck);
        }
    }

    /// `Deck.setWanderPace`: bars per move (4, 8, 16 or 32; anything else is ignored).
    pub fn set_wander_pace(&mut self, deck: &str, bars: i64) {
        if let Some(d) = self.deck_mut(deck) {
            d.walk.wander.set_pace(bars);
        }
    }

    /// `Deck.stepWanderPace`: SHIFT + the browser wheel: one pace longer (clockwise) or shorter
    /// per click, stopping at 4 and 32.
    pub fn step_wander_pace(&mut self, deck: &str, delta: i64) {
        if let Some(d) = self.deck_mut(deck) {
            d.walk.wander.step_pace(delta);
        }
    }

    /// `Deck.setWanderLock` (`DeckAction.wanderLock`): lock a dimension of the walk, or free
    /// it. The tempo and key locks hold the tempo and key the deck plays now.
    pub fn set_wander_lock(&mut self, deck: &str, dimension: &str, on: bool) {
        if !WanderLock::is_valid(dimension) {
            return;
        }
        let key = self.wander_key(deck);
        let Some(d) = self.deck_mut(deck) else { return };
        let bpm = d.spec.as_ref().map(Wander::bpm);
        d.walk.wander.set_lock_anchored(dimension, on, bpm, key);
        // the plan it was made from may have changed
        self.drop_look_ahead(deck);
    }

    /// `Deck.grabWander`: the browser wheel turned on this deck: a wandering deck pauses until
    /// a phrase passes with no turn.
    pub fn grab_wander(&mut self, deck: &str) {
        let Some(d) = self.deck_mut(deck) else { return };
        if !d.walk.wander.on {
            return;
        }
        d.walk.wander.grab();
        self.drop_look_ahead(deck);
    }

    /// `Deck.wanderHeld`: auto-mix holds the walk through a hand-off (its prep point to its end):
    /// no move, no look-ahead, and the held bars are not quiet bars. Derived, never recorded;
    /// not `wander.paused`, which is the user's.
    pub fn set_wander_held(&mut self, deck: &str, held: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.walk.held = held;
        }
    }

    /// `Deck.wanderCountdown`: bars until the walk's next move (or, paused, until it resumes);
    /// None when it is off or the deck is not a net.
    pub fn wander_countdown(&self, deck: &str) -> Option<i64> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        d.walk.wander.countdown(d.mirror.tick())
    }

    /// `Deck.wanderIdleReason`: why the walk is standing still, if it is.
    pub fn wander_idle_reason(&self, deck: &str) -> Option<&'static str> {
        let d = self.deck(deck)?;
        if !d.walk.wander.on {
            return None;
        }
        let net = d.is_net();
        let ctx = if net { self.wander_context(deck) } else { None };
        d.walk.wander.idle_reason(net, ctx.as_ref())
    }

    /// `Deck.wanderJSON`: the JSON `dj_state` shows for the walk.
    pub fn wander_json(&self, deck: &str) -> Option<JsonValue> {
        let d = self.deck(deck)?;
        Some(
            d.walk
                .wander
                .deck_json(self.wander_countdown(deck), self.wander_idle_reason(deck)),
        )
    }

    // MARK: housekeeping

    /// `DJEngine.serviceWander`: every deck's walk, in name order. About every 10 ms
    /// ([`Engine::service`] calls it).
    pub fn service_wander(&mut self) {
        let mut names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        names.sort();
        for name in names {
            self.service_wander_deck(&name);
        }
    }

    /// `Deck.serviceWander`: on each new bar, count a quiet bar while paused, and on entering
    /// the last bar of a phrase take one move. Sample decks do nothing.
    pub fn service_wander_deck(&mut self, deck: &str) {
        let Some(d) = self.deck(deck) else { return };
        if !(d.walk.wander.on && d.is_net() && d.playing) {
            return;
        }
        let bar = d.mirror.tick() / 16;
        self.advance_wander(deck, bar);
    }

    /// `Deck.advanceWander(toBar:)`: the deck's clock is in bar `bar` (0-based): what
    /// `service_wander` does with a reading.
    pub fn advance_wander(&mut self, deck: &str, bar: i64) {
        let Some(d) = self.deck_mut(deck) else { return };
        if !(d.walk.wander.on && d.is_net() && (d.spec.is_some() || d.track.raw_nets.is_some())) {
            return;
        }
        let held = d.walk.held;
        match d.walk.wander.advance(bar, held) {
            Advance::Nothing => {}
            // auto-mix holds the walk through a hand-off (AutoMix.swift): no move, no look-ahead
            Advance::Held => self.drop_look_ahead(deck),
            Advance::LookAhead(b) => self.look_ahead_wander(deck, b),
            Advance::Move(b) => {
                // a tempo or key lock restored at launch (no track then) holds what plays at
                // its first move
                let key = self.wander_key(deck);
                if let Some(d) = self.deck_mut(deck) {
                    let bpm = d.spec.as_ref().map(Wander::bpm);
                    d.walk.wander.fill_anchors(bpm, key);
                }
                self.take_wander_move(deck, b);
            }
        }
    }

    // MARK: the look-ahead

    /// `Deck.lookAheadWander(bar:)`: a bar before the move at `bar` (a phrase's last bar): plan
    /// it read-only and, if it is a swap, start composing that into the look-ahead slot so it
    /// is ready when the move is taken. Moves nothing: never advances `wander.step`, never
    /// reads the deck's clock (the start comes from `bar`). `deterministic` replay composes
    /// inline at the move and skips this.
    pub fn look_ahead_wander(&mut self, deck: &str, bar: i64) {
        let key = if self.deterministic {
            None
        } else {
            self.wander_swap_key(deck, bar)
        };
        let Some(key) = key else {
            return self.drop_look_ahead(deck);
        };
        if self.look_ahead_key(deck).as_ref() == Some(&key) {
            return;
        }
        self.request_look_ahead(deck, key);
    }

    /// `Deck.wanderSwapKey(bar:)`: the look-ahead key of the move at `bar`, if it is a swap:
    /// what `take_wander_move` will plan (with the anchors `advance_wander` fills in at the
    /// move, not written here).
    pub fn wander_swap_key(&self, deck: &str, bar: i64) -> Option<LookAheadKey> {
        let mut c = self.wander_context(deck)?;
        c.bar = bar + 1; // as the move itself will see it, so the same kind is drawn
        let d = self.deck(deck)?;
        if d.walk.wander.is_locked("tempo") && c.anchor_bpm.is_none() {
            c.anchor_bpm = d.spec.as_ref().map(Wander::bpm);
        }
        if d.walk.wander.is_locked("key") && c.key.is_none() {
            c.key = self.wander_key(deck);
        }
        let plan = Wander::plan(&c, d.walk.wander.step)?;
        let WanderAction::Swap(spec) = plan.action else {
            return None;
        };
        Some(LookAheadKey {
            step: d.walk.wander.step,
            spec,
            start: self.wander_swap_start(deck, bar),
            role_filter: self.role_filter(deck),
        })
    }

    /// The look-ahead's track, once composed (`await lookAhead.task?.value`): what it was
    /// prepared as, for a test to hold against what loads.
    pub fn look_ahead_track(&mut self, deck: &str) -> Option<&super::track::PreparedTrack> {
        self.look_ahead_ready(deck);
        self.deck(deck)?.track.look_ahead.ready_track()
    }

    /// `Deck.wanderSwapStart(bar:)`: where the swap of the move at `bar` starts: the next
    /// phrase's first bar, the transport still running (a loop starts at its top).
    pub fn wander_swap_start(&self, deck: &str, bar: i64) -> SwapStart {
        wander_swap_start(self.is_net_loop(deck), bar)
    }

    // MARK: a move

    /// `Deck.takeWanderMove(bar:lockedActions:)`: take the walk's next move now, as if a phrase
    /// had just reached its last bar (`bar`, 0-based). None when the deck has nothing to walk
    /// from or the planner has no move.
    pub fn take_wander_move(&mut self, deck: &str, bar: i64) -> Option<WanderMove> {
        let mut c = self.wander_context(deck)?;
        c.bar = bar + 1; // the bar the move is taken in, 1-based: layers come in from the next
        let step = self.deck(deck)?.walk.wander.step;
        let plan = Wander::plan(&c, step)?;
        let mut mv = WanderMove {
            step,
            bar: bar + 1,
            kind: plan.kind,
            detail: plan.detail.clone(),
            spec: (!c.is_raw()).then(|| c.spec.clone()),
            cid: None,
        };
        // the look-ahead, if it composed this very swap (else it is dropped)
        let start = self.wander_swap_start(deck, bar);
        let ahead: Option<TakenAhead> = if let WanderAction::Swap(spec) = &plan.action {
            let key = LookAheadKey {
                step,
                spec: spec.clone(),
                start,
                role_filter: self.role_filter(deck),
            };
            self.take_look_ahead(deck, &key)
        } else {
            self.drop_look_ahead(deck);
            None
        };
        match plan.action {
            WanderAction::Role(r, on) => {
                self.set_role(deck, &r, on);
            }
            WanderAction::Stem(s) => self.toggle_stem(deck, &s),
            // over the phrase's last bar
            WanderAction::NetMacro(m) => {
                self.fire_duration(deck, m, Some(1));
            }
            WanderAction::MasterMacro(m) => {
                self.fire_master(deck, m, None);
            }
            WanderAction::Stinger(n) => self.stinger(deck, n, 1),
            WanderAction::Palette(id, pins) => {
                self.set_palette(deck, Some(&id));
                for pin in pins {
                    self.set_role_voice(deck, &pin.role, Some(&pin.option));
                }
            }
            WanderAction::Layers(list) => self.set_layers(deck, list),
            WanderAction::Swap(spec) => {
                mv.cid = Some(share_cid(&spec.payload()));
                match ahead {
                    Some(ahead) if !self.deterministic => {
                        self.swap_with_ahead(deck, &spec, start, ahead);
                    }
                    other => {
                        if let Some(ahead) = other {
                            self.discard_ahead(deck, ahead);
                        }
                        self.swap_to(deck, &spec, start);
                    }
                }
                mv.spec = Some(spec);
            }
        }
        self.deck_mut(deck)?.walk.wander.took_move(mv.clone());
        Some(mv)
    }

    /// Let go of a look-ahead taken for a swap that composes inline after all.
    fn discard_ahead(&mut self, deck: &str, ahead: TakenAhead) {
        if let (Some(i), Some(track)) = (self.index(deck), ahead.ready) {
            self.discard(i, track);
        }
    }
}
