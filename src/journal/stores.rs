//! The persistent stores as values, and the edits as pure functions of them: what
//! `FavoritesStore`, the crates, `VoiceVariationStore` and `NotesStore` hold. A journal
//! replay applies its store events with these, so it ends where the files did. Ports the
//! value parts of `Session/FavoritesStore.swift`, `Session/VoiceVariationStore.swift`,
//! `Journal/Notes.swift` and `JournalStoreState` (`Journal/JournalState.swift`); the files
//! are `crate::host::stores`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::time;
use crate::cid;
use crate::json::{JsonObject, JsonValue};
use crate::session::types::VoiceTweak;
use crate::session::DeckSource;

fn trim_ws(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

// ---- favorites ---------------------------------------------------------------------------------------------------------------

/// `Favorite`: a track worth keeping.
#[derive(Clone, Debug, PartialEq)]
pub struct Favorite {
    pub title: String,
    pub source: DeckSource,
    /// Unix milliseconds (stored to the second).
    pub added: i64,
    /// A sample's audio file; None for nets.
    pub file: Option<String>,
    pub tags: Vec<String>,
}

/// `DeckSourceDescriptor.favoriteCID`: what a favorite of this source is named by.
pub fn favorite_cid(source: &DeckSource) -> Option<String> {
    match source {
        DeckSource::Net(spec) => Some(cid::share_cid(&spec.payload())),
        DeckSource::RawNets(p) => Some(cid::share_cid(p)),
        DeckSource::None | DeckSource::Sample { .. } => None,
    }
}

impl Favorite {
    pub fn new(title: &str, source: DeckSource, added: i64) -> Self {
        Favorite {
            title: title.to_string(),
            source,
            added,
            file: None,
            tags: Vec::new(),
        }
    }

    /// The name of a favorite sample loaded from `path`.
    pub fn file_cid(path: &str) -> String {
        format!("file:{path}")
    }

    /// What names a favorite: a net's share CID, a sample's file ("file:" + its path).
    pub fn cid(&self) -> String {
        favorite_cid(&self.source)
            .or_else(|| self.file.as_deref().map(Self::file_cid))
            .unwrap_or_default()
    }

    pub fn is_sample(&self) -> bool {
        favorite_cid(&self.source).is_none() && self.file.is_some()
    }

    /// The entry as the store (and the journal) writes it.
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("title", JsonValue::str(self.title.as_str())),
            ("added", JsonValue::String(time::string_seconds(self.added))),
            ("source", self.source.json()),
        ]);
        if let Some(f) = &self.file {
            o.set("file", JsonValue::str(f.as_str()));
        }
        if !self.tags.is_empty() {
            o.set(
                "tags",
                JsonValue::Array(
                    self.tags
                        .iter()
                        .map(|t| JsonValue::str(t.as_str()))
                        .collect(),
                ),
            );
        }
        JsonValue::Object(o)
    }

    /// An entry back; None if its source no longer decodes or names nothing.
    pub fn from_json(e: &JsonValue) -> Option<Favorite> {
        let source = DeckSource::from_json(e.get("source")?)?;
        let file = e.get("file").and_then(JsonValue::as_str).map(String::from);
        if favorite_cid(&source).is_none() && file.is_none() {
            return None;
        }
        Some(Favorite {
            title: e
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            source,
            added: e
                .get("added")
                .and_then(JsonValue::as_str)
                .and_then(time::date_seconds)
                .unwrap_or(time::DISTANT_PAST),
            file,
            tags: e
                .get("tags")
                .and_then(JsonValue::strings)
                .unwrap_or_default(),
        })
    }
}

/// `FavoritesStore.adding`: `favorite` on top (starring it again keeps its tags), any older
/// entry of its CID dropped.
pub fn favorites_adding(favorite: &Favorite, list: &[Favorite]) -> Vec<Favorite> {
    let cid = favorite.cid();
    if cid.is_empty() {
        return list.to_vec();
    }
    let mut f = favorite.clone();
    if f.tags.is_empty() {
        if let Some(old) = list.iter().find(|o| o.cid() == cid) {
            f.tags = old.tags.clone();
        }
    }
    let mut out = alloc::vec![f];
    out.extend(list.iter().filter(|o| o.cid() != cid).cloned());
    out
}

/// `FavoritesStore.removing`.
pub fn favorites_removing(cid: &str, list: &[Favorite]) -> Vec<Favorite> {
    list.iter().filter(|f| f.cid() != cid).cloned().collect()
}

/// `FavoritesStore.settingTags`: trimmed, blanks and case-insensitive repeats dropped.
pub fn favorites_setting_tags(cid: &str, tags: &[String], list: &[Favorite]) -> Vec<Favorite> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let clean: Vec<String> = tags
        .iter()
        .map(|t| trim_ws(t).to_string())
        .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
        .collect();
    list.iter()
        .map(|f| {
            let mut f = f.clone();
            if f.cid() == cid {
                f.tags = clean.clone();
            }
            f
        })
        .collect()
}

/// `[Favorite].allTags`: every tag used, most used first (then A–Z).
pub fn all_tags(list: &[Favorite]) -> Vec<String> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for f in list {
        for t in &f.tags {
            *count.entry(t).or_default() += 1;
        }
    }
    let mut keys: Vec<&str> = count.keys().copied().collect();
    keys.sort_by(|a, b| count[b].cmp(&count[a]).then_with(|| a.cmp(b)));
    keys.into_iter().map(String::from).collect()
}

/// `[Favorite].tagged`: the favorites carrying `tag` (all of them for None).
pub fn tagged(list: &[Favorite], tag: Option<&str>) -> Vec<Favorite> {
    match tag {
        None => list.to_vec(),
        Some(t) => list
            .iter()
            .filter(|f| f.tags.iter().any(|x| x == t))
            .cloned()
            .collect(),
    }
}

// ---- voice variations --------------------------------------------------------------------------------------------

/// `VoiceVariation`: a saved way to play an instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceVariation {
    pub name: String,
    pub params: VoiceTweak,
    pub use_synth: bool,
}

impl VoiceVariation {
    pub fn new(name: &str, params: VoiceTweak, use_synth: bool) -> Self {
        VoiceVariation {
            name: name.to_string(),
            params,
            use_synth,
        }
    }
    /// "laser · short zap": how a chip names it beside its instrument.
    pub fn label(&self, instrument: &str) -> String {
        format!("{instrument} · {}", self.name)
    }
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("name", JsonValue::str(self.name.as_str())),
            ("params", self.params.to_json()),
        ]);
        if self.use_synth {
            o.set("useSynth", JsonValue::Bool(true));
        }
        JsonValue::Object(o)
    }
    pub fn from_json(e: &JsonValue) -> Option<VoiceVariation> {
        let name = e.get("name")?.as_str()?;
        if name.is_empty() {
            return None;
        }
        Some(VoiceVariation {
            name: name.to_string(),
            params: VoiceTweak::from_json(e.get("params")),
            use_synth: e
                .get("useSynth")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
        })
    }
}

pub type Variations = BTreeMap<String, Vec<VoiceVariation>>;

/// `VoiceVariationStore.saving`: replacing one of the same name, in its place.
pub fn variations_saving(
    variation: &VoiceVariation,
    instrument: &str,
    all: &Variations,
) -> Variations {
    let mut v = variation.clone();
    v.name = trim_ws(&v.name).to_string();
    if v.name.is_empty() {
        return all.clone();
    }
    let mut all = all.clone();
    let mine = all.entry(instrument.to_string()).or_default();
    match mine.iter().position(|x| x.name == v.name) {
        Some(i) => mine[i] = v,
        None => mine.push(v),
    }
    all
}

/// `VoiceVariationStore.renaming`: a name already taken replaces that one.
pub fn variations_renaming(name: &str, to: &str, instrument: &str, all: &Variations) -> Variations {
    let to = trim_ws(to).to_string();
    let mut all = all.clone();
    let Some(mine) = all.get_mut(instrument) else {
        return all;
    };
    let Some(i) = mine.iter().position(|x| x.name == name) else {
        return all;
    };
    if to.is_empty() || to == name {
        return all;
    }
    mine[i].name = to.clone();
    let kept: Vec<VoiceVariation> = mine
        .iter()
        .enumerate()
        .filter(|(j, x)| *j == i || x.name != to)
        .map(|(_, x)| x.clone())
        .collect();
    *mine = kept;
    all
}

/// `VoiceVariationStore.deleting`.
pub fn variations_deleting(name: &str, instrument: &str, all: &Variations) -> Variations {
    let mut all = all.clone();
    let mine: Vec<VoiceVariation> = all
        .get(instrument)
        .map(|m| m.iter().filter(|x| x.name != name).cloned().collect())
        .unwrap_or_default();
    if mine.is_empty() {
        all.remove(instrument);
    } else {
        all.insert(instrument.to_string(), mine);
    }
    all
}

// ---- notes -----------------------------------------------------------------------------------------------------------------

/// `NoteTarget`: what a note is about.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoteTarget {
    /// A net track by its share CID.
    Track { cid: String },
    /// A sample by its audio content id, with the file it was loaded from when known.
    Sample { id: String, file: Option<String> },
    /// A LOOP-record clip on the row, by its id.
    Clip { id: String },
}

impl NoteTarget {
    /// "track:<cid>" | "sample:<content id>" | "clip:<id>": what notes are looked up by.
    pub fn key(&self) -> String {
        match self {
            NoteTarget::Track { cid } => format!("track:{cid}"),
            NoteTarget::Sample { id, .. } => format!("sample:{id}"),
            NoteTarget::Clip { id } => format!("clip:{id}"),
        }
    }

    /// Does a favorite name this target?
    pub fn matches(&self, f: &Favorite) -> bool {
        match self {
            NoteTarget::Track { cid } => *cid == f.cid(),
            NoteTarget::Sample { file, .. } => {
                file.as_deref().map(Favorite::file_cid).as_deref() == Some(f.cid().as_str())
            }
            NoteTarget::Clip { .. } => false,
        }
    }

    pub fn json(&self) -> JsonValue {
        match self {
            NoteTarget::Track { cid } => JsonValue::Object(JsonObject::from_pairs([
                ("kind", JsonValue::str("track")),
                ("cid", JsonValue::str(cid.as_str())),
            ])),
            NoteTarget::Sample { id, file } => {
                let mut o = JsonObject::from_pairs([
                    ("kind", JsonValue::str("sample")),
                    ("id", JsonValue::str(id.as_str())),
                ]);
                if let Some(f) = file {
                    o.set("file", JsonValue::str(f.as_str()));
                }
                JsonValue::Object(o)
            }
            NoteTarget::Clip { id } => JsonValue::Object(JsonObject::from_pairs([
                ("kind", JsonValue::str("clip")),
                ("id", JsonValue::str(id.as_str())),
            ])),
        }
    }

    pub fn from_json(v: &JsonValue) -> Option<NoteTarget> {
        match v.get("kind")?.as_str()? {
            "track" => Some(NoteTarget::Track {
                cid: v.get("cid")?.as_str()?.to_string(),
            }),
            "sample" => Some(NoteTarget::Sample {
                id: v.get("id")?.as_str()?.to_string(),
                file: v.get("file").and_then(JsonValue::as_str).map(String::from),
            }),
            "clip" => Some(NoteTarget::Clip {
                id: v.get("id")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }
}

/// `NotePosition`: a moment within the target.
#[derive(Clone, Debug, PartialEq)]
pub struct NotePosition {
    pub bar: i64,
    pub beat: i64,
    /// Seconds into the track at its own tempo.
    pub seconds: Option<f64>,
}

impl NotePosition {
    pub fn new(bar: i64, beat: i64, seconds: Option<f64>) -> Self {
        NotePosition { bar, beat, seconds }
    }
    /// "bar 17" or "bar 17.3".
    pub fn label(&self) -> String {
        if self.beat > 1 {
            format!("bar {}.{}", self.bar, self.beat)
        } else {
            format!("bar {}", self.bar)
        }
    }
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("bar", JsonValue::int(self.bar)),
            ("beat", JsonValue::int(self.beat)),
        ]);
        if let Some(s) = self.seconds {
            o.set("seconds", JsonValue::Number(s));
        }
        JsonValue::Object(o)
    }
    pub fn from_json(v: Option<&JsonValue>) -> Option<NotePosition> {
        let v = v?;
        Some(NotePosition {
            bar: v.get("bar")?.as_i64()?,
            beat: v.get("beat").and_then(JsonValue::as_i64).unwrap_or(1),
            seconds: v.get("seconds").and_then(JsonValue::as_f64),
        })
    }
}

/// `TrackNote`: text on a target, maybe at a position. Times are Unix milliseconds.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackNote {
    pub id: String,
    pub target: NoteTarget,
    /// What the target played as when the note was made (for lists; never a key).
    pub title: String,
    pub text: String,
    pub position: Option<NotePosition>,
    pub created: i64,
    pub edited: Option<i64>,
}

impl TrackNote {
    pub fn new(id: &str, target: NoteTarget, text: &str, created: i64) -> Self {
        TrackNote {
            id: id.to_string(),
            target,
            title: String::new(),
            text: text.to_string(),
            position: None,
            created,
            edited: None,
        }
    }

    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("id", JsonValue::str(self.id.as_str())),
            ("target", self.target.json()),
            ("title", JsonValue::str(self.title.as_str())),
            ("text", JsonValue::str(self.text.as_str())),
            ("created", JsonValue::String(time::string(self.created))),
        ]);
        if let Some(p) = &self.position {
            o.set("at", p.json());
        }
        if let Some(e) = self.edited {
            o.set("edited", JsonValue::String(time::string(e)));
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Option<TrackNote> {
        Some(TrackNote {
            id: v.get("id")?.as_str()?.to_string(),
            target: NoteTarget::from_json(v.get("target")?)?,
            title: v
                .get("title")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            text: v.get("text")?.as_str()?.to_string(),
            position: NotePosition::from_json(v.get("at")),
            created: v
                .get("created")
                .and_then(JsonValue::as_str)
                .and_then(time::date)
                .unwrap_or(time::DISTANT_PAST),
            edited: v
                .get("edited")
                .and_then(JsonValue::as_str)
                .and_then(time::date),
        })
    }
}

/// `[TrackNote].on(key)`: the notes on a target (by key), in track order: by position,
/// unplaced first, then by when.
pub fn notes_on(notes: &[TrackNote], key: &str) -> Vec<TrackNote> {
    let mut v: Vec<TrackNote> = notes
        .iter()
        .filter(|n| n.target.key() == key)
        .cloned()
        .collect();
    let pos = |n: &TrackNote| {
        n.position
            .as_ref()
            .map(|p| (p.bar, p.beat))
            .unwrap_or((0, 0))
    };
    v.sort_by(|a, b| pos(a).cmp(&pos(b)).then(a.created.cmp(&b.created)));
    v
}

/// `[TrackNote].on(favorite)`: the notes a favorite carries.
pub fn notes_on_favorite(notes: &[TrackNote], f: &Favorite) -> Vec<TrackNote> {
    notes
        .iter()
        .filter(|n| n.target.matches(f))
        .cloned()
        .collect()
}

/// `adding`: one with the same id is replaced (moved to the end).
pub fn notes_adding(notes: &[TrackNote], note: &TrackNote) -> Vec<TrackNote> {
    let mut v: Vec<TrackNote> = notes.iter().filter(|n| n.id != note.id).cloned().collect();
    v.push(note.clone());
    v
}

/// `editing`: new text for a note (blank text is ignored).
pub fn notes_editing(notes: &[TrackNote], id: &str, text: &str, at: i64) -> Vec<TrackNote> {
    let t = trim_ws(text);
    if t.is_empty() {
        return notes.to_vec();
    }
    notes
        .iter()
        .map(|n| {
            let mut n = n.clone();
            if n.id == id {
                n.text = t.to_string();
                n.edited = Some(at);
            }
            n
        })
        .collect()
}

/// `deleting`.
pub fn notes_deleting(notes: &[TrackNote], id: &str) -> Vec<TrackNote> {
    notes.iter().filter(|n| n.id != id).cloned().collect()
}

// ---- all of them ---------------------------------------------------------------------------------------------------

/// `JournalStoreState`: the persistent stores, as values.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct JournalStoreState {
    pub favorites: Vec<Favorite>,
    pub crates: Vec<String>,
    pub variations: Variations,
    pub notes: Vec<TrackNote>,
}

impl JournalStoreState {
    /// The crates list with `name` made (trimmed; blank or already there changes nothing).
    pub fn adding_crate(name: &str, crates: &[String]) -> Vec<String> {
        let n = trim_ws(name);
        if n.is_empty() || crates.iter().any(|c| c == n) {
            return crates.to_vec();
        }
        let mut v = crates.to_vec();
        v.push(n.to_string());
        v
    }

    pub fn json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            (
                "favorites",
                JsonValue::Array(self.favorites.iter().map(Favorite::json).collect()),
            ),
            (
                "crates",
                JsonValue::Array(
                    self.crates
                        .iter()
                        .map(|c| JsonValue::str(c.as_str()))
                        .collect(),
                ),
            ),
            (
                "variations",
                JsonValue::Object(JsonObject::from_pairs(self.variations.iter().map(
                    |(k, vs)| {
                        (
                            k.as_str(),
                            JsonValue::Array(vs.iter().map(VoiceVariation::json).collect()),
                        )
                    },
                ))),
            ),
            (
                "notes",
                JsonValue::Array(self.notes.iter().map(TrackNote::json).collect()),
            ),
        ]))
    }

    pub fn from_json(v: Option<&JsonValue>) -> Self {
        let mut vars = Variations::new();
        if let Some(o) = v
            .and_then(|v| v.get("variations"))
            .and_then(JsonValue::as_object)
        {
            for (k, list) in o.iter() {
                let vs: Vec<VoiceVariation> = list
                    .as_array()
                    .map(|a| a.iter().filter_map(VoiceVariation::from_json).collect())
                    .unwrap_or_default();
                if !vs.is_empty() {
                    vars.insert(k.to_string(), vs);
                }
            }
        }
        let arr = |k: &str| v.and_then(|v| v.get(k)).and_then(JsonValue::as_array);
        JournalStoreState {
            favorites: arr("favorites")
                .map(|a| a.iter().filter_map(Favorite::from_json).collect())
                .unwrap_or_default(),
            crates: v
                .and_then(|v| v.get("crates"))
                .and_then(JsonValue::strings)
                .unwrap_or_default(),
            variations: vars,
            notes: arr("notes")
                .map(|a| a.iter().filter_map(TrackNote::from_json).collect())
                .unwrap_or_default(),
        }
    }
}
