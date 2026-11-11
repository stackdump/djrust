//! The loop row's clips as data: `LoopClip` (a take, a crop, a retune, a sequence, an import),
//! the settings that go with a clip on the row (`ClipSound`, `ClipLaunch`, `RemovedClip`, the
//! per-clip maps), the fade a loop comes in and goes out with, and the audio maths of a crop
//! and a sequence. Port of `LoopRecorder.swift` (`LoopClip`, `SequenceStep` use,
//! `cropClip` / `makeSequence` audio), `LoopClipEdits.swift` (the removed clips, provenance
//! accessors, WAV and file name) and `LoopSound.swift` (`LoopFade`, the per-clip maps
//! `forgetClipSound` / `carryClipSound` work on) and `Session/LoopLibrary.swift` (`ClipLaunch`).
//!
//! Everything here is a function of its arguments: the engine half (the voices, the bank, the
//! scenes, the library on disk) is the host's, and calls these. Ids are
//! [`SampleBuffer::content_id`] (`SampleBuffer.contentID`, sha256 of the rate and the audio);
//! a sequence's is `"seq-"` plus that ([`sequence_id`]).
//!
//! Companions: [`super::provenance`] (what a clip was made from), [`super::clock`] (the beat
//! clock a start or stop lines up with), [`super::library`] (the index entries on disk) and
//! [`super::voices`] (the instruments a retune offers).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::beat_grid::BeatGrid;
use super::loops::{valid_clip_quantum, LoopFollow};
use super::provenance;
use super::turntable::SampleBuffer;
use super::waveform::WaveformOverview;
use crate::json::JsonValue;
use crate::session::action::voices_from_json;
use crate::session::model::{sample_content_id, DeckSource, DEMO_ID};
use crate::session::types::{LoopFx, LoopFxMode, SequenceStep};

// MARK: audio ids

impl SampleBuffer {
    /// `SampleBuffer.contentID`: sha256 over the rate's bit pattern and the float bits of both
    /// channels, hex. A mono buffer (`right` None) hashes its one channel; the stereo buffer
    /// every crop, stretch and sequence makes hashes both, even when they are equal.
    pub fn content_id(&self) -> String {
        sample_content_id(self.sample_rate, &self.left, self.right.as_deref())
    }
}

/// `DJEngine.sequenceID`: a sequence's id: its audio's content id, marked, so a one-step
/// sequence is not its clip.
pub fn sequence_id(b: &SampleBuffer) -> String {
    format!("seq-{}", b.content_id())
}

/// Columns of a clip's waveform overview (`WaveformOverview(buffer:, columns: 400)`).
pub const WAVEFORM_COLUMNS: usize = 400;

// MARK: the clip

/// The colours a clip can be given, by name (the views pick the shades).
pub const PALETTE: [&str; 8] = [
    "red", "orange", "yellow", "green", "teal", "blue", "violet", "pink",
];

/// A finished take: whole bars of one deck, on the row under the mixer. A sequence is one too:
/// its steps rendered back to back into one piece of audio, with the steps kept so it can be
/// edited (`LoopClip`). Equality is by id.
#[derive(Clone, Debug)]
pub struct LoopClip {
    /// Content id of the audio as it was made ([`SampleBuffer::content_id`]); also its key in
    /// the sample library. A clip the library brings back at another sample rate keeps it,
    /// though its resampled audio would hash differently.
    pub id: String,
    /// Its name on the row ("A loop 3" until renamed).
    pub title: String,
    /// Its colour on the row, one of [`PALETTE`] (None: none picked).
    pub color: Option<String>,
    /// The deck it was taken from.
    pub source: String,
    /// Its length in beats: whole bars from LOOP record, any number of beats once cropped.
    pub beats: i64,
    /// The tempo the bars were played at (the deck's, after its tempo fader).
    pub bpm: f64,
    /// The source deck's beat at the clip's first sample: a downbeat (a multiple of 4).
    pub start_beat: f64,
    pub buffer: Arc<SampleBuffer>,
    pub waveform: WaveformOverview,
    /// A sequence's recipe (None for a take or a crop).
    pub steps: Option<Vec<SequenceStep>>,
    /// Where it came from ([`super::provenance`]): the deck, its track and settings when LOOP
    /// went down; for a crop or a sequence, what it was made from.
    pub provenance: Option<JsonValue>,
    /// Its after effects under the mixer: played on top of the dry audio, never rendered into
    /// it. Off by default.
    pub fx: LoopFx,
}

impl PartialEq for LoopClip {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl LoopClip {
    /// `init(id:title:source:beats:bpm:startBeat:buffer:waveform:)`: no steps, provenance or
    /// colour, FX off.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        title: &str,
        source: &str,
        beats: i64,
        bpm: f64,
        start_beat: f64,
        buffer: Arc<SampleBuffer>,
        waveform: WaveformOverview,
    ) -> LoopClip {
        LoopClip {
            id: id.to_string(),
            title: title.to_string(),
            color: None,
            source: source.to_string(),
            beats,
            bpm,
            start_beat,
            buffer,
            waveform,
            steps: None,
            provenance: None,
            fx: LoopFx::default(),
        }
    }

    /// `init(id:title:source:bars:bpm:startBeat:buffer:waveform:provenance:)`: a take of whole
    /// bars.
    #[allow(clippy::too_many_arguments)]
    pub fn from_bars(
        id: &str,
        title: &str,
        source: &str,
        bars: i64,
        bpm: f64,
        start_beat: f64,
        buffer: Arc<SampleBuffer>,
        waveform: WaveformOverview,
        provenance: Option<JsonValue>,
    ) -> LoopClip {
        let mut c = LoopClip::new(
            id,
            title,
            source,
            bars * 4,
            bpm,
            start_beat,
            buffer,
            waveform,
        );
        c.provenance = provenance;
        c
    }

    /// A clip for `buffer`, as every maker of one builds it: its id is the audio's content id
    /// and its waveform has [`WAVEFORM_COLUMNS`] columns.
    pub fn from_buffer(
        title: &str,
        source: &str,
        beats: i64,
        bpm: f64,
        start_beat: f64,
        buffer: SampleBuffer,
    ) -> LoopClip {
        let waveform = WaveformOverview::new(&buffer, WAVEFORM_COLUMNS);
        LoopClip::new(
            &buffer.content_id(),
            title,
            source,
            beats,
            bpm,
            start_beat,
            Arc::new(buffer),
            waveform,
        )
    }

    /// Whole bars in it (rounded down; `beats` is the length).
    pub fn bars(&self) -> i64 {
        self.beats / 4
    }

    /// `grid`: the beat grid it loads onto a deck with.
    pub fn grid(&self) -> BeatGrid {
        BeatGrid::new(self.bpm, 0.0, 1.0)
    }

    /// `lengthLabel`: "2 bars", "1 bar", "6 beats", "1 beat".
    pub fn length_label(&self) -> String {
        if self.beats % 4 == 0 {
            let bars = self.bars();
            format!("{bars} bar{}", if bars == 1 { "" } else { "s" })
        } else {
            format!("{} beat{}", self.beats, if self.beats == 1 { "" } else { "s" })
        }
    }

    /// `wavData`: the clip's audio as a 32-bit float stereo WAV (what the library keeps, and
    /// an export): 44 header bytes and 8 a frame.
    pub fn wav_data(&self) -> Vec<u8> {
        let b = &self.buffer;
        crate::wav::wav_bytes(&b.left, b.right(), b.sample_rate, true)
    }

    /// `fileName`: a file name for an export or a drag: its title, without characters Finder
    /// minds (`/ : \ ? % * | " <` `>` and line breaks become "-"), trimmed of white space; a
    /// blank one is "loop"; then ".wav".
    pub fn file_name(&self) -> String {
        fn bad(c: char) -> bool {
            matches!(
                c,
                '/' | ':'
                    | '\\'
                    | '?'
                    | '%'
                    | '*'
                    | '|'
                    | '"'
                    | '<'
                    | '>'
                    // CharacterSet.newlines
                    | '\u{a}'..='\u{d}'
                    | '\u{85}'
                    | '\u{2028}'
                    | '\u{2029}'
            )
        }
        // CharacterSet.whitespaces: tab and the Zs category
        fn space(c: char) -> bool {
            matches!(
                c,
                '\t' | ' '
                    | '\u{a0}'
                    | '\u{1680}'
                    | '\u{2000}'..='\u{200a}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
            )
        }
        let replaced: String = self
            .title
            .chars()
            .map(|c| if bad(c) { '-' } else { c })
            .collect();
        let base = replaced.trim_matches(space);
        format!("{}.wav", if base.is_empty() { "loop" } else { base })
    }

    // MARK: where it came from

    /// `takeProvenance`: the provenance of the take this clip is made from: a crop's or a
    /// retune's original (all the way back), a sequence's first step. (Not the same function
    /// as [`super::loops::take_provenance`], which is `ClipRetune.takeProvenance`: that one
    /// refuses a sequence, a sample and a clip with none.)
    pub fn take_provenance(&self) -> Option<&JsonValue> {
        let mut p = self.provenance.as_ref();
        while let Some(op) = p.and_then(|v| v.get("op")).and_then(JsonValue::as_str) {
            match op {
                "crop" | "retune" => p = p.and_then(|v| v.get("parent")),
                "sequence" => {
                    p = p
                        .and_then(|v| v.get("steps"))
                        .and_then(JsonValue::as_array)
                        .and_then(|a| a.first())
                        .and_then(|s| s.get("provenance"))
                }
                _ => return p,
            }
        }
        p
    }

    /// `originSource`: the deck's source as LOOP went down ([`provenance::capture`]), if it
    /// was recorded, with the instruments a retune gave it ([`retuned_voices`], applied in
    /// channel order through [`DeckSource::with_instrument`]), so loading it plays what the
    /// clip does.
    ///
    /// [`retuned_voices`]: LoopClip::retuned_voices
    pub fn origin_source(&self) -> Option<DeckSource> {
        let mut source = self
            .take_provenance()?
            .get("source")
            .and_then(DeckSource::from_json)?;
        for (channel, instrument) in self.retuned_voices() {
            if let Some(next) = source.with_instrument(&instrument, channel) {
                source = next;
            }
        }
        Some(source)
    }

    /// `retunedVoices`: the instruments retunes gave this clip (MIDI channel → beats
    /// instrument), the latest's over earlier ones, through crops of it
    /// ([`provenance::retune`]). Empty for a take as taken and for a sequence (its steps keep
    /// their own).
    pub fn retuned_voices(&self) -> BTreeMap<i64, String> {
        let mut voices: BTreeMap<i64, String> = BTreeMap::new();
        let mut p = self.provenance.as_ref();
        while let Some(op) = p.and_then(|v| v.get("op")).and_then(JsonValue::as_str) {
            match op {
                "crop" => p = p.and_then(|v| v.get("parent")),
                "retune" => {
                    // the newest retune is met first: what it set stands over the ones before it
                    for (ch, name) in voices_from_json(p.and_then(|v| v.get("voices"))) {
                        voices.entry(ch).or_insert(name);
                    }
                    p = p.and_then(|v| v.get("parent"));
                }
                _ => return voices,
            }
        }
        voices
    }

    /// `originSection`: the section the deck was playing then (net decks).
    pub fn origin_section(&self) -> Option<i64> {
        self.take_provenance()?
            .get("section")?
            .get("index")?
            .as_i64()
    }

    /// `originGenre`: the genre of the net it came from, for search ("techno"); None for a
    /// sample.
    pub fn origin_genre(&self) -> Option<String> {
        self.take_provenance()?
            .get("source")?
            .get("share")?
            .get("genre")?
            .as_str()
            .map(String::from)
    }

    /// `originTitle`: the title of what the deck was playing.
    pub fn origin_title(&self) -> Option<String> {
        self.take_provenance()?
            .get("title")?
            .as_str()
            .map(String::from)
    }

    /// `originBPM`: the deck's tempo as LOOP went down (after its tempo fader), if it was
    /// recorded.
    pub fn origin_bpm(&self) -> Option<f64> {
        self.take_provenance()?.get("tempo")?.get("bpm")?.as_f64()
    }

    /// `originRolesOff`: the net deck's roles that were off as LOOP went down (switched off,
    /// disabled or under a muted stem).
    pub fn origin_roles_off(&self) -> Vec<String> {
        self.take_provenance()
            .and_then(|p| p.get("rolesOff"))
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `originMacros`: the macros (net and master) running as LOOP went down.
    pub fn origin_macros(&self) -> Vec<String> {
        self.take_provenance()
            .and_then(|p| p.get("macros"))
            .and_then(JsonValue::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What a clip was taken from, ready to load again (`ClipOrigin`): the deck's source as LOOP
/// went down, the audio for a sample (None for a net, which is composed again), and the
/// section it was in.
#[derive(Clone, Debug)]
pub struct ClipOrigin {
    pub source: DeckSource,
    pub buffer: Option<Arc<SampleBuffer>>,
    pub section: Option<i64>,
}

impl PartialEq for ClipOrigin {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
            && self.section == other.section
            && self.buffer.is_none() == other.buffer.is_none()
    }
}

/// `DJEngine.clipOrigin`, for the clip the host found (on the row or removed): what it came
/// from, if it can be loaded again: a net always (it is composed again), a sample only while
/// its audio is in `sample_library` (or it is the demo loop).
pub fn clip_origin(
    clip: &LoopClip,
    sample_library: &BTreeMap<String, Arc<SampleBuffer>>,
) -> Option<ClipOrigin> {
    let source = clip.origin_source()?;
    match &source {
        DeckSource::Net(_) | DeckSource::RawNets(_) => {
            let section = clip.origin_section();
            Some(ClipOrigin {
                source,
                buffer: None,
                section,
            })
        }
        DeckSource::Sample { id, .. } => {
            if id == DEMO_ID {
                return Some(ClipOrigin {
                    source,
                    buffer: None,
                    section: None,
                });
            }
            let b = sample_library.get(id)?.clone();
            Some(ClipOrigin {
                source,
                buffer: Some(b),
                section: None,
            })
        }
        DeckSource::None => None,
    }
}

/// `DJEngine.importTempo`: the tempo `importClip` gives a clip of `beats` beats in `buffer`
/// when it is given none.
pub fn import_tempo(buffer: &SampleBuffer, beats: i64) -> f64 {
    beats as f64 * 60.0 * buffer.sample_rate / buffer.length().max(1) as f64
}

// MARK: fades

/// How a loop under the mixer comes in and goes out (`LoopFade`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopFade {
    /// At once (a click, if it lands mid-waveform).
    Off,
    /// 10 ms: no click, still on the downbeat.
    Declick,
    Beat,
    Bar,
}

impl LoopFade {
    pub const ALL: [LoopFade; 4] = [
        LoopFade::Off,
        LoopFade::Declick,
        LoopFade::Beat,
        LoopFade::Bar,
    ];

    /// The raw string a session, the journal and the library keep.
    pub fn raw(self) -> &'static str {
        match self {
            LoopFade::Off => "off",
            LoopFade::Declick => "10ms",
            LoopFade::Beat => "beat",
            LoopFade::Bar => "bar",
        }
    }

    pub fn from_raw(s: &str) -> Option<LoopFade> {
        LoopFade::ALL.into_iter().find(|f| f.raw() == s)
    }

    /// For a menu: "off", "10 ms", "1 beat", "1 bar".
    pub fn label(self) -> &'static str {
        match self {
            LoopFade::Off => "off",
            LoopFade::Declick => "10 ms",
            LoopFade::Beat => "1 beat",
            LoopFade::Bar => "1 bar",
        }
    }

    /// `loopFadeFrames(_:bpm:short:)`: the fade in frames for a loop playing at `bpm`
    /// (`sample_rate` is the engine's). `short`: a start or stop that is not on a downbeat (a
    /// fresh take, a clip moved to a deck), which only de-clicks. `Off` is 0 even when short;
    /// a tempo of 0 or less makes a beat as long as a de-click.
    pub fn frames(self, sample_rate: f64, bpm: f64, short: bool) -> i64 {
        let declick = (sample_rate * 0.01) as i64;
        let beat = if bpm > 0.0 {
            (60.0 / bpm * sample_rate) as i64
        } else {
            declick
        };
        let frames = match self {
            LoopFade::Off => return 0,
            LoopFade::Declick => declick,
            LoopFade::Beat => beat,
            LoopFade::Bar => 4 * beat,
        };
        if short {
            frames.min(declick)
        } else {
            frames
        }
    }
}

// MARK: the settings that go with a clip

/// A clip's own sound settings under the mixer (`ClipSound`); its level is kept apart, in the
/// engine's `clipGains`, and survives a removal anyway.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipSound {
    pub muted: bool,
    pub soloed: bool,
    /// Its own key lock; None follows the row's.
    pub key_lock: Option<bool>,
    /// Its own launch boundary (beats) and fade; None follows the row's.
    pub quantum: Option<i64>,
    pub fade: Option<LoopFade>,
    /// Whose tempo it follows; None follows the row's.
    pub follow: Option<LoopFollow>,
}

/// A clip's own launch boundary (beats), fade and tempo follow, as the library keeps them;
/// None follows the row's (`ClipLaunch`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipLaunch {
    pub quantum: Option<i64>,
    pub fade: Option<LoopFade>,
    pub follow: Option<LoopFollow>,
}

impl ClipLaunch {
    pub fn is_empty(&self) -> bool {
        self.quantum.is_none() && self.fade.is_none() && self.follow.is_none()
    }
}

/// A clip taken off the row, with the place it had there (`RemovedClip`).
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedClip {
    pub clip: LoopClip,
    /// Its index on the row when it was taken off.
    pub index: i64,
    /// How it sounded then (mute, solo, key lock, launch boundary, fade, tempo follow), so
    /// putting it back undoes the removal whole.
    pub sound: ClipSound,
}

impl RemovedClip {
    pub fn new(clip: LoopClip, index: i64, sound: ClipSound) -> RemovedClip {
        RemovedClip { clip, index, sound }
    }
}

/// The per-clip maps of the engine (`clipMutes`, `clipSolos`, `clipKeyLocks`, `clipQuanta`,
/// `clipFades`, `clipFollows`), by clip id, with the operations that move them with a clip:
/// [`snapshot`] / [`restore`] around a removal, [`forget`] when a clip leaves the row, [`carry`]
/// when another takes its place.
///
/// What the engine does around them stays the host's: pushing every voice's level again when
/// a solo came or went (the methods say when), dropping the key-lock stretch cache, updating
/// the dominant deck when a follow became `Louder`.
///
/// [`snapshot`]: ClipSettings::snapshot
/// [`restore`]: ClipSettings::restore
/// [`forget`]: ClipSettings::forget
/// [`carry`]: ClipSettings::carry
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipSettings {
    pub mutes: BTreeSet<String>,
    pub solos: BTreeSet<String>,
    pub key_locks: BTreeMap<String, bool>,
    pub quanta: BTreeMap<String, i64>,
    pub fades: BTreeMap<String, LoopFade>,
    pub follows: BTreeMap<String, LoopFollow>,
}

impl ClipSettings {
    pub fn new() -> ClipSettings {
        ClipSettings::default()
    }

    // mute and solo

    pub fn is_muted(&self, id: &str) -> bool {
        self.mutes.contains(id)
    }

    pub fn is_soloed(&self, id: &str) -> bool {
        self.solos.contains(id)
    }

    /// `setClipMute`'s map change (the host then pushes the levels).
    pub fn set_mute(&mut self, id: &str, on: bool) {
        if on {
            self.mutes.insert(id.to_string());
        } else {
            self.mutes.remove(id);
        }
    }

    /// `setClipSolo`'s map change (the host then pushes the levels).
    pub fn set_solo(&mut self, id: &str, on: bool) {
        if on {
            self.solos.insert(id.to_string());
        } else {
            self.solos.remove(id);
        }
    }

    /// `audible(_:at:)`: level `gain` for clip `id` as its voice plays it: 0 when muted or
    /// soloed out (while any clip is soloed, only soloed ones are heard).
    pub fn audible(&self, id: &str, gain: f64) -> f64 {
        if self.mutes.contains(id) {
            return 0.0;
        }
        if !self.solos.is_empty() && !self.solos.contains(id) {
            return 0.0;
        }
        gain
    }

    // key lock

    /// `isKeyLocked`: its own setting, else the row's `loop_key_lock`.
    pub fn is_key_locked(&self, id: &str, loop_key_lock: bool) -> bool {
        self.key_locks.get(id).copied().unwrap_or(loop_key_lock)
    }

    pub fn set_key_lock(&mut self, id: &str, on: bool) {
        self.key_locks.insert(id.to_string(), on);
    }

    // launch boundary

    /// `clipQuantum`: clip `id`'s own launch boundary in beats, if it has one.
    pub fn quantum(&self, id: &str) -> Option<i64> {
        self.quanta.get(id).copied()
    }

    /// `quantum(for:)`: what clip `id` starts and stops on, in beats: its own boundary, else
    /// the row's. (Scenes switch on the row's whatever their clips say.)
    pub fn quantum_for(&self, row: i64, id: Option<&str>) -> i64 {
        super::loops::quantum_for(&self.quanta, row, id)
    }

    /// `setClipQuantum`: a boundary of its own (any of
    /// [`LAUNCH_QUANTA`](super::loops::LAUNCH_QUANTA); anything else is refused: false); None
    /// follows the row's again. Pending starts and stops keep theirs.
    pub fn set_quantum(&mut self, id: &str, beats: Option<i64>) -> bool {
        match valid_clip_quantum(beats) {
            Some(Some(q)) => {
                self.quanta.insert(id.to_string(), q);
                true
            }
            Some(None) => {
                self.quanta.remove(id);
                true
            }
            None => false,
        }
    }

    // fade

    /// `clipFade`: clip `id`'s own fade, if it has one.
    pub fn fade(&self, id: &str) -> Option<LoopFade> {
        self.fades.get(id).copied()
    }

    /// `loopFade(for:)`: the fade clip `id` comes in and goes out with: its own, else the
    /// row's.
    pub fn fade_for(&self, row: LoopFade, id: Option<&str>) -> LoopFade {
        id.and_then(|i| self.fades.get(i).copied()).unwrap_or(row)
    }

    /// `setClipFade`: None follows the row's again. A start or stop already waiting keeps the
    /// fade it was given.
    pub fn set_fade(&mut self, id: &str, fade: Option<LoopFade>) {
        match fade {
            Some(f) => {
                self.fades.insert(id.to_string(), f);
            }
            None => {
                self.fades.remove(id);
            }
        }
    }

    // tempo follow

    /// `clipFollow`: clip `id`'s own tempo follow, if it has one.
    pub fn follow(&self, id: &str) -> Option<LoopFollow> {
        self.follows.get(id).copied()
    }

    /// `follow(for:)`: whose tempo clip `id` follows: its own choice, else the row's.
    pub fn follow_for(&self, row: LoopFollow, id: Option<&str>) -> LoopFollow {
        super::loops::follow_for(&self.follows, row, id)
    }

    /// `setClipFollow`'s map change: None follows the row's again. (A `Louder` follow also
    /// makes the host update the dominant deck.)
    pub fn set_follow(&mut self, id: &str, mode: Option<LoopFollow>) {
        match mode {
            Some(m) => {
                self.follows.insert(id.to_string(), m);
            }
            None => {
                self.follows.remove(id);
            }
        }
    }

    /// `anyLoopFollowsLouder`: the row or a clip of its own.
    pub fn any_follows_louder(&self, row: LoopFollow) -> bool {
        super::loops::any_loop_follows_louder(row, &self.follows)
    }

    // moving with a clip

    /// How clip `id` sounds now, for `removeClip` to keep with it (`ClipSound(muted:…)`).
    pub fn snapshot(&self, id: &str) -> ClipSound {
        ClipSound {
            muted: self.mutes.contains(id),
            soloed: self.solos.contains(id),
            key_lock: self.key_locks.get(id).copied(),
            quantum: self.quanta.get(id).copied(),
            fade: self.fades.get(id).copied(),
            follow: self.follows.get(id).copied(),
        }
    }

    /// `restoreClip`'s settings half: put back what `snapshot` took (the quantum as it was,
    /// unchecked). True when a solo came back: the host pushes every voice's level again, the
    /// others go quiet.
    pub fn restore(&mut self, id: &str, sound: &ClipSound) -> bool {
        if sound.muted {
            self.mutes.insert(id.to_string());
        }
        if let Some(k) = sound.key_lock {
            self.key_locks.insert(id.to_string(), k);
        }
        if let Some(q) = sound.quantum {
            self.quanta.insert(id.to_string(), q);
        }
        if let Some(f) = sound.fade {
            self.fades.insert(id.to_string(), f);
        }
        if let Some(f) = sound.follow {
            self.follows.insert(id.to_string(), f);
        }
        if sound.soloed {
            self.solos.insert(id.to_string());
        }
        sound.soloed
    }

    /// `forgetClipSound`: a clip left the row: forget how it sounded. True when a solo was
    /// removed: the host pushes the levels again. (It also drops the clip's stretch cache.)
    pub fn forget(&mut self, id: &str) -> bool {
        self.mutes.remove(id);
        self.key_locks.remove(id);
        self.quanta.remove(id);
        self.fades.remove(id);
        self.follows.remove(id);
        self.solos.remove(id)
    }

    /// `carryClipSound`: `new` takes `old`'s place on the row (a crop, an edited sequence): it
    /// sounds the same way. (The host drops `old`'s stretch cache.)
    pub fn carry(&mut self, old: &str, new: &str) {
        if self.mutes.remove(old) {
            self.mutes.insert(new.to_string());
        }
        if self.solos.remove(old) {
            self.solos.insert(new.to_string());
        }
        if let Some(k) = self.key_locks.remove(old) {
            self.key_locks.insert(new.to_string(), k);
        }
        if let Some(q) = self.quanta.remove(old) {
            self.quanta.insert(new.to_string(), q);
        }
        if let Some(f) = self.fades.remove(old) {
            self.fades.insert(new.to_string(), f);
        }
        if let Some(f) = self.follows.remove(old) {
            self.follows.insert(new.to_string(), f);
        }
    }

    /// What `clearLoopRow` empties: the mutes, solos and key locks, and nothing else. (The
    /// launch boundaries, fades and follows of the clips it clears stay in their maps, as in
    /// Swift.)
    pub fn clear_for_reset(&mut self) {
        self.mutes.clear();
        self.solos.clear();
        self.key_locks.clear();
    }

    /// `clipLaunch`: the launch boundaries, fades and follows of the clips on the row (`row`:
    /// their ids), by id, only clips that have one: what the library saves.
    pub fn launch_map<'a>(
        &self,
        row: impl IntoIterator<Item = &'a str>,
    ) -> BTreeMap<String, ClipLaunch> {
        let mut out = BTreeMap::new();
        for id in row {
            let l = ClipLaunch {
                quantum: self.quanta.get(id).copied(),
                fade: self.fades.get(id).copied(),
                follow: self.follows.get(id).copied(),
            };
            if !l.is_empty() {
                out.insert(id.to_string(), l);
            }
        }
        out
    }

    /// `restoreClipLaunch`: give the clips the launch boundaries and fades the library saved;
    /// ids `exists` doesn't know are skipped, and a quantum that is not one of the
    /// launch quanta is refused. (A `Louder` follow also makes the host update the dominant
    /// deck.)
    pub fn restore_launch(
        &mut self,
        saved: &BTreeMap<String, ClipLaunch>,
        exists: &dyn Fn(&str) -> bool,
    ) {
        for (id, l) in saved {
            if !exists(id) {
                continue;
            }
            if let Some(q) = l.quantum {
                self.set_quantum(id, Some(q));
            }
            if let Some(f) = l.fade {
                self.set_fade(id, Some(f));
            }
            if let Some(f) = l.follow {
                self.set_follow(id, Some(f));
            }
        }
    }
}

// MARK: crop

/// The audio of `cropClip`: beats `from..to` of a clip of `beats` beats whose audio is `src`.
/// None when nothing changes or it is out of range (`0 <= from < to <= beats`, and not the
/// whole clip), and when the cut holds no samples. A beat is `len / beats` samples
/// (a double), the edges round half away from zero, and the copy is a stereo buffer even from
/// a mono clip, as `SampleBuffer(left:right:)` makes it.
pub fn crop_buffer(src: &SampleBuffer, beats: i64, from: i64, to: i64) -> Option<SampleBuffer> {
    if !(0 <= from && from < to && to <= beats && (from > 0 || to < beats)) {
        return None;
    }
    let len = src.length();
    let per_beat = len as f64 / beats as f64;
    let a = libm::round(from as f64 * per_beat) as usize;
    let b = (libm::round(to as f64 * per_beat) as usize).min(len);
    if b <= a {
        return None;
    }
    Some(SampleBuffer::new(
        src.left[a..b].to_vec(),
        Some(src.right()[a..b].to_vec()),
        src.sample_rate,
    ))
}

/// `cropClip`'s clip: the cropped audio as a clip in the original's place, or None where
/// [`crop_buffer`] refuses. Same title, source, tempo, colour and FX; `beats` is `to - from`,
/// `start_beat` the original's plus `from`; its provenance is [`provenance::crop`] of the
/// original. (The row work is the host's: the cropped clip takes the original's place, level,
/// scenes and settings, and sequences retarget.)
pub fn crop_clip(c: &LoopClip, from: i64, to: i64) -> Option<LoopClip> {
    let buffer = crop_buffer(&c.buffer, c.beats, from, to)?;
    let mut cropped = LoopClip::from_buffer(
        &c.title,
        &c.source,
        to - from,
        c.bpm,
        c.start_beat + from as f64,
        buffer,
    );
    cropped.provenance = Some(provenance::crop(c, from, to));
    cropped.color = c.color.clone();
    cropped.fx = c.fx.clone();
    Some(cropped)
}

// MARK: sequence

/// `DJEngine.stretch`: a clip's audio read `factor` source samples per output sample (linear
/// interpolation): at 0.5 it lasts twice as long. Within 1e-9 of 1 is a plain copy. Both
/// channels, the right being the left again for a mono buffer.
pub fn stretch(b: &SampleBuffer, factor: f64) -> (Vec<f32>, Vec<f32>) {
    if (factor - 1.0).abs() <= 1e-9 {
        return (b.left.clone(), b.right().to_vec());
    }
    let n = ((libm::round(b.length() as f64 / factor)) as i64).max(1) as usize;
    fn resample(x: &[f32], factor: f64, n: usize) -> Vec<f32> {
        if x.is_empty() {
            return Vec::new();
        }
        let last = x.len() - 1;
        (0..n)
            .map(|i| {
                let p = i as f64 * factor;
                let i0 = (p as usize).min(last);
                let i1 = (i0 + 1).min(last);
                let f = (p - i0 as f64) as f32;
                x[i0] + (x[i1] - x[i0]) * f
            })
            .collect()
    }
    (
        resample(&b.left, factor, n),
        resample(b.right(), factor, n),
    )
}

/// The steps of a sequence that have a clip and play at least once, with their clips
/// (`makeSequence`'s `used`): `lookup` finds a clip by id.
pub fn used_steps<'a>(
    steps: &[SequenceStep],
    lookup: impl Fn(&str) -> Option<&'a LoopClip>,
) -> Vec<(SequenceStep, &'a LoopClip)> {
    steps
        .iter()
        .filter_map(|s| lookup(&s.clip_id).map(|c| (s.clone(), c)))
        .filter(|(s, _)| s.repeats > 0)
        .collect()
}

/// The audio of a sequence: each used step's clip `repeats` times, back to back, at the first
/// clip's tempo `bpm` (each clip stretched by `bpm / c.bpm`: one at another tempo lasts
/// `c.bpm / bpm` as long). Returns both channels and the total beats.
pub fn sequence_audio(
    used: &[(SequenceStep, &LoopClip)],
    bpm: f64,
) -> (Vec<f32>, Vec<f32>, i64) {
    let mut l: Vec<f32> = Vec::new();
    let mut r: Vec<f32> = Vec::new();
    let mut beats = 0i64;
    for (step, c) in used {
        let (cl, cr) = stretch(&c.buffer, bpm / c.bpm);
        for _ in 0..step.repeats {
            l.extend_from_slice(&cl);
            r.extend_from_slice(&cr);
            beats += c.beats;
        }
    }
    (l, r, beats)
}

/// `makeSequence`'s title: "seq " and each step's clip name joined by "→", a name being the
/// last space-separated word of the clip's title (the whole title when it has none) and, for
/// a step played more than once, "×n": `seq 1×2→2`.
pub fn sequence_title(used: &[(SequenceStep, &LoopClip)]) -> String {
    let names: Vec<String> = used
        .iter()
        .map(|(s, c)| {
            let word = c
                .title
                .split(' ')
                .rev()
                .find(|w| !w.is_empty())
                .unwrap_or(&c.title);
            if s.repeats > 1 {
                format!("{word}×{}", s.repeats)
            } else {
                word.to_string()
            }
        })
        .collect();
    format!("seq {}", names.join("→"))
}

/// `LoopFX.isActive`: does the chain change the sound at all? Off, bypassed, or set to
/// nothing: the voice plays its clip untouched.
pub fn fx_is_active(fx: &LoopFx) -> bool {
    if fx.bypass {
        return false;
    }
    if fx.trim != 0.0 {
        return true;
    }
    if fx.mix <= 0.0 {
        return false;
    }
    fx.drive > 0.0 || (fx.mode != LoopFxMode::Clean && fx.amount > 0.0) || fx.tone < 1.0
}

/// `sharedFx`: the FX a new sequence starts with: its steps' when every step's clip has the
/// same active FX (the sequence sounds as its steps did), else off. Its audio is the steps'
/// dry audio either way, so the id does not depend on them.
pub fn shared_fx(clips: &[&LoopClip]) -> LoopFx {
    match clips.first() {
        Some(first) if fx_is_active(&first.fx) && clips.iter().all(|c| c.fx == first.fx) => {
            first.fx.clone()
        }
        _ => LoopFx::default(),
    }
}

/// `makeSequence`'s clip: `steps` joined into one clip, or None when no step has a clip that
/// plays. Its tempo, source and start beat are the first used clip's, its `beats` the sum of
/// each used clip's beats times its repeats, its id [`sequence_id`], its FX [`shared_fx`] of the
/// used clips, its provenance [`provenance::sequence`] (stamped `recorded`), and `steps` the
/// used steps. `sample_rate` is the engine's, the audio's own. (The row work is the host's: an
/// edited sequence (`replacing`) goes in its place and keeps the FX it had, a new one goes on
/// the end of the row.)
pub fn make_sequence_clip<'a>(
    steps: &[SequenceStep],
    lookup: impl Fn(&str) -> Option<&'a LoopClip>,
    sample_rate: f64,
    recorded: &str,
) -> Option<LoopClip> {
    let used = used_steps(steps, lookup);
    let first = used.first()?.1;
    let bpm = first.bpm;
    let (l, r, beats) = sequence_audio(&used, bpm);
    let buffer = SampleBuffer::new(l, Some(r), sample_rate);
    let waveform = WaveformOverview::new(&buffer, WAVEFORM_COLUMNS);
    let clips: Vec<&LoopClip> = used.iter().map(|(_, c)| *c).collect();
    let mut seq = LoopClip::new(
        &sequence_id(&buffer),
        &sequence_title(&used),
        &first.source,
        beats,
        bpm,
        first.start_beat,
        Arc::new(buffer),
        waveform,
    );
    seq.steps = Some(used.iter().map(|(s, _)| s.clone()).collect());
    seq.provenance = Some(provenance::sequence(&used, recorded));
    seq.fx = shared_fx(&clips);
    Some(seq)
}
