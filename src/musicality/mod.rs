//! dj's musicality pass: the deterministic, pure part of `Sources/DJCore/Musicality`.
//! (See PORTING.md for what is and is not ported.)

pub mod answer;
pub mod audio_metrics;
pub mod backbeat;
pub mod bass_motif;
pub mod beat_tracker;
pub mod dj;
pub mod echo;
pub mod energy_curve;
pub mod fft;
pub mod fixes;
pub mod groove_report;
pub mod harmonic_cadence;
pub mod hook;
pub mod loop_key;
pub mod loudness;
pub mod melody_bigrams;
pub mod melody_shape;
pub mod mix_balance;
pub mod mix_targets;
pub mod note_log;
pub mod pitch_measure;
pub mod report;
pub mod rest_gestures;
pub mod riser;
pub mod section_harmony;
pub mod section_melody;
pub mod section_segmenter;
pub mod slot_windows;
pub mod space;
pub mod spectrum;
pub mod synth_voice;
pub mod track_variety;
pub mod translation;
pub mod util;

pub use fixes::Fixes;

use crate::gen::tables::BeatsTables;
use crate::pflow::Project;
use crate::spec::TrackSpec;

/// `DJMusicality.apply`: `project` as the deck should play it. `spec` is the spec it was
/// composed from. `compose()` stays beats' byte for byte; this runs after it, on the played
/// project, and only rewrites MIDI bindings and initial markings.
pub fn apply(project: &Project, spec: &TrackSpec, fixes: Fixes) -> Project {
    let Some(genre) = BeatsTables::shared().genres.get(&spec.genre) else {
        return project.clone();
    };
    if fixes.is_empty() {
        return project.clone();
    }
    let mut p = project.clone();
    if !spec.part_seeds.is_empty() {
        track_variety::transplant_seeded_melody(&mut p, spec);
    }
    if fixes.contains(Fixes::KEY) {
        dj::keep_variants_in_key(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::PHRASES) {
        dj::align_phrases(&mut p);
    }
    if fixes.contains(Fixes::TIMING) {
        dj::fix_structure_timing(&mut p);
    }
    if fixes.contains(Fixes::BACKBEAT) {
        backbeat::play_backbeat(&mut p, &spec.genre);
    }
    if fixes.contains(Fixes::SECTION_HARMONY) {
        section_melody::normalise_section_melody(&mut p, spec);
    }
    if fixes.contains(Fixes::VARIETY) {
        track_variety::vary_harmony(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::HARMONIC_CADENCE) {
        harmonic_cadence::resolve_pad_cycle(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::REGISTER) {
        dj::separate_registers(&mut p);
    }
    if fixes.contains(Fixes::BASS_LINE) {
        dj::lead_bass(&mut p);
    }
    if fixes.contains(Fixes::VARIETY) {
        track_variety::vary_figures(&mut p, genre, spec);
    }
    // the pad's chords before `.color` adds its sevenths: what `.cadence`, `.hook` and
    // `.motif` aim the melody's chord tones at
    let mut triads = if fixes.contains(Fixes::COLOR) {
        Some(dj::pad_chords(&p))
    } else {
        None
    };
    let colored = if fixes.contains(Fixes::COLOR) {
        dj::color_chords(&mut p, genre)
    } else {
        alloc::vec::Vec::new()
    };
    if colored.is_empty() {
        triads = None;
    }
    if fixes.contains(Fixes::VOICING) {
        dj::voice_lead_pads(&mut p);
    }
    let arp_genre: Option<&str> = if fixes.contains(Fixes::ARP_STYLE) {
        Some(spec.genre.as_str())
    } else {
        None
    };
    if fixes.contains(Fixes::ARPEGGIO) && !fixes.contains(Fixes::ARP_DENSITY) {
        dj::arpeggiate(&mut p, spec.seed_for_part("arp"), arp_genre, None);
    }
    if fixes.contains(Fixes::OFFBEAT_BASS) {
        dj::move_bass_off_the_kick(&mut p, genre);
    }
    if fixes.contains(Fixes::CONTOUR) {
        dj::step_repeated_notes(&mut p);
    }
    if fixes.contains(Fixes::SHAPE) {
        melody_shape::shape_melody(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::CADENCE) {
        dj::resolve_phrase_ends(&mut p, triads.as_ref());
    }
    if fixes.contains(Fixes::HOOK) {
        hook::place_hook(&mut p, genre, spec, triads.as_ref());
    }
    if fixes.contains(Fixes::MOTIF) {
        dj::state_ideas_twice(&mut p, triads.as_ref());
    }
    if fixes.contains(Fixes::ANSWER) {
        answer::answer_phrases(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::BASS_MOTIF) {
        bass_motif::restate_bass_rhythm(&mut p, genre, spec);
    }
    if fixes.contains(Fixes::APPROACH) {
        dj::approach_chord_changes(&mut p, genre);
    }
    if fixes.contains(Fixes::LEGATO) {
        dj::hold_melody_notes(&mut p);
    }
    // After every melody rewrite, so the arp follows the melody as it plays.
    if fixes.contains(Fixes::ARPEGGIO) && fixes.contains(Fixes::ARP_DENSITY) {
        let seed = spec.seed_for_part("arp");
        let rates = dj::arp_density_rates(&p, seed, arp_genre);
        dj::arpeggiate(&mut p, seed, arp_genre, Some(&rates));
    }
    if fixes.contains(Fixes::DYNAMICS) {
        dj::accent_by_meter(&mut p);
    }
    if fixes.contains(Fixes::FILLS) {
        dj::play_fills(&mut p, genre);
    }
    if fixes.contains(Fixes::BUILDUP) {
        dj::roll_into_drops(&mut p, genre);
    }
    if fixes.contains(Fixes::CRASH) {
        dj::crash_on_sections(&mut p, genre);
    }
    if fixes.contains(Fixes::OPEN_HATS) {
        dj::open_offbeat_hats(&mut p, genre);
    }
    // on the final notes: everything that moves the pad, the melody, the arp or the bass ran
    if !colored.is_empty() {
        dj::unrub_color(&mut p, &colored);
    }
    p
}
