//! `SurfaceController.swift`: routes one surface's messages and computes its LED feedback.
//!
//! The Swift class talks to `DJEngine` and `Deck` directly. Here the engine is a
//! [`SurfaceHost`]: the controller *reads* the engine through it (soft-takeover positions,
//! locks, focus, the deck state the LEDs show, through [`DeckView`]) and *writes* by
//! emitting [`Action`] values through [`SurfaceHost::perform`]. A host (the offline
//! engine, `djhost`, a test) turns actions into deck calls, so the decode and the layout
//! logic are pure and tested here against the same literals the Swift tests use.
//!
//! Where the Swift code asks a deck something mid-decision (`d.fxUnit.killed`,
//! `d.mode == .net`), this asks the [`DeckView`] at the same point, so the emitted action
//! list matches what Swift would have done to the deck.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::control::macros::{self, master_fx_slider_value, FxParam, NetMacro};
use crate::control::surface::{
    Binding, ControlEvent, DJActionKind, Leds, SurfaceDecoder, SurfaceProfile,
};

/// What a decoded message did, for monitors, the UI and the coverage log (`SurfaceOutcome`).
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceOutcome {
    Bound(Binding),
    /// Decoded, deliberately parked by the layout.
    Unbound {
        reason: String,
    },
    /// Decoded, but the layout has no entry at all (a profile gap).
    Unmapped,
    /// Bound, but held back by soft takeover: another control (or the UI) moved this
    /// parameter last, and this one has not reached its value yet.
    Pickup(Binding),
}

/// `PadLight.Level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightLevel {
    Off,
    Dim,
    Full,
    Blink,
}

/// One LED as a display shows it (`PadLight`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadLight {
    /// APC palette index (RGB pads); 0 for single-colour LEDs.
    pub color_index: u8,
    pub level: LightLevel,
}

impl PadLight {
    pub fn new(color_index: u8, level: LightLevel) -> PadLight {
        PadLight { color_index, level }
    }
}

/// `DJEngine.loopState` (one definition: [`crate::deck::loops::LoopState`]).
pub use crate::deck::loops::LoopState;

/// `Deck.mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckMode {
    Net,
    Sample,
}

/// What `AutoMixTouch` carries: a control that steers the music pauses auto-mix. Auto-mix's
/// own (`automix::engine`), with `Expression` (the knobs, mutes, macros and FX that never
/// pause it) the surface has no binding for yet.
pub use crate::automix::engine::AutoMixTouch;

/// The per-deck parameters a continuous control writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckParam {
    Volume,
    Filter,
    Gain,
    EqLow,
    EqMid,
    EqHigh,
    /// −1…1 (±8 %), after the centre detent.
    Pitch,
    ReverbWet,
    ReverbSize,
}

/// What a control did to the engine, as data. Each variant names the Swift call it stands
/// for; `deck` is always a concrete deck name (an aimed `"*"` binding is already resolved
/// through [`SurfaceHost::focus`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `deck.volume = v`, `.filter`, `.gain`, `.eqLow/Mid/High`, `.pitch`, `.reverb.wet/size`.
    Deck {
        deck: String,
        param: DeckParam,
        value: f64,
    },
    /// `deck.setMasterFx(key, key.sliderValue(position))`: `value` is the slider position
    /// already in the slider's own units.
    MasterFx {
        deck: String,
        id: String,
        value: i64,
    },
    /// `dj.applyAction(.fx(deck:param:value:))`, unrecorded.
    FxUnit {
        deck: String,
        param: FxParam,
        value: f64,
    },
    /// `.fxKill(deck:on:onBeat:)` (`on` = the opposite of the unit's current kill).
    FxKill {
        deck: String,
        on: bool,
        on_beat: bool,
    },
    /// `.fxReset(deck:onBeat:)`.
    FxReset {
        deck: String,
        on_beat: bool,
    },
    /// `dj.focus = deck`.
    Focus {
        deck: String,
    },
    /// `Task { await d.generate(spec.nextVersion) }`.
    Regenerate {
        deck: String,
    },
    /// `deck.autoAdvance.toggle()`.
    AutoNextToggle {
        deck: String,
    },
    Crossfader(f64),
    Master(f64),
    HeadphoneMix(f64),
    HeadphoneLevel(f64),
    /// `dj.setLooping(clip[slot].id, on)`.
    ClipToggle {
        slot: usize,
        on: bool,
    },
    /// Capture to loop (docs/INPUT-HISTORY.md). `input` is an input strip's label (None: the
    /// one a deck's jog is aimed at, else the first with a history). Bindable from any
    /// control: a profile entry `{"action": "captureGrab", "param": "TD-3"}`.
    ///
    /// Grab the last `beats` (None: the strip's loop length) of the history onto the loop row.
    CaptureGrab {
        input: Option<String>,
        beats: Option<f64>,
    },
    /// Arm a live capture (`on`: pressed, starts on the next bar) or end it (released, rounds
    /// up to whole bars). A quick tap grabs instead.
    CaptureArm {
        input: Option<String>,
        on: bool,
    },
    /// Step the capture length through 0.5 ... 32 beats (`delta` < 0 halves, > 0 doubles).
    CaptureBeats {
        input: Option<String>,
        delta: i64,
    },
    /// Put the `slot`th saved loop of the library on the loop row (not playing yet).
    LibraryLoad {
        slot: usize,
    },
    /// Save the `slot`th clip of the row to the library.
    LibrarySave {
        slot: usize,
    },
    /// Scrub an input's history by `beats` (+ back in time, - toward live). Touches the head
    /// like a platter would: it holds where it is until [`Action::HistoryLive`].
    HistoryScrub {
        input: Option<String>,
        beats: f64,
    },
    /// Back to the live input.
    HistoryLive {
        input: Option<String>,
    },
    /// Loop the last `beats` (None: the strip's loop length) of an input's history in place.
    HistoryLoop {
        input: Option<String>,
        beats: Option<f64>,
    },
    /// `dj.loopRecord(deck, on:, bars: tapTakeBars)`. Beyond Swift, `deck` may name another
    /// loop source: `V`, `M` or an input strip's label (docs/LOOP-SOURCES.md).
    LoopRecord {
        deck: String,
        on: bool,
    },
    /// `dj[focus]?.nudge(amount * Deck.pitchRange)`; `amount` is −1…1.
    Nudge {
        deck: String,
        amount: f64,
    },
    /// The playing net's seed ± each click, recomposed. `decks`: the decks armed on the
    /// surface (`deckArm`, beyond Swift), each stepped; empty: the host picks the deck exactly
    /// as `SurfaceController.handle`'s `.seedStep` does. Grabs wander.
    SeedStep {
        delta: i64,
        decks: Vec<String>,
    },
    /// `d.setWander(on)`.
    WanderToggle {
        deck: String,
        on: bool,
    },
    /// `d.stepWanderPace(delta)`.
    WanderPace {
        deck: String,
        delta: i64,
    },
    /// `dj.setAutoMix(on:bars:every:)` with the current bars / every.
    AutoMixSet {
        on: bool,
    },
    /// `dj.autoMixNow()`.
    AutoMixNow,
    /// `d.keyDown(index, velocity:)` on the deck the keys are aimed at (the binding's, else the
    /// key route's, else the focus deck). `quant`: "now" / "beat" / "bar", the binding's own;
    /// None: the key route's.
    KeyDown {
        deck: String,
        index: usize,
        velocity: i64,
        quant: Option<String>,
    },
    /// `d.keyUp(index)` on the focus deck.
    KeyUp {
        deck: String,
        index: usize,
    },
    Section {
        deck: String,
        slot: i64,
    },
    SectionStep {
        deck: String,
        delta: i64,
    },
    /// The deck's sound palette, `delta` along the playable ones (`soundPrev` / `soundNext`):
    /// the same song from the next bar (`DeckAction.setPalette`).
    SoundStep {
        deck: String,
        delta: i64,
    },
    ToggleRole {
        deck: String,
        role: String,
    },
    ToggleStem {
        deck: String,
        role: String,
    },
    Macro {
        deck: String,
        id: String,
    },
    Panic {
        deck: String,
    },
    Stinger {
        deck: String,
        slot: i64,
    },
    NextTrack {
        deck: String,
    },
    /// `deck.sync(to: <the other deck>, align: .bar | .beat)`.
    Sync {
        deck: String,
        bar: bool,
    },
    /// off → beat → bar → off.
    QuantizeCycle {
        deck: String,
    },
    Rewind {
        deck: String,
    },
    PflToggle {
        deck: String,
    },
    PlayToggle {
        deck: String,
    },
    Cue {
        deck: String,
    },
    HotCue {
        deck: String,
        slot: i64,
    },
    ClearCue {
        deck: String,
        slot: i64,
    },
    LoopIn {
        deck: String,
    },
    LoopOut {
        deck: String,
    },
    LoopHalve {
        deck: String,
    },
    LoopDouble {
        deck: String,
    },
    /// A registry target the engine host resolves by id (`control::registry`): everything
    /// that needs more than the fixed variants above. `index` is the control's index in its
    /// group (a key's note number); `value` is 0...1; `pressed` is false on release.
    Target {
        id: String,
        deck: Option<String>,
        slot: Option<i64>,
        index: usize,
        value: f64,
        pressed: bool,
        args: BTreeMap<String, String>,
    },
    /// `dj.noteAutoMixTouch(deck, kind)`. A seed step's touch names the deck the step would
    /// move (the controller resolves it and sends none when there is none); `deck: None` is a
    /// deck-less touch a host may still receive.
    AutoMixTouch {
        deck: Option<String>,
        kind: AutoMixTouch,
    },
}

/// The deck state a surface's LEDs and decisions read (`Deck`). Every method has a quiet
/// default, so a host implements only what its decks have.
pub trait DeckView {
    fn mode(&self) -> DeckMode {
        DeckMode::Sample
    }
    fn playing(&self) -> bool {
        false
    }
    fn hot_cue_set(&self, _slot: usize) -> bool {
        false
    }
    fn looping(&self) -> bool {
        false
    }
    fn loop_start_set(&self) -> bool {
        false
    }
    fn pfl(&self) -> bool {
        false
    }
    fn quantize_off(&self) -> bool {
        true
    }
    fn auto_advance(&self) -> bool {
        false
    }
    /// Auto-next every N bars is on (beyond Swift).
    fn bar_next_on(&self) -> bool {
        false
    }
    fn effective_bpm(&self) -> Option<f64> {
        None
    }
    fn section_count(&self) -> usize {
        0
    }
    fn current_section(&self) -> Option<usize> {
        None
    }
    fn is_role_on(&self, _role: &str) -> bool {
        true
    }
    fn is_stem_muted(&self, _role: &str) -> bool {
        false
    }
    /// `d.nextReady != nil`.
    fn next_ready(&self) -> bool {
        false
    }
    /// The deck has a current `TrackSpec` (`currentSpec != nil`).
    fn has_spec(&self) -> bool {
        false
    }
    fn fx_killed(&self) -> bool {
        false
    }
    fn fx_neutral(&self) -> bool {
        true
    }
    /// `d.bank != nil`: this is the loop channel, whose master bus plays only the master
    /// macros.
    fn has_loop_bus(&self) -> bool {
        false
    }
    /// `d.wave.activeMacros.contains(m)` for the net macro `id`.
    fn net_macro_active(&self, _id: &str) -> bool {
        false
    }
    /// `d.isMasterActive(m)` for the master macro `id`.
    fn master_active(&self, _id: &str) -> bool {
        false
    }
    /// Is the aux / pan / shape macro `id` (`fx::AuxMacro`) running on this deck?
    fn aux_active(&self, _id: &str) -> bool {
        false
    }
    /// `(on, paused)` of the deck's wander.
    fn wander(&self) -> (bool, bool) {
        (false, false)
    }
    /// `dj.loopTakeProgress(deck)`: Some(released) while a loop take runs.
    fn loop_take(&self) -> Option<bool> {
        None
    }
}

/// The engine as the controllers see it (`DJEngine`).
/// What the keys of a controller do (the `keys` target; docs/MPK.md). One switch for the whole
/// hub, so a key never goes two ways: the TD-3 window used to take them before the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyMode {
    /// Play a deck's live lead voice, scale-locked, optionally quantised.
    #[default]
    Live,
    /// Transpose the loop row to the key pressed (`transpose`).
    Transpose,
    /// Play the TD-3 (the app sends the notes; the map sees nothing).
    Td3,
}

impl KeyMode {
    pub fn raw(self) -> &'static str {
        match self {
            KeyMode::Live => "live",
            KeyMode::Transpose => "transpose",
            KeyMode::Td3 => "td3",
        }
    }

    pub fn parse(s: &str) -> Option<KeyMode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "live" | "lead" | "deck" => KeyMode::Live,
            "transpose" | "loops" => KeyMode::Transpose,
            "td3" | "td-3" => KeyMode::Td3,
            _ => return None,
        })
    }
}

pub trait SurfaceHost {
    /// A deck by name, if it exists (`dj[name]`).
    fn deck(&self, name: &str) -> Option<&dyn DeckView>;
    /// Every deck name, sorted (`dj.decks.keys.sorted()`).
    fn deck_names(&self) -> Vec<String>;
    /// `dj.focus`.
    fn focus(&self) -> String;
    /// `dj.lockedActions.contains(key)`.
    fn is_locked(&self, key: &str) -> bool {
        let _ = key;
        false
    }
    /// The current value of a continuous parameter in its control's 0…1 travel
    /// (`faderPosition`): `action` is the binding's, `deck` its deck. None where there is
    /// no such deck or the action is not a fader.
    fn fader_position(&self, action: DJActionKind, deck: Option<&str>) -> Option<f64>;
    /// `dj.owners[param]`.
    fn owner(&self, param: &str) -> Option<String>;
    fn set_owner(&mut self, param: &str, owner: &str);
    /// `dj.clips.count`.
    fn clip_count(&self) -> usize {
        0
    }
    /// `dj.loopState(dj.clips[slot].id)`; None when the slot holds no clip.
    fn clip_loop_state(&self, _slot: usize) -> Option<LoopState> {
        None
    }
    /// Loops saved in the library, for `libraryLoad` slots.
    fn library_count(&self) -> usize {
        0
    }
    /// `dj.autoMix`: `(settings.on, a hand-off is running)`.
    fn auto_mix(&self) -> (bool, bool) {
        (false, false)
    }
    /// What the `keys` target does now.
    fn key_mode(&self) -> KeyMode {
        KeyMode::Live
    }
    /// The deck the keys are aimed at, when it is not simply the aimed one.
    fn key_deck(&self) -> Option<String> {
        None
    }
    /// Apply what a control did.
    fn perform(&mut self, action: Action);
}

/// Palette indices (APC mini mk2 velocity → colour chart) (`SurfaceController.Color`).
pub mod color {
    pub const OFF: u8 = 0;
    pub const RED: u8 = 5;
    pub const ORANGE: u8 = 9;
    pub const CYAN: u8 = 37;
    // beats' APC macro-group colours (lib/backend/apc-mini-mk2.js `GROUP_COLOR`)
    pub const WHITE: u8 = 3;
    pub const YELLOW: u8 = 13;
    pub const GREEN: u8 = 21;
    pub const MAGENTA: u8 = 53;
    pub const BLUE: u8 = 45;
    /// The SHIFT loop page: Akai's chart index 48 (#B98CFF).
    pub const VIOLET: u8 = 48;
}

pub const FLASH_SECONDS: f64 = 0.18;
/// Centre detent for pitch faders: ±2 % of travel snaps to zero.
pub const PITCH_DEAD_ZONE: f64 = 0.02;
/// Centre detent for a bipolar FX unit fader (filter, pitch): ±3 % of travel is neutral.
pub const FX_DEAD_ZONE: f64 = 0.03;
/// Soft takeover accepts a value this close to the parameter's.
pub const PICKUP_WINDOW: f64 = 0.03;

fn key(group: &str, index: usize) -> String {
    alloc::format!("{group}#{index}")
}

/// A fader position (0…1) on an FX unit control's travel (`SurfaceController.fxValue`).
/// Unipolar controls are linear. Bipolar ones (the filter, pitch) have a centre detent that
/// lands exactly on neutral, so a fader parked in the middle leaves the chain
/// bit-transparent; pitch steps in whole semitones.
pub fn fx_value(p: FxParam, position: f64) -> f64 {
    let x = if position.is_finite() {
        position.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let (lo, hi) = p.range();
    if !(lo < 0.0) {
        return lo + x * (hi - lo);
    }
    let c = x * 2.0 - 1.0;
    if !(c.abs() > FX_DEAD_ZONE) {
        return p.neutral();
    }
    let u = (c.abs() - FX_DEAD_ZONE) / (1.0 - FX_DEAD_ZONE) * if c < 0.0 { -1.0 } else { 1.0 };
    let v = u * hi;
    if p == FxParam::Pitch {
        libm::round(v)
    } else {
        v
    }
}

/// beats' APC colour for a master macro's group (`SurfaceController.color(_:)`).
pub fn master_macro_color(id: &str) -> u8 {
    match macros::master_macro_group(id) {
        Some("Pitch") => color::MAGENTA,
        Some("Feel") => color::GREEN,
        _ => color::CYAN,
    }
}

/// beats' APC colour for one of the eleven pan / shape / aux-FX macros (`GROUP_COLOR` in
/// lib/backend/apc-mini-mk2.js: FX cyan, Pan blue, Shape orange).
pub fn aux_macro_color(id: &str) -> u8 {
    match macros::aux_macro_group(id) {
        Some("Pan") => color::BLUE,
        Some("Shape") => color::ORANGE,
        _ => color::CYAN,
    }
}

/// What a `focusDeck` binding aims at: slot 0 deck A, 1 deck B, 2 the loop channel.
pub fn aim(slot: Option<i64>) -> &'static str {
    match slot {
        Some(1) => "B",
        Some(2) => "L",
        _ => "A",
    }
}

/// Engine parameter a continuous binding writes, for ownership
/// (`SurfaceController.parameter`).
pub fn parameter(b: &Binding) -> Option<String> {
    match b.action {
        DJActionKind::Crossfader => Some("crossfader".to_string()),
        DJActionKind::Master => Some("master".to_string()),
        DJActionKind::HeadphoneMix => Some("headphoneMix".to_string()),
        DJActionKind::HeadphoneLevel => Some("headphoneLevel".to_string()),
        DJActionKind::Volume
        | DJActionKind::Filter
        | DJActionKind::Gain
        | DJActionKind::EqLow
        | DJActionKind::EqMid
        | DJActionKind::EqHigh
        | DJActionKind::Pitch => Some(alloc::format!(
            "{}.{}",
            b.deck.as_deref().unwrap_or("?"),
            b.action.raw()
        )),
        _ => None,
    }
}

/// Routes one surface's messages to the engine and computes its LED feedback
/// (`SurfaceController`).
#[derive(Clone, Debug)]
pub struct SurfaceController {
    profile: Arc<SurfaceProfile>,
    decoder: SurfaceDecoder,
    exact: BTreeMap<String, Vec<Binding>>,      // "group#index"
    group_wide: BTreeMap<String, Vec<Binding>>, // "group"
    led_cache: BTreeMap<u16, u16>,              // channel<<8 | note → status<<8 | vel
    flashed: BTreeMap<String, f64>,
    /// Last value each continuous control sent, for soft-takeover crossing.
    last_value: BTreeMap<String, f64>,
    /// Software shift (APC): true while a `shift` control is held.
    shift_held: bool,
    /// Decks armed for the browser wheel (`deckArm`, latching; beyond Swift).
    armed: BTreeSet<String>,
    /// The deck whose arm the button held now flipped: another control used before it is let
    /// go makes it a shift chord, not a tap, and flips the arm back.
    arm_pending: Option<String>,
}

impl SurfaceController {
    pub fn new(profile: Arc<SurfaceProfile>) -> SurfaceController {
        let mut exact: BTreeMap<String, Vec<Binding>> = BTreeMap::new();
        let mut group_wide: BTreeMap<String, Vec<Binding>> = BTreeMap::new();
        for b in &profile.layout {
            match b.index {
                Some(i) => exact.entry(key(&b.group, i)).or_default().push(b.clone()),
                None => group_wide
                    .entry(b.group.clone())
                    .or_default()
                    .push(b.clone()),
            }
        }
        SurfaceController {
            decoder: SurfaceDecoder::new(profile.clone()),
            profile,
            exact,
            group_wide,
            led_cache: BTreeMap::new(),
            flashed: BTreeMap::new(),
            last_value: BTreeMap::new(),
            shift_held: false,
            armed: BTreeSet::new(),
            arm_pending: None,
        }
    }

    /// The decks armed for the browser wheel (`deckArm`), in order.
    pub fn armed(&self) -> Vec<String> {
        self.armed.iter().cloned().collect()
    }

    pub fn profile(&self) -> &Arc<SurfaceProfile> {
        &self.profile
    }

    pub fn shift_held(&self) -> bool {
        self.shift_held
    }

    pub fn decode(&mut self, bytes: &[u8]) -> Option<ControlEvent> {
        self.decoder.decode(bytes)
    }

    /// The binding for an event under the current shift state.
    pub fn binding_for(&self, e: &ControlEvent) -> Option<&Binding> {
        let k = key(&e.group, e.index);
        let candidates = self.exact.get(&k).or_else(|| self.group_wide.get(&e.group));
        Self::layer(candidates?, self.shift_held)
    }

    /// The binding a control answers to under the shift state `held`: one made for this
    /// layer (`shift` equal to `held`) wins over one for either layer (`shift` nil), so a
    /// `"shift": true` entry beside a plain one is that control's SHIFT layer (the APC's
    /// loop page over its grid).
    fn layer(candidates: &[Binding], held: bool) -> Option<&Binding> {
        candidates
            .iter()
            .find(|b| b.shift == Some(held))
            .or_else(|| candidates.iter().find(|b| b.shift.is_none()))
    }

    /// The binding a control's LED shows: the same layer its press would reach.
    fn lit(&self, group: &str, index: usize, shift: Option<bool>) -> Option<&Binding> {
        let held = shift.unwrap_or(self.shift_held);
        Self::layer(self.exact.get(&key(group, index))?, held)
    }

    pub fn outcome_for(&self, e: &ControlEvent) -> SurfaceOutcome {
        match self.binding_for(e) {
            None => SurfaceOutcome::Unmapped,
            Some(b) if b.action == DJActionKind::Unbound => SurfaceOutcome::Unbound {
                reason: b.reason.clone().unwrap_or_else(|| "parked".to_string()),
            },
            Some(b) => SurfaceOutcome::Bound(b.clone()),
        }
    }

    /// Decode and apply one raw message. None for messages the profile does not describe
    /// at all.
    pub fn handle_bytes(
        &mut self,
        bytes: &[u8],
        now: f64,
        host: &mut dyn SurfaceHost,
    ) -> Option<(ControlEvent, SurfaceOutcome)> {
        let e = self.decode(bytes)?;
        let o = self.handle(&e, now, host);
        Some((e, o))
    }

    /// The deck a binding acts on: its own, or for "*" the aimed one (`dj.focus`), if it
    /// exists (`target(_:)`).
    fn target(&self, b: &Binding, host: &dyn SurfaceHost) -> Option<String> {
        let name = if b.deck.as_deref() == Some("*") {
            Some(host.focus())
        } else {
            b.deck.clone()
        };
        name.filter(|n| host.deck(n).is_some())
    }

    fn flash(&mut self, b: &Binding, now: f64) {
        if let Some(i) = b.index {
            self.flashed.insert(key(&b.group, i), now);
        }
    }

    pub fn handle(
        &mut self,
        e: &ControlEvent,
        now: f64,
        host: &mut dyn SurfaceHost,
    ) -> SurfaceOutcome {
        let result = self.outcome_for(e);
        // an arm button held down while another control is used: a chord on its hardware
        // shift layer (the Inpulse's SHIFT), not a tap, so the arm goes back as it was
        if let Some(d) = self.arm_pending.clone() {
            let own = matches!(&result, SurfaceOutcome::Bound(b)
                if b.action == DJActionKind::DeckArm && b.deck.as_deref() == Some(d.as_str()));
            if !own {
                if !self.armed.remove(&d) {
                    self.armed.insert(d);
                }
                self.arm_pending = None;
            }
        }
        let b = match &result {
            SurfaceOutcome::Bound(b) => b.clone(),
            _ => return result,
        };
        // locked by the user (right-click ▸ Disable on its button): the press does nothing
        if host.is_locked(&b.lock_key()) {
            return result;
        }
        let deck = self.target(&b, host);
        let press = e.pressed && (!e.is_cc || e.value > 0.5);
        self.note_auto_mix_touch(&b, deck.as_deref(), e, press, host);

        if let (Some(param), Some(index)) = (parameter(&b), b.index) {
            let me = alloc::format!("{}/{}", self.profile.name, key(&b.group, index));
            let v = e.value;
            let previous = self.last_value.insert(me.clone(), v);
            if let Some(owner) = host.owner(&param) {
                if owner != me {
                    if let Some(cur) = host.fader_position(b.action, b.deck.as_deref()) {
                        let crossed = previous.map_or(false, |p| (p - cur) * (v - cur) <= 0.0);
                        if !((v - cur).abs() < PICKUP_WINDOW || crossed) {
                            return SurfaceOutcome::Pickup(b);
                        }
                    }
                }
            }
            host.set_owner(&param, &me);
        }

        use DJActionKind as K;
        let d_param =
            |deck: &Option<String>, param: DeckParam, value: f64, host: &mut dyn SurfaceHost| {
                if let Some(deck) = deck {
                    host.perform(Action::Deck {
                        deck: deck.clone(),
                        param,
                        value,
                    });
                }
            };
        match b.action {
            K::Volume => d_param(&deck, DeckParam::Volume, e.value, host),
            K::Filter => d_param(&deck, DeckParam::Filter, e.value, host),
            K::Gain => d_param(&deck, DeckParam::Gain, e.value, host),
            K::EqLow => d_param(&deck, DeckParam::EqLow, e.value, host),
            K::EqMid => d_param(&deck, DeckParam::EqMid, e.value, host),
            K::EqHigh => d_param(&deck, DeckParam::EqHigh, e.value, host),
            K::Pitch => {
                let p = e.value * 2.0 - 1.0;
                let p = if p.abs() < PITCH_DEAD_ZONE { 0.0 } else { p };
                d_param(&deck, DeckParam::Pitch, p, host)
            }
            K::MasterFx => {
                // a slider position on its own range, as beats' FX panel sends it
                if let (Some(d), Some(id)) = (&deck, &b.macro_id) {
                    if let Some(value) = master_fx_slider_value(id, e.value) {
                        host.perform(Action::MasterFx {
                            deck: d.clone(),
                            id: id.clone(),
                            value,
                        });
                    }
                }
            }
            K::ReverbWet => d_param(&deck, DeckParam::ReverbWet, e.value, host),
            K::ReverbSize => d_param(&deck, DeckParam::ReverbSize, e.value, host),
            K::FxUnit => {
                // the value is the poke (a setting, immediate and smoothed on the audio
                // thread); applied unrecorded, as the MIDI that moved it is what the
                // session and the journal keep
                if let (Some(d), Some(p)) = (&deck, b.param.as_deref().and_then(FxParam::from_id)) {
                    host.perform(Action::FxUnit {
                        deck: d.clone(),
                        param: p,
                        value: fx_value(p, e.value),
                    });
                }
            }
            K::FxKill | K::FxReset => {
                if let (true, Some(d)) = (press, deck.clone()) {
                    self.flash(&b, now);
                    if b.action == K::FxKill {
                        let killed = host.deck(&d).map_or(false, |v| v.fx_killed());
                        host.perform(Action::FxKill {
                            deck: d,
                            on: !killed,
                            on_beat: true,
                        });
                    } else {
                        host.perform(Action::FxReset {
                            deck: d,
                            on_beat: true,
                        });
                    }
                }
            }
            K::FocusDeck => {
                if press {
                    host.perform(Action::Focus {
                        deck: aim(b.slot).to_string(),
                    });
                }
            }
            K::Regenerate => {
                if let (true, Some(d)) = (press, deck.clone()) {
                    if host.deck(&d).map_or(false, |v| v.has_spec()) {
                        self.flash(&b, now);
                        host.perform(Action::Regenerate { deck: d });
                    }
                }
            }
            K::AutoNext => {
                if let (true, Some(d)) = (press, deck.clone()) {
                    host.perform(Action::AutoNextToggle { deck: d });
                }
            }
            K::Crossfader => host.perform(Action::Crossfader(e.value)),
            K::Master => host.perform(Action::Master(e.value)),
            K::HeadphoneMix => host.perform(Action::HeadphoneMix(e.value)),
            K::HeadphoneLevel => host.perform(Action::HeadphoneLevel(e.value)),
            K::Shift => self.shift_held = e.pressed,
            K::ClipToggle => {
                if let (true, Some(slot)) = (press, b.slot) {
                    let slot = slot as usize;
                    if slot < host.clip_count() {
                        if let Some(state) = host.clip_loop_state(slot) {
                            let on = matches!(state, LoopState::Off | LoopState::Stopping);
                            host.perform(Action::ClipToggle { slot, on });
                        }
                    }
                }
            }
            K::CaptureGrab => {
                if press {
                    self.flash(&b, now);
                    let beats = b.slot.filter(|n| *n > 0).map(|n| n as f64);
                    host.perform(Action::CaptureGrab {
                        input: b.param.clone(),
                        beats,
                    });
                }
            }
            K::CaptureArm => {
                if press {
                    self.flash(&b, now);
                }
                host.perform(Action::CaptureArm {
                    input: b.param.clone(),
                    on: press,
                });
            }
            K::CaptureBeats => {
                // an encoder steps by its clicks; a button by its slot's sign (default: double)
                let delta = if e.delta != 0 {
                    e.delta.signum()
                } else if press {
                    b.slot.map_or(1, |s| if s < 0 { -1 } else { 1 })
                } else {
                    0
                };
                if delta != 0 {
                    self.flash(&b, now);
                    host.perform(Action::CaptureBeats {
                        input: b.param.clone(),
                        delta,
                    });
                }
            }
            K::HistoryScrub => {
                // an encoder: its clicks, a quarter beat each unless args["step"] says; a button:
                // one step back (slot < 0: forward)
                let step = b.args.get("step").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.25);
                let clicks = if e.delta != 0 {
                    e.delta as f64
                } else if press {
                    b.slot.map_or(1.0, |s| if s < 0 { -1.0 } else { 1.0 })
                } else {
                    0.0
                };
                if clicks != 0.0 {
                    self.flash(&b, now);
                    host.perform(Action::HistoryScrub {
                        input: b.param.clone(),
                        beats: clicks * step,
                    });
                }
            }
            K::HistoryLive => {
                if press {
                    self.flash(&b, now);
                    host.perform(Action::HistoryLive { input: b.param.clone() });
                }
            }
            K::HistoryLoop => {
                if press {
                    self.flash(&b, now);
                    let beats = b.slot.filter(|n| *n > 0).map(|n| n as f64);
                    host.perform(Action::HistoryLoop {
                        input: b.param.clone(),
                        beats,
                    });
                }
            }
            K::LibraryLoad => {
                if let (true, Some(slot)) = (press, b.slot) {
                    if slot >= 0 && (slot as usize) < host.library_count() {
                        self.flash(&b, now);
                        host.perform(Action::LibraryLoad { slot: slot as usize });
                    }
                }
            }
            K::LibrarySave => {
                if let (true, Some(slot)) = (press, b.slot) {
                    if slot >= 0 && (slot as usize) < host.clip_count() {
                        self.flash(&b, now);
                        host.perform(Action::LibrarySave { slot: slot as usize });
                    }
                }
            }
            K::LoopRecord => {
                // a `source` arg (V, M, an input's label: docs/LOOP-SOURCES.md) records that
                // instead of the deck
                if let Some(src) = b.args.get("source").filter(|x| !x.is_empty()) {
                    host.perform(Action::LoopRecord {
                        deck: src.clone(),
                        on: press,
                    });
                } else if let Some(d) = deck.clone() {
                    if press {
                        host.perform(Action::Focus { deck: d.clone() });
                    }
                    host.perform(Action::LoopRecord { deck: d, on: press });
                }
            }
            K::Unbound => {}
            K::OneShot | K::LoopFile | K::Transpose | K::LoopRoot | K::MapPreset | K::KeyMode | K::KeyDeck | K::VoxPage | K::Td3ClockSource | K::MidiOut | K::InputLatency | K::LoopRoute | K::ClipToDeck | K::CopyDeck | K::Vocoder | K::VocoderSwitch | K::VocoderKeys | K::Freeze | K::FreezeKnob | K::HistorySnap | K::HistoryJog | K::Trick | K::TrickKnob | K::HeosVolume | K::HeosMute | K::AutoNextBars | K::AuxSend | K::AuxReturn | K::AuxFx | K::AuxMute | K::PostToggle | K::PostPreset => {
                // keys send a release too (the host may care), knobs every move; pads only act
                // on press
                if press
                    || matches!(
                        b.action,
                        K::Transpose
                            | K::MidiOut
                            | K::InputLatency
                            | K::Vocoder
                            | K::VocoderKeys
                            | K::Freeze
                            | K::FreezeKnob
                            | K::HistoryJog
                            | K::Trick
                            | K::TrickKnob
                            | K::HeosVolume
                            | K::AuxSend
                            | K::AuxReturn
                            | K::AuxFx
                    )
                {
                    self.flash(&b, now);
                    host.perform(Action::Target {
                        id: b.action.raw().to_string(),
                        // L and other non-deck targets pass through unresolved
                        deck: if b.deck.as_deref() == Some("*") {
                            Some(host.focus())
                        } else {
                            b.deck.clone()
                        },
                        slot: b.slot,
                        index: e.index,
                        value: e.value,
                        pressed: press,
                        args: b.args.clone(),
                    });
                }
            }
            K::Nudge => {
                // centred control: 0.5 = no nudge, full travel = ± the tempo-fader range
                let f = host.focus();
                if host.deck(&f).is_some() {
                    host.perform(Action::Nudge {
                        deck: f,
                        amount: (e.value - 0.5) * 2.0,
                    });
                }
            }
            K::SeedStep => {
                if e.delta != 0 {
                    let decks: Vec<String> = self
                        .armed
                        .iter()
                        .filter(|d| host.deck(d).is_some())
                        .cloned()
                        .collect();
                    host.perform(Action::SeedStep {
                        delta: e.delta,
                        decks,
                    });
                }
            }
            K::DeckArm => {
                if let Some(d) = deck.clone() {
                    if press {
                        // latching: each press flips it (taken back if it turns out a chord)
                        if !self.armed.remove(&d) {
                            self.armed.insert(d.clone());
                        }
                        self.arm_pending = Some(d);
                    } else {
                        self.arm_pending = None;
                    }
                }
            }
            K::Wander => {
                // deck-less: the focus deck's walk on or off (locked with its on-screen
                // button too)
                let f = host.focus();
                if let (true, Some(v)) = (press, host.deck(&f)) {
                    let net = v.mode() == DeckMode::Net;
                    let on = v.wander().0;
                    let lock = DJActionKind::lock_key(Some(&f), K::Wander, None);
                    if net && !host.is_locked(&lock) {
                        self.flash(&b, now);
                        host.perform(Action::WanderToggle { deck: f, on: !on });
                    }
                }
            }
            K::WanderPace => {
                let f = host.focus();
                if e.delta != 0 && host.deck(&f).map_or(false, |v| v.mode() == DeckMode::Net) {
                    host.perform(Action::WanderPace {
                        deck: f,
                        delta: e.delta,
                    });
                }
            }
            K::AutoMix => {
                // recorded as the MIDI that pressed it, replayed through the router
                if press {
                    self.flash(&b, now);
                    let (on, _) = host.auto_mix();
                    host.perform(Action::AutoMixSet { on: !on });
                }
            }
            K::AutoMixNow => {
                if press {
                    self.flash(&b, now);
                    host.perform(Action::AutoMixNow);
                }
            }
            K::Section
            | K::SectionPrev
            | K::SectionNext
            | K::SoundPrev
            | K::SoundNext
            | K::RoleMute
            | K::StemMute
            | K::Macro
            | K::Panic
            | K::Stinger
            | K::NextTrack
            | K::Keys
            | K::Sync
            | K::Quantize
            | K::Rewind
            | K::Pfl => self.perform(&b, deck.as_deref(), e, press, now, host),
            K::Play
            | K::Cue
            | K::Hotcue
            | K::Clearcue
            | K::LoopIn
            | K::LoopOut
            | K::LoopHalve
            | K::LoopDouble => {
                if let (true, Some(d)) = (press, deck.clone()) {
                    self.flash(&b, now);
                    let slot = b.slot.unwrap_or(0);
                    host.perform(match b.action {
                        K::Play => Action::PlayToggle { deck: d },
                        K::Cue => Action::Cue { deck: d },
                        K::Hotcue => Action::HotCue { deck: d, slot },
                        K::Clearcue => Action::ClearCue { deck: d, slot },
                        K::LoopIn => Action::LoopIn { deck: d },
                        K::LoopOut => Action::LoopOut { deck: d },
                        K::LoopHalve => Action::LoopHalve { deck: d },
                        _ => Action::LoopDouble { deck: d },
                    });
                } else if press {
                    // a press on a binding with no deck still flashes its pad, as Swift's
                    // `guard press` ahead of the `deck?` calls does
                    self.flash(&b, now);
                }
            }
        }
        result
    }

    /// Tell auto-mix what a control did (`noteAutoMixTouch`): the steering ones pause it;
    /// knobs, mutes, macros and FX don't. The crossfader is not a touch.
    fn note_auto_mix_touch(
        &self,
        b: &Binding,
        deck: Option<&str>,
        e: &ControlEvent,
        press: bool,
        host: &mut dyn SurfaceHost,
    ) {
        use DJActionKind as K;
        let kind = match b.action {
            K::Play => AutoMixTouch::Transport,
            K::NextTrack | K::Regenerate => AutoMixTouch::Load,
            K::Cue
            | K::Hotcue
            | K::LoopIn
            | K::LoopOut
            | K::LoopHalve
            | K::LoopDouble
            | K::Sync
            | K::Section
            | K::SectionPrev
            | K::SectionNext
            | K::SoundPrev
            | K::SoundNext
            | K::Rewind => AutoMixTouch::Steer,
            K::SeedStep => {
                // the touch is on the deck the step would move: the playing composed net decks
                // (`mode == .net`, a `currentSpec`), the focus deck among them, else the only
                // one; with none the wheel moves nothing and does not touch auto-mix
                // (`SurfaceController.noteAutoMixTouch`, `.seedStep`)
                if e.delta == 0 {
                    return;
                }
                let nets: Vec<String> = host
                    .deck_names()
                    .into_iter()
                    .filter(|n| {
                        host.deck(n).is_some_and(|d| {
                            d.playing() && d.mode() == DeckMode::Net && d.has_spec()
                        })
                    })
                    .collect();
                let focus = host.focus();
                let pick = nets
                    .iter()
                    .find(|n| **n == focus)
                    .or_else(|| (nets.len() == 1).then(|| &nets[0]))
                    .cloned();
                if let Some(d) = pick {
                    host.perform(Action::AutoMixTouch {
                        deck: Some(d),
                        kind: AutoMixTouch::Steer,
                    });
                }
                return;
            }
            K::Nudge => {
                if (e.value - 0.5).abs() > 0.02 {
                    host.perform(Action::AutoMixTouch {
                        deck: Some(host.focus()),
                        kind: AutoMixTouch::Steer,
                    });
                }
                return;
            }
            _ => return,
        };
        if let (true, Some(d)) = (press, deck) {
            host.perform(Action::AutoMixTouch {
                deck: Some(d.to_string()),
                kind,
            });
        }
    }

    /// Net-deck performance actions. They act on the binding's deck, or the focus deck for
    /// deck-less ones (keys) (`perform(_:deck:event:press:now:)`).
    fn perform(
        &mut self,
        b: &Binding,
        deck: Option<&str>,
        e: &ControlEvent,
        press: bool,
        now: f64,
        host: &mut dyn SurfaceHost,
    ) {
        use DJActionKind as K;
        if b.action == K::Keys {
            let down = e.pressed && e.value > 0.0;
            match host.key_mode() {
                // the app plays the TD-3 itself, before the map; nothing here
                KeyMode::Td3 => {}
                KeyMode::Transpose => {
                    let mut args = b.args.clone();
                    args.entry("quant".to_string()).or_insert_with(|| "bar".to_string());
                    host.perform(Action::Target {
                        id: K::Transpose.raw().to_string(),
                        deck: Some("L".to_string()),
                        slot: None,
                        index: e.index,
                        value: e.value,
                        pressed: down,
                        args,
                    });
                }
                KeyMode::Live => {
                    // the binding's own deck, else the key route's, else the aimed one
                    let target = b
                        .deck
                        .clone()
                        .filter(|d| d != "*")
                        .or_else(|| host.key_deck())
                        .unwrap_or_else(|| host.focus());
                    if host.deck(&target).is_none() {
                        return;
                    }
                    if down {
                        let velocity = (libm::round(e.value * 127.0) as i64).max(1);
                        host.perform(Action::KeyDown {
                            deck: target,
                            index: e.index,
                            velocity,
                            quant: b.args.get("quant").cloned(),
                        });
                    } else {
                        host.perform(Action::KeyUp {
                            deck: target,
                            index: e.index,
                        });
                    }
                }
            }
            return;
        }
        let Some(deck) = deck else { return };
        if !press {
            return;
        }
        let deck = deck.to_string();
        host.perform(Action::Focus { deck: deck.clone() });
        self.flash(b, now);
        let slot = b.slot.unwrap_or(0);
        match b.action {
            K::Section => host.perform(Action::Section { deck, slot }),
            K::SectionPrev => host.perform(Action::SectionStep { deck, delta: -1 }),
            K::SectionNext => host.perform(Action::SectionStep { deck, delta: 1 }),
            K::SoundPrev => host.perform(Action::SoundStep { deck, delta: -1 }),
            K::SoundNext => host.perform(Action::SoundStep { deck, delta: 1 }),
            K::RoleMute => {
                if let Some(role) = &b.role {
                    host.perform(Action::ToggleRole {
                        deck,
                        role: role.clone(),
                    });
                }
            }
            K::StemMute => {
                if let Some(role) = &b.role {
                    host.perform(Action::ToggleStem {
                        deck,
                        role: role.clone(),
                    });
                }
            }
            K::Macro => {
                if let Some(id) = &b.macro_id {
                    host.perform(Action::Macro {
                        deck,
                        id: id.clone(),
                    });
                }
            }
            K::Panic => host.perform(Action::Panic { deck }),
            K::Stinger => host.perform(Action::Stinger { deck, slot }),
            K::NextTrack => host.perform(Action::NextTrack { deck }),
            K::Sync => host.perform(Action::Sync {
                deck,
                bar: b.slot == Some(1),
            }),
            K::Quantize => host.perform(Action::QuantizeCycle { deck }),
            K::Rewind => host.perform(Action::Rewind { deck }),
            K::Pfl => host.perform(Action::PflToggle { deck }),
            _ => {}
        }
    }

    // MARK: LEDs

    /// LED messages whose state changed since the last call (empty for surfaces without
    /// LEDs). Call ~20x/s and send the result to the surface's output (`ledFrame`).
    pub fn led_frame(&mut self, now: f64, host: &dyn SurfaceHost) -> Vec<[u8; 3]> {
        let profile = self.profile.clone();
        let Some(leds) = profile.leds.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<[u8; 3]> = Vec::new();
        let mut emit =
            |cache: &mut BTreeMap<u16, u16>, ch: u8, note: u8, status_base: u8, vel: u8| {
                let status = status_base | ch;
                let k = u16::from(ch) << 8 | u16::from(note);
                let sig = u16::from(status) << 8 | u16::from(vel);
                if cache.get(&k) == Some(&sig) {
                    return;
                }
                cache.insert(k, sig);
                out.push([status, note, vel]);
            };
        for g in profile
            .groups
            .iter()
            .filter(|g| leds.rgb_groups.contains(&g.id))
        {
            for (i, note) in g.control_numbers().into_iter().enumerate() {
                let k = key(&g.id, i);
                let full = now - self.flashed.get(&k).copied().unwrap_or(-1.0) < FLASH_SECONDS;
                let (color, level) = self.rgb_state(self.lit(&g.id, i, None), host);
                // APC: the status byte's low nibble is the brightness, so no channel here.
                let shown = if full { LightLevel::Full } else { level };
                let status = match shown {
                    LightLevel::Full => leds.rgb_full_status,
                    LightLevel::Blink => leds.rgb_blink_status.unwrap_or(leds.rgb_dim_status),
                    LightLevel::Dim | LightLevel::Off => leds.rgb_dim_status,
                };
                emit(&mut self.led_cache, 0, note, status, color);
            }
        }
        for g in profile
            .groups
            .iter()
            .filter(|g| leds.single_groups.contains(&g.id))
        {
            for (i, note) in g.control_numbers().into_iter().enumerate() {
                let v = self.single_state(self.lit(&g.id, i, None), leds, None, host);
                emit(&mut self.led_cache, g.channel.unwrap_or(0), note, 0x90, v);
            }
        }
        out
    }

    /// What each LED of the surface is showing right now — the same state `led_frame`
    /// sends, as data, for a mirror on screen. Keyed by group id, one entry per control.
    /// `shift`: the layer to show instead of the one `shift_held` picks (`lights`).
    pub fn lights(
        &self,
        now: f64,
        shift: Option<bool>,
        host: &dyn SurfaceHost,
    ) -> BTreeMap<String, Vec<PadLight>> {
        let mut out = BTreeMap::new();
        let Some(leds) = self.profile.leds.as_ref() else {
            return out;
        };
        for g in self
            .profile
            .groups
            .iter()
            .filter(|g| leds.rgb_groups.contains(&g.id))
        {
            let lights = (0..g.control_numbers().len())
                .map(|i| {
                    let k = key(&g.id, i);
                    let full = now - self.flashed.get(&k).copied().unwrap_or(-1.0) < FLASH_SECONDS;
                    let (color, level) = self.rgb_state(self.lit(&g.id, i, shift), host);
                    if full {
                        return PadLight::new(color, LightLevel::Full);
                    }
                    // a surface that cannot blink a pad shows it dim, and so does its mirror
                    let shown = if level == LightLevel::Blink && leds.rgb_blink_status.is_none() {
                        LightLevel::Dim
                    } else {
                        level
                    };
                    PadLight::new(color, shown)
                })
                .collect();
            out.insert(g.id.clone(), lights);
        }
        for g in self
            .profile
            .groups
            .iter()
            .filter(|g| leds.single_groups.contains(&g.id))
        {
            let lights = (0..g.control_numbers().len())
                .map(|i| {
                    let v = self.single_state(self.lit(&g.id, i, shift), leds, shift, host);
                    let level = if v == leds.single_blink {
                        LightLevel::Blink
                    } else if v == leds.single_on {
                        LightLevel::Full
                    } else {
                        LightLevel::Off
                    };
                    PadLight::new(0, level)
                })
                .collect();
            out.insert(g.id.clone(), lights);
        }
        out
    }

    /// Messages turning every LED off (on quit / disconnect) (`blackout`).
    pub fn blackout(&mut self) -> Vec<[u8; 3]> {
        let Some(leds) = self.profile.leds.clone() else {
            return Vec::new();
        };
        self.led_cache.clear();
        let mut out = Vec::new();
        for g in &self.profile.groups {
            if leds.rgb_groups.contains(&g.id) {
                out.extend(g.control_numbers().into_iter().map(|n| [0x90, n, 0]));
            } else if leds.single_groups.contains(&g.id) {
                out.extend(
                    g.control_numbers()
                        .into_iter()
                        .map(|n| [0x90 | g.channel.unwrap_or(0), n, 0]),
                );
            }
        }
        out
    }

    fn single_state(
        &self,
        b: Option<&Binding>,
        leds: &Leds,
        shift: Option<bool>,
        host: &dyn SurfaceHost,
    ) -> u8 {
        use DJActionKind as K;
        let Some(b) = b.filter(|b| b.action != K::Unbound) else {
            return leds.single_off;
        };
        let name = self.target(b, host);
        let d: Option<&dyn DeckView> = name.as_deref().and_then(|n| host.deck(n));
        let on_off = |c: bool| if c { leds.single_on } else { leds.single_off };
        match b.action {
            K::RoleMute => {
                on_off(d.map_or(false, |d| d.is_role_on(b.role.as_deref().unwrap_or(""))))
            }
            K::StemMute => {
                on_off(d.map_or(false, |d| d.is_stem_muted(b.role.as_deref().unwrap_or(""))))
            }
            K::SectionPrev | K::SectionNext => match d {
                Some(d) if d.mode() == DeckMode::Net && d.section_count() > 0 => leds.single_on,
                _ => leds.single_off,
            },
            K::SoundPrev | K::SoundNext => match d {
                Some(d) if d.mode() == DeckMode::Net => leds.single_on,
                _ => leds.single_off,
            },
            K::NextTrack => {
                if d.map_or(false, |d| d.next_ready()) {
                    leds.single_blink
                } else {
                    leds.single_off
                }
            }
            K::Quantize => on_off(d.map_or(false, |d| !d.quantize_off())),
            K::Pfl => on_off(d.map_or(false, |d| d.pfl())),
            K::Sync => {
                // lit while this deck runs at the other deck's tempo
                let Some(d) = d else { return leds.single_off };
                let Some(mine) = d.effective_bpm() else {
                    return leds.single_off;
                };
                let me = name.as_deref().unwrap_or("");
                let other = host
                    .deck_names()
                    .into_iter()
                    .find(|n| n != me)
                    .and_then(|n| host.deck(&n).and_then(|o| o.effective_bpm()));
                match other {
                    Some(o) if (mine - o).abs() < 0.05 => leds.single_on,
                    _ => leds.single_off,
                }
            }
            K::Play => {
                if d.map_or(false, |d| d.playing()) {
                    leds.single_blink
                } else {
                    leds.single_on
                }
            }
            K::Hotcue => on_off(d.map_or(false, |d| d.hot_cue_set(b.slot.unwrap_or(0) as usize))),
            K::LoopOut => on_off(d.map_or(false, |d| d.looping())),
            K::LoopIn => on_off(d.map_or(false, |d| d.loop_start_set())),
            K::FocusDeck => {
                let a = aim(b.slot);
                let focus = host.focus();
                if focus == a {
                    return leds.single_on;
                }
                // aimed at the loops: the deck aim buttons blink (SHIFT shows the loops'
                // own, lit)
                if focus == "L" && a != "L" {
                    leds.single_blink
                } else {
                    leds.single_off
                }
            }
            K::AutoNext => on_off(d.map_or(false, |d| d.auto_advance())),
            K::AutoNextBars => on_off(d.map_or(false, |d| d.bar_next_on())),
            K::ClipToggle => {
                // lit while its loop plays, blinking on its way in or out (dark: no loop)
                let Some(slot) = b.slot else {
                    return leds.single_off;
                };
                match host.clip_loop_state(slot as usize) {
                    Some(LoopState::On) => leds.single_on,
                    Some(LoopState::Starting) | Some(LoopState::Stopping) => leds.single_blink,
                    _ => leds.single_off,
                }
            }
            K::LoopRecord => {
                // lit while held, blinking while the last bar finishes
                match d.and_then(|d| d.loop_take()) {
                    None => leds.single_off,
                    Some(released) => {
                        if released {
                            leds.single_blink
                        } else {
                            leds.single_on
                        }
                    }
                }
            }
            K::Shift => on_off(shift.unwrap_or(self.shift_held)),
            K::DeckArm => on_off(name.as_deref().is_some_and(|n| self.armed.contains(n))),
            K::AutoMix => {
                // lit while on, blinking while a hand-off runs
                let (on, running) = host.auto_mix();
                if !on {
                    leds.single_off
                } else if running {
                    leds.single_blink
                } else {
                    leds.single_on
                }
            }
            K::Wander => {
                // lit while the focus deck wanders, blinking while the wheel holds it
                let f = host.focus();
                match host.deck(&f).map(|d| d.wander()) {
                    Some((true, paused)) => {
                        if paused {
                            leds.single_blink
                        } else {
                            leds.single_on
                        }
                    }
                    _ => leds.single_off,
                }
            }
            _ => leds.single_on,
        }
    }

    /// (palette colour, brightness) for a grid pad. Never `Off`: an unlit pad is a dim pad
    /// of colour 0, which is how the APC is told to go dark.
    fn rgb_state(&self, b: Option<&Binding>, host: &dyn SurfaceHost) -> (u8, LightLevel) {
        if let Some(b) = b {
            if b.action == DJActionKind::ClipToggle {
                return self.clip_state(b.slot, host);
            }
        }
        let (color, bright) = self.deck_pad_state(b, host);
        (
            color,
            if bright {
                LightLevel::Full
            } else {
                LightLevel::Dim
            },
        )
    }

    /// The loop page: violet full while the slot's loop plays, blinking on its way in or
    /// out, dark for a slot with no loop.
    fn clip_state(&self, slot: Option<i64>, host: &dyn SurfaceHost) -> (u8, LightLevel) {
        let Some(slot) = slot else {
            return (color::OFF, LightLevel::Dim);
        };
        match host.clip_loop_state(slot as usize) {
            Some(LoopState::On) => (color::VIOLET, LightLevel::Full),
            Some(LoopState::Starting) | Some(LoopState::Stopping) => {
                (color::VIOLET, LightLevel::Blink)
            }
            _ => (color::OFF, LightLevel::Dim),
        }
    }

    /// (palette colour, solid-bright?) for a pad bound to a deck.
    fn deck_pad_state(&self, b: Option<&Binding>, host: &dyn SurfaceHost) -> (u8, bool) {
        use DJActionKind as K;
        let Some(b) = b.filter(|b| b.action != K::Unbound) else {
            return (color::OFF, false);
        };
        let Some(name) = self.target(b, host) else {
            return (color::OFF, false);
        };
        let Some(d) = host.deck(&name) else {
            return (color::OFF, false);
        };
        // the FX unit's kill (red, bright while killed) and reset (white, bright while it
        // has something to reset), on whichever target is aimed (A, B or the loops)
        if b.action == K::FxKill {
            return (color::RED, d.fx_killed());
        }
        if b.action == K::FxReset {
            return (color::WHITE, !d.fx_neutral());
        }
        // the FX surface (aimed at a deck): beats' colour per macro group, lit while it runs
        if b.deck.as_deref() == Some("*") && matches!(b.action, K::Macro | K::Stinger) {
            let id = b.macro_id.as_deref().unwrap_or("");
            // the pan, shape and aux-FX macros: the FX holds play on any strip, the pan and
            // shape moves need a net deck's parts
            if let Some(m) = crate::fx::AuxMacro::from_id(id) {
                let hold = matches!(m.kind(), crate::fx::aux_macro::AuxKind::Hold { .. });
                return if hold || (!d.has_loop_bus() && d.mode() == DeckMode::Net) {
                    (aux_macro_color(id), d.aux_active(id))
                } else {
                    (color::OFF, false)
                };
            }
            if d.has_loop_bus() {
                // aimed at the loops: only the master macros play there
                return match macros::master_macro_group(b.macro_id.as_deref().unwrap_or("\0")) {
                    Some(_) => (master_macro_color(id), d.master_active(id)),
                    None => (color::OFF, false),
                };
            }
            if d.mode() == DeckMode::Net {
                if let Some(m) = NetMacro::from_id(id) {
                    let c = if m.is_tempo() {
                        color::YELLOW
                    } else {
                        color::RED
                    };
                    return (c, d.net_macro_active(id));
                }
            }
            // the master macros, in either mode: a sample deck plays them on its own bus
            if macros::master_macro_group(id).is_some() {
                return (master_macro_color(id), d.master_active(id));
            }
            // a hit (stinger); dark on a sample
            return (
                if d.mode() == DeckMode::Net {
                    color::WHITE
                } else {
                    color::OFF
                },
                false,
            );
        }
        let deck_color = if b.deck.as_deref() == Some("A") {
            color::ORANGE
        } else {
            color::CYAN
        };
        let slot = b.slot.unwrap_or(0);
        let is_set = d.hot_cue_set(slot as usize);
        match b.action {
            K::Section => {
                if (slot as usize) < d.section_count() && slot >= 0 {
                    (
                        deck_color,
                        d.current_section() == b.slot.map(|s| s as usize),
                    )
                } else {
                    (color::OFF, false)
                }
            }
            K::Macro | K::Panic | K::Stinger => {
                if d.mode() == DeckMode::Sample && !d.has_loop_bus() {
                    // a sample deck's bus: panic and the master macros work there, the
                    // rest are dark
                    if b.action == K::Panic {
                        return (color::RED, false);
                    }
                    if b.action == K::Macro {
                        let id = b.macro_id.as_deref().unwrap_or("");
                        if macros::master_macro_group(id).is_some() {
                            return (master_macro_color(id), d.master_active(id));
                        }
                    }
                    return (color::OFF, false);
                }
                if !(d.mode() == DeckMode::Net || (d.has_loop_bus() && b.action == K::Panic)) {
                    return (color::OFF, false);
                }
                (
                    if b.action == K::Panic {
                        color::RED
                    } else {
                        deck_color
                    },
                    false,
                )
            }
            K::Hotcue => (deck_color, is_set),
            K::Clearcue => (if is_set { color::RED } else { color::OFF }, false),
            _ => (deck_color, false),
        }
    }
}
