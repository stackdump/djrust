//! The row's clips: taking one off and putting it back, naming and colouring, cropping, joining
//! into a sequence, importing outside audio, putting a clip on deck A or B, tracing it back to
//! its track, and where each LOOP take records what the deck was (`LoopClipEdits.swift`,
//! `LoopRecorder.swift`, `LoopProvenance.swift`).
//!
//! The state half of each is `crate::deck::row::ClipRow`; what is here is the half that
//! touches a voice or a deck: the loop that carries on in phase when its clip is replaced, the
//! deck that stops when its clip leaves the row, the clip loaded onto a platter.
//!
//! The retune render (`ClipRetune.render`) is [`super::retune`]. [`Engine::retune_clip`] takes
//! the render as an argument, as a live caller makes it off the main thread; without one (a
//! replay) it renders here.

use std::collections::BTreeMap;

use crate::control::controller::DeckView;
use crate::deck::beat::{tempo_match_in, PITCH_RANGE};
use crate::deck::clips::{
    clip_origin, crop_clip, used_steps, ClipLaunch, ClipOrigin, ClipSound, LoopClip,
};
use crate::deck::loops::LoopState;
use crate::deck::row::Replaced;
use crate::deck::turntable::{SampleBuffer, TurntableCommand};
use crate::journal::stores::favorite_cid;
use crate::json::{JsonObject, JsonValue};
use crate::session::model::{DeckSource, DEMO_ID};
use crate::session::types::SequenceStep;
use crate::wave::NetMacro;

use super::engine::{Engine, MainDeck};
use super::sample::{PreparedSample, OVERVIEW_COLUMNS};

/// The wall clock as provenance writes it (ISO 8601 "Z", no fractions).
pub(crate) fn iso_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::journal::time::string_seconds(ms)
}

fn s(x: &str) -> JsonValue {
    JsonValue::str(x)
}

impl MainDeck {
    /// What the deck holds, as a session would name it, whatever its transport
    /// (`Deck.playingSource`). A sample's content id is left for `provenance::finish` (hashing a
    /// whole track is not for the press).
    pub fn playing_source(&self) -> Option<DeckSource> {
        if let Some(sample) = &self.sample {
            return Some(DeckSource::Sample {
                id: if sample.is_demo() {
                    DEMO_ID.to_string()
                } else {
                    String::new()
                },
                title: sample.title.clone(),
                grid: sample.grid,
            });
        }
        // a net deck: its spec, else the raw-nets share it plays (`currentSpec`, `currentRawNets`)
        self.net_source()
    }
}

/// What Swift's string interpolation writes for a net macro: the case name (`LoopProvenance.capture`
/// records `"\($0)"`, not `NetMacro.id`).
fn net_macro_case_name(m: NetMacro) -> &'static str {
    match m {
        NetMacro::Drop => "drop",
        NetMacro::Breakdown => "breakdown",
        NetMacro::SoloDrums => "soloDrums",
        NetMacro::Cut => "cut",
        NetMacro::DoubleDrop => "doubleDrop",
        NetMacro::BeatRepeat => "beatRepeat",
        NetMacro::HalfTime => "halfTime",
        NetMacro::TapeStop => "tapeStop",
        NetMacro::TempoAnchor => "tempoAnchor",
    }
}

impl Engine {
    // MARK: where a take came from

    /// The deck as LOOP went down on it, at beat `start_beat` (the take's first downbeat)
    /// (`LoopProvenance.capture`): what it played, how it was set (`current_params`: the strip,
    /// the tempo fader, quantize, the strip reverb's knobs, the FX unit, the walk), which
    /// section, which roles were off and which STEMS groups were muted, the palette and kit it
    /// played from, the macros running.
    pub fn capture_provenance(&self, deck: &str, start_beat: f64) -> JsonObject {
        let Some(d) = self.deck(deck) else {
            return JsonObject::new();
        };
        let net = d.sample.is_none();
        let title = match &d.sample {
            Some(sample) => sample.title.clone(),
            None => d.project.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        };
        let mut o = JsonObject::from_pairs([
            ("deck", s(&d.name)),
            ("recorded", s(&iso_now())),
            ("build", s(&self.loops.build)),
            ("title", s(&title)),
            ("mode", s(if net { "net" } else { "sample" })),
            ("startBeat", JsonValue::Number(start_beat)),
            (
                "bar",
                JsonValue::int((start_beat / 4.0).floor() as i64 + 1),
            ),
        ]);
        if let Some(src) = d.playing_source() {
            o.set("source", src.json());
            if let Some(cid) = favorite_cid(&src) {
                o.set("shareCID", JsonValue::str(cid));
            }
        }
        let mut tempo = JsonObject::from_pairs([("pitch", JsonValue::Number(d.pitch))]);
        let base = match &d.sample {
            Some(sample) => sample.grid.map(|g| g.bpm),
            None => d.project.as_ref().map(|_| d.base_tempo),
        };
        if let Some(b) = base {
            tempo.set("baseBPM", JsonValue::Number(b));
        }
        if let Some(b) = DeckView::effective_bpm(d) {
            tempo.set("bpm", JsonValue::Number(b));
        }
        o.set("tempo", JsonValue::Object(tempo));
        o.set(
            "params",
            self.current_params(deck).unwrap_or_default().json(),
        );
        if net {
            if let Some(i) = self.current_section(deck) {
                o.set(
                    "section",
                    JsonValue::Object(JsonObject::from_pairs([
                        ("index", JsonValue::int(i as i64)),
                        ("name", s(&d.sections[i].name)),
                    ])),
                );
            }
            let off: Vec<JsonValue> = self
                .roles(deck)
                .iter()
                .filter(|r| !self.is_role_audible(deck, r))
                .map(|r| s(r))
                .collect();
            if !off.is_empty() {
                o.set("rolesOff", JsonValue::Array(off));
            }
            let stems: Vec<JsonValue> = d.walk.stems_muted.groups.keys().map(|k| s(k)).collect();
            if !stems.is_empty() {
                o.set("stemsMuted", JsonValue::Array(stems));
            }
            // the sounds it played from: the project's palette (a raw-nets share's too, not only
            // a spec's) and its sampled kit
            if let Some(p) = self.palette(deck) {
                o.set("palette", JsonValue::str(p));
            }
            if let Some(k) = self.kit(deck) {
                o.set("kit", JsonValue::str(k));
            }
            // `"\($0)"` of a net macro is its Swift case name (`beatRepeat`), the master macros
            // are written by id (`sweep-lp`)
            let macros: Vec<JsonValue> = d
                .mirror
                .active_macros()
                .into_iter()
                .map(|m| s(net_macro_case_name(m)))
                .chain(d.mirror.active_master_macros().into_iter().map(|m| s(m.id())))
                .collect();
            if !macros.is_empty() {
                o.set("macros", JsonValue::Array(macros));
            }
        }
        o
    }

    // MARK: taking a clip off the row and back

    /// Take a clip off the row (the audio stays in the sample library: a recorded session may
    /// name it). It is kept in the removed clips with its place and how it sounded, so
    /// [`restore_clip`](Self::restore_clip) can put it back as it was. Any deck holding it
    /// stops (`removeClip`).
    pub fn remove_clip(&mut self, id: &str) {
        if self.clip(id).is_none() {
            return;
        }
        self.stop_loop(id, true, None);
        // a captured loop stays in the capture library but no longer comes back at start
        self.library_unrow(id);
        let Some(removal) = self.loops.row.remove_clip(id) else {
            return;
        };
        self.drop_stretch(id);
        if removal.push_levels {
            self.push_clip_levels();
        }
        self.pause_decks_holding(id);
    }

    /// Put a removed clip back on the row: at `at`, or where it was (clamped to the row's end).
    /// Its level carries on as it was, and so do its mute, solo, key lock, launch boundary,
    /// fade and tempo follow. False if it is not a removed clip.
    pub fn restore_clip(&mut self, id: &str, at: Option<i64>) -> bool {
        use crate::deck::row::Restore;
        let r = self.loops.row.restore_clip(id, at);
        if matches!(r, Restore::Restored { push_levels: true }) {
            self.push_clip_levels(); // the others go quiet again
        }
        r.found()
    }

    /// Forget a removed clip (it is deleted for good from the library): it can no longer be
    /// restored.
    pub fn forget_removed_clip(&mut self, id: &str) {
        self.loops.row.forget_removed_clip(id);
    }

    /// Keep `clip` as taken off the row at `index` (newest first, once each), so
    /// [`restore_clip`](Self::restore_clip) can put it back. The library calls it for clips
    /// removed in an earlier launch; the audio joins the sample library.
    pub fn remember_removed_clip(&mut self, clip: LoopClip, index: i64) {
        self.loops
            .row
            .remember_removed_clip(clip, index, ClipSound::default());
    }

    /// `registerClipAudio`: a clip's audio in the sample library, under its id and its content
    /// id.
    pub fn register_clip_audio(&mut self, c: &LoopClip) {
        self.loops.row.register_clip_audio(c);
    }

    /// Put saved clips back on the row (after any already there), ready to loop
    /// (`restoreClips`).
    pub fn restore_clips(&mut self, clips: &[LoopClip]) {
        self.loops.row.restore_clips(clips);
    }

    /// Give the row's clips the launch boundaries, fades and follows the library saved (ids
    /// not on the row are skipped) (`restoreClipLaunch`).
    pub fn restore_clip_launch(&mut self, saved: &BTreeMap<String, ClipLaunch>) {
        self.loops.row.restore_clip_launch(saved);
        if saved.values().any(|l| l.follow == Some(crate::deck::loops::LoopFollow::Louder)) {
            self.update_dominant_deck();
        }
    }

    /// The launch boundaries, fades and follows of the clips on the row, by id (only clips that
    /// have one): what the library saves (`clipLaunch`).
    pub fn clip_launch(&self) -> BTreeMap<String, ClipLaunch> {
        self.loops.row.clip_launch()
    }

    /// Rename clip `id` (on the row or removed). Blank names are ignored.
    pub fn rename_clip(&mut self, id: &str, title: &str) {
        self.loops.row.rename_clip(id, title);
    }

    /// Give clip `id` a colour from `PALETTE` (None, or a name not in it, clears it).
    pub fn set_clip_color(&mut self, id: &str, color: Option<&str>) {
        self.loops.row.set_clip_color(id, color);
    }

    /// Move clip `id` to just before `before` (the end if None or not on the row).
    pub fn move_clip(&mut self, id: &str, before: Option<&str>) {
        self.loops.row.move_clip(id, before);
    }

    /// What clip `id` came from, if it can be loaded again: a net always (it is composed
    /// again), a sample only while its audio is in the sample library (or it is the demo loop)
    /// (`clipOrigin`).
    pub fn clip_origin(&self, id: &str) -> Option<ClipOrigin> {
        let c = self.clip(id).or_else(|| {
            self.loops
                .row
                .removed
                .iter()
                .find(|r| r.clip.id == id)
                .map(|r| &r.clip)
        })?;
        clip_origin(c, &self.loops.row.sample_library)
    }

    /// Clear the row for a reset (`clearLoopRow`): every loop out now, takes in progress
    /// dropped, and every clip off the row and forgotten (not remembered as removed), with no
    /// level, mute, solo or key lock kept.
    pub fn clear_loop_row(&mut self) {
        for id in self.looping_voice_ids() {
            self.stop_loop(&id, true, None);
        }
        self.loops.takes.clear();
        self.loops.row.clear();
        self.loops.stretch_cache.clear();
        self.push_clip_levels();
        self.sync_takes();
    }

    // MARK: editing the audio

    /// Put `new` where the clip at `i` is: its level, colour and FX carry over, and if it was
    /// looping under the mixer the new one carries on in phase (`replace(at:with:)`).
    fn replace_clip(&mut self, i: usize, new: LoopClip) -> Replaced {
        let id = self.loops.row.clips[i].id.clone();
        let was_looping = matches!(self.loop_state(&id), LoopState::On | LoopState::Starting);
        self.stop_loop(&id, true, None);
        let r = self.loops.row.replace(i, new);
        self.drop_stretch(&r.old);
        if was_looping {
            self.start_loop(&r.new, true);
        }
        r
    }

    /// Crop clip `id` to its beats `from..<to`. The cropped clip takes its place on the row
    /// (and its level, scenes and settings); if it was looping under the mixer, the cropped
    /// one carries on in phase. A deck already playing the original keeps it. Returns the new
    /// clip's id (None if nothing changed) (`cropClip`).
    pub fn crop_clip(&mut self, id: &str, from: i64, to: i64) -> Option<String> {
        let i = self.loops.row.clips.iter().position(|c| c.id == id)?;
        let cropped = crop_clip(&self.loops.row.clips[i], from, to)?;
        let r = self.replace_clip(i, cropped);
        // sequences that used the original use the crop from now on (when they are next rendered)
        self.loops.row.retarget_sequences(&r.old, &r.new);
        Some(r.new)
    }

    /// Retune clip `id`: its bars rendered again with `voices` (MIDI channel to beats
    /// instrument) over the instruments it had. `buffer` is the render when a live caller has
    /// made it off the main thread ([`render_retune`](Self::render_retune)); without one (a
    /// replay) it renders here ([`retune::render`](super::retune::render)). The retuned clip
    /// takes its place as a crop's does. Returns the new clip's id (the old one when the render
    /// is the same audio; None for a clip not on the row, one the render refuses, or an empty
    /// render).
    pub fn retune_clip(
        &mut self,
        id: &str,
        voices: &BTreeMap<i64, String>,
        buffer: Option<SampleBuffer>,
    ) -> Option<String> {
        let i = self.loops.row.clips.iter().position(|c| c.id == id)?;
        let buffer = match buffer {
            Some(b) => b,
            None => self.render_retune(id, voices)?.ok()?.buffer,
        };
        if buffer.length() == 0 {
            return None;
        }
        if buffer.content_id() == id {
            return Some(id.to_string());
        }
        let c = self.loops.row.clips[i].clone();
        let mut retuned = LoopClip::from_buffer(
            &c.title,
            &c.source,
            c.beats,
            c.bpm,
            c.start_beat,
            buffer,
        );
        retuned.provenance = Some(crate::deck::provenance::retune(
            &c,
            voices,
            &self.loops.build,
            &iso_now(),
        ));
        retuned.color = c.color.clone();
        retuned.fx = c.fx.clone();
        let r = self.replace_clip(i, retuned);
        self.loops.row.retarget_sequences(&r.old, &r.new);
        Some(r.new)
    }

    /// Join clips into a sequence: each step's clip `repeats` times, back to back, at the first
    /// clip's tempo (others are resampled to it). With `replacing`, the sequence takes that
    /// clip's place (editing a sequence; it carries on looping in phase); otherwise it goes on
    /// the end of the row. Steps whose clip is gone are left out. A new sequence takes its
    /// steps' FX when they all share the same, an edited one keeps its own. Returns the
    /// sequence's id (None if no step has a clip) (`makeSequence`).
    pub fn make_sequence(&mut self, steps: &[SequenceStep], replacing: Option<&str>) -> Option<String> {
        if used_steps(steps, |id| self.loops.row.clip(id)).is_empty() {
            return None;
        }
        let old = replacing
            .filter(|r| self.clip(r).is_some())
            .map(String::from);
        let was_looping = old
            .as_deref()
            .is_some_and(|o| matches!(self.loop_state(o), LoopState::On | LoopState::Starting));
        if let Some(o) = &old {
            self.stop_loop(o, true, None);
        }
        let made = self
            .loops
            .row
            .make_sequence(steps, replacing, self.sample_rate, &iso_now())?;
        if let Some(r) = &made.replaced {
            self.drop_stretch(&r.old);
            if was_looping {
                self.start_loop(&r.new, true);
            }
        }
        Some(made.id)
    }

    // MARK: audio from outside

    /// Put outside audio (a stock loop) on the row as a clip of `beats` beats, starting on its
    /// first sample, and loop it under the mixer from the next launch boundary, in time with
    /// whatever the loops follow. `bpm` is its tempo; None works it out from the buffer's
    /// length. The clip's id is the audio's content id and its `source` the loop channel. The
    /// same audio twice is one clip: one already on the row is looped again, a removed one is
    /// put back. Returns its id (None for an empty buffer, no beats or a tempo that is not a
    /// positive number) (`importClip`).
    pub fn import_clip(
        &mut self,
        buffer: SampleBuffer,
        title: &str,
        bpm: Option<f64>,
        beats: i64,
        provenance: Option<JsonValue>,
    ) -> Option<String> {
        let (id, push_levels) = self
            .loops
            .row
            .import_clip_levels(buffer, title, bpm, beats, provenance, "L")?;
        if push_levels {
            self.push_clip_levels(); // a removed, soloed clip put back: the others go quiet again
        }
        self.start_loop(&id, false);
        Some(id)
    }

    /// [`import_clip`](Engine::import_clip) without starting it: the clip waits on the row
    /// for its pad (the browser's "to the loop row", docs/BROWSER.md).
    pub fn import_clip_idle(
        &mut self,
        buffer: SampleBuffer,
        title: &str,
        bpm: Option<f64>,
        beats: i64,
        provenance: Option<JsonValue>,
    ) -> Option<String> {
        let (id, push_levels) = self
            .loops
            .row
            .import_clip_levels(buffer, title, bpm, beats, provenance, "L")?;
        if push_levels {
            self.push_clip_levels();
        }
        Some(id)
    }

    // MARK: a clip onto a deck

    /// Beyond Swift (the hub, docs/CONTROL.md `loops to_deck`): a clip of the row onto deck
    /// `deck` (A, B, or L to loop it), by `id`, else by row `slot`, else the newest clip on the
    /// row (the take just made), through [`perform`](Engine::perform) as a
    /// `DeckAction::Clip`, so it is recorded, journaled and in the action log. Returns the
    /// clip's id.
    pub fn clip_to_deck(
        &mut self,
        id: Option<&str>,
        slot: Option<usize>,
        deck: &str,
    ) -> Result<String, String> {
        let row = &self.loops.row.clips;
        let id = match (id, slot) {
            (Some(i), _) => row
                .iter()
                .find(|c| c.id == i)
                .map(|c| c.id.clone())
                .ok_or_else(|| format!("no clip {i:?} on the loop row"))?,
            (None, Some(k)) => row
                .get(k)
                .map(|c| c.id.clone())
                .ok_or_else(|| format!("no clip in slot {k} (the row holds {})", row.len()))?,
            (None, None) => row
                .last()
                .map(|c| c.id.clone())
                .ok_or("the loop row is empty")?,
        };
        if deck != "L" && self.deck(deck).is_none() {
            return Err(format!("no deck {deck:?}"));
        }
        self.perform(
            &crate::session::action::DeckAction::Clip {
                deck: deck.to_string(),
                id: id.clone(),
            },
            super::perform::PerformCtx::default(),
        );
        Ok(id)
    }

    /// A loop from the capture library onto deck `deck`: onto the row first if it isn't
    /// there (`library_load`), then [`clip_to_deck`](Self::clip_to_deck).
    pub fn library_to_deck(&mut self, id: &str, deck: &str) -> Result<String, String> {
        if self.clip(id).is_none() && !self.quietly(|e| e.library_load(id)) {
            return Err(format!("no loop {id:?} in the library (or it would not load)"));
        }
        self.clip_to_deck(Some(id), None, deck)
    }

    /// Put clip `id` on `deck`. On "L" it joins the loops playing under the mixer (from the
    /// next boundary, [`set_looping`](Self::set_looping)). On A or B it loads there as a looping
    /// sample with its grid, in time with the other deck, keeping that deck's transport; it is
    /// moved, not copied, so it stops looping under the mixer and on the other deck
    /// (`playClip(_:on:)`).
    ///
    /// The clip's FX come with it: the deck has a chain of its own (`DeckRenderer`), set at the
    /// load and changed with the clip's (`Deck.setClipFx`); anything else loaded there plays dry.
    pub fn play_clip(&mut self, id: &str, deck: &str) {
        let Some(clip) = self.clip(id) else { return };
        if deck == "L" {
            return self.set_looping(id, true);
        }
        if self.deck(deck).is_none() {
            return;
        }
        let (buffer, title, grid, clip_bpm, start_beat, beats, clip_fx) = (
            clip.buffer.clone(),
            clip.title.clone(),
            clip.grid(),
            clip.bpm,
            clip.start_beat,
            clip.beats as f64,
            clip.fx.clone(),
        );
        self.stop_loop(id, true, None);
        let others: Vec<String> = self
            .decks
            .iter()
            .filter(|d| d.name != deck && d.clip_id.as_deref() == Some(id) && d.mirror.is_playing())
            .map(|d| d.name.clone())
            .collect();
        for o in others {
            self.pause(&o);
        }
        let was_playing = self.deck_playing(deck);
        let mut names: Vec<&MainDeck> = self.decks.iter().collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        let reference = names
            .into_iter()
            .find(|d| {
                d.name != deck
                    && d.mirror.is_playing()
                    && d.clip_id.as_deref() != Some(id)
                    && self.beat_state(&d.name).is_some()
            })
            .map(|d| d.name.clone());
        let ref_view = reference.as_deref().and_then(|r| {
            Some((
                self.beat_state(r)?,
                self.deck(r).and_then(DeckView::effective_bpm)?,
            ))
        });
        // `Deck.loadClip`: a looping sample with its grid, its tempo matched to the reference and
        // its playhead at the point of the loop that lines up with the reference's bar
        let mut prepared = PreparedSample {
            title,
            overview: crate::deck::waveform::WaveformOverview::new(&buffer, OVERVIEW_COLUMNS),
            buffer: buffer.clone(),
            grid: Some(grid),
        };
        prepared.grid = Some(grid);
        if !self.load_sample_with(deck, prepared, false) {
            return;
        }
        let len = buffer.length() as f64;
        if let Some(d) = self.deck_mut(deck) {
            d.clip_id = Some(id.to_string());
        }
        self.set_deck_clip_fx(deck, &clip_fx, true); // its FX come with it
        if let Some(d) = self.deck_mut(deck) {
            if let Some(sd) = d.sample.as_mut() {
                sd.loop_start = Some(0.0);
                sd.loop_end = Some(len);
                sd.looping = true;
            }
        }
        self.turntable(
            deck,
            TurntableCommand::Loop {
                start: 0.0,
                end: len,
            },
        );
        // The tempo fader takes the match (`pitch = (tempoMatch(..).ratio - 1) / pitchRange`, 0
        // with no reference); the momentary nudge is not touched, so `DeckParams.pitch` (the
        // session, the journal checkpoint, `dj_state`) carries it and a jog release leaves it.
        // The fader's travel is the deck's own (Swift's is 8 %): `tempo_match_in` clamps like
        // `sync_to_in` does.
        let range = self.deck(deck).map_or(PITCH_RANGE, |d| d.pitch_range);
        let mut offset = 0.0;
        if let Some((state, target)) = ref_view {
            let m = tempo_match_in(clip_bpm, target, range);
            self.set_pitch(deck, (m.ratio - 1.0) / range);
            let mut b = (state.beat - start_beat) % beats;
            if b < 0.0 {
                b += beats;
            }
            offset = b * len / beats;
        } else {
            self.set_pitch(deck, 0.0);
        }
        if offset > 0.0 {
            self.turntable(deck, TurntableCommand::Seek(offset));
        }
        if was_playing {
            self.turntable(deck, TurntableCommand::Start);
        } else {
            self.pause(deck);
        }
    }

    pub(crate) fn pause_decks_holding(&mut self, id: &str) {
        let holding: Vec<String> = self
            .decks
            .iter()
            .filter(|d| d.clip_id.as_deref() == Some(id))
            .map(|d| d.name.clone())
            .collect();
        for d in holding {
            self.pause(&d);
        }
    }
}
