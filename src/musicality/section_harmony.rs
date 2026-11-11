//! Port of `Musicality/SectionHarmony.swift`: a per-section diatonic shift of the band
//! (ROADMAP 6.3): what the runner plays the pad / bass / arp / melody lanes through so the
//! parts move together when a section changes harmony. `.harmonicRhythm` fills the slots.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gen::core::scale_intervals;
use crate::musicality::dj::pad_chords;
use crate::musicality::fixes::Fixes;
use crate::musicality::note_log::group_name;
use crate::musicality::track_variety::seven_note_key;
use crate::musicality::util::pc;
use crate::pflow::Project;
use crate::wave::synth::is_drum_channel;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct SectionHarmony {
    /// First step of each section, ascending.
    pub start: Vec<i64>,
    /// The step after the last section.
    pub end: i64,
    /// Each section's degree shift (`.sectionHarmony`; all 0 without it).
    pub degrees: Vec<i64>,
    /// The tonal groups the maps cover, in net order.
    pub groups: Vec<String>,
    /// The distinct non-zero shifts the slots refer to (slot value k ≥ 1 is `shifts[k - 1]`).
    pub shifts: Vec<i64>,
    /// `SLOTS_PER_CYCLE` entries per section: 0 = home, k = `shifts[k - 1]`.
    pub slots: Vec<u8>,
    /// `TAIL_SLOTS` entries per section, its last bar's half-bars, or `NO_TAIL`.
    pub tail: Vec<u8>,
    /// The degree shift each group actually plays per shift, octave chosen.
    pub group_degrees: Vec<i64>,
    /// 128 notes per (shift, group), same index order as `group_degrees`.
    pub maps: Vec<u8>,
}

impl SectionHarmony {
    /// Half-bar slots in a 4-bar cycle, and their length in steps.
    pub const SLOTS_PER_CYCLE: i64 = 8;
    pub const SLOT_STEPS: i64 = 8;
    /// Half-bar slots at the end of each section that `tail` can override.
    pub const TAIL_SLOTS: i64 = 2;
    /// A `tail` entry that leaves the cycle's slot to play.
    pub const NO_TAIL: u8 = u8::MAX;
    /// Sections that hold each chord 2 bars under `.harmonicRhythm`.
    pub const HOLDING_SECTIONS: [&'static str; 3] = ["intro", "breakdown", "outro"];
    const FLOOR_NOTE: i64 = 28;
    const CEILING_NOTE: i64 = 108;

    /// `sectionDegrees`: degree shifts by section name (a breakdown depends on the key).
    pub fn section_degrees(name: &str) -> Option<i64> {
        match name {
            "bridge" => Some(5),
            "solo" => Some(4),
            _ => None,
        }
    }

    /// `breakdownShift`: to the relative minor (−2) in a key with a major third, to the
    /// relative major (+2) in one with a minor third.
    pub fn breakdown_shift(intervals: &[i64]) -> i64 {
        if intervals.len() > 2 && intervals[2] == 3 {
            2
        } else {
            -2
        }
    }

    /// `degreeShift(section:intervals:)`.
    pub fn degree_shift(name: &str, intervals: &[i64]) -> i64 {
        if name == "breakdown" {
            return Self::breakdown_shift(intervals);
        }
        Self::section_degrees(name).unwrap_or(0)
    }

    /// `init?(project:)`: the `.sectionHarmony` plan.
    pub fn new(project: &Project) -> Option<SectionHarmony> {
        Self::with_fixes(project, Fixes::SECTION_HARMONY)
    }

    /// `SectionHarmony.playing(_:fixes:)`.
    pub fn playing(project: &Project, fixes: Fixes) -> Option<SectionHarmony> {
        Self::with_fixes(project, fixes)
    }

    /// `init?(project:fixes:)`.
    pub fn with_fixes(project: &Project, fixes: Fixes) -> Option<SectionHarmony> {
        let moves = fixes.contains(Fixes::SECTION_HARMONY);
        let rhythm = fixes.contains(Fixes::HARMONIC_RHYTHM);
        if !(moves || rhythm) || project.structure.len() <= 1 {
            return None;
        }
        let name = if project.scale_name.is_empty() {
            "Major"
        } else {
            project.scale_name.as_str()
        };
        let intervals: Vec<i64> = scale_intervals(&alloc::format!("{name}Scale")).to_vec();
        if intervals.len() != 7 {
            return None;
        }
        let mut start: Vec<i64> = Vec::new();
        let mut degrees: Vec<i64> = Vec::new();
        let mut at = 0;
        for s in &project.structure {
            start.push(at);
            degrees.push(if moves {
                Self::degree_shift(&s.name, &intervals)
            } else {
                0
            });
            at += s.steps;
        }
        let end = at;

        // the shift each slot plays (any degree count; 0 = home), cycle and tail
        let n = Self::SLOTS_PER_CYCLE as usize;
        let mut cycle: Vec<i64> = Vec::new();
        for d in &degrees {
            cycle.extend(core::iter::repeat(*d).take(n));
        }
        let mut tail_shift: Vec<Option<i64>> = vec![None; start.len() * Self::TAIL_SLOTS as usize];
        if rhythm {
            Self::fill_rhythm(project, &start, &degrees, &mut cycle, &mut tail_shift);
        }
        if !(cycle.iter().any(|&d| d != 0) || tail_shift.iter().any(|d| d.unwrap_or(0) != 0)) {
            return None;
        }

        // the tonal groups and the notes each one binds
        let mut groups: Vec<String> = Vec::new();
        let mut range: Vec<(i64, i64)> = Vec::new();
        for (id, nb) in project.nets.iter() {
            if nb.role == "control" {
                continue;
            }
            let notes: Vec<i64> = nb
                .bindings
                .values()
                .filter(|b| !is_drum_channel(b.channel))
                .map(|b| b.note)
                .collect();
            let (Some(&lo), Some(&hi)) = (notes.iter().min(), notes.iter().max()) else {
                continue;
            };
            let name = group_name(nb, id);
            if let Some(g) = groups.iter().position(|x| *x == name) {
                range[g] = (range[g].0.min(lo), range[g].1.max(hi));
            } else {
                groups.push(name);
                range.push((lo, hi));
            }
        }
        if groups.is_empty() {
            return None;
        }

        // the distinct shifts: the section shifts first, as named; any other slot's shift
        // reuses one seven degrees away, else is folded into -3…3
        let mut shifts: Vec<i64> = Vec::new();
        for &d in &degrees {
            if d != 0 && !shifts.contains(&d) {
                shifts.push(d);
            }
        }
        fn index(shifts: &mut Vec<i64>, d: i64) -> u8 {
            let r = (d % 7 + 7) % 7;
            if r == 0 {
                return 0;
            }
            if let Some(k) = shifts.iter().position(|&s| s == d) {
                return (k + 1) as u8;
            }
            if let Some(k) = shifts.iter().position(|&s| (s % 7 + 7) % 7 == r) {
                return (k + 1) as u8;
            }
            shifts.push(if r > 3 { r - 7 } else { r });
            shifts.len() as u8
        }
        let slots: Vec<u8> = cycle.iter().map(|&d| index(&mut shifts, d)).collect();
        let tail: Vec<u8> = tail_shift
            .iter()
            .map(|t| t.map_or(Self::NO_TAIL, |d| index(&mut shifts, d)))
            .collect();

        let root = ((project.root_note.unwrap_or(60) % 12) + 12) % 12;
        let mut group_degrees: Vec<i64> = Vec::new();
        let mut maps: Vec<u8> = Vec::new();
        for &d in &shifts {
            for &(lo, hi) in &range {
                let pick = Self::octave(d, lo, hi, root, &intervals);
                group_degrees.push(pick);
                for n in 0..128 {
                    maps.push(Self::shift(n, pick, root, &intervals) as u8);
                }
            }
        }
        Some(SectionHarmony {
            start,
            end,
            degrees,
            groups,
            shifts,
            slots,
            tail,
            group_degrees,
            maps,
        })
    }

    /// `fillRhythm`: the slot shifts that change the band's chord against the pad's own.
    fn fill_rhythm(
        project: &Project,
        start: &[i64],
        degrees: &[i64],
        cycle: &mut [i64],
        tail: &mut [Option<i64>],
    ) {
        let Some((_, key_degrees)) = seven_note_key(project) else {
            return;
        };
        let (chords, pad_bars) = pad_chords(project);
        if pad_bars <= 0 {
            return;
        }
        let n = Self::SLOTS_PER_CYCLE;
        let halves = Self::SLOTS_PER_CYCLE / 4;
        // The pad's root degree in song bar `bar` (the ring loops from the song's first bar).
        let pad_root = |bar: i64| -> Option<i64> {
            let r = *chords.get(&(bar % pad_bars))?.first()?;
            key_degrees.iter().position(|&d| d == r).map(|i| i as i64)
        };
        let cycle_slot = |cycle: &[i64], k: usize, step: i64| -> i64 {
            cycle[k * n as usize + ((step / Self::SLOT_STEPS) % n) as usize]
        };
        // The degree the band plays at `step` of section `k` by the cycle (no tails yet).
        let played = |cycle: &[i64], k: usize, step: i64| -> Option<i64> {
            pad_root(step / 16).map(|r| r + cycle_slot(cycle, k, step))
        };
        let same = |a: i64, b: i64| (a - b) % 7 == 0;

        // intros, breakdowns, outros: the odd bar of each 2-bar pair plays the even bar's
        // chord (the pad ring has to fit the 4-bar cycle)
        if pad_bars >= 2 && 4 % pad_bars == 0 {
            for (k, s) in project.structure.iter().enumerate() {
                if !Self::HOLDING_SECTIONS.contains(&s.name.as_str()) {
                    continue;
                }
                for bar in (1..4).step_by(2) {
                    let (Some(r0), Some(r1)) = (pad_root(bar - 1), pad_root(bar)) else {
                        continue;
                    };
                    for h in 0..halves {
                        cycle[k * n as usize + (bar * halves + h) as usize] = degrees[k] + r0 - r1;
                    }
                }
            }
        }

        // section ends: a buildup's last half-bar, and the last bar into a chorus
        for k in 0..start.len().saturating_sub(1) {
            let name = project.structure[k].name.as_str();
            let next = project.structure[k + 1].name.as_str();
            let run_in = next == "chorus" && name != "chorus" && name != "drop";
            if !(name == "buildup" || run_in) {
                continue;
            }
            let last = start[k + 1] - 16;
            if last < start[k] {
                continue;
            }
            let Some(target) = played(cycle, k + 1, start[k + 1]) else {
                continue;
            };
            // V, else the ♭VII (a whole step under): whichever the key has as that interval
            let tpc = key_degrees[((target % 7 + 7) % 7) as usize];
            let candidates = [(target + 4, 5i64), (target + 6, 2i64)];
            let into = candidates
                .iter()
                .find(|c| pc(tpc - key_degrees[((c.0 % 7 + 7) % 7) as usize]) == c.1)
                .map(|c| c.0);
            let (Some(into), Some(own)) = (into, pad_root(last / 16)) else {
                continue;
            };
            tail[k * Self::TAIL_SLOTS as usize + 1] = Some(into - own);
            if !run_in {
                continue;
            }
            let Some(first) = played(cycle, k, last) else {
                continue;
            };
            // the first half-bar: its own chord, unless that is the V already or the chord
            // before it, then the IV (or the ii) on the way to the V
            let before = played(cycle, k, last - 1).unwrap_or(first);
            let pick = [first, into - 1, into - 3]
                .into_iter()
                .find(|&p| !same(p, into) && !same(p, before));
            if let Some(pick) = pick {
                if !same(pick, first) {
                    tail[k * Self::TAIL_SLOTS as usize] = Some(pick - own);
                }
            }
        }
    }

    /// `groupIndex`.
    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.groups.iter().position(|g| g == name)
    }

    /// `section(at:)`: the section `step` falls in.
    pub fn section(&self, step: i64) -> usize {
        let mut k = 0;
        while k + 1 < self.start.len() && self.start[k + 1] <= step {
            k += 1;
        }
        k
    }

    /// `note(_:group:step:)`: `n` as group `group` plays it at `step`.
    pub fn note(&self, n: i64, group: usize, step: i64) -> i64 {
        if n < 0 || n >= 128 || group >= self.groups.len() {
            return n;
        }
        let s = self.section(step);
        let mut k = self.slots[s * Self::SLOTS_PER_CYCLE as usize
            + ((step.max(0) / Self::SLOT_STEPS) % Self::SLOTS_PER_CYCLE) as usize]
            as usize;
        let section_end = if s + 1 < self.start.len() {
            self.start[s + 1]
        } else {
            self.end
        };
        let back = section_end - step;
        if back > 0 && back <= Self::TAIL_SLOTS * Self::SLOT_STEPS {
            let t = self.tail[(s + 1) * Self::TAIL_SLOTS as usize
                - ((back + Self::SLOT_STEPS - 1) / Self::SLOT_STEPS) as usize];
            if t != Self::NO_TAIL {
                k = t as usize;
            }
        }
        if k == 0 {
            return n;
        }
        self.maps[((k - 1) * self.groups.len() + group) * 128 + n as usize] as i64
    }

    /// `shift(_:by:root:intervals:)`: `n` moved `d` scale degrees in the key; a note off the
    /// scale keeps its distance from the scale tone below it.
    pub fn shift(n: i64, d: i64, root: i64, intervals: &[i64]) -> i64 {
        let rel = n - root;
        let octave = libm::floor(rel as f64 / 12.0) as i64;
        let pcv = rel - octave * 12;
        let mut i = 0usize;
        while i + 1 < intervals.len() && intervals[i + 1] <= pcv {
            i += 1;
        }
        let chroma = pcv - intervals[i];
        let j = i as i64 + d;
        let o = libm::floor(j as f64 / 7.0) as i64;
        let mut out = root + (octave + o) * 12 + intervals[(j - o * 7) as usize] + chroma;
        while out > 127 {
            out -= 12;
        }
        while out < 0 {
            out += 12;
        }
        out
    }

    /// `octave(_:lo:hi:root:intervals:)`: the octave a group plays shift `d` in.
    pub fn octave(d: i64, lo: i64, hi: i64, root: i64, intervals: &[i64]) -> i64 {
        let other = if d > 0 { d - 7 } else { d + 7 };
        let mid = (lo + hi) / 2;
        let floor = lo.min(Self::FLOOR_NOTE);
        let ceiling = hi.max(Self::CEILING_NOTE);
        let fits = |c: i64| -> bool {
            let a = Self::shift(lo, c, root, intervals);
            let b = Self::shift(hi, c, root, intervals);
            // `shift` folds into 0…127; a fold means it didn't fit
            a >= floor && b <= ceiling && (a - lo) * c >= 0 && (b - hi) * c >= 0
        };
        let mv = |c: i64| -> i64 { (Self::shift(mid, c, root, intervals) - mid).abs() };
        let mut ranked = [d, other];
        // `sorted { a, b in ma != mb ? ma < mb : (mid < 60 ? a < b : a > b) }` of two
        let less = |a: i64, b: i64| -> bool {
            let (ma, mb) = (mv(a), mv(b));
            if ma != mb {
                return ma < mb;
            }
            if mid < 60 {
                a < b
            } else {
                a > b
            }
        };
        if less(ranked[1], ranked[0]) {
            ranked.swap(0, 1);
        }
        if !fits(ranked[0]) && fits(ranked[1]) {
            ranked.swap(0, 1);
        }
        ranked[0]
    }
}
