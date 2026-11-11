//! Port of `Beats/Wave/WaveRunner.swift` (beats' `wave-engine/runner.js`): the net
//! executor and the synthesis, sample by sample. Per sample: advance the tick clock; at
//! a tick boundary fire the net and add each audible fire's weight to its lane's gate;
//! every active lane then outputs gate × carrier and the lanes are summed through the
//! master curve.
//!
//! The gate is the IIR realisation of synth.js::envelope: one state per exponential,
//! multiplied down each sample and bumped by α·w at each onset.
//!
//! This is the parity port: arithmetic and operation order follow the JS exactly, so the
//! audio matches within float32 rounding. What is ported beyond it is the part of dj's
//! runner that is pure and deterministic and has no Apple dependency: the instrument
//! table's voices (Karplus-Strong strings, vibrato, post lowpass, distortion, pitch
//! envelopes), swaps at bar boundaries, net macros with tempo effects, `align`, the fire
//! and anchor streams, scratching the net clock, the pooled live-keys voice, and the
//! musicality fixes the runner plays (`WaveOptions::musicality`): the room, its shimmer and
//! the echo with the FX unit's gains on them, kick ducking, the section energy arc and the
//! buildup's sweep, subtractive and unison voices, the sub-octave, the parts' balance trims,
//! the stereo field, the riser, the kick tuned to the key, the voices of the fill nets, the
//! section crash and the open hats, the closed hat's choke, the lowpass on the noise drums
//! (`.hfTilt`), the brushes on jazz's snare and the section plan's moved notes
//! (`.sectionHarmony`, `.harmonicRhythm`). Sampled instruments play on the host's sampler
//! (`SampleBinder`, or the deck's whole bank, `SamplerSlots`: a track's picks and palette option,
//! its drum kit, jazz's Brush kit); the master chain is not in this file. `prepare` measures what
//! no caller gave it: the trims (`.balance`), the rest schedule (`.dropGap` and 6.2's gestures)
//! and the track's gain (`.loudness`). Each prepared project owns its marking mirror
//! ([`WaveProject::marking`], published after each block by [`WaveRunner::publish_marking`]), and
//! a deck's fires and tick anchors go to overwrite rings the runner writes without allocating
//! ([`WaveRunner::fire_ring`], [`WaveRunner::anchor_ring`]); the `Vec`s of `record_events` are
//! for offline renders.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use super::event_ring::LatestRing;
use super::fire::{FireEvent, FireFlags, TickAnchor, TickAnchorFlags};
use super::groove::{groove_hash, Drift, FeelTable, GrooveTemplate, HitRecord, Voice, STEPS};
use super::instruments::InstrumentTable;
use super::macros::{MacroTargets, NetMacro};
use super::marking_mirror::MarkingMirror;
#[cfg(feature = "transfer")]
use super::marking_mirror::MarkingLayout;
use super::mip::MipTables;
use super::net::{CompiledNet, WaveGraph};
use super::part_levels::own_part_level_db;
use super::rests::{Class, Cursor, Entry, MixOutTail, RestSchedule};
use super::synth::{
    biquad, build_table, drum_kind, drum_voice, is_drum_channel, noise_seed, tonal_voice,
    tone_distortion, VoiceSpec, BUTTERWORTH_Q, PPQ, WAVE_TABLE_SIZE,
};
use super::voice_tweak::{VoiceLaneParams, VoiceSetting};
use crate::json::{js_round, JsonValue};
use crate::musicality::dj::{
    sampled_width, stereo_placement, SampledWidth, CRASH_NOTE, OPEN_HAT_NOTE,
};
use crate::musicality::echo::{EchoSpec, PingPong};
use crate::musicality::energy_curve::{BuildupSweep, EnergyCurve, SideHighpass, SweepFilter};
use crate::musicality::fixes::Fixes;
use crate::musicality::loudness::Loudness;
use crate::musicality::mix_balance::{MixBalance, SampledBalance};
use crate::musicality::note_log::group_name;
use crate::musicality::rest_gestures::build_rests;
use crate::musicality::riser::{riser_windows, RiserVoice};
use crate::musicality::section_harmony::SectionHarmony;
use crate::musicality::space::{FdnReverb, Shimmer, SpaceSpec};
use crate::musicality::synth_voice::{
    Brush, FilterEnvelope, HfTilt, KickTuning, SubOctave, Unison, VoiceDesign,
};
use crate::pflow::{OrderedMap, Project};
use crate::sound::synth_name;
use crate::voices::drumkits::{gm_note, hold_ms};
use crate::voices::sampler::velocity_scale;
use crate::voices::{SamplerSource, VoiceCatalog};

const SILENT: f64 = 1e-6;
/// Two uncorrelated oscillators as loud as one: Swift's own digits (`0.7071`), not 1/sqrt 2.
#[allow(clippy::approx_constant)]
const EQUAL_POWER: f64 = 0.7071;
const TAU: f64 = 2.0 * core::f64::consts::PI;

// lane state slots
const ENV: usize = 0;
const PREV: usize = 1;
const LEVEL: usize = 2;
const PHASE: usize = 3;
const SWEEP: usize = 4;
const SWEEP_MUL: usize = 5;
const F0: usize = 6;
const F1: usize = 7;
const TONE_INC: usize = 8;
const TONE_MIX: usize = 9;
const Z1: usize = 10;
const Z2: usize = 11;
const AM_DEPTH: usize = 12;
const FM_DEPTH: usize = 13;
const INV_SR: usize = 14;
const ATT_MUL: usize = 15;
const STATE_SIZE: usize = 16;

// runner clock slots
const TICK_POS: usize = 0;
const SPT: usize = 1;
const TEMPO: usize = 2;
const SAMPLES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
enum Carrier {
    Dc,
    Sweep,
    Noise,
    NoiseTone,
}

/// `WaveMod` (`opts.mods` entry): an outer ring's gate scales (am) or bends (fm) an inner
/// one.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct WaveMod {
    pub target: String,
    pub source: String,
    /// "am" | "fm"
    pub kind: String,
    pub depth: f64,
    pub silent_source: bool,
}

/// `WaveOptions.master`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum Master {
    /// beats' tanh (default).
    Soft,
    /// Tests: the plain sum.
    Linear,
    /// The app: linear with the master and track gain, a limiter follows in the host.
    Limit,
}

/// The hook sampled instruments (an SF2 synth in `djhost`, `Sampler.swift`'s AUSampler in
/// the Swift app) plug in at. Called at prepare for every tonal lane with the track's
/// instrument name and the lane's part (`Track.group`, else the net's `riffGroup`): a
/// `Some(slot)` makes the lane play that sampler slot, not its synth voice. The lane then
/// makes no sound of its own; each note it would have struck is reported as a
/// [`SampleNote`] in `WaveRunner::sample_notes`, for the host to render in the same block.
pub type SampleBinder = fn(instrument: &str, part: &str) -> Option<usize>;

/// What a sampler slot measured on a part's probe when it loaded (`SamplerBank.slotPartLevelDB`):
/// the sampler's level against the group's plain synth voice, dB, or None where nothing was
/// measured (an empty slot, a bank that skips the probes). A plain `fn`, like [`SampleBinder`].
pub type PartLevelFn = fn(slot: usize, group: &str) -> Option<f64>;

/// The deck's sampler bank as `buildLanes` uses it (Swift's `SamplerBank` with the catalogs it
/// ranks sources from), set as [`WaveOptions::bank`]. Everything on it is asked while a project
/// is prepared, off the audio thread; the host numbers the slots, loads what is in them and
/// hands the loaded samplers to its audio thread. It replaces [`SampleBinder`], which stays for
/// the hosts and tests that bind a lane by instrument name alone.
pub trait SamplerSlots: Send + Sync {
    /// `VoiceCatalog.shared.sources(instrument:role:seed:picked:)`: the instrument's sounds, in
    /// the order to try them (the seeded pick first; the user's pick, `picked`, by name).
    fn instrument_sources(
        &self,
        instrument: &str,
        role: &str,
        seed: f64,
        picked: bool,
    ) -> Option<Vec<SamplerSource>>;
    /// `SoundPalette.sources(option:role:seed:)`: what a palette option plays for the role.
    fn option_sources(&self, option: &str, role: &str, seed: f64) -> Option<Vec<SamplerSource>>;
    /// `SamplerBank.slot(for:)`: the slot of the first of `sources` that is loaded already or
    /// loads and passes its checks. It takes one reference on the slot it returns, which the
    /// lane that plays the slot gives back through [`release`](Self::release) when its project
    /// is freed (a full bank reuses only slots nothing references).
    fn slot_for(&self, sources: &[SamplerSource]) -> Option<usize>;
    /// `SamplerBank.release(_:)`: give back one reference [`slot_for`](Self::slot_for) took.
    /// A [`WaveProject`] calls it for each of its sampled lanes when it drops (Swift's
    /// `WaveProject.deinit`), which happens off the audio thread, so a slot no project plays has
    /// no references. The default does nothing (a bank with no eviction needs no counts).
    fn release(&self, _slot: usize) {}
    /// `DrumKits.source(for:)`: the kit an id names, wherever its bank is installed.
    fn kit_source(&self, id: &str) -> Option<SamplerSource>;
    /// `DrumKits.brushKit(for:)`: the Brush kit of a kit's bank, if it has one.
    fn brush_kit(&self, kit: &SamplerSource) -> Option<SamplerSource>;
}

/// The host's sampled instruments, rendered inside [`WaveRunner::process_with`] as Swift's
/// `SamplerBank` is inside `WaveRunner.process` (`flushSamplers`). The runner reports the MIDI its
/// sampled lanes and live keys asked for ([`SampleNote`], [`SampleKey`]) and the host plays it
/// into the block at the point of the curve Swift does: after it, or (`samplersInCurve`, with
/// `.loudness`) into a scratch pair that joins the voices before the master curve.
pub trait SamplerHost {
    /// The top of a block, before the runner renders it: which sets of slots the project that
    /// starts the block asks for (the gates at the top of `WaveRunner.process`). Real-time safe.
    fn begin_block(&mut self, runner: &WaveRunner, frames: usize);
    /// The end of a block (`flushSamplers`): play what the runner reported for the block it just
    /// rendered, adding into `left` / `right` (the first `frames` of them), and hand the sends
    /// back to the runner ([`WaveRunner::queue_sampler_send`]). `start` is the `start_frame` the
    /// runner got. Real-time safe.
    fn flush(
        &mut self,
        runner: &mut WaveRunner,
        start: i64,
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    );
}

/// One note for a sampler slot, at the sample it starts on (`Sampler`'s MIDI queue).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleNote {
    pub slot: usize,
    pub note: i64,
    /// MIDI velocity 0…127 as played (after the groove).
    pub velocity: i64,
    /// How long the note is held, ms (the tonal voice's own reading of the binding).
    pub dur_ms: f64,
    /// The frame it starts on, counted like `process_at`'s `start_frame`.
    pub frame: i64,
    /// The lane that played it ([`Lane::uid`]): Swift keeps each lane's pending note-offs on the
    /// lane, so a retrigger ends the lane's own held note and "use synth voice" ends the lane's
    /// own notes; the host keeps them in one list and tells the lanes' notes apart by this.
    pub lane: u64,
}

/// A request to the host: end the notes lane `lane` ([`Lane::uid`]) still holds on the sampler
/// (the note-offs `applyVoice` queues for a lane that goes to its synth voice, then
/// `offCount = 0`). The host answers by sending the note-off of each of that lane's pending
/// notes now and forgetting them; the other lanes' notes and the live keys are left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneEnd {
    pub lane: u64,
    /// The frame it goes out on, counted like [`SampleNote::frame`].
    pub frame: i64,
}

/// A raw MIDI message for a sampler slot, as `WaveRunner.queue(_:_:_:_:)` sends one to the bank:
/// the live keys on a sampled voice (`0x90` on, `0x80` off, `0xB0` 123 all notes off). Net notes
/// are [`SampleNote`]s (the host ends them by their length); only the live keys need this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleKey {
    pub slot: usize,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
    /// The frame it goes out on, counted like [`SampleNote::frame`].
    pub frame: i64,
}

/// What a composed track's Hit 1-4 may be curated from (`WaveOptions.stingers`, Swift's
/// `StingerContent`): the content installed here, everything (a test's) or nothing. Data only:
/// the curation (`StingerCuration`) is applied to the played project by the code that composes
/// the track, with the host's installed sounds, not by the runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub enum StingerPolicy {
    Here,
    Everything,
    Nothing,
}

/// `WaveOptions`
#[derive(Clone)]
pub struct WaveOptions {
    pub master: Master,
    pub master_gain: f64,
    pub voices: BTreeMap<String, VoiceSpec>,
    pub mods: Vec<WaveMod>,
    /// Per-instrument voices (`InstrumentTable`); None = beats' families only (parity).
    pub instruments: Option<&'static InstrumentTable>,
    /// Roles whose notes are dropped at compile time (`djctl export --mute/--solo`).
    pub role_filter: Option<RoleFilter>,
    /// The song's rest schedule (`WaveOptions.trackRests`; `DJMusicality` builds it in the
    /// app): a lane rests while a step's mask shares a class with it.
    pub rests: Option<RestSchedule>,
    /// A mix-out tail: the melodic lanes rest from its `from_tick`, the song's
    /// `stop-transport` is ignored, and the runner stops on its `end_tick`. None = the
    /// song as it ends (parity).
    pub tail: Option<MixOutTail>,
    /// One more rest joined to the song's schedule at prepare (a mix-in's).
    pub extra_rest: Option<Entry>,
    /// `.groove`: the project's swing and humanize, played the way beats' browser plays
    /// them (backend/index.js `swingDelay`, `humanizeNote`), which its wave engine leaves
    /// out. Off = beats' wave engine (parity). On too when `musicality` holds `.groove` (as in
    /// Swift, where this is the only way to ask); the flag is for a test that wants it alone.
    pub groove: bool,
    /// `.feel`: the family's groove template the drums play, in place of the project's swing
    /// and humanize (`GrooveTemplate::for_genre`). None = none; and none too without `.feel` in
    /// `musicality`: the template is read only with the flag (`feel: opts.musicality.contains(
    /// .feel) ? opts.feel : nil`), so a template set by hand plays only under `Fixes::FEEL`.
    pub feel: Option<GrooveTemplate>,
    /// `.drift`: humanize as seeded 1/f noise (`Drift`) keyed by `voicing_seed`, on top of
    /// `.feel` or `.groove`'s swing, in place of beats' white jitter. On too when `musicality`
    /// holds `.drift`, like `groove`.
    pub drift: bool,
    /// The track's seed: keys `.drift`, and (in the app) seeds the sampled-instrument picks.
    pub voicing_seed: f64,
    /// Report every onset played into `WaveRunner::hits` (`onHit`; offline measurement).
    pub record_hits: bool,
    /// Which tonal lanes play a sampler instead of a synth voice ([`SampleBinder`]).
    pub sample_binder: Option<SampleBinder>,
    /// The deck's sampler bank (Swift's `samplers`): tonal lanes bind through it with the track's
    /// `picked` and palette option, and (with the instrument table) the drums of a track that
    /// picked a kit play it. It takes the place of `sample_binder` where both are set.
    pub bank: Option<Arc<dyn SamplerSlots>>,
    /// What each bound slot measured on a part's probe ([`PartLevelFn`]); None = nothing is
    /// measured, so a sampled part takes the palette's average (`Loudness::sampled_part_db`).
    pub part_level: Option<PartLevelFn>,
    /// The sample rate the bank measured at (`SamplerBank.sampleRate`); 0 = the runner's.
    pub bank_rate: f64,
    /// The fixes the deck plays a track with (`WaveOptions.musicality`): the pass's, which
    /// `musicality::apply` runs on a composed track (`PreparedTrack.played`), and the runner's own
    /// (`Fixes::RUNNER_ONLY`: the loudness, groove, energy, duck, stereo, the room and the echo,
    /// the voice design, the balance trims, the riser, the drum voices, ...). Empty = beats' mix
    /// (parity); [`extended`](Self::extended) holds them all.
    pub musicality: Fixes,
    /// The room (`.space`), set from the track's genre (`SpaceSpec::for_genre`); None with
    /// `.space` on plays `SpaceSpec::room()`.
    pub space: Option<SpaceSpec>,
    /// The echo (`.echo`), set from the track's genre (`EchoSpec::for_genre`); None = none.
    pub echo: Option<EchoSpec>,
    /// The parts' trims (`.balance`, dB by `MixBalance` part) to prepare with; None = none.
    pub track_balance: Option<BTreeMap<String, f64>>,
    /// The loudness probe's stand-in for sampled parts (`Loudness.measure`): net id → a level
    /// on its tonal lane, so its synth voice sounds about as loud as the sampler the deck
    /// plays there. Empty elsewhere.
    pub probe_levels: BTreeMap<String, f64>,
    /// Kick-keyed ducking of the bass and pad (`.duck`): how far they dip on each kick (0 =
    /// none), set from the track's genre (`dj::duck_depth`).
    pub duck_depth: f64,
    /// The energy curve to prepare with instead of measuring one (`trackEnergy`): another start
    /// of the same song reuses the playing project's.
    pub track_energy: Option<EnergyCurve>,
    /// Per-genre sound design under `.subtractive` / `.unison` / `.stereo` (`VoiceDesign::
    /// for_genre`): which parts are plucked, how wide the pads are.
    pub voice_design: VoiceDesign,
    /// A sine an octave under the synth basses (`.subOctave`). Set by `for_genre` (dnb, dubstep,
    /// trap); false = beats' bass (parity, every other genre).
    pub sub_octave: bool,
    /// The track's genre, set by `for_genre`; None (raw nets, parity) = unknown, and what a genre
    /// picks stays off (the family the `.balance` trims are measured against).
    pub genre: Option<String>,
    /// What a composed track's Hit 1-4 may be curated from (`StingerCuration`, applied to the
    /// played project when the track is made); None = the stinger nets as composed (parity).
    pub stingers: Option<StingerPolicy>,
    /// `vinyl-brake` as a turntable brake (the deck's `TurntableBrake`): the whole deck slows to a
    /// stop and drops back in time. false = beats' compound (an octave-down hold: its tape-stop
    /// step does nothing there), what the parity fixtures run. A deliberate dj divergence
    /// (GO-DIVERGENCE.md 5); on in `WaveOptions::extended`. The deck reads it (`DeckCore`), and
    /// the project prepared with it carries it ([`WaveProject::turntable_brake`]).
    pub turntable_brake: bool,
    /// The gain to prepare with instead of measuring one (`.loudness`): a section jump or a
    /// relocation of the song already playing reuses that project's.
    pub track_gain: Option<f64>,
    /// Brushes on the snare (`Brush`, `.backbeat`'s jazz): its synth lanes play the brush voice.
    /// Set by `for_genre`; false = beats' snare (parity, and every other genre).
    pub brushes: bool,
    /// The lowpass on the bright drum voices (`HfTilt`, `.hfTilt`), set by `for_genre` from the
    /// family's cutoff; None = beats' hats (parity, and with the fix off).
    pub hf_tilt: Option<HfTilt>,
}

impl Default for WaveOptions {
    fn default() -> Self {
        WaveOptions {
            master: Master::Soft,
            master_gain: 0.8,
            voices: BTreeMap::new(),
            mods: Vec::new(),
            instruments: None,
            role_filter: None,
            rests: None,
            tail: None,
            extra_rest: None,
            groove: false,
            feel: None,
            drift: false,
            voicing_seed: 0.0,
            record_hits: false,
            sample_binder: None,
            bank: None,
            part_level: None,
            bank_rate: 0.0,
            musicality: Fixes::EMPTY,
            space: None,
            echo: None,
            track_balance: None,
            probe_levels: BTreeMap::new(),
            duck_depth: 0.0,
            track_energy: None,
            voice_design: VoiceDesign::default(),
            sub_octave: false,
            genre: None,
            stingers: None,
            turntable_brake: false,
            track_gain: None,
            brushes: false,
            hf_tilt: None,
        }
    }
}

impl WaveOptions {
    /// `WaveOptions.extended`: what the app plays - beats' wave engine plus the instrument table,
    /// the app's limiting master, every musicality fix (`.all`: the runner's own and the pass's,
    /// which `musicality::apply` runs on a composed track), the stingers curated from what is
    /// installed here and the turntable brake. The sampler bank is [`WaveOptions::bank`], the
    /// track's seed `voicing_seed`, the role filter `role_filter`: set them on the result
    /// ([`extended_with`](Self::extended_with) is `extended(samplers:seed:roleFilter:)`).
    ///
    /// (Swift reads `.groove` and `.drift` off `musicality`; so does the runner here, through the
    /// two flags beside it, which only a test that wants the swing without the rest sets.)
    pub fn extended() -> WaveOptions {
        WaveOptions {
            instruments: Some(InstrumentTable::shared()),
            musicality: Fixes::ALL,
            stingers: Some(StingerPolicy::Here),
            turntable_brake: true,
            master: Master::Limit,
            ..WaveOptions::default()
        }
    }

    /// `WaveOptions.extended(samplers:seed:roleFilter:)`: a deck's options for one track, its
    /// sampler bank (the slots the host numbers, [`SamplerSlots`]) playing the sampled
    /// instruments, voiced by `seed` (the track's), with the roles `role_filter` leaves out
    /// (`Deck.waveOptions`).
    pub fn extended_with(
        bank: Option<Arc<dyn SamplerSlots>>,
        seed: f64,
        role_filter: Option<RoleFilter>,
    ) -> WaveOptions {
        WaveOptions {
            bank,
            voicing_seed: seed,
            role_filter,
            ..WaveOptions::extended()
        }
    }

    /// What a deck plays a composed track of `genre` with, as Swift's `PreparedTrack` makes them:
    /// `WaveOptions.extended` seeded with the track's `seed`, then `for_genre`.
    pub fn for_track(genre: &str, seed: f64) -> WaveOptions {
        WaveOptions::extended_with(None, seed, None).for_genre(genre)
    }

    /// `WaveOptions.forGenre`: these options with the settings a track of `genre` plays with,
    /// for the fixes they hold: the duck depth, the room (with the pad shimmer, `.shimmer`),
    /// the echo, the groove template, the voice design, the brushes, the sub-octave and the HF
    /// tilt; and the genre itself (the rests it gates).
    pub fn for_genre(&self, genre: &str) -> WaveOptions {
        let mut o = self.clone();
        let fixes = self.musicality;
        o.genre = Some(genre.into());
        if fixes.contains(Fixes::FEEL) {
            o.feel = GrooveTemplate::for_genre(genre).cloned();
        }
        if !(fixes & (Fixes::SUBTRACTIVE | Fixes::UNISON | Fixes::STEREO)).is_empty() {
            o.voice_design = VoiceDesign::for_genre(genre);
        }
        if fixes.contains(Fixes::DUCK) {
            o.duck_depth = crate::musicality::dj::duck_depth(genre);
        }
        if fixes.contains(Fixes::SPACE) {
            o.space = Some(SpaceSpec::for_genre(genre, fixes.contains(Fixes::SHIMMER)));
        }
        if fixes.contains(Fixes::ECHO) {
            o.echo = EchoSpec::for_genre(genre);
        }
        o.brushes = fixes.contains(Fixes::BACKBEAT) && Brush::for_genre(genre);
        o.sub_octave = fixes.contains(Fixes::SUB_OCTAVE) && SubOctave::for_genre(genre);
        if fixes.contains(Fixes::HF_TILT) {
            o.hf_tilt = HfTilt::for_genre(genre);
        }
        o
    }
}

/// Which roles a render keeps. A net is named by its id, its riff group (`bass` for
/// `bass-3`) and its mixer group (`drums`, `bass`, `melody`, `arp`, `harmony`, `stinger`);
/// drum fills (`fill-N`) count as `drums`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct RoleFilter {
    pub mute: Vec<String>,
    /// When set, only these play.
    pub solo: Option<Vec<String>>,
}

impl RoleFilter {
    pub fn is_empty(&self) -> bool {
        self.mute.is_empty() && self.solo.is_none()
    }

    fn silences(&self, id: &str, nb: &crate::pflow::NetBundle) -> bool {
        let mut names: Vec<&str> = vec![id];
        if !nb.riff_group.is_empty() {
            names.push(&nb.riff_group);
        }
        if let Some(g) = nb.track.group.as_deref().filter(|g| !g.is_empty()) {
            names.push(g);
        }
        if id.starts_with("fill-") {
            names.push("drums");
        }
        if names.iter().any(|n| self.mute.iter().any(|m| m == n)) {
            return true;
        }
        if let Some(solo) = &self.solo {
            return !names.iter().any(|n| solo.iter().any(|s| s == n));
        }
        false
    }

    /// `project` with the silenced nets' note bindings removed.
    pub fn apply(&self, project: &Project) -> Project {
        if self.is_empty() {
            return project.clone();
        }
        let mut p = project.clone();
        let ids: Vec<String> = p.nets.keys().to_vec();
        for id in ids {
            let silence = {
                let nb = p.nets.get(&id).unwrap();
                nb.role != "control" && !nb.bindings.keys().is_empty() && self.silences(&id, nb)
            };
            if silence {
                p.nets.get_mut(&id).unwrap().bindings = OrderedMap::new();
            }
        }
        p
    }
}

/// One voice of the engine: a gate lane (a drum kind) or a pooled tonal lane.
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Lane {
    pub is_tonal: bool,
    pub voice: VoiceSpec,
    pub active: bool,
    st: [f64; STATE_SIZE],
    num_exp: usize,
    alpha: Vec<f64>,
    mul: Vec<f64>,
    s: Vec<f64>,
    carrier: Carrier,
    rng: i32,
    bq: [f64; 5],
    am: Option<usize>,
    fm: Option<usize>,
    pub net_id: String,
    /// Unique across the projects of a process: the project's generation in the high half, the
    /// lane's index in the low (0: a lane built on its own). Names the lane to the host
    /// ([`SampleNote::lane`], [`LaneEnd`]), as Swift's lane object is its own queue of notes.
    uid: u64,
    // tonal pool
    table: Arc<Vec<f32>>,
    poly: usize,
    inc: Vec<f64>,
    phase: Vec<f64>,
    sd: Vec<f64>,
    sa: Vec<f64>,
    md: Vec<f64>,
    /// Live keys: a held voice does not decay until released.
    hold: Vec<bool>,
    held_note: Vec<i64>,
    // pluck engine: one delay line of `dmax` samples per voice, all in `dl`
    is_pluck: bool,
    dl: Vec<f32>,
    dmax: usize,
    dw: Vec<usize>,
    dlen: Vec<f64>,
    lp: Vec<f64>,
    fb: Vec<f64>,
    excite: Vec<i64>,
    pluck_a: f64,
    vib_phase: f64,
    vib_inc: f64,
    // pitch envelope: each voice's current multiplier, its per-sample step, where it stops
    pm_on: bool,
    pm: Vec<f64>,
    pm_mul: f64,
    // band-limited tables for a swept saw / square (shared with every lane of the wave)
    // (shared tables, not sent with a transferred project: [`WaveProject::installed`] takes
    // this engine's own, which hold the same values)
    #[cfg_attr(feature = "transfer", serde(skip))]
    mip: Arc<[f32]>,
    #[cfg_attr(feature = "transfer", serde(skip))]
    mip_top: Arc<[f64]>,
    mip_base: Vec<usize>,
    mip_on: bool,
    // post chain on the voice sum: lowpass biquad, then distortion
    post: [f64; 5],
    post_on: bool,
    pz1: f64,
    pz2: f64,
    decay_scale: f64,
    max_decay: f64,
    vib_depth: f64,
    distortion: f64,
    pitch_from: f64,
    pitch_to: f64,
    /// What the prepare keyed it by (`kick`, `snare`, `clap`, `hat`, `openhat`, `tonal`, `v`).
    pub kind: String,
    /// The MIDI channel of the net this lane plays.
    pub channel: i64,
    pub instrument: String,
    /// What the lane is for the rest schedule: it drops its hits while a step's mask holds
    /// its class. None (never rests): crashes, stingers, overrides.
    rest_class: Class,
    /// `.sectionHarmony` / `.harmonicRhythm`: the lane's group in `WaveProject::harmony` (-1:
    /// not moved, e.g. drums and the live keys).
    harmony_group: i64,
    /// `.choke`: the open hat (a lane of the same project) that a hit on this closed hat lane
    /// chokes; on the open hat lane, whether it is being choked and how fast it dies then (per
    /// sample).
    choke: Option<usize>,
    choking: bool,
    choke_mul: f64,
    /// `.hfTilt`: a lowpass biquad after a noise lane's own filter (hat, open hat, crash;
    /// `HfTilt`), 5 coefficients used while `tilt_on`, and its state.
    tilt: [f64; 5],
    tilt_on: bool,
    tz1: f64,
    tz2: f64,
    /// A drum lane on a sampled kit (`Track.kit`): the GM key it plays and how long each hit is
    /// held (`DrumKits.gmNote` / `holdMs`), and its hits' velocity scale (`.balance`'s kick trim
    /// as a velocity, `DrumKits.velocityScale`).
    kit_note: i64,
    kit_hold_ms: f64,
    kit_velocity: f64,
    /// Until which sample of the runner's clock a kit hit is held (it chokes the open hat only
    /// while this is in the future: Swift keeps the notes it will end, `offCount`).
    kit_held_until: f64,
    /// The sampler slot this lane plays (-1: its synth voice).
    sample_slot: i32,
    /// The slot this lane was bound to at prepare (-1: none): what `sample_slot` goes back to
    /// after "Use synth voice" is turned off (`sampledSlot`).
    sampled_slot: i32,
    /// `.space`: how much of the lane goes to the reverb send (`SpaceSpec::send`).
    send: f64,
    /// `.shimmer`: how much goes to the pad shimmer (synth pads only, as `send`).
    shimmer_send: f64,
    /// `.echo`: how much goes to the echo (`EchoSpec::send`).
    echo_send: f64,
    /// Bass / pad voice that dips on the kick (`WaveOptions::duck_depth`); the kick that keys it.
    ducks: bool,
    is_kick: bool,
    /// `.stereo`: side gain of the whole lane, and per voice when its voices spread (empty: all
    /// at `pan`). 0 = centre. `voice_spread` is each voice's offset at full width (+-spread);
    /// `voice_pan` what it plays at, narrowed for low notes when the voice is struck.
    pan: f64,
    voice_spread: Vec<f64>,
    voice_pan: Vec<f64>,
    /// A pan macro's hold on the lane (`AuxMacro`: ping-pong, hard left / right, auto-pan,
    /// mono): -1 left ... +1 right, NaN when no macro holds it. It replaces `pan` and the
    /// voices' spread for as long as it lasts (beats sends the channel's MIDI pan, CC 10).
    pan_move: f64,
    /// A shape macro's multiplier on the lane's note decay (tighten, loosen, pulse); 1 = none.
    decay_move: f64,
    /// `.subtractive`: each voice's two-pole lowpass (`fz1`, `fz2`) whose cutoff is `f_base` x
    /// the note's frequency plus `f_amount` x velocity^2 (`f_vel`) x an envelope (`f_env`) that
    /// falls by `f_mul` a sample. `f_amount` 0: no filter. The band-limited tables are `mip`.
    f_amount: f64,
    f_base: f64,
    f_mul: f64,
    f_k: f64,
    f_q: f64,
    /// `.sweep`: each voice's base and 1/Q as struck (the lane's own outside a buildup,
    /// `BuildupSweep` inside one), so a note keeps the colour it started with.
    f_base_v: Vec<f64>,
    f_k_v: Vec<f64>,
    f_env: Vec<f64>,
    f_vel: Vec<f64>,
    fz1: Vec<f64>,
    fz2: Vec<f64>,
    /// `.unison`: each voice is two oscillators, `uni_down` and `uni_up` x its frequency (1 =
    /// off), the second with its own phase (`phase2`); with `.stereo` (`uni_wide`) they sit
    /// either side, `uni_width` apart for the voice's note (centred low, as `voice_pan` is).
    uni_down: f64,
    uni_up: f64,
    phase2: Vec<f64>,
    uni_wide: bool,
    uni_width: Vec<f64>,
    /// ...and their difference is highpassed (`Unison::SIDE_HZ`), so the low mids stay mono.
    uni_hp: [f64; 5],
    /// The pair's width at full spread (`Unison::WIDTH`, x the pad width on a pad).
    uni_max: f64,
    uz1: f64,
    uz2: f64,
    /// `.subOctave`: each voice's sine an octave down (`sub_phase`; empty = none), played where
    /// half the note's rate is at least `sub_min_rate` (`SubOctave::MIN_HZ`, cycles per sample).
    sub_phase: Vec<f64>,
    sub_min_rate: f64,
    /// `.stereo`: the voices spread (`voice_spread`) and nothing on the voice sum is in the way.
    spread_on: bool,
    base_spread: bool,
    // The prepared values `set_voice` restores for a field the tweak leaves alone.
    base_level: f64,
    base_att_mul: f64,
    base_post: [f64; 5],
    base_post_on: bool,
    base_vib_inc: f64,
    base_pm_on: bool,
    base_pm_mul: f64,
}

impl Lane {
    fn new(voice: VoiceSpec, sr: f64, tonal: bool) -> Lane {
        let mut st = [0.0; STATE_SIZE];
        st[LEVEL] = voice.level;
        st[INV_SR] = 1.0 / sr;
        Lane {
            is_tonal: tonal,
            decay_scale: voice.decay_scale,
            max_decay: voice.max_decay,
            vib_depth: voice.vibrato_depth,
            distortion: voice.distortion,
            pitch_from: voice.pitch_from,
            pitch_to: voice.pitch_to,
            voice,
            active: false,
            st,
            num_exp: 0,
            alpha: Vec::new(),
            mul: Vec::new(),
            s: Vec::new(),
            carrier: Carrier::Dc,
            rng: 1,
            bq: [0.0; 5],
            am: None,
            fm: None,
            net_id: String::new(),
            uid: 0,
            table: Arc::new(Vec::new()),
            poly: 0,
            inc: Vec::new(),
            phase: Vec::new(),
            sd: Vec::new(),
            sa: Vec::new(),
            md: Vec::new(),
            hold: Vec::new(),
            held_note: Vec::new(),
            is_pluck: false,
            dl: Vec::new(),
            dmax: 0,
            dw: Vec::new(),
            dlen: Vec::new(),
            lp: Vec::new(),
            fb: Vec::new(),
            excite: Vec::new(),
            pluck_a: 0.0,
            vib_phase: 0.0,
            vib_inc: 0.0,
            pm_on: false,
            pm: Vec::new(),
            pm_mul: 1.0,
            mip: Arc::default(),
            mip_top: Arc::default(),
            mip_base: Vec::new(),
            mip_on: false,
            post: [0.0; 5],
            post_on: false,
            pz1: 0.0,
            pz2: 0.0,
            kind: String::new(),
            channel: -1,
            instrument: String::new(),
            rest_class: Class::NONE,
            harmony_group: -1,
            choke: None,
            choking: false,
            choke_mul: 1.0,
            tilt: [0.0; 5],
            tilt_on: false,
            tz1: 0.0,
            tz2: 0.0,
            kit_note: 36,
            kit_hold_ms: 400.0,
            kit_velocity: 1.0,
            kit_held_until: f64::NEG_INFINITY,
            sample_slot: -1,
            sampled_slot: -1,
            send: 0.0,
            shimmer_send: 0.0,
            echo_send: 0.0,
            ducks: false,
            is_kick: false,
            pan: 0.0,
            voice_spread: Vec::new(),
            voice_pan: Vec::new(),
            pan_move: f64::NAN,
            decay_move: 1.0,
            f_amount: 0.0,
            f_base: 0.0,
            f_mul: 0.0,
            f_k: 1.0,
            f_q: 1.0,
            f_base_v: Vec::new(),
            f_k_v: Vec::new(),
            f_env: Vec::new(),
            f_vel: Vec::new(),
            fz1: Vec::new(),
            fz2: Vec::new(),
            uni_down: 1.0,
            uni_up: 1.0,
            phase2: Vec::new(),
            uni_wide: false,
            uni_width: Vec::new(),
            uni_hp: [0.0; 5],
            uni_max: Unison::WIDTH,
            uz1: 0.0,
            uz2: 0.0,
            sub_phase: Vec::new(),
            sub_min_rate: 0.0,
            spread_on: false,
            base_spread: false,
            base_level: 0.0,
            base_att_mul: 0.0,
            base_post: [0.0; 5],
            base_post_on: false,
            base_vib_inc: 0.0,
            base_pm_on: false,
            base_pm_mul: 1.0,
        }
    }

    /// Remember what prepare made of the voice (the end of `build_lanes`).
    fn capture_base(&mut self) {
        self.sampled_slot = self.sample_slot;
        self.spread_on = !self.voice_spread.is_empty();
        self.base_spread = self.spread_on;
        self.base_level = self.st[LEVEL];
        self.base_att_mul = self.st[ATT_MUL];
        self.base_post = self.post;
        self.base_post_on = self.post_on;
        self.base_vib_inc = self.vib_inc;
        self.base_pm_on = self.pm_on;
        self.base_pm_mul = self.pm_mul;
    }

    /// Every value `set_voice` writes, in one array (tests compare lanes bit for bit).
    pub fn voice_state(&self) -> Vec<f64> {
        let mut v = vec![
            self.st[LEVEL],
            self.st[ATT_MUL],
            self.decay_scale,
            self.max_decay,
            self.post_on as u8 as f64,
            self.distortion,
            self.vib_inc,
            self.vib_depth,
            self.pm_on as u8 as f64,
            self.pitch_from,
            self.pitch_to,
            self.pm_mul,
            self.mip_on as u8 as f64,
            self.spread_on as u8 as f64,
            self.sample_slot as f64,
        ];
        if self.post_on {
            v.extend_from_slice(&self.post);
        }
        v
    }

    pub fn pm_on(&self) -> bool {
        self.pm_on
    }
    pub fn mip_on(&self) -> bool {
        self.mip_on
    }
    /// Names the lane to the host (see [`SampleNote::lane`], [`LaneEnd`]): unique across the
    /// projects prepared in a process.
    pub fn uid(&self) -> u64 {
        self.uid
    }
    pub fn has_mip(&self) -> bool {
        !self.mip.is_empty()
    }
    /// How many samples of band-limited tables the lane holds (`mip.count`; 0 = none).
    pub fn mip_len(&self) -> usize {
        self.mip.len()
    }
    /// The sizes of the lane's band-limited state and its pitch multipliers: `mip`, `mipTop`,
    /// `mipBase`, `pm` (`.count` of each).
    pub fn mip_counts(&self) -> [usize; 4] {
        [
            self.mip.len(),
            self.mip_top.len(),
            self.mip_base.len(),
            self.pm.len(),
        ]
    }
    pub fn is_tonal(&self) -> bool {
        self.is_tonal
    }

    /// The sampler slot this lane plays, if it has one.
    pub fn sample_slot(&self) -> Option<usize> {
        (self.sample_slot >= 0).then_some(self.sample_slot as usize)
    }

    /// The sampler slot this lane was bound to at prepare, whatever it plays now (`sampledSlot`).
    pub fn sampled_slot(&self) -> Option<usize> {
        (self.sampled_slot >= 0).then_some(self.sampled_slot as usize)
    }

    /// How much of the lane goes to the room (`.space`; 0 = dry).
    pub fn send(&self) -> f64 {
        self.send
    }
    /// How much goes to the pad shimmer (`.shimmer`).
    pub fn shimmer_send(&self) -> f64 {
        self.shimmer_send
    }
    /// How much goes to the echo (`.echo`).
    pub fn echo_send(&self) -> f64 {
        self.echo_send
    }
    /// `.subtractive`: the voices' filter envelope as prepared - how far a full-velocity note
    /// opens it (Hz), where it rests (x the note's frequency) and its per-sample fall
    /// (`fAmount`, `fBase`, `fMul`); an amount of 0 is a lane with no filter.
    pub fn filter_envelope(&self) -> (f64, f64, f64) {
        (self.f_amount, self.f_base, self.f_mul)
    }
    /// `.unison`: the detune either side of the note as a frequency ratio pair (down, up), or
    /// None for a lane of single oscillators.
    pub fn unison_pair(&self) -> Option<(f64, f64)> {
        (!self.phase2.is_empty()).then_some((self.uni_down, self.uni_up))
    }
    /// `.subOctave`: does the lane play a sine an octave under its notes?
    pub fn has_sub_octave(&self) -> bool {
        !self.sub_phase.is_empty()
    }
    /// `.stereo`: where the lane sits (side gain; 0 = centre) and whether its voices spread
    /// either side of it (`pan`, `voiceSpread`).
    pub fn placement(&self) -> (f64, bool) {
        (self.pan, !self.voice_spread.is_empty())
    }
    /// `.stereo`: each voice's offset at full width (`voiceSpread`; empty = no spread).
    pub fn voice_spread(&self) -> &[f64] {
        &self.voice_spread
    }
    /// What the lane is for the rest schedule (`restClass`): it drops its hits while a step's
    /// mask holds its class. `Class::NONE` never rests.
    pub fn rest_class(&self) -> Class {
        self.rest_class
    }
    /// `.unison` under `.stereo`: the pair sits either side (`uniWide`).
    pub fn unison_wide(&self) -> bool {
        self.uni_wide
    }
    /// Does the lane dip on the kick (a bass or pad under a duck depth)?
    pub fn ducks(&self) -> bool {
        self.ducks
    }
    /// Does a hit on this lane key the duck (the song's own kick, under a duck depth)?
    pub fn keys_the_duck(&self) -> bool {
        self.is_kick
    }
    /// `.sectionHarmony` / `.harmonicRhythm`: the lane's group in the project's plan (`harmonyGroup`;
    /// -1 = its notes are played as bound).
    pub fn harmony_group(&self) -> i64 {
        self.harmony_group
    }
    /// `.choke`: the lane (of the same project) a hit on this one cuts off, if any.
    pub fn chokes(&self) -> Option<usize> {
        self.choke
    }
    /// `.choke`: the per-sample multiplier an open hat's tail dies by while it is choked (`chokeMul`;
    /// 1 = a lane nothing chokes).
    pub fn choke_mul(&self) -> f64 {
        self.choke_mul
    }
    /// A drum lane on a sampled kit: the GM key it plays, how long each hit is held (ms) and its
    /// velocity scale (`kitNote`, `kitHoldMs`, `kitVelocity`).
    pub fn kit(&self) -> (i64, f64, f64) {
        (self.kit_note, self.kit_hold_ms, self.kit_velocity)
    }
    /// `.hfTilt`: does the lane run the tilt lowpass after its own filter (`tiltOn`)?
    pub fn tilt_on(&self) -> bool {
        self.tilt_on
    }
    /// `.hfTilt`: the tilt lowpass's five coefficients (`tilt`).
    pub fn tilt(&self) -> [f64; 5] {
        self.tilt
    }

    /// The keys held down on this lane (live keys), in voice order.
    pub fn held_notes(&self) -> Vec<i64> {
        self.hold
            .iter()
            .zip(&self.held_note)
            .filter(|(held, _)| **held)
            .map(|(_, note)| *note)
            .collect()
    }

    /// The lane's level as prepared (the `st[LEVEL]` slot): the voice's own, times any trim.
    pub fn level(&self) -> f64 {
        self.st[LEVEL]
    }

    /// Scale the prepared level a host restores to (`0` mutes the lane for good, as the
    /// tests do to isolate a channel).
    pub fn set_base_level(&mut self, level: f64) {
        self.base_level = level;
        self.st[LEVEL] = level;
    }

    /// Install a channel's voice settings (`VoiceLaneParams`): each field the params set,
    /// else the lane's own prepared value (`Lane.setVoice`). A sampled tonal lane switches to its
    /// synth voice, and back to its sample (the voices stop first).
    pub fn set_voice(&mut self, p: &VoiceLaneParams) {
        use super::voice_tweak::VoiceLaneParams as P;
        self.st[LEVEL] = if p.has(P::LEVEL) {
            self.base_level * p.level_mul
        } else {
            self.base_level
        };
        if !self.is_tonal {
            return;
        }
        self.st[ATT_MUL] = if p.has(P::ATTACK) {
            p.att_mul
        } else {
            self.base_att_mul
        };
        self.decay_scale = if p.has(P::DECAY_SCALE) {
            p.decay_scale
        } else {
            self.voice.decay_scale
        };
        self.max_decay = if p.has(P::MAX_DECAY) {
            p.max_decay
        } else {
            self.voice.max_decay
        };
        if p.has(P::LOWPASS) {
            self.post_on = p.post_on;
            if self.post_on {
                self.post = p.b;
            }
        } else {
            self.post_on = self.base_post_on;
            self.post = self.base_post;
        }
        if !self.post_on {
            self.pz1 = 0.0;
            self.pz2 = 0.0;
        }
        self.distortion = if p.has(P::DRIVE) {
            p.distortion
        } else {
            self.voice.distortion
        };
        self.vib_inc = if p.has(P::VIB_RATE) {
            p.vib_inc
        } else {
            self.base_vib_inc
        };
        self.vib_depth = if p.has(P::VIB_DEPTH) {
            p.vib_depth
        } else {
            self.voice.vibrato_depth
        };
        // A pitch sweep needs its per-voice multipliers (every wavetable lane has them,
        // from prepare); a string has none.
        if !self.is_pluck && self.pm.len() == self.poly {
            if p.has(P::PITCH) {
                self.pm_on = p.pm_on;
                self.pitch_from = p.pitch_from;
                self.pitch_to = p.pitch_to;
                self.pm_mul = p.pm_mul;
            } else {
                self.pm_on = self.base_pm_on;
                self.pitch_from = self.voice.pitch_from;
                self.pitch_to = self.voice.pitch_to;
                self.pm_mul = self.base_pm_mul;
            }
            // (a swept saw or square reads the band-limited tables it was prepared with:
            // nothing is built here, on the audio thread)
            let want_mip = self.f_amount > 0.0 || (self.pm_on && !self.mip.is_empty());
            if want_mip && !self.mip_on {
                // voices already sounding read the plainest table until their next strike
                let top = (self.mip_top.len() - 1) * (WAVE_TABLE_SIZE + 1);
                for b in self.mip_base.iter_mut() {
                    *b = top;
                }
            }
            self.mip_on = want_mip;
            if !self.pm_on {
                for m in self.pm.iter_mut() {
                    *m = 1.0;
                }
            }
        }
        // a post chain on the voice sum takes the voices' spread away (one pan for all)
        self.spread_on = self.base_spread && !self.post_on && self.distortion == 0.0;
        if self.sampled_slot < 0 {
            return;
        }
        if p.has(P::SYNTH) && p.use_synth {
            self.sample_slot = -1;
        } else if self.sample_slot < 0 {
            // back to the sample: the synth voices stop
            for k in 0..self.poly {
                self.sd[k] = 0.0;
                self.sa[k] = 0.0;
            }
            self.sample_slot = self.sampled_slot;
        }
    }

    /// Is this lane a Karplus-Strong string (`isPluck`)?
    pub fn is_pluck(&self) -> bool {
        self.is_pluck
    }

    /// The envelope slot (the gate's current value): tests probe it.
    pub fn envelope(&self) -> f64 {
        self.st[ENV]
    }

    /// Every voice silent and every filter's memory cleared. The voice settings stay.
    pub fn silence(&mut self) {
        self.active = false;
        self.st[ENV] = 0.0;
        self.st[PREV] = 0.0;
        for x in self
            .s
            .iter_mut()
            .chain(self.sd.iter_mut())
            .chain(self.sa.iter_mut())
        {
            *x = 0.0;
        }
        for h in self.hold.iter_mut() {
            *h = false;
        }
        for h in self.held_note.iter_mut() {
            *h = -1;
        }
        for x in self.dl.iter_mut() {
            *x = 0.0;
        }
        for x in self.lp.iter_mut() {
            *x = 0.0;
        }
        for x in self.excite.iter_mut() {
            *x = 0;
        }
        for x in self.fz1.iter_mut().chain(self.fz2.iter_mut()) {
            *x = 0.0;
        }
        self.pz1 = 0.0;
        self.pz2 = 0.0;
        self.uz1 = 0.0;
        self.uz2 = 0.0;
        self.tz1 = 0.0;
        self.tz2 = 0.0;
        self.choking = false;
        self.kit_held_until = f64::NEG_INFINITY;
    }
}

fn gate_lane(voice: &VoiceSpec, sr: f64, seed: i64) -> Lane {
    let mut lane = Lane::new(voice.clone(), sr, false);
    lane.num_exp = voice.pulse.alpha.len();
    lane.alpha = voice.pulse.alpha.clone();
    lane.mul = voice
        .pulse
        .tau
        .iter()
        .map(|t| libm::exp(-1.0 / (t * sr)))
        .collect();
    lane.s = vec![0.0; lane.num_exp];
    lane.carrier = match voice.carrier.as_str() {
        "dc" => Carrier::Dc,
        "sweep" => Carrier::Sweep,
        "noise+tone" => Carrier::NoiseTone,
        _ => Carrier::Noise,
    };
    lane.rng = noise_seed(seed);
    lane.st[F0] = voice.f0;
    lane.st[F1] = voice.f1;
    lane.st[TONE_INC] = voice.tone / sr;
    lane.st[TONE_MIX] = voice.tone_mix;
    if lane.carrier == Carrier::Sweep {
        lane.st[SWEEP_MUL] = libm::exp(-1.0 / (voice.sweep_tau * sr));
    }
    if lane.carrier == Carrier::Noise || lane.carrier == Carrier::NoiseTone {
        lane.bq = biquad(&voice.filter, voice.fc, voice.q, sr);
    }
    lane
}

struct Caches {
    tables: BTreeMap<String, Arc<Vec<f32>>>,
}

fn tonal_lane(voice: &VoiceSpec, sr: f64, poly: usize, caches: &mut Caches, seed: i64) -> Lane {
    let mut lane = Lane::new(voice.clone(), sr, true);
    lane.rng = noise_seed(seed);
    if voice.engine == "pluck" {
        lane.is_pluck = true;
        lane.dmax = (sr / 16.0) as usize + 2; // down to ~16 Hz
        lane.dl = vec![0.0; poly * lane.dmax];
        lane.dw = vec![0; poly];
        lane.dlen = vec![1.0; poly];
        lane.lp = vec![0.0; poly];
        lane.fb = vec![0.0; poly];
        lane.excite = vec![0; poly];
        let fc = if voice.dampening < sr * 0.45 {
            voice.dampening
        } else {
            sr * 0.45
        };
        lane.pluck_a = 1.0 - libm::exp(-TAU * fc / sr);
    }
    lane.vib_inc = voice.vibrato_rate / sr;
    if !lane.is_pluck {
        // every wavetable lane can sweep (the sound panel turns it on live): its multipliers,
        // and for a saw or square the band-limited tables a swept note crosses octaves on
        // (shared, built once per rate), re-chosen as it falls
        lane.pm = vec![1.0; poly];
        if voice.wave == "saw" || voice.wave == "square" {
            let t = MipTables::shared(&voice.wave, sr);
            lane.mip = t.samples;
            lane.mip_top = t.top;
            lane.mip_base = vec![0; poly];
        }
    }
    if voice.pitch_time > 0.0 && voice.pitch_from != voice.pitch_to && !lane.is_pluck {
        lane.pm_on = true;
        lane.pm_mul = libm::pow(
            voice.pitch_to / voice.pitch_from,
            1.0 / (voice.pitch_time * sr),
        );
        lane.mip_on = !lane.mip.is_empty();
    }
    // the lowpass's coefficients are there on every tonal lane
    if voice.lowpass > 0.0 {
        lane.post = biquad("lowpass", voice.lowpass, 1.0, sr);
        lane.post_on = true;
    }
    lane.st[ATT_MUL] = libm::exp(-1.0 / (voice.attack * sr));
    let table = caches
        .tables
        .entry(voice.wave.clone())
        .or_insert_with(|| Arc::new(build_table(&voice.wave)))
        .clone();
    lane.table = table;
    lane.poly = poly;
    lane.inc = vec![0.0; poly];
    lane.phase = vec![0.0; poly];
    lane.sd = vec![0.0; poly];
    lane.sa = vec![0.0; poly];
    lane.md = vec![0.0; poly];
    lane.hold = vec![false; poly];
    lane.held_note = vec![-1; poly];
    lane
}

/// `.subtractive`: give a saw / square lane its band-limited tables and filter envelope
/// (`shapeVoice`; the tables are `MipTables::shared`).
pub(super) fn shape_voice(lane: &mut Lane, group: &str, plucked: &BTreeSet<String>, sr: f64) {
    if lane.is_pluck {
        return;
    }
    let Some(f) = FilterEnvelope::for_voice(&lane.voice, group, plucked) else {
        return;
    };
    let tables = MipTables::shared(&lane.voice.wave, sr);
    lane.mip = tables.samples;
    lane.mip_top = tables.top;
    lane.mip_base = vec![0; lane.poly];
    lane.mip_on = true;
    lane.f_amount = f.amount;
    lane.f_base = f.base;
    lane.f_mul = libm::exp(-1.0 / (f.seconds * sr));
    lane.f_k = 1.0 / f.q;
    lane.f_q = f.q;
    lane.f_base_v = vec![f.base; lane.poly];
    lane.f_k_v = vec![1.0 / f.q; lane.poly];
    lane.f_env = vec![0.0; lane.poly];
    lane.f_vel = vec![0.0; lane.poly];
    lane.fz1 = vec![0.0; lane.poly];
    lane.fz2 = vec![0.0; lane.poly];
}

/// `.unison`: play a pad / saw lead lane's voices as detuned pairs; spread wide under `.stereo`
/// when the pair is unfiltered (a filtered pair shares one filter: its difference would not be)
/// (`detune`).
pub(super) fn detune(lane: &mut Lane, group: &str, wide: bool, design: &VoiceDesign, sr: f64) {
    if lane.is_pluck {
        return;
    }
    let Some(c) = Unison::cents(&lane.voice, group) else {
        return;
    };
    lane.uni_down = libm::pow(2.0, -c / 1200.0);
    lane.uni_up = libm::pow(2.0, c / 1200.0);
    lane.phase2 = (0..lane.poly).map(|k| 0.25 + 0.13 * k as f64).collect();
    lane.uni_wide = wide && lane.f_amount == 0.0;
    lane.uni_width = vec![0.0; lane.poly];
    let pad = group == "harmony" || group == "pad";
    lane.uni_max = Unison::WIDTH * if pad { design.pad_width } else { 1.0 };
    lane.uni_hp = biquad(
        "highpass",
        if pad {
            design.pad_side_hz
        } else {
            Unison::SIDE_HZ
        },
        BUTTERWORTH_Q,
        sr,
    );
}

/// `.subOctave`: give a synth bass lane a sine per voice an octave down (`addSubOctave`).
pub(super) fn add_sub_octave(lane: &mut Lane, sr: f64) {
    if !lane.is_tonal || lane.is_pluck {
        return;
    }
    lane.sub_phase = vec![0.0; lane.poly];
    lane.sub_min_rate = SubOctave::MIN_HZ / sr;
}

/// What `buildLanes` does to a tonal synth lane of part `group`: the fixes that shape the
/// voice itself (`.subtractive`, `.unison`) and the genre's design of them. A lane in a song is
/// shaped inline (with the tables of the whole build); this is the same shaping for a lane
/// measured on its own (`ownSynthPartDB`).
#[derive(Clone, Debug, Default)]
pub struct VoiceShaping {
    pub subtractive: bool,
    pub unison: bool,
    /// `.subOctave` (the deck's, the genre's): the bass gets a sine an octave down.
    pub sub_octave: bool,
    pub design: VoiceDesign,
}

impl VoiceShaping {
    /// Shape `lane` (a fresh tonal lane of `group`, at `sr`) as `buildLanes` does.
    pub fn apply(&self, lane: &mut Lane, group: &str, sr: f64) {
        if self.subtractive {
            shape_voice(lane, group, &self.design.pluck_groups, sr);
        }
        if self.unison {
            detune(lane, group, false, &self.design, sr);
        }
        if self.sub_octave && group == "bass" {
            add_sub_octave(lane, sr);
        }
    }
}

/// What a channel plays (`ChannelVoice`): its instrument and that instrument's own synth voice.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct ChannelVoice {
    pub channel: i64,
    pub instrument: String,
    /// The instrument's own synth voice (`InstrumentTable`, before any tweak).
    pub voice: VoiceSpec,
    /// A synth voice with notes (false: a drum kit, whose level alone can be set).
    pub tonal: bool,
    /// Plays a sampled sound unless switched to its synth voice.
    pub sampled: bool,
    /// A Karplus-Strong string: no pitch sweep.
    pub pluck: bool,
}

static GENERATIONS: AtomicI32 = AtomicI32::new(0);

#[cfg(feature = "transfer")]
impl WaveProject {
    /// A project prepared in another engine instance and sent here as bytes (feature
    /// `transfer`, [`crate::transfer`]), made this engine's: what `build_lanes` takes from the
    /// process it runs in is taken again here. A fresh generation (and with it every lane's uid),
    /// the next track serial unless `track_serial` keeps one (another start of a song this engine
    /// already plays), the marking mirror re-made for those and seeded from the graph as
    /// `build_lanes` seeds it, and the shared band-limited tables at `sample_rate`. Everything else
    /// arrived as it was prepared, so it renders what the same prepare here would.
    pub fn installed(&mut self, sample_rate: f64, track_serial: Option<i32>, mut layout: MarkingLayout) {
        let generation = GENERATIONS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            lane.uid = ((generation as u32 as u64) << 32) | i as u64;
            if !lane.mip_base.is_empty() {
                let t = MipTables::shared(&lane.voice.wave, sample_rate);
                lane.mip = t.samples;
                lane.mip_top = t.top;
            }
        }
        let serial = track_serial.unwrap_or_else(super::marking_mirror::next_track_serial);
        self.generation = generation;
        self.track_serial = serial;
        layout.generation = generation;
        layout.track_serial = serial;
        let m = MarkingMirror::unseeded(layout);
        m.publish(&self.graph, 0);
        self.marking = Arc::new(m);
    }
}

/// `WaveProject`: a project compiled for a sample rate. Prepared off the audio thread and
/// handed over whole.
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct WaveProject {
    /// Process-wide, 1-based, increasing in the order projects are prepared. Stamped on
    /// every `FireEvent` and `TickAnchor`.
    pub generation: i32,
    /// Which song this project plays (`WaveProject.trackSerial`): a new composition takes the
    /// next number ([`next_track_serial`]); another start of the same song (a section launch,
    /// a relocation, `PreparedTrack.restarted`) is prepared with the first one's, so the two
    /// generations compare equal here and differ in `generation`.
    pub track_serial: i32,
    /// The tick the graph was seeded at before it started (`prepare(start_tick)`).
    pub start_tick: i64,
    pub graph: WaveGraph,
    /// The published copy of this project's marking, with the layout that explains it
    /// (`WaveProject.marking`). Created at prepare time on the preparing thread; the audio
    /// thread writes only into the mirror of the project it is running
    /// ([`WaveRunner::publish_marking`]), and the host keeps it by `Arc` for its readers.
    #[cfg_attr(feature = "transfer", serde(with = "super::marking_mirror::transfer"))]
    pub marking: Arc<MarkingMirror>,
    /// Which nets are stingers (`graph.isStingerNet`), for the fire events of a tick, read while
    /// the graph is borrowed mutably.
    stinger_nets: Vec<bool>,
    /// The references its sampled lanes hold on their bank's slots (`WaveProject.samplers`),
    /// given back when the project is freed (off the audio thread): what keeps a slot from being
    /// evicted while a project plays it.
    #[cfg_attr(feature = "transfer", serde(with = "held_transfer"))]
    held: HeldSlots,
    lanes: Vec<Lane>,
    by_net: BTreeMap<String, Vec<usize>>,
    /// Lane indices that act as am / fm sources: their `ENV` is copied to `PREV`.
    sources: Vec<usize>,
    /// Gain on everything this project plays, before the master curve (1 = as beats).
    pub gain: f64,
    /// When which lanes rest (the song's, plus the mix-out tail's); empty = none.
    pub rests: RestSchedule,
    /// The song's own rests, without the tail's: what another start of the song reuses.
    pub song_rests: RestSchedule,
    /// The mix-out tail this project plays; None = it ends with the song.
    pub tail: Option<MixOutTail>,
    rest_cursor: Cursor,
    /// The groove template it plays (`.feel`); None = none.
    pub feel: Option<GrooveTemplate>,
    feel_table: Option<FeelTable>,
    /// `.drift`'s key (`Drift::key`, from the track's `voicing_seed`).
    pub drift: Option<u64>,
    /// Index in `lanes` of the pooled voice that MIDI keys play (`liveLane`).
    live_lane: usize,
    /// The section energy arc the deck plays (`.energy`, read by `.sweep` too); None = none.
    pub energy: Option<EnergyCurve>,
    /// The noise riser into its drops (`.riser`, `RiserVoice`); None = none. Set by `prepare`.
    pub riser: Option<RiserVoice>,
    /// `vinyl-brake` brakes the deck while this project plays (`WaveOptions::turntable_brake`).
    pub turntable_brake: bool,
    /// The section plan the tonal lanes play through (`.sectionHarmony`, `.harmonicRhythm`);
    /// None = as bound.
    pub harmony: Option<SectionHarmony>,
    /// How far bass and pad dip on each kick (0: no ducking), and the sampler slots they play.
    pub duck_depth: f64,
    duck_slots: u64,
    /// The room this project plays in (`.space`); None = dry.
    pub space: Option<SpaceSpec>,
    /// The echo this project plays (`.echo`); None = none.
    pub echo: Option<EchoSpec>,
    /// The sampler slots of lanes that send to the room.
    send_slots: u64,
    /// The sampler slots of lanes that send to the echo (`EchoSpec::send` > 0), and each
    /// one's send (a slot two lanes share takes the larger).
    echo_slots: u64,
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::arr"))]
    slot_echo: [f32; 64],
    /// The parts' trims it plays with (`.balance`), dB; None = none.
    pub balance: Option<BTreeMap<String, f64>>,
    /// Each sampler slot's gain (1 = as loaded): `.balance`'s trim for the part it plays,
    /// less how far the instrument sits from its synth voice (`SampledBalance`).
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::arr"))]
    slot_gain: [f32; 64],
    /// The slots whose gain is not 1.
    gain_slots: u64,
    /// Each sampler slot's stereo width (1 = as loaded): `.stereo`'s `sampled_width` for the
    /// instrument it plays. The slots whose width is not 1, and those whose side is highpassed
    /// first (`SAMPLED_SIDE_HZ`), are for the host that renders them.
    #[cfg_attr(feature = "transfer", serde(with = "crate::transfer::arr"))]
    slot_width: [f32; 64],
    width_slots: u64,
    side_slots: u64,
    /// The nets whose tonal lane plays a sampled instrument, with the lane's group: what the
    /// loudness probe stands in for them.
    pub sampled_groups: BTreeMap<String, String>,
    /// The level the loudness probe plays each of those nets' synth voice at, against the voice
    /// as trimmed: where its sampler sits after the slot gain (`SampledBalance::probe_level`).
    pub probe_levels: BTreeMap<String, f64>,
    /// Each of those nets' sampler level against the net's own synth voice on its part probe
    /// (`own_part_level_db`), dB, where the slot was measured.
    pub sampled_part_db: BTreeMap<String, f64>,
}

/// The slots a project's sampled lanes took a reference on at prepare, and the bank to give them
/// back to (`WaveProject.deinit`): one `release` per sampled lane when it drops. A field of its own,
/// not a `Drop` on `WaveProject`, so the project's fields can still be moved out of it. It runs
/// wherever the project is freed, which is the host's main thread (the retire ring) or the thread
/// that prepared it and let it go (the render thread, if the retire ring is full): a host's
/// `release` should neither block nor lock.
#[derive(Default)]
struct HeldSlots {
    bank: Option<Arc<dyn SamplerSlots>>,
    /// The slot of each sampled lane (`Lane::sampled_slot`), a slot once per lane that plays it.
    slots: Vec<usize>,
}

/// A track's sampler slot references stay in the engine that bound them: a project with sampled
/// lanes is not transferable (the receiving engine has its own bank), one without carries none.
#[cfg(feature = "transfer")]
mod held_transfer {
    use super::HeldSlots;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(h: &HeldSlots, s: S) -> Result<S::Ok, S::Error> {
        if h.bank.is_some() || !h.slots.is_empty() {
            return Err(serde::ser::Error::custom("a project with sampled lanes is not transferable"));
        }
        s.serialize_unit()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HeldSlots, D::Error> {
        <() as serde::Deserialize>::deserialize(d)?;
        Ok(HeldSlots::default())
    }
}

impl Drop for HeldSlots {
    fn drop(&mut self) {
        if let Some(bank) = &self.bank {
            for slot in &self.slots {
                bank.release(*slot);
            }
        }
    }
}

impl WaveProject {
    /// The voice lanes, taken out (a project being freed a piece at a time: what is left plays
    /// nothing).
    pub fn take_lanes(&mut self) -> Vec<Lane> {
        core::mem::take(&mut self.lanes)
    }

    /// The bank this project's sampled lanes bound their slots in, set when a host binds lanes
    /// without [`WaveOptions::bank`] (by [`SampleBinder`], which has no handle to release
    /// through): the project gives the references back to it when it drops. One it has already
    /// (from `WaveOptions::bank`) stays.
    pub fn hold_slots_of(&mut self, bank: Arc<dyn SamplerSlots>) {
        self.held.bank.get_or_insert(bank);
    }

    pub fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    /// The pooled voice MIDI keys play (`lanes[liveLane]`).
    pub fn live_lane(&self) -> &Lane {
        &self.lanes[self.live_lane]
    }

    /// The sampler slots (bit i = slot i) whose lanes duck on the kick (`duckSlots`), for the host
    /// that renders them: it scales each by [`WaveRunner::duck_gains`]. 0 without a duck depth.
    pub fn duck_slots(&self) -> u64 {
        self.duck_slots
    }

    /// The sampler slots (bit i = slot i) that send to the room, for the host that renders
    /// them (`sendSlots`); 0 without `.space`.
    pub fn send_slots(&self) -> u64 {
        self.send_slots
    }

    /// The slots that send to the echo (`echoSlots`); 0 without `.echo` or a binder.
    pub fn echo_slots(&self) -> u64 {
        self.echo_slots
    }

    /// Slot `slot`'s send to the echo (`slotEcho`).
    pub fn slot_echo(&self, slot: usize) -> f32 {
        self.slot_echo.get(slot).copied().unwrap_or(0.0)
    }

    /// Slot `slot`'s gain (`slotGain`; 1 = as loaded).
    pub fn slot_gain(&self, slot: usize) -> f32 {
        self.slot_gain.get(slot).copied().unwrap_or(1.0)
    }

    /// The slots whose gain is not 1 (`gainSlots`).
    pub fn gain_slots(&self) -> u64 {
        self.gain_slots
    }

    /// Slot `slot`'s stereo width (`slotWidth`; 1 = as loaded).
    pub fn slot_width(&self, slot: usize) -> f32 {
        self.slot_width.get(slot).copied().unwrap_or(1.0)
    }

    /// The slots whose width is not 1 or whose side is highpassed first (`widthSlots`); 0
    /// without `.stereo` or a bank.
    pub fn width_slots(&self) -> u64 {
        self.width_slots
    }

    /// The slots whose side is highpassed first (`sideSlots`).
    pub fn side_slots(&self) -> u64 {
        self.side_slots
    }

    pub fn lanes_mut(&mut self) -> &mut [Lane] {
        &mut self.lanes
    }

    /// What channel `channel` plays in this project: its instrument and that instrument's voice
    /// as prepared, before any tweak. None when no net of the project is on that channel
    /// (`channelVoice`).
    pub fn channel_voice(&self, channel: i64) -> Option<ChannelVoice> {
        let mut on = self.lanes.iter().filter(|l| l.channel == channel);
        let any = on.clone().next()?;
        let first = on.find(|l| l.is_tonal).unwrap_or(any);
        Some(ChannelVoice {
            channel,
            instrument: first.instrument.clone(),
            voice: first.voice.clone(),
            tonal: first.is_tonal,
            sampled: self
                .lanes
                .iter()
                .any(|l| l.channel == channel && l.sampled_slot >= 0),
            pluck: first.is_pluck,
        })
    }

    /// `setting` made concrete for this project's lanes on `channel` (`voiceParams`).
    pub fn voice_params(
        &self,
        channel: i64,
        setting: &VoiceSetting,
        sample_rate: f64,
    ) -> VoiceLaneParams {
        let cv = self.channel_voice(channel);
        VoiceLaneParams::new(
            channel,
            &setting.tweak,
            setting.use_synth,
            cv.as_ref().map(|c| &c.voice),
            sample_rate,
        )
    }

    /// Set the voice of every lane on `p.channel`. For a project the audio thread does not have
    /// yet (a load, before the hand-over); the one it plays is set through
    /// [`WaveRunner::set_voice`] (`applyVoice`).
    pub fn apply_voice(&mut self, p: &VoiceLaneParams) {
        for lane in self
            .lanes
            .iter_mut()
            .filter(|l| l.channel == p.channel as i64)
        {
            lane.set_voice(p);
        }
    }

    /// Every channel the project has a lane on, with its voice ([`channel_voice`](Self::channel_voice)),
    /// in channel order: what the main thread keeps of a loaded project for the sound panel.
    pub fn channel_voices(&self) -> BTreeMap<i64, ChannelVoice> {
        let mut out = BTreeMap::new();
        for l in &self.lanes {
            if let alloc::collections::btree_map::Entry::Vacant(slot) = out.entry(l.channel) {
                if let Some(v) = self.channel_voice(l.channel) {
                    slot.insert(v);
                }
            }
        }
        out
    }

    /// The lane(s) a net renders through - tests use this to probe a gate.
    pub fn lanes_of(&self, net_id: &str) -> Vec<&Lane> {
        self.by_net
            .get(net_id)
            .map(|v| v.iter().map(|&i| &self.lanes[i]).collect())
            .unwrap_or_default()
    }
}

/// `buildLanes(graph, sr, opts)`: one lane per (music net, drum kind) for drum channels,
/// one pooled tonal lane per other music net. Writes the lane index of every MIDI-bound
/// transition into `net.lane`.
fn build_lanes(
    mut g: WaveGraph,
    sr: f64,
    opts: &WaveOptions,
    start_tick: i64,
    track_serial: Option<i32>,
    harmony: Option<SectionHarmony>,
    geometry: Option<&Project>,
) -> WaveProject {
    let mut song_rests = opts.rests.clone().unwrap_or_default();
    if let Some(extra) = opts.extra_rest {
        song_rests = song_rests.with_rest(extra);
    }
    let rests = match &opts.tail {
        Some(t) => song_rests.with_tail(t),
        None => song_rests.clone(),
    };
    let mut lanes: Vec<Lane> = Vec::new();
    let mut by_net: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut caches = Caches {
        tables: BTreeMap::new(),
    };
    let mut sampled_groups: BTreeMap<String, String> = BTreeMap::new();
    let mut probe_levels: BTreeMap<String, f64> = BTreeMap::new();
    let mut sampled_part_db: BTreeMap<String, f64> = BTreeMap::new();
    // `.balance` on sampled parts: each slot's gain (dB), set by the first lane that plays it
    let balance_slots = opts.musicality.contains(Fixes::BALANCE);
    let mut slot_gain_db: [Option<f64>; 64] = [None; 64];
    // `.fills` / `.buildup` / `.crash`: beats' drum-fill nets are control nets with notes; give
    // them a voice.
    let voice_fills =
        !(opts.musicality & (Fixes::FILLS | Fixes::BUILDUP | Fixes::CRASH)).is_empty();
    let voice_crash = opts.musicality.contains(Fixes::CRASH);
    let voice_open_hats = opts.musicality.contains(Fixes::OPEN_HATS);
    let tilt = if opts.musicality.contains(Fixes::HF_TILT) {
        opts.hf_tilt
    } else {
        None
    };
    let choke = opts.musicality.contains(Fixes::CHOKE);
    let space = opts.musicality.contains(Fixes::SPACE);
    let shimmer = space
        && opts.musicality.contains(Fixes::SHIMMER)
        && opts.space.is_some_and(|s| s.shimmer > 0.0);
    let echo = opts.musicality.contains(Fixes::ECHO) && opts.echo.is_some();
    let subtractive = opts.musicality.contains(Fixes::SUBTRACTIVE);
    let unison = opts.musicality.contains(Fixes::UNISON);
    let stereo = opts.musicality.contains(Fixes::STEREO);
    let kick_hz = if opts.musicality.contains(Fixes::KICK_TUNE) {
        KickTuning::hz(g.project.root_note)
    } else {
        None
    };
    // `.stereo` on sampled parts: each slot's width, set by the first lane that plays it
    let mut slot_width_of: [Option<SampledWidth>; 64] = [None; 64];
    let sub_octave = opts.sub_octave && opts.musicality.contains(Fixes::SUB_OCTAVE);
    let shaping = VoiceShaping {
        subtractive,
        unison,
        sub_octave,
        design: opts.voice_design.clone(),
    };
    let trims: BTreeMap<String, f64> = if balance_slots {
        opts.track_balance.clone().unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    // A sampled drum kit the track picked (dj's, never the parity path: it needs the deck's bank
    // and the instrument table, `.extended`); None = beats' synth kits.
    // (dj-only track fields - `picked`, `kit`, the palette's option - are not in the JSON the
    // graph is compiled from: they are read from the project itself, `geometry`)
    let kit = match (&opts.bank, opts.instruments) {
        (Some(bank), Some(_)) => drum_kit_source(geometry.unwrap_or(&g.project), bank.as_ref()),
        _ => None,
    };
    for ni in 0..g.nets.len() {
        if g.nets[ni].bundle.role == "control"
            && !(voice_fills && g.nets[ni].id.starts_with("fill-"))
        {
            continue;
        }
        let tr = g.nets[ni].bundle.track.clone();
        let id = g.nets[ni].id.clone();
        let over = opts.voices.get(&id);
        let mut kind_order: Vec<String> = Vec::new();
        let mut kind_lane: BTreeMap<String, usize> = BTreeMap::new();
        for t in 0..g.nets[ni].n_trans {
            if !g.nets[ni].midi[t] {
                continue;
            }
            let b = g.nets[ni]
                .bundle
                .bindings
                .get(&g.nets[ni].trans_ids[t])
                .copied()
                .unwrap();
            let ch = if b.channel != 0 {
                b.channel
            } else {
                tr.channel
            };
            let mut key: String = if over.is_some() {
                "v".into()
            } else if is_drum_channel(ch) {
                drum_kind(g.nets[ni].note[t]).into()
            } else {
                "tonal".into()
            };
            // `.crash`: the section crashes on fill nets get a crash, not beats' short open hat
            if voice_crash
                && key == "openhat"
                && g.nets[ni].id.starts_with("fill-")
                && g.nets[ni].note[t] == CRASH_NOTE
            {
                key = "crash".into();
            }
            // `.openHats`: beats voices notes 42-46 as one closed hat
            if voice_open_hats && key == "hat" && g.nets[ni].note[t] == OPEN_HAT_NOTE {
                key = "openhat".into();
            }
            if !kind_lane.contains_key(&key) {
                let mut lane = if let Some(o) = over {
                    if o.kind == "tonal" {
                        tonal_lane(o, sr, 4, &mut caches, lanes.len() as i64)
                    } else {
                        gate_lane(o, sr, lanes.len() as i64)
                    }
                } else if key == "tonal" {
                    let voice = match opts.instruments {
                        Some(table) => table.voice(Some(&tr.instrument), tr.group.as_deref()),
                        None => tonal_voice(Some(&tr.instrument), tr.group.as_deref()),
                    };
                    let poly = if tr.group.as_deref() == Some("harmony") {
                        6
                    } else {
                        4
                    };
                    let mut l = tonal_lane(&voice, sr, poly, &mut caches, lanes.len() as i64);
                    let part = tr.group.as_deref().unwrap_or(&g.nets[ni].bundle.riff_group);
                    let geo = geometry.and_then(|p| p.nets.get(&id)).map(|nb| &nb.track);
                    let picked = geo.map_or(tr.picked, |t| t.picked);
                    let option = geo
                        .and_then(|t| t.sound.option.as_deref())
                        .or(tr.sound.option.as_deref());
                    if let Some(slot) = bind_sampler(opts, &tr.instrument, part, picked, option) {
                        l.sample_slot = slot as i32;
                    }
                    l
                } else {
                    let brushed = opts.brushes && key == "snare";
                    let mut voice = if brushed {
                        Brush::voice()
                    } else {
                        drum_voice(&key, &tr.instrument)
                    };
                    if key == "kick" {
                        if let Some(hz) = kick_hz {
                            // `.kickTune`: the same sweep, landing on the key
                            voice.f0 *= hz / voice.f1;
                            voice.f1 = hz;
                        }
                    }
                    let mut l = gate_lane(&voice, sr, lanes.len() as i64);
                    // `.hfTilt`: the noise voices that carry the top end, lowpassed (a sampled
                    // kit's lane keeps it for when it plays its synth voice)
                    if let Some(tilt) = tilt {
                        if let Some(hz) = tilt.lowpass_hz(&key) {
                            if l.carrier == Carrier::Noise && !brushed {
                                l.tilt = biquad("lowpass", hz.min(0.45 * sr), tilt.q, sr);
                                l.tilt_on = true;
                            }
                        }
                    }
                    if let (Some(kit), Some(bank)) = (&kit, &opts.bank) {
                        let mut bound = false;
                        if brushed {
                            // the bank's Brush kit plays the snare
                            if let Some(brush) = bank.brush_kit(kit) {
                                bound = bind_kit(&mut l, &key, &brush, bank.as_ref());
                            }
                        }
                        // no Brush kit in the bank: the kit's own snare, played softer
                        if !bound && bind_kit(&mut l, &key, kit, bank.as_ref()) && brushed {
                            l.kit_velocity = Brush::stock_velocity();
                        }
                        // `.balance`: the kit was matched to the synth kit before its trims; the
                        // kick's trim moves the whole kit (its own balance is the kit's)
                        if !trims.is_empty() {
                            if let Some(db) = trims.get("kick") {
                                l.kit_velocity *= velocity_scale(*db);
                            }
                        }
                    }
                    l
                };
                lane.net_id = id.clone();
                lane.kind = key.clone();
                lane.channel = tr.channel;
                lane.instrument = tr.instrument.clone();
                let group = tr.group.as_deref().unwrap_or(&g.nets[ni].bundle.riff_group);
                if key == "tonal" && over.is_none() {
                    if subtractive {
                        shape_voice(&mut lane, group, &opts.voice_design.pluck_groups, sr);
                    }
                    if unison {
                        detune(&mut lane, group, stereo, &opts.voice_design, sr);
                    }
                    if sub_octave && group == "bass" {
                        add_sub_octave(&mut lane, sr);
                    }
                }
                if key == "tonal" && lane.sample_slot >= 0 {
                    sampled_groups.insert(id.clone(), String::from(group));
                    if stereo
                        && lane.sample_slot < 64
                        && slot_width_of[lane.sample_slot as usize].is_none()
                    {
                        let category = VoiceCatalog::bundled()
                            .category_of
                            .get(&tr.instrument)
                            .cloned();
                        slot_width_of[lane.sample_slot as usize] =
                            Some(sampled_width(category.as_deref(), group));
                    }
                    // the slot's own part level, measured at its load: the probe plays the synth
                    // part where this sampler sits, after its `.balance` gain. Unmeasured, the
                    // table's average. Taken against this lane's own synth voice, not the group's
                    // plain one (`own_part_level_db`): that voice is what the trim was measured
                    // on and what the probe plays in the sampler's place.
                    let slot = lane.sample_slot as usize;
                    let level = opts.part_level.and_then(|measure| {
                        let rate = if opts.bank_rate > 0.0 {
                            opts.bank_rate
                        } else {
                            sr
                        };
                        // the lane shaped as it is below
                        own_part_level_db(measure(slot, group), &lane.voice, group, rate, &|l| {
                            shaping.apply(l, group, rate)
                        })
                    });
                    if let Some(l) = level {
                        sampled_part_db.insert(id.clone(), l);
                    }
                    if balance_slots && slot < 64 {
                        let trim = if id.starts_with("fill-") {
                            0.0
                        } else {
                            MixBalance::part_of_kind("tonal", group)
                                .and_then(|p| trims.get(p).copied())
                                .unwrap_or(0.0)
                        };
                        let gain_db = slot_gain_db[slot]
                            .unwrap_or_else(|| SampledBalance::gain_db(trim, level));
                        slot_gain_db[slot] = Some(gain_db);
                        probe_levels.insert(
                            id.clone(),
                            SampledBalance::probe_level(trim, level, group, gain_db),
                        );
                    } else {
                        probe_levels.insert(id.clone(), Loudness::sampled_part_level(group, level));
                    }
                }
                if space {
                    lane.send = SpaceSpec::send(&key, group);
                    if shimmer && SpaceSpec::shimmers(&key, group) {
                        lane.shimmer_send = lane.send;
                    }
                }
                // (a sampled lane's too: it is used if the lane plays its synth voice instead)
                if echo {
                    lane.echo_send = EchoSpec::send(&key, group);
                }
                if stereo {
                    let (pan, width) = stereo_placement(&key, group);
                    lane.pan = pan;
                    let spread = width
                        * if group == "harmony" {
                            opts.voice_design.pad_width
                        } else {
                            1.0
                        };
                    // (a voice post chain - lowpass, distortion - runs on the voice sum: one
                    // pan for all)
                    if spread > 0.0
                        && lane.is_tonal
                        && !lane.post_on
                        && lane.voice.distortion == 0.0
                    {
                        lane.voice_spread = (0..lane.poly)
                            .map(|k| if k % 2 == 0 { spread } else { -spread })
                            .collect();
                        lane.voice_pan = vec![pan; lane.poly];
                    }
                }
                if opts.duck_depth > 0.0 {
                    lane.ducks = key == "tonal"
                        && matches!(tr.group.as_deref(), Some("bass") | Some("harmony"));
                    lane.is_kick =
                        key == "kick" && (g.nets[ni].bundle.riff_group == "kick" || id == "kick");
                }
                // `.balance`: the part's trim (fills and crashes are no part's: as composed)
                if !trims.is_empty() && over.is_none() && !id.starts_with("fill-") {
                    if let Some(db) =
                        MixBalance::part_of_kind(&key, group).and_then(|part| trims.get(part))
                    {
                        lane.st[LEVEL] *= libm::pow(10.0, db / 20.0);
                    }
                }
                // the loudness probe: a sampled part about as loud as its sampler
                if key == "tonal" {
                    if let Some(level) = opts.probe_levels.get(&id) {
                        lane.st[LEVEL] *= *level;
                    }
                }
                if !rests.is_empty() && over.is_none() && !g.is_stinger_net[ni] {
                    lane.rest_class = Class::of(&key, group, &id);
                }
                if let Some(h) = &harmony {
                    if lane.is_tonal {
                        lane.harmony_group = h
                            .group_index(&group_name(&g.nets[ni].bundle, &id))
                            .map_or(-1, |gi| gi as i64);
                    }
                }
                kind_lane.insert(key.clone(), lanes.len());
                kind_order.push(key.clone());
                lanes.push(lane);
            }
            g.nets[ni].lane[t] = kind_lane[&key] as i64;
        }
        if !kind_order.is_empty() {
            by_net.insert(id, kind_order.iter().map(|k| kind_lane[k]).collect());
        }
        // `.choke`: the net's closed hat cuts its open hat off (10 ms)
        if choke {
            if let (Some(&h), Some(&o)) = (kind_lane.get("hat"), kind_lane.get("openhat")) {
                lanes[h].choke = Some(o);
                lanes[o].choke_mul = libm::exp(-1.0 / (0.01 * sr));
            }
        }
    }
    // Nested modulation: an outer ring's gate scales (am) or bends (fm) an inner ring. The
    // source's previous-sample gate (`PREV`) is used, so the order lanes are evaluated in
    // never matters.
    let mut sources: Vec<usize> = Vec::new();
    for m in &opts.mods {
        let (Some(src), Some(dst)) = (by_net.get(&m.source), by_net.get(&m.target)) else {
            continue;
        };
        let src = src.clone();
        for &li in dst.clone().iter() {
            if m.kind == "fm" {
                lanes[li].fm = Some(src[0]);
                lanes[li].st[FM_DEPTH] = m.depth;
            } else {
                lanes[li].am = Some(src[0]);
                lanes[li].st[AM_DEPTH] = m.depth;
            }
        }
        if m.silent_source {
            for &li in &src {
                lanes[li].st[LEVEL] = 0.0;
            }
        }
        if !sources.contains(&src[0]) {
            sources.push(src[0]);
        }
    }
    // Live keys: one extra pooled voice, shaped like the melody net's instrument.
    let live_instrument: String = g
        .nets
        .iter()
        .find(|n| n.bundle.riff_group == "melody" || n.id == "melody")
        .map(|n| n.bundle.track.instrument.clone())
        .unwrap_or_default();
    let live_voice = match opts.instruments {
        Some(table) => table.voice(Some(&live_instrument), Some("melody")),
        None => tonal_voice(Some(&live_instrument), Some("melody")),
    };
    let mut live = tonal_lane(&live_voice, sr, 8, &mut caches, lanes.len() as i64);
    let live_option: Option<String> = g
        .nets
        .iter()
        .find(|n| n.bundle.riff_group == "melody" || n.id == "melody")
        .and_then(|n| geometry?.nets.get(&n.id)?.track.sound.option.clone());
    if let Some(slot) = bind_sampler(
        opts,
        &live_instrument,
        "melody",
        false,
        live_option.as_deref(),
    ) {
        live.sample_slot = slot as i32;
    }
    if subtractive {
        shape_voice(&mut live, "melody", &BTreeSet::new(), sr);
    }
    if unison {
        detune(&mut live, "melody", stereo, &VoiceDesign::default(), sr);
    }
    live.net_id = String::from("<live>");
    if space {
        live.send = SpaceSpec::send("tonal", "melody");
    }
    if echo {
        live.echo_send = EchoSpec::send("tonal", "melody");
    }
    let live_lane = lanes.len();
    lanes.push(live);
    for lane in lanes.iter_mut() {
        lane.capture_base();
    }
    // the sampler slots that send, echo or carry a gain, for the host that renders them
    let has_bank = opts.sample_binder.is_some() || opts.bank.is_some();
    let mut sending = 0u64;
    let mut echoing = 0u64;
    let mut ducking = 0u64;
    let mut slot_echo = [0f32; 64];
    for lane in &lanes {
        if lane.sample_slot >= 0 && lane.sample_slot < 64 {
            let slot = lane.sample_slot as usize;
            if lane.ducks {
                ducking |= 1 << slot;
            }
            if lane.send > 0.0 {
                sending |= 1 << slot;
            }
            if lane.echo_send > 0.0 {
                echoing |= 1 << slot;
                slot_echo[slot] = slot_echo[slot].max(lane.echo_send as f32);
            }
        }
    }
    let mut slot_gain = [1f32; 64];
    let mut gain_slots = 0u64;
    for i in 0..64 {
        let gain = libm::pow(10.0, slot_gain_db[i].unwrap_or(0.0) / 20.0) as f32;
        if (gain - 1.0).abs() > 1e-4 {
            slot_gain[i] = gain;
            gain_slots |= 1 << i;
        }
    }
    let mut slot_width = [1f32; 64];
    let (mut width_slots, mut side_slots) = (0u64, 0u64);
    for (i, w) in slot_width_of.iter().enumerate() {
        let Some(w) = w else { continue };
        slot_width[i] = w.width as f32;
        if (w.width - 1.0).abs() > 1e-4 {
            width_slots |= 1 << i;
        }
        if w.side_hz.is_some() {
            side_slots |= 1 << i;
        }
    }
    let project_space = if space {
        Some(opts.space.unwrap_or_else(SpaceSpec::room))
    } else {
        None
    };
    let project_echo = if echo { opts.echo } else { None };
    // the template plays only with `.feel` in the musicality (`feel: opts.musicality.contains(
    // .feel) ? opts.feel : nil`): with the flag off a template in the options is not read, and
    // the render is bit for bit what it is without one (`FeelTests.withoutTheFlagIsBitIdentical`)
    let feel = if opts.musicality.contains(Fixes::FEEL) {
        opts.feel.clone()
    } else {
        None
    };
    let generation = GENERATIONS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    for (i, lane) in lanes.iter_mut().enumerate() {
        lane.uid = ((generation as u32 as u64) << 32) | i as u64;
    }
    let track_serial = track_serial.unwrap_or_else(super::marking_mirror::next_track_serial);
    // (the compiled graph is built from the project's JSON, which has no coordinates: the layout
    // takes the net positions from the project itself; `prepare(json:)` has only the graph's own)
    let stinger_nets = g.is_stinger_net.clone();
    let marking = Arc::new(MarkingMirror::new(
        generation,
        track_serial,
        start_tick,
        &g,
        geometry.unwrap_or(&g.project),
    ));
    // the references the sampled lanes took (`bindSampler`, `bindKit`), to give back on drop
    let held = HeldSlots {
        bank: opts.bank.clone(),
        slots: lanes.iter().filter_map(Lane::sampled_slot).collect(),
    };
    WaveProject {
        generation,
        track_serial,
        start_tick,
        graph: g,
        marking,
        stinger_nets,
        held,
        lanes,
        by_net,
        sources,
        gain: 1.0,
        rests,
        song_rests,
        tail: opts.tail,
        rest_cursor: Cursor::new(),
        feel_table: feel.as_ref().map(FeelTable::new),
        feel,
        drift: if opts.drift || opts.musicality.contains(Fixes::DRIFT) {
            Some(Drift::key(opts.voicing_seed))
        } else {
            None
        },
        live_lane,
        energy: None,
        riser: None,
        turntable_brake: opts.turntable_brake,
        harmony,
        duck_depth: opts.duck_depth,
        duck_slots: ducking,
        space: project_space,
        send_slots: if project_space.is_none() { 0 } else { sending },
        echo: project_echo,
        echo_slots: if project_echo.is_none() || !has_bank {
            0
        } else {
            echoing
        },
        slot_echo,
        balance: if trims.is_empty() { None } else { Some(trims) },
        slot_gain,
        gain_slots: if has_bank { gain_slots } else { 0 },
        slot_width,
        width_slots: if has_bank {
            width_slots | side_slots
        } else {
            0
        },
        side_slots: if has_bank { side_slots } else { 0 },
        sampled_groups,
        probe_levels,
        sampled_part_db,
    }
}

/// Play `instrument` on the deck's sampler when its category picks a sampled sound for this role
/// and one loads (`bindSampler`): the slot, or None for a synth voice.
///
/// `option` (`TrackSound.option`, what the palette or the user chose for the role): a synth
/// voice plays no sample; a sampled option's sounds come first, unless the user picked the
/// instrument; the instrument's own chain follows as the fallback. Without a [`SamplerSlots`]
/// bank the old [`SampleBinder`] is asked by name alone.
fn bind_sampler(
    opts: &WaveOptions,
    instrument: &str,
    role: &str,
    picked: bool,
    option: Option<&str>,
) -> Option<usize> {
    let Some(bank) = &opts.bank else {
        return opts.sample_binder?(instrument, role);
    };
    opts.instruments?;
    if !picked {
        if let Some(o) = option {
            if o == "synth" || synth_name(o).is_some() {
                return None;
            }
        }
    }
    let role = if role.is_empty() { "melody" } else { role };
    let own = bank.instrument_sources(instrument, role, opts.voicing_seed, picked);
    let from_palette = if picked {
        None
    } else {
        option.and_then(|o| bank.option_sources(o, role, opts.voicing_seed))
    };
    let sources = match from_palette {
        Some(mut first) => {
            first.extend(own.unwrap_or_default());
            first
        }
        None => own?,
    };
    bank.slot_for(&sources)
}

/// The kit a project's drums play: the first drum-channel net's `Track.kit` (a share `tracks`
/// override on the drum channels), resolved to an installed bank; None = the synth kit
/// (`drumKitSource`).
fn drum_kit_source(project: &Project, bank: &dyn SamplerSlots) -> Option<SamplerSource> {
    for (_, nb) in project.nets.iter() {
        if nb.role == "control" || !is_drum_channel(nb.track.channel) {
            continue;
        }
        if let Some(k) = nb.track.kit.as_ref().or(nb.track.sound.kit.as_ref()) {
            return bank.kit_source(k);
        }
    }
    None
}

/// Play a drum lane's hits on the kit (its kind's GM key), when the kit loads in the deck's bank;
/// else the lane keeps beats' synth voice. Whether it bound (`bindKit`).
fn bind_kit(lane: &mut Lane, kind: &str, kit: &SamplerSource, bank: &dyn SamplerSlots) -> bool {
    let Some(slot) = bank.slot_for(core::slice::from_ref(kit)) else {
        return false;
    };
    lane.sample_slot = slot as i32;
    lane.kit_note = gm_note(kind);
    lane.kit_hold_ms = hold_ms(kind);
    true
}

impl Lane {
    /// The lane's side gain as the mix sees it: its stereo placement, or a pan macro's hold
    /// (left = mid + side, so a pan to the left is a positive side).
    #[inline]
    fn side_gain(&self) -> f64 {
        if self.pan_move.is_nan() {
            self.pan
        } else {
            -self.pan_move
        }
    }
}

/// `strike`: tune voice `v` to `note`; a string is damped and plucked again. `sweep_x`: how far
/// through a buildup the note is (`.sweep`), -1 outside one.
fn strike(lane: &mut Lane, sr: f64, v: usize, note: i64, sweep_x: f64) {
    lane.inc[v] = 440.0 * libm::pow(2.0, (note as f64 - 69.0) / 12.0) * lane.st[INV_SR];
    if !lane.voice_spread.is_empty() {
        // centred at E3 and below (the low end stays mono), full width from G4 up
        let width = (((note - 52) as f64) / 15.0).clamp(0.0, 1.0);
        lane.voice_pan[v] = lane.pan + lane.voice_spread[v] * width;
    }
    if lane.pm_on {
        lane.pm[v] = lane.pitch_from;
    }
    if lane.mip_on {
        let hz = lane.inc[v] * sr * if lane.pm_on { lane.pitch_from } else { 1.0 };
        let mut k = 0;
        while k + 1 < lane.mip_top.len() && lane.mip_top[k] < hz * 1.06 {
            k += 1;
        }
        lane.mip_base[v] = k * (WAVE_TABLE_SIZE + 1);
    }
    if lane.f_amount > 0.0 {
        lane.f_env[v] = 1.0;
        // `.sweep`: a buildup's notes struck darker at first, brighter and more resonant as it
        // goes; any other note gets the lane's own values (bit-identical renders outside one)
        if sweep_x >= 0.0 {
            lane.f_base_v[v] = BuildupSweep::base(lane.f_base, sweep_x);
            lane.f_k_v[v] = 1.0 / BuildupSweep::q(lane.f_q, sweep_x);
        } else {
            lane.f_base_v[v] = lane.f_base;
            lane.f_k_v[v] = lane.f_k;
        }
    }
    if lane.uni_wide {
        // centred at A3 and below (the pair's beating would widen the low mids), full from C5
        lane.uni_width[v] = lane.uni_max * (((note - 57) as f64) / 15.0).clamp(0.0, 1.0);
    }
    if !lane.is_pluck {
        return;
    }
    let period = 1.0 / lane.inc[v];
    // The loop lowpass delays the note by its phase delay at f0: take that off the line so
    // the string plays in tune.
    let w = TAU * lane.inc[v];
    let b = 1.0 - lane.pluck_a;
    let lp_delay = libm::atan2(b * libm::sin(w), 1.0 - b * libm::cos(w)) / w;
    let cap = (lane.dmax - 2) as f64;
    let len = {
        let x = if period - lp_delay > 2.0 {
            period - lp_delay
        } else {
            2.0
        };
        if x < cap {
            x
        } else {
            cap
        }
    };
    lane.dlen[v] = len;
    // `resonance` is the feedback per period of a 220 Hz string; scaled so every pitch
    // rings for the same time.
    lane.fb[v] = libm::pow(lane.voice.resonance, 220.0 * lane.st[INV_SR] * period);
    let base = v * lane.dmax;
    for i in base..(base + lane.dmax) {
        lane.dl[i] = 0.0;
    }
    lane.lp[v] = 0.0;
    let n = (lane.voice.attack_noise * len) as i64;
    lane.excite[v] = n.max(1);
}

/// Start a note on a tonal lane: steal the quietest voice (deterministic), continuing from
/// its current level, and decay over the note's length (`trigger`).
fn trigger(lane: &mut Lane, sr: f64, note: i64, w: f64, dur_ms: f64, sweep_x: f64) {
    lane.active = true;
    let mut v = 0;
    let mut best = f64::INFINITY;
    for k in 0..lane.poly {
        let a = libm::fabs(lane.sd[k] + lane.sa[k]);
        if a < best {
            best = a;
            v = k;
        }
    }
    let cur = lane.sd[v] + lane.sa[v];
    strike(lane, sr, v, note, sweep_x);
    if lane.f_amount > 0.0 {
        lane.f_vel[v] = w * w;
    }
    // midiToHz and noteDecay (synth.js), inlined as the JS does
    let mut tau = (if dur_ms > 0.0 { dur_ms } else { 100.0 }) / 1000.0 * lane.decay_scale;
    // a shape macro (loosen) can stretch a note past the lane's own ceiling
    let mut ceiling = lane.max_decay;
    if lane.decay_move != 1.0 {
        tau *= lane.decay_move;
        ceiling *= lane.decay_move.max(1.0);
    }
    tau = if tau < 0.03 {
        0.03
    } else if tau > ceiling {
        ceiling
    } else {
        tau
    };
    lane.md[v] = libm::exp(-lane.st[INV_SR] / tau);
    lane.sd[v] = w;
    lane.sa[v] = cur - w;
}

/// `sweepProgress`: how far through a buildup the step a note was due on is (`tick` is the
/// graph's counter after it), or -1 outside one (or without `.sweep`).
#[inline]
fn sweep_progress(on: bool, curve: Option<&EnergyCurve>, tick: i64) -> f64 {
    if !on {
        return -1.0;
    }
    curve
        .and_then(|c| c.ramp_progress((tick - 1).max(0)))
        .unwrap_or(-1.0)
}

/// The note transition `t` of `n` sounds on `lane` at song step `step`: as bound, or moved by the
/// section plan (`.sectionHarmony`) (`playedNote`).
#[inline]
fn played_note(
    harmony: Option<&SectionHarmony>,
    lane: &Lane,
    n: &CompiledNet,
    t: usize,
    step: i64,
) -> i64 {
    let note = n.note[t];
    let Some(h) = harmony else {
        return note;
    };
    if lane.harmony_group < 0 || is_drum_channel(n.channel[t] as i64) {
        return note;
    }
    h.note(note, lane.harmony_group as usize, step)
}

/// Where `onHit` reports go (`logHit`): the hit's frame, the frame of its tick, the tick.
struct LogTo<'a> {
    out: &'a mut Vec<HitRecord>,
    frame: i64,
    grid_frame: i64,
    tick: i64,
}

/// How long a sampled hit holds its note, in samples: at least 120 ms, so one-step notes still
/// speak (`samplerNote`).
#[inline]
fn hold_samples(ms: f64, sr: f64) -> f64 {
    (if ms > 0.0 { ms } else { 120.0 }).max(120.0) / 1000.0 * sr
}

/// End the note a sampled kit lane still holds, now (a choke, or the same key struck again): its
/// note-off goes to the host as raw MIDI (`endSamplerNotes`). The lane knows when its last hit's
/// hold runs out; the host ends it then itself.
#[inline]
fn end_kit_note(lane: &mut Lane, now: f64, frame: i64, keys: &mut Vec<SampleKey>) {
    if lane.kit_held_until <= now {
        return;
    }
    lane.kit_held_until = f64::NEG_INFINITY;
    if keys.len() < keys.capacity() {
        keys.push(SampleKey {
            slot: lane.sample_slot as usize,
            status: 0x80,
            data1: lane.kit_note.clamp(0, 127) as u8,
            data2: 0,
            frame,
        });
    }
}

/// Onset of transition `t` of compiled net `n` on lane `li`, at tempo `tempo` (`hit`).
/// `note` is what a tonal lane plays; `velocity` replaces the binding's (the groove's).
#[allow(clippy::too_many_arguments)]
fn hit(
    lanes: &mut [Lane],
    duck_hold: &mut f64,
    li: usize,
    sr: f64,
    tempo: f64,
    n: &CompiledNet,
    t: usize,
    note: i64,
    velocity: Option<i64>,
    sweep_x: f64,
    log: Option<LogTo<'_>>,
    frame: i64,
    now: f64,
    keys: &mut Vec<SampleKey>,
) -> Option<SampleNote> {
    let lane = &mut lanes[li];
    lane.active = true;
    let vel = velocity.unwrap_or(n.vel[t]);
    if lane.is_kick {
        *duck_hold = 1.0;
    }
    if let Some(l) = log {
        l.out.push(HitRecord {
            frame: l.frame,
            grid_frame: if l.grid_frame >= 0 {
                l.grid_frame
            } else {
                l.frame
            },
            tick: l.tick,
            net: n.id.clone(),
            note,
            channel: lane.channel,
            kind: lane.kind.clone(),
            velocity: vel,
        });
    }
    let w = vel as f64 / 127.0;
    if !lane.is_tonal {
        if lane.sample_slot >= 0 {
            // a sampled kit: the hit is its GM key; a closed hat ends the open hat's ring
            let choke = lane.choke;
            if let Some(o) = choke {
                if lanes[o].sample_slot >= 0 {
                    end_kit_note(&mut lanes[o], now, frame, keys);
                }
            }
            let lane = &mut lanes[li];
            // (a note struck again while held is ended first)
            end_kit_note(lane, now, frame, keys);
            let velocity = (libm::round((w * lane.kit_velocity).min(1.0) * 127.0) as i64).max(1);
            lane.kit_held_until = now + hold_samples(lane.kit_hold_ms, sr);
            return Some(SampleNote {
                slot: lane.sample_slot as usize,
                note: lane.kit_note.clamp(0, 127),
                velocity,
                dur_ms: lane.kit_hold_ms,
                frame,
                lane: lane.uid,
            });
        }
        // `.choke`: a closed hat that is sounding cuts its open hat's tail short
        if let Some(o) = lane.choke {
            if lanes[o].active {
                lanes[o].choking = true;
            }
        }
        let lane = &mut lanes[li];
        lane.choking = false;
        for e in 0..lane.num_exp {
            lane.s[e] += lane.alpha[e] * w;
        }
        if lane.carrier == Carrier::Sweep {
            lane.st[SWEEP] = 1.0;
        }
        return None;
    }
    let steps = n.dur_steps[t];
    let dur_ms = if steps > 0.0 {
        steps * 60000.0 / (tempo * PPQ)
    } else {
        n.dur_ms[t]
    };
    if lane.sample_slot >= 0 {
        // the note sounds on the host's sampler; this lane only reports it
        lane.active = false;
        return Some(SampleNote {
            slot: lane.sample_slot as usize,
            note,
            velocity: vel,
            dur_ms,
            frame,
            lane: lane.uid,
        });
    }
    trigger(lane, sr, note, w, dur_ms, sweep_x);
    None
}

/// A note the groove holds back until `due` (a sample time).
struct Held {
    due: f64,
    net: usize,
    trans: usize,
    lane: usize,
    vel: i64,
    tick: i64,
    grid: i64,
    note: i64,
}

const GROOVE_CAPACITY: usize = 1024;
const SAMPLE_NOTE_CAPACITY: usize = 1024;
const SAMPLE_KEY_CAPACITY: usize = 1024;
const LANE_END_CAPACITY: usize = 256;
/// The sampled instruments' send fifos (`sendFifo`, `echoFifo`): a few blocks of samples.
const FIFO_CAPACITY: usize = 8192;
const RETIRED_CAPACITY: usize = 32;

/// What `grooveHit` works the timing out from.
struct GrooveCtx<'a> {
    feel: Option<&'a FeelTable>,
    drift: Option<u64>,
    groove_on: bool,
    swing: f64,
    humanize: f64,
    tempo: f64,
    /// The song step being played (the graph's tick before the advance).
    step: i64,
}

impl GrooveCtx<'_> {
    /// Is any groove at work on this project? (`doTick`'s `groove`.)
    fn active(&self) -> bool {
        self.feel.is_some()
            || self.groove_on && (self.swing > 0.0 || self.humanize > 0.0)
            || self.drift.is_some() && self.humanize > 0.0
    }

    /// beats' browser timing, the family's template and/or the drift: the delay (ms) and
    /// velocity of one hit (`grooveHit`).
    fn timing(&self, tonal: bool, n: &CompiledNet, t: usize, note: i64) -> (f64, i64) {
        let step = self.step;
        let mut delay_ms = 0.0;
        let mut vel = n.vel[t];
        if let Some(feel) = self.feel {
            let bar = (step & (STEPS as i64 - 1)) as usize;
            delay_ms = feel.swing_ms(bar, self.tempo);
            if !tonal {
                if let Some(v) = Voice::from_note(note) {
                    let c = FeelTable::cell(v, bar);
                    delay_ms += feel.offset_ms[c];
                    vel = (libm::round(vel as f64 * feel.velocity[c]) as i64).clamp(1, 127);
                }
            }
        } else if self.drift.is_some() {
            if self.groove_on && step % 2 == 1 {
                delay_ms = self.swing / 100.0 * 60000.0 / (self.tempo * PPQ) * 0.5;
            }
        } else {
            let tick_ms = 60000.0 / (self.tempo * PPQ);
            let h = self.humanize / 100.0;
            let mut r = groove_hash(step, n.index as i64, t as i64);
            let r1 = (r & 0xFFFF) as f64 / 65535.0 * 2.0 - 1.0;
            r >>= 16;
            let r2 = (r & 0xFFFF) as f64 / 65535.0 * 2.0 - 1.0;
            if step % 2 == 1 {
                delay_ms = (self.swing / 100.0 * tick_ms * 0.5 + r1 * h * 30.0).max(0.0);
            } else if h > 0.0 {
                delay_ms = (r1 * h * 15.0).max(0.0);
            }
            if h > 0.0 {
                vel = (libm::round(vel as f64 + r2 * h * 15.0) as i64).clamp(1, 127);
            }
        }
        if let Some(key) = self.drift {
            let h = self.humanize / 100.0;
            if h > 0.0 {
                delay_ms += Drift::late_ms(key, n.index as i64, step, h);
                vel = (vel + Drift::velocity(key, n.index as i64, step, h)).clamp(1, 127);
            }
        }
        (delay_ms, vel)
    }
}

#[inline]
fn xorshift(x: &mut i32) -> f64 {
    let mut v = *x;
    v ^= v.wrapping_shl(13);
    v ^= ((v as u32) >> 17) as i32;
    v ^= v.wrapping_shl(5);
    *x = v;
    v as f64 / 2_147_483_648.0
}

/// One sample of string `k` (Karplus-Strong: a delay of one period, a lowpass and
/// feedback in the loop, excited by a noise burst). `rate` bends the pitch.
fn pluck_sample(lane: &mut Lane, k: usize, rate: f64) -> f64 {
    let base = k * lane.dmax;
    let dmax = lane.dmax as f64;
    let max_d = dmax - 2.0;
    let q = lane.dlen[k] / rate;
    let d = if q < max_d { q } else { max_d };
    let mut rp = lane.dw[k] as f64 - d;
    if rp < 0.0 {
        rp += dmax;
    }
    if rp >= dmax {
        rp -= dmax; // -ε + dmax rounds to dmax
    }
    let i0 = rp as usize;
    let fr = rp - i0 as f64;
    let i1 = if i0 + 1 == lane.dmax { 0 } else { i0 + 1 };
    let a = lane.dl[base + i0] as f64;
    let y = a + (lane.dl[base + i1] as f64 - a) * fr;
    lane.lp[k] += lane.pluck_a * (y - lane.lp[k]);
    let mut input = 0.0;
    if lane.excite[k] > 0 {
        input = xorshift(&mut lane.rng);
        lane.excite[k] -= 1;
    }
    lane.dl[base + lane.dw[k]] = (input + lane.lp[k] * lane.fb[k]) as f32;
    lane.dw[k] = if lane.dw[k] + 1 == lane.dmax {
        0
    } else {
        lane.dw[k] + 1
    };
    y
}

#[inline]
fn table_read(lane: &Lane, k: usize, ph: f64) -> f64 {
    let x = ph * WAVE_TABLE_SIZE as f64;
    let i = x as usize;
    let fr = x - i as f64;
    if !lane.mip_on {
        let a = lane.table[i] as f64;
        let b = lane.table[i + 1] as f64;
        return a + (b - a) * fr;
    }
    let a = lane.mip[lane.mip_base[k] + i] as f64;
    let b = lane.mip[lane.mip_base[k] + i + 1] as f64;
    a + (b - a) * fr
}

/// What the lanes of one sample add up to (`acc`, `accSend`, `accShimmer`, `accEcho`): the
/// mix, and what goes on to the room, its shimmer and the echo.
struct Mix {
    acc: f64,
    acc_send: f64,
    acc_shimmer: f64,
    acc_echo: f64,
    /// The bass and pad's gain on the kick (`duckGain`; 1 = not ducked).
    duck_gain: f64,
    /// `.energy`: the tonal voices (and the returns) sum apart in `acc_tone`, so the section's
    /// lowpass leaves the drums alone (`splitTone`).
    split_tone: bool,
    acc_tone: f64,
    /// `.stereo`: the side signal beside the mid one (`accS`, `accToneS`): left = mid + side,
    /// right = mid - side. Without it the side stays 0 and is never read.
    acc_s: f64,
    acc_tone_s: f64,
}

impl Mix {
    fn new(duck_gain: f64, split_tone: bool) -> Mix {
        Mix {
            acc: 0.0,
            acc_send: 0.0,
            acc_shimmer: 0.0,
            acc_echo: 0.0,
            duck_gain,
            split_tone,
            acc_tone: 0.0,
            acc_s: 0.0,
            acc_tone_s: 0.0,
        }
    }

    /// Add to the tonal side of the mix (`acc` itself, unless the tone is split off), `s` being
    /// its side.
    fn add_tone(&mut self, x: f64, s: f64) {
        if self.split_tone {
            self.acc_tone += x;
            self.acc_tone_s += s;
        } else {
            self.acc += x;
            self.acc_s += s;
        }
    }
}

/// One sample of lane `li`, added into `mix` (`laneSample`).
fn lane_sample(lanes: &mut [Lane], li: usize, sr: f64, mix: &mut Mix) {
    let fm_prev = lanes[li].fm.map(|j| lanes[j].st[PREV]);
    let am_prev = lanes[li].am.map(|j| lanes[j].st[PREV]);
    let lane = &mut lanes[li];
    if lane.sample_slot >= 0 {
        // a sampled lane: its notes sound on the host's sampler
        lane.active = false;
        return;
    }
    if !lane.is_tonal {
        let mut env = 0.0;
        let choke = if lane.choking { lane.choke_mul } else { 1.0 };
        for e in 0..lane.num_exp {
            env += lane.s[e];
            lane.s[e] *= lane.mul[e] * choke;
        }
        lane.st[ENV] = env;
        let mut c = 1.0;
        if lane.carrier == Carrier::Sweep {
            let mut f = lane.st[F1] + (lane.st[F0] - lane.st[F1]) * lane.st[SWEEP];
            lane.st[SWEEP] *= lane.st[SWEEP_MUL];
            if let Some(p) = fm_prev {
                f *= 1.0 + lane.st[FM_DEPTH] * p;
            }
            let mut ph = lane.st[PHASE] + f * lane.st[INV_SR];
            ph -= libm::floor(ph);
            lane.st[PHASE] = ph;
            c = libm::sin(TAU * ph);
        } else if lane.carrier != Carrier::Dc {
            let nz = xorshift(&mut lane.rng);
            let b = lane.bq;
            let mut y = b[0] * nz + lane.st[Z1];
            lane.st[Z1] = b[1] * nz - b[3] * y + lane.st[Z2];
            lane.st[Z2] = b[2] * nz - b[4] * y;
            if lane.tilt_on {
                let t = lane.tilt;
                let w = t[0] * y + lane.tz1;
                lane.tz1 = t[1] * y - t[3] * w + lane.tz2;
                lane.tz2 = t[2] * y - t[4] * w;
                y = w;
            }
            c = y;
            if lane.carrier == Carrier::NoiseTone {
                let mut ph = lane.st[PHASE] + lane.st[TONE_INC];
                ph -= libm::floor(ph);
                lane.st[PHASE] = ph;
                c = (1.0 - lane.st[TONE_MIX]) * y + lane.st[TONE_MIX] * libm::sin(TAU * ph);
            }
        }
        let mut out = env * c * lane.st[LEVEL];
        if let Some(p) = am_prev {
            out *= 1.0 - lane.st[AM_DEPTH] + lane.st[AM_DEPTH] * p;
        }
        mix.acc += out;
        mix.acc_s += out * lane.side_gain();
        mix.acc_send += out * lane.send;
        if libm::fabs(lane.s[0]) < SILENT && (lane.num_exp < 2 || libm::fabs(lane.s[1]) < SILENT) {
            lane.active = false;
            lane.st[ENV] = 0.0;
        }
        return;
    }
    // Tonal pool
    let mut out = 0.0;
    let mut side = 0.0; // per-voice pan (`voice_pan`), summed alongside `out`
    let mut uside = 0.0; // `.unison`: the detuned pairs' difference, either side
    let spread = lane.spread_on;
    let mut env_sum = 0.0;
    let mut live = false;
    let fmk = match fm_prev {
        Some(p) => 1.0 + lane.st[FM_DEPTH] * p,
        None => 1.0,
    };
    let am = lane.st[ATT_MUL];
    let mut vib = 1.0;
    if lane.vib_inc > 0.0 {
        let mut vp = lane.vib_phase + lane.vib_inc;
        vp -= libm::floor(vp);
        lane.vib_phase = vp;
        vib = 1.0 + lane.vib_depth * libm::sin(TAU * vp);
    }
    for k in 0..lane.poly {
        let sd = lane.sd[k];
        let sa = lane.sa[k];
        if libm::fabs(sd) < SILENT && libm::fabs(sa) < SILENT {
            continue;
        }
        live = true;
        let env = sd + sa;
        env_sum += env;
        lane.sd[k] = sd * if lane.hold[k] { 1.0 } else { lane.md[k] };
        lane.sa[k] = sa * am;
        if lane.is_pluck {
            let y = env * pluck_sample(lane, k, fmk * vib);
            out += y;
            if spread {
                side += y * lane.voice_pan[k];
            }
            continue;
        }
        let mut rate = lane.inc[k] * fmk * vib;
        if lane.pm_on {
            let p = lane.pm[k];
            rate *= p;
            let next = p * lane.pm_mul;
            let to = lane.pitch_to;
            lane.pm[k] = if lane.pm_mul < 1.0 {
                if next < to {
                    to
                } else {
                    next
                }
            } else if next > to {
                to
            } else {
                next
            };
            if lane.mip_on {
                // falling: step down to the richest table that still fits under Nyquist
                let hz = rate * sr;
                let mut b = lane.mip_base[k] / (WAVE_TABLE_SIZE + 1);
                while b > 0 && lane.mip_top[b - 1] >= hz * 1.06 {
                    b -= 1;
                }
                lane.mip_base[k] = b * (WAVE_TABLE_SIZE + 1);
            }
        }
        let mut osc;
        if lane.phase2.is_empty() {
            let mut ph = lane.phase[k] + rate;
            ph -= libm::floor(ph);
            lane.phase[k] = ph;
            osc = table_read(lane, k, ph);
        } else {
            let mut ph = lane.phase[k] + rate * lane.uni_down;
            ph -= libm::floor(ph);
            lane.phase[k] = ph;
            let mut ph2 = lane.phase2[k] + rate * lane.uni_up;
            ph2 -= libm::floor(ph2);
            lane.phase2[k] = ph2;
            let o1 = table_read(lane, k, ph);
            let o2 = table_read(lane, k, ph2);
            // equal power: two uncorrelated oscillators as loud as one
            osc = (o1 + o2) * EQUAL_POWER;
            if lane.uni_wide {
                uside += env * (o1 - o2) * EQUAL_POWER * lane.uni_width[k];
            }
        }
        if lane.f_amount > 0.0 {
            osc = filter_sample(lane, k, osc, rate * sr, sr);
        }
        if !lane.sub_phase.is_empty() {
            // `.subOctave`: after the filter, a clean sine an octave down
            let half = rate * 0.5;
            let mut sp = lane.sub_phase[k] + half;
            sp -= libm::floor(sp);
            lane.sub_phase[k] = sp;
            if half >= lane.sub_min_rate {
                osc += SubOctave::GAIN * libm::sin(TAU * sp);
            }
        }
        let y = env * osc;
        out += y;
        if spread {
            side += y * lane.voice_pan[k];
        }
    }
    lane.st[ENV] = env_sum;
    if !live {
        lane.active = false;
    }
    if lane.post_on {
        let b = lane.post;
        let y = b[0] * out + lane.pz1;
        lane.pz1 = b[1] * out - b[3] * y + lane.pz2;
        lane.pz2 = b[2] * out - b[4] * y;
        out = y;
    }
    if lane.distortion > 0.0 {
        out = tone_distortion(out, lane.distortion);
    }
    out *= lane.st[LEVEL];
    if spread {
        side *= lane.st[LEVEL];
    }
    if lane.uni_wide {
        let b = lane.uni_hp;
        let y = b[0] * uside + lane.uz1;
        lane.uz1 = b[1] * uside - b[3] * y + lane.uz2;
        lane.uz2 = b[2] * uside - b[4] * y;
        uside = y * lane.st[LEVEL];
    }
    if let Some(p) = am_prev {
        let m = 1.0 - lane.st[AM_DEPTH] + lane.st[AM_DEPTH] * p;
        out *= m;
        side *= m;
        uside *= m;
    }
    if lane.ducks {
        out *= mix.duck_gain;
        side *= mix.duck_gain;
        uside *= mix.duck_gain;
    }
    mix.acc_send += out * lane.send;
    mix.acc_shimmer += out * lane.shimmer_send;
    mix.acc_echo += out * lane.echo_send;
    // a spread lane has no post chain (`build_lanes`), so its voices' side sum is still in step;
    // nor has a wide unison one
    let s = if lane.pan_move.is_nan() {
        (if spread { side } else { out * lane.pan }) + uside
    } else {
        // a pan macro holds the lane: one pan for all its voices, no unison width
        out * lane.side_gain()
    };
    mix.add_tone(out, s);
}

/// One sample of voice `k`'s filter (`.subtractive`): a two-pole state-variable lowpass
/// (trapezoidal, so it stays stable as the cutoff sweeps every sample) at the note's frequency
/// x `f_base` plus the velocity-scaled envelope, which falls a step (`filterSample`).
#[inline]
fn filter_sample(lane: &mut Lane, k: usize, x: f64, hz: f64, sr: f64) -> f64 {
    let e = lane.f_env[k];
    lane.f_env[k] = e * lane.f_mul;
    let fc = (hz * lane.f_base_v[k] + lane.f_amount * lane.f_vel[k] * e).min(0.3 * sr);
    // tan(pi fc / sr), to a few percent below 0.3 sr (its Taylor series to x^5)
    let w = core::f64::consts::PI * fc * lane.st[INV_SR];
    let w2 = w * w;
    let g = w * (1.0 + w2 * (1.0 / 3.0 + w2 * (2.0 / 15.0)));
    let a1 = 1.0 / (1.0 + g * (g + lane.f_k_v[k]));
    let a2 = g * a1;
    let a3 = g * a2;
    let ic1 = lane.fz1[k];
    let ic2 = lane.fz2[k];
    let v3 = x - ic2;
    let v1 = a1 * ic1 + a2 * v3;
    let v2 = ic2 + a2 * ic1 + a3 * v3;
    lane.fz1[k] = 2.0 * v1 - ic1;
    lane.fz2[k] = 2.0 * v2 - ic2;
    v2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TempoFx {
    None,
    Hold,
    Sweep,
}

/// `WaveRunner.audition`: play `notes` (`(midi note, start s, length s)`) on one voice,
/// offline (instrument auditions, tests): mono, through the soft master curve.
pub fn audition(
    voice: &VoiceSpec,
    notes: &[(i64, f64, f64)],
    sample_rate: f64,
    seconds: f64,
    velocity: i64,
) -> Vec<f32> {
    audition_lane(
        own_lane(voice, sample_rate),
        notes,
        sample_rate,
        seconds,
        velocity,
    )
}

/// The lane `audition` plays a voice on: six voices, noise seed 1 (`tonalLane(voice, sr, 6,
/// seed: 1)`).
pub(super) fn own_lane(voice: &VoiceSpec, sample_rate: f64) -> Lane {
    let mut caches = Caches {
        tables: BTreeMap::new(),
    };
    tonal_lane(voice, sample_rate, 6, &mut caches, 1)
}

/// `audition` on a lane already built (and shaped, as `build_lanes` shapes it).
pub(super) fn audition_lane(
    lane: Lane,
    notes: &[(i64, f64, f64)],
    sample_rate: f64,
    seconds: f64,
    velocity: i64,
) -> Vec<f32> {
    let mut lanes = vec![lane];
    let starts: Vec<i64> = notes
        .iter()
        .map(|n| js_round(n.1 * sample_rate) as i64)
        .collect();
    let w = velocity.clamp(1, 127) as f64 / 127.0;
    let gain = WaveOptions::default().master_gain;
    let mut out = vec![0f32; (seconds * sample_rate) as usize];
    for (t, slot) in out.iter_mut().enumerate() {
        for (j, n) in notes.iter().enumerate() {
            if starts[j] == t as i64 {
                trigger(&mut lanes[0], sample_rate, n.0, w, n.2 * 1000.0, -1.0);
            }
        }
        let mut mix = Mix::new(1.0, false);
        if lanes[0].active {
            lane_sample(&mut lanes, 0, sample_rate, &mut mix);
        }
        *slot = libm::tanh(mix.acc * gain) as f32;
    }
    out
}

/// `WaveRunner`
/// Downbeats a [`WaveRunner`] keeps (`downbeats`).
pub const DOWNBEAT_RING: usize = 8;

pub struct WaveRunner {
    pub sample_rate: f64,
    pub opts: WaveOptions,
    pub master_gain: f64,
    master: Master,
    current: Option<WaveProject>,
    pending: Option<WaveProject>,
    /// The previous project's voices, ringing out after a swap.
    fading: Option<WaveProject>,
    playing: bool,
    clock: [f64; 4],
    ticks_in_block: usize,
    /// Set when a stop-transport control fires.
    stopped: bool,
    /// Report fires and tick anchors into `fires` / `anchors` (off by default; they grow, so this
    /// is for offline renders: a live deck uses the rings below).
    pub record_events: bool,
    pub fires: Vec<FireEvent>,
    pub anchors: Vec<TickAnchor>,
    /// Where audible net fires are reported, or None (the default: offline renders and sessions
    /// play exactly as before): an overwrite ring the audio thread writes without allocating
    /// (`fireRing`). Set once, before the first `process`, by the owner; the runner is the only
    /// producer. Never written by silent replays or scrubs, only by the tick clock.
    pub fire_ring: Option<Arc<LatestRing<FireEvent>>>,
    /// Where every tick of the net clock is reported, or None. Same rules as `fire_ring`
    /// (`anchorRing`).
    pub anchor_ring: Option<Arc<LatestRing<TickAnchor>>>,
    /// The latest downbeats played (`downbeats_written` of them so far, the last
    /// [`DOWNBEAT_RING`] kept): (deck frame, beat). Always on, allocation free: what the host's
    /// LOOP record and loop clock line up with (`DJEngine.downbeats` over the anchors).
    pub downbeats: [(i64, i64); DOWNBEAT_RING],
    pub downbeats_written: u64,
    last_anchor_tick: i32,
    last_anchor_generation: i32,
    /// The tick clock was started, stopped or realigned since the last anchor.
    clock_broken: bool,
    tick_frame: i64,
    block_start: i64,
    tempo_fx: TempoFx,
    fx_start_sample: f64,
    fx_dur_samples: f64,
    fx_start_bpm: f64,
    fx_target: f64,
    tempo_fx_mask: u32,
    macro_hold_ticks: i64,
    // Groove: notes held back by swing / humanize / the template, due at a sample time.
    groove_on: bool,
    held: Vec<Held>,
    groove_next: f64,
    /// Sample index within the block being rendered (0 between blocks).
    block_offset: i64,
    record_hits: bool,
    /// Every onset played, with `WaveOptions::record_hits` (`onHit`).
    pub hits: Vec<HitRecord>,
    /// The notes sampled lanes asked their slots for since the host last drained this
    /// (`std::mem::take`); see [`SampleBinder`].
    pub sample_notes: Vec<SampleNote>,
    /// The live keys played on a sampled voice, as raw MIDI for the host's sampler, in the
    /// order they were pressed (`queue`). Empty while the live voice is a synth voice. Drain it
    /// with `clear()` or `drain(..)` after reading: those keep its capacity, which is what the
    /// audio thread pushes into (a `std::mem::take` leaves none, and the keys are then dropped).
    pub sample_keys: Vec<SampleKey>,
    /// Lanes whose held sampler notes the host is to end now ([`LaneEnd`]: a sampled lane that
    /// goes to its synth voice). Fixed capacity, drained like `sample_keys`.
    pub lane_ends: Vec<LaneEnd>,
    /// Projects the runner let go of (a queued project replaced by a newer one, a faded-out
    /// one, a replaced fade), kept here so the host frees them off the audio thread
    /// (`WaveRunner.onRetire` in Swift). Preallocated; drain with [`WaveRunner::pop_retired`].
    retired: Vec<WaveProject>,
    // Scratching the net clock (a hand on the platter).
    scrubbing: bool,
    scrub_pos: f64,
    scrub_target: f64,
    scrub_rate: f64,
    scrub_chase: f64,
    scrub_smooth: f64,
    // Room (`.space`): each sample's send (the lanes' `send` share) into the reverb, whose
    // return joins the mix. Sampled instruments render a block at a time, after the samples,
    // in the host: their send waits in `send_fifo` ([`WaveRunner::queue_sampler_send`]) and
    // goes in during the next block (a block's delay, inside the room's pre-delay).
    reverb: Option<FdnReverb>,
    /// `.shimmer`: the synth pads' send through [`Shimmer`], an octave up, into the room's input.
    shimmer: Option<Shimmer>,
    /// `SpaceSpec::sampler_send`, read at construction.
    sampler_send: f64,
    send_fifo: Vec<f32>,
    fifo_read: usize,
    fifo_count: usize,
    // Echo (`.echo`): each sample's echo send; the repeats join the mix and go on into the
    // room. Sampled leads send through `echo_fifo` the same way.
    echo_fx: Option<PingPong>,
    echo_fifo: Vec<f32>,
    echo_read: usize,
    echo_count: usize,
    // Kick ducking (`.duck`): a kick sets `duck_hold` to 1, which falls back exponentially;
    // `duck_env` follows it with a 2 ms attack (no click); bass and pad voices are scaled by
    // `duck_gain` = 1 - depth x `duck_env`. `duck_buf` keeps each sample's gain of the block
    // for the sampler slots, which the host renders once a block.
    duck_hold: f64,
    duck_env: f64,
    duck_gain: f64,
    duck_buf: Vec<f32>,
    // Section energy (`.energy`): the tonal voices sum apart (`tone_l`) so their lowpass leaves
    // the drums (`pre_l`) alone; level and cutoff follow the project's `EnergyCurve`, smoothed
    // once a block.
    energy_on: bool,
    /// `.sweep`: notes struck in a buildup take its progress (`sweepOn`).
    sweep_on: bool,
    energy_primed: bool,
    energy_gain: f64,
    energy_cut: f64,
    tone_lp: (SweepFilter, SweepFilter),
    pre_l: Vec<f32>,
    tone_l: Vec<f32>,
    // The sampled instruments in the master curve (`samplersInCurve`): with `.loudness` and a host
    // that renders them (`process_with`), the slots render into `smp_l` / `smp_r` and join the
    // voices before the curve, scaled by the track gain alone; `sampler_side` keeps their lows
    // mono under `.stereo` (`samplerSide`, 150 Hz).
    samplers_in_curve: bool,
    smp_l: Vec<f32>,
    smp_r: Vec<f32>,
    sampler_side: SideHighpass,
    // Stereo (`.stereo`): each sample's side signal beside the mid one (`acc`, `acc_tone`):
    // left = mid + side, right = mid - side. Off, the side stays 0 and is never read.
    stereo_on: bool,
    pre_s: Vec<f32>,
    tone_s: Vec<f32>,
    /// `.riser`: the song step the current project is playing (its riser's place), or
    /// `i64::MIN` for none (stopped, or the tick a queued project took over on, which plays
    /// nothing).
    riser_step: i64,
}

/// The longest block the per-block buffers of the runner hold (Swift's 4096): a longer one
/// plays without the fixes that need them.
pub const MAX_BLOCK: usize = 4096;

impl WaveRunner {
    /// Every project the runner holds (the one playing, the one waiting to swap in, the one
    /// ringing out), taken out: the runner is left empty, as before its first swap. For a host
    /// that frees a runner it is done with a piece at a time ([`WaveProject::take_lanes`]).
    pub fn take_projects(&mut self) -> Vec<WaveProject> {
        [self.current.take(), self.pending.take(), self.fading.take()].into_iter().flatten().collect()
    }

    pub fn new(sample_rate: f64, opts: WaveOptions) -> WaveRunner {
        let opts_fixes = opts.musicality;
        let (opts_groove, opts_record) = (
            opts.groove || opts_fixes.contains(Fixes::GROOVE),
            opts.record_hits,
        );
        let space = opts_fixes.contains(Fixes::SPACE);
        let mut r = WaveRunner {
            sample_rate,
            master_gain: opts.master_gain,
            master: opts.master,
            opts,
            current: None,
            pending: None,
            fading: None,
            playing: false,
            clock: [0.0; 4],
            ticks_in_block: 0,
            stopped: false,
            record_events: false,
            fires: Vec::new(),
            anchors: Vec::new(),
            fire_ring: None,
            anchor_ring: None,
            downbeats: [(0, 0); DOWNBEAT_RING],
            downbeats_written: 0,
            last_anchor_tick: -1,
            last_anchor_generation: 0,
            clock_broken: true,
            tick_frame: 0,
            block_start: 0,
            tempo_fx: TempoFx::None,
            fx_start_sample: 0.0,
            fx_dur_samples: 0.0,
            fx_start_bpm: 120.0,
            fx_target: 120.0,
            tempo_fx_mask: 0,
            macro_hold_ticks: 0,
            groove_on: opts_groove,
            held: Vec::with_capacity(GROOVE_CAPACITY),
            groove_next: f64::INFINITY,
            block_offset: 0,
            record_hits: opts_record,
            hits: Vec::new(),
            sample_notes: Vec::with_capacity(SAMPLE_NOTE_CAPACITY),
            retired: Vec::with_capacity(RETIRED_CAPACITY),
            scrubbing: false,
            scrub_pos: 0.0,
            scrub_target: 0.0,
            scrub_rate: 0.0,
            scrub_chase: 0.035 * sample_rate,
            scrub_smooth: 1.0 - libm::exp(-1.0 / (0.004 * sample_rate)),
            sample_keys: Vec::with_capacity(SAMPLE_KEY_CAPACITY),
            lane_ends: Vec::with_capacity(LANE_END_CAPACITY),
            reverb: space.then(|| FdnReverb::new(sample_rate)),
            shimmer: (space && opts_fixes.contains(Fixes::SHIMMER))
                .then(|| Shimmer::new(sample_rate)),
            sampler_send: SpaceSpec::sampler_send(),
            send_fifo: vec![0.0; FIFO_CAPACITY],
            fifo_read: 0,
            fifo_count: 0,
            echo_fx: opts_fixes
                .contains(Fixes::ECHO)
                .then(|| PingPong::new(sample_rate)),
            echo_fifo: vec![0.0; FIFO_CAPACITY],
            echo_read: 0,
            echo_count: 0,
            duck_hold: 0.0,
            duck_env: 0.0,
            duck_gain: 1.0,
            duck_buf: vec![1.0; MAX_BLOCK],
            energy_on: opts_fixes.contains(Fixes::ENERGY),
            sweep_on: opts_fixes.contains(Fixes::SWEEP),
            energy_primed: false,
            energy_gain: 1.0,
            energy_cut: EnergyCurve::OPEN_HZ,
            tone_lp: (SweepFilter::default(), SweepFilter::default()),
            pre_l: vec![0.0; MAX_BLOCK],
            tone_l: vec![0.0; MAX_BLOCK],
            samplers_in_curve: opts_fixes.contains(Fixes::LOUDNESS),
            smp_l: vec![0.0; MAX_BLOCK],
            smp_r: vec![0.0; MAX_BLOCK],
            sampler_side: SideHighpass::new(150.0, sample_rate),
            stereo_on: opts_fixes.contains(Fixes::STEREO),
            pre_s: vec![0.0; MAX_BLOCK],
            tone_s: vec![0.0; MAX_BLOCK],
            riser_step: i64::MIN,
        };
        r.clock[TEMPO] = 120.0;
        r.clock[SPT] = r.spt(120.0);
        r
    }

    /// Hand a project the runner is done with to the retire list (never dropped on the
    /// audio thread when a host drains [`pop_retired`](Self::pop_retired) every block). A
    /// full list just drops it, as Swift's full ring would.
    fn retire(&mut self, p: WaveProject) {
        if self.retired.len() < self.retired.capacity() {
            self.retired.push(p);
        }
    }

    /// The next project the runner has let go of, for the host to free (or keep) elsewhere.
    pub fn pop_retired(&mut self) -> Option<WaveProject> {
        self.retired.pop()
    }

    pub fn tempo(&self) -> f64 {
        self.clock[TEMPO]
    }
    pub fn samples(&self) -> f64 {
        self.clock[SAMPLES]
    }
    pub fn playing(&self) -> bool {
        self.playing
    }
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn ticks_in_block(&self) -> usize {
        self.ticks_in_block
    }
    pub fn graph(&self) -> Option<&WaveGraph> {
        self.current.as_ref().map(|p| &p.graph)
    }
    pub fn graph_mut(&mut self) -> Option<&mut WaveGraph> {
        self.current.as_mut().map(|p| &mut p.graph)
    }
    pub fn project(&self) -> Option<&WaveProject> {
        self.current.as_ref()
    }
    pub fn project_mut(&mut self) -> Option<&mut WaveProject> {
        self.current.as_mut()
    }

    /// A channel's voice settings (the sound panel), on the project playing and the one
    /// queued for the next bar, from the next sample on (`setVoice`).
    ///
    /// A sampled lane that goes to its synth voice has the notes it holds ended now, and only
    /// those (`applyVoice`: a note-off for each of the lane's own pending notes, then the queue
    /// cleared): the host is asked ([`LaneEnd`]) to end the lane's notes; a live key on the same
    /// slot, and the notes of another lane that shares it, play on. Allocation-free.
    pub fn set_voice(&mut self, p: &VoiceLaneParams) {
        use super::voice_tweak::VoiceLaneParams as P;
        let frame = self.block_start + self.block_offset;
        for proj in [self.current.as_mut(), self.pending.as_mut()]
            .into_iter()
            .flatten()
        {
            for lane in proj.lanes.iter_mut() {
                if lane.channel == p.channel as i64 {
                    if lane.sample_slot >= 0 && p.has(P::SYNTH) && p.use_synth {
                        // (a kit lane's hold, the runner's own record of its last hit)
                        lane.kit_held_until = f64::NEG_INFINITY;
                        if self.lane_ends.len() < self.lane_ends.capacity() {
                            self.lane_ends.push(LaneEnd {
                                lane: lane.uid,
                                frame,
                            });
                        }
                    }
                    lane.set_voice(p);
                }
            }
        }
    }

    /// `WaveProject.generation` of the project the clock is running (0: none).
    pub fn generation(&self) -> i32 {
        self.current.as_ref().map_or(0, |p| p.generation)
    }
    /// Generation of the project queued behind it, waiting for the next bar (0: none).
    pub fn pending_generation(&self) -> i32 {
        self.pending.as_ref().map_or(0, |p| p.generation)
    }

    /// Publish the running project's marking to its mirror (no-op when unchanged), as the state
    /// at the end of the block that ends at deck frame `end_frame` (`publishMarking(endFrame:)`).
    /// Allocation-free.
    pub fn publish_marking(&self, end_frame: i64) {
        if let Some(p) = &self.current {
            p.marking.publish(&p.graph, end_frame);
        }
    }
    /// Fraction (0..<1) of the current tick that has elapsed.
    pub fn tick_phase(&self) -> f64 {
        if self.clock[SPT] > 0.0 {
            self.clock[TICK_POS] / self.clock[SPT]
        } else {
            0.0
        }
    }

    fn spt(&self, bpm: f64) -> f64 {
        self.sample_rate * 60.0 / (bpm * PPQ)
    }

    /// Compile everything off the audio thread. Hand the result to `load_prepared`.
    /// `start_tick` replays the net silently to that tick first (a section launch).
    pub fn prepare(
        project: &Project,
        sample_rate: f64,
        opts: &WaveOptions,
        start_tick: i64,
    ) -> WaveProject {
        Self::prepare_serial(project, sample_rate, opts, start_tick, None)
    }

    /// [`prepare`](Self::prepare) for another start of a song that was prepared before:
    /// `track_serial` is that project's (`WaveProject.trackSerial`); `None` is a new track.
    pub fn prepare_serial(
        project: &Project,
        sample_rate: f64,
        opts: &WaveOptions,
        start_tick: i64,
        track_serial: Option<i32>,
    ) -> WaveProject {
        let mut g = match &opts.role_filter {
            Some(f) => WaveGraph::new(&f.apply(project)),
            None => WaveGraph::new(project),
        };
        if start_tick > 0 {
            g.fast_forward(start_tick);
        }
        // `.balance` measures the parts' trims off the project (the hats untilted; the voice
        // fixes heard) unless another start of the song brings them; the rest schedule the
        // gestures give (`.dropGap`, `.layerBudget`, `.bandStop`, `.kickOut`) is built unless
        // another start brings that too
        let measured;
        let opts = {
            let balance = opts.musicality.contains(Fixes::BALANCE) && opts.track_balance.is_none();
            let rests = opts.rests.is_none()
                && !(opts.musicality
                    & (Fixes::DROP_GAP | Fixes::LAYER_BUDGET | Fixes::BAND_STOP | Fixes::KICK_OUT))
                    .is_empty();
            if balance || rests {
                let mut o = opts.clone();
                if balance {
                    o.track_balance = Some(MixBalance::trims(
                        project,
                        opts.musicality,
                        MixBalance::family(opts.genre.as_deref()),
                        None,
                    ));
                }
                if rests {
                    o.rests = Some(build_rests(project, opts.musicality, opts.genre.as_deref()));
                }
                measured = o;
                &measured
            } else {
                opts
            }
        };
        // `.sectionHarmony` / `.harmonicRhythm`: the band moves with the section and its chords
        // (a pure function of the played project, cheap: rebuilt on section jumps rather than
        // carried)
        let harmony = SectionHarmony::playing(project, opts.musicality);
        let mut prepared = build_lanes(
            g,
            sample_rate,
            opts,
            start_tick.max(0),
            track_serial,
            harmony,
            Some(project),
        );
        prepared.gain = opts.track_gain.unwrap_or(1.0);
        // (`.sweep` reads the curve's ramps too; the level and lowpass stay `.energy`'s)
        if opts.musicality.contains(Fixes::ENERGY) || opts.musicality.contains(Fixes::SWEEP) {
            prepared.energy = opts
                .track_energy
                .clone()
                .or_else(|| EnergyCurve::new(project));
        }
        if opts.musicality.contains(Fixes::RISER) {
            prepared.riser = RiserVoice::new(
                &riser_windows(project),
                sample_rate,
                opts.musicality.contains(Fixes::STEREO),
                opts.musicality.contains(Fixes::SPACE),
            );
        }
        // `.loudness`: measured after the lanes are built, so the probe knows which parts the
        // deck plays on samplers (each sits at its sampler's level, not its synth voice's)
        if opts.track_gain.is_none() && opts.musicality.contains(Fixes::LOUDNESS) {
            // `.energy` plays the probed section at its own gain (the whole mix times it,
            // `finish_energy`): the probe hears it too, or a song whose busiest section is not
            // its most energetic one (lofi's verse, 4 dB under its bridge) lands that much under
            // the reference
            let probed = Loudness::busiest_start(project);
            let section_gain = if opts.musicality.contains(Fixes::ENERGY) {
                prepared.energy.as_ref().map_or(1.0, |e| e.at(probed).0)
            } else {
                1.0
            };
            let balance = if opts.musicality.contains(Fixes::BALANCE) {
                opts.track_balance.as_ref()
            } else {
                None
            };
            prepared.gain = Loudness::gain(
                project,
                opts.musicality,
                balance,
                &prepared.probe_levels,
                opts.sub_octave,
                opts.hf_tilt,
                opts.genre.as_deref(),
                section_gain,
            );
        }
        prepared
    }

    /// `prepare(json:)`
    pub fn prepare_json(json: &JsonValue, sample_rate: f64, opts: &WaveOptions) -> WaveProject {
        build_lanes(
            WaveGraph::from_json(json),
            sample_rate,
            opts,
            0,
            None,
            None,
            None,
        )
    }

    /// Compile a project. Applied now when stopped, at the next bar when playing.
    pub fn load(&mut self, project: &Project) {
        let p = Self::prepare(project, self.sample_rate, &self.opts, 0);
        self.load_prepared(p);
    }

    pub fn load_json(&mut self, json: &JsonValue) {
        let p = Self::prepare_json(json, self.sample_rate, &self.opts);
        self.load_prepared(p);
    }

    /// Install a prepared project: immediately when stopped, at the next bar when playing.
    pub fn load_prepared(&mut self, next: WaveProject) {
        if self.playing && self.current.is_some() {
            if let Some(old) = self.pending.replace(next) {
                self.retire(old);
            }
            return;
        }
        self.install(next);
    }

    fn install(&mut self, next: WaveProject) {
        self.flush_groove();
        self.energy_primed = false;
        // Pointer swap: the old project's lanes ring out.
        if let Some(old) = self.current.take() {
            if let Some(gone) = self.fading.replace(old) {
                self.retire(gone);
            }
        }
        let tempo = next.graph.tempo;
        let mut next = next;
        next.rest_cursor.reset();
        if let Some(r) = next.riser.as_mut() {
            r.reset();
        }
        self.riser_step = i64::MIN;
        if let Some(r) = self.reverb.as_mut() {
            r.tune(next.space.as_ref());
        }
        if let Some(sh) = self.shimmer.as_mut() {
            sh.tune(next.space.as_ref());
        }
        if let Some(e) = self.echo_fx.as_mut() {
            e.tune(next.echo, tempo);
        }
        self.current = Some(next);
        self.set_tempo(tempo);
    }

    pub fn play(&mut self) {
        if self.current.is_none() || self.playing {
            return;
        }
        self.playing = true;
        self.stopped = false;
        self.clock_broken = true;
        self.clock[TICK_POS] = self.clock[SPT] - 1.0; // first tick lands on the next sample
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn stop(&mut self) {
        self.held.clear();
        self.groove_next = f64::INFINITY;
        self.playing = false;
        self.clock_broken = true;
        self.macro_hold_ticks = 0;
        if let Some(p) = self.current.as_mut() {
            p.graph.reset();
            p.rest_cursor.reset();
            if let Some(r) = p.riser.as_mut() {
                r.reset();
            }
        }
        self.riser_step = i64::MIN;
        self.energy_primed = false;
        self.tone_lp.0.reset();
        self.tone_lp.1.reset();
        self.duck_hold = 0.0;
        self.duck_env = 0.0;
        // (the sampled instruments' keys are the host's: `samplersAllOff` is CC 123 on every
        // slot and the end of the lanes' pending note-offs, which `DeckCore` does on a stop
        // and, for the three stops the runner makes itself, when `stopped()` goes up. The
        // slots' own side highpasses are the host's, too.)
        self.sampler_side.reset();
    }

    /// Does a pan macro hold any lane right now?
    fn pan_moving(&self) -> bool {
        self.current
            .iter()
            .chain(self.fading.iter())
            .any(|p| p.lanes.iter().any(|l| !l.pan_move.is_nan()))
    }

    fn lane_selected(l: &Lane, which: MacroTargets) -> bool {
        let drum = is_drum_channel(l.channel);
        match which {
            MacroTargets::NonDrums => !drum,
            MacroTargets::DrumsOnly => drum,
            MacroTargets::Everything => true,
            MacroTargets::NonKick => !l.is_kick,
        }
    }

    /// Hold every part `which` picks at `pan` (-1 left ... +1 right), or let go of them (None).
    /// Beats' `pan-move` macros send the channel's pan every ~80 ms; a host calls this each
    /// block with the pattern's value. Projects loaded later take it up on the next call.
    pub fn set_pan_move(&mut self, which: MacroTargets, pan: Option<f64>) {
        let v = pan.map_or(f64::NAN, |p| if p.is_finite() { p.clamp(-1.0, 1.0) } else { 0.0 });
        for p in [self.current.as_mut(), self.fading.as_mut()].into_iter().flatten() {
            for l in p.lanes.iter_mut() {
                if Self::lane_selected(l, which) {
                    l.pan_move = v;
                }
            }
        }
    }

    /// Scale the decay of the notes every part `which` picks plays from now on (beats'
    /// `decay-move`, `setChannelDecay`), or let go (None): the notes already sounding keep theirs.
    pub fn set_decay_move(&mut self, which: MacroTargets, decay: Option<f64>) {
        let v = decay.map_or(1.0, |d| if d.is_finite() { d.clamp(0.05, 8.0) } else { 1.0 });
        for p in [self.current.as_mut(), self.fading.as_mut()].into_iter().flatten() {
            for l in p.lanes.iter_mut() {
                if Self::lane_selected(l, which) {
                    l.decay_move = v;
                }
            }
        }
    }

    /// Every tail the runner holds cleared: the synth voices of the current and fading
    /// projects, the room and the shimmer and what waits for them in the sampled sends' fifos
    /// (`silence`). `stop` alone lets the tails ring out, as a deck's room does. The echo's own
    /// line is not cleared (`PingPong` has no reset: its repeats ring on).
    pub fn silence(&mut self) {
        for p in [self.current.as_mut(), self.fading.as_mut()]
            .into_iter()
            .flatten()
        {
            for l in p.lanes.iter_mut() {
                l.silence();
            }
        }
        if let Some(r) = self.current.as_mut().and_then(|p| p.riser.as_mut()) {
            r.reset();
        }
        if let Some(r) = self.reverb.as_mut() {
            r.reset();
        }
        if let Some(sh) = self.shimmer.as_mut() {
            sh.reset();
        }
        self.fifo_count = 0;
        self.echo_count = 0;
        self.duck_hold = 0.0;
        self.duck_env = 0.0;
    }

    /// The gain the bass and pad played at on each frame of the block just rendered
    /// (`duckBuf`; 1 = not ducked): what the host scales the sampler slots in
    /// [`WaveProject::duck_slots`] by. Only the first `frames` of the block are meaningful, and
    /// only when the project has a duck depth.
    pub fn duck_gains(&self, frames: usize) -> &[f32] {
        &self.duck_buf[..frames.min(self.duck_buf.len())]
    }

    /// The FX unit's sends: the room's and the echo's input gains and the echo's feedback
    /// boost, reached over the next `frames` samples (`setUnitSends`). Without `.space` /
    /// `.echo` (the parity path) there is nothing to send to. Allocation-free.
    pub fn set_unit_sends(&mut self, room: f64, echo: f64, feedback: f64, frames: usize) {
        if let Some(r) = self.reverb.as_mut() {
            r.set_send(room, frames);
        }
        if let Some(e) = self.echo_fx.as_mut() {
            e.set_send(echo, feedback, frames);
        }
    }

    /// The sampled instruments' send of the block just rendered (`queueSamplerSend`): `send`
    /// holds each frame's mono mix of the slots that send (`WaveProject::send_slots`, after
    /// their gain and duck). It goes into the room during the next block, lifted by the
    /// master gain: the samplers join the voices at the track gain alone, the voices go
    /// through the master gain as well. For the host that renders the sampled slots.
    pub fn queue_sampler_send(&mut self, send: &[f32]) {
        let level = (self.sampler_send / self.master_gain.max(0.05)) as f32;
        for s in send {
            if self.fifo_count >= self.send_fifo.len() {
                break;
            }
            let mut w = self.fifo_read + self.fifo_count;
            if w >= self.send_fifo.len() {
                w -= self.send_fifo.len();
            }
            self.send_fifo[w] = *s * level;
            self.fifo_count += 1;
        }
    }

    /// The sampled leads' echo send of the block just rendered (`queueSamplerEcho`), for the
    /// echo to take during the next block, lifted by the master gain as
    /// [`queue_sampler_send`](Self::queue_sampler_send) is.
    pub fn queue_sampler_echo(&mut self, send: &[f32]) {
        let level = (1.0 / self.master_gain.max(0.05)) as f32;
        for s in send {
            if self.echo_count >= self.echo_fifo.len() {
                break;
            }
            let mut w = self.echo_read + self.echo_count;
            if w >= self.echo_fifo.len() {
                w -= self.echo_fifo.len();
            }
            self.echo_fifo[w] = *s * level;
            self.echo_count += 1;
        }
    }

    pub fn set_tempo(&mut self, bpm: f64) {
        if !(bpm > 0.0) {
            return;
        }
        let frac = self.clock[TICK_POS] / self.clock[SPT];
        self.clock[TEMPO] = bpm;
        if let Some(p) = self.current.as_mut() {
            p.graph.tempo = bpm;
        }
        if let Some(e) = self.echo_fx.as_mut() {
            e.set_tempo(bpm);
        }
        self.clock[SPT] = self.spt(bpm);
        self.clock[TICK_POS] = frac * self.clock[SPT];
    }

    /// Mute by `strHash(netId)` (what the audio-thread facade uses).
    pub fn set_mute_hash(&mut self, hash: i32, muted: bool) {
        if let Some(g) = self.graph_mut() {
            if let Some(&i) = g.by_hash.get(&hash) {
                g.muted[i] = muted as u8;
            }
        }
    }

    pub fn set_mute(&mut self, net_id: &str, muted: bool) {
        if let Some(g) = self.graph_mut() {
            if let Some(&i) = g.by_id.get(net_id) {
                g.muted[i] = muted as u8;
            }
        }
    }

    pub fn set_role_muted(&mut self, hash: i32, muted: bool) {
        if let Some(g) = self.graph_mut() {
            g.set_role_muted(hash, muted);
        }
    }

    pub fn activate_slot(&mut self, hash: i32) {
        if let Some(g) = self.graph_mut() {
            g.activate_slot(hash);
        }
    }

    pub fn fire_stinger(&mut self, hash: i32, ticks: i64) {
        if let Some(g) = self.graph_mut() {
            g.fire_stinger(hash, ticks);
        }
        self.mark_macro_running(ticks);
    }

    // MARK: beat alignment

    /// Move this deck's beat grid by `ticks` ticks (a tick is a sixteenth). Positive moves
    /// forward in the song: whole ticks are replayed silently, the remainder shifts the
    /// tick boundary. Negative holds the clock back (the next tick comes later).
    pub fn align(&mut self, ticks: f64) {
        if self.current.is_none() {
            return;
        }
        self.clock_broken = true;
        if ticks < 0.0 {
            self.clock[TICK_POS] += ticks * self.clock[SPT];
            return;
        }
        let whole = libm::floor(ticks);
        let mut n = 0.0;
        if let Some(p) = self.current.as_mut() {
            let tail = p.tail.is_some();
            while n < whole && !p.graph.stop_requested {
                p.graph.advance();
                if tail {
                    // a mix-out tail aligns on past the song's end (`songEnds` takes the
                    // stop-transport back; the tail stops the deck on its own bar line)
                    p.graph.stop_requested = false;
                }
                n += 1.0;
            }
        }
        self.clock[TICK_POS] += (ticks - whole) * self.clock[SPT];
    }

    /// Put the tick clock `ticks` ticks further on, silently, as if it had kept running at the
    /// tempo all along (a turntable brake's release): `WaveRunner.catchUp`. Unlike
    /// [`align`](Self::align), the remainder lands exactly: a boundary it crosses is ticked
    /// now, not on the next sample. A queued project takes over on its bar, the macro hold
    /// counts down, the end of the song stops the deck.
    pub fn catch_up(&mut self, ticks: f64) {
        if !(ticks > 0.0) || self.current.is_none() || !(self.clock[SPT] > 0.0) {
            return;
        }
        self.flush_groove();
        self.clock_broken = true;
        let mut pos = self.clock[TICK_POS] / self.clock[SPT] + ticks;
        while pos >= 1.0 {
            let Some(mut proj) = self.current.take() else {
                break;
            };
            pos -= 1.0;
            if self.pending.is_some() && (proj.graph.tick + 1) % 16 == 0 {
                let next = self.pending.take().unwrap();
                self.current = Some(proj);
                self.install(next);
                continue;
            }
            if proj.tail.is_some_and(|t| proj.graph.tick >= t.end_tick()) {
                self.current = Some(proj);
                self.stopped = true;
                self.stop();
                return;
            }
            proj.graph.advance();
            if self.macro_hold_ticks > 0 {
                self.macro_hold_ticks -= 1;
            }
            let mut ends = proj.graph.stop_requested;
            if ends && proj.tail.is_some() {
                proj.graph.stop_requested = false;
                ends = false;
            }
            self.current = Some(proj);
            if ends {
                self.stopped = true;
                self.stop();
                return;
            }
        }
        self.clock[TICK_POS] = pos.max(0.0) * self.clock[SPT];
    }

    /// The tick the playing project's mix-out tail stops on (`tailEndTick`), if it has one.
    pub fn tail_end_tick(&self) -> Option<i64> {
        self.current
            .as_ref()
            .and_then(|p| p.tail)
            .map(|t| t.end_tick())
    }

    // MARK: macros

    fn begin_tempo_fx(&mut self, kind: TempoFx, target: f64, ticks: i64, m: NetMacro) {
        if self.tempo_fx == TempoFx::None {
            self.tempo_fx_mask = 0;
        }
        self.tempo_fx_mask |= m.bit();
        // A stacked fire restores to the tempo the first one started from, not its own peak.
        let start = if self.tempo_fx == TempoFx::None {
            self.clock[TEMPO]
        } else {
            self.fx_start_bpm
        };
        self.tempo_fx = kind;
        self.fx_start_bpm = start;
        self.fx_target = target;
        self.fx_start_sample = self.clock[SAMPLES];
        self.fx_dur_samples = ticks.max(1) as f64 * self.clock[SPT];
        if kind == TempoFx::Hold {
            self.set_tempo(target);
        }
    }

    fn service_tempo_fx(&mut self) {
        if self.tempo_fx == TempoFx::None {
            return;
        }
        let elapsed = self.clock[SAMPLES] - self.fx_start_sample;
        if elapsed >= self.fx_dur_samples {
            self.tempo_fx = TempoFx::None;
            self.set_tempo(self.fx_start_bpm);
        } else if self.tempo_fx == TempoFx::Sweep {
            let t = if elapsed / self.fx_dur_samples < 1.0 {
                elapsed / self.fx_dur_samples
            } else {
                1.0
            };
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            self.set_tempo(self.fx_start_bpm + (self.fx_target - self.fx_start_bpm) * eased);
        }
    }

    /// beats `markMacroRunning` (macros/runtime.js): a macro of ANY kind is running for
    /// `ticks` net ticks from its fire, whether or not it changed anything we can see.
    pub fn mark_macro_running(&mut self, ticks: i64) {
        self.macro_hold_ticks = self.macro_hold_ticks.max(ticks.max(1));
    }

    /// Fire a net macro (`catalog.js`): mutes restore after `ticks` net ticks; tempo macros
    /// last `ticks` ticks of wall time at the tempo they were fired at.
    pub fn fire_macro(&mut self, m: NetMacro, ticks: i64) {
        if self.current.is_none() {
            return;
        }
        self.mark_macro_running(ticks);
        let tempo = self.clock[TEMPO];
        let proj_tempo = self.current.as_ref().unwrap().graph.project.tempo;
        let tag = m.tag();
        match m {
            NetMacro::Drop | NetMacro::SoloDrums => {
                self.graph_mut()
                    .unwrap()
                    .fire_mute(MacroTargets::NonDrums, ticks, tag);
            }
            NetMacro::Breakdown => {
                self.graph_mut()
                    .unwrap()
                    .fire_mute(MacroTargets::DrumsOnly, ticks, tag);
            }
            NetMacro::Cut => {
                self.graph_mut()
                    .unwrap()
                    .fire_mute(MacroTargets::Everything, ticks, tag);
            }
            NetMacro::DoubleDrop => {
                let g = self.graph_mut().unwrap();
                g.fire_mute(MacroTargets::Everything, 2, tag);
                // the second half lands 260 ms later (a timer on the net clock, in ticks)
                let delay = (libm::floor(0.26 / (60.0 / (tempo * PPQ)) + 0.5) as i64).max(1);
                g.schedule_mute(MacroTargets::NonDrums, delay, ticks, tag);
            }
            NetMacro::BeatRepeat => self.graph_mut().unwrap().fire_beat_repeat(ticks, 2, 1),
            NetMacro::HalfTime => {
                let target = js_round(tempo * 0.5).max(20.0);
                self.begin_tempo_fx(TempoFx::Hold, target, ticks, m);
            }
            NetMacro::TapeStop => self.begin_tempo_fx(TempoFx::Sweep, 22.0, ticks, m),
            NetMacro::TempoAnchor => {
                self.begin_tempo_fx(TempoFx::Hold, proj_tempo.max(20.0), ticks, m)
            }
        }
    }

    /// Is a macro in effect right now: one fired and not yet run out, a pending mute
    /// restore, a running beat repeat or a tempo effect?
    pub fn macro_active(&self) -> bool {
        self.macro_hold_ticks > 0
            || self.graph().is_some_and(|g| g.has_active_macros())
            || self.tempo_fx != TempoFx::None
    }

    /// Which macros are in effect and which nets they hold muted (`WaveGraph::macro_snapshot`),
    /// tempo macros included (`macroSnapshot`). Allocation-free.
    pub fn macro_snapshot(&self) -> super::macro_mirror::MacroSnapshot {
        let Some(g) = self.graph() else {
            return Default::default();
        };
        let mut s = if g.has_active_macros() {
            g.macro_snapshot()
        } else {
            Default::default()
        };
        if self.tempo_fx != TempoFx::None {
            s.mask |= self.tempo_fx_mask;
        }
        s
    }

    /// Restore every pending macro effect now.
    pub fn panic(&mut self) {
        if let Some(g) = self.graph_mut() {
            g.cancel_macros();
        }
        self.macro_hold_ticks = 0;
        if self.tempo_fx != TempoFx::None {
            self.tempo_fx = TempoFx::None;
            self.set_tempo(self.fx_start_bpm);
        }
    }

    // MARK: live keys

    /// A raw MIDI message for a sampler slot, at this sample of the block (`queue`). Fixed
    /// capacity: a full queue drops it (it never grows on the audio thread, as `sample_notes`
    /// never does).
    fn queue_key(&mut self, slot: usize, status: u8, data1: i64, data2: i64) {
        if self.sample_keys.len() < self.sample_keys.capacity() {
            self.sample_keys.push(SampleKey {
                slot,
                status,
                data1: data1.clamp(0, 255) as u8,
                data2: data2.clamp(0, 255) as u8,
                frame: self.block_start + self.block_offset,
            });
        }
    }

    /// A key goes down on the pooled live voice (already scale-locked by the caller). The
    /// voice holds until [`note_off`](Self::note_off), then rings out (`noteOn`).
    pub fn note_on(&mut self, note: i64, velocity: i64) {
        let sr = self.sample_rate;
        let sweep_on = self.sweep_on;
        let Some(proj) = self.current.as_mut() else {
            return;
        };
        let live = proj.live_lane;
        // `.sweep`: how far through a buildup the step playing is (the graph's counter less one)
        let sweep_x = if sweep_on {
            proj.energy
                .as_ref()
                .and_then(|c| c.ramp_progress((proj.graph.tick - 1).max(0)))
                .unwrap_or(-1.0)
        } else {
            -1.0
        };
        let lane = &mut proj.lanes[live];
        if lane.sample_slot >= 0 {
            let slot = lane.sample_slot as usize;
            self.queue_key(slot, 0x90, note, velocity.clamp(1, 127));
            return;
        }
        lane.active = true;
        let w = velocity.clamp(1, 127) as f64 / 127.0;
        let mut v = 0;
        let mut best = f64::INFINITY;
        for k in 0..lane.poly {
            // prefer a free (not held) voice, then the quietest
            let a = libm::fabs(lane.sd[k] + lane.sa[k]) + if lane.hold[k] { 1000.0 } else { 0.0 };
            if a < best {
                best = a;
                v = k;
            }
        }
        let cur = lane.sd[v] + lane.sa[v];
        strike(lane, sr, v, note, sweep_x);
        if lane.f_amount > 0.0 {
            lane.f_vel[v] = w * w;
        }
        lane.md[v] = libm::exp(-lane.st[INV_SR] / 0.3); // release time constant, used after noteOff
        lane.hold[v] = true;
        lane.held_note[v] = note;
        lane.sd[v] = w;
        lane.sa[v] = cur - w;
    }

    /// The key comes up: its voice stops holding and rings out (`noteOff`).
    pub fn note_off(&mut self, note: i64) {
        let Some(proj) = self.current.as_mut() else {
            return;
        };
        let live = proj.live_lane;
        let lane = &mut proj.lanes[live];
        if lane.sample_slot >= 0 {
            let slot = lane.sample_slot as usize;
            self.queue_key(slot, 0x80, note, 0);
            return;
        }
        for k in 0..lane.poly {
            if lane.hold[k] && lane.held_note[k] == note {
                lane.hold[k] = false;
                lane.held_note[k] = -1;
            }
        }
    }

    /// Every held key comes up (`allNotesOff`). On a sampled voice the slot gets MIDI "all
    /// notes off", which ends the net lanes sharing it too.
    pub fn all_notes_off(&mut self) {
        let Some(proj) = self.current.as_mut() else {
            return;
        };
        let live = proj.live_lane;
        let lane = &mut proj.lanes[live];
        let slot = lane.sample_slot;
        for k in 0..lane.poly {
            lane.hold[k] = false;
            lane.held_note[k] = -1;
        }
        if slot >= 0 {
            self.queue_key(slot as usize, 0xB0, 123, 0);
        }
    }

    // MARK: ticks

    /// This tick of the net clock, into the anchor stream (`anchor`).
    fn anchor(&mut self, generation: i32, tick: i64, swap: bool) {
        let t = tick as i32;
        let mut flags = if swap {
            TickAnchorFlags::SWAP
        } else {
            TickAnchorFlags::default()
        };
        if self.clock_broken
            || (!swap
                && (generation != self.last_anchor_generation
                    || t != self.last_anchor_tick.wrapping_add(1)))
        {
            flags.insert(TickAnchorFlags::DISCONTINUITY);
        }
        // the counter after the tick: the step is tick - 1, its beat (tick - 1) / 4
        if !swap && t >= 1 && (t - 1) % 16 == 0 {
            let k = (self.downbeats_written as usize) % DOWNBEAT_RING;
            self.downbeats[k] = (self.tick_frame, (t as i64 - 1) / 4);
            self.downbeats_written += 1;
        }
        self.clock_broken = false;
        self.last_anchor_generation = generation;
        self.last_anchor_tick = t;
        let a = TickAnchor {
            frame: self.tick_frame,
            tick: t,
            generation,
            tempo: self.clock[TEMPO] as f32,
            flags,
        };
        if let Some(ring) = &self.anchor_ring {
            ring.write(a);
        }
        if self.record_events {
            self.anchors.push(a);
        }
    }

    fn do_tick(&mut self) {
        let Some(mut proj) = self.current.take() else {
            return;
        };
        if self.pending.is_some() && (proj.graph.tick + 1) % 16 == 0 {
            let next = self.pending.take().unwrap();
            self.current = Some(proj);
            self.install(next);
            self.ticks_in_block += 1;
            let (generation, tick) = {
                let p = self.current.as_ref().unwrap();
                (p.generation, p.graph.tick)
            };
            self.anchor(generation, tick, true);
            return;
        }
        // The mix-out tail's bar line: at the tick boundary of its end tick the deck stops
        // instead of playing that step.
        if proj.tail.is_some_and(|t| proj.graph.tick >= t.end_tick()) {
            self.current = Some(proj);
            self.stopped = true;
            self.stop();
            return;
        }
        let sr = self.sample_rate;
        let tempo = self.clock[TEMPO];
        let rec = self.record_events || self.fire_ring.is_some();
        let macro_flag = rec && self.macro_active_with(&proj);
        // The tick's anchor goes out before its fires.
        self.anchor(proj.generation, proj.graph.tick + 1, false);
        let generation = proj.generation;
        let to_vec = self.record_events;
        let ring = self.fire_ring.clone();
        let tick_frame = self.tick_frame;
        let tick_after = (proj.graph.tick + 1) as i32;
        // `.riser`: the step about to play is where its riser is
        self.riser_step = proj.graph.tick;
        let mut fires: Vec<FireEvent> = Vec::new();
        {
            let WaveProject {
                graph,
                lanes,
                rests,
                rest_cursor,
                feel_table,
                drift,
                energy,
                harmony,
                stinger_nets,
                ..
            } = &mut proj;
            let sweep_x = sweep_progress(self.sweep_on, energy.as_ref(), tick_after as i64);
            // the rest schedule at the step about to play (`advance` moves the tick past it)
            let resting = if rests.is_empty() {
                Class::NONE
            } else {
                rest_cursor.mask(graph.tick, rests)
            };
            // (stinger flags need the graph's table, which `advance_with` holds mutably: the
            // project keeps its own copy, made when it was built, so no tick copies it)
            let stinger: &[bool] = stinger_nets;
            let ctx = GrooveCtx {
                feel: feel_table.as_ref(),
                drift: *drift,
                groove_on: self.groove_on,
                swing: graph.project.swing,
                humanize: graph.project.humanize,
                tempo,
                step: graph.tick,
            };
            let groove = ctx.active();
            let (held, groove_next) = (&mut self.held, &mut self.groove_next);
            let duck_hold = &mut self.duck_hold;
            let samples = &mut self.sample_notes;
            let keys = &mut self.sample_keys;
            let (hits_out, record_hits) = (&mut self.hits, self.record_hits);
            let frame_now = self.block_start + self.block_offset;
            let clock_samples = self.clock[SAMPLES];
            graph.advance_with(|n, t| {
                let li = n.lane[t];
                if li >= 0 {
                    let li = li as usize;
                    if !resting.is_disjoint(lanes[li].rest_class) {
                        return;
                    }
                    let note = played_note(harmony.as_ref(), &lanes[li], n, t, ctx.step);
                    if groove {
                        let (delay_ms, vel) = ctx.timing(lanes[li].is_tonal, n, t, note);
                        let delay = delay_ms / 1000.0 * sr;
                        if delay >= 1.0 && held.len() < GROOVE_CAPACITY {
                            let due = clock_samples + delay;
                            held.push(Held {
                                due,
                                net: n.index,
                                trans: t,
                                lane: li,
                                vel,
                                tick: tick_after as i64,
                                grid: frame_now,
                                note,
                            });
                            if due < *groove_next {
                                *groove_next = due;
                            }
                        } else {
                            let lg = record_hits.then(|| LogTo {
                                out: &mut *hits_out,
                                frame: frame_now,
                                grid_frame: -1,
                                tick: tick_after as i64,
                            });
                            if let Some(sn) = hit(
                                lanes,
                                duck_hold,
                                li,
                                sr,
                                tempo,
                                n,
                                t,
                                note,
                                Some(vel),
                                sweep_x,
                                lg,
                                frame_now,
                                clock_samples,
                                keys,
                            ) {
                                if samples.len() < samples.capacity() {
                                    samples.push(sn);
                                }
                            }
                        }
                    } else {
                        let lg = record_hits.then(|| LogTo {
                            out: &mut *hits_out,
                            frame: frame_now,
                            grid_frame: -1,
                            tick: tick_after as i64,
                        });
                        if let Some(sn) = hit(
                            lanes,
                            duck_hold,
                            li,
                            sr,
                            tempo,
                            n,
                            t,
                            note,
                            None,
                            sweep_x,
                            lg,
                            frame_now,
                            clock_samples,
                            keys,
                        ) {
                            if samples.len() < samples.capacity() {
                                samples.push(sn);
                            }
                        }
                    }
                    if rec {
                        let steps = n.dur_steps[t];
                        let dur_ms = if steps > 0.0 {
                            steps * 60000.0 / (tempo * PPQ)
                        } else {
                            n.dur_ms[t]
                        };
                        let mut flags = if lanes[li].is_tonal {
                            FireFlags::default()
                        } else {
                            FireFlags::DRUM
                        };
                        if stinger[n.index] {
                            flags.insert(FireFlags::STINGER);
                        }
                        if macro_flag {
                            flags.insert(FireFlags::MACRO);
                        }
                        let event = FireEvent {
                            frame: tick_frame,
                            tick: tick_after,
                            generation,
                            net: n.index as i32,
                            transition: t as i32,
                            note: note as i16,
                            velocity: n.vel[t].clamp(0, 255) as u8,
                            channel: n.channel[t],
                            dur_steps: steps as f32,
                            dur_ms: dur_ms as f32,
                            flags,
                        };
                        if let Some(r) = &ring {
                            r.write(event);
                        }
                        if to_vec {
                            fires.push(event);
                        }
                    }
                }
            });
        }
        self.fires.extend(fires);
        if self.macro_hold_ticks > 0 {
            self.macro_hold_ticks -= 1;
        }
        self.ticks_in_block += 1;
        // the song's stop-transport ends the deck, unless a mix-out tail plays on past it
        let mut ends = proj.graph.stop_requested;
        if ends && proj.tail.is_some() {
            proj.graph.stop_requested = false;
            ends = false;
        }
        self.current = Some(proj);
        if ends {
            self.stopped = true;
            self.stop();
        }
    }

    fn macro_active_with(&self, proj: &WaveProject) -> bool {
        self.macro_hold_ticks > 0
            || proj.graph.has_active_macros()
            || self.tempo_fx != TempoFx::None
    }

    /// Render `frames` samples into `left` (and `right`, if given). `start_frame` is the
    /// frame of the first sample, the base of every `FireEvent` / `TickAnchor` stamp.
    pub fn process(&mut self, left: &mut [f32], right: Option<&mut [f32]>, frames: usize) {
        self.process_at(left, right, frames, 0);
    }

    pub fn process_at(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        start_frame: i64,
    ) {
        self.process_with(left, right, frames, start_frame, None);
    }

    /// [`process_at`](Self::process_at) with the host's sampled instruments rendered inside the
    /// block, as Swift's `process` renders its bank: `samplers` plays what the sampled lanes and
    /// live keys asked for at the end of the block (`flushSamplers`), into the block after the
    /// curve, or, with `.loudness`, into a scratch pair that joins the voices *before* the curve
    /// (`samplersInCurve`: the measured loudness gain and the section's energy curve then reach a
    /// sampled mix, and the master curve limits the sum). Without a right channel there is
    /// nothing for the slots to play on and `samplers` is left alone.
    pub fn process_with(
        &mut self,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        frames: usize,
        start_frame: i64,
        mut samplers: Option<&mut dyn SamplerHost>,
    ) {
        if right.is_none() {
            samplers = None;
        }
        if let Some(h) = samplers.as_deref_mut() {
            h.begin_block(self, frames);
        }
        self.ticks_in_block = 0;
        self.block_start = start_frame;
        self.service_tempo_fx();
        let gain = self.master_gain * self.current.as_ref().map_or(1.0, |p| p.gain);
        let sr = self.sample_rate;
        // an echo that is off takes nothing from the sampled leads' fifo: what waits in it is
        // dropped (`if !echoOn { echoCount = 0 }`), not kept for the next song with an echo
        if !self.echo_fx.as_ref().is_some_and(|e| e.is_on()) {
            self.echo_count = 0;
        }
        // (the curve is the one of the project that starts the block: a swap in the middle of it
        // still ends the block on that curve, read at the new project's step)
        let energy_of = self
            .current
            .as_ref()
            .filter(|p| self.energy_on && frames <= self.pre_l.len() && p.energy.is_some())
            .map(|p| p.generation);
        let has_energy = energy_of.is_some();
        // the sampled instruments join the voices before the curve with `.loudness` (and a block
        // that fits the scratch)
        let in_curve = samplers.is_some()
            && self.samplers_in_curve
            && frames <= self.pre_l.len()
            && frames <= self.smp_l.len();
        // stereo out: the side is kept (and the scratch side buffers written) only with a right
        // channel
        // (a pan macro makes the side even without `.stereo`: it is what moves the parts)
        let wide = (self.stereo_on || self.pan_moving())
            && right.is_some()
            && frames <= self.pre_s.len();
        // the dip recovers over about an eighth note (3 time constants), attack 2 ms
        let depth = if frames <= self.duck_buf.len() {
            self.current.as_ref().map_or(0.0, |p| p.duck_depth)
        } else {
            0.0
        };
        let duck_release = libm::exp(-6.0 * self.clock[TEMPO] / (60.0 * sr));
        let duck_attack = 1.0 - libm::exp(-1.0 / (0.002 * sr));
        if depth == 0.0 {
            self.duck_gain = 1.0;
        }
        for i in 0..frames {
            self.block_offset = i as i64;
            if self.scrubbing {
                self.scrub_sample();
            } else if self.playing {
                self.clock[TICK_POS] += 1.0;
                if self.clock[TICK_POS] >= self.clock[SPT] {
                    self.clock[TICK_POS] -= self.clock[SPT];
                    self.tick_frame = start_frame + i as i64;
                    self.do_tick();
                }
            }
            if !self.held.is_empty() && self.clock[SAMPLES] >= self.groove_next {
                self.service_groove();
            }
            if depth > 0.0 {
                self.duck_env += (self.duck_hold - self.duck_env) * duck_attack;
                self.duck_hold *= duck_release;
                self.duck_gain = 1.0 - depth * self.duck_env;
                self.duck_buf[i] = self.duck_gain as f32;
            }
            let mut mix = Mix::new(self.duck_gain, has_energy);
            if let Some(p) = self.current.as_mut() {
                for li in 0..p.lanes.len() {
                    if p.lanes[li].active {
                        lane_sample(&mut p.lanes, li, sr, &mut mix);
                    } else {
                        p.lanes[li].st[ENV] = 0.0;
                    }
                }
            }
            if let Some(f) = self.fading.as_mut() {
                for li in 0..f.lanes.len() {
                    if f.lanes[li].active {
                        lane_sample(&mut f.lanes, li, sr, &mut mix);
                    }
                }
            }
            if let Some(p) = self.current.as_mut() {
                for k in 0..p.sources.len() {
                    let s = p.sources[k];
                    p.lanes[s].st[PREV] = p.lanes[s].st[ENV];
                }
                if let Some(riser) = p.riser.as_mut() {
                    let at = self.playing && !self.scrubbing && self.riser_step != i64::MIN;
                    let pos =
                        at.then(|| self.riser_step as f64 + self.clock[TICK_POS] / self.clock[SPT]);
                    if riser.render(pos) {
                        // a stinger: not rested, not ducked, beside the drums (out of the
                        // tonal lowpass)
                        mix.acc += riser.mid;
                        mix.acc_s += riser.side;
                        mix.acc_send += riser.mid * riser.send;
                    }
                }
            }
            if let Some(echo) = self.echo_fx.as_mut() {
                if echo.is_on() {
                    let mut x = mix.acc_echo;
                    if self.echo_count > 0 {
                        x += self.echo_fifo[self.echo_read] as f64;
                        self.echo_read = if self.echo_read + 1 == self.echo_fifo.len() {
                            0
                        } else {
                            self.echo_read + 1
                        };
                        self.echo_count -= 1;
                    }
                    let (el, er) = echo.process(x);
                    let mid = 0.5 * (el + er);
                    let side = 0.5 * (el - er);
                    mix.acc_send += mid * EchoSpec::ROOM_SEND;
                    mix.add_tone(mid, side);
                } else {
                    // off (a dry song): the line still runs, empty, so what an earlier song
                    // left in it dies away now instead of waiting for the next song with an echo
                    let _ = echo.process(0.0);
                }
            }
            if let Some(reverb) = self.reverb.as_mut() {
                let mut x = mix.acc_send;
                if self.fifo_count > 0 {
                    x += self.send_fifo[self.fifo_read] as f64;
                    self.fifo_read = if self.fifo_read + 1 == self.send_fifo.len() {
                        0
                    } else {
                        self.fifo_read + 1
                    };
                    self.fifo_count -= 1;
                }
                if let Some(shimmer) = self.shimmer.as_mut() {
                    // off, it runs empty (its line dies away, as the echo's)
                    if shimmer.is_on() {
                        x += shimmer.process(mix.acc_shimmer);
                    } else {
                        let _ = shimmer.process(0.0);
                    }
                }
                let (wl, wr) = reverb.process(x);
                mix.add_tone(0.5 * (wl + wr), 0.5 * (wl - wr));
            }
            if has_energy {
                self.pre_l[i] = (mix.acc * gain) as f32;
                self.tone_l[i] = (mix.acc_tone * gain) as f32;
                if wide {
                    self.pre_s[i] = (mix.acc_s * gain) as f32;
                    self.tone_s[i] = (mix.acc_tone_s * gain) as f32;
                }
            } else if in_curve {
                self.pre_l[i] = (mix.acc * gain) as f32;
                if wide {
                    self.pre_s[i] = (mix.acc_s * gain) as f32;
                }
            } else if wide {
                let (l, r) = (mix.acc + mix.acc_s, mix.acc - mix.acc_s);
                let out = |v: f64| match self.master {
                    Master::Soft => libm::tanh(v * gain),
                    Master::Linear => v,
                    Master::Limit => v * gain,
                } as f32;
                left[i] = out(l);
                if let Some(rt) = right.as_deref_mut() {
                    rt[i] = out(r);
                }
            } else {
                let y = match self.master {
                    Master::Soft => libm::tanh(mix.acc * gain),
                    Master::Linear => mix.acc,
                    Master::Limit => mix.acc * gain,
                };
                left[i] = y as f32;
                if let Some(r) = right.as_deref_mut() {
                    r[i] = y as f32;
                }
            }
            self.clock[SAMPLES] += 1.0;
        }
        if let Some(generation) = energy_of {
            self.finish_energy(
                generation,
                left,
                right,
                frames,
                wide,
                start_frame,
                in_curve,
                samplers,
            );
        } else if in_curve {
            self.finish_in_curve(left, right, frames, wide, start_frame, samplers);
        } else if let (Some(h), Some(r)) = (samplers, right) {
            h.flush(self, start_frame, left, r, frames);
        }
        self.block_offset = 0;
        if let Some(f) = &self.fading {
            if !f.lanes.iter().any(|l| l.active) {
                if let Some(gone) = self.fading.take() {
                    self.retire(gone);
                }
            }
        }
    }

    /// The end of a block under `.energy`: drums (`pre_l`) plus the lowpassed tonal voices
    /// (`tone_l`), times the section's gain, through the master curve. Gain glides across the
    /// block; the cutoff moves once a block (`finishEnergy`). The sampled instruments join the
    /// tonal voices before the lowpass when `in_curve` (`samplersIn`), else the host plays them
    /// into the finished block beside the curve.
    #[allow(clippy::too_many_arguments)]
    fn finish_energy(
        &mut self,
        generation: i32,
        left: &mut [f32],
        mut right: Option<&mut [f32]>,
        frames: usize,
        wide: bool,
        start_frame: i64,
        in_curve: bool,
        mut samplers: Option<&mut dyn SamplerHost>,
    ) {
        let sr = self.sample_rate;
        let Some(proj) = self.current.as_ref() else {
            return;
        };
        // the curve of the project the block began on: the one playing, or, if a queued
        // project took over inside the block, the one that is now fading out
        let curve = if proj.generation == generation {
            proj.energy.as_ref()
        } else {
            self.fading
                .as_ref()
                .filter(|f| f.generation == generation)
                .and_then(|f| f.energy.as_ref())
        };
        let step = (proj.graph.tick - 1).max(0);
        let (target_gain, target_cut) =
            curve.map_or((self.energy_gain, self.energy_cut), |c| c.at(step));
        if !self.energy_primed {
            self.energy_gain = target_gain;
            self.energy_cut = target_cut;
            self.energy_primed = true;
        }
        let a = 1.0 - libm::exp(-(frames as f64) / (0.02 * sr));
        let g0 = self.energy_gain;
        self.energy_gain += (target_gain - self.energy_gain) * a;
        self.energy_cut *= libm::pow(target_cut / self.energy_cut, a);
        self.tone_lp.0.tune(self.energy_cut, sr);
        self.tone_lp.1.tune(self.energy_cut, sr);
        let track_gain = self.current.as_ref().map_or(1.0, |p| p.gain) as f32;
        if in_curve {
            if let Some(h) = samplers.as_deref_mut() {
                self.render_samplers_apart(h, start_frame, frames, wide);
            }
        }
        let step1 = (self.energy_gain - g0) / frames as f64;
        let linear = self.master != Master::Soft;
        for i in 0..frames {
            let g = g0 + step1 * (i + 1) as f64;
            let tone = self.tone_l[i] as f64;
            let dry = self.pre_l[i] as f64;
            if let Some(r) = right.as_deref_mut() {
                let sl = if in_curve {
                    (track_gain * self.smp_l[i]) as f64
                } else {
                    0.0
                };
                let sr = if in_curve {
                    (track_gain * self.smp_r[i]) as f64
                } else {
                    0.0
                };
                let (l, rr) = if wide {
                    let (ds, ts) = (self.pre_s[i] as f64, self.tone_s[i] as f64);
                    (
                        (dry + ds + self.tone_lp.0.process(tone + ts + sl)) * g,
                        (dry - ds + self.tone_lp.1.process(tone - ts + sr)) * g,
                    )
                } else {
                    (
                        (dry + self.tone_lp.0.process(tone + sl)) * g,
                        (dry + self.tone_lp.1.process(tone + sr)) * g,
                    )
                };
                left[i] = (if linear { l } else { libm::tanh(l) }) as f32;
                r[i] = (if linear { rr } else { libm::tanh(rr) }) as f32;
            } else {
                let l = (dry + self.tone_lp.0.process(tone)) * g;
                left[i] = (if linear { l } else { libm::tanh(l) }) as f32;
            }
        }
        if !in_curve {
            if let (Some(h), Some(r)) = (samplers, right) {
                h.flush(self, start_frame, left, r, frames);
            }
        }
    }

    /// `samplersInCurve`: the host's slots rendered into a cleared scratch pair (`smpL` / `smpR`,
    /// the lows mono under `.stereo`, `monoSamplerLows`), to be added to the voices before the
    /// curve at the track gain. The pair is taken out of the runner while the host renders
    /// (which reads the runner), and put back: an empty `Vec` neither allocates nor frees.
    fn render_samplers_apart(
        &mut self,
        host: &mut dyn SamplerHost,
        start_frame: i64,
        frames: usize,
        wide: bool,
    ) {
        let mut l = core::mem::take(&mut self.smp_l);
        let mut r = core::mem::take(&mut self.smp_r);
        l[..frames].fill(0.0);
        r[..frames].fill(0.0);
        host.flush(
            self,
            start_frame,
            &mut l[..frames],
            &mut r[..frames],
            frames,
        );
        if wide {
            for i in 0..frames {
                self.sampler_side.process(&mut l[i], &mut r[i]);
            }
        }
        self.smp_l = l;
        self.smp_r = r;
    }

    /// The end of a block with the sampled instruments in the curve and no `.energy`
    /// (the `inCurve` tail of `process`): the voices (`pre_l`, with the side `pre_s` under
    /// `.stereo`) plus the slots at the track gain, through the master curve.
    fn finish_in_curve(
        &mut self,
        left: &mut [f32],
        right: Option<&mut [f32]>,
        frames: usize,
        wide: bool,
        start_frame: i64,
        samplers: Option<&mut dyn SamplerHost>,
    ) {
        let track_gain = self.current.as_ref().map_or(1.0, |p| p.gain) as f32;
        if let Some(h) = samplers {
            self.render_samplers_apart(h, start_frame, frames, wide);
        }
        let linear = self.master != Master::Soft;
        let curve = |v: f32| {
            if linear {
                v
            } else {
                libm::tanh(v as f64) as f32
            }
        };
        if let Some(r) = right {
            for i in 0..frames {
                let s: f32 = if wide { self.pre_s[i] } else { 0.0 };
                let a = (self.pre_l[i] + s) + track_gain * self.smp_l[i];
                let b = (self.pre_l[i] - s) + track_gain * self.smp_r[i];
                left[i] = curve(a);
                r[i] = curve(b);
            }
        } else {
            // mono, as the bank renders it without a right channel: the mean of its two
            for (i, out) in left[..frames].iter_mut().enumerate() {
                let a = self.pre_l[i] + track_gain * 0.5 * (self.smp_l[i] + self.smp_r[i]);
                *out = curve(a);
            }
        }
    }

    /// Play the held-back notes that are due (`serviceGroove`).
    fn service_groove(&mut self) {
        let Some(proj) = self.current.as_mut() else {
            self.held.clear();
            self.groove_next = f64::INFINITY;
            return;
        };
        let now = self.clock[SAMPLES];
        let (sr, tempo) = (self.sample_rate, self.clock[TEMPO]);
        let frame = self.block_start + self.block_offset;
        let mut next = f64::INFINITY;
        let mut k = 0;
        while k < self.held.len() {
            if self.held[k].due <= now {
                let h = self.held.swap_remove(k);
                let n = &proj.graph.nets[h.net];
                let sweep_x = sweep_progress(self.sweep_on, proj.energy.as_ref(), h.tick);
                let lg = self.record_hits.then(|| LogTo {
                    out: &mut self.hits,
                    frame,
                    grid_frame: h.grid,
                    tick: h.tick,
                });
                if let Some(sn) = hit(
                    &mut proj.lanes,
                    &mut self.duck_hold,
                    h.lane,
                    sr,
                    tempo,
                    n,
                    h.trans,
                    h.note,
                    Some(h.vel),
                    sweep_x,
                    lg,
                    frame,
                    now,
                    &mut self.sample_keys,
                ) {
                    if self.sample_notes.len() < self.sample_notes.capacity() {
                        self.sample_notes.push(sn);
                    }
                }
            } else {
                next = next.min(self.held[k].due);
                k += 1;
            }
        }
        self.groove_next = next;
    }

    // MARK: scratching the net clock

    /// Hand on the platter: the tick clock stops following the tempo and follows the hand
    /// (`freeze`).
    pub fn freeze(&mut self) {
        self.flush_groove();
        let Some(tick) = self.graph().map(|g| g.tick) else {
            return;
        };
        if self.scrubbing {
            return;
        }
        self.scrubbing = true;
        self.clock_broken = true;
        self.scrub_pos = tick as f64 + self.tick_phase();
        self.scrub_target = self.scrub_pos;
        self.scrub_rate = 0.0;
    }

    /// Move the hand by this many ticks (negative = backwards, undoing ticks) (`scrub`).
    pub fn scrub(&mut self, ticks: f64) {
        if self.scrubbing {
            self.scrub_target += ticks;
        }
    }

    /// Hand off: back to the tempo clock from wherever the platter is (`release`).
    pub fn release(&mut self) {
        if !self.scrubbing {
            return;
        }
        self.scrubbing = false;
        self.clock_broken = true;
        self.scrub_rate = 0.0;
        self.clock[TICK_POS] = (self.scrub_pos - libm::floor(self.scrub_pos)) * self.clock[SPT];
    }

    /// Seconds the net clock takes to catch up with the hand (`setChase`).
    pub fn set_chase(&mut self, seconds: f64) {
        self.scrub_chase = seconds.max(0.002) * self.sample_rate;
    }

    pub fn is_scrubbing(&self) -> bool {
        self.scrubbing
    }

    /// Where the platter is, in ticks (`scrubPos`): it chases [`scrub_target`](Self::scrub_target).
    pub fn scrub_position(&self) -> f64 {
        self.scrub_pos
    }

    /// Where the hand has turned the platter to, in ticks (`scrubTarget`): every `scrub` adds to it.
    pub fn scrub_target(&self) -> f64 {
        self.scrub_target
    }

    /// One sample of the platter chasing the hand: whole ticks crossed forward are played,
    /// backward ones undone through the history, each with its notes (`scrubSample`).
    fn scrub_sample(&mut self) {
        let Some(mut proj) = self.current.take() else {
            return;
        };
        let spt = self.clock[SPT];
        let max_rate = 12.0 / spt; // ticks per sample: 12x normal speed
        let want = ((self.scrub_target - self.scrub_pos) / self.scrub_chase)
            .max(-max_rate)
            .min(max_rate);
        self.scrub_rate += (want - self.scrub_rate) * self.scrub_smooth;
        let mut new_pos = self.scrub_pos + self.scrub_rate;
        if (self.scrub_target - new_pos).abs() < 1e-3 {
            // settled: land on the target, not just short of it
            new_pos = self.scrub_target;
            self.scrub_rate = 0.0;
        }
        let goal = libm::floor(new_pos) as i64;
        let sr = self.sample_rate;
        let tempo = self.clock[TEMPO];
        let frame = self.block_start + self.block_offset;
        {
            let WaveProject {
                graph,
                lanes,
                energy,
                harmony,
                ..
            } = &mut proj;
            let sweep_on = self.sweep_on;
            let samples = &mut self.sample_notes;
            let keys = &mut self.sample_keys;
            let now = self.clock[SAMPLES];
            let duck_hold = &mut self.duck_hold;
            let (hits_out, record_hits) = (&mut self.hits, self.record_hits);
            // `tick`: the graph's counter at the hit, as `onHit` reports it; the step the note
            // is played at is the one before it (forward, `advance` counts first; backward,
            // `unadvance` counts back last)
            let mut play = |lanes: &mut [Lane], n: &CompiledNet, t: usize, tick: i64| {
                let li = n.lane[t];
                if li >= 0 {
                    let lg = record_hits.then_some(LogTo {
                        out: &mut *hits_out,
                        frame,
                        grid_frame: -1,
                        tick,
                    });
                    if let Some(sn) = hit(
                        lanes,
                        duck_hold,
                        li as usize,
                        sr,
                        tempo,
                        n,
                        t,
                        played_note(harmony.as_ref(), &lanes[li as usize], n, t, tick - 1),
                        None,
                        sweep_progress(sweep_on, energy.as_ref(), tick),
                        lg,
                        frame,
                        now,
                        keys,
                    ) {
                        if samples.len() < samples.capacity() {
                            samples.push(sn);
                        }
                    }
                }
            };
            while graph.tick < goal {
                let tick = graph.tick + 1; // (`advance` counts first)
                graph.advance_with(|n, t| play(lanes, n, t, tick));
                self.ticks_in_block += 1;
                graph.stop_requested = false; // a scratched-over end of song doesn't stop the deck
            }
            while graph.tick > goal {
                let tick = graph.tick; // (`unadvance` counts back last)
                let ok = graph.unadvance(|n, t| play(lanes, n, t, tick));
                if !ok {
                    // the start of the log: the platter stops here
                    new_pos = graph.tick as f64;
                    self.scrub_rate = 0.0;
                    self.scrub_target = self.scrub_target.max(new_pos);
                    break;
                }
            }
        }
        self.current = Some(proj);
        self.scrub_pos = new_pos;
        self.clock[TICK_POS] = (new_pos - libm::floor(new_pos)) * spt;
    }

    /// Play everything held back now (before a swap changes what they refer to).
    fn flush_groove(&mut self) {
        if self.held.is_empty() {
            return;
        }
        for h in self.held.iter_mut() {
            h.due = 0.0;
        }
        self.service_groove();
        self.groove_next = f64::INFINITY;
    }
}

/// A compiled project in frames (feature `transfer`), so the instance that installs it can take it
/// in a few frames at a time ([`WaveBuild`]): the project without its lanes, nets, source project
/// and the per-net parts of its marking layout, then each compiled net, then each lane, then each
/// net's marking layout. The graph's source project is returned, to travel ahead of these
/// ([`crate::transfer::write_project`]): [`WaveBuild::start`] wants it first.
#[cfg(feature = "transfer")]
impl WaveProject {
    pub fn into_frames(mut self, f: &mut crate::transfer::Frames) -> crate::transfer::Result<Project> {
        let project = core::mem::take(&mut self.graph.project);
        let nets = core::mem::take(&mut self.graph.nets);
        let lanes = core::mem::take(&mut self.lanes);
        let layout = self.marking.layout.clone();
        let marks = layout.net_ids.len();
        if layout.nets.len() != marks || layout.place_ids.len() != marks {
            return Err(crate::transfer::Error("marking layout out of step".into()));
        }
        f.push(&(nets.len() as u64, lanes.len() as u64, marks as u64, &self))?;
        for n in &nets {
            f.push(n)?;
        }
        for l in &lanes {
            f.push(l)?;
        }
        for i in 0..marks {
            f.push(&(&layout.net_ids[i], &layout.nets[i], &layout.place_ids[i]))?;
        }
        Ok(project)
    }
}

/// A [`WaveProject`] being taken in from [`WaveProject::into_frames`]: [`step`](Self::step) once
/// per frame, then [`finish`](Self::finish).
#[cfg(feature = "transfer")]
pub struct WaveBuild {
    wave: WaveProject,
    nets: usize,
    lanes: usize,
    marks: usize,
    layout: MarkingLayout,
}

#[cfg(feature = "transfer")]
impl WaveBuild {
    /// The shell frame, with the graph's source `project` (what its nets were compiled from).
    pub fn start(r: &mut crate::transfer::FrameReader, project: Project) -> crate::transfer::Result<WaveBuild> {
        let (nets, lanes, marks, mut wave): (u64, u64, u64, WaveProject) = r.next()?;
        wave.graph.project = project;
        // (the shell's layout has none of the per-net parts: a cheap copy)
        let layout = (*wave.marking.layout).clone();
        Ok(WaveBuild { wave, nets: nets as usize, lanes: lanes as usize, marks: marks as usize, layout })
    }

    pub fn done(&self) -> bool {
        self.wave.graph.nets.len() == self.nets
            && self.wave.lanes.len() == self.lanes
            && self.layout.net_ids.len() == self.marks
    }

    /// Takes the next frame (a net, else a lane, else a net's marking layout). A net's bundle is
    /// the project's again.
    pub fn step(&mut self, r: &mut crate::transfer::FrameReader) -> crate::transfer::Result<()> {
        if self.wave.graph.nets.len() < self.nets {
            let mut n: CompiledNet = r.next()?;
            if let Some(b) = self.wave.graph.project.nets.get(&n.id) {
                n.bundle = b.clone();
            }
            self.wave.graph.nets.push(n);
        } else if self.wave.lanes.len() < self.lanes {
            let l: Lane = r.next()?;
            self.wave.lanes.push(l);
        } else if self.layout.net_ids.len() < self.marks {
            let (id, info, places): (String, super::marking_mirror::NetInfo, Vec<String>) = r.next()?;
            self.layout.net_ids.push(id);
            self.layout.nets.push(info);
            self.layout.place_ids.push(places);
        } else {
            return Err(crate::transfer::Error("no frame wanted".into()));
        }
        Ok(())
    }

    /// The project, this process's ([`WaveProject::installed`]).
    pub fn finish(mut self, sample_rate: f64, track_serial: Option<i32>) -> crate::transfer::Result<WaveProject> {
        if !self.done() {
            return Err(crate::transfer::Error("frames missing".into()));
        }
        self.wave.installed(sample_rate, track_serial, self.layout);
        Ok(self.wave)
    }
}
