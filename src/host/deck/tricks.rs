//! Performance tricks on the decks (beyond Swift; docs/TRICKS.md): `crate::fx::tricks` as an
//! insert on each deck's strip, after its EQ and before the vocoder, its fader, cue and loop
//! record.
//!
//! A trick is fired on or off by [`Engine::trick`]: from a `trick` pad (momentary: on while
//! held; latched: each press toggles), the socket's `trick`, `djapp_trick`. A fire lands on the
//! deck's own grid (the trick's default quantum, or the `quant` asked for) at the exact frame:
//! the render thread splits the block there. A stopped deck, or one with no beat, fires at once.
//! Every fire is traced in words.

use std::collections::BTreeMap;

use crate::fx::tricks::{
    ride_cutoff, snap_div, DubDelay, FilterRide, Pump, Riser, Stutter, TapeFx, TrickArgs,
    TrickKind, TrickSource, VerbThrow,
};
use crate::json::{JsonObject, JsonValue};

use super::engine::Engine;
use super::trace::Traced;

/// The fewest frames between a fire and its frame: the message must reach the render thread.
const LEAD_FRAMES: f64 = 384.0;

/// Main thread → one deck's tricks.
pub enum TrickMsg {
    /// Fire `kind` on or off at render frame `at` (None: the next block), at `fpb` frames per
    /// beat.
    Fire {
        kind: TrickKind,
        on: bool,
        at: Option<i64>,
        args: TrickArgs,
        fpb: f64,
    },
    /// The deck's tempo moved: frames per beat now.
    Tempo(f64),
    /// A hand on a trick's knob (the filter: -1..1).
    Knob(TrickKind, f64),
}

#[derive(Clone, Copy)]
struct Pending {
    kind: TrickKind,
    on: bool,
    at: i64,
    args: TrickArgs,
    fpb: f64,
}

/// Fires waiting for their frame, at most this many per deck.
const MAX_PENDING: usize = 16;

/// One deck's tricks on the render thread.
pub(crate) struct TrickChain {
    stutter: Stutter,
    pump: Pump,
    pump_src: Option<usize>,
    pump_div: f64,
    delay: DubDelay,
    delay_beats: f64,
    verb: VerbThrow,
    tape: TapeFx,
    filter: FilterRide,
    riser: Riser,
    pending: [Option<Pending>; MAX_PENDING],
}

impl TrickChain {
    pub(crate) fn new(sample_rate: f64) -> TrickChain {
        TrickChain {
            stutter: Stutter::new(sample_rate),
            pump: Pump::new(sample_rate),
            pump_src: None,
            pump_div: 8.0,
            delay: DubDelay::new(sample_rate),
            delay_beats: 0.75,
            verb: VerbThrow::new(sample_rate),
            tape: TapeFx::new(sample_rate),
            filter: FilterRide::new(sample_rate),
            riser: Riser::new(sample_rate),
            pending: [None; MAX_PENDING],
        }
    }

    pub(crate) fn apply(&mut self, m: TrickMsg) {
        match m {
            TrickMsg::Fire {
                kind,
                on,
                at,
                args,
                fpb,
            } => {
                let p = Pending {
                    kind,
                    on,
                    at: at.unwrap_or(i64::MIN),
                    args,
                    fpb,
                };
                // a newer fire of the same trick replaces one still waiting
                let k = self
                    .pending
                    .iter()
                    .position(|x| x.is_some_and(|x| x.kind == kind))
                    .or_else(|| self.pending.iter().position(Option::is_none));
                if let Some(k) = k {
                    self.pending[k] = Some(p);
                }
            }
            TrickMsg::Knob(TrickKind::Filter, v) => self.filter.set(v),
            TrickMsg::Knob(..) => {}
            TrickMsg::Tempo(fpb) => {
                if fpb.is_finite() && fpb > 0.0 {
                    self.pump.set_fpb(fpb, self.pump_div);
                    self.delay.set_time(self.delay_beats, fpb);
                }
            }
        }
    }

    /// The deck whose signal the pump listens to, while it listens to one.
    pub(crate) fn side_source(&self) -> Option<usize> {
        self.pump_src
            .filter(|_| self.pump.is_on() && !self.pump.on_grid())
    }

    fn start(&mut self, p: Pending) {
        match p.kind {
            TrickKind::Stutter => {
                if p.on {
                    self.stutter.start(p.args.div, p.fpb, p.args.rise);
                } else {
                    self.stutter.stop();
                }
            }
            TrickKind::Pump => {
                if p.on {
                    let (grid, src) = match p.args.source {
                        TrickSource::Deck(n) => (false, Some(n as usize)),
                        _ => (true, None),
                    };
                    self.pump_src = src;
                    self.pump_div = p.args.div;
                    self.pump.start(p.args.amount, p.args.div, p.fpb, grid);
                } else {
                    self.pump.stop();
                }
            }
            TrickKind::Delay => {
                if p.on {
                    self.delay_beats = p.args.beats;
                    self.delay
                        .throw(p.args.amount, p.args.beats, p.fpb, p.args.feedback);
                } else {
                    self.delay.release();
                }
            }
            TrickKind::Wash => {
                if p.on {
                    self.verb.wash(p.args.amount, p.args.beats, p.fpb);
                } else if self.verb.is_washing() {
                    self.verb.release();
                }
            }
            TrickKind::Riser => {
                if p.on {
                    self.riser.start(p.args.amount, p.args.bars * 4.0 * p.fpb);
                } else {
                    self.riser.stop();
                }
            }
            TrickKind::Filter => {
                if p.on {
                    self.filter.set_resonance(p.args.res);
                    self.filter
                        .ride(p.args.amount, (p.args.bars * 4.0 * p.fpb).max(1.0));
                } else {
                    // back to centre over a beat
                    self.filter.ride(0.0, p.fpb);
                }
            }
            TrickKind::TapeStop => {
                if p.on {
                    self.tape.stop(p.args.beats, p.fpb);
                } else if self.tape.is_stopping() {
                    self.tape.release();
                }
            }
            TrickKind::SpinBack => {
                if p.on {
                    self.tape.spin_back(p.args.amount, p.args.beats, p.fpb);
                } else if self.tape.is_spinning() {
                    self.tape.release();
                }
            }
            // the time tricks are the main thread's (the deck's tempo), the gear's are the
            // hub's (MIDI): never sent here
            TrickKind::HalfTime
            | TrickKind::DoubleTime
            | TrickKind::Td3Accent
            | TrickKind::Td3Slide
            | TrickKind::Td3Cutoff => {}
            TrickKind::VerbFreeze => {
                if p.on {
                    self.verb.freeze(p.args.amount, p.args.beats, p.fpb);
                } else if self.verb.is_frozen() {
                    self.verb.release();
                }
            }
        }
    }

    /// Is any trick sounding or waiting.
    fn busy(&self) -> bool {
        self.stutter.is_active()
            || self.pump.is_active()
            || self.delay.is_active()
            || self.verb.is_active()
            || self.filter.is_active()
            || self.riser.is_active()
            || self.pending.iter().any(Option::is_some)
    }

    /// Run the tricks over a deck's block, `frame` the renderer's frame at its start. Real-time
    /// safe.
    /// `side`: the pump's source deck's mono signal for this block, when it listens to one.
    pub(crate) fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        frame: i64,
        side: Option<&[f32]>,
    ) {
        if !self.busy() {
            // the tape head always listens: a spin-back reaches into the past
            self.tape.process(l, r);
            return;
        }
        let n = l.len().min(r.len());
        let mut at = 0usize;
        loop {
            // the earliest fire due inside what is left of the block
            let next = self
                .pending
                .iter()
                .enumerate()
                .filter_map(|(k, p)| p.map(|p| (k, p)))
                .filter(|(_, p)| p.at < frame + n as i64)
                .min_by_key(|(_, p)| p.at);
            let split = next.map_or(n, |(_, p)| {
                (p.at.saturating_sub(frame).max(at as i64) as usize).min(n)
            });
            self.run(
                &mut l[at..split],
                &mut r[at..split],
                side.map(|s| &s[at..split.min(s.len())]),
            );
            at = split;
            match next {
                Some((k, p)) => {
                    self.pending[k] = None;
                    self.start(p);
                }
                None => break,
            }
        }
    }

    fn run(&mut self, l: &mut [f32], r: &mut [f32], side: Option<&[f32]>) {
        if l.is_empty() {
            return;
        }
        self.stutter.process(l, r);
        // the pump after the stutter: a repeated kick still pumps in time with the source
        self.pump.process(l, r, side);
        // the filter before the echoes and the room: their tails ring out open
        self.filter.process(l, r);
        // the echoes after the pump: they ring free of it
        self.delay.process(l, r);
        // the room: a wash takes the echoes in with it
        self.verb.process(l, r);
        // the riser over the deck, after the filter and the room (it is its own sound) and
        // before the tape (a tape stop takes it down with the rest)
        self.riser.process(l, r);
        // the tape last: a stop or a spin-back takes the whole deck, effects and all
        self.tape.process(l, r);
    }

    /// Which tricks are sounding now.
    pub(crate) fn sounding(&self) -> u32 {
        let mut bits = 0;
        if self.stutter.is_on() {
            bits |= 1 << TrickKind::Stutter as u32;
        }
        if self.pump.is_on() {
            bits |= 1 << TrickKind::Pump as u32;
        }
        if self.delay.is_active() {
            bits |= 1 << TrickKind::Delay as u32;
        }
        if self.riser.is_rising() {
            bits |= 1 << TrickKind::Riser as u32;
        }
        if self.filter.is_active() {
            bits |= 1 << TrickKind::Filter as u32;
        }
        if self.tape.is_stopping() {
            bits |= 1 << TrickKind::TapeStop as u32;
        }
        if self.tape.is_spinning() {
            bits |= 1 << TrickKind::SpinBack as u32;
        }
        if self.verb.is_washing() {
            bits |= 1 << TrickKind::Wash as u32;
        }
        if self.verb.is_frozen() {
            bits |= 1 << TrickKind::VerbFreeze as u32;
        }
        bits
    }
}

/// The TD-3 tricks (docs/TRICKS.md, "TD-3"): what the hub does to the notes it sends the TD-3,
/// and the cutoff ride for gear that has a cutoff CC. The hub (djapp) reads it as it sends.
#[derive(Clone, Debug, Default)]
pub struct GearTricks {
    pub accent: bool,
    pub slide: bool,
    pub caps: crate::gear::acid::GearCaps,
    /// The cutoff ride under way (or done, holding its last value).
    pub cutoff: Option<crate::gear::acid::CcRide>,
    /// The CC value last sent, so a pass sends only a change.
    pub cutoff_sent: Option<u8>,
    pub on: BTreeMap<TrickKind, TrickArgs>,
}

impl GearTricks {
    /// The cutoff ride's value at render frame `now`.
    pub fn cutoff_now(&self, now: i64) -> Option<u8> {
        self.cutoff.map(|r| r.value_at(now))
    }

    /// The CC message due now, if the ride's value changed since the last one sent, on
    /// `channel` (1..16).
    pub fn cutoff_due(&mut self, now: i64, channel: u8) -> Option<[u8; 3]> {
        let r = self.cutoff?;
        let v = r.value_at(now);
        if now < r.start || self.cutoff_sent == Some(v) {
            return None;
        }
        self.cutoff_sent = Some(v);
        Some([0xB0 | (channel.clamp(1, 16) - 1), r.cc, v])
    }
}

/// What the main thread keeps of a deck's tricks.
#[derive(Clone, Debug, Default)]
pub struct DeckTricks {
    /// The tricks asked on, with what they were fired with.
    pub on: BTreeMap<TrickKind, TrickArgs>,
    /// The frames per beat last sent.
    pub(crate) last_fpb: f64,
    /// Half / double time: the deck's tempo over its own (None: 1).
    pub scale: Option<f64>,
    /// A change of scale waiting for its frame (None: the next service).
    pub(crate) scale_pending: Option<(f64, Option<i64>)>,
    /// The render frame the scale took effect on.
    pub(crate) scale_since: i64,
    /// Beats the deck is behind (negative: ahead of) where normal time would have it, over the
    /// half / double time since it last ran normally.
    pub(crate) scale_drift: f64,
    /// The filter knob a hand last set, -1..1.
    pub knob: f64,
    /// The render frame a riser ends on: then it is no longer on (so a latched pad starts the
    /// next one with one press).
    pub(crate) riser_end: Option<i64>,
}

impl DeckTricks {
    /// The deck's tempo over its own: 0.5 in half time, 2 in double time, else 1.
    pub fn time_scale(&self) -> f64 {
        self.scale.unwrap_or(1.0)
    }
}

/// How a fire is timed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrickQuant {
    /// The trick's own default grid.
    Default,
    Now,
    /// On the next multiple of this many beats.
    Beats(f64),
}

impl TrickQuant {
    /// "now", "beat", "bar", "16th", "8th", "4th", "32nd", or a number of beats.
    pub fn parse(s: Option<&str>) -> TrickQuant {
        match s.map(|x| x.trim().to_ascii_lowercase()) {
            None => TrickQuant::Default,
            Some(x) => match x.as_str() {
                "" | "default" => TrickQuant::Default,
                "now" | "off" | "0" => TrickQuant::Now,
                "beat" | "4th" | "1/4" => TrickQuant::Beats(1.0),
                "bar" => TrickQuant::Beats(4.0),
                "8th" | "1/8" => TrickQuant::Beats(0.5),
                "16th" | "1/16" => TrickQuant::Beats(0.25),
                "32nd" | "1/32" => TrickQuant::Beats(0.125),
                o => o
                    .parse::<f64>()
                    .ok()
                    .filter(|b| *b > 0.0)
                    .map_or(TrickQuant::Default, TrickQuant::Beats),
            },
        }
    }
}

/// Fire arguments as text (a binding's `args`, the socket's params): `div`, `rise`, `amount`,
/// `bars`, `source` (auto, grid, or a deck: `deck_index` finds it). Missing ones keep `base`.
pub fn trick_args(
    base: TrickArgs,
    get: &dyn Fn(&str) -> Option<String>,
    deck_index: &dyn Fn(&str) -> Option<usize>,
) -> TrickArgs {
    let source = match get("source").map(|s| s.trim().to_string()) {
        None => base.source,
        Some(s) if s.eq_ignore_ascii_case("grid") => TrickSource::Grid,
        Some(s) => {
            deck_index(&s.to_uppercase()).map_or(TrickSource::Auto, |i| TrickSource::Deck(i as u8))
        }
    };
    let num = |k: &str| {
        get(k).and_then(|v| {
            let v = v.trim().trim_start_matches("1/").to_string();
            v.parse::<f64>().ok().filter(|x| x.is_finite())
        })
    };
    TrickArgs {
        div: num("div").map_or(base.div, snap_div),
        rise: num("rise").map_or(base.rise, |x| x.clamp(-12.0, 12.0)),
        // -1..1: the filter's target is signed (left: low-pass); the others clamp to 0..1
        amount: num("amount").map_or(base.amount, |x| x.clamp(-1.0, 1.0)),
        res: num("res").map_or(base.res, |x| x.clamp(0.0, 1.0)),
        bars: num("bars").map_or(base.bars, |x| x.clamp(0.25, 64.0)),
        source,
        beats: num("beats").map_or(base.beats, |x| x.clamp(1.0 / 16.0, 2.0)),
        feedback: num("feedback").map_or(base.feedback, |x| x.clamp(0.0, 0.95)),
    }
}

fn describe(kind: TrickKind, a: &TrickArgs, decks: &[String]) -> String {
    match kind {
        TrickKind::Riser => format!(
            "noise riser over {} bar{} to {:.0}%",
            a.bars,
            if a.bars == 1.0 { "" } else { "s" },
            a.amount.clamp(0.0, 1.0) * 100.0
        ),
        TrickKind::Td3Accent => "Acid Bass accents".into(),
        TrickKind::Td3Slide => "Acid Bass slides".into(),
        TrickKind::Td3Cutoff => format!(
            "Acid Bass cutoff ride to {:.0}% over {} bar{}",
            a.amount.clamp(0.0, 1.0) * 100.0,
            a.bars,
            if a.bars == 1.0 { "" } else { "s" }
        ),
        TrickKind::HalfTime | TrickKind::DoubleTime => kind.name().replace('_', " "),
        TrickKind::Filter => format!(
            "filter ride to {} over {} bar{}, resonance {:.0}%",
            knob_words(a.amount),
            a.bars,
            if a.bars == 1.0 { "" } else { "s" },
            a.res * 100.0
        ),
        TrickKind::TapeStop => format!(
            "tape stop over {} beat{}",
            a.beats,
            if a.beats == 1.0 { "" } else { "s" }
        ),
        TrickKind::SpinBack => format!(
            "spin-back over {} beat{}",
            a.beats,
            if a.beats == 1.0 { "" } else { "s" }
        ),
        TrickKind::Wash => format!(
            "reverb wash-out over {} beat{}",
            a.beats,
            if a.beats == 1.0 { "" } else { "s" }
        ),
        TrickKind::VerbFreeze => format!(
            "reverb freeze ({} beat{} caught, {:.0}%)",
            a.beats,
            if a.beats == 1.0 { "" } else { "s" },
            a.amount * 100.0
        ),
        TrickKind::Delay => format!(
            "dub delay {} beat{} feedback {:.0}% send {:.0}%",
            a.beats,
            if a.beats == 1.0 { "" } else { "s" },
            a.feedback * 100.0,
            a.amount * 100.0
        ),
        TrickKind::Pump => {
            let from = match a.source {
                TrickSource::Deck(n) => format!(
                    "{}'s kick",
                    decks.get(n as usize).map_or("?", String::as_str)
                ),
                _ => "the grid".to_string(),
            };
            format!(
                "pump {:.0}% from {from}, release 1/{}",
                a.amount * 100.0,
                a.div
            )
        }
        TrickKind::Stutter => {
            let mut s = format!("stutter 1/{}", a.div);
            if a.rise != 0.0 {
                s.push_str(&format!(" rise {:+}", a.rise));
            }
            s
        }
    }
}

/// The one knob's position in words: "low-pass 800 Hz", "high-pass 2.1 kHz", "open".
pub fn knob_words(pos: f64) -> String {
    match ride_cutoff(pos) {
        None => "open".into(),
        Some((hp, hz)) => {
            let f = if hz >= 1000.0 {
                format!("{:.1} kHz", hz / 1000.0)
            } else {
                format!("{hz:.0} Hz")
            };
            format!("{} {f}", if hp { "high-pass" } else { "low-pass" })
        }
    }
}

impl Engine {
    /// A hand on `kind`'s knob on `deck`, 0..1 (the filter: 0 low-pass all the way, 0.5 open,
    /// 1 high-pass all the way). Returns the position set.
    pub fn trick_knob(&mut self, deck: &str, kind: TrickKind, v: f64) -> Result<f64, String> {
        let i = self
            .index(deck)
            .ok_or_else(|| format!("no deck {deck:?}"))?;
        if kind != TrickKind::Filter {
            return Err(format!("{} has no knob", kind.name()));
        }
        let pos = (v.clamp(0.0, 1.0) * 2.0 - 1.0).clamp(-1.0, 1.0);
        if !self.send_msg(super::engine::Msg::Trick(i, TrickMsg::Knob(kind, pos))) {
            return Err("the render queue is full".into());
        }
        self.decks[i].tricks.knob = pos;
        if self.tracing() {
            let via = if self.trace_via.is_empty() {
                "app".to_string()
            } else {
                self.trace_via.clone()
            };
            self.trace_push(Traced {
                entry: crate::control::action_log::Entry {
                    source: via,
                    key: format!("trick:{deck}:filter:knob"),
                    text: format!("{deck} filter {}", knob_words(pos)),
                    step: None,
                    fold: true,
                },
                ops: vec![crate::session::arrangement::ArrangeOp::Live {
                    method: "trick".into(),
                    params: super::vocoder::live_params([
                        ("deck", deck.into()),
                        ("kind", kind.name().into()),
                        ("action", "knob".into()),
                        ("value", JsonValue::Number(v.clamp(0.0, 1.0))),
                    ]),
                }],
            });
        }
        Ok(pos)
    }

    /// Frames per beat on `deck` now (its running tempo; the master's when it has none).
    fn deck_fpb(&self, deck: &str) -> f64 {
        let bpm = self
            .deck(deck)
            .and_then(|d| d.beat_state(self.sample_rate))
            .map(|s| s.bpm)
            .filter(|b| *b > 0.0)
            .unwrap_or_else(|| self.master_tempo().1);
        60.0 * self.sample_rate / bpm.max(1.0)
    }

    /// The render frame of `deck`'s next multiple of `q` beats, at least [`LEAD_FRAMES`] ahead.
    /// None: at once (a stopped deck, one with no beat).
    fn deck_grid_frame(&self, deck: &str, q: f64) -> Option<i64> {
        let d = self.deck(deck)?;
        if !d.playing || q <= 0.0 {
            return None;
        }
        let s = d.beat_state(self.sample_rate).filter(|s| s.bpm > 0.0)?;
        let fpb = 60.0 * self.sample_rate / s.bpm;
        let now = d
            .mirror
            .rendered_frames
            .load(std::sync::atomic::Ordering::Acquire);
        let next = (s.beat / q).ceil() * q;
        let mut ahead = (next - s.beat) * fpb;
        if ahead < LEAD_FRAMES {
            ahead += q * fpb;
        }
        Some(now + ahead.round() as i64)
    }

    /// Fire `kind` on `deck` on or off. `args` None: the ones it was last fired with (or the
    /// defaults). Returns whether it was sent.
    pub fn trick(
        &mut self,
        deck: &str,
        kind: TrickKind,
        on: bool,
        args: Option<TrickArgs>,
        quant: TrickQuant,
    ) -> Result<(), String> {
        if kind.is_gear() {
            return self.gear_trick(deck, kind, on, args, quant);
        }
        let i = self
            .index(deck)
            .ok_or_else(|| format!("no deck {deck:?}"))?;
        let mut args = args
            .or_else(|| self.decks[i].tricks.on.get(&kind).copied())
            .unwrap_or_else(|| kind.default_args());
        if kind == TrickKind::Pump {
            // auto: the other deck's kick while it plays, else the grid; a deck that is not
            // playing has no kick to hear
            args.source = match args.source {
                TrickSource::Auto => self
                    .decks
                    .iter()
                    .position(|d| d.name != deck && d.playing)
                    .map_or(TrickSource::Grid, |k| TrickSource::Deck(k as u8)),
                TrickSource::Deck(k) if k as usize >= self.decks.len() => TrickSource::Grid,
                s => s,
            };
        }
        let q = match quant {
            TrickQuant::Default => kind.default_quantum(),
            TrickQuant::Now => 0.0,
            TrickQuant::Beats(b) => b,
        };
        let at = self.deck_grid_frame(deck, q);
        let fpb = self.deck_fpb(deck);
        if let Some(scale) = kind.time_scale() {
            // the deck's tempo is the main thread's: applied when the frame comes round
            // (service_jogs, about every 10 ms), and on the way back the deck is moved by the
            // beats it lost or gained, so it lands in phase with where it would have been
            let to = if on { scale } else { 1.0 };
            self.decks[i].tricks.scale_pending = Some((to, at));
            let other = if kind == TrickKind::HalfTime {
                TrickKind::DoubleTime
            } else {
                TrickKind::HalfTime
            };
            self.decks[i].tricks.on.remove(&other);
            if on {
                self.decks[i].tricks.on.insert(kind, args);
            } else {
                self.decks[i].tricks.on.remove(&kind);
            }
            let when = if at.is_some() { " on the beat" } else { "" };
            let text = if on {
                format!("{deck} {}{when}", kind.name().replace('_', " "))
            } else {
                format!("{deck} back to normal time{when}")
            };
            let op = self.trick_op(deck, kind, on, &args, q);
            self.trick_trace(deck, kind, text, op);
            self.service_time_scale();
            return Ok(());
        }
        if !self.send_msg(super::engine::Msg::Trick(
            i,
            TrickMsg::Fire {
                kind,
                on,
                at,
                args,
                fpb,
            },
        )) {
            return Err("the render queue is full".into());
        }
        if on {
            self.decks[i].tricks.on.insert(kind, args);
        } else {
            self.decks[i].tricks.on.remove(&kind);
        }
        if kind == TrickKind::Riser {
            let now = self.decks[i]
                .mirror
                .rendered_frames
                .load(std::sync::atomic::Ordering::Acquire);
            self.decks[i].tricks.riser_end =
                on.then(|| at.unwrap_or(now) + (args.bars * 4.0 * fpb).round() as i64);
        }
        let when = if at.is_some() {
            match q {
                x if x >= 4.0 => " on the bar".to_string(),
                x if x == 1.0 => " on the beat".to_string(),
                x => format!(" on the 1/{:.0}", 4.0 / x),
            }
        } else {
            String::new()
        };
        let text = if on {
            let names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
            format!("{deck} {}{when}", describe(kind, &args, &names))
        } else {
            format!("{deck} {} off{when}", kind.name())
        };
        let op = self.trick_op(deck, kind, on, &args, q);
        self.trick_trace(deck, kind, text, op);
        Ok(())
    }

    /// A TD-3 trick (docs/TRICKS.md, "TD-3"): accent and slide shape the notes the hub sends
    /// it from now on; a cutoff ride needs gear with a cutoff CC (`gear_tricks.caps`), and is
    /// refused, saying why, for the stock TD-3. `deck` only times a ride (its grid; the focus
    /// when it names no deck).
    fn gear_trick(
        &mut self,
        deck: &str,
        kind: TrickKind,
        on: bool,
        args: Option<TrickArgs>,
        quant: TrickQuant,
    ) -> Result<(), String> {
        let args = args
            .or_else(|| self.gear_tricks.on.get(&kind).copied())
            .unwrap_or_else(|| kind.default_args());
        let deck = if self.index(deck).is_some() {
            deck.to_string()
        } else {
            self.focus.clone()
        };
        match kind {
            TrickKind::Td3Accent => self.gear_tricks.accent = on,
            TrickKind::Td3Slide => self.gear_tricks.slide = on,
            _ => {
                let Some(cc) = self.gear_tricks.caps.cutoff_cc else {
                    return Err(
                        "the Acid Bass gear has no cutoff CC: over MIDI it takes notes, accent \
                        (velocity) and slide (legato) only. Set td3 cutoff_cc for gear that \
                        has one"
                            .into(),
                    );
                };
                let q = match quant {
                    TrickQuant::Default => kind.default_quantum(),
                    TrickQuant::Now => 0.0,
                    TrickQuant::Beats(b) => b,
                };
                let now = self.loop_frames();
                let start = self.deck_grid_frame(&deck, q).unwrap_or(now);
                let fpb = self.deck_fpb(&deck);
                let from = self
                    .gear_tricks
                    .cutoff_now(now)
                    .map_or(0.5, |v| v as f64 / 127.0);
                let (to, frames) = if on {
                    (args.amount.clamp(0.0, 1.0), args.bars * 4.0 * fpb)
                } else {
                    // back to the middle over a beat
                    (0.5, fpb)
                };
                self.gear_tricks.cutoff = Some(crate::gear::acid::CcRide {
                    cc,
                    from,
                    to,
                    start,
                    frames: frames.round() as i64,
                });
            }
        }
        if on {
            self.gear_tricks.on.insert(kind, args);
        } else {
            self.gear_tricks.on.remove(&kind);
        }
        let text = match (kind, on) {
            (_, true) => describe(kind, &args, &[]),
            (TrickKind::Td3Accent, false) => "Acid Bass accents off".into(),
            (TrickKind::Td3Slide, false) => "Acid Bass slides off".into(),
            _ => "Acid Bass cutoff back to the middle".into(),
        };
        // gear: no audio of ours, so nothing for an arrangement
        self.trick_trace("TD-3", kind, text, None);
        Ok(())
    }

    /// Is `kind` on `deck` asked on.
    pub fn trick_on(&self, deck: &str, kind: TrickKind) -> bool {
        if kind.is_gear() {
            return self.gear_tricks.on.contains_key(&kind);
        }
        self.deck(deck)
            .is_some_and(|d| d.tricks.on.contains_key(&kind))
    }

    /// The `trick` live op that fires `kind` again as it was just fired: the args as resolved
    /// (a pump's `auto` source as the deck it picked) and the grid as a number of beats.
    fn trick_op(
        &self,
        deck: &str,
        kind: TrickKind,
        on: bool,
        a: &TrickArgs,
        q: f64,
    ) -> Option<crate::session::arrangement::ArrangeOp> {
        let source: JsonValue = match a.source {
            TrickSource::Auto => "auto".into(),
            TrickSource::Grid => "grid".into(),
            TrickSource::Deck(k) => self
                .decks
                .get(k as usize)
                .map_or_else(|| "grid".into(), |d| d.name.clone().into()),
        };
        Some(crate::session::arrangement::ArrangeOp::Live {
            method: "trick".into(),
            params: super::vocoder::live_params([
                ("deck", deck.into()),
                ("kind", kind.name().into()),
                ("action", if on { "on" } else { "off" }.into()),
                (
                    "quant",
                    if q > 0.0 {
                        JsonValue::Number(q)
                    } else {
                        "now".into()
                    },
                ),
                ("div", JsonValue::Number(a.div)),
                ("rise", JsonValue::Number(a.rise)),
                ("amount", JsonValue::Number(a.amount)),
                ("bars", JsonValue::Number(a.bars)),
                ("source", source),
                ("beats", JsonValue::Number(a.beats)),
                ("feedback", JsonValue::Number(a.feedback)),
                ("res", JsonValue::Number(a.res)),
            ]),
        })
    }

    fn trick_trace(
        &mut self,
        deck: &str,
        kind: TrickKind,
        text: String,
        op: Option<crate::session::arrangement::ArrangeOp>,
    ) {
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
                key: format!("trick:{deck}:{}", kind.name()),
                text,
                step: None,
                fold: false,
            },
            ops: op.into_iter().collect(),
        });
    }

    /// A `trick` binding: `kind` (args), `mode` momentary (on while held, the default) or
    /// latched (each press toggles); `quant`, `div`, `rise`, `amount`, `bars`.
    pub(crate) fn trick_target(
        &mut self,
        deck: &str,
        args: &BTreeMap<String, String>,
        pressed: bool,
    ) -> Result<(), String> {
        let arg = |k: &str| args.get(k).cloned();
        let name = arg("kind").unwrap_or_else(|| "stutter".into());
        let kind = TrickKind::parse(&name).ok_or_else(|| format!("no trick {name:?}"))?;
        let latched = matches!(arg("mode").as_deref(), Some("latched" | "latch" | "toggle"));
        let quant = TrickQuant::parse(arg("quant").as_deref());
        let names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        let fire_args = trick_args(kind.default_args(), &arg, &|d| {
            names.iter().position(|n| n == d)
        });
        if latched {
            if pressed {
                let on = !self.trick_on(deck, kind);
                self.trick(deck, kind, on, Some(fire_args), quant)?;
            }
        } else if pressed || self.trick_on(deck, kind) {
            self.trick(deck, kind, pressed, Some(fire_args), quant)?;
        }
        Ok(())
    }

    /// Land the half / double time changes whose frame has come.
    pub(crate) fn service_time_scale(&mut self) {
        for i in 0..self.decks.len() {
            let Some((to, at)) = self.decks[i].tricks.scale_pending else {
                continue;
            };
            let now = self.decks[i]
                .mirror
                .rendered_frames
                .load(std::sync::atomic::Ordering::Acquire);
            if at.is_some_and(|a| now < a) {
                continue;
            }
            let name = self.decks[i].name.clone();
            let old = self.decks[i].tricks.time_scale();
            self.decks[i].tricks.scale_pending = None;
            // the beats the deck lost (half) or gained (double) against its own tempo
            let fpb = {
                let bpm = self.decks[i].effective_bpm().unwrap_or(120.0).max(1.0);
                60.0 * self.sample_rate / bpm
            };
            let elapsed = (now - self.decks[i].tricks.scale_since).max(0) as f64 / fpb;
            if old != 1.0 && self.decks[i].playing {
                self.decks[i].tricks.scale_drift += elapsed * (1.0 - old);
            }
            self.decks[i].tricks.scale = (to != 1.0).then_some(to);
            self.decks[i].tricks.scale_since = now;
            let pitch = self.decks[i].pitch;
            if self.decks[i].sample.is_some() {
                self.set_pitch(&name, pitch);
            } else {
                self.apply_net_tempo(&name);
            }
            if to == 1.0 {
                let behind = std::mem::take(&mut self.decks[i].tricks.scale_drift);
                if behind.abs() > 1e-6 {
                    self.move_beats(&name, behind);
                }
            }
        }
    }

    /// Keep the tricks on the decks' tempo (sends on change; `sync_history_tempo` calls it).
    pub fn sync_trick_tempo(&mut self) {
        self.service_time_scale();
        // a riser that has run its length is over
        for d in self.decks.iter_mut() {
            if let Some(end) = d.tricks.riser_end {
                let now = d
                    .mirror
                    .rendered_frames
                    .load(std::sync::atomic::Ordering::Acquire);
                if now >= end {
                    d.tricks.riser_end = None;
                    d.tricks.on.remove(&TrickKind::Riser);
                }
            }
        }
        for i in 0..self.decks.len() {
            if self.decks[i].tricks.on.is_empty() {
                continue;
            }
            let name = self.decks[i].name.clone();
            let fpb = self.deck_fpb(&name);
            if (fpb - self.decks[i].tricks.last_fpb).abs() > 0.5 {
                self.decks[i].tricks.last_fpb = fpb;
                self.send_msg(super::engine::Msg::Trick(i, TrickMsg::Tempo(fpb)));
            }
        }
    }

    /// Every deck's tricks as JSON: what is asked on (with its args), what is sounding.
    pub fn tricks_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        for d in &self.decks {
            let sounding = d.mirror.tricks_sounding();
            let mut t = JsonObject::new();
            for k in TrickKind::ALL {
                let mut x = JsonObject::new();
                x.set("on", d.tricks.on.contains_key(&k).into());
                let time = k.time_scale().is_some_and(|s| d.tricks.scale == Some(s));
                x.set("sounding", (sounding & (1 << k as u32) != 0 || time).into());
                if let Some(a) = d.tricks.on.get(&k) {
                    x.set("args", a.to_json());
                }
                t.set(k.name(), JsonValue::Object(x));
            }
            o.set(d.name.clone(), JsonValue::Object(t));
        }
        let g = &self.gear_tricks;
        let mut t = JsonObject::new();
        t.set("accent", g.accent.into());
        t.set("slide", g.slide.into());
        t.set(
            "cutoff_cc",
            g.caps
                .cutoff_cc
                .map_or(JsonValue::Null, |c| JsonValue::int(c as i64)),
        );
        t.set(
            "cutoff",
            g.cutoff_now(self.loop_frames())
                .map_or(JsonValue::Null, |v| JsonValue::int(v as i64)),
        );
        o.set("TD-3", JsonValue::Object(t));
        o.set(
            "kinds",
            JsonValue::Array(TrickKind::ALL.iter().map(|k| k.name().into()).collect()),
        );
        JsonValue::Object(o)
    }
}
