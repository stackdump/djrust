//! Which of the PD705's pads the hub may play (docs/HUB.md, "PD705 kits").
//!
//! A pad on the PD705 can hold a long sample: the Acid House kit (pd705-rs
//! `docs/ACID-HOUSE.md`) puts 4-bar loops (7.7 s) on Pads 1-3. Over MIDI a pad sounds its
//! whole sample on every note-on, whatever the note-off, so a groove or a routed loop that
//! lands on such a pad starts another loop under the drums each time. The hub can't read the
//! loaded kit from the pad (it sends nothing about it), so the player says which kit it is,
//! and the notes of that kit's loop pads are held back from `pd705_steps` and from loop
//! routes to the PD705.

use alloc::string::String;
use alloc::vec::Vec;

use super::PD705_NOTES;

/// Inputs on the pad: Pad1-9, Trig1-4, Foot1-2, numbered 1-15 in [`PD705_NOTES`] order.
pub const INPUTS: u8 = 15;

/// Kits with known loop pads: `(name, loop pads)`. `factory` is every factory kit: one-shots
/// everywhere.
pub const KITS: [(&str, &[u8]); 2] = [("factory", &[]), ("acid-house", &[1, 2, 3])];

/// The kit the hub assumes until told otherwise: the one built for the hub to drive.
pub const DEFAULT_KIT: &str = "acid-house";

/// The input (1-15) a note on channel 10 sounds, by the factory note map.
pub fn input_of(note: u8) -> Option<u8> {
    PD705_NOTES
        .iter()
        .position(|(n, _)| *n == note)
        .map(|i| i as u8 + 1)
}

/// The note that sounds input `pad` (1-15).
pub fn note_of(pad: u8) -> Option<u8> {
    PD705_NOTES
        .get((pad as usize).checked_sub(1)?)
        .map(|(n, _)| *n)
}

/// `1` → `Pad1`, `10` → `Trig1`, `14` → `Foot1`.
pub fn input_name(pad: u8) -> String {
    match pad {
        1..=9 => alloc::format!("Pad{pad}"),
        10..=13 => alloc::format!("Trig{}", pad - 9),
        _ => alloc::format!("Foot{}", pad.saturating_sub(13)),
    }
}

/// The kit loaded on the pad, as far as the hub needs it: which inputs hold loops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kit {
    /// A [`KITS`] name, or `custom`.
    pub name: String,
    /// Inputs (1-15) the hub never plays, sorted, no repeats.
    pub loop_pads: Vec<u8>,
}

impl Default for Kit {
    fn default() -> Self {
        Kit::preset(DEFAULT_KIT).expect("DEFAULT_KIT is in KITS")
    }
}

impl Kit {
    /// A kit from [`KITS`] by name (`acid-house`, `Acid House`, `acid_house` all match).
    pub fn preset(name: &str) -> Option<Kit> {
        let want: String = name
            .trim()
            .chars()
            .map(|c| {
                if c == ' ' || c == '_' {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        KITS.iter().find(|(n, _)| *n == want).map(|(n, pads)| Kit {
            name: String::from(*n),
            loop_pads: pads.to_vec(),
        })
    }

    /// A kit named `custom` with these loop pads (each 1-15).
    pub fn custom(pads: &[i64]) -> Result<Kit, String> {
        let mut v = Vec::new();
        for &p in pads {
            if !(1..=INPUTS as i64).contains(&p) {
                return Err(alloc::format!(
                    "loop pad {p}: inputs are 1-9 (pads), 10-13 (trig 1-4), 14-15 (foot 1-2)"
                ));
            }
            v.push(p as u8);
        }
        v.sort_unstable();
        v.dedup();
        Ok(Kit {
            name: String::from("custom"),
            loop_pads: v,
        })
    }

    /// Whether a note on channel 10 would sound one of this kit's loop pads.
    pub fn holds_back(&self, note: u8) -> bool {
        input_of(note).is_some_and(|p| self.loop_pads.contains(&p))
    }

    /// The notes held back, in loop-pad order.
    pub fn held_notes(&self) -> Vec<u8> {
        self.loop_pads.iter().filter_map(|&p| note_of(p)).collect()
    }

    /// `acid-house (skips Pad1, Pad2, Pad3)`, `factory (plays every pad)`.
    pub fn line(&self) -> String {
        if self.loop_pads.is_empty() {
            return alloc::format!("{} (plays every pad)", self.name);
        }
        let pads: Vec<String> = self.loop_pads.iter().map(|&p| input_name(p)).collect();
        alloc::format!("{} (skips {})", self.name, pads.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_inputs_follow_the_factory_map() {
        assert_eq!(input_of(49), Some(1));
        assert_eq!(input_of(57), Some(2));
        assert_eq!(input_of(51), Some(3));
        assert_eq!(input_of(36), Some(7));
        assert_eq!(input_of(35), Some(15));
        assert_eq!(input_of(60), None);
        for p in 1..=INPUTS {
            assert_eq!(input_of(note_of(p).unwrap()), Some(p));
        }
        assert_eq!(note_of(0), None);
        assert_eq!(note_of(16), None);
        assert_eq!(input_name(9), "Pad9");
        assert_eq!(input_name(13), "Trig4");
        assert_eq!(input_name(15), "Foot2");
    }

    #[test]
    fn acid_house_holds_back_the_loop_pads_only() {
        let k = Kit::preset("Acid House").unwrap();
        assert_eq!(k, Kit::default());
        assert_eq!(k.held_notes(), [49, 57, 51]);
        for n in [49, 57, 51] {
            assert!(k.holds_back(n));
        }
        // the groove's notes and anything off the map go through
        for n in [36, 38, 42, 48, 45, 43, 35, 44, 60] {
            assert!(!k.holds_back(n), "{n}");
        }
        assert_eq!(k.line(), "acid-house (skips Pad1, Pad2, Pad3)");
    }

    #[test]
    fn factory_plays_everything_and_custom_is_checked() {
        let f = Kit::preset("factory").unwrap();
        assert!((0..128).all(|n| !f.holds_back(n)));
        assert_eq!(f.line(), "factory (plays every pad)");
        assert_eq!(Kit::preset("nope"), None);
        let c = Kit::custom(&[3, 1, 3, 14]).unwrap();
        assert_eq!(c.loop_pads, [1, 3, 14]);
        assert!(c.holds_back(44));
        assert!(Kit::custom(&[0]).is_err());
        assert!(Kit::custom(&[16]).is_err());
    }
}
