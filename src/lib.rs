//! djrust: the bitwrap audio engine.
//!
//! The deterministic beats generator, the wave engine that plays its tracks, the DJ decks and
//! mixer around it, and the share codec and CIDs that name a track, in one crate. The pure engine
//! (everything at the crate root) has no I/O, no clock and no threads, builds `no_std` + `alloc`
//! with `--no-default-features`, and renders the same samples on every target. The live decks
//! ([`host`]) and each app's platform adapter sit behind cargo features, so an app pulls only
//! what it needs (see README.md):
//!
//! - default (`std`, `host`): the engine and the live decks ([`host::deck`])
//! - `wasm`: the wasm-bindgen facade the browser runs in an AudioWorklet ([`wasm`]); `network`
//!   adds the code that publishes or fetches snapshots by CID
//! - `audio`, `sf2`, `decode` (`native` for all three): sound devices (cpal), SoundFont voices
//!   (rustysynth) and audio files onto a sample deck (symphonia)
//! - `android`: `audio` plus the JNI handshake cpal's AAudio backend needs ([`android`])
//! - `ffi`: a small C ABI for Swift and other native callers ([`ffi`])
//!
//! Engine modules:
//!
//! - [`json`], [`rng`], [`cid`], [`codec`]: JSON with JS semantics (key order, number printing,
//!   canonical forms), mulberry32 and the variety LCG, share and model CIDs, base64url and the
//!   share gzip framing
//! - [`share`], [`gen`], [`pflow`], [`spec`]: share envelopes and links, the beats generator
//!   (`compose(genre, overrides)`), the project model and `genre:seed:structure` track specs
//! - [`wave`], [`fx`], [`musicality`]: the wave engine (the runner and its dj extensions), the
//!   master effects chain, macros, the vinyl brake and the FX unit, and the musicality passes
//!   and measurement
//! - [`sound`], [`voices`], [`layers`], [`loudness`]: sound palettes and stingers, the voice
//!   catalog and sampler rules, loop layers, offline loudness
//! - [`deck`], [`wander`], [`automix`]: deck-level pure logic (beat clock, loops, swap, perform),
//!   Wander and auto-mix
//! - [`control`], [`gear`], [`session`], [`journal`], [`roll`], [`setplan`], [`wav`]: controller
//!   decode and MIDI gear, sessions, the journal, piano rolls, sets and the one WAV encoder

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod automix;
pub mod cid;
pub mod clock;
pub mod codec;
pub mod control;
pub mod deck;
pub mod fx;
pub mod gear;
pub mod gen;
pub mod history;
pub mod input;
pub mod journal;
pub mod json;
pub mod layers;
pub mod looplib;
pub mod loudness;
pub mod mixer;
pub mod musicality;
pub mod pflow;
pub mod rng;
pub mod roll;
pub mod session;
pub mod setplan;
pub mod share;
pub mod sound;
pub mod spec;
#[cfg(feature = "transfer")]
pub mod transfer;
pub mod stretch;
pub mod voices;
pub mod wander;
pub mod wav;
pub mod wave;

#[cfg(feature = "host")]
pub mod host;

#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(all(feature = "android", target_os = "android"))]
pub mod android;

#[cfg(feature = "ffi")]
pub mod ffi;

pub use gen::compose;
pub use json::{JsonObject, JsonValue};
pub use pflow::Project;
pub use spec::TrackSpec;
