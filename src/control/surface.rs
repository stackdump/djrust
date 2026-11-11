//! Button / fader surfaces: `Surface.swift` (profile model, `ControlEvent`,
//! `SurfaceDecoder`, lock keys).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::control::jog::{req, req_str, req_u8, ProfileError};
use crate::control::regex::PortPattern;
use crate::json::JsonValue;

macro_rules! action_kinds {
    ($($variant:ident => $raw:literal),+ $(,)?) => {
        /// What a control on a surface does (`DJActionKind`). Continuous actions take the
        /// control's value (0...1); momentary ones fire on press. `Unbound` parks a control
        /// on purpose (with a `reason`) so nothing is ever dropped silently.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum DJActionKind { $($variant),+ }

        impl DJActionKind {
            pub const ALL: &'static [DJActionKind] = &[$(DJActionKind::$variant),+];

            /// The raw string (`rawValue`) the profile JSON and the lock keys use.
            pub fn raw(self) -> &'static str {
                match self { $(DJActionKind::$variant => $raw),+ }
            }

            pub fn from_raw(s: &str) -> Option<DJActionKind> {
                match s { $($raw => Some(DJActionKind::$variant),)+ _ => None }
            }
        }
    };
}

action_kinds! {
    Volume => "volume",
    Filter => "filter",
    Pitch => "pitch",
    Gain => "gain",
    EqLow => "eqLow",
    EqMid => "eqMid",
    EqHigh => "eqHigh",
    Crossfader => "crossfader",
    Master => "master",
    HeadphoneMix => "headphoneMix",
    HeadphoneLevel => "headphoneLevel",
    Pfl => "pfl",
    Play => "play",
    Cue => "cue",
    Hotcue => "hotcue",
    Clearcue => "clearcue",
    LoopIn => "loopIn",
    LoopOut => "loopOut",
    LoopHalve => "loopHalve",
    LoopDouble => "loopDouble",
    LoopRecord => "loopRecord",
    ClipToggle => "clipToggle",
    CaptureGrab => "captureGrab",
    CaptureArm => "captureArm",
    CaptureBeats => "captureBeats",
    LibraryLoad => "libraryLoad",
    LibrarySave => "librarySave",
    HistoryScrub => "historyScrub",
    HistoryLive => "historyLive",
    HistoryLoop => "historyLoop",
    Td3ClockSource => "td3ClockSource",
    MidiOut => "midiOut",
    InputLatency => "inputLatency",
    LoopRoute => "loopRoute",
    DeckArm => "deckArm",
    ClipToDeck => "clipToDeck",
    CopyDeck => "copyDeck",
    Vocoder => "vocoder",
    VocoderSwitch => "vocoderSwitch",
    VocoderKeys => "vocoderKeys",
    Freeze => "freeze",
    FreezeKnob => "freezeKnob",
    HistorySnap => "historySnap",
    HistoryJog => "historyJog",
    Trick => "trick",
    TrickKnob => "trickKnob",
    HeosVolume => "heosVolume",
    HeosMute => "heosMute",
    AutoNextBars => "autoNextBars",
    AuxSend => "auxSend",
    AuxReturn => "auxReturn",
    AuxFx => "auxFx",
    AuxMute => "auxMute",
    PostToggle => "postToggle",
    PostPreset => "postPreset",
    Shift => "shift",
    Section => "section",
    SectionPrev => "sectionPrev",
    SectionNext => "sectionNext",
    SoundPrev => "soundPrev",
    SoundNext => "soundNext",
    RoleMute => "roleMute",
    StemMute => "stemMute",
    Macro => "macro",
    Panic => "panic",
    Stinger => "stinger",
    NextTrack => "nextTrack",
    Sync => "sync",
    Quantize => "quantize",
    Rewind => "rewind",
    Keys => "keys",
    KeyMode => "keyMode",
    KeyDeck => "keyDeck",
    VoxPage => "voxPage",
    Nudge => "nudge",
    SeedStep => "seedStep",
    MasterFx => "masterFx",
    ReverbWet => "reverbWet",
    ReverbSize => "reverbSize",
    FxUnit => "fxUnit",
    FxKill => "fxKill",
    FxReset => "fxReset",
    FocusDeck => "focusDeck",
    Regenerate => "regenerate",
    AutoNext => "autoNext",
    Wander => "wander",
    WanderPace => "wanderPace",
    AutoMix => "autoMix",
    AutoMixNow => "autoMixNow",
    MapPreset => "mapPreset",
    OneShot => "oneShot",
    LoopFile => "loopFile",
    Transpose => "transpose",
    LoopRoot => "loopRoot",
    Unbound => "unbound",
}

/// `SurfaceProfile.ControlType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlType {
    /// note on/off (velocity 0 = release)
    Note,
    /// 7-bit absolute
    Cc,
    /// 14-bit absolute: MSB on CC n, LSB on CC n+32
    Cc14,
    /// 7-bit two's-complement increments (0x01 = +1, 0x7F = -1)
    Relative,
    /// 0xEn, 14-bit; one control per channel
    Pitchbend,
    /// 0xCn program change (e.g. MPK pads in PROG CHANGE mode)
    Program,
}

impl ControlType {
    fn from_raw(s: &str) -> Option<ControlType> {
        Some(match s {
            "note" => ControlType::Note,
            "cc" => ControlType::Cc,
            "cc14" => ControlType::Cc14,
            "relative" => ControlType::Relative,
            "pitchbend" => ControlType::Pitchbend,
            "program" => ControlType::Program,
            _ => return None,
        })
    }
}

/// A run of controls of one type on one channel (`SurfaceProfile.Group`). Either
/// contiguous (`first ..< first + count`) or an explicit `numbers` list.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub kind: ControlType,
    pub first: Option<u8>,
    pub count: Option<u8>,
    pub numbers: Option<Vec<u8>>,
    /// Per-index names, for monitors and docs ("play", "cue", …).
    pub names: Option<Vec<String>>,
    /// 0-based MIDI channel, or None for any.
    pub channel: Option<u8>,
    pub comment: Option<String>,
}

impl Group {
    /// MIDI note/CC numbers of the group's controls, by index.
    pub fn control_numbers(&self) -> Vec<u8> {
        if let Some(n) = &self.numbers {
            return n.clone();
        }
        match (self.first, self.count) {
            (Some(first), Some(count)) => (0..count).map(|i| first.wrapping_add(i)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn name(&self, index: usize) -> Option<&str> {
        self.names.as_ref()?.get(index).map(String::as_str)
    }
}

/// `SurfaceProfile.Binding`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub group: String,
    /// Control index within the group, or None for every control in it.
    pub index: Option<usize>,
    pub action: DJActionKind,
    pub deck: Option<String>,
    pub slot: Option<i64>,
    /// Only while this surface's shift is held (true), only while it is not (false), or
    /// regardless (None).
    pub shift: Option<bool>,
    /// Why a control is `unbound` (e.g. "needs BPM — Phase 3").
    pub reason: Option<String>,
    /// `roleMute`: the role name ("kick", "bass", …).
    pub role: Option<String>,
    /// `macro`: the net macro id ("drop", "beat-repeat", …).
    pub macro_id: Option<String>,
    /// `fxUnit`: the FX unit control (`FxParam` id: "filter", "hp", "room", …).
    pub param: Option<String>,
    /// Free-form arguments for targets that take more than the fields above (a sample
    /// path, a quantise, a bar count). The registry (`control::registry`) says which a
    /// target reads, so a new feature needs no new field here.
    pub args: BTreeMap<String, String>,
}

impl Binding {
    /// One `layout` entry (the profile's and a user map's share this shape).
    pub fn from_json(b: &JsonValue) -> Result<Binding, ProfileError> {
        let action = DJActionKind::from_raw(&req_str(b, "action")?)
            .ok_or_else(|| ProfileError("unknown action".into()))?;
        Ok(Binding {
            group: req_str(b, "group")?,
            index: b.get("index").and_then(|v| v.as_f64()).map(|n| n as usize),
            action,
            deck: opt_str(b, "deck"),
            slot: b.get("slot").and_then(|v| v.as_f64()).map(|n| n as i64),
            shift: b.get("shift").and_then(|v| v.as_bool()),
            reason: opt_str(b, "reason"),
            role: opt_str(b, "role"),
            macro_id: opt_str(b, "macro"),
            param: opt_str(b, "param"),
            args: parse_args(b),
        })
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = crate::json::JsonObject::new();
        o.set("group", JsonValue::str(self.group.clone()));
        if let Some(i) = self.index {
            o.set("index", JsonValue::int(i as i64));
        }
        o.set("action", JsonValue::str(self.action.raw()));
        let mut s = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                o.set(k, JsonValue::str(v.clone()));
            }
        };
        s("deck", &self.deck);
        s("reason", &self.reason);
        s("role", &self.role);
        s("macro", &self.macro_id);
        s("param", &self.param);
        if let Some(v) = self.slot {
            o.set("slot", JsonValue::int(v));
        }
        if let Some(v) = self.shift {
            o.set("shift", JsonValue::Bool(v));
        }
        if !self.args.is_empty() {
            let a = self
                .args
                .iter()
                .map(|(k, v)| (k.clone(), JsonValue::str(v.clone())));
            o.set("args", JsonValue::Object(crate::json::JsonObject::from_pairs(a)));
        }
        JsonValue::Object(o)
    }

    /// The action this binding fires, as the lock list names it: "<deck>.<action>[.<param>]"
    /// ("A.panic", "B.hotcue.2", "A.macro.drop", "-.crossfader") (`Binding.lockKey`).
    pub fn lock_key(&self) -> String {
        // SYNC's slot is a variant (beat / bar) of the one button, not another button
        let param = self
            .macro_id
            .clone()
            .or_else(|| self.role.clone())
            .or_else(|| self.param.clone())
            .or_else(|| {
                if self.action == DJActionKind::Sync {
                    None
                } else {
                    self.slot.map(|s| s.to_string())
                }
            });
        DJActionKind::lock_key(self.deck.as_deref(), self.action, param.as_deref())
    }
}

impl DJActionKind {
    /// `DJActionKind.lockKey`.
    pub fn lock_key(deck: Option<&str>, action: DJActionKind, param: Option<&str>) -> String {
        let mut s = alloc::format!("{}.{}", deck.unwrap_or("-"), action.raw());
        if let Some(p) = param {
            s.push('.');
            s.push_str(p);
        }
        s
    }
}

/// Note-on LED feedback (`SurfaceProfile.LEDs`).
#[derive(Clone, Debug, PartialEq)]
pub struct Leds {
    pub rgb_groups: Vec<String>,
    pub single_groups: Vec<String>,
    pub rgb_dim_status: u8,
    pub rgb_full_status: u8,
    /// Status byte that blinks an RGB pad (APC: 0x9E), or None when the surface has none:
    /// then a pad that would blink shows dim.
    pub rgb_blink_status: Option<u8>,
    pub single_off: u8,
    pub single_on: u8,
    pub single_blink: u8,
    pub comment: Option<String>,
}

/// A button/fader surface (`SurfaceProfile`): pads, faders, keys, jog-wheel decks' buttons.
#[derive(Clone, Debug)]
pub struct SurfaceProfile {
    pub name: String,
    pub manufacturer: String,
    pub match_pattern: PortPattern,
    pub match_comment: Option<String>,
    pub source: Option<String>,
    /// Id of a `JogProfile` for devices with jog wheels on the same port.
    pub jog: Option<String>,
    pub groups: Vec<Group>,
    pub leds: Option<Leds>,
    pub layout: Vec<Binding>,
}

/// `"args": {"k": "v" | number | bool}` as strings.
pub(crate) fn parse_args(j: &JsonValue) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Some(o) = j.get("args").and_then(|a| a.as_object()) {
        for (k, v) in o.iter() {
            let s = match v {
                JsonValue::String(s) => s.clone(),
                JsonValue::Number(n) => crate::json::js_number_string(*n),
                JsonValue::Bool(b) => b.to_string(),
                _ => continue,
            };
            m.insert(k.to_string(), s);
        }
    }
    m
}

fn opt_str(j: &JsonValue, key: &str) -> Option<String> {
    j.get(key).and_then(|v| v.as_str()).map(String::from)
}

impl SurfaceProfile {
    pub fn from_json(text: &str) -> Result<SurfaceProfile, ProfileError> {
        let j = JsonValue::parse(text).map_err(|e| ProfileError(e.to_string()))?;
        let mut groups = Vec::new();
        for g in req(&j, "groups")?
            .as_array()
            .ok_or_else(|| ProfileError("groups".into()))?
        {
            let kind = ControlType::from_raw(&req_str(g, "type")?)
                .ok_or_else(|| ProfileError("control type".into()))?;
            let byte = |k: &str| g.get(k).and_then(|v| v.as_f64()).map(|n| n as u8);
            let bytes = |k: &str| -> Option<Vec<u8>> {
                g.get(k)?.as_array().map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_f64())
                        .map(|n| n as u8)
                        .collect()
                })
            };
            groups.push(Group {
                id: req_str(g, "id")?,
                kind,
                first: byte("first"),
                count: byte("count"),
                numbers: bytes("numbers"),
                names: g.get("names").and_then(|v| v.strings()),
                channel: byte("channel"),
                comment: opt_str(g, "comment"),
            });
        }
        let mut layout = Vec::new();
        for b in req(&j, "layout")?
            .as_array()
            .ok_or_else(|| ProfileError("layout".into()))?
        {
            layout.push(Binding::from_json(b)?);
        }
        let leds = match j.get("leds") {
            Some(l) if l.is_object_like() => Some(Leds {
                rgb_groups: l
                    .get("rgbGroups")
                    .and_then(|v| v.strings())
                    .unwrap_or_default(),
                single_groups: l
                    .get("singleGroups")
                    .and_then(|v| v.strings())
                    .unwrap_or_default(),
                rgb_dim_status: req_u8(l, "rgbDimStatus")?,
                rgb_full_status: req_u8(l, "rgbFullStatus")?,
                rgb_blink_status: l
                    .get("rgbBlinkStatus")
                    .and_then(|v| v.as_f64())
                    .map(|n| n as u8),
                single_off: req_u8(l, "singleOff")?,
                single_on: req_u8(l, "singleOn")?,
                single_blink: req_u8(l, "singleBlink")?,
                comment: opt_str(l, "comment"),
            }),
            _ => None,
        };
        Ok(SurfaceProfile {
            name: req_str(&j, "name")?,
            manufacturer: req_str(&j, "manufacturer")?,
            match_pattern: PortPattern::new(&req_str(&j, "match")?)?,
            match_comment: opt_str(&j, "matchComment"),
            source: opt_str(&j, "source"),
            jog: opt_str(&j, "jog"),
            groups,
            leds,
            layout,
        })
    }

    pub fn matches(&self, port_name: &str) -> bool {
        self.match_pattern.is_match(port_name)
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The note/CC number for a control, for LED output.
    pub fn number(&self, group: &str, index: usize) -> Option<u8> {
        self.group(group)?.control_numbers().get(index).copied()
    }

    /// Stateless decode (7-bit view; a cc14 LSB on its own decodes to None). Use
    /// [`SurfaceDecoder`] for full 14-bit resolution.
    pub fn parse(self: &Arc<Self>, data: &[u8]) -> Option<ControlEvent> {
        SurfaceDecoder::new(self.clone()).decode(data)
    }

    /// Is this control's binding (or its group) not yet seen on a real unit (`[MIXXX]` /
    /// `[UNVERIFIED]` in the group's comment or the binding's reason)?
    /// (`SurfaceProfile.isUnverified`, ControllerLayout.swift.)
    pub fn is_unverified(&self, group: &str, index: usize) -> bool {
        fn marked(s: Option<&str>) -> bool {
            s.map_or(false, |s| {
                s.contains("[MIXXX]") || s.contains("[UNVERIFIED]")
            })
        }
        let b = self
            .layout
            .iter()
            .find(|b| b.group == group && (b.index == Some(index) || b.index.is_none()));
        marked(b.and_then(|b| b.reason.as_deref()))
            || marked(self.group(group).and_then(|g| g.comment.as_deref()))
    }
}

/// `ControlEvent`.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlEvent {
    pub group: String,
    pub index: usize,
    /// 0...1 for absolute controls (7- or 14-bit), 0 for relative.
    pub value: f64,
    /// Note-on with velocity > 0, or a CC / bend value > 0.
    pub pressed: bool,
    pub is_cc: bool,
    /// Signed increments for `relative` controls, else 0.
    pub delta: i64,
    /// Control name from the profile, if it has one.
    pub name: Option<String>,
}

/// Decodes one port's messages against a profile (`SurfaceDecoder`). Stateful only for
/// 14-bit controls: the MSB is remembered so the LSB can refine it.
#[derive(Clone, Debug)]
pub struct SurfaceDecoder {
    profile: Arc<SurfaceProfile>,
    msb: BTreeMap<u16, u8>, // channel<<8 | cc → last MSB
}

impl SurfaceDecoder {
    pub fn new(profile: Arc<SurfaceProfile>) -> SurfaceDecoder {
        SurfaceDecoder {
            profile,
            msb: BTreeMap::new(),
        }
    }

    pub fn profile(&self) -> &Arc<SurfaceProfile> {
        &self.profile
    }

    pub fn decode(&mut self, data: &[u8]) -> Option<ControlEvent> {
        if data.len() < 3 {
            return None;
        }
        let status = data[0] & 0xF0;
        let ch = data[0] & 0x0F;
        let (d1, d2) = (data[1], data[2]);
        let profile = self.profile.clone();

        let event =
            |g: &Group, i: usize, value: f64, pressed: bool, cc: bool, delta: i64| ControlEvent {
                group: g.id.clone(),
                index: i,
                value,
                pressed,
                is_cc: cc,
                delta,
                name: g.name(i).map(String::from),
            };

        for g in profile
            .groups
            .iter()
            .filter(|g| g.channel.is_none() || g.channel == Some(ch))
        {
            let nums = || g.control_numbers();
            let pos = |n: u8| nums().iter().position(|x| *x == n);
            match (status, g.kind) {
                (0x90, ControlType::Note) | (0x80, ControlType::Note) => {
                    if let Some(i) = pos(d1) {
                        return Some(event(
                            g,
                            i,
                            f64::from(d2) / 127.0,
                            status == 0x90 && d2 > 0,
                            false,
                            0,
                        ));
                    }
                }
                (0xB0, ControlType::Cc) => {
                    if let Some(i) = pos(d1) {
                        return Some(event(g, i, f64::from(d2) / 127.0, d2 > 0, true, 0));
                    }
                }
                (0xB0, ControlType::Relative) => {
                    if let Some(i) = pos(d1) {
                        let delta = if d2 < 0x40 {
                            i64::from(d2)
                        } else {
                            i64::from(d2) - 0x80
                        };
                        return Some(event(g, i, 0.0, delta != 0, true, delta));
                    }
                }
                (0xB0, ControlType::Cc14) => {
                    let key = u16::from(ch) << 8;
                    if let Some(i) = pos(d1) {
                        self.msb.insert(key | u16::from(d1), d2);
                        let v = f64::from(u16::from(d2) << 7) / 16383.0;
                        return Some(event(g, i, v.min(1.0), d2 > 0, true, 0));
                    }
                    if d1 >= 32 {
                        if let Some(i) = pos(d1 - 32) {
                            let hi = self
                                .msb
                                .get(&(key | u16::from(d1 - 32)))
                                .copied()
                                .unwrap_or(0);
                            let v = f64::from(u16::from(hi) << 7 | u16::from(d2)) / 16383.0;
                            return Some(event(g, i, v, v > 0.0, true, 0));
                        }
                    }
                }
                (0xC0, ControlType::Program) => {
                    if let Some(i) = pos(d1) {
                        return Some(event(g, i, 1.0, true, false, 0));
                    }
                }
                (0xE0, ControlType::Pitchbend) => {
                    let v = f64::from(u16::from(d2) << 7 | u16::from(d1)) / 16383.0;
                    return Some(event(g, 0, v, v != 0.5, true, 0));
                }
                _ => {}
            }
        }
        None
    }
}
