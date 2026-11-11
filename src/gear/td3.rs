//! A Behringer TD-3 pattern as the hub edits it: 16 steps of note, transpose, accent, slide and
//! tie / rest, written in td3-rs's text pattern format so the `td3-midi write` subprocess can put
//! it in a slot (docs/HUB.md M7: djrust never links td3-rs, which is GPL; the text file is the
//! interface). The format here follows td3-rs `crates/td3-sysex/src/text.rs`'s documented rules
//! (rows `Note`, `Transpose`, `Accent`, `Slide`, `Tie/Rest`, 16 comma-separated cells each).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::rng::Mulberry32;

/// Steps in a TD-3 pattern.
pub const STEPS: usize = 16;

/// The note cells the TD-3 knows: C .. B and the C an octave up.
pub const NOTES: [&str; 13] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B", "C^",
];

/// A step's octave switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transpose {
    Down,
    #[default]
    None,
    Up,
}

impl Transpose {
    fn cell(self) -> &'static str {
        match self {
            Transpose::Down => "DN",
            Transpose::None => "",
            Transpose::Up => "UP",
        }
    }
    fn octaves(self) -> i32 {
        match self {
            Transpose::Down => -1,
            Transpose::None => 0,
            Transpose::Up => 1,
        }
    }
}

/// One step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Td3Step {
    /// 0..=12: an index into [`NOTES`].
    pub note: u8,
    pub transpose: Transpose,
    pub accent: bool,
    pub slide: bool,
    /// Held into the next step (`TI`).
    pub tie: bool,
    /// Silent (`RE`).
    pub rest: bool,
}

impl Td3Step {
    /// The MIDI note the TD-3 plays for it with `base` as the pattern's C (36 = C2 is the
    /// TD-3's middle octave): for previewing on a synth voice or playing live.
    pub fn midi_note(&self, base: u8) -> u8 {
        let n = base as i32 + self.note.min(12) as i32 + 12 * self.transpose.octaves();
        n.clamp(0, 127) as u8
    }

    fn tie_rest_cell(&self) -> &'static str {
        match (self.tie, self.rest) {
            (false, false) => "",
            (true, false) => "TI",
            (false, true) => "RE",
            (true, true) => "TR",
        }
    }
}

/// A TD-3 pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Td3Pattern {
    pub steps: [Td3Step; STEPS],
    /// Steps that play, 1..=16.
    pub active: u8,
    pub triplet: bool,
}

impl Default for Td3Pattern {
    fn default() -> Self {
        Td3Pattern {
            steps: [Td3Step::default(); STEPS],
            active: STEPS as u8,
            triplet: false,
        }
    }
}

impl Td3Pattern {
    /// The text file `td3-midi write` takes.
    pub fn to_text(&self) -> String {
        let cells = |f: &dyn Fn(&Td3Step) -> &'static str| -> String {
            let v: Vec<String> = self.steps.iter().map(|s| format!("{:>2}", f(s))).collect();
            v.join(", ")
        };
        let nums: Vec<String> = (1..=STEPS).map(|i| format!("{i:02}")).collect();
        let mut out = String::from("TD-3 Pattern\n");
        out.push_str(&format!(
            "Active Steps: {}, Triplet Time: {}\n\n",
            self.active.clamp(1, STEPS as u8),
            if self.triplet { "On" } else { "Off" }
        ));
        out.push_str(&format!("// Step:    {}\n", nums.join(", ")));
        let row = |label: &str, body: String| format!("{:<11}{}\n", format!("{label}:"), body);
        out.push_str(&row("Note", cells(&|s| NOTES[s.note.min(12) as usize])));
        out.push_str(&row("Transpose", cells(&|s| s.transpose.cell())));
        out.push_str(&row("Accent", cells(&|s| if s.accent { "AC" } else { "" })));
        out.push_str(&row("Slide", cells(&|s| if s.slide { "SL" } else { "" })));
        out.push_str(&row("Tie/Rest", cells(&|s| s.tie_rest_cell())));
        out
    }

    /// An acid line from `seed`: notes from a minor-pentatonic set over the root `root` (0 = C),
    /// a few octave jumps, accents on about a quarter of the steps, slides on about a fifth,
    /// rests on about a sixth (never the first step). Same seed, same line.
    pub fn acid(seed: f64, root: u8) -> Td3Pattern {
        // minor pentatonic plus the octave, as offsets from the root
        const SCALE: [u8; 6] = [0, 3, 5, 7, 10, 12];
        let mut r = Mulberry32::new(seed);
        let mut p = Td3Pattern::default();
        for (i, s) in p.steps.iter_mut().enumerate() {
            let degree = SCALE[r.next_int(SCALE.len())];
            let n = (root % 12) + degree;
            let (note, up) = if n > 12 { (n - 12, true) } else { (n, false) };
            s.note = note;
            let jump = r.next();
            s.transpose = if up || jump > 0.88 {
                Transpose::Up
            } else if jump < 0.12 {
                Transpose::Down
            } else {
                Transpose::None
            };
            s.accent = r.next() < 0.25;
            s.slide = r.next() < 0.2;
            s.rest = i > 0 && r.next() < 0.16;
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_has_the_rows_td3_reads() {
        let mut p = Td3Pattern::default();
        p.steps[0] = Td3Step {
            note: 3,
            accent: true,
            ..Default::default()
        };
        p.steps[1] = Td3Step {
            note: 12,
            transpose: Transpose::Up,
            slide: true,
            tie: true,
            ..Default::default()
        };
        p.steps[2].rest = true;
        let t = p.to_text();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "TD-3 Pattern");
        assert_eq!(lines[1], "Active Steps: 16, Triplet Time: Off");
        assert!(lines[3].starts_with("// Step:    01, 02"));
        for (i, label) in ["Note:", "Transpose:", "Accent:", "Slide:", "Tie/Rest:"]
            .iter()
            .enumerate()
        {
            let l = lines[4 + i];
            assert!(l.starts_with(label), "{l}");
            assert_eq!(l.split(',').count(), 16, "{l}");
        }
        let cells = |row: usize| -> Vec<String> {
            let body = lines[4 + row].split_once(':').unwrap().1;
            body.split(',').map(|c| c.trim().into()).collect()
        };
        assert_eq!(cells(0)[..3], ["D#", "C^", "C"]);
        assert_eq!(cells(1)[1], "UP");
        assert_eq!(cells(2)[0], "AC");
        assert_eq!(cells(3)[1], "SL");
        assert_eq!(cells(4)[..3], ["", "TI", "RE"]);
    }

    #[test]
    fn acid_is_deterministic_and_in_range() {
        let a = Td3Pattern::acid(7.0, 9);
        assert_eq!(a, Td3Pattern::acid(7.0, 9));
        assert_ne!(a, Td3Pattern::acid(8.0, 9));
        assert!(!a.steps[0].rest);
        assert!(a.steps.iter().all(|s| s.note <= 12));
        assert!(a.steps.iter().any(|s| s.accent) && a.steps.iter().any(|s| s.slide));
        // every pitch is in A minor pentatonic
        for s in &a.steps {
            let rel = (s.note as i32 - 9).rem_euclid(12);
            assert!([0, 3, 5, 7, 10].contains(&rel), "{s:?}");
        }
    }

    #[test]
    fn midi_notes() {
        let s = Td3Step {
            note: 12,
            transpose: Transpose::Up,
            ..Default::default()
        };
        assert_eq!(s.midi_note(36), 36 + 12 + 12);
        assert_eq!(Td3Step::default().midi_note(36), 36);
    }
}
