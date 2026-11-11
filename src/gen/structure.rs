//! Port of `Generator/Structure.swift` (generator/structure.js: song templates and
//! control nets, plus the feel/macro/fade helpers compose() uses from arrange.js).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::{cos, obj, ring_layout, sin};
use crate::gen::tables::{BeatsTables, GenreTheory, OrderedFlags};
use crate::json::{floor, utf16_cmp, JsonValue};
use crate::pflow::{
    Arc, ControlBinding, NetBundle, OrderedMap, Place, Project, Section, Track, Transition,
};
use crate::rng::Mulberry32;

#[derive(Clone, Debug)]
pub struct TemplateSection {
    pub name: String,
    pub steps: i64,
    pub active: OrderedFlags,
    pub phrases: OrderedMap<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct SongTemplate {
    pub name: String,
    pub sections: Vec<TemplateSection>,
    pub slot_map: OrderedMap<Vec<Vec<i64>>>,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| String::from(*s)).collect()
}

/// `defaultPhrases(name)` (theory.js / structure.js — identical).
pub fn default_phrases(name: &str) -> Vec<String> {
    strs(match name {
        "intro" | "outro" => &["A", "A"],
        "verse" => &["A", "A", "B", "A"],
        "chorus" | "drop" => &["A", "B", "A", "B"],
        "bridge" | "breakdown" => &["C", "C"],
        "buildup" | "pre-chorus" => &["A", "B"],
        "solo" => &["A", "B", "C", "A"],
        _ => &["A", "A"],
    })
}

/// `genrePhrases(theory, section)`
pub fn genre_phrases(theory: Option<&GenreTheory>, section: &str) -> Vec<String> {
    theory
        .and_then(|t| t.phrase_patterns.as_ref())
        .and_then(|p| p.get(section))
        .cloned()
        .unwrap_or_else(|| default_phrases(section))
}

pub(crate) fn section_steps(name: &str, size: &str) -> i64 {
    match size {
        "minimal" => {
            if name == "intro" || name == "outro" {
                32
            } else {
                128
            }
        }
        "extended" => match name {
            "intro" | "outro" | "breakdown" | "bridge" | "buildup" | "pre-chorus" | "solo" => 128,
            _ => 192,
        },
        _ => match name {
            "intro" | "outro" | "breakdown" | "bridge" | "pre-chorus" | "solo" => 64,
            _ => 192,
        },
    }
}

fn section_with_phrases(name: &str, steps: i64, active: OrderedFlags) -> TemplateSection {
    let mut phrases = OrderedMap::new();
    let pattern = default_phrases(name);
    for role in active.keys() {
        if role != "snare" {
            phrases.insert(role.as_str(), pattern.clone());
        }
    }
    TemplateSection {
        name: name.into(),
        steps,
        active,
        phrases,
    }
}

/// `generateStructure(genreName, size, rng)`
pub fn generate_structure(genre: &str, size: &str, rng: &mut Mulberry32) -> SongTemplate {
    let tables = BeatsTables::shared();
    let family = tables.family_of(genre);
    let blueprints = tables
        .blueprints(family, size)
        .or_else(|| tables.blueprints(1, "standard"))
        .unwrap_or_else(|| alloc::vec![strs(&["verse", "chorus"])]);
    let blueprint = &blueprints[floor(rng.next() * blueprints.len() as f64) as usize];
    let sections = blueprint
        .iter()
        .map(|name| {
            section_with_phrases(
                name,
                section_steps(name, size),
                tables.archetype(family, name).unwrap_or_default(),
            )
        })
        .collect();
    SongTemplate {
        name: size.into(),
        sections,
        slot_map: OrderedMap::new(),
    }
}

pub fn extract_slot_index(net_id: &str, riff_group: &str) -> i64 {
    let skip = riff_group.encode_utf16().count() + 1;
    let mut idx = 0i64;
    for c in net_id.encode_utf16().skip(skip) {
        if (48..=57).contains(&c) {
            idx = idx * 10 + (c - 48) as i64;
        }
    }
    idx
}

// ---------------------------------------------------------------------------
// Accumulator control nets

pub struct StructureEvent {
    pub tick: i64,
    pub binding: ControlBinding,
}

fn control_track() -> Track {
    Track::default()
}

pub fn build_accumulator_net(events: Vec<StructureEvent>) -> NetBundle {
    let mut nb = NetBundle {
        track: control_track(),
        role: "control".into(),
        ..Default::default()
    };
    if events.is_empty() {
        nb.places.insert(
            "p0",
            Place {
                initial: alloc::vec![1.0],
                ..Default::default()
            },
        );
        nb.transitions.insert("t0", Transition::default());
        nb.arcs.push(Arc::new("p0", "t0"));
        nb.arcs.push(Arc::new("t0", "p0"));
        return nb;
    }
    let mut sorted = events;
    sorted.sort_by_key(|e| e.tick); // stable, as Array.prototype.sort
    let n = sorted.len();
    let mut prev_tick = 0;
    for (i, ev) in sorted.into_iter().enumerate() {
        let gap = ev.tick - prev_tick;
        let p_delay = format!("p_delay_{i}");
        let p_gate = format!("p_gate_{i}");
        let t_drain = format!("t_drain_{i}");
        let t_ctrl = format!("t_ctrl_{i}");
        nb.places.insert(
            p_delay.clone(),
            Place {
                initial: alloc::vec![gap as f64],
                ..Default::default()
            },
        );
        nb.places.insert(
            p_gate.clone(),
            Place {
                initial: alloc::vec![if i == 0 { 1.0 } else { 0.0 }],
                ..Default::default()
            },
        );
        nb.transitions
            .insert(t_drain.clone(), Transition::default());
        nb.arcs.push(Arc::new(p_delay.clone(), t_drain.clone()));
        nb.arcs.push(Arc::new(p_gate.clone(), t_drain.clone()));
        nb.arcs.push(Arc::new(t_drain, p_gate.clone()));
        nb.transitions.insert(t_ctrl.clone(), Transition::default());
        nb.arcs.push(Arc {
            inhibit: true,
            ..Arc::new(p_delay, t_ctrl.clone())
        });
        nb.arcs.push(Arc::new(p_gate, t_ctrl.clone()));
        if i < n - 1 {
            nb.arcs
                .push(Arc::new(t_ctrl.clone(), format!("p_gate_{}", i + 1)));
        }
        nb.control_bindings.insert(t_ctrl, ev.binding);
        prev_tick = ev.tick;
    }
    nb
}

fn role_control_net(role: &str, slot_map: &[Vec<i64>], template: &SongTemplate) -> NetBundle {
    let mut events = Vec::new();
    let mut prev_slot = slot_map
        .first()
        .and_then(|f| f.first())
        .copied()
        .unwrap_or(-1);
    let mut pos = 0;
    for (si, sec) in template.sections.iter().enumerate() {
        let mut phrases = sec.phrases.get(role).cloned().unwrap_or_default();
        if phrases.is_empty() {
            phrases = strs(&["A"]);
        }
        let phrase_len = sec.steps / phrases.len() as i64;
        for pi in 0..phrases.len() {
            let phrase_start = pos + pi as i64 * phrase_len;
            if phrase_start == 0 {
                continue;
            }
            let cur_slot = slot_map
                .get(si)
                .and_then(|s| s.get(pi))
                .copied()
                .unwrap_or(-1);
            if cur_slot != prev_slot {
                let binding = if cur_slot >= 0 {
                    ControlBinding::new("activate-slot", &format!("{role}-{cur_slot}"))
                } else {
                    ControlBinding::new("mute-track", &format!("{role}-{prev_slot}"))
                };
                events.push(StructureEvent {
                    tick: phrase_start,
                    binding,
                });
                prev_slot = cur_slot;
            }
        }
        pos += sec.steps;
    }
    build_accumulator_net(events)
}

fn linear_control_net(target: &str, template: &SongTemplate) -> NetBundle {
    let mut events = Vec::new();
    let mut was_active = template.sections[0].active.contains(target);
    let mut pos = 0;
    for si in 1..template.sections.len() {
        pos += template.sections[si - 1].steps;
        let is_active = template.sections[si].active.contains(target);
        if is_active != was_active {
            let action = if is_active {
                "unmute-track"
            } else {
                "mute-track"
            };
            events.push(StructureEvent {
                tick: pos,
                binding: ControlBinding::new(action, target),
            });
            was_active = is_active;
        }
    }
    build_accumulator_net(events)
}

fn linear_stop_net(total_steps: i64) -> NetBundle {
    build_accumulator_net(alloc::vec![StructureEvent {
        tick: total_steps,
        binding: ControlBinding::new("stop-transport", ""),
    }])
}

pub fn is_stinger_id(id: &str) -> bool {
    id.len() > 3 && id.starts_with("hit") && id.as_bytes()[3..].iter().all(u8::is_ascii_digit)
}

/// `songStructure(proj, template, musicNets)`: adds struct-* nets, sets
/// `proj.structure`, returns the nets that start muted.
pub fn song_structure(
    proj: &mut Project,
    template: &SongTemplate,
    music_nets: &[String],
) -> Vec<String> {
    let total_steps: i64 = template.sections.iter().map(|s| s.steps).sum();
    let mut initial_mutes = Vec::new();

    let slot_roles: BTreeSet<String> = music_nets
        .iter()
        .filter_map(|id| proj.nets.get(id).map(|n| n.riff_group.clone()))
        .filter(|g| !g.is_empty())
        .collect();
    let mut processed = BTreeSet::new();
    for net_id in music_nets {
        let Some(nb) = proj.nets.get(net_id) else {
            continue;
        };
        if !nb.riff_group.is_empty() {
            let role = nb.riff_group.clone();
            if !processed.insert(role.clone()) {
                continue;
            }
            let slot_map = template.slot_map.get(&role).cloned().unwrap_or_default();
            proj.nets.insert(
                format!("struct-{role}"),
                role_control_net(&role, &slot_map, template),
            );
            let first_slot = slot_map
                .first()
                .and_then(|f| f.first())
                .copied()
                .unwrap_or(-1);
            for n_id in music_nets {
                if proj.nets.get(n_id).is_some_and(|n2| n2.riff_group == role) {
                    let slot_idx = extract_slot_index(n_id, &role);
                    if slot_idx != first_slot || first_slot < 0 {
                        initial_mutes.push(n_id.clone());
                    }
                }
            }
        } else if !slot_roles.contains(net_id) {
            // Stingers are one-shot accents: never driven by structure, always muted.
            if nb.track.group.as_deref() == Some("stinger") || is_stinger_id(net_id) {
                initial_mutes.push(net_id.clone());
                continue;
            }
            proj.nets.insert(
                format!("struct-{net_id}"),
                linear_control_net(net_id, template),
            );
            if !template.sections[0].active.contains(net_id) {
                initial_mutes.push(net_id.clone());
            }
        }
    }
    proj.nets
        .insert("struct-stop", linear_stop_net(total_steps));
    proj.structure = template
        .sections
        .iter()
        .map(|sec| Section {
            name: sec.name.clone(),
            steps: sec.steps,
            phrases: sec.phrases.clone(),
        })
        .collect();
    initial_mutes
}

// ---------------------------------------------------------------------------
// Arrange helpers (fade / break / feel / macro curves)

pub const BOUNDARY_STAGGER: i64 = 12;

/// arrange.js `buildControlBundle`, in its parsed (`parseNetBundle`) form.
pub fn build_control_bundle(
    steps: i64,
    ctrl_at: &BTreeMap<i64, ControlBinding>,
    sink: bool,
    place_prefix: &str,
    trans_prefix: &str,
) -> NetBundle {
    let mut nb = NetBundle {
        role: "control".into(),
        track: Track::default(),
        ..Default::default()
    };
    let (cx, cy, radius) = ring_layout(steps);
    let place_count = if sink { steps + 1 } else { steps };
    let angle_den = if sink { steps + 1 } else { steps } as f64;
    let tau = 2.0 * core::f64::consts::PI;
    for i in 0..place_count {
        let angle = i as f64 / angle_den * tau;
        nb.places.insert(
            format!("{place_prefix}{i}"),
            Place {
                initial: alloc::vec![if i == 0 { 1.0 } else { 0.0 }],
                x: cx + radius * 0.7 * cos(angle),
                y: cy + radius * 0.7 * sin(angle),
                label: None,
            },
        );
    }
    for i in 0..steps {
        let t_angle = (i as f64 + 0.5) / angle_den * tau;
        let tid = format!("{trans_prefix}{i}");
        nb.transitions.insert(
            tid.clone(),
            Transition {
                x: cx + radius * cos(t_angle),
                y: cy + radius * sin(t_angle),
                label: None,
            },
        );
        if let Some(c) = ctrl_at.get(&i) {
            nb.control_bindings.insert(tid.clone(), c.clone());
        }
        let prev = format!("{place_prefix}{i}");
        let next = if sink {
            format!("{place_prefix}{}", i + 1)
        } else {
            format!("{place_prefix}{}", (i + 1) % steps)
        };
        nb.arcs.push(Arc::new(prev, tid.clone()));
        nb.arcs.push(Arc::new(tid, next));
    }
    nb
}

fn fade_control_net(target: &str, action: &str, steps: i64, hit_pos: i64) -> NetBundle {
    let hit = if hit_pos >= steps { steps - 1 } else { hit_pos };
    let mut ctrl = BTreeMap::new();
    ctrl.insert(hit, ControlBinding::new(action, target));
    build_control_bundle(steps, &ctrl, false, "p", "t")
}

pub fn fade_in(proj: &mut Project, targets: &[String], steps: i64) -> Vec<String> {
    let steps = if steps < 8 { 32 } else { steps };
    let mut muted = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        if !proj.nets.contains_key(target) {
            continue;
        }
        muted.push(target.clone());
        let offset = (i as i64 + 1) * (steps / (targets.len() as i64 + 1));
        proj.nets.insert(
            format!("fade-in-{target}"),
            fade_control_net(target, "unmute-track", steps, offset),
        );
    }
    muted
}

pub fn fade_out(proj: &mut Project, targets: &[String], steps: i64) {
    let steps = if steps < 8 { 32 } else { steps };
    let n = targets.len() as i64;
    for (i, target) in targets.iter().enumerate() {
        if !proj.nets.contains_key(target) {
            continue;
        }
        let offset = steps - (n - i as i64) * (steps / (n + 1));
        proj.nets.insert(
            format!("fade-out-{target}"),
            fade_control_net(target, "mute-track", steps, offset),
        );
    }
}

pub fn drum_break(proj: &mut Project, targets: &[String], cycle_len: i64, break_len: i64) {
    let cycle_len = if cycle_len < 16 { 64 } else { cycle_len };
    let mut break_len = if break_len < 4 { 8 } else { break_len };
    if break_len >= cycle_len {
        break_len = cycle_len / 4;
    }
    let mute_pos = cycle_len / 2;
    let mut sorted: Vec<&String> = targets.iter().collect();
    sorted.sort_by(|a, b| utf16_cmp(a, b));
    let unmute_base = (mute_pos + break_len) % cycle_len;
    let headroom = (((mute_pos - unmute_base - 1) % cycle_len) + cycle_len) % cycle_len;
    let max_stagger = BOUNDARY_STAGGER.min(headroom).max(1);
    for (i, target) in sorted.into_iter().enumerate() {
        if !proj.nets.contains_key(target) {
            continue;
        }
        let unmute_pos = (mute_pos + break_len + (i as i64 % max_stagger)) % cycle_len;
        let mut ctrl = BTreeMap::new();
        ctrl.insert(mute_pos, ControlBinding::new("mute-track", target));
        ctrl.insert(unmute_pos, ControlBinding::new("unmute-track", target));
        proj.nets.insert(
            format!("break-{target}"),
            build_control_bundle(cycle_len, &ctrl, false, "p", "t"),
        );
    }
}

/// A feel-curve point: the section it lands on and the (x, y) puck position.
pub struct FeelPoint {
    pub section: String,
    pub x: f64,
    pub y: f64,
}

fn section_starts(tmpl: &SongTemplate) -> (BTreeMap<String, i64>, i64) {
    let mut starts = BTreeMap::new();
    let mut cum = 0;
    for sec in &tmpl.sections {
        starts.entry(sec.name.clone()).or_insert(cum);
        cum += sec.steps;
    }
    (starts, cum)
}

pub fn inject_feel_curve(proj: &mut Project, tmpl: &SongTemplate, curve: &[FeelPoint]) {
    let (starts, total) = section_starts(tmpl);
    if total == 0 {
        return;
    }
    let mut points: Vec<(i64, ControlBinding)> = Vec::new();
    for e in curve {
        let Some(&tick) = starts.get(&e.section) else {
            continue;
        };
        let mut c = ControlBinding::new("set-feel", "");
        c.macro_params = Some(obj(alloc::vec![
            ("x", Some(JsonValue::Number(e.x))),
            ("y", Some(JsonValue::Number(e.y))),
        ]));
        points.push((tick, c));
    }
    if points.is_empty() {
        return;
    }
    points.sort_by_key(|p| p.0);
    let mut ctrl = BTreeMap::new();
    for (tick, c) in points {
        ctrl.insert(tick, c);
    }
    proj.nets.insert(
        "feel-curve",
        build_control_bundle(total, &ctrl, false, "fp", "ft"),
    );
}

/// Property names every JS object inherits from `Object.prototype`.
pub const OBJECT_PROTO_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `injectMacroCurve(proj, tmpl, curve)`: `curve` is the envelope's `macroCurve`
/// array of `{ section, macro, bars? }` objects.
pub fn inject_macro_curve(proj: &mut Project, tmpl: &SongTemplate, curve: &[JsonValue]) {
    inject_curve(proj, tmpl, curve, "macro-curve", ("mp", "mt"), |e| {
        let m = e.get("macro").filter(|m| m.is_truthy())?;
        let mut c = ControlBinding::new("fire-macro", "");
        if let Some(name) = m.as_str().filter(|n| !n.is_empty()) {
            c.macro_name = Some(name.into());
        }
        if let Some(bars) = e
            .get("bars")
            .and_then(JsonValue::as_f64)
            .filter(|b| *b > 0.0)
        {
            c.macro_bars = Some(bars);
        }
        Some(c)
    });
}

/// `resolveCurvePoints` and the control bundle built from its points. An entry whose
/// `section` names an `Object.prototype` property resolves, in JS, to the inherited
/// function: the point is kept (so the net is built) but its tick never matches a
/// transition, so it adds no control.
pub fn inject_curve(
    proj: &mut Project,
    tmpl: &SongTemplate,
    curve: &[JsonValue],
    net: &str,
    prefixes: (&str, &str),
    value: impl Fn(&JsonValue) -> Option<ControlBinding>,
) {
    let (starts, total) = section_starts(tmpl);
    if total == 0 {
        return;
    }
    let mut points: Vec<(Option<i64>, ControlBinding)> = Vec::new();
    for e in curve {
        let Some(section) = e.get("section").and_then(JsonValue::as_str) else {
            continue;
        };
        let tick = if OBJECT_PROTO_KEYS.contains(&section) {
            None
        } else if let Some(&t) = starts.get(section) {
            Some(t)
        } else {
            continue;
        };
        let Some(c) = value(e) else { continue };
        points.push((tick, c));
    }
    if points.is_empty() {
        return;
    }
    let mut timed: Vec<(i64, ControlBinding)> = points
        .into_iter()
        .filter_map(|(t, c)| t.map(|t| (t, c)))
        .collect();
    timed.sort_by_key(|p| p.0);
    let mut ctrl = BTreeMap::new();
    for (tick, c) in timed {
        ctrl.insert(tick, c);
    }
    proj.nets.insert(
        net,
        build_control_bundle(total, &ctrl, false, prefixes.0, prefixes.1),
    );
}
