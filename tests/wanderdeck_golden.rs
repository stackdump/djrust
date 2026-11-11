//! Swift goldens for Wander on a deck: the context a deck tells the planner, `setWander`, the
//! paces, the locks, `grabWander`, `advanceWander(toBar:)`, `takeWanderMove`, the look-ahead, the
//! moves on a live deck (a role, STEMS, a macro, a stinger, a palette hop with its role-voice
//! pins, a layer list, a swap), `wanderJSON`, and `serviceWander` driven by the housekeeping tick
//! (`tools/paritydump/wanderdeck.swift`, section "wanderdeck",
//! `tests/fixtures/swift/wanderdeck.json`).
//!
//! The dump drives a real `DJEngine(offline: true)` at 22.05 kHz, two decks, with the machine it
//! runs on pinned to "no SoundFont, no Minecraft bank, Apple's General MIDI bank present"
//! (`$DJ_SOUNDFONTS` at an empty folder). The ops are replayed here through [`Engine`] and
//! [`Renderer`] the same way (`common_track::Stepper`, housekeeping = `Engine::service`, which
//! runs the decks' own service and then `service_wander`), with the same machine
//! ([`machine`]), and the state both decks are in after *every* op is compared with what Swift
//! read, along with what each op returned: the move taken (kind, detail, CID), the walk's JSON
//! (`wanderJSON`, key order and all), the countdown, the idle reason, the look-ahead key, the
//! spec, the project's model CID, the roles on, the STEMS mutes, the palette, kit, layers and,
//! where the dump asked for the whole of it, the context and the voices.
//!
//! Parity form (b) of the port rules. Everything is compared exactly. Process-global counters
//! are not in the dump.

mod common_fixture;
mod common_track;

use std::collections::BTreeMap;

use common_track::Stepper;
use djrust::cid::share_cid;
use djrust::deck::slots::SwapStart;
use djrust::json::{JsonObject, JsonValue};
use djrust::layers::{LayerEntry, LayerLoop};
use djrust::sound::{Installed, PaletteSet, Sounds};
use djrust::voices::{CatalogSound, SamplerSource};
use djrust::wander::deck::{project_kit, project_palette};
use djrust::wander::{Wander, WanderAction, WanderContext};
use djrust::TrackSpec;
use djrust::host::deck::{InstalledSounds, PreparedTrack};

fn golden() -> JsonValue {
    common_fixture::swift_fixture("wanderdeck")
}

/// The machine the dump pinned: nothing installed but Apple's General MIDI bank.
fn machine() -> Installed {
    Installed {
        dir: "/dj-soundfonts-none".into(),
        files: Vec::new(),
        gm_bank: true,
        kits: Vec::new(),
        minecraft: None,
        minecraft_assets_missing: Some("the game's assets are not here".into()),
    }
}

fn bits(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}
fn ob(v: Option<f64>) -> JsonValue {
    v.map_or(JsonValue::Null, |v| JsonValue::String(bits(v)))
}
fn num(v: i64) -> JsonValue {
    JsonValue::Number(v as f64)
}
fn oi(v: Option<i64>) -> JsonValue {
    v.map_or(JsonValue::Null, num)
}
fn os(v: Option<&str>) -> JsonValue {
    v.map_or(JsonValue::Null, |v| JsonValue::String(v.to_string()))
}
fn strs<S: AsRef<str>>(xs: impl IntoIterator<Item = S>) -> JsonValue {
    JsonValue::Array(
        xs.into_iter()
            .map(|x| JsonValue::String(x.as_ref().to_string()))
            .collect(),
    )
}
fn s(v: &JsonValue, key: &str) -> String {
    v.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {key} in {}", v.js_stringified()))
        .to_string()
}
fn f(v: &JsonValue, key: &str) -> f64 {
    v.get(key)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {key} in {}", v.js_stringified()))
}
fn os_of(v: &JsonValue, key: &str) -> Option<String> {
    v.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn spec_text(spec: Option<&TrackSpec>) -> JsonValue {
    spec.map_or(JsonValue::Null, |s| {
        JsonValue::String(s.payload().canonical_js())
    })
}

fn start_text(st: SwapStart) -> String {
    match st {
        SwapStart::Top => "top".into(),
        SwapStart::NextBar => "nextBar".into(),
        SwapStart::Tick(t) => format!("tick:{t}"),
    }
}

fn spec_of(op: &JsonValue) -> TrackSpec {
    let mut spec = TrackSpec::new(
        &s(op, "genre"),
        f(op, "seed"),
        op.get("structure").and_then(JsonValue::as_str),
    );
    if let Some(JsonValue::Object(extra)) = op.get("extra") {
        spec.extra = extra.clone();
    }
    spec
}

fn layer_json(e: &LayerEntry) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs([
        ("path", JsonValue::str(e.loop_.path.as_str())),
        ("name", JsonValue::str(e.loop_.name.as_str())),
        ("category", JsonValue::str(e.loop_.category.as_str())),
        ("beats", num(e.loop_.beats)),
        ("start", num(e.start)),
        ("length", num(e.length)),
        ("gainDB", ob(Some(e.gain_db))),
        ("keyLock", JsonValue::Bool(e.key_lock)),
    ]))
}

fn loop_json(l: &LayerLoop) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs([
        ("path", JsonValue::str(l.path.as_str())),
        ("name", JsonValue::str(l.name.as_str())),
        ("category", JsonValue::str(l.category.as_str())),
        ("beats", num(l.beats)),
    ]))
}

fn voice_json(v: &djrust::wander::WanderVoice) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs([
        ("roles", strs(&v.roles)),
        ("channel", num(v.channel)),
        ("instrument", JsonValue::str(v.instrument.as_str())),
        ("options", strs(&v.options)),
        ("overridden", JsonValue::Bool(v.overridden)),
        ("option", os(v.option.as_deref())),
    ]))
}

fn key_json(k: Option<&djrust::wander::WanderKey>) -> JsonValue {
    k.map_or(JsonValue::Null, |k| k.json())
}

/// What a deck tells the planner, whole.
fn ctx_json(c: Option<&WanderContext>) -> JsonValue {
    let Some(c) = c else { return JsonValue::Null };
    JsonValue::Object(JsonObject::from_pairs([
        ("deck", JsonValue::str(c.deck.as_str())),
        ("spec", spec_text(Some(&c.spec))),
        ("isLoop", JsonValue::Bool(c.is_loop)),
        ("roles", strs(&c.roles)),
        ("rolesOn", strs(&c.roles_on)),
        ("disabled", strs(&c.disabled_roles)),
        ("stemsMuted", strs(&c.stems_muted)),
        ("lockedActions", strs(&c.locked_actions)),
        ("locks", strs(&c.locks)),
        (
            "voices",
            JsonValue::Array(c.voices.iter().map(voice_json).collect()),
        ),
        ("anchorBPM", ob(c.anchor_bpm)),
        ("key", key_json(c.key.as_ref())),
        ("palette", os(c.palette.as_deref())),
        ("kit", os(c.kit.as_deref())),
        (
            "channels",
            JsonValue::Array(c.channels.iter().map(|&x| num(x)).collect()),
        ),
        ("stingers", strs(&c.stingers)),
        ("mixBPM", ob(c.mix_bpm)),
        (
            "rawBase",
            c.raw_base
                .map_or(JsonValue::Null, |b| JsonValue::String(format!("{b:x}"))),
        ),
        (
            "layers",
            JsonValue::Array(c.layers.iter().map(layer_json).collect()),
        ),
        (
            "layerPool",
            JsonValue::Array(c.layer_pool.iter().map(loop_json).collect()),
        ),
        ("bar", num(c.bar)),
        ("pace", num(c.pace)),
    ]))
}

/// The rig: two decks on a [`Stepper`], and what a render op saw the walks do.
struct Rig {
    st: Stepper,
    blocks: i64,
    events: Vec<JsonValue>,
    seen: BTreeMap<String, (i64, usize, String)>,
}

/// What the dump's Mac names as sampled instruments, by category (its Logic / GarageBand
/// folders: the Rust engine cannot play them, but a palette's `cat:` option is playable when its
/// category has one).
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

fn installed_sounds(g: &JsonValue) -> std::sync::Arc<InstalledSounds> {
    InstalledSounds::with_catalog(machine(), catalog(g))
}

impl Rig {
    fn new(g: &JsonValue) -> Rig {
        let mut st = Stepper::new(22_050.0, true);
        st.e.deterministic = true;
        st.e.installed = Some(installed_sounds(g));
        Rig {
            st,
            blocks: 0,
            events: Vec::new(),
            seen: BTreeMap::new(),
        }
    }

    /// One block (the housekeeping if due, then the render), and a note of any walk that moved.
    fn step(&mut self) {
        self.st.block();
        self.blocks += 1;
        for name in ["A", "B"] {
            let d = self.st.e.deck(name).unwrap();
            let w = &d.walk.wander;
            let now = (
                w.step,
                w.log.len(),
                spec_text(d.spec.as_ref())
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            );
            if self.seen.get(name) == Some(&now) {
                continue;
            }
            if self.seen.contains_key(name) {
                let mut o = JsonObject::new();
                o.set("block", num(self.blocks));
                o.set("deck", JsonValue::str(name));
                o.set("tick", num(d.mirror.tick()));
                o.set("step", num(w.step));
                o.set("log", num(w.log.len() as i64));
                o.set("move", w.log.last().map_or(JsonValue::Null, |m| m.json()));
                self.events.push(JsonValue::Object(o));
            }
            self.seen.insert(name.to_string(), now);
        }
    }
}

fn state_of(rig: &mut Rig, name: &str, full: bool) -> JsonValue {
    let look = rig.st.e.look_ahead_key(name);
    let e = &rig.st.e;
    let d = e.deck(name).unwrap();
    let net = d.is_net();
    let mut o = JsonObject::new();
    o.set("mode", JsonValue::str(if net { "net" } else { "sample" }));
    o.set("playing", JsonValue::Bool(d.playing));
    o.set("wavePlaying", JsonValue::Bool(net && d.mirror.is_playing()));
    o.set("tick", num(d.mirror.tick()));
    let wander_text =
        |e: &djrust::host::deck::Engine| JsonValue::String(e.wander_json(name).unwrap().js_stringified());
    if !net {
        o.set("wander", wander_text(e));
        return JsonValue::Object(o);
    }
    let w = &d.walk.wander;
    let latest = d.track.latest.as_ref();
    o.set(
        "installed",
        JsonValue::Bool(latest.is_none_or(|l| {
            d.mirror
                .generation
                .load(std::sync::atomic::Ordering::Relaxed)
                == l.generation
        })),
    );
    o.set("latestStart", oi(latest.map(|l| l.start_tick)));
    o.set("spec", spec_text(d.spec.as_ref()));
    o.set(
        "rawCID",
        os(d.track.raw_nets.as_ref().map(share_cid).as_deref()),
    );
    o.set(
        "projectCID",
        d.project
            .as_ref()
            .map_or(JsonValue::Null, |p| JsonValue::String(p.model_cid())),
    );
    o.set("isNetLoop", JsonValue::Bool(d.is_net_loop()));
    o.set("netBaseBPM", JsonValue::String(bits(d.base_tempo)));
    o.set("wander", wander_text(e));
    o.set("on", JsonValue::Bool(w.on));
    o.set("pace", num(w.pace));
    o.set("paused", JsonValue::Bool(w.paused));
    o.set("step", num(w.step));
    o.set("quietBars", num(w.quiet_bars));
    o.set("lastBar", oi(w.last_bar));
    o.set("anchorBPM", ob(w.anchor_bpm));
    o.set("key", key_json(w.key.as_ref()));
    o.set("locks", strs(&w.locks));
    o.set(
        "log",
        JsonValue::Array(w.log.iter().map(|m| m.json()).collect()),
    );
    o.set("countdown", oi(e.wander_countdown(name)));
    o.set("idle", os(e.wander_idle_reason(name)));
    o.set(
        "lookAhead",
        look.map_or(JsonValue::Null, |k| {
            JsonValue::Object(JsonObject::from_pairs([
                ("step", num(k.step)),
                ("spec", spec_text(Some(&k.spec))),
                ("start", JsonValue::String(start_text(k.start))),
                (
                    "roleFilter",
                    k.role_filter
                        .as_ref()
                        .map_or(JsonValue::Null, |_| JsonValue::str("filtered")),
                ),
            ]))
        }),
    );
    o.set("seedTarget", spec_text(d.track.seed_target.as_ref()));
    let roles = e.roles(name);
    o.set("roles", strs(&roles));
    o.set(
        "rolesOn",
        strs(roles.iter().filter(|r| e.is_role_audible(name, r))),
    );
    o.set(
        "disabled",
        strs(
            d.disabled_roles
                .iter()
                .collect::<std::collections::BTreeSet<_>>(),
        ),
    );
    let mut stems = JsonObject::new();
    for (k, v) in &d.walk.stems_muted.groups {
        stems.set(k, strs(v));
    }
    o.set("stems", JsonValue::Object(stems));
    let project = d.project.as_ref();
    o.set("palette", os(project.and_then(project_palette).as_deref()));
    o.set("kit", os(project.and_then(project_kit).as_deref()));
    o.set(
        "layers",
        JsonValue::Array(d.walk.layer_entries.iter().map(layer_json).collect()),
    );
    o.set("layerEpoch", num(d.walk.layer_epoch));
    o.set("layerKeyName", os(d.walk.layer_key_name.as_deref()));
    o.set("layerKey", os(e.layer_key(name).as_deref()));
    if full {
        o.set("context", ctx_json(e.wander_context(name).as_ref()));
        o.set(
            "stingers",
            JsonValue::Array(e.stingers(name).iter().map(|a| a.json()).collect()),
        );
        o.set(
            "voices",
            JsonValue::Array(e.wander_voices(name).iter().map(voice_json).collect()),
        );
        o.set("wanderKey", key_json(e.wander_key(name).as_ref()));
        let src = e.net_source(name).map(|src| match src {
            djrust::session::model::DeckSource::Net(spec) => share_cid(&spec.payload()),
            djrust::session::model::DeckSource::RawNets(p) => share_cid(&p),
            _ => String::new(),
        });
        o.set("sourceCID", os(src.as_deref()));
    }
    JsonValue::Object(o)
}

fn move_json(m: Option<&djrust::wander::WanderMove>) -> JsonValue {
    m.map_or(JsonValue::Null, |m| m.json())
}

/// One op (`run` in the dump); its result as the dump writes it.
fn run(rig: &mut Rig, op: &JsonValue) -> JsonValue {
    let kind = s(op, "op");
    match kind.as_str() {
        "render" => {
            rig.events.clear();
            for _ in 0..f(op, "blocks") as usize {
                rig.step();
            }
            return JsonValue::Array(std::mem::take(&mut rig.events));
        }
        "deterministic" => {
            rig.st.e.deterministic = op.get("on").and_then(JsonValue::as_bool).unwrap();
            return JsonValue::Null;
        }
        "lockedActions" => {
            rig.st.e.locked = op
                .get("list")
                .and_then(JsonValue::as_array)
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            return JsonValue::Null;
        }
        _ => {}
    }
    let deck = os_of(op, "deck").unwrap_or_else(|| "A".into());
    let d = deck.as_str();
    match kind.as_str() {
        "net" => {
            let pr = rig.st.e.preparer(d).unwrap();
            let track = PreparedTrack::compose(&spec_of(op), &pr, 0);
            assert!(rig.st.e.load_net(d, track));
        }
        "rawNets" => {
            let pr = rig.st.e.preparer(d).unwrap();
            let track =
                PreparedTrack::from_raw_nets(op.get("payload").unwrap(), &pr).expect("nets");
            assert!(rig.st.e.load_net(d, track));
        }
        "demo" => assert!(rig.st.e.load_demo(d)),
        "play" => {
            rig.st.e.set_playing(d, true);
        }
        "pause" => {
            rig.st.e.set_playing(d, false);
        }
        "toTick" => {
            let want = f(op, "tick") as i64;
            let mut n = 0;
            while rig.st.e.deck(d).unwrap().mirror.tick() < want && n < 40_000 {
                rig.step();
                n += 1;
            }
        }
        "wander" => rig
            .st
            .e
            .set_wander(d, op.get("on").and_then(JsonValue::as_bool).unwrap()),
        "pace" => rig.st.e.set_wander_pace(d, f(op, "bars") as i64),
        "stepPace" => rig.st.e.step_wander_pace(d, f(op, "delta") as i64),
        "lock" => rig.st.e.set_wander_lock(
            d,
            &s(op, "dimension"),
            op.get("on").and_then(JsonValue::as_bool).unwrap(),
        ),
        "grab" => rig.st.e.grab_wander(d),
        "setStep" => rig.st.e.deck_mut(d).unwrap().walk.wander.step = f(op, "step") as i64,
        "lastBarNil" => rig.st.e.deck_mut(d).unwrap().walk.wander.last_bar = None,
        "advance" => {
            let mut rows = Vec::new();
            for bar in f(op, "from") as i64..=f(op, "to") as i64 {
                let before = rig.st.e.deck(d).unwrap().walk.wander.log.len();
                rig.st.e.advance_wander(d, bar);
                let look = rig.st.e.look_ahead_key(d).map(|k| k.step);
                let e = &rig.st.e;
                let w = &e.deck(d).unwrap().walk.wander;
                let moved = w.log.len() > before;
                let mut o = JsonObject::new();
                o.set("bar", num(bar));
                o.set("step", num(w.step));
                o.set("logCount", num(w.log.len() as i64));
                o.set(
                    "move",
                    if moved {
                        move_json(w.log.last())
                    } else {
                        JsonValue::Null
                    },
                );
                o.set("lookAhead", oi(look));
                o.set("countdown", oi(e.wander_countdown(d)));
                o.set("paused", JsonValue::Bool(w.paused));
                o.set("quiet", num(w.quiet_bars));
                rows.push(JsonValue::Object(o));
            }
            return JsonValue::Array(rows);
        }
        "take" => {
            let m = rig.st.e.take_wander_move(d, f(op, "bar") as i64);
            return move_json(m.as_ref());
        }
        "takeKind" => {
            let Some(c) = rig.st.e.wander_context(d) else {
                return JsonValue::Null;
            };
            let want = s(op, "kind");
            let from = rig.st.e.deck(d).unwrap().walk.wander.step;
            let Some(step) = (from..from + 400)
                .find(|&st| Wander::plan(&c, st).is_some_and(|p| p.kind.raw() == want))
            else {
                return JsonValue::Null;
            };
            rig.st.e.deck_mut(d).unwrap().walk.wander.step = step;
            let m = rig.st.e.take_wander_move(d, f(op, "bar") as i64);
            return JsonValue::Object(JsonObject::from_pairs([
                ("found", num(step)),
                ("move", move_json(m.as_ref())),
            ]));
        }
        "stepToSwap" => {
            rig.st.e.set_wander(d, true);
            rig.st.e.set_wander_pace(d, 4);
            rig.st.e.deck_mut(d).unwrap().walk.wander.last_bar = None;
            let Some(c) = rig.st.e.wander_context(d) else {
                return JsonValue::Null;
            };
            let from = op.get("from").and_then(JsonValue::as_f64).unwrap_or(0.0) as i64;
            for step in from..from + 200 {
                if let Some(WanderAction::Swap(spec)) = Wander::plan(&c, step).map(|p| p.action) {
                    rig.st.e.deck_mut(d).unwrap().walk.wander.step = step;
                    return JsonValue::Object(JsonObject::from_pairs([
                        ("step", num(step)),
                        ("spec", spec_text(Some(&spec))),
                    ]));
                }
            }
            return JsonValue::Null;
        }
        "swapKey" => {
            return rig
                .st
                .e
                .wander_swap_key(d, f(op, "bar") as i64)
                .map_or(JsonValue::Null, |k| {
                    JsonValue::Object(JsonObject::from_pairs([
                        ("step", num(k.step)),
                        ("spec", spec_text(Some(&k.spec))),
                        ("start", JsonValue::String(start_text(k.start))),
                        (
                            "roleFilter",
                            k.role_filter
                                .as_ref()
                                .map_or(JsonValue::Null, |_| JsonValue::str("filtered")),
                        ),
                    ]))
                });
        }
        "settle" => rig.st.e.wait_composes(d),
        "waitAhead" => {
            rig.st.e.wait_look_ahead(d);
            return JsonValue::Object(JsonObject::from_pairs([(
                "ready",
                JsonValue::Bool(rig.st.e.look_ahead_ready(d)),
            )]));
        }
        "layerPool" => {
            rig.st.e.deck_mut(d).unwrap().walk.layer_pool = op
                .get("pool")
                .and_then(JsonValue::as_array)
                .unwrap()
                .iter()
                .map(|p| {
                    LayerLoop::new(
                        &s(p, "path"),
                        &s(p, "name"),
                        &s(p, "category"),
                        f(p, "beats") as i64,
                    )
                })
                .collect();
        }
        "setLayers" => {
            let entries = op
                .get("entries")
                .and_then(JsonValue::as_array)
                .unwrap()
                .iter()
                .map(|e| {
                    LayerEntry::new(
                        LayerLoop::new(
                            &s(e, "path"),
                            &s(e, "name"),
                            &s(e, "category"),
                            f(e, "beats") as i64,
                        ),
                        f(e, "start") as i64,
                        f(e, "length") as i64,
                        f(e, "gainDB"),
                        e.get("keyLock").and_then(JsonValue::as_bool).unwrap(),
                    )
                })
                .collect();
            rig.st.e.set_layers(d, entries);
        }
        "palette" => rig.st.e.set_palette(d, os_of(op, "palette").as_deref()),
        "roleVoice" => rig
            .st
            .e
            .set_role_voice(d, &s(op, "role"), os_of(op, "voice").as_deref()),
        "stepSeed" => {
            return spec_text(rig.st.e.step_seed(d, f(op, "delta") as i64).as_ref());
        }
        "stinger" => rig.st.e.stinger(d, f(op, "n") as i64, 1),
        "stem" => rig.st.e.toggle_stem(d, &s(op, "stem")),
        "role" => {
            rig.st.e.set_role(
                d,
                &s(op, "role"),
                op.get("on").and_then(JsonValue::as_bool).unwrap(),
            );
        }
        other => panic!("unknown op {other}"),
    }
    JsonValue::Null
}

/// The first place two JSON values differ, as a path and both sides.
fn first_diff(path: &str, want: &JsonValue, got: &JsonValue) -> Option<String> {
    match (want, got) {
        (JsonValue::Object(w), JsonValue::Object(g)) => {
            for (k, wv) in w.iter() {
                let gv = g.get(k).cloned().unwrap_or(JsonValue::Null);
                if let Some(d) = first_diff(&format!("{path}.{k}"), wv, &gv) {
                    return Some(d);
                }
            }
            for (k, _) in g.iter() {
                if w.get(k).is_none() {
                    return Some(format!("{path}.{k}: not in the golden"));
                }
            }
            None
        }
        _ if want.js_stringified() == got.js_stringified() => None,
        _ => Some(format!(
            "{path}: golden {} / rust {}",
            want.js_stringified(),
            got.js_stringified()
        )),
    }
}

/// One scenario against the golden: the first place Rust disagrees, if it does; and how many deck
/// states were compared.
fn replay(g: &JsonValue, sc: &JsonValue) -> (Result<(), String>, usize) {
    let name = s(sc, "name");
    let mut rig = Rig::new(g);
    let mut compared = 0;
    for (n, step) in sc
        .get("steps")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .enumerate()
    {
        let op = step.get("op").unwrap();
        let result = run(&mut rig, op);
        let ctx = format!("{name} #{n} {}", op.js_stringified());
        if let Some(d) = first_diff("result", step.get("result").unwrap(), &result) {
            return (Err(format!("{ctx}: {d}")), compared);
        }
        let full = op.get("full").and_then(JsonValue::as_bool).unwrap_or(false);
        let decks = step.get("decks").unwrap();
        for deck in ["A", "B"] {
            let got = state_of(&mut rig, deck, full && deck == "A");
            if let Some(d) = first_diff(deck, decks.get(deck).unwrap(), &got) {
                return (Err(format!("{ctx}: {d}")), compared);
            }
            compared += 1;
        }
    }
    (Ok(()), compared)
}

#[test]
fn the_dumps_machine_is_the_one_this_test_gives_its_engine() {
    let g = golden();
    let set = PaletteSet::shared();
    let inst = installed_sounds(&g);
    let ctx: Sounds = inst.sounds();
    let table = g.get("palettes").and_then(JsonValue::as_array).unwrap();
    assert!(table.len() >= 10, "{} palettes", table.len());
    for row in table {
        let id = s(row, "id");
        let p = set
            .named(Some(&id))
            .unwrap_or_else(|| panic!("no palette {id}"));
        assert_eq!(p.label, s(row, "label"), "{id}: label");
        let neighbours: Vec<String> = row
            .get("neighbours")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        assert_eq!(p.neighbours, neighbours, "{id}: neighbours");
        let want = row.get("unavailable").and_then(JsonValue::as_str);
        let got = p.unavailable_reason(set, ctx);
        assert_eq!(
            want.is_some(),
            got.is_some(),
            "{id}: Swift says {want:?}, Rust {got:?}"
        );
        assert_eq!(
            set.normalized(Some(&id), ctx.installed),
            os_of(row, "normalized"),
            "{id}: normalized"
        );
    }
}

#[test]
fn wander_on_a_deck_matches_the_swift_engine() {
    let g = golden();
    let scenarios = g.get("scenarios").and_then(JsonValue::as_array).unwrap();
    // (`WANDERDECK_PARTIAL=1` lets a fixture made with `WANDERDECK_ONLY` through while iterating)
    assert!(
        scenarios.len() >= 20 || std::env::var("WANDERDECK_PARTIAL").is_ok(),
        "{} scenarios",
        scenarios.len()
    );
    let (mut compared, mut failures) = (0, Vec::new());
    for sc in scenarios {
        let (r, n) = replay(&g, sc);
        compared += n;
        if let Err(e) = r {
            failures.push(e);
        }
    }
    eprintln!(
        "wanderdeck_golden: {compared} deck states compared over {} scenarios",
        scenarios.len()
    );
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ from Swift:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
