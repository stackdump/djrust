//! A track composing on a worker for a deck, for a host that loads it through `perform`.
//!
//! Swift's `AppModel.prepareAndLoad` builds the `PreparedTrack` off the main thread and then
//! loads it with `perform(.load, prepared:)`, so the load is recorded, journalled and read by
//! auto-mix as a poke. [`ComposeJob`] is the first half (the worker) and [`ComposeJob::poll`] the
//! second; [`ComposeJobs`] is the host's list of them, with the one rule the bank needs.
//!
//! **A deck composes one track at a time.** A newer ask for a deck that is composing waits behind
//! the one in flight (the latest ask wins, as the seed wheel's does, and a deck never has two
//! workers); when the older one finishes its track is handed back ([`Engine::give_back_track`])
//! instead of loaded, and only then does the newer one start. (The samplers a compose loads wait
//! in the deck's bank until the deck is handed a project, so a dropped track loses none; the rule
//! was first written when a prepared track carried them, and it still keeps the work bounded.)

use crate::session::action::DeckAction;
use crate::session::model::DeckSource;
use crate::TrackSpec;

use super::engine::Engine;
use super::perform::{Applied, PerformCtx};
use super::track::{PreparedTrack, Task, TrackSource};

/// A track composing on a worker thread for a deck. Not loaded until [`poll`](ComposeJob::poll)
/// is asked, from the thread that owns the engine.
pub struct ComposeJob {
    pub deck: String,
    pub spec: TrackSpec,
    task: Task<Option<PreparedTrack>>,
    /// [`ComposeJobs::cancel`]led: it ends given back, not loaded.
    cancelled: bool,
}

/// Where a [`ComposeJob`] is.
#[derive(Debug, PartialEq)]
pub enum Landed {
    /// Still composing.
    Pending,
    /// Loaded through `perform`, which answered this. (A load `perform` turned down gave the
    /// track back to the bank itself.)
    Loaded(Applied),
    /// A newer ask for the deck superseded it: the composed track was handed back
    /// ([`Engine::give_back_track`]), unloaded.
    Superseded,
    /// The source made no project (`PreparedTrack.init?` was nil): there is no track.
    NotComposed,
    /// The worker died (a panic in a compose), so there is no track.
    Lost,
}

impl ComposeJob {
    /// Start composing `spec` for `deck` on a new thread, from a snapshot of what the deck
    /// compiles with ([`Engine::preparer`]). None if there is no such deck.
    pub fn start(e: &Engine, deck: &str, spec: TrackSpec) -> Option<ComposeJob> {
        let preparer = e.preparer(deck)?;
        let source = TrackSource::Net(spec.clone());
        let task = Task::spawn(move || PreparedTrack::from_source(&source, &preparer, 0));
        Some(ComposeJob {
            deck: deck.into(),
            spec,
            task,
            cancelled: false,
        })
    }

    /// Has the worker finished (or died)?
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// The end of the job, if the worker has finished: the track is taken and either loaded
    /// (`wanted`: `perform(.load(deck, .net(spec)), prepared:)`) or, when it is not wanted any
    /// more, given back to the deck's bank. [`Landed::Pending`] while it composes.
    pub fn poll(&self, e: &mut Engine, wanted: bool) -> Landed {
        if !self.task.is_finished() {
            return Landed::Pending;
        }
        match self.task.try_take() {
            Some(Some(track)) if wanted => Landed::Loaded(e.perform(
                &DeckAction::Load {
                    deck: self.deck.clone(),
                    source: DeckSource::Net(self.spec.clone()),
                },
                PerformCtx {
                    prepared: Some(track),
                    ..PerformCtx::default()
                },
            )),
            Some(Some(track)) => {
                e.give_back_track(&self.deck, track);
                Landed::Superseded
            }
            Some(None) => Landed::NotComposed,
            None => Landed::Lost,
        }
    }
}

/// How an ask ended, from [`ComposeJobs::poll`].
#[derive(Debug, PartialEq)]
pub struct Landing {
    pub deck: String,
    pub spec: TrackSpec,
    /// Never [`Landed::Pending`]: a job still composing is not reported.
    pub landed: Landed,
}

/// The composes in flight for a host's decks: at most one a deck (see the module docs), and the
/// newest ask behind it.
#[derive(Default)]
pub struct ComposeJobs {
    running: Vec<ComposeJob>,
    /// What each deck is to compose after the job in flight: the newest ask, by deck.
    asked: Vec<(String, TrackSpec)>,
}

impl ComposeJobs {
    pub fn new() -> ComposeJobs {
        ComposeJobs::default()
    }

    /// Ask for `spec` on `deck`: composing at once if the deck is not composing, else after the
    /// one in flight, replacing any ask already waiting behind it. False if there is no such
    /// deck.
    pub fn ask(&mut self, e: &Engine, deck: &str, spec: TrackSpec) -> bool {
        if self.is_composing(deck) {
            self.asked.retain(|(d, _)| d != deck);
            self.asked.push((deck.into(), spec));
            return true;
        }
        match ComposeJob::start(e, deck, spec) {
            Some(job) => {
                self.running.push(job);
                true
            }
            None => false,
        }
    }

    /// Drop `deck`'s waiting ask and let the compose in flight end unloaded (given back to the
    /// bank): the deck loaded something else itself (a share link) that a compose landing later
    /// must not replace.
    pub fn cancel(&mut self, deck: &str) {
        self.asked.retain(|(d, _)| d != deck);
        for j in self.running.iter_mut().filter(|j| j.deck == deck) {
            j.cancelled = true;
        }
    }

    /// Is a track composing, or waiting to, for `deck`?
    pub fn is_composing(&self, deck: &str) -> bool {
        self.running.iter().any(|j| j.deck == deck)
    }

    /// Has the compose in flight for `deck` finished, so the next [`poll`](Self::poll) ends it?
    pub fn is_ready(&self, deck: &str) -> bool {
        self.running
            .iter()
            .any(|j| j.deck == deck && j.is_finished())
    }

    /// Land what the workers have finished (call it from the host's frame): each finished job's
    /// track is loaded through `perform`, unless a newer ask for its deck is waiting, which
    /// gives it back to the bank and starts after it. Returns what ended, in the order the jobs
    /// were started.
    pub fn poll(&mut self, e: &mut Engine) -> Vec<Landing> {
        let mut ended = Vec::new();
        let mut i = 0;
        while i < self.running.len() {
            if !self.running[i].is_finished() {
                i += 1;
                continue;
            }
            let job = self.running.remove(i);
            let newer = self
                .asked
                .iter()
                .position(|(d, _)| *d == job.deck)
                .map(|at| self.asked.remove(at).1);
            let landed = job.poll(e, newer.is_none() && !job.cancelled);
            // the older track is handed back before the next worker starts
            if let Some(spec) = newer {
                if let Some(next) = ComposeJob::start(e, &job.deck, spec) {
                    self.running.insert(i, next);
                    i += 1;
                }
            }
            ended.push(Landing {
                deck: job.deck,
                spec: job.spec,
                landed,
            });
        }
        ended
    }
}
