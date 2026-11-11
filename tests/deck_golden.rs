//! Swift goldens for the deck's audio side (`tools/paritydump/deck.swift`, section "deck",
//! `crates/djengine/tests/fixtures/swift/deck.json`): `WaveDeckSource` against `DeckCore`.
//!
//! Every case in the file is a project, a set of options and a script of commands given as data;
//! this test runs the very same data through a `DeckCore` (commands applied at the top of the
//! block they are scheduled for, then the block rendered) and compares, block by block, what the
//! deck published and what it rendered:
//!
//! - **exact**: the tick and the tick's phase (`f64` bits), the tempo, the transport flags
//!   (playing, ended, scrubbing, macro active), the macros in effect, which nets a macro holds,
//!   the mute words, the master macros' mask, the filter corners, `rolesHeld(by:among:)`, and the
//!   marking mirror (`readMarking`: which project, sequence, tick, frame, mutes and every
//!   token; `markingLayout` of the sounding and of the latest project). These are decisions and
//!   plain arithmetic on the same `f64` operations;
//! - **audio**: the sha256 of every float rendered. Where Swift's render is reproduced to the bit
//!   the hash decides; where Apple's libm and the `libm` crate differ in the last bit of
//!   `sin` / `exp` / `pow` / `tanh` it does not, and the peaks, the sums, the raw blocks and the
//!   limiter's gain reduction are compared by [`TOLERANCE`] and [`GR_TOLERANCE_DB`] (see there).
//!
//! The file is written on a Mac and committed; the test panics, naming the `regen.sh` command,
//! when it is missing (`common_fixture`).
//! A later part that adds cases to the section (the track pipeline) adds them in
//! `tools/paritydump/deck.swift` and, if it needs a new command, one arm in [`apply`] here.

mod common_fixture;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use djrust::cid::sha256;
use djrust::fx::unit::FxUnitCommand;
use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::json::{JsonObject, JsonValue};
use djrust::musicality::fixes::Fixes;
use djrust::pflow::Project;
use djrust::wave::net::str_hash;
use djrust::wave::{Master, MarkingLayout, MarkingSnapshot, NetMacro, WaveOptions, WaveRunner};
use djrust::{compose, wave::InstrumentTable};
use djrust::host::deck::{Command, DeckCore, DeckMirror};

/// How far a render may be from Swift's, relative to the case's own peak, where it is not the
/// same bits (14 of the 17 cases are; exact would be a hash match). The three that are not:
/// `keys-on-a-plucked-string` (a Karplus-Strong string's `pow`, held to the runner golden's
/// order of magnitude), `marking-swap-at-the-bar` (four songs through swaps and mutes: a last-bit
/// libm difference somewhere in one of them, within the bound the other two set) and
/// `master-macros`, whose filter corners (`exp` / `pow`) are one ulp apart from Apple's libm, and four stacked macros
/// (a phaser, a bit crusher whose quantiser steps flip on the ulp, a distortion) carry that to
/// 1.9e-5 of the peak (-94 dB) at the worst block. The tolerance is 2.6x the measured worst.
const TOLERANCE: f64 = 5e-5;
/// The limiter's gain reduction, dB, where the audio under it is not the same bits: measured
/// worst 5.1e-5 on a 3.5 dB reduction (it follows the audio, which is 1.9e-5 of the peak off
/// there; every other case matches to the bit), 4x that.
const GR_TOLERANCE_DB: f64 = 2e-4;
/// The master filter corners, relative: one ulp of an `exp` / `pow` is 1.1e-16.
const CORNER_TOLERANCE: f64 = 1e-12;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("deck")
}

fn s<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {k} in {}", v.js_stringified()))
}

fn f(v: &JsonValue, k: &str) -> f64 {
    v.get(k)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {k}"))
}

fn arr<'a>(v: &'a JsonValue, k: &str) -> &'a [JsonValue] {
    v.get(k)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {k}"))
}

fn flag(v: &JsonValue, k: &str) -> bool {
    v.get(k).and_then(JsonValue::as_bool).unwrap_or(false)
}

fn bits(d: f64) -> String {
    format!("{:016x}", d.to_bits())
}

fn from_bits(h: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(h, 16).unwrap())
}

// ---- running a case ---------------------------------------------------------------------

fn project(spec: &JsonValue) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", f(spec, "seed").into());
    if let Some(st) = spec.get("structure").and_then(JsonValue::as_str) {
        ov.set("structure", st.into());
    }
    compose(s(spec, "genre"), &ov.into())
}

/// `deckOptions` in the dump.
fn options(case: &JsonValue) -> WaveOptions {
    let mut o = WaveOptions {
        master: match case.get("master").and_then(JsonValue::as_str) {
            Some("soft") => Master::Soft,
            Some("limit") => Master::Limit,
            _ => Master::Linear,
        },
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
        o.musicality |= Fixes::named(name.as_str().unwrap()).expect("a fix");
    }
    if let Some(g) = case.get("forGenre").and_then(JsonValue::as_str) {
        o = o.for_genre(g);
    }
    o
}

/// What the deck has loaded most recently (`WaveDeckSource.latest`): its generation and the
/// role tables of its graph, which answer `rolesHeld`.
struct Latest {
    generation: i32,
    tables: djrust::wave::RoleTables,
}

struct Run {
    core: DeckCore,
    mirror: Arc<DeckMirror>,
    rate: f64,
    opts: WaveOptions,
    want_tables: bool,
    latest: Option<Latest>,
    /// The generation of each project the case loaded, in the order it loaded them (the dump
    /// names a project by that index: generations are process-wide counters).
    gens: Vec<i32>,
}

impl Run {
    /// Compile `spec` (seeded at `start_tick`) and hand it to the deck (`load`): the deck keeps
    /// its marking mirror and layout (`remember`) as the engine's load does.
    fn load(&mut self, spec: &JsonValue, start_tick: i64) {
        let p = project(spec);
        let wp = WaveRunner::prepare(&p, self.rate, &self.opts, start_tick);
        self.gens.push(wp.generation);
        if self.want_tables {
            self.latest = Some(Latest {
                generation: wp.generation,
                tables: wp.graph.role_tables(),
            });
        }
        self.mirror.remember_marking(wp.marking.clone());
        self.core.apply(Command::Load(wp));
    }

    /// The index of the project a generation belongs to (-1 for none of them).
    fn index_of(&self, generation: i32) -> i64 {
        self.gens
            .iter()
            .position(|&g| g == generation)
            .map_or(-1, |k| k as i64)
    }
}

/// One command of a script (`deckApply` in the dump).
fn apply(run: &mut Run, op: &[JsonValue]) {
    let num = |k: usize| op[k].as_f64().unwrap();
    let int = |k: usize| num(k) as i64;
    let text = |k: usize| op[k].as_str().unwrap();
    let on = |k: usize| op[k].as_bool().unwrap();
    let c = &mut run.core;
    match op[0].as_str().unwrap() {
        "play" => c.apply(Command::Play),
        "pause" => c.apply(Command::Pause),
        "stop" => c.apply(Command::Stop),
        "panic" => c.apply(Command::Panic),
        "tempo" => c.apply(Command::Tempo(num(1))),
        "mute" => c.apply(Command::Mute {
            hash: str_hash(text(1)),
            muted: on(2),
        }),
        "roleMute" => c.apply(Command::RoleMute {
            hash: str_hash(text(1)),
            muted: on(2),
        }),
        "slot" => c.apply(Command::Slot(str_hash(text(1)))),
        "macro" => {
            let m = NetMacro::ALL[int(1) as usize];
            let ticks = if op.len() > 2 { int(2) } else { m.ticks(None) };
            c.apply(Command::Macro { m, ticks });
        }
        "stinger" => c.apply(Command::Stinger {
            hash: str_hash(text(1)),
            ticks: int(2),
        }),
        "noteOn" => c.apply(Command::note_on(int(1), int(2))),
        "noteOff" => c.apply(Command::note_off(int(1))),
        "allOff" => c.apply(Command::AllNotesOff),
        "align" => c.apply(Command::Align(num(1))),
        "freeze" => c.apply(Command::Freeze),
        "scrub" => c.apply(Command::Scrub(num(1))),
        "release" => c.apply(Command::Release),
        "chase" => c.apply(Command::Chase(num(1))),
        "master" => c.apply(Command::MasterMacro {
            m: MasterMacro::ALL[int(1) as usize],
            bars: if op.len() > 2 { int(2) as i16 } else { -1 },
        }),
        "masterFx" => c.apply(Command::MasterFx {
            key: MasterFxKey::ALL[int(1) as usize],
            value: int(2) as i32,
        }),
        "unit" => c.apply(Command::FxUnit(FxUnitCommand::Set {
            param: int(1) as u8,
            value: num(2),
            snap: on(3),
        })),
        "kill" => c.apply(Command::FxUnit(FxUnitCommand::Kill {
            on: on(1),
            frame: 0,
        })),
        "reset" => c.apply(Command::FxUnit(FxUnitCommand::Reset { frame: 0 })),
        "load" => {
            let at = if op.len() > 2 { int(2) } else { 0 };
            run.load(&op[1], at)
        }
        other => panic!("unknown deck op {other}"),
    }
}

fn words(f: impl Fn(usize) -> bool) -> String {
    (0..4)
        .map(|w| {
            let mut x = 0u64;
            for k in 0..64 {
                if f(w * 64 + k) {
                    x |= 1 << k;
                }
            }
            format!("{x:016x}")
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// `rolesHeldByMacros`, and `rolesHeld(by:)` per net macro, at a block.
type RolesAt = (usize, Vec<String>, Vec<(i64, Vec<String>)>);

/// What the dump records per block, as the same values.
struct Blocks {
    out: Vec<f32>,
    ticks: Vec<i64>,
    phases: Vec<String>,
    peaks: Vec<(u32, u32)>,
    sums: Vec<(f64, f64)>,
    tempos: Vec<String>,
    playing: Vec<bool>,
    ended: Vec<bool>,
    scrubbing: Vec<bool>,
    macro_active: Vec<bool>,
    macro_mask: Vec<i64>,
    held: Vec<String>,
    mutes: Vec<String>,
    master_mask: Vec<i64>,
    gr: Vec<f64>,
    corners: Vec<String>,
    raw: Vec<(usize, Vec<f32>)>,
    roles: Vec<RolesAt>,
    /// `readMarking` as the dump writes it, per block, and the two layouts.
    marking: Vec<String>,
    layouts: Vec<String>,
}

const ROLES: [&str; 8] = [
    "kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp",
];

fn run_case(case: &JsonValue) -> Blocks {
    let c = case.get("case").unwrap();
    let rate = c
        .get("rate")
        .and_then(JsonValue::as_f64)
        .unwrap_or(22_050.0);
    let block = c.get("block").and_then(JsonValue::as_f64).unwrap_or(128.0) as usize;
    let nblocks = f(c, "blocks") as usize;
    let opts = options(c);
    let mirror = Arc::new(DeckMirror::new());
    let brake = flag(c, "turntableBrake");
    let mut run = Run {
        core: DeckCore::new("A", rate, opts.clone(), brake, mirror.clone()),
        mirror: mirror.clone(),
        rate,
        opts,
        want_tables: c
            .get("roles")
            .is_some_and(|r| !r.as_array().unwrap().is_empty()),
        latest: None,
        gens: Vec::new(),
    };
    run.load(c.get("project").unwrap(), 0);
    let mut script: Vec<Vec<&[JsonValue]>> = vec![Vec::new(); nblocks];
    for step in arr(c, "script") {
        let row = step.as_array().unwrap();
        let at = row[0].as_f64().unwrap() as usize;
        if at < nblocks {
            script[at].push(&row[1..]);
        }
    }
    let rolled: Vec<usize> = c
        .get("roles")
        .and_then(JsonValue::as_array)
        .map(|a| a.iter().map(|v| v.as_f64().unwrap() as usize).collect())
        .unwrap_or_default();
    let raw_at: Vec<usize> = c
        .get("raw")
        .and_then(JsonValue::as_array)
        .map(|a| a.iter().map(|v| v.as_f64().unwrap() as usize).collect())
        .unwrap_or_default();

    let mut b = Blocks {
        out: Vec::new(),
        ticks: vec![],
        phases: vec![],
        peaks: vec![],
        sums: vec![],
        tempos: vec![],
        playing: vec![],
        ended: vec![],
        scrubbing: vec![],
        macro_active: vec![],
        macro_mask: vec![],
        held: vec![],
        mutes: vec![],
        master_mask: vec![],
        gr: vec![],
        corners: vec![],
        raw: vec![],
        roles: vec![],
        marking: vec![],
        layouts: vec![],
    };
    let mut snap = MarkingSnapshot::new();
    let (mut l, mut r) = (vec![0f32; block], vec![0f32; block]);
    for (k, ops) in script.iter().enumerate() {
        for op in ops {
            apply(&mut run, op);
        }
        run.core.render(&mut l, &mut r, (k * block) as i64);
        let m = &*run.mirror;
        let (mut pl, mut pr, mut sl, mut sr) = (0f32, 0f32, 0f64, 0f64);
        for i in 0..block {
            pl = pl.max(l[i].abs());
            pr = pr.max(r[i].abs());
            sl += l[i] as f64;
            sr += r[i] as f64;
        }
        b.out.extend_from_slice(&l);
        b.out.extend_from_slice(&r);
        b.ticks.push(m.tick());
        b.phases.push(bits(m.tick_phase()));
        b.peaks.push((pl.to_bits(), pr.to_bits()));
        b.sums.push((sl, sr));
        b.tempos.push(bits(m.tempo()));
        b.playing.push(m.is_playing());
        b.ended.push(m.stopped.load(Ordering::Relaxed));
        b.scrubbing.push(m.is_scrubbing());
        b.macro_active.push(m.macro_active.load(Ordering::Relaxed));
        b.macro_mask.push(m.macro_record().snapshot.mask as i64);
        b.held.push(words(|i| m.is_net_held_by_macro(i)));
        b.mutes.push(words(|i| m.is_net_muted(i)));
        b.master_mask
            .push(m.master_mask.load(Ordering::Relaxed) as i64);
        b.gr.push(m.limiter_gain_reduction_db());
        let (hp, lp, idle) = m.master_filter_corners();
        b.corners
            .push(format!("{}:{}:{}", bits(hp), bits(lp), idle as u8));
        b.marking.push(if m.read_marking(&mut snap) {
            let tokens: String = snap.tokens.iter().map(|t| bits(*t)).collect();
            let muted: String = snap
                .muted
                .iter()
                .map(|&x| if x != 0 { '1' } else { '0' })
                .collect();
            format!(
                "{}:{}:{}:{}:{}:{}",
                run.index_of(snap.generation()),
                snap.header.sequence,
                snap.tick(),
                snap.header.frame,
                muted,
                &sha_str(&tokens)[..16]
            )
        } else {
            "none".into()
        });
        let layout_key = |l: &Option<Arc<MarkingLayout>>| match l {
            None => "-".to_string(),
            Some(l) => {
                let sections: Vec<String> = l
                    .sections
                    .iter()
                    .map(|s| format!("{}@{}+{}", s.name, s.start_tick, s.steps))
                    .collect();
                format!(
                    "{}/{}/{}/{}/{}",
                    run.index_of(l.generation),
                    l.start_tick,
                    l.net_count(),
                    l.place_count(),
                    &sha_str(&sections.join(","))[..8]
                )
            }
        };
        let sounding = m.marking_layout_of(m.generation.load(Ordering::Relaxed));
        let newest = m.marking_layout();
        b.layouts.push(format!(
            "{} {} {}",
            layout_key(&sounding),
            layout_key(&newest),
            (sounding.as_ref().map(|l| l.track_serial) == newest.as_ref().map(|l| l.track_serial))
                as u8
        ));
        if raw_at.contains(&k) {
            let mut v = l.clone();
            v.extend_from_slice(&r);
            b.raw.push((k, v));
        }
        if rolled.contains(&k) {
            let latest = run.latest.as_ref().map(|x| (x.generation, &x.tables));
            let any = m
                .roles_held_by_macros_in(latest, &ROLES)
                .into_iter()
                .map(String::from)
                .collect();
            let by = NetMacro::ALL
                .iter()
                .map(|&nm| {
                    (
                        nm as i64,
                        m.roles_held_by_in(latest, nm, &ROLES)
                            .into_iter()
                            .map(String::from)
                            .collect(),
                    )
                })
                .collect();
            b.roles.push((k, any, by));
        }
    }
    b
}

// ---- comparing --------------------------------------------------------------------------

/// `[[block, value], …]` expanded to one value per block.
fn expand(runs: &JsonValue, blocks: usize) -> Vec<JsonValue> {
    let rows = runs.as_array().unwrap();
    let mut out = Vec::with_capacity(blocks);
    let mut cur = rows[0].as_array().unwrap()[1].clone();
    let mut next = 1;
    for k in 0..blocks {
        if next < rows.len() && rows[next].as_array().unwrap()[0].as_f64().unwrap() as usize == k {
            cur = rows[next].as_array().unwrap()[1].clone();
            next += 1;
        }
        out.push(cur.clone());
    }
    out
}

/// The first block each list differs at, with what each side had there, or none.
fn first_diff<T: PartialEq + std::fmt::Debug>(got: &[T], want: &[T]) -> Option<String> {
    got.iter()
        .zip(want)
        .position(|(a, b)| a != b)
        .map(|k| format!("block {k}: got {:?}, Swift {:?}", got[k], want[k]))
}

fn sha_str(s: &str) -> String {
    sha256(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

fn sha_of(x: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for v in x {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    sha256(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[test]
fn renders_match_swift() {
    let doc = golden();
    let cases = arr(&doc, "cases");
    assert!(!cases.is_empty());
    let mut problems: Vec<String> = Vec::new();
    let (mut identical, mut near) = (0, 0);
    let mut worst = 0f64;
    let mut worst_gr = 0f64;
    let mut inexact: Vec<&str> = Vec::new();
    let mut checked = 0usize;
    let mut marking_blocks = 0usize;
    for case in cases {
        let name = s(case, "name");
        let got = run_case(case);
        let n = got.ticks.len();
        let mut bad = |what: &str, at: Option<String>| {
            if let Some(k) = at {
                problems.push(format!("{name}: {what} differs first at {k}"));
            }
        };
        // ---- exact: what the deck publishes
        let want_ticks: Vec<i64> = arr(case, "ticks")
            .iter()
            .map(|v| v.as_f64().unwrap() as i64)
            .collect();
        assert_eq!(want_ticks.len(), n, "{name}: blocks");
        bad("tick", first_diff(&got.ticks, &want_ticks));
        let strs = |k: &str| -> Vec<String> {
            arr(case, k)
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        bad("tick phase", first_diff(&got.phases, &strs("phases")));
        let run_strs = |k: &str| -> Vec<String> {
            expand(case.get(k).unwrap(), n)
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let run_bools = |k: &str| -> Vec<bool> {
            expand(case.get(k).unwrap(), n)
                .iter()
                .map(|v| v.as_bool().unwrap())
                .collect()
        };
        let run_ints = |k: &str| -> Vec<i64> {
            expand(case.get(k).unwrap(), n)
                .iter()
                .map(|v| v.as_f64().unwrap() as i64)
                .collect()
        };
        bad("tempo", first_diff(&got.tempos, &run_strs("tempos")));
        bad("playing", first_diff(&got.playing, &run_bools("playing")));
        bad("ended", first_diff(&got.ended, &run_bools("ended")));
        bad(
            "scrubbing",
            first_diff(&got.scrubbing, &run_bools("scrubbing")),
        );
        bad(
            "macro active",
            first_diff(&got.macro_active, &run_bools("macroActive")),
        );
        bad(
            "macro mask",
            first_diff(&got.macro_mask, &run_ints("macroMask")),
        );
        bad(
            "nets held by a macro",
            first_diff(&got.held, &run_strs("held")),
        );
        bad("mute words", first_diff(&got.mutes, &run_strs("mutes")));
        // the marking mirror: which project, header, mutes, tokens; and the two layouts
        bad(
            "readMarking",
            first_diff(&got.marking, &run_strs("marking")),
        );
        bad(
            "markingLayout (sounding, latest)",
            first_diff(&got.layouts, &run_strs("layouts")),
        );
        marking_blocks += n;
        bad(
            "master macro mask",
            first_diff(&got.master_mask, &run_ints("masterMask")),
        );
        // the corners: HP, LP (within an ulp of a libm) and whether the chain is bypassed
        let corner = |c: &str| -> (f64, f64, bool) {
            let p: Vec<&str> = c.split(':').collect();
            (from_bits(p[0]), from_bits(p[1]), p[2] == "1")
        };
        let near_corner = |a: &(f64, f64, bool), b: &(f64, f64, bool)| {
            let close = |x: f64, y: f64| (x - y).abs() <= CORNER_TOLERANCE * x.abs().max(y.abs());
            close(a.0, b.0) && close(a.1, b.1) && a.2 == b.2
        };
        let want_corners = run_strs("corners");
        if let Some(k) = got
            .corners
            .iter()
            .zip(&want_corners)
            .position(|(g, w)| !near_corner(&corner(g), &corner(w)))
        {
            problems.push(format!(
                "{name}: filter corners differ first at block {k}: got {}, Swift {}",
                got.corners[k], want_corners[k]
            ));
        }
        checked += 12 * n;
        // ---- limiter: within a libm's worth
        let mut case_gr = 0f64;
        for (g, w) in got.gr.iter().zip(run_strs("gr")) {
            case_gr = case_gr.max((g - from_bits(&w)).abs());
        }
        worst_gr = worst_gr.max(case_gr);
        if case_gr > GR_TOLERANCE_DB {
            problems.push(format!(
                "{name}: limiter reduction is {case_gr:.2e} dB from Swift's (tolerance {GR_TOLERANCE_DB:e})"
            ));
        }
        // ---- roles held
        let want_roles = arr(case, "roles");
        assert_eq!(want_roles.len(), got.roles.len(), "{name}: roles blocks");
        for (w, (k, any, by)) in want_roles.iter().zip(&got.roles) {
            let strs = |v: &JsonValue| -> Vec<String> {
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect()
            };
            assert_eq!(f(w, "block") as usize, *k);
            if &strs(w.get("any").unwrap()) != any {
                problems.push(format!("{name}: rolesHeldByMacros at block {k}"));
            }
            for (row, (m, roles)) in arr(w, "by").iter().zip(by) {
                let row = row.as_array().unwrap();
                assert_eq!(row[0].as_f64().unwrap() as i64, *m);
                if &strs(&row[1]) != roles {
                    problems.push(format!("{name}: rolesHeld(by: {m}) at block {k}"));
                }
            }
            checked += 1 + by.len();
        }
        // ---- audio
        if sha_of(&got.out) == s(case, "hash") {
            identical += 1;
            continue;
        }
        let peak = got
            .peaks
            .iter()
            .map(|p| f32::from_bits(p.0).max(f32::from_bits(p.1)) as f64)
            .fold(0.0, f64::max)
            .max(1e-9);
        let want_peaks = arr(case, "peaks");
        let want_sums = arr(case, "sums");
        let mut case_worst = 0f64;
        for k in 0..n {
            let wp = want_peaks[k].as_array().unwrap();
            let ws = want_sums[k].as_array().unwrap();
            let (pl, pr) = (
                f32::from_bits(wp[0].as_f64().unwrap() as u32) as f64,
                f32::from_bits(wp[1].as_f64().unwrap() as u32) as f64,
            );
            let (gl, gr) = (
                f32::from_bits(got.peaks[k].0) as f64,
                f32::from_bits(got.peaks[k].1) as f64,
            );
            case_worst = case_worst
                .max((gl - pl).abs() / peak)
                .max((gr - pr).abs() / peak);
            let (sl, sr) = (
                from_bits(ws[0].as_str().unwrap()),
                from_bits(ws[1].as_str().unwrap()),
            );
            // a sum of a block is a sum of up to `block` samples, each within the tolerance
            let per = (got.sums[k].0 - sl).abs().max((got.sums[k].1 - sr).abs());
            case_worst = case_worst.max(per / (peak * (got.out.len() / (2 * n)) as f64));
        }
        for (k, v) in &got.raw {
            let want = case.get("raw").unwrap().get(&k.to_string()).unwrap();
            let want = want.as_array().unwrap();
            assert_eq!(want.len(), v.len(), "{name}: raw block {k}");
            for (g, w) in v.iter().zip(want) {
                let w = f32::from_bits(u32::from_str_radix(w.as_str().unwrap(), 16).unwrap());
                case_worst = case_worst.max((*g as f64 - w as f64).abs() / peak);
            }
            checked += v.len();
        }
        worst = worst.max(case_worst);
        near += 1;
        inexact.push(name);
        if case_worst > TOLERANCE {
            problems.push(format!(
                "{name}: audio is {case_worst:.3e} of its peak from Swift's (tolerance {TOLERANCE:e}); \
                 hash {} vs {}",
                sha_of(&got.out),
                s(case, "hash")
            ));
        }
    }
    eprintln!(
        "deck golden: {} cases, {checked} values and {marking_blocks} blocks of marking compared \
         exactly; audio {identical} bit-identical by sha256, {near} within {TOLERANCE:e} of the peak (worst {worst:.3e}; {inexact:?}); \
         limiter reduction worst {worst_gr:.2e} dB",
        cases.len()
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
