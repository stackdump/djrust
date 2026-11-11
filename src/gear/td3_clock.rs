//! The TD-3's clock source, switched with the hub's clock (docs/HUB.md, M2; docs/TD3-CLOCK.md).
//!
//! The TD-3 follows USB MIDI clock and Start / Stop only while its clock source is "MIDI USB".
//! When the hub becomes the clock master we set it to USB; when the hub stops being master (the
//! clock is off, the TD-3 unplugged from the clock's outputs, or the app exits) we put it back
//! to internal. [`Td3ClockSource`] is that decision as a state machine with no I/O: the host feeds
//! it the facts (am I master, what did the TD-3 answer) and sends the bytes it returns.
//!
//! Only these messages are ever built here: set clock source (`1B 02` / `1B 00`) and get config
//! (`75`). Pattern writes (`78`) are not in this file and the host must not add them to it.
//!
//! **Read-back.** `75` answers with a `76 ...` config dump. Which byte is the clock source is not
//! documented for every firmware, so it is learned: the dump taken before the first change is the
//! baseline, the dump after "USB" must differ from it in exactly one byte (that offset is the
//! clock source), and after "internal" that byte must be back to its baseline value.

use alloc::string::String;
use alloc::vec::Vec;

/// The SysEx prefix of every TD-3 message: `F0 00 20 32 00 01 0A`.
const HEAD: [u8; 7] = [0xF0, 0x00, 0x20, 0x32, 0x00, 0x01, 0x0A];

/// Clock source = MIDI USB.
pub const SET_USB: [u8; 10] = [0xF0, 0x00, 0x20, 0x32, 0x00, 0x01, 0x0A, 0x1B, 0x02, 0xF7];
/// Clock source = internal.
pub const SET_INTERNAL: [u8; 10] = [0xF0, 0x00, 0x20, 0x32, 0x00, 0x01, 0x0A, 0x1B, 0x00, 0xF7];
/// Get config (the answer starts `76`).
pub const GET_CONFIG: [u8; 9] = [0xF0, 0x00, 0x20, 0x32, 0x00, 0x01, 0x0A, 0x75, 0xF7];

/// The command byte of a TD-3 SysEx message, if `m` is one.
pub fn command(m: &[u8]) -> Option<u8> {
    (m.len() > 8 && m[..7] == HEAD && *m.last()? == 0xF7).then(|| m[7])
}

/// Is `m` a config dump (the answer to [`GET_CONFIG`])?
pub fn is_config_reply(m: &[u8]) -> bool {
    command(m) == Some(0x76)
}

/// Is this one of the few messages this module may send?
pub fn is_allowed(m: &[u8]) -> bool {
    m == SET_USB || m == SET_INTERNAL || m == GET_CONFIG
}

/// Where the TD-3's clock comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Internal,
    Usb,
}

/// What the host should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Send these bytes to the TD-3's port.
    Send(Vec<u8>),
    /// Tell the user (the TD-3 window's log).
    Log(String),
}

/// How sure we are of what the TD-3 is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Nothing sent yet.
    None,
    /// Sent; no config dump has come back to prove it.
    Unconfirmed,
    /// The config read-back shows the change.
    Confirmed,
    /// The read-back does not show it.
    Mismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Await {
    Nothing,
    /// A dump before the first change: the baseline.
    Baseline,
    /// A dump after setting this source.
    After(Source),
}

/// The switch.
#[derive(Clone, Debug)]
pub struct Td3ClockSource {
    /// Switch on its own with the hub's clock. Off: only [`Self::request`] changes anything.
    pub auto: bool,
    /// What we last sent (None: we have not touched it).
    set: Option<Source>,
    baseline: Option<Vec<u8>>,
    /// Index of the clock-source byte, once seen.
    offset: Option<usize>,
    awaiting: Await,
    pending_after: Option<Source>,
    confirm: Confirm,
    was_master: bool,
}

impl Default for Td3ClockSource {
    fn default() -> Self {
        Td3ClockSource::new()
    }
}

impl Td3ClockSource {
    pub fn new() -> Td3ClockSource {
        Td3ClockSource {
            auto: true,
            set: None,
            baseline: None,
            offset: None,
            awaiting: Await::Nothing,
            pending_after: None,
            confirm: Confirm::None,
            was_master: false,
        }
    }

    /// What we last set the TD-3 to, if we touched it.
    pub fn current(&self) -> Option<Source> {
        self.set
    }

    pub fn confirm(&self) -> Confirm {
        self.confirm
    }

    /// The learned config offset of the clock-source byte.
    pub fn offset(&self) -> Option<usize> {
        self.offset
    }

    /// The TD-3 is (or is not) a clock output of a running master. Call every frame: it acts on
    /// changes only. `connected` is false while the TD-3's port is missing.
    pub fn follow(&mut self, master: bool, connected: bool) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.auto || !connected {
            // a missing port cannot be told anything; try again when it returns
            if !connected {
                self.was_master = false;
                self.awaiting = Await::Nothing;
                self.pending_after = None;
                if self.set == Some(Source::Usb) {
                    self.set = None; // power-cycled or unplugged: it is back to its own setting
                    self.confirm = Confirm::None;
                }
            }
            return out;
        }
        if master && (!self.was_master || self.set != Some(Source::Usb)) && self.awaiting == Await::Nothing {
            out.extend(self.request(Source::Usb));
        } else if !master && self.was_master && self.set == Some(Source::Usb) {
            out.extend(self.request(Source::Internal));
        }
        self.was_master = master;
        out
    }

    /// Set the source now (a registry action, or [`Self::follow`]). The first change reads the
    /// config first, so there is a baseline to confirm against.
    pub fn request(&mut self, to: Source) -> Vec<Out> {
        let mut out = Vec::new();
        if self.baseline.is_none() && self.awaiting == Await::Nothing && self.set.is_none() {
            // the baseline dump, then the change; the dump's answer is not waited for (it is
            // taken by the time the set has been processed, in order)
            out.push(Out::Send(GET_CONFIG.to_vec()));
            self.awaiting = Await::Baseline;
        }
        out.push(Out::Send(
            match to {
                Source::Usb => SET_USB.to_vec(),
                Source::Internal => SET_INTERNAL.to_vec(),
            },
        ));
        out.push(Out::Send(GET_CONFIG.to_vec()));
        self.set = Some(to);
        self.confirm = Confirm::Unconfirmed;
        // the dumps arrive in the order asked: baseline (if any) first, then the after-dump
        self.queue_after(to);
        out.push(Out::Log(match to {
            Source::Usb => "clock source -> MIDI USB (the TD-3 follows the hub)".into(),
            Source::Internal => "clock source -> internal".into(),
        }));
        out
    }

    fn queue_after(&mut self, to: Source) {
        if self.awaiting != Await::Baseline {
            self.awaiting = Await::After(to);
        } else {
            self.pending_after = Some(to);
        }
    }

    /// The app is closing: put the TD-3 back to internal if we changed it.
    pub fn on_exit(&mut self) -> Vec<Out> {
        if self.set == Some(Source::Usb) {
            self.request(Source::Internal)
        } else {
            Vec::new()
        }
    }

    /// A SysEx the TD-3 sent. Returns log lines.
    pub fn on_message(&mut self, m: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        if !is_config_reply(m) {
            return out;
        }
        match self.awaiting {
            Await::Baseline => {
                self.baseline = Some(m.to_vec());
                self.awaiting = match self.pending_after.take() {
                    Some(s) => Await::After(s),
                    None => Await::Nothing,
                };
            }
            Await::After(s) => {
                self.awaiting = Await::Nothing;
                out.push(Out::Log(self.judge(s, m)));
            }
            Await::Nothing => {}
        }
        out
    }

    fn judge(&mut self, s: Source, reply: &[u8]) -> String {
        let Some(base) = self.baseline.clone() else {
            self.confirm = Confirm::Unconfirmed;
            return "config read-back: no baseline to compare with".into();
        };
        if base.len() != reply.len() {
            self.confirm = Confirm::Mismatch;
            return "config read-back: the dump changed length".into();
        }
        let diff: Vec<usize> = (0..base.len()).filter(|i| base[*i] != reply[*i]).collect();
        match (s, self.offset) {
            (Source::Usb, None) => match diff.as_slice() {
                [i] => {
                    self.offset = Some(*i);
                    self.confirm = Confirm::Confirmed;
                    alloc::format!("config read-back: byte {i} {:#04x} -> {:#04x}: clock source confirmed", base[*i], reply[*i])
                }
                [] => {
                    self.confirm = Confirm::Mismatch;
                    "config read-back: unchanged (already USB, or the change did not take)".into()
                }
                d => {
                    self.confirm = Confirm::Mismatch;
                    alloc::format!("config read-back: {} bytes differ, not a clock-source change", d.len())
                }
            },
            (Source::Usb, Some(o)) => {
                self.confirm = if reply[o] != base[o] { Confirm::Confirmed } else { Confirm::Mismatch };
                alloc::format!("config read-back: byte {o} = {:#04x}", reply[o])
            }
            (Source::Internal, Some(o)) => {
                let ok = reply[o] == base[o] && diff.is_empty();
                self.confirm = if ok { Confirm::Confirmed } else { Confirm::Mismatch };
                if ok {
                    "config read-back: back to the original: internal confirmed".into()
                } else {
                    alloc::format!("config read-back: byte {o} = {:#04x}, expected {:#04x}", reply[o], base[o])
                }
            }
            (Source::Internal, None) => {
                // never saw USB confirmed: internal is right if nothing differs from the baseline
                self.confirm = if diff.is_empty() { Confirm::Confirmed } else { Confirm::Mismatch };
                "config read-back after internal".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn dump(src: u8) -> Vec<u8> {
        let mut v = HEAD.to_vec();
        v.extend([0x76, 0, 1, 0, 0, src, 0, 0, 0xF7]);
        v
    }
    fn sends(o: &[Out]) -> Vec<Vec<u8>> {
        o.iter().filter_map(|x| if let Out::Send(b) = x { Some(b.clone()) } else { None }).collect()
    }

    #[test]
    fn exact_bytes() {
        assert_eq!(SET_USB, [0xF0, 0, 0x20, 0x32, 0, 1, 0x0A, 0x1B, 2, 0xF7]);
        assert_eq!(SET_INTERNAL, [0xF0, 0, 0x20, 0x32, 0, 1, 0x0A, 0x1B, 0, 0xF7]);
        assert!(is_allowed(&SET_USB) && is_allowed(&SET_INTERNAL) && is_allowed(&GET_CONFIG));
    }

    #[test]
    fn never_a_pattern_write() {
        let mut s = Td3ClockSource::new();
        let mut all = s.follow(true, true);
        all.extend(s.follow(false, true));
        all.extend(s.on_exit());
        for b in sends(&all) {
            assert!(is_allowed(&b), "{b:02x?}");
            assert_ne!(command(&b), Some(0x78));
        }
    }

    #[test]
    fn master_then_stop_then_confirm() {
        let mut s = Td3ClockSource::new();
        assert!(s.follow(false, true).is_empty());
        let o = s.follow(true, true);
        assert_eq!(sends(&o), vec![GET_CONFIG.to_vec(), SET_USB.to_vec(), GET_CONFIG.to_vec()]);
        assert_eq!(s.confirm(), Confirm::Unconfirmed);
        // calling again while master sends nothing
        assert!(s.follow(true, true).is_empty());
        s.on_message(&dump(0)); // baseline
        s.on_message(&dump(2)); // after USB
        assert_eq!(s.confirm(), Confirm::Confirmed);
        assert_eq!(s.offset(), Some(12));
        // stops being master: internal
        let o = s.follow(false, true);
        assert_eq!(sends(&o), vec![SET_INTERNAL.to_vec(), GET_CONFIG.to_vec()]);
        s.on_message(&dump(0));
        assert_eq!(s.confirm(), Confirm::Confirmed);
    }

    #[test]
    fn exit_restores_only_if_changed() {
        let mut s = Td3ClockSource::new();
        assert!(s.on_exit().is_empty());
        s.follow(true, true);
        assert!(sends(&s.on_exit()).contains(&SET_INTERNAL.to_vec()));
    }

    #[test]
    fn mismatch_is_reported() {
        let mut s = Td3ClockSource::new();
        s.follow(true, true);
        s.on_message(&dump(0));
        s.on_message(&dump(0));
        assert_eq!(s.confirm(), Confirm::Mismatch);
    }

    #[test]
    fn auto_off_does_nothing_until_asked() {
        let mut s = Td3ClockSource::new();
        s.auto = false;
        assert!(s.follow(true, true).is_empty());
        assert!(!s.request(Source::Usb).is_empty());
    }

    #[test]
    fn unplugged_forgets() {
        let mut s = Td3ClockSource::new();
        s.follow(true, true);
        assert!(s.follow(true, false).is_empty());
        assert_eq!(s.current(), None);
        // back: master still on -> set again
        assert!(!s.follow(true, true).is_empty());
    }
}
