//! Swift goldens for the mirrors (`tools/paritydump/mirrors.swift`, fixture
//! `swift/mirrors.json`, form (b) of the parity rules): what a reader gets back from
//! `MacroMirror` and `MarkingMirror` after the audio thread has played a project, block by block
//! or tick by tick, with macros fired, mutes set, the clock scratched and projects swapped at a
//! bar. Everything compared is an integer, a string or the bit pattern of a token count, so
//! there is no tolerance: the Rust must reproduce the Swift words exactly.
//!
//! Each case carries its ops (what ran, in Swift's order); this test replays them through the
//! same `Source` harness the ported Swift tests use, reads the mirrors after every block the way
//! the Swift dump did, and compares the records. Projects are named by their index in the case
//! (generations and track serials are process-wide counters).

mod common;
mod mirror_support;

use common::*;
use djrust::cid::{hex, sha256};
use djrust::json::JsonValue;
use djrust::wave::*;
use mirror_support::*;

struct Op {
    at: i64,
    kind: String,
    a: String,
    b: String,
    x: f64,
    y: f64,
    flag: bool,
}

fn ops_of(c: &JsonValue) -> Vec<Op> {
    rows(c, "ops")
        .iter()
        .map(|o| Op {
            at: i(o, "at"),
            kind: s(o, "kind").to_string(),
            a: s(o, "a").to_string(),
            b: s(o, "b").to_string(),
            x: f(o, "x"),
            y: f(o, "y"),
            flag: o.get("flag").and_then(JsonValue::as_bool).unwrap(),
        })
        .collect()
}

fn sha_hex(text: &str) -> String {
    hex(&sha256(text.as_bytes()))
}

fn tokens_sha(t: &[f64]) -> String {
    sha_hex(&t.iter().map(|&v| bits_hex(v)).collect::<String>())
}

fn flag_string(m: &[u8]) -> String {
    m.iter().map(|&b| if b != 0 { '1' } else { '0' }).collect()
}

fn hex64(w: u64) -> String {
    format!("{w:016x}")
}

/// `macroKey` of the dump: mask, project and every non-zero word.
fn macro_key(s: &MacroSnapshot, project: i64) -> String {
    let mut k = format!("{}|{project}", s.mask);
    for (w, &v) in s.held.iter().enumerate() {
        if v != 0 {
            k += &format!("|{w}:{v}");
        }
    }
    k
}

fn macro_words(s: &MacroSnapshot) -> Vec<(i64, String)> {
    s.held
        .iter()
        .enumerate()
        .filter(|(_, &v)| v != 0)
        .map(|(k, &v)| (k as i64, hex64(v)))
        .collect()
}

fn expect_words(d: &mut Diffs, tag: &str, got: &MacroSnapshot, want: &JsonValue) {
    let want: Vec<(i64, String)> = want
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p.at(0).unwrap().as_i64().unwrap(),
                p.at(1).unwrap().as_str().unwrap().to_string(),
            )
        })
        .collect();
    d.eq(format!("{tag} words"), macro_words(got), want);
}

fn expect_snapshot(d: &mut Diffs, tag: &str, snap: &MarkingSnapshot, want: &JsonValue) {
    d.eq(format!("{tag} seq"), snap.header.sequence, i(want, "seq"));
    d.eq(format!("{tag} tick"), snap.header.tick, i(want, "tick"));
    d.eq(format!("{tag} frame"), snap.header.frame, i(want, "frame"));
    d.eq(
        format!("{tag} muted"),
        flag_string(&snap.muted),
        s(want, "muted").to_string(),
    );
    d.eq(
        format!("{tag} tokens"),
        tokens_sha(&snap.tokens),
        s(want, "tokens").to_string(),
    );
}

/// How the layouts' ring coordinates compared. They are `sin` / `cos` of the step angles, and
/// Apple's libm differs from the `libm` crate in the last bit for some angles: a net whose
/// coordinate hash differs is compared number by number (absolute error), everything else in
/// the layout is exact.
#[derive(Default)]
struct GeomStats {
    exact: usize,
    inexact: usize,
    max_err: f64,
}

/// Largest absolute error allowed between a Rust coordinate and Swift's. Measured: 139 of 247
/// nets differ, by at most 3.64e-12, which is one ulp at the largest coordinate in the fixture
/// (30669.6; a ulp there is 3.64e-12). Allowed: 1e-11, about three ulps at that size.
const GEOM_TOLERANCE: f64 = 1e-11;

fn geom_values(info: &NetInfo) -> Vec<f64> {
    let mut v: Vec<f64> = Vec::new();
    v.extend(&info.place_x);
    v.extend(&info.place_y);
    v.extend(&info.transition_x);
    v.extend(&info.transition_y);
    for a in &info.arcs {
        v.extend([a.x1, a.y1, a.x2, a.y2]);
    }
    v
}

fn numbers(v: &JsonValue, key: &str) -> Vec<f64> {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

fn expect_geometry(
    d: &mut Diffs,
    t: &str,
    got: &NetInfo,
    want_hash: &str,
    geoms: &JsonValue,
    stats: &mut GeomStats,
) {
    let coords: String = got
        .place_x
        .iter()
        .chain(&got.place_y)
        .chain(&got.transition_x)
        .chain(&got.transition_y)
        .map(|&v| bits_hex(v))
        .collect();
    let arcs: String = got
        .arcs
        .iter()
        .map(|a| {
            [a.x1, a.y1, a.x2, a.y2]
                .iter()
                .map(|&v| bits_hex(v))
                .collect::<String>()
        })
        .collect();
    if sha_hex(&format!("{coords}|{arcs}")) == want_hash {
        d.eq(format!("{t} geom"), true, true);
        stats.exact += 1;
        return;
    }
    let want = geoms.get(want_hash).expect("the dump names its geometries");
    let mut w: Vec<f64> = Vec::new();
    for k in ["placeX", "placeY", "transitionX", "transitionY", "arcs"] {
        w.extend(numbers(want, k));
    }
    let g = geom_values(got);
    d.eq(format!("{t} geom length"), g.len(), w.len());
    let err = g
        .iter()
        .zip(&w)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max);
    d.eq(
        format!("{t} geom within {GEOM_TOLERANCE:e} (worst {err:e})"),
        err <= GEOM_TOLERANCE,
        true,
    );
    stats.inexact += 1;
    stats.max_err = stats.max_err.max(err);
}

fn expect_layout(
    d: &mut Diffs,
    tag: &str,
    l: &MarkingLayout,
    want: &JsonValue,
    geoms: &JsonValue,
    stats: &mut GeomStats,
) {
    d.eq(format!("{tag} startTick"), l.start_tick, i(want, "startTick"));
    d.eq(format!("{tag} bars"), l.bars, i(want, "bars"));
    d.eq(
        format!("{tag} netIds"),
        l.net_ids.clone(),
        want.get("netIds").unwrap().strings().unwrap(),
    );
    d.eq(
        format!("{tag} placeOffsets"),
        l.place_offsets.iter().map(|&v| v as i64).collect::<Vec<_>>(),
        want.get("placeOffsets").unwrap().ints().unwrap(),
    );
    let place_ids = l
        .place_ids
        .iter()
        .map(|p| p.join(","))
        .collect::<Vec<_>>()
        .join("|");
    d.eq(
        format!("{tag} placeIds"),
        sha_hex(&place_ids),
        s(want, "placeIds").to_string(),
    );
    let secs = rows(want, "sections");
    d.eq(format!("{tag} sections"), l.sections.len(), secs.len());
    for (k, (got, w)) in l.sections.iter().zip(secs).enumerate() {
        let t = format!("{tag} section {k}");
        d.eq(format!("{t} name"), got.name.clone(), s(w, "name").to_string());
        d.eq(format!("{t} start"), got.start_tick, i(w, "startTick"));
        d.eq(format!("{t} steps"), got.steps, i(w, "steps"));
        d.eq(format!("{t} phraseCount"), got.phrase_count, i(w, "phraseCount"));
        d.eq(
            format!("{t} phrasePattern"),
            got.phrase_pattern.join(","),
            s(w, "phrasePattern").to_string(),
        );
    }
    let nets = rows(want, "nets");
    d.eq(format!("{tag} nets"), l.nets.len(), nets.len());
    for (got, w) in l.nets.iter().zip(nets) {
        let t = format!("{tag} net {}", got.id);
        d.eq(format!("{t} id"), got.id.clone(), s(w, "id").to_string());
        d.eq(format!("{t} role"), got.role.clone(), s(w, "role").to_string());
        d.eq(
            format!("{t} riffGroup"),
            got.riff_group.clone(),
            s(w, "riffGroup").to_string(),
        );
        d.eq(
            format!("{t} riffVariant"),
            got.riff_variant.clone(),
            s(w, "riffVariant").to_string(),
        );
        d.eq(
            format!("{t} group"),
            got.group.clone(),
            w.get("group").and_then(JsonValue::as_str).map(String::from),
        );
        d.eq(
            format!("{t} instrument"),
            got.instrument.clone(),
            s(w, "instrument").to_string(),
        );
        d.eq(format!("{t} channel"), got.channel, i(w, "channel"));
        d.eq(
            format!("{t} isDrum"),
            got.is_drum,
            w.get("isDrum").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(
            format!("{t} isStinger"),
            got.is_stinger,
            w.get("isStinger").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(format!("{t} noteLow"), got.note_low, i(w, "noteLow"));
        d.eq(format!("{t} noteHigh"), got.note_high, i(w, "noteHigh"));
        d.eq(
            format!("{t} transitions"),
            got.transition_ids.join(","),
            s(w, "transitions").to_string(),
        );
        d.eq(
            format!("{t} midi"),
            got.transition_has_midi
                .iter()
                .map(|&m| if m { '1' } else { '0' })
                .collect::<String>(),
            s(w, "midi").to_string(),
        );
        d.eq(format!("{t} arcCount"), got.arc_count as i64, i(w, "arcCount"));
        d.eq(format!("{t} arcs"), got.arcs.len() as i64, i(w, "arcs"));
        expect_geometry(d, &t, got, s(w, "geom"), geoms, stats);
    }
}

fn target(name: &str) -> MacroTargets {
    match name {
        "nonDrums" => MacroTargets::NonDrums,
        "drumsOnly" => MacroTargets::DrumsOnly,
        "everything" => MacroTargets::Everything,
        _ => MacroTargets::NonKick,
    }
}

fn macro_of(id: &str) -> NetMacro {
    NetMacro::from_id(id).unwrap_or_else(|| panic!("no macro {id}"))
}

#[test]
fn the_runner_cases_publish_the_swift_words() {
    let doc = fixture("mirrors");
    let geoms = doc.get("geometries").unwrap();
    let mut geom = GeomStats::default();
    let mut d = Diffs::default();
    let mut records = 0;
    for c in rows(&doc, "runners") {
        let name = s(c, "name").to_string();
        let sr = 22_050.0;
        let mut src = Source::new(sr);
        let mut loaded: Vec<Loaded> = Vec::new();
        let mut seeds: Vec<MarkingSnapshot> = Vec::new();

        fn add(
            src: &mut Source,
            loaded: &mut Vec<Loaded>,
            seeds: &mut Vec<MarkingSnapshot>,
            genre: &str,
            seed: f64,
            structure: &str,
            tick: i64,
        ) {
            let p = prepare(&project(genre, seed, structure), 22_050.0, tick, None);
            let mut snap = MarkingSnapshot::new();
            assert!(p.mirror.read_into(&mut snap));
            seeds.push(snap);
            loaded.push(src.load(p));
        }

        add(
            &mut src,
            &mut loaded,
            &mut seeds,
            s(c, "genre"),
            f(c, "seed"),
            s(c, "structure"),
            i(c, "startTick"),
        );
        src.set_tempo(f(c, "tempo"));
        if c.get("play").and_then(JsonValue::as_bool).unwrap() {
            src.play();
        }

        let ops = ops_of(c);
        let mut last_seq = vec![0i64; 64];
        let mut last_macro = macro_key(&MacroSnapshot::default(), -1);
        let mut markings: Vec<(i64, i64, MarkingSnapshot)> = Vec::new();
        let mut macros: Vec<(i64, i64, MacroRead)> = Vec::new();
        let mut next = 0;
        let mut snap = MarkingSnapshot::new();
        for block in 0..i(c, "blocks") {
            while next < ops.len() && ops[next].at <= block {
                let op = &ops[next];
                next += 1;
                match op.kind.as_str() {
                    "fire" => src.fire(macro_of(&op.a), Some(op.x as i64)),
                    "mute" => src.set_mute(&op.a, op.flag),
                    "roleMute" => src.set_role_muted(&op.a, op.flag),
                    "panic" => src.panic(),
                    "freeze" => src.freeze(),
                    "scrub" => src.scrub(op.x),
                    "release" => src.release(),
                    "pause" => src.pause(),
                    "play" => src.play(),
                    "stop" => src.stop(),
                    "tempo" => src.set_tempo(op.x),
                    "load" => add(
                        &mut src,
                        &mut loaded,
                        &mut seeds,
                        &op.a,
                        op.x,
                        &op.b,
                        op.y as i64,
                    ),
                    k => panic!("{name}: unknown op {k}"),
                }
            }
            src.render(128, block * 128);
            let gen = src.active_generation();
            if let Some(a) = loaded.iter().position(|l| l.generation == gen) {
                assert!(loaded[a].mirror.read_into(&mut snap));
                if snap.header.sequence != last_seq[a] {
                    last_seq[a] = snap.header.sequence;
                    markings.push((block, a as i64, snap.clone()));
                }
            }
            if let Some(r) = src.main.macro_mirror.read() {
                let project = loaded
                    .iter()
                    .position(|l| l.generation == r.generation)
                    .map_or(-1, |p| p as i64);
                let key = macro_key(&r.snapshot, project);
                if key != last_macro {
                    last_macro = key;
                    macros.push((block, project, r));
                }
            }
        }

        // the projects, as the layout describes them and as the mirror was seeded
        let layouts = rows(c, "layouts");
        let seed_rows = rows(c, "seeds");
        d.eq(format!("{name} projects"), loaded.len(), layouts.len());
        for (k, l) in loaded.iter().enumerate() {
            expect_layout(
                &mut d,
                &format!("{name} layout {k}"),
                &l.layout,
                &layouts[k],
                geoms,
                &mut geom,
            );
            expect_snapshot(&mut d, &format!("{name} seed {k}"), &seeds[k], &seed_rows[k]);
        }
        // every marking record the running project's mirror showed
        let want = rows(c, "markings");
        d.eq(format!("{name} marking records"), markings.len(), want.len());
        for (got, w) in markings.iter().zip(want) {
            let tag = format!("{name} marking @{}", got.0);
            d.eq(format!("{tag} block"), got.0, i(w, "block"));
            d.eq(format!("{tag} project"), got.1, i(w, "project"));
            expect_snapshot(&mut d, &tag, &got.2, w);
            records += 1;
        }
        // every macro record
        let want = rows(c, "macros");
        d.eq(format!("{name} macro records"), macros.len(), want.len());
        for (got, w) in macros.iter().zip(want) {
            let tag = format!("{name} macros @{}", got.0);
            d.eq(format!("{tag} block"), got.0, i(w, "block"));
            d.eq(format!("{tag} project"), got.1, i(w, "project"));
            d.eq(format!("{tag} mask"), got.2.snapshot.mask as i64, i(w, "mask"));
            expect_words(&mut d, &tag, &got.2.snapshot, w.get("words").unwrap());
            records += 1;
        }
        // where every project's mirror ended, token by token
        for w in rows(c, "finals") {
            let k = i(w, "project") as usize;
            let mut fin = MarkingSnapshot::new();
            assert!(loaded[k].mirror.read_into(&mut fin));
            let tag = format!("{name} final {k}");
            expect_snapshot(&mut d, &tag, &fin, w);
            let values: Vec<f64> = w
                .get("values")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
            d.eq(
                format!("{tag} token bits"),
                fin.tokens.iter().map(|&v| bits_hex(v)).collect::<Vec<_>>(),
                values.iter().map(|&v| bits_hex(v)).collect::<Vec<_>>(),
            );
        }
    }
    assert!(records > 500, "the cases published {records} records");
    eprintln!(
        "mirrors: net geometry {} exact, {} within tolerance (worst {:e})",
        geom.exact, geom.inexact, geom.max_err
    );
    d.finish("mirrors: runner cases");
}

#[test]
fn the_graph_cases_publish_the_swift_words() {
    let doc = fixture("mirrors");
    let geoms = doc.get("geometries").unwrap();
    let mut geom = GeomStats::default();
    let mut d = Diffs::default();
    for c in rows(&doc, "graphs") {
        let name = s(c, "name").to_string();
        let proj = project(s(c, "genre"), f(c, "seed"), s(c, "structure"));
        let mut g = WaveGraph::new(&proj);
        let mirror = MarkingMirror::new(1, 1, 0, &g, &proj);
        expect_layout(
            &mut d,
            &format!("{name} layout"),
            &mirror.layout,
            c.get("layout").unwrap(),
            geoms,
            &mut geom,
        );
        let ops = ops_of(c);
        let ticks = i(c, "ticks");
        let mut snap = MarkingSnapshot::new();
        let mut last_seq = 0;
        let mut last_macro = macro_key(&MacroSnapshot::default(), 0);
        let mut markings: Vec<(i64, MarkingSnapshot)> = Vec::new();
        let mut macros: Vec<(i64, MacroSnapshot)> = Vec::new();
        let mut next = 0;
        for step in 0..=ticks {
            while next < ops.len() && ops[next].at <= step {
                let op = &ops[next];
                next += 1;
                let tag = NetMacro::from_id(&op.b).map_or(0, |m| m.tag());
                match op.kind.as_str() {
                    "fireMute" => {
                        g.fire_mute(target(&op.a), op.x as i64, tag);
                    }
                    "schedule" => g.schedule_mute(target(&op.a), op.x as i64, op.y as i64, tag),
                    "beatRepeat" => g.fire_beat_repeat(op.x as i64, 2, 1),
                    "cancel" => g.cancel_macros(),
                    "roleMute" => g.set_role_muted(str_hash(&op.a), op.flag),
                    "mute" => {
                        let k = g.by_id[&op.a];
                        g.muted[k] = op.flag as u8;
                    }
                    "stinger" => g.fire_stinger(str_hash(&op.a), op.x as i64),
                    k => panic!("{name}: unknown graph op {k}"),
                }
            }
            mirror.publish(&g, step);
            assert!(mirror.read_into(&mut snap));
            if snap.header.sequence != last_seq {
                last_seq = snap.header.sequence;
                markings.push((step, snap.clone()));
            }
            let m = g.macro_snapshot();
            let key = macro_key(&m, 0);
            if key != last_macro {
                last_macro = key;
                macros.push((step, m));
            }
            if step < ticks {
                g.advance();
            }
        }
        let want = rows(c, "markings");
        d.eq(format!("{name} marking records"), markings.len(), want.len());
        for (got, w) in markings.iter().zip(want) {
            let tag = format!("{name} marking @{}", got.0);
            d.eq(format!("{tag} step"), got.0, i(w, "step"));
            expect_snapshot(&mut d, &tag, &got.1, w);
        }
        let want = rows(c, "macros");
        d.eq(format!("{name} macro records"), macros.len(), want.len());
        assert!(macros.len() > 5, "{name}: macros were in effect");
        for (got, w) in macros.iter().zip(want) {
            let tag = format!("{name} macros @{}", got.0);
            d.eq(format!("{tag} step"), got.0, i(w, "step"));
            d.eq(format!("{tag} mask"), got.1.mask as i64, i(w, "mask"));
            expect_words(&mut d, &tag, &got.1, w.get("words").unwrap());
        }
    }
    eprintln!(
        "mirrors: net geometry {} exact, {} within tolerance (worst {:e})",
        geom.exact, geom.inexact, geom.max_err
    );
    d.finish("mirrors: graph cases");
}

#[test]
fn the_record_packs_its_bits_as_swift_does() {
    let doc = fixture("mirrors");
    let c = doc.get("hold").unwrap();
    let mut d = Diffs::default();
    let mut snap = MacroSnapshot::default();
    for h in rows(c, "holds") {
        snap.hold(h.at(0).unwrap().as_i64().unwrap(), h.at(1).unwrap().as_i64().unwrap());
    }
    expect_words(&mut d, "hold", &snap, c.get("words").unwrap());
    for q in rows(c, "queries") {
        let net = i(q, "net");
        let by: String = NetMacro::ALL
            .iter()
            .map(|&m| if snap.is_held_by(net, m) { '1' } else { '0' })
            .collect();
        d.eq(format!("net {net} by"), by, s(q, "by").to_string());
        d.eq(
            format!("net {net} any"),
            snap.is_held(net),
            q.get("any").and_then(JsonValue::as_bool).unwrap(),
        );
    }
    d.finish("mirrors: hold case");
}
