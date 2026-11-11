//! Sample decks (`Deck`'s sample mode in `DJEngine.swift`): a file or a loop on the deck's
//! turntable instead of a composed net. The turntable is `crate::deck::turntable` running on
//! the render thread inside [`DeckCore`](super::DeckCore); this is the main thread's half:
//! loading and seeking, as `Deck` does them. The CUE button, the hot cues and the loop buttons
//! (with the deck's quantize) are in [`cues`](super::cues), the beat grid's calls in
//! [`beat`](super::beat).

use std::cell::OnceCell;
use std::sync::Arc;

use crate::deck::beat_grid::{self, BeatGrid};
use crate::deck::demo_loop;
use crate::deck::level_match::SampleLevel;
use crate::deck::turntable::{SampleBuffer, TurntableCommand};
use crate::deck::waveform::WaveformOverview;

use super::core::Command;
use super::engine::Engine;
use crate::host::decode::resample;

/// Columns of the whole-track overview the window draws.
pub const OVERVIEW_COLUMNS: usize = 1200;
/// Hot cue pads per deck.
pub const HOT_CUES: usize = 8;

/// What a sample deck holds on the main thread.
pub struct SampleDeck {
    pub title: String,
    pub buffer: Arc<SampleBuffer>,
    pub overview: WaveformOverview,
    /// The measured (or, for the demo loop, known) tempo.
    pub grid: Option<BeatGrid>,
    /// Where CUE returns to, in samples (unwrapped, as the platter counts).
    pub cue_point: f64,
    pub loop_start: Option<f64>,
    pub loop_end: Option<f64>,
    pub looping: bool,
    /// The built-in demo loop (`Deck.isDemo`, set by [`Engine::load_demo`]): the deck holds
    /// nothing of the user's.
    pub is_demo: bool,
    /// The file the audio was decoded from, when the host says so
    /// ([`Engine::journal_sample_file`]; `Deck.sourceFile`).
    pub source_file: Option<String>,
    /// Does the deck's auto-gain level this audio (`levelSource`)? A clip keeps the level it was
    /// taken at instead.
    level_source: bool,
    /// What the audio measured, on first read (a full pass over it, so a load with auto-gain
    /// off never pays for it).
    level: OnceCell<SampleLevel>,
}

impl SampleDeck {
    /// Is it the built-in demo loop (`Deck.isDemo`)? Told by its title, which nothing else loads
    /// under.
    pub fn is_demo(&self) -> bool {
        self.title == "demo loop"
    }
}

/// Audio made ready to load: the overview and the tempo are measured here, so a file can be
/// decoded and prepared on a worker thread and only handed over on the main one.
pub struct PreparedSample {
    pub title: String,
    pub buffer: Arc<SampleBuffer>,
    pub overview: WaveformOverview,
    pub grid: Option<BeatGrid>,
}

impl PreparedSample {
    /// Any thread: measure `buffer` for a deck (`Deck.load` + `analyze`).
    pub fn new(buffer: SampleBuffer, title: &str) -> PreparedSample {
        let grid = beat_grid::analyze(&buffer.left, buffer.right(), buffer.sample_rate);
        Self::with_grid(Arc::new(buffer), title, grid)
    }

    /// `Deck.load` with the tempo already known (a recorded grid stands in for the analysis, or
    /// none is wanted: `setBeatGrid`, a replay).
    pub fn with_grid(
        buffer: Arc<SampleBuffer>,
        title: &str,
        grid: Option<BeatGrid>,
    ) -> PreparedSample {
        PreparedSample {
            title: title.to_string(),
            overview: WaveformOverview::new(&buffer, OVERVIEW_COLUMNS),
            buffer,
            grid,
        }
    }

    /// This audio at `rate`, as `Deck.decode` hands it to `Deck.load`: Swift's turntable takes no
    /// other rate ("resample before loading"), so whatever reads a file converts it first, and a
    /// buffer that reaches the engine at another rate is converted the same way here (linear
    /// interpolation, [`decode::resample`](crate::host::decode::resample): the one the decode path
    /// uses, so the audio is the same either way). The tempo grid is counted in the buffer's own
    /// samples, so its offset moves with them; the overview is drawn again. At the right rate it
    /// is returned as it is.
    pub fn at_rate(self, rate: f64) -> PreparedSample {
        if self.buffer.sample_rate == rate {
            return self;
        }
        let from = self.buffer.sample_rate;
        let buffer = Arc::new(resample_buffer(&self.buffer, rate));
        PreparedSample {
            title: self.title,
            overview: WaveformOverview::new(&buffer, OVERVIEW_COLUMNS),
            buffer,
            grid: self
                .grid
                .map(|g| BeatGrid::new(g.bpm, g.offset * rate / from, g.confidence)),
        }
    }

    /// `Deck.loadDemo`: the built-in two-bar loop, at its known tempo.
    pub fn demo(sample_rate: f64) -> PreparedSample {
        let buffer = demo_loop::make(sample_rate);
        PreparedSample {
            title: "demo loop".into(),
            overview: WaveformOverview::new(&buffer, OVERVIEW_COLUMNS),
            buffer: Arc::new(buffer),
            grid: Some(BeatGrid::new(demo_loop::BPM, 0.0, 1.0)),
        }
    }
}

/// `buffer` converted to `rate` (both channels, linearly: [`crate::host::decode::resample`]).
pub fn resample_buffer(buffer: &SampleBuffer, rate: f64) -> SampleBuffer {
    SampleBuffer::new(
        resample(&buffer.left, buffer.sample_rate, rate),
        buffer
            .right
            .as_deref()
            .map(|r| resample(r, buffer.sample_rate, rate)),
        rate,
    )
}

impl Engine {
    pub fn is_sample_deck(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.sample.is_some())
    }

    /// A command for the deck's turntable.
    pub fn turntable(&mut self, deck: &str, c: TurntableCommand) -> bool {
        self.command(deck, Command::Turntable(c))
    }

    /// Put prepared audio on the deck's platter (`Deck.load`): the deck becomes a sample deck,
    /// at the top of the audio, cues and loops cleared. The transport is left as it was (a
    /// sample deck that was playing plays on; a net that was playing is stopped by the switch).
    /// Audio at another sample rate is converted to the deck's first ([`PreparedSample::at_rate`]).
    /// False if the deck is unknown.
    pub fn load_sample(&mut self, deck: &str, s: PreparedSample) -> bool {
        self.load_sample_with(deck, s, true)
    }

    /// `load_sample`; `level`: give it the deck's auto-gain (`autoGain`, `SampleLevel`); a clip
    /// plays at the level it was taken at instead (`Deck.load(_:title:level:)`).
    pub fn load_sample_with(&mut self, deck: &str, s: PreparedSample, level: bool) -> bool {
        let Some(i) = self.index(deck) else {
            return false;
        };
        let s = s.at_rate(self.sample_rate);
        // the turntable keeps the tempo fader, the nudge and the transport it had (`Deck.load`
        // does not stop it: a sample loaded onto a playing sample deck plays on from its top);
        // a net deck that was playing is not any more (`leaveNetMode`), and the platter was
        // stopped when the net took the deck
        let was_net = self.decks[i].is_net();
        // (a deck with nothing on it plays the net path here until it gets a platter, where Swift's
        // starts out as a sample deck: either way the FX unit moves to the sample bus now)
        let was_platterless = self.decks[i].sample.is_none();
        let ok = self.command(deck, Command::SampleMode(true))
            && self.turntable(deck, TurntableCommand::Load(s.buffer.clone()));
        if was_platterless {
            // (`leaveNetMode`: the FX unit moves to the sample deck's bus)
            self.sync_fx_unit(deck);
        }
        let d = &mut self.decks[i];
        if was_net {
            d.playing = false;
            // (`leaveNetMode`)
            d.track.fixes = None;
            d.track.pass_applied = false;
        }
        // (`Deck.load` leaves `currentSpec` and `currentRawNets` as they were: `leaveNetMode` only
        // drops the project, the fixes and the pass. The perform golden pins it)
        d.project = None;
        d.sections.clear();
        d.roles.clear();
        d.ring_roles.clear();
        d.ring_shapes.get_mut().clear();
        d.hot_cues.clear();
        d.sample = Some(SampleDeck {
            title: s.title,
            buffer: s.buffer,
            overview: s.overview,
            grid: s.grid,
            cue_point: 0.0,
            loop_start: None,
            loop_end: None,
            looping: false,
            is_demo: false,
            source_file: None,
            level_source: level,
            level: OnceCell::new(),
        });
        // `Deck.load` clears `clipID` (and with it the clip's FX); `play_clip` sets it after
        self.clear_deck_clip(deck);
        self.apply_auto_gain(deck, true);
        ok
    }

    // MARK: level (LevelMatch.swift)

    /// Sample loads play at the app's reference loudness (`SampleLevel::auto_gain_db`); off, the
    /// audio plays at its own level. A deck setting, so a session replays with it; the gain
    /// itself is derived from the audio. Off by default (`Deck.autoGain`).
    pub fn auto_gain(&self, deck: &str) -> bool {
        self.deck(deck).is_some_and(|d| d.auto_gain)
    }

    pub fn set_auto_gain(&mut self, deck: &str, on: bool) {
        let Some(d) = self.deck_mut(deck) else { return };
        if d.auto_gain != on {
            d.auto_gain = on;
            self.apply_auto_gain(deck, false);
        }
    }

    /// What the loaded sample measured (None for a clip, a net, or nothing loaded). Measured on
    /// first read and kept.
    pub fn sample_level(&self, deck: &str) -> Option<SampleLevel> {
        let s = self
            .deck(deck)?
            .sample
            .as_ref()
            .filter(|s| s.level_source)?;
        Some(*s.level.get_or_init(|| SampleLevel::measure(&s.buffer)))
    }

    /// The auto-gain the turntable plays the sample at, dB (0 when off or nothing to level).
    pub fn auto_gain_db(&self, deck: &str) -> f64 {
        if !self.auto_gain(deck) {
            return 0.0;
        }
        self.sample_level(deck).map_or(0.0, |l| l.auto_gain_db())
    }

    pub(crate) fn apply_auto_gain(&mut self, deck: &str, now: bool) {
        let g = 10f64.powf(self.auto_gain_db(deck) / 20.0) as f32;
        self.turntable(deck, TurntableCommand::Gain(g, now));
    }

    /// `Deck.loadDemo`.
    pub fn load_demo(&mut self, deck: &str) -> bool {
        let s = PreparedSample::demo(self.sample_rate);
        let ok = self.load_sample(deck, s);
        if let Some(s) = ok
            .then(|| self.deck_mut(deck).and_then(|d| d.sample.as_mut()))
            .flatten()
        {
            s.is_demo = true;
        }
        ok
    }

    /// `Deck.isDemo`: the deck holds the demo loop, which the next track replaces (auto-mix does
    /// not take it for the user's sample).
    pub fn is_demo(&self, deck: &str) -> bool {
        self.deck(deck)
            .and_then(|d| d.sample.as_ref())
            .is_some_and(|s| s.is_demo)
    }

    /// The playhead inside the audio, in samples (the turntable's, wrapped), as of the last
    /// block.
    pub fn sample_position(&self, deck: &str) -> Option<f64> {
        let d = self.deck(deck)?;
        let s = d.sample.as_ref()?;
        let len = s.buffer.length() as f64;
        let mut p = d.mirror.turntable_position() % len;
        if p < 0.0 {
            p += len;
        }
        Some(p)
    }

    /// Move the playhead (and a hand's target) to `sample`.
    pub fn seek(&mut self, deck: &str, sample: f64) -> bool {
        let ok = self.is_sample_deck(deck) && self.turntable(deck, TurntableCommand::Seek(sample));
        if ok {
            let name = deck.to_uppercase();
            self.note_live(
                format!("seek:{name}"),
                format!("seek {name}"),
                true,
                "seek",
                super::vocoder::live_params([("deck", name.clone().into()), ("sample", crate::json::JsonValue::Number(sample))]),
            );
        }
        ok
    }
}

impl Engine {
    /// A deck's `load` of `source` into the action trace, with its words (a host that loads
    /// audio itself, a file decoded off the main thread, says so): an arrangement then loads the
    /// same audio at the same beat.
    pub fn trace_push_load(
        &mut self,
        deck: &str,
        source: crate::session::model::DeckSource,
        text: String,
    ) {
        if self.trace.is_none() {
            return;
        }
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(super::trace::Traced {
            entry: crate::control::action_log::Entry {
                source: via,
                key: format!("load:{deck}"),
                text,
                step: None,
                fold: false,
            },
            ops: vec![crate::session::arrangement::ArrangeOp::Deck(
                crate::session::action::DeckAction::Load {
                    deck: deck.to_string(),
                    source,
                },
            )],
        });
    }

    /// The content id of the audio on sample deck `deck`.
    pub fn sample_content_id(&self, deck: &str) -> Option<String> {
        self.deck(deck)?
            .sample
            .as_ref()
            .map(|s| s.buffer.content_id())
    }
}
