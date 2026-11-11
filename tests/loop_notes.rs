//! A loop's notes (`djrust::host::deck::loop_notes`, docs/LOOP-ROUTING.md): the notes had again from a
//! take's provenance are the notes its deck played while the take was recorded, and a clip with
//! no net behind it has none.

mod common_row;

use std::collections::BTreeMap;

use common_row::*;
use djrust::deck::loop_route::{no_notes, plan, Dest, LoopNotes, DRUMS};
use djrust::deck::loops::Refusal;
use djrust::json::JsonValue;
use djrust::TrackSpec;
use djrust::host::deck::loop_notes;
use djrust::host::deck::retune::RetuneEnv;

/// What the deck played on each step of the take: (step in the take, channel, note) → count.
type Played = BTreeMap<(i64, u8, u8), usize>;

/// A two-bar take from bar 1 of a techno deck at 120 (beats 4..12), and every note the deck
/// fired on those bars while it was taken.
fn take_with_fires(genre: &str, seed: f64) -> (Rig, String, Played) {
    let mut rig = Rig::app_deck_a();
    let mut spec = TrackSpec::new(genre, seed, None);
    spec.extra.set("bpm", JsonValue::Number(120.0));
    assert!(rig.e.generate("A", &spec));
    rig.r.deck_mut("A").unwrap().runner.record_events = true;
    rig.e.play("A");
    let mut fires = Vec::new();
    let mut drain = |rig: &mut Rig| {
        fires.extend(rig.r.deck_mut("A").unwrap().runner.fires.drain(..));
        rig.r.deck_mut("A").unwrap().runner.anchors.clear();
    };
    while rig.frame() < BAR + BAR / 3 {
        rig.run(BLOCK as i64);
        drain(&mut rig);
    }
    assert!(rig.e.loop_record("A", true, None));
    while rig.frame() < 2 * BAR + BAR / 2 {
        rig.run(BLOCK as i64);
        drain(&mut rig);
    }
    assert!(rig.e.loop_record("A", false, None));
    let count = rig.e.loops.row.clips.len();
    while rig.e.loops.row.clips.len() == count && rig.frame() < 5 * BAR {
        rig.run(BLOCK as i64);
        drain(&mut rig);
    }
    let id = rig.e.loops.row.clips.last().expect("a take").id.clone();
    let clip = rig.e.clip(&id).unwrap();
    assert_eq!((clip.start_beat, clip.beats), (4.0, 8));
    let first = (clip.start_beat * 4.0) as i64;
    let mut played = Played::new();
    for f in fires {
        let step = f.tick as i64 - 1 - first;
        if (0..32).contains(&step) {
            *played.entry((step, f.channel, f.note as u8)).or_default() += 1;
        }
    }
    (rig, id, played)
}

fn notes_of(n: &LoopNotes) -> Played {
    let mut out = Played::new();
    for x in &n.notes {
        *out.entry((x.at as i64, x.channel, x.note)).or_default() += 1;
    }
    out
}

#[test]
fn a_net_takes_notes_are_the_ones_its_deck_played() {
    for (genre, seed) in [("techno", 1.0), ("house", 7.0)] {
        let (rig, id, played) = take_with_fires(genre, seed);
        assert!(played.len() > 8, "{genre}: the deck played {} notes", played.len());
        let clip = rig.e.clip(&id).unwrap();
        let notes = loop_notes::extract(clip, SR, &RetuneEnv::default()).expect("notes");
        assert_eq!(notes.steps, 32);
        assert_eq!(notes_of(&notes), played, "{genre}");
        // at another rate the notes are the same: they're in sixteenths
        let again = loop_notes::extract(clip, 44_100.0, &RetuneEnv::default()).unwrap();
        assert_eq!(notes_of(&again), played, "{genre} at 44.1k");
        let parts = notes.parts();
        assert!(parts.iter().any(|p| p != DRUMS), "{genre}: {parts:?}");
        for n in &notes.notes {
            assert!((0.0..32.0).contains(&n.at) && n.len > 0.0, "{n:?}");
            assert_eq!(n.drum, (10..=15).contains(&n.channel), "{n:?}");
        }
    }
}

#[test]
fn a_take_sent_to_the_td3_is_one_line_of_notes() {
    let (rig, id, _) = take_with_fires("techno", 1.0);
    let notes = loop_notes::extract(rig.e.clip(&id).unwrap(), SR, &RetuneEnv::default()).unwrap();
    let tonal: Vec<String> = notes
        .parts()
        .into_iter()
        .filter(|p| p != DRUMS && notes.notes.iter().any(|n| &n.part == p && !n.drum))
        .collect();
    assert!(!tonal.is_empty(), "{:?}", notes.parts());
    let p = plan(&notes, &tonal.into_iter().collect(), &Dest::Td3);
    assert!(!p.is_empty());
    for w in p.windows(2) {
        assert!(w[0].at < w[1].at, "one note per start");
        assert!(w[0].at + w[0].len < w[1].at, "no overlap: {:?} {:?}", w[0], w[1]);
    }
}

#[test]
fn a_clip_with_no_net_behind_it_is_audio_only() {
    let mut rig = Rig::new();
    let c = test_clip(1);
    let id = c.id.clone();
    rig.e.loops.row.clips.push(c);
    let clip = rig.e.clip(&id).unwrap();
    let r = loop_notes::extract(clip, SR, &RetuneEnv::default());
    assert_eq!(r, Err(Refusal::NoProvenance));
    assert_eq!(no_notes(&r.unwrap_err()).0, "audio-only");
}
