//! Controllers on the host side, without a MIDI backend: what `ControllerHub.swift` does
//! once a message has arrived. Raw bytes are normalised the way CoreMIDI's UMP conversion
//! does, routed through [`crate::control::ControllerRouter`], and tallied for the
//! coverage report. Because nothing here touches a device, a recorded byte stream replays
//! exactly like live input (the `djctl decode` command, and the tests).
//!
//! The engine behind the controls is a [`DryHost`]: decks `A` and `B`, quiet, with focus
//! tracking. It exists to show what a message would *do* (the emitted `Action`s); the real
//! decks arrive with the wave engine (PORTING.md phase 3).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::control::controller::{Action, DeckView, SurfaceHost, SurfaceOutcome};
use crate::control::jog::{JogClock, JogProfile};
use crate::control::profiles;
use crate::control::router::{ControllerRouter, RouteResult, RouterHost};
use crate::control::surface::{DJActionKind, SurfaceProfile};

/// One MIDI 1.0 channel-voice message as the engine wants it: exactly three bytes
/// (`MIDIIn.midi1Bytes`). CoreMIDI hands the Swift app UMP words, whose message type 2
/// always carries two data bytes, so a program change (0xC0) or channel pressure (0xD0)
/// arrives padded with a zero. midir hands over raw bytes, so this pads them the same way.
/// System messages and sysex are dropped, as in Swift ("the jog wheel only sends channel
/// voice"), and data bytes are masked to 7 bits.
pub fn normalize_midi(bytes: &[u8]) -> Option<[u8; 3]> {
    let status = *bytes.first()?;
    if !(0x80..0xF0).contains(&status) {
        return None;
    }
    let two_byte = matches!(status & 0xF0, 0xC0 | 0xD0);
    let need = if two_byte { 2 } else { 3 };
    if bytes.len() < need {
        return None;
    }
    let d1 = bytes[1] & 0x7F;
    let d2 = if two_byte { 0 } else { bytes[2] & 0x7F };
    Some([status, d1, d2])
}

/// Split a byte stream into messages at each status byte (`djctl decode`).
pub fn split_messages(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for &b in bytes {
        if b >= 0x80 || out.is_empty() {
            out.push(Vec::new());
        }
        out.last_mut().unwrap().push(b);
    }
    out
}

/// A deck that does nothing and says nothing: every [`DeckView`] default.
pub struct QuietDeck;

impl DeckView for QuietDeck {}

/// The engine a monitor routes into: decks `A` and `B`, focus on `A`, every action
/// recorded and `Focus` applied.
#[derive(Default)]
pub struct DryHost {
    focus: String,
    owners: BTreeMap<String, String>,
    pub actions: Vec<Action>,
    /// `(port, bytes)` of every routed message, as the session recorder would see them.
    pub recorded: Vec<(String, Vec<u8>)>,
    clocks: BTreeMap<String, NullClock>,
    /// What the `keys` target does (docs/MPK.md): the engine's key route, here set by hand.
    pub keys: crate::control::controller::KeyMode,
    /// The deck the key route is aimed at (None: the aimed one).
    pub key_to: Option<String>,
}

#[derive(Default)]
struct NullClock;

impl JogClock for NullClock {
    fn freeze(&mut self) {}
    fn scrub_by(&mut self, _seconds: f64) {}
    fn release(&mut self) {}
    fn nudge(&mut self, _ratio: f64) {}
}

impl DryHost {
    pub const DECKS: [&'static str; 2] = ["A", "B"];

    pub fn new() -> DryHost {
        DryHost {
            focus: "A".into(),
            ..Default::default()
        }
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }
}

impl SurfaceHost for DryHost {
    fn deck(&self, name: &str) -> Option<&dyn DeckView> {
        Self::DECKS
            .contains(&name)
            .then_some(&QuietDeck as &dyn DeckView)
    }
    fn deck_names(&self) -> Vec<String> {
        Self::DECKS.iter().map(|d| d.to_string()).collect()
    }
    fn focus(&self) -> String {
        self.focus.clone()
    }
    fn fader_position(&self, _action: DJActionKind, _deck: Option<&str>) -> Option<f64> {
        None
    }
    fn owner(&self, param: &str) -> Option<String> {
        self.owners.get(param).cloned()
    }
    fn set_owner(&mut self, param: &str, owner: &str) {
        self.owners.insert(param.into(), owner.into());
    }
    fn key_mode(&self) -> crate::control::controller::KeyMode {
        self.keys
    }
    fn key_deck(&self) -> Option<String> {
        self.key_to.clone()
    }
    fn perform(&mut self, action: Action) {
        if let Action::Focus { deck } = &action {
            self.focus = deck.clone();
        }
        self.actions.push(action);
    }
}

impl RouterHost for DryHost {
    fn jog_clock(&mut self, deck: &str) -> Option<&mut dyn JogClock> {
        Self::DECKS
            .contains(&deck)
            .then(|| self.clocks.entry(deck.to_string()).or_default() as &mut dyn JogClock)
    }
    fn record_midi(&mut self, port: &str, bytes: &[u8]) {
        self.recorded.push((port.to_string(), bytes.to_vec()));
    }
}

/// `ControllerHub.Stats`: per-port tallies of what arrived, for the coverage report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub bound: usize,
    pub jog: usize,
    pub unbound: usize,
    pub unmapped: usize,
    pub undecoded: usize,
}

/// What `ControllerHub.route` reports for one message (`onMessage`'s text).
pub fn describe(result: &RouteResult) -> Option<String> {
    match result {
        RouteResult::Ignored => None,
        RouteResult::Jog(e) => Some(format!("{e:?}")),
        RouteResult::Undecoded => Some("(undecoded)".to_string()),
        RouteResult::Surface(e, outcome) => {
            let mut control = format!("{}[{}]", e.group, e.index);
            if let Some(n) = &e.name {
                control.push(' ');
                control.push_str(n);
            }
            if e.delta != 0 {
                control.push_str(&format!(" Δ{}", e.delta));
            } else {
                control.push_str(&format!("={:.3}", e.value));
            }
            Some(match outcome {
                SurfaceOutcome::Bound(b) => {
                    format!(
                        "{control} → {} {}",
                        b.action.raw(),
                        b.deck.as_deref().unwrap_or("")
                    )
                }
                SurfaceOutcome::Unbound { reason } => format!("{control} (unbound: {reason})"),
                SurfaceOutcome::Pickup(b) => format!(
                    "{control} → {} {} (soft takeover: move to current value)",
                    b.action.raw(),
                    b.deck.as_deref().unwrap_or("")
                ),
                SurfaceOutcome::Unmapped => format!("{control} (UNMAPPED — profile gap)"),
            })
        }
    }
}

/// One routed message.
#[derive(Debug, Clone, PartialEq)]
pub struct Routed {
    pub port: String,
    pub bytes: Vec<u8>,
    pub result: RouteResult,
    /// `describe(result)`; None for a port no profile owns.
    pub text: Option<String>,
    /// What the engine was asked to do about it.
    pub actions: Vec<Action>,
}

/// Every attached controller routed into a [`DryHost`], with the hub's bookkeeping: which
/// ports belong to a profile, per-port stats, LED frames. The replay path and the live path
/// share it; `crate::host::midi` only supplies the bytes.
pub struct ControllerSession {
    pub router: ControllerRouter,
    pub host: DryHost,
    pub stats: BTreeMap<String, Stats>,
    /// `(port, profile name)` of attached devices (`ControllerHub.devices`).
    pub devices: Vec<(String, String)>,
}

impl ControllerSession {
    pub fn new() -> ControllerSession {
        let host = DryHost::new();
        let router = ControllerRouter::with_defaults(&host.deck_names());
        ControllerSession {
            router,
            host,
            stats: BTreeMap::new(),
            devices: Vec::new(),
        }
    }

    /// Attach `port` if a profile owns it (`rescan`'s per-port step). True if it is ours.
    pub fn attach(&mut self, port: &str) -> bool {
        let Some(name) = self.router.profile_name(port) else {
            return false;
        };
        if !self.devices.iter().any(|(p, _)| p == port) {
            self.devices.push((port.to_string(), name));
        }
        self.router.attach(port);
        true
    }

    /// Forget ports that are gone.
    pub fn keep_only(&mut self, present: &[String]) {
        self.devices.retain(|(p, _)| present.contains(p));
        self.router.keep_only(present);
    }

    /// Route one raw message (normalised first). None if it is not channel-voice MIDI.
    pub fn route(&mut self, port: &str, bytes: &[u8], now: f64) -> Option<Routed> {
        let b = normalize_midi(bytes)?;
        let result = self.router.route(port, &b, now, &mut self.host);
        let actions = self.host.take_actions();
        let stat = self.stats.entry(port.to_string()).or_default();
        match &result {
            RouteResult::Ignored => {}
            RouteResult::Jog(_) => stat.jog += 1,
            RouteResult::Undecoded => stat.undecoded += 1,
            RouteResult::Surface(_, SurfaceOutcome::Bound(_) | SurfaceOutcome::Pickup(_)) => {
                stat.bound += 1
            }
            RouteResult::Surface(_, SurfaceOutcome::Unbound { .. }) => stat.unbound += 1,
            RouteResult::Surface(_, SurfaceOutcome::Unmapped) => stat.unmapped += 1,
        }
        let text = describe(&result);
        Some(Routed {
            port: port.to_string(),
            bytes: b.to_vec(),
            result,
            text,
            actions,
        })
    }

    /// Jog-ring decay etc. Call every ~10 ms.
    pub fn tick(&mut self, now: f64) {
        self.router.tick(now, &mut self.host);
    }

    /// Changed LED messages for every attached surface, by port.
    pub fn led_frames(&mut self, now: f64) -> Vec<(String, Vec<[u8; 3]>)> {
        self.router.led_frames(now, &self.host)
    }

    pub fn blackouts(&mut self) -> Vec<(String, Vec<[u8; 3]>)> {
        self.router.blackouts()
    }
}

impl Default for ControllerSession {
    fn default() -> Self {
        Self::new()
    }
}

/// A bundled surface profile by id or by any part of its name ("apc", "mpk",
/// "inpulse-200-mk3", "DJControl").
pub fn find_surface(key: &str) -> Option<Arc<SurfaceProfile>> {
    let k = key.to_lowercase();
    let ids = [
        ("apc-mini-mk2", profiles::apc_mini_mk2()),
        ("mpk-mini-mk2", profiles::mpk_mini_mk2()),
        ("inpulse-200-mk3", profiles::inpulse_200_mk3()),
    ];
    ids.into_iter()
        .find(|(id, p)| id.contains(&k) || p.name.to_lowercase().contains(&k) || k.contains(id))
        .map(|(_, p)| p)
}

/// The bundled jog profile (the Inpulse's wheels share its port).
pub fn jog_profile() -> Arc<JogProfile> {
    profiles::inpulse_200_mk3_jog()
}

/// Replay `(port, bytes)` pairs through a fresh session (the offline twin of a live run).
pub fn replay<'a, I>(messages: I) -> (ControllerSession, Vec<Routed>)
where
    I: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut s = ControllerSession::new();
    let mut out = Vec::new();
    for (i, (port, bytes)) in messages.into_iter().enumerate() {
        s.attach(port);
        if let Some(r) = s.route(port, bytes, i as f64 * 0.01) {
            out.push(r);
        }
    }
    (s, out)
}
