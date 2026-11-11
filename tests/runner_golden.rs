//! Swift goldens for `WaveRunner`'s dj extensions (`tools/paritydump`, section "runner",
//! `tools/paritydump/runner.swift`): scratching the net clock, the pooled live-keys voice, the
//! room / shimmer / echo sends with the FX unit's gains on them, and the part levels a sampled
//! instrument is matched to.
//!
//! Every audio case in `swift/runner.json` is a project, a set of options and a script of
//! gestures given as data; this test runs the very same data through the Rust runner and
//! compares what came out with what Swift's runner did: the tick, the scrub flag and the tempo
//! after every block, the per-block peak and RMS, a decimated waveform, the hits reported to
//! `onHit`, and a sha256 of the float32 render.
//!
//! The renders are held to Swift's by [`TOLERANCE`] (see there): the DSP is the same operation
//! for operation, but Apple's libm and the `libm` crate differ in the last bit of
//! `sin`/`exp`/`pow`/`tanh`. Everything that is a decision (ticks, hits, the flags) is exact.
//!
//! Part 2 of the runner port adds its features' cases to the same file: put a case in the
//! Swift section, and it is compared here with no change to this test. Part 3's (the voiced
//! drums, the section plan, the rests and the measured gain, and the decisions on a sampled kit)
//! are in `swift/runnerfixes.json`, run by the same code.

mod common;
use common::*;
use djrust::cid;
use djrust::compose;
use djrust::json::{JsonObject, JsonValue};
use djrust::musicality::apply as apply_pass;
use djrust::musicality::echo::EchoSpec;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::space::SpaceSpec;
use djrust::musicality::synth_voice::VoiceDesign;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::part_levels::{own_part_level_db, own_synth_part_db, SamplerPartLevels};
use djrust::wave::*;
use djrust::TrackSpec;
use std::collections::BTreeMap;

/// How far a render may be from Swift's, relative to the case's own peak. Measured on this
/// matrix with the 1e-4 first guess loosened to what the worst case needed: see
/// `renders_match_swift`'s report line. (Exact would be a hash match.)
const TOLERANCE: f64 = 5e-6;

fn golden() -> JsonValue {
    fixture("runner")
}

/// `runnerfixes.json`: the third part of the runner port (the drums, the section plan, the rests
/// and the measured gain, the sampled kits' decisions).
fn golden_fixes() -> JsonValue {
    fixture("runnerfixes")
}

fn rate(doc: &JsonValue) -> f64 {
    f(doc, "rate")
}

// ---- running a case ---------------------------------------------------------------------

fn ring(group: &str, channel: i64, notes: &[(i64, i64)], ms: i64) -> NetBundle {
    let mut n = NetBundle::ring(64);
    for (step, note) in notes {
        n.bind(*step, *note, channel, 100, ms);
    }
    n.track.channel = channel;
    n.track.group = Some(group.into());
    n
}

fn flag(v: &JsonValue, key: &str) -> bool {
    v.get(key).and_then(JsonValue::as_bool).unwrap_or(false)
}

/// `runnerProject` in the dump.
fn build_project(spec: &JsonValue) -> Project {
    let mut p;
    match s(spec, "kind") {
        "spec" => {
            let mut ov = JsonObject::new();
            ov.set("seed", f(spec, "seed").into());
            if let Some(st) = spec.get("structure").and_then(JsonValue::as_str) {
                ov.set("structure", st.into());
            }
            p = compose(s(spec, "genre"), &ov.into());
            if let Some(x) = spec.get("swing").and_then(JsonValue::as_f64) {
                p.swing = x;
            }
            if let Some(x) = spec.get("humanize").and_then(JsonValue::as_f64) {
                p.humanize = x;
            }
        }
        "pad" => {
            p = Project::default();
            p.tempo = 120.0;
            let low = flag(spec, "low");
            let notes: &[(i64, i64)] = if low {
                &[(0, 36)]
            } else {
                &[(0, 60), (1, 64), (2, 67)]
            };
            p.nets.insert(
                "harmony",
                ring("harmony", 7, notes, if low { 400 } else { 100 }),
            );
        }
        "kickbass" => {
            p = Project::default();
            p.tempo = 120.0;
            p.nets.insert(
                "kick",
                ring("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
            );
            p.nets.insert(
                "bass",
                ring("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
            );
        }
        "note" => {
            // SynthVoiceTests.oneNote: one note on a 64-step ring, 120 BPM
            p = Project::default();
            p.tempo = 120.0;
            let group = s(spec, "group");
            let channel = spec
                .get("channel")
                .and_then(JsonValue::as_f64)
                .unwrap_or(6.0) as i64;
            let num = |k: &str, d: f64| spec.get(k).and_then(JsonValue::as_f64).unwrap_or(d);
            let mut n = NetBundle::ring(64);
            n.bind(
                0,
                f(spec, "note") as i64,
                channel,
                num("velocity", 100.0) as i64,
                0,
            );
            n.bindings.get_mut("t0").unwrap().duration_steps = num("steps", 8.0) as i64;
            n.track.channel = channel;
            n.track.group = Some(group.into());
            n.track.instrument = spec
                .get("instrument")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .into();
            p.nets.insert(group, n);
            if let Some(root) = spec.get("rootNote").and_then(JsonValue::as_f64) {
                p.root_note = Some(root as i64);
            }
        }
        "riser" => {
            // RiserTests' project: four bars of buildup into four of drop, kick on the beats
            // and an offbeat bass (16-step rings); the riser window is steps 32..64
            p = Project::default();
            p.tempo = 120.0;
            let ring16 = |group: &str, channel: i64, notes: &[(i64, i64)], ms: i64| {
                let mut n = NetBundle::ring(16);
                for (step, note) in notes {
                    n.bind(*step, *note, channel, 100, ms);
                }
                n.track.channel = channel;
                n.track.group = Some(group.into());
                n
            };
            p.nets.insert(
                "kick",
                ring16("drums", 10, &[(0, 36), (4, 36), (8, 36), (12, 36)], 50),
            );
            p.nets.insert(
                "bass",
                ring16("bass", 6, &[(2, 33), (6, 33), (10, 33), (14, 33)], 100),
            );
            for (name, steps) in [("buildup", 64), ("drop", 64)] {
                p.structure.push(djrust::pflow::Section {
                    name: name.into(),
                    steps,
                    phrases: Default::default(),
                });
            }
        }
        "kitpad" => {
            // StereoTests' project: a four-on-the-floor kick, off-beat hats and a held pad chord
            p = Project::default();
            p.tempo = 120.0;
            let mut k = NetBundle::ring(16);
            for step in [0, 4, 8, 12] {
                k.bind(step, 36, 10, 100, 50);
            }
            k.track.channel = 10;
            k.track.group = Some("drums".into());
            let mut hats = NetBundle::ring(16);
            for step in [2, 6, 10, 14] {
                hats.bind(step, 42, 10, 90, 50);
            }
            hats.track.channel = 10;
            hats.track.group = Some("drums".into());
            let mut c = NetBundle::ring(16);
            for (v, note) in [60, 64, 67].into_iter().enumerate() {
                c.bind(v as i64, note, 7, 90, 1950);
            }
            c.track.channel = 7;
            c.track.group = Some("harmony".into());
            p.nets.insert("kick", k);
            p.nets.insert("hihat", hats);
            p.nets.insert("harmony", c);
        }
        "kickpad" => {
            p = Project::default();
            p.tempo = 120.0;
            let mut k = NetBundle::ring(16);
            for step in [0, 4, 8, 12] {
                k.bind(step, 36, 10, 100, 50);
            }
            k.track.channel = 10;
            k.track.group = Some("drums".into());
            let mut h = NetBundle::ring(16);
            for (v, note) in [60, 64, 67].into_iter().enumerate() {
                h.bind(v as i64, note, 7, 90, 1950);
            }
            h.track.channel = 7;
            h.track.group = Some("harmony".into());
            p.nets.insert("kick", k);
            p.nets.insert("harmony", h);
        }
        "lead" => {
            p = Project::default();
            p.tempo = spec
                .get("tempo")
                .and_then(JsonValue::as_f64)
                .unwrap_or(120.0);
            let group = spec
                .get("group")
                .and_then(JsonValue::as_str)
                .unwrap_or("melody");
            let mut n = NetBundle::ring(64);
            n.bind(0, 72, 4, 110, 0);
            n.bindings.get_mut("t0").unwrap().duration_steps = 1;
            n.track.channel = 4;
            n.track.group = Some(group.into());
            p.nets.insert(group, n);
        }
        "played" => {
            // a composed track after dj's pass with the fixes named in `apply`: what a deck plays
            let track = TrackSpec::new(
                s(spec, "genre"),
                f(spec, "seed"),
                spec.get("structure").and_then(JsonValue::as_str),
            );
            p = track.project();
            if let Some(names) = spec.get("apply").and_then(JsonValue::as_array) {
                p = apply_pass(&p, &track, fixes_of(names));
            }
            if let Some(x) = spec.get("swing").and_then(JsonValue::as_f64) {
                p.swing = x;
            }
            if let Some(x) = spec.get("humanize").and_then(JsonValue::as_f64) {
                p.humanize = x;
            }
        }
        "hats" => {
            // DropGapTests.closedHatChokesTheOpenHat (or, `busy`, DropGapRealTimeTests)
            p = Project::default();
            p.tempo = 120.0;
            let mut hats = NetBundle::ring(16);
            if flag(spec, "busy") {
                for step in [0, 4, 8, 12] {
                    hats.bind(step, 42, 10, 90, 50);
                }
                for step in [2, 6, 10, 14] {
                    hats.bind(step, 46, 10, 100, 50);
                }
            } else {
                hats.bind(2, 46, 10, 110, 50);
                hats.bind(3, 42, 10, 1, 50);
            }
            hats.track.channel = 10;
            hats.track.group = Some("drums".into());
            p.nets.insert("hihat", hats);
        }
        "snare" => {
            // BrushTests.snareHit
            p = Project::default();
            p.tempo = 120.0;
            let mut n = NetBundle::ring(16);
            n.bind(0, 38, 11, 100, 50);
            n.track.channel = 11;
            p.nets.insert("snare", n);
        }
        other => panic!("unknown project kind {other}"),
    }
    p
}

/// `runnerFixes` in the dump: the fixes a list of names makes.
fn fixes_of(names: &[JsonValue]) -> Fixes {
    let mut fixes = Fixes::EMPTY;
    for name in names {
        let name = name.as_str().unwrap();
        fixes |= Fixes::named(name).unwrap_or_else(|| panic!("fix {name}"));
    }
    fixes
}

fn build_options(case: &JsonValue) -> WaveOptions {
    let mut o = WaveOptions {
        master: match case.get("master").and_then(JsonValue::as_str) {
            Some("soft") => Master::Soft,
            Some("limit") => Master::Limit,
            _ => Master::Linear,
        },
        record_hits: flag(case, "recordHits"),
        ..Default::default()
    };
    if flag(case, "instruments") {
        o.instruments = Some(InstrumentTable::shared());
    }
    for name in case
        .get("fixes")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let name = name.as_str().unwrap();
        if name == "groove" {
            // (`grooveOn` is `opts.musicality.contains(.groove)` in Swift)
            o.groove = true;
        } else {
            o.musicality |= Fixes::named(name).unwrap_or_else(|| panic!("fix {name}"));
        }
    }
    // the settings a track of this genre plays with, for the fixes it holds; explicit fields
    // below override them
    if let Some(g) = case.get("forGenre").and_then(JsonValue::as_str) {
        o = o.for_genre(g);
    }
    if let Some(d) = case.get("duckDepth").and_then(JsonValue::as_f64) {
        o.duck_depth = d;
    }
    if flag(case, "subOctave") {
        o.sub_octave = true;
    }
    if let Some(g) = case.get("trackGain").and_then(JsonValue::as_f64) {
        o.track_gain = Some(g);
    }
    if let Some(JsonValue::Object(t)) = case.get("trackBalance") {
        let mut trims = BTreeMap::new();
        for (k, v) in t.iter() {
            trims.insert(k.to_string(), v.as_f64().unwrap());
        }
        o.track_balance = Some(trims);
    }
    if let Some(v) = case.get("space") {
        o.space = Some(match v.get("genre").and_then(JsonValue::as_str) {
            Some(g) => SpaceSpec::for_genre(g, flag(v, "shimmer")),
            None => {
                let mut sp = SpaceSpec::new(f(v, "seconds"), f(v, "wet"), f(v, "damping"));
                sp.shimmer = v.get("shimmer").and_then(JsonValue::as_f64).unwrap_or(0.0);
                sp
            }
        });
    }
    if let Some(v) = case.get("echo") {
        o.echo = match v.get("genre").and_then(JsonValue::as_str) {
            Some(g) => EchoSpec::for_genre(g),
            None => Some(EchoSpec {
                steps: f(v, "steps"),
                feedback: f(v, "feedback"),
                wet: f(v, "wet"),
                damping: f(v, "damping"),
            }),
        };
    }
    o
}

struct Rendered {
    out: Vec<f32>,
    /// `"channels": 2`: the right channel, and its per-block peak and RMS.
    out_r: Vec<f32>,
    peaks_r: Vec<f64>,
    rms_r: Vec<f64>,
    peaks: Vec<f64>,
    rms: Vec<f64>,
    ticks: Vec<i64>,
    scrubbing: Vec<bool>,
    tempos: Vec<f64>,
    hits: Vec<HitRecord>,
    /// The parts' trims the project was prepared with (`.balance`).
    balance: Option<BTreeMap<String, f64>>,
    /// The gain it was prepared with (`.loudness`), and its rest schedule as [from, to, mask].
    gain: f64,
    rests: Vec<[i64; 3]>,
}

fn apply(op: &[JsonValue], r: &mut WaveRunner, opts: &WaveOptions, sr: f64) {
    let num = |k: usize| op[k].as_f64().unwrap();
    match op[0].as_str().unwrap() {
        "play" => r.play(),
        "pause" => r.pause(),
        "stop" => r.stop(),
        "silence" => r.silence(),
        "tempo" => r.set_tempo(num(1)),
        "freeze" => r.freeze(),
        "scrub" => r.scrub(num(1)),
        "release" => r.release(),
        "chase" => r.set_chase(num(1)),
        "noteOn" => r.note_on(num(1) as i64, num(2) as i64),
        "noteOff" => r.note_off(num(1) as i64),
        "allOff" => r.all_notes_off(),
        "unit" => r.set_unit_sends(num(1), num(2), num(3), num(4) as usize),
        "muteAll" => {
            let ids: Vec<String> = r
                .graph()
                .unwrap()
                .nets
                .iter()
                .map(|n| n.id.clone())
                .collect();
            for id in ids {
                r.set_mute(&id, true);
            }
        }
        "load" => r.load_prepared(WaveRunner::prepare(&build_project(&op[1]), sr, opts, 0)),
        other => panic!("unknown op {other}"),
    }
}

fn run_case(case: &JsonValue, sr: f64) -> Rendered {
    let block = case
        .get("block")
        .and_then(JsonValue::as_f64)
        .unwrap_or(256.0) as usize;
    let blocks = f(case, "blocks") as usize;
    let opts = build_options(case);
    let mut r = WaveRunner::new(sr, opts.clone());
    let start_tick = case
        .get("startTick")
        .and_then(JsonValue::as_f64)
        .unwrap_or(0.0) as i64;
    let prepared = WaveRunner::prepare(
        &build_project(case.get("project").unwrap()),
        sr,
        &opts,
        start_tick,
    );
    let balance = prepared.balance.clone();
    let gain = prepared.gain;
    let rests = prepared
        .song_rests
        .entries
        .iter()
        .map(|e| [e.from, e.to, e.mask.0 as i64])
        .collect();
    r.load_prepared(prepared);
    let script = case.get("script").and_then(JsonValue::as_array).unwrap();
    let stereo = case.get("channels").and_then(JsonValue::as_f64) == Some(2.0);
    let mut res = Rendered {
        out: Vec::with_capacity(blocks * block),
        out_r: Vec::new(),
        peaks_r: Vec::new(),
        rms_r: Vec::new(),
        peaks: Vec::new(),
        rms: Vec::new(),
        ticks: Vec::new(),
        scrubbing: Vec::new(),
        tempos: Vec::new(),
        hits: Vec::new(),
        balance: None,
        gain,
        rests,
    };
    let mut buf = vec![0f32; block];
    let mut buf_r = vec![0f32; block];
    for b in 0..blocks {
        for step in script {
            let row = step.as_array().unwrap();
            if row[0].as_f64().unwrap() as usize == b {
                apply(&row[1..], &mut r, &opts, sr);
            }
        }
        r.process(&mut buf, stereo.then_some(&mut buf_r[..]), block);
        let mut pk = 0f32;
        let mut sum = 0.0f64;
        for x in &buf {
            pk = pk.max(x.abs());
            sum += *x as f64 * *x as f64;
        }
        if stereo {
            let (mut pk_r, mut sum_r) = (0f32, 0.0f64);
            for x in &buf_r {
                pk_r = pk_r.max(x.abs());
                sum_r += *x as f64 * *x as f64;
            }
            res.out_r.extend_from_slice(&buf_r);
            res.peaks_r.push(pk_r as f64);
            res.rms_r.push((sum_r / block as f64).sqrt());
        }
        res.out.extend_from_slice(&buf);
        res.peaks.push(pk as f64);
        res.rms.push((sum / block as f64).sqrt());
        res.ticks.push(r.graph().map_or(-1, |g| g.tick));
        res.scrubbing.push(r.is_scrubbing());
        res.tempos.push(r.tempo());
    }
    res.hits = std::mem::take(&mut r.hits);
    res.balance = balance;
    res
}

fn floats_hash(x: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for v in x {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    cid::hex(&cid::sha256(&bytes))
}

fn numbers(v: &JsonValue, key: &str) -> Vec<f64> {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no {key}"))
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// A per-block value stored as runs (`[[block, value], ...]` at each change), one per block.
fn unrun(v: &JsonValue, key: &str, blocks: usize) -> Vec<JsonValue> {
    let runs = v.get(key).and_then(JsonValue::as_array).unwrap();
    let mut out = Vec::with_capacity(blocks);
    for (k, r) in runs.iter().enumerate() {
        let r = r.as_array().unwrap();
        let end = runs.get(k + 1).map_or(blocks, |n| {
            n.as_array().unwrap()[0].as_f64().unwrap() as usize
        });
        while out.len() < end {
            out.push(r[1].clone());
        }
    }
    out
}

fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len());
    // (a NaN on either side is an infinite error: `f64::max` would drop it)
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = (x - y).abs();
            if d.is_nan() {
                f64::INFINITY
            } else {
                d
            }
        })
        .fold(0.0, f64::max)
}

// ---- the audio cases --------------------------------------------------------------------

/// One case against Swift's: exact where it is a decision, within `TOLERANCE` of the case's peak
/// where it is audio. Returns the worst audio error (relative) and whether the hash matched.
fn case_of(want: &JsonValue) -> &JsonValue {
    want.get("case").unwrap()
}

fn compare(d: &mut Diffs, want: &JsonValue, sr: f64) -> (f64, bool) {
    let name = s(want, "name");
    let got = run_case(want.get("case").unwrap(), sr);
    d.eq(
        format!("{name} ticks"),
        got.ticks.clone(),
        numbers(want, "ticks")
            .iter()
            .map(|x| *x as i64)
            .collect::<Vec<_>>(),
    );
    let blocks = got.ticks.len();
    d.eq(
        format!("{name} scrubbing"),
        got.scrubbing.clone(),
        unrun(want, "scrubbing", blocks)
            .iter()
            .map(|x| x.as_bool().unwrap())
            .collect::<Vec<_>>(),
    );
    d.eq(
        format!("{name} tempos"),
        got.tempos.clone(),
        unrun(want, "tempos", blocks)
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect::<Vec<_>>(),
    );
    if let Some(hits) = want.get("hits").and_then(JsonValue::as_array) {
        let rows: Vec<String> = hits
            .iter()
            .map(|h| {
                let h = h.as_array().unwrap();
                format!(
                    "{} {} {} {} {} {} {} {}",
                    h[0].as_f64().unwrap() as i64,
                    h[1].as_f64().unwrap() as i64,
                    h[2].as_f64().unwrap() as i64,
                    h[3].as_str().unwrap(),
                    h[4].as_f64().unwrap() as i64,
                    h[5].as_f64().unwrap() as i64,
                    h[6].as_str().unwrap(),
                    h[7].as_f64().unwrap() as i64,
                )
            })
            .collect();
        let mine: Vec<String> = got
            .hits
            .iter()
            .map(|h| {
                format!(
                    "{} {} {} {} {} {} {} {}",
                    h.frame, h.grid_frame, h.tick, h.net, h.note, h.channel, h.kind, h.velocity
                )
            })
            .collect();
        d.eq(format!("{name} hits"), mine, rows);
    } else {
        d.eq(format!("{name} hits"), got.hits.len(), 0);
    }
    // the trims `.balance` prepared with (measured off the project where the case gives none)
    match (&got.balance, want.get("balance")) {
        (Some(mine), Some(theirs)) => {
            d.eq(
                format!("{name} trim parts"),
                mine.keys().cloned().collect::<Vec<_>>(),
                theirs
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, _)| k.to_string())
                    .collect::<Vec<_>>(),
            );
            for (k, v) in theirs.as_object().unwrap().iter() {
                let (a, b) = (mine[k], v.as_f64().unwrap());
                d.checked += 1;
                if (a - b).abs() > 1e-6 {
                    d.items
                        .push(format!("{name}: trim {k} is {a} dB against Swift's {b}"));
                }
            }
        }
        (None, None) => {}
        (mine, theirs) => d.items.push(format!(
            "{name}: trims {mine:?} against Swift's {:?}",
            theirs.map(|t| t.as_object().map(|o| o.keys().count()))
        )),
    }
    // the gain `.loudness` measured (or was given) and the rest schedule the gestures built
    d.checked += 1;
    let want_gain = f(want, "gain");
    if (got.gain - want_gain).abs() > 1e-7 * want_gain.abs().max(1.0) {
        d.items.push(format!(
            "{name}: gain is {} against Swift's {want_gain}",
            got.gain
        ));
    }
    let want_rests: Vec<[i64; 3]> = want
        .get("rests")
        .and_then(JsonValue::as_array)
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    let r = r.as_array().unwrap();
                    [
                        r[0].as_f64().unwrap() as i64,
                        r[1].as_f64().unwrap() as i64,
                        r[2].as_f64().unwrap() as i64,
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    d.eq(format!("{name} rests"), got.rests.clone(), want_rests);
    let want_peaks = numbers(want, "peaks");
    let top = want_peaks.iter().cloned().fold(0.0, f64::max).max(1e-12);
    let hash_ok = floats_hash(&[&got.out[..], &got.out_r[..]].concat()) == s(want, "hash");
    let mut worst = max_abs_diff(&got.peaks, &want_peaks)
        .max(max_abs_diff(&got.rms, &numbers(want, "rms")))
        / top;
    let decimate = case_of(want)
        .get("decimate")
        .and_then(JsonValue::as_f64)
        .unwrap_or(128.0) as usize;
    let wave: Vec<f64> = got
        .out
        .iter()
        .step_by(decimate)
        .map(|x| *x as f64)
        .collect();
    worst = worst.max(max_abs_diff(&wave, &numbers(want, "wave")) / top);
    if !got.out_r.is_empty() {
        // the right channel, held to the same peak
        let wave_r: Vec<f64> = got
            .out_r
            .iter()
            .step_by(decimate)
            .map(|x| *x as f64)
            .collect();
        worst = worst
            .max(max_abs_diff(&got.peaks_r, &numbers(want, "peaksR")) / top)
            .max(max_abs_diff(&got.rms_r, &numbers(want, "rmsR")) / top)
            .max(max_abs_diff(&wave_r, &numbers(want, "waveR")) / top);
    }
    d.checked += 1;
    if worst.is_nan() || worst > TOLERANCE {
        // where it first parts company with Swift (the block, and the two peaks there)
        let first = got
            .peaks
            .iter()
            .zip(&want_peaks)
            .position(|(a, b)| (a - b).abs() / top > TOLERANCE)
            .map_or(String::new(), |b| {
                format!(
                    "; first differing block {b}: peak {} against Swift's {}",
                    got.peaks[b], want_peaks[b]
                )
            });
        d.items.push(format!(
            "{name}: audio is {worst:e} of the peak off Swift's (tolerance {TOLERANCE:e}){first}"
        ));
    }
    (worst, hash_ok)
}

fn audio_cases(section: &str) {
    audio_cases_in(golden(), section);
}

fn audio_cases_in(doc: JsonValue, section: &str) {
    let sr = rate(&doc);
    let mut d = Diffs::default();
    let (mut worst, mut exact, mut n) = (0.0f64, 0, 0);
    for want in rows(&doc, section) {
        let (w, hash_ok) = compare(&mut d, want, sr);
        worst = worst.max(w);
        exact += hash_ok as usize;
        n += 1;
        eprintln!(
            "  {section}/{}: worst {w:.3e} of the peak, hash {}",
            s(want, "name"),
            if hash_ok { "identical" } else { "differs" }
        );
    }
    eprintln!("{section}: {n} cases, {exact} bit-identical, worst error {worst:.3e} of the peak");
    d.finish(section);
}

/// WaveLiveTests.WaveScratchTests, FireEventTests (`scratchingTheNetClockMakes...`),
/// FixesUnderGesturesTests: golden form (b).
#[test]
fn scratching_matches_swift() {
    audio_cases("scratch");
}

/// WaveLiveTests (`liveKeysSoundWhileHeldAndRingOutAfter`), WaveDeckSourceTests,
/// SweepRealTimeTests: golden form (b).
#[test]
fn live_keys_match_swift() {
    audio_cases("live");
}

/// SpaceTests, EchoTests, RoomShimmerTests, and `setUnitSends`: golden form (b).
#[test]
fn the_room_and_the_echo_sends_match_swift() {
    audio_cases("sends");
}

/// DuckTests: bass and pad dip on each kick, through the room, on a swung song, under a hand:
/// golden form (b).
#[test]
fn ducking_matches_swift() {
    audio_cases("duck");
}

/// EnergyTests: the section arc's level and lowpass on songs, with the returns, a swap, a hand
/// on the platter and the soft master: golden form (b).
#[test]
fn the_energy_arc_matches_swift() {
    audio_cases("energy");
}

/// SynthVoiceTests, UnisonTests, SweepTests: the filter envelope, the detuned pairs and the
/// buildup's sweep, on notes and on a song under the groove, a hand and live keys: golden form
/// (b).
#[test]
fn the_voice_shaping_matches_swift() {
    audio_cases("voices");
}

/// SubOctaveTests: the sine an octave under a synth bass: golden form (b).
#[test]
fn the_sub_octave_matches_swift() {
    audio_cases("suboctave");
}

/// BalanceTests: the parts' trims as a gain on each synth lane, given and measured at prepare
/// (and the measured trims themselves against Swift's): golden form (b).
#[test]
fn the_balance_trims_match_swift() {
    audio_cases("balance");
}

/// StereoTests, UnisonTests: the mid/side pan, the spread voices and the wide unison pair, the
/// returns' side, the energy arc's wide path: golden form (b), both channels.
#[test]
fn the_stereo_field_matches_swift() {
    audio_cases("stereo");
}

/// RiserTests: the noise riser into a drop, mono and wide, through the room, from a jump, under
/// a hand and across a stop: golden form (b).
#[test]
fn the_riser_matches_swift() {
    audio_cases("riser");
}

/// MusicalityTests (the fills, the section crash and the open hats on the deck),
/// DropGapTests.closedHatChokesTheOpenHat, BrushTests, SpectralSlopeTests.HFTiltTests: the
/// voiced fill nets, crash and open hats, the choke, the noise drums' lowpass and the brush
/// snare: golden form (b).
#[test]
fn the_drums_match_swift() {
    audio_cases_in(golden_fixes(), "drums");
}

/// SectionTransposeTests, HarmonicRhythmTests: the band moved by the section plan, forward, under
/// a hand and under the groove: golden form (b).
#[test]
fn the_section_harmony_matches_swift() {
    audio_cases_in(golden_fixes(), "harmony");
}

/// DropGapTests, LayerBudgetTests, BandStopTests, KickOutTests (the schedule `prepare` builds from
/// the fixes: its entries and what rests), LoudnessRangeTests, LoudnessSpreadTests (the gain
/// `.loudness` measures in `prepare`): golden form (b).
#[test]
fn the_rests_and_the_loudness_match_swift() {
    audio_cases_in(golden_fixes(), "rests");
}

// ---- the sampled kits -------------------------------------------------------------------

/// A bank that numbers what it is asked for: the kit, and (like Apple's bank, which the Swift dump
/// loads) a Brush kit beside it.
#[derive(Default)]
struct KitBank {
    keys: std::sync::Mutex<Vec<String>>,
}

impl SamplerSlots for KitBank {
    fn instrument_sources(
        &self,
        _: &str,
        _: &str,
        _: f64,
        _: bool,
    ) -> Option<Vec<djrust::voices::SamplerSource>> {
        None
    }
    fn option_sources(
        &self,
        _: &str,
        _: &str,
        _: f64,
    ) -> Option<Vec<djrust::voices::SamplerSource>> {
        None
    }
    fn slot_for(&self, sources: &[djrust::voices::SamplerSource]) -> Option<usize> {
        let key = sources.first()?.to_string();
        let mut keys = self.keys.lock().unwrap();
        Some(keys.iter().position(|k| *k == key).unwrap_or_else(|| {
            keys.push(key);
            keys.len() - 1
        }))
    }
    fn kit_source(&self, id: &str) -> Option<djrust::voices::SamplerSource> {
        let (file, bank, program) = djrust::voices::drumkits::parse(id)?;
        (file == "gs_instruments.dls").then(|| djrust::voices::SamplerSource::Kit {
            path: format!("/System/{file}"),
            bank,
            program,
        })
    }
    fn brush_kit(
        &self,
        kit: &djrust::voices::SamplerSource,
    ) -> Option<djrust::voices::SamplerSource> {
        let djrust::voices::SamplerSource::Kit { path, .. } = kit else {
            return None;
        };
        Some(djrust::voices::SamplerSource::Kit {
            path: path.clone(),
            bank: 128,
            program: 40,
        })
    }
}

/// SampledKitTests / BrushTests: what `buildLanes` decides for the drums of a track that picked a
/// kit - which lanes bind, on which GM key, held how long, at what velocity, and whether the snare
/// shares the kick's slot - against the same project prepared by Swift on Apple's own GM kit
/// (`gs_instruments.dls#128:0`; the section is skipped when the Mac that wrote the golden had none).
/// The sampler's audio is AUSampler's and is not compared: form (b) for the decisions only.
#[test]
fn the_sampled_kit_decisions_match_swift() {
    let doc = golden_fixes();
    let kits = doc.get("kits").expect("the kits section");
    if !flag(kits, "available") {
        eprintln!("skipped: the Mac that wrote the golden had no gs_instruments.dls kit");
        return;
    }
    let id = s(kits, "kit");
    let project = |kit: Option<&str>| {
        let mut p = Project {
            tempo: 120.0,
            ..Default::default()
        };
        for (name, note, ch, steps) in [
            ("kick", 36, 10, vec![0, 4, 8, 12]),
            ("snare", 38, 11, vec![4, 12]),
            ("hihat", 42, 12, (0..16).collect::<Vec<i64>>()),
        ] {
            let mut n = NetBundle::ring(16);
            for st in steps {
                n.bind(st, note, ch, 100, 50);
            }
            n.track.channel = ch;
            n.track.kit = kit.map(String::from);
            p.nets.insert(name, n);
        }
        p
    };
    let mut d = Diffs::default();
    for (variant, kit) in [
        ("plain", Some(id)),
        ("parity", Some(id)),
        ("unpicked", None),
        ("nowhere", Some("nowhere.sf2#128:0")),
        ("balance", Some(id)),
        ("brushes", Some(id)),
    ] {
        let mut o = WaveOptions {
            master: Master::Linear,
            instruments: Some(InstrumentTable::shared()),
            bank: (variant != "parity").then(|| std::sync::Arc::new(KitBank::default()) as _),
            ..Default::default()
        };
        match variant {
            "balance" => {
                o.musicality = Fixes::BALANCE;
                o.track_balance = Some(
                    [("kick".to_string(), -6.0), ("snare".to_string(), 3.0)]
                        .into_iter()
                        .collect(),
                );
            }
            "brushes" => o.brushes = true,
            _ => {}
        }
        let prepared = WaveRunner::prepare(&project(kit), 48_000.0, &o, 0);
        let kick = prepared.lanes().iter().find(|l| l.kind == "kick").unwrap();
        let mine: Vec<&Lane> = prepared.lanes().iter().filter(|l| !l.is_tonal()).collect();
        let want = rows(kits, variant);
        d.eq(format!("{variant} lanes"), mine.len(), want.len());
        for (l, w) in mine.iter().zip(want) {
            let tag = format!("{variant}/{}", s(w, "net"));
            d.eq(format!("{tag} net"), l.net_id.as_str(), s(w, "net"));
            d.eq(format!("{tag} kind"), l.kind.as_str(), s(w, "kind"));
            d.eq(
                format!("{tag} bound"),
                l.sample_slot().is_some(),
                flag(w, "bound"),
            );
            d.eq(
                format!("{tag} with the kick"),
                l.sample_slot().is_some() && l.sample_slot() == kick.sample_slot(),
                flag(w, "withKick"),
            );
            let (note, hold, velocity) = l.kit();
            d.eq(format!("{tag} note"), note, f(w, "note") as i64);
            d.eq(format!("{tag} hold"), hold, f(w, "holdMs"));
            d.eq(format!("{tag} velocity"), velocity, f(w, "velocity"));
        }
    }
    d.finish("sampled kit decisions");
}

// ---- the part levels --------------------------------------------------------------------

/// `SamplerPartLevels` and `ownSynthPartDB` / `ownPartLevelDB` against Swift's: the probes
/// exactly, the levels (dB, K-weighted over a rendered probe) within 1e-6 dB.
#[test]
fn part_levels_match_swift() {
    let doc = golden();
    let pl = doc.get("partLevels").unwrap();
    let mut d = Diffs::default();
    d.eq(
        "passes",
        SamplerPartLevels::PASSES as i64,
        f(pl, "passes") as i64,
    );
    d.eq(
        "version",
        SamplerPartLevels::VERSION,
        f(pl, "version") as i64,
    );
    d.eq(
        "groups",
        SamplerPartLevels::GROUPS
            .iter()
            .map(|g| g.to_string())
            .collect::<Vec<_>>(),
        pl.get("groups")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .map(|g| g.as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
    );
    for p in rows(pl, "probes") {
        let g = s(p, "group");
        d.eq(
            format!("probeGroup({g})"),
            SamplerPartLevels::probe_group(g),
            s(p, "probeGroup"),
        );
        let (notes, seconds) = SamplerPartLevels::probe(s(p, "probeGroup"));
        d.eq(format!("probe({g}) seconds"), seconds, f(p, "seconds"));
        let want: Vec<(i64, f64, f64)> = p
            .get("notes")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .map(|n| {
                let n = n.as_array().unwrap();
                (
                    n[0].as_f64().unwrap() as i64,
                    n[1].as_f64().unwrap(),
                    n[2].as_f64().unwrap(),
                )
            })
            .collect();
        d.eq(format!("probe({g}) notes"), notes, want);
    }
    let mut worst = 0.0f64;
    let mut close = |d: &mut Diffs, tag: String, got: f64, want: f64| {
        let err = if (got - want).is_nan() {
            f64::INFINITY
        } else {
            (got - want).abs()
        };
        worst = worst.max(err);
        d.checked += 1;
        if err > 1e-6 {
            d.items.push(format!("{tag}: got {got}, want {want}"));
        }
    };
    for r in rows(pl, "synth") {
        let (g, sr) = (s(r, "group"), f(r, "rate"));
        close(
            &mut d,
            format!("synthLevelDB({g}, {sr})"),
            SamplerPartLevels::synth_level_db(g, sr),
            f(r, "db"),
        );
    }
    let table = InstrumentTable::shared();
    for r in rows(pl, "own") {
        let (instrument, g, sr) = (s(r, "instrument"), s(r, "group"), f(r, "rate"));
        let voice = table.voice(Some(instrument), Some(g));
        close(
            &mut d,
            format!("ownSynthPartDB({instrument}, {g}, {sr})"),
            own_synth_part_db(&voice, g, sr, &|_| {}),
            f(r, "db"),
        );
        let level = own_part_level_db(Some(f(r, "measured")), &voice, g, sr, &|_| {}).unwrap();
        close(
            &mut d,
            format!("ownPartLevelDB({instrument}, {g}, {sr})"),
            level,
            f(r, "level"),
        );
        assert!(own_part_level_db(None, &voice, g, sr, &|_| {}).is_none());
    }
    // The lanes shaped as `buildLanes` shapes them (`.subtractive`, `.unison`): the rows
    // Swift's dump writes for `ownSynthPartDB(subtractive:unison:subOctave:design:)`.
    for r in rows(pl, "shaped") {
        let (instrument, g) = (s(r, "instrument"), s(r, "group"));
        let voice = table.voice(Some(instrument), Some(g));
        let design = match s(r, "design") {
            "none" => VoiceDesign::default(),
            genre => VoiceDesign::for_genre(genre),
        };
        let shaping = VoiceShaping {
            subtractive: flag(r, "subtractive"),
            unison: flag(r, "unison"),
            sub_octave: flag(r, "subOctave"),
            design,
        };
        let sr = f(r, "rate");
        close(
            &mut d,
            format!(
                "shaped({instrument}, {g}, sub {}, uni {}, oct {}, {})",
                flag(r, "subtractive"),
                flag(r, "unison"),
                flag(r, "subOctave"),
                s(r, "design")
            ),
            own_synth_part_db(&voice, g, sr, &|l| shaping.apply(l, g, sr)),
            f(r, "db"),
        );
    }
    eprintln!("part levels: worst error {worst:.2e} dB");
    d.finish("part levels");
}
