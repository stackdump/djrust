//! How the loops under the mixer sound (`LoopSound.swift`): the fade a loop comes in and goes
//! out with, mute and solo as gains, and the key-lock decision (when a stretched copy is wanted,
//! rendered, swapped in or dropped). Pure rules; the engine state they hang on (the voices, the
//! stretch cache, the worker that renders) is the host's.
//!
//! Mute and solo are gains: a muted clip's voice (or, while any clip is soloed, every voice that
//! is not) goes to level 0 and keeps playing, so it comes back in phase. The bank ramps every
//! level change over ~5 ms and every start / stop over the fade (`LoopBank`).
//!
//! Key lock is done offline. The bank's voices are varispeed turntables: a clip at 1.05x its
//! tempo plays 5 % sharp. A locked clip whose tempo is more than [`KEY_LOCK_TOLERANCE`] off its
//! own gets a time-stretched copy at that tempo ([`crate::fx::time_stretch::wsola`]), rendered
//! off the main thread, and the voice swaps to it in phase at the next block
//! (`TurntableCommand::Swap`), its pitch fader at 0. Until the copy is ready it plays
//! varispeed; when the tempo drifts on by more than the tolerance (and holds there for
//! [`STRETCH_DEBOUNCE`]), it is rendered again.

// One `LoopFade`: the clip model's (`deck::clips`), which also keeps the per-clip settings.
pub use super::clips::LoopFade;

/// `DJEngine.loopFadeFrames(_:bpm:short:)`: the fade in frames for a loop playing at `bpm`;
/// `short`: a start or stop that is not on a downbeat (a fresh take, a clip moved to a deck),
/// which only de-clicks.
pub fn loop_fade_frames(fade: LoopFade, bpm: f64, short: bool, sample_rate: f64) -> i64 {
    let declick = (sample_rate * 0.01) as i64;
    let beat = if bpm > 0.0 {
        (60.0 / bpm * sample_rate) as i64
    } else {
        declick
    };
    let frames = match fade {
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

/// `DJEngine.audible(_:at:)`: level `gain` for a clip as its voice plays it: 0 when muted or
/// soloed out (`any_solo`: some clip is soloed; `soloed`: this one is).
pub fn audible(muted: bool, any_solo: bool, soloed: bool, gain: f64) -> f64 {
    if muted || (any_solo && !soloed) {
        0.0
    } else {
        gain
    }
}

/// Key lock re-renders when the tempo is this far (as a ratio) from the audio's: 0.5 %.
pub const KEY_LOCK_TOLERANCE: f64 = 0.005;
/// ...and has held there for this long (seconds), so a tempo fader being moved renders once.
pub const STRETCH_DEBOUNCE: f64 = 0.25;

/// What `serviceKeyLock` reads of voice `v` and its clip.
#[derive(Clone, Copy, Debug)]
pub struct KeyLockState {
    /// Does the clip keep its pitch at another tempo (its own setting, else the row's)?
    pub locked: bool,
    /// The clip's own tempo.
    pub clip_bpm: f64,
    /// The tempo the voice is to play at, and the tempo of the audio it has loaded.
    pub target_bpm: f64,
    pub buffer_bpm: f64,
    /// A render for it is in flight.
    pub stretching: bool,
    /// The tempo a stretch is wanted for and the frame it was first wanted at.
    pub want: Option<(f64, i64)>,
    /// The tempo of the clip's cached stretched copy, if it has one.
    pub cached_bpm: Option<f64>,
    pub now: i64,
    pub sample_rate: f64,
}

/// What `serviceKeyLock` does about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLockStep {
    /// Not wanted: forget any wanted stretch; `swap_back`: the voice has a stretched copy loaded
    /// and goes back to the clip's own audio.
    Own { swap_back: bool },
    /// Wanted, but the audio loaded is already at the tempo: forget any wanted stretch.
    Settled,
    /// A render is in flight: leave it alone (the next pass sees any drift).
    Wait,
    /// Swap to the cached stretched copy.
    UseCached,
    /// Tempo has held for the debounce: render a stretched copy now.
    Render,
    /// First wanted (or the wanted tempo moved): remember `(target_bpm, now)`.
    Want,
    /// Wanted at this tempo, still inside the debounce.
    Hold,
}

/// `DJEngine.serviceKeyLock(_:_:now:)`'s decision.
pub fn key_lock_step(s: &KeyLockState) -> KeyLockStep {
    let tol = KEY_LOCK_TOLERANCE;
    let stretch = s.locked && (s.target_bpm / s.clip_bpm - 1.0).abs() > tol;
    if !stretch {
        return KeyLockStep::Own {
            swap_back: s.buffer_bpm != s.clip_bpm,
        };
    }
    let off_tempo = (s.target_bpm / s.buffer_bpm - 1.0).abs() > tol;
    if !off_tempo {
        return KeyLockStep::Settled;
    }
    if s.stretching {
        return KeyLockStep::Wait;
    }
    if let Some(c) = s.cached_bpm {
        if (s.target_bpm / c - 1.0).abs() <= tol {
            return KeyLockStep::UseCached;
        }
    }
    if let Some((bpm, since)) = s.want {
        if (bpm / s.target_bpm - 1.0).abs() < 0.001 {
            return if (s.now - since) as f64 >= STRETCH_DEBOUNCE * s.sample_rate {
                KeyLockStep::Render
            } else {
                KeyLockStep::Hold
            };
        }
    }
    KeyLockStep::Want
}

/// `DJEngine.loopAudio(_:at:)`: does a key-locked clip starting at `bpm` take a stretched copy
/// already made for about that tempo (`cached_bpm`) instead of its own audio?
pub fn use_cached_copy(locked: bool, clip_bpm: f64, bpm: f64, cached_bpm: Option<f64>) -> bool {
    locked
        && (bpm / clip_bpm - 1.0).abs() > KEY_LOCK_TOLERANCE
        && cached_bpm.is_some_and(|c| (bpm / c - 1.0).abs() <= KEY_LOCK_TOLERANCE)
}

/// `finishStretch`: the copy holds the clip's beats, so its tempo follows from its length.
pub fn stretched_bpm(clip_bpm: f64, clip_length: usize, stretched_length: usize) -> f64 {
    clip_bpm * clip_length as f64 / stretched_length as f64
}
