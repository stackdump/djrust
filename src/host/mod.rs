//! The live half of the engine: the decks, the mixer and its render thread, net and sample
//! decks, the loop channel, the track pipeline, Wander and auto-mix ([`deck`]), plus the pure
//! host helpers they use (stores, the journal on disk, replay, sample loading). Device I/O is
//! behind features: `audio` runs a [`deck::Renderer`] in a cpal callback ([`audio`]), `sf2` and
//! `decode` add SoundFont voices and audio-file decoding. With no features every call that needs
//! one reports which.

// `deny`, not `forbid`: the exceptions are in `platform` (its `macos.rs` and `linux.rs`), the OS
// scheduling and timer calls (mach on macOS, pthread / clock_nanosleep on Linux) that a
// real-time thread needs to wake on time.
#![deny(unsafe_code)]

#[cfg(feature = "audio")]
pub mod audio;
pub mod beats;
pub mod browse;
pub mod caf;
pub mod controllers;
pub mod deck;
pub mod device_watch;
pub mod decode;
pub mod input_record;
pub mod journal;
pub mod minecraft;
pub mod platform;
pub mod replay;
pub mod sampler;
pub mod stores;
pub mod take_library;

use std::fmt;

#[derive(Debug)]
pub enum HostError {
    /// Built without the feature that backs this call.
    Disabled(&'static str),
    /// The platform backend failed.
    Backend(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Disabled(feat) => write!(f, "djhost was built without the `{feat}` feature"),
            HostError::Backend(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HostError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// A MIDI source (what CoreMIDI calls a source; midir an input port).
    In,
    /// A MIDI destination.
    Out,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPort {
    pub name: String,
    pub direction: Direction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
}

/// Output devices on the default host (CoreAudio / ALSA).
pub fn audio_outputs() -> Result<Vec<AudioDevice>, HostError> {
    #[cfg(feature = "audio")]
    {
        fn name(d: &cpal::Device) -> String {
            use cpal::traits::DeviceTrait;
            d.description()
                .map(|x| x.name().to_string())
                .unwrap_or_else(|_| "(unnamed)".into())
        }
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        let default = host.default_output_device().map(|d| name(&d));
        let devices = host
            .output_devices()
            .map_err(|e| HostError::Backend(e.to_string()))?;
        Ok(devices
            .map(|d| {
                let n = name(&d);
                AudioDevice {
                    is_default: Some(&n) == default.as_ref(),
                    name: n,
                }
            })
            .collect())
    }
    #[cfg(not(feature = "audio"))]
    Err(HostError::Disabled("audio"))
}

