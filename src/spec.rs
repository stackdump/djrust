//! What to compose: `TrackSpec` from `DeckGenerator.swift`, the same inputs a beats
//! share envelope carries, plus the `genre:seed[:structure]` shorthand the decks and
//! the CLI use (e.g. `synthwave:1984:standard`).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::gen::composer::compose;
use crate::gen::tables::BeatsTables;
use crate::json::{JsonObject, JsonValue};
use crate::pflow::Project;
use crate::share::Share;
use crate::sound::{apply_project_sound, Installed, PaletteSet, Sounds};
use crate::wander::carry::{spec_palette, with_palette};

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct TrackSpec {
    pub genre: String,
    pub seed: f64,
    /// "minimal" | "standard" | "extended" | …; None = an endless loop. (Not the
    /// string "loop": compose treats any non-empty structure as song mode, and an
    /// envelope that literally says "loop" gets the fallback blueprint.)
    pub structure: Option<String>,
    /// "v1" | "v2".
    pub cohesion: String,
    /// Extra `compose` overrides (traits, bpm, …), merged under the fields above.
    pub extra: JsonObject,
    /// Share-envelope fields beyond the compose params, under their envelope names
    /// ([`TrackSpec::LAYER_KEYS`]): the overrides (`tracks`, `initialMutes`, `fx`, …) and the
    /// arrange overlay directives (`fadeIn`, `drumBreak`, `feelCurve`, `counterMelody`).
    /// Applied to the composed project by [`finish`](TrackSpec::finish), so a deck plays the
    /// project the envelope describes. Empty for a plain genre + seed spec.
    pub layers: JsonObject,
    /// dj-only: a seed of its own for some parts (`bass`, `kick`, `snare`, `hihat`, `melody`,
    /// `arp`), which the musicality pass takes that part's seeded decisions from. Deck /
    /// session state: never in `extra`, `layers` or the share payload, so `compose`, the
    /// share and its CID are the same with or without it. Empty for a plain spec.
    pub part_seeds: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecError {
    Empty,
    BadSeed(String),
    TooManyParts,
}

impl core::fmt::Display for SpecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpecError::Empty => f.write_str("empty spec (want genre[:seed[:structure]])"),
            SpecError::BadSeed(s) => write!(f, "bad seed {s:?}"),
            SpecError::TooManyParts => {
                f.write_str("too many ':' parts (want genre[:seed[:structure]])")
            }
        }
    }
}

impl TrackSpec {
    pub fn new(genre: &str, seed: f64, structure: Option<&str>) -> TrackSpec {
        TrackSpec {
            genre: genre.into(),
            seed,
            structure: structure.map(String::from),
            cohesion: "v2".into(),
            extra: JsonObject::new(),
            layers: JsonObject::new(),
            part_seeds: BTreeMap::new(),
        }
    }

    /// The envelope fields `layers` holds: what `shareFromPayload` puts in `overrides`, and
    /// the overlay directives. (`macroCurve` is a compose param, in `extra`.)
    pub const LAYER_KEYS: [&'static str; 14] = [
        "tracks",
        "fx",
        "feel",
        "autoDj",
        "macrosDisabled",
        "initialMutes",
        "tempo",
        "swing",
        "humanize",
        "note",
        "fadeIn",
        "drumBreak",
        "feelCurve",
        "counterMelody",
    ];

    /// The `tracks` override fields that are the deck's sound choices, not the song's
    /// (`TrackSpec.soundFields`): the one list is `sound::SOUND_FIELDS`, which `carrying_sound`
    /// reads.
    pub const SOUND_FIELDS: [&'static str; 3] = crate::sound::SOUND_FIELDS;

    /// The spec a share envelope describes (`None` for raw-`nets` envelopes and genre-less
    /// ones). Keeps the envelope's overrides and overlay directives in `layers`.
    pub fn from_share(share: &Share) -> Option<TrackSpec> {
        if share.nets.is_some() {
            return None;
        }
        let genre = share.genre.as_deref()?;
        let mut extra = share.params.clone();
        let seed = extra.get("seed").and_then(JsonValue::as_f64).unwrap_or(0.0);
        let structure = extra
            .get("structure")
            .and_then(JsonValue::as_str)
            .map(String::from);
        let cohesion = extra
            .get("cohesion")
            .and_then(JsonValue::as_str)
            .unwrap_or("v1")
            .to_string();
        extra.remove("seed");
        extra.remove("structure");
        extra.remove("cohesion");
        // `shareFromPayload` copied these from the payload as they were, so writing them
        // back under the same names gives an envelope that reads back to the same
        // overrides / overlay.
        let mut layers = share.overrides.clone().unwrap_or_default();
        if let Some(v) = &share.fade_in {
            layers.set("fadeIn", JsonValue::Array(v.clone()));
        }
        if let Some(v) = share.drum_break {
            layers.set("drumBreak", v.into());
        }
        if let Some(v) = &share.feel_curve {
            layers.set("feelCurve", JsonValue::Array(v.clone()));
        }
        if let Some(v) = &share.counter_melody {
            layers.set("counterMelody", JsonValue::Array(v.clone()));
        }
        Some(TrackSpec {
            genre: genre.to_string(),
            seed,
            structure,
            cohesion,
            extra,
            layers,
            part_seeds: BTreeMap::new(),
        })
    }

    /// The browser's layers on a composed project: the model-level overrides, then the
    /// arrange overlay ([`Share::finish_composed`]), then the sounds the tracks' palettes and
    /// role overrides resolve to (`Project.applySound`). A no-op without `layers`, as Swift's
    /// `finish` is (a spec with no `tracks` override has no palette to resolve).
    ///
    /// Swift resolves against the machine it runs on; this asks in [`Sounds::nothing`] (no
    /// SoundFont, bank or sampled instrument installed). A host that knows what is installed
    /// calls [`finish_in`](Self::finish_in).
    pub fn finish(&self, project: &mut Project) {
        if self.layers.is_empty() {
            return;
        }
        self.finish_in(project, Sounds::nothing());
    }

    /// [`finish`](Self::finish) with the host's installed sounds (`TrackSpec.finish` on a Mac
    /// whose SoundFont folder and sampled instruments are `sounds`).
    pub fn finish_in(&self, project: &mut Project, sounds: Sounds) {
        if self.layers.is_empty() {
            return;
        }
        let mut p = self.layers.clone();
        p.set("genre", self.genre.as_str().into());
        Share::from_payload(&JsonValue::Object(p), None).finish_composed(project);
        apply_project_sound(project, self.seed, PaletteSet::shared(), sounds);
    }

    /// A beats share-envelope payload for this spec (so a deck's source is a normal share),
    /// written where `shareFromPayload` reads it: the compose overrides in `extra` under
    /// `traits` (an array `macroCurve` at the top level, as beats writes it), `layers`
    /// under their envelope names. Keys `overrides` sets itself (seed, cohesion, a set
    /// structure) are left out of `traits`. A spec without extras writes exactly what it
    /// always did.
    pub fn payload(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("genre", self.genre.as_str().into());
        o.set("seed", self.seed.into());
        if let Some(structure) = self.structure.as_deref().filter(|s| !s.is_empty()) {
            o.set("structure", structure.into());
        }
        o.set("cohesion", self.cohesion.as_str().into());
        let mut traits = JsonObject::new();
        for (k, v) in self.extra.iter() {
            if k == "seed" || k == "cohesion" {
                continue;
            }
            if k == "structure" && o.contains_key(k) {
                continue;
            }
            if k == "macroCurve" && v.as_array().is_some() {
                o.set(k, v.clone());
            } else {
                traits.set(k, v.clone());
            }
        }
        if !traits.is_empty() {
            o.set("traits", traits.into());
        }
        for (k, v) in self.layers.iter() {
            if !o.contains_key(k) {
                o.set(k, v.clone());
            }
        }
        o.into()
    }

    /// Envelope fields named in [`LAYER_KEYS`](Self::LAYER_KEYS) come back as `layers`;
    /// `traits` (an object) is spread into `extra`; every other non-core field is a compose
    /// override. Payloads written before traits moved under `traits` (sessions recorded
    /// with top-level traits) therefore read back unchanged.
    pub fn from_payload(p: &JsonValue) -> Option<TrackSpec> {
        let genre = p.get("genre")?.as_str()?;
        let seed = p.get("seed")?.as_f64()?;
        let mut extra = JsonObject::new();
        let mut layers = JsonObject::new();
        if let Some(o) = p.as_object() {
            for (k, v) in o.iter() {
                if ["genre", "seed", "structure", "cohesion"].contains(&k) {
                    continue;
                }
                match (k, v) {
                    ("traits", JsonValue::Object(traits)) => {
                        for (tk, tv) in traits.iter() {
                            extra.set(tk, tv.clone());
                        }
                    }
                    _ if Self::LAYER_KEYS.contains(&k) => layers.set(k, v.clone()),
                    _ => extra.set(k, v.clone()),
                }
            }
        }
        Some(TrackSpec {
            genre: genre.to_string(),
            seed,
            structure: p
                .get("structure")
                .and_then(JsonValue::as_str)
                .map(String::from),
            cohesion: p
                .get("cohesion")
                .and_then(JsonValue::as_str)
                .unwrap_or("v2")
                .to_string(),
            extra,
            layers,
            part_seeds: BTreeMap::new(),
        })
    }

    /// `partSeeds` as the session writes it, beside the share (never inside it): `None` when
    /// there are none, so a spec without them writes exactly what it always did.
    pub fn part_seeds_json(&self) -> Option<JsonValue> {
        if self.part_seeds.is_empty() {
            return None;
        }
        // BTreeMap order is byte order, the Swift `keys.sorted()` (ASCII role names).
        Some(
            JsonObject::from_pairs(
                self.part_seeds
                    .iter()
                    .map(|(k, v)| (k.clone(), (*v).into())),
            )
            .into(),
        )
    }

    /// This spec with the part seeds `v` holds (a session's `partSeeds` object).
    pub fn with_part_seeds(&self, v: Option<&JsonValue>) -> TrackSpec {
        let mut s = self.clone();
        if let Some(o) = v.and_then(JsonValue::as_object) {
            for (k, x) in o.iter() {
                if let Some(seed) = x.as_f64() {
                    s.part_seeds.insert(k.to_string(), seed);
                }
            }
        }
        s
    }

    /// A groove family the spec plays in place of its genre's (`extra["grooveFamily"]`, a
    /// `BeatsTables::family_of` index): a deck's feel lock. None: the genre's own.
    pub fn groove_family(&self) -> Option<i64> {
        self.extra.get("grooveFamily").and_then(JsonValue::as_i64)
    }

    /// A key offset the spec pins (`extra["keyOffset"]`, semitones from the genre's root),
    /// set by a key-locked Wander.
    pub fn pinned_key_offset(&self) -> Option<i64> {
        self.extra.get("keyOffset").and_then(JsonValue::as_i64)
    }

    /// The next version of this song for auto-next: same genre, structure, cohesion and
    /// settings, a new seed worked out from this one (so a recorded set replays the same
    /// chain), the deck's sound choices ([`carrying_sound`](Self::carrying_sound)) and none
    /// of this song's other per-track overrides.
    pub fn next_version(&self) -> TrackSpec {
        // a Park–Miller step over the 1…999 999 seed range the Generate panel uses. Swift's
        // `seed.rounded()` is toNearestOrAwayFromZero: -2.5 -> -3 and 0.5 -> 1 (not `js_round`'s
        // ties toward +inf, which would give -2).
        let s = (libm::round(self.seed) as i64).wrapping_mul(48_271) % 2_147_483_647;
        let mut next = TrackSpec::new(&self.genre, 0.0, None);
        next.seed = (s.abs() % 999_999 + 1) as f64;
        next.structure = self.structure.clone();
        next.cohesion = self.cohesion.clone();
        next.extra = self.extra.clone();
        next.carrying_sound(self)
    }

    /// This spec with `other`'s sound choices (its palette, kit pick and role overrides) on
    /// the same channels (beats gives every role the same channel in every genre), merged
    /// into any override this spec has. What the deck carries into a new track.
    pub fn carrying_sound(&self, other: &TrackSpec) -> TrackSpec {
        let Some(tracks) = other.layers.get("tracks").and_then(JsonValue::as_object) else {
            return self.clone();
        };
        let mut s = self.clone();
        let mut all = s
            .layers
            .get("tracks")
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        for (ch, o) in tracks.iter() {
            let mut entry = all
                .get(ch)
                .and_then(JsonValue::as_object)
                .cloned()
                .unwrap_or_default();
            let mut changed = false;
            for f in Self::SOUND_FIELDS {
                if let Some(v) = o.get(f) {
                    entry.set(f, v.clone());
                    changed = true;
                }
            }
            if changed {
                all.set(ch, entry.into());
            }
        }
        if !all.is_empty() {
            s.layers.set("tracks", all.into());
        }
        s
    }

    /// The `tracks` fields a deck carries into the next track it plays (auto-next, a queued
    /// next): the sound choices ([`SOUND_FIELDS`](Self::SOUND_FIELDS)) and the older per-channel
    /// instrument pick.
    pub const DECK_SOUND_FIELDS: [&'static str; 4] = ["palette", "kit", "voice", "instrument"];

    /// dj-only (no Swift counterpart): this spec with the sound `deck` (the spec the deck plays)
    /// was given, where this spec does not pin its own. Unlike
    /// [`carrying_sound`](Self::carrying_sound), a field this spec's `tracks` override already
    /// sets on a channel wins, and the instrument pick rides along too. What the deck's next
    /// track (auto-next, a queued next) is composed with, so the sound the performer chose
    /// survives the hand-over.
    pub fn carrying_deck_sound(&self, deck: &TrackSpec) -> TrackSpec {
        let Some(theirs) = deck.layers.get("tracks").and_then(JsonValue::as_object) else {
            return self.clone();
        };
        let mut all = self
            .layers
            .get("tracks")
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        let mut any = false;
        for (ch, o) in theirs.iter() {
            let Some(src) = o.as_object() else { continue };
            let mut entry = all
                .get(ch)
                .and_then(JsonValue::as_object)
                .cloned()
                .unwrap_or_default();
            let mut changed = false;
            for f in Self::DECK_SOUND_FIELDS {
                if entry.get(f).is_none() {
                    if let Some(v) = src.get(f) {
                        entry.set(f, v.clone());
                        changed = true;
                    }
                }
            }
            if changed {
                all.set(ch, entry.into());
                any = true;
            }
        }
        if !any {
            return self.clone();
        }
        let mut s = self.clone();
        s.layers.set("tracks", all.into());
        s
    }

    /// Every channel beats puts a playing role on in `genre` (the melody's and bass's from
    /// the genre, the rest fixed): where a palette is written for a track not composed yet.
    pub fn sound_channels(genre: &str) -> Vec<i64> {
        let tables = BeatsTables::shared();
        let g = tables.genres.get(genre);
        let mut all: Vec<i64> = alloc::vec![5, 7, 10, 11, 12, 13];
        all.extend(tables.stinger_specs.iter().map(|s| s.channel));
        all.push(g.map(|g| g.melody_channel).unwrap_or(4));
        all.push(g.map(|g| g.bass_channel).unwrap_or(6));
        all.sort_unstable();
        all.dedup();
        all
    }

    /// `genre[:seed[:structure]]`. A missing seed is 0; the structure is kept as
    /// written (including "loop", which compose treats as a song, as in Swift).
    pub fn parse(s: &str) -> Result<TrackSpec, SpecError> {
        let mut parts = s.split(':');
        let genre = parts
            .next()
            .filter(|g| !g.is_empty())
            .ok_or(SpecError::Empty)?;
        let seed = match parts.next() {
            Some(x) => x
                .parse::<f64>()
                .map_err(|_| SpecError::BadSeed(x.to_string()))?,
            None => 0.0,
        };
        let structure = parts.next().filter(|s| !s.is_empty());
        if parts.next().is_some() {
            return Err(SpecError::TooManyParts);
        }
        Ok(TrackSpec::new(genre, seed, structure))
    }

    /// `compose`'s overrides for this spec.
    pub fn overrides(&self) -> JsonValue {
        let mut o = self.extra.clone();
        o.set("seed", self.seed.into());
        o.set("cohesion", self.cohesion.as_str().into());
        if let Some(s) = self.structure.as_deref().filter(|s| !s.is_empty()) {
            o.set("structure", s.into());
        }
        o.into()
    }

    /// `compose` plus [`finish`](Self::finish): the project this spec plays.
    pub fn project(&self) -> Project {
        let mut p = compose(&self.genre, &self.overrides());
        self.finish(&mut p);
        p
    }

    /// `compose` plus [`finish_in`](Self::finish_in): the project this spec plays on a host
    /// with these sounds installed.
    pub fn project_in(&self, sounds: Sounds) -> Project {
        let mut p = compose(&self.genre, &self.overrides());
        self.finish_in(&mut p, sounds);
        p
    }

    /// The palette the spec names (on any channel), as recorded, or None (`TrackSpec.palette`).
    pub fn palette(&self) -> Option<String> {
        spec_palette(self)
    }

    /// This spec playing from `palette` (None or the default = none) on every role's channel
    /// (beats' channels: the same in every genre). `installed` is what the palette is recorded
    /// against (`SoundPalette.normalized`: the Minecraft bank's asset version).
    /// `TrackSpec.withPalette`.
    pub fn with_palette(&self, palette: Option<&str>, installed: &Installed) -> TrackSpec {
        let id = PaletteSet::shared().normalized(palette, installed);
        let mut s = self.clone();
        with_palette(&mut s, id.as_deref(), &Self::sound_channels(&self.genre));
        s
    }
}
