//! The hub's pads on the loop row (beyond Swift; docs/HUB.md, docs/MPK.md): keys that transpose
//! the loops, file pads that load a sample onto the row and loop it, one-shots that play once at
//! their own speed with a gain and a choke group, and the clips captured from an input strip
//! ([`super::input_capture`]).
//!
//! What these add to a clip (its root, its transposition, one-shot, gain, choke group, whether it
//! came from an input and is in the capture library) is a [`ClipExtra`] kept beside the row by
//! clip id, so `djengine`'s `LoopClip` stays the Swift one. A clip with no entry is a plain loop.
//! [`super::loops`] reads them where a clip starts and is serviced; the heavy work (a
//! pitch-shifted copy, a decoded file) is done on a worker thread and lands in
//! [`Engine::service_pads`].

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use crate::deck::clips::{LoopClip, WAVEFORM_COLUMNS};
use crate::deck::turntable::{SampleBuffer, TurntableCommand};
use crate::deck::waveform::WaveformOverview;
use crate::musicality::loop_key::{self, LoopKey};

use super::engine::Engine;
use super::loops::{LoopMsg, LoopState, StartOpts, LOOP_LEAD};

/// Shifts kept for reuse. A held key or a quick run over the keys asks for the same few.
const SHIFT_CACHE: usize = 36;

/// What the hub adds to a clip on the row.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipExtra {
    /// Captured from an input strip (the clip's `source` is its label), not a deck.
    pub from_input: bool,
    /// `start_beat` is a beat of the master clock the capture was snapped to, so its first
    /// start is in phase with it (a clip loaded from the library is not: it comes in on the
    /// bar).
    pub locked: bool,
    /// It is in the capture library on disk.
    pub saved: bool,
    /// Where it came from, as the capture library records it (a file's path).
    pub from: String,
    /// The pitch class (0 = C) the audio sits on. Keys transpose relative to it.
    pub root: u8,
    /// Semitones it is transposed by now, as a pitch-shifted copy on the bar grid.
    pub shift: i32,
    /// Semitones it is transposed by in "tape" mode (the speed follows the pitch).
    pub tape: i32,
    /// Plays once from the top instead of looping (a one-shot, a vocal).
    pub once: bool,
    /// Linear gain it plays at (a pad's `gain` arg), 1 unless set.
    pub gain: f32,
    /// Choke group: starting a clip of the group cuts the others in it.
    pub choke: Option<String>,
    /// The root was set by hand (`loopRoot`, `loops root set`): detection leaves it alone.
    pub root_manual: bool,
    /// The key the clip was found to be in, from its file name or its audio (None: not looked
    /// at yet, or a clip with no pitch).
    pub key: Option<LoopKey>,
    /// Where `key` came from: "name" (the file's name says it), "audio" (the chroma), "".
    pub key_from: &'static str,
}

impl Default for ClipExtra {
    fn default() -> Self {
        ClipExtra {
            from_input: false,
            locked: false,
            saved: false,
            from: String::new(),
            root: 0,
            shift: 0,
            tape: 0,
            once: false,
            gain: 1.0,
            choke: None,
            root_manual: false,
            key: None,
            key_from: "",
        }
    }
}

/// What one clip's root is and where it came from (`Engine::loop_root_report`).
#[derive(Clone, Debug, PartialEq)]
pub struct LoopRoot {
    pub id: String,
    pub title: String,
    /// The pitch class keys transpose it from.
    pub root: u8,
    /// Set by a hand: detection leaves it.
    pub manual: bool,
    /// The key found, if one was.
    pub key: Option<LoopKey>,
    /// "name" or "audio" when `key` is set.
    pub from: &'static str,
    pub once: bool,
}

/// When a launch or a transposition lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quant {
    Now,
    Beat,
    Bar,
}

impl Quant {
    pub fn parse(s: Option<&str>, default: Quant) -> Quant {
        match s {
            Some("now") => Quant::Now,
            Some("beat") => Quant::Beat,
            Some("bar") => Quant::Bar,
            _ => default,
        }
    }
}

/// A file pad: what to make of the file and what to do once it has loaded.
#[derive(Clone, Debug)]
pub struct FileLaunch {
    pub path: std::path::PathBuf,
    pub bars: Option<i64>,
    /// The file's own tempo: stretched to the master's when it loads.
    pub bpm: Option<f64>,
    pub once: bool,
    pub quant: Quant,
    /// Linear gain (1: as recorded).
    pub gain: f32,
    /// Choke group: launching it cuts the other clips in the group.
    pub choke: Option<String>,
    /// Play it as soon as it is loaded (a pad was pressed before it was ready).
    pub launch: bool,
}

/// What [`Engine::add_clip`] needs to know about a clip besides its audio.
pub(crate) struct ClipSpec {
    pub id: String,
    pub title: Option<String>,
    pub source: String,
    pub beats: i64,
    pub start_beat: f64,
    pub from: String,
    pub from_input: bool,
    pub locked: bool,
    pub saved: bool,
}

enum Ready {
    Shift(String, i32, Arc<SampleBuffer>),
    File(String, Result<SampleBuffer, String>),
    /// A clip's key, found off the main thread.
    Root(String, Option<LoopKey>),
}

/// The extras by clip id, and the background work: pitch-shifted copies and decoded files are
/// made off the main thread and come back here.
pub(crate) struct Pads {
    extra: BTreeMap<String, ClipExtra>,
    tx: Sender<Ready>,
    rx: Receiver<Ready>,
    /// Transposed copies, by clip id and semitones.
    shifted: BTreeMap<(String, i32), Arc<SampleBuffer>>,
    /// Shifts being computed, so a held key does not start another.
    computing: Vec<(String, i32)>,
    /// What each clip should be shifted to, and when, once its copy is ready.
    wanted: BTreeMap<String, (i32, Quant)>,
    /// Files being decoded, and what to do when they land.
    loading: BTreeMap<String, FileLaunch>,
    /// Look for a clip's key when it lands on the row (docs/MPK.md, "Loop root").
    pub auto_root: bool,
}

impl Pads {
    pub(crate) fn new() -> Pads {
        let (tx, rx) = channel();
        Pads {
            extra: BTreeMap::new(),
            tx,
            rx,
            shifted: BTreeMap::new(),
            computing: Vec::new(),
            wanted: BTreeMap::new(),
            loading: BTreeMap::new(),
            auto_root: true,
        }
    }

    /// Clip `id`'s extras (a plain loop's when it has none).
    pub(crate) fn extra(&self, id: &str) -> ClipExtra {
        self.extra.get(id).cloned().unwrap_or_default()
    }

    fn extra_mut(&mut self, id: &str) -> &mut ClipExtra {
        self.extra.entry(id.to_string()).or_default()
    }

    /// The pitch-shifted copy a transposed clip plays (None: its own audio).
    pub(crate) fn shifted_copy(&self, id: &str, shift: i32) -> Option<Arc<SampleBuffer>> {
        if shift == 0 {
            return None;
        }
        self.shifted.get(&(id.to_string(), shift)).cloned()
    }

    /// A capture's first start has happened: from now on it lines up like any clip.
    pub(crate) fn unlock(&mut self, id: &str) {
        if let Some(x) = self.extra.get_mut(id) {
            x.locked = false;
        }
    }
}

impl Engine {
    /// Clip `id`'s extras (a plain loop's when it has none).
    pub fn clip_extra(&self, id: &str) -> ClipExtra {
        self.loops.pads.extra(id)
    }

    /// Mark clip `id` as saved in the capture library (or not).
    pub(crate) fn set_clip_saved(&mut self, id: &str, saved: bool) {
        self.loops.pads.extra_mut(id).saved = saved;
    }

    /// Put a clip on the row (not playing). A clip with this id already there is kept as it is.
    pub(crate) fn add_clip(&mut self, c: ClipSpec, buffer: SampleBuffer) {
        if self.clip(&c.id).is_some() {
            return;
        }
        self.loops.row.loop_count += 1;
        let bpm = c.beats as f64 * 60.0 * self.sample_rate / buffer.length() as f64;
        let title = c
            .title
            .unwrap_or_else(|| format!("{} loop {}", c.source, self.loops.row.loop_count));
        let waveform = WaveformOverview::new(&buffer, WAVEFORM_COLUMNS);
        let buffer = Arc::new(buffer);
        let clip = LoopClip::new(
            &c.id,
            &title,
            &c.source,
            c.beats.max(1),
            bpm,
            c.start_beat,
            buffer.clone(),
            waveform,
        );
        self.loops.row.sample_library.insert(c.id.clone(), buffer);
        self.loops.row.clips.push(clip);
        let x = self.loops.pads.extra_mut(&c.id);
        x.from_input = c.from_input;
        x.locked = c.locked;
        x.saved = c.saved;
        x.from = c.from;
        if self.loops.pads.auto_root {
            self.find_root(&c.id);
        }
    }

    /// Look for clip `id`'s key unless a hand set its root: the file's name first (sample packs
    /// say "Am" and "F#min"), else its audio on a worker (inline when `deterministic`).
    pub fn find_root(&mut self, id: &str) {
        if self.loops.pads.extra(id).root_manual {
            return;
        }
        let from = self.loops.pads.extra(id).from.clone();
        let stem = std::path::Path::new(&from)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some((root, minor)) = loop_key::key_in_name(&stem) {
            let k = LoopKey {
                root,
                minor,
                correlation: 1.0,
                confidence: 1.0,
                tonal: true,
            };
            self.apply_found_root(id, Some(k), "name");
            return;
        }
        let Some(buffer) = self.clip(id).map(|c| c.buffer.clone()) else {
            return;
        };
        let (sr, id_s) = (buffer.sample_rate, id.to_string());
        let find = move || loop_key::estimate_stereo(&buffer.left, buffer.right.as_deref(), sr);
        if self.loops.deterministic {
            let k = find();
            self.apply_found_root(id, k, "audio");
            return;
        }
        let tx = self.loops.pads.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Ready::Root(id_s, find()));
        });
    }

    /// A key was found for clip `id`: kept, and made its root when it is tonal and no hand set
    /// one (a one-shot keeps C: a vocal phrase is not a loop to transpose).
    fn apply_found_root(&mut self, id: &str, key: Option<LoopKey>, from: &'static str) {
        if self.clip(id).is_none() {
            return;
        }
        let x = self.loops.pads.extra_mut(id);
        x.key = key;
        x.key_from = if key.is_some() { from } else { "" };
        if let Some(k) = key {
            if k.tonal && !x.root_manual && !x.once {
                x.root = k.root;
            }
        }
    }

    /// Look at the audio of the clips `slot` names (the playing loops without) again and make
    /// the key found their root: the manual override is let go. Now, on this thread, and what
    /// each clip answered. A name's key is used when the file has one, as when it landed.
    pub fn detect_loop_roots(&mut self, slot: Option<usize>) -> Vec<(String, Option<LoopKey>)> {
        let ids: Vec<String> = match slot {
            Some(s) => self.clip_id_at(s).into_iter().collect(),
            None => self.loops.row.clips.iter().map(|c| c.id.clone()).collect(),
        };
        let mut out = Vec::new();
        for id in ids {
            self.loops.pads.extra_mut(&id).root_manual = false;
            self.find_root_now(&id);
            out.push((id.clone(), self.loops.pads.extra(&id).key));
        }
        out
    }

    fn find_root_now(&mut self, id: &str) {
        let was = std::mem::replace(&mut self.loops.deterministic, true);
        self.find_root(id);
        self.loops.deterministic = was;
    }

    /// What the loops' roots are and why: `(id, title, root, set by hand, key found, where from)`.
    pub fn loop_root_report(&self) -> Vec<LoopRoot> {
        self.loops
            .row
            .clips
            .iter()
            .map(|c| {
                let x = self.loops.pads.extra(&c.id);
                LoopRoot {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    root: x.root,
                    manual: x.root_manual,
                    key: x.key,
                    from: x.key_from,
                    once: x.once,
                }
            })
            .collect()
    }

    /// Look for keys on the clips that land from now on (`loops.auto_root`).
    pub fn set_auto_root(&mut self, on: bool) {
        self.loops.pads.auto_root = on;
    }

    pub fn auto_root(&self) -> bool {
        self.loops.pads.auto_root
    }

    /// Start `id` looping at once, in phase (the capture and library paths).
    pub(crate) fn start_loop_now(&mut self, id: &str) {
        self.start_loop(id, true);
    }

    /// The clock something new on the row lines up with: a playing deck's (the louder one
    /// first), else a loop already going, else none.
    ///
    /// Not `loop_clock(None, None)`: that is Swift's `loopClock(for: nil)`, where a deck holding
    /// no clip never qualifies (`$0.clipID != clip?.id` is nil != nil), so with no clip it only
    /// ever finds a loop. A capture or a pad has no clip yet and wants the decks, so it asks as
    /// a clip no deck holds.
    pub(crate) fn hub_clock(&self) -> crate::deck::clock::LoopClock {
        self.loop_clock(Some(""), None)
    }

    /// The clock a loop lines up with, as (beat of now, frames per beat), if there is one.
    pub fn clock_beat_now(&self) -> Option<(f64, f64)> {
        let c = self.hub_clock();
        (c.frames_per_beat > 0.0).then(|| (c.beat(self.loop_frames()), c.frames_per_beat))
    }

    /// Start clip `id` on `quant`: now, on the next beat, or on its launch boundary.
    fn start_loop_q(&mut self, id: &str, quant: Quant) {
        self.start_loop_with(
            id,
            StartOpts {
                at_once: quant == Quant::Now,
                every: (quant == Quant::Beat).then_some(1),
                ..StartOpts::default()
            },
        );
    }

    /// Put audio on the row without starting it: `bars` whole bars (a loop; its tempo
    /// follows the master) or, with `once`, a one-shot played at its own speed. Returns its
    /// id.
    pub fn add_audio_clip(
        &mut self,
        title: &str,
        left: Vec<f32>,
        right: Vec<f32>,
        bars: i64,
        once: bool,
    ) -> String {
        let buffer = SampleBuffer::new(left, Some(right), self.sample_rate);
        let id = buffer.content_id();
        if self.clip(&id).is_some() {
            return id;
        }
        let beats = bars.max(1) * 4;
        self.add_clip(
            ClipSpec {
                id: id.clone(),
                title: Some(title.to_string()),
                source: "file".into(),
                beats,
                start_beat: 0.0,
                from: String::new(),
                from_input: false,
                locked: false,
                saved: false,
            },
            buffer,
        );
        if once {
            self.make_one_shot(&id);
        }
        id
    }

    /// A one-shot's clip tempo is the master's (it has no grid): it plays at speed 1.
    fn make_one_shot(&mut self, id: &str) {
        let bpm = self.hub_clock().bpm.unwrap_or(120.0);
        if let Some(c) = self.loops.row.clips.iter_mut().find(|c| c.id == id) {
            c.bpm = bpm;
        }
        self.loops.pads.extra_mut(id).once = true;
    }

    /// Launch clip `id` on `quant` (a one-shot plays once; a loop loops until stopped).
    pub fn launch_clip(&mut self, id: &str, quant: Quant) {
        self.start_loop_q(id, quant);
    }

    fn clip_id_at(&self, slot: usize) -> Option<String> {
        self.loops.row.clips.get(slot).map(|c| c.id.clone())
    }

    /// Clips a loop-wide control acts on: the one in `slot`, else every loop that is in.
    fn loop_targets(&self, slot: Option<usize>) -> Vec<String> {
        match slot {
            Some(s) => self.clip_id_at(s).into_iter().collect(),
            None => self
                .loops
                .voices
                .iter()
                .filter(|v| v.stop_frame.is_none())
                .filter_map(|v| v.clip.clone())
                .filter(|id| self.clip(id).is_some() && !self.loops.pads.extra(id).once)
                .collect(),
        }
    }

    /// Say which pitch class (0 = C) the loops sit on, so keys transpose against it.
    pub fn set_loop_root(&mut self, slot: Option<usize>, pitch_class: u8) {
        for id in self.loop_targets(slot) {
            let x = self.loops.pads.extra_mut(&id);
            x.root = pitch_class % 12;
            // a hand wins over what was found
            x.root_manual = true;
        }
    }

    /// A key sets the loops' fundamental: each target moves by the shortest way from its root
    /// to `key`'s pitch class. `tape`: plain resample (the loop speeds up and slows down with
    /// the pitch, like a turntable); otherwise a pitch-shifted copy of the same length, which
    /// stays on the bar grid. The copy is made off this thread; until it is ready the old
    /// pitch keeps playing, then the swap lands on `quant`.
    pub fn transpose_loops(&mut self, slot: Option<usize>, key: u8, quant: Quant, tape: bool) {
        for id in self.loop_targets(slot) {
            let root = self.loops.pads.extra(&id).root;
            let semis = crate::stretch::semitones_to(root, key);
            if tape {
                self.tape_shift(&id, semis);
            } else {
                self.request_shift(&id, semis, quant);
            }
        }
    }

    fn tape_shift(&mut self, id: &str, semis: i32) {
        if self.clip(id).is_none() {
            return;
        }
        self.loops.pads.extra_mut(id).tape = semis;
        let tape = crate::stretch::ratio(semis);
        if let Some(v) = self.voice_of(id) {
            let ratio = self.loops.voices[v].ratio;
            self.loop_send(LoopMsg::Voice(
                v,
                TurntableCommand::Pitch(ratio * tape - 1.0),
            ));
        }
    }

    /// Make every shift a key could ask for ready ahead of time (-6...+5), so a press never
    /// waits on the computing. Cheap to call again.
    pub fn warm_shifts(&mut self, slot: Option<usize>) {
        for id in self.loop_targets(slot) {
            for s in -6..=5 {
                self.compute_shift(&id, s);
            }
        }
    }

    fn compute_shift(&mut self, id: &str, semis: i32) {
        let key = (id.to_string(), semis);
        let p = &self.loops.pads;
        if semis == 0 || p.shifted.contains_key(&key) || p.computing.contains(&key) {
            return;
        }
        let Some(clip) = self.clip(id) else { return };
        let buf = clip.buffer.clone();
        let tx = self.loops.pads.tx.clone();
        let sr = self.sample_rate;
        self.loops.pads.computing.push(key);
        let id = id.to_string();
        std::thread::spawn(move || {
            let mono = buf.right.is_none();
            let chans: Vec<&[f32]> = if mono {
                vec![&buf.left]
            } else {
                vec![&buf.left, buf.right()]
            };
            let mut it = crate::stretch::pitch_shift(&chans, semis, true).into_iter();
            let l = it.next().unwrap_or_default();
            let r = if mono { None } else { it.next() };
            let _ = tx.send(Ready::Shift(
                id,
                semis,
                Arc::new(SampleBuffer::new(l, r, sr)),
            ));
        });
    }

    fn request_shift(&mut self, id: &str, semis: i32, quant: Quant) {
        self.loops
            .pads
            .wanted
            .insert(id.to_string(), (semis, quant));
        if semis == 0
            || self
                .loops
                .pads
                .shifted
                .contains_key(&(id.to_string(), semis))
        {
            self.land_shift(id);
        } else {
            self.compute_shift(id, semis);
        }
    }

    /// Hand a wanted shift to the voice, if its copy is ready.
    fn land_shift(&mut self, id: &str) {
        let Some(&(semis, quant)) = self.loops.pads.wanted.get(id) else {
            return;
        };
        let Some(clip) = self.clip(id) else { return };
        let buffer = if semis == 0 {
            clip.buffer.clone()
        } else if let Some(b) = self.loops.pads.shifted.get(&(id.to_string(), semis)) {
            b.clone()
        } else {
            return;
        };
        let clip_bpm = clip.bpm;
        let tape = crate::stretch::ratio(self.loops.pads.extra(id).tape);
        self.loops.pads.wanted.remove(id);
        if let Some(v) = self
            .voice_of(id)
            .filter(|v| self.loops.voices[*v].stop_frame.is_none())
        {
            let earliest = self.loop_frames() + if quant == Quant::Now { 0 } else { LOOP_LEAD };
            let clock = self.loop_clock(Some(id), None);
            let at = match quant {
                Quant::Now => earliest,
                Quant::Beat => clock.next_boundary(earliest, 1),
                Quant::Bar => clock.next_boundary(earliest, self.loops.quantum),
            };
            // the copy holds the clip's own beats at the clip's own tempo
            let ratio = self.loops.voices[v].target_bpm / clip_bpm;
            let len = buffer.length() as f64;
            self.loop_send(LoopMsg::SwapAt {
                v,
                frame: at,
                buffer,
                pitch: ratio * tape - 1.0,
            });
            self.set_voice_buffer(v, clip_bpm, len, ratio);
        }
        self.loops.pads.extra_mut(id).shift = semis;
    }

    /// Collect finished background work (from `service_voices`).
    pub(crate) fn service_pads(&mut self) {
        while let Ok(r) = self.loops.pads.rx.try_recv() {
            match r {
                Ready::Shift(id, semis, buf) => {
                    let p = &mut self.loops.pads;
                    p.computing.retain(|k| *k != (id.clone(), semis));
                    if p.shifted.len() >= SHIFT_CACHE {
                        let live: Vec<(String, i32)> =
                            p.extra.iter().map(|(k, x)| (k.clone(), x.shift)).collect();
                        p.shifted.retain(|k, _| live.contains(k));
                    }
                    p.shifted.insert((id.clone(), semis), buf);
                    if p.wanted.get(&id).is_some_and(|w| w.0 == semis) {
                        self.land_shift(&id);
                    }
                }
                Ready::Root(id, key) => self.apply_found_root(&id, key, "audio"),
                Ready::File(key, result) => {
                    let Some(spec) = self.loops.pads.loading.remove(&key) else {
                        continue;
                    };
                    if let Ok(buffer) = result {
                        self.add_file_clip(&key, &spec, buffer);
                        if spec.launch {
                            self.launch_file_clip(&key, spec.quant);
                        }
                    }
                }
            }
        }
    }

    fn add_file_clip(&mut self, key: &str, spec: &FileLaunch, buffer: SampleBuffer) {
        if self.clip(key).is_some() {
            return;
        }
        // a loop file knows its bars; its tempo follows. A one-shot has no grid: the master's
        let bars = spec.bars.unwrap_or(1).max(1);
        self.add_clip(
            ClipSpec {
                id: key.to_string(),
                title: Some(crate::host::decode::title_of(&spec.path)),
                source: "file".into(),
                beats: bars * 4,
                start_beat: 0.0,
                from: spec.path.display().to_string(),
                from_input: false,
                locked: false,
                saved: false,
            },
            buffer,
        );
        if spec.once {
            self.make_one_shot(key);
        }
        let x = self.loops.pads.extra_mut(key);
        if spec.once && !x.root_manual {
            // a vocal phrase is not a loop to transpose: its key is known (`key`), its root stays C
            x.root = 0;
        }
        x.gain = spec.gain;
        x.choke = spec.choke.clone();
    }

    /// Cut every other playing clip in `key`'s choke group (a closed hi-hat cuts an open one).
    fn choke_others(&mut self, key: &str) {
        let Some(group) = self.loops.pads.extra(key).choke else {
            return;
        };
        let victims: Vec<String> = self
            .loops
            .row
            .clips
            .iter()
            .map(|c| c.id.clone())
            .filter(|id| id != key)
            .filter(|id| self.loops.pads.extra(id).choke.as_deref() == Some(group.as_str()))
            .filter(|id| self.voice_of(id).is_some())
            .collect();
        for id in victims {
            self.stop_loop(&id, true, None);
        }
    }

    fn launch_file_clip(&mut self, key: &str, quant: Quant) {
        self.choke_others(key);
        if self.loops.pads.extra(key).once {
            // a hit: from the top again if it is still sounding
            if let Some(v) = self.voice_of(key) {
                self.loop_send(LoopMsg::Voice(v, TurntableCommand::Seek(0.0)));
                return;
            }
            self.start_loop_q(key, quant);
        } else if self.loop_state(key) == LoopState::Off {
            self.start_loop_q(key, quant);
        } else {
            self.stop_loop(key, false, None);
        }
    }

    /// A pad bound to a file. The first press decodes it (and stretches it to the master
    /// tempo if `bpm` says what its own is) off this thread and plays it when ready; later
    /// presses are instant. Loop files toggle on the bar; one-shots fire on `quant` and
    /// retrigger from the top while sounding. Call [`Engine::preload_file`] with the same
    /// spec when the binding is installed to take even the first press's wait away.
    pub fn trigger_file(&mut self, spec: FileLaunch) {
        let key = file_key(&spec);
        if self.clip(&key).is_some() {
            self.launch_file_clip(&key, spec.quant);
        } else if let Some(l) = self.loops.pads.loading.get_mut(&key) {
            l.launch = true;
        } else {
            self.preload_file(FileLaunch {
                launch: true,
                ..spec
            });
        }
    }

    pub fn preload_file(&mut self, spec: FileLaunch) {
        let key = file_key(&spec);
        if self.clip(&key).is_some() || self.loops.pads.loading.contains_key(&key) {
            return;
        }
        let (sr, master) = (self.sample_rate, self.hub_clock().bpm);
        let tx = self.loops.pads.tx.clone();
        let path = spec.path.clone();
        let (bpm, k) = (spec.bpm, key.clone());
        self.loops.pads.loading.insert(key, spec);
        std::thread::spawn(move || {
            let r = crate::host::decode::decode_file(&path, sr)
                .map_err(|e| e.to_string())
                .map(|b| match (bpm, master) {
                    (Some(own), Some(m)) if own > 0.0 && m > 0.0 => {
                        let n = (b.length() as f64 * own / m).round() as usize;
                        let out = crate::stretch::time_fit(&[&b.left, b.right()], n.max(1));
                        let mut it = out.into_iter();
                        let (l, r) = (it.next().unwrap_or_default(), it.next());
                        SampleBuffer::new(l, r, sr)
                    }
                    _ => b,
                });
            let _ = tx.send(Ready::File(k, r));
        });
    }
}

fn file_key(spec: &FileLaunch) -> String {
    format!(
        "file:{}|{}|{}|{}|{}|{}",
        spec.path.display(),
        spec.bars.unwrap_or(0),
        spec.bpm.unwrap_or(0.0),
        spec.once,
        spec.gain,
        spec.choke.as_deref().unwrap_or("")
    )
}
