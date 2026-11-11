//! Port of `Generator/Arrange.swift` (generator/arrange.js beyond what `compose` already
//! uses; fade / break / feel / macro curves live in `structure.rs`): `arrangeWithOpts`, the
//! "arrangement DSL" that expands a raw-`nets` share into a song, and its overlay
//! directives (fadeIn, drumBreak, feelCurve, macroCurve, counterMelody).
//!
//! The JS mutates whatever it is handed. Two facts about that matter and are modelled:
//!  - `expandArrangeVariants` clones through JSON, and the counter-melody net is built as a
//!    plain object, so after an arrange step some nets are no longer `NetBundle`
//!    instances. `simulateMusicNotes` calls `nb.resetState()` on them and the JS throws.
//!    [`ArrangeContext`] tracks which nets are in that state so this throws in exactly the
//!    same places.
//!  - A share's `nets` reach arrange as unparsed wire JSON (`transitions[].midi`), where
//!    the velocity tweak finds no `bindings` to tweak and `stampDurationSteps` only stamps
//!    bindings whose JSON carries a finite `duration`. [`ArrangeOpts::wire_form`]
//!    reproduces both.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::gen::counter_melody::{
    build_counter_melody_net, counter_seed, generate_counter_melody, js_to_fixed6, COUNTER_PPQ,
};
use crate::gen::structure::{
    build_control_bundle, default_phrases, extract_slot_index, fade_in, inject_curve,
    inject_macro_curve, is_stinger_id, section_steps, SongTemplate, TemplateSection,
    BOUNDARY_STAGGER, OBJECT_PROTO_KEYS,
};
use crate::gen::tables::OrderedFlags;
use crate::json::{floor, js_number_string, js_round, utf16_cmp, JsonObject, JsonValue};
use crate::pflow::{ControlBinding, NetBundle, OrderedMap, Project, Section};
use crate::rng::Mulberry32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrangeError {
    /// JS: `nb.resetState is not a function`: a cloned / synthesized net reached the
    /// simulator.
    NetNotParsed(String),
    /// JS: `(x).toFixed is not a function`: a non-numeric `density`.
    BadDensity,
    /// Another JS TypeError at the same point: a `size` naming an `Object.prototype`
    /// property (`blueprint.map` of undefined), a `null` entry in `fadeIn` / `sections` / a
    /// curve / `counterMelody` (property read of null).
    TypeError(String),
}

impl core::fmt::Display for ArrangeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ArrangeError::NetNotParsed(id) => write!(f, "net {id:?} is not parsed (resetState)"),
            ArrangeError::BadDensity => f.write_str("density.toFixed is not a function"),
            ArrangeError::TypeError(s) => write!(f, "TypeError: {s}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ArrangeError {}

/// `opts` of `arrangeWithOpts`.
#[derive(Clone, Debug, Default)]
pub struct ArrangeOpts {
    pub seed: f64,
    pub velocity_deltas: Option<JsonObject>,
    pub max_variants: f64,
    /// Role names; JSON values because the JS reads them as it finds them (a `null`
    /// throws).
    pub fade_in: Vec<JsonValue>,
    pub drum_break: f64,
    pub sections: Vec<JsonValue>,
    pub feel_curve: Vec<JsonValue>,
    pub macro_curve: Vec<JsonValue>,
    pub counter_melody: Vec<JsonValue>,
    pub overlay_only: bool,
    /// The project handed in is the unparsed wire form (see the module comment).
    pub wire_form: bool,
}

impl ArrangeOpts {
    pub fn new() -> Self {
        Self::default()
    }

    /// From a JS opts object (the shape fixtures and share envelopes use).
    pub fn from_json(o: &JsonValue) -> Self {
        let mut r = ArrangeOpts::new();
        let num = |k: &str| o.get(k).and_then(JsonValue::as_f64);
        let list = |k: &str| {
            o.get(k)
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default()
        };
        if let Some(s) = num("seed").filter(|s| *s != 0.0 && !s.is_nan()) {
            r.seed = s;
        }
        if let Some(d) = o.get("velocityDeltas").filter(|d| d.is_truthy()) {
            r.velocity_deltas = Some(d.as_object().cloned().unwrap_or_default());
        }
        if let Some(m) = num("maxVariants").filter(|m| !m.is_nan()) {
            r.max_variants = m;
        }
        r.fade_in = list("fadeIn");
        if let Some(d) = num("drumBreak").filter(|d| !d.is_nan()) {
            r.drum_break = d;
        }
        r.sections = list("sections");
        r.feel_curve = list("feelCurve");
        r.macro_curve = list("macroCurve");
        r.counter_melody = list("counterMelody");
        r.overlay_only = o.get("overlayOnly").is_some_and(JsonValue::is_truthy);
        r
    }
}

// ---------------------------------------------------------------------------
// Tables (arrange.js carries its own copy of the structure tables)

/// genre → family, in declaration order.
const FAMILIES: &[(&str, &str)] = &[
    ("edm", "edm"),
    ("techno", "edm"),
    ("house", "edm"),
    ("trance", "edm"),
    ("dnb", "edm"),
    ("dubstep", "edm"),
    ("speedcore", "edm"),
    ("garage", "edm"),
    ("trap", "edm"),
    ("country", "song"),
    ("blues", "song"),
    ("funk", "song"),
    ("reggae", "song"),
    ("synthwave", "song"),
    ("metal", "song"),
    ("jazz", "jazz"),
    ("bossa", "jazz"),
    ("ambient", "chill"),
    ("lofi", "chill"),
    ("wrapped", "edm"),
];
/// family → size → candidate blueprints (space-separated section names), in declaration order.
const BLUEPRINTS: &[(&str, &[(&str, &[&str])])] = &[
    (
        "edm",
        &[
            (
                "minimal",
                &["intro buildup drop outro", "intro drop breakdown drop"],
            ),
            (
                "standard",
                &[
                    "intro buildup drop breakdown buildup drop outro",
                    "intro verse buildup drop breakdown drop outro",
                    "intro buildup drop verse drop outro",
                ],
            ),
            (
                "extended",
                &[
                    "intro buildup drop breakdown verse buildup drop bridge chorus outro",
                    "intro verse buildup drop breakdown buildup drop chorus outro",
                    "intro buildup drop verse breakdown buildup drop bridge drop outro",
                    "intro verse pre-chorus drop breakdown verse buildup drop outro",
                ],
            ),
            (
                "ab",
                &[
                    "verse drop verse drop",
                    "buildup drop buildup drop",
                    "verse chorus verse chorus",
                ],
            ),
            (
                "drop",
                &[
                    "drop breakdown drop breakdown drop",
                    "buildup drop breakdown drop",
                    "drop verse drop verse drop",
                ],
            ),
            (
                "build",
                &[
                    "buildup drop buildup drop buildup drop",
                    "verse buildup drop buildup drop",
                ],
            ),
            (
                "jam",
                &[
                    "verse solo verse solo verse",
                    "verse bridge verse solo verse",
                    "drop solo drop bridge drop",
                ],
            ),
        ],
    ),
    (
        "song",
        &[
            (
                "minimal",
                &["intro verse chorus outro", "intro verse verse chorus"],
            ),
            (
                "standard",
                &[
                    "intro verse chorus verse chorus outro",
                    "intro verse chorus bridge chorus outro",
                    "intro verse pre-chorus chorus verse chorus outro",
                ],
            ),
            (
                "extended",
                &[
                    "intro verse chorus verse chorus bridge chorus outro",
                    "intro verse pre-chorus chorus verse pre-chorus chorus bridge chorus outro",
                    "intro verse chorus verse chorus breakdown verse chorus outro",
                    "intro verse verse chorus bridge verse chorus chorus outro",
                ],
            ),
            (
                "ab",
                &[
                    "verse chorus verse chorus",
                    "verse chorus bridge chorus",
                    "verse pre-chorus chorus verse chorus",
                ],
            ),
            (
                "drop",
                &[
                    "chorus verse chorus verse chorus",
                    "chorus bridge chorus bridge chorus",
                ],
            ),
            (
                "build",
                &[
                    "verse pre-chorus chorus verse pre-chorus chorus",
                    "verse buildup chorus verse buildup chorus",
                ],
            ),
            (
                "jam",
                &[
                    "verse solo verse solo verse",
                    "verse bridge solo verse solo",
                    "verse verse bridge solo verse",
                ],
            ),
        ],
    ),
    (
        "jazz",
        &[
            (
                "minimal",
                &["intro verse solo outro", "intro verse verse outro"],
            ),
            (
                "standard",
                &[
                    "intro verse solo verse outro",
                    "intro verse verse bridge verse outro",
                    "intro verse chorus solo chorus outro",
                ],
            ),
            (
                "extended",
                &[
                    "intro verse chorus solo verse chorus bridge chorus outro",
                    "intro verse solo verse bridge solo verse outro",
                    "intro verse verse solo bridge verse chorus outro",
                ],
            ),
            ("ab", &["verse solo verse solo", "verse bridge verse solo"]),
            (
                "drop",
                &[
                    "verse solo verse solo verse",
                    "solo verse bridge solo verse",
                ],
            ),
            ("build", &["verse verse solo verse solo solo"]),
            (
                "jam",
                &[
                    "verse solo solo bridge solo verse",
                    "solo bridge solo verse solo",
                    "verse solo verse bridge solo solo",
                ],
            ),
        ],
    ),
    (
        "chill",
        &[
            (
                "minimal",
                &["intro verse verse outro", "intro verse bridge outro"],
            ),
            (
                "standard",
                &[
                    "intro verse bridge verse outro",
                    "intro verse breakdown verse outro",
                    "intro verse verse bridge verse outro",
                ],
            ),
            (
                "extended",
                &[
                    "intro verse chorus breakdown verse chorus outro",
                    "intro verse bridge verse breakdown verse bridge outro",
                    "intro verse verse bridge breakdown verse chorus outro",
                ],
            ),
            (
                "ab",
                &["verse bridge verse bridge", "verse breakdown verse bridge"],
            ),
            (
                "drop",
                &[
                    "verse breakdown verse breakdown verse",
                    "verse bridge breakdown verse bridge",
                ],
            ),
            (
                "build",
                &[
                    "verse verse bridge verse chorus",
                    "breakdown verse bridge verse chorus",
                ],
            ),
            (
                "jam",
                &[
                    "verse bridge verse breakdown verse",
                    "verse verse bridge breakdown verse bridge",
                ],
            ),
        ],
    ),
];
const ARCHETYPES: &[(&str, &str)] = &[
    ("intro", "kick hihat bass"),
    ("verse", "kick snare hihat bass"),
    ("pre-chorus", "kick snare hihat bass arp"),
    ("chorus", "kick snare hihat bass melody harmony arp clap"),
    ("drop", "kick snare hihat bass melody harmony arp clap"),
    ("buildup", "kick snare hihat bass arp"),
    ("breakdown", "hihat bass melody"),
    ("bridge", "bass melody harmony"),
    ("solo", "kick hihat bass melody"),
    ("outro", "kick hihat melody"),
];
const ARCHETYPES_BY_FAMILY: &[(&str, &[(&str, &str)])] = &[
    (
        "edm",
        &[("intro", "kick hihat"), ("breakdown", "hihat bass")],
    ),
    (
        "song",
        &[
            ("pre-chorus", "kick snare bass arp"),
            ("outro", "kick hihat bass melody"),
        ],
    ),
    (
        "jazz",
        &[
            ("intro", "hihat bass"),
            ("verse", "hihat bass melody"),
            ("chorus", "hihat bass melody harmony"),
            ("solo", "hihat bass melody"),
        ],
    ),
    (
        "chill",
        &[
            ("intro", "bass"),
            ("verse", "hihat bass melody"),
            ("chorus", "hihat bass melody harmony arp"),
            ("bridge", "bass melody"),
            ("breakdown", "melody"),
            ("outro", "melody"),
        ],
    ),
];

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(' ').filter(|w| !w.is_empty())
}

/// The arrange.js tables, for the fixture comparison and for lookups.
pub struct ArrangeTables;

impl ArrangeTables {
    pub(crate) fn family(genre: &str) -> Option<&'static str> {
        FAMILIES.iter().find(|f| f.0 == genre).map(|f| f.1).or_else(|| {
            // a native genre (genres-native.json) names one of the families above
            let fam = &crate::gen::tables::BeatsTables::shared()
                .native
                .get(genre)?
                .arrange_family;
            FAMILIES.iter().map(|f| f.1).find(|f| f == fam)
        })
    }

    /// `FAMILIES[genre] || 'song'` as the two lookups that use it see it: the blueprint
    /// family and the family whose archetype overrides apply. A genre naming an
    /// `Object.prototype` property finds a function: no blueprints under its name (song's
    /// are used) and no archetype overrides.
    pub fn families_for(genre: &str) -> (&'static str, Option<&'static str>) {
        if let Some(f) = Self::family(genre) {
            return (f, Some(f));
        }
        if OBJECT_PROTO_KEYS.contains(&genre) {
            ("song", None)
        } else {
            ("song", Some("song"))
        }
    }

    /// `archetypeFor(family, name)`
    pub fn archetype(family: Option<&str>, name: &str) -> Vec<&'static str> {
        if let Some(fam) = family {
            if let Some(rows) = ARCHETYPES_BY_FAMILY.iter().find(|f| f.0 == fam) {
                if let Some(a) = rows.1.iter().find(|a| a.0 == name) {
                    return words(a.1).collect();
                }
            }
        }
        ARCHETYPES
            .iter()
            .find(|a| a.0 == name)
            .map(|a| words(a.1).collect())
            .unwrap_or_default()
    }

    fn find(family: &str, size: &str) -> Option<Vec<Vec<&'static str>>> {
        BLUEPRINTS
            .iter()
            .find(|f| f.0 == family)?
            .1
            .iter()
            .find(|s| s.0 == size)
            .map(|s| s.1.iter().map(|row| words(row).collect()).collect())
    }

    fn candidates(family: &str, size: &str) -> Vec<Vec<&'static str>> {
        Self::find(family, size).unwrap_or_else(|| Self::find("song", "standard").unwrap())
    }

    /// The tables as JSON in declaration order, for the fixture comparison.
    pub fn json() -> JsonValue {
        fn flag_obj(names: &str) -> JsonValue {
            let mut o = JsonObject::new();
            for n in words(names) {
                o.set(n, JsonValue::Bool(true));
            }
            o.into()
        }
        let mut fam = JsonObject::new();
        for (g, f) in FAMILIES {
            fam.set(*g, (*f).into());
        }
        let mut bps = JsonObject::new();
        for (f, sizes) in BLUEPRINTS {
            let mut so = JsonObject::new();
            for (size, rows) in sizes.iter() {
                so.set(
                    *size,
                    JsonValue::Array(
                        rows.iter()
                            .map(|r| JsonValue::Array(words(r).map(JsonValue::str).collect()))
                            .collect(),
                    ),
                );
            }
            bps.set(*f, so.into());
        }
        let mut arch = JsonObject::new();
        for (n, a) in ARCHETYPES {
            arch.set(*n, flag_obj(a));
        }
        let mut by_fam = JsonObject::new();
        for (f, rows) in ARCHETYPES_BY_FAMILY {
            let mut o = JsonObject::new();
            for (n, a) in rows.iter() {
                o.set(*n, flag_obj(a));
            }
            by_fam.set(*f, o.into());
        }
        JsonObject::from_pairs([
            ("families", JsonValue::from(fam)),
            ("blueprints", bps.into()),
            ("archetypes", arch.into()),
            ("archetypesByFamily", by_fam.into()),
        ])
        .into()
    }
}

// ---------------------------------------------------------------------------
// Context

/// What the JS knows implicitly about its objects while an arrange runs.
#[derive(Default)]
pub struct ArrangeContext {
    /// Nets that are plain objects in JS (no NetBundle methods).
    pub plain: BTreeSet<String>,
}

// ---------------------------------------------------------------------------
// Predicates

fn is_stinger_net(id: &str, nb: Option<&NetBundle>) -> bool {
    if nb.is_some_and(|n| n.track.group.as_deref() == Some("stinger")) {
        return true;
    }
    is_stinger_id(id)
}

fn is_drum_role_name(id: &str) -> bool {
    [
        "kick", "snare", "hat", "hihat", "clap", "cymbal", "tom", "drum", "perc",
    ]
    .iter()
    .any(|p| id.starts_with(p))
}

fn sorted_music_net_ids(proj: &Project) -> Vec<String> {
    let mut ids: Vec<String> = proj
        .nets
        .iter()
        .filter(|(_, n)| n.role != "control")
        .map(|(k, _)| k.to_string())
        .collect();
    ids.sort_by(|a, b| utf16_cmp(a, b));
    ids
}

// ---------------------------------------------------------------------------
// Templates

/// arrange.js `sectionWithPhrases` (unlike structure.js, `snare` keeps a phrase list).
fn arrange_section(name: &str, steps: i64, active: OrderedFlags) -> TemplateSection {
    let mut phrases = OrderedMap::new();
    let pattern = default_phrases(name);
    for role in active.keys() {
        if role == "snare" {
            phrases.insert(role.as_str(), alloc::vec![String::from("A"); pattern.len()]);
        } else {
            phrases.insert(role.as_str(), pattern.clone());
        }
    }
    TemplateSection {
        name: name.into(),
        steps,
        active,
        phrases,
    }
}

fn generate_arrange_structure(
    genre: &str,
    size: &str,
    roles: &[String],
    rng: &mut Mulberry32,
) -> Result<SongTemplate, ArrangeError> {
    let (blueprint_family, family) = ArrangeTables::families_for(genre);
    // `blueprintsBySize[size]` is an inherited function for these: `candidates[k]` is
    // undefined and `blueprint.map` throws.
    if OBJECT_PROTO_KEYS.contains(&size) {
        return Err(ArrangeError::TypeError(
            "blueprint.map of undefined".to_string(),
        ));
    }
    let candidates = ArrangeTables::candidates(blueprint_family, size);
    let blueprint = &candidates[rng.next_int(candidates.len())];
    let role_set: BTreeSet<&str> = roles.iter().map(String::as_str).collect();
    let sections = blueprint
        .iter()
        .map(|name| {
            let mut active = OrderedFlags::new();
            let archetype = ArrangeTables::archetype(family, name);
            for k in &archetype {
                if role_set.contains(k) {
                    active.set(k);
                }
            }
            for r in roles {
                if active.contains(r) {
                    continue;
                }
                if archetype.contains(&r.as_str()) {
                    continue;
                }
                if is_stinger_id(r) {
                    continue;
                }
                if *name != "intro" && *name != "outro" {
                    active.set(r);
                }
            }
            arrange_section(name, section_steps(name, size), active)
        })
        .collect();
    Ok(SongTemplate {
        name: size.into(),
        sections,
        slot_map: OrderedMap::new(),
    })
}

fn author_sections_to_template(
    authored: &[JsonValue],
    roles: &[String],
) -> Result<SongTemplate, ArrangeError> {
    let mut sections = Vec::new();
    for a in authored {
        if *a == JsonValue::Null {
            return Err(ArrangeError::TypeError("a.active of null".to_string()));
        }
        let mut active_set: BTreeSet<&str> = BTreeSet::new();
        if let Some(arr) = a.get("active").and_then(JsonValue::as_array) {
            for v in arr {
                if let Some(s) = v.as_str() {
                    active_set.insert(s);
                }
            }
        }
        let mut active = OrderedFlags::new();
        for r in roles {
            if active_set.contains(r.as_str()) {
                active.set(r);
            }
        }
        let steps = a
            .get("steps")
            .and_then(JsonValue::as_f64)
            .filter(|d| *d > 0.0)
            .map(|d| d as i64)
            .unwrap_or(128);
        sections.push(arrange_section(
            a.get("name").and_then(JsonValue::as_str).unwrap_or(""),
            steps,
            active,
        ));
    }
    Ok(SongTemplate {
        name: "authored".into(),
        sections,
        slot_map: OrderedMap::new(),
    })
}

fn project_structure_to_template(proj: &Project) -> Option<SongTemplate> {
    if proj.structure.is_empty() {
        return None;
    }
    let sections = proj
        .structure
        .iter()
        .map(|s| TemplateSection {
            name: s.name.clone(),
            steps: s.steps,
            active: OrderedFlags::new(),
            phrases: s.phrases.clone(),
        })
        .collect();
    Some(SongTemplate {
        name: "overlay".into(),
        sections,
        slot_map: OrderedMap::new(),
    })
}

fn cap_variant_letters(tmpl: &mut SongTemplate, max: f64) {
    if max <= 0.0 {
        return;
    }
    // `String.fromCharCode(65 + i)` for i < max; phrase letters are A–C, so the first 26
    // decide.
    let mut allowed: BTreeSet<String> = BTreeSet::new();
    let mut i = 0;
    while (i as f64) < max && i < 26 {
        allowed.insert(((65 + i) as u8 as char).to_string());
        i += 1;
    }
    for sec in tmpl.sections.iter_mut() {
        let roles: Vec<String> = sec.phrases.keys().to_vec();
        for role in roles {
            let letters = sec.phrases.get_mut(&role).unwrap();
            for l in letters.iter_mut() {
                if !allowed.contains(l.as_str()) {
                    *l = "A".into();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Variant cloning

/// `tweakVelocity` on a cloned bundle; the clone is re-parsed later (`getInt`:
/// Math.round), so a fractional delta lands rounded.
fn tweak_velocity(nb: &mut NetBundle, delta: f64) {
    let ids: Vec<String> = nb.bindings.keys().to_vec();
    for id in ids {
        let mut b = *nb.bindings.get(&id).unwrap();
        let mut v = b.velocity as f64 + delta;
        if v < 1.0 {
            v = 1.0;
        }
        if v > 127.0 {
            v = 127.0;
        }
        b.velocity = js_round(v) as i64;
        nb.bindings.insert(id, b);
    }
}

fn expand_arrange_variants(
    proj: &mut Project,
    tmpl: &mut SongTemplate,
    roles: &[String],
    deltas: &BTreeMap<String, f64>,
    tweak: bool,
    ctx: &mut ArrangeContext,
) {
    tmpl.slot_map = OrderedMap::new();
    let mut roles_in_phrases: BTreeSet<String> = BTreeSet::new();
    for sec in &tmpl.sections {
        for r in sec.phrases.keys() {
            roles_in_phrases.insert(r.clone());
        }
    }

    for role in roles {
        if !roles_in_phrases.contains(role) {
            continue;
        }
        let Some(base_bundle) = proj.nets.get(role).cloned() else {
            continue;
        };
        if is_stinger_net(role, Some(&base_bundle)) {
            continue;
        }

        let mut slot_map: Vec<Vec<i64>> = alloc::vec![Vec::new(); tmpl.sections.len()];
        let mut slot_idx = 0i64;
        let mut letter_slots: BTreeMap<String, i64> = BTreeMap::new();
        for (si, sec) in tmpl.sections.iter().enumerate() {
            let phrases = sec
                .phrases
                .get(role)
                .cloned()
                .unwrap_or_else(|| alloc::vec![String::from("A")]);
            let mut section_slots = alloc::vec![-1i64; phrases.len()];
            if sec.active.contains(role) {
                for (pi, letter) in phrases.iter().enumerate() {
                    if let Some(&s) = letter_slots.get(letter) {
                        section_slots[pi] = s;
                    } else {
                        letter_slots.insert(letter.clone(), slot_idx);
                        section_slots[pi] = slot_idx;
                        slot_idx += 1;
                    }
                }
            }
            slot_map[si] = section_slots;
        }
        tmpl.slot_map.insert(role.as_str(), slot_map);

        if slot_idx <= 1 {
            let nb = proj.nets.get_mut(role).unwrap();
            nb.riff_group = role.clone();
            nb.riff_variant = "A".into();
            continue;
        }

        let mut letters: Vec<&String> = letter_slots.keys().collect();
        letters.sort_by(|a, b| utf16_cmp(a, b));
        for letter in letters {
            let idx = letter_slots[letter];
            let slot_net_id = format!("{role}-{idx}");
            let mut clone = base_bundle.clone();
            clone.riff_group = role.clone();
            clone.riff_variant = letter.clone();
            if tweak {
                if let Some(&d) = deltas.get(letter) {
                    if d != 0.0 && !d.is_nan() {
                        tweak_velocity(&mut clone, d);
                    }
                }
            }
            proj.nets.insert(slot_net_id.as_str(), clone);
            ctx.plain.insert(slot_net_id);
        }
        proj.nets.remove(role);
        ctx.plain.remove(role);
    }
}

// ---------------------------------------------------------------------------
// Section control nets (the ring-based arrange.js flavour)

fn arrange_role_control_net(
    role: &str,
    slot_map: &[Vec<i64>],
    tmpl: &SongTemplate,
    total_steps: i64,
    stagger: i64,
) -> NetBundle {
    let mut ctrl_at: BTreeMap<i64, ControlBinding> = BTreeMap::new();
    let mut prev_slot = if !slot_map.is_empty() && !slot_map[0].is_empty() {
        slot_map[0][0]
    } else {
        -1
    };
    let mut pos = 0i64;
    for (si, sec) in tmpl.sections.iter().enumerate() {
        let phrases = sec
            .phrases
            .get(role)
            .cloned()
            .unwrap_or_else(|| alloc::vec![String::from("A")]);
        let phrase_len = if phrases.is_empty() {
            0
        } else {
            sec.steps / phrases.len() as i64
        };
        for pi in 0..phrases.len() {
            let phrase_start = pos + pi as i64 * phrase_len;
            if phrase_start == 0 {
                continue;
            }
            let mut cur_slot = -1;
            if si < slot_map.len() && pi < slot_map[si].len() {
                cur_slot = slot_map[si][pi];
            }
            if cur_slot != prev_slot {
                let mut bind_pos = phrase_start;
                if cur_slot >= 0 && stagger > 0 {
                    let mut max_off = total_steps - 1 - phrase_start;
                    if max_off > phrase_len - 1 {
                        max_off = phrase_len - 1;
                    }
                    let mut off = stagger;
                    if off > max_off {
                        off = max_off;
                    }
                    if off < 0 {
                        off = 0;
                    }
                    bind_pos = phrase_start + off;
                }
                if cur_slot >= 0 {
                    ctrl_at.insert(
                        bind_pos,
                        ControlBinding::new("activate-slot", &format!("{role}-{cur_slot}")),
                    );
                } else {
                    ctrl_at.insert(
                        bind_pos,
                        ControlBinding::new("mute-track", &format!("{role}-{prev_slot}")),
                    );
                }
                prev_slot = cur_slot;
            }
        }
        pos += sec.steps;
    }
    build_control_bundle(total_steps, &ctrl_at, true, "p", "t")
}

fn arrange_linear_control_net(
    target: &str,
    tmpl: &SongTemplate,
    total_steps: i64,
    stagger: i64,
) -> NetBundle {
    let mut ctrl_at: BTreeMap<i64, ControlBinding> = BTreeMap::new();
    let mut was_active = tmpl
        .sections
        .first()
        .is_some_and(|s| s.active.contains(target));
    let mut pos = 0i64;
    for si in 1..tmpl.sections.len().max(1) {
        pos += tmpl.sections[si - 1].steps;
        let is_active = tmpl.sections[si].active.contains(target);
        if is_active != was_active {
            let mut bind_pos = pos;
            if is_active && stagger > 0 {
                let max_off = tmpl.sections[si].steps - 1;
                let mut off = stagger;
                if off > max_off {
                    off = max_off;
                }
                if off < 0 {
                    off = 0;
                }
                bind_pos = pos + off;
            }
            ctrl_at.insert(
                bind_pos,
                ControlBinding::new(
                    if is_active {
                        "unmute-track"
                    } else {
                        "mute-track"
                    },
                    target,
                ),
            );
            was_active = is_active;
        }
    }
    build_control_bundle(total_steps, &ctrl_at, true, "p", "t")
}

fn arrange_song_structure(
    proj: &mut Project,
    tmpl: &SongTemplate,
    music_nets: &[String],
) -> Vec<String> {
    let total_steps: i64 = tmpl.sections.iter().map(|s| s.steps).sum();
    let mut initial_mutes: Vec<String> = Vec::new();
    let mut slot_roles: BTreeSet<String> = BTreeSet::new();
    for id in music_nets {
        if let Some(n) = proj.nets.get(id) {
            if !n.riff_group.is_empty() {
                slot_roles.insert(n.riff_group.clone());
            }
        }
    }
    let mut stagger_keys: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for id in music_nets {
        let Some(nb) = proj.nets.get(id) else {
            continue;
        };
        let mut key = id.clone();
        if !nb.riff_group.is_empty() {
            key = nb.riff_group.clone();
        } else if slot_roles.contains(id) {
            continue;
        }
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        stagger_keys.push(key);
    }
    stagger_keys.sort_by(|a, b| utf16_cmp(a, b));
    let mut stagger_of: BTreeMap<String, i64> = BTreeMap::new();
    for (i, k) in stagger_keys.iter().enumerate() {
        stagger_of.insert(k.clone(), i as i64 % BOUNDARY_STAGGER);
    }

    let mut processed: BTreeSet<String> = BTreeSet::new();
    for id in music_nets {
        let Some(nb) = proj.nets.get(id) else {
            continue;
        };
        let riff_group = nb.riff_group.clone();
        if !riff_group.is_empty() {
            let role = riff_group;
            if processed.contains(&role) {
                continue;
            }
            processed.insert(role.clone());
            let slot_map = tmpl.slot_map.get(&role).cloned().unwrap_or_default();
            let net = arrange_role_control_net(
                &role,
                &slot_map,
                tmpl,
                total_steps,
                stagger_of.get(&role).copied().unwrap_or(0),
            );
            proj.nets.insert(format!("struct-{role}"), net);
            let first_slot = if !slot_map.is_empty() && !slot_map[0].is_empty() {
                slot_map[0][0]
            } else {
                -1
            };
            for n_id in music_nets {
                if proj.nets.get(n_id).is_some_and(|n2| n2.riff_group == role) {
                    let slot_idx = extract_slot_index(n_id, &role);
                    if slot_idx != first_slot || first_slot < 0 {
                        initial_mutes.push(n_id.clone());
                    }
                }
            }
        } else if !slot_roles.contains(id) {
            if is_stinger_net(id, Some(nb)) {
                initial_mutes.push(id.clone());
                continue;
            }
            let net = arrange_linear_control_net(
                id,
                tmpl,
                total_steps,
                stagger_of.get(id).copied().unwrap_or(0),
            );
            proj.nets.insert(format!("struct-{id}"), net);
            if !tmpl.sections.first().is_some_and(|s| s.active.contains(id)) {
                initial_mutes.push(id.clone());
            }
        }
    }
    let mut stop = BTreeMap::new();
    stop.insert(total_steps - 1, ControlBinding::new("stop-transport", ""));
    proj.nets.insert(
        "struct-stop",
        build_control_bundle(total_steps, &stop, true, "p", "t"),
    );

    proj.structure = tmpl
        .sections
        .iter()
        .map(|s| Section {
            name: s.name.clone(),
            steps: s.steps,
            phrases: s.phrases.clone(),
        })
        .collect();
    initial_mutes
}

// ---------------------------------------------------------------------------
// Overlays

/// arrange.js `drumBreak` with the double arithmetic the overlay needs: `drumBreak: 0.3`
/// bars is 4.8 steps in JS, and a fractional position simply never matches a transition
/// index.
fn drum_break_overlay(proj: &mut Project, targets: &[String], steps: i64, break_in: f64) {
    let mut cycle = steps as f64;
    if cycle < 16.0 {
        cycle = 64.0;
    }
    let mut break_len = break_in;
    if break_len < 4.0 {
        break_len = 8.0;
    }
    if break_len >= cycle {
        break_len = floor(cycle / 4.0);
    }
    let mute_pos = floor(cycle / 2.0);
    let mut sorted: Vec<&String> = targets.iter().collect();
    sorted.sort_by(|a, b| utf16_cmp(a, b));
    let unmute_base = (mute_pos + break_len) % cycle;
    let headroom = (((mute_pos - unmute_base - 1.0) % cycle) + cycle) % cycle;
    let mut max_stagger = BOUNDARY_STAGGER as f64;
    if max_stagger > headroom {
        max_stagger = headroom;
    }
    if max_stagger < 1.0 {
        max_stagger = 1.0;
    }
    for (i, target) in sorted.into_iter().enumerate() {
        if !proj.nets.contains_key(target) {
            continue;
        }
        let unmute_pos = (mute_pos + break_len + (i as f64 % max_stagger)) % cycle;
        let mut ctrl_at: BTreeMap<i64, ControlBinding> = BTreeMap::new();
        ctrl_at.insert(mute_pos as i64, ControlBinding::new("mute-track", target));
        if unmute_pos == floor(unmute_pos) {
            ctrl_at.insert(
                unmute_pos as i64,
                ControlBinding::new("unmute-track", target),
            );
        }
        proj.nets.insert(
            format!("break-{target}"),
            build_control_bundle(cycle as i64, &ctrl_at, false, "p", "t"),
        );
    }
}

/// `injectFeelCurve` for the envelope's `[{section, x, y}]` entries (x / y pass through
/// as-is).
fn inject_feel_curve_entries(proj: &mut Project, tmpl: &SongTemplate, curve: &[JsonValue]) {
    inject_curve(proj, tmpl, curve, "feel-curve", ("fp", "ft"), |e| {
        let mut c = ControlBinding::new("set-feel", "");
        let mut mp = JsonObject::new();
        if let Some(x) = e.get("x") {
            mp.set("x", x.clone());
        }
        if let Some(y) = e.get("y") {
            mp.set("y", y.clone());
        }
        c.macro_params = Some(mp.into());
        Some(c)
    });
}

/// A `null` curve / counter-melody entry: `entry.section` throws in JS once it is reached.
fn reject_null_entries(entries: &[JsonValue], what: &str) -> Result<(), ArrangeError> {
    if entries.contains(&JsonValue::Null) {
        return Err(ArrangeError::TypeError(format!("{what} entry is null")));
    }
    Ok(())
}

/// JS `String(v)` for the values a counter-melody entry can carry (used only for seeding).
fn js_string(v: Option<&JsonValue>) -> String {
    let Some(v) = v else {
        return "undefined".into();
    };
    match v {
        JsonValue::Null => "null".into(),
        JsonValue::Bool(b) => if *b { "true" } else { "false" }.into(),
        JsonValue::Number(d) => js_number_string(*d),
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(a) => a
            .iter()
            .map(|x| match x {
                JsonValue::Null => String::new(),
                x => js_string(Some(x)),
            })
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(_) => "[object Object]".into(),
    }
}

/// `String(entry.field || '')`
fn or_empty(v: Option<&JsonValue>) -> String {
    match v {
        Some(v) if v.is_truthy() => js_string(Some(v)),
        _ => String::new(),
    }
}

fn inject_counter_melody(
    proj: &mut Project,
    tmpl: &SongTemplate,
    entries: &[JsonValue],
    base_seed: f64,
    ctx: &mut ArrangeContext,
) -> Result<(), ArrangeError> {
    let mut starts: Vec<(i64, &str)> = Vec::new();
    let mut total_steps = 0i64;
    for sec in &tmpl.sections {
        starts.push((total_steps, sec.name.as_str()));
        total_steps += sec.steps;
    }
    if total_steps == 0 {
        return Ok(());
    }
    let tempo = if proj.tempo > 0.0 { proj.tempo } else { 120.0 };
    let ms_per_tick = 60000.0 / (tempo * COUNTER_PPQ as f64);

    for (idx, entry) in entries.iter().enumerate() {
        if *entry == JsonValue::Null {
            return Err(ArrangeError::TypeError("entry.section of null".to_string()));
        }
        let Some(section) = entry.get("section").and_then(JsonValue::as_str) else {
            continue;
        };
        if !starts.iter().any(|s| s.1 == section) {
            continue;
        }

        let mut density_value = 0.0;
        if let Some(d) = entry.get("density").filter(|d| d.is_truthy()) {
            density_value = d.as_f64().ok_or(ArrangeError::BadDensity)?;
        }
        let seed = counter_seed(&[
            format!("counter-melody:{}:{}", js_number_string(base_seed), idx),
            or_empty(entry.get("section")),
            or_empty(entry.get("mode")),
            js_to_fixed6(density_value),
            or_empty(entry.get("register")),
            or_empty(entry.get("of")),
            or_empty(entry.get("instrument")),
        ]);

        let mut scope: Option<String> = None;
        if let Some(of) = entry.get("of").filter(|o| o.is_truthy()) {
            scope = Some(
                of.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| "\u{0}not-a-string".to_string()),
            );
        }
        let non_empty = |k: &str| {
            entry
                .get(k)
                .and_then(JsonValue::as_str)
                .filter(|s| !s.is_empty())
        };
        let notes = generate_counter_melody(
            proj,
            non_empty("mode"),
            density_value,
            non_empty("register"),
            seed,
            total_steps,
            scope.as_deref(),
            &ctx.plain,
        )?;
        let Some(notes) = notes.filter(|n| !n.is_empty()) else {
            continue;
        };

        // `entry.instrument || counterMelodyDefaultInstrument(entry.register)`; a truthy
        // non-string instrument parses back to '' (getString).
        let instrument: String = match entry.get("instrument").filter(|i| i.is_truthy()) {
            Some(ins) => ins.as_str().unwrap_or("").to_string(),
            None => {
                if entry.get("register").and_then(JsonValue::as_str) == Some("below") {
                    "sub-bass".into()
                } else {
                    "electric-piano".into()
                }
            }
        };

        // nextFreeCounterChannel
        let mut max_ch = 0;
        for (_, nb) in proj.nets.iter() {
            if nb.role != "control" && nb.track.channel > max_ch {
                max_ch = nb.track.channel;
            }
        }
        let mut channel = max_ch + 1;
        if channel == 16 {
            channel = 17;
        }

        let Some(nb) =
            build_counter_melody_net(&notes, total_steps, channel, &instrument, ms_per_tick)
        else {
            continue;
        };

        let net_id = format!("counter-melody-{idx}");
        proj.nets.insert(net_id.as_str(), nb);
        ctx.plain.insert(net_id.clone());
        proj.initial_mutes.push(net_id.clone());

        let mut ctrl_at: BTreeMap<i64, ControlBinding> = BTreeMap::new();
        for s in &starts {
            ctrl_at.insert(
                s.0,
                ControlBinding::new(
                    if s.1 == section {
                        "unmute-track"
                    } else {
                        "mute-track"
                    },
                    &net_id,
                ),
            );
        }
        proj.nets.insert(
            format!("gate-counter-melody-{idx}"),
            build_control_bundle(
                total_steps,
                &ctrl_at,
                false,
                &format!("gp{idx}_"),
                &format!("gt{idx}_"),
            ),
        );
    }
    Ok(())
}

fn apply_arrange_overlays(
    proj: &mut Project,
    tmpl: &SongTemplate,
    all_nets: &[String],
    opts: &ArrangeOpts,
    ctx: &mut ArrangeContext,
) -> Result<(), ArrangeError> {
    if !opts.fade_in.is_empty() {
        let mut intro_steps = 128;
        if let Some(first) = tmpl.sections.first() {
            intro_steps = first.steps;
        }
        let mut variant_targets: Vec<String> = Vec::new();
        for r in &opts.fade_in {
            // `id === role || (id.length > role.length && …)`: a null role throws on
            // `.length`; other non-strings never match.
            if *r == JsonValue::Null && !all_nets.is_empty() {
                return Err(ArrangeError::TypeError("role.length of null".to_string()));
            }
            let Some(role) = r.as_str() else { continue };
            for id in all_nets {
                if id == role || id.starts_with(&format!("{role}-")) {
                    variant_targets.push(id.clone());
                }
            }
        }
        if !variant_targets.is_empty() {
            let added = fade_in(proj, &variant_targets, intro_steps);
            proj.initial_mutes.extend(added);
        }
    }

    if opts.drum_break > 0.0 {
        let mut drum_targets: Vec<String> = Vec::new();
        for id in all_nets {
            let Some(nb) = proj.nets.get(id) else {
                continue;
            };
            if is_drum_role_name(id) || is_stinger_net(id, Some(nb)) {
                continue;
            }
            drum_targets.push(id.clone());
        }
        let total_steps: i64 = tmpl.sections.iter().map(|s| s.steps).sum();
        if total_steps > 0 {
            drum_break_overlay(proj, &drum_targets, total_steps, opts.drum_break * 16.0);
        }
    }

    let total: i64 = tmpl.sections.iter().map(|s| s.steps).sum();
    if !opts.feel_curve.is_empty() {
        if total != 0 {
            reject_null_entries(&opts.feel_curve, "feelCurve")?;
        }
        inject_feel_curve_entries(proj, tmpl, &opts.feel_curve);
    }
    if !opts.macro_curve.is_empty() {
        if total != 0 {
            reject_null_entries(&opts.macro_curve, "macroCurve")?;
        }
        inject_macro_curve(proj, tmpl, &opts.macro_curve);
    }
    if !opts.counter_melody.is_empty() {
        inject_counter_melody(proj, tmpl, &opts.counter_melody, opts.seed, ctx)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point

/// The `size` a share's `structure` value arranges with. JS looks blueprints up by
/// `String(value)` and section lengths by `=== 'minimal'` / `'extended'`; a non-string
/// value is treated here as an unknown size (song standard blueprints, standard lengths),
/// which is what JS does for every non-string except arrays whose string form names a size.
pub fn arrange_size(structure: &JsonValue) -> String {
    structure.as_str().unwrap_or("\u{0}").to_string()
}

/// `arrangeWithOpts(proj, genre, size, opts)`. Mutates `proj`; on a throw the project is
/// left as far along as the JS got (and, as in the JS `finally`, durations are stamped).
pub fn arrange_with_opts(
    proj: &mut Project,
    genre: &str,
    size: &str,
    opts: &ArrangeOpts,
) -> Result<(), ArrangeError> {
    let r = arrange_inner(proj, genre, size, opts);
    proj.stamp_duration_steps();
    r
}

fn arrange_inner(
    proj: &mut Project,
    genre: &str,
    size: &str,
    opts: &ArrangeOpts,
) -> Result<(), ArrangeError> {
    let mut ctx = ArrangeContext::default();
    if opts.wire_form {
        ctx.plain = proj.nets.keys().iter().cloned().collect();
    }

    if opts.overlay_only {
        let Some(tmpl) = project_structure_to_template(proj) else {
            return Ok(());
        };
        let ids = sorted_music_net_ids(proj);
        return apply_arrange_overlays(proj, &tmpl, &ids, opts, &mut ctx);
    }

    let mut rng = Mulberry32::new(opts.seed);
    let music_roles = sorted_music_net_ids(proj);

    let mut tmpl = if !opts.sections.is_empty() {
        author_sections_to_template(&opts.sections, &music_roles)?
    } else {
        generate_arrange_structure(
            if genre.is_empty() { "wrapped" } else { genre },
            if size.is_empty() { "standard" } else { size },
            &music_roles,
            &mut rng,
        )?
    };
    if opts.max_variants > 0.0 {
        cap_variant_letters(&mut tmpl, opts.max_variants);
    }

    let mut deltas: BTreeMap<String, f64> = BTreeMap::new();
    if let Some(vd) = &opts.velocity_deltas {
        for (k, v) in vd.iter() {
            if let Some(d) = v.as_f64() {
                deltas.insert(k.to_string(), d);
            }
        }
    } else {
        for (k, v) in [("A", 0.0), ("B", 15.0), ("C", -15.0), ("D", -10.0)] {
            deltas.insert(k.to_string(), v);
        }
    }
    expand_arrange_variants(
        proj,
        &mut tmpl,
        &music_roles,
        &deltas,
        !opts.wire_form,
        &mut ctx,
    );

    let all_nets = sorted_music_net_ids(proj);
    let initial_mutes = arrange_song_structure(proj, &tmpl, &all_nets);
    let mut merged = OrderedFlags::from_names(&proj.initial_mutes);
    for id in &initial_mutes {
        merged.set(id);
    }
    proj.initial_mutes = merged.keys().to_vec();

    apply_arrange_overlays(proj, &tmpl, &all_nets, opts, &mut ctx)
}
