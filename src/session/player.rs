//! Re-rendering a `Session`: the same engine, the same inputs at the same frames. Port of
//! the schedule half of `Session/SessionPlayer.swift`.
//!
//! `OfflineConductor` steps an engine through time in fixed blocks (apply the inputs due,
//! housekeeping, render a block); here that is the [`Conductor`] trait, and [`SessionPlayer`]
//! decides which events are due before each block. A live run and its replay take exactly the
//! same steps.

use alloc::string::String;
use alloc::vec::Vec;

use super::action::DeckAction;
use super::model::{DeckSource, Session, SessionError, SessionEvent, SessionEventKind, DEMO_ID};
use super::wav::RenderedAudio;

/// `OfflineConductor.blockFrames`.
pub const BLOCK_FRAMES: usize = 128;

/// What the player drives (the engine behind `OfflineConductor` + `ControllerRouter`).
pub trait Conductor {
    /// Frames rendered so far (`dj.frame`).
    fn frame(&self) -> i64;
    /// Apply one recorded event (`SessionPlayer.apply`: hardware MIDI to the router, an
    /// action to `applyAction`).
    fn apply(&mut self, event: &SessionEvent);
    /// One block: housekeeping then render. Returns the block's samples, empty on failure.
    fn step(&mut self) -> (Vec<f32>, Vec<f32>);
}

/// `SessionPlayer`.
pub struct SessionPlayer<'a> {
    pub session: &'a Session,
}

impl<'a> SessionPlayer<'a> {
    pub fn new(session: &'a Session) -> Self {
        SessionPlayer { session }
    }

    /// Every sample the set ever loads, which must be in hand before the first frame.
    pub fn needed_samples(&self) -> Vec<String> {
        let mut needed = Vec::new();
        for source in self.session.decks.values() {
            if let DeckSource::Sample { id, .. } = source {
                needed.push(id.clone());
            }
        }
        for e in &self.session.events {
            if let SessionEventKind::Action(a) = &e.kind {
                match a {
                    DeckAction::Load {
                        source: DeckSource::Sample { id, .. },
                        ..
                    } => needed.push(id.clone()),
                    DeckAction::ClipImport { id, .. } => needed.push(id.clone()),
                    _ => {}
                }
            }
        }
        needed
    }

    /// Check the resolver can supply every needed sample (the demo loop is built in):
    /// `SessionError.missingSample` for the first it cannot.
    pub fn check_samples(&self, has: impl Fn(&str) -> bool) -> Result<(), SessionError> {
        for id in self.needed_samples() {
            if id != DEMO_ID && !has(&id) {
                return Err(SessionError::MissingSample { id });
            }
        }
        Ok(())
    }

    /// The events due at engine `frame`, starting from `*next`: `events[next].frame <= frame`.
    /// Advances `*next` past what it returns.
    pub fn due<'s>(&'s self, next: &mut usize, frame: i64) -> &'s [SessionEvent] {
        let events = &self.session.events;
        let start = *next;
        while *next < events.len() && events[*next].frame <= frame {
            *next += 1;
        }
        &events[start..*next]
    }

    /// `SessionPlayer.render`'s loop: until `session.frames` are rendered, apply what is due
    /// before each block, step the conductor, collect the samples. The result is trimmed to
    /// exactly the session's length.
    pub fn drive<C: Conductor>(&self, c: &mut C) -> RenderedAudio {
        let want = self.session.frames.max(0) as usize;
        let mut left: Vec<f32> = Vec::with_capacity(want);
        let mut right: Vec<f32> = Vec::with_capacity(want);
        let mut next = 0;
        while left.len() < want {
            let frame = c.frame();
            for e in self.due(&mut next, frame) {
                c.apply(e);
            }
            let (l, r) = c.step();
            if l.is_empty() {
                break;
            }
            left.extend(l);
            right.extend(r);
        }
        left.truncate(want);
        right.truncate(want);
        RenderedAudio {
            sample_rate: self.session.sample_rate,
            left,
            right,
        }
    }
}
