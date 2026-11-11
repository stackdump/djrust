//! The target registry: every feature a control can be bound to, as data.
//!
//! A target is a [`DJActionKind`]. The registry says, for each one, what it is called, which
//! family it sits in, how a control drives it ([`TargetKind`]) and which arguments it reads
//! ([`ParamSpec`]). The map editor, MIDI learn and the device drawing are built from this
//! list, so they know a new feature the moment its kind exists. Adding a feature is:
//! one `DJActionKind` variant (the `info` match below will not compile without its entry),
//! one arm where the controller turns it into an `Action`, and the host's handling of that.
//! No per-device code.

use alloc::string::String;
use alloc::vec::Vec;

use crate::control::surface::DJActionKind;

/// How a control drives a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// Acts on press.
    Trigger,
    /// Follows a knob, fader or pitch bend (0...1).
    Continuous,
    /// Follows a wheel's signed steps.
    Relative,
    /// Takes the control's index as its argument (a key's note number).
    Keyed,
    /// Holds a modifier while down.
    Modifier,
}

/// What a binding's field means for a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// A deck: "A", "B", "L" (the loops) or "*" (the aimed one).
    Deck,
    /// A slot number, from 0.
    Slot,
    /// A role name ("kick", "bass"...).
    Role,
    /// A net macro id.
    Macro,
    /// An FX unit control id.
    FxParam,
    /// Free text in `args[name]`; the choices, if closed, are in [`ParamSpec::choices`].
    Arg,
}

#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub kind: ParamKind,
    /// The `args` key for [`ParamKind::Arg`], else a label.
    pub name: &'static str,
    pub choices: &'static [&'static str],
    pub help: &'static str,
}

const DECK: ParamSpec = ParamSpec {
    kind: ParamKind::Deck,
    name: "deck",
    choices: &["A", "B", "L", "*"],
    help: "A, B, L (loops) or * (the aimed one)",
};
const SLOT: ParamSpec = ParamSpec {
    kind: ParamKind::Slot,
    name: "slot",
    choices: &[],
    help: "0-based slot",
};
const ROLE: ParamSpec = ParamSpec {
    kind: ParamKind::Role,
    name: "role",
    choices: &[
        "kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp",
    ],
    help: "the part",
};
const MACRO: ParamSpec = ParamSpec {
    kind: ParamKind::Macro,
    name: "macro",
    choices: &[],
    help: "macro id",
};
const FXP: ParamSpec = ParamSpec {
    kind: ParamKind::FxParam,
    name: "param",
    choices: &[],
    help: "FX unit control",
};
const SAMPLE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "sample",
    choices: &[],
    help: "audio file: absolute, or relative to $DJ_SAMPLES",
};
const QUANT: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "quant",
    choices: &["now", "beat", "bar"],
    help: "when it lands",
};
const BARS: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "bars",
    choices: &[],
    help: "length in bars (a loop file; the tempo follows)",
};
const BPM: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "bpm",
    choices: &[],
    help: "the file's own tempo: it is time-stretched to the master (a vocal)",
};
const GAIN: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "gain",
    choices: &[],
    help: "linear gain, default 1",
};
const CHOKE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "choke",
    choices: &[],
    help: "choke group name: starting a pad cuts the others in the group",
};
const BUS: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "bus",
    choices: &["L"],
    help: "the bus it plays on (only L, the loop channel, exists so far)",
};

const AUX_STRIP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "strip",
    choices: &[],
    help: "an input strip's label (empty: the deck above; A, B and L work too)",
};
const AUX_KEY: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "key",
    choices: &["reverb-size", "reverb-damp", "reverb-wet", "delay-time", "delay-feedback", "delay-wet"],
    help: "which aux effect slider (beats' ids)",
};

const POST_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &[
        "next",
        "prev",
        "none",
        "clean-medium",
        "clean-light",
        "clean-strong",
        "expander",
        "treble-shelf",
        "master",
    ],
    help: "next / prev step the preset; a preset's name picks it (docs/POST.md)",
};

const MODE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "mode",
    choices: &["keep-tempo", "tape"],
    help: "keep-tempo: pitch shift on the bar grid; tape: plain resample, the speed follows",
};

const INPUT: ParamSpec = ParamSpec {
    kind: ParamKind::FxParam,
    name: "input",
    choices: &[],
    help: "an input strip's label, or V (the vocoder, post-fader) / M (the master) / a deck \
           (empty: the input a deck's jog is aimed at, else the first)",
};
const SOURCE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "source",
    choices: &["V", "M"],
    help: "record this instead of the deck: V (the vocoder, post-fader), M (the master) or an \
           input strip's label",
};

const CLOCK_SOURCE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "source",
    choices: &["usb", "internal", "auto", "read"],
    help: "usb / internal: set the TD-3's clock source now; auto: follow the hub's clock master; read: read the config back",
};

const OUT_DEVICE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "device",
    choices: &["td3", "pd705"],
    help: "a gear id, or part of an output port name",
};
const OUT_CHANNEL: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "channel",
    choices: &[],
    help: "1..16 (the gear's own note channel by default)",
};
const OUT_TYPE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "type",
    choices: &["note", "cc", "pc"],
    help: "what to send (default note)",
};
const OUT_NOTE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "note",
    choices: &[],
    help: "the note; a key plays its own when this is empty. Also: transpose, velocity (1..127 or touch), hold (gate or ms), controller, program",
};

const LAT_INPUT: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "input",
    choices: &[],
    help: "an input strip's label (empty: the aimed / first one); freeze also takes any strip: A, B, L, V, M",
};
const SNAP_DECK: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "deck",
    choices: &["A", "B"],
    help: "the deck whose jog is aimed at a history (empty: whichever is)",
};
const LAT_RANGE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "range",
    choices: &[],
    help: "ms across the knob, \"lo,hi\" (default 0,250)",
};

const ROUTE_PART: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "part",
    choices: &crate::deck::loop_route::PARTS,
    help: "the loop's part (default melody); drums = every drum part, all = everything",
};
const ROUTE_DEST: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "dest",
    choices: &["td3", "pd705", "synth:A", "synth:B"],
    help: "where it goes (default td3); also midi:<part of a port name>[@channel]",
};
const ROUTE_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &["toggle", "on", "off", "clear"],
    help: "toggle (default), on, off; clear takes every route off. No slot: every loop playing",
};

const VOC_PARAM: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "param",
    choices: &[
        "mix", "shift", "bands", "attack", "release", "noise", "volume", "vowel", "vowel_mix",
        "synth_level", "synth_detune", "osc1_wave", "osc1_oct", "osc1_fine", "osc1_level",
        "osc2_wave", "osc2_oct", "osc2_fine", "osc2_level", "osc3_wave", "osc3_oct", "osc3_fine",
        "osc3_level", "pwm", "sync", "ring", "cutoff", "resonance", "env_amount", "env_decay",
        "synth_attack", "synth_release", "drift",
    ],
    help: "the knob the control turns (default mix)",
};
const VOC_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &[
        "on", "insert", "mute", "pfl", "bypass", "modulator", "modulator_prev", "carrier",
        "carrier_prev", "synth", "synth_mod", "swap", "freeze", "xfade", "capture", "preset",
        "preset_next", "preset_prev", "select", "select_next", "select_prev", "add", "remove",
    ],
    help: "select (`instance`) / select_next / select_prev pick the vocoder the controls move, add adds one, remove takes the selected (or `instance`) away; on / insert / mute / pfl / bypass / freeze (hold the bands) / xfade (the crossfader morphs the vocoder) toggle; modulator, carrier step the source (_prev back); swap exchanges them; synth makes the synth the carrier, synth_mod the modulator; capture takes the last beats of `input` as the modulator loop; preset loads the synth preset `preset` (none: the next), preset_next / preset_prev step",
};
const VOC_PRESET: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "preset",
    choices: &["classic", "acid", "pad", "sync lead", "ring bell", "noise sweep"],
    help: "op preset: the synth preset to load (a factory one or one saved; empty: the next)",
};
const VOC_INPUT: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "input",
    choices: &[],
    help: "capture: the input strip's label (empty: the aimed / first); also beats",
};
const VOC_INSTANCE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "instance",
    choices: &["0", "1", "2", "3"],
    help: "which vocoder (0 is V, 1 V2, …; empty: the selected one)",
};
const VOC_TRANSPOSE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "transpose",
    choices: &[],
    help: "semitones added to each key (default 0)",
};

const FREEZE_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &["toggle", "on", "off", "hold", "quantize"],
    help: "toggle (default, latched), on, off, hold (frozen while held), quantize (toggles freezing on the beat)",
};
const FREEZE_ARGS: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "beats",
    choices: &["0.25", "0.5", "1", "2", "4", "8", "16"],
    help: "beats to freeze (default the freeze's own); also quant = beat / now",
};
const FREEZE_PARAM: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "param",
    choices: &["position", "size", "density", "spread", "jitter", "mix", "beats"],
    help: "the knob the control turns (default position)",
};

const TRICK_KIND: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "kind",
    choices: &["stutter", "pump", "delay", "wash", "verb_freeze", "tape_stop", "spin_back", "half_time", "double_time", "filter", "td3_accent", "td3_slide", "td3_cutoff", "riser"],
    help: "the trick (docs/TRICKS.md)",
};
const AUTO_NEXT_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &["toggle", "on", "off", "up", "down", "set"],
    help: "toggle (default), on, off; up / down: the next bar count of 4 8 16 32 64; set: to bars",
};
const AUTO_NEXT_BARS: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "bars",
    choices: &["4", "8", "16", "32", "64"],
    help: "bars between steps (1-1024) for set, or with toggle / on",
};
const TRICK_MODE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "mode",
    choices: &["momentary", "latched"],
    help: "momentary (default): on while held; latched: each press toggles",
};
const TRICK_ARGS: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "quant",
    choices: &["now", "32nd", "16th", "8th", "beat", "bar"],
    help: "when it lands (default: the trick's own grid); also div (4..32), rise (st), amount (0-1), bars, source (auto, grid, A, B), beats (delay time), feedback, res (filter resonance)",
};

const HEOS_ZONE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "zone",
    choices: &["Kitchen", "Patio", "Pool", "selected", "all"],
    help: "a HEOS zone's name (or part of it), a group's name, its pid, selected (the HEOS window's ticks) or all; a grouped zone's group follows only when the group is named",
};
const HEOS_MAX: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "max",
    choices: &["100", "70", "50", "30"],
    help: "the level the top of the fader sends (default 100): a ceiling, so a fader slammed up can't blast the room",
};
const HEOS_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &["toggle", "on", "off"],
    help: "toggle (default), on (muted), off",
};

const PRESET: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "preset",
    choices: &["next", "prev"],
    help: "a preset name, or next / prev",
};

const KEY_MODE: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "mode",
    choices: &["live", "transpose", "td3", "toggle"],
    help: "what the keys do: play a deck's lead voice, transpose the loops, play the TD-3; toggle: live <-> transpose",
};
const KEY_TO: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "to",
    choices: &["A", "B", "aimed", "next"],
    help: "the deck the keys play: A, B, the aimed one, or the other one",
};
const VOX_OP: ParamSpec = ParamSpec {
    kind: ParamKind::Arg,
    name: "op",
    choices: &["next", "prev", "first"],
    help: "which 16 files of the vox folder the pads play (bank A = the first 8 of the page, bank B = the next 8)",
};

/// What the registry knows about one target.
#[derive(Clone, Copy, Debug)]
pub struct TargetInfo {
    pub kind: DJActionKind,
    pub label: &'static str,
    /// The family the editor groups it under.
    pub family: &'static str,
    pub drive: TargetKind,
    pub params: &'static [ParamSpec],
}

use DJActionKind as K;
use TargetKind::{Continuous as Cont, Keyed, Modifier, Relative, Trigger};

/// The registry entry for `k`. Exhaustive on purpose: a new kind cannot ship unregistered.
pub fn info(k: DJActionKind) -> TargetInfo {
    let (label, family, drive, params): (&'static str, &'static str, TargetKind, &'static [ParamSpec]) =
        match k {
            K::Volume => ("Volume", "Mixer", Cont, &[DECK]),
            K::Filter => ("DJ filter", "Mixer", Cont, &[DECK]),
            K::Pitch => ("Tempo fader", "Mixer", Cont, &[DECK]),
            K::Gain => ("Gain", "Mixer", Cont, &[DECK]),
            K::EqLow => ("EQ low", "Mixer", Cont, &[DECK]),
            K::EqMid => ("EQ mid", "Mixer", Cont, &[DECK]),
            K::EqHigh => ("EQ high", "Mixer", Cont, &[DECK]),
            K::Crossfader => ("Crossfader", "Mixer", Cont, &[]),
            K::Master => ("Master", "Mixer", Cont, &[]),
            K::HeadphoneMix => ("Cue mix", "Mixer", Cont, &[]),
            K::HeadphoneLevel => ("Cue level", "Mixer", Cont, &[]),
            K::Pfl => ("Cue (PFL)", "Mixer", Trigger, &[DECK]),
            K::Play => ("Play / pause", "Transport", Trigger, &[DECK]),
            K::Cue => ("Cue", "Transport", Trigger, &[DECK]),
            K::Hotcue => ("Hot cue", "Transport", Trigger, &[DECK, SLOT]),
            K::Clearcue => ("Clear hot cue", "Transport", Trigger, &[DECK, SLOT]),
            K::LoopIn => ("Loop in", "Transport", Trigger, &[DECK]),
            K::LoopOut => ("Loop out", "Transport", Trigger, &[DECK]),
            K::LoopHalve => ("Loop ÷2", "Transport", Trigger, &[DECK]),
            K::LoopDouble => ("Loop ×2", "Transport", Trigger, &[DECK]),
            K::Sync => ("Sync", "Transport", Trigger, &[DECK, SLOT]),
            K::Quantize => ("Quantise", "Transport", Trigger, &[DECK]),
            K::Rewind => ("Rewind", "Transport", Trigger, &[DECK]),
            K::Nudge => ("Tempo nudge", "Transport", Cont, &[]),
            K::LoopRecord => ("Loop record (hold)", "Loops", Trigger, &[DECK, SOURCE]),
            K::ClipToggle => ("Loop clip on/off", "Loops", Trigger, &[SLOT]),
            K::ClipToDeck => ("Loop clip onto a deck", "Loops", Trigger, &[SLOT, DECK]),
            K::CopyDeck => ("Copy the other deck onto this one", "Transport", Trigger, &[DECK]),
            K::LoopFile => ("Loop from a file", "Loops", Trigger, &[SAMPLE, BARS, QUANT, GAIN, BUS]),
            K::OneShot => ("One-shot / vocal", "Loops", Trigger, &[SAMPLE, QUANT, GAIN, BPM, CHOKE, BUS]),
            K::Transpose => ("Transpose loops to the key", "Loops", Keyed, &[SLOT, QUANT, MODE]),
            K::LoopRoot => ("Set the loop root to the key", "Loops", Keyed, &[SLOT]),
            K::CaptureGrab => ("Grab last beats to a loop", "Capture", Trigger, &[INPUT, SLOT]),
            K::CaptureArm => ("Capture to a loop (hold)", "Capture", Trigger, &[INPUT]),
            K::CaptureBeats => ("Capture length ÷2 / ×2", "Capture", Relative, &[INPUT]),
            K::LibraryLoad => ("Library loop to the row", "Capture", Trigger, &[SLOT]),
            K::LibrarySave => ("Save a clip to the library", "Capture", Trigger, &[SLOT]),
            K::HistoryScrub => ("History scrub", "Capture", Relative, &[INPUT]),
            K::HistoryLive => ("History back to live", "Capture", Trigger, &[INPUT]),
            K::HistoryLoop => ("History loop in place", "Capture", Trigger, &[INPUT, SLOT]),
            K::Td3ClockSource => ("TD-3 clock source", "Gear", Trigger, &[CLOCK_SOURCE]),
            K::MidiOut => ("MIDI out to gear", "Gear", Keyed, &[OUT_DEVICE, OUT_CHANNEL, OUT_TYPE, OUT_NOTE]),
            K::InputLatency => ("Input latency offset", "Capture", Cont, &[LAT_INPUT, LAT_RANGE]),
            K::LoopRoute => ("Route a loop's part to gear", "Loops", Trigger, &[SLOT, ROUTE_PART, ROUTE_DEST, ROUTE_OP]),
            K::Freeze => ("Granular freeze", "Capture", Trigger, &[LAT_INPUT, FREEZE_OP, FREEZE_ARGS]),
            K::FreezeKnob => ("Granular freeze knob", "Capture", Cont, &[LAT_INPUT, FREEZE_PARAM]),
            K::HistorySnap => ("History jog: snap back to live", "Capture", Trigger, &[SNAP_DECK]),
            K::HistoryJog => ("History jog: hold to scrub the history, not the deck", "Capture", Trigger, &[SNAP_DECK]),
            K::TrickKnob => ("Performance trick knob (filter ride)", "Tricks", Cont, &[DECK, TRICK_KIND]),
            K::Trick => ("Performance trick", "Tricks", Trigger, &[DECK, TRICK_KIND, TRICK_MODE, TRICK_ARGS]),
            K::Vocoder => ("Vocoder knob", "Vocoder", Cont, &[VOC_PARAM, VOC_INSTANCE]),
            K::VocoderSwitch => ("Vocoder switch / source", "Vocoder", Trigger, &[VOC_OP, VOC_INPUT, VOC_PRESET, VOC_INSTANCE]),
            K::VocoderKeys => ("Vocoder synth keys", "Vocoder", Keyed, &[VOC_TRANSPOSE, VOC_INSTANCE]),
            K::HeosVolume => ("HEOS zone volume", "HEOS", Cont, &[HEOS_ZONE, HEOS_MAX]),
            K::HeosMute => ("HEOS zone mute", "HEOS", Trigger, &[HEOS_ZONE, HEOS_OP]),
            K::AutoNextBars => ("Auto-next every N bars", "Song", Trigger, &[DECK, AUTO_NEXT_OP, AUTO_NEXT_BARS]),
            K::AuxSend => ("Aux send", "Aux", Cont, &[DECK, AUX_STRIP]),
            K::AuxReturn => ("Aux FX return", "Aux", Cont, &[]),
            K::AuxFx => ("Aux effect slider", "Aux", Cont, &[AUX_KEY]),
            K::AuxMute => ("Aux FX return mute", "Aux", Trigger, &[]),
            K::PostToggle => ("Master post chain on/off", "Post", Trigger, &[]),
            K::PostPreset => ("Master post preset", "Post", Trigger, &[POST_OP]),
            K::Section => ("Section", "Song", Trigger, &[DECK, SLOT]),
            K::SectionPrev => ("Previous section", "Song", Trigger, &[DECK]),
            K::SectionNext => ("Next section", "Song", Trigger, &[DECK]),
            K::SoundPrev => ("Previous sound (palette)", "Song", Trigger, &[DECK]),
            K::SoundNext => ("Next sound (palette)", "Song", Trigger, &[DECK]),
            K::RoleMute => ("Mute a part", "Song", Trigger, &[DECK, ROLE]),
            K::StemMute => ("Mute a stem group", "Song", Trigger, &[DECK, ROLE]),
            K::Macro => ("Macro", "Macros", Trigger, &[DECK, MACRO]),
            K::MasterFx => ("Master FX", "FX", Cont, &[DECK, MACRO]),
            K::ReverbWet => ("Reverb wet", "FX", Cont, &[DECK]),
            K::ReverbSize => ("Reverb size", "FX", Cont, &[DECK]),
            K::FxUnit => ("FX unit control", "FX", Cont, &[DECK, FXP]),
            K::FxKill => ("FX unit kill", "FX", Trigger, &[DECK]),
            K::FxReset => ("FX unit reset", "FX", Trigger, &[DECK]),
            K::Panic => ("Panic", "Macros", Trigger, &[DECK]),
            K::Stinger => ("Hit / stinger", "Macros", Trigger, &[DECK, SLOT]),
            K::NextTrack => ("Next track", "Song", Trigger, &[DECK]),
            K::Keys => ("Live keys (see key mode)", "Performance", Keyed, &[DECK, QUANT]),
            K::KeyMode => ("Key mode: live / transpose / TD-3", "Performance", Trigger, &[KEY_MODE]),
            K::KeyDeck => ("Key deck: which deck the keys play", "Performance", Trigger, &[KEY_TO]),
            K::VoxPage => ("Vocal pads: next / previous page of the vox folder", "Loops", Trigger, &[VOX_OP]),
            K::SeedStep => ("Seed ±", "Song", Relative, &[]),
            K::DeckArm => ("Arm a deck for the browser wheel (latching)", "Song", Trigger, &[DECK]),
            K::Regenerate => ("Regenerate", "Song", Trigger, &[DECK]),
            K::AutoNext => ("Auto-next", "Song", Trigger, &[DECK]),
            K::Wander => ("Wander", "Song", Trigger, &[]),
            K::WanderPace => ("Wander pace ±", "Song", Relative, &[]),
            K::AutoMix => ("Auto-mix on/off", "Song", Trigger, &[]),
            K::AutoMixNow => ("Auto-mix now", "Song", Trigger, &[]),
            K::FocusDeck => ("Aim at a deck", "Mixer", Trigger, &[SLOT]),
            K::MapPreset => ("Switch binding preset", "Bindings", Trigger, &[PRESET]),
            K::Shift => ("Shift", "Mixer", Modifier, &[]),
            K::Unbound => ("Unbound", "—", Trigger, &[]),
        };
    TargetInfo {
        kind: k,
        label,
        family,
        drive,
        params,
    }
}

/// Every target, in the editor's order.
pub fn all() -> Vec<TargetInfo> {
    DJActionKind::ALL.iter().map(|k| info(*k)).collect()
}

/// The families, in the order of first appearance.
pub fn families() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for t in all() {
        if !v.contains(&t.family) {
            v.push(t.family);
        }
    }
    v
}

/// Targets a control of this shape can drive sensibly: a knob drives continuous targets, a
/// pad triggers, keys can drive keyed ones (and triggers, one per key).
pub fn fits(control: ControlShape, t: &TargetInfo) -> bool {
    match control {
        ControlShape::Absolute => matches!(t.drive, Cont),
        ControlShape::Button => matches!(t.drive, Trigger | Modifier),
        ControlShape::Key => matches!(t.drive, Keyed | Trigger),
        ControlShape::Wheel => matches!(t.drive, Relative | Cont),
    }
}

/// How a physical control behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlShape {
    Absolute,
    Button,
    Key,
    Wheel,
}

/// A one-line description of what `args` a binding of `k` still lacks, if it cannot work yet.
pub fn missing(k: DJActionKind, binding: &crate::control::surface::Binding) -> Option<String> {
    for p in info(k).params {
        let has = match p.kind {
            ParamKind::Deck => {
                binding.deck.is_some()
                    // keys without a deck play the key route's deck (the aimed one by default)
                    || k == K::Keys
                    || (k == K::LoopRecord && binding.args.contains_key("source"))
            }
            ParamKind::Slot => true,
            ParamKind::Role => binding.role.is_some(),
            ParamKind::Macro => binding.macro_id.is_some(),
            ParamKind::FxParam => binding.param.is_some() || p.name == "input",
            ParamKind::Arg => {
                match (k, p.name) {
                    (K::OneShot | K::LoopFile, "sample") | (K::MidiOut, "device") => {
                        binding.args.contains_key(p.name)
                    }
                    _ => true,
                }
            }
        };
        if !has {
            return Some(alloc::format!("needs {}", p.name));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_library_history_and_gear_are_bindable() {
        for (k, raw) in [
            (K::CaptureGrab, "captureGrab"),
            (K::CaptureArm, "captureArm"),
            (K::CaptureBeats, "captureBeats"),
            (K::LibraryLoad, "libraryLoad"),
            (K::LibrarySave, "librarySave"),
            (K::HistoryScrub, "historyScrub"),
            (K::HistoryLive, "historyLive"),
            (K::HistoryLoop, "historyLoop"),
            (K::Td3ClockSource, "td3ClockSource"),
            (K::MidiOut, "midiOut"),
            (K::InputLatency, "inputLatency"),
            (K::LoopRoute, "loopRoute"),
            (K::CopyDeck, "copyDeck"),
            (K::Vocoder, "vocoder"),
            (K::VocoderSwitch, "vocoderSwitch"),
            (K::VocoderKeys, "vocoderKeys"),
            (K::Freeze, "freeze"),
            (K::FreezeKnob, "freezeKnob"),
            (K::HistorySnap, "historySnap"),
            (K::HistoryJog, "historyJog"),
            (K::Trick, "trick"),
            (K::TrickKnob, "trickKnob"),
            (K::HeosVolume, "heosVolume"),
            (K::HeosMute, "heosMute"),
            (K::AutoNextBars, "autoNextBars"),
            (K::AuxSend, "auxSend"),
            (K::AuxReturn, "auxReturn"),
            (K::AuxFx, "auxFx"),
            (K::AuxMute, "auxMute"),
            (K::PostToggle, "postToggle"),
            (K::PostPreset, "postPreset"),
        ] {
            assert_eq!(k.raw(), raw);
            assert_eq!(DJActionKind::from_raw(raw), Some(k));
            let i = info(k);
            assert_ne!(i.family, "—", "{raw}");
            assert!(all().iter().any(|t| t.kind == k));
        }
        assert!(families().contains(&"Capture"));
        assert_eq!(info(K::HistorySnap).family, "Capture");
        assert_eq!(K::from_raw("historySnap"), Some(K::HistorySnap));
        assert!(families().contains(&"Vocoder"));
        assert!(families().contains(&"Tricks"));
        assert!(families().contains(&"HEOS"));
        assert!(families().contains(&"Aux"));
        assert!(fits(ControlShape::Absolute, &info(K::AuxSend)));
        assert!(fits(ControlShape::Absolute, &info(K::AuxReturn)));
        assert!(fits(ControlShape::Absolute, &info(K::AuxFx)));
        assert!(fits(ControlShape::Button, &info(K::AuxMute)));
        assert!(fits(ControlShape::Button, &info(K::PostToggle)));
        assert!(fits(ControlShape::Button, &info(K::PostPreset)));
        assert!(families().contains(&"Post"));
        assert!(fits(ControlShape::Absolute, &info(K::HeosVolume)));
        assert!(fits(ControlShape::Button, &info(K::HeosMute)));
        assert!(fits(ControlShape::Absolute, &info(K::Vocoder)));
        assert!(fits(ControlShape::Key, &info(K::VocoderKeys)));
        assert!(fits(ControlShape::Button, &info(K::VocoderSwitch)));
    }

    #[test]
    fn an_unnamed_input_is_not_missing() {
        let mut b = crate::control::surface::Binding::from_json(
            &crate::json::JsonValue::parse(r#"{"group":"pads","action":"captureGrab"}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(missing(K::CaptureGrab, &b), None);
        b.param = Some("TD-3".into());
        assert_eq!(missing(K::CaptureGrab, &b), None);
    }

    #[test]
    fn auto_next_bars_binds_with_a_deck_an_op_and_bars() {
        let b = crate::control::surface::Binding::from_json(
            &crate::json::JsonValue::parse(
                r#"{"group":"pads","index":1,"action":"autoNextBars","deck":"A","args":{"op":"toggle","bars":16}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(b.action, K::AutoNextBars);
        assert_eq!(b.args.get("bars").map(String::as_str), Some("16"));
        assert_eq!(missing(K::AutoNextBars, &b), None);
        assert_eq!(info(K::AutoNextBars).family, "Song");
        assert!(fits(ControlShape::Button, &info(K::AutoNextBars)));
    }

    #[test]
    fn loop_record_takes_a_source_instead_of_a_deck() {
        let parse = |t: &str| {
            crate::control::surface::Binding::from_json(&crate::json::JsonValue::parse(t).unwrap())
                .unwrap()
        };
        let b = parse(r#"{"group":"pads","action":"loopRecord"}"#);
        assert_eq!(missing(K::LoopRecord, &b), Some("needs deck".into()));
        let b = parse(r#"{"group":"pads","action":"loopRecord","args":{"source":"V"}}"#);
        assert_eq!(b.args.get("source").map(|s| s.as_str()), Some("V"));
        assert_eq!(missing(K::LoopRecord, &b), None);
        assert!(info(K::LoopRecord).params.iter().any(|p| p.name == "source"));
    }
}
