//! The beat clock a loop starts and stops on (`LoopClock`, `loopClock(for:prefer:)` and
//! `retimeWaiting` of `LoopRecorder.swift`), as functions of what the host reads off its decks
//! and voices: the engine half builds a [`ClockDeck`] per deck and a [`ClockVoice`] per loop
//! voice, calls [`select_clock`], and schedules on the [`LoopClock`] it gets back.
//!
//! Frames are the loop channel's / the decks' engine frames (`Int`), beats are the reference
//! deck's own numbering (beat 0 is its bar 1).

use alloc::string::String;
use alloc::vec::Vec;

use super::clips::LoopClip;

/// How far ahead of the render head a start or stop is scheduled at the least (the command
/// must reach the audio thread first): ~43 ms at 48 kHz (`DJEngine.loopLead`).
pub const LOOP_LEAD: i64 = 2048;

/// How far (frames) a waiting boundary may drift before it is moved: ~1.3 ms, above a sample
/// deck's beat estimate's jitter (`DJEngine.retimeTolerance`).
pub const RETIME_TOLERANCE: i64 = 64;

/// What a start or stop scheduled now on a `beats` line of a clock waits for
/// (`LoopVoice.boundary`): a line every `beats` beats of reference deck `ref_deck`, worked out
/// at that deck's tempo `bpm`; `follow` once that tempo has changed (`retimeWaiting` then
/// keeps it on the deck's line).
#[derive(Clone, Debug, PartialEq)]
pub struct WaitingBoundary {
    pub beats: i64,
    pub ref_deck: String,
    pub bpm: f64,
    pub follow: bool,
}

/// A beat clock for lining loops up: the beat at any deck frame, and the next downbeat (or
/// launch boundary) (`LoopClock`).
#[derive(Clone, Debug, PartialEq)]
pub struct LoopClock {
    /// The deck it is the clock of (None: a loop already going, or nothing).
    pub ref_deck: Option<String>,
    pub origin_frame: i64,
    pub origin_beat: f64,
    /// 0: no clock (anything starts when asked, at its own tempo).
    pub frames_per_beat: f64,
    pub bpm: Option<f64>,
}

impl LoopClock {
    /// No clock at `frame`: the fallback of `loopClock` when nothing plays.
    pub fn none(frame: i64) -> LoopClock {
        LoopClock {
            ref_deck: None,
            origin_frame: frame,
            origin_beat: 0.0,
            frames_per_beat: 0.0,
            bpm: None,
        }
    }

    /// `beat`: the clock's beat at `frame` (0 without a clock).
    pub fn beat(&self, frame: i64) -> f64 {
        if self.frames_per_beat > 0.0 {
            self.origin_beat + (frame - self.origin_frame) as f64 / self.frames_per_beat
        } else {
            0.0
        }
    }

    /// `nextDownbeat(after:)`: the first downbeat (a beat that is a multiple of 4) at or after
    /// `frame`.
    pub fn next_downbeat(&self, frame: i64) -> i64 {
        self.next_boundary(frame, 4)
    }

    /// `nearestBoundary(to:every:)`: the launch line (a beat that is a multiple of `beats`)
    /// nearest `frame`, before or after it; the one ahead on a tie.
    pub fn nearest_boundary(&self, frame: i64, beats: i64) -> i64 {
        if self.frames_per_beat <= 0.0 {
            return frame;
        }
        let q = beats.max(1) as f64;
        let mut back = self.beat(frame) % q;
        if back < 0.0 {
            back += q;
        }
        let ahead = q - back;
        if ahead <= back {
            frame + libm::ceil(ahead * self.frames_per_beat) as i64
        } else {
            frame - libm::floor(back * self.frames_per_beat) as i64
        }
    }

    /// `waitingBoundary(every:)`: what a start or stop scheduled now on a `beats` line of this
    /// clock waits for; None without a reference deck (or a tempo).
    pub fn waiting_boundary(&self, beats: i64) -> Option<WaitingBoundary> {
        let ref_deck = self.ref_deck.clone()?;
        let bpm = self.bpm?;
        Some(WaitingBoundary {
            beats,
            ref_deck,
            bpm,
            follow: false,
        })
    }

    /// `nextBoundary(after:every:)`: the first beat at or after `frame` that is a multiple of
    /// `beats`, counted in the reference's own numbering (beat 0 is its bar 1), so 4-bar
    /// phrases (16) land on bars 1, 5, 9… of the deck, not 4 bars from whenever the button was
    /// pressed. With no clock: `frame` itself.
    pub fn next_boundary(&self, frame: i64, beats: i64) -> i64 {
        if self.frames_per_beat <= 0.0 {
            return frame;
        }
        let q = beats.max(1) as f64;
        let b = self.beat(frame);
        let mut to_go = (q - b % q) % q;
        if to_go < 0.0 {
            to_go += q;
        }
        frame + libm::ceil(to_go * self.frames_per_beat) as i64
    }
}

/// The clock origin a net deck's newest tick anchor gives: `(frame, (tick - 1) / 4)` (the
/// step that begins at `frame` is `tick - 1`, four to a beat). Exact where `beatState` is a
/// block's estimate: the host picks the newest anchor with `tick >= 1`.
pub fn anchor_origin(frame: i64, tick: i64) -> (i64, f64) {
    (frame, (tick - 1) as f64 / 4.0)
}

/// A playing deck as `loopClock` sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockDeck {
    pub name: String,
    pub playing: bool,
    /// The clip it holds, if it is a clip moved onto the deck (`Deck.clipID`).
    pub clip_id: Option<String>,
    /// Its `beatState`: `(bpm, beat)` at its render head; None without a beat clock.
    pub beat_state: Option<(f64, f64)>,
    /// Its render head, engine frames (`renderedFrames`).
    pub rendered_frames: i64,
    /// A net deck's newest tick anchor as [`anchor_origin`] gives it; None for a sample deck
    /// or before the first tick.
    pub anchor: Option<(i64, f64)>,
}

/// A loop voice that is going, as `loopClock` sees it when no deck plays.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockVoice {
    /// The bank says it is sounding (`bank.isActive`).
    pub active: bool,
    /// It has been told to stop (`stopFrame != nil`).
    pub stopping: bool,
    pub clip_id: Option<String>,
    /// Its clip's beats; None when the clip is gone from the row (the voice is then not a
    /// clock, and the search stops there).
    pub clip_beats: Option<i64>,
    /// The clip's own audio length, for a voice that has none loaded yet.
    pub clip_length: usize,
    /// The length of the audio the voice holds (a stretched copy's perhaps; 0: none loaded).
    pub length: f64,
    /// Its playhead in that audio (unwrapped samples).
    pub position: f64,
    /// The tempo it plays at (`LoopVoice.targetBPM`).
    pub target_bpm: f64,
}

/// `loopClock(for:prefer:)`: what a loop lines up with: the deck it was taken from if that
/// plays, else another playing deck, else a loop already going under the mixer, else nothing
/// (it starts on its own).
///
/// - `clip_id` / `clip_source`: the clip being lined up (None for `loopClock(for: nil)`);
/// - `first`: the deck to try before the rest: `prefer`, else the dominant deck when the clip
///   follows the louder deck, else the clip's own deck (the host resolves it: it is the
///   `first` of Swift, and may be None);
/// - `decks`: every deck; candidates are `[first, the clip's source deck]` then the decks by
///   name, and the first that plays, holds some other clip than `clip_id` and has a beat
///   state wins;
/// - `voices`: the loop bank's voices in voice order, for the fallback;
/// - `loop_frames`: the loop channel's rendered frames; `sample_rate` the engine's.
pub fn select_clock(
    clip_id: Option<&str>,
    clip_source: Option<&str>,
    first: Option<&str>,
    decks: &[ClockDeck],
    voices: &[ClockVoice],
    loop_frames: i64,
    sample_rate: f64,
) -> LoopClock {
    let by_name = |n: &str| decks.iter().find(|d| d.name == n);
    let mut sorted: Vec<&ClockDeck> = decks.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut candidates: Vec<&ClockDeck> = Vec::new();
    if let Some(d) = first.and_then(by_name) {
        candidates.push(d);
    }
    if let Some(d) = clip_source.and_then(by_name) {
        candidates.push(d);
    }
    candidates.extend(sorted);
    // `$0.clipID != clip?.id` on optionals: with no clip (`loopClock(for: nil)`), a deck that
    // holds no clip either does not qualify (nil == nil)
    if let Some(d) = candidates
        .into_iter()
        .find(|d| d.playing && d.clip_id.as_deref() != clip_id && d.beat_state.is_some())
    {
        let (bpm, beat) = d.beat_state.unwrap_or((0.0, 0.0));
        let frames_per_beat = 60.0 / bpm * sample_rate;
        let (origin_frame, origin_beat) = d.anchor.unwrap_or((d.rendered_frames, beat));
        return LoopClock {
            ref_deck: Some(d.name.clone()),
            origin_frame,
            origin_beat,
            frames_per_beat,
            bpm: Some(bpm),
        };
    }
    if let Some(v) = voices
        .iter()
        .find(|v| v.active && !v.stopping && v.clip_id.as_deref() != clip_id)
    {
        if let Some(beats) = v.clip_beats {
            // What the voice holds may be a stretched copy: the beat from its own length
            // (published with its playhead, so the two always belong to the same audio). The
            // tempo is the one it plays at, `target_bpm`, as the main thread set it.
            let length = if v.length > 0.0 {
                v.length
            } else {
                v.clip_length as f64
            };
            let samples_per_beat = length / beats as f64;
            let mut pos = v.position % length;
            if pos < 0.0 {
                pos += length;
            }
            let bpm = v.target_bpm;
            return LoopClock {
                ref_deck: None,
                origin_frame: loop_frames,
                origin_beat: pos / samples_per_beat,
                frames_per_beat: 60.0 / bpm * sample_rate,
                bpm: Some(bpm),
            };
        }
    }
    LoopClock::none(loop_frames)
}

// MARK: a waiting start or stop follows its deck's tempo

/// A loop voice with a start or stop waiting, as `retimeWaiting` reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct RetimeVoice {
    /// `LoopVoice.boundary`: what its pending start or stop waits for.
    pub boundary: Option<WaitingBoundary>,
    /// Set once it is told to stop (it goes out at this frame).
    pub stop_frame: Option<i64>,
    /// The frame it was told to come in at.
    pub start_frame: Option<i64>,
    /// Samples in the audio it was started with.
    pub buffer_length: f64,
    /// The bank says it is sounding.
    pub active: bool,
}

/// What a waiting start or stop moves to.
#[derive(Clone, Debug, PartialEq)]
pub enum RetimeMove {
    /// Re-send the stop at `at` (fade: the clip's, at the voice's tempo), `stop_frame = at`.
    Stop { at: i64 },
    /// Seek the voice to `seek` samples, re-send the start at `at`, `start_frame = at`.
    Start { at: i64, seek: f64 },
}

/// `retimeWaiting`'s decision. None: the guards before the boundary is touched failed (no
/// boundary, a clock of another deck, no tempo, a tempo as scheduled and not following).
/// Otherwise `follow` is true (the voice's boundary is marked `follow = true`, whether or not
/// anything moves) and `moved` is what to do, if anything.
///
/// `clock` is `loopClock(for: clip)`; `now` the loop channel's rendered frames. A start or stop
/// moves only while both frames are well ahead of the render head (`now + 2 * LOOP_LEAD`:
/// the audio thread cannot have reached either), only while the same deck is still the clip's
/// clock, and only by less than a quarter of the line spacing (`boundary.beats / 4 *
/// frames_per_beat`): a tempo change moves the line that little, while a looping sample,
/// whose beat count starts again on every pass, would otherwise seem to put its lines
/// anywhere. A moved start is seeked to the clip's phase at its new frame.
pub fn retime_waiting(
    voice: &RetimeVoice,
    clip: &LoopClip,
    clock: &LoopClock,
    now: i64,
) -> Option<Retime> {
    let boundary = voice.boundary.as_ref()?;
    let bpm = clock.bpm?;
    if clock.ref_deck.as_deref() != Some(boundary.ref_deck.as_str()) || clock.frames_per_beat <= 0.0
    {
        return None;
    }
    if !(boundary.follow || (bpm - boundary.bpm).abs() > 1e-9) {
        return None;
    }
    let safe = now + 2 * LOOP_LEAD;
    let reach = boundary.beats as f64 / 4.0 * clock.frames_per_beat;
    let moved = |old: i64, new: i64| -> bool {
        let d = (new - old).abs();
        old > safe && d > RETIME_TOLERANCE && (d as f64) < reach
    };
    let mut result = Retime {
        follow: true,
        moved: None,
    };
    if let Some(stop) = voice.stop_frame {
        let at = clock
            .nearest_boundary(stop, boundary.beats)
            .max(now + LOOP_LEAD);
        if voice.active && moved(stop, at) {
            result.moved = Some(RetimeMove::Stop { at });
        }
        return Some(result);
    }
    let Some(start) = voice.start_frame else {
        return Some(result);
    };
    if voice.active || voice.buffer_length <= 0.0 {
        return Some(result);
    }
    let at = clock
        .nearest_boundary(start, boundary.beats)
        .max(now + LOOP_LEAD);
    if !moved(start, at) {
        return Some(result);
    }
    let beats = clip.beats as f64;
    let origin = if boundary.ref_deck == clip.source {
        clip.start_beat
    } else {
        0.0
    };
    let mut b = (clock.beat(at) - origin) % beats;
    if b < 0.0 {
        b += beats;
    }
    result.moved = Some(RetimeMove::Start {
        at,
        seek: b * voice.buffer_length / beats,
    });
    Some(result)
}

/// The outcome of [`retime_waiting`] past its first guards.
#[derive(Clone, Debug, PartialEq)]
pub struct Retime {
    /// Mark the voice's boundary `follow = true`.
    pub follow: bool,
    pub moved: Option<RetimeMove>,
}
