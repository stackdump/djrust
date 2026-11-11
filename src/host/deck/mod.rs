//! Live net decks on cross-platform crates (PORTING.md phase 3): the Rust twin of
//! `WaveDeckSource.swift`, the net half of `DeckRenderer.swift` and `DJEngine.swift`'s mixer.
//!
//! - [`core`]: one deck on the render thread: a net deck's runner -> samplers -> brake -> master
//!   chain -> limiter, or a sample deck's turntable -> clip FX -> its own master bus -> fx
//!   limiter; its commands, its loudness meter and the mirrors it publishes (the macro and
//!   marking records, the fire and anchor rings)
//! - [`sample`]: sample decks: a file or a loop on the deck's turntable, and the platter
//! - [`cues`]: CUE, the hot cues and the loop buttons, on a sample deck (samples) and a net deck
//!   (ticks), with the deck's quantize
//! - [`beat`]: the beat clock: the tempo fader and the nudge, `sync`, `follow`, the grid
//! - [`relocate`]: a net deck's clock moved to another tick; section launches
//! - [`views`]: what a display draws from a deck: ring snapshot, section timeline, waveform
//!   overlay
//! - [`samplers`]: sampled instruments inside a deck (SF2 through rustysynth)
//! - [`engine`]: the mixer (crossfader, cue) and the main-thread handle that compiles and
//!   sends
//! - [`surface`]: controllers drive the engine (`SurfaceHost`)
//! - [`perform`]: `Engine::perform(DeckAction)`, the one applier, and the session recorder's hooks
//! - [`net`]: playing a net deck: live keys, the sound panel's voice settings, the kit and the
//!   readers of what a deck plays (the roles, STEMS, Hits, palette and role voice are
//!   [`moves`]'s)
//! - [`fxdeck`]: the FX unit on a deck, and where a quantised move lands
//! - [`track`]: the track pipeline: `PreparedTrack`, `generate` / `swap` / `stepSeed`, the next
//!   track and the look-ahead slots, `service`
//! - [`offline`]: `OfflineConductor`, the fixed-block stepping of an offline engine (sessions, replays)
//! - [`journal`]: the journal's half of the engine: a checkpoint read off it, restored onto it, a reset
//! - [`retune`]: a loop's instruments tuned without taking it again (`ClipRetune.render`)
//! - [`wander`]: a deck's walk: its context, the look-ahead, the moves it takes; [`moves`]: the
//!   pokes those moves are made of (roles, STEMS, stingers, palette, role voice, layers)
//! - [`minecraft_events`]: gameplay fires Hits: the engine half of the `minecraftEvents` control
//!   method (an `mc_recent_events` window mapped to the decks' stingers)
//! - [`host`]: what a live host sets on a fresh engine (`deterministic` off, sample auto-gain on)
//! - [`compose_job`]: a track composing on a worker for a deck, loaded through `perform` by the
//!   host's frame (`prepareAndLoad`); one at a time a deck, a superseded track given back to the bank
//! - [`offload`]: where the engine's own background composes start (`bg_*`), and, feature
//!   `transfer`, offloading them to another engine instance through the host (djweb's Worker)
//! - [`automix`]: auto-mix on the decks: the countdown, the incoming track, the mix-out, the
//!   start and the crossfader's steps (`AutoMixEngine.swift`)
//!
//! `crate::host::audio` (feature `audio`) runs the [`Renderer`] in a cpal callback; every test
//! drives it by hand through [`Renderer::render_block`].

pub mod arrange_start;
pub mod audition;
pub mod aux;
pub mod post_bus;
pub mod automix;
pub mod bar_next;
pub mod copy;
pub mod beat;
pub mod capture;
pub mod clips;
pub mod control_json;
pub mod compose_job;
pub mod core;
pub mod cues;
pub mod engine;
pub mod fxdeck;
pub mod host;
pub mod input;
pub mod input_capture;
pub mod keys;
pub mod jog_target;
pub mod journal;
pub mod launch;
pub mod loop_actions;
pub mod loop_notes;
pub mod loop_pads;
pub mod loop_source;
pub mod loops;
pub mod minecraft_events;
pub mod mixer;
pub mod moves;
pub mod net;
pub mod live_moves;
pub mod live_ops;
pub mod offline;
pub mod offload;
pub mod output;
pub mod perform;
pub mod relocate;
pub mod retune;
pub mod rig;
pub mod sample;
pub mod samplers;
/// Whole-session snapshots (`DjSnapshot` v1): capture, check, restore.
pub mod snapshot;
pub mod strip_recall;
pub mod surface;
pub mod synth_presets;
pub mod teardown;
pub mod trace;
pub mod track;
#[cfg(feature = "transfer")]
pub mod transfer;
pub mod tricks;
pub mod views;
pub mod freeze;
pub mod vocoder;
pub mod vox;
pub mod wander;

pub use self::compose_job::{ComposeJob, ComposeJobs, Landed, Landing};
pub use self::core::{Command, DeckCore, DeckMirror};
pub use self::engine::{
    crossfade_gain, engine, trim_for_gain, Engine, EngineConfig, MainDeck, Msg,
    Prepared, Renderer, SectionInfo, Stems, MAX_BLOCK, ROLE_ORDER,
};
pub use self::fxdeck::LoopChannel;
pub use self::host::host_engine;
pub use self::journal::StagedRestore;
pub use self::offline::OfflineConductor;
pub use self::perform::{Applied, JournalSink, Outcome, PerformCtx, SampleResolver};
pub use self::rig::Rig;
pub use self::sample::SampleDeck;
pub use self::track::{MoveRequest, PreparedTrack, Preparer, Task, TrackSource, TrackState};
pub use self::wander::{DeckWalk, InstalledSounds};
