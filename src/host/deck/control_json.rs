//! What a control surface (djapp's socket, `djctl app-mcp`) reads back about the parts of the
//! engine that `DeckAction`s change but the plain deck / loop listings do not show: the loop
//! row in full (each clip's launch settings, key lock, FX, sequence steps, scenes, removed
//! clips), a deck's sound (kit, palette, role voices, the macros in effect, the walk, sample
//! auto-gain) and the walk on every deck. Read-only and main-thread; the writes are the
//! `DeckAction`s themselves (`Engine::perform`), so they are recorded as the window's are.
//!
//! [`loop_op`] / [`wander_op`] / [`auto_mix_op`] name the `DeckAction` op behind each socket
//! shortcut, and [`action_for`] builds that action from the shortcut's params.

use crate::json::{JsonObject, JsonValue};
use crate::session::types::{scene_clips_json, sequence_steps_json};

use super::engine::Engine;

fn obj<const N: usize>(pairs: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(pairs))
}

fn opt_i(v: Option<i64>) -> JsonValue {
    v.map_or(JsonValue::Null, JsonValue::int)
}

fn opt_s(v: Option<&str>) -> JsonValue {
    v.map_or(JsonValue::Null, JsonValue::str)
}

fn opt_b(v: Option<bool>) -> JsonValue {
    v.map_or(JsonValue::Null, JsonValue::Bool)
}

/// The loop-row shortcuts and the `DeckAction` op each one is (the params pass through).
pub const LOOP_OPS: [(&str, &str); 22] = [
    ("scene_save", "sceneSave"),
    ("scene_launch", "sceneLaunch"),
    ("scene_rename", "sceneRename"),
    ("scene_delete", "sceneDelete"),
    ("crop", "clipCrop"),
    ("sequence", "clipSequence"),
    ("retune", "clipRetune"),
    ("fx", "clipFx"),
    ("fx_link", "loopFxLink"),
    ("key_lock", "clipKeyLock"),
    ("row_key_lock", "loopKeyLock"),
    ("quantize", "clipQuantize"),
    ("row_quantize", "loopQuantize"),
    ("fade", "clipFade"),
    ("row_fade", "loopFade"),
    ("follow", "clipFollow"),
    ("row_follow", "loopFollow"),
    ("gain", "clipGain"),
    ("rename", "clipRename"),
    ("color", "clipColor"),
    ("restore", "clipRestore"),
    ("forget", "clipForget"),
];

/// The walk's shortcuts (`wander` socket method).
pub const WANDER_OPS: [(&str, &str); 4] = [
    ("on", "wander"),
    ("off", "wander"),
    ("pace", "wanderPace"),
    ("lock", "wanderLock"),
];

/// Auto-mix's shortcuts (`automix` socket method).
pub const AUTO_MIX_OPS: [(&str, &str); 4] = [
    ("set", "autoMix"),
    ("off", "autoMix"),
    ("now", "autoMixNow"),
    ("defer", "autoMixDefer"),
];

fn find(table: &[(&str, &'static str)], action: &str) -> Option<&'static str> {
    table.iter().find(|(a, _)| *a == action).map(|(_, op)| *op)
}

pub fn loop_op(action: &str) -> Option<&'static str> {
    find(&LOOP_OPS, action)
}

pub fn wander_op(action: &str) -> Option<&'static str> {
    find(&WANDER_OPS, action)
}

pub fn auto_mix_op(action: &str) -> Option<&'static str> {
    find(&AUTO_MIX_OPS, action)
}

/// The `DeckAction` JSON for shortcut `action` (op `op`): the params with `action` dropped and
/// `op` set, and `on` forced for the shortcuts that name it (`on` / `off`).
pub fn action_for(action: &str, op: &str, params: &JsonValue) -> JsonValue {
    let mut o = match params {
        JsonValue::Object(o) => o.clone(),
        _ => JsonObject::new(),
    };
    o.remove("action");
    o.set("op", JsonValue::str(op));
    match action {
        "on" => o.set("on", JsonValue::Bool(true)),
        "off" => o.set("on", JsonValue::Bool(false)),
        _ => {}
    }
    JsonValue::Object(o)
}

impl Engine {
    /// The loop row in full: the row's own settings, every clip with its launch settings
    /// (None = follows the row), key lock, FX, colour, sequence steps and state, the scenes and
    /// the clips taken off this run.
    pub fn loop_row_json(&self) -> JsonValue {
        let checks = self.clip_checkpoints();
        let clips = self
            .loops
            .row
            .clips
            .iter()
            .zip(checks.iter())
            .enumerate()
            .map(|(k, (c, ck))| {
                obj([
                    ("slot", JsonValue::int(k as i64)),
                    ("id", JsonValue::str(c.id.as_str())),
                    ("title", JsonValue::str(c.title.as_str())),
                    ("color", opt_s(c.color.as_deref())),
                    ("source", JsonValue::str(c.source.as_str())),
                    ("beats", JsonValue::int(c.beats)),
                    ("bpm", JsonValue::Number(c.bpm)),
                    ("state", JsonValue::str(format!("{:?}", self.loop_state(&c.id)))),
                    ("looping", JsonValue::Bool(ck.looping)),
                    ("gain", JsonValue::Number(ck.gain)),
                    ("muted", JsonValue::Bool(ck.muted)),
                    ("soloed", JsonValue::Bool(ck.soloed)),
                    ("key_lock", opt_b(ck.key_lock)),
                    ("quantum", opt_i(ck.quantum)),
                    ("fade", opt_s(ck.fade.as_deref())),
                    ("follow", opt_s(ck.follow.as_deref())),
                    ("fx", ck.fx.json()),
                    (
                        "steps",
                        c.steps
                            .as_ref()
                            .map_or(JsonValue::Null, |s| sequence_steps_json(s)),
                    ),
                    ("reloadable", JsonValue::Bool(self.clip_origin(&c.id).is_some())),
                ])
            })
            .collect();
        let scenes = self
            .scenes()
            .iter()
            .map(|s| {
                obj([
                    ("name", JsonValue::str(s.name.as_str())),
                    ("clips", scene_clips_json(&s.clips)),
                ])
            })
            .collect();
        let removed = self
            .loops
            .row
            .removed
            .iter()
            .map(|r| {
                obj([
                    ("id", JsonValue::str(r.clip.id.as_str())),
                    ("title", JsonValue::str(r.clip.title.as_str())),
                    ("index", JsonValue::int(r.index)),
                ])
            })
            .collect();
        obj([
            (
                "row",
                obj([
                    ("quantum", JsonValue::int(self.loop_quantum())),
                    ("fade", JsonValue::str(self.loop_fade().raw())),
                    ("follow", JsonValue::str(self.loop_follow().raw())),
                    ("key_lock", JsonValue::Bool(self.loop_key_lock())),
                    ("fx_link", JsonValue::Bool(self.loop_fx_link())),
                    ("clock_bpm", self.loop_clock_bpm().map_or(JsonValue::Null, JsonValue::Number)),
                ]),
            ),
            ("clips", JsonValue::Array(clips)),
            ("scenes", JsonValue::Array(scenes)),
            ("removed", JsonValue::Array(removed)),
        ])
    }

    /// A deck's sound and helpers beyond the transport: kit, palette, role voices, the net
    /// macros in effect (with the roles each holds), the walk, and a sample deck's auto-gain.
    /// Null for no such deck.
    pub fn deck_sound_json(&self, deck: &str) -> JsonValue {
        if self.deck(deck).is_none() {
            return JsonValue::Null;
        }
        let voices = self
            .role_voices(deck)
            .iter()
            .map(|v| {
                obj([
                    ("role", JsonValue::str(v.role.as_str())),
                    ("channel", JsonValue::int(v.channel)),
                    ("instrument", JsonValue::str(v.instrument.as_str())),
                ])
            })
            .collect();
        let act = self.macro_activity(deck);
        let macros = act
            .running
            .iter()
            .map(|m| {
                obj([
                    ("id", JsonValue::str(m.id())),
                    (
                        "holds",
                        JsonValue::Array(
                            act.targets
                                .get(m)
                                .map(|r| r.iter().map(|x| JsonValue::str(x.as_str())).collect())
                                .unwrap_or_default(),
                        ),
                    ),
                ])
            })
            .collect();
        obj([
            ("kit", opt_s(self.kit(deck).as_deref())),
            ("palette", opt_s(self.palette(deck).as_deref())),
            ("voices", JsonValue::Array(voices)),
            ("macros", JsonValue::Array(macros)),
            ("wander", self.wander_json(deck).unwrap_or(JsonValue::Null)),
            ("auto_gain", JsonValue::Bool(self.auto_gain(deck))),
            ("auto_gain_db", JsonValue::Number(self.auto_gain_db(deck))),
        ])
    }

    /// The walk on every deck, by name (`Deck.wanderJSON` each).
    pub fn wander_all_json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs(self.decks.iter().map(|d| {
            (
                d.name.clone(),
                self.wander_json(&d.name).unwrap_or(JsonValue::Null),
            )
        })))
    }

    /// Auto-mix as `dj_state` shows it, with the mixer's one-line status.
    pub fn auto_mix_view_json(&self) -> JsonValue {
        obj([
            ("state", self.auto_mix_json()),
            ("status", JsonValue::str(self.auto_mix_status())),
        ])
    }
}
