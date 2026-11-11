//! The fixes under performance, ROADMAP 6.7 (`FixesUnderJumpsTests`): every musicality pass and
//! runner fix a track plays with from its top must still play after a section jump, a skip to
//! the next track and a copy onto the other deck. A jump prepares the song again from a tick
//! (`Deck.sameSongOptions`); what continuous play has is the project the song was prepared with,
//! so the jumped project is held to that one, field by field (`divergences`), and two of them
//! are rendered (`loudness`). Form (a): each test names the Swift one; the projects are the
//! ones the audio thread plays (`Renderer::deck(..).runner.project()`), so the fixes are
//! measured on what is sounding, with every fix on (`WaveOptions::extended`).
//!
//! Not in the port, for want of a bank here: the per-sampler-slot gain and width and the
//! sampler pick of `LanePrint` (a deck with no SoundFont binds none; `deck_sampled.rs` holds the
//! sampled lanes), and a lane's `uniWidth` (no accessor).

use djrust::deck::swap::Latest;
use djrust::musicality::space::SpaceSpec;
use djrust::wave::rests::RestSchedule;
use djrust::wave::{Lane, WaveOptions, WaveProject};
use djrust::{Project, TrackSpec};
use djrust::host::deck::{engine, Engine, EngineConfig, PreparedTrack, Renderer};

const RATE: f64 = 22_050.0;
const SEED: f64 = 42.0;

/// The dance family (every genre beats files there), the chill family, and jazz for the brushes.
const DANCE: [&str; 9] = [
    "techno",
    "house",
    "edm",
    "trance",
    "dnb",
    "dubstep",
    "speedcore",
    "garage",
    "trap",
];
const CHILL: [&str; 2] = ["lofi", "ambient"];

fn spec(genre: &str, seed: f64) -> TrackSpec {
    TrackSpec::new(genre, seed, Some("standard"))
}

/// What one lane plays with, beyond the state it runs (`LanePrint`).
#[derive(Clone, Debug, PartialEq)]
struct LanePrint {
    net: String,
    kind: String,
    channel: i64,
    instrument: String,
    voice: djrust::wave::VoiceSpec,
    pan: f64,
    spread: Vec<f64>,
    level: f64,
    kit_velocity: f64,
    tilt: bool,
    sub_octave: bool,
    pluck: bool,
    mip: bool,
    unison: Option<(f64, f64)>,
    filter: (f64, f64, f64),
    ducks: bool,
    is_kick: bool,
    send: f64,
    shimmer: f64,
    echo: f64,
    rest: u32,
    harmony_group: i64,
    choke: f64,
}

impl LanePrint {
    fn of(l: &Lane) -> LanePrint {
        LanePrint {
            net: l.net_id.clone(),
            kind: l.kind.clone(),
            channel: l.channel,
            instrument: l.instrument.clone(),
            voice: l.voice.clone(),
            pan: l.placement().0,
            spread: l.voice_spread().to_vec(),
            level: l.level(),
            kit_velocity: l.kit().2,
            tilt: l.tilt_on(),
            sub_octave: l.has_sub_octave(),
            pluck: l.is_pluck(),
            mip: l.mip_on(),
            unison: l.unison_pair(),
            filter: l.filter_envelope(),
            ducks: l.ducks(),
            is_kick: l.keys_the_duck(),
            send: l.send(),
            shimmer: l.shimmer_send(),
            echo: l.echo_send(),
            rest: l.rest_class().0,
            harmony_group: l.harmony_group(),
            choke: l.choke_mul(),
        }
    }
}

/// A project's play-relevant state, to compare projects that are not alive at once.
struct Print {
    start_tick: i64,
    track_serial: i32,
    gain: f64,
    energy: Option<djrust::musicality::energy_curve::EnergyCurve>,
    duck_depth: f64,
    song_rests: RestSchedule,
    tail: Option<djrust::wave::MixOutTail>,
    harmony: Option<djrust::musicality::section_harmony::SectionHarmony>,
    space: Option<SpaceSpec>,
    echo: Option<djrust::musicality::echo::EchoSpec>,
    balance: Option<std::collections::BTreeMap<String, f64>>,
    feel: Option<djrust::wave::GrooveTemplate>,
    drift: Option<u64>,
    brake: bool,
    riser: bool,
    voice_design: djrust::musicality::synth_voice::VoiceDesign,
    lanes: Vec<LanePrint>,
}

impl Print {
    fn of(p: &WaveProject, voice_design: &djrust::musicality::synth_voice::VoiceDesign) -> Print {
        Print {
            start_tick: p.start_tick,
            track_serial: p.track_serial,
            gain: p.gain,
            energy: p.energy.clone(),
            duck_depth: p.duck_depth,
            song_rests: p.song_rests.clone(),
            tail: p.tail,
            harmony: p.harmony.clone(),
            space: p.space,
            echo: p.echo,
            balance: p.balance.clone(),
            feel: p.feel.clone(),
            drift: p.drift,
            brake: p.turntable_brake,
            riser: p.riser.is_some(),
            voice_design: voice_design.clone(),
            lanes: p.lanes().iter().map(LanePrint::of).collect(),
        }
    }
}

/// The project the audio thread plays on deck A, and what the deck says it was prepared with.
fn playing(e: &Engine, r: &mut Renderer) -> Print {
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    r.render_block(&mut l, &mut rr); // the load goes in
    let latest = e.deck("A").unwrap().track.latest.clone().unwrap();
    let p = r.deck("A").unwrap().runner.project().unwrap();
    assert_eq!(
        p.generation, latest.generation,
        "the deck plays what it loaded last"
    );
    Print::of(p, &latest.voice_design)
}

/// `divergences(_:_:)`: the classes of fix in which `jumped` plays differently from `song` (the
/// same song, another start), each with what differs. Empty: the jump plays every fix the song
/// does.
fn divergences(jumped: &Print, song: &Print) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut check = |name: &str, same: bool, detail: String| {
        if !same {
            out.push(if detail.is_empty() {
                name.to_string()
            } else {
                format!("{name}: {detail}")
            });
        }
    };
    // loudness +- 0.5 dB (10^(0.5/20))
    let ratio = jumped.gain / song.gain.max(1e-9);
    check(
        "loudness",
        ratio < 1.0593 && ratio > 1.0 / 1.0593,
        format!("{} vs {}", jumped.gain, song.gain),
    );
    check("energy", jumped.energy == song.energy, String::new());
    check("duck", jumped.duck_depth == song.duck_depth, String::new());
    check("rests", jumped.song_rests == song.song_rests, String::new());
    check("tail", jumped.tail == song.tail, String::new());
    check(
        "section harmony",
        jumped.harmony == song.harmony,
        String::new(),
    );
    check(
        "room",
        jumped.space == song.space,
        format!("{:?}", jumped.space),
    );
    check("echo", jumped.echo == song.echo, String::new());
    check(
        "trims",
        jumped.balance == song.balance,
        format!("{:?} vs {:?}", jumped.balance, song.balance),
    );
    check("feel", jumped.feel == song.feel, String::new());
    check("drift", jumped.drift == song.drift, String::new());
    check(
        "voice design",
        jumped.voice_design == song.voice_design,
        String::new(),
    );
    check("brake", jumped.brake == song.brake, String::new());
    check("riser", jumped.riser == song.riser, String::new());
    if jumped.lanes.len() != song.lanes.len() {
        out.push(format!(
            "lanes: {} vs {}",
            jumped.lanes.len(),
            song.lanes.len()
        ));
        return out;
    }
    for (x, y) in jumped.lanes.iter().zip(&song.lanes) {
        if x == y {
            continue;
        }
        let mut what: Vec<&str> = Vec::new();
        if x.net != y.net || x.kind != y.kind || x.channel != y.channel {
            what.push("lane");
        }
        if x.instrument != y.instrument {
            what.push("sampler pick");
        }
        if x.voice != y.voice {
            what.push(match y.kind.as_str() {
                "kick" => "kick tune",
                "snare" => "brushes",
                _ => "voice",
            });
        }
        if x.pan != y.pan || x.spread != y.spread {
            what.push("pans");
        }
        if x.level != y.level || x.kit_velocity != y.kit_velocity {
            what.push("trims");
        }
        if x.tilt != y.tilt {
            what.push("hf tilt");
        }
        if x.sub_octave != y.sub_octave {
            what.push("sub-octave");
        }
        if x.pluck != y.pluck || x.mip != y.mip || x.unison != y.unison || x.filter != y.filter {
            what.push("sound design");
        }
        if x.ducks != y.ducks || x.is_kick != y.is_kick {
            what.push("duck");
        }
        if x.send != y.send || x.shimmer != y.shimmer || x.echo != y.echo {
            what.push("sends");
        }
        if x.rest != y.rest {
            what.push("gaps / rests");
        }
        if x.harmony_group != y.harmony_group {
            what.push("section harmony");
        }
        if x.choke != y.choke {
            what.push("choke");
        }
        out.push(format!("{}/{}: {}", y.net, y.kind, what.join(", ")));
    }
    out
}

/// Jump deck A into each of the song's first three sections and compare each jump with the
/// project its song started with (`jumpDivergences`). Returns every divergence, by section.
fn jump_divergences(e: &mut Engine, r: &mut Renderer, song: &Print) -> Vec<String> {
    let mut out = Vec::new();
    let secs = e.deck("A").unwrap().sections.clone();
    for (index, sec) in secs.iter().enumerate().take(3) {
        if !e.jump_to_section("A", index) {
            out.push(format!("section {index}: did not jump"));
            continue;
        }
        let jumped = playing(e, r);
        if jumped.start_tick != sec.start_tick || jumped.track_serial != song.track_serial {
            out.push(format!("section {index}: not the same song at its start"));
        }
        out.extend(
            divergences(&jumped, song)
                .into_iter()
                .map(|d| format!("section {index} {}: {d}", sec.name)),
        );
    }
    out
}

// FixesUnderJumpsTests.sectionJumpsKeepEveryFix: every fix the song plays from its top, a jump
// into each of its first three sections plays too: 12 genres, dance and chill among them.
#[test]
fn section_jumps_keep_every_fix() {
    let genres: Vec<&str> = DANCE
        .iter()
        .chain(&CHILL)
        .chain(&["jazz"])
        .copied()
        .collect();
    for genre in genres {
        let (mut e, mut r) = engine(EngineConfig::new(RATE));
        assert!(e.generate("A", &spec(genre, SEED)), "{genre}");
        let song = playing(&e, &mut r);
        assert!(e.deck("A").unwrap().sections.len() >= 3, "{genre}");
        // the song really plays the fixes the jumps are held to
        assert!(
            song.gain > 0.0 && song.gain != 1.0 && song.balance.is_some(),
            "{genre}"
        );
        let found = jump_divergences(&mut e, &mut r, &song);
        assert!(found.is_empty(), "{genre}: {}", found.join("; "));
    }
}

// FixesUnderJumpsTests.sameSongOptionsCarryTheGenre: the jump options themselves: the genre's
// own settings, as `PreparedTrack` prepared the song.
#[test]
fn same_song_options_carry_the_genre() {
    for genre in ["jazz", "dnb", "techno", "lofi"] {
        let (mut e, _r) = engine(EngineConfig::new(RATE));
        assert!(e.generate("A", &spec(genre, SEED)));
        let o = e.move_request("A", 0, None, None).unwrap().opts;
        let fresh = WaveOptions::extended().for_genre(genre);
        assert_eq!(o.genre.as_deref(), Some(genre));
        assert_eq!(o.brushes, fresh.brushes, "{genre}");
        assert_eq!(o.sub_octave, fresh.sub_octave, "{genre}");
        assert_eq!(o.hf_tilt, fresh.hf_tilt, "{genre}");
        assert_eq!(o.feel, fresh.feel, "{genre}");
        assert_eq!(o.voice_design, fresh.voice_design, "{genre}");
        assert_eq!(o.duck_depth, fresh.duck_depth, "{genre}");
        if genre == "jazz" {
            assert!(o.brushes);
        }
        if genre == "dnb" {
            assert!(o.sub_octave);
        }
        // and what the song measured rides with them
        let latest: Latest = e.deck("A").unwrap().track.latest.clone().unwrap();
        assert_eq!(o.track_gain, Some(latest.gain));
        assert_eq!(o.track_balance, latest.balance);
        assert_eq!(o.rests.as_ref(), Some(&latest.song_rests));
    }
}

/// The RMS of deck A playing for `seconds`, dB, the master's output (`rmsDB(renderOffline)`).
fn rms_db(e: &mut Engine, r: &mut Renderer, seconds: f64) -> f64 {
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    e.play("A");
    let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
    let blocks = (seconds * RATE / 128.0) as usize;
    let mut sum = 0f64;
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        for (a, b) in l.iter().zip(&rr) {
            sum += *a as f64 * *a as f64 + *b as f64 * *b as f64;
        }
    }
    10.0 * (sum / (2 * blocks * 128) as f64).max(1e-30).log10()
}

// FixesUnderJumpsTests.jumpsRenderAsLoudAsTheSong: a jump plays as loud as the song does from
// the same tick (+- 0.5 dB, rendered): the song restarted there as a prepared track
// (`PreparedTrack.restarted`, its own options) on one engine, the deck's section jump on another.
#[test]
fn jumps_render_as_loud_as_the_song() {
    for genre in ["techno", "lofi"] {
        for index in 0..3 {
            // fresh engines for every section: both decks start stopped, so each load is at once
            let (mut reference, mut rr) = engine(EngineConfig::new(RATE));
            let (mut live, mut lr) = engine(EngineConfig::new(RATE));
            assert!(live.generate("A", &spec(genre, SEED)));
            let secs = live.deck("A").unwrap().sections.clone();
            let Some(sec) = secs.get(index) else { continue };
            let track =
                PreparedTrack::compose(&spec(genre, SEED), &reference.preparer("A").unwrap(), 0);
            let again = track.restarted(&reference.preparer("A").unwrap(), sec.start_tick);
            assert!(reference.load_net("A", again));
            assert!(live.jump_to_section("A", index));
            let da = rms_db(&mut reference, &mut rr, 6.0);
            let db = rms_db(&mut live, &mut lr, 6.0);
            assert!(
                da > -60.0,
                "{genre} section {index}: the reference plays: {da}"
            );
            assert!(
                (da - db).abs() < 0.5,
                "{genre} section {index}: {da} vs {db} dB"
            );
        }
    }
}

// FixesUnderJumpsTests.skipToNextTrackKeepsEveryFix: the next track handed over by the skip
// button plays every fix a fresh prepare of it does, and so do jumps inside it.
#[test]
fn skip_to_next_track_keeps_every_fix() {
    for genre in ["techno", "lofi"] {
        let (mut e, mut r) = engine(EngineConfig::new(RATE));
        assert!(e.generate("A", &spec(genre, SEED)));
        e.queue_next("A", &spec(genre, SEED + 1.0));
        e.skip_to_next_track("A");
        let next = playing(&e, &mut r);
        let next_spec = e.deck("A").unwrap().spec.clone().unwrap();
        assert_eq!(next_spec.seed, SEED + 1.0);
        let fresh = PreparedTrack::compose(&next_spec, &e.preparer("A").unwrap(), 0);
        let fresh_print = Print::of(fresh.prepared.wave(), &fresh.opts.voice_design);
        let found = divergences(&next, &fresh_print);
        assert!(found.is_empty(), "{genre} skip: {}", found.join("; "));
        let jumps = jump_divergences(&mut e, &mut r, &next);
        assert!(
            jumps.is_empty(),
            "{genre} skip, then jumps: {}",
            jumps.join("; ")
        );
    }
}

// FixesUnderJumpsTests.copyDeckKeepsEveryFix: a deck copied onto the other (`copyDeck`: its
// source prepared on that deck's bank) plays every fix the original does, and so do jumps inside
// the copy.
#[test]
fn copy_deck_keeps_every_fix() {
    for genre in ["techno", "lofi"] {
        let (mut e, mut r) = engine(EngineConfig::new(RATE));
        assert!(e.generate("A", &spec(genre, SEED)));
        let song = playing(&e, &mut r);
        let source = e.net_source("A").unwrap();
        let copy = match source {
            djrust::session::model::DeckSource::Net(s) => {
                PreparedTrack::compose(&s, &e.preparer("B").unwrap(), 0)
            }
            other => panic!("{other:?}"),
        };
        assert!(e.load_net("B", copy));
        let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
        r.render_block(&mut l, &mut rr);
        let latest = e.deck("B").unwrap().track.latest.clone().unwrap();
        let copied = Print::of(
            r.deck("B").unwrap().runner.project().unwrap(),
            &latest.voice_design,
        );
        let found = divergences(&copied, &song);
        assert!(found.is_empty(), "{genre} copy: {}", found.join("; "));
        // jumps inside the copy
        let secs = e.deck("B").unwrap().sections.clone();
        for (index, sec) in secs.iter().enumerate().take(3) {
            assert!(e.jump_to_section("B", index));
            r.render_block(&mut l, &mut rr);
            let latest = e.deck("B").unwrap().track.latest.clone().unwrap();
            let jumped = Print::of(
                r.deck("B").unwrap().runner.project().unwrap(),
                &latest.voice_design,
            );
            assert_eq!(jumped.start_tick, sec.start_tick);
            let found = divergences(&jumped, &copied);
            assert!(
                found.is_empty(),
                "{genre} copy, then jump {index}: {}",
                found.join("; ")
            );
        }
    }
}

/// The projects of a song's own two starts are the same song: the project is the played
/// composition, so a jump does not compose anything.
#[test]
fn a_jump_is_the_same_project_prepared_again() {
    let (mut e, mut r) = engine(EngineConfig::new(RATE));
    assert!(e.generate("A", &spec("techno", SEED)));
    let before: Project = e.deck("A").unwrap().project.clone().unwrap();
    let _ = playing(&e, &mut r);
    assert!(e.jump_to_section("A", 1));
    let after = e.deck("A").unwrap().project.clone().unwrap();
    assert_eq!(after.canonical_json(), before.canonical_json());
}
