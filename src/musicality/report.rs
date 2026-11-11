//! Port of `Musicality/MusicalityReport.swift`: objective measures of how musical a note log
//! is: key, harmony, register, groove and form. None of them says a track is good; each
//! catches a specific way it can sound wrong. The musicality tests hold tracks to thresholds
//! on it.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gen::core::scale_intervals;
use crate::json::{JsonObject, JsonValue};
use crate::musicality::backbeat::{beat_style, has_beat};
use crate::musicality::note_log::{NoteEvent, PartChange};
use crate::pflow::Project;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MusicalityReport {
    pub notes: usize,
    pub tonal_notes: usize,
    /// Tonal notes whose pitch class is in the project's key.
    pub in_key: f64,
    pub chord_tones_on_beats: Option<f64>,
    pub bass_on_root: Option<f64>,
    /// Pairs of sounding notes from different parts a semitone (or major seventh) apart,
    /// per 100 overlapping pairs.
    pub semitone_clashes: f64,
    pub bass_on_kick: Option<f64>,
    pub harmony_in_bass_range: Option<f64>,
    pub median_note: BTreeMap<String, i64>,
    pub velocity_spread: BTreeMap<String, f64>,
    pub melody_recurrence: Option<f64>,
    pub sections_on_phrases: f64,
    pub section_contrast: f64,
    pub density: BTreeMap<String, f64>,
    pub changes_on_bars: Option<f64>,
    pub changes_on_phrases: Option<f64>,
    pub fills_before_sections: Option<f64>,
    pub slots_from_the_top: Option<f64>,
    pub pad_motion: Option<f64>,
    pub melody_repeats: Option<f64>,
    pub melody_steps: Option<f64>,
    pub pad_color: Option<f64>,
    pub arp_chord_tones: Option<f64>,
    pub arp_doubles_melody: Option<f64>,
    pub buildups_build: Option<f64>,
    pub crash_on_sections: Option<f64>,
    pub bass_leap: Option<f64>,
    pub bass_stepwise: Option<f64>,
    pub melody_legato: Option<f64>,
    pub phrase_ends_on_chord_tones: Option<f64>,
    pub melody_idea_repeats: Option<f64>,
    pub harmonic_cadences: Option<f64>,
    pub progressions: Option<usize>,
    pub section_chord_agreement: Option<f64>,
    pub section_chord_agreement_by_section: Vec<(String, f64)>,
    pub answer_phrases: Option<f64>,
    pub hook_recurrence: Option<f64>,
    pub bar_one_chord_tones: Option<f64>,
    pub bass_idea_repeats: Option<f64>,
    pub melody_peak_position: Option<f64>,
    pub leap_recovery: Option<f64>,
    pub beat_by_section: Vec<(String, bool)>,
    pub has_beat: Option<f64>,
    pub structure_on_4_bars: Option<f64>,
    pub max_lateness_ticks: Option<i64>,
    pub phrase_bars: Vec<f64>,
    pub intro_bars: Option<i64>,
    pub outro_bars: Option<i64>,
    pub song_seconds: Option<f64>,
}

fn is_tonal(e: &NoteEvent) -> bool {
    !e.is_drum() && e.group != "stinger"
}

fn sorted_by_tick(mut v: Vec<&NoteEvent>) -> Vec<&NoteEvent> {
    v.sort_by_key(|e| e.tick);
    v
}

fn group_by<'a, K: Ord>(
    xs: impl IntoIterator<Item = &'a NoteEvent>,
    key: impl Fn(&NoteEvent) -> K,
) -> BTreeMap<K, Vec<&'a NoteEvent>> {
    let mut m: BTreeMap<K, Vec<&NoteEvent>> = BTreeMap::new();
    for e in xs {
        m.entry(key(e)).or_default().push(e);
    }
    m
}

fn dir(a: i64, b: i64) -> i64 {
    (b - a).signum()
}

impl MusicalityReport {
    /// `genre` picks the sections `has_beat` expects a beat in; without it `has_beat` is None.
    pub fn new(
        project: &Project,
        log: &[NoteEvent],
        controls: &[PartChange],
        genre: Option<&str>,
    ) -> MusicalityReport {
        let mut r = MusicalityReport {
            in_key: 1.0,
            sections_on_phrases: 1.0,
            section_contrast: 1.0,
            ..Default::default()
        };
        if !controls.is_empty() {
            let on = controls.iter().filter(|c| c.step % 16 == 0).count();
            r.changes_on_bars = Some(on as f64 / controls.len() as f64);
            let mut starts: Vec<(i64, i64)> = Vec::new();
            let mut t = 0;
            for sec in &project.structure {
                // 2 bars, or the section's own phrase when it is shorter
                let count = sec
                    .phrases
                    .values()
                    .map(|p| p.len() as i64)
                    .max()
                    .unwrap_or(1)
                    .max(1);
                starts.push((t, (sec.steps / count).min(32).max(1)));
                t += sec.steps;
            }
            if !starts.is_empty() {
                let grid = controls
                    .iter()
                    .filter(|c| {
                        let s = starts
                            .iter()
                            .rev()
                            .find(|s| s.0 <= c.step)
                            .unwrap_or(&starts[0]);
                        (c.step - s.0) % s.1 == 0
                    })
                    .count();
                r.changes_on_phrases = Some(grid as f64 / controls.len() as f64);
            }
            let slots: Vec<i64> = controls.iter().filter_map(|c| c.phase).collect();
            if !slots.is_empty() {
                r.slots_from_the_top =
                    Some(slots.iter().filter(|&&p| p == 0).count() as f64 / slots.len() as f64);
            }
        }
        r.notes = log.len();
        let tonal: Vec<&NoteEvent> = log.iter().filter(|e| is_tonal(e)).collect();
        r.tonal_notes = tonal.len();
        let bars = (log.iter().map(|e| e.tick).max().unwrap_or(16) / 16 + 1).max(1);

        // Key
        let root = project.root_note.unwrap_or(60);
        let name = if project.scale_name.is_empty() {
            "Major"
        } else {
            project.scale_name.as_str()
        };
        let pcs: BTreeSet<i64> = scale_intervals(&alloc::format!("{name}Scale"))
            .iter()
            .map(|i| (root + i) % 12)
            .collect();
        if !tonal.is_empty() {
            r.in_key = tonal
                .iter()
                .filter(|e| pcs.contains(&(e.note % 12)))
                .count() as f64
                / tonal.len() as f64;
        }

        // Chords per bar, from the harmony part (its first note in a bar is the chord's root)
        let mut chord: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for e in tonal.iter().filter(|e| e.group == "harmony") {
            chord.entry((e.tick - 1) / 16).or_default().push(e.note);
        }
        if !chord.is_empty() {
            let (mut hits, mut total) = (0, 0);
            for e in tonal
                .iter()
                .filter(|e| (e.group == "melody" || e.group == "arp") && (e.tick - 1) % 4 == 0)
            {
                let Some(c) = chord.get(&((e.tick - 1) / 16)) else {
                    continue;
                };
                total += 1;
                if c.iter().any(|n| n % 12 == e.note % 12) {
                    hits += 1;
                }
            }
            if total > 0 {
                r.chord_tones_on_beats = Some(hits as f64 / total as f64);
            }
            let arp: Vec<&&NoteEvent> = tonal
                .iter()
                .filter(|e| e.group == "arp" && chord.contains_key(&((e.tick - 1) / 16)))
                .collect();
            if !arp.is_empty() {
                let tones = arp
                    .iter()
                    .filter(|e| {
                        chord[&((e.tick - 1) / 16)]
                            .iter()
                            .any(|n| n % 12 == e.note % 12)
                    })
                    .count();
                r.arp_chord_tones = Some(tones as f64 / arp.len() as f64);
            }
            let mut first_bass: BTreeMap<i64, i64> = BTreeMap::new();
            for e in tonal.iter().filter(|e| e.group == "bass") {
                first_bass.entry((e.tick - 1) / 16).or_insert(e.note);
            }
            let (mut agree, mut counted) = (0, 0);
            for (b, n) in &first_bass {
                let Some(rt) = chord.get(b).and_then(|c| c.first()) else {
                    continue;
                };
                counted += 1;
                if rt % 12 == n % 12 {
                    agree += 1;
                }
            }
            if counted > 0 {
                r.bass_on_root = Some(agree as f64 / counted as f64);
            }
        }

        // Semitone rubs between parts
        let (mut pairs, mut clashes) = (0u64, 0u64);
        let by_start = group_by(tonal.iter().copied(), |e| e.tick);
        let mut sounding: Vec<&NoteEvent> = Vec::new();
        if let (Some(lo), Some(hi)) = (
            tonal.iter().map(|e| e.tick).min(),
            tonal.iter().map(|e| e.tick).max(),
        ) {
            for tick in lo..=hi {
                sounding.retain(|o| !((o.tick as f64) + o.steps.max(1.0) <= tick as f64));
                if let Some(starting) = by_start.get(&tick) {
                    for e in starting {
                        for o in &sounding {
                            if o.group != e.group {
                                pairs += 1;
                                let d = (o.note - e.note).abs() % 12;
                                if d == 1 || d == 11 {
                                    clashes += 1;
                                }
                            }
                        }
                        sounding.push(e);
                    }
                }
            }
        }
        if pairs > 0 {
            r.semitone_clashes = 100.0 * clashes as f64 / pairs as f64;
        }
        let arp_notes: Vec<&&NoteEvent> = tonal.iter().filter(|e| e.group == "arp").collect();
        if !arp_notes.is_empty() {
            let doubled = arp_notes
                .iter()
                .filter(|a| {
                    by_start.get(&a.tick).map_or(false, |v| {
                        v.iter()
                            .any(|m| m.group == "melody" && m.note % 12 == a.note % 12)
                    })
                })
                .count();
            r.arp_doubles_melody = Some(doubled as f64 / arp_notes.len() as f64);
        }

        // Kick and bass
        let kicks: BTreeSet<i64> = log
            .iter()
            .filter(|e| e.is_drum() && (e.note == 36 || e.note == 35))
            .map(|e| e.tick)
            .collect();
        let bass: Vec<&NoteEvent> = tonal
            .iter()
            .copied()
            .filter(|e| e.group == "bass")
            .collect();
        if !bass.is_empty() && !kicks.is_empty() {
            r.bass_on_kick = Some(
                bass.iter().filter(|e| kicks.contains(&e.tick)).count() as f64 / bass.len() as f64,
            );
        }
        let mut leaps: Vec<i64> = Vec::new();
        let bass_line = sorted_by_tick(bass.clone());
        for w in bass_line.windows(2) {
            let (a, b) = (w[0], w[1]);
            if (a.tick - 1) / 16 + 1 == (b.tick - 1) / 16 && a.note % 12 != b.note % 12 {
                leaps.push((b.note - a.note).abs());
            }
        }
        if !leaps.is_empty() {
            r.bass_leap = Some(leaps.iter().sum::<i64>() as f64 / leaps.len() as f64);
            r.bass_stepwise =
                Some(leaps.iter().filter(|&&l| l <= 2).count() as f64 / leaps.len() as f64);
        }
        let harmony: Vec<&NoteEvent> = tonal
            .iter()
            .copied()
            .filter(|e| e.group == "harmony")
            .collect();
        if let (Some(top), false) = (bass.iter().map(|e| e.note).max(), harmony.is_empty()) {
            r.harmony_in_bass_range = Some(
                harmony.iter().filter(|e| e.note <= top).count() as f64 / harmony.len() as f64,
            );
        }

        // Register and dynamics
        let by_group = group_by(log.iter(), |e| {
            if e.is_drum() {
                String::from("drums")
            } else {
                e.group.clone()
            }
        });
        for (g, es) in &by_group {
            if g != "drums" && g != "stinger" {
                let mut sorted: Vec<i64> = es.iter().map(|e| e.note).collect();
                sorted.sort();
                r.median_note.insert(g.clone(), sorted[sorted.len() / 2]);
            }
            let v: Vec<f64> = es.iter().map(|e| e.velocity as f64).collect();
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            r.velocity_spread.insert(
                g.clone(),
                libm::sqrt(v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / v.len() as f64),
            );
            r.density.insert(g.clone(), es.len() as f64 / bars as f64);
        }

        // Motifs: does a melody bar come back 4 or 8 bars later?
        let mut bar_shape: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for e in tonal.iter().filter(|e| e.group == "melody") {
            bar_shape
                .entry((e.tick - 1) / 16)
                .or_default()
                .push(((e.tick - 1) % 16) * 128 + e.note);
        }
        let transpose_free = |s: &[i64]| -> Vec<i64> {
            match s.first() {
                None => Vec::new(),
                Some(&first) => s
                    .iter()
                    .map(|v| (v / 128) * 1000 + (v % 128 - first % 128))
                    .collect(),
            }
        };
        if let Some(&first_bar) = bar_shape.keys().next() {
            let (mut recurring, mut counted) = (0, 0);
            for (&b, shape) in &bar_shape {
                if b < first_bar + 4 {
                    continue;
                }
                counted += 1;
                let me = transpose_free(shape);
                if [4, 8]
                    .iter()
                    .any(|d| bar_shape.get(&(b - d)).map(|s| transpose_free(s)) == Some(me.clone()))
                {
                    recurring += 1;
                }
            }
            if counted > 0 {
                r.melody_recurrence = Some(recurring as f64 / counted as f64);
            }
        }

        // Pad voice leading
        let mut motion: Vec<f64> = Vec::new();
        let mut prev_chord: Option<Vec<i64>> = None;
        for (_, c) in &chord {
            let mut c = c.clone();
            c.sort();
            if let Some(p) = &prev_chord {
                let ps: BTreeSet<i64> = p.iter().map(|n| n % 12).collect();
                let cs: BTreeSet<i64> = c.iter().map(|n| n % 12).collect();
                if p.len() == c.len() && ps != cs {
                    motion.push(
                        p.iter()
                            .zip(c.iter())
                            .map(|(a, b)| (a - b).abs())
                            .sum::<i64>() as f64,
                    );
                }
            }
            prev_chord = Some(c);
        }
        if !motion.is_empty() {
            r.pad_motion = Some(motion.iter().sum::<f64>() / motion.len() as f64);
        }
        if !chord.is_empty() {
            let colourful = chord
                .values()
                .filter(|c| c.iter().map(|n| n % 12).collect::<BTreeSet<_>>().len() >= 4)
                .count();
            r.pad_color = Some(colourful as f64 / chord.len() as f64);
        }

        // Melodic intervals, within a net
        let melody_tonal: Vec<&NoteEvent> = tonal
            .iter()
            .copied()
            .filter(|e| e.group == "melody")
            .collect();
        let (mut repeats, mut stepwise, mut intervals) = (0, 0, 0);
        for (_, es) in group_by(melody_tonal.iter().copied(), |e| e.net.clone()) {
            let by_tick = sorted_by_tick(es);
            for w in by_tick.windows(2) {
                if w[1].tick - w[0].tick <= 8 {
                    intervals += 1;
                    let d = (w[1].note - w[0].note).abs();
                    if d == 0 {
                        repeats += 1;
                    }
                    if d == 1 || d == 2 {
                        stepwise += 1;
                    }
                }
            }
        }
        if intervals > 0 {
            r.melody_repeats = Some(repeats as f64 / intervals as f64);
            r.melody_steps = Some(stepwise as f64 / intervals as f64);
        }

        // Phrasing: notes held, phrases closed
        let mut section_starts: Vec<i64> = Vec::new();
        let mut at = 0;
        for s in &project.structure {
            section_starts.push(at);
            at += s.steps;
        }
        let section =
            |step: i64| -> usize { section_starts.iter().rposition(|&s| s <= step).unwrap_or(0) };
        let mut held: Vec<f64> = Vec::new();
        let (mut ends, mut resolved) = (0, 0);
        for (_, es) in group_by(melody_tonal.iter().copied(), |e| e.net.clone()) {
            let by_tick = sorted_by_tick(es);
            for (k, e) in by_tick.iter().enumerate() {
                let next = if k + 1 < by_tick.len() {
                    Some(by_tick[k + 1].tick)
                } else {
                    None
                };
                if let Some(nx) = next {
                    if nx > e.tick && nx - e.tick <= 4 {
                        held.push((e.steps / (nx - e.tick) as f64).min(1.0));
                    }
                }
                let rest = next.map_or(false, |nx| {
                    nx - e.tick >= 8 && section(nx - 1) == section(e.tick - 1)
                });
                let line = next.map_or(true, |nx| (nx - 1) / 64 > (e.tick - 1) / 64);
                if rest || line {
                    if let Some(c) = chord.get(&((e.tick - 1) / 16)) {
                        ends += 1;
                        if c.iter().any(|n| n % 12 == e.note % 12) {
                            resolved += 1;
                        }
                    }
                }
            }
        }
        if !held.is_empty() {
            r.melody_legato = Some(held.iter().sum::<f64>() / held.len() as f64);
        }
        if ends > 0 {
            r.phrase_ends_on_chord_tones = Some(resolved as f64 / ends as f64);
        }

        // Ideas stated twice: bar 2 of each 4-bar phrase against bar 1
        let (mut phrases, mut restated) = (0, 0);
        for (_, es) in group_by(melody_tonal.iter().copied(), |e| e.net.clone()) {
            for (_, block) in group_by(es.iter().copied(), |e| (e.tick - 1) / 64) {
                let sorted = sorted_by_tick(block);
                let a: Vec<&NoteEvent> = sorted
                    .iter()
                    .copied()
                    .filter(|e| (e.tick - 1) % 64 < 16)
                    .collect();
                let b: Vec<&NoteEvent> = sorted
                    .iter()
                    .copied()
                    .filter(|e| (e.tick - 1) % 64 >= 16 && (e.tick - 1) % 64 < 32)
                    .collect();
                if !(a.len() >= 2 && !b.is_empty()) {
                    continue;
                }
                phrases += 1;
                let dirs = |x: &[&NoteEvent]| -> Vec<i64> {
                    x.windows(2).map(|w| dir(w[0].note, w[1].note)).collect()
                };
                let ra: Vec<i64> = a.iter().map(|e| (e.tick - 1) % 16).collect();
                let rb: Vec<i64> = b.iter().map(|e| (e.tick - 1) % 16).collect();
                if ra != rb {
                    continue;
                }
                let turns = dirs(&a)
                    .iter()
                    .zip(dirs(&b).iter())
                    .filter(|(x, y)| x != y)
                    .count();
                if 3 * turns <= a.len() - 1 {
                    restated += 1;
                }
            }
        }
        if phrases > 0 {
            r.melody_idea_repeats = Some(restated as f64 / phrases as f64);
        }

        // Harmonic motion: cadences at 4-bar lines, progressions per 4-bar block, and how
        // well each section's parts agree with its chords
        if !chord.is_empty() {
            let (mut lines, mut cadences) = (0, 0);
            for (&b, c) in &chord {
                if b % 4 != 3 {
                    continue;
                }
                let (Some(&last), Some(&next)) =
                    (c.first(), chord.get(&(b + 1)).and_then(|c| c.first()))
                else {
                    continue;
                };
                lines += 1;
                let up = ((next - last) % 12 + 12) % 12;
                if up == 5 || up == 2 {
                    cadences += 1;
                }
            }
            if lines > 0 {
                r.harmonic_cadences = Some(cadences as f64 / lines as f64);
            }
            let mut blocks: BTreeSet<Vec<i64>> = BTreeSet::new();
            let ks: BTreeSet<i64> = chord.keys().map(|k| k / 4).collect();
            for k in ks {
                let roots: Vec<i64> = (4 * k..4 * k + 4)
                    .filter_map(|b| chord.get(&b).and_then(|c| c.first()).copied())
                    .collect();
                if roots.len() == 4 {
                    blocks.insert(roots.iter().map(|x| ((x - root) % 12 + 12) % 12).collect());
                }
            }
            if !blocks.is_empty() {
                r.progressions = Some(blocks.len());
            }
            let n_sec = section_starts.len().max(1);
            let mut agreeing = vec![0usize; n_sec];
            let mut on_beats = vec![0usize; n_sec];
            for e in tonal.iter().filter(|e| {
                ["melody", "arp", "bass"].contains(&e.group.as_str()) && (e.tick - 1) % 4 == 0
            }) {
                let Some(c) = chord.get(&((e.tick - 1) / 16)) else {
                    continue;
                };
                let s = section(e.tick - 1);
                on_beats[s] += 1;
                if c.iter().any(|n| n % 12 == e.note % 12) {
                    agreeing[s] += 1;
                }
            }
            for s in 0..on_beats.len() {
                if on_beats[s] > 0 {
                    let name = if s < project.structure.len() {
                        project.structure[s].name.clone()
                    } else {
                        String::from("song")
                    };
                    r.section_chord_agreement_by_section
                        .push((name, agreeing[s] as f64 / on_beats[s] as f64));
                }
            }
            r.section_chord_agreement = r
                .section_chord_agreement_by_section
                .iter()
                .map(|x| x.1)
                .fold(None, |m: Option<f64>, v| {
                    Some(m.map_or(v, |m| if v < m { v } else { m }))
                });
        }

        // 6.4: phrases that answer, a hook that comes back, bass ideas, contour
        let tonic = ((project.root_note.unwrap_or(60) % 12) + 12) % 12;
        let mut melody_by_net: Vec<(String, Vec<&NoteEvent>)> =
            group_by(melody_tonal.iter().copied(), |e| e.net.clone())
                .into_iter()
                .collect();
        melody_by_net.sort_by(|a, b| a.0.cmp(&b.0));
        let (mut pairs8, mut answered) = (0, 0);
        let mut peaks: Vec<f64> = Vec::new();
        let (mut leaps_taken, mut recovered) = (0, 0);
        for (_, line) in &melody_by_net {
            let line = sorted_by_tick(line.clone());
            for (_, block) in group_by(line.iter().copied(), |e| (e.tick - 1) / 128) {
                // `max { $0.tick < $1.tick }` keeps the first of equal maxima
                let mut first: Option<&NoteEvent> = None;
                let mut second: Option<&NoteEvent> = None;
                for e in &block {
                    if (e.tick - 1) % 128 < 64 {
                        if first.map_or(true, |f| f.tick < e.tick) {
                            first = Some(e);
                        }
                    } else if second.map_or(true, |f| f.tick < e.tick) {
                        second = Some(e);
                    }
                }
                let (Some(first), Some(second)) = (first, second) else {
                    continue;
                };
                pairs8 += 1;
                if first.note % 12 != tonic && second.note % 12 == tonic {
                    answered += 1;
                }
            }
            for (_, block) in group_by(line.iter().copied(), |e| (e.tick - 1) / 64) {
                if block.len() < 2 {
                    continue;
                }
                let top = block.iter().map(|e| e.note).max().unwrap();
                let peak = block
                    .iter()
                    .filter(|e| e.note == top)
                    .map(|e| e.tick)
                    .min()
                    .unwrap();
                peaks.push(((peak - 1) % 64) as f64 / 64.0);
            }
            if line.len() >= 3 {
                for k in 0..line.len() - 2 {
                    let (a, b, c) = (line[k], line[k + 1], line[k + 2]);
                    if !(b.tick - a.tick <= 8
                        && (b.note - a.note).abs() >= 5
                        && c.tick - b.tick <= 8)
                    {
                        continue;
                    }
                    leaps_taken += 1;
                    let back = c.note - b.note;
                    if (1..=2).contains(&back.abs()) && back.signum() == -(b.note - a.note).signum()
                    {
                        recovered += 1;
                    }
                }
            }
        }
        if pairs8 > 0 {
            r.answer_phrases = Some(answered as f64 / pairs8 as f64);
        }
        if !peaks.is_empty() {
            r.melody_peak_position = Some(peaks.iter().sum::<f64>() / peaks.len() as f64);
        }
        if leaps_taken > 0 {
            r.leap_recovery = Some(recovered as f64 / leaps_taken as f64);
        }

        let rhythm =
            |x: &[&NoteEvent]| -> Vec<i64> { x.iter().map(|e| (e.tick - 1) % 16).collect() };
        let moves = |x: &[&NoteEvent]| -> Vec<i64> {
            x.windows(2).map(|w| dir(w[0].note, w[1].note)).collect()
        };
        let mut melody_bar_notes: BTreeMap<i64, Vec<&NoteEvent>> = BTreeMap::new();
        for e in &melody_tonal {
            melody_bar_notes
                .entry((e.tick - 1) / 16)
                .or_default()
                .push(e);
        }
        for v in melody_bar_notes.values_mut() {
            v.sort_by_key(|e| (e.tick, e.note));
        }
        if let Some((&hook_bar, hook)) = melody_bar_notes.iter().find(|(_, v)| v.len() >= 2) {
            let shape = moves(hook);
            let home = section(16 * hook_bar);
            let (mut later, mut heard) = (0, 0);
            for s in 0..section_starts.len() {
                if s <= home {
                    continue;
                }
                let lo = section_starts[s] / 16;
                let hi = (section_starts[s] + project.structure[s].steps) / 16;
                let bars: Vec<&Vec<&NoteEvent>> =
                    (lo..hi).filter_map(|b| melody_bar_notes.get(&b)).collect();
                if bars.is_empty() {
                    continue;
                }
                later += 1;
                let neg: Vec<i64> = shape.iter().map(|x| -x).collect();
                if bars.iter().any(|bar| {
                    rhythm(bar) == rhythm(hook) && (moves(bar) == shape || moves(bar) == neg)
                }) {
                    heard += 1;
                }
            }
            if later > 0 {
                r.hook_recurrence = Some(heard as f64 / later as f64);
            }
        }
        if !chord.is_empty() {
            let on_one: Vec<(&&NoteEvent, &Vec<i64>)> = tonal
                .iter()
                .filter(|e| e.group == "melody" && (e.tick - 1) / 16 % 4 == 0)
                .filter_map(|e| chord.get(&((e.tick - 1) / 16)).map(|c| (e, c)))
                .collect();
            if !on_one.is_empty() {
                let tones = on_one
                    .iter()
                    .filter(|(e, c)| c.iter().any(|n| n % 12 == e.note % 12))
                    .count();
                r.bar_one_chord_tones = Some(tones as f64 / on_one.len() as f64);
            }
        }
        let (mut bass_phrases, mut bass_echoes) = (0, 0);
        for (_, es) in group_by(bass.iter().copied(), |e| e.net.clone()) {
            for (_, block) in group_by(es.iter().copied(), |e| (e.tick - 1) / 64) {
                let sorted = sorted_by_tick(block);
                let a: Vec<&NoteEvent> = sorted
                    .iter()
                    .copied()
                    .filter(|e| (e.tick - 1) % 64 < 16)
                    .collect();
                let b: Vec<&NoteEvent> = sorted
                    .iter()
                    .copied()
                    .filter(|e| (e.tick - 1) % 64 >= 16 && (e.tick - 1) % 64 < 32)
                    .collect();
                if !(a.len() >= 2 && !b.is_empty()) {
                    continue;
                }
                bass_phrases += 1;
                let sa: BTreeSet<i64> = rhythm(&a).into_iter().collect();
                let sb: BTreeSet<i64> = rhythm(&b).into_iter().collect();
                if sa == sb {
                    bass_echoes += 1;
                }
            }
        }
        if bass_phrases > 0 {
            r.bass_idea_repeats = Some(bass_echoes as f64 / bass_phrases as f64);
        }

        // Form
        let sections = &project.structure;
        if sections.len() > 1 {
            let (mut boundary, mut filled) = (0, 0);
            for s in &sections[..sections.len() - 1] {
                boundary += s.steps;
                if log.iter().any(|e| {
                    e.is_drum()
                        && e.net.starts_with("fill-")
                        && e.tick > boundary - 16
                        && e.tick <= boundary
                }) {
                    filled += 1;
                }
            }
            r.fills_before_sections = Some(filled as f64 / (sections.len() - 1) as f64);
            let (mut end, mut buildups, mut building) = (0, 0, 0);
            for s in &sections[..sections.len() - 1] {
                end += s.steps;
                if !(s.name == "buildup" && s.steps >= 64) {
                    continue;
                }
                buildups += 1;
                let nbars = (s.steps / 16).min(8);
                let per_bar: Vec<usize> = (0..nbars)
                    .map(|bar| {
                        log.iter()
                            .filter(|e| {
                                e.is_drum()
                                    && e.net.starts_with("fill-")
                                    && (e.tick - 1) / 16 == end / 16 - nbars + bar
                            })
                            .count()
                    })
                    .collect();
                let mut sorted = per_bar.clone();
                sorted.sort();
                if per_bar == sorted
                    && per_bar[0] > 0
                    && per_bar[0] <= 4
                    && per_bar[nbars as usize - 1] >= 8
                {
                    building += 1;
                }
            }
            if buildups > 0 {
                r.buildups_build = Some(building as f64 / buildups as f64);
            }
            let crashes: BTreeSet<i64> = log
                .iter()
                .filter(|e| e.is_drum() && (e.note == 49 || e.note == 57))
                .map(|e| e.tick)
                .collect();
            let (mut downbeat, mut crashed) = (0, 0);
            for s in &sections[..sections.len() - 1] {
                downbeat += s.steps;
                if crashes.contains(&(downbeat + 1)) {
                    crashed += 1;
                }
            }
            r.crash_on_sections = Some(crashed as f64 / (sections.len() - 1) as f64);
        }
        if !sections.is_empty() {
            r.sections_on_phrases = sections.iter().filter(|s| s.steps % 64 == 0).count() as f64
                / sections.len() as f64;
            let mut start = 0;
            let mut per_bar: Vec<f64> = Vec::new();
            for s in sections {
                let n = log
                    .iter()
                    .filter(|e| e.tick > start && e.tick <= start + s.steps)
                    .count();
                per_bar.push(n as f64 / (s.steps / 16).max(1) as f64);
                start += s.steps;
            }
            let lo = per_bar.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = per_bar.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            r.section_contrast = hi / lo.max(1.0);
        }

        // M1 form: changes on 4-bar lines, how late they land, phrase / intro / outro lengths
        if !controls.is_empty() {
            r.structure_on_4_bars = Some(
                controls.iter().filter(|c| c.step % 64 == 0).count() as f64 / controls.len() as f64,
            );
            r.max_lateness_ticks = controls
                .iter()
                .map(|c| {
                    let start = section_starts
                        .iter()
                        .rev()
                        .find(|&&s| s <= c.step)
                        .copied()
                        .unwrap_or(0);
                    (c.step - start) % 16
                })
                .max();
        }
        if !sections.is_empty() {
            let total: i64 = sections.iter().map(|s| s.steps).sum();
            let mut events: BTreeSet<i64> = section_starts.iter().copied().collect();
            for c in controls {
                if c.step > 0 && c.step < total {
                    events.insert(c.step);
                }
            }
            let ev: Vec<i64> = events.into_iter().collect();
            let mut spans: Vec<f64> = Vec::new();
            for (i, &e) in ev.iter().enumerate() {
                let nx = if i + 1 < ev.len() { ev[i + 1] } else { total };
                spans.push((nx - e) as f64 / 16.0);
            }
            let mut pb: Vec<f64> = spans.into_iter().filter(|&s| s > 0.0).collect();
            pb.sort_by(|a, b| a.partial_cmp(b).unwrap());
            pb.dedup();
            r.phrase_bars = pb;
            if sections[0].name == "intro" {
                r.intro_bars = Some(sections[0].steps / 16);
            }
            if sections.len() > 1 && sections.last().unwrap().name == "outro" {
                r.outro_bars = Some(sections.last().unwrap().steps / 16);
            }
            let tempo = if project.tempo > 0.0 {
                project.tempo
            } else {
                120.0
            };
            r.song_seconds = Some(total as f64 * 60.0 / (tempo * 4.0));
        }

        // A beat: kick and snare where the style has them
        let mut beat_start = 0;
        for s in sections {
            r.beat_by_section
                .push((s.name.clone(), has_beat(log, beat_start, s.steps)));
            beat_start += s.steps;
        }
        if let Some(style) = genre.and_then(beat_style) {
            let expected: Vec<&(String, bool)> = r
                .beat_by_section
                .iter()
                .filter(|b| style.contains(&b.0.as_str()))
                .collect();
            if !expected.is_empty() {
                r.has_beat =
                    Some(expected.iter().filter(|b| b.1).count() as f64 / expected.len() as f64);
            }
        }
        r
    }
}

impl MusicalityReport {
    /// `MusicalityReport.dictionary`: the report as JSON, every figure rounded to three
    /// places, a measure that was not taken left out. Swift's dictionary has no key order
    /// (the dump sorts the keys), so compare through `canonical_js`.
    pub fn dictionary(&self) -> JsonValue {
        fn num(x: f64) -> JsonValue {
            JsonValue::Number(round3(x))
        }
        let mut d = JsonObject::new();
        d.set("notes", JsonValue::int(self.notes as i64));
        d.set("tonalNotes", JsonValue::int(self.tonal_notes as i64));
        d.set("inKey", num(self.in_key));
        d.set("semitoneClashesPer100", num(self.semitone_clashes));
        d.set("sectionsOnPhrases", num(self.sections_on_phrases));
        d.set("sectionContrast", num(self.section_contrast));
        d.set(
            "medianNote",
            JsonValue::Object(JsonObject::from_pairs(
                self.median_note
                    .iter()
                    .map(|(k, v)| (k.as_str(), JsonValue::int(*v))),
            )),
        );
        d.set(
            "velocitySpread",
            JsonValue::Object(JsonObject::from_pairs(
                self.velocity_spread.iter().map(|(k, v)| (k.as_str(), num(*v))),
            )),
        );
        d.set(
            "densityPerBar",
            JsonValue::Object(JsonObject::from_pairs(
                self.density.iter().map(|(k, v)| (k.as_str(), num(*v))),
            )),
        );
        for (key, v) in [
            ("chordTonesOnBeats", self.chord_tones_on_beats),
            ("bassOnRoot", self.bass_on_root),
            ("bassOnKick", self.bass_on_kick),
            ("harmonyInBassRange", self.harmony_in_bass_range),
            ("melodyRecurrence", self.melody_recurrence),
            ("changesOnBars", self.changes_on_bars),
            ("changesOnPhrases", self.changes_on_phrases),
            ("fillsBeforeSections", self.fills_before_sections),
            ("slotsFromTheTop", self.slots_from_the_top),
            ("padMotion", self.pad_motion),
            ("melodyRepeats", self.melody_repeats),
            ("melodySteps", self.melody_steps),
            ("padColor", self.pad_color),
            ("arpChordTones", self.arp_chord_tones),
            ("arpDoublesMelody", self.arp_doubles_melody),
            ("buildupsBuild", self.buildups_build),
            ("crashOnSections", self.crash_on_sections),
            ("bassLeap", self.bass_leap),
            ("bassStepwise", self.bass_stepwise),
            ("melodyLegato", self.melody_legato),
            ("phraseEndsOnChordTones", self.phrase_ends_on_chord_tones),
            ("melodyIdeaRepeats", self.melody_idea_repeats),
            ("harmonicCadences", self.harmonic_cadences),
        ] {
            if let Some(v) = v {
                d.set(key, num(v));
            }
        }
        if let Some(v) = self.progressions {
            d.set("progressions", JsonValue::int(v as i64));
        }
        for (key, v) in [
            ("answerPhrases", self.answer_phrases),
            ("hookRecurrence", self.hook_recurrence),
            ("barOneChordTones", self.bar_one_chord_tones),
            ("bassIdeaRepeats", self.bass_idea_repeats),
            ("melodyPeakPosition", self.melody_peak_position),
            ("leapRecovery", self.leap_recovery),
        ] {
            if let Some(v) = v {
                d.set(key, num(v));
            }
        }
        if let Some(v) = self.section_chord_agreement {
            d.set("sectionChordAgreement", num(v));
            d.set(
                "sectionChordAgreementBySection",
                JsonValue::Array(
                    self.section_chord_agreement_by_section
                        .iter()
                        .map(|(section, agreement)| {
                            JsonValue::Object(JsonObject::from_pairs([
                                ("section", JsonValue::from(section.as_str())),
                                ("agreement", num(*agreement)),
                            ]))
                        })
                        .collect(),
                ),
            );
        }
        if !self.beat_by_section.is_empty() {
            d.set(
                "beatBySection",
                JsonValue::Array(
                    self.beat_by_section
                        .iter()
                        .map(|(section, beat)| {
                            JsonValue::Object(JsonObject::from_pairs([
                                ("section", JsonValue::from(section.as_str())),
                                ("beat", JsonValue::Bool(*beat)),
                            ]))
                        })
                        .collect(),
                ),
            );
        }
        if let Some(v) = self.has_beat {
            d.set("hasBeat", num(v));
        }
        if let Some(v) = self.structure_on_4_bars {
            d.set("structureOn4Bars", num(v));
        }
        if let Some(v) = self.max_lateness_ticks {
            d.set("maxLatenessTicks", JsonValue::int(v));
        }
        if !self.phrase_bars.is_empty() {
            d.set(
                "phraseBars",
                JsonValue::Array(self.phrase_bars.iter().map(|b| num(*b)).collect()),
            );
        }
        if let Some(v) = self.intro_bars {
            d.set("introBars", JsonValue::int(v));
        }
        if let Some(v) = self.outro_bars {
            d.set("outroBars", JsonValue::int(v));
        }
        if let Some(v) = self.song_seconds {
            d.set("songSeconds", num(v));
        }
        JsonValue::Object(d)
    }
}

/// `round3`.
pub fn round3(x: f64) -> f64 {
    libm::round(x * 1000.0) / 1000.0
}

/// `MusicalityReport.isTonal`: the groups that carry pitch (everything but drums and stingers).
pub fn is_tonal_event(e: &NoteEvent) -> bool {
    is_tonal(e)
}
