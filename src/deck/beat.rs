//! Port of the pure parts of `DeckBeat.swift`: beat state, the tempo rule `sync` and `follow`
//! share, sync alignment and quantise snapping.

use super::beat_grid::BeatGrid;

/// `Deck.pitchRange`: the tempo fader's travel, ±8 %.
pub const PITCH_RANGE: f64 = 0.08;

/// Cue / loop snapping on a sample deck with a beat grid (`Quantize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantize {
    Off,
    Beat,
    Bar,
}

/// Where a deck is on its beat grid (`BeatState`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatState {
    /// The tempo the deck is running at now (BPM × tempo fader × nudge).
    pub bpm: f64,
    /// Position in beats from the grid origin (beat 0 = the downbeat of bar 0).
    pub beat: f64,
}

impl BeatState {
    pub fn beat_phase(&self) -> f64 {
        self.beat - libm::floor(self.beat)
    }
    pub fn bar_phase(&self) -> f64 {
        (self.beat / 4.0) - libm::floor(self.beat / 4.0)
    }
    /// 0…3: the beat within the bar.
    pub fn beat_in_bar(&self) -> i64 {
        libm::floor(self.bar_phase() * 4.0) as i64
    }
    pub fn bar(&self) -> i64 {
        libm::floor(self.beat / 4.0) as i64
    }

    /// `Deck.beatState` for a net deck: `tick` is the transport's tick, `tick_phase` how far
    /// into it (0…1).
    pub fn of_net(bpm: f64, tick: i64, tick_phase: f64) -> BeatState {
        let ticks = if tick == 0 {
            0.0
        } else {
            (tick - 1) as f64 + tick_phase
        };
        BeatState {
            bpm,
            beat: ticks / 4.0,
        }
    }

    /// `Deck.beatState` for a sample deck: playhead `position` in a buffer of `length`.
    pub fn of_sample(
        bpm: f64,
        grid: &BeatGrid,
        position: f64,
        length: f64,
        sample_rate: f64,
    ) -> BeatState {
        let mut p = libm::fmod(position, length);
        if p < 0.0 {
            p += length;
        }
        BeatState {
            bpm,
            beat: (p - grid.offset) / grid.samples_per_beat(sample_rate),
        }
    }
}

/// `SyncAlign`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncAlign {
    TempoOnly,
    Beat,
    Bar,
}

/// `SyncResult`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncResult {
    /// The tempo-fader position that was set (−1…1).
    pub pitch: f64,
    /// 1, or 0.5 / 2 when half or double time of the other deck fits the fader range better.
    pub multiplier: f64,
    /// The match needed more than the fader's range and was clamped.
    pub tempo_clamped: bool,
    /// How far this deck was moved to line up, in its own beats (negative = held back).
    pub shifted_beats: f64,
}

/// `Deck.tempoMatch`'s result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoMatch {
    pub mult: f64,
    pub ratio: f64,
    pub clamped: bool,
}

/// The tempo rule `sync` and `follow` share: the same tempo, or half / double time if that fits
/// the fader better. `ratio` (effective / base BPM) is clamped to the pitch range.
pub fn tempo_match(base: f64, target: f64) -> TempoMatch {
    tempo_match_in(base, target, PITCH_RANGE)
}

/// [`tempo_match`] for a tempo fader of any travel (`range` = 0.16 for ±16 %). Swift has only
/// ±8 %; the wider ranges are the hub's (docs/HUB.md, M1).
pub fn tempo_match_in(base: f64, target: f64, range: f64) -> TempoMatch {
    let mut mult = 1.0;
    let mut ratio = target / base;
    for m in [0.5, 2.0] {
        let r = target * m / base;
        if libm::fabs(r - 1.0) < libm::fabs(ratio - 1.0) {
            mult = m;
            ratio = r;
        }
    }
    TempoMatch {
        mult,
        ratio: (1.0 + range).min((1.0 - range).max(ratio)),
        clamped: libm::fabs(ratio - 1.0) > range,
    }
}

/// `Deck.sync(to:align:)`, the arithmetic: match `base` to the `target` tempo, then (unless
/// `TempoOnly`) work out how far in this deck's own beats to move so the beat or bar lines up
/// with the other deck's `you` beat position, given this deck's `me` beat position. Returns
/// the result and the new fader position; the caller applies the move.
pub fn sync_to(
    base: f64,
    target: f64,
    me: Option<BeatState>,
    you: Option<BeatState>,
    align: SyncAlign,
) -> SyncResult {
    sync_to_in(base, target, me, you, align, PITCH_RANGE)
}

/// [`sync_to`] for a tempo fader of travel `range`; `pitch` is in that fader's units (−1…1).
pub fn sync_to_in(
    base: f64,
    target: f64,
    me: Option<BeatState>,
    you: Option<BeatState>,
    align: SyncAlign,
    range: f64,
) -> SyncResult {
    let choice = tempo_match_in(base, target, range);
    let pitch = (choice.ratio - 1.0) / range;
    let mut shifted = 0.0;
    if align != SyncAlign::TempoOnly {
        if let (Some(me), Some(you)) = (me, you) {
            // Positions in the other deck's beats: with multiplier m this deck's beat is m of
            // theirs.
            let mine = me.beat * choice.mult;
            let mut err = mine - you.beat;
            let period = if align == SyncAlign::Bar { 4.0 } else { 1.0 };
            // wrap to [-period/2, period/2]
            err -= swift_round(err / period) * period;
            shifted = -err / choice.mult; // in this deck's beats
        }
    }
    SyncResult {
        pitch,
        multiplier: choice.mult,
        tempo_clamped: choice.clamped,
        shifted_beats: shifted,
    }
}

/// Swift's `.rounded()`: half away from zero.
pub fn swift_round(x: f64) -> f64 {
    libm::round(x)
}

/// `Deck.snapped`: snap a sample position to the grid.
pub fn snapped(
    position: f64,
    quantize: Quantize,
    grid: Option<&BeatGrid>,
    sample_rate: f64,
) -> f64 {
    let Some(g) = grid else { return position };
    if quantize == Quantize::Off {
        return position;
    }
    let spb = g.samples_per_beat(sample_rate);
    let unit = if quantize == Quantize::Bar {
        spb * 4.0
    } else {
        spb
    };
    g.offset + swift_round((position - g.offset) / unit) * unit
}

/// `Deck.loopBeats`: the start of a loop `beats` long taken from `position`, on the nearest
/// beat (or bar if quantising to bars), and its end.
pub fn loop_beats(
    beats: f64,
    position: f64,
    quantize: Quantize,
    grid: &BeatGrid,
    sample_rate: f64,
) -> (f64, f64) {
    let spb = grid.samples_per_beat(sample_rate);
    let unit = if quantize == Quantize::Bar {
        spb * 4.0
    } else {
        spb
    };
    let start = grid.offset + libm::floor((position - grid.offset) / unit) * unit;
    (start, start + beats * spb)
}

/// `Deck.effectiveBPM`: base × tempo fader × nudge.
pub fn effective_bpm(base: f64, pitch: f64, net_nudge: f64) -> f64 {
    effective_bpm_in(base, pitch, net_nudge, PITCH_RANGE)
}

/// [`effective_bpm`] for a tempo fader of travel `range` (the hub's wider faders, docs/HUB.md
/// M1; Swift has only ±8 %).
pub fn effective_bpm_in(base: f64, pitch: f64, net_nudge: f64, range: f64) -> f64 {
    base * (1.0 + pitch * range) * (1.0 + net_nudge)
}
