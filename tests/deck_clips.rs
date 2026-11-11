//! The loop row's clip model and the pure halves of its operations: the clip, its settings, the
//! fade, the beat clock, crop and sequence audio, provenance, the library's entries, the
//! instruments a retune offers and the edits of a net deck's source. No Swift on the box that
//! wrote this file: every expectation is a literal from the Swift test named next to it, or
//! is worked out from the Swift source (marked "worked out"). `tests/loopclips_golden.rs`
//! compares the numeric and byte-level parts with Swift's own output.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use djrust::deck::clips::*;
use djrust::deck::clock::*;
use djrust::deck::library;
use djrust::deck::loops::LoopFollow;
use djrust::deck::provenance;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::voices::RoleVoice;
use djrust::json::{JsonObject, JsonValue};
use djrust::layers::{layer_key, reset_layers};
use djrust::session::model::{DeckSource, DEMO_ID};
use djrust::session::types::{BeatGrid, LoopFx, LoopFxMode, SequenceStep};
use djrust::voices::VoiceCatalog;
use djrust::TrackSpec;

const RATE: f64 = 48_000.0;

/// A clip of `bars` bars at 120 bpm whose samples say which clip it is (`LoopLibraryTests.clip`,
/// `LoopLibraryRemovalTests.take`): left = n * 0.01 + (i % 100) * 1e-4, right = -left.
fn buffer(n: i32, len: usize, rate: f64) -> SampleBuffer {
    let l: Vec<f32> = (0..len)
        .map(|i| n as f32 * 0.01f32 + (i % 100) as f32 * 1e-4f32)
        .collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    SampleBuffer::new(l, Some(r), rate)
}

fn clip(n: i32, bars: i64) -> LoopClip {
    let b = buffer(n, 96_000 * bars as usize, RATE);
    let mut c = LoopClip::from_buffer(
        &format!("A loop {n}"),
        if n % 2 == 0 { "B" } else { "A" },
        bars * 4,
        120.0,
        4.0 * n as f64,
        b,
    );
    c.provenance = None;
    c
}

fn left(c: &LoopClip) -> &[f32] {
    &c.buffer.left
}

/// LOOP-record provenance of a net deck playing techno seed 7, in section 2
/// (`LoopLibraryRemovalTests.netProvenance`).
fn net_provenance() -> JsonValue {
    JsonValue::parse(
        r#"{"deck":"B","title":"techno 7","mode":"net","source":{"kind":"net","share":{"genre":"techno","seed":7}},"section":{"index":2,"name":"drop"}}"#,
    )
    .unwrap()
}

fn with_provenance(mut c: LoopClip, p: JsonValue) -> LoopClip {
    c.provenance = Some(p);
    c
}

// MARK: LoopClip

#[test]
fn a_clip_is_a_wav_file() {
    // LoopLibraryRemovalTests.aClipIsAWAVFile
    let c = clip(4, 1);
    let data = c.wav_data();
    assert!(data.len() > 96_000 * 8 && &data[..4] == b"RIFF");
    // worked out: 44 header bytes and 8 a frame for float32 stereo
    assert_eq!(data.len(), 44 + 96_000 * 8);
    let mut named = c.clone();
    named.title = "drop: A/B?".into();
    assert_eq!(named.file_name(), "drop- A-B-.wav");
    // worked out from fileName: every bad character is one "-", the ends are trimmed, blank is "loop"
    named.title = "  a\\b*c|d\"e<f>g%h\r\ni  ".into();
    assert_eq!(named.file_name(), "a-b-c-d-e-f-g-h--i.wav");
    named.title = "   ".into();
    assert_eq!(named.file_name(), "loop.wav");
    named.title = "".into();
    assert_eq!(named.file_name(), "loop.wav");
}

#[test]
fn a_clip_has_a_length_label_a_grid_and_a_palette() {
    // LoopRecordTests.cropKeepsTheBeatsBetweenTheEdges: "2 beats"; the others worked out
    let mut c = clip(1, 1);
    assert_eq!(c.length_label(), "1 bar");
    c.beats = 8;
    assert_eq!((c.length_label(), c.bars()), ("2 bars".to_string(), 2));
    c.beats = 6;
    assert_eq!((c.length_label(), c.bars()), ("6 beats".to_string(), 1));
    c.beats = 1;
    assert_eq!(c.length_label(), "1 beat");
    c.beats = 2;
    assert_eq!(c.length_label(), "2 beats");
    // LoopRecordTests.aClipMovesOntoADeckAndBack: the grid is the clip's tempo from sample 0
    assert_eq!(c.grid(), BeatGrid::new(120.0, 0.0, 1.0));
    assert_eq!(
        PALETTE,
        ["red", "orange", "yellow", "green", "teal", "blue", "violet", "pink"]
    );
}

#[test]
fn clips_are_equal_by_id() {
    let a = clip(1, 1);
    let mut b = clip(1, 1);
    b.title = "renamed".into();
    assert_eq!(a, b);
    assert_ne!(a, clip(2, 1));
}

#[test]
fn a_clips_id_is_its_audios_content_id() {
    // SessionTests content ids: sha256 over the rate and the float bits; a mono buffer hashes
    // one channel, so the same samples as stereo give another id
    let stereo = buffer(1, 1000, RATE);
    let mono = SampleBuffer::new(stereo.left.clone(), None, RATE);
    assert_eq!(stereo.content_id().len(), 64);
    assert_ne!(stereo.content_id(), mono.content_id());
    assert_ne!(stereo.content_id(), buffer(1, 1000, 44_100.0).content_id());
    assert_eq!(stereo.content_id(), buffer(1, 1000, RATE).content_id());
    assert_eq!(
        sequence_id(&stereo),
        format!("seq-{}", stereo.content_id()),
        "a one-step sequence is not its clip"
    );
    let c = clip(3, 1);
    assert_eq!(c.id, c.buffer.content_id());
}

// MARK: crop

#[test]
fn crop_keeps_the_beats_between_the_edges() {
    // LoopRecordTests.cropKeepsTheBeatsBetweenTheEdges
    let one = clip(1, 1);
    assert_eq!(one.beats, 4);
    // nothing to crop, or out of range: refused
    assert!(crop_clip(&one, 0, 4).is_none());
    assert!(crop_clip(&one, 2, 2).is_none());
    assert!(crop_clip(&one, 1, 5).is_none());
    assert!(crop_clip(&one, -1, 2).is_none());
    assert!(crop_clip(&one, 3, 2).is_none());
    let c = crop_clip(&one, 1, 3).unwrap();
    assert!(c.id != one.id);
    assert_eq!((c.beats, c.length_label().as_str(), c.bpm), (2, "2 beats", 120.0));
    assert_eq!((c.start_beat, c.source.as_str()), (one.start_beat + 1.0, "A"));
    let per_beat = one.buffer.length() / 4;
    assert_eq!(c.buffer.length(), 2 * per_beat);
    assert_eq!(
        left(&c),
        &left(&one)[per_beat..3 * per_beat],
        "beats 2 and 3 of the original, sample for sample"
    );
    // the right channel too, and it has one even from a mono clip
    assert_eq!(c.buffer.right(), &one.buffer.right()[per_beat..3 * per_beat]);
    assert_eq!(c.id, c.buffer.content_id());
}

#[test]
fn a_crop_keeps_title_colour_and_fx_and_says_what_it_was_made_from() {
    // LoopLibraryRemovalTests.namesAndColoursAreKeptAndRecorded (a crop keeps both),
    // LoopRecordTests.aTakeKnowsWhereItCameFrom (the crop's provenance)
    let mut one = with_provenance(clip(1, 1), net_provenance());
    one.title = "big drop".into();
    one.color = Some("orange".into());
    one.fx = LoopFx {
        drive: 6.0,
        mode: LoopFxMode::Tube,
        ..LoopFx::default()
    };
    let c = crop_clip(&one, 0, 2).unwrap();
    assert_eq!((c.title.as_str(), c.color.as_deref()), ("big drop", Some("orange")));
    assert_eq!(c.fx, one.fx);
    let p = c.provenance.as_ref().unwrap();
    assert_eq!(p.get("op").and_then(JsonValue::as_str), Some("crop"));
    assert_eq!(p.get("from").and_then(JsonValue::as_str), Some(one.id.as_str()));
    assert_eq!(p.get("beats").unwrap().ints(), Some(vec![0, 2]));
    assert_eq!(
        p.get("parent").unwrap().canonical_js(),
        one.provenance.as_ref().unwrap().canonical_js()
    );
    // worked out from LoopProvenance.crop: no parent key without a parent provenance
    let bare = crop_clip(&clip(2, 1), 1, 2).unwrap();
    assert!(bare.provenance.as_ref().unwrap().get("parent").is_none());
    assert_eq!(
        bare.provenance.unwrap().canonical_js(),
        format!(r#"{{"beats":[1,2],"from":"{}","op":"crop"}}"#, clip(2, 1).id)
    );
}

#[test]
fn a_crop_edge_rounds_half_away_from_zero() {
    // worked out from cropClip: 6 samples in 4 beats is 1.5 a beat; beat 1 -> 1.5 -> 2, beat 3 -> 4.5 -> 5
    let b = SampleBuffer::new(vec![0., 1., 2., 3., 4., 5.], None, RATE);
    let cut = crop_buffer(&b, 4, 1, 3).unwrap();
    assert_eq!(cut.left, vec![2., 3., 4.]);
    assert_eq!(cut.right.as_deref(), Some(&[2., 3., 4.][..]), "stereo even from mono");
}

// MARK: sequence

#[test]
fn a_sequence_is_its_steps_back_to_back() {
    // LoopLibraryTests.aSequenceIsItsStepsBackToBack
    let (a, b) = (clip(1, 1), clip(2, 2));
    let all = [a.clone(), b.clone()];
    let lookup = |id: &str| all.iter().find(|c| c.id == id);
    let steps = [
        SequenceStep::new(&a.id, 2),
        SequenceStep::new(&b.id, 1),
    ];
    let seq = make_sequence_clip(&steps, lookup, RATE, "2026-10-29T10:00:00Z").unwrap();
    assert_eq!((seq.beats, seq.bpm), (4 * 2 + 8, 120.0));
    assert_eq!(seq.source, a.source);
    assert_eq!(seq.steps.as_deref(), Some(&steps[..]));
    let mut want: Vec<f32> = Vec::new();
    want.extend_from_slice(left(&a));
    want.extend_from_slice(left(&a));
    want.extend_from_slice(left(&b));
    assert_eq!(left(&seq), &want[..], "exactly, sample for sample");
    assert_eq!(seq.title, "seq 1×2→2");
    assert_eq!(seq.id, sequence_id(&seq.buffer));
    assert!(seq.id.starts_with("seq-"));
    assert_eq!(seq.start_beat, a.start_beat);
}

#[test]
fn another_tempo_is_stretched_to_the_first_clips() {
    // LoopLibraryTests.anotherTempoIsStretchedToTheFirstClips
    let a = clip(1, 1);
    let fast = clip(2, 1);
    // same audio length, but say it is at 240 bpm: one bar takes half as long
    let mut quick = fast.clone();
    quick.bpm = 240.0;
    let all = [a.clone(), quick.clone()];
    let lookup = |id: &str| all.iter().find(|c| c.id == id);
    let steps = [SequenceStep::new(&a.id, 1), SequenceStep::new(&quick.id, 1)];
    let seq = make_sequence_clip(&steps, lookup, RATE, "").unwrap();
    assert_eq!((seq.bpm, seq.beats), (120.0, 8));
    // a's bar, then the 240 bpm bar played at 120: twice as long as it was
    assert_eq!(
        seq.buffer.length(),
        a.buffer.length() + 2 * quick.buffer.length()
    );
}

#[test]
fn a_sequence_leaves_out_steps_with_no_clip_or_no_repeats() {
    // LoopLibraryTests.editingASequenceKeepsItsPlaceAndItsLoop: nothing left is nil, a missing
    // clip is left out; "repeats <= 0" is dropped (makeSequence's filter)
    let (b, c) = (clip(2, 1), clip(3, 1));
    let all = [b.clone(), c.clone()];
    let lookup = |id: &str| all.iter().find(|x| x.id == id);
    assert!(make_sequence_clip(&[SequenceStep::new("gone", 1)], lookup, RATE, "").is_none());
    assert!(make_sequence_clip(&[SequenceStep::new(&b.id, 0)], lookup, RATE, "").is_none());
    let steps = [SequenceStep::new(&b.id, 4), SequenceStep::new("gone", 1)];
    let seq = make_sequence_clip(&steps, lookup, RATE, "").unwrap();
    assert_eq!(seq.steps.as_deref(), Some(&steps[..1]), "a missing clip is left out");
    assert_eq!(seq.beats, 16);
    // the title names the last word of the title, twice over for repeats
    assert_eq!(seq.title, "seq 2×4");
    let steps = [
        SequenceStep::new(&b.id, 1),
        SequenceStep::new(&c.id, 0),
        SequenceStep::new(&c.id, 3),
    ];
    let seq = make_sequence_clip(&steps, lookup, RATE, "").unwrap();
    assert_eq!(seq.title, "seq 2→3×3");
}

#[test]
fn a_sequence_says_what_it_was_made_from_and_outlives_its_clips() {
    // LoopRecordTests.aTakeKnowsWhereItCameFrom: the sequence's first step carries the
    // crop's provenance, and a sequence still knows its first step's track
    let one = with_provenance(clip(1, 1), net_provenance());
    let cropped = crop_clip(&one, 0, 2).unwrap();
    let all = [cropped.clone()];
    let lookup = |id: &str| all.iter().find(|x| x.id == id);
    let seq =
        make_sequence_clip(&[SequenceStep::new(&cropped.id, 2)], lookup, RATE, "2026-10-29T10:00:00Z")
            .unwrap();
    let sp = seq.provenance.as_ref().unwrap();
    assert_eq!(sp.get("op").and_then(JsonValue::as_str), Some("sequence"));
    assert_eq!(
        sp.get("recorded").and_then(JsonValue::as_str),
        Some("2026-10-29T10:00:00Z")
    );
    let step0 = sp.get("steps").unwrap().at(0).unwrap();
    assert_eq!(
        step0.get("provenance").unwrap().canonical_js(),
        cropped.provenance.as_ref().unwrap().canonical_js(),
        "outlives the clips it used"
    );
    assert_eq!(step0.get("clip").and_then(JsonValue::as_str), Some(cropped.id.as_str()));
    assert_eq!(step0.get("repeats").and_then(JsonValue::as_i64), Some(2));
    // LoopLibraryRemovalTests.aClipKnowsTheTrackItCameFrom: "and a sequence, by its first step"
    assert_eq!(seq.origin_genre().as_deref(), Some("techno"));
    assert_eq!(seq.origin_section(), Some(2));
}

#[test]
fn a_sequences_fx_are_its_steps_when_they_all_share_one_active_fx() {
    // worked out from sharedFx: all the same active FX -> that FX; mixed, or inactive -> off
    let fx = LoopFx {
        drive: 9.0,
        mode: LoopFxMode::Hard,
        amount: 0.55,
        tone: 0.75,
        ..LoopFx::default()
    };
    let mut a = clip(1, 1);
    let mut b = clip(2, 1);
    a.fx = fx.clone();
    b.fx = fx.clone();
    assert_eq!(shared_fx(&[&a, &b]), fx);
    b.fx = LoopFx::default();
    assert_eq!(shared_fx(&[&a, &b]), LoopFx::default(), "mixed: off");
    assert_eq!(shared_fx(&[]), LoopFx::default());
    let off = LoopFx {
        bypass: true,
        ..fx.clone()
    };
    a.fx = off.clone();
    b.fx = off;
    assert_eq!(shared_fx(&[&a, &b]), LoopFx::default(), "bypassed is not active");
    // LoopFX.isActive (LoopFxTests): trim alone is active, mix 0 silences the rest
    assert!(fx_is_active(&LoopFx {
        trim: 3.0,
        ..LoopFx::default()
    }));
    assert!(!fx_is_active(&LoopFx {
        mix: 0.0,
        drive: 12.0,
        ..LoopFx::default()
    }));
    assert!(!fx_is_active(&LoopFx::default()));
    assert!(fx_is_active(&LoopFx {
        tone: 0.5,
        ..LoopFx::default()
    }));
    assert!(!fx_is_active(&LoopFx {
        mode: LoopFxMode::Crush,
        amount: 0.0,
        ..LoopFx::default()
    }));
}

#[test]
fn stretching_reads_factor_samples_per_output_sample() {
    // worked out from stretch: at 0.5 it lasts twice as long; 1 (within 1e-9) is a plain copy
    let b = SampleBuffer::new(vec![0.0, 1.0, 2.0, 3.0], None, RATE);
    let (l, r) = stretch(&b, 0.5);
    assert_eq!(l, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.0]);
    assert_eq!(l, r);
    let (l, _) = stretch(&b, 1.0 + 1e-10);
    assert_eq!(l, vec![0.0, 1.0, 2.0, 3.0]);
    // 2: half as long, every other sample
    let (l, _) = stretch(&b, 2.0);
    assert_eq!(l, vec![0.0, 2.0]);
    // the length rounds half away from zero: 4 / 1.6 = 2.5 -> 3
    let (l, _) = stretch(&b, 1.6);
    assert_eq!(l.len(), 3);
}

// MARK: provenance

#[test]
fn provenance_finish_fills_in_the_bars_the_tempo_and_a_samples_content_id() {
    // LoopRecordTests.aSampleTakeNamesItsSampleByContent
    let o = JsonObject::from_pairs([
        ("deck", JsonValue::str("A")),
        (
            "source",
            DeckSource::Sample {
                id: String::new(),
                title: "ramp".into(),
                grid: Some(BeatGrid::new(120.0, 0.0, 1.0)),
            }
            .json(),
        ),
    ]);
    let p = provenance::finish(o.clone(), 1, 120.0, Some("abc123"));
    let s = p.get("source").unwrap();
    assert_eq!(s.get("kind").and_then(JsonValue::as_str), Some("sample"));
    assert_eq!(s.get("title").and_then(JsonValue::as_str), Some("ramp"));
    assert_eq!(s.get("id").and_then(JsonValue::as_str), Some("abc123"));
    assert_eq!(s.get("grid").unwrap().get("bpm").and_then(JsonValue::as_f64), Some(120.0));
    assert_eq!(p.get("bars").and_then(JsonValue::as_i64), Some(1));
    assert_eq!(p.get("bpm").and_then(JsonValue::as_f64), Some(120.0));
    // no sample buffer, or a source that already has an id: untouched
    let p = provenance::finish(o, 2, 90.0, None);
    assert_eq!(
        p.get("source").unwrap().get("id").and_then(JsonValue::as_str),
        Some("")
    );
    let named = JsonObject::from_pairs([(
        "source",
        DeckSource::Sample {
            id: "keep".into(),
            title: "t".into(),
            grid: None,
        }
        .json(),
    )]);
    let p = provenance::finish(named, 1, 100.0, Some("other"));
    assert_eq!(
        p.get("source").unwrap().get("id").and_then(JsonValue::as_str),
        Some("keep")
    );
}

#[test]
fn a_stock_loops_provenance_names_its_file_and_its_sample() {
    // LoopRecordTests.anImportJoinsTheRowAndLoopsInTimeOnce / LoopProvenance.stock: kind "stock"
    // is what the publish refusal looks for; key shapes worked out from the Swift
    let l = djrust::layers::StockLoop {
        path: "/lib/Tone 01.caf".into(),
        name: "Tone 01".into(),
        pack: "Pack".into(),
        category: "Texture/Atmosphere".into(),
        subcategory: "Pad".into(),
        genre: "Ambient".into(),
        beats: 8,
        seconds: 4.0,
        key: Some("A minor".into()),
    };
    let p = provenance::stock(&l, "id1", "dev", "2026-10-29T10:00:00Z");
    let keys: Vec<&str> = p.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "op", "kind", "recorded", "build", "title", "path", "pack", "category", "subcategory",
            "genre", "beats", "bpm", "source", "key"
        ]
    );
    assert_eq!(p.get("kind").and_then(JsonValue::as_str), Some("stock"));
    assert_eq!(p.get("bpm").and_then(JsonValue::as_f64), Some(120.0));
    let s = p.get("source").unwrap();
    assert_eq!(s.get("id").and_then(JsonValue::as_str), Some("id1"));
    assert_eq!(s.get("grid").unwrap().get("bpm").and_then(JsonValue::as_f64), Some(120.0));
    let mut no_key = l;
    no_key.key = None;
    assert!(provenance::stock(&no_key, "id1", "dev", "").get("key").is_none());
}

#[test]
fn what_a_clip_remembers_of_its_track() {
    // LoopLibraryRemovalTests.aClipKnowsTheTrackItCameFrom
    let net = with_provenance(clip(1, 1), net_provenance());
    let other = clip(9, 1);
    let sample_prov = JsonValue::Object(JsonObject::from_pairs([
        ("deck", JsonValue::str("A")),
        (
            "source",
            DeckSource::Sample {
                id: other.id.clone(),
                title: "kick.wav".into(),
                grid: Some(BeatGrid::new(120.0, 0.0, 1.0)),
            }
            .json(),
        ),
    ]));
    let sampled = with_provenance(clip(2, 1), sample_prov);
    let mut library: BTreeMap<String, Arc<SampleBuffer>> = BTreeMap::new();
    let o = clip_origin(&net, &library).unwrap();
    match &o.source {
        DeckSource::Net(spec) => {
            assert_eq!((spec.genre.as_str(), spec.seed), ("techno", 7.0));
        }
        other => panic!("a net: {other:?}"),
    }
    assert_eq!(o.section, Some(2));
    assert!(o.buffer.is_none());
    assert_eq!(net.origin_genre().as_deref(), Some("techno"));
    assert_eq!(net.origin_title().as_deref(), Some("techno 7"));
    assert!(
        clip_origin(&sampled, &library).is_none(),
        "its audio is not in the library"
    );
    library.insert(other.id.clone(), other.buffer.clone());
    assert!(clip_origin(&sampled, &library).unwrap().buffer.is_some());
    // the demo loop needs no audio
    let demo = with_provenance(
        clip(3, 1),
        JsonValue::parse(&format!(
            r#"{{"source":{{"kind":"sample","id":"{DEMO_ID}","title":"demo"}}}}"#
        ))
        .unwrap(),
    );
    let o = clip_origin(&demo, &BTreeMap::new()).unwrap();
    assert!(o.buffer.is_none() && o.section.is_none());
    // a clip with no provenance, or none that names a source, has no origin
    assert!(clip_origin(&clip(4, 1), &library).is_none());
    assert!(net.origin_roles_off().is_empty() && net.origin_macros().is_empty());
    assert_eq!(net.origin_bpm(), None);
}

#[test]
fn a_takes_origin_fields_come_from_its_provenance() {
    // LoopRecordTests.aTakeKnowsWhereItCameFrom (the fields the accessors read): tempo.bpm,
    // rolesOff, macros
    let p = JsonValue::parse(
        r#"{"deck":"A","mode":"net","title":"t","tempo":{"pitch":0.25,"baseBPM":120,"bpm":122.4},
            "rolesOff":["melody","arp"],"macros":["beatRepeat"],"source":{"kind":"net","share":{"genre":"techno","seed":1}}}"#,
    )
    .unwrap();
    let c = with_provenance(clip(1, 1), p);
    assert_eq!(c.origin_bpm(), Some(122.4));
    assert_eq!(c.origin_roles_off(), ["melody", "arp"]);
    assert_eq!(c.origin_macros(), ["beatRepeat"]);
    // through a crop, and a crop of a crop
    let crop = crop_clip(&c, 0, 2).unwrap();
    let crop2 = crop_clip(&crop, 1, 2).unwrap();
    assert_eq!(crop2.origin_bpm(), Some(122.4));
    assert_eq!(crop2.take_provenance().unwrap().get("deck").and_then(JsonValue::as_str), Some("A"));
}

#[test]
fn retunes_stack_through_crops() {
    // LoopRetuneTests.retunesStackThroughCrops (the provenance half): a retune of a retuned
    // clip keeps the first one's instruments under its own, a crop of it too, the newest
    // retune of a channel stands, and the provenance is four ops deep
    let take = with_provenance(clip(1, 2), net_provenance());
    let voice = |ch: i64, name: &str| BTreeMap::from([(ch, name.to_string())]);
    let rec = "2026-10-29T10:00:00Z";
    let mk = |parent: &LoopClip, v: &BTreeMap<i64, String>| {
        let mut c = clip(parent.start_beat as i32 + 10, 2);
        c.provenance = Some(provenance::retune(parent, v, "dev", rec));
        c
    };
    let first = mk(&take, &voice(3, "square"));
    let second = mk(&first, &voice(5, "piano"));
    assert_eq!(
        second.retuned_voices(),
        BTreeMap::from([(3, "square".to_string()), (5, "piano".to_string())])
    );
    let cropped = crop_clip(&second, 0, 4).unwrap();
    assert_eq!(cropped.retuned_voices(), second.retuned_voices());
    let mut third = mk(&cropped, &voice(3, "saw"));
    third.id = "third".into();
    assert_eq!(third.retuned_voices().get(&3).map(String::as_str), Some("saw"));
    assert_eq!(third.retuned_voices().get(&5).map(String::as_str), Some("piano"));
    // retune -> crop -> retune -> retune -> take = 4 ops deep
    let mut depth = 0;
    let mut p = third.provenance.as_ref();
    while p.and_then(|v| v.get("op")).and_then(JsonValue::as_str).is_some() {
        depth += 1;
        p = p.and_then(|v| v.get("parent"));
    }
    assert_eq!(depth, 4);
    // the newest retune stands in the source too, through the take's own
    let taken = DeckSource::from_json(&net_provenance().get("source").cloned().unwrap()).unwrap();
    let want = taken
        .with_instrument("saw", 3)
        .unwrap()
        .with_instrument("piano", 5)
        .unwrap();
    assert_eq!(third.origin_source().unwrap(), want);
    // LoopRetuneTests.anotherInstrumentChangesOnlyItsPart: originSource.withInstrument
    assert_eq!(
        first.origin_source().unwrap(),
        taken.with_instrument("square", 3).unwrap()
    );
    // a take as taken has no retuned voices and its own source; a sequence has none either
    assert!(take.retuned_voices().is_empty());
    assert_eq!(take.origin_source().unwrap(), taken);
    let all = [take.clone()];
    let seq = make_sequence_clip(
        &[SequenceStep::new(&take.id, 1)],
        |id| all.iter().find(|c| c.id == id),
        RATE,
        rec,
    )
    .unwrap();
    assert!(seq.retuned_voices().is_empty());
    // the retune's keys (worked out from LoopProvenance.retune)
    let p = provenance::retune(&take, &voice(3, "square"), "dev", rec);
    assert_eq!(
        p.canonical_js(),
        format!(
            r#"{{"build":"dev","from":"{}","op":"retune","parent":{},"recorded":"{rec}","voices":{{"3":"square"}}}}"#,
            take.id,
            take.provenance.as_ref().unwrap().canonical_js()
        )
    );
}

// MARK: ClipSettings

#[test]
fn a_clips_boundary_and_fade_go_with_it_and_are_forgotten_with_it() {
    // LoopLaunchTests.aClipsBoundaryAndFadeGoWithItAndAreForgottenWithIt
    let (c, d) = (clip(1, 1), clip(2, 1));
    let mut s = ClipSettings::new();
    assert!(s.set_quantum(&c.id, Some(8)));
    s.set_fade(&c.id, Some(LoopFade::Bar));
    let row = [c.id.as_str(), d.id.as_str()];
    assert_eq!(
        s.launch_map(row),
        BTreeMap::from([(
            c.id.clone(),
            ClipLaunch {
                quantum: Some(8),
                fade: Some(LoopFade::Bar),
                follow: None
            }
        )]),
        "only clips with one"
    );
    // taken off and put back: as it was
    let sound = s.snapshot(&c.id);
    assert!(!s.forget(&c.id));
    assert!(s.quantum(&c.id).is_none() && s.fade(&c.id).is_none() && s.launch_map(row).is_empty());
    assert_eq!(
        sound,
        ClipSound {
            quantum: Some(8),
            fade: Some(LoopFade::Bar),
            ..ClipSound::default()
        }
    );
    assert!(!s.restore(&c.id, &sound));
    assert_eq!((s.quantum(&c.id), s.fade(&c.id)), (Some(8), Some(LoopFade::Bar)));
    // the library's, back on a fresh row (unknown ids skipped)
    let mut fresh = ClipSettings::new();
    let saved = BTreeMap::from([
        (
            c.id.clone(),
            ClipLaunch {
                quantum: Some(16),
                ..ClipLaunch::default()
            },
        ),
        (
            "gone".to_string(),
            ClipLaunch {
                fade: Some(LoopFade::Off),
                ..ClipLaunch::default()
            },
        ),
    ]);
    let ids = [c.id.clone(), d.id.clone()];
    fresh.restore_launch(&saved, &|id| ids.iter().any(|x| x == id));
    assert_eq!((fresh.quantum(&c.id), fresh.fade(&c.id)), (Some(16), None));
    assert_eq!(
        fresh.launch_map(row).keys().collect::<Vec<_>>(),
        vec![&c.id]
    );
    // a quantum that is not offered is refused, in the library too
    fresh.restore_launch(
        &BTreeMap::from([(
            d.id.clone(),
            ClipLaunch {
                quantum: Some(3),
                ..ClipLaunch::default()
            },
        )]),
        &|_| true,
    );
    assert_eq!(fresh.quantum(&d.id), None);
}

#[test]
fn a_clips_own_boundary_overrides_the_rows() {
    // LoopLaunchTests.aClipsOwnBoundaryOverridesTheRowsButAScenesDoesNot (the settings half)
    let (one, two) = ("one", "two");
    let mut s = ClipSettings::new();
    assert!(s.set_quantum(one, Some(16)));
    assert!(!s.set_quantum(two, Some(3)), "not offered: refused");
    assert_eq!((s.quantum(one), s.quantum(two)), (Some(16), None));
    assert_eq!((s.quantum_for(1, Some(one)), s.quantum_for(1, Some(two))), (16, 1));
    assert_eq!(s.quantum_for(1, None), 1);
    // null follows the row again
    assert!(s.set_quantum(one, None));
    assert_eq!((s.quantum(one), s.quantum_for(4, Some(one))), (None, 4));
}

#[test]
fn a_clips_own_follow_beats_the_rows_and_survives_removal() {
    // LoopLaunchTests.aClipFollowingItsOwnDeckIgnoresTheLouderOne / the follow round trip
    let mut s = ClipSettings::new();
    assert_eq!(s.follow_for(LoopFollow::Louder, Some("x")), LoopFollow::Louder);
    assert!(s.any_follows_louder(LoopFollow::Louder));
    s.set_follow("x", Some(LoopFollow::Source));
    assert_eq!(s.follow_for(LoopFollow::Louder, Some("x")), LoopFollow::Source);
    assert_eq!(s.follow_for(LoopFollow::Louder, Some("y")), LoopFollow::Louder);
    assert_eq!(s.follow_for(LoopFollow::Louder, None), LoopFollow::Louder);
    assert!(!s.any_follows_louder(LoopFollow::Source));
    s.set_follow("y", Some(LoopFollow::Louder));
    assert!(s.any_follows_louder(LoopFollow::Source), "a clip of its own");
    // removal keeps it: ClipSound(follow: .source)
    let sound = s.snapshot("x");
    assert_eq!(
        sound,
        ClipSound {
            follow: Some(LoopFollow::Source),
            ..ClipSound::default()
        }
    );
    s.forget("x");
    assert_eq!(s.follow("x"), None);
    s.restore("x", &sound);
    assert_eq!(s.follow("x"), Some(LoopFollow::Source));
    s.set_follow("x", None);
    assert_eq!(s.follow("x"), None);
}

#[test]
fn undoing_a_removal_keeps_mute_solo_and_key_lock() {
    // LoopLibraryRemovalTests.undoingARemovalKeepsItsMuteSoloAndKeyLock (the settings half)
    let mut s = ClipSettings::new();
    s.set_mute("a", true);
    s.set_key_lock("a", true);
    s.set_solo("b", true);
    s.set_key_lock("c", false);
    let sounds: Vec<(&str, ClipSound)> = ["a", "b", "c"].iter().map(|i| (*i, s.snapshot(i))).collect();
    let mut pushed = Vec::new();
    for (id, _) in &sounds {
        pushed.push(s.forget(id));
    }
    assert_eq!(pushed, [false, true, false], "removing the soloed one pushes the levels");
    assert!(s.solos.is_empty() && s.mutes.is_empty(), "off the row, they are not heard");
    for (id, sound) in &sounds {
        s.restore(id, sound);
    }
    assert!(s.is_muted("a") && s.is_key_locked("a", false) && !s.is_soloed("a"));
    assert!(s.is_soloed("b") && !s.is_muted("b"));
    assert_eq!(s.key_locks.get("c"), Some(&false), "its own setting, not the row's");
    assert!(!s.is_key_locked("c", true));
    assert_eq!(s.audible("c", 1.0), 0.0, "b is soloed again");
    assert_eq!(s.audible("b", 0.7), 0.7);
    assert_eq!(s.audible("a", 1.0), 0.0, "muted");
    // the restore of a soloed clip says the levels need pushing
    assert!(s.restore("z", &ClipSound { soloed: true, ..ClipSound::default() }));
}

#[test]
fn another_clip_takes_a_clips_place_with_its_settings() {
    // LoopRetuneTests.aRetunedClipTakesItsPlaceAndKeepsItsSettings (carryClipSound)
    let mut s = ClipSettings::new();
    s.set_mute("old", true);
    s.set_solo("old", true);
    s.set_key_lock("old", true);
    s.set_quantum("old", Some(16));
    s.set_fade("old", Some(LoopFade::Beat));
    s.set_follow("old", Some(LoopFollow::Source));
    let before = s.snapshot("old");
    s.carry("old", "new");
    assert_eq!(s.snapshot("old"), ClipSound::default());
    assert_eq!(s.snapshot("new"), before);
    // a clip with nothing set leaves nothing behind
    s.carry("nothing", "else");
    assert_eq!(s.snapshot("else"), ClipSound::default());
    // clearLoopRow empties the mutes, solos and key locks and, as in Swift, no more
    s.clear_for_reset();
    assert!(s.mutes.is_empty() && s.solos.is_empty() && s.key_locks.is_empty());
    assert_eq!(s.quantum("new"), Some(16));
}

// MARK: fades

#[test]
fn the_fades_have_their_raw_values_labels_and_frames() {
    // LoopSoundTests.theSoundActionsRoundTrip: the labels; theRowsFadeSetsHowLongALoopTakesToGoOut:
    // a bar at 120 bpm is 96 000 frames, a short fade 480
    assert_eq!(
        LoopFade::ALL.map(LoopFade::label),
        ["off", "10 ms", "1 beat", "1 bar"]
    );
    assert_eq!(LoopFade::ALL.map(LoopFade::raw), ["off", "10ms", "beat", "bar"]);
    for f in LoopFade::ALL {
        assert_eq!(LoopFade::from_raw(f.raw()), Some(f));
    }
    assert_eq!(LoopFade::from_raw("nonsense"), None);
    assert_eq!(LoopFade::Bar.frames(RATE, 120.0, false), 96_000);
    assert_eq!(LoopFade::Bar.frames(RATE, 120.0, true), 480);
    assert_eq!(LoopFade::Beat.frames(RATE, 120.0, false), 24_000);
    assert_eq!(LoopFade::Declick.frames(RATE, 120.0, false), 480);
    assert_eq!(LoopFade::Off.frames(RATE, 120.0, false), 0);
    assert_eq!(LoopFade::Off.frames(RATE, 120.0, true), 0);
    // worked out: no tempo makes a beat as long as a de-click; the beat truncates
    assert_eq!(LoopFade::Bar.frames(RATE, 0.0, false), 4 * 480);
    assert_eq!(LoopFade::Beat.frames(44_100.0, 127.5, false), 20_752);
    // LoopSoundTests.aClipsOwnFadeOverridesTheRowsAndFollowsACrop: its own, else the row's
    let mut s = ClipSettings::new();
    s.set_fade("one", Some(LoopFade::Bar));
    assert_eq!(s.fade_for(LoopFade::Off, Some("one")), LoopFade::Bar);
    assert_eq!(s.fade_for(LoopFade::Off, Some("other")), LoopFade::Off);
    assert_eq!(s.fade_for(LoopFade::Off, None), LoopFade::Off);
    assert_eq!(s.fade_for(LoopFade::Off, Some("one")).frames(RATE, 120.0, false), 96_000);
    assert_eq!(s.fade_for(LoopFade::Off, Some("one")).frames(RATE, 120.0, true), 480);
    assert_eq!(s.fade_for(LoopFade::Off, Some("other")).frames(RATE, 120.0, false), 0);
}

// MARK: the clock

/// Deck A at 120 bpm, its beat clock running from frame `at` at beat `beat`.
fn deck_a(at: i64, beat: f64, bpm: f64) -> ClockDeck {
    ClockDeck {
        name: "A".into(),
        playing: true,
        clip_id: None,
        beat_state: Some((bpm, beat)),
        rendered_frames: at,
        anchor: None,
    }
}

fn clock_of(deck: ClockDeck, clip_id: Option<&str>, clip_source: Option<&str>) -> LoopClock {
    select_clock(clip_id, clip_source, None, &[deck], &[], 0, RATE)
}

/// Is beat `b` on a line every `q` beats (to within the frame it was rounded to)?
fn on_line(b: f64, q: f64) -> bool {
    let r = (b / q - libm_round(b / q)) * q;
    r.abs() <= (1.0 / 24_000.0) * 1.01
}

fn libm_round(x: f64) -> f64 {
    x.round()
}

#[test]
fn with_four_bars_a_start_lands_on_a_phrase_of_the_deck() {
    // LoopLaunchTests.withFourBarsAStartLandsOnAPhraseOfTheDeck (the clock half)
    let beat = 24_000i64;
    for (at, b0) in [(50_000, 9.37), (50_000, 0.0), (123_457, 31.99), (7, 15.0)] {
        let clock = clock_of(deck_a(at, b0, 120.0), Some("one"), Some("A"));
        assert_eq!(clock.ref_deck.as_deref(), Some("A"));
        let now = at + 1000;
        let down = clock.next_boundary(now + LOOP_LEAD, 16);
        assert!(on_line(clock.beat(down), 16.0), "on a 16-beat line of A's own count");
        assert_eq!(libm_round(clock.beat(down) / 4.0) as i64 % 4, 0, "bar 1, 5, 9…");
        assert!(down >= now + LOOP_LEAD, "never before the frame asked for");
        assert!(down - now <= 16 * beat + LOOP_LEAD);
    }
    // an exact boundary is its own: at or after
    let clock = clock_of(deck_a(0, 0.0, 120.0), Some("one"), Some("A"));
    assert_eq!(clock.next_boundary(96_000, 4), 96_000);
    // (rounded up to a whole frame; the float error of the beat may add one)
    assert!((192_000..=192_001).contains(&clock.next_boundary(96_001, 4)));
    assert_eq!(clock.next_downbeat(1), 96_000);
}

#[test]
fn on_the_beat_a_start_waits_at_most_a_beat() {
    // LoopLaunchTests.onTheBeatAStartWaitsAtMostABeat (the clock half)
    let beat = 24_000i64;
    for at in [0, 1, 11_999, 12_000, 777_777] {
        let clock = clock_of(deck_a(at, 3.3, 120.0), Some("one"), Some("A"));
        let now = at + 10;
        let go = clock.next_boundary(now + LOOP_LEAD, 1);
        assert!(go - now <= beat + LOOP_LEAD);
        assert!(on_line(clock.beat(go), 1.0));
    }
}

#[test]
fn the_nearest_boundary_is_before_or_after_the_frame() {
    // worked out from nearestBoundary: beat 5.3 on a 4-beat line: back 1.3, ahead 2.7 -> back
    let clock = clock_of(deck_a(0, 5.3, 120.0), Some("one"), Some("A"));
    let f = clock.nearest_boundary(0, 4);
    assert!(f < 0 && on_line(clock.beat(f), 4.0));
    // back is 5.3 % 4 = 1.2999999999999998 beats: 31 199 whole frames, not 31 200
    assert_eq!(f, -((5.3f64 % 4.0) * 24_000.0).floor() as i64);
    // beat 6.7: back 2.7, ahead 1.3 -> ahead
    let clock = clock_of(deck_a(0, 6.7, 120.0), Some("one"), Some("A"));
    let f = clock.nearest_boundary(0, 4);
    assert!(f > 0 && on_line(clock.beat(f), 4.0));
    // exactly half way: the one ahead (ahead <= back)
    let clock = clock_of(deck_a(0, 6.0, 120.0), Some("one"), Some("A"));
    assert_eq!(clock.nearest_boundary(0, 4), 48_000);
    // negative beats (before the origin) wrap the same way
    let clock = clock_of(deck_a(0, 0.0, 120.0), Some("one"), Some("A"));
    assert_eq!(clock.nearest_boundary(-10_000, 4), 0);
    // no clock: the frame itself
    let none = LoopClock::none(500);
    assert_eq!((none.nearest_boundary(77, 4), none.next_boundary(77, 4)), (77, 77));
    assert_eq!(none.beat(1000), 0.0);
    assert!(none.waiting_boundary(4).is_none());
}

#[test]
fn a_clock_comes_from_the_first_playing_deck_that_holds_another_clip() {
    // worked out from loopClock(for:prefer:): candidates [first, the clip's deck] then the decks
    // by name; a deck qualifies when it plays, holds some other clip and has a beat state
    let mut a = deck_a(1000, 8.0, 120.0);
    let mut b = deck_a(2000, 2.0, 126.0);
    b.name = "B".into();
    let decks = [b.clone(), a.clone()];
    let clock = |first: Option<&str>, clip: Option<&str>, src: Option<&str>| {
        select_clock(clip, src, first, &decks, &[], 99, RATE)
    };
    // the clip's own deck first
    assert_eq!(clock(None, Some("c"), Some("B")).ref_deck.as_deref(), Some("B"));
    assert_eq!(clock(None, Some("c"), Some("A")).ref_deck.as_deref(), Some("A"));
    // `first` (prefer, or the louder deck) before the clip's own
    assert_eq!(clock(Some("A"), Some("c"), Some("B")).ref_deck.as_deref(), Some("A"));
    // no preference, no deck of its own: by name
    assert_eq!(clock(None, Some("c"), None).ref_deck.as_deref(), Some("A"));
    let c = clock(None, Some("c"), Some("B"));
    assert_eq!(c.bpm, Some(126.0));
    assert!((c.frames_per_beat - 60.0 / 126.0 * RATE).abs() < 1e-9);
    assert_eq!((c.origin_frame, c.origin_beat), (2000, 2.0));
    // a deck that holds the very clip is not its clock
    a.clip_id = Some("c".into());
    let decks2 = [b.clone(), a.clone()];
    let c = select_clock(Some("c"), Some("A"), None, &decks2, &[], 99, RATE);
    assert_eq!(c.ref_deck.as_deref(), Some("B"));
    // a stopped deck, or one with no beat state, is not either
    b.playing = false;
    let decks3 = [b.clone(), a.clone()];
    assert_eq!(select_clock(Some("c"), None, None, &decks3, &[], 99, RATE), LoopClock::none(99));
    b.playing = true;
    b.beat_state = None;
    let decks4 = [b, a];
    assert_eq!(select_clock(Some("c"), None, None, &decks4, &[], 99, RATE), LoopClock::none(99));
    // Swift's `$0.clipID != clip?.id`: with no clip at all, a deck holding no clip is not a clock
    let plain = [deck_a(0, 0.0, 120.0)];
    assert_eq!(select_clock(None, None, None, &plain, &[], 5, RATE), LoopClock::none(5));
}

#[test]
fn a_net_decks_newest_tick_anchor_is_the_origin() {
    // worked out from loopClock: origin (anchor frame, (tick - 1) / 4) for a net deck
    assert_eq!(anchor_origin(48_000, 17), (48_000, 4.0));
    assert_eq!(anchor_origin(48_100, 18), (48_100, 4.25));
    let mut d = deck_a(60_000, 5.9, 120.0);
    d.anchor = Some(anchor_origin(48_000, 17));
    let c = clock_of(d, Some("c"), Some("A"));
    assert_eq!((c.origin_frame, c.origin_beat), (48_000, 4.0));
    assert!((c.beat(60_000) - 4.5).abs() < 1e-12);
}

#[test]
fn with_no_deck_a_loop_already_going_is_the_clock() {
    // worked out from loopClock's second branch
    let v = |active, stopping, clip: &str, beats, pos| ClockVoice {
        active,
        stopping,
        clip_id: Some(clip.into()),
        clip_beats: beats,
        clip_length: 96_000,
        length: 96_000.0,
        position: pos,
        target_bpm: 120.0,
    };
    // 4 beats in 96 000 samples: 24 000 a beat; a playhead at 30 000 is beat 1.25
    let voices = [v(true, false, "other", Some(4), 30_000.0)];
    let c = select_clock(Some("c"), Some("A"), None, &[], &voices, 5000, RATE);
    assert_eq!(c.ref_deck, None);
    assert_eq!((c.origin_frame, c.origin_beat), (5000, 1.25));
    assert!((c.frames_per_beat - 24_000.0).abs() < 1e-9 && c.bpm == Some(120.0));
    // the playhead wraps, a negative one too
    let voices = [v(true, false, "other", Some(4), 96_000.0 + 30_000.0)];
    assert_eq!(select_clock(Some("c"), None, None, &[], &voices, 0, RATE).origin_beat, 1.25);
    let voices = [v(true, false, "other", Some(4), -66_000.0)];
    assert_eq!(select_clock(Some("c"), None, None, &[], &voices, 0, RATE).origin_beat, 1.25);
    // the clip's own voice, a stopping one and a silent one are not clocks
    for voices in [
        [v(true, false, "c", Some(4), 0.0)],
        [v(true, true, "other", Some(4), 0.0)],
        [v(false, false, "other", Some(4), 0.0)],
    ] {
        assert_eq!(select_clock(Some("c"), None, None, &[], &voices, 7, RATE), LoopClock::none(7));
    }
    // the first voice that qualifies ends the search, even when its clip is gone from the row
    let voices = [
        v(true, false, "gone", None, 0.0),
        v(true, false, "other", Some(4), 30_000.0),
    ];
    assert_eq!(select_clock(Some("c"), None, None, &[], &voices, 7, RATE), LoopClock::none(7));
    // a voice with no audio loaded yet uses its clip's length
    let mut loading = v(true, false, "other", Some(4), 24_000.0);
    loading.length = 0.0;
    assert_eq!(
        select_clock(Some("c"), None, None, &[], &[loading], 0, RATE).origin_beat,
        1.0
    );
    // a deck wins over a voice
    let d = [deck_a(0, 0.0, 100.0)];
    let c = select_clock(Some("c"), Some("A"), None, &d, &[v(true, false, "other", Some(4), 0.0)], 0, RATE);
    assert_eq!(c.ref_deck.as_deref(), Some("A"));
}

#[test]
fn a_waiting_boundary_names_the_deck_and_its_tempo() {
    let c = clock_of(deck_a(0, 0.0, 120.0), Some("c"), Some("A"));
    assert_eq!(
        c.waiting_boundary(32),
        Some(WaitingBoundary {
            beats: 32,
            ref_deck: "A".into(),
            bpm: 120.0,
            follow: false
        })
    );
}

// MARK: a waiting start follows its deck

#[test]
fn a_waiting_loop_follows_its_deck_when_the_tempo_moves_during_the_countdown() {
    // LoopLaunchTests.aWaitingLoopFollowsItsDeckWhenTheTempoMovesDuringTheCountdown, worked out
    // for the pure decision: quantum 32, a start planned on an 8-bar line of A at 120 bpm; a
    // bar into the wait A's tempo goes up 5 %, its line comes sooner
    let one = {
        let mut c = clip(1, 1);
        c.source = "A".into();
        c
    };
    let line_beat = 32.0;
    // A at 120 bpm: beat 2.0 at frame 100_000 (so the line is 30 beats on)
    let before = clock_of(deck_a(100_000, 2.0, 120.0), Some(&one.id), Some("A"));
    let planned = before.next_boundary(100_000 + LOOP_LEAD, 32);
    assert!(on_line(before.beat(planned), 32.0));
    assert_eq!(before.beat(planned).round(), line_beat);
    let wait = before.waiting_boundary(32).unwrap();
    let now = 100_000 + 96_000;
    let voice = RetimeVoice {
        boundary: Some(wait.clone()),
        stop_frame: None,
        start_frame: Some(planned),
        buffer_length: 96_000.0,
        active: false,
    };
    // the tempo has not moved: nothing to do (and `follow` stays unset)
    let same = clock_of(deck_a(now, 6.0, 120.0), Some(&one.id), Some("A"));
    assert_eq!(retime_waiting(&voice, &one, &same, now), None);
    // A at 126 bpm (pitch +5 %) from the same beat: its line comes sooner
    let after = clock_of(deck_a(now, 6.0, 126.0), Some(&one.id), Some("A"));
    let r = retime_waiting(&voice, &one, &after, now).unwrap();
    assert!(r.follow);
    let (at, seek) = match r.moved {
        Some(RetimeMove::Start { at, seek }) => (at, seek),
        other => panic!("a moved start: {other:?}"),
    };
    assert!(at < planned - 10_000, "follows the deck: {at} vs {planned}");
    assert!((after.beat(at) - line_beat).abs() < 0.01, "the same line of A");
    // seeked to the clip's phase there: clip.source == "A", so minus its startBeat (0 for clip 1)
    // the line is a multiple of the clip's 4 beats: phase 0
    let phase = seek / 96_000.0;
    assert!(phase.min(1.0 - phase) < 0.01, "voice phase on the downbeat: {phase}");
    // once it follows, a tempo that comes back still moves it (follow stays on)
    let mut voice2 = voice.clone();
    voice2.boundary.as_mut().unwrap().follow = true;
    voice2.start_frame = Some(at);
    voice2.boundary.as_mut().unwrap().bpm = 126.0;
    let back = clock_of(deck_a(now, 6.0, 120.0), Some(&one.id), Some("A"));
    let r = retime_waiting(&voice2, &one, &back, now).unwrap();
    assert!(r.follow && matches!(r.moved, Some(RetimeMove::Start { .. })));
    // another deck's clock, or no boundary, is not for this voice
    let mut b = deck_a(now, 6.0, 126.0);
    b.name = "B".into();
    let other = select_clock(Some(&one.id), None, Some("B"), &[b], &[], now, RATE);
    assert_eq!(retime_waiting(&voice, &one, &other, now), None);
    let mut none = voice.clone();
    none.boundary = None;
    assert_eq!(retime_waiting(&none, &one, &after, now), None);
}

#[test]
fn a_waiting_stop_follows_its_deck_and_only_while_it_is_sounding() {
    // retimeWaiting's stop branch, worked out
    let one = clip(1, 1);
    let before = clock_of(deck_a(0, 0.0, 120.0), Some(&one.id), Some("A"));
    let stop = before.next_boundary(LOOP_LEAD, 16);
    let voice = RetimeVoice {
        boundary: before.waiting_boundary(16),
        stop_frame: Some(stop),
        start_frame: None,
        buffer_length: 96_000.0,
        active: true,
    };
    let now = 1000;
    let after = clock_of(deck_a(0, 0.0, 126.0), Some(&one.id), Some("A"));
    let r = retime_waiting(&voice, &one, &after, now).unwrap();
    assert!(r.follow);
    let at = match r.moved {
        Some(RetimeMove::Stop { at }) => at,
        other => panic!("a moved stop: {other:?}"),
    };
    assert!(at < stop && (after.beat(at) - 16.0).abs() < 0.01);
    // not sounding any more: nothing to re-send, but it still follows
    let mut quiet = voice.clone();
    quiet.active = false;
    let r = retime_waiting(&quiet, &one, &after, now).unwrap();
    assert!(r.follow && r.moved.is_none());
    // too close to the render head (within 2 * LOOP_LEAD): the audio thread may have reached it
    let mut near = voice;
    near.stop_frame = Some(now + 2 * LOOP_LEAD);
    let r = retime_waiting(&near, &one, &after, now).unwrap();
    assert!(r.moved.is_none());
}

#[test]
fn a_start_only_moves_by_less_than_a_quarter_of_the_line_spacing() {
    // retimeWaiting: reach = boundary.beats / 4 * framesPerBeat; moved(old, new) wants
    // old > now + 2 * LOOP_LEAD, |new - old| > RETIME_TOLERANCE and < reach
    let one = clip(1, 1);
    let c = clock_of(deck_a(0, 0.0, 120.0), Some(&one.id), Some("A"));
    let b = WaitingBoundary {
        beats: 4,
        ref_deck: "A".into(),
        bpm: 119.0,
        follow: false,
    };
    let mk = |start: i64| RetimeVoice {
        boundary: Some(b.clone()),
        stop_frame: None,
        start_frame: Some(start),
        buffer_length: 96_000.0,
        active: false,
    };
    // 24 000 frames a beat, one beat of reach on a 4-beat line; the nearest line to 100_000 is 96_000
    assert_eq!(c.nearest_boundary(100_000, 4), 96_000);
    let moved = |start: i64| retime_waiting(&mk(start), &one, &c, 0).unwrap().moved;
    assert!(matches!(moved(100_000), Some(RetimeMove::Start { at: 96_000, .. })));
    assert_eq!(moved(96_000 + RETIME_TOLERANCE), None, "inside the tolerance");
    assert!(moved(96_000 + RETIME_TOLERANCE + 1).is_some());
    assert_eq!(moved(2 * LOOP_LEAD), None, "not well ahead of the render head");
}

// MARK: the library's entries

#[test]
fn a_launch_setting_is_written_only_when_set() {
    // LoopLaunchTests.clipLaunchSurvivesTheLibraryAndIsLeftOutWithout and
    // aClipsFollowSurvivesTheLibraryAndIsLeftOutWithout: nothing when unset, byte for byte
    let row = [clip(1, 1), clip(2, 1)];
    let plain = library::index_text(&row, &BTreeMap::new(), &[]);
    assert!(!plain.contains("quantum") && !plain.contains("\"fade\"") && !plain.contains("\"follow\""));
    assert!(plain.ends_with("}\n"));
    assert_eq!(
        library::entry(&row[0], &ClipLaunch::default()),
        library::entry(&row[0], &ClipLaunch::default())
    );
    let launch = BTreeMap::from([(
        row[0].id.clone(),
        ClipLaunch {
            quantum: Some(8),
            fade: Some(LoopFade::Beat),
            follow: None,
        },
    )]);
    let text = library::index_text(&row, &launch, &[]);
    assert!(text.contains(r#""fade":"beat""#) && text.contains(r#""quantum":8"#));
    let doc = JsonValue::parse(&text).unwrap();
    let entries = doc.get("loops").unwrap().as_array().unwrap();
    assert_eq!(library::load_launch(entries), launch);
    // the follow: ClipLaunch(fade: .bar, follow: .source) on the second
    let launch = BTreeMap::from([(
        row[1].id.clone(),
        ClipLaunch {
            quantum: None,
            fade: Some(LoopFade::Bar),
            follow: Some(LoopFollow::Source),
        },
    )]);
    let text = library::index_text(&row, &launch, &[]);
    assert!(text.contains(r#""follow":"source""#));
    let doc = JsonValue::parse(&text).unwrap();
    assert_eq!(
        library::load_launch(doc.get("loops").unwrap().as_array().unwrap()),
        launch
    );
    assert_eq!(library::index_text(&row, &BTreeMap::new(), &[]), plain, "byte for byte");
    // an unknown raw string is no setting
    let e = JsonValue::parse(r#"{"id":"x","fade":"nonsense","follow":"nonsense"}"#).unwrap();
    assert!(library::launch_from_entry(&e).is_empty());
}

#[test]
fn the_library_keeps_a_sequences_steps_and_a_clips_provenance() {
    // LoopLibraryTests.theLibraryKeepsASequencesSteps
    let plain = clip(5, 2);
    let mut seq = plain.clone();
    seq.id = format!("seq-{}", plain.id);
    seq.title = "seq x×2→y".into();
    seq.source = "A".into();
    seq.start_beat = 0.0;
    seq.steps = Some(vec![SequenceStep::new("x", 2), SequenceStep::new("y", 1)]);
    let one = clip(1, 1);
    let audio = |c: &LoopClip| {
        let b = c.buffer.clone();
        move |_: &str| Some(SampleBuffer::new(b.left.clone(), Some(b.right().to_vec()), RATE))
    };
    let text = library::index_text(&[one.clone(), seq.clone()], &BTreeMap::new(), &[]);
    let doc = JsonValue::parse(&text).unwrap();
    let entries = doc.get("loops").unwrap().as_array().unwrap();
    assert_eq!(doc.get("schema").and_then(JsonValue::as_str), Some("djrust/loops/v1"));
    let back0 = library::clip_from_entry(&entries[0], audio(&one)).unwrap();
    let back1 = library::clip_from_entry(&entries[1], audio(&plain)).unwrap();
    assert!(back0.steps.is_none() && back1.steps == seq.steps);
    assert_eq!(back1.id, seq.id, "still a sequence's id after a reload");
    // provenance is kept as it was written
    let p = JsonValue::parse(r#"{"deck":"B","bar":9}"#).unwrap();
    let mut with_p = plain.clone();
    with_p.source = "B".into();
    with_p.start_beat = 32.0;
    with_p.provenance = Some(p.clone());
    let text = library::index_text(&[with_p.clone()], &BTreeMap::new(), &[]);
    let doc = JsonValue::parse(&text).unwrap();
    let back = library::clip_from_entry(&doc.get("loops").unwrap().as_array().unwrap()[0], audio(&plain))
        .unwrap();
    assert_eq!(back.provenance.unwrap().canonical_js(), p.canonical_js());
    assert_eq!((back.source.as_str(), back.start_beat, back.beats), ("B", 32.0, 8));
}

#[test]
fn a_saved_row_comes_back_as_it_was() {
    // LoopLibraryTests.aSavedRowComesBackAsItWas: same audio, same order, titles, bars
    // [1, 2, 1], sources [A, B, A], start beats [4, 8, 12], all 120 bpm
    let row = [clip(1, 1), clip(2, 2), clip(3, 1)];
    let text = library::index_text(&row, &BTreeMap::new(), &[]);
    let doc = JsonValue::parse(&text).unwrap();
    let back: Vec<LoopClip> = doc
        .get("loops")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .zip(&row)
        .map(|(e, c)| {
            let b = c.buffer.clone();
            library::clip_from_entry(e, move |_| {
                Some(SampleBuffer::new(b.left.clone(), Some(b.right().to_vec()), RATE))
            })
            .unwrap()
        })
        .collect();
    assert_eq!(
        back.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        row.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
    );
    assert_eq!(back.iter().map(LoopClip::bars).collect::<Vec<_>>(), [1, 2, 1]);
    assert_eq!(
        back.iter().map(|c| c.source.as_str()).collect::<Vec<_>>(),
        ["A", "B", "A"]
    );
    assert_eq!(
        back.iter().map(|c| c.start_beat).collect::<Vec<_>>(),
        [4.0, 8.0, 12.0]
    );
    assert!(back.iter().all(|c| c.bpm == 120.0));
    assert_eq!(
        back.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["A loop 1", "A loop 2", "A loop 3"]
    );
    // worked out from LoopLibrary.clip: an older index with bars, no title or source
    let old = JsonValue::parse(&format!(r#"{{"id":"{}","bars":2,"bpm":100}}"#, row[0].id)).unwrap();
    let c = library::clip_from_entry(&old, |_| Some(buffer(1, 10, RATE))).unwrap();
    assert_eq!((c.beats, c.title.as_str(), c.source.as_str(), c.start_beat), (8, "loop", "A", 0.0));
    // no audio, no tempo, no length, or an id the library cannot name: no clip
    assert!(library::clip_from_entry(&old, |_| None).is_none());
    for bad in [
        r#"{"id":"x","beats":4}"#,
        r#"{"id":"x","bpm":100}"#,
        r#"{"id":"x","beats":0,"bpm":100}"#,
        r#"{"beats":4,"bpm":100}"#,
    ] {
        let e = JsonValue::parse(bad).unwrap();
        assert!(library::clip_from_entry(&e, |_| Some(buffer(1, 10, RATE))).is_none(), "{bad}");
    }
}

#[test]
fn a_clip_taken_off_the_row_goes_to_removed_with_its_place_and_when() {
    // LoopLibraryRemovalTests.removalKeepsTheAudioUnderRemovedWithItsPlace
    let (a, b, c) = (clip(1, 1), clip(2, 1), clip(3, 1));
    let entries = |row: &[&LoopClip]| -> Vec<JsonValue> {
        row.iter()
            .map(|c| JsonValue::Object(library::entry(c, &ClipLaunch::default())))
            .collect()
    };
    let keep = |ids: &[&LoopClip]| -> BTreeSet<String> { ids.iter().map(|c| c.id.clone()).collect() };
    // save([a, b, c]) then save([a, c]): b goes, from index 1
    let list = library::plan_move_off_the_row(
        &entries(&[&a, &b, &c]),
        &keep(&[&a, &c]),
        &[],
        "2026-10-29T10:00:00Z",
        &|_| true,
        1,
    )
    .unwrap();
    let removed: Vec<library::RemovedLoop> =
        list.iter().filter_map(library::RemovedLoop::from_entry).collect();
    assert_eq!(removed.len(), 1);
    assert_eq!((removed[0].id.as_str(), removed[0].index, removed[0].title.as_str()), (b.id.as_str(), 1, b.title.as_str()));
    assert!(!removed[0].removed.is_empty(), "with when");
    // then save([c]): a goes, newest first -> [a, b]
    let list2 = library::plan_move_off_the_row(
        &entries(&[&a, &c]),
        &keep(&[&c]),
        &list,
        "2026-10-29T11:00:00Z",
        &|_| true,
        1,
    )
    .unwrap();
    let ids: Vec<&str> = list2.iter().map(|e| e.get("id").unwrap().as_str().unwrap()).collect();
    assert_eq!(ids, [a.id.as_str(), b.id.as_str()], "newest first");
    // nothing leaves, nothing stray: removed.json is left alone
    assert!(library::plan_move_off_the_row(
        &entries(&[&a, &c]),
        &keep(&[&a, &c]),
        &list,
        "",
        &|_| true,
        0
    )
    .is_none());
    // an entry whose audio is gone is dropped (kept only while the audio is in removed/ or
    // about to be moved there)
    let list3 = library::plan_move_off_the_row(
        &entries(&[&a, &c]),
        &keep(&[&c]),
        &list,
        "t",
        &|id| id != b.id,
        0,
    )
    .unwrap();
    assert_eq!(list3.len(), 1);
    // a clip put back on the row leaves removed.json
    let list4 = library::plan_move_off_the_row(
        &entries(&[&a, &c]),
        &keep(&[&a, &b, &c]),
        &list,
        "t",
        &|_| true,
        0,
    )
    .unwrap();
    assert!(list4.is_empty(), "restored: {list4:?}");
    assert_eq!(
        library::removed_index_text(Vec::new()),
        "{\"removed\":[],\"schema\":\"djrust/loops-removed/v1\"}\n"
    );
    // RemovedLoop for a clip removed in this launch
    let r = RemovedClip::new(b.clone(), 1, ClipSound::default());
    let l = library::RemovedLoop::from_removed(&r, "when");
    assert_eq!((l.index, l.removed.as_str(), l.is_sequence), (1, "when", false));
}

#[test]
fn scenes_ride_along_in_the_index() {
    // LoopLibraryTests.anotherSampleRateKeepsTheIdsSoScenesAndFilesStay: lib.loadScenes() == scenes
    use djrust::journal::state::LoopScene;
    use djrust::session::types::SceneClip;
    let row = [clip(1, 1), clip(2, 2), clip(3, 1)];
    let scenes = vec![LoopScene {
        name: "Scene 1".into(),
        clips: row[..2].iter().map(|c| SceneClip::new(&c.id)).collect(),
    }];
    let text = library::index_text(&row, &BTreeMap::new(), &scenes);
    let doc = JsonValue::parse(&text).unwrap();
    let back: Vec<LoopScene> = doc
        .get("scenes")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(LoopScene::from_json)
        .collect();
    assert_eq!(back, scenes);
    // no scenes, no key
    assert!(JsonValue::parse(&library::index_text(&row, &BTreeMap::new(), &[]))
        .unwrap()
        .get("scenes")
        .is_none());
}

// MARK: import

#[test]
fn an_imports_tempo_comes_from_its_length() {
    // LoopRecordTests.anImportJoinsTheRowAndLoopsInTimeOnce: 2 * bar - 3 frames of 220 Hz in 8
    // beats: bpm = 8 * 60 * 48000 / len
    let len = 2 * 96_000 - 3;
    let b = SampleBuffer::new(vec![0.1; len], None, RATE);
    assert_eq!(import_tempo(&b, 8), 8.0 * 60.0 * 48_000.0 / len as f64);
    assert!((import_tempo(&b, 8) - 120.0).abs() < 0.01);
}

// MARK: the instruments a retune offers

fn iron() -> TrackSpec {
    TrackSpec::new("techno", 831_071.0, Some("standard"))
}

/// The project a source plays: a net's spec, `layers` (the `tracks` override) and all.
fn project_of(source: &DeckSource) -> djrust::Project {
    match source {
        DeckSource::Net(spec) => spec.project(),
        other => panic!("not a net: {other:?}"),
    }
}

#[test]
fn an_instrument_change_is_a_tracks_override_the_project_plays() {
    // FavoritesTests.anInstrumentChangeIsATracksOverrideTheProjectPlays
    let base = iron().project();
    let bass = base
        .nets
        .values()
        .find(|n| n.riff_group == "bass")
        .expect("a bass net");
    let other = djrust::gen::tables::BeatsTables::shared()
        .instrument_sets
        .get("techno")
        .and_then(|s| s.get("bass"))
        .and_then(|l| l.iter().find(|i| **i != bass.track.instrument))
        .unwrap()
        .clone();
    let source = DeckSource::Net(iron());
    let spec = source.with_instrument(&other, bass.track.channel).unwrap();
    // a second change on another channel keeps the first
    let both = spec.with_instrument("supersaw", 99).unwrap();
    let DeckSource::Net(both_spec) = &both else { panic!("a net") };
    let tracks = both_spec.layers.get("tracks").unwrap();
    assert_eq!(
        tracks
            .get(&bass.track.channel.to_string())
            .and_then(|t| t.get("instrument"))
            .and_then(JsonValue::as_str),
        Some(other.as_str())
    );
    assert_eq!(
        tracks.get("99").and_then(|t| t.get("instrument")).and_then(JsonValue::as_str),
        Some("supersaw")
    );
    let p = project_of(&spec);
    let changed: Vec<_> = p.nets.values().filter(|n| n.riff_group == "bass").collect();
    assert!(!changed.is_empty() && changed.iter().all(|n| n.track.instrument == other));
    // it is part of the share, so a favorite of it is a different track from the plain one
    assert_ne!(
        djrust::journal::stores::favorite_cid(&spec),
        djrust::journal::stores::favorite_cid(&source)
    );
    // and survives the session encoding a favorite is stored in
    assert_eq!(DeckSource::from_json(&spec.json()).unwrap(), spec);
    // nothing else is a net
    assert!(DeckSource::None.with_instrument("saw", 1).is_none());
    assert!(DeckSource::Sample { id: "x".into(), title: "t".into(), grid: None }
        .with_instrument("saw", 1)
        .is_none());
}

#[test]
fn a_raw_nets_deck_takes_the_instrument_in_its_payload() {
    // DeckVoices.withInstrument's rawNets case: the payload's `tracks`, merged into any already there
    let payload = JsonValue::parse(
        r#"{"name":"L","tempo":128,"nets":{"x":{}},"tracks":{"2":{"instrument":"bass","kit":"k"}}}"#,
    )
    .unwrap();
    let raw = DeckSource::RawNets(payload);
    let DeckSource::RawNets(p) = raw.with_instrument("acid", 2).unwrap() else { panic!() };
    assert_eq!(
        p.get("tracks").unwrap().canonical_js(),
        r#"{"2":{"instrument":"acid","kit":"k"}}"#
    );
    let DeckSource::RawNets(p) = raw.with_instrument("pad", 5).unwrap() else { panic!() };
    assert_eq!(
        p.get("tracks").unwrap().js_stringified(),
        r#"{"2":{"instrument":"bass","kit":"k"},"5":{"instrument":"pad"}}"#
    );
    // a payload that is not an object is not a net
    assert!(DeckSource::RawNets(JsonValue::Null).with_instrument("pad", 5).is_none());
}

#[test]
fn a_track_field_is_set_cleared_and_removed_with_its_empty_parents() {
    // DeckKits.withTrackField / withKit: nil or "" clears the field, an emptied entry goes, an
    // emptied `tracks` goes
    let net = DeckSource::Net(iron());
    let kit = net.with_kit(Some("808"), &[10, 11]).unwrap();
    let DeckSource::Net(d) = &kit else { panic!() };
    assert_eq!(d.layers.get("tracks").unwrap().js_stringified(), r#"{"10":{"kit":"808"},"11":{"kit":"808"}}"#);
    let with_inst = kit.with_instrument("saw", 10).unwrap();
    let DeckSource::Net(d) = &with_inst else { panic!() };
    assert_eq!(
        d.layers.get("tracks").unwrap().js_stringified(),
        r#"{"10":{"kit":"808","instrument":"saw"},"11":{"kit":"808"}}"#
    );
    // clearing the kit leaves the instrument; "" is the same as None
    let cleared = with_inst.with_kit(None, &[10, 11]).unwrap();
    let DeckSource::Net(d) = &cleared else { panic!() };
    assert_eq!(d.layers.get("tracks").unwrap().js_stringified(), r#"{"10":{"instrument":"saw"}}"#);
    let back = kit.with_kit(Some(""), &[10, 11]).unwrap();
    let DeckSource::Net(d) = &back else { panic!() };
    assert!(d.layers.get("tracks").is_none(), "an emptied `tracks` goes");
    assert_eq!(back, net);
    let pal = net.with_palette(Some("p"), &[3]).unwrap();
    let DeckSource::Net(d) = &pal else { panic!() };
    assert_eq!(d.layers.get("tracks").unwrap().js_stringified(), r#"{"3":{"palette":"p"}}"#);
}

#[test]
fn a_decks_roles_with_an_instrument_to_choose() {
    // DeckVoices.RoleVoice.list: (VoiceTweakTests/LoopRetuneTests use roleVoices): roles in the
    // order given, roles sharing a channel merged, this genre's instruments first
    let project = iron().project();
    let roles: Vec<String> = ["kick", "snare", "hihat", "bass", "melody", "harmony", "arp", "nonsense"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let catalog = VoiceCatalog::bundled();
    let voices = RoleVoice::list(&project, &roles, "techno", &catalog);
    assert!(!voices.is_empty());
    // every channel once; a role that matches no net is left out
    let channels: BTreeSet<i64> = voices.iter().map(|v| v.channel).collect();
    assert_eq!(channels.len(), voices.len());
    assert!(voices.iter().all(|v| !v.role.contains("nonsense") && !v.options.is_empty()));
    // the instrument the net plays is among the options
    assert!(voices.iter().all(|v| v.options.contains(&v.instrument)));
    // the drums share a channel: one choice named with the roles joined
    if let Some(drums) = voices.iter().find(|v| v.role.contains(" · ")) {
        assert!(drums.role.starts_with("kick") || drums.role.starts_with("snare"));
    }
    // this genre's list comes first, without repeats
    let bass = voices.iter().find(|v| v.role == "bass").expect("a bass role");
    let own = djrust::gen::tables::BeatsTables::shared()
        .instrument_sets
        .get("techno")
        .and_then(|s| s.get("bass"))
        .unwrap();
    let mut want = own.clone();
    if !want.contains(&bass.instrument) {
        want.insert(0, bass.instrument.clone());
    }
    assert_eq!(&bass.options[..want.len()], &want[..]);
    assert_eq!(
        bass.options.iter().collect::<BTreeSet<_>>().len(),
        bass.options.len(),
        "no repeats"
    );
    // a role beats lists no instruments for (harmony) is offered what fits it
    if let Some(h) = voices.iter().find(|v| v.role == "harmony") {
        assert!(!h.options.is_empty());
    }
}

// MARK: a deck's layers

#[test]
fn a_raw_tracks_layers_and_key_come_from_its_payload() {
    // WanderLayerTests.aRawTracksLayersAndKeyComeFromItsPayload: the roll's envelope
    let env = djrust::roll::envelope(
        &JsonValue::parse(
            r#"{"name":"L","tempo":128,"steps":128,"key":"A minor",
                "layers":[{"loop":"/lib/Vox A.caf","name":"Vox A","bar":3,"bars":4,"beats":16,"category":"Vocals"}],
                "parts":{"kick":{"every":16,"until":128,"notes":[[0],[4],[8],[12]]},
                         "bass":{"every":16,"until":128,"notes":[[2,45,2],[6,45,2],[10,45,2],[14,45,2]]}}}"#,
        )
        .unwrap(),
    )
    .unwrap();
    let (entries, key) = reset_layers(&DeckSource::RawNets(env));
    assert_eq!(entries.len(), 1);
    assert_eq!(key.as_deref(), Some("A minor"));
    let e = &entries[0];
    assert_eq!(
        (e.loop_.name.as_str(), e.loop_.category.as_str(), e.start, e.length),
        ("Vox A", "Vocals", 3, 4)
    );
    // a composed deck, a sample and an empty deck have none
    for s in [
        DeckSource::Net(iron()),
        DeckSource::None,
        DeckSource::Sample { id: "x".into(), title: "t".into(), grid: None },
    ] {
        assert_eq!(reset_layers(&s), (Vec::new(), None));
    }
    // layers that do not parse are none
    let bad = JsonValue::parse(r#"{"loopLayers":"nope","djKey":"C major"}"#).unwrap();
    assert_eq!(reset_layers(&DeckSource::RawNets(bad)), (Vec::new(), Some("C major".to_string())));
}

#[test]
fn the_layer_key_is_the_tracks_own_else_the_genres_scale_and_root() {
    // WanderLayerTests: d.layerKey == "A minor"; the rest worked out from DeckLayers.layerKey
    assert_eq!(layer_key(Some("A minor"), None).as_deref(), Some("A minor"));
    assert_eq!(layer_key(Some("A minor"), Some(("major", 0))).as_deref(), Some("A minor"));
    assert_eq!(layer_key(None, None), None);
    assert_eq!(layer_key(None, Some(("minor", 9))).as_deref(), Some("A minor"));
    assert_eq!(layer_key(None, Some(("Natural Minor", 3))).as_deref(), Some("Eb minor"));
    assert_eq!(layer_key(None, Some(("major", 0))).as_deref(), Some("C major"));
    assert_eq!(layer_key(None, Some(("mixolydian", 6))).as_deref(), Some("F# major"));
    for scale in ["dorian", "phrygian", "aeolian", "blues"] {
        assert_eq!(layer_key(None, Some((scale, 11))).as_deref(), Some("B minor"), "{scale}");
    }
    // the root wraps: -1 is B, 13 is C#
    assert_eq!(layer_key(None, Some(("major", -1))).as_deref(), Some("B major"));
    assert_eq!(layer_key(None, Some(("major", 13))).as_deref(), Some("C# major"));
    assert_eq!(layer_key(None, Some(("major", 8))).as_deref(), Some("Ab major"));
}
