//! The loop row's bookkeeping without its voices: the clips in order, the clips taken off it,
//! each clip's level and settings, the sample library and the scenes, and the operations on
//! them that `DJEngine` makes (`LoopRecorder.swift`, `LoopClipEdits.swift`, `LoopLaunch.swift`
//! scenes). Everything here is the state half of those methods; the engine half wraps each
//! call in what touches a voice, and the doc of each method says what that is.
//!
//! The pattern for a method that changes a clip a voice may be playing (`replace`, crop,
//! retune, a sequence edit): the host records whether the old clip was looping (`On` or
//! `Starting`), stops its voice now (`stopLoop(id, now: true)`, which still needs the old clip
//! on the row for its clock), calls the method, and if the clip was looping starts the new one
//! at once, in phase (`startLoop(new, atOnce: true)`).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::clips::{
    clip_origin, crop_clip, import_tempo, make_sequence_clip, ClipLaunch, ClipOrigin, ClipSettings,
    ClipSound, LoopClip, RemovedClip, PALETTE, WAVEFORM_COLUMNS,
};
use super::loops::Scenes;
use crate::journal::state::LoopScene;
use super::provenance;
use super::turntable::SampleBuffer;
use super::waveform::WaveformOverview;
use crate::json::JsonValue;
use crate::session::types::{LoopFx, SequenceStep};

/// What `replace(at:with:)` did: clip `old` is gone from the row (or taken over by `new`),
/// whose level, scenes and settings now belong to `new`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replaced {
    pub old: String,
    pub new: String,
}

/// `restoreClip`'s answer: a clip that is not among the removed ones, one that is already on
/// the row (the removed entry is dropped, nothing else changes), or put back; `push_levels`
/// when it was soloed (every voice's level is worked out again: the others go quiet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restore {
    NotRemoved,
    AlreadyOnRow,
    Restored { push_levels: bool },
}

impl Restore {
    /// The `Bool` `restoreClip` returns.
    pub fn found(&self) -> bool {
        !matches!(self, Restore::NotRemoved)
    }
}

/// `removeClip`'s outcome: the index it had; `push_levels` when it was soloed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    pub index: usize,
    pub push_levels: bool,
}

/// `makeSequence`'s outcome: the sequence's id, and, when it took the place of a clip
/// (`replacing`), what that replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MadeSequence {
    pub id: String,
    pub replaced: Option<Replaced>,
}

/// The state of the loop row (`DJEngine`'s `clips`, `removedClips`, `clipGains`, the per-clip
/// settings, `sampleLibrary`, `scenes` and `loopCount`).
#[derive(Clone, Debug, Default)]
pub struct ClipRow {
    pub clips: Vec<LoopClip>,
    /// Newest first.
    pub removed: Vec<RemovedClip>,
    pub settings: ClipSettings,
    /// Each clip's own level, 0...1 (1 when it has none).
    pub gains: BTreeMap<String, f64>,
    /// Audio by id, for a session to name.
    pub sample_library: BTreeMap<String, Arc<SampleBuffer>>,
    pub scenes: Scenes,
    /// How many takes the row has numbered ("A loop 3").
    pub loop_count: i64,
}

impl ClipRow {
    pub fn new() -> ClipRow {
        ClipRow::default()
    }

    /// `clip(_:)` for a clip on the row (the engine's also finds a curated stinger's loop).
    pub fn clip(&self, id: &str) -> Option<&LoopClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// `clipGain`: a clip's level under the mixer, 0...1 (1 by default).
    pub fn clip_gain(&self, id: &str) -> f64 {
        self.gains.get(id).copied().unwrap_or(1.0)
    }

    /// `setClipGain`'s map change (the voice's level, now or `at` a frame, is the host's).
    pub fn set_clip_gain(&mut self, id: &str, g: f64) {
        self.gains.insert(id.to_string(), g);
    }

    /// `registerClipAudio`: a clip's audio in the sample library: under its id, and under its
    /// content id too when that differs (a library clip resampled to this rate keeps the id it
    /// was saved under), so a deck holding it can still be named by its content. A sequence's
    /// id is its own.
    pub fn register_clip_audio(&mut self, c: &LoopClip) {
        self.sample_library.insert(c.id.clone(), c.buffer.clone());
        let content = c.buffer.content_id();
        if content != c.id && !c.id.starts_with("seq-") {
            self.sample_library.insert(content, c.buffer.clone());
        }
    }

    /// `restoreClips`: put saved clips back on the row (after any already there), ready to
    /// loop.
    pub fn restore_clips(&mut self, restored: &[LoopClip]) {
        for c in restored {
            if self.clip(&c.id).is_none() {
                self.register_clip_audio(c);
                self.clips.push(c.clone());
            }
        }
        self.loop_count = self.loop_count.max(self.clips.len() as i64);
    }

    /// `moveClip`: clip `id` to just before `before` (the end if None, or if `before` is not on
    /// the row). `id == before`, or an `id` not on the row, changes nothing.
    pub fn move_clip(&mut self, id: &str, before: Option<&str>) {
        if Some(id) == before {
            return;
        }
        let Some(from) = self.index(id) else { return };
        let c = self.clips.remove(from);
        let to = before
            .and_then(|b| self.index(b))
            .unwrap_or(self.clips.len());
        self.clips.insert(to, c);
    }

    // MARK: taking a clip off the row and back

    /// `removeClip`'s row half: take clip `id` off the row, remembering it with its place and
    /// how it sounded, and forget its settings. The host stops its voice first (now) and
    /// pauses any deck holding the clip afterwards, and pushes the levels when `push_levels`.
    pub fn remove_clip(&mut self, id: &str) -> Option<Removal> {
        let i = self.index(id)?;
        let sound = self.settings.snapshot(id);
        let clip = self.clips.remove(i);
        self.remember_removed_clip(clip, i as i64, sound);
        let push_levels = self.settings.forget(id);
        Some(Removal {
            index: i,
            push_levels,
        })
    }

    /// `rememberRemovedClip(_:at:sound:)`: keep `clip` as taken off the row at `index` (newest
    /// first, once each), so `restore_clip` can put it back. The library calls it for clips
    /// removed in an earlier launch (with the default sound); the audio joins the sample
    /// library.
    pub fn remember_removed_clip(&mut self, clip: LoopClip, index: i64, sound: ClipSound) {
        self.removed.retain(|r| r.clip.id != clip.id);
        self.register_clip_audio(&clip);
        self.removed.insert(0, RemovedClip::new(clip, index, sound));
    }

    /// `restoreClip`: put a removed clip back on the row: at `at`, or where it was (clamped to
    /// the row's end). Its level carries on as it was (the gains are not touched by removal),
    /// and so do its mute, solo, key lock, launch boundary, fade and tempo follow.
    pub fn restore_clip(&mut self, id: &str, at: Option<i64>) -> Restore {
        let Some(k) = self.removed.iter().position(|r| r.clip.id == id) else {
            return Restore::NotRemoved;
        };
        let r = self.removed.remove(k);
        if self.clip(id).is_some() {
            return Restore::AlreadyOnRow;
        }
        self.sample_library
            .insert(id.to_string(), r.clip.buffer.clone());
        let at = at.unwrap_or(r.index).max(0) as usize;
        self.clips.insert(at.min(self.clips.len()), r.clip);
        self.loop_count = self.loop_count.max(self.clips.len() as i64);
        let push_levels = self.settings.restore(id, &r.sound);
        Restore::Restored { push_levels }
    }

    /// `forgetRemovedClip`: a removed clip is deleted for good from the library: it can no
    /// longer be restored.
    pub fn forget_removed_clip(&mut self, id: &str) {
        self.removed.retain(|r| r.clip.id != id);
    }

    // MARK: names and colours

    /// `editClip`: change clip `id`, on the row and among the removed ones.
    pub fn edit_clip(&mut self, id: &str, change: impl Fn(&mut LoopClip)) {
        if let Some(i) = self.index(id) {
            change(&mut self.clips[i]);
        }
        if let Some(r) = self.removed.iter_mut().find(|r| r.clip.id == id) {
            change(&mut r.clip);
        }
    }

    /// `renameClip`: rename clip `id` (on the row or removed). The name is trimmed of white
    /// space and new lines; a blank one is ignored.
    pub fn rename_clip(&mut self, id: &str, title: &str) {
        let t = title.trim_matches(|c: char| c.is_whitespace()).to_string();
        if t.is_empty() {
            return;
        }
        self.edit_clip(id, |c| c.title = t.clone());
    }

    /// `setClipColor`: a colour from [`PALETTE`] (None, or a name not in it, clears it).
    pub fn set_clip_color(&mut self, id: &str, color: Option<&str>) {
        let c = color.filter(|c| PALETTE.contains(c)).map(String::from);
        self.edit_clip(id, |clip| clip.color = c.clone());
    }

    /// `clipOrigin`: what clip `id` (on the row or removed) came from, if it can be loaded
    /// again: a net always (it is composed again), a sample only while its audio is in the
    /// sample library (or it is the demo loop).
    pub fn clip_origin(&self, id: &str) -> Option<ClipOrigin> {
        let c = self
            .clip(id)
            .or_else(|| self.removed.iter().find(|r| r.clip.id == id).map(|r| &r.clip))?;
        clip_origin(c, &self.sample_library)
    }

    // MARK: replacing a clip

    /// `replace(at:with:)`'s row half: put `new` where the clip at `i` is. Its colour and FX
    /// carry over when `new` has none; its level, scenes and settings follow (`clipGains`,
    /// `replaceInScenes`, `carryClipSound`); its audio joins the sample library. If `new`'s id
    /// is already on the row the old entry is removed instead of replaced. (As in Swift: when
    /// `new` has the id of the clip it replaces, that clip is itself the one found on the
    /// row, and so it is removed. `retune_clip` guards the case; an edited sequence that
    /// renders to the same audio does not.) The host's voice work is described on the module.
    pub fn replace(&mut self, i: usize, mut new: LoopClip) -> Replaced {
        if new.color.is_none() {
            new.color = self.clips[i].color.clone();
        }
        if new.fx == LoopFx::default() {
            new.fx = self.clips[i].fx.clone();
        }
        let id = self.clips[i].id.clone();
        self.sample_library
            .insert(new.id.clone(), new.buffer.clone());
        if let Some(g) = self.gains.get(&id).copied() {
            self.gains.insert(new.id.clone(), g);
        }
        self.scenes.replace_clip(&id, &new.id);
        self.settings.carry(&id, &new.id);
        let new_id = new.id.clone();
        if self.clip(&new_id).is_none() {
            self.clips[i] = new;
        } else {
            self.clips.remove(i);
        }
        Replaced { old: id, new: new_id }
    }

    /// Sequences that used `old` use `new` from now on (when they are next rendered).
    pub fn retarget_sequences(&mut self, old: &str, new: &str) {
        for c in &mut self.clips {
            if let Some(steps) = &mut c.steps {
                for s in steps.iter_mut() {
                    if s.clip_id == old {
                        *s = SequenceStep::new(new, s.repeats);
                    }
                }
            }
        }
    }

    /// `cropClip`'s row half: clip `id` cropped to its beats `from..<to`. The cropped clip
    /// takes its place on the row (and its level, scenes and settings); sequences that used
    /// the original use the crop. None if nothing changed (not on the row, or
    /// [`crop_clip`] refuses). A deck already playing the original keeps it.
    pub fn crop_clip(&mut self, id: &str, from: i64, to: i64) -> Option<Replaced> {
        let i = self.index(id)?;
        let cropped = crop_clip(&self.clips[i], from, to)?;
        let r = self.replace(i, cropped);
        self.retarget_sequences(&r.old, &r.new);
        Some(r)
    }

    /// `retuneClip`'s row half, for `buffer` the render (a live caller makes it off the main
    /// thread; a replay renders it where it is asked for): the retuned clip takes the place of
    /// clip `id` as a crop's does (`replace`), with the provenance
    /// [`provenance::retune`] (stamped `recorded`, `build`), and sequences that used it use it.
    /// Returns the new id: the old one when the render is the same audio (nothing changes),
    /// None for a clip not on the row or an empty buffer.
    #[allow(clippy::too_many_arguments)]
    pub fn retune_clip(
        &mut self,
        id: &str,
        voices: &BTreeMap<i64, String>,
        buffer: SampleBuffer,
        build: &str,
        recorded: &str,
    ) -> Option<(String, Option<Replaced>)> {
        let i = self.index(id)?;
        if buffer.length() == 0 {
            return None;
        }
        if buffer.content_id() == id {
            return Some((id.to_string(), None));
        }
        let c = self.clips[i].clone();
        let mut retuned = LoopClip::from_buffer(
            &c.title,
            &c.source,
            c.beats,
            c.bpm,
            c.start_beat,
            buffer,
        );
        retuned.provenance = Some(provenance::retune(&c, voices, build, recorded));
        retuned.color = c.color.clone();
        retuned.fx = c.fx.clone();
        let r = self.replace(i, retuned);
        self.retarget_sequences(&r.old, &r.new);
        Some((r.new.clone(), Some(r)))
    }

    /// `makeSequence`'s row half: `steps` joined into a sequence ([`make_sequence_clip`]). With
    /// `replacing`, the sequence takes that clip's place (editing a sequence) and keeps the FX
    /// it had; otherwise it goes on the end of the row (unless the row has that audio already).
    /// None if no step has a clip.
    pub fn make_sequence(
        &mut self,
        steps: &[SequenceStep],
        replacing: Option<&str>,
        sample_rate: f64,
        recorded: &str,
    ) -> Option<MadeSequence> {
        let mut seq = make_sequence_clip(steps, |id| self.clip(id), sample_rate, recorded)?;
        let id = seq.id.clone();
        if let Some(i) = replacing.and_then(|r| self.index(r)) {
            // an edited sequence keeps its own FX, off included
            seq.fx = self.clips[i].fx.clone();
            let replaced = self.replace(i, seq);
            return Some(MadeSequence {
                id,
                replaced: Some(replaced),
            });
        }
        if self.clip(&id).is_none() {
            self.sample_library
                .insert(id.clone(), seq.buffer.clone());
            self.clips.push(seq);
        }
        Some(MadeSequence { id, replaced: None })
    }

    // MARK: scenes

    /// `saveScene`: the loops playing now (and those on their way in: `looping`), in row order,
    /// with their levels and FX (those that have any on), as scene `name`: a new one on the
    /// end, or the one already called that, overwritten. None for an empty name.
    pub fn save_scene(&mut self, name: &str, looping: &[String]) -> Option<LoopScene> {
        let row: Vec<(String, f64, Option<LoopFx>)> = self
            .clips
            .iter()
            .map(|c| {
                let fx = (c.fx != LoopFx::default()).then(|| c.fx.clone());
                (c.id.clone(), self.clip_gain(&c.id), fx)
            })
            .collect();
        self.scenes.save(name, &row, looping)
    }

    // MARK: audio from outside

    /// `importClip`'s row half: put outside audio on the row as a clip of `beats` beats,
    /// starting on its first sample. `bpm` is its tempo; None works it out from the buffer's
    /// length ([`import_tempo`]). The clip's id is the audio's content id and its `source` the
    /// loop channel's name (`loop_channel`, "L"). The same audio twice is one clip: one already
    /// on the row stays, a removed one is put back. Returns its id (None for an empty buffer,
    /// no beats or a tempo that is not a positive number); the host then starts it
    /// (`startLoop(id)`: at the next launch boundary).
    pub fn import_clip(
        &mut self,
        buffer: SampleBuffer,
        title: &str,
        bpm: Option<f64>,
        beats: i64,
        provenance: Option<JsonValue>,
        loop_channel: &str,
    ) -> Option<String> {
        self.import_clip_levels(buffer, title, bpm, beats, provenance, loop_channel)
            .map(|(id, _)| id)
    }

    /// [`import_clip`](Self::import_clip), and whether putting a removed clip back brought a
    /// solo with it: `restoreClip` then pushes every voice's level again (the others go
    /// quiet), which the host does when this says so.
    pub fn import_clip_levels(
        &mut self,
        buffer: SampleBuffer,
        title: &str,
        bpm: Option<f64>,
        beats: i64,
        provenance: Option<JsonValue>,
        loop_channel: &str,
    ) -> Option<(String, bool)> {
        if beats <= 0 || buffer.length() == 0 {
            return None;
        }
        let tempo = bpm.unwrap_or_else(|| import_tempo(&buffer, beats));
        if !tempo.is_finite() || tempo <= 0.0 {
            return None;
        }
        let id = buffer.content_id();
        let buffer = Arc::new(buffer);
        self.sample_library.insert(id.clone(), buffer.clone());
        let mut push_levels = false;
        if self.clip(&id).is_none() {
            match self.restore_clip(&id, None) {
                Restore::Restored { push_levels: p } => push_levels = p,
                Restore::AlreadyOnRow => {}
                Restore::NotRemoved => {
                    let waveform = WaveformOverview::new(&buffer, WAVEFORM_COLUMNS);
                    let mut c = LoopClip::new(
                        &id,
                        title,
                        loop_channel,
                        beats,
                        tempo,
                        0.0,
                        buffer,
                        waveform,
                    );
                    c.provenance = provenance;
                    self.clips.push(c);
                }
            }
        }
        Some((id, push_levels))
    }

    // MARK: the whole row

    /// `clipLaunch`: the launch boundaries, fades and follows of the clips on the row, by id
    /// (only clips that have one): what the library saves.
    pub fn clip_launch(&self) -> BTreeMap<String, ClipLaunch> {
        self.settings.launch_map(self.clips.iter().map(|c| c.id.as_str()))
    }

    /// `restoreClipLaunch`: give the row's clips the launch settings the library saved (ids not
    /// on the row are skipped). A `Louder` follow also makes the host update the dominant deck.
    pub fn restore_clip_launch(&mut self, saved: &BTreeMap<String, ClipLaunch>) {
        let ids: Vec<String> = self.clips.iter().map(|c| c.id.clone()).collect();
        self.settings
            .restore_launch(saved, &|id| ids.iter().any(|x| x == id));
    }

    /// `clearLoopRow`'s row half (a reset): every clip off the row and forgotten, not
    /// remembered as removed; no level, mute, solo or key lock kept. (The host stops the loops
    /// now, drops the takes in progress and the stretch cache, and pushes the levels.)
    pub fn clear(&mut self) {
        self.clips.clear();
        self.removed.clear();
        self.gains.clear();
        self.settings.clear_for_reset();
        self.loop_count = 0;
    }
}
