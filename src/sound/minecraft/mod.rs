//! The Minecraft palette's sound bank (`Audio/MinecraftBank.swift`, `Audio/MinecraftAssets.swift`):
//! the file names, the programs at their fixed numbers, the kit's zones and the sidecar that
//! says what was built, then the importer that builds `minecraft-local.sf2` from the user's own
//! installed game sounds.
//!
//! The importer is split where Swift's is: what is arithmetic on samples and bytes lives here
//! with no I/O ([`assets`]: locating and checking the installed assets over an [`assets::AssetFs`];
//! [`import`]: root measurement, crossfade loops, octave zones and shifts, the kit's levels;
//! [`sf2`]: the SoundFont 2 writer), and what touches a disk or decodes an Ogg Vorbis file
//! (`MinecraftImport.decode` is AVFoundation) is `crate::host::minecraft`.
//!
//! Licensing, as in Swift: the output is built from the user's own game assets, stays on their
//! machine and is never committed, shipped or published (`session::publish` refuses a session that
//! plays it; [`LOCAL_ONLY_FILES`](crate::session::publish::LOCAL_ONLY_FILES) names the file).
//! `music/*` and `records/*` (the soundtrack and discs) are never read ([`assets::is_forbidden`]).

pub mod assets;
pub mod import;
pub mod sf2;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::RangeInclusive;
use once_cell::race::OnceBox;

use crate::json::{JsonObject, JsonValue};

pub const FILE_NAME: &str = "minecraft-local.sf2";
pub const SIDECAR_NAME: &str = "minecraft-local.json";
/// The kit preset: fixed-pitch zones on the GM drum keys (melodic bank 0).
pub const KIT_PROGRAM: i64 = 127;
/// `MinecraftBank.kitID`.
pub fn kit_id() -> String {
    format!("{FILE_NAME}#0:{KIT_PROGRAM}")
}
/// The bank layout recorded in the sidecar (`formatVersion`): 1 = one zone per tone program;
/// 2 = each tone program in three octave zones ([`import::octave_zones`]). A sidecar without
/// one is format 1.
pub const FORMAT_VERSION: i64 = 2;
pub const SIDECAR_SCHEMA: &str = concat!(env!("DJRUST_FORMAT_NS"), "/minecraft-local/v1");

/// `MinecraftBank.Kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Pitched, played across the keyboard from its root; `looped`: a crossfade loop holds it.
    Tone,
    /// Fixed pitch, played as recorded on any key (stingers).
    Fx,
}

/// `MinecraftBank.Program`: one program of the bank, a Minecraft sound at a fixed program
/// number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub name: String,
    pub program: i64,
    /// The note block event that names it (its sound comes from sounds.json), or none.
    pub event: Option<String>,
    /// The sound path (`note/harp2`), used when the event is missing.
    pub sound: String,
    pub kind: Kind,
    /// MIDI key the recording sounds at (Minecraft's note blocks are all F#).
    pub root: i64,
    /// Swift's `loop`.
    pub looped: bool,
    /// The first index that has it (copper trumpets came with index 30).
    pub min_index: i64,
}

fn tone(
    name: &str,
    program: i64,
    event: &str,
    sound: &str,
    root: i64,
    looped: bool,
    min_index: i64,
) -> Program {
    Program {
        name: name.into(),
        program,
        event: Some(format!("block.note_block.{event}")),
        sound: sound.into(),
        kind: Kind::Tone,
        root,
        looped,
        min_index,
    }
}

fn fx(name: &str, program: i64, sound: &str, event: Option<&str>) -> Program {
    Program {
        name: name.into(),
        program,
        event: event.map(String::from),
        sound: sound.into(),
        kind: Kind::Fx,
        root: 60,
        looped: false,
        min_index: 0,
    }
}

/// `MinecraftBank.programs`, every program at its fixed number (sessions name sounds by
/// program through the palette). Roots are the note blocks' F#: bass and didgeridoo F#2,
/// guitar F#3, harp / bit / banjo / pling / iron xylophone / trumpets F#4, flute and cow bell
/// F#5, bell, xylophone and chime F#6; the importer measures each and corrects the cents.
pub fn bank_programs() -> &'static [Program] {
    static SHARED: OnceBox<Vec<Program>> = OnceBox::new();
    SHARED.get_or_init(|| {
        let mut v = alloc::vec![
            tone("harp2", 0, "harp", "note/harp2", 66, false, 0),
            tone("bassattack", 1, "bass", "note/bassattack", 42, false, 0),
            tone(
                "didgeridoo",
                2,
                "didgeridoo",
                "note/didgeridoo",
                42,
                true,
                0
            ),
            tone("guitar", 3, "guitar", "note/guitar", 54, false, 0),
            tone("bit", 4, "bit", "note/bit", 66, true, 0),
            tone("banjo", 5, "banjo", "note/banjo", 66, false, 0),
            tone("pling", 6, "pling", "note/pling", 66, false, 0),
            tone(
                "iron_xylophone",
                7,
                "iron_xylophone",
                "note/iron_xylophone",
                66,
                false,
                0
            ),
            tone("flute", 8, "flute", "note/flute", 78, true, 0),
            tone("cow_bell", 9, "cow_bell", "note/cow_bell", 78, false, 0),
            tone("bell", 10, "bell", "note/bell", 90, false, 0),
            tone("xylobone", 11, "xylophone", "note/xylobone", 90, false, 0),
            tone("icechime", 12, "chime", "note/icechime", 90, false, 0),
            tone("trumpet", 13, "trumpet", "note/trumpet", 66, true, 30),
            tone(
                "trumpet_exposed",
                14,
                "trumpet_exposed",
                "note/trumpet_exposed",
                66,
                true,
                30
            ),
            tone(
                "trumpet_weathered",
                15,
                "trumpet_weathered",
                "note/trumpet_weathered",
                66,
                true,
                30
            ),
            tone(
                "trumpet_oxidized",
                16,
                "trumpet_oxidized",
                "note/trumpet_oxidized",
                66,
                true,
                30
            ),
        ];
        for i in 0..8 {
            v.push(fx(
                &format!("goat_horn_call{i}"),
                32 + i,
                &format!("item/goat_horn/call{i}"),
                Some(&format!("item.goat_horn.sound.{i}")),
            ));
        }
        v.push(fx("bell_use", 40, "block/bell/bell_use01", None));
        v.push(fx("bell_resonate", 41, "block/bell/resonate", None));
        v.push(fx("amethyst_shimmer", 42, "block/amethyst/shimmer", None));
        v.push(fx("anvil_land", 43, "random/anvil_land", None));
        v.push(fx("orb", 44, "random/orb", None));
        Box::new(v)
    })
}

/// `MinecraftBank.programs` as (name, number) pairs: what palette selection needs.
pub fn programs() -> Vec<(String, i64)> {
    bank_programs()
        .iter()
        .map(|p| (p.name.clone(), p.program))
        .collect()
}

/// `MinecraftBank.program(named:)`.
pub fn program_named(name: &str) -> Option<i64> {
    bank_programs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.program)
}

/// One zone of the kit preset (`MinecraftBank.kitZones`): GM keys → a sound, played at its own
/// pitch. `level` is the energy of one hit against one kick (dB): the recordings are not
/// balanced against each other (the snare and hat are a third of the kick's length), so the
/// import scales them to a mixed kit's balance, in a bar of four kicks, two snares and sixteen
/// hats, `MixBalance.targetDB`'s snare 6 dB and hats about 10 dB under the kick. A zone that
/// reuses another's sound takes its level.
#[derive(Clone, Debug, PartialEq)]
pub struct KitZone {
    pub name: &'static str,
    pub sound: &'static str,
    pub keys: RangeInclusive<i64>,
    pub level: f64,
}

/// `MinecraftBank.kitZones`.
pub fn kit_zones() -> [KitZone; 4] {
    [
        KitZone {
            name: "bd",
            sound: "note/bd",
            keys: 35..=36,
            level: 0.0,
        },
        KitZone {
            name: "snare",
            sound: "note/snare",
            keys: 37..=41,
            level: -3.0,
        },
        KitZone {
            name: "hat",
            sound: "note/hat",
            keys: 42..=46,
            level: -13.0,
        },
        KitZone {
            name: "crash",
            sound: "note/hat",
            keys: 49..=57,
            level: -13.0,
        },
    ]
}

/// `MinecraftBank.path(dir:)`: the bank's file in the SoundFont folder
/// (`NSString.appendingPathComponent`).
pub fn path(dir: &str) -> String {
    append_path_component(dir, FILE_NAME)
}

/// The sidecar's path in the SoundFont folder.
pub fn sidecar_path(dir: &str) -> String {
    append_path_component(dir, SIDECAR_NAME)
}

/// `NSString.appendingPathComponent`: the two joined by a `/`, runs of slashes collapsed to one
/// and no trailing slash (a lone `/` stays the root).
pub fn append_path_component(dir: &str, name: &str) -> String {
    let joined = if dir.is_empty() {
        String::from(name)
    } else {
        format!("{dir}/{name}")
    };
    let mut out = String::with_capacity(joined.len());
    let mut slash = false;
    for c in joined.chars() {
        if c == '/' {
            if slash {
                continue;
            }
            slash = true;
        } else {
            slash = false;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// `MinecraftBank.Sidecar`: what the bank was built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidecar {
    /// The asset index (`"32"`) the sounds came from.
    pub asset_index: String,
    /// Program name (and kit zone `kit:bd`...) to the object's SHA-1.
    pub sha1: BTreeMap<String, String>,
    /// The programs built (some are missing from older indexes).
    pub programs: Vec<String>,
    /// The bank layout when it was built ([`FORMAT_VERSION`]; a sidecar without one is
    /// format 1).
    pub format: i64,
}

impl Sidecar {
    /// `Sidecar.init(assetIndex:sha1:programs:format:)`.
    pub fn new(
        asset_index: impl Into<String>,
        sha1: BTreeMap<String, String>,
        programs: Vec<String>,
        format: i64,
    ) -> Sidecar {
        Sidecar {
            asset_index: asset_index.into(),
            sha1,
            programs,
            format,
        }
    }

    /// What a track records as the palette's version: the asset index, plus the bank format
    /// when it is not 1 (`32+b2`), so a recording names the sounds and how they were cut.
    pub fn version(&self) -> String {
        if self.format > 1 {
            format!("{}+b{}", self.asset_index, self.format)
        } else {
            self.asset_index.clone()
        }
    }

    /// `Sidecar.json`: schema, assetIndex, format, programs, then the hashes in sorted order.
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("schema", JsonValue::str(SIDECAR_SCHEMA));
        o.set("assetIndex", JsonValue::str(self.asset_index.clone()));
        o.set("format", JsonValue::Number(self.format as f64));
        o.set(
            "programs",
            JsonValue::Array(
                self.programs
                    .iter()
                    .map(|p| JsonValue::str(p.clone()))
                    .collect(),
            ),
        );
        let mut h = JsonObject::new();
        for (k, v) in &self.sha1 {
            h.set(k.clone(), JsonValue::str(v.clone()));
        }
        o.set("sha1", JsonValue::Object(h));
        JsonValue::Object(o)
    }

    /// The file's text: `canonicalJS` and a newline (what `MinecraftImport.run` writes).
    pub fn file_text(&self) -> String {
        let mut t = self.json().canonical_js();
        t.push('\n');
        t
    }

    /// `Sidecar.init?(json:)`.
    pub fn from_json(v: &JsonValue) -> Option<Sidecar> {
        if v.get("schema")?.as_str()? != SIDECAR_SCHEMA {
            return None;
        }
        let mut sha1 = BTreeMap::new();
        if let Some(h) = v.get("sha1").and_then(JsonValue::as_object) {
            for (k, x) in h.iter() {
                if let Some(s) = x.as_str() {
                    sha1.insert(String::from(k), String::from(s));
                }
            }
        }
        Some(Sidecar {
            asset_index: v.get("assetIndex")?.as_str()?.into(),
            sha1,
            programs: v
                .get("programs")
                .and_then(JsonValue::strings)
                .unwrap_or_default(),
            format: v.get("format").and_then(JsonValue::as_i64).unwrap_or(1),
        })
    }

    /// The text half of `Sidecar.load(dir:)`: parse it, then `init?(json:)`.
    pub fn parse(text: &str) -> Option<Sidecar> {
        JsonValue::parse(text)
            .ok()
            .and_then(|v| Sidecar::from_json(&v))
    }
}
