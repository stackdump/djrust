//! The action log (beyond Swift): what the hands did, in words, as the window shows it.
//! The lines read "Browser A +1 → seed 4213" or "APC pad 3: filter macro on B", not MIDI bytes.
//!
//! Pure: the host says what happened (a source, a key naming the control and its target, the
//! text, a step for a wheel or a ± button) and when (host seconds, the tempo, and the master's
//! beat if a deck sets it). The log adds the rest:
//! - a **bar marker** before the first action in a new bar (with the bars skipped since the
//!   last one), so there is no marker for each bar while nothing happens;
//! - the **beats since the last action**, on every line;
//! - the **pattern of moves** of the control being turned: its clicks this run folded into
//!   "forward 2, back 1";
//! - continuous controls (faders, knobs, nudges, scrubs) **folded**: a fader moved again is
//!   the same line, updated, with its move count.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Lines kept.
pub const LOG_CAP: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Action,
    Bar,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    /// Rises by one per line, never reused: the socket's `since`.
    pub seq: u64,
    pub kind: LineKind,
    /// Host seconds of the line (of its latest move, for a folded one).
    pub t: f64,
    /// The bar (1-based) on the master's beat, when a deck sets it.
    pub bar: Option<i64>,
    /// The master's beat, when a deck sets it.
    pub beat: Option<f64>,
    /// Beats since the action before (at the tempo then). None for the first and for markers.
    pub since: Option<f64>,
    /// The control, in words ("Browser", "APC pad 3", "Inpulse A play").
    pub source: String,
    /// What it did, in words.
    pub text: String,
    /// The control and its target: lines with the same key fold and form a run.
    pub key: String,
    /// The run of moves of this control so far ("forward 2, back 1"); empty if none.
    pub pattern: String,
    /// Moves folded into this line (1 for a single action).
    pub moves: u32,
}

/// What one action is, for [`ActionLog::record`].
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub source: String,
    pub key: String,
    pub text: String,
    /// A wheel's or a ± button's clicks (signed): it goes into the run's pattern.
    pub step: Option<i64>,
    /// A continuous control: a move right after one of the same key updates that line.
    pub fold: bool,
}

/// The log's own beat timeline: continuous from when it started, following the master deck's
/// beat while one sets it (so bars land on the music's bars), carried on by the tempo across a
/// reload, a jump or a change of master deck, and kept in the master's bar phase after one.
/// Never runs backwards. What arrangement events are stamped with.
#[derive(Clone, Debug, Default)]
pub struct BeatClock {
    beat: f64,
    last_t: Option<f64>,
    last_master: Option<f64>,
}

impl BeatClock {
    pub fn new() -> BeatClock {
        BeatClock::default()
    }

    /// The timeline's beat now.
    pub fn beat(&self) -> f64 {
        self.beat
    }

    /// Move to host time `t` at `bpm`, with the master deck's beat if one sets it. Returns
    /// the timeline's beat.
    pub fn advance(&mut self, t: f64, bpm: f64, master: Option<f64>) -> f64 {
        let dt = self.last_t.map_or(0.0, |l| (t - l).max(0.0));
        self.last_t = Some(t);
        let by_tempo = dt * bpm.max(1.0) / 60.0;
        match (self.last_master, master) {
            (Some(prev), Some(m)) if m >= prev && m - prev < by_tempo + 1.0 => {
                // the master moved on as music does: follow it exactly
                self.beat += m - prev;
            }
            (_, Some(m)) => {
                // a jump, a reload, a new master: carry on by the tempo, then take the
                // master's phase in the bar (moving forward only)
                let carried = self.beat + by_tempo;
                let bar = libm::floor(carried / 4.0) * 4.0;
                // `m.rem_euclid(4.0)`, spelled out: `f64::rem_euclid` needs std
                let r = m % 4.0;
                let mut b = bar + if r < 0.0 { r + 4.0 } else { r };
                if b < carried {
                    b += 4.0;
                }
                // a phase step of more than half a bar would be a leap: keep the tempo's
                self.beat = if b - carried <= 2.0 { b } else { carried };
            }
            (_, None) => self.beat += by_tempo,
        }
        self.last_master = master;
        self.beat
    }
}

#[derive(Clone, Debug, Default)]
pub struct ActionLog {
    lines: Vec<LogLine>,
    seq: u64,
    last_t: Option<f64>,
    last_bar: Option<i64>,
    run_key: String,
    /// (direction, clicks) segments of the run.
    run: Vec<(i8, u32)>,
}

/// A run of moves in words: `[(1, 2), (-1, 1)]` → "forward 2, back 1".
pub fn pattern(run: &[(i8, u32)]) -> String {
    run.iter()
        .map(|(d, n)| format!("{} {n}", if *d > 0 { "forward" } else { "back" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The bar (1-based) a master beat falls in.
pub fn bar_of(beat: f64) -> i64 {
    libm::floor(beat / 4.0) as i64 + 1
}

impl ActionLog {
    pub fn new() -> ActionLog {
        ActionLog::default()
    }

    pub fn lines(&self) -> &[LogLine] {
        &self.lines
    }

    /// The lines after `seq` (a folded line whose seq is older but was updated is not
    /// repeated: read [`lines`](Self::lines) for the whole picture).
    pub fn since(&self, seq: u64) -> Vec<&LogLine> {
        self.lines.iter().filter(|l| l.seq > seq).collect()
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.last_t = None;
        self.last_bar = None;
        self.run_key.clear();
        self.run.clear();
    }

    fn push(&mut self, mut l: LogLine) {
        self.seq += 1;
        l.seq = self.seq;
        self.lines.push(l);
        let over = self.lines.len().saturating_sub(LOG_CAP);
        self.lines.drain(..over);
    }

    /// One action at host time `t`, the tempo `bpm`, the master's `beat` (if a deck sets it).
    pub fn record(&mut self, t: f64, bpm: f64, beat: Option<f64>, e: Entry) {
        let bar = beat.map(bar_of);
        if let Some(b) = bar {
            if self.last_bar != Some(b) {
                let gap = self.last_bar.map(|l| b - l).filter(|g| *g > 1);
                self.push(LogLine {
                    seq: 0,
                    kind: LineKind::Bar,
                    t,
                    bar,
                    beat,
                    since: None,
                    source: String::new(),
                    text: match gap {
                        Some(g) => format!("bar {b} (+{g})"),
                        None => format!("bar {b}"),
                    },
                    key: String::new(),
                    pattern: String::new(),
                    moves: 0,
                });
                self.last_bar = Some(b);
            }
        }
        let since = self
            .last_t
            .map(|l| ((t - l).max(0.0)) * bpm.max(1.0) / 60.0);
        self.last_t = Some(t);

        // a fader moved again: the same line, updated
        if e.fold {
            if let Some(last) = self.lines.last_mut() {
                if last.kind == LineKind::Action && last.key == e.key {
                    last.text = e.text;
                    last.t = t;
                    last.beat = beat;
                    last.bar = bar;
                    last.moves += 1;
                    return;
                }
            }
        }

        // the run of moves of one control
        if e.key != self.run_key {
            self.run_key = e.key.clone();
            self.run.clear();
        }
        if let Some(d) = e.step.filter(|d| *d != 0) {
            let dir: i8 = if d > 0 { 1 } else { -1 };
            let n = d.unsigned_abs() as u32;
            match self.run.last_mut() {
                Some((ld, ln)) if *ld == dir => *ln += n,
                _ => self.run.push((dir, n)),
            }
        }
        let pat = if e.step.is_some() {
            pattern(&self.run)
        } else {
            String::new()
        };
        self.push(LogLine {
            seq: 0,
            kind: LineKind::Action,
            t,
            bar,
            beat,
            since,
            source: e.source,
            text: e.text,
            key: e.key,
            pattern: pat,
            moves: 1,
        });
    }
}

impl LogLine {
    /// The line as the window prints it: "bar 37" for a marker, else
    /// "+2.0b  Browser A +1 → seed 4213   [forward 2, back 1]".
    pub fn render(&self) -> String {
        if self.kind == LineKind::Bar {
            return format!("── {} ──", self.text);
        }
        let mut s = match self.since {
            Some(b) => format!("+{b:.1}b  "),
            None => "       ".to_string(),
        };
        s.push_str(&self.text);
        if self.moves > 1 {
            s.push_str(&format!(" (×{})", self.moves));
        }
        if !self.pattern.is_empty() {
            s.push_str(&format!("   [{}]", self.pattern));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(key: &str, text: &str, step: Option<i64>, fold: bool) -> Entry {
        Entry {
            source: key.into(),
            key: key.into(),
            text: text.into(),
            step,
            fold,
        }
    }

    #[test]
    fn a_wheel_turned_shows_its_pattern_and_the_beats_between() {
        let mut log = ActionLog::new();
        // 120 bpm: half a second is a beat
        log.record(0.0, 120.0, Some(0.0), e("browser:A", "Browser A +1 → seed 4213", Some(1), false));
        log.record(0.5, 120.0, Some(1.0), e("browser:A", "Browser A +1 → seed 4214", Some(1), false));
        log.record(2.0, 120.0, Some(4.0), e("browser:A", "Browser A -1 → seed 4213", Some(-1), false));
        let l = log.lines();
        assert_eq!(l[0].kind, LineKind::Bar);
        assert_eq!(l[0].text, "bar 1");
        assert_eq!(l[1].since, None);
        assert_eq!(l[2].since, Some(1.0));
        assert_eq!(l[2].pattern, "forward 2");
        // a new bar: a marker before the action
        assert_eq!(l[3].text, "bar 2");
        assert_eq!(l[4].pattern, "forward 2, back 1");
        assert_eq!(l[4].since, Some(3.0));
        assert!(l[4].render().starts_with("+3.0b  Browser A -1 → seed 4213"));
        assert!(l[4].render().ends_with("[forward 2, back 1]"));
    }

    #[test]
    fn idle_bars_make_one_marker_with_the_gap() {
        let mut log = ActionLog::new();
        log.record(0.0, 120.0, Some(2.0), e("x", "a", None, false));
        log.record(20.0, 120.0, Some(42.0), e("y", "b", None, false));
        let bars: Vec<&str> = log
            .lines()
            .iter()
            .filter(|l| l.kind == LineKind::Bar)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(bars, ["bar 1", "bar 11 (+10)"]);
        // no master beat: no markers
        let mut log = ActionLog::new();
        log.record(0.0, 120.0, None, e("x", "a", None, false));
        assert_eq!(log.lines().len(), 1);
    }

    #[test]
    fn another_control_starts_a_new_run_and_faders_fold() {
        let mut log = ActionLog::new();
        log.record(0.0, 120.0, None, e("browser:A", "s1", Some(1), false));
        log.record(0.1, 120.0, None, e("vol:A", "Volume A 0.50", None, true));
        log.record(0.2, 120.0, None, e("vol:A", "Volume A 0.60", None, true));
        log.record(0.3, 120.0, None, e("vol:A", "Volume A 0.70", None, true));
        log.record(0.4, 120.0, None, e("browser:A", "s2", Some(-1), false));
        let l = log.lines();
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].text, "Volume A 0.70");
        assert_eq!(l[1].moves, 3);
        assert_eq!(l[2].pattern, "back 1");
        assert_eq!(log.since(l[1].seq).len(), 1);
    }

    #[test]
    fn the_beat_clock_follows_the_master_and_carries_on_through_a_reload() {
        let mut c = BeatClock::new();
        // the master at beat 10 when the log starts: the timeline starts in its bar phase
        assert_eq!(c.advance(0.0, 120.0, Some(10.0)), 2.0);
        assert_eq!(c.advance(0.5, 120.0, Some(11.0)), 3.0);
        // the master deck reloads (its beat back to 0) a beat later: carried on, in phase
        let b = c.advance(1.0, 120.0, Some(0.0));
        assert_eq!(b, 4.0);
        assert_eq!(c.advance(1.5, 120.0, Some(1.0)), 5.0);
        // no master: by the tempo
        assert_eq!(c.advance(2.0, 120.0, None), 6.0);
    }

    #[test]
    fn the_log_keeps_its_cap() {
        let mut log = ActionLog::new();
        for k in 0..(LOG_CAP + 20) {
            log.record(k as f64, 120.0, None, e(&format!("k{k}"), "x", None, false));
        }
        assert_eq!(log.lines().len(), LOG_CAP);
        assert_eq!(log.last_seq(), (LOG_CAP + 20) as u64);
    }
}
