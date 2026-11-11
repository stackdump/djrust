//! Captures a performance: the engine's starting state, then every input with the frame it
//! arrived on. Port of the pure model of `Session/SessionRecorder.swift`; attaching to an
//! engine (`SessionRecorder.start(on:)`, `DJEngine.perform`) is the host's job.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::action::DeckAction;
use super::model::{
    DeckParams, DeckSource, Session, SessionError, SessionEvent, SessionEventKind, DEMO_ID,
};
use super::types::{AutoMixState, BeatGrid};
use crate::json::JsonValue;
use crate::spec::TrackSpec;

/// What a deck is doing, as far as "may a session start from here" is concerned
/// (`Deck.sourceDescriptor`).
#[derive(Clone, Debug)]
pub enum DeckSnapshot {
    /// A net deck: either a composed spec or a raw-nets payload, or neither (hand-built).
    Net {
        playing: bool,
        tick: i64,
        has_hot_cues: bool,
        spec: Option<TrackSpec>,
        raw_nets: Option<JsonValue>,
    },
    /// A sample deck; `buffer_id` None = nothing loaded.
    Sample {
        playing: bool,
        /// The platter's unwrapped position and the CUE point, in samples (`turntable.position`,
        /// `cuePoint`: Doubles, and a fraction of a sample is not at rest).
        position: f64,
        has_hot_cues: bool,
        looping: bool,
        cue_point: f64,
        buffer_id: Option<String>,
        is_demo: bool,
        title: String,
        grid: Option<BeatGrid>,
    },
}

/// `Deck.sourceDescriptor()`: what the deck holds, as a session can name it; a deck that is
/// moving, or that holds something a session can't name, is refused.
pub fn source_descriptor(name: &str, d: &DeckSnapshot) -> Result<DeckSource, SessionError> {
    let at_rest = || SessionError::NotAtRest { deck: name.into() };
    match d {
        DeckSnapshot::Net {
            playing,
            tick,
            has_hot_cues,
            spec,
            raw_nets,
        } => {
            if *playing {
                return Err(at_rest());
            }
            let source = if let Some(spec) = spec {
                DeckSource::Net(spec.clone())
            } else if let Some(p) = raw_nets {
                DeckSource::RawNets(p.clone())
            } else {
                return Err(SessionError::Unrecordable { deck: name.into() });
            };
            if *tick != 0 || *has_hot_cues {
                return Err(at_rest());
            }
            Ok(source)
        }
        DeckSnapshot::Sample {
            playing,
            position,
            has_hot_cues,
            looping,
            cue_point,
            buffer_id,
            is_demo,
            title,
            grid,
        } => {
            if *playing {
                return Err(at_rest());
            }
            let Some(buffer_id) = buffer_id else {
                return Ok(DeckSource::None);
            };
            if *position != 0.0 || *has_hot_cues || *looping || *cue_point != 0.0 {
                return Err(at_rest());
            }
            Ok(DeckSource::Sample {
                id: if *is_demo {
                    DEMO_ID.into()
                } else {
                    buffer_id.clone()
                },
                title: title.clone(),
                grid: *grid,
            })
        }
    }
}

/// `SessionRecorder`.
#[derive(Clone, Debug)]
pub struct SessionRecorder {
    start_frame: i64,
    sample_rate: f64,
    decks: BTreeMap<String, DeckSource>,
    params: BTreeMap<String, DeckParams>,
    crossfader: f64,
    master: f64,
    loop_gain: f64,
    auto_mix: AutoMixState,
    events: Vec<SessionEvent>,
}

impl SessionRecorder {
    /// Begin recording at engine frame `start_frame`. The decks' starting contents are
    /// whatever [`source_descriptor`] returned for each; `auto_mix` is the engine's
    /// `autoMix.start`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        start_frame: i64,
        sample_rate: f64,
        decks: BTreeMap<String, DeckSource>,
        params: BTreeMap<String, DeckParams>,
        crossfader: f64,
        master: f64,
        loop_gain: f64,
        auto_mix: &AutoMixState,
    ) -> Self {
        SessionRecorder {
            start_frame,
            sample_rate,
            decks,
            params,
            crossfader,
            master,
            loop_gain,
            auto_mix: auto_mix.start(),
            events: Vec::new(),
        }
    }

    fn append(&mut self, engine_frame: i64, kind: SessionEventKind) {
        self.events.push(SessionEvent {
            frame: engine_frame - self.start_frame,
            kind,
        });
    }

    /// Hardware MIDI, recorded by the controller router.
    pub fn record_midi(&mut self, engine_frame: i64, port: &str, bytes: &[u8]) {
        self.append(
            engine_frame,
            SessionEventKind::Midi {
                port: port.into(),
                bytes: bytes.to_vec(),
            },
        );
    }

    /// Everything else, recorded by `DJEngine.perform`.
    pub fn record(&mut self, engine_frame: i64, action: DeckAction) {
        self.append(engine_frame, SessionEventKind::Action(action));
    }

    /// Stop recording and return the session.
    pub fn finish(self, engine_frame: i64) -> Session {
        Session {
            sample_rate: self.sample_rate,
            frames: engine_frame - self.start_frame,
            decks: self.decks,
            params: self.params,
            crossfader: self.crossfader,
            master: self.master,
            loop_gain: self.loop_gain,
            auto_mix: self.auto_mix,
            events: self.events,
        }
    }

    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }
}
