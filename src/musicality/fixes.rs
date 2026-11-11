//! `DJMusicality.Fixes`: the option set of dj's pass over a composed track. Bit positions
//! are Swift's, so a raw value recorded by the app means the same flags here.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{BitAnd, BitOr, BitOrAssign, Not};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Fixes(pub u64);

macro_rules! fix {
    ($($(#[$m:meta])* $name:ident = $bit:expr, $s:expr;)*) => {
        impl Fixes {
            $($(#[$m])* pub const $name: Fixes = Fixes(1 << $bit);)*
            /// Each fix by the name `djctl render --fixes` takes, in bit order.
            pub const NAMED: &'static [(&'static str, Fixes)] = &[$(($s, Fixes::$name),)*];
        }
    };
}

fix! {
    /// Tension variants back in the track's key.
    KEY = 0, "key";
    /// Structure changes on the beat they were composed for.
    TIMING = 1, "timing";
    /// Each part in its own octave.
    REGISTER = 2, "register";
    /// Every track at about one loudness (applied by the runner).
    LOUDNESS = 3, "loudness";
    /// The project's swing and humanize (runner).
    GROOVE = 4, "groove";
    /// Accents by meter, melody peaks leaned on.
    DYNAMICS = 5, "dynamics";
    /// The bass off the kick in four-on-the-floor genres.
    OFFBEAT_BASS = 6, "offbeatBass";
    /// Phrase changes on the 2 / 4-bar grid.
    PHRASES = 7, "phrases";
    /// A snare roll into every section change.
    FILLS = 8, "fills";
    /// The pad voiced for least motion.
    VOICING = 9, "voicing";
    /// The arrangement's energy arc (runner).
    ENERGY = 10, "energy";
    /// Sidechain pump on bass and pad (runner).
    DUCK = 11, "duck";
    /// Fewer repeated melody notes.
    CONTOUR = 12, "contour";
    /// Sevenths in jazz, bossa, house, garage and funk.
    COLOR = 13, "color";
    /// The arp plays the chords.
    ARPEGGIO = 14, "arpeggio";
    /// A buildup builds: a snare roll that doubles its rate.
    BUILDUP = 15, "buildup";
    /// A crash on every section downbeat after the first.
    CRASH = 16, "crash";
    /// The bass moves to each chord root by the shorter way.
    BASS_LINE = 17, "bassLine";
    /// Open hats on the off-beat eighths.
    OPEN_HATS = 18, "openHats";
    /// A stereo picture (runner).
    STEREO = 19, "stereo";
    /// The melody sings: notes last until the next.
    LEGATO = 20, "legato";
    /// Phrases end at rest on a chord tone.
    CADENCE = 21, "cadence";
    /// The bass leads into each chord change.
    APPROACH = 22, "approach";
    /// The breath before the drop (runner).
    DROP_GAP = 23, "dropGap";
    /// A closed hat chokes the open hat (runner).
    CHOKE = 24, "choke";
    /// A send reverb by genre (runner).
    SPACE = 25, "space";
    /// Band-limited saw / square with a filter envelope (runner).
    SUBTRACTIVE = 26, "subtractive";
    /// Pads and saw leads as detuned pairs (runner).
    UNISON = 27, "unison";
    /// Tempo-synced ping-pong echo (runner).
    ECHO = 28, "echo";
    /// The kick's body tuned to the key (runner).
    KICK_TUNE = 29, "kickTune";
    /// Each 4-bar phrase states its idea twice.
    MOTIF = 30, "motif";
    /// The parts balanced against each other (runner).
    BALANCE = 31, "balance";
    /// A track of its own per seed (`TrackVariety`).
    VARIETY = 32, "variety";
    /// The pad cycle's last bar resolves into its first.
    HARMONIC_CADENCE = 33, "harmonicCadence";
    /// Per-section harmony.
    SECTION_HARMONY = 34, "sectionHarmony";
    /// Two bars per chord in quiet sections (runner, through `SectionHarmony`).
    HARMONIC_RHYTHM = 35, "harmonicRhythm";
    /// 8-bar question and answer.
    ANSWER = 36, "answer";
    /// One seeded chord-tone hook in every melodic section.
    HOOK = 37, "hook";
    /// Contour by corpus, leap recovery, a peak at two-thirds.
    SHAPE = 38, "shape";
    /// Bass bar 2 echoes bar 1's rhythm.
    BASS_MOTIF = 39, "bassMotif";
    /// Per-genre arp figures.
    ARP_STYLE = 40, "arpStyle";
    /// The arp's rate follows the phrase's density.
    ARP_DENSITY = 41, "arpDensity";
    /// Kick and snare in chill / jazz verse and chorus.
    BACKBEAT = 42, "backbeat";
    /// The family's groove template (runner).
    FEEL = 43, "feel";
    /// Humanize as seeded 1/f noise (runner).
    DRIFT = 44, "drift";
    /// Voices' filter envelopes open through a buildup (runner).
    SWEEP = 45, "sweep";
    /// At most N melodic parts per section (runner rests).
    LAYER_BUDGET = 46, "layerBudget";
    /// Song-family band stop before a chorus (runner).
    BAND_STOP = 47, "bandStop";
    /// Kick out for the last bar of a buildup (runner).
    KICK_OUT = 48, "kickOut";
    /// A noise riser into drops (runner).
    RISER = 49, "riser";
    /// A sub-octave sine under dnb / dubstep / trap basses (runner).
    SUB_OCTAVE = 50, "subOctave";
    /// HF tilt at the offending voices (runner).
    HF_TILT = 51, "hfTilt";
    /// Pad shimmer and room level (runner).
    SHIMMER = 52, "shimmer";
}

impl Fixes {
    pub const EMPTY: Fixes = Fixes(0);

    /// `Fixes.all`: every flag.
    pub const ALL: Fixes = Fixes((1 << 53) - 1);

    /// `Fixes.runnerOnly`: the fixes that live wholly in the runner, what a deck whose nets
    /// the pass never saw (a raw-`nets` share) still plays with.
    pub const RUNNER_ONLY: Fixes = Fixes(
        Fixes::LOUDNESS.0
            | Fixes::GROOVE.0
            | Fixes::ENERGY.0
            | Fixes::DUCK.0
            | Fixes::STEREO.0
            | Fixes::DROP_GAP.0
            | Fixes::CHOKE.0
            | Fixes::SPACE.0
            | Fixes::SUBTRACTIVE.0
            | Fixes::UNISON.0
            | Fixes::ECHO.0
            | Fixes::KICK_TUNE.0
            | Fixes::BALANCE.0
            | Fixes::FEEL.0
            | Fixes::DRIFT.0
            | Fixes::SWEEP.0
            | Fixes::LAYER_BUDGET.0
            | Fixes::BAND_STOP.0
            | Fixes::KICK_OUT.0
            | Fixes::RISER.0
            | Fixes::SUB_OCTAVE.0
            | Fixes::HF_TILT.0
            | Fixes::SHIMMER.0,
    );

    pub fn contains(self, other: Fixes) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `Fixes.names`: the flags in this set, in bit order (what `dj_state` reports).
    pub fn names(self) -> Vec<String> {
        Fixes::NAMED
            .iter()
            .filter(|(_, f)| self.contains(*f))
            .map(|(n, _)| String::from(*n))
            .collect()
    }

    /// A fix by its `named` name; `"none"` is the empty set.
    pub fn named(name: &str) -> Option<Fixes> {
        if name == "none" {
            return Some(Fixes::EMPTY);
        }
        Fixes::NAMED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| *f)
    }
}

impl BitOr for Fixes {
    type Output = Fixes;
    fn bitor(self, o: Fixes) -> Fixes {
        Fixes(self.0 | o.0)
    }
}
impl BitOrAssign for Fixes {
    fn bitor_assign(&mut self, o: Fixes) {
        self.0 |= o.0;
    }
}
impl BitAnd for Fixes {
    type Output = Fixes;
    fn bitand(self, o: Fixes) -> Fixes {
        Fixes(self.0 & o.0)
    }
}
impl Not for Fixes {
    type Output = Fixes;
    fn not(self) -> Fixes {
        Fixes(!self.0 & Fixes::ALL.0)
    }
}
