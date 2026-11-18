//! A formant-synthesis voice: stylised, robotic singing with no recorded audio (beyond Swift;
//! docs/VOICE.md in djrust-vevr-se).
//!
//! A source (a band-limited saw, a pulse, or a glottal pulse) runs through five formant
//! resonators, in cascade (a Klatt-style serial vocal tract: the formants' relative levels come
//! out of the filter by themselves) or in parallel (each formant its own band-pass and level,
//! alternating signs, as in Klatt's parallel branch). Each note sings a vowel (a e i o u, plus
//! the choir's "ah" and "oo") or a syllable (a consonant onset into a vowel: `da`, `ro`, `moo`),
//! and the formants glide from wherever they were, so vowels morph smoothly from note to note.
//! A choir is the same note sung by up to [`MAX_UNISON`] copies, detuned, each with its own
//! vibrato phase and slow drift, over two resonator banks a few percent apart (two "singers"'
//! vocal tracts), so it smears like a section instead of a single synth.
//!
//! Pure DSP, `no_std`, no allocation after construction: the wave runner's `"formant"` engine
//! (one [`FormantVoice`] per pooled voice) and the vocoder's voice carrier ([`FormantSynth`])
//! both drive it a sample at a time.
//!
//! # Sources
//!
//! - The vowel table's F1-F3 are the adult-male averages of Peterson & Barney (1952), "Control
//!   methods used in a study of the vowels", JASA 24(2): published measurements. F4 / F5 and the
//!   bandwidths are the defaults of Klatt's synthesiser as described in Klatt (1980), "Software
//!   for a cascade/parallel formant synthesizer", JASA 67(3).
//! - The resonator is Klatt's (1980) digital resonator, `y = A x + B y1 + C y2`.
//! - The glottal pulse is the derivative of Rosenberg's (1971) trigonometric pulse, "Effect of
//!   glottal pulse shape on the quality of natural vowels", JASA 49(2B).
//! - The consonant loci are textbook values (formant transitions toward a place of
//!   articulation), not copied from any synthesiser's source.
//!
//! Nothing here is derived from espeak-ng or any other GPL code.

use alloc::string::String;
use core::f64::consts::PI;

/// Formants per resonator bank.
pub const N_FORMANTS: usize = 5;
/// The most unison copies (choir singers) a voice runs.
pub const MAX_UNISON: usize = 8;
/// A diphthong's second vowel starts this long after the note, ms, and takes this time constant.
const GLIDE_AT_MS: f64 = 110.0;
const GLIDE_MS: f64 = 75.0;
/// Samples between coefficient updates (the formant glide, vibrato depth ramp).
pub const CONTROL_PERIOD: u32 = 32;

// MARK: vowels

/// The vowels in knob order: a position `v` in 0..=6 names one, fractions morph between
/// neighbours. Three more (`ae` as in "cat", `ih` as in "bit", `er` as in "bird") sit at 7..=9:
/// the lyrics' vowels (docs/VOICE.md). They are not on the live knob ([`VOWEL_MAX`]). `ah` (as in "but") and `oo` (as in "book") are the choir's open "aah" and
/// rounded "ooh".
pub const VOWEL_NAMES: [&str; 10] = ["a", "e", "i", "o", "u", "ah", "oo", "ae", "ih", "er"];

/// One vowel: five formant frequencies (Hz), bandwidths (Hz) and parallel-branch levels
/// (linear, the cascade ignores them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VowelShape {
    pub f: [f64; N_FORMANTS],
    pub bw: [f64; N_FORMANTS],
    pub amp: [f64; N_FORMANTS],
}

const fn shape(f: [f64; 3], f4: f64, amp: [f64; 3]) -> VowelShape {
    VowelShape {
        f: [f[0], f[1], f[2], f4, 3750.0],
        bw: [80.0, 100.0, 130.0, 250.0, 300.0],
        amp: [amp[0], amp[1], amp[2], 0.10, 0.05],
    }
}

/// The table (adult male; F1-F3 Peterson & Barney 1952, F4/F5 and bandwidths Klatt 1980).
pub const VOWELS: [VowelShape; 10] = [
    shape([730.0, 1090.0, 2440.0], 3300.0, [1.0, 0.50, 0.25]), // a  (father)
    shape([530.0, 1840.0, 2480.0], 3300.0, [1.0, 0.40, 0.25]), // e  (bed)
    shape([270.0, 2290.0, 3010.0], 3400.0, [1.0, 0.25, 0.20]), // i  (beet)
    shape([570.0, 840.0, 2410.0], 3300.0, [1.0, 0.45, 0.15]),  // o  (bought)
    shape([300.0, 870.0, 2240.0], 3300.0, [1.0, 0.30, 0.10]),  // u  (boot)
    shape([640.0, 1190.0, 2390.0], 3300.0, [1.0, 0.55, 0.25]), // ah (but)
    shape([440.0, 1020.0, 2240.0], 3300.0, [1.0, 0.35, 0.12]), // oo (book)
    shape([660.0, 1720.0, 2410.0], 3300.0, [1.0, 0.40, 0.25]), // ae (cat)
    shape([390.0, 1990.0, 2550.0], 3300.0, [1.0, 0.35, 0.20]), // ih (bit)
    shape([490.0, 1350.0, 1690.0], 3300.0, [1.0, 0.45, 0.30]), // er (bird)
];

/// The highest vowel position on the live knob (`a` .. `oo`); the lyrics' vowels lie beyond it.
pub const VOWEL_MAX: f64 = 6.0;
/// The highest position in [`VOWELS`].
const TABLE_MAX: f64 = (VOWELS.len() - 1) as f64;

/// Vowel position `v` (0 a .. 6 oo), morphed between neighbours: frequencies in log,
/// bandwidths and levels linearly.
pub fn vowel_at(v: f64) -> VowelShape {
    let v = if v.is_finite() {
        v.clamp(0.0, TABLE_MAX)
    } else {
        0.0
    };
    let i = (libm::floor(v) as usize).min(VOWELS.len() - 2);
    let t = v - i as f64;
    let (a, b) = (&VOWELS[i], &VOWELS[i + 1]);
    let mut out = *a;
    for k in 0..N_FORMANTS {
        out.f[k] = a.f[k] * libm::pow(b.f[k] / a.f[k], t);
        out.bw[k] = a.bw[k] + (b.bw[k] - a.bw[k]) * t;
        out.amp[k] = a.amp[k] + (b.amp[k] - a.amp[k]) * t;
    }
    out
}

/// A vowel's position by name (`a e i o u ah oo`, and `aa` / `ee` / `uh` / `ooh` / `aah`
/// spellings), or a number 0..=6 as text.
pub fn parse_vowel(s: &str) -> Option<f64> {
    let s = s.trim();
    let v = match s.to_ascii_lowercase().as_str() {
        "a" => 0.0,
        "e" | "eh" => 1.0,
        "i" | "ee" => 2.0,
        "o" | "aw" => 3.0,
        "u" | "uu" => 4.0,
        "ah" | "aa" | "aah" | "uh" => 5.0,
        "oo" | "ooh" => 6.0,
        "ae" => 7.0,
        "ih" => 8.0,
        "er" => 9.0,
        _ => {
            let x: f64 = s.parse().ok()?;
            return (x.is_finite() && (0.0..=VOWEL_MAX).contains(&x)).then_some(x);
        }
    };
    Some(v)
}

/// The nearest vowel's name.
pub fn vowel_name(v: f64) -> &'static str {
    let v = if v.is_finite() { v } else { 0.0 };
    VOWEL_NAMES[(libm::round(v.clamp(0.0, TABLE_MAX)) as usize).min(VOWEL_NAMES.len() - 1)]
}

// MARK: syllables

/// The consonant a syllable starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum Onset {
    #[default]
    None = 0,
    B,
    D,
    G,
    P,
    T,
    K,
    M,
    N,
    L,
    R,
    W,
    Y,
    S,
    H,
    // the lyrics' consonants (docs/VOICE.md): appended, so every packed code keeps its meaning
    F,
    V,
    Z,
    Sh,
    Zh,
    Ch,
    Jh,
    Th,
    Dh,
    Ng,
}

impl Onset {
    pub const ALL: [Onset; 25] = [
        Onset::None,
        Onset::B,
        Onset::D,
        Onset::G,
        Onset::P,
        Onset::T,
        Onset::K,
        Onset::M,
        Onset::N,
        Onset::L,
        Onset::R,
        Onset::W,
        Onset::Y,
        Onset::S,
        Onset::H,
        Onset::F,
        Onset::V,
        Onset::Z,
        Onset::Sh,
        Onset::Zh,
        Onset::Ch,
        Onset::Jh,
        Onset::Th,
        Onset::Dh,
        Onset::Ng,
    ];

    pub fn letter(self) -> &'static str {
        match self {
            Onset::None => "",
            Onset::B => "b",
            Onset::D => "d",
            Onset::G => "g",
            Onset::P => "p",
            Onset::T => "t",
            Onset::K => "k",
            Onset::M => "m",
            Onset::N => "n",
            Onset::L => "l",
            Onset::R => "r",
            Onset::W => "w",
            Onset::Y => "y",
            Onset::S => "s",
            Onset::H => "h",
            Onset::F => "f",
            Onset::V => "v",
            Onset::Z => "z",
            Onset::Sh => "sh",
            Onset::Zh => "zh",
            Onset::Ch => "ch",
            Onset::Jh => "j",
            Onset::Th => "th",
            Onset::Dh => "dh",
            Onset::Ng => "ng",
        }
    }

    /// The onset spelled `s` exactly (`d`, `sh`, `j`), or None.
    pub fn from_text(s: &str) -> Option<Onset> {
        Onset::ALL
            .iter()
            .copied()
            .find(|o| *o != Onset::None && o.letter() == s)
    }

    /// Whether the consonant is voiced (the vocal cords buzz through it).
    pub fn voiced(self) -> bool {
        matches!(
            self,
            Onset::B
                | Onset::D
                | Onset::G
                | Onset::M
                | Onset::N
                | Onset::L
                | Onset::R
                | Onset::W
                | Onset::Y
                | Onset::V
                | Onset::Z
                | Onset::Zh
                | Onset::Jh
                | Onset::Dh
                | Onset::Ng
        )
    }

    /// Whether it is a sonorant (a nasal, liquid or glide): it keeps the voice going.
    pub fn sonorant(self) -> bool {
        matches!(
            self,
            Onset::M | Onset::N | Onset::L | Onset::R | Onset::W | Onset::Y | Onset::Ng
        )
    }

    /// How long a consonant on its own lasts (as a cluster member or a coda), ms.
    pub fn duration_ms(self) -> f64 {
        match self {
            Onset::None => 0.0,
            Onset::B | Onset::D | Onset::G | Onset::P | Onset::T | Onset::K => 55.0,
            Onset::M | Onset::N | Onset::Ng => 70.0,
            Onset::L | Onset::R | Onset::W | Onset::Y => 60.0,
            Onset::H => 50.0,
            Onset::Ch | Onset::Jh => 95.0,
            _ => 85.0,
        }
    }

    /// How the onset sounds (see [`OnsetShape`]).
    pub fn shape(self) -> OnsetShape {
        let o = OnsetShape::default();
        let locus = |f1: f64, f2: f64, f3: f64| Some([f1, f2, f3]);
        match self {
            Onset::None => o,
            // voiced stops: a short closure, a burst, formants out of the place's locus
            Onset::B => OnsetShape {
                locus: locus(250.0, 800.0, 2200.0),
                glide_ms: 40.0,
                gap_ms: 10.0,
                burst: 0.25,
                burst_hz: 600.0,
                ..o
            },
            Onset::D => OnsetShape {
                locus: locus(250.0, 1700.0, 2600.0),
                glide_ms: 45.0,
                gap_ms: 10.0,
                burst: 0.35,
                burst_hz: 3500.0,
                ..o
            },
            Onset::G => OnsetShape {
                locus: locus(250.0, 2000.0, 2300.0),
                glide_ms: 50.0,
                gap_ms: 12.0,
                burst: 0.35,
                burst_hz: 2000.0,
                ..o
            },
            // voiceless stops: the burst, then aspiration through the formants until voicing
            Onset::P => OnsetShape {
                locus: locus(250.0, 800.0, 2200.0),
                glide_ms: 40.0,
                gap_ms: 45.0,
                burst: 0.5,
                burst_hz: 800.0,
                aspirate: 0.5,
                ..o
            },
            Onset::T => OnsetShape {
                locus: locus(250.0, 1700.0, 2600.0),
                glide_ms: 45.0,
                gap_ms: 50.0,
                burst: 0.6,
                burst_hz: 4000.0,
                aspirate: 0.5,
                ..o
            },
            Onset::K => OnsetShape {
                locus: locus(250.0, 2000.0, 2300.0),
                glide_ms: 50.0,
                gap_ms: 55.0,
                burst: 0.5,
                burst_hz: 2200.0,
                aspirate: 0.5,
                ..o
            },
            // nasals: a quiet, dark murmur, then the release
            Onset::M => OnsetShape {
                locus: locus(250.0, 1100.0, 2300.0),
                glide_ms: 40.0,
                murmur_ms: 60.0,
                ..o
            },
            Onset::N => OnsetShape {
                locus: locus(250.0, 1700.0, 2600.0),
                glide_ms: 40.0,
                murmur_ms: 60.0,
                ..o
            },
            // liquids and glides: voiced all through, a slow transition
            Onset::L => OnsetShape {
                locus: locus(360.0, 1300.0, 2900.0),
                glide_ms: 60.0,
                murmur_ms: 25.0,
                ..o
            },
            Onset::R => OnsetShape {
                locus: locus(330.0, 1060.0, 1600.0),
                glide_ms: 80.0,
                ..o
            },
            Onset::W => OnsetShape {
                locus: locus(290.0, 610.0, 2150.0),
                glide_ms: 70.0,
                ..o
            },
            Onset::Y => OnsetShape {
                locus: locus(260.0, 2070.0, 3020.0),
                glide_ms: 70.0,
                ..o
            },
            // fricative and aspirate: noise first, voicing after
            Onset::S => OnsetShape {
                gap_ms: 90.0,
                fricative: 0.5,
                fric_hz: 5000.0,
                ..o
            },
            Onset::H => OnsetShape {
                gap_ms: 60.0,
                aspirate: 0.7,
                ..o
            },
            // fricatives: f and th are weak and broad, s and sh strong; the voiced ones buzz
            // quietly under the noise
            Onset::F => OnsetShape {
                gap_ms: 70.0,
                fricative: 0.30,
                fric_hz: 6500.0,
                ..o
            },
            Onset::V => OnsetShape {
                gap_ms: 60.0,
                fricative: 0.16,
                fric_hz: 5500.0,
                voice_bar: 0.55,
                murmur_ms: 60.0,
                ..o
            },
            Onset::Z => OnsetShape {
                gap_ms: 70.0,
                fricative: 0.32,
                fric_hz: 5200.0,
                voice_bar: 0.55,
                murmur_ms: 70.0,
                ..o
            },
            Onset::Sh => OnsetShape {
                gap_ms: 100.0,
                fricative: 0.55,
                fric_hz: 2600.0,
                ..o
            },
            Onset::Zh => OnsetShape {
                gap_ms: 75.0,
                fricative: 0.32,
                fric_hz: 2400.0,
                voice_bar: 0.55,
                murmur_ms: 75.0,
                ..o
            },
            Onset::Th => OnsetShape {
                gap_ms: 80.0,
                fricative: 0.16,
                fric_hz: 7000.0,
                ..o
            },
            Onset::Dh => OnsetShape {
                gap_ms: 35.0,
                fricative: 0.10,
                fric_hz: 6000.0,
                voice_bar: 0.6,
                murmur_ms: 35.0,
                glide_ms: 30.0,
                locus: locus(300.0, 1500.0, 2600.0),
                ..o
            },
            // affricates: the burst first, then the noise
            Onset::Ch => OnsetShape {
                gap_ms: 100.0,
                burst: 0.45,
                burst_hz: 3200.0,
                burst_first: true,
                fricative: 0.5,
                fric_hz: 2800.0,
                ..o
            },
            Onset::Jh => OnsetShape {
                gap_ms: 85.0,
                burst: 0.30,
                burst_hz: 3000.0,
                burst_first: true,
                fricative: 0.30,
                fric_hz: 2600.0,
                voice_bar: 0.5,
                murmur_ms: 85.0,
                ..o
            },
            // "ng": a nasal at the soft palate
            Onset::Ng => OnsetShape {
                locus: locus(250.0, 2000.0, 2300.0),
                glide_ms: 45.0,
                murmur_ms: 70.0,
                ..o
            },
        }
    }
}

/// What an onset does, in time from the note's start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnsetShape {
    /// F1-F3 the formants start from (None: the vowel's own).
    pub locus: Option<[f64; 3]>,
    /// How long the formants take to reach the vowel (the glide's time constant), ms.
    pub glide_ms: f64,
    /// Voicing starts this late, ms.
    pub gap_ms: f64,
    /// A noise burst at the release (stops), its level and centre.
    pub burst: f64,
    pub burst_hz: f64,
    /// Aspiration (noise through the formants) until voicing.
    pub aspirate: f64,
    /// A nasal murmur: the voice quiet and dark for this long, ms.
    pub murmur_ms: f64,
    /// A sibilant: high-passed noise until voicing.
    pub fricative: f64,
    pub fric_hz: f64,
    /// The level of the voice's buzz before voicing proper (voiced fricatives and affricates).
    pub voice_bar: f64,
    /// The burst comes at the start, not at the release (an affricate: burst, then noise).
    pub burst_first: bool,
}

impl Default for OnsetShape {
    fn default() -> Self {
        OnsetShape {
            locus: None,
            glide_ms: 0.0,
            gap_ms: 0.0,
            burst: 0.0,
            burst_hz: 1000.0,
            aspirate: 0.0,
            murmur_ms: 0.0,
            fricative: 0.0,
            fric_hz: 5000.0,
            voice_bar: 0.0,
            burst_first: false,
        }
    }
}

/// The vowel of a consonant on its own (a cluster member): no nucleus, noise only.
pub const NO_VOWEL: u8 = u8::MAX;

/// A syllable: an optional consonant onset into a vowel, which may glide to a second vowel (a
/// diphthong) and end in a consonant (the coda). Packed into a `u16` on a MIDI binding
/// ([`code`](Self::code), which carries the onset and the vowel only; 0 = none), written as text
/// in a document (`"vowel": "da"`).
///
/// The text is `[onset]vowel[+glide][.coda]`: `da`, `sha`, `ah+ih` (the "i" of "ride"),
/// `ra+ih.t` ("right"); a consonant alone is `.t` ([`Syllable::consonant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Syllable {
    pub onset: Onset,
    /// Index into [`VOWELS`] ([`NO_VOWEL`]: a consonant on its own).
    pub vowel: u8,
    /// The vowel the first glides to, after the note has settled (`ay`, `ow`, `oy`).
    pub glide: Option<u8>,
    /// A consonant the note ends on, when it is let go.
    pub coda: Onset,
}

impl Syllable {
    pub fn vowel(v: u8) -> Syllable {
        Syllable {
            onset: Onset::None,
            vowel: v.min(VOWELS.len() as u8 - 1),
            glide: None,
            coda: Onset::None,
        }
    }

    /// `onset` + `vowel`, nothing else.
    pub fn new(onset: Onset, vowel: u8) -> Syllable {
        Syllable {
            onset,
            ..Syllable::vowel(vowel)
        }
    }

    /// A consonant on its own: noise, a burst, a hum, no vowel (the `str` of "street").
    pub fn consonant(o: Onset) -> Syllable {
        Syllable {
            onset: o,
            vowel: NO_VOWEL,
            glide: None,
            coda: Onset::None,
        }
    }

    /// The packed form: `onset << 8 | (vowel + 1)`, never 0. The glide and the coda are not in
    /// it.
    pub fn code(self) -> u16 {
        ((self.onset as u16) << 8) | (self.vowel as u16 + 1).min(0xff)
    }

    /// The packed form back (0, or a code out of range: None).
    pub fn from_code(c: u16) -> Option<Syllable> {
        let v = (c & 0xff) as usize;
        let o = (c >> 8) as usize;
        if v == 0 || v > VOWELS.len() || o >= Onset::ALL.len() {
            return None;
        }
        Some(Syllable::new(Onset::ALL[o], (v - 1) as u8))
    }

    /// `a`, `oo`, `da`, `roo`, `kah`, `sha`, `ah+ih.t` (case and surrounding space ignored).
    /// The vowel spellings are [`parse_vowel`]'s, whole numbers only.
    pub fn parse(s: &str) -> Option<Syllable> {
        let s = s.trim().to_ascii_lowercase();
        let (body, coda) = match s.split_once('.') {
            Some((b, c)) => (b, Some(c)),
            None => (s.as_str(), None),
        };
        let coda = match coda {
            None => Onset::None,
            Some(c) => Onset::from_text(c).filter(|_| !c.is_empty())?,
        };
        if body.is_empty() {
            // `.t`: a consonant alone
            return (coda != Onset::None).then(|| Syllable::consonant(coda));
        }
        let (head, glide) = match body.split_once('+') {
            Some((h, g)) => (h, Some(g)),
            None => (body, None),
        };
        let glide = match glide {
            None => None,
            Some(g) => Some(Self::whole_vowel(g)?),
        };
        let mut syl = if let Some(v) = Self::whole_vowel(head) {
            Syllable::vowel(v)
        } else {
            // the longest onset that leaves a vowel
            let mut found = None;
            for n in (1..=2.min(head.len() - 1)).rev() {
                if !head.is_char_boundary(n) {
                    continue;
                }
                let (o, rest) = head.split_at(n);
                if let (Some(o), Some(v)) = (Onset::from_text(o), Self::whole_vowel(rest)) {
                    found = Some(Syllable::new(o, v));
                    break;
                }
            }
            found?
        };
        syl.glide = glide;
        syl.coda = coda;
        Some(syl)
    }

    fn whole_vowel(s: &str) -> Option<u8> {
        if s.is_empty() || s.chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        parse_vowel(s)
            .filter(|v| libm::fmod(*v, 1.0) == 0.0)
            .map(|v| v as u8)
    }

    /// The canonical text: the onset's letter, then the vowel's name, then `+glide` and
    /// `.coda` when it has them.
    pub fn text(self) -> String {
        if self.vowel == NO_VOWEL {
            return alloc::format!(".{}", self.onset.letter());
        }
        let mut s = String::from(self.onset.letter());
        if self.vowel != NO_VOWEL {
            s.push_str(VOWEL_NAMES[self.vowel as usize]);
        }
        if let Some(g) = self.glide {
            s.push('+');
            s.push_str(VOWEL_NAMES[g as usize]);
        }
        if self.coda != Onset::None {
            s.push('.');
            s.push_str(self.coda.letter());
        }
        s
    }
}

// MARK: params and presets

/// The voice's excitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum Source {
    /// A PolyBLEP saw: buzzy, the classic robot.
    Saw,
    /// A PolyBLEP pulse ([`VoiceParams::pulse_width`]): hollow, nasal.
    Pulse,
    /// The derivative of a Rosenberg glottal pulse: the most voice-like.
    Glottal,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Saw, Source::Pulse, Source::Glottal];
    pub fn name(self) -> &'static str {
        match self {
            Source::Saw => "saw",
            Source::Pulse => "pulse",
            Source::Glottal => "glottal",
        }
    }
    pub fn parse(s: &str) -> Option<Source> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "saw" | "sawtooth" | "buzz" => Source::Saw,
            "pulse" | "square" | "pwm" => Source::Pulse,
            "glottal" | "glottis" | "voice" | "rosenberg" => Source::Glottal,
            _ => return None,
        })
    }
}

/// How the resonators are wired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum Topology {
    /// In series: the levels come from the filter (a smooth, natural vowel).
    Cascade,
    /// Side by side, each with its own level and alternating sign (brighter, more "talk box").
    Parallel,
}

impl Topology {
    pub fn name(self) -> &'static str {
        match self {
            Topology::Cascade => "cascade",
            Topology::Parallel => "parallel",
        }
    }
    pub fn parse(s: &str) -> Option<Topology> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "cascade" | "serial" => Topology::Cascade,
            "parallel" => Topology::Parallel,
            _ => return None,
        })
    }
}

/// A voice's settings: what a preset sets and a lane carries.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct VoiceParams {
    pub source: Source,
    /// The pulse's duty cycle, 0.05..0.5.
    pub pulse_width: f64,
    pub topology: Topology,
    /// The vowel a note without its own sings (a position, 0 a .. 6 oo).
    pub vowel: f64,
    /// How long a vowel takes to morph into the next (the glide's time constant), ms.
    pub morph_ms: f64,
    /// Formant shift, semitones: + smaller / brighter (a female or child voice), - bigger.
    pub formant_shift: f64,
    /// Bandwidth multiplier (wider: breathier, less ringing).
    pub bandwidth: f64,
    /// Vibrato: rate Hz, depth cents, and the delay before it is fully in, ms.
    pub vibrato_rate: f64,
    pub vibrato_depth: f64,
    pub vibrato_delay: f64,
    /// Unison copies (the choir), 1..=[`MAX_UNISON`].
    pub unison: usize,
    /// The copies' detune at the edges, cents (they spread evenly between ±spread).
    pub spread: f64,
    /// The two resonator banks' formant offset (0.03: ±3 %).
    pub formant_spread: f64,
    /// Slow random pitch wander per copy, cents (0: a machine).
    pub drift: f64,
    /// Aspiration noise through the formants all the time, 0..1.
    pub breath: f64,
    /// Output level.
    pub level: f64,
}

impl Default for VoiceParams {
    fn default() -> Self {
        VoiceParams {
            source: Source::Glottal,
            pulse_width: 0.3,
            topology: Topology::Cascade,
            vowel: 5.0,
            morph_ms: 60.0,
            formant_shift: 0.0,
            bandwidth: 1.0,
            vibrato_rate: 5.2,
            vibrato_depth: 20.0,
            vibrato_delay: 250.0,
            unison: 1,
            spread: 0.0,
            formant_spread: 0.0,
            drift: 0.0,
            breath: 0.0,
            level: 1.0,
        }
    }
}

/// A named voice.
#[derive(Clone, Copy, Debug)]
pub struct VoicePreset {
    pub name: &'static str,
    pub summary: &'static str,
    pub params: VoiceParams,
}

#[allow(clippy::too_many_arguments)]
const fn p(
    source: Source,
    topology: Topology,
    vowel: f64,
    morph_ms: f64,
    formant_shift: f64,
    vibrato: (f64, f64, f64),
    unison: usize,
    spread: f64,
    drift: f64,
    breath: f64,
) -> VoiceParams {
    VoiceParams {
        source,
        pulse_width: 0.3,
        topology,
        vowel,
        morph_ms,
        formant_shift,
        bandwidth: 1.0,
        vibrato_rate: vibrato.0,
        vibrato_depth: vibrato.1,
        vibrato_delay: vibrato.2,
        unison,
        spread,
        formant_spread: if unison > 1 { 0.03 } else { 0.0 },
        drift,
        breath,
        level: 1.0,
    }
}

/// The factory voices. The instrument names the wave engine knows them by are `voice-<name>`
/// ([`instrument_preset`]).
pub const PRESETS: [VoicePreset; 8] = [
    VoicePreset {
        name: "robot",
        summary: "one buzzing saw voice, hard vowel changes, no vibrato: the vocoder robot",
        params: p(
            Source::Saw,
            Topology::Cascade,
            0.0,
            12.0,
            0.0,
            (0.0, 0.0, 0.0),
            1,
            0.0,
            0.0,
            0.0,
        ),
    },
    VoicePreset {
        name: "robot-choir",
        summary: "four saw robots a few cents apart, quick morphs: a Daft-Punk-ish machine choir",
        params: p(
            Source::Saw,
            Topology::Cascade,
            5.0,
            25.0,
            -1.0,
            (5.8, 6.0, 0.0),
            4,
            9.0,
            0.0,
            0.0,
        ),
    },
    VoicePreset {
        name: "android",
        summary: "a hollow pulse voice in parallel formants, a fast shallow vibrato",
        params: p(
            Source::Pulse,
            Topology::Parallel,
            3.0,
            30.0,
            1.0,
            (7.0, 8.0, 0.0),
            2,
            5.0,
            0.0,
            0.0,
        ),
    },
    VoicePreset {
        name: "choir-aah",
        summary: "six glottal voices singing aah, a slow vibrato that comes in late",
        params: p(
            Source::Glottal,
            Topology::Cascade,
            5.0,
            90.0,
            0.0,
            (5.0, 22.0, 300.0),
            6,
            14.0,
            6.0,
            0.08,
        ),
    },
    VoicePreset {
        name: "choir-ooh",
        summary: "six glottal voices singing ooh, darker and rounder",
        params: p(
            Source::Glottal,
            Topology::Cascade,
            6.0,
            110.0,
            -0.5,
            (4.8, 20.0, 300.0),
            6,
            12.0,
            6.0,
            0.06,
        ),
    },
    VoicePreset {
        name: "soprano",
        summary: "a bright choir: formants up four semitones",
        params: p(
            Source::Glottal,
            Topology::Cascade,
            5.0,
            80.0,
            4.0,
            (5.5, 28.0, 250.0),
            4,
            10.0,
            5.0,
            0.1,
        ),
    },
    VoicePreset {
        name: "bass-choir",
        summary: "a deep choir: formants down three semitones",
        params: p(
            Source::Glottal,
            Topology::Cascade,
            6.0,
            120.0,
            -3.0,
            (4.5, 15.0, 350.0),
            5,
            10.0,
            5.0,
            0.05,
        ),
    },
    VoicePreset {
        name: "talkbox",
        summary: "one saw through sharp parallel formants: a talk box on a lead",
        params: p(
            Source::Saw,
            Topology::Parallel,
            0.0,
            40.0,
            0.0,
            (5.5, 10.0, 200.0),
            1,
            0.0,
            0.0,
            0.0,
        ),
    },
];

/// A preset by name (`robot`, `choir-aah`, …; `voice-` prefix allowed).
pub fn preset(name: &str) -> Option<&'static VoicePreset> {
    let n = name.trim();
    let n = n.strip_prefix("voice-").unwrap_or(n);
    PRESETS.iter().find(|p| p.name == n)
}

/// The wave engine's instruments that are formant voices: `voice-<preset>`.
pub fn instrument_preset(instrument: &str) -> Option<&'static VoicePreset> {
    instrument.strip_prefix("voice-").and_then(preset)
}

/// The instrument names of every preset (`voice-robot`, …).
pub fn instrument_names() -> [&'static str; PRESETS.len()] {
    [
        "voice-robot",
        "voice-robot-choir",
        "voice-android",
        "voice-choir-aah",
        "voice-choir-ooh",
        "voice-soprano",
        "voice-bass-choir",
        "voice-talkbox",
    ]
}

// MARK: DSP pieces

/// Klatt's resonator: `y = a x + b y1 + c y2`, unity gain at DC.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
    /// Parallel: 1 / the gain at the centre frequency (so a level of 1 peaks at 1).
    norm: f64,
}

impl Resonator {
    /// New coefficients for centre `f` and bandwidth `bw` (Hz); the state is kept.
    pub fn tune(&mut self, f: f64, bw: f64, sr: f64) {
        let f = f.clamp(20.0, 0.45 * sr);
        let t = 1.0 / sr;
        let c = -libm::exp(-2.0 * PI * bw * t);
        let b = 2.0 * libm::exp(-PI * bw * t) * libm::cos(2.0 * PI * f * t);
        self.a = 1.0 - b - c;
        self.b = b;
        self.c = c;
        let g = self.gain_at(f, sr);
        self.norm = if g > 1e-9 { 1.0 / g } else { 1.0 };
    }

    /// |H| at `f` Hz.
    pub fn gain_at(&self, f: f64, sr: f64) -> f64 {
        let w = 2.0 * PI * f / sr;
        // 1 - b e^{-jw} - c e^{-2jw}
        let re = 1.0 - self.b * libm::cos(w) - self.c * libm::cos(2.0 * w);
        let im = self.b * libm::sin(w) + self.c * libm::sin(2.0 * w);
        libm::fabs(self.a) / libm::sqrt(re * re + im * im).max(1e-12)
    }

    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    pub fn reset(&mut self) {
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// An RBJ biquad for the noise colour (burst band-pass, sibilance high-pass).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
struct NoiseFilter {
    c: [f64; 5],
    z1: f64,
    z2: f64,
}

impl NoiseFilter {
    fn set(&mut self, high: bool, fc: f64, q: f64, sr: f64) {
        let w = 2.0 * PI * fc.clamp(20.0, 0.45 * sr) / sr;
        let (cw, sw) = (libm::cos(w), libm::sin(w));
        let alpha = sw / (2.0 * q);
        let (b0, b1, b2) = if high {
            ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0)
        } else {
            (alpha, 0.0, -alpha)
        };
        let a0 = 1.0 + alpha;
        self.c = [
            b0 / a0,
            b1 / a0,
            b2 / a0,
            -2.0 * cw / a0,
            (1.0 - alpha) / a0,
        ];
    }
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let c = self.c;
        let y = c[0] * x + self.z1;
        self.z1 = c[1] * x - c[3] * y + self.z2;
        self.z2 = c[2] * x - c[4] * y;
        y
    }
}

#[inline]
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// The opening and closing fractions of the glottal cycle (Rosenberg's pulse C).
const GLOTTAL_OPEN: f64 = 0.40;
const GLOTTAL_CLOSE: f64 = 0.16;

/// One sample of the source at phase `p` (0..1) moving `dt` a sample.
#[inline]
pub fn source_sample(src: Source, p: f64, dt: f64, pw: f64) -> f64 {
    match src {
        Source::Saw => 2.0 * p - 1.0 - poly_blep(p, dt),
        Source::Pulse => {
            let mut v = if p < pw { 1.0 } else { -1.0 };
            v += poly_blep(p, dt);
            let q = p - pw;
            v -= poly_blep(if q < 0.0 { q + 1.0 } else { q }, dt);
            // the pulse's mean is 2 pw - 1: take it off
            v - (2.0 * pw - 1.0)
        }
        Source::Glottal => {
            let (tp, tn) = (GLOTTAL_OPEN, GLOTTAL_CLOSE);
            let tc = tp + tn;
            let naive = if p < tp {
                (PI / (2.0 * tp)) * libm::sin(PI * p / tp)
            } else if p < tc {
                -(PI / (2.0 * tn)) * libm::sin(PI * (p - tp) / (2.0 * tn))
            } else {
                0.0
            };
            // the closure is a step up of π / 2tn: band-limit it
            let h = PI / (2.0 * tn);
            let mut x = p - tc;
            if x < 0.0 {
                x += 1.0;
            }
            (naive + 0.5 * h * poly_blep(x, dt)) * 0.1
        }
    }
}

#[inline]
fn xorshift(x: &mut u32) -> f64 {
    let mut v = *x;
    v ^= v << 13;
    v ^= v >> 17;
    v ^= v << 5;
    *x = v;
    (v as f64 / u32::MAX as f64) * 2.0 - 1.0
}

/// The cents → ratio of a pitch offset.
#[inline]
fn cents(c: f64) -> f64 {
    libm::exp2(c / 1200.0)
}

// MARK: the voice

/// One sung note (a pooled voice): its source copies, two resonator banks and its onset.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct FormantVoice {
    hz: f64,
    phase: [f64; MAX_UNISON],
    vib_phase: [f64; MAX_UNISON],
    drift: [f64; MAX_UNISON],
    drift_target: [f64; MAX_UNISON],
    rng: u32,
    /// Samples since the note started.
    t: u32,
    ctl: u32,
    /// Formants in log2 Hz: where they are, where they glide to; bandwidths.
    cur: [f64; N_FORMANTS],
    tgt: [f64; N_FORMANTS],
    bw: [f64; N_FORMANTS],
    amp: [f64; N_FORMANTS],
    glide_k: f64,
    /// The vowel the note sings (a position).
    vowel: f64,
    banks: [[Resonator; N_FORMANTS]; 2],
    onset: Onset,
    /// The consonant the note ends on, and the samples since it began (None: not yet).
    coda: Onset,
    coda_t: Option<u32>,
    /// A second vowel the note glides to once it has settled.
    glide_to: Option<f64>,
    /// False for a consonant on its own: noise and a hum, no voiced vowel.
    nucleus: bool,
    burst: NoiseFilter,
    fric: NoiseFilter,
    voicing: f64,
    murmur_lp: f64,
    /// 1 / the cascade's gain at F1 (for an even level across vowels).
    norm: f64,
    sounding: bool,
}

impl FormantVoice {
    pub fn new(seed: u32) -> FormantVoice {
        let s = if seed == 0 { 0x9E37_79B9 } else { seed };
        let mut v = FormantVoice {
            hz: 220.0,
            phase: [0.0; MAX_UNISON],
            vib_phase: [0.0; MAX_UNISON],
            drift: [0.0; MAX_UNISON],
            drift_target: [0.0; MAX_UNISON],
            rng: s,
            t: 0,
            ctl: 0,
            cur: [0.0; N_FORMANTS],
            tgt: [0.0; N_FORMANTS],
            bw: [0.0; N_FORMANTS],
            amp: [0.0; N_FORMANTS],
            glide_k: 1.0,
            vowel: 0.0,
            banks: [[Resonator::default(); N_FORMANTS]; 2],
            onset: Onset::None,
            coda: Onset::None,
            coda_t: None,
            glide_to: None,
            nucleus: true,
            burst: NoiseFilter::default(),
            fric: NoiseFilter::default(),
            voicing: 1.0,
            murmur_lp: 0.0,
            norm: 1.0,
            sounding: false,
        };
        // copies start apart, so a chord's voices don't phase in lockstep
        for k in 0..MAX_UNISON {
            v.phase[k] = (xorshift(&mut v.rng) + 1.0) * 0.5;
            v.vib_phase[k] = k as f64 / MAX_UNISON as f64;
        }
        v
    }

    /// The vowel position the note sings.
    pub fn vowel(&self) -> f64 {
        self.vowel
    }

    /// The formant frequencies now (Hz, the first bank's centre).
    pub fn formants(&self) -> [f64; N_FORMANTS] {
        self.cur.map(libm::exp2)
    }

    fn set_target(&mut self, p: &VoiceParams, v: f64) {
        self.vowel = v;
        let s = vowel_at(v);
        let shift = cents(p.formant_shift * 100.0);
        for k in 0..N_FORMANTS {
            self.tgt[k] = libm::log2(s.f[k] * shift);
            self.bw[k] = s.bw[k] * shift * p.bandwidth.max(0.1);
            self.amp[k] = s.amp[k];
        }
    }

    /// Start a note at `hz` singing `syllable` (None: the params' vowel, or `vowel` when given).
    /// A voice that was silent snaps its formants to the start (an onset's locus or the vowel);
    /// one still sounding glides from where it is (legato: the vowel morphs).
    pub fn start(&mut self, p: &VoiceParams, hz: f64, syllable: Option<Syllable>, sr: f64) {
        self.start_at(p, hz, syllable, None, sr)
    }

    /// [`start`](Self::start) with the vowel position a note without a syllable sings.
    pub fn start_at(
        &mut self,
        p: &VoiceParams,
        hz: f64,
        syllable: Option<Syllable>,
        vowel: Option<f64>,
        sr: f64,
    ) {
        self.hz = hz;
        self.t = 0;
        self.ctl = 0;
        let v = match syllable {
            Some(s) if s.vowel != NO_VOWEL => s.vowel as f64,
            _ => vowel.unwrap_or(p.vowel),
        };
        self.set_target(p, v);
        self.onset = syllable.map_or(Onset::None, |s| s.onset);
        self.nucleus = syllable.is_none_or(|s| s.vowel != NO_VOWEL);
        self.coda = syllable.map_or(Onset::None, |s| s.coda);
        self.coda_t = None;
        self.glide_to = syllable.and_then(|s| s.glide).map(|g| g as f64);
        let os = self.onset.shape();
        let glide_ms = if self.onset != Onset::None && os.glide_ms > 0.0 {
            os.glide_ms
        } else {
            p.morph_ms
        };
        self.glide_k = glide_coef(glide_ms, sr);
        let shift = cents(p.formant_shift * 100.0);
        if let Some(l) = os.locus {
            for (c, f) in self.cur.iter_mut().zip(l) {
                *c = libm::log2(f * shift);
            }
            if !self.sounding {
                for k in 3..N_FORMANTS {
                    self.cur[k] = self.tgt[k];
                }
            }
        } else if !self.sounding {
            self.cur = self.tgt;
        }
        if os.burst > 0.0 {
            self.burst.set(false, os.burst_hz * shift, 1.2, sr);
        }
        if os.fricative > 0.0 {
            self.fric.set(true, os.fric_hz, 0.7, sr);
        }
        if !self.sounding {
            for bank in self.banks.iter_mut() {
                for r in bank.iter_mut() {
                    r.reset();
                }
            }
            self.murmur_lp = 0.0;
        }
        self.sounding = true;
        self.retune(p, sr);
    }

    /// A new vowel for the sounding note (a live knob): the formants glide there at `morph_ms`.
    pub fn set_vowel(&mut self, p: &VoiceParams, v: f64, sr: f64) {
        self.set_target(p, v);
        self.glide_k = glide_coef(p.morph_ms, sr);
    }

    /// The note has ended and its envelope is silent: the next note starts fresh.
    pub fn release(&mut self) {
        self.sounding = false;
    }

    /// The pitch, for a glide (the formants stay).
    pub fn set_hz(&mut self, hz: f64) {
        self.hz = hz;
    }

    fn retune(&mut self, p: &VoiceParams, sr: f64) {
        let split = if p.unison > 1 { p.formant_spread } else { 0.0 };
        for (b, bank) in self.banks.iter_mut().enumerate() {
            let m = if b == 0 { 1.0 - split } else { 1.0 + split };
            for (k, r) in bank.iter_mut().enumerate() {
                r.tune(libm::exp2(self.cur[k]) * m, self.bw[k], sr);
            }
        }
        // the cascade peaks about Q1 at F1: level the vowels against each other
        let f1 = libm::exp2(self.cur[0]);
        self.norm = (self.bw[0] / f1).clamp(0.02, 1.0) * 2.0;
    }

    /// One sample, `rate` × the note's pitch (the lane's pitch bend / FM; 1 = as struck).
    pub fn sample(&mut self, p: &VoiceParams, rate: f64, sr: f64) -> f64 {
        let ms = 1000.0 / sr;
        let t_ms = self.t as f64 * ms;
        if self.ctl == 0 {
            // a diphthong: once the note has settled, head for its second vowel
            if let Some(g) = self.glide_to {
                if t_ms >= GLIDE_AT_MS && self.coda_t.is_none() {
                    self.glide_to = None;
                    self.set_target(p, g);
                    self.glide_k = glide_coef(GLIDE_MS, sr);
                }
            }
            // control rate: glide the formants, wander the copies
            let k = self.glide_k;
            let mut moving = false;
            for i in 0..N_FORMANTS {
                let d = self.tgt[i] - self.cur[i];
                if libm::fabs(d) > 1e-5 {
                    self.cur[i] += d * k;
                    moving = true;
                } else {
                    self.cur[i] = self.tgt[i];
                }
            }
            if moving || self.t == 0 {
                self.retune(p, sr);
            }
            if p.drift > 0.0 {
                let n = p.unison.clamp(1, MAX_UNISON);
                for j in 0..n {
                    if (self.t / CONTROL_PERIOD) % 64 == j as u32 * 8 {
                        self.drift_target[j] = xorshift(&mut self.rng) * p.drift;
                    }
                    self.drift[j] += (self.drift_target[j] - self.drift[j]) * 0.02;
                }
            }
            self.ctl = CONTROL_PERIOD;
        }
        self.ctl -= 1;

        // the onset: voicing, murmur, noise
        let os = self.onset.shape();
        let ramp = 5.0;
        self.voicing = if !self.nucleus {
            os.voice_bar
        } else if t_ms < os.gap_ms {
            os.voice_bar
        } else {
            os.voice_bar.max(((t_ms - os.gap_ms) / ramp).min(1.0))
        };
        let mut murmur = if os.murmur_ms > 0.0 && t_ms < os.murmur_ms + 20.0 {
            (1.0 - ((t_ms - os.murmur_ms) / 20.0).clamp(0.0, 1.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mut aspiration = p.breath * 0.15;
        if os.aspirate > 0.0 && t_ms < os.gap_ms + ramp {
            aspiration += os.aspirate * (1.0 - self.voicing);
        }
        // the coda: the voice closes on its last consonant
        let cs = self.coda.shape();
        let mut c_ms = -1.0;
        if let Some(ct) = self.coda_t {
            c_ms = ct as f64 * ms;
            let hold = if self.coda.sonorant() {
                1.0
            } else if self.coda.voiced() {
                0.45
            } else {
                0.0
            };
            let f = (c_ms / 15.0).min(1.0);
            self.voicing *= 1.0 + (hold - 1.0) * f;
            let dark = if matches!(self.coda, Onset::M | Onset::N | Onset::Ng) {
                (c_ms / 20.0).min(1.0)
            } else if self.coda.voiced() && !self.coda.sonorant() {
                (c_ms / 20.0).min(1.0) * 0.6
            } else {
                0.0
            };
            murmur = murmur.max(dark);
            // a voiceless stop's aspiration after its burst
            if cs.aspirate > 0.0 && c_ms > cs.gap_ms * 0.7 && c_ms < cs.gap_ms * 0.7 + 35.0 {
                aspiration += cs.aspirate * 0.6 * (1.0 - (c_ms - cs.gap_ms * 0.7) / 35.0);
            }
        }

        // the source: the copies, detuned, each with its vibrato and drift
        let n = p.unison.clamp(1, MAX_UNISON);
        let vib_on = if p.vibrato_delay > 0.0 {
            (t_ms / p.vibrato_delay).min(1.0)
        } else {
            1.0
        };
        let pw = p.pulse_width.clamp(0.05, 0.5);
        let mut src = [0.0f64; 2];
        for j in 0..n {
            let det = if n > 1 {
                p.spread * (2.0 * j as f64 / (n - 1) as f64 - 1.0)
            } else {
                0.0
            };
            let mut c = det + self.drift[j];
            if p.vibrato_depth > 0.0 && p.vibrato_rate > 0.0 {
                let r =
                    p.vibrato_rate * (1.0 + 0.06 * (j as f64 - (n - 1) as f64 * 0.5) / n as f64);
                let mut vp = self.vib_phase[j] + r / sr;
                vp -= libm::floor(vp);
                self.vib_phase[j] = vp;
                c += p.vibrato_depth * vib_on * libm::sin(2.0 * PI * vp);
            }
            let dt = (self.hz * rate * cents(c) / sr).clamp(0.0, 0.45);
            let mut ph = self.phase[j] + dt;
            ph -= libm::floor(ph);
            self.phase[j] = ph;
            src[j & 1] += source_sample(p.source, ph, dt.max(1e-9), pw);
        }
        let uni = 1.0 / libm::sqrt(n as f64);
        let noise = xorshift(&mut self.rng);
        let asp = noise * aspiration * 0.5;
        let voiced = self.voicing * (1.0 - 0.65 * murmur);
        let x0 = src[0] * uni * voiced + asp;
        let x1 = src[1] * uni * voiced + asp;

        // the tract
        let mut y = 0.0;
        let banks = if n > 1 { 2 } else { 1 };
        for (b, x) in [x0, x1].into_iter().enumerate().take(banks) {
            let bank = &mut self.banks[b];
            match p.topology {
                Topology::Cascade => {
                    let mut s = x;
                    for r in bank.iter_mut() {
                        s = r.tick(s);
                    }
                    y += s * self.norm;
                }
                Topology::Parallel => {
                    let mut s = 0.0;
                    for (k, r) in bank.iter_mut().enumerate() {
                        let g = self.amp[k] * r.norm;
                        let sign = if k & 1 == 0 { 1.0 } else { -1.0 };
                        s += sign * g * r.tick(x);
                    }
                    y += s * 0.5;
                }
            }
        }
        // a nasal murmur: darker while it lasts
        if murmur > 0.0 {
            self.murmur_lp += (y - self.murmur_lp) * (1.0 - libm::exp(-2.0 * PI * 400.0 / sr));
            y = y * (1.0 - murmur) + self.murmur_lp * murmur;
        }
        // the burst and the sibilant, outside the tract
        if os.burst > 0.0 {
            let at = if os.aspirate > 0.0 || os.burst_first {
                0.0
            } else {
                os.gap_ms
            };
            if t_ms >= at && t_ms < at + 12.0 {
                let e = 1.0 - (t_ms - at) / 12.0;
                y += self.burst.tick(noise) * os.burst * e;
            }
        }
        if os.fricative > 0.0 && t_ms < os.gap_ms + 15.0 {
            let e = (t_ms / 10.0).min(1.0) * (1.0 - ((t_ms - os.gap_ms) / 15.0).clamp(0.0, 1.0));
            y += self.fric.tick(noise) * os.fricative * e;
        }
        // the coda's noise: a stop's burst after its closure, a fricative's hiss
        if c_ms >= 0.0 {
            let release = if cs.aspirate > 0.0 { cs.gap_ms * 0.7 } else { 25.0 };
            if cs.burst > 0.0 {
                let at = if cs.burst_first { 0.0 } else { release };
                if c_ms >= at && c_ms < at + 12.0 {
                    y += self.burst.tick(noise) * cs.burst * (1.0 - (c_ms - at) / 12.0);
                }
            }
            if cs.fricative > 0.0 {
                let e = ((c_ms - 8.0) / 12.0).clamp(0.0, 1.0)
                    * (1.0 - ((c_ms - self.coda.duration_ms() + 20.0) / 20.0).clamp(0.0, 1.0));
                y += self.fric.tick(noise) * cs.fricative * e;
            }
        }
        self.t = self.t.saturating_add(1);
        if self.coda_t.is_some() {
            self.coda_t = self.coda_t.map(|c| c.saturating_add(1));
        }
        y * p.level
    }

    /// The note is being let go: close on its coda, if it has one. Returns how long the
    /// consonant takes, ms (0: no coda, release at once).
    pub fn begin_coda(&mut self, p: &VoiceParams, sr: f64) -> f64 {
        if self.coda == Onset::None || self.coda_t.is_some() || !self.sounding {
            return 0.0;
        }
        let cs = self.coda.shape();
        let shift = cents(p.formant_shift * 100.0);
        self.coda_t = Some(0);
        self.glide_to = None;
        if let Some(l) = cs.locus {
            for k in 0..3 {
                self.tgt[k] = libm::log2(l[k] * shift);
            }
            self.glide_k = glide_coef(cs.glide_ms.max(25.0), sr);
        }
        if cs.burst > 0.0 {
            self.burst.set(false, cs.burst_hz * shift, 1.2, sr);
        }
        if cs.fricative > 0.0 {
            self.fric.set(true, cs.fric_hz, 0.7, sr);
        }
        self.coda.duration_ms()
    }
}

/// The per-control-period step of a one-pole glide with time constant `ms`.
fn glide_coef(ms: f64, sr: f64) -> f64 {
    if ms <= 0.0 {
        return 1.0;
    }
    1.0 - libm::exp(-(CONTROL_PERIOD as f64) / (ms * 0.001 * sr))
}

/// A cascade's response |H(f)| in dB for vowel position `v` (no shift), for a view and the tests.
pub fn cascade_response_db(v: f64, shift_st: f64, f: f64, sr: f64) -> f64 {
    let s = vowel_at(v);
    let m = cents(shift_st * 100.0);
    let mut g = 1.0;
    for k in 0..N_FORMANTS {
        let mut r = Resonator::default();
        r.tune(s.f[k] * m, s.bw[k] * m, sr);
        g *= r.gain_at(f, sr);
    }
    20.0 * libm::log10(g.max(1e-12))
}

// MARK: a polyphonic voice for the vocoder (and offline renders)

/// The most notes [`FormantSynth`] holds.
pub const SYNTH_VOICES: usize = 8;

/// Up to [`SYNTH_VOICES`] notes of one voice preset, gated on and off: the vocoder's voice
/// carrier, and `djctl voice`'s offline render. Real-time safe.
#[derive(Clone, Debug)]
pub struct FormantSynth {
    sr: f64,
    pub params: VoiceParams,
    voices: [FormantVoice; SYNTH_VOICES],
    notes: [Option<u8>; SYNTH_VOICES],
    held: [bool; SYNTH_VOICES],
    /// Samples a note keeps its gate open for its coda, after its note off.
    coda_left: [u32; SYNTH_VOICES],
    gate: [f64; SYNTH_VOICES],
    /// The vowel position notes sing (a live knob; a note's syllable wins at its start).
    vowel: f64,
    attack_step: f64,
    release_step: f64,
    level: f32,
}

impl FormantSynth {
    pub fn new(sample_rate: f64, params: VoiceParams) -> FormantSynth {
        let mut s = FormantSynth {
            sr: sample_rate,
            params,
            voices: core::array::from_fn(|k| {
                FormantVoice::new(0x51ED_270B ^ (k as u32 + 1).wrapping_mul(0x85EB_CA6B))
            }),
            notes: [None; SYNTH_VOICES],
            held: [false; SYNTH_VOICES],
            coda_left: [0; SYNTH_VOICES],
            gate: [0.0; SYNTH_VOICES],
            vowel: params.vowel,
            attack_step: 0.0,
            release_step: 0.0,
            level: 1.0,
        };
        s.set_gate_times(40.0, 250.0);
        s
    }

    /// Attack and release, ms.
    pub fn set_gate_times(&mut self, attack_ms: f64, release_ms: f64) {
        self.attack_step = 1.0 / (attack_ms.max(1.0) * 0.001 * self.sr);
        self.release_step = 1.0 / (release_ms.max(1.0) * 0.001 * self.sr);
    }

    pub fn set_level(&mut self, v: f64) {
        self.level = v.clamp(0.0, 2.0) as f32;
    }

    /// A new preset's params (sounding notes carry on with them).
    pub fn set_params(&mut self, p: VoiceParams) {
        self.params = p;
        self.vowel = p.vowel;
    }

    /// The vowel every sounding note glides to and new notes sing.
    pub fn set_vowel(&mut self, v: f64) {
        let v = if v.is_finite() {
            v.clamp(0.0, VOWEL_MAX)
        } else {
            0.0
        };
        self.vowel = v;
        for k in 0..SYNTH_VOICES {
            if self.notes[k].is_some() {
                self.voices[k].set_vowel(&self.params, v, self.sr);
            }
        }
    }

    pub fn vowel(&self) -> f64 {
        self.vowel
    }

    pub fn note_on(&mut self, note: u8) {
        self.note_on_syllable(note, None)
    }

    /// A note singing `syllable` (None: the current vowel).
    pub fn note_on_syllable(&mut self, note: u8, syllable: Option<Syllable>) {
        let hz = 440.0 * libm::pow(2.0, (note as f64 - 69.0) / 12.0);
        // the same note again, or a free voice, or the quietest
        let k = self
            .notes
            .iter()
            .position(|n| *n == Some(note))
            .or_else(|| self.notes.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                let mut best = 0;
                for k in 1..SYNTH_VOICES {
                    if self.gate[k] < self.gate[best] {
                        best = k;
                    }
                }
                best
            });
        if self.notes[k].is_none() {
            self.voices[k].release();
        }
        self.notes[k] = Some(note);
        self.held[k] = true;
        self.coda_left[k] = 0;
        let p = self.params;
        self.voices[k].start_at(&p, hz, syllable, Some(self.vowel), self.sr);
    }

    /// Let `note` go. A note with a coda stays open for the consonant, then releases.
    pub fn note_off(&mut self, note: u8) {
        let p = self.params;
        for k in 0..SYNTH_VOICES {
            if self.notes[k] == Some(note) && self.held[k] && self.coda_left[k] == 0 {
                let ms = self.voices[k].begin_coda(&p, self.sr);
                if ms > 0.0 {
                    self.coda_left[k] = (ms * 0.001 * self.sr) as u32 + 1;
                } else {
                    self.held[k] = false;
                }
            }
        }
    }

    pub fn all_off(&mut self) {
        self.held = [false; SYNTH_VOICES];
        self.coda_left = [0; SYNTH_VOICES];
    }

    /// The notes sounding (held or releasing).
    pub fn sounding(&self) -> usize {
        self.notes.iter().filter(|n| n.is_some()).count()
    }

    /// Add the next `out.len()` frames into `out` (mono).
    pub fn render(&mut self, out: &mut [f32]) {
        let p = self.params;
        let scale = self.level * 0.6 / libm::sqrtf(self.sounding().max(1) as f32);
        for k in 0..SYNTH_VOICES {
            if self.notes[k].is_none() {
                continue;
            }
            for o in out.iter_mut() {
                if self.coda_left[k] > 0 {
                    self.coda_left[k] -= 1;
                    if self.coda_left[k] == 0 {
                        self.held[k] = false;
                    }
                }
                let target = if self.held[k] { 1.0 } else { 0.0 };
                let g = &mut self.gate[k];
                if *g < target {
                    *g = (*g + self.attack_step).min(1.0);
                } else if *g > target {
                    *g = (*g - self.release_step).max(0.0);
                }
                let y = self.voices[k].sample(&p, 1.0, self.sr) * self.gate[k];
                *o += y as f32 * scale;
            }
            if !self.held[k] && self.gate[k] <= 0.0 {
                self.notes[k] = None;
                self.voices[k].release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    const SR: f64 = 48_000.0;

    /// The local maxima of the cascade's response between 150 Hz and 4 kHz.
    fn peaks(v: f64, shift: f64) -> Vec<f64> {
        let mut out = Vec::new();
        let mut prev = (0.0, f64::NEG_INFINITY);
        let mut rising = false;
        let mut f = 150.0;
        while f < 4000.0 {
            let g = cascade_response_db(v, shift, f, SR);
            if g > prev.1 {
                rising = true;
            } else if rising {
                out.push(prev.0);
                rising = false;
            }
            prev = (f, g);
            f += 2.0;
        }
        out
    }

    #[test]
    fn every_vowel_peaks_at_its_formants() {
        for (i, v) in VOWELS.iter().enumerate() {
            let ps = peaks(i as f64, 0.0);
            for k in 0..3 {
                let want = v.f[k];
                let near = ps
                    .iter()
                    .map(|p| libm::fabs(p - want) / want)
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    near < 0.06,
                    "vowel {} F{}: want {want} Hz, peaks {ps:?}",
                    VOWEL_NAMES[i],
                    k + 1
                );
            }
        }
    }

    #[test]
    fn the_formant_shift_moves_every_peak() {
        // +4 semitones: x 1.26
        let ps = peaks(0.0, 4.0);
        let want = 730.0 * cents(400.0);
        assert!(
            ps.iter().any(|p| libm::fabs(p - want) / want < 0.06),
            "{ps:?}"
        );
    }

    #[test]
    fn vowels_parse_and_morph() {
        assert_eq!(parse_vowel("a"), Some(0.0));
        assert_eq!(parse_vowel("OO"), Some(6.0));
        assert_eq!(parse_vowel("aah"), Some(5.0));
        assert_eq!(parse_vowel("2.5"), Some(2.5));
        assert_eq!(parse_vowel("x"), None);
        assert_eq!(parse_vowel("9"), None);
        let m = vowel_at(0.5);
        assert!(m.f[1] > VOWELS[0].f[1] && m.f[1] < VOWELS[1].f[1]);
        assert_eq!(vowel_name(5.2), "ah");
    }

    #[test]
    fn syllables_round_trip() {
        for s in [
            "a", "e", "i", "o", "u", "ah", "oo", "da", "roo", "kah", "mi", "so", "ha", "we",
        ] {
            let syl = Syllable::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(syl.text(), s);
            assert_eq!(Syllable::from_code(syl.code()), Some(syl));
            assert_ne!(syl.code(), 0);
        }
        assert_eq!(Syllable::parse("dee").unwrap().text(), "di");
        assert_eq!(Syllable::parse("xa"), None);
        assert_eq!(Syllable::parse("d"), None);
        assert_eq!(Syllable::parse("d2"), None);
        assert_eq!(Syllable::from_code(0), None);
    }

    fn render(p: &VoiceParams, hz: f64, syl: Option<Syllable>, n: usize) -> Vec<f32> {
        let mut v = FormantVoice::new(7);
        v.start(p, hz, syl, SR);
        (0..n).map(|_| v.sample(p, 1.0, SR) as f32).collect()
    }

    /// The amplitude of `f` in `x` (Goertzel), normalised.
    fn tone(x: &[f32], f: f64) -> f64 {
        let w = 2.0 * PI * f / SR;
        let c = 2.0 * libm::cos(w);
        let (mut s1, mut s2) = (0.0, 0.0);
        for (i, &s) in x.iter().enumerate() {
            // Hann window
            let h = 0.5 - 0.5 * libm::cos(2.0 * PI * i as f64 / x.len() as f64);
            let s0 = s as f64 * h + c * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        libm::sqrt(s1 * s1 + s2 * s2 - c * s1 * s2) / x.len() as f64
    }

    #[test]
    fn a_rendered_vowel_is_loudest_near_its_formants() {
        // a low glottal note: its harmonics sample the envelope every 60 Hz
        let f0 = 60.0;
        let mut p = VoiceParams {
            vibrato_depth: 0.0,
            ..VoiceParams::default()
        };
        for (i, v) in VOWELS.iter().enumerate() {
            p.vowel = i as f64;
            let x = render(&p, f0, None, 48_000);
            let x = &x[24_000..];
            // the spectrum's envelope, sampled at the harmonics: its local maxima
            let amps: Vec<f64> = (1..=60).map(|h| tone(x, h as f64 * f0)).collect();
            let maxima: Vec<f64> = (1..amps.len() - 1)
                .filter(|&i| amps[i] > amps[i - 1] && amps[i] >= amps[i + 1])
                .map(|i| (i + 1) as f64 * f0)
                .collect();
            for k in 0..3 {
                let want = v.f[k];
                assert!(
                    maxima
                        .iter()
                        .any(|m| libm::fabs(m - want) <= want * 0.08 + f0),
                    "vowel {} F{}: want {want} Hz, spectral peaks {maxima:?}",
                    VOWEL_NAMES[i],
                    k + 1
                );
            }
        }
    }

    #[test]
    fn every_source_sounds_and_stays_bounded() {
        for src in Source::ALL {
            for topo in [Topology::Cascade, Topology::Parallel] {
                let p = VoiceParams {
                    source: src,
                    topology: topo,
                    unison: 4,
                    spread: 10.0,
                    formant_spread: 0.03,
                    ..VoiceParams::default()
                };
                let x = render(&p, 220.0, Syllable::parse("da"), 24_000);
                let peak = x.iter().fold(0f32, |m, s| m.max(s.abs()));
                let rms = libm::sqrtf(x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32);
                assert!(x.iter().all(|s| s.is_finite()));
                assert!(rms > 0.01, "{src:?} {topo:?} rms {rms}");
                assert!(peak < 4.0, "{src:?} {topo:?} peak {peak}");
            }
        }
    }

    #[test]
    fn a_vowel_morphs_instead_of_jumping() {
        let p = VoiceParams {
            morph_ms: 100.0,
            ..VoiceParams::default()
        };
        let mut v = FormantVoice::new(1);
        v.start_at(&p, 200.0, None, Some(0.0), SR);
        for _ in 0..4800 {
            v.sample(&p, 1.0, SR);
        }
        // to i: F2 1090 -> 2290; 20 ms in it is on its way, 500 ms in it is there
        v.set_vowel(&p, 2.0, SR);
        for _ in 0..960 {
            v.sample(&p, 1.0, SR);
        }
        let f2 = v.formants()[1];
        assert!(f2 > 1150.0 && f2 < 2200.0, "{f2}");
        for _ in 0..48_000 {
            v.sample(&p, 1.0, SR);
        }
        assert!(
            libm::fabs(v.formants()[1] - 2290.0) < 5.0,
            "{:?}",
            v.formants()
        );
    }

    #[test]
    fn a_consonant_starts_from_its_locus() {
        let p = VoiceParams::default();
        let mut v = FormantVoice::new(1);
        v.start(&p, 200.0, Syllable::parse("wi"), SR);
        v.sample(&p, 1.0, SR);
        // w starts with F2 610 Hz; i is 2290
        assert!(
            libm::fabs(v.formants()[1] - 610.0) < 20.0,
            "{:?}",
            v.formants()
        );
        // a stop's voicing waits for its gap: t is silent of voice for 50 ms
        let x = render(&p, 200.0, Syllable::parse("ta"), 4800);
        let late = &x[3600..];
        assert!(late.iter().map(|s| s.abs()).fold(0f32, f32::max) > 0.0);
    }

    #[test]
    fn renders_are_deterministic() {
        let p = preset("robot-choir").unwrap().params;
        let a = render(&p, 196.0, Syllable::parse("roo"), 9600);
        let b = render(&p, 196.0, Syllable::parse("roo"), 9600);
        assert_eq!(a, b);
        let mut s1 = FormantSynth::new(SR, preset("choir-aah").unwrap().params);
        let mut s2 = s1.clone();
        let mut o1 = vec![0f32; 4800];
        let mut o2 = vec![0f32; 4800];
        for s in [&mut s1, &mut s2] {
            s.note_on(60);
            s.note_on(64);
        }
        s1.render(&mut o1);
        s2.render(&mut o2);
        assert_eq!(o1, o2);
    }

    #[test]
    fn presets_are_found_by_instrument_name() {
        for (k, name) in instrument_names().iter().enumerate() {
            assert_eq!(instrument_preset(name).unwrap().name, PRESETS[k].name);
        }
        assert!(preset("voice-robot").is_some());
        assert!(instrument_preset("robot").is_none());
    }

    #[test]
    fn the_synth_gates_and_frees_its_voices() {
        let mut s = FormantSynth::new(SR, preset("robot").unwrap().params);
        s.set_gate_times(5.0, 20.0);
        let mut out = vec![0f32; 2400];
        s.note_on(57);
        s.render(&mut out);
        assert_eq!(s.sounding(), 1);
        assert!(out.iter().any(|x| x.abs() > 0.01));
        s.set_vowel(4.0);
        assert_eq!(s.vowel(), 4.0);
        s.note_off(57);
        out.fill(0.0);
        s.render(&mut out);
        assert_eq!(s.sounding(), 0);
    }

    // MARK: the lyrics' consonants, codas and diphthongs

    /// The share of `x`'s Goertzel energy above 3 kHz (a spectral tilt, 0..1).
    fn brightness(x: &[f32]) -> f64 {
        let (mut lo, mut hi) = (1e-12, 1e-12);
        for k in 1..60 {
            let f = 100.0 * k as f64 * 1.12;
            if f > 20_000.0 {
                break;
            }
            let e = tone(x, f);
            if f < 3000.0 {
                lo += e * e;
            } else {
                hi += e * e;
            }
        }
        hi / (lo + hi)
    }

    #[test]
    fn extended_syllables_round_trip_as_text() {
        for s in [
            "fa", "va", "za", "sha", "zha", "cha", "ja", "tha", "dha", "nga", "ah+ih", "ra+ih.t",
            "sha+oo.ng", "ka.z", ".s", ".t", "ae", "ih.n", "er.d", "ma.m",
        ] {
            let syl = Syllable::parse(s).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(syl.text(), s, "{s}");
        }
        let r = Syllable::parse("ra+ih.t").unwrap();
        assert_eq!((r.onset, r.vowel, r.glide, r.coda), (Onset::R, 0, Some(8), Onset::T));
        // a plain code carries the onset and the vowel only
        assert_eq!(Syllable::from_code(r.code()), Some(Syllable::new(Onset::R, 0)));
        assert_eq!(Syllable::parse("sha.").map(|_| ()), None);
        assert_eq!(Syllable::parse("sh"), None);
        assert!(Syllable::consonant(Onset::T).text() == ".t");
        // every old code means what it did
        assert_eq!(Syllable::new(Onset::H, 6).code(), (14 << 8) | 7);
    }

    #[test]
    fn fricatives_differ_by_where_their_noise_sits() {
        let p = preset("robot").unwrap().params;
        let n = 4800;
        let bright = |o: Onset| brightness(&render(&p, 110.0, Some(Syllable::consonant(o)), n));
        let (s, sh, f, th) = (bright(Onset::S), bright(Onset::Sh), bright(Onset::F), bright(Onset::Th));
        assert!(s > 0.5 && sh > 0.2, "s {s} sh {sh}");
        assert!(s > sh, "s is brighter than sh: {s} {sh}");
        // f and th are weak
        let level = |o: Onset| {
            let x = render(&p, 110.0, Some(Syllable::consonant(o)), n);
            x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
        };
        assert!(level(Onset::S) > level(Onset::F), "s louder than f");
        assert!(level(Onset::Sh) > level(Onset::Th));
        assert!(f.is_finite() && th.is_finite());
    }

    #[test]
    fn voiced_fricatives_buzz_and_voiceless_ones_do_not() {
        let p = preset("robot").unwrap().params;
        let n = 3600;
        let low = |o: Onset| {
            let x = render(&p, 110.0, Some(Syllable::consonant(o)), n);
            (1..8).map(|h| tone(&x, 110.0 * h as f64).powi(2)).sum::<f64>()
        };
        assert!(low(Onset::Z) > 5.0 * low(Onset::S), "z buzzes: {} {}", low(Onset::Z), low(Onset::S));
        assert!(low(Onset::V) > 5.0 * low(Onset::F));
        assert!(low(Onset::Zh) > 5.0 * low(Onset::Sh));
    }

    #[test]
    fn an_affricate_bursts_first_then_hisses() {
        let p = preset("robot").unwrap().params;
        let x = render(&p, 110.0, Some(Syllable::consonant(Onset::Ch)), 6000);
        let early = x[..480].iter().map(|v| v.abs()).fold(0f32, f32::max);
        let mid = x[1200..2400].iter().map(|v| v.abs()).fold(0f32, f32::max);
        assert!(early > 0.05 && mid > 0.02, "{early} {mid}");
    }

    #[test]
    fn a_coda_closes_the_note() {
        let p = preset("robot").unwrap().params;
        let sr = SR;
        let run = |coda: Onset| {
            let mut s = FormantSynth::new(sr, p);
            s.set_gate_times(5.0, 20.0);
            let mut syl = Syllable::new(Onset::D, 0);
            syl.coda = coda;
            s.note_on_syllable(57, Some(syl));
            let mut out = vec![0f32; 14_400];
            s.render(&mut out);
            s.note_off(57);
            let mut tail = vec![0f32; 9600];
            s.render(&mut tail);
            (out, tail, s.sounding())
        };
        let (_, plain_tail, _) = run(Onset::None);
        let (_, t_tail, _) = run(Onset::T);
        let (_, s_tail, _) = run(Onset::S);
        let (_, n_tail, _) = run(Onset::N);
        // without a coda the note dies within ~20 ms; with one it keeps sounding for the consonant
        let e = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        let (plain, t, sx, nn) = (e(&plain_tail[1500..]), e(&t_tail[1000..]), e(&s_tail[1000..]), e(&n_tail[1000..]));
        assert!(plain < 1e-6, "plain note is silent after its release: {plain}");
        assert!(t > 1e-4 && sx > 1e-4 && nn > 1e-4, "{t} {sx} {nn}");
        // the s coda is a hiss: bright; the n coda a hum: dark
        assert!(brightness(&s_tail[500..4000]) > 3.0 * brightness(&n_tail[500..4000]));
        // and every coda ends by itself
        let mut s = FormantSynth::new(SR, p);
        s.set_gate_times(5.0, 20.0);
        let mut syl = Syllable::new(Onset::B, 1);
        syl.coda = Onset::Ng;
        s.note_on_syllable(60, Some(syl));
        let mut out = vec![0f32; 4800];
        s.render(&mut out);
        s.note_off(60);
        s.note_off(60); // twice is the same
        for _ in 0..20 {
            s.render(&mut out);
        }
        assert_eq!(s.sounding(), 0);
    }

    #[test]
    fn a_diphthong_glides_to_its_second_vowel() {
        let p = preset("choir-aah").unwrap().params;
        let mut v = FormantVoice::new(5);
        let mut syl = Syllable::new(Onset::None, 0); // a
        syl.glide = Some(8); // ih
        v.start(&p, 110.0, Some(syl), SR);
        for _ in 0..(SR * 0.02) as usize {
            v.sample(&p, 1.0, SR);
        }
        let early = v.formants();
        for _ in 0..(SR * 0.6) as usize {
            v.sample(&p, 1.0, SR);
        }
        let late = v.formants();
        // a (730/1090) toward ih (390/1990)
        assert!(early[0] > 650.0 && early[1] < 1300.0, "{early:?}");
        assert!((late[0] - 390.0).abs() < 60.0 && (late[1] - 1990.0).abs() < 200.0, "{late:?}");
    }

    #[test]
    fn every_onset_and_coda_is_bounded_and_deterministic() {
        let p = preset("robot-choir").unwrap().params;
        for o in Onset::ALL {
            for vowel in [0u8, 4, 9] {
                let mut syl = Syllable::new(o, vowel);
                syl.coda = o;
                let mut s = FormantSynth::new(SR, p);
                s.set_gate_times(5.0, 30.0);
                s.note_on_syllable(52, Some(syl));
                let mut a = vec![0f32; 9600];
                s.render(&mut a);
                s.note_off(52);
                let mut b = vec![0f32; 9600];
                s.render(&mut b);
                assert!(a.iter().chain(b.iter()).all(|x| x.is_finite() && x.abs() < 4.0), "{o:?}");
                let mut s2 = FormantSynth::new(SR, p);
                s2.set_gate_times(5.0, 30.0);
                s2.note_on_syllable(52, Some(syl));
                let mut a2 = vec![0f32; 9600];
                s2.render(&mut a2);
                assert_eq!(a, a2, "{o:?}");
            }
        }
    }
}
