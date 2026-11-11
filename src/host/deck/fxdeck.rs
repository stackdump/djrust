//! The FX unit on a deck (`FxUnitDeck.swift`): where its settings go, which controls a deck has,
//! and where a quantised reset or kill lands. The pokes are `DeckAction.fx` / `.fxReset` /
//! `.fxKill`; the deck keeps the settings (`Deck.fxUnit`, in `DeckParams`, so sessions and
//! journal checkpoints start from them) and sends each change to the chain it plays through
//! now: a deck's [`DeckCore`](super::DeckCore) (net or sample) or, on "L", the loops' bus.
//!
//! A deck's unit lives in its core whichever mode it is in, so Swift's `syncFxUnit` (a mode
//! switch hands the setting to the other chain) has nothing to do here: the setting follows the
//! deck across modes by construction.

use std::collections::BTreeSet;

use crate::deck::beat::Quantize;
use crate::deck::clock::{LoopClock, LOOP_LEAD};
use crate::fx::unit::{FxParam, FxUnitCommand, FxUnitSettings};
use crate::fx::{MasterMacro, StripSettings};
use crate::session::action::DeckAction;
use crate::wander::WanderState;

use crate::control::controller::{DeckMode, DeckView};

use super::core::Command;
use super::engine::Engine;

/// What the pokes aimed at the loop channel `L` leave behind (`loopChannel`'s `Deck`): the FX
/// unit's settings, the strip knobs (the bank's own EQ plays the EQ and filter, the strip reverb
/// the reverb's three; the pitch has no DSP, the channel has no turntable) and the fields only a
/// `Deck` has (`quantize`, `autoAdvance`, `autoGain`, the disabled roles, the walk's settings):
/// `dj["L"]` is the loop channel's own `Deck`, so `DJEngine.applyAction` runs the deck-only ops
/// on it too and `Deck.currentParams` reads them back into a journal checkpoint's `loopChannel`.
/// Nothing of the deck-only fields is audible.
pub struct LoopChannel {
    pub fx_unit: FxUnitSettings,
    pub strip: StripSettings,
    pub reverb_wet: f64,
    pub reverb_size: f64,
    pub reverb_tone: f64,
    /// The channel's aux send (docs/AUX.md).
    pub aux: f64,
    pub pitch: f64,
    /// `Deck.quantize` (`DeckAction.quantize`).
    pub quantize: Quantize,
    /// `Deck.autoAdvance` (`DeckAction.autoAdvance`).
    pub auto_advance: bool,
    /// `Deck.autoGain` (`.param autoGain`).
    pub auto_gain: bool,
    /// `Deck.disabledRoles` (`DeckAction.roleDisable`).
    pub disabled_roles: BTreeSet<String>,
    /// `Deck.wander`: the walk's settings (`DeckAction.wander`, `.wanderPace`, `.wanderLock`).
    /// The loop channel is never a net, so nothing walks: this is only what is set.
    pub wander: WanderState,
    /// The master macros running on the loops' bus (or waiting for their bar), as bits, as the
    /// surface last asked ([`SurfaceHost::deck`](crate::control::controller::SurfaceHost) takes
    /// it from the loops' mirror each time it hands this channel out): what `d.isMasterActive(m)`
    /// reads for a pad that aims at the loops.
    pub master_mask: std::cell::Cell<u32>,
    /// The aux FX macros running aimed at the loops (bits of `AuxMacro::bit`), as of the last
    /// look ([`SurfaceHost::deck`](crate::control::controller::SurfaceHost::deck)).
    pub aux_mask: std::cell::Cell<u32>,
}

impl Default for LoopChannel {
    fn default() -> Self {
        LoopChannel {
            fx_unit: FxUnitSettings::new(),
            strip: StripSettings::default(),
            reverb_wet: 0.0,
            reverb_size: 0.5,
            reverb_tone: 0.5,
            aux: 0.0,
            pitch: 0.0,
            quantize: Quantize::Off,
            auto_advance: false,
            auto_gain: false,
            disabled_roles: BTreeSet::new(),
            wander: WanderState::default(),
            master_mask: std::cell::Cell::new(0),
            aux_mask: std::cell::Cell::new(0),
        }
    }
}

/// The loop channel as a surface target (`SurfaceController.target` reaches `dj["L"]`): the
/// pads that aim at it read its FX unit's kill and neutrality for their lights.
impl DeckView for LoopChannel {
    fn mode(&self) -> DeckMode {
        DeckMode::Sample
    }
    fn fx_killed(&self) -> bool {
        self.fx_unit.killed
    }
    fn fx_neutral(&self) -> bool {
        self.fx_unit.is_neutral()
    }
    fn has_loop_bus(&self) -> bool {
        true
    }
    /// `d.isMasterActive(m)`: a master macro pad aimed at the loops lights while its macro runs
    /// on the loops' bus.
    fn master_active(&self, id: &str) -> bool {
        MasterMacro::from_id(id).is_some_and(|m| self.master_mask.get() & m.bit() != 0)
    }
    fn aux_active(&self, id: &str) -> bool {
        crate::fx::AuxMacro::from_id(id).is_some_and(|m| self.aux_mask.get() & m.bit() != 0)
    }
}

impl Engine {
    fn fx_unit_mut(&mut self, deck: &str) -> Option<&mut FxUnitSettings> {
        if deck == "L" && self.index(deck).is_none() {
            return Some(&mut self.loop_channel.fx_unit);
        }
        self.deck_mut(deck).map(|d| &mut d.fx_unit)
    }

    /// The deck's FX unit setting (`Deck.fxUnit`); "L" has one too.
    pub fn fx_unit(&self, deck: &str) -> Option<&FxUnitSettings> {
        if deck == "L" && self.index(deck).is_none() {
            return Some(&self.loop_channel.fx_unit);
        }
        self.deck(deck).map(|d| &d.fx_unit)
    }

    /// `sendFxUnit`: to whichever chain the deck plays through now.
    fn send_fx_unit(&mut self, deck: &str, c: FxUnitCommand) -> bool {
        if deck == "L" && self.index(deck).is_none() {
            self.send_loop_fx_unit(c);
            return true;
        }
        self.command(deck, Command::FxUnit(c))
    }

    /// `Deck.fxUnitControls`: the controls this deck's chain has right now: every chain control
    /// on any target; the room on a net deck whose song has one (`.space`), echo and feedback on
    /// one whose genre has an echo.
    pub fn fx_unit_controls(&self, deck: &str) -> Vec<FxParam> {
        let mut v: Vec<FxParam> = FxParam::CHAIN.to_vec();
        if let Some(d) = self.deck(deck).filter(|d| d.is_net()) {
            let l = d.track.latest.as_ref();
            if l.is_some_and(|l| l.space.is_some()) {
                v.push(FxParam::Room);
            }
            if l.is_some_and(|l| l.echo.is_some()) {
                v.push(FxParam::Echo);
                v.push(FxParam::Feedback);
            }
        }
        v
    }

    /// `Deck.fxUnitMissing`: why a control is greyed out here, or None when the deck has it.
    pub fn fx_unit_missing(&self, deck: &str, p: FxParam) -> Option<String> {
        if self.fx_unit_controls(deck).contains(&p) {
            return None;
        }
        if deck == "L" && self.index(deck).is_none() {
            return Some("the loops have no room or echo send (their reverb is Reverb)".into());
        }
        if !self.deck(deck).is_some_and(|d| d.is_net()) {
            return Some("a sample deck has no room or echo send (its reverb is Reverb)".into());
        }
        Some(
            if p == FxParam::Room {
                "this song has no room"
            } else {
                "this genre has no echo"
            }
            .into(),
        )
    }

    /// `Deck.setFx`: one control, smoothed on the audio thread.
    pub fn set_fx(&mut self, deck: &str, param: FxParam, value: f64) -> bool {
        let Some(u) = self.fx_unit_mut(deck) else {
            return false;
        };
        u.set(param, value);
        let value = u.get(param);
        self.send_fx_unit(
            deck,
            FxUnitCommand::Set {
                param: param.index() as u8,
                value,
                snap: false,
            },
        )
    }

    /// `Deck.resetFx(atFrame:)`: every control back to neutral and the kill off; `frame` (deck
    /// frames, 0 = now): the block holding it ([`fx_beat_frame`](Self::fx_beat_frame)).
    pub fn fx_reset_at(&mut self, deck: &str, frame: i64) -> bool {
        let Some(u) = self.fx_unit_mut(deck) else {
            return false;
        };
        *u = FxUnitSettings::new();
        if let Some(d) = self.deck_mut(deck) {
            d.fx_killed = false;
        }
        self.send_fx_unit(deck, FxUnitCommand::Reset { frame })
    }

    /// `Deck.killFx(_:atFrame:)`: the unit bypassed (its knobs kept) or back.
    pub fn fx_kill_at(&mut self, deck: &str, on: bool, frame: i64) -> bool {
        let Some(u) = self.fx_unit_mut(deck) else {
            return false;
        };
        u.killed = on;
        if let Some(d) = self.deck_mut(deck) {
            d.fx_killed = on;
        }
        self.send_fx_unit(deck, FxUnitCommand::Kill { on, frame })
    }

    pub fn fx_kill(&mut self, deck: &str, on: bool) -> bool {
        self.fx_kill_at(deck, on, 0)
    }

    pub fn fx_reset(&mut self, deck: &str) -> bool {
        self.fx_reset_at(deck, 0)
    }

    /// `Deck.applyFxUnit`: a whole setting at once, at once (a session's or a checkpoint's
    /// starting state): every control without smoothing, then the kill.
    pub fn apply_fx_unit(&mut self, deck: &str, s: &FxUnitSettings) {
        let Some(u) = self.fx_unit_mut(deck) else {
            return;
        };
        *u = s.clone();
        let killed = s.killed;
        if let Some(d) = self.deck_mut(deck) {
            d.fx_killed = killed;
        }
        self.sync_fx_unit(deck);
    }

    /// `Deck.syncFxUnit`: hand the whole setting to the chain the deck plays through now, without
    /// smoothing (a mode switch: the other chain kept whatever it last had).
    pub(crate) fn sync_fx_unit(&mut self, deck: &str) {
        let Some(s) = self.fx_unit(deck).cloned() else {
            return;
        };
        for p in FxParam::ALL {
            self.send_fx_unit(
                deck,
                FxUnitCommand::Set {
                    param: p.index() as u8,
                    value: s.get(p),
                    snap: true,
                },
            );
        }
        self.send_fx_unit(
            deck,
            FxUnitCommand::Kill {
                on: s.killed,
                frame: 0,
            },
        );
    }

    /// `DJEngine.fxBeatFrame`: where a quantised FX unit move (`onBeat`) on `deck` lands: the
    /// next beat of its own grid at least `LOOP_LEAD` frames ahead (the loops: their clock's), in
    /// that deck's frames. 0 (at once) on a stopped deck or one with no grid.
    pub fn fx_beat_frame(&self, deck: &str) -> i64 {
        if deck == "L" && self.index(deck).is_none() {
            let clock = self.loop_clock(None, None);
            if clock.frames_per_beat <= 0.0 {
                return 0;
            }
            return clock.next_boundary(self.loop_frames() + LOOP_LEAD, 1);
        }
        let Some(d) = self.deck(deck) else { return 0 };
        if !d.playing {
            return 0;
        }
        // (`deck.beatState`: the deck's own, at its effective tempo, not the loop recorder's
        // reading of the mirror, `Engine::beat_state`)
        let Some(s) = d.beat_state(self.sample_rate).filter(|s| s.bpm > 0.0) else {
            return 0;
        };
        let now = d
            .mirror
            .rendered_frames
            .load(std::sync::atomic::Ordering::Relaxed);
        let clock = LoopClock {
            ref_deck: Some(deck.to_string()),
            origin_frame: now,
            origin_beat: s.beat,
            frames_per_beat: 60.0 / s.bpm * self.sample_rate,
            bpm: Some(s.bpm),
        };
        clock.next_boundary(now + LOOP_LEAD, 1)
    }

    /// `FxUnitSettings.actions(deck:)` for the loop channel: the actions that set its unit from
    /// neutral (a recording that starts with it).
    pub(crate) fn loop_channel_fx_actions(&self) -> Vec<DeckAction> {
        let u = &self.loop_channel.fx_unit;
        let mut v: Vec<DeckAction> = u
            .moved()
            .into_iter()
            .map(|p| DeckAction::Fx {
                deck: "L".into(),
                param: p.id().to_string(),
                value: u.get(p),
            })
            .collect();
        if u.killed {
            v.push(DeckAction::FxKill {
                deck: "L".into(),
                on: true,
                on_beat: false,
            });
        }
        v
    }

    /// `DeckParams.autoGain`'s starting gain, there from the first sample.
    pub(crate) fn apply_gain_now(&mut self, deck: &str) {
        self.apply_auto_gain(deck, true);
    }
}
