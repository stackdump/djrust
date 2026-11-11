//! Swift goldens for the walk and the auto-mix rule (`tools/paritydump`, section "wander",
//! `tests/fixtures/swift/wander.json`).
//!
//! The dump owns the contexts: every case carries its `ctx`, which is rebuilt here, so a plan that
//! differs is the planner and not two definitions drifting apart (the share CID of the rebuilt spec
//! is checked first). What a Mac has installed decides which sound palettes a hop can reach, so the
//! dump records its palette table and the planner is run over that table ([`palettes_for`]); a
//! separate test checks [`WanderPalettes::from_sounds`] and the bundled palettes against it.
//! `tests/wander.rs` and `tests/automix.rs` hold the Swift suites' own literals.

mod common;
use common::*;
use djrust::automix::{
    net_descriptor_json, AutoMix, AutoMixHandoff, AutoMixPlan, AutoMixSettings, AutoMixState,
};
use djrust::cid::share_cid;
use djrust::gen::tables::BeatsTables;
use djrust::json::{JsonObject, JsonValue};
use djrust::sound::{Installed, MinecraftSidecar, PaletteSet, Sounds};
use djrust::voices::VoiceCatalog;
use djrust::wander::carry::spec_palette;
use djrust::wander::*;
use djrust::TrackSpec;

fn golden() -> JsonValue {
    fixture("wander")
}

// MARK: building the dump's contexts

fn strings(v: &JsonValue, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(JsonValue::strings)
        .unwrap_or_else(|| panic!("no strings {key}"))
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn opt_str(v: &JsonValue, key: &str) -> Option<String> {
    v.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn loop_of(v: &JsonValue) -> LayerLoop {
    LayerLoop::new(s(v, "path"), s(v, "name"), s(v, "category"), i(v, "beats"))
}

/// A `WanderContext` from the dump's `ctx` (`ctxJSON` in `tools/paritydump/main.swift`).
fn context_of(v: &JsonValue) -> WanderContext {
    let sp = v.get("spec").expect("ctx.spec");
    let spec = TrackSpec::from_payload(sp.get("share").expect("spec.share"))
        .expect("a spec payload")
        .with_part_seeds(sp.get("partSeeds"));
    let roles = strings(v, "roles");
    let roles_on = strings(v, "rolesOn");
    let locks = strings(v, "locks");
    let voices: Vec<WanderVoice> = rows(v, "voices")
        .iter()
        .map(|x| WanderVoice {
            roles: strings(x, "roles"),
            channel: i(x, "channel"),
            instrument: s(x, "instrument").to_string(),
            options: strings(x, "options"),
            overridden: x.get("overridden").and_then(JsonValue::as_bool).unwrap(),
            option: opt_str(x, "option"),
        })
        .collect();
    let mut c = WanderContext::new(
        s(v, "deck"),
        spec,
        v.get("isLoop").and_then(JsonValue::as_bool).unwrap(),
        &refs(&roles),
        &refs(&roles_on),
        &refs(&locks),
        voices,
    );
    c.disabled_roles = strings(v, "disabledRoles").into_iter().collect();
    c.stems_muted = strings(v, "stemsMuted").into_iter().collect();
    c.locked_actions = strings(v, "lockedActions").into_iter().collect();
    c.anchor_bpm = v.get("anchorBPM").and_then(JsonValue::as_f64);
    c.key = v
        .get("key")
        .filter(|k| k.as_object().is_some())
        .map(|k| WanderKey {
            scale: s(k, "scale").to_string(),
            root_pc: i(k, "rootPC"),
            offset: i(k, "offset"),
        });
    c.palette = opt_str(v, "palette");
    c.kit = opt_str(v, "kit");
    c.channels = rows(v, "channels")
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    c.stingers = strings(v, "stingers");
    c.mix_bpm = v.get("mixBPM").and_then(JsonValue::as_f64);
    c.raw_base = opt_str(v, "rawBase").map(|h| u64::from_str_radix(&h, 16).unwrap());
    c.layers = rows(v, "layers")
        .iter()
        .map(|e| LayerEntry {
            loop_: loop_of(e),
            start: i(e, "start"),
            length: i(e, "length"),
            gain_db: f(e, "gainDB"),
            key_lock: e.get("keyLock").and_then(JsonValue::as_bool).unwrap(),
        })
        .collect();
    c.layer_pool = rows(v, "layerPool").iter().map(loop_of).collect();
    c.bar = i(v, "bar");
    c.pace = i(v, "pace");
    c
}

/// The palettes a hop from `palette` can reach, as the dump's Mac saw them
/// (`SoundPalette.playing(palette)` and its playable neighbours).
fn palettes_for(table: &[JsonValue], palette: Option<&str>) -> Option<WanderPalettes> {
    let here = PaletteSet::shared().playing(palette)?;
    let row = table
        .iter()
        .find(|r| s(r, "id") == here.id)
        .unwrap_or_else(|| panic!("the dump has no palette {}", here.id));
    Some(WanderPalettes {
        here_label: s(row, "label").to_string(),
        options: rows(row, "options")
            .iter()
            .map(|o| PaletteHop {
                id: s(o, "id").to_string(),
                label: s(o, "label").to_string(),
                normalized: opt_str(o, "normalized"),
            })
            .collect(),
    })
}

/// The context with the palettes its deck's palette can hop to.
fn planned(c: &WanderContext, table: &[JsonValue]) -> WanderContext {
    let mut c = c.clone();
    c.palettes = palettes_for(table, c.palette.as_deref());
    c
}

// MARK: comparing plans

fn spec_json(sp: &TrackSpec) -> JsonValue {
    JsonObject::from_pairs([
        ("genre", JsonValue::str(sp.genre.as_str())),
        ("seed", JsonValue::Number(sp.seed)),
        (
            "structure",
            sp.structure
                .as_deref()
                .map_or(JsonValue::Null, JsonValue::str),
        ),
        (
            "tracks",
            sp.layers
                .get("tracks")
                .map_or(JsonValue::Null, |t| JsonValue::String(t.js_stringified())),
        ),
        (
            "keyOffset",
            sp.extra
                .get("keyOffset")
                .cloned()
                .unwrap_or(JsonValue::Null),
        ),
        ("partSeeds", sp.part_seeds_json().unwrap_or(JsonValue::Null)),
        ("cid", JsonValue::String(share_cid(&sp.payload()))),
    ])
    .into()
}

fn entry_json(e: &LayerEntry) -> JsonValue {
    JsonObject::from_pairs([
        ("path", JsonValue::str(e.loop_.path.as_str())),
        ("name", JsonValue::str(e.loop_.name.as_str())),
        ("category", JsonValue::str(e.loop_.category.as_str())),
        ("beats", JsonValue::int(e.loop_.beats)),
        ("start", JsonValue::int(e.start)),
        ("length", JsonValue::int(e.length)),
        ("gainDB", JsonValue::Number(e.gain_db)),
        ("keyLock", JsonValue::Bool(e.key_lock)),
    ])
    .into()
}

/// The plan's action as the dump writes it (`actionJSON`).
fn action_json(a: &WanderAction) -> JsonValue {
    fn one(k: &str, v: JsonValue) -> JsonValue {
        JsonObject::from_pairs([(k, v)]).into()
    }
    match a {
        WanderAction::Role(r, on) => JsonObject::from_pairs([
            ("role", JsonValue::str(r.as_str())),
            ("on", JsonValue::Bool(*on)),
        ])
        .into(),
        WanderAction::Stem(x) => one("stem", JsonValue::str(x.as_str())),
        WanderAction::NetMacro(m) => one("netMacro", JsonValue::str(m.id())),
        WanderAction::MasterMacro(m) => one("masterMacro", JsonValue::str(m.id())),
        WanderAction::Stinger(n) => one("stinger", JsonValue::int(*n)),
        WanderAction::Swap(sp) => one("swap", spec_json(sp)),
        WanderAction::Palette(id, pins) => JsonObject::from_pairs([
            ("palette", JsonValue::str(id.as_str())),
            (
                "pins",
                JsonValue::Array(
                    pins.iter()
                        .map(|p| {
                            JsonObject::from_pairs([
                                ("role", JsonValue::str(p.role.as_str())),
                                ("option", JsonValue::str(p.option.as_str())),
                            ])
                            .into()
                        })
                        .collect(),
                ),
            ),
        ])
        .into(),
        WanderAction::Layers(l) => one(
            "layers",
            JsonValue::Array(l.iter().map(entry_json).collect()),
        ),
    }
}

/// Compare two JSON values leaf by leaf down objects, so a diff names the field.
fn cmp(d: &mut Diffs, tag: String, got: &JsonValue, want: &JsonValue) {
    match (got, want) {
        (JsonValue::Object(g), JsonValue::Object(w)) => {
            let mut keys: Vec<String> = w.iter().map(|(k, _)| k.to_string()).collect();
            for (k, _) in g.iter() {
                if !keys.iter().any(|x| x == k) {
                    keys.push(k.to_string());
                }
            }
            for k in keys {
                let null = JsonValue::Null;
                cmp(
                    d,
                    format!("{tag} {k}"),
                    g.get(&k).unwrap_or(&null),
                    w.get(&k).unwrap_or(&null),
                );
            }
        }
        _ => d.eq(tag, got.js_stringified(), want.js_stringified()),
    }
}

/// One planned step against the dump's row (`planJSON`).
fn check_plan(d: &mut Diffs, tag: &str, got: Option<WanderPlan>, want: &JsonValue) {
    let step = i(want, "step");
    let tag = format!("{tag} {step}");
    if want.get("none").is_some() {
        d.eq(format!("{tag} none"), got.is_none(), true);
        return;
    }
    let Some(got) = got else {
        d.eq(tag, "none".to_string(), "a plan".to_string());
        return;
    };
    d.eq(
        format!("{tag} kind"),
        got.kind.raw().to_string(),
        s(want, "kind").to_string(),
    );
    d.eq(
        format!("{tag} detail"),
        got.detail.clone(),
        s(want, "detail").to_string(),
    );
    cmp(
        d,
        format!("{tag} action"),
        &action_json(&got.action),
        want.get("action").unwrap(),
    );
}

fn auto_mix_rows(
    d: &mut Diffs,
    tag: &str,
    c: &WanderContext,
    key: Option<&WanderKey>,
    rows_: &[JsonValue],
) {
    for m in rows_ {
        let n = i(m, "n");
        let got = AutoMix::plan(c, n, key);
        if m.get("none").is_some() {
            d.eq(format!("{tag} {n} none"), got.is_none(), true);
            continue;
        }
        let Some(got) = got else {
            d.eq(
                format!("{tag} {n}"),
                "none".to_string(),
                "a plan".to_string(),
            );
            continue;
        };
        d.eq(
            format!("{tag} {n} kind"),
            got.kind.clone(),
            s(m, "kind").to_string(),
        );
        d.eq(
            format!("{tag} {n} detail"),
            got.detail.clone(),
            s(m, "detail").to_string(),
        );
        d.eq(
            format!("{tag} {n} from"),
            got.from.clone(),
            opt_str(m, "from"),
        );
        d.eq(
            format!("{tag} {n} cid"),
            got.spec.as_ref().map(|x| share_cid(&x.payload())),
            opt_str(m, "cid"),
        );
    }
}

/// Swift's `walkJSON`: each move changes the context the next is drawn from.
fn check_walk(
    d: &mut Diffs,
    name: &str,
    start: &WanderContext,
    table: &[JsonValue],
    walk: &[JsonValue],
) {
    let mut c = start.clone();
    for (step, want) in walk.iter().enumerate() {
        let step = step as i64;
        c.bar = start.bar + step * c.pace;
        let plan = Wander::plan(&planned(&c, table), step);
        let Some(p) = plan.clone() else {
            check_plan(d, &format!("{name} walk"), None, want);
            continue;
        };
        check_plan(d, &format!("{name} walk"), plan, want);
        match &p.action {
            WanderAction::Role(r, on) => {
                if *on {
                    c.roles_on.insert(r.clone());
                } else {
                    c.roles_on.remove(r);
                }
            }
            WanderAction::Stem(x) => {
                if !c.stems_muted.remove(x) {
                    c.stems_muted.insert(x.clone());
                }
            }
            WanderAction::Swap(sp) => {
                c.spec = sp.clone();
                if p.kind == WanderMoveKind::Palette {
                    c.palette = spec_palette(sp);
                }
            }
            WanderAction::Layers(l) => c.layers = l.clone(),
            _ => {}
        }
        // the log line the deck keeps (`Deck.takeWanderMove`)
        let (spec, cid) = match &p.action {
            WanderAction::Swap(sp) => (Some(sp.clone()), Some(share_cid(&sp.payload()))),
            _ if c.is_raw() => (None, None),
            _ => (Some(c.spec.clone()), None),
        };
        let mv = WanderMove {
            step,
            bar: c.bar,
            kind: p.kind,
            detail: p.detail.clone(),
            spec,
            cid,
        };
        d.eq(
            format!("{name} walk {step} move"),
            mv.json().js_stringified(),
            s(want, "move").to_string(),
        );
    }
}

fn root_key(g: &djrust::gen::tables::GenreDef, shift: i64) -> WanderKey {
    WanderKey {
        scale: g.scale_name.clone(),
        root_pc: (g.root_note + shift) % 12,
        offset: 0,
    }
}

#[test]
fn the_walk_and_the_auto_mix_rule_match_swift() {
    let doc = golden();
    let table = rows(&doc, "palettes");
    let mut d = Diffs::default();
    for case in rows(&doc, "cases") {
        let name = s(case, "name");
        let c = context_of(case.get("ctx").unwrap());
        d.eq(
            format!("{name} specCid"),
            share_cid(&c.spec.payload()),
            s(case, "specCid").to_string(),
        );
        let pc = planned(&c, table);
        for p in rows(case, "plans").iter().chain(rows(case, "finds")) {
            check_plan(
                &mut d,
                &format!("{name} wander"),
                Wander::plan(&pc, i(p, "step")),
                p,
            );
        }
        if let Some(mixes) = case.get("automix").and_then(JsonValue::as_array) {
            let g = BeatsTables::shared().genres.get(&c.spec.genre);
            // the auto-mix draw uses the playing deck's own key (its genre's root, offset 0)
            let key = g.map(|g| root_key(g, 0));
            auto_mix_rows(&mut d, &format!("{name} automix"), &c, key.as_ref(), mixes);
            if let Some(m) = case.get("automixFifth").and_then(JsonValue::as_array) {
                let fifth = g.map(|g| root_key(g, 7));
                auto_mix_rows(
                    &mut d,
                    &format!("{name} automixFifth"),
                    &c,
                    fifth.as_ref(),
                    m,
                );
            }
            if let Some(m) = case.get("automixNoKey").and_then(JsonValue::as_array) {
                auto_mix_rows(&mut d, &format!("{name} automixNoKey"), &c, None, m);
            }
        } else {
            d.eq(format!("{name} raw"), c.is_raw(), true);
        }
        d.eq(
            format!("{name} canMove"),
            Wander::can_move(&pc),
            case.get("canMove").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(
            format!("{name} canDraw"),
            AutoMix::can_draw(&c),
            case.get("canDraw").and_then(JsonValue::as_bool).unwrap(),
        );
        d.eq(
            format!("{name} neighbours"),
            Wander::neighbours(&c.spec.genre),
            strings(case, "neighbours"),
        );
        if let Some(walk) = case.get("walk").and_then(JsonValue::as_array) {
            check_walk(&mut d, name, &c, table, walk);
        }
    }
    d.finish("wander golden");
}

#[test]
fn every_genre_walks_as_swift_does() {
    let doc = golden();
    let table = rows(&doc, "palettes");
    let mut d = Diffs::default();
    let ids: Vec<String> = rows(&doc, "genres")
        .iter()
        .map(|g| s(g, "genre").to_string())
        .collect();
    let mut want_ids = BeatsTables::shared().beats_genre_ids();
    want_ids.sort();
    d.eq("genres".to_string(), ids, want_ids);
    let mut plain_genre_hops = 0;
    for g in rows(&doc, "genres") {
        let id = s(g, "genre");
        let c = context_of(g.get("ctx").unwrap());
        d.eq(format!("{id} bpm"), Wander::bpm(&c.spec), f(g, "bpm"));
        d.eq(
            format!("{id} neighbours"),
            Wander::neighbours(id),
            strings(g, "neighbours"),
        );
        let pc = planned(&c, table);
        for p in rows(g, "plans") {
            check_plan(
                &mut d,
                &format!("{id} wander"),
                Wander::plan(&pc, i(p, "step")),
                p,
            );
        }
        // plain Wander: no mix tempo (`Deck.wanderContext` sets none), so a genre hop is not held
        // to an incoming deck's pitch range
        let mut plain = pc.clone();
        plain.mix_bpm = None;
        for p in rows(g, "plainPlans").iter().chain(rows(g, "plainGenre")) {
            check_plan(
                &mut d,
                &format!("{id} plain wander"),
                Wander::plan(&plain, i(p, "step")),
                p,
            );
        }
        plain_genre_hops += rows(g, "plainGenre").len();
    }
    assert!(plain_genre_hops >= 30, "{plain_genre_hops} plain genre hops");
    d.finish("wander genres golden");
}

#[test]
fn the_draw_matches_swifts_splitmix() {
    let doc = golden();
    let mut d = Diffs::default();
    for r in rows(&doc, "rng") {
        let cid = s(r, "cid");
        let step = i(r, "step");
        let spec = TrackSpec::from_payload(r.get("spec").unwrap()).unwrap();
        d.eq(
            format!("{cid} spec"),
            share_cid(&spec.payload()),
            cid.to_string(),
        );
        d.eq(
            format!("{cid} base"),
            format!("{:016x}", WanderRandom::base(&spec)),
            s(r, "base").to_string(),
        );
        let mut a = WanderRandom::new(&spec, step);
        let mut b = WanderRandom::new(&spec, step);
        let next: Vec<String> = (0..6).map(|_| format!("{:016x}", a.next())).collect();
        d.eq(format!("{cid} {step} next"), next, strings(r, "next"));
        let ints: Vec<i64> = [7, 2, 999_999, 3, 1, 0, 13]
            .iter()
            .map(|&n| b.int(n))
            .collect();
        let want: Vec<i64> = rows(r, "ints")
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect();
        d.eq(format!("{cid} {step} ints"), ints, want);
    }
    for r in rows(&doc, "rawBases") {
        d.eq(
            format!("raw base of {:?}", s(r, "cid")),
            format!("{:016x}", WanderRandom::base_of_cid(s(r, "cid"))),
            s(r, "base").to_string(),
        );
    }
    d.finish("wander rng golden");
}

// MARK: tables, constants and codecs

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn the_tables_and_constants_match_swift() {
    let doc = golden();
    let k = doc.get("constants").unwrap();
    let mut d = Diffs::default();
    let kinds: Vec<String> = WanderMoveKind::ALL
        .iter()
        .map(|x| x.raw().to_string())
        .collect();
    d.eq("kinds".to_string(), kinds, strings(k, "kinds"));
    d.eq(
        "weights".to_string(),
        WanderMoveKind::ALL
            .iter()
            .map(|x| x.weight())
            .collect::<Vec<_>>(),
        rows(k, "weights")
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect(),
    );
    d.eq(
        "locks".to_string(),
        WanderMoveKind::ALL
            .iter()
            .map(|x| x.lock().map(String::from))
            .collect::<Vec<_>>(),
        rows(k, "locks")
            .iter()
            .map(|x| x.as_str().map(String::from))
            .collect(),
    );
    d.eq(
        "swaps".to_string(),
        WanderMoveKind::ALL
            .iter()
            .filter(|x| x.swaps())
            .map(|x| x.raw().to_string())
            .collect::<Vec<_>>(),
        strings(k, "swaps"),
    );
    d.eq(
        "dimensions".to_string(),
        WanderLock::DIMENSIONS
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>(),
        strings(k, "dimensions"),
    );
    d.eq(
        "keepPrefix".to_string(),
        WanderLock::KEEP_PREFIX.to_string(),
        s(k, "keepPrefix").to_string(),
    );
    d.eq(
        "tempoTolerance".to_string(),
        WanderLock::TEMPO_TOLERANCE,
        f(k, "tempoTolerance"),
    );
    d.eq(
        "paces".to_string(),
        WanderState::PACES.to_vec(),
        rows(k, "paces")
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect(),
    );
    d.eq(
        "defaultPace".to_string(),
        WanderState::DEFAULT_PACE,
        i(k, "defaultPace"),
    );
    d.eq(
        "logLimit".to_string(),
        WanderState::LOG_LIMIT as i64,
        i(k, "logLimit"),
    );
    d.eq(
        "tonalRoles".to_string(),
        sorted(Wander::TONAL_ROLES.iter().map(|x| x.to_string()).collect()),
        strings(k, "tonalRoles"),
    );
    d.eq(
        "rawKinds".to_string(),
        sorted(
            Wander::RAW_KINDS
                .iter()
                .map(|x| x.raw().to_string())
                .collect(),
        ),
        strings(k, "rawKinds"),
    );
    d.eq(
        "maxLayers".to_string(),
        Wander::MAX_LAYERS as i64,
        i(k, "maxLayers"),
    );
    d.eq(
        "netMacros".to_string(),
        Wander::NET_MACROS
            .iter()
            .map(|m| m.id().to_string())
            .collect::<Vec<_>>(),
        strings(k, "netMacros"),
    );
    d.eq(
        "netMacroLabels".to_string(),
        Wander::NET_MACROS
            .iter()
            .map(|m| m.label().to_string())
            .collect::<Vec<_>>(),
        strings(k, "netMacroLabels"),
    );
    d.eq(
        "masterMacros".to_string(),
        Wander::MASTER_MACROS
            .iter()
            .map(|m| m.id().to_string())
            .collect::<Vec<_>>(),
        strings(k, "masterMacros"),
    );
    d.eq(
        "masterMacroLabels".to_string(),
        Wander::MASTER_MACROS
            .iter()
            .map(|m| m.label().to_string())
            .collect::<Vec<_>>(),
        strings(k, "masterMacroLabels"),
    );
    d.eq(
        "structures".to_string(),
        Wander::STRUCTURES
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>(),
        strings(k, "structures"),
    );
    let stems = k.get("stems").unwrap();
    for (name, group) in Wander::STEMS {
        d.eq(
            format!("stems {name}"),
            group.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
            strings(stems, name),
        );
    }
    d.eq(
        "stems count".to_string(),
        Wander::STEMS.len(),
        stems.as_object().unwrap().len(),
    );
    d.eq(
        "seedableParts".to_string(),
        djrust::wander::SEEDABLE_PARTS
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>(),
        strings(k, "seedableParts"),
    );
    for (dim, want) in k.get("labels").unwrap().as_object().unwrap().iter() {
        d.eq(
            format!("label {dim}"),
            WanderLock::label(dim),
            want.as_str().unwrap().to_string(),
        );
    }
    for (dim, want) in k.get("valid").unwrap().as_object().unwrap().iter() {
        d.eq(
            format!("valid {dim:?}"),
            WanderLock::is_valid(dim),
            want.as_bool().unwrap(),
        );
    }
    for (cat, want) in k.get("defaultGain").unwrap().as_object().unwrap().iter() {
        d.eq(
            format!("default gain {cat:?}"),
            default_gain(cat),
            want.as_f64().unwrap(),
        );
    }
    let a = k.get("automix").unwrap();
    d.eq(
        "phraseBars".to_string(),
        AutoMix::PHRASE_BARS,
        i(a, "phraseBars"),
    );
    d.eq("prepBars".to_string(), AutoMix::PREP_BARS, i(a, "prepBars"));
    d.eq(
        "nowPrepBars".to_string(),
        AutoMix::NOW_PREP_BARS,
        i(a, "nowPrepBars"),
    );
    d.eq(
        "quietBars".to_string(),
        AutoMix::QUIET_BARS,
        i(a, "quietBars"),
    );
    d.eq(
        "loopPhrases".to_string(),
        AutoMix::LOOP_PHRASES,
        i(a, "loopPhrases"),
    );
    d.eq(
        "salt".to_string(),
        format!("{:016x}", AutoMix::SALT),
        s(a, "salt").to_string(),
    );
    d.eq(
        "maxDipLU".to_string(),
        AutoMix::MAX_DIP_LU,
        f(a, "maxDipLU"),
    );
    d.eq(
        "maxDipSource".to_string(),
        AutoMix::MAX_DIP_SOURCE.to_string(),
        s(a, "maxDipSource").to_string(),
    );
    d.eq(
        "automix kinds".to_string(),
        AutoMix::KINDS
            .iter()
            .map(|x| x.raw().to_string())
            .collect::<Vec<_>>(),
        strings(a, "kinds"),
    );
    d.eq(
        "pitchNames".to_string(),
        AutoMix::PITCH_NAMES
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>(),
        strings(a, "pitchNames"),
    );
    d.eq(
        "lengths".to_string(),
        AutoMixSettings::LENGTHS.to_vec(),
        rows(a, "lengths")
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect(),
    );
    d.eq(
        "everies".to_string(),
        AutoMixSettings::EVERIES.to_vec(),
        rows(a, "everies")
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect(),
    );
    d.eq(
        "defaultBars".to_string(),
        AutoMixSettings::DEFAULT_BARS,
        i(a, "defaultBars"),
    );
    d.eq(
        "autoMix logLimit".to_string(),
        AutoMixState::LOG_LIMIT as i64,
        i(a, "logLimit"),
    );
    d.finish("wander constants golden");
}

#[test]
fn the_state_and_key_codecs_match_swift() {
    let doc = golden();
    let mut d = Diffs::default();
    for r in rows(&doc, "states") {
        let input = s(r, "input");
        let st = WanderState::from_json(&JsonValue::parse(input).unwrap());
        d.eq(
            format!("state {input} json"),
            st.json().js_stringified(),
            s(r, "json").to_string(),
        );
        d.eq(
            format!("state {input} settings"),
            st.settings().json().js_stringified(),
            s(r, "settings").to_string(),
        );
        d.eq(
            format!("state {input} kept"),
            st.kept().into_iter().collect::<Vec<_>>(),
            strings(r, "kept"),
        );
        d.eq(
            format!("state {input} key"),
            st.key.as_ref().map(|k| k.json().js_stringified()),
            opt_str(r, "key"),
        );
    }
    for r in rows(&doc, "keys") {
        let input = s(r, "input");
        let got = WanderKey::from_json(Some(&JsonValue::parse(input).unwrap()));
        d.eq(
            format!("key {input}"),
            got.map(|k| k.json().js_stringified()),
            opt_str(r, "json"),
        );
    }
    d.finish("wander codec golden");
}

#[test]
fn the_auto_mix_codecs_match_swift() {
    let doc = golden();
    let a = doc.get("automixCodec").unwrap();
    let mut d = Diffs::default();
    for r in rows(a, "settings") {
        let st = AutoMixSettings::new(
            r.get("on").and_then(JsonValue::as_bool).unwrap(),
            i(r, "bars"),
            i(r, "every"),
        );
        let tag = format!(
            "settings {} {} {}",
            r.get("on").unwrap().js_stringified(),
            i(r, "bars"),
            i(r, "every")
        );
        d.eq(format!("{tag} bars"), st.bars, i(r, "outBars"));
        d.eq(format!("{tag} every"), st.every, i(r, "outEvery"));
        d.eq(
            format!("{tag} label"),
            st.label(),
            s(r, "label").to_string(),
        );
        d.eq(
            format!("{tag} default"),
            st.is_default(),
            r.get("isDefault").and_then(JsonValue::as_bool).unwrap(),
        );
    }
    for r in rows(a, "states") {
        let input = s(r, "input");
        let v = JsonValue::parse(input).unwrap();
        let st = AutoMixState::from_json(Some(&v));
        let tag = format!("state {input}");
        d.eq(
            format!("{tag} session"),
            st.session_json().js_stringified(),
            s(r, "session").to_string(),
        );
        d.eq(
            format!("{tag} checkpoint"),
            st.checkpoint_json().js_stringified(),
            s(r, "checkpoint").to_string(),
        );
        d.eq(
            format!("{tag} isSessionDefault"),
            st.is_session_default(),
            r.get("isSessionDefault")
                .and_then(JsonValue::as_bool)
                .unwrap(),
        );
        d.eq(
            format!("{tag} start"),
            st.start().session_json().js_stringified(),
            s(r, "start").to_string(),
        );
        d.eq(
            format!("{tag} startFrom"),
            AutoMixState::from_start_json(Some(&v))
                .session_json()
                .js_stringified(),
            s(r, "startFrom").to_string(),
        );
        d.eq(
            format!("{tag} next"),
            st.next().map(|p| p.json().js_stringified()),
            opt_str(r, "next"),
        );
        d.eq(
            format!("{tag} incoming"),
            st.incoming().map(String::from),
            opt_str(r, "incoming"),
        );
        d.eq(
            format!("{tag} fader"),
            st.fader().to_string(),
            s(r, "fader").to_string(),
        );
    }
    for r in rows(a, "handoffs") {
        let h = AutoMixHandoff {
            n: i(r, "n"),
            from: s(r, "from").to_string(),
            to: s(r, "to").to_string(),
            bar: i(r, "bar"),
            kind: s(r, "kind").to_string(),
            move_: s(r, "move").to_string(),
            spec: r.get("spec").and_then(TrackSpec::from_payload),
        };
        d.eq(
            format!("handoff {} json", h.n),
            h.json().js_stringified(),
            s(r, "json").to_string(),
        );
        d.eq(format!("handoff {} cid", h.n), h.cid(), opt_str(r, "cid"));
    }
    // the plans the rule draws for the first two hand-offs over the unlocked techno song
    let case = rows(&doc, "cases")
        .iter()
        .find(|c| s(c, "name") == "techno")
        .unwrap();
    let c = context_of(case.get("ctx").unwrap());
    let g = BeatsTables::shared().genres.get("techno").unwrap();
    let key = root_key(g, 0);
    for (n, r) in rows(a, "plans").iter().enumerate() {
        let drawn = AutoMix::plan(&c, n as i64, Some(&key)).unwrap();
        d.eq(
            format!("plan {n} json"),
            drawn.json().js_stringified(),
            s(r, "json").to_string(),
        );
        let back = AutoMixPlan::from_json(Some(&JsonValue::parse(s(r, "json")).unwrap()));
        d.eq(
            format!("plan {n} round trip"),
            back.map(|p| p.json().js_stringified()),
            opt_str(r, "roundTrip"),
        );
        d.eq(
            format!("plan {n} descriptor"),
            drawn
                .spec
                .as_ref()
                .map(|x| net_descriptor_json(x).js_stringified()),
            JsonValue::parse(s(r, "json"))
                .unwrap()
                .get("spec")
                .map(|x| x.js_stringified()),
        );
    }
    d.finish("auto-mix codec golden");
}

// MARK: the palette table

/// A machine with everything installed, the minecraft bank's version as the dump's Mac recorded it.
fn full_machine(version: &str) -> Installed {
    let (index, format) = match version.split_once("+b") {
        Some((i, f)) => (i, f.parse().unwrap()),
        None => (version, 1),
    };
    Installed {
        dir: "/sf".into(),
        files: vec![
            "MuseScore_General.sf2".into(),
            "minecraft-local.sf2".into(),
            "Kits.sf2".into(),
        ],
        gm_bank: true,
        kits: vec![],
        minecraft: Some(MinecraftSidecar {
            asset_index: index.into(),
            sha1: Default::default(),
            programs: djrust::sound::minecraft::programs()
                .into_iter()
                .map(|(n, _)| n)
                .collect(),
            format,
        }),
        minecraft_assets_missing: None,
    }
}

#[test]
fn the_palette_table_and_the_hops_match_swift() {
    let doc = golden();
    let table = rows(&doc, "palettes");
    let set = PaletteSet::shared();
    let mut d = Diffs::default();
    d.eq(
        "palette ids".to_string(),
        set.palettes
            .iter()
            .map(|p| p.id.clone())
            .collect::<Vec<_>>(),
        table.iter().map(|p| s(p, "id").to_string()).collect(),
    );
    for row in table {
        let id = s(row, "id");
        let p = set
            .named(Some(id))
            .unwrap_or_else(|| panic!("no palette {id}"));
        d.eq(
            format!("{id} label"),
            p.label.clone(),
            s(row, "label").to_string(),
        );
        d.eq(
            format!("{id} neighbours"),
            p.neighbours.clone(),
            strings(row, "neighbours"),
        );
    }
    // the Mac the dump ran on: its minecraft version, and the hops it could take
    let mc = table.iter().find(|p| s(p, "id") == "minecraft").unwrap();
    let version =
        opt_str(mc, "normalized").and_then(|n| n.split_once('@').map(|x| x.1.to_string()));
    let inst = full_machine(version.as_deref().unwrap_or("0"));
    let mut cat = VoiceCatalog::bundled();
    cat.sound_fonts = inst.sound_fonts();
    let ctx = Sounds {
        catalog: &cat,
        installed: &inst,
    };
    let playable = |id: &str| {
        table
            .iter()
            .find(|r| s(r, "id") == id)
            .is_some_and(|r| r.get("unavailable").is_none_or(|u| u.as_str().is_none()))
    };
    for row in table {
        let id = s(row, "id");
        if !playable(id) {
            eprintln!(
                "wander golden: {id} is not playable on the dump's Mac; its hops are not compared"
            );
            continue;
        }
        d.eq(
            format!("{id} normalized"),
            set.normalized(Some(id), ctx.installed),
            opt_str(row, "normalized"),
        );
        let mut got = WanderPalettes::from_sounds(set, Some(id), ctx).unwrap();
        // the machine above has everything installed: a neighbour the dump's Mac could not play
        // is no hop there
        got.options.retain(|o| playable(&o.id));
        let want = palettes_for(table, Some(id)).unwrap();
        d.eq(format!("{id} hops"), got, want);
    }
    // the default palette is what a deck with none plays
    let studio = WanderPalettes::from_sounds(set, None, ctx).unwrap();
    d.eq(
        "studio here".to_string(),
        studio.here_label,
        "Studio".to_string(),
    );
    d.finish("wander palettes golden");
}
