//! The master-chain macros of beats' catalog (lib/macros/catalog.js): the FX, pitch and feel
//! kinds, which drive beats' master FX sliders (and, for feel, the tempo) rather than the net
//! (`MasterMacro.swift`).
//!
//! Ported: what reaches wave audio when beats routes the wave engine through Tone
//! (`&fx=tone`, lib/backend/wave.js `connectOutput`): the master inserts HP, LP, phaser, bit
//! crusher, distortion and pitch shift, and the tempo. NOT ported, because nothing of theirs
//! reaches wave audio in beats: `delay-throw`, `cathedral`, `dub-delay`, the Pan macros and the
//! Shape macros. Two macros are ported in part, for the same reason: `reverb-wash` keeps its
//! HP move and `riser` its HP and phaser moves.
//!
//! The catalog data here is checked against beats' own (`macro-fx.json` `meta.catalog`).

use crate::json::js_round;

/// One of beats' master FX sliders (lib/ui/build.js, `.pn-fx-slider[data-fx=…]`): an integer
/// position on the slider's own range, mapped to the engine by `apply`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MasterFxKey {
    HpFreq,
    LpFreq,
    PhaserFreq,
    PhaserDepth,
    PhaserWet,
    CrushBits,
    Distortion,
    MasterPitch,
}

impl MasterFxKey {
    pub const ALL: [MasterFxKey; 8] = [
        MasterFxKey::HpFreq,
        MasterFxKey::LpFreq,
        MasterFxKey::PhaserFreq,
        MasterFxKey::PhaserDepth,
        MasterFxKey::PhaserWet,
        MasterFxKey::CrushBits,
        MasterFxKey::Distortion,
        MasterFxKey::MasterPitch,
    ];

    /// `rawValue`.
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Option<MasterFxKey> {
        Self::ALL.get(i).copied()
    }

    pub fn id(self) -> &'static str {
        match self {
            MasterFxKey::HpFreq => "hp-freq",
            MasterFxKey::LpFreq => "lp-freq",
            MasterFxKey::PhaserFreq => "phaser-freq",
            MasterFxKey::PhaserDepth => "phaser-depth",
            MasterFxKey::PhaserWet => "phaser-wet",
            MasterFxKey::CrushBits => "crush-bits",
            MasterFxKey::Distortion => "distortion",
            MasterFxKey::MasterPitch => "master-pitch",
        }
    }

    pub fn from_id(id: &str) -> Option<MasterFxKey> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }

    /// The `<input type="range">`'s `min` (build.js).
    pub fn min(self) -> i32 {
        if self == MasterFxKey::MasterPitch {
            -12
        } else {
            0
        }
    }

    /// The `<input type="range">`'s `max` (build.js).
    pub fn max(self) -> i32 {
        if self == MasterFxKey::MasterPitch {
            12
        } else {
            100
        }
    }

    /// `data-default` (build.js), also the FX Reset / Feel Reset target (`FX_HARD_DEFAULTS`).
    pub fn default_value(self) -> i32 {
        match self {
            MasterFxKey::LpFreq => 100,
            MasterFxKey::PhaserDepth => 50,
            _ => 0,
        }
    }

    /// A slider position, 0...1 of its travel (a NaN or infinite one is 0), in the slider's own
    /// units: its range, rounded. What a controller's master FX fader and `DeckAction.masterFx`
    /// both set (`MasterFxKey.sliderValue`).
    pub fn slider_value(self, position: f64) -> i64 {
        let (min, max) = (self.min() as f64, self.max() as f64);
        let p = if position.is_finite() {
            position.clamp(0.0, 1.0)
        } else {
            0.0
        };
        libm::round(min + p * (max - min)) as i64
    }

    /// What the browser does to `slider.value = Math.round(v)`: clamp to the range (the step
    /// is 1 and the value already whole).
    pub fn sanitize(self, v: f64) -> i32 {
        let r = js_round(v);
        if !r.is_finite() {
            return self.max().min(self.default_value()).max(self.min());
        }
        r.min(self.max() as f64).max(self.min() as f64) as i32
    }
}

/// A master-chain macro (`lib/macros/catalog.js`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MasterMacro {
    SweepLp,
    SweepHp,
    ReverbWash,
    Riser,
    BuildCrush,
    PhaserDrone,
    FilterRes,
    OctaveUp,
    OctaveDown,
    PitchBend,
    VinylBrake,
    FeelChill,
    FeelDrive,
    FeelAmbient,
    FeelEuphoric,
    FeelBuild,
    FeelWindDown,
    FeelReset,
}

/// One slider move of an FX macro: `{ fxKey, toValue }`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Op {
    pub key: MasterFxKey,
    pub to: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// `fx-sweep`: ramp to the value over 80 % of the duration, back over the rest.
    Sweep(&'static [Op]),
    /// `fx-hold`: jump to the value, hold, ramp back over the last `tail_frac` of it.
    Hold(&'static [Op], f64),
    /// `compound` (only `vinyl-brake`): sub-macros at offsets.
    Compound,
    /// `feel-snap`: the Feel puck to a corner for the duration.
    FeelSnap { x: f64, y: f64 },
    /// `feel-sweep`: ease the puck to a point, then snap back.
    FeelSweep { x: f64, y: f64 },
    /// `genre-reset`: ease every FX slider to its default and the tempo to the genre's.
    GenreReset,
}

const fn op(key: MasterFxKey, to: f64) -> Op {
    Op { key, to }
}

static SWEEP_LP: [Op; 1] = [op(MasterFxKey::LpFreq, 5.0)];
static SWEEP_HP: [Op; 1] = [op(MasterFxKey::HpFreq, 80.0)];
// beats: reverb-wet 95, reverb-size 90, reverb-damp 15 (dropped: no reverb on wave audio)
static REVERB_WASH: [Op; 1] = [op(MasterFxKey::HpFreq, 30.0)];
// beats: reverb-wet 80 between the two (dropped)
static RISER: [Op; 2] = [
    op(MasterFxKey::HpFreq, 70.0),
    op(MasterFxKey::PhaserWet, 60.0),
];
static BUILD_CRUSH: [Op; 2] = [
    op(MasterFxKey::CrushBits, 80.0),
    op(MasterFxKey::Distortion, 20.0),
];
static PHASER_DRONE: [Op; 3] = [
    op(MasterFxKey::PhaserWet, 90.0),
    op(MasterFxKey::PhaserDepth, 100.0),
    op(MasterFxKey::PhaserFreq, 40.0),
];
static FILTER_RES: [Op; 2] = [
    op(MasterFxKey::LpFreq, 40.0),
    op(MasterFxKey::Distortion, 30.0),
];
static OCTAVE_UP: [Op; 1] = [op(MasterFxKey::MasterPitch, 12.0)];
static OCTAVE_DOWN: [Op; 1] = [op(MasterFxKey::MasterPitch, -12.0)];
static PITCH_BEND: [Op; 1] = [op(MasterFxKey::MasterPitch, 7.0)];

impl MasterMacro {
    pub const ALL: [MasterMacro; 18] = [
        MasterMacro::SweepLp,
        MasterMacro::SweepHp,
        MasterMacro::ReverbWash,
        MasterMacro::Riser,
        MasterMacro::BuildCrush,
        MasterMacro::PhaserDrone,
        MasterMacro::FilterRes,
        MasterMacro::OctaveUp,
        MasterMacro::OctaveDown,
        MasterMacro::PitchBend,
        MasterMacro::VinylBrake,
        MasterMacro::FeelChill,
        MasterMacro::FeelDrive,
        MasterMacro::FeelAmbient,
        MasterMacro::FeelEuphoric,
        MasterMacro::FeelBuild,
        MasterMacro::FeelWindDown,
        MasterMacro::FeelReset,
    ];

    /// `rawValue`.
    pub fn raw(self) -> u8 {
        self as u8
    }

    pub fn from_raw(r: u8) -> Option<MasterMacro> {
        Self::ALL.get(r as usize).copied()
    }

    pub fn id(self) -> &'static str {
        use MasterMacro::*;
        match self {
            SweepLp => "sweep-lp",
            SweepHp => "sweep-hp",
            ReverbWash => "reverb-wash",
            Riser => "riser",
            BuildCrush => "build-crush",
            PhaserDrone => "phaser-drone",
            FilterRes => "filter-res",
            OctaveUp => "octave-up",
            OctaveDown => "octave-down",
            PitchBend => "pitch-bend",
            VinylBrake => "vinyl-brake",
            FeelChill => "feel-chill",
            FeelDrive => "feel-drive",
            FeelAmbient => "feel-ambient",
            FeelEuphoric => "feel-euphoric",
            FeelBuild => "feel-build",
            FeelWindDown => "feel-wind-down",
            FeelReset => "feel-reset",
        }
    }

    pub fn label(self) -> &'static str {
        use MasterMacro::*;
        match self {
            SweepLp => "Sweep LP",
            SweepHp => "Sweep HP",
            ReverbWash => "Reverb Wash",
            Riser => "Riser",
            BuildCrush => "Bit Crush",
            PhaserDrone => "Phaser Drone",
            FilterRes => "Res Ping",
            OctaveUp => "Octave Up",
            OctaveDown => "Octave Down",
            PitchBend => "Pitch Bend",
            VinylBrake => "Vinyl Brake",
            FeelChill => "Chill",
            FeelDrive => "Drive",
            FeelAmbient => "Ambient",
            FeelEuphoric => "Euphoric",
            FeelBuild => "Build Up",
            FeelWindDown => "Wind Down",
            FeelReset => "Reset",
        }
    }

    pub fn group(self) -> &'static str {
        use MasterMacro::*;
        match self {
            SweepLp | SweepHp | ReverbWash | Riser | BuildCrush | PhaserDrone | FilterRes => "FX",
            OctaveUp | OctaveDown | PitchBend | VinylBrake => "Pitch",
            _ => "Feel",
        }
    }

    /// Bars (every one of these is in bars).
    pub fn default_duration(self) -> i32 {
        use MasterMacro::*;
        match self {
            SweepLp | Riser | FeelBuild | FeelWindDown | FeelReset => 4,
            OctaveUp | OctaveDown | VinylBrake => 1,
            _ => 2,
        }
    }

    pub fn duration_options(self) -> &'static [i32] {
        use MasterMacro::*;
        match self {
            Riser | FeelBuild | FeelWindDown => &[2, 4, 8],
            FilterRes | VinylBrake => &[1, 2],
            OctaveUp | OctaveDown | PitchBend => &[1, 2, 4],
            FeelReset => &[2, 4, 8, 16],
            _ => &[1, 2, 4, 8],
        }
    }

    pub fn kind(self) -> Kind {
        use MasterMacro::*;
        match self {
            SweepLp => Kind::Sweep(&SWEEP_LP),
            SweepHp => Kind::Sweep(&SWEEP_HP),
            ReverbWash => Kind::Hold(&REVERB_WASH, 0.55),
            Riser => Kind::Sweep(&RISER),
            BuildCrush => Kind::Sweep(&BUILD_CRUSH),
            // no tailFrac in the catalog: fxHold's default, 0.6
            PhaserDrone => Kind::Hold(&PHASER_DRONE, 0.6),
            FilterRes => Kind::Sweep(&FILTER_RES),
            OctaveUp => Kind::Hold(&OCTAVE_UP, 0.3),
            OctaveDown => Kind::Hold(&OCTAVE_DOWN, 0.3),
            PitchBend => Kind::Sweep(&PITCH_BEND),
            VinylBrake => Kind::Compound,
            FeelChill => Kind::FeelSnap { x: 0.0, y: 0.0 },
            FeelDrive => Kind::FeelSnap { x: 1.0, y: 0.0 },
            FeelAmbient => Kind::FeelSnap { x: 0.0, y: 1.0 },
            FeelEuphoric => Kind::FeelSnap { x: 1.0, y: 1.0 },
            FeelBuild => Kind::FeelSweep { x: 0.85, y: 0.85 },
            FeelWindDown => Kind::FeelSweep { x: 0.25, y: 0.30 },
            FeelReset => Kind::GenreReset,
        }
    }

    pub fn from_id(id: &str) -> Option<MasterMacro> {
        Self::ALL.iter().copied().find(|m| m.id() == id)
    }

    /// Its bit in `WaveDeckSource.activeMasterMacros`' mask.
    pub fn bit(self) -> u32 {
        1u32 << (self as u32)
    }
}
