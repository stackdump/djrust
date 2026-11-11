//! Port of `Musicality/SectionMelody.swift`: the bridge / solo melody brought back to the
//! home harmony (6.3 section harmony; the runner does the transpose).

use alloc::string::String;
use alloc::vec::Vec;

use crate::gen::core::scale_intervals;
use crate::gen::theme::{build_track_theme, motif_mode, TrackTheme};
use crate::musicality::section_harmony::SectionHarmony;
use crate::musicality::slot_windows::slot_windows;
use crate::pflow::Project;
use crate::spec::TrackSpec;
use crate::wave::synth::is_drum_channel;

/// `normaliseSectionMelody`: move every melody / arp slot net that sounds only in bridge
/// sections (or only in solo ones) back by that shift, diatonically in the project's key.
pub fn normalise_section_melody(p: &mut Project, spec: &TrackSpec) {
    if p.cohesion.as_deref() != Some("v2")
        || spec.extra.get("hook").and_then(|v| v.as_bool()) == Some(true)
        || SectionHarmony::new(p).is_none()
    {
        return;
    }
    let name = if p.scale_name.is_empty() {
        "Major"
    } else {
        p.scale_name.as_str()
    };
    let intervals = scale_intervals(&alloc::format!("{name}Scale")).to_vec();
    if intervals.len() != 7 {
        return;
    }
    let theme = build_track_theme(&spec.genre, spec.seed);
    let root = ((p.root_note.unwrap_or(60) % 12) + 12) % 12;
    for (net, d) in section_melody_shifts(p, &theme) {
        let Some(nb) = p.nets.get_mut(&net) else {
            continue;
        };
        for t in nb.bindings.keys().to_vec() {
            let b = nb.bindings.get_mut(&t).unwrap();
            if is_drum_channel(b.channel) {
                continue;
            }
            b.note = SectionHarmony::shift(b.note, -d, root, &intervals);
        }
    }
}

/// `sectionMelodyShifts`: the melody / arp slot nets beats moved for a section's harmony,
/// with the shift. Sorted by net id.
pub fn section_melody_shifts(p: &Project, theme: &TrackTheme) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for role in ["melody", "arp"] {
        for (net, windows) in slot_windows(p, role) {
            let mut shift: Option<i64> = None;
            let mut ok = !windows.is_empty();
            for w in &windows {
                let d = SectionHarmony::section_degrees(&w.section_name).unwrap_or(0);
                let mode = theme
                    .energy
                    .get(&w.section_name)
                    .and_then(|prof| prof.roles.get(role))
                    .and_then(|r| r.motif_mode);
                let good = d != 0
                    && mode.map_or(false, |m| m != motif_mode::IGNORE)
                    && shift.map_or(true, |s| s == d);
                if !good {
                    ok = false;
                    break;
                }
                shift = Some(d);
            }
            if ok {
                if let Some(shift) = shift {
                    out.push((net, shift));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
