//! Capture to loop (docs/INPUT-HISTORY.md): a piece of a live input becomes a clip on the loop
//! row, and is saved to the loop library so it is there after a restart.
//!
//! Two ways in, both quantised to whole beats at the app tempo:
//!
//! - **Grab** ([`Engine::capture_grab`]): the last N beats already in the input's recording, ending
//!   on the latest bar (N a multiple of 4) or beat of the master clock when one is running, else
//!   ending now. Instant.
//! - **Arm** ([`Engine::capture_start`] / [`Engine::capture_end`]): the capture starts on the next
//!   bar of the master clock (at once with none), runs while held, and ends on the bar after the
//!   release, like LOOP record on a deck. A quick tap of the same control grabs instead, so one
//!   button does both.
//!
//! What is captured is the strip's recording (after the DC block and the trim, before the strip's
//! EQ and fader), the same signal as the history. A clip keeps the tempo it was captured at and
//! follows the master tempo from there, as every clip does.
//!
//! The main thread only: it reads the strip's [`InputTap`](super::input::InputTap).

use std::sync::atomic::Ordering;

use crate::deck::turntable::SampleBuffer;
use crate::history;
use crate::looplib::{
    capture_frames, frames_since_boundary, frames_to_boundary, held_bars, quantize_beats,
    start_beat_of, LoopMeta, MAX_CAPTURE_BEATS,
};

use super::engine::Engine;
use super::loop_pads::ClipSpec;

/// A quick tap of the arm control, seconds.
const TAP_SECONDS: f64 = 0.25;

/// A live capture in progress on one input.
pub(crate) struct InputTake {
    input: usize,
    label: String,
    /// First frame of the recording it takes, and the recording's end when it was armed.
    start: i64,
    pressed: i64,
    /// The master beat of `start` when it is on the master clock.
    start_beat: Option<f64>,
    bar_frames: f64,
    released: Option<i64>,
    end: Option<i64>,
    bars: i64,
    cursor: i64,
    audio: Vec<f32>,
}

impl Engine {
    /// The input a capture control means: `label` if given, else the one a deck's jog is aimed
    /// at, else the first strip with a recording.
    pub fn capture_input(&self, label: Option<&str>) -> Option<usize> {
        let has = |i: usize| self.inputs[i].mirror.tap.get().is_some();
        if let Some(l) = label {
            return self.input_index(l).filter(|i| has(*i));
        }
        if let Some((_, super::strip_recall::RecallId::Input(i))) = &self.history_jog {
            if has(*i) {
                return Some(*i);
            }
        }
        (0..self.inputs.len()).find(|i| has(*i))
    }

    /// The tempo and frames per beat a capture is measured in: the master clock's when one runs,
    /// else the master tempo.
    pub(crate) fn capture_clock(&self) -> (Option<f64>, f64) {
        match self.clock_beat_now() {
            Some((beat, fpb)) => (Some(beat), fpb),
            None => {
                let (_, bpm) = self.master_tempo();
                (None, history::frames_per_beat(bpm, self.sample_rate))
            }
        }
    }

    /// Take the last `beats` (None: the strip's loop length; snapped to whole beats) of input
    /// `i`'s recording onto the loop row, looping at once. Returns the clip's id, or None when
    /// the strip has no recording or not that much yet.
    pub fn capture_grab(&mut self, i: usize, beats: Option<f64>) -> Option<String> {
        let (label, n_beats, start_beat, audio) = self.grab_input_audio(i, beats)?;
        let id = self.finish_capture(&label, n_beats, start_beat, audio, None);
        self.note_capture(i, "grab", Some(n_beats));
        Some(id)
    }

    /// The audio a [`capture_grab`](Self::capture_grab) takes, without putting it anywhere:
    /// (the input's label, whole beats, the start beat on the master clock, the mono audio).
    /// The vocoder's capture modulator reads it too.
    pub(crate) fn grab_input_audio(
        &self,
        i: usize,
        beats: Option<f64>,
    ) -> Option<(String, i64, Option<f64>, Vec<f32>)> {
        let inp = self.inputs.get(i)?;
        let tap = inp.mirror.tap.get()?;
        let n_beats = quantize_beats(beats.unwrap_or(inp.loop_beats));
        let label = inp.settings.label.clone();
        let (beat, fpb) = self.capture_clock();
        // whole bars end on a bar line, anything else on a beat
        let q = if n_beats % 4 == 0 { 4 } else { 1 };
        let back = beat.map_or(0, |b| frames_since_boundary(b, q, fpb));
        let frames = capture_frames(n_beats, 60.0 * self.sample_rate / fpb, self.sample_rate);
        // the audio made at the bar line reaches the recording `lat` later; a grab that would
        // end past the recording's end is held back to it
        let lat = self.input_latency_frames(i);
        let end = (tap.end() - back + lat).min(tap.end());
        let start = end - frames as i64;
        if start < tap.oldest() {
            return None;
        }
        let mut audio = Vec::with_capacity(frames);
        if tap.copy(start, end, &mut audio) != end || audio.len() != frames {
            return None;
        }
        let start_beat = beat.map(|b| start_beat_of(b, q, n_beats));
        Some((label, n_beats, start_beat, audio))
    }

    /// Arm a live capture on input `i`: it starts on the next bar of the master clock (now, with
    /// none). False when the strip has no recording or one is already running.
    pub fn capture_start(&mut self, i: usize) -> bool {
        let Some(inp) = self.inputs.get(i) else {
            return false;
        };
        let Some(tap) = inp.mirror.tap.get() else {
            return false;
        };
        if self.input_takes.iter().any(|t| t.input == i) {
            return false;
        }
        let label = inp.settings.label.clone();
        let now = tap.end();
        let lat = self.input_latency_frames(i);
        let (beat, fpb) = self.capture_clock();
        let (start, start_beat) = match beat {
            Some(b) => {
                let ahead = frames_to_boundary(b, 4, fpb);
                let next = if ahead == 0 { b } else { (b / 4.0).floor() * 4.0 + 4.0 };
                (now + ahead + lat, Some(next))
            }
            None => (now + lat.max(0), None),
        };
        self.note_capture(i, "start", None);
        self.input_takes.push(InputTake {
            input: i,
            label,
            start,
            pressed: now,
            start_beat,
            bar_frames: 4.0 * fpb,
            released: None,
            end: None,
            bars: 0,
            cursor: start,
            audio: Vec::new(),
        });
        true
    }

    /// Let go of input `i`'s armed capture: it ends on the bar after the release. With
    /// `tap_grabs`, a release within a quarter second of the press drops the capture and grabs the
    /// last beats instead. True if a capture was ended (or the grab worked).
    pub fn capture_end(&mut self, i: usize, tap_grabs: bool) -> bool {
        self.capture_end_with(i, tap_grabs, None)
    }

    /// [`capture_end`](Self::capture_end), a tap grabbing `tap_beats` (None: the strip's loop
    /// length).
    pub(crate) fn capture_end_with(&mut self, i: usize, tap_grabs: bool, tap_beats: Option<f64>) -> bool {
        let Some(k) = self.input_takes.iter().position(|t| t.input == i) else {
            return false;
        };
        let Some(tap) = self.inputs[i].mirror.tap.get() else {
            return false;
        };
        let lat = self.input_latency_frames(i);
        let now = tap.end();
        let t = &mut self.input_takes[k];
        if t.released.is_some() {
            return false;
        }
        if tap_grabs && (now - t.pressed) as f64 <= TAP_SECONDS * self.sample_rate {
            self.input_takes.remove(k);
            return self.capture_grab(i, tap_beats).is_some();
        }
        // the release is at `now`; the recording of it arrives `lat` later
        let released = now + lat.max(0);
        if released <= t.start {
            // let go before it began
            self.input_takes.remove(k);
            return false;
        }
        t.released = Some(released);
        set_end(t, released);
        self.note_capture(i, "end", None);
        true
    }

    /// Press / release in one call, the shape a controller button has. A tap grabs.
    pub fn capture_arm(&mut self, i: usize, on: bool) -> bool {
        if on {
            self.capture_start(i)
        } else {
            self.capture_end(i, true)
        }
    }

    /// Is a capture armed or running on input `i`? `(bars so far, released)`.
    pub fn capture_progress(&self, i: usize) -> Option<(i64, bool)> {
        let t = self.input_takes.iter().find(|t| t.input == i)?;
        let tap_end = self.inputs[i].mirror.tap.get().map_or(0, |x| x.end());
        let bars = if t.end.is_some() {
            t.bars
        } else {
            held_bars((tap_end - t.start).max(1), t.bar_frames)
        };
        Some((bars, t.released.is_some()))
    }

    /// True while the capture on `i` is armed but its start bar has not come.
    pub fn capture_waiting(&self, i: usize) -> bool {
        self.input_takes.iter().any(|t| {
            t.input == i
                && self.inputs[i]
                    .mirror
                    .tap
                    .get()
                    .is_some_and(|x| x.end() < t.start)
        })
    }

    /// Move live captures on: copy what the strips recorded, end released ones, turn finished
    /// ones into clips. Called from [`Engine::service_loops`].
    pub(crate) fn service_input_takes(&mut self) {
        let mut k = 0;
        while k < self.input_takes.len() {
            let i = self.input_takes[k].input;
            let Some(tap) = self.inputs.get(i).and_then(|x| x.mirror.tap.get()) else {
                self.input_takes.remove(k);
                continue;
            };
            let now = tap.end();
            let oldest = tap.oldest();
            let t = &mut self.input_takes[k];
            // a hold past the longest capture ends on its own
            if t.released.is_none()
                && now > t.start
                && (now - t.start) as f64 >= MAX_CAPTURE_BEATS as f64 / 4.0 * t.bar_frames
            {
                t.released = Some(now);
                set_end(t, now);
            }
            if t.cursor < oldest {
                // the ring ran past it (the service stalled): the capture has a hole
                self.input_takes.remove(k);
                continue;
            }
            let to = t.end.unwrap_or(i64::MAX);
            t.cursor = tap.copy(t.cursor, to, &mut t.audio);
            if t.end.is_some_and(|e| t.cursor >= e) {
                let t = self.input_takes.remove(k);
                let n = (t.end.unwrap_or(t.start) - t.start) as usize;
                let mut audio = t.audio;
                audio.truncate(n);
                if audio.len() == n && n > 0 {
                    self.finish_capture(&t.label, t.bars * 4, t.start_beat, audio, None);
                }
                continue;
            }
            k += 1;
        }
    }

    /// A captured piece onto the row, saved, and looping in phase: mono (an input) or stereo
    /// (`right`: a bus, `V` or `M`). Returns the clip's id.
    pub(crate) fn finish_capture(
        &mut self,
        label: &str,
        beats: i64,
        start_beat: Option<f64>,
        audio: Vec<f32>,
        right: Option<Vec<f32>>,
    ) -> String {
        let id = match &right {
            None => audio_hash(&audio),
            Some(r) => audio_hash(&[audio.as_slice(), r.as_slice()].concat()),
        };
        let bpm = beats as f64 * 60.0 * self.sample_rate / audio.len() as f64;
        let title = format!("{label} {beats}b · {bpm:.0}");
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let mut saved = false;
        if let Some(lib) = &self.capture_library {
            let meta = LoopMeta {
                id: id.clone(),
                title: title.clone(),
                bpm,
                beats,
                sample_rate: self.sample_rate,
                frames: audio.len(),
                channels: if right.is_some() { 2 } else { 1 },
                source: label.to_string(),
                kind: "input".into(),
                created,
                row: true,
                from: String::new(),
            };
            saved = lib.save(&meta, &audio, right.as_deref()).is_ok();
        }
        let buffer = SampleBuffer::new(audio, right, self.sample_rate);
        self.add_clip(
            ClipSpec {
                id: id.clone(),
                title: Some(title),
                source: label.to_string(),
                beats,
                start_beat: start_beat.unwrap_or(0.0),
                from: String::new(),
                from_input: true,
                locked: start_beat.is_some(),
                saved,
            },
            buffer,
        );
        self.start_loop_now(&id);
        id
    }

    // MARK: the library

    /// Point the engine at a loop library (None: captures are not kept).
    pub fn set_loop_library(&mut self, library: Option<crate::host::stores::captures::CaptureLibrary>) {
        self.capture_library = library;
    }

    /// The library's entries, oldest first (the slots of `libraryLoad`).
    pub fn library_entries(&self) -> Vec<LoopMeta> {
        self.capture_library.as_ref().map(|l| l.list()).unwrap_or_default()
    }

    /// Put a saved loop on the row (not playing). Returns false if it is not in the library or
    /// is already on the row (a loop already there that the library holds is marked saved).
    pub fn library_load(&mut self, id: &str) -> bool {
        if self.clip(id).is_some() {
            // Already on the row: the take library puts the row back first, so a loop both
            // libraries hold is here before this one loads it. It is still this library's loop
            // (saved), so a removal takes it off *this* library's row too (`library_unrow`).
            if self.capture_library.as_ref().is_some_and(|l| l.meta(id).is_some()) {
                self.set_clip_saved(id, true);
            }
            return false;
        }
        let Some(lib) = &self.capture_library else {
            return false;
        };
        let Some(saved) = lib.load(id, self.sample_rate) else {
            return false;
        };
        let m = saved.meta.clone();
        if !m.row {
            let mut m2 = m.clone();
            m2.row = true;
            let _ = lib.write_meta(&m2);
        }
        let (id, title) = (m.id.clone(), m.title.clone());
        self.add_clip(
            ClipSpec {
                id: m.id,
                title: Some(m.title),
                source: m.source,
                beats: m.beats,
                start_beat: 0.0,
                from: m.from,
                from_input: m.kind == "input",
                locked: false,
                saved: true,
            },
            saved.buffer,
        );
        self.note_resolved(
            format!("library:{id}"),
            format!("library loop {title} to the row"),
            false,
            "library",
            super::vocoder::live_params([("action", "load".into()), ("id", id.into())]),
        );
        true
    }

    /// Put every saved loop marked for the row onto it (at start). Returns how many.
    pub fn library_load_row(&mut self) -> usize {
        let ids: Vec<String> = self
            .library_entries()
            .into_iter()
            .filter(|m| m.row)
            .map(|m| m.id)
            .collect();
        self.quietly(|e| ids.iter().filter(|id| e.library_load(id)).count())
    }

    /// Save the clip `id` of the row to the library, now. False without a library or clip.
    pub fn library_save(&mut self, id: &str) -> bool {
        let Some(lib) = &self.capture_library else {
            return false;
        };
        let Some(c) = self.loops.row.clips.iter().find(|c| c.id == id) else {
            return false;
        };
        let x = self.loops.pads.extra(id);
        let meta = LoopMeta {
            id: c.id.clone(),
            title: c.title.clone(),
            bpm: c.bpm,
            beats: c.beats,
            sample_rate: self.sample_rate,
            frames: c.buffer.length(),
            channels: if c.buffer.right.is_some() { 2 } else { 1 },
            source: c.source.clone(),
            kind: if x.from_input { "input" } else { "deck" }.into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64),
            row: true,
            from: x.from.clone(),
        };
        let ok = lib
            .save(&meta, &c.buffer.left, c.buffer.right.as_deref())
            .is_ok();
        if ok {
            self.set_clip_saved(id, true);
            self.note_resolved(
                format!("library:{id}"),
                format!("save loop {id} to the library"),
                false,
                "library",
                super::vocoder::live_params([("action", "save".into()), ("id", id.into())]),
            );
        }
        ok
    }

    /// Delete a loop from the library (and take it off the row).
    pub fn library_delete(&mut self, id: &str) -> bool {
        self.loops.row.clips.iter().any(|c| c.id == id).then(|| self.remove_clip(id));
        let ok = self
            .capture_library
            .as_ref()
            .is_some_and(|l| l.delete(id).unwrap_or(false));
        if ok {
            // words only: a replay never deletes from a library
            self.trace_push_text(&format!("delete loop {id} from the library"));
        }
        ok
    }

    /// A clip left the row: it stays in the library but no longer comes back at start.
    pub(crate) fn library_unrow(&mut self, id: &str) {
        let Some(lib) = &self.capture_library else { return };
        if self.clip(id).is_none() || !self.loops.pads.extra(id).saved {
            return;
        }
        if let Some(mut m) = lib.meta(id) {
            if m.row {
                m.row = false;
                let _ = lib.write_meta(&m);
            }
        }
    }

    /// A clip came back on the row: a captured one returns at start again (the other half of
    /// [`library_unrow`](Self::library_unrow)).
    pub(crate) fn library_rerow(&mut self, id: &str) {
        let Some(lib) = &self.capture_library else { return };
        if self.clip(id).is_none() || !self.loops.pads.extra(id).saved {
            return;
        }
        if let Some(mut m) = lib.meta(id) {
            if !m.row {
                m.row = true;
                let _ = lib.write_meta(&m);
            }
        }
    }

    /// Frames of the loop channel mirror, for tests.
    #[doc(hidden)]
    pub fn rendered_frames(&self) -> i64 {
        self.loops.mirror.rendered.load(Ordering::Relaxed)
    }
}

/// Fix a released capture's end: the bar after the release, counted from its start.
fn set_end(t: &mut InputTake, released: i64) {
    t.bars = held_bars((released - t.start).max(1), t.bar_frames);
    t.end = Some(t.start + (t.bars as f64 * t.bar_frames).round() as i64);
}

fn audio_hash(audio: &[f32]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in audio {
        for b in v.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{h:016x}")
}
