//! The wave engine: beats' `wave-engine` (net.js, synth.js, runner.js) plus the pure,
//! deterministic part of dj's extensions to it. Port of `Sources/DJCore/Beats/Wave`.
//!
//! - [`net`]: the Petri-net executor (`WaveGraph`), exact against beats' JS tick for tick
//! - [`synth`]: pulses, voices, wavetables, noise seeds, biquads
//! - [`runner`]: `WaveRunner`, the sample-by-sample renderer into caller buffers
//! - [`macros`], [`history`]: live-performance controls and the reversible clock
//! - [`instruments`]: the per-instrument voice table beyond beats' wave engine
//! - [`fire`]: the event and tick-anchor records; [`event_ring`]: the overwrite rings the audio
//!   thread writes them into and the readers' cursors
//! - [`macro_mirror`], [`marking_mirror`]: the seqlock-published records the audio thread
//!   hands to the display (which macros hold which nets; every net's tokens and mutes)
//! - [`part_levels`]: a part's level on its own synth voice, what a sampler is matched to
//!
//! Parity is held by `tests/fixtures/js/wave.json` (written by beats' own JS).

pub mod event_ring;
pub mod fire;
pub mod for_palette;
pub mod groove;
pub mod history;
pub mod instruments;
pub mod macro_mirror;
pub mod macros;
pub mod marking_mirror;
pub mod mip;
pub mod net;
pub mod part_levels;
pub mod rests;
pub mod runner;
pub mod synth;
pub mod voice_tweak;

pub use event_ring::{read_events, EventCursor, LatestRing};
pub use fire::{FireEvent, FireFlags, TickAnchor, TickAnchorFlags};
pub use groove::{Drift, GrooveTemplate, HitRecord};
pub use instruments::InstrumentTable;
pub use macro_mirror::{roles_held, roles_held_in, MacroMirror, MacroRead, MacroSnapshot};
pub use macros::{MacroTargets, NetMacro};
pub use marking_mirror::{
    next_track_serial, MarkingBook, MarkingHeader, MarkingLayout, MarkingMirror, MarkingSnapshot,
    NetArcGeometry, NetInfo,
};
pub use net::{
    deterministic_rand, p_invariants, str_hash, CompiledNet, ControlAction, RoleTables, WaveGraph,
};
pub use part_levels::{own_part_level_db, own_synth_part_db, SamplerPartLevels};
pub use rests::{MixOutTail, RestSchedule};
pub use runner::{
    audition, ChannelVoice, Lane, Master, PartLevelFn, RoleFilter, SampleKey, SamplerHost,
    SamplerSlots, StingerPolicy, VoiceShaping, WaveMod, WaveOptions, WaveProject, WaveRunner,
};
pub use synth::{
    biquad, build_table, drum_kind, drum_voice, is_drum_channel, noise_seed, pulse, tonal_voice,
    Pulse, VoiceSpec, PPQ, WAVE_TABLE_SIZE,
};
pub use voice_tweak::{VoiceLaneParams, VoiceSetting, VoiceTweak};
