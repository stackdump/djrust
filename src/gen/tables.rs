//! Typed view over `data/tables.json` (port of `Generator/Tables.swift`). The file is
//! beats-btw-dj's `Resources/beats/tables.json`, which its scripts/beats-tables.mjs lifts
//! from beats' generator modules (genres, theory, instruments, structure, energy).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use crate::gen::core::{scale_intervals, scale_notes};
use crate::json::{JsonObject, JsonValue};
use crate::pflow::OrderedMap;

pub const TABLES_JSON: &str = include_str!("../../data/tables.json");
/// djrust's own genres, beyond beats' catalogue (electro, deephouse, italo, acid): merged
/// into the shared tables after beats' own, so `tables.json` stays byte-for-byte beats'.
pub const NATIVE_GENRES_JSON: &str = include_str!("../../data/genres-native.json");

/// What a native genre carries beyond a beats genre's tables.
#[derive(Clone, Debug)]
pub struct NativeGenre {
    /// The beats genre whose hard-coded musicality rules (echo, chord colour, arp figure,
    /// voice design, beat style, ducking) it follows.
    pub like: String,
    /// arrange.js family ("edm", "song", …).
    pub arrange_family: String,
    /// stingers.json family ("club", "retro", …).
    pub stinger_family: String,
}

/// An ordered set of roles/sections: a JS object used as a record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrderedFlags {
    keys: Vec<String>,
}

impl OrderedFlags {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn from_names<I: IntoIterator<Item = S>, S: AsRef<str>>(names: I) -> Self {
        let mut f = Self::new();
        for n in names {
            f.set(n.as_ref());
        }
        f
    }
    pub fn set(&mut self, k: &str) {
        if !self.contains(k) {
            self.keys.push(k.to_string());
        }
    }
    pub fn contains(&self, k: &str) -> bool {
        self.keys.iter().any(|x| x == k)
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChordDegree {
    pub root: i64,
    pub tones: Vec<i64>,
}

impl ChordDegree {
    pub fn json(&self) -> JsonValue {
        JsonObject::from_pairs([
            ("root", JsonValue::int(self.root)),
            (
                "tones",
                JsonValue::Array(self.tones.iter().map(|t| JsonValue::int(*t)).collect()),
            ),
        ])
        .into()
    }
}

/// `{ chords: [{ root, tones }], stepsPer }`
#[derive(Clone, Debug, PartialEq)]
pub struct ChordProg {
    pub chords: Vec<ChordDegree>,
    pub steps_per: i64,
}

impl ChordProg {
    fn from_json(v: &JsonValue) -> Self {
        let chords = v
            .get("chords")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .map(|c| ChordDegree {
                        root: c.get("root").and_then(JsonValue::as_i64).unwrap_or(0),
                        tones: c.get("tones").and_then(JsonValue::ints).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        ChordProg {
            chords,
            steps_per: v.get("stepsPer").and_then(JsonValue::as_i64).unwrap_or(4),
        }
    }
    pub fn json(&self) -> JsonValue {
        JsonObject::from_pairs([
            (
                "chords",
                JsonValue::Array(self.chords.iter().map(ChordDegree::json).collect()),
            ),
            ("stepsPer", JsonValue::int(self.steps_per)),
        ])
        .into()
    }
}

#[derive(Clone, Debug)]
pub struct DrumVariant {
    pub hits_add: i64,
    pub hits_mul: Option<f64>,
    pub rotation_add: i64,
}

#[derive(Clone, Debug)]
pub struct GenreTheory {
    pub chord_progs: Vec<ChordProg>,
    pub drum_styles: BTreeMap<String, BTreeMap<String, DrumVariant>>,
    pub phrase_patterns: Option<OrderedMap<Vec<String>>>,
}

#[derive(Clone, Debug)]
pub struct GenreDef {
    pub name: String,
    pub bpm: f64,
    /// e.g. "MinorScale"
    pub scale_name: String,
    pub root_note: i64,
    pub kick: Vec<i64>,
    pub snare: Vec<i64>,
    pub hihat: Vec<i64>,
    pub melody_channel: i64,
    pub melody_density: f64,
    pub melody_duration: i64,
    pub bass_channel: i64,
    pub bass_density: f64,
    pub bass_duration: i64,
    pub swing: f64,
    pub humanize: f64,
    pub duration_variation: f64,
    pub syncopation: f64,
    pub ghost_notes: f64,
    pub drum_fills: bool,
    pub walking_bass: bool,
    pub call_response: bool,
    pub tension_curve: bool,
    pub modal_interchange: f64,
    pub polyrhythm: i64,
    pub theory: Option<GenreTheory>,
}

impl GenreDef {
    pub fn scale(&self, root: i64) -> Vec<i64> {
        scale_notes(root, scale_intervals(&self.scale_name), 2)
    }
}

#[derive(Clone, Debug)]
pub struct RoleProfile {
    pub active: bool,
    pub motif_mode: Option<i64>,
    pub register_shift: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SectionProfile {
    pub roles: OrderedMap<RoleProfile>,
    pub filter_open: f64,
    pub energy: f64,
}

#[derive(Clone, Debug)]
pub struct StingerSpec {
    pub id: String,
    pub channel: i64,
    pub default_instrument: String,
}

#[derive(Clone, Debug)]
pub struct ThemeTable {
    pub family: String,
    pub groove: i64,
    pub supported: bool,
    pub energy: OrderedMap<SectionProfile>,
}

#[derive(Debug)]
pub struct BeatsTables {
    pub genre_ids: Vec<String>,
    pub genres: BTreeMap<String, GenreDef>,
    pub instrument_sets: BTreeMap<String, OrderedMap<Vec<String>>>,
    pub minor_chord_prog: ChordProg,
    pub major_chord_prog: ChordProg,
    themes: BTreeMap<String, ThemeTable>,
    archetypes: JsonValue,
    archetypes_by_family: JsonValue,
    genre_families: BTreeMap<String, i64>,
    blueprints: JsonValue,
    pub stinger_specs: Vec<StingerSpec>,
    pub stinger_instrument_set: Vec<String>,
    fallback_sets: JsonValue,
    pub adjectives: Vec<String>,
    pub nouns: Vec<String>,
    pub grooves: BTreeMap<String, i64>,
    pub motif_modes: BTreeMap<String, i64>,
    /// The native genres (`genres-native.json`), by id; empty in tables parsed without them.
    pub native: BTreeMap<String, NativeGenre>,
    /// Native genres' instrument sets, kept apart from beats' `instrument_sets` so the
    /// instrument lists beats genres offer (every genre's, for a role) stay beats'.
    pub native_instrument_sets: BTreeMap<String, OrderedMap<Vec<String>>>,
}

static SHARED: OnceBox<BeatsTables> = OnceBox::new();

fn obj_iter(v: Option<&JsonValue>) -> impl Iterator<Item = (&str, &JsonValue)> {
    v.and_then(JsonValue::as_object)
        .into_iter()
        .flat_map(JsonObject::iter)
}

impl BeatsTables {
    /// The tables bundled with the crate, parsed once.
    pub fn shared() -> &'static BeatsTables {
        SHARED.get_or_init(|| {
            let mut t = JsonValue::parse(TABLES_JSON).expect("bundled tables.json parses");
            let native = JsonValue::parse(NATIVE_GENRES_JSON).expect("genres-native.json parses");
            let ids = merge_native(&mut t, &native);
            let mut tables = BeatsTables::from_json(&t);
            for id in ids.keys() {
                if let Some(set) = tables.instrument_sets.remove(id) {
                    tables.native_instrument_sets.insert(id.clone(), set);
                }
            }
            tables.native = ids;
            Box::new(tables)
        })
    }

    /// `genre`'s instrument set, beats' or native.
    pub fn instrument_set(&self, genre: &str) -> Option<&OrderedMap<Vec<String>>> {
        self.instrument_sets
            .get(genre)
            .or_else(|| self.native_instrument_sets.get(genre))
    }

    /// Whether `genre` is one of djrust's own (not in beats' catalogue).
    pub fn is_native(&self, genre: &str) -> bool {
        self.native.contains_key(genre)
    }

    /// The genre whose hard-coded musicality rules `genre` follows: a native genre's `like`,
    /// any other genre itself.
    pub fn kin<'a>(&'a self, genre: &'a str) -> &'a str {
        self.native.get(genre).map(|n| n.like.as_str()).unwrap_or(genre)
    }

    /// The beats genres only, in table order: what Swift's catalogue holds.
    pub fn beats_genre_ids(&self) -> Vec<String> {
        self.genre_ids
            .iter()
            .filter(|g| !self.is_native(g))
            .cloned()
            .collect()
    }

    pub fn from_json(t: &JsonValue) -> BeatsTables {
        fn theory(v: Option<&JsonValue>) -> Option<GenreTheory> {
            let v = v?;
            let mut styles = BTreeMap::new();
            for (role, vs) in obj_iter(v.get("drumStyles")) {
                let mut m = BTreeMap::new();
                for (letter, d) in obj_iter(Some(vs)) {
                    m.insert(
                        letter.to_string(),
                        DrumVariant {
                            hits_add: d.get("hitsAdd").and_then(JsonValue::as_i64).unwrap_or(0),
                            hits_mul: d.get("hitsMul").and_then(JsonValue::as_f64),
                            rotation_add: d
                                .get("rotationAdd")
                                .and_then(JsonValue::as_i64)
                                .unwrap_or(0),
                        },
                    );
                }
                styles.insert(role.to_string(), m);
            }
            let phrase_patterns =
                v.get("phrasePatterns")
                    .and_then(JsonValue::as_object)
                    .map(|pp| {
                        let mut m = OrderedMap::new();
                        for (k, a) in pp.iter() {
                            m.insert(k, a.strings().unwrap_or_default());
                        }
                        m
                    });
            Some(GenreTheory {
                chord_progs: v
                    .get("chordProgs")
                    .and_then(JsonValue::as_array)
                    .map(|a| a.iter().map(ChordProg::from_json).collect())
                    .unwrap_or_default(),
                drum_styles: styles,
                phrase_patterns,
            })
        }
        let theories = t.get("theories");
        let mut genres = BTreeMap::new();
        let mut genre_ids = Vec::new();
        for (id, g) in obj_iter(t.get("genres")) {
            genre_ids.push(id.to_string());
            let d = |k: &str| g.get(k).and_then(JsonValue::as_f64).unwrap_or(0.0);
            let i = |k: &str| g.get(k).and_then(JsonValue::as_i64).unwrap_or(0);
            let b = |k: &str| g.get(k).and_then(JsonValue::as_bool).unwrap_or(false);
            let ints = |k: &str| g.get(k).and_then(JsonValue::ints).unwrap_or_default();
            genres.insert(
                id.to_string(),
                GenreDef {
                    name: g
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or(id)
                        .to_string(),
                    bpm: d("bpm"),
                    scale_name: g
                        .get("scale")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("MajorScale")
                        .to_string(),
                    root_note: i("rootNote"),
                    kick: ints("kick"),
                    snare: ints("snare"),
                    hihat: ints("hihat"),
                    melody_channel: i("melodyChannel"),
                    melody_density: d("melodyDensity"),
                    melody_duration: i("melodyDuration"),
                    bass_channel: i("bassChannel"),
                    bass_density: d("bassDensity"),
                    bass_duration: i("bassDuration"),
                    swing: d("swing"),
                    humanize: d("humanize"),
                    duration_variation: d("durationVariation"),
                    syncopation: d("syncopation"),
                    ghost_notes: d("ghostNotes"),
                    drum_fills: b("drumFills"),
                    walking_bass: b("walkingBass"),
                    call_response: b("callResponse"),
                    tension_curve: b("tensionCurve"),
                    modal_interchange: d("modalInterchange"),
                    polyrhythm: i("polyrhythm"),
                    theory: theory(theories.and_then(|th| th.get(id))),
                },
            );
        }

        let mut instrument_sets = BTreeMap::new();
        for (id, s) in obj_iter(t.get("instrumentSets")) {
            let mut m = OrderedMap::new();
            for (role, a) in obj_iter(Some(s)) {
                m.insert(role, a.strings().unwrap_or_default());
            }
            instrument_sets.insert(id.to_string(), m);
        }

        let mut themes = BTreeMap::new();
        for (id, v) in obj_iter(t.get("themes")) {
            let mut energy = OrderedMap::new();
            for (sec, p) in obj_iter(v.get("energy")) {
                let mut roles = OrderedMap::new();
                for (role, rp) in obj_iter(p.get("roles")) {
                    roles.insert(
                        role,
                        RoleProfile {
                            active: rp
                                .get("active")
                                .and_then(JsonValue::as_bool)
                                .unwrap_or(false),
                            motif_mode: rp.get("motifMode").and_then(JsonValue::as_i64),
                            register_shift: rp.get("registerShift").and_then(JsonValue::as_i64),
                        },
                    );
                }
                energy.insert(
                    sec,
                    SectionProfile {
                        roles,
                        filter_open: p
                            .get("filterOpen")
                            .and_then(JsonValue::as_f64)
                            .unwrap_or(0.0),
                        energy: p.get("energy").and_then(JsonValue::as_f64).unwrap_or(0.0),
                    },
                );
            }
            themes.insert(
                id.to_string(),
                ThemeTable {
                    family: v
                        .get("family")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("edm")
                        .to_string(),
                    groove: v.get("groove").and_then(JsonValue::as_i64).unwrap_or(0),
                    supported: v
                        .get("supported")
                        .and_then(JsonValue::as_bool)
                        .unwrap_or(false),
                    energy,
                },
            );
        }

        let s = t.get("structure");
        let sget = |k: &str| s.and_then(|s| s.get(k)).cloned().unwrap_or(JsonValue::Null);
        let mut genre_families = BTreeMap::new();
        for (k, v) in obj_iter(s.and_then(|s| s.get("genreFamilies"))) {
            genre_families.insert(k.to_string(), v.as_i64().unwrap_or(1));
        }
        let st = t.get("stingers");
        let stinger_specs = st
            .and_then(|s| s.get("STINGER_SPECS"))
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| StingerSpec {
                        id: x
                            .get("id")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("")
                            .to_string(),
                        channel: x.get("channel").and_then(JsonValue::as_i64).unwrap_or(0),
                        default_instrument: x
                            .get("defaultInstrument")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let int_map = |v: Option<&JsonValue>| {
            let mut m = BTreeMap::new();
            for (k, x) in obj_iter(v) {
                m.insert(k.to_string(), x.as_i64().unwrap_or(0));
            }
            m
        };
        let words = |k: &str| {
            t.get("nameWords")
                .and_then(|w| w.get(k))
                .and_then(JsonValue::strings)
                .unwrap_or_default()
        };
        BeatsTables {
            genre_ids,
            genres,
            instrument_sets,
            minor_chord_prog: ChordProg::from_json(
                t.get("minorChordProg").unwrap_or(&JsonValue::Null),
            ),
            major_chord_prog: ChordProg::from_json(
                t.get("majorChordProg").unwrap_or(&JsonValue::Null),
            ),
            themes,
            archetypes: sget("sectionArchetypes"),
            archetypes_by_family: sget("sectionArchetypesByFamily"),
            genre_families,
            blueprints: sget("structureBlueprints"),
            stinger_specs,
            stinger_instrument_set: st
                .and_then(|s| s.get("STINGER_INSTRUMENT_SET"))
                .and_then(JsonValue::strings)
                .unwrap_or_default(),
            fallback_sets: st
                .and_then(|s| s.get("FALLBACK_SETS"))
                .cloned()
                .unwrap_or(JsonValue::Null),
            adjectives: words("adjectives"),
            nouns: words("nouns"),
            grooves: int_map(t.get("grooves")),
            motif_modes: int_map(t.get("motifModes")),
            native: BTreeMap::new(),
            native_instrument_sets: BTreeMap::new(),
        }
    }

    pub fn theme_table(&self, genre: &str) -> Option<&ThemeTable> {
        self.themes.get(genre)
    }

    /// structure.js `archetypeFor(family, name)`: None when the section has none.
    pub fn archetype(&self, family: i64, name: &str) -> Option<OrderedFlags> {
        let fam = self
            .archetypes_by_family
            .get(&family.to_string())
            .and_then(|f| f.get(name))
            .or_else(|| self.archetypes.get(name));
        fam.and_then(JsonValue::as_object)
            .map(|o| OrderedFlags::from_names(o.keys()))
    }

    pub fn blueprints(&self, family: i64, size: &str) -> Option<Vec<Vec<String>>> {
        self.blueprints
            .get(&family.to_string())
            .and_then(|f| f.get(size))
            .and_then(JsonValue::as_array)
            .map(|a| a.iter().map(|b| b.strings().unwrap_or_default()).collect())
    }

    pub fn family_of(&self, genre: &str) -> i64 {
        self.genre_families.get(genre).copied().unwrap_or(1)
    }

    /// FALLBACK_SETS[group]: None when absent or null.
    pub fn fallback_set(&self, group: &str) -> Option<Vec<String>> {
        self.fallback_sets.get(group).and_then(JsonValue::strings)
    }
}

/// Appends each native genre (`genres-native.json`) to the beats tables' `genres`,
/// `theories`, `instrumentSets`, `themes` and `structure.genreFamilies`, after beats' own so
/// beats' ids keep their order. A beats id is never overwritten.
fn merge_native(t: &mut JsonValue, native: &JsonValue) -> BTreeMap<String, NativeGenre> {
    fn obj<'a>(t: &'a mut JsonValue, path: &[&str]) -> Option<&'a mut JsonObject> {
        let mut cur = t;
        for k in path {
            let JsonValue::Object(o) = cur else {
                return None;
            };
            if !o.contains_key(k) {
                o.set(*k, JsonObject::new().into());
            }
            cur = o.get_mut(k)?;
        }
        match cur {
            JsonValue::Object(o) => Some(o),
            _ => None,
        }
    }
    let mut out = BTreeMap::new();
    for (id, n) in obj_iter(native.get("genres")) {
        if t.get("genres").and_then(|g| g.get(id)).is_some() {
            continue;
        }
        let part = |k: &str| n.get(k).cloned().unwrap_or(JsonValue::Null);
        let text = |k: &str, d: &str| n.get(k).and_then(JsonValue::as_str).unwrap_or(d).to_string();
        for (path, key) in [
            (&["genres"][..], "genre"),
            (&["theories"][..], "theory"),
            (&["instrumentSets"][..], "instrumentSet"),
            (&["themes"][..], "theme"),
            (&["structure", "genreFamilies"][..], "family"),
        ] {
            if let Some(o) = obj(t, path) {
                o.set(id, part(key));
            }
        }
        out.insert(
            id.to_string(),
            NativeGenre {
                like: text("like", id),
                arrange_family: text("arrangeFamily", "song"),
                stinger_family: text("stingerFamily", ""),
            },
        );
    }
    out
}
