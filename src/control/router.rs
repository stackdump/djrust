//! `ControllerRouter.swift`: every attached controller, wired to one engine. Pure routing:
//! no MIDI backend, no threads, no wall clock. `crate::host::midi` feeds it from hardware (or
//! from a byte stream), a session replay feeds it from a recording.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::control::controller::{
    Action, AutoMixTouch, PadLight, SurfaceController, SurfaceHost, SurfaceOutcome,
};
use crate::control::jog::{JogAdapter, JogClock, JogEvent, JogProfile};
use crate::control::surface::{ControlEvent, ControlType, SurfaceProfile};

/// Where one incoming MIDI message went (`RouteResult`).
#[derive(Clone, Debug, PartialEq)]
pub enum RouteResult {
    Jog(JogEvent),
    Surface(ControlEvent, SurfaceOutcome),
    /// A surface port's message that its profile does not describe.
    Undecoded,
    /// No controller is attached to this port.
    Ignored,
}

/// What the router needs beyond [`SurfaceHost`]: the decks' jog clocks, and a place to
/// record what arrived (`dj.recorder?.record(midi:)` / `dj.journal?.record(.midi)`).
pub trait RouterHost: SurfaceHost {
    /// The clock a jog adapter for `deck` drives.
    fn jog_clock(&mut self, deck: &str) -> Option<&mut dyn JogClock>;
    /// A routed message, for the session recorder and the journal.
    fn record_midi(&mut self, _port: &str, _bytes: &[u8]) {}
    /// Whether the controller's SHIFT layer was held for the jog message about to be handled
    /// on `deck` (beyond Swift: a host can send a shifted wheel somewhere else).
    fn jog_shift(&mut self, _deck: &str, _shift: bool) {}
}

#[derive(Debug)]
pub struct ControllerRouter {
    pub jog_profile: Arc<JogProfile>,
    pub surface_profiles: Vec<Arc<SurfaceProfile>>,
    jog_adapters: Vec<JogAdapter>,
    surfaces: BTreeMap<String, SurfaceController>, // by port name
    /// The profiles as shipped, by name, once a user map has been laid over them.
    bases: BTreeMap<String, Arc<SurfaceProfile>>,
}

impl ControllerRouter {
    /// `decks`: the engine's deck names, sorted (`engine.decks.keys.sorted()`), one jog
    /// adapter each.
    pub fn new(
        decks: &[String],
        jog_profile: Arc<JogProfile>,
        surfaces: Vec<Arc<SurfaceProfile>>,
    ) -> ControllerRouter {
        let jog_adapters = decks
            .iter()
            .map(|d| JogAdapter::new(&jog_profile, d))
            .collect();
        ControllerRouter {
            jog_profile,
            surface_profiles: surfaces,
            jog_adapters,
            surfaces: BTreeMap::new(),
            bases: BTreeMap::new(),
        }
    }

    /// Lay `map` over the shipped profile named `device`, for every port that profile owns,
    /// now (attached controllers are rebuilt, so their LEDs are sent again). `None` puts the
    /// shipped bindings back. False if no profile has that name.
    pub fn apply_map(
        &mut self,
        device: &str,
        map: Option<&crate::control::map::UserMap>,
    ) -> bool {
        let Some(i) = self.surface_profiles.iter().position(|p| p.name == device) else {
            return false;
        };
        let base = self
            .bases
            .entry(device.to_string())
            .or_insert_with(|| self.surface_profiles[i].clone())
            .clone();
        let profile = match map {
            Some(m) => Arc::new(crate::control::map::overlay(&base, m)),
            None => base,
        };
        self.surface_profiles[i] = profile.clone();
        let ports: Vec<String> = self
            .surfaces
            .iter()
            .filter(|(_, c)| c.profile().name == device)
            .map(|(p, _)| p.clone())
            .collect();
        for port in ports {
            self.surfaces
                .insert(port, SurfaceController::new(profile.clone()));
        }
        true
    }

    /// Replace the group `group.id` of the profile named `device` (the learned pad notes), in
    /// the shipped base and the live profile, and rebuild the attached controllers. False if no
    /// profile has that name.
    pub fn set_group(&mut self, device: &str, group: crate::control::surface::Group) -> bool {
        let Some(i) = self.surface_profiles.iter().position(|p| p.name == device) else {
            return false;
        };
        let patch = |p: &Arc<SurfaceProfile>| {
            let mut q = (**p).clone();
            match q.groups.iter().position(|g| g.id == group.id) {
                Some(k) => q.groups[k] = group.clone(),
                None => q.groups.push(group.clone()),
            }
            Arc::new(q)
        };
        if let Some(b) = self.bases.get_mut(device) {
            *b = patch(b);
        }
        let profile = patch(&self.surface_profiles[i]);
        self.surface_profiles[i] = profile.clone();
        let ports: Vec<String> = self
            .surfaces
            .iter()
            .filter(|(_, c)| c.profile().name == device)
            .map(|(p, _)| p.clone())
            .collect();
        for port in ports {
            self.surfaces
                .insert(port, SurfaceController::new(profile.clone()));
        }
        true
    }

    /// Add a device (a generic profile made for a port nobody wrote one for). False when a
    /// profile with that name is there already.
    pub fn add_profile(&mut self, profile: Arc<SurfaceProfile>) -> bool {
        if self.surface_profiles.iter().any(|p| p.name == profile.name) {
            return false;
        }
        self.surface_profiles.push(profile);
        true
    }

    /// The profile as shipped (before any user map).
    pub fn base_profile(&self, device: &str) -> Option<Arc<SurfaceProfile>> {
        self.bases
            .get(device)
            .cloned()
            .or_else(|| self.surface_profiles.iter().find(|p| p.name == device).cloned())
    }

    /// The bundled profiles (`JogProfile.inpulse200MK3`, `SurfaceProfile.all`).
    pub fn with_defaults(decks: &[String]) -> ControllerRouter {
        ControllerRouter::new(
            decks,
            crate::control::profiles::inpulse_200_mk3_jog(),
            crate::control::profiles::all_surfaces(),
        )
    }

    pub fn jog_adapters(&self) -> &[JogAdapter] {
        &self.jog_adapters
    }

    pub fn surface(&self, port: &str) -> Option<&SurfaceController> {
        self.surfaces.get(port)
    }

    pub fn surface_mut(&mut self, port: &str) -> Option<&mut SurfaceController> {
        self.surfaces.get_mut(port)
    }

    pub fn attached_ports(&self) -> impl Iterator<Item = &str> {
        self.surfaces.keys().map(String::as_str)
    }

    /// The profile that handles a port, if any.
    pub fn profile_name(&self, port: &str) -> Option<String> {
        self.surface_profiles
            .iter()
            .find(|p| p.matches(port))
            .map(|p| p.name.clone())
            .or_else(|| {
                self.jog_profile
                    .matches(port)
                    .then(|| self.jog_profile.name.clone())
            })
    }

    /// Forget `port`'s surface (it was unplugged). Attaching it again starts afresh, so its
    /// LEDs are all sent again: a replugged device comes up dark otherwise.
    pub fn detach(&mut self, port: &str) -> bool {
        self.surfaces.remove(port).is_some()
    }

    /// Create the surface controller for `port` if a profile matches. True if the port is
    /// ours.
    pub fn attach(&mut self, port: &str) -> bool {
        if self.profile_name(port).is_none() {
            return false;
        }
        if !self.surfaces.contains_key(port) {
            if let Some(p) = self.surface_profiles.iter().find(|p| p.matches(port)) {
                self.surfaces
                    .insert(port.to_string(), SurfaceController::new(p.clone()));
            }
        }
        true
    }

    /// Forget surface controllers whose ports are gone.
    pub fn keep_only(&mut self, present: &[String]) {
        self.surfaces
            .retain(|port, _| present.iter().any(|p| p == port));
    }

    /// Route one message. The jog wheels get first look (the Inpulse shares its port with
    /// ordinary controls). Recorded through the host.
    pub fn route(
        &mut self,
        port: &str,
        bytes: &[u8],
        now: f64,
        host: &mut dyn RouterHost,
    ) -> RouteResult {
        if self.jog_profile.matches(port) {
            if let Some(e) = self.jog_profile.parse(bytes) {
                host.record_midi(port, bytes);
                let shift = match &e {
                    JogEvent::Touch { shift, .. }
                    | JogEvent::Scratch { shift, .. }
                    | JogEvent::Bend { shift, .. } => *shift,
                };
                host.jog_shift(e.deck(), shift);
                for a in &mut self.jog_adapters {
                    if let Some(clock) = host.jog_clock(&a.deck) {
                        a.handle(&e, clock);
                    }
                }
                // a hand on the platter (touch, scratch, ring nudge) steers its deck:
                // auto-mix pauses
                let touched = match &e {
                    JogEvent::Touch { down: true, .. }
                    | JogEvent::Scratch { .. }
                    | JogEvent::Bend { .. } => true,
                    JogEvent::Touch { .. } => false,
                };
                if touched {
                    host.perform(Action::AutoMixTouch {
                        deck: Some(e.deck().to_string()),
                        kind: AutoMixTouch::Steer,
                    });
                }
                return RouteResult::Jog(e);
            }
        }
        let Some(s) = self.surfaces.get_mut(port) else {
            return RouteResult::Ignored;
        };
        host.record_midi(port, bytes);
        match s.handle_bytes(bytes, now, host) {
            None => RouteResult::Undecoded,
            Some((e, outcome)) => RouteResult::Surface(e, outcome),
        }
    }

    /// Time-based housekeeping (jog-ring nudge decay). Call every ~10 ms.
    pub fn tick(&mut self, now: f64, host: &mut dyn RouterHost) {
        for a in &mut self.jog_adapters {
            if let Some(clock) = host.jog_clock(&a.deck) {
                a.decay(now, clock);
            }
        }
    }

    /// The lights of the attached surface whose profile is `profile_name`, with its port.
    /// `shift`: show that layer, not the held one.
    pub fn lights(
        &self,
        profile_name: &str,
        now: f64,
        shift: Option<bool>,
        host: &dyn SurfaceHost,
    ) -> Option<(String, BTreeMap<String, Vec<PadLight>>)> {
        let (port, s) = self
            .surfaces
            .iter()
            .find(|(_, s)| s.profile().name == profile_name)?;
        Some((port.clone(), s.lights(now, shift, host)))
    }

    /// Press and release a control on screen, as if it had been touched on the hardware:
    /// the same MIDI bytes through `route`, so it is recorded like any controller input.
    /// `shift`: with the surface's software SHIFT held around it (the APC's loop page);
    /// false when the surface has no SHIFT control.
    pub fn press(
        &mut self,
        port: &str,
        group: &str,
        index: usize,
        shift: bool,
        now: f64,
        host: &mut dyn RouterHost,
    ) -> bool {
        let Some(s) = self.surfaces.get(port) else {
            return false;
        };
        let profile = s.profile().clone();
        let Some((ch, note)) = note_control(&profile, group, index) else {
            return false;
        };
        let mut held = None;
        if shift {
            let Some(b) = profile.layout.iter().find(|b| {
                b.action == crate::control::surface::DJActionKind::Shift && b.index.is_some()
            }) else {
                return false;
            };
            let Some(sh) = note_control(&profile, &b.group, b.index.unwrap()) else {
                return false;
            };
            held = Some(sh);
            self.route(port, &[0x90 | sh.0, sh.1, 127], now, host);
        }
        self.route(port, &[0x90 | ch, note, 127], now, host);
        self.route(port, &[0x80 | ch, note, 0], now, host);
        if let Some((sc, sn)) = held {
            self.route(port, &[0x80 | sc, sn, 0], now, host);
        }
        true
    }

    /// LED updates for every attached surface, by port.
    pub fn led_frames(&mut self, now: f64, host: &dyn SurfaceHost) -> Vec<(String, Vec<[u8; 3]>)> {
        self.surfaces
            .iter_mut()
            .map(|(port, s)| (port.clone(), s.led_frame(now, host)))
            .collect()
    }

    pub fn blackouts(&mut self) -> Vec<(String, Vec<[u8; 3]>)> {
        self.surfaces
            .iter_mut()
            .map(|(port, s)| (port.clone(), s.blackout()))
            .collect()
    }

    /// The LED state of the attached controller with profile `profile_name`, for the device
    /// map; None if that controller is not attached or its profile has no LEDs
    /// (`deviceLights(profileName:)`).
    pub fn device_lights(
        &self,
        profile_name: &str,
        now: f64,
        host: &dyn SurfaceHost,
    ) -> Option<crate::control::leds::DeviceLights> {
        let leds = self
            .surface_profiles
            .iter()
            .find(|p| p.name == profile_name)?
            .leds
            .as_ref()?;
        let (_, groups) = self.lights(profile_name, now, None, host)?;
        Some(crate::control::leds::DeviceLights {
            groups,
            rgb_groups: leds.rgb_groups.iter().cloned().collect(),
        })
    }
}

/// (channel, note) of a note control, or None for anything else.
fn note_control(p: &SurfaceProfile, group: &str, index: usize) -> Option<(u8, u8)> {
    let g = p.group(group)?;
    if g.kind != ControlType::Note {
        return None;
    }
    let nums = g.control_numbers();
    nums.get(index).map(|n| (g.channel.unwrap_or(0), *n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{inject, profiles};

    #[test]
    fn a_generic_profile_added_owns_its_port_and_its_software_port() {
        let mut r = ControllerRouter::with_defaults(&["A".to_string(), "B".to_string()]);
        let g = Arc::new(profiles::generic("Launch Thing 3"));
        assert!(r.add_profile(g.clone()));
        assert!(!r.add_profile(g.clone()), "no second copy");
        assert_eq!(r.profile_name("Launch Thing 3 MIDI 1").as_deref(), Some(g.name.as_str()));
        let sw = inject::software_port(&g.name);
        assert_eq!(r.profile_name(&sw).as_deref(), Some(g.name.as_str()));
        assert!(r.attach(&sw));
        assert!(r.surface(&sw).is_some());
        // the shipped devices are not taken over by it
        assert_eq!(r.profile_name("APC mini mk2 Control").as_deref(), Some("APC mini mk2"));
    }
}
