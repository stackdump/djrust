//! The loop row's pure rules: `LoopLaunch.swift` (launch quanta, scenes, the countdown),
//! `LoopFollow.swift` (whose tempo the loops follow, the dominant deck) and `LoopRetune.swift`
//! (why a clip can't be rendered again, which provenance counts, how far a render goes).
//! The `DJEngine` state they hang on (the voices, the bank, the clock) is the host's; these
//! are the decisions, as functions of what the host reads.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::JsonValue;
use crate::session::types::LoopFx;

// MARK: launch quanta

/// The launch boundaries offered (beats): a beat, a bar, 2, 4 and 8 bars.
pub const LAUNCH_QUANTA: [i64; 5] = [1, 4, 8, 16, 32];
pub const DEFAULT_LOOP_QUANTUM: i64 = 4;
pub const DEFAULT_TAP_TAKE_BARS: i64 = 2;

/// "beat", "bar", "2 bars"… for a launch boundary of `beats`.
pub fn quantum_label(beats: i64) -> String {
    match beats {
        1 => "beat".to_string(),
        4 => "bar".to_string(),
        b if b % 4 == 0 => format!("{} bars", b / 4),
        b => format!("{b} beats"),
    }
}

/// `setLoopQuantum`: what loops start and stop on, in beats (any of [`LAUNCH_QUANTA`]; anything
/// else is refused: None).
pub fn valid_loop_quantum(beats: i64) -> Option<i64> {
    LAUNCH_QUANTA.contains(&beats).then_some(beats)
}

/// `setClipQuantum`: Some(Some(q)) sets, Some(None) follows the row again, None is refused.
pub fn valid_clip_quantum(beats: Option<i64>) -> Option<Option<i64>> {
    match beats {
        Some(b) if !LAUNCH_QUANTA.contains(&b) => None,
        other => Some(other),
    }
}

/// `quantum(for:)`: what clip `id` starts and stops on, in beats: its own boundary, else the
/// row's. Scenes switch on the row's whatever their clips say.
pub fn quantum_for(clip_quanta: &BTreeMap<String, i64>, row: i64, id: Option<&str>) -> i64 {
    id.and_then(|i| clip_quanta.get(i).copied()).unwrap_or(row)
}

/// `setTapTakeBars`: how many bars a quick tap of a controller's LOOP records (1…64; None:
/// none, a tap is a short hold). Some(x) = accepted, None = refused.
pub fn valid_tap_take_bars(bars: Option<i64>) -> Option<Option<i64>> {
    match bars {
        Some(b) if !(1..=64).contains(&b) => None,
        other => Some(other),
    }
}

// MARK: countdown

/// `loopCountdown`: how long a waiting clip has to go, in beats of the clock it lines up with
/// (the clip's own tempo with none), and how long it was given in all (the countdown's full
/// turn). `at` is the frame it switches at, `now` the loop channel's rendered frames,
/// `scheduled_at` the frame it was scheduled (None: `now`).
pub fn loop_countdown(
    at: i64,
    now: i64,
    scheduled_at: Option<i64>,
    frames_per_beat: f64,
    clip_bpm: f64,
    sample_rate: f64,
) -> (f64, f64) {
    let per_beat = if frames_per_beat > 0.0 {
        frames_per_beat
    } else {
        60.0 / clip_bpm * sample_rate
    };
    let left = (at - now).max(0) as f64 / per_beat;
    let total = (at - scheduled_at.unwrap_or(now)).max(0) as f64 / per_beat;
    (left, total.max(left))
}

// MARK: scenes

// One `SceneClip` and one `LoopScene`: the session's (`session::types`, the clip's FX typed)
// and the journal's checkpoint (`journal::state`). They are the same types the actions, the
// journal and the library read and write.
pub use crate::journal::state::LoopScene;
pub use crate::session::types::{scene_clips_from_json, scene_clips_json, SceneClip};

/// The scenes on the row (`DJEngine.scenes` and its operations).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Scenes {
    pub list: Vec<LoopScene>,
}

impl Scenes {
    pub fn scene(&self, name: &str) -> Option<&LoopScene> {
        self.list.iter().find(|s| s.name == name)
    }

    /// The name a new scene gets: "Scene n", the first n not taken.
    pub fn next_name(&self) -> String {
        let mut n = self.list.len() + 1;
        while self.scene(&format!("Scene {n}")).is_some() {
            n += 1;
        }
        format!("Scene {n}")
    }

    /// `saveScene`: the loops playing now (and those on their way in), in row order, as scene
    /// `name`: a new one on the end, or the one already called that, overwritten. `row` is the
    /// clips in row order with their gain and FX (None when neutral); `looping` the ids
    /// playing or on their way in. None for an empty name.
    pub fn save(
        &mut self,
        name: &str,
        row: &[(String, f64, Option<LoopFx>)],
        looping: &[String],
    ) -> Option<LoopScene> {
        if name.is_empty() {
            return None;
        }
        let s = LoopScene {
            name: name.to_string(),
            clips: row
                .iter()
                .filter(|(id, _, _)| looping.contains(id))
                .map(|(id, gain, fx)| SceneClip {
                    clip_id: id.clone(),
                    gain: *gain,
                    fx: fx.clone(),
                })
                .collect(),
        };
        match self.list.iter().position(|x| x.name == name) {
            Some(i) => self.list[i] = s.clone(),
            None => self.list.push(s.clone()),
        }
        Some(s)
    }

    /// `renameScene`: refused if `to` is empty or taken.
    pub fn rename(&mut self, name: &str, to: &str) {
        if to.is_empty() || self.scene(to).is_some() {
            return;
        }
        if let Some(i) = self.list.iter().position(|s| s.name == name) {
            self.list[i].name = to.to_string();
        }
    }

    pub fn delete(&mut self, name: &str) {
        self.list.retain(|s| s.name != name);
    }

    /// Every scene gone (a reset).
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Put saved scenes back (after any already there; a name already taken is skipped).
    pub fn restore(&mut self, restored: &[LoopScene]) {
        for s in restored {
            if self.scene(&s.name).is_none() {
                self.list.push(s.clone());
            }
        }
    }

    /// A clip replaced on the row (cropped, a sequence rewritten): scenes use the new one.
    pub fn replace_clip(&mut self, id: &str, new: &str) {
        for s in &mut self.list {
            for c in &mut s.clips {
                if c.clip_id == id {
                    c.clip_id = new.to_string();
                }
            }
        }
    }
}

/// A loop voice's state as the row sees it (`LoopState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopState {
    Off,
    Starting,
    On,
    Stopping,
}

/// What `launchScene(clips:)` does on the boundary, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneOp {
    /// `stopLoop(id, now: false, at:)` for every loop looping that the scene leaves out.
    Stop { id: String },
    /// `setClipGain(id, gain, at: sounding ? at : nil)`.
    SetGain {
        id: String,
        gain: f64,
        at_boundary: bool,
    },
    /// `setClipFx(id, fx, at: sounding ? at : nil)`.
    SetFx {
        id: String,
        fx: LoopFx,
        at_boundary: bool,
    },
    /// `startLoop(id, at:)`.
    Start { id: String },
}

/// `launchScene(clips:)`: bring exactly `wanted` in at their levels (and FX, those that carry
/// them) and take every other loop out, all at one frame. A clip already in stays in (its
/// level and FX change on the boundary); clips no longer on the row are left out.
pub fn plan_scene_launch(
    wanted: &[SceneClip],
    clip_exists: &dyn Fn(&str) -> bool,
    looping: &[String],
    state: &dyn Fn(&str) -> LoopState,
) -> Vec<SceneOp> {
    let present: Vec<&SceneClip> = wanted.iter().filter(|c| clip_exists(&c.clip_id)).collect();
    let mut ops = Vec::new();
    for id in looping {
        if !present.iter().any(|c| &c.clip_id == id) {
            ops.push(SceneOp::Stop { id: id.clone() });
        }
    }
    for s in present {
        let st = state(&s.clip_id);
        let sounding = st == LoopState::On || st == LoopState::Stopping;
        ops.push(SceneOp::SetGain {
            id: s.clip_id.clone(),
            gain: s.gain,
            at_boundary: sounding,
        });
        if let Some(fx) = &s.fx {
            ops.push(SceneOp::SetFx {
                id: s.clip_id.clone(),
                fx: fx.clone(),
                at_boundary: sounding,
            });
        }
        ops.push(SceneOp::Start {
            id: s.clip_id.clone(),
        });
    }
    ops
}

// MARK: follow

/// Whose tempo the loops under the mixer follow (`LoopFollow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopFollow {
    /// Each loop follows the deck it was taken from (else whichever deck plays).
    Source,
    /// Every loop follows the louder of the playing decks: the one the room hears.
    Louder,
}

impl LoopFollow {
    pub fn raw(self) -> &'static str {
        match self {
            LoopFollow::Source => "source",
            LoopFollow::Louder => "louder",
        }
    }
    pub fn from_raw(s: &str) -> Option<LoopFollow> {
        match s {
            "source" => Some(LoopFollow::Source),
            "louder" => Some(LoopFollow::Louder),
            _ => None,
        }
    }
}

/// How much louder another deck must be to take the loops over: 3 dB.
pub fn dominance_margin() -> f64 {
    libm::pow(10.0, 3.0 / 20.0)
}

/// `channelLevel`: how loud a deck's channel is in the master: its fader × trim × crossfader (0
/// when muted). The mixer's setting, not the audio's level, so it is steady and a replay
/// gives the same.
pub fn channel_level(mute: bool, trim: f64, volume: f64, crossfade_gain: f64) -> f64 {
    if mute {
        0.0
    } else {
        trim * volume * crossfade_gain
    }
}

/// A deck as `updateDominantDeck` sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct DominantCandidate {
    pub name: String,
    /// Playing, and with a beat state (a tempo).
    pub playing_with_tempo: bool,
    pub level: f64,
}

/// `updateDominantDeck`: the deck the loops follow under `.louder`: the louder playing deck
/// with a tempo. It only changes hands when another deck is clearly louder (3 dB: a crossfade
/// does not flap it back and forth across the middle), or when it stops or goes silent.
pub fn update_dominant_deck(decks: &[DominantCandidate], current: Option<&str>) -> Option<String> {
    let mut playing: Vec<&DominantCandidate> =
        decks.iter().filter(|d| d.playing_with_tempo).collect();
    playing.sort_by(|a, b| a.name.cmp(&b.name));
    // `max(by: <)`: the first of equals
    let mut best: Option<&DominantCandidate> = None;
    for d in &playing {
        match best {
            Some(b) if !(b.level < d.level) => {}
            _ => best = Some(d),
        }
    }
    let best = best.filter(|b| b.level > 0.0)?;
    if let Some(cur) = current.and_then(|c| playing.iter().find(|d| d.name == c)) {
        if cur.level > 0.0 && best.level < cur.level * dominance_margin() {
            return Some(cur.name.clone());
        }
    }
    Some(best.name.clone())
}

/// `follow(for:)`: whose tempo clip `id` follows: its own choice, else the row's.
pub fn follow_for(
    clip_follows: &BTreeMap<String, LoopFollow>,
    row: LoopFollow,
    id: Option<&str>,
) -> LoopFollow {
    id.and_then(|i| clip_follows.get(i).copied()).unwrap_or(row)
}

/// `anyLoopFollowsLouder`: only then is the dominant deck worth keeping up to date.
pub fn any_loop_follows_louder(
    row: LoopFollow,
    clip_follows: &BTreeMap<String, LoopFollow>,
) -> bool {
    row == LoopFollow::Louder || clip_follows.values().any(|f| *f == LoopFollow::Louder)
}

// MARK: retune

/// Why a clip can't be rendered again (`ClipRetune.Refusal`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Taken off a sample deck (or a crop of one): there is no net to play again.
    NotANet,
    /// A sequence: several takes, each its own render.
    Sequence,
    /// A take with macros running: what they did depends on when they were fired.
    Macros(Vec<String>),
    /// Its provenance is missing or names no source.
    NoProvenance,
    /// The song ended (or never reached the take's bars) in the render.
    SongEnded,
    /// The take starts further into the song than a render will go for it.
    TooFar,
}

impl Refusal {
    /// For people: why "Retune" did nothing.
    pub fn message(&self) -> String {
        match self {
            Refusal::NotANet => {
                "Taken from a sample, not a net: it has no instruments to retune.".into()
            }
            Refusal::Sequence => "A sequence: retune the loops it is made of.".into(),
            Refusal::Macros(m) => format!(
                "Taken with {} running: take it again without, to retune it.",
                m.join(", ")
            ),
            Refusal::NoProvenance => "There is no record of the track it was taken from.".into(),
            Refusal::SongEnded => "The track ends before the loop's bars.".into(),
            Refusal::TooFar => "Taken too far into its track to render again.".into(),
        }
    }
}

/// Bars of song played before the take's first beat and thrown away (in ticks).
pub const RETUNE_PREROLL_TICKS: i64 = 32;
/// The furthest into a song a render goes (ticks, 16 a bar).
pub const RETUNE_MAX_TICKS: i64 = 16 * 512;

/// `takeProvenance`: the provenance of the take a clip is made of: through crops (their beats
/// are already the clip's own `startBeat` and `beats`) and retunes (the same bars), never a
/// sequence.
pub fn take_provenance(provenance: Option<&JsonValue>) -> Result<&JsonValue, Refusal> {
    let mut p = provenance;
    while let Some(op) = p.and_then(|v| v.get("op")).and_then(JsonValue::as_str) {
        match op {
            "crop" | "retune" => p = p.and_then(|v| v.get("parent")),
            "sequence" => return Err(Refusal::Sequence),
            _ => return Err(Refusal::NoProvenance),
        }
    }
    let p = p.ok_or(Refusal::NoProvenance)?;
    if p.get("mode").and_then(JsonValue::as_str) == Some("sample") {
        return Err(Refusal::NotANet);
    }
    Ok(p)
}

/// The take's tick range in the song and where the render starts from: `(start_tick,
/// end_tick, from_tick)` (`render`).
pub fn retune_ticks(start_beat: f64, beats: i64) -> Result<(i64, i64, i64), Refusal> {
    let start = libm::round(start_beat * 4.0) as i64;
    let end = start + 4 * beats;
    if start < 0 || end > RETUNE_MAX_TICKS {
        return Err(Refusal::TooFar);
    }
    Ok((start, end, (start - RETUNE_PREROLL_TICKS).max(0)))
}

/// The most frames a retune render plays before giving up (`limit`).
pub fn retune_frame_limit(end_tick: i64, bpm: f64, sample_rate: f64) -> i64 {
    let per_tick = sample_rate * 60.0 / (bpm.max(1.0) * 4.0);
    ((end_tick + RETUNE_PREROLL_TICKS + 64) as f64 * per_tick) as i64
}

/// A tick anchor of the retune render: the step that begins at `frame` is `tick − 1`.
/// Returns `(start_frame, end_frame)` found so far among `anchors` of `(tick, frame, is_swap)`
/// for the take's `start_tick` / `end_tick`.
pub fn retune_cut(
    anchors: &[(i64, i64, bool)],
    start_tick: i64,
    end_tick: i64,
    mut start: Option<i64>,
    mut end: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    for (tick, frame, swap) in anchors {
        if *swap {
            continue;
        }
        let step = tick - 1;
        if step == start_tick && start.is_none() {
            start = Some(*frame);
        }
        if step == end_tick && start.is_some() && end.is_none() {
            end = Some(*frame);
        }
    }
    (start, end)
}
