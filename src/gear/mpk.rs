//! The Akai MPK Mini Mk II's pads (docs/MPK.md), which are the one control on the unit whose
//! MIDI depends on a mode the hub cannot set:
//!
//! * **PROG mode** (the unit's `PROG CHANGE` button lit, or a preset whose pads are Program
//!   Change): a pad sends Program Change 0-7 and bank B repeats bank A, so 16 pads are 8.
//! * **NOTE mode**: a pad sends a note, banks A and B are different notes, normally on MIDI
//!   channel 10. The profile's `padNotes` group says which notes; they were never captured from
//!   this unit (`[UNVERIFIED]`), so [`PadLearn`] records them as they are pressed.
//!
//! Pure: [`PadWatch`] reads the messages the host passes in and says which mode it saw;
//! [`PadLearn`] collects the 16 note numbers; [`PadMap`] is what is saved (JSON
//! `djrust/pads/v1`) and laid over the profile's `padNotes` group with [`PadMap::apply`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::jog::ProfileError;
use crate::control::surface::{Group, SurfaceProfile};
use crate::json::{JsonObject, JsonValue};

/// The pads: 8 in each of banks A and B.
pub const PAD_COUNT: usize = 16;
/// The profile group the pads' notes live in.
pub const GROUP: &str = "padNotes";
/// The factory channel (0-based: channel 10) the pads send notes on.
pub const FACTORY_CHANNEL: u8 = 9;
/// The factory first note of bank A (bank B continues from it).
pub const FACTORY_FIRST: u8 = 36;
/// The schema of a saved pad map.
pub const SCHEMA: &str = "djrust/pads/v1";

/// What the pads are sending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PadMode {
    /// No pad has been pressed yet (or not since the unit was plugged in).
    #[default]
    Unknown,
    /// Program Change: bank B repeats bank A.
    Prog,
    /// Notes: 16 distinct pads.
    Note,
}

impl PadMode {
    pub fn label(self) -> &'static str {
        match self {
            PadMode::Unknown => "unknown (press a pad)",
            PadMode::Prog => "PROG",
            PadMode::Note => "NOTE",
        }
    }
}

/// Which mode the last pad press came from. The latest press wins, so switching the unit's mode
/// while djapp runs is followed on the next pad.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadWatch {
    pub mode: PadMode,
    /// Program Changes seen since the watch began.
    pub prog_seen: u32,
    /// Pad notes seen since the watch began.
    pub note_seen: u32,
    /// The last Program Change number (0-based).
    pub last_program: Option<u8>,
}

impl PadWatch {
    pub fn new() -> PadWatch {
        PadWatch::default()
    }

    /// One message from the unit. `pad_channel` is the channel the pads' notes are on (the
    /// profile's `padNotes` group), `pad_notes` the notes it holds. Returns true when the mode
    /// changed.
    pub fn observe(&mut self, bytes: &[u8], pad_channel: u8, pad_notes: &[u8]) -> bool {
        let Some(&status) = bytes.first() else {
            return false;
        };
        let before = self.mode;
        match status & 0xF0 {
            0xC0 => {
                if let Some(&p) = bytes.get(1) {
                    self.prog_seen += 1;
                    self.last_program = Some(p);
                    self.mode = PadMode::Prog;
                }
            }
            0x90 => {
                let (Some(&n), Some(&v)) = (bytes.get(1), bytes.get(2)) else {
                    return false;
                };
                // the keys are on channel 0; a note on the pads' channel is a pad. If the pads
                // were learned onto channel 0 as well, the notes themselves say it.
                let on_pads = status & 0x0F == pad_channel
                    && (pad_channel != 0 || pad_notes.contains(&n));
                if v > 0 && on_pads {
                    self.note_seen += 1;
                    self.mode = PadMode::Note;
                }
            }
            _ => {}
        }
        self.mode != before
    }

    /// The on-screen notice for the current mode: `Some` while the pads are in PROG mode.
    pub fn notice(&self) -> Option<String> {
        (self.mode == PadMode::Prog).then(|| NOTICE.to_string())
    }
}

/// What the screen says in PROG mode.
pub const NOTICE: &str = "MPK pads are in PROG mode: they send Program Change and bank B repeats \
bank A, so only 8 pads exist and the 16-pad presets (loop clips, mutes, vocals) do not apply. \
Switch the MPK to NOTE mode: press its PROG CHANGE button so the light goes out (or, in the \
MPK Editor, set the pads' type to Note), then hit \"Learn pad notes\" once so djapp knows which \
notes your unit sends.";

/// What one pad press did to a [`PadLearn`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LearnStep {
    /// Not a pad press (a release, a CC, another channel than the first pad's).
    Ignored,
    /// Pad `index` (0-based over A then B) is note `note`.
    Got { index: usize, note: u8 },
    /// That note is already recorded for another pad: press the next pad.
    Duplicate { note: u8 },
    /// A Program Change: the unit is still in PROG mode, so no note will arrive.
    ProgMode,
}

/// Records the 16 pads' note numbers as they are pressed: bank A pad 1..8, then bank B pad
/// 1..8. The first note fixes the channel; a note on another channel is ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct PadLearn {
    pub count: usize,
    pub channel: Option<u8>,
    pub notes: Vec<u8>,
}

impl PadLearn {
    pub fn new(count: usize) -> PadLearn {
        PadLearn {
            count: count.clamp(1, 64),
            channel: None,
            notes: Vec::new(),
        }
    }

    pub fn sixteen() -> PadLearn {
        PadLearn::new(PAD_COUNT)
    }

    pub fn done(&self) -> bool {
        self.notes.len() >= self.count
    }

    /// The pad to press next (0-based), None when done.
    pub fn next_pad(&self) -> Option<usize> {
        (!self.done()).then_some(self.notes.len())
    }

    /// The words for the pad to press next.
    pub fn prompt(&self) -> String {
        match self.next_pad() {
            None => "all pads learned".to_string(),
            Some(i) => alloc::format!(
                "press pad {} of bank {} ({} of {})",
                i % 8 + 1,
                if i < 8 { 'A' } else { 'B' },
                i + 1,
                self.count
            ),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> LearnStep {
        let Some(&status) = bytes.first() else {
            return LearnStep::Ignored;
        };
        if status & 0xF0 == 0xC0 {
            return LearnStep::ProgMode;
        }
        if status & 0xF0 != 0x90 || self.done() {
            return LearnStep::Ignored;
        }
        let (Some(&n), Some(&v)) = (bytes.get(1), bytes.get(2)) else {
            return LearnStep::Ignored;
        };
        if v == 0 {
            return LearnStep::Ignored;
        }
        let ch = status & 0x0F;
        match self.channel {
            // the first note may have been a stray key (the keys are on channel 1, the pads
            // normally on 10): a note on another channel before a second pad is in starts over
            // there. After that the channel is fixed and a key is just ignored.
            Some(c) if c != ch && self.notes.len() == 1 => {
                self.notes.clear();
                self.channel = Some(ch);
            }
            Some(c) if c != ch => return LearnStep::Ignored,
            None => self.channel = Some(ch),
            _ => {}
        }
        if self.notes.contains(&n) {
            return LearnStep::Duplicate { note: n };
        }
        self.notes.push(n);
        LearnStep::Got {
            index: self.notes.len() - 1,
            note: n,
        }
    }

    /// The finished map, None until every pad was pressed.
    pub fn result(&self, device: &str) -> Option<PadMap> {
        self.done().then(|| PadMap {
            device: device.to_string(),
            channel: self.channel.unwrap_or(FACTORY_CHANNEL),
            notes: self.notes.clone(),
        })
    }
}

/// The learned pad notes of one device: what replaces the profile's `[UNVERIFIED]` `padNotes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadMap {
    pub device: String,
    /// 0-based MIDI channel.
    pub channel: u8,
    /// One note per pad, bank A 1..8 then bank B 1..8.
    pub notes: Vec<u8>,
}

impl PadMap {
    /// The factory numbers the profile ships (consecutive from 36 on channel 10).
    pub fn factory(device: &str) -> PadMap {
        PadMap {
            device: device.to_string(),
            channel: FACTORY_CHANNEL,
            notes: (0..PAD_COUNT as u8).map(|i| FACTORY_FIRST + i).collect(),
        }
    }

    pub fn to_json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("device", JsonValue::str(self.device.clone())),
            ("channel", JsonValue::int(i64::from(self.channel))),
            (
                "notes",
                JsonValue::Array(self.notes.iter().map(|n| JsonValue::int(i64::from(*n))).collect()),
            ),
        ]))
    }

    pub fn from_json(j: &JsonValue) -> Result<PadMap, ProfileError> {
        if j.get("schema").and_then(JsonValue::as_str) != Some(SCHEMA) {
            return Err(ProfileError("not a djrust pad map".into()));
        }
        let device = j
            .get("device")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ProfileError("pads: device".into()))?
            .to_string();
        let channel = j
            .get("channel")
            .and_then(JsonValue::as_f64)
            .filter(|c| (0.0..16.0).contains(c))
            .ok_or_else(|| ProfileError("pads: channel 0-15".into()))? as u8;
        let mut notes = Vec::new();
        for n in j
            .get("notes")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| ProfileError("pads: notes".into()))?
        {
            let n = n
                .as_f64()
                .filter(|n| (0.0..128.0).contains(n))
                .ok_or_else(|| ProfileError("pads: a note is 0-127".into()))?;
            notes.push(n as u8);
        }
        if notes.is_empty() || notes.len() > 64 {
            return Err(ProfileError("pads: 1-64 notes".into()));
        }
        Ok(PadMap {
            device,
            channel,
            notes,
        })
    }

    pub fn parse(text: &str) -> Result<PadMap, ProfileError> {
        PadMap::from_json(&JsonValue::parse(text).map_err(|e| ProfileError(e.to_string()))?)
    }

    /// The profile's `padNotes` group with these numbers and this channel.
    pub fn group(&self, template: Option<&Group>) -> Group {
        let mut g = template.cloned().unwrap_or_else(|| Group {
            id: GROUP.to_string(),
            kind: crate::control::surface::ControlType::Note,
            first: None,
            count: None,
            numbers: None,
            names: None,
            channel: None,
            comment: None,
        });
        g.first = None;
        g.count = None;
        g.numbers = Some(self.notes.clone());
        g.channel = Some(self.channel);
        g.comment = Some(alloc::format!(
            "learned from the unit: channel {}, notes {:?}, bank A then bank B",
            self.channel + 1,
            self.notes
        ));
        g
    }

    /// `profile` with its `padNotes` group replaced.
    pub fn apply(&self, profile: &SurfaceProfile) -> SurfaceProfile {
        let mut p = profile.clone();
        let g = self.group(p.groups.iter().find(|g| g.id == GROUP));
        match p.groups.iter().position(|x| x.id == GROUP) {
            Some(i) => p.groups[i] = g,
            None => p.groups.push(g),
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::profiles;

    fn on(ch: u8, n: u8) -> [u8; 3] {
        [0x90 | ch, n, 100]
    }

    #[test]
    fn a_program_change_is_prog_mode_and_a_pad_note_is_note_mode() {
        let mut w = PadWatch::new();
        assert_eq!(w.mode, PadMode::Unknown);
        assert!(w.notice().is_none(), "nothing pressed yet: no nagging");
        assert!(w.observe(&[0xC0, 3], 9, &[]));
        assert_eq!(w.mode, PadMode::Prog);
        assert!(w.notice().unwrap().contains("NOTE mode"));
        // bank B sends the same number: no change
        assert!(!w.observe(&[0xC0, 3], 9, &[]));
        // a key on channel 1 is not a pad
        assert!(!w.observe(&on(0, 60), 9, &[]));
        assert_eq!(w.mode, PadMode::Prog);
        // the unit is switched to NOTE: the next pad says so and the notice goes
        assert!(w.observe(&on(9, 36), 9, &[]));
        assert_eq!(w.mode, PadMode::Note);
        assert!(w.notice().is_none());
        assert_eq!((w.prog_seen, w.note_seen), (2, 1));
    }

    #[test]
    fn pads_learned_onto_channel_one_are_told_from_the_keys_by_their_notes() {
        let mut w = PadWatch::new();
        assert!(!w.observe(&on(0, 60), 0, &[10, 11]));
        assert!(w.observe(&on(0, 10), 0, &[10, 11]));
        assert_eq!(w.mode, PadMode::Note);
        // a note-on with velocity 0 is a release
        let mut w = PadWatch::new();
        assert!(!w.observe(&[0x99, 36, 0], 9, &[]));
    }

    #[test]
    fn learn_takes_sixteen_notes_in_order_and_fixes_the_channel() {
        let mut l = PadLearn::sixteen();
        assert_eq!(l.prompt(), "press pad 1 of bank A (1 of 16)");
        // the first press fixes channel 3; a key on channel 1 and a release are ignored
        assert_eq!(l.feed(&on(2, 40)), LearnStep::Got { index: 0, note: 40 });
        assert_eq!(l.feed(&[0x82, 40, 0]), LearnStep::Ignored);
        assert_eq!(l.feed(&[0x92, 41, 0]), LearnStep::Ignored);
        // the same pad twice (a bounce): not recorded again
        assert_eq!(l.feed(&on(2, 40)), LearnStep::Duplicate { note: 40 });
        assert_eq!(l.feed(&on(2, 41)), LearnStep::Got { index: 1, note: 41 });
        // with two pads in, a key on channel 1 is just ignored
        assert_eq!(l.feed(&on(0, 61)), LearnStep::Ignored);
        for (i, n) in (42..=55).enumerate() {
            assert_eq!(
                l.feed(&on(2, n)),
                LearnStep::Got {
                    index: i + 2,
                    note: n
                }
            );
        }
        assert!(l.done());
        assert_eq!(l.prompt(), "all pads learned");
        assert_eq!(l.feed(&on(2, 99)), LearnStep::Ignored, "no 17th pad");
        let m = l.result("MPK Mini Mk II").unwrap();
        assert_eq!(m.channel, 2);
        assert_eq!(m.notes, (40..=55).collect::<Vec<u8>>());
    }

    #[test]
    fn a_stray_key_before_the_first_pad_does_not_fix_the_wrong_channel() {
        let mut l = PadLearn::sixteen();
        // a key on channel 1, then the pads on channel 10
        assert_eq!(l.feed(&on(0, 60)), LearnStep::Got { index: 0, note: 60 });
        assert_eq!(l.feed(&on(9, 36)), LearnStep::Got { index: 0, note: 36 }, "starts over on the pads' channel");
        assert_eq!(l.channel, Some(9));
        assert_eq!(l.notes, [36]);
        assert_eq!(l.feed(&on(9, 37)), LearnStep::Got { index: 1, note: 37 });
        // once two pads are in, a key is ignored
        assert_eq!(l.feed(&on(0, 62)), LearnStep::Ignored);
        assert_eq!(l.notes, [36, 37]);
    }

    #[test]
    fn learn_says_so_when_the_unit_is_still_in_prog_mode() {
        let mut l = PadLearn::sixteen();
        assert_eq!(l.feed(&[0xC0, 0]), LearnStep::ProgMode);
        assert!(l.notes.is_empty());
        assert!(l.result("x").is_none());
    }

    #[test]
    fn bank_b_prompts_name_the_bank() {
        let mut l = PadLearn::sixteen();
        for n in 0..8u8 {
            l.feed(&on(9, 50 + n));
        }
        assert_eq!(l.prompt(), "press pad 1 of bank B (9 of 16)");
    }

    #[test]
    fn a_pad_map_round_trips_and_replaces_the_profiles_pad_notes() {
        let m = PadMap {
            device: "MPK Mini Mk II".into(),
            channel: 2,
            notes: (60..76).collect(),
        };
        let back = PadMap::parse(&m.to_json().js_stringified()).unwrap();
        assert_eq!(back, m);
        let base = profiles::mpk_mini_mk2();
        let p = m.apply(&base);
        let g = p.groups.iter().find(|g| g.id == GROUP).unwrap();
        assert_eq!(g.channel, Some(2));
        assert_eq!(g.control_numbers(), (60..76).collect::<Vec<u8>>());
        // the layout (what each pad does) is the profile's, untouched
        assert_eq!(p.layout, base.layout);
        // and the decoder reads the learned notes, not 36..51
        let mut s = crate::control::controller::SurfaceController::new(alloc::sync::Arc::new(p));
        let e = s.decode(&[0x92, 63, 100]).unwrap();
        assert_eq!((e.group.as_str(), e.index), (GROUP, 3));
        assert!(s.decode(&[0x99, 39, 100]).is_none(), "the factory note is gone");
    }

    #[test]
    fn a_bad_pad_file_is_refused() {
        assert!(PadMap::parse("{}").is_err());
        assert!(PadMap::parse(r#"{"schema":"djrust/pads/v1","device":"x","channel":16,"notes":[1]}"#).is_err());
        assert!(PadMap::parse(r#"{"schema":"djrust/pads/v1","device":"x","channel":1,"notes":[200]}"#).is_err());
        assert!(PadMap::parse(r#"{"schema":"djrust/pads/v1","device":"x","channel":1,"notes":[]}"#).is_err());
    }

    #[test]
    fn the_factory_map_is_the_profiles_group() {
        let f = PadMap::factory("MPK Mini Mk II");
        let base = profiles::mpk_mini_mk2();
        let g = base.groups.iter().find(|g| g.id == GROUP).unwrap();
        assert_eq!(g.control_numbers(), f.notes);
        assert_eq!(g.channel, Some(f.channel));
    }
}
