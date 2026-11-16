//! `StudioShell`: what the beats studio's engine (djwasm, the `cabi` cdylib) wraps around one
//! [`WaveRunner`] on the *parity* path, `WaveOptions::default()`.
//!
//! The runner alone is djwasm 0.2.0, which renders bit for bit what beats' JS wave engine does.
//! The shell adds the studio's live controls without moving a sample of that default path:
//!
//! - **Untouched = not called.** Every stage starts off. [`StudioShell::process`] is
//!   `runner.process` plus a frame counter and `if` checks that fail until a control has been
//!   used; a feature that was never engaged runs no code at all.
//! - **A default value is not an engagement.** A control sent at its neutral value maps back to
//!   "off" (a panic with nothing running, a seek to where the net already is while stopped,
//!   swing and humanize on an instance without groove).
//! - **Nothing allocates after [`StudioShell::new`].** The event ring and the control dispatch
//!   are fixed capacity; [`StudioShell::write_events`] formats into a caller's writer.
//! - **Block-quantised.** Every call lands between two blocks; the engine's own events (a
//!   control net's `fire-macro`, a restore, a tempo move) are stamped with the frame clock.
//!
//! djwasm 0.3.0 "net clock" engages: the frame clock, the nine net macros (`NetMacro`) fired
//! by the page or by a control net's `fire-macro` (control macros are opt-in, off by default as
//! in 0.2.0, with a disabled list), the macro mask, seek, and groove (latched at construction by
//! [`FLAG_GROOVE`], as `WaveOptions.groove` is). The master chain, the aux bus and the voice
//! table (0.4) and the runner extensions (0.5) are later stages of this same shell: until they
//! exist their catalog ids answer [`ERR_UNSUPPORTED`], so a page runs them itself.

use core::fmt::Write;

use super::macros::NetMacro;
use super::runner::{WaveOptions, WaveRunner};
use crate::fx::{AuxMacro, MasterMacro};
use crate::json::{js_round, JsonValue};

/// The call did what it was asked.
pub const OK: i32 = 0;
/// The argument was not what the call wants (an id no catalog has, a bad number).
pub const ERR_ARG: i32 = -1;
/// There is no project to act on.
pub const ERR_NOT_READY: i32 = -2;
/// A catalog id (or a flag) this build knows but does not run: the page runs it itself.
pub const ERR_UNSUPPORTED: i32 = -3;

/// Construction flag: swing and humanize move notes (`WaveOptions.groove`). Latched.
pub const FLAG_GROOVE: u32 = 1 << 0;
/// Construction flag reserved for the stinger voices (0.5); refused until then.
pub const FLAG_STINGER_VOICES: u32 = 1 << 1;

/// What this build does, as `djw_features()` reports it: one bit per studio feature.
pub mod feature {
    pub const SEEK: u64 = 1 << 0;
    pub const NET_MACROS: u64 = 1 << 1;
    pub const CONTROL_MACROS: u64 = 1 << 2;
    pub const GROOVE: u64 = 1 << 3;
    pub const FRAME_CLOCK: u64 = 1 << 4;
    pub const EVENTS: u64 = 1 << 5;
    pub const FX: u64 = 1 << 6;
    pub const MASTER_MACROS: u64 = 1 << 7;
    pub const AUX_MACROS: u64 = 1 << 8;
    pub const VOICE: u64 = 1 << 9;
    pub const STRIKE: u64 = 1 << 10;
    pub const INSTRUMENT: u64 = 1 << 11;
    pub const TRANSPOSE: u64 = 1 << 12;
    pub const LOOP: u64 = 1 << 13;
    pub const STINGER_VOICES: u64 = 1 << 14;
    pub const ROUTES: u64 = 1 << 15;

    /// Every name with its bit, in bit order (for `djw_keys`).
    pub const NAMES: [(&str, u64); 16] = [
        ("SEEK", SEEK),
        ("NET_MACROS", NET_MACROS),
        ("CONTROL_MACROS", CONTROL_MACROS),
        ("GROOVE", GROOVE),
        ("FRAME_CLOCK", FRAME_CLOCK),
        ("EVENTS", EVENTS),
        ("FX", FX),
        ("MASTER_MACROS", MASTER_MACROS),
        ("AUX_MACROS", AUX_MACROS),
        ("VOICE", VOICE),
        ("STRIKE", STRIKE),
        ("INSTRUMENT", INSTRUMENT),
        ("TRANSPOSE", TRANSPOSE),
        ("LOOP", LOOP),
        ("STINGER_VOICES", STINGER_VOICES),
        ("ROUTES", ROUTES),
    ];

    /// djwasm 0.3.0, plus the live instrument change ([`INSTRUMENT`]:
    /// `StudioShell::set_instrument`, `djw_set_instrument`).
    pub const THIS_BUILD: u64 =
        SEEK | NET_MACROS | CONTROL_MACROS | GROOVE | FRAME_CLOCK | EVENTS | INSTRUMENT;
}

/// The bit a macro id takes in the disabled mask (and, for net macros, `djw_macro_mask`):
/// net macros from 0, master macros from [`MASTER_BIT`], aux macros from [`AUX_BIT`], the four
/// stinger pads (`hit1`..`hit4`) from [`HIT_BIT`].
pub const MASTER_BIT: u32 = 16;
pub const AUX_BIT: u32 = 40;
pub const HIT_BIT: u32 = 60;
/// The stinger pads of beats' catalog: not net, master or aux macros.
pub const HITS: [&str; 4] = ["hit1", "hit2", "hit3", "hit4"];

/// One id of beats' macro catalog (`lib/macros/catalog.js`, 42 ids).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogId {
    Net(NetMacro),
    Master(MasterMacro),
    Aux(AuxMacro),
    /// `hit1`..`hit4` (0..3).
    Hit(u8),
}

impl CatalogId {
    /// Allocation-free.
    pub fn resolve(id: &str) -> Option<CatalogId> {
        if let Some(m) = NetMacro::from_id(id) {
            return Some(CatalogId::Net(m));
        }
        if let Some(m) = MasterMacro::from_id(id) {
            return Some(CatalogId::Master(m));
        }
        if let Some(m) = AuxMacro::from_id(id) {
            return Some(CatalogId::Aux(m));
        }
        HITS.iter()
            .position(|h| *h == id)
            .map(|i| CatalogId::Hit(i as u8))
    }

    pub fn id(self) -> &'static str {
        match self {
            CatalogId::Net(m) => m.id(),
            CatalogId::Master(m) => m.id(),
            CatalogId::Aux(m) => m.id(),
            CatalogId::Hit(i) => HITS[i as usize],
        }
    }

    pub fn bit(self) -> u64 {
        1u64 << match self {
            CatalogId::Net(m) => m as u32,
            CatalogId::Master(m) => MASTER_BIT + m.raw() as u32,
            CatalogId::Aux(m) => AUX_BIT + m.raw() as u32,
            CatalogId::Hit(i) => HIT_BIT + i as u32,
        }
    }
}

/// Who asked for an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A page call (`djw_fire_macro`).
    Page,
    /// A control net's `fire-macro` transition.
    Control,
    /// The engine itself (a restore, a tempo move).
    Engine,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Page => "page",
            Source::Control => "control",
            Source::Engine => "engine",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A macro fire was asked for: `res` is the fire's result code ([`OK`], [`ERR_ARG`] for an
    /// id no catalog has, [`ERR_UNSUPPORTED`] for one this build leaves to the page).
    Fired,
    /// A control net asked for a macro on the disabled list: nothing happened.
    Skipped,
    /// A net macro's effect ended (its bit left the macro mask).
    Restored,
    /// The engine moved the tempo (a tempo macro, its restore, a project swap while watching).
    Tempo,
}

impl EventKind {
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Fired => "fired",
            EventKind::Skipped => "skipped",
            EventKind::Restored => "restored",
            EventKind::Tempo => "tempo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudioEvent {
    pub kind: EventKind,
    /// None: an id no catalog has (`Fired` with [`ERR_ARG`]) or no macro (`Tempo`).
    pub id: Option<CatalogId>,
    pub source: Source,
    pub result: i32,
    /// The frame clock at the first block the event affects.
    pub frame: u64,
    /// The net tick then.
    pub tick: i64,
    /// `Tempo`: the new bpm.
    pub bpm: f64,
}

/// How many events wait for the page at most; more are counted and dropped.
pub const EVENT_CAPACITY: usize = 64;
/// How many control `fire-macro`s one block dispatches at most (more are counted and dropped).
pub const CONTROL_CAPACITY: usize = 16;

/// The fixed ring the shell appends to and the page drains.
pub struct EventRing {
    buf: [StudioEvent; EVENT_CAPACITY],
    len: usize,
    dropped: u32,
}

impl EventRing {
    fn new() -> EventRing {
        EventRing {
            buf: [StudioEvent {
                kind: EventKind::Tempo,
                id: None,
                source: Source::Engine,
                result: 0,
                frame: 0,
                tick: 0,
                bpm: 0.0,
            }; EVENT_CAPACITY],
            len: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, e: StudioEvent) {
        // A tempo move the page has not read yet is replaced, not stacked: a sweep moves the
        // tempo on every block and only the latest value matters.
        if e.kind == EventKind::Tempo
            && self.len > 0
            && self.buf[self.len - 1].kind == EventKind::Tempo
        {
            self.buf[self.len - 1] = e;
            return;
        }
        if self.len < EVENT_CAPACITY {
            self.buf[self.len] = e;
            self.len += 1;
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    pub fn events(&self) -> &[StudioEvent] {
        &self.buf[..self.len]
    }

    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Waiting entries, the overflow count included as one.
    pub fn count(&self) -> u32 {
        self.len as u32 + (self.dropped > 0) as u32
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }
}

pub struct StudioShell {
    runner: WaveRunner,
    flags: u32,
    sample_rate: f64,
    opts: WaveOptions,
    /// Swing and humanize the page set on a [`FLAG_GROOVE`] instance: every project loaded
    /// after takes them instead of its own (None until [`set_groove`](Self::set_groove)).
    groove: Option<(f64, f64)>,
    /// Frames rendered while playing since the transport started from a stop, or since a seek.
    frame: u64,
    /// The next [`play`](Self::play) starts the frame clock from 0.
    fresh: bool,
    /// A control net's `fire-macro` runs in the engine (off: 0.2.0, the page does it).
    control_macros: bool,
    /// Ids a control net may not fire ([`CatalogId::bit`]).
    disabled: u64,
    /// The macro mask and the tempo are watched after each block (a macro was fired, or control
    /// macros are on); off again once nothing is in effect.
    watching: bool,
    last_mask: u32,
    last_tempo: f64,
    ring: EventRing,
    /// Control fires dropped because one block asked for more than [`CONTROL_CAPACITY`].
    control_overflow: u32,
}

impl StudioShell {
    /// `flags`: [`FLAG_GROOVE`]. [`ERR_UNSUPPORTED`] for [`FLAG_STINGER_VOICES`], [`ERR_ARG`] for
    /// any other bit. `new(sr, 0)` is exactly djwasm 0.2.0's runner.
    pub fn new(sample_rate: f64, flags: u32) -> Result<StudioShell, i32> {
        if flags & FLAG_STINGER_VOICES != 0 {
            return Err(ERR_UNSUPPORTED);
        }
        if flags & !FLAG_GROOVE != 0 {
            return Err(ERR_ARG);
        }
        let opts = if flags & FLAG_GROOVE != 0 {
            WaveOptions {
                groove: true,
                ..WaveOptions::default()
            }
        } else {
            WaveOptions::default()
        };
        let mut runner = WaveRunner::new(sample_rate, opts.clone());
        // tape-stop as the page sweeps it: whole bpm (`setTempoTransient`). Only a fired
        // tape-stop reads it, so 0.2.0's path is untouched.
        runner.set_tempo_sweep_rounding(true);
        Ok(StudioShell {
            runner,
            flags,
            sample_rate,
            opts,
            groove: None,
            frame: 0,
            fresh: true,
            control_macros: false,
            disabled: 0,
            watching: false,
            last_mask: 0,
            last_tempo: 0.0,
            ring: EventRing::new(),
            control_overflow: 0,
        })
    }

    pub fn runner(&self) -> &WaveRunner {
        &self.runner
    }

    /// The runner itself, for the calls the shell does not wrap (mute, report, marking).
    pub fn runner_mut(&mut self) -> &mut WaveRunner {
        &mut self.runner
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    // MARK: transport (the runner's, plus the frame clock)

    pub fn play(&mut self) {
        if !self.runner.playing() {
            if self.fresh || self.runner.stopped() {
                self.frame = 0;
            }
            self.fresh = false;
        }
        self.runner.play();
    }

    pub fn pause(&mut self) {
        self.runner.pause();
    }

    pub fn stop(&mut self) {
        self.runner.stop();
        self.fresh = true;
    }

    /// Load a project document: at once when stopped, at the next bar when playing. Exactly
    /// `runner.load_json` (then [`loaded`](Self::loaded)), unless the page set a groove on this
    /// instance: then the incoming project plays the page's swing and humanize, not its own,
    /// from its first tick (a project swapped in at the bar included).
    pub fn load_json(&mut self, doc: &JsonValue) {
        match self.groove {
            None => self.runner.load_json(doc),
            Some((swing, humanize)) => {
                let mut p = WaveRunner::prepare_json(doc, self.sample_rate, &self.opts);
                p.graph.project.swing = swing;
                p.graph.project.humanize = humanize;
                self.runner.load_prepared(p);
            }
        }
        self.loaded();
    }

    /// After a project load: one installed at once (not playing) starts the clock afresh.
    pub fn loaded(&mut self) {
        if !self.runner.playing() {
            self.fresh = true;
        }
    }

    /// On by default (the page's whole-bpm tape-stop). Off plays the DJ app's exact sweep:
    /// only `cabi/tests/host_oracle.rs` turns it off, to hold the rest of the port to the host
    /// bit for bit.
    pub fn set_tempo_sweep_rounding(&mut self, on: bool) {
        self.runner.set_tempo_sweep_rounding(on);
    }

    pub fn set_tempo(&mut self, bpm: f64) {
        self.runner.set_tempo(bpm);
        // the page's own move is not an engine event
        self.last_tempo = self.runner.tempo();
    }

    /// Render one block. Untouched, this is `runner.process` and a frame count.
    pub fn process(&mut self, left: &mut [f32], right: Option<&mut [f32]>, frames: usize) {
        let playing = self.runner.playing();
        self.runner.process(left, right, frames);
        if playing {
            self.frame += frames as u64;
        }
        if self.control_macros && self.runner.ticks_in_block() > 0 {
            self.dispatch_controls();
        }
        if self.watching {
            self.watch();
        }
    }

    // MARK: macros

    /// Fire a catalog macro. `duration` is in the macro's own unit (bars; ticks for `cut`), -1
    /// its default. Net macros run here ([`OK`]); every other catalog id is [`ERR_UNSUPPORTED`]
    /// in this build, as is a permanent (0) net macro (the page holds those); [`ERR_ARG`] for an
    /// id no catalog has or a duration below -1; [`ERR_NOT_READY`] with no project.
    pub fn fire_macro(&mut self, id: &str, duration: i32) -> i32 {
        let Some(c) = CatalogId::resolve(id) else {
            return ERR_ARG;
        };
        if duration < -1 {
            return ERR_ARG;
        }
        let d = (duration >= 0).then_some(duration as f64);
        self.fire(c, d, Source::Page)
    }

    fn fire(&mut self, c: CatalogId, duration: Option<f64>, source: Source) -> i32 {
        let res = match c {
            CatalogId::Net(_) if self.runner.graph().is_none() => ERR_NOT_READY,
            CatalogId::Net(_) if duration == Some(0.0) => ERR_UNSUPPORTED,
            CatalogId::Net(m) => {
                if !self.watching {
                    self.watching = true;
                    self.last_mask = self.runner.macro_snapshot().mask;
                    self.last_tempo = self.runner.tempo();
                }
                if m == NetMacro::DoubleDrop {
                    // The page's compound (`effects.js` runCompound) ignores the duration for
                    // its steps: a 2-tick cut, then 260 ms later a drop of the drop macro's own
                    // default (1 bar). Only the running mark (`markMacroRunning`) takes the
                    // duration asked for.
                    self.runner.mark_macro_running(net_ticks(m, duration));
                    self.runner.fire_macro(m, NetMacro::Drop.ticks(None));
                } else {
                    self.runner.fire_macro(m, net_ticks(m, duration));
                }
                OK
            }
            _ => ERR_UNSUPPORTED,
        };
        if res != ERR_NOT_READY {
            self.event(EventKind::Fired, Some(c), source, res, 0.0);
        }
        res
    }

    /// Let go of a latched macro. Net macros are timed, so there is nothing to let go of
    /// ([`OK`]); the latchable ones are master and aux macros, [`ERR_UNSUPPORTED`] here.
    pub fn release_macro(&mut self, id: &str) -> i32 {
        match CatalogId::resolve(id) {
            None => ERR_ARG,
            Some(CatalogId::Net(_)) => OK,
            Some(_) => ERR_UNSUPPORTED,
        }
    }

    /// Restore every macro effect now. With nothing in effect it touches nothing.
    pub fn panic(&mut self) {
        if self.runner.macro_active() {
            self.runner.panic();
            if self.watching {
                self.watch();
            }
        }
    }

    /// The net macros in effect (`NetMacro::bit`). Allocation-free.
    pub fn macro_mask(&self) -> u64 {
        if self.runner.graph().is_none() || !self.runner.macro_active() {
            return 0;
        }
        self.runner.macro_snapshot().mask as u64
    }

    pub fn set_control_macros(&mut self, on: bool) {
        self.control_macros = on;
        if on && !self.watching {
            self.watching = true;
            self.last_mask = self.macro_mask() as u32;
            self.last_tempo = self.runner.tempo();
        }
    }

    pub fn control_macros(&self) -> bool {
        self.control_macros
    }

    /// Put `id` on (or take it off) the list a control net may not fire. [`ERR_ARG`] for an id
    /// no catalog has.
    pub fn set_macro_disabled(&mut self, id: &str, disabled: bool) -> i32 {
        let Some(c) = CatalogId::resolve(id) else {
            return ERR_ARG;
        };
        if disabled {
            self.disabled |= c.bit();
        } else {
            self.disabled &= !c.bit();
        }
        OK
    }

    pub fn disabled_mask(&self) -> u64 {
        self.disabled
    }

    /// Control fires a block had no room for (more than [`CONTROL_CAPACITY`]).
    pub fn control_overflow(&self) -> u32 {
        self.control_overflow
    }

    /// The `fire-macro` controls of the tick that just began: `macro`, else the legacy
    /// `macroId`, else `targetNet` (beats' `control.macro || control.targetNet`), for
    /// `macroBars` in the macro's own unit (none: its default). Like `djw_report`, it sees the
    /// block's last tick only: at a 128-frame quantum two ticks share a block only above
    /// ~5600 bpm.
    fn dispatch_controls(&mut self) {
        let mut pending: [(Option<CatalogId>, Option<f64>); CONTROL_CAPACITY] =
            [(None, None); CONTROL_CAPACITY];
        let mut n = 0;
        if let Some(g) = self.runner.graph() {
            let count = g.control_count.min(g.controls.len()) & !1;
            for pair in g.controls[..count].chunks_exact(2) {
                let Some(net) = g.nets.get(pair[0] as usize) else {
                    continue;
                };
                let Some(tid) = net.trans_ids.get(pair[1] as usize) else {
                    continue;
                };
                let Some(c) = net.bundle.control_bindings.get(tid) else {
                    continue;
                };
                if c.action != "fire-macro" {
                    continue;
                }
                let id = nonempty(&c.macro_name)
                    .or_else(|| nonempty(&c.macro_id))
                    .or_else(|| Some(c.target_net.as_str()).filter(|s| !s.is_empty()));
                let Some(id) = id else {
                    continue;
                };
                if n < CONTROL_CAPACITY {
                    pending[n] = (CatalogId::resolve(id), c.macro_bars.filter(|b| *b > 0.0));
                    n += 1;
                } else {
                    self.control_overflow = self.control_overflow.saturating_add(1);
                }
            }
        }
        for &(c, bars) in &pending[..n] {
            match c {
                None => self.event(EventKind::Fired, None, Source::Control, ERR_ARG, 0.0),
                Some(c) if self.disabled & c.bit() != 0 => {
                    self.event(EventKind::Skipped, Some(c), Source::Control, OK, 0.0)
                }
                Some(c) => {
                    self.fire(c, bars, Source::Control);
                }
            }
        }
    }

    fn watch(&mut self) {
        let mask = self.macro_mask() as u32;
        let gone = self.last_mask & !mask;
        if gone != 0 {
            for m in NetMacro::ALL {
                if gone & m.bit() != 0 {
                    self.event(
                        EventKind::Restored,
                        Some(CatalogId::Net(m)),
                        Source::Engine,
                        OK,
                        0.0,
                    );
                }
            }
        }
        self.last_mask = mask;
        let t = self.runner.tempo();
        if t.to_bits() != self.last_tempo.to_bits() {
            self.last_tempo = t;
            self.event(EventKind::Tempo, None, Source::Engine, OK, t);
        }
        if mask == 0 && !self.control_macros && !self.runner.macro_active() {
            self.watching = false;
        }
    }

    fn event(
        &mut self,
        kind: EventKind,
        id: Option<CatalogId>,
        source: Source,
        result: i32,
        bpm: f64,
    ) {
        let tick = self.runner.graph().map_or(0, |g| g.tick);
        self.ring.push(StudioEvent {
            kind,
            id,
            source,
            result,
            frame: self.frame,
            tick,
            bpm,
        });
    }

    // MARK: events

    pub fn events(&self) -> &EventRing {
        &self.ring
    }

    pub fn event_count(&self) -> u32 {
        self.ring.count()
    }

    /// Drain the ring as JSON into `w` (allocation-free when `w` is):
    /// `[{"k","id","src","res","frame","tick"[,"bpm"]},…]`, the dropped count last as
    /// `{"k":"overflow","id":"","src":"engine","res":N,…}`.
    pub fn write_events(&mut self, w: &mut dyn Write) -> core::fmt::Result {
        w.write_char('[')?;
        let mut first = true;
        for e in self.ring.events() {
            if !first {
                w.write_char(',')?;
            }
            first = false;
            write!(
                w,
                "{{\"k\":\"{}\",\"id\":\"{}\",\"src\":\"{}\",\"res\":{},\"frame\":{},\"tick\":{}",
                e.kind.name(),
                e.id.map_or("", CatalogId::id),
                e.source.name(),
                e.result,
                e.frame,
                e.tick
            )?;
            if e.kind == EventKind::Tempo {
                write!(w, ",\"bpm\":{}", e.bpm)?;
            }
            w.write_char('}')?;
        }
        if self.ring.dropped() > 0 {
            if !first {
                w.write_char(',')?;
            }
            write!(
                w,
                "{{\"k\":\"overflow\",\"id\":\"\",\"src\":\"engine\",\"res\":{},\"frame\":{},\"tick\":{}}}",
                self.ring.dropped(),
                self.frame,
                self.runner.graph().map_or(0, |g| g.tick)
            )?;
        }
        w.write_char(']')?;
        self.ring.clear();
        Ok(())
    }

    // MARK: seek and groove

    /// Move the net to `tick` at once (beats' `fastForwardTo`): the transport stops, the tails
    /// are cut, the marking is replayed silently from tick 0, and play resumes if it was on.
    /// The frame clock restarts. Returns the tick reached (less than `tick` past a song's
    /// `stop-transport`), or a negative code.
    ///
    /// A seek to the tick the net is already on while the transport is not running is the
    /// neutral value: only the frame clock restarts, and the marking, the page's mutes, the
    /// tails and any macro in effect are left as they are (a real seek would replay from 0 and,
    /// as the worker's `fastForwardTo` does, drop the page's mutes).
    pub fn seek(&mut self, tick: f64) -> f64 {
        if !(tick.is_finite() && tick >= 0.0) {
            return ERR_ARG as f64;
        }
        let Some(now) = self.runner.graph().map(|g| g.tick) else {
            return ERR_NOT_READY as f64;
        };
        let was = self.runner.playing();
        if !was && now == tick as i64 {
            self.frame = 0;
            self.fresh = false;
            return now as f64;
        }
        self.runner.stop();
        self.runner.silence();
        if let Some(g) = self.runner.graph_mut() {
            g.fast_forward(tick as i64);
        }
        self.frame = 0;
        self.fresh = false;
        if was {
            self.runner.play();
        }
        self.runner.graph().map_or(0.0, |g| g.tick as f64)
    }

    /// Swing and humanize, 0..100 (clamped). Moves notes only on an instance built with
    /// [`FLAG_GROOVE`]; elsewhere it changes nothing and returns [`OK`]. The values stick: a
    /// project loaded later (at once, or swapped in at the bar) plays them from its first tick
    /// instead of its own ([`load_json`](Self::load_json)). [`ERR_NOT_READY`] with no project
    /// (nothing is kept then).
    pub fn set_groove(&mut self, swing: f64, humanize: f64) -> i32 {
        if !(swing.is_finite() && humanize.is_finite()) {
            return ERR_ARG;
        }
        if self.flags & FLAG_GROOVE == 0 {
            return OK;
        }
        let Some(g) = self.runner.graph_mut() else {
            return ERR_NOT_READY;
        };
        let (s, h) = (swing.clamp(0.0, 100.0), humanize.clamp(0.0, 100.0));
        g.project.swing = s;
        g.project.humanize = h;
        self.groove = Some((s, h));
        OK
    }

    /// The swing and humanize every later load takes (None: each project its own).
    pub fn groove(&self) -> Option<(f64, f64)> {
        self.groove
    }

    /// Play net `net_id` on `instrument` from the next sample on, without moving the song
    /// ([`feature::INSTRUMENT`]): the net's lanes are rebuilt as a load of the project with
    /// that instrument would build them and swapped in, on the project playing and on the one
    /// queued for the next bar ([`WaveRunner::set_instrument`] has the contract). [`OK`];
    /// [`ERR_NOT_READY`] with no project; [`ERR_ARG`] for an empty name, or an id that is not
    /// a net with lanes (a control net, a net with no MIDI bindings, an unknown id). Allocates:
    /// a message call, like a load. The old lanes wait in the runner's retire list.
    pub fn set_instrument(&mut self, net_id: &str, instrument: &str) -> i32 {
        if self.runner.project().is_none() {
            return ERR_NOT_READY;
        }
        if instrument.is_empty() {
            return ERR_ARG;
        }
        if self.runner.set_instrument(net_id, instrument) == 0 {
            return ERR_ARG;
        }
        OK
    }
}

fn nonempty(s: &Option<alloc::string::String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// Net ticks for a duration in `m`'s own unit (a bar is 16 ticks; `cut` counts ticks), rounded
/// as JS does; at least one. None: the macro's default.
pub fn net_ticks(m: NetMacro, duration: Option<f64>) -> i64 {
    match duration {
        None => m.ticks(None),
        Some(d) => {
            let unit = if m == NetMacro::Cut { 1.0 } else { 16.0 };
            (js_round(d * unit) as i64).max(1)
        }
    }
}
