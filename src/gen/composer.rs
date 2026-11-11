//! Port of `Generator/Composer.swift` (generator/composer.js): the orchestrator.
//! Given a genre and the envelope's overrides (seed, structure, cohesion, ...),
//! builds the same Project beats does.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::gen::core::{drum_seed, Accent, GenParams, MAJOR_INTERVALS};
use crate::gen::euclidean::{drum_riff, euclidean, euclidean_melodic, melody_riff};
use crate::gen::markov::markov_melody;
use crate::gen::structure::{
    default_phrases, drum_break, fade_in, fade_out, generate_structure, genre_phrases,
    inject_feel_curve, inject_macro_curve, is_stinger_id, song_structure, FeelPoint, SongTemplate,
};
use crate::gen::tables::{BeatsTables, ChordProg, GenreDef, OrderedFlags};
use crate::gen::techniques::{
    apply_layer_budget, follow_bass_ring, hook_mode, render_hook, snap_to_chords,
    technique_context, thin_cell, ComposeTechniques, TechniqueContext,
};
use crate::gen::theme::{
    acid_bass_ring, octave_bounce_bass_ring,
    build_track_theme, chord_bass_ring, chord_bossa_bass_ring, chord_pad_net,
    chord_walking_bass_ring, cohesion_genre_supported, groove, groove_lock, kick_hit_mask,
    motif_mode, motif_net, render_motif, TrackTheme,
};
use crate::gen::variety::{
    add_chorus, apply_modal_interchange, call_response_melody, drum_fill_net, ghost_note_hihat,
    tension_for_variant, walking_bass_line,
};
use crate::json::{floor, js_round, js_to_int32, JsonObject, JsonValue};
use crate::pflow::{OrderedMap, Project};
use crate::rng::Mulberry32;

/// `generateTrackName(genre, seed)`
pub fn generate_track_name(genre: &str, seed: f64) -> String {
    let t = BeatsTables::shared();
    let mut rng = Mulberry32::new(js_to_int32(seed) as f64);
    let adj = &t.adjectives[rng.next_int(t.adjectives.len())];
    let noun = &t.nouns[rng.next_int(t.nouns.len())];
    format!("{genre} \u{00b7} {adj} {noun}")
}

const DRUM_ROLES: [&str; 4] = ["kick", "snare", "hihat", "clap"];

fn group_for(key: &str) -> Option<&'static str> {
    match key {
        "kick" | "snare" | "hihat" | "clap" | "tom" | "perc" => Some("drums"),
        "bass" => Some("bass"),
        "melody" | "lead" => Some("melody"),
        "arp" => Some("arp"),
        "pad" | "chord" | "chords" | "harmony" => Some("harmony"),
        k if is_stinger_id(k) => Some("stinger"),
        _ => None,
    }
}

/// `fallbackInstrumentSet(groupOrRole, currentInstrument)`
pub fn fallback_instrument_set(group_or_role: &str, current: &str) -> Vec<String> {
    let t = BeatsTables::shared();
    if group_or_role == "stinger" {
        return t.stinger_instrument_set.clone();
    }
    if let Some(set) = t.fallback_set(group_or_role).filter(|s| s.len() > 1) {
        if !current.is_empty() && !set.iter().any(|s| s == current) {
            let mut out = alloc::vec![current.to_string()];
            out.extend(set);
            return out;
        }
        return set;
    }
    let mut out: Vec<String> = Vec::new();
    let first = if current.is_empty() { "piano" } else { current };
    for v in [
        first,
        "piano",
        "electric-piano",
        "bass",
        "sub-bass",
        "lead",
        "sync-lead",
        "pad",
        "warm-pad",
        "strings",
        "organ",
        "fm-bell",
        "marimba",
    ] {
        if !v.is_empty() && !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
    }
    out
}

fn str_values(v: &[String]) -> Vec<JsonValue> {
    v.iter().map(|s| JsonValue::String(s.clone())).collect()
}

fn ensure_group_and_instrument_set(proj: &mut Project) {
    let ids: Vec<String> = proj.nets.keys().to_vec();
    for id in ids {
        let nb = proj.nets.get_mut(&id).unwrap();
        if nb.role == "control" {
            continue;
        }
        let key = if nb.riff_group.is_empty() {
            id.clone()
        } else {
            nb.riff_group.clone()
        };
        if nb.track.group.as_deref().is_none_or(str::is_empty) {
            if let Some(g) = group_for(&key) {
                nb.track.group = Some(g.into());
            }
        }
        if nb.track.instrument_set.len() < 2 {
            let gk = nb.track.group.clone().unwrap_or(key);
            let set = fallback_instrument_set(&gk, &nb.track.instrument);
            nb.track.instrument_set = str_values(&set);
            if nb.track.instrument.is_empty() && !set.is_empty() {
                nb.track.instrument = set[0].clone();
            }
        }
    }
}

fn add_stinger_tracks(proj: &mut Project, seed: f64) {
    let t = BeatsTables::shared();
    for spec in &t.stinger_specs {
        if proj.nets.contains_key(&spec.id) {
            continue;
        }
        let p = GenParams {
            channel: spec.channel,
            velocity: 95,
            duration: 80,
            seed,
            ..GenParams::default()
        };
        let mut nb = euclidean(4, 16, 0, 60, &p);
        nb.track.instrument = spec.default_instrument.clone();
        nb.track.instrument_set = str_values(&t.stinger_instrument_set);
        nb.track.group = Some("stinger".into());
        proj.nets.insert(spec.id.clone(), nb);
        if !proj.initial_mutes.contains(&spec.id) {
            proj.initial_mutes.push(spec.id.clone());
        }
    }
}

fn truncated(v: Vec<i64>, n: usize) -> Vec<i64> {
    if v.len() > n {
        v[..n].to_vec()
    } else {
        v
    }
}

fn expand_variants(
    proj: &mut Project,
    tmpl: &mut SongTemplate,
    genre: &GenreDef,
    rng: &mut Mulberry32,
    tension_curve: bool,
    theme: Option<&TrackTheme>,
    tech: Option<&TechniqueContext>,
) {
    let mut roles_in_phrases = OrderedFlags::new();
    for sec in &tmpl.sections {
        for role in sec.phrases.keys() {
            roles_in_phrases.set(role);
        }
    }
    let a = alloc::vec![String::from("A")];

    for role in roles_in_phrases.keys() {
        let Some(base) = proj.nets.get(role) else {
            continue;
        };
        let base_track = base.track.clone();
        let is_drum = DRUM_ROLES.contains(&role.as_str());

        let mut slot_map: Vec<Vec<i64>> = Vec::new();
        let mut slot_idx = 0i64;
        let mut total_slots = 0;
        let mut letter_slots: BTreeMap<String, i64> = BTreeMap::new();
        for sec in &tmpl.sections {
            let phrases = sec.phrases.get(role).unwrap_or(&a);
            let mut section_slots = Vec::new();
            if sec.active.contains(role) {
                for letter in phrases {
                    if is_drum {
                        if let Some(&s) = letter_slots.get(letter) {
                            section_slots.push(s);
                        } else {
                            letter_slots.insert(letter.clone(), slot_idx);
                            section_slots.push(slot_idx);
                            slot_idx += 1;
                            total_slots += 1;
                        }
                    } else {
                        section_slots.push(slot_idx);
                        slot_idx += 1;
                        total_slots += 1;
                    }
                }
            } else {
                section_slots = alloc::vec![-1; phrases.len()];
            }
            slot_map.push(section_slots);
        }
        tmpl.slot_map.insert(role.as_str(), slot_map.clone());
        if total_slots <= 1 {
            continue;
        }

        for (si, sec) in tmpl.sections.iter().enumerate() {
            let phrases = sec.phrases.get(role).unwrap_or(&a);
            for (pi, letter) in phrases.iter().enumerate() {
                let idx = slot_map[si][pi];
                if idx < 0 {
                    continue;
                }
                let slot_net_id = format!("{role}-{idx}");
                if proj.nets.contains_key(&slot_net_id) {
                    continue;
                }
                let mut nb = if is_drum {
                    let spec = match role.as_str() {
                        "kick" => &genre.kick,
                        "hihat" => &genre.hihat,
                        _ => &genre.snare,
                    };
                    let p = GenParams {
                        channel: base_track.channel,
                        velocity: base_track.default_velocity,
                        duration: 50,
                        seed: drum_seed(&format!("{}:{}:{}", genre.name, role, letter)),
                        accent: match role.as_str() {
                            "kick" => Accent::Kick,
                            "snare" => Accent::Snare,
                            "hihat" => Accent::Hihat,
                            _ => Accent::None,
                        },
                        ..GenParams::default()
                    };
                    let style = genre
                        .theory
                        .as_ref()
                        .and_then(|t| t.drum_styles.get(role.as_str()))
                        .and_then(|m| m.get(letter.as_str()));
                    drum_riff(letter, spec[0], spec[1], spec[2], spec[3], &p, style)
                } else {
                    let (mut scale, mut root_note, mut density, duration);
                    if role == "bass" {
                        scale = truncated(genre.scale(genre.root_note), MAJOR_INTERVALS.len());
                        root_note = genre.root_note;
                        density = genre.bass_density;
                        duration = genre.bass_duration;
                    } else {
                        root_note = genre.root_note + 12;
                        scale = genre.scale(root_note);
                        density = genre.melody_density;
                        duration = genre.melody_duration;
                    }
                    let mut vel = base_track.default_velocity;
                    // bassFollow: every bass slot plays the same pattern (no per-letter
                    // tension).
                    let follow = role == "bass" && tech.is_some_and(|t| t.t.bass_follow);
                    if tension_curve && !follow {
                        let tension = tension_for_variant(letter);
                        density *= tension.density_mul;
                        if density > 1.0 {
                            density = 1.0;
                        }
                        vel += tension.velocity_add;
                        // keyLock: the shift is in semitones and moves the scale's root (a B
                        // phrase plays a tone higher, in another key); with it the phrase
                        // stays in key.
                        if !tech.is_some_and(|t| t.t.key_lock) {
                            root_note += tension.register_shift;
                            scale = genre.scale(root_note);
                            if role == "bass" {
                                scale = truncated(scale, MAJOR_INTERVALS.len());
                            }
                        }
                    }
                    let p = GenParams {
                        scale: scale.clone(),
                        root_note,
                        channel: base_track.channel,
                        velocity: vel,
                        duration,
                        density,
                        seed: rng.next_int63() as f64,
                        duration_variation: genre.duration_variation,
                        ..GenParams::default()
                    };
                    let hook = tech.and_then(|t| t.hook.as_ref());
                    let arp_mode = || {
                        theme?
                            .energy
                            .get(&sec.name)?
                            .roles
                            .get(role)?
                            .motif_mode
                            .filter(|m| *m != motif_mode::IGNORE)
                    };
                    if let (Some(tech), Some(hook), "melody") = (tech, hook, role.as_str()) {
                        let cell = render_hook(hook, hook_mode(&sec.name), &tech.plan);
                        motif_net(&cell, &tech.melody_scale, &p)
                    } else if let (Some(tech), Some(hook), "arp", Some(mode)) =
                        (tech, hook, role.as_str(), arp_mode())
                    {
                        motif_net(&render_hook(hook, mode, &tech.plan), &tech.melody_scale, &p)
                    } else if let (true, None, Some(tech)) = (follow, theme, tech) {
                        let mut fp = p.clone();
                        fp.seed = tech.bass_seed;
                        follow_bass_ring(&tech.plan, &tech.bass_scale, &fp)
                    } else {
                        match theme {
                            Some(theme) => {
                                let role_prof = if follow {
                                    None
                                } else {
                                    theme.energy.get(&sec.name).and_then(|p| p.roles.get(role))
                                };
                                let mode = role_prof.and_then(|r| r.motif_mode);
                                let shift = role_prof.and_then(|r| r.register_shift).unwrap_or(0);
                                let harmonic_offset = theme.harmonic_offset(&sec.name);
                                if role == "bass" {
                                    if theme.groove == groove::WALKING {
                                        chord_walking_bass_ring(&theme.plan, &scale, &p, shift)
                                    } else if theme.groove == groove::BOSSA {
                                        chord_bossa_bass_ring(
                                            &theme.plan,
                                            &scale,
                                            &p,
                                            shift,
                                            genre.bpm,
                                        )
                                    } else {
                                        let mask = groove_lock(&kick_hit_mask(genre), theme.groove);
                                        chord_bass_ring(&theme.plan, &mask, &scale, &p, shift)
                                    }
                                } else if role == "melody" || role == "arp" {
                                    match mode {
                                        None | Some(motif_mode::IGNORE) => melody_riff(letter, &p),
                                        Some(mode) => {
                                            let mut cell =
                                                render_motif(&theme.motif, mode, harmonic_offset);
                                            if role == "melody" {
                                                if let Some(d) =
                                                    tech.and_then(|t| t.t.melody_density)
                                                {
                                                    cell = thin_cell(&cell, d);
                                                }
                                            }
                                            motif_net(&cell, &scale, &p)
                                        }
                                    }
                                } else {
                                    melody_riff(letter, &p)
                                }
                            }
                            None => melody_riff(letter, &p),
                        }
                    }
                };
                if let Some(tech) = tech {
                    if role == "melody" || (role == "arp" && tech.t.chord_snap_arp) {
                        snap_to_chords(&mut nb, tech, tech.t.chord_snap);
                    }
                }
                nb.riff_group = role.clone();
                nb.riff_variant = letter.clone();
                nb.track = base_track.clone();
                proj.nets.insert(slot_net_id, nb);
            }
        }
        proj.nets.remove(role);
    }
}

/// `compose(genreName, overrides)`. `overrides` is the JS overrides object. Unlike
/// the JS (which falls back to the wall clock), a missing numeric `seed` composes
/// with seed 0: the engine has no clock. Hosts that want a fresh track pick a seed.
pub fn compose(genre_name: &str, ov: &JsonValue) -> Project {
    let tables = BeatsTables::shared();
    let base_genre = tables
        .genres
        .get(genre_name)
        .unwrap_or_else(|| &tables.genres["techno"]);
    let get = |k: &str| ov.get(k);
    // melodyDensity / bassDensity: numeric overrides of the genre table (dj-only)
    let genre_ov: GenreDef;
    let genre: &GenreDef = if get("melodyDensity").and_then(JsonValue::as_f64).is_some()
        || get("bassDensity").and_then(JsonValue::as_f64).is_some()
    {
        let mut g = base_genre.clone();
        if let Some(v) = get("melodyDensity").and_then(JsonValue::as_f64) {
            g.melody_density = v;
        }
        if let Some(v) = get("bassDensity").and_then(JsonValue::as_f64) {
            g.bass_density = v;
        }
        genre_ov = g;
        &genre_ov
    } else {
        base_genre
    };
    let tech = ComposeTechniques::from_json(ov);

    let seed = get("seed").and_then(JsonValue::as_f64).unwrap_or(0.0);
    let mut rng = Mulberry32::new(seed);
    let bpm = get("bpm").and_then(JsonValue::as_f64).unwrap_or(genre.bpm);

    let mut df = get("drum-fills")
        .and_then(JsonValue::as_bool)
        .unwrap_or(genre.drum_fills);
    let wb = get("walking-bass")
        .and_then(JsonValue::as_bool)
        .unwrap_or(genre.walking_bass);
    let mut poly_steps = genre.polyrhythm;
    if let Some(v) = get("polyrhythm").and_then(JsonValue::as_f64) {
        poly_steps = v as i64;
    }
    let sync = get("syncopation")
        .and_then(JsonValue::as_f64)
        .unwrap_or(genre.syncopation);
    let cr = get("call-response")
        .and_then(JsonValue::as_bool)
        .unwrap_or(genre.call_response);
    let tc = get("tension-curve")
        .and_then(JsonValue::as_bool)
        .unwrap_or(genre.tension_curve);
    let mi = get("modal-interchange")
        .and_then(JsonValue::as_f64)
        .unwrap_or(genre.modal_interchange);
    let gn = get("ghost-notes")
        .and_then(JsonValue::as_f64)
        .unwrap_or(genre.ghost_notes);

    let cohesion = get("cohesion")
        .and_then(JsonValue::as_str)
        .filter(|c| !c.is_empty())
        .unwrap_or("v2");
    let theme = (cohesion == "v2" && cohesion_genre_supported(genre_name))
        .then(|| build_track_theme(genre_name, seed));
    let ctx = technique_context(&tech, genre, genre_name, seed, theme.as_ref());

    let mut proj = Project {
        name: generate_track_name(&genre.name, seed),
        tempo: bpm,
        swing: genre.swing,
        humanize: get("humanize")
            .and_then(JsonValue::as_f64)
            .unwrap_or(genre.humanize),
        root_note: Some(genre.root_note),
        scale_name: genre
            .scale_name
            .strip_suffix("Scale")
            .unwrap_or(&genre.scale_name)
            .to_string(),
        ..Project::default()
    };
    let kick_steps = if genre.kick.len() > 1 && genre.kick[1] != 0 {
        genre.kick[1]
    } else {
        16
    };
    proj.bars = Some((js_round(kick_steps as f64 / 16.0) as i64).max(1));
    if theme.is_some() {
        proj.cohesion = Some("v2".into());
    }

    // === Drums ===
    let d_seed = drum_seed(genre_name);
    let kick_p = GenParams {
        channel: 10,
        velocity: 100,
        duration: 50,
        seed: d_seed,
        accent: Accent::Kick,
        ..GenParams::default()
    };
    let k = &genre.kick;
    proj.nets
        .insert("kick", euclidean(k[0], k[1], k[2], k[3], &kick_p));

    let snare_p = GenParams {
        channel: 11,
        seed: d_seed + 1.0,
        accent: Accent::Snare,
        ..kick_p.clone()
    };
    let sn = &genre.snare;
    proj.nets
        .insert("snare", euclidean(sn[0], sn[1], sn[2], sn[3], &snare_p));

    let h = &genre.hihat;
    let (mut hihat_hits, mut hihat_steps) = (h[0], h[1]);
    if poly_steps > 0 {
        hihat_steps = poly_steps;
        hihat_hits = (floor((hihat_hits * poly_steps) as f64 / h[1] as f64) as i64).max(1);
    }
    let hihat_p = GenParams {
        channel: 12,
        seed: d_seed + 2.0,
        accent: Accent::Hihat,
        ..kick_p.clone()
    };
    let hihat = if gn > 0.0 {
        ghost_note_hihat(hihat_hits, hihat_steps, h[2], h[3], &hihat_p, gn)
    } else {
        euclidean(hihat_hits, hihat_steps, h[2], h[3], &hihat_p)
    };
    proj.nets.insert("hihat", hihat);

    // Chord progression
    let mut chord_prog: ChordProg = match genre.theory.as_ref().map(|t| &t.chord_progs) {
        Some(progs) if !progs.is_empty() => progs[rng.next_int(progs.len())].clone(),
        _ => {
            let test = genre.scale(60);
            if test.len() >= 7 && test[2] - test[0] == 4 {
                tables.major_chord_prog.clone()
            } else {
                tables.minor_chord_prog.clone()
            }
        }
    };
    if mi > 0.0 {
        chord_prog = apply_modal_interchange(&chord_prog, &genre.scale(60), mi, &mut rng);
    }

    // Bass
    let bass_scale = truncated(genre.scale(genre.root_note), MAJOR_INTERVALS.len());
    let bass_p = GenParams {
        scale: bass_scale.clone(),
        root_note: genre.root_note,
        channel: genre.bass_channel,
        velocity: 90,
        duration: genre.bass_duration,
        density: genre.bass_density,
        seed: rng.next_int63() as f64,
        chords: Some(chord_prog.clone()),
        duration_variation: genre.duration_variation,
        syncopation: sync,
        ..GenParams::default()
    };
    let bass = match &theme {
        Some(theme) if theme.groove == groove::WALKING => {
            chord_walking_bass_ring(&theme.plan, &bass_scale, &bass_p, 0)
        }
        Some(theme) if theme.groove == groove::BOSSA => {
            chord_bossa_bass_ring(&theme.plan, &bass_scale, &bass_p, 0, genre.bpm)
        }
        // native genres' own figures (genres-native.json)
        Some(theme) if genre_name == "italo" => {
            octave_bounce_bass_ring(&theme.plan, &bass_scale, &bass_p)
        }
        Some(theme) if genre_name == "acid" => acid_bass_ring(&theme.plan, &bass_scale, &bass_p),
        Some(theme) => {
            let mask = groove_lock(&kick_hit_mask(genre), theme.groove);
            chord_bass_ring(&theme.plan, &mask, &bass_scale, &bass_p, 0)
        }
        None if ctx.is_some() && tech.bass_follow => {
            let ctx = ctx.as_ref().unwrap();
            let mut p = bass_p.clone();
            p.seed = ctx.bass_seed;
            follow_bass_ring(&ctx.plan, &ctx.bass_scale, &p)
        }
        None if wb => walking_bass_line(&bass_p),
        None => markov_melody(&bass_p),
    };
    proj.nets.insert("bass", bass);

    // Melody
    let melody_root = genre.root_note + 12;
    let melody_p = GenParams {
        scale: genre.scale(melody_root),
        root_note: melody_root,
        channel: genre.melody_channel,
        velocity: 85,
        duration: genre.melody_duration,
        density: genre.melody_density,
        seed: rng.next_int63() as f64,
        chords: Some(chord_prog),
        duration_variation: genre.duration_variation,
        syncopation: sync,
        ..GenParams::default()
    };
    if let Some(theme) = &theme {
        let motif = match tech.melody_density {
            Some(d) => thin_cell(&theme.motif, d),
            None => theme.motif.clone(),
        };
        proj.nets
            .insert("melody", motif_net(&motif, &melody_p.scale, &melody_p));
        let bar_ms = floor(4.0 * 60000.0 / bpm * 0.95) as i64;
        let pad_p = GenParams {
            channel: 7,
            velocity: 68,
            duration: bar_ms,
            ..GenParams::default()
        };
        let mut pad = chord_pad_net(&theme.plan, &genre.scale(genre.root_note), &pad_p);
        pad.track.instrument = if genre_name == "synthwave" {
            "warm-pad"
        } else {
            "dark-pad"
        }
        .into();
        proj.nets.insert("harmony", pad);
    } else if cr {
        proj.nets.insert("melody", call_response_melody(&melody_p));
    } else {
        proj.nets.insert("melody", markov_melody(&melody_p));
    }
    if let Some(ctx) = &ctx {
        if let Some(hook) = &ctx.hook {
            proj.nets.insert(
                "melody",
                motif_net(
                    &render_hook(hook, motif_mode::PLAY, &ctx.plan),
                    &ctx.melody_scale,
                    &melody_p,
                ),
            );
        }
        snap_to_chords(proj.nets.get_mut("melody").unwrap(), ctx, tech.chord_snap);
    }

    // Genre-specific extras (a native genre takes its `like`'s: electro edm's, italo synthwave's)
    let kin = tables.kin(genre_name);
    if matches!(kin, "edm" | "synthwave" | "trance") {
        if kin == "edm" {
            let clap_p = GenParams {
                channel: 13,
                velocity: 100,
                duration: 50,
                seed: d_seed + 3.0,
                accent: Accent::Snare,
                ..GenParams::default()
            };
            proj.nets.insert("clap", euclidean(2, 16, 4, 39, &clap_p));
        }
        let arp_scale = truncated(genre.scale(genre.root_note + 24), 5);
        let arp_p = GenParams {
            channel: 5,
            velocity: 80,
            duration: 60,
            ..GenParams::default()
        };
        let arp_seed = rng.next_int63() as f64;
        proj.nets
            .insert("arp", euclidean_melodic(&arp_scale, 16, arp_seed, &arp_p));
        if let Some(ctx) = &ctx {
            if tech.chord_snap_arp {
                snap_to_chords(proj.nets.get_mut("arp").unwrap(), ctx, tech.chord_snap);
            }
        }
    }

    // Assign instruments
    if let Some(sets) = tables.instrument_set(genre_name) {
        let mut role_instrument: BTreeMap<&str, String> = BTreeMap::new();
        for (role, instruments) in sets.iter() {
            role_instrument.insert(role, instruments[rng.next_int(instruments.len())].clone());
        }
        let ids: Vec<String> = proj.nets.keys().to_vec();
        for id in ids {
            let nb = proj.nets.get_mut(&id).unwrap();
            let lookup = if nb.riff_group.is_empty() {
                id.clone()
            } else {
                nb.riff_group.clone()
            };
            if let Some(instruments) = sets.get(&lookup).filter(|i| !i.is_empty()) {
                nb.track.instrument_set = str_values(instruments);
                nb.track.instrument = role_instrument
                    .get(lookup.as_str())
                    .cloned()
                    .unwrap_or_else(|| instruments[0].clone());
            }
        }
    }

    // Tag every music net with its mixer group + guarantee an instrumentSet of ≥ 2.
    let ids: Vec<String> = proj.nets.keys().to_vec();
    for id in &ids {
        let nb = proj.nets.get_mut(id).unwrap();
        if nb.role == "control" {
            continue;
        }
        let key = if nb.riff_group.is_empty() {
            id.clone()
        } else {
            nb.riff_group.clone()
        };
        let group = group_for(&key);
        if let Some(g) = group {
            nb.track.group = Some(g.into());
        }
        if nb.track.instrument_set.len() < 2 {
            let set = fallback_instrument_set(group.unwrap_or(&key), &nb.track.instrument);
            nb.track.instrument_set = str_values(&set);
            if nb.track.instrument.is_empty() {
                nb.track.instrument = set[0].clone();
            }
        }
    }

    // Override instruments
    if let Some(inst) = get("instruments").and_then(JsonValue::as_object) {
        for id in &ids {
            let nb = proj.nets.get_mut(id).unwrap();
            let mut name = inst.get(id);
            if name.is_none_or(|n| !n.is_truthy()) && !nb.riff_group.is_empty() {
                name = inst.get(&nb.riff_group);
            }
            if let Some(n) = name.and_then(JsonValue::as_str).filter(|n| !n.is_empty()) {
                nb.track.instrument = n.into();
            }
        }
    }

    // Chorus
    if get("chorus").and_then(JsonValue::as_bool) == Some(true) {
        add_chorus(&mut proj, genre, &mut rng);
    }

    // Structure mode
    if let Some(struct_name) = get("structure")
        .and_then(JsonValue::as_str)
        .filter(|s| !s.is_empty())
    {
        let mut tmpl = generate_structure(genre_name, struct_name, &mut rng);

        if let Some(theme) = &theme {
            for sec in tmpl.sections.iter_mut() {
                if let Some(prof) = theme.energy.get(&sec.name).filter(|p| !p.roles.is_empty()) {
                    sec.active = OrderedFlags::from_names(
                        prof.roles.iter().filter(|(_, r)| r.active).map(|(k, _)| k),
                    );
                    let pattern = default_phrases(&sec.name);
                    let mut phrases = OrderedMap::new();
                    for role in sec.active.keys() {
                        if role != "snare" && role != "harmony" {
                            phrases.insert(role.as_str(), pattern.clone());
                        }
                    }
                    sec.phrases = phrases;
                }
            }
        }
        for sec in tmpl.sections.iter_mut() {
            let roles: Vec<String> = sec.phrases.keys().to_vec();
            for role in roles {
                sec.phrases
                    .insert(role, genre_phrases(genre.theory.as_ref(), &sec.name));
            }
        }
        if theme.is_some() {
            df = true;
        }

        expand_variants(
            &mut proj,
            &mut tmpl,
            genre,
            &mut rng,
            tc,
            theme.as_ref(),
            ctx.as_ref(),
        );
        if let Some(budget) = tech.layer_budget {
            apply_layer_budget(&mut tmpl, budget);
        }

        if df {
            let mut pos = 0;
            let mut fill_idx = 0;
            for sec in &tmpl.sections {
                pos += sec.steps;
                if pos > 4 {
                    let fill_len = if sec.steps >= 128 { 8 } else { 4 };
                    proj.nets
                        .insert(format!("fill-{fill_idx}"), drum_fill_net(pos, fill_len));
                    fill_idx += 1;
                }
            }
        }

        let music_nets: Vec<String> = proj.nets.keys().to_vec();
        proj.initial_mutes = song_structure(&mut proj, &tmpl, &music_nets);
        let total_steps: i64 = proj.structure.iter().map(|s| s.steps).sum();
        if total_steps > 0 {
            proj.bars = Some((js_round(total_steps as f64 / 16.0) as i64).max(1));
        }

        if let Some(theme) = &theme {
            let feel: Vec<FeelPoint> = tmpl
                .sections
                .iter()
                .filter_map(|sec| {
                    theme.energy.get(&sec.name).map(|p| FeelPoint {
                        section: sec.name.clone(),
                        x: p.energy,
                        y: p.filter_open,
                    })
                })
                .collect();
            if !feel.is_empty() {
                inject_feel_curve(&mut proj, &tmpl, &feel);
            }
        }
        if let Some(mc) = get("macroCurve")
            .and_then(JsonValue::as_array)
            .filter(|a| !a.is_empty())
        {
            inject_macro_curve(&mut proj, &tmpl, mc);
        }

        let s = rng.next_int63() as f64;
        add_stinger_tracks(&mut proj, s);
        ensure_group_and_instrument_set(&mut proj);
        proj.stamp_duration_steps();
        return proj;
    }

    // Loop mode: fade-in/out/break
    let mut melodic: Vec<String> = alloc::vec!["bass".into(), "melody".into()];
    if proj.nets.contains_key("arp") {
        melodic.push("arp".into());
    }
    if proj.nets.contains_key("harmony") {
        melodic.push("harmony".into());
    }
    if get("fade-in").and_then(JsonValue::as_bool) == Some(true) {
        let _ = rng.next_int63();
        proj.initial_mutes = fade_in(&mut proj, &melodic, 32);
    }
    if get("fade-out").and_then(JsonValue::as_bool) == Some(true) {
        let _ = rng.next_int63();
        fade_out(&mut proj, &melodic, 32);
    }
    if get("drum-break").and_then(JsonValue::as_bool) == Some(true) {
        let mut targets: Vec<String> = alloc::vec!["bass".into(), "melody".into()];
        if proj.nets.contains_key("harmony") {
            targets.push("harmony".into());
        }
        let _ = rng.next_int63();
        drum_break(&mut proj, &targets, 64, 8);
    }

    let s = rng.next_int63() as f64;
    add_stinger_tracks(&mut proj, s);
    ensure_group_and_instrument_set(&mut proj);
    proj.stamp_duration_steps();
    proj
}

/// Convenience: `compose` with an overrides object built from pairs.
pub fn compose_with(genre: &str, pairs: &[(&str, JsonValue)]) -> Project {
    let mut o = JsonObject::new();
    for (k, v) in pairs {
        o.set(*k, v.clone());
    }
    compose(genre, &JsonValue::Object(o))
}
