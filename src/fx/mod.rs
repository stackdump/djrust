//! The master effects chain (`Beats/Wave/MasterFx/*`, `Beats/Wave/TurntableBrake.swift` and the
//! pure part of `Audio/FxUnit.swift`).
//!
//! - [`tone_param`]: Tone.js's `Param` automation timeline
//! - [`periodic_wave`]: Chrome's band-limited `PeriodicWave` / oscillator (the pitch shifter's LFOs)
//! - [`master_macro`]: the catalog of master-chain macros and the FX sliders
//! - [`aux_bus`], [`aux_macro`]: the aux send/return bus (reverb + ping-pong echo) and the eleven
//!   Pan / Shape / aux-FX macros of beats' catalog that need it (docs/HUB.md M4)
//! - [`master_chain`]: the DSP (HP, phaser, LP, crusher, distortion, pitch shift) and the setters
//! - [`automation`]: beats' macro animation on the transport clock
//! - [`brake`]: dj's varispeed vinyl brake
//! - [`unit`]: the FX unit (knobs on a target's chain)
//! - [`strip`]: a deck's channel EQ and one-knob DJ filter (`Deck.filterNode`)
//! - [`strip_reverb`]: a channel's reverb (`Audio/DeckReverb.swift`): Swift's knob mapping, an FDN
//!   room standing in for Apple's AUReverb2
//! - [`limiter`], [`true_peak`]: the deck's last stage, a lookahead true-peak limiter
//!   (`Audio/PeakLimiter.swift`, `Musicality/TruePeak.swift`)
//! - [`loop_master`]: the master chain and macros on the loop channel (`Audio/LoopMasterBus.swift`)
//! - [`time_stretch`]: WSOLA, the offline stretch behind a loop's key lock
//!   (`Audio/TimeStretch.swift`)
//! - [`post`]: the post-processing ("master") chain for renders and the optional live output stage
//!   (docs/POST.md)
//! - [`loop_fx`]: a loop clip's insert chain (drive, shaper, tone, auto-level) and the loop
//!   channel's safety limiter (`Audio/LoopFx.swift`)

pub mod automation;
pub mod aux_bus;
pub mod aux_macro;
pub mod brake;
pub mod limiter;
pub mod loop_fx;
pub mod loop_master;
pub mod master_chain;
pub mod master_macro;
pub mod periodic_wave;
pub mod post;
pub mod strip;
pub mod strip_reverb;
pub mod time_stretch;
pub mod tone_param;
pub mod tricks;
pub mod true_peak;
pub mod unit;
pub mod eurosynth;
pub mod granular;
pub mod vocoder;

pub use automation::{FxAutomation, FxAutomationEvent};
pub use aux_bus::{AuxBus, AuxFxKey, AuxSettings};
pub use aux_macro::AuxMacro;
pub use limiter::PeakLimiter;
pub use loop_fx::{LoopFxLimiter, LoopFxParams, LoopFxVoice};
pub use loop_master::{LoopMasterBus, LoopMasterCommand};
pub use master_chain::{MasterChain, MasterParamId, MasterParamOp, MasterParams, Stage};
pub use master_macro::{MasterFxKey, MasterMacro};
pub use strip::{StripEq, StripKnob, StripSettings};
pub use strip_reverb::{ReverbKnob, StripReverb};
pub use tone_param::ToneParam;
pub use vocoder::{CarrierSynth, FormantFilter, Vocoder, VocoderParam, VocoderSettings};
