//! Swift golden for `DrumKits`' pure parts and the project's drum accessors (`tools/paritydump`,
//! section "kits": `swift/kits.json`, which was an empty stub until now). `DrumKits.parse` over
//! good and bad ids, `isKitName`, the SoundFont `phdr` reader and `DrumKits.scan` over the very
//! files the dump wrote (their bytes are in the fixture), `brushPreset` over preset lists, and
//! `Project.drumChannels` / `drumKit` / `setDrumKit` on five specs. `voices.rs` holds the same
//! rules by the Swift tests' literals; this holds them to Swift's output.

mod common;
use common::*;
use djrust::json::JsonValue;
use djrust::voices::bank::{sf2_presets, BankPreset};
use djrust::voices::drumkits;
use djrust::TrackSpec;

fn hex_bytes(h: &str) -> Vec<u8> {
    unhex(h)
}

fn presets_of(v: &JsonValue) -> Vec<(String, i64, i64)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let p = p.as_array().unwrap();
            (
                p[0].as_str().unwrap().to_string(),
                p[1].as_f64().unwrap() as i64,
                p[2].as_f64().unwrap() as i64,
            )
        })
        .collect()
}

fn preset_tuples(p: &[BankPreset]) -> Vec<(String, i64, i64)> {
    p.iter().map(|p| (p.name.clone(), p.bank, p.program)).collect()
}

#[test]
fn kit_ids_parse_as_swift_parses_them() {
    let doc = fixture("kits");
    let mut d = Diffs::default();
    for r in rows(&doc, "parse") {
        let id = s(r, "id");
        let want = r.get("parsed").and_then(JsonValue::as_array).map(|a| {
            (
                a[0].as_str().unwrap().to_string(),
                a[1].as_f64().unwrap() as i64,
                a[2].as_f64().unwrap() as i64,
            )
        });
        d.eq(format!("parse({id:?})"), drumkits::parse(id), want);
    }
    d.finish("kit ids");
}

#[test]
fn kit_names_are_recognised_as_swift_does() {
    let doc = fixture("kits");
    let mut d = Diffs::default();
    for r in rows(&doc, "isKitName") {
        let name = s(r, "name");
        d.eq(
            format!("isKitName({name:?})"),
            drumkits::is_kit_name(name),
            r.get("isKit").and_then(JsonValue::as_bool).unwrap(),
        );
    }
    d.finish("kit names");
}

/// The `phdr` reader over each bank's bytes, `scan` over all of them in the order given and over
/// each alone.
#[test]
fn banks_and_scans_match_swift() {
    let doc = fixture("kits");
    let mut d = Diffs::default();
    let mut banks: Vec<(String, Vec<BankPreset>)> = Vec::new();
    for r in rows(&doc, "banks") {
        let file = s(r, "file");
        let presets = sf2_presets(&hex_bytes(s(r, "bytes")));
        d.eq(
            format!("{file} presets"),
            preset_tuples(&presets),
            presets_of(r.get("presets").unwrap()),
        );
        banks.push((format!("/sf/{file}"), presets));
    }
    let want = rows(&doc, "scan");
    let got = drumkits::scan(&banks);
    d.eq("scan count", got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        let tag = format!("scan {}", g.id);
        d.eq(format!("{tag} id"), g.id.as_str(), s(w, "id"));
        d.eq(format!("{tag} name"), g.name.as_str(), s(w, "name"));
        d.eq(format!("{tag} bank"), g.bank.as_str(), s(w, "bank"));
        d.eq(format!("{tag} label"), g.label(), s(w, "label").to_string());
        d.eq(format!("{tag} source"), g.source.to_string(), s(w, "source").to_string());
    }
    for (r, b) in rows(&doc, "scanEach").iter().zip(&banks) {
        let ids: Vec<String> = drumkits::scan(std::slice::from_ref(b))
            .into_iter()
            .map(|k| k.id)
            .collect();
        let want: Vec<String> = r.get("kits").unwrap().strings().unwrap();
        d.eq(format!("scan of {}", s(r, "file")), ids, want);
    }
    d.finish("banks and scans");
}

#[test]
fn the_brush_preset_matches_swift() {
    let doc = fixture("kits");
    let mut d = Diffs::default();
    for (n, r) in rows(&doc, "brush").iter().enumerate() {
        let presets: Vec<BankPreset> = presets_of(r.get("presets").unwrap())
            .into_iter()
            .map(|(name, bank, program)| BankPreset { name, bank, program })
            .collect();
        let got = drumkits::brush_preset(&presets).map(|p| (p.name.clone(), p.bank, p.program));
        let want = r.get("brush").and_then(JsonValue::as_array).map(|a| {
            (
                a[0].as_str().unwrap().to_string(),
                a[1].as_f64().unwrap() as i64,
                a[2].as_f64().unwrap() as i64,
            )
        });
        d.eq(format!("case {n}"), got, want);
    }
    d.finish("brush presets");
}

fn int_list(v: &JsonValue) -> Vec<i64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap() as i64)
        .collect()
}

fn net_kits(p: &djrust::pflow::Project) -> String {
    JsonValue::Array(
        p.nets
            .iter()
            .filter(|(_, nb)| nb.role != "control")
            .map(|(id, nb)| {
                JsonValue::Array(vec![
                    JsonValue::str(id),
                    JsonValue::int(nb.track.channel),
                    nb.track.kit.as_deref().map_or(JsonValue::Null, JsonValue::str),
                ])
            })
            .collect(),
    )
    .js_stringified()
}

#[test]
fn the_projects_drum_accessors_match_swift() {
    let doc = fixture("kits");
    let mut d = Diffs::default();
    for r in rows(&doc, "projects") {
        let mut p = TrackSpec::new(
            s(r, "genre"),
            f(r, "seed"),
            r.get("structure").and_then(JsonValue::as_str),
        )
        .project();
        let tag = format!("{} {}", s(r, "genre"), f(r, "seed"));
        let kit = |v: &JsonValue| v.as_str().map(String::from);
        d.eq(format!("{tag} channels"), p.drum_channels(), int_list(r.get("channels").unwrap()));
        d.eq(format!("{tag} kit before"), p.drum_kit(), kit(r.get("kitBefore").unwrap()));
        p.set_drum_kit(Some("MuseScore_General.sf2#128:32"));
        d.eq(format!("{tag} channels set"), p.drum_channels(), int_list(r.get("channelsSet").unwrap()));
        d.eq(format!("{tag} kit set"), p.drum_kit(), kit(r.get("kitSet").unwrap()));
        d.eq(
            format!("{tag} nets set"),
            net_kits(&p),
            r.get("netsSet").unwrap().js_stringified(),
        );
        p.set_drum_kit(None);
        d.eq(format!("{tag} kit after"), p.drum_kit(), kit(r.get("kitAfter").unwrap()));
        d.eq(
            format!("{tag} channels after"),
            p.drum_channels(),
            int_list(r.get("channelsAfter").unwrap()),
        );
        d.eq(format!("{tag} nets after"), net_kits(&p), r.get("netsAfter").unwrap().js_stringified());
    }
    d.finish("project drum accessors");
}
