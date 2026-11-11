//! Port of `Generator/Techniques.swift`: dj-only composition techniques, opt-in `compose`
//! overrides that beats does not have (COMPOSE-OPTIONS.md). Every one is off unless its key
//! is in the overrides, and with none of them `compose` takes exactly the beats path (the
//! parity fixtures run that path). They never draw from compose's own rng: switching one
//! on changes only what it is about.
//!
//! No oracle: beats has none of these, and the Swift fixtures cover the default path only.
//! The tests in `tests/techniques.rs` hold the literal expectations of the Swift tests.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::GenParams;
use crate::gen::markov::compose_sequence;
use crate::gen::structure::SongTemplate;
use crate::gen::tables::{ChordDegree, GenreDef, OrderedFlags};
use crate::gen::theme::{
    build_track_theme, chord_at, clamp_degree, cycle_steps, degree_to_midi, hash_str, motif_mode,
    noted_ring, ChordPlan, MotifCell, TrackTheme,
};
use crate::json::{floor, js_to_int32, JsonValue};
use crate::pflow::NetBundle;
use crate::rng::Mulberry32;

const MAJOR_LEN: usize = 7;

/// `chordSnap`: which melody notes are moved onto the sounding chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordSnap {
    Off,
    Strong,
    All,
}

impl ChordSnap {
    /// strong = beats 1 and 3 of each bar; all = every beat. (16th-note steps.)
    pub fn applies(self, step: i64) -> bool {
        match self {
            ChordSnap::Off => false,
            ChordSnap::Strong => step % 8 == 0,
            ChordSnap::All => step % 4 == 0,
        }
    }
}

/// The techniques an overrides object asks for.
#[derive(Clone, Debug)]
pub struct ComposeTechniques {
    pub hook: bool,
    pub hook_bars: i64,
    pub chord_snap: ChordSnap,
    pub chord_snap_arp: bool,
    /// None = off; Some(0) = auto (per section name); Some(n) = at most n melodic roles
    /// everywhere.
    pub layer_budget: Option<i64>,
    pub bass_follow: bool,
    pub key_lock: bool,
    /// `melodyDensity` when it was given (it also thins motif cells, which ignore density).
    pub melody_density: Option<f64>,
}

impl ComposeTechniques {
    pub fn from_json(ov: &JsonValue) -> Self {
        let mut t = ComposeTechniques {
            hook: ov.get("hook").and_then(JsonValue::as_bool).unwrap_or(false),
            hook_bars: 2,
            chord_snap: ChordSnap::Off,
            chord_snap_arp: ov
                .get("chordSnapArp")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            layer_budget: None,
            bass_follow: ov
                .get("bassFollow")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            key_lock: ov
                .get("keyLock")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            melody_density: ov.get("melodyDensity").and_then(JsonValue::as_f64),
        };
        if let Some(b) = ov.get("hookBars").and_then(JsonValue::as_f64) {
            if b == 1.0 || b == 2.0 {
                t.hook_bars = b as i64;
            }
        }
        if let Some(s) = ov.get("chordSnap").and_then(JsonValue::as_str) {
            t.chord_snap = match s {
                "strong" => ChordSnap::Strong,
                "all" => ChordSnap::All,
                _ => ChordSnap::Off,
            };
        }
        match ov.get("layerBudget") {
            Some(JsonValue::String(s)) if s == "auto" => t.layer_budget = Some(0),
            Some(v) => {
                if let Some(n) = v.as_f64().filter(|n| *n >= 1.0) {
                    t.layer_budget = Some(n as i64);
                }
            }
            None => {}
        }
        t
    }

    /// Whether any technique acts while slots are composed (they share one context, whose
    /// chord plan is built as cohesion v2 would when it is off).
    pub fn needs_context(&self) -> bool {
        self.hook || self.chord_snap != ChordSnap::Off || self.bass_follow || self.key_lock
    }
}

/// What `expandVariants` needs to apply the techniques to a slot.
#[derive(Clone, Debug)]
pub struct TechniqueContext {
    pub t: ComposeTechniques,
    pub plan: ChordPlan,
    pub hook: Option<HookCell>,
    /// The key's scale an octave up (the melody's), for hooks and snapping.
    pub melody_scale: Vec<i64>,
    pub bass_scale: Vec<i64>,
    /// The key's pitch classes per chord of `plan`.
    pub chord_pcs: Vec<Vec<i64>>,
    /// `bassFollow`'s one seed for every bass slot.
    pub bass_seed: f64,
}

pub fn technique_context(
    t: &ComposeTechniques,
    genre: &GenreDef,
    genre_name: &str,
    seed: f64,
    theme: Option<&TrackTheme>,
) -> Option<TechniqueContext> {
    if !t.needs_context() {
        return None;
    }
    let plan = match theme {
        Some(theme) => theme.plan.clone(),
        None => build_track_theme(genre_name, seed).plan,
    };
    let key = genre.scale(genre.root_note);
    let mut bass_scale = key.clone();
    if bass_scale.len() > MAJOR_LEN {
        bass_scale.truncate(MAJOR_LEN);
    }
    let density = t.melody_density.unwrap_or(genre.melody_density);
    Some(TechniqueContext {
        t: t.clone(),
        hook: t
            .hook
            .then(|| build_hook(genre_name, seed, t.hook_bars, density)),
        melody_scale: genre.scale(genre.root_note + 12),
        bass_scale,
        chord_pcs: plan
            .chords
            .iter()
            .map(|c| {
                c.tones
                    .iter()
                    .map(|t| (degree_to_midi(clamp_degree(*t), &key) % 12 + 12) % 12)
                    .collect()
            })
            .collect(),
        bass_seed: ((js_to_int32(seed) as u32)
            ^ hash_str(&alloc::format!("bass-follow/{genre_name}"))) as f64,
        plan,
    })
}

// ---------------------------------------------------------------------------
// hook

/// A 1–2 bar hook: a rhythm and a contour of chord-tone targets (scale degrees above the
/// melody root, measured on the tonic chord). Each bar of it is played against the chord
/// sounding there, every target moved to that chord's nearest tone, so the shape stays and
/// every note is a chord tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookCell {
    pub mask: Vec<bool>,
    /// Scale-degree target per step (-1 = rest).
    pub target: Vec<i64>,
}

impl HookCell {
    pub fn length(&self) -> usize {
        self.mask.len()
    }
}

/// Section → how the hook is varied there (the motif modes; Play where nothing is set).
pub fn hook_mode(section: &str) -> i64 {
    match section {
        "intro" => motif_mode::FRAGMENT,
        "verse" => motif_mode::PLAY,
        "pre-chorus" => motif_mode::FRAGMENT,
        "buildup" => motif_mode::TRANSPOSED,
        "drop" => motif_mode::PLAY,
        "chorus" => motif_mode::PLAY,
        "breakdown" => motif_mode::FRAGMENT,
        "bridge" => motif_mode::INVERTED,
        "solo" => motif_mode::AUGMENTED,
        "outro" => motif_mode::FRAGMENT,
        _ => motif_mode::PLAY,
    }
}

/// Pure function of genre + seed + bars + density; its own rng (never compose's).
pub fn build_hook(genre: &str, seed: f64, bars: i64, density: f64) -> HookCell {
    let u = js_to_int32(seed) as u32;
    let mut rng = Mulberry32::from_i32((u ^ hash_str(&alloc::format!("hook/{genre}"))) as i32);
    let bars = if bars == 1 { 1 } else { 2 };
    let len = (16 * bars) as usize;
    // Rhythm: the downbeat, then onsets drawn from a pop/EDM-weighted pool.
    let hits = ((crate::json::js_round(density * 8.0)) as i64).clamp(3, 7) as usize;
    fn rhythm(fixed: &[i64], hits: usize, rng: &mut Mulberry32) -> Vec<i64> {
        let mut pool: Vec<(i64, i64)> = alloc::vec![
            (3, 4),
            (6, 4),
            (8, 3),
            (10, 3),
            (12, 3),
            (14, 2),
            (4, 2),
            (2, 1),
            (11, 1),
            (7, 1)
        ];
        let mut on: Vec<i64> = fixed.to_vec();
        pool.retain(|p| !on.contains(&p.0));
        while on.len() < hits && !pool.is_empty() {
            let total: i64 = pool.iter().map(|p| p.1).sum();
            let mut r = rng.next_int(total as usize) as i64;
            let mut k = 0;
            while r >= pool[k].1 {
                r -= pool[k].1;
                k += 1;
            }
            on.push(pool.remove(k).0);
        }
        on.sort_unstable();
        on
    }
    let first = rhythm(&[0], hits, &mut rng);
    // Contour: chord-tone index walk (0 root, 1 third, 2 fifth, 3 octave, 4 tenth).
    let degree_of = [0i64, 2, 4, 7, 9];
    let moves = [-1i64, 1, 1, -1, 0, 2, -2];
    fn walk(n: usize, from: i64, moves: &[i64], rng: &mut Mulberry32) -> Vec<i64> {
        let mut idx = from;
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(idx);
            idx = (idx + moves[rng.next_int(moves.len())]).clamp(0, 4);
        }
        out
    }
    let mut mask = alloc::vec![false; len];
    let mut target = alloc::vec![-1i64; len];
    let start = rng.next_int(3) as i64;
    let a = walk(first.len(), start, &moves, &mut rng);
    for (k, s) in first.iter().enumerate() {
        mask[*s as usize] = true;
        target[*s as usize] = degree_of[a[k] as usize];
    }
    if bars == 2 {
        // Answer: the first half of the call again, a new second half that comes home.
        let keep: Vec<i64> = first.iter().copied().filter(|s| *s < 8).collect();
        let tail: Vec<i64> = rhythm(&keep, hits, &mut rng)
            .into_iter()
            .filter(|s| *s >= 8)
            .collect();
        let mut b = walk(tail.len(), a[keep.len().min(a.len() - 1)], &moves, &mut rng);
        if let Some(last) = b.last_mut() {
            *last = 0;
        }
        for (k, s) in keep.iter().enumerate() {
            mask[16 + *s as usize] = true;
            target[16 + *s as usize] = degree_of[a[k] as usize];
        }
        for (k, s) in tail.iter().enumerate() {
            mask[16 + *s as usize] = true;
            target[16 + *s as usize] = degree_of[b[k] as usize];
        }
    }
    HookCell { mask, target }
}

/// The hook as one section plays it (`mode` a motif mode), over `plan`: a cell as long as
/// the plan cycle (or a multiple), every note on the sounding chord. Ignore plays it as is.
pub fn render_hook(hook: &HookCell, mode: i64, plan: &ChordPlan) -> MotifCell {
    let cycle = cycle_steps(plan).max(16) as usize;
    let stretch = if mode == motif_mode::AUGMENTED { 2 } else { 1 };
    let span = hook.length() * stretch;
    let mut n = cycle;
    while n % span != 0 {
        n += cycle;
    }
    let mut degrees = alloc::vec![-1i64; n];
    let mut mask = alloc::vec![false; n];
    for i in 0..n {
        if i % stretch != 0 {
            continue;
        }
        let h = (i / stretch) % hook.length();
        if !hook.mask[h] {
            continue;
        }
        if mode == motif_mode::FRAGMENT && h >= hook.length() / 2 {
            continue;
        }
        let mut t = hook.target[h];
        if mode == motif_mode::TRANSPOSED {
            t += 2;
        }
        if mode == motif_mode::INVERTED {
            t = 9 - t;
        }
        degrees[i] = nearest_chord_degree(t, &chord_at(plan, i as i64));
        mask[i] = true;
    }
    MotifCell {
        degrees,
        mask,
        contour: 0,
    }
}

/// The tone of `chord` (any octave, degree ≥ 0) nearest to degree `t`; ties go down.
pub fn nearest_chord_degree(t: i64, chord: &ChordDegree) -> i64 {
    let mut best = -1i64;
    for tone in &chord.tones {
        let mut d = clamp_degree(*tone);
        while d <= t + 7 {
            if best < 0
                || (d - t).abs() < (best - t).abs()
                || ((d - t).abs() == (best - t).abs() && d < best)
            {
                best = d;
            }
            d += 7;
        }
    }
    best.max(0)
}

// ---------------------------------------------------------------------------
// chord snap

/// Copy a ring net out to `length` steps (a multiple of its own), bindings repeated.
pub fn tile_ring(nb: &NetBundle, length: i64) -> NetBundle {
    let n = nb.transitions.len() as i64;
    if n == 0 || length <= n || length % n != 0 {
        return nb.clone();
    }
    let mut out = NetBundle::ring(length);
    out.track = nb.track.clone();
    out.role = nb.role.clone();
    out.riff_group = nb.riff_group.clone();
    out.riff_variant = nb.riff_variant.clone();
    for i in 0..length {
        if let Some(b) = nb.bindings.get(&alloc::format!("t{}", i % n)) {
            out.bindings.insert(alloc::format!("t{i}"), *b);
        }
    }
    out.track.ring_size = Some(length as f64);
    out.track.beats = Some(out.bindings.len() as f64);
    out
}

pub fn is_plain_ring(nb: &NetBundle) -> bool {
    let n = nb.transitions.len();
    n > 0
        && nb.places.len() == n
        && nb.transitions.contains_key(&alloc::format!("t{}", n - 1))
        && nb.control_bindings.is_empty()
}

pub fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Move the notes on `mode`'s beats to the nearest pitch of the chord sounding there (the
/// ring is tiled out to the plan cycle first, so it can follow the chords).
pub fn snap_to_chords(nb: &mut NetBundle, ctx: &TechniqueContext, mode: ChordSnap) {
    if mode == ChordSnap::Off || !is_plain_ring(nb) || ctx.plan.chords.is_empty() {
        return;
    }
    let n = nb.transitions.len() as i64;
    let cycle = cycle_steps(&ctx.plan);
    let l = n / gcd(n, cycle) * cycle;
    if l != n {
        *nb = tile_ring(nb, l);
    }
    for i in 0..l {
        if !mode.applies(i) {
            continue;
        }
        let key = alloc::format!("t{i}");
        let Some(mut b) = nb.bindings.get(&key).copied() else {
            continue;
        };
        let pcs = &ctx.chord_pcs[((i / ctx.plan.steps_per_chord) as usize) % ctx.plan.chords.len()];
        b.note = nearest_pitch(b.note, pcs);
        nb.bindings.insert(key, b);
    }
}

/// The MIDI note with a pitch class in `pcs` nearest to `note`; ties go down.
pub fn nearest_pitch(note: i64, pcs: &[i64]) -> i64 {
    for d in 0..=6 {
        if note - d >= 0 && pcs.contains(&(((note - d) % 12 + 12) % 12)) {
            return note - d;
        }
        if note + d <= 127 && pcs.contains(&((note + d) % 12)) {
            return note + d;
        }
    }
    note
}

// ---------------------------------------------------------------------------
// melody density on motif cells

/// Keep at most `density × 16` notes per bar of a motif cell, strongest steps first.
pub fn thin_cell(cell: &MotifCell, density: f64) -> MotifCell {
    let keep = (floor(density * 16.0) as i64).max(1) as usize;
    let mut out = cell.clone();
    let mut bar = 0;
    while bar < cell.degrees.len() {
        let end = (bar + 16).min(cell.degrees.len());
        let on: Vec<usize> = (bar..end)
            .filter(|i| cell.mask[*i] && cell.degrees[*i] >= 0)
            .collect();
        if on.len() > keep {
            let weight = |i: usize| -> i64 {
                let s = i - bar;
                if s % 16 == 0 {
                    4
                } else if s % 8 == 0 {
                    3
                } else if s % 4 == 0 {
                    2
                } else if s % 2 == 0 {
                    1
                } else {
                    0
                }
            };
            let mut sorted = on.clone();
            // (weight, -index) descending: strongest first, earlier steps first on ties
            sorted.sort_by(|a, b| weight(*b).cmp(&weight(*a)).then(a.cmp(b)));
            let kept: BTreeSet<usize> = sorted.into_iter().take(keep).collect();
            for i in on {
                if !kept.contains(&i) {
                    out.mask[i] = false;
                    out.degrees[i] = -1;
                }
            }
        }
        bar = end;
    }
    out
}

// ---------------------------------------------------------------------------
// bass follow

/// One bass bar (a seeded Markov bass line, degrees from the chord root), played on every
/// chord of the plan: the same pattern each cycle, whatever the section.
pub fn follow_bass_ring(plan: &ChordPlan, scale: &[i64], params: &GenParams) -> NetBundle {
    let mut rng = Mulberry32::new(params.seed);
    let tones: BTreeSet<i64> = [0, 2, 4].into_iter().collect();
    let seq = compose_sequence(16, 7, &tones, true, params.density, &mut rng);
    let bars = plan.chords.len().max(1);
    let bar_steps = if plan.chords.is_empty() {
        16
    } else {
        plan.steps_per_chord as usize
    };
    let mut notes = alloc::vec![0i64; bars * bar_steps];
    let mut mask = alloc::vec![false; bars * bar_steps];
    for b in 0..bars {
        let root = if plan.chords.is_empty() {
            0
        } else {
            clamp_degree(chord_at(plan, (b * bar_steps) as i64).root)
        };
        for s in 0..bar_steps {
            if seq[s % 16] < 0 {
                continue;
            }
            mask[b * bar_steps + s] = true;
            let mut note = degree_to_midi(root + seq[s % 16], scale);
            // keep it in the bass register an octave span above the key's root
            if let Some(low) = scale.first() {
                if note >= low + 12 + 7 {
                    note -= 12;
                }
            }
            notes[b * bar_steps + s] = note;
        }
    }
    let mut nb = noted_ring(&notes, &mask, params);
    nb.track.generator = Some(String::from("bass-follow"));
    nb
}

// ---------------------------------------------------------------------------
// layer budget

/// Melodic roles in the order a budget keeps them.
pub const LAYER_PRIORITY: [&str; 8] = [
    "bass", "melody", "lead", "harmony", "pad", "chord", "chords", "arp",
];

/// `layerBudget: "auto"` per section name (others: 2).
pub fn auto_layer_budget(section: &str) -> i64 {
    match section {
        "intro" => 1,
        "verse" => 2,
        "pre-chorus" => 2,
        "buildup" => 2,
        "drop" => 3,
        "chorus" => 3,
        "breakdown" => 1,
        "bridge" => 2,
        "solo" => 2,
        "outro" => 1,
        _ => 2,
    }
}

/// Deactivate the lowest-priority melodic roles of each section beyond its budget: their
/// slots become -1 (the structure nets mute them there); nets are left in place.
pub fn apply_layer_budget(tmpl: &mut SongTemplate, budget: i64) {
    for si in 0..tmpl.sections.len() {
        let sec = &tmpl.sections[si];
        let cap = if budget > 0 {
            budget
        } else {
            auto_layer_budget(&sec.name)
        } as usize;
        let melodic: Vec<&str> = LAYER_PRIORITY
            .iter()
            .copied()
            .filter(|r| sec.active.contains(r))
            .collect();
        if melodic.len() <= cap {
            continue;
        }
        let dropped: BTreeSet<String> = melodic[cap..].iter().map(|s| String::from(*s)).collect();
        let kept = OrderedFlags::from_names(
            sec.active
                .keys()
                .iter()
                .filter(|k| !dropped.contains(k.as_str())),
        );
        tmpl.sections[si].active = kept;
        for role in &dropped {
            if let Some(map) = tmpl.slot_map.get_mut(role) {
                if si < map.len() {
                    let n = map[si].len();
                    map[si] = alloc::vec![-1; n];
                }
            }
        }
    }
}
