//! `RingSpin.swift`: the main ring's extras from beats' workspace (SPEC mode 22):
//! lib/ui/canvas.js `drawRing`, lib/macros/runtime.js `autoDjSpin` / `autoDjSpinAnimate`
//! and petri-note.css `.pn-transition.playhead`. Values only: the ring view draws them.

/// How the ring's arc layer is drawn (`RingArcs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingArcs {
    /// The deck is playing.
    pub playing: bool,
    /// Rotation of the arc layer about the ring's centre, degrees clockwise.
    pub angle_degrees: f64,
    /// Arrowheads point back along the arcs (`_autoDjReverse`).
    pub reverse_arrows: bool,
}

impl RingArcs {
    /// `ctx.strokeStyle = '#4a90d9'`.
    pub const STROKE_HEX: &'static str = "#4a90d9";
    /// `ctx.globalAlpha = playing ? 0.3 : 1`.
    pub const PLAYING_ALPHA: f64 = 0.3;
    /// `drawArrowhead`: the two wings leave the tip at ±30 degrees to the arc.
    pub const WING_ANGLE: f64 = core::f64::consts::PI / 6.0;

    pub fn alpha(&self) -> f64 {
        if self.playing {
            Self::PLAYING_ALPHA
        } else {
            1.0
        }
    }

    /// `(playing ? 1 : 2) / scale`: a 1 px or 2 px line on screen whatever the zoom.
    pub fn line_width(&self, scale: f64) -> f64 {
        (if self.playing { 1.0 } else { 2.0 }) / scale.max(1e-9)
    }

    /// How far short of the target dot the arrowhead's tip stops: 1.4 x the dot's base
    /// radius.
    pub fn tip_set_back(base: f64) -> f64 {
        base * 1.4
    }

    /// The wing length of an arrowhead on a chord of length `chord` between dots of radius
    /// `base`, None where none fits.
    pub fn head_length(chord: f64, base: f64) -> Option<f64> {
        let length = (chord * 0.35).min(4.0);
        if chord >= 2.0 * Self::tip_set_back(base) + length {
            Some(length)
        } else {
            None
        }
    }
}

/// The persistent playhead: the last-fired step stays lit until the next one fires
/// (`RingPlayhead`).
pub struct RingPlayhead;

impl RingPlayhead {
    pub const HEX: &'static str = "#ffd36a";
    pub const RGB8: (u8, u8, u8) = (255, 211, 106);
    pub const OUTER_GLOW_ALPHA: f64 = 0.7;
    pub const INNER_GLOW_ALPHA: f64 = 0.35;
    pub const NODE_PX: f64 = 40.0;
    pub const BORDER_PX: f64 = 3.0;
    pub const OUTER_BLUR_PX: f64 = 6.0;
    pub const INNER_BLUR_PX: f64 = 4.0;
    /// The one free choice: the ring's radius as a multiple of `base`.
    pub const RADIUS_FACTOR: f64 = 1.9;
    /// The smallest any of the ring's sizes is drawn, in points.
    pub const FLOOR_POINTS: f64 = 1.0;

    pub fn radius(base: f64) -> f64 {
        base * Self::RADIUS_FACTOR
    }
    /// Points per CSS px.
    pub fn points_per_px(base: f64) -> f64 {
        Self::radius(base) / (Self::NODE_PX / 2.0)
    }
    pub fn line_width(base: f64) -> f64 {
        Self::FLOOR_POINTS.max(Self::BORDER_PX * Self::points_per_px(base))
    }
    /// The radius to hand a shadow (its sigma): half the 6 px blur.
    pub fn outer_glow_radius(base: f64) -> f64 {
        Self::FLOOR_POINTS.max(Self::OUTER_BLUR_PX / 2.0 * Self::points_per_px(base))
    }
    /// The width of the ramp inside the ring: the 4 px blur.
    pub fn inner_glow_radius(base: f64) -> f64 {
        Self::FLOOR_POINTS.max(Self::INNER_BLUR_PX * Self::points_per_px(base))
    }
}

/// The Auto-DJ ring swing (`autoDjSpin` + `autoDjSpinAnimate`), driven by an injected clock.
///
/// Each call to `spin` nudges the target by ±90 degrees per step, alternating direction
/// between calls, and tweens to it over 800 ms with a cubic ease-out. The tween starts from
/// wherever the last one had got to. Pure arithmetic: `angle_at` takes a time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingSpin {
    target_angle: f64,
    from_angle: f64,
    start_time: f64,
    direction: f64,
    reverse_arrows: bool,
    advances_seen: Option<i64>,
}

impl Default for RingSpin {
    fn default() -> Self {
        RingSpin::new()
    }
}

impl RingSpin {
    /// `autoDjSpinAnimate`: `DURATION = 800` ms.
    pub const DURATION: f64 = 0.8;
    /// `dir * 90 * steps`.
    pub const DEGREES_PER_STEP: f64 = 90.0;

    pub fn new() -> RingSpin {
        RingSpin {
            target_angle: 0.0,
            from_angle: 0.0,
            start_time: f64::NEG_INFINITY,
            direction: 1.0,
            reverse_arrows: false,
            advances_seen: None,
        }
    }

    pub fn target_angle(&self) -> f64 {
        self.target_angle
    }

    pub fn reverse_arrows(&self) -> bool {
        self.reverse_arrows
    }

    pub fn advances_seen(&self) -> Option<i64> {
        self.advances_seen
    }

    /// `1 - (1 - t)^3`, with `t` clamped to 0...1.
    pub fn eased(t: f64) -> f64 {
        let c = t.clamp(0.0, 1.0);
        1.0 - libm::pow(1.0 - c, 3.0)
    }

    /// The angle at `now`: `from + (to - from) * eased`.
    pub fn angle_at(&self, now: f64) -> f64 {
        let t = (now - self.start_time) / Self::DURATION; // +infinity before the first spin
        self.from_angle + (self.target_angle - self.from_angle) * Self::eased(t)
    }

    /// Is a swing still running at `now`?
    pub fn is_spinning(&self, now: f64) -> bool {
        now - self.start_time < Self::DURATION
    }

    /// `autoDjSpin(el, steps)`: a no-op for `steps <= 0`.
    pub fn spin(&mut self, steps: i64, now: f64) {
        if steps <= 0 {
            return;
        }
        let current = self.angle_at(now);
        self.target_angle += self.direction * Self::DEGREES_PER_STEP * steps as f64;
        self.reverse_arrows = self.direction < 0.0;
        self.direction = -self.direction;
        self.start_time = now;
        self.from_angle = current;
    }

    /// Spin once when `advances` has moved since the last look. The first look only records
    /// the count.
    pub fn observe(&mut self, advances: i64, now: f64) {
        let seen = self.advances_seen;
        self.advances_seen = Some(advances);
        if let Some(seen) = seen {
            if advances != seen {
                self.spin(1, now);
            }
        }
    }

    /// The arc layer's state for a frame.
    pub fn arcs(&self, playing: bool, now: f64) -> RingArcs {
        RingArcs {
            playing,
            angle_degrees: self.angle_at(now),
            reverse_arrows: self.reverse_arrows,
        }
    }
}
