//! Swift goldens for the Minecraft bank and its importer (`tools/paritydump/minecraft.swift`,
//! section "minecraft", `tests/fixtures/swift/minecraft.json`): the program table, the sidecar
//! text, the asset helpers, the octave filter / zones / shifts, the hit energy, the root
//! measurement, the crossfade loop, the SoundFont writer and the whole import over a synthetic
//! assets tree. Parity form (b) of the port rules.
//!
//! Everything is on synthetic sounds (an integer synthesiser, see `minecraft_support`); no
//! Minecraft asset is read or needed. Floats are compared by bit pattern or by the hash of
//! their bit patterns, and every comparison here is exact: the Rust `libm` and Apple's agree on
//! every call these paths make. The decode step (`MinecraftImport.decode`, AVFoundation) is the
//! host's: `djhost/tests/minecraft_import.rs` runs the same tree through symphonia.

mod common;
mod minecraft_support;

use std::collections::BTreeMap;
use std::ops::Range;

use common::*;
use djrust::json::JsonValue;
use djrust::sound::minecraft::assets::{
    is_forbidden, newest, sha1_hex, AssetFs, MinecraftAssets, Unavailable,
};
use djrust::sound::minecraft::import::{
    build, crossfade_loop, hit_energy, measure_root, octave_filter, octave_shift, octave_zones,
    ImportError,
};
use djrust::sound::minecraft::sf2::{Sf2Writer, Zone};
use djrust::sound::minecraft::{self as mc, Kind, Sidecar};
use minecraft_support::*;

fn golden() -> JsonValue {
    fixture("minecraft")
}

fn arr<'a>(v: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {key}"))
}

fn flag(v: &JsonValue, key: &str) -> bool {
    v.get(key)
        .and_then(JsonValue::as_bool)
        .unwrap_or_else(|| panic!("no bool {key}"))
}

fn opt_s(v: &JsonValue, key: &str) -> Option<String> {
    v.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn range_of(v: Option<&JsonValue>) -> Option<Range<usize>> {
    let a = v?.as_array()?;
    Some(a[0].as_f64()? as usize..a[1].as_f64()? as usize)
}

fn range_json(r: &Option<Range<usize>>) -> String {
    format!("{r:?}")
}

// MARK: the bank as data

#[test]
fn the_programs_the_kit_and_the_paths_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    let c = g.get("constants").unwrap();
    d.eq("fileName", mc::FILE_NAME, s(c, "fileName"));
    d.eq("sidecarName", mc::SIDECAR_NAME, s(c, "sidecarName"));
    d.eq("kitProgram", mc::KIT_PROGRAM, i(c, "kitProgram"));
    d.eq("kitID", mc::kit_id(), s(c, "kitID").to_string());
    d.eq("formatVersion", mc::FORMAT_VERSION, i(c, "formatVersion"));
    d.eq("schema", mc::SIDECAR_SCHEMA, s(c, "schema"));
    d.eq(
        "sampleRate",
        djrust::sound::minecraft::import::SAMPLE_RATE,
        f(c, "sampleRate"),
    );

    let programs = arr(&g, "programs");
    d.eq("programs: count", mc::bank_programs().len(), programs.len());
    for (p, w) in mc::bank_programs().iter().zip(programs) {
        let tag = format!("program {}", s(w, "name"));
        d.eq(format!("{tag}: name"), p.name.as_str(), s(w, "name"));
        d.eq(format!("{tag}: program"), p.program, i(w, "program"));
        d.eq(format!("{tag}: event"), p.event.clone(), opt_s(w, "event"));
        d.eq(format!("{tag}: sound"), p.sound.as_str(), s(w, "sound"));
        d.eq(
            format!("{tag}: kind"),
            p.kind == Kind::Tone,
            s(w, "kind") == "tone",
        );
        d.eq(format!("{tag}: root"), p.root, i(w, "root"));
        d.eq(format!("{tag}: loop"), p.looped, flag(w, "loop"));
        d.eq(format!("{tag}: minIndex"), p.min_index, i(w, "minIndex"));
    }
    // the (name, number) view palette selection uses
    d.eq(
        "programs()",
        mc::programs()
            .into_iter()
            .map(|(n, p)| format!("{n}={p}"))
            .collect::<Vec<_>>(),
        programs
            .iter()
            .map(|w| format!("{}={}", s(w, "name"), i(w, "program")))
            .collect::<Vec<_>>(),
    );

    let kit = arr(&g, "kitZones");
    d.eq("kit zones", mc::kit_zones().len(), kit.len());
    for (z, w) in mc::kit_zones().iter().zip(kit) {
        d.eq(format!("kit {}", z.name), z.name, s(w, "name"));
        d.eq(format!("kit {}: sound", z.name), z.sound, s(w, "sound"));
        d.eq(
            format!("kit {}: keys", z.name),
            (*z.keys.start(), *z.keys.end()),
            (i(w, "lo"), i(w, "hi")),
        );
        d.eq(
            format!("kit {}: level", z.name),
            bits_hex(z.level),
            s(w, "level").to_string(),
        );
    }

    for w in arr(&g, "paths") {
        let dir = s(w, "dir");
        d.eq(
            format!("path({dir:?})"),
            mc::path(dir),
            s(w, "bank").to_string(),
        );
        d.eq(
            format!("sidecar path({dir:?})"),
            mc::sidecar_path(dir),
            s(w, "sidecar").to_string(),
        );
    }
    d.finish("minecraft bank data");
}

#[test]
fn the_sidecar_text_and_parse_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "sidecars") {
        let sha1: BTreeMap<String, String> = w
            .get("sha1")
            .and_then(JsonValue::as_object)
            .unwrap()
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_str().unwrap().to_string()))
            .collect();
        let sc = Sidecar::new(
            s(w, "assetIndex"),
            sha1,
            w.get("programs").and_then(JsonValue::strings).unwrap(),
            i(w, "format"),
        );
        d.eq(
            format!("{}: version", s(w, "assetIndex")),
            sc.version(),
            s(w, "version").to_string(),
        );
        d.eq(
            format!("{}: text", s(w, "assetIndex")),
            sc.file_text(),
            s(w, "text").to_string(),
        );
    }
    for w in arr(&g, "sidecarParses") {
        let text = s(w, "text");
        let got = Sidecar::parse(text);
        let want = w.get("parsed").unwrap();
        match (got, want) {
            (None, JsonValue::Null) => d.eq(format!("parse {text:?}"), true, true),
            (Some(sc), JsonValue::Object(_)) => {
                d.eq(
                    format!("parse {text:?}: assetIndex"),
                    sc.asset_index.as_str(),
                    s(want, "assetIndex"),
                );
                d.eq(
                    format!("parse {text:?}: format"),
                    sc.format,
                    i(want, "format"),
                );
                d.eq(
                    format!("parse {text:?}: programs"),
                    sc.programs.clone(),
                    want.get("programs").and_then(JsonValue::strings).unwrap(),
                );
                d.eq(
                    format!("parse {text:?}: sha1"),
                    sc.sha1
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>(),
                    want.get("sha1")
                        .and_then(JsonValue::as_object)
                        .unwrap()
                        .iter()
                        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
                        .collect::<Vec<_>>(),
                );
                d.eq(
                    format!("parse {text:?}: version"),
                    sc.version(),
                    s(want, "version").to_string(),
                );
            }
            (got, _) => d.eq(
                format!("parse {text:?}: parsed at all"),
                got.is_some(),
                !matches!(want, JsonValue::Null),
            ),
        }
    }
    d.finish("minecraft sidecar");
}

#[test]
fn the_asset_helpers_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "newest") {
        let ids = w.get("ids").and_then(JsonValue::strings).unwrap();
        d.eq(
            format!("newest({ids:?})"),
            newest(&ids).map(String::from),
            opt_s(w, "newest"),
        );
    }
    for w in arr(&g, "forbidden") {
        d.eq(
            format!("isForbidden({:?})", s(w, "sound")),
            is_forbidden(s(w, "sound")),
            flag(w, "forbidden"),
        );
    }
    for w in arr(&g, "sha1") {
        // the inputs are rebuilt as the dump builds them
        let len = i(w, "length") as usize;
        let bytes: Vec<u8> = match len {
            0 => vec![],
            3 => b"abc".to_vec(),
            43 => b"The quick brown fox jumps over the lazy dog".to_vec(),
            n => (0..n).map(|k| ((k * 7 + 3) & 255) as u8).collect(),
        };
        d.eq(
            format!("input {len} bytes"),
            bytes_sha(&bytes),
            s(w, "bytesSha256").to_string(),
        );
        d.eq(
            format!("sha1({len} bytes)"),
            sha1_hex(&bytes),
            s(w, "sha1").to_string(),
        );
    }
    for w in arr(&g, "objectPaths") {
        let a = MinecraftAssets::new(s(w, "root"), "1", BTreeMap::new(), BTreeMap::new());
        d.eq(
            format!("objectPath({:?}, {:?})", s(w, "root"), s(w, "hash")),
            a.object_path(s(w, "hash")),
            s(w, "path").to_string(),
        );
    }
    d.finish("minecraft asset helpers");
}

// MARK: the sample maths

#[test]
fn the_octave_zones_and_filter_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "octaveZones") {
        let root = i(w, "root");
        let got: Vec<[i64; 3]> = octave_zones(root)
            .iter()
            .map(|z| [*z.keys.start(), *z.keys.end(), z.shift])
            .collect();
        let want: Vec<[i64; 3]> = arr(w, "zones")
            .iter()
            .map(|z| {
                let a = z.as_array().unwrap();
                [
                    a[0].as_f64().unwrap() as i64,
                    a[1].as_f64().unwrap() as i64,
                    a[2].as_f64().unwrap() as i64,
                ]
            })
            .collect();
        d.eq(format!("octaveZones({root})"), got, want);
    }
    let want = arr(&g, "octaveFilter");
    d.eq("filter taps", octave_filter().len(), want.len());
    // Exact, but for two taps. Apple's `sin(+-24.12743157956961)` (the argument of taps 111 and
    // 143, t = -+16) is 0x3feb04bbff642e92; the correctly rounded value, which `libm` returns,
    // is ...e91 (checked against an 80-digit decimal evaluation). One ulp in two of 255
    // coefficients leaves no trace in the float32 output: every shift below, and both imported
    // banks, are bit-identical to Swift's.
    let mut inexact = Vec::new();
    for (k, (h, w)) in octave_filter().iter().zip(want).enumerate() {
        let got = h.to_bits();
        let swift = u64::from_str_radix(w.as_str().unwrap(), 16).unwrap();
        if got != swift {
            inexact.push(k);
            d.eq(
                format!("filter tap {k} within one ulp"),
                got.abs_diff(swift) <= 1,
                true,
            );
        }
    }
    d.eq("the taps that are not exact", inexact, vec![111, 143]);
    d.finish("minecraft octave zones and filter");
}

/// The input a golden row names (`mcShifts`, `mcEnergies`, ...).
fn input_named(name: &str) -> Vec<f32> {
    if name.starts_with("tri440") {
        return floats(&Synth::tri(440.0, 22_050).int16());
    }
    if let Some(len) = name
        .strip_prefix("noise ")
        .and_then(|n| n.parse::<usize>().ok())
    {
        return floats(&Synth::noise(len as u32 + 1, len, len.max(1)).int16());
    }
    match name {
        "hold 3 kHz" => floats(&Synth::tri_hold(3_000.0, 9_999, 800).int16()),
        "tri 15 kHz" => floats(&Synth::tri(15_000.0, 12_000).int16()),
        other => panic!("no input named {other}"),
    }
}

#[test]
fn the_octave_shifts_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "octaveShifts") {
        let name = s(w, "name");
        let octaves = i(w, "octaves");
        let tag = format!("{name} @{octaves:+}");
        let pcm = input_named(name);
        d.eq(
            format!("{tag}: input"),
            floats_sha(&pcm),
            s(w, "inSha").to_string(),
        );
        let (out, loop_) = octave_shift(&pcm, range_of(w.get("loop")), octaves);
        d.eq(format!("{tag}: count"), out.len(), i(w, "count") as usize);
        d.eq(
            format!("{tag}: output"),
            floats_sha(&out),
            s(w, "sha").to_string(),
        );
        d.eq(
            format!("{tag}: loop"),
            range_json(&loop_),
            range_json(&range_of(w.get("outLoop"))),
        );
        let peek = w.get("peek").unwrap();
        let heads: Vec<String> = out.iter().take(6).map(|x| bits32(*x)).collect();
        let want: Vec<String> = arr(peek, "head")
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        d.eq(
            format!("{tag}: head"),
            heads,
            want[..out.len().min(6)].to_vec(),
        );
    }
    d.finish("minecraft octave shifts");
}

#[test]
fn the_hit_energies_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "hitEnergy") {
        let pcm: Vec<f32> = match s(w, "name") {
            "silence" => vec![0.0; 100],
            "empty" => vec![],
            "tri" => floats(&Synth::tri(440.0, 22_050).int16()),
            "noise" => floats(&Synth::noise(5, 8_000, 8_000).int16()),
            "one" => vec![1.0],
            "tiny" => vec![1e-12, -1e-12],
            "full scale" => vec![1.0; 4_410],
            "loud" => vec![30_000.0; 3],
            other => panic!("no input named {other}"),
        };
        d.eq(
            format!("{}: count", s(w, "name")),
            pcm.len(),
            i(w, "count") as usize,
        );
        d.eq(
            format!("hitEnergy {}", s(w, "name")),
            bits_hex(hit_energy(&pcm)),
            s(w, "energy").to_string(),
        );
    }
    d.finish("minecraft hit energy");
}

#[test]
fn the_roots_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    for w in arr(&g, "measureRoot") {
        let name = s(w, "name");
        let pcm: Vec<f32> = match name {
            "harp +12c" => floats(&Synth::tri(372.5, 22_050).int16()),
            "a semitone up" => floats(&Synth::tri(392.0, 22_050).int16()),
            "a semitone down" => floats(&Synth::tri(350.0, 22_050).int16()),
            "bass" => floats(&Synth::tri(93.5, 22_050).int16()),
            "flute" => floats(&Synth::tri_hold(741.0, 22_050, 1_800).int16()),
            "bell -6c" => floats(&Synth::tri(1_475.0, 22_050).int16()),
            "silence" => vec![0.0; 2_000],
            "short" => floats(&Synth::tri(372.5, 900).int16()),
            "shorter" => floats(&Synth::tri(372.5, 400).int16()),
            "noise" => floats(&Synth::noise(3, 3_000, 3_000).int16()),
            "two octaves up" => floats(&Synth::tri(1_480.0, 22_050).int16()),
            "nominal 0" => floats(&Synth::tri(8.5, 3_000).int16()),
            other => panic!("no input named {other}"),
        };
        d.eq(
            format!("{name}: input"),
            floats_sha(&pcm),
            s(w, "inSha").to_string(),
        );
        let (root, cents) = measure_root(&pcm, i(w, "near"));
        d.eq(format!("{name}: root"), root, i(w, "root"));
        d.eq(
            format!("{name}: cents"),
            bits_hex(cents),
            s(w, "cents").to_string(),
        );
    }
    d.finish("minecraft measureRoot");
}

#[test]
fn the_crossfade_loops_match_swift() {
    let g = golden();
    let mut d = Diffs::default();
    let mut found = 0;
    for w in arr(&g, "crossfadeLoop") {
        let name = s(w, "name");
        let n = i(w, "n") as usize;
        let mut pcm = floats(&Synth::noise(n as u32 + 1, n, n.max(1)).int16());
        let before = pcm.clone();
        d.eq(
            format!("{name}: input"),
            floats_sha(&pcm),
            s(w, "inSha").to_string(),
        );
        let cents = f64::from_bits(u64::from_str_radix(s(w, "cents"), 16).unwrap());
        let loop_ = crossfade_loop(&mut pcm, i(w, "root"), cents);
        found += loop_.is_some() as usize;
        d.eq(
            format!("{name}: loop"),
            range_json(&loop_),
            range_json(&range_of(w.get("loop"))),
        );
        d.eq(
            format!("{name}: output"),
            floats_sha(&pcm),
            s(w, "sha").to_string(),
        );
        d.eq(
            format!("{name}: changed"),
            before.iter().zip(&pcm).filter(|(a, b)| a != b).count(),
            i(w, "changed") as usize,
        );
    }
    assert!(found >= 6, "most cases find a loop ({found})");
    d.finish("minecraft crossfadeLoop");
}

// MARK: the SoundFont writer

fn zone(
    keys: std::ops::RangeInclusive<i64>,
    sample: usize,
    fixed: bool,
    root: i64,
    looped: bool,
    release: f64,
    att: f64,
) -> Zone {
    Zone {
        keys,
        sample,
        fixed_pitch: fixed,
        root_key: root,
        looped,
        release_seconds: release,
        attenuation_db: att,
    }
}

/// The writers `mcWriters` builds, in its order.
fn writers() -> Vec<(&'static str, Sf2Writer, bool)> {
    let mut v = Vec::new();
    v.push(("empty", Sf2Writer::new("x"), true));
    v.push(("odd name", Sf2Writer::new("ab"), true));
    let mut one = Sf2Writer::new("minecraft-local");
    let a = one.add_sample(
        "tiny",
        &[0.0, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0, 0.25],
        60,
        0,
        None,
    );
    one.add_preset(
        "mc tiny",
        7,
        vec![zone(0..=127, a, true, 60, false, 8.0, 0.0)],
    );
    v.push(("one fixed-pitch sample", one, true));

    let mut tone = Sf2Writer::new("minecraft-local");
    let pluck = floats(&Synth::tri(440.0, 600).int16());
    let doubled: Vec<f32> = pluck.iter().chain(pluck.iter()).copied().collect();
    let down = tone.add_sample("harp2-1", &doubled, 54, 12, Some(100..500));
    let mid = tone.add_sample("harp2", &pluck, 66, -12, Some(50..250));
    let up = tone.add_sample("harp2+1", &pluck[..300], 78, 300, None);
    tone.add_preset(
        "mc harp2",
        0,
        vec![
            zone(0..=59, down, false, 54, true, 0.15, 0.0),
            zone(60..=72, mid, false, 66, true, 0.15, 3.04),
            zone(73..=127, up, false, 78, false, 1.2, 0.05),
        ],
    );
    tone.add_preset(
        "this preset name is far longer than twenty bytes",
        127,
        vec![zone(35..=36, up, true, 60, false, 2.0, 200.0)],
    );
    tone.add_preset(
        "é-in-the-twentieth-byte-é",
        5,
        vec![zone(35..=36, down, true, 255, false, 0.0001, 0.04)],
    );
    v.push(("a tone in three zones and two odd presets", tone, false));

    let mut empty = Sf2Writer::new("no presets");
    empty.add_sample("lonely", &[0.1, 0.2], 300, -300, Some(0..2));
    v.push(("a sample no preset uses", empty, true));

    let mut releases = Sf2Writer::new("releases");
    let r = releases.add_sample("r", &[0.5], 0, 127, None);
    releases.add_preset(
        "rel",
        1,
        vec![
            zone(0..=0, r, false, 60, false, 0.001, 0.0),
            zone(1..=1, r, false, 60, false, 0.5, 0.0),
            zone(2..=2, r, false, 60, false, 8.0, 0.0),
            zone(3..=3, r, false, 60, false, 100.0, 0.0),
            zone(4..=4, r, false, 60, false, 2.0, 143.96),
            zone(5..=5, r, false, 60, false, 0.0, 0.04),
        ],
    );
    v.push(("release times", releases, true));
    v
}

#[test]
fn the_soundfont_writer_matches_swift_byte_for_byte() {
    let g = golden();
    let rows = arr(&g, "sf2Writers");
    let ws = writers();
    let mut d = Diffs::default();
    assert_eq!(rows.len(), ws.len(), "the same writers");
    for (w, (name, writer, has_hex)) in rows.iter().zip(&ws) {
        d.eq(format!("{name}: name"), *name, s(w, "name"));
        let bytes = writer.data();
        d.eq(
            format!("{name}: length"),
            bytes.len(),
            i(w, "length") as usize,
        );
        // the layout first: a mismatch says which chunk
        let want: Vec<(String, usize, Option<String>)> = arr(w, "layout")
            .iter()
            .map(|c| {
                (
                    s(c, "path").to_string(),
                    i(c, "size") as usize,
                    opt_s(c, "sha"),
                )
            })
            .collect();
        d.eq(format!("{name}: layout"), layout(&bytes), want);
        d.eq(
            format!("{name}: sha"),
            bytes_sha(&bytes),
            s(w, "sha").to_string(),
        );
        if *has_hex {
            d.eq(
                format!("{name}: bytes"),
                hex_of(&bytes),
                s(w, "hex").to_string(),
            );
        }
    }
    d.finish("minecraft SF2 writer");
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// MARK: the assets tree and the import

const ROOT: &str = "/mem/assets";

fn tree_fs(g: &JsonValue) -> MemFs {
    MemFs::with(ROOT, &tree_files(g))
}

#[test]
fn the_synthetic_tree_is_the_trees_swift_built() {
    let g = golden();
    let mut d = Diffs::default();
    for (w, x) in arr(&g, "sounds").iter().zip(tree_sounds(&g)) {
        let samples = x.synth.int16();
        d.eq(
            format!("{}: int16 hash", x.sound),
            int16_sha(&samples),
            s(w, "int16Sha").to_string(),
        );
        let wav = mono_wav(&samples);
        d.eq(
            format!("{}: wav length", x.sound),
            wav.len(),
            i(w, "wavLength") as usize,
        );
        d.eq(
            format!("{}: wav sha1", x.sound),
            sha1_hex(&wav),
            s(w, "wavSha1").to_string(),
        );
    }
    let files = tree_files(&g);
    let want = arr(&g, "files");
    d.eq("file count", files.len(), want.len());
    for (w, (path, bytes)) in want.iter().zip(&files) {
        d.eq(format!("file {path}"), path.as_str(), s(w, "path"));
        d.eq(
            format!("file {path}: length"),
            bytes.len(),
            i(w, "length") as usize,
        );
        d.eq(
            format!("file {path}: sha1"),
            sha1_hex(bytes),
            s(w, "sha1").to_string(),
        );
    }
    d.finish("minecraft synthetic tree");
}

fn describe(r: Result<MinecraftAssets, Unavailable>, mask: &str) -> (bool, Option<bool>, String) {
    match r {
        Ok(a) => (
            true,
            None,
            format!("{} {} {}", a.index_id, a.object_count(), a.event_count()),
        ),
        Err(Unavailable::Unreadable(_)) => (false, Some(false), String::new()),
        Err(e) => (false, Some(true), e.to_string().replace(mask, "<root>")),
    }
}

#[test]
fn locating_the_assets_matches_swift() {
    let g = golden();
    let mut d = Diffs::default();
    let full = tree_fs(&g);
    let none = MemFs::default();
    let mut no_indexes = MemFs::default();
    no_indexes.folder("/mem/noindexes/objects");
    let mut no_sounds = MemFs::default();
    no_sounds.folder("/mem/nosounds/objects");
    let indexes = g.get("indexes").and_then(JsonValue::as_object).unwrap();
    no_sounds.0.insert(
        "/mem/nosounds/indexes/5.json".into(),
        indexes
            .get("5")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec(),
    );
    no_sounds.0.insert(
        "/mem/nosounds/indexes/7.json".into(),
        br#"{"objects":{"minecraft/other.json":{"hash":"00","size":1},"x":{"size":2}}}"#.to_vec(),
    );
    no_sounds.0.insert(
        "/mem/nosounds/indexes/readme.txt".into(),
        b"not an index".to_vec(),
    );
    let rows = arr(&g, "locate");
    let case = |name: &str| {
        rows.iter()
            .find(|r| s(r, "case") == name)
            .unwrap_or_else(|| panic!("no case {name}"))
            .get("result")
            .unwrap()
    };
    let check = |d: &mut Diffs, name: &str, got: (bool, Option<bool>, String)| {
        let want = case(name);
        d.eq(format!("{name}: ok"), got.0, flag(want, "ok"));
        if let Some(un) = got.1 {
            d.eq(
                format!("{name}: is an Unavailable"),
                un,
                flag(want, "unavailable"),
            );
        }
        if got.0 {
            let w = format!(
                "{} {} {}",
                s(want, "index"),
                i(want, "objects"),
                i(want, "events")
            );
            d.eq(format!("{name}: assets"), got.2, w);
        } else if got.1 == Some(true) {
            d.eq(
                format!("{name}: error"),
                got.2,
                s(want, "error").to_string(),
            );
        }
    };
    check(
        &mut d,
        "newest",
        describe(MinecraftAssets::locate(&full, ROOT, None), ROOT),
    );
    check(
        &mut d,
        "index 29",
        describe(MinecraftAssets::locate(&full, ROOT, Some("29")), ROOT),
    );
    check(
        &mut d,
        "index 5 (only sounds.json)",
        describe(MinecraftAssets::locate(&full, ROOT, Some("5")), ROOT),
    );
    check(
        &mut d,
        "index 31 (none)",
        describe(MinecraftAssets::locate(&full, ROOT, Some("31")), ROOT),
    );
    check(
        &mut d,
        "no such folder",
        describe(
            MinecraftAssets::locate(&none, "/nonexistent/assets", None),
            ROOT,
        ),
    );
    check(
        &mut d,
        "empty folder",
        describe(
            MinecraftAssets::locate(&none, "/mem/bare", None),
            "/mem/bare",
        ),
    );
    check(
        &mut d,
        "objects but no indexes",
        describe(
            MinecraftAssets::locate(&no_indexes, "/mem/noindexes", None),
            "/mem/noindexes",
        ),
    );
    check(
        &mut d,
        "sounds.json object not on disk",
        describe(
            MinecraftAssets::locate(&no_sounds, "/mem/nosounds", Some("5")),
            "/mem/nosounds",
        ),
    );
    check(
        &mut d,
        "index without sounds.json",
        describe(
            MinecraftAssets::locate(&no_sounds, "/mem/nosounds", Some("7")),
            "/mem/nosounds",
        ),
    );
    check(
        &mut d,
        "newest of two (7 > 5)",
        describe(
            MinecraftAssets::locate(&no_sounds, "/mem/nosounds", None),
            "/mem/nosounds",
        ),
    );
    check(
        &mut d,
        "trailing slash",
        describe(
            MinecraftAssets::locate(&full, &format!("{ROOT}/"), None),
            ROOT,
        ),
    );
    d.eq(
        "unavailableReason",
        MinecraftAssets::unavailable_reason(&none, "/nonexistent/assets"),
        opt_s(&g, "unavailableReason"),
    );
    d.eq(
        "unavailableReason of the tree",
        MinecraftAssets::unavailable_reason(&full, ROOT),
        None,
    );
    d.finish("minecraft locate");
}

#[test]
fn events_and_files_resolve_as_swift() {
    let g = golden();
    let mut d = Diffs::default();
    let fs = tree_fs(&g);
    let a = MinecraftAssets::locate(&fs, ROOT, Some("30")).unwrap();
    for w in arr(&g, "events") {
        d.eq(
            format!("sounds(event: {})", s(w, "event")),
            a.sounds(s(w, "event")),
            w.get("sounds").and_then(JsonValue::strings).unwrap(),
        );
    }
    for w in arr(&g, "files30") {
        let sound = s(w, "sound");
        match a.file(&fs, sound) {
            Ok((path, sha1)) => {
                d.eq(format!("file({sound}): ok"), true, flag(w, "ok"));
                d.eq(
                    format!("file({sound}): path"),
                    path,
                    format!("{ROOT}/{}", s(w, "path")),
                );
                d.eq(
                    format!("file({sound}): sha1"),
                    sha1,
                    s(w, "sha1").to_string(),
                );
            }
            Err(e) => {
                d.eq(format!("file({sound}): ok"), false, flag(w, "ok"));
                d.eq(
                    format!("file({sound}): error"),
                    e.to_string(),
                    opt_s(w, "error").unwrap_or_default(),
                );
            }
        }
    }
    let a29 = MinecraftAssets::locate(&fs, ROOT, Some("29")).unwrap();
    d.eq(
        "trumpet in 29",
        a29.file(&fs, "note/trumpet").is_ok(),
        flag(&g, "trumpetIn29"),
    );
    d.eq(
        "trumpet in 30",
        a.file(&fs, "note/trumpet").is_ok(),
        flag(&g, "trumpetIn30"),
    );
    d.finish("minecraft events and files");
}

fn decoder<'a>(fs: &'a MemFs) -> impl FnMut(&str) -> Result<Vec<f32>, String> + 'a {
    move |path: &str| decode_mono_wav(&fs.read(path).ok_or("no such object")?)
}

fn check_import(g: &JsonValue, key: &str, index: &str) {
    let fs = tree_fs(g);
    let w = g.get(key).unwrap();
    let assets = MinecraftAssets::locate(&fs, ROOT, Some(index)).unwrap();
    let mut dec = decoder(&fs);
    let built = build(&assets, &fs, &mut dec).expect("the import builds");
    let mut d = Diffs::default();
    let r = &built.report;
    d.eq(
        format!("{key}: assetIndex"),
        r.asset_index.as_str(),
        s(w, "assetIndex"),
    );
    d.eq(format!("{key}: bytes"), r.bytes, i(w, "bytes") as usize);
    d.eq(
        format!("{key}: skipped"),
        r.skipped.clone(),
        w.get("skipped").and_then(JsonValue::strings).unwrap(),
    );
    let want = arr(w, "programs");
    d.eq(
        format!("{key}: programs built"),
        r.programs.len(),
        want.len(),
    );
    for (p, x) in r.programs.iter().zip(want) {
        d.eq(format!("{key}: program"), p.name.as_str(), s(x, "name"));
        d.eq(
            format!("{key}: {} number", p.name),
            p.program,
            i(x, "program"),
        );
        d.eq(
            format!("{key}: {} cents", p.name),
            bits_hex(p.cents),
            s(x, "cents").to_string(),
        );
        d.eq(format!("{key}: {} loop", p.name), p.looped, flag(x, "loop"));
    }
    d.eq(
        format!("{key}: sidecar"),
        built.sidecar.file_text(),
        s(w, "sidecarText").to_string(),
    );
    // chunk by chunk first, then the whole
    let want_layout: Vec<(String, usize, Option<String>)> = arr(w, "layout")
        .iter()
        .map(|c| {
            (
                s(c, "path").to_string(),
                i(c, "size") as usize,
                opt_s(c, "sha"),
            )
        })
        .collect();
    d.eq(format!("{key}: layout"), layout(&built.sf2), want_layout);
    d.eq(
        format!("{key}: length"),
        built.sf2.len(),
        i(w, "length") as usize,
    );
    d.eq(
        format!("{key}: sha"),
        bytes_sha(&built.sf2),
        s(w, "sha").to_string(),
    );
    d.finish(&format!("minecraft import ({key})"));
}

#[test]
fn the_import_at_index_29_builds_swifts_bank() {
    // index 29 predates the copper trumpets: they are skipped for their minIndex
    check_import(&golden(), "import29", "29");
}

#[test]
fn the_import_at_index_30_builds_swifts_bank() {
    check_import(&golden(), "import30", "30");
}

#[test]
fn a_missing_kit_sound_stops_the_import_and_a_missing_tone_does_not() {
    let g = golden();
    let mut fs = tree_fs(&g);
    let assets = MinecraftAssets::locate(&fs, ROOT, Some("29")).unwrap();
    // delete the snare's object: the kit cannot be built
    let (snare_path, _) = assets.file(&fs, "note/snare").unwrap();
    fs.0.remove(&snare_path);
    let mut dec = decoder(&fs);
    match build(&assets, &fs, &mut dec) {
        Err(ImportError::Unavailable(Unavailable::MissingObject(p))) => {
            assert_eq!(p, "minecraft/sounds/note/snare.ogg");
        }
        other => panic!(
            "expected the snare to be missing, got {:?}",
            other.map(|b| b.report)
        ),
    }
    // a decode failure stops it too, with the host's reason
    let fs = tree_fs(&g);
    let mut failing = |_: &str| Err::<Vec<f32>, String>("cannot decode".into());
    match build(&assets, &fs, &mut failing) {
        Err(ImportError::Decode(why)) => assert_eq!(why, "cannot decode"),
        other => panic!(
            "expected a decode failure, got {:?}",
            other.map(|b| b.report)
        ),
    }
}
