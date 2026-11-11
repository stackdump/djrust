//! Where a composed track waits for its deck (`DeckGenerator.swift`: `SwapStart`,
//! `NextTrackSlot`, `LookAheadSlot`): the pure state the deck's compose bookkeeping keeps.
//!
//! Swift guards each slot with a `Mutex` and composes on a dispatch queue / detached task.
//! Here a slot is a plain struct the host drives: it asks the slot for a [`request`], composes
//! wherever it likes (a thread, inline), and hands the result back with the token the request
//! gave it ([`complete`]). A newer request, a cancel or a take bumps the token, so a compose
//! that finishes late is refused and comes back to the host to drop. The slot never holds a
//! lock, so what it is handed back (the old track) is released by the host outside its own.
//!
//! `T` is the prepared track (Swift's `PreparedTrack`: the composed project compiled and ready
//! for a deck), which lives in the host.
//!
//! [`request`]: NextTrackSlot::request
//! [`complete`]: NextTrackSlot::complete

use alloc::string::String;

use crate::spec::TrackSpec;
use crate::wave::runner::RoleFilter;

/// Where a track swapped onto a playing deck starts (`SwapStart`; `Deck.generate`,
/// `Deck.swap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStart {
    /// Its first tick (a new song, a loop).
    Top,
    /// The bar after the one the deck is in when it is prepared: the transport keeps going.
    NextBar,
    /// This tick (a phrase boundary worked out beforehand: Wander).
    Tick(i64),
}

impl SwapStart {
    /// A `.tick` the deck has reached at `now` (`generate` re-prepares such a track).
    pub fn passed(self, now: i64) -> bool {
        match self {
            SwapStart::Tick(t) => t > 0 && now >= t,
            _ => false,
        }
    }

    /// The tick to start at, given the deck's tick now.
    pub fn tick(self, now: i64) -> i64 {
        match self {
            SwapStart::Top => 0,
            SwapStart::NextBar => (now / 16 + 1) * 16,
            SwapStart::Tick(t) => t,
        }
    }
}

/// A set of names, as Swift's `Set<String>` compares them: in any order, once each.
fn same_names(a: &[String], b: &[String]) -> bool {
    let has = |xs: &[String], x: &String| xs.iter().any(|y| y == x);
    a.iter().all(|x| has(b, x)) && b.iter().all(|x| has(a, x))
}

/// `RoleFilter ==`: Swift's `mute` and `solo` are sets, this port's are lists.
fn same_filter(a: &Option<RoleFilter>, b: &Option<RoleFilter>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            same_names(&a.mute, &b.mute)
                && match (&a.solo, &b.solo) {
                    (None, None) => true,
                    (Some(x), Some(y)) => same_names(x, y),
                    _ => false,
                }
        }
        _ => false,
    }
}

struct Ready<T> {
    spec: TrackSpec,
    track: T,
}

/// Holds the next track for a deck: composing in the background, then ready
/// (`NextTrackSlot`).
pub struct NextTrackSlot<T> {
    ready: Option<Ready<T>>,
    in_flight: Option<TrackSpec>,
    token: u64,
}

impl<T> Default for NextTrackSlot<T> {
    fn default() -> Self {
        NextTrackSlot {
            ready: None,
            in_flight: None,
            token: 0,
        }
    }
}

impl<T> NextTrackSlot<T> {
    pub fn new() -> NextTrackSlot<T> {
        NextTrackSlot::default()
    }

    /// The spec of the track that is ready.
    pub fn ready_spec(&self) -> Option<&TrackSpec> {
        self.ready.as_ref().map(|r| &r.spec)
    }

    /// The spec being composed.
    pub fn pending_spec(&self) -> Option<&TrackSpec> {
        self.in_flight.as_ref()
    }

    /// A track is ready or being composed.
    pub fn is_busy(&self) -> bool {
        self.ready.is_some() || self.in_flight.is_some()
    }

    /// Begin a background compose; a newer request supersedes an older one (and drops what
    /// was ready). Returns the token the composed track must come back with. The track that
    /// was ready, if any, is returned for the host to release.
    pub fn request(&mut self, spec: TrackSpec) -> (u64, Option<T>) {
        self.token += 1;
        let old = self.ready.take().map(|r| r.track);
        self.in_flight = Some(spec);
        (self.token, old)
    }

    /// A compose finished: `track` becomes the ready one unless a newer request or a cancel
    /// superseded it, in which case it is handed back (`Err`) to be dropped.
    pub fn complete(&mut self, token: u64, track: T) -> Result<(), T> {
        if self.token != token {
            return Err(track);
        }
        match self.in_flight.take() {
            Some(spec) => {
                self.ready = Some(Ready { spec, track });
                Ok(())
            }
            None => Err(track),
        }
    }

    /// Compose on the calling thread (replay mode, `requestSync`): `track` is the composed
    /// `spec`, ready at once. Returns the track it replaced, if any.
    pub fn request_sync(&mut self, spec: TrackSpec, track: T) -> Option<T> {
        self.token += 1;
        self.in_flight = None;
        self.ready
            .replace(Ready { spec, track })
            .map(|old| old.track)
    }

    /// The ready track, leaving the slot's ready side empty (what is composing is not
    /// disturbed).
    pub fn take(&mut self) -> Option<T> {
        self.ready.take().map(|r| r.track)
    }

    /// Forget what is ready and what is composing (a compose still running is refused when it
    /// finishes). Returns the ready track for the host to release.
    pub fn cancel(&mut self) -> Option<T> {
        self.token += 1;
        self.in_flight = None;
        self.ready.take().map(|r| r.track)
    }
}

/// What a Wander look-ahead was composed for (`LookAheadSlot.Key`): the move it reads ahead
/// (its step), the swap it plans, where that starts, and the deck's role filter. The move
/// reuses the look-ahead only when all four match.
#[derive(Clone, Debug)]
pub struct LookAheadKey {
    pub step: i64,
    pub spec: TrackSpec,
    pub start: SwapStart,
    pub role_filter: Option<RoleFilter>,
}

impl PartialEq for LookAheadKey {
    fn eq(&self, other: &Self) -> bool {
        self.step == other.step
            && self.spec == other.spec
            && self.start == other.start
            && same_filter(&self.role_filter, &other.role_filter)
    }
}

/// A look-ahead taken by its move (`LookAheadSlot.Taken`): the track if it had finished, and
/// the token of the compose to wait for if it had not (Swift hands over the `Task`).
#[derive(Debug)]
pub struct Taken<T> {
    pub ready: Option<T>,
    pub token: u64,
}

/// A look-ahead asked for (`LookAheadSlot.request`).
#[derive(Debug)]
pub struct LookAheadRequest<T> {
    /// What the composed track must come back with.
    pub token: u64,
    /// The tick to compose it starting at: `key.start.tick(0)`, so `.top` or `.tick`, never the
    /// deck's clock.
    pub start_tick: i64,
    /// What the slot held before, for the host to release.
    pub replaced: Option<T>,
}

/// Wander's look-ahead (`Deck.lookAheadWander`): the swap the walk's next move plans, composed
/// a bar before the move so it is ready when the move is taken. One per deck; a newer request
/// supersedes it, and whatever the move does not take is dropped (its samplers pin bank slots
/// until then).
pub struct LookAheadSlot<T> {
    key: Option<LookAheadKey>,
    composing: bool,
    ready: Option<T>,
    token: u64,
}

impl<T> Default for LookAheadSlot<T> {
    fn default() -> Self {
        LookAheadSlot {
            key: None,
            composing: false,
            ready: None,
            token: 0,
        }
    }
}

impl<T> LookAheadSlot<T> {
    pub fn new() -> LookAheadSlot<T> {
        LookAheadSlot::default()
    }

    /// What the slot holds (composed or composing); None when empty.
    pub fn key(&self) -> Option<&LookAheadKey> {
        self.key.as_ref()
    }

    /// The compose of the held key has finished.
    pub fn is_ready(&self) -> bool {
        self.ready.is_some()
    }

    /// The composed track, once the compose has finished (what a test reads the look-ahead's
    /// preparation off; a move takes it with [`take`](Self::take)).
    pub fn ready_track(&self) -> Option<&T> {
        self.ready.as_ref()
    }

    /// The token of the compose the slot holds (the one to wait for), if any.
    pub fn task(&self) -> Option<u64> {
        self.composing.then_some(self.token)
    }

    /// Compose `key.spec` in the background, replacing what the slot held.
    pub fn request(&mut self, key: LookAheadKey) -> LookAheadRequest<T> {
        let start_tick = key.start.tick(0);
        self.token += 1;
        let replaced = self.ready.take();
        self.key = Some(key);
        self.composing = true;
        LookAheadRequest {
            token: self.token,
            start_tick,
            replaced,
        }
    }

    /// The composed track of `token` is in: kept unless the slot moved on (a newer request, a
    /// take, a drop), in which case it is handed back (`Err`). A move that took the compose
    /// (`Taken::token`) gets its track from whoever composed it, not from the slot.
    pub fn complete(&mut self, token: u64, track: T) -> Result<(), T> {
        if self.token == token && self.composing {
            self.ready = Some(track);
            Ok(())
        } else {
            Err(track)
        }
    }

    /// The look-ahead for `key`, off the slot; None when it holds another (or none). Either
    /// way the slot is empty after.
    pub fn take(&mut self, key: &LookAheadKey) -> Option<Taken<T>> {
        let was = self.token;
        let held = self.key.take();
        let composing = self.composing;
        let ready = self.ready.take();
        self.clear();
        if held.as_ref() == Some(key) && composing {
            Some(Taken { ready, token: was })
        } else {
            None
        }
    }

    /// `drop()`: let go of what the slot holds; the track, if it was composed, is returned for
    /// the host to release.
    pub fn drop_held(&mut self) -> Option<T> {
        let ready = self.ready.take();
        self.clear();
        ready
    }

    /// `clear()`: empty, with a newer token so a compose still running is refused.
    fn clear(&mut self) {
        self.key = None;
        self.composing = false;
        self.ready = None;
        self.token += 1;
    }
}
