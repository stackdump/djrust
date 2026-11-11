//! TD-3 performance tricks over MIDI (beyond Swift; docs/TRICKS.md, "TD-3"): accent, slide and
//! a cutoff ride, on the notes the hub sends the TD-3 (the MPK's keys, routed loops).
//!
//! What the stock TD-3 takes over MIDI, as far as the hub knows it (docs/HUB.md M7): note on /
//! off on its channel, **accent from velocity** (a hard hit), **slide from legato** (the next
//! note on before the last one's off), clock and Start / Stop. It has **no CC for its cutoff**
//! (or resonance, env mod, decay, accent level): those are knobs only. So the cutoff ride is
//! for gear that has one: [`GearCaps::cutoff_cc`] is None for the TD-3 and is set by hand for a
//! clone or a modded unit that listens to a CC.

use alloc::vec::Vec;

/// The velocity an accented note goes out at.
pub const ACCENT_VELOCITY: u8 = 127;
/// The most an unaccented note goes out at while accents are being played (so the TD-3's
/// accent only lands where it is meant to).
pub const PLAIN_VELOCITY_MAX: u8 = 96;
/// A slide holds a note's off this long at most, waiting for the next note, seconds.
pub const SLIDE_HOLD_S: f64 = 0.5;
/// How far a slid note overlaps the next, seconds.
pub const SLIDE_OVERLAP_S: f64 = 0.004;

/// What a piece of gear takes beyond notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GearCaps {
    /// The CC its filter cutoff listens to, if any.
    pub cutoff_cc: Option<u8>,
}

impl GearCaps {
    /// The hub's gear by id: the TD-3 has no cutoff CC.
    pub fn of(gear: &str) -> GearCaps {
        // the TD-3 (the only bass synth the hub knows) has none; a clone or a modded unit
        // that has one is set by hand
        let _ = gear;
        GearCaps { cutoff_cc: None }
    }
}

/// Accent and slide on a live stream of note messages (the MPK's keys into the TD-3).
#[derive(Clone, Debug, Default)]
pub struct Td3Shaper {
    pub accent: bool,
    pub slide: bool,
    /// Accent every note while held (true), or only notes struck harder than
    /// [`PLAIN_VELOCITY_MAX`] and squash the rest under it (false: the "accent the hits" mode).
    pub accent_all: bool,
    /// A note's off held back for a slide: (the message, when it was held, host seconds).
    held: Option<([u8; 3], f64)>,
}

impl Td3Shaper {
    pub fn new() -> Td3Shaper {
        Td3Shaper {
            accent_all: true,
            ..Default::default()
        }
    }

    /// One message from the keys → what goes to the TD-3, at host time `now` (seconds).
    pub fn shape(&mut self, m: [u8; 3], now: f64) -> Vec<[u8; 3]> {
        let kind = m[0] & 0xF0;
        let on = kind == 0x90 && m[2] > 0;
        let off = kind == 0x80 || (kind == 0x90 && m[2] == 0);
        let mut out = Vec::new();
        if on {
            let mut m = m;
            if self.accent {
                m[2] = if self.accent_all || m[2] > PLAIN_VELOCITY_MAX {
                    ACCENT_VELOCITY
                } else {
                    m[2].min(PLAIN_VELOCITY_MAX)
                };
            }
            out.push(m);
            // the next note is on: now the held one may go (legato: the TD-3 slides)
            if let Some((h, _)) = self.held.take() {
                if h[1] != m[1] {
                    out.push(h);
                }
            }
            return out;
        }
        if off && self.slide {
            // hold it for the next note; one already held goes now
            if let Some((h, _)) = self.held.replace((m, now)) {
                out.push(h);
            }
            return out;
        }
        out.push(m);
        out
    }

    /// A held off whose wait is over (no next note came), or every held off when the slide is
    /// let go. Call every pass.
    pub fn tick(&mut self, now: f64) -> Option<[u8; 3]> {
        match self.held {
            Some((h, at)) if !self.slide || now - at >= SLIDE_HOLD_S => {
                self.held = None;
                Some(h)
            }
            _ => None,
        }
    }
}

/// Slide scheduled notes (a routed loop's, `(on, off, note)` in seconds, in order): each note's
/// off is moved past the next note's on when the gap to it is short, so they overlap and the
/// TD-3 slides. Notes further apart than [`SLIDE_HOLD_S`] are left alone.
pub fn slide_schedule<T>(notes: &mut [(f64, f64, T)]) {
    for k in 0..notes.len().saturating_sub(1) {
        let next_on = notes[k + 1].0;
        let (on, off) = (notes[k].0, notes[k].1);
        if next_on > on && next_on - off < SLIDE_HOLD_S {
            notes[k].1 = off.max(next_on + SLIDE_OVERLAP_S);
        }
    }
}

/// A CC ridden from one value to another over a time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CcRide {
    pub cc: u8,
    pub from: f64,
    pub to: f64,
    /// When it starts and how long it takes, in render frames.
    pub start: i64,
    pub frames: i64,
}

impl CcRide {
    /// Where it is at frame `now`, 0..127.
    pub fn value_at(&self, now: i64) -> u8 {
        let t = if self.frames <= 0 {
            1.0
        } else {
            ((now - self.start) as f64 / self.frames as f64).clamp(0.0, 1.0)
        };
        let v = self.from + (self.to - self.from) * t;
        libm::round(v.clamp(0.0, 1.0) * 127.0) as u8
    }

    pub fn done(&self, now: i64) -> bool {
        now >= self.start + self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_raise_the_velocity() {
        let mut s = Td3Shaper::new();
        assert_eq!(s.shape([0x90, 48, 70], 0.0), alloc::vec![[0x90, 48, 70]]);
        s.accent = true;
        assert_eq!(s.shape([0x90, 48, 70], 0.0), alloc::vec![[0x90, 48, 127]]);
        s.accent_all = false;
        assert_eq!(s.shape([0x90, 48, 120], 0.0), alloc::vec![[0x90, 48, 127]]);
        assert_eq!(s.shape([0x90, 48, 110], 0.0)[0][2], 127);
        assert_eq!(s.shape([0x90, 48, 90], 0.0)[0][2], 90);
        // offs pass as they are
        assert_eq!(s.shape([0x80, 48, 0], 0.0), alloc::vec![[0x80, 48, 0]]);
    }

    #[test]
    fn a_slide_holds_the_off_past_the_next_on() {
        let mut s = Td3Shaper::new();
        s.slide = true;
        assert_eq!(s.shape([0x90, 48, 100], 0.0).len(), 1);
        // the key comes up: nothing yet
        assert!(s.shape([0x80, 48, 0], 0.1).is_empty());
        // the next key: its on, then the held off (legato)
        assert_eq!(
            s.shape([0x90, 50, 100], 0.2),
            alloc::vec![[0x90, 50, 100], [0x80, 48, 0]]
        );
        // a held off goes after the wait, or as soon as the slide is let go
        assert!(s.shape([0x80, 50, 0], 0.3).is_empty());
        assert_eq!(s.tick(0.4), None);
        assert_eq!(s.tick(0.3 + SLIDE_HOLD_S), Some([0x80, 50, 0]));
        s.shape([0x90, 52, 100], 1.0);
        s.shape([0x80, 52, 0], 1.1);
        s.slide = false;
        assert_eq!(s.tick(1.1), Some([0x80, 52, 0]));
    }

    #[test]
    fn scheduled_notes_overlap_into_the_next() {
        let mut n = alloc::vec![(0.0, 0.1, 'a'), (0.125, 0.225, 'b'), (2.0, 2.1, 'c')];
        slide_schedule(&mut n);
        assert!((n[0].1 - (0.125 + SLIDE_OVERLAP_S)).abs() < 1e-12);
        // 'b' to 'c' is far: no slide
        assert_eq!(n[1].1, 0.225);
        assert_eq!(n[2].1, 2.1);
    }

    #[test]
    fn a_cc_ride_runs_from_one_value_to_the_other() {
        let r = CcRide {
            cc: 74,
            from: 0.0,
            to: 1.0,
            start: 1_000,
            frames: 1_000,
        };
        assert_eq!(r.value_at(0), 0);
        assert_eq!(r.value_at(1_500), 64);
        assert_eq!(r.value_at(9_999), 127);
        assert!(r.done(2_000) && !r.done(1_999));
        assert_eq!(GearCaps::of("td3").cutoff_cc, None);
    }
}
