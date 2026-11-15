//! Shared by the djwasm integration tests: the golden corpus, the ABI-1 render driver and its
//! stream hashes. Everything here goes through the exported `djw_*` functions only, exactly as a
//! worklet does, so a test that holds a render to the golden holds the *module surface*.
//!
//! The corpus and the hash recipe are written into `fixtures/golden-0.2.0.json` next to each
//! case's hashes, so an outside replayer (the scratch Node script that ran the same corpus on the
//! pinned 0.2.0 wasm, or beats' own golden harness) can drive the same cases without this file.
#![allow(dead_code)]

use std::sync::Mutex;

use djrust::json::{JsonObject, JsonValue};
use djwasm::*;

/// The exports share one engine and one out buffer: one test at a time, per test binary.
pub static LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The render quantum every renderer uses (Web Audio's).
pub const Q: usize = 128;

pub fn out() -> String {
    unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(djw_out_ptr(), djw_out_len()))
            .unwrap()
            .to_string()
    }
}

pub fn out_bytes() -> Vec<u8> {
    unsafe { std::slice::from_raw_parts(djw_out_ptr(), djw_out_len()).to_vec() }
}

pub fn call(f: unsafe extern "C" fn(*const u8, usize) -> i32, s: &str) -> i32 {
    unsafe { f(s.as_ptr(), s.len()) }
}

pub fn sha(bytes: &[u8]) -> String {
    djrust::cid::hex(&djrust::cid::sha256(bytes))
}

pub fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Where a case's project comes from: composed by the module itself (`djw_compose`, beats'
/// `compose` byte for byte) or a document file under `tests/fixtures/`.
#[derive(Clone, Debug)]
pub enum Source {
    Compose(String),
    File(String),
}

impl Source {
    pub fn doc(&self) -> String {
        match self {
            Source::Compose(spec) => {
                assert!(call(djw_compose, spec) > 0, "compose {spec}: {}", out());
                JsonValue::parse(&out())
                    .unwrap()
                    .get("doc")
                    .unwrap()
                    .js_stringified()
            }
            Source::File(name) => std::fs::read_to_string(fixtures().join(name))
                .unwrap_or_else(|e| panic!("{name}: {e}")),
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        match self {
            Source::Compose(spec) => o.set("compose", JsonValue::parse(spec).unwrap()),
            Source::File(name) => o.set("file", JsonValue::str(name.clone())),
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Source {
        if let Some(c) = v.get("compose") {
            Source::Compose(c.js_stringified())
        } else {
            Source::File(v.get("file").and_then(JsonValue::as_str).unwrap().into())
        }
    }
}

/// An ABI-1 call made between two quanta (before quantum `q` is rendered).
#[derive(Clone, Debug)]
pub enum Op {
    Tempo(f64),
    Mute(String, bool),
    Load(Source),
    Play,
    Pause,
    Stop,
}

impl Op {
    pub fn apply(&self) {
        match self {
            Op::Tempo(b) => djw_set_tempo(*b),
            Op::Mute(id, m) => {
                assert_eq!(unsafe { djw_set_mute(id.as_ptr(), id.len(), *m as i32) }, 0);
            }
            Op::Load(s) => {
                let d = s.doc();
                assert!(call(djw_load, &d) > 0, "{}", out());
            }
            Op::Play => djw_play(),
            Op::Pause => djw_pause(),
            Op::Stop => djw_stop(),
        }
    }

    fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        match self {
            Op::Tempo(b) => {
                o.set("op", JsonValue::str("tempo"));
                o.set("bpm", JsonValue::Number(*b));
            }
            Op::Mute(id, m) => {
                o.set("op", JsonValue::str("mute"));
                o.set("id", JsonValue::str(id.clone()));
                o.set("muted", JsonValue::Bool(*m));
            }
            Op::Load(s) => {
                o.set("op", JsonValue::str("load"));
                o.set("source", s.to_json());
            }
            Op::Play => o.set("op", JsonValue::str("play")),
            Op::Pause => o.set("op", JsonValue::str("pause")),
            Op::Stop => o.set("op", JsonValue::str("stop")),
        }
        JsonValue::Object(o)
    }

    fn from_json(v: &JsonValue) -> Op {
        match v.get("op").and_then(JsonValue::as_str).unwrap() {
            "tempo" => Op::Tempo(v.get("bpm").and_then(JsonValue::as_f64).unwrap()),
            "mute" => Op::Mute(
                v.get("id").and_then(JsonValue::as_str).unwrap().into(),
                v.get("muted").and_then(JsonValue::as_bool).unwrap(),
            ),
            "load" => Op::Load(Source::from_json(v.get("source").unwrap())),
            "play" => Op::Play,
            "pause" => Op::Pause,
            "stop" => Op::Stop,
            o => panic!("op {o}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    pub source: Source,
    pub sample_rate: f64,
    /// Quanta of [`Q`] frames to render.
    pub quanta: usize,
    pub script: Vec<(usize, Op)>,
}

impl Case {
    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("name", JsonValue::str(self.name.clone()));
        o.set("source", self.source.to_json());
        o.set("sampleRate", JsonValue::Number(self.sample_rate));
        o.set("quanta", JsonValue::int(self.quanta as i64));
        o.set(
            "script",
            JsonValue::Array(
                self.script
                    .iter()
                    .map(|(q, op)| {
                        let mut s = JsonObject::new();
                        s.set("q", JsonValue::int(*q as i64));
                        s.set("call", op.to_json());
                        JsonValue::Object(s)
                    })
                    .collect(),
            ),
        );
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Case {
        Case {
            name: v.get("name").and_then(JsonValue::as_str).unwrap().into(),
            source: Source::from_json(v.get("source").unwrap()),
            sample_rate: v.get("sampleRate").and_then(JsonValue::as_f64).unwrap(),
            quanta: v.get("quanta").and_then(JsonValue::as_f64).unwrap() as usize,
            script: v
                .get("script")
                .and_then(JsonValue::as_array)
                .unwrap()
                .iter()
                .map(|s| {
                    (
                        s.get("q").and_then(JsonValue::as_f64).unwrap() as usize,
                        Op::from_json(s.get("call").unwrap()),
                    )
                })
                .collect(),
        }
    }
}

pub const GENRES: [&str; 19] = [
    "ambient",
    "blues",
    "bossa",
    "country",
    "dnb",
    "dubstep",
    "edm",
    "funk",
    "garage",
    "house",
    "jazz",
    "lofi",
    "metal",
    "reggae",
    "speedcore",
    "synthwave",
    "techno",
    "trance",
    "trap",
];

/// The macroCurve + feelCurve arrangement: net macros (honoured from 0.3.0 when control macros
/// are on), master and aux ids (0.4) and one id no build knows, on every section name a
/// blueprint uses (entries for sections the pick lacks are skipped by compose).
pub const MACRO_CURVE_SPEC: &str = r#"{"genre":"techno","seed":11,"structure":"standard","macroCurve":[{"section":"intro","macro":"beat-repeat","bars":1},{"section":"verse","macro":"drop","bars":1},{"section":"build","macro":"half-time","bars":1},{"section":"chorus","macro":"cut","bars":4},{"section":"drop","macro":"sweep-lp","bars":2},{"section":"breakdown","macro":"ping-pong","bars":2},{"section":"bridge","macro":"tape-stop","bars":1},{"section":"outro","macro":"no-such-macro","bars":1}],"feelCurve":[{"section":"intro","x":0.2,"y":0.3},{"section":"verse","x":0.7,"y":0.6}]}"#;

/// The whole golden corpus (ABI-1 calls only).
pub fn corpus() -> Vec<Case> {
    let mut v = Vec::new();
    let secs = |sr: f64, s: f64| ((s * sr) as usize).div_ceil(Q);
    for sr in [48000.0, 44100.0] {
        let tag = sr as i64;
        for g in GENRES {
            for seed in [42, 7] {
                for structure in [None, Some("standard")] {
                    let spec = match structure {
                        None => format!(r#"{{"genre":"{g}","seed":{seed}}}"#),
                        Some(s) => {
                            format!(r#"{{"genre":"{g}","seed":{seed},"structure":"{s}"}}"#)
                        }
                    };
                    v.push(Case {
                        name: format!("{g}:{seed}:{}@{tag}", structure.unwrap_or("loop")),
                        source: Source::Compose(spec),
                        sample_rate: sr,
                        quanta: secs(sr, 10.0),
                        script: vec![],
                    });
                }
            }
        }
        for (name, spec) in [
            (
                "dnb:3:drop",
                r#"{"genre":"dnb","seed":3,"structure":"drop"}"#,
            ),
            (
                "metal:9:build",
                r#"{"genre":"metal","seed":9,"structure":"build"}"#,
            ),
            (
                "house:5:fade-in+drum-break",
                r#"{"genre":"house","seed":5,"fade-in":true,"drum-break":true}"#,
            ),
            ("techno:11:macro-curve", MACRO_CURVE_SPEC),
        ] {
            v.push(Case {
                name: format!("{name}@{tag}"),
                source: Source::Compose(spec.into()),
                sample_rate: sr,
                quanta: secs(sr, 20.0),
                script: vec![],
            });
        }
        for f in [
            "hand-authored.json",
            "macro-orchestrated.json",
            "metronome.json",
            "phantom-aqueduct.json",
            "voltage-rush.json",
        ] {
            v.push(Case {
                name: format!("examples/{f}@{tag}"),
                source: Source::File(format!("examples/{f}")),
                sample_rate: sr,
                quanta: secs(sr, 20.0),
                script: vec![],
            });
        }
        // Live scripts: what a page does while playing, at fixed quanta.
        let c = |s: &str| Source::Compose(s.into());
        v.push(Case {
            name: format!("live:tempo@{tag}"),
            source: c(r#"{"genre":"techno","seed":42}"#),
            sample_rate: sr,
            quanta: secs(sr, 12.0),
            script: vec![(375, Op::Tempo(140.0)), (1125, Op::Tempo(100.0))],
        });
        v.push(Case {
            name: format!("live:mute@{tag}"),
            source: c(r#"{"genre":"house","seed":7}"#),
            sample_rate: sr,
            quanta: secs(sr, 12.0),
            script: vec![
                (300, Op::Mute("bass".into(), true)),
                (500, Op::Mute("kick".into(), true)),
                (900, Op::Mute("bass".into(), false)),
                (1100, Op::Mute("kick".into(), false)),
            ],
        });
        v.push(Case {
            name: format!("live:swap@{tag}"),
            source: c(r#"{"genre":"dnb","seed":42}"#),
            sample_rate: sr,
            quanta: secs(sr, 14.0),
            script: vec![(400, Op::Load(c(r#"{"genre":"jazz","seed":7}"#)))],
        });
        v.push(Case {
            name: format!("live:transport@{tag}"),
            source: c(r#"{"genre":"funk","seed":42}"#),
            sample_rate: sr,
            quanta: secs(sr, 12.0),
            script: vec![
                (300, Op::Pause),
                (400, Op::Play),
                (700, Op::Stop),
                (750, Op::Play),
            ],
        });
    }
    v
}

/// One render's streams, hashed: what the golden stores per case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hashes {
    /// sha256 of the interleaved little-endian f32 L/R bytes.
    pub pcm: String,
    /// sha256 of the i32 LE ticks-per-quantum sequence.
    pub ticks: String,
    /// sha256 of every `djw_report` text (blocks that ticked), each followed by `\n`.
    pub report: String,
    /// sha256 of `djw_marking` every 32 quanta and at the end, each followed by `\n`.
    pub marking: String,
    /// sha256 of `djw_muted` on the same schedule.
    pub muted: String,
    /// sha256 of the `djw_take_stopped` byte per quantum.
    pub stopped: String,
}

impl Hashes {
    pub const KEYS: [&'static str; 6] = ["pcm", "ticks", "report", "marking", "muted", "stopped"];

    pub fn get(&self, k: &str) -> &str {
        match k {
            "pcm" => &self.pcm,
            "ticks" => &self.ticks,
            "report" => &self.report,
            "marking" => &self.marking,
            "muted" => &self.muted,
            "stopped" => &self.stopped,
            _ => unreachable!(),
        }
    }
}

pub struct Render {
    /// Interleaved L/R.
    pub lr: Vec<f32>,
    pub hashes: Hashes,
}

/// Run a case through the ABI-1 sequence: `init` (normally `djw_init`), `djw_load`, `djw_play`,
/// then per quantum: the script's calls for it, `hook(q)`, `djw_process(128)`, `djw_report` when
/// it ticked, `djw_take_stopped`, and every 32 quanta `djw_marking` + `djw_muted`.
pub fn run_with(case: &Case, init: &dyn Fn(f64) -> i32, hook: &mut dyn FnMut(usize)) -> Render {
    run_full(case, init, &mut || {}, hook)
}

/// [`run_with`] plus `after_load()` between `djw_load` and `djw_play`.
pub fn run_full(
    case: &Case,
    init: &dyn Fn(f64) -> i32,
    after_load: &mut dyn FnMut(),
    hook: &mut dyn FnMut(usize),
) -> Render {
    assert!(init(case.sample_rate) > 0, "init: {}", out());
    let doc = case.source.doc();
    assert!(call(djw_load, &doc) > 0, "{}: {}", case.name, out());
    after_load();
    djw_play();
    let mut lr = Vec::with_capacity(case.quanta * Q * 2);
    let (mut ticks, mut report, mut marking, mut muted, mut stopped) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let snap = |marking: &mut Vec<u8>, muted: &mut Vec<u8>| {
        assert!(djw_marking() >= 2);
        marking.extend(out_bytes());
        marking.push(b'\n');
        assert!(djw_muted() >= 2);
        muted.extend(out_bytes());
        muted.push(b'\n');
    };
    for q in 0..case.quanta {
        for (at, op) in &case.script {
            if *at == q {
                op.apply();
            }
        }
        hook(q);
        let t = djw_process(Q as u32);
        assert!(t >= 0, "process: {t}");
        ticks.extend_from_slice(&t.to_le_bytes());
        unsafe {
            let l = std::slice::from_raw_parts(djw_left_ptr(), Q);
            let r = std::slice::from_raw_parts(djw_right_ptr(), Q);
            for i in 0..Q {
                lr.push(l[i]);
                lr.push(r[i]);
            }
        }
        if t > 0 {
            let n = djw_report();
            if n > 0 {
                report.extend(out_bytes());
                report.push(b'\n');
            }
        }
        stopped.push(djw_take_stopped() as u8);
        if q % 32 == 31 {
            snap(&mut marking, &mut muted);
        }
    }
    snap(&mut marking, &mut muted);
    let mut pcm = Vec::with_capacity(lr.len() * 4);
    for x in &lr {
        pcm.extend_from_slice(&x.to_le_bytes());
    }
    Render {
        hashes: Hashes {
            pcm: sha(&pcm),
            ticks: sha(&ticks),
            report: sha(&report),
            marking: sha(&marking),
            muted: sha(&muted),
            stopped: sha(&stopped),
        },
        lr,
    }
}

pub fn run(case: &Case) -> Render {
    run_with(case, &|sr| djw_init(sr), &mut |_| {})
}

/// The golden fixture: `{"engine","recipe","cases":[{...case, "hashes":{...}}]}`.
pub struct Golden {
    pub cases: Vec<(Case, Hashes)>,
}

pub fn golden_path() -> std::path::PathBuf {
    fixtures().join("golden-0.2.0.json")
}

pub fn load_golden() -> Golden {
    let text = std::fs::read_to_string(golden_path()).expect("golden-0.2.0.json");
    let v = JsonValue::parse(&text).unwrap();
    let cases = v
        .get("cases")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|c| {
            let h = c.get("hashes").unwrap();
            let s = |k: &str| h.get(k).and_then(JsonValue::as_str).unwrap().to_string();
            (
                Case::from_json(c),
                Hashes {
                    pcm: s("pcm"),
                    ticks: s("ticks"),
                    report: s("report"),
                    marking: s("marking"),
                    muted: s("muted"),
                    stopped: s("stopped"),
                },
            )
        })
        .collect();
    Golden { cases }
}

/// Every stream that differs from the golden, as `"case: key"` lines.
pub fn diff(case: &Case, want: &Hashes, got: &Hashes) -> Vec<String> {
    Hashes::KEYS
        .iter()
        .filter(|k| want.get(k) != got.get(k))
        .map(|k| format!("{}: {k}", case.name))
        .collect()
}

/// The first sample index (interleaved) at which two renders differ, and the largest |a-b|.
pub fn first_diff(a: &[f32], b: &[f32]) -> Option<(usize, f32)> {
    let first = a
        .iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits())?;
    let max = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    Some((first, max))
}

/// A doc's net ids in document order.
pub fn net_ids(doc: &str) -> Vec<String> {
    JsonValue::parse(doc)
        .unwrap()
        .get("nets")
        .and_then(JsonValue::as_object)
        .unwrap()
        .iter()
        .map(|(k, _)| k.to_string())
        .collect()
}

/// `djw_fire_macro(id, duration)`.
pub fn fire(id: &str, duration: i32) -> i32 {
    unsafe { djw_fire_macro(id.as_ptr(), id.len(), duration) }
}

pub fn disable(id: &str, on: bool) -> i32 {
    unsafe { djw_set_macro_disabled(id.as_ptr(), id.len(), on as i32) }
}

/// Every id of beats' macro catalog (42), from `djw_keys`.
pub fn catalog_ids() -> Vec<String> {
    assert!(djw_keys() > 0);
    let k = JsonValue::parse(&out()).unwrap();
    let m = k.get("macros").unwrap();
    let mut v = Vec::new();
    for g in ["net", "master", "aux", "hit"] {
        for id in m.get(g).and_then(JsonValue::as_array).unwrap() {
            v.push(id.as_str().unwrap().to_string());
        }
    }
    v
}

/// Drain `djw_events` (when any wait) as parsed JSON entries.
pub fn drain_events() -> Vec<JsonValue> {
    if djw_event_count() == 0 {
        return Vec::new();
    }
    let n = djw_events();
    assert!(n >= 2);
    JsonValue::parse(&out())
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

pub fn ev_str<'a>(e: &'a JsonValue, k: &str) -> &'a str {
    e.get(k).and_then(JsonValue::as_str).unwrap()
}

pub fn ev_num(e: &JsonValue, k: &str) -> f64 {
    e.get(k).and_then(JsonValue::as_f64).unwrap()
}

/// The doc with its nets in id order (the host's RawNets order): what the host oracle and the
/// module are both given, so noise lanes seed alike.
pub fn id_sorted(doc: &str) -> String {
    let mut v = JsonValue::parse(doc).unwrap();
    if let JsonValue::Object(o) = &mut v {
        let nets = o
            .get("nets")
            .and_then(JsonValue::as_object)
            .unwrap()
            .clone();
        let mut ids: Vec<&str> = nets.iter().map(|(k, _)| k).collect();
        ids.sort();
        let mut sorted = JsonObject::new();
        for id in ids {
            sorted.set(id, nets.get(id).unwrap().clone());
        }
        o.set("nets", JsonValue::Object(sorted));
    }
    v.js_stringified()
}

/// Render `quanta` blocks of the loaded, playing module into interleaved L/R.
pub fn render_quanta(quanta: usize, hook: &mut dyn FnMut(usize)) -> Vec<f32> {
    let mut lr = Vec::with_capacity(quanta * Q * 2);
    for q in 0..quanta {
        hook(q);
        djw_process(Q as u32);
        unsafe {
            let l = std::slice::from_raw_parts(djw_left_ptr(), Q);
            let r = std::slice::from_raw_parts(djw_right_ptr(), Q);
            for i in 0..Q {
                lr.push(l[i]);
                lr.push(r[i]);
            }
        }
    }
    lr
}
