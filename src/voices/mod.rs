//! Voicing: which sampled instrument a net plays (port of `Audio/VoiceCatalog.swift`, the
//! pure parts of `Audio/Sampler.swift`, `Audio/DrumKits.swift`).
//!
//! beats' composer still picks its instrument names (so tracks stay beats-identical); each
//! name maps to a *category* (`data/categories.json`), and a net plays a pick from the sounds
//! installed in that category, seeded by the track and the net's role. Everything here is a
//! pure function of the catalog data and a list of what is installed: no files, no audio.
//! Hosts discover the installed content (`crate::host::sampler`) and pass it in.
//!
//! The order a net tries things in — the *instrument fallback ranking* — is
//! [`VoiceCatalog::sources`]: the seeded pick from its category's pool, the rest of the pool
//! (rotated), then the category's General MIDI programs from each SoundFont (the floor bank
//! last), then, for `keepSynth` categories, the synth voice, else Apple's own GM (`.gm`).

pub mod bank;
pub mod drumkits;
pub mod pitch;
pub mod sampler;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{JsonError, JsonValue};

/// FNV-1a: the same on every run (Swift's `Hasher` is seeded per process).
/// `VoiceCatalog.stableHash`.
pub fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A `Double` as Swift's string interpolation prints it (`"\(seed)"`), which is what the
/// pick hashes are keyed on: `1984.0`, `0.5`, `1e+16`. Integral values below 1e16 and
/// ordinary fractions are exact; exponent forms follow Swift's `1e-07` style.
pub fn swift_double(d: f64) -> String {
    if d.is_nan() {
        return "nan".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = d.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Rust prints `1e-7` / `1.5e16`; Swift prints `1e-07` / `1.5e+16`.
        let s = format!("{:e}", d);
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = e.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{}e{}{:02}", m, sign, exp.abs());
    }
    let s = format!("{}", d);
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

/// `(dir as NSString).appendingPathComponent(name)`.
pub fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `NSString.lastPathComponent`.
pub fn last_component(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or(t)
}

/// `NSString.deletingPathExtension` of a bare component.
fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

// MARK: SamplerSource

/// Where a sampled instrument comes from (`SamplerSource`). All of it lives on the machine
/// that plays it and is never copied into the repo or published.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SamplerSource {
    /// A Logic / GarageBand EXS instrument (absolute path). **Apple-only.**
    Exs(String),
    /// A General MIDI program (0-based) from macOS's own `gs_instruments.dls`.
    /// **Apple-only.**
    Gm(i64),
    /// A General MIDI program (0-based) from a SoundFont bank (absolute path), melodic
    /// bank (MSB 0x79).
    Sf2(String, i64),
    /// A drum kit from a sound bank (.sf2 or .dls) for a net's drum lanes (`DrumKits`):
    /// `bank` 128 is the percussion bank, any other a melodic bank whose preset maps
    /// fixed-pitch drum zones onto the GM keys.
    Kit {
        path: String,
        bank: i64,
        program: i64,
    },
}

impl core::fmt::Display for SamplerSource {
    /// `SamplerSource.description`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SamplerSource::Exs(p) => write!(f, "exs:{}", last_component(p)),
            SamplerSource::Gm(n) => write!(f, "gm:{n}"),
            SamplerSource::Sf2(p, n) => write!(f, "sf2:{}#{n}", last_component(p)),
            SamplerSource::Kit {
                path,
                bank,
                program,
            } => write!(f, "kit:{}#{bank}:{program}", last_component(path)),
        }
    }
}

impl SamplerSource {
    /// `Logic/08 Guitars/Vintage Strat.exs` → its absolute path (absolute paths pass
    /// through). `SamplerSource.exs(named:)`.
    pub fn exs_named(name: &str) -> SamplerSource {
        for (prefix, root) in EXS_ROOTS {
            if let Some(rest) = name.strip_prefix(prefix) {
                return SamplerSource::Exs(format!("{root}{rest}"));
            }
        }
        SamplerSource::Exs(name.to_string())
    }

    /// A drum kit (`isKit`), level-checked as one.
    pub fn is_kit(&self) -> bool {
        matches!(self, SamplerSource::Kit { .. })
    }

    /// From a bank that is only ever played on this machine (`localOnlyBanks`): level-checked
    /// on load but not tuning-checked.
    pub fn is_local_only(&self) -> bool {
        match self {
            SamplerSource::Sf2(p, _) | SamplerSource::Kit { path: p, .. } => {
                LOCAL_ONLY_BANKS.contains(&last_component(p))
            }
            SamplerSource::Exs(_) | SamplerSource::Gm(_) => false,
        }
    }
}

/// `SamplerSource.roots`: Apple's sampler-instrument libraries. Apple-only.
pub const EXS_ROOTS: [(&str, &str); 2] = [
    (
        "Logic/",
        "/Library/Application Support/Logic/Sampler Instruments/",
    ),
    (
        "GarageBand/",
        "/Library/Application Support/GarageBand/Instrument Library/Sampler/Sampler Instruments/",
    ),
];

/// Apple's GM bank, `SamplerSource.gmBank`. Apple-only.
pub const GM_BANK_PATH: &str =
    "/System/Library/Components/CoreAudio.component/Contents/Resources/gs_instruments.dls";
/// `gs_instruments.dls`.
pub const GM_BANK_FILE: &str = "gs_instruments.dls";

/// The floor under every category: General MIDI from a free, complete bank
/// (`SamplerSource.floorSoundFont`).
pub const FLOOR_SOUND_FONT: &str = "MuseScore_General.sf2";

/// Banks in the SoundFont folder that are not General MIDI and so never join a category's
/// `gm` programs: they are played only by name (`SamplerSource.localOnlyBanks`).
pub const LOCAL_ONLY_BANKS: [&str; 1] = ["minecraft-local.sf2"];

/// Bank files whose sounds a published set may not play (`SamplerSource.localOnlyFiles`).
pub fn local_only_files() -> BTreeSet<&'static str> {
    let mut s: BTreeSet<&'static str> = LOCAL_ONLY_BANKS.into_iter().collect();
    s.insert(GM_BANK_FILE);
    s
}

/// The .sf2 banks among `names` (the SoundFont folder's listing), in the order voicing
/// tries them: others by name, then [`FLOOR_SOUND_FONT`] last. Local-only banks never join.
/// `SamplerSource.soundFonts(in:)`, with the directory listing passed in.
pub fn sound_fonts_in(dir: &str, names: &[String]) -> Vec<String> {
    let mut v: Vec<&String> = names
        .iter()
        .filter(|n| n.to_lowercase().ends_with(".sf2") && !LOCAL_ONLY_BANKS.contains(&n.as_str()))
        .collect();
    v.sort_by(|a, b| {
        ((a.as_str() == FLOOR_SOUND_FONT) as u8, a.as_str())
            .cmp(&((b.as_str() == FLOOR_SOUND_FONT) as u8, b.as_str()))
    });
    v.into_iter().map(|n| join_path(dir, n)).collect()
}

// MARK: catalog

/// A sampled instrument found on this machine, sorted into a category (`CatalogSound`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSound {
    /// Display name (`Vintage Strat`).
    pub name: String,
    pub category: String,
    /// Every installed copy, best first (Logic before GarageBand, combined patches before
    /// articulations, iOS copies last).
    pub sources: Vec<SamplerSource>,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub category: String,
    pub any: Vec<String>,
    pub not: Vec<String>,
}

/// `VoiceCatalog.synthOnly`: beats instruments that always play their own synth voice.
pub const SYNTH_ONLY: [&str; 1] = ["laser"];

/// Words in a sound's name that stand for an instrument, beyond the instrument's own words
/// (`VoiceCatalog.nameWords`).
const NAME_WORDS: &[(&str, &[&str])] = &[
    ("chiptune", &["bitrate", "8-bit", "8 bit", "chip"]),
    ("tape-lead", &["tape", "vhs"]),
    ("talkbox", &["talk", "vocoder"]),
    ("screech", &["screech", "scream"]),
    ("scream-lead", &["scream", "screech"]),
    ("supersaw", &["supersaw", "super saw"]),
    ("detuned-saw", &["detune"]),
    ("pwm-lead", &["pwm", "pulse"]),
    ("sync-lead", &["sync"]),
    ("trance-lead", &["trance"]),
    ("hoover", &["hoover"]),
    ("wobble-lead", &["wobble"]),
    ("music-box", &["music box"]),
    ("steel-drum", &["steel"]),
    ("electric-piano", &["electric piano", "rhodes", "wurli"]),
    ("glass-pad", &["glass"]),
    ("dark-pad", &["dark"]),
    ("warm-pad", &["warm"]),
    ("am-pad", &["am "]),
];
/// Generic words that say nothing about which sound fits (`VoiceCatalog.vagueWords`).
const VAGUE_WORDS: [&str; 8] = ["lead", "pad", "synth", "saw", "am", "fm", "big", "duo"];

/// Does a sound's name fit the instrument (one of its words, or of `nameWords`)?
/// `VoiceCatalog.matches`.
pub fn matches(sound: &str, instrument: &str) -> bool {
    let name = sound.to_lowercase();
    let own = instrument
        .split('-')
        .filter(|w| !w.is_empty() && !VAGUE_WORDS.contains(w));
    let extra = NAME_WORDS
        .iter()
        .find(|(k, _)| *k == instrument)
        .map(|(_, w)| *w)
        .unwrap_or(&[]);
    own.chain(extra.iter().copied()).any(|w| name.contains(w))
}

/// The voice catalog: rules, the instrument → category map, GM programs, and what is
/// installed (`VoiceCatalog`).
#[derive(Clone, Debug)]
pub struct VoiceCatalog {
    pub rules: Vec<Rule>,
    /// beats instrument name → category.
    pub category_of: BTreeMap<String, String>,
    /// category → General MIDI programs (0-based), the SoundFont floor.
    pub gm: BTreeMap<String, Vec<i64>>,
    pub keep_synth: BTreeSet<String>,
    /// category → other categories whose sounds it also draws on.
    extra_pools: BTreeMap<String, Vec<String>>,
    /// Installed sounds, grouped by category, each group sorted by name.
    pub sounds: BTreeMap<String, Vec<CatalogSound>>,
    /// SoundFont banks (.sf2 paths) whose `gm` programs come after the pool, best first.
    pub sound_fonts: Vec<String>,
}

/// The bundled `categories.json` text.
pub const CATEGORIES_JSON: &str = include_str!("../../data/categories.json");

impl VoiceCatalog {
    /// The bundled rules and maps with nothing installed.
    pub fn bundled() -> VoiceCatalog {
        VoiceCatalog::from_json(
            &JsonValue::parse(CATEGORIES_JSON).expect("bundled categories.json parses"),
            BTreeMap::new(),
            Vec::new(),
        )
    }

    /// The bundled catalog with `sounds` and `sound_fonts` installed.
    pub fn with_installed(
        sounds: BTreeMap<String, Vec<CatalogSound>>,
        sound_fonts: Vec<String>,
    ) -> VoiceCatalog {
        let mut c = VoiceCatalog::bundled();
        c.sounds = sounds;
        c.sound_fonts = sound_fonts;
        c
    }

    /// `VoiceCatalog.init(json:roots:soundFonts:)`, with the scan done by the caller
    /// ([`scan`]).
    pub fn from_json(
        json: &JsonValue,
        sounds: BTreeMap<String, Vec<CatalogSound>>,
        sound_fonts: Vec<String>,
    ) -> VoiceCatalog {
        fn strs(v: Option<&JsonValue>) -> Vec<String> {
            v.and_then(JsonValue::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        let rules = json
            .get("rules")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(Rule {
                            category: r.get("category")?.as_str()?.to_string(),
                            any: strs(r.get("any")),
                            not: strs(r.get("not")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut category_of = BTreeMap::new();
        if let Some(o) = json.get("instruments").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                if let Some(s) = v.as_str() {
                    category_of.insert(k.to_string(), s.to_string());
                }
            }
        }
        let mut gm = BTreeMap::new();
        if let Some(o) = json.get("gm").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                gm.insert(k.to_string(), v.ints().unwrap_or_default());
            }
        }
        let mut extra_pools = BTreeMap::new();
        if let Some(o) = json.get("pools").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                extra_pools.insert(k.to_string(), strs(Some(v)));
            }
        }
        VoiceCatalog {
            rules,
            category_of,
            gm,
            keep_synth: strs(json.get("keepSynth")).into_iter().collect(),
            extra_pools,
            sounds,
            sound_fonts,
        }
    }

    /// Parse a catalog from JSON text (tests, user overrides).
    pub fn from_text(
        text: &str,
        sounds: BTreeMap<String, Vec<CatalogSound>>,
        sound_fonts: Vec<String>,
    ) -> Result<VoiceCatalog, JsonError> {
        Ok(VoiceCatalog::from_json(
            &JsonValue::parse(text)?,
            sounds,
            sound_fonts,
        ))
    }

    /// The first rule whose words match the path (folders and name), lowercased
    /// (`VoiceCatalog.categorize`).
    pub fn categorize(rel_path: &str, rules: &[Rule]) -> Option<String> {
        let (dir, file) = match rel_path.rfind('/') {
            Some(i) => (&rel_path[..=i], &rel_path[i + 1..]),
            None => ("", rel_path),
        };
        let stem = format!("{dir}{}", strip_extension(file)).to_lowercase();
        let text = format!(" {}", stem.replace('/', " / "));
        for r in rules {
            if r.any.iter().any(|w| text.contains(w.as_str())) {
                if r.not.iter().any(|w| text.contains(w.as_str())) {
                    continue;
                }
                return Some(r.category.clone());
            }
        }
        None
    }

    /// `Trumpets+` / `Trumpets Legato` / `Flute iOS KB` → one stem; lower rank = preferred
    /// copy (the combined `+` patch, then the plain one, then articulations, then iOS
    /// copies). `VoiceCatalog.canonical`.
    pub fn canonical(name: &str) -> (String, i64) {
        let mut s = name.to_string();
        let mut rank = 1;
        if s.ends_with('+') {
            s.pop();
            rank = 0;
        }
        for (suffix, r) in [
            (" iOS KB", 3),
            (" KB", 3),
            (" Legato", 2),
            (" Staccato", 2),
            (" Crescendo", 2),
        ] {
            if s.ends_with(suffix) {
                s.truncate(s.len() - suffix.len());
                rank = rank.max(r);
            }
        }
        if s.ends_with(" iOS") {
            s.truncate(s.len() - 4);
            rank = 3;
        }
        (s.trim().to_string(), rank)
    }

    /// Every sound a category can play (its own, then any pools it borrows)
    /// (`VoiceCatalog.pool`).
    pub fn pool(&self, category: &str) -> Vec<CatalogSound> {
        let mut out = self.sounds.get(category).cloned().unwrap_or_default();
        if let Some(extra) = self.extra_pools.get(category) {
            for e in extra {
                out.extend(self.sounds.get(e).cloned().unwrap_or_default());
            }
        }
        out
    }

    /// What a net with `instrument` should try, in order: the seeded pick from its
    /// category's pool, the rest of the pool, then the category's `gm` programs from each
    /// SoundFont, then — for a `keepSynth` category — its synth voice, else Apple's own GM
    /// programs. `None` when the pick is the synth voice or the instrument has no category.
    ///
    /// A `keepSynth` category offers its synth voice as one of the picks only beside
    /// installed sounds; with none a SoundFont program comes first.
    ///
    /// `picked`: the user chose this instrument (a share `tracks` override). Then the pick
    /// follows the name — a sound whose name has one of the instrument's words first — else
    /// the seeded pick with the name in the hash. Without it (what the composer chose) the
    /// pick is the category's, so tracks keep their sound. `VoiceCatalog.sources`.
    pub fn sources(
        &self,
        instrument: &str,
        role: &str,
        seed: f64,
        picked: bool,
    ) -> Option<Vec<SamplerSource>> {
        let category = self.category_of.get(instrument)?;
        let mut pool = self.pool(category);
        let synth = self.keep_synth.contains(category);
        if picked {
            if let Some(i) = pool.iter().position(|s| matches(&s.name, instrument)) {
                let first = pool.remove(i);
                pool.insert(0, first);
                return Some(pool.into_iter().flat_map(|s| s.sources).collect());
            }
        }
        let offers_synth = synth && !pool.is_empty();
        let count = pool.len() + usize::from(offers_synth);
        let mut ordered: Vec<SamplerSource> = Vec::new();
        if count > 0 {
            let seed_s = swift_double(seed);
            let key = if picked {
                format!("{seed_s}|{role}|{category}|{instrument}")
            } else {
                format!("{seed_s}|{role}|{category}")
            };
            let pick = (stable_hash(&key) % count as u64) as usize;
            if offers_synth && pick == pool.len() {
                return None;
            }
            for k in 0..pool.len() {
                ordered.extend(pool[(pick + k) % pool.len()].sources.iter().cloned());
            }
        }
        let programs = self.gm.get(category).cloned().unwrap_or_default();
        for bank in &self.sound_fonts {
            ordered.extend(
                programs
                    .iter()
                    .map(|p| SamplerSource::Sf2(bank.clone(), *p)),
            );
        }
        // A category with a synth voice of its own falls back to it, not to Apple's General
        // MIDI (a tuned plucked sitar beats the GM one).
        if !synth {
            ordered.extend(programs.iter().map(|p| SamplerSource::Gm(*p)));
        }
        if ordered.is_empty() {
            None
        } else {
            Some(ordered)
        }
    }

    /// What a role beats has no instrument list for can play: harmony the sustained
    /// families (pads first, then strings, keys, organ, choir, bells); any other, every
    /// tonal instrument. `Deck.fallbackOptions`.
    pub fn fallback_options(&self, role: &str) -> Vec<String> {
        let categories: Vec<String> = if role == "harmony" {
            [
                "pad",
                "strings",
                "keys.epiano",
                "keys.piano",
                "keys.organ",
                "vocal",
                "synth.bell",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        } else {
            let set: BTreeSet<&String> = self.category_of.values().collect();
            set.into_iter()
                .filter(|c| !c.starts_with("bass"))
                .cloned()
                .collect()
        };
        categories
            .iter()
            .flat_map(|c| {
                self.category_of
                    .iter()
                    .filter(move |(_, v)| *v == c)
                    .map(|(k, _)| k.clone())
            })
            .collect()
    }
}

/// The scan of Logic / GarageBand EXS libraries, with the directory walks done by the
/// caller: `roots` is each root path with every relative path under it, in enumeration
/// order. Pure port of `VoiceCatalog.scan`. (EXS is Apple-only; on other hosts the list is
/// simply empty and the pools fall through to General MIDI.)
pub fn scan(
    roots: &[(String, Vec<String>)],
    rules: &[Rule],
) -> BTreeMap<String, Vec<CatalogSound>> {
    struct Acc {
        name: String,
        category: String,
        copies: Vec<(i64, SamplerSource)>,
    }
    let mut by_key: BTreeMap<String, Acc> = BTreeMap::new();
    // Logic's library first: where both have an instrument, its copy is the complete one.
    let mut ordered: Vec<&(String, Vec<String>)> = roots.iter().collect();
    ordered.sort_by(|a, b| (!a.0.contains("/Logic/"), &a.0).cmp(&(!b.0.contains("/Logic/"), &b.0)));
    for (r, (root, rels)) in ordered.iter().enumerate() {
        for rel in rels {
            if !rel.ends_with(".exs") {
                continue;
            }
            let name = strip_extension(last_component(rel)).to_string();
            let Some(category) = VoiceCatalog::categorize(rel, rules) else {
                continue;
            };
            if category == "skip" {
                continue;
            }
            // Articulation and platform copies of one instrument are one sound, tried in order.
            let (stem, rank) = VoiceCatalog::canonical(&name);
            let key = format!("{category}|{}", stem.to_lowercase());
            let src = SamplerSource::Exs(join_path(root, rel));
            by_key
                .entry(key)
                .or_insert_with(|| Acc {
                    name: stem,
                    category: category.clone(),
                    copies: Vec::new(),
                })
                .copies
                .push((rank * 10 + r as i64, src));
        }
    }
    let mut out: BTreeMap<String, Vec<CatalogSound>> = BTreeMap::new();
    for (_, mut v) in by_key {
        v.copies.sort_by_key(|c| c.0);
        out.entry(v.category.clone())
            .or_default()
            .push(CatalogSound {
                name: v.name,
                category: v.category,
                sources: v.copies.into_iter().map(|c| c.1).collect(),
            });
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.name.cmp(&b.name));
    }
    out
}

// MARK: General MIDI names

/// General MIDI program names (0-based), as MuseScore_General names them
/// (`GeneralMIDI.name`).
pub fn general_midi_name(program: i64) -> String {
    if program >= 0 && (program as usize) < GM_NAMES.len() {
        GM_NAMES[program as usize].to_string()
    } else {
        format!("GM {program}")
    }
}

const GM_NAMES: [&str; 128] = [
    "Grand Piano",
    "Bright Grand Piano",
    "Electric Grand",
    "Honky-Tonk Piano",
    "Tine Electric Piano",
    "FM Electric Piano",
    "Harpsichord",
    "Clavinet",
    "Celesta",
    "Glockenspiel",
    "Music Box",
    "Vibraphone",
    "Marimba",
    "Xylophone",
    "Tubular Bells",
    "Dulcimer",
    "Drawbar Organ",
    "Percussive Organ",
    "Rock Organ",
    "Church Organ",
    "Reed Organ",
    "Accordion",
    "Harmonica",
    "Bandoneon",
    "Nylon String Guitar",
    "Steel String Guitar",
    "Jazz Guitar",
    "Clean Guitar",
    "Palm Muted Guitar",
    "Overdrive Guitar",
    "Distortion Guitar",
    "Guitar Harmonics",
    "Acoustic Bass",
    "Fingered Bass",
    "Picked Bass",
    "Fretless Bass",
    "Slap Bass",
    "Pop Bass",
    "Synth Bass 1",
    "Synth Bass 2",
    "Violin",
    "Viola",
    "Cello",
    "Contrabass",
    "Strings Tremolo",
    "Strings Pizzicato",
    "Harp",
    "Timpani",
    "Strings Fast",
    "Strings Slow",
    "Synth Strings 1",
    "Synth Strings 2",
    "Choir Aahs",
    "Voice Oohs",
    "Synth Voice",
    "Orchestra Hit",
    "Trumpet",
    "Trombone",
    "Tuba",
    "Harmon Mute Trumpet",
    "French Horns",
    "Brass Section",
    "Synth Brass 1",
    "Synth Brass 2",
    "Soprano Sax",
    "Alto Sax",
    "Tenor Sax",
    "Baritone Sax",
    "Oboe",
    "English Horn",
    "Bassoon",
    "Clarinet",
    "Piccolo",
    "Flute",
    "Recorder",
    "Pan Flute",
    "Bottle Chiff",
    "Shakuhachi",
    "Whistle",
    "Ocarina",
    "Square Lead",
    "Saw Lead",
    "Calliope Lead",
    "Chiffer Lead",
    "Charang",
    "Solo Vox",
    "5th Saw Wave",
    "Bass & Lead",
    "Fantasia",
    "Warm Pad",
    "Polysynth",
    "Space Voice",
    "Bowed Glass",
    "Metal Pad",
    "Halo Pad",
    "Sweep Pad",
    "Ice Rain",
    "Soundtrack",
    "Crystal",
    "Atmosphere",
    "Brightness",
    "Goblin",
    "Echo Drops",
    "Star Theme",
    "Sitar",
    "Banjo",
    "Shamisen",
    "Koto",
    "Kalimba",
    "Bagpipe",
    "Fiddle",
    "Shenai",
    "Tinker Bell",
    "Agogo",
    "Steel Drums",
    "Woodblock",
    "Taiko Drum",
    "Melodic Tom",
    "Synth Drum",
    "Reverse Cymbal",
    "Fret Noise",
    "Breath Noise",
    "Sea Shore",
    "Bird Tweet",
    "Telephone",
    "Helicopter",
    "Applause",
    "Gun Shot",
];
