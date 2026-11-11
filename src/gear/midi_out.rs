//! MIDI-out targets: a control of any device sends a note, CC or program change to a piece of
//! gear (a pad plays the PD705, a key plays the TD-3, a knob sets a CC). Pure: [`MidiOut::parse`]
//! reads a binding's `args`, [`MidiOut::on_press`] / [`MidiOut::on_release`] give the bytes.
//!
//! Binding args (all optional but `device`):
//!
//! | arg | meaning |
//! |---|---|
//! | `device` | a gear id (`td3`, `pd705`) or part of a port name |
//! | `channel` | 1..16; the gear's own note channel when it has one, else 1 |
//! | `type` | `note` (default), `cc`, `pc` |
//! | `note` | fixed note (default: the control's index, i.e. a key plays its own note) |
//! | `transpose` | semitones added to the note, -48..48 |
//! | `velocity` | 1..127 fixed (default 100); `velocity=touch` follows the control's value |
//! | `controller` | CC number for `type=cc` (default 1); the value follows the control |
//! | `program` | program number for `type=pc` (default: the control's index) |
//! | `hold` | `gate` (default: note-off on release) or a length in ms |
//!
//! Only 2- and 3-byte channel messages are built, never SysEx: a MIDI-out binding cannot write
//! a pattern or change a setting on a device.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Note,
    Cc,
    Pc,
}

/// How long a note sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Until the control is released.
    Gate,
    /// This many ms, whatever the control does.
    Ms(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MidiOut {
    pub device: String,
    pub kind: Kind,
    /// 0-based.
    pub channel: u8,
    pub note: Option<u8>,
    pub transpose: i32,
    pub velocity: Option<u8>,
    pub touch: bool,
    pub controller: u8,
    pub program: Option<u8>,
    pub hold: Hold,
}

fn num(args: &BTreeMap<String, String>, k: &str) -> Option<i64> {
    args.get(k).and_then(|v| v.trim().parse::<i64>().ok())
}

impl MidiOut {
    pub fn parse(args: &BTreeMap<String, String>) -> Result<MidiOut, String> {
        let device = args
            .get("device")
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .ok_or("midiOut needs a device")?;
        let kind = match args.get("type").map(|t| t.to_lowercase()).as_deref() {
            None | Some("note") => Kind::Note,
            Some("cc") => Kind::Cc,
            Some("pc") | Some("program") => Kind::Pc,
            Some(o) => return Err(alloc::format!("midiOut type {o}: note, cc or pc")),
        };
        let default_ch = super::spec(&device).and_then(|g| g.note_channel).unwrap_or(0);
        let channel = match num(args, "channel") {
            Some(c) if (1..=16).contains(&c) => (c - 1) as u8,
            Some(c) => return Err(alloc::format!("midiOut channel {c}: 1..16")),
            None => default_ch,
        };
        let velocity_arg = args.get("velocity").map(|v| v.trim().to_lowercase());
        let touch = velocity_arg.as_deref() == Some("touch");
        let velocity = match (&velocity_arg, touch) {
            (Some(_), true) | (None, _) => None,
            (Some(v), false) => Some(
                v.parse::<i64>()
                    .ok()
                    .filter(|n| (1..=127).contains(n))
                    .ok_or("midiOut velocity: 1..127 or touch")? as u8,
            ),
        };
        let hold = match args.get("hold").map(|h| h.trim().to_lowercase()).as_deref() {
            None | Some("gate") => Hold::Gate,
            Some(ms) => Hold::Ms(
                ms.parse::<u32>()
                    .ok()
                    .filter(|n| (1..=10_000).contains(n))
                    .ok_or("midiOut hold: gate or 1..10000 ms")?,
            ),
        };
        Ok(MidiOut {
            device,
            kind,
            channel,
            note: num(args, "note").map(|n| n.clamp(0, 127) as u8),
            transpose: num(args, "transpose").unwrap_or(0).clamp(-48, 48) as i32,
            velocity,
            touch,
            controller: num(args, "controller").unwrap_or(1).clamp(0, 127) as u8,
            program: num(args, "program").map(|n| n.clamp(0, 127) as u8),
            hold,
        })
    }

    fn note_for(&self, index: usize) -> u8 {
        let n = self.note.map_or(index.min(127) as i32, i32::from) + self.transpose;
        n.clamp(0, 127) as u8
    }

    /// The bytes for a press (or, for a continuous control, a move) of control `index` at `value`
    /// (0...1).
    pub fn on_press(&self, index: usize, value: f64) -> Vec<u8> {
        // (libm: `f64::round` is std's, and djengine builds for no_std and wasm32)
        let v7 = libm::round(value.clamp(0.0, 1.0) * 127.0) as u8;
        match self.kind {
            Kind::Note => {
                let vel = match (self.velocity, self.touch) {
                    (Some(v), _) => v,
                    (None, true) => v7.max(1),
                    (None, false) => 100,
                };
                alloc::vec![0x90 | self.channel, self.note_for(index), vel]
            }
            Kind::Cc => alloc::vec![0xB0 | self.channel, self.controller, v7],
            Kind::Pc => alloc::vec![0xC0 | self.channel, self.program.unwrap_or(index.min(127) as u8)],
        }
    }

    /// The note-off for a release (only a gated note has one).
    pub fn on_release(&self, index: usize) -> Option<Vec<u8>> {
        (self.kind == Kind::Note && self.hold == Hold::Gate)
            .then(|| alloc::vec![0x80 | self.channel, self.note_for(index), 0])
    }

    /// The note-off to send after `ms` for a fixed-length note.
    pub fn timed_off(&self, index: usize) -> Option<(u32, Vec<u8>)> {
        match (self.kind, self.hold) {
            (Kind::Note, Hold::Ms(ms)) => {
                Some((ms, alloc::vec![0x80 | self.channel, self.note_for(index), 0]))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_pad_plays_the_pd705_on_its_own_channel() {
        let m = MidiOut::parse(&args(&[("device", "pd705"), ("note", "38")])).unwrap();
        assert_eq!(m.on_press(0, 1.0), [0x99, 38, 100]);
        assert_eq!(m.on_release(0), Some(alloc::vec![0x89, 38, 0]));
    }

    #[test]
    fn a_key_plays_its_own_note_transposed() {
        let m = MidiOut::parse(&args(&[("device", "td3"), ("channel", "2"), ("transpose", "-12")])).unwrap();
        assert_eq!(m.on_press(60, 1.0), [0x91, 48, 100]);
        assert_eq!(m.on_release(60), Some(alloc::vec![0x81, 48, 0]));
    }

    #[test]
    fn velocity_touch_follows_the_value_and_timed_notes_have_no_release() {
        let m = MidiOut::parse(&args(&[("device", "td3"), ("velocity", "touch"), ("hold", "120")])).unwrap();
        assert_eq!(m.on_press(40, 0.5)[2], 64);
        assert_eq!(m.on_release(40), None);
        assert_eq!(m.timed_off(40), Some((120, alloc::vec![0x80, 40, 0])));
    }

    #[test]
    fn cc_and_program_change() {
        let cc = MidiOut::parse(&args(&[("device", "td3"), ("type", "cc"), ("controller", "74")])).unwrap();
        assert_eq!(cc.on_press(0, 1.0), [0xB0, 74, 127]);
        assert_eq!(cc.on_release(0), None);
        let pc = MidiOut::parse(&args(&[("device", "x"), ("type", "pc"), ("program", "5"), ("channel", "16")])).unwrap();
        assert_eq!(pc.on_press(0, 1.0), [0xCF, 5]);
    }

    #[test]
    fn bad_args_are_errors_and_nothing_is_sysex() {
        assert!(MidiOut::parse(&args(&[])).is_err());
        assert!(MidiOut::parse(&args(&[("device", "td3"), ("channel", "17")])).is_err());
        assert!(MidiOut::parse(&args(&[("device", "td3"), ("type", "sysex")])).is_err());
        assert!(MidiOut::parse(&args(&[("device", "td3"), ("velocity", "0")])).is_err());
        let m = MidiOut::parse(&args(&[("device", "td3")])).unwrap();
        for i in [0usize, 60, 127, 500] {
            assert_ne!(m.on_press(i, 1.0)[0], 0xF0);
            assert!(m.on_press(i, 1.0)[1] <= 127);
        }
    }
}
