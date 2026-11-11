//! The pure rules of the track pipeline (`DeckGenerator.swift`, `DeckPerform.swift`): where a
//! track starts, and which options another start of the same song is prepared with.
//!
//! - [`start_for`]: `PreparedTrack.init(spec:startTick:)` and `restarted(atTick:)` start at the
//!   tick asked for only inside the song; a tick at or past its end (or any on a loop) is the top.
//! - [`Latest`]: what `Deck.wave.latest` tells the deck about the project it loaded last: its
//!   generation, where it started, which song it is, and everything `prepare` measured that
//!   another start of the song reuses instead of measuring again.
//! - [`same_song_options`] / [`moved_options`]: `Deck.sameSongOptions` / `Deck.movedOptions`.
//! - [`restarted_options`]: the options `PreparedTrack.restarted` prepares with.
//!
//! Composing and preparing, the threads they run on and the slots a result waits in are the
//! host's (`crate::host::deck::track`); these decide *with what*.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::musicality::echo::EchoSpec;
use crate::musicality::energy_curve::EnergyCurve;
use crate::musicality::fixes::Fixes;
use crate::musicality::space::SpaceSpec;
use crate::musicality::synth_voice::VoiceDesign;
use crate::pflow::Project;
use crate::wave::rests::{Entry, MixOutTail, RestSchedule};
use crate::wave::runner::{WaveOptions, WaveProject};
use crate::wave::GrooveTemplate;

/// The song's length in ticks: the sum of its structure's steps (`project.structure.reduce`).
pub fn song_length(project: &Project) -> i64 {
    project.structure.iter().map(|s| s.steps).sum()
}

/// `startTick > 0 && startTick < length ? startTick : 0`: a track starts where it is asked to
/// only inside the song; at or past its end, or on a loop (no structure: length 0), it starts at
/// the top.
pub fn start_for(start_tick: i64, length: i64) -> i64 {
    if start_tick > 0 && start_tick < length {
        start_tick
    } else {
        0
    }
}

/// `Project.palette`: the palette the track plays from, the first playing (non-control) net's;
/// None = none.
pub fn project_palette(project: &Project) -> Option<String> {
    for id in project.nets.keys() {
        let Some(nb) = project.nets.get(id) else {
            continue;
        };
        if nb.role == "control" {
            continue;
        }
        return nb.track.palette.clone();
    }
    None
}

/// What the deck knows of the project it loaded last (`Deck.wave.latest`, a `WaveProject`),
/// read off it before it goes to the audio thread: the numbers `sameSongOptions` and
/// `PreparedTrack.restarted` carry to another start of the song.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct Latest {
    /// `WaveProject.generation`: process-wide, never compared across runs.
    pub generation: i32,
    /// Where it started (`WaveProject.startTick`).
    pub start_tick: i64,
    /// Which song it is (`WaveProject.trackSerial`).
    pub track_serial: i32,
    pub gain: f64,
    pub energy: Option<EnergyCurve>,
    pub duck_depth: f64,
    pub space: Option<SpaceSpec>,
    pub echo: Option<EchoSpec>,
    pub balance: Option<BTreeMap<String, f64>>,
    /// The song's own rests, without the tail's.
    pub song_rests: RestSchedule,
    /// With the tail's.
    pub rests: RestSchedule,
    pub tail: Option<MixOutTail>,
    pub feel: Option<GrooveTemplate>,
    /// The sound design it was prepared with (`WaveOptions.voiceDesign`).
    pub voice_design: VoiceDesign,
    /// What each channel plays (`channelVoice`), for the sound panel and the voice settings a
    /// deck keeps through a hand-over.
    pub channel_voices: BTreeMap<i64, crate::wave::ChannelVoice>,
    /// The graph's role tables (`latest.graph.roleTargets` / `groupNets`), which `rolesHeld` and
    /// `isRoleAudible` read after the project has gone to the audio thread.
    pub role_tables: crate::wave::RoleTables,
    /// The nets of its lanes that were bound to a sampler (`lane.samplerSlot >= 0` as prepared):
    /// (net id, channel, slot), in lane order. `WaveDeckSource.sampledNets` reads the lanes'
    /// slot live, which is -1 for a lane the sound panel put on its synth voice: the channel is
    /// there so the host can leave those out.
    pub sampled: Vec<(String, i64, usize)>,
}

impl Latest {
    /// `p` as `wave.latest` shows it; `voice_design` is what it was prepared with
    /// (`WaveProject.voiceDesign`, which this port's `WaveProject` does not keep).
    pub fn of(p: &WaveProject, voice_design: &VoiceDesign) -> Latest {
        Latest {
            generation: p.generation,
            start_tick: p.start_tick,
            track_serial: p.track_serial,
            gain: p.gain,
            energy: p.energy.clone(),
            duck_depth: p.duck_depth,
            space: p.space,
            echo: p.echo,
            balance: p.balance.clone(),
            song_rests: p.song_rests.clone(),
            rests: p.rests.clone(),
            tail: p.tail,
            feel: p.feel.clone(),
            voice_design: voice_design.clone(),
            channel_voices: p.channel_voices(),
            role_tables: p.graph.role_tables(),
            sampled: p
                .lanes()
                .iter()
                .filter_map(|l| l.sampled_slot().map(|s| (l.net_id.clone(), l.channel, s)))
                .collect(),
        }
    }
}

/// `Deck.sameSongOptions`: `opts` for another start of the song this deck plays.
///
/// The settings of its genre, as `PreparedTrack` prepared it (`forGenre`: the genre itself, the
/// sub-octave...), then the playing project's loudness gain, energy curve, duck depth, room,
/// echo, part trims, rest schedule, groove template and voice design instead of measuring them
/// again, and its mix-out tail (in song ticks, so a jump inside or before it still stops on the
/// same bar line) unless `opts` brings one. `fixes` are the deck's (`Deck.fixes`: what the song
/// was loaded with; a raw-`nets` share's are the runner's only), intersected with `opts`'s
/// before `forGenre` reads them; `genre` is the spec's (a raw-`nets` share has none: its genre's
/// settings are left as they were), `latest` the project loaded last, `palette` the track's
/// (`Deck.netProject.palette`): a palette changed on the same song brings its room and echo.
pub fn same_song_options(
    opts: &WaveOptions,
    fixes: Option<Fixes>,
    genre: Option<&str>,
    latest: Option<&Latest>,
    palette: Option<&str>,
) -> WaveOptions {
    let mut o = opts.clone();
    if let Some(f) = fixes {
        o.musicality = o.musicality & f;
    }
    if let Some(g) = genre {
        o = o.for_genre(g);
    }
    if let Some(l) = latest {
        o.track_gain = Some(l.gain);
        if let Some(e) = &l.energy {
            o.track_energy = Some(e.clone());
        }
        o.duck_depth = l.duck_depth;
        if let Some(s) = l.space {
            o.space = Some(s);
        }
        if let Some(e) = l.echo {
            o.echo = Some(e);
        }
        if let Some(b) = &l.balance {
            o.track_balance = Some(b.clone());
        }
        o.rests = Some(l.song_rests.clone());
        if o.tail.is_none() {
            o.tail = l.tail;
        }
        if let Some(f) = &l.feel {
            o.feel = Some(f.clone());
        }
        o.voice_design = l.voice_design.clone();
    }
    if let Some(g) = genre {
        o = o.for_palette(palette, g);
    }
    o
}

/// `Deck.movedOptions`: [`same_song_options`] with a new mix-out tail and / or one more rest (in
/// song ticks).
///
/// The rest joins the song's schedule at prepare (`WaveOptions::extra_rest`), which is where
/// Swift puts it (`trackRests = song.with(rest:)`): the song's schedule is the one
/// `same_song_options` carries over from the loaded project, so the two are the same schedule.
pub fn moved_options(
    opts: &WaveOptions,
    fixes: Option<Fixes>,
    genre: Option<&str>,
    latest: Option<&Latest>,
    palette: Option<&str>,
    tail: Option<MixOutTail>,
    rest: Option<Entry>,
) -> WaveOptions {
    let mut o = same_song_options(opts, fixes, genre, latest, palette);
    if tail.is_some() {
        o.tail = tail;
    }
    if rest.is_some() {
        o.extra_rest = rest;
    }
    o
}

/// The options `PreparedTrack.restarted(atTick:)` prepares with: `opts` (the track's) and the
/// loudness gain, energy, duck depth, room, echo, part trims and rest schedule `prepared`
/// measured, so nothing is measured again; only the net is replayed to the new tick.
pub fn restarted_options(opts: &WaveOptions, prepared: &WaveProject) -> WaveOptions {
    let mut o = opts.clone();
    o.track_gain = Some(prepared.gain);
    if let Some(e) = &prepared.energy {
        o.track_energy = Some(e.clone());
    }
    o.duck_depth = prepared.duck_depth;
    if let Some(s) = prepared.space {
        o.space = Some(s);
    }
    if let Some(e) = prepared.echo {
        o.echo = Some(e);
    }
    if let Some(b) = &prepared.balance {
        o.track_balance = Some(b.clone());
    }
    o.rests = Some(prepared.rests.clone());
    o
}
