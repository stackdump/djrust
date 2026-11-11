//! `MacroChase.swift`, SPEC mode 26: the macro chase pulse (lib/ui/controllers.js
//! `macroPulse`, lib/macros/runtime.js `executeMacro` call sites, petri-note.css
//! `.pn-pulsing`, `.pn-pulsing-hot`, `.pn-macro-btn.running`, `@keyframes pn-macro-pulse`).
//!
//! In beats, firing a mute macro lights the mute buttons it touches in a "running light":
//! every target glows dimly (`pn-pulsing`) and one at a time, 120 ms each, glows hard
//! (`pn-pulsing-hot`), until the macro's duration is over. Here the same thing for a deck's
//! role toggles, driven by what the engine says ([`MacroActivity`]), never by a UI timer
//! that guesses how long a macro lasts. Pure arithmetic over an injected `dt`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::control::leds::Rgba;
use crate::control::macros::NetMacro;

/// What the engine says is running on one deck, read from its mirrors (`MacroActivity`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MacroActivity {
    /// The macros in effect, in catalogue order.
    pub running: Vec<NetMacro>,
    /// For each running macro that chases, the roles it is holding muted, in mixer order.
    pub targets: BTreeMap<NetMacro, Vec<String>>,
}

impl MacroActivity {
    pub fn idle() -> MacroActivity {
        MacroActivity::default()
    }
}

/// One macro's chase: its targets, and where the running light is (`MacroChase`).
///
/// `macroPulse`: `BLINK = 120`; the first frame lights element 0 at once, then every 120 ms
/// the hot one moves on, wrapping round the targets.
#[derive(Clone, Debug, PartialEq)]
pub struct MacroChase {
    pub macro_: NetMacro,
    /// The roles being chased, in order (the engine's held set as of the last update).
    targets: Vec<String>,
    /// Seconds since the chase began.
    elapsed: f64,
}

impl MacroChase {
    /// 120 ms per step.
    pub const STEP: f64 = 0.12;

    pub fn new(macro_: NetMacro, targets: Vec<String>) -> MacroChase {
        MacroChase {
            macro_,
            targets,
            elapsed: 0.0,
        }
    }

    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    pub fn elapsed(&self) -> f64 {
        self.elapsed
    }

    /// Let `dt` seconds pass. Negative or non-finite `dt` is ignored.
    pub fn advance(&mut self, dt: f64) {
        if dt.is_finite() && dt > 0.0 {
            self.elapsed += dt;
        }
    }

    /// The engine's held set changed (a mute landed late, a restore came early): follow it,
    /// keep the running light where it is.
    pub fn retarget(&mut self, roles: Vec<String>) {
        self.targets = roles;
    }

    /// How many 120 ms boundaries have passed (0 in the first step). A hair of tolerance so
    /// that a clock that adds 0.04 three times sits on the boundary, not just under it.
    pub fn steps(&self) -> usize {
        ((self.elapsed + 1e-9) / Self::STEP) as usize
    }

    /// The index of the hot target; None with no targets.
    pub fn hot_index(&self) -> Option<usize> {
        if self.targets.is_empty() {
            None
        } else {
            Some(self.steps() % self.targets.len())
        }
    }

    pub fn hot(&self) -> Option<&str> {
        self.hot_index().map(|i| self.targets[i].as_str())
    }
}

/// How one role toggle is lit (`MacroPulseLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroPulseLevel {
    None,
    /// `.pn-pulsing`: touched by a running macro.
    Dim,
    /// `.pn-pulsing-hot`: the running light is on it.
    Hot,
}

/// What the views draw for a deck: which macros are running, which roles pulse, which is hot
/// (`MacroPulseState`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MacroPulseState {
    pub running: BTreeSet<NetMacro>,
    pub pulsing: BTreeSet<String>,
    pub hot: BTreeSet<String>,
}

impl MacroPulseState {
    pub fn is_running(&self, m: NetMacro) -> bool {
        self.running.contains(&m)
    }

    pub fn level(&self, role: &str) -> MacroPulseLevel {
        if self.hot.contains(role) {
            MacroPulseLevel::Hot
        } else if self.pulsing.contains(role) {
            MacroPulseLevel::Dim
        } else {
            MacroPulseLevel::None
        }
    }
}

/// The chases of one deck (`MacroChaseBoard`). `update` takes what the engine says and the
/// time that passed; a macro that appears starts a chase, one that disappears loses it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MacroChaseBoard {
    /// Running chases, in the order the macros appeared.
    pub chases: Vec<MacroChase>,
    pub running: Vec<NetMacro>,
}

impl MacroChaseBoard {
    pub fn new() -> MacroChaseBoard {
        MacroChaseBoard::default()
    }

    pub fn update(&mut self, activity: &MacroActivity, dt: f64) {
        self.running = activity.running.clone();
        // end on macro end
        self.chases.retain(|c| activity.running.contains(&c.macro_));
        for &m in &activity.running {
            if m.chase_targets().is_none() {
                continue;
            }
            let roles = activity.targets.get(&m).cloned().unwrap_or_default();
            if let Some(c) = self.chases.iter_mut().find(|c| c.macro_ == m) {
                c.advance(dt);
                c.retarget(roles);
            } else {
                self.chases.push(MacroChase::new(m, roles)); // starts at step 0: no dt yet
            }
        }
    }

    pub fn state(&self) -> MacroPulseState {
        let mut s = MacroPulseState {
            running: self.running.iter().copied().collect(),
            ..Default::default()
        };
        for c in &self.chases {
            s.pulsing.extend(c.targets().iter().cloned());
            if let Some(h) = c.hot() {
                s.hot.insert(h.into());
            }
        }
        s
    }
}

/// A `box-shadow`: `0 0 <blur>px <spread>px rgba(233, 69, 96, <alpha>)` (`MacroPulseStyle.Glow`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub spread: f64,
    pub blur: f64,
    pub alpha: f64,
}

/// The look of the pulse, in beats' CSS numbers (`MacroPulseStyle`).
pub struct MacroPulseStyle;

impl MacroPulseStyle {
    /// `.pn-pulsing`: `0 0 4px 1px rgba(233, 69, 96, 0.35)`.
    pub const DIM: Glow = Glow {
        spread: 1.0,
        blur: 4.0,
        alpha: 0.35,
    };
    /// `.pn-pulsing-hot`: `0 0 10px 3px rgba(233, 69, 96, 0.95)`.
    pub const HOT: Glow = Glow {
        spread: 3.0,
        blur: 10.0,
        alpha: 0.95,
    };
    /// `animation: pn-macro-pulse 1s ease-in-out infinite`.
    pub const RUNNING_PERIOD: f64 = 1.0;
    /// `@keyframes pn-macro-pulse`: `0%, 100% { 0 0 0 0 rgba(…, 0.7) }`.
    pub const RUNNING_REST: Glow = Glow {
        spread: 0.0,
        blur: 0.0,
        alpha: 0.7,
    };
    /// `50% { 0 0 10px 3px rgba(…, 0.95) }`.
    pub const RUNNING_PEAK: Glow = Glow {
        spread: 3.0,
        blur: 10.0,
        alpha: 0.95,
    };

    /// `rgba(233, 69, 96, …)`: the accent `#e94560` of every glow here.
    pub fn color() -> Rgba {
        Rgba::from_hex("#e94560").unwrap()
    }
    /// `.pn-macro-btn.running`: fill `#e94560`, white text, border `#f8b3c0`.
    pub fn running_fill() -> Rgba {
        Self::color()
    }
    pub fn running_border() -> Rgba {
        Rgba::from_hex("#f8b3c0").unwrap()
    }

    pub fn glow(level: MacroPulseLevel) -> Option<Glow> {
        match level {
            MacroPulseLevel::None => None,
            MacroPulseLevel::Dim => Some(Self::DIM),
            MacroPulseLevel::Hot => Some(Self::HOT),
        }
    }

    /// The running button's glow at time `t` seconds: the keyframes, eased in and out within
    /// each half of the period (CSS applies `ease-in-out` between every pair of keyframes).
    pub fn running_glow_at(t: f64) -> Glow {
        let x = t / Self::RUNNING_PERIOD;
        let p = x - libm::trunc(x);
        let phase = if p < 0.0 { p + 1.0 } else { p };
        // 0 at the ends, 1 at the middle
        let half = if phase < 0.5 {
            phase * 2.0
        } else {
            (1.0 - phase) * 2.0
        };
        let k = Self::ease_in_out(half);
        let mix = |a: f64, b: f64| a + (b - a) * k;
        Glow {
            spread: mix(Self::RUNNING_REST.spread, Self::RUNNING_PEAK.spread),
            blur: mix(Self::RUNNING_REST.blur, Self::RUNNING_PEAK.blur),
            alpha: mix(Self::RUNNING_REST.alpha, Self::RUNNING_PEAK.alpha),
        }
    }

    /// CSS `ease-in-out`: `cubic-bezier(0.42, 0, 0.58, 1)`.
    pub fn ease_in_out(x: f64) -> f64 {
        Self::cubic_bezier(x, 0.42, 0.0, 0.58, 1.0)
    }

    /// y of the cubic Bezier through (0,0), (x1,y1), (x2,y2), (1,1) at progress `x`:
    /// bisection on the curve parameter.
    pub fn cubic_bezier(x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        fn at(t: f64, a: f64, b: f64) -> f64 {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..48 {
            let mid = (lo + hi) / 2.0;
            if at(mid, x1, x2) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        at((lo + hi) / 2.0, y1, y2)
    }
}
