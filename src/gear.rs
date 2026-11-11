//! The music gear the hub knows (docs/HUB.md): which MIDI port is which device, what changed
//! between two port scans (hot-plug), what a message says in words, and a per-device monitor of
//! recent traffic. Pure: hosts list ports and pass bytes in with their timestamps.

pub mod acid;
pub mod mpk;
pub mod td3;
pub mod midi_out;
pub mod pd705;
pub mod pd705_kit;
pub mod pd705_load;
pub mod pd705_pattern;
pub mod td3_clock;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// What a piece of gear is to the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GearKind {
    /// Sends controls the map turns into actions (has a profile in `data/profiles`).
    Controller,
    /// Makes sound from MIDI the hub sends it.
    Instrument,
}

/// One device the hub knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GearSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: GearKind,
    /// Lower-case substrings of the port name that identify it (CoreMIDI and ALSA names).
    pub patterns: &'static [&'static str],
    /// It follows MIDI clock and Start / Stop.
    pub follows_clock: bool,
    /// The channel it plays notes on (0-based), if the hub sends it notes.
    pub note_channel: Option<u8>,
}

/// Every device in docs/HUB.md.
pub const CATALOG: [GearSpec; 5] = [
    GearSpec {
        id: "inpulse",
        name: "Hercules DJControl Inpulse 200 MK3",
        kind: GearKind::Controller,
        patterns: &["inpulse"],
        follows_clock: false,
        note_channel: None,
    },
    GearSpec {
        id: "apc",
        name: "Akai APC mini mk2",
        kind: GearKind::Controller,
        patterns: &["apc mini mk2"],
        follows_clock: false,
        note_channel: None,
    },
    GearSpec {
        id: "mpk",
        name: "Akai MPK Mini Mk II",
        kind: GearKind::Controller,
        patterns: &["mpk mini"],
        follows_clock: false,
        note_channel: None,
    },
    GearSpec {
        id: "td3",
        name: "Behringer TD-3",
        kind: GearKind::Instrument,
        patterns: &["td-3"],
        // once its clock source is set to MIDI USB (td3-rs docs/PROTOCOL.md)
        follows_clock: true,
        note_channel: Some(0),
    },
    GearSpec {
        id: "pd705",
        name: "Lekato PD705",
        kind: GearKind::Instrument,
        // the pad's USB name is "DrumPad" (VID 35ba, PID 1101; pd705-rs FORMAT.md §8)
        patterns: &["drumpad", "pd705"],
        // it ignores clock and transport: the hub plays its notes on the clock instead
        follows_clock: false,
        note_channel: Some(9),
    },
];

pub fn spec(id: &str) -> Option<&'static GearSpec> {
    CATALOG.iter().find(|g| g.id == id)
}

/// Which device a port belongs to (None: unknown, or a virtual bus).
pub fn identify(port: &str) -> Option<&'static GearSpec> {
    let p = port.to_lowercase();
    CATALOG
        .iter()
        .find(|g| g.patterns.iter().any(|pat| p.contains(pat)))
}

/// Input or output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Direction {
    In,
    Out,
}

/// The ports one scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortScan {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl PortScan {
    fn has(&self, dir: Direction, port: &str) -> bool {
        match dir {
            Direction::In => self.inputs.iter().any(|p| p == port),
            Direction::Out => self.outputs.iter().any(|p| p == port),
        }
    }
}

/// A port that came or went between two scans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortChange {
    pub attached: bool,
    pub direction: Direction,
    pub port: String,
    /// The device it belongs to, if known.
    pub gear: Option<&'static str>,
}

/// What changed from `before` to `now`: detachments first, then attachments, each in the
/// scan's own order (inputs before outputs).
pub fn diff(before: &PortScan, now: &PortScan) -> Vec<PortChange> {
    let mut out = Vec::new();
    let change = |attached: bool, direction: Direction, port: &String| PortChange {
        attached,
        direction,
        port: port.clone(),
        gear: identify(port).map(|g| g.id),
    };
    for (dir, list) in [
        (Direction::In, &before.inputs),
        (Direction::Out, &before.outputs),
    ] {
        for p in list {
            if !now.has(dir, p) {
                out.push(change(false, dir, p));
            }
        }
    }
    for (dir, list) in [(Direction::In, &now.inputs), (Direction::Out, &now.outputs)] {
        for p in list {
            if !before.has(dir, p) {
                out.push(change(true, dir, p));
            }
        }
    }
    out
}

/// One device as a scan sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GearPresence {
    pub gear: &'static GearSpec,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl GearPresence {
    pub fn present(&self) -> bool {
        !self.inputs.is_empty() || !self.outputs.is_empty()
    }
}

/// Every catalogued device, present or not, in catalogue order.
pub fn presence(scan: &PortScan) -> Vec<GearPresence> {
    CATALOG
        .iter()
        .map(|g| {
            let mine = |list: &Vec<String>| {
                list.iter()
                    .filter(|p| identify(p).is_some_and(|x| x.id == g.id))
                    .cloned()
                    .collect()
            };
            GearPresence {
                gear: g,
                inputs: mine(&scan.inputs),
                outputs: mine(&scan.outputs),
            }
        })
        .collect()
}

/// The ports a scan found that belong to no catalogued device (virtual buses, other gear).
pub fn unknown_ports(scan: &PortScan) -> Vec<(Direction, String)> {
    let mut out = Vec::new();
    for p in &scan.inputs {
        if identify(p).is_none() {
            out.push((Direction::In, p.clone()));
        }
    }
    for p in &scan.outputs {
        if identify(p).is_none() {
            out.push((Direction::Out, p.clone()));
        }
    }
    out
}

// MARK: describing messages

/// The PD705's factory note map (pd705-rs `pd705-midi` `DEFAULT_MAP`, FORMAT.md §4).
pub const PD705_NOTES: [(u8, &str); 15] = [
    (49, "pad 1 crash"),
    (57, "pad 2 crash 2"),
    (51, "pad 3 ride"),
    (48, "pad 4 tom hi-mid"),
    (45, "pad 5 tom low"),
    (43, "pad 6 floor tom"),
    (36, "pad 7 kick"),
    (38, "pad 8 snare"),
    (42, "pad 9 hat"),
    (22, "trigger 1"),
    (24, "trigger 2"),
    (41, "trigger 3"),
    (39, "trigger 4"),
    (44, "foot 1 hat pedal"),
    (35, "foot 2 kick"),
];

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// `60` → `C4`.
pub fn note_name(n: u8) -> String {
    format!("{}{}", NOTE_NAMES[(n % 12) as usize], (n / 12) as i32 - 1)
}

/// A message in words, with what it means on `gear` when that is known: `note on ch10 36
/// v100 · pad 7 kick`, `cc ch1 #48 = 64`, `clock`, `sysex 11 bytes 00 20 32 …`.
pub fn describe(bytes: &[u8], gear: Option<&str>) -> String {
    let Some(&status) = bytes.first() else {
        return "empty".into();
    };
    if status < 0x80 {
        return format!("data {}", hex(bytes, 6));
    }
    if status >= 0xF0 {
        return match status {
            0xF0 => format!("sysex {} bytes {}", bytes.len(), hex(&bytes[1..], 6)),
            0xF1 => "quarter frame".into(),
            0xF2 => {
                let pos = bytes.get(1).copied().unwrap_or(0) as u32
                    | (bytes.get(2).copied().unwrap_or(0) as u32) << 7;
                format!("song position {pos}")
            }
            0xF3 => format!("song select {}", bytes.get(1).copied().unwrap_or(0)),
            0xF6 => "tune request".into(),
            0xF8 => "clock".into(),
            0xFA => "start".into(),
            0xFB => "continue".into(),
            0xFC => "stop".into(),
            0xFE => "active sensing".into(),
            0xFF => "reset".into(),
            _ => format!("system {status:02x}"),
        };
    }
    let ch = (status & 0x0F) + 1;
    let d1 = bytes.get(1).copied().unwrap_or(0);
    let d2 = bytes.get(2).copied().unwrap_or(0);
    let base = match status & 0xF0 {
        0x80 => format!("note off ch{ch} {d1} {}", note_name(d1)),
        0x90 if d2 == 0 => format!("note off ch{ch} {d1} {}", note_name(d1)),
        0x90 => format!("note on ch{ch} {d1} {} v{d2}", note_name(d1)),
        0xA0 => format!("aftertouch ch{ch} {d1} = {d2}"),
        0xB0 => format!("cc ch{ch} #{d1} = {d2}"),
        0xC0 => format!("program ch{ch} {d1}"),
        0xD0 => format!("pressure ch{ch} {d1}"),
        _ => {
            let v = d1 as i32 | (d2 as i32) << 7;
            format!("pitch bend ch{ch} {:+}", v - 8192)
        }
    };
    let meaning = match (gear, status & 0xF0) {
        (Some("pd705"), 0x80 | 0x90) => PD705_NOTES
            .iter()
            .find(|(n, _)| *n == d1)
            .map(|(_, s)| s.to_string()),
        _ => None,
    };
    match meaning {
        Some(m) => format!("{base} · {m}"),
        None => base,
    }
}

fn hex(bytes: &[u8], max: usize) -> String {
    let mut s = String::new();
    for (i, b) in bytes.iter().take(max).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > max {
        s.push_str(" …");
    }
    s
}

// MARK: the monitor

/// One message the monitor kept.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorEntry {
    /// Host time, seconds.
    pub time: f64,
    pub bytes: Vec<u8>,
    pub text: String,
}

/// What one port has sent lately.
#[derive(Clone, Debug, Default)]
pub struct PortTraffic {
    pub gear: Option<&'static str>,
    /// The latest messages, oldest first. Clock ticks and active sensing are counted, not kept.
    pub recent: VecDeque<MonitorEntry>,
    pub messages: u64,
    pub clock_ticks: u64,
    pub active_sensing: u64,
    pub last_time: Option<f64>,
    clock: ClockMeter,
}

impl PortTraffic {
    /// The tempo of the clock this port sends, if it is sending one.
    pub fn clock_bpm(&self, now: f64) -> Option<f64> {
        self.clock.bpm(now)
    }
}

/// Recent MIDI traffic per input port.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub ports: BTreeMap<String, PortTraffic>,
    pub keep: usize,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor::new(64)
    }
}

impl Monitor {
    pub fn new(keep: usize) -> Monitor {
        Monitor {
            ports: BTreeMap::new(),
            keep: keep.max(1),
        }
    }

    /// A message from `port` at host time `time` (seconds).
    pub fn push(&mut self, port: &str, bytes: &[u8], time: f64) {
        let keep = self.keep;
        let t = self
            .ports
            .entry(port.to_string())
            .or_insert_with(|| PortTraffic {
                gear: identify(port).map(|g| g.id),
                ..Default::default()
            });
        t.messages += 1;
        t.last_time = Some(time);
        match bytes.first() {
            Some(0xF8) => {
                t.clock_ticks += 1;
                t.clock.tick(time);
                return;
            }
            Some(0xFE) => {
                t.active_sensing += 1;
                return;
            }
            Some(0xFA) | Some(0xFC) => t.clock.reset(),
            _ => {}
        }
        let text = describe(bytes, t.gear);
        t.recent.push_back(MonitorEntry {
            time,
            bytes: bytes.to_vec(),
            text,
        });
        while t.recent.len() > keep {
            t.recent.pop_front();
        }
    }

    /// Forget a port (it was unplugged).
    pub fn forget(&mut self, port: &str) {
        self.ports.remove(port);
    }

    pub fn clear(&mut self) {
        for t in self.ports.values_mut() {
            t.recent.clear();
            t.messages = 0;
            t.clock_ticks = 0;
            t.active_sensing = 0;
        }
    }
}

/// The tempo of an incoming 24-PPQN clock: the mean tick interval over the last beat or so.
/// Silent for more than half a second: no clock.
#[derive(Clone, Debug, Default)]
pub struct ClockMeter {
    ticks: VecDeque<f64>,
}

impl ClockMeter {
    const KEEP: usize = 25;
    const STALE_S: f64 = 0.5;

    pub fn tick(&mut self, time: f64) {
        if let Some(&last) = self.ticks.back() {
            if time - last > Self::STALE_S || time < last {
                self.ticks.clear();
            }
        }
        self.ticks.push_back(time);
        while self.ticks.len() > Self::KEEP {
            self.ticks.pop_front();
        }
    }

    pub fn reset(&mut self) {
        self.ticks.clear();
    }

    pub fn bpm(&self, now: f64) -> Option<f64> {
        let (&first, &last) = (self.ticks.front()?, self.ticks.back()?);
        let n = self.ticks.len();
        if n < 3 || now - last > Self::STALE_S || last <= first {
            return None;
        }
        Some(60.0 * (n - 1) as f64 / ((last - first) * 24.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn scan(i: &[&str], o: &[&str]) -> PortScan {
        PortScan {
            inputs: i.iter().map(|s| s.to_string()).collect(),
            outputs: o.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn identifies_workbits_ports() {
        // what CoreMIDI listed on workbit, 2026-11-03
        let id = |p| identify(p).map(|g| g.id);
        assert_eq!(id("MPK Mini Mk II"), Some("mpk"));
        assert_eq!(id("APC mini mk2 Control"), Some("apc"));
        assert_eq!(id("APC mini mk2 Notes"), Some("apc"));
        assert_eq!(id("TD-3"), Some("td3"));
        assert_eq!(id("DrumPad"), Some("pd705"));
        assert_eq!(id("DJControl Inpulse 200 MK3"), Some("inpulse"));
        // ALSA spelling
        assert_eq!(id("APC mini mk2:APC mini mk2 Control 20:0"), Some("apc"));
        assert_eq!(id("btw Bus 1"), None);
    }

    #[test]
    fn presence_and_unknown_ports() {
        let s = scan(
            &[
                "btw Bus 1",
                "MPK Mini Mk II",
                "APC mini mk2 Control",
                "APC mini mk2 Notes",
                "TD-3",
                "DrumPad",
            ],
            &["btw Bus 1", "TD-3", "DrumPad", "APC mini mk2 Control"],
        );
        let p = presence(&s);
        let by = |id: &str| p.iter().find(|g| g.gear.id == id).unwrap();
        assert!(!by("inpulse").present());
        assert_eq!(by("apc").inputs.len(), 2);
        assert_eq!(by("td3").outputs, vec!["TD-3".to_string()]);
        assert_eq!(
            unknown_ports(&s),
            vec![
                (Direction::In, "btw Bus 1".into()),
                (Direction::Out, "btw Bus 1".into())
            ]
        );
    }

    #[test]
    fn diff_reports_detach_then_attach() {
        let a = scan(&["TD-3", "DrumPad"], &["TD-3"]);
        let b = scan(&["DrumPad", "MPK Mini Mk II"], &["TD-3", "MPK Mini Mk II"]);
        let d = diff(&a, &b);
        assert_eq!(d.len(), 3);
        assert_eq!(
            d[0],
            PortChange {
                attached: false,
                direction: Direction::In,
                port: "TD-3".into(),
                gear: Some("td3")
            }
        );
        assert!(d[1].attached && d[1].direction == Direction::In && d[1].gear == Some("mpk"));
        assert!(d[2].attached && d[2].direction == Direction::Out);
        assert!(diff(&b, &b).is_empty());
    }

    #[test]
    fn describes_messages() {
        assert_eq!(
            describe(&[0x99, 36, 100], Some("pd705")),
            "note on ch10 36 C2 v100 · pad 7 kick"
        );
        assert_eq!(
            describe(&[0x99, 36, 0], Some("pd705")),
            "note off ch10 36 C2 · pad 7 kick"
        );
        assert_eq!(describe(&[0x90, 60, 90], None), "note on ch1 60 C4 v90");
        assert_eq!(describe(&[0xB0, 48, 64], Some("apc")), "cc ch1 #48 = 64");
        assert_eq!(describe(&[0xC0, 3], None), "program ch1 3");
        assert_eq!(describe(&[0xE0, 0, 64], None), "pitch bend ch1 +0");
        assert_eq!(describe(&[0xF8], None), "clock");
        assert_eq!(describe(&[0xF2, 0x10, 0x01], None), "song position 144");
        assert_eq!(
            describe(&[0xF0, 0, 0x20, 0x32, 0, 1, 0x0A, 0x1B, 2, 0xF7], None),
            "sysex 10 bytes 00 20 32 00 01 0a …"
        );
    }

    #[test]
    fn monitor_keeps_recent_and_counts_clock() {
        let mut m = Monitor::new(3);
        for k in 0..5u8 {
            m.push("DrumPad", &[0x99, 36 + k, 100], k as f64 * 0.1);
        }
        let t = &m.ports["DrumPad"];
        assert_eq!(t.gear, Some("pd705"));
        assert_eq!(t.messages, 5);
        assert_eq!(t.recent.len(), 3);
        assert_eq!(t.recent[0].bytes, vec![0x99, 38, 100]);
        // a 120 BPM clock from the TD-3: 24 ticks a beat
        let dt = 60.0 / (120.0 * 24.0);
        for k in 0..48 {
            m.push("TD-3", &[0xF8], 1.0 + k as f64 * dt);
        }
        let t = &m.ports["TD-3"];
        assert_eq!((t.clock_ticks, t.recent.len()), (48, 0));
        let now = 1.0 + 47.0 * dt;
        assert!((t.clock_bpm(now).unwrap() - 120.0).abs() < 1e-9);
        assert_eq!(t.clock_bpm(now + 1.0), None, "stale");
        m.forget("TD-3");
        assert!(!m.ports.contains_key("TD-3"));
    }
}
