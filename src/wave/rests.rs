//! Port of `Beats/Wave/RestSchedule.swift`: when which lanes of a net deck rest.
//!
//! The runner's arrangement gestures (the drop gap, and 6.2's layer budget, band stop and
//! kick out), built at prepare and read on the audio thread. A lane rests while a step's
//! class mask shares a bit with its [`Class`]; its nets keep running, only its hits are
//! dropped. The gestures themselves are `DJMusicality` (not ported yet): a host gives the
//! runner the schedule (`WaveOptions::rests`), and the mix-out tail / mix-in rests are
//! built here.
//!
//! `entries` are disjoint and ascending ([`RestSchedule::merging`] merges overlapping
//! gestures into one mask per stretch), so the runner reads them with a cursor that only
//! moves forward between jumps ([`Cursor`]): no search, no allocation.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

/// What a lane is, for resting: one bit per kind. A lane is one class (or none: crashes,
/// stingers, overrides and the live keys never rest; a fill chain's drums are `FILL`,
/// which only `.bandStop` rests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Class(pub u32);

impl Class {
    pub const NONE: Class = Class(0);
    /// The kick drum.
    pub const KICK: Class = Class(1 << 0);
    /// Every other drum of a net's own kit (snare, hats, open hat, toms…), not fills or
    /// crashes.
    pub const DRUMS: Class = Class(1 << 1);
    /// Tonal lanes by part (`Track.group`, else the net's `riffGroup`).
    pub const BASS: Class = Class(1 << 2);
    pub const HARMONY: Class = Class(1 << 3);
    pub const MELODY: Class = Class(1 << 4);
    pub const ARP: Class = Class(1 << 5);
    /// Any other tonal part.
    pub const TONAL: Class = Class(1 << 6);
    /// The drums of a fill chain (`fill-N`: fills, buildup hits), not its crashes.
    pub const FILL: Class = Class(1 << 7);

    /// The rhythm section `.dropGap` stops: kick, drums, bass.
    pub const RHYTHM: Class = Class(Self::KICK.0 | Self::DRUMS.0 | Self::BASS.0);
    pub const MELODIC: Class =
        Class(Self::HARMONY.0 | Self::MELODY.0 | Self::ARP.0 | Self::TONAL.0);

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn union(self, o: Class) -> Class {
        Class(self.0 | o.0)
    }
    pub fn is_disjoint(self, o: Class) -> bool {
        self.0 & o.0 == 0
    }

    /// The class of a lane: `kind` is its drum kind or "tonal", `group` its part, `net`
    /// the net.
    pub fn of(kind: &str, group: &str, net: &str) -> Class {
        if kind == "tonal" {
            return match group {
                "bass" => Class::BASS,
                "harmony" => Class::HARMONY,
                "melody" => Class::MELODY,
                "arp" => Class::ARP,
                _ => Class::TONAL,
            };
        }
        if kind == "crash" {
            return Class::NONE;
        }
        if net.starts_with("fill-") {
            return Class::FILL;
        }
        if kind == "kick" {
            Class::KICK
        } else {
            Class::DRUMS
        }
    }
}

/// Steps `from ..< to` (sixteenths of the song from 0), during which `mask`'s lanes rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Entry {
    pub from: i64,
    pub to: i64,
    pub mask: Class,
}

impl Entry {
    pub fn new(from: i64, to: i64, mask: Class) -> Entry {
        Entry { from, to, mask }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct RestSchedule {
    /// Disjoint, ascending, none empty.
    pub entries: Vec<Entry>,
}

impl RestSchedule {
    pub fn empty() -> RestSchedule {
        RestSchedule::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `gestures` made disjoint: where they overlap, one entry with both masks. Empty and
    /// mask-less entries are dropped; adjacent entries with the same mask stay apart.
    pub fn merging(gestures: &[Entry]) -> RestSchedule {
        let live: Vec<&Entry> = gestures
            .iter()
            .filter(|e| e.to > e.from && !e.mask.is_empty())
            .collect();
        let mut cuts = BTreeSet::new();
        for e in &live {
            cuts.insert(e.from);
            cuts.insert(e.to);
        }
        let points: Vec<i64> = cuts.into_iter().collect();
        let mut out = Vec::new();
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut mask = Class::NONE;
            for e in &live {
                if e.from <= a && e.to >= b {
                    mask = mask.union(e.mask);
                }
            }
            if !mask.is_empty() {
                out.push(Entry::new(a, b, mask));
            }
        }
        RestSchedule { entries: out }
    }

    /// What rests at `step`, by a plain scan (tests, off the audio thread).
    pub fn mask(&self, step: i64) -> Class {
        self.entries
            .iter()
            .find(|e| e.from <= step && step < e.to)
            .map_or(Class::NONE, |e| e.mask)
    }

    /// This schedule with a mix-out tail's rest on top (`MixOutTail.entry`).
    pub fn with_tail(&self, tail: &MixOutTail) -> RestSchedule {
        self.with_rest(tail.entry())
    }

    /// This schedule with one more rest on top (a mix-in's, [`RestSchedule::mix_in`]).
    pub fn with_rest(&self, rest: Entry) -> RestSchedule {
        let mut all = self.entries.clone();
        all.push(rest);
        RestSchedule::merging(&all)
    }

    /// A mix-in (ROADMAP 6.2, `Deck.mixIn`): the deck comes in drums-forward, its melodic
    /// lanes (the mix-out tail's mask) resting from the bar line `from_tick` for `bars`
    /// bars, released on the bar line after.
    pub fn mix_in(from_tick: i64, bars: i64) -> Entry {
        Entry::new(
            from_tick,
            from_tick + bars.max(0) * 16,
            MixOutTail::REST_MASK,
        )
    }
}

/// What rests at `step`, for a reader that mostly moves forward one step at a time. Moving
/// back (a stop, a jump) re-seeks from the top. Allocation-free.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Cursor {
    index: usize,
}

impl Cursor {
    pub fn new() -> Cursor {
        Cursor::default()
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }

    pub fn mask(&mut self, step: i64, s: &RestSchedule) -> Class {
        let e = &s.entries;
        if self.index > e.len() || (self.index > 0 && e[self.index - 1].to > step) {
            self.index = 0;
        }
        while self.index < e.len() && e[self.index].to <= step {
            self.index += 1;
        }
        if self.index < e.len() && e[self.index].from <= step {
            e[self.index].mask
        } else {
            Class::NONE
        }
    }
}

/// A mix-out tail (ROADMAP 6.2, performable form; `WaveOptions.tail`): from `from_tick`
/// the deck plays only its drums-forward lanes (kick, drums, bass: the melodic classes
/// rest) for `bars` bars, plays on past the song's own `stop-transport`, and stops on
/// `end_tick`. `from_tick` is a bar line (the caller picks it), so `end_tick` is one too.
/// Fills, crashes, stingers and the live keys play on, as they do through every rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct MixOutTail {
    pub from_tick: i64,
    pub bars: i64,
}

impl MixOutTail {
    /// How long a mix-out plays when nobody says (`DeckAction.mixOut`): 16 bars.
    pub const DEFAULT_BARS: i64 = 16;
    /// What rests in a tail (and a mix-in): everything but the drums-forward lanes.
    pub const REST_MASK: Class = Class::MELODIC;

    pub fn new(from_tick: i64, bars: i64) -> MixOutTail {
        MixOutTail { from_tick, bars }
    }

    /// The tick the deck stops on: `bars` bars after `from_tick`.
    pub fn end_tick(&self) -> i64 {
        self.from_tick + self.bars.max(0) * 16
    }

    /// What rests: the melodic lanes, from `from_tick` to the stop.
    pub fn entry(&self) -> Entry {
        Entry::new(self.from_tick, self.end_tick(), Self::REST_MASK)
    }
}
