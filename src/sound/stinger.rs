//! Curated stingers: what a net deck's Hit 1–4 fire (port of `Audio/StingerCuration.swift`,
//! over `data/stingers.json`).
//!
//! The four Hits are a pure function of the configuration — the track's genre (family), its
//! sound palette, its seed, its key and tempo, and what is installed here — seeded as
//! `SoundPalette::pick` seeds a role, so the same configuration always gets the same hits.
//! Nothing here touches beats' composition: the assignments live on the played project and
//! a one-shot's sound in `TrackSound.curated`, so `compose()`, the share and its CID stay
//! beats'.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use super::{apply_sound, PaletteSet, SoundNet, Sounds};
use crate::gen::core::scale_intervals;
use crate::gen::tables::BeatsTables;
use crate::fx::MasterMacro;
use crate::json::{JsonObject, JsonValue};
use crate::voices::{stable_hash, swift_double};
use crate::wave::NetMacro;

pub const STINGERS_JSON: &str = include_str!("../../data/stingers.json");

/// The stinger nets' note (`addStingerTracks`: a euclidean ring on 60).
pub const STINGER_NOTE: i64 = 60;
pub const SLOT_COUNT: usize = 4;

/// An installed Apple Loop (`StockLoop`): the one `crate::layers` defines, which the layer
/// resolver reads too.
pub use crate::layers::StockLoop;

/// `localizedStandardCompare`, approximately: case-insensitive, digit runs by value.
fn natural_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: String = a[si..i]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .into();
            let nb: String = b[sj..j]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .into();
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            let (x, y) = (
                a[i].to_lowercase().next().unwrap_or(a[i]),
                b[j].to_lowercase().next().unwrap_or(b[j]),
            );
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Loops matching `category` ("" = any) and every word of `query` (in name, pack, genre,
/// subcategory), nearest in tempo to `bpm` first (half and double time count as near), then
/// by name (`[StockLoop].matching`).
pub fn matching<'a>(
    loops: &'a [StockLoop],
    category: &str,
    query: &str,
    near: Option<f64>,
) -> Vec<&'a StockLoop> {
    let words: Vec<String> = query
        .to_lowercase()
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    let mut hits: Vec<&StockLoop> = loops
        .iter()
        .filter(|l| {
            (category.is_empty() || l.category == category)
                && words.iter().all(|w| {
                    [&l.name, &l.pack, &l.genre, &l.subcategory]
                        .iter()
                        .any(|f| f.to_lowercase().contains(w.as_str()))
                })
        })
        .collect();
    let Some(bpm) = near.filter(|b| *b > 0.0) else {
        return hits;
    };
    let dist = |l: &StockLoop| {
        [0.5, 1.0, 2.0]
            .iter()
            .map(|m| (l.bpm() * m - bpm).abs())
            .fold(f64::INFINITY, f64::min)
    };
    hits.sort_by(|x, y| {
        let (a, b) = (dist(x), dist(y));
        if a != b {
            a.partial_cmp(&b).unwrap_or(core::cmp::Ordering::Equal)
        } else {
            natural_cmp(&x.name, &y.name)
        }
    });
    hits
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// A sound on the Hit's stinger net (`option`, SoundPalette grammar).
    OneShot,
    /// A net or master macro by its beats id.
    Macro,
    /// A clip launched on the loop row.
    Loop,
}

impl CandidateKind {
    pub const ALL: [CandidateKind; 3] = [
        CandidateKind::OneShot,
        CandidateKind::Macro,
        CandidateKind::Loop,
    ];
    pub fn from_name(s: &str) -> Option<CandidateKind> {
        match s {
            "oneShot" => Some(CandidateKind::OneShot),
            "macro" => Some(CandidateKind::Macro),
            "loop" => Some(CandidateKind::Loop),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            CandidateKind::OneShot => "oneShot",
            CandidateKind::Macro => "macro",
            CandidateKind::Loop => "loop",
        }
    }
}

/// A loop candidate's source (`StingerCandidate.LoopSource`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopSource {
    /// An installed Apple Loop matching `category` ("" = any) and every word of `query`.
    Stock { category: String, query: String },
    /// A loop-library clip by id.
    Clip(String),
}

/// One candidate of the curated table (`StingerCandidate`).
#[derive(Clone, Debug, PartialEq)]
pub struct StingerCandidate {
    pub id: String,
    pub kind: CandidateKind,
    pub label: String,
    pub option: Option<String>,
    pub macro_id: Option<String>,
    pub loop_source: Option<LoopSource>,
    pub bars: Option<i64>,
    pub max_beats: i64,
    pub tempo_tolerance: f64,
    pub families: Vec<String>,
    pub palettes: Vec<String>,
    /// `any` (unpitched) or `tonal` (pitched on the stinger nets' note).
    pub key: String,
    /// The track tempos it fits, BPM (closed).
    pub tempo: (f64, f64),
}

impl StingerCandidate {
    /// It plays local-only content (an Apple Loop, Apple's GM bank, a local bank).
    pub fn is_local_only(&self, set: &PaletteSet) -> bool {
        if matches!(self.loop_source, Some(LoopSource::Stock { .. })) {
            return true;
        }
        self.option
            .as_deref()
            .map(|o| set.is_local_only_option(o))
            .unwrap_or(false)
    }
}

/// A loop a Hit resolved to (`StingerAssignment.Loop`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct AssignedLoop {
    /// A stock loop's file, or nil for a library clip.
    pub path: Option<String>,
    /// A library clip's id.
    pub clip_id: Option<String>,
    pub name: String,
    pub beats: i64,
    pub bpm: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum AssignmentKind {
    OneShot,
    Macro,
    Loop,
    /// No candidate fits: the Hit's stinger net as before.
    Fallback,
}

impl AssignmentKind {
    pub fn name(&self) -> &'static str {
        match self {
            AssignmentKind::OneShot => "oneShot",
            AssignmentKind::Macro => "macro",
            AssignmentKind::Loop => "loop",
            AssignmentKind::Fallback => "fallback",
        }
    }
}

/// What one Hit fires, as [`resolve`] assigned it (`StingerAssignment`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct StingerAssignment {
    /// 0…3 (Hit 1…4).
    pub slot: usize,
    pub kind: AssignmentKind,
    pub candidate: Option<String>,
    pub label: String,
    pub option: Option<String>,
    pub macro_id: Option<String>,
    pub bars: Option<i64>,
    pub loop_: Option<AssignedLoop>,
    pub local_only: bool,
}

impl StingerAssignment {
    /// The Hit's stinger net (`hit1`…`hit4`).
    pub fn net_id(&self) -> String {
        format!("hit{}", self.slot + 1)
    }

    pub fn fallback(slot: usize) -> StingerAssignment {
        StingerAssignment {
            slot,
            kind: AssignmentKind::Fallback,
            candidate: None,
            label: "stinger".into(),
            option: None,
            macro_id: None,
            bars: None,
            loop_: None,
            local_only: false,
        }
    }

    /// How the Hit button's help says it.
    pub fn help(&self) -> String {
        let n = self.slot + 1;
        match self.kind {
            AssignmentKind::OneShot => format!("Hit {n}: {}", self.label),
            AssignmentKind::Macro => format!("Hit {n}: {} (macro)", self.label),
            AssignmentKind::Loop => format!(
                "Hit {n}: {} — {}",
                self.label,
                self.loop_
                    .as_ref()
                    .map(|l| l.name.as_str())
                    .unwrap_or("loop")
            ),
            AssignmentKind::Fallback => format!("Hit {n}: the track's stinger"),
        }
    }

    /// As `dj_state.decks.X.stingers` shows it.
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("hit", JsonValue::int(self.slot as i64 + 1)),
            ("kind", JsonValue::str(self.kind.name())),
            ("label", JsonValue::str(self.label.clone())),
        ]);
        if let Some(c) = &self.candidate {
            o.set("candidate", JsonValue::str(c.clone()));
        }
        if let Some(c) = &self.option {
            o.set("option", JsonValue::str(c.clone()));
        }
        if let Some(c) = &self.macro_id {
            o.set("macro", JsonValue::str(c.clone()));
        }
        if let Some(b) = self.bars {
            o.set("bars", JsonValue::int(b));
        }
        if let Some(l) = &self.loop_ {
            let mut lo = JsonObject::from_pairs([
                ("name", JsonValue::str(l.name.clone())),
                ("beats", JsonValue::int(l.beats)),
                ("bpm", JsonValue::Number(libm::round(l.bpm * 10.0) / 10.0)),
            ]);
            if let Some(p) = &l.path {
                lo.set("path", JsonValue::str(p.clone()));
            }
            if let Some(c) = &l.clip_id {
                lo.set("clip", JsonValue::str(c.clone()));
            }
            o.set("loop", JsonValue::Object(lo));
        }
        if self.local_only {
            o.set("localOnly", JsonValue::Bool(true));
        }
        JsonValue::Object(o)
    }
}

/// What the resolver may pick from here: which options play, which macros this app plays,
/// which library clips exist, which stock loops are installed (`StingerContent`).
pub struct StingerContent<'a> {
    pub option: Box<dyn Fn(&str) -> bool + 'a>,
    pub macro_ok: Box<dyn Fn(&str) -> bool + 'a>,
    pub clips: BTreeSet<String>,
    pub stock: Vec<StockLoop>,
}

/// Is a catalog id one this app plays (`Deck.isMacro(id:)`: `NetMacro(id:) != nil ||
/// MasterMacro(id:) != nil`)?
pub fn is_macro(id: &str) -> bool {
    NetMacro::from_id(id).is_some() || MasterMacro::from_id(id).is_some()
}

impl<'a> StingerContent<'a> {
    pub fn new(option: impl Fn(&str) -> bool + 'a) -> Self {
        StingerContent {
            option: Box::new(option),
            macro_ok: Box::new(is_macro),
            clips: BTreeSet::new(),
            stock: Vec::new(),
        }
    }
    /// Everything a candidate names is there (tests).
    pub fn everything() -> Self {
        Self::new(|_| true)
    }
    /// Nothing installed: every Hit falls back unless a macro fits.
    pub fn nothing() -> Self {
        Self::new(|_| false)
    }
    /// This machine: the options that play here (`SoundPalette.unavailableReason`).
    pub fn here(set: &'a PaletteSet, ctx: Sounds<'a>, stock: Vec<StockLoop>) -> Self {
        let mut c =
            Self::new(move |o| super::unavailable_reason_option(set, o, "stinger", ctx).is_none());
        c.stock = stock;
        c
    }
}

/// The track's key, as the key fit reads it (`StingerCuration.Key`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub root: i64,
    pub intervals: Vec<i64>,
}

impl Key {
    /// A project's root and scale name (`"Minor"`, …); `Key(project)`.
    pub fn from_scale(root: i64, scale_name: &str) -> Key {
        Key {
            root,
            intervals: scale_intervals(&format!("{scale_name}Scale")).to_vec(),
        }
    }
    /// The scale holds `note`'s pitch class.
    pub fn holds(&self, note: i64) -> bool {
        self.intervals
            .contains(&(((note - self.root) % 12 + 12) % 12))
    }
}

/// The curated table (`StingerCuration.Table`).
#[derive(Debug)]
pub struct Table {
    pub families: BTreeMap<String, String>,
    pub slots: Vec<Vec<CandidateKind>>,
    pub candidates: Vec<StingerCandidate>,
}

static TABLE: OnceBox<Table> = OnceBox::new();

impl Table {
    /// The bundled table, parsed once.
    pub fn shared() -> &'static Table {
        TABLE.get_or_init(|| {
            Box::new(Table::parse(
                &JsonValue::parse(STINGERS_JSON).expect("bundled stingers.json parses"),
            ))
        })
    }

    /// `StingerCuration.parse`.
    pub fn parse(json: &JsonValue) -> Table {
        let mut families = BTreeMap::new();
        if let Some(o) = json.get("families").and_then(JsonValue::as_object) {
            for (family, genres) in o.iter() {
                for g in genres.strings().unwrap_or_default() {
                    families.insert(g, family.to_string());
                }
            }
        }
        let slots = json
            .get("slots")
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| {
                        s.strings()
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|k| CandidateKind::from_name(k))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let candidates = json
            .get("candidates")
            .and_then(JsonValue::as_array)
            .map(|a| a.iter().filter_map(Self::parse_candidate).collect())
            .unwrap_or_default();
        Table {
            families,
            slots,
            candidates,
        }
    }

    /// `StingerCuration.parse(candidate:)`.
    pub fn parse_candidate(v: &JsonValue) -> Option<StingerCandidate> {
        let id = v.get("id")?.as_str()?.to_string();
        let kind = CandidateKind::from_name(v.get("kind")?.as_str()?)?;
        let s = |k: &str| v.get(k).and_then(JsonValue::as_str).map(String::from);
        let loop_source = if let Some(st) = v.get("stock") {
            Some(LoopSource::Stock {
                category: st
                    .get("category")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .into(),
                query: st
                    .get("query")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .into(),
            })
        } else {
            s("clip").map(LoopSource::Clip)
        };
        let t: Vec<f64> = v
            .get("tempo")
            .and_then(JsonValue::as_array)
            .map(|a| a.iter().filter_map(JsonValue::as_f64).collect())
            .unwrap_or_default();
        let lo = t.first().copied().unwrap_or(0.0);
        let hi = t.get(1).copied().unwrap_or(999.0);
        Some(StingerCandidate {
            label: s("label").unwrap_or_else(|| id.clone()),
            id,
            kind,
            option: s("option"),
            macro_id: s("macro"),
            loop_source,
            bars: v.get("bars").and_then(JsonValue::as_i64),
            max_beats: v.get("maxBeats").and_then(JsonValue::as_i64).unwrap_or(16),
            tempo_tolerance: v
                .get("tempoTolerance")
                .and_then(JsonValue::as_f64)
                .unwrap_or(0.12),
            families: v
                .get("families")
                .and_then(JsonValue::strings)
                .unwrap_or_else(|| alloc::vec!["*".into()]),
            palettes: v
                .get("palettes")
                .and_then(JsonValue::strings)
                .unwrap_or_else(|| alloc::vec!["*".into()]),
            key: s("key").unwrap_or_else(|| "any".into()),
            tempo: (lo.min(hi), lo.max(hi)),
        })
    }
}

/// The family of a beats genre ("club", "bass"…), "" for one the table doesn't sort
/// (`StingerCuration.family(of:)`).
pub fn family_of(genre: &str) -> String {
    Table::shared()
        .families
        .get(genre)
        .cloned()
        .or_else(|| {
            crate::gen::tables::BeatsTables::shared()
                .native
                .get(genre)
                .map(|n| n.stinger_family.clone())
        })
        .unwrap_or_default()
}

/// The four Hits for a configuration: for each Hit, the first kind (`slots`) with an
/// eligible candidate not already taken by an earlier Hit, drawn from them by `seed` + Hit
/// (candidates naming the family or palette outright weigh double); a Hit with none falls
/// back. `bpm`: the track's tempo (None: the genre's); `key`: its key (None: tonal
/// candidates fit). Deterministic. `StingerCuration.resolve`.
pub fn resolve(
    set: &PaletteSet,
    genre: &str,
    palette: Option<&str>,
    seed: f64,
    installed: &StingerContent,
    bpm: Option<f64>,
    key: Option<&Key>,
) -> Vec<StingerAssignment> {
    let palette_id = set
        .playing(palette)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "studio".into());
    let tempo = bpm.unwrap_or_else(|| {
        BeatsTables::shared()
            .genres
            .get(genre)
            .map(|g| g.bpm)
            .unwrap_or(120.0)
    });
    compute(set, genre, &palette_id, seed, installed, tempo, key)
}

/// `StingerCuration.compute`.
pub fn compute(
    set: &PaletteSet,
    genre: &str,
    palette_id: &str,
    seed: f64,
    installed: &StingerContent,
    tempo: f64,
    key: Option<&Key>,
) -> Vec<StingerAssignment> {
    let table = Table::shared();
    let family = family_of(genre);
    let seed_s = swift_double(seed);
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for slot in 0..SLOT_COUNT {
        let kinds: &[CandidateKind] = table.slots.get(slot).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut picked: Option<StingerAssignment> = None;
        for kind in kinds {
            let mut pool: Vec<&StingerCandidate> = Vec::new();
            for c in table
                .candidates
                .iter()
                .filter(|c| c.kind == *kind && !taken.contains(c.id.as_str()))
            {
                if !fits(c, &family, palette_id, tempo, key) || !available(c, installed, tempo) {
                    continue;
                }
                let weight = (if c.families.contains(&family) { 2 } else { 1 })
                    * (if c.palettes.iter().any(|p| p == palette_id) {
                        2
                    } else {
                        1
                    });
                for _ in 0..weight {
                    pool.push(c);
                }
            }
            if pool.is_empty() {
                continue;
            }
            let h = stable_hash(&format!(
                "{seed_s}|hit{}|stinger:{genre}|{palette_id}",
                slot + 1
            ));
            let c = pool[(h % pool.len() as u64) as usize];
            taken.insert(c.id.as_str());
            picked = Some(assignment(set, c, slot, seed, installed, tempo));
            break;
        }
        out.push(picked.unwrap_or_else(|| StingerAssignment::fallback(slot)));
    }
    out
}

/// The candidate fits the track: family, palette, tempo, key (`StingerCuration.fits`).
pub fn fits(
    c: &StingerCandidate,
    family: &str,
    palette: &str,
    tempo: f64,
    key: Option<&Key>,
) -> bool {
    if !(c.families.iter().any(|f| f == "*")
        || (!family.is_empty() && c.families.iter().any(|f| f == family)))
    {
        return false;
    }
    if !(c.palettes.iter().any(|p| p == "*") || c.palettes.iter().any(|p| p == palette)) {
        return false;
    }
    if !(c.tempo.0 <= tempo && tempo <= c.tempo.1) {
        return false;
    }
    if c.key == "tonal" {
        if let Some(k) = key {
            if !k.holds(STINGER_NOTE) {
                return false;
            }
        }
    }
    true
}

/// What it plays is here (and, for a macro, one this app plays)
/// (`StingerCuration.available`).
pub fn available(c: &StingerCandidate, installed: &StingerContent, tempo: f64) -> bool {
    match c.kind {
        CandidateKind::OneShot => c
            .option
            .as_deref()
            .map(|o| (installed.option)(o))
            .unwrap_or(false),
        CandidateKind::Macro => c
            .macro_id
            .as_deref()
            .map(|m| (installed.macro_ok)(m))
            .unwrap_or(false),
        CandidateKind::Loop => !loops(c, installed, tempo).is_empty(),
    }
}

/// A loop candidate's matches here, nearest the track's tempo first, then by name
/// (`StingerCuration.loops`).
pub fn loops(c: &StingerCandidate, installed: &StingerContent, tempo: f64) -> Vec<AssignedLoop> {
    match &c.loop_source {
        Some(LoopSource::Clip(id)) => {
            if installed.clips.contains(id) {
                alloc::vec![AssignedLoop {
                    path: None,
                    clip_id: Some(id.clone()),
                    name: c.label.clone(),
                    beats: 0,
                    bpm: tempo,
                }]
            } else {
                Vec::new()
            }
        }
        Some(LoopSource::Stock { category, query }) => {
            let ratio = |l: &StockLoop| {
                [0.5, 1.0, 2.0]
                    .iter()
                    .map(|m| (l.bpm() * m - tempo).abs() / tempo.max(1.0))
                    .fold(f64::INFINITY, f64::min)
            };
            matching(&installed.stock, category, query, Some(tempo))
                .into_iter()
                .filter(|l| l.beats > 0 && l.beats <= c.max_beats && ratio(l) <= c.tempo_tolerance)
                .map(|l| AssignedLoop {
                    path: Some(l.path.clone()),
                    clip_id: None,
                    name: l.name.clone(),
                    beats: l.beats,
                    bpm: l.bpm(),
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// `StingerCuration.assignment`.
pub fn assignment(
    set: &PaletteSet,
    c: &StingerCandidate,
    slot: usize,
    seed: f64,
    installed: &StingerContent,
    tempo: f64,
) -> StingerAssignment {
    let mut a = StingerAssignment {
        slot,
        kind: AssignmentKind::Fallback,
        candidate: Some(c.id.clone()),
        label: c.label.clone(),
        option: None,
        macro_id: None,
        bars: c.bars,
        loop_: None,
        local_only: c.is_local_only(set),
    };
    match c.kind {
        CandidateKind::OneShot => {
            a.kind = AssignmentKind::OneShot;
            a.option = c.option.clone();
        }
        CandidateKind::Macro => {
            a.kind = AssignmentKind::Macro;
            a.macro_id = c.macro_id.clone();
        }
        CandidateKind::Loop => {
            a.kind = AssignmentKind::Loop;
            // the nearest few in tempo, one drawn by the seed: a pack's variants all get a turn
            let near: Vec<AssignedLoop> = loops(c, installed, tempo).into_iter().take(8).collect();
            if !near.is_empty() {
                let h = stable_hash(&format!(
                    "{}|hit{}|loop:{}",
                    swift_double(seed),
                    slot + 1,
                    c.id
                ));
                a.loop_ = Some(near[(h % near.len() as u64) as usize].clone());
            }
        }
    }
    a
}

/// Play `assignments` on these nets' Hits: each one-shot's option goes on its stinger net
/// (`TrackSound.curated`; the user's role override still wins), and the sounds are resolved
/// again. The composition is untouched. (`Project.curateStingers`; the caller keeps the
/// assignments list, which is `Project.stingers`.)
pub fn curate_stingers(
    nets: &mut [SoundNet],
    assignments: &[StingerAssignment],
    seed: f64,
    set: &PaletteSet,
    ctx: Sounds,
) {
    for a in assignments {
        let id = a.net_id();
        if let Some(nb) = nets.iter_mut().find(|n| n.id == id) {
            nb.sound.curated = if a.kind == AssignmentKind::OneShot {
                a.option.clone()
            } else {
                None
            };
        }
    }
    apply_sound(nets, seed, set, ctx);
}
