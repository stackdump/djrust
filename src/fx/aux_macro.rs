//! The eleven macros of beats' catalog (lib/macros/catalog.js) that no earlier port could fire,
//! because the DSP they move was not here (docs/HUB.md M4):
//!
//! | group | macros | what they move in beats | here |
//! |---|---|---|---|
//! | Pan | `ping-pong`, `hard-left`, `hard-right`, `auto-pan`, `mono` | the MIDI pan (CC 10) of each channel the macro targets (`channelParamMove`, `pan-move`) | each part's pan in the aimed deck's runner ([`WaveRunner::set_pan_move`](crate::wave::WaveRunner)) |
//! | Shape | `tighten`, `loosen`, `pulse` | each channel's note decay (`setChannelDecay`, `decay-move`) | each part's decay multiplier in the aimed deck's runner |
//! | FX | `delay-throw`, `dub-delay`, `cathedral` | the master delay and reverb sliders (`fx-hold`) | the aux bus's sliders ([`super::aux_bus`]) **and** the aimed strip's aux send, so the aimed deck goes into the effect |
//!
//! beats' catalog data is copied from `catalog.js` (MIT, bitwrap.io): ids, labels, durations,
//! targets, patterns and values. Their *shape in time* is `lib/ui/controllers.js`
//! `channelParamMove` (pan and decay) and `lib/macros/effects.js` `fxHold` (the FX holds): both
//! are pure functions of the time since the macro started, here in beats, so a macro is the
//! same on every host and offline.
//!
//! Nothing here moves a knob: [`AuxMacro::pan_at`], [`AuxMacro::decay_at`] and [`hold_at`] say
//! what the value is, and the renderer (`crate::host::deck::engine`) applies it each block.

use libm::{floor, sin};

use super::aux_bus::AuxFxKey;
use crate::wave::MacroTargets;

/// The aux send the aimed strip is pushed to while an FX macro holds: the whole point of "Delay
/// Throw on A" is that A goes into the delay.
pub const THROW_SEND: f64 = 0.8;

/// A macro the aux bus and the per-part moves make audible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum AuxMacro {
    PingPong,
    HardLeft,
    HardRight,
    AutoPan,
    Mono,
    DelayThrow,
    DubDelay,
    Cathedral,
    Tighten,
    Loosen,
    Pulse,
}

/// How a pan or decay value moves over the macro (`pattern`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pattern {
    /// Left, right, left… every `step_beats` (`pingpong`).
    PingPong { step_beats: f64 },
    /// A constant (`hold`).
    Hold(f64),
    /// A sine of `rate_beats` per cycle (`sweep`); a decay sweeps `min…max`.
    Sweep { rate_beats: f64, min: f64, max: f64 },
}

/// What kind of move it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuxKind {
    /// `pan-move`: the parts' pan, -1 left … +1 right.
    Pan(Pattern),
    /// `decay-move`: the parts' decay multiplier (1 = as written).
    Decay(Pattern),
    /// `fx-hold`: the aux sliders jump to the values, hold, and fade back over the last
    /// `tail_frac` of the duration.
    Hold { ops: &'static [(AuxFxKey, f64)], tail_frac: f64 },
}

static DELAY_THROW: [(AuxFxKey, f64); 3] = [
    (AuxFxKey::DelayWet, 100.0),
    (AuxFxKey::DelayFeedback, 72.0),
    (AuxFxKey::DelayTime, 38.0),
];
static DUB_DELAY: [(AuxFxKey, f64); 3] = [
    (AuxFxKey::DelayTime, 50.0),
    (AuxFxKey::DelayFeedback, 80.0),
    (AuxFxKey::DelayWet, 70.0),
];
static CATHEDRAL: [(AuxFxKey, f64); 3] = [
    (AuxFxKey::ReverbSize, 95.0),
    (AuxFxKey::ReverbDamp, 8.0),
    (AuxFxKey::ReverbWet, 60.0),
];

impl AuxMacro {
    pub const ALL: [AuxMacro; 11] = [
        AuxMacro::PingPong,
        AuxMacro::HardLeft,
        AuxMacro::HardRight,
        AuxMacro::AutoPan,
        AuxMacro::Mono,
        AuxMacro::DelayThrow,
        AuxMacro::DubDelay,
        AuxMacro::Cathedral,
        AuxMacro::Tighten,
        AuxMacro::Loosen,
        AuxMacro::Pulse,
    ];

    pub fn raw(self) -> u8 {
        self as u8
    }

    pub fn from_raw(r: u8) -> Option<AuxMacro> {
        Self::ALL.get(r as usize).copied()
    }

    pub fn id(self) -> &'static str {
        use AuxMacro::*;
        match self {
            PingPong => "ping-pong",
            HardLeft => "hard-left",
            HardRight => "hard-right",
            AutoPan => "auto-pan",
            Mono => "mono",
            DelayThrow => "delay-throw",
            DubDelay => "dub-delay",
            Cathedral => "cathedral",
            Tighten => "tighten",
            Loosen => "loosen",
            Pulse => "pulse",
        }
    }

    pub fn from_id(id: &str) -> Option<AuxMacro> {
        Self::ALL.iter().copied().find(|m| m.id() == id)
    }

    pub fn label(self) -> &'static str {
        use AuxMacro::*;
        match self {
            PingPong => "Ping-Pong",
            HardLeft => "Hard Left",
            HardRight => "Hard Right",
            AutoPan => "Auto-Pan",
            Mono => "Mono",
            DelayThrow => "Delay Throw",
            DubDelay => "Dub Delay",
            Cathedral => "Cathedral",
            Tighten => "Tighten",
            Loosen => "Loosen",
            Pulse => "Pulse",
        }
    }

    /// The catalog group.
    pub fn group(self) -> &'static str {
        use AuxMacro::*;
        match self {
            PingPong | HardLeft | HardRight | AutoPan | Mono => "Pan",
            DelayThrow | DubDelay | Cathedral => "FX",
            Tighten | Loosen | Pulse => "Shape",
        }
    }

    /// Bars.
    pub fn default_duration(self) -> i32 {
        use AuxMacro::*;
        match self {
            HardLeft | HardRight | DelayThrow => 1,
            AutoPan | Cathedral => 4,
            _ => 2,
        }
    }

    pub fn duration_options(self) -> &'static [i32] {
        use AuxMacro::*;
        match self {
            AutoPan => &[2, 4, 8],
            Cathedral => &[2, 4, 8],
            DubDelay => &[1, 2, 4],
            _ => &[1, 2, 4, 8],
        }
    }

    /// Which parts a pan or shape move takes (`targets`). The FX holds take the aimed strip.
    pub fn targets(self) -> MacroTargets {
        use AuxMacro::*;
        match self {
            PingPong | HardLeft | HardRight => MacroTargets::NonDrums,
            _ => MacroTargets::Everything,
        }
    }

    pub fn kind(self) -> AuxKind {
        use AuxMacro::*;
        match self {
            PingPong => AuxKind::Pan(Pattern::PingPong { step_beats: 1.0 }),
            HardLeft => AuxKind::Pan(Pattern::Hold(-1.0)),
            HardRight => AuxKind::Pan(Pattern::Hold(1.0)),
            AutoPan => AuxKind::Pan(Pattern::Sweep { rate_beats: 4.0, min: -1.0, max: 1.0 }),
            Mono => AuxKind::Pan(Pattern::Hold(0.0)),
            DelayThrow => AuxKind::Hold { ops: &DELAY_THROW, tail_frac: 0.35 },
            DubDelay => AuxKind::Hold { ops: &DUB_DELAY, tail_frac: 0.4 },
            Cathedral => AuxKind::Hold { ops: &CATHEDRAL, tail_frac: 0.5 },
            Tighten => AuxKind::Decay(Pattern::Hold(0.3)),
            Loosen => AuxKind::Decay(Pattern::Hold(2.5)),
            Pulse => AuxKind::Decay(Pattern::Sweep { rate_beats: 2.0, min: 0.2, max: 2.5 }),
        }
    }

    /// Its bit in a mask of the macros running.
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// The pan at `beats` into the macro (-1 left, +1 right), for a pan move. A pan move ends by
    /// putting the pan back (the renderer clears it), so there is no value past the end.
    pub fn pan_at(self, beats: f64) -> Option<f64> {
        match self.kind() {
            AuxKind::Pan(p) => Some(pattern_at(p, beats, true)),
            _ => None,
        }
    }

    /// The decay multiplier at `beats` into the macro, for a shape move.
    pub fn decay_at(self, beats: f64) -> Option<f64> {
        match self.kind() {
            AuxKind::Decay(p) => Some(pattern_at(p, beats, false)),
            _ => None,
        }
    }
}

/// `channelParamMove`'s value at `elapsed` beats: a ping-pong flips side every `step_beats`
/// (left first), a sweep is `sin(2π t / rate)` (a pan) or that mapped onto `min…max` (a decay), a
/// hold is its value.
pub fn pattern_at(p: Pattern, beats: f64, pan: bool) -> f64 {
    match p {
        Pattern::PingPong { step_beats } => {
            let step = floor(beats.max(0.0) / step_beats.max(1e-9)) as i64;
            if step % 2 == 0 {
                -1.0
            } else {
                1.0
            }
        }
        Pattern::Hold(v) => v,
        Pattern::Sweep { rate_beats, min, max } => {
            let s = sin(beats / rate_beats * 2.0 * core::f64::consts::PI);
            if pan {
                s
            } else {
                min + (s + 1.0) * 0.5 * (max - min)
            }
        }
    }
}

/// `fxHold`'s value at `elapsed` of `duration` (any one unit): `to` at once, held until the last
/// `tail_frac` of the duration, then a straight line back to `start`; `start` from the end.
/// (beats' tail is at least 50 ms; a macro is never shorter than a bar, so it never bites.)
pub fn hold_at(start: f64, to: f64, elapsed: f64, duration: f64, tail_frac: f64) -> f64 {
    if elapsed >= duration {
        return start;
    }
    let tail = (duration * tail_frac).max(1e-9);
    let sustain = (duration - tail).max(0.0);
    if elapsed < sustain {
        return to;
    }
    let t = (elapsed - sustain) / tail;
    to + (start - to) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn the_catalog_is_beats_eleven() {
        let table: Vec<(&str, &str, i32, &[i32])> = AuxMacro::ALL
            .iter()
            .map(|m| (m.id(), m.group(), m.default_duration(), m.duration_options()))
            .collect();
        assert_eq!(
            table,
            vec![
                ("ping-pong", "Pan", 2, &[1, 2, 4, 8][..]),
                ("hard-left", "Pan", 1, &[1, 2, 4, 8][..]),
                ("hard-right", "Pan", 1, &[1, 2, 4, 8][..]),
                ("auto-pan", "Pan", 4, &[2, 4, 8][..]),
                ("mono", "Pan", 2, &[1, 2, 4, 8][..]),
                ("delay-throw", "FX", 1, &[1, 2, 4, 8][..]),
                ("dub-delay", "FX", 2, &[1, 2, 4][..]),
                ("cathedral", "FX", 4, &[2, 4, 8][..]),
                ("tighten", "Shape", 2, &[1, 2, 4, 8][..]),
                ("loosen", "Shape", 2, &[1, 2, 4, 8][..]),
                ("pulse", "Shape", 2, &[1, 2, 4, 8][..]),
            ]
        );
        for m in AuxMacro::ALL {
            assert_eq!(AuxMacro::from_id(m.id()), Some(m));
            assert_eq!(AuxMacro::from_raw(m.raw()), Some(m));
            assert!(m.duration_options().contains(&m.default_duration()), "{}", m.id());
        }
        assert_eq!(AuxMacro::from_id("drop"), None);
        // targets as the catalog has them
        assert_eq!(AuxMacro::PingPong.targets(), MacroTargets::NonDrums);
        assert_eq!(AuxMacro::AutoPan.targets(), MacroTargets::Everything);
        assert_eq!(AuxMacro::Mono.targets(), MacroTargets::Everything);
        assert_eq!(AuxMacro::Tighten.targets(), MacroTargets::Everything);
    }

    #[test]
    fn pan_patterns_follow_channel_param_move() {
        // ping-pong: left on the first beat, right on the second…
        let pp = |b: f64| AuxMacro::PingPong.pan_at(b).unwrap();
        assert_eq!((pp(0.0), pp(0.99), pp(1.0), pp(1.5), pp(2.0), pp(3.0)), (-1.0, -1.0, 1.0, 1.0, -1.0, 1.0));
        assert_eq!(AuxMacro::HardLeft.pan_at(0.3), Some(-1.0));
        assert_eq!(AuxMacro::HardRight.pan_at(7.0), Some(1.0));
        assert_eq!(AuxMacro::Mono.pan_at(1.0), Some(0.0));
        // auto-pan: a sine, 4 beats a cycle, right at the first quarter, left at the third
        let ap = |b: f64| AuxMacro::AutoPan.pan_at(b).unwrap();
        assert!(ap(0.0).abs() < 1e-12);
        assert!((ap(1.0) - 1.0).abs() < 1e-12);
        assert!((ap(3.0) + 1.0).abs() < 1e-12);
        assert!(ap(4.0).abs() < 1e-9);
        assert_eq!(AuxMacro::Tighten.pan_at(0.0), None);
    }

    #[test]
    fn decay_patterns_follow_channel_param_move() {
        assert_eq!(AuxMacro::Tighten.decay_at(1.0), Some(0.3));
        assert_eq!(AuxMacro::Loosen.decay_at(1.0), Some(2.5));
        // pulse: 0.2…2.5 on a sine of 2 beats, the middle at the start
        let p = |b: f64| AuxMacro::Pulse.decay_at(b).unwrap();
        assert!((p(0.0) - 1.35).abs() < 1e-12);
        assert!((p(0.5) - 2.5).abs() < 1e-12);
        assert!((p(1.5) - 0.2).abs() < 1e-12);
        assert_eq!(AuxMacro::Mono.decay_at(0.0), None);
    }

    #[test]
    fn a_hold_jumps_holds_and_fades_back_like_fx_hold() {
        // 8 beats, tail 0.5: holds for 4, then a straight line back
        assert_eq!(hold_at(15.0, 100.0, 0.0, 8.0, 0.5), 100.0);
        assert_eq!(hold_at(15.0, 100.0, 3.99, 8.0, 0.5), 100.0);
        assert!((hold_at(15.0, 100.0, 6.0, 8.0, 0.5) - 57.5).abs() < 1e-9);
        assert_eq!(hold_at(15.0, 100.0, 8.0, 8.0, 0.5), 15.0);
        assert_eq!(hold_at(15.0, 100.0, 20.0, 8.0, 0.5), 15.0);
        // the cathedral's own: 4 bars = 16 beats, tail 0.5
        let AuxKind::Hold { ops, tail_frac } = AuxMacro::Cathedral.kind() else { panic!() };
        assert_eq!(tail_frac, 0.5);
        assert!(ops.contains(&(AuxFxKey::ReverbSize, 95.0)));
        assert!(ops.contains(&(AuxFxKey::ReverbWet, 60.0)));
    }
}
