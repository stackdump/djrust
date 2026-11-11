//! Loop grab / record from any source (beyond Swift; docs/LOOP-SOURCES.md): the decks, as
//! LOOP always did, and also the vocoder strip `V`, the master output `M` and the input strips
//! by label.
//!
//! - **A deck** is [`Engine::grab_last_bars`] / [`Engine::loop_record`], unchanged.
//! - **An input** is the capture to loop of docs/INPUT-HISTORY.md ([`Engine::capture_grab`],
//!   [`Engine::capture_start`] / [`Engine::capture_end`]): the strip's recording, mono.
//! - **`V` and `M`** are read from [`BusTaps`](super::capture::BusTaps), stereo. `V` is taken
//!   **after its fader** by default (what it adds to the master; as an insert, the insert's
//!   output) and before it with `pre`; `M` after the master fader. They are measured on the
//!   master clock when one runs, else the master tempo, as an input capture is: a grab ends on
//!   the latest bar line; a record starts on the bar line just gone (as LOOP on a deck does) and
//!   ends on the bar after the release, or, released within a quarter second with `bars`, is
//!   exactly that many bars.
//!
//! Whatever it comes from, the take lands on the loop row, looping in phase, and is saved to
//! the loop library (when there is one) under the source's name. Every grab and record from a
//! bus is traced in words (no session op, like the input captures); a deck's or an input's is
//! traced where it always was.

use crate::looplib::{
    capture_frames, frames_since_boundary, held_bars, start_beat_of, MAX_CAPTURE_BEATS,
};

use super::capture::CaptureTap;
use super::engine::Engine;
use super::trace::Traced;
use crate::json::JsonValue;
use crate::session::arrangement::ArrangeOp;

/// A quick tap of LOOP, seconds: with `bars`, a take of exactly that many bars.
const TAP_SECONDS: f64 = 0.25;

/// A mix bus a loop can be taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    /// `V` after its fader.
    Vocoder,
    /// `V` before its fader.
    VocoderPre,
    /// `M`, after the master fader.
    Master,
    /// `L`, the loop row after its channel's fader.
    Loops,
}

impl Bus {
    /// The name a take from it carries ("V", "M").
    pub fn label(self) -> &'static str {
        match self {
            Bus::Vocoder | Bus::VocoderPre => "V",
            Bus::Master => "M",
            Bus::Loops => "L",
        }
    }

    /// The name with the tap point, for the log.
    pub fn describe(self) -> &'static str {
        match self {
            Bus::Vocoder => "V",
            Bus::VocoderPre => "V (pre-fader)",
            Bus::Master => "M",
            Bus::Loops => "L",
        }
    }
}

/// Where a loop grab or record takes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopSource {
    Deck(String),
    Bus(Bus),
    /// An input strip, by index.
    Input(usize),
}

/// A record in progress on a bus.
pub(crate) struct BusTake {
    bus: Bus,
    start: i64,
    pressed: i64,
    start_beat: Option<f64>,
    bar_frames: f64,
    fixed_bars: Option<i64>,
    released: Option<i64>,
    end: Option<i64>,
    bars: i64,
    cursor: i64,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Engine {
    /// The source a name means: a deck ("A", "B"), `V` / `vocoder` (`pre`: before its fader),
    /// `M` / `master`, or an input strip's label (exact, else ignoring case). None for anything
    /// else.
    pub fn loop_source(&self, name: &str, pre: bool) -> Option<LoopSource> {
        let n = name.trim();
        if let Some(d) = self.decks.iter().find(|d| d.name.eq_ignore_ascii_case(n)) {
            return Some(LoopSource::Deck(d.name.clone()));
        }
        match n.to_ascii_lowercase().as_str() {
            "v" | "vocoder" => {
                return Some(LoopSource::Bus(if pre { Bus::VocoderPre } else { Bus::Vocoder }))
            }
            "m" | "master" => return Some(LoopSource::Bus(Bus::Master)),
            "l" | "loops" => return Some(LoopSource::Bus(Bus::Loops)),
            _ => {}
        }
        self.input_index(n)
            .or_else(|| {
                self.inputs
                    .iter()
                    .position(|i| i.settings.label.eq_ignore_ascii_case(n))
            })
            .map(LoopSource::Input)
    }

    /// The source a capture control's `input` names when it is not an input strip: a deck or
    /// a bus. None for an input (or none named).
    pub(crate) fn non_input_source(&self, name: Option<&str>) -> Option<LoopSource> {
        match self.loop_source(name?, false)? {
            LoopSource::Input(_) => None,
            s => Some(s),
        }
    }

    /// Is `name` a bus (`V`, `M`)? Their moves are traced by the calls they make.
    pub(crate) fn names_bus(&self, name: Option<&str>) -> bool {
        name.is_some_and(|n| matches!(self.loop_source(n, false), Some(LoopSource::Bus(_))))
    }

    /// Every name [`loop_source`](Self::loop_source) takes, for an error message.
    pub fn loop_source_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        v.push("V".into());
        v.push("M".into());
        v.push("L".into());
        v.extend(self.inputs.iter().map(|i| i.settings.label.clone()));
        v
    }

    /// The name a source goes by.
    pub fn loop_source_label(&self, s: &LoopSource) -> String {
        match s {
            LoopSource::Deck(d) => d.clone(),
            LoopSource::Bus(b) => b.label().to_string(),
            LoopSource::Input(i) => self
                .inputs
                .get(*i)
                .map_or_else(String::new, |x| x.settings.label.clone()),
        }
    }

    fn bus_tap(&self, bus: Bus) -> &CaptureTap {
        match bus {
            Bus::Vocoder => &self.bus.vocoder,
            Bus::VocoderPre => &self.bus.vocoder_pre,
            Bus::Master => &self.bus.master,
            Bus::Loops => &self.bus.loops,
        }
    }

    fn source_trace(&mut self, key: String, text: String) {
        self.source_trace_op(key, text, None);
    }

    /// A bus capture in the trace, with the `source` live op that takes it again.
    fn source_trace_op(&mut self, key: String, text: String, op: Option<ArrangeOp>) {
        if !self.tracing() {
            return;
        }
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(Traced {
            entry: crate::control::action_log::Entry {
                source: via,
                key,
                text,
                step: None,
                fold: false,
            },
            ops: op.into_iter().collect(),
        });
    }

    /// Grab the last `bars` whole bars of `src` onto the loop row (and into the library).
    /// Returns the clip's id; None when the source has not played that much yet (or is a deck
    /// that is not playing).
    pub fn source_grab(&mut self, src: &LoopSource, bars: i64) -> Option<String> {
        let bars = bars.clamp(1, MAX_CAPTURE_BEATS / 4);
        match src {
            LoopSource::Deck(d) => self.grab_last_bars(d, bars),
            LoopSource::Input(i) => self.capture_grab(*i, Some((bars * 4) as f64)),
            LoopSource::Bus(b) => {
                let id = self.bus_grab(*b, bars);
                let what = match &id {
                    Some(id) => format!(
                        "grab {bars} bar{} of {} → {}",
                        if bars == 1 { "" } else { "s" },
                        b.describe(),
                        self.clip(id).map_or(id.clone(), |c| c.title.clone())
                    ),
                    None => format!("grab {bars} bars of {}: not that much yet", b.describe()),
                };
                let op = id.is_some().then(|| {
                    source_op(vec![
                        ("bus", b.label().into()),
                        ("action", "grab".into()),
                        ("bars", JsonValue::int(bars)),
                    ])
                });
                self.source_trace_op(format!("grab:{}", b.label()), what, op);
                id
            }
        }
    }

    fn bus_grab(&mut self, bus: Bus, bars: i64) -> Option<String> {
        let n_beats = bars * 4;
        let (beat, fpb) = self.capture_clock();
        let now = self.loop_frames();
        let tap = self.bus_tap(bus);
        let (oldest, written) = tap.span()?;
        // the clock's beat is at `now`: end on the bar line before it
        let back = beat.map_or(0, |b| frames_since_boundary(b, 4, fpb));
        let end = (now - back).min(written);
        let frames = capture_frames(n_beats, 60.0 * self.sample_rate / fpb, self.sample_rate);
        let start = end - frames as i64;
        if start < oldest {
            return None;
        }
        let (mut l, mut r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        if tap.copy(start, end, &mut l, &mut r) != end || l.len() != frames {
            return None;
        }
        let start_beat = beat.map(|b| start_beat_of(b, 4, n_beats));
        Some(self.finish_capture(bus.label(), n_beats, start_beat, l, Some(r)))
    }

    /// LOOP pressed (`on`) or let go on `src`. With `bars`, a release within a quarter second
    /// makes it a take of exactly that many bars (an input's tap grabs that many). True if a
    /// take started (or one was ended).
    pub fn source_record(&mut self, src: &LoopSource, on: bool, bars: Option<i64>) -> bool {
        match src {
            LoopSource::Deck(d) => self.loop_record(d, on, bars),
            LoopSource::Input(i) => {
                if on {
                    self.capture_start(*i)
                } else {
                    self.capture_end_with(*i, true, bars.map(|b| (b * 4) as f64))
                }
            }
            LoopSource::Bus(b) => {
                let ok = if on {
                    self.bus_record_start(*b, bars)
                } else {
                    self.bus_record_end(*b)
                };
                let what = format!(
                    "loop record {} {}{}",
                    b.describe(),
                    if on { "down" } else { "up" },
                    if ok { "" } else { " (nothing to do)" }
                );
                let op = ok.then(|| {
                    let mut o = vec![
                        ("bus", b.label().into()),
                        ("action", if on { "start" } else { "end" }.into()),
                    ];
                    if let (true, Some(n)) = (on, bars) {
                        o.push(("bars", JsonValue::int(n)));
                    }
                    source_op(o)
                });
                self.source_trace_op(format!("looprec:{}", b.label()), what, op);
                if !on {
                    // a take whose end has already been played finishes now
                    self.service_bus_takes();
                }
                ok
            }
        }
    }

    /// The take on a bus (either tap of `V`): bars so far and whether it was let go.
    pub fn bus_take_progress(&self, bus: Bus) -> Option<(i64, bool)> {
        let t = self.bus_takes.iter().find(|t| t.bus.label() == bus.label())?;
        let now = t.released.unwrap_or_else(|| self.loop_frames());
        let bars = if t.end.is_some() {
            t.bars
        } else {
            held_bars((now - t.start).max(1), t.bar_frames)
        };
        Some((bars, t.released.is_some()))
    }

    /// The take a source has going: bars so far and whether it was let go.
    pub fn source_take_progress(&self, src: &LoopSource) -> Option<(i64, bool)> {
        match src {
            LoopSource::Deck(d) => self.loop_take_progress(d),
            LoopSource::Input(i) => self.capture_progress(*i),
            LoopSource::Bus(b) => self.bus_take_progress(*b),
        }
    }

    fn bus_record_start(&mut self, bus: Bus, bars: Option<i64>) -> bool {
        // one take per bus name: V pre and post are the same strip
        if self.bus_takes.iter().any(|t| t.bus.label() == bus.label()) {
            return false;
        }
        let now = self.loop_frames();
        let (beat, fpb) = self.capture_clock();
        let (start, start_beat) = match beat {
            Some(b) => {
                let back = frames_since_boundary(b, 4, fpb);
                (now - back, Some((b / 4.0).floor() * 4.0))
            }
            None => (now, None),
        };
        // a bar line already gone further back than the tap holds starts now instead
        let start = match self.bus_tap(bus).span() {
            Some((oldest, _)) if start >= oldest => start,
            _ => now,
        };
        self.bus_takes.push(BusTake {
            bus,
            start,
            pressed: now,
            start_beat,
            bar_frames: 4.0 * fpb,
            fixed_bars: bars.map(|b| b.clamp(1, MAX_CAPTURE_BEATS / 4)),
            released: None,
            end: None,
            bars: 0,
            cursor: start,
            left: Vec::new(),
            right: Vec::new(),
        });
        true
    }

    fn bus_record_end(&mut self, bus: Bus) -> bool {
        let now = self.loop_frames();
        let tap = (TAP_SECONDS * self.sample_rate) as i64;
        let Some(t) = self
            .bus_takes
            .iter_mut()
            .find(|t| t.bus.label() == bus.label())
        else {
            return false;
        };
        if t.released.is_some() {
            return false;
        }
        t.released = Some(now);
        match t.fixed_bars {
            Some(b) if now - t.pressed <= tap => {
                t.bars = b;
                t.end = Some(t.start + (b as f64 * t.bar_frames).round() as i64);
            }
            _ => set_end(t, now),
        }
        true
    }

    /// Move bus takes on: copy what the buses played, end ones held past the longest take, turn
    /// finished ones into clips. Called from [`Engine::service_loops`].
    pub(crate) fn service_bus_takes(&mut self) {
        let mut k = 0;
        while k < self.bus_takes.len() {
            let bus = self.bus_takes[k].bus;
            let tap = match bus {
                Bus::Vocoder => &self.bus.vocoder,
                Bus::VocoderPre => &self.bus.vocoder_pre,
                Bus::Master => &self.bus.master,
                Bus::Loops => &self.bus.loops,
            };
            let Some((oldest, written)) = tap.span() else {
                k += 1;
                continue;
            };
            let t = &mut self.bus_takes[k];
            if t.released.is_none()
                && written > t.start
                && (written - t.start) as f64 >= (MAX_CAPTURE_BEATS / 4) as f64 * t.bar_frames
            {
                t.released = Some(written);
                set_end(t, written);
            }
            if t.cursor < oldest {
                // the ring ran past it (the service stalled): the take has a hole
                self.bus_takes.remove(k);
                continue;
            }
            let to = t.end.unwrap_or(i64::MAX);
            t.cursor = tap.copy(t.cursor, to, &mut t.left, &mut t.right);
            if t.end.is_some_and(|e| t.cursor >= e) {
                let t = self.bus_takes.remove(k);
                let n = (t.end.unwrap_or(t.start) - t.start) as usize;
                let (mut l, mut r) = (t.left, t.right);
                l.truncate(n);
                r.truncate(n);
                if l.len() == n && r.len() == n && n > 0 {
                    let id = self.finish_capture(t.bus.label(), t.bars * 4, t.start_beat, l, Some(r));
                    let title = self.clip(&id).map_or(id.clone(), |c| c.title.clone());
                    self.source_trace(
                        format!("looptake:{}", t.bus.label()),
                        format!("loop take of {} → {title}", t.bus.describe()),
                    );
                }
                continue;
            }
            k += 1;
        }
    }
}

/// Fix a released take's end: the bar after the release, counted from its start.
fn set_end(t: &mut BusTake, released: i64) {
    t.bars = held_bars((released - t.start).max(1), t.bar_frames);
    t.end = Some(t.start + (t.bars as f64 * t.bar_frames).round() as i64);
}

fn source_op(pairs: Vec<(&'static str, JsonValue)>) -> ArrangeOp {
    ArrangeOp::Live {
        method: "source".into(),
        params: JsonValue::Object(crate::json::JsonObject::from_pairs(pairs)),
    }
}
