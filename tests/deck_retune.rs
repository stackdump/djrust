//! `LoopRetuneTests` (Swift suite, form (a)): a net take rendered again from its provenance
//! (`ClipRetune.render`, `djrust::host::deck::retune`) and the action that puts the render in the clip's
//! place (`Engine::retune_clip`, `DJEngine.retuneClip`). The decks here play synth voices on every
//! tonal channel (instruments with no sampled category; the engine has no library): sampled sounds
//! with round robin differ between any two renders, synth voices never do.
//!
//! Swift's `ClipRetuneTestsProvenanceDepth` and the round trip of the action
//! (`theRetuneActionRoundTrips`) are held elsewhere: the action's JSON, summary, clip ids and
//! fold key in `swift/sessions.json` (`tests/session_golden.rs`), the provenance's shape in
//! `swift/loopclips.json` (`tests/loopclips_golden.rs`). The render itself is also compared with
//! Swift's render of the same takes: `tests/retune_golden.rs`.

mod common_row;

use std::collections::BTreeMap;
use std::sync::Arc;

use common_row::*;
use djrust::deck::clips::{crop_clip, LoopClip};
use djrust::deck::loops::{LoopState, Refusal};
use djrust::deck::provenance;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::voices::RoleVoice;
use djrust::json::{JsonValue, JsonObject};
use djrust::session::action::DeckAction as A;
use djrust::session::model::DeckSource;
use djrust::session::types::{LoopFx, LoopFxMode, SequenceStep};
use djrust::wander::carry::project_drum_channels;
use djrust::wave::synth::tonal_voice;
use djrust::TrackSpec;
use djrust::host::deck::retune::{self, RetuneEnv};
use djrust::host::deck::PerformCtx;

/// Techno at 120 with every tonal channel on a plain synth voice ("saw": no sampled category).
fn synth_spec() -> TrackSpec {
    let mut spec = TrackSpec::new("techno", 1.0, None);
    spec.extra.set("bpm", JsonValue::Number(120.0));
    let project = spec.project();
    let drums = project_drum_channels(&project);
    let mut tonal: Vec<i64> = project
        .nets
        .iter()
        .filter(|(_, nb)| nb.role != "control")
        .map(|(_, nb)| nb.track.channel)
        .filter(|c| !drums.contains(c))
        .collect();
    tonal.sort_unstable();
    tonal.dedup();
    let mut source = DeckSource::Net(spec.clone());
    for ch in tonal {
        source = source.with_instrument("saw", ch).unwrap_or(source);
    }
    match source {
        DeckSource::Net(s) => s,
        _ => spec,
    }
}

/// The difference of `a` and `b` against `b`'s level, dB (both channels) (`nullDB`).
fn null_db(a: &SampleBuffer, b: &SampleBuffer) -> f64 {
    let (mut diff, mut reference) = (0.0f64, 0.0f64);
    for i in 0..a.length().min(b.length()) {
        let dl = (a.left[i] - b.left[i]) as f64;
        let dr = (a.right()[i] - b.right()[i]) as f64;
        diff += dl * dl + dr * dr;
        reference += b.left[i] as f64 * b.left[i] as f64 + b.right()[i] as f64 * b.right()[i] as f64;
    }
    10.0 * (diff.max(1e-30) / reference.max(1e-30)).log10()
}

fn env(rig: &Rig) -> RetuneEnv {
    rig.e.retune_env()
}

fn render(rig: &Rig, clip: &LoopClip, voices: &BTreeMap<i64, String>) -> retune::Retuned {
    retune::render(clip, voices, SR, &env(rig)).expect("it renders")
}

/// A two-bar take from bar 1 of the synth-voiced deck, on the row of a rig still running.
fn net_rig() -> (Rig, LoopClip, Vec<RoleVoice>) {
    let mut rig = Rig::app_deck_a();
    assert!(rig.e.generate("A", &synth_spec()));
    rig.e.play("A");
    rig.run_to(BAR + BAR / 3);
    assert!(rig.e.loop_record("A", true, None));
    rig.run_to(2 * BAR + BAR / 2);
    assert!(rig.e.loop_record("A", false, None));
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap().clone();
    assert!(clip.start_beat == 4.0 && clip.beats == 8, "{} {}", clip.start_beat, clip.beats);
    let roles = rig.e.role_voices("A");
    (rig, clip, roles)
}

fn net_take() -> (Rig, LoopClip, Vec<RoleVoice>) {
    let (mut rig, clip, roles) = net_rig();
    rig.e.stop("A");
    (rig, clip, roles)
}

/// A tonal part on a synth voice, and another instrument for it. Only a part whose wave the
/// instrument decides: beats' wave engine voices every bass-group lane as a saw and every
/// harmony lane as its soft pad wave whatever the instrument (`tonalVoice`), so "square" on the
/// bass or the pad renders the same bytes and there would be nothing to retune.
fn retunable_part(roles: &[RoleVoice]) -> Option<&RoleVoice> {
    roles.iter().find(|r| {
        !r.role.contains(" · ")
            && r.instrument == "saw"
            && tonal_voice(Some("square"), Some(&r.role)).wave
                != tonal_voice(Some("saw"), Some(&r.role)).wave
    })
}

fn retunable(roles: &[RoleVoice]) -> (RoleVoice, BTreeMap<i64, String>) {
    let part = retunable_part(roles).expect("a retunable part").clone();
    let voices = BTreeMap::from([(part.channel, "square".to_string())]);
    (part, voices)
}

fn none() -> BTreeMap<i64, String> {
    BTreeMap::new()
}

// LoopRetuneTests.theSameVoicesNullAgainstTheTake
#[test]
fn the_same_voices_null_against_the_take() {
    let (rig, clip, _) = net_take();
    let again = render(&rig, &clip, &none());
    assert_eq!(again.buffer.length(), clip.buffer.length(), "exactly the take's beats");
    let db = null_db(&again.buffer, &clip.buffer);
    println!("retune null, same voices: {db} dB");
    assert!(db <= -40.0, "{db} dB");
    assert_eq!(Some(again.source), clip.origin_source());
}

// LoopRetuneTests.aCropRendersItsOwnBeats (the crop as `cropClip` makes it: beats 2..<6)
#[test]
fn a_crop_renders_its_own_beats() {
    let (rig, clip, _) = net_take();
    let crop = crop_clip(&clip, 2, 6).unwrap();
    let again = render(&rig, &crop, &none());
    assert_eq!(again.buffer.length(), crop.buffer.length());
    let db = null_db(&again.buffer, &crop.buffer);
    assert!(db <= -40.0, "{db} dB");
}

// LoopRetuneTests.anotherInstrumentChangesOnlyItsPart
#[test]
fn another_instrument_changes_only_its_part() {
    let (rig, clip, roles) = net_take();
    let (part, voices) = retunable(&roles);
    let changed = render(&rig, &clip, &voices);
    let db = null_db(&changed.buffer, &clip.buffer);
    println!("retune null, {} on square: {db} dB", part.role);
    assert!(db > -30.0, "the part sounds different: {db} dB");
    // with that part left out of both, the rest of the mix is the take's
    let off = [part.role.clone()];
    let rest = retune::render_roles_off(&clip, &voices, SR, &env(&rig), &off).unwrap();
    let rest_as_taken = retune::render_roles_off(&clip, &none(), SR, &env(&rig), &off).unwrap();
    let rest_db = null_db(&rest.buffer, &rest_as_taken.buffer);
    println!("retune null without {}: {rest_db} dB", part.role);
    assert!(rest_db <= -40.0, "{rest_db} dB");
    // the override is the share's own `tracks` entry
    assert_eq!(
        Some(changed.source),
        clip.origin_source()
            .and_then(|s| s.with_instrument("square", part.channel))
    );
}

// LoopRetuneTests.sampleTakesAreRefused
#[test]
fn sample_takes_are_refused() {
    let mut rig = Rig::app_deck_a();
    assert!(rig.e.load_demo("A"));
    rig.e.play("A");
    rig.run_to(BAR + BAR / 3);
    assert!(rig.e.loop_record("A", true, None));
    rig.run_to(2 * BAR + BAR / 2);
    assert!(rig.e.loop_record("A", false, None));
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap().clone();
    let refused = |c: &LoopClip| retune::render(c, &none(), SR, &env(&rig)).err();
    assert_eq!(refused(&clip), Some(Refusal::NotANet));
    // and so is a crop of one
    let crop = crop_clip(&clip, 0, 4).unwrap();
    assert_eq!(refused(&crop), Some(Refusal::NotANet));
}

// LoopRetuneTests.sequencesMacrosAndBareClipsAreRefused
#[test]
fn sequences_macros_and_bare_clips_are_refused() {
    let (rig, clip, _) = net_take();
    let like = |id: &str, provenance: Option<JsonValue>| {
        let mut c = LoopClip::new(
            id,
            id,
            "A",
            clip.beats,
            clip.bpm,
            clip.start_beat,
            clip.buffer.clone(),
            clip.waveform.clone(),
        );
        c.provenance = provenance;
        c
    };
    let refused = |c: &LoopClip| retune::render(c, &none(), SR, &env(&rig)).err();
    let mut seq = like(
        "seq",
        Some(provenance::sequence(&[(SequenceStep::new(&clip.id, 1), &clip)], "2026-01-01T00:00:00Z")),
    );
    seq.steps = Some(vec![SequenceStep::new(&clip.id, 1)]);
    assert_eq!(refused(&seq), Some(Refusal::Sequence));
    // a take with a macro running
    let mut o: JsonObject = clip.provenance.as_ref().unwrap().as_object().unwrap().clone();
    o.set(
        "macros",
        JsonValue::Array(vec![JsonValue::str("beatRepeat")]),
    );
    let with_macro = like("m", Some(JsonValue::Object(o)));
    assert_eq!(
        refused(&with_macro),
        Some(Refusal::Macros(vec!["beatRepeat".to_string()]))
    );
    assert_eq!(refused(&like("b", None)), Some(Refusal::NoProvenance));
}

fn ctx(buffer: &SampleBuffer) -> PerformCtx {
    PerformCtx {
        prepared: None,
        buffer: Some(Arc::new(buffer.clone())),
    }
}

// LoopRetuneTests.aRetunedClipTakesItsPlaceAndKeepsItsSettings (the action,
// DJEngine.retuneClip): the retuned clip takes the take's place and keeps its settings: level,
// colour, FX, key lock, its scenes, and it carries on looping. Its provenance says what changed.
#[test]
fn a_retuned_clip_takes_its_place_and_keeps_its_settings() {
    let (mut rig, clip, roles) = net_rig();
    let (part, voices) = retunable(&roles);
    // something before it on the row, so its place is not just "first"
    rig.e.perform(
        &A::ClipSequence {
            steps: vec![SequenceStep::new(&clip.id, 2)],
            replacing: None,
        },
        PerformCtx::default(),
    );
    rig.e.perform(
        &A::ClipMove {
            id: clip.id.clone(),
            before: None,
        },
        PerformCtx::default(),
    );
    let clips = &rig.e.loops.row.clips;
    let index = clips.iter().position(|c| c.id == clip.id).expect("on the row");
    assert_eq!(index, clips.len() - 1);
    let fx = LoopFx {
        drive: 6.0,
        mode: LoopFxMode::Tube,
        ..LoopFx::default()
    };
    let mut d = |a: A| {
        rig.e.perform(&a, PerformCtx::default());
    };
    d(A::ClipGain { id: clip.id.clone(), value: 0.4 });
    d(A::ClipColor { id: clip.id.clone(), color: Some("teal".into()) });
    d(A::ClipFx { id: clip.id.clone(), fx: fx.clone() });
    d(A::ClipKeyLock { id: clip.id.clone(), on: true });
    d(A::SceneSave { name: "one".into() });
    let state = rig.e.loop_state(&clip.id);
    assert!(matches!(state, LoopState::On | LoopState::Starting), "the take loops under the mixer");
    rig.run(BAR / 2);

    let rendered = render(&rig, &clip, &voices);
    rig.e.perform(
        &A::ClipRetune { id: clip.id.clone(), voices: voices.clone() },
        ctx(&rendered.buffer),
    );
    let id = rig.e.loops.row.clips[index].id.clone();
    assert!(id != clip.id && id == rendered.buffer.content_id(), "in the take's place");
    assert!(rig.e.clip(&clip.id).is_none());
    let new = rig.e.clip(&id).expect("the retuned clip").clone();
    assert!(new.beats == clip.beats && new.bpm == clip.bpm && new.start_beat == clip.start_beat);
    assert!(new.title == clip.title && new.color.as_deref() == Some("teal") && new.fx == fx);
    assert!(rig.e.clip_gain(&id) == 0.4 && rig.e.is_key_locked(&id));
    assert!(
        matches!(rig.e.loop_state(&id), LoopState::On | LoopState::Starting),
        "it carries on"
    );
    assert!(
        rig.e.scene("one").unwrap().clips.iter().any(|c| c.clip_id == id),
        "its scene has it"
    );
    let first_steps: Vec<String> = rig.e.loops.row.clips[0]
        .steps
        .as_ref()
        .map(|s| s.iter().map(|x| x.clip_id.clone()).collect())
        .unwrap_or_default();
    assert_eq!(first_steps, vec![id.clone()], "the sequence uses it from now on");
    // provenance: what it was made from, and the instruments
    let p = new.provenance.as_ref().unwrap();
    assert_eq!(p.get("op").and_then(JsonValue::as_str), Some("retune"));
    assert_eq!(p.get("from").and_then(JsonValue::as_str), Some(clip.id.as_str()));
    assert_eq!(new.retuned_voices(), voices);
    assert_eq!(
        new.origin_source(),
        clip.origin_source()
            .and_then(|s| s.with_instrument("square", part.channel))
    );
    assert!(new.origin_section() == clip.origin_section() && new.origin_bpm() == clip.origin_bpm());
    // the instruments a retune offers now show the new one
    let offered = retune::voices_for(&new, &env(&rig));
    assert_eq!(
        offered
            .iter()
            .find(|v| v.channel == part.channel)
            .map(|v| v.instrument.as_str()),
        Some("square")
    );
    rig.run(BAR);
}

/// `ClipRetune.voices(for:)` calls `spec.project()`, which resolves a palette's sounds against
/// what the Mac has installed, so "the instrument it plays now" is the pick that machine makes.
/// Rust-only (the Swift tests of `voices(for:)` have no palette'd clip: `LoopRetuneTests` line
/// 271 holds a synth-voiced one): the offered instruments must agree with `project_in(sounds)`
/// per role, once with nothing installed and once with the floor bank.
#[test]
fn the_offered_instruments_are_the_ones_the_installed_sounds_pick() {
    use djrust::sound::{Installed, Sounds};
    use djrust::voices::VoiceCatalog;
    use djrust::host::deck::wander::InstalledSounds;

    // a take of a palette'd track, as the app would record one (nothing installed on this rig)
    let mut spec =
        TrackSpec::new("techno", 1.0, None).with_palette(Some("synthwave"), &Installed::default());
    spec.extra.set("bpm", JsonValue::Number(120.0));
    let mut rig = Rig::app_deck_a();
    assert!(rig.e.generate("A", &spec));
    rig.e.play("A");
    rig.run_to(BAR + BAR / 3);
    assert!(rig.e.loop_record("A", true, None));
    rig.run_to(2 * BAR + BAR / 2);
    assert!(rig.e.loop_record("A", false, None));
    let id = rig.run_until_clip(4 * BAR).expect("a take");
    let clip = rig.e.clip(&id).unwrap().clone();
    assert_eq!(clip.origin_source(), Some(DeckSource::Net(spec.clone())), "the take names the palette'd spec");

    let floor = Installed {
        dir: "/golden/floor".into(),
        files: vec!["MuseScore_General.sf2".into()],
        ..Installed::default()
    };
    let nothing = RetuneEnv::default();
    let here = RetuneEnv {
        installed: Some(InstalledSounds::new(floor.clone())),
        #[cfg(feature = "sf2")]
        library: None,
    };
    // per role: what the project plays on that channel with those sounds
    let instrument_of = |sounds: Sounds, voice: &RoleVoice| -> String {
        let project = spec.project_in(sounds);
        let found = project
            .nets
            .iter()
            .find(|(_, nb)| nb.track.channel == voice.channel && nb.role != "control")
            .map(|(_, nb)| nb.track.instrument.clone())
            .unwrap_or_default();
        found
    };
    let bare = retune::voices_for(&clip, &nothing);
    let offered = retune::voices_for(&clip, &here);
    assert!(!bare.is_empty() && bare.len() == offered.len());
    let catalog = VoiceCatalog::with_installed(Default::default(), floor.sound_fonts());
    let installed_sounds = Sounds { catalog: &catalog, installed: &floor };
    for v in &bare {
        assert_eq!(v.instrument, instrument_of(Sounds::nothing(), v), "{}: nothing installed", v.role);
    }
    for v in &offered {
        assert_eq!(v.instrument, instrument_of(installed_sounds, v), "{}: the floor bank", v.role);
    }
    assert!(
        bare.iter().zip(&offered).any(|(a, b)| a.instrument != b.instrument),
        "a palette that picks differently here: {bare:?} vs {offered:?}"
    );
    // and the engine's own view, with the same library, answers like the env does
    rig.e.installed = Some(InstalledSounds::new(floor));
    assert_eq!(rig.e.clip_voices(&id), offered);
}

// LoopRetuneTests.aReplayRendersTheSameRetune: a replay has no render handed over:
// `applyAction` renders the same bars, and with synth voices it is the same audio, so the retuned
// clip has the live one's id.
#[test]
fn a_replay_renders_the_same_retune() {
    let (mut rig, clip, roles) = net_take();
    let (_, voices) = retunable(&roles);
    let live = render(&rig, &clip, &voices);
    rig.e.apply_loop_action(
        &A::ClipRetune { id: clip.id.clone(), voices: voices.clone() },
        None,
    );
    assert_eq!(
        rig.e.loops.row.clips.last().map(|c| c.id.clone()),
        Some(live.buffer.content_id())
    );
    // a clip that isn't on the row, or one ClipRetune refuses, changes nothing
    let row: Vec<String> = rig.e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
    assert_eq!(rig.e.retune_clip("nope", &voices, None), None);
    rig.e.perform(
        &A::ClipSequence {
            steps: vec![SequenceStep::new(&row[0], 2)],
            replacing: None,
        },
        PerformCtx::default(),
    );
    let seq = rig.e.loops.row.clips.last().unwrap().id.clone();
    assert_eq!(rig.e.retune_clip(&seq, &voices, None), None);
    let mut expected = row.clone();
    expected.push(seq);
    let now: Vec<String> = rig.e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
    assert_eq!(now, expected);
}

// LoopRetuneTests.retunesStackThroughCrops: a retune of a retuned clip keeps the first one's
// instruments under its own, and so does a crop of it.
#[test]
fn retunes_stack_through_crops() {
    let (mut rig, clip, roles) = net_take();
    let (part, voices) = retunable(&roles);
    let other = roles
        .iter()
        .find(|r| !r.role.contains(" · ") && r.channel != part.channel)
        .expect("another part");
    let swap = other
        .options
        .iter()
        .find(|o| **o != other.instrument)
        .expect("another instrument")
        .clone();
    let first = rig.e.retune_clip(&clip.id, &voices, None).expect("first retune");
    let second = rig
        .e
        .retune_clip(&first, &BTreeMap::from([(other.channel, swap.clone())]), None)
        .expect("second retune");
    let c = rig.e.clip(&second).unwrap().clone();
    assert_eq!(
        c.retuned_voices(),
        BTreeMap::from([(part.channel, "square".to_string()), (other.channel, swap)])
    );
    let cropped = rig.e.crop_clip(&second, 0, 4).expect("a crop");
    assert_eq!(rig.e.clip(&cropped).unwrap().retuned_voices(), c.retuned_voices());
    // the newest retune of a channel stands
    let third = rig
        .e
        .retune_clip(&cropped, &BTreeMap::from([(part.channel, "saw".to_string())]), None)
        .expect("third retune");
    let t = rig.e.clip(&third).unwrap();
    assert_eq!(t.retuned_voices().get(&part.channel).map(String::as_str), Some("saw"));
    // how many ops deep the provenance goes (retune, crop, retune, retune, take = 4)
    let mut depth = 0;
    let mut p = t.provenance.as_ref();
    while p.and_then(|v| v.get("op")).and_then(JsonValue::as_str).is_some() {
        depth += 1;
        p = p.and_then(|v| v.get("parent"));
    }
    assert_eq!(depth, 4);
}

// The render with sampled instruments: the render's own bank loads them (`WaveDeckSource` makes
// the bank it plays through), so a take on a sampler plays its sound again, the same bytes every
// time, and another instrument on a sampled channel changes the audio. (Swift's decks in
// `LoopRetuneTests` are synth-voiced by design, since sampled sounds with round robin differ
// between renders; a SoundFont's do not.) On MuseScore_General.sf2 when present.
#[cfg(feature = "sf2")]
#[test]
fn a_retune_on_a_bank_plays_its_sampled_instruments() {
    use djrust::json::JsonObject;
    use djrust::host::sampler::{sound_font_dir, Library};

    let dir = sound_font_dir();
    if !dir.join("MuseScore_General.sf2").exists() {
        eprintln!("SKIPPED: no MuseScore_General.sf2 in {} (make soundfonts)", dir.display());
        return;
    }
    let env = RetuneEnv {
        installed: None,
        library: Some(Arc::new(Library::scan(&dir))),
    };
    let mut spec = TrackSpec::new("country", 7.0, Some("standard"));
    spec.extra.set("bpm", JsonValue::Number(120.0));
    let source = DeckSource::Net(spec);
    let take = JsonValue::Object(JsonObject::from_pairs([
        ("mode", JsonValue::str("net")),
        ("source", source.json()),
        (
            "tempo",
            JsonValue::Object(JsonObject::from_pairs([("bpm", JsonValue::Number(120.0))])),
        ),
    ]));
    let buffer = SampleBuffer::new(vec![0.0; 480], None, 24_000.0);
    let mut clip = LoopClip::new(
        "take",
        "take",
        "A",
        8,
        120.0,
        4.0,
        Arc::new(buffer.clone()),
        djrust::deck::waveform::WaveformOverview::new(&buffer, 4),
    );
    clip.provenance = Some(take);
    let a = retune::render(&clip, &none(), 24_000.0, &env).expect("it renders");
    let b = retune::render(&clip, &none(), 24_000.0, &env).expect("it renders again");
    assert!(a.buffer.left.iter().any(|v| *v != 0.0), "it sounds");
    assert!(a.buffer == b.buffer, "the same bytes every time");
    // a sampled melody on another instrument
    let project = match &source {
        DeckSource::Net(s) => s.project(),
        _ => unreachable!(),
    };
    let melody = project
        .nets
        .iter()
        .find(|(_, nb)| nb.role != "control" && nb.riff_group == "melody")
        .map(|(_, nb)| nb.track.channel)
        .expect("a melody");
    let other = retune::render(&clip, &BTreeMap::from([(melody, "square".to_string())]), 24_000.0, &env)
        .expect("a retuned render");
    assert!(other.buffer != a.buffer, "another instrument sounds different");
}
