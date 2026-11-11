//! Swift goldens for the loop row's clip model (`tools/paritydump/loopclips.swift`, section
//! "loopclips" -> `tests/fixtures/swift/loopclips.json`): content ids, fade frames, the beat
//! clock's grid, stretch / crop / sequence audio (sha256 of the f32 bit patterns), provenance,
//! the library's index texts, `RoleVoice.list` and the `withInstrument` family. Every
//! comparison is exact: strings and canonical JSON by equality, doubles by bit pattern,
//! audio by hash (see `stretch_matches_swift` for the one float-DSP stage).
//!
//! The inputs the dump built are rebuilt here the same way; the dump's file and this one must
//! stay in step. The fixture is committed: the test panics when it is missing.

mod common;
use common::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use djrust::deck::clips::*;
use djrust::deck::clock::LoopClock;
use djrust::deck::library;
use djrust::deck::loops::{take_provenance, LoopFollow, Refusal};
use djrust::deck::provenance;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::perform::ROLE_ORDER as DECK_ROLE_ORDER;
use djrust::deck::voices::RoleVoice;
use djrust::journal::state::LoopScene;
use djrust::json::{JsonObject, JsonValue};
use djrust::session::model::DeckSource;
use djrust::session::types::{LoopFx, LoopFxMode, SceneClip, SequenceStep};
use djrust::voices::VoiceCatalog;
use djrust::wander::carry::project_drum_channels;
use djrust::TrackSpec;

const RATE: f64 = 48_000.0;

fn golden() -> JsonValue {
    fixture("loopclips")
}

// MARK: the inputs, as the dump builds them

fn ramp(n: i32, len: usize, rate: f64) -> SampleBuffer {
    let l: Vec<f32> = (0..len)
        .map(|i| n as f32 * 0.01f32 + (i % 100) as f32 * 1e-4f32)
        .collect();
    let r: Vec<f32> = l.iter().map(|x| -x).collect();
    SampleBuffer::new(l, Some(r), rate)
}

fn noise(len: usize, rate: f64) -> SampleBuffer {
    let l: Vec<f32> = (0..len)
        .map(|i| ((i as i64 * 2_654_435_761) % 2001) as f32 / 1000.0f32 - 1.0f32)
        .collect();
    let r: Vec<f32> = (0..len)
        .map(|i| ((i as i64 * 40_503 + 17) % 2001) as f32 / 1000.0f32 - 1.0f32)
        .collect();
    SampleBuffer::new(l, Some(r), rate)
}

#[allow(clippy::too_many_arguments)]
fn lc_clip(
    n: i32,
    bars: i64,
    bpm: f64,
    provenance: Option<JsonValue>,
    color: Option<&str>,
    fx: LoopFx,
    buffer: Option<SampleBuffer>,
    beats: Option<i64>,
) -> LoopClip {
    let b = buffer.unwrap_or_else(|| ramp(n, 96_000 * bars as usize, RATE));
    let waveform = djrust::deck::waveform::WaveformOverview::new(&b, 40);
    let mut c = LoopClip::new(
        &b.content_id(),
        &format!("A loop {n}"),
        if n % 2 == 0 { "B" } else { "A" },
        beats.unwrap_or(bars * 4),
        bpm,
        4.0 * n as f64,
        Arc::new(b),
        waveform,
    );
    c.provenance = provenance;
    c.color = color.map(String::from);
    c.fx = fx;
    c
}

fn plain(n: i32) -> LoopClip {
    lc_clip(n, 1, 120.0, None, None, LoopFx::default(), None, None)
}

fn crop_prov() -> JsonValue {
    JsonValue::parse(
        r#"{"deck":"B","title":"techno 7","mode":"net","source":{"kind":"net","share":{"genre":"techno","seed":7}},"section":{"index":2,"name":"drop"}}"#,
    )
    .unwrap()
}

fn tube() -> LoopFx {
    LoopFx {
        drive: 6.0,
        mode: LoopFxMode::Tube,
        ..LoopFx::default()
    }
}

fn hard() -> LoopFx {
    LoopFx {
        drive: 9.0,
        mode: LoopFxMode::Hard,
        amount: 0.55,
        tone: 0.75,
        ..LoopFx::default()
    }
}

fn floats_sha(x: &[f32]) -> String {
    let mut h = Sha256::new();
    for f in x {
        h.update(f.to_bits().to_le_bytes());
    }
    djrust::cid::hex(&h.finalize())
}

fn bytes_sha(x: &[u8]) -> String {
    djrust::cid::hex(&Sha256::digest(x))
}

fn bits(d: f64) -> String {
    bits_hex(d)
}

fn opt_str(v: &JsonValue, key: &str) -> Option<String> {
    v.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn opt_bits(v: Option<f64>) -> Option<String> {
    v.map(bits_hex)
}

fn without_recorded(v: &JsonValue) -> JsonValue {
    let mut o = v.as_object().cloned().unwrap_or_default();
    o.remove("recorded");
    JsonValue::Object(o)
}

// MARK: 1-4

#[test]
fn content_ids_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "contentIds") {
        let name = s(row, "name");
        let b = match name {
            "ramp1" => ramp(1, 96_000, RATE),
            "ramp2" => ramp(2, 96_000, RATE),
            "ramp3x2bars" => ramp(3, 192_000, RATE),
            "ramp1_44100" => ramp(1, 1000, 44_100.0),
            "tiny" => ramp(5, 1, RATE),
            "mono" => SampleBuffer::new((0..1000).map(|i| i as f32 * 1e-3).collect(), None, RATE),
            "noise" => noise(5000, RATE),
            other => panic!("unknown buffer {other}"),
        };
        d.eq(format!("{name} length"), b.length() as i64, i(row, "length"));
        d.eq(format!("{name} id"), b.content_id(), s(row, "id").to_string());
        d.eq(
            format!("{name} sequence id"),
            sequence_id(&b),
            s(row, "seqId").to_string(),
        );
    }
    d.finish("content ids");
}

#[test]
fn fade_frames_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "fades") {
        let fade = LoopFade::from_raw(s(row, "fade")).expect("a fade");
        let bpm = f(row, "bpm");
        let short = row.get("short").and_then(JsonValue::as_bool).unwrap();
        let tag = format!("{} {} @{} short {}", f(row, "rate"), fade.raw(), bpm, short);
        d.eq(format!("{tag} label"), fade.label(), s(row, "label"));
        d.eq(
            format!("{tag} frames"),
            fade.frames(f(row, "rate"), bpm, short),
            i(row, "frames"),
        );
    }
    d.finish("fade frames");
}

#[test]
fn the_clock_grid_matches_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    let unbits = |h: &str| f64::from_bits(u64::from_str_radix(h, 16).unwrap());
    for row in rows(&doc, "clocks") {
        if row.get("noTempo").is_some() {
            let c = LoopClock {
                ref_deck: Some("A".into()),
                origin_frame: 0,
                origin_beat: 0.0,
                frames_per_beat: 24_000.0,
                bpm: None,
            };
            d.eq(
                "no tempo: no waiting boundary",
                c.waiting_boundary(4).is_none(),
                row.get("waiting") == Some(&JsonValue::Null),
            );
            continue;
        }
        let frames: Vec<i64> = row.get("frames").and_then(JsonValue::ints).unwrap_or_else(|| {
            // the "none" row carries no frames of its own
            vec![
                -100_000, -1, 0, 1, 24_000, 47_999, 48_000, 96_000, 96_001, 123_456, 200_000,
                777_777,
            ]
        });
        let beats = row.get("beats").and_then(JsonValue::as_array).unwrap();
        if row.get("none").is_some() {
            let c = LoopClock::none(500);
            for (k, fr) in frames.iter().enumerate() {
                let tag = format!("none frame {fr}");
                d.eq(
                    format!("{tag} beat"),
                    bits(c.beat(*fr)),
                    beats[k].as_str().unwrap().to_string(),
                );
                d.eq(
                    format!("{tag} next"),
                    c.next_boundary(*fr, 4),
                    row.get("next").unwrap().ints().unwrap()[k],
                );
                d.eq(
                    format!("{tag} nearest"),
                    c.nearest_boundary(*fr, 4),
                    row.get("nearest").unwrap().ints().unwrap()[k],
                );
            }
            d.eq(
                "none: no waiting boundary",
                c.waiting_boundary(4).is_none(),
                row.get("waiting") == Some(&JsonValue::Null),
            );
            continue;
        }
        let bpm = unbits(s(row, "bpm"));
        let fpb = unbits(s(row, "framesPerBeat"));
        let c = LoopClock {
            ref_deck: Some("A".into()),
            origin_frame: i(row, "originFrame"),
            origin_beat: unbits(s(row, "originBeat")),
            frames_per_beat: 60.0 / bpm * RATE,
            bpm: Some(bpm),
        };
        let tag = format!("origin {} {} @{bpm}", c.origin_frame, c.origin_beat);
        d.eq(format!("{tag} frames per beat"), bits(c.frames_per_beat), bits(fpb));
        let everys = row.get("everys").unwrap().ints().unwrap();
        let down = row.get("downbeats").unwrap().ints().unwrap();
        let next = row.get("next").and_then(JsonValue::as_array).unwrap();
        let nearest = row.get("nearest").and_then(JsonValue::as_array).unwrap();
        for (k, fr) in frames.iter().enumerate() {
            d.eq(
                format!("{tag} beat({fr})"),
                bits(c.beat(*fr)),
                beats[k].as_str().unwrap().to_string(),
            );
            d.eq(format!("{tag} downbeat({fr})"), c.next_downbeat(*fr), down[k]);
            let nx = next[k].ints().unwrap();
            let ne = nearest[k].ints().unwrap();
            for (e, every) in everys.iter().enumerate() {
                d.eq(
                    format!("{tag} next({fr}, {every})"),
                    c.next_boundary(*fr, *every),
                    nx[e],
                );
                d.eq(
                    format!("{tag} nearest({fr}, {every})"),
                    c.nearest_boundary(*fr, *every),
                    ne[e],
                );
            }
        }
        let waiting = row.get("waiting").and_then(JsonValue::as_array).unwrap();
        for (e, every) in everys.iter().enumerate() {
            let got = c.waiting_boundary(*every).unwrap();
            let want = &waiting[e];
            d.eq(format!("{tag} waiting({every}) beats"), got.beats, i(want, "beats"));
            d.eq(
                format!("{tag} waiting({every}) ref"),
                got.ref_deck.as_str(),
                s(want, "ref"),
            );
            d.eq(format!("{tag} waiting({every}) bpm"), bits(got.bpm), s(want, "bpm").to_string());
            d.eq(
                format!("{tag} waiting({every}) follow"),
                got.follow,
                want.get("follow").and_then(JsonValue::as_bool).unwrap(),
            );
        }
    }
    d.finish("clock grid");
}

#[test]
fn stretch_matches_swift() {
    // The one float-DSP stage of the clip model: linear interpolation in f32, one multiply and
    // one add per sample, the position in f64. Rust does not fuse them (no -ffp-contract) and
    // neither does Swift, so every hash is compared exactly, no tolerance.
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "stretch") {
        let b = match s(row, "source") {
            "ramp" => ramp(1, 96_000, RATE),
            "noise" => noise(5000, RATE),
            "mono" => SampleBuffer::new(
                (0..777).map(|i| (i % 37) as f32 * 0.02f32 - 0.3f32).collect(),
                None,
                RATE,
            ),
            other => panic!("unknown source {other}"),
        };
        let factor = f64::from_bits(u64::from_str_radix(s(row, "factor"), 16).unwrap());
        let (l, r) = stretch(&b, factor);
        let tag = format!("{} x{factor}", s(row, "source"));
        d.eq(format!("{tag} length"), l.len() as i64, i(row, "length"));
        d.eq(format!("{tag} left"), floats_sha(&l), s(row, "lsha").to_string());
        d.eq(format!("{tag} right"), floats_sha(&r), s(row, "rsha").to_string());
    }
    d.finish("stretch");
}

// MARK: crop and sequence

fn crop_source(name: &str) -> LoopClip {
    match name {
        "oneBar" => lc_clip(1, 1, 120.0, Some(crop_prov()), Some("teal"), tube(), None, None),
        "twoBars" => lc_clip(2, 2, 120.0, None, None, LoopFx::default(), None, None),
        "odd1001" => lc_clip(4, 1, 120.0, None, None, LoopFx::default(), Some(ramp(4, 1001, RATE)), Some(4)),
        "mono600" => lc_clip(
            6,
            1,
            120.0,
            None,
            None,
            LoopFx::default(),
            Some(SampleBuffer::new(
                (0..600).map(|i| i as f32 * 1e-3).collect(),
                None,
                RATE,
            )),
            Some(6),
        ),
        other => panic!("unknown clip {other}"),
    }
}

#[test]
fn crops_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "crops") {
        let name = s(row, "name");
        let (from, to) = (i(row, "from"), i(row, "to"));
        let tag = format!("{name} {from}..{to}");
        let src = crop_source(name);
        d.eq(format!("{tag} source id"), src.id.clone(), s(row, "clip").to_string());
        let got = crop_clip(&src, from, to);
        if row.get("id") == Some(&JsonValue::Null) {
            d.eq(format!("{tag} refused"), got.is_none(), true);
            continue;
        }
        let Some(c) = got else {
            d.eq(format!("{tag} cropped"), false, true);
            continue;
        };
        d.eq(format!("{tag} id"), c.id.clone(), s(row, "id").to_string());
        d.eq(format!("{tag} beats"), c.beats, i(row, "beats"));
        d.eq(format!("{tag} bpm"), bits(c.bpm), s(row, "bpm").to_string());
        d.eq(format!("{tag} startBeat"), bits(c.start_beat), s(row, "startBeat").to_string());
        d.eq(format!("{tag} title"), c.title.clone(), s(row, "title").to_string());
        d.eq(format!("{tag} color"), c.color.clone(), opt_str(row, "color"));
        d.eq(format!("{tag} source"), c.source.clone(), s(row, "source").to_string());
        d.eq(format!("{tag} length"), c.buffer.length() as i64, i(row, "length"));
        d.eq(format!("{tag} left"), floats_sha(&c.buffer.left), s(row, "lsha").to_string());
        d.eq(format!("{tag} right"), floats_sha(c.buffer.right()), s(row, "rsha").to_string());
        d.eq(format!("{tag} label"), c.length_label(), s(row, "lengthLabel").to_string());
        d.eq(
            format!("{tag} provenance"),
            c.provenance.as_ref().map(|p| p.canonical_js()).unwrap_or("null".into()),
            s(row, "provenance").to_string(),
        );
        d.eq(format!("{tag} fx"), c.fx.json().canonical_js(), s(row, "fx").to_string());
        d.eq(
            format!("{tag} content id of the buffer"),
            c.buffer.content_id(),
            s(row, "bufferContentId").to_string(),
        );
    }
    d.finish("crops");
}

fn sequence_clips(name: &str) -> (Vec<LoopClip>, Vec<SequenceStep>) {
    let qa = lc_clip(1, 1, 120.0, Some(crop_prov()), None, LoopFx::default(), None, None);
    let qb = lc_clip(2, 2, 120.0, None, None, LoopFx::default(), None, None);
    let mut quick = lc_clip(7, 1, 120.0, None, None, LoopFx::default(), None, None);
    quick.beats = 4;
    quick.bpm = 240.0;
    quick.start_beat = 0.0;
    let slow_buf = ramp(8, 90_000, RATE);
    let slow_wave = djrust::deck::waveform::WaveformOverview::new(&slow_buf, 40);
    let slow = LoopClip::new(
        &slow_buf.content_id(),
        "slow  8 ",
        "A",
        12,
        90.0,
        8.0,
        Arc::new(slow_buf),
        slow_wave,
    );
    let bypassed = LoopFx {
        bypass: true,
        ..hard()
    };
    let fa = lc_clip(11, 1, 120.0, None, None, hard(), None, None);
    let fb = lc_clip(12, 1, 120.0, None, None, hard(), None, None);
    let fc = lc_clip(13, 1, 120.0, None, None, bypassed, None, None);
    let fd = lc_clip(14, 1, 120.0, None, None, LoopFx::default(), None, None);
    let st = |c: &LoopClip, r: i64| SequenceStep::new(&c.id, r);
    match name {
        "back to back" => (vec![qa.clone(), qb.clone()], vec![st(&qa, 2), st(&qb, 1)]),
        "tempo 240" => (vec![qa.clone(), quick.clone()], vec![st(&qa, 1), st(&quick, 1)]),
        "tempo 90" => (vec![qa.clone(), slow.clone()], vec![st(&qa, 1), st(&slow, 3)]),
        "first is slow" => (vec![slow.clone(), qa.clone()], vec![st(&slow, 2), st(&qa, 1)]),
        "missing" => (vec![qb.clone()], vec![st(&qb, 4), SequenceStep::new("gone", 1)]),
        "only missing" => (vec![qb], vec![SequenceStep::new("gone", 1)]),
        "zero repeats" => (vec![qb.clone()], vec![st(&qb, 0)]),
        "empty" => (vec![qb], vec![]),
        "shared fx" => (vec![fa.clone(), fb.clone()], vec![st(&fa, 1), st(&fb, 2)]),
        "mixed fx" => (vec![fa.clone(), fd.clone()], vec![st(&fa, 1), st(&fd, 1)]),
        "bypassed fx" => (vec![fc.clone(), fc.clone()], vec![st(&fc, 1), st(&fc, 1)]),
        "one step" => (vec![qa.clone()], vec![st(&qa, 1)]),
        other => panic!("unknown case {other}"),
    }
}

#[test]
fn sequences_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "sequences") {
        let name = s(row, "name");
        let (clips, steps) = sequence_clips(name);
        let got = make_sequence_clip(
            &steps,
            |id| clips.iter().find(|c| c.id == id),
            RATE,
            "2026-10-29T10:00:00Z",
        );
        if row.get("id") == Some(&JsonValue::Null) {
            d.eq(format!("{name} nothing left"), got.is_none(), true);
            continue;
        }
        let Some(c) = got else {
            d.eq(format!("{name} made"), false, true);
            continue;
        };
        d.eq(format!("{name} id"), c.id.clone(), s(row, "id").to_string());
        d.eq(format!("{name} title"), c.title.clone(), s(row, "title").to_string());
        d.eq(format!("{name} beats"), c.beats, i(row, "beats"));
        d.eq(format!("{name} bpm"), bits(c.bpm), s(row, "bpm").to_string());
        d.eq(format!("{name} startBeat"), bits(c.start_beat), s(row, "startBeat").to_string());
        d.eq(format!("{name} source"), c.source.clone(), s(row, "source").to_string());
        d.eq(format!("{name} length"), c.buffer.length() as i64, i(row, "length"));
        d.eq(format!("{name} left"), floats_sha(&c.buffer.left), s(row, "lsha").to_string());
        d.eq(format!("{name} right"), floats_sha(c.buffer.right()), s(row, "rsha").to_string());
        d.eq(
            format!("{name} steps"),
            djrust::session::types::sequence_steps_json(c.steps.as_deref().unwrap_or(&[]))
                .canonical_js(),
            row.get("keptSteps").unwrap().canonical_js(),
        );
        d.eq(
            format!("{name} provenance"),
            without_recorded(c.provenance.as_ref().unwrap()).canonical_js(),
            s(row, "provenance").to_string(),
        );
        d.eq(format!("{name} fx"), c.fx.json().canonical_js(), s(row, "fx").to_string());
    }
    d.finish("sequences");
}

// MARK: the library

/// `LoopLibrary` on a folder, as sets of file names and the two documents' text.
#[derive(Default)]
struct Disk {
    main: BTreeSet<String>,
    removed: BTreeSet<String>,
    index: Option<String>,
    removed_index: Option<String>,
}

impl Disk {
    fn entries(text: &Option<String>, key: &str) -> Vec<JsonValue> {
        text.as_deref()
            .and_then(|t| JsonValue::parse(t).ok())
            .and_then(|d| d.get(key).and_then(JsonValue::as_array).cloned())
            .unwrap_or_default()
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = self.main.iter().map(|id| format!("{id}.wav")).collect();
        v.push("library.json".into());
        if self.removed_index.is_some() {
            v.push("removed".into());
        }
        v.sort();
        v
    }

    fn removed_files(&self) -> Vec<String> {
        let mut v: Vec<String> = self.removed.iter().map(|id| format!("{id}.wav")).collect();
        if self.removed_index.is_some() {
            v.push("removed.json".into());
        }
        v.sort();
        v
    }

    /// `LoopLibrary.save`.
    fn save(
        &mut self,
        clips: &[&LoopClip],
        scenes: Option<Vec<LoopScene>>,
        launch: Option<BTreeMap<String, ClipLaunch>>,
        stamp: &str,
    ) {
        let before = Self::entries(&self.index, "loops");
        let scenes = scenes.unwrap_or_else(|| {
            Self::entries(&self.index, "scenes")
                .iter()
                .filter_map(LoopScene::from_json)
                .collect()
        });
        let launch = launch.unwrap_or_else(|| library::load_launch(&before));
        for c in clips {
            if !self.main.contains(&c.id) {
                // moved back out of removed/, or written
                self.removed.remove(&c.id);
                self.main.insert(c.id.clone());
            }
        }
        let keep: BTreeSet<String> = clips.iter().map(|c| c.id.clone()).collect();
        let strays: Vec<String> = self.main.iter().filter(|id| !keep.contains(*id)).cloned().collect();
        let old = Self::entries(&self.removed_index, "removed");
        let (main, removed) = (self.main.clone(), self.removed.clone());
        let has_audio = move |id: &str| removed.contains(id) || main.contains(id);
        if let Some(list) =
            library::plan_move_off_the_row(&before, &keep, &old, stamp, &has_audio, strays.len())
        {
            self.removed_index = Some(library::removed_index_text(list));
            for id in strays {
                self.main.remove(&id);
                self.removed.insert(id);
            }
        }
        let owned: Vec<LoopClip> = clips.iter().map(|c| (*c).clone()).collect();
        self.index = Some(library::index_text(&owned, &launch, &scenes));
    }
}

#[test]
fn the_librarys_files_match_swift() {
    let doc = golden();
    let lib = doc.get("library").unwrap();
    let a = lc_clip(1, 1, 120.0, Some(crop_prov()), None, LoopFx::default(), None, None);
    let b = lc_clip(2, 2, 120.0, None, Some("teal"), tube(), None, None);
    let c = plain(3);
    let seq_buf = ramp(5, 192_000, RATE);
    let seq_wave = djrust::deck::waveform::WaveformOverview::new(&seq_buf, 40);
    let mut seq = LoopClip::new(
        &sequence_id(&seq_buf),
        "seq x×2→y",
        "A",
        8,
        120.0,
        0.0,
        Arc::new(seq_buf),
        seq_wave,
    );
    seq.steps = Some(vec![SequenceStep::new("x", 2), SequenceStep::new("y", 1)]);
    let scene_fx = LoopFx {
        drive: 12.0,
        mode: LoopFxMode::Soft,
        ..LoopFx::default()
    };
    let scenes = vec![LoopScene {
        name: "Scene 1".into(),
        clips: vec![
            SceneClip {
                clip_id: a.id.clone(),
                gain: 0.5,
                fx: None,
            },
            SceneClip {
                clip_id: b.id.clone(),
                gain: 1.0,
                fx: Some(scene_fx),
            },
        ],
    }];
    let launch = BTreeMap::from([
        (
            a.id.clone(),
            ClipLaunch {
                quantum: Some(8),
                fade: Some(LoopFade::Beat),
                follow: None,
            },
        ),
        (
            c.id.clone(),
            ClipLaunch {
                quantum: None,
                fade: None,
                follow: Some(LoopFollow::Source),
            },
        ),
    ]);
    let mut d = Diffs::default();
    // the audio files
    for (clip, row) in [&a, &b, &c, &seq].iter().zip(rows(lib, "clips")) {
        let wav = clip.wav_data();
        d.eq(format!("{} id", clip.title), clip.id.clone(), s(row, "id").to_string());
        d.eq(format!("{} wav bytes", clip.title), wav.len() as i64, i(row, "wavBytes"));
        d.eq(format!("{} wav sha", clip.title), bytes_sha(&wav), s(row, "wavSha").to_string());
        d.eq(format!("{} file name", clip.title), clip.file_name(), s(row, "fileName").to_string());
    }
    d.eq(
        "scenes json",
        JsonValue::Array(scenes.iter().map(LoopScene::json).collect()).canonical_js(),
        lib.get("scenes").unwrap().canonical_js(),
    );
    // the four saves, and the two documents after each
    let mut disk = Disk::default();
    let steps = rows(lib, "steps");
    type Save<'a> = (
        &'a str,
        Vec<&'a LoopClip>,
        Option<Vec<LoopScene>>,
        Option<BTreeMap<String, ClipLaunch>>,
    );
    let plan: [Save; 4] = [
        ("save a b c seq", vec![&a, &b, &c, &seq], Some(scenes.clone()), Some(launch.clone())),
        ("save a c", vec![&a, &c], None, None),
        ("save c", vec![&c], None, None),
        ("save c b", vec![&c, &b], None, None),
    ];
    for ((name, clips, sc, la), want) in plan.into_iter().zip(steps) {
        d.eq(format!("{name} step"), name, s(want, "step"));
        disk.save(&clips, sc, la, s(want, "stamp"));
        d.eq(
            format!("{name}: library.json"),
            disk.index.clone().unwrap_or_default(),
            s(want, "library").to_string(),
        );
        d.eq(
            format!("{name}: removed.json"),
            disk.removed_index.clone(),
            want.get("removed").and_then(JsonValue::as_str).map(String::from),
        );
        d.eq(
            format!("{name}: files"),
            disk.files(),
            want.get("files").unwrap().strings().unwrap(),
        );
        d.eq(
            format!("{name}: removed files"),
            disk.removed_files(),
            want.get("removedFiles").unwrap().strings().unwrap(),
        );
    }
    // what the library reads back
    let removed: Vec<library::RemovedLoop> = Disk::entries(&disk.removed_index, "removed")
        .iter()
        .filter_map(library::RemovedLoop::from_entry)
        .collect();
    let want_removed = rows(lib, "removedNow");
    d.eq("removed count", removed.len(), want_removed.len());
    for (r, w) in removed.iter().zip(want_removed) {
        d.eq(format!("removed {} id", r.id), r.id.clone(), s(w, "id").to_string());
        d.eq(format!("removed {} title", r.id), r.title.clone(), s(w, "title").to_string());
        d.eq(format!("removed {} source", r.id), r.source.clone(), s(w, "source").to_string());
        d.eq(format!("removed {} beats", r.id), r.beats, i(w, "beats"));
        d.eq(format!("removed {} bpm", r.id), bits(r.bpm), s(w, "bpm").to_string());
        d.eq(format!("removed {} color", r.id), r.color.clone(), opt_str(w, "color"));
        d.eq(format!("removed {} index", r.id), r.index, i(w, "index"));
        d.eq(format!("removed {} when", r.id), r.removed.clone(), s(w, "removed").to_string());
        d.eq(
            format!("removed {} is sequence", r.id),
            r.is_sequence,
            w.get("isSequence").and_then(JsonValue::as_bool).unwrap(),
        );
    }
    let entries = Disk::entries(&disk.index, "loops");
    let loaded: Vec<LoopClip> = entries
        .iter()
        .filter_map(|e| library::clip_from_entry(e, |_| Some(ramp(1, 100, RATE))))
        .collect();
    let want_loaded = rows(lib, "loadedNow");
    d.eq("loaded count", loaded.len(), want_loaded.len());
    for (c, w) in loaded.iter().zip(want_loaded) {
        let id = c.id.clone();
        d.eq(format!("loaded {id} id"), id.clone(), s(w, "id").to_string());
        d.eq(format!("loaded {id} title"), c.title.clone(), s(w, "title").to_string());
        d.eq(format!("loaded {id} source"), c.source.clone(), s(w, "source").to_string());
        d.eq(format!("loaded {id} beats"), c.beats, i(w, "beats"));
        d.eq(format!("loaded {id} bpm"), bits(c.bpm), s(w, "bpm").to_string());
        d.eq(format!("loaded {id} startBeat"), bits(c.start_beat), s(w, "startBeat").to_string());
        d.eq(format!("loaded {id} color"), c.color.clone(), opt_str(w, "color"));
        d.eq(format!("loaded {id} fx"), c.fx.json().canonical_js(), s(w, "fx").to_string());
        d.eq(
            format!("loaded {id} steps"),
            c.steps
                .as_ref()
                .map(|v| djrust::session::types::sequence_steps_json(v).canonical_js()),
            w.get("steps").filter(|v| **v != JsonValue::Null).map(JsonValue::canonical_js),
        );
        d.eq(
            format!("loaded {id} provenance"),
            c.provenance.as_ref().map(JsonValue::canonical_js).unwrap_or("null".into()),
            s(w, "provenance").to_string(),
        );
    }
    let launch_back = library::load_launch(&entries);
    let want_launch = rows(lib, "launchNow");
    d.eq("launch count", launch_back.len(), want_launch.len());
    for w in want_launch {
        let l = launch_back.get(s(w, "id")).cloned().unwrap_or_default();
        d.eq(format!("launch {} quantum", s(w, "id")), l.quantum, w.get("quantum").and_then(JsonValue::as_i64));
        d.eq(format!("launch {} fade", s(w, "id")), l.fade.map(|f| f.raw().to_string()), opt_str(w, "fade"));
        d.eq(
            format!("launch {} follow", s(w, "id")),
            l.follow.map(|f| f.raw().to_string()),
            opt_str(w, "follow"),
        );
    }
    let scenes_back: Vec<JsonValue> = Disk::entries(&disk.index, "scenes")
        .iter()
        .filter_map(LoopScene::from_json)
        .map(|s| s.json())
        .collect();
    d.eq(
        "scenes back",
        JsonValue::Array(scenes_back).canonical_js(),
        JsonValue::Array(rows(lib, "scenesNow").to_vec()).canonical_js(),
    );
    d.finish("library");
}

// MARK: provenance

/// `ClipRetune.voices(for:)`.
fn voices_for(clip: &LoopClip, catalog: &VoiceCatalog) -> Vec<RoleVoice> {
    if take_provenance(clip.provenance.as_ref()).is_err() || !clip.origin_macros().is_empty() {
        return Vec::new();
    }
    let Some(source) = clip.origin_source() else {
        return Vec::new();
    };
    let (project, genre) = match &source {
        DeckSource::Net(spec) => (spec.project(), spec.genre.clone()),
        DeckSource::RawNets(payload) => {
            let Ok(p) = djrust::share::Share::from_payload(payload, None).project() else {
                return Vec::new();
            };
            (
                p,
                payload
                    .get("genre")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        }
        DeckSource::Sample { .. } | DeckSource::None => return Vec::new(),
    };
    RoleVoice::list_for_retune(&project, &genre, &clip.origin_roles_off(), catalog)
}

fn voices_json(v: &[RoleVoice]) -> String {
    JsonValue::Array(
        v.iter()
            .map(|r| {
                JsonValue::Object(JsonObject::from_pairs([
                    ("role", JsonValue::str(r.role.as_str())),
                    ("channel", JsonValue::int(r.channel)),
                    ("instrument", JsonValue::str(r.instrument.as_str())),
                    (
                        "options",
                        JsonValue::Array(r.options.iter().map(|o| JsonValue::str(o.as_str())).collect()),
                    ),
                ]))
            })
            .collect(),
    )
    .canonical_js()
}

#[test]
fn provenance_accessors_match_swift() {
    let doc = golden();
    let catalog = VoiceCatalog::bundled();
    let mut d = Diffs::default();
    for row in rows(&doc, "provenance") {
        let name = s(row, "name");
        let prov = opt_str(row, "provenance").map(|t| JsonValue::parse(&t).unwrap());
        let mut c = plain(1);
        c.provenance = prov;
        d.eq(
            format!("{name} take"),
            c.take_provenance().map(JsonValue::canonical_js),
            opt_str(row, "take"),
        );
        d.eq(
            format!("{name} origin source"),
            c.origin_source().map(|s| s.json().canonical_js()),
            opt_str(row, "originSource"),
        );
        let voices: Vec<(i64, String)> = c.retuned_voices().into_iter().collect();
        let want: Vec<(i64, String)> = rows(row, "retunedVoices")
            .iter()
            .map(|v| (i(v, "channel"), s(v, "instrument").to_string()))
            .collect();
        d.eq(format!("{name} retuned voices"), voices, want);
        d.eq(
            format!("{name} section"),
            c.origin_section(),
            row.get("section").and_then(JsonValue::as_i64),
        );
        d.eq(format!("{name} genre"), c.origin_genre(), opt_str(row, "genre"));
        d.eq(format!("{name} title"), c.origin_title(), opt_str(row, "title"));
        d.eq(format!("{name} bpm"), opt_bits(c.origin_bpm()), opt_str(row, "bpm"));
        d.eq(
            format!("{name} roles off"),
            c.origin_roles_off(),
            row.get("rolesOff").unwrap().strings().unwrap(),
        );
        d.eq(
            format!("{name} macros"),
            c.origin_macros(),
            row.get("macros").unwrap().strings().unwrap(),
        );
        let refusal = match take_provenance(c.provenance.as_ref()) {
            Ok(_) => "ok",
            Err(Refusal::NotANet) => "notANet",
            Err(Refusal::Sequence) => "sequence",
            Err(Refusal::Macros(_)) => "macros",
            Err(Refusal::NoProvenance) => "noProvenance",
            Err(Refusal::SongEnded) => "songEnded",
            Err(Refusal::TooFar) => "tooFar",
        };
        d.eq(format!("{name} retune take"), refusal, s(row, "retuneTake"));
        d.eq(
            format!("{name} voices for a retune"),
            voices_json(&voices_for(&c, &catalog)),
            row.get("voicesFor").unwrap().canonical_js(),
        );
    }
    d.finish("provenance accessors");
}

#[test]
fn provenance_builders_match_swift() {
    let doc = golden();
    let b = doc.get("builders").unwrap();
    let build = s(b, "build");
    let rec = "2026-10-29T10:00:00Z";
    let net1 = r#"{"deck":"B","title":"techno 7","mode":"net","startBeat":4,"source":{"kind":"net","share":{"genre":"techno","seed":7}},"section":{"index":2,"name":"drop"},"tempo":{"pitch":0.25,"baseBPM":120,"bpm":122.4},"rolesOff":["melody","arp"],"macros":[]}"#;
    let parent = lc_clip(1, 1, 120.0, Some(JsonValue::parse(net1).unwrap()), None, LoopFx::default(), None, None);
    let bare = plain(2);
    let mut d = Diffs::default();
    d.eq("crop", provenance::crop(&parent, 1, 3).canonical_js(), s(b, "crop").to_string());
    d.eq("crop of a bare clip", provenance::crop(&bare, 0, 2).canonical_js(), s(b, "cropBare").to_string());
    let voices = BTreeMap::from([(3, "saw".to_string()), (1, "piano".to_string())]);
    d.eq(
        "retune",
        without_recorded(&provenance::retune(&parent, &voices, build, rec)).canonical_js(),
        s(b, "retune").to_string(),
    );
    d.eq(
        "retune of a bare clip",
        without_recorded(&provenance::retune(&bare, &BTreeMap::new(), build, rec)).canonical_js(),
        s(b, "retuneBare").to_string(),
    );
    d.eq(
        "sequence",
        without_recorded(&provenance::sequence(
            &[(SequenceStep::new(&parent.id, 2), &parent), (SequenceStep::new(&bare.id, 1), &bare)],
            rec,
        ))
        .canonical_js(),
        s(b, "sequence").to_string(),
    );
    let stock = djrust::layers::StockLoop {
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
    let mut drum = stock.clone();
    drum.key = None;
    drum.seconds = 3.9994;
    d.eq(
        "stock",
        without_recorded(&provenance::stock(&stock, "id1", build, rec)).canonical_js(),
        s(b, "stock").to_string(),
    );
    d.eq(
        "stock drum loop (tempo snapped to a whole bpm)",
        without_recorded(&provenance::stock(&drum, "id2", build, rec)).canonical_js(),
        s(b, "stockDrum").to_string(),
    );
    let capture_like = JsonObject::from_pairs([
        ("deck", JsonValue::str("A")),
        ("title", JsonValue::str("ramp")),
        ("mode", JsonValue::str("sample")),
        (
            "source",
            DeckSource::Sample {
                id: String::new(),
                title: "ramp".into(),
                grid: None,
            }
            .json(),
        ),
    ]);
    let sample_buf = ramp(9, 4800, RATE);
    d.eq("sample content id", sample_buf.content_id(), s(b, "finishSampleId").to_string());
    d.eq(
        "finish with the sample",
        provenance::finish(capture_like.clone(), 2, 119.5, Some(&sample_buf.content_id())).canonical_js(),
        s(b, "finishSample").to_string(),
    );
    d.eq(
        "finish without",
        provenance::finish(capture_like, 1, 120.0, None).canonical_js(),
        s(b, "finishNoBuffer").to_string(),
    );
    d.finish("provenance builders");
}

// MARK: voices and the source edits

#[test]
fn role_voices_and_source_edits_match_swift() {
    let doc = golden();
    let catalog = VoiceCatalog::bundled();
    let mut d = Diffs::default();
    for row in rows(&doc, "voices") {
        let structure = opt_str(row, "structure");
        let spec = TrackSpec::new(s(row, "genre"), f(row, "seed"), structure.as_deref());
        let tag = format!("{} {}", s(row, "genre"), f(row, "seed"));
        let project = spec.project();
        let roles: Vec<String> = DECK_ROLE_ORDER.iter().map(|r| r.to_string()).collect();
        let voices = RoleVoice::list(&project, &roles, spec.genre.as_str(), &catalog);
        d.eq(
            format!("{tag} RoleVoice.list"),
            voices_json(&voices),
            row.get("voices").unwrap().canonical_js(),
        );
        let source = DeckSource::Net(spec.clone());
        let canon = |s: Option<DeckSource>| s.map(|s| s.json().canonical_js());
        let drums = project_drum_channels(&project);
        if row.get("instrument").is_some() {
            let channel = i(row, "channel");
            let first = source.with_instrument("saw", channel);
            d.eq(format!("{tag} withInstrument"), canon(first.clone()), opt_str(row, "instrument"));
            d.eq(
                format!("{tag} withInstrument twice"),
                canon(first.and_then(|f| f.with_instrument("square", channel + 1))),
                opt_str(row, "twice"),
            );
            d.eq(format!("{tag} first non-drum voice"), drums.contains(&channel), false);
        }
        d.eq(format!("{tag} drum channels"), drums.clone(), row.get("drums").unwrap().ints().unwrap());
        d.eq(format!("{tag} withKit"), canon(source.with_kit(Some("808"), &drums)), opt_str(row, "kit"));
        d.eq(
            format!("{tag} withKit nil"),
            canon(
                source
                    .with_kit(Some("808"), &drums)
                    .and_then(|s| s.with_kit(None, &drums)),
            ),
            opt_str(row, "kitCleared"),
        );
        d.eq(format!("{tag} withKit empty"), canon(source.with_kit(Some(""), &drums)), opt_str(row, "kitEmpty"));
        d.eq(
            format!("{tag} withPalette"),
            canon(source.with_palette(Some("Classic"), &[1, 2])),
            opt_str(row, "palette"),
        );
    }
    // a raw-nets source: the payload's `tracks`
    let raw = doc.get("rawEdits").unwrap();
    let envelope = doc.get("rollEnvelope").unwrap().clone();
    let source = DeckSource::RawNets(envelope.clone());
    let canon = |s: Option<DeckSource>| s.map(|s| s.json().canonical_js());
    d.eq(
        "roll envelope (the Rust roll port writes the same payload)",
        djrust::roll::envelope(
            &JsonValue::parse(
                r#"{"name":"L","tempo":128,"steps":128,"key":"A minor","parts":{"kick":{"every":16,"until":128,"notes":[[0],[4],[8],[12]]},"bass":{"every":16,"until":128,"notes":[[2,45,2],[6,45,2],[10,45,2],[14,45,2]]},"melody":{"every":16,"until":128,"notes":[[0,69,2],[4,72,2]]}}}"#,
            )
            .unwrap(),
        )
        .unwrap()
        .canonical_js(),
        envelope.canonical_js(),
    );
    d.eq("raw source", source.json().canonical_js(), s(raw, "source").to_string());
    d.eq("raw withInstrument", canon(source.with_instrument("acid", 2)), opt_str(raw, "instrument"));
    d.eq(
        "raw withInstrument twice",
        canon(source.with_instrument("acid", 2).and_then(|s| s.with_instrument("pad", 5))),
        opt_str(raw, "twice"),
    );
    d.eq("raw withKit", canon(source.with_kit(Some("808"), &[10])), opt_str(raw, "kit"));
    d.eq(
        "raw withKit nil",
        canon(source.with_kit(Some("808"), &[10]).and_then(|s| s.with_kit(None, &[10]))),
        opt_str(raw, "kitCleared"),
    );
    let not_nets = doc.get("notNets").unwrap();
    d.eq(
        "none is no net",
        DeckSource::None.with_instrument("saw", 1).is_none(),
        not_nets.get("none").and_then(JsonValue::as_bool).unwrap(),
    );
    d.eq(
        "a sample is no net",
        DeckSource::Sample { id: "x".into(), title: "t".into(), grid: None }
            .with_instrument("saw", 1)
            .is_none(),
        not_nets.get("sample").and_then(JsonValue::as_bool).unwrap(),
    );
    d.finish("role voices and source edits");
}

// MARK: names and tempos

#[test]
fn file_names_labels_and_import_tempos_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for row in rows(&doc, "fileNames") {
        let mut c = plain(1);
        c.title = s(row, "title").to_string();
        d.eq(format!("fileName({:?})", c.title), c.file_name(), s(row, "fileName").to_string());
    }
    for row in rows(&doc, "lengthLabels") {
        let mut c = plain(1);
        c.beats = i(row, "beats");
        d.eq(format!("{} beats label", c.beats), c.length_label(), s(row, "label").to_string());
        d.eq(format!("{} beats bars", c.beats), c.bars(), i(row, "bars"));
    }
    for row in rows(&doc, "importTempos") {
        let b = ramp(1, i(row, "length") as usize, RATE);
        d.eq(
            format!("import tempo {} frames {} beats", b.length(), i(row, "beats")),
            bits(import_tempo(&b, i(row, "beats"))),
            s(row, "bpm").to_string(),
        );
    }
    d.finish("file names, labels and import tempos");
}
