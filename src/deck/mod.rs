//! Deck-level pure logic: the beat grid and tempo rules, the sample turntable's position
//! model, waveform peaks, the section timeline and timeline dots, ring snapshots, loop launch
//! quantisation and stinger / Minecraft event mapping. Everything here is a function of its
//! inputs: the live `Deck` / `DJEngine` classes that wire them to audio stay in the host.

pub mod beat;
pub mod beat_grid;
pub mod clips;
pub mod clock;

pub use beat::{tempo_match, TempoMatch, PITCH_RANGE};
pub mod demo_loop;
pub mod kits;
pub mod level_match;
pub mod library;
pub mod loop_route;
pub mod loop_sound;
pub mod loops;
pub mod minecraft;
pub mod perform;
pub mod provenance;
pub mod row;
pub mod slots;
pub mod snapshot;
pub mod stinger;
pub mod swap;
pub mod timeline;
pub mod turntable;
pub mod voices;
pub mod waveform;
