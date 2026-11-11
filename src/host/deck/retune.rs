//! Tuning a loop's instruments without taking it again (`LoopRetune.swift`, `ClipRetune.render`):
//! a net take's provenance says what the deck was playing (its share, with any `tracks`
//! override), where (the take's first beat) and how fast, so the same bars are rendered again
//! offline with other instruments on some channels. Nothing here touches a deck, the speakers
//! or `compose`: the share is composed as a deck composes it ([`PreparedTrack`], the app's
//! `WaveOptions::extended()`) and played through a [`DeckCore`] of its own, which is the net
//! source of a deck (`WaveDeckSource`): its output is before the deck's EQ, filter and fader,
//! where a take is the deck after its EQ. With the EQ flat the two are the same audio, and
//! played with the voices it was taken with, the render nulls against the take (sampled
//! instruments with round robin or modulation aside, which differ between any two renders).
//!
//! The rules (the refusals, the take's ticks, the cut) are `crate::deck::loops`; this file
//! is the part that plays: the preparer with a sampler bank of its own, the song's levels kept
//! when an instrument changes, the offline render and the cut. Off the main thread is fine
//! (it composes and renders, which takes a while): [`RetuneEnv`] is what it reads of the
//! engine and is `Send`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::deck::clips::LoopClip;
use crate::deck::loops::{
    retune_cut, retune_frame_limit, retune_ticks, take_provenance, Refusal,
};
use crate::deck::turntable::SampleBuffer;
use crate::deck::voices::RoleVoice;
use crate::json::JsonValue;
use crate::session::model::DeckSource;
use crate::share::Share;
use crate::voices::VoiceCatalog;
use crate::wave::net::str_hash;
use crate::wave::{TickAnchorFlags, WaveOptions};

use super::core::{Command, DeckCore, DeckMirror};
use super::engine::Engine;
use super::track::{PreparedTrack, Preparer, TrackSource};
use super::wander::InstalledSounds;

/// A clip's bars rendered again (`ClipRetune.Retuned`).
pub struct Retuned {
    /// Exactly the clip's beats, from its first beat, at its tempo.
    pub buffer: SampleBuffer,
    /// The share it was played from: the take's, with `voices` merged into its `tracks` override
    /// (`DeckSource::with_instrument`).
    pub source: DeckSource,
}

/// What a retune render reads of the host: the sounds installed here, which its palettes resolve
/// against, and (feature `sf2`) the library its sampler bank opens. Without either, every
/// instrument plays a synth voice.
#[derive(Clone, Default)]
pub struct RetuneEnv {
    /// What the host says is installed ([`Engine::installed`]); first choice over `library`.
    pub installed: Option<Arc<InstalledSounds>>,
    /// The SoundFonts the bank plays sampled instruments from.
    #[cfg(feature = "sf2")]
    pub library: Option<Arc<crate::host::sampler::Library>>,
}

impl RetuneEnv {
    /// `TrackSpec.project()` with the sounds installed here (the palettes' picks): what the host
    /// says is installed, else the scanned library (feature `sf2`), else nothing
    /// (`Preparer::project_of`).
    pub fn project_of(&self, spec: &crate::TrackSpec) -> crate::pflow::Project {
        if let Some(i) = &self.installed {
            return spec.project_in(i.sounds());
        }
        #[cfg(feature = "sf2")]
        if let Some(lib) = &self.library {
            return spec.project_in(lib.sounds());
        }
        spec.project()
    }
}

/// Frames the render takes at a time (`block`).
const BLOCK: usize = 512;

/// `ClipRetune.render(clip:voices:sampleRate:)`: render `clip`'s bars again with `voices` (MIDI
/// channel to beats instrument) over the instruments it was taken with, at `sample_rate`. The
/// other channels play what they played. Pure: a fresh source and sampler bank, nothing shared
/// with a deck.
///
/// The output is pre-EQ and pre-fader. Refused for sample takes, crops of them, sequences and
/// takes with macros running ([`Refusal`]).
pub fn render(
    clip: &LoopClip,
    voices: &BTreeMap<i64, String>,
    sample_rate: f64,
    env: &RetuneEnv,
) -> Result<Retuned, Refusal> {
    render_roles_off(clip, voices, sample_rate, env, &[])
}

/// [`render`] with `roles_off`: more roles to leave silent, beside the take's own (the tests solo a
/// part) (`render(clip:voices:sampleRate:rolesOff:)`).
pub fn render_roles_off(
    clip: &LoopClip,
    voices: &BTreeMap<i64, String>,
    sample_rate: f64,
    env: &RetuneEnv,
    roles_off: &[String],
) -> Result<Retuned, Refusal> {
    let Replay {
        mut core,
        start_tick,
        end_tick,
        bpm,
        source,
        ..
    } = replay_core(clip, voices, sample_rate, env, roles_off)?;

    // Play until the anchor of the tick after the take's last one: the take ran from the
    // downbeat anchor of its first beat to the one after its last, so the cut does too.
    let limit = retune_frame_limit(end_tick, bpm, sample_rate);
    let (mut left, mut right): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
    let (mut start, mut end): (Option<i64>, Option<i64>) = (None, None);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut done: i64 = 0;
    let mut base_frame: i64 = 0; // the frame of left[0]
    while end.is_none_or(|e| done < e) {
        if done >= limit || core.mirror.stopped.load(Ordering::Relaxed) {
            return Err(Refusal::SongEnded);
        }
        core.render(&mut l, &mut r, done);
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
        done += BLOCK as i64;
        let anchors: Vec<(i64, i64, bool)> = core
            .runner
            .anchors
            .drain(..)
            .map(|a| {
                (
                    a.tick as i64,
                    a.frame,
                    a.flags.contains(TickAnchorFlags::SWAP),
                )
            })
            .collect();
        core.runner.fires.clear();
        (start, end) = retune_cut(&anchors, start_tick, end_tick, start, end);
        // until the take's first beat, keep only the last few blocks (a song played from its
        // top to a late take would otherwise be held whole)
        if start.is_none() && left.len() > 8 * BLOCK {
            let drop = left.len() - 4 * BLOCK;
            left.drain(..drop);
            right.drain(..drop);
            base_frame += drop as i64;
        }
        if done % (BLOCK as i64 * 64) == 0 {
            drain_retired(&mut core);
        }
    }
    drain_retired(&mut core);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(Refusal::SongEnded);
    };
    if end <= start || start < base_frame {
        return Err(Refusal::SongEnded);
    }
    let range = (start - base_frame) as usize..(end - base_frame) as usize;
    let buffer = SampleBuffer::new(
        left[range.clone()].to_vec(),
        Some(right[range].to_vec()),
        sample_rate,
    );
    Ok(Retuned { buffer, source })
}

/// A take's song set up to play again from a little before its first beat, as [`render`]
/// plays it: what [`render`] and `loop_notes::extract` share.
pub(crate) struct Replay {
    /// Loaded, the take's roles muted, at the take's tempo, playing; recording its events.
    pub core: DeckCore,
    pub start_tick: i64,
    pub end_tick: i64,
    /// The tempo the take was played at.
    pub bpm: f64,
    /// The share played: the take's, with `voices` and earlier retunes merged in.
    pub source: DeckSource,
    /// Each net of the loaded project, in project order (a fire's `net`): (id, riff group).
    pub nets: Vec<(String, String)>,
    /// The loaded project's generation (a fire's `generation`).
    pub generation: i32,
}

/// The setup half of [`render_roles_off`]: the refusals, the share with `voices`, the song's
/// own levels kept, composed from a little before the take and loaded into a fresh source.
pub(crate) fn replay_core(
    clip: &LoopClip,
    voices: &BTreeMap<i64, String>,
    sample_rate: f64,
    env: &RetuneEnv,
    roles_off: &[String],
) -> Result<Replay, Refusal> {
    let take = take_provenance(clip.provenance.as_ref())?;
    let taken = take
        .get("source")
        .and_then(DeckSource::from_json)
        .ok_or(Refusal::NoProvenance)?;
    match taken {
        DeckSource::Net(_) | DeckSource::RawNets(_) => {}
        DeckSource::Sample { .. } | DeckSource::None => return Err(Refusal::NotANet),
    }
    let macros = clip.origin_macros();
    if !macros.is_empty() {
        return Err(Refusal::Macros(macros));
    }
    // a retuned clip plays its earlier retunes' instruments, under the ones asked for now
    let mut source = taken.clone();
    let mut all = clip.retuned_voices();
    for (channel, instrument) in voices {
        all.insert(*channel, instrument.clone());
    }
    for (channel, instrument) in &all {
        source = source
            .with_instrument(instrument, *channel)
            .ok_or(Refusal::NotANet)?;
    }

    let (start_tick, end_tick, from) = retune_ticks(clip.start_beat, clip.beats)?;
    // A deck's source, as `Deck` builds one, with a sampler bank of its own.
    let base = Preparer::standalone(
        sample_rate,
        WaveOptions::extended(),
        env.installed.clone(),
        #[cfg(feature = "sf2")]
        env.library.clone(),
    );
    let mut preparer = base.clone();
    if source != taken {
        // the song's own levels, as the take had them (`Deck.sameSongOptions`): the loudness
        // gain, part trims, energy, room and echo are measured over the whole mix, so measured
        // again with another instrument they would move every part, not only its own
        let song = prepare(&taken, from, &base).ok_or(Refusal::NotANet)?;
        let wave = song.prepared.wave();
        let o = &mut preparer.opts;
        o.track_gain = Some(wave.gain);
        o.track_energy = wave.energy.clone();
        o.duck_depth = wave.duck_depth;
        o.space = wave.space;
        o.echo = wave.echo;
        o.track_balance = wave.balance.clone();
        o.rests = Some(wave.rests.clone());
        // (the samplers it bound stay in the bank: the track below finds their slots numbered)
    }
    let track = prepare(&source, from, &preparer).ok_or(Refusal::NotANet)?;

    let mut core = DeckCore::new(
        "retune",
        sample_rate,
        base.opts.clone(),
        base.opts.turntable_brake,
        Arc::new(DeckMirror::new()),
    );
    core.runner.record_events = true;
    let PreparedTrack {
        played, prepared, ..
    } = track;
    #[cfg(feature = "sf2")]
    for (slot, s) in preparer.take_samplers() {
        core.apply(Command::Sampler(slot, s));
    }
    let nets: Vec<(String, String)> = prepared
        .project
        .graph
        .nets
        .iter()
        .map(|n| (n.id.clone(), n.bundle.riff_group.clone()))
        .collect();
    let generation = prepared.project.generation;
    core.apply(Command::Load(prepared.project));
    let mut off: BTreeSet<String> = clip.origin_roles_off().into_iter().collect();
    off.extend(roles_off.iter().cloned());
    for role in &off {
        core.apply(Command::RoleMute {
            hash: str_hash(role),
            muted: true,
        });
    }
    let bpm = clip.origin_bpm().unwrap_or(clip.bpm);
    if bpm > 0.0 && (bpm - played.tempo).abs() > 1e-9 {
        core.apply(Command::Tempo(bpm));
    }
    core.apply(Command::Play);
    Ok(Replay {
        core,
        start_tick,
        end_tick,
        bpm,
        source,
        nets,
        generation,
    })
}

/// `ClipRetune.voices(for:)`: the instruments `clip` can be retuned on, without a deck: its
/// track's roles with an instrument to choose (`RoleVoice`, as `Deck.roleVoices` lists them),
/// each on the instrument the clip plays now (its take's, with any earlier retune's). The roles
/// that were off in the take are left out (they are silent in it). Empty for a clip [`render`]
/// refuses.
///
/// `spec.project()` in Swift resolves a palette's sounds against what the Mac has installed, so
/// the instrument a role plays "now" is the pick that machine makes: `env` is what this host
/// has ([`Engine::retune_env`]; with neither `installed` nor a library nothing is, as for
/// `Sounds::nothing`). A raw-`nets` share's project has no palette to resolve (`Share.project()`),
/// and the catalog half (`RoleVoice.list` reads only the instrument -> category map) is the
/// bundled one.
pub fn voices_for(clip: &LoopClip, env: &RetuneEnv) -> Vec<RoleVoice> {
    if take_provenance(clip.provenance.as_ref()).is_err() || !clip.origin_macros().is_empty() {
        return Vec::new();
    }
    let Some(source) = clip.origin_source() else {
        return Vec::new();
    };
    let (project, genre) = match &source {
        DeckSource::Net(spec) => (env.project_of(spec), spec.genre.clone()),
        DeckSource::RawNets(payload) => {
            let Ok(p) = Share::from_payload(payload, None).project() else {
                return Vec::new();
            };
            let genre = payload.get("genre").and_then(JsonValue::as_str);
            (p, genre.unwrap_or("").to_string())
        }
        DeckSource::Sample { .. } | DeckSource::None => return Vec::new(),
    };
    RoleVoice::list_for_retune(
        &project,
        &genre,
        &clip.origin_roles_off(),
        &VoiceCatalog::bundled(),
    )
}

/// `drainRetired`: the projects and buffers the deck let go of, freed here.
pub(crate) fn drain_retired(core: &mut DeckCore) {
    while core.runner.pop_retired().is_some() {}
    while core.pop_retired_buffer().is_some() {}
}

/// `source` composed as a deck composes it, from tick `from` (a raw-`nets` share from its top)
/// (`ClipRetune.prepare`).
pub(crate) fn prepare(source: &DeckSource, from: i64, preparer: &Preparer) -> Option<PreparedTrack> {
    match source {
        DeckSource::Net(spec) => Some(PreparedTrack::compose(spec, preparer, from)),
        DeckSource::RawNets(payload) => {
            PreparedTrack::from_source(&TrackSource::RawNets(payload.clone()), preparer, 0)
        }
        DeckSource::Sample { .. } | DeckSource::None => None,
    }
}

impl Engine {
    /// `ClipRetune.voices(for:)` for clip `id` on the row, against what is installed on this
    /// engine ([`voices_for`]). Empty for a clip not on the row.
    pub fn clip_voices(&self, id: &str) -> Vec<RoleVoice> {
        self.clip(id)
            .map(|c| voices_for(c, &self.retune_env()))
            .unwrap_or_default()
    }

    /// What a retune render reads of this engine: the sounds installed here and its library.
    /// Hand it to [`render`] on any thread.
    pub fn retune_env(&self) -> RetuneEnv {
        RetuneEnv {
            installed: self.installed.clone(),
            #[cfg(feature = "sf2")]
            library: self.library.clone(),
        }
    }

    /// `ClipRetune.render` for clip `id` on the row, with this engine's sample rate and sounds:
    /// the render a live caller makes off the main thread and hands to
    /// [`retune_clip`](Self::retune_clip). None for a clip not on the row.
    pub fn render_retune(
        &self,
        id: &str,
        voices: &BTreeMap<i64, String>,
    ) -> Option<Result<Retuned, Refusal>> {
        let clip = self.clip(id)?;
        Some(render(clip, voices, self.sample_rate, &self.retune_env()))
    }
}
