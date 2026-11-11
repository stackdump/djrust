//! Sound palettes and how a track resolves its sounds (port of `Audio/SoundPalette.swift`
//! and `Audio/PaletteSound.swift`).
//!
//! A palette (`data/palettes.json`) is a named sound world for a whole track: for each role
//! — the drums (`kit`), bass, melody, harmony, arp, stingers — a pool of options. A track
//! plays one option per role, picked by the track seed + role ([`SoundPalette::pick`],
//! applied by [`apply_sound`]), so the same configuration always sounds the same.
//!
//! Everything here is a pure function of the palette data, the voice catalog and an
//! [`Installed`] description of what the host has (SoundFont files, Apple's GM bank, the
//! Minecraft bank). Nothing touches a disk.

pub mod minecraft;
pub mod role_sounds;
pub mod stinger;

pub use role_sounds::{playing_palette, role_sounds, RoleSoundChoice, RoleSoundRow};

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use crate::gen::tables::BeatsTables;
use crate::json::{JsonObject, JsonValue};
use crate::pflow::{NetBundle, Project};
use crate::voices::drumkits::{self, DrumKit};
use crate::voices::{
    general_midi_name, join_path, local_only_files, sound_fonts_in, stable_hash, swift_double,
    SamplerSource, VoiceCatalog, GM_BANK_FILE,
};
use crate::wave::instruments::InstrumentTable;

pub use minecraft::Sidecar as MinecraftSidecar;

/// The bundled `palettes.json` text.
pub const PALETTES_JSON: &str = include_str!("../../data/palettes.json");

/// What the host has installed: the inputs to every "can this play here" question
/// (`SamplerSource.soundFontDir` and the file system probes of the Swift code).
#[derive(Clone, Debug, Default)]
pub struct Installed {
    /// The SoundFont folder.
    pub dir: String,
    /// The folder's file names.
    pub files: Vec<String>,
    /// Apple's `gs_instruments.dls` exists (Mac only).
    pub gm_bank: bool,
    /// The kits found in the banks (`DrumKits.installed`), for labels.
    pub kits: Vec<DrumKit>,
    /// The Minecraft bank's sidecar when the bank and its sidecar are both installed
    /// (`MinecraftBank.isInstalled`).
    pub minecraft: Option<MinecraftSidecar>,
    /// Why the Minecraft bank cannot be built here (`MinecraftAssets.unavailableReason`),
    /// nil when the game's assets are there to import from.
    pub minecraft_assets_missing: Option<String>,
}

impl Installed {
    /// Nothing installed.
    pub fn nothing(dir: &str) -> Installed {
        Installed {
            dir: dir.to_string(),
            ..Installed::default()
        }
    }

    pub fn has_file(&self, name: &str) -> bool {
        self.files.iter().any(|f| f == name)
    }

    /// The .sf2 banks in voicing order (`SamplerSource.soundFonts(in:)`).
    pub fn sound_fonts(&self) -> Vec<String> {
        sound_fonts_in(&self.dir, &self.files)
    }

    /// `MinecraftBank.isInstalled`.
    pub fn minecraft_installed(&self) -> bool {
        self.has_file(minecraft::FILE_NAME) && self.minecraft.is_some()
    }

    /// `MinecraftBank.unavailableReason`.
    pub fn minecraft_reason(&self) -> Option<String> {
        if self.minecraft_installed() {
            return None;
        }
        if let Some(why) = &self.minecraft_assets_missing {
            return Some(why.clone());
        }
        Some("not imported yet: run `make minecraft` (djctl minecraft import)".into())
    }

    /// `DrumKits.label`: a kit id's readable name, or the id.
    pub fn kit_label(&self, id: &str) -> String {
        self.kits
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.label())
            .unwrap_or_else(|| id.to_string())
    }
}

/// The catalog and the installed content together: the context every question is asked in.
#[derive(Clone, Copy)]
pub struct Sounds<'a> {
    pub catalog: &'a VoiceCatalog,
    pub installed: &'a Installed,
}

static NOTHING: OnceBox<(VoiceCatalog, Installed)> = OnceBox::new();

impl Sounds<'static> {
    /// The context of a host that knows nothing about its sounds: the bundled catalog with no
    /// SoundFont, bank or sampled instrument installed. A palette then resolves as Swift does
    /// on a machine with none (its synth voices and `own`/`inst:` options still play; every
    /// sampled option is left out of the pick). What [`TrackSpec::finish`] asks in; a host
    /// that has scanned its folders passes its own ([`TrackSpec::finish_in`]).
    ///
    /// [`TrackSpec::finish`]: crate::spec::TrackSpec::finish
    /// [`TrackSpec::finish_in`]: crate::spec::TrackSpec::finish_in
    pub fn nothing() -> Sounds<'static> {
        let (catalog, installed) =
            NOTHING.get_or_init(|| Box::new((VoiceCatalog::bundled(), Installed::default())));
        Sounds { catalog, installed }
    }
}

/// A category played from one bank: sound names (programs), or a kit preset's program
/// (`SoundPalette.BankCategory`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BankCategory {
    pub bank: String,
    pub sounds: Vec<String>,
    pub kit: Option<i64>,
}

/// A sound palette (`SoundPalette`).
#[derive(Clone, Debug, PartialEq)]
pub struct SoundPalette {
    pub id: String,
    pub label: String,
    /// An emoji for the chooser.
    pub icon: String,
    /// `#RRGGBB`, the card's accent.
    pub color: String,
    pub description: String,
    /// Where Wander's palette hop may go from here.
    pub neighbours: Vec<String>,
    /// Role (bass, melody, harmony, arp, stinger, `*` = any other tonal role, `kit` = the
    /// drums) → options, in the order the chooser shows them. Insertion-ordered.
    pub roles: Vec<(String, Vec<String>)>,
    /// The genre whose room / echo the palette plays in (`hints`); nil = the track's genre's.
    pub room: Option<String>,
    pub echo: Option<String>,
}

/// Every shipped palette, the bank categories and the default id (`SoundPalette.data`).
#[derive(Debug)]
pub struct PaletteSet {
    pub palettes: Vec<SoundPalette>,
    pub categories: BTreeMap<String, BankCategory>,
    pub default_id: String,
}

/// The roles a palette names, in the chooser's order (`SoundPalette.roleOrder`).
pub const ROLE_ORDER: [&str; 6] = ["kit", "bass", "melody", "harmony", "arp", "stinger"];

static PALETTES: OnceBox<PaletteSet> = OnceBox::new();

impl PaletteSet {
    /// The bundled palettes, parsed once.
    pub fn shared() -> &'static PaletteSet {
        PALETTES.get_or_init(|| {
            let cats = JsonValue::parse(crate::voices::CATEGORIES_JSON)
                .map(|j| Self::parse_categories(&j))
                .unwrap_or_default();
            let pals = JsonValue::parse(PALETTES_JSON).expect("bundled palettes.json parses");
            let (palettes, default_id) = Self::parse_palettes(&pals);
            Box::new(PaletteSet {
                palettes,
                categories: cats,
                default_id,
            })
        })
    }

    /// `SoundPalette.parse(categories:)`.
    pub fn parse_categories(json: &JsonValue) -> BTreeMap<String, BankCategory> {
        let mut cats = BTreeMap::new();
        if let Some(o) = json.get("bankCategories").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                let Some(bank) = v.get("bank").and_then(JsonValue::as_str) else {
                    continue;
                };
                cats.insert(
                    k.to_string(),
                    BankCategory {
                        bank: bank.to_string(),
                        sounds: v
                            .get("sounds")
                            .and_then(JsonValue::strings)
                            .unwrap_or_default(),
                        kit: v.get("kit").and_then(JsonValue::as_i64),
                    },
                );
            }
        }
        cats
    }

    /// `SoundPalette.parse(palettes:)`.
    pub fn parse_palettes(json: &JsonValue) -> (Vec<SoundPalette>, String) {
        let mut out: Vec<SoundPalette> = Vec::new();
        if let Some(o) = json.get("palettes").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                let Some(roles) = v.get("roles").and_then(JsonValue::as_object) else {
                    continue;
                };
                let roles = roles
                    .iter()
                    .map(|(r, opts)| (r.to_string(), opts.strings().unwrap_or_default()))
                    .collect();
                let s = |key: &str| v.get(key).and_then(JsonValue::as_str).map(String::from);
                out.push(SoundPalette {
                    id: k.to_string(),
                    label: s("label").unwrap_or_else(|| k.to_string()),
                    icon: s("icon").unwrap_or_default(),
                    color: s("color").unwrap_or_else(|| "#8E8E93".into()),
                    description: s("description").unwrap_or_default(),
                    neighbours: v
                        .get("neighbours")
                        .and_then(JsonValue::strings)
                        .unwrap_or_default(),
                    roles,
                    room: v
                        .get("hints")
                        .and_then(|h| h.get("room"))
                        .and_then(JsonValue::as_str)
                        .map(String::from),
                    echo: v
                        .get("hints")
                        .and_then(|h| h.get("echo"))
                        .and_then(JsonValue::as_str)
                        .map(String::from),
                });
            }
        }
        let default = json
            .get("default")
            .and_then(JsonValue::as_str)
            .map(String::from)
            .or_else(|| out.first().map(|p| p.id.clone()))
            .unwrap_or_else(|| "studio".into());
        (out, default)
    }

    /// The palette a track names: its id, optionally with the asset version it was played
    /// with (`minecraft@32`). `SoundPalette.named`.
    pub fn named(&self, id: Option<&str>) -> Option<&SoundPalette> {
        let id = id.filter(|s| !s.is_empty())?;
        let bare = id.split('@').next().unwrap_or(id);
        self.palettes.iter().find(|p| p.id == bare)
    }

    /// The palette a track plays: the one it names, else the default
    /// (`SoundPalette.playing`).
    pub fn playing(&self, id: Option<&str>) -> Option<&SoundPalette> {
        self.named(id)
            .or_else(|| self.named(Some(&self.default_id)))
    }

    pub fn bank_category(&self, option: &str) -> Option<&BankCategory> {
        option
            .strip_prefix("bank:")
            .and_then(|c| self.categories.get(c))
    }

    /// `SoundPalette.normalized`: what a track writes for palette `id` — nil for none or the
    /// default, else the id with its asset version when it comes without one.
    pub fn normalized(&self, id: Option<&str>, installed: &Installed) -> Option<String> {
        let id = id.filter(|s| !s.is_empty())?;
        let Some(p) = self.named(Some(id)) else {
            return Some(id.to_string());
        };
        if p.id == self.default_id {
            return None;
        }
        if version(Some(id)).is_none() {
            Some(p.recorded(self, installed))
        } else {
            Some(id.to_string())
        }
    }

    /// `SoundPalette.isLocalOnly(option:)`: an option (or kit id) that plays local-only
    /// content.
    pub fn is_local_only_option(&self, option: &str) -> bool {
        if option.starts_with("dls:") {
            return true;
        }
        if let Some(b) = self.bank_category(option) {
            return local_only_files().contains(b.bank.as_str());
        }
        if let Some((file, _, _)) = drumkits::parse(option) {
            return local_only_files().contains(file.as_str());
        }
        false
    }

    /// `SoundPalette.kitID(option:)`: the kit id a kit option plays, "synth" for one of
    /// beats' synth kits, nil for an option that is no kit.
    pub fn kit_id(&self, option: &str) -> Option<String> {
        if option == "synth" || option == "own" || synth_name(option).is_some() {
            return Some("synth".into());
        }
        if let Some(b) = self.bank_category(option) {
            if let Some(p) = b.kit {
                return Some(format!("{}#0:{}", b.bank, p));
            }
        }
        if drumkits::parse(option).is_some() {
            return Some(option.to_string());
        }
        None
    }

    /// `SoundPalette.kit(for:dir:)`: the kit a track's palette gives its drums, when the
    /// palette can play here.
    pub fn kit_for(&self, palette: Option<&str>, ctx: Sounds) -> Option<String> {
        let p = self.named(palette)?;
        if !p.is_playable(self, ctx) {
            return None;
        }
        p.kit(self)
    }
}

/// The asset version a recorded palette names (`minecraft@32` → "32"), or nil
/// (`SoundPalette.version`).
pub fn version(id: Option<&str>) -> Option<String> {
    let id = id?;
    let at = id.find('@')?;
    let v = &id[at + 1..];
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// `synth:<name>` → name (`SoundPalette.synthName`).
pub fn synth_name(option: &str) -> Option<&str> {
    option.strip_prefix("synth:")
}

/// `inst:<name>` → name: the role plays as if `name` were picked for it
/// (`SoundPalette.pickedName`).
pub fn picked_name(option: &str) -> Option<&str> {
    option.strip_prefix("inst:")
}

/// `isSynthDrumKit` and `isDrumChannel`: beats' synth drum kits (`wave::synth::synth_drum_kits`)
/// and the drum channels (10...15) are the wave engine's, which plays them.
pub use crate::wave::synth::{is_drum_channel, is_synth_drum_kit};

/// A beats instrument the synth engine voices (its own table or its families' words)
/// (`SoundPalette.knownInstrument`).
pub fn known_instrument(name: &str, catalog: &VoiceCatalog) -> bool {
    if InstrumentTable::shared().entries.contains_key(name) || catalog.category_of.contains_key(name) {
        return true;
    }
    let t = BeatsTables::shared();
    // beats' stinger voices (airhorn, subdrop, booj…) are in no genre's set, only the stingers'
    if name != "unbound" && t.stinger_instrument_set.iter().any(|s| s == name) {
        return true;
    }
    t.instrument_sets
        .values()
        .any(|set| set.values().any(|list| list.iter().any(|i| i == name)))
}

impl SoundPalette {
    pub fn is_default(&self, set: &PaletteSet) -> bool {
        self.id == set.default_id
    }

    /// How a track records this palette: with the version of what it plays here (the
    /// Minecraft bank's asset index), so a session, favorite or loop names what it was made
    /// with. `SoundPalette.recorded`.
    pub fn recorded(&self, set: &PaletteSet, installed: &Installed) -> String {
        if self.banks(set).contains(minecraft::FILE_NAME) {
            if let Some(sc) = &installed.minecraft {
                return format!("{}@{}", self.id, sc.version());
            }
        }
        self.id.clone()
    }

    /// The options for `role` (`*` for a tonal role the palette doesn't name), nil = the
    /// role plays its instrument's own sound. `SoundPalette.pool`.
    pub fn pool(&self, role: &str) -> Option<&[String]> {
        let get = |r: &str| self.roles.iter().find(|(k, _)| k == r).map(|(_, v)| v);
        if let Some(p) = get(role) {
            if !p.is_empty() {
                return Some(p);
            }
        }
        if role == "kit" {
            None
        } else {
            get("*").filter(|v| !v.is_empty()).map(|v| v.as_slice())
        }
    }

    fn role_named(&self, role: &str) -> bool {
        self.roles.iter().any(|(k, _)| k == role)
    }

    /// `role`'s options that can play here, in order (`SoundPalette.available`).
    pub fn available(&self, role: &str, set: &PaletteSet, ctx: Sounds) -> Vec<String> {
        self.pool(role)
            .unwrap_or(&[])
            .iter()
            .filter(|o| unavailable_reason_option(set, o, role, ctx).is_none())
            .cloned()
            .collect()
    }

    /// The option a net of `role` plays under this palette: one of the available options,
    /// picked by the track seed and the net (`key`: its id, so each stinger hit gets its
    /// own). nil when the palette has no pool for the role, or none of it is installed.
    /// `SoundPalette.pick`.
    pub fn pick(
        &self,
        role: &str,
        key: &str,
        seed: f64,
        set: &PaletteSet,
        ctx: Sounds,
    ) -> Option<String> {
        let ok = self.available(role, set, ctx);
        if ok.is_empty() {
            return None;
        }
        let i = (stable_hash(&format!("{}|{key}|palette:{}", swift_double(seed), self.id))
            % ok.len() as u64) as usize;
        Some(ok[i].clone())
    }

    /// Every role the palette names with why nothing of it can play here (nil: something
    /// can). `SoundPalette.roleReport`.
    pub fn role_report(&self, set: &PaletteSet, ctx: Sounds) -> Vec<(String, Option<String>)> {
        let mut names: Vec<&str> = ROLE_ORDER
            .iter()
            .copied()
            .filter(|r| self.role_named(r))
            .collect();
        if self.role_named("*") {
            names.push("*");
        }
        names
            .into_iter()
            .map(|role| {
                let opts = self.pool(role).unwrap_or(&[]);
                if opts
                    .iter()
                    .any(|o| unavailable_reason_option(set, o, role, ctx).is_none())
                {
                    return (role.to_string(), None);
                }
                let why = opts
                    .iter()
                    .find_map(|o| unavailable_reason_option(set, o, role, ctx));
                (
                    role.to_string(),
                    Some(why.unwrap_or_else(|| "no options".into())),
                )
            })
            .collect()
    }

    /// Why the palette can't play here, or nil: its bank is missing, or a role it names has
    /// nothing installed. `SoundPalette.unavailableReason`.
    pub fn unavailable_reason(&self, set: &PaletteSet, ctx: Sounds) -> Option<String> {
        if self.banks(set).contains(minecraft::FILE_NAME) {
            if let Some(why) = ctx.installed.minecraft_reason() {
                return Some(why);
            }
        }
        for (role, why) in self.role_report(set, ctx) {
            if let Some(why) = why {
                return Some(format!("{role}: {why}"));
            }
        }
        None
    }

    /// It can play here.
    pub fn is_playable(&self, set: &PaletteSet, ctx: Sounds) -> bool {
        self.unavailable_reason(set, ctx).is_none()
    }

    /// What a net of `role` tries first under this palette (its seeded pick's sounds), nil
    /// when the pick plays a synth voice, the palette has no pool for the role, or nothing
    /// of it is installed. `SoundPalette.sources(role:seed:dir:)`.
    pub fn sources(
        &self,
        role: &str,
        seed: f64,
        set: &PaletteSet,
        ctx: Sounds,
    ) -> Option<Vec<SamplerSource>> {
        let opt = self.pick(role, role, seed, set, ctx)?;
        sources_for_option(set, &opt, role, seed, ctx)
    }

    /// The banks this palette plays (local banks and Apple's GM bank).
    pub fn banks(&self, set: &PaletteSet) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (_, opts) in &self.roles {
            for o in opts {
                if let Some(b) = set.bank_category(o) {
                    out.insert(b.bank.clone());
                }
                if o.starts_with("dls:") {
                    out.insert(GM_BANK_FILE.to_string());
                }
                if let Some((file, _, _)) = drumkits::parse(o) {
                    out.insert(file);
                }
            }
        }
        out
    }

    /// Plays content that may only be played on this machine (the Minecraft bank, Apple's
    /// GM bank): never published. `SoundPalette.isLocalOnly`.
    pub fn is_local_only(&self, set: &PaletteSet) -> bool {
        self.roles
            .iter()
            .flat_map(|(_, v)| v.iter())
            .any(|o| set.is_local_only_option(o))
    }

    /// The kit id the palette's drums play first, or nil (a synth kit). `SoundPalette.kit`.
    pub fn kit(&self, set: &PaletteSet) -> Option<String> {
        let opts = self
            .roles
            .iter()
            .find(|(k, _)| k == "kit")
            .map(|(_, v)| v)?;
        for o in opts {
            if let Some(k) = set.kit_id(o) {
                if k != "synth" {
                    return Some(k);
                }
            }
        }
        None
    }
}

/// Why `option` can't play for `role` here, or nil when it can
/// (`SoundPalette.unavailableReason(option:role:dir:)` / `check`).
pub fn unavailable_reason_option(
    set: &PaletteSet,
    option: &str,
    role: &str,
    ctx: Sounds,
) -> Option<String> {
    let inst = ctx.installed;
    if option == "own" || option == "synth" {
        return None;
    }
    if let Some(name) = synth_name(option).or_else(|| picked_name(option)) {
        if role == "kit" {
            return if is_synth_drum_kit(name) {
                None
            } else {
                Some(format!("no synth kit {name}"))
            };
        }
        return if known_instrument(name, ctx.catalog) {
            None
        } else {
            Some(format!("no instrument {name}"))
        };
    }
    if role == "kit" {
        if let Some(b) = set.bank_category(option) {
            if b.kit.is_none() {
                return Some(format!("{option} has no kit"));
            }
            return bank_reason(&b.bank, inst);
        }
        let Some((file, _, _)) = drumkits::parse(option) else {
            return Some(format!("not a kit: {option}"));
        };
        return if drumkits::source_for(option, &inst.dir, &inst.files, inst.gm_bank).is_none() {
            Some(format!("{file} is not installed"))
        } else {
            None
        };
    }
    if let Some(c) = option.strip_prefix("cat:") {
        if !ctx.catalog.pool(c).is_empty() {
            return None;
        }
        if !ctx.catalog.gm.get(c).map(|g| g.is_empty()).unwrap_or(true)
            && !inst.sound_fonts().is_empty()
        {
            return None;
        }
        return Some(format!(
            "no {c} sound installed (make soundfonts installs the General MIDI floor)"
        ));
    }
    if let Some(n) = option.strip_prefix("gm:") {
        if n.parse::<i64>().is_err() {
            return Some(format!("not a program: {option}"));
        }
        return if inst.sound_fonts().is_empty() {
            Some(format!(
                "{} is not installed (make soundfonts)",
                crate::voices::FLOOR_SOUND_FONT
            ))
        } else {
            None
        };
    }
    if let Some(n) = option.strip_prefix("dls:") {
        if n.parse::<i64>().is_err() {
            return Some(format!("not a program: {option}"));
        }
        return if inst.gm_bank {
            None
        } else {
            Some("Apple's gs_instruments.dls is not on this Mac".into())
        };
    }
    if let Some(b) = set.bank_category(option) {
        if b.sounds.is_empty() {
            return Some(format!("{option} has no sounds"));
        }
        return bank_reason(&b.bank, inst);
    }
    Some(format!("unknown option {option}"))
}

/// `SoundPalette.bankReason`.
fn bank_reason(bank: &str, inst: &Installed) -> Option<String> {
    if bank == minecraft::FILE_NAME {
        return if inst.minecraft_installed() {
            None
        } else {
            Some(
                inst.minecraft_reason()
                    .unwrap_or_else(|| format!("{bank} is not installed")),
            )
        };
    }
    if inst.has_file(bank) {
        None
    } else {
        Some(format!("{bank} is not installed"))
    }
}

/// What `option` plays for `role` on a sampler: the seeded sound first (track seed + role),
/// then the rest; nil for an option that plays a synth voice (or nothing installed).
/// `SoundPalette.sources(option:role:seed:dir:)`.
pub fn sources_for_option(
    set: &PaletteSet,
    option: &str,
    role: &str,
    seed: f64,
    ctx: Sounds,
) -> Option<Vec<SamplerSource>> {
    let inst = ctx.installed;
    let seed_s = swift_double(seed);
    let mut out: Vec<SamplerSource> = Vec::new();
    if let Some(c) = option.strip_prefix("cat:") {
        let pool = ctx.catalog.pool(c);
        if !pool.is_empty() {
            let pick = (stable_hash(&format!("{seed_s}|{role}|{c}")) % pool.len() as u64) as usize;
            for k in 0..pool.len() {
                out.extend(pool[(pick + k) % pool.len()].sources.iter().cloned());
            }
        }
        let programs = ctx.catalog.gm.get(c).cloned().unwrap_or_default();
        for bank in inst.sound_fonts() {
            out.extend(
                programs
                    .iter()
                    .map(|p| SamplerSource::Sf2(bank.clone(), *p)),
            );
        }
    } else if let Some(n) = option
        .strip_prefix("gm:")
        .and_then(|n| n.parse::<i64>().ok())
    {
        out = inst
            .sound_fonts()
            .into_iter()
            .map(|b| SamplerSource::Sf2(b, n))
            .collect();
    } else if let Some(n) = option
        .strip_prefix("dls:")
        .and_then(|n| n.parse::<i64>().ok())
    {
        if inst.gm_bank {
            out = alloc::vec![SamplerSource::Gm(n)];
        }
    } else if let Some(c) = set.bank_category(option) {
        if !c.sounds.is_empty() && bank_reason(&c.bank, inst).is_none() {
            let category = &option[5..];
            let built: Option<&Vec<String>> = if c.bank == minecraft::FILE_NAME {
                inst.minecraft.as_ref().map(|s| &s.programs)
            } else {
                None
            };
            let programs: Vec<i64> = c
                .sounds
                .iter()
                .filter(|n| built.map(|b| b.contains(n)).unwrap_or(true))
                .filter_map(|name| {
                    if c.bank == minecraft::FILE_NAME {
                        minecraft::program_named(name)
                    } else {
                        None
                    }
                })
                .collect();
            if programs.is_empty() {
                return None;
            }
            let path = join_path(&inst.dir, &c.bank);
            let pick = (stable_hash(&format!("{seed_s}|{role}|{category}")) % programs.len() as u64)
                as usize;
            out = (0..programs.len())
                .map(|k| SamplerSource::Sf2(path.clone(), programs[(pick + k) % programs.len()]))
                .collect();
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// A short name for what `option` plays as `role` with `seed` (the sound a category picks,
/// a GM program's name, a kit's preset). `SoundPalette.label(option:role:seed:dir:)`.
pub fn option_label(set: &PaletteSet, option: &str, role: &str, seed: f64, ctx: Sounds) -> String {
    if option == "own" {
        return if role == "kit" {
            "synth kit".into()
        } else {
            "own sound".into()
        };
    }
    if option == "synth" {
        return if role == "kit" {
            "synth kit".into()
        } else {
            "synth voice".into()
        };
    }
    if let Some(n) = synth_name(option) {
        return if role == "kit" {
            format!("{} synth kit", n.replace("drums-", ""))
        } else {
            n.to_string()
        };
    }
    if let Some(n) = picked_name(option) {
        return n.to_string();
    }
    if role == "kit" {
        if let Some(id) = set.kit_id(option) {
            return ctx.installed.kit_label(&id);
        }
        return option.to_string();
    }
    if let Some(c) = option.strip_prefix("cat:") {
        let pool = ctx.catalog.pool(c);
        if !pool.is_empty() {
            let i = (stable_hash(&format!("{}|{role}|{c}", swift_double(seed))) % pool.len() as u64)
                as usize;
            return pool[i].name.clone();
        }
        if let Some(p) = ctx.catalog.gm.get(c).and_then(|g| g.first()) {
            return general_midi_name(*p);
        }
        return c.to_string();
    }
    if let Some(n) = option
        .strip_prefix("gm:")
        .and_then(|n| n.parse::<i64>().ok())
    {
        return general_midi_name(n);
    }
    if let Some(n) = option
        .strip_prefix("dls:")
        .and_then(|n| n.parse::<i64>().ok())
    {
        return format!("{} (Apple)", general_midi_name(n));
    }
    if let Some(b) = set.bank_category(option) {
        let category = &option[5..];
        if let Some(SamplerSource::Sf2(_, prog)) =
            sources_for_option(set, option, role, seed, ctx).and_then(|v| v.into_iter().next())
        {
            if let Some((name, _)) = minecraft::programs().into_iter().find(|(_, p)| *p == prog) {
                return name;
            }
        }
        return b
            .sounds
            .first()
            .cloned()
            .unwrap_or_else(|| category.to_string());
    }
    option.to_string()
}

// MARK: a track's sounds

/// What a net plays, as its palette and the user's choices resolve it (`TrackSound`): the
/// project model's own type, so [`apply_project_sound`] writes it straight onto the tracks.
pub use crate::pflow::TrackSound;

/// One net as the sound resolver sees it: the fields of a `NetBundle` + `Track` that
/// `Project.applySound` reads and writes. [`apply_project_sound`] fills these from a
/// project's own nets (the project model carries the dj-only track fields); a host that
/// holds the choices elsewhere (the stinger curation, a chooser preview) builds them itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundNet {
    pub id: String,
    /// `nb.role == "control"`: skipped.
    pub is_control: bool,
    pub channel: i64,
    /// `track.group`.
    pub group: Option<String>,
    pub riff_group: String,
    /// `track.instrument`.
    pub instrument: String,
    pub picked: bool,
    /// `track.kit`: the kit the user picked (`DeckAction.setKit`).
    pub kit: Option<String>,
    /// `track.palette`.
    pub palette: Option<String>,
    pub sound: TrackSound,
}

impl SoundNet {
    /// The nets of a composed project as the resolver sees them, every one on `palette`
    /// (what `TrackSpec.withPalette` writes on every role channel). The dj-only choices
    /// (`picked`, `kit`, `sound`) start empty: beats' model has none.
    pub fn from_project(project: &crate::pflow::Project, palette: Option<&str>) -> Vec<SoundNet> {
        project
            .nets
            .iter()
            .map(|(id, nb)| SoundNet {
                id: id.to_string(),
                is_control: nb.role == "control",
                channel: nb.track.channel,
                group: nb.track.group.clone(),
                riff_group: nb.riff_group.clone(),
                instrument: nb.track.instrument.clone(),
                picked: false,
                kit: None,
                palette: palette.filter(|p| !p.is_empty()).map(String::from),
                sound: TrackSound::default(),
            })
            .collect()
    }
}

/// The role a net's sound is chosen as (`pool`: the palette's role, `key`: what its pick is
/// seeded with): the drums share one kit, a tonal net is its group seeded by its riff group
/// or id (each stinger hit its own). `Project.soundRole`.
pub fn sound_role(n: &SoundNet) -> (String, String) {
    role_of(&n.id, n.channel, n.group.as_deref(), &n.riff_group)
}

fn role_of(id: &str, channel: i64, group: Option<&str>, riff_group: &str) -> (String, String) {
    if is_drum_channel(channel) {
        return ("kit".into(), "kit".into());
    }
    let r = if riff_group.is_empty() {
        id.to_string()
    } else {
        riff_group.to_string()
    };
    (group.map(String::from).unwrap_or_else(|| r.clone()), r)
}

/// Resolve every net's sound from its palette and the user's choices, as a pure function of
/// the nets' overrides, the track seed and what is installed: run after the share's
/// overrides and again after any sound poke. Idempotent: a palette's synth voice replaces
/// the instrument only until the next resolve. `Project.applySound(seed:dir:)`, over the
/// resolver's view of the nets ([`apply_project_sound`] is the one on a [`Project`]).
pub fn apply_sound(nets: &mut [SoundNet], seed: f64, set: &PaletteSet, ctx: Sounds) {
    for nb in nets.iter_mut() {
        resolve_net(nb, seed, set, ctx);
    }
}

/// `Project.applySound(seed:dir:)` on a project: every playing net's `instrument`, `picked`
/// and [`TrackSound`] resolved from its palette (`track.palette`), its role override
/// (`sound.override_`) and what is installed (`ctx`). Nets are independent of one another, so
/// the order they are visited in does not matter.
pub fn apply_project_sound(project: &mut Project, seed: f64, set: &PaletteSet, ctx: Sounds) {
    let ids: Vec<String> = project.nets.keys().to_vec();
    for id in ids {
        let Some(nb) = project.nets.get_mut(&id) else {
            continue;
        };
        if nb.role == "control" {
            continue;
        }
        let t = &nb.track;
        let mut n = SoundNet {
            id: id.clone(),
            is_control: false,
            channel: t.channel,
            group: t.group.clone(),
            riff_group: nb.riff_group.clone(),
            instrument: t.instrument.clone(),
            picked: t.picked,
            kit: t.kit.clone(),
            palette: t.palette.clone(),
            sound: t.sound.clone(),
        };
        resolve_net(&mut n, seed, set, ctx);
        let t = &mut nb.track;
        t.instrument = n.instrument;
        t.picked = n.picked;
        t.sound = n.sound;
    }
}

/// One net's turn of `Project.applySound`.
fn resolve_net(nb: &mut SoundNet, seed: f64, set: &PaletteSet, ctx: Sounds) {
    if nb.is_control {
        return;
    }
    let (pool, key) = sound_role(nb);
    let palette = set.named(nb.palette.as_deref());
    let override_pick = nb
        .sound
        .override_
        .as_deref()
        .and_then(|o| picked_name(o).map(|n| (o.to_string(), n.to_string())));
    if let (Some((o, name)), true) = (override_pick, pool != "kit") {
        // the user's pick of an instrument (`inst:` or a `tracks` instrument) plays as picked
        if nb.sound.composed.is_none() {
            nb.sound.composed = Some(nb.instrument.clone());
        }
        nb.instrument = name;
        nb.picked = true;
        nb.sound.option = Some(o);
        nb.sound.kit = None;
    } else if nb.picked && pool != "kit" {
        nb.sound.option = None;
        nb.sound.kit = None;
    } else {
        if let (Some(c), false) = (&nb.sound.composed, nb.picked) {
            nb.instrument = c.clone();
        }
        let opt = nb
            .sound
            .override_
            .clone()
            .or_else(|| {
                if pool == "stinger" {
                    nb.sound.curated.clone()
                } else {
                    None
                }
            })
            .or_else(|| palette.and_then(|p| p.pick(&pool, &key, seed, set, ctx)));
        nb.sound.option = opt.clone();
        nb.sound.kit = None;
        if let (Some(o), false) = (&opt, nb.picked) {
            if let Some(name) = synth_name(o) {
                // a synth voice (or synth kit) of another name: the lane voices that instrument
                if nb.sound.composed.is_none() {
                    nb.sound.composed = Some(nb.instrument.clone());
                }
                nb.instrument = name.to_string();
            }
        }
        if pool == "kit" {
            if let Some(o) = &opt {
                nb.sound.kit = set.kit_id(o);
            }
        }
    }
}

/// The channels whose nets play as `role`, sorted (`Project.channels(soundRole:)`).
pub fn channels_for_role(nets: &[SoundNet], role: &str) -> Vec<i64> {
    let mut out = BTreeSet::new();
    for n in nets {
        if !n.is_control && sound_role(n).0 == role {
            out.insert(n.channel);
        }
    }
    out.into_iter().collect()
}

/// The palette roles the project plays, in the chooser's order (`Project.soundRoles`).
pub fn sound_roles(nets: &[SoundNet]) -> Vec<String> {
    let have: BTreeSet<String> = nets
        .iter()
        .filter(|n| !n.is_control)
        .map(|n| sound_role(n).0)
        .collect();
    ROLE_ORDER
        .iter()
        .filter(|r| have.contains(**r))
        .map(|r| r.to_string())
        .collect()
}

/// The first net that plays as `role`, sorted by id (`Project.soundNet`).
pub fn sound_net<'a>(nets: &'a [SoundNet], role: &str) -> Option<&'a SoundNet> {
    let mut v: Vec<&SoundNet> = nets.iter().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v.into_iter()
        .find(|n| !n.is_control && sound_role(n).0 == role)
}

/// `Project.soundNet` on a project: the first playing net, by id, that plays as `role` (the
/// one the chooser's panel shows).
pub fn project_sound_net<'a>(project: &'a Project, role: &str) -> Option<&'a NetBundle> {
    let mut ids: Vec<&str> = project.nets.keys().iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.into_iter().find_map(|id| {
        let nb = project.nets.get(id)?;
        (nb.role != "control"
            && role_of(
                id,
                nb.track.channel,
                nb.track.group.as_deref(),
                &nb.riff_group,
            )
            .0 == role)
            .then_some(nb)
    })
}

// MARK: the spec's sound fields

/// The `tracks` override fields that are the deck's sound choices, not the song's
/// (`TrackSpec.soundFields`).
pub const SOUND_FIELDS: [&str; 3] = ["palette", "kit", "voice"];

/// `mine` (a spec's `tracks` override, channel → fields) with `other`'s sound choices — its
/// palette, kit pick and role overrides — on the same channels, merged into any override
/// `mine` has. What the deck carries into a new track (`TrackSpec.carryingSound`).
pub fn carrying_sound(mine: &JsonObject, other: &JsonObject) -> JsonObject {
    let mut all = mine.clone();
    for (ch, o) in other.iter() {
        let mut entry = all
            .get(ch)
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        let mut changed = false;
        if let Some(src) = o.as_object() {
            for f in SOUND_FIELDS {
                if let Some(v) = src.get(f) {
                    entry.set(f, v.clone());
                    changed = true;
                }
            }
        }
        if changed {
            all.set(ch, JsonValue::Object(entry));
        }
    }
    all
}

/// Every channel beats puts a playing role on in `genre` (the melody's and bass's from the
/// genre, the rest fixed): where a palette is written for a track not composed yet
/// (`TrackSpec.soundChannels(genre:)`).
pub fn sound_channels(genre: &str) -> Vec<i64> {
    let t = BeatsTables::shared();
    let g = t.genres.get(genre);
    let mut set: BTreeSet<i64> = [5, 7, 10, 11, 12, 13].into_iter().collect();
    set.extend(t.stinger_specs.iter().map(|s| s.channel));
    set.insert(g.map(|g| g.melody_channel).unwrap_or(4));
    set.insert(g.map(|g| g.bass_channel).unwrap_or(6));
    set.into_iter().collect()
}

// MARK: chooser cards

/// One palette as the chooser shows it (`PaletteCard`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteCard {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub color: String,
    pub description: String,
    /// Why it can't play here (the card greys out), nil when it can.
    pub unavailable: Option<String>,
    pub local_only: bool,
    pub selected: bool,
}

impl PaletteCard {
    /// Every shipped palette, `current` (a track's recorded palette, nil = the default) lit.
    pub fn cards(set: &PaletteSet, current: Option<&str>, ctx: Sounds) -> Vec<PaletteCard> {
        let here = set.playing(current).map(|p| p.id.clone());
        set.palettes
            .iter()
            .map(|p| PaletteCard {
                id: p.id.clone(),
                label: p.label.clone(),
                icon: p.icon.clone(),
                color: p.color.clone(),
                description: p.description.clone(),
                unavailable: p.unavailable_reason(set, ctx),
                local_only: p.is_local_only(set),
                selected: Some(&p.id) == here.as_ref(),
            })
            .collect()
    }

    /// The palette `delta` cards along from the lit one, among those that can play here,
    /// wrapping (the chooser's arrow keys; `soundPrev` / `soundNext`). None when no other card
    /// can play. With none lit, +1 is the first playable card and -1 the last.
    pub fn step(cards: &[PaletteCard], delta: i64) -> Option<String> {
        let playable: Vec<&PaletteCard> = cards.iter().filter(|c| c.unavailable.is_none()).collect();
        let n = playable.len() as i64;
        if n == 0 || delta == 0 {
            return None;
        }
        let at = playable.iter().position(|c| c.selected);
        let next = match at {
            Some(i) => (i as i64 + delta).rem_euclid(n),
            None if delta > 0 => (delta - 1).rem_euclid(n),
            None => (n + delta).rem_euclid(n),
        } as usize;
        (Some(next) != at).then(|| playable[next].id.clone())
    }
}
