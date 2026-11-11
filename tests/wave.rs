//! Wave-engine parity, mirroring `BeatsWaveTests.swift`. The oracle is beats' own JS
//! (wave-engine net.js / synth.js / runner.js, through `scripts/beats-wave.mjs`), so
//! `tests/fixtures/js/wave.json` needs no Mac: the executor traces must match exactly,
//! the audio within 1e-5.

use djrust::json::{js_round, JsonValue};
use djrust::wave::*;
use djrust::{compose, Project};

fn wave() -> JsonValue {
    let path = format!("{}/tests/fixtures/js/wave.json", env!("CARGO_MANIFEST_DIR"));
    JsonValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn rows<'a>(d: &'a JsonValue, k: &str) -> &'a [JsonValue] {
    d.get(k)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no {k}"))
}

fn num(v: &JsonValue, k: &str) -> f64 {
    v.get(k)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {k}"))
}

fn string<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {k}"))
}

fn project(genre: &str, seed: f64, structure: &str) -> Project {
    let mut ov = djrust::JsonObject::new();
    ov.set("seed", seed.into());
    if structure != "loop" {
        ov.set("structure", structure.into());
    }
    compose(genre, &ov.into())
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 255,
    };
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|&c| c != b'=') {
        buf = (buf << 6) | val(c) as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

fn float32s(s: &str) -> Vec<f32> {
    base64(s)
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn fnv(h: u32, v: i32) -> u32 {
    (h ^ v as u32).wrapping_mul(16_777_619)
}

/// Mirror of tickChecksum() in scripts/beats-wave.mjs.
fn tick_checksum(g: &WaveGraph) -> u32 {
    let mut h: u32 = 2_166_136_261;
    h = fnv(h, g.tick as i32);
    for i in 0..g.fired_count {
        h = fnv(h, g.fired[i]);
    }
    h = fnv(h, 0x7fff);
    for i in 0..g.control_count {
        h = fnv(h, g.controls[i]);
    }
    h = fnv(h, g.overflow as i32);
    for n in &g.nets {
        for p in 0..n.n_places {
            h = fnv(h, js_round(n.state[p] * 1000.0) as i32);
        }
    }
    for i in 0..g.muted.len() {
        h = fnv(h, g.muted[i] as i32 + 2 * i as i32);
    }
    for i in 0..g.muted_notes.len() {
        if g.muted_notes[i] != 0 {
            h = fnv(h, i as i32 + 100_000);
        }
    }
    for i in 0..g.muted_groups.len() {
        h = fnv(h, g.muted_groups[i] as i32 + 3 * i as i32);
    }
    fnv(h, g.stop_requested as i32)
}

#[test]
fn integer_helpers_match() {
    let w = wave();
    for r in rows(&w, "strHashes") {
        assert_eq!(
            str_hash(string(r, "s")) as f64,
            num(r, "h"),
            "strHash {}",
            string(r, "s")
        );
    }
    for r in rows(&w, "noiseSeeds") {
        assert_eq!(
            noise_seed(num(r, "i") as i64) as f64,
            num(r, "seed"),
            "noiseSeed"
        );
    }
    for r in rows(&w, "rands") {
        let got = deterministic_rand(num(r, "tick") as i64, num(r, "salt") as i32);
        let want = f64::from_bits(u64::from_str_radix(string(r, "bits"), 16).unwrap());
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "rand {}/{}",
            num(r, "tick"),
            num(r, "salt")
        );
    }
}

#[test]
fn voices_match() {
    let w = wave();
    let voices = w.get("voices").unwrap();
    for r in rows(voices, "drums") {
        let want = VoiceSpec::from_json(r.get("voice").unwrap());
        let got = drum_voice(string(r, "kind"), string(r, "instrument"));
        assert_eq!(
            got,
            want,
            "drum {}/{}",
            string(r, "instrument"),
            string(r, "kind")
        );
    }
    let tonal = rows(voices, "tonal");
    assert_eq!(tonal.len(), 27 * 6);
    for r in tonal {
        let want = VoiceSpec::from_json(r.get("voice").unwrap());
        let got = tonal_voice(
            r.get("instrument").and_then(JsonValue::as_str),
            r.get("group").and_then(JsonValue::as_str),
        );
        assert_eq!(
            got,
            want,
            "tonal {:?}/{:?}",
            r.get("instrument"),
            r.get("group")
        );
    }
}

#[test]
fn biquads_and_tables_match() {
    let w = wave();
    for r in rows(&w, "biquads") {
        let got = biquad(string(r, "type"), num(r, "fc"), num(r, "q"), num(r, "sr"));
        let want = r.get("coeffs").unwrap().as_array().unwrap();
        for (a, b) in got.iter().zip(want) {
            assert!(
                (a - b.as_f64().unwrap()).abs() < 1e-13,
                "{}",
                string(r, "type")
            );
        }
    }
    for r in rows(&w, "tables") {
        let t = build_table(string(r, "wave"));
        assert_eq!(t.len(), WAVE_TABLE_SIZE + 1);
        for (k, want) in r.get("sum").unwrap().as_array().unwrap().iter().enumerate() {
            let d = (t[k * 251] as f64 - want.as_f64().unwrap()).abs();
            assert!(d < 1e-6, "{}[{}]", string(r, "wave"), k * 251);
        }
        assert_eq!(t[WAVE_TABLE_SIZE] as f64, num(r, "last"));
    }
}

#[test]
fn tick_traces_are_exact() {
    let w = wave();
    let traces = rows(&w, "traces");
    assert_eq!(traces.len(), 8);
    for tr in traces {
        let tag = format!("{}/{}", string(tr, "genre"), string(tr, "structure"));
        let mut g = WaveGraph::new(&project(
            string(tr, "genre"),
            num(tr, "seed"),
            string(tr, "structure"),
        ));
        assert_eq!(g.nets.len(), num(tr, "nets") as usize, "{tag} net count");
        let ids: Vec<String> = g.nets.iter().map(|n| n.id.clone()).collect();
        assert_eq!(
            Some(ids),
            tr.get("netIds").unwrap().strings(),
            "{tag} net order"
        );
        let sums = tr.get("checksums").unwrap().as_array().unwrap();
        let head = tr.get("head").unwrap().as_array().unwrap();
        let mut first_bad = None;
        for (k, sum) in sums.iter().enumerate() {
            g.advance();
            if sum.as_f64().unwrap() as i64 as u32 != tick_checksum(&g) && first_bad.is_none() {
                first_bad = Some(k);
            }
            if k < head.len() {
                let fired: Vec<i64> = g.fired[..g.fired_count].iter().map(|&x| x as i64).collect();
                assert_eq!(
                    Some(fired),
                    head[k].get("fired").unwrap().ints(),
                    "{tag} fired @{k}"
                );
                let ctl: Vec<i64> = g.controls[..g.control_count]
                    .iter()
                    .map(|&x| x as i64)
                    .collect();
                assert_eq!(
                    Some(ctl),
                    head[k].get("controls").unwrap().ints(),
                    "{tag} controls @{k}"
                );
            }
        }
        assert_eq!(first_bad, None, "{tag}: first checksum mismatch");
        assert_eq!(
            g.stop_requested,
            tr.get("stopRequested").unwrap().as_bool().unwrap(),
            "{tag} stop"
        );
        let muted: Vec<i64> = g.muted.iter().map(|&x| x as i64).collect();
        assert_eq!(
            Some(muted),
            tr.get("mutedAtEnd").unwrap().ints(),
            "{tag} muted"
        );
        assert_eq!(
            Some(g.groups.clone()),
            tr.get("groups").unwrap().strings(),
            "{tag} groups"
        );
    }
}

#[test]
fn incidence_and_invariants_match() {
    let w = wave();
    let rs = rows(&w, "invariants");
    let g = WaveGraph::new(&project("techno", 42.0, "standard"));
    assert_eq!(g.nets.len(), rs.len());
    for (n, r) in g.nets.iter().zip(rs) {
        assert_eq!(n.id, string(r, "id"));
        assert!(n.n_places == num(r, "P") as usize && n.n_trans == num(r, "T") as usize);
        let mat = n.incidence_matrix();
        let weighted: Vec<f64> = mat
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .fold(0.0, |a, (i, x)| a + x * (i + 1) as f64)
            })
            .collect();
        let want: Vec<f64> = r
            .get("incidence")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        assert_eq!(weighted, want, "incidence {}", n.id);
        // Elimination is cubic: long linear chains (fills, curves) are checked in JS only.
        if n.n_places > 128 {
            continue;
        }
        let ys = p_invariants(&mat);
        assert_eq!(
            ys.len(),
            num(r, "invariants") as usize,
            "invariant count {}",
            n.id
        );
        let ones = ys.len() == 1 && ys[0].iter().all(|v| (v - 1.0).abs() < 1e-9);
        assert_eq!(
            ones,
            r.get("ones").unwrap().as_bool().unwrap(),
            "all-ones invariant {}",
            n.id
        );
    }
}

#[test]
fn inhibitor_and_conflict_semantics() {
    // p0 feeds t0 and t1 (a conflict); t2 is inhibited by p3 and fires into p3.
    let doc = JsonValue::parse(
        r#"{"name":"x","tempo":120,"nets":{"n":{"places":{"p0":{"initial":[1]},"p1":{},"p2":{},"p3":{},"p4":{"initial":[1]}},
        "transitions":{"t0":{},"t1":{},"t2":{}},
        "arcs":[{"source":"p0","target":"t0"},{"source":"p0","target":"t1"},{"source":"t0","target":"p1"},{"source":"t1","target":"p2"},
        {"source":"p4","target":"t2"},{"source":"p3","target":"t2","inhibit":true},{"source":"t2","target":"p3"}]}}}"#,
    )
    .unwrap();
    let mut g = WaveGraph::from_json(&doc);
    g.advance();
    let n = &g.nets[0];
    // exactly one of t0/t1 won p0's token; t2 fired once and then p3 inhibits it
    assert_eq!(n.state[0], 0.0);
    assert_eq!(n.state[1] + n.state[2], 1.0);
    assert!(n.state[3] == 1.0 && n.state[4] == 0.0);
    g.advance();
    assert_eq!(g.nets[0].state[3], 1.0);
}

struct Rendered {
    left: Vec<f32>,
    right: Vec<f32>,
    tempo_at_end: f64,
    stopped: bool,
    samples: f64,
    graph_tick: i64,
}

fn render_case(c: &JsonValue) -> Rendered {
    let sr = num(c, "sr");
    let mut opts = WaveOptions::default();
    if let Some(r) = c.get("runner") {
        match r.get("master").and_then(JsonValue::as_str) {
            Some("linear") => opts.master = Master::Linear,
            Some("limit") => opts.master = Master::Limit,
            _ => {}
        }
        if let Some(g) = r.get("masterGain").and_then(JsonValue::as_f64) {
            opts.master_gain = g;
        }
        if let Some(vs) = r.get("voices").and_then(JsonValue::as_object) {
            for (id, v) in vs.iter() {
                opts.voices.insert(id.to_string(), VoiceSpec::from_json(v));
            }
        }
        for m in r
            .get("mods")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
        {
            opts.mods.push(WaveMod {
                target: string(m, "target").into(),
                source: string(m, "source").into(),
                kind: string(m, "kind").into(),
                depth: num(m, "depth"),
                silent_source: m
                    .get("silentSource")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false),
            });
        }
    }
    let spec = |p: &JsonValue| {
        project(
            p.get("genre")
                .and_then(JsonValue::as_str)
                .unwrap_or("techno"),
            p.get("seed").and_then(JsonValue::as_f64).unwrap_or(42.0),
            p.get("structure")
                .and_then(JsonValue::as_str)
                .unwrap_or("loop"),
        )
    };
    let mut runner = WaveRunner::new(sr, opts);
    runner.load(&spec(c.get("project").unwrap()));
    if let Some(t) = c.get("tempo").and_then(JsonValue::as_f64) {
        runner.set_tempo(t);
    }
    runner.play();
    let frames = js_round(num(c, "seconds") * sr) as usize;
    let mut left = vec![0f32; frames];
    let mut right = vec![0f32; frames];
    let mut ops: Vec<(usize, &JsonValue)> = c
        .get("ops")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .collect();
    ops.sort_by(|a, b| {
        num(a.1, "block")
            .partial_cmp(&num(b.1, "block"))
            .unwrap()
            .then(a.0.cmp(&b.0))
    });
    let (mut oi, mut blk, mut off) = (0, 0usize, 0usize);
    while off < frames {
        while oi < ops.len() && num(ops[oi].1, "block") <= blk as f64 {
            let op = ops[oi].1;
            oi += 1;
            match string(op, "op") {
                "tempo" => runner.set_tempo(num(op, "bpm")),
                "mute" => runner.set_mute(
                    string(op, "netId"),
                    op.get("muted").unwrap().as_bool().unwrap(),
                ),
                "load" => runner.load(&spec(op.get("project").unwrap())),
                "pause" => runner.pause(),
                "play" => runner.play(),
                "stop" => runner.stop(),
                _ => {}
            }
        }
        let n = 128.min(frames - off);
        runner.process(&mut left[off..off + n], Some(&mut right[off..off + n]), n);
        off += 128;
        blk += 1;
    }
    Rendered {
        left,
        right,
        tempo_at_end: runner.tempo(),
        stopped: runner.stopped(),
        samples: runner.samples(),
        graph_tick: runner.graph().unwrap().tick,
    }
}

#[test]
fn renders_match_within_one_e_minus_five() {
    let w = wave();
    let cases = rows(&w, "renders");
    assert_eq!(cases.len(), 12);
    let mut worst = 0f32;
    for c in cases {
        let tag = string(c, "tag");
        let want = float32s(string(c, "audio"));
        let got = render_case(c);
        assert_eq!(got.left.len(), want.len(), "{tag} length");
        let mut max_diff = 0f32;
        let mut at = 0;
        for i in 0..want.len().min(got.left.len()) {
            let d = (got.left[i] - want[i]).abs();
            if d > max_diff {
                max_diff = d;
                at = i;
            }
        }
        worst = worst.max(max_diff);
        assert!(max_diff < 1e-5, "{tag}: max |d| {max_diff} at sample {at}");
        assert!(got.left == got.right, "{tag} mono -> stereo identical");
        assert_eq!(
            got.graph_tick,
            num(c, "graphTick") as i64,
            "{tag} graph tick"
        );
        assert_eq!(got.tempo_at_end, num(c, "tempoAtEnd"), "{tag} tempo");
        assert_eq!(
            got.stopped,
            c.get("stopped").unwrap().as_bool().unwrap(),
            "{tag} stopped"
        );
        assert_eq!(got.samples, num(c, "samplesCounted"), "{tag} samples");
    }
    eprintln!("max sample error across the 12 renders: {worst:e}");
}

/// The comparison has to be able to fail: another case's audio must not satisfy it.
#[test]
fn render_comparison_rejects_other_audio() {
    let w = wave();
    let cases = rows(&w, "renders");
    let other = float32s(string(&cases[2], "audio"));
    let got = render_case(&cases[0]);
    let mut max_diff = 0f32;
    for i in 0..other.len().min(got.left.len()) {
        max_diff = max_diff.max((got.left[i] - other[i]).abs());
    }
    assert!(max_diff > 1e-2);
    assert!(got.left.iter().any(|&x| x != 0.0));
}

#[test]
fn rendering_is_deterministic() {
    let w = wave();
    let c = &rows(&w, "renders")[0];
    assert!(render_case(c).left == render_case(c).left);
}
