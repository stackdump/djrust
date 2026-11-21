//! Loop pairs: the rule that makes a loop's A and B go together, and the measure of it.
//!
//! In short, a pair is a **shared frame**, a **shared call** and an **answer that does not rival
//! it**:
//!
//! 1. *Frame*: A and B are in one key, at one tempo, the same number of bars, with the same
//!    parts, and every part's ring divides the loop (so the loops start and end on the same
//!    bar line and every phrase lines up).
//! 2. *Call*: the groove (kick, snare, bass, harmony) is the same in both; those notes are in
//!    **unison**: same step, same note, so layered they stack and never mask.
//! 3. *Answer*: the voices that speak (melody, arp) differ, and B's notes **fill the gaps** of
//!    A's: B never starts a note while A's line is sounding a different one (a **clash**).
//!
//! Every loop generator hands its A and B to [`pair`] (or checks them with [`report`]). The
//! Acid Bass bank is the reference: its B repeats A's first half and answers in the second;
//! Floor & Sub splits drums from bass.
//!
//! Pure (no I/O, `no_std + alloc`): the notes are read the way the deck plays them
//! (`musicality::note_log`), one tick a sixteenth.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::musicality::note_log::{capture_all, NoteEvent};
use crate::pflow::Project;
use crate::wave::synth::{drum_kind, is_drum_channel};

/// The version of the pairing rule, written into a loop set's document ([`crate::loopsets`]) so
/// its CID names the rule its loops were paired by.
pub const VERSION: u32 = 1;

/// Notes below this (MIDI, G3, ~196 Hz) are the low band: the bass and its octave.
pub const LOW_BELOW: i64 = 55;
/// Notes from this (MIDI, C6) up are the high band.
pub const HIGH_FROM: i64 = 84;

/// Where a note sits in the spectrum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    /// Kicks, toms, the bass.
    Low,
    /// Snares and claps, chords, the lead.
    Mid,
    /// Hats and cymbals, the top of an arp.
    High,
}

impl Band {
    pub fn name(self) -> &'static str {
        match self {
            Band::Low => "low",
            Band::Mid => "mid",
            Band::High => "high",
        }
    }
}

/// The band `e` is in.
pub fn band_of(e: &NoteEvent) -> Band {
    if is_drum_channel(e.channel) {
        match drum_kind(e.note) {
            "kick" => Band::Low,
            "hat" | "openhat" => Band::High,
            _ => Band::Mid,
        }
    } else if e.note < LOW_BELOW {
        Band::Low
    } else if e.note >= HIGH_FROM {
        Band::High
    } else {
        Band::Mid
    }
}

/// What A and B do to one part, counted in B's notes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartReport {
    /// The part's band, by its first note in A (else in B).
    pub band: Option<Band>,
    /// Notes of the part in A and B over the loop.
    pub a: usize,
    pub b: usize,
    /// B's notes that A plays too: same step, same note (same drum for a drum part).
    pub unison: usize,
    /// B's notes in a gap of A's line: nothing of A's part is sounding on that step.
    pub fill: usize,
    /// B's notes an octave (or more) from a note A's line sounds on that step: the same pitch
    /// class in another register, a doubling that does not crowd it.
    pub octave: usize,
    /// B's notes that start while A's line sounds other notes (pitched parts only).
    pub clash: usize,
}

/// A and B measured together ([`report`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PairReport {
    pub same_key: bool,
    pub same_tempo: bool,
    pub same_bars: bool,
    /// Both have the same parts. A pair that splits the parts between its loops (drums in A,
    /// bass in B) says false here and is still a pair.
    pub same_parts: bool,
    /// Every part both have has the same ring in both, so each starts on the same bar line and
    /// a phrase lines up with its partner.
    pub aligned: bool,
    /// Every ring divides the loop, so no part is cut mid-ring at the loop point. A genre's
    /// own polymetre (a 6-step hat over 16) says false here for A and B alike.
    pub loops_clean: bool,
    /// The loop's length (A's), in bars.
    pub bars: i64,
    /// By part name.
    pub parts: BTreeMap<String, PartReport>,
}

impl PairReport {
    /// The frame holds: one key, one tempo, one length, phrase-aligned. (Whether the parts are
    /// the same is [`same_parts`](Self::same_parts): a pair may split them.)
    pub fn frame_ok(&self) -> bool {
        self.same_key && self.same_tempo && self.same_bars && self.aligned
    }

    /// The parts' counts added up by band.
    pub fn band(&self, band: Band) -> PartReport {
        let mut t = PartReport {
            band: Some(band),
            ..Default::default()
        };
        for p in self.parts.values().filter(|p| p.band == Some(band)) {
            t.a += p.a;
            t.b += p.b;
            t.unison += p.unison;
            t.fill += p.fill;
            t.octave += p.octave;
            t.clash += p.clash;
        }
        t
    }

    /// B's notes that start on A's own notes or in its gaps, over all of B's notes: 1.0 when
    /// nothing clashes.
    pub fn harmony_ratio(&self) -> f64 {
        let b: usize = self.parts.values().map(|p| p.b).sum();
        let c: usize = self.parts.values().map(|p| p.clash).sum();
        if b == 0 {
            1.0
        } else {
            1.0 - c as f64 / b as f64
        }
    }

    /// The part names where B says something A does not: notes in A's gaps or an octave away.
    pub fn answering(&self) -> Vec<&str> {
        self.parts
            .iter()
            .filter(|(_, p)| p.fill > 0 || p.octave > 0)
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// Does the lead (the melody, else the arp) answer? A pair whose lead is the same notes
    /// twice has no answer.
    pub fn lead_answers(&self) -> bool {
        let lead = if self.parts.contains_key("melody") {
            "melody"
        } else {
            "arp"
        };
        self.parts
            .get(lead)
            .is_some_and(|p| p.fill > 0 || p.octave > 0)
    }
}

/// The loop's notes by tick (0-based step), per part name (the net's group name).
fn notes_of(p: &Project) -> (i64, Vec<(i64, NoteEvent)>) {
    let bars = p.bars.unwrap_or(1).max(1);
    let (events, _) = capture_all(p, Some(bars * 16));
    (bars, events.into_iter().map(|e| (e.tick - 1, e)).collect())
}

/// A's notes of one part, for asking what sounds on a step.
struct Line {
    /// (start, length in steps, note), by start.
    notes: Vec<(i64, i64, i64)>,
    /// Drum parts have no sustain: a hit is its step only.
    drum: bool,
}

impl Line {
    /// The notes sounding on step `t` of a loop `len` steps long (a note held past the end
    /// wraps onto the start).
    fn at(&self, t: i64, len: i64) -> Vec<i64> {
        let mut v = Vec::new();
        for &(s, l, n) in &self.notes {
            let l = if self.drum { 1 } else { l.max(1) };
            if [s - len, s].iter().any(|&s| t >= s && t < s + l) {
                v.push(n);
            }
        }
        v
    }
}

/// How B's note stands to the notes A's line sounds with it.
#[derive(PartialEq)]
enum Stand {
    Unison,
    Fill,
    Octave,
    Clash,
}

fn stand(e: &NoteEvent, a: &[i64]) -> Stand {
    if a.is_empty() {
        Stand::Fill
    } else if e.is_drum() {
        if a.iter().any(|&n| drum_kind(n) == drum_kind(e.note)) {
            Stand::Unison
        } else {
            Stand::Fill
        }
    } else if a.contains(&e.note) {
        Stand::Unison
    } else if a.iter().any(|&n| (n - e.note).rem_euclid(12) == 0) {
        Stand::Octave
    } else {
        Stand::Clash
    }
}

fn lines(events: &[(i64, NoteEvent)], keep: impl Fn(&NoteEvent) -> bool) -> BTreeMap<String, Line> {
    let mut m: BTreeMap<String, Line> = BTreeMap::new();
    for (t, e) in events.iter().filter(|(_, e)| keep(e)) {
        let l = m.entry(e.net.clone()).or_insert(Line {
            notes: Vec::new(),
            drum: e.is_drum(),
        });
        l.notes.push((*t, libm::ceil(e.steps) as i64, e.note));
    }
    m
}

/// Do A and B go together? Counts, per part, how B's notes stand to A's.
pub fn report(a: &Project, b: &Project) -> PairReport {
    let (bars, ea) = notes_of(a);
    let (bars_b, eb) = notes_of(b);
    let len = bars * 16;
    let names = |p: &Project| p.nets.keys().to_vec();
    let clean = |p: &Project| {
        p.nets
            .values()
            .all(|n| n.places.is_empty() || len % n.places.len() as i64 == 0)
    };
    // the parts both have: a pair that splits the parts between A and B (Floor & Sub) has
    // fewer in common, and those must line up
    let same_rings = a.nets.iter().all(|(k, n)| {
        b.nets
            .get(k)
            .map_or(true, |m| m.places.len() == n.places.len())
    });
    let la = lines(&ea, |_| true);
    let mut parts: BTreeMap<String, PartReport> = BTreeMap::new();
    for (_, e) in &ea {
        let r = parts.entry(e.net.clone()).or_default();
        r.a += 1;
        r.band.get_or_insert(band_of(e));
    }
    for (t, e) in &eb {
        let r = parts.entry(e.net.clone()).or_default();
        r.b += 1;
        r.band.get_or_insert(band_of(e));
        let sounding = la.get(&e.net).map(|l| l.at(*t, len)).unwrap_or_default();
        match stand(e, &sounding) {
            Stand::Unison => r.unison += 1,
            Stand::Fill => r.fill += 1,
            Stand::Octave => r.octave += 1,
            Stand::Clash => r.clash += 1,
        }
    }
    PairReport {
        same_key: a.root_note == b.root_note && a.scale_name == b.scale_name,
        same_tempo: a.tempo == b.tempo,
        same_bars: bars == bars_b,
        same_parts: names(a) == names(b),
        aligned: same_rings,
        loops_clean: clean(a) && clean(b),
        bars,
        parts,
    }
}

/// Is `p`'s net `k` a part that stays in unison (the groove) rather than answering?
fn is_groove(k: &str, n: &crate::pflow::NetBundle) -> bool {
    is_drum_channel(n.track.channel) || matches!(k, "bass" | "harmony")
}

/// The top of the register a voice moves up into (MIDI C7): above it the octave goes down.
const OCTAVE_CEILING: i64 = 96;

/// Make `b` A's partner. The groove (the drums, the bass, the harmony) stays as `b` has it:
/// generators cut both loops from one track, so it is the same notes, in unison. Each voice
/// (the melody, the arp) is made to answer:
///
/// - the notes of B's that start while A's line sounds another note are dropped, leaving B in
///   A's gaps (and on A's own notes);
/// - a voice with nothing of its own left (every note dropped, or B was A's notes again), or
///   with less than half of its notes left, becomes A's line an octave up (down when that
///   would pass [`OCTAVE_CEILING`]): the same notes in another register, so the two never
///   crowd one another.
///
/// Deterministic, and a function of the two loops alone. `b` comes back with nothing else
/// touched; applied again it changes nothing.
pub fn complement(a: &Project, mut b: Project) -> Project {
    let (bars, ea) = notes_of(a);
    let (_, eb) = notes_of(&b);
    let len = bars * 16;
    let la = lines(&ea, |_| true);
    for k in b.nets.keys().to_vec() {
        let Some(n) = b.nets.get(&k) else { continue };
        if is_groove(&k, n) {
            continue;
        }
        let (Some(line), Some(an)) = (la.get(&k), a.nets.get(&k)) else {
            continue;
        };
        // each binding's worst: does any of its fires clash, does any answer (fill / octave)
        let mut clash: BTreeMap<String, bool> = BTreeMap::new();
        let mut answers: BTreeMap<String, bool> = BTreeMap::new();
        for (t, e) in eb.iter().filter(|(_, e)| e.net == k) {
            let ring = (n.places.len() as i64).max(1);
            let key = alloc::format!("t{}", t.rem_euclid(ring));
            match stand(e, &line.at(*t, len)) {
                Stand::Clash => *clash.entry(key).or_default() = true,
                Stand::Fill | Stand::Octave => *answers.entry(key).or_default() = true,
                Stand::Unison => {}
            }
        }
        let total = n.bindings.len();
        let kept_answers = answers.keys().filter(|k| !clash.contains_key(*k)).count();
        let dropped = clash.len();
        let n = b.nets.get_mut(&k).unwrap();
        if kept_answers > 0 && (total - dropped) * 2 >= total {
            for d in clash.keys() {
                n.bindings.remove(d);
            }
        } else if n.places.len() == an.places.len() {
            let top = an.bindings.values().map(|m| m.note).max().unwrap_or(0);
            let shift = if top + 12 <= OCTAVE_CEILING { 12 } else { -12 };
            n.bindings = an.bindings.clone();
            for key in n.bindings.keys().to_vec() {
                if let Some(m) = n.bindings.get_mut(&key) {
                    m.note = (m.note + shift).clamp(0, 127);
                }
            }
        }
        n.track.beats = Some(n.bindings.len() as f64);
    }
    b
}

/// A loop and its partner, paired: `a` as it is, `b` as [`complement`] makes it.
pub fn pair(a: Project, b: Project) -> (Project, Project) {
    let b = complement(&a, b);
    (a, b)
}

/// The report as one line, for logs and test messages.
pub fn summary(r: &PairReport) -> String {
    let mut s = alloc::format!("{} bars", r.bars);
    for (k, p) in &r.parts {
        s += &alloc::format!(" {k}:{}u/{}f/{}o/{}c", p.unison, p.fill, p.octave, p.clash);
    }
    s.to_string()
}
