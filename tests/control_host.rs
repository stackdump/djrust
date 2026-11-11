//! The controller layer against a mock engine: ports of SurfaceControllerTests,
//! Phase1ActionTests, SurfaceNetTests, SurfaceBeatTests, BrowserSeedTests, PadLightsTests and
//! the fxValue / lock parts of the beats-btw-dj suite. Where Swift asserts on a `Deck`, these
//! assert on the emitted `Action` list (and on a mock that applies the simple ones).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use djrust::control::controller::{
    fx_value, Action, AutoMixTouch, DeckMode, DeckParam, DeckView, LightLevel, LoopState, PadLight,
    SurfaceController, SurfaceHost, SurfaceOutcome,
};
use djrust::control::jog::{JogClock, JogEvent};
use djrust::control::macros::{master_macro_group, FxParam};
use djrust::control::profiles::{apc_mini_mk2, inpulse_200_mk3, mpk_mini_mk2};
use djrust::control::router::{ControllerRouter, RouteResult, RouterHost};
use djrust::control::surface::{DJActionKind, SurfaceProfile};

#[derive(Clone, Default)]
struct MockDeck {
    net: bool,
    playing: bool,
    hot_cues: BTreeSet<usize>,
    looping: bool,
    loop_start: bool,
    pfl: bool,
    quantized: bool,
    auto_advance: bool,
    bpm: Option<f64>,
    sections: usize,
    current_section: Option<usize>,
    roles_off: BTreeSet<String>,
    stems_muted: BTreeSet<String>,
    next_ready: bool,
    has_spec: bool,
    killed: bool,
    fx: BTreeMap<FxParam, f64>,
    loop_bus: bool,
    active_net: BTreeSet<String>,
    active_master: BTreeSet<String>,
    active_aux: BTreeSet<String>,
    wander: (bool, bool),
    loop_take: Option<bool>,
    vals: BTreeMap<u8, f64>,
}

fn pk(p: DeckParam) -> u8 {
    p as u8
}

impl DeckView for MockDeck {
    fn mode(&self) -> DeckMode {
        if self.net {
            DeckMode::Net
        } else {
            DeckMode::Sample
        }
    }
    fn playing(&self) -> bool {
        self.playing
    }
    fn hot_cue_set(&self, slot: usize) -> bool {
        self.hot_cues.contains(&slot)
    }
    fn looping(&self) -> bool {
        self.looping
    }
    fn loop_start_set(&self) -> bool {
        self.loop_start
    }
    fn pfl(&self) -> bool {
        self.pfl
    }
    fn quantize_off(&self) -> bool {
        !self.quantized
    }
    fn auto_advance(&self) -> bool {
        self.auto_advance
    }
    fn effective_bpm(&self) -> Option<f64> {
        self.bpm
    }
    fn section_count(&self) -> usize {
        self.sections
    }
    fn current_section(&self) -> Option<usize> {
        self.current_section
    }
    fn is_role_on(&self, role: &str) -> bool {
        !self.roles_off.contains(role)
    }
    fn is_stem_muted(&self, role: &str) -> bool {
        self.stems_muted.contains(role)
    }
    fn next_ready(&self) -> bool {
        self.next_ready
    }
    fn has_spec(&self) -> bool {
        self.has_spec
    }
    fn fx_killed(&self) -> bool {
        self.killed
    }
    fn fx_neutral(&self) -> bool {
        !self.killed && self.fx.iter().all(|(p, v)| *v == p.neutral())
    }
    fn has_loop_bus(&self) -> bool {
        self.loop_bus
    }
    fn net_macro_active(&self, id: &str) -> bool {
        self.active_net.contains(id)
    }
    fn master_active(&self, id: &str) -> bool {
        self.active_master.contains(id)
    }
    fn aux_active(&self, id: &str) -> bool {
        self.active_aux.contains(id)
    }
    fn wander(&self) -> (bool, bool) {
        self.wander
    }
    fn loop_take(&self) -> Option<bool> {
        self.loop_take
    }
}

#[derive(Default)]
struct Clock {
    calls: Vec<String>,
}

impl JogClock for Clock {
    fn freeze(&mut self) {
        self.calls.push("freeze".into());
    }
    fn scrub_by(&mut self, s: f64) {
        self.calls.push(format!("scrub {s:.6}"));
    }
    fn release(&mut self) {
        self.calls.push("release".into());
    }
    fn nudge(&mut self, r: f64) {
        self.calls.push(format!("nudge {r:.4}"));
    }
}

struct Mock {
    decks: BTreeMap<String, MockDeck>,
    focus: String,
    owners: BTreeMap<String, String>,
    crossfader: f64,
    master: f64,
    headphone_mix: f64,
    headphone_level: f64,
    clips: Vec<LoopState>,
    auto_mix: (bool, bool),
    locked: BTreeSet<String>,
    actions: Vec<Action>,
    recorded: Vec<(String, Vec<u8>)>,
    clocks: BTreeMap<String, Clock>,
}

impl Mock {
    /// A/B sample decks (`loadDemo`), focus A.
    fn new() -> Mock {
        let mut decks = BTreeMap::new();
        for n in ["A", "B"] {
            decks.insert(n.to_string(), MockDeck::default());
        }
        let mut m = Mock {
            decks,
            focus: "A".into(),
            owners: BTreeMap::new(),
            crossfader: 0.5,
            master: 1.0,
            headphone_mix: 0.5,
            headphone_level: 0.5,
            clips: Vec::new(),
            auto_mix: (false, false),
            locked: BTreeSet::new(),
            actions: Vec::new(),
            recorded: Vec::new(),
            clocks: BTreeMap::new(),
        };
        for d in m.decks.values_mut() {
            d.vals.insert(pk(DeckParam::Volume), 1.0);
            d.vals.insert(pk(DeckParam::Pitch), 0.0);
        }
        m
    }

    fn with_loops(mut self) -> Mock {
        let mut l = MockDeck::default();
        l.loop_bus = true;
        self.decks.insert("L".into(), l);
        self
    }

    fn dk(&mut self, n: &str) -> &mut MockDeck {
        self.decks.get_mut(n).unwrap()
    }

    fn take(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    fn val(&self, deck: &str, p: DeckParam) -> f64 {
        self.decks[deck]
            .vals
            .get(&pk(p))
            .copied()
            .unwrap_or(f64::NAN)
    }
}

impl SurfaceHost for Mock {
    fn deck(&self, name: &str) -> Option<&dyn DeckView> {
        self.decks.get(name).map(|d| d as &dyn DeckView)
    }
    fn deck_names(&self) -> Vec<String> {
        self.decks.keys().cloned().collect()
    }
    fn focus(&self) -> String {
        self.focus.clone()
    }
    fn is_locked(&self, key: &str) -> bool {
        self.locked.contains(key)
    }
    fn fader_position(&self, action: DJActionKind, deck: Option<&str>) -> Option<f64> {
        let d = deck.and_then(|d| self.decks.get(d));
        let v = |p: DeckParam| d.and_then(|d| d.vals.get(&pk(p)).copied());
        match action {
            DJActionKind::Crossfader => Some(self.crossfader),
            DJActionKind::Master => Some(self.master),
            DJActionKind::HeadphoneMix => Some(self.headphone_mix),
            DJActionKind::HeadphoneLevel => Some(self.headphone_level),
            DJActionKind::Volume => v(DeckParam::Volume),
            DJActionKind::Filter => v(DeckParam::Filter),
            DJActionKind::Gain => v(DeckParam::Gain),
            DJActionKind::EqLow => v(DeckParam::EqLow),
            DJActionKind::EqMid => v(DeckParam::EqMid),
            DJActionKind::EqHigh => v(DeckParam::EqHigh),
            DJActionKind::Pitch => v(DeckParam::Pitch).map(|p| (p + 1.0) / 2.0),
            _ => None,
        }
    }
    fn owner(&self, param: &str) -> Option<String> {
        self.owners.get(param).cloned()
    }
    fn set_owner(&mut self, param: &str, owner: &str) {
        self.owners.insert(param.into(), owner.into());
    }
    fn clip_count(&self) -> usize {
        self.clips.len()
    }
    fn clip_loop_state(&self, slot: usize) -> Option<LoopState> {
        self.clips.get(slot).copied()
    }
    fn library_count(&self) -> usize {
        2
    }
    fn auto_mix(&self) -> (bool, bool) {
        self.auto_mix
    }
    fn perform(&mut self, action: Action) {
        // apply what the tests read back, as the engine would
        match &action {
            Action::Deck { deck, param, value } => {
                self.dk(deck).vals.insert(pk(*param), *value);
            }
            Action::Crossfader(v) => self.crossfader = *v,
            Action::Master(v) => self.master = *v,
            Action::HeadphoneMix(v) => self.headphone_mix = *v,
            Action::HeadphoneLevel(v) => self.headphone_level = *v,
            Action::Focus { deck } => self.focus = deck.clone(),
            Action::PlayToggle { deck } => {
                let d = self.dk(deck);
                d.playing = !d.playing;
            }
            Action::AutoNextToggle { deck } => {
                let d = self.dk(deck);
                d.auto_advance = !d.auto_advance;
            }
            Action::HotCue { deck, slot } => {
                self.dk(deck).hot_cues.insert(*slot as usize);
            }
            Action::ClearCue { deck, slot } => {
                self.dk(deck).hot_cues.remove(&(*slot as usize));
            }
            Action::ToggleRole { deck, role } => {
                let d = self.dk(deck);
                if !d.roles_off.remove(role) {
                    d.roles_off.insert(role.clone());
                }
            }
            Action::ToggleStem { deck, role } => {
                let d = self.dk(deck);
                if !d.stems_muted.remove(role) {
                    d.stems_muted.insert(role.clone());
                }
            }
            Action::Macro { deck, id } => {
                let d = self.dk(deck);
                if master_macro_group(id).is_some() {
                    d.active_master.insert(id.clone());
                } else if djrust::control::macros::aux_macro_group(id).is_some() {
                    d.active_aux.insert(id.clone());
                } else {
                    d.active_net.insert(id.clone());
                }
            }
            Action::Panic { deck } => {
                let d = self.dk(deck);
                d.active_master.clear();
                d.active_aux.clear();
                d.active_net.clear();
            }
            Action::FxUnit { deck, param, value } => {
                self.dk(deck).fx.insert(*param, *value);
            }
            Action::FxKill { deck, on, .. } => self.dk(deck).killed = *on,
            Action::FxReset { deck, .. } => {
                let d = self.dk(deck);
                d.fx.clear();
                d.killed = false;
            }
            Action::ClipToggle { slot, on } => {
                self.clips[*slot] = if *on { LoopState::On } else { LoopState::Off };
            }
            _ => {}
        }
        self.actions.push(action);
    }
}

impl RouterHost for Mock {
    fn jog_clock(&mut self, deck: &str) -> Option<&mut dyn JogClock> {
        Some(self.clocks.entry(deck.to_string()).or_default() as &mut dyn JogClock)
    }
    fn record_midi(&mut self, port: &str, bytes: &[u8]) {
        self.recorded.push((port.to_string(), bytes.to_vec()));
    }
}

fn controller(p: Arc<SurfaceProfile>) -> SurfaceController {
    SurfaceController::new(p)
}

fn send(
    s: &mut SurfaceController,
    m: &mut Mock,
    bytes: &[u8],
) -> Option<(djrust::control::ControlEvent, SurfaceOutcome)> {
    send_at(s, m, bytes, 0.0)
}

fn send_at(
    s: &mut SurfaceController,
    m: &mut Mock,
    bytes: &[u8],
    now: f64,
) -> Option<(djrust::control::ControlEvent, SurfaceOutcome)> {
    s.handle_bytes(bytes, now, m)
}

fn has(frame: &[[u8; 3]], m: [u8; 3]) -> bool {
    frame.contains(&m)
}

fn s(x: &str) -> String {
    x.to_string()
}

// MARK: SurfaceControllerTests

#[test]
fn faders_drive_the_mixer() {
    // The mixer lives on the Inpulse (the APC is an FX surface): 14-bit faders, MSB then LSB.
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0xB1, 0x00, 0]); // deck A volume
    send(&mut c, &mut m, &[0xB1, 0x20, 0]);
    send(&mut c, &mut m, &[0xB2, 0x00, 127]); // deck B volume
    send(&mut c, &mut m, &[0xB2, 0x20, 127]);
    send(&mut c, &mut m, &[0xB0, 0x00, 127]); // crossfader hard B
    send(&mut c, &mut m, &[0xB0, 0x20, 127]);
    assert_eq!(m.val("A", DeckParam::Volume), 0.0);
    assert_eq!(m.val("B", DeckParam::Volume), 1.0);
    assert_eq!(m.crossfader, 1.0);
    // and the APC's master fader is the master
    let mut apc = controller(apc_mini_mk2());
    send(&mut apc, &mut m, &[0xB0, 0x38, 64]);
    assert!((m.master - 64.0 / 127.0).abs() < 1e-6);
}

#[test]
fn pitch_fader_has_centre_detent() {
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0xB1, 0x08, 64]); // 64/127 is just off centre
    send(&mut c, &mut m, &[0xB1, 0x28, 0]);
    assert_eq!(m.val("A", DeckParam::Pitch), 0.0);
    send(&mut c, &mut m, &[0xB1, 0x08, 127]);
    send(&mut c, &mut m, &[0xB1, 0x28, 127]);
    assert_eq!(m.val("A", DeckParam::Pitch), 1.0);
}

#[test]
fn filter_sweeps_low_and_high_pass() {
    // the EQ bands are the engine's; the control's contract is the 0…1 value it sends
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    let mut filter = |m: &mut Mock, v: u8| {
        send(&mut c, m, &[0xB1, 0x01, v]);
        send(&mut c, m, &[0xB1, 0x21, if v == 127 { 127 } else { 0 }]);
        m.val("A", DeckParam::Filter)
    };
    assert_eq!(filter(&mut m, 0), 0.0);
    assert!((filter(&mut m, 64) - 0.5).abs() < 0.01);
    assert_eq!(filter(&mut m, 127), 1.0);
}

#[test]
fn play_toggles_on_press_only() {
    let mut m = Mock::new();
    let mut c = controller(apc_mini_mk2());
    send(&mut c, &mut m, &[0x90, 0x70, 0x7F]);
    send(&mut c, &mut m, &[0x90, 0x70, 0x00]); // release does nothing
    assert!(m.decks["A"].playing);
    send(&mut c, &mut m, &[0x90, 0x70, 0x7F]);
    assert!(!m.decks["A"].playing);
}

#[test]
fn hot_cue_sets_then_clears() {
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0x96, 0x00, 0x7F]); // deck A pad 1 (HOT CUE mode)
    assert!(m.take().contains(&Action::HotCue {
        deck: s("A"),
        slot: 0
    }));
    send(&mut c, &mut m, &[0x96, 0x08, 0x7F]); // SHIFT + pad 1: clear it
    assert!(m.take().contains(&Action::ClearCue {
        deck: s("A"),
        slot: 0
    }));
}

#[test]
fn leds_reflect_state_and_only_send_changes() {
    let mut m = Mock::new();
    let mut c = controller(apc_mini_mk2());
    let first = c.led_frame(100.0, &m);
    assert_eq!(first.len(), 64 + 8 + 8); // every LED once
    assert!(c.led_frame(100.0, &m).is_empty()); // nothing changed
    assert!(
        has(&first, [0x92, 0, 0]),
        "a macro pad is dark while the aimed deck plays a sample"
    );
    assert!(has(&first, [0x92, 63, 0])); // unbound pad: off
    assert!(has(&first, [0x90, 0x70, 1])); // play (the aimed deck): on
    assert!(
        has(&first, [0x90, 0x73, 1]) && has(&first, [0x90, 0x74, 0]),
        "aimed at A"
    );
    send_at(&mut c, &mut m, &[0x90, 0x74, 0x7F], 100.0); // aim at B
    let next = c.led_frame(100.01, &m);
    assert!(has(&next, [0x90, 0x73, 0]) && has(&next, [0x90, 0x74, 1]));
    assert_eq!(m.focus, "B");
    assert_eq!(c.blackout().len(), 80);
}

/// A sample deck runs the master macros on its own bus, so the APC's master-macro pads light
/// for it as for a net deck: in their group's colour, full while one runs. Net macros stay
/// dark.
#[test]
fn master_macro_leds_light_on_a_sample_deck() {
    let mut m = Mock::new();
    let mut c = controller(apc_mini_mk2());
    let leds = c.profile().leds.clone().unwrap();
    let cyan = djrust::control::controller::master_macro_color("sweep-lp");
    assert_eq!(cyan, 37);
    let first = c.led_frame(100.0, &m);
    assert!(
        has(&first, [leds.rgb_dim_status, 8, cyan]),
        "sweep-lp (pad 8): lit dim, idle"
    );
    assert!(
        has(
            &first,
            [
                leds.rgb_dim_status,
                35,
                djrust::control::controller::master_macro_color("vinyl-brake")
            ]
        ),
        "vinyl-brake (pad 35): lit dim, idle"
    );
    assert!(
        has(&first, [leds.rgb_dim_status, 0, 0]),
        "a net macro pad: dark"
    );
    send_at(&mut c, &mut m, &[0x90, 8, 0x7F], 100.0); // press sweep-lp on the aimed (sample) deck
    assert!(m.take().contains(&Action::Macro {
        deck: s("A"),
        id: s("sweep-lp")
    }));
    assert!(m.decks["A"].active_master.contains("sweep-lp"));
    let next = c.led_frame(101.0, &m); // past the press flash
    assert!(
        has(&next, [leds.rgb_full_status, 8, cyan]),
        "sweep-lp: full while it runs"
    );
}

#[test]
fn mpk_has_no_leds() {
    let m = Mock::new();
    let mut c = controller(mpk_mini_mk2());
    assert!(c.led_frame(0.0, &m).is_empty());
}

// MARK: Phase1ActionTests

#[test]
fn inpulse_transport_cue_and_loop_buttons() {
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0x91, 0x06, 0x7F]); // CUE
    send(&mut c, &mut m, &[0x91, 0x07, 0x7F]); // PLAY
    let a = m.take();
    assert!(
        a.contains(&Action::Cue { deck: s("A") })
            && a.contains(&Action::PlayToggle { deck: s("A") })
    );
    // LOOP (0x03) is LOOP record; STEMS music / mic (0x09 / 0x0A) mute role groups.
    let binding = |c: &mut SurfaceController, b: &[u8]| {
        let e = c.decode(b).unwrap();
        c.binding_for(&e).cloned()
    };
    assert_eq!(
        binding(&mut c, &[0x91, 0x03, 0x7F]).map(|b| b.action),
        Some(DJActionKind::LoopRecord)
    );
    assert_eq!(
        binding(&mut c, &[0x92, 0x03, 0x7F]).and_then(|b| b.deck),
        Some(s("B"))
    );
    let stems = binding(&mut c, &[0x91, 0x09, 0x7F]).unwrap();
    assert_eq!(
        (stems.action, stems.role.as_deref()),
        (DJActionKind::StemMute, Some("music"))
    );
    assert_eq!(
        binding(&mut c, &[0x91, 0x0A, 0x7F]).and_then(|b| b.role),
        Some(s("mic"))
    );
    send(&mut c, &mut m, &[0x92, 0x07, 0x7F]); // deck B PLAY
    assert!(m.decks["B"].playing);
}

#[test]
fn inpulse_knobs_drive_gain_and_eq() {
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0xB5, 0x05, 0x7F]); // SHIFT + HIGH/GAIN = gain → full
    send(&mut c, &mut m, &[0xB5, 0x25, 0x7F]);
    assert_eq!(m.val("B", DeckParam::Gain), 1.0);
    send(&mut c, &mut m, &[0xB2, 0x02, 0x00]); // EQ low → kill
    send(&mut c, &mut m, &[0xB2, 0x22, 0x00]);
    assert_eq!(m.val("B", DeckParam::EqLow), 0.0);
    send(&mut c, &mut m, &[0xB2, 0x05, 0x40]); // HIGH → centre
    send(&mut c, &mut m, &[0xB2, 0x25, 0x00]);
    assert!((m.val("B", DeckParam::EqHigh) - 0.5).abs() < 0.01);
    send(&mut c, &mut m, &[0xB2, 0x04, 0x7F]); // MID → +6 dB
    send(&mut c, &mut m, &[0xB2, 0x24, 0x7F]);
    assert_eq!(m.val("B", DeckParam::EqMid), 1.0);
}

#[test]
fn apc_shift_is_held_in_software() {
    let mut m = Mock::new();
    let mut c = controller(apc_mini_mk2());
    assert!(!c.shift_held());
    send(&mut c, &mut m, &[0x90, 0x7A, 0x7F]);
    assert!(c.shift_held());
    send(&mut c, &mut m, &[0x80, 0x7A, 0x00]);
    assert!(!c.shift_held());
}

#[test]
fn soft_takeover_between_devices() {
    let mut m = Mock::new();
    let mut ip = controller(inpulse_200_mk3());
    let mut mpk = controller(mpk_mini_mk2());
    let fader = |ip: &mut SurfaceController, m: &mut Mock, v: u8| {
        // the Inpulse's deck A volume, 14-bit
        send(ip, m, &[0xB1, 0x00, v]);
        send(ip, m, &[0xB1, 0x20, 0]);
    };
    fader(&mut ip, &mut m, 0);
    assert_eq!(m.val("A", DeckParam::Volume), 0.0);
    // MPK knob 2 (A volume) is physically at max: held back, not a jump.
    let (e, out) = send(&mut mpk, &mut m, &[0xB0, 2, 127]).unwrap();
    let b = mpk.binding_for(&e).unwrap().clone();
    assert_eq!(out, SurfaceOutcome::Pickup(b));
    assert_eq!(m.val("A", DeckParam::Volume), 0.0);
    // Turning it down through the current value picks it up.
    send(&mut mpk, &mut m, &[0xB0, 2, 60]);
    assert_eq!(m.val("A", DeckParam::Volume), 0.0);
    send(&mut mpk, &mut m, &[0xB0, 2, 2]); // within the window
    assert!((m.val("A", DeckParam::Volume) - 2.0 / 127.0).abs() < 1e-9);
    // The MPK now owns it and turns it up to ~0.79.
    send(&mut mpk, &mut m, &[0xB0, 2, 100]);
    assert!((m.val("A", DeckParam::Volume) - 100.0 / 127.0).abs() < 1e-9);
    // The Inpulse fader is still physically near the bottom: held back.
    fader(&mut ip, &mut m, 1);
    assert!((m.val("A", DeckParam::Volume) - 100.0 / 127.0).abs() < 1e-9);
    // Pushing it up past the current value picks it up.
    fader(&mut ip, &mut m, 110);
    assert!((m.val("A", DeckParam::Volume) - 110.0 / 127.0).abs() < 0.01);
    assert_eq!(
        m.owners.get("A.volume").map(String::as_str),
        Some("DJControl Inpulse 200 Mk3/deckA.faders#0")
    );
}

// MARK: BrowserSeedTests

#[test]
fn the_browser_wheel_steps_the_seed_one_click_at_a_time() {
    let mut m = Mock::new();
    // a playing composed net deck: the wheel has a deck to move
    m.dk("A").net = true;
    m.dk("A").playing = true;
    m.dk("A").has_spec = true;
    let mut c = controller(inpulse_200_mk3());
    send(&mut c, &mut m, &[0xB0, 0x01, 0x01]); // clockwise
    send(&mut c, &mut m, &[0xB0, 0x01, 0x7F]); // counter-clockwise
    let a = m.take();
    assert!(
        a.contains(&Action::SeedStep { delta: 1, decks: vec![] })
            && a.contains(&Action::SeedStep { delta: -1, decks: vec![] }),
        "{a:?}"
    );
    // a steering poke on the deck the step moves: auto-mix pauses
    assert!(a.contains(&Action::AutoMixTouch {
        deck: Some(s("A")),
        kind: AutoMixTouch::Steer
    }));
}

/// The seed wheel's auto-mix touch names the deck the step would move, and there is none when
/// nothing would move (`SurfaceController.noteAutoMixTouch`, `.seedStep`: the playing net decks
/// with a `currentSpec`, the focus deck among them, else the only one; `if let d`). Swift has no
/// test of it, so these are literals read off that code.
#[test]
fn the_seed_wheels_touch_names_the_deck_the_step_moves() {
    let wheel = |m: &mut Mock| {
        let mut c = controller(inpulse_200_mk3());
        send(&mut c, m, &[0xB0, 0x01, 0x01]);
        m.take()
    };
    let touches = |a: &[Action]| -> Vec<Option<String>> {
        a.iter()
            .filter_map(|x| match x {
                Action::AutoMixTouch { deck, kind: AutoMixTouch::Steer } => Some(deck.clone()),
                _ => None,
            })
            .collect()
    };
    let compose = |m: &mut Mock, n: &str, playing: bool| {
        let d = m.dk(n);
        d.net = true;
        d.has_spec = true;
        d.playing = playing;
    };

    // only a sample deck is playing: the wheel moves nothing, so auto-mix is not touched
    let mut m = Mock::new();
    m.dk("A").playing = true;
    let a = wheel(&mut m);
    assert!(a.contains(&Action::SeedStep { delta: 1, decks: vec![] }), "the step is still asked for: {a:?}");
    assert!(touches(&a).is_empty(), "{a:?}");

    // no deck is playing at all
    let mut m = Mock::new();
    assert!(touches(&wheel(&mut m)).is_empty());

    // a composed net that is not playing, and a playing net with no spec (a raw-nets share)
    let mut m = Mock::new();
    compose(&mut m, "A", false);
    m.dk("B").net = true;
    m.dk("B").playing = true;
    assert!(touches(&wheel(&mut m)).is_empty());

    // one playing composed net, focus on the other deck: it is the only one, so it is touched
    let mut m = Mock::new();
    compose(&mut m, "B", true);
    m.focus = "A".into();
    assert_eq!(touches(&wheel(&mut m)), [Some(s("B"))]);

    // two playing composed nets: the focus deck's
    for focus in ["A", "B"] {
        let mut m = Mock::new();
        compose(&mut m, "A", true);
        compose(&mut m, "B", true);
        m.focus = focus.into();
        assert_eq!(touches(&wheel(&mut m)), [Some(s(focus))], "focus {focus}");
    }

    // two playing composed nets and the focus on neither (the loop channel): no touch
    let mut m = Mock::new().with_loops();
    compose(&mut m, "A", true);
    compose(&mut m, "B", true);
    m.focus = "L".into();
    assert!(touches(&wheel(&mut m)).is_empty());
}

// MARK: SurfaceNetTests

/// Each deck's SHIFT latches that deck in or out for the browser wheel, lit while armed; a
/// SHIFT held for a chord on its hardware layer leaves the arm as it was.
#[test]
fn inpulse_shift_arms_decks_for_the_browser_wheel() {
    let mut m = Mock::new();
    let mut c = controller(inpulse_200_mk3());
    let wheel = |c: &mut SurfaceController, m: &mut Mock| {
        send(c, m, &[0xB0, 0x01, 0x01]);
        m.take()
            .into_iter()
            .find_map(|a| match a {
                Action::SeedStep { decks, .. } => Some(decks),
                _ => None,
            })
            .expect("a seed step")
    };
    // none armed: the host picks, as before
    assert_eq!(wheel(&mut c, &mut m), Vec::<String>::new());
    // tap deck B's SHIFT: B armed, its SHIFT lit
    send(&mut c, &mut m, &[0x92, 0x04, 0x7F]);
    send(&mut c, &mut m, &[0x92, 0x04, 0x00]);
    assert_eq!(c.armed(), [s("B")]);
    assert_eq!(wheel(&mut c, &mut m), [s("B")]);
    let f = c.led_frame(0.0, &m);
    assert!(has(&f, [0x92, 0x04, 127]), "{f:?}");
    assert!(has(&f, [0x91, 0x04, 0]), "{f:?}");
    // tap A too: both turn together
    send(&mut c, &mut m, &[0x91, 0x04, 0x7F]);
    send(&mut c, &mut m, &[0x91, 0x04, 0x00]);
    assert_eq!(wheel(&mut c, &mut m), [s("A"), s("B")]);
    assert!(has(&c.led_frame(0.1, &m), [0x91, 0x04, 127]));
    // SHIFT held for SHIFT + SYNC (a chord on the hardware layer): A stays armed
    send(&mut c, &mut m, &[0x91, 0x04, 0x7F]);
    send(&mut c, &mut m, &[0x94, 0x05, 0x7F]);
    send(&mut c, &mut m, &[0x94, 0x05, 0x00]);
    send(&mut c, &mut m, &[0x91, 0x04, 0x00]);
    assert_eq!(c.armed(), [s("A"), s("B")]);
    // tap A again: off, its LED out; B alone
    send(&mut c, &mut m, &[0x91, 0x04, 0x7F]);
    send(&mut c, &mut m, &[0x91, 0x04, 0x00]);
    assert_eq!(wheel(&mut c, &mut m), [s("B")]);
    assert!(has(&c.led_frame(0.2, &m), [0x91, 0x04, 0]));
    // and B: none armed, the old rule again
    send(&mut c, &mut m, &[0x92, 0x04, 0x7F]);
    send(&mut c, &mut m, &[0x92, 0x04, 0x00]);
    assert_eq!(wheel(&mut c, &mut m), Vec::<String>::new());
}

#[test]
fn apc_track_buttons_mute_the_aimed_decks_parts() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(apc_mini_mk2());
    // track button 1 = kick, on the aimed deck (A to start with)
    send(&mut c, &mut m, &[0x90, 100, 0x7F]);
    assert!(m.take().contains(&Action::ToggleRole {
        deck: s("A"),
        role: s("kick")
    }));
    assert!(!m.decks["A"].roles_off.is_empty());
    // aim at B (scene 5), press again
    send(&mut c, &mut m, &[0x90, 0x74, 0x7F]);
    send(&mut c, &mut m, &[0x90, 100, 0x7F]);
    assert!(m.take().contains(&Action::ToggleRole {
        deck: s("B"),
        role: s("kick")
    }));
    assert_eq!(m.focus, "B");
}

#[test]
fn inpulse_stems_buttons_mute_the_music_or_the_lead() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(inpulse_200_mk3());
    let _ = c.led_frame(0.0, &m);
    // STEMS music (note icon, 0x09): everything but the drums goes, and its LED lights
    send(&mut c, &mut m, &[0x91, 0x09, 0x7F]);
    send(&mut c, &mut m, &[0x91, 0x09, 0x00]);
    assert!(m.take().contains(&Action::ToggleStem {
        deck: s("A"),
        role: s("music")
    }));
    assert!(
        m.decks["A"].stems_muted.contains("music") && !m.decks["A"].stems_muted.contains("mic")
    );
    assert!(has(&c.led_frame(0.0, &m), [0x91, 0x09, 0x7F]));
    // STEMS mic (0x0A) on top, then music back: the lead stays out until mic is pressed again
    send(&mut c, &mut m, &[0x91, 0x0A, 0x7F]);
    send(&mut c, &mut m, &[0x91, 0x09, 0x7F]);
    assert!(
        !m.decks["A"].stems_muted.contains("music") && m.decks["A"].stems_muted.contains("mic")
    );
    send(&mut c, &mut m, &[0x91, 0x0A, 0x7F]);
    assert!(m.decks["A"].stems_muted.is_empty());
    // only changes go out: mic's LED went on and off between frames, music's went off
    assert_eq!(c.led_frame(0.0, &m), vec![[0x91, 0x09, 0x00]]);
    // deck B's buttons are deck B's
    send(&mut c, &mut m, &[0x92, 0x0A, 0x7F]);
    assert!(m.focus == "B" && !m.decks["A"].stems_muted.contains("mic"));
}

/// The grid is beats' macro layout (lib/backend/apc-mini-mk2.js), in beats' group colours,
/// bright while a macro runs; beats' macros dj doesn't have stay dark.
#[test]
fn apc_pads_are_beats_macro_grid_in_group_colours() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(apc_mini_mk2());
    let frame = c.led_frame(0.0, &m);
    assert!(has(&frame, [0x92, 0, 5]), "drop: Mute red");
    assert!(has(&frame, [0x92, 8, 37]), "sweep-lp: FX cyan");
    // (the eleven pan, shape and aux-FX macros Swift leaves unbound are bound, docs/AUX.md)
    assert!(has(&frame, [0x92, 11, 37]), "delay-throw: FX cyan");
    assert!(has(&frame, [0x92, 24, 45]), "ping-pong: Pan blue");
    assert!(has(&frame, [0x92, 60, 9]), "tighten: Shape orange");
    assert!(has(&frame, [0x92, 13, 0]), "a gap in the grid: dark");
    assert!(has(&frame, [0x92, 32, 53]), "octave-up: Pitch magenta");
    assert!(has(&frame, [0x92, 36, 13]), "half-time: Tempo yellow");
    assert!(has(&frame, [0x92, 40, 21]), "feel-chill: Feel green");
    assert!(has(&frame, [0x92, 56, 3]), "hit 1: white");
    send(&mut c, &mut m, &[0x90, 0, 0x7F]); // drop
    assert!(m.take().contains(&Action::Macro {
        deck: s("A"),
        id: s("drop")
    }));
    send(&mut c, &mut m, &[0x90, 24, 0x7F]); // ping-pong
    assert!(m.take().contains(&Action::Macro {
        deck: s("A"),
        id: s("ping-pong")
    }));
    // after the press's flash on the uptime clock
    let lit = c.led_frame(1e12, &m);
    assert!(has(&lit, [0x96, 0, 5]), "bright while it runs");
    assert!(has(&lit, [0x96, 24, 45]), "ping-pong bright while it runs");
}

/// djrust's APC layout (docs/HUB.md, diverging from Swift's FX faders): the faders are the
/// mixer, SHIFT + fader 1-8 is the aimed target's FX unit, the master fader is the master.
#[test]
fn apc_faders_are_the_mixer_and_shift_makes_them_the_fx_unit() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    m.decks.insert("L".into(), MockDeck::default());
    let mut c = controller(apc_mini_mk2());
    for (cc, v) in [
        (0x30, 127),
        (0x31, 0),
        (0x32, 64),
        (0x33, 0),
        (0x34, 127),
        (0x35, 32),
        (0x36, 127),
        (0x37, 0),
    ] {
        send(&mut c, &mut m, &[0xB0, cc, v]);
    }
    let val = |m: &Mock, d: &str, p: DeckParam| m.decks[d].vals.get(&pk(p)).copied();
    assert_eq!(val(&m, "A", DeckParam::Volume), Some(1.0));
    assert_eq!(val(&m, "B", DeckParam::Volume), Some(0.0));
    assert!((val(&m, "L", DeckParam::Volume).unwrap() - 64.0 / 127.0).abs() < 1e-12);
    assert_eq!(val(&m, "A", DeckParam::Filter), Some(0.0));
    assert_eq!(val(&m, "B", DeckParam::Filter), Some(1.0));
    assert!((val(&m, "L", DeckParam::Filter).unwrap() - 32.0 / 127.0).abs() < 1e-12);
    assert_eq!(m.crossfader, 1.0);
    assert_eq!(m.headphone_mix, 0.0);
    assert!(m.decks["A"].fx.is_empty(), "no FX unit without SHIFT");
    // SHIFT held: the same faders are the aimed deck's FX unit
    send(&mut c, &mut m, &[0x90, 0x7A, 0x7F]);
    for (cc, v) in [
        (0x30, 0),
        (0x31, 127),
        (0x32, 0),
        (0x33, 127),
        (0x34, 127),
        (0x35, 64),
        (0x36, 127),
        (0x37, 127),
    ] {
        send(&mut c, &mut m, &[0xB0, cc, v]);
    }
    send(&mut c, &mut m, &[0x80, 0x7A, 0x00]);
    let u = m.decks["A"].fx.clone();
    assert!(
        u[&FxParam::Filter] == -1.0
            && u[&FxParam::Hp] == 1.0
            && u[&FxParam::Lp] == 0.0
            && u[&FxParam::PhaserMix] == 1.0
    );
    assert!(u[&FxParam::Crush] == 1.0 && (u[&FxParam::Drive] - 64.0 / 127.0).abs() < 1e-12);
    assert!(u[&FxParam::Pitch] == 12.0 && u[&FxParam::Room] == 1.0);
    assert!(m.decks["B"].fx.is_empty(), "B untouched");
    assert_eq!(
        val(&m, "A", DeckParam::Volume),
        Some(1.0),
        "the mixer kept its place"
    );
    // aimed at B: SHIFT + fader moves B's FX unit
    send(&mut c, &mut m, &[0x90, 0x74, 0x7F]);
    send(&mut c, &mut m, &[0x90, 0x7A, 0x7F]);
    send(&mut c, &mut m, &[0xB0, 0x34, 64]);
    send(&mut c, &mut m, &[0x80, 0x7A, 0x00]);
    assert!(
        (m.decks["B"].fx[&FxParam::Crush] - 64.0 / 127.0).abs() < 1e-12
            && m.decks["A"].fx[&FxParam::Crush] == 1.0
    );
    // the master fader is the master, with or without SHIFT
    send(&mut c, &mut m, &[0xB0, 0x38, 64]);
    assert!((m.master - 64.0 / 127.0).abs() < 1e-6);
}

/// The bipolar faders' centre detent lands exactly on neutral, the ends reach the range,
/// pitch steps in semitones; unipolar faders are linear.
#[test]
fn fx_unit_faders_map_onto_each_controls_travel() {
    let v = |p: FxParam, cc: i32| fx_value(p, f64::from(cc) / 127.0);
    for cc in 62..=65 {
        assert!(
            v(FxParam::Filter, cc) == 0.0 && v(FxParam::Pitch, cc) == 0.0,
            "detent at {cc}"
        );
    }
    assert!(v(FxParam::Filter, 0) == -1.0 && v(FxParam::Filter, 127) == 1.0);
    assert!(v(FxParam::Filter, 61) < 0.0 && v(FxParam::Filter, 66) > 0.0);
    assert!(v(FxParam::Pitch, 0) == -12.0 && v(FxParam::Pitch, 127) == 12.0);
    assert!((0..=127).all(|c| v(FxParam::Pitch, c) == v(FxParam::Pitch, c).round()));
    assert!(
        v(FxParam::Lp, 127) == 1.0
            && v(FxParam::Lp, 0) == 0.0
            && v(FxParam::Hp, 0) == 0.0
            && v(FxParam::Room, 127) == 1.0
    );
    // monotonic all the way up
    for p in [
        FxParam::Filter,
        FxParam::Pitch,
        FxParam::Hp,
        FxParam::Lp,
        FxParam::Crush,
    ] {
        assert!((1..=127).all(|c| v(p, c) >= v(p, c - 1)), "{p:?}");
    }
}

/// Pad 7 (bottom row, next to PANIC) toggles the kill and pad 6 resets, on the aimed target;
/// their LEDs show the state.
#[test]
fn apc_kill_and_reset_pads_act_on_the_aimed_target() {
    let mut m = Mock::new().with_loops();
    m.dk("A").net = true;
    let mut c = controller(apc_mini_mk2());
    let later = 1e12; // past the presses' flash
    let pads = c.lights(later, None, &m)["pads"].clone();
    assert!(
        pads[6] == PadLight::new(3, LightLevel::Dim)
            && pads[7] == PadLight::new(5, LightLevel::Dim)
    );
    send(&mut c, &mut m, &[0x90, 0x7A, 0x7F]); // SHIFT + fader 5: crush
    send(&mut c, &mut m, &[0xB0, 0x34, 127]);
    send(&mut c, &mut m, &[0x80, 0x7A, 0x00]);
    send(&mut c, &mut m, &[0x90, 7, 0x7F]); // kill
    send(&mut c, &mut m, &[0x80, 7, 0]);
    assert!(m.decks["A"].killed && m.decks["A"].fx[&FxParam::Crush] == 1.0);
    let pads = c.lights(later, None, &m)["pads"].clone();
    assert!(pads[6].level == LightLevel::Full && pads[7] == PadLight::new(5, LightLevel::Full));
    send(&mut c, &mut m, &[0x90, 7, 0x7F]); // kill off again
    assert!(!m.decks["A"].killed);
    send(&mut c, &mut m, &[0x90, 6, 0x7F]); // reset
    assert!(m.decks["A"].fx_neutral() && !m.decks["A"].killed);
    // aimed at the loops: the same pads act there
    send(&mut c, &mut m, &[0x90, 0x7A, 0x7F]);
    send(&mut c, &mut m, &[0x90, 0x73, 0x7F]); // SHIFT + aim A = L
    send(&mut c, &mut m, &[0x80, 0x7A, 0x00]);
    assert_eq!(m.focus, "L");
    send(&mut c, &mut m, &[0x90, 7, 0x7F]);
    assert!(m.decks["L"].killed && !m.decks["A"].killed);
    // SHIFT + pad 7 is still the loop page's loop 64, not the kill
    assert!(c.profile().layout.iter().any(|b| b.group == "pads"
        && b.index == Some(7)
        && b.shift == Some(true)
        && b.action == DJActionKind::ClipToggle));
}

#[test]
fn apc_macro_pads_fire_macros_and_panic_restores() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(apc_mini_mk2());
    send(&mut c, &mut m, &[0x90, 0, 0x7F]); // bottom row, col 0: drop (aimed deck A)
    send(&mut c, &mut m, &[0x90, 0x77, 0x7F]); // scene 8: panic
    send(&mut c, &mut m, &[0x90, 1, 0x7F]); // breakdown mutes the drums
    let a = m.take();
    for want in [
        Action::Macro {
            deck: s("A"),
            id: s("drop"),
        },
        Action::Panic { deck: s("A") },
        Action::Macro {
            deck: s("A"),
            id: s("breakdown"),
        },
    ] {
        assert!(a.contains(&want), "{want:?} in {a:?}");
    }
}

#[test]
fn mpk_keys_play_the_live_voice_and_the_joystick_nudges_tempo() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(mpk_mini_mk2());
    send(&mut c, &mut m, &[0x90, 61, 110]);
    send(&mut c, &mut m, &[0x80, 61, 0]);
    let a = m.take();
    assert_eq!(
        a[0],
        Action::KeyDown {
            deck: s("A"),
            index: 61,
            velocity: 110,
            quant: None
        }
    );
    assert_eq!(
        a[1],
        Action::KeyUp {
            deck: s("A"),
            index: 61
        }
    );
    // pads turn the aimed deck's parts off and on
    send(&mut c, &mut m, &[0x99, 36, 100]); // pad 1: kick
    send(&mut c, &mut m, &[0x99, 40, 100]); // pad 5: bass
    let a = m.take();
    assert!(a.contains(&Action::ToggleRole {
        deck: s("A"),
        role: s("kick")
    }));
    assert!(a.contains(&Action::ToggleRole {
        deck: s("A"),
        role: s("bass")
    }));
    // joystick full right → +8 % on the focus deck (amount 1 = the full tempo-fader range)
    send(&mut c, &mut m, &[0xE0, 0x7F, 0x7F]);
    send(&mut c, &mut m, &[0xE0, 0x00, 0x40]); // centre
    let a = m.take();
    let nudges: Vec<f64> = a
        .iter()
        .filter_map(|x| match x {
            Action::Nudge { deck, amount } if deck == "A" => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(nudges.len(), 2);
    assert!(
        (nudges[0] - 1.0).abs() < 1e-9 && nudges[1].abs() < 1e-3,
        "{nudges:?}"
    );
}

#[test]
fn inpulse_pads_drive_macros_roles_and_stingers() {
    let mut m = Mock::new();
    m.dk("A").net = true;
    let mut c = controller(inpulse_200_mk3());
    let prof = inpulse_200_mk3();
    let note = |g: &str, i: usize| prof.number(g, i).unwrap();
    send(&mut c, &mut m, &[0x96, note("deckA.rollPads", 3), 0x7F]); // breakdown
    send(&mut c, &mut m, &[0x96, note("deckA.fxPads", 3), 0x7F]); // role: bass
    let a = m.take();
    assert!(
        a.contains(&Action::Macro {
            deck: s("A"),
            id: s("breakdown")
        }),
        "{a:?}"
    );
    assert!(
        a.contains(&Action::ToggleRole {
            deck: s("A"),
            role: s("bass")
        }),
        "{a:?}"
    );
}

// MARK: SurfaceBeatTests

#[test]
fn inpulse_sync_rewind_and_load_buttons() {
    let mut m = Mock::new();
    m.dk("A").bpm = Some(127.0);
    m.dk("B").bpm = Some(100.0);
    let prof = inpulse_200_mk3();
    let mut c = controller(prof.clone());
    // deck B (channel 2): SYNC is deckB.buttons[2] → note 5
    let sync_b = prof.number("deckB.buttons", 2).unwrap();
    send(&mut c, &mut m, &[0x92, sync_b, 0x7F]);
    assert!(m.take().contains(&Action::Sync {
        deck: s("B"),
        bar: false
    }));
    let on = prof.leds.as_ref().unwrap().single_on;
    assert!(
        !has(&c.led_frame(0.0, &m), [0x92, sync_b, on]),
        "SYNC is dark while the tempi differ"
    );
    m.dk("B").bpm = Some(127.0); // the engine matched them
    let led = c.led_frame(0.0, &m);
    assert!(
        has(&led, [0x92, sync_b, on]),
        "SYNC lights while the tempi match: {led:?}"
    );
    // shift + SYNC = bar sync (shift buttons live on channel 5 for deck B)
    let shift_sync_b = prof.number("deckB.shiftButtons", 0).unwrap();
    send(&mut c, &mut m, &[0x95, shift_sync_b, 0x7F]);
    assert!(m.take().contains(&Action::Sync {
        deck: s("B"),
        bar: true
    }));
    // rewind (shift + CUE)
    let shift_cue_b = prof.number("deckB.shiftButtons", 1).unwrap();
    send(&mut c, &mut m, &[0x95, shift_cue_b, 0x7F]);
    assert!(m.take().contains(&Action::Rewind { deck: s("B") }));
}

#[test]
fn apc_scenes_aim_play_and_auto_next() {
    let mut m = Mock::new();
    let mut c = controller(apc_mini_mk2());
    send(&mut c, &mut m, &[0x90, 0x70, 0x7F]); // play: the aimed deck, A
    assert!(m.decks["A"].playing && !m.decks["B"].playing);
    send(&mut c, &mut m, &[0x90, 0x74, 0x7F]); // aim at B
    send(&mut c, &mut m, &[0x90, 0x70, 0x7F]);
    assert!(m.decks["B"].playing);
    send(&mut c, &mut m, &[0x90, 0x72, 0x7F]); // auto-next on B
    assert!(m.decks["B"].auto_advance && !m.decks["A"].auto_advance);
    assert!(has(&c.led_frame(0.0, &m), [0x90, 0x72, 1]));
}

// MARK: locks and wander (DisableTests / surface wander)

#[test]
fn a_locked_action_does_nothing() {
    let mut m = Mock::new();
    m.locked.insert("*.play".into()); // the APC binds play to the aimed deck, "*"
    let mut c = controller(apc_mini_mk2());
    let (_, out) = send(&mut c, &mut m, &[0x90, 0x70, 0x7F]).unwrap();
    assert!(matches!(out, SurfaceOutcome::Bound(_)));
    assert!(m.actions.is_empty() && !m.decks["A"].playing);
    // the lock key names the binding: "<deck>.<action>[.<param>]"
    let e = c.decode(&[0x90, 0x70, 0x7F]).unwrap();
    assert_eq!(c.binding_for(&e).unwrap().lock_key(), "*.play");
    // a per-deck binding (the Inpulse's) names its deck, and a hot cue its slot
    let mut ip = controller(inpulse_200_mk3());
    let e = ip.decode(&[0x96, 0x01, 0x7F]).unwrap();
    assert_eq!(ip.binding_for(&e).unwrap().lock_key(), "A.hotcue.2");
}

// MARK: ControllerRouter / PadLightsTests

fn router_with_apc() -> (ControllerRouter, String, Mock) {
    let m = Mock::new();
    let mut r = ControllerRouter::with_defaults(&m.deck_names());
    let port = "APC mini mk2 Control".to_string();
    assert!(r.attach(&port));
    (r, port, m)
}

#[test]
fn the_mirror_says_what_the_leds_say() {
    let (r, port, mut m) = router_with_apc();
    m.dk("A").net = true;
    let _ = port;
    let (found_port, lights) = r.lights("APC mini mk2", 1_000.0, None, &m).unwrap();
    assert_eq!(found_port, "APC mini mk2 Control");
    assert!(lights["pads"].len() == 64 && lights["track"].len() == 8 && lights["scene"].len() == 8);
    // the macro grid in beats' group colours, aimed at deck A (a net)
    assert_eq!(
        lights["pads"][0],
        PadLight::new(5, LightLevel::Dim),
        "drop: red"
    );
    assert_eq!(
        lights["pads"][8],
        PadLight::new(37, LightLevel::Dim),
        "sweep-lp: cyan"
    );
    assert_eq!(lights["pads"][11].color_index, 37, "delay-throw: FX cyan");
    assert_eq!(lights["pads"][24].color_index, 45, "ping-pong: Pan blue");
    assert_eq!(lights["pads"][13].color_index, 0, "a gap in the grid: dark");
    assert!(r.lights("MPK Mini mk2", 0.0, None, &m).is_none());
}

#[test]
fn single_leds_report_on_off_blink() {
    let (r, _, mut m) = router_with_apc();
    m.dk("A").net = true;
    let get =
        |m: &Mock, g: &str, i: usize| r.lights("APC mini mk2", 0.0, None, m).unwrap().1[g][i].level;
    assert_eq!(get(&m, "scene", 0), LightLevel::Full, "play button is lit");
    m.dk("A").playing = true;
    assert_eq!(
        get(&m, "scene", 0),
        LightLevel::Blink,
        "a playing deck's button blinks"
    );
    assert_eq!(get(&m, "track", 0), LightLevel::Full, "kick role is on");
}

#[test]
fn pressing_on_screen_is_pressing_the_hardware() {
    let (mut r, port, mut m) = router_with_apc();
    assert!(r.press(&port, "scene", 4, false, 0.0, &mut m)); // aim at deck B
    assert!(r.press(&port, "scene", 0, false, 0.0, &mut m)); // play: deck B
    assert!(m.decks["B"].playing);
    assert!(r.press(&port, "pads", 0, false, 0.0, &mut m)); // drop on B (a sample): routed
    assert!(!r.press(&port, "pads", 99, false, 0.0, &mut m));
    assert!(
        !r.press(&port, "faders", 0, false, 0.0, &mut m),
        "continuous controls aren't clickable"
    );
    assert!(!r.press("nope", "pads", 0, false, 0.0, &mut m));
}

#[test]
fn on_screen_presses_are_recorded_like_hardware() {
    let (mut r, port, mut m) = router_with_apc();
    r.press(&port, "scene", 0, false, 0.0, &mut m); // deck A play
    assert_eq!(
        m.recorded,
        vec![
            (port.clone(), vec![0x90, 112, 127]),
            (port.clone(), vec![0x80, 112, 0])
        ]
    );
}

#[test]
fn a_shifted_press_holds_shift_around_the_pad_and_is_recorded() {
    let (mut r, port, mut m) = router_with_apc();
    assert!(r.press(&port, "pads", 56, true, 0.0, &mut m)); // loop 1
    assert_eq!(
        m.recorded
            .iter()
            .map(|(_, b)| b.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![0x90, 122, 127],
            vec![0x90, 56, 127],
            vec![0x80, 56, 0],
            vec![0x80, 122, 0]
        ]
    );
    assert!(
        !r.surface(&port).unwrap().shift_held(),
        "SHIFT is let go again"
    );
    // a surface without a SHIFT control cannot press shifted
    let mpk = "MPK mini";
    assert!(r.attach(mpk));
    assert!(r.surface(mpk).is_some());
    assert!(!r.press(mpk, "padNotes", 8, true, 0.0, &mut m));
    assert!(r.press(mpk, "padNotes", 8, false, 0.0, &mut m));
}

#[test]
fn the_router_sends_jog_messages_to_the_decks_clocks() {
    let mut m = Mock::new();
    let mut r = ControllerRouter::with_defaults(&m.deck_names());
    let port = "DJControl Inpulse 200 Mk3";
    assert!(r.attach(port));
    // touch, scratch, release on deck A
    let t = r.route(port, &[0x91, 0x08, 0x7F], 0.0, &mut m);
    assert_eq!(
        t,
        RouteResult::Jog(JogEvent::Touch {
            deck: s("A"),
            shift: false,
            down: true
        })
    );
    r.route(port, &[0xB1, 0x0A, 0x02], 0.0, &mut m);
    r.route(port, &[0x91, 0x08, 0x00], 0.0, &mut m);
    let spt = 60.0 / (100.0 / 3.0) / 248.0;
    assert_eq!(
        m.clocks["A"].calls,
        vec![
            "freeze".to_string(),
            format!("scrub {:.6}", 2.0 * spt),
            "release".to_string()
        ]
    );
    assert!(m.clocks["B"].calls.is_empty());
    // a hand on the platter steers its deck: auto-mix pauses (touch, scratch; not the release)
    let touches = m
        .take()
        .into_iter()
        .filter(|a| matches!(a, Action::AutoMixTouch { .. }))
        .count();
    assert_eq!(touches, 2);
    // an ordinary control on the same port reaches the surface
    match r.route(port, &[0x91, 0x07, 0x7F], 0.0, &mut m) {
        RouteResult::Surface(e, SurfaceOutcome::Bound(b)) => {
            assert_eq!(
                (e.name.as_deref(), b.action),
                (Some("play"), DJActionKind::Play)
            );
        }
        other => panic!("{other:?}"),
    }
    // a port nobody owns is ignored; a surface message the profile doesn't describe is undecoded
    assert_eq!(
        r.route("btw Bus 1", &[0x90, 1, 1], 0.0, &mut m),
        RouteResult::Ignored
    );
    assert_eq!(
        r.route(port, &[0xF8, 0, 0], 0.0, &mut m),
        RouteResult::Undecoded
    );
    assert_eq!(r.profile_name("btw Bus 1"), None);
}

// Not a Swift test: the surface reports a touch in auto-mix's own `AutoMixTouch` (it was a
// second enum, without `Expression`), so the kind an `Action::AutoMixTouch` carries goes
// straight to `AutoMixState::note_touch`: expression never pauses a walk, steering does.
#[test]
fn the_surfaces_auto_mix_touch_is_the_one_the_engine_takes() {
    use djrust::automix::{AutoMixSettings, AutoMixState};
    let touches = [
        Action::AutoMixTouch {
            deck: Some("A".into()),
            kind: AutoMixTouch::Expression,
        },
        Action::AutoMixTouch {
            deck: Some("A".into()),
            kind: AutoMixTouch::Steer,
        },
    ];
    let paused: Vec<bool> = touches
        .iter()
        .map(|a| {
            let Action::AutoMixTouch { deck, kind } = a else {
                unreachable!()
            };
            let mut st = AutoMixState {
                settings: AutoMixSettings::new(true, 4, 4),
                ..AutoMixState::default()
            };
            st.observe_countdown("A", 0);
            st.note_touch(deck.as_deref(), *kind, true);
            st.paused
        })
        .collect();
    assert_eq!(paused, [false, true]);
}


/// Capture to loop is bound like any other action: a profile entry names the action, the input
/// by `param` and a length / slot by `slot`, and any control of any device can carry it.
#[test]
fn capture_actions_bind_to_any_control_from_a_profile() {
    let mut text = include_str!("../data/profiles/mpk-mini-mk2.json").to_string();
    for (idx, old_slot, new) in [
        (8, 0, r#""action": "captureGrab",
   "param": "TD-3",
   "slot": 2"#),
        (9, 1, r#""action": "captureArm",
   "param": "TD-3""#),
        (10, 2, r#""action": "captureBeats",
   "slot": -1"#),
        (11, 3, r#""action": "libraryLoad",
   "slot": 1"#),
        (12, 4, r#""action": "librarySave",
   "slot": 0"#),
    ] {
        let old = format!(
            "\"index\": {idx},\n   \"action\": \"clipToggle\",\n   \"slot\": {old_slot}"
        );
        assert!(text.contains(&old), "pad {idx}");
        text = text.replace(&old, &format!("\"index\": {idx},\n   {new}"));
    }
    let profile = Arc::new(SurfaceProfile::from_json(&text).expect("the profile parses"));
    let mut m = Mock::new();
    m.clips = vec![LoopState::Off; 3];
    let mut c = controller(profile);
    // pads 9..13 of the notes bank: notes 44..48 on channel 10
    send(&mut c, &mut m, &[0x99, 44, 0x7F]);
    send(&mut c, &mut m, &[0x99, 45, 0x7F]); // arm: pressed
    send(&mut c, &mut m, &[0x89, 45, 0x00]); // and released
    send(&mut c, &mut m, &[0x99, 46, 0x7F]);
    send(&mut c, &mut m, &[0x99, 47, 0x7F]);
    send(&mut c, &mut m, &[0x99, 48, 0x7F]);
    let a: Vec<Action> = m
        .take()
        .into_iter()
        .filter(|a| {
            matches!(
                a,
                Action::CaptureGrab { .. }
                    | Action::CaptureArm { .. }
                    | Action::CaptureBeats { .. }
                    | Action::LibraryLoad { .. }
                    | Action::LibrarySave { .. }
            )
        })
        .collect();
    assert_eq!(
        a,
        vec![
            Action::CaptureGrab { input: Some("TD-3".into()), beats: Some(2.0) },
            Action::CaptureArm { input: Some("TD-3".into()), on: true },
            Action::CaptureArm { input: Some("TD-3".into()), on: false },
            Action::CaptureBeats { input: None, delta: -1 },
            Action::LibraryLoad { slot: 1 },
            Action::LibrarySave { slot: 0 },
        ]
    );
    // a library slot the library does not have is not sent
    let text2 = text.replace(r#""action": "libraryLoad",
   "slot": 1"#, r#""action": "libraryLoad",
   "slot": 9"#);
    let mut c2 = controller(Arc::new(SurfaceProfile::from_json(&text2).unwrap()));
    let mut m2 = Mock::new();
    send(&mut c2, &mut m2, &[0x99, 47, 0x7F]);
    assert!(m2.take().iter().all(|a| !matches!(a, Action::LibraryLoad { .. })));
}
