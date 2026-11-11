//! Port of `Beats/Wave/FireEvent.swift`: what the wave engine tells the scopes.
//!
//! Both records are plain values, stamped in the deck's **frame time base**: the count of
//! frames the runner has rendered. A record stamped `frame` is exactly where in the output
//! the tick began sounding: the start-of-block frame plus the sample offset of the tick
//! inside the block. Audio seconds = frame / sampleRate.
//!
//! The Swift app moves them through lock-free `LatestRing`s; here the runner appends to
//! `WaveRunner::fires` / `anchors` (switch on with `record_events`) and the host drains
//! them, so the engine stays free of rings and threads. An anchor is written before the
//! fires of its tick.

/// `FireFlags`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FireFlags(pub u16);

impl FireFlags {
    /// The hit landed on a drum lane (a gate: kick, snare, hat, clap...), not a tonal one.
    pub const DRUM: FireFlags = FireFlags(1 << 0);
    /// The net is a stinger (`hit1`, `hit2`...), one-bar accents the performer fires.
    pub const STINGER: FireFlags = FireFlags(1 << 1);
    /// A macro (a mute timer, beat repeat, or a tempo effect) was in effect when it fired.
    pub const MACRO: FireFlags = FireFlags(1 << 2);

    pub fn contains(self, other: FireFlags) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn insert(&mut self, other: FireFlags) {
        self.0 |= other.0;
    }
}

/// `FireEvent`: one audible hit of a transition - what the engine actually plays, the same
/// rule as `onFire` in beats' JS. Muted nets and muted notes, control nets, and
/// transitions with no voice produce no event; neither do silent replays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FireEvent {
    /// Frame at which this tick's audio begins (the sample the gates were struck on).
    pub frame: i64,
    /// The graph's tick counter *after* the step that fired this: the first tick is 1.
    pub tick: i32,
    /// Which project this came from (`WaveProject::generation`).
    pub generation: i32,
    /// Index into `WaveGraph::nets` (project order) of the net that fired.
    pub net: i32,
    /// Index into that net's `trans_ids`.
    pub transition: i32,
    /// MIDI note number as it sounds.
    pub note: i16,
    /// MIDI velocity 0...127 as bound (not scaled).
    pub velocity: u8,
    /// MIDI channel 1...16 (the binding's, else the track's); 10...15 are drums.
    pub channel: u8,
    /// Length in sixteenth steps (0: the binding only has a duration in ms).
    pub dur_steps: f32,
    /// Length in ms the way the tonal voice hears it.
    pub dur_ms: f32,
    pub flags: FireFlags,
}

/// `TickAnchorFlags`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TickAnchorFlags(pub u16);

impl TickAnchorFlags {
    /// Not a step: the clock tick at which a queued project took over (at a bar boundary).
    pub const SWAP: TickAnchorFlags = TickAnchorFlags(1 << 0);
    /// This anchor does not continue the previous one: the net clock was started, paused
    /// and resumed, stopped, realigned or scratched since, or this is the first anchor.
    pub const DISCONTINUITY: TickAnchorFlags = TickAnchorFlags(1 << 1);

    pub fn contains(self, other: TickAnchorFlags) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn insert(&mut self, other: TickAnchorFlags) {
        self.0 |= other.0;
    }
}

/// `TickAnchor`: one tick of the net clock - exactly when each tick began in the frame
/// time base, including ticks that fire nothing audible and ticks that change the tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TickAnchor {
    pub frame: i64,
    /// The graph's tick counter after this tick (same meaning as `FireEvent::tick`).
    pub tick: i32,
    pub generation: i32,
    /// The net clock's tempo in BPM during this tick (after any tempo macro).
    pub tempo: f32,
    pub flags: TickAnchorFlags,
}
