//! Loop routing (beyond Swift; docs/LOOP-ROUTING.md): a loop's parts sent out as notes, to gear
//! and to a deck's live voice, while the loop plays.
//!
//! A clip taken off a net deck knows what the deck was playing (its provenance), so its notes
//! can be had again: the host replays the take's bars and keeps the net's fires
//! (`crate::host::deck::loop_notes`), each tagged with its part ("melody", "bass", "kick"…). Audio
//! from anywhere else (a sample deck, an input strip, a file or stock loop) has no notes:
//! [`no_notes`] says why, and nothing is sent for it.
//!
//! The rules here are pure: which parts of which clip go where ([`Routes`]), what a note
//! becomes at a destination ([`voice`], [`plan`]: the TD-3 is monophonic and has a range), and
//! when each one is due, following the clip's playhead ([`Follow`]). The host reads the
//! playhead, turns the times into its clock's and sends.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::loops::Refusal;
use super::perform::ROLE_ORDER;

/// Every drum part at once (kick, snare, hihat, clap and any other drum net).
pub const DRUMS: &str = "drums";
/// Every part of the clip.
pub const ALL: &str = "all";
/// The parts the window and the registry offer, in this order. A clip may have others
/// (a stinger, a part of its own): those route by name too.
pub const PARTS: [&str; 10] = [
    "melody", "bass", "harmony", "arp", DRUMS, "kick", "snare", "hihat", "clap", ALL,
];

/// The TD-3's range, MIDI notes: anything outside is moved by octaves into it.
pub const TD3_LOW: u8 = 24;
pub const TD3_HIGH: u8 = 72;
/// The PD705's note channel (10), 0-based.
pub const PD705_CHANNEL: u8 = 9;
/// The shortest note sent, in sixteenths (a fire with no length still gates).
pub const MIN_LEN: f64 = 0.25;
/// How far ahead of a note's next use its previous note-off lands, in sixteenths: the off of
/// one note never cuts the next one on the same key.
const GAP: f64 = 0.05;

// MARK: notes

/// One note of a loop, as the net played it in the take.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopNote {
    /// Where it starts, in sixteenths from the clip's first sample (0 ≤ at < steps).
    pub at: f64,
    /// How long it is held, in sixteenths.
    pub len: f64,
    /// The part it belongs to: its net's riff group, else the net's id.
    pub part: String,
    /// Played on a drum channel (10…15).
    pub drum: bool,
    /// The MIDI channel it played on, 1…16.
    pub channel: u8,
    pub note: u8,
    pub velocity: u8,
}

/// A clip's notes: what [`plan`] and [`Follow`] work from.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LoopNotes {
    /// The clip's length in sixteenths (its beats × 4).
    pub steps: u32,
    /// In order of `at`, then of note.
    pub notes: Vec<LoopNote>,
}

impl LoopNotes {
    /// Notes in order, each `at` wrapped into the clip.
    pub fn new(steps: u32, mut notes: Vec<LoopNote>) -> LoopNotes {
        let s = steps.max(1) as f64;
        for n in &mut notes {
            // `rem_euclid(s)`, spelled out: `f64::rem_euclid` needs std
            let r = n.at % s;
            n.at = if r < 0.0 { r + s } else { r };
            if n.at >= s {
                n.at = 0.0;
            }
            n.len = n.len.max(MIN_LEN);
        }
        notes.sort_by(|a, b| {
            a.at.partial_cmp(&b.at)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.note.cmp(&b.note))
        });
        LoopNotes { steps, notes }
    }

    /// The parts that have notes: the known roles in [`ROLE_ORDER`], then the others by name,
    /// then [`DRUMS`] when any note is a drum.
    pub fn parts(&self) -> Vec<String> {
        let have: BTreeSet<&str> = self.notes.iter().map(|n| n.part.as_str()).collect();
        let mut out: Vec<String> = ROLE_ORDER
            .iter()
            .filter(|r| have.contains(*r))
            .map(|r| r.to_string())
            .collect();
        for p in &have {
            if !ROLE_ORDER.contains(p) {
                out.push(p.to_string());
            }
        }
        if self.notes.iter().any(|n| n.drum) {
            out.push(DRUMS.to_string());
        }
        out
    }

    /// How many notes `part` has.
    pub fn count(&self, part: &str) -> usize {
        self.notes.iter().filter(|n| part_matches(part, n)).count()
    }
}

/// Does a route of `part` take note `n`?
pub fn part_matches(part: &str, n: &LoopNote) -> bool {
    part == ALL || part == n.part || (part == DRUMS && n.drum)
}

/// Why a clip has no notes, for people and for the socket: `(kind, message)`. `kind` is
/// "audio-only" for sound from anywhere but a net (nothing to recover), else what stopped it.
pub fn no_notes(r: &Refusal) -> (&'static str, String) {
    match r {
        Refusal::NotANet => (
            "audio-only",
            "Audio only: taken from a sample deck, so there are no notes to send.".into(),
        ),
        Refusal::NoProvenance => (
            "audio-only",
            "Audio only: captured from an input, a file or a stock loop (no net behind it), so there are no notes to send."
                .into(),
        ),
        Refusal::Sequence => (
            "sequence",
            "A sequence: route the loops it is made of.".into(),
        ),
        Refusal::Macros(m) => (
            "macros",
            format!(
                "Taken with {} running: its notes can't be had exactly. Take it again without.",
                m.join(", ")
            ),
        ),
        Refusal::SongEnded | Refusal::TooFar => (
            "unreachable",
            "Its bars could not be played again (the take is past where a replay reaches).".into(),
        ),
    }
}

// MARK: destinations

/// Where a part can go.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dest {
    /// The TD-3 over MIDI, on its note channel. Monophonic.
    Td3,
    /// The PD705's pads over MIDI, channel 10.
    Pd705,
    /// A deck's live voice (the software synth the MPK keys play): "A" or "B".
    Synth(String),
    /// Any MIDI output whose name holds `port`, on `channel` (1…16; None: the note's own).
    Midi { port: String, channel: Option<u8> },
}

impl Dest {
    /// `td3`, `pd705`, `synth` (deck A), `synth:B`, `midi:<part of a port name>[@channel]`.
    pub fn parse(s: &str) -> Result<Dest, String> {
        let t = s.trim();
        let low = t.to_lowercase();
        match low.as_str() {
            "td3" | "td-3" => return Ok(Dest::Td3),
            "pd705" | "pd-705" => return Ok(Dest::Pd705),
            "synth" => return Ok(Dest::Synth("A".into())),
            _ => {}
        }
        if let Some(d) = low.strip_prefix("synth:") {
            return match d {
                "a" | "b" => Ok(Dest::Synth(d.to_uppercase())),
                _ => Err(format!("synth:{d}: the synth is deck A's or B's live voice")),
            };
        }
        if low.starts_with("midi:") {
            let rest = &t[5..];
            let (port, channel) = match rest.rsplit_once('@') {
                Some((p, c)) => {
                    let ch: u8 = c
                        .trim()
                        .parse()
                        .ok()
                        .filter(|c| (1..=16).contains(c))
                        .ok_or_else(|| format!("{s}: the channel is 1..16"))?;
                    (p.trim(), Some(ch))
                }
                None => (rest.trim(), None),
            };
            if port.is_empty() {
                return Err(format!("{s}: name the port (part of its name)"));
            }
            return Ok(Dest::Midi {
                port: port.to_string(),
                channel,
            });
        }
        Err(format!(
            "{s:?}: a destination is td3, pd705, synth, synth:A, synth:B or midi:<port>[@channel]"
        ))
    }

    /// The destination as [`parse`](Self::parse) reads it back: the key routes are kept under.
    pub fn key(&self) -> String {
        match self {
            Dest::Td3 => "td3".into(),
            Dest::Pd705 => "pd705".into(),
            Dest::Synth(d) => format!("synth:{d}"),
            Dest::Midi { port, channel: None } => format!("midi:{port}"),
            Dest::Midi {
                port,
                channel: Some(c),
            } => format!("midi:{port}@{c}"),
        }
    }

    /// For the window.
    pub fn label(&self) -> String {
        match self {
            Dest::Td3 => "TD-3".into(),
            Dest::Pd705 => "PD705".into(),
            Dest::Synth(d) => format!("Synth {d}"),
            Dest::Midi { port, channel: None } => format!("MIDI {port}"),
            Dest::Midi {
                port,
                channel: Some(c),
            } => format!("MIDI {port} ch{c}"),
        }
    }

    /// Plays one note at a time.
    pub fn is_mono(&self) -> bool {
        matches!(self, Dest::Td3)
    }

    /// Goes out over MIDI (else: to a deck).
    pub fn is_midi(&self) -> bool {
        !matches!(self, Dest::Synth(_))
    }
}

/// The destinations every window offers, before any `midi:` one is added.
pub fn standard_dests() -> Vec<Dest> {
    Vec::from([
        Dest::Td3,
        Dest::Pd705,
        Dest::Synth("A".into()),
        Dest::Synth("B".into()),
    ])
}

/// What note `n` becomes at `dest`: `(channel 0…15, note)`. `transpose` (the clip's key
/// shift, semitones) moves tonal notes, never drums. `td3_channel` is the TD-3's note
/// channel, 1…16.
pub fn voice(dest: &Dest, n: &LoopNote, transpose: i32, td3_channel: u8) -> (u8, u8) {
    let shifted = if n.drum {
        n.note
    } else {
        (n.note as i32 + transpose).clamp(0, 127) as u8
    };
    match dest {
        Dest::Td3 => (td3_channel.clamp(1, 16) - 1, fold(shifted, TD3_LOW, TD3_HIGH)),
        Dest::Pd705 => (PD705_CHANNEL, shifted),
        Dest::Synth(_) => (0, shifted),
        Dest::Midi { channel, .. } => (
            channel.unwrap_or(n.channel).clamp(1, 16) - 1,
            shifted,
        ),
    }
}

/// At the PD705, drop the planned notes that would land on a note `skip` holds back (the
/// loaded kit's loop pads, `gear::pd705::Kit::holds_back`), judged by the note actually sent
/// ([`voice`], after `transpose`). Other destinations are left alone. Returns how many went.
pub fn hold_back(
    planned: &mut Vec<LoopNote>,
    dest: &Dest,
    transpose: i32,
    td3_channel: u8,
    skip: impl Fn(u8) -> bool,
) -> usize {
    if *dest != Dest::Pd705 {
        return 0;
    }
    let before = planned.len();
    planned.retain(|n| !skip(voice(dest, n, transpose, td3_channel).1));
    before - planned.len()
}

/// `note` moved by whole octaves into `low..=high` (a range narrower than an octave keeps
/// the low end).
pub fn fold(note: u8, low: u8, high: u8) -> u8 {
    let mut n = note as i32;
    while n < low as i32 {
        n += 12;
    }
    while n > high as i32 && n - 12 >= low as i32 {
        n -= 12;
    }
    n.clamp(0, 127) as u8
}

/// What a clip sends to one destination: the notes of the `parts` routed there, made fit for
/// it. A mono destination keeps one note per start (the lowest for a part named "bass", else
/// the highest), each cut short before the next starts so the TD-3 doesn't slide into it. On
/// any destination a note is cut short before the same key's next note (so its note-off can't
/// silence that one). The clip loops: the last note is cut against the first.
pub fn plan(notes: &LoopNotes, parts: &BTreeSet<String>, dest: &Dest) -> Vec<LoopNote> {
    let steps = notes.steps.max(1) as f64;
    let mut out: Vec<LoopNote> = notes
        .notes
        .iter()
        .filter(|n| parts.iter().any(|p| part_matches(p, n)))
        .cloned()
        .collect();
    if dest.is_mono() {
        let mut kept: Vec<LoopNote> = Vec::new();
        for n in out {
            match kept.last_mut() {
                Some(k) if (k.at - n.at).abs() < 1e-6 => {
                    let low = n.part == "bass" && k.part == "bass";
                    if (low && n.note < k.note) || (!low && n.note > k.note) {
                        *k = n;
                    }
                }
                _ => kept.push(n),
            }
        }
        let count = kept.len();
        for i in 0..count {
            let next = if i + 1 < count {
                kept[i + 1].at
            } else {
                kept[0].at + steps
            };
            let room = next - kept[i].at - GAP;
            kept[i].len = kept[i].len.min(room).max(GAP);
        }
        out = kept;
    } else {
        let count = out.len();
        for i in 0..count {
            let (at, note, ch) = (out[i].at, out[i].note, out[i].channel);
            // the next start of the same key, around the loop
            let next = (1..=count)
                .map(|k| (i + k) % count)
                .find(|&j| out[j].note == note && out[j].channel == ch)
                .map(|j| {
                    let a = out[j].at;
                    if a > at {
                        a
                    } else {
                        a + steps
                    }
                });
            if let Some(nx) = next {
                let room = nx - at - GAP;
                out[i].len = out[i].len.min(room).max(GAP);
            }
        }
    }
    out
}

// MARK: routes

/// Which parts of which clip go where: clip id → part → destination keys ([`Dest::key`]).
/// A route outlives the clip stopping and starting; it goes with the clip off the row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Routes {
    clips: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

impl Routes {
    /// Route `part` of `clip` to `dest` (`on`) or not. Returns whether anything changed.
    pub fn set(&mut self, clip: &str, part: &str, dest: &str, on: bool) -> bool {
        if on {
            self.clips
                .entry(clip.to_string())
                .or_default()
                .entry(part.to_string())
                .or_default()
                .insert(dest.to_string())
        } else {
            let Some(parts) = self.clips.get_mut(clip) else {
                return false;
            };
            let Some(dests) = parts.get_mut(part) else {
                return false;
            };
            let changed = dests.remove(dest);
            if dests.is_empty() {
                parts.remove(part);
            }
            if parts.is_empty() {
                self.clips.remove(clip);
            }
            changed
        }
    }

    /// Flip one route; returns whether it is on now.
    pub fn toggle(&mut self, clip: &str, part: &str, dest: &str) -> bool {
        let on = !self.is_on(clip, part, dest);
        self.set(clip, part, dest, on);
        on
    }

    pub fn is_on(&self, clip: &str, part: &str, dest: &str) -> bool {
        self.clips
            .get(clip)
            .and_then(|p| p.get(part))
            .is_some_and(|d| d.contains(dest))
    }

    /// Clip `clip`'s routes: part → destinations.
    pub fn of(&self, clip: &str) -> Option<&BTreeMap<String, BTreeSet<String>>> {
        self.clips.get(clip)
    }

    /// Clip `clip`'s routes turned round: destination → the parts sent there.
    pub fn by_dest(&self, clip: &str) -> BTreeMap<String, BTreeSet<String>> {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(parts) = self.clips.get(clip) {
            for (part, dests) in parts {
                for d in dests {
                    out.entry(d.clone()).or_default().insert(part.clone());
                }
            }
        }
        out
    }

    /// The clips with any route.
    pub fn clips(&self) -> Vec<String> {
        self.clips.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// Take every route off `clip`, or off every clip.
    pub fn clear(&mut self, clip: Option<&str>) {
        match clip {
            Some(c) => {
                self.clips.remove(c);
            }
            None => self.clips.clear(),
        }
    }

    /// Keep only the clips `keep` says are still there (the others went off the row).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.clips.retain(|c, _| keep(c));
    }
}

// MARK: timing

/// How far the playhead may stray from where it should be, in sixteenths, before the follow
/// starts again from it (a restart, a jump, the host stalling).
pub const RESYNC_STEPS: f64 = 1.0;
/// How far back a follow that starts (or starts again) looks, in sixteenths: a loop launched
/// on the bar is first read a frame or a block after its downbeat, and the downbeat's notes
/// go out at once rather than not at all.
pub const JOIN_GRACE: f64 = 0.35;
/// How much of a small error each reading takes up: the playhead is read once a block, so a
/// single reading is as much as a block out.
pub const SLEW: f64 = 0.1;

/// One note due: when it goes on and off (host seconds), and which.
#[derive(Clone, Debug, PartialEq)]
pub struct Due<'a> {
    pub on: f64,
    pub off: f64,
    pub note: &'a LoopNote,
}

/// Following a clip's playhead in host time, to say when each of its notes is due.
///
/// The host reads the playhead (unwrapped: sixteenths since the clip's voice started, counting
/// every pass of the loop) and its rate each pass and hands both to [`observe`](Self::observe);
/// [`due`](Self::due) then gives the notes from where the last call stopped up to a horizon a
/// little ahead, each with its host time, for a clock that sends on time.
#[derive(Clone, Debug, PartialEq)]
pub struct Follow {
    /// Host time and unwrapped position of the estimate.
    t: f64,
    pos: f64,
    /// Sixteenths per second.
    rate: f64,
    /// Everything before this position has been handed out.
    done: f64,
}

impl Follow {
    /// From a first reading: notes from `pos` on.
    pub fn new(t: f64, pos: f64, rate: f64) -> Follow {
        Follow {
            t,
            pos,
            rate,
            done: pos,
        }
    }

    /// From a first reading of a loop just started or just routed: the notes of the last
    /// [`JOIN_GRACE`] are handed out too (late by as much, at most).
    pub fn joining(t: f64, pos: f64, rate: f64) -> Follow {
        Follow {
            done: (pos - JOIN_GRACE).max(0.0),
            ..Follow::new(t, pos, rate)
        }
    }

    /// Where the playhead is at host time `t`, by the estimate.
    pub fn pos_at(&self, t: f64) -> f64 {
        self.pos + (t - self.t) * self.rate
    }

    /// When the playhead is at `u`, by the estimate.
    pub fn time_of(&self, u: f64) -> f64 {
        if self.rate > 0.0 {
            self.t + (u - self.pos) / self.rate
        } else {
            self.t
        }
    }

    /// Everything before this position has been handed out.
    pub fn done(&self) -> f64 {
        self.done
    }

    /// A reading of the playhead `pos` at host time `t`, moving at `rate` sixteenths a second.
    /// Small errors are taken up a little at a time; a big one starts the follow again from
    /// the reading (returns true). After a jump back, or a stall that left notes behind, the
    /// notes resume from the reading (the ones missed are not sent late).
    pub fn observe(&mut self, t: f64, pos: f64, rate: f64) -> bool {
        let predicted = self.pos_at(t);
        let err = pos - predicted;
        if !err.is_finite() || err.abs() > RESYNC_STEPS {
            self.t = t;
            self.pos = pos;
            self.rate = rate;
            if pos < self.done - RESYNC_STEPS || pos > self.done + RESYNC_STEPS {
                self.done = (pos - JOIN_GRACE).max(0.0);
            }
            return true;
        }
        self.pos = predicted + err * SLEW;
        self.t = t;
        self.rate = rate;
        false
    }

    /// The notes of `notes` (one pass of a loop `steps` long) that start from where the last
    /// call stopped up to host time `horizon`, in order. Each goes off after its `len` (at
    /// least [`MIN_LEN`]).
    pub fn due<'a>(&mut self, notes: &'a [LoopNote], steps: f64, horizon: f64) -> Vec<Due<'a>> {
        let mut out = Vec::new();
        if self.rate <= 0.0 || steps <= 0.0 {
            return out;
        }
        let end = self.pos_at(horizon);
        if end <= self.done {
            return out;
        }
        let k0 = libm::floor(self.done / steps) as i64;
        let k1 = libm::floor(end / steps) as i64;
        for k in k0..=k1 {
            let base = k as f64 * steps;
            for n in notes {
                let u = base + n.at;
                if u >= self.done && u < end {
                    out.push(Due {
                        on: self.time_of(u),
                        off: self.time_of(u + n.len.max(GAP)),
                        note: n,
                    });
                }
            }
        }
        self.done = end;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(at: f64, part: &str, note: u8) -> LoopNote {
        LoopNote {
            at,
            len: 1.0,
            part: part.into(),
            drum: matches!(part, "kick" | "snare" | "hihat" | "clap"),
            channel: if matches!(part, "kick" | "snare" | "hihat" | "clap") {
                10
            } else {
                1
            },
            note,
            velocity: 100,
        }
    }

    fn set(parts: &[&str]) -> BTreeSet<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn destinations_parse_and_print_back() {
        for (s, d) in [
            ("td3", Dest::Td3),
            ("TD-3", Dest::Td3),
            ("pd705", Dest::Pd705),
            ("synth", Dest::Synth("A".into())),
            ("synth:b", Dest::Synth("B".into())),
            (
                "midi:IAC Bus 1@3",
                Dest::Midi {
                    port: "IAC Bus 1".into(),
                    channel: Some(3),
                },
            ),
            (
                "midi:Minilogue",
                Dest::Midi {
                    port: "Minilogue".into(),
                    channel: None,
                },
            ),
        ] {
            assert_eq!(Dest::parse(s), Ok(d.clone()), "{s}");
            assert_eq!(Dest::parse(&d.key()), Ok(d));
        }
        assert!(Dest::parse("midi:x@17").is_err());
        assert!(Dest::parse("midi:").is_err());
        assert!(Dest::parse("synth:C").is_err());
        assert!(Dest::parse("tb303").is_err());
        assert!(Dest::Td3.is_mono() && !Dest::Pd705.is_mono());
        assert!(!Dest::Synth("A".into()).is_midi());
    }

    #[test]
    fn the_pd705_never_gets_a_kits_loop_pads() {
        let kit = crate::gear::pd705::Kit::preset("acid-house").unwrap();
        let notes = || {
            alloc::vec![
                n(0.0, "kick", 36),
                n(1.0, "melody", 49),
                n(2.0, "melody", 55),
                n(3.0, "bass", 45),
            ]
        };
        // untransposed: the melody's 49 is Pad1 (a loop), 55 and the bass's 45 (Pad5) are not
        let mut v = notes();
        assert_eq!(
            hold_back(&mut v, &Dest::Pd705, 0, 1, |x| kit.holds_back(x)),
            1
        );
        assert_eq!(v.iter().map(|x| x.note).collect::<Vec<_>>(), [36, 55, 45]);
        // transposed +2: 49 → 51 (Pad3) and 55 → 57 (Pad2) go, 45 → 47 stays; drums never move
        let mut v = notes();
        assert_eq!(
            hold_back(&mut v, &Dest::Pd705, 2, 1, |x| kit.holds_back(x)),
            2
        );
        assert_eq!(v.iter().map(|x| x.note).collect::<Vec<_>>(), [36, 45]);
        // other destinations are not the pad
        let mut v = notes();
        assert_eq!(
            hold_back(&mut v, &Dest::Synth("A".into()), 0, 1, |x| kit
                .holds_back(x)),
            0
        );
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn parts_list_known_roles_first_then_drums() {
        let notes = LoopNotes::new(
            16,
            Vec::from([
                n(0.0, "kick", 36),
                n(2.0, "melody", 72),
                n(4.0, "bass", 40),
                n(5.0, "hit1", 60),
            ]),
        );
        assert_eq!(notes.parts(), ["kick", "bass", "melody", "hit1", "drums"]);
        assert_eq!(notes.count(DRUMS), 1);
        assert_eq!(notes.count(ALL), 4);
        assert_eq!(notes.count("melody"), 1);
    }

    #[test]
    fn notes_wrap_into_the_clip_and_sort() {
        let notes = LoopNotes::new(16, Vec::from([n(17.0, "melody", 70), n(-0.5, "melody", 60)]));
        assert_eq!(notes.notes[0].at, 1.0);
        assert_eq!(notes.notes[1].at, 15.5);
    }

    #[test]
    fn the_td3_gets_its_channel_and_its_range() {
        let mel = n(0.0, "melody", 84);
        assert_eq!(voice(&Dest::Td3, &mel, 0, 2), (1, 72));
        assert_eq!(voice(&Dest::Td3, &n(0.0, "bass", 12), 0, 1), (0, 24));
        assert_eq!(voice(&Dest::Td3, &n(0.0, "bass", 40), 0, 1), (0, 40));
        // a clip transposed up: tonal notes follow, drums don't
        assert_eq!(voice(&Dest::Pd705, &mel, 3, 1), (9, 87));
        assert_eq!(voice(&Dest::Pd705, &n(0.0, "kick", 36), 3, 1), (9, 36));
        // a MIDI port keeps the note's channel unless told
        let any = Dest::Midi {
            port: "x".into(),
            channel: None,
        };
        assert_eq!(voice(&any, &n(0.0, "snare", 38), 0, 1), (9, 38));
        let ch5 = Dest::Midi {
            port: "x".into(),
            channel: Some(5),
        };
        assert_eq!(voice(&ch5, &mel, 0, 1), (4, 84));
        assert_eq!(fold(100, 24, 72), 64);
        assert_eq!(fold(5, 24, 72), 29);
    }

    #[test]
    fn a_mono_plan_keeps_one_note_per_start_and_never_overlaps() {
        let mut chord = Vec::from([
            n(0.0, "melody", 60),
            n(0.0, "melody", 67),
            n(0.0, "melody", 64),
            n(4.0, "melody", 62),
            n(4.0, "bass", 30),
        ]);
        chord[3].len = 20.0;
        let notes = LoopNotes::new(16, chord);
        let p = plan(&notes, &set(&["melody"]), &Dest::Td3);
        assert_eq!(p.iter().map(|x| x.note).collect::<Vec<_>>(), [67, 62]);
        assert!(p[0].len <= 4.0 - GAP + 1e-9);
        // the last one is cut against the first one's next pass
        assert!(p[1].len <= 12.0 - GAP + 1e-9, "{}", p[1].len);
        // a bass kept low
        let b = LoopNotes::new(16, Vec::from([n(0.0, "bass", 40), n(0.0, "bass", 28)]));
        let p = plan(&b, &set(&["bass"]), &Dest::Td3);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].note, 28);
    }

    #[test]
    fn a_poly_plan_keeps_chords_and_cuts_a_key_before_its_next_strike() {
        let mut v = Vec::from([
            n(0.0, "harmony", 60),
            n(0.0, "harmony", 64),
            n(2.0, "harmony", 60),
            n(3.0, "kick", 36),
        ]);
        v[0].len = 8.0;
        let notes = LoopNotes::new(16, v);
        let p = plan(&notes, &set(&["harmony"]), &Dest::Pd705);
        assert_eq!(p.len(), 3);
        let first = p.iter().find(|x| x.note == 60 && x.at == 0.0).unwrap();
        assert!(first.len <= 2.0 - GAP + 1e-9);
        let drums = plan(&notes, &set(&[DRUMS]), &Dest::Pd705);
        assert_eq!(drums.len(), 1);
    }

    #[test]
    fn routes_set_toggle_clear() {
        let mut r = Routes::default();
        assert!(r.set("c1", "melody", "td3", true));
        assert!(!r.set("c1", "melody", "td3", true));
        assert!(r.toggle("c1", "melody", "synth:A"));
        assert!(r.set("c1", "bass", "td3", true));
        let by = r.by_dest("c1");
        assert_eq!(by["td3"], set(&["bass", "melody"]));
        assert_eq!(by["synth:A"], set(&["melody"]));
        assert!(!r.toggle("c1", "melody", "synth:A"));
        assert!(r.set("c1", "melody", "td3", false));
        assert!(r.set("c1", "bass", "td3", false));
        assert!(r.is_empty(), "{r:?}");
        r.set("c1", "melody", "td3", true);
        r.set("c2", "melody", "td3", true);
        r.retain(|c| c == "c2");
        assert_eq!(r.clips(), ["c2"]);
        r.clear(None);
        assert!(r.is_empty());
    }

    #[test]
    fn the_follow_hands_out_each_note_once_across_the_loop_point() {
        // 16 steps at 8 steps a second (120 bpm): 2 s a pass
        let notes = LoopNotes::new(
            16,
            Vec::from([n(0.0, "melody", 60), n(8.0, "melody", 62), n(15.0, "melody", 64)]),
        );
        let mut f = Follow::new(0.0, 14.0, 8.0);
        let mut got: Vec<(f64, u8)> = Vec::new();
        let mut t = 0.0;
        while t < 3.0 {
            // a reading as late as a block, either way
            let jitter = if (t * 60.0) as i64 % 2 == 0 { 0.08 } else { -0.08 };
            f.observe(t, 14.0 + t * 8.0 + jitter, 8.0);
            for d in f.due(&notes.notes, 16.0, t + 0.08) {
                got.push((d.on, d.note.note));
            }
            t += 1.0 / 60.0;
        }
        let keys: Vec<u8> = got.iter().map(|g| g.1).collect();
        // 15 (at 0.125 s), 0 of the next pass (0.25 s), 8 (1.25 s), 15 (2.125 s), 0 (2.25 s)
        assert_eq!(keys, [64, 60, 62, 64, 60]);
        for (want, (on, _)) in [0.125, 0.25, 1.25, 2.125, 2.25].iter().zip(&got) {
            assert!((on - want).abs() < 0.01, "{on} vs {want}");
        }
    }

    #[test]
    fn a_jump_starts_again_without_sending_the_notes_skipped() {
        let notes = LoopNotes::new(16, Vec::from([n(0.0, "melody", 60), n(4.0, "melody", 62)]));
        let mut f = Follow::new(0.0, 0.5, 8.0);
        assert!(f.due(&notes.notes, 16.0, 0.1).is_empty());
        // the clip started again from its top a little later
        assert!(f.observe(0.2, 0.0, 8.0));
        let d = f.due(&notes.notes, 16.0, 0.25);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].note.note, 60);
        // the host stalled for a second: the notes it slept through are dropped, not burst
        assert!(f.observe(1.25, 10.0, 8.0));
        assert!(f.due(&notes.notes, 16.0, 1.3).is_empty());
        assert!(f.done() >= 10.0);
    }

    #[test]
    fn a_loop_read_just_after_its_downbeat_still_sends_the_downbeat() {
        let notes = LoopNotes::new(16, Vec::from([n(0.0, "melody", 60), n(4.0, "melody", 62)]));
        // first read 20 ms in (0.16 of a sixteenth at 120)
        let mut f = Follow::joining(1.02, 0.16, 8.0);
        let d = f.due(&notes.notes, 16.0, 1.1);
        assert_eq!(d.len(), 1);
        assert!((d[0].on - 1.0).abs() < 1e-9, "{}", d[0].on);
        // mid-loop: only what started within the grace
        let mut f = Follow::joining(0.0, 4.5, 8.0);
        assert!(f.due(&notes.notes, 16.0, 0.05).is_empty());
    }

    #[test]
    fn the_offs_follow_the_length_at_the_rate() {
        let mut v = n(0.0, "melody", 60);
        v.len = 2.0;
        let notes = LoopNotes::new(16, Vec::from([v]));
        let mut f = Follow::new(0.0, 0.0, 8.0);
        let d = f.due(&notes.notes, 16.0, 0.05);
        assert_eq!(d.len(), 1);
        assert!((d[0].off - 0.25).abs() < 1e-9);
    }

    #[test]
    fn audio_only_reasons_are_named() {
        assert_eq!(no_notes(&Refusal::NotANet).0, "audio-only");
        assert_eq!(no_notes(&Refusal::NoProvenance).0, "audio-only");
        assert_eq!(no_notes(&Refusal::Macros(Vec::from(["roll".into()]))).0, "macros");
    }
}
