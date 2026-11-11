//! The beats generator (port of beats-btw-dj `Beats/Generator/`, itself a byte-for-byte
//! port of beats' public/lib/generator/*.js).

pub mod arrange;
pub mod composer;
pub mod core;
pub mod counter_melody;
pub mod euclidean;
pub mod markov;
pub mod structure;
pub mod tables;
pub mod techniques;
pub mod theme;
pub mod variety;

pub use composer::{compose, compose_with, fallback_instrument_set, generate_track_name};
pub use tables::BeatsTables;
