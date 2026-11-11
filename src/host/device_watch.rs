//! Audio devices that come and go: a Bluetooth headset reconnecting, a USB interface pulled,
//! the system default moving. Beyond Swift (where `AVAudioEngine` posts
//! `AVAudioEngineConfigurationChange` and `DJEngine` restarts itself).
//!
//! A stream that loses its device stops calling back: the renderer then sits in its slot with
//! nobody pulling it, the command ring fills (`Engine::dropped` climbs), the meters read 0 and
//! every mirror the window draws from stops moving, while the rest of the app (the socket, the
//! MIDI clock) runs on as if nothing happened. This module is how a host notices and what it
//! does about it, with no device in it so the tests drive it by hand:
//!
//! - [`StreamHealth`]: what a stream's callbacks leave behind: a heartbeat the data callback
//!   bumps, and the fault the error callback reports (`crate::host::audio` fills both).
//! - [`Watchdog`]: reads a [`StreamHealth`] on the host's own clock: alive, faulted (the
//!   backend said so: device gone, stream invalidated), or stalled (no callback for
//!   [`STALL_AFTER`] s, which catches a backend that dies without saying).
//! - [`Recovery`]: one end (the output, or an input strip) and the device it wants: lost at a
//!   time, retried with back-off while the device is listed, restored. The host opens and
//!   drops the streams; this decides when.
//! - [`pick_fallback`]: where the master goes while its device is away (the system default,
//!   else the built-in speakers, else nowhere: the host keeps rendering on a thread so the
//!   decks still move and the command ring keeps draining).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Seconds with no callback before a running stream counts as stalled.
pub const STALL_AFTER: f64 = 1.0;
/// Seconds a new stream gets to deliver its first callback (Bluetooth can be slow to start).
pub const START_GRACE: f64 = 3.0;
/// The first retry after a loss, seconds; each failed retry doubles it up to [`RETRY_MAX`].
pub const RETRY_FIRST: f64 = 1.0;
pub const RETRY_MAX: f64 = 8.0;

/// What a stream's callbacks report. Shared between the stream (any thread) and the host.
#[derive(Debug, Default)]
pub struct StreamHealth {
    beats: AtomicU64,
    faulted: AtomicBool,
    fault: Mutex<Option<String>>,
    /// Errors the backend reported that the stream survives (an xrun, a reroute).
    notices: AtomicU64,
    /// Output callbacks that played silence because the renderer was busy or away (a device
    /// switch holding the slot): what the listener hears as a dropout.
    silent: AtomicU64,
    /// Callbacks that came more than twice their buffer's length after the one before: the
    /// device starved (an underrun the backend may not report).
    late: AtomicU64,
    /// Frames in the last callback's buffer.
    frames: AtomicU64,
    /// The longest gap between two callbacks, microseconds (reset by [`StreamHealth::take_max_gap`]).
    max_gap_us: AtomicU64,
}

impl StreamHealth {
    pub fn new() -> StreamHealth {
        StreamHealth::default()
    }

    /// The data callback ran (real-time safe: one relaxed add).
    pub fn beat(&self) {
        self.beats.fetch_add(1, Ordering::Relaxed);
    }

    pub fn beats(&self) -> u64 {
        self.beats.load(Ordering::Relaxed)
    }

    /// The backend says the stream is done (device gone, stream invalidated). The first
    /// reason is kept.
    pub fn fault(&self, why: &str) {
        if let Ok(mut g) = self.fault.try_lock() {
            if g.is_none() {
                *g = Some(why.to_string());
            }
        }
        self.faulted.store(true, Ordering::Release);
    }

    /// An error the stream lives through.
    pub fn notice(&self) {
        self.notices.fetch_add(1, Ordering::Relaxed);
    }

    pub fn notices(&self) -> u64 {
        self.notices.load(Ordering::Relaxed)
    }

    /// A data callback of `frames` frames came `gap_s` seconds after the one before (None: the
    /// first), at `sample_rate`. Real-time safe: relaxed atomics only.
    pub fn callback(&self, frames: usize, gap_s: Option<f64>, sample_rate: f64) {
        self.frames.store(frames as u64, Ordering::Relaxed);
        if let Some(g) = gap_s {
            let us = (g * 1e6) as u64;
            self.max_gap_us.fetch_max(us, Ordering::Relaxed);
            let expected = frames as f64 / sample_rate.max(1.0);
            if expected > 0.0 && g > 2.0 * expected + 0.002 {
                self.late.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A callback played silence (the renderer was not there to pull).
    pub fn silent_fill(&self) {
        self.silent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn silent_fills(&self) -> u64 {
        self.silent.load(Ordering::Relaxed)
    }

    pub fn late_callbacks(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }

    /// Frames in the last callback's buffer (0: none yet).
    pub fn buffer_frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// The longest gap between two callbacks since the last call, seconds.
    pub fn take_max_gap(&self) -> f64 {
        self.max_gap_us.swap(0, Ordering::Relaxed) as f64 / 1e6
    }

    /// Everything that sounds like a dropout: backend xruns, silent fills, late callbacks.
    pub fn underruns(&self) -> u64 {
        self.notices() + self.silent_fills() + self.late_callbacks()
    }

    /// Why the stream faulted, if it did.
    pub fn faulted(&self) -> Option<String> {
        if !self.faulted.load(Ordering::Acquire) {
            return None;
        }
        Some(
            self.fault
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "stream error".to_string()),
        )
    }
}

/// How the backend's error should be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The stream is gone: rebuild it.
    Fatal,
    /// It goes on (an xrun; the default device rerouted under a default-following stream).
    Notice,
}

/// A stream as the host sees it now.
#[derive(Clone, Debug, PartialEq)]
pub enum Liveness {
    Alive,
    /// Started, no callback yet, still inside [`START_GRACE`].
    Starting,
    Faulted(String),
    /// No callback for this many seconds.
    Stalled(f64),
}

impl Liveness {
    pub fn is_lost(&self) -> bool {
        matches!(self, Liveness::Faulted(_) | Liveness::Stalled(_))
    }

    /// In words, for the log ("device disconnected", "no callback for 1.2 s").
    pub fn why(&self) -> String {
        match self {
            Liveness::Faulted(w) => w.clone(),
            Liveness::Stalled(s) => format!("no audio callback for {s:.1} s"),
            Liveness::Alive => "alive".into(),
            Liveness::Starting => "starting".into(),
        }
    }
}

/// Reads one stream's [`StreamHealth`] against the host's clock (seconds, any origin).
#[derive(Clone, Debug)]
pub struct Watchdog {
    started: f64,
    seen: u64,
    /// When the heartbeat last moved (None: not yet).
    moved: Option<f64>,
}

impl Watchdog {
    /// A stream started at `now`.
    pub fn new(now: f64) -> Watchdog {
        Watchdog {
            started: now,
            seen: 0,
            moved: None,
        }
    }

    pub fn check(&mut self, health: &StreamHealth, now: f64) -> Liveness {
        if let Some(why) = health.faulted() {
            return Liveness::Faulted(why);
        }
        let beats = health.beats();
        if beats != self.seen {
            self.seen = beats;
            self.moved = Some(now);
        }
        match self.moved {
            Some(t) if now - t > STALL_AFTER => Liveness::Stalled(now - t),
            Some(_) => Liveness::Alive,
            None if now - self.started > START_GRACE => Liveness::Stalled(now - self.started),
            None => Liveness::Starting,
        }
    }
}

/// Does the device list hold one named like `want` (case-insensitive part of a name, as
/// `Output::open` and `Input::open` match)? Returns the full name.
pub fn listed<'a>(want: &str, names: &'a [String]) -> Option<&'a str> {
    let w = want.to_lowercase();
    names
        .iter()
        .find(|n| n.to_lowercase().contains(&w))
        .map(String::as_str)
}

/// Where the master goes while `lost` is away: the system default if it is something else,
/// else a built-in speaker ("MacBook Pro Speakers", "Built-in Output"), else None (no device:
/// the host renders on a thread).
pub fn pick_fallback(lost: &str, default: Option<&str>, available: &[String]) -> Option<String> {
    let lost_l = lost.to_lowercase();
    let other = |n: &str| n.to_lowercase() != lost_l;
    if let Some(d) = default.filter(|d| other(d) && available.iter().any(|a| a == d)) {
        return Some(d.to_string());
    }
    let builtin = |n: &str| {
        let l = n.to_lowercase();
        l.contains("speakers") && (l.contains("macbook") || l.contains("built-in"))
            || l.contains("built-in output")
    };
    available
        .iter()
        .find(|n| other(n) && builtin(n))
        .cloned()
}

/// One end's state: the device it wants, and whether it has it.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Playing (or capturing) on the device it wants.
    Live,
    /// The device went away at `since`; the next try is due at `next_try`.
    Lost {
        since: f64,
        why: String,
        next_try: f64,
        tries: u32,
    },
}

/// The recovery of one end (the output, or an input strip).
#[derive(Clone, Debug)]
pub struct Recovery {
    /// The device it wants back (a full name, or the part of one the user gave).
    pub wanted: String,
    pub phase: Phase,
    /// Losses so far, and recoveries.
    pub losses: u32,
    pub restores: u32,
}

impl Recovery {
    pub fn new(wanted: &str) -> Recovery {
        Recovery {
            wanted: wanted.to_string(),
            phase: Phase::Live,
            losses: 0,
            restores: 0,
        }
    }

    pub fn is_lost(&self) -> bool {
        matches!(self.phase, Phase::Lost { .. })
    }

    /// The device went away at `now` (a second report while lost is ignored).
    pub fn lose(&mut self, now: f64, why: &str) {
        if self.is_lost() {
            return;
        }
        self.losses += 1;
        self.phase = Phase::Lost {
            since: now,
            why: why.to_string(),
            next_try: now + RETRY_FIRST,
            tries: 0,
        };
    }

    /// Should the host try to reopen the wanted device now? Only when a try is due and the
    /// platform lists it again; returns its full name then.
    pub fn due<'a>(&self, now: f64, available: &'a [String]) -> Option<&'a str> {
        match &self.phase {
            Phase::Lost { next_try, .. } if now >= *next_try => listed(&self.wanted, available),
            _ => None,
        }
    }

    /// Is a try due (before asking the platform for its device list, which is not free)?
    pub fn retry_due(&self, now: f64) -> bool {
        matches!(&self.phase, Phase::Lost { next_try, .. } if now >= *next_try)
    }

    /// A try was due but the device is not listed yet: look again in [`RETRY_FIRST`] s (no
    /// back-off: a device that is simply away is noticed as soon as it returns).
    pub fn not_listed(&mut self, now: f64) {
        if let Phase::Lost { next_try, .. } = &mut self.phase {
            *next_try = now + RETRY_FIRST;
        }
    }

    /// The reopen failed (listed but not ready yet, or at another sample rate): back off.
    pub fn retry_failed(&mut self, now: f64) {
        if let Phase::Lost {
            next_try, tries, ..
        } = &mut self.phase
        {
            *tries += 1;
            let wait = (RETRY_FIRST * f64::from(1u32 << (*tries).min(4))).min(RETRY_MAX);
            *next_try = now + wait;
        }
    }

    /// It is back. Returns how long it was gone.
    pub fn restored(&mut self, now: f64) -> f64 {
        let gone = match &self.phase {
            Phase::Lost { since, .. } => now - since,
            Phase::Live => 0.0,
        };
        if self.is_lost() {
            self.restores += 1;
        }
        self.phase = Phase::Live;
        gone
    }

    /// Seconds lost so far (None: live).
    pub fn lost_for(&self, now: f64) -> Option<f64> {
        match &self.phase {
            Phase::Lost { since, .. } => Some(now - since),
            Phase::Live => None,
        }
    }

    /// "live" or "lost".
    pub fn state(&self) -> &'static str {
        match self.phase {
            Phase::Live => "live",
            Phase::Lost { .. } => "lost",
        }
    }
}

/// A bounded log of what happened to the audio ends, for the status and the window.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    pub lines: Vec<(f64, String)>,
}

/// Lines kept.
pub const EVENT_CAP: usize = 64;

impl EventLog {
    pub fn push(&mut self, t: f64, line: String) {
        self.lines.push((t, line));
        let over = self.lines.len().saturating_sub(EVENT_CAP);
        self.lines.drain(..over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_beating_stream_is_alive_and_one_that_stops_is_stalled() {
        let h = StreamHealth::new();
        let mut w = Watchdog::new(0.0);
        assert_eq!(w.check(&h, 0.5), Liveness::Starting);
        h.beat();
        assert_eq!(w.check(&h, 0.6), Liveness::Alive);
        h.beat();
        assert_eq!(w.check(&h, 1.5), Liveness::Alive);
        // the device went away without a word: the callbacks just stop
        assert_eq!(w.check(&h, 2.4), Liveness::Alive);
        let l = w.check(&h, 2.6);
        assert!(matches!(l, Liveness::Stalled(s) if (s - 1.1).abs() < 1e-9), "{l:?}");
        assert!(l.is_lost());
        // and comes back to life if it starts again (a host would already have moved on)
        h.beat();
        assert_eq!(w.check(&h, 2.7), Liveness::Alive);
    }

    #[test]
    fn a_stream_that_never_starts_is_stalled_after_the_grace() {
        let h = StreamHealth::new();
        let mut w = Watchdog::new(10.0);
        assert_eq!(w.check(&h, 12.9), Liveness::Starting);
        assert!(w.check(&h, 13.1).is_lost());
    }

    #[test]
    fn a_fault_wins_and_keeps_the_first_reason() {
        let h = StreamHealth::new();
        let mut w = Watchdog::new(0.0);
        h.beat();
        h.fault("Device disconnected");
        h.fault("Device sample rate changed");
        h.beat();
        assert_eq!(
            w.check(&h, 0.1),
            Liveness::Faulted("Device disconnected".into())
        );
        h.notice();
        assert_eq!(h.notices(), 1);
    }

    #[test]
    fn the_fallback_is_the_default_else_the_built_in_speakers_else_nothing() {
        let all = names(&["Tune Train", "MacBook Pro Speakers", "BlackHole 2ch"]);
        // the system moved its default off the headphones: follow it
        assert_eq!(
            pick_fallback("Tune Train", Some("BlackHole 2ch"), &all).as_deref(),
            Some("BlackHole 2ch")
        );
        // the default is still the lost device (CoreAudio has not moved it yet): the speakers
        assert_eq!(
            pick_fallback("Tune Train", Some("Tune Train"), &all).as_deref(),
            Some("MacBook Pro Speakers")
        );
        assert_eq!(
            pick_fallback("Tune Train", None, &names(&["MacBook Pro Speakers"])).as_deref(),
            Some("MacBook Pro Speakers")
        );
        // the speakers themselves went (an odd day): another default, else nothing
        assert_eq!(
            pick_fallback("MacBook Pro Speakers", Some("MacBook Pro Speakers"), &all),
            None
        );
        assert_eq!(pick_fallback("Tune Train", None, &names(&["BlackHole 2ch"])), None);
        // a default that is not listed (gone between the two queries) is not used
        assert_eq!(
            pick_fallback("Tune Train", Some("AirPods"), &names(&["Tune Train"])),
            None
        );
    }

    #[test]
    fn lost_then_retried_with_back_off_until_it_is_listed_and_opens() {
        let mut r = Recovery::new("Tune Train");
        assert!(!r.is_lost());
        r.lose(100.0, "Device disconnected");
        r.lose(100.5, "no audio callback for 1.0 s"); // the watchdog's second word: ignored
        assert_eq!(r.losses, 1);
        assert_eq!(r.lost_for(101.0), Some(1.0));
        let gone = names(&["MacBook Pro Speakers"]);
        let back = names(&["MacBook Pro Speakers", "Tune Train"]);
        assert_eq!(r.due(100.5, &back), None, "not before the first retry");
        assert!(!r.retry_due(100.5));
        assert!(r.retry_due(101.0));
        assert_eq!(r.due(101.0, &gone), None, "not while it is not listed");
        r.not_listed(101.0);
        assert!(!r.retry_due(101.5), "looked at the list: again in a second");
        assert_eq!(r.due(102.0, &back), Some("Tune Train"));
        let mut r = {
            let mut q = Recovery::new("Tune Train");
            q.lose(100.0, "Device disconnected");
            q
        };
        assert_eq!(r.due(101.0, &back), Some("Tune Train"));
        // listed but not ready (Bluetooth still negotiating): 2 s, 4 s, 8 s, 8 s
        let mut t = 101.0;
        for wait in [2.0, 4.0, 8.0, 8.0, 8.0] {
            r.retry_failed(t);
            assert_eq!(r.due(t + wait - 0.01, &back), None);
            assert_eq!(r.due(t + wait, &back), Some("Tune Train"));
            t += wait;
        }
        let gone_for = r.restored(t);
        assert!((gone_for - (t - 100.0)).abs() < 1e-9);
        assert_eq!((r.losses, r.restores, r.state()), (1, 1, "live"));
        assert_eq!(r.due(t + 100.0, &back), None, "a live end is never due");
    }

    #[test]
    fn a_wanted_part_of_a_name_matches_like_open_does() {
        let r = {
            let mut r = Recovery::new("sound blaster");
            r.lose(0.0, "x");
            r
        };
        let l = names(&["Sound Blaster Play! 3", "MacBook Pro Microphone"]);
        assert_eq!(r.due(5.0, &l), Some("Sound Blaster Play! 3"));
    }

    #[test]
    fn the_event_log_is_bounded() {
        let mut l = EventLog::default();
        for i in 0..(EVENT_CAP + 10) {
            l.push(i as f64, format!("{i}"));
        }
        assert_eq!(l.lines.len(), EVENT_CAP);
        assert_eq!(l.lines[0].1, "10");
    }
}
