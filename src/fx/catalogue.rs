//! The performance catalogue: every trick and macro a player can fire, once, in one category.
//!
//! The engine has four families (the tricks, the song's own net macros, the master-chain macros
//! and the pan / shape / aux macros), and they grew by overlap: three ways to stop a tape, two
//! to throw a delay, two risers. This is the list the apps show. An id that is not here is not
//! offered (the families keep the DSP; ids merged away are simply gone).
//!
//! Each entry says how it may be engaged ([`Engagement`](crate::host::deck::Engagement)):
//! a [`Category::Transition`] is a momentary one-shot; the rest run for 1, 2, 3 or 4 bars,
//! or latch on, except the [`Category::Sweeps`], which have bars but no latch (they end by
//! themselves: an ease and a snap back).

use super::aux_macro::AuxMacro;
use super::master_macro::MasterMacro;
use super::tricks::TrickKind;
use crate::wave::macros::NetMacro;

/// Where an entry sits in the panel. Nothing sits in two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Short one-shots: always momentary.
    Transition,
    FilterTone,
    Space,
    Rhythm,
    Arrange,
    Pitch,
    StereoShape,
    Feel,
    /// Eases that end by themselves: bar lengths, no latch.
    Sweeps,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Transition,
        Category::FilterTone,
        Category::Space,
        Category::Rhythm,
        Category::Arrange,
        Category::Pitch,
        Category::StereoShape,
        Category::Feel,
        Category::Sweeps,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Category::Transition => "transition",
            Category::FilterTone => "filter-tone",
            Category::Space => "space",
            Category::Rhythm => "rhythm",
            Category::Arrange => "arrange",
            Category::Pitch => "pitch",
            Category::StereoShape => "stereo-shape",
            Category::Feel => "feel",
            Category::Sweeps => "sweeps",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Transition => "Transition",
            Category::FilterTone => "Filter & tone",
            Category::Space => "Space",
            Category::Rhythm => "Rhythm",
            Category::Arrange => "Arrange",
            Category::Pitch => "Pitch",
            Category::StereoShape => "Stereo & shape",
            Category::Feel => "Feel",
            Category::Sweeps => "Sweeps",
        }
    }

    /// A one-shot: momentary, no bar length, no latch.
    pub fn is_transition(self) -> bool {
        self == Category::Transition
    }

    /// May an entry of this category stay on until it is let go?
    pub fn latches(self) -> bool {
        !matches!(self, Category::Transition | Category::Sweeps)
    }
}

/// Which engine family fires it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Trick(TrickKind),
    Net(NetMacro),
    Master(MasterMacro),
    Aux(AuxMacro),
}

/// One thing a player can fire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    /// What [`Engine::engage`](crate::host::deck::Engine::engage) takes: the trick's name or
    /// the macro's id.
    pub id: &'static str,
    pub label: &'static str,
    pub category: Category,
    pub family: Family,
}

impl Entry {
    pub fn transition(&self) -> bool {
        self.category.is_transition()
    }

    pub fn latchable(&self) -> bool {
        self.category.latches()
    }

    /// Needs a net deck's parts (a pan, shape or song macro).
    pub fn needs_net(&self) -> bool {
        match self.family {
            Family::Net(_) => true,
            Family::Aux(m) => !matches!(m.kind(), super::aux_macro::AuxKind::Hold { .. }),
            _ => false,
        }
    }
}

const fn e(id: &'static str, label: &'static str, category: Category, family: Family) -> Entry {
    Entry { id, label, category, family }
}

use Category::*;
use Family::{Aux, Master, Net, Trick};

/// The catalogue, in panel order.
pub static ENTRIES: [Entry; 39] = [
    // transitions
    e("riser", "Riser", Transition, Trick(TrickKind::Riser)),
    e("tape_stop", "Tape Stop", Transition, Trick(TrickKind::TapeStop)),
    e("spin_back", "Spin Back", Transition, Trick(TrickKind::SpinBack)),
    e("verb_freeze", "Reverb Freeze", Transition, Trick(TrickKind::VerbFreeze)),
    e("cut", "Cut", Transition, Net(NetMacro::Cut)),
    e("double-drop", "Double Drop", Transition, Net(NetMacro::DoubleDrop)),
    // filter and tone
    e("filter", "Filter", FilterTone, Trick(TrickKind::Filter)),
    e("filter-res", "Res Ping", FilterTone, Master(MasterMacro::FilterRes)),
    e("build-crush", "Bit Crush", FilterTone, Master(MasterMacro::BuildCrush)),
    e("phaser-drone", "Phaser Drone", FilterTone, Master(MasterMacro::PhaserDrone)),
    // space
    e("delay", "Delay", Space, Trick(TrickKind::Delay)),
    e("wash", "Wash", Space, Trick(TrickKind::Wash)),
    e("cathedral", "Cathedral", Space, Aux(AuxMacro::Cathedral)),
    // rhythm
    e("stutter", "Stutter", Rhythm, Trick(TrickKind::Stutter)),
    e("pump", "Pump", Rhythm, Trick(TrickKind::Pump)),
    e("beat-repeat", "Gate Chop", Rhythm, Net(NetMacro::BeatRepeat)),
    e("half_time", "Half Time", Rhythm, Trick(TrickKind::HalfTime)),
    e("double_time", "Double Time", Rhythm, Trick(TrickKind::DoubleTime)),
    e("tempo-anchor", "Anchor", Rhythm, Net(NetMacro::TempoAnchor)),
    // arrange
    e("drop", "Drums Only", Arrange, Net(NetMacro::Drop)),
    e("breakdown", "Breakdown", Arrange, Net(NetMacro::Breakdown)),
    // pitch
    e("octave-up", "Octave Up", Pitch, Master(MasterMacro::OctaveUp)),
    e("octave-down", "Octave Down", Pitch, Master(MasterMacro::OctaveDown)),
    e("pitch-bend", "Pitch Bend", Pitch, Master(MasterMacro::PitchBend)),
    // stereo and shape
    e("ping-pong", "Ping-Pong", StereoShape, Aux(AuxMacro::PingPong)),
    e("auto-pan", "Auto-Pan", StereoShape, Aux(AuxMacro::AutoPan)),
    e("mono", "Mono", StereoShape, Aux(AuxMacro::Mono)),
    e("hard-left", "Hard Left", StereoShape, Aux(AuxMacro::HardLeft)),
    e("hard-right", "Hard Right", StereoShape, Aux(AuxMacro::HardRight)),
    e("tighten", "Tighten", StereoShape, Aux(AuxMacro::Tighten)),
    e("loosen", "Loosen", StereoShape, Aux(AuxMacro::Loosen)),
    e("pulse", "Pulse", StereoShape, Aux(AuxMacro::Pulse)),
    // feel
    e("feel-chill", "Chill", Feel, Master(MasterMacro::FeelChill)),
    e("feel-drive", "Drive", Feel, Master(MasterMacro::FeelDrive)),
    e("feel-ambient", "Ambient", Feel, Master(MasterMacro::FeelAmbient)),
    e("feel-euphoric", "Euphoric", Feel, Master(MasterMacro::FeelEuphoric)),
    // sweeps: bar lengths, no latch
    e("feel-build", "Build Up", Sweeps, Master(MasterMacro::FeelBuild)),
    e("feel-wind-down", "Wind Down", Sweeps, Master(MasterMacro::FeelWindDown)),
    e("feel-reset", "Reset", Sweeps, Master(MasterMacro::FeelReset)),
];

/// The entry for an id, if it is offered.
pub fn find(id: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|x| x.id == id)
}

/// The entries of one category, in panel order.
pub fn in_category(c: Category) -> impl Iterator<Item = &'static Entry> {
    ENTRIES.iter().filter(move |x| x.category == c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_entry_names_its_family_id() {
        for (i, a) in ENTRIES.iter().enumerate() {
            assert!(ENTRIES[i + 1..].iter().all(|b| b.id != a.id), "{} twice", a.id);
            let own = match a.family {
                Family::Trick(k) => k.name(),
                Family::Net(m) => m.id(),
                Family::Master(m) => m.id(),
                Family::Aux(m) => m.id(),
            };
            assert_eq!(own, a.id);
        }
    }

    #[test]
    fn each_family_member_is_in_one_place_or_merged_away() {
        let gone = [
            // merged away: the same engine code or the same sound as something kept
            "solo-drums", "half-time", "tape-stop", "vinyl-brake", "reverb-wash", "sweep-lp",
            "sweep-hp", "riser", "delay-throw", "dub-delay",
        ];
        for m in NetMacro::ALL {
            let n = ENTRIES.iter().filter(|x| x.family == Family::Net(m)).count();
            assert_eq!(n, !gone.contains(&m.id()) as usize, "{}", m.id());
        }
        for m in MasterMacro::ALL {
            if m == MasterMacro::Riser {
                // the noise riser (a trick) is the one riser
                assert!(find("riser").is_some_and(|x| matches!(x.family, Family::Trick(_))));
                continue;
            }
            let n = ENTRIES.iter().filter(|x| x.family == Family::Master(m)).count();
            assert_eq!(n, !gone.contains(&m.id()) as usize, "{}", m.id());
        }
        for m in AuxMacro::ALL {
            let n = ENTRIES.iter().filter(|x| x.family == Family::Aux(m)).count();
            assert_eq!(n, !gone.contains(&m.id()) as usize, "{}", m.id());
        }
        for k in TrickKind::ALL.iter().filter(|k| !k.is_gear()) {
            assert_eq!(ENTRIES.iter().filter(|x| x.family == Family::Trick(*k)).count(), 1, "{}", k.name());
        }
    }

    #[test]
    fn transitions_do_not_latch_and_sweeps_have_no_latch() {
        for x in ENTRIES.iter() {
            assert_eq!(x.transition(), x.category == Category::Transition);
            if x.category == Category::Sweeps || x.transition() {
                assert!(!x.latchable(), "{}", x.id);
            } else {
                assert!(x.latchable(), "{}", x.id);
            }
        }
    }
}
