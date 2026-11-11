//! `ControlWalk.swift`: a guided check of one device's controls. The target is each control
//! of the profile in turn; the user moves it and the walk records whether that control (and
//! not another) fired.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::control::device_map::DeviceMapRow;

/// `ControlWalk.Outcome`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalkOutcome {
    Ok,
    /// Skipped with nothing heard: the control sends nothing the app sees.
    Skipped,
    /// Skipped after a different control fired, e.g. "mixer.faders[0]".
    HeardInstead(String),
}

#[derive(Clone, Debug)]
pub struct ControlWalk {
    pub device: String,
    pub rows: Vec<DeviceMapRow>,
    position: usize,
    outcomes: BTreeMap<String, WalkOutcome>,
    /// The last other control heard while waiting for the current target.
    heard: Option<String>,
    last_done: Option<String>,
}

impl ControlWalk {
    pub fn new(device: &str, rows: Vec<DeviceMapRow>) -> ControlWalk {
        ControlWalk {
            device: device.into(),
            rows,
            position: 0,
            outcomes: BTreeMap::new(),
            heard: None,
            last_done: None,
        }
    }

    pub fn key(group: &str, index: usize) -> String {
        format!("{group}[{index}]")
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn heard(&self) -> Option<&str> {
        self.heard.as_deref()
    }

    pub fn current(&self) -> Option<&DeviceMapRow> {
        self.rows.get(self.position)
    }

    pub fn is_done(&self) -> bool {
        self.position >= self.rows.len()
    }

    /// A control fired. Returns true when it was the target (the walk advances).
    pub fn feed(&mut self, group: &str, index: usize) -> bool {
        let Some(row) = self.current() else {
            return false;
        };
        let k = Self::key(group, index);
        if group == row.group && index == row.index {
            self.outcomes.insert(k.clone(), WalkOutcome::Ok);
            self.last_done = Some(k);
            self.heard = None;
            self.position += 1;
            return true;
        }
        // A fader that was just finished keeps streaming for a moment; that is not news.
        if self.last_done.as_deref() != Some(k.as_str()) {
            self.heard = Some(k);
        }
        false
    }

    /// Give up on the current target (it sends nothing, or the user can't find it).
    pub fn skip(&mut self) {
        let Some(row) = self.current() else { return };
        let k = Self::key(&row.group, row.index);
        let outcome = match self.heard.take() {
            Some(h) => WalkOutcome::HeardInstead(h),
            None => WalkOutcome::Skipped,
        };
        self.outcomes.insert(k, outcome);
        self.last_done = None;
        self.position += 1;
    }

    pub fn back(&mut self) {
        if self.position == 0 {
            return;
        }
        self.position -= 1;
        let r = &self.rows[self.position];
        self.outcomes.remove(&Self::key(&r.group, r.index));
        self.heard = None;
        self.last_done = None;
    }

    pub fn outcome(&self, group: &str, index: usize) -> Option<&WalkOutcome> {
        self.outcomes.get(&Self::key(group, index))
    }

    /// `(ok, skipped, wrong)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for o in self.outcomes.values() {
            match o {
                WalkOutcome::Ok => c.0 += 1,
                WalkOutcome::Skipped => c.1 += 1,
                WalkOutcome::HeardInstead(_) => c.2 += 1,
            }
        }
        c
    }

    /// One line per control, then the totals.
    pub fn report(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("control walk · {}", self.device));
        for r in &self.rows {
            let k = Self::key(&r.group, r.index);
            let num = r.number.map_or(String::new(), |n| format!(" ({n})"));
            lines.push(match self.outcomes.get(&k) {
                Some(WalkOutcome::Ok) => format!("ok       {k} {}{num} — {}", r.control, r.action),
                Some(WalkOutcome::Skipped) => {
                    format!("SILENT   {k} {}{num} — {}", r.control, r.action)
                }
                Some(WalkOutcome::HeardInstead(other)) => format!(
                    "WRONG    {k} {}{num} — {} · heard {other} instead",
                    r.control, r.action
                ),
                None => format!("not run  {k} {}{num}", r.control),
            });
        }
        let c = self.counts();
        lines.push(format!(
            "ok {} · silent {} · wrong {} of {}",
            c.0,
            c.1,
            c.2,
            self.rows.len()
        ));
        lines.join("\n")
    }
}
