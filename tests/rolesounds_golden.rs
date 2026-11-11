//! Swift golden for the fine-tune rows of a net deck (`Deck.roleSounds`, `RoleSoundRow.json`,
//! `RoleSoundChoice.selected`, `Deck.playingPalette`): `tools/paritydump/rolesounds.swift`,
//! section "rolesounds", `tests/fixtures/swift/rolesounds.json`.
//!
//! The dump drives a real `DJEngine(offline: true)` deck A at 24 kHz with the machine pinned to "no
//! SoundFont, no Minecraft bank, Apple's General MIDI bank present" and records the sampled
//! instruments its Mac names. Each scenario's ops (a track on a palette, role voices, palette and
//! kit pokes, "the first option that is not selected and can play") are replayed here through
//! [`Engine`] with the same machine, and after every op the palette the deck plays, the id as
//! recorded, every row as JSON (key order and all) and the options selected per role are compared
//! exactly: 3 genres x every palette, the PaletteDeckTests flows, and every role moved to another
//! option.
//!
//! Parity form (b) of the port rules.

mod common_fixture;
mod common_track;

use std::collections::BTreeMap;

use common_track::Stepper;
use djrust::json::{JsonObject, JsonValue};
use djrust::sound::Installed;
use djrust::voices::drumkits::DrumKit;
use djrust::voices::{CatalogSound, SamplerSource};
use djrust::wander::carry::with_track_field;
use djrust::TrackSpec;
use djrust::host::deck::{Engine, InstalledSounds, PreparedTrack};

fn golden() -> JsonValue {
    common_fixture::swift_fixture("rolesounds")
}

/// The machine the dump pinned: nothing installed but Apple's General MIDI bank, whose kits the
/// dump recorded (they name a kit option), and the game's assets as that Mac had them (present:
/// the Minecraft palette says "not imported yet"; absent: the assets' own error).
fn machine(g: &JsonValue) -> Installed {
    let kits = rows(g, "kits")
        .iter()
        .map(|k| DrumKit {
            id: text(k, "id").unwrap(),
            name: text(k, "name").unwrap(),
            bank: text(k, "bank").unwrap(),
            // a label reads the id, name and bank only
            source: SamplerSource::Gm(0),
        })
        .collect();
    Installed {
        dir: "/dj-soundfonts-none".into(),
        files: Vec::new(),
        gm_bank: true,
        kits,
        minecraft: None,
        minecraft_assets_missing: text(g, "minecraftAssetsMissing"),
    }
}

/// What the dump's Mac names as sampled instruments, by category (a palette's `cat:` option is
/// playable when its category has one, and its label names one of them).
fn catalog(g: &JsonValue) -> BTreeMap<String, Vec<CatalogSound>> {
    let mut out = BTreeMap::new();
    if let Some(JsonValue::Object(c)) = g.get("catalog") {
        for (cat, names) in c.iter() {
            let sounds = names
                .as_array()
                .unwrap()
                .iter()
                .filter_map(JsonValue::as_str)
                .map(|n| CatalogSound {
                    name: n.to_string(),
                    category: cat.to_string(),
                    sources: vec![SamplerSource::Exs(format!("/exs/{cat}/{n}.exs"))],
                })
                .collect();
            out.insert(cat.to_string(), sounds);
        }
    }
    out
}

fn rows<'a>(v: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    v.get(key)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no {key} in the fixture"))
}

fn text(op: &JsonValue, key: &str) -> Option<String> {
    op.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn opt(v: Option<String>) -> JsonValue {
    v.map_or(JsonValue::Null, JsonValue::String)
}

/// What the dump reads after every op.
fn state(e: &Engine) -> JsonValue {
    let rows = e.role_sounds("A");
    JsonValue::Object(JsonObject::from_pairs([
        ("palette", opt(e.playing_palette("A").map(|p| p.id.clone()))),
        ("recorded", opt(e.palette("A"))),
        (
            "rows",
            JsonValue::Array(rows.iter().map(|r| r.json()).collect()),
        ),
        (
            "selected",
            JsonValue::Object(JsonObject::from_pairs(rows.iter().map(|r| {
                (
                    r.role.clone(),
                    JsonValue::Array(
                        r.choices
                            .iter()
                            .filter(|c| c.selected)
                            .map(|c| JsonValue::str(c.option.as_str()))
                            .collect(),
                    ),
                )
            }))),
        ),
    ]))
}

/// One op, as the dump runs it; what it returns (`other`: the option it picked).
fn run(st: &mut Stepper, op: &JsonValue) -> JsonValue {
    let kind = text(op, "op").unwrap_or_default();
    match kind.as_str() {
        "net" => {
            let mut spec = TrackSpec::new(
                &text(op, "genre").unwrap(),
                op.get("seed").and_then(JsonValue::as_f64).unwrap(),
                op.get("structure").and_then(JsonValue::as_str),
            );
            let palette = text(op, "palette");
            spec =
                st.e.with_sounds(|ctx| spec.with_palette(palette.as_deref(), ctx.installed));
            // an instrument the spec's `tracks` picks (the older way: `track.picked`)
            if let Some(pick) = op.get("instrument") {
                let channel = pick.get("channel").and_then(JsonValue::as_i64).unwrap();
                with_track_field(
                    &mut spec,
                    "instrument",
                    pick.get("name").and_then(JsonValue::as_str),
                    &[channel],
                );
            }
            let pr = st.e.preparer("A").unwrap();
            let track = PreparedTrack::compose(&spec, &pr, 0);
            assert!(st.e.load_net("A", track));
        }
        "roleVoice" => st.e.set_role_voice(
            "A",
            &text(op, "role").unwrap(),
            text(op, "voice").as_deref(),
        ),
        "palette" => st.e.set_palette("A", text(op, "palette").as_deref()),
        "kit" => st.e.set_kit("A", text(op, "kit").as_deref()),
        "other" => {
            let role = text(op, "role").unwrap();
            let rows = st.e.role_sounds("A");
            let pick = rows.iter().find(|r| r.role == role).and_then(|r| {
                r.choices
                    .iter()
                    .find(|c| !c.selected && c.unavailable.is_none())
            });
            let Some(pick) = pick else {
                return JsonValue::Null;
            };
            let option = pick.option.clone();
            st.e.set_role_voice("A", &role, Some(&option));
            return JsonValue::String(option);
        }
        other => panic!("unknown op {other}"),
    }
    JsonValue::Null
}

/// The first place two JSON values differ, as a path and both sides.
fn first_diff(path: &str, want: &JsonValue, got: &JsonValue) -> Option<String> {
    match (want, got) {
        (JsonValue::Object(w), JsonValue::Object(g)) => {
            let wk: Vec<&str> = w.keys().collect();
            let gk: Vec<&str> = g.keys().collect();
            if wk != gk {
                return Some(format!("{path}: keys {gk:?}, want {wk:?}"));
            }
            for (k, wv) in w.iter() {
                if let Some(d) = first_diff(&format!("{path}.{k}"), wv, g.get(k).unwrap()) {
                    return Some(d);
                }
            }
            None
        }
        (JsonValue::Array(w), JsonValue::Array(g)) => {
            if w.len() != g.len() {
                return Some(format!("{path}: {} items, want {}", g.len(), w.len()));
            }
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                if let Some(d) = first_diff(&format!("{path}[{i}]"), wv, gv) {
                    return Some(d);
                }
            }
            None
        }
        _ if want.js_stringified() == got.js_stringified() => None,
        _ => Some(format!(
            "{path}: got {}, want {}",
            got.js_stringified(),
            want.js_stringified()
        )),
    }
}

/// One scenario replayed on an engine of its own: the states compared and the differences found.
fn check_scenario(g: &JsonValue, sc: &JsonValue) -> (usize, Vec<String>) {
    let name = sc.get("name").and_then(JsonValue::as_str).unwrap();
    let mut st = Stepper::new(24_000.0, true);
    st.e.deterministic = true;
    st.e.installed = Some(InstalledSounds::with_catalog(machine(g), catalog(g)));
    let mut checks = 0;
    let mut diffs = Vec::new();
    for (i, step) in rows(sc, "steps").iter().enumerate() {
        let op = step.get("op").unwrap();
        let result = if *op == JsonValue::Null {
            JsonValue::Null
        } else {
            run(&mut st, op)
        };
        checks += 1;
        let tag = format!("{name} #{i}");
        let want_result = step.get("result").unwrap();
        diffs.extend(first_diff(&format!("{tag} result"), want_result, &result));
        let want_state = step.get("state").unwrap();
        diffs.extend(first_diff(
            &format!("{tag} state"),
            want_state,
            &state(&st.e),
        ));
    }
    (checks, diffs)
}

#[test]
fn the_rows_match_swift_op_by_op() {
    let g = golden();
    let scenarios = rows(&g, "scenarios");
    // each scenario is an engine of its own: spread them over the cores (composing a track is
    // most of the time)
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    let results: Vec<(usize, Vec<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let g = &g;
                scope.spawn(move || {
                    scenarios
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i % threads == t)
                        .map(|(_, sc)| check_scenario(g, sc))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let checks: usize = results.iter().map(|r| r.0).sum();
    let mut diffs: Vec<String> = results.into_iter().flat_map(|r| r.1).collect();
    diffs.sort();
    assert!(checks > 0, "nothing was checked");
    assert!(
        diffs.is_empty(),
        "{} of {checks} states differ from Swift; first:\n  {}",
        diffs.len(),
        diffs
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    eprintln!("rolesounds: {checks} deck states match Swift");
}
