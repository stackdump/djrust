//! One line of the journal: what happened, when (the engine frame — the timebase replays
//! use — and the wall clock, metadata only), in order. Port of `Journal/JournalEvent.swift`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::state::JournalState;
use super::stores::{Favorite, JournalStoreState, TrackNote, VoiceVariation};
use super::time;
use crate::cid;
use crate::json::{js_number_string, JsonObject, JsonValue};
use crate::session::action::DeckAction;
use crate::session::model::DeckSource;
use crate::session::types::AutoMixSettings;

/// `JournalEvent`: what the journal records. `Action` and `Midi` are the performance (every
/// input `DJEngine.perform` and the `ControllerRouter` see, the same as a session's events);
/// the rest are the state changes that are not pokes — the persistent stores and the notes —
/// so a replay rebuilds them too.
#[derive(Clone, Debug, PartialEq)]
pub enum JournalEvent {
    /// The full rebuildable state: every file starts with one; mid-file ones add the
    /// playheads and leave the stores out.
    Checkpoint(JournalState),
    Action(DeckAction),
    Midi {
        port: String,
        bytes: Vec<u8>,
    },
    /// The sample with this content id was loaded from `path`.
    SampleFile {
        id: String,
        path: String,
    },
    /// A clip removed in an earlier run came back from the loop library's `removed/`.
    LibraryRestore {
        id: String,
        index: i64,
    },
    FavoriteAdd(Favorite),
    FavoriteRemove {
        cid: String,
    },
    FavoriteTags {
        cid: String,
        tags: Vec<String>,
    },
    CrateAdd {
        name: String,
    },
    CrateDelete {
        name: String,
    },
    VariationSave {
        instrument: String,
        variation: VoiceVariation,
    },
    VariationRename {
        instrument: String,
        name: String,
        to: String,
    },
    VariationDelete {
        instrument: String,
        name: String,
    },
    NoteAdd(TrackNote),
    NoteEdit {
        id: String,
        text: String,
        at: i64,
    },
    NoteDelete {
        id: String,
    },
    /// Every store replaced wholesale.
    StoresReplace(JournalStoreState),
    /// A milestone with no state of its own: "app.start", "session.start", …
    Mark(String),
    /// A live op (beyond Swift; docs/ACTION-LOG.md): a move that is no `DeckAction` and no MIDI
    /// (a mixer-side move of the window or the socket: an input strip's knob, the vocoder, a
    /// trick, the headphone cue…), as `{method, params}` of
    /// [`ArrangeOp::Live`](crate::session::arrangement::ArrangeOp::Live). A reader that does
    /// not know the kind skips the line (the Swift app does), so a journal stays readable.
    Live { method: String, params: JsonValue },
}

fn s(v: &str) -> JsonValue {
    JsonValue::str(v)
}

impl JournalEvent {
    /// The `k` field of the line.
    pub fn kind(&self) -> &'static str {
        match self {
            JournalEvent::Checkpoint(_) => "checkpoint",
            JournalEvent::Action(_) => "act",
            JournalEvent::Midi { .. } => "midi",
            JournalEvent::SampleFile { .. } => "sample.file",
            JournalEvent::LibraryRestore { .. } => "library.restore",
            JournalEvent::FavoriteAdd(_) => "favorite.add",
            JournalEvent::FavoriteRemove { .. } => "favorite.remove",
            JournalEvent::FavoriteTags { .. } => "favorite.tags",
            JournalEvent::CrateAdd { .. } => "crate.add",
            JournalEvent::CrateDelete { .. } => "crate.delete",
            JournalEvent::VariationSave { .. } => "variation.save",
            JournalEvent::VariationRename { .. } => "variation.rename",
            JournalEvent::VariationDelete { .. } => "variation.delete",
            JournalEvent::NoteAdd(_) => "note.add",
            JournalEvent::NoteEdit { .. } => "note.edit",
            JournalEvent::NoteDelete { .. } => "note.delete",
            JournalEvent::StoresReplace(_) => "stores.replace",
            JournalEvent::Mark(_) => "mark",
            JournalEvent::Live { .. } => "live",
        }
    }

    pub fn is_note(&self) -> bool {
        matches!(
            self,
            JournalEvent::NoteAdd(_)
                | JournalEvent::NoteEdit { .. }
                | JournalEvent::NoteDelete { .. }
        )
    }

    /// A rewind's mark (`Journal.rewindMark`).
    pub fn is_rewind_mark(&self) -> bool {
        matches!(self, JournalEvent::Mark(t) if t.starts_with("rewind "))
    }

    /// The event's own fields (beside `v`, `seq`, `t`, `f`, `k`).
    fn fields(&self) -> Vec<(&'static str, JsonValue)> {
        match self {
            JournalEvent::Checkpoint(st) => vec![("state", st.json())],
            JournalEvent::Action(a) => vec![("a", a.json())],
            JournalEvent::Midi { port, bytes } => vec![
                ("p", s(port)),
                (
                    "b",
                    JsonValue::Array(bytes.iter().map(|b| JsonValue::int(*b as i64)).collect()),
                ),
            ],
            JournalEvent::SampleFile { id, path } => vec![("id", s(id)), ("path", s(path))],
            JournalEvent::LibraryRestore { id, index } => {
                vec![("id", s(id)), ("index", JsonValue::int(*index))]
            }
            JournalEvent::FavoriteAdd(f) => vec![("favorite", f.json())],
            JournalEvent::FavoriteRemove { cid } => vec![("cid", s(cid))],
            JournalEvent::FavoriteTags { cid, tags } => vec![
                ("cid", s(cid)),
                (
                    "tags",
                    JsonValue::Array(tags.iter().map(|t| s(t)).collect()),
                ),
            ],
            JournalEvent::CrateAdd { name } | JournalEvent::CrateDelete { name } => {
                vec![("name", s(name))]
            }
            JournalEvent::VariationSave {
                instrument,
                variation,
            } => {
                vec![
                    ("instrument", s(instrument)),
                    ("variation", variation.json()),
                ]
            }
            JournalEvent::VariationRename {
                instrument,
                name,
                to,
            } => {
                vec![
                    ("instrument", s(instrument)),
                    ("name", s(name)),
                    ("to", s(to)),
                ]
            }
            JournalEvent::VariationDelete { instrument, name } => {
                vec![("instrument", s(instrument)), ("name", s(name))]
            }
            JournalEvent::NoteAdd(n) => vec![("note", n.json())],
            JournalEvent::NoteEdit { id, text, at } => vec![
                ("id", s(id)),
                ("text", s(text)),
                ("at", JsonValue::String(time::string(*at))),
            ],
            JournalEvent::NoteDelete { id } => vec![("id", s(id))],
            JournalEvent::StoresReplace(st) => vec![("stores", st.json())],
            JournalEvent::Mark(t) => vec![("text", s(t))],
            JournalEvent::Live { method, params } => {
                vec![("m", s(method)), ("params", params.clone())]
            }
        }
    }

    /// The event of a line, by its `k`; None for a kind this build doesn't know (a reader
    /// skips it).
    pub fn from_kind(kind: &str, v: &JsonValue) -> Option<JournalEvent> {
        let st = |k: &str| v.get(k).and_then(JsonValue::as_str).map(String::from);
        Some(match kind {
            "checkpoint" => JournalEvent::Checkpoint(JournalState::from_json(v.get("state")?)?),
            "act" => JournalEvent::Action(DeckAction::from_json(v.get("a")?)?),
            "midi" => JournalEvent::Midi {
                port: st("p")?,
                bytes: v
                    .get("b")?
                    .ints()?
                    .into_iter()
                    .map(|x| x.clamp(0, 255) as u8)
                    .collect(),
            },
            "sample.file" => JournalEvent::SampleFile {
                id: st("id")?,
                path: st("path")?,
            },
            "library.restore" => JournalEvent::LibraryRestore {
                id: st("id")?,
                index: v.get("index").and_then(JsonValue::as_i64).unwrap_or(0),
            },
            "favorite.add" => JournalEvent::FavoriteAdd(Favorite::from_json(v.get("favorite")?)?),
            "favorite.remove" => JournalEvent::FavoriteRemove { cid: st("cid")? },
            "favorite.tags" => JournalEvent::FavoriteTags {
                cid: st("cid")?,
                tags: v
                    .get("tags")
                    .and_then(JsonValue::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(JsonValue::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            "crate.add" => JournalEvent::CrateAdd { name: st("name")? },
            "crate.delete" => JournalEvent::CrateDelete { name: st("name")? },
            "variation.save" => JournalEvent::VariationSave {
                instrument: st("instrument")?,
                variation: VoiceVariation::from_json(v.get("variation")?)?,
            },
            "variation.rename" => JournalEvent::VariationRename {
                instrument: st("instrument")?,
                name: st("name")?,
                to: st("to")?,
            },
            "variation.delete" => JournalEvent::VariationDelete {
                instrument: st("instrument")?,
                name: st("name")?,
            },
            "note.add" => JournalEvent::NoteAdd(TrackNote::from_json(v.get("note")?)?),
            "note.edit" => JournalEvent::NoteEdit {
                id: st("id")?,
                text: st("text")?,
                at: v.get("at")?.as_str().and_then(time::date)?,
            },
            "note.delete" => JournalEvent::NoteDelete { id: st("id")? },
            "stores.replace" => {
                let o = v.get("stores")?;
                o.as_object()?;
                JournalEvent::StoresReplace(JournalStoreState::from_json(Some(o)))
            }
            "mark" => JournalEvent::Mark(st("text")?),
            "live" => JournalEvent::Live {
                method: st("m")?,
                params: v.get("params").cloned().unwrap_or(JsonValue::Null),
            },
            _ => return None,
        })
    }

    /// One line for people (`djctl journal show`, the History panel).
    pub fn summary(&self) -> String {
        match self {
            JournalEvent::Checkpoint(st) => {
                let mut parts: Vec<String> = st
                    .decks
                    .iter()
                    .map(|(n, d)| format!("{n}: {}", source_summary(&d.source)))
                    .collect();
                parts.push(format!("{} clips", st.clips.len()));
                format!(
                    "{}{}",
                    if st.has_stores {
                        "checkpoint · "
                    } else {
                        "checkpoint (mid) · "
                    },
                    parts.join(" · ")
                )
            }
            JournalEvent::Action(a) => action_summary(a),
            JournalEvent::Midi { port, bytes } => {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
                format!("midi {port} {}", hex.join(" "))
            }
            JournalEvent::SampleFile { id, path } => {
                format!("sample {}… ← {path}", prefix(id, 10))
            }
            JournalEvent::LibraryRestore { id, .. } => {
                format!("loop {}… back from the library", prefix(id, 10))
            }
            JournalEvent::FavoriteAdd(f) => format!("★ {}", f.title),
            JournalEvent::FavoriteRemove { cid } => format!("☆ unstarred {}…", prefix(cid, 14)),
            JournalEvent::FavoriteTags { cid, tags } => format!(
                "tags {}… = {}",
                prefix(cid, 14),
                if tags.is_empty() {
                    "none".to_string()
                } else {
                    tags.iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            ),
            JournalEvent::CrateAdd { name } => format!("crate + {name}"),
            JournalEvent::CrateDelete { name } => format!("crate − {name}"),
            JournalEvent::VariationSave {
                instrument,
                variation,
            } => {
                format!("variation saved {}", variation.label(instrument))
            }
            JournalEvent::VariationRename {
                instrument,
                name,
                to,
            } => {
                format!("variation {instrument} · {name} → {to}")
            }
            JournalEvent::VariationDelete { instrument, name } => {
                format!("variation deleted {instrument} · {name}")
            }
            JournalEvent::NoteAdd(n) => format!(
                "✎ {}{}: {}",
                if n.title.is_empty() {
                    n.target.key()
                } else {
                    n.title.clone()
                },
                n.position
                    .as_ref()
                    .map(|p| format!(" @ {}", p.label()))
                    .unwrap_or_default(),
                n.text
            ),
            JournalEvent::NoteEdit { text, .. } => format!("✎ edited: {text}"),
            JournalEvent::NoteDelete { id } => format!("✎ deleted {id}"),
            JournalEvent::StoresReplace(st) => format!(
                "stores replaced · {} favorites · {} crates · {} variations · {} notes",
                st.favorites.len(),
                st.crates.len(),
                st.variations.values().map(Vec::len).sum::<usize>(),
                st.notes.len()
            ),
            JournalEvent::Mark(t) => format!("— {t}"),
            JournalEvent::Live { method, params } => {
                let p = params.js_stringified();
                if p.chars().count() > 100 {
                    let cut: String = p.chars().take(97).collect();
                    format!("live {method} {cut}…")
                } else {
                    format!("live {method} {p}")
                }
            }
        }
    }

    /// Consecutive events with the same key (a fader moved) fold into one row in a list.
    pub fn fold_key(&self) -> Option<String> {
        match self {
            JournalEvent::Action(a) => match a {
                DeckAction::Param { deck, name, .. } => Some(format!("{deck}.{name}")),
                DeckAction::MasterFx { deck, key, .. } => Some(format!("{deck}.fx.{key}")),
                DeckAction::Fx { deck, param, .. } => Some(format!("{deck}.fxunit.{param}")),
                DeckAction::Crossfader(_) => Some("crossfader".into()),
                DeckAction::Master(_) => Some("master".into()),
                DeckAction::ClipGain { id, .. } => Some(format!("gain.{id}")),
                DeckAction::ClipFx { id, .. } => Some(format!("fx.{id}")),
                DeckAction::ClipRetune { id, .. } => Some(format!("retune.{id}")),
                DeckAction::ClipImport { id, .. } => Some(format!("import.{id}")),
                DeckAction::ClipQuantize { id, .. } => Some(format!("quantize.{id}")),
                DeckAction::ClipFade { id, .. } => Some(format!("fade.{id}")),
                DeckAction::ClipFollow { id, .. } => Some(format!("follow.{id}")),
                DeckAction::VoiceTweak { deck, channel, .. } => {
                    Some(format!("voice.{deck}.{channel}"))
                }
                _ => None,
            },
            // a knob turned
            JournalEvent::Midi { port, bytes } if bytes.len() == 3 && bytes[0] & 0xF0 == 0xB0 => {
                Some(format!("cc.{port}.{}.{}", bytes[0], bytes[1]))
            }
            _ => None,
        }
    }
}

fn prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `DeckSourceDescriptor.summary`: "net techno #42", "sample demo loop", "empty".
pub fn source_summary(src: &DeckSource) -> String {
    match src {
        DeckSource::None => "empty".into(),
        DeckSource::Net(spec) => {
            format!("net {} #{}", spec.genre, js_number_string(spec.seed))
        }
        DeckSource::RawNets(p) => format!("share {}…", prefix(&cid::share_cid(p), 12)),
        DeckSource::Sample { id, title, .. } => format!(
            "sample {}",
            if title.is_empty() {
                prefix(id, 10)
            } else {
                title.clone()
            }
        ),
    }
}

/// `DeckAction.summary`: one line for people: "A play", "B volume 0.42", "A load net
/// techno #42".
pub fn action_summary(a: &DeckAction) -> String {
    let j = a.json();
    let op = j
        .get("op")
        .and_then(JsonValue::as_str)
        .unwrap_or("?")
        .to_string();
    match a {
        DeckAction::Param { deck, name, value } => format!("{deck} {name} {value:.2}"),
        DeckAction::Crossfader(v) => format!("crossfader {v:.2}"),
        DeckAction::Master(v) => format!("master {v:.2}"),
        DeckAction::Play { deck, on } => format!("{deck} {}", if *on { "play" } else { "pause" }),
        DeckAction::Load { deck, source } => format!("{deck} load {}", source_summary(source)),
        DeckAction::QueueNext { deck, spec } => format!(
            "{deck} next up {}",
            source_summary(&DeckSource::Net(spec.clone()))
        ),
        DeckAction::Lock { key, on } => format!("{} {key}", if *on { "lock" } else { "unlock" }),
        DeckAction::MasterFx { deck, key, value } => format!("{deck} fx {key} {value:.2}"),
        DeckAction::Focus { deck } => format!("focus {deck}"),
        DeckAction::Fx { deck, param, value } => format!("{deck} fx unit {param} {value:.2}"),
        DeckAction::FxReset { deck, on_beat } => {
            format!(
                "{deck} fx unit reset{}",
                if *on_beat { " on the beat" } else { "" }
            )
        }
        DeckAction::FxKill { deck, on, on_beat } => format!(
            "{deck} fx unit {}{}",
            if *on { "killed" } else { "back" },
            if *on_beat { " on the beat" } else { "" }
        ),
        DeckAction::StemMute { deck, stem, on } => {
            format!("{deck} stem {stem} {}", if *on { "muted" } else { "back" })
        }
        DeckAction::AutoMix { on, bars, every } => {
            let m = AutoMixSettings::new(*on, *bars, *every);
            format!(
                "auto-mix {} · {} bars · {}",
                if *on { "on" } else { "off" },
                m.bars,
                if m.every == 0 {
                    "to the outro".to_string()
                } else {
                    format!("every {} phrases", m.every)
                }
            )
        }
        DeckAction::AutoMixNow => "mix now".into(),
        DeckAction::AutoMixDefer { handoff } => format!("auto-mix deferred #{handoff}"),
        DeckAction::ClipRetune { id, voices } => format!(
            "clipRetune {}…{}",
            prefix(id, 12),
            voices
                .iter()
                .map(|(k, v)| format!(" ch{k}={v}"))
                .collect::<String>()
        ),
        _ => {
            // the rest: the op, the deck, then the other fields
            let mut parts: Vec<String> = Vec::new();
            if let Some(d) = j.get("deck").and_then(JsonValue::as_str) {
                parts.push(d.to_string());
            }
            parts.push(op);
            if let Some(o) = j.as_object() {
                for (k, v) in o.iter() {
                    if k == "op" || k == "deck" {
                        continue;
                    }
                    match v {
                        JsonValue::String(x) => parts.push(format!(
                            "{k}={}",
                            if x.chars().count() > 16 {
                                format!("{}…", prefix(x, 12))
                            } else {
                                x.clone()
                            }
                        )),
                        JsonValue::Number(n) => parts.push(format!("{k}={}", js_number_string(*n))),
                        JsonValue::Bool(b) => parts.push(format!("{k}={b}")),
                        JsonValue::Null => continue,
                        _ => parts.push(format!("{k}=…")),
                    }
                }
            }
            parts.join(" ")
        }
    }
}

/// `DeckAction.clipIDs`: the row clips an action names.
pub fn clip_ids(a: &DeckAction) -> Vec<String> {
    use DeckAction::*;
    match a {
        Clip { id, .. }
        | ClipLoop { id, .. }
        | ClipGain { id, .. }
        | ClipCrop { id, .. }
        | ClipRetune { id, .. }
        | ClipRemove { id }
        | ClipRename { id, .. }
        | ClipColor { id, .. }
        | ClipMute { id, .. }
        | ClipSolo { id, .. }
        | ClipKeyLock { id, .. }
        | ClipFx { id, .. }
        | ClipQuantize { id, .. }
        | ClipFade { id, .. }
        | ClipFollow { id, .. } => vec![id.clone()],
        ClipMove { id, before } => {
            let mut v = vec![id.clone()];
            v.extend(before.clone());
            v
        }
        ClipSequence { steps, replacing } => {
            let mut v: Vec<String> = steps.iter().map(|s| s.clip_id.clone()).collect();
            v.extend(replacing.clone());
            v
        }
        SceneLaunch { clips, .. } => clips
            .as_ref()
            .map(|c| c.iter().map(|x| x.clip_id.clone()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `JournalEntry`: one line of the journal.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalEntry {
    /// Line number in its file, from 0 (the checkpoint).
    pub seq: i64,
    /// Wall clock when it was recorded (Unix ms): metadata only.
    pub time: i64,
    /// Engine frame it was applied on: the replay timebase.
    pub frame: i64,
    pub event: JournalEvent,
}

pub const VERSION: i64 = 1;

impl JournalEntry {
    pub fn json(&self) -> JsonValue {
        let mut o = JsonObject::from_pairs([
            ("v", JsonValue::int(VERSION)),
            ("seq", JsonValue::int(self.seq)),
            ("t", JsonValue::String(time::string(self.time))),
            ("f", JsonValue::int(self.frame)),
            ("k", JsonValue::str(self.event.kind())),
        ]);
        for (k, v) in self.event.fields() {
            o.set(k, v);
        }
        JsonValue::Object(o)
    }

    /// The line's text (canonical JSON: sorted keys, no newline).
    pub fn line(&self) -> String {
        self.json().canonical_js()
    }

    pub fn from_json(v: &JsonValue) -> Option<JournalEntry> {
        let seq = v.get("seq")?.as_i64()?;
        let frame = v.get("f")?.as_i64()?;
        let kind = v.get("k")?.as_str()?;
        let event = JournalEvent::from_kind(kind, v)?;
        Some(JournalEntry {
            seq,
            time: v
                .get("t")
                .and_then(JsonValue::as_str)
                .and_then(time::date)
                .unwrap_or(time::DISTANT_PAST),
            frame,
            event,
        })
    }

    pub fn from_line(line: &str) -> Option<JournalEntry> {
        Self::from_json(&JsonValue::parse(line).ok()?)
    }
}
