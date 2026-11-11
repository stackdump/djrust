//! Controllers drive the engine: `SurfaceHost` for [`Engine`] and `DeckView` for [`MainDeck`],
//! the host half of `crate::control` (which decodes a message into an [`Action`] and
//! leaves the deck calls to the host).
//!
//! What a deck does with the actions: volume, gain, crossfader, master, headphones, the FX unit
//! (controls, kill, reset), master FX sliders and macros, net macros, section launches, role and
//! stem mutes, stingers, panic, play / rewind, the tempo fader and the nudge, SYNC and QUANTIZE,
//! the CUE button, hot cues and loop in / out / halve / double (a sample deck's samples, a net
//! deck's ticks; the loop buttons do nothing on a net, as in Swift), the platter, regenerate
//! (the spec's next version), the seed wheel, the walk (the browser push, SHIFT + the wheel), the
//! strip's EQ, filter and reverb knobs, the live keys, and the loops' pads. A control that has a
//! [`DeckAction`](crate::session::action::DeckAction) twin does what the twin does, through
//! the same methods ([`perform`](super::perform)), with the loop channel `L` a target where
//! Swift's is. What no call here answers (an action that is the Swift UI's or the replay's) is
//! counted in [`Engine::unhandled`], so a monitor can say what a control would have done on the
//! Mac.

use std::sync::atomic::Ordering;

use crate::control::controller::{
    Action, DeckMode, DeckParam, DeckView, LoopState, SurfaceHost,
};
use crate::control::jog::{JogClock, JogProfile};
use crate::control::router::RouterHost;
use crate::control::surface::DJActionKind;
use crate::deck::beat::Quantize;
use crate::deck::perform::current_section_index;
use crate::deck::slots::SwapStart;
use crate::deck::turntable::TurntableCommand;
use crate::fx::{MasterFxKey, MasterMacro};
use crate::session::action::DeckAction;

use super::engine::{Engine, MainDeck};
use super::loop_pads::{FileLaunch, Quant};

/// A pad's `sample` argument as a path: absolute, or under `$DJ_SAMPLES`.
pub fn sample_path(sample: Option<&str>) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(sample.filter(|s| !s.is_empty())?);
    if p.is_absolute() {
        return Some(p);
    }
    let root = std::env::var_os("DJ_SAMPLES").filter(|d| !d.is_empty())?;
    Some(std::path::PathBuf::from(root).join(p))
}

/// The loop channel's name in profile bindings and actions.
pub const LOOP_CHANNEL: &str = "L";

impl MainDeck {
    fn role_audible(&self, role: &str) -> bool {
        self.roles
            .iter()
            .find(|(r, _)| r == role)
            .is_some_and(|(_, ix)| ix.iter().any(|&i| !self.mirror.is_net_muted(i)))
    }
}

impl DeckView for MainDeck {
    fn mode(&self) -> DeckMode {
        // a deck with nothing loaded is a sample deck, as `Deck.mode` starts out
        if self.is_net() {
            DeckMode::Net
        } else {
            DeckMode::Sample
        }
    }
    fn hot_cue_set(&self, slot: usize) -> bool {
        self.hot_cues.contains_key(&(slot as i64))
    }
    fn looping(&self) -> bool {
        self.sample.as_ref().is_some_and(|s| s.looping)
    }
    fn loop_start_set(&self) -> bool {
        self.sample.as_ref().is_some_and(|s| s.loop_start.is_some())
    }
    fn playing(&self) -> bool {
        self.playing
    }
    fn quantize_off(&self) -> bool {
        self.quantize == Quantize::Off
    }
    fn pfl(&self) -> bool {
        self.pfl
    }
    fn auto_advance(&self) -> bool {
        self.auto_advance
    }
    fn bar_next_on(&self) -> bool {
        self.bar_next.on
    }
    fn effective_bpm(&self) -> Option<f64> {
        MainDeck::effective_bpm(self)
    }
    fn section_count(&self) -> usize {
        self.sections.len()
    }
    fn current_section(&self) -> Option<usize> {
        current_section_index(&self.sections, self.mirror.tick())
    }
    fn is_role_on(&self, role: &str) -> bool {
        self.role_audible(role)
    }
    fn is_stem_muted(&self, role: &str) -> bool {
        // `mode == .net && stemsMuted[stem] != nil`: the button pressed, not a role silent now
        self.is_net() && self.walk.stems_muted.is_muted(role)
    }
    fn has_spec(&self) -> bool {
        self.spec.is_some()
    }
    /// `d.nextReady != nil`: the next track is composed (the NEXT TRACK pad stops blinking).
    fn next_ready(&self) -> bool {
        self.track.next.ready_spec().is_some()
    }
    fn fx_killed(&self) -> bool {
        self.fx_killed
    }
    fn fx_neutral(&self) -> bool {
        self.fx_unit.is_neutral()
    }
    fn net_macro_active(&self, id: &str) -> bool {
        // a macro of the catalogue: its own bit in the macro record (`wave.activeMacros
        // .contains(m)`); an id that names none (the app's "" asks whether anything runs) is
        // "some macro is running"
        match crate::wave::NetMacro::from_id(id) {
            Some(m) => self.mirror.is_active(m),
            None => self.mirror.macro_active.load(Ordering::Relaxed),
        }
    }
    fn master_active(&self, id: &str) -> bool {
        MasterMacro::from_id(id).is_some_and(|m| self.mirror.is_master_active(m))
    }
    fn aux_active(&self, id: &str) -> bool {
        crate::fx::AuxMacro::from_id(id)
            .is_some_and(|m| self.mirror.aux_mask.load(Ordering::Relaxed) & m.bit() != 0)
    }
    fn loop_take(&self) -> Option<bool> {
        self.loop_take
    }
    fn wander(&self) -> (bool, bool) {
        (self.walk.wander.on, self.walk.wander.paused)
    }
}

/// A deck's jog clock (`Deck`'s `JogClock` half): touch freezes the net clock, turning while
/// touched scratches it (platter seconds to ticks at the deck's tempo), letting go hands it back
/// to the tempo, and the outer ring nudges the tempo. The router calls these on the main
/// thread; [`Engine::service_jogs`] sends them to the deck in order.
#[derive(Default)]
pub struct JogShim {
    pub deck: String,
    pending_nudge: Option<f64>,
    ops: Vec<JogOp>,
    pub freezes: u64,
    pub scrubbed_seconds: f64,
    pub releases: u64,
    /// The controller's SHIFT layer on the message being handled (`RouterHost::jog_shift`).
    shift: bool,
}

/// One wheel move; `bool`: SHIFT was held for it (where the aimed wheel goes, jog_target.rs).
#[derive(Clone, Copy, Debug)]
enum JogOp {
    Freeze(bool),
    Scrub(f64, bool),
    Release,
}

impl JogClock for JogShim {
    fn freeze(&mut self) {
        self.freezes += 1;
        self.ops.push(JogOp::Freeze(self.shift));
    }
    fn scrub_by(&mut self, seconds: f64) {
        self.scrubbed_seconds += seconds;
        // consecutive turns fold into one command (on the same SHIFT layer)
        let shift = self.shift;
        match self.ops.last_mut() {
            Some(JogOp::Scrub(s, sh)) if *sh == shift => *s += seconds,
            _ => self.ops.push(JogOp::Scrub(seconds, shift)),
        }
    }
    fn release(&mut self) {
        self.releases += 1;
        self.ops.push(JogOp::Release);
    }
    fn nudge(&mut self, ratio: f64) {
        self.pending_nudge = Some(ratio);
    }
}

impl Engine {
    fn jog_index(&mut self, deck: &str) -> Option<usize> {
        self.index(deck)?;
        if let Some(i) = self.jogs.iter().position(|j| j.deck == deck) {
            return Some(i);
        }
        self.jogs.push(JogShim {
            deck: deck.to_string(),
            ..Default::default()
        });
        Some(self.jogs.len() - 1)
    }

    /// Apply what the jog rings asked for (call about every 10 ms, with `ControllerRouter::tick`).
    pub fn service_jogs(&mut self) {
        self.sync_history_tempo();
        // a load or an eject moves the aimed wheel between the deck and the history: logged
        self.note_jog_targets();
        for i in 0..self.jogs.len() {
            let ops = std::mem::take(&mut self.jogs[i].ops);
            if !ops.is_empty() {
                let deck = self.jogs[i].deck.clone();
                for op in ops {
                    self.jog(&deck, op);
                }
            }
            if let Some(r) = self.jogs[i].pending_nudge.take() {
                // the ring bends the platter or the net's clock, on top of the tempo fader (the
                // adapter only reports a change, as `JogAdapter` does)
                let deck = self.jogs[i].deck.clone();
                if self.history_aimed(&deck).is_some() {
                    continue; // the outer ring does nothing to a history
                }
                self.nudge(&deck, r);
            }
        }
    }

    fn jog(&mut self, deck: &str, op: JogOp) {
        use super::core::Command;
        use super::jog_target::JogTarget;
        // an aimed wheel follows the deck: the history while it is empty or a modifier is held,
        // latched at touch-down for the whole gesture (jog_target.rs)
        let target = match op {
            JogOp::Freeze(shift) => self.jog_latch(deck, shift),
            _ => self.jog_target(deck).0,
        };
        let op = match op {
            JogOp::Scrub(seconds, shift) => {
                JogOp::Scrub(self.jog_scrub_seconds(deck, seconds, shift), shift)
            }
            o => o,
        };
        if matches!(op, JogOp::Release) {
            self.jog_unlatch(deck);
        }
        if let JogTarget::History(i) = target {
            // this wheel scrubs an input's history (platter seconds -> frames in input_scrub)
            match op {
                JogOp::Freeze(_) => self.input_touch(i),
                JogOp::Scrub(seconds, _) => self.input_scrub(i, seconds),
                JogOp::Release => self.input_release(i),
            };
            return;
        }
        if self.is_sample_deck(deck) {
            let c = match op {
                JogOp::Freeze(_) => TurntableCommand::Freeze,
                JogOp::Scrub(seconds, _) => TurntableCommand::Scrub(seconds * self.sample_rate),
                JogOp::Release => TurntableCommand::Release,
            };
            self.turntable(deck, c);
            return;
        }
        let c = match op {
            JogOp::Freeze(_) => Command::Freeze,
            // platter seconds -> ticks at the deck's tempo (a tick is a sixteenth: 4 per beat):
            // the tempo it is set to run at, base x fader x nudge (`effectiveBPM ?? 120`), not the
            // runner's instantaneous one, which a tempo or Feel macro moves under the hand (and
            // which reads 0 until the first block is published)
            JogOp::Scrub(seconds, _) => {
                let bpm = self
                    .deck(deck)
                    .and_then(|d| d.effective_bpm())
                    .unwrap_or(120.0);
                Command::Scrub(seconds * bpm * 4.0 / 60.0)
            }
            JogOp::Release => Command::Release,
        };
        self.command(deck, c);
    }

    /// The platter by hand (the window's): touch, turn by `seconds` of platter time, let go.
    pub fn jog_touch(&mut self, deck: &str, down: bool) {
        self.jog_touch_shifted(deck, down, false);
    }
    /// The same, with a modifier held at touch-down (the window's SHIFT key): an aimed deck's
    /// platter goes to the history even with a loop on it.
    pub fn jog_touch_shifted(&mut self, deck: &str, down: bool, shift: bool) {
        self.jog(deck, if down { JogOp::Freeze(shift) } else { JogOp::Release });
        self.note_live(
            format!("jog:{deck}:touch"),
            format!("platter {deck} {}", if down { "held" } else { "let go" }),
            false,
            "jog",
            if shift {
                super::vocoder::live_params([("deck", deck.into()), ("touch", down.into()), ("shift", true.into())])
            } else {
                super::vocoder::live_params([("deck", deck.into()), ("touch", down.into())])
            },
        );
    }
    pub fn jog_scrub(&mut self, deck: &str, seconds: f64) {
        self.jog(deck, JogOp::Scrub(seconds, false));
        self.note_live(
            format!("jog:{deck}:scrub"),
            format!("platter {deck} {seconds:+.3} s"),
            true,
            "jog",
            super::vocoder::live_params([("deck", deck.into()), ("scrub", crate::json::JsonValue::Number(seconds))]),
        );
    }

    fn miss(&mut self, what: &str) {
        if self.unhandled.len() < 256 {
            self.unhandled.push(what.to_string());
        }
    }

    /// What a control did, as deck calls (`SurfaceController.handle`'s side of each case).
    pub fn apply_action(&mut self, a: Action) {
        match a {
            Action::Deck { deck, param, value } => {
                let name = match param {
                    DeckParam::Volume => "volume",
                    DeckParam::Filter => "filter",
                    DeckParam::Gain => "gain",
                    DeckParam::EqLow => "eqLow",
                    DeckParam::EqMid => "eqMid",
                    DeckParam::EqHigh => "eqHigh",
                    // the tempo fader, -1...1 for +-8 % (`Deck.pitch`)
                    DeckParam::Pitch => "pitch",
                    DeckParam::ReverbWet => "reverbWet",
                    DeckParam::ReverbSize => "reverbSize",
                };
                // (the router has set the parameter's owner already)
                self.apply_param(&deck, name, value);
            }
            Action::MasterFx { deck, id, value } => match MasterFxKey::from_id(&id) {
                Some(k) => {
                    self.set_master_fx_slider(&deck, k, value as i32);
                }
                None => self.miss(&format!("masterFx {id}")),
            },
            Action::FxUnit { deck, param, value } => {
                self.set_fx(&deck, param, value);
            }
            // `dj.applyAction(.fxKill(deck:on:onBeat:))`: on the next beat of the deck's grid
            Action::FxKill { deck, on, on_beat } => {
                let at = if on_beat {
                    self.fx_beat_frame(&deck)
                } else {
                    0
                };
                self.fx_kill_at(&deck, on, at);
            }
            Action::FxReset { deck, on_beat } => {
                let at = if on_beat {
                    self.fx_beat_frame(&deck)
                } else {
                    0
                };
                self.fx_reset_at(&deck, at);
            }
            Action::Focus { deck } => self.focus = deck,
            Action::Regenerate { deck } => {
                // `d.generate(spec.nextVersion)`: the deck's next version, from its top
                if let Some(next) = self
                    .deck(&deck)
                    .and_then(|d| d.spec.as_ref())
                    .map(|s| s.next_version())
                {
                    if self.deterministic {
                        self.generate(&deck, &next);
                    } else {
                        self.begin_generate(&deck, &next, SwapStart::Top);
                    }
                } else {
                    self.miss(&format!("regenerate {deck} (no spec)"));
                }
            }
            // hand over to the pre-composed next track and keep playing; nothing happens
            // while none is ready
            Action::NextTrack { deck } => self.skip_to_next_track(&deck),
            Action::AutoNextToggle { deck } => {
                if let Some(d) = self.deck_mut(&deck) {
                    d.auto_advance = !d.auto_advance;
                }
            }
            Action::Crossfader(v) => self.set_crossfader(v),
            Action::Master(v) => self.set_master(v),
            Action::HeadphoneMix(v) => self.set_headphone_mix(v),
            Action::HeadphoneLevel(v) => self.set_headphone_level(v),
            Action::Nudge { deck, amount } => {
                self.nudge(&deck, amount * 0.08);
            }
            Action::SeedStep { delta, decks } if !decks.is_empty() => {
                // decks armed on the surface (`deckArm`): every one of them that holds a
                // composed net turns, playing or cued; one alone takes the focus
                let nets: Vec<String> = decks
                    .into_iter()
                    .filter(|n| {
                        self.decks
                            .iter()
                            .any(|d| &d.name == n && d.is_net() && d.spec.is_some())
                    })
                    .collect();
                if nets.is_empty() || delta == 0 {
                    self.miss("seedStep (no armed deck holds a composed net)");
                }
                if nets.len() == 1 {
                    self.focus = nets[0].clone();
                }
                for n in &nets {
                    self.step_seed(n, delta);
                    self.grab_wander(n);
                }
            }
            Action::SeedStep { delta, .. } => {
                // deck-less: the focus deck if it is playing a composed net (loop or song),
                // else the one that is (`SurfaceController`'s `.seedStep`)
                let mut nets: Vec<String> = self
                    .decks
                    .iter()
                    .filter(|d| d.playing && d.is_net() && d.spec.is_some())
                    .map(|d| d.name.clone())
                    .collect();
                nets.sort();
                let pick = if delta == 0 {
                    None
                } else {
                    nets.iter()
                        .find(|n| **n == self.focus)
                        .or_else(|| (nets.len() == 1).then(|| &nets[0]))
                        .cloned()
                };
                match pick {
                    Some(n) => {
                        self.focus = n.clone();
                        self.step_seed(&n, delta);
                        // turning it on a wandering deck grabs the walk
                        self.grab_wander(&n);
                    }
                    None => self.miss("seedStep (no playing composed net deck)"),
                }
            }
            Action::Section { deck, slot } => {
                if slot >= 0 {
                    self.launch_section(&deck, slot as usize);
                }
            }
            Action::SectionStep { deck, delta } => {
                self.step_section(&deck, delta);
            }
            Action::SoundStep { deck, delta } => {
                if !self.step_palette(&deck, delta) {
                    self.miss(&format!("sound {deck} (no net track, or no other palette)"));
                }
            }
            // a deck whose wheel is aimed at an input: its LOOP record button is capture to loop
            // (hold = live capture on the bar, tap = the last beats). The same capture actions
            // are bindable to any control: `captureGrab`, `captureArm`.
            Action::LoopRecord { deck, on } if self.history_aimed(&deck).is_some() => {
                use super::strip_recall::{RecallId, StripId};
                match self.history_aimed(&deck) {
                    Some(RecallId::Input(i)) => {
                        self.capture_arm(i, on);
                    }
                    // another strip's history: LOOP records that strip (docs/LOOP-SOURCES.md)
                    Some(RecallId::Strip(sid)) => {
                        let name = match sid {
                            StripId::Deck(k) => self.decks.get(k).map(|d| d.name.clone()),
                            StripId::Vocoder => Some("V".into()),
                            StripId::Master => Some("M".into()),
                            StripId::Loops => Some("L".into()),
                        };
                        if let Some(src) = name.and_then(|n| self.loop_source(&n, false)) {
                            let bars = self.loops.tap_take_bars;
                            self.source_record(&src, on, if on { bars } else { None });
                        }
                    }
                    None => {}
                }
            }
            // naming a bus or a deck (V, M, A: docs/LOOP-SOURCES.md), a capture control takes
            // from it: a grab of whole bars (`beats` rounded up, 4 bars by default), a hold
            // records
            Action::CaptureGrab { input, beats } => {
                if let Some(src) = self.non_input_source(input.as_deref()) {
                    let bars = beats.map_or(4, |b| ((b / 4.0).ceil() as i64).max(1));
                    self.source_grab(&src, bars);
                } else if let Some(i) = self.capture_input(input.as_deref()) {
                    self.capture_grab(i, beats);
                }
            }
            Action::CaptureArm { input, on } => {
                if let Some(src) = self.non_input_source(input.as_deref()) {
                    let bars = self.loops.tap_take_bars;
                    self.source_record(&src, on, if on { bars } else { None });
                } else if let Some(i) = self.capture_input(input.as_deref()) {
                    self.capture_arm(i, on);
                }
            }
            Action::CaptureBeats { input, delta } => {
                if let Some(i) = self.recall_target(input.as_deref()) {
                    self.input_loop_step(i, delta.signum() as i32);
                }
            }
            // the history controls take any strip with a history (an input, a deck, L, V, M)
            Action::HistoryScrub { input, beats } => {
                if let Some(i) = self.recall_target(input.as_deref()) {
                    self.input_touch(i);
                    self.input_scrub_beats(i, beats);
                }
            }
            Action::HistoryLive { input } => {
                if let Some(i) = self.recall_target(input.as_deref()) {
                    self.input_live(i);
                }
            }
            Action::HistoryLoop { input, beats } => {
                if let Some(i) = self.recall_target(input.as_deref()) {
                    let beats = beats.unwrap_or(self.recall_loop_beats(i));
                    self.input_loop(i, beats);
                }
            }
            Action::LibraryLoad { slot } => {
                if let Some(m) = self.library_entries().get(slot) {
                    let id = m.id.clone();
                    self.library_load(&id);
                }
            }
            Action::LibrarySave { slot } => {
                if let Some(id) = self.loops.row.clips.get(slot).map(|c| c.id.clone()) {
                    self.library_save(&id);
                }
            }
            Action::LoopRecord { deck, on } => {
                let bars = self.loops.tap_take_bars;
                match self.loop_source(&deck, false) {
                    // V, M or an input (a LOOP binding's `source`)
                    Some(src) if self.index(&deck).is_none() => {
                        self.source_record(&src, on, if on { bars } else { None });
                    }
                    _ => {
                        self.loop_record(&deck, on, bars);
                    }
                }
            }
            Action::ToggleRole { deck, role } => {
                self.toggle_role(&deck, &role);
            }
            Action::ToggleStem { deck, role } => self.toggle_stem(&deck, &role),
            Action::Macro { deck, id } => {
                if !Self::is_macro_id(&id) {
                    self.miss(&format!("macro {id}"));
                } else {
                    self.apply_macro(&deck, &id, None);
                }
            }
            Action::Panic { deck } => {
                self.apply_deck_op(&DeckAction::Panic { deck });
            }
            // Hit 1-4 as curated (`Deck.stinger`)
            Action::Stinger { deck, slot } => self.stinger(&deck, slot, 1),
            Action::KeyDown {
                deck,
                index,
                velocity,
                quant,
            } => self.live_key(&deck, index as i64, velocity, true, quant.as_deref()),
            Action::KeyUp { deck, index } => self.live_key(&deck, index as i64, 0, false, None),
            Action::PlayToggle { deck } => {
                // `deck.setPlaying(!deck.playing)`: the main thread's flag, not the mirror's,
                // which is a block behind
                if let Some(playing) = self.deck(&deck).map(|d| d.playing) {
                    self.set_playing(&deck, !playing);
                }
            }
            Action::Rewind { deck } => {
                self.rewind(&deck);
            }
            // a deck whose wheel is aimed at an input's history: CUE is back to live and the loop
            // buttons loop beats of it
            Action::Cue { deck } if self.history_aimed(&deck).is_some() => {
                let Some(i) = self.history_aimed(&deck) else { return };
                self.input_live(i);
            }
            Action::LoopIn { deck } if self.history_aimed(&deck).is_some() => {
                if let Some(i) = self.history_aimed(&deck) {
                    let beats = self.recall_loop_beats(i);
                    self.input_loop(i, beats);
                }
            }
            Action::LoopOut { deck } if self.history_aimed(&deck).is_some() => {
                let Some(i) = self.history_aimed(&deck) else { return };
                self.input_live(i);
            }
            Action::LoopHalve { deck } if self.history_aimed(&deck).is_some() => {
                let Some(i) = self.history_aimed(&deck) else { return };
                self.input_loop_step(i, -1);
            }
            Action::LoopDouble { deck } if self.history_aimed(&deck).is_some() => {
                let Some(i) = self.history_aimed(&deck) else { return };
                self.input_loop_step(i, 1);
            }
            Action::Sync { deck, bar } => {
                if self.sync_to_partner(&deck, bar).is_none() {
                    // counted, as an action that did nothing (the hub's mixer reads these)
                    self.miss(&format!("sync {deck} (no deck to sync to)"));
                }
            }
            Action::QuantizeCycle { deck } => {
                self.cycle_quantize(&deck);
            }
            Action::Cue { deck } => self.cue(&deck),
            Action::HotCue { deck, slot } if slot >= 0 => self.hot_cue(&deck, slot as usize),
            Action::ClearCue { deck, slot } if slot >= 0 => self.clear_cue(&deck, slot as usize),
            Action::LoopIn { deck } => self.loop_in(&deck),
            Action::LoopOut { deck } => self.loop_out(&deck),
            Action::LoopHalve { deck } => self.loop_halve(&deck),
            Action::LoopDouble { deck } => self.loop_double(&deck),
            Action::PflToggle { deck } => {
                let on = !self.deck(&deck).is_some_and(|d| d.pfl);
                self.set_pfl(&deck, on);
            }
            // `dj.noteAutoMixTouch(deck, kind)`; a seed step's deck is the one the step moves
            Action::AutoMixTouch { deck, kind } => self.note_auto_mix_touch(deck.as_deref(), kind),
            // `dj.setAutoMix(on:bars:every:)` with the settings it has, and `dj.autoMixNow()`
            // (a press is recorded as the MIDI that made it, so neither is a `DeckAction` here)
            Action::AutoMixSet { on } => {
                let s = &self.auto_mix.settings;
                let (bars, every) = (s.bars, s.every);
                self.set_auto_mix(on, bars, every);
            }
            Action::AutoMixNow => self.auto_mix_now(),
            Action::WanderToggle { deck, on } => self.set_wander(&deck, on),
            Action::WanderPace { deck, delta } => self.step_wander_pace(&deck, delta),
            // `dj.setLooping(clips[slot].id, on)`: a slot with no clip does nothing
            Action::ClipToggle { slot, on } => {
                if let Some(id) = self.loops.row.clips.get(slot).map(|c| c.id.clone()) {
                    self.set_looping(&id, on);
                }
            }
            Action::Target {
                ref id,
                deck: ref action_deck,
                slot,
                index,
                pressed,
                value,
                ref args,
                ..
            } => {
                let slot_ix = slot.filter(|s| *s >= 0).map(|s| s as usize);
                let arg = |k: &str| args.get(k).map(String::as_str);
                match id.as_str() {
                    "clipToDeck" => {
                        if pressed {
                            let to = action_deck.clone().unwrap_or_else(|| self.focus.clone());
                            if let Err(e) = self.clip_to_deck(None, slot_ix, &to) {
                                self.miss(&format!("clipToDeck: {e}"));
                            }
                        }
                    }
                    "copyDeck" => {
                        // the other deck onto this one (docs/COPY-DECK.md)
                        if pressed {
                            let to = action_deck.clone().unwrap_or_else(|| self.focus.clone());
                            let from = arg("from")
                                .filter(|f| !f.is_empty())
                                .map(str::to_string)
                                .or_else(|| self.sync_partner(&to));
                            match from {
                                Some(f) => {
                                    if let Err(e) = self.copy_deck(&f, &to) {
                                        self.miss(&format!("copyDeck: {e}"));
                                    }
                                }
                                None => self.miss("copyDeck (no other deck)"),
                            }
                        }
                    }
                    "vocoder" => {
                        // a knob: its travel across the param's range (`instance`: that vocoder,
                        // else the selected one)
                        let name = arg("param").filter(|p| !p.is_empty()).unwrap_or("mix");
                        match crate::fx::vocoder::VocoderParam::parse(name) {
                            Some(p) => {
                                if let Err(e) = self.on_bound_vocoder(arg("instance"), |e| {
                                    e.vocoder_knob(p, value);
                                }) {
                                    self.miss(&format!("vocoder: {e}"));
                                }
                            }
                            None => self.miss(&format!("vocoder (no knob {name:?})")),
                        }
                    }
                    // the aux bus (docs/AUX.md): a send on a deck or an input (a knob's travel),
                    // the return, one of the six sliders in its own units, the return's mute
                    "auxSend" => {
                        let strip = arg("strip")
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .or_else(|| action_deck.clone())
                            .unwrap_or_else(|| self.focus.clone());
                        if !self.set_aux_send(&strip, value) {
                            self.miss(&format!("auxSend (no strip {strip:?})"));
                        }
                    }
                    "auxReturn" => self.set_aux_return(value),
                    "auxFx" => match arg("key").and_then(crate::fx::AuxFxKey::from_id) {
                        Some(k) => self.set_aux_fx(k, k.slider_value(value) as f64),
                        None => self.miss("auxFx (no such slider)"),
                    },
                    "auxMute" => {
                        if pressed {
                            let on = !self.aux.mute;
                            self.set_aux_mute(on);
                        }
                    }
                    // the live post chain on the master (docs/POST.md): on / off, the preset
                    "postToggle" => {
                        if pressed {
                            let on = !self.post.on;
                            self.set_post(on, None);
                        }
                    }
                    "postPreset" => {
                        if pressed {
                            match arg("op").filter(|o| !o.is_empty()).unwrap_or("next") {
                                "next" => {
                                    self.step_post_preset(1);
                                }
                                "prev" => {
                                    self.step_post_preset(-1);
                                }
                                name => match crate::fx::post::PostChain::preset(name) {
                                    Some(c) => {
                                        let on = !c.is_empty();
                                        self.set_post(on, Some(c));
                                    }
                                    None => self.miss(&format!("postPreset (no preset {name:?})")),
                                },
                            }
                        }
                    }
                    "trickKnob" => {
                        let to = action_deck.clone().unwrap_or_else(|| self.focus.clone());
                        let name = arg("kind").filter(|k| !k.is_empty()).unwrap_or("filter");
                        let r = crate::fx::tricks::TrickKind::parse(name)
                            .ok_or_else(|| format!("no trick {name:?}"))
                            .and_then(|k| self.trick_knob(&to, k, value));
                        if let Err(e) = r {
                            self.miss(&format!("trickKnob: {e}"));
                        }
                    }
                    "autoNextBars" => {
                        let to = action_deck.clone().unwrap_or_else(|| self.focus.clone());
                        if let Err(e) = self.bar_next_target(&to, args, pressed) {
                            self.miss(&format!("autoNextBars: {e}"));
                        }
                    }
                    "trick" => {
                        let to = action_deck.clone().unwrap_or_else(|| self.focus.clone());
                        if let Err(e) = self.trick_target(&to, args, pressed) {
                            self.miss(&format!("trick: {e}"));
                        }
                    }
                    // the history the jog is aimed at, back to now (`deck`: that deck's jog)
                    "historySnap" => {
                        let d = arg("deck").filter(|d| !d.is_empty()).map(str::to_uppercase);
                        if !self.history_snap(d.as_deref()) {
                            self.miss("historySnap (no jog aimed at a history)");
                        }
                    }
                    // held: the aimed deck's wheel scrubs its history even with a loop on the
                    // deck (the SHIFT modifier as a binding of its own; jog_target.rs)
                    "historyJog" => {
                        let d = arg("deck")
                            .filter(|d| !d.is_empty())
                            .map(str::to_string)
                            .or_else(|| action_deck.clone());
                        if pressed && self.history_jog.is_none() {
                            self.miss("historyJog (no jog aimed at a history: --history-jog)");
                        }
                        self.history_jog_hold(d.as_deref(), pressed);
                    }
                    "freeze" | "freezeKnob" => {
                        if let Err(e) = self.freeze_target(id, arg("input"), args, value, pressed) {
                            self.miss(&format!("{id}: {e}"));
                        }
                    }
                    "vocoderKeys" => {
                        let t = arg("transpose").and_then(|t| t.trim().parse::<i64>().ok()).unwrap_or(0);
                        let note = (index as i64 + t).clamp(0, 127) as u8;
                        if let Err(e) = self.on_bound_vocoder(arg("instance"), |e| e.vocoder_note(note, pressed)) {
                            self.miss(&format!("vocoderKeys: {e}"));
                        }
                    }
                    "vocoderSwitch" => {
                        if pressed {
                            let op = arg("op").unwrap_or("on");
                            // select / select_next / select_prev / add pick the instance themselves
                            let r = if op.starts_with("select") || op == "add" {
                                let which = if op == "select" { arg("instance").or(arg("input")) } else { None };
                                self.vocoder_target_with(op, which, arg("beats"), arg("preset"))
                            } else {
                                self.on_bound_vocoder(arg("instance"), |e| {
                                    e.vocoder_target_with(op, arg("input"), arg("beats"), arg("preset"))
                                })
                                .and_then(|r| r)
                            };
                            if let Err(e) = r {
                                self.miss(&format!("vocoderSwitch: {e}"));
                            }
                        }
                    }
                    "inputLatency" => {
                        // a knob sets the offset across `range` (ms, default 0,250)
                        let (lo, hi) = arg("range")
                            .and_then(|r| r.split_once(','))
                            .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                            .unwrap_or((0.0f64, 250.0f64));
                        match self.capture_input(arg("input")) {
                            Some(i) => {
                                self.set_input_latency(i, lo + value.clamp(0.0, 1.0) * (hi - lo));
                            }
                            None => self.miss("inputLatency (no such input)"),
                        }
                    }
                    "keyMode" => {
                        if pressed {
                            match arg("mode").unwrap_or("toggle") {
                                "toggle" => {
                                    self.toggle_key_mode();
                                }
                                m => match crate::control::controller::KeyMode::parse(m) {
                                    Some(mode) => self.set_key_mode(mode),
                                    None => self.miss(&format!("keyMode ({m:?}: live, transpose, td3 or toggle)")),
                                },
                            }
                        }
                    }
                    "voxPage" => {
                        if pressed {
                            self.vox_page_step(arg("op").unwrap_or("next"));
                        }
                    }
                    "keyDeck" => {
                        if pressed {
                            if let Err(e) = self.set_key_deck(arg("to").unwrap_or("next")) {
                                self.miss(&format!("keyDeck: {e}"));
                            }
                        }
                    }
                    "transpose" => {
                        if pressed {
                            self.transpose_loops(
                                slot_ix,
                                index.min(127) as u8,
                                Quant::parse(arg("quant"), Quant::Bar),
                                arg("mode") == Some("tape"),
                            );
                        }
                    }
                    "loopRoot" => {
                        if pressed {
                            self.set_loop_root(slot_ix, (index % 12) as u8);
                        }
                    }
                    "oneShot" | "loopFile" => {
                        if pressed {
                            // the only bus there is today is the loop channel L; say so rather
                            // than playing a pad on a bus it did not ask for in silence
                            if let Some(bus) = arg("bus").filter(|b| !b.eq_ignore_ascii_case("L")) {
                                self.miss(&format!("{id}: bus {bus} (only L exists; playing on L)"));
                            }
                            // `vox:N`: the Nth file of the vocal page (docs/MPK.md, "Vocals")
                            let vox_pad = arg("sample")
                                .and_then(|s| s.strip_prefix("vox:"))
                                .map(|n| n.trim().parse::<usize>());
                            if let (Some(pad), true) = (vox_pad, id == "oneShot") {
                                let Ok(pad) = pad else {
                                    self.miss(&format!("oneShot ({:?}: vox:N takes a pad number 0-15)", arg("sample").unwrap_or("")));
                                    return;
                                };
                                let launch = self.vox_launch(
                                    pad,
                                    arg("quant"),
                                    arg("choke"),
                                    arg("bpm").and_then(|b| b.parse().ok()),
                                    arg("gain")
                                        .and_then(|g| g.parse::<f32>().ok())
                                        .filter(|g| g.is_finite() && *g >= 0.0)
                                        .unwrap_or(1.0),
                                );
                                match launch {
                                    Ok(l) => self.trigger_file(l),
                                    Err(e) => self.miss(&e),
                                }
                                return;
                            }
                            match sample_path(arg("sample")) {
                                Some(path) => {
                                    let once = id == "oneShot";
                                    self.trigger_file(FileLaunch {
                                        path,
                                        bars: arg("bars").and_then(|b| b.parse().ok()),
                                        bpm: arg("bpm").and_then(|b| b.parse().ok()),
                                        once,
                                        quant: Quant::parse(
                                            arg("quant"),
                                            if once { Quant::Beat } else { Quant::Bar },
                                        ),
                                        gain: arg("gain")
                                            .and_then(|g| g.parse::<f32>().ok())
                                            .filter(|g| g.is_finite() && *g >= 0.0)
                                            .unwrap_or(1.0),
                                        choke: arg("choke")
                                            .filter(|c| !c.is_empty())
                                            .map(str::to_string),
                                        launch: true,
                                    });
                                }
                                None => self.miss(&format!("{id} (no sample set)")),
                            }
                        }
                    }
                    _ => self.hub_requests.push(a),
                }
            }
            other => self.miss(&format!("{other:?}")),
        }
    }
}

impl SurfaceHost for Engine {
    fn deck(&self, name: &str) -> Option<&dyn DeckView> {
        match Engine::deck(self, name) {
            Some(d) => Some(d as &dyn DeckView),
            // the loop channel is a target like a deck (`dj["L"]`); the macros its bus is
            // running are the mirror's as of now
            None if name == "L" => {
                self.loop_channel
                    .master_mask
                    .set(self.loop_master_running_mask());
                self.loop_channel.aux_mask.set(self.aux_mirror.loop_mask());
                Some(&self.loop_channel as &dyn DeckView)
            }
            None => None,
        }
    }
    fn deck_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        v.sort();
        v
    }
    fn focus(&self) -> String {
        self.focus.clone()
    }
    fn library_count(&self) -> usize {
        self.library_entries().len()
    }
    fn is_locked(&self, key: &str) -> bool {
        self.locked.iter().any(|k| k == key)
    }
    fn fader_position(&self, action: DJActionKind, deck: Option<&str>) -> Option<f64> {
        let d = || deck.and_then(|n| Engine::deck(self, n));
        if deck == Some(LOOP_CHANNEL) {
            let strip = &self.loop_channel.strip;
            return match action {
                DJActionKind::Volume => Some(self.loops.volume),
                DJActionKind::Filter => Some(strip.filter),
                DJActionKind::EqLow => Some(strip.eq_low),
                DJActionKind::EqMid => Some(strip.eq_mid),
                DJActionKind::EqHigh => Some(strip.eq_high),
                _ => None,
            };
        }
        match action {
            DJActionKind::Volume => d().map(|d| d.volume),
            DJActionKind::Gain => d().map(|d| d.gain),
            DJActionKind::Filter => d().map(|d| d.strip.filter),
            DJActionKind::EqLow => d().map(|d| d.strip.eq_low),
            DJActionKind::EqMid => d().map(|d| d.strip.eq_mid),
            DJActionKind::EqHigh => d().map(|d| d.strip.eq_high),
            DJActionKind::Pitch => d().map(|d| (d.pitch + 1.0) / 2.0),
            DJActionKind::Crossfader => Some(self.crossfader),
            DJActionKind::Master => Some(self.master),
            DJActionKind::HeadphoneMix => Some(self.headphone_mix),
            DJActionKind::HeadphoneLevel => Some(self.headphone_level),
            _ => None,
        }
    }
    fn owner(&self, param: &str) -> Option<String> {
        self.owners.get(param).cloned()
    }
    fn set_owner(&mut self, param: &str, owner: &str) {
        self.owners.insert(param.into(), owner.into());
    }
    fn perform(&mut self, action: Action) {
        // a controller's move: the journal has the MIDI that made it (`in_surface`)
        let outer = std::mem::replace(&mut self.in_surface, true);
        if self.trace.is_some() {
            let via = self.trace_via.clone();
            let focus_before = self.focus.clone();
            let before = self.trace.as_ref().map_or(0, Vec::len);
            self.apply_action(action.clone());
            // what the action traced itself (a target that performs a clip move, say): its
            // description then carries no op of its own
            self.surface_traced = self.trace.as_ref().map_or(0, Vec::len) - before;
            self.trace_control(&action, &via, &focus_before);
            self.surface_traced = 0;
        } else {
            self.apply_action(action);
        }
        self.in_surface = outer;
    }
    fn auto_mix(&self) -> (bool, bool) {
        (self.auto_mix.settings.on, self.auto_mix.run.is_some())
    }
    fn key_mode(&self) -> crate::control::controller::KeyMode {
        self.keys.mode
    }
    fn key_deck(&self) -> Option<String> {
        self.keys.deck.clone().filter(|d| Engine::deck(self, d).is_some())
    }
    fn clip_count(&self) -> usize {
        self.loops.row.clips.len()
    }
    fn clip_loop_state(&self, slot: usize) -> Option<LoopState> {
        let id = &self.loops.row.clips.get(slot)?.id;
        Some(self.loop_state(id))
    }
}

impl RouterHost for Engine {
    fn jog_clock(&mut self, deck: &str) -> Option<&mut dyn JogClock> {
        let i = self.jog_index(deck)?;
        Some(&mut self.jogs[i] as &mut dyn JogClock)
    }

    fn jog_shift(&mut self, deck: &str, shift: bool) {
        if let Some(i) = self.jog_index(deck) {
            self.jogs[i].shift = shift;
        }
    }

    /// `dj.recorder?.record(midi:)` and `dj.journal?.record(.midi)`.
    fn record_midi(&mut self, port: &str, bytes: &[u8]) {
        let frame = self.frame();
        if let Some(r) = self.recorder.as_mut() {
            r.record_midi(frame, port, bytes);
        }
        if let Some(j) = self.journal.as_mut() {
            j.record(
                frame,
                crate::journal::event::JournalEvent::Midi {
                    port: port.to_string(),
                    bytes: bytes.to_vec(),
                },
            );
        }
    }
}

#[allow(dead_code)]
fn _jog_profile_is_unused(_: &JogProfile) {}
